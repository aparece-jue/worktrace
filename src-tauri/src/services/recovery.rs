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
//! # `time_edit` 的形状（取数据前先按 `reason` / `change` 过滤）
//!
//! 四种形状都进同一张表，所以「有没有审计行」回答不了任何业务问题：
//!
//! - `"change": "normalize_crashed_interval"`：`after_json` 带 `candidate_end` 与
//!   `candidate_end_source` ∈ {`last_checkpoint`, `interval_start`}；
//! - `"change": "rebind_run"`：只改会话归属，**没有**候选终点来源（不改区间事实）；
//! - `"change": "reconcile_confirm"`（`reason = "reconcile:confirm"`）：确认的端点是
//!   **用户给定的值**，不是推导出来的候选 ⇒ **不写** `candidate_end_source`；
//! - `"change": "reconcile_discard_uncertain"`（`reason = "reconcile:discard_uncertain"`）：
//!   只作废，同样不推导候选端点；
//! - `"change": "discard_session"`（`reason = "discard_session"`，P3 Task 4）：作废**整次**
//!   的全部区间，同样不推导候选端点——它不是「丢弃不确定区间」的另一个按钮，
//!   两者的语义与审计取值都不同（02 §3/§4）。
//!
//! 四种形状的 `before_json`/`after_json` 都带 `session` 与 `intervals`
//! （逐字段的区间前后值，含 `voided_at`），改动前的事实原样留在 `before_json` 里。
//!
//! # 本模块不做的事
//!
//! **不解门禁**——那是 S1 `AppState::rescan_recovery`（提交之后由 `AppState` 调）干的；
//! [`reconcile`] 与 [`discard_session`] 是**用户命令**：它们自己拥有事务、恰好加一次
//! `revision`，但 `AppState.recovery` 这份门禁快照不归它们改。也不重建协调器镜像：
//! 提交后的 `load_session` 同样在 `AppState` 里。
//!
//! [`attention_overview`] 是**只读**入口（同一读事务信封），不写任何事实。

use rusqlite::Transaction;

use crate::domain::error::DomainError;
use crate::domain::interval::{IntervalRange, IntervalSet};
use crate::domain::session::{SessionAttention, SessionState};
use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::guards::{guard_epoch, guard_row_version};
use crate::storage::meta::require_meta;
use crate::storage::session_repo::{
    self, fault_reason, IntervalRow, InvariantFault, SessionRow, SessionStateUpdate,
};
use crate::storage::time_edit_repo::{self, TimeEdit};
use crate::storage::WriteOutcome;

use super::audit::edit_json_base;
use super::history::HistoryEditReport;
use super::tx::{settle, settle_into, write_tx, SettledReport};

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
///
/// `Serialize`（P8 Task 2b）：它是 `attention_overview` 这条 IPC 命令响应里的列表项，
/// 直接装 [`AttentionOverview`] 交给前端，不在命令层复制一份字段做镜像 DTO——
/// 来由与 [`crate::storage::task_repo::TaskRow`] 逐字相同。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
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
///
/// `Serialize`（P8 Task 2b）：来由与 [`SessionAttentionItem`] 逐字相同。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
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

/// 全局待确认概览（R7；§0.5 钉死的第四个 DTO）。P8 的「恢复确认」入口与 P5 的
/// 排除口径都读它。
///
/// # 作用域（口径写在这里，`tests/reconcile.rs` 用测试钉住）
///
/// `items` 是**三者的并集**，每个会话只出现一次，按 `(started_at, id)` 升序：
///
/// 1. **不变量损坏**的会话（[`session_repo::invariant_faults`]，**不分状态、不分 run**）；
/// 2. **有未作废待确认区间**的会话（[`session_repo::pending_intervals`]，**不分状态、不分 run**）；
/// 3. **不属于当前 run 的未结束会话**（[`session_repo::unfinished_sessions`]，与门禁同口径）。
///
/// **为什么必须带上 1、2 两条终态分支**（Task 1 评审 m2）：门禁那三条查询都不筛会话
/// 状态——一个 `finished`/`discarded` 会话只要还挂着不变量损坏、或还挂着未作废的待确认
/// 区间（`ended_at` 已闭合的候选也算），门禁就一直是关的。若列表只遍历未结束会话，
/// 用户会看到「门禁关着，但列表里没有任何待处理项」。取并集之后
/// **门禁关着 ⇒ 列表非空**；反过来不成立（当前 run 自己的 `recovering` 会话也进列表，
/// 而它不是门禁材料）。
///
/// **正在计时的会话不进列表**：当前 run 的 `running` 会话在既没有损坏、又没有待确认
/// 区间时属于计时快照（`TimerSnapshot`），不属于恢复概览——两者在界面上是两块
/// （P7 计划的「恢复提示与时钟校正」第 1 条）。
///
/// `Serialize`（P8 Task 2b）：**它就是命令 8 `attention_overview` 的响应 DTO**
/// （`commands::attention_overview_impl` 原样返回它），所以不在命令层另造一份镜像
/// 字段表——与 `stats::TodayView` 同一条口径。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AttentionOverview {
    /// 需要用户处理或诊断的会话（口径见类型文档）。
    pub items: Vec<SessionAttentionItem>,
    /// 待确认区间总数（含终点未知的零长度候选）= `items` 里区间数之和。
    pub pending_intervals: usize,
    /// 有未作废待确认区间的会话数。
    pub pending_sessions: usize,
    /// 第 1 类（隔离）会话数。
    pub fault_sessions: usize,
    /// 这次读看到的库身份。
    pub data_epoch: String,
    /// 这次读看到的业务版本。读**不**改它。
    pub revision: i64,
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
// 对账（P3 Task 2，S2 的服务半部）：确认或丢弃不确定区间
// ─────────────────────────────────────────────────────────────────────────────

/// 对账动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileAction {
    /// 确认：给出**全部**待确认区间的新起止（缺一条、多一条都整条拒绝）。
    Confirm,
    /// 丢弃不确定区间：作废该会话**全部**待确认区间，保留此前的可信前缀。
    /// 不接受区间列表（一次一条会重新引入「作废哪一条」的歧义）；
    /// 它**不能**用来作废整次会话——那是 `discard_session`。
    DiscardUncertain,
}

/// 对账之后会话停在哪个状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileTargetState {
    Paused,
    Finished,
}

impl ReconcileTargetState {
    fn as_state(self) -> SessionState {
        match self {
            Self::Paused => SessionState::Paused,
            Self::Finished => SessionState::Finished,
        }
    }
}

/// 用户确认的一段区间。起止都是**用户给定的值**，不是候选端点推导出来的
/// （所以审计里没有 `candidate_end_source`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedRange {
    pub interval_id: String,
    pub started_at: i64,
    pub ended_at: i64,
}

/// 一次对账请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileRequest {
    pub session_id: String,
    pub action: ReconcileAction,
    pub target_state: ReconcileTargetState,
    /// `Confirm` 必填，且必须**恰好覆盖**该会话的全部待确认区间
    /// （待确认集合为空时必须是空列表）；`DiscardUncertain` 必须为空。
    pub ranges: Vec<ConfirmedRange>,
}

/// 一次对账的结果：会话 + 该会话**全部**区间（不只是被处理的那几条）。
///
/// `Serialize`（P8 Task 2a）：它就是 `reconcile` 这条 IPC 命令的响应 DTO
/// （`commands::reconcile_impl` 原样返回它，并把它当 `domain.changed` 的载荷），
/// 所以不在命令层另造一份镜像字段表——与 `stats::TodayView` 同一条口径。
/// 它也因此把 `SessionRow` / `IntervalRow` 带进了 IPC 契约。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReconcileReport {
    pub session: SessionRow,
    pub intervals: Vec<IntervalRow>,
    pub revision: i64,
    pub data_epoch: String,
}

impl SettledReport for ReconcileReport {
    fn revision_mut(&mut self) -> &mut i64 {
        &mut self.revision
    }

    fn data_epoch_mut(&mut self) -> &mut String {
        &mut self.data_epoch
    }
}

/// 用户命令：一次事务处理该会话的**全部**待确认区间。
///
/// 前置与判据（Task 2）：
/// - 会话必须存在、版本匹配（`env.expected_row_version`）；
/// - **命中第 1 类（不变量损坏）一律拒绝**：损坏不是「不确定」，确认一下就好的说法
///   会把损坏洗成事实，所以文案指向诊断；
/// - 只接 `recovering`（R6）：`paused`/`running` 想改事实要先 `finish` 再走 `correct`，
///   `paused` + 待确认那一类的出口是 `discard_session`（Task 4），不在这里放宽；
/// - `Confirm`：每条 `ended_at >= started_at`（零长度合法）、`ended_at <= now`
///   （未来的「已发生工时」不是事实）、`ranges` 两两不重叠（[`IntervalSet`]）、
///   且**逐条**与全部有效人工区间不重叠（S7，跨会话，端点相接不算）；
/// - `DiscardUncertain`：`ranges` 必须为空，作废集合由服务从库里取。
///
/// **写之前的全部校验都在写之前**：所以「整条命令拒绝」就是零变化——不会出现
/// 「前两条确认了、第三条被拒」。事务由本函数拥有，`settle` 保证恰好一次 `revision`。
///
/// 幂等：重复调用会先撞上「非 `recovering`」的前置（确认之后会话已经停在
/// `paused`/`finished`），所以 `Unchanged` 没有可达输入——本函数只产出 `Changed`。
/// 这是「不重复审计、不重复加版本」的实现方式：第二次请求被拒绝，而不是被当成又一笔写。
///
/// `current_run_id` 由调用方（`AppState::reconcile`）从协调器取：会话收尾要把
/// `run_id` 切到**本次 run**，而服务层够不着协调器；原始恢复归属保留在 `time_edit` 里。
pub fn reconcile(
    db: &mut Db,
    env: WriteEnvelope,
    req: ReconcileRequest,
    now: i64,
    current_run_id: &str,
) -> Result<WriteOutcome<ReconcileReport>, AppError> {
    let expected_version = env.expected_row_version.ok_or_else(|| AppError::Domain {
        detail: "缺少记录版本，无法安全地处理这条会话。".into(),
    })?;

    let tx = write_tx(db, &env)?;

    let session =
        session_repo::get_session(&tx, &req.session_id)?.ok_or(DomainError::UnknownSession)?;
    guard_row_version(session.row_version, expected_version)?;

    // 第 1 类：不变量损坏只诊断，不允许「确认一下就修好」。
    if session_repo::invariant_faults(&tx, None)?
        .iter()
        .any(|fault| fault.session_id == req.session_id)
    {
        return Err(AppError::Domain {
            detail: "这个会话的计时记录已损坏，无法通过确认修复，请查看诊断信息。".into(),
        });
    }

    if session.state != SessionState::Recovering {
        return Err(AppError::Domain {
            detail: "只有待确认状态的会话能确认或丢弃不确定区间；正在计时或已暂停的会话请先结束计时，或改用作废整次记录。".into(),
        });
    }

    let before_intervals = session_repo::intervals_of_session(&tx, &req.session_id)?;
    let pending: Vec<&IntervalRow> = before_intervals
        .iter()
        .filter(|interval| interval.needs_review && interval.voided_at.is_none())
        .collect();

    match req.action {
        ReconcileAction::Confirm => validate_confirm(&tx, &pending, &req.ranges, now)?,
        ReconcileAction::DiscardUncertain => {
            if !req.ranges.is_empty() {
                return Err(AppError::Domain {
                    detail: "丢弃不确定区间时由服务取出全部待确认区间，不能指定区间列表。".into(),
                });
            }
        }
    }

    match req.action {
        ReconcileAction::Confirm => {
            for range in &req.ranges {
                session_repo::confirm_interval(
                    &tx,
                    &range.interval_id,
                    range.started_at,
                    range.ended_at,
                    // 零长度合法：`duration_ms = 0`（半开区间的空集）。
                    range.ended_at - range.started_at,
                )?;
            }
        }
        ReconcileAction::DiscardUncertain => {
            for interval in &pending {
                session_repo::void_interval(&tx, &interval.id, now)?;
            }
        }
    }

    // 会话收尾。`ended_at` 只在 `Finished` 时写：`Paused` 传 `None`，
    // `update_session_state` 的 `COALESCE` 保持原值。
    let after_intervals = session_repo::intervals_of_session(&tx, &req.session_id)?;
    let ended_at = match req.target_state {
        ReconcileTargetState::Finished => Some(finished_end(&after_intervals, session.started_at)),
        ReconcileTargetState::Paused => None,
    };
    let updated = session_repo::update_session_state(
        &tx,
        &req.session_id,
        expected_version,
        req.target_state.as_state(),
        SessionStateUpdate {
            ended_at,
            // 02 §4/§10：恢复归属切到本次 run，原始归属留在下面的审计里。
            run_id: Some(current_run_id),
            needs_review: Some(false),
        },
    )?;

    record_edit(
        &tx,
        &ScanEdit {
            change: match req.action {
                ReconcileAction::Confirm => "reconcile_confirm",
                ReconcileAction::DiscardUncertain => "reconcile_discard_uncertain",
            },
            before: &session,
            after: &updated,
            before_intervals: &before_intervals,
            after_intervals: &after_intervals,
            // Ruling 8：端点由用户给定，没有「候选推导」，所以没有 candidate_* 键。
            candidate: None,
            reason: match req.action {
                ReconcileAction::Confirm => "reconcile:confirm",
                ReconcileAction::DiscardUncertain => "reconcile:discard_uncertain",
            },
        },
        now,
    )?;

    // `Changed` 是这一条命令唯一的取值（前置已经排除了「什么都不用改」的输入），
    // 但写的形状仍然走 `settle`：它负责「恰好一次 revision」与同事务读回版本；
    // 版本位由 `settle_into`（R10 的唯一填法）填回报告。
    let outcome = settle_into(settle(
        &tx,
        WriteOutcome::Changed(ReconcileReport {
            session: updated,
            intervals: after_intervals,
            // 下面由 `settle` 的结果填权威值。
            revision: 0,
            data_epoch: String::new(),
        }),
    )?);
    tx.commit().map_err(map_sqlite)?;
    Ok(outcome)
}

/// `Confirm` 的逐条校验（全部通过之后调用方才开始写）。
fn validate_confirm(
    tx: &Transaction<'_>,
    pending: &[&IntervalRow],
    ranges: &[ConfirmedRange],
    now: i64,
) -> Result<(), AppError> {
    // 逐一对应：缺一条、多一条、外来会话的区间、重复 id、指向非待确认行，全都
    // 会在这一步失配 ⇒ 整条命令拒绝（零变化）。
    let mut expected: Vec<&str> = pending
        .iter()
        .map(|interval| interval.id.as_str())
        .collect();
    let mut given: Vec<&str> = ranges
        .iter()
        .map(|range| range.interval_id.as_str())
        .collect();
    expected.sort_unstable();
    given.sort_unstable();
    if expected != given {
        return Err(AppError::Domain {
            detail: "确认的区间必须与该会话的全部待确认区间一一对应，请刷新后重试。".into(),
        });
    }

    let mut confirmed = IntervalSet::new();
    for range in ranges {
        let interval = IntervalRange::new(range.started_at, range.ended_at)?;
        if range.ended_at > now {
            return Err(AppError::Domain {
                detail: "确认的结束时刻不能晚于当前时间。".into(),
            });
        }
        // 两两不许重叠（半开：端点相接允许），命中即 `OverlappingInterval`。
        confirmed.insert(interval)?;
        // 与全部有效人工区间不重叠（跨会话），端点相接不算重叠。
        session_repo::require_no_human_overlap(tx, interval.start, interval.end, None)?;
    }
    Ok(())
}

/// `target_state = Finished` 时会话的结束时刻，**一条口径**：
/// `COALESCE((SELECT MAX(ended_at) FROM work_interval WHERE session_id = ? AND voided_at IS NULL
/// AND ended_at IS NOT NULL), session.started_at)`。
///
/// `voided_at IS NULL` 排除刚被作废的段、`ended_at IS NOT NULL` 排除仍无终点的段、
/// 兜底到会话起点保证 `ck_session_range`（一条可用区间都没有的会话也要能结束）。
fn finished_end(intervals: &[IntervalRow], session_started_at: i64) -> i64 {
    intervals
        .iter()
        .filter(|interval| interval.voided_at.is_none())
        .filter_map(|interval| interval.ended_at)
        .max()
        .unwrap_or(session_started_at)
}

// ─────────────────────────────────────────────────────────────────────────────
// 作废整次（P3 Task 4）：`discard_session`
// ─────────────────────────────────────────────────────────────────────────────

/// 一次「作废整次」的请求。`env` 的版本位是**会话**版本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardSessionRequest {
    pub session_id: String,
}

/// 用户命令：作废整次会话——**全部区间**软作废 + 会话 `discarded`。
///
/// # 为什么它不走 `end_session_in_tx`
///
/// 那条原语按 `run_id` 拒跨 run（`timer/primitives.rs`），而「作废整次」必须能作用于
/// **旧 run 留下的会话**——那正是恢复材料。所以这里只做 S8 的区间作废加会话状态更新：
/// 因而不受 `StaleRunContext` 限制，**也不参与「以可信方式闭合」**——它不会把停机时间
/// 算成工时（未知终点的段作废后仍是 `NULL`）。
///
/// # 无状态前置（Ruling 6）
///
/// `running` / `paused` / `recovering` / `finished` 一视同仁：`paused` 却仍挂着待确认
/// 区间这一类四类判定盖不住的形态，出口就是这条命令。已 `discarded` 且区间都已作废时
/// 是**幂等**的：零写入、零审计、零版本（不移动第一次作废时记下的 `ended_at`）。
///
/// # 与 `reconcile(DiscardUncertain)` 的区别（02 §3/§4：不许含糊共用一个丢弃按钮）
///
/// - `reconcile` 只作废**待确认**区间，可信前缀原样保留，且只接 `recovering`；
/// - `discard_session` 作废**全部**区间（含可信前缀与运行中的开放区间）并把会话推成
///   `discarded`，**不隐式改变任务状态**、不删除任何审计行。
///
/// 两者的 `time_edit.reason`（`reconcile:discard_uncertain` / `discard_session`）与
/// 规则文案都不同，所以在错误与审计上可分辨。
///
/// `ended_at` 记 `max(now, session.started_at)`（`ck_session_range` 的下界兜底）；
/// `run_id` **不动**（终态会话不再参与门禁与联动）；`needs_review` 清假。
/// 端点是用户给定的整段事实，没有候选推导 ⇒ **不写** `candidate_*`（Ruling 8）。
pub fn discard_session(
    db: &mut Db,
    env: WriteEnvelope,
    req: DiscardSessionRequest,
    now: i64,
) -> Result<WriteOutcome<HistoryEditReport>, AppError> {
    let expected_version = env.expected_row_version.ok_or_else(|| AppError::Domain {
        detail: "缺少记录版本，无法安全地作废这次会话。".into(),
    })?;

    let tx = write_tx(db, &env)?;

    let session =
        session_repo::get_session(&tx, &req.session_id)?.ok_or(DomainError::UnknownSession)?;
    guard_row_version(session.row_version, expected_version)?;

    let before_intervals = session_repo::intervals_of_session(&tx, &req.session_id)?;
    // 报告要答「作废了哪一段」，所以一条区间都没有的会话只能显式拒绝
    // （`HistoryEditReport.interval` 是必填的）；这也是防御分支，正常路径造不出来。
    let first = before_intervals
        .first()
        .cloned()
        .ok_or_else(|| AppError::Domain {
            detail: "这条会话没有任何计时区间，无法作废。".into(),
        })?;

    // 幂等：已经到了「整次作废」的终态，没有任何字段要改。
    // 已经 `discarded` 的会话在下面也不会再移动 `ended_at`——作废时刻是**第一次**
    // 作废的时刻（与 `void_interval` 不覆盖 `voided_at`、`mark_clean_exit` 不覆盖
    // 第一次退出时刻同一口径），否则重复提交会把一个终态会话的终点一直往后推。
    let all_voided = before_intervals
        .iter()
        .all(|interval| interval.voided_at.is_some());
    if all_voided && session.state == SessionState::Discarded && !session.needs_review {
        let outcome = settle_into(settle(
            &tx,
            WriteOutcome::Unchanged(report(session.clone(), first)),
        )?);
        tx.commit().map_err(map_sqlite)?;
        return Ok(outcome);
    }

    // 全部区间软作废（含运行中的开放区间与已归一的候选段）。
    // `void_interval` 的已知/未知时长规则见 S8：有 `duration_ms` 的保留端点，
    // 没有的把 `ended_at` 清回 `NULL`——不给未确认的候选补 0 时长冒充事实。
    let mut first_voided = None;
    for interval in &before_intervals {
        let voided = session_repo::void_interval(&tx, &interval.id, now)?;
        if first_voided.is_none() {
            first_voided = Some(voided);
        }
    }
    let reported = first_voided.unwrap_or(first);

    let ended_at = if session.state == SessionState::Discarded {
        // 已经作废过：不动它第一次记下的终点。
        None
    } else {
        Some(now.max(session.started_at))
    };
    let updated = session_repo::update_session_state(
        &tx,
        &req.session_id,
        expected_version,
        SessionState::Discarded,
        SessionStateUpdate {
            ended_at,
            // `run_id` 不动：终态会话不再参与门禁与联动。
            run_id: None,
            needs_review: Some(false),
        },
    )?;

    let after_intervals = session_repo::intervals_of_session(&tx, &req.session_id)?;
    record_edit(
        &tx,
        &ScanEdit {
            change: "discard_session",
            before: &session,
            after: &updated,
            before_intervals: &before_intervals,
            after_intervals: &after_intervals,
            // Ruling 8：整段事实由用户给定，没有候选端点推导。
            candidate: None,
            reason: "discard_session",
        },
        now,
    )?;

    let outcome = settle_into(settle(
        &tx,
        WriteOutcome::Changed(HistoryEditReport {
            session: updated,
            interval: reported,
            // 下面由 `settle` 的结果填权威值。
            revision: 0,
            data_epoch: String::new(),
        }),
    )?);
    tx.commit().map_err(map_sqlite)?;
    Ok(outcome)
}

/// 报告骨架（`revision`/`data_epoch` 由 `settle` 的结果填）。
fn report(session: SessionRow, interval: IntervalRow) -> HistoryEditReport {
    HistoryEditReport {
        session,
        interval,
        revision: 0,
        data_epoch: String::new(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 全局待确认概览（R7，只读）
// ─────────────────────────────────────────────────────────────────────────────

/// 全局待确认概览：形状与作用域见 [`AttentionOverview`]。
///
/// 纯读：不开写事务、不加 `revision`。为了「数据与元数据出自同一读事务」，这里显式
/// 开一个只读事务，`guard_epoch` 在事务内跑（形状抄 `catalog::list_projects`）。
///
/// `current_run_id` 由调用方从协调器取（`AppState::coordinator().run_id()`）：
/// `is_current_run` 要用它分组，而服务层够不着协调器。
pub fn attention_overview(
    db: &Db,
    expected_data_epoch: &str,
    current_run_id: &str,
) -> Result<AttentionOverview, AppError> {
    let tx = db
        .connection()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    guard_epoch(&tx, expected_data_epoch)?;

    // 一份谓词两个入口（S9）：`None` = 不限 run（故障与待确认是全局事实），
    // `Some(当前 run)` = 排除当前 run（未结束会话与门禁同口径）。
    let faults = session_repo::invariant_faults(&tx, None)?;
    let pending = session_repo::pending_intervals(&tx, None)?;
    let unfinished = session_repo::unfinished_sessions(&tx, Some(current_run_id))?;

    let mut candidates: Vec<SessionRow> = Vec::new();
    for session in unfinished {
        push_unique(&mut candidates, session);
    }
    for id in faults
        .iter()
        .map(|fault| fault.session_id.as_str())
        .chain(pending.iter().map(|interval| interval.session_id.as_str()))
    {
        if candidates.iter().any(|known| known.id == id) {
            continue;
        }
        // 只有真读到了行才建条目：故障/待确认查询带 JOIN，理论上可能出现孤儿区间
        // （`work_interval.session_id` 有外键，正常路径不会有）。
        if let Some(session) = session_repo::get_session(&tx, id)? {
            push_unique(&mut candidates, session);
        }
    }
    candidates.sort_by(|a, b| (a.started_at, &a.id).cmp(&(b.started_at, &b.id)));

    let items: Vec<SessionAttentionItem> = candidates
        .iter()
        .map(|session| {
            let fault_reason = faults
                .iter()
                .find(|fault| fault.session_id == session.id)
                .map(|fault| fault.reason.to_string());
            let attention = if fault_reason.is_some() {
                SessionAttention::InvariantBroken
            } else {
                // 未结束的别的 run 会话（例如没有余段的 `recovering`）也要有人处理，
                // 所以与「有待确认区间」共用同一个标记，不用 `None`（那是「正常」）。
                SessionAttention::NeedsReview
            };
            let intervals = pending
                .iter()
                .filter(|interval| interval.session_id == session.id)
                .map(pending_item)
                .collect();
            item(session, current_run_id, attention, fault_reason, intervals)
        })
        .collect();

    let meta = require_meta(&tx)?;
    // 读事务什么都没写：直接结束它（回滚一个只读事务不改变任何事实）。
    drop(tx);

    Ok(AttentionOverview {
        pending_intervals: pending.len(),
        pending_sessions: items
            .iter()
            .filter(|item| !item.intervals.is_empty())
            .count(),
        fault_sessions: items
            .iter()
            .filter(|item| item.fault_reason.is_some())
            .count(),
        items,
        data_epoch: meta.data_epoch,
        revision: meta.revision,
    })
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

/// 公共骨架（[`edit_json_base`]）+ 本模块自己的可选键：有候选端点推导时才有
/// `candidate_end` / `candidate_end_source`（Ruling 8）——扫描归一那条路没有候选，
/// 两个键就不出现。
fn edit_json(
    change: &str,
    session: &SessionRow,
    intervals: &[IntervalRow],
    candidate: Option<(i64, &'static str)>,
) -> String {
    let mut json = edit_json_base(change, session, intervals);
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
        .iter()
        .filter(|interval| interval.needs_review && interval.voided_at.is_none())
        .map(pending_item)
        .collect())
}

/// 一行区间 → 展示口径的待确认条目（扫描与全局概览共用同一份字段映射）。
fn pending_item(interval: &IntervalRow) -> PendingIntervalItem {
    PendingIntervalItem {
        id: interval.id.clone(),
        started_at: interval.started_at,
        ended_at: interval.ended_at,
        duration_ms: interval.duration_ms,
        sampled_end_wall_at: interval.sampled_end_wall_at,
        needs_review: interval.needs_review,
    }
}

/// 往候选集合里加一条会话，按 id 去重（三个来源的并集里同一条只能出现一次）。
fn push_unique(candidates: &mut Vec<SessionRow>, row: SessionRow) {
    if !candidates.iter().any(|known| known.id == row.id) {
        candidates.push(row);
    }
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
