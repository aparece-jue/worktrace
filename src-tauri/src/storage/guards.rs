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

/// 便捷组合：读一行的 `row_version` 并校验它存在与匹配。
///
/// 不存在时返回领域错误（未知记录），存在但版本不符返回 `VERSION_CONFLICT`——
/// **两者必须可区分**，前端对它们的处理完全不同。
pub fn guard_row_version_of(
    tx: &Transaction<'_>,
    table: &'static str,
    id: &str,
    expected: i64,
) -> Result<(), AppError> {
    // 表名来自代码常量，不是用户输入；id 走参数绑定。
    let sql = format!("SELECT row_version FROM {table} WHERE id = ?1");
    let actual: Option<i64> = tx
        .query_row(&sql, [id], |r| r.get(0))
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(map_sqlite)?;

    match actual {
        None => Err(AppError::Domain {
            detail: format!("no such {table}"),
        }),
        Some(v) => guard_row_version(v, expected),
    }
}
