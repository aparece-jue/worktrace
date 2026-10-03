//! P1 Task 1：错误契约（00 §4）。
//!
//! 计划要求的四件事：未知记录、非法状态、epoch 冲突、版本冲突都能被区分；
//! 且错误文本不含数据库路径、SQL 与业务正文。

use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::domain::tag::TagKind;
use worktrace_lib::domain::task::{TaskStatus, TransitionCause};
use worktrace_lib::error::{
    AppError, AuthorityKind, AuthorityTarget, ErrorAuthority, RecordVersion,
};
use worktrace_lib::services::error_response::capture_error_response;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::{project_repo, session_repo, tag_repo, task_repo};

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

        // ① 用户读到的那句话是中文：至少含一个 CJK 字符。
        //
        // 断言落在 `detail()` 上，**不是** `message()` 上：`Domain` 的 message 模板是
        // `操作不被允许：{detail}`，模板自带中文 ⇒ 在 message 里找 CJK 对
        // `DOMAIN_ERROR` 恒真，什么都没守住（上一轮评审的 out-of-scope 观察，T6 订正）。
        // 真正决定用户读到什么的就是 detail，所以断它。
        //
        // 两个计时变体（`UntrustedSample` / `StaleRunContext`）在 `From` 里被映射成
        // `RECOVERY_REQUIRED`，因此没有 detail：那时用户读到的就是模板本身。
        let user_text: String = match shown.detail() {
            Some(detail) => {
                assert!(!detail.is_empty(), "{:?} 的 detail 为空", shown.code());
                detail.to_string()
            }
            None => {
                assert!(
                    matches!(shown, AppError::RecoveryRequired),
                    "没有 detail 的只应是映射成 RECOVERY_REQUIRED 的那两个：{:?}",
                    shown.code()
                );
                message.clone()
            }
        };
        assert!(
            user_text
                .chars()
                .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "用户文案必须是中文：{user_text}"
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

// ─────────────────────────────────────────────────────────────────────────────
// 错误上下文的载荷形状（P4 Task 6，裁决 R-T6-a..d）
// ─────────────────────────────────────────────────────────────────────────────
//
// 形状是**逐条请求 → 逐条返回** `{kind, id, row_version}`：
// - `kind` 是白名单枚举，决定读哪张表，**绝不**由客户端指定表名、绝不拼进 SQL；
// - `row_version = null` 表示「**已显式确认不存在**」，与「根本没请求」区分开；
// - 顺序按 kind 白名单顺序、同 kind 内按请求顺序，请求与响应一一对应；
// - 任何一次读取**报错** ⇒ 整体不返回上下文（`authority = None`、`requires_handshake`）；
//   行不存在是**数据**，不是失败。

/// 建库样板照 `tests/transaction_boundary.rs::bootstrap`：
/// 临时文件库 → 迁移 → 一个 run → 四种实体各一条。
struct Fixture {
    _dir: tempfile::TempDir,
    db: Db,
    /// 建库时定下的库身份。
    epoch: String,
}

/// 四种 kind 各一条：`t1`（任务）/ `s1`（会话）/ `p1`（项目）/ `g1`、`g2`（标签）。
///
/// **版本值刻意互不相同**（任务 1 / 会话 0 / 项目 2 / 标签 5）：读错表时行不存在
/// （得到 `None`），读错列时数字也会不一样。任务与项目的版本由真实写路径推出来；
/// 标签在 V0.1 没有改名入口，只能由夹具摆一个值（见下面的注释）。
fn bootstrap() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();

    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let meta = init_meta(&tx).unwrap();
    tx.execute(
        "INSERT INTO application_run(id, started_at) VALUES('run-1', 1000)",
        [],
    )
    .unwrap();

    // 项目：建（0）→ 改名两次（1、2）。
    project_repo::create_project(&tx, "p1", "项目一", 1000).unwrap();
    project_repo::rename_project(&tx, "p1", 0, "项目一改", 1000).unwrap();
    project_repo::rename_project(&tx, "p1", 1, "项目一改二", 1000).unwrap();

    // 任务：建（0）→ 理清中（1）。
    task_repo::create_task(&tx, "t1", "任务一", Some("p1"), 1000).unwrap();
    task_repo::transition_task(
        &tx,
        "t1",
        0,
        TaskStatus::Clarifying,
        TransitionCause::User,
        1000,
    )
    .unwrap();

    // 标签：四类里取两条（`g1` 是上下文类，`g2` 是领域类）。
    tag_repo::create_tag(&tx, "g1", TagKind::Context, "家里", 1000).unwrap();
    tag_repo::create_tag(&tx, "g2", TagKind::Domain, "写作", 1000).unwrap();
    // V0.1 没有标签改名入口，版本推不动，所以手工摆一个与别处都不相同的值。
    tx.execute("UPDATE tag SET row_version = 5 WHERE id = 'g1'", [])
        .unwrap();

    // 会话：直接建一条运行中的（版本 0）。
    session_repo::create_session(
        &tx,
        "s1",
        "t1",
        "run-1",
        SessionMode::Foreground,
        TimerKind::Stopwatch,
        None,
        1000,
        "iv1",
    )
    .unwrap();

    tx.commit().unwrap();
    Fixture {
        _dir: dir,
        db,
        epoch: meta.data_epoch,
    }
}

/// 四类 kind 各请求一条 ⇒ 逐条返回具体版本（R-T6-a：`kind`/`id`/`row_version` 都要钉住）。
#[test]
fn authority_reports_one_version_per_requested_kind() {
    let f = bootstrap();
    let response = capture_error_response(
        &f.db,
        &AppError::VersionConflict {
            expected: 1,
            actual: 2,
        },
        &[
            AuthorityTarget::new(AuthorityKind::Task, "t1"),
            AuthorityTarget::new(AuthorityKind::Session, "s1"),
            AuthorityTarget::new(AuthorityKind::Project, "p1"),
            AuthorityTarget::new(AuthorityKind::Tag, "g1"),
        ],
    );

    assert_eq!(response.code, "VERSION_CONFLICT");
    assert!(!response.requires_handshake);
    let authority = response.authority.expect("读得到就必须给上下文");
    assert_eq!(authority.data_epoch, f.epoch);
    assert_eq!(authority.revision, 0, "只读捕获，不加 revision");
    assert_eq!(
        authority.records,
        vec![
            RecordVersion {
                kind: AuthorityKind::Task,
                id: "t1".into(),
                row_version: Some(1),
            },
            RecordVersion {
                kind: AuthorityKind::Session,
                id: "s1".into(),
                row_version: Some(0),
            },
            RecordVersion {
                kind: AuthorityKind::Project,
                id: "p1".into(),
                row_version: Some(2),
            },
            RecordVersion {
                kind: AuthorityKind::Tag,
                id: "g1".into(),
                row_version: Some(5),
            },
        ]
    );
}

/// `row_version = null` 表示「**已显式确认不存在**」，而且那条**必须出现在响应里**——
/// 它与「根本没请求」是两件事（R-T6-a）。
#[test]
fn authority_marks_absent_targets_as_null_instead_of_omitting_them() {
    let f = bootstrap();
    let response = capture_error_response(
        &f.db,
        &AppError::Domain {
            detail: "找不到这个项目。".into(),
        },
        &[
            AuthorityTarget::new(AuthorityKind::Project, "gone"),
            AuthorityTarget::new(AuthorityKind::Task, "t1"),
        ],
    );

    let authority = response.authority.expect("行不存在是数据，不是失败");
    assert_eq!(
        authority.records,
        vec![
            RecordVersion {
                kind: AuthorityKind::Task,
                id: "t1".into(),
                row_version: Some(1),
            },
            RecordVersion {
                kind: AuthorityKind::Project,
                id: "gone".into(),
                row_version: None,
            },
        ],
        "请求了两条就返回两条，缺的那条用 null 表示「确认不存在」"
    );
    assert!(!response.requires_handshake, "缺行不触发重新握手");

    // 「根本没请求」是另一件事：空请求 ⇒ 空列表，而不是一堆 null。
    let empty = capture_error_response(&f.db, &AppError::RecoveryRequired, &[]);
    let empty = empty.authority.expect("空请求也要给 epoch/revision");
    assert!(empty.records.is_empty());
}

/// 顺序确定：按 kind 白名单顺序、同 kind 内按请求顺序（R-T6-b）。
#[test]
fn authority_orders_by_kind_whitelist_then_request_order() {
    let f = bootstrap();
    let response = capture_error_response(
        &f.db,
        &AppError::RecoveryRequired,
        &[
            AuthorityTarget::new(AuthorityKind::Tag, "g2"),
            AuthorityTarget::new(AuthorityKind::Project, "p1"),
            AuthorityTarget::new(AuthorityKind::Tag, "g1"),
            AuthorityTarget::new(AuthorityKind::Session, "s1"),
            AuthorityTarget::new(AuthorityKind::Task, "t1"),
        ],
    );

    let authority = response.authority.unwrap();
    let order: Vec<(AuthorityKind, &str)> = authority
        .records
        .iter()
        .map(|r| (r.kind, r.id.as_str()))
        .collect();
    assert_eq!(
        order,
        vec![
            (AuthorityKind::Task, "t1"),
            (AuthorityKind::Session, "s1"),
            (AuthorityKind::Project, "p1"),
            // 同 kind 内保持请求顺序：先请求的 g2 在前。
            (AuthorityKind::Tag, "g2"),
            (AuthorityKind::Tag, "g1"),
        ]
    );
}

/// 任何一次读取**报错** ⇒ 整体不返回上下文（R-T6-c）。
#[test]
fn authority_is_dropped_entirely_when_any_read_fails() {
    let f = bootstrap();
    // 故障注入：把项目的版本列写成 TEXT。SQLite 的普通表（非 STRICT）装得下它，
    // 于是 `read_row` 在 `get::<i64>` 上失败——这是**读取报错**，不是「行不存在」。
    f.db.connection()
        .execute("UPDATE project SET row_version = 'x' WHERE id = 'p1'", [])
        .unwrap();

    // 任务那一条本来读得到：要证明的正是「读得到一个也不给半个上下文」。
    let response = capture_error_response(
        &f.db,
        &AppError::Storage {
            detail: "disk".into(),
        },
        &[
            AuthorityTarget::new(AuthorityKind::Task, "t1"),
            AuthorityTarget::new(AuthorityKind::Project, "p1"),
        ],
    );
    assert!(response.authority.is_none(), "有一次读取失败就整体不返回");
    assert!(response.requires_handshake);

    // 只请求那条读得到的任务 ⇒ 上下文正常：失败原因确实只是项目那一条。
    let ok = capture_error_response(
        &f.db,
        &AppError::RecoveryRequired,
        &[AuthorityTarget::new(AuthorityKind::Task, "t1")],
    );
    assert_eq!(ok.authority.unwrap().records[0].row_version, Some(1));
}

/// `requires_handshake` 的第一条分支：库身份不符 ⇒ 要重新握手，但**上下文照给**。
///
/// 第二条分支（读取失败 ⇒ 不给上下文）由
/// [`authority_is_dropped_entirely_when_any_read_fails`] 与
/// `tests/timer_regressions.rs` 的事务时机用例覆盖。
#[test]
fn authority_requires_handshake_on_epoch_mismatch_but_still_returns_records() {
    let f = bootstrap();
    let response = capture_error_response(
        &f.db,
        &AppError::DataEpochMismatch,
        &[AuthorityTarget::new(AuthorityKind::Task, "t1")],
    );

    assert!(response.requires_handshake, "库身份不符必须重新握手");
    let authority = response.authority.expect("读得到就仍然给上下文");
    assert_eq!(
        authority.records,
        vec![RecordVersion {
            kind: AuthorityKind::Task,
            id: "t1".into(),
            row_version: Some(1),
        }]
    );
}

/// 线上形状（P7 按这份接）：`kind` 是小写白名单取值；缺行序列化成 `"row_version":null`，
/// **字段不能省**——省了前端就分不出「确认不存在」与「没请求」。
#[test]
fn authority_wire_shape_keeps_kind_and_null_row_version() {
    let authority = ErrorAuthority {
        data_epoch: "epoch-a".into(),
        revision: 7,
        records: vec![
            RecordVersion {
                kind: AuthorityKind::Project,
                id: "p1".into(),
                row_version: Some(2),
            },
            RecordVersion {
                kind: AuthorityKind::Tag,
                id: "gone".into(),
                row_version: None,
            },
        ],
    };

    assert_eq!(
        serde_json::to_string(&authority).unwrap(),
        r#"{"data_epoch":"epoch-a","revision":7,"records":[{"kind":"project","id":"p1","row_version":2},{"kind":"tag","id":"gone","row_version":null}]}"#
    );
}

/// `kind` 是**白名单枚举**：客户端给不出表名，未知取值在反序列化这一步就被拒（R-T6-a）。
#[test]
fn unknown_authority_kind_is_rejected_at_the_contract_boundary() {
    let bad = serde_json::from_str::<AuthorityTarget>(r#"{"kind":"user_table","id":"x"}"#);
    assert!(bad.is_err(), "不在白名单里的 kind 必须拒绝：{bad:?}");

    let ok = serde_json::from_str::<AuthorityTarget>(r#"{"kind":"tag","id":"g1"}"#).unwrap();
    assert_eq!(ok, AuthorityTarget::new(AuthorityKind::Tag, "g1"));
}
