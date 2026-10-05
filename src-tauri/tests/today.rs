//! P5 Task 2：Today 聚合（F-010）。
//!
//! 覆盖（Task 2 的测试清单）：
//! 五项数值分别正确且互不覆盖 · 跨日边界 `23:50–00:10` **两天各 10 分钟**（含夏令时
//! 切换日：`America/New_York` 2026-03-08 是 23 小时）· 无任何 session 时返回空结构而
//! 不是错误 · 列表顺序稳定（连续两次一致，且与 `services::daily_plan::plan_for`
//! **逐字段**一致——Ruling P5-1 的钉住用例）· 完成的任务保留在当天列表里并带状态 ·
//! `revision` 随一次业务写前进、纯读不前进 · 待确认按**条数**判有无（零长度候选与
//! 终点未知候选各一条）。
//!
//! 断言一律给出**期望数值本身**（计划 §断言口径），不用「非空」「大于 0」。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::domain::task::{TaskStatus, TransitionCause};
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::daily_plan::{self, DailyPlanQuery};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::stats::{
    self, Measure, MeasureColumn, StatsClass, StatsRange, TodayQuery, TodayView,
};
use worktrace_lib::services::tasks::TransitionTaskRequest;
use worktrace_lib::services::timer::coordinator::{Coordinator, SessionRequest, StartRequest};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::run_repo;

/// 与本进程启动同名的本次 run（手工装置与真启动装置都用它）。
const RUN: &str = "run-1";

/// 上海 2026-03-10 00:00（+08:00）= 1773072000000；今日的日界从它推。
const SH_MID: i64 = 1_773_072_000_000;
/// 上海 2026-03-10 12:00——「此刻」的固定取值。
const SH_NOON: i64 = 1_773_115_200_000;
/// 上海 2026-03-10 23:50 / 23:55。
const SH_2350: i64 = 1_773_157_800_000;
const SH_2355: i64 = 1_773_158_100_000;
/// 上海 2026-03-11 00:00（次日的日界）/ 00:05 / 00:10。
const SH_NEXT_MID: i64 = 1_773_158_400_000;
const SH_NEXT_0005: i64 = 1_773_158_700_000;
const SH_NEXT_0010: i64 = 1_773_159_000_000;
/// 一小时 / 一天的毫秒数（只有非夏令时日的**步进**才用它，日界一律由服务算）。
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;

/// 纽约 2026-03-08 00:00 EST → 2026-03-09 00:00 EDT：夏令时切换日是 **23 小时**。
const NY_MID: i64 = 1_772_946_000_000;
const NY_NEXT_MID: i64 = 1_773_028_800_000;
/// 纽约 2026-03-08 00:30 / 03:30（墙钟 3 小时，真实只有 2 小时）/ 12:00。
const NY_0030: i64 = 1_772_947_800_000;
const NY_0330: i64 = 1_772_955_000_000;
const NY_NOON: i64 = 1_772_985_600_000;
/// 纽约 2026-03-09 01:00——已经属于**次日**了。
const NY_NEXT_0100: i64 = 1_773_032_400_000;

/// 忽略全部事件的出口：统计用例不关心广播，但 `startup` 需要一个。
struct NoSink;

impl EventSink for NoSink {
    fn broadcast(&self, _envelope: &EventEnvelope) -> Result<(), String> {
        Ok(())
    }
}

/// 取 Today 里某一类的那一列。三个取列分支钉住「字段 ↔ 类别」的对应关系：
/// 换错字段会在每个用到它的用例上变红。
fn column(view: &TodayView, class: StatsClass) -> &MeasureColumn {
    match class {
        StatsClass::Confirmed => &view.confirmed_human,
        StatsClass::Live => &view.live_human,
        StatsClass::Pending => &view.pending_human,
    }
}

/// 三个数字都必须是**人工**列，且五个口径字段与视图同源（02 §6）。
fn assert_scope_fields(column: &MeasureColumn, view: &TodayView) {
    assert_eq!(column.measure, Measure::Human, "Today 的三项都是人工口径");
    assert_eq!(column.timezone, view.timezone);
    assert_eq!(column.range, view.range);
    assert_eq!(column.as_of, view.as_of);
    assert_eq!(column.data_epoch, view.data_epoch);
    assert_eq!(column.revision, view.revision);
}

/// 今日选择列表的任务 id，按视图给出的顺序。
fn task_ids(view: &TodayView) -> Vec<&str> {
    view.tasks.iter().map(|task| task.id.as_str()).collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// 装置一：手工造区间事实（形状可控）+ 真协调器（活会话走真 `start`）
// ─────────────────────────────────────────────────────────────────────────────

struct H {
    _dir: tempfile::TempDir,
    db: Db,
    coord: Coordinator,
    clock: Arc<Mutex<FakeClock>>,
    epoch: String,
}

fn setup(wall: i64) -> H {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let meta = init_meta(&tx).unwrap();
    run_repo::start_run(&tx, RUN, wall).unwrap();
    tx.commit().unwrap();

    // 时钟与协调器共享：`elapse` 要能推进它，`attributed_end` 才会跟着走。
    let clock = Arc::new(Mutex::new(FakeClock::new(wall, 0)));
    let coord = Coordinator::new(Box::new(Arc::clone(&clock)), RUN);
    H {
        _dir: dir,
        db,
        coord,
        clock,
        epoch: meta.data_epoch,
    }
}

impl H {
    fn task(&self, id: &str, title: &str, created_at: i64) {
        self.db
            .connection()
            .execute(
                "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
                 VALUES(?1,?2,'Doing',0,?3,?3)",
                rusqlite::params![id, title, created_at],
            )
            .unwrap();
    }

    fn session(
        &self,
        id: &str,
        task_id: &str,
        mode: &str,
        state: &str,
        needs_review: i64,
        started_at: i64,
    ) {
        self.db
            .connection()
            .execute(
                "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,
                                          needs_review,row_version)
                 VALUES(?1,?2,?3,?4,?5,'stopwatch',?6,?7,0)",
                rusqlite::params![id, task_id, RUN, mode, state, started_at, needs_review],
            )
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn interval(
        &self,
        id: &str,
        session_id: &str,
        started_at: i64,
        ended_at: Option<i64>,
        duration_ms: Option<i64>,
        needs_review: i64,
        voided_at: Option<i64>,
    ) {
        self.db
            .connection()
            .execute(
                "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms,
                                           needs_review,voided_at)
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

    /// 把任务加入某一天的今日计划（走 P4 的生产写入口，不手写 SQL）。
    fn plan(&mut self, task_id: &str, date: &str, timezone: &str) {
        let env = WriteEnvelope::for_create(self.epoch.clone());
        daily_plan::add_to_plan(&mut self.db, env, task_id, date, timezone, SH_NOON).unwrap();
    }

    /// 真起一次前台计时（协调器的内存镜像与库行一起建立）。
    fn start(&mut self, task_id: &str) {
        let req = StartRequest {
            expected_data_epoch: self.epoch.clone(),
            task_id: task_id.to_string(),
            task_expected_version: 0,
            mode: SessionMode::Foreground,
            timer_kind: TimerKind::Stopwatch,
            target_duration_ms: None,
            expected_interval_ms: 30_000,
        };
        self.coord.start(&mut self.db, req).unwrap();
    }

    /// 推进一段真实流逝：**60 秒一片、每片一次真采样**。一次跳满会把
    /// `d_monotonic > 3 × 期望采样间隔` 判成 `Suspended`（08 §1 的长间隔），
    /// 那是异常分割路径，不是本文件要观察的「今天有多少」。
    fn elapse(&mut self, total_ms: i64) {
        let mut left = total_ms;
        while left > 0 {
            let step = left.min(60_000);
            self.clock.lock().unwrap().advance_both(step);
            self.coord.stats_sample(&mut self.db).unwrap();
            left -= step;
        }
    }

    /// 与生产入口同一条取数路径：一次样本 + 一次 Today 聚合。
    fn today(&mut self, timezone: &str) -> Result<TodayView, AppError> {
        let query = TodayQuery {
            timezone: timezone.to_string(),
            expected_data_epoch: self.epoch.clone(),
        };
        let sample = self.coord.stats_sample(&mut self.db)?;
        stats::today(&self.db, sample, &query)
    }

    fn revision(&self) -> i64 {
        require_meta(self.db.connection()).unwrap().revision
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ①–⑤ 五项分别显示、不预先相加
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_five_items_are_reported_separately_and_never_pre_summed() {
    let mut h = setup(SH_NOON);
    // ① 今日选择列表：两个任务。
    h.task("t-a", "写文档", 1_000);
    h.task("t-b", "改代码", 2_000);
    h.plan("t-a", "2026-03-10", "Asia/Shanghai");
    h.plan("t-b", "2026-03-10", "Asia/Shanghai");

    // ③ 已确认人工：今天 09:00–10:00 一条闭合的可信前台区间 = 1 小时。
    h.session(
        "s-done",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        SH_NOON - 3 * HOUR,
    );
    h.interval(
        "i-done",
        "s-done",
        SH_NOON - 3 * HOUR,
        Some(SH_NOON - 2 * HOUR),
        Some(HOUR),
        0,
        None,
    );

    // ⑤ 待确认：今天 11:00–11:10 的候选（recovering 会话、未作废、端点已知）= 10 分钟。
    h.session(
        "s-rec",
        "t-a",
        "FOREGROUND",
        "recovering",
        1,
        SH_NOON - HOUR,
    );
    h.interval(
        "i-pend",
        "s-rec",
        SH_NOON - HOUR,
        Some(SH_NOON - 3_000_000),
        Some(600_000),
        1,
        None,
    );

    // ② 当前任务 + ④ 运行暂计：真起一次计时，样本走过 5 分钟。
    h.task("t-live", "现在做的", 3_000);
    h.start("t-live");
    h.elapse(300_000);

    let view = h.today("Asia/Shanghai").unwrap();

    // ① 列表
    assert_eq!(task_ids(&view), ["t-a", "t-b"]);
    // ② 当前任务与运行状态（取自同一次样本）
    let current = view.current.as_ref().expect("此刻有一条运行中的会话");
    assert_eq!(current.task_id, "t-live");
    assert_eq!(current.task_title, "现在做的");
    assert_eq!(current.state, SessionState::Running);

    // ③④⑤ 三项分别显示，各自等于自己那一段，**任何一个都不是三项之和**
    //    （1h + 5m + 10m = 4_500_000 从未出现）。
    assert_eq!(
        column(&view, StatsClass::Confirmed).ms,
        Some(HOUR),
        "已确认人工工时 = 那一条 09:00–10:00 的 1 小时"
    );
    assert_eq!(column(&view, StatsClass::Confirmed).intervals, 1);
    assert_eq!(
        column(&view, StatsClass::Live).ms,
        Some(300_000),
        "运行暂计 = 这条开放区间裁剪到今天的 5 分钟"
    );
    assert_eq!(column(&view, StatsClass::Live).intervals, 1);
    assert_eq!(
        column(&view, StatsClass::Pending).ms,
        Some(600_000),
        "待确认时间 = 那条候选端点之间裁剪到今天的 10 分钟"
    );
    assert_eq!(column(&view, StatsClass::Pending).intervals, 1);

    // 口径字段：五个都要有，且与视图同源
    for class in [StatsClass::Confirmed, StatsClass::Live, StatsClass::Pending] {
        let col = column(&view, class);
        assert_eq!(col.class, class);
        assert_scope_fields(col, &view);
    }

    // 视图自身的口径：日期、时区、真实日界、此刻、库身份与业务版本
    assert_eq!(view.date, "2026-03-10");
    assert_eq!(view.timezone, "Asia/Shanghai");
    assert_eq!(
        view.range,
        StatsRange {
            from: SH_MID,
            to: SH_MID + DAY
        }
    );
    assert_eq!(view.as_of, SH_NOON + 300_000);
    assert_eq!(view.revision, h.revision());
    assert_eq!(view.data_epoch, h.epoch);
}

// ─────────────────────────────────────────────────────────────────────────────
// 跨日边界
// ─────────────────────────────────────────────────────────────────────────────

/// `23:50–00:10` 的一条无暂停闭合区间：两天各 10 分钟。
fn cross_midnight_rows(h: &H) {
    h.task("t-x", "跨午夜", 1_000);
    h.session("s-x", "t-x", "FOREGROUND", "finished", 0, SH_2350);
    h.interval(
        "i-x",
        "s-x",
        SH_2350,
        Some(SH_NEXT_0010),
        Some(1_200_000),
        0,
        None,
    );
}

#[test]
fn a_session_across_midnight_is_ten_minutes_on_each_local_day() {
    // 此刻还在 23:55：今天 = 03-10，只有 23:50–24:00 这 10 分钟。
    let mut before = setup(SH_2355);
    cross_midnight_rows(&before);
    let day1 = before.today("Asia/Shanghai").unwrap();
    assert_eq!(day1.date, "2026-03-10");
    assert_eq!(
        day1.range,
        StatsRange {
            from: SH_MID,
            to: SH_NEXT_MID
        }
    );
    assert_eq!(
        column(&day1, StatsClass::Confirmed).ms,
        Some(600_000),
        "第一天 23:50–24:00 是 10 分钟"
    );

    // 此刻已过午夜 00:05：今天 = 03-11，只有 00:00–00:10 这 10 分钟。
    // 同一条区间**不被任一天丢掉**，也不被两天各算成 20 分钟。
    let mut after = setup(SH_NEXT_0005);
    cross_midnight_rows(&after);
    let day2 = after.today("Asia/Shanghai").unwrap();
    assert_eq!(day2.date, "2026-03-11");
    assert_eq!(
        day2.range,
        StatsRange {
            from: SH_NEXT_MID,
            to: SH_NEXT_MID + DAY
        }
    );
    assert_eq!(
        column(&day2, StatsClass::Confirmed).ms,
        Some(600_000),
        "第二天 00:00–00:10 是 10 分钟"
    );
}

#[test]
fn a_dst_transition_day_is_twenty_three_hours_and_keeps_its_own_bounds() {
    let mut h = setup(NY_NOON);
    h.task("t-dst", "夏令时", 1_000);
    h.session("s-dst", "t-dst", "FOREGROUND", "finished", 0, NY_0030);
    // 本地 00:30→03:30：墙钟跨 3 小时，但 02:00→03:00 那一小时不存在 ⇒ 真实 2 小时。
    h.interval(
        "i-dst",
        "s-dst",
        NY_0030,
        Some(NY_0330),
        Some(2 * HOUR),
        0,
        None,
    );
    // 次日凌晨的一段不属于今天。
    h.interval(
        "i-next",
        "s-dst",
        NY_NEXT_0100,
        Some(NY_NEXT_0100 + 600_000),
        Some(600_000),
        0,
        None,
    );

    let view = h.today("America/New_York").unwrap();
    assert_eq!(view.date, "2026-03-08");
    assert_eq!(
        view.range,
        StatsRange {
            from: NY_MID,
            to: NY_NEXT_MID
        },
        "日界由「次日零点」换算，不是 start + 24h"
    );
    assert_eq!(
        view.range.to - view.range.from,
        23 * HOUR,
        "夏令时切换日只有 23 小时"
    );
    assert_eq!(
        column(&view, StatsClass::Confirmed).ms,
        Some(2 * HOUR),
        "00:30→03:30 是真实 2 小时；次日那一段不算进今天"
    );
    assert_eq!(column(&view, StatsClass::Confirmed).intervals, 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// 待确认：按**条数**判有无（不是 `ms.is_some()`）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_zero_length_pending_candidate_counts_as_one_interval_with_zero_ms() {
    let mut h = setup(SH_NOON);
    h.task("t-zero", "零长度候选", 1_000);
    h.session("s-zero", "t-zero", "FOREGROUND", "recovering", 1, SH_NOON);
    // 只有 `[t,t)` 一条候选（P3 的 S5 归一在有可信前缀时会写这种形状）。
    h.interval("i-zero", "s-zero", SH_NOON, Some(SH_NOON), Some(0), 1, None);

    let view = h.today("Asia/Shanghai").unwrap();
    let pending = column(&view, StatsClass::Pending);
    assert_eq!(
        pending.intervals, 1,
        "零长度候选也是一条待确认记录（按条数判有无）"
    );
    assert_eq!(pending.ms, Some(0), "它的跨度是 0 毫秒，不是「没有」");
    assert_eq!(column(&view, StatsClass::Confirmed).ms, Some(0));
    assert_eq!(column(&view, StatsClass::Live).ms, Some(0));
}

#[test]
fn a_pending_candidate_without_a_known_endpoint_is_counted_but_gives_no_milliseconds() {
    let mut h = setup(SH_NOON);
    h.task("t-unknown", "终点未知", 1_000);
    h.session(
        "s-unknown",
        "t-unknown",
        "FOREGROUND",
        "recovering",
        1,
        SH_NOON - HOUR,
    );
    // 没有可信检查点的整段待确认：终点未知、时长未知（08 §1）。
    // 它**必须**以「1 条」出现在待确认栏里——`ms.is_some()` 会说「没有待确认」，
    // 那正是本条用例要挡住的误判（Ruling P5-12 的携带项）。
    h.interval(
        "i-unknown",
        "s-unknown",
        SH_NOON - HOUR,
        None,
        None,
        1,
        None,
    );

    let view = h.today("Asia/Shanghai").unwrap();
    let pending = column(&view, StatsClass::Pending);
    assert_eq!(pending.intervals, 1, "终点未知的候选照样是一条待确认记录");
    assert_eq!(pending.ms, None, "终点未知 ⇒ 不推算毫秒（02 §4）");
}

// ─────────────────────────────────────────────────────────────────────────────
// 装置二：真启动 + 真 AppState（覆盖 `AppState::stats_today` 这条生产入口）
// ─────────────────────────────────────────────────────────────────────────────

struct AppFixture {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    lock_path: PathBuf,
}

/// 建一个装了给定任务的库（`startup` 之前建好，任务行的 `created_at` 由用例定）。
fn app_fixture(tasks: &[(&str, &str, i64)]) -> AppFixture {
    let dir = tempfile::tempdir().unwrap();
    let fixture = AppFixture {
        db_path: dir.path().join("worktrace.db"),
        lock_path: dir.path().join("instance.lock"),
        _dir: dir,
    };
    let db = Db::open(&fixture.db_path).unwrap();
    migrate(db.connection()).unwrap();
    for (id, title, created_at) in tasks {
        db.connection()
            .execute(
                "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
                 VALUES(?1,?2,'Doing',0,?3,?3)",
                rusqlite::params![id, title, created_at],
            )
            .unwrap();
    }
    drop(db);
    fixture
}

/// 真跑一次启动；采样节拍放到 1 分钟，避免周期线程干扰用例。
fn app_started(fixture: &AppFixture) -> Box<RunningApp> {
    let config = StartupConfig {
        db_path: fixture.db_path.clone(),
        lock_path: fixture.lock_path.clone(),
        sampling_interval_ms: 60_000,
    };
    let clock = Arc::new(Mutex::new(FakeClock::new(SH_NOON, 0)));
    let outcome = startup(config, Box::new(clock), Arc::new(NoSink), &NoProbe, &|| {
        Ok(())
    })
    .expect("启动应当成功");
    match outcome {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    }
}

fn query(epoch: &str, timezone: &str) -> TodayQuery {
    TodayQuery {
        timezone: timezone.to_string(),
        expected_data_epoch: epoch.to_string(),
    }
}

/// 这条连接累计写过的行数（只读断言的判据，与 `tests/recovery_scan.rs` 同一写法）。
fn total_changes(db: &Db) -> i64 {
    db.connection()
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn an_app_without_any_session_returns_an_empty_view_instead_of_an_error() {
    let fixture = app_fixture(&[]);
    let running = app_started(&fixture);
    let epoch = running.data_epoch().to_string();
    let app = Arc::clone(running.app());

    let before = {
        let guard = lock_app(&app);
        total_changes(guard.db())
    };

    let view = {
        let mut guard = lock_app(&app);
        guard
            .stats_today(&query(&epoch, "utc"))
            .expect("没有任何 session 也必须是一次成功的读")
    };

    // 五项都在，只是都是「空」——不是错误、不是空白。
    assert!(view.tasks.is_empty(), "今天没选任何任务");
    assert!(view.current.is_none(), "没有活动会话");
    assert_eq!(
        column(&view, StatsClass::Confirmed).ms,
        Some(0),
        "空库的已确认人工工时是 0，不是缺失"
    );
    assert_eq!(column(&view, StatsClass::Confirmed).intervals, 0);
    assert_eq!(column(&view, StatsClass::Live).ms, Some(0));
    assert_eq!(column(&view, StatsClass::Live).intervals, 0);
    assert_eq!(
        column(&view, StatsClass::Pending).intervals,
        0,
        "有没有待确认按条数判"
    );
    assert_eq!(
        column(&view, StatsClass::Pending).ms,
        None,
        "一条候选都没有 ⇒ 不给毫秒（零长度候选那条路径见另一个用例）"
    );
    // 口径字段一个不少：时区过归一入口（小写 utc → UTC），日期按该时区算。
    assert_eq!(view.timezone, "UTC");
    assert_eq!(view.date, "2026-03-10");
    assert_eq!(
        view.range,
        StatsRange {
            from: 1_773_100_800_000,
            to: 1_773_187_200_000
        },
        "UTC 的 2026-03-10 真实日界"
    );
    assert_eq!(view.as_of, SH_NOON);
    assert_eq!(view.data_epoch, epoch);
    assert_eq!(view.revision, 0, "空库的业务版本是 0");

    // **只读**：一次 Today 不多写任何一行（也就不会凭空推 revision）。
    let after = {
        let guard = lock_app(&app);
        total_changes(guard.db())
    };
    assert_eq!(after, before, "Today 是只读查询：不写库、不加 revision");
}

#[test]
fn the_today_list_matches_plan_for_and_keeps_completed_tasks() {
    // 三个任务 created_at 相同：顺序只能由 id 决定（P4 的 `task.created_at, task.id`）。
    let fixture = app_fixture(&[
        ("t-c", "丙", 5_000),
        ("t-b", "乙", 5_000),
        ("t-a", "甲", 5_000),
        ("t-out", "别的日子", 5_000),
    ]);
    let running = app_started(&fixture);
    let epoch = running.data_epoch().to_string();
    let app = Arc::clone(running.app());

    for id in ["t-c", "t-b", "t-a"] {
        let mut guard = lock_app(&app);
        let now = guard.now_ms().unwrap();
        let env = WriteEnvelope::for_create(epoch.clone());
        daily_plan::add_to_plan(guard.db_mut(), env, id, "2026-03-10", "Asia/Shanghai", now)
            .unwrap();
    }

    let first = {
        let mut guard = lock_app(&app);
        guard.stats_today(&query(&epoch, "Asia/Shanghai")).unwrap()
    };
    assert_eq!(task_ids(&first), ["t-a", "t-b", "t-c"]);
    assert_eq!(first.date, "2026-03-10");

    // G1 的钉住用例：Today 的今日选择列表与 P4 的读入口**逐字段一致**（含顺序）。
    let plan = {
        let guard = lock_app(&app);
        daily_plan::plan_for(
            guard.db(),
            DailyPlanQuery {
                date: "2026-03-10".to_string(),
                timezone: "Asia/Shanghai".to_string(),
                expected_data_epoch: epoch.clone(),
            },
        )
        .unwrap()
    };
    assert_eq!(
        first.tasks, plan.tasks,
        "Today 的列表必须与 `services::daily_plan::plan_for` 一致（含顺序）"
    );
    assert_eq!(first.data_epoch, plan.data_epoch);
    assert_eq!(first.revision, plan.revision);

    // 连续两次调用：顺序与内容都稳定。
    let second = {
        let mut guard = lock_app(&app);
        guard.stats_today(&query(&epoch, "Asia/Shanghai")).unwrap()
    };
    assert_eq!(first.tasks, second.tasks, "连续两次调用的列表必须一致");

    // 完成的任务**保留**在当天列表里并带状态（P4 的约定）。
    {
        let mut guard = lock_app(&app);
        let env = WriteEnvelope::for_update(epoch.clone(), 0);
        guard
            .transition_task(
                env,
                TransitionTaskRequest {
                    task_id: "t-b".to_string(),
                    target: TaskStatus::Done,
                    cause: TransitionCause::User,
                },
            )
            .unwrap();
    }
    let after = {
        let mut guard = lock_app(&app);
        guard.stats_today(&query(&epoch, "Asia/Shanghai")).unwrap()
    };
    assert_eq!(
        task_ids(&after),
        ["t-a", "t-b", "t-c"],
        "完成的任务不因完成而消失，位置也不动"
    );
    assert_eq!(after.tasks[1].status, TaskStatus::Done);
    assert_eq!(after.tasks[1].title, "乙");
    assert!(
        !task_ids(&after).contains(&"t-out"),
        "不在今天计划里的任务不得出现"
    );
}

#[test]
fn a_pure_read_does_not_advance_revision_but_a_business_write_does() {
    let fixture = app_fixture(&[("t-1", "任务", 1_000)]);
    let running = app_started(&fixture);
    let epoch = running.data_epoch().to_string();
    let app = Arc::clone(running.app());

    let before = {
        let mut guard = lock_app(&app);
        guard.stats_today(&query(&epoch, "Asia/Shanghai")).unwrap()
    };
    let again = {
        let mut guard = lock_app(&app);
        guard.stats_today(&query(&epoch, "Asia/Shanghai")).unwrap()
    };
    assert_eq!(
        again.revision, before.revision,
        "纯读不前进：连续两次 Today 的 revision 相同"
    );
    assert_eq!(again.tasks, before.tasks);
    assert_eq!(again.confirmed_human, before.confirmed_human);

    // 一次业务写：把任务选进今天（恰好一次 revision）。
    {
        let mut guard = lock_app(&app);
        let now = guard.now_ms().unwrap();
        let env = WriteEnvelope::for_create(epoch.clone());
        daily_plan::add_to_plan(
            guard.db_mut(),
            env,
            "t-1",
            "2026-03-10",
            "Asia/Shanghai",
            now,
        )
        .unwrap();
    }

    let after = {
        let mut guard = lock_app(&app);
        guard.stats_today(&query(&epoch, "Asia/Shanghai")).unwrap()
    };
    assert_eq!(
        after.revision,
        before.revision + 1,
        "一次业务写恰好让 revision 前进 1"
    );
    assert_eq!(task_ids(&after), ["t-1"], "写之后今天的列表里多了这个任务");
    // 信封里的 `revision` 会随业务写前进，所以这里比的是**数字**，不是整个列。
    assert_eq!(
        after.confirmed_human.ms, before.confirmed_human.ms,
        "选进今天不是工时事实：三项数字不变"
    );
    assert_eq!(after.live_human.ms, before.live_human.ms);
    assert_eq!(after.pending_human.ms, before.pending_human.ms);
}

#[test]
fn the_current_task_and_run_state_come_from_the_same_sample() {
    let fixture = app_fixture(&[("t-1", "正在做的任务", 1_000)]);
    let running = app_started(&fixture);
    let epoch = running.data_epoch().to_string();
    let app = Arc::clone(running.app());

    {
        let mut guard = lock_app(&app);
        let task = guard
            .db()
            .connection()
            .query_row("SELECT row_version FROM task WHERE id = 't-1'", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        guard
            .start(StartRequest {
                expected_data_epoch: epoch.clone(),
                task_id: "t-1".to_string(),
                task_expected_version: task,
                mode: SessionMode::Foreground,
                timer_kind: TimerKind::Stopwatch,
                target_duration_ms: None,
                expected_interval_ms: 30_000,
            })
            .unwrap();
    }

    let running_view = {
        let mut guard = lock_app(&app);
        guard.stats_today(&query(&epoch, "Asia/Shanghai")).unwrap()
    };
    let current = running_view.current.as_ref().expect("真起了一次计时");
    assert_eq!(current.task_id, "t-1");
    assert_eq!(current.task_title, "正在做的任务");
    assert_eq!(current.state, SessionState::Running);
    assert_eq!(
        column(&running_view, StatsClass::Live).intervals,
        1,
        "开放区间算一条运行暂计"
    );
    assert_eq!(
        column(&running_view, StatsClass::Live).ms,
        Some(0),
        "时钟还没走：暂计是 0 毫秒"
    );

    // 暂停之后：**没有开放区间**，但当前任务与「已暂停」这个状态照样要报出来
    // （所以当前任务不能从开放区间推）。
    {
        let mut guard = lock_app(&app);
        let snapshot = guard.snapshot().unwrap();
        guard
            .pause(SessionRequest {
                expected_data_epoch: epoch.clone(),
                session_id: snapshot.session_id.clone().unwrap(),
                session_expected_version: snapshot.session_version.unwrap(),
            })
            .unwrap();
    }
    let paused_view = {
        let mut guard = lock_app(&app);
        guard.stats_today(&query(&epoch, "Asia/Shanghai")).unwrap()
    };
    let current = paused_view
        .current
        .as_ref()
        .expect("暂停的会话仍是当前会话");
    assert_eq!(current.task_id, "t-1");
    assert_eq!(current.state, SessionState::Paused);
    assert_eq!(
        column(&paused_view, StatsClass::Live).ms,
        Some(0),
        "没有开放区间就没有运行暂计"
    );
    assert_eq!(column(&paused_view, StatsClass::Live).intervals, 0);
}
