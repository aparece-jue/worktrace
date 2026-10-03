//! 项目与标签的**输入校验入口**（P4 Task 1，裁决 R6）、**项目服务**（P4 Task 2）
//! **标签服务**（P4 Task 3）与**任务捕获 / 筛选**（P4 Task 5）。
//!
//! 为什么校验在服务层：`domain/` 只放纯规则并返回 `DomainError`，而命令的入口要按契约
//! 返回 `AppError`。后续任务一律从这里取校验，
//! **不得各自再写一份**——「同一条规则只有一处实现」就是这几个函数存在的理由。
//!
//! T1 只放了校验入口；T2 在此之上加了项目与任务归属的**写服务**：它们拥有事务
//! （`db.connection_mut().unchecked_transaction()`）、在事务内做 epoch/版本校验、
//! 调用仓储原语、并**恰好加一次 `revision`**。T3 用同一套骨架加标签与打标：
//! 关系增删是 epoch-only 的集合操作，无变化时不写审计、不加任何版本。
//! T5 补上任务的捕获（新建 `Inbox`）与理清为待办，以及那条**只读**的筛选查询
//! （它在自己的读事务里做 epoch 校验，把数据 / 总数 / epoch / revision 一起交回）。
//! 仓储仍不提交、不加 revision。
//!
//! COMP-01/COMP-03 收尾：写结果 DTO 一并带 `data_epoch`（与 `revision` 同在写事务
//! 里读回）；项目与标签的查询入口都收**请求带来的** epoch，并在同一个读事务里校验它、
//! 取数据与元数据，返回 `{items, data_epoch, revision}` 信封；完整项目列表
//! （[`list_projects`]，可选状态过滤）也在本模块，调用方不直调仓储。
//!
//! 统一口径：文本输入先去掉首尾空白，全空白视为空输入；取值域匹配**大小写敏感**
//! （与 schema 的 CHECK 一致）。

use crate::domain::error::DomainError;
use crate::domain::project::{self, ProjectStatus};
use crate::domain::tag::{self, TagKind};
use crate::domain::task::{TaskStatus, TransitionCause};
use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::guards::{guard_epoch, guard_row_version};
use crate::storage::meta::{bump_revision, require_meta};
use crate::storage::project_repo::{self, ProjectRow};
use crate::storage::tag_repo::{self, TagRow};
use crate::storage::task_repo::{self, Page, TaskFilter, TaskRow};
use crate::storage::WriteOutcome;

use super::tx::{settle, write_tx};

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
    /// 这次写所在的库身份（与 `revision` **同一写事务**取得，提交后返回）。
    pub data_epoch: String,
}

/// 任务归属写命令的产物。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskProjectChange {
    pub task: TaskRow,
    /// 提交后的权威 `revision`；`Unchanged` 时与调用前相等。
    pub revision: i64,
    /// 这次写所在的库身份（与 `revision` **同一写事务**取得，提交后返回）。
    pub data_epoch: String,
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
    // 库身份与版本在同一事务里读回（COMP-01 裁决 R-A）：提交后补读会让「这次写
    // 发生在哪个库」由另一个时刻的快照回答。
    let data_epoch = require_meta(&tx)?.data_epoch;
    tx.commit().map_err(map_sqlite)?;

    Ok(WriteOutcome::Changed(ProjectChange {
        project,
        revision,
        data_epoch,
    }))
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
    let settled = settle(&tx, outcome)?.map(|(project, s)| ProjectChange {
        project,
        revision: s.revision,
        data_epoch: s.data_epoch,
    });
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
    let settled = settle(&tx, outcome)?.map(|(project, s)| ProjectChange {
        project,
        revision: s.revision,
        data_epoch: s.data_epoch,
    });
    tx.commit().map_err(map_sqlite)?;
    Ok(settled)
}

/// 完整项目列表的**读结果信封**（COMP-01）：`items` 与 `data_epoch` / `revision`
/// 出自**同一个读事务**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectList {
    /// 按 `created_at, id` 稳定排序。
    pub items: Vec<ProjectRow>,
    /// 这次读看到的库身份。
    pub data_epoch: String,
    /// 这次读看到的业务版本。读**不**改它。
    pub revision: i64,
}

/// 标签列表的**读结果信封**（COMP-01）：`items` 与 `data_epoch` / `revision`
/// 出自**同一个读事务**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagList {
    /// 按 `created_at, id` 稳定排序。
    pub items: Vec<TagRow>,
    /// 这次读看到的库身份。
    pub data_epoch: String,
    /// 这次读看到的业务版本。读**不**改它。
    pub revision: i64,
}

/// 完整项目列表（COMP-03）：`status` 为 `None` 时**不限制状态**——归档与 `done`
/// 的历史都在里面；为 `Some(s)` 时只列该状态。仓储的
/// `project_repo::list_projects(conn, Option<ProjectStatus>)` 已经是这个语义，直接用。
///
/// 纯读：不开写事务、不加 `revision`。为了「数据与元数据出自同一读事务」，这里显式
/// 开一个只读事务，`guard_epoch` 在事务内跑（形状与 [`list_tasks_filtered`] 一致）。
pub fn list_projects(
    db: &Db,
    expected_data_epoch: &str,
    status: Option<ProjectStatus>,
) -> Result<ProjectList, AppError> {
    let tx = db
        .connection()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    guard_epoch(&tx, expected_data_epoch)?;

    let items = project_repo::list_projects(&tx, status)?;
    let meta = require_meta(&tx)?;
    // 读事务什么都没写：直接结束它（回滚一个只读事务不改变任何事实），
    // 免得读代码的人以为这里还欠一个 `commit`。
    drop(tx);

    Ok(ProjectList {
        items,
        data_epoch: meta.data_epoch,
        revision: meta.revision,
    })
}

/// 新建任务时可选的项目列表：**只列 active**（F-004），按 `created_at, id` 稳定排序。
///
/// 归档/done 项目从这里消失，但它们的历史仍在库里——完整列表用本模块的
/// [`list_projects`]（`status = None` 含全部状态）。两个入口共用同一段读事务骨架。
pub fn list_selectable_projects(
    db: &Db,
    expected_data_epoch: &str,
) -> Result<ProjectList, AppError> {
    list_projects(db, expected_data_epoch, Some(ProjectStatus::Active))
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
    let settled = settle(&tx, outcome)?.map(|(task, s)| TaskProjectChange {
        task,
        revision: s.revision,
        data_epoch: s.data_epoch,
    });
    tx.commit().map_err(map_sqlite)?;
    Ok(settled)
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

// ─────────────────────────────────────────────────────────────────────────────
// 标签与任务打标（P4 Task 3）
// ─────────────────────────────────────────────────────────────────────────────

/// 新建标签写命令的产物。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagChange {
    pub tag: TagRow,
    /// 提交后的权威 `revision`。
    pub revision: i64,
    /// 这次写所在的库身份（与 `revision` **同一写事务**取得，提交后返回）。
    pub data_epoch: String,
}

/// 打标 / 去标写命令的产物：这个任务**当前**的标签集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTagsChange {
    pub tags: Vec<TagRow>,
    /// 提交后的权威 `revision`；`Unchanged` 时与调用前相等。
    pub revision: i64,
    /// 这次写所在的库身份（与 `revision` **同一写事务**取得，提交后返回）。
    pub data_epoch: String,
}

/// 新建标签（F-005）。`WriteEnvelope::for_create`：新建实体只需要 epoch。
///
/// 三类输入各走**唯一**的校验入口：kind 走 [`parse_tag_kind`]，名字走
/// [`normalize_tag_name`]，层级走 `domain::tag::ensure_no_parent`——非空 `parent_id`
/// 一律拒绝（裁决 R-T3-e：层级属 V0.2），空白按「未提供」。
/// 同 kind 内重名由仓储在事务内拒绝（裁决 R-T3-f）。一次成功的新建恰好 `revision + 1`。
pub fn create_tag(
    db: &mut Db,
    env: WriteEnvelope,
    kind: &str,
    name: &str,
    parent_id: Option<&str>,
    now: i64,
) -> Result<WriteOutcome<TagChange>, AppError> {
    // 纯输入校验放在开事务之前：坏输入连事务都不必开（不改 revision、不写审计）。
    let kind = parse_tag_kind(kind)?;
    let name = normalize_tag_name(name)?;
    tag::ensure_no_parent(parent_id)?;

    let tx = write_tx(db, &env)?;
    let id = uuid::Uuid::new_v4().to_string();
    let tag = tag_repo::create_tag(&tx, &id, kind, &name, now)?;
    let revision = bump_revision(&tx)?;
    // 库身份与版本在同一事务里读回（COMP-01 裁决 R-A）。
    let data_epoch = require_meta(&tx)?.data_epoch;
    tx.commit().map_err(map_sqlite)?;

    Ok(WriteOutcome::Changed(TagChange {
        tag,
        revision,
        data_epoch,
    }))
}

/// 标签选择器的数据源：全部标签，可按 kind 过滤（四类各一组）。
///
/// 纯读：不开写事务、不加 revision。与其它查询入口同一形状——`guard_epoch` 与
/// 元数据读都落在**同一个读事务**里，返回 [`TagList`] 信封。
pub fn list_tags(
    db: &Db,
    expected_data_epoch: &str,
    kind: Option<TagKind>,
) -> Result<TagList, AppError> {
    let tx = db
        .connection()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    guard_epoch(&tx, expected_data_epoch)?;

    let items = tag_repo::list_tags(&tx, kind)?;
    let meta = require_meta(&tx)?;
    // 读事务什么都没写：直接结束它（回滚一个只读事务不改变任何事实）。
    drop(tx);

    Ok(TagList {
        items,
        data_epoch: meta.data_epoch,
        revision: meta.revision,
    })
}

/// 某个任务身上的标签。纯读，口径与信封同 [`list_tags`]。
///
/// 写路径（[`tag_task`] / [`untag_task`]）在同一写事务里用
/// `tag_repo::tags_of_task(&tx, …)` 读回集合，不经过这里。
pub fn tags_of_task(
    db: &Db,
    expected_data_epoch: &str,
    task_id: &str,
) -> Result<TagList, AppError> {
    let tx = db
        .connection()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    guard_epoch(&tx, expected_data_epoch)?;

    let items = tag_repo::tags_of_task(&tx, task_id)?;
    let meta = require_meta(&tx)?;
    // 读事务什么都没写：直接结束它（回滚一个只读事务不改变任何事实）。
    drop(tx);

    Ok(TagList {
        items,
        data_epoch: meta.data_epoch,
        revision: meta.revision,
    })
}

/// 打标（F-005）：把**一个**标签加到**一个**任务上。
///
/// 这是显式集合操作，不是「整体替换这个任务的标签集合」——后者需要一套关联集合
/// 的版本契约，首版明确不做（裁决 R-T3-c）。
///
/// 信封只带 epoch：`task_tag` 没有版本列，打标也不改 task/tag 的任何字段，所以
/// **没有可校验的实体版本**，不拿别的实体的版本假装（裁决 R-T3-i）——`for_create`
/// 在这里表达的就是「只带库身份」这一形状。
/// 同一条关联重复提交 ⇒ `Unchanged`：不写审计、不加任何版本、不加 revision（R-T3-b）。
pub fn tag_task(
    db: &mut Db,
    env: WriteEnvelope,
    task_id: &str,
    tag_id: &str,
    now: i64,
) -> Result<WriteOutcome<TaskTagsChange>, AppError> {
    let tx = write_tx(db, &env)?;
    let outcome = tag_repo::tag_task(&tx, task_id, tag_id, now)?;
    let settled = settle(&tx, outcome)?;
    let tags = tag_repo::tags_of_task(&tx, task_id)?;
    tx.commit().map_err(map_sqlite)?;

    Ok(settled.map(|(_, s)| TaskTagsChange {
        tags,
        revision: s.revision,
        data_epoch: s.data_epoch,
    }))
}

/// 去标：把**一个**标签从**一个**任务上移除。口径与 [`tag_task`] 完全对称，
/// 包括「本来就不在集合里 ⇒ `Unchanged`、零写入」。
pub fn untag_task(
    db: &mut Db,
    env: WriteEnvelope,
    task_id: &str,
    tag_id: &str,
    now: i64,
) -> Result<WriteOutcome<TaskTagsChange>, AppError> {
    let tx = write_tx(db, &env)?;
    let outcome = tag_repo::untag_task(&tx, task_id, tag_id, now)?;
    let settled = settle(&tx, outcome)?;
    let tags = tag_repo::tags_of_task(&tx, task_id)?;
    tx.commit().map_err(map_sqlite)?;

    Ok(settled.map(|(_, s)| TaskTagsChange {
        tags,
        revision: s.revision,
        data_epoch: s.data_epoch,
    }))
}

// ─────────────────────────────────────────────────────────────────────────────
// 任务捕获、理清为待办与筛选（P4 Task 5）
// ─────────────────────────────────────────────────────────────────────────────

/// 任务写命令的产物（捕获与理清为待办共用这一形状）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskChange {
    pub task: TaskRow,
    /// 提交后的权威 `revision`。
    pub revision: i64,
    /// 这次写所在的库身份（与 `revision` **同一写事务**取得，提交后返回）。
    pub data_epoch: String,
}

/// 捕获一个任务（F-002 的 Inbox 入口）。`WriteEnvelope::for_create`：新建只需要 epoch。
///
/// 复用 P1 的 `task_repo::create_task`（新任务状态恒为 `Inbox`、版本从 0 起）：
/// 服务这一层负责的只有「开事务 + 校验库身份 + 恰好加一次 `revision`」。
/// 标题的空值在这里先判一次——`EmptyText` 的字段名会被拼进用户看到的句子，
/// 所以传的是「任务标题」而不是 P1 仓储里的列名 `task.title`（仓储那道仍是兜底）。
pub fn create_task(
    db: &mut Db,
    env: WriteEnvelope,
    title: &str,
    project_id: Option<&str>,
    now: i64,
) -> Result<WriteOutcome<TaskChange>, AppError> {
    // 纯输入校验放在开事务之前：坏输入连事务都不必开（不改 revision、不写审计）。
    let title = title.trim();
    if title.is_empty() {
        return Err(DomainError::EmptyText {
            field: "任务标题"
        }
        .into());
    }

    let tx = write_tx(db, &env)?;
    let id = uuid::Uuid::new_v4().to_string();
    let task = task_repo::create_task(&tx, &id, title, project_id, now)?;
    let revision = bump_revision(&tx)?;
    // 库身份与版本在同一事务里读回（COMP-01 裁决 R-A）。
    let data_epoch = require_meta(&tx)?.data_epoch;
    tx.commit().map_err(map_sqlite)?;

    Ok(WriteOutcome::Changed(TaskChange {
        task,
        revision,
        data_epoch,
    }))
}

/// 把任务理清为待办（F-002）。`WriteEnvelope::for_update`（epoch + **任务**版本）。
///
/// # 只接受没有在计时的 `Inbox` / `Clarifying`
///
/// 别处的状态编排归 P3（裁决 R-T5-e）：`Doing → Ready`、`Review → Ready` 在 02 §5
/// 的跃迁表里本来就是合法的，所以这道状态闸必须在这里显式加——不能只靠
/// `task_repo::transition_task` 的跃迁表，那会把「P3 的状态联动」从这个入口放进来。
///
/// # 顺序与 `set_task_project` 一致
///
/// 版本守卫排在状态闸之前：请求方手上的版本旧了就该先刷新，而不是拿到一句关于
/// 「当前处于什么状态」的判断。跃迁复用 P1 原语（它自己写 `task_change`），
/// 同一事务里 `revision` 恰好 +1。
pub fn clarify_ready(
    db: &mut Db,
    env: WriteEnvelope,
    task_id: &str,
    now: i64,
) -> Result<TaskChange, AppError> {
    let expected = require_version(&env)?;

    let tx = write_tx(db, &env)?;
    let before = task_repo::get_task(&tx, task_id)?.ok_or(DomainError::UnknownTask)?;
    guard_row_version(before.row_version, expected)?;

    if !matches!(before.status, TaskStatus::Inbox | TaskStatus::Clarifying) {
        return Err(DomainError::TaskNotClarifiable {
            status: before.status.as_str(),
        }
        .into());
    }
    // 正在计时的任务不能理清。正常路径上有会话的任务已经是 `Doing`，上面那道闸先
    // 拦住了；这一条守的是「状态与事实对不上」的数据（与 `set_task_project` 同一条规则）。
    if task_repo::has_running_session(&tx, task_id)? {
        return Err(DomainError::TaskHasRunningSession.into());
    }

    let task = task_repo::transition_task(
        &tx,
        task_id,
        expected,
        TaskStatus::Ready,
        TransitionCause::User,
        now,
    )?;
    let revision = bump_revision(&tx)?;
    // 库身份与版本在同一事务里读回（COMP-01 裁决 R-A）。
    let data_epoch = require_meta(&tx)?.data_epoch;
    tx.commit().map_err(map_sqlite)?;

    Ok(TaskChange {
        task,
        revision,
        data_epoch,
    })
}

/// 筛选查询的请求（R-T5-b）：条件 + 分页窗口 + 请求方手上的库身份。
///
/// 库身份是**必须**的：查询会把数据、总数与当时的 `data_epoch` / `revision` 一起
/// 交回，调用方随后要拿这组值去发写命令——所以它得先确认自己看的是哪个库。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskQuery {
    pub filter: TaskFilter,
    pub page: Page,
    pub expected_data_epoch: String,
}

/// 筛选查询的结果：四项都出自**同一个读事务**（R-T5-b）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskQueryResult {
    /// 这一页的任务（`created_at, id` 稳定排序）。
    pub tasks: Vec<TaskRow>,
    /// 满足条件的**总数**，与分页窗口无关。
    pub total: i64,
    /// 这次读看到的库身份。
    pub data_epoch: String,
    /// 这次读看到的业务版本。查询**不**改它。
    pub revision: i64,
}

/// 任务筛选查询（F-002 的轻量 GTD 列表）。
///
/// **纯读**：不开写事务、不加 `revision`、不写审计。为了「数据 / total / epoch /
/// revision 出自同一读事务」，这里显式开一个只读事务，`guard_epoch` 在事务内跑，
/// 查询与元数据读落在同一个快照上。
///
/// 情境（上下文）标签必须真的存在且属于 `Context` 类（04 F-005）：那是**类别**
/// 筛选，拿「领域」类标签当上下文条件是调用方搞错了，要拒绝；而项目 ID 是直接键，
/// 指向一个不存在的项目只意味着「那里没有任务」，不算非法输入。
pub fn list_tasks_filtered(db: &Db, query: TaskQuery) -> Result<TaskQueryResult, AppError> {
    let tx = db
        .connection()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    guard_epoch(&tx, &query.expected_data_epoch)?;

    if let Some(tag_id) = query.filter.context_tag_id.as_deref() {
        let tag = tag_repo::get_tag(&tx, tag_id)?.ok_or(DomainError::UnknownTag)?;
        if tag.kind != TagKind::Context {
            return Err(DomainError::ContextTagRequired {
                kind: tag.kind.as_str(),
            }
            .into());
        }
    }

    let task_page = task_repo::list_tasks_filtered(&tx, &query.filter, query.page)?;
    let meta = require_meta(&tx)?;
    // 读事务什么都没写：直接结束它（回滚一个只读事务不改变任何事实），
    // 免得读代码的人以为这里还欠一个 `commit`。
    drop(tx);

    Ok(TaskQueryResult {
        tasks: task_page.tasks,
        total: task_page.total,
        data_epoch: meta.data_epoch,
        revision: meta.revision,
    })
}
