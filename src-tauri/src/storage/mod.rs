//! 存储层：SQLite、迁移与仓储。
//!
//! 不得调用 `platform::`，也不得反向引用 `commands::`。
//! 写入一律接受调用方的 `&Transaction`，由服务层拥有事务并决定何时加 `revision`。
//!
//! # `task_change` 的三种 JSON 形状（P4 Task 6 集中登记，取数据前必读）
//!
//! `task_change` 是 V0.1 **唯一**的任务级审计表，写入口只有一个
//! （[`task_repo::record_change`]），但 P4 期间它被扩成了**三种形状**。
//! 三种都进同一张表，所以「这个任务有没有 `task_change` 行」**回答不了任何业务问题**：
//! 取数据前必须按 `before_json` / `after_json` 的**形状**过滤。
//!
//! 1. **任务字段形状**（[`task_repo`]）：键就是被改掉的列，值是该列的新旧值。
//!    - `create_task`：`before = {}`，`after = {"status":"Inbox","title":"…"}`
//!    - `transition_task`：`{"status":"Doing","quality":null}`
//!    - `set_task_project`：`{"project_id":"<id>"}` / `{"project_id":null}`
//!    - `freeze_baseline_estimate`：`{"baseline_estimate_json":"…"}`
//! 2. **标签集合形状**（[`tag_repo::tag_task`] / [`tag_repo::untag_task`]）：
//!    `{"tags":["<tag id>", …]}`——键固定是 `tags`，值是**变化前后的完整集合**。
//! 3. **今日计划集合形状**（[`daily_plan_repo::add_to_plan`] /
//!    [`daily_plan_repo::remove_from_plan`]）：
//!    `{"daily_plan":[{"local_date":"2026-10-03","timezone":"Asia/Shanghai"}, …]}`。
//!
//! ⚠️ **P5 统计「完成项」时必须按 JSON 形状过滤**，例如
//! `json_extract(after_json, '$.status') = 'Done'`，**不能只看有没有 `task_change`**：
//! 打一个标签、加一次今日计划同样会写一条 `task_change`，按「有审计行」筛出来的
//! 「完成项」会把它们全部算进去。

pub mod checkpoint_repo;
pub mod daily_plan_repo;
pub mod db;
pub mod guards;
pub mod meta;
pub mod migrations;
pub mod project_repo;
pub mod run_repo;
pub mod schema_v1;
pub mod session_repo;
pub mod tag_repo;
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
    /// 真的写入了：新增了一行，或既有行的 `row_version` 已前进。调用方负责加一次 `revision`。
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
}
