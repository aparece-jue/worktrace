//! 托盘（F-011，P7 Task 4）。**平台叶子**：只装配图标与菜单，不懂业务。
//!
//! ## 分层：为什么业务动作不在这里
//!
//! `scripts/check-layers.ps1` 禁止 `platform` 引用 `services` / `storage` /
//! `commands`（这一层在最底下，只做 OS 适配），而 F-011 又要求「托盘动作与界面动作
//! 走同一批命令」。两件事同时成立的办法是把**「点到了什么」与「点了之后干什么」分开**：
//!
//! - 本模块给出 [`TrayAction`]（四个动作）、[`MENU_ITEMS`]（菜单的静态描述）与
//!   [`build`]（装配图标 + 菜单 + 事件回调）：**没有一行业务判断**；
//! - 「动作 → 命令体/服务入口」的映射留在 `commands::` 那一侧（`tray_pause_impl` /
//!   `tray_quit_impl`，前者复用 `pause_timer` 的命令体、后者复用 Task 0 的显式退出入口），
//!   由组合根 `lib.rs` 把两者接上。
//!
//! ## 四项 + 一个预留项（R4 裁决）
//!
//! P7 实际提供**四项**：当前任务、暂停、快速捕获、退出。**「完成」是预留项**——
//! P3 的 `transition_task` 服务入口接入后由 P8 启用。P7 用一个**禁用项**占位
//! （[`MENU_ITEMS`] 里 `action: None`）：菜单里看得见，点了不会有任何动作，
//! 也不存在「前端先 finish 再改状态」那条错路（P7 不开放尚未存在的动作）。
//! 显示 HUD 的托盘项属 V0.1b，本计划不加。
//!
//! ## 图标
//!
//! 用 [`tauri::AppHandle::default_window_icon`]（`tauri.conf.json` 的 `bundle.icon`，
//! 构建期就嵌好了）。**不启用 `image-png` / `image-ico`**：那要 `image` crate，
//! 本机离线缓存里没有（`Cargo.lock` 里也没有），一开就构建不出来。

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Runtime};

/// 托盘图标的 id（诊断与多托盘场景用；本应用只有一个）。
pub const TRAY_ID: &str = "worktrace-tray";

/// 鼠标悬停提示。
const TRAY_TOOLTIP: &str = "Worktrace";

/// 托盘菜单上的一个**用户动作**。
///
/// 它只说「点到了什么」，不说「该干什么」——去向由组合根接线（见模块头）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// 当前任务：把用户带到正在计时的那个任务上。
    CurrentTask,
    /// 暂停：暂停当前**运行中**的会话。
    Pause,
    /// 快速捕获：把用户带到捕获输入上（F-001）。
    QuickCapture,
    /// 退出：走显式退出入口（Task 0），不是杀进程。
    Quit,
}

impl TrayAction {
    /// 全部动作。菜单里每一个动作都应当恰好出现一次（用例钉住）。
    pub const ALL: [TrayAction; 4] = [
        TrayAction::CurrentTask,
        TrayAction::Pause,
        TrayAction::QuickCapture,
        TrayAction::Quit,
    ];

    /// 菜单项 id，也是这条动作的稳定名字（诊断里用它，别用文案）。
    pub const fn menu_id(self) -> &'static str {
        match self {
            TrayAction::CurrentTask => "tray.current_task",
            TrayAction::Pause => "tray.pause",
            TrayAction::QuickCapture => "tray.quick_capture",
            TrayAction::Quit => "tray.quit",
        }
    }
}

/// 一条菜单项的静态描述。
///
/// `action: None` 表示**预留项**：菜单里出现且**禁用**，点了没有动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuItemSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub action: Option<TrayAction>,
}

/// P7 的托盘菜单：**四项 + 一个预留项**。
///
/// 顺序就是显示顺序（退出在最后）。这份表**就是**菜单本身——[`build`] 逐条照它装配，
/// 所以用例钉住这张表就等于钉住了真实菜单（不再有第二份手抄的菜单定义）。
pub const MENU_ITEMS: [MenuItemSpec; 5] = [
    MenuItemSpec {
        id: TrayAction::CurrentTask.menu_id(),
        label: "当前任务",
        action: Some(TrayAction::CurrentTask),
    },
    MenuItemSpec {
        id: TrayAction::Pause.menu_id(),
        label: "暂停",
        action: Some(TrayAction::Pause),
    },
    MenuItemSpec {
        id: TrayAction::QuickCapture.menu_id(),
        label: "快速捕获",
        action: Some(TrayAction::QuickCapture),
    },
    // 预留项：P3 的 transition_task 接入后由 P8 启用（见模块头「四项 + 一个预留项」）。
    MenuItemSpec {
        id: "tray.finish_reserved",
        label: "完成（P8 启用）",
        action: None,
    },
    MenuItemSpec {
        id: TrayAction::Quit.menu_id(),
        label: "退出",
        action: Some(TrayAction::Quit),
    },
];

/// 菜单项 id → 动作。预留项与未知 id 都返回 `None`（点了什么也不做）。
pub fn action_for(menu_id: &str) -> Option<TrayAction> {
    MENU_ITEMS
        .iter()
        .find(|item| item.id == menu_id)
        .and_then(|item| item.action)
}

/// 装配托盘：图标 + 菜单 + 事件回调。
///
/// `on_action` 是菜单事件唯一的出口——本模块把它收到的 [`TrayAction`] 原样交出去，
/// 不判断当前该不该暂停、也不碰任何状态（那些都在 `commands::` 那侧的命令体里）。
///
/// 图标注册进应用后由 Tauri 托管（`build` 内部登记到资源表），所以调用方不必保存返回值。
pub fn build<R: Runtime>(
    app: &AppHandle<R>,
    on_action: impl Fn(&AppHandle<R>, TrayAction) + Send + Sync + 'static,
) -> tauri::Result<()> {
    let mut menu = MenuBuilder::new(app);
    for item in MENU_ITEMS.iter() {
        match item.action {
            // 有动作 = 可点；文案与 id 都来自这张表。
            Some(_) => {
                menu = menu.text(item.id, item.label);
            }
            // 预留项：禁用项。P7 不提供它的动作，所以它点了也不会走到 `on_action`。
            None => {
                let disabled = MenuItemBuilder::with_id(item.id, item.label)
                    .enabled(false)
                    .build(app)?;
                menu = menu.item(&disabled);
            }
        }
    }
    let menu = menu.build()?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip(TRAY_TOOLTIP);
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder
        .on_menu_event(move |app, event| {
            if let Some(action) = action_for(event.id().as_ref()) {
                on_action(app, action);
            }
        })
        .build(app)?;
    Ok(())
}
