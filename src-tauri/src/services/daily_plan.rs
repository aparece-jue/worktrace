//! 今日计划的**输入校验入口**（P4 Task 1，裁决 R6）。
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
//! # 为什么在服务层而不是 `domain/`
//!
//! 时区解析要读时区库（本机是打包进产物的 tzdb，系统时区还要问操作系统），
//! 所以它不进 `domain/`——那里只放纯规则。[`crate::domain::localdate::LocalDate`]
//! 表达的正是被校验之后的那个日期。

use crate::domain::error::DomainError;
use crate::domain::localdate::LocalDate;
use crate::error::AppError;

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
