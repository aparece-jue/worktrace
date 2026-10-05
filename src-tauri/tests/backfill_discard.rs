//! P3 Task 4：`backfill`（手工补录）与 `discard_session`（作废整次）。
//!
//! 装置抄 `tests/correct.rs` / `tests/reconcile.rs`：造一个「上一个 run 留下事实」的库，
//! 走**真实**的 `bootstrap::startup`，然后在同一把锁（`lock_app`）里调 `AppState` 的
//! 两个命令入口（S2 包装——P8 的 IPC 就走这两条）。
//!
//! 覆盖（Task 4 的测试清单）：
//! 补录不产生 `task_change` 行 · 不占前台（补录之后另一次 `start` 仍能成功）·
//! `work_session.state = 'finished'` 且 `sampled_end_wall_at IS NULL`（不把服务算的时刻
//! 冒充采样）· 不写检查点 · 不冻估时基准 · 重叠被拒（端点相接不算）· `ended_at > now`、
//! 负区间、未知任务被拒（逐字段零变化）· **失败整体回滚**（命令中途失败的注入）·
//! 作废整次：**全部**区间 `voided_at` 非空 + `needs_review` 清零 + 会话 `discarded` +
//! `needs_review = false` · 已知时长保留端点、无时长候选清 `ended_at` ·
//! 作废不改任务状态（逐字段比任务行）· 两者都写审计（Ruling 8：没有 `candidate_*`）·
//! 作废后 `requires_recovery()` 变化 · 作废运行中的会话后镜像停在 `discarded`
//! （R8，与 `finish` 停在 `finished` 同形）· Ruling 6 携带：`paused` + 待确认区间的
//! 出口是 `discard_session` · 与 `reconcile(discard_uncertain)` 可分辨（后者只动作废
//! 待确认段、保留可信前缀）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::history::BackfillRequest;
use worktrace_lib::services::recovery::{
    DiscardSessionRequest, ReconcileAction, ReconcileRequest, ReconcileTargetState,
};
use worktrace_lib::services::timer::coordinator::StartRequest;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::run_repo;

/// 启动那一刻的挂钟（`FakeClock` 不自己走，所以 `now` 恒等于它）。
const WALL: i64 = 1_700_000_000_000;
/// 崩掉的那一代次。
const OLD_RUN: &str = "run-old";

/// 补录范围：落在 `WALL` 之前，与下面的既有事实都不重叠。
const BF_START: i64 = WALL - 20_000;
const BF_END: i64 = WALL - 18_000;

/// 既有 `finished` 会话的区间（重叠用例的靶子）。
const S1_START: i64 = WALL - 60_000;
const S1_END: i64 = WALL - 58_000;

#[test]
fn empty_backfills_do_not_overlap_closed_intervals() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);
    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let point = S1_START + 500;
    state
        .backfill(env_create(&epoch), backfill_request("t1", point, point))
        .expect("an empty interval inside trusted history is not an overlap");
}

#[test]
fn an_existing_empty_interval_does_not_block_a_nonempty_backfill() {
    let fx = fixture();
    drop(seeded(&fx));
    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let point = BF_START + 500;
    state
        .backfill(env_create(&epoch), backfill_request("t1", point, point))
        .unwrap();
    state
        .backfill(env_create(&epoch), backfill_request("t1", BF_START, BF_END))
        .expect("existing empty history consumes no human time");
}

#[test]
fn backfill_rejects_an_unrepresentable_duration_without_writes() {
    let fx = fixture();
    drop(seeded(&fx));
    let running = started(&fx);
    let mut state = lock_app(running.app());
    let before = world(state.db());
    let error = state
        .backfill(
            env_create(running.data_epoch()),
            backfill_request("t1", i64::MIN, WALL),
        )
        .expect_err("extreme endpoints must be rejected rather than panic or wrap");
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(world(state.db()), before);
}

/// 恢复会话的几何：可信前缀 + 已知终点的候选 + 终点未知的开放候选。
const T0: i64 = WALL - 40_000;
const PREFIX_END: i64 = WALL - 38_000;
const CAND_END: i64 = WALL - 37_000;
const OPEN_START: i64 = WALL - 36_000;

/// `paused` + 待确认那一类（四类判定盖不住的形态，Ruling 6）。
const P_START: i64 = WALL - 30_000;
const P_END: i64 = WALL - 29_000;

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

/// 真跑一次启动（第 ④ 步的扫描会归一崩溃段、重绑第 4 类）。
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

/// 建库：迁移 + 元数据 + 上一代 run + 一条 `Doing` 任务 + 一条任务完成事件。
fn seeded(fx: &Fixture) -> Db {
    let mut db = Db::open(&fx.db_path).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    run_repo::start_run(&tx, OLD_RUN, WALL - 90_000).unwrap();
    tx.execute(
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Doing',0,1000,1000)",
        [],
    )
    .unwrap();
    // 已有的一条完成事件：补录**不得**伪造第二条（02 §3）。
    tx.execute(
        "INSERT INTO task_change(id,task_id,before_json,after_json,created_at)
         VALUES('tc-done','t1','{\"status\":\"Doing\"}','{\"status\":\"Done\"}',?1)",
        rusqlite::params![WALL - 70_000],
    )
    .unwrap();
    tx.commit().unwrap();
    db
}

// ── 造事实 ──────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn insert_session(
    db: &Db,
    id: &str,
    run: &str,
    mode: &str,
    state: &str,
    started_at: i64,
    ended_at: Option<i64>,
    needs_review: i64,
) {
    db.connection()
        .execute(
            "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,ended_at,
                                      needs_review,row_version)
             VALUES(?1,'t1',?2,?3,?4,'stopwatch',?5,?6,?7,0)",
            rusqlite::params![id, run, mode, state, started_at, ended_at, needs_review],
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

/// 一条可信的闭合区间（`needs_review = 0`、时长与起止一致）。
fn insert_trusted_interval(db: &Db, id: &str, session_id: &str, start: i64, end: i64) {
    insert_interval(
        db,
        id,
        session_id,
        start,
        Some(end),
        Some(end - start),
        0,
        None,
    );
}

/// 一条干净的 `finished` 会话（`s1`）：不占前台、不是门禁材料。
fn finished_fixture(db: &Db) {
    insert_session(
        db,
        "s1",
        OLD_RUN,
        "FOREGROUND",
        "finished",
        S1_START,
        Some(S1_END),
        0,
    );
    insert_trusted_interval(db, "s1-a", "s1", S1_START, S1_END);
}

/// 一条 S5 归一过的 `recovering` 会话：可信前缀 + 已知终点的候选 + 终点未知的开放候选。
///
/// 三种区间覆盖 S8 的两条作废规则（有 `duration_ms` 保留端点、没有则清 `ended_at`），
/// 而且三条谓词都不命中（开放那条是 `recovering AND needs_review = 1`，第 3 条显式放行）。
fn recovering_fixture(db: &Db, id: &str) {
    insert_session(db, id, OLD_RUN, "FOREGROUND", "recovering", T0, None, 1);
    insert_trusted_interval(db, &format!("{id}-prefix"), id, T0, PREFIX_END);
    insert_interval(
        db,
        &format!("{id}-cand"),
        id,
        PREFIX_END,
        Some(CAND_END),
        None,
        1,
        None,
    );
    insert_interval(
        db,
        &format!("{id}-open"),
        id,
        OPEN_START,
        None,
        None,
        1,
        None,
    );
}

/// Ruling 6 的形态：`paused` 却仍挂着待确认区间（四类判定都盖不住）。
fn paused_pending_fixture(db: &Db, id: &str) {
    insert_session(
        db,
        id,
        OLD_RUN,
        "FOREGROUND",
        "paused",
        P_START,
        Some(P_END),
        1,
    );
    insert_interval(
        db,
        &format!("{id}-cand"),
        id,
        P_START,
        Some(P_END),
        None,
        1,
        None,
    );
}

// ── 读事实 ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionFacts {
    state: String,
    run_id: String,
    mode: String,
    timer_kind: String,
    target_duration_ms: Option<i64>,
    started_at: i64,
    ended_at: Option<i64>,
    needs_review: i64,
    row_version: i64,
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

/// 「逐字段比对」用的全局事实：行数 + 版本 + 两种审计。
///
/// 被拒命令的零变化用 `assert_eq!(world(db), before)` 一句话比对**每个字段**，
/// 而不是只比行数（总纲 §5 第 8 条的口径）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorldFacts {
    sessions: i64,
    intervals: i64,
    revision: i64,
    time_edit_rows: i64,
    task_change_rows: i64,
    last_task_change: Option<(String, String, i64)>,
}

/// 一条会话 + 它的**全部**区间 + 全局事实。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionWorld {
    world: WorldFacts,
    session: SessionFacts,
    intervals: Vec<(String, IntervalFacts)>,
}

/// 任务行（作废/补录都**不得**隐式改它）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskFacts {
    project_id: Option<String>,
    title: String,
    status: String,
    row_version: i64,
    created_at: i64,
    updated_at: i64,
}

fn scalar(db: &Db, sql: &str) -> i64 {
    db.connection().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn revision(db: &Db) -> i64 {
    scalar(db, "SELECT revision FROM app_meta WHERE singleton = 1")
}

fn world(db: &Db) -> WorldFacts {
    WorldFacts {
        sessions: scalar(db, "SELECT COUNT(*) FROM work_session"),
        intervals: scalar(db, "SELECT COUNT(*) FROM work_interval"),
        revision: revision(db),
        time_edit_rows: scalar(db, "SELECT COUNT(*) FROM time_edit"),
        task_change_rows: scalar(db, "SELECT COUNT(*) FROM task_change"),
        last_task_change: db
            .connection()
            .query_row(
                "SELECT before_json, after_json, created_at FROM task_change
                  ORDER BY created_at DESC, id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok(),
    }
}

fn session(db: &Db, id: &str) -> SessionFacts {
    db.connection()
        .query_row(
            "SELECT state, run_id, mode, timer_kind, target_duration_ms, started_at, ended_at,
                    needs_review, row_version
               FROM work_session WHERE id = ?1",
            [id],
            |r| {
                Ok(SessionFacts {
                    state: r.get(0)?,
                    run_id: r.get(1)?,
                    mode: r.get(2)?,
                    timer_kind: r.get(3)?,
                    target_duration_ms: r.get(4)?,
                    started_at: r.get(5)?,
                    ended_at: r.get(6)?,
                    needs_review: r.get(7)?,
                    row_version: r.get(8)?,
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

fn session_world(db: &Db, id: &str) -> SessionWorld {
    let ids: Vec<String> = {
        let mut stmt = db
            .connection()
            .prepare("SELECT id FROM work_interval WHERE session_id = ?1 ORDER BY id")
            .unwrap();
        let rows = stmt.query_map([id], |r| r.get::<_, String>(0)).unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    };
    SessionWorld {
        world: world(db),
        session: session(db, id),
        intervals: ids
            .into_iter()
            .map(|interval_id| {
                let facts = interval(db, &interval_id);
                (interval_id, facts)
            })
            .collect(),
    }
}

fn task(db: &Db, id: &str) -> TaskFacts {
    db.connection()
        .query_row(
            "SELECT project_id, title, status, row_version, created_at, updated_at
               FROM task WHERE id = ?1",
            [id],
            |r| {
                Ok(TaskFacts {
                    project_id: r.get(0)?,
                    title: r.get(1)?,
                    status: r.get(2)?,
                    row_version: r.get(3)?,
                    created_at: r.get(4)?,
                    updated_at: r.get(5)?,
                })
            },
        )
        .unwrap()
}

/// 某会话的全部审计行：`(change, before_json, after_json, reason)`。
fn edits(
    db: &Db,
    session_id: &str,
) -> Vec<(String, serde_json::Value, serde_json::Value, Option<String>)> {
    let mut stmt = db
        .connection()
        .prepare(
            "SELECT before_json, after_json, reason FROM time_edit
              WHERE session_id = ?1 ORDER BY created_at, id",
        )
        .unwrap();
    let rows = stmt
        .query_map([session_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .unwrap();
    rows.map(|row| {
        let (before, after, reason) = row.unwrap();
        let before: serde_json::Value = serde_json::from_str(&before).unwrap();
        let after: serde_json::Value = serde_json::from_str(&after).unwrap();
        let change = before["change"].as_str().unwrap().to_string();
        (change, before, after, reason)
    })
    .collect()
}

/// 审计 JSON 里某条区间的条目（`intervals` 数组按 id 取）。
fn audit_interval(json: &serde_json::Value, id: &str) -> serde_json::Value {
    json["intervals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("审计里没有区间 {id}：{json}"))
        .clone()
}

// ── 请求构造 ────────────────────────────────────────────────────────────────

fn backfill_request(task_id: &str, started_at: i64, ended_at: i64) -> BackfillRequest {
    BackfillRequest {
        task_id: task_id.to_string(),
        started_at,
        ended_at,
    }
}

fn discard_request(session_id: &str) -> DiscardSessionRequest {
    DiscardSessionRequest {
        session_id: session_id.to_string(),
    }
}

fn env_create(epoch: &str) -> WriteEnvelope {
    WriteEnvelope::for_create(epoch)
}

fn env_update(epoch: &str, row_version: i64) -> WriteEnvelope {
    WriteEnvelope::for_update(epoch, row_version)
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

fn task_version(db: &Db) -> i64 {
    task(db, "t1").row_version
}

fn assert_code(error: &AppError, code: &str) {
    assert_eq!(error.code(), code, "实际：{error:?}");
}

// ── 命令中途失败的注入 ──────────────────────────────────────────────────────

/// 让 `time_edit` 的插入失败：命令的会话/区间写入与审计**同一事务**，
/// 所以失败必须整体回滚（不留半个会话、不留半条审计）。
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

// ─────────────────────────────────────────────────────────────────────────────
// backfill：一条 `finished` 会话 + 一条可信闭合区间
// ─────────────────────────────────────────────────────────────────────────────

/// 补录建的是**终态**会话与**可信闭合**区间：`finished`、`needs_review = 0`、
/// `duration_ms = ended_at - started_at`，而 `sampled_end_wall_at` **必须是 NULL**
/// ——补录没有采样，不得把服务算的时刻写进那一列冒充证据。
#[test]
fn backfilling_records_a_finished_session_with_a_trusted_closed_interval() {
    let fx = fixture();
    let db = seeded(&fx);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let run = running.run_id().to_string();
    let before = world(state.db());
    assert!(
        !state.recovery().requires_recovery(),
        "夹具里没有恢复材料，门禁应当是开的"
    );

    let outcome = state
        .backfill(env_create(&epoch), backfill_request("t1", BF_START, BF_END))
        .expect("补录应当成功");
    let (report, changed) = outcome.into_parts();
    assert!(changed, "补录必然新建事实");

    // 会话：终态、当前 run、正计时、无预算。
    assert_eq!(report.session.state, SessionState::Finished);
    assert_eq!(report.session.mode, SessionMode::Foreground);
    assert_eq!(report.session.timer_kind, TimerKind::Stopwatch);
    assert_eq!(report.session.target_duration_ms, None);
    assert_eq!(report.session.run_id, run);
    assert_eq!(report.session.started_at, BF_START);
    assert_eq!(report.session.ended_at, Some(BF_END));
    assert!(!report.session.needs_review);
    assert_eq!(report.session.row_version, 0, "新建行从 0 开始");

    // 区间：可信闭合（不是候选）。
    assert_eq!(report.interval.session_id, report.session.id);
    assert_eq!(report.interval.started_at, BF_START);
    assert_eq!(report.interval.ended_at, Some(BF_END));
    assert_eq!(report.interval.duration_ms, Some(BF_END - BF_START));
    assert!(!report.interval.needs_review);
    assert_eq!(report.interval.voided_at, None);
    assert_eq!(
        report.interval.sampled_end_wall_at, None,
        "补录没有采样，不得把服务算的时刻写成采样证据"
    );
    assert_eq!(report.revision, before.revision + 1);
    assert_eq!(report.data_epoch, epoch);

    let db = state.db();
    let session_id = report.session.id.clone();
    let interval_id = report.interval.id.clone();
    assert_eq!(session(db, &session_id).state, "finished");
    assert_eq!(interval(db, &interval_id).sampled_end_wall_at, None);
    assert_eq!(
        scalar(
            db,
            &format!(
                "SELECT COUNT(*) FROM interval_checkpoint WHERE interval_id = '{interval_id}'"
            )
        ),
        0,
        "补录不写检查点（它不是计时路径）"
    );
    // 不占前台：库里没有任何 running 行，镜像也没被这条命令碰过。
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM work_session WHERE mode='FOREGROUND' AND state='running'"
        ),
        0
    );
    assert!(
        state.coordinator().live().is_none(),
        "backfill 不做镜像刷新"
    );
    assert_eq!(world(db).sessions, before.sessions + 1);
    assert_eq!(world(db).intervals, before.intervals + 1);
}

/// 补录**不伪造完成事件**、不动任务状态、也不冻估时基准（那是 `start` 的事）。
#[test]
fn backfilling_produces_no_task_change_row_and_leaves_the_task_alone() {
    let fx = fixture();
    let db = seeded(&fx);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before_task = task(state.db(), "t1");
    let before = world(state.db());

    let (report, changed) = state
        .backfill(env_create(&epoch), backfill_request("t1", BF_START, BF_END))
        .unwrap()
        .into_parts();
    assert!(changed);

    let db = state.db();
    assert_eq!(
        task(db, "t1"),
        before_task,
        "补录不改任务状态、不冻估时基准（判据：任务行逐字段不变，含 row_version/updated_at）"
    );
    let after = world(db);
    assert_eq!(after.task_change_rows, before.task_change_rows);
    assert_eq!(after.last_task_change, before.last_task_change);
    assert_eq!(
        after.time_edit_rows,
        before.time_edit_rows + 1,
        "写一条审计"
    );
    assert_eq!(after.revision, before.revision + 1);

    // 审计挂在新会话上，`change`/`reason` 都是 `backfill`。
    let rows = edits(db, &report.session.id);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "backfill");
    assert_eq!(rows[0].3.as_deref(), Some("backfill"));
}

/// **不占前台**：补录之后另一次 `start` 仍然成功（它建的是 `finished` 行，
/// 不碰 `uq_running_foreground`），而且镜像由那次 `start` 装载。
#[test]
fn backfilling_does_not_take_the_foreground_slot() {
    let fx = fixture();
    let db = seeded(&fx);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let (report, _) = state
        .backfill(env_create(&epoch), backfill_request("t1", BF_START, BF_END))
        .unwrap()
        .into_parts();
    assert!(state.coordinator().live().is_none());

    let task_v = task_version(state.db());
    let outcome = state
        .start(start_request(&epoch, task_v))
        .expect("补录不占前台槽位：紧接着的 start 必须成功");
    assert_eq!(outcome.snapshot.state, Some(SessionState::Running));
    let live = state.coordinator().live().expect("start 装载镜像");
    assert_ne!(live.id, report.session.id, "镜像装的是新会话，不是补录那条");

    let db = state.db();
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM work_session WHERE mode='FOREGROUND' AND state='running'"
        ),
        1,
        "前台槽位只有那一条新会话"
    );
    assert_eq!(session(db, &report.session.id).state, "finished");
    assert_eq!(session(db, &live.id).state, "running");
}

/// 已经有既有人工时间压着这段范围 ⇒ 拒绝，且**逐字段零变化**。
/// 端点相接**不算**重叠（半开口径）。
#[test]
fn backfilling_an_overlapping_range_is_rejected_without_any_write() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = world(state.db());

    let error = state
        .backfill(
            env_create(&epoch),
            backfill_request("t1", S1_START + 1_000, S1_END + 1_000),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    let detail = error.detail().unwrap_or_default().to_string();
    assert!(detail.contains("重叠"), "命中重叠规则的文案：{detail}");
    assert!(
        detail.contains(&S1_START.to_string()) && detail.contains(&S1_END.to_string()),
        "文案要能指出与哪一段重叠：{detail}"
    );
    assert_eq!(
        world(state.db()),
        before,
        "被拒的补录不得留下半个会话或半条审计"
    );

    // 端点相接：`[S1_END, …)` 与 `[S1_START, S1_END)` 交集为空 ⇒ 允许。
    let (report, changed) = state
        .backfill(
            env_create(&epoch),
            backfill_request("t1", S1_END, S1_END + 1_000),
        )
        .expect("端点相接不算重叠")
        .into_parts();
    assert!(changed);
    assert_eq!(report.interval.started_at, S1_END);
}

/// 未来区间、负区间、未知任务各自被拒，三种都不写任何东西。
#[test]
fn backfill_refuses_a_future_end_a_negative_range_and_an_unknown_task() {
    let fx = fixture();
    let db = seeded(&fx);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = world(state.db());

    let error = state
        .backfill(
            env_create(&epoch),
            backfill_request("t1", BF_START, WALL + 1),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert!(
        error.detail().unwrap_or_default().contains("不能晚于"),
        "未来区间要说清是哪条规则：{error:?}"
    );

    let error = state
        .backfill(env_create(&epoch), backfill_request("t1", BF_END, BF_START))
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");

    let error = state
        .backfill(
            env_create(&epoch),
            backfill_request("nope", BF_START, BF_END),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert!(error.detail().unwrap_or_default().contains("找不到"));

    assert_eq!(world(state.db()), before, "三次被拒都是零变化");
    // 三次都通过之后，同样的请求仍然能用（证明拒绝没有留下副作用）。
    state
        .backfill(env_create(&epoch), backfill_request("t1", BF_START, BF_END))
        .expect("合法补录仍然成功");
}

/// 审计形状（Ruling 8/R12）：`change`/`reason` = `backfill`；`after_json` 是共享形状
/// （`session` + 逐字段的 `intervals`，含 `voided_at`）；端点不是候选推导 ⇒ **没有**
/// `candidate_*`；`user_reason` 只属 `correct` ⇒ 也没有它。
/// `before_json` 是**创建型**审计的空前态（补录前没有这条会话）。
#[test]
fn the_audit_records_a_backfill_and_has_no_candidate_keys() {
    let fx = fixture();
    let db = seeded(&fx);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let (report, _) = state
        .backfill(env_create(&epoch), backfill_request("t1", BF_START, BF_END))
        .unwrap()
        .into_parts();

    let db = state.db();
    let rows = edits(db, &report.session.id);
    assert_eq!(rows.len(), 1);
    let (change, before, after, reason) = &rows[0];
    assert_eq!(change, "backfill");
    assert_eq!(reason.as_deref(), Some("backfill"));
    assert!(
        before.get("session").is_none() && before.get("intervals").is_none(),
        "补录前没有这条会话，before 里不该编一个出来：{before}"
    );
    assert_eq!(after["session"]["state"], "finished");
    assert_eq!(after["session"]["needs_review"], false);
    let entry = audit_interval(after, &report.interval.id);
    assert_eq!(entry["started_at"], BF_START);
    assert_eq!(entry["ended_at"], BF_END);
    assert_eq!(entry["duration_ms"], BF_END - BF_START);
    assert_eq!(entry["sampled_end_wall_at"], serde_json::Value::Null);
    assert_eq!(entry["needs_review"], false);
    assert_eq!(entry["voided_at"], serde_json::Value::Null);
    for json in [before, after] {
        assert!(json.get("candidate_end").is_none(), "没有候选端点推导");
        assert!(json.get("candidate_end_source").is_none());
        assert!(
            json.get("user_reason").is_none(),
            "user_reason 只属 correct"
        );
    }
}

/// **失败整体回滚**：审计写不进去时，会话与区间也必须跟着回滚——不留半个会话。
#[test]
fn a_failed_backfill_rolls_back_the_whole_session() {
    let fx = fixture();
    let db = seeded(&fx);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = world(state.db());

    reject_time_edit_writes(state.db());
    let error = state
        .backfill(env_create(&epoch), backfill_request("t1", BF_START, BF_END))
        .unwrap_err();
    assert_code(&error, "STORAGE_ERROR");
    assert_eq!(
        world(state.db()),
        before,
        "会话、区间、审计必须在同一个事务里一起回滚"
    );

    allow_time_edit_writes(state.db());
    state
        .backfill(env_create(&epoch), backfill_request("t1", BF_START, BF_END))
        .expect("注入解除之后同一条请求必须成功");
}

// ─────────────────────────────────────────────────────────────────────────────
// discard_session：作废整次
// ─────────────────────────────────────────────────────────────────────────────

/// 作废整次：**全部**区间（含终点未知的开放候选）都 `voided_at` 非空、`needs_review` 清零，
/// 会话 `discarded` 且 `needs_review = false`；已知时长的区间保留原起点，
/// 无时长的候选把 `ended_at` 清回 `NULL`（不补 0 时长冒充事实）。
#[test]
fn discarding_a_session_voids_every_interval_and_marks_it_discarded() {
    let fx = fixture();
    let db = seeded(&fx);
    recovering_fixture(&db, "s-rec");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = session_world(state.db(), "s-rec");
    assert_eq!(before.session.state, "recovering");

    let (report, changed) = state
        .discard_session(
            env_update(&epoch, before.session.row_version),
            discard_request("s-rec"),
        )
        .expect("作废整次必须能作用于 recovery 材料")
        .into_parts();
    assert!(changed);

    assert_eq!(report.session.state, SessionState::Discarded);
    assert!(!report.session.needs_review);
    assert_eq!(
        report.session.ended_at,
        Some(WALL),
        "作废时刻 = max(now, session.started_at)"
    );
    assert_eq!(report.session.run_id, OLD_RUN, "run_id 不动");
    assert_eq!(report.session.row_version, before.session.row_version + 1);
    assert_eq!(
        report.interval.id, "s-rec-prefix",
        "报告里的区间取被作废的第一条（按 started_at）"
    );
    assert_eq!(report.interval.voided_at, Some(WALL));

    let db = state.db();
    // 三条区间一条不落。
    for id in ["s-rec-prefix", "s-rec-cand", "s-rec-open"] {
        let facts = interval(db, id);
        assert_eq!(facts.voided_at, Some(WALL), "{id} 必须被作废");
        assert_eq!(facts.needs_review, 0, "{id} 的 needs_review 必须清零");
    }
    // 已知时长：端点与时长原样保留（作废不是「没发生过」）。
    let prefix = interval(db, "s-rec-prefix");
    assert_eq!(prefix.started_at, T0);
    assert_eq!(prefix.ended_at, Some(PREFIX_END));
    assert_eq!(prefix.duration_ms, Some(PREFIX_END - T0));
    // 无时长候选：`ended_at` 清回 NULL（不补造时长）。
    let cand = interval(db, "s-rec-cand");
    assert_eq!(cand.ended_at, None, "候选端点不是事实，不能留着当时长");
    assert_eq!(cand.duration_ms, None);
    // 终点未知的开放候选：保持 NULL。
    let open = interval(db, "s-rec-open");
    assert_eq!(open.ended_at, None);
    assert_eq!(open.duration_ms, None);

    let after = session_world(db, "s-rec");
    assert_eq!(after.world.revision, before.world.revision + 1);
    assert_eq!(after.world.time_edit_rows, before.world.time_edit_rows + 1);
    assert_eq!(
        after.intervals.len(),
        before.intervals.len(),
        "软删除不是删行"
    );
}

/// **作废不改任务状态**：任务行逐字段（含 `row_version`/`updated_at`）不变，
/// 也不产生 `task_change`。
#[test]
fn discarding_a_session_does_not_touch_the_task_or_its_change_log() {
    let fx = fixture();
    let db = seeded(&fx);
    recovering_fixture(&db, "s-rec");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before_task = task(state.db(), "t1");
    let before = world(state.db());

    state
        .discard_session(env_update(&epoch, 0), discard_request("s-rec"))
        .unwrap();

    let db = state.db();
    assert_eq!(task(db, "t1"), before_task, "作废不隐式改变任务状态");
    let after = world(db);
    assert_eq!(after.task_change_rows, before.task_change_rows);
    assert_eq!(after.last_task_change, before.last_task_change);
}

/// 审计形状：两侧都带全部区间的逐字段值（`before` 未作废、`after` 已作废），
/// `reason = discard_session`，没有 `candidate_*`（Ruling 8）。
#[test]
fn the_audit_records_a_discard_session_with_voided_at_on_both_sides() {
    let fx = fixture();
    let db = seeded(&fx);
    recovering_fixture(&db, "s-rec");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    state
        .discard_session(env_update(&epoch, 0), discard_request("s-rec"))
        .unwrap();

    let db = state.db();
    let rows = edits(db, "s-rec");
    assert_eq!(rows.len(), 1);
    let (change, before, after, reason) = &rows[0];
    assert_eq!(change, "discard_session");
    assert_eq!(reason.as_deref(), Some("discard_session"));
    assert_eq!(before["session"]["state"], "recovering");
    assert_eq!(after["session"]["state"], "discarded");

    let old = audit_interval(before, "s-rec-cand");
    assert_eq!(old["voided_at"], serde_json::Value::Null);
    assert_eq!(old["ended_at"], CAND_END, "候选端点完整留在 before 里");
    let new = audit_interval(after, "s-rec-cand");
    assert_eq!(new["voided_at"], WALL);
    assert_eq!(new["ended_at"], serde_json::Value::Null);
    assert_eq!(new["duration_ms"], serde_json::Value::Null);
    for id in ["s-rec-prefix", "s-rec-open"] {
        assert_eq!(
            audit_interval(after, id)["voided_at"],
            WALL,
            "{id} 也要在审计里"
        );
    }
    for json in [before, after] {
        assert!(json.get("candidate_end").is_none());
        assert!(json.get("candidate_end_source").is_none());
        assert!(json.get("user_reason").is_none());
    }
}

/// 两者**在错误与审计上可分辨**：`reconcile(discard_uncertain)` 只作废待确认段、
/// 保留可信前缀并把会话推到目标状态；`discard_session` 作废整次。
#[test]
fn reconcile_discards_only_uncertain_intervals_while_discard_session_voids_all() {
    let fx = fixture();
    let db = seeded(&fx);
    recovering_fixture(&db, "s-rec");
    recovering_fixture(&db, "s-all");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    state
        .reconcile(
            env_update(&epoch, 0),
            ReconcileRequest {
                session_id: "s-rec".into(),
                action: ReconcileAction::DiscardUncertain,
                target_state: ReconcileTargetState::Finished,
                ranges: Vec::new(),
            },
        )
        .unwrap();
    state
        .discard_session(env_update(&epoch, 0), discard_request("s-all"))
        .unwrap();

    let db = state.db();
    // 对账：可信前缀还在，只是两条候选被作废。
    assert_eq!(interval(db, "s-rec-prefix").voided_at, None);
    assert_eq!(interval(db, "s-rec-cand").voided_at, Some(WALL));
    assert_eq!(interval(db, "s-rec-open").voided_at, Some(WALL));
    assert_eq!(session(db, "s-rec").state, "finished");
    // 作废整次：一条不剩。
    for id in ["s-all-prefix", "s-all-cand", "s-all-open"] {
        assert_eq!(interval(db, id).voided_at, Some(WALL), "{id} 必须被作废");
    }
    assert_eq!(session(db, "s-all").state, "discarded");
    // 审计上也能分辨。
    assert_eq!(
        edits(db, "s-rec")[0].3.as_deref(),
        Some("reconcile:discard_uncertain")
    );
    assert_eq!(edits(db, "s-all")[0].3.as_deref(), Some("discard_session"));
}

/// **门禁解除**：作废掉最后一条待处理事实之后 `requires_recovery()` 变假，
/// 而且 `start` 真的能用（闭环）。
#[test]
fn discarding_the_last_pending_fact_opens_the_recovery_gate() {
    let fx = fixture();
    let db = seeded(&fx);
    recovering_fixture(&db, "s-rec");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    assert!(
        state.recovery().requires_recovery(),
        "启动时有一条别的 run 的 recovering 会话"
    );
    let task_v = task_version(state.db());
    assert_code(
        &state.start(start_request(&epoch, task_v)).unwrap_err(),
        "RECOVERY_REQUIRED",
    );

    state
        .discard_session(env_update(&epoch, 0), discard_request("s-rec"))
        .expect("作废整次是这条待确认事实的出口");

    let scan = state.recovery();
    assert!(!scan.requires_recovery(), "事实处理完 ⇒ 门禁放开");
    assert!(scan.unfinished_sessions.is_empty());
    assert!(scan.pending_intervals.is_empty());
    assert!(scan.invariant_faults.is_empty());
    let task_v = task_version(state.db());
    state
        .start(start_request(&epoch, task_v))
        .expect("门禁解除后 start 必须成功");
}

/// Ruling 6 的**携带**：`paused` + 仍有待确认区间（四类判定都盖不住的形态）
/// 出口就是 `discard_session`——它无状态前置，作废整次即消掉那条待确认事实。
#[test]
fn a_paused_session_with_pending_intervals_can_be_discarded() {
    let fx = fixture();
    let db = seeded(&fx);
    paused_pending_fixture(&db, "s-paused");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = session_world(state.db(), "s-paused");
    assert_eq!(before.session.state, "paused");
    assert_eq!(
        before.session.run_id, OLD_RUN,
        "这一类不重绑 run_id（Ruling 6）"
    );
    assert!(before.session.needs_review == 1);
    assert!(state.recovery().requires_recovery());

    state
        .discard_session(env_update(&epoch, 0), discard_request("s-paused"))
        .expect("paused + 待确认的出口是 discard_session");

    let db = state.db();
    let after = session(db, "s-paused");
    assert_eq!(after.state, "discarded");
    assert_eq!(after.needs_review, 0);
    let cand = interval(db, "s-paused-cand");
    assert_eq!(cand.voided_at, Some(WALL));
    assert_eq!(cand.needs_review, 0);
    assert_eq!(cand.ended_at, None, "无时长的候选不能留下假终点");
    assert!(!state.recovery().requires_recovery(), "待确认事实已消掉");
}

/// **作废运行中的会话**（本 run、镜像那条）：提交后门禁重扫 + 按条件刷新镜像，
/// 镜像停在 `discarded`——与 `finish` 停在 `finished` **完全同一口径**（R8，`live`
/// 只表示「本 run 最后装载过哪条会话」，不是「正在计时」）。
/// 界面口径也跟着对：不再有活动暂计、不再有待确认时长，前台槽位随之空出。
#[test]
fn discarding_the_running_session_leaves_the_mirror_on_discarded() {
    let fx = fixture();
    let db = seeded(&fx);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let task_v = task_version(state.db());
    state.start(start_request(&epoch, task_v)).unwrap();
    let live = state.coordinator().live().expect("start 装载镜像").clone();
    assert_eq!(live.state, SessionState::Running);
    let session_id = live.id.clone();
    // 会话的开放区间（作废之后要核对它的起点没被改、终点仍是 NULL）。
    let interval_id: String = state
        .db()
        .connection()
        .query_row(
            "SELECT id FROM work_interval
              WHERE session_id = ?1 AND ended_at IS NULL AND voided_at IS NULL",
            [&session_id],
            |r| r.get(0),
        )
        .unwrap();
    let open_started_at = interval(state.db(), &interval_id).started_at;

    state
        .discard_session(
            env_update(&epoch, live.row_version),
            discard_request(&session_id),
        )
        .expect("作废运行中的会话");

    {
        let live = state.coordinator().live().expect("镜像还在");
        assert_eq!(live.id, session_id);
        assert_eq!(
            live.state,
            SessionState::Discarded,
            "作废后镜像停在 discarded（与 finish 停 finished 同形）"
        );
    }

    let snapshot = state.snapshot().unwrap();
    assert_eq!(snapshot.state, Some(SessionState::Discarded));
    assert_eq!(snapshot.active_ms, 0, "不得再按 running 计暂计");
    assert_eq!(snapshot.pending_ms, None, "待确认段已作废");

    let db = state.db();
    let facts = interval(db, &interval_id);
    assert_eq!(facts.started_at, open_started_at);
    assert_eq!(facts.voided_at, Some(WALL));
    assert_eq!(facts.ended_at, None, "未知终点保持 NULL");
    assert_eq!(session(db, &session_id).state, "discarded");
    assert_eq!(session(db, &session_id).ended_at, Some(WALL));

    // 前台槽位空出来了。
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM work_session WHERE mode='FOREGROUND' AND state='running'"
        ),
        0
    );
    let task_v = task_version(state.db());
    state
        .start(start_request(&epoch, task_v))
        .expect("作废之后可以开始新计时");
}

/// **R13 的反向**：作废一条**不是**镜像的会话时不得抢走 `live`。
///
/// 与 `tests/reconcile.rs::reconciling_another_session_does_not_clobber_the_live_mirror`
/// 同形：无条件 `load_session`（Ruling 13 明文禁止、Task 2 犯过的那一版）会把 `live`
/// 换到那条刚被作废的会话上，而 `load_session` 对 `discarded` 会话不报错、`active_ms`
/// 也照样是 0——所以这条断言必须显式钉住「镜像还是 A，且还是 `running`」。
#[test]
fn discarding_another_session_does_not_clobber_the_live_mirror() {
    let fx = fixture();
    let db = seeded(&fx);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let run = running.run_id().to_string();

    // 本 run 的 A：协调器镜像的就是它。
    let task_v = task_version(state.db());
    state.start(start_request(&epoch, task_v)).unwrap();
    let live_id = state.coordinator().live().unwrap().id.clone();
    assert_eq!(
        state.coordinator().live().unwrap().state,
        SessionState::Running
    );

    // 同一个 run 里另一条可作废的 B（本 run 的会话不是门禁材料，所以不挡计时）：
    // 这里手工造它的理由与 Task 2 的同形用例一致——它只需要「一条能被作废的会话」。
    insert_session(
        state.db(),
        "s-b",
        &run,
        "FOREGROUND",
        "recovering",
        WALL - 2_000,
        None,
        1,
    );
    insert_interval(
        state.db(),
        "s-b-cand",
        "s-b",
        WALL - 2_000,
        Some(WALL - 1_000),
        None,
        1,
        None,
    );

    state
        .discard_session(env_update(&epoch, 0), discard_request("s-b"))
        .expect("作废别的会话");

    // 命令本身生效了：B 整次作废。
    let db = state.db();
    assert_eq!(session(db, "s-b").state, "discarded");
    assert_eq!(interval(db, "s-b-cand").voided_at, Some(WALL));

    // 而镜像必须原样停在 A 上：既没被抢走，也没被换成已作废的 B。
    {
        let live = state.coordinator().live().expect("镜像还在");
        assert_eq!(live.id, live_id, "作废别的会话不得把 live 换成它");
        assert_eq!(
            live.state,
            SessionState::Running,
            "正在计时那条的镜像必须原样保留"
        );
    }
    let snapshot = state.snapshot().unwrap();
    assert_eq!(
        snapshot.session_id.as_deref(),
        Some(live_id.as_str()),
        "快照仍描述正在计时那条"
    );
    assert_eq!(snapshot.state, Some(SessionState::Running));
}

/// 重复作废（会话已经是 `discarded`、区间都已作废）⇒ `Unchanged`：
/// 不写审计、不加 `revision`、不加 `row_version`、不移动 `ended_at`。
#[test]
fn discarding_an_already_discarded_session_changes_nothing() {
    let fx = fixture();
    let db = seeded(&fx);
    recovering_fixture(&db, "s-rec");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    state
        .discard_session(env_update(&epoch, 0), discard_request("s-rec"))
        .unwrap();
    let after_first = session_world(state.db(), "s-rec");

    let (report, changed) = state
        .discard_session(env_update(&epoch, 1), discard_request("s-rec"))
        .unwrap()
        .into_parts();
    assert!(!changed, "第二次作废没有可改的事实");
    assert_eq!(report.session.state, SessionState::Discarded);
    assert_eq!(
        session_world(state.db(), "s-rec"),
        after_first,
        "零写入、零版本、零审计"
    );
}

/// **失败整体回滚**：作废循环与审计同一事务——审计写不进去时，
/// 已经作废的区间、已经改过的会话版本都必须退回去。
#[test]
fn a_failed_discard_rolls_back_every_voided_interval() {
    let fx = fixture();
    let db = seeded(&fx);
    recovering_fixture(&db, "s-rec");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = session_world(state.db(), "s-rec");

    reject_time_edit_writes(state.db());
    let error = state
        .discard_session(env_update(&epoch, 0), discard_request("s-rec"))
        .unwrap_err();
    assert_code(&error, "STORAGE_ERROR");
    assert_eq!(
        session_world(state.db(), "s-rec"),
        before,
        "三条区间的作废与会话状态都必须回滚"
    );

    allow_time_edit_writes(state.db());
    state
        .discard_session(env_update(&epoch, 0), discard_request("s-rec"))
        .expect("注入解除之后必须成功");
}

/// 版本过期 / 会话不存在 / 没有区间：三种拒绝都不写任何东西。
///
/// 「没有区间」是**防御分支**：`HistoryEditReport.interval` 是必填的（P8 要展示被作废
/// 的那一段），一条区间都没有的会话无法回答「作废了哪一段」，所以显式拒绝而不是造一行。
#[test]
fn discard_session_refuses_a_stale_version_an_unknown_session_and_an_empty_session() {
    let fx = fixture();
    let db = seeded(&fx);
    recovering_fixture(&db, "s-rec");
    // 手工造的边缘事实：一条区间都没有的 `paused` 会话。
    insert_session(
        &db,
        "s-bare",
        OLD_RUN,
        "FOREGROUND",
        "paused",
        P_START,
        None,
        0,
    );
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = session_world(state.db(), "s-rec");

    assert_code(
        &state
            .discard_session(env_update(&epoch, 99), discard_request("s-rec"))
            .unwrap_err(),
        "VERSION_CONFLICT",
    );
    assert_code(
        &state
            .discard_session(env_update(&epoch, 0), discard_request("nope"))
            .unwrap_err(),
        "DOMAIN_ERROR",
    );
    let bare_version = session(state.db(), "s-bare").row_version;
    let error = state
        .discard_session(env_update(&epoch, bare_version), discard_request("s-bare"))
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");

    assert_eq!(
        session_world(state.db(), "s-rec"),
        before,
        "三次被拒都是零变化"
    );
}
