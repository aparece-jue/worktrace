//! 迁移：把库推进到 [`SCHEMA_VERSION`]。
//!
//! 三条不可退让的性质（P1 Task 2）：
//! 1. **整批原子**：DDL 与 `user_version` 在同一个事务里，失败回滚成「一张表都没有」
//!    或「保持原样」，绝不出现半套表。
//! 2. **幂等**：已经是当前版本的库再跑一次是空操作。
//! 3. **拒绝未来版本**：库版本高于本程序时直接失败，不尝试降级。
//!
//! 备份不在这里做。P1 原文要求「现有库迁移前由初始化流程做一致备份，备份失败拒绝
//! 迁移」——那是 P6 的初始化编排（单实例 → 备份 → 迁移），本模块只负责迁移本身，
//! 由调用方保证顺序。这样拆是因为备份要停计时、关连接，属于进程级动作。

use rusqlite::Connection;

use crate::error::AppError;

use super::db::map_sqlite;
use super::schema_v1::SCHEMA_V1_SQL;

/// 当前程序理解的 schema 版本。写进 `PRAGMA user_version`。
pub const SCHEMA_VERSION: i64 = 1;

/// 把库迁移到 [`SCHEMA_VERSION`]。幂等。
pub fn migrate(conn: &Connection) -> Result<(), AppError> {
    migrate_with_ddl(conn, SCHEMA_V1_SQL)
}

/// 读当前库版本。
pub fn current_version(conn: &Connection) -> Result<i64, AppError> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(map_sqlite)
}

/// 用给定的 DDL 迁移。**故障注入用**：计划要求验证「故障中断后无半套表」，
/// 而这条必须在真实的磁盘库上测（集成测试够不到 `pub(crate)`）。
///
/// 正常路径请用 [`migrate`]，它固定使用已发布的 `SCHEMA_V1_SQL`。
pub fn migrate_with_ddl(conn: &Connection, ddl: &str) -> Result<(), AppError> {
    let from = current_version(conn)?;

    if from > SCHEMA_VERSION {
        return Err(AppError::Storage {
            detail: format!(
                "database schema v{from} is newer than this build (v{SCHEMA_VERSION}); refusing to run"
            ),
        });
    }
    if from == SCHEMA_VERSION {
        return Ok(());
    }

    // SQLite 的 DDL 是事务性的，`PRAGMA user_version` 也参与事务，
    // 所以这里不需要「先建表再补版本」的两段式，也不会有中间态。
    let tx = conn.unchecked_transaction().map_err(map_sqlite)?;
    tx.execute_batch(ddl).map_err(map_sqlite)?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(map_sqlite)?;
    tx.commit().map_err(map_sqlite)?;

    Ok(())
}

/// 库里现有的用户表名（含 `sqlite_` 前缀的除外）。诊断与测试用。
pub fn user_tables(conn: &Connection) -> Result<Vec<String>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type='table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .map_err(map_sqlite)?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::Db;

    #[test]
    fn fresh_db_lands_on_schema_version_with_twelve_tables() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(current_version(db.connection()).unwrap(), 0, "新库应为 0");
        migrate(db.connection()).unwrap();

        assert_eq!(current_version(db.connection()).unwrap(), SCHEMA_VERSION);
        let tables = user_tables(db.connection()).unwrap();
        assert_eq!(tables.len(), 12, "实际表：{tables:?}");
    }

    #[test]
    fn migrating_twice_is_a_no_op() {
        let db = Db::open_in_memory().unwrap();
        migrate(db.connection()).unwrap();
        let first = user_tables(db.connection()).unwrap();

        migrate(db.connection()).expect("第二次迁移应当成功且无副作用");

        assert_eq!(user_tables(db.connection()).unwrap(), first);
        assert_eq!(current_version(db.connection()).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn a_newer_database_is_refused() {
        let db = Db::open_in_memory().unwrap();
        db.connection()
            .pragma_update(None, "user_version", SCHEMA_VERSION + 5)
            .unwrap();

        let err = migrate(db.connection()).unwrap_err();
        assert_eq!(err.code(), "STORAGE_ERROR");
        assert!(
            !err.message().contains("user_version"),
            "面向用户的文案不得带内部细节：{}",
            err.message()
        );
        // 不得因为拒绝而降级或改版本。
        assert_eq!(
            current_version(db.connection()).unwrap(),
            SCHEMA_VERSION + 5
        );
    }

    /// 计划原文：「故障中断后无半套表」。
    #[test]
    fn a_failing_migration_leaves_no_half_schema() {
        let db = Db::open_in_memory().unwrap();

        // 前半段合法、后半段语法错误：模拟迁移途中失败。
        let broken = "CREATE TABLE a(id TEXT PRIMARY KEY NOT NULL);\n\
                      CREATE TABLE b(id TEXT PRIMARY KEY NOT NULL);\n\
                      CREATE TABLE c(THIS IS NOT SQL);\n";
        let err = migrate_with_ddl(db.connection(), broken).unwrap_err();
        assert_eq!(err.code(), "STORAGE_ERROR");

        assert!(
            user_tables(db.connection()).unwrap().is_empty(),
            "失败迁移不得留下半套表：{:?}",
            user_tables(db.connection()).unwrap()
        );
        assert_eq!(
            current_version(db.connection()).unwrap(),
            0,
            "版本也不得前进"
        );
    }
}
