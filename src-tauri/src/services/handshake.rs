//! 只读的库身份握手；**不要求调用方已知任何 epoch**（00 §5 规则 1/4）。
//!
//! 顺序：窗口先监听并暂存事件 → 调用本入口取库身份 → 携该 epoch 拉业务一致快照。
//! 之后发生恢复/换库时，由**业务查询**的 epoch 守卫返回 `DATA_EPOCH_MISMATCH`，
//! 客户端据此重新握手，而不是拿未知来源的通知当权威身份。
//!
//! 这是全仓**唯一**不要求 `expected_data_epoch` 的查询入口——因为它正是产出
//! epoch 的那一个；其余查询一律只接受请求带来的期望值。
use crate::error::AppError;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::meta::require_meta;

/// 握手与周期校验的返回：只给身份与版本，不含任何业务数据。
///
/// 它**不代替**业务快照：窗口仍需携返回的 epoch 去拉自己需要的一致视图。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RevisionSnapshot {
    pub data_epoch: String,
    pub revision: i64,
}

/// 首次握手、恢复后重新握手与可见窗口的周期版本校验共用这一个入口。
///
/// 纯读：不采样时钟、不初始化元数据、不增加 `revision`、不写任何行、也不改动
/// 调用方的缓存。库未初始化时按存储层失败拒绝（不顺手建元数据）。
pub fn get_revision(db: &Db) -> Result<RevisionSnapshot, AppError> {
    let tx = db
        .connection()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    let meta = require_meta(&tx)?;
    Ok(RevisionSnapshot {
        data_epoch: meta.data_epoch,
        revision: meta.revision,
    })
}
