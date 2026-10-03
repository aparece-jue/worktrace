//! P2 留给后续计划的两个接缝。
//!
//! - **P3**：同一事务中的暂停/结束原语（`services::timer::primitives`）。
//!   组合服务要在**自己的一次事务**里结束会话，不能调用会自行提交的公开命令。
//! - **P5**：内部统计采样接缝（`Coordinator::stats_sample`）。
//!   闭合工时与 live 暂计必须来自同一个瞬间，否则同一段时间会被算两遍。

use std::sync::{Arc, Mutex};

use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::domain::task::TaskStatus;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::timer::coordinator::{Coordinator, StartRequest};
use worktrace_lib::services::timer::primitives::{
    end_session_in_tx, open_interval_of, EndSessionFacts,
};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{bump_revision, init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::session_repo;
use worktrace_lib::storage::task_repo;

struct H {
    _dir: tempfile::TempDir,
    db: Db,
    clock: Arc<Mutex<FakeClock>>,
    coord: Coordinator,
    epoch: String,
}

fn setup() -> H {
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
        "INSERT INTO task(id,project_id,title,status,row_version,created_at,updated_at)
         VALUES('t1','p1','任务','Ready',0,0,0)",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    let clock = Arc::new(Mutex::new(FakeClock::new(1_700_000_000_000, 0)));
    let coord = Coordinator::new(Box::new(Arc::clone(&clock)), "run-1");
    H {
        _dir: dir,
        db,
        clock,
        coord,
        epoch: meta.data_epoch,
    }
}

impl H {
    fn start(&mut self) -> String {
        let req = StartRequest {
            expected_data_epoch: self.epoch.clone(),
            task_id: "t1".into(),
            task_expected_version: 0,
            mode: SessionMode::Foreground,
            timer_kind: TimerKind::Stopwatch,
            target_duration_ms: None,
            expected_interval_ms: 30_000,
        };
        self.coord
            .start(&mut self.db, req)
            .unwrap()
            .snapshot
            .session_id
            .unwrap()
    }

    fn advance(&self, ms: i64) {
        self.clock.lock().unwrap().advance_both(ms);
    }

    fn revision(&self) -> i64 {
        require_meta(self.db.connection()).unwrap().revision
    }

    fn session_state(&self, sid: &str) -> SessionState {
        session_repo::get_session(self.db.connection(), sid)
            .unwrap()
            .unwrap()
            .state
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// P3 接缝：事务内原语
// ─────────────────────────────────────────────────────────────────────────────

/// 原语在**调用方的事务里**工作：事务回滚，它就什么都没留下。
#[test]
fn the_primitive_rolls_back_with_the_callers_transaction() {
    let mut h = setup();
    let sid = h.start();
    h.advance(7_000);
    h.coord.load_session(h.db.connection(), &sid).unwrap();
    let version = h.coord.live().unwrap().row_version;
    let attributed_end = 1_700_000_007_000;

    let tx = h.db.connection_mut().unchecked_transaction().unwrap();
    end_session_in_tx(
        &tx,
        &EndSessionFacts {
            run_id: "run-1".into(),
            session_id: sid.clone(),
            expected_row_version: version,
            attributed_end,
            sampled_end_wall_at: attributed_end,
            target_state: SessionState::Paused,
        },
    )
    .unwrap();
    tx.rollback().unwrap();

    assert_eq!(
        h.session_state(&sid),
        SessionState::Running,
        "回滚后会话仍是 running"
    );
    let ivs = session_repo::intervals_of_session(h.db.connection(), &sid).unwrap();
    assert_eq!(ivs[0].ended_at, None, "回滚后区间仍开放");
}

/// **原语不加 revision**——一次业务事务只加一次，由拥有事务的那一方加。
#[test]
fn the_primitive_does_not_bump_revision() {
    let mut h = setup();
    let sid = h.start();
    h.advance(7_000);
    h.coord.load_session(h.db.connection(), &sid).unwrap();
    let version = h.coord.live().unwrap().row_version;
    let rev_before = h.revision();

    let tx = h.db.connection_mut().unchecked_transaction().unwrap();
    end_session_in_tx(
        &tx,
        &EndSessionFacts {
            run_id: "run-1".into(),
            session_id: sid.clone(),
            expected_row_version: version,
            attributed_end: 1_700_000_007_000,
            sampled_end_wall_at: 1_700_000_007_000,
            target_state: SessionState::Paused,
        },
    )
    .unwrap();
    // 在**同一个事务里**读 revision——比事务外读更准
    let rev_inside: i64 = tx
        .query_row(
            "SELECT revision FROM app_meta WHERE singleton = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        rev_inside, rev_before,
        "原语不得自己加 revision（还没提交呢）"
    );
    tx.commit().unwrap();
    assert_eq!(
        h.revision(),
        rev_before,
        "提交了也不加——加 revision 是调用方的事"
    );
}

/// 组合服务的真实用法：**一次事务**里改任务状态 + 结束会话 + 加一次 revision。
#[test]
fn a_composed_service_can_end_a_session_inside_its_own_transaction() {
    let mut h = setup();
    let sid = h.start();
    h.advance(9_000);
    h.coord.load_session(h.db.connection(), &sid).unwrap();
    let version = h.coord.live().unwrap().row_version;
    let task_version = task_repo::get_task(h.db.connection(), "t1")
        .unwrap()
        .unwrap()
        .row_version;
    let rev_before = h.revision();

    // 组合服务：完成任务的同一次事务里结束会话
    let tx = h.db.connection_mut().unchecked_transaction().unwrap();
    end_session_in_tx(
        &tx,
        &EndSessionFacts {
            run_id: "run-1".into(),
            session_id: sid.clone(),
            expected_row_version: version,
            attributed_end: 1_700_000_009_000,
            sampled_end_wall_at: 1_700_000_009_000,
            target_state: SessionState::Finished,
        },
    )
    .unwrap();
    task_repo::transition_task(
        &tx,
        "t1",
        task_version,
        TaskStatus::Done,
        worktrace_lib::domain::task::TransitionCause::User,
        1_700_000_009_000,
    )
    .unwrap();
    let rev = bump_revision(&tx).unwrap();
    tx.commit().unwrap();

    assert_eq!(rev, rev_before + 1, "整次业务写恰好一次 revision");
    assert_eq!(h.session_state(&sid), SessionState::Finished);
    assert_eq!(
        task_repo::get_task(h.db.connection(), "t1")
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Done
    );
    let ivs = session_repo::intervals_of_session(h.db.connection(), &sid).unwrap();
    assert_eq!(ivs[0].duration_ms, Some(9_000), "区间按归属终点闭合");
}

/// `Running` 之外的会话调原语要报错——组合服务不该在没计时的时候结束。
#[test]
fn the_primitive_reflects_session_state_correctly() {
    let mut h = setup();
    let sid = h.start();
    h.advance(3_000);
    h.coord.load_session(h.db.connection(), &sid).unwrap();

    // 先正常暂停（区间关闭）
    let version = h.coord.live().unwrap().row_version;
    let tx = h.db.connection_mut().unchecked_transaction().unwrap();
    end_session_in_tx(
        &tx,
        &EndSessionFacts {
            run_id: "run-1".into(),
            session_id: sid.clone(),
            expected_row_version: version,
            attributed_end: 1_700_000_003_000,
            sampled_end_wall_at: 1_700_000_003_000,
            target_state: SessionState::Paused,
        },
    )
    .unwrap();
    tx.commit().unwrap();

    // paused 状态没有开放区间
    assert_eq!(
        open_interval_of(
            &h.db.connection_mut().unchecked_transaction().unwrap(),
            &sid
        )
        .unwrap_err()
        .code(),
        "DOMAIN_ERROR"
    );

    // **paused 可以直接结束**（02 §3），且不再有区间可关
    let v2 = h.session_state(&sid);
    assert_eq!(v2, SessionState::Paused);
    let row = session_repo::get_session(h.db.connection(), &sid)
        .unwrap()
        .unwrap();
    let tx = h.db.connection_mut().unchecked_transaction().unwrap();
    end_session_in_tx(
        &tx,
        &EndSessionFacts {
            run_id: "run-1".into(),
            session_id: sid.clone(),
            expected_row_version: row.row_version,
            attributed_end: 1_700_000_005_000,
            sampled_end_wall_at: 1_700_000_005_000,
            target_state: SessionState::Finished,
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(h.session_state(&sid), SessionState::Finished);
    let ivs = session_repo::intervals_of_session(h.db.connection(), &sid).unwrap();
    assert_eq!(ivs.len(), 1, "不得为 paused 会话新开或新关区间");
}

/// `recovering` 拒绝，且不许把会话「结束」进恢复态。
#[test]
fn the_primitive_refuses_recovery_in_both_directions() {
    let mut h = setup();
    let sid = h.start();
    let row = session_repo::get_session(h.db.connection(), &sid)
        .unwrap()
        .unwrap();

    // 不许结束进 recovering
    let tx = h.db.connection_mut().unchecked_transaction().unwrap();
    assert_eq!(
        end_session_in_tx(
            &tx,
            &EndSessionFacts {
                run_id: "run-1".into(),
                session_id: sid.clone(),
                expected_row_version: row.row_version,
                attributed_end: 1_700_000_001_000,
                sampled_end_wall_at: 1_700_000_001_000,
                target_state: SessionState::Recovering,
            },
        )
        .unwrap_err()
        .code(),
        "DOMAIN_ERROR"
    );
    tx.rollback().unwrap();

    // 已经是 recovering 的会话拒绝被结束
    h.db.connection()
        .execute(
            "UPDATE work_session SET state='recovering' WHERE id=?1",
            [&sid],
        )
        .unwrap();
    let row = session_repo::get_session(h.db.connection(), &sid)
        .unwrap()
        .unwrap();
    let tx = h.db.connection_mut().unchecked_transaction().unwrap();
    assert_eq!(
        end_session_in_tx(
            &tx,
            &EndSessionFacts {
                run_id: "run-1".into(),
                session_id: sid.clone(),
                expected_row_version: row.row_version,
                attributed_end: 1_700_000_001_000,
                sampled_end_wall_at: 1_700_000_001_000,
                target_state: SessionState::Finished,
            },
        )
        .unwrap_err()
        .code(),
        "RECOVERY_REQUIRED"
    );
    tx.rollback().unwrap();
}

/// 版本不符要拒绝（原语也守乐观并发）。
#[test]
fn the_primitive_guards_the_row_version() {
    let mut h = setup();
    let sid = h.start();
    let tx = h.db.connection_mut().unchecked_transaction().unwrap();
    assert_eq!(
        end_session_in_tx(
            &tx,
            &EndSessionFacts {
                run_id: "run-1".into(),
                session_id: sid.clone(),
                expected_row_version: 99,
                attributed_end: 1_700_000_001_000,
                sampled_end_wall_at: 1_700_000_001_000,
                target_state: SessionState::Paused,
            },
        )
        .unwrap_err()
        .code(),
        "VERSION_CONFLICT"
    );
    tx.rollback().unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────
// P5 接缝：统计采样
// ─────────────────────────────────────────────────────────────────────────────

/// 闭合工时与 live 暂计**分列且互斥**，加起来就是总工时。
#[test]
fn stats_split_closed_and_live_without_double_counting() {
    let mut h = setup();
    let sid = h.start();

    // 先正常工作 20 秒再暂停 → 20 秒可信闭合
    h.advance(20_000);
    h.coord.load_session(h.db.connection(), &sid).unwrap();
    let version = h.coord.live().unwrap().row_version;
    let tx = h.db.connection_mut().unchecked_transaction().unwrap();
    end_session_in_tx(
        &tx,
        &EndSessionFacts {
            run_id: "run-1".into(),
            session_id: sid.clone(),
            expected_row_version: version,
            attributed_end: 1_700_000_020_000,
            sampled_end_wall_at: 1_700_000_020_000,
            target_state: SessionState::Paused,
        },
    )
    .unwrap();
    tx.commit().unwrap();

    // 再开一段（用 resume 走正常路径）
    let tv = task_repo::get_task(h.db.connection(), "t1")
        .unwrap()
        .unwrap()
        .row_version;
    let sv = session_repo::get_session(h.db.connection(), &sid)
        .unwrap()
        .unwrap()
        .row_version;
    let req = worktrace_lib::services::timer::coordinator::ResumeRequest {
        expected_data_epoch: h.epoch.clone(),
        task_id: "t1".into(),
        task_expected_version: tv,
        session_id: sid.clone(),
        session_expected_version: sv,
    };
    h.coord.resume(&mut h.db, req).unwrap();
    h.advance(7_000);

    let s = h.coord.stats_sample(&mut h.db).unwrap();
    assert_eq!(s.closed_trusted_ms, 20_000, "已确认闭合 20 秒");
    assert_eq!(s.live_ms, 7_000, "当前开放区间暂计 7 秒");
    assert_eq!(
        s.total_ms(),
        27_000,
        "两者互斥，加起来就是 27 秒——不是 34 也不是 54"
    );
    assert_eq!(s.state, Some(SessionState::Running));
    assert!(s.open_interval_id.is_some());
    assert_eq!(s.session_id.as_deref(), Some(sid.as_str()));
    assert_eq!(s.run_id, "run-1");
}

/// 归属终点由接缝一次给出——调用方不用自己再取挂钟，也就不会取到第二个瞬间。
#[test]
fn stats_gives_one_attribution_endpoint() {
    let mut h = setup();
    h.start();
    h.advance(5_000);
    let s = h.coord.stats_sample(&mut h.db).unwrap();
    assert_eq!(s.attributed_end, 1_700_000_005_000, "A(M) 一次算好");
    assert_eq!(s.live_ms, 5_000, "暂计就是到该终点为止");
}

/// 暂停期间没有开放区间：live 为 0，closed 保留。
#[test]
fn stats_has_no_live_time_while_paused() {
    let mut h = setup();
    let sid = h.start();
    h.advance(12_000);
    h.coord.load_session(h.db.connection(), &sid).unwrap();
    let version = h.coord.live().unwrap().row_version;
    let tx = h.db.connection_mut().unchecked_transaction().unwrap();
    end_session_in_tx(
        &tx,
        &EndSessionFacts {
            run_id: "run-1".into(),
            session_id: sid.clone(),
            expected_row_version: version,
            attributed_end: 1_700_000_012_000,
            sampled_end_wall_at: 1_700_000_012_000,
            target_state: SessionState::Paused,
        },
    )
    .unwrap();
    tx.commit().unwrap();
    h.coord.load_session(h.db.connection(), &sid).unwrap();

    h.advance(600_000);
    let s = h.coord.stats_sample(&mut h.db).unwrap();
    assert_eq!(s.closed_trusted_ms, 12_000);
    assert_eq!(s.live_ms, 0, "暂停期间没有 live");
    assert!(s.open_interval_id.is_none());
}

/// 没有会话时给一个空的统计采样，而不是报错。
#[test]
fn stats_without_a_session_is_empty_not_an_error() {
    let mut h = setup();
    let s = h.coord.stats_sample(&mut h.db).unwrap();
    assert_eq!(s.session_id, None);
    assert_eq!(s.closed_trusted_ms, 0);
    assert_eq!(s.live_ms, 0);
    assert_eq!(s.total_ms(), 0);
    assert!(s.open_interval_id.is_none());
}
