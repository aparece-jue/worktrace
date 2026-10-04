//! P3 Task 5：**端到端闭环**——把 Task 1–4 的恢复与历史链路在**真库**上串起来跑。
//!
//! 装置抄 `tests/reconcile.rs` / `tests/backfill_discard.rs`：造一个「上一个 run 崩溃时
//! 留下现场」的库，走**真实**的 `bootstrap::startup`（第 ④ 步的扫描会做四类判定并落地
//! 事实），然后在同一把锁（`lock_app`）里依次调用 `AppState` 的命令入口（S2 包装——
//! P8 的 IPC 就走这些）。
//!
//! 链路：
//!
//! ```text
//! running（旧 run，开放区间 + 最后成功检查点）
//!   → 崩溃（旧 run 的库行 + 检查点还在）
//!   → 重启扫描：第 2 类归一（可信前缀闭合 + 终点未知的待确认段）⇒ recovering
//!   → 门禁关着：start 被拒且逐字段零变化
//!   → reconcile(confirm) ⇒ finished，门禁放开
//!   → start 成功（本 run 正常计时）⇒ finish
//!   → resume 上一代留下的 paused 会话（run_id 切到本次 run）
//!   → correct（重定时可信历史）→ backfill（手工补录）→ discard_session（作废整次，
//!     重复提交幂等）
//! ```
//!
//! 每一步都断言三件事：
//!
//! 1. **统计可读的区间集合始终两两不重叠**（用 `domain::interval::IntervalSet::insert`
//!    这一条既有原语判，不新写相交实现），且**同一段时间不会被算两遍**——
//!    可信前缀 `duration_ms = 检查点归属时刻 - started_at` 只计一次，
//!    停机时间（检查点 → 现在）与待确认段（`duration_ms IS NULL`）**都不进任何
//!    「已确认」数字**；
//! 2. `revision` 每次**用户命令**恰好 +1（整批一次的扫描事务另计一次），
//!    幂等重复 +0；
//! 3. 被拒的命令**逐字段零变化**（`WorldFacts` 全表快照，总纲 §5 第 8 条的口径）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use worktrace_lib::domain::interval::{IntervalRange, IntervalSet};
use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::history::{BackfillRequest, CorrectAction, CorrectRequest};
use worktrace_lib::services::recovery::{
    ConfirmedRange, DiscardSessionRequest, ReconcileAction, ReconcileRequest, ReconcileTargetState,
};
use worktrace_lib::services::timer::coordinator::{ResumeRequest, SessionRequest, StartRequest};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::run_repo;

/// 启动那一刻的挂钟。`FakeClock` 不自己走，所以 `now` 与归属时刻恒等于它。
const WALL: i64 = 1_700_000_000_000;
/// 崩掉的那一代次。
const OLD_RUN: &str = "run-old";

// 崩溃现场的几何（全部落在 `WALL` 之前，单位毫秒）。
/// 旧 run 那个 running 会话的区间起点。
const T0: i64 = WALL - 60_000;
/// 最后成功检查点的归属时刻：可信前缀在这里闭合，**停机时间从这里开始**。
const PREFIX_END: i64 = WALL - 40_000;
/// 检查点记录的挂钟值（与归属时刻不同，特意留出偏移好分辨两者）。
const CHECKPOINT_WALL: i64 = WALL - 40_500;
/// 用户在 `reconcile(confirm)` 里给待确认段指定的终点。
const CONFIRMED_END: i64 = WALL - 30_000;
/// `correct` 重定时之后的可信前缀终点（收短 1 秒，留出与后一段的间隙）。
const CORRECT_END: i64 = PREFIX_END - 1_000;
/// 第 4 类：旧 run 留下的 `paused` 会话（没有开放区间、没有待确认段）与它的可信区间。
const P_START: i64 = WALL - 20_000;
const P_END: i64 = WALL - 18_000;
/// 手工补录的范围（不占前台、不伪造完成事件）。
const BF_START: i64 = WALL - 10_000;
const BF_END: i64 = WALL - 8_000;

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

struct Fixture {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    lock_path: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    Fixture {
        db_path: dir.path().join("worktrace.db"),
        lock_path: dir.path().join("instance.lock"),
        _dir: dir,
    }
}

fn started(fx: &Fixture) -> Box<RunningApp> {
    let sink = Arc::new(RecordingSink::default());
    let outcome = startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path),
        Box::new(FakeClock::new(WALL, 0)),
        sink,
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功");
    match outcome {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    }
}

/// 崩溃现场：旧 run + 一条任务 + 一个 running 会话（开放区间 + 检查点）
/// + 一个 paused 会话（无开放区间、无待确认段 ⇒ 第 4 类）。
fn seeded(fx: &Fixture) -> Db {
    let mut db = Db::open(&fx.db_path).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    run_repo::start_run(&tx, OLD_RUN, WALL - 120_000).unwrap();
    tx.execute(
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Doing',0,1000,1000)",
        [],
    )
    .unwrap();
    // 崩溃在运行中：running + 开放区间。
    tx.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
         VALUES('r','t1','run-old','FOREGROUND','running','stopwatch',?1,0)",
        rusqlite::params![T0],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO work_interval(id,session_id,started_at,needs_review)
         VALUES('r-prefix','r',?1,0)",
        rusqlite::params![T0],
    )
    .unwrap();
    // 「最后成功持久化的归属时刻」——崩溃后的唯一可信边界。
    tx.execute(
        "INSERT INTO interval_checkpoint(interval_id,run_id,wall_at,attribution_at,elapsed_ms)
         VALUES('r-prefix','run-old',?1,?2,?3)",
        rusqlite::params![CHECKPOINT_WALL, PREFIX_END, PREFIX_END - T0],
    )
    .unwrap();
    // 崩溃前已经暂停、且没有待确认事实的会话：第 4 类（扫描会把它重绑到本次 run）。
    tx.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,ended_at,row_version)
         VALUES('p','t1','run-old','FOREGROUND','paused','stopwatch',?1,?2,0)",
        rusqlite::params![P_START, P_END],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms,needs_review)
         VALUES('p-1','p',?1,?2,?3,0)",
        rusqlite::params![P_START, P_END, P_END - P_START],
    )
    .unwrap();
    tx.commit().unwrap();
    db
}

/// 只留下「上一代 run + 一条任务」的库：没有恢复材料 ⇒ 门禁开着。
///
/// `resume` 要过门禁（`AppState::resume` 先问 `guard_business_timing`），所以
/// 「`resume` 自己重绑 run_id」那条用例需要一个干净起点。
fn seeded_task(fx: &Fixture) -> Db {
    let mut db = Db::open(&fx.db_path).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    run_repo::start_run(&tx, OLD_RUN, WALL - 120_000).unwrap();
    tx.execute(
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Doing',0,1000,1000)",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    db
}

fn start_request(epoch: &str, task_version: i64) -> StartRequest {
    StartRequest {
        expected_data_epoch: epoch.to_string(),
        task_id: "t1".into(),
        task_expected_version: task_version,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: 1_000,
    }
}

fn assert_code(error: &AppError, code: &str) {
    assert_eq!(error.code(), code, "实际：{error:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 事实的快照（全部直连 SQL，不拿被测的服务入口当 oracle）
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionFacts {
    id: String,
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
struct CheckpointFacts {
    interval_id: String,
    run_id: String,
    wall_at: i64,
    attribution_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuditFacts {
    id: String,
    session_id: String,
    reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskFacts {
    status: String,
    row_version: i64,
    updated_at: i64,
}

/// 全库事实快照：被拒命令的「逐字段零变化」用 `assert_eq!(world(..), before)` 一句话比对
/// （总纲 §5 第 8 条：不只比行数）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorldFacts {
    revision: i64,
    sessions: Vec<SessionFacts>,
    intervals: Vec<IntervalFacts>,
    checkpoints: Vec<CheckpointFacts>,
    audits: Vec<AuditFacts>,
    task: TaskFacts,
}

fn revision(db: &Db) -> i64 {
    db.connection()
        .query_row(
            "SELECT revision FROM app_meta WHERE singleton = 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

fn task_facts(db: &Db) -> TaskFacts {
    db.connection()
        .query_row(
            "SELECT status, row_version, updated_at FROM task WHERE id = 't1'",
            [],
            |r| {
                Ok(TaskFacts {
                    status: r.get(0)?,
                    row_version: r.get(1)?,
                    updated_at: r.get(2)?,
                })
            },
        )
        .unwrap()
}

fn task_version(db: &Db) -> i64 {
    task_facts(db).row_version
}

fn read_session(db: &Db, id: &str) -> SessionFacts {
    db.connection()
        .query_row(
            "SELECT id, run_id, state, started_at, ended_at, needs_review, row_version
               FROM work_session WHERE id = ?1",
            [id],
            |r| {
                Ok(SessionFacts {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    state: r.get(2)?,
                    started_at: r.get(3)?,
                    ended_at: r.get(4)?,
                    needs_review: r.get(5)?,
                    row_version: r.get(6)?,
                })
            },
        )
        .unwrap()
}

fn read_interval(db: &Db, id: &str) -> IntervalFacts {
    intervals_where(db, "i.id = ?1", [id]).remove(0)
}

/// 一条会话的**全部**区间（含已作废的），按 `started_at, id` 稳定排序。
fn intervals_of(db: &Db, session_id: &str) -> Vec<IntervalFacts> {
    intervals_where(db, "i.session_id = ?1", [session_id])
}

fn intervals_where<P: rusqlite::Params>(db: &Db, predicate: &str, params: P) -> Vec<IntervalFacts> {
    let sql = format!(
        "SELECT i.id, i.session_id, i.started_at, i.ended_at, i.duration_ms,
                i.sampled_end_wall_at, i.needs_review, i.voided_at
           FROM work_interval i WHERE {predicate} ORDER BY i.started_at, i.id"
    );
    let conn = db.connection();
    let mut stmt = conn.prepare(&sql).unwrap();
    let rows = stmt
        .query_map(params, |r| {
            Ok(IntervalFacts {
                id: r.get(0)?,
                session_id: r.get(1)?,
                started_at: r.get(2)?,
                ended_at: r.get(3)?,
                duration_ms: r.get(4)?,
                sampled_end_wall_at: r.get(5)?,
                needs_review: r.get(6)?,
                voided_at: r.get(7)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    rows
}

/// 该会话唯一那条待确认区间（扫描刚造出来的那条）的 id。
fn pending_interval_id(db: &Db, session_id: &str) -> String {
    let pending: Vec<IntervalFacts> = intervals_of(db, session_id)
        .into_iter()
        .filter(|i| i.needs_review == 1 && i.voided_at.is_none())
        .collect();
    assert_eq!(pending.len(), 1, "恢复现场应当恰好有一条待确认区间");
    pending[0].id.clone()
}

fn checkpoint_of(db: &Db, interval_id: &str) -> Option<CheckpointFacts> {
    use rusqlite::OptionalExtension;
    db.connection()
        .query_row(
            "SELECT interval_id, run_id, wall_at, attribution_at
               FROM interval_checkpoint WHERE interval_id = ?1",
            [interval_id],
            |r| {
                Ok(CheckpointFacts {
                    interval_id: r.get(0)?,
                    run_id: r.get(1)?,
                    wall_at: r.get(2)?,
                    attribution_at: r.get(3)?,
                })
            },
        )
        .optional()
        .unwrap()
}

fn world(db: &Db) -> WorldFacts {
    let conn = db.connection();
    let mut sessions_stmt = conn
        .prepare(
            "SELECT id, run_id, state, started_at, ended_at, needs_review, row_version
               FROM work_session ORDER BY id",
        )
        .unwrap();
    let sessions = sessions_stmt
        .query_map([], |r| {
            Ok(SessionFacts {
                id: r.get(0)?,
                run_id: r.get(1)?,
                state: r.get(2)?,
                started_at: r.get(3)?,
                ended_at: r.get(4)?,
                needs_review: r.get(5)?,
                row_version: r.get(6)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    let mut checkpoints_stmt = conn
        .prepare(
            "SELECT interval_id, run_id, wall_at, attribution_at
               FROM interval_checkpoint ORDER BY interval_id",
        )
        .unwrap();
    let checkpoints = checkpoints_stmt
        .query_map([], |r| {
            Ok(CheckpointFacts {
                interval_id: r.get(0)?,
                run_id: r.get(1)?,
                wall_at: r.get(2)?,
                attribution_at: r.get(3)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    let mut audits_stmt = conn
        .prepare("SELECT id, session_id, reason FROM time_edit ORDER BY id")
        .unwrap();
    let audits = audits_stmt
        .query_map([], |r| {
            Ok(AuditFacts {
                id: r.get(0)?,
                session_id: r.get(1)?,
                reason: r.get(2)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    WorldFacts {
        revision: revision(db),
        sessions,
        intervals: intervals_where(db, "1 = 1", []),
        checkpoints,
        audits,
        task: task_facts(db),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 「统计可读」的口径：已确认 = 未作废 + 非待确认 + 有 duration_ms
// ─────────────────────────────────────────────────────────────────────────────

/// 统计可读的**已确认**区间。
///
/// 口径就是 `IntervalFacts::is_trusted_closed()`：`duration_ms IS NOT NULL`、
/// `needs_review = 0`、`voided_at IS NULL`。两条排除都是要害：
/// - 待确认段（`duration_ms IS NULL`）**不是已确认数字**；
/// - 正在计时会话的开放区间（`duration_ms IS NULL`）同样只是「暂计」。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfirmedRow {
    id: String,
    session_id: String,
    range: IntervalRange,
}

fn confirmed_rows(db: &Db) -> Vec<ConfirmedRow> {
    intervals_where(
        db,
        "i.voided_at IS NULL AND i.needs_review = 0 AND i.duration_ms IS NOT NULL",
        [],
    )
    .into_iter()
    .map(|i| ConfirmedRow {
        id: i.id,
        session_id: i.session_id,
        range: IntervalRange {
            start: i.started_at,
            end: i.ended_at.expect("已确认区间必须有终点"),
        },
    })
    .collect()
}

/// 已确认区间构成的**互不重叠**集合 + 总时长。
///
/// 用 `IntervalSet::insert` 判重叠（P3 的相交判定唯一入口）：任何两段相交都会
/// `Err`，所以「统计可读的区间集合始终非重叠」这句话在这里是一条**真的断言**，
/// 而不是一句注释。
fn confirmed_set(db: &Db) -> (Vec<ConfirmedRow>, IntervalSet) {
    let rows = confirmed_rows(db);
    let mut set = IntervalSet::new();
    for row in &rows {
        set.insert(row.range)
            .unwrap_or_else(|e| panic!("已确认区间必须两两不重叠，但 {} 撞上了：{e:?}", row.id));
    }
    (rows, set)
}

/// 断言「已确认数字」正好是这几段、且**同一段时间只被算一次**。
fn assert_confirmed(db: &Db, expected_total_ms: i64, expected_count: usize) {
    let (rows, set) = confirmed_set(db);
    assert_eq!(
        set.items().len(),
        expected_count,
        "已确认区间条数不对：{rows:?}"
    );
    assert_eq!(
        set.total_ms(),
        expected_total_ms,
        "已确认总时长不对（要么漏算、要么同一段时间被算了两遍）：{rows:?}"
    );
    assert_eq!(
        set.total_ms(),
        rows.iter().map(|r| r.range.duration_ms()).sum::<i64>(),
        "互不重叠 ⇒ 求和即并集长度"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 主链路
// ─────────────────────────────────────────────────────────────────────────────

/// 崩溃 → 重启扫描 → 对账确认 → 门禁放开 → 计时可用 → 修正 / 补录 / 作废整次。
///
/// 全程断言：统计可读集合非重叠且不重复计数、每个用户命令恰好 +1 `revision`、
/// 被拒命令逐字段零变化、幂等重复零变化。
#[test]
fn a_crash_restart_recovery_and_history_chain_stays_single_counted() {
    let fx = fixture();
    let db = seeded(&fx);
    let seeded_revision = revision(&db);
    assert_eq!(seeded_revision, 0, "新库的初始 revision 是 0");
    drop(db);

    // ① 重启：第 ④ 步的扫描做四类判定并落地事实。
    let running = started(&fx);
    let epoch = running.data_epoch().to_string();
    let current_run = running.run_id().to_string();
    let mut state = lock_app(running.app());

    // 扫描事务是**整批一次**的版本推进（与用户命令分开计）。
    assert_eq!(
        revision(state.db()),
        seeded_revision + 1,
        "扫描事务整批只加一次 revision"
    );
    assert_eq!(
        world(state.db()).audits.len(),
        2,
        "扫描留下两条审计：归一崩溃段 + 重绑第 4 类"
    );

    // 第 2 类：可信前缀闭合、待确认段终点未知；会话 recovering，run_id **仍是旧 run**。
    let r = read_session(state.db(), "r");
    assert_eq!(r.state, "recovering");
    assert_eq!(r.run_id, OLD_RUN, "恢复归属要等 reconcile 才切到本次 run");
    assert_eq!(r.needs_review, 1);
    assert_eq!(r.row_version, 1, "扫描改过一次状态 ⇒ 版本 +1");

    let prefix = read_interval(state.db(), "r-prefix");
    assert_eq!(prefix.ended_at, Some(PREFIX_END), "可信前缀在检查点处闭合");
    assert_eq!(
        prefix.duration_ms,
        Some(PREFIX_END - T0),
        "可信前缀的时长 = 检查点归属时刻 - started_at（不是「到崩溃时刻」那段猜测）"
    );
    assert_eq!(
        prefix.sampled_end_wall_at,
        Some(CHECKPOINT_WALL),
        "检查点记下的挂钟值原样保留（证据不被改写）"
    );
    let pending_id = pending_interval_id(state.db(), "r");
    let pending = read_interval(state.db(), &pending_id);
    assert_eq!(pending.started_at, PREFIX_END);
    assert_eq!(
        pending.ended_at,
        Some(PREFIX_END),
        "候选端点是零长度，不是事实"
    );
    assert_eq!(
        pending.duration_ms, None,
        "待确认段没有时长 ⇒ 不得计入任何「已确认」数字"
    );
    assert_eq!(pending.needs_review, 1);

    // 第 4 类：paused 会话保持 paused，run_id 重绑本次 run。
    let p = read_session(state.db(), "p");
    assert_eq!(p.state, "paused");
    assert_eq!(p.run_id, current_run, "第 4 类重绑到本次 run");

    // 门禁关着（旧 run 的 recovering 会话 + 待确认段），但已确认集合只有两段、
    // 互不重叠：可信前缀 20 秒 + 旧 paused 会话的 2 秒。
    assert!(state.recovery().requires_recovery(), "门禁必须关着");
    assert_eq!(
        state.recovery().unfinished_sessions,
        vec!["r".to_string()],
        "第 4 类重绑过的会话不再算「别的 run 的残留」"
    );
    assert_eq!(state.recovery().pending_intervals, vec![pending_id.clone()]);
    assert_confirmed(state.db(), (PREFIX_END - T0) + (P_END - P_START), 2);

    // ② 门禁关着时 start 被拒，且**逐字段零变化**。
    let before = world(state.db());
    let task_v = task_version(state.db());
    let err = state
        .start(start_request(&epoch, task_v))
        .expect_err("门禁关着时不得开始新计时");
    assert_code(&err, "RECOVERY_REQUIRED");
    assert_eq!(
        world(state.db()),
        before,
        "被门禁拒绝的命令不得写入任何东西（不采样、不建会话、不加 revision）"
    );

    // ③ 对账确认：一次事务处理该会话的全部待确认区间，会话推到 finished。
    let (report, changed) = state
        .reconcile(
            WriteEnvelope::for_update(&epoch, r.row_version),
            ReconcileRequest {
                session_id: "r".into(),
                action: ReconcileAction::Confirm,
                target_state: ReconcileTargetState::Finished,
                ranges: vec![ConfirmedRange {
                    interval_id: pending_id.clone(),
                    started_at: PREFIX_END,
                    ended_at: CONFIRMED_END,
                }],
            },
        )
        .expect("对账确认应当成功")
        .into_parts();
    assert!(changed, "对账确认必然是一次真实写入");
    assert_eq!(
        revision(state.db()),
        seeded_revision + 2,
        "一条用户命令恰好一次 revision"
    );
    assert_eq!(
        report.revision,
        revision(state.db()),
        "报告里的版本是权威值"
    );

    let r = read_session(state.db(), "r");
    assert_eq!(r.state, "finished");
    assert_eq!(r.run_id, current_run, "恢复归属在提交时切到本次 run");
    assert_eq!(r.needs_review, 0);
    assert_eq!(r.ended_at, Some(CONFIRMED_END), "用用户给的终点收尾");

    let confirmed_segment = read_interval(state.db(), &pending_id);
    assert_eq!(confirmed_segment.started_at, PREFIX_END);
    assert_eq!(confirmed_segment.ended_at, Some(CONFIRMED_END));
    assert_eq!(
        confirmed_segment.duration_ms,
        Some(CONFIRMED_END - PREFIX_END)
    );
    assert_eq!(confirmed_segment.needs_review, 0);

    // 崩溃那一段 [T0, CONFIRMED_END) 正好被算一次：20 秒前缀 + 10 秒确认段，
    // 而不是「前缀 + 候选到停机时刻」那种重复计数。
    assert_confirmed(state.db(), (CONFIRMED_END - T0) + (P_END - P_START), 3);

    // ④ 门禁放开，`start` 成功（闭环）。
    assert!(
        !state.recovery().requires_recovery(),
        "对账提交后仍持锁重扫 ⇒ 门禁必须放开"
    );
    let task_v = task_version(state.db());
    let started_outcome = state
        .start(start_request(&epoch, task_v))
        .expect("门禁放开后 start 必须成功");
    assert_eq!(revision(state.db()), seeded_revision + 3, "start 恰好 +1");
    let s_id = started_outcome
        .snapshot
        .session_id
        .clone()
        .expect("start 之后快照必须指向新会话");
    let s = read_session(state.db(), &s_id);
    assert_eq!(s.run_id, current_run);
    assert_eq!(s.state, "running");
    // 新开的开放区间没有时长 ⇒ 已确认数字一点都没变（不会把「正在计时」当成已确认）。
    assert_confirmed(state.db(), (CONFIRMED_END - T0) + (P_END - P_START), 3);

    // ⑤ 结束这个本 run 的会话（0 长度区间：假时钟不走）。
    let finished = state
        .finish(SessionRequest {
            expected_data_epoch: epoch.clone(),
            session_id: s_id.clone(),
            session_expected_version: s.row_version,
        })
        .expect("结束本 run 的会话");
    assert_eq!(
        finished.revision,
        seeded_revision + 4,
        "finish 恰好 +1（计时命令的版本在 CommandOutcome 里）"
    );
    assert_eq!(read_session(state.db(), &s_id).state, "finished");
    assert_confirmed(state.db(), (CONFIRMED_END - T0) + (P_END - P_START), 4);

    // ⑥ 恢复后的 `resume`：把上一代留下的暂停会话绑到**本次 run**，并开一条新区间。
    let p = read_session(state.db(), "p");
    let task_v = task_version(state.db());
    let resumed = state
        .resume(ResumeRequest {
            expected_data_epoch: epoch.clone(),
            task_id: "t1".into(),
            task_expected_version: task_v,
            session_id: "p".into(),
            session_expected_version: p.row_version,
        })
        .expect("暂停会话可以继续");
    assert_eq!(resumed.revision, seeded_revision + 5, "resume 恰好 +1");
    let p = read_session(state.db(), "p");
    assert_eq!(p.state, "running");
    assert_eq!(
        p.run_id, current_run,
        "恢复之后会话必须属于本次 run（run_id 切到当前 run）"
    );
    assert_eq!(
        resumed.snapshot.session_id.as_deref(),
        Some("p"),
        "快照指向刚继续的会话"
    );
    // 新开的区间与它的检查点都挂在本次 run 上。
    let p_open = intervals_of(state.db(), "p")
        .into_iter()
        .find(|i| i.ended_at.is_none() && i.voided_at.is_none())
        .expect("resume 必须开一条新区间");
    assert_eq!(p_open.started_at, WALL, "归属时刻来自当前采样");
    let cp = checkpoint_of(state.db(), &p_open.id).expect("resume 必须写检查点");
    assert_eq!(cp.run_id, current_run, "检查点属于本次 run");
    assert_eq!(cp.attribution_at, WALL);
    // 它还没有时长 ⇒ 已确认数字仍然不变（同一段时间不会被算两遍）。
    assert_confirmed(state.db(), (CONFIRMED_END - T0) + (P_END - P_START), 4);

    // ⑦ 修正可信历史：把可信前缀收短 1 秒（`correct` 只接 finished 会话）。
    let r = read_session(state.db(), "r");
    let (edited, changed) = state
        .correct(
            WriteEnvelope::for_update(&epoch, r.row_version),
            CorrectRequest {
                session_id: "r".into(),
                interval_id: "r-prefix".into(),
                action: CorrectAction::Retime {
                    started_at: T0,
                    ended_at: CORRECT_END,
                },
                reason: Some("端到端：把可信前缀收短一点".into()),
            },
        )
        .expect("修正可信历史应当成功")
        .into_parts();
    assert!(changed, "重定时必然是一次真实写入");
    assert_eq!(revision(state.db()), seeded_revision + 6, "correct 恰好 +1");
    assert_eq!(edited.revision, revision(state.db()));
    assert_eq!(edited.interval.ended_at, Some(CORRECT_END));
    assert_eq!(
        edited.interval.duration_ms,
        Some(CORRECT_END - T0),
        "时长与新的起止一致"
    );
    // 被修正的那一段只剩 19 秒；后一段 10 秒不受影响 ⇒ 总长少 1 秒，且仍不重叠。
    assert_confirmed(
        state.db(),
        (CORRECT_END - T0) + (CONFIRMED_END - PREFIX_END) + (P_END - P_START),
        4,
    );
    assert_eq!(
        read_interval(state.db(), &pending_id).duration_ms,
        Some(CONFIRMED_END - PREFIX_END),
        "修正一段不能动到另一段"
    );

    // ⑧ 手工补录：新建的是一条终态会话 + 一条可信闭合区间（不占前台槽位）。
    let (backfilled, changed) = state
        .backfill(
            WriteEnvelope::for_create(&epoch),
            BackfillRequest {
                task_id: "t1".into(),
                started_at: BF_START,
                ended_at: BF_END,
            },
        )
        .expect("补录应当成功")
        .into_parts();
    assert!(changed, "补录必然是一次真实写入");
    assert_eq!(
        revision(state.db()),
        seeded_revision + 7,
        "backfill 恰好 +1"
    );
    let bf_session = backfilled.session.id.clone();
    assert_eq!(read_session(state.db(), &bf_session).state, "finished");
    assert_eq!(backfilled.interval.duration_ms, Some(BF_END - BF_START));
    assert_confirmed(
        state.db(),
        (CORRECT_END - T0) + (CONFIRMED_END - PREFIX_END) + (P_END - P_START) + (BF_END - BF_START),
        5,
    );

    // ⑨ 作废整次：整个崩溃会话的两段区间一起作废，补录与暂停会话的事实不受影响。
    let r = read_session(state.db(), "r");
    let (discarded, changed) = state
        .discard_session(
            WriteEnvelope::for_update(&epoch, r.row_version),
            DiscardSessionRequest {
                session_id: "r".into(),
            },
        )
        .expect("作废整次应当成功")
        .into_parts();
    assert!(changed, "作废整次必然是一次真实写入");
    assert_eq!(
        revision(state.db()),
        seeded_revision + 8,
        "discard_session 恰好 +1"
    );
    assert_eq!(discarded.session.state, SessionState::Discarded);
    let r = read_session(state.db(), "r");
    assert_eq!(r.state, "discarded");
    assert_eq!(r.needs_review, 0);
    for interval in intervals_of(state.db(), "r") {
        assert_eq!(interval.voided_at, Some(WALL), "{} 应当被作废", interval.id);
        assert_eq!(interval.needs_review, 0, "作废与待确认互斥");
    }
    let retimed = read_interval(state.db(), "r-prefix");
    assert_eq!(
        (retimed.ended_at, retimed.duration_ms),
        (Some(CORRECT_END), Some(CORRECT_END - T0)),
        "有时长的区间作废后保留原起止（作废不是「没发生过」）"
    );
    // 作废之后它不再进「已确认」数字：只剩补录的 2 秒与两条零长度区间。
    assert_confirmed(state.db(), (BF_END - BF_START) + (P_END - P_START), 3);
    assert!(
        !state.recovery().requires_recovery(),
        "作废整次提交后重扫 ⇒ 门禁仍然开着"
    );

    // ⑩ 幂等：重复作废零写入、零版本、零审计（`time_edit` 一条都不多）。
    let r = read_session(state.db(), "r");
    let before = world(state.db());
    let (_, changed) = state
        .discard_session(
            WriteEnvelope::for_update(&epoch, r.row_version),
            DiscardSessionRequest {
                session_id: "r".into(),
            },
        )
        .expect("重复作废是幂等，不是失败")
        .into_parts();
    assert!(!changed, "重复作废的终态没有任何字段要改");
    assert_eq!(
        world(state.db()),
        before,
        "幂等重复必须逐字段零变化（含 revision 与审计）"
    );
}

/// `resume` 自身把会话的 `run_id` 切到**当前 run**（`coordinator.rs:857`）。
///
/// 为什么要在启动之后直写一行 `paused` + 旧 `run_id`：启动扫描的第 4 类会把
/// 「无待确认的暂停会话」重绑到本次 run，所以这个形态在**扫描之后**不会再自然出现。
/// 但 `resume` 里那条 `run_id` 写入是它自己的契约（P6 的恢复/替换库路径与
/// P2 的跨 run 防御都依赖它），不能只靠「扫描已经替我们绑好了」。这里把扫描绕开，
/// 直接钉住 `resume` 的那一次写入。
#[test]
fn resuming_a_paused_session_of_a_previous_run_rebinds_it_to_the_current_run() {
    let fx = fixture();
    let db = seeded_task(&fx);
    drop(db);

    let running = started(&fx);
    let epoch = running.data_epoch().to_string();
    let current_run = running.run_id().to_string();
    let mut state = lock_app(running.app());
    let revision_after_startup = revision(state.db());
    assert!(
        !state.recovery().requires_recovery(),
        "这个起点没有任何恢复材料"
    );

    // 直写：一个属于**上一个 run** 的暂停会话（没有待确认事实，也没有开放区间）。
    // 启动扫描已经跑过，所以这一次不会有人替 `resume` 重绑。
    let legacy_start = WALL - 5_000;
    let legacy_end = WALL - 4_000;
    {
        let conn = state.db().connection();
        conn.execute(
            "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,ended_at,row_version)
             VALUES('legacy','t1','run-old','FOREGROUND','paused','stopwatch',?1,?2,0)",
            rusqlite::params![legacy_start, legacy_end],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms,needs_review)
             VALUES('legacy-1','legacy',?1,?2,?3,0)",
            rusqlite::params![legacy_start, legacy_end, legacy_end - legacy_start],
        )
        .unwrap();
    }
    let legacy = read_session(state.db(), "legacy");
    assert_eq!(legacy.run_id, OLD_RUN, "现场就是「上一代 run 的暂停会话」");

    let task_v = task_version(state.db());
    let outcome = state
        .resume(ResumeRequest {
            expected_data_epoch: epoch.clone(),
            task_id: "t1".into(),
            task_expected_version: task_v,
            session_id: "legacy".into(),
            session_expected_version: legacy.row_version,
        })
        .expect("暂停会话可以继续");
    assert_eq!(
        outcome.revision,
        revision_after_startup + 1,
        "resume 恰好 +1"
    );
    let legacy = read_session(state.db(), "legacy");
    assert_eq!(legacy.state, "running");
    assert_eq!(
        legacy.run_id, current_run,
        "resume 必须把会话绑到当前 run（`coordinator.rs:857` 那一次 update_session_state）"
    );
    assert_eq!(
        outcome.snapshot.session_id.as_deref(),
        Some("legacy"),
        "快照指向刚继续的会话"
    );

    // 新开的区间与检查点都属于当前 run——否则心跳/结束会撞上跨 run 判据。
    let open = intervals_of(state.db(), "legacy")
        .into_iter()
        .find(|i| i.ended_at.is_none() && i.voided_at.is_none())
        .expect("resume 必须开一条新区间");
    assert_eq!(open.started_at, WALL);
    let cp = checkpoint_of(state.db(), &open.id).expect("resume 必须写检查点");
    assert_eq!(cp.run_id, current_run);
    assert_eq!(cp.attribution_at, WALL);

    // 老的那段可信历史原样保留，仍然计入且只计一次（这条用例的库里只有它一段已确认）。
    let kept = read_interval(state.db(), "legacy-1");
    assert_eq!(kept.voided_at, None);
    assert_eq!(kept.duration_ms, Some(legacy_end - legacy_start));
    assert_confirmed(state.db(), legacy_end - legacy_start, 1);
}
