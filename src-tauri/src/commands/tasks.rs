//! 任务（F-002）的请求 DTO 与命令体；`#[tauri::command]` 包装留在 `super`。

use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::services::bootstrap::AppState;
use crate::services::catalog;
use crate::services::events::Broadcaster;

use super::announce;

/// 捕获一个任务（F-002 的 Inbox 入口）。新建 ⇒ 只需 epoch。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreateTaskRequest {
    pub expected_data_epoch: String,
    pub title: String,
    pub project_id: Option<String>,
}

/// 理清为待办（F-002）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ClarifyReadyRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub expected_row_version: i64,
}

/// 改任务的归属（F-002）。`project` 是二值：`{"bind":"<id>"}` / `"clear"`。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SetTaskProjectRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub expected_row_version: i64,
    pub project: catalog::ProjectTarget,
}

/// [`list_tasks`] 的命令体（IPC 包装只做转发）。
pub fn list_tasks_impl(
    app: &mut AppState,
    request: catalog::TaskQueryRequest,
) -> Result<catalog::TaskQueryResult, AppError> {
    let query = catalog::TaskQuery::try_from(request)?;
    catalog::list_tasks_filtered(app.db()?, query)
}

/// [`create_task`] 的命令体（IPC 包装只做转发）。
pub fn create_task_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: CreateTaskRequest,
) -> Result<catalog::TaskChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) = catalog::create_task(
        app.db_mut()?,
        env,
        &request.title,
        request.project_id.as_deref(),
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

/// [`clarify_ready`] 的命令体（IPC 包装只做转发）。
pub fn clarify_ready_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: ClarifyReadyRequest,
) -> Result<catalog::TaskChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
    let change = catalog::clarify_ready(app.db_mut()?, env, &request.task_id, now)?;
    // `services::catalog::clarify_ready` 没有 `Unchanged` 分支：它只在真的跃迁时成功，
    // 所以这一次业务写必然改了库。
    Ok(announce(
        broadcaster,
        true,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

/// [`set_task_project`] 的命令体（IPC 包装只做转发）。
pub fn set_task_project_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: SetTaskProjectRequest,
) -> Result<catalog::TaskProjectChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
    let (change, changed) =
        catalog::set_task_project(app.db_mut()?, env, &request.task_id, request.project, now)?
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
