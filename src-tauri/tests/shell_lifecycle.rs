//! P7 Task 4：托盘与窗口生命周期。
//!
//! 计划里的测试口径是「托盘动作与界面动作调用同一命令；关窗不触发退出；重开窗口触发
//! 快照。**其余必须人工验收**」。所以这个文件钉的是**能用 Rust 断言的那半边**：
//!
//! - 托盘菜单的描述与动作映射（四项 + 一个预留项）；
//! - 「关掉全部窗口不退出」的决策（`RunEvent::ExitRequested` 的两个分支）；
//! - 唤醒请求的接收决策（抬起 / 重建 / 什么都不做）；
//! - 主窗 label 与 `tauri.conf.json` / `capabilities/default.json` 的一致性
//!   （对不上就等于「重建出来的窗口没有权限」）；
//! - 托盘的「暂停」「退出」落在**与 IPC 相同的**命令体/服务入口上（用效果相等与
//!   显式退出的库内证据断言）。
//!
//! **真实托盘图标/菜单交互、关掉全部窗口后仍然计时**在集成测试里不可能成立：
//! 测试进程里没有事件循环，也就没有窗口与托盘（`tauri::test` 的 mock 运行时本轮
//! 没有启用）。步骤与记录表见 `tests/manual-shell.md`。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use worktrace_lib::commands::{self, StartTimerRequest, TrayPause};
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::platform::tray::{self, MenuItemSpec, TrayAction};
use worktrace_lib::platform::window::{self, ActivationPlan};
use worktrace_lib::services::bootstrap::{
    lock_app, startup, AppState, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::timer::coordinator::SessionRequest;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;

fn item(id: &str) -> &'static MenuItemSpec {
    tray::MENU_ITEMS
        .iter()
        .find(|item| item.id == id)
        .unwrap_or_else(|| panic!("菜单里应当有 {id}"))
}

// ─────────────────────────────────────────────────────────────────────────────
// 菜单：四项 + 一个预留项（F-011 / R4 裁决）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_menu_offers_exactly_the_four_p7_actions() {
    let clickable: Vec<&str> = tray::MENU_ITEMS
        .iter()
        .filter(|item| item.action.is_some())
        .map(|item| item.id)
        .collect();
    assert_eq!(
        clickable,
        vec![
            TrayAction::CurrentTask.menu_id(),
            TrayAction::Pause.menu_id(),
            TrayAction::QuickCapture.menu_id(),
            TrayAction::Quit.menu_id(),
        ],
        "P7 实际提供四项：当前任务、暂停、快速捕获、退出（退出在最后）"
    );

    let labels: Vec<&str> = tray::MENU_ITEMS
        .iter()
        .filter(|item| item.action.is_some())
        .map(|item| item.label)
        .collect();
    assert_eq!(
        labels,
        vec!["当前任务", "暂停", "快速捕获", "退出"],
        "菜单文案是面向用户的中文，不写模块名"
    );
}

#[test]
fn finish_is_reserved_and_disabled_without_an_action() {
    let reserved: Vec<&str> = tray::MENU_ITEMS
        .iter()
        .filter(|item| item.action.is_none())
        .map(|item| item.id)
        .collect();
    assert_eq!(
        reserved,
        vec!["tray.finish_reserved"],
        "有且只有「完成」是预留项：P3 的 transition_task 接入后由 P8 启用"
    );
    assert!(
        item("tray.finish_reserved").label.contains("完成"),
        "预留项就是「完成」，只是 P7 不给它动作"
    );
    assert_eq!(
        tray::action_for("tray.finish_reserved"),
        None,
        "点了预留项必须什么都不发生（菜单里它是禁用项，这里再钉一次）"
    );
}

#[test]
fn every_menu_row_is_an_action_or_the_reserved_item() {
    assert_eq!(tray::MENU_ITEMS.len(), 5, "四项 + 一个预留项");
    for action in TrayAction::ALL {
        assert_eq!(
            tray::MENU_ITEMS
                .iter()
                .filter(|item| item.action == Some(action))
                .count(),
            1,
            "每条动作在菜单里恰好出现一次：{}",
            action.menu_id()
        );
    }
    for item in tray::MENU_ITEMS.iter() {
        assert!(!item.id.is_empty(), "菜单项 id 不能为空");
        assert!(!item.label.is_empty(), "菜单项文案不能为空");
    }

    let mut ids: Vec<&str> = tray::MENU_ITEMS.iter().map(|item| item.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), tray::MENU_ITEMS.len(), "菜单项 id 必须唯一");
}

#[test]
fn a_menu_id_maps_back_to_its_own_action() {
    for action in TrayAction::ALL {
        assert_eq!(
            tray::action_for(action.menu_id()),
            Some(action),
            "id 与动作必须一一对应（写错一个字就是「点了没反应」）"
        );
    }
    assert_eq!(tray::action_for("tray.nonexistent"), None);
    assert_eq!(tray::action_for(""), None);
}

// ─────────────────────────────────────────────────────────────────────────────
// 窗口：关窗不退出 / 唤醒的决策（F-009 / F-016）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn closing_the_last_window_is_prevented_but_a_programmatic_exit_is_not() {
    assert!(
        window::should_prevent_exit(None),
        "`ExitRequested {{ code: None }}` = 用户关掉最后一个窗口 ⇒ 阻止退出：\
         托盘还得在、周期采样还得跑（F-009）"
    );
    assert!(
        !window::should_prevent_exit(Some(0)),
        "`code = Some(0)` = 托盘「退出」走的 `AppHandle::exit(0)` ⇒ 必须放行，\
         否则退出会变成一个关不掉的进程"
    );
    assert!(
        !window::should_prevent_exit(Some(1)),
        "退出事务失败时的非零码同样要放行"
    );
}

#[test]
fn an_activation_request_raises_or_rebuilds_and_nothing_else() {
    assert_eq!(
        window::plan_activation(false, true),
        ActivationPlan::Ignore,
        "没有请求时**什么都不做**：不能凭一个不存在的请求把窗口抬起来"
    );
    assert_eq!(
        window::plan_activation(false, false),
        ActivationPlan::Ignore,
        "没有请求、窗口也没了 ⇒ 仍然什么都不做（不凭空建窗）"
    );
    assert_eq!(
        window::plan_activation(true, true),
        ActivationPlan::Raise,
        "有请求且主窗还在 ⇒ 抬起/聚焦，不重建（重建会丢掉用户当前的界面状态）"
    );
    assert_eq!(
        window::plan_activation(true, false),
        ActivationPlan::Rebuild,
        "有请求但主窗已经关掉 ⇒ 按配置重建（新页面加载 ⇒ 立即拉快照）"
    );
}

#[test]
fn the_main_window_label_matches_the_config_and_the_capability() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = read_json(&manifest.join("tauri.conf.json"));
    let labels: Vec<&str> = config["app"]["windows"]
        .as_array()
        .expect("tauri.conf.json 的 app.windows 必须是数组")
        .iter()
        .filter_map(|window| window["label"].as_str())
        .collect();
    assert!(
        labels.contains(&window::MAIN_WINDOW_LABEL),
        "重建主窗是从 tauri.conf.json 找 label = `{}` 的那份配置；找不到就重建不出来（现有：{labels:?}）",
        window::MAIN_WINDOW_LABEL
    );

    let capabilities = read_json(&manifest.join("capabilities/default.json"));
    let windows: Vec<&str> = capabilities["windows"]
        .as_array()
        .expect("capabilities/default.json 的 windows 必须是数组")
        .iter()
        .filter_map(|label| label.as_str())
        .collect();
    assert!(
        windows.contains(&window::MAIN_WINDOW_LABEL),
        "重建出来的窗口 label 不变，权限名单必须覆盖它，否则窗口能开、JS 却调不动任何命令\
         （现有：{windows:?}）"
    );
}

fn read_json(path: &std::path::Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("读不到 {}：{error}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("{} 不是合法 JSON：{error}", path.display()))
}

// ─────────────────────────────────────────────────────────────────────────────
// 托盘动作：与 IPC 走同一批入口
// ─────────────────────────────────────────────────────────────────────────────

/// 假时钟的挂钟。两条路径用的是**同一个**时刻，所以「效果相等」比得了。
const WALL: i64 = 1_700_000_000_000;

/// 采样节拍：默认给一个**跑不起来**的值，免得采样线程在临界区外面偷偷加拍；
/// 需要观察采样本身的用例用 [`launch_with_interval`] 显式给一个小节拍。
const IDLE_SAMPLING_MS: u64 = 3_600_000;

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<EventEnvelope>>,
}

impl EventSink for RecordingSink {
    fn broadcast(&self, envelope: &EventEnvelope) -> Result<(), String> {
        self.events.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

/// 一个真应用（走 `services::bootstrap::startup`），一个 Ready 任务 `t1`。
struct Rig {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    running: Box<RunningApp>,
    sink: Arc<RecordingSink>,
    epoch: String,
}

fn launch() -> Rig {
    launch_with_interval(IDLE_SAMPLING_MS)
}

fn launch_with_interval(sampling_interval_ms: u64) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("worktrace.db");
    let lock_path = dir.path().join("instance.lock");

    {
        let mut db = Db::open(&db_path).unwrap();
        migrate(db.connection()).unwrap();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        init_meta(&tx).unwrap();
        tx.execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES('t1','任务一','Ready',0,1000,1000)",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let mut config = StartupConfig::new(&db_path, &lock_path);
    config.sampling_interval_ms = sampling_interval_ms;
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

    let epoch = running.data_epoch().to_string();
    Rig {
        _dir: dir,
        db_path,
        running,
        sink,
        epoch,
    }
}

impl Rig {
    /// 取串行边界。**同一个测试里只能取一次**（`Mutex` 不可重入）。
    fn state(&self) -> MutexGuard<'_, AppState> {
        lock_app(self.running.app())
    }

    fn events(&self) -> Vec<EventEnvelope> {
        self.sink.events.lock().unwrap().clone()
    }

    /// 另一条连接：退出之后读库用（`shutdown` 自己会取那把锁，不能持锁调它）。
    fn db(&self) -> Db {
        Db::open(&self.db_path).unwrap()
    }
}

fn start_request(rig: &Rig, task_id: &str) -> StartTimerRequest {
    StartTimerRequest {
        expected_data_epoch: rig.epoch.clone(),
        task_id: task_id.to_string(),
        task_expected_version: 0,
        mode: "FOREGROUND".to_string(),
        timer_kind: "stopwatch".to_string(),
        target_duration_ms: None,
        expected_interval_ms: 1_000,
    }
}

fn revision_of(state: &AppState) -> i64 {
    state
        .db()
        .connection()
        .query_row(
            "SELECT revision FROM app_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn text_of(state: &AppState, sql: &str) -> String {
    state
        .db()
        .connection()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

/// 库里会话的四个事实（状态 / `ended_at` / 区间时长 / 待确认）。
fn session_facts(state: &AppState) -> Vec<(String, i64, i64, i64)> {
    let mut statement = state
        .db()
        .connection()
        .prepare(
            "SELECT s.state, COALESCE(s.ended_at, -1),
                    COALESCE(i.duration_ms, -1), COALESCE(i.needs_review, 0)
             FROM work_session s JOIN work_interval i ON i.session_id = s.id
             ORDER BY s.id",
        )
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap();
    rows.map(|row| row.unwrap()).collect()
}

/// 界面点「暂停」时回传的正是**展示中的快照**字段；托盘那条入口用的是同一次读到的
/// 快照。两条路径因此可以直接比效果：响应、库里的行、revision、广播。
#[test]
fn the_tray_pause_lands_on_the_same_command_body_as_the_ipc_one() {
    let tray_rig = launch();
    let ipc_rig = launch();

    let tray_outcome = {
        let mut state = tray_rig.state();
        commands::start_timer_impl(
            &mut state,
            tray_rig.running.broadcaster(),
            start_request(&tray_rig, "t1"),
        )
        .unwrap();
        let revision_before = revision_of(&state);

        let paused = commands::tray_pause_impl(&mut state, tray_rig.running.broadcaster())
            .expect("运行中的会话必须能被托盘暂停");
        let TrayPause::Paused(outcome) = paused else {
            panic!("有运行中的会话时托盘暂停不能是「无事可做」");
        };
        assert_eq!(
            outcome.revision,
            revision_before + 1,
            "暂停是一次成功业务写 ⇒ 恰好推进一次 revision"
        );
        outcome
    };

    let ipc_outcome = {
        let mut state = ipc_rig.state();
        commands::start_timer_impl(
            &mut state,
            ipc_rig.running.broadcaster(),
            start_request(&ipc_rig, "t1"),
        )
        .unwrap();
        let revision_before = revision_of(&state);

        let snapshot = commands::timer_snapshot_impl(&mut state).unwrap();
        let outcome = commands::pause_timer_impl(
            &mut state,
            ipc_rig.running.broadcaster(),
            SessionRequest {
                expected_data_epoch: snapshot.data_epoch.clone(),
                session_id: snapshot.session_id.clone().expect("有会话"),
                session_expected_version: snapshot.session_version.expect("有会话版本"),
            },
        )
        .unwrap();
        assert_eq!(outcome.revision, revision_before + 1);
        outcome
    };

    // ① 响应：状态、版本、暂计、任务版本逐项相同（会话 id 是随机 uuid，不比）。
    assert_eq!(tray_outcome.snapshot.state, ipc_outcome.snapshot.state);
    assert_eq!(
        tray_outcome.snapshot.session_version,
        ipc_outcome.snapshot.session_version
    );
    assert_eq!(
        tray_outcome.snapshot.active_ms,
        ipc_outcome.snapshot.active_ms
    );
    assert_eq!(tray_outcome.revision, ipc_outcome.revision);
    assert_eq!(tray_outcome.task_version, ipc_outcome.task_version);

    // ② 库里的行：四条事实逐项相同。
    assert_eq!(
        session_facts(&tray_rig.state()),
        session_facts(&ipc_rig.state()),
        "托盘暂停与 IPC 暂停必须落下同一组事实（这才是「同一命令」）"
    );
    assert_eq!(
        session_facts(&tray_rig.state())[0].0,
        "paused",
        "暂停不是结束：会话进 paused，不写 ended_at"
    );

    // ③ 广播：同样一条 `domain.changed`、同一个 revision。
    let tray_events = tray_rig.events();
    let ipc_events = ipc_rig.events();
    assert_eq!(tray_events.len(), ipc_events.len());
    assert_eq!(tray_events.len(), 2, "start 一条 + pause 一条");
    let (tray_last, ipc_last) = (tray_events.last().unwrap(), ipc_events.last().unwrap());
    assert_eq!(tray_last.event, ipc_last.event);
    assert_eq!(tray_last.revision, ipc_last.revision);
    // `data_epoch` 是**每个库自己的**（两条夹具两个临时库），所以比的是
    // 「事件带的 epoch 就是本库的权威 epoch」，不是两个库相等。
    assert_eq!(tray_last.data_epoch, tray_rig.epoch);
    assert_eq!(ipc_last.data_epoch, ipc_rig.epoch);
    assert_eq!(
        tray_last.event,
        worktrace_lib::services::events::EVENT_DOMAIN_CHANGED
    );
}

#[test]
fn the_tray_pause_without_a_running_session_writes_nothing() {
    let rig = launch();
    let mut state = rig.state();

    // ① 完全没有会话：什么都不做。
    let revision_before = revision_of(&state);
    let changes_before = state.db().connection().total_changes();
    assert_eq!(
        commands::tray_pause_impl(&mut state, rig.running.broadcaster()).unwrap(),
        TrayPause::NothingToPause
    );
    assert_eq!(
        revision_of(&state),
        revision_before,
        "没有会话时不能推进 revision"
    );
    assert_eq!(
        state.db().connection().total_changes(),
        changes_before,
        "没有会话时不能写库"
    );
    assert!(rig.events().is_empty(), "没有业务写就不该有 domain.changed");

    // ② 会话已经暂停：不重复暂停（否则会撞 VERSION_CONFLICT，还会白广播一次）。
    commands::start_timer_impl(
        &mut state,
        rig.running.broadcaster(),
        start_request(&rig, "t1"),
    )
    .unwrap();
    let snapshot = commands::timer_snapshot_impl(&mut state).unwrap();
    commands::pause_timer_impl(
        &mut state,
        rig.running.broadcaster(),
        SessionRequest {
            expected_data_epoch: snapshot.data_epoch.clone(),
            session_id: snapshot.session_id.clone().expect("有会话"),
            session_expected_version: snapshot.session_version.expect("有会话版本"),
        },
    )
    .unwrap();

    let revision_after_pause = revision_of(&state);
    let changes_after_pause = state.db().connection().total_changes();
    let events_after_pause = rig.events().len();

    assert_eq!(
        commands::tray_pause_impl(&mut state, rig.running.broadcaster()).unwrap(),
        TrayPause::NothingToPause
    );
    assert_eq!(revision_of(&state), revision_after_pause);
    assert_eq!(
        state.db().connection().total_changes(),
        changes_after_pause,
        "重复暂停不能有任何写入"
    );
    assert_eq!(
        text_of(&state, "SELECT state FROM work_session"),
        "paused",
        "已经暂停的会话不该被再动一次"
    );
    drop(state);
    assert_eq!(
        rig.events().len(),
        events_after_pause,
        "无事可做 ⇒ 不广播（不空转制造通知）"
    );
}

/// 托盘「退出」必须落在 **Task 0 的显式退出入口**上：库内证据与 `tests/exit.rs`
/// 逐条一致（结束当前 run 的 running/paused、写 `clean_exit_at`、保留 recovering、
/// 先停定时器）。它证明退出不是 `std::process::exit`，也不是另一份实现。
#[test]
fn the_tray_quit_goes_through_the_explicit_exit_entry() {
    let rig = launch_with_interval(10);
    let session_id = {
        let mut state = rig.state();
        let started = commands::start_timer_impl(
            &mut state,
            rig.running.broadcaster(),
            start_request(&rig, "t1"),
        )
        .unwrap();
        started.snapshot.session_id.clone().expect("有会话")
    };

    // 另一条 recovering 会话（02 §4：退出**保留**它，不得顺手确认历史）。
    let run_id = rig.running.run_id().to_string();
    rig.db()
        .connection()
        .execute(
            "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,
                                      row_version,needs_review)
             VALUES('s-recovering','t1',?1,'FOREGROUND','recovering','stopwatch',1200,0,1)",
            rusqlite::params![run_id],
        )
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.running.sampling_ticks() < 2 {
        assert!(Instant::now() < deadline, "采样驱动没有跑起来");
        std::thread::sleep(Duration::from_millis(5));
    }

    // 注意：**不能持着 state 的锁**调退出（`shutdown` 自己会取那把锁）。
    let report = commands::tray_quit_impl(&rig.running).expect("托盘退出必须走显式退出入口");

    assert_eq!(report.run_id, run_id);
    assert_eq!(report.clean_exit_at, WALL, "退出时刻来自协调器的时钟采样");
    assert!(report.clean_exit_recorded);
    assert_eq!(report.sessions_ended, vec![session_id.clone()]);
    assert_eq!(report.recovering_kept, vec!["s-recovering".to_string()]);

    let db = rig.db();
    let conn = db.connection();
    assert_eq!(
        conn.query_row(
            "SELECT clean_exit_at FROM application_run WHERE id = ?1",
            [&run_id],
            |row| row.get::<_, Option<i64>>(0)
        )
        .unwrap(),
        Some(WALL),
        "`clean_exit_at` 落库——这就是「不是杀进程」的证据"
    );
    assert_eq!(
        conn.query_row(
            "SELECT state FROM work_session WHERE id = ?1",
            [&session_id],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "finished"
    );
    assert_eq!(
        conn.query_row(
            "SELECT state FROM work_session WHERE id = 's-recovering'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "recovering"
    );

    let ticks_after = rig.running.sampling_ticks();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        rig.running.sampling_ticks(),
        ticks_after,
        "退出先停定时器：之后不得再有采样触发"
    );
}

/// 关掉全部窗口**不清核心**：测试进程里本来就没有（也不可能有）窗口对象，
/// 采样照样在跑——F-009 的 Rust 半边。真实关窗行为见 `tests/manual-shell.md`。
#[test]
fn the_core_keeps_running_with_no_window_at_all() {
    let rig = launch_with_interval(10);
    let before = rig.running.sampling_ticks();
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.running.sampling_ticks() < before + 2 {
        assert!(
            Instant::now() < deadline,
            "没有窗口引用时周期采样仍必须被驱动（F-009）"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(
        window::should_prevent_exit(None),
        "而且这一刻的关窗事件（code=None）必须被拦下，否则进程会跟着最后一个窗口一起走"
    );
}
