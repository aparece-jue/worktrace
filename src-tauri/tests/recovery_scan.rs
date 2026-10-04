//! P3 Task 1：启动恢复扫描（四类判定）与共享恢复原语。
//!
//! 装置抄 `tests/startup_order.rs::fixture` 与 `tests/ipc_commands.rs` 的
//! startup + guard 写法：手工造出「上一个 run 崩过」的库，再走**真实**的
//! `bootstrap::startup`，并在同一把锁（`lock_app`）里核对事实。
//!
//! 判定顺序不可颠倒（02 §4 表）：先看不变量是否可信，再看有无开放/待确认区间，
//! 最后看当前状态：
//!
//! 1. 不变量损坏 → **只诊断**（`faults`），不写任何事实、不加版本；
//! 2. `running` + 开放区间 → 可信前缀闭合 + 终点未知的待确认段，会话转 `recovering`；
//! 3. `recovering` → 原样保持（含 P2 留下的 `ended_at IS NULL` 待确认段）；
//! 4. `paused` 且无开放/待确认区间 → 保持 `paused`，`run_id` 重绑当前 run。
//!
//! 版本口径（02 文末「启动扫描的版本与审计补充」）：整批扫描事务**恰好一次**
//! revision（只在真有字段变化时），每个被改的会话各一条 `time_edit`。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use worktrace_lib::domain::session::{SessionAttention, SessionState};
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, scan_recovery, startup, RunningApp, Startup, StartupConfig, StartupProbe, StartupStep,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::recovery::scan_at_startup;
use worktrace_lib::services::timer::coordinator::SessionRequest;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::run_repo;
use worktrace_lib::storage::session_repo::{self, InvariantFault};

/// 启动那一刻的挂钟（`FakeClock` 不自己走）。
const WALL: i64 = 1_700_000_000_000;
/// 崩掉的那一代次。
const OLD_RUN: &str = "run-old";
/// 「本次 run」——直接调扫描的用例自己指定；走 `startup` 的用例由启动第 ③ 步创建。
const NOW_RUN: &str = "run-now";

// ─────────────────────────────────────────────────────────────────────────────
// 装置
// ─────────────────────────────────────────────────────────────────────────────

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

#[derive(Default)]
struct RecordingProbe {
    steps: Mutex<Vec<StartupStep>>,
}

impl StartupProbe for RecordingProbe {
    fn step(&self, step: StartupStep) {
        self.steps.lock().unwrap().push(step);
    }
}

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

/// 真跑一次启动：第 ④ 步就是被测的恢复扫描。
fn started(fx: &Fixture) -> Box<RunningApp> {
    let probe = RecordingProbe::default();
    let sink = Arc::new(RecordingSink::default());
    let outcome = startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path),
        Box::new(FakeClock::new(WALL, 0)),
        sink,
        &probe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功");
    match outcome {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    }
}

/// 建一个「上一个 run 崩过」的库：迁移 + 元数据 + 两代 run + 一条任务。
///
/// 两代 run 都先建好，`application_run.id` 是 `work_session.run_id` 的外键目标。
fn seeded(fx: &Fixture) -> Db {
    let mut db = Db::open(&fx.db_path).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    run_repo::start_run(&tx, OLD_RUN, 500).unwrap();
    run_repo::start_run(&tx, NOW_RUN, 600).unwrap();
    tx.execute(
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Doing',0,1000,1000)",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    db
}

// ── 造事实：四类判定要的正是**手工写进去的历史**（服务层不会产生损坏行） ──

fn insert_session(
    db: &Db,
    id: &str,
    run: &str,
    mode: &str,
    state: &str,
    started_at: i64,
    needs_review: i64,
) {
    db.connection()
        .execute(
            "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,
                                      needs_review,row_version)
             VALUES(?1,'t1',?2,?3,?4,'stopwatch',?5,?6,0)",
            rusqlite::params![id, run, mode, state, started_at, needs_review],
        )
        .unwrap();
}

fn insert_interval(
    db: &Db,
    id: &str,
    session_id: &str,
    started_at: i64,
    ended_at: Option<i64>,
    duration_ms: Option<i64>,
    needs_review: i64,
) {
    db.connection()
        .execute(
            "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms,needs_review)
             VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![
                id,
                session_id,
                started_at,
                ended_at,
                duration_ms,
                needs_review
            ],
        )
        .unwrap();
}

fn insert_checkpoint(
    db: &Db,
    interval_id: &str,
    run: &str,
    wall_at: i64,
    attribution_at: i64,
    elapsed_ms: i64,
) {
    db.connection()
        .execute(
            "INSERT INTO interval_checkpoint(interval_id,run_id,wall_at,attribution_at,elapsed_ms)
             VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![interval_id, run, wall_at, attribution_at, elapsed_ms],
        )
        .unwrap();
}

// ── 读事实 ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionFacts {
    state: String,
    run_id: String,
    needs_review: i64,
    row_version: i64,
    ended_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IntervalFacts {
    session_id: String,
    started_at: i64,
    ended_at: Option<i64>,
    duration_ms: Option<i64>,
    sampled_end_wall_at: Option<i64>,
    needs_review: i64,
    voided_at: Option<i64>,
}

fn session(db: &Db, id: &str) -> SessionFacts {
    db.connection()
        .query_row(
            "SELECT state, run_id, needs_review, row_version, ended_at
               FROM work_session WHERE id = ?1",
            [id],
            |r| {
                Ok(SessionFacts {
                    state: r.get(0)?,
                    run_id: r.get(1)?,
                    needs_review: r.get(2)?,
                    row_version: r.get(3)?,
                    ended_at: r.get(4)?,
                })
            },
        )
        .unwrap()
}

fn interval(db: &Db, id: &str) -> IntervalFacts {
    db.connection()
        .query_row(
            "SELECT session_id, started_at, ended_at, duration_ms, sampled_end_wall_at,
                    needs_review, voided_at
               FROM work_interval WHERE id = ?1",
            [id],
            |r| {
                Ok(IntervalFacts {
                    session_id: r.get(0)?,
                    started_at: r.get(1)?,
                    ended_at: r.get(2)?,
                    duration_ms: r.get(3)?,
                    sampled_end_wall_at: r.get(4)?,
                    needs_review: r.get(5)?,
                    voided_at: r.get(6)?,
                })
            },
        )
        .unwrap()
}

/// 某会话的待确认区间 id（升序）。
fn pending_ids(db: &Db, session_id: &str) -> Vec<String> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare(
            "SELECT id FROM work_interval
              WHERE session_id = ?1 AND needs_review = 1 AND voided_at IS NULL
              ORDER BY started_at, id",
        )
        .unwrap();
    let rows = stmt
        .query_map([session_id], |r| r.get::<_, String>(0))
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

fn interval_count(db: &Db, session_id: &str) -> i64 {
    db.connection()
        .query_row(
            "SELECT COUNT(*) FROM work_interval WHERE session_id = ?1",
            [session_id],
            |r| r.get(0),
        )
        .unwrap()
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

fn total_changes(db: &Db) -> i64 {
    db.connection()
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap()
}

fn time_edit_count(db: &Db) -> i64 {
    db.connection()
        .query_row("SELECT COUNT(*) FROM time_edit", [], |r| r.get(0))
        .unwrap()
}

/// 某会话的（before_json, after_json, reason）。
fn time_edit_of(db: &Db, session_id: &str) -> (String, String, Option<String>) {
    db.connection()
        .query_row(
            "SELECT before_json, after_json, reason FROM time_edit WHERE session_id = ?1",
            [session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

/// 全表「孤儿开放区间」：会话不是 `running`，却留着未作废的开放区间（C3 的自造损坏）。
///
/// `recovering` + `needs_review=1` 的开放段是**合法形态**（08 §1：没有可信检查点则
/// 整个当前区间待确认），所以单独用 [`orphan_open_intervals`] 计这一条。
fn orphan_open_intervals(db: &Db) -> i64 {
    db.connection()
        .query_row(
            "SELECT COUNT(*) FROM work_interval i JOIN work_session s ON s.id = i.session_id
              WHERE i.ended_at IS NULL AND i.voided_at IS NULL AND s.state <> 'running'
                AND NOT (s.state = 'recovering' AND i.needs_review = 1)",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// `recovering` 会话的「终点未知」开放段条数（合法，见 [`orphan_open_intervals`]）。
fn unknown_end_open_intervals(db: &Db) -> i64 {
    db.connection()
        .query_row(
            "SELECT COUNT(*) FROM work_interval i JOIN work_session s ON s.id = i.session_id
              WHERE i.ended_at IS NULL AND i.voided_at IS NULL
                AND s.state = 'recovering' AND i.needs_review = 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

// ─────────────────────────────────────────────────────────────────────────────
// 第 2 类：崩溃区间 → 可信前缀 + 终点未知的待确认段
// ─────────────────────────────────────────────────────────────────────────────

/// 有检查点 ⇒ 原区间成为**可信前缀**，另起一段零长度候选。
#[test]
fn a_crashed_open_interval_becomes_a_trusted_prefix_plus_an_unknown_pending_tail() {
    let fx = fixture();
    let db = seeded(&fx);
    insert_session(&db, "s-crash", OLD_RUN, "FOREGROUND", "running", 1000, 0);
    insert_interval(&db, "iv-crash", "s-crash", 1000, None, None, 0);
    insert_checkpoint(&db, "iv-crash", OLD_RUN, 1400, 1350, 350);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());

    {
        let db = state.db();
        // 可信前缀：原区间在最后成功检查点处闭合（id 不变）。
        let prefix = interval(db, "iv-crash");
        assert_eq!(prefix.ended_at, Some(1350), "闭合点 = 检查点的归属时刻");
        assert_eq!(prefix.duration_ms, Some(350), "时长按归属时刻算");
        assert_eq!(
            prefix.sampled_end_wall_at,
            Some(1400),
            "前缀的采样挂钟来自检查点"
        );
        assert_eq!(prefix.needs_review, 0, "有可信前缀就是已确认事实");
        assert_eq!(prefix.voided_at, None);

        // 待确认段：零长度候选，五个字段逐个钉住。
        let tails = pending_ids(db, "s-crash");
        assert_eq!(tails.len(), 1, "恰好一段待确认余段");
        assert_ne!(tails[0], "iv-crash", "余段是新的一行，不覆盖可信前缀");
        let tail = interval(db, &tails[0]);
        assert_eq!(tail.session_id, "s-crash");
        assert_eq!(tail.started_at, 1350, "从可信前缀的界开始");
        assert_eq!(tail.ended_at, Some(1350), "候选端点，不是事实");
        assert_eq!(tail.duration_ms, None, "未确认就没有时长");
        assert_eq!(tail.sampled_end_wall_at, None, "没有这一拍的墙钟样本");
        assert_eq!(tail.needs_review, 1);
        assert_eq!(tail.voided_at, None);

        // 会话：recovering + needs_review，**run_id 不变**（02 §10）。
        let s = session(db, "s-crash");
        assert_eq!(s.state, "recovering");
        assert_eq!(s.needs_review, 1);
        assert_eq!(s.run_id, OLD_RUN, "recovering 保持原恢复归属");
        assert_eq!(s.row_version, 1, "被改的会话恰好 +1");

        // 全表：绝不留「非 running 会话 + 未作废开放区间」。
        assert_eq!(orphan_open_intervals(db), 0, "不得留下孤儿开放区间");
        assert_eq!(
            unknown_end_open_intervals(db),
            0,
            "这一批没有 recovering 的终点未知段"
        );
    }

    // 再扫一次：无字段变化 ⇒ 不加 revision、不写审计、不加版本。
    let before = {
        let db = state.db();
        (
            revision(db),
            total_changes(db),
            time_edit_count(db),
            session(db, "s-crash").row_version,
        )
    };
    let report = scan_at_startup(state.db_mut(), running.run_id(), WALL).unwrap();
    assert!(!report.revision_changed, "重复扫描没有变化");
    assert!(report.normalized_sessions.is_empty());
    assert!(report.rebound_sessions.is_empty());
    assert_eq!(
        report.recovering_kept,
        ["s-crash"],
        "归一后的会话第二次落进第 3 类（原样保持）"
    );
    assert!(report.faults.is_empty());

    let db = state.db();
    assert_eq!(revision(db), before.0, "无变化不得加 revision");
    assert_eq!(total_changes(db), before.1, "无变化不得写任何行");
    assert_eq!(time_edit_count(db), before.2, "无变化不得写审计");
    assert_eq!(session(db, "s-crash").row_version, before.3);
}

/// 没有检查点（或检查点没推进过）⇒ 整段待确认，`ended_at` 收在起点上。
#[test]
fn a_crashed_open_interval_without_a_checkpoint_becomes_one_pending_interval() {
    let fx = fixture();
    let db = seeded(&fx);
    insert_session(&db, "s-crash", OLD_RUN, "FOREGROUND", "running", 1000, 0);
    insert_interval(&db, "iv-crash", "s-crash", 1000, None, None, 0);
    // 检查点的归属时刻没越过区间起点：不算可信前缀。
    insert_checkpoint(&db, "iv-crash", OLD_RUN, 900, 1000, 0);
    drop(db);

    let running = started(&fx);
    let state = lock_app(running.app());
    let db = state.db();

    assert_eq!(
        pending_ids(db, "s-crash"),
        ["iv-crash"],
        "整段待确认：原区间自己变成待确认段"
    );
    let whole = interval(db, "iv-crash");
    assert_eq!(whole.started_at, 1000);
    assert_eq!(whole.ended_at, Some(1000), "终点收在起点上（候选）");
    assert_eq!(whole.duration_ms, None);
    assert_eq!(
        whole.sampled_end_wall_at, None,
        "没有可信前缀就没有检查点挂钟"
    );
    assert_eq!(whole.needs_review, 1);
    assert_eq!(session(db, "s-crash").state, "recovering");
    assert_eq!(orphan_open_intervals(db), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// 四类判定各就其位
// ─────────────────────────────────────────────────────────────────────────────

/// 四类同时在场：各自的状态/版本/审计互不串味，门禁快照跟着事实走。
#[test]
fn the_startup_scan_keeps_the_four_classes_of_old_record_apart() {
    let fx = fixture();
    let db = seeded(&fx);
    // 第 2 类：崩溃在运行中（有可信检查点）。
    insert_session(&db, "s-crash", OLD_RUN, "FOREGROUND", "running", 1000, 0);
    insert_interval(&db, "iv-crash", "s-crash", 1000, None, None, 0);
    insert_checkpoint(&db, "iv-crash", OLD_RUN, 1400, 1350, 350);
    // 第 3 类：P2 留下的「终点未知」恢复记录（开放 + 待确认，是**合法**形态）。
    insert_session(&db, "s-rec", OLD_RUN, "BACKGROUND", "recovering", 1200, 1);
    insert_interval(&db, "iv-rec", "s-rec", 1200, None, None, 1);
    // 第 4 类：干净暂停（没有开放/待确认区间）。
    insert_session(&db, "s-pause", OLD_RUN, "BACKGROUND", "paused", 1300, 0);
    insert_interval(&db, "iv-pause", "s-pause", 1300, Some(1600), Some(300), 0);
    // 第 1 类：running 却没有开放区间（判据之间不一致，只能诊断）。
    insert_session(&db, "s-fault", OLD_RUN, "BACKGROUND", "running", 1500, 0);
    drop(db);

    let running = started(&fx);
    let state = lock_app(running.app());
    let db = state.db();

    // 第 1 类：只诊断，不写事实、不加版本。
    assert_eq!(
        session(db, "s-fault"),
        SessionFacts {
            state: "running".into(),
            run_id: OLD_RUN.into(),
            needs_review: 0,
            row_version: 0,
            ended_at: None,
        },
        "损坏记录一个字都不许被自动修复"
    );
    assert_eq!(interval_count(db, "s-fault"), 0, "不得替它造区间");

    // 第 2 类：归一 + 转 recovering。
    assert_eq!(session(db, "s-crash").state, "recovering");
    assert_eq!(session(db, "s-crash").row_version, 1);
    assert_eq!(interval(db, "iv-crash").ended_at, Some(1350));

    // 第 3 类：原样保持（含 P2 的终点未知段），row_version 不动。
    assert_eq!(
        session(db, "s-rec"),
        SessionFacts {
            state: "recovering".into(),
            run_id: OLD_RUN.into(),
            needs_review: 1,
            row_version: 0,
            ended_at: None,
        },
        "recovering 不重复计入、不加工时"
    );
    assert_eq!(interval(db, "iv-rec").ended_at, None, "终点未知段原样保留");
    assert_eq!(interval(db, "iv-rec").needs_review, 1);

    // 第 4 类：保持 paused，run_id 重绑当前 run（**不自动继续计时**）。
    let pause = session(db, "s-pause");
    assert_eq!(pause.state, "paused", "扫描不自动继续计时");
    assert_eq!(pause.run_id, running.run_id(), "重绑到本次 run");
    assert_eq!(pause.row_version, 1);
    assert_eq!(pause.ended_at, None);
    assert_eq!(
        interval(db, "iv-pause").duration_ms,
        Some(300),
        "历史时长不动"
    );

    // 版本与审计：整批**恰好一次** revision，每个被改的会话各一条审计。
    assert_eq!(revision(db), 1, "一批扫描事务恰好一次 revision");
    assert_eq!(time_edit_count(db), 2, "只有两个会话真的变了");
    assert!(
        time_edit_of_optional(db, "s-fault").is_none(),
        "隔离的损坏记录不写事实、也不写审计"
    );

    let (before_json, after_json, reason) = time_edit_of(db, "s-crash");
    let before: serde_json::Value = serde_json::from_str(&before_json).unwrap();
    let after: serde_json::Value = serde_json::from_str(&after_json).unwrap();
    assert_eq!(before["change"], "normalize_crashed_interval");
    assert_eq!(before["session"]["state"], "running");
    assert_eq!(
        before["intervals"][0]["ended_at"],
        serde_json::Value::Null,
        "改动前必须留原始开放事实"
    );
    assert_eq!(after["session"]["state"], "recovering");
    assert_eq!(after["candidate_end"], 1350);
    assert_eq!(after["candidate_end_source"], "last_checkpoint");
    assert_eq!(after["intervals"][0]["ended_at"], 1350);
    assert_eq!(after["intervals"][0]["needs_review"], false);
    assert_eq!(after["intervals"][1]["needs_review"], true);
    assert_eq!(
        after["intervals"][1]["duration_ms"],
        serde_json::Value::Null
    );
    assert_eq!(reason.as_deref(), Some("crashed open interval normalized"));

    let (before_json, after_json, reason) = time_edit_of(db, "s-pause");
    let before: serde_json::Value = serde_json::from_str(&before_json).unwrap();
    let after: serde_json::Value = serde_json::from_str(&after_json).unwrap();
    assert_eq!(before["change"], "rebind_run");
    assert_eq!(before["session"]["run_id"], OLD_RUN);
    assert_eq!(after["session"]["run_id"], running.run_id());
    assert_eq!(after["session"]["state"], "paused");
    assert!(
        after.get("candidate_end_source").is_none(),
        "重绑不改区间事实，就没有候选终点来源"
    );
    assert_eq!(
        reason.as_deref(),
        Some("paused session rebound to this run")
    );

    // 门禁快照（紧随其后的 `scan_recovery`）：重绑过的会话不再是恢复材料。
    let gate = running.recovery();
    assert_eq!(gate.unfinished_sessions, ["s-crash", "s-rec", "s-fault"]);
    assert_eq!(
        gate.invariant_faults,
        [InvariantFault {
            session_id: "s-fault".into(),
            reason: "running 会话没有开放区间",
        }],
        "recovering 的终点未知段不是不变量损坏（C3 收紧）"
    );
    assert_eq!(gate.pending_intervals.len(), 2);
    assert!(gate.pending_intervals.iter().any(|id| id == "iv-rec"));
    assert_eq!(
        pending_ids(db, "s-crash").len(),
        1,
        "归一后的待确认段也进了门禁"
    );
    assert!(gate.requires_recovery());
    assert_eq!(
        orphan_open_intervals(db),
        0,
        "除了 recovering 的合法未知段，不得有别的开放区间"
    );
    assert_eq!(
        unknown_end_open_intervals(db),
        1,
        "唯一一条是 s-rec 的合法未知段"
    );
}

/// 上一代次的暂停会话重绑后**可被结束**（不再是 `StaleRunContext`）。
#[test]
fn an_old_paused_session_rebound_to_this_run_can_then_be_finished() {
    let fx = fixture();
    let db = seeded(&fx);
    insert_session(&db, "s-pause", OLD_RUN, "FOREGROUND", "paused", 1000, 0);
    insert_interval(&db, "iv-pause", "s-pause", 1000, Some(1600), Some(600), 0);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());

    let version = {
        let db = state.db();
        let facts = session(db, "s-pause");
        assert_eq!(
            facts.run_id,
            running.run_id(),
            "扫描已把 run_id 重绑到本次 run"
        );
        assert_eq!(facts.row_version, 1);
        facts.row_version
    };

    let outcome = state
        .finish(SessionRequest {
            expected_data_epoch: running.data_epoch().to_string(),
            session_id: "s-pause".into(),
            session_expected_version: version,
        })
        .expect("重绑之后结束原会话不该再撞 StaleRunContext");
    assert_eq!(outcome.revision, 2, "扫描一次 + 结束一次");

    let db = state.db();
    assert_eq!(session(db, "s-pause").state, "finished");
    assert_eq!(session(db, "s-pause").row_version, 2);
    assert_eq!(session(db, "s-pause").ended_at, Some(WALL));
    assert_eq!(
        interval(db, "iv-pause").duration_ms,
        Some(600),
        "历史时长不动"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 扫描结论（§0.5 的 DTO 形状）
// ─────────────────────────────────────────────────────────────────────────────

/// `StartupScanReport` 的每一类名单与 `attention` 的逐字段形状。
#[test]
fn the_scan_report_carries_every_class_with_its_own_attention_verdict() {
    let fx = fixture();
    let mut db = seeded(&fx);
    insert_session(&db, "s-crash", OLD_RUN, "FOREGROUND", "running", 1000, 0);
    insert_interval(&db, "iv-crash", "s-crash", 1000, None, None, 0);
    insert_checkpoint(&db, "iv-crash", OLD_RUN, 1400, 1350, 350);
    insert_session(&db, "s-rec", OLD_RUN, "BACKGROUND", "recovering", 1200, 1);
    insert_interval(&db, "iv-rec", "s-rec", 1200, None, None, 1);
    insert_session(&db, "s-pause", OLD_RUN, "BACKGROUND", "paused", 1300, 0);
    insert_interval(&db, "iv-pause", "s-pause", 1300, Some(1600), Some(300), 0);
    insert_session(&db, "s-fault", OLD_RUN, "BACKGROUND", "running", 1500, 0);

    let report = scan_at_startup(&mut db, NOW_RUN, WALL).unwrap();

    assert_eq!(report.normalized_sessions, ["s-crash"]);
    assert_eq!(report.rebound_sessions, ["s-pause"]);
    assert_eq!(report.recovering_kept, ["s-rec"]);
    assert_eq!(
        report.faults,
        [InvariantFault {
            session_id: "s-fault".into(),
            reason: "running 会话没有开放区间",
        }]
    );
    assert!(report.revision_changed, "这一批真的改了事实");

    // 顺序 = 未结束会话的读取顺序（started_at, id）。
    let ids: Vec<&str> = report
        .attention
        .iter()
        .map(|item| item.session_id.as_str())
        .collect();
    assert_eq!(ids, ["s-crash", "s-rec", "s-pause", "s-fault"]);

    let crash = &report.attention[0];
    assert_eq!(crash.task_id, "t1");
    assert_eq!(crash.state, SessionState::Recovering);
    assert_eq!(crash.run_id, OLD_RUN);
    assert!(!crash.is_current_run);
    assert_eq!(crash.attention, SessionAttention::NeedsReview);
    assert_eq!(crash.fault_reason, None);
    assert_eq!(crash.session_row_version, 1, "归一后的版本");
    assert!(crash.session_needs_review);
    assert_eq!(crash.intervals.len(), 1, "只列待确认区间，可信前缀不进列表");
    let tail = &crash.intervals[0];
    assert_eq!(tail.started_at, 1350);
    assert_eq!(tail.ended_at, Some(1350));
    assert_eq!(tail.duration_ms, None);
    assert_eq!(tail.sampled_end_wall_at, None);
    assert!(tail.needs_review);

    let rec = &report.attention[1];
    assert_eq!(rec.state, SessionState::Recovering);
    assert_eq!(rec.attention, SessionAttention::NeedsReview);
    assert_eq!(rec.session_row_version, 0, "原样保持的会话版本不动");
    assert_eq!(rec.intervals.len(), 1);
    assert_eq!(rec.intervals[0].id, "iv-rec");
    assert_eq!(rec.intervals[0].ended_at, None, "终点未知的候选端点为空");
    assert_eq!(rec.intervals[0].duration_ms, None);

    let pause = &report.attention[2];
    assert_eq!(pause.state, SessionState::Paused);
    assert_eq!(
        pause.attention,
        SessionAttention::None,
        "干净暂停不需要处理"
    );
    assert_eq!(pause.run_id, NOW_RUN);
    assert!(pause.is_current_run, "重绑后它属于本次 run");
    assert_eq!(pause.session_row_version, 1);
    assert_eq!(pause.fault_reason, None);
    assert!(pause.intervals.is_empty());

    let fault = &report.attention[3];
    assert_eq!(fault.state, SessionState::Running);
    assert_eq!(fault.attention, SessionAttention::InvariantBroken);
    assert_eq!(
        fault.fault_reason.as_deref(),
        Some("running 会话没有开放区间")
    );
    assert!(!fault.is_current_run);
    assert_eq!(fault.session_row_version, 0);
    assert!(fault.intervals.is_empty());

    // 一次批量事务：恰好一次 revision。
    assert_eq!(revision(&db), 1);
}

/// 干净库（没有别的 run 的残留）⇒ 不写、不加版本。
#[test]
fn a_scan_with_nothing_to_do_writes_nothing_and_leaves_the_revision_alone() {
    let fx = fixture();
    let mut db = seeded(&fx);
    let writes_before = total_changes(&db);

    let report = scan_at_startup(&mut db, NOW_RUN, WALL).unwrap();

    assert!(!report.revision_changed);
    assert!(report.normalized_sessions.is_empty());
    assert!(report.rebound_sessions.is_empty());
    assert!(report.recovering_kept.is_empty());
    assert!(report.faults.is_empty());
    assert!(report.attention.is_empty());
    assert_eq!(revision(&db), 0, "没有变化就不加 revision");
    assert_eq!(total_changes(&db), writes_before, "没有变化就不写任何行");
}

// ─────────────────────────────────────────────────────────────────────────────
// 防御分支（R4）：判据之间不一致时只降级、不 abort 整批
// ─────────────────────────────────────────────────────────────────────────────

/// `running` 却没有开放区间：归第 1 类，且**后面的行照常处理**。
#[test]
fn a_running_session_without_an_open_interval_is_diagnosed_without_aborting_the_batch() {
    let fx = fixture();
    let db = seeded(&fx);
    // 先出现的坏行……
    insert_session(&db, "s-broken", OLD_RUN, "BACKGROUND", "running", 1000, 0);
    // ……以及排在它后面、必须被继续处理的好行。
    insert_session(&db, "s-crash", OLD_RUN, "FOREGROUND", "running", 1500, 0);
    insert_interval(&db, "iv-crash", "s-crash", 1500, None, None, 0);
    insert_checkpoint(&db, "iv-crash", OLD_RUN, 1800, 1750, 250);
    drop(db);

    let running = started(&fx);
    let state = lock_app(running.app());
    let db = state.db();

    assert_eq!(
        running.recovery().invariant_faults,
        [InvariantFault {
            session_id: "s-broken".into(),
            reason: "running 会话没有开放区间",
        }]
    );
    assert_eq!(
        session(db, "s-broken"),
        SessionFacts {
            state: "running".into(),
            run_id: OLD_RUN.into(),
            needs_review: 0,
            row_version: 0,
            ended_at: None,
        },
        "损坏行只诊断，不写事实"
    );
    assert_eq!(interval_count(db, "s-broken"), 0);
    // 整批没有中断：后面的崩溃区间照常归一。
    assert_eq!(session(db, "s-crash").state, "recovering");
    assert_eq!(interval(db, "iv-crash").ended_at, Some(1750));
    assert_eq!(revision(db), 1, "一批扫描恰好一次 revision");
}

/// S5 的契约：没有开放区间就回 `NoOpenInterval`，**不写任何东西**。
///
/// 服务层的第 1 类判据（`invariant_faults` 的 `running_without_open_interval`）
/// 会在扫描里先一步拦住同样的行；这条直接钉住原语本身的防御行为。
#[test]
fn normalizing_a_crashed_interval_without_an_open_row_is_rejected_and_writes_nothing() {
    let fx = fixture();
    let mut db = seeded(&fx);
    insert_session(&db, "s-mixed", OLD_RUN, "BACKGROUND", "running", 1000, 0);
    let writes_before = total_changes(&db);

    let err = {
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        session_repo::normalize_crashed_open_interval(&tx, "s-mixed")
            .expect_err("没有开放区间就没有可归一的崩溃区间")
    };
    assert_eq!(err.code(), "DOMAIN_ERROR");

    assert_eq!(session(&db, "s-mixed").row_version, 0, "失败不留半个改动");
    assert_eq!(interval_count(&db, "s-mixed"), 0);
    assert_eq!(total_changes(&db), writes_before, "被拒的原语不写任何行");
}

// ─────────────────────────────────────────────────────────────────────────────
// 「paused + 待确认区间」：四类之外的手工事实
// ─────────────────────────────────────────────────────────────────────────────

/// 暂停会话仍有待确认区间时**不重绑**：它要先走 `reconcile`，不能被悄悄接管。
#[test]
fn an_old_paused_session_with_pending_intervals_is_left_for_the_user() {
    let fx = fixture();
    let db = seeded(&fx);
    insert_session(&db, "s-suspect", OLD_RUN, "FOREGROUND", "paused", 1000, 1);
    insert_interval(&db, "iv-suspect", "s-suspect", 1000, Some(1500), None, 1);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());

    {
        let db = state.db();
        assert_eq!(
            session(db, "s-suspect"),
            SessionFacts {
                state: "paused".into(),
                run_id: OLD_RUN.into(),
                needs_review: 1,
                row_version: 0,
                ended_at: None,
            },
            "有待确认事实就不重绑，原样交给用户处理"
        );
        assert_eq!(
            interval(db, "iv-suspect"),
            IntervalFacts {
                session_id: "s-suspect".into(),
                started_at: 1000,
                ended_at: Some(1500),
                duration_ms: None,
                sampled_end_wall_at: None,
                needs_review: 1,
                voided_at: None,
            }
        );
        assert_eq!(revision(db), 0, "原样保持 = 没有字段变化 = 不加 revision");
        assert_eq!(time_edit_count(db), 0);
        assert!(
            running.recovery().requires_recovery(),
            "它仍是别的 run 的待确认事实，门禁照旧关着"
        );
    }

    // 扫描结论里它归 `NeedsReview`（不是第 4 类）。
    let report = scan_at_startup(state.db_mut(), running.run_id(), WALL).unwrap();
    assert!(!report.revision_changed);
    assert!(report.rebound_sessions.is_empty());
    assert_eq!(report.attention.len(), 1);
    let item = &report.attention[0];
    assert_eq!(item.session_id, "s-suspect");
    assert_eq!(item.state, SessionState::Paused);
    assert_eq!(item.attention, SessionAttention::NeedsReview);
    assert_eq!(item.run_id, OLD_RUN, "保持原归属");
    assert!(!item.is_current_run);
    assert_eq!(item.intervals.len(), 1);
    assert_eq!(item.intervals[0].id, "iv-suspect");
    assert_eq!(item.intervals[0].duration_ms, None);

    // 门禁快照与报告同源：两条只读查询都看得见它。
    let gate = scan_recovery(state.db().connection(), running.run_id()).unwrap();
    assert_eq!(gate.unfinished_sessions, ["s-suspect"]);
    assert_eq!(gate.pending_intervals, ["iv-suspect"]);
    assert!(gate.invariant_faults.is_empty(), "它不是损坏，只是待确认");
}

/// `time_edit` 里没有这个会话时返回 `None`（用例断言「隔离的记录不写审计」）。
fn time_edit_of_optional(db: &Db, session_id: &str) -> Option<String> {
    use rusqlite::OptionalExtension;
    db.connection()
        .query_row(
            "SELECT id FROM time_edit WHERE session_id = ?1",
            [session_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .unwrap()
}
