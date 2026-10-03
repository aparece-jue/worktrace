//! P1 Task 4 要求的事务边界测试。
//!
//! 计划原文：同一事务理清任务 + 建立会话 + 初始检查点 + 审计，只加一次 revision；
//! 末步骤故障全部回滚；预算重新打开库后仍存在；并发前台 start 仅一次成功。
//!
//! 这里用一个**测试内的组合服务**代替 P2 的真实服务：P1 交付的是仓储原语，
//! 「谁拥有事务」这条规则必须由一个组合调用方来证明。

use worktrace_lib::domain::error::DomainError;
use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::domain::task::{TaskStatus, TransitionCause};
use worktrace_lib::error::AppError;
use worktrace_lib::storage::checkpoint_repo::{self, Checkpoint};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::guards::{guard_epoch, guard_row_version_of};
use worktrace_lib::storage::meta::{bump_revision, init_meta, read_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::session_repo;
use worktrace_lib::storage::task_repo;

/// 建库 + 迁移 + 元数据 + 一个 run + 一个项目。返回 (临时目录, Db, epoch)。
fn bootstrap() -> (tempfile::TempDir, Db, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();

    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let meta = init_meta(&tx).unwrap();
    tx.execute(
        "INSERT INTO application_run(id, started_at) VALUES('run-1', 1000)",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO project(id,name,row_version,status,created_at,updated_at)
         VALUES('p1','项目',0,'active',1000,1000)",
        [],
    )
    .unwrap();
    // 任务也在这里建：`start_session` 的第一步就是校验它的版本，
    // 少了它每个用例都会以「no such task」失败。
    // 走仓储而不是裸 INSERT，这样审计记录数与后面的断言口径一致。
    task_repo::create_task(&tx, "t1", "任务", Some("p1"), 1000).unwrap();
    // 02 §5 里 Inbox 不能直接到 Doing，必须先 Ready。建完就理清好，
    // 后续用例从 Ready 出发，task_version 因此是 1。
    task_repo::transition_task(&tx, "t1", 0, TaskStatus::Ready, TransitionCause::User, 1000)
        .unwrap();
    tx.commit().unwrap();

    (dir, db, meta.data_epoch)
}

/// 组合服务：理清任务 + 建立会话 + 初始检查点 + 审计，**一个事务、一次 revision**。
#[allow(clippy::too_many_arguments)]
fn start_session(
    db: &mut Db,
    epoch: &str,
    task_id: &str,
    task_version: i64,
    session_id: &str,
    interval_id: &str,
    at: i64,
) -> Result<i64, AppError> {
    let tx = db
        .connection_mut()
        .unchecked_transaction()
        .map_err(|e| AppError::Storage {
            detail: e.to_string(),
        })?;

    // ① 请求校验必须在写事务内
    guard_epoch(&tx, epoch)?;
    guard_row_version_of(&tx, "task", task_id, task_version)?;

    // ② 任务理清到 Doing
    task_repo::transition_task(
        &tx,
        task_id,
        task_version,
        TaskStatus::Doing,
        TransitionCause::User,
        at,
    )?;

    // ③ 会话 + 初始区间
    session_repo::create_session(
        &tx,
        session_id,
        task_id,
        "run-1",
        SessionMode::Foreground,
        TimerKind::Countdown,
        Some(25 * 60 * 1000),
        at,
        interval_id,
    )?;

    // ④ 初始检查点 elapsed=0
    checkpoint_repo::write(
        &tx,
        &Checkpoint {
            interval_id: interval_id.to_string(),
            run_id: "run-1".to_string(),
            wall_at: at,
            attribution_at: at,
            elapsed_ms: 0,
        },
    )?;

    // ⑤ 一次业务写恰好加一次 revision
    let rev = bump_revision(&tx)?;
    tx.commit().map_err(|e| AppError::Storage {
        detail: e.to_string(),
    })?;
    Ok(rev)
}

fn count(db: &Db, table: &str) -> i64 {
    db.connection()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

// ─────────────────────────────────────────────────────────────────────────────

/// 一个事务里做完四件事，revision **恰好 +1**（不是 +4）。
#[test]
fn one_business_write_bumps_revision_exactly_once() {
    let (_dir, mut db, epoch) = bootstrap();
    let before = require_meta(db.connection()).unwrap().revision;

    let rev = start_session(&mut db, &epoch, "t1", 1, "s1", "i1", 5000).unwrap();

    assert_eq!(rev, before + 1, "一次业务写只加一次");

    // 四类事实都落库了
    assert_eq!(
        task_repo::get_task(db.connection(), "t1")
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Doing
    );
    assert_eq!(
        session_repo::get_session(db.connection(), "s1")
            .unwrap()
            .unwrap()
            .state,
        SessionState::Running
    );
    assert_eq!(count(&db, "work_interval"), 1);
    assert_eq!(
        checkpoint_repo::latest(db.connection(), "i1")
            .unwrap()
            .unwrap()
            .elapsed_ms,
        0
    );
    assert_eq!(
        count(&db, "task_change"),
        3,
        "创建 + 理清 + 起做，各一条审计"
    );
}

/// 计划原文：「末步骤故障全部回滚」。
///
/// 在最后一步（写检查点）注入失败——检查点的 `run_id` 与 session 不符，
/// 仓储会拒绝。整批必须回到调用前。
#[test]
fn a_failure_in_the_last_step_rolls_back_everything() {
    let (_dir, mut db, epoch) = bootstrap();
    let rev_before = require_meta(db.connection()).unwrap().revision;

    let tx = db.connection_mut().unchecked_transaction().unwrap();
    guard_epoch(&tx, &epoch).unwrap();
    task_repo::transition_task(&tx, "t1", 1, TaskStatus::Doing, TransitionCause::User, 5000)
        .unwrap();
    session_repo::create_session(
        &tx,
        "s1",
        "t1",
        "run-1",
        SessionMode::Foreground,
        TimerKind::Stopwatch,
        None,
        5000,
        "i1",
    )
    .unwrap();
    // ← 末步骤：run_id 与 session 不符，检查点仓储必须拒绝
    let err = checkpoint_repo::write(
        &tx,
        &Checkpoint {
            interval_id: "i1".into(),
            run_id: "run-STRANGER".into(),
            wall_at: 5000,
            attribution_at: 5000,
            elapsed_ms: 0,
        },
    )
    .unwrap_err();
    // 不是普通的参数错误：run_id 对不上说明内存里还留着上一轮 run 的基线，
    // 必须走恢复流程，所以契约码是 RECOVERY_REQUIRED 而不是 DOMAIN_ERROR。
    assert_eq!(err.code(), "RECOVERY_REQUIRED");
    drop(tx); // 未提交 = 回滚

    // 字段级核对：不只是行数
    assert_eq!(
        require_meta(db.connection()).unwrap().revision,
        rev_before,
        "revision 不得前进"
    );
    assert_eq!(count(&db, "work_session"), 0, "会话不得残留");
    assert_eq!(count(&db, "work_interval"), 0, "区间不得残留");
    assert_eq!(count(&db, "interval_checkpoint"), 0, "检查点不得残留");
    assert_eq!(count(&db, "task_change"), 2, "只应有创建与理清那两条审计");

    let task = task_repo::get_task(db.connection(), "t1").unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Ready, "任务状态不得变化");
    assert_eq!(task.row_version, 1, "版本不得变化");
}

/// 计划原文：「预算重新打开库后仍存在」。
#[test]
fn the_timer_budget_survives_reopening_the_database() {
    let (dir, mut db, epoch) = bootstrap();
    start_session(&mut db, &epoch, "t1", 1, "s1", "i1", 5000).unwrap();
    let path = db.path().unwrap().to_path_buf();
    drop(db);

    let db2 = Db::open(&path).unwrap();
    let s = session_repo::get_session(db2.connection(), "s1")
        .unwrap()
        .unwrap();
    assert_eq!(s.timer_kind, TimerKind::Countdown);
    assert_eq!(
        s.target_duration_ms,
        Some(25 * 60 * 1000),
        "倒计时预算必须持久化"
    );
    assert_eq!(s.mode, SessionMode::Foreground);
    let _ = dir;
}

/// 计划原文：「并发前台 start 仅一次成功」。
///
/// 唯一索引 `uq_running_foreground` 是结构性保证——两个连接同时开前台会话，
/// 第二个必然失败，不需要应用层加锁。
#[test]
fn a_second_concurrent_foreground_start_is_refused_by_the_index() {
    let (dir, mut db, epoch) = bootstrap();
    start_session(&mut db, &epoch, "t1", 1, "s1", "i1", 5000).unwrap();

    // 第二个任务、第二个连接
    let path = db.path().unwrap().to_path_buf();
    let mut db2 = Db::open(&path).unwrap();
    let tx = db2.connection_mut().unchecked_transaction().unwrap();
    tx.execute(
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t2','第二个','Doing',0,6000,6000)",
        [],
    )
    .unwrap();
    let err = session_repo::create_session(
        &tx,
        "s2",
        "t2",
        "run-1",
        SessionMode::Foreground,
        TimerKind::Stopwatch,
        None,
        6000,
        "i2",
    )
    .unwrap_err();
    assert_eq!(err.code(), "STORAGE_ERROR", "唯一索引应拒绝第二个前台会话");
    drop(tx);

    assert_eq!(count(&db2, "work_session"), 1, "只能有一个会话");
    let _ = (dir, epoch);
}

/// 请求 epoch 不符时：**不写入任何东西**，且返回 DATA_EPOCH_MISMATCH。
#[test]
fn a_stale_epoch_request_writes_nothing() {
    let (_dir, mut db, _epoch) = bootstrap();
    let rev_before = require_meta(db.connection()).unwrap().revision;

    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let err = guard_epoch(&tx, "epoch-from-a-previous-database").unwrap_err();
    assert_eq!(err.code(), "DATA_EPOCH_MISMATCH");
    drop(tx);

    assert_eq!(require_meta(db.connection()).unwrap().revision, rev_before);
    assert_eq!(count(&db, "work_session"), 0);
    assert_eq!(count(&db, "work_interval"), 0);
}

/// 版本冲突与「未知记录」必须可区分——前者刷新后重试，后者是调用方搞错了 id。
#[test]
fn version_conflict_and_unknown_record_are_distinguishable() {
    let (_dir, mut db, _epoch) = bootstrap();

    let tx = db.connection_mut().unchecked_transaction().unwrap();

    let unknown = guard_row_version_of(&tx, "task", "no-such-task", 0).unwrap_err();
    assert_eq!(unknown.code(), "DOMAIN_ERROR", "未知记录不是版本冲突");

    let conflict = guard_row_version_of(&tx, "task", "t1", 7).unwrap_err();
    assert_eq!(conflict.code(), "VERSION_CONFLICT");
    match conflict {
        AppError::VersionConflict { expected, actual } => {
            assert_eq!((expected, actual), (7, 1), "要带上期望与实际供诊断");
        }
        other => panic!("应为 VersionConflict，实际 {other:?}"),
    }
    drop(tx);
}

/// 领域错误到契约码的映射：待恢复独立成码，非法状态仍可辨。
#[test]
fn domain_errors_map_to_distinguishable_contract_codes() {
    let recovering: AppError = DomainError::UntrustedSample {
        reason: "clock went backwards",
    }
    .into();
    assert_eq!(
        recovering.code(),
        "RECOVERY_REQUIRED",
        "时钟不可信应触发恢复流程"
    );

    let illegal: AppError = DomainError::IllegalTransition {
        from: "Inbox",
        to: "Review",
    }
    .into();
    assert_eq!(illegal.code(), "DOMAIN_ERROR");

    let occupied: AppError = DomainError::IntervalOpenInWrongState {
        state: "recovering",
    }
    .into();
    assert_eq!(occupied.code(), "DOMAIN_ERROR");

    // 同码但 detail 必须可辨，否则诊断日志里分不出是哪种拒绝。
    assert_ne!(illegal.detail(), occupied.detail());
    assert!(illegal.detail().unwrap().contains("Inbox"));
}

/// 心跳检查点**不加 revision**——这是 00 §5 明写的例外，必须钉住。
#[test]
fn writing_a_checkpoint_does_not_bump_revision() {
    let (_dir, mut db, epoch) = bootstrap();
    start_session(&mut db, &epoch, "t1", 1, "s1", "i1", 5000).unwrap();
    let rev_after_start = require_meta(db.connection()).unwrap().revision;

    // 模拟几次心跳（各自一个短事务，不加 revision）
    for i in 1..=3 {
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        checkpoint_repo::write(
            &tx,
            &Checkpoint {
                interval_id: "i1".into(),
                run_id: "run-1".into(),
                wall_at: 5000 + i * 1000,
                attribution_at: 5000 + i * 1000,
                elapsed_ms: i * 1000,
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    assert_eq!(
        require_meta(db.connection()).unwrap().revision,
        rev_after_start,
        "心跳不得加 revision"
    );
    assert_eq!(
        checkpoint_repo::latest(db.connection(), "i1")
            .unwrap()
            .unwrap()
            .elapsed_ms,
        3000,
        "检查点应被覆盖为最后一次"
    );
    assert_eq!(
        count(&db, "interval_checkpoint"),
        1,
        "一个区间只有一条检查点"
    );
}

/// 闭合区间不接受再写检查点——给已结束的区间写心跳是无意义的。
#[test]
fn a_closed_interval_refuses_new_checkpoints() {
    let (_dir, mut db, epoch) = bootstrap();
    start_session(&mut db, &epoch, "t1", 1, "s1", "i1", 5000).unwrap();

    let tx = db.connection_mut().unchecked_transaction().unwrap();
    session_repo::close_interval(
        &tx,
        "i1",
        worktrace_lib::domain::interval::ClosedIntervalFacts {
            ended_at: 9_000,
            duration_ms: Some(4_000),
            sampled_end_wall_at: 9_000,
            needs_review: false,
        },
    )
    .unwrap();
    let err = checkpoint_repo::write(
        &tx,
        &Checkpoint {
            interval_id: "i1".into(),
            run_id: "run-1".into(),
            wall_at: 9_500,
            attribution_at: 9_500,
            elapsed_ms: 4_500,
        },
    )
    .unwrap_err();
    assert_eq!(err.code(), "DOMAIN_ERROR");
    drop(tx);
}

/// `read_meta` 在未初始化的库上返回 `None`，不是报错。
#[test]
fn read_meta_is_none_before_initialisation() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();
    assert!(read_meta(db.connection()).unwrap().is_none());
}
#[test]
fn invalid_checkpoints_preserve_the_last_trusted_fact() {
    let (_dir, mut db, epoch) = bootstrap();
    start_session(&mut db, &epoch, "t1", 1, "s1", "i1", 1000).unwrap();
    let initial = checkpoint_repo::latest(db.connection(), "i1")
        .unwrap()
        .unwrap();
    for (wall, attribution, elapsed) in [(1100, 1200, 100), (999, 1000, 0), (1100, 999, 0)] {
        let tx = db.connection_mut().transaction().unwrap();
        assert!(checkpoint_repo::write(
            &tx,
            &Checkpoint {
                interval_id: "i1".into(),
                run_id: "run-1".into(),
                wall_at: wall,
                attribution_at: attribution,
                elapsed_ms: elapsed,
            }
        )
        .is_err());
        tx.commit().unwrap();
        assert_eq!(
            checkpoint_repo::latest(db.connection(), "i1").unwrap(),
            Some(initial.clone())
        );
    }
    for sql in [
        "UPDATE work_interval SET needs_review=1 WHERE id='i1'",
        "UPDATE work_interval SET voided_at=1000 WHERE id='i1'",
        "UPDATE work_session SET state='recovering', needs_review=1 WHERE id='s1'",
    ] {
        let tx = db.connection_mut().transaction().unwrap();
        tx.execute(sql, []).unwrap();
        assert!(checkpoint_repo::write(&tx, &initial).is_err());
        tx.rollback().unwrap();
    }
    assert!(db
        .connection()
        .execute(
            "INSERT INTO interval_checkpoint VALUES(NULL,'run-1',1000,1000,0)",
            []
        )
        .is_err());
}

#[test]
fn review_to_ready_clears_quality_and_audits_actual_values() {
    let (_dir, mut db, _) = bootstrap();
    let tx = db.connection_mut().transaction().unwrap();
    tx.execute(
        "UPDATE task SET status='Review', quality='good' WHERE id='t1'",
        [],
    )
    .unwrap();
    let row =
        task_repo::transition_task(&tx, "t1", 1, TaskStatus::Ready, TransitionCause::User, 2000)
            .unwrap();
    assert_eq!(row.quality, None);
    let (before, after): (String, String) = tx.query_row(
        "SELECT before_json, after_json FROM task_change WHERE task_id='t1' AND created_at=2000", [],
        |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&before).unwrap()["quality"],
        "good"
    );
    assert!(serde_json::from_str::<serde_json::Value>(&after).unwrap()["quality"].is_null());
    let row = task_repo::create_task(&tx, "trimmed", "  padded  ", None, 3000).unwrap();
    let json: String = tx
        .query_row(
            "SELECT after_json FROM task_change WHERE task_id='trimmed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap()["title"],
        row.title
    );
    tx.commit().unwrap();
}

#[test]
fn invalid_persisted_enums_are_rejected_instead_of_guessed() {
    let (_dir, mut db, epoch) = bootstrap();
    start_session(&mut db, &epoch, "t1", 1, "s1", "i1", 1000).unwrap();
    db.connection().execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE task SET status='broken' WHERE id='t1'; UPDATE work_session SET state='broken' WHERE id='s1';").unwrap();
    assert!(task_repo::get_task(db.connection(), "t1").is_err());
    assert!(session_repo::get_session(db.connection(), "s1").is_err());
}

/// 库里的枚举值非法时必须**失败且说清是哪一列**，不许回落到默认值。
///
/// 这条守两件事：
/// 1. 回落（`unwrap_or(Inbox)`）会把读不懂的数据伪装成合法状态——比报错危险得多；
/// 2. 诊断文本必须指向那一列。`rusqlite::Error::InvalidQuery` 的文案是
///    "Query is not read-only"，会把人引到完全错误的方向。
///
/// 场景用 `PRAGMA ignore_check_constraints` 构造：它模拟的正是「CHECK 被绕过」
/// 或「更新版本写入的新取值被旧版本读到」。
#[test]
fn an_unknown_enum_value_fails_loudly_and_names_the_column() {
    let (_dir, db, _epoch) = bootstrap();

    db.connection()
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    db.connection()
        .execute("UPDATE task SET status='GARBAGE' WHERE id='t1'", [])
        .expect("绕过 CHECK 后应能写入非法状态");
    db.connection()
        .execute_batch("PRAGMA ignore_check_constraints=OFF")
        .unwrap();

    let err = task_repo::get_task(db.connection(), "t1").unwrap_err();
    assert_eq!(err.code(), "STORAGE_ERROR", "读坏数据是存储层失败");

    let detail = err.detail().unwrap_or_default();
    assert!(
        detail.contains("task.status"),
        "诊断必须点名是哪一列，实际：{detail}"
    );
    assert!(
        detail.contains("GARBAGE"),
        "诊断应带上读到的值，实际：{detail}"
    );
    assert!(
        !detail.contains("read-only"),
        "不得出现 rusqlite InvalidQuery 的误导文案，实际：{detail}"
    );
    assert!(
        !err.message().contains("GARBAGE"),
        "库内容不得进入面向用户的文案：{}",
        err.message()
    );
}
