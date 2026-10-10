//! 握手 `get_revision` 的命令体；`#[tauri::command]` 包装留在 `super`。

use crate::error::AppError;
use crate::services::bootstrap::AppState;
use crate::services::handshake;

/// [`get_revision`] 的命令体（IPC 包装只做转发）。
pub fn get_revision_impl(app: &mut AppState) -> Result<handshake::RevisionSnapshot, AppError> {
    handshake::get_revision(app.db()?)
}
