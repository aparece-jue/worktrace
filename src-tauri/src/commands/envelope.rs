//! 写事务信封的**转发路径**。
//!
//! `WriteEnvelope` 住在 crate 根（[`crate::envelope`]）：`storage` 与 `services` 都要用它，
//! 而它们不得依赖 `commands`（总纲 §5 第 1 条的方向是 `commands → services`）。
//! 这条路径保留下来，供命令/ IPC 层照旧按 `commands::envelope::WriteEnvelope` 引用。

pub use crate::envelope::WriteEnvelope;
