//! 计时命令的请求 DTO 与命令体；`#[tauri::command]` 包装留在 `super`。

use crate::error::AppError;
use crate::services::bootstrap::AppState;
use crate::services::events::Broadcaster;
use crate::services::timer::coordinator::{
    parse_session_mode, parse_timer_kind, CommandOutcome, ResumeRequest, SessionRequest,
    StartRequest,
};
use crate::services::timer::snapshot::TimerSnapshot;

use super::{announce, default_expected_interval_ms};

/// 开始计时（F-003 的 start）。
///
/// `mode` / `timer_kind` 是字符串（见模块头）；`expected_interval_ms` 省略时按
/// 本进程的采样节拍取值——它只用于识别挂起，不是「多久记一次工时」。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct StartTimerRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub task_expected_version: i64,
    pub mode: String,
    pub timer_kind: String,
    /// 倒计时必须有正预算；正计时必须没有（`services::timer` 会拒绝相反的组合）。
    pub target_duration_ms: Option<i64>,
    #[serde(default = "default_expected_interval_ms")]
    pub expected_interval_ms: i64,
}

/// [`timer_snapshot`] 的命令体（IPC 包装只做转发）。
pub fn timer_snapshot_impl(app: &mut AppState) -> Result<TimerSnapshot, AppError> {
    app.snapshot()
}

/// [`timer_tick`] 的命令体（IPC 包装只做转发）。
pub fn timer_tick_impl(app: &mut AppState) -> Result<TimerSnapshot, AppError> {
    app.tick()
}

/// [`start_timer`] 的命令体（IPC 包装只做转发）。
pub fn start_timer_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: StartTimerRequest,
) -> Result<CommandOutcome, AppError> {
    let req = StartRequest {
        expected_data_epoch: request.expected_data_epoch,
        task_id: request.task_id.clone(),
        task_expected_version: request.task_expected_version,
        mode: parse_session_mode(&request.mode)?,
        timer_kind: parse_timer_kind(&request.timer_kind)?,
        target_duration_ms: request.target_duration_ms,
        expected_interval_ms: request.expected_interval_ms,
    };
    // 计时命令没有「幂等重复」这一支：能走到这里就说明这次状态跃迁真的提交了
    // （被拒的命令在服务层就返回错误，不会广播）。
    let outcome = app.start(req)?;
    Ok(announce(
        broadcaster,
        true,
        outcome.snapshot.data_epoch.clone(),
        outcome.revision,
        outcome.snapshot.as_of,
        outcome,
    ))
}

/// [`pause_timer`] 的命令体（IPC 包装只做转发）。
pub fn pause_timer_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: SessionRequest,
) -> Result<CommandOutcome, AppError> {
    // 计时命令没有「幂等重复」这一支：能走到这里就说明这次状态跃迁真的提交了
    // （被拒的命令在服务层就返回错误，不会广播）。
    let outcome = app.pause(request)?;
    Ok(announce(
        broadcaster,
        true,
        outcome.snapshot.data_epoch.clone(),
        outcome.revision,
        outcome.snapshot.as_of,
        outcome,
    ))
}

/// [`resume_timer`] 的命令体（IPC 包装只做转发）。
pub fn resume_timer_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: ResumeRequest,
) -> Result<CommandOutcome, AppError> {
    // 计时命令没有「幂等重复」这一支：能走到这里就说明这次状态跃迁真的提交了
    // （被拒的命令在服务层就返回错误，不会广播）。
    let outcome = app.resume(request)?;
    Ok(announce(
        broadcaster,
        true,
        outcome.snapshot.data_epoch.clone(),
        outcome.revision,
        outcome.snapshot.as_of,
        outcome,
    ))
}

/// [`finish_timer`] 的命令体（IPC 包装只做转发）。
pub fn finish_timer_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: SessionRequest,
) -> Result<CommandOutcome, AppError> {
    // 计时命令没有「幂等重复」这一支：能走到这里就说明这次状态跃迁真的提交了
    // （被拒的命令在服务层就返回错误，不会广播）。
    let outcome = app.finish(request)?;
    Ok(announce(
        broadcaster,
        true,
        outcome.snapshot.data_epoch.clone(),
        outcome.revision,
        outcome.snapshot.as_of,
        outcome,
    ))
}
