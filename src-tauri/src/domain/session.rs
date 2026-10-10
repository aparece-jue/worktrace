//! 会话状态与计时类型（02 §3）。

use super::error::{DomainError, DomainResult};

/// 会话状态。取值与 schema 的 CHECK 逐字一致。
///
/// `recovering` 不是「运行中的一种」——它表示**待用户确认**，不再正常计时。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionState {
    Running,
    Paused,
    Recovering,
    Finished,
    Discarded,
}

impl SessionState {
    pub const ALL: [SessionState; 5] = [
        Self::Running,
        Self::Paused,
        Self::Recovering,
        Self::Finished,
        Self::Discarded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Recovering => "recovering",
            Self::Finished => "finished",
            Self::Discarded => "discarded",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// 该状态是否允许出现开放区间。**只有 `running` 允许**。
    pub fn allows_open_interval(self) -> bool {
        matches!(self, Self::Running)
    }

    /// 该状态是否会累计实时暂计。
    pub fn accrues_live_time(self) -> bool {
        matches!(self, Self::Running)
    }

    /// 终态：不再变化（除 `reconcile` 把 recovering 推到终态）。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Finished | Self::Discarded)
    }
}

/// IPC/JSON 形状：**就是落库用的那套小写字符串**（[`SessionState::as_str`]）。
///
/// 手写而不是派生，理由与 [`crate::domain::task::TaskStatus`] 的实现逐字相同。
impl serde::Serialize for SessionState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// 会话模式。02 §6 的统计口径依赖它：人工**只有** `FOREGROUND`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionMode {
    Foreground,
    Background,
    Passive,
    Waiting,
}

impl SessionMode {
    pub const ALL: [SessionMode; 4] = [
        Self::Foreground,
        Self::Background,
        Self::Passive,
        Self::Waiting,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "FOREGROUND",
            Self::Background => "BACKGROUND",
            Self::Passive => "PASSIVE",
            Self::Waiting => "WAITING",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// 02 §6：人工仅 FOREGROUND。禁止把并行机器时长加成人工。
    pub fn counts_as_human(self) -> bool {
        matches!(self, Self::Foreground)
    }
}

/// IPC/JSON 形状：**就是落库用的那套大写字符串**（[`SessionMode::as_str`]）。
///
/// 手写而不是派生，理由与 [`SessionState`] / [`TimerKind`] 的实现逐字相同。
/// P8 Task 2a 起它随 [`crate::storage::session_repo::SessionRow`] 进 IPC 响应
/// （恢复与历史写命令的报告里带会话行）。
impl serde::Serialize for SessionMode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// 计时类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimerKind {
    Stopwatch,
    Countdown,
}

impl TimerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopwatch => "stopwatch",
            Self::Countdown => "countdown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "stopwatch" => Some(Self::Stopwatch),
            "countdown" => Some(Self::Countdown),
            _ => None,
        }
    }
}

/// IPC/JSON 形状：**就是落库用的那套小写字符串**（[`TimerKind::as_str`]）。
///
/// 手写而不是派生，理由与 [`crate::domain::task::TaskStatus`] 的实现逐字相同。
impl serde::Serialize for TimerKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// 会话的计时预算。倒计时必须有正预算；正计时必须没有。
///
/// 这条与 schema 的 `ck_timer_budget` 是同一规则的两处表达——schema 兜底，
/// 这里给出可命名的错误。**两处都要有**：服务层不该靠数据库报错来发现业务违规。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerBudget {
    pub kind: TimerKind,
    pub target_duration_ms: Option<i64>,
}

impl TimerBudget {
    pub fn stopwatch() -> Self {
        Self {
            kind: TimerKind::Stopwatch,
            target_duration_ms: None,
        }
    }

    pub fn countdown(target_duration_ms: i64) -> DomainResult<Self> {
        if target_duration_ms <= 0 {
            return Err(DomainError::NegativeInterval {
                started_at: 0,
                ended_at: target_duration_ms,
            });
        }
        Ok(Self {
            kind: TimerKind::Countdown,
            target_duration_ms: Some(target_duration_ms),
        })
    }

    /// 剩余毫秒；正计时返回 `None`（没有「剩余」这回事）。
    pub fn remaining_ms(self, elapsed_ms: i64) -> Option<i64> {
        self.target_duration_ms.map(|t| (t - elapsed_ms).max(0))
    }

    /// 超时毫秒；倒计时未超时返回 `Some(0)`，正计时返回 `None`。到点**只提示，不自动完成**。
    pub fn overtime_ms(self, elapsed_ms: i64) -> Option<i64> {
        self.target_duration_ms
            .map(|t| elapsed_ms.saturating_sub(t).max(0))
    }
}

/// 会话是否「需要用户处理」。恢复扫描与 UI 都据此分流。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAttention {
    /// 正常。
    None,
    /// 有普通待确认区间：`reconcile` 可以处理。
    NeedsReview,
    /// 不变量损坏：**禁止自动修复**，需要诊断。不能与普通待确认混为一谈。
    InvariantBroken,
}

impl SessionAttention {
    /// IPC/JSON 形状：小写下划线（与 `SessionState` 的落库字符串同一风格）。
    ///
    /// 它不是落库枚举（`work_session` 里没有这一列，它由扫描算出来），所以取值
    /// **只在这一处定义**：前端据它决定「待确认」还是「损坏」两块界面。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::NeedsReview => "needs_review",
            Self::InvariantBroken => "invariant_broken",
        }
    }
}

/// IPC/JSON 形状：就是 [`SessionAttention::as_str`] 那套小写字符串。
///
/// 手写而不是派生，理由与 [`SessionState`] / [`SessionMode`] / [`TimerKind`] 的实现
/// 逐字相同。P8 Task 2b 起它随 [`AttentionOverview`] 进 IPC 响应
/// （`attention_overview` 的列表项带 `attention` 这一位）。
///
/// [`AttentionOverview`]: crate::services::recovery::AttentionOverview
impl serde::Serialize for SessionAttention {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
