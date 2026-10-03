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

/// 一次会话状态更新要改的字段。`None` 表示「保持不动」。
///
/// 用结构体而不是一串 `Option` 参数：调用点写 `..Default::default()` 时字段名是自解释的，
/// 而 `update_session_state(tx, id, v, st, None, Some(run), None)` 没人读得懂。
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionStateUpdate<'a> {
    /// 结束时刻。`None` 保持原值。
    pub ended_at: Option<i64>,
    /// 切到当前 run。resume 与恢复重建都要用（02 §10：恢复后显式 resume 要把
    /// `session.run_id` 切到当前 application_run，否则下次启动会把新会话误当旧进程记录）。
    pub run_id: Option<&'a str>,
    /// 待确认标记。异常分割置真、确认/作废置假。
    pub needs_review: Option<bool>,
}

/// 更新会话状态，带乐观并发校验。同时 `row_version + 1`。
///
/// 除状态外还能改 `ended_at`/`run_id`/`needs_review`——resume 要切 run_id，
/// 异常分割要置 needs_review，两者都需要。其余字段仍然只能通过各自的专门入口改。
pub fn update_session_state(
    tx: &Transaction<'_>,
    id: &str,
    expected_version: i64,
    target: SessionState,
    update: SessionStateUpdate<'_>,
) -> Result<SessionRow, AppError> {
    let before = get_session(tx, id)?.ok_or(DomainError::EmptyText { field: "session" })?;
    guard_row_version(before.row_version, expected_version)?;

    let n = tx
        .execute(
            "UPDATE work_session
                SET state = ?1,
                    ended_at = COALESCE(?2, ended_at),
                    run_id = COALESCE(?3, run_id),
                    needs_review = COALESCE(?4, needs_review),
                    row_version = row_version + 1
              WHERE id = ?5 AND row_version = ?6",
            rusqlite::params![
                target.as_str(),
                update.ended_at,
                update.run_id,
                update.needs_review.map(|b| b as i64),
                id,
                expected_version
            ],
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

// ─────────────────────────────────────────────────────────────────────────────
// 异常分割（P2 Task 4）
// ─────────────────────────────────────────────────────────────────────────────

/// 一次异常分割的结果，供协调器写审计与重建内存。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnomalySplit {
    /// 可信前缀的区间 id（保留原 id；`None` 表示没有可信前缀）。
    pub trusted_interval_id: Option<String>,
    /// 可信前缀的结束时刻（= 最后成功检查点的归属时刻）。
    pub trusted_until: Option<i64>,
    /// 待确认余段的区间 id（`None` 表示余段为零长度、已省略）。
    pub pending_interval_id: Option<String>,
    /// 余段的候选结束时刻。**是候选，不是已确认事实**。
    pub candidate_end: i64,
}

/// 把会话当前的开放区间按「最后成功检查点」切成可信前缀与待确认余段。
///
/// 规则（P2 Task 4）：
/// - 有检查点：原区间在检查点处闭合（**它本身就是可信前缀**，id 不变），
///   另开一段从检查点到候选结束的余段并标 `needs_review=1`。
/// - 没有检查点：**整段待确认**——原区间直接标 `needs_review=1` 并在候选结束处闭合。
/// - 前缀零长度（检查点正好在区间起点）时**省略前缀**，不建零长度区间。
///
/// 待确认余段的 `duration_ms` 为 `NULL`：它还没有被确认，不能算成工时。
/// schema 的 `ck_interval_duration` 只要求**可信闭合**必须有 duration，正好允许这一点。
pub fn split_for_anomaly(
    tx: &Transaction<'_>,
    session_id: &str,
    trusted_until: Option<i64>,
    candidate_end: i64,
    sampled_wall_at: Option<i64>,
) -> Result<AnomalySplit, AppError> {
    let open = tx
        .query_row(
            "SELECT id, started_at FROM work_interval
              WHERE session_id = ?1 AND ended_at IS NULL AND voided_at IS NULL",
            [session_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(map_sqlite)?;

    let Some((open_id, started_at)) = open else {
        // 没有开放区间（例如异常在暂停期间被发现）：无需分割。
        return Ok(AnomalySplit {
            trusted_interval_id: None,
            trusted_until: None,
            pending_interval_id: None,
            candidate_end,
        });
    };

    // 可信前缀只在「检查点确实推进过」时才存在；零长度前缀省略。
    if let Some(end) = trusted_until.filter(|t| *t > started_at) {
        // 原区间在检查点处闭合，成为可信前缀
        tx.execute(
            "UPDATE work_interval SET ended_at = ?1, duration_ms = ?2, sampled_end_wall_at = (SELECT wall_at FROM interval_checkpoint WHERE interval_id=?3)
              WHERE id = ?3",
            rusqlite::params![end, end - started_at, open_id],
        )
        .map_err(map_sqlite)?;

        // 余段：从检查点到候选结束，待确认、无 duration
        let pending_id = uuid::Uuid::new_v4().to_string();
        let remainder_end = candidate_end.max(end);
        if remainder_end > end || sampled_wall_at.is_none() {
            tx.execute(
                "INSERT INTO work_interval(id, session_id, started_at, ended_at, duration_ms,
                                           sampled_end_wall_at, needs_review)
                 VALUES(?1, ?2, ?3, ?4, NULL, ?5, 1)",
                rusqlite::params![
                    pending_id,
                    session_id,
                    end,
                    sampled_wall_at.map(|_| remainder_end),
                    sampled_wall_at
                ],
            )
            .map_err(map_sqlite)?;
            return Ok(AnomalySplit {
                trusted_interval_id: Some(open_id),
                trusted_until: Some(end),
                pending_interval_id: Some(pending_id),
                candidate_end: remainder_end,
            });
        }
        return Ok(AnomalySplit {
            trusted_interval_id: Some(open_id),
            trusted_until: Some(end),
            pending_interval_id: None,
            candidate_end: end,
        });
    }

    // 没有可信前缀：整段待确认
    let end = candidate_end.max(started_at);
    tx.execute(
        "UPDATE work_interval SET ended_at = ?1, duration_ms = NULL, sampled_end_wall_at = ?3,
                                  needs_review = 1
          WHERE id = ?2",
        rusqlite::params![sampled_wall_at.map(|_| end), open_id, sampled_wall_at],
    )
    .map_err(map_sqlite)?;
    Ok(AnomalySplit {
        trusted_interval_id: None,
        trusted_until: None,
        pending_interval_id: Some(open_id),
        candidate_end: end,
    })
}

/// 当前唯一运行的前台会话；结束其它暂停会话不能替换它的内存镜像。
pub fn running_foreground(conn: &Connection) -> Result<Option<SessionRow>, AppError> {
    conn.query_row(
        &format!("{SESSION_SELECT} WHERE state='running' AND mode='FOREGROUND'"),
        [],
        read_session,
    )
    .optional()
    .map_err(map_sqlite)
}
/// 拒绝「已有别的前台会话正在计时」这一**可预期的领域冲突**。
///
/// 与 `uq_running_foreground` 的分工：唯一索引是**兜底**（防并发与绕过服务的写入路径），
/// 本函数是**业务事务内的前置检查**——让调用方拿到 `DOMAIN_ERROR` 与可理解的文案，
/// 而不是把一个可预期的冲突报成 `STORAGE_ERROR`（用户会看到「存储暂时不可用，请稍后重试」
/// 并被引导去重试一个永远不会成功的操作）。
///
/// `exclude` 供 `resume` 使用：目标会话自身不算占用。
pub fn require_no_running_foreground(
    conn: &Connection,
    exclude: Option<&str>,
) -> Result<(), AppError> {
    let taken: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM work_session \
             WHERE mode='FOREGROUND' AND state='running' AND id <> ?1)",
            [exclude.unwrap_or("")],
            |r| r.get(0),
        )
        .map_err(map_sqlite)?;
    if taken {
        return Err(AppError::Domain {
            detail: "已有正在计时的前台会话，请先暂停或结束它。".into(),
        });
    }
    Ok(())
}

/// 新归属不能落入既有可信人工时间，或越过未来已有记录。
pub fn require_available_human_start(conn: &Connection, start: i64) -> Result<(), AppError> {
    let conflict: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM work_interval i JOIN work_session s ON s.id=i.session_id WHERE s.mode='FOREGROUND' AND i.voided_at IS NULL AND i.needs_review=0 AND i.ended_at>?1)", [start], |r|r.get(0)).map_err(map_sqlite)?;
    if conflict {
        return Err(AppError::Domain {
            detail: "新的计时归属与已有人工记录冲突，请先确认时间归属。".into(),
        });
    }
    Ok(())
}
