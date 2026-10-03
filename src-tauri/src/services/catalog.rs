//! 项目与标签的**输入校验入口**（P4 Task 1，裁决 R6）。
//!
//! 为什么在服务层：`domain/` 只放纯规则并返回 `DomainError`，而命令的入口要按契约
//! 返回 `AppError`。后续任务（T2 项目服务、T3 标签服务）一律从这里取校验，
//! **不得各自再写一份**——「同一条规则只有一处实现」就是这几个函数存在的理由。
//!
//! 本文件在 T1 **只有校验入口**：建项目、改名、归档、建标签、打标都是 T2/T3 的业务，
//! 这里不出现事务、SQL 与仓储调用。
//!
//! 统一口径：文本输入先去掉首尾空白，全空白视为空输入；取值域匹配**大小写敏感**
//! （与 schema 的 CHECK 一致）。

use crate::domain::error::DomainError;
use crate::domain::project::ProjectStatus;
use crate::domain::tag::{self, TagKind};
use crate::error::AppError;

/// 标签类型输入的校验入口。
///
/// `Knowledge` 与任何大小写变体都不在取值域里（V0.2 才加）。
pub fn parse_tag_kind(raw: &str) -> Result<TagKind, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DomainError::EmptyText {
            field: "标签类型"
        }
        .into());
    }
    TagKind::parse(trimmed).ok_or_else(|| invalid("标签类型", raw))
}

/// 标签名输入的校验入口：去首尾空白，空名拒绝。
///
/// 唯一性（同 kind、大小写敏感）由存储层的 `uq_tag_root` 兜底，服务层的预检
/// 必须用**这个函数**产出的名字去比，两处口径才一致。
pub fn normalize_tag_name(raw: &str) -> Result<String, AppError> {
    tag::normalize_name(raw).map_err(Into::into)
}

/// 项目状态输入的校验入口（**写路径**）。
///
/// 与 `ProjectStatus::parse`（读路径）的差别：这里额外拒绝 V0.1 不写的 `done`。
/// UI 只提供「创建 / 重命名 / 归档」（F-004），放它进来等于悄悄开了一个规格里
/// 没有的状态写入；读路径仍然要读得懂它，所以那条规则留在 `domain`。
pub fn parse_project_status(raw: &str) -> Result<ProjectStatus, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DomainError::EmptyText {
            field: "项目状态"
        }
        .into());
    }
    let status = ProjectStatus::parse(trimmed).ok_or_else(|| invalid("项目状态", raw))?;
    if !status.is_writable_in_v01() {
        return Err(DomainError::NotInThisVersion {
            what: "把项目标记为已完成",
        }
        .into());
    }
    Ok(status)
}

/// 「这个值不在取值域里」。
///
/// 复用 `UnknownEnumValue` 而不是新增变体（`domain/error.rs` 是 P1 已发布的形状）；
/// `field` 是面向用户的中文，`value` 原样回显用户给的值——诊断时最需要的就是它。
fn invalid(field: &'static str, value: &str) -> AppError {
    DomainError::UnknownEnumValue {
        field,
        value: value.to_string(),
    }
    .into()
}
