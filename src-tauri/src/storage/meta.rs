//! `app_meta`：库身份（`data_epoch`）与业务 `revision`（00 §5）。
//!
//! 单行表。两条不变量写在这里，因为它们是全项目的写入口径：
//! - `data_epoch` 只在**建库**与**恢复/替换库**时改变，业务写永不改它；
//! - 一次成功的**业务**写恰好 `revision + 1`；心跳、tick、纯读不加（00 §5）。

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::error::AppError;

use super::db::map_sqlite;

/// 库身份与版本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub data_epoch: String,
    pub revision: i64,
}

/// 读当前元数据。库未初始化时返回 `None`。
pub fn read_meta(conn: &Connection) -> Result<Option<Meta>, AppError> {
    conn.query_row(
        "SELECT data_epoch, revision FROM app_meta WHERE singleton = 1",
        [],
        |r| {
            Ok(Meta {
                data_epoch: r.get(0)?,
                revision: r.get(1)?,
            })
        },
    )
    .optional()
    .map_err(map_sqlite)
}

/// 读元数据，缺失即报错。已初始化的库用这个。
pub fn require_meta(conn: &Connection) -> Result<Meta, AppError> {
    read_meta(conn)?.ok_or_else(|| AppError::Storage {
        detail: "app_meta is not initialised".into(),
    })
}

/// 初始化元数据。**只建库时调用一次**。
///
/// `data_epoch` 用 UUID：它是库身份，恢复/替换库时必须换一个全新的值，
/// 让所有持有旧 epoch 的请求被拒（F-019）。
pub fn init_meta(tx: &Transaction<'_>) -> Result<Meta, AppError> {
    let epoch = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO app_meta(singleton, data_epoch, revision) VALUES(1, ?1, 0)",
        [&epoch],
    )
    .map_err(map_sqlite)?;
    Ok(Meta {
        data_epoch: epoch,
        revision: 0,
    })
}

/// 一次**业务**写成功后调用，`revision` 恰好 +1，返回新值。
///
/// 调用点必须落在业务服务的事务里，且**一次业务操作只调一次**（总纲 §9）。
/// 仓储不得调用它。心跳与 tick 不得调用它。
pub fn bump_revision(tx: &Transaction<'_>) -> Result<i64, AppError> {
    tx.execute(
        "UPDATE app_meta SET revision = revision + 1 WHERE singleton = 1",
        [],
    )
    .map_err(map_sqlite)?;
    let rev: i64 = tx
        .query_row(
            "SELECT revision FROM app_meta WHERE singleton = 1",
            [],
            |r| r.get(0),
        )
        .map_err(map_sqlite)?;
    Ok(rev)
}
