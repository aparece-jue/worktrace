//! P1 Task 1：错误契约（00 §4）。
//!
//! 计划要求的四件事：未知记录、非法状态、epoch 冲突、版本冲突都能被区分；
//! 且错误文本不含数据库路径、SQL 与业务正文。

use std::path::{Path, PathBuf};

use worktrace_lib::domain::error::DomainError;
use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::domain::tag::TagKind;
use worktrace_lib::domain::task::{TaskStatus, TaskTransition, TransitionCause};
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
            "SELECT",
            "INSERT",
            "UPDATE ",
            "DELETE",
            ".db",
            "C:\\",
            "/home/",
            "sqlite",
            // FOLLOW-04 收口时删掉的英文片段：这里是**用户可见文本**（code + message）
            // 的第二道网；`src` 侧的防回潮由 `src_has_no_retired_english_error_text`
            // 扫源码（那份表里没有 `vanished`——`AppError::Storage` 的诊断按契约保留
            // 英文，只是它永远不进用户文案）。
            "no such",
            "vanished",
            "task.title",
            "stopwatch with a budget",
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

/// 从**真实校验入口**造一个领域错误：映射表坏掉时，手写字面量的用例抓不住
/// （终评 M2：`NotInThisVersion` 曾把 `to.as_str()` 直接印进用户文案）。
fn transition_error(from: TaskStatus, to: TaskStatus, cause: TransitionCause) -> DomainError {
    TaskTransition::new(from, to, cause).expect_err("这三个跃迁都必须被拒绝")
}

/// **`DomainError` 的文案是给用户看的**，不是日志。
///
/// 因为 `AppError::Domain { detail }` 的 `message()` 会把 detail 原样拼进去。
/// 这一条曾经是坏的：全部 14 个变体都是英文技术串，用户会看到
/// 「操作不被允许：an interval is already open」。
#[test]
fn domain_error_messages_are_user_facing_chinese() {
    let cases: Vec<DomainError> = vec![
        DomainError::IllegalTransition {
            from: "Inbox",
            to: "Doing",
        },
        DomainError::ReopenMustBeExplicit { from: "Done" },
        // `what` 的取值语义是「这项功能 / 这种组合」：生产里要么是中文
        // （「把任务关联到已完成的项目」等），要么是任务状态名（经 `zh_status`，
        // 由下面三条 `transition_error` 覆盖）。这里用中文取值钉住**直通**路径。
        DomainError::NotInThisVersion { what: "番茄钟" },
        DomainError::IntervalAlreadyOpen,
        DomainError::NoOpenInterval,
        // `state` 保留生产取值（`SessionState::as_str()`）——它经 `zh_session_state`
        // 打中文，正是 ④ 要守的那条：收口前这里走的是任务状态的映射表。
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
        // 字段名是**面向用户**的「任务标题」，不是列名 `task.title`（FOLLOW-04 收口）。
        DomainError::EmptyText {
            field: "任务标题"
        },
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
        // FOLLOW-04（用户可见文案一致性）新增的两个变体。
        DomainError::UnknownSession,
        DomainError::UnknownInterval,
        // 三条**走真实跃迁校验入口**造出来的错误（不是手写字面量）：
        // 手写 `what: "Scheduled"` 的用例对 M2 那种「构造时塞了内部枚举名」的毛病
        // 恒真——只有从入口造出来的错误才带着映射表的结果。
        transition_error(
            TaskStatus::Ready,
            TaskStatus::Scheduled,
            TransitionCause::User,
        ),
        transition_error(
            TaskStatus::Review,
            TaskStatus::Waiting,
            TransitionCause::User,
        ),
        transition_error(
            TaskStatus::Cancelled,
            TaskStatus::Ready,
            TransitionCause::User,
        ),
    ];
    assert_eq!(cases.len(), 28, "28 个变体都要覆盖，加了新的记得补进来");

    for e in cases {
        // ④ 的豁免在 `into()` 之前判：`UnknownEnumValue` 需要回显非法取值与列名。
        let echoes_the_bad_value = matches!(e, DomainError::UnknownEnumValue { .. });
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
        // `TagNameTaken` 曾经直接把 `TagKind::as_str()` 印进文案，正是靠这里拦住；
        // 任务状态这边同样要**十个取值一个不漏**（终评 M2：`Scheduled` / `Review` /
        // `Cancelled` 三个曾是盲区，`NotInThisVersion` 就从缺口里漏出过 `Scheduled`）。
        for banned in [
            "Inbox",
            "Clarifying",
            "Ready",
            "Scheduled",
            "Doing",
            "Waiting",
            "Blocked",
            "Review",
            "Done",
            "Cancelled",
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
        // ④ **变体级**：除 `UnknownEnumValue` 外，用户文案里不得出现 ASCII 字母。
        //
        // ① 只要求「至少含一个 CJK」，中文句子里夹一个英文词照样通过——
        // FOLLOW-04 收口正是靠这条抓出两个漏网：`IntervalOpenInWrongState` 拿着
        // **会话**状态走**任务**状态的映射表（用户会读到「会话处于「recovering」时…」），
        // 以及测试里 `NotInThisVersion { what: "pomodoro" }` 这种非生产取值。
        //
        // 豁免 `UnknownEnumValue`：它要回显**非法取值**与**列名**（「「state」里是一个
        // 无法识别的值 "???"。」），回显是诊断所需，写进豁免说明而不是放宽整条规则；
        // 它的生产构造点（`services::daily_plan`）用的是中文列名「时区」。
        if !echoes_the_bad_value {
            assert!(
                !user_text.chars().any(|c| c.is_ascii_alphabetic()),
                "除 UnknownEnumValue 外的用户文案不得含 ASCII 字母：{user_text}"
            );
        }
    }
}

/// `zh_session_state` **五个取值一个不漏**：`IntervalOpenInWrongState` 的 `state` 直接来自
/// `SessionState::as_str()`，映射表漏掉哪一个，用户就会读到那个英文取值。
///
/// 这是变体级规则（④）的补强：④ 只覆盖用例里那一个会话状态，与任务状态那边用三条
/// `transition_error` 补齐十个取值是同一个道理。
#[test]
fn every_session_state_renders_in_chinese_without_the_raw_value() {
    for state in SessionState::ALL {
        let shown = DomainError::IntervalOpenInWrongState {
            state: state.as_str(),
        }
        .to_string();
        assert!(
            !shown.chars().any(|c| c.is_ascii_alphabetic()),
            "会话状态 {} 没进中文映射表，用户会读到：{shown}",
            state.as_str()
        );
        assert!(!shown.contains(state.as_str()), "不得漏出内部取值：{shown}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 文案门禁（FOLLOW-04：把「用户可见文案必须是中文」变成可机器检查）
// ─────────────────────────────────────────────────────────────────────────────
//
// 上面那条用例是**变体级**：它只看测试里构造的那一份取值。下面两条门禁补上它够不着的
// 两类，三条规则分工如下：
//
// - 变体级（`domain_error_messages_are_user_facing_chinese` 的 ④）：每个 `DomainError`
//   变体渲染出的用户文案不得含 ASCII 字母（`UnknownEnumValue` 豁免，理由见该处）；
// - 构造点级（`inline_user_facing_error_literals_are_chinese`）：`src/**/*.rs` 里
//   **内联字面量**形式的用户文案必须含中文。生产代码里的
//   `EmptyText { field: "task.title" }` 这种列名，变体级规则永远看不到；
// - 禁用子串（`src_has_no_retired_english_error_text`）：本次删掉的英文片段不得回潮。
//
// 表达式形式（`field: FIELD`、`what: to.as_str()`、`detail: other.to_string()`）由变体级
// 规则与各自的映射表（`zh_status` / `zh_kind` / `zh_session_state`）覆盖，扫源码时跳过。

/// 一个字符是不是 CJK 统一表意文字（与 ① 的判据同一个区间）。
fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// `src/**/*.rs` 的（路径，源码），源码已剔除**整行注释**。
///
/// 只剔整行注释（`//` 开头，含 `///`/`//!`）：本仓的文档注释都是整行的，而注释里会
/// 引用历史文案（如「原先借用 `EmptyText{field:"project"}`」）——那不是产出的文案。
/// 空行占位，所以行号与源码一致。
fn src_rust_sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("src 目录必须可读") {
            let path = entry.expect("目录项必须可读").path();
            if path.is_dir() {
                walk(&path, out);
            } else if matches!(path.extension().and_then(|e| e.to_str()), Some("rs")) {
                out.push(path);
            }
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(
        !files.is_empty(),
        "扫不到 src 下的 .rs，门禁等于空转：{root:?}"
    );
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).expect("源码是 UTF-8");
            let code = text
                .lines()
                .map(|line| {
                    if line.trim_start().starts_with("//") {
                        ""
                    } else {
                        line
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            (path, code)
        })
        .collect()
}

/// `anchor`（如 `field:`）之后**紧跟的内联字符串字面量**；表达式形式返回 `None`。
fn inline_literal_after(text: &str, anchor: &str) -> Option<String> {
    let rest = text.get(text.find(anchor)? + anchor.len()..)?;
    let body = rest.trim_start().strip_prefix('"')?;
    let mut literal = String::new();
    let mut escaped = false;
    for c in body.chars() {
        if escaped {
            literal.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Some(literal);
        } else {
            literal.push(c);
        }
    }
    None
}

/// 某个构造点在源码里出现的内联字面量：`(行号, 字面量)`。
fn inline_literals(code: &str, variant: &str, field: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut from = 0usize;
    while let Some(offset) = code[from..].find(variant) {
        let at = from + offset;
        // 只看构造点之后的一小段：够跨过 `Variant {\n    field: "…"`，
        // 又不会把下一个构造点的字面量算到这一个头上。
        let tail: String = code[at..].chars().take(400).collect();
        if let Some(literal) = inline_literal_after(&tail, field) {
            found.push((code[..at].matches('\n').count() + 1, literal));
        }
        from = at + variant.len();
    }
    found
}

/// **构造点级**：`src` 里内联字面量的用户文案必须至少含一个 CJK 字符。
///
/// 变体级规则看的是测试构造的那一份取值；生产代码里的字面量只有扫源码才抓得住——
/// FOLLOW-04 收口前这三处都在漏：`EmptyText { field: "task.title" }`（列名）、
/// `NotInThisVersion { what: "stopwatch with a budget" }`、
/// `AppError::Domain { detail: "no such session" }`（整句英文）。
#[test]
fn inline_user_facing_error_literals_are_chinese() {
    // （构造点, 字段, 这个字面量会被拼进的那句话）
    let patterns = [
        ("EmptyText {", "field:", "「{field}」不能为空。"),
        (
            "NotInThisVersion {",
            "what:",
            "当前版本还没有「{what}」这项功能。",
        ),
        ("AppError::Domain", "detail:", "操作不被允许：{detail}"),
    ];

    let mut scanned = 0usize;
    for (path, code) in src_rust_sources() {
        for (variant, field, template) in patterns {
            for (line, literal) in inline_literals(&code, variant, field) {
                assert!(
                    literal.chars().any(is_cjk),
                    "{}:{} 的 `{}` 是内联字面量，会拼进用户句子「{}」，必须含中文：{:?}",
                    path.display(),
                    line,
                    field,
                    template,
                    literal
                );
                scanned += 1;
            }
        }
    }
    assert!(scanned > 0, "一个字面量都没扫到，门禁等于空转");
}

/// **禁用子串**：本次收口删掉的英文片段不得回到 `src`。
///
/// 表里**没有** `vanished`：`AppError::Storage` 的 detail 是内部诊断（`message()` 固定为
/// 「存储暂时不可用，请稍后重试。」），按契约保留英文，所以 `task vanished after insert`
/// 这类片段留在这里会误伤；它们由 `errors_never_leak_paths_sql_or_payload` 在
/// **用户可见文本**上守。
#[test]
fn src_has_no_retired_english_error_text() {
    let retired = [
        "no such",
        "task.title",
        "stopwatch with a budget",
        "checkpoint requires a trusted running interval",
        "checkpoint attribution and elapsed disagree",
        "checkpoint must not move backwards",
    ];

    for (path, code) in src_rust_sources() {
        for fragment in retired {
            assert!(
                !code.contains(fragment),
                "{} 里还有已收口的英文文案 {:?}：用户可见的错误文案要走 `DomainError`（中文），\
                 或 `EmptyText`/`NotInThisVersion` 的中文字面量；内部诊断才用 `AppError::Storage`",
                path.display(),
                fragment
            );
        }
    }
}

/// `TagNameTaken` 的两件事必须分开：字段留**代码里的取值**（诊断与 T6 的结构化载荷要用），
/// 只有面向用户的 `Display` 走 `zh_kind` 翻成中文（术语见 99-glossary §5）。
///
/// 这条用例守的是「别用把字段改成中文的办法去修泄漏」——那样结构化载荷就废了。
#[test]
fn tag_name_taken_keeps_the_code_value_and_renders_chinese() {
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

/// **四类 kind 各请求一个不存在的 ID** ⇒ 四条都是 `row_version = None`。
///
/// 上一条用例只覆盖了 `Project` 的缺失路径（四个读取分支逐字同形，但
/// 「缺失当成读取失败」这类错误只会打在其中一条上——上一轮的定向篡改就曾打空过一次）。
/// 这一条把四个分支的「缺失 = 数据」逐条钉住，并确认 `null` 真的进了线上 JSON。
#[test]
fn every_kind_reports_a_missing_target_as_null() {
    let f = bootstrap();
    let response = capture_error_response(
        &f.db,
        &AppError::Domain {
            detail: "找不到这个任务。".into(),
        },
        &[
            AuthorityTarget::new(AuthorityKind::Task, "ghost-task"),
            AuthorityTarget::new(AuthorityKind::Session, "ghost-session"),
            AuthorityTarget::new(AuthorityKind::Project, "ghost-project"),
            AuthorityTarget::new(AuthorityKind::Tag, "ghost-tag"),
        ],
    );

    assert!(!response.requires_handshake, "行不存在是数据，不是读取失败");
    let authority = response.authority.expect("四条都读得到（都是「不存在」）");
    let pairs: Vec<(AuthorityKind, &str, Option<i64>)> = authority
        .records
        .iter()
        .map(|r| (r.kind, r.id.as_str(), r.row_version))
        .collect();
    assert_eq!(
        pairs,
        vec![
            (AuthorityKind::Task, "ghost-task", None),
            (AuthorityKind::Session, "ghost-session", None),
            (AuthorityKind::Project, "ghost-project", None),
            (AuthorityKind::Tag, "ghost-tag", None),
        ],
        "四条一一对应，且版本全为 null"
    );

    // `null` 必须真的出现在序列化结果里（省掉字段就退化成「我没拿到」）。
    let json = serde_json::to_string(&authority).unwrap();
    assert_eq!(
        json.matches("\"row_version\":null").count(),
        4,
        "四条都要带显式的 null：{json}"
    );
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
