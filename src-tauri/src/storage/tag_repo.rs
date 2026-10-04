//! Tag 仓储（P4 Task 3）。
//!
//! 只管 `tag` 与 `task_tag` 两张表。V0.1 的标签是**平铺**的：层级（`parent_id`）
//! 与权重（`weight`）都属 V0.2，所以这里的插入一律写 NULL，schema 的
//! `ck_tag_parent_kind` / `weight` 取值范围只是兜底。
//!
//! 与其它仓储同一口径：写函数取调用方的 `&Transaction`，**不自行
//! `begin`/`commit`，也不自行 `bump_revision`**（一次业务写恰好加一次 revision
//! 的责任在服务层，总纲 §9）。
//!
//! 幂等（重复打标、重复去标）在这里就断掉：返回 [`WriteOutcome::Unchanged`]，
//! **不发任何写语句**（判定用的那条 SELECT 当然已经发过）。

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::domain::error::DomainError;
use crate::domain::tag::TagKind;
use crate::error::AppError;

use super::db::map_sqlite;
use super::task_repo::{enum_error, record_change, require_task};
use super::WriteOutcome;

/// `tag` 的一行。
///
/// `Serialize` 的来由与 [`crate::storage::project_repo::ProjectRow`] 逐字相同：
/// `services::catalog::TagList.items` 直接装它交给 IPC。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TagRow {
    pub id: String,
    pub kind: TagKind,
    pub name: String,
    /// V0.1 没有层级（F-005 只要求四类平铺标签），读得到但恒为 `None`。
    pub parent_id: Option<String>,
    /// 新建时为 0。V0.1 没有改名入口，所以现在一直是 0；未来的改名要用它做版本校验。
    pub row_version: i64,
    pub created_at: i64,
}

const SELECT: &str = "SELECT id, kind, name, parent_id, row_version, created_at FROM tag";

/// `task_tag` 与 `tag` 的连接：列名全部限定到 `tag`，不依赖两表列名不撞车。
const SELECT_OF_TASK: &str = "SELECT tag.id, tag.kind, tag.name, tag.parent_id, tag.row_version,
                                     tag.created_at
                              FROM tag JOIN task_tag ON task_tag.tag_id = tag.id";

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TagRow> {
    let kind: String = r.get(1)?;
    Ok(TagRow {
        id: r.get(0)?,
        // schema 的 CHECK 保证了取值合法，所以解析失败只有两种可能：库被绕过 CHECK
        // 写坏过，或更新版本写入的取值被旧版本读到。两种都要**说清是哪一列**——
        // 回落会把手边读不懂的数据伪装成合法标签。
        kind: TagKind::parse(&kind).ok_or_else(|| enum_error(1, "tag.kind", &kind))?,
        name: r.get(2)?,
        parent_id: r.get(3)?,
        row_version: r.get(4)?,
        created_at: r.get(5)?,
    })
}

pub fn get_tag(conn: &Connection, id: &str) -> Result<Option<TagRow>, AppError> {
    let sql = format!("{SELECT} WHERE id = ?1");
    conn.query_row(&sql, [id], read_row)
        .optional()
        .map_err(map_sqlite)
}

/// 列出标签，按 `created_at, id` 稳定排序（与项目/任务列表同一口径）。
///
/// `kind` 为 `None` 时列**全部四类**；不为 `None` 时只列这一类（F-005 的标签选择器
/// 按四类分组，`Context` 那一组还会被 F-002 的筛选复用）。
pub fn list_tags(conn: &Connection, kind: Option<TagKind>) -> Result<Vec<TagRow>, AppError> {
    let sql = if kind.is_some() {
        format!("{SELECT} WHERE kind = ?1 ORDER BY created_at, id")
    } else {
        format!("{SELECT} ORDER BY created_at, id")
    };
    let mut stmt = conn.prepare(&sql).map_err(map_sqlite)?;
    let rows = match kind {
        Some(k) => stmt.query_map([k.as_str()], read_row).map_err(map_sqlite)?,
        None => stmt.query_map([], read_row).map_err(map_sqlite)?,
    };
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

/// 某个任务身上的标签，按 `created_at, id` 稳定排序。
///
/// 任务不存在时返回空表：这是**读**路径，「没有标签」与「没有这个任务」在展示上
/// 是同一种状态；写路径（[`tag_task`]）才需要把未知任务单独说清楚。
pub fn tags_of_task(conn: &Connection, task_id: &str) -> Result<Vec<TagRow>, AppError> {
    let sql =
        format!("{SELECT_OF_TASK} WHERE task_tag.task_id = ?1 ORDER BY tag.created_at, tag.id");
    let mut stmt = conn.prepare(&sql).map_err(map_sqlite)?;
    let rows = stmt.query_map([task_id], read_row).map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

/// 新建标签。kind 与名字必须已过 `services::catalog::{parse_tag_kind, normalize_tag_name}`，
/// 这里仍做兜底校验——仓储不该信任任何调用方（与 `create_project` 同一口径）。
///
/// **同 kind 内同名（规范化后比较、大小写敏感）拒绝**：先查一次，给出用户读得懂的
/// 领域错误（裁决 R-T3-f：错误里能看出是哪个 kind 的哪个名字被拒），
/// `uq_tag_root(kind, name)` 仍是并发下的兜底——`unchecked_transaction` 是 DEFERRED，
/// 极端并发下唯一索引仍会拦，只是那种情况会以存储错误的形式出现。
pub fn create_tag(
    tx: &Transaction<'_>,
    id: &str,
    kind: TagKind,
    name: &str,
    now: i64,
) -> Result<TagRow, AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(DomainError::EmptyText { field: "标签名" }.into());
    }

    let taken: Option<String> = tx
        .query_row(
            "SELECT id FROM tag WHERE kind = ?1 AND name = ?2 AND parent_id IS NULL",
            rusqlite::params![kind.as_str(), name],
            |r| r.get(0),
        )
        .optional()
        .map_err(map_sqlite)?;
    if taken.is_some() {
        return Err(DomainError::TagNameTaken {
            kind: kind.as_str(),
            name: name.to_string(),
        }
        .into());
    }

    tx.execute(
        "INSERT INTO tag(id, kind, name, parent_id, row_version, created_at)
         VALUES(?1, ?2, ?3, NULL, 0, ?4)",
        rusqlite::params![id, kind.as_str(), name, now],
    )
    .map_err(map_sqlite)?;

    get_tag(tx, id)?.ok_or_else(|| AppError::Storage {
        detail: "tag vanished after insert".into(),
    })
}

/// 给任务打标（F-005）。这条关联已经在集合里 ⇒ `Unchanged`：不写、不加版本、不记 revision。
///
/// `weight` 恒写 NULL（裁决 R-T3-d：V0.1 不暴露权重入参，权重规则属 V0.2）。
/// 关联表没有版本列，任务行与标签行也都不动——所以这里**没有可校验的实体版本**，
/// 关系增删只有服务层的 epoch 守卫（裁决 R-T3-i）。
///
/// 审计写在 `task_change` 上：`{"tags":[...]}` → `{"tags":[...]}`，
/// 记的是**变化前后的完整标签集合**（与 `set_task_project` 记 `{"project_id":…}`
/// 同一形状），这样一次关联变更可以脱离当时的库状态被读懂。
pub fn tag_task(
    tx: &Transaction<'_>,
    task_id: &str,
    tag_id: &str,
    now: i64,
) -> Result<WriteOutcome<()>, AppError> {
    require_task(tx, task_id)?;
    require_tag(tx, tag_id)?;

    let before = tag_ids_of_task(tx, task_id)?;
    if before.iter().any(|id| id == tag_id) {
        return Ok(WriteOutcome::Unchanged(()));
    }

    tx.execute(
        "INSERT INTO task_tag(task_id, tag_id, weight) VALUES(?1, ?2, NULL)",
        rusqlite::params![task_id, tag_id],
    )
    .map_err(map_sqlite)?;

    // 新标签在集合里的位置由它自己的 `created_at, id` 决定，所以重读一次而不是
    // 猜着插进 `before` 里——顺序与 `tags_of_task` 必须逐字一致。
    let after = tag_ids_of_task(tx, task_id)?;
    record_change(tx, task_id, &tags_json(&before), &tags_json(&after), now)?;

    Ok(WriteOutcome::Changed(()))
}

/// 去掉任务身上的某个标签。这条关联本来就不在集合里 ⇒ `Unchanged`。
///
/// 与 [`tag_task`] 对称：同样只加一次 revision、只写一条审计，不动任何实体版本。
pub fn untag_task(
    tx: &Transaction<'_>,
    task_id: &str,
    tag_id: &str,
    now: i64,
) -> Result<WriteOutcome<()>, AppError> {
    require_task(tx, task_id)?;
    require_tag(tx, tag_id)?;

    let before = tag_ids_of_task(tx, task_id)?;
    if !before.iter().any(|id| id == tag_id) {
        return Ok(WriteOutcome::Unchanged(()));
    }

    tx.execute(
        "DELETE FROM task_tag WHERE task_id = ?1 AND tag_id = ?2",
        rusqlite::params![task_id, tag_id],
    )
    .map_err(map_sqlite)?;

    // 删除不动其余行的相对顺序，所以 after 直接从 before 推出来即可（省一次读）。
    let after: Vec<String> = before.iter().filter(|id| *id != tag_id).cloned().collect();
    record_change(tx, task_id, &tags_json(&before), &tags_json(&after), now)?;

    Ok(WriteOutcome::Changed(()))
}

/// 这个任务当前的标签 id，按 `created_at, id` 排序（与 [`tags_of_task`] 同一口径）。
fn tag_ids_of_task(conn: &Connection, task_id: &str) -> Result<Vec<String>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT tag.id FROM tag JOIN task_tag ON task_tag.tag_id = tag.id
             WHERE task_tag.task_id = ?1 ORDER BY tag.created_at, tag.id",
        )
        .map_err(map_sqlite)?;
    let rows = stmt
        .query_map([task_id], |r| r.get(0))
        .map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

/// 审计里的标签集合形状。
fn tags_json(ids: &[String]) -> String {
    serde_json::json!({ "tags": ids }).to_string()
}

/// 标签必须存在。不存在即 [`DomainError::UnknownTag`]。
///
/// 任务存在性走 [`crate::storage::task_repo::require_task`]：判据只留一处，
/// 本模块不再写第二份。
fn require_tag(conn: &Connection, id: &str) -> Result<(), AppError> {
    let found: Option<i64> = conn
        .query_row("SELECT 1 FROM tag WHERE id = ?1", [id], |r| r.get(0))
        .optional()
        .map_err(map_sqlite)?;
    match found {
        Some(_) => Ok(()),
        None => Err(DomainError::UnknownTag.into()),
    }
}
