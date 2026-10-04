//! P3 Task 5：**真实日界**（S10）——`services::daily_plan::{local_day_bounds, local_days_covering}`。
//!
//! 断言口径（S10 的语义是钉死的，见计划 §0.3）：
//!
//! - **一天不是 24 小时**：`end` 是「次日零点」的换算结果，**不是** `start + 86_400_000`。
//!   夏令时切换日因此是 23/25 小时（半小时制 DST 的 `Australia/Lord_Howe` 是 23.5/24.5 小时）。
//!   这两类日子都已经是**过去的事实**（tzdb 不重写历史），所以这里把逐毫秒的边界写死——
//!   换成 `start + 86_400_000` 的实现会在这几条上立刻红。
//! - **半开**：`[start, end)`；`end` 属于次日，端点相接不算重叠。
//! - `local_days_covering` 只返回与 `[from, to)` **正相交**（`overlap_ms > 0`）的本地日，
//!   按日升序，元素是该日的真实半开界；逐日拼接**无缺口、无重叠**（裁片之和 == 原范围长度）。
//! - **零长度范围** ⇒ 空 `Vec`；**反向范围** ⇒ `DOMAIN_ERROR`，**不静默交换端点**。
//! - 时区只有 `normalize_timezone` 这一个校验入口：两个函数对坏时区的拒绝一致
//!   （不接受 `Etc/Unknown`、固定偏移、空白）。
//!
//! 独立 oracle：日界的**属性**用 jiff 自己的 `Timestamp → Zoned → date()` 换算核对
//! （`start` 属于本日、`start - 1` 属于前一日、`end` 属于次日、`end - 1` 属于本日），
//! 而不是回读被测函数的输出；期望的日期序列也由 jiff 的日历逐日推进得到。

use worktrace_lib::domain::interval::{IntervalRange, IntervalSet};
use worktrace_lib::domain::localdate::LocalDate;
use worktrace_lib::error::AppError;
use worktrace_lib::services::daily_plan::{local_day_bounds, local_days_covering};

const UTC: &str = "UTC";
const SHANGHAI: &str = "Asia/Shanghai";
/// 智利：DST 切换发生在**周六 24:00**，切换日分别是 23 小时与 25 小时。
const SANTIAGO: &str = "America/Santiago";
/// 豪勋爵岛：DST 只挪 **30 分钟**，切换日是 23.5 / 24.5 小时（不是整小时）。
const LORD_HOWE: &str = "Australia/Lord_Howe";

/// 一天的毫秒数——**只用作反例**：任何一天都不该假设它等于这个常量。
const DAY_MS: i64 = 86_400_000;
const HOUR_MS: i64 = 3_600_000;

// ─────────────────────────────────────────────────────────────────────────────
// 独立 oracle（jiff 自己的换算，与被测实现无关）
// ─────────────────────────────────────────────────────────────────────────────

/// 某个时刻在某个时区里的**本地日期**（`YYYY-MM-DD`）。
///
/// 这一步只走 jiff 的公开换算，不碰被测函数：它是「边界确实落在这一天」的裁判。
fn local_date_of(wall_ms: i64, timezone: &str) -> String {
    let at = jiff::Timestamp::from_millisecond(wall_ms).expect("测试里的时刻都在可表示范围内");
    let zone = jiff::tz::TimeZone::get(timezone).expect("测试里的时区名必须是可用的 IANA 名");
    at.to_zoned(zone).date().to_string()
}

fn jiff_date(raw: &str) -> jiff::civil::Date {
    raw.parse().expect("测试里的日期形状固定")
}

fn next_day(day: &str) -> String {
    jiff_date(day)
        .tomorrow()
        .expect("测试范围不越过可表示的日期上界")
        .to_string()
}

fn prev_day(day: &str) -> String {
    jiff_date(day)
        .yesterday()
        .expect("测试范围不越过可表示的日期下界")
        .to_string()
}

/// 由 jiff 的日历逐日推进得到的日期序列（含两端）。
fn days_between(first: &str, last: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut day = jiff_date(first);
    let end = jiff_date(last);
    loop {
        out.push(day.to_string());
        if day == end {
            return out;
        }
        day = day.tomorrow().expect("测试范围不越过可表示的日期上界");
    }
}

fn date(raw: &str) -> LocalDate {
    LocalDate::parse(raw).expect("测试里的日期都是合法日历日")
}

/// 取出 `local_days_covering` 结果里的日期序列。
fn covered_dates(days: &[(LocalDate, IntervalRange)]) -> Vec<String> {
    days.iter().map(|(day, _)| day.to_string()).collect()
}

/// 日界的**属性**：起点属于本日、终点属于次日；各减一毫秒分别落在前一日与本日。
///
/// 这四条一起把「半开」与「边界确实是本地零点（或跳变那一刻）」钉死，
/// 而且用的是独立换算——不是「实现算出什么就信什么」。
fn assert_bounds_are_the_real_local_day(timezone: &str, day: &str, bounds: IntervalRange) {
    assert_eq!(
        local_date_of(bounds.start, timezone),
        day,
        "{timezone} 的 {day}：起点必须落在这一天"
    );
    assert_eq!(
        local_date_of(bounds.end, timezone),
        next_day(day),
        "{timezone} 的 {day}：终点必须落在次日（半开）"
    );
    assert_eq!(
        local_date_of(bounds.start - 1, timezone),
        prev_day(day),
        "{timezone} 的 {day}：起点前一毫秒必须还在前一天"
    );
    assert_eq!(
        local_date_of(bounds.end - 1, timezone),
        day,
        "{timezone} 的 {day}：终点前一毫秒必须还在这一天"
    );
}

/// 一个日界的「写死 + 属性」双重断言（属性部分见
/// [`assert_bounds_are_the_real_local_day`]）。
fn assert_day(timezone: &str, day: &str, expected_start: i64, expected_end: i64) {
    let bounds = local_day_bounds(timezone, date(day)).expect("合法时区与日期必须能算出日界");
    assert_eq!(
        bounds,
        IntervalRange {
            start: expected_start,
            end: expected_end,
        },
        "{timezone} 的 {day} 日界（毫秒）对不上"
    );
    assert_bounds_are_the_real_local_day(timezone, day, bounds);
}

fn assert_code(error: &AppError, code: &str) {
    assert_eq!(error.code(), code, "实际：{error:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 日界：24 小时的普通日、跨月、跨年、闰日
// ─────────────────────────────────────────────────────────────────────────────

/// 一天的界就是**本地**两个零点之间的半开区间；同一时刻在不同时区属于不同的日。
#[test]
fn a_day_is_the_half_open_interval_between_two_local_midnights() {
    // 2026-10-03 / 10-04（UTC）。
    assert_day(UTC, "2026-10-03", 1_790_985_600_000, 1_791_072_000_000);
    assert_day(UTC, "2026-10-04", 1_791_072_000_000, 1_791_158_400_000);

    // 同一个本地日期在 `Asia/Shanghai` 是另一段区间：比 UTC 早 8 小时开始。
    assert_day(SHANGHAI, "2026-10-03", 1_790_956_800_000, 1_791_043_200_000);
    let utc = local_day_bounds(UTC, date("2026-10-03")).unwrap();
    let shanghai = local_day_bounds(SHANGHAI, date("2026-10-03")).unwrap();
    assert_eq!(
        utc.start - shanghai.start,
        8 * HOUR_MS,
        "上海比 UTC 早 8 小时进入这一天（时区必须参与计算）"
    );
    assert_eq!(shanghai.duration_ms(), DAY_MS, "上海这一天是 24 小时");
}

/// 跨月、跨年、闰日：相邻日的半开界**首尾相接**（前一日 `end` == 后一日 `start`），
/// 既不留缺口也不重叠。
#[test]
fn adjacent_days_meet_exactly_at_the_midnight_they_share() {
    // 跨月：10-31 → 11-01。
    assert_day(UTC, "2026-10-31", 1_793_404_800_000, 1_793_491_200_000);
    assert_day(UTC, "2026-11-01", 1_793_491_200_000, 1_793_577_600_000);
    // 跨年：12-31 → 次年 01-01。
    assert_day(UTC, "2026-12-31", 1_798_675_200_000, 1_798_761_600_000);
    assert_day(UTC, "2027-01-01", 1_798_761_600_000, 1_798_848_000_000);
    // 闰日：2024-02-29 存在，且它到 03-01 仍是 24 小时。
    assert_day(UTC, "2024-02-29", 1_709_164_800_000, 1_709_251_200_000);

    for (earlier, later) in [
        ("2026-10-31", "2026-11-01"),
        ("2026-12-31", "2027-01-01"),
        ("2024-02-29", "2024-03-01"),
    ] {
        let a = local_day_bounds(UTC, date(earlier)).unwrap();
        let b = local_day_bounds(UTC, date(later)).unwrap();
        assert_eq!(
            a.end, b.start,
            "{earlier} 的终点必须正好是 {later} 的起点（半开、无缺口、无重叠）"
        );
        assert_eq!(a.overlap_ms(b), 0, "端点相接不算重叠");
        assert!(!a.overlaps(b));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 日界：一天不是 24 小时
// ─────────────────────────────────────────────────────────────────────────────

/// 夏令时切换日**不是 24 小时**：`end` 必须由「次日零点」换算，而不是 `start + 86_400_000`。
///
/// 用两组独立的 IANA 事实：
/// - `America/Santiago` 2024-09-08（春季前跳）是 **23** 小时、2024-04-06（秋季回拨）是 **25** 小时；
/// - `Australia/Lord_Howe` 2024-10-06 是 **23.5** 小时、2024-04-07 是 **24.5** 小时
///   （它的 DST 只挪 30 分钟——「非整小时的一天」也在这里被钉住）。
#[test]
fn a_dst_transition_day_is_not_twenty_four_hours() {
    // 智利：周六 24:00 跳变。
    assert_day(SANTIAGO, "2024-09-08", 1_725_768_000_000, 1_725_850_800_000);
    assert_day(SANTIAGO, "2024-04-06", 1_712_372_400_000, 1_712_462_400_000);
    // 豪勋爵岛：半小时制 DST。
    assert_day(
        LORD_HOWE,
        "2024-10-06",
        1_728_135_000_000,
        1_728_219_600_000,
    );
    assert_day(
        LORD_HOWE,
        "2024-04-07",
        1_712_408_400_000,
        1_712_496_600_000,
    );

    let short = local_day_bounds(SANTIAGO, date("2024-09-08")).unwrap();
    let long = local_day_bounds(SANTIAGO, date("2024-04-06")).unwrap();
    let half_short = local_day_bounds(LORD_HOWE, date("2024-10-06")).unwrap();
    let half_long = local_day_bounds(LORD_HOWE, date("2024-04-07")).unwrap();

    assert_eq!(short.duration_ms(), 23 * HOUR_MS, "前跳那天是 23 小时");
    assert_eq!(long.duration_ms(), 25 * HOUR_MS, "回拨那天是 25 小时");
    assert_eq!(
        half_short.duration_ms(),
        23 * HOUR_MS + 30 * 60_000,
        "半小时制 DST 的前跳那天是 23.5 小时"
    );
    assert_eq!(
        half_long.duration_ms(),
        24 * HOUR_MS + 30 * 60_000,
        "半小时制 DST 的回拨那天是 24.5 小时"
    );
    for (label, bounds) in [
        ("Santiago 前跳", short),
        ("Santiago 回拨", long),
        ("Lord Howe 前跳", half_short),
        ("Lord Howe 回拨", half_long),
    ] {
        assert_ne!(
            bounds.duration_ms(),
            DAY_MS,
            "{label}：这一天不是 24 小时——`start + 86_400_000` 一定是错的实现"
        );
    }

    // 切换日两侧仍是 24 小时，而且三段首尾相接——只有切换日那一段被压缩/拉长。
    let before = local_day_bounds(SANTIAGO, date("2024-09-07")).unwrap();
    let after = local_day_bounds(SANTIAGO, date("2024-09-09")).unwrap();
    assert_eq!(before.duration_ms(), DAY_MS);
    assert_eq!(after.duration_ms(), DAY_MS);
    assert_eq!(before.end, short.start);
    assert_eq!(short.end, after.start);
    assert_eq!(
        before.duration_ms() + short.duration_ms() + after.duration_ms(),
        71 * HOUR_MS,
        "三个相连的本地日加起来是 71 小时（不是 72）"
    );
}

/// 同一个 `LocalDate` 在不同时区是不同长度的区间：切换发生在哪个时区，就只影响那个时区。
#[test]
fn the_same_date_has_different_bounds_in_different_zones() {
    let santiago = local_day_bounds(SANTIAGO, date("2024-09-08")).unwrap();
    let utc = local_day_bounds(UTC, date("2024-09-08")).unwrap();
    assert_eq!(utc.duration_ms(), DAY_MS, "UTC 那天没有切换");
    assert_ne!(santiago.duration_ms(), utc.duration_ms());
    assert_ne!(santiago.start, utc.start, "两个时区的零点不是同一个时刻");
}

/// 日期越过可表示范围时**报错，不钳制**（钳制会把一个坏输入变成看起来合理的日界）。
///
/// `9999-12-31` 是 `LocalDate` 允许的最大日期，但它的「次日零点」不存在——
/// 这正是 `Date::tomorrow` 失败的分支。
#[test]
fn a_date_whose_next_midnight_does_not_exist_is_rejected() {
    let err = local_day_bounds(UTC, date("9999-12-31")).expect_err("次日零点不可表示就该报错");
    assert_code(&err, "DOMAIN_ERROR");
}

// ─────────────────────────────────────────────────────────────────────────────
// covering：覆盖哪些天、按什么顺序、边界怎么算
// ─────────────────────────────────────────────────────────────────────────────

/// `local_days_covering` 覆盖的**恰好**是端点所触及的那些本地日，按日升序，
/// 且每个元素都是该日的**真实半开界**（与 `local_day_bounds` 逐字段一致）。
#[test]
fn covering_returns_exactly_the_days_the_range_touches_in_ascending_order() {
    // 2026-10-03 12:00（UTC）→ 2026-10-06 12:00（UTC）：三个完整日 + 两个半天。
    let from = 1_790_985_600_000 + 12 * HOUR_MS;
    let to = 1_791_244_800_000 + 12 * HOUR_MS;
    let days = local_days_covering(UTC, from, to).expect("合法范围必须能覆盖");

    assert_eq!(
        covered_dates(&days),
        vec!["2026-10-03", "2026-10-04", "2026-10-05", "2026-10-06"],
        "首尾两个半天也算被覆盖，且按日升序"
    );
    for (day, range) in &days {
        assert_eq!(
            *range,
            local_day_bounds(UTC, *day).expect("逐日再算一次必须一致"),
            "{day} 的元素必须是该日的真实半开界（不是按范围裁出来的片段）"
        );
        assert!(
            range.clipped_ms(from, to) > 0,
            "{day} 必须与范围正相交（overlap_ms > 0）"
        );
    }

    // 独立 oracle：端点各自的本地日 → 逐日推进。
    let expected = days_between(&local_date_of(from, UTC), &local_date_of(to - 1, UTC));
    assert_eq!(covered_dates(&days), expected);
}

/// **半开**：端点正好落在日界上时不含次日；多一毫秒就把次日带进来。
#[test]
fn covering_is_half_open_so_the_day_that_starts_at_to_is_excluded() {
    let day_start = 1_790_985_600_000; // 2026-10-03 的零点（UTC）
    let next_start = 1_791_072_000_000; // 2026-10-04 的零点

    assert_eq!(
        covered_dates(&local_days_covering(UTC, day_start, next_start).unwrap()),
        vec!["2026-10-03"],
        "[d0, d1) 只覆盖 d0 那一天"
    );
    assert_eq!(
        covered_dates(&local_days_covering(UTC, day_start, next_start - 1).unwrap()),
        vec!["2026-10-03"],
        "少一毫秒仍然是同一天"
    );
    assert_eq!(
        covered_dates(&local_days_covering(UTC, day_start, next_start + 1).unwrap()),
        vec!["2026-10-03", "2026-10-04"],
        "多一毫秒才把次日带进来"
    );
    assert_eq!(
        covered_dates(&local_days_covering(UTC, next_start, next_start + 1).unwrap()),
        vec!["2026-10-04"],
        "从零点开始的一毫秒只属于这一天"
    );
}

/// **零长度范围不覆盖任何一天**：`from == to` ⇒ 空 `Vec`（不是「当天」）。
#[test]
fn a_zero_length_range_covers_no_day() {
    // 零点上、日中间、以及跨月的零点上，三种位置都是空。
    for at in [
        1_790_985_600_000,
        1_790_985_600_000 + 12 * HOUR_MS,
        1_793_491_200_000,
    ] {
        assert!(
            local_days_covering(UTC, at, at).unwrap().is_empty(),
            "零长度范围（{at}）不覆盖任何一天"
        );
        assert!(local_days_covering(SANTIAGO, at, at).unwrap().is_empty());
    }
}

/// **反向范围被拒，不静默交换端点**：`from > to` ⇒ `DOMAIN_ERROR`
/// （与 `IntervalRange::new` 同一判据）。交换会把「调用方算错了」变成「悄悄换了一天」。
#[test]
fn a_reversed_range_is_rejected_instead_of_swapped() {
    let from = 1_791_072_000_000; // 2026-10-04
    let to = 1_790_985_600_000; // 2026-10-03（更早）
    let err = local_days_covering(UTC, from, to).expect_err("反向范围必须被拒");
    assert_code(&err, "DOMAIN_ERROR");
    assert!(
        err.detail().unwrap_or_default().contains("负"),
        "文案要说清是负区间：{:?}",
        err.detail()
    );

    // 相邻零点的反向（差一天）同样被拒——不能靠「按天取整」蒙混过去。
    let err = local_days_covering(UTC, to + 1, to).expect_err("差一毫秒的反向也是反向");
    assert_code(&err, "DOMAIN_ERROR");
}

/// 覆盖一个**横跨夏令时切换**的范围：逐日拼接无缺口、无重叠，
/// 裁片之和正好等于原范围长度，而切换日那一天仍然是它真实的 23 小时。
#[test]
fn covering_days_tile_the_range_without_gaps_or_overlaps() {
    // 2024-09-06 01:00 → 2024-09-09 23:00（圣地亚哥当地时间），中间夹着 09-08 那一天。
    let from = 1_725_595_200_000 + HOUR_MS;
    let to = 1_725_937_200_000 - HOUR_MS;
    let days = local_days_covering(SANTIAGO, from, to).expect("合法范围必须能覆盖");

    assert_eq!(
        covered_dates(&days),
        vec!["2024-09-06", "2024-09-07", "2024-09-08", "2024-09-09"]
    );

    // 逐日首尾相接：前一日 end == 后一日 start（无缺口、无重叠）。
    for pair in days.windows(2) {
        assert_eq!(
            pair[0].1.end, pair[1].1.start,
            "{} 与 {} 之间既不能有缺口也不能有重叠",
            pair[0].0, pair[1].0
        );
    }

    // 范围两端都被覆盖住（第一个日包含 from、最后一个日包含 to - 1）。
    assert!(days[0].1.start <= from && from < days[0].1.end);
    let last = days.last().unwrap();
    assert!(last.1.start < to && to <= last.1.end);

    // 裁片用 `domain::interval` 的既有三件（clipped_ms / IntervalSet::insert），
    // 不新写相交判定：片段两两不重叠，且拼起来正好是原范围。
    let mut set = IntervalSet::new();
    let mut clipped_total = 0;
    for (_, range) in &days {
        assert_eq!(
            range.clipped_ms(from, to),
            range.overlap_ms(IntervalRange {
                start: from,
                end: to
            }),
            "clipped_ms 就是与 [from, to) 的 overlap_ms"
        );
        let piece = IntervalRange {
            start: range.start.max(from),
            end: range.end.min(to),
        };
        set.insert(piece).expect("裁片两两不重叠（端点相接允许）");
        clipped_total += range.clipped_ms(from, to);
    }
    assert_eq!(set.items().len(), days.len());
    assert_eq!(
        set.total_ms(),
        to - from,
        "裁片之和 == 原范围长度（无缺口、无重叠）"
    );
    assert_eq!(clipped_total, to - from);

    // 覆盖里只有切换日不是 24 小时。
    let not_a_full_day: Vec<i64> = days
        .iter()
        .map(|(_, range)| range.duration_ms())
        .filter(|duration| *duration != DAY_MS)
        .collect();
    assert_eq!(
        not_a_full_day,
        vec![23 * HOUR_MS],
        "四个相连的本地日里，只有 2024-09-08 是 23 小时"
    );
}

/// 范围**完全落在**切换日之内时，返回的仍然是那一天的**真实半开界**（23 小时），
/// 不是「按范围裁出来的 2 小时」。
#[test]
fn covering_inside_a_dst_day_still_returns_that_days_real_bounds() {
    let day_start = 1_725_768_000_000; // 2024-09-08 00:00（圣地亚哥，跳变那一刻）
    let from = day_start + HOUR_MS;
    let to = day_start + 3 * HOUR_MS;
    let days = local_days_covering(SANTIAGO, from, to).unwrap();
    assert_eq!(covered_dates(&days), vec!["2024-09-08"]);
    assert_eq!(
        days[0].1,
        local_day_bounds(SANTIAGO, date("2024-09-08")).unwrap()
    );
    assert_eq!(days[0].1.duration_ms(), 23 * HOUR_MS);
    assert_eq!(days[0].1.clipped_ms(from, to), 2 * HOUR_MS);
}

/// 跨月、跨年、跨闰日的覆盖：日数与日期序列都由 jiff 的日历独立推出来比对。
#[test]
fn covering_across_month_year_and_leap_boundaries_matches_the_calendar() {
    for (from_day, to_day) in [
        ("2026-10-30", "2026-11-02"),
        ("2026-12-30", "2027-01-02"),
        ("2024-02-27", "2024-03-02"),
    ] {
        let from = local_day_bounds(UTC, date(from_day)).unwrap().start + HOUR_MS;
        let to = local_day_bounds(UTC, date(to_day)).unwrap().start + HOUR_MS;
        let days = local_days_covering(UTC, from, to).unwrap();
        assert_eq!(
            covered_dates(&days),
            days_between(from_day, to_day),
            "{from_day} → {to_day} 的覆盖日序列"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 时区校验：两个入口同一道门
// ─────────────────────────────────────────────────────────────────────────────

/// 坏时区在**两个**入口上被同样拒绝：时区解析只有 `normalize_timezone` 这一个入口，
/// 不接受没有 IANA 名称的写法（`Etc/Unknown`、固定偏移）、也不接受空白。
#[test]
fn both_entries_reject_the_same_bad_timezones() {
    for bad in ["Etc/Unknown", "+08:00", "", "   ", "Mars/Olympus"] {
        let from_bounds = local_day_bounds(bad, date("2026-10-03")).expect_err("坏时区必须被拒");
        assert_code(&from_bounds, "DOMAIN_ERROR");
        let from_covering = local_days_covering(bad, 1_790_985_600_000, 1_791_072_000_000)
            .expect_err("坏时区必须被拒");
        assert_code(&from_covering, "DOMAIN_ERROR");
    }

    // 大小写不敏感的那一个特例仍然收敛到同一个存储键：`utc` 与 `UTC` 算出同一段区间。
    assert_eq!(
        local_day_bounds("utc", date("2026-10-03")).unwrap(),
        local_day_bounds(UTC, date("2026-10-03")).unwrap()
    );
    assert_eq!(
        covered_dates(&local_days_covering("utc", 1_790_985_600_000, 1_791_072_000_001).unwrap()),
        vec!["2026-10-03", "2026-10-04"]
    );
}
