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
        // 用途不同的两类 detail，守的东西也不同：
        //
        // - `Domain` 的 detail **按契约就是面向用户的**——`message()` 是
        //   `操作不被允许：{detail}`，它会被原样拼进用户看到的句子。所以对它的要求
        //   不是「别进 message」（它必然进），而是「本身得是用户读得懂的中文」，
        //   那条由 `domain_error_messages_are_user_facing_chinese` 守。
        // - `Storage` 的 detail 是**内部诊断**，任何时候都不该出现在用户文案里。
        //
        // 原先这里是 `assert_ne!(message(), detail)`：因为 message 带了前缀，
        // 它对 Domain 恒真、什么都没守住；对 Storage 也拦不住「message 里嵌了 detail」
        // 这种真正的泄漏。改成按变体分别守。
        if let AppError::Storage { detail } = e {
            assert!(
                !e.message().contains(detail.as_str()),
                "内部诊断不得进入用户文案：{} ⊃ {detail}",
                e.message()
            );
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

/// **`DomainError` 的文案是给用户看的**，不是日志。
///
/// 因为 `AppError::Domain { detail }` 的 `message()` 会把 detail 原样拼进去。
/// 这一条曾经是坏的：全部 14 个变体都是英文技术串，用户会看到
/// 「操作不被允许：an interval is already open」。
#[test]
fn domain_error_messages_are_user_facing_chinese() {
    use worktrace_lib::domain::error::DomainError;

    let cases: Vec<DomainError> = vec![
        DomainError::IllegalTransition {
            from: "Inbox",
            to: "Doing",
        },
        DomainError::ReopenMustBeExplicit { from: "Done" },
        DomainError::NotInThisVersion { what: "pomodoro" },
        DomainError::IntervalAlreadyOpen,
        DomainError::NoOpenInterval,
        DomainError::IntervalOpenInWrongState {
            state: "recovering",
        },
        DomainError::NegativeInterval {
            started_at: 10,
            ended_at: 5,
        },
        DomainError::OverlappingInterval {
            existing_start: 1,
            existing_end: 9,
        },
        DomainError::TrustedIntervalWithoutDuration,
        DomainError::PendingAndVoided,
        DomainError::EmptyText { field: "task" },
        DomainError::UntrustedSample {
            reason: "monotonic went backwards",
        },
        DomainError::UnknownEnumValue {
            field: "state",
            value: "???".into(),
        },
        DomainError::StaleRunContext {
            expected: "run-1".into(),
            actual: "run-2".into(),
        },
        // P4 Task 2（项目服务与任务归属）新增的五个变体。
        DomainError::UnknownTask,
        DomainError::UnknownProject,
        DomainError::ProjectArchived,
        DomainError::TaskNotInClarifying { status: "Doing" },
        DomainError::TaskHasRunningSession,
        // P4 Task 3（标签与幂等关联）新增的两个变体。
        DomainError::UnknownTag,
        DomainError::TagNameTaken {
            kind: "Domain",
            name: "写作".into(),
        },
        // P4 Task 5（任务筛选、捕获与理清为待办）新增的两个变体。
        DomainError::ContextTagRequired { kind: "Domain" },
        DomainError::TaskNotClarifiable { status: "Doing" },
    ];
    assert_eq!(cases.len(), 23, "23 个变体都要覆盖，加了新的记得补进来");

    for e in cases {
        let shown: AppError = e.into();
        let message = shown.message();

        // ① 是中文：至少含一个 CJK 字符
        assert!(
            message
                .chars()
                .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "用户文案必须是中文：{message}"
        );
        // ② 不说内部标识：裸枚举名不该出现。
        //
        // 覆盖两类：任务状态名（`zh_status` 负责翻译）与四类标签的 kind
        // （`zh_kind` 负责翻译，术语见 99-glossary §5）。两个映射表都**漏一个就红**——
        // `TagNameTaken` 曾经直接把 `TagKind::as_str()` 印进文案，正是靠这里拦住。
        for banned in [
            "Inbox",
            "Clarifying",
            "Ready",
            "Doing",
            "Waiting",
            "Blocked",
            "Done",
            "Domain",
            "Activity",
            "Context",
            "Report",
        ] {
            assert!(
                !message.contains(banned),
                "不得漏出内部标识 {banned:?}：{message}"
            );
        }
        // ③ 不承诺可以重试——这些拒绝与输入无关，重试一次还是被拒
        assert!(
            !message.contains("重试"),
            "拒绝类文案不该说「重试」：{message}"
        );
    }
}

/// `TagNameTaken` 的两件事必须分开：字段留**代码里的取值**（诊断与 T6 的结构化载荷要用），
/// 只有面向用户的 `Display` 走 `zh_kind` 翻成中文（术语见 99-glossary §5）。
///
/// 这条用例守的是「别用把字段改成中文的办法去修泄漏」——那样结构化载荷就废了。
#[test]
fn tag_name_taken_keeps_the_code_value_and_renders_chinese() {
    use worktrace_lib::domain::error::DomainError;

    let e = DomainError::TagNameTaken {
        kind: "Context",
        name: "家里".into(),
    };
    match &e {
        DomainError::TagNameTaken { kind, name } => {
            assert_eq!(*kind, "Context", "字段保留原值，供诊断与结构化载荷");
            assert_eq!(name, "家里");
        }
        _ => unreachable!(),
    }
    assert_eq!(
        e.to_string(),
        "「上下文」这一类里已经有叫「家里」的标签了。"
    );
}
