//! P5 Task 3：JSON 明细导出（F-018）。
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

use crate::error::AppError;
use crate::services::stats::{
    self, DayTotal, MeasureColumn, RangeReport, StatsInterval, StatsRange, StatsRangeQuery,
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
