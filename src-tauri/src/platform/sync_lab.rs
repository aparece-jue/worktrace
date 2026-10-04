//! 第二个窗口 `sync-lab`（P7 Task 6a 的真实双窗口实验）。**平台叶子**：只碰窗口对象，
//! 不懂业务，也不认识命令层。
//!
//! ## 为什么不是 `tauri.conf.json` 里的静态窗口
//!
//! 它是**实验器材**，不是产品的一部分：启动时不该出现，`manual-shell.md` 的既有验收
//! （单窗口、托盘、关窗后继续计时）也不该被它影响。所以只在需要时由
//! [`open_sync_lab`] 显式创建；触发入口是 debug 构建才存在的命令
//! `__p7_open_sync_lab`（`commands::dev`），发布构建里没有任何东西能调它。
//!
//! ## 与主窗的关系：**同一份配置，只改 label**
//!
//! [`lab_config_from`] 拿主窗那份 `tauri.conf.json` 配置（[`main_window_config`]）
//! 复制一份、只换 `label` ⇒ 入口 URL、标题、尺寸、最小尺寸都还是同一处来源，
//! 于是 `sync-lab` 跑的是**同一份前端**（`manual-sync.md` §2.0 的前提）。
//! 这条「只改 label」由 `tests/shell_lifecycle.rs` 拿真配置逐字段核对。
//!
//! ⚠️ **权限要单独登记**：`capabilities/default.json` 的 `windows` 名单按 label 匹配，
//! 新窗口不在名单里就**没有** `core:` 权限（监听事件都会失败）。所以
//! [`SYNC_LAB_WINDOW_LABEL`] 必须与那份名单逐字一致——同一个用例读文件核对。
//!
//! ## Windows 上只能在异步命令里建窗
//!
//! `WebviewWindowBuilder::from_config` 在同步命令/事件回调里会死锁（WebView2 的已知
//! 问题，见 tauri 文档），所以 [`open_sync_lab`] 的调用方是 `async` 命令。
//!
//! [`main_window_config`]: crate::platform::window::main_window_config

use tauri::utils::config::WindowConfig;
use tauri::{AppHandle, Manager, Runtime, WebviewWindow, WebviewWindowBuilder};

use crate::platform::window;

/// 实验窗口的 label。
///
/// **必须与 `capabilities/default.json` 的 `windows` 名单逐字一致**：对不上就是
/// 「窗口能开、但它的 JS 一条命令都调不了」。用例 `tests/shell_lifecycle.rs` 直接读
/// 那个文件核对，不靠人记得。
pub const SYNC_LAB_WINDOW_LABEL: &str = "sync-lab";

/// 实验窗口的配置 = 主窗那份**只改 label**。
///
/// 不在这里手抄第二份（标题/尺寸/入口 URL 一个都不写）：抄一份就是给
/// 「改了 `tauri.conf.json` 忘了改这里」留缝，而实验窗口与主窗不同源，
/// 实验的结论就不再能说明产品窗口的行为。
pub fn lab_config_from(main: &WindowConfig) -> WindowConfig {
    let mut config = main.clone();
    config.label = SYNC_LAB_WINDOW_LABEL.to_string();
    config
}

/// 开实验窗口；已经开着就抬起来（不重复建第二个）。
///
/// 返回的窗口 label 恒为 [`SYNC_LAB_WINDOW_LABEL`]——调用方（dev 命令）把它回给控制台，
/// 于是「开没开、开的是哪个 label」当场可见。
pub fn open_sync_lab<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    if let Some(existing) = app.get_webview_window(SYNC_LAB_WINDOW_LABEL) {
        existing.show()?;
        existing.unminimize()?;
        existing.set_focus()?;
        return Ok(existing);
    }

    let config = lab_config_from(&window::main_window_config(app)?);
    WebviewWindowBuilder::from_config(app, &config)?.build()
}
