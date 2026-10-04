//! 命令层：IPC 边界（P7 Task 1）。
//!
//! 不直连 SQL，也不接受 `Connection`（总纲 §9）：命令层只做参数反序列化、
//! 调用服务、把 `Result<T, AppError>` 映射成前端可用的形状。
//! 事务与 epoch/version 校验由服务层与 `storage::guards` 负责。
//! 请求信封 [`crate::envelope::WriteEnvelope`] 与错误 [`crate::error::AppError`]
//! 都住在 crate 根（`storage`/`services` 也要用，而它们不得依赖 `commands`），
//! 命令层直接引用它们。
//!
//! # 三条硬约束（D6 定稿，写在 `services::bootstrap` 的模块头）
//!
//! 1. 命令一律 `async`；
//! 2. 阻塞段在 [`tauri::async_runtime::spawn_blocking`] 里，**不在 IPC/UI 线程上跑
//!    SQLite**；
//! 3. **`Connection` 不跨 `await`**——锁与事务都活在那个阻塞闭包内。
//!
//! 每次命令都走同一把 `Mutex<AppState>`（[`lock_app`]），所以用户命令与周期采样
//! 天然串行——这就是「广播按提交顺序」里那条串行边界的落点。
//!
//! # 错误：五个码原样透传
//!
//! 失败统一走 [`capture_error_response`]（**在原事务结束之后、同一串行边界内**捕获，
//! 不另调 `timer.snapshot` 补版本），载荷是共享的
//! [`ErrorResponse`]（`code`/`message`/`authority`/`requires_handshake`）。
//! 前端只按 `code` 分支，`message` 直接用（R8）。
//!
//! # 请求形状：枚举一律是字符串
//!
//! `mode` / `timer_kind` / `statuses` 在 IPC 里都是**字符串**，命令体显式过
//! [`parse_session_mode`] / [`parse_timer_kind`] / `TaskStatus::parse`
//! （后者在 `services::catalog::TaskQueryRequest` 的 `TryFrom` 里），**不依赖 serde
//! 的枚举反序列化**：后者的失败拿不到 `ErrorResponse.code`，会退化成 Tauri 的
//! 反序列化错误（00 §4 只认那五个码）。
//!
//! 每个命令只收**一个** `request` 参数（请求 DTO 见下）：这样 IPC 的参数名不受
//! Tauri 的 `camelCase` 参数重命名影响，字段名就是本文件里写的那套 snake_case。
//!
//! # 本阶段不做
//!
//! 恢复确认相关命令（P3）；统计与导出命令（P5，P8 接入）；维护态分流（P6）。
//! 命令层**不发** `domain.changed`：本任务的交付边界只到「写命令返回响应」，
//! 事件发送侧的接线归 Task 2/6a（见实施报告「遗留」一节）。

use std::sync::Arc;

use tauri::State;

use crate::envelope::WriteEnvelope;
use crate::error::{AppError, AuthorityKind, AuthorityTarget, ErrorResponse};
use crate::services::bootstrap::{lock_app, AppState, RunningApp};
use crate::services::error_response::capture_error_response;
use crate::services::timer::coordinator::{
    parse_session_mode, parse_timer_kind, CommandOutcome, ResumeRequest, SessionRequest,
    StartRequest,
};
use crate::services::timer::snapshot::TimerSnapshot;
use crate::services::{catalog, daily_plan, handshake};

// ─────────────────────────────────────────────────────────────────────────────
// 命令体的执行骨架
// ─────────────────────────────────────────────────────────────────────────────

/// 在**阻塞线程**上、**串行边界内**执行一次命令体，并统一映射错误。
///
/// `targets` 是这次请求涉及的受控实体（错误上下文用，见
/// [`capture_error_response`]）。它在 `body` 失败**之后**、**仍持有同一把锁**时被使用：
/// 权威 `epoch`/`revision`/目标版本因此出自同一次读事务，不会互相矛盾。
async fn run_command<T, F>(
    state: &State<'_, RunningApp>,
    targets: Vec<AuthorityTarget>,
    body: F,
) -> Result<T, ErrorResponse>
where
    T: Send + 'static,
    F: FnOnce(&mut AppState) -> Result<T, AppError> + Send + 'static,
{
    let app = Arc::clone(state.app());
    match tauri::async_runtime::spawn_blocking(move || {
        let mut guard = lock_app(&app);
        // 分开两步写：`body` 的可变借用必须在 `capture_error_response` 借用同一个
        // `guard` 之前结束（写成一条表达式会被临时值的生命周期绊住）。
        let outcome = body(&mut guard);
        match outcome {
            Ok(value) => Ok(value),
            Err(error) => Err(capture_error_response(guard.db(), &error, &targets)),
        }
    })
    .await
    {
        Ok(result) => result,
        Err(join) => Err(internal_failure(join.to_string())),
    }
}

/// 阻塞任务 panic / 被取消：这是**缺陷**，不是用户错误，也不是业务失败。
///
/// 五个码里没有「内部错误」，取最接近的 `STORAGE_ERROR`。**刻意不带 `authority`**：
/// panic 发生在临界区里，此时再去读一次库既拿不到可信版本，也不该在错误路径上开事务；
/// `requires_handshake` 因此为真，客户端据此重新握手而不是拿一个来源不明的版本继续。
fn internal_failure(detail: String) -> ErrorResponse {
    let error = AppError::Storage { detail };
    ErrorResponse {
        code: error.code().to_owned(),
        message: error.message(),
        authority: None,
        requires_handshake: true,
    }
}

/// 请求涉及的受控实体。`kind` 是白名单枚举，`id` 原样回显
/// （记录已被删除时，回显是它唯一还能对上的身份）。
fn target(kind: AuthorityKind, id: &str) -> AuthorityTarget {
    AuthorityTarget::new(kind, id.to_string())
}

/// `start` 请求里 `expected_interval_ms` 的缺省值：本进程的周期采样节拍。
///
/// 它只用于**识别挂起**（采样隔了多久没来），不是「多久记一次工时」。
/// 省略时按生产节拍取值：客户端不该猜这个数，猜错会让挂起检测失灵。
fn default_expected_interval_ms() -> i64 {
    crate::services::bootstrap::DEFAULT_SAMPLING_INTERVAL_MS as i64
}

// ─────────────────────────────────────────────────────────────────────────────
// 请求 DTO（IPC 形状）
// ─────────────────────────────────────────────────────────────────────────────

/// 只带库身份的请求（可选项目列表用）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct EpochRequest {
    pub expected_data_epoch: String,
}

/// 项目列表请求（F-004）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ListProjectsRequest {
    pub expected_data_epoch: String,
    /// `active` / `archived` / `done`（**读路径**取值域，含 V0.1 不写的 `done`）；
    /// `null` / 省略 = 不限制状态，归档与历史都在里面。
    pub status: Option<String>,
}

/// 新建项目（F-004）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreateProjectRequest {
    pub expected_data_epoch: String,
    pub name: String,
}

/// 重命名项目（F-004）。改既有对象 ⇒ 必须带项目版本。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RenameProjectRequest {
    pub expected_data_epoch: String,
    pub project_id: String,
    pub expected_row_version: i64,
    pub name: String,
}

/// 归档项目（F-004）。归档不删任务、不动历史。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ArchiveProjectRequest {
    pub expected_data_epoch: String,
    pub project_id: String,
    pub expected_row_version: i64,
}

/// 标签列表请求（F-005）。`kind` 省略 = 全部四类。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ListTagsRequest {
    pub expected_data_epoch: String,
    /// `Domain` / `Activity` / `Context` / `Report`（大小写敏感）。
    pub kind: Option<String>,
}

/// 新建标签（F-005）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreateTagRequest {
    pub expected_data_epoch: String,
    pub kind: String,
    pub name: String,
    /// V0.1 没有层级：非空值一律被服务拒绝（`services::catalog::create_tag`）。
    pub parent_id: Option<String>,
}

/// 打标 / 去标（F-005）：一次一个标签、一个任务。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TaskTagRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub tag_id: String,
}

/// 某个任务身上的标签（纯读）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TaskTagsRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
}

/// 捕获一个任务（F-002 的 Inbox 入口）。新建 ⇒ 只需 epoch。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreateTaskRequest {
    pub expected_data_epoch: String,
    pub title: String,
    pub project_id: Option<String>,
}

/// 理清为待办（F-002）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ClarifyReadyRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub expected_row_version: i64,
}

/// 改任务的归属（F-002）。`project` 是二值：`{"bind":"<id>"}` / `"clear"`。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SetTaskProjectRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub expected_row_version: i64,
    pub project: catalog::ProjectTarget,
}

/// 今日计划的加入 / 移除（F-010）。日期与时区都是**原始输入**，
/// 由 `services::daily_plan` 的唯一入口校验与规范化。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PlanMutationRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub date: String,
    pub timezone: String,
}

/// 开始计时（F-003 的 start）。
///
/// `mode` / `timer_kind` 是字符串（见模块头）；`expected_interval_ms` 省略时按
/// 本进程的采样节拍取值——它只用于识别挂起，不是「多久记一次工时」。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct StartTimerRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub task_expected_version: i64,
    pub mode: String,
    pub timer_kind: String,
    /// 倒计时必须有正预算；正计时必须没有（`services::timer` 会拒绝相反的组合）。
    pub target_duration_ms: Option<i64>,
    #[serde(default = "default_expected_interval_ms")]
    pub expected_interval_ms: i64,
}

// ─────────────────────────────────────────────────────────────────────────────
// 握手
// ─────────────────────────────────────────────────────────────────────────────

/// 首次握手与周期版本校验（00 §5 规则 1/4）。
///
/// 纯读、不要求任何已知 epoch；返回的 `data_epoch` 是随后所有业务请求的入参。
/// 它**不代替**业务快照：窗口拿到 epoch 之后仍要逐条拉自己需要的一致视图。
#[tauri::command]
pub async fn get_revision(
    state: State<'_, RunningApp>,
) -> Result<handshake::RevisionSnapshot, ErrorResponse> {
    run_command(&state, Vec::new(), |app| handshake::get_revision(app.db())).await
}

// ─────────────────────────────────────────────────────────────────────────────
// 项目（F-004）
// ─────────────────────────────────────────────────────────────────────────────

/// 完整项目列表：`status = null` 时含归档与 `done` 的历史（Task 5 的列表用）。
#[tauri::command]
pub async fn list_projects(
    state: State<'_, RunningApp>,
    request: ListProjectsRequest,
) -> Result<catalog::ProjectList, ErrorResponse> {
    run_command(&state, Vec::new(), move |app| {
        let status = match request.status.as_deref() {
            // 读路径：`done` 是库里合法的值，这里必须读得懂（写路径才拒绝它）。
            Some(raw) => Some(catalog::parse_project_status_read(raw)?),
            None => None,
        };
        catalog::list_projects(app.db(), &request.expected_data_epoch, status)
    })
    .await
}

/// 新建任务时可选的项目：**只列 active**（F-004）。归档/done 从这里消失，
/// 但它们的历史仍在完整列表里。
#[tauri::command]
pub async fn list_selectable_projects(
    state: State<'_, RunningApp>,
    request: EpochRequest,
) -> Result<catalog::ProjectList, ErrorResponse> {
    run_command(&state, Vec::new(), move |app| {
        catalog::list_selectable_projects(app.db(), &request.expected_data_epoch)
    })
    .await
}

/// 新建项目。同名项目允许存在（schema 没有唯一索引，F-004 也没要求）。
#[tauri::command]
pub async fn create_project(
    state: State<'_, RunningApp>,
    request: CreateProjectRequest,
) -> Result<catalog::ProjectChange, ErrorResponse> {
    run_command(&state, Vec::new(), move |app| {
        let now = app.now_ms()?;
        let env = WriteEnvelope::for_create(request.expected_data_epoch);
        let change = catalog::create_project(app.db_mut(), env, &request.name, now)?.into_value();
        Ok(change)
    })
    .await
}

/// 重命名项目。改成同名 ⇒ 幂等：不写库、不加 `revision`，返回当前行。
#[tauri::command]
pub async fn rename_project(
    state: State<'_, RunningApp>,
    request: RenameProjectRequest,
) -> Result<catalog::ProjectChange, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Project, &request.project_id)];
    run_command(&state, targets, move |app| {
        let now = app.now_ms()?;
        let env =
            WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
        let change =
            catalog::rename_project(app.db_mut(), env, &request.project_id, &request.name, now)?
                .into_value();
        Ok(change)
    })
    .await
}

/// 归档项目（F-004）。已归档 ⇒ 幂等。
#[tauri::command]
pub async fn archive_project(
    state: State<'_, RunningApp>,
    request: ArchiveProjectRequest,
) -> Result<catalog::ProjectChange, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Project, &request.project_id)];
    run_command(&state, targets, move |app| {
        let now = app.now_ms()?;
        let env =
            WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
        let change =
            catalog::archive_project(app.db_mut(), env, &request.project_id, now)?.into_value();
        Ok(change)
    })
    .await
}

// ─────────────────────────────────────────────────────────────────────────────
// 标签（F-005）
// ─────────────────────────────────────────────────────────────────────────────

/// 标签选择器的数据源：全部标签，可按 `kind` 过滤（四类各一组）。
#[tauri::command]
pub async fn list_tags(
    state: State<'_, RunningApp>,
    request: ListTagsRequest,
) -> Result<catalog::TagList, ErrorResponse> {
    run_command(&state, Vec::new(), move |app| {
        let kind = match request.kind.as_deref() {
            Some(raw) => Some(catalog::parse_tag_kind(raw)?),
            None => None,
        };
        catalog::list_tags(app.db(), &request.expected_data_epoch, kind)
    })
    .await
}

/// 新建标签。
#[tauri::command]
pub async fn create_tag(
    state: State<'_, RunningApp>,
    request: CreateTagRequest,
) -> Result<catalog::TagChange, ErrorResponse> {
    run_command(&state, Vec::new(), move |app| {
        let now = app.now_ms()?;
        let env = WriteEnvelope::for_create(request.expected_data_epoch);
        let change = catalog::create_tag(
            app.db_mut(),
            env,
            &request.kind,
            &request.name,
            request.parent_id.as_deref(),
            now,
        )?
        .into_value();
        Ok(change)
    })
    .await
}

/// 某个任务身上的标签。写路径（打标/去标）不用它——它们在同一个写事务里读回集合。
#[tauri::command]
pub async fn tags_of_task(
    state: State<'_, RunningApp>,
    request: TaskTagsRequest,
) -> Result<catalog::TagList, ErrorResponse> {
    run_command(&state, Vec::new(), move |app| {
        catalog::tags_of_task(app.db(), &request.expected_data_epoch, &request.task_id)
    })
    .await
}

/// 打标：把**一个**标签加到**一个**任务上。
///
/// 信封只带 epoch：`task_tag` 没有版本列，打标也不改任何实体的字段，
/// 所以没有可校验的实体版本（裁决 R-T3-i）。重复打标 ⇒ 幂等。
#[tauri::command]
pub async fn tag_task(
    state: State<'_, RunningApp>,
    request: TaskTagRequest,
) -> Result<catalog::TaskTagsChange, ErrorResponse> {
    let targets = vec![
        target(AuthorityKind::Task, &request.task_id),
        target(AuthorityKind::Tag, &request.tag_id),
    ];
    run_command(&state, targets, move |app| {
        let now = app.now_ms()?;
        let env = WriteEnvelope::for_create(request.expected_data_epoch);
        let change = catalog::tag_task(app.db_mut(), env, &request.task_id, &request.tag_id, now)?
            .into_value();
        Ok(change)
    })
    .await
}

/// 去标。口径与 [`tag_task`] 完全对称，包括「本来就不在集合里 ⇒ 幂等」。
#[tauri::command]
pub async fn untag_task(
    state: State<'_, RunningApp>,
    request: TaskTagRequest,
) -> Result<catalog::TaskTagsChange, ErrorResponse> {
    let targets = vec![
        target(AuthorityKind::Task, &request.task_id),
        target(AuthorityKind::Tag, &request.tag_id),
    ];
    run_command(&state, targets, move |app| {
        let now = app.now_ms()?;
        let env = WriteEnvelope::for_create(request.expected_data_epoch);
        let change =
            catalog::untag_task(app.db_mut(), env, &request.task_id, &request.tag_id, now)?
                .into_value();
        Ok(change)
    })
    .await
}

// ─────────────────────────────────────────────────────────────────────────────
// 任务（F-002）
// ─────────────────────────────────────────────────────────────────────────────

/// 任务筛选查询（轻量 GTD 列表）。
///
/// 收的是服务层的 IPC 请求 DTO（[`catalog::TaskQueryRequest`]）：
/// 状态串、三值项目选择器与分页的校验都在它的 `TryFrom` 里，命令层只转发。
#[tauri::command]
pub async fn list_tasks(
    state: State<'_, RunningApp>,
    request: catalog::TaskQueryRequest,
) -> Result<catalog::TaskQueryResult, ErrorResponse> {
    run_command(&state, Vec::new(), move |app| {
        let query = catalog::TaskQuery::try_from(request)?;
        catalog::list_tasks_filtered(app.db(), query)
    })
    .await
}

/// 捕获一个任务（F-002 的 Inbox 入口）。空标题被服务拒绝。
#[tauri::command]
pub async fn create_task(
    state: State<'_, RunningApp>,
    request: CreateTaskRequest,
) -> Result<catalog::TaskChange, ErrorResponse> {
    run_command(&state, Vec::new(), move |app| {
        let now = app.now_ms()?;
        let env = WriteEnvelope::for_create(request.expected_data_epoch);
        let change = catalog::create_task(
            app.db_mut(),
            env,
            &request.title,
            request.project_id.as_deref(),
            now,
        )?
        .into_value();
        Ok(change)
    })
    .await
}

/// 理清为待办（F-002）。只接受没有在计时的 `Inbox` / `Clarifying`（P3 的状态编排
/// 归 P3，不从这个入口进来）。
#[tauri::command]
pub async fn clarify_ready(
    state: State<'_, RunningApp>,
    request: ClarifyReadyRequest,
) -> Result<catalog::TaskChange, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Task, &request.task_id)];
    run_command(&state, targets, move |app| {
        let now = app.now_ms()?;
        let env =
            WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
        let change = catalog::clarify_ready(app.db_mut(), env, &request.task_id, now)?;
        Ok(change)
    })
    .await
}

/// 改任务的归属（绑定到 active 项目 / 解除关联）。同值 ⇒ 幂等。
#[tauri::command]
pub async fn set_task_project(
    state: State<'_, RunningApp>,
    request: SetTaskProjectRequest,
) -> Result<catalog::TaskProjectChange, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Task, &request.task_id)];
    run_command(&state, targets, move |app| {
        let now = app.now_ms()?;
        let env =
            WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
        let change =
            catalog::set_task_project(app.db_mut(), env, &request.task_id, request.project, now)?
                .into_value();
        Ok(change)
    })
    .await
}

// ─────────────────────────────────────────────────────────────────────────────
// 今日计划（F-010 的「今日选择」半边）
// ─────────────────────────────────────────────────────────────────────────────

/// 读某一天（某个时区）的今日选择列表。日期与时区都在服务入口过唯一校验。
#[tauri::command]
pub async fn plan_for(
    state: State<'_, RunningApp>,
    request: daily_plan::DailyPlanQuery,
) -> Result<daily_plan::DailyPlanView, ErrorResponse> {
    let targets = Vec::new();
    run_command(&state, targets, move |app| {
        daily_plan::plan_for(app.db(), request)
    })
    .await
}

/// 把一个任务加入今日计划。重复加入 ⇒ 幂等。
#[tauri::command]
pub async fn add_to_plan(
    state: State<'_, RunningApp>,
    request: PlanMutationRequest,
) -> Result<daily_plan::DailyPlanChange, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Task, &request.task_id)];
    run_command(&state, targets, move |app| {
        let now = app.now_ms()?;
        let env = WriteEnvelope::for_create(request.expected_data_epoch);
        let change = daily_plan::add_to_plan(
            app.db_mut(),
            env,
            &request.task_id,
            &request.date,
            &request.timezone,
            now,
        )?
        .into_value();
        Ok(change)
    })
    .await
}

/// 把一个任务从今日计划里移除。口径与 [`add_to_plan`] 对称。
#[tauri::command]
pub async fn remove_from_plan(
    state: State<'_, RunningApp>,
    request: PlanMutationRequest,
) -> Result<daily_plan::DailyPlanChange, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Task, &request.task_id)];
    run_command(&state, targets, move |app| {
        let now = app.now_ms()?;
        let env = WriteEnvelope::for_create(request.expected_data_epoch);
        let change = daily_plan::remove_from_plan(
            app.db_mut(),
            env,
            &request.task_id,
            &request.date,
            &request.timezone,
            now,
        )?
        .into_value();
        Ok(change)
    })
    .await
}

// ─────────────────────────────────────────────────────────────────────────────
// 计时（P2 协调器）
// ─────────────────────────────────────────────────────────────────────────────

/// 计时快照：当前任务 + 运行中的实时暂计（P7 的 Today 用它取计时半边）。
///
/// 它自己也取一次采样（检测与展示值来自同一次采样），所以是一条**查询命令**，
/// 不是纯读。
#[tauri::command]
pub async fn timer_snapshot(state: State<'_, RunningApp>) -> Result<TimerSnapshot, ErrorResponse> {
    run_command(&state, Vec::new(), |app| app.snapshot()).await
}

/// 推进一步：与快照同形，另外让 `tick_seq` 前进一步（前端据此丢弃旧 tick）。
#[tauri::command]
pub async fn timer_tick(state: State<'_, RunningApp>) -> Result<TimerSnapshot, ErrorResponse> {
    run_command(&state, Vec::new(), |app| app.tick()).await
}

/// 开始计时。开始新计时要过恢复门禁（有别的 run 的未闭合/待确认记录 ⇒
/// `RECOVERY_REQUIRED`）；查询不受影响。
#[tauri::command]
pub async fn start_timer(
    state: State<'_, RunningApp>,
    request: StartTimerRequest,
) -> Result<CommandOutcome, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Task, &request.task_id)];
    run_command(&state, targets, move |app| {
        let req = StartRequest {
            expected_data_epoch: request.expected_data_epoch,
            task_id: request.task_id.clone(),
            task_expected_version: request.task_expected_version,
            mode: parse_session_mode(&request.mode)?,
            timer_kind: parse_timer_kind(&request.timer_kind)?,
            target_duration_ms: request.target_duration_ms,
            expected_interval_ms: request.expected_interval_ms,
        };
        app.start(req)
    })
    .await
}

/// 暂停（会话与会话版本由快照给出）。暂停值冻结，不在前端算。
#[tauri::command]
pub async fn pause_timer(
    state: State<'_, RunningApp>,
    request: SessionRequest,
) -> Result<CommandOutcome, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Session, &request.session_id)];
    run_command(&state, targets, move |app| app.pause(request)).await
}

/// 继续计时。**两份版本**：任务与会话各自有自己的并发版本。
#[tauri::command]
pub async fn resume_timer(
    state: State<'_, RunningApp>,
    request: ResumeRequest,
) -> Result<CommandOutcome, ErrorResponse> {
    let targets = vec![
        target(AuthorityKind::Task, &request.task_id),
        target(AuthorityKind::Session, &request.session_id),
    ];
    run_command(&state, targets, move |app| app.resume(request)).await
}

/// 结束计时。到点只提示、不自动完成（F-003 的完整联动归 P8）。
#[tauri::command]
pub async fn finish_timer(
    state: State<'_, RunningApp>,
    request: SessionRequest,
) -> Result<CommandOutcome, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Session, &request.session_id)];
    run_command(&state, targets, move |app| app.finish(request)).await
}
