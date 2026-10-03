//! 会话与区间仓储（P1 Task 4）。
//!
//! **工时只来自协调器**：`close_interval` 收的是 `ClosedIntervalFacts`，
//! 仓储不读时钟、不由 wall-now 推算任何时长。

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::domain::error::DomainError;
use crate::domain::interval::ClosedIntervalFacts;
use crate::domain::session::{SessionMode, SessionState, TimerKind};
use crate::error::AppError;

use super::db::map_sqlite;
use super::guards::guard_row_version;

/// `work_session` 的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    pub task_id: String,
    pub run_id: String,
    pub mode: SessionMode,
    pub state: SessionState,
    pub timer_kind: TimerKind,
    pub target_duration_ms: Option<i64>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub needs_review: bool,
    pub row_version: i64,
}

/// `work_interval` 的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntervalRow {
    pub id: String,
    pub session_id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub voided_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub sampled_end_wall_at: Option<i64>,
    pub needs_review: bool,
}

const SESSION_SELECT: &str =
    "SELECT id, task_id, run_id, mode, state, timer_kind, target_duration_ms, \
     started_at, ended_at, needs_review, row_version FROM work_session";
const INTERVAL_SELECT: &str =
    "SELECT id, session_id, started_at, ended_at, voided_at, duration_ms, \
     sampled_end_wall_at, needs_review FROM work_interval";

fn read_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    let mode: String = r.get(3)?;
    let state: String = r.get(4)?;
    let kind: String = r.get(5)?;
    Ok(SessionRow {
        id: r.get(0)?,
        task_id: r.get(1)?,
        run_id: r.get(2)?,
        mode: SessionMode::parse(&mode)
            .ok_or_else(|| super::task_repo::enum_error(3, "work_session.mode", &mode))?,
        state: SessionState::parse(&state)
            .ok_or_else(|| super::task_repo::enum_error(4, "work_session.state", &state))?,
        timer_kind: TimerKind::parse(&kind)
            .ok_or_else(|| super::task_repo::enum_error(5, "work_session.timer_kind", &kind))?,
        target_duration_ms: r.get(6)?,
        started_at: r.get(7)?,
        ended_at: r.get(8)?,
        needs_review: r.get::<_, i64>(9)? != 0,
        row_version: r.get(10)?,
    })
}

fn read_interval(r: &rusqlite::Row<'_>) -> rusqlite::Result<IntervalRow> {
    Ok(IntervalRow {
        id: r.get(0)?,
        session_id: r.get(1)?,
        started_at: r.get(2)?,
        ended_at: r.get(3)?,
        voided_at: r.get(4)?,
        duration_ms: r.get(5)?,
        sampled_end_wall_at: r.get(6)?,
        needs_review: r.get::<_, i64>(7)? != 0,
    })
}

pub fn get_session(conn: &Connection, id: &str) -> Result<Option<SessionRow>, AppError> {
    let sql = format!("{SESSION_SELECT} WHERE id = ?1");
    conn.query_row(&sql, [id], read_session)
        .optional()
        .map_err(map_sqlite)
}

pub fn get_interval(conn: &Connection, id: &str) -> Result<Option<IntervalRow>, AppError> {
    let sql = format!("{INTERVAL_SELECT} WHERE id = ?1");
    conn.query_row(&sql, [id], read_interval)
        .optional()
        .map_err(map_sqlite)
}

pub fn intervals_of_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<IntervalRow>, AppError> {
    let sql = format!("{INTERVAL_SELECT} WHERE session_id = ?1 ORDER BY started_at, id");
    let mut stmt = conn.prepare(&sql).map_err(map_sqlite)?;
    let rows = stmt
        .query_map([session_id], read_interval)
        .map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

/// 建立会话并**同时**开出第一个区间。
///
/// 计划原文：「持久化模式、预算与初始区间」。`mode` 必填——DDL 是 `NOT NULL`，
/// V0.1 只写 FOREGROUND，其余取值由服务层拒绝。
///
/// 预算规则与 schema 的 `ck_timer_budget` 一致，这里给出可命名的领域错误：
/// 倒计时必须有正预算，正计时必须为 `None`。
#[allow(clippy::too_many_arguments)]
pub fn create_session(
    tx: &Transaction<'_>,
    id: &str,
    task_id: &str,
    run_id: &str,
    mode: SessionMode,
    timer_kind: TimerKind,
    target_duration_ms: Option<i64>,
    attributed_start: i64,
    interval_id: &str,
) -> Result<SessionRow, AppError> {
    match (timer_kind, target_duration_ms) {
        (TimerKind::Countdown, Some(ms)) if ms > 0 => {}
        (TimerKind::Countdown, _) => {
            return Err(DomainError::NegativeInterval {
                started_at: 0,
                ended_at: target_duration_ms.unwrap_or(0),
            }
            .into())
        }
        (TimerKind::Stopwatch, None) => {}
        (TimerKind::Stopwatch, Some(_)) => {
            return Err(DomainError::NotInThisVersion {
                what: "stopwatch with a budget",
            }
            .into())
        }
    }

    tx.execute(
        "INSERT INTO work_session(id, task_id, run_id, mode, state, timer_kind, target_duration_ms,
                                  started_at, row_version)
         VALUES(?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7, 0)",
        rusqlite::params![
            id,
            task_id,
            run_id,
            mode.as_str(),
            timer_kind.as_str(),
            target_duration_ms,
            attributed_start
        ],
    )
    .map_err(map_sqlite)?;

    open_interval(tx, interval_id, id, attributed_start)?;

    get_session(tx, id)?.ok_or_else(|| AppError::Storage {
        detail: "session vanished after insert".into(),
    })
}

/// 开一个新区间。唯一索引 `uq_open_interval` 会挡住第二个开放区间。
pub fn open_interval(
    tx: &Transaction<'_>,
    id: &str,
    session_id: &str,
    attributed_start: i64,
) -> Result<(), AppError> {
    let session =
        get_session(tx, session_id)?.ok_or(DomainError::EmptyText { field: "session" })?;
    if !session.state.allows_open_interval() {
        return Err(DomainError::IntervalOpenInWrongState {
            state: session.state.as_str(),
        }
        .into());
    }

    tx.execute(
        "INSERT INTO work_interval(id, session_id, started_at) VALUES(?1, ?2, ?3)",
        rusqlite::params![id, session_id, attributed_start],
    )
    .map_err(map_sqlite)?;
    Ok(())
}

/// 用协调器已验证的事实闭合区间。
pub fn close_interval(
    tx: &Transaction<'_>,
    id: &str,
    facts: ClosedIntervalFacts,
) -> Result<IntervalRow, AppError> {
    let before = get_interval(tx, id)?.ok_or(DomainError::EmptyText { field: "interval" })?;
    if before.ended_at.is_some() {
        return Err(DomainError::IllegalTransition {
            from: "closed",
            to: "closed",
        }
        .into());
    }
    if facts.ended_at < before.started_at {
        return Err(DomainError::NegativeInterval {
            started_at: before.started_at,
            ended_at: facts.ended_at,
        }
        .into());
    }

    tx.execute(
        "UPDATE work_interval
            SET ended_at = ?1, duration_ms = ?2, sampled_end_wall_at = ?3, needs_review = ?4
          WHERE id = ?5 AND ended_at IS NULL",
        rusqlite::params![
            facts.ended_at,
            facts.duration_ms,
            facts.sampled_end_wall_at,
            facts.needs_review as i64,
            id
        ],
    )
    .map_err(map_sqlite)?;

    get_interval(tx, id)?.ok_or_else(|| AppError::Storage {
        detail: "interval vanished after update".into(),
    })
}

/// 更新会话状态，带乐观并发校验。同时 `row_version + 1`。
pub fn update_session_state(
    tx: &Transaction<'_>,
    id: &str,
    expected_version: i64,
    target: SessionState,
    ended_at: Option<i64>,
) -> Result<SessionRow, AppError> {
    let before = get_session(tx, id)?.ok_or(DomainError::EmptyText { field: "session" })?;
    guard_row_version(before.row_version, expected_version)?;

    let n = tx
        .execute(
            "UPDATE work_session SET state = ?1, ended_at = COALESCE(?2, ended_at),
                                     row_version = row_version + 1
             WHERE id = ?3 AND row_version = ?4",
            rusqlite::params![target.as_str(), ended_at, id, expected_version],
        )
        .map_err(map_sqlite)?;
    if n == 0 {
        return Err(AppError::VersionConflict {
            expected: expected_version,
            actual: -1,
        });
    }

    get_session(tx, id)?.ok_or_else(|| AppError::Storage {
        detail: "session vanished".into(),
    })
}
