//! P2 Task 2 的集成测试：统一采样检测与持续归属。
//!
//! 计划要求覆盖：500ms 回拨下暂停/继续不重叠、缓慢累计漂移、1999/2000/2001ms 边界、
//! 30 秒前异常、单调回拨、采样失败、可信/延迟系统事件。
//!
//! 纯判定的边界（阈值、漂移、挂起）已在 `anchor.rs` 的单元测试里穷举；这里测的是
//! **检测接进协调器之后**的行为：命令/查询/系统事件是否走同一条路径、心跳前移参照点
//! 是否真的生效、采样失败与延迟事件会不会被误当成可信。

use std::sync::{Arc, Mutex};

use worktrace_lib::domain::session::TimerKind;
use worktrace_lib::platform::clock::{Clock, ClockSample, FakeClock};
use worktrace_lib::services::timer::anchor::SampleVerdict;
use worktrace_lib::services::timer::coordinator::{Coordinator, HEARTBEAT_INTERVAL_MS};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;

struct H {
    _dir: tempfile::TempDir,
    db: Db,
    clock: Arc<Mutex<FakeClock>>,
    coord: Coordinator,
}

fn setup() -> H {
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
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Doing',0,0,0)",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
         VALUES('s1','t1','run-1','FOREGROUND','running','stopwatch',0,0)",
        [],
    )
    .unwrap();
    // 基线的挂钟值用真实量级，避免与小数值混淆
    tx.execute(
        "INSERT INTO work_interval(id,session_id,started_at) VALUES('i1','s1',1700000000000)",
        [],
    )
    .unwrap();
    tx.commit().unwrap();

    let clock = Arc::new(Mutex::new(FakeClock::new(1_700_000_000_000, 0)));
    let mut coord = Coordinator::new(Box::new(Arc::clone(&clock)), "run-1");
    let sample = clock.lock().unwrap().sample().unwrap();
    coord.establish_anchor(sample);
    coord.load_session(db.connection(), "s1").unwrap();

    H {
        _dir: dir,
        db,
        clock,
        coord,
    }
}

impl H {
    fn advance_both(&self, ms: i64) {
        self.clock.lock().unwrap().advance_both(ms);
    }
    fn advance_wall_only(&self, ms: i64) {
        self.clock.lock().unwrap().advance_wall(ms);
    }
    fn advance_mono_only(&self, ms: i64) {
        self.clock.lock().unwrap().advance_monotonic(ms);
    }
    fn sample(&self) -> ClockSample {
        self.clock.lock().unwrap().sample().unwrap()
    }
    fn snapshot(&mut self) -> worktrace_lib::services::timer::snapshot::TimerSnapshot {
        self.coord.snapshot(&mut self.db).unwrap()
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// 检测对**每一拍**都生效，与 30 秒的检查点节奏无关。
///
/// 计划原文：「30 秒只控制检查点频率，不能用于提前跳过异常检测」。
#[test]
fn an_anomaly_is_detected_well_before_the_30_second_checkpoint() {
    let mut h = setup();
    h.advance_both(1_000);
    assert_eq!(h.snapshot().as_of, 1_700_000_001_000);
    assert_eq!(h.coord.last_verdict(), SampleVerdict::Trusted);

    // 才第 2 拍（远早于 30 秒）挂钟就跳了 5 秒
    h.advance_wall_only(5_000);
    h.advance_mono_only(1_000);
    let _ = h.snapshot();

    match h.coord.last_verdict() {
        SampleVerdict::Jumped { delta_gap_ms } => {
            // 挂钟走了 5000、单调钟走了 1000 → 差 4000
            assert_eq!(delta_gap_ms, 4_000, "墙钟比单调钟多走 4 秒");
        }
        other => panic!("第 2 拍就该判出来，实际 {other:?}"),
    }
    assert!(h.coord.last_verdict().needs_recovery());
}

/// 阈值边界：1999 / 2000 可信，2001 越界。**恰好 2000ms 不算**。
#[test]
fn the_boundary_at_2000ms_is_exact() {
    for (gap, trusted) in [(1999_i64, true), (2000, true), (2001, false)] {
        let mut h = setup();
        h.advance_mono_only(1_000);
        h.advance_wall_only(1_000 + gap);
        let _ = h.snapshot();
        assert_eq!(
            h.coord.last_verdict().facts_are_trustworthy(),
            trusted,
            "gap={gap} 的判定不对：{:?}",
            h.coord.last_verdict()
        );
    }
}

/// 挂钟回拨 500ms：事实不再可信，走**恢复事务**；且工时不出现负数。
///
/// Task 4 之后这条的行为变了：异常采样会**先提交独立系统恢复事务**再返回，
/// 所以会话进 `recovering`、live 暂计归零，待确认段单列在 `pending_ms`。
/// 这正是总纲 §9 要的——检测异常的查询确实写了库。
#[test]
fn a_500ms_wall_setback_triggers_the_recovery_transaction() {
    let mut h = setup();
    h.advance_both(10_000);
    assert_eq!(h.snapshot().active_ms, 10_000, "前 10 秒正常累计");

    // 用户把系统时间往回拨 500ms，单调钟照走
    h.advance_wall_only(-500);
    h.advance_mono_only(1_000);
    let snap = h.snapshot();

    assert_eq!(
        h.coord.last_verdict(),
        SampleVerdict::WallBackwards { d_wall_ms: -500 },
        "回拨必须被标出来"
    );
    // 恢复事务已提交：没有可信检查点 → 整段待确认
    assert_eq!(
        snap.state,
        Some(worktrace_lib::domain::session::SessionState::Recovering)
    );
    assert!(snap.needs_attention());
    assert_eq!(snap.active_ms, 0, "没有可信前缀，已确认工时为 0");
    assert_eq!(snap.pending_ms, Some(11_000), "整段进入待确认，且**单列**");
    assert!(snap.active_ms >= 0, "工时不因墙钟回拨变成负数");
    assert!(!snap.is_running(), "recovering 不再是 running");
}

/// 单调钟倒退是硬故障，与挂钟倒退分开判定。
#[test]
fn a_monotonic_setback_is_a_hard_fault() {
    let mut h = setup();
    h.advance_both(5_000);
    let _ = h.snapshot(); // 先让检测器看到 5 秒的正常推进
    h.advance_mono_only(-100); // 然后单调钟倒退
    let _ = h.snapshot();
    assert!(
        matches!(
            h.coord.last_verdict(),
            SampleVerdict::MonotonicBackwards { .. }
        ),
        "实际 {:?}",
        h.coord.last_verdict()
    );
}

/// 采样失败：协调器报错，**不得**伪造一个可信样本继续。
#[test]
fn a_failed_sample_is_not_silently_replaced() {
    let mut h = setup();
    h.clock.lock().unwrap().fail_forever();

    let err = h.coord.snapshot(&mut h.db).unwrap_err();
    assert_eq!(
        err.code(),
        "RECOVERY_REQUIRED",
        "取不到事实就要走恢复，而不是普通报错"
    );
    assert!(
        !err.message().contains("clock") && !err.message().contains("sample"),
        "技术细节不得进入用户文案：{}",
        err.message()
    );

    // 恢复后照常工作
    h.clock.lock().unwrap().recover();
    h.advance_both(1_000);
    assert_eq!(h.snapshot().as_of, 1_700_000_001_000);
}

/// **心跳前移参照点**：模拟实测的 14ms/分钟漂移，长时间运行不得误报。
///
/// 这是 `docs/validation/p2-clock-mapping.md` 那个发现的回归测试——不前移的话，
/// 两三小时后健康会话会被判成异常。
#[test]
fn heartbeat_reanchoring_keeps_a_long_run_trusted() {
    let mut h = setup();
    // 模拟 60 分钟：每拍 1 秒，挂钟比单调钟多走 14/60 ms
    let mut flagged = 0;
    for i in 1..=3_600_i64 {
        h.advance_mono_only(1_000);
        h.advance_wall_only(1_000 + if i % 60 == 0 { 14 } else { 0 });
        let _ = h.snapshot();
        if h.coord.last_verdict().needs_recovery() {
            flagged += 1;
        }
        // 每 30 秒当作一次成功心跳
        if i % 30 == 0 {
            let s = h.sample();
            h.coord.reanchor_drift_on_heartbeat(s);
        }
    }
    assert_eq!(flagged, 0, "心跳前移参照点后，一小时的正常漂移不该被误判");

    // 对照：同一台机器若从不前移参照点，早晚越界
    let mut h2 = setup();
    let mut hit = false;
    for i in 1..=20_000_i64 {
        h2.advance_mono_only(1_000);
        h2.advance_wall_only(1_000 + if i % 60 == 0 { 14 } else { 0 });
        let _ = h2.snapshot();
        if h2.coord.last_verdict().needs_recovery() {
            hit = true;
            break;
        }
    }
    assert!(hit, "不前移参照点，按实测速率早晚会误报");
}

/// 休眠：判为长间隔，**不需要恢复**。用实测数据（129 秒、Δgap −36ms）。
#[test]
fn a_suspend_does_not_require_recovery() {
    let mut h = setup();
    h.advance_both(1_000);
    let _ = h.snapshot();
    assert_eq!(h.coord.last_verdict(), SampleVerdict::Trusted);

    // 实测：休眠 129.121 秒，挂钟 129.085 秒
    h.advance_mono_only(129_121 - 1_000);
    h.advance_wall_only(129_085 - 1_000);
    let snap = h.snapshot();

    assert!(
        matches!(h.coord.last_verdict(), SampleVerdict::Suspended { .. }),
        "实际 {:?}",
        h.coord.last_verdict()
    );
    assert!(
        !h.coord.last_verdict().needs_recovery(),
        "休眠不得推入 recovering"
    );
    assert!(snap.active_ms > 0, "休眠后的快照仍可用");
}

/// 系统事件与命令、查询走**同一条**检测路径——不能因为来源不同就跳过检测。
#[test]
fn system_events_go_through_the_same_detection_path() {
    let mut h = setup();
    h.advance_both(1_000);
    let _ = h.snapshot();
    assert_eq!(h.coord.last_verdict(), SampleVerdict::Trusted);

    // 「系统事件」在 P2 这一层只是又一次采样——延迟到达的事件带的是旧时刻，
    // 于是表现为一次跳变。
    h.advance_mono_only(1_000);
    h.advance_wall_only(9_000);
    let _ = h.snapshot();
    assert!(
        h.coord.last_verdict().needs_recovery(),
        "系统事件不得绕过检测：{:?}",
        h.coord.last_verdict()
    );
}

/// 显式重建基线后，归属改用新参照且累计偏差从零起算。
#[test]
fn reestablishing_the_anchor_resets_attribution_and_drift() {
    let mut h = setup();
    h.advance_both(60_000);
    assert_eq!(h.snapshot().active_ms, 60_000);

    // 系统时间被校正到真实时刻
    h.advance_wall_only(3_600_000);
    h.coord.reestablish_anchor(h.sample());
    let _ = h.snapshot();
    assert_eq!(
        h.coord.last_verdict(),
        SampleVerdict::Trusted,
        "重建后不该立刻判异常"
    );

    h.advance_both(1_000);
    let snap = h.snapshot();
    assert_eq!(snap.as_of, 1_700_003_661_000, "归属改用新基线");
    assert_eq!(h.coord.last_verdict(), SampleVerdict::Trusted);
}

/// `HEARTBEAT_INTERVAL_MS` 是检查点节奏，不是检测节奏——它不该影响判定。
#[test]
fn the_heartbeat_interval_only_governs_checkpoint_cadence() {
    assert_eq!(HEARTBEAT_INTERVAL_MS, 30_000);

    // 同一个异常，无论距离上一次心跳多久都会被看到
    for elapsed in [1_000_i64, 29_000, 31_000, 120_000] {
        let mut h = setup();
        h.advance_mono_only(elapsed);
        h.advance_wall_only(elapsed + 5_000);
        let _ = h.snapshot();
        assert!(
            h.coord.last_verdict().needs_recovery(),
            "elapsed={elapsed}ms 时异常被漏掉了"
        );
    }
}

/// 未建立基线时不做检测，但也**不谎报**可信——判定保持 `Trusted` 只是因为没得判。
#[test]
fn without_an_anchor_there_is_nothing_to_judge() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    tx.commit().unwrap();

    let clock = Arc::new(Mutex::new(FakeClock::new(1_700_000_000_000, 0)));
    let mut coord = Coordinator::new(Box::new(clock), "run-1");
    let snap = coord.snapshot(&mut db).unwrap();

    assert_eq!(snap.session_id, None);
    assert_eq!(snap.as_of, 1_700_000_000_000, "退回采样本身的挂钟值");
    assert_eq!(coord.last_verdict(), SampleVerdict::Trusted);
}

/// 预算与归属可以同时工作：倒计时会话的剩余随暂计推进。
#[test]
fn attribution_and_budget_advance_together() {
    let mut h = setup();
    h.db.connection()
        .execute(
            "UPDATE work_session SET timer_kind='countdown', target_duration_ms=5000 WHERE id='s1'",
            [],
        )
        .unwrap();
    h.coord.load_session(h.db.connection(), "s1").unwrap();

    h.advance_both(3_000);
    let snap = h.snapshot();
    assert_eq!(snap.timer_kind, Some(TimerKind::Countdown));
    assert_eq!(snap.active_ms, 3_000);
    assert_eq!(snap.remaining_ms, Some(2_000));
    assert_eq!(snap.overtime_ms, Some(0));

    h.advance_both(4_000);
    let late = h.snapshot();
    assert_eq!(late.remaining_ms, Some(0));
    assert_eq!(late.overtime_ms, Some(2_000));
}
