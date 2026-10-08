//! Worktrace 后端库（**组合根**）。
//!
//! 分层（01 §2，总纲 §9）：`commands → services → {storage, domain, platform}`。
//! `commands` 不直连 SQL 也不接受 `Connection`；`storage` 不反向引用
//! `commands` 也不调用 `platform`；`domain` 不做 IO。
//!
//! P1 交付 `domain`/`platform`/`storage`/`commands` 的基座与 `error`；
//! `services` 自 P2 起逐份加入。`envelope` 与 `error` 一样住在 crate 根：
//! `storage` 与 `services` 都要用它，而它们不得依赖 `commands`。
//!
//! # 启动只有一条入口（P7 Task 0/1）
//!
//! 本文件**不重排**「单实例 → 打开库并迁移 → 建 `application_run` → 恢复扫描 →
//! 协调器与周期采样 → 开窗口」这段顺序：顺序写在
//! [`services::bootstrap::startup`] 里，这里只提供它要的时钟、事件出口、探针与
//! 「开窗口」回调。`scripts/check-layers.ps1` 的入口点规则把这条从约定变成机器检查
//! （`lib.rs`/`main.rs` 不得出现 `Db::open` / `migrate(` / `run_repo::`）。
//!
//! 窗口在**最后一步**才开：后端就绪之前窗口不该开始拉数据。前端能发 IPC 的时机
//! 在事件循环启动之后，而 `setup` 在事件循环之前跑完，所以「先就绪、后开窗」在这条
//! 接线上是成立的。
//!
//! # 托盘、窗口生命周期与唤醒接收（P7 Task 4）
//!
//! 这一层是**组合根**，Task 4 的三处接线都在这里，且都只是接线：
//!
//! - **托盘**：[`platform::tray::build`] 装配菜单（F-011 的四项 + 「完成」预留项），
//!   动作交给 [`on_tray_action`]；后者把服务动作转给 `commands::` 那一侧的命令体
//!   （与 IPC 同一批入口），窗口动作转给 [`platform::window`]。托盘里没有业务判断，
//!   `platform` 也不反向引用上层（分层门禁第六条）。
//! - **关掉全部窗口不退出**（F-009）：[`RunEvent::ExitRequested`] 只在 `code = None`
//!   （用户关掉最后一个窗口）时 `prevent_exit`，程序化的 `exit` 一律放行——判定在
//!   [`platform::window::should_prevent_exit`]。
//! - **单实例唤醒的接收侧**：[`platform::window::spawn_activation_watcher`] 消费
//!   Task 0 的 `take_activation_request()`，把请求变成「抬起主窗（已关则重建）」——
//!   Task 0 报告里那条「发送侧 + 接收原语」的交付边界到这里闭环。
//!
//! # 实验器材：dev 注入开关与 `sync-lab`（P7 Task 6a）
//!
//! 真实双窗口实验（`tests/manual-sync.md`）要的四条 dev 命令注册在下面
//! `invoke_handler` 的末尾，**每条都带 `#[cfg(debug_assertions)]`**；命令体所在的
//! `commands::dev` 整份同样带守卫，实验窗口的平台半边在 `platform::sync_lab`。
//! 发布构建里它们不参与编译，也就无从注册（守卫由 `tests/dev_injections.rs` 核对）。
//!
//! 窗口仍然只在启动第⑥步开主窗——`sync-lab` 由人在需要时经 dev 命令开，
//! `manual-shell.md` 的既有验收步骤不受影响。

pub mod commands;
pub mod domain;
pub mod envelope;
pub mod error;
pub mod platform;
pub mod services;
pub mod storage;

pub use error::AppError;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::platform::clock::SystemClock;
use crate::platform::diagnostics::Diagnostics;
use crate::platform::tray::{self, TrayAction};
use crate::platform::window;
use crate::services::bootstrap::{
    self, PreMigrationBackup, Startup, StartupConfig, StartupProbe, StartupStep,
};
use crate::services::events::{EventEnvelope, EventSink};

use tauri::{AppHandle, Emitter, Manager, RunEvent};

/// 前端订阅事件用的频道名（`listen("worktrace:event", …)`）。
///
/// 00 §5 的信封字段是 **payload** 的形状，频道名不属于信封，所以它属于这个 Tauri
/// 适配层，不放进 `services::events`（那一层不认识 Tauri）。Task 2 的 `domainState`
/// 按这个名字订阅，Task 6a 的双窗口实验同。
const EVENT_CHANNEL: &str = "worktrace:event";

/// 生产事件出口：把信封 emit 给所有窗口。
///
/// [`EventSink::broadcast`] 是**投递式**的：`emit` 失败只被 `Broadcaster` 记成诊断，
/// 不回滚任何已提交业务（00 §4：广播失败不得让用户重做一次已经成功的操作）。
struct TauriEventSink {
    app: tauri::AppHandle,
}

impl EventSink for TauriEventSink {
    fn broadcast(&self, envelope: &EventEnvelope) -> Result<(), String> {
        self.app
            .emit(EVENT_CHANNEL, envelope)
            .map_err(|error| error.to_string())
    }
}

/// 启动次序的生产探针：每一步写进**正式诊断日志**（P6 Task 2b 收掉 2a 交接的那条 minor）。
///
/// 为什么不再是 `println!`：release 的 Windows 子系统**没有控制台**（`tauri.conf.json` 的
/// `windows_subsystem`），输出直接丢弃——真机上「卡在哪一步」「迁移前备份走了哪条分支」
/// 就再也查不到。落点与状态跃迁（维护态、故障态）写的是**同一个文件**：路径都来自同一份
/// [`StartupConfig::diagnostic_log`]（经 `Diagnostics::from_optional_path`）。
struct StartupTrace {
    diagnostics: Diagnostics,
}

impl StartupProbe for StartupTrace {
    fn step(&self, step: StartupStep) {
        self.diagnostics
            .record("startup.step", &format!("step={}", step.as_str()));
    }

    /// 迁移前备份的按需判据结果。三条分支里有两条是「这次没备份」，原因不同 ⇒ 分开打，
    /// 免得事后只看到「没有产物」而分不清是首启、版本相等，还是判据坏了。
    fn pre_migration_backup(&self, outcome: PreMigrationBackup) {
        self.diagnostics.record(
            "startup.pre_migration_backup",
            &format!("outcome={outcome:?}"),
        );
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 事件循环退出之后，单实例唤醒的轮询线程必须停：那时再碰窗口对象没有意义。
    // `App::run` 最终会 `std::process::exit`，但这个窗口期不值得赌。
    let alive = Arc::new(AtomicBool::new(true));

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            // 握手
            commands::get_revision,
            // 项目（F-004）
            commands::list_projects,
            commands::list_selectable_projects,
            commands::create_project,
            commands::rename_project,
            commands::archive_project,
            // 标签（F-005）
            commands::list_tags,
            commands::create_tag,
            commands::tags_of_task,
            commands::tag_task,
            commands::untag_task,
            // 任务（F-002）
            commands::list_tasks,
            commands::create_task,
            commands::clarify_ready,
            commands::set_task_project,
            // 今日计划（F-010）
            commands::plan_for,
            commands::add_to_plan,
            commands::remove_from_plan,
            // 计时（P2）
            commands::timer_snapshot,
            commands::timer_tick,
            commands::start_timer,
            commands::pause_timer,
            commands::resume_timer,
            commands::finish_timer,
            // P7 Task 6a 的 dev 注入开关与实验窗口（**只在 debug 构建**）。
            //
            // 每条臂上的 `#[cfg(debug_assertions)]` 由 `tauri::generate_handler!`
            // 原样交给生成的 match 臂（tauri-macros 的 `command/handler.rs` 用
            // `Attribute::parse_outer()` 读每条命令前的属性），而命令体所在的
            // `commands::dev` 整份也带守卫 ⇒ **发布构建里这几条既不编译也不注册**，
            // 不是「运行时关掉」。两道守卫由 `tests/dev_injections.rs` 读源码核对。
            #[cfg(debug_assertions)]
            commands::dev::__p7_drop_next_event,
            #[cfg(debug_assertions)]
            commands::dev::__p7_delay_next_query_ms,
            #[cfg(debug_assertions)]
            commands::dev::__p7_replay_event,
            #[cfg(debug_assertions)]
            commands::dev::__p7_open_sync_lab,
        ])
        .setup({
            let alive = Arc::clone(&alive);
            move |app| setup(app, &alive)
        })
        .build(tauri::generate_context!())
        .expect("error while building Worktrace");

    app.run(move |_app, event| match event {
        // F-009：关掉全部窗口**不退出**——托盘还在、周期采样还在跑。
        // `code = None` 是「用户交互」（最后一个窗口被关掉）；`Some(_)` 是程序化的
        // `AppHandle::exit`（托盘「退出」、第二次启动的自己退出），必须放行，
        // 否则托盘的「退出」会变成一个关不掉的进程。
        RunEvent::ExitRequested { code, api, .. } => {
            if window::should_prevent_exit(code) {
                api.prevent_exit();
            }
        }
        // ⚠️ 这里**不**兜底再调一次显式退出（fix round 1，评审 M4）：`App::run` 最后
        // 直接 `std::process::exit`，`RunningApp` 的 `Drop` 不会跑，所以
        // **写 `clean_exit_at` 只发生在显式退出（托盘「退出」）那条路径上**——它自己
        // 已经做过了。P8 若新增退出入口，必须显式调 `RunningApp::shutdown()`，
        // 否则那一次 run 会以「没有 `clean_exit_at`」结束；那是给崩溃/强杀准备的
        // 恢复输入（F-015），不该成为正常退出的常态。
        RunEvent::Exit => alive.store(false, Ordering::SeqCst),
        _ => {}
    });
}

/// 启动接线：调唯一的启动入口，然后在它**成功之后**挂上托盘与唤醒接收。
///
/// 六步顺序一个字没动（见模块头）：本函数只提供 `startup` 要的时钟、事件出口、
/// 探针与「开窗口」回调。
fn setup(app: &mut tauri::App, alive: &Arc<AtomicBool>) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();

    // 库与锁的路径**只解析一次**：唤醒请求写在锁文件旁边，两次解析万一不一致，
    // 第二次启动的通知就会石沉大海。
    let config = StartupConfig::from_app_paths()?;
    let lock_path = config.lock_path.clone();

    // ⑥ 开窗口：抬起主窗（若已经被关掉则按 `tauri.conf.json` 重建）。失败即启动失败
    // （bootstrap 会停掉采样线程，不留一个跑着的后台线程）。
    let window_handle = handle.clone();
    let open_window = move || -> Result<(), AppError> {
        window::raise_or_rebuild_main(&window_handle)
            .map(|_| ())
            .map_err(|error| AppError::Storage {
                detail: format!("open_window: {error}"),
            })
    };

    let sink = Arc::new(TauriEventSink {
        app: handle.clone(),
    });
    // 启动探针与状态跃迁（维护态、故障态）写**同一个**诊断文件：路径都来自这份 config。
    let trace = StartupTrace {
        diagnostics: Diagnostics::from_optional_path(config.diagnostic_log.clone()),
    };
    // 时钟**只建一个**（P6 Task 2c）：克隆一份给 OS 事件源，原件交给 `startup`（协调器持有）。
    // 各建一个会得到两个 `Instant` 原点，事件的边界样本必然过不了 `system_pause` 的校验，
    // 现象是「每次锁屏都掉进 recovering」——R-02 静默落空。见 `platform::clock::SystemClock`。
    let clock = SystemClock::new();
    let event_clock = clock.clone();
    match bootstrap::startup(config, Box::new(clock), sink, &trace, &open_window)? {
        Startup::Running(running) => {
            // 事件源的回调捕获 `SharedApp` 与广播出口**本身**（不是 Tauri 托管状态）：
            // 先克隆、再 `manage`，回调因此不需要在事件线程上查 Tauri 状态。
            let shared = Arc::clone(running.app());
            let broadcaster = Arc::clone(running.broadcaster());

            // `RunningApp` 必须活到进程退出：它的 `Drop` 会停掉采样线程并释放
            // 单实例锁。交给 Tauri 托管，生命周期就等于进程。
            app.manage(*running);

            // 托盘（F-011）：四项 + 「完成」预留项。菜单点到的动作交给
            // `on_tray_action`，由它调 `commands::` 那一侧的命令体。
            tray::build(&handle, on_tray_action)?;

            // 单实例唤醒的接收侧：轮询请求文件 → 抬起（已关则重建）主窗。
            window::spawn_activation_watcher(handle.clone(), lock_path, Arc::clone(alive));

            // 正式 OS 事件源（P6 Task 2c，锁屏/解锁、休眠/唤醒、系统改时）：
            // 装在 `startup` **成功之后**，与上面两条同一段。它只做适配——事件怎么
            // 处理由 `services::bootstrap` 那条与周期采样并列的路径决定。
            // 起不来（平台不支持 / 注册失败）只记诊断：不 panic，也不假装成功；
            // 周期采样在另一条线程上，不受影响。
            // 传的是**工厂**：窗口这类平台对象只在创建它们的线程上有效，事件线程由
            // `start_system_events` 起，所以源要在那条线程上造（见该函数的说明）。
            if let Err(error) = bootstrap::start_system_events(
                move || crate::platform::system_events::os_source(event_clock),
                Arc::clone(alive),
                shared,
                broadcaster,
                &trace.diagnostics,
            ) {
                eprintln!("[worktrace] system events unavailable: {error}");
            }
            Ok(())
        }
        Startup::AlreadyRunning { notified } => {
            // 已有实例持有锁：本进程**不打开库、不迁移、不建 run、不启动计时**
            // （F-016），通知发没发出去都要退出。
            println!("[worktrace] another instance is running (notified={notified})");
            handle.exit(0);
            Ok(())
        }
    }
}

/// 托盘动作去往哪一类入口（Task 4）。
///
/// 与 [`tray_dispatch`] 一起构成「动作 → 入口」的**唯一**一张表：`on_tray_action`
/// 只 match 它，用例断言它。为什么单列一张表（fix round 1，评审 I2）：这段路由原先
/// 直接写在 `on_tray_action` 的 match 里，把 `Pause` 接到 `Quit` 上不会有任何用例变红——
/// 而「托盘动作与界面动作走同一批命令」这条要求，接缝正是这几行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayDispatch {
    /// 窗口动作：抬起主窗（已关则按配置重建）。**不是**命令。
    Window,
    /// 复用 `commands::tray_pause_impl`（内部就是 `pause_timer_impl`）。
    Pause,
    /// 复用 `commands::tray_quit_impl`（内部就是 Task 0 的显式退出入口）。
    Quit,
}

/// 动作 → 去向。**只有这一处**决定托盘动作往哪走。
///
/// 注意它只分类、不执行：真正落到命令体上的映射在 `commands::`（`tray_pause_impl` /
/// `tray_quit_impl`），所以托盘与 IPC 走的是同一批入口。
pub fn tray_dispatch(action: TrayAction) -> TrayDispatch {
    match action {
        TrayAction::CurrentTask | TrayAction::QuickCapture => TrayDispatch::Window,
        TrayAction::Pause => TrayDispatch::Pause,
        TrayAction::Quit => TrayDispatch::Quit,
    }
}

/// 托盘动作 → 入口（Task 4 的接线点）：按 [`tray_dispatch`] 分派。
///
/// - **窗口动作**（当前任务 / 快速捕获）：抬起主窗（已关则重建）；
/// - **服务动作**（暂停 / 退出）：交给 `commands::` 那一侧——与 IPC 同一批入口，
///   托盘不另写业务逻辑。
fn on_tray_action(app: &AppHandle, action: TrayAction) {
    match tray_dispatch(action) {
        // P7 的落点是「把主窗抬起来」：视图跳转（定位到当前任务 / 聚焦捕获输入框）
        // 依赖前端的视图与路由，而 P7 是固定布局、没有路由——登记为 P8。
        TrayDispatch::Window => match window::raise_or_rebuild_main(app) {
            Ok(plan) => println!("[worktrace] tray: {} -> {plan:?}", action.menu_id()),
            Err(error) => eprintln!("[worktrace] tray: {} failed: {error}", action.menu_id()),
        },
        TrayDispatch::Pause => commands::spawn_tray_pause(app),
        TrayDispatch::Quit => commands::spawn_tray_quit(app),
    }
}
