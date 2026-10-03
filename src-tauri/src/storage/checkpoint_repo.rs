//! 心跳检查点仓储（P1 Task 4；消费方是 P2 的心跳）。
//!
//! 一次会话的一个区间只有一行检查点（`interval_id` 是主键），每次心跳**覆盖**它。
//! 它代表「最后成功持久化的可信点」——异常分割时用它算可信前缀。
//!
//! **心跳不加 `revision`**：心跳服务拥有自己的短事务，且不得调用
//! `meta::bump_revision`（00 §5：心跳、tick、纯读不加）。

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::domain::error::DomainError;
use crate::error::AppError;

use super::db::map_sqlite;

/// 一条检查点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub interval_id: String,
    pub run_id: String,
    pub wall_at: i64,
    pub attribution_at: i64,
    pub elapsed_ms: i64,
}

/// 写入（或覆盖）检查点。
///
/// 计划要求「检查 run/interval 对应、归属和 elapsed 一致」：
/// - interval 必须存在且仍开放——给已闭合的区间写检查点是无意义的；
/// - 检查点的 `run_id` 必须与该 interval 所属 session 的 `run_id` 一致，
///   否则说明内存里还留着上一个 run 的状态（08 §1 明确禁止沿用旧 Instant）。
pub fn write(tx: &Transaction<'_>, cp: &Checkpoint) -> Result<(), AppError> {
    if cp.elapsed_ms < 0 {
        return Err(DomainError::NegativeInterval {
            started_at: 0,
            ended_at: cp.elapsed_ms,
        }
        .into());
    }

    let (session_run, ended_at, started_at, voided_at, interval_review, state, session_review): (
        String,
        Option<i64>,
        i64,
        Option<i64>,
        bool,
        String,
        bool,
    ) = tx
        .query_row(
            "SELECT s.run_id, i.ended_at, i.started_at, i.voided_at,
                    i.needs_review, s.state, s.needs_review
               FROM work_interval i JOIN work_session s ON s.id = i.session_id
              WHERE i.id = ?1",
            [&cp.interval_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite)?
        .ok_or(DomainError::EmptyText { field: "interval" })?;

    if session_run != cp.run_id {
        return Err(DomainError::StaleRunContext {
            expected: session_run,
            actual: cp.run_id.clone(),
        }
        .into());
    }
    if ended_at.is_some()
        || voided_at.is_some()
        || interval_review
        || session_review
        || state != "running"
    {
        return Err(AppError::Domain {
            detail: "checkpoint requires a trusted running interval".into(),
        });
    }
    if started_at.checked_add(cp.elapsed_ms) != Some(cp.attribution_at) {
        return Err(AppError::Domain {
            detail: "checkpoint attribution and elapsed disagree".into(),
        });
    }
    if let Some(previous) = latest(tx, &cp.interval_id)? {
        if previous.run_id != cp.run_id
            || cp.elapsed_ms < previous.elapsed_ms
            || cp.attribution_at < previous.attribution_at
            || cp.wall_at < previous.wall_at
        {
            return Err(AppError::Domain {
                detail: "checkpoint must not move backwards".into(),
            });
        }
    }

    tx.execute(
        "INSERT INTO interval_checkpoint(interval_id, run_id, wall_at, attribution_at, elapsed_ms)
         VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(interval_id) DO UPDATE SET
             run_id = excluded.run_id,
             wall_at = excluded.wall_at,
             attribution_at = excluded.attribution_at,
             elapsed_ms = excluded.elapsed_ms",
        rusqlite::params![
            cp.interval_id,
            cp.run_id,
            cp.wall_at,
            cp.attribution_at,
            cp.elapsed_ms
        ],
    )
    .map_err(map_sqlite)?;
    Ok(())
}

/// 读某区间最后的检查点。
pub fn latest(conn: &Connection, interval_id: &str) -> Result<Option<Checkpoint>, AppError> {
    conn.query_row(
        "SELECT interval_id, run_id, wall_at, attribution_at, elapsed_ms
           FROM interval_checkpoint WHERE interval_id = ?1",
        [interval_id],
        |r| {
            Ok(Checkpoint {
                interval_id: r.get(0)?,
                run_id: r.get(1)?,
                wall_at: r.get(2)?,
                attribution_at: r.get(3)?,
                elapsed_ms: r.get(4)?,
            })
        },
    )
    .optional()
    .map_err(map_sqlite)
}
