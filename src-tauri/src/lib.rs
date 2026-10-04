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

pub mod commands;
pub mod domain;
pub mod envelope;
pub mod error;
pub mod platform;
pub mod services;
pub mod storage;

pub use error::AppError;

use crate::platform::clock::SystemClock;
use crate::services::bootstrap::{self, Startup, StartupConfig, StartupProbe, StartupStep};
use crate::services::events::{EventEnvelope, EventSink};

use tauri::{Emitter, Manager};

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

/// 启动次序的生产探针：每步打印一行。
///
/// Task 0 只留了 `NoProbe` 占位，这里兑现「生产侧换诊断日志」：真机上也能看见六步
/// 都在、顺序固定，而不是只有测试里那一条断言。P6 接入正式诊断日志时替换本类型
/// （release 的 Windows 子系统没有控制台，输出直接丢弃，不会崩）。
struct StartupTrace;

impl StartupProbe for StartupTrace {
    fn step(&self, step: StartupStep) {
        println!("[worktrace] startup: {}", step.as_str());
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // ⑥ 开窗口：**最小实现**。真实窗口由 `tauri.conf.json` 的 `app.windows`
            // 建好，这里只保证它在最后一步显示出来；`open_window` 失败即启动失败
            // （bootstrap 会停掉采样线程，不留一个跑着的后台线程）。
            //
            // ⚠️ Task 4 会替换这段：关掉全部窗口不退出、重开窗口先拉快照、托盘入口，
            // 以及消费 `platform::single_instance::take_activation_request()` 的
            // 「唤起既有主窗」。本任务只把第⑥步接上，不碰窗口生命周期。
            let window_handle = handle.clone();
            let open_window = move || -> Result<(), AppError> {
                if let Some(window) = window_handle.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
                Ok(())
            };

            let sink = std::sync::Arc::new(TauriEventSink {
                app: handle.clone(),
            });
            match bootstrap::startup(
                StartupConfig::from_app_paths()?,
                Box::new(SystemClock::new()),
                sink,
                &StartupTrace,
                &open_window,
            )? {
                Startup::Running(running) => {
                    // `RunningApp` 必须活到进程退出：它的 `Drop` 会停掉采样线程并释放
                    // 单实例锁。交给 Tauri 托管，生命周期就等于进程。
                    app.manage(*running);
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
        })
        .run(tauri::generate_context!())
        .expect("error while running Worktrace");
}
