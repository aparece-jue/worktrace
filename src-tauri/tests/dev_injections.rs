//! P7 Task 6a 的 dev 注入开关（`manual-sync.md` §1）：**只在 debug 构建存在**。
//!
//! 这个文件钉两件事：
//!
//! 1. **发布构建里没有它们**。三条（+ 开窗一条）命令的注册臂逐条带
//!    `#[cfg(debug_assertions)]`，命令体所在的 `commands::dev` 整份也带守卫——
//!    所以发布构建的 handler 列表里没有它们。这两道守卫是**源码事实**，用例直接读
//!    `src/lib.rs` / `src/commands/mod.rs` 核对（`cargo test` 只在 debug 档跑，
//!    发布那一支的 match 臂本身没法在同一次编译里观察；能观察的是守卫，以及
//!    「命令名只允许出现在受守卫的两个文件里」）。
//! 2. **开关的语义**（`manual-sync.md` §1 的两处订正）：丢弃按**事件名**筛
//!    （不能被一条 `timer.tick` 吃掉）、重播的参数是**旧 `revision`**（信封里没有
//!    `event_seq`）、延迟只作用一次且**先取数据再睡**、三条都**不写库不改 revision**。
//!
//! 真实双窗口的广播时序仍然只能在实机上观察，见 `manual-sync.md`。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use worktrace_lib::commands::{self, dev, CreateTaskRequest};
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, AppGuard, AppState, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{
    Broadcaster, EmitOutcome, EventEnvelope, EventSink, EVENT_DOMAIN_CHANGED, EVENT_TIMER_TICK,
};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;

/// 允许注册进 handler 的 dev 命令，逐字（顺序 = `lib.rs` 注册表里的顺序）。
///
/// 多一条就得改这里——**这是有意的摩擦**：每多一条 dev 命令，`manual-sync.md` §1
/// 的清单与「发布构建没有它们」的守卫都要一起看一遍。
const DEV_COMMANDS: [&str; 4] = [
    "commands::dev::__p7_drop_next_event",
    "commands::dev::__p7_delay_next_query_ms",
    "commands::dev::__p7_replay_event",
    "commands::dev::__p7_open_sync_lab",
];

/// 守卫的写法就这一种（`lib.rs` 与 `commands/mod.rs` 都用它）。
const DEBUG_GUARD: &str = "#[cfg(debug_assertions)]";

const WALL: i64 = 1_700_000_000_000;

// ─────────────────────────────────────────────────────────────────────────────
// 1. 只在 debug 构建（读源码核对守卫）
// ─────────────────────────────────────────────────────────────────────────────

fn source(rel: &str) -> String {
    let path = manifest().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("读不到 {}：{error}", path.display()))
}

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `lib.rs` 里 `generate_handler![…]` 的每条注册项：注册路径 + 它头上有没有守卫。
fn registered_commands(lib: &str) -> Vec<(String, bool)> {
    let start = lib
        .find("tauri::generate_handler![")
        .expect("lib.rs 必须有 generate_handler![（命令面的唯一入口）");
    let mut arms = Vec::new();
    let mut guarded = false;
    for line in lib[start..].lines().skip(1) {
        let line = line.trim();
        if line == "])" {
            break;
        }
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if let Some(rest) = line.strip_prefix(DEBUG_GUARD) {
            let rest = rest.trim();
            if rest.is_empty() {
                guarded = true;
                continue;
            }
            // 属性与路径同行也认（rustfmt 怎么排都不影响判定）。
            arms.push((rest.trim_end_matches(',').to_string(), true));
            continue;
        }
        arms.push((line.trim_end_matches(',').to_string(), guarded));
        guarded = false;
    }
    assert!(
        !arms.is_empty(),
        "注册表解析为空？解析器与 lib.rs 的写法对不上了"
    );
    arms
}

/// `declaration` 这一行（例如 `pub mod dev;`）的上一非空行是不是 [`DEBUG_GUARD`]。
fn is_guarded(source: &str, declaration: &str) -> bool {
    let lines: Vec<&str> = source.lines().map(str::trim).collect();
    let Some(at) = lines.iter().position(|line| *line == declaration) else {
        return false;
    };
    lines[..at]
        .iter()
        .rev()
        .find(|line| !line.is_empty())
        .is_some_and(|line| *line == DEBUG_GUARD)
}

/// **发布构建的 handler 列表里没有这几条命令**：注册表里每条 dev 臂都带
/// `#[cfg(debug_assertions)]`（`tauri::generate_handler!` 把属性原样交给 match 臂），
/// 于是发布构建里那几条臂整条不参与编译。
#[test]
fn the_dev_commands_are_registered_only_under_the_debug_guard() {
    let arms = registered_commands(&source("src/lib.rs"));

    let ungated: Vec<&str> = arms
        .iter()
        .filter(|(_, guarded)| !*guarded)
        .map(|(path, _)| path.as_str())
        .collect();
    let gated: Vec<&str> = arms
        .iter()
        .filter(|(_, guarded)| *guarded)
        .map(|(path, _)| path.as_str())
        .collect();

    assert_eq!(
        gated, DEV_COMMANDS,
        "受守卫的注册项必须**恰好**是这几条 dev 命令（多一条少一条都要在这里登记）"
    );
    assert!(
        ungated.iter().all(|path| !path.contains("commands::dev::")),
        "不带守卫的注册项就是发布构建的 handler 列表，里面一个 dev 命令都不许有：{ungated:?}"
    );
    // 业务命令的条数：**改注册表就要改这个数**。它挡的是「注册项被删/被改写法」——
    // 只写 `>= 1` 之类的下限，从 `lib.rs` 删掉一条业务命令就不会红（P8 Task 2a 起：
    // 25 → 30，含恢复与历史的五条写命令；P8 Task 2b：30 → 34，含恢复读取/重试与历史读取；
    // P8 Task 3a：34 → 37，含导出、备份与恢复）。
    assert!(
        ungated.len() >= 37,
        "37 条业务命令不带守卫（发布构建里也在）：只有 {} 条被解析出来，注册表是不是被改了写法？",
        ungated.len()
    );
}

/// dev 命令名只允许住在两个文件里：`lib.rs`（受守卫的注册臂，上一条用例核对）
/// 与 `commands/dev.rs`（命令体）。别处出现就是「绕过守卫的第二条注册路径」。
#[test]
fn the_dev_commands_live_only_in_the_debug_gated_module() {
    // ① 模块声明本身带守卫 ⇒ 发布构建里命令体、延迟开关、整个 dev.rs 都不存在。
    assert!(
        is_guarded(&source("src/commands/mod.rs"), "pub mod dev;"),
        "`pub mod dev;` 必须带 {DEBUG_GUARD}：否则发布构建里这些命令会存在"
    );

    // ② `__p7_` 只出现在那两个文件的**代码**里（注释里提到命令名是允许的：
    //    文档要能把「哪个命令干这个」写清楚，注册才算数）。
    let root = manifest().join("src");
    let mut offenders = Vec::new();
    let mut scanned = 0;
    let mut files = Vec::new();
    collect_rs_files(&root, &mut files);
    for file in files {
        scanned += 1;
        let text = std::fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("读不到 {}：{error}", file.display()));
        let mentioned = text
            .lines()
            .map(|line| line.split("//").next().unwrap_or_default())
            .any(|code| code.contains("__p7_"));
        if !mentioned {
            continue;
        }
        let rel = file
            .strip_prefix(manifest())
            .expect("src 下的文件都在 crate 目录里")
            .to_string_lossy()
            .replace('\\', "/");
        if rel != "src/lib.rs" && rel != "src/commands/dev.rs" {
            offenders.push(rel);
        }
    }
    assert!(scanned > 0, "src 下一个 .rs 都没扫到？");
    assert!(
        offenders.is_empty(),
        "dev 命令只允许定义在 src/commands/dev.rs、注册在 src/lib.rs 的守卫臂里；\
         这些文件的代码里也出现了：{offenders:?}"
    );
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("读不到 {}：{e}", dir.display()))
    {
        let path = entry.expect("目录项读得到").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// 延迟开关按 **(调用方窗口, 命令名)** 匹配，所以命令包装交给 `run_command` 的两个键
/// 都得对：命令名与 `lib.rs` 注册表逐字一致（写错一个字，装开关的命令永远不触发），
/// 而且必须把调用方窗口一起交上去（少一个，装在 B 上的开关会被 A 的重拉先吃掉）。
///
/// **两个骨架一起数**（P8 Task 3a）：`restore` 走的是 [`run_maintenance_command`]
/// （它**不进** `run_command` 的单临界区形状，理由见 `commands/mod.rs` 那一节），
/// 但两个键一样要交对。哪条命令走哪个骨架由下一条用例逐条钉住。
#[test]
fn every_command_passes_its_own_name_and_the_calling_window_to_run_command() {
    let commands_source = source("src/commands/mod.rs");
    let mut calls = run_command_calls(&commands_source);
    calls.extend(named_calls(&commands_source, MAINTENANCE_HEAD));

    let names: Vec<String> = calls
        .iter()
        .map(|call| {
            first_string_literal(call).unwrap_or_else(|| panic!("调用里没有命令名：{call}"))
        })
        .collect();

    let registered: Vec<String> = registered_commands(&source("src/lib.rs"))
        .into_iter()
        .filter(|(_, guarded)| !*guarded)
        .map(|(path, _)| {
            path.rsplit("::")
                .next()
                .expect("注册路径至少有一段")
                .to_string()
        })
        .collect();

    for name in &registered {
        assert!(
            names.contains(name),
            "`{name}` 的包装没有把命令名交给命令骨架（dev 延迟开关按名字匹配）：{names:?}"
        );
    }
    assert!(
        names.iter().any(|name| name == "list_tasks"),
        "manual-sync.md §2.2 用的就是 `list_tasks`：{names:?}"
    );
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        names.len(),
        "同一条命令名出现两次（复制粘贴没改名字？）：{names:?}"
    );

    for call in &calls {
        assert!(
            call.contains("window.label()"),
            "每条命令都要把**调用方窗口**交给命令骨架（dev 延迟开关的另一个键）：{call}"
        );
    }
}

/// **`restore` 是唯一不进 `run_command` 的命令**（P8 Task 3a 的计划要求：
/// 「这是唯一不进 `run_command` 单临界区形状的命令」）。
///
/// 这条断言就是它的机器判据，两侧都钉：
/// - 唯一性：走 [`run_maintenance_command`] 的命令名**恰好**是 `{"restore"}`——
///   把别的命令也搬过去（`run_command` 的维护态快速失败会失效）当场红；
/// - 不能改回去：谁把 `restore` 接回 `run_command` 的闭包，它就出现在前者里、
///   从这个集合里消失 ⇒ 红。**改回去的真实后果是静默死锁**（`run_command` 正持锁，
///   而 `restore_from_backup` 要自己按段取锁），所以这条判据必须存在——它挡的是
///   一条"门禁全绿、真机上卡死"的改动。
#[test]
fn restore_is_the_only_command_that_does_not_go_through_run_command() {
    let commands_source = source("src/commands/mod.rs");
    let plain: Vec<String> = run_command_calls(&commands_source)
        .iter()
        .filter_map(|call| first_string_literal(call))
        .collect();
    let maintenance: Vec<String> = named_calls(&commands_source, MAINTENANCE_HEAD)
        .iter()
        .filter_map(|call| first_string_literal(call))
        .collect();

    assert_eq!(
        maintenance,
        vec!["restore".to_string()],
        "走 `run_maintenance_command` 的命令必须恰好是 restore"
    );
    assert!(
        !plain.iter().any(|name| name == "restore"),
        "restore 不得回到 run_command 的单临界区形状：{plain:?}"
    );
}

/// `run_command(…)` 的调用表达式（括号配平，跟 rustfmt 怎么折行无关）。
fn run_command_calls(source: &str) -> Vec<String> {
    let calls = named_calls(source, "run_command(");
    assert!(!calls.is_empty(), "一条 run_command 调用都没解析出来？");
    calls
}

/// `restore` 专用骨架的调用头（P8 Task 3a）。它与 `run_command(` **不互为子串**
/// （`...nance_command(` 里没有 `run_command(`），所以两个解析器各数各的。
const MAINTENANCE_HEAD: &str = "run_maintenance_command(";

/// 某个调用头（例如 `run_command(`）的调用表达式（括号配平，跟 rustfmt 怎么折行无关）。
fn named_calls(source: &str, head: &str) -> Vec<String> {
    let mut calls = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find(head) {
        let open = at + head.len() - 1;
        let bytes = rest.as_bytes();
        let mut depth = 0usize;
        let mut end = open;
        while end < bytes.len() {
            match bytes[end] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        end += 1;
                        break;
                    }
                }
                _ => {}
            }
            end += 1;
        }
        calls.push(rest[open..end].to_string());
        rest = &rest[end..];
    }
    calls
}

fn first_string_literal(call: &str) -> Option<String> {
    let start = call.find('"')? + 1;
    let end = call[start..].find('"')? + start;
    Some(call[start..end].to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. 开关的语义（行为）
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct RecordingSink {
    seen: Mutex<Vec<EventEnvelope>>,
}

impl EventSink for RecordingSink {
    fn broadcast(&self, envelope: &EventEnvelope) -> Result<(), String> {
        self.seen.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

fn notification(event: &str, revision: i64) -> EventEnvelope {
    EventEnvelope::new("e1", event, revision, WALL, serde_json::json!({}))
}

fn seen_revisions(sink: &RecordingSink) -> Vec<i64> {
    sink.seen
        .lock()
        .unwrap()
        .iter()
        .map(|envelope| envelope.revision)
        .collect()
}

/// 注入 (a)：**按事件名筛**。有活动会话时每秒一条 `timer.tick`——不筛的话
/// 「丢掉下一条」会被一条无害 tick 吃掉，实机看起来「什么都没发生」
/// （`manual-sync.md` §1 的 ⚠️）。
#[test]
fn the_drop_switch_filters_by_event_name() {
    let sink = Arc::new(RecordingSink::default());
    let broadcaster = Broadcaster::new(Arc::clone(&sink) as Arc<dyn EventSink>);

    broadcaster.arm_drop_next(EVENT_DOMAIN_CHANGED);

    // 先来一条 tick：照常投递，而且**不能**把开关吃掉。
    assert_eq!(
        broadcaster.emit(notification(EVENT_TIMER_TICK, 1)),
        EmitOutcome::Sent,
        "tick 与开关装的事件名不同 ⇒ 照常投递"
    );
    assert_eq!(seen_revisions(&sink), vec![1]);
    assert_eq!(
        broadcaster.diagnostics().dropped,
        0,
        "tick 不该消费掉这个开关（消费了就再也丢不到那条业务通知）"
    );

    // 真正要丢的那条：不进 sink。
    assert_eq!(
        broadcaster.emit(notification(EVENT_DOMAIN_CHANGED, 2)),
        EmitOutcome::Dropped
    );
    assert_eq!(seen_revisions(&sink), vec![1], "被丢掉的通知不进 sink");

    // 一次一发：下一条同名的照常投递（丢掉的是「一条」通知，不是这类通知）。
    assert_eq!(
        broadcaster.emit(notification(EVENT_DOMAIN_CHANGED, 3)),
        EmitOutcome::Sent
    );
    assert_eq!(seen_revisions(&sink), vec![1, 3]);
    assert_eq!(broadcaster.diagnostics().dropped, 1);
    assert_eq!(broadcaster.diagnostics().sent, 2);

    // 对称：也能丢 tick（开关不挑事件类型，只按名字）。
    broadcaster.arm_drop_next(EVENT_TIMER_TICK);
    assert_eq!(
        broadcaster.emit(notification(EVENT_TIMER_TICK, 4)),
        EmitOutcome::Dropped
    );
    assert_eq!(seen_revisions(&sink), vec![1, 3]);
}

/// 注入 (b)：只延迟**装了开关的那个窗口**里、**那一条命令**的下一次响应，而且一次一发。
///
/// 按窗口分是 §2.2 判据成立的前提：一次 `domain.changed` 之后每个窗口都会重拉
/// `list_tasks`（A 还会因为 `afterWrite()` 当场再拉一次），只按命令名匹配的话装在 B 上的
/// 开关会被 A 的重拉先吃掉，B 那次根本没被延迟。
///
/// 真正的时序口径（先取数据再 sleep、不跨 `await` 持 `Connection`）在
/// `commands::run_command` 里：这个函数是在阻塞段 `.await` 回来之后才被调用的。
#[test]
fn the_delay_switch_delays_only_the_armed_window_and_command_once() {
    const MS: u64 = 120;
    const HALF: Duration = Duration::from_millis(MS / 2);

    // 第一次调用会初始化 tauri 的全局异步运行时，别把它的开销算进断言。
    tauri::async_runtime::block_on(async {});

    let immediate = Instant::now();
    tauri::async_runtime::block_on(dev::delay_response_if_armed("sync-lab", "list_tasks"));
    let immediate = immediate.elapsed();
    assert!(
        immediate < HALF,
        "没装开关时必须立刻返回（实测 {immediate:?}）"
    );

    dev::arm_delay("sync-lab", "list_tasks", MS);

    // 别的窗口不吃掉它——这正是「在 B 上装、A 也在重拉」的场面。
    let other_window = Instant::now();
    tauri::async_runtime::block_on(dev::delay_response_if_armed("main", "list_tasks"));
    let other_window = other_window.elapsed();
    assert!(
        other_window < HALF,
        "开关只认装它的那个窗口（实测 {other_window:?}）"
    );

    // 别的命令也不吃。
    let other_command = Instant::now();
    tauri::async_runtime::block_on(dev::delay_response_if_armed("sync-lab", "timer_snapshot"));
    let other_command = other_command.elapsed();
    assert!(
        other_command < HALF,
        "开关只认它自己那条命令（实测 {other_command:?}）"
    );

    // 装开关的那个窗口 + 那条命令：必须等够。
    let delayed = Instant::now();
    tauri::async_runtime::block_on(dev::delay_response_if_armed("sync-lab", "list_tasks"));
    let delayed = delayed.elapsed();
    assert!(
        delayed >= Duration::from_millis(MS),
        "装了开关的那一次必须等够 {MS} ms（实测 {delayed:?}）"
    );

    // 一次一发：下一条同窗口同名的立刻返回。
    let after = Instant::now();
    tauri::async_runtime::block_on(dev::delay_response_if_armed("sync-lab", "list_tasks"));
    let after = after.elapsed();
    assert!(after < HALF, "一次一发：第二次不该再等（实测 {after:?}）");
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. 注入 (c)：旧 revision 重播（要一个真库）
// ─────────────────────────────────────────────────────────────────────────────

struct Rig {
    _dir: tempfile::TempDir,
    running: Box<RunningApp>,
    sink: Arc<RecordingSink>,
    epoch: String,
}

/// 一个真应用（走 `services::bootstrap::startup`），采样节拍设成 1 小时：
/// 这里只测注入开关，不让周期采样插进来写检查点。
fn launch() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("worktrace.db");
    let lock_path = dir.path().join("instance.lock");

    {
        let mut db = Db::open(&db_path).unwrap();
        migrate(db.connection()).unwrap();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        init_meta(&tx).unwrap();
        tx.commit().unwrap();
    }

    let mut config = StartupConfig::new(&db_path, &lock_path);
    config.sampling_interval_ms = 3_600_000;
    let sink = Arc::new(RecordingSink::default());
    let running = match startup(
        config,
        Box::new(FakeClock::new(WALL, 0)),
        Arc::clone(&sink) as Arc<dyn EventSink>,
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    };

    Rig {
        _dir: dir,
        epoch: running.data_epoch().to_string(),
        running,
        sink,
    }
}

impl Rig {
    fn state(&self) -> AppGuard<'_> {
        lock_app(self.running.app())
    }

    fn events(&self) -> Vec<EventEnvelope> {
        self.sink.seen.lock().unwrap().clone()
    }
}

fn revision_of(state: &AppState) -> i64 {
    state
        .db()
        .unwrap()
        .connection()
        .query_row(
            "SELECT revision FROM app_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

/// 注入 (c)：参数是**旧 `revision`**（信封里没有 `event_seq`，`manual-sync.md` §1
/// 的订正）。重播**只广播**：不写库、不推进 revision，旧版本照常投递
/// （`Broadcaster` 只把它记成 `out_of_order` 诊断）——客户端据此按闸门②/③丢弃它。
#[test]
fn the_replay_switch_emits_the_old_revision_without_writing() {
    let rig = launch();
    let mut state = rig.state();

    // 一次真实写入：revision 0 → 1（重播要播的是「旧」版本，所以得有个新版本）。
    commands::create_task_impl(
        &mut state,
        rig.running.broadcaster(),
        CreateTaskRequest {
            expected_data_epoch: rig.epoch.clone(),
            title: "实机-a".to_string(),
            project_id: None,
        },
    )
    .unwrap();
    let current = revision_of(&state);
    assert_eq!(current, 1, "一次成功业务写恰好推进一次 revision");
    let changes_before = state.db().unwrap().connection().total_changes();

    let envelope =
        dev::replay_event_impl(&mut state, rig.running.broadcaster(), current - 1).unwrap();

    assert_eq!(envelope.event, EVENT_DOMAIN_CHANGED);
    assert_eq!(
        envelope.revision,
        current - 1,
        "重播用的就是传进来的那条**旧** revision"
    );
    assert_eq!(envelope.data_epoch, rig.epoch, "epoch 取当前库身份");
    assert_eq!(envelope.at, WALL, "时刻走时钟接缝（FakeClock）");

    assert_eq!(
        revision_of(&state),
        current,
        "重播不是业务写：不推进 revision"
    );
    assert_eq!(
        state.db().unwrap().connection().total_changes(),
        changes_before,
        "重播不写库"
    );

    let events = rig.events();
    assert_eq!(events.len(), 2, "create 一条 + 重播一条");
    assert_eq!(
        events.last().unwrap().revision,
        current - 1,
        "旧版本的通知**照常投递**（丢掉它才是错的：客户端按闸门②/③自己判）"
    );
}
