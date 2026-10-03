//! 写事务信封（00 §5）。
//!
//! 每个业务命令都带 `expected_data_epoch`；修改既有对象时另带
//! `expected_row_version`。关系增删等幂等操作不伪造实体版本。

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
