//! 写事务的公共骨架（P4 Task 6 收口，裁决 R-T6-l）。
//!
//! [`write_tx`] 与 [`settle`] 原先在 `services::catalog` 与 `services::daily_plan`
//! 里**各有一份、逐字相同**——那是 Task 4 为不碰 Task 5 的收口文件而付的代价。
//! 两个服务模块现在都引用这里这一份：守卫顺序、`Unchanged` 读回当前 revision、
//! 错误文案都保持原样；COMP-01 只把 `settle` 的返回值从裸 `i64` 换成 [`Settled`]
//! （版本 + 库身份，仍在同一个写事务里读回）。
//!
//! 可见性 `pub(super)`：只有 `services` 自己的子模块用它，命令层与仓储层都不该直接拿——
//! 事务的所有权属于服务层，仓储只接受 `&Transaction`。
//!
//! **使用者清单（P3 Task 6 收口，最终版）**：`catalog` / `daily_plan` / `recovery` /
//! `history` / `tasks`。P3 的三个服务按 Ruling 2/4 随各自任务登记（Task 1 只注册
//! `recovery`、Task 3 注册 `history`、Task 6 注册 `tasks` 并收口成这一份）。

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

/// 收口一次写原语：`Changed` 才加一次 `revision`；两个分支都在**同一个写事务**里
/// 读回权威 `revision` 与库身份 `data_epoch`（R-T2-e，COMP-01 裁决 R-A）。
pub(super) fn settle<T>(
    tx: &Transaction<'_>,
    outcome: WriteOutcome<T>,
) -> Result<WriteOutcome<(T, Settled)>, AppError> {
    match outcome {
        WriteOutcome::Changed(value) => {
            // 一次成功的业务写恰好加一次版本，随后在同一事务里读回权威值。
            bump_revision(tx)?;
            Ok(WriteOutcome::Changed((value, Settled::read(tx)?)))
        }
        WriteOutcome::Unchanged(value) => Ok(WriteOutcome::Unchanged((value, Settled::read(tx)?))),
    }
}

/// 一次写的结果版本：业务 `revision` + **库身份** `data_epoch`（裁决 R-A）。
///
/// 两者都出自那次写所在的事务。`data_epoch` 在一次业务写里不会变，但同样必须
/// **同事务读**：提交后补读会让「这次写发生在哪个库」由另一个时刻的快照回答，
/// 而恢复/替换库恰好会换掉它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Settled {
    /// 提交后的权威 `revision`；`Unchanged` 时与调用前相等。
    pub revision: i64,
    /// 这次写看到的库身份（业务写永不改它）。
    pub data_epoch: String,
}

impl Settled {
    /// 从**调用方事务**里读回版本与库身份（绝不提交后补读）。
    fn read(tx: &Transaction<'_>) -> Result<Self, AppError> {
        let meta = require_meta(tx)?;
        Ok(Self {
            revision: meta.revision,
            data_epoch: meta.data_epoch,
        })
    }
}
