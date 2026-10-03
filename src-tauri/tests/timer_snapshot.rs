//! P2 Task 1 的快照测试。
//!
//! 计划要求覆盖：暂停冻结、倒计时超时、同一采样查询/tick 一致、旧会话/版本 tick 被过滤、
//! 预算重新载入不丢失。
//!
//! 另外把 P1 交付时**零调用零测试**的 `session_repo::intervals_of_session` 测起来——
//! 它是本任务算 `active_ms` 的入口，P1 没有消费者，所以「P1 无回归」对它原本是空洞的。

use std::sync::{Arc, Mutex};

use worktrace_lib::domain::session::{SessionState, TimerKind};
use worktrace_lib::platform::clock::{Clock, FakeClock};
use worktrace_lib::services::timer::coordinator::Coordinator;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::session_repo;

struct Harness {
    _dir: tempfile::TempDir,
    db: Db,
    clock: Arc<Mutex<FakeClock>>,
    coord: Coordinator,
}

/// 建库 + 一个 run/task/会话，并把时钟交给协调器。
///
/// 时钟用 `Arc<Mutex<FakeClock>>`：协调器持有 `Box<dyn Clock>`，测试仍要能推进它。
fn harness(timer_kind: TimerKind, target: Option<i64>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();

    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    tx.execute(
        "INSERT INTO application_run(id, started_at) VALUES('run-1', 1000)",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Doing',0,1000,1000)",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,target_duration_ms,
                                  started_at,row_version)
         VALUES('s1','t1','run-1','FOREGROUND','running',?1,?2,1000,0)",
        rusqlite::params![timer_kind.as_str(), target],
    )
    .unwrap();
    tx.commit().unwrap();

    let clock = Arc::new(Mutex::new(FakeClock::new(1_700_000_000_000, 0)));
    let coord = Coordinator::new(Box::new(Arc::clone(&clock)), "run-1");

    Harness {
        _dir: dir,
        db,
        clock,
        coord,
    }
}

impl Harness {
    /// 插一段区间。`ended_at = None` 表示仍开放。
    fn interval(&self, id: &str, start: i64, end: Option<i64>, review: bool, voided: bool) {
        let duration = end.map(|e| e - start);
        self.db
            .connection()
            .execute(
                "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms,
                                           needs_review,voided_at)
                 VALUES(?1,'s1',?2,?3,?4,?5,?6)",
                rusqlite::params![
                    id,
                    start,
                    end,
                    duration,
                    review as i64,
                    if voided { Some(1) } else { None }
                ],
            )
            .unwrap();
    }

    fn set_state(&self, state: SessionState, version: i64) {
        self.db
            .connection()
            .execute(
                "UPDATE work_session SET state = ?1, row_version = ?2 WHERE id = 's1'",
                rusqlite::params![state.as_str(), version],
            )
            .unwrap();
    }

    /// 推进假时钟（两个数值一起，模拟正常流逝）。
    fn advance(&self, ms: i64) {
        self.clock.lock().unwrap().advance_both(ms);
    }

    /// 装载会话并建立基线；`at_monotonic` 是建立基线时的单调读数。
    fn start(&mut self, at_monotonic: i64) {
        self.clock.lock().unwrap().advance_monotonic(at_monotonic);
        let sample = self.clock.lock().unwrap().sample().unwrap();
        self.coord.establish_anchor(sample);
        self.coord.load_session(self.db.connection(), "s1").unwrap();
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// 计划原文：「暂停冻结」——暂停后不再累计 live，`active_ms` 停在已确认工时上。
#[test]
fn pausing_freezes_active_ms() {
    let mut h = harness(TimerKind::Stopwatch, None);
    h.interval(
        "i1",
        1_700_000_000_000,
        Some(1_700_000_010_000),
        false,
        false,
    ); // 已确认 10s
    h.start(0);

    // running：开放区间从**基线那一刻**开始（归属挂钟 = 基线的 wall_at），再走 5 秒。
    // 注意 started_at 是归属挂钟量级，不是单调读数——写错就会算出天文数字。
    h.interval("i2", 1_700_000_000_000, None, false, false);
    h.coord.load_session(h.db.connection(), "s1").unwrap();
    h.advance(5_000);
    let running = h.coord.snapshot(h.db.connection()).unwrap();
    assert_eq!(running.active_ms, 15_000, "已确认 10s + 暂计 5s");

    // 暂停：库里没有开放区间了，状态也变了
    // ended_at 必须等于 started_at + duration_ms，否则撞 schema 的 ck_interval_duration
    h.db.connection()
        .execute(
            "UPDATE work_interval SET ended_at=1700000005000, duration_ms=5000 WHERE id='i2'",
            [],
        )
        .unwrap();
    h.set_state(SessionState::Paused, 1);
    h.coord.load_session(h.db.connection(), "s1").unwrap();

    h.advance(60_000); // 暂停期间过了一分钟
    let paused = h.coord.snapshot(h.db.connection()).unwrap();
    assert_eq!(paused.active_ms, 15_000, "暂停后不得再增长");
    assert_eq!(paused.state, Some(SessionState::Paused));
    assert!(!paused.is_running());
}

/// 计划原文：「倒计时超时」；正计时的剩余/超时必须为 `None`。
#[test]
fn countdown_reports_remaining_then_overtime_and_stopwatch_reports_nothing() {
    let mut h = harness(TimerKind::Countdown, Some(10_000));
    h.interval("i1", 1_700_000_000_000, None, false, false);
    h.start(0);

    h.advance(3_000);
    let early = h.coord.snapshot(h.db.connection()).unwrap();
    assert_eq!(early.remaining_ms, Some(7_000));
    assert_eq!(early.overtime_ms, Some(0), "未超时为 0，不是 None");
    assert_eq!(early.timer_kind, Some(TimerKind::Countdown));

    h.advance(10_000); // 共 13 秒，超 3 秒
    let late = h.coord.snapshot(h.db.connection()).unwrap();
    assert_eq!(late.remaining_ms, Some(0), "超时后剩余不为负");
    assert_eq!(late.overtime_ms, Some(3_000));

    // 正计时：两个字段都是 None
    let mut s = harness(TimerKind::Stopwatch, None);
    s.interval("i1", 1_700_000_000_000, None, false, false);
    s.start(0);
    s.advance(99_000);
    let sw = s.coord.snapshot(s.db.connection()).unwrap();
    assert_eq!(sw.remaining_ms, None, "正计时没有剩余");
    assert_eq!(sw.overtime_ms, None, "正计时不会超时");
}

/// 计划原文：「同一采样查询/tick 一致」。
#[test]
fn snapshot_and_tick_agree_on_the_same_sample() {
    let mut h = harness(TimerKind::Stopwatch, None);
    h.interval("i1", 1_700_000_000_000, None, false, false);
    h.start(0);
    h.advance(4_000);

    // 时钟不动 → 两次调用看到的是同一个采样
    let snap = h.coord.snapshot(h.db.connection()).unwrap();
    let tick = h.coord.tick(h.db.connection()).unwrap();

    assert_eq!(snap.as_of, tick.as_of, "as_of 必须一致");
    assert_eq!(snap.active_ms, tick.active_ms, "active_ms 必须一致");
    assert_eq!(snap.session_version, tick.session_version);

    // 唯一区别：tick 让 tick_seq 前进
    assert_eq!(tick.tick_seq, snap.tick_seq + 1);
}

/// 计划原文：「旧会话/版本 tick 被过滤」。
#[test]
fn a_tick_from_a_stale_session_or_version_is_flagged() {
    let mut h = harness(TimerKind::Stopwatch, None);
    h.interval("i1", 1_700_000_000_000, None, false, false);
    h.start(0);

    assert!(!h.coord.is_stale_tick("s1", 0), "当前会话与版本，不算过期");
    assert!(h.coord.is_stale_tick("s1", 7), "版本对不上 → 过期");
    assert!(h.coord.is_stale_tick("s-other", 0), "会话对不上 → 过期");

    // 会话推进一个版本后，旧的 0 就该过期
    h.set_state(SessionState::Paused, 1);
    h.coord.load_session(h.db.connection(), "s1").unwrap();
    assert!(h.coord.is_stale_tick("s1", 0), "版本已前进，旧的应当过期");
    assert!(!h.coord.is_stale_tick("s1", 1));
}

/// 计划原文：「预算重新载入不丢失」。
#[test]
fn the_budget_survives_reloading_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("w.db");
    let clock = Arc::new(Mutex::new(FakeClock::new(1_700_000_000_000, 0)));

    {
        let mut db = Db::open(&path).unwrap();
        migrate(db.connection()).unwrap();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        init_meta(&tx).unwrap();
        tx.execute(
            "INSERT INTO application_run(id, started_at) VALUES('run-1', 1000)",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES('t1','任务','Doing',0,1000,1000)",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,target_duration_ms,
                                      started_at,row_version)
             VALUES('s1','t1','run-1','FOREGROUND','running','countdown',60000,1000,0)",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // 重新打开库、重新装载——预算必须原样回来
    let db = Db::open(&path).unwrap();
    let mut coord = Coordinator::new(Box::new(Arc::clone(&clock)), "run-1");
    coord.load_session(db.connection(), "s1").unwrap();

    let live = coord.live().expect("session loaded");
    assert_eq!(live.budget.kind, TimerKind::Countdown);
    assert_eq!(live.budget.target_duration_ms, Some(60_000));
    assert_eq!(live.state, SessionState::Running);
    assert_eq!(live.row_version, 0);
}

/// 把 P1 里**零调用零测试**的 `intervals_of_session` 测起来，并确认分类口径：
/// 只有可信闭合进已确认工时；待确认与已作废都不进。
#[test]
fn intervals_of_session_splits_trusted_pending_and_voided() {
    let mut h = harness(TimerKind::Stopwatch, None);
    const W: i64 = 1_699_999_980_000; // 归属挂钟量级，且三段都排在基线时刻之前
    h.interval("i-trusted", W, Some(W + 5000), false, false); // 5s，可信
    h.interval("i-pending", W + 5000, Some(W + 8000), true, false); // 待确认，不计
    h.interval("i-voided", W + 8000, Some(W + 11000), false, true); // 已作废，不计
                                                                    // 开放区间从基线那一刻起（归属挂钟），这样暂计才等于推进的时长
    h.interval("i-open", 1_700_000_000_000, None, false, false);

    let rows = session_repo::intervals_of_session(h.db.connection(), "s1").unwrap();
    assert_eq!(rows.len(), 4, "四段都应当读出来（按 started_at 排序）");
    assert_eq!(rows[0].id, "i-trusted", "按 started_at 排序");
    assert_eq!(rows[3].id, "i-open", "开放区间起点最晚");

    h.start(0);
    let live = h.coord.live().unwrap();
    assert_eq!(live.closed_trusted_ms, 5_000, "只有可信闭合那一段计入");
    assert_eq!(
        live.open_interval.as_ref().map(|(id, _)| id.as_str()),
        Some("i-open")
    );

    h.advance(3_000);
    let snap = h.coord.snapshot(h.db.connection()).unwrap();
    assert_eq!(snap.active_ms, 8_000, "5s 已确认 + 3s 暂计");
}

/// `recovering` 不累计 live——待确认的时间不能算成工时。
#[test]
fn a_recovering_session_accrues_no_live_time() {
    let mut h = harness(TimerKind::Stopwatch, None);
    h.interval(
        "i-trusted",
        1_699_999_999_000,
        Some(1_699_999_999_000 + 3000),
        false,
        false,
    ); // 3s 已确认
    h.start(0);
    h.set_state(SessionState::Recovering, 1);
    h.coord.load_session(h.db.connection(), "s1").unwrap();

    h.advance(30_000);
    let snap = h.coord.snapshot(h.db.connection()).unwrap();
    assert_eq!(snap.active_ms, 3_000, "recovering 不得叠加可疑 live");
    assert!(snap.needs_attention());
    assert!(!snap.is_running());
}

/// 没有会话时返回 `idle` 快照，而不是报错——界面要能显示「当前没有在计时」。
#[test]
fn an_idle_coordinator_returns_an_idle_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    tx.commit().unwrap();

    let clock = Arc::new(Mutex::new(FakeClock::new(1_700_000_000_000, 0)));
    let mut coord = Coordinator::new(Box::new(clock), "run-1");
    let snap = coord.snapshot(db.connection()).unwrap();

    assert_eq!(snap.session_id, None);
    assert_eq!(snap.active_ms, 0);
    assert_eq!(snap.remaining_ms, None);
    assert!(!snap.needs_attention());
    assert_eq!(snap.run_id, "run-1");
}

/// `tick_seq` 在**当前 run 内**递增：换一个会话也不清零。
#[test]
fn tick_seq_keeps_counting_across_sessions_within_a_run() {
    let mut h = harness(TimerKind::Stopwatch, None);
    h.interval("i1", 1_700_000_000_000, None, false, false);
    h.start(0);

    for _ in 0..3 {
        h.coord.tick(h.db.connection()).unwrap();
    }
    assert_eq!(h.coord.tick_seq(), 3);

    // 换一个会话（同一 run）：不清零。
    // 先把 s1 收掉——uq_running_foreground 只允许一个 running 的前台会话。
    h.db.connection()
        .execute("UPDATE work_session SET state='finished' WHERE id='s1'", [])
        .unwrap();
    h.db.connection()
        .execute(
            "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,
                                      started_at,row_version)
             VALUES('s2','t1','run-1','FOREGROUND','running','stopwatch',2000,0)",
            [],
        )
        .unwrap();
    h.coord.load_session(h.db.connection(), "s2").unwrap();
    let snap = h.coord.tick(h.db.connection()).unwrap();
    assert_eq!(snap.tick_seq, 4, "新会话不清零 tick_seq");
    assert_eq!(snap.session_id.as_deref(), Some("s2"));
}
