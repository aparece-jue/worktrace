//! Task 状态机（02 §5）。

use super::error::{DomainError, DomainResult};

/// 02 §5 的全部状态。`Scheduled` 在 V0.1 **结构上存在但拒绝写入**——
/// schema 与枚举都保留它，是因为 V0.2 要加，届时不必改已发布的 CHECK。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskStatus {
    Inbox,
    Clarifying,
    Ready,
    Scheduled,
    Doing,
    Blocked,
    Waiting,
    Review,
    Done,
    Cancelled,
}

impl TaskStatus {
    pub const ALL: [TaskStatus; 10] = [
        Self::Inbox,
        Self::Clarifying,
        Self::Ready,
        Self::Scheduled,
        Self::Doing,
        Self::Blocked,
        Self::Waiting,
        Self::Review,
        Self::Done,
        Self::Cancelled,
    ];

    /// 落库用的字符串。与 schema 的 CHECK 取值逐字一致。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "Inbox",
            Self::Clarifying => "Clarifying",
            Self::Ready => "Ready",
            Self::Scheduled => "Scheduled",
            Self::Doing => "Doing",
            Self::Blocked => "Blocked",
            Self::Waiting => "Waiting",
            Self::Review => "Review",
            Self::Done => "Done",
            Self::Cancelled => "Cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// 终结态：只能经显式 reopen 离开。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }

    /// V0.1 是否接受写入。`Scheduled` 属 V0.2（F-105）。
    pub fn is_writable_in_v01(self) -> bool {
        !matches!(self, Self::Scheduled)
    }

    /// 按 02 §5 的跃迁表判断允许性（**不含**「显式 reopen」这一附加条件，
    /// 那个由 [`TaskTransition::new`] 结合 cause 判断）。
    pub fn allowed_targets(self) -> &'static [TaskStatus] {
        use TaskStatus::*;
        match self {
            Inbox => &[Clarifying, Ready, Cancelled],
            Clarifying => &[Inbox, Ready, Cancelled],
            Ready => &[Doing, Scheduled, Blocked, Waiting, Review, Done, Cancelled],
            Scheduled => &[Ready, Doing, Blocked, Waiting, Review, Done, Cancelled],
            Doing => &[Ready, Blocked, Waiting, Review, Done, Cancelled],
            Blocked | Waiting => &[Ready, Cancelled],
            Review => &[Ready, Done, Cancelled],
            Done | Cancelled => &[Ready],
        }
    }
}

/// 跃迁原因。`Reopen` 是 `Done`/`Cancelled` 回到 `Ready` 的**唯一**合法原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionCause {
    /// 用户推进（默认原因）。
    User,
    /// 显式重新打开一个已完成/已取消的任务。
    Reopen,
}

/// 一次已校验的任务跃迁。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskTransition {
    pub from: TaskStatus,
    pub to: TaskStatus,
    pub cause: TransitionCause,
}

impl TaskTransition {
    /// 校验一次跃迁。规则来自 02 §5：
    /// - 目标必须在允许集合里；
    /// - `Scheduled` 在 V0.1 不接受写入（即便它在表里）；
    /// - `Done`/`Cancelled` → `Ready` **只能**由显式 reopen 触发。
    pub fn new(
        from: TaskStatus,
        to: TaskStatus,
        cause: TransitionCause,
    ) -> DomainResult<TaskTransition> {
        if !to.is_writable_in_v01() {
            return Err(DomainError::NotInThisVersion { what: to.as_str() });
        }
        if !from.allowed_targets().contains(&to) {
            return Err(DomainError::IllegalTransition {
                from: from.as_str(),
                to: to.as_str(),
            });
        }
        if from.is_terminal() && cause != TransitionCause::Reopen {
            return Err(DomainError::ReopenMustBeExplicit {
                from: from.as_str(),
            });
        }
        Ok(TaskTransition { from, to, cause })
    }

    /// 02 §5 末尾：「重开时清除当前质量」。
    pub fn clears_quality(self) -> bool {
        self.from.is_terminal() && self.to == TaskStatus::Ready
    }
}
