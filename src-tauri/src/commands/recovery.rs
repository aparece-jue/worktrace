//! 恢复与历史（P3 的服务层入口，P8 的 IPC）的请求 DTO、字符串枚举解析与命令体；`#[tauri::command]` 包装留在 `super`。

use crate::domain::error::DomainError;
use crate::domain::task::{TaskStatus, TransitionCause};
use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::services::bootstrap::AppState;
use crate::services::events::Broadcaster;
use crate::services::timer::coordinator::ClockCorrectionAccepted;
use crate::services::timer::snapshot::TimerSnapshot;
use crate::services::{history, recovery, tasks};

use super::{announce, unknown_enum_value, EpochRequest};

/// 对账（确认 / 丢弃不确定区间）。`expected_row_version` 是**会话**版本。
///
/// `action` / `target_state` 是字符串（见模块头）：`confirm` / `discard_uncertain`
/// 与 `paused` / `finished`。`ranges` 只在 `confirm` 时携带内容，且必须**恰好覆盖**
/// 该会话的全部待确认区间；`discard_uncertain` 必须给空列表（作废集合由服务从库里取，
/// 不由客户端指认一条）。两条动作都是本命令，**作废整次**是 [`DiscardSessionRequest`]。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ReconcileRequest {
    pub expected_data_epoch: String,
    pub session_id: String,
    pub expected_row_version: i64,
    pub action: String,
    pub target_state: String,
    pub ranges: Vec<ConfirmedRangeRequest>,
}

/// [`ReconcileRequest::ranges`] 的一项：用户确认的一段区间。
///
/// 起止都是**用户给定的值**（与 `services::recovery::ConfirmedRange` 逐字段相同），
/// 不是候选端点推导出来的——候选端点只是展示材料。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ConfirmedRangeRequest {
    pub interval_id: String,
    pub started_at: i64,
    pub ended_at: i64,
}

/// 历史修正（重定时 / 软删除）。`expected_row_version` 是**所属会话**的版本，**必填**。
///
/// `action` 是字符串：`retime` 必须带 `started_at` + `ended_at`（时长由服务算），
/// `delete` 不用它们。三个可选字段（`started_at` / `ended_at` / `reason`）都可以**省略**
/// ——省略等价于 `null`。`reason` 是用户给的理由，落在审计里。
///
/// **版本位必填**：它与其他四条命令、以及 `expected_data_epoch` 同一口径——必填标量
/// 少了就是传输层（serde）错误，**不是**一条「我不知道版本」的可用调用路径。
/// 服务层那条「缺少记录版本，无法安全地修正这段历史。」守卫因此从命令层不可达；
/// 它在服务层被 `tests/correct.rs` 的两条用例覆盖（既有的一条用 `for_create` 信封，
/// P8 Task 2a 修复轮补的一条额外钉住「是哪一条拒绝」的文案）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CorrectRequest {
    pub expected_data_epoch: String,
    pub session_id: String,
    pub expected_row_version: i64,
    pub interval_id: String,
    pub action: String,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub reason: Option<String>,
}

/// 手工补录一段**已经发生**的人工时间（新建 ⇒ 只需 epoch）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BackfillRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub started_at: i64,
    pub ended_at: i64,
}

/// 作废整次会话。`expected_row_version` 是**会话**版本。
///
/// 它与 [`ReconcileRequest`] 的 `discard_uncertain` 是**两条命令、两个语义**：
/// 这里作废该会话的全部区间并把会话推成 `discarded`（无状态前置），那边只丢
/// 待确认区间、保留可信前缀。界面上也不许合并成一个「丢弃」按钮。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DiscardSessionRequest {
    pub expected_data_epoch: String,
    pub session_id: String,
    pub expected_row_version: i64,
}

/// 任务状态跃迁。`expected_row_version` 是**任务**版本。
///
/// `target` / `cause` 是字符串：`target` 是落库用的状态名（`Done` / `Blocked` /
/// `Ready` …，与 `TaskStatus::as_str()` 逐字一致），`cause` 是 `user` / `reopen`
/// （`reopen` 是终结态回 `Ready` 的唯一合法原因）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TransitionTaskRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub expected_row_version: i64,
    pub target: String,
    pub cause: String,
}

/// 常规历史的一次读取（命令 13）：半开窗口 `[from, to)` + 分页窗口 + 可选详情。
///
/// 前五个标量都是**必填**（与其余命令的 epoch/版本同一口径）：`from`/`to` 少的不是
/// 「全窗口」而是传输层（serde）错误——「不限制范围」在这里是一条不该存在的路径
/// （历史页永远有一个正在看的范围）。`limit` 的取值域是 `1..=100`、`offset` `>= 0`，
/// 越界由服务层经 `task_repo::require_page` 拒绝（命令层不复制那条规则）。
///
/// `session_id` 省略 / `null` = 只要列表；给了就额外返回那条会话的详情
/// （session + 全部区间 + 全部审计，含真实 `row_version`）。**`HistoryQuery` 里没有
/// `total`**：响应形状由 P8 计划钉死为 `{data_epoch, revision, sessions, selected}`，
/// 翻页按「取满 `limit` 条 ⇒ 还可能有下一页」——不在这里发明第二个形状。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct HistoryQuery {
    pub expected_data_epoch: String,
    pub from: i64,
    pub to: i64,
    pub limit: i64,
    pub offset: i64,
    pub session_id: Option<String>,
}

/// `reconcile` 的动作字符串（模块头：字符串枚举显式解析，不靠 serde）。
fn parse_reconcile_action(raw: &str) -> Result<recovery::ReconcileAction, AppError> {
    match raw.trim() {
        "confirm" => Ok(recovery::ReconcileAction::Confirm),
        "discard_uncertain" => Ok(recovery::ReconcileAction::DiscardUncertain),
        "" => Err(DomainError::EmptyText {
            field: "对账动作"
        }
        .into()),
        other => Err(unknown_enum_value("对账动作", other)),
    }
}

/// 对账之后会话停在哪个状态。取值域只有 `paused` / `finished` 两个
/// （`running` / `recovering` / `discarded` 都不是「对账后的状态」）。
fn parse_reconcile_target_state(raw: &str) -> Result<recovery::ReconcileTargetState, AppError> {
    match raw.trim() {
        "paused" => Ok(recovery::ReconcileTargetState::Paused),
        "finished" => Ok(recovery::ReconcileTargetState::Finished),
        "" => Err(DomainError::EmptyText {
            field: "对账后的会话状态",
        }
        .into()),
        other => Err(unknown_enum_value("对账后的会话状态", other)),
    }
}

/// `correct` 的动作（字符串 + 可选起止 → 服务枚举）。
///
/// `retime` 的两个时刻由**请求**给（服务的 `CorrectAction::Retime` 必带它们）；
/// 少一个就是构造不出请求，落在 `DOMAIN_ERROR` 上。`delete` 不带时刻。
fn parse_correct_action(request: &CorrectRequest) -> Result<history::CorrectAction, AppError> {
    match request.action.trim() {
        "retime" => match (request.started_at, request.ended_at) {
            (Some(started_at), Some(ended_at)) => Ok(history::CorrectAction::Retime {
                started_at,
                ended_at,
            }),
            _ => Err(AppError::Domain {
                detail: "重定时必须同时给出开始与结束时刻。".into(),
            }),
        },
        "delete" => Ok(history::CorrectAction::Delete),
        "" => Err(DomainError::EmptyText {
            field: "修正动作"
        }
        .into()),
        other => Err(unknown_enum_value("修正动作", other)),
    }
}

/// 任务状态目标。**就是落库用的状态名**（`TaskStatus::as_str()`），
/// 与 `parse_session_mode` 同形：空白 ⇒ `EmptyText`，取值域外 ⇒ `UnknownEnumValue`。
///
/// 取值域里含 V0.1 不写的 `Scheduled`：这里**不**自己加「可写性」判断——
/// 那条规则在服务层（`TaskStatus::is_writable_in_v01` 与跃迁表），命令层不重复。
fn parse_task_target(raw: &str) -> Result<TaskStatus, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DomainError::EmptyText {
            field: "任务状态"
        }
        .into());
    }
    TaskStatus::parse(trimmed).ok_or_else(|| unknown_enum_value("任务状态", raw))
}

/// 跃迁原因。`TransitionCause` 在 domain 里没有 `parse`/`as_str`（它从不落库、
/// 也不进任何快照），所以 IPC 的字符串形状在这里一次定死：`user` / `reopen`。
fn parse_transition_cause(raw: &str) -> Result<TransitionCause, AppError> {
    match raw.trim() {
        "user" => Ok(TransitionCause::User),
        "reopen" => Ok(TransitionCause::Reopen),
        "" => Err(DomainError::EmptyText {
            field: "跃迁原因"
        }
        .into()),
        other => Err(unknown_enum_value("跃迁原因", other)),
    }
}

/// [`reconcile`] 的命令体（IPC 包装只做转发）。
pub fn reconcile_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: ReconcileRequest,
) -> Result<recovery::ReconcileReport, AppError> {
    // 先解析（可能拒绝，且不该为一次坏请求取时钟样本），再取信封与时钟。
    let action = parse_reconcile_action(&request.action)?;
    let target_state = parse_reconcile_target_state(&request.target_state)?;
    let env = WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
    let req = recovery::ReconcileRequest {
        session_id: request.session_id,
        action,
        target_state,
        ranges: request
            .ranges
            .into_iter()
            .map(|range| recovery::ConfirmedRange {
                interval_id: range.interval_id,
                started_at: range.started_at,
                ended_at: range.ended_at,
            })
            .collect(),
    };
    let now = app.now_ms()?;
    let (report, changed) = app.reconcile(env, req)?.into_parts();
    Ok(announce(
        broadcaster,
        changed,
        report.data_epoch.clone(),
        report.revision,
        now,
        report,
    ))
}

/// [`correct`] 的命令体（IPC 包装只做转发）。
pub fn correct_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: CorrectRequest,
) -> Result<history::HistoryEditReport, AppError> {
    let action = parse_correct_action(&request)?;
    // 改既有对象 ⇒ `for_update` 这条规范构造点（版本位必填，见请求 DTO）。
    let env = WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
    let req = history::CorrectRequest {
        session_id: request.session_id,
        interval_id: request.interval_id,
        action,
        reason: request.reason,
    };
    let now = app.now_ms()?;
    let (report, changed) = app.correct(env, req)?.into_parts();
    Ok(announce(
        broadcaster,
        changed,
        report.data_epoch.clone(),
        report.revision,
        now,
        report,
    ))
}

/// [`backfill`] 的命令体（IPC 包装只做转发）。
pub fn backfill_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: BackfillRequest,
) -> Result<history::HistoryEditReport, AppError> {
    // 补录**新建**一条 `finished` 会话：没有可校验的行版本 ⇒ `for_create`。
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let req = history::BackfillRequest {
        task_id: request.task_id,
        started_at: request.started_at,
        ended_at: request.ended_at,
    };
    let now = app.now_ms()?;
    let (report, changed) = app.backfill(env, req)?.into_parts();
    Ok(announce(
        broadcaster,
        changed,
        report.data_epoch.clone(),
        report.revision,
        now,
        report,
    ))
}

/// [`discard_session`] 的命令体（IPC 包装只做转发）。
pub fn discard_session_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: DiscardSessionRequest,
) -> Result<history::HistoryEditReport, AppError> {
    let env = WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
    let req = recovery::DiscardSessionRequest {
        session_id: request.session_id,
    };
    let now = app.now_ms()?;
    let (report, changed) = app.discard_session(env, req)?.into_parts();
    Ok(announce(
        broadcaster,
        changed,
        report.data_epoch.clone(),
        report.revision,
        now,
        report,
    ))
}

/// [`transition_task`] 的命令体（IPC 包装只做转发）。
pub fn transition_task_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: TransitionTaskRequest,
) -> Result<tasks::TaskTransitionReport, AppError> {
    let target = parse_task_target(&request.target)?;
    let cause = parse_transition_cause(&request.cause)?;
    let env = WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
    let req = tasks::TransitionTaskRequest {
        task_id: request.task_id,
        target,
        cause,
    };
    let now = app.now_ms()?;
    let (report, changed) = app.transition_task(env, req)?.into_parts();
    Ok(announce(
        broadcaster,
        changed,
        report.data_epoch.clone(),
        report.revision,
        now,
        report,
    ))
}

/// [`accept_detected_clock_correction`] 的命令体（IPC 包装只做转发）。
///
/// 广播口径见本节标题下的第 6 条：`accepted` 就是「这次到底改没改库」这一位
/// （服务只在审计提交成功时给它 `true`），所以它直接当 [`announce`] 的 `changed`——
/// 与写命令族「仅 `Changed` 才广播」同一姿势，不另造判据、不按 `revision` 自比。
pub fn accept_detected_clock_correction_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: EpochRequest,
) -> Result<ClockCorrectionAccepted, AppError> {
    // 信封的 `at` 在**写入之前**取（照 `create_project_impl` 的顺序）：提交成功之后再取
    // 时钟，一旦取不到就会把一次**已经提交**的接受报成失败。这次采样只是读一次原始挂钟
    // （`Coordinator::wall_ms`），不 `observe`、不推进检测器的 `last`——服务那一次
    // 「采样 + 恰好观察一次」的口径不受影响。
    let now = app.now_ms()?;
    let accepted = app.accept_detected_clock_correction(&request.expected_data_epoch)?;
    Ok(announce(
        broadcaster,
        accepted.accepted,
        accepted.data_epoch.clone(),
        accepted.revision,
        now,
        accepted,
    ))
}

/// [`retry_recovery`] 的命令体（IPC 包装只做转发）。
pub fn retry_recovery_impl(
    app: &mut AppState,
    request: EpochRequest,
) -> Result<TimerSnapshot, AppError> {
    app.retry_recovery(&request.expected_data_epoch)
}

/// [`attention_overview`] 的命令体（IPC 包装只做转发）。
///
/// 三个参数在这里组装：`db` 与 `coordinator` 都是 `AppState` 的 `pub` 访问器，`run_id`
/// 由协调器给出（服务层够不着它）——这条命令**没有** `AppState` 包装。
pub fn attention_overview_impl(
    app: &mut AppState,
    request: EpochRequest,
) -> Result<recovery::AttentionOverview, AppError> {
    let run_id = app.coordinator()?.run_id().to_owned();
    recovery::attention_overview(app.db()?, &request.expected_data_epoch, &run_id)
}

/// [`history_view`] 的命令体（IPC 包装只做转发）。
pub fn history_view_impl(
    app: &mut AppState,
    request: HistoryQuery,
) -> Result<history::HistoryView, AppError> {
    history::history_view(
        app.db()?,
        &request.expected_data_epoch,
        history::HistoryViewRequest {
            from: request.from,
            to: request.to,
            limit: request.limit,
            offset: request.offset,
            session_id: request.session_id,
        },
    )
}
