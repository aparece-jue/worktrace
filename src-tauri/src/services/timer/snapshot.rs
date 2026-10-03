//! `TimerSnapshot`：协调器一次采样产出的**唯一**计时快照（00 §5）。
//!
//! 字段命名与 00 §5 统一：`data_epoch`、`revision`、`run_id`、`session_id`、
//! `session_version`、`tick_seq`、`as_of`、`active_ms`、`state`、`timer_kind`、
//! `remaining_ms`/`overtime_ms`。
//!
//! **事实与展示值来自同一次采样**——这是本类型存在的理由。如果分开取数，
//! 展示的 `active_ms` 和它依据的区间就会来自不同瞬间。

use crate::domain::session::{SessionState, TimerKind};

/// 一次采样的完整计时快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerSnapshot {
    /// 库身份。前端据此丢弃旧 epoch 的响应。
    pub data_epoch: String,
    /// 业务版本。与快照同一次读事务取得。
    pub revision: i64,
    /// 本次进程运行代次。重启会换一个。
    pub run_id: String,
    /// 当前会话；无活动会话时为 `None`。
    pub session_id: Option<String>,
    /// 会话的乐观并发版本；无会话时为 `None`。
    pub session_version: Option<i64>,
    /// 当前 run 内递增。**新会话不清零，新 run 才重置**（00 §5）。
    pub tick_seq: u64,
    /// 本次采样的归属挂钟时刻 `A(M)`。
    pub as_of: i64,
    /// 已确认可信工时 + 当前可信开放区间的暂计。
    pub active_ms: i64,
    pub state: Option<SessionState>,
    pub timer_kind: Option<TimerKind>,
    /// 倒计时的剩余毫秒；**正计时为 `None`**（没有「剩余」这回事）。
    pub remaining_ms: Option<i64>,
    /// 倒计时的超时毫秒（未超时为 `Some(0)`）；**正计时为 `None`**。
    pub overtime_ms: Option<i64>,
}

impl TimerSnapshot {
    /// 没有活动会话时的快照。
    pub fn idle(
        data_epoch: String,
        revision: i64,
        run_id: String,
        tick_seq: u64,
        as_of: i64,
    ) -> Self {
        Self {
            data_epoch,
            revision,
            run_id,
            session_id: None,
            session_version: None,
            tick_seq,
            as_of,
            active_ms: 0,
            state: None,
            timer_kind: None,
            remaining_ms: None,
            overtime_ms: None,
        }
    }

    /// 是否处于需要用户处理的会话状态。
    pub fn needs_attention(&self) -> bool {
        matches!(self.state, Some(SessionState::Recovering))
    }

    /// 会话是否在正常计时（决定前端展示「运行中」还是「已暂停」）。
    pub fn is_running(&self) -> bool {
        matches!(self.state, Some(SessionState::Running))
    }
}
