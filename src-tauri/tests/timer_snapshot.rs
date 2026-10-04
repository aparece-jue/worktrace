//! P2 Task 1 的快照测试。
//!
//! 计划要求覆盖：暂停冻结、倒计时超时、同一采样查询/tick 一致、旧会话/版本 tick 被过滤、
//! 预算重新载入不丢失。
//!
//! 另外把 P1 交付时**零调用零测试**的 `session_repo::intervals_of_session` 测起来——
//! 它是本任务算 `active_ms` 的入口，P1 没有消费者，所以「P1 无回归」对它原本是空洞的。

use std::sync::{Arc, Mutex};

use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::platform::clock::{Clock, FakeClock};
use worktrace_lib::services::timer::coordinator::{
    Coordinator, ResumeRequest, SessionRequest, StartRequest,
};
use worktrace_lib::services::timer::snapshot::TimerSnapshot;
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
    let running = h.coord.snapshot(&mut h.db).unwrap();
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
    let paused = h.coord.snapshot(&mut h.db).unwrap();
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
    let early = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(early.remaining_ms, Some(7_000));
    assert_eq!(early.overtime_ms, Some(0), "未超时为 0，不是 None");
    assert_eq!(early.timer_kind, Some(TimerKind::Countdown));

    h.advance(10_000); // 共 13 秒，超 3 秒
    let late = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(late.remaining_ms, Some(0), "超时后剩余不为负");
    assert_eq!(late.overtime_ms, Some(3_000));

    // 正计时：两个字段都是 None
    let mut s = harness(TimerKind::Stopwatch, None);
    s.interval("i1", 1_700_000_000_000, None, false, false);
    s.start(0);
    s.advance(99_000);
    let sw = s.coord.snapshot(&mut s.db).unwrap();
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
    let snap = h.coord.snapshot(&mut h.db).unwrap();
    let tick = h.coord.tick(&mut h.db).unwrap();

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
    let snap = h.coord.snapshot(&mut h.db).unwrap();
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
    let snap = h.coord.snapshot(&mut h.db).unwrap();
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
    let snap = coord.snapshot(&mut db).unwrap();

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
        h.coord.tick(&mut h.db).unwrap();
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
    let snap = h.coord.tick(&mut h.db).unwrap();
    assert_eq!(snap.tick_seq, 4, "新会话不清零 tick_seq");
    assert_eq!(snap.session_id.as_deref(), Some("s2"));
}

// ─────────────────────────────────────────────────────────────────────────────
// 快照的任务身份（P7 Task 3 的契约补口）
//
// 缺口：`resume_timer` 要 `task_id` + `task_expected_version`，而「这条暂停的会话属于
// 哪个任务」原本**没有**任何读路径——`TaskRow` 不带会话、24 条命令里没有 session→task
// 的查询、托盘只做 `pause`。于是重开窗口（F-009 的正常路径）或托盘暂停之后，前端
// **构造不出**「继续」按钮的请求。下面四条把这条缝钉住。
// ─────────────────────────────────────────────────────────────────────────────

/// 前端 `buildResumeRequest`（`src/components/timerRequests.ts`）的逐条镜像：
/// **任务身份两件套同时可得**才能构造出 `ResumeRequest`，否则返回 `None`
/// （调用方据此让「继续」按钮不出现——不是"点了再失败"）。
fn resume_request_of(snapshot: &TimerSnapshot) -> Option<ResumeRequest> {
    let task_id = snapshot.task_id.clone()?;
    let task_row_version = snapshot.task_row_version?;
    Some(ResumeRequest {
        expected_data_epoch: snapshot.data_epoch.clone(),
        task_id,
        task_expected_version: task_row_version,
        session_id: snapshot.session_id.clone()?,
        session_expected_version: snapshot.session_version?,
    })
}

/// 空闲快照**没有**任务身份：两个字段都是 `None`，JSON 里是 `null`。
///
/// 更要紧的是**键必须存在**：`serde(skip_serializing_if = "Option::is_none")` 会让
/// 空闲快照直接少两个键，而 `src/types/ipc.ts` 声明的是 `string | null`——前端读到的
/// 会是 `undefined`。`Value::Null` 与"缺键"在下标取值上长得一样，所以两条 `contains_key`
/// 不是重复断言，是这条用例里唯一能分辨二者的断言。
#[test]
fn an_idle_snapshot_carries_no_task_identity() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    tx.commit().unwrap();

    let clock = Arc::new(Mutex::new(FakeClock::new(1_700_000_000_000, 0)));
    let mut coord = Coordinator::new(Box::new(clock), "run-1");
    let snap = coord.snapshot(&mut db).unwrap();

    assert_eq!(snap.task_id, None, "没有会话就没有任务身份");
    assert_eq!(snap.task_row_version, None);

    let json = serde_json::to_value(&snap).unwrap();
    let object = json.as_object().expect("快照的 JSON 形状是对象");
    assert!(object.contains_key("task_id"), "空闲时也必须有这个键");
    assert!(
        object.contains_key("task_row_version"),
        "空闲时也必须有这个键"
    );
    assert_eq!(json["task_id"], serde_json::Value::Null, "Option ⇒ null");
    assert_eq!(json["task_row_version"], serde_json::Value::Null);
}

/// 有会话时两个字段跟着**这条会话**走：running 与 paused 各一条，且换会话就换身份。
///
/// 两条会话分属两个不同版本的任务（0 与 7），是为了让「填常量」在这里也活不下来。
#[test]
fn a_snapshot_carries_the_task_identity_of_its_own_session() {
    let mut h = harness(TimerKind::Stopwatch, None);
    h.start(0);

    let running = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(running.session_id.as_deref(), Some("s1"));
    assert_eq!(running.state, Some(SessionState::Running));
    assert_eq!(running.task_id.as_deref(), Some("t1"), "会话属于 t1");
    assert_eq!(running.task_row_version, Some(0), "t1 的 row_version");

    // 另一个任务（版本 7）+ 它的暂停会话：身份必须跟着会话换。
    h.db.connection()
        .execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES('t2','另一个任务','Doing',7,1000,1000)",
            [],
        )
        .unwrap();
    h.db.connection()
        .execute("UPDATE work_session SET state='finished' WHERE id='s1'", [])
        .unwrap();
    h.db.connection()
        .execute(
            "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,
                                      started_at,row_version)
             VALUES('s2','t2','run-1','FOREGROUND','paused','stopwatch',2000,3)",
            [],
        )
        .unwrap();
    h.coord.load_session(h.db.connection(), "s2").unwrap();

    let paused = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(paused.session_id.as_deref(), Some("s2"));
    assert_eq!(paused.state, Some(SessionState::Paused));
    assert_eq!(
        paused.task_id.as_deref(),
        Some("t2"),
        "不是常量，跟着会话走"
    );
    assert_eq!(paused.task_row_version, Some(7), "t2 的 row_version");
    assert_eq!(
        paused.session_version,
        Some(3),
        "会话版本与任务版本是两份版本，不得串用"
    );
}

/// 命令路径的小夹具：库里只有 run / project / task，**没有会话**——会话由 `start` 真开出来。
///
/// 与上面的 `harness()` 分开：那个是「库里已有会话、直接装载」的查询夹具，用它跑
/// `start` 会撞 `uq_running_foreground`。
struct CommandHarness {
    _dir: tempfile::TempDir,
    db: Db,
    clock: Arc<Mutex<FakeClock>>,
    coord: Coordinator,
    epoch: String,
}

fn command_harness() -> CommandHarness {
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
    tx.execute(
        "INSERT INTO task(id,project_id,title,status,row_version,created_at,updated_at)
         VALUES('t1','p1','任务','Doing',0,1000,1000)",
        [],
    )
    .unwrap();
    tx.commit().unwrap();

    let clock = Arc::new(Mutex::new(FakeClock::new(1_700_000_000_000, 0)));
    let coord = Coordinator::new(Box::new(Arc::clone(&clock)), "run-1");
    CommandHarness {
        _dir: dir,
        db,
        clock,
        coord,
        epoch: meta.data_epoch,
    }
}

impl CommandHarness {
    fn advance(&self, ms: i64) {
        self.clock.lock().unwrap().advance_both(ms);
    }

    fn start_request(&self) -> StartRequest {
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

    fn session_request(&self, snapshot: &TimerSnapshot) -> SessionRequest {
        SessionRequest {
            expected_data_epoch: self.epoch.clone(),
            session_id: snapshot.session_id.clone().expect("有会话"),
            session_expected_version: snapshot.session_version.expect("有会话版本"),
        }
    }
}

/// **本次补口的目的**：真实的 `start → pause` 之后，只拿一份**全新查询**的快照
/// （模拟"重开窗口"：本窗口没有那次 `start` 的上下文），用快照里的两个任务字段
/// 就能构造出 `ResumeRequest`，并且它真的 `resume` 成功。
///
/// 注意 `start` 会冻结估时基准，把 `task.row_version` 从 0 推到 1——所以
/// 「不读任务行、填个常量 0」在这里会直接撞 `VERSION_CONFLICT`（见报告里的反向验证）。
#[test]
fn the_snapshot_task_identity_is_enough_to_resume_a_paused_session() {
    let mut c = command_harness();

    // 请求先构造出来：`c.coord` 是 `&mut` 借、`c.start_request()` 是 `&` 借，不能同处一个实参列表。
    let start = c.start_request();
    let started = c.coord.start(&mut c.db, start).unwrap();
    assert_eq!(started.snapshot.state, Some(SessionState::Running));
    assert_eq!(started.snapshot.task_id.as_deref(), Some("t1"));

    c.advance(5_000);
    let pause = c.session_request(&started.snapshot);
    let paused = c.coord.pause(&mut c.db, pause).unwrap();
    assert_eq!(paused.snapshot.state, Some(SessionState::Paused));

    // 「重开窗口」＝一次全新查询：手上只有这份快照。
    let fresh = c.coord.snapshot(&mut c.db).unwrap();
    assert_eq!(fresh.state, Some(SessionState::Paused));
    assert_eq!(fresh.task_id.as_deref(), Some("t1"));
    assert!(
        fresh.task_row_version.is_some(),
        "有会话就必须给出任务版本，否则「继续」的请求构造不出来"
    );

    let request = resume_request_of(&fresh).expect("快照必须给出可用的任务身份");
    assert_eq!(request.task_id, "t1");
    assert_eq!(request.session_id, fresh.session_id.clone().unwrap());

    let resumed = c.coord.resume(&mut c.db, request).unwrap();
    assert_eq!(resumed.snapshot.state, Some(SessionState::Running));
    assert_eq!(
        resumed.snapshot.session_id, fresh.session_id,
        "还是同一条会话"
    );
    assert_eq!(resumed.snapshot.task_id, fresh.task_id);
}

/// 任务行版本**每次采样重读**：暂停期间任务被改（版本 +1）快照要跟着变；
/// 拿旧版本 `resume` 必须 `VERSION_CONFLICT`，拿快照里那个新版本必须成功。
///
/// 前半条是防"把任务版本缓存在 `LiveSession` 里"的——那样快照会一直报 0，
/// 前端按它构造的「继续」请求永远失败。
#[test]
fn the_snapshot_task_version_tracks_task_edits_and_a_stale_one_conflicts() {
    let mut h = harness(TimerKind::Stopwatch, None);
    h.start(0);
    h.set_state(SessionState::Paused, 1);
    h.coord.load_session(h.db.connection(), "s1").unwrap();

    let before = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(before.state, Some(SessionState::Paused));
    assert_eq!(before.task_id.as_deref(), Some("t1"));
    assert_eq!(before.task_row_version, Some(0));

    // 任务被改过：P7 还没有任务改名命令，直接改行 + bump 版本，模拟同一种效果。
    h.db.connection()
        .execute(
            "UPDATE task SET title='改过的标题', row_version=row_version+1,
                              updated_at=updated_at+1
              WHERE id='t1'",
            [],
        )
        .unwrap();

    let after = h.coord.snapshot(&mut h.db).unwrap();
    assert_eq!(
        after.task_row_version,
        Some(1),
        "任务版本必须每次重读，不能缓存在内存镜像里"
    );
    assert_eq!(
        after.session_version, before.session_version,
        "改任务不动会话版本"
    );

    // 旧版本 ⇒ VERSION_CONFLICT。第一阶段的只读守卫就拒：不采样、不写库。
    let stale = ResumeRequest {
        task_expected_version: 0,
        ..resume_request_of(&before).unwrap()
    };
    let err = h.coord.resume(&mut h.db, stale).unwrap_err();
    assert_eq!(err.code(), "VERSION_CONFLICT");

    // 快照给的那个版本 ⇒ 真的能继续；失败的那次没有留下任何副作用。
    let resumed = h
        .coord
        .resume(&mut h.db, resume_request_of(&after).unwrap())
        .unwrap();
    assert_eq!(resumed.snapshot.state, Some(SessionState::Running));
    assert_eq!(resumed.snapshot.task_row_version, Some(1));
}
