//! **维护态与隔离 + 退出意图互斥 + 诊断落点**（P6 Task 2a）。
//!
//! # 这个文件在钉什么
//!
//! 维护态的标志放在**串行边界之内**的 `AppState` 里（与 `db` 同一临界区，不新增第二把锁）：
//!
//! | 判据 | 唯一落点 | 本文件的用例 |
//! | --- | --- | --- |
//! | 采样整拍跳过 | `sampling_action` 取锁后第一句（`tests/periodic_sampling.rs`） | —— |
//! | 写入门禁 | `run_command`（需要 Tauri 运行时，**本文件用命令体同一条门禁代替**）、两条托盘路径、四个统计/导出入口、`retry_recovery` | 下面五条 |
//! | 退出意图 | `RunningApp::shutdown` → `AppState::begin_exit`（在 `sampling.stop()` **之前**） | 一条 |
//! | 诊断落点 | `platform::diagnostics`（注入路径，读回内容） | 两条 |
//!
//! **命令包装（`run_command`）本身不在本文件的覆盖范围内**：`#[tauri::command]` 生成的包装
//! 要 Tauri 运行时（`tauri/test` 的 `mock_builder`）才能调，本仓没有启用那个 feature
//! （见 `tests/ipc_commands.rs` 的文件头，同一处遗留）。命令层的门禁与托盘一样，是
//! **取锁之后的第一句**——托盘那条路径可以在这里直接调（`tray_pause_impl` 收 `&mut AppState`），
//! 命令包装那一条由评审核对 diff。
//!
//! # 断言口径
//!
//! 每条断言写**具体数值**：进入时刻、时长、`revision`、连接级 `total_changes()`、行数、
//! 会话状态、日志行的逐字内容。**并且**关键用例都配一条**正控**（维护态退出之后同一条
//! 路径真的恢复了：托盘暂停真的暂停、统计入口真的撞上 P2 的异常事务）——否则"被挡住"
//! 可能只是"这条路径本来就不干活"。
//!
//! # 装置
//!
//! 真 `startup()`（单实例 → 开库迁移 → 建本次 run → 恢复扫描 → 协调器 → 采样线程），
//! 假时钟与协调器**共享**一个 `Arc`。除 `shutdown` 那条用例（它要一个真在跑的采样线程）
//! 之外，采样节拍都放到 1 小时：周期线程先睡后跑，用例期间一拍都不落，时刻编排完全由
//! 用例自己决定。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use worktrace_lib::commands::{tray_pause_impl, TrayPause};
use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, AppState, MaintenancePhase, NoProbe, RunningApp, SharedApp, Startup,
    StartupConfig, DIAGNOSTIC_LOG_FILE,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::export::WeeklyQuery;
use worktrace_lib::services::stats::{StatsRangeQuery, TodayQuery};
use worktrace_lib::services::timer::coordinator::StartRequest;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;

/// 用例的挂钟原点：2026-03-10 12:00（+08:00）。
const WALL: i64 = 1_773_115_200_000;
/// 今天 2026-03-10 00:00 —— 范围查询的半开起点。
const TUE_MID: i64 = 1_773_072_000_000;
/// 明天 2026-03-11 00:00 —— 半开终点。
const WED_MID: i64 = 1_773_158_400_000;
const TZ: &str = "Asia/Shanghai";

/// 采样节拍：用例自己编排时刻时用 1 小时（周期线程一拍都不落）。
const IDLE_INTERVAL_MS: u64 = 3_600_000;
/// `shutdown` 那条用例要一个**真在跑**的采样线程。
const FAST_INTERVAL_MS: u64 = 10;

const ONE_MINUTE: i64 = 60_000;
/// 拨给墙钟的越界量（判据是严格大于 2000ms：见 `platform::clock::THRESHOLD_MS`）。
const WALL_JUMP: i64 = 5_000;
/// 同一次调用里推进的单调钟：让"余段的候选终点"严格晚于可信前缀终点。
const MONOTONIC_STEP: i64 = 1_000;

/// 临时目录里诊断日志的文件名（**注入的路径**，不是生产那个名字）。
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
    // 夹具里的裸 SQL 是**装置**，不是被测路径（与 `tests/today.rs` 同一写法）。
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

/// 真启动。诊断日志**注入到临时目录**（绝不写真实数据目录）。
fn started(fx: &Fixture, interval_ms: u64, log: bool) -> Box<RunningApp> {
    let mut config = StartupConfig::new(&fx.db_path, &fx.lock_path);
    config.sampling_interval_ms = interval_ms;
    if log {
        config = config.with_diagnostic_log(&fx.log_path);
    }

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

/// 起一次计时（夹具里已经有一个 `Ready` 任务；`start` 不需要项目）。
fn start_session(app: &SharedApp, epoch: &str) {
    let request = StartRequest {
        expected_data_epoch: epoch.to_string(),
        task_id: "t1".to_string(),
        task_expected_version: 0,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: 30_000,
    };
    lock_app(app).start(request).expect("开始计时");
}

// ─────────────────────────────────────────────────────────────────────────────
// 请求与只读探针
// ─────────────────────────────────────────────────────────────────────────────

fn today_query(epoch: &str) -> TodayQuery {
    TodayQuery {
        timezone: TZ.to_string(),
        expected_data_epoch: epoch.to_string(),
    }
}

fn range_query(epoch: &str) -> StatsRangeQuery {
    StatsRangeQuery {
        from: TUE_MID,
        to: WED_MID,
        timezone: TZ.to_string(),
        expected_data_epoch: epoch.to_string(),
    }
}

fn weekly_query(epoch: &str) -> WeeklyQuery {
    WeeklyQuery {
        timezone: TZ.to_string(),
        anchor: None,
        expected_data_epoch: epoch.to_string(),
    }
}

/// 统计 / 导出的**四个** `&mut self` 入口（成功类型各不相同，这里统一抹成 `Result<(), _>`：
/// 本文件只问一件事——维护态有没有在**取样本之前**把它们挡住）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entry {
    Snapshot,
    Today,
    Json,
    Weekly,
}

const ENTRIES: [Entry; 4] = [Entry::Snapshot, Entry::Today, Entry::Json, Entry::Weekly];

impl Entry {
    fn name(self) -> &'static str {
        match self {
            Entry::Snapshot => "stats_snapshot",
            Entry::Today => "stats_today",
            Entry::Json => "export_json",
            Entry::Weekly => "export_weekly_markdown",
        }
    }

    fn call(self, state: &mut AppState, epoch: &str) -> Result<(), AppError> {
        match self {
            Entry::Snapshot => state.stats_snapshot(&range_query(epoch)).map(|_| ()),
            Entry::Today => state.stats_today(&today_query(epoch)).map(|_| ()),
            Entry::Json => state.export_json(&range_query(epoch)).map(|_| ()),
            Entry::Weekly => state
                .export_weekly_markdown(&weekly_query(epoch))
                .map(|_| ()),
        }
    }
}

/// 一次**只读**的库内事实快照：维护态期间它必须一动不动。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Probe {
    revision: i64,
    /// 连接级累计写入行数（抓得住 UPDATE，也抓得住回滚掉的写入尝试）。
    total_changes: i64,
    sessions: i64,
    running: i64,
    recovering: i64,
    audit: i64,
    checkpoints: i64,
}

/// 探针取在 App **自己那条连接**上（新连接上 `total_changes()` 恒为 0）。
fn probe(app: &SharedApp) -> Probe {
    let state = lock_app(app);
    let conn = state.db().connection();
    let scalar = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    Probe {
        revision: scalar("SELECT revision FROM app_meta WHERE singleton = 1"),
        total_changes: scalar("SELECT total_changes()"),
        sessions: scalar("SELECT COUNT(*) FROM work_session"),
        running: scalar("SELECT COUNT(*) FROM work_session WHERE state = 'running'"),
        recovering: scalar("SELECT COUNT(*) FROM work_session WHERE state = 'recovering'"),
        audit: scalar("SELECT COUNT(*) FROM time_edit"),
        checkpoints: scalar("SELECT COUNT(*) FROM interval_checkpoint"),
    }
}

fn begin(app: &SharedApp, at_ms: i64) {
    lock_app(app)
        .begin_maintenance(MaintenancePhase::Restore, at_ms)
        .expect("进入维护态");
}

fn end(app: &SharedApp) {
    lock_app(app).end_maintenance().expect("退出维护态");
}

fn log_lines(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.lines().map(|line| line.to_string()).collect(),
        // 文件不存在 ⇒ 没有记录（这就是"零记录"的判据，不是错误）。
        Err(_) => Vec::new(),
    }
}

/// 断言这是一条**维护态拒绝**：本阶段用的是临时的内部错误
/// （`STORAGE_ERROR` + `maintenance:` 前缀的 detail）。
///
/// **TODO（P6 Task 4）**：换成 `DATA_RESTORE_IN_PROGRESS` 之后要改的就是这一个函数
/// 两条维护态采样用例不涉及错误码（只断言零写入与采样计数），无需改动。
fn assert_maintenance_refusal(error: &AppError) {
    assert_eq!(error.code(), "STORAGE_ERROR", "实际：{error:?}");
    let detail = error.detail().unwrap_or_default();
    assert!(
        detail.starts_with("maintenance:"),
        "维护态拒绝的 detail 必须以 maintenance: 开头，实际：{detail}"
    );
}

fn wait_for_ticks(running: &RunningApp, n: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while running.sampling_ticks() < n {
        assert!(
            Instant::now() < deadline,
            "采样驱动没有按周期触发：只跑了 {} 拍",
            running.sampling_ticks()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 维护态本身：重入拒绝 / 幂等清位 / 门禁恢复
// ─────────────────────────────────────────────────────────────────────────────

/// 已在维护态 ⇒ `begin_maintenance` **拒绝**，且**不覆盖**第一次的进入时刻。
#[test]
fn begin_maintenance_refuses_re_entrance_without_overwriting_the_entry() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS, false);
    let app = app_of(&running);

    begin(&app, WALL);
    {
        let state = lock_app(&app);
        let current = state.maintenance().expect("维护态已置位");
        assert_eq!(current.phase(), MaintenancePhase::Restore);
        assert_eq!(current.entered_at_ms(), WALL);
    }

    let later = WALL + ONE_MINUTE;
    let error = lock_app(&app)
        .begin_maintenance(MaintenancePhase::Restore, later)
        .expect_err("重入必须被拒");
    assert_maintenance_refusal(&error);

    let state = lock_app(&app);
    let current = state.maintenance().expect("拒绝重入不得把维护态清掉");
    assert_eq!(current.entered_at_ms(), WALL, "拒绝重入也不得覆盖进入时刻");
    assert!(!state.sampling_allowed(), "维护态期间不允许采样");
}

/// `end_maintenance` **幂等**：第一次交回被清掉的记录，之后是 `None`。
#[test]
fn end_maintenance_is_idempotent_and_returns_the_cleared_record() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS, false);
    let app = app_of(&running);

    begin(&app, WALL);
    let cleared = lock_app(&app).end_maintenance().expect("第一次清位有记录");
    assert_eq!(cleared.phase(), MaintenancePhase::Restore);
    assert_eq!(cleared.entered_at_ms(), WALL);

    let state = lock_app(&app);
    assert!(state.maintenance().is_none(), "清位之后不该还有维护态");
    assert!(state.sampling_allowed(), "清位之后采样恢复");
    drop(state);
    assert!(
        lock_app(&app).end_maintenance().is_none(),
        "已清则 None（幂等）"
    );
}

/// `guard_writable` 在维护态拒绝、退出后恢复。
#[test]
fn guard_writable_refuses_in_maintenance_and_recovers_after() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS, false);
    let app = app_of(&running);

    assert!(lock_app(&app).guard_writable().is_ok(), "平时可写");

    begin(&app, WALL);
    let error = lock_app(&app).guard_writable().expect_err("维护态必须拒绝");
    assert_maintenance_refusal(&error);

    end(&app);
    assert!(
        lock_app(&app).guard_writable().is_ok(),
        "维护态退出之后写入口恢复"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 退出意图：互斥与顺序
// ─────────────────────────────────────────────────────────────────────────────

/// **两个方向**都互斥：维护态拒绝退出意图，退出意图拒绝维护态。
///
/// 顺带钉住"拒绝不留副作用"：被拒的那一方**没有**置位（用另一方的下一次调用作判据）。
#[test]
fn maintenance_and_exit_intent_refuse_each_other_in_both_directions() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS, false);
    let app = app_of(&running);

    // 方向一：维护态 ⇒ `begin_exit` 拒绝（不碰采样线程，见下一条用例）。
    begin(&app, WALL);
    let error = lock_app(&app)
        .begin_exit()
        .expect_err("维护态下不得置退出意图");
    assert_maintenance_refusal(&error);
    {
        let state = lock_app(&app);
        assert!(state.maintenance().is_some(), "拒绝退出不得改动维护态");
    }
    end(&app);
    // 退出意图**没有**被置上：如果被置上了，下面这一句会失败。
    begin(&app, WALL + ONE_MINUTE);
    end(&app);

    // 方向二：退出意图 ⇒ `begin_maintenance` 拒绝。
    {
        let mut state = lock_app(&app);
        state.begin_exit().expect("锁外第一次置退出意图");
        let error = state
            .begin_maintenance(MaintenancePhase::Restore, WALL + 2 * ONE_MINUTE)
            .expect_err("退出意图已置位时不得进入维护态");
        assert_maintenance_refusal(&error);
        assert!(state.maintenance().is_none(), "拒绝进入不得留下维护态");
        // 退出是终态：重复置位幂等（不改语义、也不报错）。
        state.begin_exit().expect("重复置退出意图是幂等的");
    }
}

/// **`shutdown` 的顺序**：退出意图先于 `sampling.stop()`。
///
/// 判据是行为性的：维护态下 `shutdown` 被拒之后**采样线程仍在跑**（`ticks` 还在涨）、
/// 退出事务一行没写。若顺序反了（先 `stop()` 再发现维护态），`stop()` 不可逆 ⇒
/// 采样永久停、退出事务没跑、进程只能强杀。
#[test]
fn shutdown_is_refused_in_maintenance_before_the_sampler_is_stopped() {
    let fx = fixture();
    let running = started(&fx, FAST_INTERVAL_MS, false);
    let app = app_of(&running);
    let epoch = running.data_epoch().to_string();

    wait_for_ticks(&running, 2);
    start_session(&app, &epoch);
    let before = probe(&app);
    assert_eq!(before.running, 1, "夹具里应当有一个正在跑的会话");

    begin(&app, WALL);
    let ticks_before = running.sampling_ticks();

    let error = running.shutdown().expect_err("维护态下必须拒绝退出");
    assert_maintenance_refusal(&error);

    // 采样线程仍在跑（`stop()` 不可逆：顺序反了这里就不再涨）。
    wait_for_ticks(&running, ticks_before + 3);
    assert!(
        running.sampling_ticks() > ticks_before,
        "拒绝退出之后采样线程必须仍在跑：{ticks_before} -> {}",
        running.sampling_ticks()
    );
    // 退出事务一行没写：会话仍 running、clean_exit_at 仍为空。
    assert_eq!(probe(&app), before, "被拒的退出不得写任何东西");
    let clean_exit_at: Option<i64> = lock_app(&app)
        .db()
        .connection()
        .query_row(
            "SELECT clean_exit_at FROM application_run WHERE id = ?1",
            [running.run_id()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(clean_exit_at, None, "被拒的退出不得写 clean_exit_at");

    // 拒绝不是终态：维护结束之后同一条入口照常退出。
    end(&app);
    let report = running.shutdown().expect("维护结束后退出应当成功");
    assert_eq!(report.run_id, running.run_id());
    assert_eq!(report.sessions_ended.len(), 1, "那个 running 会话被结束");
}

// ─────────────────────────────────────────────────────────────────────────────
// 四个统计/导出入口：维护态必须在**取样本之前**挡住
// ─────────────────────────────────────────────────────────────────────────────

/// **维护态 + 库里有事实**：四个入口必须被挡在采样之前——既没有产出数字，
/// 也没有经 P2 的异常事务写库。
///
/// 夹具是**armed** 的：一个正在跑的会话 + 一次越界墙钟跳变 ⇒ 样本一取就会走 P2 的
/// 异常路径（分割区间、写 `time_edit`、会话置 `recovering`、`revision + 1`）。
/// 所以"四个入口一行都没写"是有判别力的；用例末尾的**正控**（退出维护态后再调一次，
/// 那笔事务真的落库）证明这个夹具本来就会写。
#[test]
fn the_four_statistics_and_export_entries_are_refused_before_the_sample() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS, false);
    let app = app_of(&running);
    let epoch = running.data_epoch().to_string();

    // ① 起计时、走满 1 分钟并落一个检查点：异常分割的可信前缀就是它。
    start_session(&app, &epoch);
    fx.clock.lock().unwrap().advance_both(ONE_MINUTE);
    lock_app(&app).sample_tick().expect("周期采样应当成功");
    let armed_revision = probe(&app).revision;

    // ② 拨过阈值：下一次取样本必然撞上墙钟异常。
    {
        let mut clock = fx.clock.lock().unwrap();
        clock.advance_monotonic(MONOTONIC_STEP);
        clock.advance_wall(WALL_JUMP + MONOTONIC_STEP);
    }

    // ③ 进入维护态，四个入口逐个来一次。
    begin(&app, WALL + ONE_MINUTE);
    let before = probe(&app);
    for entry in ENTRIES {
        let error = {
            let mut state = lock_app(&app);
            entry.call(&mut state, &epoch).expect_err("维护态必须拒绝")
        };
        assert_maintenance_refusal(&error);
        assert_ne!(
            error.code(),
            "RECOVERY_REQUIRED",
            "{} 必须在取样本**之前**被维护态挡住，而不是先在协调器里撞上恢复语义",
            entry.name()
        );
    }

    // ④ 一行都没写：那笔恢复事务没有发生（`revision` 不动、无审计、会话仍 running）。
    assert_eq!(
        probe(&app),
        before,
        "四个入口在维护态下必须零写入（它们不是只读查询：正常会经 P2 的异常事务写库）"
    );
    assert_eq!(before.revision, armed_revision, "探针自己不该动 revision");

    // ⑤ 正控：退出维护态之后再调一次，同样的夹具**真的**会写那笔异常事务。
    end(&app);
    let error = {
        let mut state = lock_app(&app);
        state
            .stats_snapshot(&range_query(&epoch))
            .expect_err("墙钟异常必须原样交出来")
    };
    assert_eq!(error.code(), "RECOVERY_REQUIRED");
    let after = probe(&app);
    assert_eq!(
        after.revision,
        before.revision + 1,
        "一次异常恰好一次版本——这条正控证明夹具本来就是 armed 的"
    );
    assert_eq!(after.audit, before.audit + 1, "恰好一条异常审计");
    assert_eq!(after.recovering, 1, "会话被置成 recovering");
    assert!(
        after.total_changes > before.total_changes,
        "那笔恢复事务真的落了库：{before:?} -> {after:?}"
    );
}

/// `retry_recovery` 走**同一条**维护判据（它是要重试一笔恢复事务的写入口，
/// 不能因为"用户显式触发"就放行）。
#[test]
fn retry_recovery_is_refused_in_maintenance() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS, false);
    let app = app_of(&running);
    let epoch = running.data_epoch().to_string();

    begin(&app, WALL);
    let before = probe(&app);
    let error = {
        let mut state = lock_app(&app);
        state
            .retry_recovery(&epoch)
            .expect_err("维护态必须拒绝重试对账")
    };
    assert_maintenance_refusal(&error);
    assert_eq!(probe(&app), before, "被拒的重试不得写任何东西");

    // 正控：退出维护态后这条入口不再返回维护态拒绝（这里没有故障 ⇒ Ok）。
    end(&app);
    let mut state = lock_app(&app);
    state
        .retry_recovery(&epoch)
        .expect("没有待重试的故障时应当成功");
}

// ─────────────────────────────────────────────────────────────────────────────
// 托盘：绕过 `run_command` 的那条暂停路径
// ─────────────────────────────────────────────────────────────────────────────

/// 托盘「暂停」自己判维护态（它**不走** `run_command`）：维护态下拒绝且库零写入；
/// 维护态结束之后同一条路径照常暂停（正控）。
#[test]
fn tray_pause_is_refused_in_maintenance_and_writes_nothing() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS, false);
    let app = app_of(&running);
    let epoch = running.data_epoch().to_string();

    start_session(&app, &epoch);
    let before = probe(&app);
    assert_eq!(before.running, 1);

    begin(&app, WALL);
    let error = {
        let mut state = lock_app(&app);
        tray_pause_impl(&mut state, running.broadcaster()).expect_err("维护态必须拒绝托盘暂停")
    };
    assert_maintenance_refusal(&error);
    assert_eq!(probe(&app), before, "被拒的托盘暂停不得写任何东西");

    // 正控：维护态结束之后，同一条路径真的把会话暂停了。
    end(&app);
    let outcome = {
        let mut state = lock_app(&app);
        tray_pause_impl(&mut state, running.broadcaster()).expect("维护结束后托盘暂停应当生效")
    };
    assert!(
        matches!(outcome, TrayPause::Paused(_)),
        "托盘暂停应当真的暂停会话：{outcome:?}"
    );
    let after = probe(&app);
    assert_eq!(after.running, 0, "会话不再 running");
    assert!(after.revision > before.revision, "暂停是一次业务写");
}

// ─────────────────────────────────────────────────────────────────────────────
// 正式诊断日志：进入/退出各一条，写进**注入的**路径
// ─────────────────────────────────────────────────────────────────────────────

/// 维护态的进入/退出各记一条（含 `phase`、`entered_at_ms`、时长），
/// 且只写进**注入的**路径（测试绝不碰真实数据目录）。
#[test]
fn maintenance_entry_and_exit_are_recorded_once_with_values() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS, true);
    let app = app_of(&running);

    assert!(log_lines(&fx.log_path).is_empty(), "启动本身不记维护态的行");

    begin(&app, WALL);
    // 维护窗口走 5 秒（挂钟与单调钟一起走，不构成异常）。
    fx.clock.lock().unwrap().advance_both(5_000);
    end(&app);

    let lines = log_lines(&fx.log_path);
    assert_eq!(lines.len(), 2, "进入/退出各一条：{lines:?}");
    assert_eq!(
        lines[0],
        format!("event=maintenance.begin phase=Restore entered_at_ms={WALL}")
    );
    assert_eq!(
        lines[1],
        format!(
            "event=maintenance.end phase=Restore entered_at_ms={WALL} \
             left_at_ms={} duration_ms=5000",
            WALL + 5_000
        )
    );

    // 幂等的第二次清位**不再**记一行（否则日志会说"退出了两次"）。
    assert!(lock_app(&app).end_maintenance().is_none());
    assert_eq!(log_lines(&fx.log_path).len(), 2);
}

/// 显式路径构造（`new`）**默认关闭**诊断落盘：夹具忘了注入也不会写进真实数据目录
/// （Task 1 的变异实测过那种污染）。生产走 `from_app_paths()`，它必须开启。
#[test]
fn the_diagnostic_log_is_off_by_default_and_on_for_the_production_paths() {
    let fx = fixture();
    let explicit = StartupConfig::new(&fx.db_path, &fx.lock_path);
    assert!(
        explicit.diagnostic_log.is_none(),
        "显式路径构造必须默认关闭诊断落盘"
    );

    // 只解析路径，**不写任何文件**。
    let production = StartupConfig::from_app_paths().expect("应有 APPDATA / XDG_DATA_HOME / HOME");
    let log = production
        .diagnostic_log
        .expect("生产路径必须开启正式诊断日志");
    assert_eq!(
        log.file_name().and_then(|name| name.to_str()),
        Some(DIAGNOSTIC_LOG_FILE)
    );
    assert_eq!(
        log.parent(),
        production.db_path.parent(),
        "诊断日志与库、锁同目录"
    );
}
