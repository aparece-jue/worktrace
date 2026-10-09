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

/// 换一个全新的 `data_epoch`，返回新值。**恢复/替换库的提交路径专用**。
///
/// 分工写死（三个函数各管一段，别互相代劳）：
/// - [`init_meta`] 只在**建库**时 INSERT 一次（重复 INSERT 会撞 `app_meta.singleton` 的 PK）；
/// - [`bump_revision`] 只动 `revision`；
/// - 本函数只动 `data_epoch`，**不动 `revision`**。
///
/// **不 bump revision**：新 epoch 内不存在「旧响应」，所以没有要作废的缓存；bump 只会给
/// 「一次业务写恰好 +1」这条口径多造一条没有业务含义的例外（恢复后的 `revision` 可以低于
/// 原库，它只在**新 epoch 内**比较，00 §5）。
///
/// **调用约定**：只由恢复/替换库的**提交路径**在与 `run_repo::start_run` **同一个事务**里
/// 调一次；回滚路径不调（原库的 epoch 原样保留）。本函数不自行 begin/commit——事务归调用方。
///
/// **影响行数必须是 1**（P6 Task 4b 收掉 4a 评审的 Minor 3）：返回值会被恢复流程当成
/// **新库身份**去广播（客户端据此重新握手），所以它必须真的落了库。库里没有 `app_meta`
/// 行时 `UPDATE` 会影响 0 行——那种库不是一份可用的 worktrace 库（`init_meta` 只在建库时
/// INSERT），照旧返回一个"新 epoch"会让恢复带着一个**没落库的身份**继续往下走。
/// 这与 [`bump_revision`] 的惯例（不校验影响行数）**刻意不同**：那个只在已初始化的库里调，
/// 而本函数的候选库来自一份外部文件，正是最需要判一句"它到底是不是我们的库"的地方。
pub fn rotate_epoch(tx: &Transaction<'_>) -> Result<String, AppError> {
    let epoch = uuid::Uuid::new_v4().to_string();
    let affected = tx
        .execute(
            "UPDATE app_meta SET data_epoch = ?1 WHERE singleton = 1",
            [&epoch],
        )
        .map_err(map_sqlite)?;
    if affected != 1 {
        return Err(AppError::Storage {
            detail: format!("rotate_epoch: expected 1 app_meta row, updated {affected}"),
        });
    }
    Ok(epoch)
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
