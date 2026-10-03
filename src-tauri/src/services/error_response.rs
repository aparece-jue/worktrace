//! 错误之后的只读权威版本捕获；不得调用会采样/写恢复事实的 timer.snapshot。
use crate::error::{
    AppError, AuthorityKind, AuthorityTarget, ErrorAuthority, ErrorResponse, RecordVersion,
};
use crate::storage::{db::Db, meta::require_meta, project_repo, session_repo, tag_repo, task_repo};

/// 在原操作事务结束后、同一串行服务边界内调用。请求按**目标**逐条给出，
/// 响应按同一份顺序逐条返回（P4 Task 6，裁决 R-T6-a/b）。
///
/// - `kind` 是白名单枚举，决定读哪张表；表名只来自下面那个 `match` 的分支，
///   **绝不**拼进 SQL；
/// - **行不存在是数据**：那一条返回 `row_version = None`（已显式确认不存在），
///   与「根本没请求」区分开——后者是列表里没有这一条；
/// - **读取报错是失败**：任何一次读取报错 ⇒ 整体不返回上下文
///   （`authority = None`、`requires_handshake = true`），绝不返回半个上下文；
/// - 版本与 epoch 在**同一个读事务**里取得（R-T6-c）。
pub fn capture_error_response(
    db: &Db,
    error: &AppError,
    targets: &[AuthorityTarget],
) -> ErrorResponse {
    let read = || -> Result<ErrorAuthority, AppError> {
        let tx = db
            .connection()
            .unchecked_transaction()
            .map_err(|_| AppError::RecoveryRequired)?;
        let meta = require_meta(&tx)?;

        // 白名单顺序在外、请求顺序在内 ⇒ 输出顺序确定（R-T6-b）：
        // kind 按白名单，同 kind 内保持请求顺序。
        let mut records = Vec::with_capacity(targets.len());
        for kind in AuthorityKind::ALL {
            for target in targets.iter().filter(|t| t.kind == kind) {
                // 四个分支各自把行投影成版本：`kind` 决定读哪张表，表名只在这里出现。
                let row_version = match kind {
                    AuthorityKind::Task => {
                        task_repo::get_task(&tx, &target.id)?.map(|row| row.row_version)
                    }
                    AuthorityKind::Session => {
                        session_repo::get_session(&tx, &target.id)?.map(|row| row.row_version)
                    }
                    AuthorityKind::Project => {
                        project_repo::get_project(&tx, &target.id)?.map(|row| row.row_version)
                    }
                    AuthorityKind::Tag => {
                        tag_repo::get_tag(&tx, &target.id)?.map(|row| row.row_version)
                    }
                };
                records.push(RecordVersion {
                    kind,
                    id: target.id.clone(),
                    row_version,
                });
            }
        }

        Ok(ErrorAuthority {
            data_epoch: meta.data_epoch,
            revision: meta.revision,
            records,
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
