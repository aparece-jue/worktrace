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
//! # 错误：六个码原样透传
//!
//! 失败统一走 [`capture_error_response`]（**在原事务结束之后、同一串行边界内**捕获，
//! 不另调 `timer.snapshot` 补版本），载荷是共享的
//! [`ErrorResponse`]（`code`/`message`/`authority`/`requires_handshake`）。
//! 前端只按 `code` 分支，`message` 直接用（R8）。
//!
//! # 命令体与包装分开（P7 Task 1 fix round 1，评审 I6）
//!
//! 每条命令都是**一对**：`#[tauri::command] pub async fn x(…)`（IPC 包装：取锁、
//! 跑阻塞段、把错误映射成 [`ErrorResponse`]）与 `pub fn x_impl(…)`（命令体：解析请求 →
//! 调服务 → 返回响应）。包装只有一行转发。
//!
//! 为什么分开：`#[tauri::command]` 生成的包装要 Tauri 运行时才能调，而命令体只需要一个
//! `&mut AppState`——分开之后 `tests/ipc_commands.rs` 能**逐条**覆盖 24 条命令
//! （不需要 `tauri::test`，因此也不需要动 `Cargo.toml`）。一个 `finish_timer` 里误调
//! `app.pause` 的复制粘贴错误，现在会当场断言失败。
//!
//! # 请求形状：枚举一律是字符串
//!
//! `mode` / `timer_kind` / `statuses` 在 IPC 里都是**字符串**，命令体显式过
//! [`parse_session_mode`] / [`parse_timer_kind`] / `TaskStatus::parse`
//! （后者在 `services::catalog::TaskQueryRequest` 的 `TryFrom` 里），**不依赖 serde
//! 的枚举反序列化**：后者的失败拿不到 `ErrorResponse.code`，会退化成 Tauri 的
//! 反序列化错误（00 §4 只认那六个码）。
//!
//! 每个命令只收**一个** `request` 参数（请求 DTO 见下）：这样 IPC 的参数名不受
//! Tauri 的 `camelCase` 参数重命名影响，字段名就是本文件里写的那套 snake_case。
//!
//! # 写命令广播 `domain.changed`（P7 Task 1 fix round 1）
//!
//! 00 §5：**同一 epoch 内一次业务写对应一条 `domain.changed`**。落点在这里——
//! 命令体拿到写结果后、**释放锁之前**调 [`announce`]：
//!
//! - **仅 `Changed` 才广播**：[`crate::storage::WriteOutcome::into_parts`] 的第二个返回值；
//!   `Unchanged`（改同名、重复打标、重复加入计划）不广播——没有 revision 变化就没有
//!   缓存要失效；
//! - **载荷就是该命令的响应 DTO**，不另造形状；
//! - **响应形状不变**：不加 `{changed, value}` 信封（那会改 15 份快照，而且计划没写），
//!   这一位只用于「要不要广播」这个内部判断；
//! - **失败只记诊断**：`Broadcaster::emit` 不返回错误，命令照常成功、已提交业务不回滚。
//!
//! 计时命令（`start`/`pause`/`resume`/`finish`）没有「幂等重复」这一支：能走到广播
//! 就说明这次状态跃迁真的提交了，所以它们的 `changed` 恒为真。
//!
//! # 托盘动作（P7 Task 4）
//!
//! 托盘菜单点到的动作走**与 IPC 相同的命令体**：暂停 = [`tray_pause_impl`]（内部就是
//! [`pause_timer_impl`]），退出 = [`tray_quit_impl`]（内部就是 Task 0 的显式退出入口
//! `RunningApp::shutdown`）。菜单本身的装配在 `platform::tray`（那一层不碰业务），
//! 组合根 `lib.rs` 把动作接到这两个入口上。
//!
//! 与 IPC 的一点差别：托盘**没有响应通道**，所以 `spawn_tray_*` 在阻塞线程里执行完
//! 只把结果写进诊断。串行边界与 IPC 完全相同——同一把
//! `Mutex<AppState>`、同样不在 UI 回调里开事务。
//!
//! **托盘绕过 [`run_command`]**（P6 Task 2a，计划 fix round 4 的 C-2）：所以
//! `guard_writable` 挡不住它们，两条路径各自判维护态——暂停在
//! [`tray_pause_impl`] 取锁之后先过门禁（拒绝即返回，不写任何东西）；退出走
//! `RunningApp::shutdown` 里的 `AppState::begin_exit`（维护态下拒绝，
//! **采样线程仍在跑**、进程也不退出）。两条拒绝都落到正式诊断日志
//! （release 的 Windows 子系统没有控制台，`eprintln!` 没人看得见）。
//!
//! # dev 注入开关（P7 Task 6a，**只在 debug 构建存在**）
//!
//! [`dev`] 是真实双窗口实验用的四条 dev 命令（丢一条通知 / 延迟一次响应 /
//! 旧 revision 重播 / 开实验窗口），整份模块带 `#[cfg(debug_assertions)]`；
//! `lib.rs` 的注册表里那四条也**逐条**带守卫。发布构建里它们不是「被关掉」，
//! 是**不存在**（守卫与核对见 `dev` 的模块头与 `tests/dev_injections.rs`）。
//!
//! 它对命令层唯一的侵入是 [`run_command`] 的前两个参数：命令名与**调用方窗口**
//! label。注入 (b) 按这两个键决定要不要把这一次的响应推迟返回——只按命令名分的话，
//! 装在 B 上的开关会被 A 的重拉先吃掉（`manual-sync.md` §2.2），所以两个键都必须
//! 与真实调用一致（`tests/dev_injections.rs` 核对）。
//!
//! # 本阶段不做
//!
//! 恢复确认相关命令（P3）；统计与导出命令（P5，P8 接入）；恢复流程本身与装卸运行态
//! （P6 Task 4b）。维护态的新错误码 `DATA_RESTORE_IN_PROGRESS` 与四处联动**已落地**
//! （P6 Task 4a），被拒响应的形状见 [`maintenance_response`]。

use std::sync::Arc;

use tauri::{AppHandle, Manager, State};

use crate::envelope::WriteEnvelope;
use crate::error::{AppError, AuthorityKind, AuthorityTarget, ErrorResponse};
use crate::services::bootstrap::{
    is_maintenance_refusal, lock_app, AppState, ExitReport, RunningApp,
};
use crate::services::error_response::capture_error_response;
use crate::services::events::{Broadcaster, EventEnvelope};
use crate::services::timer::coordinator::{
    parse_session_mode, parse_timer_kind, CommandOutcome, ResumeRequest, SessionRequest,
    StartRequest,
};
use crate::services::timer::snapshot::TimerSnapshot;
use crate::services::{catalog, daily_plan, handshake};

/// 实验器材（P7 Task 6a）：四条 dev 命令，**只在 debug 构建编译**。
#[cfg(debug_assertions)]
pub mod dev;

// ─────────────────────────────────────────────────────────────────────────────
// 命令体的执行骨架
// ─────────────────────────────────────────────────────────────────────────────

/// 在**阻塞线程**上、**串行边界内**执行一次命令体，并统一映射错误。
///
/// `command` 与 `window` 是这次调用的身份：命令名（与 `lib.rs` 注册表逐字一致）与
/// **调用方窗口 label**。dev 注入 (b) 按这两个键决定要不要把这一次的**响应**推迟返回
/// （P7 Task 6a，只在 debug 构建有作用）——按窗口分是必须的：一次 `domain.changed`
/// 之后每个窗口都会重拉同一条查询，不按窗口分的话装在 B 上的开关会被 A 的重拉先吃掉，
/// 实验结论就取决于两个 WebView 谁先跑（`manual-sync.md` §2.2）。
///
/// `targets` 是这次请求涉及的受控实体（错误上下文用，见
/// [`capture_error_response`]）。它在 `body` 失败**之后**、**仍持有同一把锁**时被使用：
/// 权威 `epoch`/`revision`/目标版本因此出自同一次读事务，不会互相矛盾。
async fn run_command<T, F>(
    command: &'static str,
    window: &str,
    state: &State<'_, RunningApp>,
    targets: Vec<AuthorityTarget>,
    body: F,
) -> Result<T, ErrorResponse>
where
    T: Send + 'static,
    F: FnOnce(&mut AppState) -> Result<T, AppError> + Send + 'static,
{
    #[cfg(debug_assertions)]
    let probe = dev::CommandProbe::start(command, window);
    let app = Arc::clone(state.app());
    let result = match tauri::async_runtime::spawn_blocking(move || {
        let mut guard = lock_app(&app);
        // **维护态门禁**（P6 Task 2a）——`guard_writable` 的唯一调用点：取锁之后、
        // 命令体之前。维护态期间**全部命令一律拒绝**（含只读）：此刻运行态正要被换掉，
        // 放行只读命令只会让它们报出与"正在恢复"无关的错误，或者读到占位状态。
        //
        // 恢复流程自己的 ②③ 两段**不重新进这里**（它在同一次后台调用里连续调服务原语），
        // 所以白名单里**没有任何 IPC 命令名**。
        if let Err(error) = guard.guard_writable() {
            return Err(maintenance_response(&error));
        }
        body(&mut guard).map_err(|error| match guard.db() {
            Ok(db) => capture_error_response(db, &error, &targets),
            // **运行态不在手**（维护态的换库窗口）：错误路径**不能再读库**——
            // 拿不到权威版本就不编一个，按维护态的形状回答（`authority: None`）。
            // 这条分支今天不可达：上面的 `guard_writable` 已经先拒了维护态；
            // 留着它是为了让「取不到库」不会退化成 panic 或假版本。
            Err(_) => maintenance_response(&AppError::DataRestoreInProgress),
        })
    })
    .await
    {
        Ok(result) => result,
        Err(join) => Err(internal_failure(join.to_string())),
    };

    // dev 注入 (b)（P7 Task 6a）：上面那次阻塞调用**已经取完数据、放开锁**，
    // 这里只把响应推迟返回（`manual-sync.md` §2.2 要的「先取数据再 sleep」）。
    // 发布构建里 `commands::dev` 整份不存在，这两行也随之不编译。
    #[cfg(debug_assertions)]
    {
        probe.record("body_complete", Some(result.is_ok()));
        dev::delay_response_if_armed(window, command).await;
        probe.record("return", Some(result.is_ok()));
    }
    #[cfg(not(debug_assertions))]
    let _ = (command, window);

    result
}

/// 阻塞任务 panic / 被取消：这是**缺陷**，不是用户错误，也不是业务失败。
///
/// 六个码里没有「内部错误」，取最接近的 `STORAGE_ERROR`。**刻意不带 `authority`**：
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

/// 维护态拒绝的响应形状（计划 fix round 2 的 I1）：**不读库** ⇒ 没有 `authority`，
/// 也不要求重新握手（库身份没变，重新握手由维护结束后的新 `data_epoch` 触发）。
///
/// 为什么不能走 [`capture_error_response`]：它要读 `guard.db()` 才知道权威版本，
/// 而维护态期间**运行态正要被换掉**（Task 4b 的 `take_runtime`），那一刻既读不到、
/// 也不该在错误路径上开事务。
///
/// `code`/`message` 来自 [`AppError::DataRestoreInProgress`]（第六个码
/// `DATA_RESTORE_IN_PROGRESS`，P6 Task 4a 落地）：码是前端的分支键，文案是面向用户的
/// 中文，两者都由 `error.rs` 给出——这里**不重抄一份**，免得又多一处漂移源。
fn maintenance_response(error: &AppError) -> ErrorResponse {
    ErrorResponse {
        code: error.code().to_owned(),
        message: error.message(),
        authority: None,
        requires_handshake: false,
    }
}

/// 请求涉及的受控实体。`kind` 是白名单枚举，`id` 原样回显
/// （记录已被删除时，回显是它唯一还能对上的身份）。
fn target(kind: AuthorityKind, id: &str) -> AuthorityTarget {
    AuthorityTarget::new(kind, id.to_string())
}

/// 提交之后广播一条 `domain.changed`，并把载荷原样交回调用方。
///
/// 判据、位置与失败口径见模块头「写命令广播 `domain.changed`」一节。
/// `payload` 序列化失败**不是**业务失败：降级成 `null` 载荷，信封里的
/// `data_epoch`/`revision` 仍然是对的——客户端使缓存失效只看那两个字段。
fn announce<T: serde::Serialize>(
    broadcaster: &Broadcaster,
    changed: bool,
    data_epoch: String,
    revision: i64,
    at: i64,
    value: T,
) -> T {
    if changed {
        let payload = serde_json::to_value(&value).unwrap_or(serde_json::Value::Null);
        broadcaster.emit(EventEnvelope::domain_changed(
            data_epoch, revision, at, payload,
        ));
    }
    value
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
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
) -> Result<handshake::RevisionSnapshot, ErrorResponse> {
    run_command(
        "get_revision",
        window.label(),
        &state,
        Vec::new(),
        get_revision_impl,
    )
    .await
}

/// [`get_revision`] 的命令体（IPC 包装只做转发）。
pub fn get_revision_impl(app: &mut AppState) -> Result<handshake::RevisionSnapshot, AppError> {
    handshake::get_revision(app.db()?)
}

// ─────────────────────────────────────────────────────────────────────────────
// 项目（F-004）
// ─────────────────────────────────────────────────────────────────────────────

/// 完整项目列表：`status = null` 时含归档与 `done` 的历史（Task 5 的列表用）。
#[tauri::command]
pub async fn list_projects(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: ListProjectsRequest,
) -> Result<catalog::ProjectList, ErrorResponse> {
    run_command(
        "list_projects",
        window.label(),
        &state,
        Vec::new(),
        move |app| list_projects_impl(app, request),
    )
    .await
}

/// [`list_projects`] 的命令体（IPC 包装只做转发）。
pub fn list_projects_impl(
    app: &mut AppState,
    request: ListProjectsRequest,
) -> Result<catalog::ProjectList, AppError> {
    let status = match request.status.as_deref() {
        // 读路径：`done` 是库里合法的值，这里必须读得懂（写路径才拒绝它）。
        Some(raw) => Some(catalog::parse_project_status_read(raw)?),
        None => None,
    };
    catalog::list_projects(app.db()?, &request.expected_data_epoch, status)
}

/// 新建任务时可选的项目：**只列 active**（F-004）。归档/done 从这里消失，
/// 但它们的历史仍在完整列表里。
#[tauri::command]
pub async fn list_selectable_projects(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: EpochRequest,
) -> Result<catalog::ProjectList, ErrorResponse> {
    run_command(
        "list_selectable_projects",
        window.label(),
        &state,
        Vec::new(),
        move |app| list_selectable_projects_impl(app, request),
    )
    .await
}

/// [`list_selectable_projects`] 的命令体（IPC 包装只做转发）。
pub fn list_selectable_projects_impl(
    app: &mut AppState,
    request: EpochRequest,
) -> Result<catalog::ProjectList, AppError> {
    catalog::list_selectable_projects(app.db()?, &request.expected_data_epoch)
}

/// 新建项目。同名项目允许存在（schema 没有唯一索引，F-004 也没要求）。
#[tauri::command]
pub async fn create_project(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: CreateProjectRequest,
) -> Result<catalog::ProjectChange, ErrorResponse> {
    let broadcaster = Arc::clone(state.broadcaster());
    run_command(
        "create_project",
        window.label(),
        &state,
        Vec::new(),
        move |app| create_project_impl(app, &broadcaster, request),
    )
    .await
}

/// [`create_project`] 的命令体（IPC 包装只做转发）。
pub fn create_project_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: CreateProjectRequest,
) -> Result<catalog::ProjectChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) =
        catalog::create_project(app.db_mut()?, env, &request.name, now)?.into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

/// 重命名项目。改成同名 ⇒ 幂等：不写库、不加 `revision`，返回当前行。
#[tauri::command]
pub async fn rename_project(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: RenameProjectRequest,
) -> Result<catalog::ProjectChange, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Project, &request.project_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command(
        "rename_project",
        window.label(),
        &state,
        targets,
        move |app| rename_project_impl(app, &broadcaster, request),
    )
    .await
}

/// [`rename_project`] 的命令体（IPC 包装只做转发）。
pub fn rename_project_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: RenameProjectRequest,
) -> Result<catalog::ProjectChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
    let (change, changed) =
        catalog::rename_project(app.db_mut()?, env, &request.project_id, &request.name, now)?
            .into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

/// 归档项目（F-004）。已归档 ⇒ 幂等。
#[tauri::command]
pub async fn archive_project(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: ArchiveProjectRequest,
) -> Result<catalog::ProjectChange, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Project, &request.project_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command(
        "archive_project",
        window.label(),
        &state,
        targets,
        move |app| archive_project_impl(app, &broadcaster, request),
    )
    .await
}

/// [`archive_project`] 的命令体（IPC 包装只做转发）。
pub fn archive_project_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: ArchiveProjectRequest,
) -> Result<catalog::ProjectChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
    let (change, changed) =
        catalog::archive_project(app.db_mut()?, env, &request.project_id, now)?.into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// 标签（F-005）
// ─────────────────────────────────────────────────────────────────────────────

/// 标签选择器的数据源：全部标签，可按 `kind` 过滤（四类各一组）。
#[tauri::command]
pub async fn list_tags(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: ListTagsRequest,
) -> Result<catalog::TagList, ErrorResponse> {
    run_command(
        "list_tags",
        window.label(),
        &state,
        Vec::new(),
        move |app| list_tags_impl(app, request),
    )
    .await
}

/// [`list_tags`] 的命令体（IPC 包装只做转发）。
pub fn list_tags_impl(
    app: &mut AppState,
    request: ListTagsRequest,
) -> Result<catalog::TagList, AppError> {
    let kind = match request.kind.as_deref() {
        Some(raw) => Some(catalog::parse_tag_kind(raw)?),
        None => None,
    };
    catalog::list_tags(app.db()?, &request.expected_data_epoch, kind)
}

/// 新建标签。
#[tauri::command]
pub async fn create_tag(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: CreateTagRequest,
) -> Result<catalog::TagChange, ErrorResponse> {
    let broadcaster = Arc::clone(state.broadcaster());
    run_command(
        "create_tag",
        window.label(),
        &state,
        Vec::new(),
        move |app| create_tag_impl(app, &broadcaster, request),
    )
    .await
}

/// [`create_tag`] 的命令体（IPC 包装只做转发）。
pub fn create_tag_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: CreateTagRequest,
) -> Result<catalog::TagChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) = catalog::create_tag(
        app.db_mut()?,
        env,
        &request.kind,
        &request.name,
        request.parent_id.as_deref(),
        now,
    )?
    .into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

/// 某个任务身上的标签。写路径（打标/去标）不用它——它们在同一个写事务里读回集合。
#[tauri::command]
pub async fn tags_of_task(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: TaskTagsRequest,
) -> Result<catalog::TagList, ErrorResponse> {
    run_command(
        "tags_of_task",
        window.label(),
        &state,
        Vec::new(),
        move |app| tags_of_task_impl(app, request),
    )
    .await
}

/// [`tags_of_task`] 的命令体（IPC 包装只做转发）。
pub fn tags_of_task_impl(
    app: &mut AppState,
    request: TaskTagsRequest,
) -> Result<catalog::TagList, AppError> {
    catalog::tags_of_task(app.db()?, &request.expected_data_epoch, &request.task_id)
}

/// 打标：把**一个**标签加到**一个**任务上。
///
/// 信封只带 epoch：`task_tag` 没有版本列，打标也不改任何实体的字段，
/// 所以没有可校验的实体版本（裁决 R-T3-i）。重复打标 ⇒ 幂等。
#[tauri::command]
pub async fn tag_task(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: TaskTagRequest,
) -> Result<catalog::TaskTagsChange, ErrorResponse> {
    let targets = vec![
        target(AuthorityKind::Task, &request.task_id),
        target(AuthorityKind::Tag, &request.tag_id),
    ];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command("tag_task", window.label(), &state, targets, move |app| {
        tag_task_impl(app, &broadcaster, request)
    })
    .await
}

/// [`tag_task`] 的命令体（IPC 包装只做转发）。
pub fn tag_task_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: TaskTagRequest,
) -> Result<catalog::TaskTagsChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) =
        catalog::tag_task(app.db_mut()?, env, &request.task_id, &request.tag_id, now)?.into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

/// 去标。口径与 [`tag_task`] 完全对称，包括「本来就不在集合里 ⇒ 幂等」。
#[tauri::command]
pub async fn untag_task(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: TaskTagRequest,
) -> Result<catalog::TaskTagsChange, ErrorResponse> {
    let targets = vec![
        target(AuthorityKind::Task, &request.task_id),
        target(AuthorityKind::Tag, &request.tag_id),
    ];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command("untag_task", window.label(), &state, targets, move |app| {
        untag_task_impl(app, &broadcaster, request)
    })
    .await
}

/// [`untag_task`] 的命令体（IPC 包装只做转发）。
pub fn untag_task_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: TaskTagRequest,
) -> Result<catalog::TaskTagsChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) =
        catalog::untag_task(app.db_mut()?, env, &request.task_id, &request.tag_id, now)?
            .into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
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
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: catalog::TaskQueryRequest,
) -> Result<catalog::TaskQueryResult, ErrorResponse> {
    run_command(
        "list_tasks",
        window.label(),
        &state,
        Vec::new(),
        move |app| list_tasks_impl(app, request),
    )
    .await
}

/// [`list_tasks`] 的命令体（IPC 包装只做转发）。
pub fn list_tasks_impl(
    app: &mut AppState,
    request: catalog::TaskQueryRequest,
) -> Result<catalog::TaskQueryResult, AppError> {
    let query = catalog::TaskQuery::try_from(request)?;
    catalog::list_tasks_filtered(app.db()?, query)
}

/// 捕获一个任务（F-002 的 Inbox 入口）。空标题被服务拒绝。
#[tauri::command]
pub async fn create_task(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: CreateTaskRequest,
) -> Result<catalog::TaskChange, ErrorResponse> {
    let broadcaster = Arc::clone(state.broadcaster());
    run_command(
        "create_task",
        window.label(),
        &state,
        Vec::new(),
        move |app| create_task_impl(app, &broadcaster, request),
    )
    .await
}

/// [`create_task`] 的命令体（IPC 包装只做转发）。
pub fn create_task_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: CreateTaskRequest,
) -> Result<catalog::TaskChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) = catalog::create_task(
        app.db_mut()?,
        env,
        &request.title,
        request.project_id.as_deref(),
        now,
    )?
    .into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

/// 理清为待办（F-002）。只接受没有在计时的 `Inbox` / `Clarifying`（P3 的状态编排
/// 归 P3，不从这个入口进来）。
#[tauri::command]
pub async fn clarify_ready(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: ClarifyReadyRequest,
) -> Result<catalog::TaskChange, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Task, &request.task_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command(
        "clarify_ready",
        window.label(),
        &state,
        targets,
        move |app| clarify_ready_impl(app, &broadcaster, request),
    )
    .await
}

/// [`clarify_ready`] 的命令体（IPC 包装只做转发）。
pub fn clarify_ready_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: ClarifyReadyRequest,
) -> Result<catalog::TaskChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
    let change = catalog::clarify_ready(app.db_mut()?, env, &request.task_id, now)?;
    // `services::catalog::clarify_ready` 没有 `Unchanged` 分支：它只在真的跃迁时成功，
    // 所以这一次业务写必然改了库。
    Ok(announce(
        broadcaster,
        true,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

/// 改任务的归属（绑定到 active 项目 / 解除关联）。同值 ⇒ 幂等。
#[tauri::command]
pub async fn set_task_project(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: SetTaskProjectRequest,
) -> Result<catalog::TaskProjectChange, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Task, &request.task_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command(
        "set_task_project",
        window.label(),
        &state,
        targets,
        move |app| set_task_project_impl(app, &broadcaster, request),
    )
    .await
}

/// [`set_task_project`] 的命令体（IPC 包装只做转发）。
pub fn set_task_project_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: SetTaskProjectRequest,
) -> Result<catalog::TaskProjectChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_update(request.expected_data_epoch, request.expected_row_version);
    let (change, changed) =
        catalog::set_task_project(app.db_mut()?, env, &request.task_id, request.project, now)?
            .into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// 今日计划（F-010 的「今日选择」半边）
// ─────────────────────────────────────────────────────────────────────────────

/// 读某一天（某个时区）的今日选择列表。日期与时区都在服务入口过唯一校验。
#[tauri::command]
pub async fn plan_for(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: daily_plan::DailyPlanQuery,
) -> Result<daily_plan::DailyPlanView, ErrorResponse> {
    let targets = Vec::new();
    run_command("plan_for", window.label(), &state, targets, move |app| {
        plan_for_impl(app, request)
    })
    .await
}

/// [`plan_for`] 的命令体（IPC 包装只做转发）。
pub fn plan_for_impl(
    app: &mut AppState,
    request: daily_plan::DailyPlanQuery,
) -> Result<daily_plan::DailyPlanView, AppError> {
    daily_plan::plan_for(app.db()?, request)
}

/// 把一个任务加入今日计划。重复加入 ⇒ 幂等。
#[tauri::command]
pub async fn add_to_plan(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: PlanMutationRequest,
) -> Result<daily_plan::DailyPlanChange, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Task, &request.task_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command("add_to_plan", window.label(), &state, targets, move |app| {
        add_to_plan_impl(app, &broadcaster, request)
    })
    .await
}

/// [`add_to_plan`] 的命令体（IPC 包装只做转发）。
pub fn add_to_plan_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: PlanMutationRequest,
) -> Result<daily_plan::DailyPlanChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) = daily_plan::add_to_plan(
        app.db_mut()?,
        env,
        &request.task_id,
        &request.date,
        &request.timezone,
        now,
    )?
    .into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

/// 把一个任务从今日计划里移除。口径与 [`add_to_plan`] 对称。
#[tauri::command]
pub async fn remove_from_plan(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: PlanMutationRequest,
) -> Result<daily_plan::DailyPlanChange, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Task, &request.task_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command(
        "remove_from_plan",
        window.label(),
        &state,
        targets,
        move |app| remove_from_plan_impl(app, &broadcaster, request),
    )
    .await
}

/// [`remove_from_plan`] 的命令体（IPC 包装只做转发）。
pub fn remove_from_plan_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: PlanMutationRequest,
) -> Result<daily_plan::DailyPlanChange, AppError> {
    let now = app.now_ms()?;
    let env = WriteEnvelope::for_create(request.expected_data_epoch);
    let (change, changed) = daily_plan::remove_from_plan(
        app.db_mut()?,
        env,
        &request.task_id,
        &request.date,
        &request.timezone,
        now,
    )?
    .into_parts();
    Ok(announce(
        broadcaster,
        changed,
        change.data_epoch.clone(),
        change.revision,
        now,
        change,
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// 计时（P2 协调器）
// ─────────────────────────────────────────────────────────────────────────────

/// 计时快照：当前任务 + 运行中的实时暂计（P7 的 Today 用它取计时半边）。
///
/// 它自己也取一次采样（检测与展示值来自同一次采样），所以是一条**查询命令**，
/// 不是纯读。
#[tauri::command]
pub async fn timer_snapshot(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
) -> Result<TimerSnapshot, ErrorResponse> {
    run_command(
        "timer_snapshot",
        window.label(),
        &state,
        Vec::new(),
        timer_snapshot_impl,
    )
    .await
}

/// [`timer_snapshot`] 的命令体（IPC 包装只做转发）。
pub fn timer_snapshot_impl(app: &mut AppState) -> Result<TimerSnapshot, AppError> {
    app.snapshot()
}

/// 推进一步：与快照同形，另外让 `tick_seq` 前进一步（前端据此丢弃旧 tick）。
#[tauri::command]
pub async fn timer_tick(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
) -> Result<TimerSnapshot, ErrorResponse> {
    run_command(
        "timer_tick",
        window.label(),
        &state,
        Vec::new(),
        timer_tick_impl,
    )
    .await
}

/// [`timer_tick`] 的命令体（IPC 包装只做转发）。
pub fn timer_tick_impl(app: &mut AppState) -> Result<TimerSnapshot, AppError> {
    app.tick()
}

/// 开始计时。开始新计时要过恢复门禁（有别的 run 的未闭合/待确认记录 ⇒
/// `RECOVERY_REQUIRED`）；查询不受影响。
#[tauri::command]
pub async fn start_timer(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: StartTimerRequest,
) -> Result<CommandOutcome, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Task, &request.task_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command("start_timer", window.label(), &state, targets, move |app| {
        start_timer_impl(app, &broadcaster, request)
    })
    .await
}

/// [`start_timer`] 的命令体（IPC 包装只做转发）。
pub fn start_timer_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: StartTimerRequest,
) -> Result<CommandOutcome, AppError> {
    let req = StartRequest {
        expected_data_epoch: request.expected_data_epoch,
        task_id: request.task_id.clone(),
        task_expected_version: request.task_expected_version,
        mode: parse_session_mode(&request.mode)?,
        timer_kind: parse_timer_kind(&request.timer_kind)?,
        target_duration_ms: request.target_duration_ms,
        expected_interval_ms: request.expected_interval_ms,
    };
    // 计时命令没有「幂等重复」这一支：能走到这里就说明这次状态跃迁真的提交了
    // （被拒的命令在服务层就返回错误，不会广播）。
    let outcome = app.start(req)?;
    Ok(announce(
        broadcaster,
        true,
        outcome.snapshot.data_epoch.clone(),
        outcome.revision,
        outcome.snapshot.as_of,
        outcome,
    ))
}

/// 暂停（会话与会话版本由快照给出）。暂停值冻结，不在前端算。
#[tauri::command]
pub async fn pause_timer(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: SessionRequest,
) -> Result<CommandOutcome, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Session, &request.session_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command("pause_timer", window.label(), &state, targets, move |app| {
        pause_timer_impl(app, &broadcaster, request)
    })
    .await
}

/// [`pause_timer`] 的命令体（IPC 包装只做转发）。
pub fn pause_timer_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: SessionRequest,
) -> Result<CommandOutcome, AppError> {
    // 计时命令没有「幂等重复」这一支：能走到这里就说明这次状态跃迁真的提交了
    // （被拒的命令在服务层就返回错误，不会广播）。
    let outcome = app.pause(request)?;
    Ok(announce(
        broadcaster,
        true,
        outcome.snapshot.data_epoch.clone(),
        outcome.revision,
        outcome.snapshot.as_of,
        outcome,
    ))
}

/// 继续计时。**两份版本**：任务与会话各自有自己的并发版本。
#[tauri::command]
pub async fn resume_timer(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: ResumeRequest,
) -> Result<CommandOutcome, ErrorResponse> {
    let targets = vec![
        target(AuthorityKind::Task, &request.task_id),
        target(AuthorityKind::Session, &request.session_id),
    ];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command(
        "resume_timer",
        window.label(),
        &state,
        targets,
        move |app| resume_timer_impl(app, &broadcaster, request),
    )
    .await
}

/// [`resume_timer`] 的命令体（IPC 包装只做转发）。
pub fn resume_timer_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: ResumeRequest,
) -> Result<CommandOutcome, AppError> {
    // 计时命令没有「幂等重复」这一支：能走到这里就说明这次状态跃迁真的提交了
    // （被拒的命令在服务层就返回错误，不会广播）。
    let outcome = app.resume(request)?;
    Ok(announce(
        broadcaster,
        true,
        outcome.snapshot.data_epoch.clone(),
        outcome.revision,
        outcome.snapshot.as_of,
        outcome,
    ))
}

/// 结束计时。到点只提示、不自动完成（F-003 的完整联动归 P8）。
#[tauri::command]
pub async fn finish_timer(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: SessionRequest,
) -> Result<CommandOutcome, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Session, &request.session_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command(
        "finish_timer",
        window.label(),
        &state,
        targets,
        move |app| finish_timer_impl(app, &broadcaster, request),
    )
    .await
}

/// [`finish_timer`] 的命令体（IPC 包装只做转发）。
pub fn finish_timer_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    request: SessionRequest,
) -> Result<CommandOutcome, AppError> {
    // 计时命令没有「幂等重复」这一支：能走到这里就说明这次状态跃迁真的提交了
    // （被拒的命令在服务层就返回错误，不会广播）。
    let outcome = app.finish(request)?;
    Ok(announce(
        broadcaster,
        true,
        outcome.snapshot.data_epoch.clone(),
        outcome.revision,
        outcome.snapshot.as_of,
        outcome,
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// 托盘动作（P7 Task 4）：**复用与 IPC 相同的命令体/服务入口**，不另开业务路径
// ─────────────────────────────────────────────────────────────────────────────
//
// 菜单本身的装配在 `platform::tray`：那一层只把「点到了什么」交出来，不碰业务
// （分层门禁也不允许它反向引用这一层）。这里补齐另一半——「动作 → 命令体」，
// 由组合根 `lib.rs` 接上。
//
// 托盘**没有响应通道**，所以结果只进诊断；但执行姿势与 IPC 逐条相同：切到阻塞线程
// （菜单回调跑在 UI 线程上，长事务不得留在那里，D6 硬约束 2）、取同一把
// `Mutex<AppState>`（于是「用户命令」与「托盘动作」不可能并发进入协调器）。

/// 托盘「暂停」的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayPause {
    /// 没有**运行中**的会话：什么都没做（不写库、不加 revision、不广播）。
    NothingToPause,
    /// 复用 [`pause_timer_impl`] 暂停了当前会话。
    Paused(Box<CommandOutcome>),
}

/// 托盘「暂停」：暂停**当前正在跑的那个会话**。
///
/// 会话 id 与会话版本取自**刚取到的快照**，与界面点「暂停」用的是同一组字段
/// （界面把展示中的快照回传，这里把刚读到的快照交回），落点都是 [`pause_timer_impl`]
/// → `AppState::pause` → `Coordinator::pause`。**没有第二条业务路径。**
///
/// 没有会话、或当前会话已经暂停（`recovering` 同理）⇒ [`TrayPause::NothingToPause`]：
/// 重复 `pause` 会撞 `VERSION_CONFLICT`，对用户毫无意义，空转还会白广播一次。
pub fn tray_pause_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
) -> Result<TrayPause, AppError> {
    // 托盘**绕过 `run_command`**（P6 Task 2a，计划 fix round 4 的 C-2）：所以维护态
    // 得自己判，而且必须在**取锁之后**判（"取不到锁"只说明有别的操作在临界区里）。
    // 拒绝对应"库零写入"：这一句之后才轮到快照与命令体。
    app.guard_writable()?;

    let snapshot = app.snapshot()?;
    if !snapshot.is_running() {
        return Ok(TrayPause::NothingToPause);
    }
    let (Some(session_id), Some(session_version)) =
        (snapshot.session_id.clone(), snapshot.session_version)
    else {
        return Ok(TrayPause::NothingToPause);
    };

    pause_timer_impl(
        app,
        broadcaster,
        SessionRequest {
            expected_data_epoch: snapshot.data_epoch,
            session_id,
            session_expected_version: session_version,
        },
    )
    .map(|outcome| TrayPause::Paused(Box::new(outcome)))
}

/// 托盘「退出」：**复用 Task 0 的显式退出入口**（[`RunningApp::shutdown`]）。
///
/// 顺序与语义全在那一条入口里（先停定时器，再一个事务结束 `running`/`paused`、
/// 写 `clean_exit_at`、按需推进 revision；`recovering` 记录保留）。这里不做任何改动：
/// 托盘只是它的第二个调用方，不是第二份实现。**不是杀进程。**
pub fn tray_quit_impl(running: &RunningApp) -> Result<ExitReport, AppError> {
    running.shutdown()
}

/// 托盘的诊断行：`message()`（用户文案）+ `detail()`（内部细节，只有它有诊断价值）。
///
/// 为什么要拼起来：`AppError::Storage` 的 `message()` 是固定的一句「存储暂时不可用…」，
/// 真正能定位问题的东西在 `detail()` 里（例如「显式退出不能在持有串行边界的线程上调用」）。
/// **只在诊断出口**这么写：IPC 的错误载荷仍然只送 `message()`（P4 Task 6 的口径）。
fn diagnostic(error: &AppError) -> String {
    match error.detail() {
        Some(detail) => format!("{}（{}）", error.message(), detail),
        None => error.message(),
    }
}

/// 落盘诊断那一行的 `detail` 段：`code=<码> detail=<内部原因>`。
///
/// 与 [`diagnostic`] 分开：那个是给人读的合并句，这个是给日志/脚本按 `k=v` 解析的。
fn tray_diagnostic(error: &AppError) -> String {
    match error.detail() {
        Some(detail) => format!("code={} detail={detail}", error.code()),
        None => format!("code={}", error.code()),
    }
}

/// 托盘「暂停」：在**阻塞线程**上、**串行边界内**执行（与 [`run_command`] 同一条骨架）。
pub fn spawn_tray_pause(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (shared, broadcaster) = {
            let running = handle.state::<RunningApp>();
            (Arc::clone(running.app()), Arc::clone(running.broadcaster()))
        };
        let mut state = lock_app(&shared);
        match tray_pause_impl(&mut state, &broadcaster) {
            Ok(TrayPause::Paused(outcome)) => {
                println!("[worktrace] tray: 已暂停（revision {}）", outcome.revision)
            }
            Ok(TrayPause::NothingToPause) => {
                println!("[worktrace] tray: 没有运行中的计时，暂停未执行")
            }
            Err(error) => {
                // 落盘诊断：release 的 Windows 子系统没有控制台，`eprintln!` 没人看得见
                // （维护态拒绝必须留下痕迹）。
                state
                    .diagnostics()
                    .record("tray.pause.refused", &tray_diagnostic(&error));
                eprintln!(
                    "[worktrace] tray: 暂停失败：{}（{}）",
                    diagnostic(&error),
                    error.code()
                )
            }
        }
    });
}

/// 托盘「退出」：同样切到阻塞线程（退出要开事务），成功之后才结束进程。
///
/// 退出事务失败时（例如库里有一条结束不了的会话）**仍然退出，用非零码标出来**：
/// 此刻事务已经回滚、库是一致的，这一次 run 会以「没有 `clean_exit_at`」结束——
/// 那正是恢复扫描的输入（F-015）。把用户困在一个没有窗口的托盘里比一次不干净退出更糟。
///
/// **唯一的例外是维护态**（P6 Task 2a，计划 fix round 4 的 C-2）：恢复正把库换到一半，
/// 此刻退出会让库停在中间态。所以那时 `shutdown()` 的 `begin_exit` 先拒绝，
/// 这里**不调 `handle.exit`**——采样线程仍在跑、进程继续活着，拒绝原因落盘。
/// （恢复很短；这是"不阻止用户用操作系统的强杀"，不是"没有出口"。）
///
/// **事件名按事实选**（P6 终审 M-8）：维护态拒绝退出 = 进程有意留着（`refused`）；
/// 其它失败 = 退出事务没做成、随后仍以非零码退出（`failed`）。两者原先共用一个
/// `tray.quit.refused`，事后排查会把"退出事务失败"读成"维护态拒绝退出"。
fn tray_quit_refusal_event(error: &AppError) -> &'static str {
    if is_maintenance_refusal(error) {
        "tray.quit.refused"
    } else {
        "tray.quit.failed"
    }
}

pub fn spawn_tray_quit(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = {
            let running = handle.state::<RunningApp>();
            tray_quit_impl(&running)
        };
        match result {
            Ok(report) => {
                println!(
                    "[worktrace] tray: 退出（run {} clean_exit_at {} 结束会话 {}）",
                    report.run_id,
                    report.clean_exit_at,
                    report.sessions_ended.len()
                );
                handle.exit(0);
            }
            Err(error) => {
                // 落盘诊断：release 的 Windows 子系统没有控制台（见 `spawn_tray_pause`）。
                // 退出被拒时这一行是**唯一**能说明"为什么点了没反应"的东西。
                let shared = Arc::clone(handle.state::<RunningApp>().app());
                let refusal = is_maintenance_refusal(&error);
                lock_app(&shared)
                    .diagnostics()
                    .record(tray_quit_refusal_event(&error), &tray_diagnostic(&error));
                eprintln!(
                    "[worktrace] tray: 退出失败：{}（{}）",
                    diagnostic(&error),
                    error.code()
                );
                if refusal {
                    // 维护态：拒绝退出（见上面的文档）。判定按第六个码
                    // `DATA_RESTORE_IN_PROGRESS`（[`is_maintenance_refusal`]）。
                    return;
                }
                handle.exit(1);
            }
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;

    /// 退出被拒的诊断事件名必须与事实相符（P6 终审 M-8）：
    /// 维护态 ⇒ `refused`（进程有意留着）；退出事务失败 ⇒ `failed`（随后非零码退出）。
    #[test]
    fn the_tray_quit_event_name_matches_the_kind_of_refusal() {
        assert_eq!(
            tray_quit_refusal_event(&AppError::DataRestoreInProgress),
            "tray.quit.refused",
            "维护态拒绝退出"
        );
        assert_eq!(
            tray_quit_refusal_event(&AppError::Storage {
                detail: "退出事务失败".to_string()
            }),
            "tray.quit.failed",
            "退出事务失败不是维护态拒绝"
        );
    }
}
