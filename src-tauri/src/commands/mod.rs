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
//! 调服务 → 返回响应）。包装只有一行转发——例外只有两处，都在「导出与备份 / 恢复」
//! 一节里写清：`backup` 的包装多凑一只**组合根的同源时钟**，`restore` 的包装走
//! [`run_maintenance_command`]（**不进 `run_command`**）。
//!
//! 为什么分开：`#[tauri::command]` 生成的包装要 Tauri 运行时才能调，而命令体只需要一个
//! `&mut AppState`（`restore` 那一对收 [`SharedApp`]：它自己按段取锁，见该节）——分开之后
//! `tests/ipc_commands.rs` 能**逐条**覆盖 37 条命令
//! （不需要 `tauri::test`，因此也不需要动 `Cargo.toml`）。一个 `finish_timer` 里误调
//! `app.pause` 的复制粘贴错误，现在会当场断言失败。
//!
//! # 请求形状：枚举一律是字符串
//!
//! `mode` / `timer_kind` / `statuses` 在 IPC 里都是**字符串**，命令体显式过
//! [`crate::services::timer::coordinator::parse_session_mode`] / [`crate::services::timer::coordinator::parse_timer_kind`] / `TaskStatus::parse`
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
//! - **响应形状不变**：不加 `{changed, value}` 信封（那会改掉每一份响应快照，而且计划没写），
//!   这一位只用于「要不要广播」这个内部判断；
//! - **失败只记诊断**：`Broadcaster::emit` 不返回错误，命令照常成功、已提交业务不回滚。
//!
//! 计时命令（`start`/`pause`/`resume`/`finish`）没有「幂等重复」这一支：能走到广播
//! 就说明这次状态跃迁真的提交了，所以它们的 `changed` 恒为真。
//!
//! # 托盘动作（P7 Task 4，P8 Task 2d 补「完成」）
//!
//! 托盘菜单点到的动作走**与 IPC 相同的命令体**：暂停 = [`tray_pause_impl`]（内部就是
//! [`pause_timer_impl`]），完成 = [`tray_finish_impl`]（内部就是 [`transition_task_impl`]，
//! 目标 `Done`、原因 `user`），退出 = [`tray_quit_impl`]（内部就是 Task 0 的显式退出入口
//! `RunningApp::shutdown`）。菜单本身的装配在 `platform::tray`（那一层不碰业务），
//! 组合根 `lib.rs` 把动作接到这三个入口上。
//!
//! 与 IPC 的一点差别：托盘**没有响应通道**，所以 `spawn_tray_*` 在阻塞线程里执行完
//! 只把结果写进诊断。串行边界与 IPC 完全相同——同一把
//! `Mutex<AppState>`、同样不在 UI 回调里开事务。
//!
//! **托盘绕过 [`run_command`]**（P6 Task 2a，计划 fix round 4 的 C-2）：所以
//! `guard_writable` 挡不住它们，三条路径各自判维护态——暂停与完成在
//! [`tray_pause_impl`] / [`tray_finish_impl`] 取锁之后先过门禁（拒绝即返回，不写任何东西）；
//! 退出走 `RunningApp::shutdown` 里的 `AppState::begin_exit`（维护态下拒绝，
//! **采样线程仍在跑**、进程也不退出）。拒绝都落到正式诊断日志
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
//! HUD 与全局捕获热键（F-012/F-013，V0.1b）。恢复确认、统计与导出、备份/恢复的命令
//! **都已接线**（P8 Task 1/2/3a，业务命令 37 条）：P3/P5/P6 只交付服务层入口，IPC 包装
//! 在这里。维护态的新错误码 `DATA_RESTORE_IN_PROGRESS` 与四处联动**已落地**
//! （P6 Task 4a），被拒响应的形状见 [`maintenance_response`]。
//!
//! # 文件布局（P8 收口批次拆分）
//!
//! 请求 DTO 与命令体按域拆在子模块里（`projects` / `tags` / `tasks` /
//! `daily_plan` / `stats` / `recovery` / `export` / `timer` / `handshake`），
//! 本文件只留执行骨架、`#[tauri::command]` 包装（`lib.rs` 的注册表逐条指向
//! 它们）与托盘入口。`pub use` 把各域的命令体与请求 DTO 原样再导出 ⇒
//! `commands::x_impl` / `commands::XxxRequest` 这些路径一处未变。

use std::sync::Arc;

use tauri::{AppHandle, Manager, State};

use crate::domain::error::DomainError;
use crate::domain::task::TaskStatus;
use crate::error::{AppError, AuthorityKind, AuthorityTarget, ErrorResponse};
use crate::services::bootstrap::{
    is_maintenance_refusal, lock_app, AppState, ExitReport, RunningApp, SharedApp,
};
use crate::services::error_response::capture_error_response;
use crate::services::events::{Broadcaster, EventEnvelope};
use crate::services::timer::coordinator::{
    ClockCorrectionAccepted, CommandOutcome, ResumeRequest, SessionRequest,
};
use crate::services::timer::snapshot::TimerSnapshot;
use crate::services::{catalog, history};

mod daily_plan;
mod export;
mod handshake;
mod projects;
mod recovery;
mod stats;
mod tags;
mod tasks;
mod timer;

pub use daily_plan::add_to_plan_impl;
pub use daily_plan::plan_for_impl;
pub use daily_plan::remove_from_plan_impl;
pub use daily_plan::PlanMutationRequest;
pub use export::backup_impl;
pub use export::export_data_impl;
pub use export::restore_impl;
pub use export::BackupRequest;
pub use export::BackupResult;
pub use export::ExportRequest;
pub use export::ExportResult;
pub use export::RestoreRequest;
pub use export::RestoreResult;
pub use handshake::get_revision_impl;
pub use projects::archive_project_impl;
pub use projects::create_project_impl;
pub use projects::list_projects_impl;
pub use projects::list_selectable_projects_impl;
pub use projects::rename_project_impl;
pub use projects::ArchiveProjectRequest;
pub use projects::CreateProjectRequest;
pub use projects::ListProjectsRequest;
pub use projects::RenameProjectRequest;
pub use recovery::accept_detected_clock_correction_impl;
pub use recovery::attention_overview_impl;
pub use recovery::backfill_impl;
pub use recovery::correct_impl;
pub use recovery::discard_session_impl;
pub use recovery::history_view_impl;
pub use recovery::reconcile_impl;
pub use recovery::retry_recovery_impl;
pub use recovery::transition_task_impl;
pub use recovery::BackfillRequest;
pub use recovery::ConfirmedRangeRequest;
pub use recovery::CorrectRequest;
pub use recovery::DiscardSessionRequest;
pub use recovery::HistoryQuery;
pub use recovery::ReconcileRequest;
pub use recovery::TransitionTaskRequest;
pub use stats::stats_today_impl;
pub use tags::create_tag_impl;
pub use tags::list_tags_impl;
pub use tags::tag_task_impl;
pub use tags::tags_of_task_impl;
pub use tags::untag_task_impl;
pub use tags::CreateTagRequest;
pub use tags::ListTagsRequest;
pub use tags::TaskTagRequest;
pub use tags::TaskTagsRequest;
pub use tasks::clarify_ready_impl;
pub use tasks::create_task_impl;
pub use tasks::list_tasks_impl;
pub use tasks::set_task_project_impl;
pub use tasks::ClarifyReadyRequest;
pub use tasks::CreateTaskRequest;
pub use tasks::SetTaskProjectRequest;
pub use timer::finish_timer_impl;
pub use timer::pause_timer_impl;
pub use timer::resume_timer_impl;
pub use timer::start_timer_impl;
pub use timer::timer_snapshot_impl;
pub use timer::timer_tick_impl;
pub use timer::StartTimerRequest;

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
) -> Result<crate::services::handshake::RevisionSnapshot, ErrorResponse> {
    run_command(
        "get_revision",
        window.label(),
        &state,
        Vec::new(),
        get_revision_impl,
    )
    .await
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

// ─────────────────────────────────────────────────────────────────────────────
// 今日计划（F-010 的「今日选择」半边）
// ─────────────────────────────────────────────────────────────────────────────

/// 读某一天（某个时区）的今日选择列表。日期与时区都在服务入口过唯一校验。
#[tauri::command]
pub async fn plan_for(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: crate::services::daily_plan::DailyPlanQuery,
) -> Result<crate::services::daily_plan::DailyPlanView, ErrorResponse> {
    let targets = Vec::new();
    run_command("plan_for", window.label(), &state, targets, move |app| {
        plan_for_impl(app, request)
    })
    .await
}

/// 把一个任务加入今日计划。重复加入 ⇒ 幂等。
#[tauri::command]
pub async fn add_to_plan(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: PlanMutationRequest,
) -> Result<crate::services::daily_plan::DailyPlanChange, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Task, &request.task_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command("add_to_plan", window.label(), &state, targets, move |app| {
        add_to_plan_impl(app, &broadcaster, request)
    })
    .await
}

/// 把一个任务从今日计划里移除。口径与 [`add_to_plan`] 对称。
#[tauri::command]
pub async fn remove_from_plan(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: PlanMutationRequest,
) -> Result<crate::services::daily_plan::DailyPlanChange, ErrorResponse> {
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

// ─────────────────────────────────────────────────────────────────────────────
// 统计（F-010 的「今日工时」半边）
// ─────────────────────────────────────────────────────────────────────────────

/// Today 聚合（F-010 的五项）：今日选择列表、当前任务、已确认 / 运行暂计 / 待确认三组。
///
/// 收的是服务层的 IPC 请求 DTO（[`crate::services::stats::TodayQuery`]，**不另造请求形状**）：
/// 它**不带日期**——「今天」由服务从**同一次样本的归属终点**算，所以 `date` / `range` /
/// `as_of` 三者天然同源；想看别的日子用范围报表。`timezone` 是原始输入，归一在
/// [`stats::today`] 那一条唯一入口上（与日界、存储键同一套），命令层只转发。
///
/// **纯读**（命令表第 12 条）：不 `bump_revision`、不广播 `domain.changed`、不补采时钟、
/// 不重算数字。异常采样可能先提交 P2 的恢复事务——那是服务层的事，这里不「顺手」刷新。
#[tauri::command]
pub async fn stats_today(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: crate::services::stats::TodayQuery,
) -> Result<crate::services::stats::TodayView, ErrorResponse> {
    run_command(
        "stats_today",
        window.label(),
        &state,
        Vec::new(),
        move |app| stats_today_impl(app, request),
    )
    .await
}

// ─────────────────────────────────────────────────────────────────────────────
// 恢复与历史（P3 的服务层入口，P8 的 IPC）
// ─────────────────────────────────────────────────────────────────────────────
//
// 五条**写**命令：对账、修正、补录、作废整次、任务跃迁。服务层与 `AppState` 入口由
// P3 交付（P3 计划「边界」：P3 不新增 `#[tauri::command]`），这里只接线。共同口径：
//
// - 服务返回的是 [`crate::storage::WriteOutcome`]（`Changed` / `Unchanged`），命令体
//   `into_parts()` 解它，**只有 `Changed` 才广播** `domain.changed`——与 `create_task`
//   一族同一条姿势。响应形状**不加** `{changed, value}` 信封（模块头）。
// - 响应用服务层的报告类型本身（`ReconcileReport` / `HistoryEditReport` /
//   `TaskTransitionReport`），它们自带 `data_epoch` / `revision`，页面据此判旧；
//   不在命令层复制一份字段做镜像 DTO。
// - 转发**不带 `run_id`**：`AppState::reconcile` 等方法内部自己取（协调器只有它够得着）。
// - `correct` 只对 `finished` 会话开放、`discard_session` 与
//   `reconcile(discard_uncertain)` 是两条不同命令、`backfill` 不启动计时——这些都是
//   **服务层**的判据，命令层不重复实现、也不放宽（界面据会话 `state` 禁用入口）。
// - 命令体里的 `now` **只用于广播信封的 `at`**：这三份报告都没有时刻字段，而写事务
//   自己的时钟样本在服务层（`AppState::reconcile` 等方法内部取）。两次采样走同一条
//   时钟接缝（`AppState::now_ms` → 协调器），不引入第二个时间源。

/// 「这个值不在取值域里」。与 `services::timer::coordinator` 里那份同形：
/// `field` 面向用户、`value` 原样回显，错误码落在 `DOMAIN_ERROR` 上。
fn unknown_enum_value(field: &'static str, value: &str) -> AppError {
    DomainError::UnknownEnumValue {
        field,
        value: value.to_string(),
    }
    .into()
}

/// 对账：一次处理该会话的**全部**待确认区间（确认或丢弃）。
///
/// 只接 `recovering`（R6）：非 `recovering` 一律拒绝——包括「已经确认过、想再确认一次」，
/// 所以这条命令**没有 `Unchanged` 这一支**（拒绝而不是当成又一笔写，见服务层文档）。
#[tauri::command]
pub async fn reconcile(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: ReconcileRequest,
) -> Result<crate::services::recovery::ReconcileReport, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Session, &request.session_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command("reconcile", window.label(), &state, targets, move |app| {
        reconcile_impl(app, &broadcaster, request)
    })
    .await
}

/// 历史修正：重定时 / 软删除一条可信区间。只对 `finished` 会话开放。
#[tauri::command]
pub async fn correct(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: CorrectRequest,
) -> Result<history::HistoryEditReport, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Session, &request.session_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command("correct", window.label(), &state, targets, move |app| {
        correct_impl(app, &broadcaster, request)
    })
    .await
}

/// 手工补录一段已经发生的人工时间。**独立入口**：不启动计时、不伪造完成事件。
#[tauri::command]
pub async fn backfill(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: BackfillRequest,
) -> Result<history::HistoryEditReport, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Task, &request.task_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command("backfill", window.label(), &state, targets, move |app| {
        backfill_impl(app, &broadcaster, request)
    })
    .await
}

/// 作废整次会话（二次确认是界面的硬前置；命令层只转发）。
#[tauri::command]
pub async fn discard_session(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: DiscardSessionRequest,
) -> Result<history::HistoryEditReport, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Session, &request.session_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command(
        "discard_session",
        window.label(),
        &state,
        targets,
        move |app| discard_session_impl(app, &broadcaster, request),
    )
    .await
}

/// 任务状态跃迁：同一事务里联动该任务的会话（结束 / 暂停），并广播一次。
#[tauri::command]
pub async fn transition_task(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: TransitionTaskRequest,
) -> Result<crate::services::tasks::TaskTransitionReport, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Task, &request.task_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command(
        "transition_task",
        window.label(),
        &state,
        targets,
        move |app| transition_task_impl(app, &broadcaster, request),
    )
    .await
}

// ─────────────────────────────────────────────────────────────────────────────
// 恢复的读取与重试（命令 6/7/8）、历史读取（命令 13）
// ─────────────────────────────────────────────────────────────────────────────
//
// 这四条都**只做参数解析与转发**，不新增业务逻辑：
//
// - 6 `accept_detected_clock_correction`：请求就是**库身份**（照既有
//   [`EpochRequest`] 的形状；P8 计划表里的名字 `AcceptClockCorrectionRequest` 是服务层
//   那个类型的名字，它没有 `Deserialize`，命令层不为它另造一份单字段镜像）。响应
//   [`ClockCorrectionAccepted`] **自带** `data_epoch` / `revision`，`accepted == false`
//   是「没有待接受的校正」这条**正常**路径（幂等零变化），不是失败。
//   **它是写命令**（计划文末那张表的共同口径：「业务写命令（1–6）按服务层实际变更决定
//   版本推进与广播」）：`accepted == true` ⇒ 服务提交了一条审计并恰好 +1 `revision`
//   ⇒ 命令层按同一位**广播恰好一条** `domain.changed`；`accepted == false` ⇒ 零变化
//   ⇒ **一条都不发**。`revision` 是跨窗口唯一水位，只推版本不广播会让别的窗口把它读成
//   「丢了一次通知」（`RevisionGate` 规则④判跳号 ⇒ 白跑一次 `Resync`）。
// - 7 `retry_recovery`：`&str` 也是 **`expected_data_epoch`**（不是 run_id），请求同样是
//   [`EpochRequest`]。它**不带 `WriteEnvelope`**（没有用户可编辑对象，判据在协调器里）、
//   内部已 `guard_writable`；**不保证每次推进 `revision`**、**不强制广播**——恢复事务
//   可能提交、也可能只是重载/重扫或无变化。返回的是**提交后**的权威快照，命令层不做
//   任何「顺手刷新」。
//
//   ⚠️ **第 7 条不广播是计划明文给的豁免，不是漏写**：那张表的共同口径把命令 1–6 归入
//   「按实际变更决定版本推进与广播」，第 7 条单独写「**不保证每次写入**；复用 P3 的实际
//   结果与异常闭环」。而且服务返回的是 [`TimerSnapshot`]——**没有 `changed` 位**，命令层
//   无从判断这一次到底提交了事务、还是只重载/重扫/无变化，硬发一条就会有假通知，按
//   `revision` 自比又正是被禁的「读出来再跟自己比」。恢复事务自己那层的闭环（P3 的
//   结果与异常）已经写清了什么时候推进版本。
// - 8 `attention_overview`：**只读**，只做三件事——转发请求里的 epoch、从
//   `AppState::coordinator()` 取 `run_id()`、把两者与 `AppState::db()` 一起交给自由函数
//   `services::recovery::attention_overview`。**没有 `AppState::attention_overview`
//   这个包装**（协调器只有 `AppState` 够得着，而这条读路径不需要它做别的事）。
// - 13 `history_view`：**只读**新服务（`services::history`），同一读事务里校验 epoch、
//   取分页会话与可选详情；不采样、不写库、不推进 `revision`。`limit`/`offset` 的越界由
//   服务层经 `task_repo::require_page` 拒绝——命令层不复制那条规则，也不碰 `storage::`
//   （`scripts/check-layers.ps1` 第 1 条）。
//
// 因此：6 **只按 `accepted` 广播**（真布尔驱动，与「仅 `Changed` 才广播」同一条姿势）、
// 7 按上面那条豁免不广播、8/13 是纯读不广播。

/// 显式接受一次**已检测但未接受**的墙钟校正（命令 6）。
///
/// 不自动接受（08 §1）：界面必须在用户点过之后才调它。失败原样交出去——协调器只在审计
/// **提交之后**清内存标记，命令层不做任何补偿。
#[tauri::command]
pub async fn accept_detected_clock_correction(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: EpochRequest,
) -> Result<ClockCorrectionAccepted, ErrorResponse> {
    let broadcaster = Arc::clone(state.broadcaster());
    run_command(
        "accept_detected_clock_correction",
        window.label(),
        &state,
        Vec::new(),
        move |app| accept_detected_clock_correction_impl(app, &broadcaster, request),
    )
    .await
}

/// 用户显式点「重试」时的恢复重试（命令 7）。**不做定时自动重试**。
#[tauri::command]
pub async fn retry_recovery(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: EpochRequest,
) -> Result<TimerSnapshot, ErrorResponse> {
    run_command(
        "retry_recovery",
        window.label(),
        &state,
        Vec::new(),
        move |app| retry_recovery_impl(app, request),
    )
    .await
}

/// 全局待确认概览（命令 8）：恢复页与「待确认」栏的唯一数据源。
#[tauri::command]
pub async fn attention_overview(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: EpochRequest,
) -> Result<crate::services::recovery::AttentionOverview, ErrorResponse> {
    run_command(
        "attention_overview",
        window.label(),
        &state,
        Vec::new(),
        move |app| attention_overview_impl(app, request),
    )
    .await
}

/// 常规历史（命令 13）：窗口内的一页终态会话 + 可选详情。
#[tauri::command]
pub async fn history_view(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: HistoryQuery,
) -> Result<history::HistoryView, ErrorResponse> {
    run_command(
        "history_view",
        window.label(),
        &state,
        Vec::new(),
        move |app| history_view_impl(app, request),
    )
    .await
}

// ─────────────────────────────────────────────────────────────────────────────
// 导出与备份 / 恢复（P8 Task 3a：命令 9/10/11）
// ─────────────────────────────────────────────────────────────────────────────
//
// P5 交付了导出的**内容生成**、P6 交付了备份/恢复的**服务原语**；**落盘、IPC 与能力
// 登记全部归 P8**（P5 计划把落盘订正给 P8）。这一节就是那三件事的接线，共同口径：
//
// - **落盘在锁内**：导出与备份的文件 IO 都发生在 `run_command` 的阻塞段里（同一把锁的
//   **短**临界区）。它们不跨 `await`、不做长活，所以**不放锁外**——放出去就丢掉了
//   "用户命令与周期采样天然串行"这条串行边界（`backup_consistent` 的 `VACUUM INTO`
//   还必须与写操作在同一条连接上）。唯一的例外是 `restore`，理由见它自己的文档。
// - **导出与备份都不推进 `revision`、不广播** `domain.changed`：它们不修改业务事实。
// - **响应自带 `data_epoch` / `revision`**：页面的本视图水位靠它判旧。
// - **枚举一律字符串**（模块头）：`format` 走显式解析，不靠 serde 的枚举反序列化。
// - **落盘失败走 Rust 的错误契约**（`AppError` 的 `code` / `message`），界面**不要**
//   在 `invoke` 的 `catch` 里自己编文案（计划 Task 3 的「落盘的失败路径」条）。
//
// **取消 ≠ 失败**（同一条的 ①）：按零依赖方案，V0.1 **没有"选择保存位置"这一步**
// （路径固定、对话框不引入）⇒ 这一档今天只落在"用户主动放弃"的场景上（例如恢复的
// 二次确认被取消）：不弹错误、不写文件、不重试，界面回到可再次触发的状态。恢复的
// `confirmed: false` 就是它在服务端的落点（拒绝且零副作用）。**将来若引入保存对话框，
// 用户取消走的就是这一档**——不是 `AppError`。

/// 导出（F-018，命令 9）：调 P5 的生成函数拿内容 → 写进 `<app_data_dir>/exports/` →
/// 返回**真实存在的绝对路径**。
///
/// 转发（计划第 9 行）：`json` → `AppState::export_json(&StatsRangeQuery)`（范围来自
/// `from` / `to`）；`markdown` → `AppState::export_weekly_markdown(&WeeklyQuery)`
/// （周界**由服务算**——`anchor` 只是"哪一周"的输入，"本周"更是同一次样本的归属终点）。
/// 命令层不算周界、也不把范围硬塞进周回顾。
///
/// **纯副作用是文件系统那一半**：不 `bump_revision`、不广播 `domain.changed`、不补采
/// 时钟（文件名那一次 `now_ms` 只用于命名）。编号 `revision` 与 `data_epoch` 从**本次
/// 生成结果**里取。
///
/// `dir_override` 见 [`export::export_dir`]：生产 IPC 传 `None`，用例注入临时目录。
#[tauri::command]
pub async fn export_data(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: ExportRequest,
) -> Result<ExportResult, ErrorResponse> {
    run_command(
        "export_data",
        window.label(),
        &state,
        Vec::new(),
        move |app| export_data_impl(app, request, None),
    )
    .await
}

/// 备份（F-019 的备份半边，命令 10）：在**同一条连接**上 `VACUUM INTO` 一份库副本。
///
/// 六个参数**自己凑**（不是一行转发，勘察 §1-B4）：
/// `dir_override` = `None`（生产缺省 `app_data_dir()/backups`，由原语自己解析）、
/// `conn` 与 `db_version` 来自 `AppState`（命令层不引用 `storage::`，所以版本号走
/// [`backup::current_version`] 这个转发）、`clock` 是**组合根的同源钟**
/// （`RunningApp::clock_source`，与 OS 事件源、协调器共用同一个 `Instant` 原点）、
/// `diagnostics` 来自 `AppState`。
///
/// **写，但不改业务事实**：不加 `revision`、不广播 `domain.changed`。信封只用
/// `expected_data_epoch` 做身份校验（`expected_row_version: None`——备份没有可校验的
/// 实体版本）。维护态期间被 [`AppState::guard_writable`] 拒绝；**进程内串行**：走同一把
/// 锁的短临界区，不把长活放到锁外。
#[tauri::command]
pub async fn backup(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: BackupRequest,
) -> Result<BackupResult, ErrorResponse> {
    // 组合根的同源时钟：取不到就**在取锁之前**拒掉。绝不在命令层新建一只钟——
    // 各建一只 `SystemClock` 会得到两个 `Instant` 原点（R-02 静默落空）。
    let clock = match state.clock_source() {
        Ok(source) => source(),
        Err(error) => return Err(wiring_failure(&error)),
    };
    run_command("backup", window.label(), &state, Vec::new(), move |app| {
        backup_impl(app, &*clock, request, None)
    })
    .await
}

/// 恢复（F-019 的危险半边，命令 11）：**全项目唯一不进 `run_command` 的命令**。
///
/// 为什么不进（三条理由，任何一条都够）：
///
/// 1. 服务入口 [`backup::restore_from_backup`] 把三段（`begin_restore` →（锁外）
///    `prepare_and_swap` → `commit_restore` / `abort_restore`）连在**一次调用**里，
///    而它**自己按段取锁**；`run_command` 的闭包**正持着**那把非重入 `Mutex` ⇒ 放进
///    去不是报错而是**静默死锁**。P6 终审已把这条从 `debug_assert!` 升级成硬判据
///    （持锁调用被主动拒绝）⇒ 接线错误是**红**，不是卡住，用例钉住的就是这一点。
/// 2. 中间那段（关连接、拷候选库、校验、同卷改名）是**不持锁的长活**：放进单临界区
///    等于把整个进程顶在锁上，"维护态里其余命令快速失败"随之失效。
/// 3. 失败时的错误映射与其余命令不同（见 [`error_response_without_lock`]）：**回滚也
///    失败**时运行态不在手，那时不能像 `run_command` 那样把错误换成维护态码。
#[tauri::command]
pub async fn restore(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: RestoreRequest,
) -> Result<RestoreResult, ErrorResponse> {
    let app = Arc::clone(state.app());
    let broadcaster = Arc::clone(state.broadcaster());
    // 同源时钟（组合根那一份）：取不到就**在进维护态之前**拒掉——那时进程状态与磁盘
    // 一个字节都没变。
    let clock = match state.clock_source() {
        Ok(source) => source,
        Err(error) => return Err(wiring_failure(&error)),
    };
    run_maintenance_command("restore", window.label(), app, move |app| {
        restore_impl(app, &broadcaster, &clock, request)
    })
    .await
}

/// `restore` 专用的执行骨架：把 [`SharedApp`] 交给命令体，**由命令体自己按段取锁**。
///
/// 与 [`run_command`] 的差别只有这一点（那一个替命令体取一把包住整个 body 的锁）。
/// 其余照旧：阻塞段在 `spawn_blocking` 里（**不在 IPC/UI 线程上跑 SQLite**）、
/// 错误映射也在**同一条阻塞线程**上做（它要开一个读事务拿权威版本）。
async fn run_maintenance_command<T, F>(
    command: &'static str,
    window: &str,
    app: SharedApp,
    body: F,
) -> Result<T, ErrorResponse>
where
    T: Send + 'static,
    F: FnOnce(&SharedApp) -> Result<T, AppError> + Send + 'static,
{
    #[cfg(debug_assertions)]
    let probe = dev::CommandProbe::start(command, window);
    let result = match tauri::async_runtime::spawn_blocking(move || match body(&app) {
        Ok(value) => Ok(value),
        // **原错误原样交出去**（见 [`error_response_without_lock`]）：这里读不到库是
        // "恢复失败且回滚也失败"的现场，不是"正在恢复"。
        Err(error) => Err(error_response_without_lock(&app, &error)),
    })
    .await
    {
        Ok(result) => result,
        Err(join) => Err(internal_failure(join.to_string())),
    };

    // dev 注入 (b)（P7 Task 6a）：与 [`run_command`] 同一姿势——取数已经完成、锁已经
    // 放开，这里只把**响应**推迟返回。发布构建里 `commands::dev` 整份不存在。
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

/// **锁外 / 换库窗口**的错误映射（命令 10 的预检失败与命令 11 的失败共用）。
///
/// 与 [`run_command`] 里那条**刻意不同**：那一条在"取不到库"时把错误**换成**维护态码
/// （它只在维护态可达，换掉是对的）；这里"取不到库"是**恢复失败且回滚也失败**的现场，
/// 用户要看到的是**恢复为什么失败**（`code` / `message`），把它换成
/// `DATA_RESTORE_IN_PROGRESS` 就等于把"恢复失败卡住"伪装成"正在恢复"——P6 终审 I-2
/// 专门立了这条规矩（`startup.failed` / `restore.failed` 两条诊断就是为它补的）。
///
/// 所以：**原错误原样交出去**，只在读得到库时补上权威上下文；读不到就
/// `authority: None` + `requires_handshake: true`——**绝不**为了凑一个版本去强读缺失的
/// `Db`，也绝不编一个（`capture_error_response` 对"读不到"的处置同此：`authority: None`
/// ⇒ `requires_handshake`）。
pub fn error_response_without_lock(app: &SharedApp, error: &AppError) -> ErrorResponse {
    match lock_app(app).db() {
        Ok(db) => capture_error_response(db, error, &[]),
        Err(_) => ErrorResponse {
            code: error.code().to_owned(),
            message: error.message(),
            authority: None,
            requires_handshake: true,
        },
    }
}

/// **接线缺陷**（`backup` / `restore` 的预检取不到组合根的同源时钟）的响应形状。
///
/// 取 [`internal_failure`] 那一档：`STORAGE_ERROR` + `authority: None` +
/// `requires_handshake: true`。**刻意不读库**——这两条预检发生在**还没进阻塞段**的
/// async 线程上，而取权威版本要开一个读事务；SQLite 不许在这一层跑（模块头三条硬约束）。
/// 生产上这条分支不可达（`lib.rs::setup` 启动成功后一定 `attach_clock`），
/// 它是给"组合根改了、忘了挂钟"准备的明确失败。
fn wiring_failure(error: &AppError) -> ErrorResponse {
    internal_failure(error.detail().unwrap_or(error.code()).to_string())
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

/// 托盘「完成」的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayFinish {
    /// 没有**正在计时**的会话（无会话 / 会话已暂停 / 会话已是 `recovering`）：这次托盘动作
    /// 什么都没做——不跃迁任务、不结束或暂停任何会话、不广播。
    ///
    /// ⚠️ 措辞是"**托盘没有发起写**"，**不是**"这条路径零写入"（fix round 1，评审 Minor-4）：
    /// 走到这里之前那句 `app.snapshot()` 在采样判出异常时会**先提交一笔独立的系统恢复事务**
    /// （`Coordinator::snapshot` 写明"所以这次查询确实写了库，调用方不能宣称「查询全程只读」"）。
    /// 那笔写与托盘意图无关，是协调器自己那条闭环（幂等分支零写入）。
    NothingToFinish,
    /// 复用 [`transition_task_impl`] 把当前计时中的任务推到了 `Done`。
    Finished(Box<crate::services::tasks::TaskTransitionReport>),
}

/// 托盘「完成」：把**当前正在计时的那条任务**推到 `Done`（P8 Task 2d；命令 5 的第二个消费方）。
///
/// 「哪条任务」只有快照一个来源（`task_id` + `task_row_version`）：托盘不在界面上，
/// 拿不到任何"行上的版本"，而 `transition_task` 的版本守卫要的正是这两个字段
/// ——与 [`tray_pause_impl`] 取会话 id/版本是同一姿势。
///
/// 判据与联动全在 [`transition_task_impl`] → `services::tasks::transition_task`：
/// 完成会在**同一个事务**里结束这条任务全部 `running`/`paused` 会话，并在有 `recovering`
/// 会话、`needs_review` 或待确认区间时**整体拒绝**（`RECOVERY_REQUIRED`）。
/// 托盘**不另写业务逻辑**，也不在这条入口之外自己再广播一次（`transition_task_impl`
/// 已按 `Changed` 广播）。
///
/// 边界——两条都是"什么都不做"，不是"造一个假动作"：
/// - **没有正在计时**（无会话 / 会话已暂停 / 上一次异常已把会话置成 `recovering`）⇒
///   [`TrayFinish::NothingToFinish`]。`is_running` 只看 `running`，与 [`tray_pause_impl`]
///   同一条判据：暂停中的会话由界面接手（那一页看得见状态与「继续」）。
///   **协调器已被判故障**时不是这一支：那时 `app.snapshot()` 自己返回 `RECOVERY_REQUIRED`
///   （`Coordinator::refuse_if_faulted`），走下面的拒绝臂、落 `tray.finish.refused`；
/// - 快照没带全任务身份（`task_id` / `task_row_version` 缺一）⇒ 同样什么都不做：
///   **不猜**一条任务、也不编一个版本去撞服务端的守卫。
///
/// 版本冲突 / 领域拒绝 / 维护态拒绝都原样交回调用方：托盘**没有界面回执通道**
/// （已知限制，登记在 `spawn_tray_finish` 的诊断里）。
pub fn tray_finish_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
) -> Result<TrayFinish, AppError> {
    // 托盘**绕过 `run_command`**（见模块头）：维护态自己判，而且必须在**取锁之后**判。
    app.guard_writable()?;

    let snapshot = app.snapshot()?;
    if !snapshot.is_running() {
        return Ok(TrayFinish::NothingToFinish);
    }
    let (Some(task_id), Some(task_row_version)) =
        (snapshot.task_id.clone(), snapshot.task_row_version)
    else {
        return Ok(TrayFinish::NothingToFinish);
    };

    transition_task_impl(
        app,
        broadcaster,
        TransitionTaskRequest {
            expected_data_epoch: snapshot.data_epoch,
            task_id,
            expected_row_version: task_row_version,
            // 两个字符串按命令层自己的取值域给：`target` 就是落库用的状态名
            // （`TaskStatus::as_str`），`cause` 与 `parse_transition_cause` 的字面量一致。
            target: TaskStatus::Done.as_str().to_string(),
            cause: "user".to_string(),
        },
    )
    .map(|report| TrayFinish::Finished(Box::new(report)))
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

/// 托盘「完成」：在**阻塞线程**上、**串行边界内**执行（与 [`spawn_tray_pause`] 同一条骨架）。
///
/// 三种结果各有诊断：做了（带上提交后的任务标题、revision 与两个联动名单的条数）、
/// 没有正在计时的会话（什么都没做）、被拒（版本冲突 / 领域拒绝 / 维护态 / 协调器已判故障）。
/// 后两种是托盘的**已知限制**：它没有响应通道，界面不会弹任何东西——用户看到的是
/// "没反应"，真相在诊断里；界面的下一次重拉读到的仍是库里的权威状态。
pub fn spawn_tray_finish(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (shared, broadcaster) = {
            let running = handle.state::<RunningApp>();
            (Arc::clone(running.app()), Arc::clone(running.broadcaster()))
        };
        let mut state = lock_app(&shared);
        match tray_finish_impl(&mut state, &broadcaster) {
            Ok(TrayFinish::Finished(report)) => println!(
                "[worktrace] tray: 完成「{}」（revision {}，结束会话 {}，暂停会话 {}）",
                report.task.title,
                report.revision,
                report.ended_sessions.len(),
                report.paused_sessions.len()
            ),
            Ok(TrayFinish::NothingToFinish) => {
                // 措辞按**实际语义**（fix round 1，评审 Minor-4）：可能是"根本没有会话"、
                // "会话已暂停"，也可能是"会话已被判为 recovering、等恢复处理"——三种都归
                // `is_running()` 这一条判据，也都不归托盘这条路径管。
                println!("[worktrace] tray: 没有正在计时（或已被判为待恢复）的会话，完成未执行")
            }
            Err(error) => {
                // 落盘诊断：release 的 Windows 子系统没有控制台，`eprintln!` 没人看得见
                // （维护态拒绝必须留下痕迹）。事件名说清"这次托盘完成被拒"，
                // 具体码在 `tray_diagnostic` 的 `code=` 里。
                state
                    .diagnostics()
                    .record("tray.finish.refused", &tray_diagnostic(&error));
                eprintln!(
                    "[worktrace] tray: 完成失败：{}（{}）",
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
                crate::platform::window::show_exit_failure(&format!(
                    "{}\n详细原因已记录在 Worktrace 诊断日志。",
                    error.message()
                ));
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
