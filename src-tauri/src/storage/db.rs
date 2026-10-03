//! 数据库执行边界（P1 Task 2）。
//!
//! ## 选定的线程方案
//!
//! 06 §4 的实验结论（见 `IMPLEMENTATION-NOTES.md`：两种边界都**必须**串行，
//! 这是单连接 + SQLite 写锁的固有性质；差别只在**谁的线程被阻塞**）。
//!
//! 本层因此**不**自作主张加 `Mutex`：`Db` 就是连接的持有者，读写经
//! `connection()` / `connection_mut()` 借出。把连接放到哪个线程、要不要异步化，
//! 由调用方（P6/P7 的进程接线）决定。理由：
//! - `Connection` 是 `Send` 但 `!Sync`，加锁与否是**消费者**的拓扑问题；
//! - 在库里塞一个 `Mutex` 会让 `&mut self` 与事务生命周期纠缠不清，而
//!   P1 Task 4 要求仓储接受 `&Transaction`，事务必须由服务层自由持有。
//!
//! 一条硬约束写在文档与评审清单里：**不得把 `Connection` 跨 `await` 持有，
//! 也不得在 UI 回调里直接跑长操作**——SQLite 调用是阻塞的。

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::error::AppError;

/// 等待锁的时长。超过即返回 `SQLITE_BUSY`，由调用方映射成 `STORAGE_ERROR`。
///
/// 5 秒的依据：最长的正常写事务是心跳（单行 upsert），备份走在线备份接口而非
/// 长事务。给到 5 秒足以覆盖一次 checkpoint 的抖动，又不至于让界面卡死。
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// 数据库句柄。
///
/// 只负责「打开并配好一个连接」。事务的开启与提交属于服务层（P1 Task 4）。
pub struct Db {
    conn: Connection,
    /// 内存库没有文件路径，备份与 WAL 相关行为据此分流。
    path: Option<std::path::PathBuf>,
}

impl Db {
    /// 打开磁盘数据库。会验证 WAL 生效——磁盘库必须走 WAL，否则并发读写会互相阻塞。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open(&path).map_err(map_sqlite)?;
        configure(&conn)?;

        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
            .map_err(map_sqlite)?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(AppError::Storage {
                detail: format!("expected journal_mode=wal on disk db, got {mode}"),
            });
        }

        Ok(Self {
            conn,
            path: Some(path),
        })
    }

    /// 打开内存数据库。
    ///
    /// **不要求 WAL**：内存库的 journal_mode 恒为 `memory`，强求 WAL 会直接失败。
    /// 其余 PRAGMA 与磁盘库一致，测试才有意义。
    pub fn open_in_memory() -> Result<Self, AppError> {
        let conn = Connection::open_in_memory().map_err(map_sqlite)?;
        configure(&conn)?;
        Ok(Self { conn, path: None })
    }

    /// 只读借用。仓储的查询函数取这个。
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// 可变借用。服务层用它开启事务。
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// 磁盘库的路径；内存库为 `None`。
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// 拆出连接，供备份等需要独占的场景使用（P6）。
    pub fn into_connection(self) -> Connection {
        self.conn
    }
}

/// 每个连接都必须重新设置的 PRAGMA。
///
/// `foreign_keys` 是**连接级**开关且默认关闭——忘记设置会让所有 `REFERENCES`
/// 形同虚设，而且测试里看不出来。放在这里一次设好。
fn configure(conn: &Connection) -> Result<(), AppError> {
    conn.execute_batch("PRAGMA foreign_keys=ON;")
        .map_err(map_sqlite)?;
    conn.busy_timeout(BUSY_TIMEOUT).map_err(map_sqlite)?;

    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .map_err(map_sqlite)?;
    if fk != 1 {
        return Err(AppError::Storage {
            detail: "foreign_keys did not turn on".into(),
        });
    }
    Ok(())
}

/// 把 `rusqlite::Error` 收进契约错误。
///
/// **脱敏**：SQLite 的错误文本可能带上表名、列名甚至 SQL 片段，一律不进
/// 面向用户的 `message`，只留在 `detail` 里供诊断。
pub(crate) fn map_sqlite(e: rusqlite::Error) -> AppError {
    AppError::Storage {
        detail: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_db_has_foreign_keys_on() {
        let db = Db::open_in_memory().expect("open");
        let fk: i64 = db
            .connection()
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1, "内存库也必须开外键");
        assert!(db.path().is_none());
    }

    #[test]
    fn in_memory_db_reports_memory_journal_and_that_is_fine() {
        // 计划原文：「磁盘数据库验证 WAL，内存测试不要求 WAL」。
        let db = Db::open_in_memory().expect("open");
        let mode: String = db
            .connection()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "memory");
    }

    #[test]
    fn disk_db_actually_uses_wal() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("w.db")).expect("open");
        let mode: String = db
            .connection()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        assert!(db.path().is_some());
    }

    #[test]
    fn busy_timeout_is_configured() {
        let db = Db::open_in_memory().expect("open");
        let t: i64 = db
            .connection()
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(t, 5000);
    }
}
