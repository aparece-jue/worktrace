//! 请求校验守卫（总纲 §9）。
//!
//! 两条必须在**调用方的写事务内**执行，不能先读后比：
//! - `guard_epoch`：请求方看到的库身份 vs 当前库；
//! - `guard_row_version`：请求方的对象版本 vs 库里现值。
//!
//! **禁止**用「读到当前 epoch 再拿它跟自己比」代替请求校验——那样永远通过，
//! 等于没有校验。所以 `guard_epoch` 只接受**请求里带来的**期望值。

use rusqlite::Transaction;

use crate::error::AppError;

use super::db::map_sqlite;

/// 校验请求的 `data_epoch` 与库一致。
///
/// 不一致返回 `DATA_EPOCH_MISMATCH`，且**不写入任何东西**（F-019）。
/// 错误文本不带 epoch 字面量——库身份不进用户可见文案（00 §4 的脱敏要求）。
pub fn guard_epoch(tx: &Transaction<'_>, expected: &str) -> Result<(), AppError> {
    let actual: String = tx
        .query_row(
            "SELECT data_epoch FROM app_meta WHERE singleton = 1",
            [],
            |r| r.get(0),
        )
        .map_err(map_sqlite)?;
    if actual != expected {
        return Err(AppError::DataEpochMismatch);
    }
    Ok(())
}

/// 校验对象版本。
///
/// `expected` 来自请求（`WriteEnvelope::expected_row_version`），
/// `actual` 来自同事务内刚读到的行。
pub fn guard_row_version(actual: i64, expected: i64) -> Result<(), AppError> {
    if actual != expected {
        return Err(AppError::VersionConflict { expected, actual });
    }
    Ok(())
}
