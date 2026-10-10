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
//! HUD 与全局捕获热键（F-012/F-013，V0.1b）。恢复确认、统计与导出、备份/恢复的命令
//! **都已接线**（P8 Task 1/2/3a，业务命令 37 条）：P3/P5/P6 只交付服务层入口，IPC 包装
//! 在这里。维护态的新错误码 `DATA_RESTORE_IN_PROGRESS` 与四处联动**已落地**
//! （P6 Task 4a），被拒响应的形状见 [`maintenance_response`]。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::{AppHandle, Manager, State};

use crate::domain::error::DomainError;
use crate::domain::task::{TaskStatus, TransitionCause};
use crate::envelope::WriteEnvelope;
use crate::error::{AppError, AuthorityKind, AuthorityTarget, ErrorResponse};
use crate::platform::clock::Clock;
use crate::services::bootstrap::{
    holds_app_lock, is_maintenance_refusal, lock_app, AppState, ExitReport, RunningApp, SharedApp,
};
use crate::services::error_response::capture_error_response;
use crate::services::events::{Broadcaster, EventEnvelope};
use crate::services::timer::coordinator::{
    parse_session_mode, parse_timer_kind, ClockCorrectionAccepted, CommandOutcome, ResumeRequest,
    SessionRequest, StartRequest,
};
use crate::services::timer::snapshot::TimerSnapshot;
use crate::services::{
    backup, catalog, daily_plan, export, handshake, history, recovery, stats, tasks,
};

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

// ─────────────────────────────────────────────────────────────────────────────
// 导出与备份 / 恢复的请求与响应 DTO（P8 Task 3a：命令 9/10/11）
// ─────────────────────────────────────────────────────────────────────────────

/// 导出请求（命令 9）：按 `format` 判别该带哪些标量。
///
/// **判别式，不是"都能传"**：`json` 要 `from`/`to`（半开范围）；`markdown` 只要可选的
/// `anchor`（这一周里的**任一刻**，周界由服务算）。不适用却传了 ⇒ 命令体**显式拒绝**
/// （不是"忽略它"）：忽略会让调用方以为自己指定了范围，而服务用的是另一套口径
/// （计划「2026-10-08 第三轮契约收口」的导出 DTO 条：不接受同时传不适用字段的含糊请求）。
///
/// `from` / `to` / `anchor` 都是 `Option`：必填与否**取决于判别式**，而 serde 表达不了
/// "当 format=json 时必填"——那条规则落在 [`parse_export_request`] 上（拒绝原因可读，
/// 且是 `DOMAIN_ERROR` 而不是传输层的反序列化错误）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ExportRequest {
    /// `json`（范围明细）或 `markdown`（自然周回顾）。
    pub format: String,
    /// 用户时区（原始输入，归一在服务那一条唯一入口上）。
    pub timezone: String,
    pub expected_data_epoch: String,
    /// `json` 必填：范围起点（含）。
    pub from: Option<i64>,
    /// `json` 必填：范围终点（**不含**）。
    pub to: Option<i64>,
    /// `markdown` 可选：这一周里的任一刻；省略 = 同一次样本的归属终点（"本周"）。
    pub anchor: Option<i64>,
}

/// 备份请求（命令 10）。只带库身份：它不修改任何业务事实，没有可校验的实体版本。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BackupRequest {
    pub expected_data_epoch: String,
}

/// 恢复请求（命令 11）：全项目唯一的危险操作。
///
/// `confirmed` 是**命令层再校验一次**的二次确认位——界面上的二次确认是硬前置，但
/// 不能只靠界面。缺它（反序列化失败）或为 `false` 都拒绝，且**不产生任何副作用**。
/// `expected_data_epoch` 是**进维护态之前**的身份守卫：拿旧展示来点恢复 ⇒
/// `DATA_EPOCH_MISMATCH`，此时进程状态与磁盘一个字节都没变。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RestoreRequest {
    /// 待恢复的备份产物路径（用户从备份目录里挑的那一份）。
    pub backup_path: String,
    pub expected_data_epoch: String,
    pub confirmed: bool,
}

/// 一次导出的结果（命令 9）：**真实存在的绝对路径** + 与内容同一份数据的版本信封。
///
/// 来由（P8 Task 3a）：P5 只产出内容（`ExportJson` / `ExportMarkdown` 是服务层类型、
/// 没有 serde），**落盘与 IPC 归 P8**（P5 计划把落盘订正给 P8）。`data_epoch` /
/// `revision` **直接取自这一次生成结果**——不在写文件之后重新查询（计划第三轮契约
/// 收口：那样报的是"响应时刻"的版本，与文件里的数字就不再同源）。
///
/// 落盘本身**不推进业务 `revision`**、不广播 `domain.changed`：写文件不是业务事实。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExportResult {
    /// 落盘后的**绝对**路径（界面据此 `revealItemInDir`，也可复制给用户）。
    pub path: String,
    /// 写进文件的字节数（UTF-8 长度，与文件大小逐字节相符）。
    pub bytes: u64,
    pub data_epoch: String,
    pub revision: i64,
}

/// 一次备份的结果（命令 10）。
///
/// 来由：备份产生一份库副本、**不修改业务事实** ⇒ **不加 `revision`**、不广播
/// `domain.changed`（计划第 10 条）。信封里的两个值就是这次备份时刻的权威身份与版本，
/// 与写命令的"提交后读回"同一口径——只是这里没有提交，所以它们是**产物落地之后**
/// 读回来的同一个值（判据：备份命令里没有 `bump_revision`）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupResult {
    /// 备份产物的**绝对**路径（`VACUUM INTO` 出来的单文件快照）。
    pub path: String,
    /// 产物大小（字节）。
    pub bytes: u64,
    pub data_epoch: String,
    pub revision: i64,
}

/// 一次恢复的结果（命令 11）。
///
/// 来由：`revision` **必须在恢复的锁内冻结**（勘察 §1-B6 / 决策 3）。`RestoreOutcome`
/// 原先不带它，P8 扩了返回材料：`services::backup::Identity` 在**两步重扫之后、
/// `install_runtime` 之前**、仍持同一把锁时读回 `meta.revision`，提交与回滚两条路径
/// 各自把它放进 outcome。**不得**换库放锁之后再读一次拼上去——那样 `data_epoch` 与
/// `revision` 就不是同一个时刻的两个字段了。
///
/// `applied` 在成功时恒为 `true`：服务把"没换成"当 `Err` 交回（回滚成功也报原始
/// `Err(cause)`），所以**没有可达的正常 `false`**。这里照抄 `outcome.committed` 而不是
/// 写死 `true`——写死会把"服务将来改形状"的风险藏起来。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RestoreResult {
    /// 恢复之后的 `data_epoch`（提交路径 = 全新值）：前端据此**重新握手**，旧展示必须丢。
    pub data_epoch: String,
    /// 与 [`RestoreResult::data_epoch`] 同一时刻（锁内同一次读）的权威版本。
    pub revision: i64,
    pub applied: bool,
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
// 统计（F-010 的「今日工时」半边）
// ─────────────────────────────────────────────────────────────────────────────

/// Today 聚合（F-010 的五项）：今日选择列表、当前任务、已确认 / 运行暂计 / 待确认三组。
///
/// 收的是服务层的 IPC 请求 DTO（[`stats::TodayQuery`]，**不另造请求形状**）：
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
    request: stats::TodayQuery,
) -> Result<stats::TodayView, ErrorResponse> {
    run_command(
        "stats_today",
        window.label(),
        &state,
        Vec::new(),
        move |app| stats_today_impl(app, request),
    )
    .await
}

/// [`stats_today`] 的命令体（IPC 包装只做转发）。
pub fn stats_today_impl(
    app: &mut AppState,
    request: stats::TodayQuery,
) -> Result<stats::TodayView, AppError> {
    app.stats_today(&request)
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

/// 对账：一次处理该会话的**全部**待确认区间（确认或丢弃）。
///
/// 只接 `recovering`（R6）：非 `recovering` 一律拒绝——包括「已经确认过、想再确认一次」，
/// 所以这条命令**没有 `Unchanged` 这一支**（拒绝而不是当成又一笔写，见服务层文档）。
#[tauri::command]
pub async fn reconcile(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: ReconcileRequest,
) -> Result<recovery::ReconcileReport, ErrorResponse> {
    let targets = vec![target(AuthorityKind::Session, &request.session_id)];
    let broadcaster = Arc::clone(state.broadcaster());
    run_command("reconcile", window.label(), &state, targets, move |app| {
        reconcile_impl(app, &broadcaster, request)
    })
    .await
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

/// 任务状态跃迁：同一事务里联动该任务的会话（结束 / 暂停），并广播一次。
#[tauri::command]
pub async fn transition_task(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: TransitionTaskRequest,
) -> Result<tasks::TaskTransitionReport, ErrorResponse> {
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

/// [`retry_recovery`] 的命令体（IPC 包装只做转发）。
pub fn retry_recovery_impl(
    app: &mut AppState,
    request: EpochRequest,
) -> Result<TimerSnapshot, AppError> {
    app.retry_recovery(&request.expected_data_epoch)
}

/// 全局待确认概览（命令 8）：恢复页与「待确认」栏的唯一数据源。
#[tauri::command]
pub async fn attention_overview(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    request: EpochRequest,
) -> Result<recovery::AttentionOverview, ErrorResponse> {
    run_command(
        "attention_overview",
        window.label(),
        &state,
        Vec::new(),
        move |app| attention_overview_impl(app, request),
    )
    .await
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

/// 读一次权威身份，并且**只接受请求带来的期望值**（命令 9/10/11 共用）。
///
/// 与 `storage::guards::guard_epoch` **同一判据**（都读同一行 `app_meta` 再比期望值）；
/// 区别只是**命令层不得引用 `storage::`**（`scripts/check-layers.ps1` 第 1 条），所以走
/// 公开的握手入口。判据没有被削弱：比的是**请求带来的**期望值，不是"读出来再跟自己比"。
///
/// 调用方必须在同一个临界区里用它：拿到锁之后读到的身份，才配得上"这次操作的身份"。
fn require_epoch(
    app: &AppState,
    expected_data_epoch: &str,
) -> Result<handshake::RevisionSnapshot, AppError> {
    let authority = handshake::get_revision(app.db()?)?;
    if authority.data_epoch != expected_data_epoch {
        return Err(AppError::DataEpochMismatch);
    }
    Ok(authority)
}

/// 一次导出请求**解析之后**的形状：判别式 + 该形状真正要用的标量。
///
/// 把"判别式"与"标量"绑在一个枚举里，命令体就不必再 `unwrap` 一次可选字段——
/// 校验与取值只有一处（`parse_export_request`），也不会有"校验过了但取错分支"的缝。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportRequestShape {
    /// 范围明细（`[from, to)`）。
    Json { from: i64, to: i64 },
    /// 自然周回顾（`anchor` = 这一周里的任一刻；`None` = 本周）。
    Markdown { anchor: Option<i64> },
}

/// 解析导出的 `format` 与它该带的标量（字符串枚举显式解析，见模块头）。
///
/// 不适用却传了的字段**拒绝**而不是忽略（计划第三轮契约收口）。
fn parse_export_request(request: &ExportRequest) -> Result<ExportRequestShape, AppError> {
    let shape = match request.format.trim() {
        "json" => {
            let (Some(from), Some(to)) = (request.from, request.to) else {
                return Err(AppError::Domain {
                    detail: "JSON 导出必须同时给出 from 与 to（半开范围）。".into(),
                });
            };
            if request.anchor.is_some() {
                return Err(AppError::Domain {
                    detail: "JSON 导出不接受 anchor（那是 Markdown 周回顾的参数）。".into(),
                });
            }
            ExportRequestShape::Json { from, to }
        }
        "markdown" => {
            if request.from.is_some() || request.to.is_some() {
                return Err(AppError::Domain {
                    detail: "Markdown 周回顾不接受 from/to：周界由服务按 anchor 算，\
                             不能用任意范围冒充自然周。"
                        .into(),
                });
            }
            ExportRequestShape::Markdown {
                anchor: request.anchor,
            }
        }
        "" => {
            return Err(DomainError::EmptyText {
                field: "导出格式"
            }
            .into())
        }
        other => return Err(unknown_enum_value("导出格式", other)),
    };
    Ok(shape)
}

/// 导出产物的目录名（生产缺省：`<app_data_dir>/exports`，与备份同在一个应用数据目录下）。
const EXPORT_DIR_NAME: &str = "exports";

/// 备份阶段标记：`backup_consistent` 的失败 `detail` 前缀。用户手动备份与"迁移前备份"
/// 分开，日志里一眼看得出是哪一次失败。
const USER_BACKUP_STAGE: &str = "user backup";

/// 导出产物的落盘目录（**只算路径**，不创建）。
///
/// `dir_override` 与 `services::backup::backup_consistent` 的同名参数同一姿势：生产传
/// `None`（走 [`crate::platform::paths::app_data_dir`]），测试注入临时目录——**绝不写
/// 真实的 `%APPDATA%`**（`tests/startup_order.rs` 与 `tests/backup_restore.rs` 都立过
/// 这条规矩，环境变量那条路还要求串行执行、会把并行测试拖成互相干扰）。
fn export_dir(dir_override: Option<&Path>) -> Result<PathBuf, AppError> {
    match dir_override {
        Some(dir) => Ok(dir.to_path_buf()),
        None => Ok(crate::platform::paths::app_data_dir()
            .map_err(|error| export_io("resolve the app data directory", error))?
            .join(EXPORT_DIR_NAME)),
    }
}

/// 导出产物的文件名：`worktrace-export-<形状>-<Unix 毫秒>.<扩展名>`。
///
/// 时间戳来自**同一条时钟接缝**（`AppState::now_ms`），与备份产物命名同一口径。名字的
/// 粒度是**毫秒**，所以同一毫秒内的两次同形状导出会撞成同一个名字——**这是"重名"，
/// 不是"同一份产物"**：两次 JSON 导出可以问的是**不同的范围**（内容就不同），
/// 两次周回顾也可以问不同的周。所以撞名时**拒绝覆盖**（见 [`write_export`]），
/// 与 `services::backup::backup_consistent` 的"同名即拒绝、不覆盖"同一口径。
///
/// 取舍（fix round 1，评审 Minor-4）：走 IPC 时每次导出都是一次独立往返，
/// 落进同一毫秒**实际不可达**；而"静默覆盖"的代价是把用户可能正开着的那份文件换成
/// 内容不同的另一份。真要支持批量导出，正确的改法是名字里加序号/UUID，
/// **不是**退回静默覆盖。
fn export_file_name(shape: ExportRequestShape, at_ms: i64) -> String {
    match shape {
        ExportRequestShape::Json { .. } => format!("worktrace-export-json-{at_ms}.json"),
        ExportRequestShape::Markdown { .. } => {
            format!("worktrace-export-weekly-{at_ms}.md")
        }
    }
}

/// 把内容写进导出目录，返回**绝对**路径与写出的字节数。
///
/// 两条不变量（fix round 1 的 Minor-3 / Minor-4）：
///
/// 1. **不留半份产物**：先写同目录的 `<名字>.partial`、写成功后再改名为正式名
///    （同卷改名）。磁盘满 / 写一半失败 ⇒ `exports/` 里**不会**出现一个名字正常、
///    内容截断的文件（用户不会以为它是一份完整导出）；失败时顺手清掉临时文件
///    （best-effort：删不掉也不改变"这次导出失败"这个结论）。
/// 2. **不覆盖已有产物**：同名产物已经存在 ⇒ 拒绝（与 `backup_consistent` 同一口径），
///    而不是把内容换成另一份（见 [`export_file_name`] 的取舍说明）。
fn write_export(dir: &Path, file_name: &str, text: &str) -> Result<(PathBuf, u64), AppError> {
    std::fs::create_dir_all(dir)
        .map_err(|error| export_io("create the export directory", error))?;
    let path = dir.join(file_name);
    if path.exists() {
        return Err(AppError::Storage {
            detail: format!("export: artifact already exists: {}", path.display()),
        });
    }
    let staged = dir.join(format!("{file_name}.partial"));
    let bytes = text.as_bytes();
    if let Err(error) = std::fs::write(&staged, bytes) {
        let _ = std::fs::remove_file(&staged);
        return Err(export_io("write the artifact", error));
    }
    if let Err(error) = std::fs::rename(&staged, &path) {
        let _ = std::fs::remove_file(&staged);
        return Err(export_io("publish the artifact", error));
    }
    Ok((absolute_path(path)?, bytes.len() as u64))
}

/// 落盘失败的统一形状：`code` 是 `STORAGE_ERROR`，`detail` 是**内部诊断**（任务 3 的
/// 「落盘的失败路径」条：目标目录不可写 / 磁盘满 / 路径过长都走这条，用户看到的是
/// `AppError::message()` 那句固定文案，不是这里拼的英文）。
fn export_io(stage: &str, error: std::io::Error) -> AppError {
    AppError::Storage {
        detail: format!("export: {stage}: {error}"),
    }
}

/// 响应里给的是**绝对**路径（`revealItemInDir` 与"复制路径"都要求它）。
///
/// 生产缺省路径本来就是绝对的（`app_data_dir()` 由 `%APPDATA%` / `$XDG_DATA_HOME` 推出）；
/// 这里只为**注入的相对目录**兜底：拼上当前目录，而不是报错。
fn absolute_path(path: PathBuf) -> Result<PathBuf, AppError> {
    if path.is_absolute() {
        return Ok(path);
    }
    let cwd =
        std::env::current_dir().map_err(|error| export_io("resolve an absolute path", error))?;
    Ok(cwd.join(path))
}

/// 路径 → 响应里的字符串。不是合法 UTF-8 就拒绝：界面拿它去开文件管理器，
/// 半个路径比一条明确的错误更糟。
fn path_to_string(path: &Path) -> Result<String, AppError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| AppError::Storage {
            detail: format!("path is not valid UTF-8: {}", path.display()),
        })
}

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
/// `dir_override` 见 [`export_dir`]：生产 IPC 传 `None`，用例注入临时目录。
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

/// [`export_data`] 的命令体（IPC 包装只做转发）。
pub fn export_data_impl(
    app: &mut AppState,
    request: ExportRequest,
    dir_override: Option<&Path>,
) -> Result<ExportResult, AppError> {
    let shape = parse_export_request(&request)?;
    // 文件名里的时间戳：在**生成之前**取一次（失败就不该留下半份产物），
    // 走与业务写同一条时钟接缝（`AppState::now_ms` → 协调器 → `Clock`）。
    let at_ms = app.now_ms()?;
    let (text, data_epoch, revision) = match shape {
        ExportRequestShape::Json { from, to } => {
            let query = stats::StatsRangeQuery {
                from,
                to,
                timezone: request.timezone,
                expected_data_epoch: request.expected_data_epoch,
            };
            let export = app.export_json(&query)?;
            (export.text, export.data_epoch, export.revision)
        }
        ExportRequestShape::Markdown { anchor } => {
            let query = export::WeeklyQuery {
                timezone: request.timezone,
                anchor,
                expected_data_epoch: request.expected_data_epoch,
            };
            let export = app.export_weekly_markdown(&query)?;
            (export.text, export.data_epoch, export.revision)
        }
    };
    let (path, bytes) = write_export(
        &export_dir(dir_override)?,
        &export_file_name(shape, at_ms),
        &text,
    )?;
    Ok(ExportResult {
        path: path_to_string(&path)?,
        bytes,
        data_epoch,
        revision,
    })
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

/// [`backup`] 的命令体（IPC 包装只做转发 + 凑一只同源钟）。
///
/// `dir_override` 与 [`export_data_impl`] 同一姿势：生产 `None`，用例注入临时目录。
pub fn backup_impl(
    app: &mut AppState,
    clock: &(dyn Clock + Send),
    request: BackupRequest,
    dir_override: Option<&Path>,
) -> Result<BackupResult, AppError> {
    // 维护态门禁：命令体**自己**也要挡一次。`run_command` 那一层也挡（取锁之后第一句），
    // 但用例直接调命令体，且"备份在维护态被拒"是计划第 10 行点名的判据。
    app.guard_writable()?;
    // 身份守卫：过期请求**在产出任何东西之前**被拒（不留下半份产物）。
    // 信封是写命令的共同形状，只是 `backup_consistent` 是六参原语、不收它——校验因此
    // 在这里用握手入口做，判据与 `guard_epoch` 同源（见 [`require_epoch`]）。
    let envelope = WriteEnvelope::for_create(request.expected_data_epoch);
    require_epoch(app, &envelope.expected_data_epoch)?;
    let artifact = backup::backup_consistent(
        dir_override,
        app.db()?.connection(),
        backup::current_version(app.db()?.connection())?,
        clock,
        USER_BACKUP_STAGE,
        app.diagnostics(),
    )?;
    // 响应里的身份与版本在**产物落地之后**读回：备份不改业务事实，所以它们与上面那次
    // 守卫读到的相同；这样写是为了让"响应里的值 = 响应时刻库里的值"成为结构性事实，
    // 而不是"因为没人写所以碰巧相等"。
    let authority = require_epoch(app, &envelope.expected_data_epoch)?;
    let bytes = std::fs::metadata(&artifact)
        .map_err(|error| AppError::Storage {
            detail: format!("{USER_BACKUP_STAGE}: stat the artifact: {error}"),
        })?
        .len();
    Ok(BackupResult {
        path: path_to_string(&artifact)?,
        bytes,
        data_epoch: authority.data_epoch,
        revision: authority.revision,
    })
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

/// [`restore`] 的命令体：二次确认 → 身份守卫 → **一次调用体内**走完三段。
///
/// 收 `&SharedApp` / `&Broadcaster` / `&ClockSource`（不是 `&mut AppState`）：三段各自
/// 取锁、中间那段不持锁，命令体自己不持有任何借用——这正是"不进 `run_command`"在类型
/// 上的样子。返回的 `revision` 来自 [`backup::RestoreOutcome::revision`]，即**锁内冻结**
/// 的那个值（见 [`RestoreResult`]）。
pub fn restore_impl(
    app: &SharedApp,
    broadcaster: &Broadcaster,
    clock: &backup::ClockSource,
    request: RestoreRequest,
) -> Result<RestoreResult, AppError> {
    // ⓪ **持锁调用 ⇒ 立刻拒绝**，判据与 `restore_from_backup` 第一句**同源**
    //    （`holds_app_lock`），但位置必须在这里：下面 ② 的身份守卫要 `lock_app`，
    //    而错误接线（把恢复塞回 `run_command` 的闭包）正持着那把非重入 `Mutex`——
    //    没有这一句，现象是**静默死锁**（服务入口那句要到 ① 段才跑得到，太晚）。
    //    有了它，"接线错误"是**红**：明确的 `STORAGE_ERROR`，什么都不碰。
    if holds_app_lock(app) {
        return Err(AppError::Storage {
            detail: "恢复流程必须在锁外开始（命令体自己按段取锁），不要在持有串行边界的线程上调用"
                .to_string(),
        });
    }
    // ① 二次确认：命令层**再校验一次**。拒绝时一个字都不写：不进维护态、不碰文件、
    //    连库都不读（`confirmed: false` 与"文件不存在"是两件事，先答前一件）。
    if !request.confirmed {
        return Err(AppError::Domain {
            detail: "恢复会替换当前数据库，需要明确的二次确认（confirmed 必须为 true）。".into(),
        });
    }
    // ② 身份守卫：在**进维护态之前**（失败时进程状态与磁盘一个字节都没变）。
    //    这一小段自己取锁、随即释放——不能把 guard 带进 ③：`restore_from_backup` 的
    //    第一句就是"持锁调用 ⇒ 拒绝"，带进去会**红**（这正是我们要的行为，不是要绕过的）。
    {
        let state = lock_app(app);
        require_epoch(&state, &request.expected_data_epoch)?;
    }
    // ③ 三段流程必须在**这一次调用体内**：服务入口自己把它们连起来，命令层不得拆成
    //    多次 IPC（P6 验收 §6 第一条）。
    let outcome = backup::restore_from_backup(
        app,
        broadcaster,
        Path::new(&request.backup_path),
        None,
        clock,
    )?;
    // `applied` 照抄 `outcome.committed`：服务把"没换成"当 `Err` 交回（回滚成功也报原始
    // 原因），所以能走到这里就恒为 `true`。
    Ok(RestoreResult {
        data_epoch: outcome.data_epoch,
        revision: outcome.revision,
        applied: outcome.committed,
    })
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
