//! 业务服务层：拥有事务、编排仓储、返回契约信封。
//!
//! **仓储只提供事务原语**（接受 `&Transaction`，不自行提交、不自行加 revision）；
//! 「一次业务操作恰好加一次 revision」的责任在这里（总纲 §9）。

pub mod timer;

pub mod error_response;

// P4：项目/标签/今日计划的输入校验入口（Task 1 建，T2/T3/T4 复用）。
pub mod catalog;
pub mod daily_plan;
