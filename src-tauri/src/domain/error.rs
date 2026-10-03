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
    NotInThisVersion { what: &'static str },
    /// 已经有一个开放区间了；一个会话同时只能有一个。
    IntervalAlreadyOpen,
    /// 没有开放区间，却要求闭合。
    NoOpenInterval,
    /// 该状态不允许有开放区间。
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
    /// 携带的运行上下文已过期：给出的 `run_id` 与持久化事实不符。
    ///
    /// 典型来源是内存里还留着上一轮 run 的基线（08 §1 明确禁止沿用旧 `Instant`）。
    /// 它**不是**普通的参数错误——必须触发恢复流程，所以单独命名。
    StaleRunContext { expected: String, actual: String },
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IllegalTransition { from, to } => write!(f, "illegal transition {from} -> {to}"),
            Self::ReopenMustBeExplicit { from } => {
                write!(f, "reopening from {from} must be an explicit reopen")
            }
            Self::NotInThisVersion { what } => write!(f, "{what} is not part of this version"),
            Self::IntervalAlreadyOpen => write!(f, "an interval is already open"),
            Self::NoOpenInterval => write!(f, "no open interval to close"),
            Self::IntervalOpenInWrongState { state } => {
                write!(f, "state {state} must not hold an open interval")
            }
            Self::NegativeInterval {
                started_at,
                ended_at,
            } => {
                write!(f, "negative interval: {started_at} -> {ended_at}")
            }
            Self::OverlappingInterval {
                existing_start,
                existing_end,
            } => write!(
                f,
                "overlaps an existing interval [{existing_start}, {existing_end})"
            ),
            Self::TrustedIntervalWithoutDuration => {
                write!(f, "a trusted closed interval must carry duration_ms")
            }
            Self::PendingAndVoided => write!(f, "an interval cannot be pending and voided at once"),
            Self::EmptyText { field } => write!(f, "{field} must not be empty"),
            Self::UntrustedSample { reason } => write!(f, "untrusted clock sample: {reason}"),
            Self::StaleRunContext { expected, actual } => {
                write!(f, "stale run context: expected {expected}, got {actual}")
            }
        }
    }
}

impl std::error::Error for DomainError {}

/// 领域层的统一返回类型。
pub type DomainResult<T> = Result<T, DomainError>;
