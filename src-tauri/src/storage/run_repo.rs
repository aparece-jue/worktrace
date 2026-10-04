//! `application_run` 原语（P7 Task 0）。
//!
//! 表本身在 P1 的 schema 里就有（`storage/schema_v1.rs:28`，字段
//! `id`/`started_at`/`clean_exit_at`），但 P1 只交付了表，**没有任何仓储原语**：
//! 之前每个测试都自己写裸 `INSERT INTO application_run`。这里补最小的三个，
//! 让「启动建 run」「显式退出写 `clean_exit_at`」不再各自拼 SQL。
//!
//! 与其他仓储同样的口径：
//! - 写函数接受调用方的 `&Transaction`，**不自行 begin/commit**，
//!   事务由 `services::bootstrap` 拥有；
//! - **不加 `revision`**：`application_run` 是运行代次簿记，不是业务数据。
//!   `app_meta.revision` 只由业务写推进（00 §5）。

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::error::AppError;

use super::db::map_sqlite;

/// `application_run` 的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub id: String,
    pub started_at: i64,
    /// 显式退出时刻；进程被杀 / 断电时保持 `NULL`。
    pub clean_exit_at: Option<i64>,
}

const RUN_SELECT: &str = "SELECT id, started_at, clean_exit_at FROM application_run";

fn read_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
    Ok(RunRow {
        id: r.get(0)?,
        started_at: r.get(1)?,
        clean_exit_at: r.get(2)?,
    })
}

/// 新建一次运行代次。**每次成功启动恰好一行**（02 §4）。
///
/// `started_at` 是调用方用已验证的时钟采样得到的挂钟毫秒——仓储不读时钟。
pub fn start_run(tx: &Transaction<'_>, id: &str, started_at: i64) -> Result<RunRow, AppError> {
    tx.execute(
        "INSERT INTO application_run(id, started_at) VALUES(?1, ?2)",
        rusqlite::params![id, started_at],
    )
    .map_err(map_sqlite)?;

    get_run(tx, id)?.ok_or_else(|| AppError::Storage {
        detail: "application_run vanished after insert".into(),
    })
}

/// 读一次运行代次。
pub fn get_run(conn: &Connection, id: &str) -> Result<Option<RunRow>, AppError> {
    conn.query_row(&format!("{RUN_SELECT} WHERE id = ?1"), [id], read_run)
        .optional()
        .map_err(map_sqlite)
}

/// 写 `clean_exit_at`，标记这次运行**显式退出**。
///
/// 返回 `true` 表示本次真的写入了；`false` 表示之前已经标记过（幂等：
/// 重复退出不覆盖第一次的时刻）。`id` 不存在时返回 `AppError::Storage`——
/// 那是调用方的编程错误（本进程的 run 一定在库里）。
pub fn mark_clean_exit(
    tx: &Transaction<'_>,
    id: &str,
    clean_exit_at: i64,
) -> Result<bool, AppError> {
    let n = tx
        .execute(
            "UPDATE application_run SET clean_exit_at = ?1
              WHERE id = ?2 AND clean_exit_at IS NULL",
            rusqlite::params![clean_exit_at, id],
        )
        .map_err(map_sqlite)?;
    if n > 0 {
        return Ok(true);
    }

    match get_run(tx, id)? {
        Some(_) => Ok(false),
        None => Err(AppError::Storage {
            detail: "application_run not found for clean exit".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::Db;
    use crate::storage::migrations::migrate;

    fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("w.db")).unwrap();
        migrate(db.connection()).unwrap();
        (dir, db)
    }

    #[test]
    fn start_run_then_read_it_back() {
        let (_dir, mut db) = db();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        let run = start_run(&tx, "run-1", 1000).unwrap();
        tx.commit().unwrap();

        assert_eq!(
            run,
            RunRow {
                id: "run-1".into(),
                started_at: 1000,
                clean_exit_at: None,
            }
        );
        assert_eq!(get_run(db.connection(), "run-1").unwrap(), Some(run));
        assert_eq!(get_run(db.connection(), "run-404").unwrap(), None);
    }

    #[test]
    fn mark_clean_exit_writes_once_and_is_idempotent() {
        let (_dir, mut db) = db();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        start_run(&tx, "run-1", 1000).unwrap();
        assert!(mark_clean_exit(&tx, "run-1", 2000).unwrap(), "首次应写入");
        assert!(
            !mark_clean_exit(&tx, "run-1", 3000).unwrap(),
            "重复应无写入"
        );
        tx.commit().unwrap();

        let run = get_run(db.connection(), "run-1").unwrap().unwrap();
        assert_eq!(run.clean_exit_at, Some(2000), "第一次的时刻不被覆盖");
    }

    #[test]
    fn marking_an_unknown_run_is_a_storage_error() {
        let (_dir, mut db) = db();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        let err = mark_clean_exit(&tx, "run-404", 1).unwrap_err();
        assert_eq!(err.code(), "STORAGE_ERROR");
    }
}
