//! **正式 OS 事件源：接线、维护态同一判据、失败路径**（P6 Task 2c）。
//!
//! # 这个文件在钉什么
//!
//! 事件的归属路径是**与周期采样并列的第二条**，不是同一条：
//!
//! ```text
//! platform::system_events::spawn（Windows：隐藏消息窗 + 三类系统通知）
//!   ↓ SystemEvent { kind, boundary: Option<ClockSample> }
//! 组合根（services::bootstrap::start_system_events）
//!   ↓ lock_app（**排队等锁**，不是 try_lock）→ AppState::sampling_allowed()（与采样同一判据）
//!   ↓ AppState::system_boundary(Option<ClockSample>) → Coordinator::system_pause
//!   ↓ 有会话才 Broadcaster::emit(timer.tick)
//! ```
//!
//! | 判据 | 本文件的用例 |
//! | --- | --- |
//! | 可信离开边界 ⇒ `paused`；边界未知 ⇒ `recovering` | ①② |
//! | 唤醒**不自动继续** | ③ |
//! | 事件与周期采样交错**不产生双重分割** | ④ |
//! | 取不到锁**排队等**，拿到之后按当时状态重判（不丢弃、不 panic） | ⑤⑥ |
//! | 维护态判据与采样同一处（整事件跳过 + 入口自身拒一次） | ⑦ |
//! | 事件源注册失败 ⇒ 只记诊断、不 panic、周期采样照常 | ⑧ |
//! | 可注入事件源端到端进同一把锁 | ⑨ |
//! | 事件线程意外退出的可观察出口 | ⑩ |
//! | 退出路径不因事件线程堵在锁上而自死锁 | ⑪ |
//!
//! **不依赖真实 30 秒、也不依赖真实 OS 通知**：时钟是 `FakeClock`，事件源是注入的脚本源；
//! 只有 `platform::system_events` 自己的单元用例会碰真 Windows 消息窗（那一条也不依赖
//! 系统真的锁屏——是**自己往窗口投消息**）。
//!
//! **实机归 P8**：锁屏 30 分钟、休眠/唤醒、正反改时的到达延迟与行为，只能靠实机复核
//! （`docs/validation/pre-p6-closure.md` 第 10 条），本文件不声称那半边。
//!
//! # 装置
//!
//! 真 `startup()`（单实例 → 开库迁移 → 建本次 run → 恢复扫描 → 协调器 → 采样线程），
//! 假时钟与协调器共享一个 `Arc`。采样节拍默认放到 1 小时：周期线程先睡后跑，用例期间
//! 一拍都不落，时刻编排完全由用例自己决定（只有明确要"采样在跑"的两条用 10ms）。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::{Clock, ClockSample, FakeClock};
use worktrace_lib::platform::diagnostics::Diagnostics;
use worktrace_lib::platform::system_events::{SystemEvent, SystemEventKind, SystemEventSource};
use worktrace_lib::services::bootstrap::{
    is_maintenance_refusal, lock_app, start_system_events, startup, system_events_action,
    MaintenancePhase, NoProbe, RunningApp, SharedApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink, EVENT_TIMER_TICK};
use worktrace_lib::services::timer::coordinator::StartRequest;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::session_repo;

/// 用例的挂钟原点：2026-03-10 12:00（+08:00）。
const WALL: i64 = 1_773_115_200_000;

/// 采样节拍：用例自己编排时刻时用 1 小时（周期线程一拍都不落）。
const IDLE_INTERVAL_MS: u64 = 3_600_000;
/// 「周期采样真的在跑」的两条用例用 10ms。
const FAST_INTERVAL_MS: u64 = 10;

/// 会话声明的采样间隔（P2 的长间隔阈值是它的 3 倍）。
const EXPECTED_INTERVAL_MS: i64 = 30_000;

/// 临时目录里诊断日志的文件名（**注入的路径**，不是生产那个名字）。
const LOG_NAME: &str = "diagnostics.log";

/// 等异步效果的上限。事件线程要取锁、要过消息循环，给足余量但不无限等。
const DEADLINE: Duration = Duration::from_secs(10);

// ─────────────────────────────────────────────────────────────────────────────
// 注入的事件源
// ─────────────────────────────────────────────────────────────────────────────

/// 脚本事件源：事件由用例经通道喂进来（**不依赖真实 OS 通知**）。
struct ScriptedSource {
    rx: Receiver<SystemEvent>,
    /// `run` 返回前置位——用例据此断言「停止信号之后线程真的退出了」。
    stopped: Arc<AtomicBool>,
}

impl SystemEventSource for ScriptedSource {
    fn name(&self) -> &'static str {
        "scripted"
    }

    fn start(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn run(&mut self, alive: &AtomicBool, emit: &mut dyn FnMut(SystemEvent)) -> io::Result<()> {
        while alive.load(Ordering::SeqCst) {
            match self.rx.recv_timeout(Duration::from_millis(10)) {
                Ok(event) => emit(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// 注册失败的事件源（平台不支持 / 监听注册失败的那条路径）。
struct FailingSource;

impl SystemEventSource for FailingSource {
    fn name(&self) -> &'static str {
        "failing"
    }

    fn start(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "probe: this platform has no listener",
        ))
    }

    fn run(&mut self, _alive: &AtomicBool, _emit: &mut dyn FnMut(SystemEvent)) -> io::Result<()> {
        unreachable!("注册失败的事件源不该被要求跑消息循环")
    }
}

/// 注册成功、随即意外结束的事件源（监听线程死了）。
struct DyingSource;

impl SystemEventSource for DyingSource {
    fn name(&self) -> &'static str {
        "dying"
    }

    fn start(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn run(&mut self, _alive: &AtomicBool, _emit: &mut dyn FnMut(SystemEvent)) -> io::Result<()> {
        Err(io::Error::other("probe: listener died"))
    }
}

/// 记下每一条广播（断言「输出」那半边）。
#[derive(Default)]
struct RecordingSink {
    seen: Mutex<Vec<EventEnvelope>>,
}

impl RecordingSink {
    fn events(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|envelope| envelope.event.clone())
            .collect()
    }
}

impl EventSink for RecordingSink {
    fn broadcast(&self, envelope: &EventEnvelope) -> Result<(), String> {
        self.seen.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 装置
// ─────────────────────────────────────────────────────────────────────────────

struct Fixture {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    lock_path: PathBuf,
    log_path: PathBuf,
    /// 与协调器**共享**的假时钟（推进时间始终走平台时钟接缝）。
    clock: Arc<Mutex<FakeClock>>,
    sink: Arc<RecordingSink>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("worktrace.db");
    let lock_path = dir.path().join("instance.lock");

    let mut db = Db::open(&db_path).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    // 夹具里的裸 SQL 是**装置**，不是被测路径。
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
        sink: Arc::new(RecordingSink::default()),
    }
}

/// 真启动。诊断日志**注入到临时目录**（绝不写真实数据目录）。
fn started(fx: &Fixture, interval_ms: u64) -> Box<RunningApp> {
    let mut config = StartupConfig::new(&fx.db_path, &fx.lock_path);
    config.sampling_interval_ms = interval_ms;
    config = config.with_diagnostic_log(&fx.log_path);

    match startup(
        config,
        Box::new(Arc::clone(&fx.clock)),
        Arc::clone(&fx.sink) as Arc<dyn EventSink>,
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

fn diagnostics_of(fx: &Fixture) -> Diagnostics {
    Diagnostics::to_file(&fx.log_path)
}

fn log_lines(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.lines().map(|line| line.to_string()).collect(),
        // 文件不存在 ⇒ 没有记录（这就是「零记录」的判据，不是错误）。
        Err(_) => Vec::new(),
    }
}

/// 等到日志里出现包含 `needle` 的一行。
fn wait_for_log_line(path: &Path, needle: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if log_lines(path).iter().any(|line| line.contains(needle)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "诊断日志里没有 {needle}：{:#?}",
            log_lines(path)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 等到 `probe` 满足条件。
fn wait_for<F: Fn() -> bool>(what: &str, ready: F) {
    let deadline = Instant::now() + DEADLINE;
    while !ready() {
        assert!(Instant::now() < deadline, "等到超时：{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 时刻编排与只读探针
// ─────────────────────────────────────────────────────────────────────────────

/// 推进两个时钟（正常流逝）。
fn advance(fx: &Fixture, ms: i64) {
    fx.clock.lock().unwrap().advance_both(ms);
}

/// 取一个**事件时刻**的样本（此刻的时钟读数）。
fn boundary(fx: &Fixture) -> ClockSample {
    fx.clock.lock().unwrap().sample().unwrap()
}

/// 起一次计时（夹具里已经有一个 `Ready` 任务；`start` 不需要项目）。
fn start_session(app: &SharedApp, epoch: &str) -> String {
    let request = StartRequest {
        expected_data_epoch: epoch.to_string(),
        task_id: "t1".to_string(),
        task_expected_version: 0,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: EXPECTED_INTERVAL_MS,
    };
    lock_app(app)
        .start(request)
        .expect("开始计时")
        .snapshot
        .session_id
        .expect("开始之后应当有会话")
}

fn events(fx: &Fixture) -> Vec<String> {
    fx.sink.events()
}

/// 一次**只读**的库内事实快照。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Probe {
    revision: i64,
    /// 连接级累计写入行数（抓得住 UPDATE，也抓得住回滚掉的写入尝试）。
    total_changes: i64,
    sessions: i64,
    running: i64,
    paused: i64,
    recovering: i64,
    /// 待确认区间数（`needs_review = 1`）。
    pending_intervals: i64,
    audit: i64,
    checkpoints: i64,
}

/// 探针取在 App **自己那条连接**上（新连接上 `total_changes()` 恒为 0）。
fn probe(app: &SharedApp) -> Probe {
    probe_in(&lock_app(app))
}

/// 同上，但在**已经持锁**的地方用（`std::sync::Mutex` 不可重入：再 `lock_app` 会自死锁）。
fn probe_in(state: &worktrace_lib::services::bootstrap::AppState) -> Probe {
    let conn = state.db().unwrap().connection();
    let scalar = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    Probe {
        revision: scalar("SELECT revision FROM app_meta WHERE singleton = 1"),
        total_changes: scalar("SELECT total_changes()"),
        sessions: scalar("SELECT COUNT(*) FROM work_session"),
        running: scalar("SELECT COUNT(*) FROM work_session WHERE state = 'running'"),
        paused: scalar("SELECT COUNT(*) FROM work_session WHERE state = 'paused'"),
        recovering: scalar("SELECT COUNT(*) FROM work_session WHERE state = 'recovering'"),
        pending_intervals: scalar("SELECT COUNT(*) FROM work_interval WHERE needs_review = 1"),
        audit: scalar("SELECT COUNT(*) FROM time_edit"),
        checkpoints: scalar("SELECT COUNT(*) FROM interval_checkpoint"),
    }
}

fn state_of(
    app: &SharedApp,
    session_id: &str,
) -> Option<worktrace_lib::domain::session::SessionState> {
    state_in(&lock_app(app), session_id)
}

/// 同上，但在**已经持锁**的地方用。
fn state_in(
    state: &worktrace_lib::services::bootstrap::AppState,
    session_id: &str,
) -> Option<worktrace_lib::domain::session::SessionState> {
    session_repo::get_session(state.db().unwrap().connection(), session_id)
        .unwrap()
        .map(|session| session.state)
}

/// 会话自己的「已归属时长」——平台边界是否真的落在边界样本上，看它。
fn active_ms_of(app: &SharedApp, session_id: &str) -> Option<i64> {
    let intervals = {
        let state = lock_app(app);
        session_repo::intervals_of_session(state.db().unwrap().connection(), session_id).unwrap()
    };
    let mut total = 0;
    for interval in intervals {
        if interval.voided_at.is_some() {
            continue;
        }
        // 未闭合/待确认的区间没有 duration ⇒ 整条会话的已归属时长不可得（None）。
        total += interval.duration_ms?;
    }
    Some(total)
}

// ─────────────────────────────────────────────────────────────────────────────
// ① 可信离开边界 ⇒ paused；② 边界未知 ⇒ recovering
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_locked_event_with_a_trusted_boundary_pauses_the_session() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS);
    let app = app_of(&running);
    let session_id = start_session(&app, running.data_epoch());

    advance(&fx, 10_000);
    let boundary = boundary(&fx);
    // 事件之后协调器还会自己采一次样：边界必须落在「上次可信观察」与「当前样本」之间。
    advance(&fx, 129_000);
    let before = probe(&app);

    system_events_action(
        &app,
        running.broadcaster(),
        SystemEvent {
            kind: SystemEventKind::Locked,
            boundary: Some(boundary),
        },
    );

    let after = probe(&app);
    assert_eq!(
        state_of(&app, &session_id),
        Some(worktrace_lib::domain::session::SessionState::Paused),
        "锁屏是可信离开边界 ⇒ 会话暂停（R-02）"
    );
    assert_eq!(
        active_ms_of(&app, &session_id),
        Some(10_000),
        "已归属时长必须停在事件边界上，而不是当前样本上"
    );
    assert_eq!(
        after.revision,
        before.revision + 1,
        "一次成功的平台边界恰好一次 revision"
    );
    assert_eq!(after.pending_intervals, 0, "可信边界不该留下待确认区间");
    assert_eq!(
        after.audit, before.audit,
        "可信边界不是时钟异常，不写 time_edit"
    );
    assert_eq!(
        events(&fx),
        vec![EVENT_TIMER_TICK.to_string()],
        "有会话的边界要广播一条 timer.tick"
    );
    assert_eq!(lock_app(&app).system_boundary_errors(), 0);
}

#[test]
fn a_locked_event_without_a_boundary_leaves_the_session_recovering() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS);
    let app = app_of(&running);
    let session_id = start_session(&app, running.data_epoch());

    advance(&fx, 139_000);
    let before = probe(&app);

    system_events_action(
        &app,
        running.broadcaster(),
        SystemEvent {
            kind: SystemEventKind::Locked,
            // 拿不到事件时刻的样本 ⇒ 调用方按「边界未知」处理（P2 原文，本任务不另写判断）。
            boundary: None,
        },
    );

    let after = probe(&app);
    assert_eq!(
        state_of(&app, &session_id),
        Some(worktrace_lib::domain::session::SessionState::Recovering),
        "边界未知 ⇒ recovering（P2 既有分支）"
    );
    assert_eq!(
        after.pending_intervals,
        before.pending_intervals + 1,
        "边界未知的余段必须标成待确认，不能静默计入工时"
    );
    assert_eq!(
        after.audit,
        before.audit + 1,
        "异常分割写一条 time_edit 审计"
    );
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(lock_app(&app).system_boundary_errors(), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// ③ 唤醒不自动继续
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_unlock_event_never_resumes_a_paused_session() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS);
    let app = app_of(&running);
    let session_id = start_session(&app, running.data_epoch());

    advance(&fx, 10_000);
    let locked_at = boundary(&fx);
    advance(&fx, 129_000);
    system_events_action(
        &app,
        running.broadcaster(),
        SystemEvent {
            kind: SystemEventKind::Locked,
            boundary: Some(locked_at),
        },
    );
    assert_eq!(
        state_of(&app, &session_id),
        Some(worktrace_lib::domain::session::SessionState::Paused)
    );

    let before = probe(&app);
    advance(&fx, 5_000);
    let unlocked_at = boundary(&fx);
    for kind in [SystemEventKind::Unlocked, SystemEventKind::Resumed] {
        system_events_action(
            &app,
            running.broadcaster(),
            SystemEvent {
                kind,
                boundary: Some(unlocked_at),
            },
        );
    }

    assert_eq!(
        state_of(&app, &session_id),
        Some(worktrace_lib::domain::session::SessionState::Paused),
        "唤醒/解锁**不自动继续**：本路径永不调 start/resume"
    );
    assert_eq!(
        probe(&app),
        before,
        "唤醒事件在「没有 running 会话」时必须是只读观察：一行都不写"
    );
    assert_eq!(lock_app(&app).system_boundary_errors(), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// ④ 事件与周期采样交错 ⇒ 不产生双重分割
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_event_and_the_periodic_tick_do_not_split_twice() {
    // 两种顺序都跑：谁先到，第二次调用都必须看到「已经 recovering」而幂等返回
    // （P2 的 try_handle_anomaly 对 recovering 会话不重复分割/不重复写审计）。
    for event_first in [true, false] {
        let fx = fixture();
        let running = started(&fx, IDLE_INTERVAL_MS);
        let app = app_of(&running);
        let session_id = start_session(&app, running.data_epoch());

        // 让下一拍的间隔远超 3×expected_interval_ms（P2 的挂起规则）。
        advance(&fx, 139_000);
        let before = probe(&app);

        let event = || {
            system_events_action(
                &app,
                running.broadcaster(),
                SystemEvent {
                    kind: SystemEventKind::Locked,
                    boundary: None,
                },
            );
        };
        let tick = || {
            // 顺序 B 里这一拍自己就会撞上长间隔异常（心跳先检测）：`Err(RecoveryRequired)`
            // 正是「已经分割过」的信号，不是本用例的失败。
            let _ = lock_app(&app).sample_tick();
        };
        if event_first {
            event();
            tick();
        } else {
            tick();
            event();
        }

        let after = probe(&app);
        assert_eq!(
            after.pending_intervals,
            before.pending_intervals + 1,
            "event_first={event_first}：只允许一次分割"
        );
        assert_eq!(
            after.audit,
            before.audit + 1,
            "event_first={event_first}：只允许一条 time_edit"
        );
        assert_eq!(
            after.revision,
            before.revision + 1,
            "event_first={event_first}：只允许一次 revision"
        );
        assert_eq!(
            state_of(&app, &session_id),
            Some(worktrace_lib::domain::session::SessionState::Recovering),
            "event_first={event_first}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑤ 取不到锁 ⇒ 排队等（不丢弃、不 panic）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_event_that_arrives_while_the_lock_is_held_waits_instead_of_being_dropped() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS);
    let app = app_of(&running);
    let session_id = start_session(&app, running.data_epoch());

    advance(&fx, 10_000);
    let locked_at = boundary(&fx);
    advance(&fx, 129_000);

    let (tx, rx) = mpsc::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    // 工厂闭包是 `move`：把一份克隆搬进去，用例这边仍留着 `stopped` 做断言。
    let source_stopped = Arc::clone(&stopped);
    let alive = Arc::new(AtomicBool::new(true));
    start_system_events(
        move || -> Box<dyn SystemEventSource> {
            Box::new(ScriptedSource {
                rx,
                stopped: source_stopped,
            })
        },
        Arc::clone(&alive),
        Arc::clone(&app),
        Arc::clone(running.broadcaster()),
        &diagnostics_of(&fx),
    )
    .expect("脚本事件源应当注册成功");

    // 用例自己持锁 ⇒ 事件线程只能在 lock_app 上排队。
    // 注意：持锁期间不能再 `lock_app`（`std::sync::Mutex` 不可重入），所以只读检查
    // 一律走 `*_in(&guard, …)`。
    let guard = lock_app(&app);
    tx.send(SystemEvent {
        kind: SystemEventKind::Locked,
        boundary: Some(locked_at),
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        state_in(&guard, &session_id),
        Some(worktrace_lib::domain::session::SessionState::Running),
        "锁还握着，事件不该已经生效（也不该被丢弃——它在排队）"
    );

    // 放锁：排队的事件必须**照样**生效，而不是「取不到锁就算了」。
    drop(guard);
    wait_for("排队的事件拿到锁之后生效", || {
        state_of(&app, &session_id) == Some(worktrace_lib::domain::session::SessionState::Paused)
    });
    assert_eq!(active_ms_of(&app, &session_id), Some(10_000));
    assert_eq!(
        lock_app(&app).system_boundary_errors(),
        0,
        "「锁被占着」不是错误，不涨失败计数"
    );

    // 停止信号之后事件线程自己退出（没有任何路径 join 它）。
    alive.store(false, Ordering::SeqCst);
    wait_for("事件源在停止信号之后退出", || {
        stopped.load(Ordering::SeqCst)
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑥ 拿到锁之后按**当时**的状态重新判定
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_event_is_judged_by_the_state_at_lock_time_not_at_arrival() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS);
    let app = app_of(&running);
    let session_id = start_session(&app, running.data_epoch());

    advance(&fx, 10_000);
    let locked_at = boundary(&fx);
    advance(&fx, 129_000);

    let (tx, rx) = mpsc::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    // 工厂闭包是 `move`：把一份克隆搬进去，用例这边仍留着 `stopped` 做断言。
    let source_stopped = Arc::clone(&stopped);
    let alive = Arc::new(AtomicBool::new(true));
    start_system_events(
        move || -> Box<dyn SystemEventSource> {
            Box::new(ScriptedSource {
                rx,
                stopped: source_stopped,
            })
        },
        Arc::clone(&alive),
        Arc::clone(&app),
        Arc::clone(running.broadcaster()),
        &diagnostics_of(&fx),
    )
    .expect("脚本事件源应当注册成功");

    // 持锁 → 事件到达（排队）→ **在同一个临界区里**进入维护态 → 放锁。
    // 事件拿到锁时看到的是「维护态」，所以必须整事件跳过，而不是按到达时的状态写库。
    let before = probe(&app);
    let mut guard = lock_app(&app);
    tx.send(SystemEvent {
        kind: SystemEventKind::Locked,
        boundary: Some(locked_at),
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    guard
        .begin_maintenance(MaintenancePhase::Restore, WALL)
        .expect("进入维护态");
    drop(guard);

    wait_for_log_line(&fx.log_path, "event=system_events.skipped");
    assert_eq!(
        state_of(&app, &session_id),
        Some(worktrace_lib::domain::session::SessionState::Running),
        "维护态里事件整条跳过：会话状态一动不动"
    );
    assert_eq!(probe(&app), before, "维护态里事件零写入");
    assert_eq!(lock_app(&app).system_boundary_errors(), 0);
    assert_eq!(events(&fx).len(), 0, "被跳过的事件不广播");

    alive.store(false, Ordering::SeqCst);
    wait_for("事件源在停止信号之后退出", || {
        stopped.load(Ordering::SeqCst)
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑦ 维护态：与采样**同一个判据** + 入口自身再拒一次
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_maintenance_window_swallows_events_and_the_boundary_entry_refuses_them() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS);
    let app = app_of(&running);
    let session_id = start_session(&app, running.data_epoch());

    advance(&fx, 10_000);
    let locked_at = boundary(&fx);
    advance(&fx, 129_000);

    lock_app(&app)
        .begin_maintenance(MaintenancePhase::Restore, WALL)
        .expect("进入维护态");
    let before = probe(&app);

    // 处理路径：取锁之后第一句就问 `sampling_allowed()`（与采样同一处判据）。
    assert!(!lock_app(&app).sampling_allowed());
    system_events_action(
        &app,
        running.broadcaster(),
        SystemEvent {
            kind: SystemEventKind::Locked,
            boundary: Some(locked_at),
        },
    );
    assert_eq!(probe(&app), before, "维护态里事件零写入（含读）");
    wait_for_log_line(&fx.log_path, "event=system_events.skipped");
    assert_eq!(
        lock_app(&app).system_boundary_errors(),
        0,
        "维护态不是错误：既不失败计数，也不进 sampling_errors 那条口径"
    );
    assert_eq!(events(&fx).len(), 0);

    // 防线：调用方忘了问，入口自己也要拒（换库窗口里 db 句柄正被关闭/替换）。
    let refusal = lock_app(&app)
        .system_boundary(Some(locked_at))
        .expect_err("维护态下 system_boundary 必须拒绝");
    assert!(
        is_maintenance_refusal(&refusal),
        "必须是维护态拒绝：{refusal:?}"
    );
    assert_eq!(probe(&app), before, "被拒的调用零写入");
    assert_eq!(lock_app(&app).system_boundary_errors(), 0);

    // 正控：退出维护态之后同一条路径真的恢复。
    lock_app(&app).end_maintenance().expect("退出维护态");
    assert!(lock_app(&app).sampling_allowed());
    system_events_action(
        &app,
        running.broadcaster(),
        SystemEvent {
            kind: SystemEventKind::Locked,
            boundary: Some(locked_at),
        },
    );
    assert_eq!(
        state_of(&app, &session_id),
        Some(worktrace_lib::domain::session::SessionState::Paused),
        "维护结束后同一个事件必须真的生效"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑧ 事件源起不来 ⇒ 只记诊断，周期采样照常
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_failing_event_source_is_reported_and_periodic_sampling_keeps_running() {
    let fx = fixture();
    let running = started(&fx, FAST_INTERVAL_MS);
    let app = app_of(&running);

    let result = start_system_events(
        || -> Box<dyn SystemEventSource> { Box::new(FailingSource) },
        Arc::new(AtomicBool::new(true)),
        Arc::clone(&app),
        Arc::clone(running.broadcaster()),
        &diagnostics_of(&fx),
    );

    let error = result.expect_err("注册失败必须**同步**报出来（不 panic、也不假装成功）");
    assert_eq!(error.kind(), io::ErrorKind::Unsupported, "{error:?}");
    wait_for_log_line(&fx.log_path, "event=system_events.unavailable");

    // 周期采样在另一条线程上 ⇒ 事件源起不来不影响它。
    let seen = running.sampling_ticks();
    wait_for("事件源失败之后周期采样继续涨", || {
        running.sampling_ticks() > seen + 3
    });
    assert!(!running.sampling_died_unexpectedly());
    assert_eq!(running.sampling_errors(), 0, "空闲拍不写库、也不报错");
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑨ 注入事件源端到端走组合根接线
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_injected_source_delivers_events_through_the_composition_root_wiring() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS);
    let app = app_of(&running);
    let session_id = start_session(&app, running.data_epoch());

    advance(&fx, 10_000);
    let locked_at = boundary(&fx);
    advance(&fx, 129_000);

    let (tx, rx) = mpsc::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    // 工厂闭包是 `move`：把一份克隆搬进去，用例这边仍留着 `stopped` 做断言。
    let source_stopped = Arc::clone(&stopped);
    let alive = Arc::new(AtomicBool::new(true));
    start_system_events(
        move || -> Box<dyn SystemEventSource> {
            Box::new(ScriptedSource {
                rx,
                stopped: source_stopped,
            })
        },
        Arc::clone(&alive),
        Arc::clone(&app),
        Arc::clone(running.broadcaster()),
        &diagnostics_of(&fx),
    )
    .expect("脚本事件源应当注册成功");

    tx.send(SystemEvent {
        kind: SystemEventKind::Locked,
        boundary: Some(locked_at),
    })
    .unwrap();
    wait_for("事件经事件源线程进入同一条边界", || {
        state_of(&app, &session_id) == Some(worktrace_lib::domain::session::SessionState::Paused)
    });
    assert_eq!(events(&fx), vec![EVENT_TIMER_TICK.to_string()]);

    alive.store(false, Ordering::SeqCst);
    wait_for("事件源在停止信号之后退出", || {
        stopped.load(Ordering::SeqCst)
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑩ 事件线程意外退出的可观察出口
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_source_that_dies_unexpectedly_is_reported_once() {
    let fx = fixture();
    let running = started(&fx, IDLE_INTERVAL_MS);
    let app = app_of(&running);

    start_system_events(
        || -> Box<dyn SystemEventSource> { Box::new(DyingSource) },
        Arc::new(AtomicBool::new(true)),
        Arc::clone(&app),
        Arc::clone(running.broadcaster()),
        &diagnostics_of(&fx),
    )
    .expect("注册成功；线程随后自己退出");

    wait_for_log_line(&fx.log_path, "event=system_events.died_unexpectedly");
    let lines: Vec<String> = log_lines(&fx.log_path)
        .into_iter()
        .filter(|line| line.contains("event=system_events.died_unexpectedly"))
        .collect();
    assert_eq!(lines.len(), 1, "只报一次：{lines:#?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑪ 事件线程堵在锁上不会把退出路径卡住
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_event_thread_waiting_for_the_app_lock_does_not_block_the_shutdown_path() {
    let fx = fixture();
    // 要一条**真在跑**的采样线程：`shutdown` 会 join 它，而它每一拍都要取锁。
    let running = started(&fx, FAST_INTERVAL_MS);
    let app = app_of(&running);
    let session_id = start_session(&app, running.data_epoch());

    advance(&fx, 10_000);
    let locked_at = boundary(&fx);
    advance(&fx, 129_000);

    let (tx, rx) = mpsc::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    // 工厂闭包是 `move`：把一份克隆搬进去，用例这边仍留着 `stopped` 做断言。
    let source_stopped = Arc::clone(&stopped);
    let alive = Arc::new(AtomicBool::new(true));
    start_system_events(
        move || -> Box<dyn SystemEventSource> {
            Box::new(ScriptedSource {
                rx,
                stopped: source_stopped,
            })
        },
        Arc::clone(&alive),
        Arc::clone(&app),
        Arc::clone(running.broadcaster()),
        &diagnostics_of(&fx),
    )
    .expect("脚本事件源应当注册成功");

    // 用例持锁 ⇒ 采样线程与事件线程都在排队。
    let guard = lock_app(&app);
    tx.send(SystemEvent {
        kind: SystemEventKind::Locked,
        boundary: Some(locked_at),
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));

    let (done_tx, done_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        // `shutdown`（持锁 → `begin_exit` → `sampling.stop()`（join）→ `explicit_exit`）
        // **不得**经过事件线程：没有任何路径 join 它，所以这里不可能出现
        // 「持锁 join 一个正堵在这把锁上的线程」那种自死锁（G11 同族）。
        scope.spawn(|| {
            let result = running.shutdown();
            let _ = done_tx.send(result);
        });
        std::thread::sleep(Duration::from_millis(200));
        drop(guard);
        match done_rx.recv_timeout(DEADLINE) {
            Ok(result) => assert!(result.is_ok(), "退出路径应当成功：{result:?}"),
            Err(_) => panic!("shutdown 没有在期限内返回：事件线程或采样线程把退出路径卡住了"),
        }
    });

    // 事件线程仍然按「拿到锁之后的状态」走完（此时运行态已被退出路径结束），
    // 关键是它没有把进程卡死、也没有 panic。
    alive.store(false, Ordering::SeqCst);
    wait_for("事件源在停止信号之后退出", || {
        stopped.load(Ordering::SeqCst)
    });
    let state = state_of(&app, &session_id);
    assert!(
        state.is_some(),
        "退出路径处理过这条会话（结束它），事件线程随后仍能安全收尾"
    );
}
