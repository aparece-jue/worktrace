//! Project 仓储（P4 Task 2）。
//!
//! 只管 `project` 表。任务的归属写在 `storage::task_repo::set_task_project`——
//! 谁的行谁负责（控制器裁决 R-T2-a）。
//!
//! 与 P1 的其他仓储同一口径：写函数取调用方的 `&Transaction`，**不自行
//! `begin`/`commit`，也不自行 `bump_revision`**（一次业务写恰好加一次 revision
//! 的责任在服务层，总纲 §9）。
//!
//! 幂等（重命名成同名、归档已归档）在这里就断掉：返回
//! [`WriteOutcome::Unchanged`]，**不发任何写语句**（判定用的那条 SELECT 当然已经发过）。

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::domain::error::DomainError;
use crate::domain::project::ProjectStatus;
use crate::error::AppError;

use super::db::map_sqlite;
use super::guards::guard_row_version;
use super::task_repo::enum_error;
use super::WriteOutcome;

/// `project` 的一行。
///
/// `Serialize` 是给 IPC 用的（P7 Task 1）：`services::catalog::ProjectList.items`
/// 直接装它，查询响应原样把它交给前端。**派生在行类型上**而不是另写一份 DTO——
/// 行类型是这条投影的唯一出处，第二份形状迟早与列漂移。`status` 的 JSON 形状由
/// `domain::project::ProjectStatus` 那份手写实现决定（就是落库字符串）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    /// V0.1 没有描述的写入口（F-004 只要求创建/改名/归档），读得到但恒为 `None`。
    pub description: Option<String>,
    pub row_version: i64,
    pub status: ProjectStatus,
    pub created_at: i64,
    pub updated_at: i64,
}

const SELECT: &str =
    "SELECT id, name, description, row_version, status, created_at, updated_at FROM project";

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRow> {
    let status: String = r.get(4)?;
    Ok(ProjectRow {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        row_version: r.get(3)?,
        // 读路径读得懂 `done`（裁决 R-T2-c）；解析失败只可能是库被绕过 CHECK 写坏过，
        // 或更新版本写入的取值被旧版本读到——按列名报错，不回落默认值。
        status: ProjectStatus::parse(&status)
            .ok_or_else(|| enum_error(4, "project.status", &status))?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

pub fn get_project(conn: &Connection, id: &str) -> Result<Option<ProjectRow>, AppError> {
    let sql = format!("{SELECT} WHERE id = ?1");
    conn.query_row(&sql, [id], read_row)
        .optional()
        .map_err(map_sqlite)
}

/// 列出项目，按 `created_at, id` 稳定排序（与任务列表同一口径）。
///
/// `status` 为 `None` 时列**全部**（含 `done`——读路径读得懂它，R-T2-c）；
/// 「新建任务的选择列表」传 `Some(Active)`，归档项目因此自然消失。
/// 过滤只发生在这里，**不是**删除：归档项目的历史仍然完整可读。
pub fn list_projects(
    conn: &Connection,
    status: Option<ProjectStatus>,
) -> Result<Vec<ProjectRow>, AppError> {
    let sql = if status.is_some() {
        format!("{SELECT} WHERE status = ?1 ORDER BY created_at, id")
    } else {
        format!("{SELECT} ORDER BY created_at, id")
    };
    let mut stmt = conn.prepare(&sql).map_err(map_sqlite)?;
    let rows = match status {
        Some(s) => stmt.query_map([s.as_str()], read_row).map_err(map_sqlite)?,
        None => stmt.query_map([], read_row).map_err(map_sqlite)?,
    };
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

/// 新建项目：状态恒为 `active`，版本从 0 起（F-004：V0.1 只能创建与归档）。
pub fn create_project(
    tx: &Transaction<'_>,
    id: &str,
    name: &str,
    now: i64,
) -> Result<ProjectRow, AppError> {
    let name = name.trim();
    if name.is_empty() {
        // 兜底：服务层已经用同一个口径校验过（`services::catalog`），这里再挡一次
        // 与 `create_task` 对标题的做法一致——仓储不该信任任何调用方。
        return Err(DomainError::EmptyText { field: "项目名" }.into());
    }

    tx.execute(
        "INSERT INTO project(id, name, description, row_version, status, created_at, updated_at)
         VALUES(?1, ?2, NULL, 0, 'active', ?3, ?3)",
        rusqlite::params![id, name, now],
    )
    .map_err(map_sqlite)?;

    get_project(tx, id)?.ok_or_else(|| AppError::Storage {
        detail: "project vanished after insert".into(),
    })
}

/// 重命名。名字必须已过 `services::catalog::normalize_project_name`（这里只兜底去空白）。
///
/// 新名字与**规范化后**的现名相同 ⇒ `Unchanged`：不写、不加版本、不记 revision。
pub fn rename_project(
    tx: &Transaction<'_>,
    id: &str,
    expected_version: i64,
    name: &str,
    now: i64,
) -> Result<WriteOutcome<ProjectRow>, AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(DomainError::EmptyText { field: "项目名" }.into());
    }

    let before = require_project(tx, id)?;
    // 版本守卫排在幂等判断之前：请求方看的那一行已经旧了，就不能把它的请求
    // 当成「与现状一致」而报成功。
    guard_row_version(before.row_version, expected_version)?;
    ensure_writable_in_v01(&before)?;

    if before.name == name {
        return Ok(WriteOutcome::Unchanged(before));
    }

    let n = tx
        .execute(
            "UPDATE project SET name = ?1, row_version = row_version + 1, updated_at = ?2
             WHERE id = ?3 AND row_version = ?4",
            rusqlite::params![name, now, id, expected_version],
        )
        .map_err(map_sqlite)?;
    if n == 0 {
        // 同事务内刚读过，n==0 只可能是并发写入——交给上层重试。
        return Err(AppError::VersionConflict {
            expected: expected_version,
            actual: -1,
        });
    }

    Ok(WriteOutcome::Changed(require_project(tx, id)?))
}

/// 归档：只把状态置为 `archived`。
///
/// **不删任务、不动历史**（02 §2 末、F-004）：任务行、归属、审计、工时全部原样保留，
/// 项目只是从「新建任务的选择列表」里消失，并且不再接收新的归属。
/// 已经是 `archived` ⇒ `Unchanged`。
pub fn archive_project(
    tx: &Transaction<'_>,
    id: &str,
    expected_version: i64,
    now: i64,
) -> Result<WriteOutcome<ProjectRow>, AppError> {
    let before = require_project(tx, id)?;
    guard_row_version(before.row_version, expected_version)?;
    ensure_writable_in_v01(&before)?;

    if before.status == ProjectStatus::Archived {
        return Ok(WriteOutcome::Unchanged(before));
    }

    let n = tx
        .execute(
            "UPDATE project SET status = 'archived', row_version = row_version + 1, updated_at = ?1
             WHERE id = ?2 AND row_version = ?3",
            rusqlite::params![now, id, expected_version],
        )
        .map_err(map_sqlite)?;
    if n == 0 {
        return Err(AppError::VersionConflict {
            expected: expected_version,
            actual: -1,
        });
    }

    Ok(WriteOutcome::Changed(require_project(tx, id)?))
}

/// 读项目，不存在即 [`DomainError::UnknownProject`]。
fn require_project(conn: &Connection, id: &str) -> Result<ProjectRow, AppError> {
    get_project(conn, id)?.ok_or_else(|| DomainError::UnknownProject.into())
}

/// V0.1 的项目写范围：`active`/`archived`（[`ProjectStatus::is_writable_in_v01`]）。
///
/// `done` 是后续版本的状态：schema 与读路径都保留它，而本版**读得懂但不改它**——
/// 顺手把自己读不懂的状态覆盖掉，比拒绝危险得多。
fn ensure_writable_in_v01(p: &ProjectRow) -> Result<(), AppError> {
    if p.status.is_writable_in_v01() {
        Ok(())
    } else {
        Err(DomainError::NotInThisVersion {
            what: "修改已完成的项目",
        }
        .into())
    }
}
