//! 今日计划（F-010 的「今日选择」半边）的请求 DTO 与命令体；`#[tauri::command]` 包装留在 `super`。

use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::services::bootstrap::AppState;
use crate::services::daily_plan;
use crate::services::events::Broadcaster;

use super::announce;

/// 今日计划的加入 / 移除（F-010）。日期与时区都是**原始输入**，
/// 由 `services::daily_plan` 的唯一入口校验与规范化。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PlanMutationRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub date: String,
    pub timezone: String,
}

/// [`plan_for`] 的命令体（IPC 包装只做转发）。
pub fn plan_for_impl(
    app: &mut AppState,
    request: daily_plan::DailyPlanQuery,
) -> Result<daily_plan::DailyPlanView, AppError> {
    daily_plan::plan_for(app.db()?, request)
}

/// [`add_to_plan`] 的命令体（IPC 包装只做转发）。
pub fn add_to_plan_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: PlanMutationRequest,
) -> Result<daily_plan::DailyPlanChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) = daily_plan::add_to_plan(
        app.db_mut()?,
        env,
        &request.task_id,
        &request.date,
        &request.timezone,
        now,
    )?
    .into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

/// [`remove_from_plan`] 的命令体（IPC 包装只做转发）。
pub fn remove_from_plan_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: PlanMutationRequest,
) -> Result<daily_plan::DailyPlanChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) = daily_plan::remove_from_plan(
        app.db_mut()?,
        env,
        &request.task_id,
        &request.date,
        &request.timezone,
        now,
    )?
    .into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}
