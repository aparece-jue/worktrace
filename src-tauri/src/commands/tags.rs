//! 标签（F-005）的请求 DTO 与命令体；`#[tauri::command]` 包装留在 `super`。

use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::services::bootstrap::AppState;
use crate::services::catalog;
use crate::services::events::Broadcaster;

use super::announce;

/// 标签列表请求（F-005）。`kind` 省略 = 全部四类。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ListTagsRequest {
    pub expected_data_epoch: String,
    /// `Domain` / `Activity` / `Context` / `Report`（大小写敏感）。
    pub kind: Option<String>,
}

/// 新建标签（F-005）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreateTagRequest {
    pub expected_data_epoch: String,
    pub kind: String,
    pub name: String,
    /// V0.1 没有层级：非空值一律被服务拒绝（`services::catalog::create_tag`）。
    pub parent_id: Option<String>,
}

/// 打标 / 去标（F-005）：一次一个标签、一个任务。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TaskTagRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub tag_id: String,
}

/// 某个任务身上的标签（纯读）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TaskTagsRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
}

/// [`list_tags`] 的命令体（IPC 包装只做转发）。
pub fn list_tags_impl(
    app: &mut AppState,
    request: ListTagsRequest,
) -> Result<catalog::TagList, AppError> {
    let kind = match request.kind.as_deref() {
        Some(raw) => Some(catalog::parse_tag_kind(raw)?),
        None => None,
    };
    catalog::list_tags(app.db()?, &request.expected_data_epoch, kind)
}

/// [`create_tag`] 的命令体（IPC 包装只做转发）。
pub fn create_tag_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: CreateTagRequest,
) -> Result<catalog::TagChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) = catalog::create_tag(
        app.db_mut()?,
        env,
        &request.kind,
        &request.name,
        request.parent_id.as_deref(),
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

/// [`tags_of_task`] 的命令体（IPC 包装只做转发）。
pub fn tags_of_task_impl(
    app: &mut AppState,
    request: TaskTagsRequest,
) -> Result<catalog::TagList, AppError> {
    catalog::tags_of_task(app.db()?, &request.expected_data_epoch, &request.task_id)
}

/// [`tag_task`] 的命令体（IPC 包装只做转发）。
pub fn tag_task_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: TaskTagRequest,
) -> Result<catalog::TaskTagsChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) =
        catalog::tag_task(app.db_mut()?, env, &request.task_id, &request.tag_id, now)?.into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

/// [`untag_task`] 的命令体（IPC 包装只做转发）。
pub fn untag_task_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: TaskTagRequest,
) -> Result<catalog::TaskTagsChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) =
        catalog::untag_task(app.db_mut()?, env, &request.task_id, &request.tag_id, now)?
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
