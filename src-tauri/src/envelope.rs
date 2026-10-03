//! 写事务信封（00 §5）。
//!
//! 每个业务命令都带 `expected_data_epoch`；修改既有对象时另带
//! `expected_row_version`。关系增删等幂等操作不伪造实体版本。
//!
//! # 为什么住在 crate 根
//!
//! 理由与 [`crate::error::AppError`] 完全一样：**`storage` 与 `services` 都要用它，
//! 而它们不得依赖 `commands`**（总纲 §5 第 1 条的分层方向是 `commands → services`）。
//! 这个类型原先放在 `commands::envelope`：P4 Task 2 的 `services::catalog` 一引用它，
//! 就产生了仓库里第一条 **services → commands** 的反向边——门禁当时全绿（没有人查这条），
//! 规则却已经不成立。所以类型搬到 crate 根。
//!
//! P4 Task 6 收口时把 `commands::envelope` 那条转发路径**删掉**了：它的唯一消费者是
//! 一条转发用例，生产代码零调用（本仓库不留「定义了没人调」的 API）。**IPC 层（P7）
//! 从 `crate::envelope` 引用它**，路径就是本模块——多写一行 `use crate::envelope::WriteEnvelope;`
//! 即可，不需要一层转手。同时 `scripts/check-layers.ps1` 会拦住 `services/` 与
//! `storage/` 里的 `commands::`，防止反向边再长回来。
//!
//! # 谁用它（P4 Task 6 收口时按实际调用方核对出的事实清单）
//!
//! [`crate::services::catalog`]（项目、归属、标签、捕获、理清）：
//! - `for_create` —— [`create_project`](crate::services::catalog::create_project)、
//!   [`create_tag`](crate::services::catalog::create_tag)、
//!   [`create_task`](crate::services::catalog::create_task)，以及 epoch-only 的集合操作
//!   [`tag_task`](crate::services::catalog::tag_task)、
//!   [`untag_task`](crate::services::catalog::untag_task)；
//! - `for_update` —— [`rename_project`](crate::services::catalog::rename_project)、
//!   [`archive_project`](crate::services::catalog::archive_project)、
//!   [`set_task_project`](crate::services::catalog::set_task_project)、
//!   [`clarify_ready`](crate::services::catalog::clarify_ready)。
//!
//! [`crate::services::daily_plan`]（今日计划增删）：
//! - epoch-only 的 [`add_to_plan`](crate::services::daily_plan::add_to_plan)、
//!   [`remove_from_plan`](crate::services::daily_plan::remove_from_plan)。
//!
//! **单对象命令用它**：一次请求只动一个可编辑对象时，`expected_row_version`
//! 放在信封里正合适。P4 的 `create_task` / `clarify_ready` 就是这种形状。
//!
//! **多对象命令不要用它**：P2 的计时命令要同时校验**任务与会话各自**的版本
//! （`resume` 一份都不够），一个 `Option<i64>` 装不下两个。那里的做法是**让请求
//! 自带字段**——`StartRequest`/`ResumeRequest` 各有自己的 `*_expected_version`，
//! 比硬塞进信封清楚得多。

/// 一次写操作的请求信封。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteEnvelope {
    /// 请求方看到的库身份。与当前库不一致即拒绝，且不写入。
    pub expected_data_epoch: String,
    /// 更新既有可编辑对象时必填；新建实体为 `None`。
    pub expected_row_version: Option<i64>,
}

impl WriteEnvelope {
    /// 新建实体：只需 epoch。
    pub fn for_create(expected_data_epoch: impl Into<String>) -> Self {
        Self {
            expected_data_epoch: expected_data_epoch.into(),
            expected_row_version: None,
        }
    }

    /// 修改既有对象：epoch + 版本。
    pub fn for_update(expected_data_epoch: impl Into<String>, expected_row_version: i64) -> Self {
        Self {
            expected_data_epoch: expected_data_epoch.into(),
            expected_row_version: Some(expected_row_version),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_envelope_has_no_version() {
        let e = WriteEnvelope::for_create("epoch-a");
        assert_eq!(e.expected_data_epoch, "epoch-a");
        assert_eq!(e.expected_row_version, None);
    }

    #[test]
    fn update_envelope_carries_the_version() {
        let e = WriteEnvelope::for_update("epoch-a", 3);
        assert_eq!(e.expected_row_version, Some(3));
    }
}
