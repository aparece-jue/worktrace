//! P1/P2 评审问题的行为回归测试。
//!
//! 计划要求：正常心跳约每 30 秒写检查点且**不加 revision**；失败不推进持久化标记、
//! 后续可重试；**不可信样本不得写可信检查点**；内存里的可信点不能当恢复事实。

use std::sync::{Arc, Mutex};

use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::timer::coordinator::{Coordinator, StartRequest};
use worktrace_lib::storage::checkpoint_repo;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;

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
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Ready',0,0,0)",
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
    fn start(&mut self) {
        let req = StartRequest {
            expected_data_epoch: self.epoch.clone(),
            task_id: "t1".into(),
            task_expected_version: 0,
            mode: SessionMode::Foreground,
            timer_kind: TimerKind::Stopwatch,
            target_duration_ms: None,
            expected_interval_ms: 30_000,
        };
        self.coord.start(&mut self.db, req).unwrap();
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

    fn revision(&self) -> i64 {
        require_meta(self.db.connection()).unwrap().revision
    }

    fn checkpoints(&self, interval_id: &str) -> Option<checkpoint_repo::Checkpoint> {
        checkpoint_repo::latest(self.db.connection(), interval_id).unwrap()
    }
}

use worktrace_lib::domain::session::SessionState;
use worktrace_lib::platform::clock::ClockSample;
use worktrace_lib::services::timer::coordinator::{ResumeRequest, SessionRequest};
fn session_request(h: &H) -> SessionRequest {
    let l = h.coord.live().unwrap();
    SessionRequest {
        expected_data_epoch: h.epoch.clone(),
        session_id: l.id.clone(),
        session_expected_version: l.row_version,
    }
}
#[test]
fn heartbeat_detects_before_cadence_and_never_persists_a_bad_sample() {
    for elapsed in [1_000, 30_000] {
        let mut h = setup();
        h.start();
        let id = h
            .coord
            .live()
            .unwrap()
            .open_interval
            .as_ref()
            .unwrap()
            .0
            .clone();
        h.advance_mono_only(elapsed);
        h.advance_wall_only(elapsed + 5_000);
        assert_eq!(
            h.coord.heartbeat(&mut h.db).unwrap_err().code(),
            "RECOVERY_REQUIRED"
        );
        assert_eq!(h.checkpoints(&id).unwrap().elapsed_ms, 0);
        assert_eq!(h.coord.live().unwrap().state, SessionState::Recovering);
    }
}
#[test]
fn failed_sample_has_unknown_endpoint_and_never_resumes_accrual() {
    let mut h = setup();
    h.start();
    h.advance(30_000);
    h.coord.heartbeat(&mut h.db).unwrap();
    h.advance(1_000);
    h.clock.lock().unwrap().fail_once();
    assert!(h.coord.snapshot(&mut h.db).is_err());
    h.advance(1_000);
    let s = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(s.state, Some(SessionState::Recovering));
    assert_eq!(s.active_ms, 30_000);
    assert_eq!(s.pending_ms, None);
    let ivs = worktrace_lib::storage::session_repo::intervals_of_session(
        h.db.connection(),
        s.session_id.as_ref().unwrap(),
    )
    .unwrap();
    let pending = ivs.iter().find(|i| i.needs_review).unwrap();
    assert_eq!(pending.ended_at, None);
    assert_eq!(pending.sampled_end_wall_at, None);
}
#[test]
fn trusted_system_boundary_excludes_sleep_and_late_boundary_requires_review() {
    let mut h = setup();
    h.start();
    h.advance(139_000);
    let s = h
        .coord
        .system_pause(
            &mut h.db,
            Some(ClockSample {
                wall_ms: 1_700_000_010_000,
                monotonic_ms: 10_000,
            }),
        )
        .unwrap();
    assert_eq!(s.state, Some(SessionState::Paused));
    assert_eq!(s.active_ms, 10_000);
    h.advance(30_000);
    assert_eq!(h.coord.snapshot(&mut h.db).unwrap().active_ms, 10_000);
    assert_eq!(
        worktrace_lib::storage::session_repo::get_session(
            h.db.connection(),
            s.session_id.as_ref().unwrap()
        )
        .unwrap()
        .unwrap()
        .ended_at,
        None
    );
    let mut h = setup();
    h.start();
    h.advance(129_000);
    let s = h.coord.system_pause(&mut h.db, None).unwrap();
    assert_eq!(s.state, Some(SessionState::Recovering));
    assert_eq!(s.active_ms, 0);
}
#[test]
fn mismatched_resume_is_rejected_before_sampling_or_writes() {
    let mut h = setup();
    h.start();
    let req = session_request(&h);
    h.coord.pause(&mut h.db, req).unwrap();
    h.db.connection().execute("INSERT INTO task(id,title,status,row_version,created_at,updated_at) VALUES('t2','other','Ready',0,0,0)",[]).unwrap();
    h.db.connection()
        .execute("UPDATE task SET status='Waiting' WHERE id='t1'", [])
        .unwrap();
    let rev = h.revision();
    let l = h.coord.live().unwrap();
    let req = ResumeRequest {
        expected_data_epoch: h.epoch.clone(),
        task_id: "t2".into(),
        task_expected_version: 0,
        session_id: l.id.clone(),
        session_expected_version: l.row_version,
    };
    h.clock.lock().unwrap().fail_once();
    assert!(h.coord.resume(&mut h.db, req).is_err());
    assert_eq!(h.revision(), rev);
    assert_eq!(h.coord.live().unwrap().state, SessionState::Paused);
}
#[test]
fn finished_session_is_immutable() {
    let mut h = setup();
    h.start();
    let req = session_request(&h);
    h.coord.finish(&mut h.db, req).unwrap();
    let rev = h.revision();
    let req = session_request(&h);
    assert!(h.coord.finish(&mut h.db, req).is_err());
    assert_eq!(h.revision(), rev);
}
#[test]
fn archive_blocks_start_and_resume_without_partial_facts() {
    let mut h = setup();
    h.db.connection().execute("INSERT INTO project(id,name,status,row_version,created_at,updated_at) VALUES('p','p','active',0,0,0)",[]).unwrap();
    h.db.connection()
        .execute("UPDATE task SET project_id='p' WHERE id='t1'", [])
        .unwrap();
    h.start();
    let req = session_request(&h);
    h.coord.pause(&mut h.db, req).unwrap();
    h.db.connection()
        .execute("UPDATE project SET status='archived' WHERE id='p'", [])
        .unwrap();
    let rev = h.revision();
    let row = worktrace_lib::storage::task_repo::get_task(h.db.connection(), "t1")
        .unwrap()
        .unwrap();
    let l = h.coord.live().unwrap();
    let req = ResumeRequest {
        expected_data_epoch: h.epoch.clone(),
        task_id: "t1".into(),
        task_expected_version: row.row_version,
        session_id: l.id.clone(),
        session_expected_version: l.row_version,
    };
    assert!(h.coord.resume(&mut h.db, req).is_err());
    assert_eq!(h.revision(), rev);
    let req = session_request(&h);
    h.coord.finish(&mut h.db, req).unwrap();
    let rev = h.revision();
    let req = StartRequest {
        expected_data_epoch: h.epoch.clone(),
        task_id: "t1".into(),
        task_expected_version: row.row_version,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: 30_000,
    };
    assert!(h.coord.start(&mut h.db, req).is_err());
    assert_eq!(h.revision(), rev);
}
#[test]
fn anomaly_audit_is_json_and_preserves_raw_wall() {
    let mut h = setup();
    h.start();
    h.advance(30_000);
    h.coord.heartbeat(&mut h.db).unwrap();
    h.advance(1_000);
    h.advance_wall_only(5_000);
    h.coord.snapshot(&mut h.db).unwrap();
    let (before, after): (String, String) =
        h.db.connection()
            .query_row("SELECT before_json,after_json FROM time_edit", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
    let _: serde_json::Value = serde_json::from_str(&before).unwrap();
    let a: serde_json::Value = serde_json::from_str(&after).unwrap();
    assert_eq!(a["sampled_wall_at"], 1_700_000_036_000i64);
    assert_eq!(a["candidate_end"], 1_700_000_031_000i64);
    let ivs = worktrace_lib::storage::session_repo::intervals_of_session(
        h.db.connection(),
        &h.coord.live().unwrap().id,
    )
    .unwrap();
    assert_eq!(
        ivs.iter()
            .find(|i| i.needs_review)
            .unwrap()
            .sampled_end_wall_at,
        Some(1_700_000_036_000)
    );
}
#[test]
fn drift_above_allowance_cannot_be_hidden_by_heartbeats() {
    let mut h = setup();
    h.start();
    let mut recovered = false;
    for _ in 0..10 {
        h.advance(30_000);
        h.advance_wall_only(500);
        if h.coord.heartbeat(&mut h.db).is_err() {
            recovered = true;
            break;
        }
    }
    assert!(recovered);
    assert_eq!(h.coord.live().unwrap().state, SessionState::Recovering);
}
#[test]
fn four_hours_of_measured_natural_drift_remains_trusted() {
    let mut h = setup();
    h.start();
    for _ in 0..480 {
        h.advance(30_000);
        h.advance_wall_only(7);
        assert!(h.coord.heartbeat(&mut h.db).unwrap());
    }
    let s = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(s.state, Some(SessionState::Running));
    assert_eq!(s.active_ms, 14_400_000);
}
#[test]
fn failed_anomaly_locks_all_entrypoints_until_recovery_commits() {
    let mut h = setup();
    h.start();
    h.db.connection().execute_batch("CREATE TRIGGER fail_audit BEFORE INSERT ON time_edit BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    h.advance(1_000);
    h.advance_wall_only(5_000);
    assert!(h.coord.snapshot(&mut h.db).is_err());
    assert!(h.coord.is_faulted());
    h.db.connection()
        .execute_batch("DROP TRIGGER fail_audit;")
        .unwrap();
    assert!(h.coord.snapshot(&mut h.db).is_err());
    assert!(h.coord.tick(&mut h.db).is_err());
    assert!(h.coord.heartbeat(&mut h.db).is_err());
    assert!(h.coord.stats_sample(&mut h.db).is_err());
    let s = h.coord.retry_recovery(&mut h.db).unwrap();
    assert_eq!(s.state, Some(SessionState::Recovering));
    assert!(!h.coord.is_faulted());
}

#[test]
fn finishing_another_paused_session_preserves_active_timer() {
    let mut h = setup();
    h.start();
    let old = session_request(&h);
    let old_id = old.session_id.clone();
    h.coord.pause(&mut h.db, old).unwrap();
    let old_v = worktrace_lib::storage::session_repo::get_session(h.db.connection(), &old_id)
        .unwrap()
        .unwrap()
        .row_version;
    h.db.connection().execute("INSERT INTO task(id,title,status,row_version,created_at,updated_at) VALUES('t2','other','Ready',0,0,0)",[]).unwrap();
    let out = h
        .coord
        .start(
            &mut h.db,
            StartRequest {
                expected_data_epoch: h.epoch.clone(),
                task_id: "t2".into(),
                task_expected_version: 0,
                mode: SessionMode::Foreground,
                timer_kind: TimerKind::Stopwatch,
                target_duration_ms: None,
                expected_interval_ms: 30_000,
            },
        )
        .unwrap();
    let active = out.snapshot.session_id;
    h.advance(1_000);
    h.coord
        .finish(
            &mut h.db,
            SessionRequest {
                expected_data_epoch: h.epoch.clone(),
                session_id: old_id,
                session_expected_version: old_v,
            },
        )
        .unwrap();
    let snap = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(snap.session_id, active);
    assert_eq!(snap.active_ms, 1_000);
    assert_eq!(snap.state, Some(SessionState::Running));
}
#[test]
fn rebased_clock_cannot_start_inside_confirmed_history() {
    let mut h = setup();
    h.start();
    h.advance(10_000);
    let req = session_request(&h);
    h.coord.finish(&mut h.db, req).unwrap();
    let row = worktrace_lib::storage::task_repo::get_task(h.db.connection(), "t1")
        .unwrap()
        .unwrap();
    h.coord.reestablish_anchor(ClockSample {
        wall_ms: 1_700_000_005_000,
        monotonic_ms: 10_000,
    });
    // 同步并显式接受校正，检测通过但新归属仍须校验历史冲突。
    h.advance_wall_only(-5_000);
    h.coord
        .accept_clock_correction(h.clock.lock().unwrap().sample().unwrap());
    let rev = h.revision();
    let req = StartRequest {
        expected_data_epoch: h.epoch.clone(),
        task_id: "t1".into(),
        task_expected_version: row.row_version,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: 30_000,
    };
    assert!(h.coord.start(&mut h.db, req).is_err());
    assert_eq!(h.revision(), rev);
}

#[test]
fn transaction_primitive_rejects_invalid_targets_without_changes() {
    use worktrace_lib::services::timer::primitives::{end_session_in_tx, EndSessionFacts};
    let mut h = setup();
    h.start();
    let sid = h.coord.live().unwrap().id.clone();
    let v = h.coord.live().unwrap().row_version;
    for target in [
        SessionState::Running,
        SessionState::Recovering,
        SessionState::Discarded,
    ] {
        let tx = h.db.connection_mut().unchecked_transaction().unwrap();
        assert!(end_session_in_tx(
            &tx,
            &EndSessionFacts {
                session_id: sid.clone(),
                expected_row_version: v,
                attributed_end: 1_700_000_000_000,
                sampled_end_wall_at: 1_700_000_000_000,
                target_state: target
            }
        )
        .is_err());
        tx.commit().unwrap();
        assert_eq!(h.coord.live().unwrap().state, SessionState::Running);
        assert_eq!(
            worktrace_lib::storage::session_repo::intervals_of_session(h.db.connection(), &sid)
                .unwrap()[0]
                .ended_at,
            None
        );
    }
}
#[test]
fn v01_rejects_nonforeground_modes() {
    let mut h = setup();
    let rev = h.revision();
    for mode in [
        SessionMode::Background,
        SessionMode::Passive,
        SessionMode::Waiting,
    ] {
        let req = StartRequest {
            expected_data_epoch: h.epoch.clone(),
            task_id: "t1".into(),
            task_expected_version: 0,
            mode,
            timer_kind: TimerKind::Stopwatch,
            target_duration_ms: None,
            expected_interval_ms: 30_000,
        };
        assert!(h.coord.start(&mut h.db, req).is_err());
        assert_eq!(h.revision(), rev);
    }
    let n: i64 =
        h.db.connection()
            .query_row("SELECT COUNT(*) FROM work_session", [], |r| r.get(0))
            .unwrap();
    assert_eq!(n, 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// 长期漂移界不得被无关的系统事件勾销
// ─────────────────────────────────────────────────────────────────────────────

use worktrace_lib::platform::clock::Clock;
use worktrace_lib::services::timer::anchor::SampleVerdict;

/// **系统事件不是时钟校正**：唤醒后长期界必须还在。
///
/// 改之前 `system_pause` 在「当前没有 running 会话」的分支里也调了
/// `reestablish_anchor`，而它会把归属基线、短期参照、长期参照一起重置。
/// 于是一台空闲时反复收到休眠/唤醒事件的机器，**每次事件都把长期偏差一笔勾销**，
/// 长期界永远不会触发，等于没有。
///
/// 构造上要让**只有长期界可能触发**：
/// - 每拍墙钟比单调钟多走 100ms，单拍增量差 100ms < 2000ms；
/// - 每拍都调 `reanchor_drift_on_heartbeat`，短期累计偏差始终为 100ms；
/// - 于是唯一判据就是「相对长期参照的偏差 vs 2000ms + elapsed×500ppm」。
#[test]
fn system_events_do_not_forgive_the_lifetime_drift_bound() {
    let mut h = setup();
    h.coord
        .establish_anchor(h.clock.lock().unwrap().sample().unwrap());

    // 15 拍：偏差 1500ms，长期界 2000 + 15000×500/1e6 = 2007ms → 尚未越界
    for _ in 0..15 {
        h.advance_mono_only(1_000);
        h.advance_wall_only(1_100);
        let s = h.clock.lock().unwrap().sample().unwrap();
        let _ = h.coord.snapshot(&mut h.db).unwrap();
        h.coord.reanchor_drift_on_heartbeat(s);
    }
    assert!(
        !h.coord.last_verdict().needs_recovery(),
        "15 拍时偏差 1500ms 还在界内：{:?}",
        h.coord.last_verdict()
    );

    // 一次「没有会话在跑」的系统事件（比如空闲时系统自己睡了一下）
    h.coord.system_pause(&mut h.db, None).unwrap();

    // 再走 15 拍：累计偏差 3000ms，长期界 2000 + 30000×500/1e6 = 2015ms → 越界
    for _ in 0..15 {
        h.advance_mono_only(1_000);
        h.advance_wall_only(1_100);
        let s = h.clock.lock().unwrap().sample().unwrap();
        let _ = h.coord.snapshot(&mut h.db).unwrap();
        h.coord.reanchor_drift_on_heartbeat(s);
    }

    match h.coord.last_verdict() {
        SampleVerdict::Drifted { cumulative_gap_ms } => {
            assert_eq!(
                cumulative_gap_ms, 3_000,
                "偏差要从**进程开始**算，不能被中间那次系统事件勾销"
            );
        }
        other => panic!("系统事件不该勾销长期界，实际判定 {other:?}"),
    }
}

#[test]
fn paused_clock_correction_is_audited_once_and_resume_stays_running() {
    let mut h = setup();
    h.start();
    h.advance(10_000);
    let req = session_request(&h);
    h.coord.pause(&mut h.db, req).unwrap();
    let rev = h.revision();
    let before_version = h.coord.live().unwrap().row_version;
    h.advance_wall_only(5_000);
    let corrected = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(corrected.state, Some(SessionState::Paused));
    assert_eq!(corrected.active_ms, 10_000);
    assert_eq!(h.revision(), rev + 1);
    assert_eq!(corrected.session_version, Some(before_version + 1));
    let audit: String =
        h.db.connection()
            .query_row("SELECT after_json FROM time_edit", [], |r| r.get(0))
            .unwrap();
    let audit: serde_json::Value = serde_json::from_str(&audit).unwrap();
    assert_eq!(audit["clock_correction_accepted"], true);
    assert_eq!(audit["intervals_changed"], false);
    h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(h.revision(), rev + 1);
    let task = worktrace_lib::storage::task_repo::get_task(h.db.connection(), "t1")
        .unwrap()
        .unwrap();
    let req = ResumeRequest {
        expected_data_epoch: h.epoch.clone(),
        task_id: "t1".into(),
        task_expected_version: task.row_version,
        session_id: corrected.session_id.unwrap(),
        session_expected_version: corrected.session_version.unwrap(),
    };
    assert_eq!(
        h.coord.resume(&mut h.db, req).unwrap().snapshot.state,
        Some(SessionState::Running)
    );
    h.advance(1_000);
    let snap = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(snap.state, Some(SessionState::Running));
    assert_eq!(snap.active_ms, 11_000);
}
#[test]
fn long_gap_is_not_clock_correction_and_lifetime_drift_remains_visible() {
    let mut h = setup();
    h.start();
    for _ in 0..15 {
        h.advance(1_000);
        h.advance_wall_only(100);
        h.coord.snapshot(&mut h.db).unwrap();
        let sample = h.clock.lock().unwrap().sample().unwrap();
        h.coord.reanchor_drift_on_heartbeat(sample);
    }
    h.advance(100_000);
    assert_eq!(
        h.coord.snapshot(&mut h.db).unwrap().state,
        Some(SessionState::Recovering)
    );
    let audit: String =
        h.db.connection()
            .query_row("SELECT after_json FROM time_edit", [], |r| r.get(0))
            .unwrap();
    let audit: serde_json::Value = serde_json::from_str(&audit).unwrap();
    assert_eq!(audit["clock_correction_accepted"], false);
    h.db.connection().execute("INSERT INTO task(id,title,status,row_version,created_at,updated_at) VALUES('t2','other','Ready',0,0,0)", []).unwrap();
    h.coord
        .start(
            &mut h.db,
            StartRequest {
                expected_data_epoch: h.epoch.clone(),
                task_id: "t2".into(),
                task_expected_version: 0,
                mode: SessionMode::Foreground,
                timer_kind: TimerKind::Stopwatch,
                target_duration_ms: None,
                expected_interval_ms: 30_000,
            },
        )
        .unwrap();
    let mut caught = false;
    for _ in 0..15 {
        h.advance(1_000);
        h.advance_wall_only(100);
        let snap = h.coord.snapshot(&mut h.db).unwrap();
        if snap.state == Some(SessionState::Recovering) {
            assert!(matches!(
                h.coord.last_verdict(),
                SampleVerdict::Drifted { .. }
            ));
            caught = true;
            break;
        }
        let sample = h.clock.lock().unwrap().sample().unwrap();
        h.coord.reanchor_drift_on_heartbeat(sample);
    }
    assert!(caught, "长间隔不能使进程累计偏差逃过长期检测");
}
#[test]
fn failed_paused_correction_does_not_move_references_or_versions() {
    let mut h = setup();
    h.start();
    let req = session_request(&h);
    h.coord.pause(&mut h.db, req).unwrap();
    let rev = h.revision();
    let version = h.coord.live().unwrap().row_version;
    h.db.connection().execute_batch("CREATE TRIGGER fail_clock_audit BEFORE INSERT ON time_edit BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    h.advance_wall_only(5_000);
    assert!(h.coord.snapshot(&mut h.db).is_err());
    assert_eq!(h.revision(), rev);
    assert_eq!(h.coord.live().unwrap().row_version, version);
    assert!(h.coord.is_faulted());
    h.db.connection()
        .execute_batch("DROP TRIGGER fail_clock_audit;")
        .unwrap();
    let corrected = h.coord.retry_recovery(&mut h.db).unwrap();
    assert_eq!(corrected.state, Some(SessionState::Paused));
    assert_eq!(h.revision(), rev + 1);
    assert!(!h.coord.is_faulted());
    h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(h.revision(), rev + 1);
}
#[test]
fn monotonic_failure_is_not_accepted_as_wall_clock_correction() {
    let mut h = setup();
    h.start();
    h.advance(1_000);
    h.coord.snapshot(&mut h.db).unwrap();
    h.advance_mono_only(-100);
    let snap = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(snap.state, Some(SessionState::Recovering));
    assert!(h.coord.is_faulted());
    assert!(h.coord.retry_recovery(&mut h.db).is_err());
    let audit: String =
        h.db.connection()
            .query_row("SELECT after_json FROM time_edit", [], |r| r.get(0))
            .unwrap();
    let audit: serde_json::Value = serde_json::from_str(&audit).unwrap();
    assert_eq!(audit["clock_correction_accepted"], false);
}

#[test]
fn unresolved_clock_change_while_recovering_cannot_start_new_work() {
    let mut h = setup();
    h.start();
    h.advance(129_000);
    assert_eq!(
        h.coord.snapshot(&mut h.db).unwrap().state,
        Some(SessionState::Recovering)
    );
    h.advance_wall_only(5_000);
    h.coord.snapshot(&mut h.db).unwrap();
    h.db.connection().execute("INSERT INTO task(id,title,status,row_version,created_at,updated_at) VALUES('t2','other','Ready',0,0,0)", []).unwrap();
    let rev = h.revision();
    let req = StartRequest {
        expected_data_epoch: h.epoch.clone(),
        task_id: "t2".into(),
        task_expected_version: 0,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: 30_000,
    };
    assert_eq!(
        h.coord.start(&mut h.db, req).unwrap_err().code(),
        "RECOVERY_REQUIRED"
    );
    assert_eq!(h.revision(), rev);
    let n: i64 =
        h.db.connection()
            .query_row("SELECT COUNT(*) FROM work_session", [], |r| r.get(0))
            .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn paused_monotonic_failure_cannot_commit_resume() {
    let mut h = setup();
    h.start();
    h.advance(1_000);
    let req = session_request(&h);
    h.coord.pause(&mut h.db, req).unwrap();
    let live = h.coord.live().unwrap().clone();
    let task = worktrace_lib::storage::task_repo::get_task(h.db.connection(), "t1")
        .unwrap()
        .unwrap();
    let rev = h.revision();
    h.advance_mono_only(-100);
    let result = h.coord.resume(
        &mut h.db,
        ResumeRequest {
            expected_data_epoch: h.epoch.clone(),
            task_id: "t1".into(),
            task_expected_version: task.row_version,
            session_id: live.id.clone(),
            session_expected_version: live.row_version,
        },
    );
    assert_eq!(result.unwrap_err().code(), "RECOVERY_REQUIRED");
    let persisted = worktrace_lib::storage::session_repo::get_session(h.db.connection(), &live.id)
        .unwrap()
        .unwrap();
    assert_eq!(persisted.state, SessionState::Paused);
    assert_eq!(persisted.row_version, live.row_version);
    assert_eq!(h.revision(), rev);
    assert!(h.coord.is_faulted());
    assert!(h.coord.retry_recovery(&mut h.db).is_err());
    assert_eq!(
        h.coord.snapshot(&mut h.db).unwrap_err().code(),
        "RECOVERY_REQUIRED"
    );
}

fn nonrunning_monotonic_fixture(state: Option<SessionState>) -> H {
    let mut h = setup();
    if let Some(state) = state {
        h.start();
        h.advance(1_000);
        let req = session_request(&h);
        if state == SessionState::Paused {
            h.coord.pause(&mut h.db, req).unwrap();
        } else {
            h.coord.finish(&mut h.db, req).unwrap();
        }
    } else {
        let sample = h.clock.lock().unwrap().sample().unwrap();
        h.coord.establish_anchor(sample);
        h.advance(1_000);
        h.coord.snapshot(&mut h.db).unwrap();
    }
    h.advance_mono_only(-100);
    h
}

#[test]
fn nonrunning_monotonic_failure_cannot_start_new_work() {
    for state in [
        None,
        Some(SessionState::Paused),
        Some(SessionState::Finished),
    ] {
        let mut h = nonrunning_monotonic_fixture(state);
        h.db.connection().execute("INSERT INTO task(id,title,status,row_version,created_at,updated_at) VALUES('t2','other','Ready',0,0,0)", []).unwrap();
        let rev = h.revision();
        let result = h.coord.start(
            &mut h.db,
            StartRequest {
                expected_data_epoch: h.epoch.clone(),
                task_id: "t2".into(),
                task_expected_version: 0,
                mode: SessionMode::Foreground,
                timer_kind: TimerKind::Stopwatch,
                target_duration_ms: None,
                expected_interval_ms: 30_000,
            },
        );
        assert_eq!(
            result.unwrap_err().code(),
            "RECOVERY_REQUIRED",
            "state={state:?}"
        );
        assert!(h.coord.is_faulted());
        assert_eq!(h.revision(), rev);
        assert!(
            worktrace_lib::storage::session_repo::running_foreground(h.db.connection())
                .unwrap()
                .is_none()
        );
        let count: i64 =
            h.db.connection()
                .query_row(
                    "SELECT COUNT(*) FROM work_session WHERE task_id='t2'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(count, 0);
        let task = worktrace_lib::storage::task_repo::get_task(h.db.connection(), "t2")
            .unwrap()
            .unwrap();
        assert_eq!(task.row_version, 0);
        assert!(h.coord.retry_recovery(&mut h.db).is_err());
    }
}

#[test]
fn nonrunning_monotonic_failure_cannot_return_statistics() {
    for state in [
        None,
        Some(SessionState::Paused),
        Some(SessionState::Finished),
    ] {
        let mut h = nonrunning_monotonic_fixture(state);
        let rev = h.revision();
        assert_eq!(
            h.coord.stats_sample(&mut h.db).unwrap_err().code(),
            "RECOVERY_REQUIRED",
            "state={state:?}"
        );
        assert!(h.coord.is_faulted());
        assert_eq!(h.revision(), rev);
        assert!(h.coord.retry_recovery(&mut h.db).is_err());
    }
}

#[test]
fn system_pause_does_not_mask_monotonic_failure() {
    for with_boundary in [false, true] {
        let mut h = setup();
        h.start();
        h.advance(1_000);
        h.coord.snapshot(&mut h.db).unwrap();
        let boundary = h.clock.lock().unwrap().sample().unwrap();
        h.advance_mono_only(-100);
        let snap = h
            .coord
            .system_pause(&mut h.db, with_boundary.then_some(boundary))
            .unwrap();
        assert_eq!(snap.state, Some(SessionState::Recovering));
        assert!(matches!(
            h.coord.last_verdict(),
            SampleVerdict::MonotonicBackwards { .. }
        ));
        assert!(h.coord.is_faulted());
        assert!(h.coord.retry_recovery(&mut h.db).is_err());
    }
}

#[test]
fn trusted_departure_boundary_does_not_hide_wall_clock_jump() {
    let mut h = setup();
    h.start();
    h.advance(1_000);
    let boundary = h.clock.lock().unwrap().sample().unwrap();
    h.advance(1_000);
    h.advance_wall_only(5_000);
    let snap = h.coord.system_pause(&mut h.db, Some(boundary)).unwrap();
    assert_eq!(snap.state, Some(SessionState::Recovering));
    assert!(matches!(
        h.coord.last_verdict(),
        SampleVerdict::Jumped { .. }
    ));
    let audit: String =
        h.db.connection()
            .query_row("SELECT after_json FROM time_edit", [], |r| r.get(0))
            .unwrap();
    let audit: serde_json::Value = serde_json::from_str(&audit).unwrap();
    assert_eq!(audit["clock_correction_accepted"], true);
}

#[test]
fn unaccepted_clock_correction_cannot_expire_with_lifetime_allowance() {
    let mut h = setup();
    h.start();
    h.advance(129_000);
    h.coord.snapshot(&mut h.db).unwrap();
    h.advance_wall_only(5_000);
    h.coord.snapshot(&mut h.db).unwrap();
    let rev = h.revision();
    for _ in 0..210 {
        h.advance(30_000);
        h.coord.snapshot(&mut h.db).unwrap();
    }
    assert_eq!(h.coord.last_verdict(), SampleVerdict::Trusted);
    h.db.connection().execute("INSERT INTO task(id,title,status,row_version,created_at,updated_at) VALUES('t2','other','Ready',0,0,0)", []).unwrap();
    let result = h.coord.start(
        &mut h.db,
        StartRequest {
            expected_data_epoch: h.epoch.clone(),
            task_id: "t2".into(),
            task_expected_version: 0,
            mode: SessionMode::Foreground,
            timer_kind: TimerKind::Stopwatch,
            target_duration_ms: None,
            expected_interval_ms: 30_000,
        },
    );
    assert_eq!(result.unwrap_err().code(), "RECOVERY_REQUIRED");
    assert_eq!(
        h.coord.stats_sample(&mut h.db).unwrap_err().code(),
        "RECOVERY_REQUIRED"
    );
    assert_eq!(h.revision(), rev);
    assert!(
        worktrace_lib::storage::session_repo::running_foreground(h.db.connection())
            .unwrap()
            .is_none()
    );
}

/// **本 run 尚无会话**时的墙钟跳变：没有工时事实被牵连，也没有可挂校正审计的会话，
/// 所以下一次 `start` 必须用当前样本**整体重定基线**，而不是拿旧基线算 `started_at`。
///
/// 改之前：`try_handle_anomaly` 在 `live == None` 时只处理单调钟硬故障，其余直接返回
/// ——不写审计、不置未接受标记、也不重定基线；而 `start` 里的判据是
/// `anchor_state.is_none()`，run 初始化已建立基线故为假，于是放行且归属整整偏出
/// 跳变量（NTP 步进时是几分钟/几小时，随后按日期分桶的统计会跟着错）。
#[test]
fn wall_jump_before_first_session_rebases_instead_of_shifting_attribution() {
    let mut h = setup();

    // run 初始化：建立基线（08 §1 对 lifetime_ref 的要求），此刻还没有任何会话。
    let s = h.clock.lock().unwrap().sample().unwrap();
    h.coord.establish_anchor(s);
    h.advance(1_000);
    h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(h.coord.last_verdict(), SampleVerdict::Trusted);

    // 无会话期间墙钟向前跳 5 秒：检测得到，但没有可写的审计、也无处可挂。
    h.advance_wall_only(5_000);
    let now_wall = h.clock.lock().unwrap().sample().unwrap().wall_ms;
    h.coord.snapshot(&mut h.db).unwrap();
    assert!(matches!(
        h.coord.last_verdict(),
        SampleVerdict::Jumped { .. }
    ));

    // 跳变之后的第一段会话：归属必须落在**当前**墙钟上。
    let out = h
        .coord
        .start(
            &mut h.db,
            StartRequest {
                expected_data_epoch: h.epoch.clone(),
                task_id: "t1".into(),
                task_expected_version: 0,
                mode: SessionMode::Foreground,
                timer_kind: TimerKind::Stopwatch,
                target_duration_ms: None,
                expected_interval_ms: 30_000,
            },
        )
        .unwrap();
    let sid = out.snapshot.session_id.unwrap();
    let ivs = worktrace_lib::storage::session_repo::intervals_of_session(h.db.connection(), &sid)
        .unwrap();
    assert_eq!(
        ivs[0].started_at, now_wall,
        "无会话时的墙钟跳变必须在 start 时重定基线，不能拿旧基线算归属"
    );

    // 重定之后这一段是干净的：不再被长期界追着判异常。
    h.advance(1_000);
    let snap = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(snap.state, Some(SessionState::Running));
    assert_eq!(snap.active_ms, 1_000);
}

#[test]
fn loaded_paused_session_without_anchor_can_resume_in_new_run() {
    let mut h = setup();
    h.start();
    h.advance(1_000);
    let req = session_request(&h);
    h.coord.pause(&mut h.db, req).unwrap();
    let sid = h.coord.live().unwrap().id.clone();
    h.coord = Coordinator::new(Box::new(Arc::clone(&h.clock)), "run-2");
    h.db.connection()
        .execute(
            "INSERT INTO application_run(id,started_at) VALUES('run-2',0)",
            [],
        )
        .unwrap();
    h.coord.load_session(h.db.connection(), &sid).unwrap();
    let task = worktrace_lib::storage::task_repo::get_task(h.db.connection(), "t1")
        .unwrap()
        .unwrap();
    let req = ResumeRequest {
        expected_data_epoch: h.epoch.clone(),
        task_id: "t1".into(),
        task_expected_version: task.row_version,
        session_id: sid.clone(),
        session_expected_version: h.coord.live().unwrap().row_version,
    };
    let expected = h.clock.lock().unwrap().sample().unwrap().wall_ms;
    let result = h.coord.resume(&mut h.db, req).unwrap();
    assert_eq!(result.snapshot.state, Some(SessionState::Running));
    let intervals =
        worktrace_lib::storage::session_repo::intervals_of_session(h.db.connection(), &sid)
            .unwrap();
    assert_eq!(
        intervals
            .iter()
            .find(|i| i.ended_at.is_none())
            .unwrap()
            .started_at,
        expected
    );
    h.advance(1_000);
    assert_eq!(h.coord.snapshot(&mut h.db).unwrap().active_ms, 2_000);
}

#[test]
fn trusted_boundary_after_four_hours_natural_drift_pauses() {
    for drift in [-7, 7] {
        let mut h = setup();
        h.start();
        for _ in 0..480 {
            h.advance(30_000);
            h.advance_wall_only(drift);
            h.coord.heartbeat(&mut h.db).unwrap();
        }
        h.advance(1_000);
        let boundary = h.clock.lock().unwrap().sample().unwrap();
        h.advance(120_000);
        let snap = h.coord.system_pause(&mut h.db, Some(boundary)).unwrap();
        assert_eq!(snap.state, Some(SessionState::Paused), "drift={drift}");
        assert_eq!(snap.active_ms, 14_401_000);
        assert!(!snap.needs_attention());
    }
}

#[test]
fn transient_boundary_wall_jump_is_not_accepted_when_current_sample_is_normal() {
    let mut h = setup();
    h.start();
    h.advance(1_000);
    let mut boundary = h.clock.lock().unwrap().sample().unwrap();
    boundary.wall_ms += 5_000;
    h.advance(10_000);
    let snap = h.coord.system_pause(&mut h.db, Some(boundary)).unwrap();
    assert_eq!(snap.state, Some(SessionState::Recovering));
}

#[test]
fn statistics_without_anchor_use_sampled_wall_time_like_snapshot() {
    let mut h = setup();
    h.advance(1_000);
    let revision = h.revision();
    let expected = h.clock.lock().unwrap().sample().unwrap().wall_ms;
    let stats = h.coord.stats_sample(&mut h.db).unwrap();
    let snap = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(stats.attributed_end, expected);
    assert_eq!(stats.attributed_end, snap.as_of);
    assert_eq!(stats.session_id, None);
    assert_eq!(stats.total_ms(), 0);
    assert_eq!(h.revision(), revision);
}
