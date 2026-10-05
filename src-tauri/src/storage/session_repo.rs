//! 会话与区间仓储（P1 Task 4）。
//!
//! **工时只来自协调器**：`close_interval` 收的是 `ClosedIntervalFacts`，
//! 仓储不读时钟、不由 wall-now 推算任何时长。

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::domain::error::DomainError;
use crate::domain::interval::ClosedIntervalFacts;
use crate::domain::session::{SessionMode, SessionState, TimerKind};
use crate::error::AppError;

use super::checkpoint_repo;
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

/// `work_interval` 的一行加上它所属会话的模式（P5 统计的 measure 归类要用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntervalWithMode {
    pub interval: IntervalRow,
    pub mode: SessionMode,
}

const SESSION_SELECT: &str =
    "SELECT id, task_id, run_id, mode, state, timer_kind, target_duration_ms, \
     started_at, ended_at, needs_review, row_version FROM work_session";
const INTERVAL_SELECT: &str =
    "SELECT id, session_id, started_at, ended_at, voided_at, duration_ms, \
     sampled_end_wall_at, needs_review FROM work_interval";
/// 与 [`INTERVAL_SELECT`] 同序，但带表别名：跨表的恢复查询（P7 Task 0）要 JOIN。
const INTERVAL_SELECT_ALIASED: &str =
    "SELECT i.id, i.session_id, i.started_at, i.ended_at, i.voided_at, i.duration_ms, \
     i.sampled_end_wall_at, i.needs_review FROM work_interval i";
/// 与 [`INTERVAL_SELECT_ALIASED`] 同序，末尾多一列 `s.mode`：P5 的范围统计要按会话
/// 模式分 measure（人工 / 机器 / 等待），JOIN 一次就取回来，不再逐会话回查（避免 N+1）。
const INTERVAL_WITH_MODE_SELECT: &str =
    "SELECT i.id, i.session_id, i.started_at, i.ended_at, i.voided_at, i.duration_ms, \
     i.sampled_end_wall_at, i.needs_review, s.mode FROM work_interval i \
     JOIN work_session s ON s.id = i.session_id";

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

/// 与 [`INTERVAL_WITH_MODE_SELECT`] 同序：前 8 列是 [`IntervalRow`]，第 9 列是会话模式。
fn read_interval_with_mode(r: &rusqlite::Row<'_>) -> rusqlite::Result<IntervalWithMode> {
    let mode: String = r.get(8)?;
    Ok(IntervalWithMode {
        interval: read_interval(r)?,
        mode: SessionMode::parse(&mode)
            .ok_or_else(|| super::task_repo::enum_error(8, "work_session.mode", &mode))?,
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

/// 与半开范围 `[from, to)` **相交**的区间（连同会话模式），按 `(started_at, id)` 升序。
///
/// 这是 P5 统计（范围报表 / Today / 导出）**唯一**的区间取数入口：**不按会话遍历各查
/// 一次**——那正是计划明写要避免的 N+1。半开相交的谓词与
/// [`require_no_human_overlap`] 同形，没有第二处再写一遍。
///
/// **排除口径只在这里做一次**（服务层不再重判）：
/// - `i.voided_at IS NULL`：已作废的区间不进任何统计（02 §6），只能在历史/审计里看；
/// - `s.state <> 'discarded'`：已丢弃会话的区间同样不进统计；
/// - `(i.ended_at IS NULL OR i.started_at < i.ended_at)`：P3 收紧的「**空区间不占时间**」
///   ——零长度段与任何半开范围都不相交，本来就不该出现在结果里。
///
/// **`needs_review = 1` 不在这里排除**：待确认区间要单列一栏（02 §6），由服务层按
/// `attention_overview` 的待确认集合分出来，丢掉它反而会让用户看不到要处理的那条。
///
/// 调用方保证 `from <= to`（服务层用 `IntervalRange::new` 校验）；本函数不自行
/// begin/commit，也不加版本。
pub fn intervals_overlapping(
    conn: &Connection,
    from: i64,
    to: i64,
) -> Result<Vec<IntervalWithMode>, AppError> {
    let sql = format!(
        "{INTERVAL_WITH_MODE_SELECT} \
         WHERE s.state <> 'discarded' AND i.voided_at IS NULL \
           AND (i.ended_at IS NULL OR i.started_at < i.ended_at) \
           AND i.started_at < ?2 \
           AND (i.ended_at IS NULL OR ?1 < i.ended_at) \
         ORDER BY i.started_at, i.id"
    );
    let mut stmt = conn.prepare(&sql).map_err(map_sqlite)?;
    let rows = stmt
        .query_map(rusqlite::params![from, to], read_interval_with_mode)
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
            // `what` 是**面向用户**的「这项功能/这种组合」，不是代码里的标识：
            // 收口前这里是 `stopwatch with a budget`，用户会读到整句英文。
            return Err(DomainError::NotInThisVersion {
                what: "给正计时设预算",
            }
            .into());
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
    let session = get_session(tx, session_id)?.ok_or(DomainError::UnknownSession)?;
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
    let before = get_interval(tx, id)?.ok_or(DomainError::UnknownInterval)?;
    if before.ended_at.is_some() {
        // 没有开放区间却要求闭合：这条规则本来就是 `NoOpenInterval` 的语义
        // （「没有开放区间，却要求闭合」）。原先借用 `IllegalTransition` 并塞进
        // `from: "closed", to: "closed"`，用户会读到一句关于**任务**跃迁、
        // 且带内部标识的胡话：「任务不能从「closed」变成「closed」。」。
        return Err(DomainError::NoOpenInterval.into());
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

// ─────────────────────────────────────────────────────────────────────────────
// 手工补录的建会话原语（P3 Task 4，S6）
// ─────────────────────────────────────────────────────────────────────────────

/// 一条**补录**会话的全部字段。
///
/// `started_at`/`ended_at`/`duration_ms` 由服务算好（仓储不读时钟、不推算工时）；
/// 其余字段是补录的固定形状，调用方照样逐字段写出来——一眼能看出「补录只有这一种」。
pub struct NewFinishedSession<'a> {
    pub id: &'a str,
    pub task_id: &'a str,
    pub run_id: &'a str,
    pub mode: SessionMode,
    pub timer_kind: TimerKind,
    pub target_duration_ms: Option<i64>,
    pub interval_id: &'a str,
    pub started_at: i64,
    pub ended_at: i64,
    pub duration_ms: i64,
}

/// 建立一个**终态**会话与它的可信闭合区间（P3 Task 4 的 `backfill`）。
///
/// **为什么不能复用 [`create_session`]**：它把 `state` 硬编 `'running'` 并紧跟
/// [`open_interval`]——那会短暂占用 `uq_running_foreground` 的前台槽位，而补录
/// 既不计时也不占前台（02 §3）。这里 `state` 直接写 `finished`：会话建好就是终态，
/// 一条 `running` 行都不会出现。
///
/// 区间两端都是**用户给定的事实**（不是候选端点）：`needs_review = 0`、
/// `duration_ms` 与起止同一次写入，所以 `ck_interval_duration` 恒成立。
/// `sampled_end_wall_at` 留 `NULL`——补录没有采样，不得把服务算的时刻写成采样证据；
/// 也**不写** `interval_checkpoint`（检查点是计时路径的产物）。
pub fn create_finished_session(
    tx: &Transaction<'_>,
    facts: &NewFinishedSession<'_>,
) -> Result<SessionRow, AppError> {
    tx.execute(
        "INSERT INTO work_session(id, task_id, run_id, mode, state, timer_kind,
                                  target_duration_ms, started_at, ended_at, needs_review,
                                  row_version)
         VALUES(?1, ?2, ?3, ?4, 'finished', ?5, ?6, ?7, ?8, 0, 0)",
        rusqlite::params![
            facts.id,
            facts.task_id,
            facts.run_id,
            facts.mode.as_str(),
            facts.timer_kind.as_str(),
            facts.target_duration_ms,
            facts.started_at,
            facts.ended_at
        ],
    )
    .map_err(map_sqlite)?;

    tx.execute(
        "INSERT INTO work_interval(id, session_id, started_at, ended_at, duration_ms,
                                   sampled_end_wall_at, needs_review, voided_at)
         VALUES(?1, ?2, ?3, ?4, ?5, NULL, 0, NULL)",
        rusqlite::params![
            facts.interval_id,
            facts.id,
            facts.started_at,
            facts.ended_at,
            facts.duration_ms
        ],
    )
    .map_err(map_sqlite)?;

    get_session(tx, facts.id)?.ok_or_else(|| AppError::Storage {
        detail: "session vanished after insert".into(),
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
    let before = get_session(tx, id)?.ok_or(DomainError::UnknownSession)?;
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

// ─────────────────────────────────────────────────────────────────────────────
// 启动恢复的归一原语（P3 Task 1，S5）
// ─────────────────────────────────────────────────────────────────────────────

/// 旧 run 的「`running` + 开放区间」→「可信前缀（可信闭合）+ 终点未知的待确认段」。
///
/// **为什么不能复用 [`split_for_anomaly`]**：它用 `sampled_wall_at: Option<i64>`
/// 决定 `ended_at`——`None` 时 `ended_at` 也写成 `NULL`，而启动扫描**没有**这一拍的
/// 墙钟样本（进程刚起来，旧 run 的样本不存在）。那会留下
/// `voided_at IS NULL AND ended_at IS NULL` 的行，让下一次启动命中**自造的**
/// `open_interval_outside_running`。
///
/// 精确动作（C3）：
/// - 闭合点 `t` = 最后成功检查点的 `attribution_at`，且必须**严格越过**区间起点
///   （`t > started_at`）才算可信；
/// - **有可信前缀**：原区间在 `t` 处闭合（id 不变）——`duration_ms = t - started_at`、
///   `sampled_end_wall_at` = 检查点挂钟、`needs_review = 0`；另插一行 `[t, t)` 的
///   **零长度候选段**（`duration_ms = NULL`、`sampled_end_wall_at = NULL`、
///   `needs_review = 1`）。候选端点**不是**事实，所以不给它编时长。
/// - **没有可信前缀**（无检查点，或检查点没推进过）：整段待确认——原区间
///   `ended_at = started_at`、`duration_ms = NULL`、`sampled_end_wall_at = NULL`、
///   `needs_review = 1`。
///
/// 两种情形都**不留** `voided_at IS NULL AND ended_at IS NULL` 的行：`uq_open_interval`
/// 会挡住下一个开放区间，`open_interval_outside_running` 会把残留判成损坏。
///
/// 判据之间不一致时（第 2 类的前提就是「有开放区间」，走到这里说明查询与事实对不上）
/// 返回 [`DomainError::NoOpenInterval`]，由扫描层按第 1 类降级——**不**静默返回一个
/// 全 `None` 的 split（那会把「损坏」伪装成「没事」）。
pub fn normalize_crashed_open_interval(
    tx: &Transaction<'_>,
    session_id: &str,
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
        return Err(DomainError::NoOpenInterval.into());
    };

    let checkpoint = checkpoint_repo::latest(tx, &open_id)?;
    let trusted_until = checkpoint
        .as_ref()
        .map(|cp| cp.attribution_at)
        .filter(|t| *t > started_at);

    if let Some(end) = trusted_until {
        // 原区间成为可信前缀：在检查点处闭合，保留原 id。
        tx.execute(
            "UPDATE work_interval
                SET ended_at = ?1, duration_ms = ?2, sampled_end_wall_at = ?3, needs_review = 0
              WHERE id = ?4",
            rusqlite::params![
                end,
                end - started_at,
                checkpoint.as_ref().map(|cp| cp.wall_at),
                open_id
            ],
        )
        .map_err(map_sqlite)?;

        // 余段：起止都是候选端点，`duration_ms` 留空——它还没有被确认。
        let pending_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO work_interval(id, session_id, started_at, ended_at, duration_ms,
                                       sampled_end_wall_at, needs_review)
             VALUES(?1, ?2, ?3, ?3, NULL, NULL, 1)",
            rusqlite::params![pending_id, session_id, end],
        )
        .map_err(map_sqlite)?;

        return Ok(AnomalySplit {
            trusted_interval_id: Some(open_id),
            trusted_until: Some(end),
            pending_interval_id: Some(pending_id),
            candidate_end: end,
        });
    }

    // 没有可信前缀：整段待确认，终点收在起点上（同样是候选）。
    tx.execute(
        "UPDATE work_interval
            SET ended_at = started_at, duration_ms = NULL, sampled_end_wall_at = NULL,
                needs_review = 1
          WHERE id = ?1",
        [&open_id],
    )
    .map_err(map_sqlite)?;

    Ok(AnomalySplit {
        trusted_interval_id: None,
        trusted_until: None,
        pending_interval_id: Some(open_id),
        candidate_end: started_at,
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

// ─────────────────────────────────────────────────────────────────────────────
// 人工时间的重叠校验与区间事实写入口（P3 Task 2，S7/S8）
//
// 服务层**不写 SQL**：确认、作废、重叠校验各是一条命名原语。三条都取调用方的
// `&Transaction`（读入口取 `&Connection`，`Transaction` 自动 `Deref`），不自行
// begin/commit、不自行加 revision——事务与版本归服务层。
// ─────────────────────────────────────────────────────────────────────────────

/// `[start, end)` 是否与**全部有效人工区间**相交（半开，端点相接不算）。
///
/// 口径（02 §3/§6、08 §1）：
/// - 只算 **FOREGROUND** 会话的区间（`BACKGROUND`/`PASSIVE`/`WAITING` 这类机器时间
///   按独立口径，不参与人工时间的互斥）；
/// - 只算**有效**区间：`voided_at IS NULL` 且 `needs_review = 0`。
///   **待确认的候选范围不是已确认的重叠事实**（08 §1 原文：「候选时长只是证据，
///   确认时仍要重新校验冲突」），所以 `needs_review = 1` 的行不参与——它们正是
///   本次要确认或作废的对象；
/// - **正在计时（`ended_at IS NULL`）的区间也算**：它还没有终点，但起点已经落在
///   时间轴上。确认一段与它相交的时间会造出两段互相覆盖的人工区间，而
///   「恢复确认不允许与后来已记录人工时间重叠」（02 §3 原文）正是要挡住这件事。
///   它的右端点未知，所以命中时给的是 [`AppError::Domain`] 的整句中文，
///   **不**编一个假的 `existing_end` 去凑 [`DomainError::OverlappingInterval`]。
///
/// `exclude_interval` 供 `correct` 排除被修正的那一段自身（否则它会与自己相撞）。
pub fn require_no_human_overlap(
    conn: &Connection,
    start: i64,
    end: i64,
    exclude_interval: Option<&str>,
) -> Result<(), AppError> {
    // 半开相交：`existing.started_at < end AND start < existing.ended_at`。
    // 上式仅适用于非空区间；[t,t) 不占任何时间，两个方向都要排除空集。
    if start == end {
        return Ok(());
    }
    // 没有终点的行按「延伸到未来」处理，所以只要 `existing.started_at < end` 就算相交。
    let hit = conn
        .query_row(
            "SELECT i.started_at, i.ended_at FROM work_interval i \
               JOIN work_session s ON s.id = i.session_id \
              WHERE s.mode = 'FOREGROUND' AND i.voided_at IS NULL AND i.needs_review = 0 \
                AND (i.ended_at IS NULL OR i.started_at < i.ended_at) \
                AND (?3 IS NULL OR i.id <> ?3) \
                AND i.started_at < ?2 \
                AND (i.ended_at IS NULL OR ?1 < i.ended_at) \
              ORDER BY i.started_at, i.id LIMIT 1",
            rusqlite::params![start, end, exclude_interval],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?)),
        )
        .optional()
        .map_err(map_sqlite)?;
    match hit {
        None => Ok(()),
        Some((existing_start, Some(existing_end))) => Err(DomainError::OverlappingInterval {
            existing_start,
            existing_end,
        }
        .into()),
        Some(_) => Err(AppError::Domain {
            detail: "与一段正在计时的区间重叠，请先把它停下来再确认。".into(),
        }),
    }
}

/// 确认一段待确认区间：写入用户给定的起止与由起止算出的时长，清 `needs_review`。
///
/// - 只接受 `needs_review = 1 AND voided_at IS NULL` 的行。已作废、或本来就已确认的行
///   一律拒绝：那是把两类事实混在一起（[`DomainError::PendingAndVoided`]）。
///   服务层已经用「待确认集合逐一对应」挡住了这两种输入，这里是原语自己的契约。
/// - `voided_at` 保持 `NULL`、`sampled_end_wall_at` **保持原值**：确认只补终点与时长，
///   不改采样留痕。
/// - 时长由调用方按 `ended_at - started_at` 给（服务已校验 `ended_at >= started_at`）；
///   仓储不读时钟、不推算工时。
pub fn confirm_interval(
    tx: &Transaction<'_>,
    interval_id: &str,
    started_at: i64,
    ended_at: i64,
    duration_ms: i64,
) -> Result<IntervalRow, AppError> {
    let before = get_interval(tx, interval_id)?.ok_or(DomainError::UnknownInterval)?;
    if before.voided_at.is_some() || !before.needs_review {
        return Err(DomainError::PendingAndVoided.into());
    }

    let changed = tx
        .execute(
            "UPDATE work_interval
                SET started_at = ?1, ended_at = ?2, duration_ms = ?3, needs_review = 0
              WHERE id = ?4 AND needs_review = 1 AND voided_at IS NULL",
            rusqlite::params![started_at, ended_at, duration_ms, interval_id],
        )
        .map_err(map_sqlite)?;
    if changed == 0 {
        return Err(DomainError::PendingAndVoided.into());
    }

    get_interval(tx, interval_id)?.ok_or_else(|| AppError::Storage {
        detail: "interval vanished after confirm".into(),
    })
}

/// 作废一段区间（软删除，保留审计）：`voided_at = …`、清 `needs_review`。
///
/// 按「时长是否已知」处理端点（S8 / 2026-10-04 复审补正）：
/// - `duration_ms IS NOT NULL`（可信闭合）⇒ 起止与时长**原样保留**；
/// - `duration_ms IS NULL`（候选段，终点本来就未知）⇒ `ended_at` 清回 `NULL`。
///   给未确认的候选补 0 时长就是造数，而既有 `ck_interval_duration` 不豁免作废行
///   （「已作废 + 已闭合 + 无时长」直接违反 CHECK）。修改前的候选端点完整记在
///   `time_edit.before_json` 里。
///
/// 已经作废的行再作废一次是幂等的：`voided_at` 保留第一次的时刻（与
/// `run_repo::mark_clean_exit` 同一口径），不覆盖历史。
pub fn void_interval(
    tx: &Transaction<'_>,
    interval_id: &str,
    voided_at: i64,
) -> Result<IntervalRow, AppError> {
    let before = get_interval(tx, interval_id)?.ok_or(DomainError::UnknownInterval)?;
    if before.voided_at.is_some() {
        return Ok(before);
    }

    tx.execute(
        "UPDATE work_interval
            SET voided_at = ?1, needs_review = 0,
                ended_at = CASE WHEN duration_ms IS NULL THEN NULL ELSE ended_at END
          WHERE id = ?2",
        rusqlite::params![voided_at, interval_id],
    )
    .map_err(map_sqlite)?;

    get_interval(tx, interval_id)?.ok_or_else(|| AppError::Storage {
        detail: "interval vanished after void".into(),
    })
}

/// 重定时一段**已确认且未作废**的区间（S8 的第三条，P3 Task 3 的 `correct` 用）。
///
/// - 只接受 `needs_review = 0 AND voided_at IS NULL`：待确认的候选端点不是事实
///   （要先用 `reconcile` 确认），已作废的行只能在历史与审计里看。服务层已经用
///   「可信历史」前置挡住这两种输入，这里是原语自己的契约。
/// - `started_at`/`ended_at`/`duration_ms` **一次 UPDATE 同时写**：`duration_ms` 是
///   同一事实的校验值，不能独立编辑（08 §1），也不允许出现「改了起止但时长没跟着变」
///   的行（`ck_interval_duration` 兜底）。
/// - 时长由调用方按 `ended_at - started_at` 给；仓储不读时钟、不推算工时。
pub fn retime_interval(
    tx: &Transaction<'_>,
    interval_id: &str,
    started_at: i64,
    ended_at: i64,
    duration_ms: i64,
) -> Result<IntervalRow, AppError> {
    let before = get_interval(tx, interval_id)?.ok_or(DomainError::UnknownInterval)?;
    if before.voided_at.is_some() || before.needs_review {
        return Err(AppError::Domain {
            detail: "只有已确认且未作废的区间能重新定时。".into(),
        });
    }

    let changed = tx
        .execute(
            "UPDATE work_interval
                SET started_at = ?1, ended_at = ?2, duration_ms = ?3
              WHERE id = ?4 AND needs_review = 0 AND voided_at IS NULL",
            rusqlite::params![started_at, ended_at, duration_ms, interval_id],
        )
        .map_err(map_sqlite)?;
    if changed == 0 {
        return Err(AppError::Domain {
            detail: "只有已确认且未作废的区间能重新定时。".into(),
        });
    }

    get_interval(tx, interval_id)?.ok_or_else(|| AppError::Storage {
        detail: "interval vanished after retime".into(),
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// 恢复扫描用的查询（P7 Task 0；P3 Task 1 泛化成「可选 run 过滤」）
//
// **为什么必须能按 `run_id` 过滤**：既有的 `running_foreground`（本文件上文）不带
// `run_id`——它回答的是「现在有没有前台在跑」，而启动扫描问的是另一个问题：
// 「**不是本次 run** 的会话里，有没有没结束的、待确认的、或事实已经坏掉的」。
// 两个问题的答案在崩溃重启后恰好相反：崩溃留下的 running 行必须被判为
// 「上一代次的残留」，不能被当成本次 run 正在计时的会话。
//
// 这三个查询是 P3 之前的**开发验证库门禁**（Task 0 第 8 条）：命中任一就不许
// 开新计时，等恢复完成。四类判定归 P3，不在这里下结论。
//
// P3 Task 1（S9）：每个查询收 `Option<&str>`——`Some(current)` = 排除当前 run
// （门禁与启动扫描），`None` = **不限 run**（全局待确认概览）。两种语义由
// SQL 里的 `(?1 IS NULL OR s.run_id <> ?1)` 一个参数承载：**一份谓词，两个入口**，
// 不许复制 SQL。既有签名（`*_of_other_runs`）保留为薄包装，
// `tests/startup_order.rs::the_recovery_scan_only_counts_sessions_of_other_runs`
// 直接调它们，语义不动。
// ─────────────────────────────────────────────────────────────────────────────

/// 未结束（非终态）的会话。`run = None` 表示**不限 run**。
///
/// 含 `running`/`paused`/`recovering` 三种：`paused` 也「未结束」——02 §4 表里它
/// 只是保持暂停，不是终态。
pub fn unfinished_sessions(
    conn: &Connection,
    run: Option<&str>,
) -> Result<Vec<SessionRow>, AppError> {
    let sql = format!(
        "{SESSION_SELECT} WHERE (?1 IS NULL OR run_id <> ?1) AND state NOT IN ('finished','discarded') \
         ORDER BY started_at, id"
    );
    let mut stmt = conn.prepare(&sql).map_err(map_sqlite)?;
    let rows = stmt.query_map([run], read_session).map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

/// 未结束的会话，**排除当前 run**（门禁与启动扫描用的那一支）。
pub fn unfinished_sessions_of_other_runs(
    conn: &Connection,
    current_run_id: &str,
) -> Result<Vec<SessionRow>, AppError> {
    unfinished_sessions(conn, Some(current_run_id))
}

/// **当前 run** 的未结束会话（含 `recovering`）。
///
/// 显式退出用它找出要结束的会话；调用方按状态分流——`recovering` 必须保留
/// （02 §4：「recovering 记录保留不清」），所以这里**不**替调用方过滤。
pub fn unfinished_sessions_of_run(
    conn: &Connection,
    run_id: &str,
) -> Result<Vec<SessionRow>, AppError> {
    let sql = format!(
        "{SESSION_SELECT} WHERE run_id = ?1 AND state NOT IN ('finished','discarded') \
         ORDER BY started_at, id"
    );
    let mut stmt = conn.prepare(&sql).map_err(map_sqlite)?;
    let rows = stmt.query_map([run_id], read_session).map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

/// 未作废的**待确认区间**。`run = None` 表示**不限 run**（全局概览）。
pub fn pending_intervals(
    conn: &Connection,
    run: Option<&str>,
) -> Result<Vec<IntervalRow>, AppError> {
    let sql = format!(
        "{INTERVAL_SELECT_ALIASED} JOIN work_session s ON s.id = i.session_id \
         WHERE i.needs_review = 1 AND i.voided_at IS NULL AND (?1 IS NULL OR s.run_id <> ?1) \
         ORDER BY i.started_at, i.id"
    );
    let mut stmt = conn.prepare(&sql).map_err(map_sqlite)?;
    let rows = stmt.query_map([run], read_interval).map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

/// 未作废的待确认区间，且其会话不属于当前 run。
pub fn pending_intervals_of_other_runs(
    conn: &Connection,
    current_run_id: &str,
) -> Result<Vec<IntervalRow>, AppError> {
    pending_intervals(conn, Some(current_run_id))
}

/// 一条「事实已经不满足不变量」的会话（02 §4 的「状态/区间不变量损坏」类）。
///
/// `reason` 是**诊断**文本（会进报告与日志，不进用户文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantFault {
    pub session_id: String,
    pub reason: &'static str,
}

/// 不变量损坏的会话。`run = None` 表示**不限 run**（全局概览）。
/// （排除当前 run 的那一支见 [`invariant_faults_of_other_runs`]。）
///
/// 只查三条**无歧义**的运行时约束（都能被 SQL 直接判定，且 P2 的健康路径
/// 不会产生）：
/// 1. `running` 会话带未作废的待确认区间（02 §4：「running 必须无待确认区间」）；
/// 2. `running` 会话没有开放区间（02 §4 表：running 那行以「有开放区间」为前提）；
/// 3. 非 `running` 会话残留开放区间（02 §3：「paused 无 open interval」）。
///
/// **第 3 条有一条显式放行（P3 Task 1，C3 衍生）**：`recovering` 会话的**待确认**
/// 开放区间（`needs_review = 1`）是 P2 在采样失败分支留下的合法形态——08 §1
/// 「没有可信检查点则整个当前区间待确认」。它没有可信前缀，所以整段留在待确认
/// 状态、终点未知；把它判成损坏，等于把 P2 的正常输出说成自造损坏。
///
/// **刻意不查「recovering 必须有待确认区间」**：02 §4 原文允许「有待确认区间**或
/// 显式不变量故障标记**」，而 P2 的 `split_for_anomaly` 在候选终点正好落在检查点上时
/// 会留下一个没有余段的 `recovering` 会话——那是正常结果，不是损坏。
/// 把它误判成损坏会让门禁拒绝一批本来能恢复的数据；该分类归 P3 的四类判定。
pub fn invariant_faults(
    conn: &Connection,
    run: Option<&str>,
) -> Result<Vec<InvariantFault>, AppError> {
    // 三个分支用 UNION ALL 合成一条查询：每条都带 `(?1 IS NULL OR s.run_id <> ?1)`，
    // 不能让「当前 run 的会话」被算进上一代次的损坏里（也不能在全局概览里漏掉）。
    let sql = "\
        SELECT s.id, 'running_with_pending_interval' FROM work_session s \
          WHERE (?1 IS NULL OR s.run_id <> ?1) AND s.state = 'running' \
            AND EXISTS(SELECT 1 FROM work_interval i WHERE i.session_id = s.id \
                        AND i.needs_review = 1 AND i.voided_at IS NULL) \
        UNION ALL \
        SELECT s.id, 'running_without_open_interval' FROM work_session s \
          WHERE (?1 IS NULL OR s.run_id <> ?1) AND s.state = 'running' \
            AND NOT EXISTS(SELECT 1 FROM work_interval i WHERE i.session_id = s.id \
                            AND i.ended_at IS NULL AND i.voided_at IS NULL) \
        UNION ALL \
        SELECT s.id, 'open_interval_outside_running' FROM work_session s \
          WHERE (?1 IS NULL OR s.run_id <> ?1) AND s.state <> 'running' \
            AND EXISTS(SELECT 1 FROM work_interval i WHERE i.session_id = s.id \
                        AND i.ended_at IS NULL AND i.voided_at IS NULL \
                        AND NOT (s.state = 'recovering' AND i.needs_review = 1)) \
        ORDER BY 1, 2";

    let mut stmt = conn.prepare(sql).map_err(map_sqlite)?;
    let rows = stmt
        .query_map([run], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(map_sqlite)?;

    let mut faults = Vec::new();
    for row in rows {
        let (session_id, code) = row.map_err(map_sqlite)?;
        faults.push(InvariantFault {
            session_id,
            reason: fault_reason(&code)?,
        });
    }
    Ok(faults)
}

/// 不变量损坏的会话，**排除当前 run**（门禁与启动扫描用的那一支）。
pub fn invariant_faults_of_other_runs(
    conn: &Connection,
    current_run_id: &str,
) -> Result<Vec<InvariantFault>, AppError> {
    invariant_faults(conn, Some(current_run_id))
}

/// 诊断码 → 可读原因。未知码是编程错误（查询与映射表必须同步改）。
///
/// `pub(crate)`（P3 Task 1）：恢复扫描在「判据之间不一致」时按第 1 类降级，要复用
/// **同一个**诊断码的文案，不能自己复制一份字符串。
pub(crate) fn fault_reason(code: &str) -> Result<&'static str, AppError> {
    match code {
        "running_with_pending_interval" => Ok("running 会话带未作废的待确认区间"),
        "running_without_open_interval" => Ok("running 会话没有开放区间"),
        "open_interval_outside_running" => Ok("非 running 会话残留开放区间"),
        other => Err(AppError::Storage {
            detail: format!("unknown invariant fault code: {other}"),
        }),
    }
}
