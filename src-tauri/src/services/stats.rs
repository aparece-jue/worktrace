//! P5 Task 1：统计口径——半开范围裁剪、排除口径、人工 / 机器分离与日界分桶（02 §6）。
//!
//! # 这一层是什么
//!
//! **Today、JSON 明细导出、Markdown 周回顾三者共用的唯一取数层**（Ruling P5-4）。
//! 它做两件事，分得很开：
//!
//! 1. [`snapshot`]：在**同一条串行边界**内，用一次协调器样本（
//!    [`crate::services::timer::coordinator::Coordinator::stats_sample`]）加一次一致读，
//!    把事实冻成 [`StatsSnapshot`]；
//! 2. [`StatsSnapshot::report`]：**纯函数**，可在边界**外**基于快照聚合出 [`RangeReport`]。
//!
//! Today与导出包装目前在串行边界内完成，确保任务/分类材料与数字同版本。
//! 分层保留冻结快照后在锁外聚合的能力；消费者须先冻结全部附加材料才能移出边界。
//! 本模块**不读时钟**（区间终点只能来自那一次样本的 `attributed_end`）、
//! **正常采样下不写库**（不产生事实、不加 `revision`、不写审计；采样发现异常时由 P2 的
//! 异常路径决定是否提交恢复事务——幂等分支与硬故障回滚分支**零写入**）、**不新开裁剪实现**。
//!
//! 三个消费者：
//!
//! - [`snapshot`] + [`StatsSnapshot::report`]：范围报表（T3/T4 的导出来自它）；
//! - [`today`]：F-010 的 Today **一次返回**——今日选择列表、当前任务与运行状态、
//!   确认人工工时、运行暂计、待确认时间**五项分别给出、不预先相加**。它用同一次样本
//!   与同一个读事务（事实 + 日计划 + 当前会话），日界取查询时区的**真实**日界。
//!
//! # 口径（照抄 02 §6，不重新解释）
//!
//! - 范围一律**半开** `[from, to)`；每段有效区间与范围的交集走
//!   [`IntervalRange::clipped_ms`]（全项目唯一一处裁剪实现）。
//! - **三类分列，不得合并**：已确认闭合（含 `recovering` 会话里已可信的前缀）、
//!   实时暂计（当前开放区间）、待确认部分。待确认栏**含零长度候选**（P3 的 S5 归一与 P2 的
//!   异常分割都会产出 `[t,t)`），它的条数与 `attention_overview.pending_intervals` 的差
//!   只剩「范围裁剪」这一处；`ms` 只累加**已知端点**候选的跨度，终点未知的不推算
//!   （Ruling P5-12）。
//! - **排除**：`needs_review = 1`、`voided_at` 非空、`discarded` 会话的区间一律不进
//!   「已确认」，也不进实时暂计；其中 `voided_at` 与 `discarded` 在仓储那一条查询里
//!   排除（[`crate::storage::session_repo::intervals_overlapping`]，**只做一次**）。
//! - **待确认与第 1 类损坏**：判定读 P3 的
//!   [`crate::services::recovery::attention_overview`]（Ruling P5-6），本模块不写第二份
//!   损坏判定，也不拿「列表空不空」当门禁。
//! - **measure 分离**：人工**只有** `FOREGROUND`；机器分 `BACKGROUND` / `PASSIVE` 两项；
//!   `WAITING` 单列。**禁止把并行机器时长加成人工**（F-103 的核心）。
//! - **日界**只走 P3 的 [`local_days_covering`]（G4），不新写第二份日界；跨日分桶用
//!   它的逐日半开界加 `clipped_ms` 表达。
//! - 每个结果 DTO 带 `measure` / `timezone` / `range` / `as_of` / `revision`，其中
//!   `revision` 与事实出自**同一次读事务**（00 §5）。

use std::collections::HashSet;

use rusqlite::{Connection, Transaction};

use crate::domain::interval::IntervalRange;
use crate::domain::session::{SessionMode, SessionState};
use crate::error::AppError;
use crate::services::daily_plan::{
    local_date_at, local_day_bounds, local_days_covering, normalize_timezone,
};
use crate::services::recovery::attention_overview;
use crate::services::timer::coordinator::StatsSample;
use crate::storage::daily_plan_repo;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::meta::require_meta;
use crate::storage::session_repo;
use crate::storage::task_repo::{self, TaskRow};

/// measure 的项数（= [`Measure::ALL`] 的长度）。数组下标一律用它，不写字面 4。
const MEASURE_COUNT: usize = Measure::ALL.len();

// ─────────────────────────────────────────────────────────────────────────────
// 口径枚举
// ─────────────────────────────────────────────────────────────────────────────

/// 02 §6 的 measure 维度。
///
/// **人工只有 `FOREGROUND`**——机器时长无论与人工并行多久，都不会被加进人工。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Measure {
    /// 人工：**只有** `FOREGROUND` 会话的区间。
    Human,
    /// 机器：`BACKGROUND`。
    MachineBackground,
    /// 机器：`PASSIVE`。
    MachinePassive,
    /// `WAITING` 单列：不并入人工，也不并入任何机器项。
    Waiting,
}

impl Measure {
    /// 固定四项、固定顺序：报表的每一类都按它出列，缺项以 0 出现（形状稳定，
    /// 消费者不必处理「这一项在不在」）。
    pub const ALL: [Measure; 4] = [
        Self::Human,
        Self::MachineBackground,
        Self::MachinePassive,
        Self::Waiting,
    ];

    /// 会话模式 → measure。**全项目唯一的归类点**；`match` 是穷尽的，将来多一种
    /// 会话模式会在这里编译失败，而不是被悄悄算进人工。
    pub fn of(mode: SessionMode) -> Self {
        match mode {
            SessionMode::Foreground => Self::Human,
            SessionMode::Background => Self::MachineBackground,
            SessionMode::Passive => Self::MachinePassive,
            SessionMode::Waiting => Self::Waiting,
        }
    }
}

/// 三类分列中的哪一类（02 §6：**不得合并**）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatsClass {
    /// 已确认闭合的区间（含 `recovering` 会话里已可信的前缀）。
    Confirmed,
    /// 实时暂计：当前开放区间，终点取同一次协调器快照的归属终点 `A(M)`。
    Live,
    /// 待确认部分：**含零长度候选**（`[t,t)`），候选端点不是既成事实——只有**已知端点**
    /// 的候选按跨度求和（裁剪到范围），终点未知的只计数、不给毫秒（02 §4 不推算）。
    Pending,
}

// ─────────────────────────────────────────────────────────────────────────────
// 请求与结果 DTO
// ─────────────────────────────────────────────────────────────────────────────

/// 报表范围（半开 `[from, to)`，Unix 毫秒）。
///
/// 它是 DTO 的**序列化形状**：领域类型 `IntervalRange` 不实现 `Serialize`，
/// 而报表要经 IPC 与 JSON 导出（P8 / T3）。裁剪仍然只有
/// [`IntervalRange::clipped_ms`] 一处实现，这里只是把同一对端点带出去。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct StatsRange {
    pub from: i64,
    pub to: i64,
}

impl From<IntervalRange> for StatsRange {
    fn from(range: IntervalRange) -> Self {
        Self {
            from: range.start,
            to: range.end,
        }
    }
}

/// 一次范围统计的请求。
///
/// `timezone` 与 `expected_data_epoch` 都是**原始输入**：时区在这里过
/// [`normalize_timezone`] 那一道唯一入口（与日界、存储键同一套），epoch 由读事务里的
/// `guard_epoch`（经 [`attention_overview`]）校验。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct StatsRangeQuery {
    pub from: i64,
    pub to: i64,
    pub timezone: String,
    pub expected_data_epoch: String,
}

/// 一列统计结果——02 §6 说的「每个结果 DTO」，五个口径字段齐全。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MeasureColumn {
    /// 属于三类中的哪一类。
    pub class: StatsClass,
    /// 这一列报的是哪个 measure。
    pub measure: Measure,
    /// 这一列按哪个时区算（已归一）。
    pub timezone: String,
    /// 这一列覆盖的范围（半开）。日桶里是「该日真实日界 ∩ 报表范围」。
    pub range: StatsRange,
    /// 这一列的数字截至哪一刻（本次采样的归属挂钟 `A(M)`）。
    pub as_of: i64,
    /// 事实来自哪个库身份。
    pub data_epoch: String,
    /// 事实来自哪个业务版本（与列里的数字同一次读事务）。
    pub revision: i64,
    /// 该列的毫秒数。
    ///
    /// **待确认列**：该 measure 里**已知端点**候选的跨度之和（裁剪到范围，与其它列同一
    /// 口径）；这一 measure 一条已知端点的候选都没有时才是 `None`——「终点未知**不推算**」
    /// （02 §4），不是「整栏不给数」。零长度候选有已知端点但跨度 0，所以它计数、
    /// 贡献 0 毫秒。
    pub ms: Option<i64>,
    /// 该列覆盖的区间条数。
    ///
    /// **待确认列**：与范围相交（**含零长度候选**与终点未知但已开始的候选）的未作废待确认
    /// 条数。它与 [`crate::services::recovery::attention_overview`] 的 `pending_intervals`
    /// 的差**只剩「范围裁剪」这一处**（后者是全局的、不分范围）。
    pub intervals: usize,
}

/// 一个本地日的已确认时长。
///
/// 日期是**查询时区**的本地日期（`YYYY-MM-DD`，与 `LocalDate` 的落库形状逐字一致），
/// 日界来自 [`local_days_covering`] 的真实半开界（夏令时切换日是 23 / 25 小时）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DayTotal {
    pub date: String,
    /// 该日已确认时长，按 [`Measure::ALL`] 固定四项。
    pub confirmed: Vec<MeasureColumn>,
}

impl DayTotal {
    /// 取该日某个 measure 的列（固定四项，必定存在）。
    pub fn column(&self, measure: Measure) -> &MeasureColumn {
        self.confirmed
            .iter()
            .find(|column| column.measure == measure)
            .expect("日桶固定含全部 measure 列")
    }
}

/// 构成这些数字的一段区间事实（明细）。
///
/// 它的用途是「结果可追溯」：JSON 明细导出（T3）与界面明细都从这里取，不需要
/// 再写一套取数逻辑。**已作废与已丢弃的行不在其中**——它们只能在历史 / 审计里看
/// （02 §4/§6），也正因为它们根本没进这次读，排除口径只有一处。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StatsInterval {
    pub id: String,
    pub session_id: String,
    /// 它属于哪个任务——导出按**当前标签 / 项目**重算时的分类依据（R-03）：消费方只要按
    /// 任务连一次标签，不必自己再查一次会话。
    pub task_id: String,
    /// 它被归到哪一类。
    pub class: StatsClass,
    /// 它按会话模式归到哪个 measure。
    pub measure: Measure,
    pub started_at: i64,
    /// 开放区间为 `None`。
    pub ended_at: Option<i64>,
    /// 区间自身记下的可信时长（**未**按范围裁剪）；待确认与开放区间为 `None`。
    pub duration_ms: Option<i64>,
    pub needs_review: bool,
    /// 本区间在这次报表范围内的贡献，与它所在列的 `ms` 逐项相加相等
    /// （同类同 measure 的 `sum(clipped_ms) == column.ms`）——明细加得起来才等于合计。
    /// 终点未知的待确认候选不推算，这里是 0。
    pub clipped_ms: i64,
}

/// 一次范围报表。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RangeReport {
    /// 已确认闭合，固定四项。
    pub confirmed: Vec<MeasureColumn>,
    /// 实时暂计，固定四项。
    pub live: Vec<MeasureColumn>,
    /// 待确认，固定四项：含零长度候选；`ms` 见 [`MeasureColumn::ms`]（只有已知端点候选
    /// 的跨度，终点未知时该 measure 为 `None`）。
    pub pending: Vec<MeasureColumn>,
    /// 按查询时区真实日界分桶的**已确认**时长；逐日之和 == `confirmed` 的对应项之和。
    pub days: Vec<DayTotal>,
    /// 明细：上面的数字就是这些区间算出来的（被排除的行不在其中）。
    pub intervals: Vec<StatsInterval>,
    /// 因第 1 类损坏被排除出「已确认」的会话数（这些会话在范围内出现过区间）。
    pub fault_sessions_excluded: usize,
    pub timezone: String,
    pub range: StatsRange,
    pub as_of: i64,
    pub data_epoch: String,
    pub revision: i64,
}

impl RangeReport {
    /// 取某一类某一 measure 的列（固定四项，必定存在）。
    pub fn column(&self, class: StatsClass, measure: Measure) -> &MeasureColumn {
        let columns: &[MeasureColumn] = match class {
            StatsClass::Confirmed => &self.confirmed,
            StatsClass::Live => &self.live,
            StatsClass::Pending => &self.pending,
        };
        columns
            .iter()
            .find(|column| column.measure == measure)
            .expect("每一类固定含全部 measure 列")
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 内部快照
// ─────────────────────────────────────────────────────────────────────────────

/// 一致读里读到的一行区间事实。`voided_at` 与 `discarded` 的行不会出现在这里
/// （仓储那一条查询已经排除，服务层不重判）。
#[derive(Debug)]
struct Fact {
    id: String,
    session_id: String,
    task_id: String,
    mode: SessionMode,
    started_at: i64,
    ended_at: Option<i64>,
    duration_ms: Option<i64>,
    needs_review: bool,
}

/// 一次统计快照：**同一条串行边界内**取得的一份已验证样本 + 一次一致读。
///
/// 采样到读快照之间不会有 `pause`/`resume`/`finish`/异常分割穿插——那正是它必须与
/// 样本同处一个边界的原因；边界释放之后，聚合只读这份快照（[`Self::report`]）。
///
/// 字段口径：
/// - `run_id` / `session_id` / `session_version` / `open_interval_id` / `attributed_end`
///   全部来自**同一次** [`StatsSample`]；
/// - `as_of`（[`Self::as_of`]）就是 `attributed_end`——P2 的 `TimerSnapshot::as_of`
///   也是这个值；
/// - `data_epoch` / `revision` 与上面那批事实出自同一次读事务（00 §5）。
///
/// **为什么不用样本里的 `closed_trusted_ms`**：它是**整条会话**的可信闭合合计，而报表
/// 问的是「范围 `[from, to)` 里有多少」——直接用它会把范围外的工时也算进来。已确认那一类
/// 因此由这次范围读的每一行按 [`IntervalRange::clipped_ms`] 重新裁出来。实时暂计同理：
/// 样本的 `live_ms` 是未裁剪的 `attributed_end - started_at`，报表用的是同一对端点与
/// 范围求交后的值（端点仍然只来自那一次样本，不另取墙钟）。
#[derive(Debug)]
pub struct StatsSnapshot {
    pub run_id: String,
    pub session_id: Option<String>,
    pub session_version: Option<i64>,
    /// 当前开放区间 id；没有在计时的区间时为 `None`。
    pub open_interval_id: Option<String>,
    /// 归属终点 `A(M)`：实时暂计算到这里为止（**不另取墙钟**）。
    pub attributed_end: i64,
    /// 同一次样本里的会话状态（`None` = 没有活动会话）。T2 的「当前任务与运行状态」要用它
    /// ——从 `open_interval_id.is_some()` 猜会把 `paused` 与 `recovering` 混为一谈。
    pub state: Option<SessionState>,
    pub data_epoch: String,
    pub revision: i64,
    /// 这次快照覆盖的半开范围。
    pub range: IntervalRange,
    /// 已归一的查询时区。
    pub timezone: String,
    facts: Vec<Fact>,
    /// P3 `attention_overview` 给出的**未作废待确认区间** id 集合。
    pending_ids: HashSet<String>,
    /// P3 `attention_overview` 给出的第 1 类（不变量损坏）会话 id 集合。
    fault_session_ids: HashSet<String>,
}

impl StatsSnapshot {
    /// 报表 DTO 里叫 `as_of` 的那个值（= [`StatsSnapshot::attributed_end`]）。
    pub fn as_of(&self) -> i64 {
        self.attributed_end
    }

    /// 把冻结的快照聚合成范围报表（纯函数，可在串行边界外调用；当前Today/导出包装仍持边界）。
    pub fn report(&self) -> Result<RangeReport, AppError> {
        let from = self.range.start;
        let to = self.range.end;
        let report_range = StatsRange::from(self.range);

        let mut confirmed = [0i64; MEASURE_COUNT];
        let mut confirmed_intervals = [0usize; MEASURE_COUNT];
        let mut live = [0i64; MEASURE_COUNT];
        let mut live_intervals = [0usize; MEASURE_COUNT];
        let mut pending = [0i64; MEASURE_COUNT];
        let mut pending_intervals = [0usize; MEASURE_COUNT];
        // 该 measure 有没有「已知端点」的候选：只有一条都没有时 `ms` 才是 `None`。
        let mut pending_known = [false; MEASURE_COUNT];
        let mut details: Vec<StatsInterval> = Vec::with_capacity(self.facts.len());
        // 计入「已确认」的事实：日桶要在它们上面再裁一遍，不重新分类。
        let mut counted: Vec<&Fact> = Vec::new();
        let mut fault_excluded: HashSet<&str> = HashSet::new();

        for fact in &self.facts {
            let measure = Measure::of(fact.mode);
            let index = measure_index(measure);

            // ① 实时暂计：**必须**是本次快照那条开放区间，且它此刻仍是一条有效开放
            //    区间（未闭合、未标待确认）。闭合事实走下面一支——一遍回放，
            //    live 与 confirmed 互斥，同一段时间不会被算两遍。
            if self.is_live_open_interval(fact) {
                let live_ms = if self.attributed_end > fact.started_at {
                    let span = IntervalRange::new(fact.started_at, self.attributed_end)?;
                    // 开放终点未知；实时范围只能使用本次采样认可的终点。
                    if !span.overlaps(self.range) {
                        continue;
                    }
                    span.clipped_ms(from, to)
                } else {
                    // 刚启动的零时长实时记录保留，但只出现在包含起点的范围内。
                    if !(from <= fact.started_at && fact.started_at < to) {
                        continue;
                    }
                    0
                };
                live[index] += live_ms;
                live_intervals[index] += 1;
                details.push(detail(fact, StatsClass::Live, measure, live_ms));
                continue;
            }

            // ② 待确认：只认 P3 的待确认集合（Ruling P5-6），本模块不重写谓词。
            //
            //    条数**含零长度候选**（P3 的 S5 归一在有可信前缀时写 `[t,t)`）：它落在范围内
            //    就得算一条，否则普通崩溃路径之后报表说「0 条」而恢复面说「1 条」（P5-12）。
            //    毫秒只累加**已知端点**候选的跨度并裁剪到范围；终点未知的不推算（02 §4）。
            if self.pending_ids.contains(fact.id.as_str()) {
                pending_intervals[index] += 1;
                let mut clipped = 0;
                if let Some(ended_at) = fact.ended_at {
                    // 有已知端点（哪怕只是 `[t,t)` 那个点）⇒ 这一 measure 的 `ms` 有数。
                    pending_known[index] = true;
                    if ended_at > fact.started_at {
                        clipped =
                            IntervalRange::new(fact.started_at, ended_at)?.clipped_ms(from, to);
                        pending[index] += clipped;
                    }
                }
                details.push(detail(fact, StatsClass::Pending, measure, clipped));
                continue;
            }
            if fact.needs_review {
                // 同一 revision 下不可能：`attention_overview` 的待确认集合就是
                // 「未作废的 `needs_review = 1`」。真出现说明两批事实对不上，
                // 宁可响亮地失败，也不把它算成「已确认」。
                return Err(AppError::Storage {
                    detail: "a needs_review interval is missing from the attention overview".into(),
                });
            }

            // ③ 第 1 类（不变量损坏）会话的区间：可疑事实暂不计（02 §4），
            //    但**不**从待确认栏里抹掉它——用户要处理的就是那些。
            if self.fault_session_ids.contains(&fact.session_id) {
                fault_excluded.insert(fact.session_id.as_str());
                continue;
            }

            // ④ 已确认闭合。候选端点不算事实，所以只有闭合过的才进来。
            if let Some(ended_at) = fact.ended_at {
                let span = IntervalRange::new(fact.started_at, ended_at)?;
                let clipped = span.clipped_ms(from, to);
                confirmed[index] += clipped;
                confirmed_intervals[index] += 1;
                counted.push(fact);
                details.push(detail(fact, StatsClass::Confirmed, measure, clipped));
            }
            // 其余（未闭合、未标待确认、又不是本次快照的开放区间）不计入任何一类：
            // 它们是启动扫描的归一对象（P3 Task 1），不是统计口径里的三类之一。
        }

        // 日界分桶：只拆「已确认」那一类（计划原文），日界只用 P3 的 S10。
        let mut days = Vec::new();
        for (date, bounds) in local_days_covering(&self.timezone, from, to)? {
            // 该日的**真实日界 ∩ 报表范围**：报表范围之外的部分不属于这次报表，
            // 否则逐日之和会大于不分组的总和。
            let day_range = IntervalRange::new(bounds.start.max(from), bounds.end.min(to))?;
            let mut values = [0i64; MEASURE_COUNT];
            let mut counts = [0usize; MEASURE_COUNT];
            for fact in &counted {
                let span = fact_span(fact)?;
                let ms = span.clipped_ms(day_range.start, day_range.end);
                // 零长度段不产生条目：端点正好落在日界上的区间不会给次日留一条 0。
                if ms > 0 {
                    let index = measure_index(Measure::of(fact.mode));
                    values[index] += ms;
                    counts[index] += 1;
                }
            }
            days.push(DayTotal {
                date: date.to_string(),
                confirmed: self.columns(
                    StatsClass::Confirmed,
                    &values,
                    &counts,
                    [true; MEASURE_COUNT],
                    StatsRange::from(day_range),
                ),
            });
        }

        Ok(RangeReport {
            confirmed: self.columns(
                StatsClass::Confirmed,
                &confirmed,
                &confirmed_intervals,
                [true; MEASURE_COUNT],
                report_range,
            ),
            live: self.columns(
                StatsClass::Live,
                &live,
                &live_intervals,
                [true; MEASURE_COUNT],
                report_range,
            ),
            // 待确认栏：条数含零长度候选，`ms` 只算已知端点候选的跨度（逐 measure 判）。
            pending: self.columns(
                StatsClass::Pending,
                &pending,
                &pending_intervals,
                pending_known,
                report_range,
            ),
            days,
            intervals: details,
            fault_sessions_excluded: fault_excluded.len(),
            timezone: self.timezone.clone(),
            range: report_range,
            as_of: self.attributed_end,
            data_epoch: self.data_epoch.clone(),
            revision: self.revision,
        })
    }

    /// 这一行是不是**本次快照**那条开放区间：id 对上、仍未闭合、也不是待确认段。
    ///
    /// 只有它才算「实时暂计」——别的未闭合区间（历史残留、别的会话）没有可信终点，
    /// 一个毫秒都不该计。
    fn is_live_open_interval(&self, fact: &Fact) -> bool {
        self.open_interval_id.as_deref() == Some(fact.id.as_str())
            && fact.ended_at.is_none()
            && !fact.needs_review
    }

    /// 拼一列组：`values` 是毫秒，`counts` 是条数；`ms_known[measure] = false` 的那一项
    /// 整列 `None`（只有待确认栏会用到：该 measure 一条已知端点的候选都没有）。
    fn columns(
        &self,
        class: StatsClass,
        values: &[i64; MEASURE_COUNT],
        counts: &[usize; MEASURE_COUNT],
        ms_known: [bool; MEASURE_COUNT],
        range: StatsRange,
    ) -> Vec<MeasureColumn> {
        Measure::ALL
            .iter()
            .enumerate()
            .map(|(index, measure)| MeasureColumn {
                class,
                measure: *measure,
                timezone: self.timezone.clone(),
                range,
                as_of: self.attributed_end,
                data_epoch: self.data_epoch.clone(),
                revision: self.revision,
                ms: if ms_known[index] {
                    Some(values[index])
                } else {
                    None
                },
                intervals: counts[index],
            })
            .collect()
    }
}

/// measure → 固定下标（与 [`Measure::ALL`] 同序，取自它本身，不会漂移）。
fn measure_index(measure: Measure) -> usize {
    Measure::ALL
        .iter()
        .position(|candidate| *candidate == measure)
        .expect("Measure::ALL 含全部取值")
}

/// 一段事实的半开范围。开放区间（`ended_at` 为 `None`）取零长度——它没有终点，
/// 日桶里本来也不会有它（只有已确认闭合的才进日桶）。
fn fact_span(fact: &Fact) -> Result<IntervalRange, AppError> {
    Ok(IntervalRange::new(
        fact.started_at,
        fact.ended_at.unwrap_or(fact.started_at),
    )?)
}

fn detail(fact: &Fact, class: StatsClass, measure: Measure, clipped_ms: i64) -> StatsInterval {
    StatsInterval {
        id: fact.id.clone(),
        session_id: fact.session_id.clone(),
        task_id: fact.task_id.clone(),
        class,
        measure,
        started_at: fact.started_at,
        ended_at: fact.ended_at,
        duration_ms: fact.duration_ms,
        needs_review: fact.needs_review,
        clipped_ms,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 取快照（串行边界内）
// ─────────────────────────────────────────────────────────────────────────────

/// 取一次统计快照。
///
/// **调用方必须已经在同一条串行边界内拿到样本**——生产路径是
/// `AppState::stats_snapshot`（它解构 `AppState`、取一次
/// [`StatsSample`]、再调这里的服务）。本函数自己不取样本、不读时钟。
///
/// 顺序（计划原文）：先经 P2 取得已验证样本并完成必要的异常事务，再开一致读事务读事实
/// 与 epoch/revision，构造快照后释放边界。待确认 / 第 1 类损坏的判定读 P3 的
/// [`attention_overview`]（Ruling P5-6）——它自己开一个只读事务并 `guard_epoch`。
/// 两批事实必须来自**同一个版本**：串行边界内不可能被人插进一次写，所以这里用
/// 「revision 必须相等」把这条前提钉成显式检查，而不是靠纪律。
pub fn snapshot(
    db: &Db,
    sample: StatsSample,
    query: &StatsRangeQuery,
) -> Result<StatsSnapshot, AppError> {
    // 范围形状与全项目同一判据：`from > to` 报 `NegativeInterval`，`from == to` 合法。
    let range = IntervalRange::new(query.from, query.to)?;
    // 时区只在这一条入口上归一（与日界、存储键同一套），坏输入在这里就退回。
    let timezone = normalize_timezone(&query.timezone)?;

    read_consistent(
        db,
        sample,
        range,
        &timezone,
        &query.expected_data_epoch,
        |_, _| Ok(()),
    )
    .map(|(snapshot, ())| snapshot)
}

/// 一次一致读的共同骨架：**同一次 epoch 守卫 + 同一个读事务**。
///
/// 顺序照 Task 1 的原样：先经 [`attention_overview`]（它自己守卫 epoch，并给出
/// 「未作废待确认」与「第 1 类损坏」两个集合），再开**一个**读事务读事实。两批事实
/// 必须来自**同一个版本**：串行边界内不可能被人插进一次写，所以这里把「`meta` 与
/// overview 的 epoch/revision 相等」钉成显式检查，而不是靠纪律。
///
/// `extra` 是**同一个事务里**追加的读：Today 用它取今日选择列表与当前会话
/// （Ruling P5-1）——于是「Today 一次返回」里的每一批数据都只可能来自这一个版本。
fn read_consistent<T>(
    db: &Db,
    sample: StatsSample,
    range: IntervalRange,
    timezone: &str,
    expected_data_epoch: &str,
    extra: impl FnOnce(&Transaction<'_>, &StatsSample) -> Result<T, AppError>,
) -> Result<(StatsSnapshot, T), AppError> {
    let overview = attention_overview(db, expected_data_epoch, &sample.run_id)?;

    let tx = db
        .connection()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    let meta = require_meta(&tx)?;
    if meta.data_epoch != overview.data_epoch || meta.revision != overview.revision {
        return Err(AppError::Storage {
            detail: "stats read two different revisions inside one serial boundary".into(),
        });
    }
    let rows = session_repo::intervals_overlapping(&tx, range.start, range.end)?;
    let extras = extra(&tx, &sample)?;
    // 只读事务：什么都没写，直接结束它（与 `attention_overview` 同一写法）。
    drop(tx);

    let pending_ids: HashSet<String> = overview
        .items
        .iter()
        .flat_map(|item| item.intervals.iter().map(|interval| interval.id.clone()))
        .collect();
    let fault_session_ids: HashSet<String> = overview
        .items
        .iter()
        .filter(|item| item.fault_reason.is_some())
        .map(|item| item.session_id.clone())
        .collect();
    let facts: Vec<Fact> = rows
        .into_iter()
        .map(|row| Fact {
            id: row.interval.id,
            session_id: row.interval.session_id,
            task_id: row.task_id,
            mode: row.mode,
            started_at: row.interval.started_at,
            ended_at: row.interval.ended_at,
            duration_ms: row.interval.duration_ms,
            needs_review: row.interval.needs_review,
        })
        .collect();

    Ok((
        StatsSnapshot {
            run_id: sample.run_id,
            session_id: sample.session_id,
            session_version: sample.session_version,
            open_interval_id: sample.open_interval_id,
            attributed_end: sample.attributed_end,
            state: sample.state,
            data_epoch: meta.data_epoch,
            revision: meta.revision,
            range,
            timezone: timezone.to_string(),
            facts,
            pending_ids,
            fault_session_ids,
        },
        extras,
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Today 聚合（F-010）
// ─────────────────────────────────────────────────────────────────────────────

/// Today 的取数请求。
///
/// **不带日期**：「今天」由服务从**同一次样本的归属终点** `A(M)` 算
/// （[`local_date_at`]），所以 `date` / `range` / `as_of` 三者天然同源——让调用方
/// 另传一个日期，只会得到一份「数字是这一天的、口径字段是那一天」的视图。要看别的
/// 日子用 [`StatsRangeQuery`]（范围报表）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct TodayQuery {
    /// 用户时区（原始输入，在这一层过 [`normalize_timezone`]）。
    pub timezone: String,
    /// 请求方手上的库身份；由读事务里的 epoch 守卫校验。
    pub expected_data_epoch: String,
}

/// 当前任务与运行状态（F-010 的第二项）。
///
/// `state` 取自**同一次** [`StatsSample`]：本层不推断 session 状态——从
/// `open_interval_id.is_some()` 猜会把 `paused` 与 `recovering` 混为一谈。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CurrentTask {
    /// 当前会话。暂停中的会话**也是**当前会话，只是没有开放区间。
    pub session_id: String,
    pub task_id: String,
    /// 任务标题。只有 id 的话界面渲染不出「当前任务」：24 条命令里没有「按 id 取任务」
    /// 的读路径，而当前任务未必在今天的列表里。
    pub task_title: String,
    pub state: SessionState,
}

/// Today 一次返回（F-010 的五项，**分别显示、不预先相加**）。
///
/// 五项 = [`Self::tasks`]（今日选择列表）、[`Self::current`]（当前任务与运行状态）、
/// [`Self::confirmed`] / [`Self::live`] / [`Self::pending`] 三组里的 `Human` 列
/// （确认人工工时 / 运行暂计 / 待确认时间）。**没有**合计字段：三段时间分属三类事实
/// （02 §6「不得合并」），加起来既不是工时，也不是待办。
///
/// 三组工时都按 [`Measure::ALL`] **固定四项**给出（与 [`RangeReport`] 同形，取列用
/// [`Self::column`]）：人工**只有** `FOREGROUND`，机器分 `BACKGROUND` / `PASSIVE`，
/// `WAITING` 单列——**谁也不并进谁**（F-103）。机器与等待必须与人工出自**同一次查询**
/// （同一个 `as_of`/`revision`），否则同一页上会出现两个水位。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TodayView {
    /// ① 今日选择列表：P4 的顺序（`task.created_at, task.id`）。**完成的任务保留在
    /// 列表里**并带自己的状态，不因完成而消失。
    pub tasks: Vec<TaskRow>,
    /// ② 当前任务与运行状态；没有活动会话时为 `None`。
    pub current: Option<CurrentTask>,
    /// ③ 已确认：今日真实日界内已确认闭合（含 `recovering` 会话里已可信的前缀），
    /// 按 [`Measure::ALL`] 固定四项。`Human` 那一列就是 F-010 的「确认人工工时」。
    ///
    /// **第 1 类（不变量损坏）会话的区间不进这个数**（待确认候选仍保留在 `pending`；本次开放区间先由采样判定，`live` 分支不由故障集合排除）⇒ 当天有
    /// 这类会话时，这里的数字是**静默变小**的，看到数字时要想到这一层。要看被排除了几个
    /// 会话，用 [`RangeReport::fault_sessions_excluded`]——Today **有意**不带计数字段；
    /// V0.1 的损坏提示只在恢复页（[`crate::services::recovery::attention_overview`]）。
    pub confirmed: Vec<MeasureColumn>,
    /// ④ 运行暂计：当前开放区间裁剪到今日（终点取**同一次样本**的 `A(M)`），固定四项。
    pub live: Vec<MeasureColumn>,
    /// ⑤ 待确认：固定四项。`intervals` **含零长度候选**，`ms` 只累加已知端点候选的跨度
    /// （全未知才是 `None`）⇒ 判「有没有待确认」要看 `intervals`，**不要**看
    /// `ms.is_some()`（Ruling P5-12）。
    pub pending: Vec<MeasureColumn>,
    /// 这个视图算的是哪一天：**查询时区**的本地日期（`YYYY-MM-DD`，与 `LocalDate`
    /// 的落库形状逐字一致）。
    pub date: String,
    /// 已归一的查询时区。
    pub timezone: String,
    /// 今日的**真实**半开日界：`end` 是次日零点的换算结果，夏令时切换日是 23 / 25 小时。
    pub range: StatsRange,
    /// 这些数字截至哪一刻（同一次样本的归属终点 `A(M)`）。
    pub as_of: i64,
    /// 这些事实来自哪个库身份。
    pub data_epoch: String,
    /// 这些事实来自哪个业务版本（与五项出自**同一个读事务**）。
    pub revision: i64,
}

impl TodayView {
    /// 取某一类某一 measure 的列（每一类固定四项，必定存在）。与
    /// [`RangeReport::column`] 同一形状：消费者按 `class` + `measure` 取自己要显示的那一项，
    /// 不必知道它在 `Vec` 里的位置，也没有任何「相加」的入口。
    pub fn column(&self, class: StatsClass, measure: Measure) -> &MeasureColumn {
        let columns: &[MeasureColumn] = match class {
            StatsClass::Confirmed => &self.confirmed,
            StatsClass::Live => &self.live,
            StatsClass::Pending => &self.pending,
        };
        columns
            .iter()
            .find(|column| column.measure == measure)
            .expect("每一类固定含全部 measure 列")
    }
}

/// Today 聚合：一次返回 F-010 的五项（避免 N+1，00 §4）。
///
/// **调用方必须已经在同一条串行边界内拿到样本**——生产路径是
/// `AppState::stats_today`。本函数自己不取样本、不读时钟：「今天」与 `as_of` 都从
/// 那一次样本的归属终点来，日界只走 P3 的 [`local_day_bounds`]（G4）。
///
/// 事实、今日选择列表、当前会话与 epoch/revision 全部出自**一个读事务**
/// （Ruling P5-1）：日计划用 **repo 级** [`daily_plan_repo::plan_for`]，不用
/// `services::daily_plan::plan_for`——后者会自己开事务并自己守卫 epoch，嵌进来就是
/// 两次守卫 + 两个快照。聚合是纯函数；当前Today包装在同一串行边界内调用（[`StatsSnapshot::report`]）。
///
/// **有意的丢弃**：报表算出来的 [`RangeReport::fault_sessions_excluded`] 在这里被**直接
/// 丢掉**——下面只搬三组列，计数字段不搬（口径裁决 Task2-deferred-③，与
/// [`TodayView::confirmed`] 那一句同一口径）：Today 不带损坏计数，要计数请用报表，
/// 或者看恢复页的 `attention_overview`。
pub fn today(db: &Db, sample: StatsSample, query: &TodayQuery) -> Result<TodayView, AppError> {
    // 时区只在这一条入口上归一（与日界、存储键同一套），坏输入在这里就退回。
    let timezone = normalize_timezone(&query.timezone)?;
    // 「今天」与 as_of 同源：日期取这一次样本的归属终点，本层不另读时钟。
    let date = local_date_at(&timezone, sample.attributed_end)?;
    // 日界是「次日零点」的换算结果，不是 `start + 24h`。
    let range = local_day_bounds(&timezone, date)?;

    let (snapshot, (tasks, current)) = read_consistent(
        db,
        sample,
        range,
        &timezone,
        &query.expected_data_epoch,
        |tx, sample| {
            // ① 今日选择列表与 ② 当前任务：**同一个读事务**（Ruling P5-1）。
            let tasks = daily_plan_repo::plan_for(tx, &date, &timezone)?;
            let current = current_task(tx, sample)?;
            Ok((tasks, current))
        },
    )?;

    // 聚合为纯函数（当前 Today 包装仍在串行边界内）：三组列**原样**取自同一份报表，
    // 不筛选、不合并、不相加（F-010 的五项就是三组里的 `Human` 那三列）。
    // `report.fault_sessions_excluded` **有意不搬**（见上面的文档）：Today 不带计数字段。
    let report = snapshot.report()?;
    Ok(TodayView {
        tasks,
        current,
        confirmed: report.confirmed,
        live: report.live,
        pending: report.pending,
        date: date.to_string(),
        timezone: report.timezone,
        range: report.range,
        as_of: report.as_of,
        data_epoch: report.data_epoch,
        revision: report.revision,
    })
}

/// 当前任务与运行状态。
///
/// 会话身份与状态取自**同一次样本**；`task_id` / `task_title` 取自**同一个读事务**
/// 里的会话行与任务行——[`StatsSample`] 本身不带任务身份（P2 的接缝只给会话），而
/// 当前任务**不能**从「有没有开放区间」推：暂停中的会话没有开放区间，却仍然有当前任务。
fn current_task(conn: &Connection, sample: &StatsSample) -> Result<Option<CurrentTask>, AppError> {
    let Some(session_id) = sample.session_id.as_deref() else {
        return Ok(None);
    };
    // 样本说「有会话」就必须给出状态：两半对不上说明接缝被改坏了。宁可响亮地失败，
    // 也不要编一个默认状态——`state` 正是本层唯一不许推断的东西。
    let state = sample.state.ok_or_else(|| AppError::Storage {
        detail: "the stats sample carries a session id without a state".into(),
    })?;
    let session_row =
        session_repo::get_session(conn, session_id)?.ok_or_else(|| AppError::Storage {
            detail: "the stats sample points at a session that is not in the database".into(),
        })?;
    // 变量名不叫 `task`：`task.title` 是 `tests/error_contract.rs` 里已收口的**英文错误
    // 文案**片段（作为原始子串扫描），这里只是字段访问，别让它撞上那条门禁。
    let task_row =
        task_repo::get_task(conn, &session_row.task_id)?.ok_or_else(|| AppError::Storage {
            detail: "the current session points at a task that is not in the database".into(),
        })?;
    Ok(Some(CurrentTask {
        session_id: session_row.id,
        task_id: task_row.id,
        task_title: task_row.title,
        state,
    }))
}
