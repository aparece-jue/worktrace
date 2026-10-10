//! `time_edit` 审计仓储（P2 新增，P3 复用）。
//!
//! 02 §9：「操作失败不得出现半个审计记录」——所以它只接受调用方的 `&Transaction`，
//! 与它所记录的改动**同事务**。P2 用它记异常分割，P3 用它记确认与历史修正。

use rusqlite::{Connection, Transaction};

use crate::error::AppError;

use super::db::map_sqlite;

/// 一条时间修正审计。
///
/// `Serialize` 的来由与 [`crate::storage::task_repo::TaskRow`] 逐字相同：
/// `HistoryDetail.edits` 直接装它交给 IPC（P8 Task 2b 的 `history_view`），
/// 不再在命令层复制一份字段做镜像 DTO。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TimeEdit {
    pub id: String,
    pub session_id: String,
    /// 改动前的区间状态（JSON）。
    pub before_json: String,
    /// 改动后的区间状态（JSON）。
    pub after_json: String,
    /// 改动原因；异常分割会写明是时钟异常。
    pub reason: Option<String>,
    pub created_at: i64,
}

/// 写一条审计。与它所描述的改动**同一事务**。
pub fn write(tx: &Transaction<'_>, edit: &TimeEdit) -> Result<(), AppError> {
    tx.execute(
        "INSERT INTO time_edit(id, session_id, before_json, after_json, reason, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            edit.id,
            edit.session_id,
            edit.before_json,
            edit.after_json,
            edit.reason,
            edit.created_at
        ],
    )
    .map_err(map_sqlite)?;
    Ok(())
}

/// 某会话的全部审计，按时间排序。P3 的修正界面与诊断都从这里读。
pub fn edits_of_session(conn: &Connection, session_id: &str) -> Result<Vec<TimeEdit>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, before_json, after_json, reason, created_at
               FROM time_edit WHERE session_id = ?1 ORDER BY created_at, id",
        )
        .map_err(map_sqlite)?;
    let rows = stmt
        .query_map([session_id], |r| {
            Ok(TimeEdit {
                id: r.get(0)?,
                session_id: r.get(1)?,
                before_json: r.get(2)?,
                after_json: r.get(3)?,
                reason: r.get(4)?,
                created_at: r.get(5)?,
            })
        })
        .map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}
