//! P3 Task 2：`reconcile`（确认 / 丢弃不确定区间）与恢复门禁解除（S1）。
//!
//! 装置抄 `tests/recovery_scan.rs`：手工造出「上一个 run 崩过」的库，再走**真实**的
//! `bootstrap::startup`，然后在同一把锁（`lock_app`）里调服务入口与 `AppState` 包装。
//!
//! 覆盖（Task 2 的测试清单）：
//! 全部待确认一次处理完 · 缺一条/多一条/外来/非待确认被拒 · 重叠被拒（含跨会话、
//! 端点相接不算）· `ended_at > now` 被拒 · `DiscardUncertain` 不动别的会话与既有
//! 闭合区间 · 非 `recovering` 被拒（逐字段）· 第 1 类被拒 · `time_edit` 前后值完整 ·
//! 确认后 `session.needs_review = false` 且该会话能被 `resume` · 失败整体回滚 ·
//! 提交后 `AppState::recovery().requires_recovery()` 随事实变化。
//!
//! 另有 S1 自身的失败闭环（扫描查询失败 ⇒ 置标记、保留旧快照、门禁继续关闭；
//! 再扫成功才清标记）与 `attention_overview` 的作用域口径（终态会话也要有条目，
//! 正在计时的当前 run 会话不进列表）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use worktrace_lib::domain::session::{SessionAttention, SessionMode, SessionState, TimerKind};
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, NoProbe, RecoveryScan, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::recovery::{
    attention_overview, ConfirmedRange, ReconcileAction, ReconcileRequest, ReconcileTargetState,
};
use worktrace_lib::services::timer::coordinator::{ResumeRequest, StartRequest};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::run_repo;

/// 启动那一刻的挂钟（`FakeClock` 不自己走，所以 `now` 恒等于它）。
const WALL: i64 = 1_700_000_000_000;
/// 崩掉的那一代次。
const OLD_RUN: &str = "run-old";
/// 恢复会话的起点。
const T0: i64 = WALL - 20_000;
/// 可信前缀的终点（= S5 归一出来的候选端点）。
const PREFIX_END: i64 = WALL - 18_000;

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

/// 真跑一次启动（第 ④ 步的扫描会把 `running` 崩溃段归一成 `recovering`）。
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

/// 建一个「上一个 run 崩过」的库：迁移 + 元数据 + 上一代 run + 一条 `Doing` 任务。
fn seeded(fx: &Fixture) -> Db {
    let mut db = Db::open(&fx.db_path).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    run_repo::start_run(&tx, OLD_RUN, T0 - 1_000).unwrap();
    tx.execute(
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Doing',0,1000,1000)",
        [],
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

/// 一个「崩过、已被启动扫描归一」的会话：可信前缀 + 零长度候选 + 终点未知段。
///
/// 这三段的形状就是 S5（Task 1）产出的形态：前缀 `needs_review=0` 且有时长，
/// 两个待确认段都是 `duration_ms = NULL`（候选端点不是事实）。区间 id 按会话前缀
/// 生成（`<session>-prefix` / `-cand` / `-unknown`），同一个库里可以放多个会话。
fn crashed_recovering(db: &Db, session_id: &str) {
    let id = |suffix: &str| format!("{session_id}-{suffix}");
    insert_session(
        db,
        session_id,
        OLD_RUN,
        "FOREGROUND",
        "recovering",
        T0,
        None,
        1,
    );
    insert_interval(
        db,
        &id("prefix"),
        session_id,
        T0,
        Some(PREFIX_END),
        Some(PREFIX_END - T0),
        0,
        None,
    );
    insert_interval(
        db,
        &id("cand"),
        session_id,
        PREFIX_END,
        Some(PREFIX_END),
        None,
        1,
        None,
    );
    insert_interval(
        db,
        &id("unknown"),
        session_id,
        WALL - 17_000,
        None,
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

/// 「逐字段比对」用的整体快照：会话 + 该会话全部区间（按 id）+ 版本 + 审计行数。
///
/// 被拒命令的零变化用 `assert_eq!(facts(db, "s1"), before)` 一句话比对**每个字段**，
/// 而不是只比行数（总纲 §5 第 8 条的口径）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Facts {
    session: SessionFacts,
    intervals: Vec<(String, IntervalFacts)>,
    revision: i64,
    time_edit_rows: i64,
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
    }
}

fn scalar(db: &Db, sql: &str) -> i64 {
    db.connection().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn revision(db: &Db) -> i64 {
    scalar(db, "SELECT revision FROM app_meta WHERE singleton = 1")
}

/// 全表「终点未知的未作废区间」条数（收尾后本会话必须为 0）。
fn unknown_end_open_intervals(db: &Db, session_id: &str) -> i64 {
    db.connection()
        .query_row(
            "SELECT COUNT(*) FROM work_interval
              WHERE session_id = ?1 AND voided_at IS NULL AND ended_at IS NULL",
            [session_id],
            |r| r.get(0),
        )
        .unwrap()
}

/// 待确认区间 id（升序）。
fn pending_ids(db: &Db, session_id: &str) -> Vec<String> {
    let mut stmt = db
        .connection()
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

/// 某会话的（before_json, after_json, reason）；多行时取第一行。
fn time_edit_of(
    db: &Db,
    session_id: &str,
) -> (serde_json::Value, serde_json::Value, Option<String>) {
    let (before, after, reason) = db
        .connection()
        .query_row(
            "SELECT before_json, after_json, reason FROM time_edit WHERE session_id = ?1
              ORDER BY created_at, id",
            [session_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .unwrap();
    (
        serde_json::from_str(&before).unwrap(),
        serde_json::from_str(&after).unwrap(),
        reason,
    )
}

// ── 请求构造 ────────────────────────────────────────────────────────────────

fn confirm(
    session_id: &str,
    target: ReconcileTargetState,
    ranges: &[(&str, i64, i64)],
) -> ReconcileRequest {
    ReconcileRequest {
        session_id: session_id.to_string(),
        action: ReconcileAction::Confirm,
        target_state: target,
        ranges: ranges
            .iter()
            .map(|(id, start, end)| ConfirmedRange {
                interval_id: (*id).to_string(),
                started_at: *start,
                ended_at: *end,
            })
            .collect(),
    }
}

fn discard(session_id: &str, target: ReconcileTargetState) -> ReconcileRequest {
    ReconcileRequest {
        session_id: session_id.to_string(),
        action: ReconcileAction::DiscardUncertain,
        target_state: target,
        ranges: Vec::new(),
    }
}

fn env(epoch: &str, row_version: i64) -> WriteEnvelope {
    WriteEnvelope::for_update(epoch, row_version)
}

/// 一组「待确认区间 id + 用户给定的起止」。
type RangeList<'a> = Vec<(&'a str, i64, i64)>;

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

fn assert_code(error: &AppError, code: &str) {
    assert_eq!(error.code(), code, "实际：{error:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 确认：一次事务处理该会话的全部待确认区间
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn confirming_every_pending_interval_in_one_command_closes_the_session() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let run = running.run_id().to_string();

    assert_eq!(
        pending_ids(state.db(), "s1"),
        ["s1-cand", "s1-unknown"],
        "归一后的两段都在待确认集合里"
    );
    assert!(
        state.recovery().requires_recovery(),
        "启动后门禁关着（别的 run 还有恢复材料）"
    );

    let outcome = state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", PREFIX_END, WALL - 17_500),
                    ("s1-unknown", WALL - 17_000, WALL - 16_500),
                ],
            ),
        )
        .unwrap();
    let (report, changed) = outcome.into_parts();
    assert!(changed, "确认是一笔真实写入");
    assert_eq!(report.session.state, SessionState::Paused);
    assert_eq!(report.intervals.len(), 3, "报告给出该会话的全部区间");

    let db = state.db();
    // 两条待确认段各自被确认：起止、时长、needs_review 全变。
    assert_eq!(
        interval(db, "s1-cand"),
        IntervalFacts {
            session_id: "s1".into(),
            started_at: PREFIX_END,
            ended_at: Some(WALL - 17_500),
            duration_ms: Some(500),
            sampled_end_wall_at: None,
            needs_review: 0,
            voided_at: None,
        }
    );
    assert_eq!(
        interval(db, "s1-unknown"),
        IntervalFacts {
            session_id: "s1".into(),
            started_at: WALL - 17_000,
            ended_at: Some(WALL - 16_500),
            duration_ms: Some(500),
            sampled_end_wall_at: None,
            needs_review: 0,
            voided_at: None,
        }
    );
    // 可信前缀一个字段都不动。
    assert_eq!(
        interval(db, "s1-prefix"),
        IntervalFacts {
            session_id: "s1".into(),
            started_at: T0,
            ended_at: Some(PREFIX_END),
            duration_ms: Some(PREFIX_END - T0),
            sampled_end_wall_at: None,
            needs_review: 0,
            voided_at: None,
        }
    );
    // 会话收尾：状态跃迁、run 切到本次、会话级待确认清零、版本 +1、Paused 不写 ended_at。
    assert_eq!(
        session(db, "s1"),
        SessionFacts {
            state: "paused".into(),
            run_id: run.clone(),
            started_at: T0,
            ended_at: None,
            needs_review: 0,
            row_version: 1,
        }
    );
    assert_eq!(revision(db), 1, "一笔用户命令恰好一次 revision");
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM interval_checkpoint"),
        0,
        "确认不写检查点"
    );
    assert_eq!(
        unknown_end_open_intervals(db, "s1"),
        0,
        "该会话不得再有「未作废且终点未知」的区间"
    );

    // 提交后门禁随事实变化（S1）。
    assert!(
        !state.recovery().requires_recovery(),
        "唯一的恢复材料处理完，门禁必须放开"
    );
    assert!(state.guard_business_timing().is_ok());
}

#[test]
fn confirming_to_finished_stamps_the_session_end_from_trusted_intervals() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Finished,
                &[
                    ("s1-cand", PREFIX_END, WALL - 17_500),
                    ("s1-unknown", WALL - 17_000, WALL - 16_500),
                ],
            ),
        )
        .unwrap();

    let db = state.db();
    let s = session(db, "s1");
    assert_eq!(s.state, "finished");
    assert_eq!(
        s.ended_at,
        Some(WALL - 16_500),
        "结束时刻 = 全部未作废闭合区间里最晚的 ended_at"
    );
    assert_eq!(s.needs_review, 0);
}

#[test]
fn confirming_an_empty_pending_set_only_moves_the_session_state() {
    let fx = fixture();
    let db = seeded(&fx);
    // P2 的「候选终点正好落在检查点上」：recovering 且没有余段（待确认集合为空）。
    insert_session(
        &db,
        "s-empty",
        OLD_RUN,
        "FOREGROUND",
        "recovering",
        T0,
        None,
        1,
    );
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    state
        .reconcile(
            env(&epoch, 0),
            confirm("s-empty", ReconcileTargetState::Finished, &[]),
        )
        .unwrap();

    let db = state.db();
    let s = session(db, "s-empty");
    assert_eq!(s.state, "finished");
    assert_eq!(s.needs_review, 0);
    assert_eq!(
        s.ended_at,
        Some(T0),
        "一条可用区间都没有时兜底到会话起点（ck_session_range）"
    );
    assert_eq!(revision(db), 1);
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM time_edit WHERE session_id='s-empty'"
        ),
        1,
        "纯状态跃迁也留审计"
    );
    assert!(!state.recovery().requires_recovery());
}

// ─────────────────────────────────────────────────────────────────────────────
// 拒绝：缺一条 / 多一条 / 外来 / 非待确认（整条命令零变化）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_range_list_that_does_not_cover_the_pending_set_is_rejected() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    // 另一个会话的区间：确认列表里塞它会撞「不属于该会话」。
    insert_session(
        &db,
        "s2",
        OLD_RUN,
        "FOREGROUND",
        "finished",
        WALL - 12_000,
        None,
        0,
    );
    insert_interval(
        &db,
        "s2-other",
        "s2",
        WALL - 12_000,
        Some(WALL - 11_000),
        Some(1_000),
        0,
        None,
    );
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let cases: Vec<(&str, RangeList)> = vec![
        ("缺一条", vec![("s1-cand", PREFIX_END, PREFIX_END)]),
        (
            "多一条（外来会话的区间）",
            vec![
                ("s1-cand", PREFIX_END, PREFIX_END),
                ("s1-unknown", WALL - 17_000, WALL - 16_500),
                ("s2-other", WALL - 10_000, WALL - 9_000),
            ],
        ),
        (
            "指向不是待确认的区间（可信前缀）",
            vec![
                ("s1-prefix", PREFIX_END, WALL - 17_500),
                ("s1-unknown", WALL - 17_000, WALL - 16_500),
            ],
        ),
        (
            "同一个区间重复两次",
            vec![
                ("s1-cand", PREFIX_END, PREFIX_END),
                ("s1-cand", PREFIX_END, PREFIX_END),
            ],
        ),
    ];

    for (label, ranges) in cases {
        let before = facts(state.db(), "s1");
        let before_other = facts(state.db(), "s2");
        let error = state
            .reconcile(
                env(&epoch, 0),
                confirm("s1", ReconcileTargetState::Paused, &ranges),
            )
            .unwrap_err();
        assert_code(&error, "DOMAIN_ERROR");
        assert_eq!(facts(state.db(), "s1"), before, "{label}：必须零变化");
        assert_eq!(
            facts(state.db(), "s2"),
            before_other,
            "{label}：别的会话也不许动"
        );
    }
}

#[test]
fn an_unknown_session_is_rejected() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let error = state
        .reconcile(
            env(&epoch, 0),
            confirm("s-404", ReconcileTargetState::Paused, &[]),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(revision(state.db()), 0);
}

#[test]
fn a_stale_session_version_is_rejected_without_writing() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let before = facts(state.db(), "s1");
    let error = state
        .reconcile(
            env(&epoch, 7),
            confirm("s1", ReconcileTargetState::Paused, &[]),
        )
        .unwrap_err();
    assert_code(&error, "VERSION_CONFLICT");
    assert_eq!(facts(state.db(), "s1"), before);
}

#[test]
fn a_missing_record_version_is_rejected() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let before = facts(state.db(), "s1");
    let error = state
        .reconcile(
            WriteEnvelope::for_create(&epoch),
            confirm("s1", ReconcileTargetState::Paused, &[]),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s1"), before);
}

#[test]
fn an_epoch_mismatch_is_rejected_without_writing() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());

    let before = facts(state.db(), "s1");
    let error = state
        .reconcile(
            env("epoch-from-another-database", 0),
            confirm("s1", ReconcileTargetState::Paused, &[]),
        )
        .unwrap_err();
    assert_code(&error, "DATA_EPOCH_MISMATCH");
    assert_eq!(facts(state.db(), "s1"), before);
}

// ─────────────────────────────────────────────────────────────────────────────
// 拒绝：重叠（跨会话、可信前缀、正在计时的区间、未来、ranges 内部）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_confirmed_range_that_overlaps_confirmed_human_time_is_rejected() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    // 后来已记录的人工时间（另一个会话：[WALL-5000, WALL-4000)）。
    insert_session(
        &db,
        "s2",
        OLD_RUN,
        "FOREGROUND",
        "finished",
        WALL - 5_000,
        None,
        0,
    );
    insert_interval(
        &db,
        "s2-other",
        "s2",
        WALL - 5_000,
        Some(WALL - 4_000),
        Some(1_000),
        0,
        None,
    );
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    // 与别的会话的已确认区间相交 ⇒ 拒绝（半开：完全落在内部）。
    let before = facts(state.db(), "s1");
    let error = state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", PREFIX_END, PREFIX_END),
                    ("s1-unknown", WALL - 4_500, WALL - 4_400),
                ],
            ),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s1"), before, "重叠必须整条拒绝");

    // 与**本会话的可信前缀**相交（把起点往前改）⇒ 同样拒绝（S7 不是特例）。
    let error = state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", T0 - 100, PREFIX_END - 500),
                    ("s1-unknown", WALL - 17_000, WALL - 16_500),
                ],
            ),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s1"), before);

    // 端点相接**不算**重叠：确认段正好收在别人起点上。
    let outcome = state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", PREFIX_END, PREFIX_END),
                    ("s1-unknown", WALL - 6_000, WALL - 5_000),
                ],
            ),
        )
        .unwrap();
    let (report, _) = outcome.into_parts();
    assert_eq!(report.session.state, SessionState::Paused);
    assert_eq!(
        interval(state.db(), "s1-unknown").ended_at,
        Some(WALL - 5_000)
    );
}

#[test]
fn a_confirmed_range_that_touches_an_open_running_interval_is_rejected() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let run = running.run_id().to_string();

    // 当前 run 里另有一条正在计时的会话：它的区间还没有终点（`ended_at IS NULL`），
    // 所以任何晚于它起点的确认都会与它重叠——那会造出两段互相覆盖的人工时间。
    insert_session(
        state.db(),
        "s-live",
        &run,
        "FOREGROUND",
        "running",
        WALL - 1_000,
        None,
        0,
    );
    insert_interval(
        state.db(),
        "s-live-open",
        "s-live",
        WALL - 1_000,
        None,
        None,
        0,
        None,
    );

    let before = facts(state.db(), "s1");
    let error = state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", PREFIX_END, PREFIX_END),
                    ("s1-unknown", WALL - 17_000, WALL - 500),
                ],
            ),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s1"), before);

    // 收在它的起点上：端点相接，不算重叠。
    state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", PREFIX_END, PREFIX_END),
                    ("s1-unknown", WALL - 17_000, WALL - 1_000),
                ],
            ),
        )
        .unwrap();
}

#[test]
fn confirmed_ranges_that_overlap_each_other_are_rejected() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let before = facts(state.db(), "s1");
    let error = state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", PREFIX_END, WALL - 16_000),
                    ("s1-unknown", WALL - 16_500, WALL - 16_000),
                ],
            ),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s1"), before);
}

#[test]
fn a_confirmed_range_that_ends_in_the_future_is_rejected() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let before = facts(state.db(), "s1");
    let error = state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", PREFIX_END, PREFIX_END),
                    ("s1-unknown", WALL - 1_000, WALL + 1),
                ],
            ),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s1"), before);
}

#[test]
fn a_confirmed_range_that_ends_before_it_starts_is_rejected() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let before = facts(state.db(), "s1");
    let error = state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", PREFIX_END, PREFIX_END - 1),
                    ("s1-unknown", WALL - 17_000, WALL - 16_500),
                ],
            ),
        )
        .unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s1"), before);
}

// ─────────────────────────────────────────────────────────────────────────────
// 拒绝：非 recovering / 第 1 类
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_session_that_is_not_recovering_is_rejected_field_by_field() {
    let fx = fixture();
    let db = seeded(&fx);
    // R6 的形态：paused 却仍挂着**已闭合**的待确认区间（不是不变量损坏）。
    insert_session(
        &db,
        "s-paused",
        OLD_RUN,
        "FOREGROUND",
        "paused",
        WALL - 15_000,
        None,
        1,
    );
    insert_interval(
        &db,
        "s-paused-cand",
        "s-paused",
        WALL - 15_000,
        Some(WALL - 14_000),
        None,
        1,
        None,
    );
    // m2 的终态形态：finished 带已闭合的待确认区间（门禁会数它，但它不是 recovering）。
    insert_session(
        &db,
        "s-fin",
        OLD_RUN,
        "FOREGROUND",
        "finished",
        WALL - 13_000,
        None,
        1,
    );
    insert_interval(
        &db,
        "s-fin-cand",
        "s-fin",
        WALL - 13_000,
        Some(WALL - 12_500),
        None,
        1,
        None,
    );
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    for session_id in ["s-paused", "s-fin"] {
        let before = facts(state.db(), session_id);
        let error = state
            .reconcile(
                env(&epoch, 0),
                discard(session_id, ReconcileTargetState::Paused),
            )
            .unwrap_err();
        assert_code(&error, "DOMAIN_ERROR");
        assert_eq!(
            facts(state.db(), session_id),
            before,
            "{session_id}：前置不满足必须整条拒绝"
        );
        assert_eq!(
            pending_ids(state.db(), session_id).len(),
            1,
            "{session_id}：待确认事实原样保留"
        );
    }
}

#[test]
fn a_session_with_a_broken_invariant_is_rejected_and_points_at_diagnostics() {
    let fx = fixture();
    let db = seeded(&fx);
    // 第 1 类：recovering 却留着**已确认**的开放区间（`open_interval_outside_running`）。
    insert_session(
        &db,
        "s-broken",
        OLD_RUN,
        "FOREGROUND",
        "recovering",
        T0,
        None,
        0,
    );
    insert_interval(&db, "s-broken-open", "s-broken", T0, None, None, 0, None);
    // 第 1 类的另一面：running 却带着待确认区间（`running_with_pending_interval`）。
    insert_session(
        &db,
        "s-running",
        OLD_RUN,
        "FOREGROUND",
        "running",
        WALL - 10_000,
        None,
        0,
    );
    insert_interval(
        &db,
        "s-running-open",
        "s-running",
        WALL - 10_000,
        None,
        None,
        0,
        None,
    );
    insert_interval(
        &db,
        "s-running-cand",
        "s-running",
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

    for session_id in ["s-broken", "s-running"] {
        let before = facts(state.db(), session_id);
        let error = state
            .reconcile(
                env(&epoch, 0),
                discard(session_id, ReconcileTargetState::Paused),
            )
            .unwrap_err();
        assert_code(&error, "DOMAIN_ERROR");
        let detail = error.detail().unwrap_or_default();
        assert!(
            detail.contains("损坏"),
            "文案必须指向诊断而不是「确认一下就修好」：{detail}"
        );
        assert_eq!(facts(state.db(), session_id), before, "损坏会话零写入");
    }
    // 损坏的会话仍然算恢复材料：门禁不因为被拒而放开。
    assert!(state.recovery().requires_recovery());
}

// ─────────────────────────────────────────────────────────────────────────────
// DiscardUncertain：只作废本会话的待确认区间
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn discarding_uncertain_intervals_voids_only_the_pending_ones() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    // 一条**已知时长**的待确认区间：作废时端点与时长原样保留。
    insert_interval(
        &db,
        "s1-known",
        "s1",
        WALL - 16_000,
        Some(WALL - 15_000),
        Some(1_000),
        1,
        None,
    );
    // 另一个会话的待确认区间与可信区间：一个字段都不许动。
    crashed_recovering(&db, "s2");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let run = running.run_id().to_string();

    let before_other = facts(state.db(), "s2");
    let outcome = state
        .reconcile(env(&epoch, 0), discard("s1", ReconcileTargetState::Paused))
        .unwrap();
    let (report, changed) = outcome.into_parts();
    assert!(changed);
    assert_eq!(report.intervals.len(), 4);

    let db = state.db();
    // 零长度候选与终点未知段：作废 + 清 needs_review，`ended_at` 清为 NULL（不许补零时长）。
    assert_eq!(
        interval(db, "s1-cand"),
        IntervalFacts {
            session_id: "s1".into(),
            started_at: PREFIX_END,
            ended_at: None,
            duration_ms: None,
            sampled_end_wall_at: None,
            needs_review: 0,
            voided_at: Some(WALL),
        },
        "未确认的候选端点不能留成事实"
    );
    assert_eq!(
        interval(db, "s1-unknown"),
        IntervalFacts {
            session_id: "s1".into(),
            started_at: WALL - 17_000,
            ended_at: None,
            duration_ms: None,
            sampled_end_wall_at: None,
            needs_review: 0,
            voided_at: Some(WALL),
        }
    );
    // 已知时长的待确认区间：作废时保留端点与时长。
    assert_eq!(
        interval(db, "s1-known"),
        IntervalFacts {
            session_id: "s1".into(),
            started_at: WALL - 16_000,
            ended_at: Some(WALL - 15_000),
            duration_ms: Some(1_000),
            sampled_end_wall_at: None,
            needs_review: 0,
            voided_at: Some(WALL),
        }
    );
    // 既有可信前缀原样保留（这是「丢弃不确定区间」与「作废整次」的分界）。
    assert_eq!(
        interval(db, "s1-prefix"),
        IntervalFacts {
            session_id: "s1".into(),
            started_at: T0,
            ended_at: Some(PREFIX_END),
            duration_ms: Some(PREFIX_END - T0),
            sampled_end_wall_at: None,
            needs_review: 0,
            voided_at: None,
        }
    );
    assert_eq!(
        session(db, "s1"),
        SessionFacts {
            state: "paused".into(),
            run_id: run,
            started_at: T0,
            ended_at: None,
            needs_review: 0,
            row_version: 1,
        }
    );
    assert_eq!(revision(db), 1);
    assert_eq!(unknown_end_open_intervals(db, "s1"), 0);
    // 另一个会话的终点未知段仍在（它还没被处理），字段一个不动。
    assert_eq!(unknown_end_open_intervals(db, "s2"), 1);
    let after_other = facts(db, "s2");
    assert_eq!(
        after_other.session, before_other.session,
        "别的会话的状态与版本不动"
    );
    assert_eq!(
        after_other.intervals, before_other.intervals,
        "别的会话的区间一个字段都不动"
    );
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM time_edit WHERE session_id='s2'"),
        0,
        "别的会话不写审计"
    );
    // 还有别的待处理事实 ⇒ 门禁仍然关着。
    assert!(state.recovery().requires_recovery());
}

#[test]
fn discard_uncertain_refuses_an_explicit_range_list() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let before = facts(state.db(), "s1");
    let mut req = discard("s1", ReconcileTargetState::Paused);
    req.ranges = vec![ConfirmedRange {
        interval_id: "s1-cand".into(),
        started_at: PREFIX_END,
        ended_at: PREFIX_END,
    }];
    let error = state.reconcile(env(&epoch, 0), req).unwrap_err();
    assert_code(&error, "DOMAIN_ERROR");
    assert_eq!(facts(state.db(), "s1"), before);
}

// ─────────────────────────────────────────────────────────────────────────────
// 审计：前后值完整（Ruling 8：确认端点是用户给定值，不写候选来源）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_audit_records_before_and_after_for_every_processed_interval() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let run = running.run_id().to_string();

    state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", PREFIX_END, WALL - 17_500),
                    ("s1-unknown", WALL - 17_000, WALL - 16_500),
                ],
            ),
        )
        .unwrap();

    let db = state.db();
    assert_eq!(
        scalar(db, "SELECT COUNT(*) FROM time_edit WHERE session_id='s1'"),
        1,
        "一条用户命令一条审计"
    );
    let (before, after, reason) = time_edit_of(db, "s1");
    assert_eq!(reason.as_deref(), Some("reconcile:confirm"));
    assert_eq!(before["session"]["state"], "recovering");
    assert_eq!(before["session"]["run_id"], OLD_RUN);
    assert_eq!(before["session"]["needs_review"], true);
    assert_eq!(after["session"]["state"], "paused");
    assert_eq!(after["session"]["run_id"], run);
    assert_eq!(after["session"]["needs_review"], false);

    // 区间按 `started_at, id` 排序：0 = 可信前缀，1 = 零长度候选，2 = 终点未知段。
    assert_eq!(before["intervals"][0]["ended_at"], PREFIX_END);
    assert_eq!(before["intervals"][0]["needs_review"], false);
    assert_eq!(before["intervals"][1]["started_at"], PREFIX_END);
    assert_eq!(before["intervals"][1]["ended_at"], PREFIX_END);
    assert_eq!(
        before["intervals"][1]["duration_ms"],
        serde_json::Value::Null
    );
    assert_eq!(before["intervals"][1]["needs_review"], true);
    assert_eq!(before["intervals"][1]["voided_at"], serde_json::Value::Null);
    assert_eq!(before["intervals"][2]["started_at"], WALL - 17_000);
    assert_eq!(before["intervals"][2]["ended_at"], serde_json::Value::Null);
    // **后**值。
    assert_eq!(after["intervals"][1]["ended_at"], WALL - 17_500);
    assert_eq!(after["intervals"][1]["duration_ms"], 500);
    assert_eq!(after["intervals"][1]["needs_review"], false);
    assert_eq!(after["intervals"][2]["ended_at"], WALL - 16_500);
    assert_eq!(after["intervals"][2]["duration_ms"], 500);
    assert_eq!(after["intervals"][2]["needs_review"], false);
    // Ruling 8：确认的端点是用户给定的，不是从候选推导出来的。
    assert!(
        after.get("candidate_end").is_none(),
        "确认路径不写候选端点：{after}"
    );
    assert!(
        after.get("candidate_end_source").is_none(),
        "确认路径不写候选来源：{after}"
    );
}

#[test]
fn the_audit_of_a_discard_names_its_own_reason_and_keeps_the_candidate_endpoint() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    state
        .reconcile(
            env(&epoch, 0),
            discard("s1", ReconcileTargetState::Finished),
        )
        .unwrap();

    let (before, after, reason) = time_edit_of(state.db(), "s1");
    assert_eq!(reason.as_deref(), Some("reconcile:discard_uncertain"));
    assert_eq!(
        before["intervals"][1]["ended_at"], PREFIX_END,
        "候选端点原文留在 before_json 里"
    );
    assert_eq!(
        after["intervals"][1]["ended_at"],
        serde_json::Value::Null,
        "未确认候选的端点被清空"
    );
    assert_eq!(after["intervals"][1]["voided_at"], WALL);
    assert_eq!(after["intervals"][1]["needs_review"], false);
    assert!(
        after.get("candidate_end_source").is_none(),
        "作废不推导候选端点：{after}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 闭环：确认之后会话可以被 resume
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_confirmed_session_can_be_resumed() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let run = running.run_id().to_string();

    let outcome = state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", PREFIX_END, WALL - 17_500),
                    ("s1-unknown", WALL - 17_000, WALL - 16_500),
                ],
            ),
        )
        .unwrap();
    let (report, _) = outcome.into_parts();
    assert!(!report.session.needs_review, "确认必须清会话级待确认");
    assert_eq!(report.session.run_id, run);

    let version = session(state.db(), "s1").row_version;
    state
        .resume(ResumeRequest {
            expected_data_epoch: epoch,
            task_id: "t1".into(),
            task_expected_version: 0,
            session_id: "s1".into(),
            session_expected_version: version,
        })
        .expect("确认之后该会话必须能继续");

    let db = state.db();
    let s = session(db, "s1");
    assert_eq!(s.state, "running");
    assert_eq!(s.run_id, run);
    assert_eq!(s.needs_review, 0);
    assert_eq!(
        pending_ids(db, "s1"),
        Vec::<String>::new(),
        "resume 之后没有待确认残留"
    );
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM work_interval WHERE session_id='s1' AND ended_at IS NULL
               AND voided_at IS NULL"
        ),
        1,
        "resume 开出恰好一个新的开放区间"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 回滚
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_failure_after_the_interval_writes_rolls_the_whole_transaction_back() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    let before = facts(state.db(), "s1");
    // 让审计写不进去：区间与会话的 UPDATE 已经执行过，失败必须整体回滚。
    state
        .db()
        .connection()
        .execute("DROP TABLE time_edit", [])
        .unwrap();

    let error = state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", PREFIX_END, WALL - 17_500),
                    ("s1-unknown", WALL - 17_000, WALL - 16_500),
                ],
            ),
        )
        .unwrap_err();
    assert_code(&error, "STORAGE_ERROR");

    assert_eq!(
        session(state.db(), "s1"),
        before.session,
        "会话状态必须回滚"
    );
    for (id, expected) in &before.intervals {
        assert_eq!(&interval(state.db(), id), expected, "{id} 必须回滚");
    }
    assert_eq!(revision(state.db()), before.revision, "版本必须回滚");
    assert_eq!(pending_ids(state.db(), "s1").len(), 2, "待确认集合原样保留");
}

// ─────────────────────────────────────────────────────────────────────────────
// S1：门禁随事实变化；扫描失败必须闭环
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_recovery_gate_opens_only_after_every_recovery_fact_is_resolved() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    crashed_recovering(&db, "s2");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let run = running.run_id().to_string();

    assert!(state.recovery().requires_recovery());
    state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", PREFIX_END, PREFIX_END),
                    ("s1-unknown", WALL - 17_000, WALL - 16_500),
                ],
            ),
        )
        .unwrap();
    assert!(
        state.recovery().requires_recovery(),
        "还有别的待处理事实 ⇒ 门禁必须仍然关着"
    );
    assert!(state.guard_business_timing().is_err());

    state
        .reconcile(
            env(&epoch, 0),
            discard("s2", ReconcileTargetState::Finished),
        )
        .unwrap();
    assert!(
        !state.recovery().requires_recovery(),
        "全部处理完 ⇒ 门禁放开"
    );
    assert!(state.guard_business_timing().is_ok());

    // 闭环的最后一步：门禁放开之后 start 真的能开始计时。
    state
        .start(start_request(&epoch))
        .expect("门禁解除后 start 必须成功");
    assert_eq!(session(state.db(), "s1").state, "paused");
    assert_eq!(session(state.db(), "s2").state, "finished");
    assert_ne!(session(state.db(), "s1").run_id, OLD_RUN);
    assert_eq!(session(state.db(), "s1").run_id, run);
}

#[test]
fn a_failed_rescan_keeps_the_gate_closed_until_a_later_scan_succeeds() {
    let fx = fixture();
    let db = seeded(&fx);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    assert!(
        !state.recovery().requires_recovery(),
        "干净的库：启动门禁本来就是开的"
    );
    assert!(state.guard_business_timing().is_ok());

    // 注入「三条扫描查询失败」：把 work_interval 改名（查询会报 no such table）。
    state
        .db()
        .connection()
        .execute_batch("ALTER TABLE work_interval RENAME TO work_interval_hidden;")
        .unwrap();

    let error = state.rescan_recovery().unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(
        state.recovery(),
        &RecoveryScan::default(),
        "失败不得把旧快照当成成功结果（这里旧快照本来就是空的）"
    );
    assert!(
        !state.recovery().requires_recovery(),
        "也不得谎报「有恢复材料」——挡住计时的必须是那个失败标记"
    );
    // 但门禁必须继续关着：扫描失败 = 不知道有没有未处理的恢复事实。
    assert_code(
        &state.guard_business_timing().unwrap_err(),
        "RECOVERY_REQUIRED",
    );
    let error = state.start(start_request(&epoch)).unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");

    // 扫描恢复成功 ⇒ 清标记，门禁放开，start 可用（不要求重做任何已提交命令）。
    state
        .db()
        .connection()
        .execute_batch("ALTER TABLE work_interval_hidden RENAME TO work_interval;")
        .unwrap();
    let scan = state.rescan_recovery().unwrap();
    assert!(!scan.requires_recovery());
    assert!(state.guard_business_timing().is_ok());
    state
        .start(start_request(&epoch))
        .expect("重扫成功之后门禁必须放开");
}

/// S1 的另一半：扫描失败时**旧快照必须原样保留**（既不清空、也不替换成空结论）。
///
/// 上一个用例用的是干净库（旧快照本来就是空的），只证到「不得谎报」；
/// 这里让失败前的快照**非默认**，才真的把「保留」钉住。
#[test]
fn a_failed_rescan_preserves_a_non_default_snapshot() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s-rec");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let before = state.recovery().clone();
    assert_eq!(before.unfinished_sessions, ["s-rec"]);
    assert_eq!(before.pending_intervals, ["s-rec-cand", "s-rec-unknown"]);
    assert!(before.requires_recovery());

    // 让三条扫描查询失败（改表名 ⇒ no such table）。
    state
        .db()
        .connection()
        .execute_batch("ALTER TABLE work_interval RENAME TO work_interval_hidden;")
        .unwrap();

    let error = state.rescan_recovery().unwrap_err();
    assert_code(&error, "RECOVERY_REQUIRED");
    assert_eq!(
        state.recovery(),
        &before,
        "扫描失败不得替换（更不得清空）上一次成功扫描的结论"
    );
    assert_eq!(state.recovery().unfinished_sessions, ["s-rec"]);
    assert_eq!(
        state.recovery().pending_intervals,
        ["s-rec-cand", "s-rec-unknown"]
    );
    assert_code(
        &state.guard_business_timing().unwrap_err(),
        "RECOVERY_REQUIRED",
    );
}

#[test]
fn rescan_recovery_reports_the_new_facts_and_preserves_the_committed_ones() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s1");
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    // 直接重扫（幂等）：没有写入，版本不动。
    let before = revision(state.db());
    let scan = state.rescan_recovery().unwrap();
    assert!(scan.requires_recovery());
    assert_eq!(scan.pending_intervals, ["s1-cand", "s1-unknown"]);
    assert_eq!(scan.unfinished_sessions, ["s1"]);
    assert_eq!(revision(state.db()), before, "重扫是只读的");

    // 提交之后再重扫：事实变了，快照跟着变。
    state
        .reconcile(
            env(&epoch, 0),
            confirm(
                "s1",
                ReconcileTargetState::Paused,
                &[
                    ("s1-cand", PREFIX_END, PREFIX_END),
                    ("s1-unknown", WALL - 17_000, WALL - 16_500),
                ],
            ),
        )
        .unwrap();
    let scan = state.rescan_recovery().unwrap();
    assert!(!scan.requires_recovery());
    assert!(scan.pending_intervals.is_empty());
    assert!(scan.unfinished_sessions.is_empty());
}

// ─────────────────────────────────────────────────────────────────────────────
// 提交后的镜像刷新（计划「新增-2」）：只刷新协调器正镜像的那条
// ─────────────────────────────────────────────────────────────────────────────

/// 对账**别的**会话时不得抢走 `live`：正在计时那条的内存镜像与心跳必须原样保留。
#[test]
fn reconciling_another_session_does_not_clobber_the_live_mirror() {
    let fx = fixture();
    let db = seeded(&fx);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let run = running.run_id().to_string();

    // 正在计时的 B：本 run 的会话，协调器镜像的就是它。
    state.start(start_request(&epoch)).unwrap();
    let live_id = state.coordinator().live().unwrap().id.clone();
    assert_eq!(
        state.coordinator().live().unwrap().state,
        SessionState::Running
    );

    // 同一个 run 里另一条会话 A 变 `recovering`（门禁只数**别的** run，所以它不挡计时；
    // 这也正是「本 run 有 recovering + 用户合法 start 了另一条」的可达位移）。
    insert_session(
        state.db(),
        "s-a",
        &run,
        "FOREGROUND",
        "recovering",
        WALL - 2_000,
        None,
        1,
    );

    state
        .reconcile(env(&epoch, 0), discard("s-a", ReconcileTargetState::Paused))
        .unwrap();

    let live = state.coordinator().live().expect("镜像还在");
    assert_eq!(live.id, live_id, "对账别的会话不得把 live 换成它");
    assert_eq!(
        live.state,
        SessionState::Running,
        "正在计时那条的镜像必须原样保留"
    );
    assert_eq!(session(state.db(), "s-a").state, "paused");
}

/// 被改动的会话**正是**镜像那条时，必须按已提交事实刷新它——不能继续按旧状态出快照。
#[test]
fn reconciling_the_mirrored_session_refreshes_its_live_state() {
    let fx = fixture();
    let db = seeded(&fx);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();

    // A 由本 run 自己 `start`：镜像就是它。
    state.start(start_request(&epoch)).unwrap();
    let live_id = state.coordinator().live().unwrap().id.clone();
    let (open_id, started_at): (String, i64) = state
        .db()
        .connection()
        .query_row(
            "SELECT id, started_at FROM work_interval
              WHERE session_id = ?1 AND ended_at IS NULL AND voided_at IS NULL",
            [&live_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    // 模拟 P2 的异常分支：库里 A 已经是 `recovering` + 一段待确认区间，
    // 而内存镜像还停在 `running`（这就是「留了一个按旧状态出快照的 live」）。
    state
        .db()
        .connection()
        .execute(
            "UPDATE work_interval SET ended_at = started_at, needs_review = 1
              WHERE id = ?1",
            [&open_id],
        )
        .unwrap();
    state
        .db()
        .connection()
        .execute(
            "UPDATE work_session SET state = 'recovering', needs_review = 1,
                                    row_version = row_version + 1
              WHERE id = ?1",
            [&live_id],
        )
        .unwrap();
    let version = session(state.db(), &live_id).row_version;

    state
        .reconcile(
            env(&epoch, version),
            confirm(
                &live_id,
                ReconcileTargetState::Paused,
                &[(&open_id, started_at, started_at)],
            ),
        )
        .unwrap();

    let live = state.coordinator().live().expect("镜像还在");
    assert_eq!(live.id, live_id);
    assert_eq!(
        live.state,
        SessionState::Paused,
        "镜像必须按已提交事实刷新，不能继续停在 running"
    );
    assert_eq!(live.row_version, version + 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// attention_overview（R7）：作用域口径
// ─────────────────────────────────────────────────────────────────────────────

/// 口径：`items` = 全部**不变量损坏**的会话（不分状态与 run）
/// ∪ 全部**有未作废待确认区间**的会话（不分状态与 run）
/// ∪ 全部**不属于当前 run 的未结束**会话。
/// 正在计时的当前 run 会话（无损坏、无待确认）**不进列表**——它与计时快照分离。
#[test]
fn attention_overview_keeps_the_live_session_out_and_the_terminal_ones_in() {
    let fx = fixture();
    let db = seeded(&fx);
    crashed_recovering(&db, "s-rec");
    // P2 的既有形态：`recovering` 却**没有**待确认区间（候选终点正好落在检查点上）。
    // 它照样让门禁关着，所以必须出现在列表里——否则就是「门禁关着但列表是空的」（m2 的洞）。
    insert_session(
        &db,
        "s-empty",
        OLD_RUN,
        "FOREGROUND",
        "recovering",
        WALL - 14_000,
        None,
        1,
    );
    // 终态会话 + 已闭合的待确认区间：门禁会数它，列表里必须有它（m2）。
    insert_session(
        &db,
        "s-fin",
        OLD_RUN,
        "FOREGROUND",
        "finished",
        WALL - 12_000,
        None,
        1,
    );
    insert_interval(
        &db,
        "s-fin-cand",
        "s-fin",
        WALL - 12_000,
        Some(WALL - 11_500),
        None,
        1,
        None,
    );
    // 终态会话 + 不变量损坏（非 running 却留着未作废的开放区间）：也要有条目。
    insert_session(
        &db,
        "s-bad",
        OLD_RUN,
        "FOREGROUND",
        "discarded",
        WALL - 6_000,
        None,
        0,
    );
    insert_interval(
        &db,
        "s-bad-open",
        "s-bad",
        WALL - 6_000,
        None,
        None,
        0,
        None,
    );
    drop(db);

    let running = started(&fx);
    let state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let run = running.run_id().to_string();

    // 当前 run 里正在计时的会话：与计时快照分离，不进列表。
    insert_session(
        state.db(),
        "s-live",
        &run,
        "FOREGROUND",
        "running",
        WALL - 1_000,
        None,
        0,
    );
    insert_interval(
        state.db(),
        "s-live-open",
        "s-live",
        WALL - 1_000,
        None,
        None,
        0,
        None,
    );

    let revision_before = revision(state.db());
    let overview = attention_overview(state.db(), &epoch, &run).unwrap();

    assert_eq!(revision(state.db()), revision_before, "概览是纯读");
    assert_eq!(overview.data_epoch, epoch);
    assert_eq!(overview.revision, revision_before);
    let ids: Vec<&str> = overview
        .items
        .iter()
        .map(|item| item.session_id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["s-rec", "s-empty", "s-fin", "s-bad"],
        "按 started_at, id 升序；正在计时的 s-live 不在里面"
    );

    // 数量与条目必须自洽（P8 的列表读的就是 items）。
    assert_eq!(overview.pending_intervals, 3);
    assert_eq!(overview.pending_sessions, 2);
    assert_eq!(overview.fault_sessions, 1);
    assert_eq!(
        overview.pending_intervals,
        overview
            .items
            .iter()
            .map(|item| item.intervals.len())
            .sum::<usize>()
    );
    assert_eq!(
        overview.fault_sessions,
        overview
            .items
            .iter()
            .filter(|item| item.fault_reason.is_some())
            .count()
    );

    let rec = &overview.items[0];
    assert_eq!(rec.state, SessionState::Recovering);
    assert_eq!(rec.run_id, OLD_RUN);
    assert!(!rec.is_current_run);
    assert_eq!(rec.attention, SessionAttention::NeedsReview);
    assert_eq!(rec.fault_reason, None);
    assert_eq!(rec.session_row_version, 0);
    assert!(rec.session_needs_review);
    assert_eq!(
        rec.intervals
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>(),
        ["s-rec-cand", "s-rec-unknown"]
    );
    assert_eq!(rec.intervals[1].ended_at, None, "候选端点只是候选");
    assert_eq!(
        rec.intervals[1].duration_ms, None,
        "未确认 ⇒ 不得当已确认时长"
    );
    assert!(rec.intervals[1].needs_review);
    assert_eq!(rec.intervals[1].sampled_end_wall_at, None);

    // 第三支（不属于当前 run 的未结束会话）**单独承重**：它一个区间都没有，
    // 只可能由 `unfinished_sessions(Some(current_run))` 带进列表。
    let empty = &overview.items[1];
    assert_eq!(empty.session_id, "s-empty");
    assert_eq!(empty.state, SessionState::Recovering);
    assert_eq!(empty.run_id, OLD_RUN);
    assert!(!empty.is_current_run);
    assert_eq!(empty.attention, SessionAttention::NeedsReview);
    assert_eq!(empty.fault_reason, None);
    assert!(
        empty.intervals.is_empty(),
        "没有区间也必须在列表里：门禁数的是它的「未结束」"
    );
    assert_eq!(empty.session_row_version, 0);
    assert!(empty.session_needs_review);

    let fin = &overview.items[2];
    assert_eq!(fin.state, SessionState::Finished);
    assert_eq!(fin.attention, SessionAttention::NeedsReview);
    assert_eq!(fin.fault_reason, None);
    assert_eq!(fin.intervals.len(), 1);
    assert_eq!(fin.intervals[0].ended_at, Some(WALL - 11_500));

    let bad = &overview.items[3];
    assert_eq!(bad.state, SessionState::Discarded);
    assert_eq!(bad.attention, SessionAttention::InvariantBroken);
    assert_eq!(
        bad.fault_reason.as_deref(),
        Some("非 running 会话残留开放区间")
    );
    assert!(bad.intervals.is_empty());
}

#[test]
fn attention_overview_marks_current_run_sessions_and_rejects_a_stale_epoch() {
    let fx = fixture();
    let db = seeded(&fx);
    drop(db);

    let running = started(&fx);
    let mut state = lock_app(running.app());
    let epoch = running.data_epoch().to_string();
    let run = running.run_id().to_string();

    // 干净的库：正在计时的会话不进列表。
    state.start(start_request(&epoch)).unwrap();
    let live_id = state.coordinator().live().unwrap().id.clone();
    let overview = attention_overview(state.db(), &epoch, &run).unwrap();
    assert!(
        overview.items.is_empty(),
        "正在计时的会话与恢复概览分离：{:?}",
        overview.items
    );
    assert_eq!(overview.pending_intervals, 0);
    assert_eq!(overview.pending_sessions, 0);
    assert_eq!(overview.fault_sessions, 0);

    // 让当前 run 的会话带上不变量损坏（running + 待确认区间）：必须被列出，
    // 且 `is_current_run` 为真（P8 据此分组）。
    state
        .db()
        .connection()
        .execute(
            "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms,needs_review)
             VALUES('iv-current',?1,?2,?3,NULL,1)",
            rusqlite::params![live_id, WALL - 500, WALL - 400],
        )
        .unwrap();
    let overview = attention_overview(state.db(), &epoch, &run).unwrap();
    assert_eq!(overview.items.len(), 1);
    assert_eq!(overview.items[0].session_id, live_id);
    assert!(overview.items[0].is_current_run);
    assert_eq!(
        overview.items[0].attention,
        SessionAttention::InvariantBroken
    );
    assert_eq!(
        overview.items[0].fault_reason.as_deref(),
        Some("running 会话带未作废的待确认区间")
    );
    assert_eq!(
        overview.items[0].session_row_version,
        session(state.db(), &live_id).row_version
    );

    // 库身份守卫照旧。
    let error = attention_overview(state.db(), "epoch-from-another-database", &run).unwrap_err();
    assert_code(&error, "DATA_EPOCH_MISMATCH");
}
