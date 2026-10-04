//! P3 Task 6：**任务状态编排与会话联动**的原子性。
//!
//! 被测入口是 `services::tasks::transition_task`（经 `AppState::transition_task`，S2），
//! 取样与检测走 S3（`Coordinator::boundary_facts`）。装置抄 `tests/backfill_discard.rs`
//! 与 `tests/recovery_end_to_end.rs`：真跑一次 `bootstrap::startup`，然后在**同一把锁**
//! （`lock_app`）里下命令——P8 的 IPC 就走这条边界。
//!
//! 每个用例都钉住同一条口径：
//!
//! 1. 一笔用户命令**恰好**加一次 `revision`，被修改的会话 `row_version` 同步 +1；
//! 2. 被拒的命令（recovering / 第 1 类损坏 / 待确认事实）**逐字段零变化**；
//! 3. 取样只经 S3 一次——**不得**出现第二次取样或第二笔事务（R10：版本口径）；
//! 4. 末步骤失败（写审计失败）时，前面所有会话写入一起回滚。
//!
//! 后台采样节拍设成 1 小时：采样线程不得与用例自己的取样计数抢拍。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::domain::task::{TaskStatus, TransitionCause};
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::{Clock, ClockSample, FakeClock, SampleError};
use worktrace_lib::services::bootstrap::{
    lock_app, startup, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::tasks::TransitionTaskRequest;
use worktrace_lib::services::timer::coordinator::{SessionRequest, StartRequest};
use worktrace_lib::services::timer::snapshot::TimerSnapshot;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::run_repo;

/// 启动那一刻的挂钟。`FakeClock` 不自己走，所以「现在」只随用例的推进而变。
const WALL: i64 = 1_700_000_000_000;
/// 崩掉的那一代次（夹具里的旧 run；本文件的用例都不用它的会话）。
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
/// R10 的要害是「`report.revision` 只来自这次写事务」——所以这条路径上**不能**
/// 再出现一次取样：`Coordinator::snapshot` 自己取样，并在判为异常时再提交一笔
/// 独立系统恢复事务，于是「写事务的版本」与「快照的版本」会差 1。
/// 采样计数是这句话唯一的直接证据。
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

    /// 制造一次异常采样：单调钟走 1 秒、挂钟走 31 秒（阈值 2 秒）⇒ `Jumped`。
    fn make_anomaly(&self) {
        let mut clock = self.clock.lock().unwrap();
        clock.advance_monotonic(1_000);
        clock.advance_wall(31_000);
    }

    fn sample_count(&self) -> u64 {
        self.samples.load(Ordering::SeqCst)
    }

    /// 当前挂钟（= 下一条命令的归属终点，因为用例只推进 `FakeClock`）。
    fn wall_now(&self) -> i64 {
        self.clock.lock().unwrap().wall_ms()
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
            // 后台采样线程不得与用例的取样计数抢拍（本文件只测命令路径）。
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

// ── 读事实 ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskFacts {
    id: String,
    status: String,
    quality: Option<String>,
    row_version: i64,
    updated_at: i64,
}

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
struct ChangeFacts {
    task_id: String,
    before_json: String,
    after_json: String,
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

fn total_changes(db: &Db) -> i64 {
    db.connection()
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap()
}

fn task_row(db: &Db, id: &str) -> TaskFacts {
    db.connection()
        .query_row(
            "SELECT id,status,quality,row_version,updated_at FROM task WHERE id = ?1",
            [id],
            |r| {
                Ok(TaskFacts {
                    id: r.get(0)?,
                    status: r.get(1)?,
                    quality: r.get(2)?,
                    row_version: r.get(3)?,
                    updated_at: r.get(4)?,
                })
            },
        )
        .unwrap()
}

fn task_version(db: &Db, id: &str) -> i64 {
    task_row(db, id).row_version
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
        .prepare("SELECT task_id,before_json,after_json,created_at FROM task_change ORDER BY created_at, id")
        .unwrap();
    let changes = changes_stmt
        .query_map([], |r| {
            Ok(ChangeFacts {
                task_id: r.get(0)?,
                before_json: r.get(1)?,
                after_json: r.get(2)?,
                created_at: r.get(3)?,
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

fn session_request(epoch: &str, session_id: &str, session_version: i64) -> SessionRequest {
    SessionRequest {
        expected_data_epoch: epoch.to_string(),
        session_id: session_id.to_string(),
        session_expected_version: session_version,
    }
}

fn transition_request(
    task_id: &str,
    target: TaskStatus,
    cause: TransitionCause,
) -> TransitionTaskRequest {
    TransitionTaskRequest {
        task_id: task_id.to_string(),
        target,
        cause,
    }
}

fn assert_code(error: &AppError, code: &str) {
    assert_eq!(error.code(), code, "实际：{error:?}");
}

/// 让 `task_change` 的插入失败：那一步在 `task_repo::transition_task` 的**最后**，
/// 此时会话已经被结束过——所以它证明的是「前面写了一半也会整体回滚」。
fn reject_task_change_writes(db: &Db) {
    db.connection()
        .execute_batch(
            "CREATE TRIGGER reject_task_change BEFORE INSERT ON task_change
             BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;",
        )
        .unwrap();
}

fn allow_task_change_writes(db: &Db) {
    db.connection()
        .execute_batch("DROP TRIGGER reject_task_change;")
        .unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────
// 完成 / 取消：多会话联动，恰好一次 revision
// ─────────────────────────────────────────────────────────────────────────────

/// 完成任务会结束它**全部** running/paused 会话：运行中的闭合开放区间，
/// 暂停的直接 finished；整笔命令只加一次 `revision`，每条被改的会话 `row_version` +1。
#[test]
fn completing_a_task_ends_every_running_and_paused_session_with_one_revision() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    // S1：跑一会儿再暂停（任务仍是 Doing——暂停不改变任务状态）。
    let task_v = task_version(state.db(), "t1");
    state
        .start(start_request(&epoch, "t1", task_v))
        .expect("开始计时");
    let s1 = state.coordinator().live().unwrap().id.clone();
    app.advance(10_000);
    let s1_version = session_of(state.db(), &s1).row_version;
    state
        .pause(session_request(&epoch, &s1, s1_version))
        .expect("暂停");
    let s1_paused = session_of(state.db(), &s1);
    assert_eq!(s1_paused.state, "paused");
    // 暂停之后 S1 的区间已经可信闭合——后面结束**别的**会话不得把它重写一遍。
    let s1_intervals = intervals_of(state.db(), &s1);

    // S2：同一任务上再开一段（前台槽位在暂停后已释放）。
    app.advance(10_000);
    let task_v = task_version(state.db(), "t1");
    state
        .start(start_request(&epoch, "t1", task_v))
        .expect("再次开始计时");
    let s2 = state.coordinator().live().unwrap().id.clone();
    assert_ne!(s1, s2, "结束后再次开始是新 session");
    app.advance(5_000);

    let before = world(state.db());
    let rev_before = before.revision;
    let t2_before = task_row(state.db(), "t2");
    let expected_end = app.wall_now();
    let task_v = task_version(state.db(), "t1");
    let (report, changed) = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Done, TransitionCause::User),
        )
        .expect("完成任务应当成功")
        .into_parts();

    assert!(changed, "真的有会话被结束 ⇒ 必然 Changed");
    assert_eq!(report.task.status, TaskStatus::Done);
    assert_eq!(report.task.row_version, task_v + 1);
    assert_eq!(report.ended_sessions, vec![s1.clone(), s2.clone()]);
    assert!(report.paused_sessions.is_empty());
    assert_eq!(
        report.revision,
        rev_before + 1,
        "一笔用户命令恰好加一次 revision"
    );
    assert_eq!(report.data_epoch, epoch);
    assert_eq!(
        report.revision,
        revision(state.db()),
        "R10：报告里的版本必须与提交后 meta.revision 一致"
    );

    // 两条会话都终结在同一个归属终点上。
    let s1_after = session_of(state.db(), &s1);
    assert_eq!(s1_after.state, "finished");
    assert_eq!(s1_after.ended_at, Some(expected_end));
    assert_eq!(s1_after.row_version, s1_paused.row_version + 1);
    let s2_after = session_of(state.db(), &s2);
    assert_eq!(s2_after.state, "finished");
    assert_eq!(s2_after.ended_at, Some(expected_end));
    assert_eq!(s2_after.row_version, 1, "新建行 0 → 结束后 1");

    // 运行中的那条：开放区间在归属终点处可信闭合。
    let s2_intervals = intervals_of(state.db(), &s2);
    assert_eq!(s2_intervals.len(), 1);
    assert_eq!(s2_intervals[0].ended_at, Some(expected_end));
    assert_eq!(s2_intervals[0].duration_ms, Some(5_000));
    assert_eq!(s2_intervals[0].sampled_end_wall_at, Some(expected_end));
    assert_eq!(s2_intervals[0].needs_review, 0);
    // 暂停过的 S1：它的可信区间**不得**被重写、不得多出第二段。
    assert_eq!(
        intervals_of(state.db(), &s1),
        s1_intervals,
        "结束别的会话不得重写已闭合的事实"
    );

    // 审计：恰好一条 Doing → Done；对照任务一个字段都没动。
    let after = world(state.db());
    assert_eq!(after.changes.len(), before.changes.len() + 1);
    // 按**内容**取那一条，不按顺序：同一毫秒里可能落多条审计（`created_at` 相同），
    // 排序退化成 UUID 就不确定了。
    let completions: Vec<&ChangeFacts> = after
        .changes
        .iter()
        .filter(|change| change.after_json.contains("\"Done\""))
        .collect();
    assert_eq!(completions.len(), 1, "完成恰好留一条审计：{completions:?}");
    assert_eq!(completions[0].task_id, "t1");
    assert!(completions[0].before_json.contains("\"Doing\""));
    assert_eq!(task_row(state.db(), "t2"), t2_before);
    // 提交后收尾只重建镜像，不再加版本。
    assert_eq!(revision(state.db()), rev_before + 1);
    let live = state.coordinator().live().unwrap();
    assert_eq!(
        live.id, s2,
        "镜像落在刚刚计时的那条会话上（与 P2 的 finish 同一条收尾路径）"
    );
    assert_eq!(live.state, SessionState::Finished);
}

/// 取消与完成同一条联动路径：running 会话被结束，任务进终态，恰好一次 `revision`。
#[test]
fn cancelling_a_task_ends_its_running_session_with_one_revision() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let task_v = task_version(state.db(), "t2");
    state
        .start(start_request(&epoch, "t2", task_v))
        .expect("开始计时");
    let session_id = state.coordinator().live().unwrap().id.clone();
    app.advance(7_000);

    let rev_before = revision(state.db());
    let expected_end = app.wall_now();
    let task_v = task_version(state.db(), "t2");
    let (report, changed) = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t2", TaskStatus::Cancelled, TransitionCause::User),
        )
        .expect("取消任务应当成功")
        .into_parts();

    assert!(changed);
    assert_eq!(report.task.status, TaskStatus::Cancelled);
    assert_eq!(report.ended_sessions, vec![session_id.clone()]);
    assert_eq!(report.revision, rev_before + 1);
    assert_eq!(revision(state.db()), rev_before + 1);

    let session = session_of(state.db(), &session_id);
    assert_eq!(session.state, "finished");
    assert_eq!(session.ended_at, Some(expected_end));
    let intervals = intervals_of(state.db(), &session_id);
    assert_eq!(
        intervals[0].duration_ms,
        Some(7_000),
        "取消也按归属终点闭合"
    );
    assert_eq!(task_row(state.db(), "t1").status, "Doing", "别的任务不动");
}

// ─────────────────────────────────────────────────────────────────────────────
// 完成/取消前的整体闸：recovering / 第 1 类损坏 / 待确认事实
// ─────────────────────────────────────────────────────────────────────────────

/// 该任务有 `recovering`（还没作废的待确认区间）⇒ **整体拒绝**，且逐字段零变化；
/// 同一任务上那条本来能正常结束的 running 会话也不得被部分执行。
#[test]
fn a_recovering_session_with_pending_intervals_rejects_the_whole_completion() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let run = app.running.run_id().to_string();
    let mut state = lock_app(app.running.app());

    // 本 run 的一条 recovering 会话：可信前缀 + 终点未知的待确认段（S5 归一后的形态）。
    insert_session(
        state.db(),
        "s-rec",
        "t1",
        &run,
        "recovering",
        WALL - 30_000,
        None,
        1,
    );
    insert_interval(
        state.db(),
        "s-rec-prefix",
        "s-rec",
        WALL - 30_000,
        Some(WALL - 20_000),
        Some(10_000),
        0,
        None,
    );
    insert_interval(
        state.db(),
        "s-rec-open",
        "s-rec",
        WALL - 20_000,
        None,
        None,
        1,
        None,
    );
    // 同一任务上还有一条能正常结束的 running 会话：整体闸必须挡在写之前。
    insert_session(
        state.db(),
        "s-run",
        "t1",
        &run,
        "running",
        WALL - 5_000,
        None,
        0,
    );
    insert_interval(
        state.db(),
        "s-run-a",
        "s-run",
        WALL - 5_000,
        None,
        None,
        0,
        None,
    );

    let before = world(state.db());
    let task_v = task_version(state.db(), "t1");
    let error = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Done, TransitionCause::User),
        )
        .expect_err("有 recovering 会话时完成必须整体拒绝");
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(
        world(state.db()),
        before,
        "被拒的命令不得写任何东西：会话、区间、任务、审计、revision 逐字段不变"
    );

    // 取消同理（同一条闸，不是「只有完成才挡」）。
    let error = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Cancelled, TransitionCause::User),
        )
        .expect_err("取消也要整体拒绝");
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(world(state.db()), before);
}

/// 第 1 类（不变量损坏）同样整体拒绝：「结束一下」会把损坏洗成事实。
///
/// 这条夹具命中的是 `running_without_open_interval`——recovering/待确认那道闸
/// **盖不住它**（既不是 recovering，也没有待确认区间），所以它证明的是
/// 第 1 类判据本身在起作用。
#[test]
fn class_one_damage_rejects_the_whole_completion() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let run = app.running.run_id().to_string();
    let mut state = lock_app(app.running.app());

    // running 却没有开放区间：02 §4 表的前提不成立。
    insert_session(
        state.db(),
        "s-broken",
        "t1",
        &run,
        "running",
        WALL - 5_000,
        None,
        0,
    );

    let before = world(state.db());
    let task_v = task_version(state.db(), "t1");
    let error = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Done, TransitionCause::User),
        )
        .expect_err("第 1 类损坏必须整体拒绝");
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(world(state.db()), before, "只诊断、不修，也不写半个事实");

    let error = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Cancelled, TransitionCause::User),
        )
        .expect_err("取消同样拒绝");
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(world(state.db()), before);
}

/// 会话上挂着**未作废的待确认区间**（会话级标记为 0、只有区间待确认）⇒ 整体拒绝。
///
/// 判据必须看区间本身，而不只是 `work_session.needs_review`：
/// `paused` + 待确认正是 Ruling 6 那一类四类判定盖不住的形态。
#[test]
fn a_pending_interval_rejects_the_whole_completion() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let run = app.running.run_id().to_string();
    let mut state = lock_app(app.running.app());

    insert_session(
        state.db(),
        "s-paused",
        "t1",
        &run,
        "paused",
        WALL - 9_000,
        Some(WALL - 8_000),
        0,
    );
    insert_interval(
        state.db(),
        "s-paused-cand",
        "s-paused",
        WALL - 9_000,
        Some(WALL - 8_000),
        None,
        1,
        None,
    );

    let before = world(state.db());
    let task_v = task_version(state.db(), "t1");
    let error = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Done, TransitionCause::User),
        )
        .expect_err("待确认事实必须挡住完成");
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(world(state.db()), before);
}

// ─────────────────────────────────────────────────────────────────────────────
// Blocked / Waiting：同事务暂停 running 会话
// ─────────────────────────────────────────────────────────────────────────────

/// 设为 `Blocked`/`Waiting` 时暂停该任务的 running 会话（`end_session_in_tx` + `Paused`），
/// 已经暂停的会话**不动**（不重复写、不重复加版本）。
#[test]
fn blocked_and_waiting_pause_only_the_running_sessions() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    // t1：先跑一段、暂停，再开一段（现在是 running）。
    let task_v = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", task_v)).unwrap();
    let s1 = state.coordinator().live().unwrap().id.clone();
    app.advance(4_000);
    let v = session_of(state.db(), &s1).row_version;
    state.pause(session_request(&epoch, &s1, v)).unwrap();
    app.advance(4_000);
    let task_v = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", task_v)).unwrap();
    let s2 = state.coordinator().live().unwrap().id.clone();
    app.advance(3_000);

    let s1_before = session_of(state.db(), &s1);
    let s1_intervals_before = intervals_of(state.db(), &s1);
    let rev_before = revision(state.db());
    let expected_end = app.wall_now();
    let task_v = task_version(state.db(), "t1");
    let (report, changed) = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Blocked, TransitionCause::User),
        )
        .expect("设为 Blocked 应当成功")
        .into_parts();
    assert!(changed);
    assert_eq!(report.task.status, TaskStatus::Blocked);
    assert_eq!(report.paused_sessions, vec![s2.clone()]);
    assert!(report.ended_sessions.is_empty());
    assert_eq!(report.revision, rev_before + 1);

    let s2_after = session_of(state.db(), &s2);
    assert_eq!(s2_after.state, "paused");
    assert_eq!(s2_after.ended_at, None, "暂停不写会话终点");
    assert_eq!(s2_after.row_version, 1);
    let intervals = intervals_of(state.db(), &s2);
    assert_eq!(intervals[0].ended_at, Some(expected_end));
    assert_eq!(intervals[0].duration_ms, Some(3_000));
    assert_eq!(intervals[0].needs_review, 0);
    assert_eq!(
        session_of(state.db(), &s1),
        s1_before,
        "已暂停的会话一个字段都不该动"
    );
    assert_eq!(intervals_of(state.db(), &s1), s1_intervals_before);
    assert_eq!(
        state.coordinator().live().map(|live| live.id.clone()),
        Some(s2),
        "镜像跟着被暂停的那条"
    );

    // t2：Waiting 走同一条联动。
    app.advance(4_000);
    let task_v = task_version(state.db(), "t2");
    state.start(start_request(&epoch, "t2", task_v)).unwrap();
    let s3 = state.coordinator().live().unwrap().id.clone();
    app.advance(2_000);
    let task_v = task_version(state.db(), "t2");
    let (report, changed) = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t2", TaskStatus::Waiting, TransitionCause::User),
        )
        .expect("设为 Waiting 应当成功")
        .into_parts();
    assert!(changed);
    assert_eq!(report.task.status, TaskStatus::Waiting);
    assert_eq!(report.paused_sessions, vec![s3.clone()]);
    assert_eq!(session_of(state.db(), &s3).state, "paused");
}

/// 02 §5：**暂停不必改变 `Doing`**——Doing 表示任务尚在处理，不等同 running session。
#[test]
fn a_plain_pause_does_not_move_the_task_out_of_doing() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let task_v = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", task_v)).unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    // `start` 会冻结估时基准（首次 start 的既有语义），任务版本因此前进过一格。
    let task_v_after_start = task_version(state.db(), "t1");
    app.advance(6_000);

    let v = session_of(state.db(), &session_id).row_version;
    state
        .pause(session_request(&epoch, &session_id, v))
        .expect("暂停");
    assert_eq!(session_of(state.db(), &session_id).state, "paused");
    assert_eq!(
        task_row(state.db(), "t1").status,
        "Doing",
        "暂停不自动改变任务状态"
    );
    assert_eq!(
        task_row(state.db(), "t1").row_version,
        task_v_after_start,
        "暂停不碰任务行"
    );
}

/// P2 既有语义：**单独结束会话不等于完成任务**（02 §3 末）。
///
/// 会话命令不得反向自动推进任务状态，也不得伪造完成事件——联动只发生在
/// 「任务状态命令」这一侧。
#[test]
fn finishing_a_session_does_not_complete_the_task() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let task_v = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", task_v)).unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    let task_v_after_start = task_version(state.db(), "t1");
    let changes_before = world(state.db()).changes.len();
    app.advance(5_000);

    let v = session_of(state.db(), &session_id).row_version;
    state
        .finish(session_request(&epoch, &session_id, v))
        .expect("结束计时");
    assert_eq!(session_of(state.db(), &session_id).state, "finished");
    assert_eq!(
        task_row(state.db(), "t1").status,
        "Doing",
        "结束会话不自动完成任务"
    );
    assert_eq!(
        task_row(state.db(), "t1").row_version,
        task_v_after_start,
        "结束会话不碰任务行"
    );
    assert_eq!(
        world(state.db()).changes.len(),
        changes_before,
        "结束会话不得伪造完成事件（不写 task_change）"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// reopen 与幂等
// ─────────────────────────────────────────────────────────────────────────────
/// 显式 reopen：终结态回 `Ready`、清当前质量、保留历史；**不**自动恢复旧会话。
#[test]
fn reopening_clears_the_quality_and_does_not_restore_any_session() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    // 先真正完成一次（带着一条 running 会话），再补上完成质量。
    let task_v = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", task_v)).unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    app.advance(5_000);
    let task_v = task_version(state.db(), "t1");
    state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Done, TransitionCause::User),
        )
        .expect("完成");
    assert_eq!(task_row(state.db(), "t1").status, "Done");
    state
        .db()
        .connection()
        .execute("UPDATE task SET quality='normal' WHERE id='t1'", [])
        .unwrap();

    let session_before = session_of(state.db(), &session_id);
    let intervals_before = intervals_of(state.db(), &session_id);
    let sessions_before = world(state.db()).sessions.len();
    let rev_before = revision(state.db());
    let task_v = task_version(state.db(), "t1");
    let (report, changed) = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Ready, TransitionCause::Reopen),
        )
        .expect("显式 reopen 应当成功")
        .into_parts();

    assert!(changed);
    assert_eq!(report.task.status, TaskStatus::Ready);
    assert_eq!(report.task.quality, None, "重开清除当前质量");
    assert_eq!(report.task.row_version, task_v + 1);
    assert!(report.ended_sessions.is_empty(), "重开不结束任何会话");
    assert!(report.paused_sessions.is_empty());
    assert_eq!(report.revision, rev_before + 1, "reopen 也是一笔用户命令");

    // 旧质量留在变更记录里；旧会话保持终态，不自动恢复、也不新建会话。
    let after = world(state.db());
    assert_eq!(after.sessions.len(), sessions_before, "重开不新建会话");
    // 按**内容**取 reopen 那一条：完成与重开可能落在同一毫秒里（`created_at` 相同），
    // 按顺序取会变成不确定的断言。
    let reopened: Vec<&ChangeFacts> = after
        .changes
        .iter()
        .filter(|change| change.after_json.contains("\"Ready\""))
        .collect();
    assert_eq!(reopened.len(), 1, "reopen 恰好留一条审计：{reopened:?}");
    assert_eq!(reopened[0].task_id, "t1");
    assert!(
        reopened[0].before_json.contains("\"normal\""),
        "旧质量保留在变更记录里：{:?}",
        reopened[0]
    );
    assert!(
        reopened[0].after_json.contains("null"),
        "新质量为空：{:?}",
        reopened[0]
    );
    assert_eq!(session_of(state.db(), &session_id), session_before);
    assert_eq!(intervals_of(state.db(), &session_id), intervals_before);
    assert_eq!(
        state.coordinator().live().map(|live| live.state),
        Some(SessionState::Finished),
        "不自动恢复旧会话（镜像仍停在已结束的那条）"
    );
}

/// 幂等重复：已经是目标状态、且没有 running/paused 会话 ⇒ `Unchanged`，
/// 不写 `task_change`、不加 `revision`、不动 `row_version`、一行都不写。
#[test]
fn repeating_the_same_transition_writes_nothing() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let task_v = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", task_v)).unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    app.advance(5_000);
    let task_v = task_version(state.db(), "t1");
    state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Done, TransitionCause::User),
        )
        .expect("完成");

    let before = world(state.db());
    let writes_before = total_changes(state.db());
    let task_v = task_version(state.db(), "t1");
    let (report, changed) = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Done, TransitionCause::User),
        )
        .expect("重复提交是幂等的")
        .into_parts();

    assert!(!changed, "没有任何字段要改 ⇒ Unchanged");
    assert_eq!(report.task.status, TaskStatus::Done);
    assert_eq!(report.task.row_version, task_v);
    assert!(report.ended_sessions.is_empty());
    assert!(report.paused_sessions.is_empty());
    assert_eq!(report.revision, before.revision, "Unchanged 不加版本");
    assert_eq!(report.data_epoch, epoch);
    assert_eq!(world(state.db()), before, "逐字段零变化");
    assert_eq!(total_changes(state.db()), writes_before, "一行都不写");
    assert_eq!(session_of(state.db(), &session_id).state, "finished");
}

// ─────────────────────────────────────────────────────────────────────────────
// 失败与并发
// ─────────────────────────────────────────────────────────────────────────────

/// 末步骤（写 `task_change`）失败 ⇒ 前面的会话结束**全部回滚**，镜像也不动。
#[test]
fn a_failure_in_the_last_step_rolls_back_every_session_change() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let task_v = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", task_v)).unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    app.advance(5_000);

    let before = world(state.db());
    let live_before = state.coordinator().live().cloned();
    reject_task_change_writes(state.db());
    let task_v = task_version(state.db(), "t1");
    let error = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Done, TransitionCause::User),
        )
        .expect_err("审计写不进去 ⇒ 整笔命令失败");
    assert_code(&error, "STORAGE_ERROR");
    assert_eq!(
        world(state.db()),
        before,
        "会话、区间、任务与审计必须在同一个事务里一起回滚"
    );
    let live_after = state.coordinator().live().cloned();
    assert_eq!(
        live_after.map(|live| (live.id, live.state, live.row_version, live.open_interval)),
        live_before.map(|live| (live.id, live.state, live.row_version, live.open_interval)),
        "事务回滚 ⇒ 内存镜像也不得跟着动（还没到提交后收尾）"
    );

    // 解除注入之后同一条请求必须成功（证明失败只来自注入的那一步）。
    allow_task_change_writes(state.db());
    let (report, changed) = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Done, TransitionCause::User),
        )
        .expect("解除注入后应当成功")
        .into_parts();
    assert!(changed);
    assert_eq!(report.task.status, TaskStatus::Done);
    assert_eq!(report.ended_sessions, vec![session_id.clone()]);
    assert_eq!(session_of(state.db(), &session_id).state, "finished");
    assert_eq!(report.revision, revision(state.db()));
}

/// 与采样一拍并发：同一把锁（`AppGuard`）串行 ⇒ 那一拍只能看到**提交后**的完整状态，
/// 不会看到「会话已结束但任务还没改」的半个状态。
#[test]
fn a_concurrent_tick_cannot_observe_a_half_applied_transition() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let shared = Arc::clone(app.running.app());
    let mut state = lock_app(app.running.app());

    let task_v = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", task_v)).unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    app.advance(5_000);

    let (entered_tx, entered_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel::<Result<TimerSnapshot, AppError>>();
    let worker = thread::spawn(move || {
        entered_tx.send(()).unwrap();
        let snapshot = lock_app(&shared).tick();
        done_tx.send(snapshot).unwrap();
    });

    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("采样线程应当先跑到锁前面");
    // 给它一点时间真的去抢锁；本线程正持着锁，所以它不可能完成。
    thread::sleep(Duration::from_millis(100));
    assert!(
        done_rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "命令持锁期间那一拍不可能完成——同一把锁上排队"
    );

    let task_v = task_version(state.db(), "t1");
    let (report, changed) = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Done, TransitionCause::User),
        )
        .expect("完成任务应当成功")
        .into_parts();
    assert!(changed);
    drop(state);

    let snapshot = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("释放锁之后那一拍必须完成")
        .expect("tick 应当成功");
    worker.join().unwrap();

    // 那一拍看到的是**提交后**的完整状态：会话、任务版本、版本号三者一致。
    assert_eq!(snapshot.session_id, Some(session_id.clone()));
    assert_eq!(snapshot.state, Some(SessionState::Finished));
    assert_eq!(snapshot.session_version, Some(1));
    assert_eq!(snapshot.revision, report.revision);
    assert_eq!(snapshot.task_row_version, Some(report.task.row_version));

    let state = lock_app(app.running.app());
    assert_eq!(session_of(state.db(), &session_id).state, "finished");
    assert_eq!(task_row(state.db(), "t1").status, "Done");
    assert_eq!(revision(state.db()), report.revision);
}

// ─────────────────────────────────────────────────────────────────────────────
// S3：异常检测先提交系统事务，原意图不执行
// ─────────────────────────────────────────────────────────────────────────────

/// 取样判为异常 ⇒ **先提交独立系统恢复事务**（会话转 recovering + 审计 + 恰好一次
/// `revision`），随后原命令返回 `RECOVERY_REQUIRED`、**不执行原意图**；
/// 重复提交**不会**再提交第二笔系统事务。
#[test]
fn an_anomaly_commits_one_system_transaction_and_skips_the_original_intent() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    let task_v = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", task_v)).unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    app.advance(10_000);

    let before = world(state.db());
    let session_before = session_of(state.db(), &session_id);
    let task_v = task_version(state.db(), "t1");
    app.make_anomaly();
    let error = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Done, TransitionCause::User),
        )
        .expect_err("异常检测先提交系统事务，原命令不执行");
    assert_code(&error, "RECOVERY_REQUIRED");

    let after = world(state.db());
    // 原意图没有执行：任务还是 Doing，没有 task_change，会话版本没有被用户事务碰过。
    assert_eq!(task_row(state.db(), "t1").status, "Doing");
    assert_eq!(after.changes.len(), before.changes.len());
    // 系统事务恰好一次：recovering + 一条审计 + 一次 revision。
    assert_eq!(after.edits.len(), before.edits.len() + 1);
    assert_eq!(after.revision, before.revision + 1);
    let session = session_of(state.db(), &session_id);
    assert_eq!(session.state, "recovering");
    assert_eq!(session.needs_review, 1);
    assert_eq!(
        session.row_version,
        session_before.row_version + 1,
        "系统事务只把会话推成 recovering 一次"
    );
    assert_eq!(session.ended_at, None, "会话本身没有终点，终点在区间上");
    let intervals = intervals_of(state.db(), &session_id);
    assert_eq!(intervals.len(), 1, "没有可信前缀时不另造余段");
    assert_eq!(intervals[0].needs_review, 1);
    assert_eq!(intervals[0].duration_ms, None, "候选时长不是工时");

    // 第二次提交：那一拍采样已经被消费，系统事务**不得**再提交一次。
    let task_v = task_version(state.db(), "t1");
    let error = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Done, TransitionCause::User),
        )
        .expect_err("recovering 会话仍然挡住完成");
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(world(state.db()), after, "第二笔事务一次都没有发生");
    assert_eq!(revision(state.db()), before.revision + 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// R10：版本口径与「不产生第二次取样 / 第二笔事务」
// ─────────────────────────────────────────────────────────────────────────────

/// `report.revision`/`report.data_epoch` **只来自这次写事务**（`settle` 同事务读回）：
/// 提交后 `meta.revision` 必须逐字相等；取样**恰好一次**（S3），
/// 提交后的镜像重建既不再取样、也不开第二笔事务。
#[test]
fn the_report_revision_comes_from_the_write_transaction_only() {
    let paths = fixture();
    let db = seeded(&paths);
    drop(db);
    let app = started(paths);
    let epoch = app.epoch();
    let mut state = lock_app(app.running.app());

    // ① 没有会话被结束的那一次：Ready → Doing。
    let samples_before = app.sample_count();
    let writes_before = total_changes(state.db());
    let rev_before = revision(state.db());
    let tick_before = state.coordinator().tick_seq();
    let task_v = task_version(state.db(), "t3");
    let (report, changed) = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t3", TaskStatus::Doing, TransitionCause::User),
        )
        .expect("Ready → Doing 应当成功")
        .into_parts();
    assert!(changed);
    assert_eq!(report.task.status, TaskStatus::Doing);
    assert!(report.ended_sessions.is_empty());
    assert_eq!(report.revision, rev_before + 1);
    assert_eq!(report.data_epoch, epoch);
    assert_eq!(
        report.revision,
        revision(state.db()),
        "报告里的 revision 必须等于提交后 meta.revision"
    );
    assert_eq!(
        app.sample_count() - samples_before,
        1,
        "这条路径只经 S3 取一次样本；再取一次（例如 Coordinator::snapshot）就是违规"
    );
    assert_eq!(
        total_changes(state.db()) - writes_before,
        3,
        "只有这次用户事务的写入：task UPDATE + task_change INSERT + revision 自增，没有第二笔事务"
    );
    assert_eq!(
        state.coordinator().tick_seq(),
        tick_before,
        "命令不推进采样拍号"
    );

    // ② 有会话被结束的那一次：提交后收尾只重建镜像，版本仍只来自写事务。
    let task_v = task_version(state.db(), "t1");
    state.start(start_request(&epoch, "t1", task_v)).unwrap();
    let session_id = state.coordinator().live().unwrap().id.clone();
    app.advance(5_000);

    let samples_before = app.sample_count();
    let writes_before = total_changes(state.db());
    let tick_before = state.coordinator().tick_seq();
    let task_v = task_version(state.db(), "t1");
    let (report, changed) = state
        .transition_task(
            WriteEnvelope::for_update(&epoch, task_v),
            transition_request("t1", TaskStatus::Done, TransitionCause::User),
        )
        .expect("完成任务应当成功")
        .into_parts();
    assert!(changed);
    assert_eq!(report.ended_sessions, vec![session_id.clone()]);
    assert_eq!(
        report.revision,
        revision(state.db()),
        "提交后收尾（rebuild_from_committed）不得让版本再变一次"
    );
    assert_eq!(
        app.sample_count() - samples_before,
        1,
        "取样仍然只有 S3 那一次：收尾用的是同一个样本"
    );
    assert_eq!(
        total_changes(state.db()) - writes_before,
        5,
        "会话闭合 + 会话状态 + 任务 + 审计 + revision 自增，恰好五行；收尾不开第二笔事务"
    );
    assert_eq!(state.coordinator().tick_seq(), tick_before);
    assert_eq!(
        state.coordinator().run_id(),
        app.running.run_id(),
        "命令不换 run"
    );
}
