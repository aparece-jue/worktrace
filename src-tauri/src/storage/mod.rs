//! 存储层：SQLite、迁移与仓储。
//!
//! 不得调用 `platform::`，也不得反向引用 `commands::`。
//! 写入一律接受调用方的 `&Transaction`，由服务层拥有事务并决定何时加 `revision`。

pub mod checkpoint_repo;
pub mod db;
pub mod guards;
pub mod meta;
pub mod migrations;
pub mod project_repo;
pub mod schema_v1;
pub mod session_repo;
pub mod task_repo;
pub mod time_edit_repo;

/// 写原语的结果：区分「真的改了库」与「请求与现状一致，什么都没写」。
///
/// 为什么不是一个 `bool`：幂等重复（重命名同名、归档已归档、设成同一归属）**不写审计、
/// 不加 `row_version`、不加 `revision`**（总纲 §5 第 8 条、裁决 R-T2-e）。用变体表达
/// 之后，调用方必须先解构才能拿到值，「把幂等重复当成一次业务写去加 revision」
/// 这件事就没法顺手写出来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome<T> {
    /// 真的写入了：实体的 `row_version` 已经前进，调用方负责加一次 `revision`。
    Changed(T),
    /// 请求与现状一致：**没有写任何东西**，调用方不得加 `revision`。
    Unchanged(T),
}

impl<T> WriteOutcome<T> {
    /// 变换载荷，保留「变了 / 没变」这一位。
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> WriteOutcome<U> {
        match self {
            Self::Changed(value) => WriteOutcome::Changed(f(value)),
            Self::Unchanged(value) => WriteOutcome::Unchanged(f(value)),
        }
    }

    /// 取出载荷（两种变体都有）。
    pub fn into_value(self) -> T {
        match self {
            Self::Changed(value) | Self::Unchanged(value) => value,
        }
    }
}
