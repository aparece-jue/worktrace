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
//! 2. [`StatsSnapshot::report`]：**纯函数**，在边界**外**基于快照聚合出 [`RangeReport`]。
//!
//! 分开的理由是计划原文：「构造内部 StatsSnapshot 后释放边界；聚合 / 序列化在外部基于
//! 此快照进行」。所以本模块**不读时钟**（区间终点只能来自那一次样本的 `attributed_end`）、
//! **不写库**（不产生事实、不加 `revision`、不写审计）、**不新开裁剪实现**。
//!
//! # 口径（照抄 02 §6，不重新解释）
//!
//! - 范围一律**半开** `[from, to)`；每段有效区间与范围的交集走
//!   [`IntervalRange::clipped_ms`]（全项目唯一一处裁剪实现）。
//! - **三类分列，不得合并**：已确认闭合（含 `recovering` 会话里已可信的前缀）、
//!   实时暂计（当前开放区间）、待确认部分。
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

use crate::domain::interval::IntervalRange;
use crate::domain::session::SessionMode;
use crate::error::AppError;
use crate::services::daily_plan::{local_days_covering, normalize_timezone};
use crate::services::recovery::attention_overview;
use crate::services::timer::coordinator::StatsSample;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::meta::require_meta;
use crate::storage::session_repo;

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
    /// 待确认部分：候选端点**不是**事实，所以这一栏没有时长。
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
    /// 该列的毫秒数。**待确认列恒为 `None`**：候选端点不是事实，不换算成时长。
    pub ms: Option<i64>,
    /// 该列覆盖的区间条数（待确认列用它说「有几条要处理」）。
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
    /// 它被归到哪一类。
    pub class: StatsClass,
    /// 它按会话模式归到哪个 measure。
    pub measure: Measure,
    pub started_at: i64,
    /// 开放区间为 `None`。
    pub ended_at: Option<i64>,
    /// 待确认与开放区间没有可信时长（`None`）。
    pub duration_ms: Option<i64>,
    pub needs_review: bool,
}

/// 一次范围报表。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RangeReport {
    /// 已确认闭合，固定四项。
    pub confirmed: Vec<MeasureColumn>,
    /// 实时暂计，固定四项。
    pub live: Vec<MeasureColumn>,
    /// 待确认，固定四项（`ms` 恒为 `None`）。
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

    /// 把冻结的快照聚合成范围报表（纯函数，**在串行边界外**调用）。
    pub fn report(&self) -> Result<RangeReport, AppError> {
        let from = self.range.start;
        let to = self.range.end;
        let report_range = StatsRange::from(self.range);

        let mut confirmed = [0i64; MEASURE_COUNT];
        let mut confirmed_intervals = [0usize; MEASURE_COUNT];
        let mut live = [0i64; MEASURE_COUNT];
        let mut live_intervals = [0usize; MEASURE_COUNT];
        let mut pending_intervals = [0usize; MEASURE_COUNT];
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
                    IntervalRange::new(fact.started_at, self.attributed_end)?.clipped_ms(from, to)
                } else {
                    // 归属终点没有越过起点（改时后退等）：暂计 0，不编造负数。
                    0
                };
                live[index] += live_ms;
                live_intervals[index] += 1;
                details.push(detail(fact, StatsClass::Live, measure));
                continue;
            }

            // ② 待确认：只认 P3 的待确认集合（Ruling P5-6），本模块不重写谓词。
            if self.pending_ids.contains(fact.id.as_str()) {
                pending_intervals[index] += 1;
                details.push(detail(fact, StatsClass::Pending, measure));
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
                confirmed[index] += span.clipped_ms(from, to);
                confirmed_intervals[index] += 1;
                counted.push(fact);
                details.push(detail(fact, StatsClass::Confirmed, measure));
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
                    true,
                    StatsRange::from(day_range),
                ),
            });
        }

        Ok(RangeReport {
            confirmed: self.columns(
                StatsClass::Confirmed,
                &confirmed,
                &confirmed_intervals,
                true,
                report_range,
            ),
            live: self.columns(StatsClass::Live, &live, &live_intervals, true, report_range),
            // 待确认栏只有条数与 measure：候选端点不是事实，`ms` 恒为 `None`。
            pending: self.columns(
                StatsClass::Pending,
                &[0; MEASURE_COUNT],
                &pending_intervals,
                false,
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

    /// 拼一列组：`values` 是毫秒（`ms_known = false` 时整列 `None`），`counts` 是条数。
    fn columns(
        &self,
        class: StatsClass,
        values: &[i64; MEASURE_COUNT],
        counts: &[usize; MEASURE_COUNT],
        ms_known: bool,
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
                ms: if ms_known { Some(values[index]) } else { None },
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

fn detail(fact: &Fact, class: StatsClass, measure: Measure) -> StatsInterval {
    StatsInterval {
        id: fact.id.clone(),
        session_id: fact.session_id.clone(),
        class,
        measure,
        started_at: fact.started_at,
        ended_at: fact.ended_at,
        duration_ms: fact.duration_ms,
        needs_review: fact.needs_review,
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

    let overview = attention_overview(db, &query.expected_data_epoch, &sample.run_id)?;

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
            mode: row.mode,
            started_at: row.interval.started_at,
            ended_at: row.interval.ended_at,
            duration_ms: row.interval.duration_ms,
            needs_review: row.interval.needs_review,
        })
        .collect();

    Ok(StatsSnapshot {
        run_id: sample.run_id,
        session_id: sample.session_id,
        session_version: sample.session_version,
        open_interval_id: sample.open_interval_id,
        attributed_end: sample.attributed_end,
        data_epoch: meta.data_epoch,
        revision: meta.revision,
        range,
        timezone,
        facts,
        pending_ids,
        fault_session_ids,
    })
}
