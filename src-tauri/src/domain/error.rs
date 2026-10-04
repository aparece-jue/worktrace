//! 领域错误（00 §4 的 `AppError::Domain` 的语义化版本）。
//!
//! 领域层**不依赖** `error::AppError` 的具体形状，只表达「哪条规则被违反」，
//! 由上层映射成契约码。这样领域规则可以被单独测试，也不会因为错误信封调整而改动。

/// 领域规则的违反。每个变体对应一条可命名的规则。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    /// 不在允许的跃迁表里（02 §5）。
    IllegalTransition {
        from: &'static str,
        to: &'static str,
    },
    /// `Done`/`Cancelled` 只能经**显式 reopen** 回到 `Ready`，普通推进不行。
    ReopenMustBeExplicit { from: &'static str },
    /// V0.1 不接受的目标状态（目前是 `Scheduled`，属 V0.2）。
    ///
    /// `what` 保留**代码里的原值**（诊断与结构化载荷用），面向用户的 [`std::fmt::Display`]
    /// 经 [`zh_status`] 打中文——与 [`Self::TaskNotInClarifying`] 同一条规矩。
    /// 终评 M2：这里曾经把 `what` 原样印进句子，用户会看到
    /// 「当前版本还没有「Scheduled」这项功能。」。
    NotInThisVersion { what: &'static str },
    /// 已经有一个开放区间了；一个会话同时只能有一个。
    IntervalAlreadyOpen,
    /// 没有开放区间，却要求闭合。
    NoOpenInterval,
    /// 该状态不允许有开放区间。
    ///
    /// `state` 保留**代码里的原值**（`SessionState::as_str()`：`running`/`paused`…，
    /// 供诊断与结构化载荷用），面向用户的 [`std::fmt::Display`] 经 [`zh_session_state`]
    /// 打中文——与 [`Self::TaskNotInClarifying`] 用 [`zh_status`] 是同一条规矩。
    /// FOLLOW-04 收口前这里错用了任务状态的映射表，用户会读到
    /// 「会话处于「recovering」时不该有开放的计时区间。」。
    IntervalOpenInWrongState { state: &'static str },
    /// 区间为负：`ended_at < started_at`。
    NegativeInterval { started_at: i64, ended_at: i64 },
    /// 区间与同会话的另一段有效区间重叠（半开区间，端点相接不算重叠）。
    OverlappingInterval {
        existing_start: i64,
        existing_end: i64,
    },
    /// 可信闭合区间缺 `duration_ms`。
    TrustedIntervalWithoutDuration,
    /// 待确认与已作废是两类事实，不能同时成立。
    PendingAndVoided,
    /// 文本字段为空（去空白后）。
    EmptyText { field: &'static str },
    /// 时钟样本不可信（采样失败或数值回退）。
    UntrustedSample { reason: &'static str },
    /// 库里的枚举值不在取值域内。
    ///
    /// 正常路径上 schema 的 CHECK 挡得住，所以出现它只有两种可能：
    /// 库里被绕过 CHECK 写坏过，或**更新版本写入的新取值被旧版本读到**。
    /// 两种都必须**失败并说清是哪一列**，而不是回落到一个默认值——
    /// 回落会把「读不懂的数据」伪装成合法状态，比报错危险得多。
    UnknownEnumValue { field: &'static str, value: String },
    /// 携带的运行上下文已过期：给出的 `run_id` 与持久化事实不符。
    ///
    /// 典型来源是内存里还留着上一轮 run 的基线（08 §1 明确禁止沿用旧 `Instant`）。
    /// 它**不是**普通的参数错误——必须触发恢复流程，所以单独命名。
    StaleRunContext { expected: String, actual: String },
    // ── P4 Task 2（项目服务与任务归属）新增 ──────────────────────────────────
    /// 目标任务不存在（已被删除，或请求里的 ID 从来不存在）。
    ///
    /// 语义是「找不到」，不是「为空」——所以不复用 [`Self::EmptyText`]。
    UnknownTask,
    /// 目标项目不存在（已被删除，或请求里的 ID 从来不存在）。
    ///
    /// 与版本冲突分开：前者是「这条记录不在」，后者是「你手上的版本旧了」，
    /// 前端对它们的处理不同（重新拉列表 vs 重新拉这一行）。
    UnknownProject,
    /// 项目已归档：不能再接收新任务（02 §2 末、F-004）。
    ///
    /// 归档项目的**历史仍可修正**，被拒的只是「新的归属」。
    ProjectArchived,
    /// 这个任务已经离开理清阶段（`Inbox`/`Clarifying`/`Ready`），归属不能再改。
    ///
    /// V0.1 只提供「理清阶段的可选归类」（F-002）：任务一旦开始，
    /// 归属与状态联动的修改都归 P3，不能从这个最小入口绕过去。
    TaskNotInClarifying { status: &'static str },
    /// 任务正有会话在运行，整理类操作之前必须先停下来。
    ///
    /// 与 [`Self::TaskNotInClarifying`] 分开：那条讲的是任务的生命周期位置，
    /// 这条讲的是「此刻正在计时」——用户的下一步动作不同（停止计时 vs 无从下手）。
    ///
    /// 文案是**中性**的：两个调用方都走这条规则（`set_task_project` 的改归属与
    /// `clarify_ready` 的理清为待办），句子不能只对其中一个成立（Task 5 fix round 1）。
    TaskHasRunningSession,
    // ── P4 Task 3（标签与幂等关联）新增 ────────────────────────────────────
    /// 目标标签不存在（已被删除，或请求里的 ID 从来不存在）。
    ///
    /// 与 [`Self::UnknownTask`] / [`Self::UnknownProject`] 同一形状：「找不到」
    /// 不是「为空」，前端要能分别提示。
    UnknownTag,
    /// 同一个 kind 里已经有同名的根标签了。
    ///
    /// 唯一性口径：kind + 规范化后的名字 + **大小写敏感**（02 §2、`uq_tag_root`）。
    /// 带上 `kind` 与 `name`，用户看到的是「哪一类里的哪个名字」被拒，而不是
    /// 「数据库里有一条唯一约束」——存储层的唯一索引只是并发下的兜底（裁决 R-T3-f）。
    ///
    /// `kind` 保留**代码里的原值**（`TagKind::as_str()`）：它供诊断与结构化载荷用，
    /// 而 [`std::fmt::Display`] 经 `zh_kind` 打中文——用户不该看到 `Domain` 这种
    /// 内部标识（与 [`Self::TaskNotInClarifying`] 用 `zh_status` 是同一条规矩）。
    TagNameTaken { kind: &'static str, name: String },
    // ── P4 Task 5（任务筛选、捕获与理清为待办）新增 ──────────────────────────
    /// 筛选用的情境（上下文）标签不是 `Context` 类（04 F-005「非法情境 ID 拒绝」）。
    ///
    /// 与 [`Self::UnknownTag`] 分开：那条讲的是「这个标签不在」，这条讲的是
    /// 「在，但用错了类别」——用户的下一步动作不同（重新选一个 vs 换个类别）。
    /// `kind` 与 [`Self::TagNameTaken`] 同一口径：字段留代码里的原值，
    /// 只有面向用户的 `Display` 经 `zh_kind` 翻成中文。
    ContextTagRequired { kind: &'static str },
    /// 「理清为待办」只接受 `Inbox`/`Clarifying` 的任务（裁决 R-T5-e）。
    ///
    /// `Doing → Ready`、`Review → Ready` 在 02 §5 的跃迁表里是**合法**的，但那些
    /// 编排归 P3 的状态联动：这个最小入口不能变成绕过联动规则的旁路。
    /// 所以这里要单独说清「这个入口只处理哪两个状态」，而不是借跃迁表的措辞。
    TaskNotClarifiable { status: &'static str },
    // ── FOLLOW-04（用户可见文案一致性）新增 ────────────────────────────────
    /// 目标会话不存在（已被删除，或请求里的 ID 从来不存在）。
    ///
    /// 与 [`Self::UnknownTask`] / [`Self::UnknownProject`] / [`Self::UnknownTag`]
    /// 同一形状：「找不到」不是「为空」，所以不复用 [`Self::EmptyText`]——
    /// 收口前 `session_repo` 的三处借用它，用户会读到「「session」不能为空。」。
    UnknownSession,
    /// 目标计时区间不存在（已被删除，或请求里的 ID 从来不存在）。
    ///
    /// 与 [`Self::UnknownSession`] 同一形状；收口前 `session_repo::close_interval`
    /// 与 `checkpoint_repo::write` 借用 `EmptyText { field: "interval" }`，
    /// 用户会读到「「interval」不能为空。」。
    UnknownInterval,
}

/// 任务状态的中文名。用户的提示语里不该出现 `Inbox` 这种内部标识。
fn zh_status(raw: &str) -> &str {
    match raw {
        "Inbox" => "收集箱",
        "Clarifying" => "理清中",
        "Ready" => "待办",
        "Doing" => "进行中",
        "Waiting" => "等待中",
        "Blocked" => "受阻",
        "Scheduled" => "已排期",
        "Review" => "复盘",
        "Done" => "已完成",
        "Cancelled" => "已取消",
        other => other,
    }
}

/// 标签类别的中文名，术语以 99-glossary §5 的「标签体系」为准
/// （Domain/Activity/Context/Report → 领域/活动/上下文/汇报）。
///
/// 与 [`zh_status`] 同一理由：四类标签是给用户看的概念，`Domain` 这种取值是
/// 代码与库里的内部标识，不该出现在提示语里。`Knowledge` 不在 V0.1 的取值域
/// （`TagKind::parse` 不认它），所以这里没有它的映射。
fn zh_kind(raw: &str) -> &str {
    match raw {
        "Domain" => "领域",
        "Activity" => "活动",
        "Context" => "上下文",
        "Report" => "汇报",
        other => other,
    }
}

/// **会话状态**的中文名，取值与 [`crate::domain::session::SessionState`] 一一对应。
///
/// 与 [`zh_status`] / [`zh_kind`] 同一理由，但**不是同一张表**：任务状态是
/// `Inbox`/`Ready`…，会话状态是 `running`/`paused`…，两者没有任何一个取值重合
/// （FOLLOW-04 收口前 [`DomainError::IntervalOpenInWrongState`] 误用任务状态表，
/// 会话状态整句漏成了英文）。五个取值一个不能少。
fn zh_session_state(raw: &str) -> &str {
    match raw {
        "running" => "运行中",
        "paused" => "已暂停",
        "recovering" => "待确认",
        "finished" => "已结束",
        "discarded" => "已作废",
        other => other,
    }
}

/// **这里的文案是面向用户的**，不是给日志看的。
///
/// 原因：`AppError::Domain { detail }` 的 `message()` 是
/// `format!("操作不被允许：{detail}")`——`detail` 会被**原样拼进用户看到的句子**。
/// 所以它必须是用户读得懂的一句中文，且不得出现内部标识（枚举名、表名、字段名）。
/// 诊断信息应通过 `code` 与结构化字段表达，不要塞进这里。
impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IllegalTransition { from, to } => {
                write!(
                    f,
                    "任务不能从「{}」变成「{}」。",
                    zh_status(from),
                    zh_status(to)
                )
            }
            Self::ReopenMustBeExplicit { from } => {
                write!(f, "「{}」是终态，要重新开始必须显式重开。", zh_status(from))
            }
            // `what` 目前唯一的生产取值是任务状态名（`Scheduled`），所以这里也过一遍
            // `zh_status`：非状态取值（如 `pomodoro`）不在映射表里，原样返回。
            Self::NotInThisVersion { what } => {
                write!(f, "当前版本还没有「{}」这项功能。", zh_status(what))
            }
            Self::IntervalAlreadyOpen => write!(f, "已经有一段正在计时的区间。"),
            Self::NoOpenInterval => write!(f, "当前没有正在计时的区间。"),
            Self::IntervalOpenInWrongState { state } => {
                write!(
                    f,
                    "会话处于「{}」时不该有开放的计时区间。",
                    zh_session_state(state)
                )
            }
            Self::NegativeInterval {
                started_at,
                ended_at,
            } => {
                write!(f, "计时区间时长是负的（{started_at} → {ended_at}）。")
            }
            Self::OverlappingInterval {
                existing_start,
                existing_end,
            } => {
                write!(f, "与已有区间 [{existing_start}, {existing_end}) 重叠。")
            }
            Self::TrustedIntervalWithoutDuration => write!(f, "已确认的计时区间必须有时长。"),
            Self::PendingAndVoided => write!(f, "同一段区间不能既待确认又已作废。"),
            Self::EmptyText { field } => write!(f, "「{field}」不能为空。"),
            Self::UntrustedSample { reason } => write!(f, "时钟采样不可信：{reason}。"),
            Self::UnknownEnumValue { field, value } => {
                write!(f, "「{field}」里是一个无法识别的值 {value:?}。")
            }
            Self::StaleRunContext { expected, actual } => {
                write!(f, "运行上下文已过期：期望 {expected}，实际 {actual}。")
            }
            Self::UnknownTask => write!(f, "找不到这个任务。"),
            Self::UnknownProject => write!(f, "找不到这个项目。"),
            Self::ProjectArchived => write!(f, "项目已归档，不能把任务关联到它。"),
            Self::TaskNotInClarifying { status } => {
                write!(f, "任务处于「{}」时不能改归属。", zh_status(status))
            }
            Self::TaskHasRunningSession => write!(f, "这个任务正在计时，请先停止计时再继续。"),
            Self::UnknownTag => write!(f, "找不到这个标签。"),
            Self::TagNameTaken { kind, name } => {
                write!(
                    f,
                    "「{}」这一类里已经有叫「{name}」的标签了。",
                    zh_kind(kind)
                )
            }
            Self::ContextTagRequired { kind } => {
                write!(
                    f,
                    "「{}」类的标签不能用作上下文筛选，请换一个「上下文」类的标签。",
                    zh_kind(kind)
                )
            }
            Self::TaskNotClarifiable { status } => {
                write!(
                    f,
                    "只有「收集箱」或「理清中」的任务能理清为待办，这个任务处于「{}」。",
                    zh_status(status)
                )
            }
            Self::UnknownSession => write!(f, "找不到这个会话。"),
            Self::UnknownInterval => write!(f, "找不到这段计时区间。"),
        }
    }
}

impl std::error::Error for DomainError {}

/// 领域层的统一返回类型。
pub type DomainResult<T> = Result<T, DomainError>;
