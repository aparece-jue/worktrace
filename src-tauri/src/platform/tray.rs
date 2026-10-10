//! 托盘（F-011，P7 Task 4）。**平台叶子**：只装配图标与菜单，不懂业务。
//!
//! ## 分层：为什么业务动作不在这里
//!
//! `scripts/check-layers.ps1` 禁止 `platform` 引用 `services` / `storage` /
//! `commands`（这一层在最底下，只做 OS 适配），而 F-011 又要求「托盘动作与界面动作
//! 走同一批命令」。两件事同时成立的办法是把**「点到了什么」与「点了之后干什么」分开**：
//!
//! - 本模块给出 [`TrayAction`]（五个动作）、[`MENU_ITEMS`]（菜单的静态描述）与
//!   [`build`]（装配图标 + 菜单 + 事件回调）：**没有一行业务判断**；
//! - 「动作 → 命令体/服务入口」的映射留在 `commands::` 那一侧（`tray_pause_impl` /
//!   `tray_finish_impl` / `tray_quit_impl`，前两者分别复用 `pause_timer_impl` 与
//!   `transition_task_impl` 的命令体、后者复用 Task 0 的显式退出入口），
//!   由组合根 `lib.rs` 把两者接上。
//!
//! ## 五项（P8 Task 2d 起，「完成」不再是预留项）
//!
//! P7 提供**四项**（当前任务、暂停、快速捕获、退出），「完成」是**预留项**——P7 用一个
//! **禁用项**占位（[`MENU_ITEMS`] 里 `action: None`）：菜单里看得见、点了不会有任何动作，
//! 因为那时 `transition_task` 还不存在，开了就是"一条通往尚未存在服务的路径"。
//!
//! P8 Task 2d 把它接上：命令 5 的服务入口与命令体都在，托盘「完成」走
//! `commands::tray_finish_impl`（**复用 `transition_task_impl`**，托盘不另写业务逻辑）。
//! 于是菜单里五项全部可点（`action: None` 这条机制保留——它是"预留下一个动作"的通用形状，
//! 今天没有第二个预留项）。
//! 显示 HUD 的托盘项属 V0.1b，本计划不加。
//!
//! ## 视图跳转：跳到哪一页由**这张表**给（P8 Task 7）
//!
//! [`TrayAction::CurrentTask`] / [`TrayAction::QuickCapture`] 不只是"抬窗"：
//!
//! - 「当前任务」要切到计时视图；
//! - 「快速捕获」要切到收件箱并**聚焦捕获输入框**（计划 §6.4-11）。
//!
//! 本模块只回答"点到了什么、该去哪个视图"（[`tray_view_for`]），**怎么跳**是前端的事
//! （`src/state/pageRequest.ts` 的 `requestPage` + `src/App.tsx` 的订阅，M8 建立的那一套）；
//! 组合根 `lib.rs` 把 [`TrayView`] 变成一条窗口作用域的定向事件后**照旧抬窗**（抬窗不能丢）。
//!
//! **通道为什么是一条新窗口事件而不是广播**：`services/events.rs` 那对名字
//! （`domain.changed` / `timer.tick`）是**业务广播**的封闭词表——它们的两个字段
//! （`data_epoch` / `revision`）说的都是"这次业务写把库推到了哪一版"，四个消费方按那套
//! 去重规则接纳。而"用户点了托盘的哪一项"两样都不是：它不改库、没有版本、也不该被卷进
//! 去重规则。所以它走**窗口作用域**的定向事件：[`TRAY_VIEW_EVENT`] + `emit_to(主窗 label, …)`，
//! 只发给主窗，不新建窗口、不动信封。
//! （注：理由**不是**"混进 `worktrace:event` 会让每个窗口白重拉一遍"——`domainState.onEvent`
//! 对不认识的事件名是**直接忽略**的；理由就是上面那条：信封是业务事实的词表。）
//! 签名的第二处（前端）在同名常量里，由 `src/types/__tests__/event-constants.test.ts`
//! 读本文件核对（前端在**仓库根**，所以路径不带 `src-tauri/` 前缀；另一侧的反向判据在
//! `tests/shell_lifecycle.rs`）。
//!
//! **一条已知限制**（fix round 1，评审 Critical-2）：主窗**已经被关掉**时点这两个动作，
//! 跳转请求发给的是尚不存在的窗口（零接收者），随后才重建 ⇒ 新窗口是全新 JS 上下文、
//! 按默认页（收件箱）挂载，**这一次跳转丢失**。要不要补发由组合根决定，理由与代价写在
//! `lib.rs::on_tray_action` 的同名小节；实机判据见 `tests/manual-v01.md` §10.5。
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
    /// 完成：把**当前正在计时的那条任务**推到 `Done`（P8 Task 2d 启用，P7 是禁用占位项）。
    Finish,
    /// 快速捕获：把用户带到捕获输入上（F-001）。
    QuickCapture,
    /// 退出：走显式退出入口（Task 0），不是杀进程。
    Quit,
}

impl TrayAction {
    /// 全部动作。菜单里每一个动作都应当恰好出现一次（用例钉住）。
    ///
    /// 次序与 [`MENU_ITEMS`] 一致；`Finish` 留在 P7 给那个预留项排的位置上
    /// （fix round 1，评审 Minor-2：本任务只把动作接上，**不重排**菜单）。
    pub const ALL: [TrayAction; 5] = [
        TrayAction::CurrentTask,
        TrayAction::Pause,
        TrayAction::QuickCapture,
        TrayAction::Finish,
        TrayAction::Quit,
    ];

    /// 菜单项 id，也是这条动作的稳定名字（诊断里用它，别用文案）。
    pub const fn menu_id(self) -> &'static str {
        match self {
            TrayAction::CurrentTask => "tray.current_task",
            TrayAction::Pause => "tray.pause",
            // P7 那个占位项的 id 是 `tray.finish_reserved`（`_reserved` 说的就是"还没有动作"）：
            // 动作存在之后它不再预留，id 跟着动作名走（`menu_id` 是唯一来源）。
            TrayAction::Finish => "tray.finish",
            TrayAction::QuickCapture => "tray.quick_capture",
            TrayAction::Quit => "tray.quit",
        }
    }
}

/// 一条菜单项的静态描述。
///
/// `action: None` 表示**预留项**：菜单里出现且**禁用**，点了没有动作。
/// （P8 Task 2d 起没有这种行——「完成」已接上动作；这条机制留着给下一个预留动作。）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuItemSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub action: Option<TrayAction>,
}

/// 托盘菜单：**五项**，全部可点（P8 Task 2d 起）。
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
    // P7 那个禁用占位项「完成（P8 启用）」**原位**启用（位置不变，见 `TrayAction::ALL`；
    // 只有 id 从 `tray.finish_reserved` 改成 `tray.finish`，见 `TrayAction::menu_id`）。
    MenuItemSpec {
        id: TrayAction::Finish.menu_id(),
        label: "完成",
        action: Some(TrayAction::Finish),
    },
    MenuItemSpec {
        id: TrayAction::Quit.menu_id(),
        label: "退出",
        action: Some(TrayAction::Quit),
    },
];

/// 托盘动作要求的**视图跳转**（P8 Task 7）。
///
/// `page` 是**前端 `PageKey` 的字符串**（前端 `src/state/pageRequest.ts` 的联合类型）
/// ——两侧同名，前端 `requestPage` 直接吃它，不需要第二张"名字 → 页面"的对照表。
/// `focus` 说"到了那一页还要把光标放进捕获输入框"：只有「快速捕获」为真。
///
/// 为什么用 `&'static str` 而不是 Rust 侧再枚举一遍八个页面：本模块只发**两个**目标，
/// 再造一个八变体的枚举就是第二份 `PageKey`（而它一个消费者都没有）。
///
/// `Serialize` 按全路径派生（本模块不 `use serde`）：它的形状就是发给前端的那两个键
/// （`{page, focus}`），由 `src/lib.rs` 的用例与前端两侧各钉一次。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct TrayView {
    pub page: &'static str,
    pub focus: bool,
}

/// 视图跳转事件名（**窗口作用域**，不是 `worktrace:event` 那条广播）。
///
/// 见模块头「视图跳转」：`lib.rs` 用 `emit_to(主窗 label, …)` 只发给主窗。
pub const TRAY_VIEW_EVENT: &str = "worktrace:tray-view";

/// 视图跳转的目标页（与前端 `PageKey` 逐字相同）。
pub const TRAY_VIEW_PAGE_INBOX: &str = "inbox";
/// 视图跳转的目标页（与前端 `PageKey` 逐字相同）。
pub const TRAY_VIEW_PAGE_TIMER: &str = "timer";

/// 这条托盘动作要求跳到哪个视图；不是视图动作就返回 `None`。
///
/// **只有这一处**决定"点到什么 → 去哪个视图"（与 [`action_for`] 同一条纪律：
/// 路由集中在一张表上，用例断言它）。今天两个视图动作：
///
/// - 「快速捕获」（F-001）⇒ 收件箱 + **聚焦捕获输入框**；
/// - 「当前任务」⇒ 计时视图。
///
/// 这里**不看库里的状态**（有没有正在计时的任务）：那是服务层的事，托盘没有回执通道，
/// 空态由页面自己照实显示（"没有在计时"不是错误，也不该编一个假会话出来）。
pub fn tray_view_for(action: TrayAction) -> Option<TrayView> {
    match action {
        TrayAction::QuickCapture => Some(TrayView {
            page: TRAY_VIEW_PAGE_INBOX,
            focus: true,
        }),
        TrayAction::CurrentTask => Some(TrayView {
            page: TRAY_VIEW_PAGE_TIMER,
            focus: false,
        }),
        TrayAction::Pause | TrayAction::Finish | TrayAction::Quit => None,
    }
}

/// 菜单项 id → 动作。未知 id 返回 `None`（点了什么也不做）。
pub fn action_for(menu_id: &str) -> Option<TrayAction> {
    MENU_ITEMS
        .iter()
        .find(|item| item.id == menu_id)
        .and_then(|item| item.action)
}

/// 装配托盘：图标 + 菜单 + 事件回调。
///
/// `on_action` 是菜单事件唯一的出口——本模块把它收到的 [`TrayAction`] 原样交出去，
/// 不判断当前该不该暂停/完成、也不碰任何状态（那些都在 `commands::` 那侧的命令体里）。
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
            // 预留项：禁用项。它没有动作，所以点了也不会走到 `on_action`
            // （P8 Task 2d 起菜单里没有这种行，机制留着）。
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
