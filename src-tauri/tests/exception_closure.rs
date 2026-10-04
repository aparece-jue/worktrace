//! P3 Task 7：**异常闭环**——时钟校正的显式接受（S4）与协调器故障态的唯一生产出口（S12）。
//!
//! 被测入口是 `AppState::accept_detected_clock_correction`（S4）与
//! `AppState::retry_recovery`（S12）。装置抄 `tests/task_session_atomicity.rs`：
//! 真跑一次 `bootstrap::startup`，然后在**同一把锁**（`lock_app`）里下命令——P8 的 IPC
//! 就走这条边界。后台采样节拍设成 1 小时，不与用例自己的取样计数抢拍。
//!
//! 三条口径贯穿本文件（`docs/validation/pre-p3-closure.md` 的「P3 必须验证的异常闭环」）：
//!
//! 1. **提交前失败零写入**：被拒的命令逐字段比对（总纲 §5 第 8 条）；失败三段分清
//!    ——系统异常事务、用户事务、提交后收尾；
//! 2. **S4 在提交之后才清内存标记**：审计写失败 ⇒ 整体回滚且标记保留（失败保留标记）；
//! 3. **S12 先提交、后重扫**：协调器故障态由那笔恢复事务清掉，门禁字段由 S1 重算；
//!    只做一个都闭环不了（`refuse_if_faulted` 会继续拒绝 10 个入口）。
//!
//! 本文件只钉**行为**：默认时钟不自己走，用例推进多少就是多少。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::{Clock, ClockSample, FakeClock, SampleError};
use worktrace_lib::services::bootstrap::{
    lock_app, startup, AppState, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::recovery::{
    ConfirmedRange, DiscardSessionRequest, ReconcileAction, ReconcileRequest, ReconcileTargetState,
};
use worktrace_lib::services::timer::anchor::SampleVerdict;
use worktrace_lib::services::timer::coordinator::{ResumeRequest, StartRequest};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::run_repo;
use worktrace_lib::storage::time_edit_repo;

/// 启动那一刻的挂钟。`FakeClock` 不自己走，所以「现在」只随用例的推进而变。
const WALL: i64 = 1_700_000_000_000;
/// 崩掉的那一代次（夹具里的旧 run）。
const OLD_RUN: &str = "run-old";

// ─────────────────────────────────────────────────────────────────────────────
// 装置
// ─────────────────────────────────────────────────────────────────────────────

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

/// 会数数的时钟：把「这次命令取了几次样本」变成可断言的数字。
///
/// §9 的要害是「校验请求阶段不得采样」：被拒的旧 epoch/旧版本请求若也去取采样，
/// 就会在 `observe` 里消费掉那一拍，把「独立采样仍能隔离异常」变成「异常被谁吃掉都不知道」。
struct CountingClock {
    clock: Arc<Mutex<FakeClock>>,
    samples: Arc<AtomicU64>,
}

impl Clock for CountingClock {
    fn sample(&self) -> Result<ClockSample, SampleError> {
        self.samples.fetch_add(1, Ordering::SeqCst);
        self.clock.lock().unwrap().sample()
    }
}

/// 库与实例锁的路径（临时目录的所有权随后交给 [`App`]）。
struct Paths {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    lock_path: PathBuf,
}

fn fixture() -> Paths {
    let dir = tempfile::tempdir().unwrap();
    Paths {
        db_path: dir.path().join("worktrace.db"),
        lock_path: dir.path().join("instance.lock"),
        _dir: dir,
    }
}

struct App {
    _dir: tempfile::TempDir,
    running: Box<RunningApp>,
    clock: Arc<Mutex<FakeClock>>,
    samples: Arc<AtomicU64>,
}

impl App {
    fn epoch(&self) -> String {
        self.running.data_epoch().to_string()
    }

    /// 两个时钟一起走：正常流逝，不触发任何异常判定。
    fn advance(&self, ms: i64) {
        self.clock.lock().unwrap().advance_both(ms);
    }

    /// 只推进挂钟（模拟用户改系统时间）。
    fn advance_wall_only(&self, ms: i64) {
        self.clock.lock().unwrap().advance_wall(ms);
    }

    /// 只推进单调钟（正常流逝；负数是硬故障注入）。
    fn advance_mono_only(&self, ms: i64) {
        self.clock.lock().unwrap().advance_monotonic(ms);
    }

    fn wall_now(&self) -> i64 {
        self.clock.lock().unwrap().wall_ms()
    }

    fn mono_now(&self) -> i64 {
        self.clock.lock().unwrap().monotonic_ms()
    }

    fn sample_count(&self) -> u64 {
        self.samples.load(Ordering::SeqCst)
    }
}

/// 真跑一次启动（第 ④ 步的扫描会做四类判定并落地事实）。
fn started(paths: Paths) -> App {
    let clock = Arc::new(Mutex::new(FakeClock::new(WALL, 0)));
    let samples = Arc::new(AtomicU64::new(0));
    let outcome = startup(
        StartupConfig {
            db_path: paths.db_path.clone(),
            lock_path: paths.lock_path.clone(),
            // 后台采样线程不得与用例的取样计数抢拍。
            sampling_interval_ms: 3_600_000,
        },
        Box::new(CountingClock {
            clock: Arc::clone(&clock),
            samples: Arc::clone(&samples),
        }),
        Arc::new(RecordingSink::default()),
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功");
    let running = match outcome {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    };
    App {
        _dir: paths._dir,
        running,
        clock,
        samples,
    }
}

/// 建库：迁移 + 元数据 + 上一代 run + 三条任务（t1/t2 `Doing`、t3 `Ready`），没有会话。
fn seeded(paths: &Paths) -> Db {
    let mut db = Db::open(&paths.db_path).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    run_repo::start_run(&tx, OLD_RUN, WALL - 120_000).unwrap();
    for (id, title, status) in [
        ("t1", "任务一", "Doing"),
        ("t2", "任务二", "Doing"),
        ("t3", "任务三", "Ready"),
    ] {
        tx.execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES(?1,?2,?3,0,1000,1000)",
            rusqlite::params![id, title, status],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    db
}

// ── 造事实（合成形态用；正常路径一律走命令入口） ──────────────────────────────

#[allow(clippy::too_many_arguments)]
fn insert_session(
    db: &Db,
    id: &str,
    task_id: &str,
    run: &str,
    state: &str,
    started_at: i64,
    ended_at: Option<i64>,
    needs_review: i64,
) {
    db.connection()
        .execute(
            "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,ended_at,
                                      needs_review,row_version)
             VALUES(?1,?2,?3,'FOREGROUND',?4,'stopwatch',?5,?6,?7,0)",
            rusqlite::params![id, task_id, run, state, started_at, ended_at, needs_review],
        )
        .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn insert_interval(
    db: &Db,
    id: &str,
    session_id: &str,
    started_at: i64,
    ended_at: Option<i64>,
    duration_ms: Option<i64>,
    needs_review: i64,
    voided_at: Option<i64>,
) {
    db.connection()
        .execute(
            "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms,needs_review,
                                       voided_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                id,
                session_id,
                started_at,
                ended_at,
                duration_ms,
                needs_review,
                voided_at
            ],
        )
        .unwrap();
}

/// 上一次崩溃留下的 `recovering` 会话（第 3 类：原样保持，不推断、不加工时）。
/// 它属于**别的 run**，所以启动门禁一定是关着的。
fn seed_old_recovering_session(db: &Db, session_id: &str) {
    insert_session(
        db,
        session_id,
        "t1",
        OLD_RUN,
        "recovering",
        WALL - 600_000,
        None,
        1,
    );
    insert_interval(
        db,
        &format!("{session_id}-prefix"),
        session_id,
        WALL - 600_000,
        Some(WALL - 540_000),
        Some(60_000),
        0,
        None,
    );
    insert_interval(
        db,
        &format!("{session_id}-cand"),
        session_id,
        WALL - 540_000,
        Some(WALL - 540_000),
        None,
        1,
        None,
    );
}

// ── 读事实 ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionFacts {
    id: String,
    task_id: String,
    run_id: String,
    state: String,
    started_at: i64,
    ended_at: Option<i64>,
    needs_review: i64,
    row_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IntervalFacts {
    id: String,
    session_id: String,
    started_at: i64,
    ended_at: Option<i64>,
    duration_ms: Option<i64>,
    sampled_end_wall_at: Option<i64>,
    needs_review: i64,
    voided_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskFacts {
    id: String,
    status: String,
    quality: Option<String>,
    row_version: i64,
    updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChangeFacts {
    task_id: String,
    created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditFacts {
    session_id: String,
    reason: Option<String>,
}

/// 全库事实快照：被拒命令的「逐字段零变化」用 `assert_eq!(world(..), before)` 一句话比对
/// （总纲 §5 第 8 条：不只比行数）。
///
/// 刻意**不含** `total_changes()`：SQLite 把回滚掉的写入也计入那个计数，
/// 所以它证明的是「有没有尝试写」，不是「有没有留下事实」——两者分别断言。
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorldFacts {
    revision: i64,
    tasks: Vec<TaskFacts>,
    sessions: Vec<SessionFacts>,
    intervals: Vec<IntervalFacts>,
    changes: Vec<ChangeFacts>,
    edits: Vec<EditFacts>,
}

fn revision(db: &Db) -> i64 {
    require_meta(db.connection()).unwrap().revision
}

fn task_version(db: &Db, id: &str) -> i64 {
    db.connection()
        .query_row("SELECT row_version FROM task WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
}

fn session_of(db: &Db, id: &str) -> SessionFacts {
    db.connection()
        .query_row(
            "SELECT id,task_id,run_id,state,started_at,ended_at,needs_review,row_version
               FROM work_session WHERE id = ?1",
            [id],
            |r| {
                Ok(SessionFacts {
                    id: r.get(0)?,
                    task_id: r.get(1)?,
                    run_id: r.get(2)?,
                    state: r.get(3)?,
                    started_at: r.get(4)?,
                    ended_at: r.get(5)?,
                    needs_review: r.get::<_, i64>(6)?,
                    row_version: r.get(7)?,
                })
            },
        )
        .unwrap()
}

fn intervals_of(db: &Db, session_id: &str) -> Vec<IntervalFacts> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare(
            "SELECT id,session_id,started_at,ended_at,duration_ms,sampled_end_wall_at,
                    needs_review,voided_at
               FROM work_interval WHERE session_id = ?1 ORDER BY started_at, id",
        )
        .unwrap();
    let rows = stmt
        .query_map([session_id], |r| {
            Ok(IntervalFacts {
                id: r.get(0)?,
                session_id: r.get(1)?,
                started_at: r.get(2)?,
                ended_at: r.get(3)?,
                duration_ms: r.get(4)?,
                sampled_end_wall_at: r.get(5)?,
                needs_review: r.get::<_, i64>(6)?,
                voided_at: r.get(7)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    rows
}

fn world(db: &Db) -> WorldFacts {
    let conn = db.connection();

    let mut tasks_stmt = conn
        .prepare("SELECT id,status,quality,row_version,updated_at FROM task ORDER BY id")
        .unwrap();
    let tasks = tasks_stmt
        .query_map([], |r| {
            Ok(TaskFacts {
                id: r.get(0)?,
                status: r.get(1)?,
                quality: r.get(2)?,
                row_version: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    let mut sessions_stmt = conn
        .prepare(
            "SELECT id,task_id,run_id,state,started_at,ended_at,needs_review,row_version
               FROM work_session ORDER BY id",
        )
        .unwrap();
    let sessions = sessions_stmt
        .query_map([], |r| {
            Ok(SessionFacts {
                id: r.get(0)?,
                task_id: r.get(1)?,
                run_id: r.get(2)?,
                state: r.get(3)?,
                started_at: r.get(4)?,
                ended_at: r.get(5)?,
                needs_review: r.get::<_, i64>(6)?,
                row_version: r.get(7)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    let mut intervals_stmt = conn
        .prepare(
            "SELECT id,session_id,started_at,ended_at,duration_ms,sampled_end_wall_at,
                    needs_review,voided_at
               FROM work_interval ORDER BY id",
        )
        .unwrap();
    let intervals = intervals_stmt
        .query_map([], |r| {
            Ok(IntervalFacts {
                id: r.get(0)?,
                session_id: r.get(1)?,
                started_at: r.get(2)?,
                ended_at: r.get(3)?,
                duration_ms: r.get(4)?,
                sampled_end_wall_at: r.get(5)?,
                needs_review: r.get::<_, i64>(6)?,
                voided_at: r.get(7)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    let mut changes_stmt = conn
        .prepare("SELECT task_id,created_at FROM task_change ORDER BY created_at, id")
        .unwrap();
    let changes = changes_stmt
        .query_map([], |r| {
            Ok(ChangeFacts {
                task_id: r.get(0)?,
                created_at: r.get(1)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    let mut edits_stmt = conn
        .prepare("SELECT session_id,reason FROM time_edit ORDER BY created_at, id")
        .unwrap();
    let edits = edits_stmt
        .query_map([], |r| {
            Ok(EditFacts {
                session_id: r.get(0)?,
                reason: r.get(1)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    WorldFacts {
        revision: revision(db),
        tasks,
        sessions,
        intervals,
        changes,
        edits,
    }
}

// ── 命令的构造 ──────────────────────────────────────────────────────────────

fn start_request(epoch: &str, task_id: &str, task_version: i64) -> StartRequest {
    StartRequest {
        expected_data_epoch: epoch.to_string(),
        task_id: task_id.to_string(),
        task_expected_version: task_version,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: 30_000,
    }
}

fn resume_request(
    epoch: &str,
    task_id: &str,
    task_version: i64,
    session_id: &str,
    session_version: i64,
) -> ResumeRequest {
    ResumeRequest {
        expected_data_epoch: epoch.to_string(),
        task_id: task_id.to_string(),
        task_expected_version: task_version,
        session_id: session_id.to_string(),
        session_expected_version: session_version,
    }
}

fn confirm(
    session_id: &str,
    target: ReconcileTargetState,
    ranges: &[(String, i64, i64)],
) -> ReconcileRequest {
    ReconcileRequest {
        session_id: session_id.to_string(),
        action: ReconcileAction::Confirm,
        target_state: target,
        ranges: ranges
            .iter()
            .map(|(id, start, end)| ConfirmedRange {
                interval_id: id.clone(),
                started_at: *start,
                ended_at: *end,
            })
            .collect(),
    }
}

fn discard_request(session_id: &str) -> DiscardSessionRequest {
    DiscardSessionRequest {
        session_id: session_id.to_string(),
    }
}

fn env(epoch: &str, row_version: i64) -> WriteEnvelope {
    WriteEnvelope::for_update(epoch, row_version)
}

fn assert_code(error: &AppError, code: &str) {
    assert_eq!(error.code(), code, "实际：{error:?}");
}

// ── 故障注入 ────────────────────────────────────────────────────────────────

/// 让 `time_edit` 的插入失败：命令的写入与审计**同一事务**，所以失败必须整体回滚。
///
/// 为什么用触发器而不是改表名：改表名会让**校验阶段**的读查询先失败，
/// 证不到「已经写了一半再回滚」；触发器挡在审计那一步，前面的事务内写入全都发生过。
fn reject_time_edit_writes(db: &Db) {
    db.connection()
        .execute_batch(
            "CREATE TRIGGER reject_time_edit BEFORE INSERT ON time_edit
             BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;",
        )
        .unwrap();
}

fn allow_time_edit_writes(db: &Db) {
    db.connection()
        .execute_batch("DROP TRIGGER reject_time_edit;")
        .unwrap();
}

/// 让**门禁重扫**失败，但用户命令本身仍能提交。
///
/// 往**别的 run** 插一条 `state` 不在取值域里的会话行：扫描的 `read_session` 在枚举解析处
/// 报错（`enum_error`），而用户命令只按 id 读自己那条会话，不受影响——这正是
/// 「数据库提交成功但重扫门禁失败」这条时序要的注入点。
///
/// CHECK 约束由 `PRAGMA ignore_check_constraints` 临时让开：它只作用于这一条注入语句，
/// 值本身仍是普通 TEXT，所以按 `r.get::<_, String>` 读它的读法不受影响
/// （只有枚举解析会失败——那正是启动扫描的判据）。
fn break_the_scan(db: &Db) {
    db.connection()
        .execute_batch(
            "PRAGMA ignore_check_constraints = ON;
             INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,
                                      ended_at,needs_review,row_version)
             VALUES('s-broken','t1','run-old','FOREGROUND','bogus','stopwatch',0,NULL,0,0);
             PRAGMA ignore_check_constraints = OFF;",
        )
        .unwrap();
}

fn repair_the_scan(db: &Db) {
    db.connection()
        .execute("DELETE FROM work_session WHERE id = 's-broken'", [])
        .unwrap();
}

// ── 共享的异常形态 ──────────────────────────────────────────────────────────

/// 把协调器推到「**已检测但未接受**的墙钟校正」，返回会话 id。
///
/// 两步：①长间隔先让会话进 `recovering`（`Suspended` 不是墙钟校正，不清标记也不置标记）；
/// ②在 `recovering` 里观察一次墙钟异常——只有那个分支会置未接受标记（`coordinator.rs`）。
fn flag_the_unaccepted_correction(app: &App, state: &mut AppState) -> String {
    let epoch = app.epoch();
    let version = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", version)).unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();

    app.advance(129_000);
    state.snapshot().unwrap();
    assert_eq!(
        session_of(state.db(), &session_id).state,
        "recovering",
        "长间隔必须先按异常隔离"
    );

    app.advance_wall_only(31_000);
    state.snapshot().unwrap();
    session_id
}

/// 让一场异常事务在提交前失败：协调器进入故障态，库里零变化。
fn fault_the_coordinator(app: &App, state: &mut AppState) -> String {
    let epoch = app.epoch();
    let version = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", version)).unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    app.advance(1_000);
    reject_time_edit_writes(state.db());
    app.advance_wall_only(31_000);
    let error = state.snapshot().unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert!(state.coordinator().is_faulted(), "异常事务失败必须进故障态");
    session_id
}

// ─────────────────────────────────────────────────────────────────────────────
// ① 旧 epoch / 旧版本 + 时钟异常：预检先拒、零写入、独立采样仍能隔离
// ─────────────────────────────────────────────────────────────────────────────

/// `docs/validation/pre-p3-closure.md` 第 1 行：**请求预检先拒绝，用户命令零写入；
/// 独立采样仍能隔离异常，不能由旧请求触发写入**（总纲 §9）。
///
/// 「旧请求不得采样」是这里最尖的一根针：S4 在预检通过之后**自己**要取一次样本，
/// 所以预检一旦挪到采样之后，被拒的请求就会把那一拍异常消费掉——之后谁也隔离不了它。
#[test]
fn a_stale_epoch_and_a_stale_version_are_rejected_before_any_sampling() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let version = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", version)).unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    let session_version = state.coordinator().live().unwrap().row_version;

    // 一次**尚未被观察**的墙钟跳变：谁先采样，谁就得处理它。
    let before = world(state.db());
    app.advance(1_000);
    app.advance_wall_only(31_000);
    let samples_before = app.sample_count();

    // ① 旧 epoch：只读预检先拒，**不采样、不写**。
    let error = state
        .accept_detected_clock_correction("epoch-of-another-database")
        .unwrap_err();
    assert_code(&error, "DATA_EPOCH_MISMATCH");
    assert_eq!(world(state.db()), before, "被拒的请求必须零写入");
    assert_eq!(
        app.sample_count(),
        samples_before,
        "被拒的请求不得借它采样（§9：校验阶段不采样）"
    );

    // ② 旧版本：同样在写入之前被拒。
    //    （`discard_session` 的 `now` 本来就取一次墙钟，那个样本不推进检测状态，
    //    所以这里钉的是零写入与「异常没有被谁消费掉」。）
    let error = state
        .discard_session(
            env(&epoch, session_version + 41),
            discard_request(&session_id),
        )
        .unwrap_err();
    assert_code(&error, "VERSION_CONFLICT");
    assert_eq!(world(state.db()), before, "被拒的请求必须零写入");

    // ③ 异常还在：独立采样（周期采样走的就是这条入口）仍然发现并隔离它。
    state.snapshot().unwrap();
    let after = world(state.db());
    assert_eq!(
        after.edits.len(),
        before.edits.len() + 1,
        "系统异常事务恰好一条审计"
    );
    assert_eq!(
        after.revision,
        before.revision + 1,
        "系统异常事务恰好一次版本"
    );
    let session = session_of(state.db(), &session_id);
    assert_eq!(session.state, "recovering");
    assert_eq!(session.needs_review, 1);
    assert_eq!(
        intervals_of(state.db(), &session_id)[0].duration_ms,
        None,
        "候选时长不是工时"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ③ 已检测墙钟校正：未接受持续拒绝；接受先提交审计再清内存标记；不确认可疑工时
// ─────────────────────────────────────────────────────────────────────────────

/// `docs/validation/pre-p3-closure.md` 第 3 行前半：**未接受持续拒绝 `start`/`resume`**，
/// 且拒绝路径逐字段零变化。
#[test]
fn an_unaccepted_clock_correction_keeps_rejecting_start_and_resume() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let session_id = flag_the_unaccepted_correction(&app, &mut state);
    let session_version = session_of(state.db(), &session_id).row_version;
    let before = world(state.db());

    let t2_version = task_version(state.db(), "t2");
    let error = state
        .start(start_request(&epoch, "t2", t2_version))
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");

    // `resume` 也走同一条 `sample_and_detect`：未接受的校正在状态判据之前就挡住它。
    // （请求里的任务/会话必须互相匹配，否则先撞的是「任务与会话不匹配」那条领域规则。）
    let t1_version = task_version(state.db(), "t1");
    let error = state
        .resume(resume_request(
            &epoch,
            "t1",
            t1_version,
            &session_id,
            session_version,
        ))
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");

    assert_eq!(
        world(state.db()),
        before,
        "两条被拒命令都不得留下任何字段变化"
    );
    assert!(
        !state.recovery().requires_recovery(),
        "本 run 的 recovering 不是门禁事实——挡计时的是协调器那一把闸"
    );
}

/// `docs/validation/pre-p3-closure.md` 第 3 行中段：**接受校正先提交审计再清内存标记**，
/// 且**不自动确认待确认工时**；同一命令重复提交是幂等零变化。
#[test]
fn accepting_the_correction_commits_the_audit_then_clears_the_flag() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let session_id = flag_the_unaccepted_correction(&app, &mut state);
    let intervals_before = intervals_of(state.db(), &session_id);
    let gate_before = state.recovery().clone();
    let revision_before = revision(state.db());
    let edits_before = time_edit_repo::edits_of_session(state.db().connection(), &session_id)
        .unwrap()
        .len();
    assert_eq!(
        intervals_before.len(),
        1,
        "本文件的口径：没有可信前缀，整段待确认"
    );
    assert_eq!(intervals_before[0].needs_review, 1);

    let accepted = state.accept_detected_clock_correction(&epoch).unwrap();
    assert!(accepted.accepted);
    assert_eq!(accepted.data_epoch, epoch);
    assert_eq!(accepted.revision, revision_before + 1, "恰好一次版本");

    // 审计：一条不多、一条不少，形状按 §0.3（before = 三参照点，after = 本次样本）。
    let edits = time_edit_repo::edits_of_session(state.db().connection(), &session_id).unwrap();
    assert_eq!(edits.len(), edits_before + 1);
    let edit = edits.last().unwrap();
    assert_eq!(edit.reason.as_deref(), Some("clock_correction:accepted"));
    assert_eq!(edit.created_at, app.wall_now());
    let before_json: serde_json::Value = serde_json::from_str(&edit.before_json).unwrap();
    for key in ["attribution", "short_term", "lifetime"] {
        assert!(
            before_json[key].get("wall_at").is_some()
                && before_json[key].get("monotonic_at").is_some(),
            "before_json 必须带三个参照点（缺 {key}）"
        );
    }
    let after_json: serde_json::Value = serde_json::from_str(&edit.after_json).unwrap();
    assert_eq!(after_json["sampled_wall_at"], app.wall_now());
    assert_eq!(after_json["sampled_monotonic_ms"], app.mono_now());
    assert_eq!(after_json["clock_correction_accepted"], true);
    assert_eq!(after_json["intervals_changed"], false);
    // 审计必须写清「被接受的是哪一种异常」：这一拍相对检测器的 `last`（= 置标记那一拍）
    // 只多出一段 `lifetime_ref` 累计偏差（长间隔那一步重建过参照），所以判决是 `Drifted`。
    // 值取自**这一次观察**——多观察一次就是另一个数（见下一个用例的 `Jumped` 探针）。
    assert_eq!(
        after_json["verdict"], "Drifted { cumulative_gap_ms: 31000 }",
        "审计的 verdict 必须是这一拍观察到的判决"
    );
    assert_eq!(
        state.coordinator().last_verdict(),
        SampleVerdict::Drifted {
            cumulative_gap_ms: 31_000
        },
        "同一次观察的产物必须与审计一致（retry_recovery 也读它）"
    );

    // 不自动确认任何可疑工时：区间逐字段与接受之前**完全一样**。
    assert_eq!(intervals_of(state.db(), &session_id), intervals_before);
    assert_eq!(state.recovery(), &gate_before, "S4 不改恢复门禁快照");

    // 同一命令再来一次：没有待接受的校正 ⇒ 幂等零变化（无审计、无版本）。
    let again = state.accept_detected_clock_correction(&epoch).unwrap();
    assert!(!again.accepted);
    assert_eq!(again.revision, accepted.revision);
    assert_eq!(
        time_edit_repo::edits_of_session(state.db().connection(), &session_id)
            .unwrap()
            .len(),
        edits_before + 1,
        "幂等重复不得写第二条审计"
    );

    // 标记确实清了：计时恢复。
    let t2_version = task_version(state.db(), "t2");
    state
        .start(start_request(&epoch, "t2", t2_version))
        .expect("接受校正之后 start 必须成功");
}

/// S4 的「`observe` **恰好一次**」探针（§0.3 S4 顺序的第 ② 步）。
///
/// 同一个采样看两次，第二次的增量恒为 0（`anchor.rs::observe` 会推进 `last`），判决就从
/// **依赖 `last`** 的 `Jumped` 掉成 `Drifted`：审计会写错「被接受的是哪种异常」，而
/// `last_verdict`（`Coordinator::retry_recovery` 会读它）也跟着错。
///
/// 所以这里让未接受标记置真之后**再跳一次**，断言审计与 `last_verdict` 都是这一拍的
/// `Jumped`——多观察一次，这两条断言**必红**。
#[test]
fn accepting_the_correction_observes_the_sample_exactly_once() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let session_id = flag_the_unaccepted_correction(&app, &mut state);
    // 标记已置真（会话 `recovering`），此时时钟又跳一次：这一拍相对检测器的 `last`
    // 是一段 5 秒的挂钟单拍跳变（`Jumped` 的判据依赖 `last`，看两次就不成立了）。
    app.advance_wall_only(5_000);

    let accepted = state.accept_detected_clock_correction(&epoch).unwrap();
    assert!(accepted.accepted);

    let edits = time_edit_repo::edits_of_session(state.db().connection(), &session_id).unwrap();
    let edit = edits
        .iter()
        .find(|edit| edit.reason.as_deref() == Some("clock_correction:accepted"))
        .expect("接受校正必须留下自己那条审计");
    let after_json: serde_json::Value = serde_json::from_str(&edit.after_json).unwrap();
    assert_eq!(
        after_json["verdict"], "Jumped { delta_gap_ms: 5000 }",
        "审计的 verdict 必须来自**这一次**观察"
    );
    assert_eq!(after_json["sampled_wall_at"], app.wall_now());
    assert_eq!(
        state.coordinator().last_verdict(),
        SampleVerdict::Jumped {
            delta_gap_ms: 5_000
        },
        "多观察一次会把这一拍的判决降级成 `Drifted`（`last` 已被它推进）"
    );
}

/// Ruling 34：这条路上观察到的**非 `Trusted`** 判决不得被静默丢掉——**长间隔**。
///
/// 若本命令是睡眠/休眠之后的**第一个采样入口**，`observe` 已经把那一拍消费掉：丢掉判决
/// 就等于把停机时间留在开放区间里当成工时，既不分割也不标 `needs_review`
/// （违反 08 §1 / 02 §4 的「异常必须在最后可信检查点分割」与总纲 §9）。
/// 它必须走既有异常处理：系统恢复事务先提交，再拒绝原命令。
///
/// 这条同时是 `pre-p3-closure` 第 2 行（长间隔 / 无基线 / 未知终点 / 可信前缀）在本文件里的证据。
#[test]
fn a_long_gap_seen_by_the_correction_command_is_not_counted_as_work() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let version = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", version)).unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    let open_before = intervals_of(state.db(), &session_id);
    assert_eq!(open_before.len(), 1);
    assert_eq!(open_before[0].ended_at, None, "起点是一条开放区间");

    let before = world(state.db());
    let samples_before = app.sample_count();
    // 睡眠/休眠：两个时钟一起走了 129 秒（长间隔，两钟同步 ⇒ 不是墙钟校正）。
    app.advance(129_000);

    let error = state.accept_detected_clock_correction(&epoch).unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(
        app.sample_count() - samples_before,
        1,
        "这条路径只取一次样本"
    );

    // 既有异常处理的产物：区间收成待确认段、会话 `recovering`、恰好一次 revision + 一条审计。
    let after = world(state.db());
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.edits.len(), before.edits.len() + 1);
    assert_eq!(
        after.edits.last().unwrap().reason.as_deref(),
        Some("untrusted observation gap")
    );
    let session = session_of(state.db(), &session_id);
    assert_eq!(session.state, "recovering");
    assert_eq!(session.needs_review, 1);
    let intervals = intervals_of(state.db(), &session_id);
    assert_eq!(intervals.len(), 1, "没有可信前缀时不另造余段");
    assert_eq!(intervals[0].needs_review, 1);
    assert_eq!(intervals[0].duration_ms, None, "无证据不猜时长");
    assert_eq!(
        intervals[0].ended_at,
        Some(app.wall_now()),
        "开放区间收在候选终点上（候选不是事实）"
    );

    // 停机时间**不进可信暂计**：它只是待确认。
    let snapshot = state.snapshot().unwrap();
    assert_eq!(snapshot.state, Some(SessionState::Recovering));
    assert_eq!(snapshot.active_ms, 0, "停机时间不得被静默算成工时");
    assert_eq!(
        snapshot.pending_ms,
        Some(129_000),
        "它进的是待确认，不是工时"
    );

    // 这条路上也没有发生任何「接受」。
    assert!(
        after
            .edits
            .iter()
            .all(|edit| edit.reason.as_deref() != Some("clock_correction:accepted")),
        "没有待接受的校正：不得写接受审计"
    );
}

/// Ruling 34 的另一半：**`live` 为 `None`**（本 run 还没有过任何会话）时的同一拍长间隔。
///
/// 既有异常处理对「没有会话」什么都不写——没有开放工时需要分割，重定基线是 `start`
/// 路径的事（`Coordinator::start` 的 `live.is_none()` 分支）。所以这里如实返回
/// `RECOVERY_REQUIRED` 且**零写入**（与 `snapshot`/`start` 同一条最小口径，不新造机制），
/// 下一拍就恢复可信。
#[test]
fn a_long_gap_without_a_live_session_is_reported_without_writing() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    assert!(state.coordinator().live().is_none());
    let before = world(state.db());
    app.advance(129_000);

    let error = state.accept_detected_clock_correction(&epoch).unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert!(!state.coordinator().is_faulted());
    assert_eq!(
        world(state.db()),
        before,
        "没有会话：既有异常处理不写任何事实（与 snapshot/start 同一口径）"
    );

    // 下一拍已经可信：不残留故障，也不谎报「有待接受的校正」。
    let again = state.accept_detected_clock_correction(&epoch).unwrap();
    assert!(!again.accepted);
    assert_eq!(again.revision, before.revision);
    assert!(state.guard_business_timing().is_ok());
}

/// 失败保留标记（§0.3 S4 的「不提交、不清标记」）：审计写失败 ⇒ 整体回滚，
/// 内存里的未接受标记必须原地不动——不然用户会「什么都没写成，却把校正接受掉了」。
#[test]
fn a_failed_audit_keeps_the_unaccepted_clock_correction() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let _session_id = flag_the_unaccepted_correction(&app, &mut state);
    let before = world(state.db());
    let revision_before = revision(state.db());

    reject_time_edit_writes(state.db());
    let error = state.accept_detected_clock_correction(&epoch).unwrap_err();
    assert_code(&error, "STORAGE_ERROR");
    assert_eq!(
        world(state.db()),
        before,
        "审计失败 ⇒ 整体回滚，零写入零版本"
    );

    // 标记没被清：`start` 仍然被同一把闸挡住。
    let t2_version = task_version(state.db(), "t2");
    let error = state
        .start(start_request(&epoch, "t2", t2_version))
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(world(state.db()), before);
    assert_eq!(revision(state.db()), revision_before);

    // 排障之后**不需要重做任何东西**：直接再接受一次即可（标记还在）。
    allow_time_edit_writes(state.db());
    let accepted = state.accept_detected_clock_correction(&epoch).unwrap();
    assert!(accepted.accepted);
    assert_eq!(accepted.revision, revision_before + 1);
    let t2_version = task_version(state.db(), "t2");
    state
        .start(start_request(&epoch, "t2", t2_version))
        .expect("重试成功之后计时必须恢复");
}

/// `docs/validation/pre-p3-closure.md` 第 3 行末句：**`reconcile` 与接受校正独立测试**。
///
/// 两把闸互不代替：`reconcile` 管「可疑工时怎么办」，S4 管「新墙钟参照接不接受」。
/// 前者做完之后，未接受标记必须**原样留着**——S4 仍然要写审计、加版本才算接受；
/// 反过来，S4 不确认任何可疑工时（上一个用例的 `intervals_changed: false` 那一半）。
#[test]
fn reconciling_the_pending_work_does_not_accept_the_clock_correction() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let session_id = flag_the_unaccepted_correction(&app, &mut state);
    let pending = intervals_of(state.db(), &session_id);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].needs_review, 1);
    let session_version = session_of(state.db(), &session_id).row_version;
    let revision_before = revision(state.db());

    // 用户确认了这段可疑工时（reconcile 的活）⇒ 会话收尾成 finished。
    let ranges = vec![(
        pending[0].id.clone(),
        pending[0].started_at,
        pending[0].started_at + 60_000,
    )];
    state
        .reconcile(
            env(&epoch, session_version),
            confirm(&session_id, ReconcileTargetState::Finished, &ranges),
        )
        .unwrap();
    let confirmed = intervals_of(state.db(), &session_id);
    assert_eq!(confirmed[0].needs_review, 0, "reconcile 确认了它");
    assert_eq!(confirmed[0].duration_ms, Some(60_000));
    assert_eq!(session_of(state.db(), &session_id).state, "finished");
    assert_eq!(
        revision(state.db()),
        revision_before + 1,
        "这一步只加 reconcile 自己那一次版本"
    );

    // 探针：reconcile 之后 S4 仍然有活要干——未接受标记原样留着。
    // （为什么不拿 `start` 当这一步的探针：会话已经不是 `recovering`，P2 的
    //   「无运行工时的墙钟异常」分支会在下一次采样里原子地记审计并接受校正——那是 P2
    //   既有的另一条路径，与本条命令的独立性无关；`recovering` 态的持续拒绝由
    //   上一个用例逐字段钉住。）
    let accepted = state.accept_detected_clock_correction(&epoch).unwrap();
    assert!(accepted.accepted, "reconcile 不得替用户接受墙钟校正");
    assert_eq!(accepted.revision, revision_before + 2, "接受才加第二次版本");
    let edits = time_edit_repo::edits_of_session(state.db().connection(), &session_id).unwrap();
    // 两条审计可能落在**同一毫秒**（同一拍的 `now`），所以按 reason 找，不按顺序取 `last()`。
    assert!(
        edits
            .iter()
            .any(|edit| edit.reason.as_deref() == Some("clock_correction:accepted")),
        "接受校正必须留下自己那条审计"
    );

    let t2_version = task_version(state.db(), "t2");
    state
        .start(start_request(&epoch, "t2", t2_version))
        .expect("接受之后才放行");
}

/// §0.3 S4 的硬故障判据：单调读数倒退**不是**墙钟校正——它不能被一次「接受校正」
/// 顺手掩盖掉（`is_wall_clock_anomaly` 不含 `MonotonicBackwards`）。
#[test]
fn a_backwards_monotonic_clock_is_not_accepted_as_a_correction() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let session_id = flag_the_unaccepted_correction(&app, &mut state);
    let before = world(state.db());

    // 单调钟倒退：这一拍读数已失去本 run 的意义（正常平台永不出现）。
    app.advance_mono_only(-5_000);
    let error = state.accept_detected_clock_correction(&epoch).unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert!(
        state.coordinator().is_faulted(),
        "硬故障必须锁死协调器，而不是被当校正接受掉"
    );
    assert_eq!(world(state.db()), before, "被拒的接受零写入");

    // 未接受标记也没被清：会话与它的待确认事实原样留着（新 run 才安全重建）。
    let t2_version = task_version(state.db(), "t2");
    let error = state
        .start(start_request(&epoch, "t2", t2_version))
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(world(state.db()), before);
    assert_eq!(session_of(state.db(), &session_id).state, "recovering");
}

/// 上一条用例走的是 `flag == true`：会话已经挂着**待接受的墙钟校正**，于是它连
/// Ruling 34 那个分支都进不去。这一条走 `flag == false`——本来会返回 `Unchanged` 的
/// 那一路，也是 Ruling 34 真正要补的那条路。
///
/// 硬故障在这条路上必须和别的采样入口一样**零写入**：单调读数倒退不是「异常要隔离」，
/// 而是这一拍读数对本 run 已无意义（`snapshot`/`tick`/`heartbeat`/`start` 在
/// `MonotonicBackwards` 下都不分割、不写审计、不加版本）。所以异常处理后置块必须排在
/// `MonotonicBackwards` 守卫**之后**：先认硬故障，再谈「有没有别的判决要被处理」。
#[test]
fn a_backwards_monotonic_clock_writes_nothing_without_an_unaccepted_correction() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let t1_version = task_version(state.db(), "t1");
    state
        .start(start_request(&epoch, "t1", t1_version))
        .unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    app.advance(1_000);

    // 前置由探针自己证明：没有待接受的校正 ⇒ 本条命令本来返回 `Unchanged`（零变化）。
    let probe = state.accept_detected_clock_correction(&epoch).unwrap();
    assert!(
        !probe.accepted,
        "前置：这条路上没有待接受的校正（`flag == false`）"
    );
    let before = world(state.db());

    // 单调钟倒退：硬故障，必须锁死协调器（正常平台永不出现）。
    app.advance_mono_only(-5_000);
    let error = state.accept_detected_clock_correction(&epoch).unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert!(state.coordinator().is_faulted(), "硬故障必须锁死协调器");
    assert_eq!(
        world(state.db()),
        before,
        "硬故障零写入：不得先提交一笔恢复事务（分割 / 审计 / 版本 +1）"
    );
    assert_eq!(
        session_of(state.db(), &session_id).state,
        "running",
        "硬故障不得把会话推成 recovering 或标待确认"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ④ 存储忙 / 磁盘不足使异常事务失败：整体回滚、faulted；S12 成功后重算门禁
// ─────────────────────────────────────────────────────────────────────────────

/// `docs/validation/pre-p3-closure.md` 第 4 行前半：**事务完整回滚，协调器停止可信暂计并
/// `faulted`**；故障态下所有入口一律拒绝。
#[test]
fn a_failed_anomaly_transaction_rolls_everything_back_and_faults_the_coordinator() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let version = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", version)).unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    app.advance(1_000);

    let before = world(state.db());
    reject_time_edit_writes(state.db());
    app.advance_wall_only(31_000);
    let error = state.snapshot().unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");

    assert!(state.coordinator().is_faulted(), "提交失败必须进故障态");
    assert_eq!(
        world(state.db()),
        before,
        "异常事务整体回滚：不留半个分割、不留半条审计、不加版本"
    );
    assert_eq!(session_of(state.db(), &session_id).state, "running");

    // 恢复入口仍然可用（否则门禁自己成了死锁），但故障态把业务入口全挡住。
    assert!(state.guard_business_timing().is_ok());
    let t2_version = task_version(state.db(), "t2");
    let error = state
        .start(start_request(&epoch, "t2", t2_version))
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_code(&state.snapshot().unwrap_err(), "RECOVERY_REQUIRED");
    assert_eq!(world(state.db()), before, "故障态下的拒绝同样零写入");
}

/// `docs/validation/pre-p3-closure.md` 第 4 行后半：**S12 用户显式重试成功后重算门禁，
/// 失败不清故障**；成功之后计时真的可用（「故障态下计时仍可用」的唯一证据）。
///
/// **门禁重算必须可证伪**：成功路径唯一能证明「`rescan_recovery()` 真的跑了」的事实，
/// 是一条**本次重试之前才注入**的、别的 run 的恢复材料——它不在启动快照里，只有重扫
/// 才会看见它。所以这里注入之后先断言缓存快照**还是**开着的，重试成功后再断言它**关了**。
#[test]
fn retry_recovery_clears_the_fault_only_after_a_successful_commit() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let session_id = fault_the_coordinator(&app, &mut state);
    let before = world(state.db());

    // 故障原因还在 ⇒ 恢复事务仍然提交不了 ⇒ 故障**不清**。
    let error = state.retry_recovery(&epoch).unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert!(
        state.coordinator().is_faulted(),
        "重试失败不得清故障（否则坏事实会在下一拍被当成好事实）"
    );
    assert_eq!(world(state.db()), before, "重试失败同样零写入");

    // 注入一条**别的 run** 的 `recovering` 会话（带一条待确认区间）：启动时库是干净的，
    // 所以它不在门禁快照里——只有真的重扫过，下面那条「门禁关了」才可能成立。
    insert_session(
        state.db(),
        "s-other",
        "t3",
        OLD_RUN,
        "recovering",
        WALL - 300_000,
        None,
        1,
    );
    insert_interval(
        state.db(),
        "s-other-pending",
        "s-other",
        WALL - 300_000,
        Some(WALL - 300_000),
        None,
        1,
        None,
    );
    assert!(
        !state.recovery().requires_recovery(),
        "注入不改缓存快照：重扫才会看到它"
    );

    // 排障之后重试：那笔恢复事务提交一次，故障态清除，门禁按**提交后**的事实重算。
    allow_time_edit_writes(state.db());
    let samples_after_failure = app.sample_count();
    let snapshot = state.retry_recovery(&epoch).unwrap();
    assert_eq!(
        app.sample_count() - samples_after_failure,
        1,
        "S12 全路径只取一次样本：预检与重扫都不得采样"
    );
    assert!(!state.coordinator().is_faulted());
    assert_eq!(snapshot.session_id.as_deref(), Some(session_id.as_str()));
    assert_eq!(snapshot.state, Some(SessionState::Recovering));

    let after = world(state.db());
    assert_eq!(
        after.edits.len(),
        before.edits.len() + 1,
        "恢复事务恰好一条审计"
    );
    assert_eq!(after.revision, before.revision + 1, "恢复事务恰好一次版本");
    assert_eq!(session_of(state.db(), &session_id).state, "recovering");
    assert_eq!(
        intervals_of(state.db(), &session_id)[0].duration_ms,
        None,
        "候选时长不是工时"
    );

    // **门禁字段跟着事实走了**：刚注入的那条别的 run 的恢复材料必须出现在快照里。
    assert!(
        state.recovery().requires_recovery(),
        "重试成功后必须按提交后的事实重算门禁（否则这里会是启动时的空快照）"
    );
    assert!(
        state
            .recovery()
            .unfinished_sessions
            .iter()
            .any(|id| id == "s-other"),
        "快照必须包含刚注入的那条会话：{:?}",
        state.recovery()
    );
    assert_code(
        &state.guard_business_timing().unwrap_err(),
        "RECOVERY_REQUIRED",
    );
    let t2_version = task_version(state.db(), "t2");
    let error = state
        .start(start_request(&epoch, "t2", t2_version))
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");

    // 把那批事实处理掉（这里是直接清掉）⇒ 显式重扫之后门禁放开、计时真的可用：
    // 「故障态下计时仍可用」这条闭环没有被注入的恢复材料挡住。
    state
        .db()
        .connection()
        .execute_batch(
            "DELETE FROM work_interval WHERE session_id = 's-other';
             DELETE FROM work_session WHERE id = 's-other';",
        )
        .unwrap();
    assert!(!state.rescan_recovery().unwrap().requires_recovery());
    assert!(state.guard_business_timing().is_ok());
    state
        .start(start_request(&epoch, "t2", t2_version))
        .expect("重建成功且恢复事实清掉之后 start 必须可用");
}

/// `docs/validation/pre-p3-closure.md` 第 4 行末句：**单调倒退仍需新 run，禁止用 S12 掩盖**。
#[test]
fn a_hard_monotonic_fault_cannot_be_released_by_retry_recovery() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let version = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", version)).unwrap();
    app.advance(1_000);
    app.advance_mono_only(-5_000);
    // 硬故障那一拍仍然会先落一笔系统事务（把开放事实收成待确认），随后才锁死。
    state.snapshot().unwrap();
    assert!(state.coordinator().is_faulted());

    let before = world(state.db());
    let error = state.retry_recovery(&epoch).unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert!(
        state.coordinator().is_faulted(),
        "单调读数已失去本 run 的意义：只能新 run 重建"
    );
    assert_eq!(world(state.db()), before, "S12 不得用一次重试掩盖硬故障");
    let t2_version = task_version(state.db(), "t2");
    let error = state
        .start(start_request(&epoch, "t2", t2_version))
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert!(
        state.guard_business_timing().is_ok(),
        "门禁与协调器故障是两把闸：这里挡人的是故障态，不是恢复事实"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑤ 数据库提交成功但重扫门禁失败：不宣称回滚、不重复执行、用提交后权威版本
// ─────────────────────────────────────────────────────────────────────────────

/// `docs/validation/pre-p3-closure.md` 第 5 行（S12 侧）：协调器那笔恢复事务**已经提交**，
/// 门禁重扫却失败——已提交事实保留、故障态确实清了，但命令返回 `RECOVERY_REQUIRED`，
/// 出口是**显式重扫成功**，且重扫不得重做那笔事务。
#[test]
fn retry_recovery_keeps_the_committed_recovery_when_the_rescan_fails() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let session_id = fault_the_coordinator(&app, &mut state);
    allow_time_edit_writes(state.db());
    let revision_before = revision(state.db());
    break_the_scan(state.db());

    let error = state.retry_recovery(&epoch).unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert!(
        !state.coordinator().is_faulted(),
        "协调器那笔恢复事务已经提交：故障态确实清了（不能谎称回滚）"
    );

    // 已提交事实保留：恰好一条审计、恰好一次版本、会话事实按那笔事务落地。
    let committed = world(state.db());
    assert_eq!(committed.edits.len(), 1);
    assert_eq!(committed.revision, revision_before + 1);
    assert_eq!(session_of(state.db(), &session_id).state, "recovering");

    // 重扫失败 ⇒ S1 的失败标记挡住计时；旧快照原样保留。
    assert_code(
        &state.guard_business_timing().unwrap_err(),
        "RECOVERY_REQUIRED",
    );
    let t2_version = task_version(state.db(), "t2");
    let error = state
        .start(start_request(&epoch, "t2", t2_version))
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");

    // 恢复出口：显式重扫成功。重扫不重做任何已提交的用户/系统事务。
    repair_the_scan(state.db());
    let after_repair = world(state.db());
    let scan = state.rescan_recovery().unwrap();
    assert!(!scan.requires_recovery());
    assert_eq!(
        world(state.db()),
        after_repair,
        "重扫是只读的：既不重复审计，也不重复加版本"
    );
    assert_eq!(
        after_repair.edits.len(),
        committed.edits.len(),
        "重扫没有重做那笔恢复事务的审计"
    );
    assert_eq!(after_repair.revision, committed.revision);
    let t2_version = task_version(state.db(), "t2");
    state
        .start(start_request(&epoch, "t2", t2_version))
        .expect("重扫成功之后门禁必须放开");
}

/// `docs/validation/pre-p3-closure.md` 第 5 行（用户命令侧，Task 2 评审登记的携带项）：
/// `bootstrap.rs` 的 `self.rescan_recovery()?` 失败会提前返回，于是这次命令的提交后收尾
/// （条件化镜像刷新）不再被求值。
///
/// 钉住的时序：**committed 事实保留、不重复审计/版本、`start`/`resume` 拒绝、
/// 再次扫描成功才解除**；并且 P8 不得自动重发那条写命令（重发会撞版本，不会重复执行）。
#[test]
fn a_committed_command_survives_a_failed_rescan_without_repeating_its_writes() {
    let paths = fixture();
    let db = seeded(&paths);
    seed_old_recovering_session(&db, "s-old");
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    assert!(
        state.recovery().requires_recovery(),
        "别的 run 的 recovering 会话必须挡住计时"
    );
    let session_version = session_of(state.db(), "s-old").row_version;
    let revision_before = revision(state.db());
    let edits_before = world(state.db()).edits.len();

    // 注入「三条扫描查询失败」——用户命令本身仍然能提交（它只读自己那条会话）。
    break_the_scan(state.db());
    let error = state
        .discard_session(env(&epoch, session_version), discard_request("s-old"))
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");

    // 不宣称回滚：那次写是真的。
    let committed = world(state.db());
    assert_eq!(session_of(state.db(), "s-old").state, "discarded");
    let intervals = intervals_of(state.db(), "s-old");
    assert!(
        intervals
            .iter()
            .all(|interval| interval.voided_at.is_some()),
        "整次作废：全部区间软作废"
    );
    assert!(intervals.iter().all(|interval| interval.needs_review == 0));
    assert_eq!(committed.edits.len(), edits_before + 1, "恰好一条审计");
    assert_eq!(committed.revision, revision_before + 1, "恰好一次版本");

    // 不重复执行用户意图：同一条请求再来一次会撞会话版本，零变化。
    let error = state
        .discard_session(env(&epoch, session_version), discard_request("s-old"))
        .unwrap_err();
    assert_code(&error, "VERSION_CONFLICT");
    assert_eq!(world(state.db()), committed, "重发不得重复执行");

    // 扫描失败 ⇒ 旧快照原样保留、计时继续被挡。
    assert!(state.recovery().requires_recovery());
    assert_code(
        &state.guard_business_timing().unwrap_err(),
        "RECOVERY_REQUIRED",
    );
    let t1_version = task_version(state.db(), "t1");
    let error = state
        .start(start_request(&epoch, "t1", t1_version))
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");

    // 恢复出口：显式重扫成功 ⇒ 按**提交后**的事实算，门禁放开，计时可用。
    repair_the_scan(state.db());
    let after_repair = world(state.db());
    let scan = state.rescan_recovery().unwrap();
    assert!(!scan.requires_recovery(), "作废过的会话不再挡计时");
    assert_eq!(world(state.db()), after_repair, "重扫不重复任何写入");
    assert_eq!(
        after_repair.edits.len(),
        committed.edits.len(),
        "重扫不得重复那条用户事务的审计"
    );
    assert_eq!(after_repair.revision, committed.revision);
    let t1_version = task_version(state.db(), "t1");
    state
        .start(start_request(&epoch, "t1", t1_version))
        .expect("重扫成功之后门禁必须放开");
}

/// 上一条用例留下的缺口（Task 7 评审登记的携带项）：它作废的是**别的 run** 的会话，
/// 而那时 `live == None`——于是「重扫失败之后仍然刷新镜像」这条路径既没被钉住、
/// 也没被证伪。这一条把被作废的会话换成**协调器此刻镜像的那条**（`live`）。
///
/// 不刷新的话，采样线程会继续按旧状态出快照：这条已经 `discarded` 的会话仍被报成
/// `running`、`active_ms` 继续涨，而且 ~30s 一次的心跳会在
/// `checkpoint_repo::write` 的守卫上失败（017 的「不得留一个继续按旧状态出快照的
/// `live`」，即计划的新增-2）。
///
/// 与「重扫失败」共存的还有三条口径：命令仍然返回 `RECOVERY_REQUIRED`（提交后约定：
/// 缺的是内存与事实重新对上，不是「再试一次」）、已提交事实逐字段保留、失败没有
/// 顺带多写一条审计或多加一次版本。
#[test]
fn a_failed_rescan_still_refreshes_the_mirror_of_the_discarded_live_session() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    // 镜像 = 本 run 正在计时的会话：这次作废的正是它。
    let t1_version = task_version(state.db(), "t1");
    state
        .start(start_request(&epoch, "t1", t1_version))
        .unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    assert_eq!(
        state.coordinator().live().unwrap().state,
        SessionState::Running,
        "前置：镜像是一条正在计时的会话"
    );
    let session_version = session_of(state.db(), &session_id).row_version;
    let revision_before = revision(state.db());
    let edits_before = world(state.db()).edits.len();

    // 注入「三条扫描查询失败」——用户命令本身仍然能提交（它只读自己那条会话）。
    break_the_scan(state.db());
    let error = state
        .discard_session(env(&epoch, session_version), discard_request(&session_id))
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");

    // 已提交事实保留：恰好一条审计、恰好一次版本，会话与全部区间按命令落地。
    let committed = world(state.db());
    assert_eq!(session_of(state.db(), &session_id).state, "discarded");
    let intervals = intervals_of(state.db(), &session_id);
    assert!(
        intervals
            .iter()
            .all(|interval| interval.voided_at.is_some()),
        "整次作废：全部区间软作废"
    );
    assert_eq!(committed.edits.len(), edits_before + 1, "恰好一条审计");
    assert_eq!(committed.revision, revision_before + 1, "恰好一次版本");

    // **镜像必须跟着已提交事实走**：重扫失败只说明「门禁快照没重算」，
    // 不说明「内存可以停在旧状态」。
    let live = state
        .coordinator()
        .live()
        .expect("live 不清成 None（与 finish 之后同一口径）");
    assert_eq!(live.id, session_id);
    assert_eq!(
        live.state,
        SessionState::Discarded,
        "镜像停在旧状态 = 继续把已作废的会话报成 running"
    );
    assert_eq!(
        live.row_version,
        session_of(state.db(), &session_id).row_version,
        "镜像的会话版本必须追上已提交的那一行"
    );
    assert!(live.open_interval.is_none(), "作废之后没有开放区间");
    assert_eq!(live.closed_trusted_ms, 0, "整次作废不留下可信工时");

    // 扫描失败 ⇒ 「失败的扫描没有结论」，失败标记自己挡住计时（旧快照不被沿用），
    // 而这次失败没有多写任何东西。
    assert_code(
        &state.guard_business_timing().unwrap_err(),
        "RECOVERY_REQUIRED",
    );

    // 恢复出口：显式重扫成功。重扫只重算门禁，不重做任何已提交的写；刷新过的镜像
    // 也不因为重扫而变回去。
    repair_the_scan(state.db());
    let scan = state.rescan_recovery().unwrap();
    assert!(!scan.requires_recovery(), "作废过的会话不再挡计时");
    assert!(
        state.guard_business_timing().is_ok(),
        "重扫成功必须清掉失败标记"
    );
    assert_eq!(revision(state.db()), committed.revision, "重扫不加版本");
    assert_eq!(
        world(state.db()).edits.len(),
        committed.edits.len(),
        "重扫不重复任何审计"
    );
    let snapshot = state.snapshot().unwrap();
    assert_eq!(snapshot.session_id.as_deref(), Some(session_id.as_str()));
    assert_eq!(
        snapshot.state,
        Some(SessionState::Discarded),
        "采样线程看到的必须已是作废后的状态"
    );
    assert_eq!(snapshot.active_ms, 0, "作废的会话不再累积工时");
}

/// 同一条时序的另一半：`reconcile` 也是「提交之后第一步重扫门禁」的命令，
/// 而且它一旦失败，留下的旧镜像正是**用户刚对完账的那条 `recovering` 会话**——
/// 不刷新的话，快照会继续按 `recovering` + 待确认区间出结果，直到某条别的命令
/// 把它重新载入。
#[test]
fn a_failed_rescan_still_refreshes_the_mirror_of_the_reconciled_live_session() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    // 镜像 = 本 run 的会话；长间隔先把它按异常隔离成 `recovering`（唯一能走
    // `reconcile` 的状态），这一拍同时把新事实载回镜像。
    let t1_version = task_version(state.db(), "t1");
    state
        .start(start_request(&epoch, "t1", t1_version))
        .unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    app.advance(129_000);
    state.snapshot().unwrap();
    let recovering = session_of(state.db(), &session_id);
    assert_eq!(recovering.state, "recovering", "前置：异常已按待确认隔离");
    let session_version = recovering.row_version;
    assert_eq!(
        state.coordinator().live().unwrap().row_version,
        session_version,
        "前置：镜像就是这条 recovering 会话"
    );
    let revision_before = revision(state.db());
    let edits_before = world(state.db()).edits.len();

    // 注入「三条扫描查询失败」——用户命令本身仍然能提交。
    break_the_scan(state.db());
    let error = state
        .reconcile(
            env(&epoch, session_version),
            ReconcileRequest {
                session_id: session_id.clone(),
                action: ReconcileAction::DiscardUncertain,
                target_state: ReconcileTargetState::Paused,
                ranges: Vec::new(),
            },
        )
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");

    // 已提交事实保留：恰好一条审计、恰好一次版本，会话按命令收尾。
    let committed = world(state.db());
    assert_eq!(session_of(state.db(), &session_id).state, "paused");
    assert_eq!(committed.edits.len(), edits_before + 1, "恰好一条审计");
    assert_eq!(committed.revision, revision_before + 1, "恰好一次版本");

    // **镜像必须跟着已提交事实走**（与作废那条同一口径）。
    let live = state.coordinator().live().expect("live 不清成 None");
    assert_eq!(live.id, session_id);
    assert_eq!(
        live.state,
        SessionState::Paused,
        "镜像停在旧状态 = 继续把已对账的会话报成 recovering"
    );
    assert_eq!(
        live.row_version,
        session_of(state.db(), &session_id).row_version,
        "镜像的会话版本必须追上已提交的那一行"
    );
    assert!(
        live.open_interval.is_none(),
        "丢弃不确定区间之后没有开放区间"
    );

    // 扫描失败 ⇒ 失败标记自己挡住计时，且这次失败没有多写任何东西。
    assert_code(
        &state.guard_business_timing().unwrap_err(),
        "RECOVERY_REQUIRED",
    );

    repair_the_scan(state.db());
    let scan = state.rescan_recovery().unwrap();
    assert!(!scan.requires_recovery());
    assert!(
        state.guard_business_timing().is_ok(),
        "重扫成功必须清掉失败标记"
    );
    assert_eq!(revision(state.db()), committed.revision, "重扫不加版本");
    assert_eq!(
        world(state.db()).edits.len(),
        committed.edits.len(),
        "重扫不重复任何审计"
    );
    let snapshot = state.snapshot().unwrap();
    assert_eq!(
        snapshot.state,
        Some(SessionState::Paused),
        "采样线程看到的必须已是收尾后的状态"
    );
}
