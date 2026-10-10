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
///
/// `Serialize`（P7 Task 1）：它同时是两条 IPC 通道的形状——
/// `timer_snapshot` / `timer_tick` 两条命令的响应，以及 `timer.tick` 事件的 `payload`
/// （[`crate::services::events::timer_tick_payload`] 直接序列化本类型，不再手抄一份字段表）。
/// `state` / `timer_kind` 的 JSON 形状由 `domain::session` 那两份手写实现决定（落库字符串）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
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
    /// 当前会话所属的任务；无会话时为 `None`。
    ///
    /// **快照为什么必须带任务身份**：`resume_timer` 要 `task_id` +
    /// `task_expected_version`（[`super::coordinator::ResumeRequest`]），而「这个会话属于哪个
    /// 任务」在既有命令里**没有第二条读路径**——`TaskRow` 不带会话，`list_tasks` 也不按会话
    /// 筛，托盘只做 `pause`。于是冷启动（重开窗口，F-009 的正常路径）或托盘暂停之后，
    /// 本窗口不是这条会话的发起方，只有快照能给出任务身份；没有它，「继续」按钮的请求
    /// **根本构造不出来**。
    ///
    /// 与 `session_id` 同一口径：无会话即 `None`；有会话时取自会话行的 `task_id`，
    /// 与会话本身同一次读快照。
    pub task_id: Option<String>,
    /// 任务行的乐观并发版本（`task.row_version`）；无会话时为 `None`。
    ///
    /// 供前端直接填 `ResumeRequest::task_expected_version`，不必再查一次任务。
    /// **每次采样重读任务行**，不在协调器内存里缓存：暂停期间改标题会 bump 任务的
    /// `row_version`，缓存的值会让「继续」拿着过期版本去撞 `VERSION_CONFLICT`。
    pub task_row_version: Option<i64>,
    /// 任务标题（`task.title`）；无会话时为 `None`。
    ///
    /// **为什么标题也要进契约**：既有命令里没有「按 id 取任务」的读路径
    /// （`list_tasks` 只按 status / project / context 筛，`TaskRow` 不带会话），
    /// 所以冷启动（重开窗口）或托盘暂停之后，计时页与状态栏的「当前任务」**没有第二个
    /// 来源**——只补 `task_id` / `task_row_version` 的话，界面只能永久显示占位文案。
    ///
    /// 与任务版本同一口径、同一时机：**每次采样重读任务行**（`build()` 已经为版本读了
    /// 那一行，多带一个字段不额外查库），所以暂停期间改标题下一拍就跟着变，不是缓存值。
    pub task_title: Option<String>,
    /// 当前 run 内递增。**新会话不清零，新 run 才重置**（00 §5）。
    pub tick_seq: u64,
    /// 本次采样的归属挂钟时刻 `A(M)`。
    pub as_of: i64,
    /// 已确认可信工时 + 当前可信开放区间的暂计。
    pub active_ms: i64,
    /// **待确认**的那一段（有候选归属但没被用户确认）。
    ///
    /// 与 `active_ms` **分列**（计划要求）：待确认的时间不是工时，混在一起会让
    /// 用户以为已经算上了。`None` 表示没有待确认段。
    pub pending_ms: Option<i64>,
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
            // 空闲快照没有会话，也就没有任务身份可给——三个字段必须一起是 `None`：
            // 前端的「继续」按钮要求 `task_id` / `task_row_version` / `task_title` 同时可得。
            task_id: None,
            task_row_version: None,
            task_title: None,
            tick_seq,
            as_of,
            active_ms: 0,
            pending_ms: None,
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
