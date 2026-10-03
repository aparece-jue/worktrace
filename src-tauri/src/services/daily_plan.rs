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
//!   一天），永远**不**拿 `updated_at` 或 UTC 日期代替（02 §9）。
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

use rusqlite::Transaction;

use crate::domain::error::DomainError;
use crate::domain::localdate::LocalDate;
use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::storage::daily_plan_repo;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::guards::guard_epoch;
use crate::storage::meta::{bump_revision, require_meta};
use crate::storage::task_repo::TaskRow;
use crate::storage::WriteOutcome;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyPlanQuery {
    pub date: String,
    pub timezone: String,
    pub expected_data_epoch: String,
}

/// 读结果：这一天（这个时区）选中的任务 + 这次读看到的库身份与业务版本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyPlanView {
    /// 按 `task.created_at, task.id` 稳定排序。
    pub tasks: Vec<TaskRow>,
    /// 这次读看到的库身份。
    pub data_epoch: String,
    /// 这次读看到的业务版本。读**不**改它。
    pub revision: i64,
}

/// 加入 / 移除的产物：这一天**当前**的计划 + 提交后的 `revision`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyPlanChange {
    /// 按 `task.created_at, task.id` 稳定排序。
    pub tasks: Vec<TaskRow>,
    /// 提交后的权威 `revision`；`Unchanged` 时与调用前相等。
    pub revision: i64,
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

    Ok(settled.map(|(_, revision)| DailyPlanChange { tasks, revision }))
}

/// 把一个任务从某一天（某个时区）的今日计划里移除。口径与 [`add_to_plan`] 完全对称，
/// 包括「本来就不在集合里 ⇒ `Unchanged`、零写入」。
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

    Ok(settled.map(|(_, revision)| DailyPlanChange { tasks, revision }))
}

/// 开一个写事务并把库身份守卫做掉。
///
/// 与 `services::catalog` 里那份同名同形，但**没有**抽成公共模块：
/// `services/catalog.rs` 已由 T5 收口（本任务不得改它），而这段骨架只有十来行。
/// 若 P7 再加写服务，应当抽一个 `services/tx.rs`——那时的收益才盖过改动成本。
/// `guard_epoch` **必须在写事务内**执行（总纲 §9），且只接受请求带来的期望值。
fn write_tx<'a>(db: &'a mut Db, env: &WriteEnvelope) -> Result<Transaction<'a>, AppError> {
    let tx = db
        .connection_mut()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    guard_epoch(&tx, &env.expected_data_epoch)?;
    Ok(tx)
}

/// 收口一次写原语：`Changed` 才加一次 `revision`；`Unchanged` 读回当前值。
fn settle<T>(
    tx: &Transaction<'_>,
    outcome: WriteOutcome<T>,
) -> Result<WriteOutcome<(T, i64)>, AppError> {
    match outcome {
        WriteOutcome::Changed(value) => Ok(WriteOutcome::Changed((value, bump_revision(tx)?))),
        WriteOutcome::Unchanged(value) => {
            Ok(WriteOutcome::Unchanged((value, require_meta(tx)?.revision)))
        }
    }
}
