//! P5 Task 4：Markdown 周回顾（F-018）。
//!
//! 覆盖（Task 4 的测试清单 + 控制器 G3 的裁决）：
//! - 周界 = 查询时区的**周一 → 下周一、半开**；跨周的记录按真实日界裁剪后各归其周；
//! - 完成任务按 `task_change` 里 `status → Done` 的**事件时刻**入周（改过 `updated_at` 不影响）；
//! - 同一任务「完成 → 重开 → 再完成」⇒ 两周的回顾里各出现一次（G3 的钉法）；
//! - `task_change` 的三种形状按**形状**过滤：贴标签 / 加今日计划 / 建任务都不算完成；
//! - 待确认单独成节，且**不进**人工合计；
//! - 同一个周重复生成内容稳定（除生成时间）；
//! - 空周也能生成合法文档；
//! - 全文只陈述事实：不含「效率」「节省」这类推断性措辞（04 的 F-206）；
//! - 夏令时周按真实日界（167 小时）而不是 7 × 24 小时；
//! - 生产入口 `AppState::export_weekly_markdown` 与 `services::export::weekly` 是同一份文档。
//!
//! 断言一律给出**期望数值本身**（计划 §断言口径），不用「非空」「大于 0」。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::domain::task::{TaskStatus, TransitionCause};
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, AppState, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::export::{self, WeeklyQuery};
use worktrace_lib::services::stats::{self, Measure, RangeReport, StatsClass, StatsRangeQuery};
use worktrace_lib::services::timer::coordinator::{Coordinator, SessionRequest, StartRequest};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{bump_revision, init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::run_repo;
use worktrace_lib::storage::task_repo;

/// 与本进程启动同名的本次 run（手工装置与真启动装置都用它）。
const RUN: &str = "run-1";

/// 上海 2026-03-09 00:00（+08:00）——**周一**，周 A 的起点。
const WEEK_A: i64 = 1_772_985_600_000;
/// 上海 2026-03-16 00:00（+08:00）——下一个**周一**，周 A 的终点（不含）、周 B 的起点。
const WEEK_B: i64 = 1_773_590_400_000;
/// 上海 2026-03-10 12:00——「此刻」的固定取值（周 A 里的周二中午）。
const SH_NOON: i64 = 1_773_115_200_000;

/// 纽约 2026-03-02 00:00（EST）——夏令时切换那一周的周一。
const NY_WEEK: i64 = 1_772_427_600_000;
/// 纽约 2026-03-09 00:00（EDT）——下一周周一；本周只有 **167** 小时。
const NY_NEXT: i64 = 1_773_028_800_000;
/// 纽约 2026-03-04 12:00——切换那一周里的「此刻」。
const NY_NOON: i64 = 1_772_643_600_000;

const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;
const MINUTE: i64 = 60_000;

// ─────────────────────────────────────────────────────────────────────────────
// Markdown 解析小工具（断言的是**数值**与**行**，不是整篇文本）
// ─────────────────────────────────────────────────────────────────────────────

/// 找包含 `needle` 的那一行，并断言它只出现在一行里。
fn line_with<'a>(text: &'a str, needle: &str) -> &'a str {
    let mut hits = text.lines().filter(|line| line.contains(needle));
    let first = hits
        .next()
        .unwrap_or_else(|| panic!("找不到包含「{needle}」的行：\n{text}"));
    assert!(hits.next().is_none(), "「{needle}」应当只出现在一行里");
    first
}

/// 在 `block` 里找**以 `prefix` 开头**的那一行，并断言唯一。
fn line_starting_with<'a>(block: &'a str, prefix: &str) -> &'a str {
    let mut hits = block.lines().filter(|line| line.starts_with(prefix));
    let first = hits
        .next()
        .unwrap_or_else(|| panic!("找不到以「{prefix}」开头的行：\n{block}"));
    assert!(hits.next().is_none(), "以「{prefix}」开头的行应当只有一行");
    first
}

/// 取一个 `## ` 小节（到下一个 `## ` 之前）。
fn section<'a>(text: &'a str, heading: &str) -> &'a str {
    let start = text
        .find(heading)
        .unwrap_or_else(|| panic!("找不到小节 {heading}：\n{text}"));
    let rest = &text[start + heading.len()..];
    match rest.find("\n## ") {
        Some(end) => &rest[..end],
        None => rest,
    }
}

/// 一行里「（<n> 毫秒）」的那个 n——口径要求每个时长都同时给毫秒。
fn ms_in_line(line: &str) -> i64 {
    let marker = " 毫秒）";
    let end = line
        .find(marker)
        .unwrap_or_else(|| panic!("这一行没有毫秒口径：{line}"));
    let open = line[..end]
        .rfind('（')
        .unwrap_or_else(|| panic!("这一行没有毫秒口径：{line}"));
    line[open + '（'.len_utf8()..end]
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("这一行的毫秒数解析不了：{line}"))
}

/// 文档里那一行周界的两个端点（Unix 毫秒）。
fn week_range_ms(text: &str) -> (i64, i64) {
    let line = line_with(text, "周界（Unix 毫秒）：");
    let open = line.find('[').expect("周界行里有半开范围") + 1;
    let close = line.rfind(')').expect("周界行里有半开范围");
    let (from, to) = line[open..close]
        .split_once(", ")
        .unwrap_or_else(|| panic!("周界行的形状不对：{line}"));
    (from.parse().unwrap(), to.parse().unwrap())
}

/// 把给定的几个毫秒数在文本里抹成同一个记号：两次生成之间**只有时间水位**可以变。
fn blank_watermarks(text: &str, marks: &[i64]) -> String {
    let mut out = text.to_string();
    for ms in marks {
        out = out.replace(&ms.to_string(), "<WM>");
    }
    out
}

/// 忽略全部事件的出口：周回顾用例不关心广播，但 `startup` 需要一个。
struct NoSink;

impl EventSink for NoSink {
    fn broadcast(&self, _envelope: &EventEnvelope) -> Result<(), String> {
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 装置：手工造事实（形状可控）+ 真协调器（活会话走真 `start`）
// ─────────────────────────────────────────────────────────────────────────────

struct H {
    _dir: tempfile::TempDir,
    db: Db,
    coord: Coordinator,
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

    // 协调器持有这个时钟的 `Arc`（本装置不推墙钟：周回顾的用例都用手工造好的事实）。
    let clock = Arc::new(Mutex::new(FakeClock::new(wall, 0)));
    let coord = Coordinator::new(Box::new(Arc::clone(&clock)), RUN);
    H {
        _dir: dir,
        db,
        coord,
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

    /// 走仓储原语做一次真跃迁（真写 `task_change`），并按服务层的口径推一次 revision。
    fn transition(
        &self,
        task_id: &str,
        expected_version: i64,
        to: TaskStatus,
        cause: TransitionCause,
        at: i64,
    ) {
        let tx = self.db.connection().unchecked_transaction().unwrap();
        task_repo::transition_task(&tx, task_id, expected_version, to, cause, at).unwrap();
        bump_revision(&tx).unwrap();
        tx.commit().unwrap();
    }

    /// 直接写一条 `task_change`：用来造**别的形状**的审计行（标签集合 / 今日计划集合）。
    fn audit_row(&self, id: &str, task_id: &str, before_json: &str, after_json: &str, at: i64) {
        self.db
            .connection()
            .execute(
                "INSERT INTO task_change(id,task_id,before_json,after_json,created_at)
                 VALUES(?1,?2,?3,?4,?5)",
                rusqlite::params![id, task_id, before_json, after_json, at],
            )
            .unwrap();
    }

    fn range_query(&self, from: i64, to: i64, timezone: &str) -> StatsRangeQuery {
        StatsRangeQuery {
            from,
            to,
            timezone: timezone.to_string(),
            expected_data_epoch: self.epoch.clone(),
        }
    }

    /// 与生产入口同一条取数路径：一次样本 + 一次范围报表（周回顾的数字要对齐的就是它）。
    fn report(&mut self, from: i64, to: i64, timezone: &str) -> RangeReport {
        let query = self.range_query(from, to, timezone);
        let sample = self.coord.stats_sample(&mut self.db).unwrap();
        stats::snapshot(&self.db, sample, &query)
            .unwrap()
            .report()
            .unwrap()
    }

    /// 与生产入口同一条取数路径：一次样本 + 一份 Markdown 周回顾。
    fn weekly(
        &mut self,
        anchor: Option<i64>,
        timezone: &str,
        generated_at: i64,
    ) -> export::ExportMarkdown {
        let query = WeeklyQuery {
            timezone: timezone.to_string(),
            anchor,
            expected_data_epoch: self.epoch.clone(),
        };
        let sample = self.coord.stats_sample(&mut self.db).unwrap();
        export::weekly(&self.db, sample, &query, generated_at).expect("周回顾应当生成")
    }

    fn weekly_text(&mut self, anchor: Option<i64>, timezone: &str, generated_at: i64) -> String {
        self.weekly(anchor, timezone, generated_at).text
    }

    fn revision(&self) -> i64 {
        require_meta(self.db.connection()).unwrap().revision
    }

    /// 这条连接累计写过的行数（只读断言的判据，与 `tests/export_json.rs` 同一写法）。
    fn total_changes(&self) -> i64 {
        self.db
            .connection()
            .query_row("SELECT total_changes()", [], |r| r.get(0))
            .unwrap()
    }

    /// 一次 Markdown 周回顾写过多少行（0 = 只读）。
    fn weekly_writes(&mut self, anchor: Option<i64>, timezone: &str) -> i64 {
        let before = self.total_changes();
        let _ = self.weekly(anchor, timezone, SH_NOON);
        self.total_changes() - before
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ① 周界：周一 → 下周一，半开，按查询时区
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_week_runs_from_monday_to_the_next_monday_half_open_in_the_query_timezone() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    // 周二 09:00–10:00 的一条已确认前台区间。
    h.session(
        "s-a",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        WEEK_A + DAY + 9 * HOUR,
    );
    h.interval(
        "i-a",
        "s-a",
        WEEK_A + DAY + 9 * HOUR,
        Some(WEEK_A + DAY + 10 * HOUR),
        Some(HOUR),
        0,
        None,
    );

    let text = h.weekly_text(Some(SH_NOON), "Asia/Shanghai", SH_NOON);

    assert!(
        text.starts_with("# 周回顾 2026-03-09 至 2026-03-16\n"),
        "标题就是这一周：\n{text}"
    );
    assert_eq!(line_with(&text, "时区："), "- 时区：Asia/Shanghai");
    assert_eq!(
        line_with(&text, "周界："),
        "- 周界：2026-03-09（周一）00:00 至 2026-03-16（周一）00:00（半开：含起、不含止）"
    );
    assert_eq!(
        line_with(&text, "周界（Unix 毫秒）："),
        "- 周界（Unix 毫秒）：[1772985600000, 1773590400000)"
    );

    // 数字：本周人工 1 小时，落在 03-10 那一天的日桶里；其余各日 0。
    assert_eq!(ms_in_line(line_with(&text, "本周合计：")), HOUR);
    let section_one = section(&text, "## 一、人工投入");
    assert_eq!(
        ms_in_line(line_starting_with(section_one, "| 2026-03-10 |")),
        HOUR
    );
    assert_eq!(
        ms_in_line(line_starting_with(section_one, "| 2026-03-09 |")),
        0,
        "周一没有工时"
    );
    assert_eq!(
        section_one
            .lines()
            .filter(|line| line.starts_with("| 2026-"))
            .count(),
        7,
        "一周固定七天（真实日界）"
    );

    // 半开范围：周一 00:00 那一刻属于本周（含起），下周一 00:00 那一刻属于下一周（不含止）。
    let at_monday = h.weekly_text(Some(WEEK_A), "Asia/Shanghai", SH_NOON);
    assert!(at_monday.starts_with("# 周回顾 2026-03-09 至 2026-03-16\n"));
    let last_ms = h.weekly_text(Some(WEEK_B - 1), "Asia/Shanghai", SH_NOON);
    assert!(
        last_ms.starts_with("# 周回顾 2026-03-09 至 2026-03-16\n"),
        "下周一的前一毫秒仍在上一周"
    );
    let next_monday = h.weekly_text(Some(WEEK_B), "Asia/Shanghai", SH_NOON);
    assert!(next_monday.starts_with("# 周回顾 2026-03-16 至 2026-03-23\n"));

    // 只读：生成一份周回顾一行都不写。
    assert_eq!(h.weekly_writes(Some(SH_NOON), "Asia/Shanghai"), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// ② 跨周的记录：按真实日界裁剪，两段各归其周、求和等于原区间
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn records_crossing_the_week_boundary_are_clipped_into_the_week_that_owns_them() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    // 周日 23:50 → 周一 00:10（横跨周界，共 20 分钟）。
    let start = WEEK_B - 10 * MINUTE;
    let end = WEEK_B + 10 * MINUTE;
    h.session("s-a", "t-a", "FOREGROUND", "finished", 0, start);
    h.interval("i-a", "s-a", start, Some(end), Some(20 * MINUTE), 0, None);

    let week_a = h.weekly_text(Some(SH_NOON), "Asia/Shanghai", SH_NOON);
    let week_b = h.weekly_text(Some(WEEK_B + 12 * HOUR), "Asia/Shanghai", SH_NOON);

    // 周 A：只拿到周日的 10 分钟。
    assert_eq!(ms_in_line(line_with(&week_a, "本周合计：")), 10 * MINUTE);
    let days_a = section(&week_a, "## 一、人工投入");
    assert_eq!(
        ms_in_line(line_starting_with(days_a, "| 2026-03-15 |")),
        10 * MINUTE,
        "周日那一半归周 A"
    );
    assert_eq!(ms_in_line(line_starting_with(days_a, "| 2026-03-14 |")), 0);

    // 周 B：只拿到周一的 10 分钟。
    assert_eq!(ms_in_line(line_with(&week_b, "本周合计：")), 10 * MINUTE);
    let days_b = section(&week_b, "## 一、人工投入");
    assert_eq!(
        ms_in_line(line_starting_with(days_b, "| 2026-03-16 |")),
        10 * MINUTE,
        "周一那一半归周 B"
    );
    assert_eq!(ms_in_line(line_starting_with(days_b, "| 2026-03-17 |")), 0);

    // 不丢不重：两周之和 == 原区间时长。
    assert_eq!(
        ms_in_line(line_with(&week_a, "本周合计：")) + ms_in_line(line_with(&week_b, "本周合计：")),
        20 * MINUTE
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ③ 完成项按完成事件时刻入周（改过 updated_at 不影响）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_completion_is_attributed_by_its_event_time_not_by_the_task_row() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    // 完成事件：2026-03-10 09:00（周 A）。
    let completed_at = WEEK_A + DAY + 9 * HOUR;
    h.transition(
        "t-a",
        0,
        TaskStatus::Done,
        TransitionCause::User,
        completed_at,
    );
    // 之后任务行被改过：`updated_at` 挪到周 B（2026-03-17）。
    h.db.connection()
        .execute(
            "UPDATE task SET updated_at = ?1 WHERE id = 't-a'",
            rusqlite::params![WEEK_B + DAY],
        )
        .unwrap();

    let week_a = h.weekly_text(Some(SH_NOON), "Asia/Shanghai", SH_NOON);
    let completed_a = section(&week_a, "## 二、完成任务");
    assert_eq!(
        completed_a
            .lines()
            .filter(|line| line.starts_with("| 2026-"))
            .count(),
        1,
        "周 A 只有这一条完成记录"
    );
    assert_eq!(
        line_starting_with(completed_a, "| 2026-03-10 |"),
        "| 2026-03-10 | 写文档 | 已完成 |"
    );

    // `updated_at` 所在的那一周（周 B）里**没有**这条完成记录。
    let week_b = h.weekly_text(Some(WEEK_B + DAY), "Asia/Shanghai", SH_NOON);
    assert!(section(&week_b, "## 二、完成任务").contains("本周没有完成记录。"));
    assert!(
        !week_b.contains("写文档"),
        "完成项不跟着 `updated_at` 走：\n{week_b}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ④ 同一任务「完成 → 重开 → 再完成」⇒ 两周各出现一次（G3）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_task_completed_reopened_and_completed_again_appears_in_both_weeks() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    h.task("t-b", "改代码", 2_000);

    // t-a：周 A 完成 → 周 B 重开 → 周 B 再完成（两个 Done 事件）。
    h.transition(
        "t-a",
        0,
        TaskStatus::Done,
        TransitionCause::User,
        WEEK_A + DAY + 9 * HOUR,
    );
    h.transition(
        "t-a",
        1,
        TaskStatus::Ready,
        TransitionCause::Reopen,
        WEEK_B + DAY + 9 * HOUR,
    );
    h.transition(
        "t-a",
        2,
        TaskStatus::Done,
        TransitionCause::User,
        WEEK_B + 2 * DAY + 9 * HOUR,
    );
    // t-b：周 A 完成 → 周 B 重开，**没有**再完成（周 A 的那条不该被说成「仍然完成」）。
    h.transition(
        "t-b",
        0,
        TaskStatus::Done,
        TransitionCause::User,
        WEEK_A + 2 * DAY + 9 * HOUR,
    );
    h.transition(
        "t-b",
        1,
        TaskStatus::Ready,
        TransitionCause::Reopen,
        WEEK_B + 3 * DAY + 9 * HOUR,
    );

    let week_a = h.weekly_text(Some(SH_NOON), "Asia/Shanghai", SH_NOON);
    let week_b = h.weekly_text(Some(WEEK_B + DAY), "Asia/Shanghai", SH_NOON);

    // 同一个任务因此出现在**两周**的回顾里，各带自己的完成日期。
    let completed_a = section(&week_a, "## 二、完成任务");
    let completed_b = section(&week_b, "## 二、完成任务");
    assert_eq!(
        line_starting_with(completed_a, "| 2026-03-10 |"),
        "| 2026-03-10 | 写文档 | 已完成 |"
    );
    assert_eq!(
        line_starting_with(completed_b, "| 2026-03-18 |"),
        "| 2026-03-18 | 写文档 | 已完成 |"
    );
    assert_eq!(
        completed_a
            .lines()
            .filter(|line| line.starts_with("| 2026-"))
            .count(),
        2,
        "周 A 有两条完成记录（t-a 与 t-b）"
    );
    assert_eq!(
        completed_b
            .lines()
            .filter(|line| line.starts_with("| 2026-"))
            .count(),
        1,
        "重开本身不是完成事件 ⇒ 周 B 只有 t-a 的第二次完成"
    );
    // 02 §10：完成之后被重开的，不许在回顾里说成仍然完成。
    assert_eq!(
        line_starting_with(completed_a, "| 2026-03-11 |"),
        "| 2026-03-11 | 改代码 | 已重新打开 |"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑤ 按 JSON 形状过滤：贴标签 / 加今日计划 / 建任务都不是完成事件
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn only_status_done_events_count_as_completions_not_tag_or_daily_plan_rows() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    // 同一周里的三种形状（`task_change` 的三种形状见 `src/storage/mod.rs`）：
    // ① 任务字段形状（建任务：status=Inbox）；② 标签集合形状；③ 今日计划集合形状。
    h.audit_row(
        "c-1",
        "t-a",
        "{}",
        "{\"status\":\"Inbox\",\"title\":\"写文档\"}",
        WEEK_A + 8 * HOUR,
    );
    h.audit_row(
        "c-2",
        "t-a",
        "{\"tags\":[]}",
        "{\"tags\":[\"tg-1\"]}",
        WEEK_A + 9 * HOUR,
    );
    h.audit_row(
        "c-3",
        "t-a",
        "{\"daily_plan\":[]}",
        "{\"daily_plan\":[{\"local_date\":\"2026-03-10\",\"timezone\":\"Asia/Shanghai\"}]}",
        WEEK_A + 10 * HOUR,
    );
    // 只有这一条是「status → Done」的完成事件。
    h.transition(
        "t-a",
        0,
        TaskStatus::Done,
        TransitionCause::User,
        WEEK_A + DAY + 9 * HOUR,
    );

    let week_a = h.weekly_text(Some(SH_NOON), "Asia/Shanghai", SH_NOON);
    let completed = section(&week_a, "## 二、完成任务");
    let rows: Vec<&str> = completed
        .lines()
        .filter(|line| line.starts_with("| 2026-"))
        .collect();
    assert_eq!(
        rows,
        vec!["| 2026-03-10 | 写文档 | 已完成 |"],
        "只有 status→Done 的事件算完成：贴标签 / 加今日计划 / 建任务都不算"
    );
    assert!(completed.contains("完成记录 1 条。"));
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑥ 待确认：单独成节，不进人工合计
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn pending_candidates_are_listed_separately_and_never_join_the_human_total() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    // 已确认：周二 09:00–10:00（1 小时）。
    h.session(
        "s-a",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        WEEK_A + DAY + 9 * HOUR,
    );
    h.interval(
        "i-a",
        "s-a",
        WEEK_A + DAY + 9 * HOUR,
        Some(WEEK_A + DAY + 10 * HOUR),
        Some(HOUR),
        0,
        None,
    );
    // 待确认候选（已知端点，10 分钟）：同一周里的另一条前台记录。
    h.session(
        "s-p",
        "t-a",
        "FOREGROUND",
        "recovering",
        1,
        WEEK_A + DAY + 11 * HOUR,
    );
    h.interval(
        "i-p",
        "s-p",
        WEEK_A + DAY + 11 * HOUR,
        Some(WEEK_A + DAY + 11 * HOUR + 10 * MINUTE),
        None,
        1,
        None,
    );

    let text = h.weekly_text(Some(SH_NOON), "Asia/Shanghai", SH_NOON);
    let report = h.report(WEEK_A, WEEK_B, "Asia/Shanghai");

    // 人工合计 == 报表的「已确认 / 人工」列（同一套取数），**不含**那 10 分钟候选。
    assert_eq!(
        ms_in_line(line_with(&text, "本周合计：")),
        report
            .column(StatsClass::Confirmed, Measure::Human)
            .ms
            .unwrap()
    );
    assert_eq!(ms_in_line(line_with(&text, "本周合计：")), HOUR);
    assert!(line_with(&text, "本周合计：").contains("共 1 条已确认区间"));

    // 待确认单独成节：条数与已知端点跨度都来自报表的待确认列。
    let pending = section(&text, "## 三、待确认记录");
    assert!(pending.contains("不计入"), "必须写明不并入人工合计");
    assert_eq!(
        report.column(StatsClass::Pending, Measure::Human).intervals,
        1
    );
    assert_eq!(
        line_starting_with(pending, "| 人工 |"),
        "| 人工 | 1 | 10 分钟（600000 毫秒） |"
    );
    // 候选记录本身也列出来（F-018 的「待确认记录」），一条。
    let records: Vec<&str> = pending
        .lines()
        .filter(|line| line.starts_with("| 2026-"))
        .collect();
    assert_eq!(records, vec!["| 2026-03-10 | 2026-03-10 | 人工 | 写文档 |"]);
    // 没有被作废的候选：机器三类都没有候选。
    assert_eq!(
        line_starting_with(pending, "| 机器（后台） |"),
        "| 机器（后台） | 0 | 无候选 |"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑦ 同一个周重复生成：除生成时间外逐字节一致
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn two_generations_of_the_same_week_are_byte_identical_except_for_the_generated_time() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    h.session(
        "s-a",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        WEEK_A + DAY + 9 * HOUR,
    );
    h.interval(
        "i-a",
        "s-a",
        WEEK_A + DAY + 9 * HOUR,
        Some(WEEK_A + DAY + 10 * HOUR),
        Some(HOUR),
        0,
        None,
    );
    h.transition(
        "t-a",
        0,
        TaskStatus::Done,
        TransitionCause::User,
        WEEK_A + DAY + 9 * HOUR,
    );
    h.session(
        "s-p",
        "t-a",
        "FOREGROUND",
        "recovering",
        1,
        WEEK_A + 2 * DAY,
    );
    h.interval(
        "i-p",
        "s-p",
        WEEK_A + 2 * DAY,
        Some(WEEK_A + 2 * DAY + 10 * MINUTE),
        None,
        1,
        None,
    );

    let first = h.weekly(Some(SH_NOON), "Asia/Shanghai", SH_NOON);
    let second = h.weekly(Some(SH_NOON), "Asia/Shanghai", SH_NOON);
    assert_eq!(
        first.text, second.text,
        "同一输入两次生成必须逐字节一致（含生成时间）"
    );

    let later = h.weekly(Some(SH_NOON), "Asia/Shanghai", SH_NOON + 1);
    assert_ne!(later.text, first.text, "生成时间不同 ⇒ 文本不同");
    assert_eq!(
        later
            .text
            .replace(&(SH_NOON + 1).to_string(), &SH_NOON.to_string()),
        first.text,
        "除生成时间外没有任何字节随调用变化"
    );
    // 信封里的周与版本不变。
    assert_eq!(later.week_start, "2026-03-09");
    assert_eq!(later.week_end, "2026-03-16");
    assert_eq!(later.range.from, WEEK_A);
    assert_eq!(later.range.to, WEEK_B);
    assert_eq!(later.timezone, "Asia/Shanghai");
    assert_eq!(later.data_epoch, h.epoch);
    assert_eq!(later.revision, h.revision());
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑧ 空周：仍然是一份合法文档
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_empty_week_is_still_a_valid_document() {
    let mut h = setup(SH_NOON);
    let text = h.weekly_text(Some(SH_NOON), "Asia/Shanghai", SH_NOON);

    for heading in ["## 一、人工投入", "## 二、完成任务", "## 三、待确认记录"] {
        assert!(text.contains(heading), "空周也要有三节：{heading}\n{text}");
    }
    assert_eq!(ms_in_line(line_with(&text, "本周合计：")), 0);
    assert!(line_with(&text, "本周合计：").contains("共 0 条已确认区间"));

    let human = section(&text, "## 一、人工投入");
    let days: Vec<&str> = human
        .lines()
        .filter(|line| line.starts_with("| 2026-"))
        .collect();
    assert_eq!(days.len(), 7, "空周仍然逐日列出七天");
    for day in &days {
        assert_eq!(ms_in_line(day), 0, "空周每一天都是 0：{day}");
    }

    let completed = section(&text, "## 二、完成任务");
    assert!(completed.contains("本周没有完成记录。"));
    assert_eq!(
        completed
            .lines()
            .filter(|line| line.starts_with("| 2026-"))
            .count(),
        0
    );

    let pending = section(&text, "## 三、待确认记录");
    assert!(pending.contains("本周没有待确认记录。"));
    // 待确认四类固定出现；一条候选都没有时跨度一栏写「无候选」，不是 0（不推算）。
    for label in [
        "| 人工 |",
        "| 机器（后台） |",
        "| 机器（被动） |",
        "| 等待 |",
    ] {
        assert_eq!(
            line_starting_with(pending, label),
            format!("{label} 0 | 无候选 |")
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑨ 只陈述事实：不含推断性措辞（04 的 F-206）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_review_states_facts_only_and_never_claims_ai_conclusions() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    h.session(
        "s-a",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        WEEK_A + DAY + 9 * HOUR,
    );
    h.interval(
        "i-a",
        "s-a",
        WEEK_A + DAY + 9 * HOUR,
        Some(WEEK_A + DAY + 10 * HOUR),
        Some(HOUR),
        0,
        None,
    );
    h.transition(
        "t-a",
        0,
        TaskStatus::Done,
        TransitionCause::User,
        WEEK_A + DAY + 9 * HOUR,
    );
    h.session(
        "s-p",
        "t-a",
        "FOREGROUND",
        "recovering",
        1,
        WEEK_A + 2 * DAY,
    );
    h.interval(
        "i-p",
        "s-p",
        WEEK_A + 2 * DAY,
        Some(WEEK_A + 2 * DAY + 10 * MINUTE),
        None,
        1,
        None,
    );

    let text = h.weekly_text(Some(SH_NOON), "Asia/Shanghai", SH_NOON);

    // 不查裸子串「AI」：会话模式名 `WAITING` 里就有它（那是规范里的术语，不是推断）。
    for forbidden in [
        "效率",
        "节省",
        "省下",
        "提升",
        "智能",
        "预测",
        "建议",
        "评估收益",
        "采纳率",
        "模型",
    ] {
        assert!(
            !text.contains(forbidden),
            "周回顾只陈述事实与已确认数字，不许出现「{forbidden}」这类推断：\n{text}"
        );
    }
    // 口径必须写出来（读者要知道数字是怎么来的）。
    assert!(text.contains("口径："));
    assert!(line_with(&text, "本周合计：").contains("已确认"));
    assert!(section(&text, "## 二、完成任务").contains("task_change"));
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑩ 夏令时周按真实日界（167 小时），不是 7 × 24 小时
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_dst_week_is_bounded_by_real_midnights_not_by_168_hours() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    // 2026-03-08（周日）01:30 EST → 03:30 EDT：本地钟面跨 2 小时，**真实**只有 1 小时。
    let start = 1_772_951_400_000;
    let end = 1_772_955_000_000;
    h.session("s-a", "t-a", "FOREGROUND", "finished", 0, start);
    h.interval("i-a", "s-a", start, Some(end), Some(HOUR), 0, None);

    let text = h.weekly_text(Some(NY_NOON), "America/New_York", SH_NOON);

    assert!(text.starts_with("# 周回顾 2026-03-02 至 2026-03-09\n"));
    let (from, to) = week_range_ms(&text);
    assert_eq!(from, NY_WEEK);
    assert_eq!(to, NY_NEXT);
    assert_eq!(
        to - from,
        167 * HOUR,
        "夏令时切换周只有 167 小时（真实日界，不是 7 × 24）"
    );
    let human = section(&text, "## 一、人工投入");
    assert_eq!(
        human
            .lines()
            .filter(|line| line.starts_with("| 2026-"))
            .count(),
        7
    );
    // 那一段真实 1 小时落在切换当天（周日）的日桶里。
    assert_eq!(
        ms_in_line(line_starting_with(human, "| 2026-03-08 |")),
        HOUR
    );
    assert_eq!(ms_in_line(line_with(&text, "本周合计：")), HOUR);
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑪ 生产入口：真启动 + `AppState::export_weekly_markdown`
// ─────────────────────────────────────────────────────────────────────────────

struct AppFixture {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    lock_path: PathBuf,
}

fn app_fixture() -> AppFixture {
    let dir = tempfile::tempdir().unwrap();
    let fixture = AppFixture {
        db_path: dir.path().join("worktrace.db"),
        lock_path: dir.path().join("instance.lock"),
        _dir: dir,
    };
    let db = Db::open(&fixture.db_path).unwrap();
    migrate(db.connection()).unwrap();
    db.connection()
        .execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES('t1','任务','Doing',0,1000,1000)",
            [],
        )
        .unwrap();
    drop(db);
    fixture
}

fn app_started(fixture: &AppFixture, clock: Arc<Mutex<FakeClock>>) -> Box<RunningApp> {
    let config = StartupConfig {
        db_path: fixture.db_path.clone(),
        lock_path: fixture.lock_path.clone(),
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

fn session_request(guard: &mut AppState, epoch: &str) -> SessionRequest {
    let snapshot = guard.snapshot().unwrap();
    SessionRequest {
        expected_data_epoch: epoch.to_string(),
        session_id: snapshot.session_id.clone().unwrap(),
        session_expected_version: snapshot.session_version.unwrap(),
    }
}

/// 周回顾的请求（`anchor` 省略 = 「本周」，由同一次样本的水位决定）。
fn weekly_query(epoch: &str, anchor: Option<i64>) -> WeeklyQuery {
    WeeklyQuery {
        timezone: "Asia/Shanghai".to_string(),
        anchor,
        expected_data_epoch: epoch.to_string(),
    }
}

#[test]
fn the_app_state_entry_point_renders_the_same_week_as_the_service() {
    let fixture = app_fixture();
    let wall = SH_NOON;
    let clock = Arc::new(Mutex::new(FakeClock::new(wall, 0)));
    let running = app_started(&fixture, Arc::clone(&clock));
    let epoch = running.data_epoch().to_string();
    let app = Arc::clone(running.app());

    // 真起一次计时、走满 2 分钟、再结束 ⇒ 一条 2 分钟的已确认前台区间。
    {
        let mut guard = lock_app(&app);
        guard.start(start_request(&epoch)).unwrap();
    }
    for _ in 0..2 {
        clock.lock().unwrap().advance_both(MINUTE);
        let mut guard = lock_app(&app);
        guard.sample_tick().unwrap();
    }
    {
        let mut guard = lock_app(&app);
        let request = session_request(&mut guard, &epoch);
        guard.finish(request).unwrap();
    }

    let before_changes = {
        let guard = lock_app(&app);
        guard
            .db()
            .connection()
            .query_row("SELECT total_changes()", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let before_revision = {
        let guard = lock_app(&app);
        require_meta(guard.db().connection()).unwrap().revision
    };

    let exported = {
        let mut guard = lock_app(&app);
        guard
            .export_weekly_markdown(&weekly_query(&epoch, None))
            .expect("生产入口必须能生成周回顾")
    };
    let text = exported.text.clone();

    // 「本周」= 样本水位所在的那一周（2026-03-10 ⇒ 2026-03-09 → 2026-03-16）。
    assert!(text.starts_with("# 周回顾 2026-03-09 至 2026-03-16\n"));
    assert_eq!(ms_in_line(line_with(&text, "本周合计：")), 2 * MINUTE);
    assert_eq!(exported.week_start, "2026-03-09");
    assert_eq!(exported.week_end, "2026-03-16");
    assert_eq!(exported.range.from, WEEK_A);
    assert_eq!(exported.range.to, WEEK_B);
    assert_eq!(exported.timezone, "Asia/Shanghai");
    assert_eq!(exported.data_epoch, epoch);
    assert_eq!(exported.revision, before_revision);
    // 两个时间水位都在文档里，且**由调用方（平台时钟接缝）给出**。
    assert!(
        line_with(&text, "生成时间（generated_at）：").contains(&(wall + 2 * MINUTE).to_string())
    );
    assert!(line_with(&text, "数据截至（as_of）：").contains(&(wall + 2 * MINUTE).to_string()));

    // 只读：生产入口同样一行都不写。
    let after_changes = {
        let guard = lock_app(&app);
        guard
            .db()
            .connection()
            .query_row("SELECT total_changes()", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(after_changes, before_changes, "周回顾不写库");

    // 同一个周重复生成：除时间水位外逐字节一致。
    clock.lock().unwrap().advance_both(MINUTE);
    let later = {
        let mut guard = lock_app(&app);
        guard
            .export_weekly_markdown(&weekly_query(&epoch, None))
            .expect("第二次生成")
    };
    assert_ne!(later.text, text, "生成时间前进 ⇒ 文本不同");
    let marks = [wall + 2 * MINUTE, wall + 3 * MINUTE];
    assert_eq!(
        blank_watermarks(&later.text, &marks),
        blank_watermarks(&text, &marks),
        "除 time 水位（generated_at / as_of）外两份周回顾逐字节一致"
    );
    assert_eq!(later.revision, before_revision, "纯读不前进 revision");
}

#[test]
fn the_weekly_query_defaults_to_this_week_when_the_anchor_is_omitted() {
    // P8 以后要从 IPC 反序列化这个请求：省略 `anchor` 必须是合法的「本周」。
    let query: WeeklyQuery =
        serde_json::from_str(r#"{"timezone":"Asia/Shanghai","expected_data_epoch":"epoch-1"}"#)
            .expect("省略 anchor 的请求必须能反序列化");
    assert_eq!(query.anchor, None);
    assert_eq!(query.timezone, "Asia/Shanghai");

    let with_anchor: WeeklyQuery = serde_json::from_str(
        r#"{"timezone":"utc","anchor":1773028800000,"expected_data_epoch":"epoch-1"}"#,
    )
    .unwrap();
    assert_eq!(with_anchor.anchor, Some(1_773_028_800_000));
}
