//! P5 Task 5：**端到端一致性**——Today、JSON 明细导出、Markdown 周回顾在**真库 + 真入口**上
//! 必须对同一批区间给出同一组数字（F-010 / F-018；Ruling P5-3 的「库明细 ↔ 服务输出」那半边）。
//!
//! # 这个文件在验什么
//!
//! 装置照 `tests/recovery_end_to_end.rs`：先用裸 SQL 造一组**真实形状**的历史事实
//! （四种会话模式、跨午夜含暂停、上一周、待确认、已作废、已丢弃），再走**真实**
//! [`startup`]（第 ④ 步的扫描会做四类判定），然后在同一把锁（[`lock_app`]）里用
//! `AppState` 的读入口取三种视图：
//!
//! ```text
//! AppState::stats_today             → TodayView（F-010 的五项，按今天）
//! AppState::export_json             → JSON 明细（今天 / 本周两个范围）
//! AppState::export_weekly_markdown  → Markdown 周回顾（周一 → 下周一，半开）
//! AppState::stats_snapshot + report → 范围报表（三者的共同取数层）
//! ```
//!
//! **每个数字都要能追到区间**：断言先写出「这个数字由哪几条区间、各贡献多少」的算式，
//! 再用一个**独立 oracle**核对——测试自己直连 SQL 读 `work_interval`，按 02 §6 的
//! `max(0, min(end,to) - max(start,from))` 手算。oracle **刻意不调用**被测的
//! `IntervalRange::clipped_ms`：拿实现证明实现等于没证明。
//!
//! # 人工验收的另一半（Ruling P5-3，**不在本文件**）
//!
//! 计划 Task 5 原文要求「在真实界面上核对 Today 的五项数字、导出后用外部工具重算」——
//! 那需要 **P8 的界面**，本计划不冒充。本文件做的是它能做的那一半：
//! 「**库明细 ↔ 服务输出**」的交叉核对（真库 + 真 `startup()` + 真 `AppState` 入口）。
//!
//! # 只读
//!
//! 统计与导出**不写库、不加 `revision`**；本文件唯一的写命令是 P3 的
//! `AppState::correct`（F-017 的「修正后一致」），它按自己的口径恰好 +1 `revision`。
//! 夹具（`seeded`）里的裸 SQL 是**装置**，不是被测路径——`tests/recovery_end_to_end.rs`
//! 的崩溃现场也是这么造的。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rusqlite::Transaction;
use serde_json::Value;

use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, AppGuard, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::export::WeeklyQuery;
use worktrace_lib::services::history::{CorrectAction, CorrectRequest};
use worktrace_lib::services::stats::{
    Measure, MeasureColumn, StatsClass, StatsRange, StatsRangeQuery, TodayQuery, TodayView,
};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::run_repo;

// ─────────────────────────────────────────────────────────────────────────────
// 时间的来历（全部是上海时区的真实时刻，夏令时无关——上海 2026 年没有切换）
// ─────────────────────────────────────────────────────────────────────────────

/// 查询时区：与全部日界、周界、日桶同一套（G4 只有这一个入口）。
const TZ: &str = "Asia/Shanghai";

/// 上一周的周一 2026-03-02 00:00（+08:00）。
const PREV_MON_MID: i64 = 1_772_380_800_000;
/// 本周的周一 2026-03-09 00:00 —— 周回顾的半开起点。
const MON_MID: i64 = 1_772_985_600_000;
/// 今天 2026-03-10（周二）00:00 —— Today 的半开起点。
const TUE_MID: i64 = 1_773_072_000_000;
/// 明天 2026-03-11 00:00 —— Today 的半开终点。
const WED_MID: i64 = 1_773_158_400_000;
/// 下周一 2026-03-16 00:00 —— 周回顾的半开终点。
const NEXT_MON_MID: i64 = 1_773_590_400_000;
/// 启动那一刻的挂钟：2026-03-10 12:00。`FakeClock` 不自己走 ⇒ 全部 `as_of` 都是它。
const WALL: i64 = 1_773_115_200_000;

const MIN: i64 = 60_000;
const HOUR: i64 = 3_600_000;

/// 崩掉的那一代次：夹具的事实都挂在它上面（本次 run 由 `startup` 现建）。
const OLD_RUN: &str = "run-old";

// 每段区间的几何（`seeded` 就是照这些值落库的；断言里的每个数字都从它们来）。

/// 跨午夜会话 `s-cross`：23:50 → 00:00（第 1 段，落在 03-09）→ **暂停跨过午夜** →
/// 00:10 → 00:20（第 2 段，落在 03-10）。两段各 10 分钟：02 §8 的「两天各 10 分钟」。
const CROSS_A_START: i64 = TUE_MID - 10 * MIN;
const CROSS_B_START: i64 = TUE_MID + 10 * MIN;
const CROSS_B_END: i64 = TUE_MID + 20 * MIN;
/// 今天的人工：09:00 → 10:00（1 小时）。
const HUMAN_START: i64 = TUE_MID + 9 * HOUR;
const HUMAN_END: i64 = TUE_MID + 10 * HOUR;
/// `correct` 之后的人工区间终点（收短到 30 分钟）。
const HUMAN_SHORT_END: i64 = TUE_MID + 9 * HOUR + 30 * MIN;
/// 与人工**完全并行**的 1 小时后台（F-103 的核心：不许加成 2 小时人工）。
const BG_START: i64 = HUMAN_START;
const BG_END: i64 = HUMAN_END;
/// 并行的 15 分钟被动采集。
const PASS_START: i64 = TUE_MID + 9 * HOUR + 30 * MIN;
const PASS_END: i64 = TUE_MID + 9 * HOUR + 45 * MIN;
/// 5 分钟等待。
const WAIT_START: i64 = TUE_MID + 10 * HOUR;
const WAIT_END: i64 = TUE_MID + 10 * HOUR + 5 * MIN;
/// 待确认候选：08:00 → 08:10（端点已知、`duration_ms` 为 NULL ⇒ 不是事实）。
const PEND_START: i64 = TUE_MID + 8 * HOUR;
const PEND_END: i64 = TUE_MID + 8 * HOUR + 10 * MIN;
/// 已作废的 1 小时（`correct:delete` 的形状）：一个字都不该进任何数字。
const VOID_START: i64 = TUE_MID + 11 * HOUR;
const VOID_END: i64 = TUE_MID + 12 * HOUR;
/// 已丢弃整次会话的 1 小时（`discarded`）：同上。
const DISC_START: i64 = TUE_MID + 6 * HOUR;
const DISC_END: i64 = TUE_MID + 7 * HOUR;
/// 上一周的 1 小时：只该出现在**上一周**的周回顾里。
const PREV_START: i64 = PREV_MON_MID + 9 * HOUR;
const PREV_END: i64 = PREV_MON_MID + 10 * HOUR;
/// 完成事件（`task_change` 里 `status` 变为 `Done` 的那一条）的时刻。
const DONE_AT: i64 = TUE_MID + 11 * HOUR;

// 期望值（写成算式，评审可以直接对照上面每段区间的几何）。

/// 今天已确认人工 = 跨午夜第 2 段 10 分钟 + 人工 1 小时 = 70 分钟。
const TODAY_CONFIRMED_HUMAN: i64 = 600_000 + 3_600_000;
/// 本周已确认人工 = 跨午夜第 1 段（03-09）10 分钟 + 第 2 段（03-10）10 分钟 + 人工 1 小时 = 80 分钟。
const WEEK_CONFIRMED_HUMAN: i64 = 600_000 + 600_000 + 3_600_000;
/// `correct` 把人工从 1 小时收短到 30 分钟之后：
const TODAY_CONFIRMED_HUMAN_AFTER_CORRECT: i64 = 600_000 + 1_800_000;
const WEEK_CONFIRMED_HUMAN_AFTER_CORRECT: i64 = 600_000 + 600_000 + 1_800_000;
/// `correct` 把时钟也往前推了这么久（`as_of` 必须跟着前进）。
const CORRECT_ELAPSED: i64 = 5 * MIN;

// ─────────────────────────────────────────────────────────────────────────────
// 装置
// ─────────────────────────────────────────────────────────────────────────────

/// 忽略全部事件的出口：统计用例不关心广播，但 `startup` 需要一个。
struct NoSink;

impl EventSink for NoSink {
    fn broadcast(&self, _envelope: &EventEnvelope) -> Result<(), String> {
        Ok(())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    lock_path: PathBuf,
    /// 与协调器**共享**的假时钟：`startup` 拿走一个 `Arc` 句柄，测试留一个，
    /// 于是「推进时间」始终走平台时钟接缝，不另造第二个时间源。
    clock: Arc<Mutex<FakeClock>>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    Fixture {
        db_path: dir.path().join("worktrace.db"),
        lock_path: dir.path().join("instance.lock"),
        clock: Arc::new(Mutex::new(FakeClock::new(WALL, 0))),
        _dir: dir,
    }
}

/// 真启动：单实例 → 打开库/迁移 → 建本次 run → 恢复扫描 → 协调器 → 采样线程。
fn started(fx: &Fixture) -> Box<RunningApp> {
    let outcome = startup(
        StartupConfig::new(&fx.db_path, &fx.lock_path),
        Box::new(Arc::clone(&fx.clock)),
        Arc::new(NoSink),
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功");
    match outcome {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    }
}

/// 推进假时钟：**60 秒一片、每片一次真采样**。一次跳满会被判成 `Suspended`
/// 的长间隔（`HEARTBEAT_INTERVAL_MS × 3 = 90 秒`），那是异常分割路径，
/// 不是本文件要观察的东西（与 `tests/today.rs::elapse` 同一理由）。
fn elapse(fx: &Fixture, state: &mut AppGuard<'_>, total_ms: i64) {
    let mut left = total_ms;
    while left > 0 {
        let step = left.min(60_000);
        fx.clock.lock().unwrap().advance_both(step);
        state.sample_tick().expect("周期采样应当成功");
        left -= step;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 夹具：一组**真实形状**的历史事实（裸 SQL，装置用）
// ─────────────────────────────────────────────────────────────────────────────

/// 造历史：三种任务 + 今日计划 + 完成事件 + 九次会话（见每个 `session` 的注释）。
///
/// 全部挂在**上一代 run** 上——这正是「用户关掉应用、下次打开」的形状；
/// 本次 run 由 [`startup`] 现建，扫描会自己做四类判定（本夹具里没有需要归一的
/// `running` 现场，所以扫描一个字都不写）。
fn seeded(fx: &Fixture) {
    let mut db = Db::open(&fx.db_path).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    run_repo::start_run(&tx, OLD_RUN, WALL - 24 * HOUR).unwrap();

    for (id, title, status, created_at) in [
        ("t-doc", "写文档", "Done", 1_000_i64),
        ("t-code", "改代码", "Doing", 2_000),
        ("t-prev", "上周的任务", "Doing", 500),
    ] {
        tx.execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES(?1,?2,?3,0,?4,?4)",
            rusqlite::params![id, title, status, created_at],
        )
        .unwrap();
    }
    // 今日选择列表（Today 的第一项）：顺序由 `task.created_at, id` 决定。
    for task_id in ["t-doc", "t-code"] {
        tx.execute(
            "INSERT INTO daily_plan(task_id,local_date,timezone) VALUES(?1,'2026-03-10',?2)",
            rusqlite::params![task_id, TZ],
        )
        .unwrap();
    }
    // 完成事件：形状一是「任务字段形状」（`after_json` 里就是被改的列）。
    tx.execute(
        "INSERT INTO task_change(id,task_id,before_json,after_json,created_at)
         VALUES('c-done','t-doc','{\"status\":\"Doing\"}','{\"status\":\"Done\"}',?1)",
        rusqlite::params![DONE_AT],
    )
    .unwrap();

    // ① 跨午夜 + 暂停：23:50 → 00:00（03-09）／暂停跨过午夜／00:10 → 00:20（03-10）。
    session(
        &tx,
        "s-cross",
        "t-doc",
        "FOREGROUND",
        "finished",
        0,
        CROSS_A_START,
        Some(CROSS_B_END),
    );
    interval(
        &tx,
        "i-cross-a",
        "s-cross",
        CROSS_A_START,
        Some(TUE_MID),
        Some(10 * MIN),
        0,
        None,
    );
    interval(
        &tx,
        "i-cross-b",
        "s-cross",
        CROSS_B_START,
        Some(CROSS_B_END),
        Some(10 * MIN),
        0,
        None,
    );

    // ② 今天的人工 1 小时。
    session(
        &tx,
        "s-human",
        "t-doc",
        "FOREGROUND",
        "finished",
        0,
        HUMAN_START,
        Some(HUMAN_END),
    );
    interval(
        &tx,
        "i-human",
        "s-human",
        HUMAN_START,
        Some(HUMAN_END),
        Some(HOUR),
        0,
        None,
    );

    // ③ 与人工**完全并行**的机器：后台 1 小时 + 被动 15 分钟（并行不算进人工）。
    session(
        &tx,
        "s-bg",
        "t-code",
        "BACKGROUND",
        "finished",
        0,
        BG_START,
        Some(BG_END),
    );
    interval(
        &tx,
        "i-bg",
        "s-bg",
        BG_START,
        Some(BG_END),
        Some(HOUR),
        0,
        None,
    );
    session(
        &tx,
        "s-pass",
        "t-code",
        "PASSIVE",
        "finished",
        0,
        PASS_START,
        Some(PASS_END),
    );
    interval(
        &tx,
        "i-pass",
        "s-pass",
        PASS_START,
        Some(PASS_END),
        Some(15 * MIN),
        0,
        None,
    );

    // ④ 等待 5 分钟：单列，谁都不并。
    session(
        &tx,
        "s-wait",
        "t-code",
        "WAITING",
        "finished",
        0,
        WAIT_START,
        Some(WAIT_END),
    );
    interval(
        &tx,
        "i-wait",
        "s-wait",
        WAIT_START,
        Some(WAIT_END),
        Some(5 * MIN),
        0,
        None,
    );

    // ⑤ 待确认候选：会话与区间都标 `needs_review`，端点已知、`duration_ms` 为 NULL
    //    （候选端点不是事实）⇒ 只进「待确认」栏，不进任何「已确认」数字。
    session(
        &tx,
        "s-pend",
        "t-doc",
        "FOREGROUND",
        "finished",
        1,
        PEND_START,
        Some(PEND_END),
    );
    interval(
        &tx,
        "i-pend",
        "s-pend",
        PEND_START,
        Some(PEND_END),
        None,
        1,
        None,
    );

    // ⑥ 已作废的 1 小时（`correct:delete` 的形状：软删除，行还在）。
    session(
        &tx,
        "s-void",
        "t-code",
        "FOREGROUND",
        "finished",
        0,
        VOID_START,
        Some(VOID_END),
    );
    interval(
        &tx,
        "i-void",
        "s-void",
        VOID_START,
        Some(VOID_END),
        Some(HOUR),
        0,
        Some(VOID_END),
    );

    // ⑦ 已丢弃整次会话的 1 小时（`discarded`）：仓储那一条查询就把它排除了。
    session(
        &tx,
        "s-disc",
        "t-code",
        "FOREGROUND",
        "discarded",
        0,
        DISC_START,
        Some(DISC_END),
    );
    interval(
        &tx,
        "i-disc",
        "s-disc",
        DISC_START,
        Some(DISC_END),
        Some(HOUR),
        0,
        None,
    );

    // ⑧ 上一周的 1 小时：只该出现在**上一周**的周回顾里。
    session(
        &tx,
        "s-prev",
        "t-prev",
        "FOREGROUND",
        "finished",
        0,
        PREV_START,
        Some(PREV_END),
    );
    interval(
        &tx,
        "i-prev",
        "s-prev",
        PREV_START,
        Some(PREV_END),
        Some(HOUR),
        0,
        None,
    );

    tx.commit().unwrap();
}

/// 一行会话（`ended_at` 只对已结束的会话给值）。
#[allow(clippy::too_many_arguments)]
fn session(
    tx: &Transaction<'_>,
    id: &str,
    task_id: &str,
    mode: &str,
    state: &str,
    needs_review: i64,
    started_at: i64,
    ended_at: Option<i64>,
) {
    tx.execute(
        "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,ended_at,
                                  needs_review,row_version)
         VALUES(?1,?2,?3,?4,?5,'stopwatch',?6,?7,?8,0)",
        rusqlite::params![
            id,
            task_id,
            OLD_RUN,
            mode,
            state,
            started_at,
            ended_at,
            needs_review
        ],
    )
    .unwrap();
}

/// 一行区间事实（`duration_ms` 为 `None` 只对「待确认候选」合法——见 schema 的 CHECK）。
#[allow(clippy::too_many_arguments)]
fn interval(
    tx: &Transaction<'_>,
    id: &str,
    session_id: &str,
    started_at: i64,
    ended_at: Option<i64>,
    duration_ms: Option<i64>,
    needs_review: i64,
    voided_at: Option<i64>,
) {
    tx.execute(
        "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms,needs_review,voided_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![id, session_id, started_at, ended_at, duration_ms, needs_review, voided_at],
    )
    .unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────
// 独立 oracle：直连 SQL 的库明细 + 手算的 02 §6 公式
// ─────────────────────────────────────────────────────────────────────────────

/// 一条区间事实的库明细（**含**已作废与已丢弃的行——排除是被测口径，不是 oracle 的）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct DbInterval {
    id: String,
    mode: String,
    session_state: String,
    started_at: i64,
    ended_at: Option<i64>,
    duration_ms: Option<i64>,
    needs_review: bool,
    voided_at: Option<i64>,
}

/// 直连 SQL 读全部区间（不经过被测服务；顺序与仓储同口径：`started_at, id`）。
fn db_intervals(db: &Db) -> Vec<DbInterval> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare(
            "SELECT i.id, s.mode, s.state, i.started_at, i.ended_at, i.duration_ms,
                    i.needs_review, i.voided_at
               FROM work_interval i JOIN work_session s ON s.id = i.session_id
              ORDER BY i.started_at, i.id",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok(DbInterval {
                id: r.get(0)?,
                mode: r.get(1)?,
                session_state: r.get(2)?,
                started_at: r.get(3)?,
                ended_at: r.get(4)?,
                duration_ms: r.get(5)?,
                needs_review: r.get::<_, i64>(6)? == 1,
                voided_at: r.get(7)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    rows
}

/// 「未作废、未丢弃、已确认闭合」的筛选（02 §6 的排除口径）。
fn is_confirmed(row: &DbInterval) -> bool {
    row.session_state != "discarded"
        && row.voided_at.is_none()
        && !row.needs_review
        && row.ended_at.is_some()
}

/// **独立 oracle**：范围内某 mode 的已确认贡献，用 02 §6 的公式手算。
///
/// 刻意不调 `IntervalRange::clipped_ms` —— 那是被测实现的一部分；
/// `max(0, min(end,to) - max(start,from))` 在这里重写一遍，才有资格当对照。
fn oracle_confirmed_ms(rows: &[DbInterval], mode: &str, from: i64, to: i64) -> i64 {
    rows.iter()
        .filter(|row| row.mode == mode && is_confirmed(row))
        .map(|row| {
            let start = row.started_at;
            let end = row.ended_at.expect("已确认闭合必有终点");
            (end.min(to) - start.max(from)).max(0)
        })
        .sum()
}

// ─────────────────────────────────────────────────────────────────────────────
// 三种视图的读取（全部经 AppState，真入口）
// ─────────────────────────────────────────────────────────────────────────────

fn range_query(epoch: &str, from: i64, to: i64) -> StatsRangeQuery {
    StatsRangeQuery {
        from,
        to,
        timezone: TZ.to_string(),
        expected_data_epoch: epoch.to_string(),
    }
}

fn today_query(epoch: &str) -> TodayQuery {
    TodayQuery {
        timezone: TZ.to_string(),
        expected_data_epoch: epoch.to_string(),
    }
}

fn weekly_query(epoch: &str, anchor: Option<i64>) -> WeeklyQuery {
    WeeklyQuery {
        timezone: TZ.to_string(),
        anchor,
        expected_data_epoch: epoch.to_string(),
    }
}

/// Today 里某一类某一 measure 的列（三组都固定四项，取不到才是 bug）。
fn column(view: &TodayView, class: StatsClass, measure: Measure) -> &MeasureColumn {
    view.column(class, measure)
}

/// 从导出的 JSON 里取某一类某一 measure 的 `ms`（`null` ⇒ `None`）。
fn json_ms(doc: &Value, class: &str, measure: &str) -> Option<i64> {
    json_column(doc, class, measure)["ms"].as_i64()
}

/// 从导出的 JSON 里取某一类某一 measure 的条数。
fn json_intervals(doc: &Value, class: &str, measure: &str) -> u64 {
    json_column(doc, class, measure)["intervals"]
        .as_u64()
        .expect("条数是整数")
}

fn json_column<'a>(doc: &'a Value, class: &str, measure: &str) -> &'a Value {
    doc[class]
        .as_array()
        .unwrap_or_else(|| panic!("{class} 必须是数组"))
        .iter()
        .find(|column| column["measure"] == measure)
        .unwrap_or_else(|| panic!("{class} 缺 {measure} 列"))
}

/// 导出的明细里某一类某一 measure 的 `clipped_ms` 之和（`null` 记 0——与
/// `criteria.exclusions` 写明的口径一致）。
fn json_detail_sum(doc: &Value, class: &str, measure: &str) -> i64 {
    doc["intervals"]
        .as_array()
        .expect("明细是数组")
        .iter()
        .filter(|row| row["class"] == class && row["measure"] == measure)
        .map(|row| row["clipped_ms"].as_i64().expect("明细行必带 clipped_ms"))
        .sum()
}

/// 导出的明细里出现过的区间 id（按导出的顺序）。
fn json_detail_ids(doc: &Value) -> Vec<String> {
    doc["intervals"]
        .as_array()
        .expect("明细是数组")
        .iter()
        .map(|row| row["id"].as_str().expect("明细行必带 id").to_string())
        .collect()
}

/// 日桶里某一天某一 measure 的 `ms`。
fn json_day_ms(doc: &Value, date: &str, measure: &str) -> Option<i64> {
    let day = doc["days"]
        .as_array()
        .expect("日桶是数组")
        .iter()
        .find(|day| day["date"] == date)
        .unwrap_or_else(|| panic!("日桶里没有 {date}"));
    json_ms(day, "confirmed", measure)
}

/// 把 `parse` 出来的 JSON 文档读出来（导出文本必须是合法 JSON）。
fn parse_json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("导出必须是合法 JSON：{error}"))
}

// ─────────────────────────────────────────────────────────────────────────────
// ① 三视图一致 + 每个数字都能追到区间
// ─────────────────────────────────────────────────────────────────────────────

/// Today（今天 03-10）、今天范围的 JSON 导出、本周的 Markdown 周回顾与本周范围的
/// JSON 导出，四份输出对**同一批区间**给出**同一组数字**：人工只算 `FOREGROUND`，
/// 机器（后台 / 被动）与等待单列，待确认排除在已确认之外——而每个数字都能在
/// 库明细里找到出处。
#[test]
fn today_json_and_weekly_report_the_same_intervals_and_the_same_numbers() {
    let fx = fixture();
    seeded(&fx);
    let running = started(&fx);
    let epoch = running.data_epoch().to_string();
    let mut state = lock_app(running.app());

    // ① 库明细（独立 oracle）：10 条区间，含已作废与已丢弃的那两条。
    let rows = db_intervals(state.db());
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        vec![
            // 上一周（03-02）那条时间最早，排在最前。
            "i-prev",
            "i-cross-a",
            "i-cross-b",
            "i-disc",
            "i-pend",
            "i-bg",
            "i-human",
            "i-pass",
            "i-wait",
            "i-void",
        ],
        "夹具的 10 条区间按 started_at, id 排出来"
    );

    // ② Today：真入口、真库。今天是 2026-03-10，日界就是上海的当天零点。
    let today = state
        .stats_today(&today_query(&epoch))
        .expect("Today 应当可读");
    assert_eq!(today.date, "2026-03-10");
    assert_eq!(today.timezone, TZ);
    assert_eq!(
        today.range,
        StatsRange {
            from: TUE_MID,
            to: WED_MID
        },
        "Today 的范围是真实日界（次日零点），不是 start + 24h"
    );
    assert_eq!(
        today.as_of, WALL,
        "当前没有在计时的会话 ⇒ as_of 就是这次样本的墙钟"
    );
    assert_eq!(today.data_epoch, epoch);

    // ③ 已确认人工 = 跨午夜第 2 段（00:10–00:20，10 分钟）+ 今天的人工（09:00–10:00，1 小时）
    //    = 70 分钟 = 4_200_000 毫秒；两条区间。
    let human = column(&today, StatsClass::Confirmed, Measure::Human);
    assert_eq!(human.ms, Some(TODAY_CONFIRMED_HUMAN));
    assert_eq!(human.intervals, 2);
    assert_eq!(
        human.ms,
        Some(oracle_confirmed_ms(&rows, "FOREGROUND", TUE_MID, WED_MID))
    );
    // 已作废的 1 小时与已丢弃的 1 小时若被算进来会是 4_200_000 + 3_600_000 + 3_600_000
    // ——「排除口径有没有生效」的判别式。待确认的 10 分钟同理（会是 4_800_000）。
    assert_ne!(human.ms, Some(TODAY_CONFIRMED_HUMAN + 2 * HOUR));
    assert_ne!(human.ms, Some(TODAY_CONFIRMED_HUMAN + 10 * MIN));

    // ④ 机器与等待**分列**，谁也不并进人工：1 小时后台 + 1 小时人工 = 人工 1 小时（F-103）。
    let background = column(&today, StatsClass::Confirmed, Measure::MachineBackground);
    let passive = column(&today, StatsClass::Confirmed, Measure::MachinePassive);
    let waiting = column(&today, StatsClass::Confirmed, Measure::Waiting);
    assert_eq!(background.ms, Some(HOUR), "09:00–10:00 的后台整整 1 小时");
    assert_eq!(background.intervals, 1);
    assert_eq!(passive.ms, Some(15 * MIN), "09:30–09:45 的被动采集 15 分钟");
    assert_eq!(passive.intervals, 1);
    assert_eq!(waiting.ms, Some(5 * MIN), "10:00–10:05 的等待 5 分钟");
    assert_eq!(waiting.intervals, 1);
    assert_eq!(
        background.ms,
        Some(oracle_confirmed_ms(&rows, "BACKGROUND", TUE_MID, WED_MID))
    );
    assert_eq!(
        passive.ms,
        Some(oracle_confirmed_ms(&rows, "PASSIVE", TUE_MID, WED_MID))
    );
    assert_eq!(
        waiting.ms,
        Some(oracle_confirmed_ms(&rows, "WAITING", TUE_MID, WED_MID))
    );
    // F-103 的核心：1 小时前台（这里是 70 分钟的人工）+ 1 小时**完全并行**的后台
    // 必须报人工 70 分钟，不是 2 小时 10 分。机器在另一列里，谁也不并进谁。
    assert_ne!(
        human.ms,
        Some(TODAY_CONFIRMED_HUMAN + HOUR),
        "并行机器时长不得并进人工（机器是另一列）"
    );

    // ⑤ 运行暂计：本夹具没有在计时的会话 ⇒ 四项都是 0（不是 `null`）。
    for measure in Measure::ALL {
        assert_eq!(column(&today, StatsClass::Live, measure).ms, Some(0));
        assert_eq!(column(&today, StatsClass::Live, measure).intervals, 0);
    }

    // ⑥ 待确认：人工 1 条、已知端点 10 分钟（候选端点不是事实 ⇒ 不进已确认）；
    //    其余三项一条候选都没有 ⇒ `ms` 是 `None`（不是 0）。
    let pending_human = column(&today, StatsClass::Pending, Measure::Human);
    assert_eq!(
        pending_human.intervals, 1,
        "待确认按条数看，不看 ms.is_some()"
    );
    assert_eq!(
        pending_human.ms,
        Some(10 * MIN),
        "08:00–08:10 的已知候选端点跨度"
    );
    for measure in [
        Measure::MachineBackground,
        Measure::MachinePassive,
        Measure::Waiting,
    ] {
        let empty_column = column(&today, StatsClass::Pending, measure);
        assert_eq!(empty_column.intervals, 0);
        assert_eq!(empty_column.ms, None, "一条已知端点候选都没有时才是 None");
    }

    // ⑦ 今日选择列表：P4 的顺序（`task.created_at, id`），两个任务都在。
    assert_eq!(
        today
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec!["t-doc", "t-code"]
    );

    // ── JSON 导出（今天）─── 同一批数字，逐字段相等。
    let json_today = state
        .export_json(&range_query(&epoch, TUE_MID, WED_MID))
        .expect("今天范围的导出应当成功");
    let doc = parse_json(&json_today.text);
    assert_eq!(json_today.revision, today.revision);
    assert_eq!(json_today.data_epoch, today.data_epoch);
    assert_eq!(doc["as_of"].as_i64(), Some(today.as_of));
    assert_eq!(doc["revision"].as_i64(), Some(today.revision));
    assert_eq!(doc["timezone"], TZ);
    assert_eq!(doc["range"]["from"].as_i64(), Some(TUE_MID));
    assert_eq!(doc["range"]["to"].as_i64(), Some(WED_MID));
    assert_eq!(doc["fault_sessions_excluded"].as_u64(), Some(0));
    assert_eq!(json_ms(&doc, "confirmed", "human"), human.ms);
    assert_eq!(
        json_intervals(&doc, "confirmed", "human"),
        human.intervals as u64
    );
    assert_eq!(
        json_ms(&doc, "confirmed", "machine_background"),
        background.ms
    );
    assert_eq!(json_ms(&doc, "confirmed", "machine_passive"), passive.ms);
    assert_eq!(json_ms(&doc, "confirmed", "waiting"), waiting.ms);
    assert_eq!(json_ms(&doc, "pending", "human"), Some(10 * MIN));
    assert_eq!(json_intervals(&doc, "pending", "human"), 1);
    assert_eq!(json_ms(&doc, "pending", "machine_background"), None);

    // 明细：出现的就是今天这 6 条（跨午夜第 2 段、待确认、后台、人工、被动、等待），
    // 已作废 / 已丢弃 / 上一周一条都不在里面；明细求和等于列合计（加得起来才叫合计）。
    assert_eq!(
        json_detail_ids(&doc),
        vec!["i-cross-b", "i-pend", "i-bg", "i-human", "i-pass", "i-wait"],
        "明细按 started_at, id；已作废与已丢弃被仓储那一条查询排除"
    );
    for (class, measure) in [
        ("confirmed", "human"),
        ("confirmed", "machine_background"),
        ("confirmed", "machine_passive"),
        ("confirmed", "waiting"),
        ("pending", "human"),
    ] {
        assert_eq!(
            json_detail_sum(&doc, class, measure),
            json_ms(&doc, class, measure).unwrap_or(0),
            "{class}/{measure}：明细之和必须等于列合计"
        );
    }
    // 逐条的贡献也要说清来路。
    let cross_b = doc["intervals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "i-cross-b")
        .expect("跨午夜第 2 段在明细里");
    assert_eq!(cross_b["clipped_ms"].as_i64(), Some(10 * MIN));
    assert_eq!(cross_b["class"], "confirmed");
    assert_eq!(cross_b["measure"], "human");
    // 「含暂停」的证据：`s-cross` 这一场在**今天**的贡献只有第 2 段那 10 分钟——
    // 暂停的 10 分钟（00:00–00:10）没有任何区间覆盖；若按 23:50 → 00:20 整段算，
    // 这里会变成 20 分钟、今天的人工会变成 4_800_000。
    let cross_today: i64 = doc["intervals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["session_id"] == "s-cross")
        .map(|row| row["clipped_ms"].as_i64().expect("明细行必带 clipped_ms"))
        .sum();
    assert_eq!(cross_today, 10 * MIN);
    let pend = doc["intervals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "i-pend")
        .expect("待确认候选在明细里");
    assert_eq!(pend["class"], "pending");
    assert_eq!(pend["needs_review"], Value::Bool(true));
    assert_eq!(
        pend["duration_ms"],
        Value::Null,
        "候选端点不是事实 ⇒ 没有时长"
    );
    assert_eq!(pend["clipped_ms"].as_i64(), Some(10 * MIN));

    // 日桶：今天只有一天，数字与列相同。
    assert_eq!(doc["days"].as_array().unwrap().len(), 1);
    assert_eq!(
        json_day_ms(&doc, "2026-03-10", "human"),
        Some(TODAY_CONFIRMED_HUMAN)
    );
    assert_eq!(
        json_day_ms(&doc, "2026-03-10", "machine_background"),
        Some(HOUR)
    );

    // ── Markdown 周回顾（本周 = 03-09 → 03-16，半开）───
    let weekly = state
        .export_weekly_markdown(&weekly_query(&epoch, None))
        .expect("周回顾应当成功");
    assert_eq!(weekly.week_start, "2026-03-09");
    assert_eq!(weekly.week_end, "2026-03-16");
    assert_eq!(weekly.timezone, TZ);
    assert_eq!(
        weekly.range,
        StatsRange {
            from: MON_MID,
            to: NEXT_MON_MID
        }
    );
    assert_eq!(weekly.revision, today.revision, "三种视图读的是同一个版本");
    assert_eq!(weekly.data_epoch, today.data_epoch);
    // 本周人工 = 跨午夜两段各 10 分钟 + 人工 1 小时 = 80 分钟。
    assert!(
        weekly
            .text
            .contains("本周合计：1 小时 20 分（4800000 毫秒），共 3 条已确认区间。"),
        "周回顾的人工合计必须给出具体数值：\n{}",
        weekly.text
    );
    // 逐日：03-09 只有跨午夜第 1 段 10 分钟（暂停跨过午夜，第 2 段落到了 03-10）。
    assert!(
        weekly
            .text
            .contains("| 2026-03-09 | 10 分钟（600000 毫秒） |"),
        "03-09 应当只有跨午夜第 1 段的 10 分钟：\n{}",
        weekly.text
    );
    assert!(
        weekly
            .text
            .contains("| 2026-03-10 | 1 小时 10 分（4200000 毫秒） |"),
        "03-10 应当等于 Today 的已确认人工：\n{}",
        weekly.text
    );
    // 待确认单列，且不并入人工合计。
    assert!(
        weekly
            .text
            .contains("| 人工 | 1 | 10 分钟（600000 毫秒） |"),
        "待确认 1 条、已知端点 10 分钟：\n{}",
        weekly.text
    );
    // 完成任务按 `task_change` 的完成事件时刻入周。
    assert!(
        weekly.text.contains("| 2026-03-10 | 写文档 | 已完成 |"),
        "完成事件落在本周：\n{}",
        weekly.text
    );
    assert!(weekly.text.contains("完成记录 1 条。"), "{}", weekly.text);
    // 上一周那 1 小时不在本周的任何数字里（80 分钟 + 60 分钟 = 140 分钟 ⇒ 8400000 毫秒会露馅）。
    assert!(
        !weekly.text.contains("8400000"),
        "上一周的区间不得进本周：\n{}",
        weekly.text
    );

    // ── 本周范围的 JSON：与周回顾逐项一致，并给出「Today ⊂ 本周」的等式 ──
    let json_week = state
        .export_json(&range_query(&epoch, MON_MID, NEXT_MON_MID))
        .expect("本周范围的导出应当成功");
    let week_doc = parse_json(&json_week.text);
    assert_eq!(
        json_ms(&week_doc, "confirmed", "human"),
        Some(WEEK_CONFIRMED_HUMAN),
        "本周人工 = 03-09 的 10 分钟 + 03-10 的 10 分钟 + 人工 1 小时"
    );
    assert_eq!(
        json_ms(&week_doc, "confirmed", "human"),
        Some(
            json_day_ms(&week_doc, "2026-03-09", "human").unwrap()
                + json_day_ms(&week_doc, "2026-03-10", "human").unwrap()
        ),
        "逐日之和必须等于不分组的总和（分桶不丢不重）"
    );
    assert_eq!(
        json_day_ms(&week_doc, "2026-03-10", "human"),
        Some(TODAY_CONFIRMED_HUMAN),
        "本周日桶里 03-10 那一行 == Today 的已确认人工"
    );
    assert_eq!(
        json_day_ms(&week_doc, "2026-03-09", "human"),
        Some(10 * MIN)
    );
    assert_eq!(
        week_doc["days"].as_array().unwrap().len(),
        7,
        "03-09…03-15 共 7 天"
    );
    for date in [
        "2026-03-11",
        "2026-03-12",
        "2026-03-13",
        "2026-03-14",
        "2026-03-15",
    ] {
        assert_eq!(json_day_ms(&week_doc, date, "human"), Some(0));
    }
    assert!(
        !json_detail_ids(&week_doc).contains(&"i-prev".to_string()),
        "上一周的区间不该出现在本周的明细里"
    );
    // 周回顾的人合计、周 JSON 的列、日桶三者同源。
    assert_eq!(
        json_ms(&week_doc, "confirmed", "human"),
        Some(oracle_confirmed_ms(
            &rows,
            "FOREGROUND",
            MON_MID,
            NEXT_MON_MID
        ))
    );
    assert_eq!(
        json_ms(&week_doc, "pending", "human"),
        Some(10 * MIN),
        "周回顾第三节与周 JSON 的待确认是同一列"
    );

    // 上一周自己的周回顾只含它自己那 1 小时——跨周边界两侧各归各的。
    let prev_weekly = state
        .export_weekly_markdown(&weekly_query(&epoch, Some(PREV_START)))
        .expect("上一周的周回顾应当成功");
    assert_eq!(prev_weekly.week_start, "2026-03-02");
    assert_eq!(prev_weekly.week_end, "2026-03-09");
    assert!(
        prev_weekly
            .text
            .contains("本周合计：1 小时（3600000 毫秒），共 1 条已确认区间。"),
        "上一周只有 1 小时：\n{}",
        prev_weekly.text
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ② 修正后一致（F-017 / 02 §8「区间修正后报表重算」）
// ─────────────────────────────────────────────────────────────────────────────

/// 用 P3 的 `correct` 把今天那条 1 小时的人工区间收短到 30 分钟：
/// **Today / JSON / Markdown 三者同步变化**（各减 30 分钟，其余数字一个不动），
/// 且 `revision` 恰好 +1、`as_of` 随时钟前进，`data_epoch` 不变。
#[test]
fn correcting_one_interval_moves_today_json_and_weekly_together() {
    let fx = fixture();
    seeded(&fx);
    let running = started(&fx);
    let epoch = running.data_epoch().to_string();
    let mut state = lock_app(running.app());

    // ① 修正前的基线：三种视图都是「修正前」的那组数字。
    let before = state
        .stats_today(&today_query(&epoch))
        .expect("Today 应当可读");
    assert_eq!(
        column(&before, StatsClass::Confirmed, Measure::Human).ms,
        Some(TODAY_CONFIRMED_HUMAN)
    );
    let before_week = state
        .export_json(&range_query(&epoch, MON_MID, NEXT_MON_MID))
        .expect("本周范围导出应当成功");
    let before_week_doc = parse_json(&before_week.text);
    assert_eq!(
        json_ms(&before_week_doc, "confirmed", "human"),
        Some(WEEK_CONFIRMED_HUMAN)
    );
    let baseline_revision = before.revision;

    // ② 真写命令：P3 的 `correct`（重定时 09:00–10:00 → 09:00–09:30）。
    //    版本位是**会话**版本；夹具里 `s-human` 的 `row_version` 是 0。
    let (report, changed) = state
        .correct(
            WriteEnvelope::for_update(&epoch, 0),
            CorrectRequest {
                session_id: "s-human".into(),
                interval_id: "i-human".into(),
                action: CorrectAction::Retime {
                    started_at: HUMAN_START,
                    ended_at: HUMAN_SHORT_END,
                },
                reason: Some("端到端用例：把一小时的人工收短到 30 分钟".into()),
            },
        )
        .expect("修正可信历史应当成功")
        .into_parts();
    assert!(changed, "这是一次真实修正");
    assert_eq!(
        report.revision,
        baseline_revision + 1,
        "一次成功的业务写恰好推进一次 revision"
    );
    assert_eq!(report.interval.ended_at, Some(HUMAN_SHORT_END));
    assert_eq!(report.interval.duration_ms, Some(30 * MIN));

    // 时钟前进 5 分钟：`as_of` 是**数据水位**，必须跟着走（不是恒定值）。
    elapse(&fx, &mut state, CORRECT_ELAPSED);
    let moved_as_of = WALL + CORRECT_ELAPSED;

    // ③ 修正后：Today 的人工少掉 30 分钟，其余三项一个不动。
    let after = state
        .stats_today(&today_query(&epoch))
        .expect("Today 应当可读");
    assert_eq!(
        column(&after, StatsClass::Confirmed, Measure::Human).ms,
        Some(TODAY_CONFIRMED_HUMAN_AFTER_CORRECT),
        "4_200_000 - 1_800_000：跨午夜第 2 段 10 分钟 + 收短后的 30 分钟"
    );
    assert_eq!(
        column(&after, StatsClass::Confirmed, Measure::Human).intervals,
        2
    );
    assert_eq!(
        column(&after, StatsClass::Confirmed, Measure::MachineBackground).ms,
        Some(HOUR)
    );
    assert_eq!(
        column(&after, StatsClass::Confirmed, Measure::MachinePassive).ms,
        Some(15 * MIN)
    );
    assert_eq!(
        column(&after, StatsClass::Confirmed, Measure::Waiting).ms,
        Some(5 * MIN)
    );
    assert_eq!(
        column(&after, StatsClass::Pending, Measure::Human).ms,
        Some(10 * MIN)
    );
    assert_eq!(after.as_of, moved_as_of, "as_of 随平台时钟前进");
    assert_eq!(after.revision, baseline_revision + 1);
    assert_eq!(after.data_epoch, epoch, "修正不改库身份");

    // ④ JSON（今天）：与 Today 逐字段一致，明细里那条区间就是修正后的贡献。
    let json_after = state
        .export_json(&range_query(&epoch, TUE_MID, WED_MID))
        .expect("导出应当成功");
    let doc_after = parse_json(&json_after.text);
    assert_eq!(json_after.revision, baseline_revision + 1);
    assert_eq!(doc_after["as_of"].as_i64(), Some(moved_as_of));
    assert_eq!(
        json_ms(&doc_after, "confirmed", "human"),
        Some(TODAY_CONFIRMED_HUMAN_AFTER_CORRECT)
    );
    assert_eq!(json_intervals(&doc_after, "confirmed", "human"), 2);
    let retimed = doc_after["intervals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "i-human")
        .expect("被修正的区间仍在明细里");
    assert_eq!(retimed["started_at"].as_i64(), Some(HUMAN_START));
    assert_eq!(retimed["ended_at"].as_i64(), Some(HUMAN_SHORT_END));
    assert_eq!(retimed["duration_ms"].as_i64(), Some(30 * MIN));
    assert_eq!(retimed["clipped_ms"].as_i64(), Some(30 * MIN));

    // ⑤ Markdown 周回顾：本周人工从 80 分钟变 50 分钟，03-10 那一行从 70 变 40。
    let weekly_after = state
        .export_weekly_markdown(&weekly_query(&epoch, None))
        .expect("周回顾应当成功");
    assert_eq!(weekly_after.revision, baseline_revision + 1);
    assert!(weekly_after
        .text
        .contains("本周合计：50 分钟（3000000 毫秒），共 3 条已确认区间。"));
    assert!(weekly_after
        .text
        .contains("| 2026-03-10 | 40 分钟（2400000 毫秒） |"));
    assert!(
        weekly_after
            .text
            .contains("| 2026-03-09 | 10 分钟（600000 毫秒） |"),
        "上一段的 10 分钟不受影响"
    );

    // ⑥ 本周 JSON 与周回顾同源：三者同步、且只动被修正的那一段。
    let week_after = state
        .export_json(&range_query(&epoch, MON_MID, NEXT_MON_MID))
        .expect("导出应当成功");
    let week_after_doc = parse_json(&week_after.text);
    assert_eq!(week_after.revision, baseline_revision + 1);
    assert_eq!(
        json_ms(&week_after_doc, "confirmed", "human"),
        Some(WEEK_CONFIRMED_HUMAN_AFTER_CORRECT)
    );
    assert_eq!(
        json_day_ms(&week_after_doc, "2026-03-10", "human"),
        Some(TODAY_CONFIRMED_HUMAN_AFTER_CORRECT),
        "周 JSON 的 03-10 == Today（修正后仍然相等）"
    );
    assert_eq!(
        json_day_ms(&before_week_doc, "2026-03-11", "human"),
        json_day_ms(&week_after_doc, "2026-03-11", "human"),
        "没被碰过的那一天一个毫秒都不动"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ③ 空范围（02 §8「空范围」的 P5 半边）
// ─────────────────────────────────────────────────────────────────────────────

/// 空范围两种形态都合法且处处为零：`[t, t)`（零长度）与「一整天没有任何区间」。
/// 两者都必须给出**结构完整**的结果（不是错误，也不是缺字段），且明细为空。
#[test]
fn an_empty_range_and_a_day_without_intervals_are_zero_in_every_view() {
    let fx = fixture();
    seeded(&fx);
    let running = started(&fx);
    let epoch = running.data_epoch().to_string();
    let mut state = lock_app(running.app());

    // ① 零长度范围 `[WALL, WALL)`：口径字段齐全、四项都是 0、没有日桶、没有明细。
    let snapshot = state
        .stats_snapshot(&range_query(&epoch, WALL, WALL))
        .expect("零长度范围不是错误");
    let report = snapshot.report().expect("零长度范围可以聚合");
    for measure in Measure::ALL {
        let column = report.column(StatsClass::Confirmed, measure);
        assert_eq!(column.ms, Some(0));
        assert_eq!(column.intervals, 0);
        assert_eq!(
            column.range,
            StatsRange {
                from: WALL,
                to: WALL
            }
        );
    }
    assert!(report.days.is_empty(), "零长度范围覆盖不到任何一天");
    assert!(report.intervals.is_empty(), "零长度范围里没有任何区间");
    assert_eq!(report.fault_sessions_excluded, 0);

    let json_empty = state
        .export_json(&range_query(&epoch, WALL, WALL))
        .expect("空范围导出必须合法");
    let empty_doc = parse_json(&json_empty.text);
    assert_eq!(json_ms(&empty_doc, "confirmed", "human"), Some(0));
    assert_eq!(empty_doc["days"].as_array().unwrap().len(), 0);
    assert_eq!(empty_doc["intervals"].as_array().unwrap().len(), 0);
    assert_eq!(empty_doc["schema_version"].as_u64(), Some(1));
    assert_eq!(empty_doc["units"]["durations"], "milliseconds");
    assert_eq!(empty_doc["range"]["from"].as_i64(), Some(WALL));

    // ② 一整天没有区间（03-12 周四）：仍然是合法报表，日桶给出这一天、数值全 0。
    let idle_from = TUE_MID + 2 * (WED_MID - TUE_MID);
    let idle_to = idle_from + (WED_MID - TUE_MID);
    let snapshot = state
        .stats_snapshot(&range_query(&epoch, idle_from, idle_to))
        .expect("空的一天不是错误");
    let report = snapshot.report().expect("空的一天可以聚合");
    for measure in Measure::ALL {
        let column = report.column(StatsClass::Confirmed, measure);
        assert_eq!(column.ms, Some(0), "{measure:?} 在这天没有区间");
        assert_eq!(column.intervals, 0);
    }
    assert_eq!(report.days.len(), 1);
    assert_eq!(report.days[0].date, "2026-03-12");
    assert_eq!(report.days[0].column(Measure::Human).ms, Some(0));
    assert!(report.intervals.is_empty());

    let json_idle = state
        .export_json(&range_query(&epoch, idle_from, idle_to))
        .expect("空的一天导出必须合法");
    let idle_doc = parse_json(&json_idle.text);
    assert_eq!(idle_doc["days"].as_array().unwrap().len(), 1);
    assert_eq!(json_day_ms(&idle_doc, "2026-03-12", "human"), Some(0));
    assert_eq!(idle_doc["intervals"].as_array().unwrap().len(), 0);
}
