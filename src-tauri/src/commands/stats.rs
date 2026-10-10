//! 统计（F-010 的「今日工时」半边）`stats_today` 的命令体；`#[tauri::command]` 包装留在 `super`。

use crate::error::AppError;
use crate::services::bootstrap::AppState;
use crate::services::stats;

/// [`stats_today`] 的命令体（IPC 包装只做转发）。
pub fn stats_today_impl(
    app: &mut AppState,
    request: stats::TodayQuery,
) -> Result<stats::TodayView, AppError> {
    app.stats_today(&request)
}
