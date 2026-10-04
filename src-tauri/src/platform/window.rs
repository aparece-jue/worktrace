//! 主窗生命周期与「唤起既有主窗」的接收侧（F-009 / F-016，P7 Task 4）。
//! **平台叶子**：窗口对象与单实例请求文件的适配，不懂业务。
//!
//! ## 三件事
//!
//! 1. **关掉全部窗口不退出**（F-009）：`RunEvent::ExitRequested { code: None }` 是
//!    「用户关掉了最后一个窗口」，[`should_prevent_exit`] 说这种时候要阻止退出——
//!    托盘还得在、周期采样还得跑。程序化的 `AppHandle::exit(code)`（`Some(_)`）
//!    必须放行，否则托盘的「退出」会变成一个关不掉的进程。
//! 2. **重开窗口先拉快照**：主窗被关掉之后走 [`ActivationPlan::Rebuild`]——按
//!    `tauri.conf.json` 那份窗口配置重建，于是新窗口是一次**全新页面加载**，
//!    前端挂载时按 Task 2 的顺序「先监听、再拉一致快照」。抬起一个还活着的窗口
//!    则是 `show` + 还原 + 聚焦（隐藏窗口重新显示前的校验是前端规则 4 的事）。
//! 3. **单实例唤醒的接收侧**：Task 0 交付了发送侧与接收原语
//!    （`single_instance::{request_activation, take_activation_request}`），
//!    [`spawn_activation_watcher`] 把「请求 → 抬起/重建主窗」接上，这一半闭环。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime, WebviewWindow, WebviewWindowBuilder};

use crate::platform::single_instance;

/// 主窗的 label。
///
/// **必须与 `tauri.conf.json` 的 `app.windows[].label` 以及
/// `capabilities/default.json` 的 `windows` 名单逐字一致**：前者决定重建出来的窗口
/// 长什么样，后者决定它的 JS 有没有权限调命令（对不上就是「窗口能开、但什么都点不动」）。
/// 用例 `tests/shell_lifecycle.rs` 直接读那两个文件核对，不靠人记得。
pub const MAIN_WINDOW_LABEL: &str = "main";

/// 唤醒请求的轮询间隔（毫秒）。
///
/// 请求是**另一个进程**写下的一个文件，没有可订阅的内核对象，只能轮询。
/// 半秒 = 一次 `remove_file` 系统调用；人感觉不到延迟，也不值得为此上文件监听依赖。
pub const ACTIVATION_POLL_INTERVAL_MS: u64 = 500;

/// 一次唤醒请求该怎么处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationPlan {
    /// 没有请求：什么都不做（**不凭空建窗口**）。
    Ignore,
    /// 主窗还在：显示 + 取消最小化 + 聚焦。
    Raise,
    /// 主窗已经关掉：按 `tauri.conf.json` 的配置**重建**（全新页面加载 ⇒ 立即拉快照）。
    Rebuild,
}

/// **决策函数**：这次有请求吗？主窗还在吗？
///
/// 生产路径（[`spawn_activation_watcher`]）与用例调用的是**同一个**函数，
/// 不是一份与实现平行的真值表。
pub fn plan_activation(requested: bool, main_window_exists: bool) -> ActivationPlan {
    match (requested, main_window_exists) {
        (false, _) => ActivationPlan::Ignore,
        (true, true) => ActivationPlan::Raise,
        (true, false) => ActivationPlan::Rebuild,
    }
}

/// 「全部窗口关闭」这件事要不要阻止进程退出（F-009）。
///
/// `code` 来自 `RunEvent::ExitRequested`：`None` = 用户交互（关掉最后一个窗口），
/// `Some(_)` = 程序化 `AppHandle::exit`（托盘「退出」、第二次启动的自己退出）。
pub fn should_prevent_exit(code: Option<i32>) -> bool {
    code.is_none()
}

/// 主窗是否还在（关掉全部窗口之后就是 `false`）。
pub fn main_window_exists<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.get_webview_window(MAIN_WINDOW_LABEL).is_some()
}

/// 抬起主窗；已经关掉就按配置重建。返回实际走了哪条路（诊断用）。
///
/// 启动第⑥步、托盘的「当前任务」/「快速捕获」、单实例唤醒都用这一条。
pub fn raise_or_rebuild_main<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<ActivationPlan> {
    let plan = plan_activation(true, main_window_exists(app));
    apply_activation(app, plan)?;
    Ok(plan)
}

/// 执行一个 [`ActivationPlan`]。
///
/// **决策只在 [`plan_activation`] 一处**——本函数只负责把决定变成窗口操作。
pub fn apply_activation<R: Runtime>(app: &AppHandle<R>, plan: ActivationPlan) -> tauri::Result<()> {
    match plan {
        ActivationPlan::Ignore => Ok(()),
        ActivationPlan::Raise => {
            // 决策与执行之间窗口理论上可能刚好被关掉（同一条线程上概率极低）：
            // 这时不补建，下一次唤醒请求自然会走到 `Rebuild`。
            if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                raise(&window)?;
            }
            Ok(())
        }
        ActivationPlan::Rebuild => rebuild_main_window(app),
    }
}

/// 把窗口带到用户面前：显示、还原（若最小化）、聚焦。
fn raise<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    window.show()?;
    window.unminimize()?;
    window.set_focus()?;
    Ok(())
}

/// 关掉之后的重建：**从 `tauri.conf.json` 那一份窗口配置建**（标题、尺寸、最小尺寸
/// 都还是同一处来源，不在这里手抄第二份），所以新窗口与首次启动那个一模一样。
///
/// 新窗口 = 新的 JS 上下文 = 前端重新挂载：它按 Task 2 的顺序先监听再拉快照，
/// 于是「重开窗口立即拉快照」不需要核心额外推一次通知。
fn rebuild_main_window<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == MAIN_WINDOW_LABEL)
        .ok_or_else(|| {
            tauri::Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("tauri.conf.json has no window with label `{MAIN_WINDOW_LABEL}`"),
            ))
        })?;

    // 重建出来的窗口 label 不变 ⇒ `capabilities/default.json` 的 windows 名单照样覆盖它
    // （两处一致性由 `tests/shell_lifecycle.rs` 读文件核对）。
    WebviewWindowBuilder::from_config(app, config)?.build()?;
    Ok(())
}

/// 单实例唤醒请求的**接收侧**：轮询请求文件，按决策抬起/重建主窗。
///
/// 为什么是一个独立的线程：请求是另一个进程写下的文件，没有可订阅的事件；而这件事
/// **不属于周期采样**——F-009 明写采样驱动与窗口无关，把它塞进采样只会让「窗口」
/// 重新长回计时路径上。
///
/// `alive` 由组合根在 `RunEvent::Exit` 时置假：事件循环结束之后不该再碰窗口对象，
/// 这个线程随之退出（它是普通线程，不是 async 任务，零新增依赖）。
pub fn spawn_activation_watcher<R: Runtime>(
    app: AppHandle<R>,
    lock_path: PathBuf,
    alive: Arc<AtomicBool>,
) {
    let spawned = std::thread::Builder::new()
        .name("worktrace-activation".to_string())
        .spawn(move || {
            while alive.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(ACTIVATION_POLL_INTERVAL_MS));
                if !alive.load(Ordering::SeqCst) {
                    break;
                }

                // 读不到请求就当没有请求：拿不到一次通知绝不能把这个线程弄死
                // （通道本身失败仍由 Task 0 的原语报出来，故障路径硬化归 P6）。
                let requested =
                    single_instance::take_activation_request(&lock_path).unwrap_or(false);
                let plan = plan_activation(requested, main_window_exists(&app));
                if let Err(error) = apply_activation(&app, plan) {
                    eprintln!("[worktrace] activation: {plan:?} failed: {error}");
                }
            }
        });

    if let Err(error) = spawned {
        // 起不了这个线程只影响「第二次启动能不能唤起既有主窗」（F-016 的一半）：
        // 记诊断即可，不该把一个已经在跑的核心拖死。
        eprintln!("[worktrace] activation watcher failed to start: {error}");
    }
}
