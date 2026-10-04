//! 今日计划的**输入校验入口**（P4 Task 1，裁决 R6）与**今日选择列表服务**
//! （P4 Task 4，F-010 的「今日选择」半边）。
//!
//! T4 的今日计划服务从这里取校验，不得另写一份时区/日期规则。
//!
//! # 时区口径：读写一致，且不搬移历史
//!
//! - **规范化**：[`normalize_timezone`] 是唯一入口。它去掉首尾空白，再要求这个名字
//!   能在**内置的** IANA 库里解析出 `TimeZone::iana_name()`：`utc`/`Utc`/`UTC`
//!   都收敛到 `UTC`，而 `Etc/Unknown`、固定偏移（`+08:00`）、任意无空白串一律拒绝。
//!   写入用的存储键与查询用的键都出自它，所以主键
//!   `daily_plan(task_id, local_date, timezone)` 不会为同一个时区裂成两行。
//! - **不搬移**：用户换时区时**旧计划一行都不动**——它们是「当时那个时区的
//!   那一天」的事实，新时区只影响之后写入的行。本模块不提供任何迁移/改写入口，
//!   也不要用「把旧行的日期按新时区重算一遍」来实现换时区。
//! - **日界**：[`local_date_at`] 按**所选时区**算日期（同一时刻在上海与纽约可能差
//!   一天），永远**不**拿 `updated_at` 或 UTC 日期代替（02 §9）。反向换算由
//!   [`local_day_bounds`] / [`local_days_covering`]（P3 S10）负责：一天的界是
//!   「次日零点」的换算结果，**不是** `start + 86_400_000`——夏令时切换日是
//!   23/25 小时（半小时制 DST 的时区是 23.5/24.5）。P5 的统计口径直接用这一对
//!   函数加 [`crate::domain::interval::IntervalRange::clipped_ms`]，不另写一份。
//!
//! # 今日选择不是排期（P4 Task 4）
//!
//! [`add_to_plan`] / [`remove_from_plan`] 只写 `daily_plan` 一行：不设 `Scheduled`、
//! 不建 `time_block`、不启动计时，也不因为任务状态或日期变化去搬移已有行
//! （裁决 R-T4-e）。完成项留在当天列表里，由 UI 按任务行自己的状态显示。
//!
//! # 为什么在服务层而不是 `domain/`
//!
//! 时区解析要读时区库（本机是打包进产物的 tzdb，系统时区还要问操作系统），
//! 所以它不进 `domain/`——那里只放纯规则。[`crate::domain::localdate::LocalDate`]
//! 表达的正是被校验之后的那个日期。

use crate::domain::error::DomainError;
use crate::domain::interval::IntervalRange;
use crate::domain::localdate::LocalDate;
use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::storage::daily_plan_repo;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::guards::guard_epoch;
use crate::storage::meta::require_meta;
use crate::storage::task_repo::TaskRow;
use crate::storage::WriteOutcome;

use super::tx::{settle, write_tx};

/// 本地日期输入的校验入口（`daily_plan.local_date`）。
pub fn parse_local_date(raw: &str) -> Result<LocalDate, AppError> {
    LocalDate::parse(raw).map_err(Into::into)
}

/// 时区输入的**唯一**校验入口：解析 + 规范化，返回落库用的存储键。
///
/// 读路径必须用同一个函数处理请求里的时区，否则「UTC」与「utc」会变成两行计划。
pub fn normalize_timezone(raw: &str) -> Result<String, AppError> {
    let zone = timezone_of(raw)?;
    // 没有 IANA 名称的时区（`Etc/Unknown`、固定偏移、POSIX 规则）不能当存储键：
    // 它们描述不了「一天」从哪儿开始，而今日计划正需要这个边界。
    zone.iana_name()
        .map(str::to_string)
        .ok_or_else(|| unknown_timezone(raw))
}

/// 机器当前时区的规范化名称——「今日」的默认时区。
///
/// `TimeZone::system()` 需要 `tz-system` 特性（Windows 上经 CLDR 映射到 IANA 名称，
/// 本机实测是 `Asia/Shanghai`）。读不出来时**不猜**：返回错误让用户显式选一个——
/// 猜一个 `UTC` 会把「今天」整体算错一天。
pub fn system_timezone_name() -> Result<String, AppError> {
    jiff::tz::TimeZone::system()
        .iana_name()
        .map(str::to_string)
        .ok_or_else(|| AppError::Domain {
            detail: "读不出系统时区，请手动选择时区。".into(),
        })
}

/// 给定时刻在指定时区里的本地日期。
///
/// `wall_ms` 是 Unix 毫秒，与 storage 的 `INTEGER` 列、`platform::clock::ClockSample`
/// 的挂钟同一个单位。时刻超出可表示范围时**报错，不钳制**：钳制会把一个坏时刻变成
/// 一个看起来合理的日期，那正是「日期错了一天」这类最难查的缺陷的来源。
pub fn local_date_at(timezone: &str, wall_ms: i64) -> Result<LocalDate, AppError> {
    // 与写路径同一口径：能当存储键的名字才允许拿来算日期，读写不会分叉。
    let name = normalize_timezone(timezone)?;
    let zone = timezone_of(&name)?;
    let at = jiff::Timestamp::from_millisecond(wall_ms).map_err(|_| AppError::Domain {
        detail: "这个时刻超出可表示的日期范围。".into(),
    })?;
    LocalDate::from_jiff(at.to_zoned(zone).date()).map_err(Into::into)
}

/// 解析一个时区名（不要求它有 IANA 名称）：空白输入与非时区输入在这里被拒。
///
/// 私有：对外只有 [`normalize_timezone`]（要 IANA 名称）与 [`local_date_at`]
/// （先过同一道校验）两个入口，避免出现「校验过了但没规范化」的第三条路径。
fn timezone_of(raw: &str) -> Result<jiff::tz::TimeZone, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DomainError::EmptyText { field: "时区" }.into());
    }
    jiff::tz::TimeZone::get(trimmed).map_err(|_| unknown_timezone(raw))
}

// ─────────────────────────────────────────────────────────────────────────────
// 真实日界（P3 S10）
// ─────────────────────────────────────────────────────────────────────────────
//
// 「某一天」**不是**一个固定长度：夏令时切换日是 23 或 25 小时（半小时制 DST 的时区
// 是 23.5 / 24.5 小时）。所以一天的界只能由「次日零点的换算」得到，不能写
// `start + 86_400_000`——那会在切换日把第二天（或前一天）的一小时算进来。
//
// 这两个函数是「本地日 ↔ 半开毫秒区间」的唯一换算入口：时区一律先过
// [`normalize_timezone`]（与写路径同一个存储键），P5 的统计口径直接用它们加
// `IntervalRange::clipped_ms`，不自己再算一份。

/// 本地日期在给定时区里的**真实**半开界 `[start, end)`。
///
/// - `end` 是**次日零点**的换算结果，不是 `start + 86_400_000`（08 §1）：夏令时切换日
///   因此是 23/25 小时。
/// - 本地零点正好落在跳变**缺口**里时（例如 `America/Santiago` 2024-09-08 的 `00:00`
///   被跳过），jiff 的默认消歧给出跳变之后的那一刻——那正是这一天真正开始的一刻，
///   不是「前一天再晚一点」。
/// - 日期（或它的次日）越过可表示范围时**报错，不钳制**：与 [`local_date_at`] 的越界
///   口径一致，坏输入不该变成一个看起来合理的区间。
pub fn local_day_bounds(timezone: &str, date: LocalDate) -> Result<IntervalRange, AppError> {
    // 与写路径同一口径：能当存储键的名字才允许拿来算日界，读写不会分叉。
    let name = normalize_timezone(timezone)?;
    let zone = timezone_of(&name)?;
    bounds_in(&zone, date)
}

/// 一个半开范围 `[from, to)` 覆盖到的每一个本地日：`(本地日期, 该日的真实半开界)`，
/// 按日升序。
///
/// 语义（与 `IntervalRange` 的半开口径同一套）：
/// - 只返回与 `[from, to)` **正相交**（`IntervalRange::overlap_ms > 0`）的日；
/// - `from == to`（零长度）⇒ 空 `Vec`——零长度范围不覆盖任何一天；
/// - `from > to` ⇒ [`DomainError::NegativeInterval`]，**不静默交换端点**：交换会把
///   「调用方算错了」变成「悄悄换了一天」；
/// - 端点正好落在日界上时**不含次日**（半开：`[d0, d1)` 只覆盖 `d0`）。
///
/// 逐日按同一个换算推进（前一日的 `end` 就是后一日的 `start`），所以结果天然
/// **无缺口、无重叠**；调用方要取与范围的交集请用
/// [`crate::domain::interval::IntervalRange::clipped_ms`]，不要自己算。
pub fn local_days_covering(
    timezone: &str,
    from: i64,
    to: i64,
) -> Result<Vec<(LocalDate, IntervalRange)>, AppError> {
    // 区间形状先判：与 `IntervalRange::new` 同一判据，且**绝不交换端点**。
    if to < from {
        return Err(DomainError::NegativeInterval {
            started_at: from,
            ended_at: to,
        }
        .into());
    }
    // 时区仍要校验：坏输入不该因为「范围是空的」而悄悄放行（本模块的时区校验只有
    // `normalize_timezone` 这一个入口，读写与统计都走它）。
    let name = normalize_timezone(timezone)?;
    let zone = timezone_of(&name)?;
    if to == from {
        return Ok(Vec::new());
    }

    // 半开：最后一个被覆盖的毫秒是 `to - 1`，它的本地日就是最后一天。
    // 这也正是「端点落在日界上不含次日」的实现——`to` 是零点的瞬间本身不算被覆盖。
    let first = local_date_at(&name, from)?;
    let last = local_date_at(&name, to - 1)?;

    let mut days = Vec::new();
    let mut date = first;
    let mut start = midnight_in(&zone, date)?;
    loop {
        let next = next_date(date)?;
        let end = midnight_in(&zone, next)?;
        // 前一日的 `end` 就是后一日的 `start`：同一个换算结果，不重复算第二次。
        days.push((date, IntervalRange::new(start, end)?));
        if date == last {
            return Ok(days);
        }
        date = next;
        start = end;
    }
}

/// 某一天在给定（已解析）时区里的半开界。两个公开入口共用这一份换算。
fn bounds_in(zone: &jiff::tz::TimeZone, date: LocalDate) -> Result<IntervalRange, AppError> {
    let start = midnight_in(zone, date)?;
    let end = midnight_in(zone, next_date(date)?)?;
    Ok(IntervalRange::new(start, end)?)
}

/// `date` 在 `zone` 里那一天的**零点**（Unix 毫秒）。
///
/// `Date::at(0,0,0,0).to_zoned(zone)` 就是「本地零点这一刻」；缺口与重叠由 jiff 的
/// 默认消歧处理（见 [`local_day_bounds`]）。
fn midnight_in(zone: &jiff::tz::TimeZone, date: LocalDate) -> Result<i64, AppError> {
    let civil = to_civil(date)?;
    let zoned = civil
        .at(0, 0, 0, 0)
        .to_zoned(zone.clone())
        .map_err(|_| date_out_of_range())?;
    Ok(zoned.timestamp().as_millisecond())
}

/// 次日。**这是「一天不是 24 小时」的实现要点**：`end` 由它推进，而不是加常量。
fn next_date(date: LocalDate) -> Result<LocalDate, AppError> {
    let next = to_civil(date)?
        .tomorrow()
        .map_err(|_| date_out_of_range())?;
    LocalDate::from_jiff(next).map_err(Into::into)
}

/// `LocalDate` → jiff 的日历日。
///
/// `LocalDate` 已经过同一套日历校验（年 0..=9999），所以这里不会失败；真失败了也只能是
/// 内部不一致，同样按「超出可表示范围」报错，**不回落**到某个默认日期。
fn to_civil(date: LocalDate) -> Result<jiff::civil::Date, AppError> {
    jiff::civil::Date::new(date.year(), date.month(), date.day()).map_err(|_| date_out_of_range())
}

/// 「这个日期（或它的次日）超出可表示的日期范围」。
///
/// 与 [`local_date_at`] 的越界文案同一口径：**报错，不钳制**。用手写的
/// `AppError::Domain`（中文、面向用户），不新增错误码、也不新增 `DomainError` 变体。
fn date_out_of_range() -> AppError {
    AppError::Domain {
        detail: "这个日期超出可表示的日期范围。".into(),
    }
}

/// 「这不是一个可用的 IANA 时区」。
///
/// 复用 `UnknownEnumValue` 而不是新增变体：`domain/error.rs` 是 P1 已发布的形状，
/// 而这条规则的语义正是「这个值不在取值域里」。`field` 写成面向用户的中文，
/// `value` 原样回显输入——诊断时最需要的就是它。
fn unknown_timezone(raw: &str) -> AppError {
    DomainError::UnknownEnumValue {
        field: "时区",
        value: raw.to_string(),
    }
    .into()
}

// ─────────────────────────────────────────────────────────────────────────────
// 今日选择列表（P4 Task 4：F-010 的「今日选择」半边）
// ─────────────────────────────────────────────────────────────────────────────
//
// 这一半只做**人工选择**：把任务选进「某一天（某个时区）」，或取消选择。
// 它**不是排期**（裁决 R-T4-e）——不设 `Scheduled`、不建 `time_block`、不启动计时，
// 也不因为任务状态或日期的变化去搬移任何一行。完成项留在当天的列表里，
// 由 UI 按任务行自己的状态显示。

/// 今日计划的**读**请求：哪一天 + 哪个时区 + 请求方手上的库身份。
///
/// 日期与时区都是**原始输入**：它们在这里过 [`parse_local_date`] /
/// [`normalize_timezone`] 两道唯一入口（R-T4-h），调用方不要自己先校验一遍。
///
/// `Deserialize`（P7 Task 1）：这个形状本来就是 IPC 的（三个字段都是字符串），
/// 所以命令层直接收它，不再造一个逐字相同的转发 DTO。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct DailyPlanQuery {
    pub date: String,
    pub timezone: String,
    pub expected_data_epoch: String,
}

/// 读结果：这一天（这个时区）选中的任务 + 这次读看到的库身份与业务版本。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DailyPlanView {
    /// 按 `task.created_at, task.id` 稳定排序。
    pub tasks: Vec<TaskRow>,
    /// 这次读看到的库身份。
    pub data_epoch: String,
    /// 这次读看到的业务版本。读**不**改它。
    pub revision: i64,
}

/// 加入 / 移除的产物：这一天**当前**的计划 + 提交后的 `revision` 与库身份。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DailyPlanChange {
    /// 按 `task.created_at, task.id` 稳定排序。
    pub tasks: Vec<TaskRow>,
    /// 提交后的权威 `revision`；`Unchanged` 时与调用前相等。
    pub revision: i64,
    /// 这次写所在的库身份（与 `revision` **同一写事务**取得，提交后返回）。
    pub data_epoch: String,
}

/// 读某一天（某个时区）的今日选择列表。**纯读**：不开写事务、不加 `revision`、
/// 不写审计。
///
/// 为什么要带库身份：读完这一页，用户紧接着就会加入 / 移除——那些写命令的信封
/// 只带 epoch（计划表没有版本列，加入也不改任何实体，裁决 R-T4-f），所以这一次读
/// 必须先确认自己看的是哪个库，并把当时的 `revision` 一并交回（与
/// `catalog::list_tasks_filtered` 同一形状）。
pub fn plan_for(db: &Db, query: DailyPlanQuery) -> Result<DailyPlanView, AppError> {
    // 纯输入校验放在事务之前：坏日期 / 坏时区不必开事务，也轮不到 epoch 说话
    // （重新握一次手也不会让它们变合法）。
    let date = parse_local_date(&query.date)?;
    let timezone = normalize_timezone(&query.timezone)?;

    let tx = db
        .connection()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    guard_epoch(&tx, &query.expected_data_epoch)?;

    let tasks = daily_plan_repo::plan_for(&tx, &date, &timezone)?;
    let meta = require_meta(&tx)?;
    // 读事务什么都没写：直接结束它（回滚一个只读事务不改变任何事实），
    // 免得读代码的人以为这里还欠一个 `commit`。
    drop(tx);

    Ok(DailyPlanView {
        tasks,
        data_epoch: meta.data_epoch,
        revision: meta.revision,
    })
}

/// 把一个任务加入某一天（某个时区）的今日计划。
///
/// 信封只带 epoch（`WriteEnvelope::for_create` 的形状）：计划表没有版本列，加入也
/// 不改任何实体，所以**没有可校验的实体版本**，不拿别的实体的版本假装
/// （裁决 R-T4-f，与 `catalog::tag_task` 同一口径）。
///
/// 重复加入 ⇒ `Unchanged`：不写审计、不加 revision（总纲 §5 第 8 条②）。
///
/// `now` 是调用方给的墙钟毫秒，**只用作审计行的 `created_at`**：`daily_plan` 表没有
/// 时间戳列，计划行本身不记时间，也不得拿 `task.updated_at` 推断安排（02 §9）。
pub fn add_to_plan(
    db: &mut Db,
    env: WriteEnvelope,
    task_id: &str,
    date: &str,
    timezone: &str,
    now: i64,
) -> Result<WriteOutcome<DailyPlanChange>, AppError> {
    // 纯输入校验放在开事务之前：坏输入连事务都不必开（不改 revision、不写审计）。
    let date = parse_local_date(date)?;
    let timezone = normalize_timezone(timezone)?;

    let tx = write_tx(db, &env)?;
    let outcome = daily_plan_repo::add_to_plan(&tx, task_id, &date, &timezone, now)?;
    let settled = settle(&tx, outcome)?;
    let tasks = daily_plan_repo::plan_for(&tx, &date, &timezone)?;
    tx.commit().map_err(map_sqlite)?;

    Ok(settled.map(|(_, s)| DailyPlanChange {
        tasks,
        revision: s.revision,
        data_epoch: s.data_epoch,
    }))
}

/// 把一个任务从某一天（某个时区）的今日计划里移除。口径与 [`add_to_plan`] 完全对称，
/// 包括「本来就不在集合里 ⇒ `Unchanged`、零写入」。
///
/// `now` 的用途与 [`add_to_plan`] 相同：只写审计行的 `created_at`，计划行不记时间。
pub fn remove_from_plan(
    db: &mut Db,
    env: WriteEnvelope,
    task_id: &str,
    date: &str,
    timezone: &str,
    now: i64,
) -> Result<WriteOutcome<DailyPlanChange>, AppError> {
    let date = parse_local_date(date)?;
    let timezone = normalize_timezone(timezone)?;

    let tx = write_tx(db, &env)?;
    let outcome = daily_plan_repo::remove_from_plan(&tx, task_id, &date, &timezone, now)?;
    let settled = settle(&tx, outcome)?;
    let tasks = daily_plan_repo::plan_for(&tx, &date, &timezone)?;
    tx.commit().map_err(map_sqlite)?;

    Ok(settled.map(|(_, s)| DailyPlanChange {
        tasks,
        revision: s.revision,
        data_epoch: s.data_epoch,
    }))
}
