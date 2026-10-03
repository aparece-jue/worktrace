//! 写事务的公共骨架（P4 Task 6 收口，裁决 R-T6-l）。
//!
//! [`write_tx`] 与 [`settle`] 原先在 `services::catalog` 与 `services::daily_plan`
//! 里**各有一份、逐字相同**——那是 Task 4 为不碰 Task 5 的收口文件而付的代价。
//! 两个服务模块现在都引用这里这一份，**行为一字未改**：守卫顺序、`Unchanged`
//! 读回当前 revision、错误文案都保持原样。
//!
//! 可见性 `pub(super)`：只有 `services` 自己的子模块（`catalog` / `daily_plan`）用
//! 它，命令层与仓储层都不该直接拿——事务的所有权属于服务层，仓储只接受 `&Transaction`。

use rusqlite::Transaction;

use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::guards::guard_epoch;
use crate::storage::meta::{bump_revision, require_meta};
use crate::storage::WriteOutcome;

/// 开一个写事务并把库身份守卫做掉。
///
/// 事务的所有权从这一行起到 `commit` 为止都归服务：仓储不 `begin`、不 `commit`。
/// `guard_epoch` **必须在写事务内**执行（总纲 §9），且只接受请求带来的期望值——
/// 「读出当前 epoch 再跟自己比」等于没有校验。
pub(super) fn write_tx<'a>(
    db: &'a mut Db,
    env: &WriteEnvelope,
) -> Result<Transaction<'a>, AppError> {
    let tx = db
        .connection_mut()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    guard_epoch(&tx, &env.expected_data_epoch)?;
    Ok(tx)
}

/// 收口一次写原语：`Changed` 才加一次 `revision`；`Unchanged` 读回当前值（R-T2-e）。
pub(super) fn settle<T>(
    tx: &Transaction<'_>,
    outcome: WriteOutcome<T>,
) -> Result<WriteOutcome<(T, i64)>, AppError> {
    match outcome {
        WriteOutcome::Changed(value) => Ok(WriteOutcome::Changed((value, bump_revision(tx)?))),
        WriteOutcome::Unchanged(value) => {
            Ok(WriteOutcome::Unchanged((value, require_meta(tx)?.revision)))
        }
    }
}
