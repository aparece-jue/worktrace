//! P1 Task 1：错误契约（00 §4）。
//!
//! 计划要求的四件事：未知记录、非法状态、epoch 冲突、版本冲突都能被区分；
//! 且错误文本不含数据库路径、SQL 与业务正文。

use worktrace_lib::error::AppError;

/// 未知记录与非法状态都必须能与「冲突」区分开，不能压成一个 DOMAIN_ERROR 之外的
/// 兜底码，也不能互相混淆。
#[test]
fn unknown_record_and_illegal_state_are_distinguishable() {
    let unknown = AppError::Domain {
        detail: "no such task".into(),
    };
    let illegal = AppError::Domain {
        detail: "task is already Done".into(),
    };

    // 两者同为 DOMAIN_ERROR（这是契约允许的：前端按 code 分支，按 message 提示），
    // 但 detail 必须各自可辨，诊断日志才用得上。
    assert_eq!(unknown.code(), "DOMAIN_ERROR");
    assert_eq!(illegal.code(), "DOMAIN_ERROR");
    assert_ne!(unknown.detail(), illegal.detail());
}

#[test]
fn epoch_and_version_conflicts_are_not_the_same_code() {
    let epoch = AppError::DataEpochMismatch;
    let version = AppError::VersionConflict {
        expected: 2,
        actual: 5,
    };

    assert_eq!(epoch.code(), "DATA_EPOCH_MISMATCH");
    assert_eq!(version.code(), "VERSION_CONFLICT");
    assert_ne!(epoch.code(), version.code());
}

#[test]
fn recovery_has_its_own_code() {
    assert_eq!(AppError::RecoveryRequired.code(), "RECOVERY_REQUIRED");
}

/// 计划原文：「错误不包含数据库路径、SQL、业务正文」。
#[test]
fn errors_never_leak_paths_sql_or_payload() {
    let cases = [
        AppError::DataEpochMismatch,
        AppError::VersionConflict {
            expected: 1,
            actual: 2,
        },
        AppError::RecoveryRequired,
        AppError::Domain {
            detail: "illegal transition".into(),
        },
        AppError::Storage {
            detail: "database is locked".into(),
        },
    ];

    for e in &cases {
        let shown = format!("{} {} {:?}", e.code(), e.message(), e.message());
        for banned in [
            "SELECT", "INSERT", "UPDATE ", "DELETE", ".db", "C:\\", "/home/", "sqlite",
        ] {
            assert!(
                !shown.contains(banned),
                "面向用户的文本不得出现 {banned:?}：{shown}"
            );
        }
        // 面向用户的 message 不得等于内部 detail —— 两者用途不同。
        if let Some(d) = e.detail() {
            assert_ne!(e.message(), d, "message 不应直接复用内部 detail");
        }
    }
}

/// 版本冲突要能带上期望值与实际值供诊断，但不得进入 message。
#[test]
fn version_conflict_carries_numbers_but_not_in_message() {
    let e = AppError::VersionConflict {
        expected: 7,
        actual: 9,
    };
    match e {
        AppError::VersionConflict { expected, actual } => {
            assert_eq!((expected, actual), (7, 9));
        }
        _ => unreachable!(),
    }
    assert!(!e.message().contains('7'));
    assert!(!e.message().contains('9'));
}
