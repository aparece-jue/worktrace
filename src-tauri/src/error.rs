//! Shared error contract (00 §4).
//!
//! Lives at the crate root because both `storage` and `services` need to produce
//! it, and `storage` must never depend on `commands` (layering rule, 总纲 §9).
//!
//! `code` is the only thing the frontend branches on. `message` is user-facing
//! and must stay free of database identity, SQL text and business payloads.

/// The contract error. Every expected failure in the app funnels through here.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The database identity changed (restore/replace). The caller must re-handshake.
    ///
    /// Deliberately carries no epoch strings: `data_epoch` is database identity,
    /// and 00 §4 requires the detail to be redacted. The frontend only needs the
    /// code to branch; diagnosis belongs in the diagnostic log, not the error text.
    #[error("the database identity changed; a fresh snapshot is required")]
    DataEpochMismatch,

    /// Optimistic-concurrency clash on an existing record.
    #[error("the record was modified by someone else; reload and retry")]
    VersionConflict { expected: i64, actual: i64 },

    /// The session/interval facts need user confirmation before this command may run.
    #[error("a recovery decision is required before this command can run")]
    RecoveryRequired,

    /// A domain rule was violated. `detail` is a redacted rule description.
    #[error("operation not allowed: {detail}")]
    Domain { detail: String },

    /// Infrastructure failure (SQLite, IO, migration). Detail is redacted too.
    #[error("storage failure")]
    Storage { detail: String },
}

impl AppError {
    /// The stable contract code. This is what the frontend branches on.
    pub fn code(&self) -> &'static str {
        match self {
            AppError::DataEpochMismatch => "DATA_EPOCH_MISMATCH",
            AppError::VersionConflict { .. } => "VERSION_CONFLICT",
            AppError::RecoveryRequired => "RECOVERY_REQUIRED",
            AppError::Domain { .. } => "DOMAIN_ERROR",
            AppError::Storage { .. } => "STORAGE_ERROR",
        }
    }

    /// User-facing text. Never contains epoch, paths, SQL or business payload.
    pub fn message(&self) -> String {
        match self {
            AppError::DataEpochMismatch => "数据已被恢复或替换，请刷新后重试。".to_string(),
            AppError::VersionConflict { .. } => "这条记录已被修改，请刷新后重试。".to_string(),
            AppError::RecoveryRequired => "存在待确认的计时记录，请先处理恢复再继续。".to_string(),
            AppError::Domain { detail } => format!("操作不被允许：{detail}"),
            AppError::Storage { .. } => "存储暂时不可用，请稍后重试。".to_string(),
        }
    }

    /// Redacted internal detail for the diagnostic log. Not shown to users.
    pub fn detail(&self) -> Option<&str> {
        match self {
            AppError::Domain { detail } | AppError::Storage { detail } => Some(detail),
            _ => None,
        }
    }
}

impl From<crate::domain::error::DomainError> for AppError {
    /// 领域错误 → 契约错误的映射。
    ///
    /// 规格只给了四个码（`DATA_EPOCH_MISMATCH`、`VERSION_CONFLICT`、
    /// `RECOVERY_REQUIRED`，以及 V0.2 的 `POMO_STATE_CONFLICT`），所以
    /// 「不把所有错误压成一个桶」的准确含义是：
    /// - **待恢复**独立成码——它要触发前端换流程，不是普通报错；
    /// - **版本冲突**独立成码——它来自守卫，前端提示「刷新后重试」；
    /// - 非法状态与占用冲突同属 `DOMAIN_ERROR`，但 `detail` 必须各自可辨，
    ///   否则诊断日志里分不出是「跃迁不合法」还是「前台已被占用」。
    ///
    /// 未知记录同样走 `Domain`，不另设码：规格没有 `NOT_FOUND`，
    /// 而前端对它的处理与其它领域拒绝一致（提示 + 刷新）。
    fn from(e: crate::domain::error::DomainError) -> Self {
        use crate::domain::error::DomainError as D;
        match e {
            // 时钟样本不可信、或运行上下文已过期 → 该会话必须进入 recovering，
            // 等用户确认。这两种都不是「参数写错了」，而是**内存状态与持久化事实
            // 已经对不上**，继续按普通错误提示会让用户重试一个永远失败的操作。
            D::UntrustedSample { .. } | D::StaleRunContext { .. } => AppError::RecoveryRequired,
            other => AppError::Domain {
                detail: other.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_and_distinct() {
        let all = [
            AppError::DataEpochMismatch,
            AppError::VersionConflict {
                expected: 1,
                actual: 2,
            },
            AppError::RecoveryRequired,
            AppError::Domain { detail: "x".into() },
            AppError::Storage { detail: "x".into() },
        ];
        let codes: Vec<&str> = all.iter().map(|e| e.code()).collect();
        assert_eq!(
            codes,
            vec![
                "DATA_EPOCH_MISMATCH",
                "VERSION_CONFLICT",
                "RECOVERY_REQUIRED",
                "DOMAIN_ERROR",
                "STORAGE_ERROR"
            ]
        );
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "code 必须互不相同");
    }

    #[test]
    fn no_error_leaks_database_identity_or_sql() {
        // The leak we are guarding against: an epoch string or a SQL fragment
        // ending up in Debug output or the user-facing message.
        let e = AppError::Storage {
            detail: "SELECT * FROM secret_table".into(),
        };
        assert!(
            !e.message().contains("SELECT"),
            "message 不得含 SQL：{}",
            e.message()
        );
        let e2 = AppError::DataEpochMismatch;
        let rendered = format!("{e2:?} {e2}");
        assert!(
            !rendered.contains("epoch"),
            "错误文本不得带库身份：{rendered}"
        );
        assert_eq!(e2.detail(), None, "库身份类错误不提供 detail");
    }

    #[test]
    fn messages_are_non_empty_and_chinese() {
        let all = [
            AppError::DataEpochMismatch,
            AppError::VersionConflict {
                expected: 1,
                actual: 2,
            },
            AppError::RecoveryRequired,
            AppError::Domain {
                detail: "非法状态".into(),
            },
            AppError::Storage {
                detail: "disk".into(),
            },
        ];
        for e in all {
            let m = e.message();
            assert!(!m.is_empty(), "{:?} 的 message 为空", e.code());
            assert!(
                m.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "应为中文提示：{m}"
            );
        }
    }
}
