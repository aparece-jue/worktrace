//! P3 Task 3：`correct`——仅 `finished` 会话的可信历史修正（重定时 / 软删除）。
//!
//! 装置抄 `tests/reconcile.rs`：造一个「上一个 run 留下事实」的库，走**真实**的
//! `bootstrap::startup`，然后在同一把锁（`lock_app`）里调 `AppState::correct`。
//!
//! 覆盖（Task 3 的测试清单）：
//! 负区间被拒 · 与同会话/其他会话的有效人工区间重叠被拒（含端点相接**不算**重叠、
//! 机器时间不参与互斥、`exclude_interval` 不排除自己时的反例）· `ended_at > now` 被拒 ·
//! 删除是软删除（区间行仍在、审计留存、`task_change` 不动）· `recovering` 被拒 ·
//! 修正后 `duration_ms` 与起止一致 · **幂等重复零变化**（`Unchanged`，不写审计、
//! 不加 `revision`、不加 `row_version`）· 被拒时**逐字段**比对（状态、版本、起止、时长、
//! `needs_review`、`voided_at`、`revision`、`time_edit`/`task_change` 行数）·
//! 并发保护用**所属会话版本**（换一条区间的旧版本请求也拒绝）·
//! 提交后只刷新**正镜像**的那条会话。

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
use worktrace_lib::services::history::{CorrectAction, CorrectRequest};
use worktrace_lib::services::timer::coordinator::{SessionRequest, StartRequest};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::run_repo;
use worktrace_lib::storage::session_repo;

/// 启动那一刻的挂钟（`FakeClock` 不自己走，所以 `now` 恒等于它）。
const WALL: i64 = 1_700_000_000_000;
/// 崩掉的那一代次。
const OLD_RUN: &str = "run-old";
/// 任务完成事件的时刻（`task_change` 里的那一条）。
const T_DONE: i64 = WALL - 30_000;

// 会话与区间的几何（都落在 `WALL` 之前，彼此留了空隙）。
const A_START: i64 = WALL - 20_000;
const A_END: i64 = WALL - 18_000;
const B_START: i64 = WALL - 17_000;
const B_END: i64 = WALL - 16_000;
const S2_START: i64 = WALL - 15_000;
const S2_END: i64 = WALL - 14_000;
const S3_START: i64 = WALL - 13_000;
const S3_END: i64 = WALL - 12_000;

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

/// 真跑一次启动（第 ④ 步的扫描会归一崩溃段；本文件的夹具都是干净事实）。
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

/// 建库：迁移 + 元数据 + 上一代 run + 一条 `Doing` 任务 + 一条已完成事件。
fn seeded(fx: &Fixture) -> Db {
    let mut db = Db::open(&fx.db_path).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    run_repo::start_run(&tx, OLD_RUN, WALL - 60_000).unwrap();
    tx.execute(
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Doing',0,1000,1000)",
        [],
    )
    .unwrap();
    // 任务的完成事件：`correct` **不得**移动它（02 §10：完成时刻按 `task_change` 选，
    // 不用 `updated_at`、也不用会话结束时间）。
    tx.execute(
        "INSERT INTO task_change(id,task_id,before_json,after_json,created_at)
         VALUES('tc-done','t1','{\"status\":\"Doing\"}','{\"status\":\"Done\"}',?1)",
        rusqlite::params![T_DONE],
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

/// 三条干净的 `finished` 会话：`s1`（两条可信区间）、`s2`（另一条人工会话）、
/// `s3`（机器模式会话）。它们都**不是**门禁材料，所以启动后门禁是开的。
fn finished_fixture(db: &Db) {
    insert_session(
        db,
        "s1",
        OLD_RUN,
        "FOREGROUND",
        "finished",
        A_START,
        Some(B_END),
        0,
    );
    insert_trusted_interval(db, "s1-a", "s1", A_START, A_END);
    insert_trusted_interval(db, "s1-b", "s1", B_START, B_END);

    insert_session(
        db,
        "s2",
        OLD_RUN,
        "FOREGROUND",
        "finished",
        S2_START,
        Some(S2_END),
        0,
    );
    insert_trusted_interval(db, "s2-a", "s2", S2_START, S2_END);

    insert_session(
        db,
        "s3",
        OLD_RUN,
        "BACKGROUND",
        "finished",
        S3_START,
        Some(S3_END),
        0,
    );
    insert_trusted_interval(db, "s3-a", "s3", S3_START, S3_END);
}

// ── 读事实 ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionFacts {
    state: String,
    run_id: String,
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

/// 「逐字段比对」用的整体快照：会话 + 该会话全部区间 + 版本 + 两种审计的行数。
///
/// 被拒命令的零变化用 `assert_eq!(facts(db, "s1"), before)` 一句话比对**每个字段**，
/// 而不是只比行数（总纲 §5 第 8 条的口径）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Facts {
    session: SessionFacts,
    intervals: Vec<(String, IntervalFacts)>,
    revision: i64,
    time_edit_rows: i64,
    task_change_rows: i64,
    /// 最近一条 `task_change` 的 `(before_json, after_json, created_at)`。
    last_task_change: Option<(String, String, i64)>,
}

fn session(db: &Db, id: &str) -> SessionFacts {
    db.connection()
        .query_row(
            "SELECT state, run_id, started_at, ended_at, needs_review, row_version
               FROM work_session WHERE id = ?1",
            [id],
            |r| {
                Ok(SessionFacts {
                    state: r.get(0)?,
                    run_id: r.get(1)?,
                    started_at: r.get(2)?,
                    ended_at: r.get(3)?,
                    needs_review: r.get(4)?,
                    row_version: r.get(5)?,
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

fn facts(db: &Db, session_id: &str) -> Facts {
    let ids: Vec<String> = {
        let mut stmt = db
            .connection()
            .prepare("SELECT id FROM work_interval WHERE session_id = ?1 ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map([session_id], |r| r.get::<_, String>(0))
            .unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    };
    Facts {
        session: session(db, session_id),
        intervals: ids
            .into_iter()
            .map(|id| {
                let f = interval(db, &id);
                (id, f)
            })
            .collect(),
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

fn scalar(db: &Db, sql: &str) -> i64 {
    db.connection().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn revision(db: &Db) -> i64 {
    scalar(db, "SELECT revision FROM app_meta WHERE singleton = 1")
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

fn retime(session_id: &str, interval_id: &str, started_at: i64, ended_at: i64) -> CorrectRequest {
    CorrectRequest {
        session_id: session_id.to_string(),
        interval_id: interval_id.to_string(),
        action: CorrectAction::Retime {
            started_at,
            ended_at,
        },
        reason: None,
    }
}

fn delete(session_id: &str, interval_id: &str) -> CorrectRequest {
    CorrectRequest {
        session_id: session_id.to_string(),
        interval_id: interval_id.to_string(),
        action: CorrectAction::Delete,
        reason: None,
    }
}

fn env(epoch: &str, row_version: i64) -> WriteEnvelope {
    WriteEnvelope::for_update(epoch, row_version)
}

fn start_request(epoch: &str) -> StartRequest {
    StartRequest {
        expected_data_epoch: epoch.to_string(),
        task_id: "t1".into(),
        task_expected_version: 0,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: 1_000,
    }
}

fn finish_request(epoch: &str, session_id: &str, version: i64) -> SessionRequest {
    SessionRequest {
        expected_data_epoch: epoch.to_string(),
        session_id: session_id.to_string(),
        session_expected_version: version,
    }
}

fn assert_code(error: &AppError, code: &str) {
    assert_eq!(error.code(), code, "实际：{error:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 重定时：一次改三件（起止 + 时长）
// ─────────────────────────────────────────────────────────────────────────────

/// `Retime` 同时改 `started_at`/`ended_at`/`duration_ms`，并且**不动**会话的
/// `ended_at`、**不动** `task_change`（完成时刻不因修正区间而移动）。
#[test]
fn retiming_a_finished_interval_rewrites_start_end_and_duration_together() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    assert!(
        !state.recovery().requires_recovery(),
        "夹具里没有恢复材料，门禁应当是开的（correct 也不该被门禁挡住）"
    );

    let before = facts(state.db(), "s1");
    let (new_start, new_end) = (WALL - 16_800, WALL - 15_300);
    let outcome = state
        .correct(env(&epoch, 0), retime("s1", "s1-b", new_start, new_end))
        .expect("修正可信历史应当成功");
    let (report, changed) = outcome.into_parts();
    assert!(changed, "重定时是一笔真实写入");

    // 报告里的区间是写入后的权威行。
    assert_eq!(report.interval.started_at, new_start);
    assert_eq!(report.interval.ended_at, Some(new_end));
    assert_eq!(report.interval.duration_ms, Some(new_end - new_start));
    assert_eq!(report.session.state, SessionState::Finished);
    assert_eq!(report.session.row_version, 1, "会话版本 +1（并发保护用它）");
    assert_eq!(report.revision, before.revision + 1);

    let db = state.db();
    let after = facts(db, "s1");
    assert_eq!(
        interval(db, "s1-b"),
        IntervalFacts {
            session_id: "s1".into(),
            started_at: new_start,
            ended_at: Some(new_end),
            duration_ms: Some(new_end - new_start),
            sampled_end_wall_at: None,
            needs_review: 0,
            voided_at: None,
        }
    );
    // 同会话的另一段与整条会话的别的字段都不动；只有 `row_version` +1。
    assert_eq!(interval(db, "s1-a"), before.intervals[0].1);
    let mut expected_session = before.session.clone();
    expected_session.row_version += 1;
    assert_eq!(session(db, "s1"), expected_session);
    assert_eq!(
        session(db, "s1").ended_at,
        Some(B_END),
        "会话的 ended_at 记录的是「结束那一刻」，不因修正区间而移动"
    );
    assert_eq!(
        after.last_task_change, before.last_task_change,
        "已完成任务的完成时刻不因修正区间而移动"
    );
    assert_eq!(after.task_change_rows, before.task_change_rows);
    assert_eq!(
        after.revision,
        before.revision + 1,
        "一次用户命令恰好加一次 revision"
    );
    assert_eq!(after.time_edit_rows, before.time_edit_rows + 1);
}

/// 审计：`before_json` 留旧值、`after_json` 留新值，逐字段可查；
/// `reason = correct:retime`；端点是**用户给定值**，所以**没有** `candidate_end_source`
/// （Ruling 8）。
#[test]
fn the_audit_keeps_the_values_before_and_after_a_retime() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let (new_start, new_end) = (WALL - 16_800, WALL - 15_300);
    let mut request = retime("s1", "s1-b", new_start, new_end);
    request.reason = Some("  用户补记：会议拖了半小时  ".into());
    state.correct(env(&epoch, 0), request).unwrap();

    let db = state.db();
    let rows = edits(db, "s1");
    assert_eq!(rows.len(), 1);
    let (change, before, after, reason) = &rows[0];
    assert_eq!(change, "correct_retime");
    assert_eq!(reason.as_deref(), Some("correct:retime"));

    let old = audit_interval(before, "s1-b");
    assert_eq!(old["started_at"], B_START);
    assert_eq!(old["ended_at"], B_END);
    assert_eq!(old["duration_ms"], B_END - B_START);
    assert_eq!(old["voided_at"], serde_json::Value::Null);

    let new = audit_interval(after, "s1-b");
    assert_eq!(new["started_at"], new_start);
    assert_eq!(new["ended_at"], new_end);
    assert_eq!(new["duration_ms"], new_end - new_start);

    // 未被改动的另一段两侧都在审计里，且值相同。
    assert_eq!(
        audit_interval(before, "s1-a"),
        audit_interval(after, "s1-a")
    );

    assert!(
        after.get("candidate_end").is_none() && after.get("candidate_end_source").is_none(),
        "用户给定的端点不是候选推导，不写这两个键：{after}"
    );
    assert_eq!(
        after["user_reason"], "用户补记：会议拖了半小时",
        "用户理由去空白后留在 after_json 里"
    );
    assert!(
        before.get("user_reason").is_none(),
        "改动前的事实里没有这条理由"
    );
    // 会话两侧的 row_version 是 0 → 1。
    assert_eq!(before["session"]["row_version"], 0);
    assert_eq!(after["session"]["row_version"], 1);
}

/// 幂等重复（04 §9 用例 3）：新起止与现值逐字段相同 ⇒ `Unchanged`——
/// 不写审计、不加 `revision`、不加 `row_version`，且返回当前行。
#[test]
fn repeating_the_same_retime_changes_nothing() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let first = state
        .correct(
            env(&epoch, 0),
            retime("s1", "s1-b", WALL - 16_800, WALL - 15_300),
        )
        .unwrap();
    assert!(first.into_parts().1, "第一次是真实写入");
    let before = facts(state.db(), "s1");

    // 第二次：同一个会话版本（第一次之后是 1）、同一组起止。
    let second = state
        .correct(
            env(&epoch, 1),
            retime("s1", "s1-b", WALL - 16_800, WALL - 15_300),
        )
        .expect("幂等重复不是错误");
    let (report, changed) = second.into_parts();
    assert!(!changed, "逐字段相同 ⇒ Unchanged");
    assert_eq!(report.interval.started_at, WALL - 16_800);
    assert_eq!(report.interval.ended_at, Some(WALL - 15_300));
    assert_eq!(report.interval.duration_ms, Some(1_500));
    assert_eq!(report.session.row_version, 1, "报告里是当前行（版本没动）");
    assert_eq!(report.revision, before.revision);

    assert_eq!(
        facts(state.db(), "s1"),
        before,
        "零变化：状态、版本、起止、时长、needs_review、voided_at、revision、审计行数"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 重叠（S7）：同一规则、端点相接不算、机器时间不参与
// ─────────────────────────────────────────────────────────────────────────────

/// 与**同会话**的另一段有效人工区间重叠 ⇒ 整条拒绝、零变化。
#[test]
fn a_retime_that_overlaps_another_interval_of_the_same_session_is_rejected() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = facts(state.db(), "s1");

    let error = state
        .correct(
            env(&epoch, 0),
            retime("s1", "s1-b", A_END - 500, B_END - 500),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(
        facts(state.db(), "s1"),
        before,
        "被拒命令逐字段零变化（含 revision 与两种审计行数）"
    );
}

/// 与**其他会话**的有效人工区间重叠同样拒绝（跨会话口径）。
#[test]
fn a_retime_that_overlaps_another_sessions_interval_is_rejected() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = facts(state.db(), "s1");
    let other_before = interval(state.db(), "s2-a");

    let error = state
        .correct(
            env(&epoch, 0),
            retime("s1", "s1-b", S2_START + 500, S2_END + 500),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s1"), before);
    assert_eq!(interval(state.db(), "s2-a"), other_before, "别的会话不动");
}

/// 端点相接**不算**重叠：贴着邻居的边界重定时是合法的（半开区间）。
#[test]
fn a_retime_that_only_touches_a_neighbouring_interval_is_allowed() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    // 紧贴同会话前一段的右端点：[A_END, B_START)。
    let outcome = state
        .correct(env(&epoch, 0), retime("s1", "s1-b", A_END, B_START))
        .expect("端点相接不算重叠");
    assert!(outcome.into_parts().1);
    assert_eq!(interval(state.db(), "s1-b").started_at, A_END);
    assert_eq!(
        interval(state.db(), "s1-b").duration_ms,
        Some(B_START - A_END)
    );

    // 紧贴**别的会话**区间的右端点。
    let outcome = state
        .correct(env(&epoch, 1), retime("s1", "s1-b", S2_END, S2_END + 500))
        .expect("端点相接不算重叠（跨会话同一规则）");
    assert!(outcome.into_parts().1);
    assert_eq!(interval(state.db(), "s1-b").started_at, S2_END);
}

/// 机器模式（`BACKGROUND`/`PASSIVE`/`WAITING`）按独立口径，**不参与**人工时间的互斥。
#[test]
fn machine_time_does_not_block_a_retime() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    // 目标区间与 s3（BACKGROUND）的区间整段重叠。
    let outcome = state
        .correct(env(&epoch, 0), retime("s1", "s1-b", S3_START, S3_END))
        .expect("机器时间不参与人工互斥");
    assert!(outcome.into_parts().1);
    assert_eq!(
        interval(state.db(), "s1-b").duration_ms,
        Some(S3_END - S3_START)
    );
}

/// `exclude_interval` 的反例：**不**排除自己时，用自己当前的起止做重叠校验必然命中自己
/// ——这正是 `correct` 必须传 `Some(该区间自身)` 的原因。仓储级钉住。
#[test]
fn the_overlap_check_would_collide_with_the_interval_itself_without_the_exclusion() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);

    let tx = db.connection().unchecked_transaction().unwrap();
    let error = session_repo::require_no_human_overlap(&tx, B_START, B_END, None).unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    // 排除自己之后同一组起止合法。
    session_repo::require_no_human_overlap(&tx, B_START, B_END, Some("s1-b")).unwrap();
    drop(tx);

    // 服务层的重定时用的就是「排除自己」那一支：改成自己的现值仍然成功（幂等）。
    drop(db);
    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    state
        .correct(env(&epoch, 0), retime("s1", "s1-b", B_START, B_END))
        .expect("自己与自己不算重叠");
}

// ─────────────────────────────────────────────────────────────────────────────
// 请求级校验：负区间、未来端点
// ─────────────────────────────────────────────────────────────────────────────

/// 负区间被拒（`ended_at < started_at`），零变化。
#[test]
fn a_negative_retime_is_rejected() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = facts(state.db(), "s1");

    let error = state
        .correct(env(&epoch, 0), retime("s1", "s1-b", B_END, B_START))
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s1"), before);
}

/// `ended_at > now`（未来的「已发生工时」不是事实）被拒，零变化。
#[test]
fn a_retime_that_ends_in_the_future_is_rejected() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = facts(state.db(), "s1");

    let error = state
        .correct(env(&epoch, 0), retime("s1", "s1-b", WALL - 1_000, WALL + 1))
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s1"), before);
}

// ─────────────────────────────────────────────────────────────────────────────
// 删除 = 软删除
// ─────────────────────────────────────────────────────────────────────────────

/// 删除是**软删除**：区间行仍在（`voided_at` 置位）、审计留存、会话与 `task_change` 不动。
#[test]
fn deleting_an_interval_is_a_soft_delete_that_keeps_the_row_and_the_audit() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = facts(state.db(), "s1");

    let outcome = state
        .correct(env(&epoch, 0), delete("s1", "s1-b"))
        .expect("删除误记应当成功");
    let (report, changed) = outcome.into_parts();
    assert!(changed);
    assert_eq!(report.interval.voided_at, Some(WALL));
    assert_eq!(
        report.interval.ended_at,
        Some(B_END),
        "时长已知的区间保留原始起止"
    );

    let db = state.db();
    let after = facts(db, "s1");
    // 行还在，只是作废了：起止与时长原样保留。
    assert_eq!(
        interval(db, "s1-b"),
        IntervalFacts {
            session_id: "s1".into(),
            started_at: B_START,
            ended_at: Some(B_END),
            duration_ms: Some(B_END - B_START),
            sampled_end_wall_at: None,
            needs_review: 0,
            voided_at: Some(WALL),
        }
    );
    assert_eq!(interval(db, "s1-a"), before.intervals[0].1, "别的区间不动");
    assert_eq!(session(db, "s1").ended_at, Some(B_END), "会话结束时刻不动");
    assert_eq!(session(db, "s1").row_version, 1);
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.time_edit_rows, before.time_edit_rows + 1);
    assert_eq!(after.last_task_change, before.last_task_change);
    assert_eq!(after.task_change_rows, before.task_change_rows);

    let rows = edits(db, "s1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "correct_delete");
    assert_eq!(rows[0].3.as_deref(), Some("correct:delete"));
    assert_eq!(
        audit_interval(&rows[0].1, "s1-b")["voided_at"],
        serde_json::Value::Null
    );
    assert_eq!(audit_interval(&rows[0].2, "s1-b")["voided_at"], WALL);
}

/// 删除之后不能再用 `correct` 修它（已作废的只能在审计里看）——这正是
/// 「作废整次/删除之后走恢复流程」那一类事实。
#[test]
fn a_voided_interval_cannot_be_corrected_again() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    state.correct(env(&epoch, 0), delete("s1", "s1-b")).unwrap();
    let before = facts(state.db(), "s1");

    let error = state
        .correct(env(&epoch, 1), retime("s1", "s1-b", B_START, B_END))
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(facts(state.db(), "s1"), before);
}

// ─────────────────────────────────────────────────────────────────────────────
// 前置：只接 finished；区间必须是该会话的可信闭合区间
// ─────────────────────────────────────────────────────────────────────────────

/// `recovering` 的区间编辑走 `reconcile`，不通过 `correct`。
#[test]
fn a_recovering_session_is_rejected_and_points_at_reconcile() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    // P2 的异常形态：待确认区间 + 会话 recovering。
    insert_session(
        &db,
        "s-rec",
        OLD_RUN,
        "FOREGROUND",
        "recovering",
        WALL - 10_000,
        None,
        1,
    );
    insert_interval(
        &db,
        "s-rec-iv",
        "s-rec",
        WALL - 10_000,
        Some(WALL - 9_000),
        None,
        1,
        None,
    );
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = facts(state.db(), "s-rec");

    let error = state
        .correct(
            env(&epoch, 0),
            retime("s-rec", "s-rec-iv", WALL - 9_500, WALL - 9_400),
        )
        .unwrap_err();
    // 唯一指向恢复流程的码：确认/丢弃不确定区间走 reconcile。
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(facts(state.db(), "s-rec"), before);
}

/// `running`/`paused` 想改可信历史必须先 `finish`；`discarded` 的整次记录不是可信历史。
#[test]
fn only_finished_sessions_can_be_corrected() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    insert_session(
        &db,
        "s-disc",
        OLD_RUN,
        "FOREGROUND",
        "discarded",
        WALL - 8_000,
        Some(WALL - 7_000),
        0,
    );
    insert_interval(
        &db,
        "s-disc-iv",
        "s-disc",
        WALL - 8_000,
        Some(WALL - 7_000),
        Some(1_000),
        0,
        Some(WALL - 7_000),
    );
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    // 本 run 里真开一条：running。
    state.start(start_request(&epoch)).unwrap();
    let live_id = state.coordinator().live().unwrap().id.clone();
    let version = session(state.db(), &live_id).row_version;
    let open_id: String = state
        .db()
        .connection()
        .query_row(
            "SELECT id FROM work_interval WHERE session_id = ?1 AND ended_at IS NULL",
            [&live_id],
            |r| r.get(0),
        )
        .unwrap();
    let before = facts(state.db(), &live_id);
    let error = state
        .correct(
            env(&epoch, version),
            retime(&live_id, &open_id, WALL - 1_000, WALL),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), &live_id), before, "running：零变化");

    // 暂停之后同样要先结束（`paused` 不是终态）。
    state
        .pause(SessionRequest {
            expected_data_epoch: epoch.clone(),
            session_id: live_id.clone(),
            session_expected_version: version,
        })
        .unwrap();
    let version = session(state.db(), &live_id).row_version;
    let before = facts(state.db(), &live_id);
    let error = state
        .correct(env(&epoch, version), delete(&live_id, &open_id))
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), &live_id), before, "paused：零变化");
    assert_eq!(
        state.coordinator().live().unwrap().state,
        SessionState::Paused,
        "被拒命令不动协调器镜像"
    );

    // `discarded`：整次作废的记录只在历史与审计里可查。
    let before = facts(state.db(), "s-disc");
    let error = state
        .correct(env(&epoch, 0), delete("s-disc", "s-disc-iv"))
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s-disc"), before);
}

/// 目标区间必须**属于该会话**、未作废、已确认；否则走恢复流程（`RECOVERY_REQUIRED`）。
#[test]
fn an_interval_that_is_not_a_trusted_one_of_that_session_is_rejected() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    // s1 上再挂一条待确认区间（P2/P3 的候选端点形态）。
    insert_interval(&db, "s1-pending", "s1", B_END, Some(B_END), None, 1, None);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    // ① 别人的区间：会话不匹配。
    let before = facts(state.db(), "s1");
    let error = state
        .correct(env(&epoch, 0), delete("s1", "s2-a"))
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(facts(state.db(), "s1"), before);

    // ② 待确认的候选端点不是事实。
    let error = state
        .correct(
            env(&epoch, 0),
            retime("s1", "s1-pending", B_END, B_END + 100),
        )
        .unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(facts(state.db(), "s1"), before);

    // ③ 不存在的区间：找不到就是找不到（DOMAIN_ERROR），不是「去恢复」。
    let error = state
        .correct(env(&epoch, 0), delete("s1", "s1-nope"))
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s1"), before);

    // ④ 不存在的会话。
    let error = state
        .correct(env(&epoch, 0), delete("s-nope", "s1-b"))
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s1"), before);
}

/// 缺版本、epoch 不匹配、旧版本：三条都在写之前拒绝。
#[test]
fn missing_version_or_a_stale_epoch_or_version_is_rejected_without_writing() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = facts(state.db(), "s1");

    let error = state
        .correct(
            WriteEnvelope::for_create(&epoch),
            retime("s1", "s1-b", WALL - 16_800, WALL - 15_300),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");

    let error = state
        .correct(
            env("epoch-of-another-database", 0),
            retime("s1", "s1-b", WALL - 16_800, WALL - 15_300),
        )
        .unwrap_err();
    assert_code(&error, "DATA_EPOCH_MISMATCH");

    let error = state
        .correct(
            env(&epoch, 7),
            retime("s1", "s1-b", WALL - 16_800, WALL - 15_300),
        )
        .unwrap_err();
    assert_code(&error, "VERSION_CONFLICT");

    assert_eq!(facts(state.db(), "s1"), before);
}

/// 并发保护用**所属会话版本**：改完一条之后，另一条区间的旧版本请求也要拒绝
/// （会话版本是这一族命令的共同版本位）。
#[test]
fn a_stale_version_is_refused_even_when_the_request_targets_another_interval() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    state
        .correct(
            env(&epoch, 0),
            retime("s1", "s1-b", WALL - 16_800, WALL - 15_300),
        )
        .unwrap();
    let before = facts(state.db(), "s1");

    let error = state
        .correct(env(&epoch, 0), retime("s1", "s1-a", A_START, A_END - 500))
        .unwrap_err();
    assert_code(&error, "VERSION_CONFLICT");
    assert_eq!(facts(state.db(), "s1"), before, "旧版本请求零变化");

    // 刷新到当前版本后可以继续改另一条。
    let version = session(state.db(), "s1").row_version;
    let outcome = state
        .correct(
            env(&epoch, version),
            retime("s1", "s1-a", A_START, A_END - 500),
        )
        .unwrap();
    assert!(outcome.into_parts().1);
    assert_eq!(
        interval(state.db(), "s1-a").duration_ms,
        Some(A_END - 500 - A_START)
    );
    assert_eq!(session(state.db(), "s1").row_version, version + 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// 仓储原语（S8 的第三条）：只允许已确认且未作废的区间
// ─────────────────────────────────────────────────────────────────────────────

/// `retime_interval` 自己的契约：待确认的候选端点、已作废的行一律拒绝，且零写入。
#[test]
fn the_retime_primitive_refuses_pending_and_voided_rows() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    insert_interval(&db, "s1-pending", "s1", B_END, Some(B_END), None, 1, None);

    let tx = db.connection().unchecked_transaction().unwrap();

    let error = session_repo::retime_interval(&tx, "s1-pending", B_END, B_END + 1, 1).unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    let error = session_repo::retime_interval(&tx, "s1-nope", B_END, B_END + 1, 1).unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");

    // 已作废的行（先软删除，再试图重定时）。
    let voided = session_repo::void_interval(&tx, "s1-b", WALL).unwrap();
    assert_eq!(voided.voided_at, Some(WALL));
    let error = session_repo::retime_interval(&tx, "s1-b", B_START, B_END - 100, 100).unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");

    // 可信闭合的行：三个字段一起变，且时长与起止一致。
    let retimed =
        session_repo::retime_interval(&tx, "s1-a", A_START + 10, A_END, A_END - A_START - 10)
            .unwrap();
    assert_eq!(retimed.started_at, A_START + 10);
    assert_eq!(retimed.ended_at, Some(A_END));
    assert_eq!(retimed.duration_ms, Some(A_END - A_START - 10));
    assert_eq!(
        retimed.duration_ms,
        Some(retimed.ended_at.unwrap() - retimed.started_at)
    );
    tx.commit().unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────
// 提交后的镜像刷新（计划「新增-2」+ Ruling 13）：只刷新正镜像的那条
// ─────────────────────────────────────────────────────────────────────────────

/// 被修正的会话**正是**镜像那条时，必须按已提交事实刷新它（`finish` 之后
/// `live` 仍停在那条 `finished` 会话上，所以这条真的会命中）。
#[test]
fn correcting_the_mirrored_session_refreshes_its_live_state() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    state.start(start_request(&epoch)).unwrap();
    let live_id = state.coordinator().live().unwrap().id.clone();
    let version = session(state.db(), &live_id).row_version;
    let open_id: String = state
        .db()
        .connection()
        .query_row(
            "SELECT id FROM work_interval WHERE session_id = ?1 AND ended_at IS NULL",
            [&live_id],
            |r| r.get(0),
        )
        .unwrap();
    state
        .finish(finish_request(&epoch, &live_id, version))
        .unwrap();
    assert_eq!(
        state.coordinator().live().unwrap().state,
        SessionState::Finished,
        "finish 之后 live 仍停在那条已结束的会话上"
    );
    let version = session(state.db(), &live_id).row_version;

    state
        .correct(
            env(&epoch, version),
            retime(&live_id, &open_id, WALL - 3_000, WALL - 1_000),
        )
        .unwrap();

    let live = state.coordinator().live().expect("镜像还在");
    assert_eq!(live.id, live_id);
    assert_eq!(live.state, SessionState::Finished);
    assert_eq!(live.row_version, version + 1, "镜像按已提交事实刷新");
}

/// 修正**别的**会话时不得抢走 `live`。
#[test]
fn correcting_another_session_does_not_clobber_the_live_mirror() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    state.start(start_request(&epoch)).unwrap();
    let live_id = state.coordinator().live().unwrap().id.clone();
    let open_version = session(state.db(), &live_id).row_version;
    state
        .finish(finish_request(&epoch, &live_id, open_version))
        .unwrap();
    let live_id = state.coordinator().live().unwrap().id.clone();
    let live_version = state.coordinator().live().unwrap().row_version;

    // 修正夹具里的 s1：它**不是**镜像那条。
    state
        .correct(
            env(&epoch, 0),
            retime("s1", "s1-b", WALL - 16_800, WALL - 15_300),
        )
        .unwrap();

    let live = state.coordinator().live().expect("镜像还在");
    assert_eq!(live.id, live_id, "修正别的会话不得把 live 换成它");
    assert_eq!(live.row_version, live_version, "镜像那条一个字段都没动");
    assert_eq!(session(state.db(), "s1").row_version, 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// 不重扫门禁（计划原文）与零长度边界
// ─────────────────────────────────────────────────────────────────────────────

/// `correct` **不重扫门禁**：它只改 `finished` 会话的区间事实，恢复性快照照旧
/// （重扫是 `reconcile`/`discard_session` 的提交后收尾，归 S1）。
///
/// 这条用例顺带把一条**可达边界**钉在明处（见 task-3-report.md 的「顾虑」）：
/// `finished` 会话上残留的开放区间属于第 1 类不变量损坏（`open_interval_outside_running`），
/// 门禁会数它；把它软删除之后事实已经不再损坏，但**门禁快照要等下一次重扫**才跟着变。
/// 计划要求 `correct` 不重扫、也没要求它先做第 1 类判定，所以这是**记录在案的行为**，
/// 不是漏掉的一步：本用例的第二个断言就是这条口径的机器证据。
#[test]
fn correct_does_not_rescan_the_recovery_gate() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    // 第 1 类：非 `running` 会话残留开放区间（`needs_review = 0`）。
    insert_interval(&db, "s1-open", "s1", WALL - 1_000, None, None, 0, None);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    assert!(
        state.recovery().requires_recovery(),
        "残留开放区间是不变量损坏，启动后门禁关着"
    );

    state
        .correct(env(&epoch, 0), delete("s1", "s1-open"))
        .expect("删除这条残留的开放区间");
    assert_eq!(
        interval(state.db(), "s1-open").voided_at,
        Some(WALL),
        "事实已经软删除，库里的不变量损坏消失"
    );
    assert!(
        state.recovery().requires_recovery(),
        "correct 不重扫门禁：快照要等下一次 rescan_recovery 才跟着事实走"
    );
    let scan = state.rescan_recovery().unwrap();
    assert!(!scan.requires_recovery(), "重扫之后门禁才跟着已提交事实走");
}

/// 零长度合法（半开区间的空集）：`ended_at == started_at` ⇒ `duration_ms = 0`，
/// 三个字段仍然一起写（`IntervalRange::new` 明确要求覆盖这个边界）。
#[test]
fn a_zero_length_retime_is_allowed_and_keeps_the_duration_in_step() {
    let fx = fixture();
    let db = seeded(&fx);
    finished_fixture(&db);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let before = facts(state.db(), "s1");

    let outcome = state
        .correct(env(&epoch, 0), retime("s1", "s1-b", B_START, B_START))
        .expect("零长度是合法边界");
    let (report, changed) = outcome.into_parts();
    assert!(changed, "起止确实变了（B_END → B_START），所以是真实写入");
    assert_eq!(report.interval.started_at, B_START);
    assert_eq!(report.interval.ended_at, Some(B_START));
    assert_eq!(report.interval.duration_ms, Some(0));

    let db = state.db();
    let retimed = interval(db, "s1-b");
    assert_eq!(
        retimed.duration_ms,
        Some(retimed.ended_at.unwrap() - retimed.started_at),
        "时长与起止始终一致"
    );
    assert_eq!(facts(db, "s1").revision, before.revision + 1);
}
