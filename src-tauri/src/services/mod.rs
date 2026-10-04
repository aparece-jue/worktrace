//! 业务服务层：拥有事务、编排仓储、返回契约信封。
//!
//! **仓储只提供事务原语**（接受 `&Transaction`，不自行提交、不自行加 revision）；
//! 「一次业务操作恰好加一次 revision」的责任在这里（总纲 §9）。

pub mod timer;

// P7 Task 0：唯一启动入口与串行执行边界；事件信封与去重协议。
pub mod bootstrap;
pub mod events;

pub mod error_response;
pub mod handshake;

// P4：项目/标签/今日计划的输入校验入口（Task 1 建，T2/T3/T4 复用）。
pub mod catalog;
pub mod daily_plan;

// P3 Task 1：启动恢复扫描（四类判定 + 崩溃区间归一）与共享恢复原语。
pub mod recovery;

// P3 Task 3：已完成历史的时间修正（`correct`：重定时 / 软删除）。
pub mod history;

// P3 Task 6：任务状态编排与会话联动（`transition_task`）。S11 的最后一块——
// 到这里 `services` 子树的注册就齐了（前两个由 Task 1/3 各自注册，见 Ruling 2）。
pub mod tasks;

// 写事务骨架（write_tx / settle）只有本层用，所以不对 crate 外公开（R-T6-l）。
mod tx;
