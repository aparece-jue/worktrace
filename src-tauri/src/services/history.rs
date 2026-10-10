//! 已完成历史的时间修正（P3 Task 3）：`correct`，与手工补录（P3 Task 4）：`backfill`。
//!
//! P8 Task 2b 在同一文件里补上历史的**读**半边：[`history_view`]（分页会话 + 可选详情）。
//! 它是**只读**服务——不采样、不写库、不推进 `revision`、不写审计，与上面两条写命令
//! 共用同一批仓储读函数（[`session_repo::history_sessions`] 是常规历史唯一的取数入口）。
//!
//! # `backfill`：把**已经发生**的人工时间补录成一条终态会话
//!
//! 补录不是「补一次计时」：它直接建一条 `finished` 会话与一条**可信闭合**区间
//! （[`session_repo::create_finished_session`]，S6）——**不启动计时、不占前台槽位**
//! （`uq_running_foreground` 全程不受影响）、**不伪造完成事件**（不写 `task_change`，
//! 02 §3 原文）、不冻结估时基准（那是 `start` 的事）、不写 `interval_checkpoint`。
//!
//! 校验（写之前全部做完）：任务存在 · 范围非负（[`IntervalRange::new`]）·
//! `ended_at <= now`（未来的「已发生工时」不是事实）· 与**全部**有效人工时间不重叠
//! （S7，跨会话、端点相接不算、正在计时的区间算重叠、机器模式不参与互斥）。
//! 成功必然 `Changed`：它新建了一条会话与一条区间。
//!
//! 审计是**创建型**的：`before_json` 只有 `change` 键（补录前没有这条会话，
//! 与 `create_task` 把 `before_json` 写成 `"{}"` 同一口径），`after_json` 是下面那份
//! 共享形状。`reason = backfill`，端点是用户给定的值 ⇒ **不写** `candidate_*`（Ruling 8）。
//!
//! # `correct`：只修「可信历史」
//!
//! 只接 `finished` 会话（02 §3 的 `correct` 行）：`recovering` 走 [`reconcile`]，
//! `running`/`paused` 先 `finish`（它们的事实还在变），`discarded` 是整次作废的记录
//! ——它不是可信历史，只在历史与审计里可查。
//!
//! 被修正的区间必须是该会话**已确认且未作废**的区间。候选端点、已作废的行一律拒绝：
//! 那是恢复流程（[`reconcile`]）或「只在审计里看」的对象，不是可以就地改写的事实。
//!
//! [`reconcile`]: crate::services::recovery::reconcile
//!
//! # 两个动作
//!
//! - `Retime { started_at, ended_at }`：用户给定的新起止，`duration_ms` 由二者算出
//!   （08 §1「手工修正以用户确认的起止重新计算 duration_ms」），三个字段**一次 UPDATE**
//!   同时写——不允许留下「改了起止但时长没跟着变」的行（`ck_interval_duration` 兜底）。
//!   校验：非负（`IntervalRange::new`）、`ended_at <= now`、与**全部**有效人工区间
//!   不重叠（S7，`exclude_interval = Some(该区间自身)`，端点相接不算；机器时间按独立
//!   口径不参与互斥）。
//! - `Delete`：软删除（`void_interval`）——`voided_at` 置位、`needs_review` 清零，
//!   行与审计都留着；有时长的区间保留原始起止（作废不是「没发生过」，是「不计入」）。
//!
//! # 幂等（04 §9 用例 3）
//!
//! `Retime` 的新起止与现值**逐字段相同** ⇒ [`WriteOutcome::Unchanged`]：不写审计、
//! 不加 `revision`、不加 `row_version`，只返回当前行。（`Delete` 没有这一支：
//! 已作废的行在区间前置里就被拒了，重复删除不会变成「又作废一次」。）
//!
//! # 相交判定的唯一入口（P3 Task 5，I3）
//!
//! 两段区间是否相交**只有一处实现**：[`crate::domain::interval`] 的
//! [`IntervalRange::overlap_ms`] / [`IntervalRange::overlaps`] /
//! [`IntervalRange::clipped_ms`]，以及
//! [`IntervalFacts`](crate::domain::interval::IntervalFacts) /
//! [`IntervalSet`](crate::domain::interval::IntervalSet)（`insert` 已经是
//! 「与既有区间重叠则拒绝、端点相接允许」）。本模块**不新写第二份相交实现**：
//! `correct` 的重叠校验走 [`session_repo::require_no_human_overlap`]（S7，半开、
//! 端点相接不算），它与上面那条公式是同一套口径；P5 的统计口径同样直接复用它们。
//!
//! # 并发：用**所属会话**的版本
//!
//! `env.expected_row_version` 是会话版本，不新增 `interval.row_version`。一次真实的
//! 修正会在同一事务里 `update_session_state`（传原状态、`SessionStateUpdate::default()`）
//! 把 `session.row_version` 恰好 +1 ⇒ 改另一条区间的旧版本请求同样被拒。
//!
//! # 不改的三件事
//!
//! - **不改 `session.ended_at`**：它记录的是「会话结束那一刻」的事实；「最后一个区间的
//!   结束」由区间算（P5 的口径）。
//! - **不改写 `task_change`**：已完成任务的完成时刻不因修正区间而移动（02 §10：报告按
//!   `task_change` 里的完成事件选完成项，不用 `updated_at`、也不用会话结束时间）。
//! - **不重扫恢复门禁**：`correct` 不改恢复性；门禁快照归
//!   `AppState::rescan_recovery`（S1），由 `AppState::correct` 之外的路径维护。
//!
//! # `time_edit` 的形状（与 `services::recovery` 的扫描审计同一份形状）
//!
//! `before_json`/`after_json` 各带 `session{id,state,run_id,needs_review,row_version}`
//! 与该会话**全部区间**的逐字段值（含 `voided_at`）——改动前后的值都留在里面。
//! `change = correct_retime | correct_delete`、`reason = correct:retime | correct:delete`；
//! `backfill` 用 `change = reason = backfill`（创建型，见上文）。
//! 端点是**用户给定的值**，不是候选推导 ⇒ **不写** `candidate_end`/`candidate_end_source`
//! （Ruling 8）。用户给的理由（`CorrectRequest::reason`）去空白后写在 `after_json` 的
//! `user_reason` 键里：`reason` 列要留给可机器过滤的 `correct:*` 取值。

use rusqlite::Transaction;

use crate::domain::error::DomainError;
use crate::domain::interval::IntervalRange;
use crate::domain::session::{SessionMode, SessionState, TimerKind};
use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::guards::{guard_epoch, guard_row_version};
use crate::storage::meta::require_meta;
use crate::storage::session_repo::{
    self, IntervalRow, NewFinishedSession, SessionRow, SessionStateUpdate,
};
use crate::storage::task_repo::{self, Page};
use crate::storage::time_edit_repo::{self, TimeEdit};
use crate::storage::WriteOutcome;

use super::audit::edit_json_base;
use super::tx::{settle, settle_into, write_tx, SettledReport};

/// 一次历史修正的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectAction {
    /// 重定时：用户给定的新起止（时长由二者算出）。
    Retime { started_at: i64, ended_at: i64 },
    /// 删除误记：软删除该区间（作废，不 `DELETE`）。
    Delete,
}

/// 一次历史修正的请求。`env` 的版本位是**所属会话**的版本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectRequest {
    pub session_id: String,
    pub interval_id: String,
    pub action: CorrectAction,
    /// 用户给的理由（可选）；落在审计的 `after_json.user_reason` 里。
    pub reason: Option<String>,
}

/// 一次修正的结果：会话（版本已 +1）+ 被修正的那条区间（修正后的权威行）。
///
/// `Serialize`（P8 Task 2a）：它是 `correct` / `backfill` / `discard_session` 三条
/// IPC 命令共用的响应 DTO（`commands::*_impl` 原样返回它，并把它当
/// `domain.changed` 的载荷），所以不在命令层另造一份镜像字段表——与
/// `stats::TodayView` 同一条口径。它也因此把 `SessionRow` / `IntervalRow`
/// 带进了 IPC 契约。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HistoryEditReport {
    pub session: SessionRow,
    pub interval: IntervalRow,
    pub revision: i64,
    pub data_epoch: String,
}

/// 用户命令：修正一条可信历史区间（重定时 / 软删除）。
///
/// 事务由本函数拥有：`write_tx`（含 `guard_epoch`）→ 全部校验 → 写入 → 会话版本 +1 →
/// 审计 → `settle`（`Changed` 时恰好一次 `revision`）→ `commit`。**全部校验都在任何
/// 写入之前**，所以「整条命令拒绝」就是零变化。
pub fn correct(
    db: &mut Db,
    env: WriteEnvelope,
    req: CorrectRequest,
    now: i64,
) -> Result<WriteOutcome<HistoryEditReport>, AppError> {
    let expected_version = env.expected_row_version.ok_or_else(|| AppError::Domain {
        detail: "缺少记录版本，无法安全地修正这段历史。".into(),
    })?;

    let tx = write_tx(db, &env)?;

    let session =
        session_repo::get_session(&tx, &req.session_id)?.ok_or(DomainError::UnknownSession)?;
    guard_row_version(session.row_version, expected_version)?;
    require_finished(&session)?;

    let interval =
        session_repo::get_interval(&tx, &req.interval_id)?.ok_or(DomainError::UnknownInterval)?;
    // 区间必须属于这条会话、已确认、未作废。
    //
    // 后两条与「会话必须是 finished」同一口径：候选端点与已作废的行都不是可以就地
    // 改写的事实，用户该去的是恢复流程（`reconcile`）。用 `RECOVERY_REQUIRED` 而不是
    // 普通拒绝，是因为前端要据此换流程（00 §4：只有「未解决的恢复事实」用这个码）。
    if interval.session_id != req.session_id
        || interval.voided_at.is_some()
        || interval.needs_review
    {
        return Err(AppError::RecoveryRequired);
    }

    // 请求级校验全部在写之前。
    if let CorrectAction::Retime {
        started_at,
        ended_at,
    } = req.action
    {
        let range = IntervalRange::new(started_at, ended_at)?;
        // 逐字段相同 ⇒ 幂等：一个字段都不改（含 `session.row_version`），也不写审计。
        if interval.started_at == started_at && interval.ended_at == Some(ended_at) {
            let outcome = settle_into(settle(
                &tx,
                WriteOutcome::Unchanged(report(session, interval)),
            )?);
            tx.commit().map_err(map_sqlite)?;
            return Ok(outcome);
        }
        if ended_at > now {
            return Err(AppError::Domain {
                detail: "修正后的结束时刻不能晚于当前时间。".into(),
            });
        }
        // 与全部有效人工区间不重叠（跨会话、半开、端点相接不算）。排除自己：
        // 否则「原地重定时」会与自己相撞（`tests/correct.rs` 钉住了这条反例）。
        session_repo::require_no_human_overlap(
            &tx,
            range.start,
            range.end,
            Some(&req.interval_id),
        )?;
    }

    let before_intervals = session_repo::intervals_of_session(&tx, &req.session_id)?;
    let (updated_interval, change, reason) = match req.action {
        CorrectAction::Retime {
            started_at,
            ended_at,
        } => (
            session_repo::retime_interval(
                &tx,
                &req.interval_id,
                started_at,
                ended_at,
                ended_at - started_at,
            )?,
            "correct_retime",
            "correct:retime",
        ),
        CorrectAction::Delete => (
            session_repo::void_interval(&tx, &req.interval_id, now)?,
            "correct_delete",
            "correct:delete",
        ),
    };

    // 一次真实修正改的是「这条会话的区间事实」：同一事务里把会话版本 +1
    // （状态、`ended_at`、`run_id`、`needs_review` 一个都不动）。
    let updated_session = session_repo::update_session_state(
        &tx,
        &req.session_id,
        expected_version,
        SessionState::Finished,
        SessionStateUpdate::default(),
    )?;
    let after_intervals = session_repo::intervals_of_session(&tx, &req.session_id)?;

    let user_reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    record_edit(
        &tx,
        &HistoryEdit {
            change,
            reason,
            before_session: &session,
            after_session: &updated_session,
            before_intervals: &before_intervals,
            after_intervals: &after_intervals,
            user_reason,
        },
        now,
    )?;

    let outcome = settle_into(settle(
        &tx,
        WriteOutcome::Changed(HistoryEditReport {
            session: updated_session,
            interval: updated_interval,
            // 下面由 `settle` 的结果填权威值（同一事务里读回）。
            revision: 0,
            data_epoch: String::new(),
        }),
    )?);
    tx.commit().map_err(map_sqlite)?;
    Ok(outcome)
}

/// 只接 `finished`（02 §3）。其余状态各自说清该去哪儿。
fn require_finished(session: &SessionRow) -> Result<(), AppError> {
    match session.state {
        SessionState::Finished => Ok(()),
        // 待确认的区间编辑走 `reconcile`；这是唯一指向恢复流程的码。
        SessionState::Recovering => Err(AppError::RecoveryRequired),
        SessionState::Running | SessionState::Paused => Err(AppError::Domain {
            detail: "这条会话还在计时，请先结束计时再修正历史。".into(),
        }),
        SessionState::Discarded => Err(AppError::Domain {
            detail: "整次作废的记录不是可信历史，不能再修正。".into(),
        }),
    }
}

/// 报告骨架：`revision`/`data_epoch` 由 [`settle`] 的结果填。
fn report(session: SessionRow, interval: IntervalRow) -> HistoryEditReport {
    HistoryEditReport {
        session,
        interval,
        revision: 0,
        data_epoch: String::new(),
    }
}

impl SettledReport for HistoryEditReport {
    fn revision_mut(&mut self) -> &mut i64 {
        &mut self.revision
    }

    fn data_epoch_mut(&mut self) -> &mut String {
        &mut self.data_epoch
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 手工补录（P3 Task 4）
// ─────────────────────────────────────────────────────────────────────────────

/// 一次手工补录的请求。
///
/// `env` 是**新建**信封：补录没有可校验的行版本（它不修改任何既有的可编辑对象）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillRequest {
    pub task_id: String,
    pub started_at: i64,
    pub ended_at: i64,
}

/// 用户命令：把一段**已经发生**的人工时间补录成一条 `finished` 会话。
///
/// 全部校验都在任何写入之前 ⇒ 「整条命令拒绝」就是零变化（不会留下半个会话）。
/// 固定字段（02 §6：人工只有 FOREGROUND；V0.1 不提供补录倒计时/机器会话的入口）：
/// `mode = FOREGROUND`、`timer_kind = stopwatch`、`target_duration_ms = NULL`，
/// `run_id` 取**当前 run**（由调用方从协调器取：服务层够不着协调器）。
///
/// 本函数**不碰镜像、不碰门禁**：它新建的是一条终态会话，不改变协调器正镜像的那条，
/// 也不产生任何待处理事实；提交后的收尾归 `AppState::backfill`（那里明确什么都不做）。
pub fn backfill(
    db: &mut Db,
    env: WriteEnvelope,
    req: BackfillRequest,
    now: i64,
    run_id: &str,
) -> Result<WriteOutcome<HistoryEditReport>, AppError> {
    let tx = write_tx(db, &env)?;

    // ① 任务必须存在（补录的是「这个任务的时间」）。
    task_repo::get_task(&tx, &req.task_id)?.ok_or(DomainError::UnknownTask)?;

    // ② 范围合法：非负，且不越过「现在」——未来的「已发生工时」不是事实。
    let range = IntervalRange::new(req.started_at, req.ended_at)?;
    if range.end > now {
        return Err(AppError::Domain {
            detail: "补录的结束时刻不能晚于当前时间。".into(),
        });
    }

    // ③ 与全部有效人工时间不重叠（S7：跨会话、半开、端点相接不算；
    //    正在计时的区间算重叠；机器模式按独立口径不参与互斥）。
    session_repo::require_no_human_overlap(&tx, range.start, range.end, None)?;

    // ④ 一次事务里建会话 + 区间 + 审计：失败一起回滚。
    let session_id = uuid::Uuid::new_v4().to_string();
    let interval_id = uuid::Uuid::new_v4().to_string();
    let session = session_repo::create_finished_session(
        &tx,
        &NewFinishedSession {
            id: &session_id,
            task_id: &req.task_id,
            run_id,
            mode: SessionMode::Foreground,
            timer_kind: TimerKind::Stopwatch,
            target_duration_ms: None,
            interval_id: &interval_id,
            started_at: range.start,
            ended_at: range.end,
            duration_ms: range.duration_ms(),
        },
    )?;

    let intervals = session_repo::intervals_of_session(&tx, &session_id)?;
    let interval = intervals
        .first()
        .cloned()
        .ok_or_else(|| AppError::Storage {
            detail: "backfilled interval vanished after insert".into(),
        })?;
    record_backfill(&tx, &session, &intervals, now)?;

    // 成功必然 `Changed`：这条命令的定义就是「新建事实」。
    let outcome = settle_into(settle(
        &tx,
        WriteOutcome::Changed(report(session, interval)),
    )?);
    tx.commit().map_err(map_sqlite)?;
    Ok(outcome)
}

/// 补录的创建型审计：改动前没有这条会话，所以 `before_json` 只有判别键。
fn record_backfill(
    tx: &Transaction<'_>,
    session: &SessionRow,
    intervals: &[IntervalRow],
    now: i64,
) -> Result<(), AppError> {
    time_edit_repo::write(
        tx,
        &TimeEdit {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session.id.clone(),
            before_json: serde_json::json!({ "change": BACKFILL_CHANGE }).to_string(),
            after_json: edit_json(BACKFILL_CHANGE, session, intervals, None),
            reason: Some(BACKFILL_CHANGE.to_string()),
            created_at: now,
        },
    )
}

/// 补录在 `time_edit` 两个判别位上的取值（审计的消费者按它过滤）。
const BACKFILL_CHANGE: &str = "backfill";

// ─────────────────────────────────────────────────────────────────────────────
// 审计
// ─────────────────────────────────────────────────────────────────────────────

/// 一条修正审计（与它描述的改动**同事务**）。
struct HistoryEdit<'a> {
    change: &'static str,
    reason: &'static str,
    before_session: &'a SessionRow,
    after_session: &'a SessionRow,
    before_intervals: &'a [IntervalRow],
    after_intervals: &'a [IntervalRow],
    /// 用户给的理由（已去空白）；只在 `after_json` 里出现。
    user_reason: Option<&'a str>,
}

fn record_edit(tx: &Transaction<'_>, edit: &HistoryEdit<'_>, now: i64) -> Result<(), AppError> {
    time_edit_repo::write(
        tx,
        &TimeEdit {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: edit.after_session.id.clone(),
            before_json: edit_json(
                edit.change,
                edit.before_session,
                edit.before_intervals,
                None,
            ),
            after_json: edit_json(
                edit.change,
                edit.after_session,
                edit.after_intervals,
                edit.user_reason,
            ),
            reason: Some(edit.reason.to_string()),
            created_at: now,
        },
    )
}

/// 与 `services::recovery` 的扫描审计**同一份骨架**（Ruling 8/R12）：公共部分在
/// [`edit_json_base`]（唯一一处形状），本模块只加自己的可选键 `user_reason`
/// ——没有候选端点推导，所以没有 `candidate_*` 键。
fn edit_json(
    change: &str,
    session: &SessionRow,
    intervals: &[IntervalRow],
    user_reason: Option<&str>,
) -> String {
    let mut json = edit_json_base(change, session, intervals);
    if let Some(reason) = user_reason {
        json["user_reason"] = serde_json::json!(reason);
    }
    json.to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// 常规历史：读（P8 Task 2b）
// ─────────────────────────────────────────────────────────────────────────────

/// 一次常规历史读取的请求（服务层形状；IPC 的 `HistoryQuery` 由命令层解析后传进来）。
///
/// `limit` / `offset` 在这里是裸整数，**由本服务**装进 [`Page`]（命令层不许碰
/// `storage::`，见 `scripts/check-layers.ps1` 的第 1 条规则）；取值域与越界文案归
/// [`Page`] / `task_repo::require_page`（分页规则只有一处实现）。`session_id` 是
/// **可选详情**的键——`None` 表示只要列表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryViewRequest {
    /// 半开窗口的下界（含）。
    pub from: i64,
    /// 半开窗口的上界（**不含**）。
    pub to: i64,
    /// 每页条数（1..=100）。
    pub limit: i64,
    /// 跳过条数（>= 0）。
    pub offset: i64,
    /// 要展开详情的会话；`None` = 不取详情。
    pub session_id: Option<String>,
}

/// 一个会话的**详情**：会话行 + 它**全部**区间 + 它**全部**审计。
///
/// 「全部」是有意的：已作废的区间与已删除的审计都留着（软删除），历史页要能看到
/// 「这条记录被改过什么」。`session.row_version` 就是**库里的真实行版本**，**版本**可以
/// 直接当 `correct` / `discard_session` 的 `expected_row_version` 用（不要在界面上自己
/// 加一）。
///
/// **但「版本可用」不等于「动作合法」，两条动作的合法前提并不相同**：`correct` 只接
/// `finished`（[`require_finished`]，详情对**任何状态**的会话都返回，这条读路径不判状态），
/// `recovering` 要走 `reconcile`、`running`/`paused` 要先结束会话；`discard_session` 则是
/// **无状态前置**——`running` / `paused` / `recovering` / `finished` 都能作废（唯一的库内
/// 前置是「该会话至少一条区间」）。界面据 `state` 决定 `correct` 的入口开不开，别把这句读成
/// 「拿到版本就能改」，也别拿 `state` 把作废一起禁掉。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HistoryDetail {
    pub session: SessionRow,
    /// 该会话的全部区间，按 `(started_at, id)` 升序（含 `voided_at` 非空的）。
    pub intervals: Vec<IntervalRow>,
    /// 该会话的全部审计，按 `(created_at, id)` 升序。
    pub edits: Vec<TimeEdit>,
}

/// 历史页的一次读取结果（命令 13 的响应 DTO）。
///
/// `sessions` 是**窗口内的这一页**（口径见 [`session_repo::history_sessions`]：
/// 只含 `finished`/`discarded`，半开窗口，按 `(started_at, id)` 升序）。它**不是**
/// 「窗口内的总数」——契约形状由 P8 计划钉死（`{data_epoch, revision, sessions,
/// selected}`），所以这里**不另加** `total` 字段；调用方按「取满了 `limit` 条 ⇒ 还
/// 可能有下一页」翻页（`limit`/`offset` 的越界在服务层就拒绝，不静默截断）。
///
/// `selected` 是显式要详情的那个会话；**指名一个不存在的 id 不是错误**，只是
/// `None`（与 [`crate::services::catalog::list_tasks_filtered`] 里「项目 ID 是直接键，
/// 指向不存在的项目只意味着那里没有任务」同一口径）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HistoryView {
    /// 窗口内这一页的终态会话（`finished` / `discarded`）。
    pub sessions: Vec<SessionRow>,
    /// 显式指名的那条会话的详情；没给或不存在时为 `None`。
    pub selected: Option<HistoryDetail>,
    /// 这次读看到的库身份。
    pub data_epoch: String,
    /// 这次读看到的业务版本。读**不**改它。
    pub revision: i64,
}

/// 常规历史（命令 13）：窗口内的一页终态会话，外加可选的一条详情。
///
/// **纯读**：不开写事务、不加 `revision`、不采样、不写审计。为了「数据与元数据出自
/// 同一个读事务」，这里显式开一个只读事务，`guard_epoch` 在事务内跑（形状抄
/// `catalog::list_projects`）。被拒的旧 epoch **零读取、零写入**。
///
/// 三条口径：
/// - **常规历史不看恢复面**：列表只覆盖 `finished` / `discarded`，`recovering` 与正在
///   计时的会话不在这里（它们归恢复页与计时区）。这里**不调用** `attention_overview`
///   ——它有自己的读事务，调它会让「两批事实同一个版本」变成靠纪律。
/// - **窗口形状**由 [`IntervalRange::new`] 校验：`from > to` 报 `NegativeInterval`
///   （`from == to` 合法，与全项目同一判据）。
/// - **详情不按窗口过滤**：显式给了 `session_id` 就按 id 取（哪怕它不在窗口里）——
///   界面是从列表里点的，id 与窗口本就同源；再判一次只会多一个拒绝路径。
pub fn history_view(
    db: &Db,
    expected_data_epoch: &str,
    req: HistoryViewRequest,
) -> Result<HistoryView, AppError> {
    // 范围形状与全项目同一判据；`from == to` 是合法的空窗口。
    IntervalRange::new(req.from, req.to)?;

    let tx = db
        .connection()
        .unchecked_transaction()
        .map_err(map_sqlite)?;
    guard_epoch(&tx, expected_data_epoch)?;

    let sessions = session_repo::history_sessions(
        &tx,
        req.from,
        req.to,
        Page {
            limit: req.limit,
            offset: req.offset,
        },
    )?;
    let selected = match req.session_id {
        Some(session_id) => match session_repo::get_session(&tx, &session_id)? {
            Some(session) => Some(HistoryDetail {
                intervals: session_repo::intervals_of_session(&tx, &session_id)?,
                edits: time_edit_repo::edits_of_session(&tx, &session_id)?,
                session,
            }),
            None => None,
        },
        None => None,
    };

    let meta = require_meta(&tx)?;
    // 读事务什么都没写：直接结束它（回滚一个只读事务不改变任何事实），
    // 免得读代码的人以为这里还欠一个 `commit`。
    drop(tx);

    Ok(HistoryView {
        sessions,
        selected,
        data_epoch: meta.data_epoch,
        revision: meta.revision,
    })
}
