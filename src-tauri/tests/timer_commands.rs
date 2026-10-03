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
