//! P5 Task 3 / Task 4：JSON 明细导出与 Markdown 周回顾（F-018）。
//!
//! # 这一层是什么
//!
//! 把**已经确认的事实**导出成第三方能复现的 JSON：**内容与界面显示一致**（F-018 原文），
//! 所以它**不另写一套取数逻辑**——数字全部来自 [`crate::services::stats::snapshot`] +
//! [`crate::services::stats::StatsSnapshot::report`]（Today 与报表用的就是这一条路径，
//! Ruling P5-4）。本模块只做三件事：
//!
//! 1. 把范围报表的数字**原样**放进导出的顶层（字段名与 DTO 逐字相同）；
//! 2. 写清这次的**筛选口径**（范围、时区、分类依据）与**单位**，使第三方能复现
//!    （02 §6、02 §9；R-03「导出保存当时结果与筛选口径」）；
//! 3. 按 `task_id` 做**标签连接**（R-03 的「按当前标签 / 项目归属重算」）：
//!    任务行 + 项目行 + 标签行随导出一起给出。**只做连接，不重算任何时长**——
//!    标签那一段里没有任何毫秒字段，按标签归并时长由第三方自己做。
//!
//! [`weekly`]（Task 4）是同一层的第二种导出：**Markdown 周回顾**，三节——人工投入 /
//! 完成任务 / 待确认记录（F-018 原文）。它同样只消费上面的范围报表（数字一个不另算），
//! 另加两批只读事实：`task_change` 里的**完成事件**（02 §10：按事件时刻，不按
//! `updated_at`、也不按会话结束时间）与明细里的待确认候选。周界由本层定：
//! 查询时区的**周一 → 下周一（半开）**，端点取真实日界（G4 只走 S10 的两个函数）。
//!
//! # 两个时间不是一回事（Ruling P5-19）
//!
//! - `as_of`：**数字**截至哪一刻——同一次采样的归属终点 `A(M)`，全部数字都来自它；
//! - `generated_at`：**这份文件**何时产出——生成本刻的墙钟。
//!
//! 两者可以不等（例如系统时间被小幅改动、或导出发生在采样之后）：这时导出**不代表**
//! 数据更新到了 `generated_at`。这句话也写进了导出的 `criteria.watermarks`，第三方不必
//! 读代码就能知道该怎么解释这两个字段。
//!
//! # 边界（不要越界）
//!
//! - **落盘归 P8**：本模块返回**字符串**，不引入文件系统 / 对话框依赖，不写文件。
//! - **不依赖 AI、不需要网络**（F-018 原文）。
//! - **只读**：不写库、不加 `revision`、不写审计。
//! - **不做任何按 `weight` 的分配**（属 V0.2）；不做标签层级去重汇总（V0.2）。
//! - **不读时钟**：`generated_at` 由调用方传入（分层门禁机器禁止 `src/services` 读系统
//!   时钟）。生产路径 [`crate::services::bootstrap::AppState::export_json`] 从**平台时钟
//!   接缝**取一次生成本刻的墙钟传进来（`Coordinator::wall_ms` → `platform::clock::Clock`），
//!   与那一次样本同处一条串行边界；数字仍**全部**来自样本（`as_of` 不因它变化）。
//!
//! # `schema_version` 是**导出格式**的版本
//!
//! 它与 **07 入站导入协议**里的 `schema_version` **不是同一个东西**，只是重名：
//!
//! - 这里的 [`SCHEMA_VERSION`] 描述**本文件（出站明细导出）的字段形状**：加字段、
//!   改字段含义、改单位时由**本项目**递增，读它的是未来的导入器 / 分析脚本；
//! - 07 的 `schema_version` 描述**外部 agent 交进来的批次文件**（`batch_id` / `producer`
//!   / `items` …），由**导入协议**那一侧定义与演进。
//!
//! 两者各自独立演进：导出格式升版不代表能收新版导入批次，反之亦然。**别为了让两个数字
//! 相等而改这里**——它们回答的是不同的问题。

use std::collections::BTreeMap;
use std::fmt::Write as _;

use jiff::civil::Date;

use crate::domain::interval::IntervalRange;
use crate::domain::localdate::LocalDate;
use crate::domain::task::TaskStatus;
use crate::error::AppError;
use crate::services::daily_plan::{local_date_at, local_day_bounds, normalize_timezone};
use crate::services::stats::{
    self, DayTotal, Measure, MeasureColumn, RangeReport, StatsClass, StatsInterval, StatsRange,
    StatsRangeQuery,
};
use crate::services::timer::coordinator::StatsSample;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::meta::require_meta;
use crate::storage::project_repo::{self, ProjectRow};
use crate::storage::tag_repo::{self, TagRow};
use crate::storage::task_repo::{self, TaskRow};

/// **导出格式**的版本（见模块文档：与 07 入站导入协议的同名字段无关）。
///
/// 递增它的时机：字段增删、字段含义变化、单位变化。**不是**每次内容变化都递增。
const SCHEMA_VERSION: u32 = 1;

// ─────────────────────────────────────────────────────────────────────────────
// 口径声明：单位与筛选依据（导出的「怎么读 / 怎么复现」那一半）
// ─────────────────────────────────────────────────────────────────────────────

/// 单位口径。数字本身不带单位，全在名字里说清（F-018：导出含「单位」）。
#[derive(Debug, serde::Serialize)]
struct Units {
    /// 全部时长字段（`ms` / `clipped_ms` / `duration_ms`）的单位。
    durations: &'static str,
    /// 全部时刻字段（`generated_at` / `as_of` / `range` / 区间端点）的单位。
    instants: &'static str,
    /// 日期字段（日桶的 `date`）的形状。
    dates: &'static str,
    /// 分钟口径：**明确说清没有**，免得读者自己换算或猜。
    minutes: &'static str,
}

/// 筛选口径（R-03：导出保存当时结果与**筛选口径**）。
///
/// 这些是**口径声明**，不是数字：数字都在 `confirmed` / `live` / `pending` / `days` /
/// `intervals` 里，第三方按这份声明就能重算并核对。
#[derive(Debug, serde::Serialize)]
struct Criteria {
    /// 分类依据：`current_assignments` = 按**当前**标签 / 项目归属（R-03），不是历史快照。
    classification: &'static str,
    /// 范围与日界的算法。
    range_basis: &'static str,
    /// 排除口径。
    exclusions: &'static str,
    /// 人工 / 机器分列口径。
    measures: &'static str,
    /// 待确认栏的毫秒口径（含「什么时候是 `null`」）。
    pending: &'static str,
    /// 标签连接的读法与「不可相加」的提醒。
    labels: &'static str,
    /// 两个时间的口径（Ruling P5-19）：`as_of` 是数据水位，`generated_at` 是文件产出时刻。
    watermarks: &'static str,
}

// ─────────────────────────────────────────────────────────────────────────────
// 导出文档
// ─────────────────────────────────────────────────────────────────────────────

/// 明细里出现过的任务：**标签连接**（R-03）。
///
/// 只承载「这个区间属于谁」的那一半事实——任务行、项目行、标签行**原样**来自仓储
/// （字段名与 IPC 上的 DTO 逐字相同），**不含任何时长字段**：按标签 / 项目归并时长是
/// 读者拿 `intervals[].clipped_ms` 自己做的事，本层不重算、也不按 `weight` 分配。
#[derive(Debug, serde::Serialize)]
struct TaskClassification {
    task: TaskRow,
    project: Option<ProjectRow>,
    tags: Vec<TagRow>,
}

/// 导出文档的**顶层形状**（序列化后的字段名与顺序就是这份声明）。
///
/// 除了 `schema_version` / `generated_at` / `units` / `criteria` / `tasks` 五个导出专属
/// 字段，其余字段**逐字**来自 [`RangeReport`]——字段名与 DTO 一致，数字不做任何二次聚合。
#[derive(Debug, serde::Serialize)]
struct Document {
    /// **导出格式**的版本（≠ 07 入站协议的同名字段，见模块文档）。
    schema_version: u32,
    /// 生成时刻（Unix 毫秒）：**这份文件**何时产出。**由调用方传入**（服务层不读时钟）。
    /// 它与 [`crate::services::stats::StatsSnapshot::as_of`]（数字截至哪一刻）是两件事，
    /// 可以不等（Ruling P5-19）。
    generated_at: i64,
    /// 单位口径。
    units: Units,
    /// 筛选口径（范围 / 时区 / 分类依据）。
    criteria: Criteria,
    /// 事实来自哪个库身份（与界面核对「是不是同一份数据」）。
    data_epoch: String,
    /// 事实来自哪个业务版本（与数字同一次读事务）。
    revision: i64,
    /// 数字截至哪一刻（同一次采样的归属终点 `A(M)`）。
    as_of: i64,
    /// 已归一的查询时区（日界与日桶都按它算）。
    timezone: String,
    /// 半开范围 `[from, to)`。
    range: StatsRange,
    /// 已确认闭合，固定四项。
    confirmed: Vec<MeasureColumn>,
    /// 实时暂计，固定四项。
    live: Vec<MeasureColumn>,
    /// 待确认，固定四项（`ms` 为 `null` = 该 measure 没有已知端点的候选，不是 0）。
    pending: Vec<MeasureColumn>,
    /// 按查询时区真实日界分桶的已确认时长。
    days: Vec<DayTotal>,
    /// 明细：上面的数字就是这些区间算出来的。
    intervals: Vec<StatsInterval>,
    /// 因第 1 类损坏被排除出「已确认」的会话数。
    fault_sessions_excluded: usize,
    /// 标签连接（R-03）：明细里出现过的任务 / 项目 / 标签。
    tasks: Vec<TaskClassification>,
}

/// 一次 JSON 导出的结果：**内容** + 与它同一份数据的版本信封。
///
/// 落盘归 P8（本层不碰文件系统）：P8 把 [`ExportJson::text`] 原样写盘，用
/// [`ExportJson::data_epoch`] / [`ExportJson::revision`] 判断这份结果有没有过期。
#[derive(Debug, Clone)]
pub struct ExportJson {
    /// JSON 文本（UTF-8，两级缩进——导出是给人看也要给机器读的）。
    pub text: String,
    /// 这份文本里的事实来自哪个库身份。
    pub data_epoch: String,
    /// 这份文本里的事实来自哪个业务版本。
    pub revision: i64,
}

/// 生成一份 JSON 明细导出（F-018）。
///
/// 取数走**与 Today / 报表完全相同**的那一条路径（[`stats::snapshot`] +
/// [`crate::services::stats::StatsSnapshot::report`]），本函数只做序列化与 R-03 的标签连接。
///
/// `generated_at` 是**显式参数**：服务层不读时钟（分层门禁机器强制），所以「同一输入两次
/// 导出除生成时间外字节一致」是可测的。生产路径
/// （[`crate::services::bootstrap::AppState::export_json`]）从平台时钟接缝取一次生成本刻的
/// 墙钟传进来；**数字不受它影响**——`as_of` 仍是那一次样本的归属终点（Ruling P5-19）。
///
/// 请求用的就是报表的 [`StatsRangeQuery`]：范围 / 时区 / epoch 守卫与报表同源，导出不会
/// 出现「数字是一个范围、口径字段是另一个范围」。
pub fn json(
    db: &Db,
    sample: StatsSample,
    query: &StatsRangeQuery,
    generated_at: i64,
) -> Result<ExportJson, AppError> {
    // ① 与界面同一条取数路径：一次样本 → 一个一致读快照 → 一个范围报表。
    let snapshot = stats::snapshot(db, sample, query)?;
    let report = snapshot.report()?;
    // ② R-03 的标签连接：同一版本上把明细里出现过的任务连出项目与标签（不重算时长）。
    let tasks = classify(db, &report)?;

    let document = Document {
        schema_version: SCHEMA_VERSION,
        generated_at,
        units: Units {
            durations: "milliseconds",
            instants: "unix_epoch_milliseconds",
            dates: "YYYY-MM-DD",
            minutes: "not used: no field is reported in minutes",
        },
        criteria: Criteria {
            classification: "current_assignments",
            range_basis: "半开范围 [from, to)：每段区间与范围的交集是它的 clipped_ms\
                          （max(0, min(end,to) - max(start,from))）；日桶按 timezone 的真实日界\
                          切分（夏令时切换日是 23 / 25 小时），逐日之和等于不分组的总和。",
            exclusions: "needs_review=1、voided_at 非空、discarded 会话的区间都不进任何「已确认」数字；\
                         其中 voided_at 非空与 discarded 会话的区间也不进明细；\
                         未作废的待确认候选会出现在明细里（needs_review=true、class=pending），\
                         它的 clipped_ms 计入 pending 列（含零长度候选：计数但不贡献跨度）；\
                         该列 ms 为 null 时按 0 求和，明细之和因此等于列合计。",
            measures: "人工只有 FOREGROUND；机器分 BACKGROUND 与 PASSIVE 两项；WAITING 单列。\
                       四类分列、谁也不并进谁（1h 前台 + 1h 后台 = 人工 1h），三类之间不预先相加。",
            pending: "待确认栏的 ms 只累加「已知端点」候选的跨度（裁剪到范围）；终点未知不推算\
                      ——该 measure 一条已知端点的候选都没有时 ms 为 null（不是 0）。条数看 intervals。",
            labels: "标签与项目按每条明细的 task_id 取当前归属（导出这一刻的当前分类，见 tasks 段），\
                     只做连接、不重算任何时长，也不做任何按 weight 的分配；按标签归并时\
                     多个标签之和可大于人工总量，不可相加。",
            watermarks: "数字截至 as_of（同一次采样的归属终点，全部数字都来自它）；generated_at\
                         只说明本文件何时生成，不代表数据更新到那一刻——两者可以不等。",
        },
        data_epoch: report.data_epoch,
        revision: report.revision,
        as_of: report.as_of,
        timezone: report.timezone,
        range: report.range,
        confirmed: report.confirmed,
        live: report.live,
        pending: report.pending,
        days: report.days,
        intervals: report.intervals,
        fault_sessions_excluded: report.fault_sessions_excluded,
        tasks,
    };
    let text =
        serde_json::to_string_pretty(&document).expect("the export document is plain JSON data");
    Ok(ExportJson {
        text,
        data_epoch: document.data_epoch.clone(),
        revision: document.revision,
    })
}

/// R-03 的**标签连接**：明细里出现过的任务 → 任务行 + 项目行 + 标签行。
///
/// 三条约束：
///
/// - **只读、只连接**：不重算时长、不按 `weight` 分配、不做标签层级去重（V0.2）。
/// - **同一个版本**：这次连接与报表的数字必须出自同一个 `data_epoch` / `revision`。
///   生产路径上整条导出都在同一条串行边界内（没人能插进一次写），这里仍把「相等」钉成
///   显式检查——与 [`crate::services::stats`] 里那条同一形状，宁可响亮地失败。
/// - **稳定顺序**：任务按 `task_id` 排序（导出要逐字节可复现，不能依赖哈希顺序）；
///   标签按仓储自己的 `tag.created_at, tag.id` 稳定序。
fn classify(db: &Db, report: &RangeReport) -> Result<Vec<TaskClassification>, AppError> {
    let mut ids: Vec<&str> = report
        .intervals
        .iter()
        .map(|interval| interval.task_id.as_str())
        .collect();
    ids.sort_unstable();
    ids.dedup();

    let tx = db
        .connection()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    let meta = require_meta(&tx)?;
    if meta.data_epoch != report.data_epoch || meta.revision != report.revision {
        return Err(AppError::Storage {
            detail: "the export joined tags from a different revision than the report".into(),
        });
    }

    let mut tasks = Vec::with_capacity(ids.len());
    for id in ids {
        // 明细指着一个不存在的任务：宁可响亮地失败，也不给一份少了一个任务的连接。
        let task = task_repo::get_task(&tx, id)?.ok_or_else(|| AppError::Storage {
            detail: "the export detail points at a task that is not in the database".into(),
        })?;
        let project = match task.project_id.as_deref() {
            Some(project_id) => {
                Some(project_repo::get_project(&tx, project_id)?.ok_or_else(|| {
                    AppError::Storage {
                        detail: "a task points at a project that is not in the database".into(),
                    }
                })?)
            }
            None => None,
        };
        let tags = tag_repo::tags_of_task(&tx, id)?;
        tasks.push(TaskClassification {
            task,
            project,
            tags,
        });
    }
    // 只读事务：什么都没写，直接结束它（与 `stats::snapshot` 同一写法）。
    drop(tx);
    Ok(tasks)
}

// ─────────────────────────────────────────────────────────────────────────────
// Markdown 周回顾（P5 Task 4，F-018）
// ─────────────────────────────────────────────────────────────────────────────

/// 周回顾的请求。
///
/// **周界由本服务定**：查询时区的**周一 → 下周一（半开）**，端点取真实日界
/// （夏令时切换周是 167 / 169 小时）。调用方只回答「哪一周」——给这一周里的任一刻。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct WeeklyQuery {
    /// 用户时区（原始输入，在这一层过 [`normalize_timezone`]）。
    pub timezone: String,
    /// 这一周里的**任一刻**（Unix 毫秒）。`None` = 同一次样本的归属终点 `A(M)`，
    /// 也就是「本周」——与 Today 的「今天」同一口径（都由那一次样本决定，调用方不必、
    /// 也不该为此另采一次墙钟）。
    #[serde(default)]
    pub anchor: Option<i64>,
    /// 请求方手上的库身份；由读事务里的 epoch 守卫校验。
    pub expected_data_epoch: String,
}

/// 一次周回顾的结果：**内容** + 与它同一份数据的版本信封。
///
/// 落盘归 P8（本层不碰文件系统）：P8 把 [`ExportMarkdown::text`] 原样写盘，用
/// [`ExportMarkdown::data_epoch`] / [`ExportMarkdown::revision`] 判断这份结果有没有过期；
/// [`ExportMarkdown::week_start`] / [`ExportMarkdown::week_end`] 与
/// [`ExportMarkdown::range`] 是这一周的**身份**（命名文件与显示「这是哪一周」都用它，
/// 不必让调用方再算一遍周界）。
#[derive(Debug, Clone)]
pub struct ExportMarkdown {
    /// Markdown 文本（UTF-8）。
    pub text: String,
    /// 这份文本里的事实来自哪个库身份。
    pub data_epoch: String,
    /// 这份文本里的事实来自哪个业务版本。
    pub revision: i64,
    /// 这一周的起点：查询时区的**周一**（`YYYY-MM-DD`）。
    pub week_start: String,
    /// 这一周的终点：**下一个周一**（`YYYY-MM-DD`，半开 ⇒ 不含）。
    pub week_end: String,
    /// 同上的半开毫秒范围 `[from, to)`。
    pub range: StatsRange,
    /// 已归一的查询时区。
    pub timezone: String,
}

/// 生成一份 Markdown 周回顾（F-018）：人工投入 / 完成任务 / 待确认记录三节。
///
/// 取数走**与 Today / 报表 / JSON 导出完全相同**的那一条路径（[`stats::snapshot`] +
/// [`crate::services::stats::StatsSnapshot::report`]），本函数只做字符串拼接与两批只读事实
/// 的连接，**不第二次聚合任何数字**（Ruling P5-4/P5-20）：
///
/// - **人工投入**：报表的「已确认 / 人工」列 + 逐日日桶（同一批列，不另算）；
/// - **完成任务**：`task_change` 里 `status → Done` 的**事件时刻**（02 §10）落在这一周的
///   记录，**每个 Done 事件各归其周**——同一任务完成两次就出现在两周的回顾里；
/// - **待确认记录**：报表的「待确认」列 + 明细里的候选，单列在此、**不并入**人工合计。
///
/// `generated_at` 是**显式参数**：服务层不读时钟（分层门禁机器强制），所以「同一输入两次
/// 生成除生成时间外字节一致」是可测的。生产路径
/// （[`crate::services::bootstrap::AppState::export_weekly_markdown`]）从平台时钟接缝取一次
/// **生成本刻的墙钟**传进来；**数字不受它影响**——`as_of` 仍是那一次样本的归属终点
/// （Ruling P5-19 的同一口径）。
///
/// **只读**：不写库、不加 `revision`、不写审计；**不依赖 AI、也不需要网络**；
/// **不落盘**（归 P8）。文案只陈述事实与已确认数字，不含任何推断性结论（04 的 F-206）。
pub fn weekly(
    db: &Db,
    sample: StatsSample,
    query: &WeeklyQuery,
    generated_at: i64,
) -> Result<ExportMarkdown, AppError> {
    // 时区只在这一条入口上归一（与日界、存储键同一套），坏输入在这里就退回。
    let timezone = normalize_timezone(&query.timezone)?;
    // 「本周」默认取**同一次样本**的归属终点（与 Today 的「今天」同一口径）。
    let anchor = query.anchor.unwrap_or(sample.attributed_end);
    let (week_start, week_end, range) = week_bounds(&timezone, anchor)?;

    // ① 与 Today / JSON 导出同一条取数路径：一次样本 → 一个一致读快照 → 一个范围报表。
    let snapshot = stats::snapshot(
        db,
        sample,
        &StatsRangeQuery {
            from: range.start,
            to: range.end,
            timezone: timezone.clone(),
            expected_data_epoch: query.expected_data_epoch.clone(),
        },
    )?;
    let report = snapshot.report()?;
    // ② 完成事件与待确认候选（第二次只读，显式比对版本，见 [`records`]）。
    let (completed, pending) = records(db, &report)?;

    Ok(ExportMarkdown {
        text: render(
            &report,
            week_start,
            week_end,
            generated_at,
            &completed,
            &pending,
        )?,
        data_epoch: report.data_epoch.clone(),
        revision: report.revision,
        week_start: week_start.to_string(),
        week_end: week_end.to_string(),
        range: report.range,
        timezone: report.timezone.clone(),
    })
}

/// 某一刻所在的那一周：**周一 → 下周一（半开）**，端点取查询时区的**真实日界**。
///
/// 「哪一天是周一」是纯日历问题（`jiff` 的 `weekday()`），而**毫秒端点只由 S10 的
/// [`local_day_bounds`] 给出**（G4：不新写第二份日界）——夏令时切换周因此是 167 / 169
/// 小时，而不是 `7 × 24`。
fn week_bounds(
    timezone: &str,
    anchor: i64,
) -> Result<(LocalDate, LocalDate, IntervalRange), AppError> {
    let date = local_date_at(timezone, anchor)?;
    let monday = monday_of(date)?;
    let next_monday = shift_days(monday, 7)?;
    let start = local_day_bounds(timezone, monday)?.start;
    let end = local_day_bounds(timezone, next_monday)?.start;
    Ok((monday, next_monday, IntervalRange::new(start, end)?))
}

/// `date` 所在周的周一（ISO：一周从周一开始）。
fn monday_of(date: LocalDate) -> Result<LocalDate, AppError> {
    let mut civil = civil_of(date)?;
    // `to_monday_zero_offset()`：周一 = 0 … 周日 = 6，最多回退 6 天。
    for _ in 0..civil.weekday().to_monday_zero_offset() {
        civil = civil.yesterday().map_err(|_| date_out_of_range())?;
    }
    from_civil(civil)
}

/// 往后推 `days` 天。
fn shift_days(date: LocalDate, days: u8) -> Result<LocalDate, AppError> {
    let mut civil = civil_of(date)?;
    for _ in 0..days {
        civil = civil.tomorrow().map_err(|_| date_out_of_range())?;
    }
    from_civil(civil)
}

/// `LocalDate` → `jiff` 的日历日（与 `LocalDate::new` 同一套日历规则，取值由公开访问器给）。
fn civil_of(date: LocalDate) -> Result<Date, AppError> {
    Date::new(date.year(), date.month(), date.day()).map_err(|_| date_out_of_range())
}

/// `jiff` 的日历日 → `LocalDate`（仍走 `new`，所以范围与日历规则与解析入口一致）。
fn from_civil(civil: Date) -> Result<LocalDate, AppError> {
    LocalDate::new(civil.year(), civil.month(), civil.day()).map_err(Into::into)
}

/// 「这个日期（或它的前一天 / 后一天）超出可表示的日期范围」。
///
/// 与 [`local_date_at`] 的越界口径一致：**报错，不钳制**——钳制会把一个坏日期变成一个
/// 看起来合理的周。用手写的 `AppError::Domain`（中文、面向用户），不新增错误码。
fn date_out_of_range() -> AppError {
    AppError::Domain {
        detail: "这个日期超出可表示的日期范围。".into(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 第二节与第三节的事实（完成事件、待确认候选）
// ─────────────────────────────────────────────────────────────────────────────

/// 周回顾**只收人工**（Ruling P5-24）：机器（BACKGROUND / PASSIVE）与等待（WAITING）
/// 的时长与待确认记录都去 **JSON 明细导出**里看。
///
/// 三节共用这一个判断——「列哪几类」只写一遍，免得计数表与候选表各判一次而分叉。
const REVIEW_MEASURES: [Measure; 1] = [Measure::Human];

/// 一条完成记录：完成事件时刻在查询时区里的本地日期 + 任务的标题与**当前**状态。
struct CompletedRecord {
    date: String,
    title: String,
    /// 任务行**这一刻**的状态：完成之后又被重开的要能看出来（02 §10）。
    status: TaskStatus,
}

/// 一条待确认记录（明细里 `class = pending` 的那一条）。
struct PendingRecord {
    started: String,
    /// 终点未知时为 `None`——**不推算**（02 §4），文档里写「未知」。
    ended: Option<String>,
    measure: Measure,
    title: String,
}

/// 第二节与第三节要用的事实：完成事件 + 待确认候选，以及它们的任务行（标题、状态）。
///
/// 三条约束：
///
/// - **按形状与事件时刻取完成事件**：读走 [`task_repo::done_events_within`]——它按
///   `json_extract(after_json,'$.status') = 'Done'` 过滤、返回**全部** Done 事件
///   （不取「最后一条」：同一毫秒可以落多行，见该函数的文档）；
/// - **待确认候选取自明细**：与第三节的条数 / 跨度同源（报表的 `pending` 列），不重判谓词；
/// - **同一个版本**：这次读取与报表的数字必须出自同一个 `data_epoch` / `revision`。
///   生产路径上整条周回顾都在同一条串行边界内（没人能插进一次写），这里仍把「相等」
///   钉成显式检查——与 [`classify`] 同一形状，宁可响亮地失败。
fn records(
    db: &Db,
    report: &RangeReport,
) -> Result<(Vec<CompletedRecord>, Vec<PendingRecord>), AppError> {
    let tx = db
        .connection()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    let meta = require_meta(&tx)?;
    if meta.data_epoch != report.data_epoch || meta.revision != report.revision {
        return Err(AppError::Storage {
            detail: "the weekly review read its records from a different revision than the report"
                .into(),
        });
    }

    let events = task_repo::done_events_within(&tx, report.range.from, report.range.to)?;
    let candidates: Vec<&StatsInterval> = report
        .intervals
        .iter()
        .filter(|interval| {
            interval.class == StatsClass::Pending && REVIEW_MEASURES.contains(&interval.measure)
        })
        .collect();

    // 两节提到过的任务各读一行（同一个任务不重复读）；缺行时宁可响亮地失败，
    // 也不给一份少了一条记录的报告。
    let mut ids: Vec<&str> = events
        .iter()
        .map(|event| event.task_id.as_str())
        .chain(candidates.iter().map(|interval| interval.task_id.as_str()))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let mut tasks: BTreeMap<&str, TaskRow> = BTreeMap::new();
    for id in ids {
        let row = task_repo::get_task(&tx, id)?.ok_or_else(|| AppError::Storage {
            detail: "the weekly review points at a task that is not in the database".into(),
        })?;
        tasks.insert(id, row);
    }

    let mut completed = Vec::with_capacity(events.len());
    for event in &events {
        let row = &tasks[event.task_id.as_str()];
        completed.push(CompletedRecord {
            date: local_date_at(&report.timezone, event.completed_at)?.to_string(),
            title: row.title.clone(),
            status: row.status,
        });
    }
    let mut pending = Vec::with_capacity(candidates.len());
    for interval in &candidates {
        let ended = match interval.ended_at {
            Some(ended_at) => Some(local_date_at(&report.timezone, ended_at)?.to_string()),
            None => None,
        };
        pending.push(PendingRecord {
            started: local_date_at(&report.timezone, interval.started_at)?.to_string(),
            ended,
            measure: interval.measure,
            title: tasks[interval.task_id.as_str()].title.clone(),
        });
    }
    // 只读事务：什么都没写，直接结束它（与 `stats::snapshot` 同一写法）。
    drop(tx);
    Ok((completed, pending))
}

// ─────────────────────────────────────────────────────────────────────────────
// 渲染（手写拼接，不引模板引擎）
// ─────────────────────────────────────────────────────────────────────────────

/// 手写 Markdown 拼接：**不引模板引擎**，表格列宽也不做对齐美化（等宽字体之外无意义）。
///
/// 文案只陈述事实与已确认数字（04 的 F-206）：没有任何「效率」「节省」这类推断。
/// 每个时长同时给人类写法与**毫秒数**（单位口径写在文档里），日期一律是查询时区的本地日期。
fn render(
    report: &RangeReport,
    week_start: LocalDate,
    week_end: LocalDate,
    generated_at: i64,
    completed: &[CompletedRecord],
    pending: &[PendingRecord],
) -> Result<String, AppError> {
    let mut out = String::new();
    let human = report.column(StatsClass::Confirmed, Measure::Human);
    let as_of_date = local_date_at(&report.timezone, report.as_of)?;
    let generated_date = local_date_at(&report.timezone, generated_at)?;

    let _ = writeln!(out, "# 周回顾 {week_start} 至 {week_end}");
    let _ = writeln!(out);
    let _ = writeln!(out, "- 时区：{}", report.timezone);
    let _ = writeln!(
        out,
        "- 周界：{week_start}（周一）00:00 至 {week_end}（周一）00:00（半开：含起、不含止）"
    );
    let _ = writeln!(
        out,
        "- 周界（Unix 毫秒）：[{}, {})",
        report.range.from, report.range.to
    );
    let _ = writeln!(out, "- 数据截至（as_of）：{as_of_date}（{}）", report.as_of);
    let _ = writeln!(
        out,
        "- 生成时间（generated_at）：{generated_date}（{generated_at}）"
    );
    let _ = writeln!(
        out,
        "- 数据版本：revision={}，data_epoch={}",
        report.revision, report.data_epoch
    );
    let _ = writeln!(
        out,
        "- 口径：本周的时长数字来自区间事实，按查询时区的真实日界裁剪（夏令时切换周不是 168 小时）；日期一律写成查询时区的本地日期。本回顾只陈述事实与已确认数字，不含推断性结论。"
    );
    let _ = writeln!(out);

    // ── 一、人工投入 ──
    let _ = writeln!(out, "## 一、人工投入");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "口径：人工只统计 FOREGROUND 会话的已确认闭合区间；机器并行时长（BACKGROUND / PASSIVE）与等待（WAITING）不计入人工，本节也不列，它们的时长见 JSON 明细导出。"
    );
    let _ = writeln!(out);
    // 已确认列恒为 `Some`（`report()` 对已确认那一类一律给数）；`unwrap_or(0)` 只是不 panic 的出口。
    let _ = writeln!(
        out,
        "本周合计：{}，共 {} 条已确认区间。",
        format_duration(human.ms.unwrap_or(0)),
        human.intervals
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "| 日期 | 人工 |");
    let _ = writeln!(out, "| --- | --- |");
    for day in &report.days {
        let _ = writeln!(
            out,
            "| {} | {} |",
            day.date,
            format_duration(day.column(Measure::Human).ms.unwrap_or(0))
        );
    }
    let _ = writeln!(out);

    // ── 二、完成任务 ──
    let _ = writeln!(out, "## 二、完成任务");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "口径：按 task_change 里 status 变为 Done 的事件时刻归属到本周（不是任务的 updated_at，也不是会话结束时间）；同一任务完成多次就有多条记录，各归各周。当前状态取生成本文这一刻的任务行：完成之后又被重开的记「已重新打开」，不把它说成仍然完成。"
    );
    let _ = writeln!(out);
    if completed.is_empty() {
        let _ = writeln!(out, "本周没有完成记录。");
    } else {
        let _ = writeln!(out, "| 完成日期 | 任务 | 当前状态 |");
        let _ = writeln!(out, "| --- | --- | --- |");
        for record in completed {
            let _ = writeln!(
                out,
                "| {} | {} | {} |",
                record.date,
                cell(&record.title),
                status_text(record.status)
            );
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "完成记录 {} 条。", completed.len());
    }
    let _ = writeln!(out);

    // ── 三、待确认记录 ──
    let _ = writeln!(out, "## 三、待确认记录");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "口径：未作废的待确认候选（needs_review）单列在此，不计入上面的人工合计；终点未知的候选不推算，只给条数，跨度一栏记「未知」。本节只列人工的候选，机器与等待的待确认记录见 JSON 明细导出。"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "| 类型 | 候选条数 | 已知端点跨度 |");
    let _ = writeln!(out, "| --- | --- | --- |");
    for measure in REVIEW_MEASURES {
        let column = report.column(StatsClass::Pending, measure);
        // 一条候选都没有时不给「0」也不给「未知」：那是「无候选」，不是「算出来是 0」。
        let span = if column.intervals == 0 {
            "无候选".to_string()
        } else {
            match column.ms {
                Some(ms) => format_duration(ms),
                None => "未知（终点未知，不推算）".to_string(),
            }
        };
        let _ = writeln!(
            out,
            "| {} | {} | {span} |",
            measure_text(measure),
            column.intervals
        );
    }
    let _ = writeln!(out);
    if pending.is_empty() {
        let _ = writeln!(out, "本周没有待确认记录。");
    } else {
        let _ = writeln!(out, "| 开始日期 | 结束日期 | 类型 | 任务 |");
        let _ = writeln!(out, "| --- | --- | --- | --- |");
        for record in pending {
            let ended = record.ended.clone().unwrap_or_else(|| "未知".to_string());
            let _ = writeln!(
                out,
                "| {} | {ended} | {} | {} |",
                record.started,
                measure_text(record.measure),
                cell(&record.title)
            );
        }
    }
    Ok(out)
}

/// 一段时长的人类写法 + 毫秒数：`1 小时 40 分（6000000 毫秒）`。
///
/// 毫秒数**总是**给出（单位口径）：分钟以下的部分只靠人类写法会丢，而这是一个可以拿去
/// 核对的导出。
fn format_duration(ms: i64) -> String {
    let minutes = ms / 60_000;
    let hours = minutes / 60;
    let mins = minutes % 60;
    let seconds = ms / 1_000;
    let text = if hours > 0 && mins > 0 {
        format!("{hours} 小时 {mins} 分")
    } else if hours > 0 {
        format!("{hours} 小时")
    } else if minutes > 0 {
        format!("{minutes} 分钟")
    } else if seconds > 0 {
        format!("{seconds} 秒")
    } else {
        "0".to_string()
    };
    format!("{text}（{ms} 毫秒）")
}

/// [`Measure`] 的中文名（文档里给用户看的那个）。
fn measure_text(measure: Measure) -> &'static str {
    match measure {
        Measure::Human => "人工",
        Measure::MachineBackground => "机器（后台）",
        Measure::MachinePassive => "机器（被动）",
        Measure::Waiting => "等待",
    }
}

/// 完成记录里「当前状态」那一列。
///
/// 只有 `Done` 才说「已完成」；从 `Done` 出来过的任务一律不再宣称仍然完成（02 §10：
/// 「完成后重开显示『重新打开』，避免周报宣称仍已完成」）。从 `Done` 只能回到 `Ready`
/// （02 §5 的跃迁表），所以其余取值就是「重开之后又走到了哪一步」。
fn status_text(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Done => "已完成",
        TaskStatus::Cancelled => "已取消",
        _ => "已重新打开",
    }
}

/// 表格单元里的用户文本：竖线会把表格拆坏（Markdown 里要写成 `\|`），换行同理。
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}
