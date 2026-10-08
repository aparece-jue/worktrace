//! **协调器故障态的检测半边**（P6 Task 2b）。
//!
//! # 这个文件在钉什么
//!
//! `Coordinator::faulted` 一旦置真，`refuse_if_faulted` 会拒绝 `snapshot`/`tick`/`start`/
//! `pause`/`resume`/`finish`/`heartbeat`/`system_pause` 等十个入口 ⇒ 计时在故障后**没有
//! 出口**。四份分工写死（三份计划 + 本任务）：**实现 P2**（`Coordinator::retry_recovery`）、
//! **生产出口 P3**（`AppState::retry_recovery`，S12）、**触发 P8**（用户显式点「重试对账」）、
//! **检测 P6**（本文件）。P6 只做三件，不扩张：
//!
//! | 判据 | 落点 | 本文件的用例 |
//! | --- | --- | --- |
//! | 采样路径**按跃迁**记一条（不是每拍一条） | `sampling_action` → `AppState::observe_timer_availability` | 前两条 |
//! | 启动后第一次采样就故障要**点名** | 同一条路径的第一个观察点 | 第三条 |
//! | 进入/清除各记一条（含 `run_id`、置真点分组名、`wall_ms`） | `platform::diagnostics` 落点（**注入**路径） | 三条都读回日志 |
//!
//! **不做**：不自动重试、不自动清故障、不新增（广播）事件名、不改 `refuse_if_faulted`
//! 的拒绝集合。清除只走 P3 的 `retry_recovery`——第二条用例正是它。
//!
//! # 两条**不同**的故障路径（别混成一条）
//!
//! 本文件只覆盖**路 A**：协调器返回 `Err`（含故障态）而采样线程**还活着**、下一拍还会来
//! （所以 `ticks` 照涨、`sampling_errors` 照涨：后者是「采样失败」的既有口径，不改成
//! 「故障次数」）。**路 B**（`on_tick` panic ⇒ 线程没了、`ticks` 停涨）是看门狗那一半，
//! 用例在 `tests/periodic_sampling.rs`（`Scheduler` 的直接注入），本文件只断言第三条用例里
//! `ticks` 仍在涨、且没有 `sampler.died_unexpectedly`——即"这不是线程死了"。
//!
//! # 装置
//!
//! 真 `startup()`（单实例 → 开库迁移 → 建本次 run → 恢复扫描 → 协调器 → 采样线程）、
//! 假时钟与协调器**共享**一个 `Arc`、诊断日志**注入到临时目录**（绝不写真实数据目录）。
//! 故障注入用两条既有手法：① 一条让 `time_edit` 写不进去的 SQL 触发器（异常事务失败 ⇒
//! 置真点分组 `anomaly_transaction_failed`，写法照 `tests/exception_closure.rs`）；
//! ② 把单调钟拨回去（硬故障 ⇒ `monotonic_backwards`，正常平台永不出现）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, NoProbe, RunningApp, SharedApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::timer::coordinator::StartRequest;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;

/// 用例的挂钟原点。
const WALL: i64 = 1_700_000_000_000;
/// 采样节拍：故障检测要在**采样拍**上被看见，所以压到 10ms。
const FAST_INTERVAL_MS: u64 = 10;
/// 「启动后第一次采样」那条用例要留出**在第一次采样之前**布好故障的时间。
const SLOW_INTERVAL_MS: u64 = 250;
/// 拨给墙钟的越界量（判据是严格大于 2000ms：见 `platform::clock::THRESHOLD_MS`）。
const WALL_JUMP: i64 = 31_000;
/// 把单调钟拨回去的量（硬故障：`d_mono < 0`）。
const MONOTONIC_BACKWARDS: i64 = 5_000;

/// 诊断日志的文件名（**注入的路径**，不是生产那个 `worktrace.log`）。
const LOG_NAME: &str = "diagnostics.log";

// ─────────────────────────────────────────────────────────────────────────────
// 装置
// ─────────────────────────────────────────────────────────────────────────────

struct NoSink;

impl EventSink for NoSink {
    fn broadcast(&self, _envelope: &EventEnvelope) -> Result<(), String> {
        Ok(())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    lock_path: PathBuf,
    log_path: PathBuf,
    /// 与协调器**共享**的假时钟（推进时间始终走平台时钟接缝）。
    clock: Arc<Mutex<FakeClock>>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("worktrace.db");
    let lock_path = dir.path().join("instance.lock");

    let mut db = Db::open(&db_path).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    // 夹具里的裸 SQL 是**装置**，不是被测路径（与 `tests/maintenance_isolation.rs` 同一写法）。
    tx.execute(
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Ready',0,1000,1000)",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);

    Fixture {
        log_path: dir.path().join(LOG_NAME),
        _dir: dir,
        db_path,
        lock_path,
        clock: Arc::new(Mutex::new(FakeClock::new(WALL, 0))),
    }
}

/// 真启动，诊断日志注入到临时目录。
fn started(fx: &Fixture, interval_ms: u64) -> Box<RunningApp> {
    let mut config = StartupConfig::new(&fx.db_path, &fx.lock_path);
    config.sampling_interval_ms = interval_ms;
    let config = config.with_diagnostic_log(&fx.log_path);

    match startup(
        config,
        Box::new(Arc::clone(&fx.clock)),
        Arc::new(NoSink),
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    }
}

fn app_of(running: &RunningApp) -> SharedApp {
    Arc::clone(running.app())
}

/// 起一次计时（夹具里已经有一个 `Ready` 任务）。
fn start_session(app: &SharedApp, epoch: &str) {
    let request = StartRequest {
        expected_data_epoch: epoch.to_string(),
        task_id: "t1".to_string(),
        task_expected_version: 0,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: 1_000,
    };
    lock_app(app).start(request).expect("开始计时");
}

/// 注入「审计写不进去」：异常恢复事务必然失败 ⇒ 协调器进故障态（组 `anomaly_transaction_failed`）。
fn reject_time_edit_writes(app: &SharedApp) {
    lock_app(app)
        .db()
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_time_edit BEFORE INSERT ON time_edit
             BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;",
        )
        .unwrap();
}

fn allow_time_edit_writes(app: &SharedApp) {
    lock_app(app)
        .db()
        .connection()
        .execute_batch("DROP TRIGGER reject_time_edit;")
        .unwrap();
}

/// 诊断日志的每一行（文件不存在 ⇒ 空）。读的是**注入的**路径。
fn log_lines(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

/// 日志里以 `event=<name>` 开头的行。
fn lines_of(path: &Path, event: &str) -> Vec<String> {
    let prefix = format!("event={event}");
    log_lines(path)
        .into_iter()
        .filter(|line| line.starts_with(&prefix))
        .collect()
}

/// 等到谓词成立（或超时返回 false）。
fn wait_until(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// 等采样线程跑够 `n` 拍。
fn wait_for_ticks(running: &RunningApp, n: u64) -> bool {
    wait_until(|| running.sampling_ticks() >= n)
}

// ─────────────────────────────────────────────────────────────────────────────
// ① 采样路径：按**跃迁**记一条，不是每拍一条
// ─────────────────────────────────────────────────────────────────────────────

/// 采样拍自己撞上「异常事务失败」⇒ 进入故障态被**记一条**；故障持续期间连跑多拍，
/// 诊断**不再增行**，而 `sampling_errors` 照涨（它是「采样失败」的既有口径，不是故障次数）。
///
/// 夹具是 **armed** 的：运行中的会话 + 让审计写不进去的触发器 + 越过阈值的墙钟跳变 ⇒
/// 下一拍必然走 P2 的异常路径且那笔系统事务必然失败。用例开头先断言**故障之前**日志里
/// 一行「不可用」都没有（正控：不是一开始就坏）。
#[test]
fn a_fault_caused_by_the_sampler_is_recorded_once_per_transition() {
    let fx = fixture();
    let running = started(&fx, FAST_INTERVAL_MS);
    let app = app_of(&running);
    let epoch = running.data_epoch().to_string();

    // 拨动之前先让采样真的跑起来：健康拍不写任何「不可用」的行。
    assert!(wait_for_ticks(&running, 2), "采样线程必须真的在跑");
    assert!(
        lines_of(&fx.log_path, "timer.unavailable.begin").is_empty(),
        "健康采样不得写「计时不可用」：{:?}",
        log_lines(&fx.log_path)
    );
    assert!(lock_app(&app).snapshot().is_ok(), "故障之前命令照常成功");
    assert!(!lock_app(&app).timer_faulted());

    start_session(&app, &epoch);
    reject_time_edit_writes(&app);
    // 拨过阈值：下一拍采样的检测必然判成墙钟异常，而那笔恢复事务写不进审计。
    fx.clock.lock().unwrap().advance_wall(WALL_JUMP);

    assert!(
        wait_until(|| !lines_of(&fx.log_path, "timer.unavailable.begin").is_empty()),
        "采样路径必须把「进入故障态」记进正式诊断：{:?}",
        log_lines(&fx.log_path)
    );
    assert!(
        lock_app(&app).timer_faulted(),
        "timer_faulted() 必须照实为真（复合语义：故障态或提交后待刷新）"
    );

    // 一条记录里要有：run_id、置真点分组名、wall_ms、观察点，以及**照实**的复合文案。
    let line = lines_of(&fx.log_path, "timer.unavailable.begin")[0].clone();
    assert!(
        line.contains(&format!("run_id={}", running.run_id())),
        "{line}"
    );
    assert!(line.contains("wall_ms="), "{line}");
    assert!(line.contains("entry=sample_tick"), "{line}");
    assert!(
        line.contains("origin=anomaly_transaction_failed"),
        "采样路径撞上的是「异常事务失败」那一组：{line}"
    );
    assert!(
        line.contains("reason=计时不可用（故障态或提交后待刷新）"),
        "P6-3：文案照实投影复合语义，不得写成「进入故障态」：{line}"
    );

    // **只记一条**：故障持续期间连跑五拍以上，日志不增行，而 sampling_errors 照涨。
    let ticks_before = running.sampling_ticks();
    let errors_before = running.sampling_errors();
    assert!(
        wait_for_ticks(&running, ticks_before + 5),
        "故障态不停止调度器：ticks 照涨"
    );
    assert!(
        running.sampling_errors() > errors_before,
        "sampling_errors 口径不变：故障期间每一拍仍然是一次采样失败（{errors_before} -> {}）",
        running.sampling_errors()
    );
    assert_eq!(
        lines_of(&fx.log_path, "timer.unavailable.begin").len(),
        1,
        "持续期间不得重复刷（1 秒一拍会刷爆诊断）：{:?}",
        log_lines(&fx.log_path)
    );

    // 既有主通道不变：命令仍然拿 `RECOVERY_REQUIRED`。
    let error = lock_app(&app).snapshot().unwrap_err();
    assert_eq!(error.code(), "RECOVERY_REQUIRED");
    assert_eq!(
        lines_of(&fx.log_path, "timer.unavailable.end").len(),
        0,
        "没清除就不该有「清除」的行"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ② 清除：用户显式重试成功 ⇒ 清除各记一条（且只有一条）
// ─────────────────────────────────────────────────────────────────────────────

/// 与上一条同样的注入进入故障态；排障之后调 **P3 的 `retry_recovery`**（S12，清除的唯一出口）
/// ⇒ 故障态清除、`timer_faulted()` 转假、日志里**恰好一条**「清除」。
///
/// 断言 `entry=retry_recovery` 是有意的：清除记录必须由**清除入口自己**落（用户点完
/// 「重试对账」日志里立刻有结论），而不是只等下一拍采样顺手发现。
#[test]
fn retry_recovery_clears_the_fault_and_records_exactly_one_recovery_line() {
    let fx = fixture();
    let running = started(&fx, FAST_INTERVAL_MS);
    let app = app_of(&running);
    let epoch = running.data_epoch().to_string();

    // 先让**健康**的采样拍跑起来（否则第一次观察就可能撞上故障，进入记录会变成
    // `startup.timer_unavailable`——那是第三条用例的形态）。
    assert!(wait_for_ticks(&running, 2), "采样线程必须真的在跑");
    assert!(
        lines_of(&fx.log_path, "timer.unavailable.begin").is_empty(),
        "健康采样不得写「计时不可用」：{:?}",
        log_lines(&fx.log_path)
    );

    start_session(&app, &epoch);
    reject_time_edit_writes(&app);
    fx.clock.lock().unwrap().advance_wall(WALL_JUMP);
    assert!(
        wait_until(|| !lines_of(&fx.log_path, "timer.unavailable.begin").is_empty()),
        "先要真的进入故障态：{:?}",
        log_lines(&fx.log_path)
    );
    assert!(lock_app(&app).timer_faulted());

    // 排障之后重试：那笔恢复事务提交一次，故障态清除。
    allow_time_edit_writes(&app);
    let snapshot = lock_app(&app)
        .retry_recovery(&epoch)
        .expect("排障之后重试应当成功");
    assert!(
        snapshot.session_id.is_some(),
        "恢复事务之后仍镜像着那条会话"
    );
    assert!(
        !lock_app(&app).timer_faulted(),
        "清除之后 timer_faulted() 必须转假"
    );

    let ends = lines_of(&fx.log_path, "timer.unavailable.end");
    assert_eq!(ends.len(), 1, "清除只记一条：{:?}", log_lines(&fx.log_path));
    assert!(
        ends[0].contains(&format!("run_id={}", running.run_id())),
        "{}",
        ends[0]
    );
    assert!(ends[0].contains("entry=retry_recovery"), "{}", ends[0]);
    assert!(ends[0].contains("wall_ms="), "{}", ends[0]);
    assert_eq!(
        lines_of(&fx.log_path, "timer.unavailable.begin").len(),
        1,
        "进入仍然只有一条：{:?}",
        log_lines(&fx.log_path)
    );

    // 之后连跑多拍：没有新的跃迁 ⇒ 两个方向都不再增行。
    let ticks_before = running.sampling_ticks();
    assert!(wait_for_ticks(&running, ticks_before + 5));
    assert_eq!(lines_of(&fx.log_path, "timer.unavailable.begin").len(), 1);
    assert_eq!(lines_of(&fx.log_path, "timer.unavailable.end").len(), 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// ③ 启动路径：启动后**第一次**采样就已经故障 ⇒ 点名
// ─────────────────────────────────────────────────────────────────────────────

/// 采样节拍 250ms，留出「启动之后、第一次采样之前」布故障的窗口：把单调钟拨回去
/// ⇒ 第一次采样就是硬故障。启动诊断必须**点名**（否则用户看到「刚打开就不能计时」
/// 却不知道原因），而且它是**同一个跃迁的启动形态**——不会再补一条普通的进入行。
///
/// 顺带钉住两条「两条路径不同」的判据：`ticks` 照涨（线程活着 ⇒ 这是路 A，不是 panic）
/// 且没有 `sampler.died_unexpectedly`。
#[test]
fn the_first_sample_after_startup_names_an_immediate_fault() {
    let fx = fixture();
    let running = started(&fx, SLOW_INTERVAL_MS);

    // 第一次采样之前布好故障：单调读数倒退（`d_mono < 0`）是硬故障，正常平台永不出现。
    fx.clock
        .lock()
        .unwrap()
        .advance_monotonic(-MONOTONIC_BACKWARDS);

    assert!(wait_for_ticks(&running, 1), "采样线程必须真的跑起来");
    assert!(
        wait_until(|| !lines_of(&fx.log_path, "startup.timer_unavailable").is_empty()),
        "启动后第一次采样就故障必须在启动诊断里点名：{:?}",
        log_lines(&fx.log_path)
    );

    let line = lines_of(&fx.log_path, "startup.timer_unavailable")[0].clone();
    assert!(
        line.contains(&format!("run_id={}", running.run_id())),
        "{line}"
    );
    assert!(line.contains("entry=sample_tick"), "{line}");
    assert!(line.contains("origin=monotonic_backwards"), "{line}");
    assert!(
        line.contains("reason=计时不可用（故障态或提交后待刷新）"),
        "{line}"
    );
    assert!(
        lines_of(&fx.log_path, "timer.unavailable.begin").is_empty(),
        "点名就是那**一条**进入记录，不得再补一条：{:?}",
        log_lines(&fx.log_path)
    );
    assert!(lock_app(&app_of(&running)).timer_faulted());

    // 路 A（协调器返回 Err、线程还活着）：ticks 照涨，看门狗那半边**不**发声。
    let ticks_before = running.sampling_ticks();
    assert!(wait_for_ticks(&running, ticks_before + 3), "ticks 照涨");
    assert!(
        !running.sampling_died_unexpectedly(),
        "线程没死，看门狗不得误报"
    );
    assert!(
        lines_of(&fx.log_path, "sampler.died_unexpectedly").is_empty(),
        "看门狗不得为一条只是「故障态」的路径写行：{:?}",
        log_lines(&fx.log_path)
    );
}
