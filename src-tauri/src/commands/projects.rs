//! 项目（F-004）的请求 DTO 与命令体；`#[tauri::command]` 包装留在 `super`。

use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::services::bootstrap::AppState;
use crate::services::catalog;
use crate::services::events::Broadcaster;

use super::{announce, EpochRequest};

/// 项目列表请求（F-004）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ListProjectsRequest {
    pub expected_data_epoch: String,
    /// `active` / `archived` / `done`（**读路径**取值域，含 V0.1 不写的 `done`）；
    /// `null` / 省略 = 不限制状态，归档与历史都在里面。
    pub status: Option<String>,
}

/// 新建项目（F-004）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreateProjectRequest {
    pub expected_data_epoch: String,
    pub name: String,
}

/// 重命名项目（F-004）。改既有对象 ⇒ 必须带项目版本。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RenameProjectRequest {
    pub expected_data_epoch: String,
    pub project_id: String,
    pub expected_row_version: i64,
    pub name: String,
}

/// 归档项目（F-004）。归档不删任务、不动历史。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ArchiveProjectRequest {
    pub expected_data_epoch: String,
    pub project_id: String,
    pub expected_row_version: i64,
}

/// [`list_projects`] 的命令体（IPC 包装只做转发）。
pub fn list_projects_impl(
    app: &mut AppState,
    request: ListProjectsRequest,
) -> Result<catalog::ProjectList, AppError> {
    let status = match request.status.as_deref() {
        // 读路径：`done` 是库里合法的值，这里必须读得懂（写路径才拒绝它）。
        Some(raw) => Some(catalog::parse_project_status_read(raw)?),
        None => None,
    };
    catalog::list_projects(app.db()?, &request.expected_data_epoch, status)
}

/// [`list_selectable_projects`] 的命令体（IPC 包装只做转发）。
pub fn list_selectable_projects_impl(
    app: &mut AppState,
    request: EpochRequest,
) -> Result<catalog::ProjectList, AppError> {
    catalog::list_selectable_projects(app.db()?, &request.expected_data_epoch)
}

/// [`create_project`] 的命令体（IPC 包装只做转发）。
pub fn create_project_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: CreateProjectRequest,
) -> Result<catalog::ProjectChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) =
        catalog::create_project(app.db_mut()?, env, &request.name, now)?.into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

/// [`rename_project`] 的命令体（IPC 包装只做转发）。
pub fn rename_project_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: RenameProjectRequest,
) -> Result<catalog::ProjectChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
    let (change, changed) =
        catalog::rename_project(app.db_mut()?, env, &request.project_id, &request.name, now)?
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

/// [`archive_project`] 的命令体（IPC 包装只做转发）。
pub fn archive_project_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: ArchiveProjectRequest,
) -> Result<catalog::ProjectChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
    let (change, changed) =
        catalog::archive_project(app.db_mut()?, env, &request.project_id, now)?.into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}
