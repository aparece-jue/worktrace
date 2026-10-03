//! 项目与标签的**输入校验入口**（P4 Task 1，裁决 R6）与**项目服务**（P4 Task 2）。
//!
//! 为什么校验在服务层：`domain/` 只放纯规则并返回 `DomainError`，而命令的入口要按契约
//! 返回 `AppError`。后续任务（T3 标签服务）一律从这里取校验，
//! **不得各自再写一份**——「同一条规则只有一处实现」就是这几个函数存在的理由。
//!
//! T1 只放了校验入口；T2 在此之上加了项目与任务归属的**写服务**：它们拥有事务
//! （`db.connection_mut().unchecked_transaction()`）、在事务内做 epoch/版本校验、
//! 调用仓储原语、并**恰好加一次 `revision`**。仓储仍不提交、不加 revision。
//!
//! 统一口径：文本输入先去掉首尾空白，全空白视为空输入；取值域匹配**大小写敏感**
//! （与 schema 的 CHECK 一致）。

use rusqlite::Transaction;

use crate::domain::error::DomainError;
use crate::domain::project::{self, ProjectStatus};
use crate::domain::tag::{self, TagKind};
use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::guards::guard_epoch;
use crate::storage::meta::{bump_revision, require_meta};
use crate::storage::project_repo::{self, ProjectRow};
use crate::storage::task_repo::{self, TaskRow};
use crate::storage::WriteOutcome;

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

/// 项目名输入的校验入口：去首尾空白，空名拒绝（与标签名同一口径）。
///
/// 项目名**不要求唯一**：schema 没有唯一索引，02 §2 与 F-004 也没要求。
/// 写入与比较都必须用**这个函数**的产出，服务层预检与存储层的
/// `length(trim(name)) > 0` 约束才给出一致答案。
pub fn normalize_project_name(raw: &str) -> Result<String, AppError> {
    project::normalize_name(raw).map_err(Into::into)
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

// ─────────────────────────────────────────────────────────────────────────────
// 项目与任务归属的写服务（P4 Task 2）
// ─────────────────────────────────────────────────────────────────────────────

/// 任务的归属目标（裁决 R-T2-b）。
///
/// 用枚举表达「绑定 / 解除」，**不用 `Option<Option<String>>`**：后者让「没有提供」
/// 与「解除关联」在类型上长得一样，读代码的人得去翻调用点才知道收到的是哪一种。
/// 本命令没有第三态——「这次不改归属」的调用方不该调用这个入口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectTarget {
    /// 绑定到指定项目（必须存在且 `active`）。
    Bind(String),
    /// 解除关联（`task.project_id` 置空）。
    Clear,
}

impl ProjectTarget {
    /// 仓储要的列值：`None` 表示解除。
    fn as_project_id(&self) -> Option<&str> {
        match self {
            Self::Bind(id) => Some(id.as_str()),
            Self::Clear => None,
        }
    }
}

/// 项目写命令的产物。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectChange {
    pub project: ProjectRow,
    /// 提交后的权威 `revision`；`Unchanged` 时与调用前相等。
    pub revision: i64,
}

/// 任务归属写命令的产物。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskProjectChange {
    pub task: TaskRow,
    /// 提交后的权威 `revision`；`Unchanged` 时与调用前相等。
    pub revision: i64,
}

/// 新建项目（F-004）。`WriteEnvelope::for_create`：新建实体只需要 epoch。
///
/// `now` 是调用方给的墙钟毫秒（与 `ClockSample::wall_ms`、库里的 INTEGER 同单位）。
/// 服务不自己读钟：读钟要经 `platform::clock::Clock`，而命令层（P7）已经在自己的
/// 串行边界里采样了；这里再采一次只会多出一个不受 `FakeClock` 控制的时间源。
pub fn create_project(
    db: &mut Db,
    env: WriteEnvelope,
    name: &str,
    now: i64,
) -> Result<WriteOutcome<ProjectChange>, AppError> {
    // 纯输入校验放在开事务之前：坏输入连事务都不必开（不改 revision、不写审计）。
    let name = normalize_project_name(name)?;

    let tx = write_tx(db, &env)?;
    let id = uuid::Uuid::new_v4().to_string();
    let project = project_repo::create_project(&tx, &id, &name, now)?;
    let revision = bump_revision(&tx)?;
    tx.commit().map_err(map_sqlite)?;

    Ok(WriteOutcome::Changed(ProjectChange { project, revision }))
}

/// 重命名（F-004）。`WriteEnvelope::for_update`：带 epoch 与项目版本。
///
/// 改成**同名** ⇒ `Unchanged`：不写、不加版本、不加 revision（R-T2-e）。
pub fn rename_project(
    db: &mut Db,
    env: WriteEnvelope,
    project_id: &str,
    name: &str,
    now: i64,
) -> Result<WriteOutcome<ProjectChange>, AppError> {
    let name = normalize_project_name(name)?;
    let expected = require_version(&env)?;

    let tx = write_tx(db, &env)?;
    let outcome = project_repo::rename_project(&tx, project_id, expected, &name, now)?;
    let settled =
        settle(&tx, outcome)?.map(|(project, revision)| ProjectChange { project, revision });
    tx.commit().map_err(map_sqlite)?;
    Ok(settled)
}

/// 归档（F-004）。归档**不删任务、不动历史**，只让项目不再接收新归属、
/// 并从「新建任务的选择列表」里消失。
///
/// 已经归档 ⇒ `Unchanged`：不写、不加版本、不加 revision（R-T2-e）。
pub fn archive_project(
    db: &mut Db,
    env: WriteEnvelope,
    project_id: &str,
    now: i64,
) -> Result<WriteOutcome<ProjectChange>, AppError> {
    let expected = require_version(&env)?;

    let tx = write_tx(db, &env)?;
    let outcome = project_repo::archive_project(&tx, project_id, expected, now)?;
    let settled =
        settle(&tx, outcome)?.map(|(project, revision)| ProjectChange { project, revision });
    tx.commit().map_err(map_sqlite)?;
    Ok(settled)
}

/// 新建任务时可选的项目列表：**只列 active**（F-004），按 `created_at, id` 稳定排序。
///
/// 纯读：不开写事务、不加 revision。归档项目从这里消失，但它们的历史仍在库里
/// （要完整列表用 `storage::project_repo::list_projects(conn, None)`）。
pub fn list_selectable_projects(db: &Db) -> Result<Vec<ProjectRow>, AppError> {
    project_repo::list_projects(db.connection(), Some(ProjectStatus::Active))
}

/// 给已有任务指定/解除项目（F-002 的「可选项目」）。
///
/// `WriteEnvelope::for_update`：带 epoch 与**任务**版本（归属写在任务行上）。
/// 同一事务内校验任务版本、目标项目存在且 `active`，写
/// `project_id` / `updated_at` / `row_version` 与一条 `task_change`，`revision` 恰好 +1。
/// 同值 ⇒ `Unchanged`，零写入（R-T2-e）。
pub fn set_task_project(
    db: &mut Db,
    env: WriteEnvelope,
    task_id: &str,
    target: ProjectTarget,
    now: i64,
) -> Result<WriteOutcome<TaskProjectChange>, AppError> {
    let expected = require_version(&env)?;

    let tx = write_tx(db, &env)?;
    let outcome = task_repo::set_task_project(&tx, task_id, expected, target.as_project_id(), now)?;
    let settled =
        settle(&tx, outcome)?.map(|(task, revision)| TaskProjectChange { task, revision });
    tx.commit().map_err(map_sqlite)?;
    Ok(settled)
}

/// 开一个写事务并把库身份守卫做掉。
///
/// 事务的所有权从这一行起到 `commit` 为止都归服务：仓储不 `begin`、不 `commit`。
/// `guard_epoch` **必须在写事务内**执行（总纲 §9），且只接受请求带来的期望值——
/// 「读出当前 epoch 再跟自己比」等于没有校验。
fn write_tx<'a>(db: &'a mut Db, env: &WriteEnvelope) -> Result<Transaction<'a>, AppError> {
    let tx = db
        .connection_mut()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    guard_epoch(&tx, &env.expected_data_epoch)?;
    Ok(tx)
}

/// 收口一次写原语：`Changed` 才加一次 `revision`；`Unchanged` 读回当前值（R-T2-e）。
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

/// 更新类命令必须带记录版本（`WriteEnvelope::for_update`）。
///
/// 信封里的版本是 `Option`（新建命令用 `for_create`，那里本就为空），所以更新入口
/// 要显式拒绝「没带版本」的请求：没有期望版本就没有乐观并发保护，而总纲 §9
/// 禁止拿「读出来的当前值」冒充期望值。
fn require_version(env: &WriteEnvelope) -> Result<i64, AppError> {
    env.expected_row_version.ok_or_else(|| AppError::Domain {
        detail: "缺少记录版本，无法安全地修改这条记录。".into(),
    })
}
