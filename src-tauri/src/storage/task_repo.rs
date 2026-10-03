//! Task 仓储（P1 Task 4）。
//!
//! **不接受 `Connection` 做写**：所有写函数取调用方的 `&Transaction`，
//! 不自行 `begin`/`commit`，也不自行 `bump_revision`。一次业务写恰好加一次
//! revision 的责任在服务层（总纲 §9）。

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::domain::error::DomainError;
use crate::domain::task::{TaskStatus, TaskTransition, TransitionCause};
use crate::error::AppError;

use super::db::map_sqlite;
use super::guards::guard_row_version;

/// `task` 的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub id: String,
    pub project_id: Option<String>,
    pub title: String,
    pub status: TaskStatus,
    pub quality: Option<String>,
    pub row_version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

const SELECT: &str =
    "SELECT id, project_id, title, status, quality, row_version, created_at, updated_at FROM task";

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    let status: String = r.get(3)?;
    Ok(TaskRow {
        id: r.get(0)?,
        project_id: r.get(1)?,
        title: r.get(2)?,
        // schema 的 CHECK 保证了取值合法；这里若解析失败说明有人绕过 CHECK 写坏了库。
        status: TaskStatus::parse(&status).unwrap_or(TaskStatus::Inbox),
        quality: r.get(4)?,
        row_version: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

/// 新建任务。新任务状态为 `Inbox`，版本从 0 起。
///
/// 归属到**已归档**的项目一律拒绝（F-004），且在事务内检查，不只依赖 UI 过滤。
pub fn create_task(
    tx: &Transaction<'_>,
    id: &str,
    title: &str,
    project_id: Option<&str>,
    now: i64,
) -> Result<TaskRow, AppError> {
    if title.trim().is_empty() {
        return Err(DomainError::EmptyText {
            field: "task.title",
        }
        .into());
    }
    if let Some(pid) = project_id {
        let status: Option<String> = tx
            .query_row("SELECT status FROM project WHERE id = ?1", [pid], |r| {
                r.get(0)
            })
            .optional()
            .map_err(map_sqlite)?;
        match status.as_deref() {
            None => return Err(DomainError::EmptyText { field: "project" }.into()),
            Some("archived") => {
                return Err(DomainError::IntervalOpenInWrongState {
                    state: "project archived",
                }
                .into())
            }
            Some(_) => {}
        }
    }

    tx.execute(
        "INSERT INTO task(id, project_id, title, status, row_version, created_at, updated_at)
         VALUES(?1, ?2, ?3, 'Inbox', 0, ?4, ?4)",
        rusqlite::params![id, project_id, title.trim(), now],
    )
    .map_err(map_sqlite)?;

    record_change(
        tx,
        id,
        "{}",
        &format!("{{\"status\":\"Inbox\",\"title\":{}}}", json_str(title)),
        now,
    )?;

    get_task(tx, id)?.ok_or_else(|| AppError::Storage {
        detail: "task vanished after insert".into(),
    })
}

pub fn get_task(conn: &Connection, id: &str) -> Result<Option<TaskRow>, AppError> {
    let sql = format!("{SELECT} WHERE id = ?1");
    conn.query_row(&sql, [id], read_row)
        .optional()
        .map_err(map_sqlite)
}

pub fn list_tasks(conn: &Connection) -> Result<Vec<TaskRow>, AppError> {
    let sql = format!("{SELECT} ORDER BY created_at, id");
    let mut stmt = conn.prepare(&sql).map_err(map_sqlite)?;
    let rows = stmt.query_map([], read_row).map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

/// 状态跃迁。与 `task_change` **同一事务**。
///
/// 只改状态与质量；「完成/取消时结束会话」是组合服务的事（P3 Task 6），
/// 仓储不越界去动 `work_session`。
pub fn transition_task(
    tx: &Transaction<'_>,
    id: &str,
    expected_version: i64,
    to: TaskStatus,
    cause: TransitionCause,
    now: i64,
) -> Result<TaskRow, AppError> {
    let before = get_task(tx, id)?.ok_or(DomainError::EmptyText { field: "task" })?;
    guard_row_version(before.row_version, expected_version)?;

    let transition = TaskTransition::new(before.status, to, cause)?;

    // 重开要清当前质量（02 §5 末）。
    let quality = if transition.clears_quality() {
        None
    } else {
        before.quality.clone()
    };

    let n = tx
        .execute(
            "UPDATE task SET status = ?1, quality = ?2, row_version = row_version + 1,
                             updated_at = ?3
             WHERE id = ?4 AND row_version = ?5",
            rusqlite::params![to.as_str(), quality, now, id, expected_version],
        )
        .map_err(map_sqlite)?;
    if n == 0 {
        // 同事务内刚读过，n==0 只可能是并发写入——交给上层重试。
        return Err(AppError::VersionConflict {
            expected: expected_version,
            actual: -1,
        });
    }

    record_change(
        tx,
        id,
        &format!("{{\"status\":\"{}\"}}", before.status.as_str()),
        &format!("{{\"status\":\"{}\"}}", to.as_str()),
        now,
    )?;

    get_task(tx, id)?.ok_or_else(|| AppError::Storage {
        detail: "task vanished after update".into(),
    })
}

/// 写一条变更审计。与实体更新同事务——「操作失败不得出现半个审计记录」（02 §9）。
fn record_change(
    tx: &Transaction<'_>,
    task_id: &str,
    before_json: &str,
    after_json: &str,
    now: i64,
) -> Result<(), AppError> {
    tx.execute(
        "INSERT INTO task_change(id, task_id, before_json, after_json, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            task_id,
            before_json,
            after_json,
            now
        ],
    )
    .map_err(map_sqlite)?;
    Ok(())
}

/// 最小的 JSON 字符串转义，避免为了两处审计文本引入 serde 往返。
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
