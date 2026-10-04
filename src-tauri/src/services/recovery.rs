//! 启动恢复扫描（P3 Task 1）：把上一个 run 留下的事实判成四类，并且**只对能安全
//! 归一的那一类动手**。
//!
//! # 判定顺序不可颠倒（02 §4 表）
//!
//! 先看事实是否还满足不变量，再看有无开放/待确认区间，最后看当前状态：
//!
//! 1. **状态/区间不变量损坏** → 只诊断、隔离，**禁止自动修复事实**：不写任何行、
//!    不加版本、不写审计。判据就是 [`session_repo::invariant_faults`] 的三条谓词
//!    （`Some(current_run_id)`，即「别的 run 的」）。
//! 2. **`running` 且有开放区间**（只可能是别的 run）→ S5
//!    [`session_repo::normalize_crashed_open_interval`]：可信前缀闭合 + 终点未知的
//!    待确认段，会话转 `recovering`（`run_id` 不变，02 §10）。
//! 3. **`recovering`** → 原样保持（含 P2 留下的 `ended_at IS NULL` 待确认段），
//!    不增加已知工时、不写任何行。
//! 4. **`paused` 且无开放/待确认区间** → 保持 `paused`，`run_id` 重绑当前 run
//!    （此后 `finish`/`resume` 不会再撞 `StaleRunContext`），**不自动继续计时**。
//!
//! 四类之外还有一类**只可能手工造出来**的事实：`paused` 却仍挂着待确认区间。
//! 它既不是损坏（三条谓词都不命中），也不能按第 4 类重绑——重绑会造出一个
//! 「可以被结束」却仍带待确认事实的会话，而 `end_session_in_tx` 照样会拒。
//! 所以与第 3 类同口径：**原样保持**、归 `NeedsReview`、等 `reconcile`（Task 2）。
//!
//! # 两类事实分开记录
//!
//! 第 1 类只进 [`StartupScanReport::faults`]（`SessionAttention::InvariantBroken`），
//! 不能用 `reconcile` 混过去（Task 2 会再拒一次）；第 2/3 类归 `NeedsReview`，
//! 第 4 类 `None`。`attention` 为**每个被分类的会话**给出一项，四类都在里面。
//!
//! # 版本与审计口径（02 文末「启动扫描的版本与审计补充」）
//!
//! 扫描查询本身不加 revision；**同一批事务里若实际改变了**会话状态、`run_id` 或
//! 区间事实 ⇒ 每个被改的会话 `row_version` 恰好 +1、整个扫描事务 `revision` 恰好
//! +1、每个被改的会话写一条 `time_edit`；**没有字段变化就不写审计、不加任何版本**。
//! 落点是 [`settle`]（`Changed` 才 `bump_revision` 一次 + 同事务读回版本），
//! 不在这里自己再写一次 `bump_revision`。
//!
//! 系统事务**不开 epoch 守卫**（没有客户端请求），照 `AppState::explicit_exit`
//! 里开事务那段的写法。
//!
//! # `time_edit` 的两种形状（取数据前先按 `change` 过滤）
//!
//! 与 `task_change` 的三种形状同一约定：两种都进同一张表，所以「有没有审计行」
//! 回答不了任何业务问题。
//!
//! - `"change": "normalize_crashed_interval"`：`after_json` 带 `candidate_end` 与
//!   `candidate_end_source` ∈ {`last_checkpoint`, `interval_start`}；
//! - `"change": "rebind_run"`：只改会话归属，**没有**候选终点来源（不改区间事实）。
//!
//! 两种形状的 `before_json`/`after_json` 都带 `session` 与 `intervals`
//! （逐字段的区间前后值），改动前的开放事实原样留在 `before_json` 里。
//!
//! # 本模块不做的事
//!
//! 不解门禁——那是 S1 `AppState::rescan_recovery`（Task 2/Task 4）在 `reconcile` /
//! `discard_session` 提交后干的；也不重建协调器镜像：启动路径上扫描发生在协调器
//! 创建之前（`live` 还是 `None`），旧协调器整体丢弃，扫描**不去修一个还活着的
//! `live`**。

use rusqlite::Transaction;

use crate::domain::error::DomainError;
use crate::domain::session::{SessionAttention, SessionState};
use crate::error::AppError;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::session_repo::{
    self, fault_reason, IntervalRow, InvariantFault, SessionRow, SessionStateUpdate,
};
use crate::storage::time_edit_repo::{self, TimeEdit};
use crate::storage::WriteOutcome;

use super::tx::settle;

// ─────────────────────────────────────────────────────────────────────────────
// 扫描结论（§0.5 钉死的 DTO 形状；P8 的 IPC 包装与快照 fixture 照抄）
// ─────────────────────────────────────────────────────────────────────────────

/// 一次启动扫描的结论。四类名单各自独立，另有整批的版本位。
///
/// `attention` 是给用户看的那一份（P8 的「恢复确认」入口与 `attention_overview`
/// 都从它派生）：四类会话各一项，不按类别拆表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupScanReport {
    /// 第 2 类：被 S5 归一过的会话（可信前缀 + 终点未知的待确认段）。
    pub normalized_sessions: Vec<String>,
    /// 第 4 类：`run_id` 被重绑到当前 run 的会话。
    pub rebound_sessions: Vec<String>,
    /// 第 3 类：原样保持的 `recovering` 会话。
    pub recovering_kept: Vec<String>,
    /// 第 1 类：只诊断的 `InvariantFault`（含防御分支降级进来的那条）。
    pub faults: Vec<InvariantFault>,
    /// 每个被分类会话的处置与待确认区间。
    pub attention: Vec<SessionAttentionItem>,
    /// 这一批是否真的改了库（`true` ⇔ 本次扫描 `revision` 恰好 +1）。
    pub revision_changed: bool,
}

/// 一个需要用户（或诊断）看一眼的会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAttentionItem {
    pub session_id: String,
    pub task_id: String,
    pub state: SessionState,
    /// 扫描**之后**的归属：第 4 类已经是当前 run。
    pub run_id: String,
    /// `run_id` 是否等于本次 `application_run`（P8 据此分组）。
    pub is_current_run: bool,
    pub attention: SessionAttention,
    /// 该会话的待确认区间（含终点未知的零长度候选）。
    pub intervals: Vec<PendingIntervalItem>,
    /// 第 1 类的诊断原因（`InvariantFault.reason` 照抄，**不进用户文案**）。
    pub fault_reason: Option<String>,
    /// 扫描之后的会话版本，供 `reconcile`/`discard` 的 `expected_row_version`。
    pub session_row_version: i64,
    /// 会话级待确认标记（`reconcile` 的清理对象）。
    pub session_needs_review: bool,
}

/// 一条待确认区间。
///
/// 字段与 [`IntervalRow`] 同源、同一次读事务，但按展示口径裁过：不带 `session_id`
/// （父项里已有）与 `voided_at`（待确认集合按定义 `voided_at IS NULL`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingIntervalItem {
    pub id: String,
    pub started_at: i64,
    /// **候选端点**（不是已确认事实）：S5 归一时等于可信前缀的界、
    /// P2 分割时等于候选结束、终点未知时为 `None`。
    pub ended_at: Option<i64>,
    /// `None` = 未确认（终点未知或还没被用户确认）⇒ **不得**当已确认时长用。
    pub duration_ms: Option<i64>,
    pub sampled_end_wall_at: Option<i64>,
    /// 恒为 `true`（这个列表就是待确认集合）。
    pub needs_review: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// 扫描
// ─────────────────────────────────────────────────────────────────────────────

/// 启动第 ④ 步：一个系统事务里做完四类判定与归一，返回结论。
///
/// `now` 是启动第 ③ 步那次采样的 `sample.wall_ms`，**只用于审计行的 `created_at`**
/// ——它**不是**区间终点（C3：旧 run 的区间终点只能来自它自己的检查点）。
pub fn scan_at_startup(
    db: &mut Db,
    current_run_id: &str,
    now: i64,
) -> Result<StartupScanReport, AppError> {
    let tx = db
        .connection_mut()
        .unchecked_transaction()
        .map_err(map_sqlite)?;

    // 判定顺序第一步：先看不变量是否可信。损坏的会话后面一律只诊断。
    let mut faults = session_repo::invariant_faults(&tx, Some(current_run_id))?;
    let unfinished = session_repo::unfinished_sessions(&tx, Some(current_run_id))?;

    let mut normalized_sessions = Vec::new();
    let mut rebound_sessions = Vec::new();
    let mut recovering_kept = Vec::new();
    let mut attention = Vec::new();
    let mut changed = false;

    for session in &unfinished {
        // 第 1 类：事实已经不满足不变量 → 隔离，一个字都不写。
        if let Some(fault) = faults.iter().find(|f| f.session_id == session.id) {
            attention.push(item(
                session,
                current_run_id,
                SessionAttention::InvariantBroken,
                Some(fault.reason.to_string()),
                pending_items(&tx, &session.id)?,
            ));
            continue;
        }

        match session.state {
            SessionState::Running => {
                let before = session_repo::intervals_of_session(&tx, &session.id)?;
                match session_repo::normalize_crashed_open_interval(&tx, &session.id) {
                    Ok(split) => {
                        // `run_id` 保持不变：recovering 保持原恢复归属，直到
                        // `reconcile` 更新并审计（02 §10）。
                        let updated = session_repo::update_session_state(
                            &tx,
                            &session.id,
                            session.row_version,
                            SessionState::Recovering,
                            SessionStateUpdate {
                                needs_review: Some(true),
                                ..Default::default()
                            },
                        )?;
                        // 归一后原区间已被改写，重新读一遍才是 `after_json` 的事实。
                        let after = session_repo::intervals_of_session(&tx, &updated.id)?;
                        let source = if split.trusted_interval_id.is_some() {
                            "last_checkpoint"
                        } else {
                            "interval_start"
                        };
                        record_edit(
                            &tx,
                            &ScanEdit {
                                change: "normalize_crashed_interval",
                                before: session,
                                after: &updated,
                                before_intervals: &before,
                                after_intervals: &after,
                                candidate: Some((split.candidate_end, source)),
                                reason: "crashed open interval normalized",
                            },
                            now,
                        )?;
                        normalized_sessions.push(session.id.clone());
                        changed = true;
                        attention.push(item(
                            &updated,
                            current_run_id,
                            SessionAttention::NeedsReview,
                            None,
                            pending_items(&tx, &updated.id)?,
                        ));
                    }
                    // 防御分支（R4）：判据之间不一致——`running` 却没有开放区间。
                    // 按第 1 类降级，并**继续处理后面的行**（不 abort 整批）。
                    Err(err) if is_no_open_interval(&err) => {
                        let reason = fault_reason("running_without_open_interval")?;
                        faults.push(InvariantFault {
                            session_id: session.id.clone(),
                            reason,
                        });
                        attention.push(item(
                            session,
                            current_run_id,
                            SessionAttention::InvariantBroken,
                            Some(reason.to_string()),
                            Vec::new(),
                        ));
                    }
                    Err(err) => return Err(err),
                }
            }
            SessionState::Recovering => {
                // 第 3 类：原样保持。P2 留下的「终点未知」开放段（`ended_at IS NULL`
                // 且 `needs_review = 1`）是 08 §1 的合法形态，不是损坏。
                recovering_kept.push(session.id.clone());
                attention.push(item(
                    session,
                    current_run_id,
                    SessionAttention::NeedsReview,
                    None,
                    pending_items(&tx, &session.id)?,
                ));
            }
            SessionState::Paused => {
                let pending = pending_items(&tx, &session.id)?;
                if !pending.is_empty() {
                    // 四类之外（手工事实）：有待确认区间就不能重绑——重绑会让它
                    // 变成一个「可以结束」却仍带待确认事实的会话，而结束原语会拒。
                    attention.push(item(
                        session,
                        current_run_id,
                        SessionAttention::NeedsReview,
                        None,
                        pending,
                    ));
                    continue;
                }

                // 第 4 类：保持 paused，`run_id` 重绑当前 run，**不自动继续计时**。
                let before = session_repo::intervals_of_session(&tx, &session.id)?;
                let updated = session_repo::update_session_state(
                    &tx,
                    &session.id,
                    session.row_version,
                    SessionState::Paused,
                    SessionStateUpdate {
                        run_id: Some(current_run_id),
                        ..Default::default()
                    },
                )?;
                let after = session_repo::intervals_of_session(&tx, &updated.id)?;
                record_edit(
                    &tx,
                    &ScanEdit {
                        change: "rebind_run",
                        before: session,
                        after: &updated,
                        before_intervals: &before,
                        after_intervals: &after,
                        candidate: None,
                        reason: "paused session rebound to this run",
                    },
                    now,
                )?;
                rebound_sessions.push(session.id.clone());
                changed = true;
                attention.push(item(
                    &updated,
                    current_run_id,
                    SessionAttention::None,
                    None,
                    Vec::new(),
                ));
            }
            // `unfinished_sessions` 已排掉终态，这里不会有别的取值。
            SessionState::Finished | SessionState::Discarded => {}
        }
    }

    let report = StartupScanReport {
        normalized_sessions,
        rebound_sessions,
        recovering_kept,
        faults,
        attention,
        revision_changed: false,
    };
    let outcome = if changed {
        WriteOutcome::Changed(report)
    } else {
        WriteOutcome::Unchanged(report)
    };
    let (mut report, revision_changed) = match settle(&tx, outcome)? {
        WriteOutcome::Changed((report, _)) => (report, true),
        WriteOutcome::Unchanged((report, _)) => (report, false),
    };
    report.revision_changed = revision_changed;
    tx.commit().map_err(map_sqlite)?;
    Ok(report)
}

// ─────────────────────────────────────────────────────────────────────────────
// 内部
// ─────────────────────────────────────────────────────────────────────────────

/// 一条扫描审计（与它描述的改动**同事务**）。
///
/// 用结构体而不是一长串参数：调用点写字段名，`change` 与 `candidate` 的取值集合
/// 一眼可查（形状见模块头）。
struct ScanEdit<'a> {
    change: &'static str,
    before: &'a SessionRow,
    after: &'a SessionRow,
    before_intervals: &'a [IntervalRow],
    after_intervals: &'a [IntervalRow],
    /// `(候选终点, 来源)`；重绑不改区间事实时为 `None`（`after_json` 里就没有
    /// `candidate_end_source` 这一对键，不留 `null` 冒充取值）。
    candidate: Option<(i64, &'static str)>,
    reason: &'static str,
}

fn record_edit(tx: &Transaction<'_>, edit: &ScanEdit<'_>, now: i64) -> Result<(), AppError> {
    time_edit_repo::write(
        tx,
        &TimeEdit {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: edit.after.id.clone(),
            before_json: edit_json(edit.change, edit.before, edit.before_intervals, None),
            after_json: edit_json(
                edit.change,
                edit.after,
                edit.after_intervals,
                edit.candidate,
            ),
            reason: Some(edit.reason.to_string()),
            created_at: now,
        },
    )
}

fn edit_json(
    change: &str,
    session: &SessionRow,
    intervals: &[IntervalRow],
    candidate: Option<(i64, &'static str)>,
) -> String {
    let mut json = serde_json::json!({
        "change": change,
        "session": {
            "id": session.id,
            "state": session.state.as_str(),
            "run_id": session.run_id,
            "needs_review": session.needs_review,
            "row_version": session.row_version,
        },
        "intervals": intervals.iter().map(|i| serde_json::json!({
            "id": i.id,
            "started_at": i.started_at,
            "ended_at": i.ended_at,
            "duration_ms": i.duration_ms,
            "sampled_end_wall_at": i.sampled_end_wall_at,
            "needs_review": i.needs_review,
        })).collect::<Vec<_>>(),
    });
    if let Some((end, source)) = candidate {
        json["candidate_end"] = serde_json::json!(end);
        json["candidate_end_source"] = serde_json::json!(source);
    }
    json.to_string()
}

/// 待确认区间（`needs_review = 1` 且未作废），按读取顺序（`started_at, id`）。
fn pending_items(
    tx: &Transaction<'_>,
    session_id: &str,
) -> Result<Vec<PendingIntervalItem>, AppError> {
    Ok(session_repo::intervals_of_session(tx, session_id)?
        .into_iter()
        .filter(|interval| interval.needs_review && interval.voided_at.is_none())
        .map(|interval| PendingIntervalItem {
            id: interval.id,
            started_at: interval.started_at,
            ended_at: interval.ended_at,
            duration_ms: interval.duration_ms,
            sampled_end_wall_at: interval.sampled_end_wall_at,
            needs_review: interval.needs_review,
        })
        .collect())
}

fn item(
    session: &SessionRow,
    current_run_id: &str,
    attention: SessionAttention,
    fault_reason: Option<String>,
    intervals: Vec<PendingIntervalItem>,
) -> SessionAttentionItem {
    SessionAttentionItem {
        session_id: session.id.clone(),
        task_id: session.task_id.clone(),
        state: session.state,
        run_id: session.run_id.clone(),
        is_current_run: session.run_id == current_run_id,
        attention,
        intervals,
        fault_reason,
        session_row_version: session.row_version,
        session_needs_review: session.needs_review,
    }
}

/// S5 在「判据之间不一致」（`running` 却没有开放区间）时回
/// [`DomainError::NoOpenInterval`]。用**同一个枚举**渲染出的文案比对，不复制字符串
/// 字面量——文案只有一个来源。
fn is_no_open_interval(err: &AppError) -> bool {
    matches!(err, AppError::Domain { detail } if *detail == DomainError::NoOpenInterval.to_string())
}
