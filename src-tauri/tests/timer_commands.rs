//! P2 Task 3 的仓储侧测试：估时基准冻结、会话状态扩展字段、审计仓储。
//!
//! 命令层（`start/pause/resume/finish`）的测试在同一文件的下半部分。
//!
//! 顺带把 P1 里**零调用零测试**的 `session_repo::update_session_state` 测起来——
//! 它的签名本次被扩展（加了 `run_id`/`needs_review`），而 P1 从没有调用方，
//! 所以「P1 无回归」对它原本是空洞的。

use worktrace_lib::domain::session::SessionState;
use worktrace_lib::error::AppError;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{bump_revision, init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::session_repo::{self, SessionStateUpdate};
use worktrace_lib::storage::task_repo::{self, FreezeOutcome};
use worktrace_lib::storage::time_edit_repo::{self, TimeEdit};

struct DbFixture {
    _dir: tempfile::TempDir,
    db: Db,
}

fn fixture() -> DbFixture {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    tx.execute(
        "INSERT INTO application_run(id, started_at) VALUES('run-1', 0)",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO project(id,name,row_version,status,created_at,updated_at)
         VALUES('p1','项目',0,'active',0,0)",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    DbFixture { _dir: dir, db }
}

impl DbFixture {
    fn tx(&mut self) -> rusqlite::Transaction<'_> {
        self.db.connection_mut().unchecked_transaction().unwrap()
    }

    fn task_with_estimate(&self, id: &str, estimate: Option<&str>) {
        self.db
            .connection()
            .execute(
                "INSERT INTO task(id,project_id,title,status,estimated_json,row_version,
                                  created_at,updated_at)
                 VALUES(?1,'p1','任务','Ready',?2,0,0,0)",
                rusqlite::params![id, estimate],
            )
            .unwrap();
    }

    fn session_of(&self, session_id: &str, task_id: &str) {
        self.db
            .connection()
            .execute(
                "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,
                                          started_at,row_version)
                 VALUES(?1,?2,'run-1','FOREGROUND','running','stopwatch',0,0)",
                rusqlite::params![session_id, task_id],
            )
            .unwrap();
    }

    fn baseline_of(&self, task_id: &str) -> Option<String> {
        task_repo::read_estimate(self.db.connection(), task_id)
            .unwrap()
            .unwrap()
            .baseline_estimate_json
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 估时基准冻结
// ─────────────────────────────────────────────────────────────────────────────

/// 首次 start 冻结基准；**之后再 start 不覆盖**（02 §9）。
#[test]
fn the_estimate_baseline_freezes_once_and_never_again() {
    let mut f = fixture();
    f.task_with_estimate("t1", Some(r#"{"p50":3600000}"#));

    // 首次：冻结
    let tx = f.tx();
    let outcome = task_repo::freeze_baseline_estimate(&tx, "t1", 0, 100).unwrap();
    assert_eq!(
        outcome,
        FreezeOutcome::Frozen {
            baseline: Some(r#"{"p50":3600000}"#.into()),
            new_version: 1
        }
    );
    tx.commit().unwrap();

    // 用户改了估时——那不该动基准
    f.db.connection()
        .execute(
            "UPDATE task SET estimated_json = '{\"p50\":9999999}' WHERE id = 't1'",
            [],
        )
        .unwrap();

    // 第二次 start（已有会话）：不覆盖
    f.session_of("s1", "t1");
    let tx = f.tx();
    let again = task_repo::freeze_baseline_estimate(&tx, "t1", 1, 200).unwrap();
    assert_eq!(
        again,
        FreezeOutcome::AlreadyFrozen {
            baseline: Some(r#"{"p50":3600000}"#.into())
        }
    );
    tx.commit().unwrap();

    assert_eq!(
        f.baseline_of("t1"),
        Some(r#"{"p50":3600000}"#.into()),
        "基准仍是首次那个"
    );
    // 版本没被第二次启动推动
    let v: i64 =
        f.db.connection()
            .query_row("SELECT row_version FROM task WHERE id='t1'", [], |r| {
                r.get(0)
            })
            .unwrap();
    assert_eq!(v, 1, "AlreadyFrozen 不该改版本");
}

/// **本来没有估时的任务也不能反复「冻结」。**
///
/// 判据是「还没有任何会话」，不是「基准是不是 NULL」——否则每次 start 都会写一次
/// `NULL`，看起来像冻结、实际每次都在动，且每次都要加版本与审计。
#[test]
fn a_task_without_an_estimate_is_marked_frozen_by_its_first_session() {
    let mut f = fixture();
    f.task_with_estimate("t2", None);

    let tx = f.tx();
    let first = task_repo::freeze_baseline_estimate(&tx, "t2", 0, 100).unwrap();
    assert_eq!(
        first,
        FreezeOutcome::Frozen {
            baseline: None,
            new_version: 1
        }
    );
    // 首次冻结要留审计
    let changes: i64 = tx
        .query_row(
            "SELECT count(*) FROM task_change WHERE task_id='t2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(changes, 1, "冻结基准要有 task_change");
    tx.commit().unwrap();

    f.session_of("s1", "t2");
    let tx = f.tx();
    let second = task_repo::freeze_baseline_estimate(&tx, "t2", 1, 200).unwrap();
    assert_eq!(second, FreezeOutcome::AlreadyFrozen { baseline: None });
    let after: i64 = tx
        .query_row(
            "SELECT count(*) FROM task_change WHERE task_id='t2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, 1, "第二次不该再写审计");
    let v: i64 = tx
        .query_row("SELECT row_version FROM task WHERE id='t2'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(v, 1, "第二次不该动版本");
    tx.commit().unwrap();
}

/// 版本不符要拒绝，且不留痕。
#[test]
fn freezing_with_a_stale_version_is_refused() {
    let mut f = fixture();
    f.task_with_estimate("t1", Some("{}"));

    let tx = f.tx();
    let err = task_repo::freeze_baseline_estimate(&tx, "t1", 99, 100).unwrap_err();
    assert_eq!(err.code(), "VERSION_CONFLICT");
    match err {
        AppError::VersionConflict { expected, actual } => assert_eq!((expected, actual), (99, 0)),
        other => panic!("应为 VersionConflict，实际 {other:?}"),
    }
    let n: i64 = tx
        .query_row(
            "SELECT count(*) FROM task_change WHERE task_id='t1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "被拒时不得写审计");
    tx.rollback().unwrap();
    assert_eq!(f.baseline_of("t1"), None);
}

// ─────────────────────────────────────────────────────────────────────────────
// 会话状态扩展字段
// ─────────────────────────────────────────────────────────────────────────────

/// `update_session_state` 现在能同时改 state / ended_at / run_id / needs_review。
/// 这是 P1 里零测试的那个函数——签名本次被扩展。
#[test]
fn update_session_state_can_move_run_id_and_needs_review() {
    let mut f = fixture();
    f.task_with_estimate("t1", None);
    f.session_of("s1", "t1");

    let tx = f.tx();
    let row = session_repo::update_session_state(
        &tx,
        "s1",
        0,
        SessionState::Recovering,
        SessionStateUpdate {
            needs_review: Some(true),
            run_id: Some("run-1"),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(row.state, SessionState::Recovering);
    assert!(row.needs_review, "needs_review 应被置真");
    assert_eq!(row.row_version, 1, "版本 +1");
    tx.commit().unwrap();
}

/// 未提供的字段**保持不动**——用一个 `Update::default()` 只改状态验证。
#[test]
fn unspecified_fields_stay_untouched() {
    let mut f = fixture();
    f.task_with_estimate("t1", None);
    f.session_of("s1", "t1");
    // run_id 有外键指向 application_run，所以 run-old 必须真的存在
    f.db.connection()
        .execute(
            "INSERT INTO application_run(id, started_at) VALUES('run-old', 0)",
            [],
        )
        .unwrap();
    f.db.connection()
        .execute(
            "UPDATE work_session SET needs_review=1, run_id='run-old' WHERE id='s1'",
            [],
        )
        .unwrap();

    let tx = f.tx();
    let row = session_repo::update_session_state(
        &tx,
        "s1",
        0,
        SessionState::Paused,
        SessionStateUpdate {
            ended_at: Some(5_000),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(row.state, SessionState::Paused);
    assert_eq!(row.ended_at, Some(5_000));
    assert!(row.needs_review, "没传的字段不该被清掉");
    assert_eq!(row.run_id, "run-old", "没传 run_id 就不该改");
    tx.commit().unwrap();
}

/// **resume 要切 run_id**：02 §10 说恢复后显式 resume 必须把 `session.run_id`
/// 切到当前 run，否则下次启动会把新会话误当旧进程记录。
#[test]
fn resuming_switches_the_session_to_the_current_run() {
    let mut f = fixture();
    f.task_with_estimate("t1", None);
    f.session_of("s1", "t1");
    f.db.connection()
        .execute(
            "UPDATE work_session SET state='paused', run_id='run-1' WHERE id='s1'",
            [],
        )
        .unwrap();
    f.db.connection()
        .execute(
            "INSERT INTO application_run(id, started_at) VALUES('run-2', 9999)",
            [],
        )
        .unwrap();

    let tx = f.tx();
    let row = session_repo::update_session_state(
        &tx,
        "s1",
        0,
        SessionState::Running,
        SessionStateUpdate {
            run_id: Some("run-2"),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(row.run_id, "run-2", "resume 必须切到当前 run");
    tx.commit().unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────
// 审计仓储
// ─────────────────────────────────────────────────────────────────────────────

/// 审计与改动同事务：外层回滚，审计也不留。
#[test]
fn a_time_edit_rolls_back_with_its_transaction() {
    let mut f = fixture();
    f.task_with_estimate("t1", None);
    f.session_of("s1", "t1");

    let tx = f.tx();
    time_edit_repo::write(
        &tx,
        &TimeEdit {
            id: "e1".into(),
            session_id: "s1".into(),
            before_json: r#"{"duration_ms":null}"#.into(),
            after_json: r#"{"duration_ms":5000}"#.into(),
            reason: Some("clock anomaly".into()),
            created_at: 100,
        },
    )
    .unwrap();
    tx.rollback().unwrap();

    assert!(
        time_edit_repo::edits_of_session(f.db.connection(), "s1")
            .unwrap()
            .is_empty(),
        "回滚后不得留半个审计记录（02 §9）"
    );
}

/// 提交后能按会话读回来，且按时间排序。
#[test]
fn time_edits_come_back_in_order() {
    let mut f = fixture();
    f.task_with_estimate("t1", None);
    f.session_of("s1", "t1");

    let tx = f.tx();
    for (i, at) in [(1, 300), (2, 100), (3, 200)] {
        time_edit_repo::write(
            &tx,
            &TimeEdit {
                id: format!("e{i}"),
                session_id: "s1".into(),
                before_json: "{}".into(),
                after_json: "{}".into(),
                reason: None,
                created_at: at,
            },
        )
        .unwrap();
    }
    tx.commit().unwrap();

    let edits = time_edit_repo::edits_of_session(f.db.connection(), "s1").unwrap();
    assert_eq!(edits.len(), 3);
    let times: Vec<i64> = edits.iter().map(|e| e.created_at).collect();
    assert_eq!(times, vec![100, 200, 300], "按 created_at 排序");
}

/// 只增不删：审计表没有删除入口，`time_edit` 的存在本身就是「改过」的证据。
#[test]
fn audit_rows_are_append_only() {
    let mut f = fixture();
    f.task_with_estimate("t1", None);
    f.session_of("s1", "t1");
    let tx = f.tx();
    time_edit_repo::write(
        &tx,
        &TimeEdit {
            id: "e1".into(),
            session_id: "s1".into(),
            before_json: "{}".into(),
            after_json: "{}".into(),
            reason: None,
            created_at: 1,
        },
    )
    .unwrap();
    tx.commit().unwrap();

    // 仓储没有 delete/update 入口——下面这句是注释形式的约束：
    // pub fn delete(...) 不该存在。
    assert_eq!(
        time_edit_repo::edits_of_session(f.db.connection(), "s1")
            .unwrap()
            .len(),
        1
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 事务边界：一次业务写恰好一次 revision
// ─────────────────────────────────────────────────────────────────────────────

/// 冻结基准 + 建会话 + 写审计在一个事务里，revision **恰好 +1**。
#[test]
fn one_command_bumps_revision_exactly_once() {
    let mut f = fixture();
    f.task_with_estimate("t1", Some(r#"{"p50":60000}"#));
    let before = require_meta(f.db.connection()).unwrap().revision;

    let tx = f.tx();
    let frozen = task_repo::freeze_baseline_estimate(&tx, "t1", 0, 100).unwrap();
    assert!(matches!(frozen, FreezeOutcome::Frozen { .. }));
    tx.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
         VALUES('s1','t1','run-1','FOREGROUND','running','stopwatch',100,0)",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO work_interval(id,session_id,started_at) VALUES('i1','s1',100)",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO interval_checkpoint(interval_id,run_id,wall_at,attribution_at,elapsed_ms)
         VALUES('i1','run-1',100,100,0)",
        [],
    )
    .unwrap();
    time_edit_repo::write(
        &tx,
        &TimeEdit {
            id: "e1".into(),
            session_id: "s1".into(),
            before_json: "{}".into(),
            after_json: "{}".into(),
            reason: None,
            created_at: 100,
        },
    )
    .unwrap();
    let rev = bump_revision(&tx).unwrap();
    tx.commit().unwrap();

    assert_eq!(rev, before + 1, "一次业务写只加一次 revision");
    assert_eq!(
        require_meta(f.db.connection()).unwrap().revision,
        before + 1
    );
}

/// 末步骤失败 → 整批回滚，并做**字段级**核对（不只比行数）。
#[test]
fn a_failure_in_the_last_step_rolls_back_field_by_field() {
    let mut f = fixture();
    f.task_with_estimate("t1", Some(r#"{"p50":60000}"#));
    let rev_before = require_meta(f.db.connection()).unwrap().revision;

    let tx = f.tx();
    task_repo::freeze_baseline_estimate(&tx, "t1", 0, 100).unwrap();
    tx.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
         VALUES('s1','t1','run-1','FOREGROUND','running','stopwatch',100,0)",
        [],
    )
    .unwrap();
    // 区间也要插上：否则检查点会先在「找不到区间」失败，走的是另一条错误路径，
    // 测不到我们真正想验的「run_id 不符 → 走恢复」。
    tx.execute(
        "INSERT INTO work_interval(id,session_id,started_at) VALUES('i1','s1',100)",
        [],
    )
    .unwrap();
    // 末步骤：检查点的 run_id 与会话不符 → 被拒
    let err = worktrace_lib::storage::checkpoint_repo::write(
        &tx,
        &worktrace_lib::storage::checkpoint_repo::Checkpoint {
            interval_id: "i1".into(),
            run_id: "run-STRANGER".into(),
            wall_at: 100,
            attribution_at: 100,
            elapsed_ms: 0,
        },
    )
    .unwrap_err();
    assert_eq!(err.code(), "RECOVERY_REQUIRED");
    tx.rollback().unwrap();

    // 字段级：基准、版本、审计、会话、revision 全都不动
    assert_eq!(f.baseline_of("t1"), None, "基准不得残留");
    let (v, changes): (i64, i64) =
        f.db.connection()
            .query_row(
                "SELECT t.row_version, (SELECT count(*) FROM task_change WHERE task_id='t1')
               FROM task t WHERE t.id='t1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
    assert_eq!(v, 0, "任务版本不得前进");
    assert_eq!(changes, 0, "不得留审计");
    let sessions: i64 =
        f.db.connection()
            .query_row("SELECT count(*) FROM work_session", [], |r| r.get(0))
            .unwrap();
    assert_eq!(sessions, 0, "不得留会话");
    assert_eq!(
        require_meta(f.db.connection()).unwrap().revision,
        rev_before,
        "revision 不得前进"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 命令层：start / pause / resume / finish
// ─────────────────────────────────────────────────────────────────────────────

mod commands {
    use std::sync::{Arc, Mutex};

    use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
    use worktrace_lib::domain::task::TaskStatus;
    use worktrace_lib::platform::clock::{Clock, FakeClock};
    use worktrace_lib::services::timer::coordinator::{
        Coordinator, ResumeRequest, SessionRequest, StartRequest,
    };
    use worktrace_lib::storage::db::Db;
    use worktrace_lib::storage::meta::{init_meta, require_meta};
    use worktrace_lib::storage::migrations::migrate;
    use worktrace_lib::storage::session_repo;
    use worktrace_lib::storage::task_repo;

    struct Cmd {
        _dir: tempfile::TempDir,
        db: Db,
        clock: Arc<Mutex<FakeClock>>,
        coord: Coordinator,
        epoch: String,
    }

    fn setup(task_status: TaskStatus, estimate: Option<&str>) -> Cmd {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(dir.path().join("w.db")).unwrap();
        migrate(db.connection()).unwrap();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        let meta = init_meta(&tx).unwrap();
        tx.execute(
            "INSERT INTO application_run(id, started_at) VALUES('run-1', 0)",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO project(id,name,row_version,status,created_at,updated_at)
             VALUES('p1','项目',0,'active',0,0)",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO task(id,project_id,title,status,estimated_json,row_version,
                              created_at,updated_at)
             VALUES('t1','p1','任务',?1,?2,0,0,0)",
            rusqlite::params![task_status.as_str(), estimate],
        )
        .unwrap();
        tx.commit().unwrap();

        let clock = Arc::new(Mutex::new(FakeClock::new(1_700_000_000_000, 0)));
        let coord = Coordinator::new(Box::new(Arc::clone(&clock)), "run-1");
        Cmd {
            _dir: dir,
            db,
            clock,
            coord,
            epoch: meta.data_epoch,
        }
    }

    impl Cmd {
        fn start_req(&self) -> StartRequest {
            StartRequest {
                expected_data_epoch: self.epoch.clone(),
                task_id: "t1".into(),
                task_expected_version: 0,
                mode: SessionMode::Foreground,
                timer_kind: TimerKind::Stopwatch,
                target_duration_ms: None,
                expected_interval_ms: 30_000,
            }
        }

        fn session_req(&self, session_id: &str, version: i64) -> SessionRequest {
            SessionRequest {
                expected_data_epoch: self.epoch.clone(),
                session_id: session_id.into(),
                session_expected_version: version,
            }
        }

        fn resume_req(&self, session_id: &str, tv: i64, sv: i64) -> ResumeRequest {
            ResumeRequest {
                expected_data_epoch: self.epoch.clone(),
                task_id: "t1".into(),
                task_expected_version: tv,
                session_id: session_id.into(),
                session_expected_version: sv,
            }
        }

        fn advance(&self, ms: i64) {
            self.clock.lock().unwrap().advance_both(ms);
        }

        fn advance_wall_only(&self, ms: i64) {
            self.clock.lock().unwrap().advance_wall(ms);
        }

        fn advance_mono_only(&self, ms: i64) {
            self.clock.lock().unwrap().advance_monotonic(ms);
        }

        fn task_status(&self) -> TaskStatus {
            task_repo::get_task(self.db.connection(), "t1")
                .unwrap()
                .unwrap()
                .status
        }

        fn task_version(&self) -> i64 {
            task_repo::get_task(self.db.connection(), "t1")
                .unwrap()
                .unwrap()
                .row_version
        }

        fn count(&self, table: &str) -> i64 {
            self.db
                .connection()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        }
    }

    /// `start` 建起完整链条：会话 + 区间 + 初始检查点 + 任务 Doing + 恰好一次 revision。
    #[test]
    fn start_builds_the_whole_chain() {
        let mut c = setup(TaskStatus::Ready, Some(r#"{"p50":60000}"#));
        let rev_before = require_meta(c.db.connection()).unwrap().revision;

        let req = c.start_req();
        let out = c.coord.start(&mut c.db, req).unwrap();

        assert_eq!(out.revision, rev_before + 1, "一次业务写只加一次");
        assert_eq!(c.task_status(), TaskStatus::Doing);
        assert_eq!(c.count("work_session"), 1);
        assert_eq!(c.count("work_interval"), 1);
        assert_eq!(c.count("interval_checkpoint"), 1, "初始检查点 elapsed=0");
        assert_eq!(c.count("task_change"), 2, "理清 + 起做各一条审计");
        assert_eq!(out.snapshot.state, Some(SessionState::Running));
        assert_eq!(out.snapshot.active_ms, 0, "刚起步暂计为 0");
    }

    /// **「理清」是两步**：02 §5 里 `Inbox → Doing` 不合法，必须 `Inbox → Ready → Doing`。
    #[test]
    fn start_from_inbox_clarifies_through_ready_first() {
        let mut c = setup(TaskStatus::Inbox, None);
        let req = c.start_req();
        c.coord.start(&mut c.db, req).unwrap();

        assert_eq!(c.task_status(), TaskStatus::Doing);
        assert_eq!(c.count("task_change"), 3, "冻结基准、Ready、Doing 各一条");
        // 版本链：冻结基准 0→1，Ready 1→2，Doing 2→3
        assert_eq!(c.task_version(), 3, "冻结基准也算一次任务改动");
    }

    /// 首次 start 冻结估时基准，之后再 start（新会话）不覆盖。
    #[test]
    fn start_freezes_the_estimate_baseline_once() {
        let mut c = setup(TaskStatus::Ready, Some(r#"{"p50":111}"#));
        let req = c.start_req();
        c.coord.start(&mut c.db, req).unwrap();
        let baseline = task_repo::read_estimate(c.db.connection(), "t1")
            .unwrap()
            .unwrap()
            .baseline_estimate_json;
        assert_eq!(baseline, Some(r#"{"p50":111}"#.into()));

        // 用户改估时后重开会话
        c.db.connection()
            .execute(
                "UPDATE task SET estimated_json='{\"p50\":999}' WHERE id='t1'",
                [],
            )
            .unwrap();
        c.advance(1_000);
        let sid = c.coord.live().unwrap().id.clone();
        let sv = c.coord.live().unwrap().row_version;
        let req = c.session_req(&sid, sv);
        c.coord.finish(&mut c.db, req).unwrap();

        let tv = c.task_version();
        let mut req = c.start_req();
        req.task_expected_version = tv;
        c.coord.start(&mut c.db, req).unwrap();

        let after = task_repo::read_estimate(c.db.connection(), "t1")
            .unwrap()
            .unwrap()
            .baseline_estimate_json;
        assert_eq!(
            after,
            Some(r#"{"p50":111}"#.into()),
            "基准不得被第二次 start 覆盖"
        );
    }

    /// `pause` 用已验证的单调差闭合区间，并把工时冻结下来。
    #[test]
    fn pause_closes_the_interval_with_a_verified_difference() {
        let mut c = setup(TaskStatus::Ready, None);
        let req = c.start_req();
        c.coord.start(&mut c.db, req).unwrap();
        c.advance(7_000);

        let sid = c.coord.live().unwrap().id.clone();
        let sv = c.coord.live().unwrap().row_version;
        let req = c.session_req(&sid, sv);
        let out = c.coord.pause(&mut c.db, req).unwrap();

        assert_eq!(out.snapshot.state, Some(SessionState::Paused));
        assert_eq!(out.snapshot.active_ms, 7_000);
        assert_eq!(c.count("work_interval"), 1);

        let iv = session_repo::intervals_of_session(c.db.connection(), &sid).unwrap();
        assert!(iv[0].ended_at.is_some(), "区间已闭合");
        assert_eq!(iv[0].duration_ms, Some(7_000), "可信闭合区间带 duration");
        assert_eq!(
            iv[0].sampled_end_wall_at,
            Some(1_700_000_007_000),
            "保留采样到的结束挂钟"
        );

        // 暂停后再走时间也不涨
        c.advance(60_000);
        let snap = c.coord.snapshot(&mut c.db).unwrap();
        assert_eq!(snap.active_ms, 7_000);
    }

    /// **`paused` 可直接 `finish`**（02 §3）。
    #[test]
    fn a_paused_session_can_finish_directly() {
        let mut c = setup(TaskStatus::Ready, None);
        let req = c.start_req();
        c.coord.start(&mut c.db, req).unwrap();
        c.advance(3_000);
        let sid = c.coord.live().unwrap().id.clone();
        let sv = c.coord.live().unwrap().row_version;
        let req = c.session_req(&sid, sv);
        c.coord.pause(&mut c.db, req).unwrap();

        let sv2 = c.coord.live().unwrap().row_version;
        let req = c.session_req(&sid, sv2);
        let out = c.coord.finish(&mut c.db, req).unwrap();
        assert_eq!(out.snapshot.state, Some(SessionState::Finished));
        assert_eq!(out.snapshot.active_ms, 3_000, "暂停期间的时长不计入");
        assert_eq!(
            c.count("work_interval"),
            1,
            "暂停时已闭合，finish 不再开新区间"
        );
    }

    /// `resume` **两份版本都要对**。
    #[test]
    fn resume_validates_both_versions() {
        let mut c = setup(TaskStatus::Ready, None);
        let req = c.start_req();
        c.coord.start(&mut c.db, req).unwrap();
        c.advance(2_000);
        let sid = c.coord.live().unwrap().id.clone();
        let sv = c.coord.live().unwrap().row_version;
        let req = c.session_req(&sid, sv);
        c.coord.pause(&mut c.db, req).unwrap();

        let tv = c.task_version();
        let sv2 = c.coord.live().unwrap().row_version;

        // 任务版本错
        let bad_task = c.resume_req(&sid, tv + 99, sv2);
        assert_eq!(
            c.coord.resume(&mut c.db, bad_task).unwrap_err().code(),
            "VERSION_CONFLICT"
        );

        // 会话版本错
        let bad_sess = c.resume_req(&sid, tv, sv2 + 99);
        assert_eq!(
            c.coord.resume(&mut c.db, bad_sess).unwrap_err().code(),
            "VERSION_CONFLICT"
        );

        // 都对才通过，并开新区间、不清空已用工时
        let req = c.resume_req(&sid, tv, sv2);
        let out = c.coord.resume(&mut c.db, req).unwrap();
        assert_eq!(out.snapshot.state, Some(SessionState::Running));
        assert_eq!(c.count("work_interval"), 2, "续接开新区间");
        c.advance(1_000);
        let snap = c.coord.snapshot(&mut c.db).unwrap();
        assert_eq!(snap.active_ms, 3_000, "2 秒旧工时 + 1 秒新暂计，不清空");
    }

    /// `resume` 拒绝不在 `Ready`/`Doing` 的任务——**不隐式解除等待、不重开已结束的任务**。
    #[test]
    fn resume_refuses_tasks_that_are_not_ready_or_doing() {
        for status in [
            TaskStatus::Waiting,
            TaskStatus::Blocked,
            TaskStatus::Done,
            TaskStatus::Inbox,
        ] {
            let mut c = setup(status, None);
            // 手工造一个 paused 会话（绕过 start，因为 start 会改任务状态）
            c.db.connection()
                .execute(
                    "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,
                                              started_at,row_version)
                     VALUES('s1','t1','run-1','FOREGROUND','paused','stopwatch',0,0)",
                    [],
                )
                .unwrap();
            let req = c.resume_req("s1", 0, 0);
            let err = c.coord.resume(&mut c.db, req).unwrap_err();
            assert_eq!(err.code(), "DOMAIN_ERROR", "{status:?} 不该被允许继续");
            assert_eq!(c.count("work_interval"), 0, "{status:?} 被拒时不得开区间");
        }
    }

    /// **旧 epoch 直接拒绝，不写入任何东西**（尤其不得借无效请求写异常事实）。
    #[test]
    fn a_stale_epoch_is_rejected_before_any_sampling_or_writing() {
        let mut c = setup(TaskStatus::Ready, None);
        let rev_before = require_meta(c.db.connection()).unwrap().revision;

        let mut req = c.start_req();
        req.expected_data_epoch = "epoch-from-a-previous-database".into();
        let err = c.coord.start(&mut c.db, req).unwrap_err();

        assert_eq!(err.code(), "DATA_EPOCH_MISMATCH");
        assert_eq!(c.count("work_session"), 0);
        assert_eq!(c.count("work_interval"), 0);
        assert_eq!(c.count("task_change"), 0);
        assert_eq!(c.task_status(), TaskStatus::Ready, "任务状态不得变化");
        assert_eq!(
            require_meta(c.db.connection()).unwrap().revision,
            rev_before
        );
    }

    /// **有效命令遇异常**：原意图不执行，但**独立系统恢复事务已提交**（总纲 §9）。
    ///
    /// Task 4 之后这条的行为是：
    /// - 用户命令返回 `RECOVERY_REQUIRED`，**不做它想做的事**（区间不由它闭合）；
    /// - 但检测到的异常作为**独立系统状态事务**落库——会话转 `recovering`、
    ///   区间被分割、写审计、`revision + 1`。
    ///
    /// 所以「被拒的用户命令自身不改 revision」这条要读准：这里加的那一次是**系统事务**
    /// 加的，不是用户命令加的。
    #[test]
    fn an_anomaly_makes_the_command_refuse_but_commits_the_recovery_transaction() {
        let mut c = setup(TaskStatus::Ready, None);
        let req = c.start_req();
        c.coord.start(&mut c.db, req).unwrap();
        let sid = c.coord.live().unwrap().id.clone();
        let sv = c.coord.live().unwrap().row_version;
        let sessions_before = c.count("work_session");
        let rev_before = require_meta(c.db.connection()).unwrap().revision;

        // 单调钟走 1 秒、挂钟走 31 秒 → 单拍增量差 30 秒，判为跳变。
        // 注意不能只 advance(30_000)：那样两个时钟同步推进，是正常的一拍，不是异常。
        c.advance_mono_only(1_000);
        c.advance_wall_only(31_000);
        let req = c.session_req(&sid, sv);
        let err = c.coord.pause(&mut c.db, req).unwrap_err();

        assert_eq!(err.code(), "RECOVERY_REQUIRED", "用户命令被拒");
        assert_eq!(c.count("work_session"), sessions_before, "不得新建会话");

        // 系统恢复事务已提交
        assert_eq!(
            require_meta(c.db.connection()).unwrap().revision,
            rev_before + 1,
            "系统事务自己加一次 revision"
        );
        assert_eq!(
            c.coord.live().unwrap().state,
            SessionState::Recovering,
            "会话已转 recovering"
        );

        // **原意图没执行**：pause 本该把区间闭合并置 paused；现在区间是被**分割**的
        let iv = session_repo::intervals_of_session(c.db.connection(), &sid).unwrap();
        assert_eq!(iv.len(), 1, "没有可信检查点 → 整段待确认，不新增区间");
        assert!(
            iv[0].needs_review,
            "区间是被标成待确认，不是被 pause 正常闭合"
        );
        assert_eq!(
            iv[0].duration_ms, None,
            "待确认段没有 duration——它不是已确认工时"
        );
    }

    /// **占用冲突**：前台只能有一个 running 会话（`uq_running_foreground`）。
    #[test]
    fn a_second_foreground_start_is_refused_by_the_index() {
        let mut c = setup(TaskStatus::Ready, None);
        let req = c.start_req();
        c.coord.start(&mut c.db, req).unwrap();

        // 第二个任务
        c.db.connection()
            .execute(
                "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
                 VALUES('t2','第二个','Ready',0,0,0)",
                [],
            )
            .unwrap();
        let mut req = c.start_req();
        req.task_id = "t2".into();
        let err = c.coord.start(&mut c.db, req).unwrap_err();

        assert_eq!(err.code(), "STORAGE_ERROR", "唯一索引应拦下第二个前台会话");
        assert_eq!(c.count("work_session"), 1);
    }

    /// `recovering` 拒绝普通工作命令，需 P3 的 `reconcile`。
    #[test]
    fn a_recovering_session_refuses_normal_commands() {
        let mut c = setup(TaskStatus::Ready, None);
        let req = c.start_req();
        c.coord.start(&mut c.db, req).unwrap();
        let sid = c.coord.live().unwrap().id.clone();
        c.db.connection()
            .execute(
                "UPDATE work_session SET state='recovering', needs_review=1 WHERE id=?1",
                [&sid],
            )
            .unwrap();
        c.coord.load_session(c.db.connection(), &sid).unwrap();
        let sv = c.coord.live().unwrap().row_version;

        let req = c.session_req(&sid, sv);
        assert_eq!(
            c.coord.pause(&mut c.db, req).unwrap_err().code(),
            "RECOVERY_REQUIRED"
        );
        let req = c.session_req(&sid, sv);
        assert_eq!(
            c.coord.finish(&mut c.db, req).unwrap_err().code(),
            "RECOVERY_REQUIRED"
        );
    }

    /// **采样失败不得伪造样本**：命令拒绝，且不写任何东西。
    #[test]
    fn a_failed_sample_makes_the_command_refuse() {
        let mut c = setup(TaskStatus::Ready, None);
        c.clock.lock().unwrap().fail_forever();

        let req = c.start_req();
        let err = c.coord.start(&mut c.db, req).unwrap_err();
        assert_eq!(err.code(), "RECOVERY_REQUIRED");
        assert_eq!(c.count("work_session"), 0);
        assert_eq!(c.task_status(), TaskStatus::Ready);
    }

    /// 整个链条的失败注入：末步骤故障 → 字段级回滚。
    #[test]
    fn a_failure_anywhere_leaves_no_partial_facts() {
        let mut c = setup(TaskStatus::Ready, Some(r#"{"p50":1}"#));
        // 把 task 的估时字段改成一个会撞 CHECK 的值不行——改成让 session 的预算非法：
        let mut req = c.start_req();
        req.timer_kind = TimerKind::Countdown;
        req.target_duration_ms = Some(0); // 倒计时预算必须为正 → 被拒

        let err = c.coord.start(&mut c.db, req).unwrap_err();
        assert_eq!(err.code(), "DOMAIN_ERROR");

        // 字段级核对
        assert_eq!(c.count("work_session"), 0, "不得留会话");
        assert_eq!(c.count("work_interval"), 0, "不得留区间");
        assert_eq!(c.count("interval_checkpoint"), 0, "不得留检查点");
        assert_eq!(c.task_status(), TaskStatus::Ready, "任务状态不得变化");
        assert_eq!(c.task_version(), 0, "任务版本不得前进");
        assert_eq!(
            task_repo::read_estimate(c.db.connection(), "t1")
                .unwrap()
                .unwrap()
                .baseline_estimate_json,
            None,
            "基准不得残留"
        );
        assert_eq!(c.count("task_change"), 0, "不得留审计");
    }
    /// **提交之后的失败不得返回普通可重试失败**（计划原文）。
    ///
    /// 事务已经落库了，客户端拿到可重试错误就会重发，而重发会**重复创建**
    /// （第二次 start 会开出第二个会话）。所以收尾失败一律是恢复语义。
    #[test]
    fn a_post_commit_failure_is_recovery_not_retryable() {
        let mut c = setup(TaskStatus::Ready, None);
        let sample = c.clock.lock().unwrap().sample().unwrap();

        // 直接走收尾入口，给一个不存在的会话——模拟「提交成功但内存重建失败」
        let err = c
            .coord
            .rebuild_from_committed(c.db.connection(), "no-such-session", sample)
            .unwrap_err();

        assert_eq!(err.code(), "RECOVERY_REQUIRED", "必须是恢复语义");
        for retryable in [
            "DOMAIN_ERROR",
            "STORAGE_ERROR",
            "VERSION_CONFLICT",
            "DATA_EPOCH_MISMATCH",
        ] {
            assert_ne!(err.code(), retryable, "{retryable} 会让客户端重发");
        }
        assert!(c.coord.live().is_none(), "内存不得保留冒充已提交状态的旧值");
        assert!(
            !err.message().contains("no-such-session"),
            "内部标识不进用户文案"
        );
    }

    /// 正常路径的收尾仍然成功，并返回**已提交事实**里的权威 revision。
    #[test]
    fn a_successful_commit_returns_the_authoritative_revision() {
        let mut c = setup(TaskStatus::Ready, None);
        let req = c.start_req();
        let out = c.coord.start(&mut c.db, req).unwrap();

        assert_eq!(
            out.revision,
            require_meta(c.db.connection()).unwrap().revision,
            "响应里的 revision 必须等于库里那个"
        );
        assert_eq!(
            out.snapshot.session_id.as_deref(),
            Some(c.coord.live().unwrap().id.as_str())
        );
    }
}
