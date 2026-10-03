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
//! 规则却已经不成立。所以类型搬到 crate 根，`commands::envelope` 保留为转发路径。
//! 现在 `scripts/check-layers.ps1` 会拦住 `services/` 与 `storage/` 里的 `commands::`。
//!
//! # 谁用它（事实，非计划）
//!
//! 真实调用方（P4 Task 2 落地）：
//! - [`crate::services::catalog::create_project`] —— `for_create`；
//! - [`crate::services::catalog::rename_project`]、
//!   [`crate::services::catalog::archive_project`]、
//!   [`crate::services::catalog::set_task_project`] —— `for_update`。
//!
//! **单对象命令用它**：一次请求只动一个可编辑对象时，`expected_row_version`
//! 放在信封里正合适。P4 的 `create_task` / `transition_task` 等就是这种形状。
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

    /// `commands::envelope` 那条转发路径仍然指向**同一个类型**。
    ///
    /// 保留这条路径是为了 P7 的 IPC 层（它可能按 `commands::envelope` 写）；这个用例
    /// 同时是它的唯一消费者——没有消费者又不删，就是仓库明令不留的「死 API」。
    #[test]
    fn the_commands_path_is_the_same_type() {
        let from_commands: crate::commands::envelope::WriteEnvelope =
            WriteEnvelope::for_create("epoch-a");
        assert_eq!(from_commands.expected_row_version, None);
    }
}
