//! P5 Task 1：统计口径（02 §6）——半开范围裁剪、排除口径、人工/机器分离、日界分桶。
//!
//! 覆盖（Task 1 的测试清单）：
//! 半开端点相接不重叠 · 范围完全在区间内/外/部分相交的边界 · 空范围合法 ·
//! 跨日 `23:50–00:10` **两天各 10 分钟** · `needs_review`/`voided_at`/`discarded`
//! 被排除且已作废的不显示为待确认 · 1h 前台 + 1h 后台 → 人工 1h、机器 1h ·
//! `WAITING` 单列不并入任何一项 · 损坏会话的区间不计入已确认 ·
//! DTO 五字段齐全且 `revision` 与快照一致 · 统计取样与 pause/resume/finish 并发 ·
//! 异常分割后前缀计入已确认、余段单列待确认 · 跨日分桶每日之和 == 总和 ·
//! 端点落在日界不产生零长度段 · 不同时区归属日期不同但总和相同。
//!
//! 断言一律给出**期望数值本身**（计划 §断言口径），不用「非空」「大于 0」。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::stats::{
    Measure, MeasureColumn, RangeReport, StatsClass, StatsRangeQuery, StatsSnapshot,
};
use worktrace_lib::services::timer::coordinator::{
    Coordinator, ResumeRequest, SessionRequest, StartRequest,
};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::run_repo;

/// 原始事实装置用的挂钟起点（`FakeClock` 不自己走）。
const WALL: i64 = 1_700_000_000_000;
/// 上海 2026-03-10 00:00（+08:00）= 2026-03-09T16:00Z 的 Unix 毫秒；跨日与日界用例都从它推。
const SH_MID: i64 = 1_773_072_000_000;
/// 与应用启动同名的本次 run。
const RUN: &str = "run-1";

/// 忽略全部事件的出口：统计用例不关心广播，但 `startup` 需要一个。
struct NoSink;

impl EventSink for NoSink {
    fn broadcast(&self, _envelope: &EventEnvelope) -> Result<(), String> {
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 装置一：手工造区间事实（形状可控），协调器**没有**活动会话
// ─────────────────────────────────────────────────────────────────────────────

struct H {
    _dir: tempfile::TempDir,
    db: Db,
    coord: Coordinator,
    epoch: String,
}

fn setup() -> H {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let meta = init_meta(&tx).unwrap();
    run_repo::start_run(&tx, RUN, WALL).unwrap();
    tx.execute(
        "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
         VALUES('t1','任务','Doing',0,1000,1000)",
        [],
    )
    .unwrap();
    tx.commit().unwrap();

    let coord = Coordinator::new(Box::new(FakeClock::new(WALL, 0)), RUN);
    H {
        _dir: dir,
        db,
        coord,
        epoch: meta.data_epoch,
    }
}

impl H {
    fn session(&self, id: &str, mode: &str, state: &str, needs_review: i64) {
        self.db
            .connection()
            .execute(
                "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,
                                          needs_review,row_version)
                 VALUES(?1,'t1',?2,?3,?4,'stopwatch',?5,?6,0)",
                rusqlite::params![id, RUN, mode, state, WALL, needs_review],
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

    fn query(&self, from: i64, to: i64, timezone: &str) -> StatsRangeQuery {
        StatsRangeQuery {
            from,
            to,
            timezone: timezone.to_string(),
            expected_data_epoch: self.epoch.clone(),
        }
    }

    /// 真取一次样本 + 一次一致读：与生产入口同一条路径。
    fn snapshot(&mut self, from: i64, to: i64, timezone: &str) -> StatsSnapshot {
        let query = self.query(from, to, timezone);
        let sample = self.coord.stats_sample(&mut self.db).unwrap();
        worktrace_lib::services::stats::snapshot(&self.db, sample, &query).unwrap()
    }

    fn report(&mut self, from: i64, to: i64, timezone: &str) -> RangeReport {
        self.snapshot(from, to, timezone).report().unwrap()
    }

    fn revision(&self) -> i64 {
        require_meta(self.db.connection()).unwrap().revision
    }
}

/// 取某一类某一 measure 的毫秒数（形状固定，取不到就是用例写错了）。
fn ms(report: &RangeReport, class: StatsClass, measure: Measure) -> Option<i64> {
    report.column(class, measure).ms
}

/// 取某一天某一 measure 的已确认毫秒数；这一天不在分桶里就是 `None`。
fn day_ms(report: &RangeReport, date: &str, measure: Measure) -> Option<i64> {
    report
        .days
        .iter()
        .find(|day| day.date == date)
        .map(|day| day.column(measure).ms.expect("日桶只有已确认列"))
}

/// 某个 measure 在全部日桶上的和。
fn days_sum(report: &RangeReport, measure: Measure) -> i64 {
    report
        .days
        .iter()
        .map(|day| day.column(measure).ms.unwrap())
        .sum()
}

// ─────────────────────────────────────────────────────────────────────────────
// 半开裁剪
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_touching_endpoint_belongs_to_exactly_one_of_two_half_open_intervals() {
    let mut h = setup();
    h.session("s-fg", "FOREGROUND", "finished", 0);
    // 两段各 1 小时，端点相接：`[T-2h, T-1h)` 与 `[T-1h, T)`。
    h.interval(
        "i-left",
        "s-fg",
        SH_MID - 7_200_000,
        Some(SH_MID - 3_600_000),
        Some(3_600_000),
        0,
        None,
    );
    h.interval(
        "i-right",
        "s-fg",
        SH_MID - 3_600_000,
        Some(SH_MID),
        Some(3_600_000),
        0,
        None,
    );

    let left = h.report(SH_MID - 7_200_000, SH_MID - 3_600_000, "UTC");
    assert_eq!(
        ms(&left, StatsClass::Confirmed, Measure::Human),
        Some(3_600_000),
        "左段自己的范围里只有左段的 1 小时"
    );
    let right = h.report(SH_MID - 3_600_000, SH_MID, "UTC");
    assert_eq!(
        ms(&right, StatsClass::Confirmed, Measure::Human),
        Some(3_600_000),
        "右段自己的范围里只有右段的 1 小时"
    );

    let both = h.report(SH_MID - 7_200_000, SH_MID, "UTC");
    assert_eq!(
        ms(&both, StatsClass::Confirmed, Measure::Human),
        Some(7_200_000),
        "两段一起 = 2 小时；端点相接不重叠，不能算出 2 小时以外的数"
    );
    assert_eq!(
        both.column(StatsClass::Confirmed, Measure::Human).intervals,
        2
    );

    // 骑在相接点上的 2 毫秒：两段各贡献 1 毫秒，既不多也不少。
    let seam = h.report(SH_MID - 3_600_001, SH_MID - 3_599_999, "UTC");
    assert_eq!(
        ms(&seam, StatsClass::Confirmed, Measure::Human),
        Some(2),
        "交界处 1 毫秒归左段、1 毫秒归右段"
    );
    assert_eq!(
        seam.column(StatsClass::Confirmed, Measure::Human).intervals,
        2
    );
}

#[test]
fn clipping_covers_inside_outside_partial_and_empty_ranges() {
    let mut h = setup();
    h.session("s-fg", "FOREGROUND", "finished", 0);
    let a = SH_MID;
    let b = SH_MID + 3_600_000;
    h.interval("i-hour", "s-fg", a, Some(b), Some(3_600_000), 0, None);

    // 范围完全在区间**内**：`[a+10m, b-10m)` 是 40 分钟。
    let inside = h.report(a + 600_000, b - 600_000, "UTC");
    assert_eq!(
        ms(&inside, StatsClass::Confirmed, Measure::Human),
        Some(2_400_000)
    );
    assert_eq!(inside.days.len(), 1, "这一小时落在同一个本地日里");
    assert_eq!(
        inside.days[0].column(Measure::Human).range.from,
        a + 600_000,
        "日桶的范围是「该日真实日界 ∩ 报表范围」"
    );
    assert_eq!(inside.days[0].column(Measure::Human).range.to, b - 600_000);

    // 范围完全在区间**外**（前 / 后）：0 毫秒、0 条区间、四个 measure 全 0。
    for (from, to) in [(a - 3_600_000, a), (b, b + 3_600_000)] {
        let outside = h.report(from, to, "UTC");
        for measure in Measure::ALL {
            assert_eq!(
                ms(&outside, StatsClass::Confirmed, measure),
                Some(0),
                "范围外的 {measure:?} 必须是 0"
            );
        }
        assert_eq!(
            outside
                .column(StatsClass::Confirmed, Measure::Human)
                .intervals,
            0
        );
        // 日桶按**查询范围**逐日给出（这一版的口径）：范围里没有任何区间时，
        // 桶仍在，但每一项都是 0——不丢天，也不凭空多出时长。
        assert!(!outside.days.is_empty());
        for day in &outside.days {
            for measure in Measure::ALL {
                assert_eq!(day.column(measure).ms, Some(0), "空桶的每一项都是 0");
            }
        }
    }

    // **部分相交**：左半边、右半边，各 30 分钟。
    let left = h.report(a - 1_800_000, a + 1_800_000, "UTC");
    assert_eq!(
        ms(&left, StatsClass::Confirmed, Measure::Human),
        Some(1_800_000)
    );
    let right = h.report(b - 1_800_000, b + 1_800_000, "UTC");
    assert_eq!(
        ms(&right, StatsClass::Confirmed, Measure::Human),
        Some(1_800_000)
    );

    // **端点相接**：`[b, b+1)` 与 `[a-1, a)` 都是 0。
    for (from, to) in [(b, b + 1), (a - 1, a)] {
        let touching = h.report(from, to, "UTC");
        assert_eq!(
            ms(&touching, StatsClass::Confirmed, Measure::Human),
            Some(0),
            "端点相接不算相交"
        );
    }

    // **空范围**（`from == to`）是合法的：0 毫秒、0 个日桶，不 panic。
    let empty = h.report(a + 10, a + 10, "UTC");
    assert_eq!(ms(&empty, StatsClass::Confirmed, Measure::Human), Some(0));
    assert_eq!(ms(&empty, StatsClass::Live, Measure::Human), Some(0));
    assert_eq!(
        empty.column(StatsClass::Pending, Measure::Human).intervals,
        0
    );
    assert!(empty.days.is_empty());
    assert_eq!(empty.range.from, a + 10);
    assert_eq!(empty.range.to, a + 10);
}

// ─────────────────────────────────────────────────────────────────────────────
// 跨日与日界分桶
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_session_running_from_2350_to_0010_counts_ten_minutes_on_each_day() {
    let mut h = setup();
    h.session("s-fg", "FOREGROUND", "finished", 0);
    // 本地 23:50 → 次日 00:10，无暂停，共 20 分钟。
    h.interval(
        "i-night",
        "s-fg",
        SH_MID - 600_000,
        Some(SH_MID + 600_000),
        Some(1_200_000),
        0,
        None,
    );

    let report = h.report(SH_MID - 86_400_000, SH_MID + 86_400_000, "Asia/Shanghai");
    assert_eq!(
        ms(&report, StatsClass::Confirmed, Measure::Human),
        Some(1_200_000),
        "跨午夜不算丢：两段之和 = 原区间 20 分钟"
    );
    assert_eq!(
        day_ms(&report, "2026-03-09", Measure::Human),
        Some(600_000),
        "午夜前 10 分钟归 03-09"
    );
    assert_eq!(
        day_ms(&report, "2026-03-10", Measure::Human),
        Some(600_000),
        "午夜后 10 分钟归 03-10（F-010 的验收项）"
    );
}

#[test]
fn per_day_buckets_sum_to_the_total_and_a_boundary_end_does_not_leak_into_the_next_day() {
    let mut h = setup();
    h.session("s-fg", "FOREGROUND", "finished", 0);
    h.session("s-bg", "BACKGROUND", "finished", 0);
    // A：本地 03-09 22:00 → 03-10 00:00，**终点正好落在日界上**，2 小时。
    h.interval(
        "i-to-midnight",
        "s-fg",
        SH_MID - 7_200_000,
        Some(SH_MID),
        Some(7_200_000),
        0,
        None,
    );
    // B：本地 03-09 23:50 → 03-10 00:10，跨日 20 分钟（机器时间）。
    h.interval(
        "i-crossing",
        "s-bg",
        SH_MID - 600_000,
        Some(SH_MID + 600_000),
        Some(1_200_000),
        0,
        None,
    );

    let report = h.report(SH_MID - 86_400_000, SH_MID + 86_400_000, "Asia/Shanghai");

    // 分桶不丢不重：每日之和 == 不分组的总和。
    assert_eq!(days_sum(&report, Measure::Human), 7_200_000);
    assert_eq!(
        days_sum(&report, Measure::Human),
        ms(&report, StatsClass::Confirmed, Measure::Human).unwrap()
    );
    assert_eq!(days_sum(&report, Measure::MachineBackground), 1_200_000);
    assert_eq!(
        days_sum(&report, Measure::MachineBackground),
        ms(&report, StatsClass::Confirmed, Measure::MachineBackground).unwrap()
    );

    // 同一区间跨日 ⇒ 拆成两段，两段之和 == 原区间时长（20 分钟）。
    assert_eq!(
        day_ms(&report, "2026-03-09", Measure::MachineBackground),
        Some(600_000)
    );
    assert_eq!(
        day_ms(&report, "2026-03-10", Measure::MachineBackground),
        Some(600_000)
    );

    // 端点落在日界上不产生零长度段：A 全部落在 03-09，03-10 拿到 0。
    assert_eq!(
        day_ms(&report, "2026-03-09", Measure::Human),
        Some(7_200_000)
    );
    assert_eq!(day_ms(&report, "2026-03-10", Measure::Human), Some(0));
    let days_with_human = report
        .days
        .iter()
        .filter(|day| day.column(Measure::Human).ms.unwrap() > 0)
        .count();
    assert_eq!(days_with_human, 1, "只有 03-09 拿到人工时间，没有零长度段");
}

#[test]
fn the_same_interval_lands_on_different_dates_in_different_timezones_with_the_same_total() {
    let mut h = setup();
    h.session("s-fg", "FOREGROUND", "finished", 0);
    // UTC 2026-03-09 23:00–24:00 ＝ 上海 2026-03-10 07:00–08:00，1 小时。
    let from = 1_773_097_200_000;
    let to = from + 3_600_000;
    h.interval("i-hour", "s-fg", from, Some(to), Some(3_600_000), 0, None);

    let utc = h.report(from, to, "UTC");
    assert_eq!(utc.range.from, from);
    assert_eq!(
        ms(&utc, StatsClass::Confirmed, Measure::Human),
        Some(3_600_000)
    );
    assert_eq!(day_ms(&utc, "2026-03-09", Measure::Human), Some(3_600_000));
    assert_eq!(day_ms(&utc, "2026-03-10", Measure::Human), None);

    let shanghai = h.report(from, to, "Asia/Shanghai");
    assert_eq!(
        ms(&shanghai, StatsClass::Confirmed, Measure::Human),
        Some(3_600_000),
        "换时区不改变总和"
    );
    assert_eq!(
        day_ms(&shanghai, "2026-03-10", Measure::Human),
        Some(3_600_000),
        "同一段在上海归属 03-10"
    );
    assert_eq!(day_ms(&shanghai, "2026-03-09", Measure::Human), None);
    assert_eq!(shanghai.timezone, "Asia/Shanghai");
    assert_eq!(utc.timezone, "UTC");
}

// ─────────────────────────────────────────────────────────────────────────────
// 排除口径
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn voided_pending_and_discarded_records_are_excluded_and_voided_is_not_pending() {
    let mut h = setup();
    // 正常闭合 1 小时。
    h.session("s-ok", "FOREGROUND", "finished", 0);
    h.interval(
        "i-ok",
        "s-ok",
        SH_MID,
        Some(SH_MID + 3_600_000),
        Some(3_600_000),
        0,
        None,
    );
    // 已作废的 1 小时：不进任何「已确认」数字，也不显示为待确认。
    h.session("s-void", "FOREGROUND", "finished", 0);
    h.interval(
        "i-void",
        "s-void",
        SH_MID,
        Some(SH_MID + 3_600_000),
        Some(3_600_000),
        0,
        Some(SH_MID + 3_600_000),
    );
    // 已丢弃会话里一条「看起来可信」的 1 小时：同样排除。
    h.session("s-disc", "FOREGROUND", "discarded", 0);
    h.interval(
        "i-disc",
        "s-disc",
        SH_MID,
        Some(SH_MID + 3_600_000),
        Some(3_600_000),
        0,
        None,
    );
    // recovering 会话：可信前缀 10 分钟 + 未作废的待确认余段（候选端点不是事实）。
    h.session("s-rec", "FOREGROUND", "recovering", 1);
    h.interval(
        "i-prefix",
        "s-rec",
        SH_MID + 3_600_000,
        Some(SH_MID + 4_200_000),
        Some(600_000),
        0,
        None,
    );
    h.interval("i-tail", "s-rec", SH_MID + 4_200_000, None, None, 1, None);

    let snapshot = h.snapshot(SH_MID, SH_MID + 7_200_000, "UTC");
    let report = snapshot.report().unwrap();

    // 已确认 = 正常 1 小时 + recovering 的可信前缀 10 分钟；作废与丢弃都不算。
    assert_eq!(
        ms(&report, StatsClass::Confirmed, Measure::Human),
        Some(4_200_000)
    );
    // 待确认栏只有那一条未作废的余段，且**不给毫秒数**（候选端点不是事实）。
    let pending = report.column(StatsClass::Pending, Measure::Human);
    assert_eq!(pending.intervals, 1);
    assert_eq!(pending.ms, None);
    assert_eq!(ms(&report, StatsClass::Live, Measure::Human), Some(0));

    // 作废与丢弃的行根本没被读进来（排除只做一次，在 SQL 谓词里）。
    let ids: Vec<&str> = report.intervals.iter().map(|iv| iv.id.as_str()).collect();
    assert!(!ids.contains(&"i-void"), "已作废的行不进统计读");
    assert!(!ids.contains(&"i-disc"), "已丢弃会话的行不进统计读");
    assert!(ids.contains(&"i-prefix"));
    assert!(ids.contains(&"i-tail"));
}

#[test]
fn intervals_of_a_broken_session_are_not_counted_as_confirmed() {
    let mut h = setup();
    h.session("s-ok", "FOREGROUND", "finished", 0);
    h.interval(
        "i-ok",
        "s-ok",
        SH_MID,
        Some(SH_MID + 3_600_000),
        Some(3_600_000),
        0,
        None,
    );
    // 第 1 类（不变量损坏）：非 running 会话残留开放区间（P3 的判定）。
    // 它还有一段看似可信的 1 小时闭合区间——损坏会话的区间一律不计入已确认。
    h.session("s-fault", "FOREGROUND", "paused", 0);
    h.interval(
        "i-fault-closed",
        "s-fault",
        SH_MID,
        Some(SH_MID + 3_600_000),
        Some(3_600_000),
        0,
        None,
    );
    h.interval(
        "i-fault-open",
        "s-fault",
        SH_MID + 3_600_000,
        None,
        None,
        0,
        None,
    );

    let report = h.report(SH_MID, SH_MID + 7_200_000, "UTC");
    assert_eq!(
        ms(&report, StatsClass::Confirmed, Measure::Human),
        Some(3_600_000),
        "只有健康会话的 1 小时；损坏会话的 1 小时被排除"
    );
    assert_eq!(
        report
            .column(StatsClass::Confirmed, Measure::Human)
            .intervals,
        1
    );
    assert_eq!(report.fault_sessions_excluded, 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// 人工 / 机器分离
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_hour_of_foreground_plus_an_hour_of_background_reports_one_hour_of_human_work() {
    let mut h = setup();
    h.session("s-fg", "FOREGROUND", "finished", 0);
    h.session("s-bg", "BACKGROUND", "finished", 0);
    // 两段**完全并行**（同一时间轴区间）。
    h.interval(
        "i-fg",
        "s-fg",
        SH_MID,
        Some(SH_MID + 3_600_000),
        Some(3_600_000),
        0,
        None,
    );
    h.interval(
        "i-bg",
        "s-bg",
        SH_MID,
        Some(SH_MID + 3_600_000),
        Some(3_600_000),
        0,
        None,
    );

    let report = h.report(SH_MID, SH_MID + 3_600_000, "UTC");
    assert_eq!(
        ms(&report, StatsClass::Confirmed, Measure::Human),
        Some(3_600_000),
        "1h 前台 + 1h 后台必须报人工 1 小时，不是 2 小时（F-103）"
    );
    assert_eq!(
        ms(&report, StatsClass::Confirmed, Measure::MachineBackground),
        Some(3_600_000),
        "并行机器时长单独一项，等于 1 小时"
    );
    assert_eq!(
        ms(&report, StatsClass::Confirmed, Measure::MachinePassive),
        Some(0)
    );
}

#[test]
fn waiting_time_gets_its_own_column_instead_of_being_merged_into_another_measure() {
    let mut h = setup();
    h.session("s-fg", "FOREGROUND", "finished", 0);
    h.session("s-bg", "BACKGROUND", "finished", 0);
    h.session("s-ps", "PASSIVE", "finished", 0);
    h.session("s-wt", "WAITING", "finished", 0);
    h.interval(
        "i-fg",
        "s-fg",
        SH_MID,
        Some(SH_MID + 3_600_000),
        Some(3_600_000),
        0,
        None,
    );
    h.interval(
        "i-bg",
        "s-bg",
        SH_MID,
        Some(SH_MID + 3_600_000),
        Some(3_600_000),
        0,
        None,
    );
    h.interval(
        "i-ps",
        "s-ps",
        SH_MID,
        Some(SH_MID + 1_800_000),
        Some(1_800_000),
        0,
        None,
    );
    h.interval(
        "i-wt",
        "s-wt",
        SH_MID,
        Some(SH_MID + 2_700_000),
        Some(2_700_000),
        0,
        None,
    );

    let report = h.report(SH_MID, SH_MID + 3_600_000, "UTC");
    assert_eq!(
        ms(&report, StatsClass::Confirmed, Measure::Human),
        Some(3_600_000)
    );
    assert_eq!(
        ms(&report, StatsClass::Confirmed, Measure::MachineBackground),
        Some(3_600_000)
    );
    assert_eq!(
        ms(&report, StatsClass::Confirmed, Measure::MachinePassive),
        Some(1_800_000)
    );
    assert_eq!(
        ms(&report, StatsClass::Confirmed, Measure::Waiting),
        Some(2_700_000),
        "WAITING 单列：45 分钟不进人工，也不进任何机器项"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// DTO 口径字段
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn every_column_carries_the_five_contract_fields_from_one_read() {
    let mut h = setup();
    h.session("s-fg", "FOREGROUND", "finished", 0);
    h.interval(
        "i-fg",
        "s-fg",
        SH_MID,
        Some(SH_MID + 3_600_000),
        Some(3_600_000),
        0,
        None,
    );

    let snapshot = h.snapshot(SH_MID, SH_MID + 3_600_000, "Asia/Shanghai");
    let revision = h.revision();
    let report = snapshot.report().unwrap();

    assert_eq!(report.as_of, WALL, "as_of 是本次采样的归属挂钟");
    assert_eq!(report.as_of, snapshot.as_of());
    assert_eq!(report.as_of, snapshot.attributed_end);
    assert_eq!(report.data_epoch, h.epoch);
    assert_eq!(report.revision, revision, "revision 与数据同一次读");
    assert_eq!(snapshot.revision, revision);
    assert_eq!(report.timezone, "Asia/Shanghai");
    assert_eq!(report.range.from, SH_MID);
    assert_eq!(report.range.to, SH_MID + 3_600_000);

    // 五个字段落在**每一列**上（02 §6：报表必须返回 measure/timezone/range/as_of/revision）。
    for class in [StatsClass::Confirmed, StatsClass::Live, StatsClass::Pending] {
        let columns: &[MeasureColumn] = match class {
            StatsClass::Confirmed => &report.confirmed,
            StatsClass::Live => &report.live,
            StatsClass::Pending => &report.pending,
        };
        assert_eq!(
            columns.len(),
            Measure::ALL.len(),
            "每类固定四项，缺项以 0 出现"
        );
        for (column, want_measure) in columns.iter().zip(Measure::ALL) {
            assert_eq!(column.class, class);
            assert_eq!(column.measure, want_measure);
            assert_eq!(column.timezone, "Asia/Shanghai");
            assert_eq!(column.range.from, SH_MID);
            assert_eq!(column.range.to, SH_MID + 3_600_000);
            assert_eq!(column.as_of, WALL);
            assert_eq!(column.data_epoch, h.epoch);
            assert_eq!(column.revision, revision);
        }
    }
    // 列的条数与明细列表一致（同一遍分类）。
    assert_eq!(
        report
            .intervals
            .iter()
            .filter(|iv| iv.class == StatsClass::Confirmed)
            .count(),
        1
    );
}

#[test]
fn a_stale_data_epoch_is_rejected_before_any_fact_is_read() {
    let mut h = setup();
    h.session("s-fg", "FOREGROUND", "finished", 0);
    h.interval(
        "i-fg",
        "s-fg",
        SH_MID,
        Some(SH_MID + 3_600_000),
        Some(3_600_000),
        0,
        None,
    );

    let mut query = h.query(SH_MID, SH_MID + 3_600_000, "UTC");
    query.expected_data_epoch = "epoch-from-another-database".to_string();
    let sample = h.coord.stats_sample(&mut h.db).unwrap();
    // 样本本身不检查 epoch（它不是业务读）；epoch 守卫在一致读那一步。
    let err = worktrace_lib::services::stats::snapshot(&h.db, sample, &query).unwrap_err();
    assert_eq!(err.code(), "DATA_EPOCH_MISMATCH");
}

// ─────────────────────────────────────────────────────────────────────────────
// 装置二：真启动 + 真协调器（并发与异常分割）
// ─────────────────────────────────────────────────────────────────────────────

struct AppFixture {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    lock_path: PathBuf,
}

fn app_fixture() -> AppFixture {
    let dir = tempfile::tempdir().unwrap();
    let fx = AppFixture {
        db_path: dir.path().join("worktrace.db"),
        lock_path: dir.path().join("instance.lock"),
        _dir: dir,
    };
    // 先建一个带任务行的库：`start` 需要一条可执行的任务。
    let db = Db::open(&fx.db_path).unwrap();
    migrate(db.connection()).unwrap();
    db.connection()
        .execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES('t1','任务','Doing',0,1000,1000)",
            [],
        )
        .unwrap();
    fx
}

/// 真跑一次启动；采样节拍放到 1 分钟，避免周期线程干扰并发用例。
fn started(fx: &AppFixture, clock: Arc<Mutex<FakeClock>>) -> Box<RunningApp> {
    let config = StartupConfig {
        db_path: fx.db_path.clone(),
        lock_path: fx.lock_path.clone(),
        sampling_interval_ms: 60_000,
    };
    let outcome = startup(config, Box::new(clock), Arc::new(NoSink), &NoProbe, &|| {
        Ok(())
    })
    .expect("启动应当成功");
    match outcome {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    }
}

fn start_request(epoch: &str) -> StartRequest {
    StartRequest {
        expected_data_epoch: epoch.to_string(),
        task_id: "t1".into(),
        task_expected_version: 0,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: 30_000,
    }
}

fn session_request(
    guard: &mut worktrace_lib::services::bootstrap::AppState,
    epoch: &str,
) -> SessionRequest {
    let snapshot = guard.snapshot().unwrap();
    SessionRequest {
        expected_data_epoch: epoch.to_string(),
        session_id: snapshot.session_id.clone().unwrap(),
        session_expected_version: snapshot.session_version.unwrap(),
    }
}

/// 一次统计读的结果：`Err` 只记错误码（跃迁过程中「先提交恢复事务再拒绝」是合法结果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Observed {
    Report(i64, i64, usize),
    Refused,
}

#[test]
fn a_report_never_mixes_the_two_sides_of_a_pause_resume_finish_sequence() {
    let fx = app_fixture();
    let clock = Arc::new(Mutex::new(FakeClock::new(WALL, 0)));
    let running = started(&fx, Arc::clone(&clock));
    let epoch = running.data_epoch().to_string();
    let app = Arc::clone(running.app());

    {
        let mut guard = lock_app(&app);
        guard.start(start_request(&epoch)).unwrap();
    }
    // 计时满 1 小时：**分片推进 + 每片一次真实采样**。一次跳满 1 小时会撞上
    // 08 §1 的「无可靠通知的长间隔」（d_mono > 3 × 期望采样间隔即判 `Suspended`），
    // 那是异常分割路径，不是本用例要观察的「跃迁前 / 跃迁后」。
    for _ in 0..60 {
        clock.lock().unwrap().advance_both(60_000);
        let mut guard = lock_app(&app);
        guard.sample_tick().unwrap();
    }

    let stop = Arc::new(AtomicBool::new(false));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let worker = {
        let app = Arc::clone(&app);
        let stop = Arc::clone(&stop);
        let observed = Arc::clone(&observed);
        let epoch = epoch.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let query = StatsRangeQuery {
                    from: WALL,
                    to: WALL + 7_200_000,
                    timezone: "UTC".into(),
                    expected_data_epoch: epoch.clone(),
                };
                // 取样与一致读在同一个串行边界内完成；聚合在边界外。
                let snapshot = {
                    let mut guard = lock_app(&app);
                    guard.stats_snapshot(&query)
                };
                let seen = match snapshot {
                    Ok(snapshot) => match snapshot.report() {
                        Ok(report) => Observed::Report(
                            ms(&report, StatsClass::Confirmed, Measure::Human).unwrap(),
                            ms(&report, StatsClass::Live, Measure::Human).unwrap(),
                            report.column(StatsClass::Pending, Measure::Human).intervals,
                        ),
                        Err(_) => Observed::Refused,
                    },
                    Err(_) => Observed::Refused,
                };
                observed.lock().unwrap().push(seen);
                std::thread::sleep(Duration::from_micros(200));
            }
        })
    };

    std::thread::sleep(Duration::from_millis(20));
    {
        let mut guard = lock_app(&app);
        let req = session_request(&mut guard, &epoch);
        guard.pause(req).unwrap();
    }
    std::thread::sleep(Duration::from_millis(20));
    {
        let mut guard = lock_app(&app);
        let snapshot = guard.snapshot().unwrap();
        let req = ResumeRequest {
            expected_data_epoch: epoch.clone(),
            task_id: "t1".into(),
            task_expected_version: snapshot.task_row_version.unwrap(),
            session_id: snapshot.session_id.clone().unwrap(),
            session_expected_version: snapshot.session_version.unwrap(),
        };
        guard.resume(req).unwrap();
    }
    std::thread::sleep(Duration::from_millis(20));
    {
        let mut guard = lock_app(&app);
        let req = session_request(&mut guard, &epoch);
        guard.finish(req).unwrap();
    }
    std::thread::sleep(Duration::from_millis(20));
    stop.store(true, Ordering::SeqCst);
    worker.join().unwrap();

    let seen = observed.lock().unwrap().clone();
    assert!(!seen.is_empty(), "并发阶段必须真的读到过统计");
    // 跃迁前的完整快照：已确认 0、暂计 1 小时、待确认 0。
    let before = Observed::Report(0, 3_600_000, 0);
    // 跃迁后的完整快照：已确认 1 小时、暂计 0、待确认 0。
    let after = Observed::Report(3_600_000, 0, 0);
    for (index, one) in seen.iter().enumerate() {
        assert!(
            *one == before || *one == after || *one == Observed::Refused,
            "第 {index} 次统计既不是跃迁前也不是跃迁后的完整快照：{one:?}"
        );
    }
    assert!(seen.contains(&before), "并发阶段必须观察到跃迁前的快照");
    assert!(seen.contains(&after), "并发阶段必须观察到跃迁后的快照");
    assert!(
        seen.iter()
            .all(|one| *one != Observed::Report(3_600_000, 3_600_000, 0)),
        "不能把同一段时间既算已确认又算暂计（重复计时）"
    );

    // 收尾：结束后再做一次确定的统计读。
    let mut guard = lock_app(&app);
    let snapshot = guard
        .stats_snapshot(&StatsRangeQuery {
            from: WALL,
            to: WALL + 7_200_000,
            timezone: "UTC".into(),
            expected_data_epoch: epoch.clone(),
        })
        .unwrap();
    drop(guard);
    let report = snapshot.report().unwrap();
    assert_eq!(
        ms(&report, StatsClass::Confirmed, Measure::Human),
        Some(3_600_000)
    );
    assert_eq!(ms(&report, StatsClass::Live, Measure::Human), Some(0));
}

#[test]
fn an_anomaly_split_keeps_the_trusted_prefix_confirmed_and_the_remainder_pending() {
    let fx = app_fixture();
    let clock = Arc::new(Mutex::new(FakeClock::new(WALL, 0)));
    let running = started(&fx, Arc::clone(&clock));
    let epoch = running.data_epoch().to_string();
    let app = Arc::clone(running.app());

    {
        let mut guard = lock_app(&app);
        guard.start(start_request(&epoch)).unwrap();
    }
    // 30 秒后写下一个可信检查点：它就是将来可信前缀的终点。
    clock.lock().unwrap().advance_both(30_000);
    {
        let mut guard = lock_app(&app);
        guard.sample_tick().unwrap();
    }

    let query = StatsRangeQuery {
        from: WALL,
        to: WALL + 3_600_000,
        timezone: "UTC".into(),
        expected_data_epoch: epoch.clone(),
    };

    // 下一拍采样失败 ⇒ 统计取样先提交恢复事务（分割区间），再按恢复语义拒绝。
    clock.lock().unwrap().fail_once();
    let refused = {
        let mut guard = lock_app(&app);
        guard.stats_snapshot(&query)
    };
    assert_eq!(
        refused.unwrap_err().code(),
        "RECOVERY_REQUIRED",
        "异常那一拍不产出统计，先落恢复事务"
    );

    // 之后统计照常可读：可信前缀计入已确认，余段单列待确认且**不给时长**。
    let snapshot = {
        let mut guard = lock_app(&app);
        guard.stats_snapshot(&query).unwrap()
    };
    let report = snapshot.report().unwrap();
    assert_eq!(
        ms(&report, StatsClass::Confirmed, Measure::Human),
        Some(30_000),
        "最后可信检查点之前的前缀是已确认工时"
    );
    assert_eq!(ms(&report, StatsClass::Live, Measure::Human), Some(0));
    let pending = report.column(StatsClass::Pending, Measure::Human);
    assert_eq!(pending.intervals, 1, "终点未知的余段单列一条待确认");
    assert_eq!(pending.ms, None);
    // 前缀不得被重复计入：已确认恰好 30 秒，不等于「前缀 + 余段」。
    assert_eq!(
        report
            .column(StatsClass::Confirmed, Measure::Human)
            .intervals,
        1
    );
}
