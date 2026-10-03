//! 写事务信封（00 §5）。
//!
//! 每个业务命令都带 `expected_data_epoch`；修改既有对象时另带
//! `expected_row_version`。关系增删等幂等操作不伪造实体版本。
//!
//! # 谁用它（P2 的结论，别再重新决定一次）
//!
//! **单对象命令用它**：一次请求只动一个可编辑对象时，`expected_row_version`
//! 放在信封里正合适。P4 的 `create_task` / `transition_task` 等就是这种形状。
//!
//! **多对象命令不要用它**：P2 的计时命令要同时校验**任务与会话各自**的版本
//! （`resume` 一份都不够），一个 `Option<i64>` 装不下两个。那里的做法是**让请求
//! 自带字段**——`StartRequest`/`ResumeRequest` 各有自己的 `*_expected_version`，
//! 比硬塞进信封清楚得多。
//!
//! 所以本类型在 P2 交付时**没有消费者**，这不是遗漏：它服务的是 P4 那一类命令。
//! 若到 P4 结束时仍然没有调用方，就该删掉——留着一个没人用的信封只会让下一个人
//! 以为「应该用它」。

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
