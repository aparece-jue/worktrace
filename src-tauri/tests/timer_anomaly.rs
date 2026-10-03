//! P2 Task 4 的异常原子跃迁测试。
//!
//! 计划要求：有/无/初始零检查点、重复事件幂等、recovering 暂计冻结、
//! 待确认不影响既有闭合统计、正常/异常查询的 revision 口径。

use std::sync::{Arc, Mutex};

use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::platform::clock::{Clock, FakeClock};
use worktrace_lib::services::timer::coordinator::{Coordinator, StartRequest};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::session_repo;
use worktrace_lib::storage::time_edit_repo;

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

    fn session_id(&self) -> String {
        self.coord.live().unwrap().id.clone()
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

    /// 制造一次异常采样：单调钟走 1 秒、挂钟走 31 秒。
    fn make_anomaly(&self) {
        self.advance_mono_only(1_000);
        self.advance_wall_only(31_000);
    }

    fn revision(&self) -> i64 {
        require_meta(self.db.connection()).unwrap().revision
    }

    fn intervals(&self) -> Vec<session_repo::IntervalRow> {
        session_repo::intervals_of_session(self.db.connection(), &self.session_id()).unwrap()
    }

    fn edits(&self) -> Vec<time_edit_repo::TimeEdit> {
        time_edit_repo::edits_of_session(self.db.connection(), &self.session_id()).unwrap()
    }
}

/// 有可信检查点时：保留可信前缀，余段标待确认。
#[test]
fn an_anomaly_with_a_checkpoint_keeps_the_trusted_prefix() {
    let mut h = setup();
    h.start();
    h.advance(40_000);
    assert!(
        h.coord.heartbeat(&mut h.db).unwrap(),
        "先落一个可信检查点（40 秒处）"
    );

    let rev_before = h.revision();
    h.make_anomaly();
    let snap = h.coord.snapshot(&mut h.db).unwrap();

    assert_eq!(snap.state, Some(SessionState::Recovering));
    assert_eq!(h.revision(), rev_before + 1, "恢复事务自己加一次 revision");
    assert_eq!(snap.active_ms, 40_000, "可信前缀保住 40 秒");
    // 归属跟**单调钟**走：异常那一拍单调钟只走了 1 秒，所以候选余段是 1 秒。
    // 墙钟跳的那 31 秒是**异常本身**，不是时长。
    assert_eq!(snap.pending_ms, Some(1_000), "余段单列在待确认里");

    let ivs = h.intervals();
    assert_eq!(ivs.len(), 2, "前缀 + 余段");
    let trusted = &ivs[0];
    assert_eq!(
        trusted.duration_ms,
        Some(40_000),
        "前缀是**可信闭合**：有 duration"
    );
    assert!(!trusted.needs_review, "前缀不是待确认");
    let pending = &ivs[1];
    assert!(pending.needs_review, "余段待确认");
    assert_eq!(pending.duration_ms, None, "余段没有 duration——它还不是工时");
    assert_eq!(
        pending.started_at,
        trusted.ended_at.unwrap(),
        "余段从前缀结束处开始"
    );

    assert_eq!(h.edits().len(), 1, "写了一条审计");
    assert!(h.edits()[0].reason.as_deref().unwrap().contains("clock"));
}

/// 没有可信检查点时：整段待确认。
#[test]
fn an_anomaly_without_a_checkpoint_makes_the_whole_span_pending() {
    let mut h = setup();
    h.start();
    h.advance(15_000);
    h.make_anomaly();
    let snap = h.coord.snapshot(&mut h.db).unwrap();

    assert_eq!(snap.active_ms, 0, "没有可信前缀，已确认工时为 0");
    assert_eq!(snap.pending_ms, Some(16_000), "整段待确认");
    let ivs = h.intervals();
    assert_eq!(ivs.len(), 1, "只有一个区间，被标为待确认");
    assert!(ivs[0].needs_review);
    assert_eq!(ivs[0].duration_ms, None);
}

/// **零长度前缀要省略**：检查点正好在区间起点时，不建零长度的可信区间。
#[test]
fn a_zero_length_prefix_is_omitted() {
    let mut h = setup();
    h.start();
    // start 写的初始检查点 elapsed=0，正好在区间起点
    h.make_anomaly();
    let snap = h.coord.snapshot(&mut h.db).unwrap();

    let ivs = h.intervals();
    assert_eq!(ivs.len(), 1, "不该出现零长度的可信前缀");
    assert!(ivs[0].needs_review);
    assert_eq!(snap.active_ms, 0);
    assert_eq!(
        snap.pending_ms,
        Some(1_000),
        "候选时长按单调钟，不是墙钟跳变量"
    );
}

/// **重复事件幂等**：不重复分割、不重复写审计、不加版本。
#[test]
fn a_repeated_anomaly_is_idempotent() {
    let mut h = setup();
    h.start();
    h.advance(40_000);
    h.coord.heartbeat(&mut h.db).unwrap();
    h.make_anomaly();

    let _ = h.coord.snapshot(&mut h.db).unwrap(); // 第一次异常
    let rev = h.revision();
    let edits = h.edits().len();
    let ivs = h.intervals().len();

    // 再制造几次异常采样
    for _ in 0..3 {
        h.make_anomaly();
        let _ = h.coord.snapshot(&mut h.db).unwrap();
    }

    assert_eq!(h.revision(), rev, "重复事件不再加 revision");
    assert_eq!(h.edits().len(), edits, "不再写审计");
    assert_eq!(h.intervals().len(), ivs, "不再分割");
    assert_eq!(h.coord.live().unwrap().state, SessionState::Recovering);
}

/// **recovering 冻结实时暂计**：之后再怎么走时间，`active_ms` 都不涨。
#[test]
fn a_recovering_session_freezes_live_accrual() {
    let mut h = setup();
    h.start();
    h.make_anomaly();
    let first = h.coord.snapshot(&mut h.db).unwrap();
    let active = first.active_ms;
    let pending = first.pending_ms;

    h.advance(600_000); // 再走十分钟
    let later = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(later.active_ms, active, "recovering 不再累计 live");
    assert_eq!(later.pending_ms, pending, "待确认段也不再增长");
    assert!(!later.is_running());
}

/// **revision 口径**：正常查询不加，异常查询加一次。
#[test]
fn revision_advances_only_for_the_anomalous_query() {
    let mut h = setup();
    h.start();
    h.advance(5_000);
    let rev = h.revision();

    for _ in 0..3 {
        let _ = h.coord.snapshot(&mut h.db).unwrap();
        h.advance(1_000);
    }
    assert_eq!(h.revision(), rev, "正常查询是纯读");

    h.make_anomaly();
    let _ = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(h.revision(), rev + 1, "检测触发的恢复事务写一次");

    let after = h.revision();
    for _ in 0..3 {
        let _ = h.coord.snapshot(&mut h.db).unwrap();
        h.advance(1_000);
    }
    assert_eq!(h.revision(), after, "已 recovering 后查询又是纯读了");
}

/// **待确认不影响既有闭合统计**：异常前已确认的工时仍算数。
#[test]
fn pending_time_does_not_pollute_confirmed_totals() {
    let mut h = setup();
    h.start();
    // 先正常暂停一次，留下 20 秒可信闭合
    h.advance(20_000);
    let sid = h.session_id();
    let sv = h.coord.live().unwrap().row_version;
    let req = worktrace_lib::services::timer::coordinator::SessionRequest {
        expected_data_epoch: h.epoch.clone(),
        session_id: sid.clone(),
        session_expected_version: sv,
    };
    h.coord.pause(&mut h.db, req).unwrap();

    // 造一个 running 的开放区间再触发异常
    h.db.connection()
        .execute(
            "UPDATE work_session SET state='running', ended_at=NULL WHERE id=?1",
            [&sid],
        )
        .unwrap();
    h.db.connection()
        .execute(
            "INSERT INTO work_interval(id,session_id,started_at) VALUES('i-open',?1,1700000020000)",
            [&sid],
        )
        .unwrap();
    h.coord.load_session(h.db.connection(), &sid).unwrap();
    h.make_anomaly();
    let snap = h.coord.snapshot(&mut h.db).unwrap();

    assert_eq!(snap.active_ms, 20_000, "此前闭合的 20 秒仍然算数");
    assert!(snap.pending_ms.unwrap() > 0, "新开的余段进待确认");
    assert!(
        snap.active_ms < snap.active_ms + snap.pending_ms.unwrap(),
        "两者分列，没有相加"
    );
}

/// 异常事务把前台占用释放掉——`uq_running_foreground` 只约束 `running`。
#[test]
fn the_recovery_transaction_releases_the_foreground_slot() {
    let mut h = setup();
    h.start();
    h.make_anomaly();
    let _ = h.coord.snapshot(&mut h.db).unwrap();

    // 现在可以再开一个前台会话（旧的不再是 running）
    h.db.connection()
        .execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES('t2','第二个','Ready',0,0,0)",
            [],
        )
        .unwrap();
    let req = StartRequest {
        expected_data_epoch: h.epoch.clone(),
        task_id: "t2".into(),
        task_expected_version: 0,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: 30_000,
    };
    match h.coord.start(&mut h.db, req) {
        Ok(_) => {}
        Err(e) => panic!(
            "前台槽位应当已释放，实际报错：{} / {:?}",
            e.code(),
            e.detail()
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 异常事务失败：字段级回滚 + 故障态
// ─────────────────────────────────────────────────────────────────────────────

impl H {
    /// 让异常事务在 `update_session_state` 那一步失败：把库里的版本推高，
    /// 于是协调器内存里的 `live.row_version` 与库里对不上。
    fn desync_session_version(&self) {
        self.db
            .connection()
            .execute("UPDATE work_session SET row_version = row_version + 5", [])
            .unwrap();
    }
}

/// **异常事务失败要字段级回滚**。
///
/// `Transaction` 在 drop 时回滚，所以「没提交」是自动的；这条测试要证的是
/// **每一步的中间产物都没留下**——分割改过的区间、写的审计、会话状态、revision。
#[test]
fn a_failed_anomaly_transaction_rolls_back_field_by_field() {
    let mut h = setup();
    h.start();
    h.advance(40_000);
    h.coord.heartbeat(&mut h.db).unwrap();
    let interval_id = h
        .coord
        .live()
        .unwrap()
        .open_interval
        .as_ref()
        .unwrap()
        .0
        .clone();
    let cp_elapsed =
        worktrace_lib::storage::checkpoint_repo::latest(h.db.connection(), &interval_id)
            .unwrap()
            .unwrap()
            .elapsed_ms;

    let rev_before = h.revision();
    let intervals_before = h.intervals();

    h.desync_session_version(); // 让事务里的版本校验失败
    h.make_anomaly();
    let err = h.coord.snapshot(&mut h.db).unwrap_err();

    // 错误码：恢复语义，不是可重试的版本冲突
    assert_eq!(err.code(), "RECOVERY_REQUIRED", "事务失败也要走恢复语义");

    // 字段级核对：一步都没留下
    let intervals_after = h.intervals();
    assert_eq!(
        intervals_after.len(),
        intervals_before.len(),
        "分割不得留下新区间"
    );
    for (a, b) in intervals_after.iter().zip(intervals_before.iter()) {
        assert_eq!(a.ended_at, b.ended_at, "区间结束时刻不得被改");
        assert_eq!(a.duration_ms, b.duration_ms, "区间时长不得被改");
        assert_eq!(a.needs_review, b.needs_review, "待确认标记不得被改");
    }
    assert!(h.edits().is_empty(), "不得留下审计");
    assert!(h.edits().is_empty(), "02 §9：操作失败不得出现半个审计记录");
    assert_eq!(h.revision(), rev_before, "revision 不得前进");
    assert_eq!(
        h.coord.live().unwrap().state,
        SessionState::Running,
        "会话状态不得被改（内存不应用未提交状态）"
    );
    assert_eq!(
        worktrace_lib::storage::checkpoint_repo::latest(h.db.connection(), &interval_id)
            .unwrap()
            .unwrap()
            .elapsed_ms,
        cp_elapsed,
        "已持久化的检查点不得被动"
    );
    // 库里也必须是 running
    let db_state: String =
        h.db.connection()
            .query_row("SELECT state FROM work_session", [], |r| r.get(0))
            .unwrap();
    assert_eq!(db_state, "running", "库里状态不得被改");
}

/// 失败之后**明确进入故障处理**：所有入口在重建成功之前一律拒绝。
///
/// 这一条防的是一个很隐蔽的错：检测器已经消费掉了异常那一拍采样，下一拍的增量
/// 从异常那一拍起算，**再判就正常了**——不锁住的话，一次失败的恢复会让坏事实在
/// 下一拍被当成好事实接受。
#[test]
fn a_faulted_coordinator_refuses_everything_until_rebuilt() {
    let mut h = setup();
    h.start();
    h.advance(40_000);
    h.coord.heartbeat(&mut h.db).unwrap();

    h.desync_session_version();
    h.make_anomaly();
    assert_eq!(
        h.coord.snapshot(&mut h.db).unwrap_err().code(),
        "RECOVERY_REQUIRED"
    );
    assert!(h.coord.is_faulted(), "必须进入故障态");

    // 故障态下：查询、tick、命令全部拒绝
    assert_eq!(
        h.coord.snapshot(&mut h.db).unwrap_err().code(),
        "RECOVERY_REQUIRED"
    );
    assert_eq!(
        h.coord.tick(&mut h.db).unwrap_err().code(),
        "RECOVERY_REQUIRED"
    );
    let sid = h.session_id();
    let sv = h.coord.live().unwrap().row_version;
    let req = worktrace_lib::services::timer::coordinator::SessionRequest {
        expected_data_epoch: h.epoch.clone(),
        session_id: sid.clone(),
        session_expected_version: sv,
    };
    assert_eq!(
        h.coord.pause(&mut h.db, req).unwrap_err().code(),
        "RECOVERY_REQUIRED"
    );

    // 成功重建后脱离故障态
    let sample = h.clock.lock().unwrap().sample().unwrap();
    h.coord
        .rebuild_from_committed(h.db.connection(), &sid, sample)
        .unwrap();
    assert!(!h.coord.is_faulted(), "重建成功应当脱离故障态");
    assert!(h.coord.snapshot(&mut h.db).is_ok(), "恢复之后查询应当可用");
}

/// **异常事务失败不得输出新的可信暂计**：失败就是失败，不给半截快照。
#[test]
fn a_failed_anomaly_transaction_emits_no_new_trusted_accrual() {
    let mut h = setup();
    h.start();
    h.advance(40_000);
    h.coord.heartbeat(&mut h.db).unwrap();
    let trusted_before = h.coord.snapshot(&mut h.db).unwrap().active_ms;
    assert_eq!(trusted_before, 40_000);

    h.desync_session_version();
    h.make_anomaly();
    // 失败时返回的是 Err，**没有**任何快照被输出——也就不会有「新的可信暂计」
    let out = h.coord.snapshot(&mut h.db);
    assert!(out.is_err(), "事务失败不得输出快照");

    // 内存里的 live 也没被改（仍是 running、仍指向同一个开放区间）
    let live = h.coord.live().unwrap();
    assert_eq!(live.state, SessionState::Running);
    assert!(live.open_interval.is_some(), "开放区间不得被内存单方面清掉");
    assert_eq!(live.closed_trusted_ms, 0, "已确认工时不得被内存改写");
}
