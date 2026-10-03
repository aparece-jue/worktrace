//! 供**组合服务**（P3 的任务状态编排等）在**自己的事务里**使用的原语。
//!
//! 为什么单独一份：协调器的 `pause`/`finish` 是**自己提交的公开命令**。P3 的
//! 「完成任务时同事务结束它的会话」不能调用它们——那会变成事务里套事务，
//! 而且用户命令的语义（校验 epoch/版本、采样、检测、恰好加一次 revision）
//! 也不属于组合服务的那一次事务。
//!
//! 这里只做**事务内的事实变更**：
//! - 接受 `&Transaction`，**不提交**；
//! - **不加 revision**（一次业务事务只加一次，由拥有事务的那一方加）；
//! - **不采样**（样本由调用方在它自己的串行边界里取得并验证）。
//!
//! 归属终点 `attributed_end` 必须是调用方用 `A(M)` 算出来的**已验证**值——
//! 仓储不推算工时。

use rusqlite::Transaction;

use crate::domain::interval::ClosedIntervalFacts;
use crate::domain::session::SessionState;
use crate::error::AppError;
use crate::storage::guards::guard_row_version;
use crate::storage::session_repo::{self, SessionRow, SessionStateUpdate};

/// 一次「在事务里结束会话」需要的事实。
#[derive(Debug, Clone)]
pub struct EndSessionFacts {
    pub session_id: String,
    /// 调用方期望的会话版本。
    pub expected_row_version: i64,
    /// 归属终点 `A(M)`，由调用方用已验证样本算出。
    pub attributed_end: i64,
    /// 采样到的结束挂钟时刻，原样留存以便日后对账。
    pub sampled_end_wall_at: i64,
    /// 结束后的会话状态。
    pub target_state: SessionState,
}

/// 取会话当前开放区间。没有就报错——组合服务不该在没计时的时候调结束原语。
pub fn open_interval_of(tx: &Transaction<'_>, session_id: &str) -> Result<(String, i64), AppError> {
    for iv in session_repo::intervals_of_session(tx, session_id)? {
        if iv.ended_at.is_none() && iv.voided_at.is_none() {
            return Ok((iv.id, iv.started_at));
        }
    }
    Err(AppError::Domain {
        detail: "当前没有正在计时的区间。".into(),
    })
}

/// **在调用方的事务里**关闭开放区间并置目标状态。不提交、不加 revision。
///
/// `target_state` 为 [`SessionState::Paused`] 或 [`SessionState::Finished`]：
/// - `Paused` 时若当前是 `Running`，先闭合区间；
/// - `Finished` 时 `Running` 与 `Paused` 都可以（02 §3：可从暂停直接结束），
///   只有 `Running` 才有关闭区间的动作。
///
/// `Recovering` 一律拒绝——那要等 P3 的对账流程。
pub fn end_session_in_tx(
    tx: &Transaction<'_>,
    facts: &EndSessionFacts,
) -> Result<SessionRow, AppError> {
    let session =
        session_repo::get_session(tx, &facts.session_id)?.ok_or_else(|| AppError::Domain {
            detail: "会话不存在。".into(),
        })?;
    guard_row_version(session.row_version, facts.expected_row_version)?;

    if session.state == SessionState::Recovering {
        return Err(AppError::RecoveryRequired);
    }
    if facts.target_state == SessionState::Recovering {
        // 「结束」不该把会话推进恢复态；恢复有它自己的入口。
        return Err(AppError::Domain {
            detail: "结束会话不能进入恢复状态。".into(),
        });
    }

    if session.state == SessionState::Running {
        let (interval_id, started_at) = open_interval_of(tx, &facts.session_id)?;
        session_repo::close_interval(
            tx,
            &interval_id,
            ClosedIntervalFacts {
                ended_at: facts.attributed_end,
                duration_ms: Some((facts.attributed_end - started_at).max(0)),
                sampled_end_wall_at: facts.sampled_end_wall_at,
                needs_review: false,
            },
        )?;
    }

    session_repo::update_session_state(
        tx,
        &facts.session_id,
        session.row_version,
        facts.target_state,
        SessionStateUpdate {
            ended_at: Some(facts.attributed_end),
            ..Default::default()
        },
    )
}
