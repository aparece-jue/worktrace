//! 错误之后的只读权威版本捕获；不得调用会采样/写恢复事实的 timer.snapshot。
use crate::error::{AppError, ErrorAuthority, ErrorResponse, RecordVersion};
use crate::storage::{db::Db, meta::require_meta, session_repo, task_repo};

/// 在原操作事务结束后、同一串行服务边界内调用。目标 ID 只能来自已解析请求。
/// 版本和 epoch 在同一读事务取得；任何读取失败则不返回部分上下文。
pub fn capture_error_response(
    db: &Db,
    error: &AppError,
    task_id: Option<&str>,
    session_id: Option<&str>,
) -> ErrorResponse {
    let read = || -> Result<ErrorAuthority, AppError> {
        let tx = db
            .connection()
            .unchecked_transaction()
            .map_err(|_| AppError::RecoveryRequired)?;
        let meta = require_meta(&tx)?;
        let task = match task_id {
            Some(id) => task_repo::get_task(&tx, id)?.map(|r| RecordVersion {
                id: r.id,
                row_version: r.row_version,
            }),
            None => None,
        };
        let session = match session_id {
            Some(id) => session_repo::get_session(&tx, id)?.map(|r| RecordVersion {
                id: r.id,
                row_version: r.row_version,
            }),
            None => None,
        };
        Ok(ErrorAuthority {
            data_epoch: meta.data_epoch,
            revision: meta.revision,
            task,
            session,
        })
    };
    let authority = read().ok();
    let requires_handshake = authority.is_none() || matches!(error, AppError::DataEpochMismatch);
    ErrorResponse {
        code: error.code().to_owned(),
        message: error.message(),
        authority,
        requires_handshake,
    }
}
