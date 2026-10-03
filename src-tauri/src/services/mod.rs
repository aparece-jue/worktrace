//! 业务服务层：拥有事务、编排仓储、返回契约信封。
//!
//! **仓储只提供事务原语**（接受 `&Transaction`，不自行提交、不自行加 revision）；
//! 「一次业务操作恰好加一次 revision」的责任在这里（总纲 §9）。

pub mod timer;

pub mod error_response;
