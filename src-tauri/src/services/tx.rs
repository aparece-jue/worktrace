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
//! **使用者清单（P3 Task 7 收口）**：`catalog` / `daily_plan` / `recovery` /
//! `history` / `tasks` / `timer::coordinator`（S4 的时钟校正接受——它只要 `settle`，
//! 不建 `write_tx`：那条命令没有实体版本位）。P3 的三个服务按 Ruling 2/4 随各自任务
//! 登记（Task 1 只注册 `recovery`、Task 3 注册 `history`、Task 6 注册 `tasks` 并收口）。

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
///
/// # ⚠️ 既有性质：`DEFERRED` + **先读后写** ⇒ 第二个写者会让升级**立刻** `SQLITE_BUSY`
/// （P6 终审修复波的 out-of-scope 登记；C3 只把它写在这里，**不改行为**）
///
/// `unchecked_transaction()` 开的是 **DEFERRED** 事务：`guard_epoch`（这一行下面那次读）
/// 先把它变成读事务，第一次写才去升级写锁。SQLite 的既有语义是——升级写锁时若已有别的
/// 写事务在跑，**立刻**返回 `SQLITE_BUSY`，**不等** `busy_timeout`（超时只作用于"拿不到
/// 读锁/写锁时的首次获取"，不作用于"读事务升级"）。所以第二条写连接一旦出现：
///
/// - 现象不是"等一会儿再写"，而是**立刻**失败（`AppError::Storage`）；
/// - 重试由**调用方**决定，本函数不做退避也不重试（今天没有调用方需要）；
/// - 今天生产只有**一条**写连接（单实例锁 + 单 `Db`，`Db` 不实现 `Clone`），所以这条
///   路径不可达；`tests/wal_concurrency.rs` 里那条第二连接写者也只是测试装置（它自带
///   退避重试，见 `retry_write`）。
///
/// **引入第二个写者之前必须先改这里**：改成 `BEGIN IMMEDIATE`（写事务起点就持写锁，
/// 让 `busy_timeout` 真正生效）或在调用方加带退避的重试。两件事都不在本文件今天的
/// 范围内——事务模式的变更要单独裁决（P6 明确不做）。
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

/// 一份带「权威版本位」（`revision` + `data_epoch`）的报告。
///
/// 实现只声明**这两个字段在哪**；怎么填由 [`settle_into`] 一处说了算。
pub(super) trait SettledReport {
    fn revision_mut(&mut self) -> &mut i64;
    fn data_epoch_mut(&mut self) -> &mut String;
}

/// 把 [`settle`] 读回的权威版本位填进报告，保留「变了 / 没变」这一位。
///
/// P3 终审 I2b：这对赋值原先在 `recovery`（对账与作废各一处）、`history`、`tasks`、
/// `timer::coordinator` 各写一遍——R10 的契约五份拷贝（外加 `coordinator` 那处
/// `into_parts()` 之后的直接赋值）。收成一处之后，「报告里的 `revision`/`data_epoch`
/// 从哪来」只有一个答案：**那次写所在的事务**。
pub(super) fn settle_into<T: SettledReport>(
    outcome: WriteOutcome<(T, Settled)>,
) -> WriteOutcome<T> {
    outcome.map(|(mut report, settled)| {
        *report.revision_mut() = settled.revision;
        *report.data_epoch_mut() = settled.data_epoch;
        report
    })
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
