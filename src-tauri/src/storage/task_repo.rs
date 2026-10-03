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
        // schema 的 CHECK 保证了取值合法，所以解析失败只有两种可能：库被绕过 CHECK
        // 写坏过，或更新版本写入的取值被旧版本读到。两种都要**说清是哪一列**——
        // `InvalidQuery` 的文案是 "Query is not read-only"，会把人指向完全错误的方向。
        status: TaskStatus::parse(&status).ok_or_else(|| enum_error(3, "task.status", &status))?,
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
        &format!(
            "{{\"status\":\"Inbox\",\"title\":{}}}",
            serde_json::to_string(title.trim()).expect("serializing a string cannot fail")
        ),
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
    let quality = if transition.clears_quality()
        || !matches!(
            to,
            TaskStatus::Review | TaskStatus::Done | TaskStatus::Cancelled
        ) {
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
        &serde_json::json!({"status": before.status.as_str(), "quality": before.quality})
            .to_string(),
        &serde_json::json!({"status": to.as_str(), "quality": quality}).to_string(),
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

/// 把「列里的值不在取值域内」包成带列名的 SQLite 转换错误。
///
/// 用 `FromSqlConversionFailure` 而不是 `InvalidQuery`：后者的文案是
/// "Query is not read-only"，对「库里的枚举值非法」这种情况会把人指向
/// 完全错误的方向，而诊断恰恰是库损坏时最需要的东西。
pub(crate) fn enum_error(column: usize, field: &'static str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Text,
        Box::new(crate::domain::error::DomainError::UnknownEnumValue {
            field,
            value: value.to_string(),
        }),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// 估时信封与基准冻结（P2 Task 3）
// ─────────────────────────────────────────────────────────────────────────────

/// 任务的估时信封。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EstimateEnvelope {
    /// 当前估时（用户/AI 给的）。可能为空。
    pub estimated_json: Option<String>,
    /// **首次 start 时冻结下来的基准**。一旦冻结就不再随 `estimated_json` 变化
    /// （02 §9：后续改估时不改基准；显式重新定基准须保留 task_change）。
    pub baseline_estimate_json: Option<String>,
}

/// 本次调用是否真的冻结了。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreezeOutcome {
    /// 首次 start：写入了基准（`baseline` 为 `None` 表示当时本来就没有估时）。
    Frozen {
        baseline: Option<String>,
        new_version: i64,
    },
    /// 早已冻结过，本次**不动**。`baseline` 是当时冻结下来的值。
    AlreadyFrozen { baseline: Option<String> },
}

/// 读估时信封。
pub fn read_estimate(
    conn: &Connection,
    task_id: &str,
) -> Result<Option<EstimateEnvelope>, AppError> {
    conn.query_row(
        "SELECT estimated_json, baseline_estimate_json FROM task WHERE id = ?1",
        [task_id],
        |r| {
            Ok(EstimateEnvelope {
                estimated_json: r.get(0)?,
                baseline_estimate_json: r.get(1)?,
            })
        },
    )
    .optional()
    .map_err(map_sqlite)
}

/// 首次 `start` 时冻结估时基准。
///
/// **判据是「这个任务还没有任何会话」，不是「`baseline_estimate_json` 是不是空」**——
/// 否则一个本来就没估时的任务会在每次 start 时反复「冻结」（每次都是 `NULL`），
/// 而 02 §9 要的是「第一次 start 时冻结，后续不改」。用会话存在与否做标记，
/// 第一次之后无论基准是不是 `NULL` 都不再动它。
pub fn freeze_baseline_estimate(
    tx: &Transaction<'_>,
    task_id: &str,
    expected_version: i64,
    now: i64,
) -> Result<FreezeOutcome, AppError> {
    let before = get_task(tx, task_id)?.ok_or(DomainError::EmptyText { field: "task" })?;
    guard_row_version(before.row_version, expected_version)?;

    let existing: i64 = tx
        .query_row(
            "SELECT count(*) FROM work_session WHERE task_id = ?1",
            [task_id],
            |r| r.get(0),
        )
        .map_err(map_sqlite)?;
    let current: Option<String> = tx
        .query_row(
            "SELECT baseline_estimate_json FROM task WHERE id = ?1",
            [task_id],
            |r| r.get(0),
        )
        .map_err(map_sqlite)?;

    if existing > 0 {
        return Ok(FreezeOutcome::AlreadyFrozen { baseline: current });
    }

    let estimated: Option<String> = tx
        .query_row(
            "SELECT estimated_json FROM task WHERE id = ?1",
            [task_id],
            |r| r.get(0),
        )
        .map_err(map_sqlite)?;

    let n = tx
        .execute(
            "UPDATE task SET baseline_estimate_json = ?1, row_version = row_version + 1,
                             updated_at = ?2
             WHERE id = ?3 AND row_version = ?4",
            rusqlite::params![estimated, now, task_id, expected_version],
        )
        .map_err(map_sqlite)?;
    if n == 0 {
        return Err(AppError::VersionConflict {
            expected: expected_version,
            actual: -1,
        });
    }

    record_change(
        tx,
        task_id,
        &format!(
            "{{\"baseline_estimate_json\":{}}}",
            json_or_null(current.as_deref())
        ),
        &format!(
            "{{\"baseline_estimate_json\":{}}}",
            json_or_null(estimated.as_deref())
        ),
        now,
    )?;

    Ok(FreezeOutcome::Frozen {
        baseline: estimated,
        new_version: expected_version + 1,
    })
}

/// 把可选的 JSON 片段拼进审计文本：`None` 写成 `null`。
fn json_or_null(v: Option<&str>) -> String {
    v.unwrap_or("null").to_string()
}

/// 开始/继续计时必须在调用方事务内检查归属项目。
pub fn require_active_project(conn: &Connection, task_id: &str) -> Result<(), AppError> {
    let task = get_task(conn, task_id)?.ok_or_else(|| AppError::Domain {
        detail: "任务不存在。".into(),
    })?;
    if let Some(project_id) = task.project_id {
        let status: String = conn
            .query_row(
                "SELECT status FROM project WHERE id=?1",
                [project_id],
                |r| r.get(0),
            )
            .map_err(map_sqlite)?;
        if status != "active" {
            return Err(AppError::Domain {
                detail: "项目已归档，不能开始或继续计时。".into(),
            });
        }
    }
    Ok(())
}
