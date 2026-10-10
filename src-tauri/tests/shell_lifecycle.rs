//! P7 Task 4：托盘与窗口生命周期。
//!
//! 计划里的测试口径是「托盘动作与界面动作调用同一命令；关窗不触发退出；重开窗口触发
//! 快照。**其余必须人工验收**」。所以这个文件钉的是**能用 Rust 断言的那半边**：
//!
//! - 托盘菜单的描述与动作映射（P8 Task 2d 起**五项、全部可点**：「完成」不再是禁用占位项）；
//! - 「关掉全部窗口不退出」的决策（`RunEvent::ExitRequested` 的两个分支）；
//! - 唤醒请求的接收决策（抬起 / 重建 / 什么都不做）；
//! - 主窗 label 与 `tauri.conf.json` / `capabilities/default.json` 的一致性
//!   （对不上就等于「重建出来的窗口没有权限」）；
//! - 实验窗口 `sync-lab`（Task 6a）同样这一条：**不是静态窗口**、配置与主窗同源
//!   （只改 label）、且登记进权限名单；
//! - 托盘的「暂停」「退出」落在**与 IPC 相同的**命令体/服务入口上（用效果相等与
//!   显式退出的库内证据断言），「完成」同样（P8 Task 2d：与 IPC 的 `transition_task_impl`
//!   效果逐项相等；没有正在计时的任务时**零写入**）；
//! - **视图跳转的目标页**（P8 Task 7：「快速捕获」⇒ 收件箱 + 聚焦、「当前任务」⇒ 计时视图）
//!   与"页名与前端 `PageKey` 逐字相同"这条跨语言契约（读前端源码核对，见
//!   `the_tray_view_pages_are_the_frontend_page_keys_and_nothing_else`）。
//!
//! **真实托盘图标/菜单交互、关掉全部窗口后仍然计时**在集成测试里不可能成立：
//! 测试进程里没有事件循环，也就没有窗口与托盘（`tauri::test` 的 mock 运行时本轮
//! 没有启用）⇒ "点一下托盘，那一页真的切过来了"由**前端**用例钉住
//! （前端 `src/__tests__/trayViewRequests.test.ts` 与 `src/__tests__/App.test.tsx`），
//! 这里钉的是它上游那张表。步骤与记录表见 `tests/manual-v01.md`（§托盘视图跳转）
//! 与 `tests/manual-shell.md`。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use worktrace_lib::commands::{
    self, StartTimerRequest, TransitionTaskRequest, TrayFinish, TrayPause,
};
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::platform::sync_lab;
use worktrace_lib::platform::tray::{self, MenuItemSpec, TrayAction};
use worktrace_lib::platform::window::{self, ActivationPlan};
use worktrace_lib::services::bootstrap::{
    lock_app, startup, AppGuard, AppState, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::timer::coordinator::SessionRequest;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::{tray_dispatch, TrayDispatch};

fn item(id: &str) -> &'static MenuItemSpec {
    tray::MENU_ITEMS
        .iter()
        .find(|item| item.id == id)
        .unwrap_or_else(|| panic!("菜单里应当有 {id}"))
}

// ─────────────────────────────────────────────────────────────────────────────
// 菜单：五项，全部可点（F-011；P8 Task 2d 起「完成」不再预留）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_menu_offers_exactly_the_five_actions() {
    let clickable: Vec<&str> = tray::MENU_ITEMS
        .iter()
        .filter(|item| item.action.is_some())
        .map(|item| item.id)
        .collect();
    assert_eq!(
        clickable,
        vec![
            TrayAction::CurrentTask.menu_id(),
            TrayAction::Pause.menu_id(),
            TrayAction::QuickCapture.menu_id(),
            TrayAction::Finish.menu_id(),
            TrayAction::Quit.menu_id(),
        ],
        "五项，次序是 P7 那份菜单的次序（「完成」就在它当年占位的位置上，不重排）：\
         当前任务、暂停、快速捕获、完成、退出（退出在最后）"
    );

    let labels: Vec<&str> = tray::MENU_ITEMS
        .iter()
        .filter(|item| item.action.is_some())
        .map(|item| item.label)
        .collect();
    assert_eq!(
        labels,
        vec!["当前任务", "暂停", "快速捕获", "完成", "退出"],
        "菜单文案是面向用户的中文，不写模块名；「完成」不再带「（P8 启用）」"
    );
}

/// 「完成」从**禁用占位项**变成**真实动作**（P8 Task 2d）。
///
/// 三件事一起钉住：① 它现在有动作（P7 的 `action: None` 必须消失）；② 它的 id 随动作名走
/// ——`tray.finish_reserved` 的 `_reserved` 说的就是"还没有动作"，接上之后不再预留；
/// ③ 菜单里**不再有**任何禁用项（P7 登记的"点了没反应"那一项已闭合）。
#[test]
fn finish_is_a_real_action_and_no_row_is_left_disabled() {
    assert_eq!(
        tray::action_for(TrayAction::Finish.menu_id()),
        Some(TrayAction::Finish),
        "菜单 id 必须映射回「完成」动作（`action_for` 是菜单事件的唯一入口）"
    );
    assert_eq!(
        TrayAction::Finish.menu_id(),
        "tray.finish",
        "id 是动作的稳定名字：不再是 P7 那个 `tray.finish_reserved`"
    );
    assert!(
        item(TrayAction::Finish.menu_id()).label.contains("完成"),
        "这一项就是「完成」"
    );

    let disabled: Vec<&str> = tray::MENU_ITEMS
        .iter()
        .filter(|item| item.action.is_none())
        .map(|item| item.id)
        .collect();
    assert_eq!(
        disabled,
        Vec::<&str>::new(),
        "五项全部可点：P7 的「完成（P8 启用）」禁用项已经启用，没有第二个预留项"
    );
    assert_eq!(
        tray::action_for("tray.finish_reserved"),
        None,
        "旧 id 不再对应任何动作（菜单里也没有它了）——留着它就是第二个名字"
    );
}

#[test]
fn every_menu_row_is_an_action() {
    assert_eq!(tray::MENU_ITEMS.len(), 5, "五项");
    for action in TrayAction::ALL {
        assert_eq!(
            tray::MENU_ITEMS
                .iter()
                .filter(|item| item.action == Some(action))
                .count(),
            1,
            "每条动作在菜单里恰好出现一次：{}",
            action.menu_id()
        );
    }
    for item in tray::MENU_ITEMS.iter() {
        assert!(!item.id.is_empty(), "菜单项 id 不能为空");
        assert!(!item.label.is_empty(), "菜单项文案不能为空");
    }

    let mut ids: Vec<&str> = tray::MENU_ITEMS.iter().map(|item| item.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), tray::MENU_ITEMS.len(), "菜单项 id 必须唯一");
}

#[test]
fn a_menu_id_maps_back_to_its_own_action() {
    for action in TrayAction::ALL {
        assert_eq!(
            tray::action_for(action.menu_id()),
            Some(action),
            "id 与动作必须一一对应（写错一个字就是「点了没反应」）"
        );
    }
    assert_eq!(tray::action_for("tray.nonexistent"), None);
    assert_eq!(tray::action_for(""), None);
}

/// **动作 → 入口的路由表**（fix round 1，评审 I2）：`on_tray_action` 只 match
/// `tray_dispatch`，所以这张表就是「托盘动作与界面动作走同一批命令」那条要求的接缝。
/// 把 `Pause` 接到 `Quit` 上（一次粘贴错误）必须在这里红——原先它谁都不会吵醒。
///
/// P8 Task 7 起两个窗口动作**带上视图**（`TrayDispatch::View(Some(..))`）：只抬窗不再够，
/// 所以这里比的是 `tray_view_for` 那**一处**给出的目标页，不再是笼统的 `Window`。
#[test]
fn each_tray_action_is_dispatched_to_its_own_entry() {
    let view = |action: TrayAction| match tray_dispatch(action) {
        TrayDispatch::View(Some(view)) => view,
        other => panic!(
            "{} 必须是带视图的窗口动作，实际 {other:?}",
            action.menu_id()
        ),
    };

    assert_eq!(
        view(TrayAction::CurrentTask),
        view_of(tray::TRAY_VIEW_PAGE_TIMER, false),
        "当前任务：切到计时视图 + 抬起主窗（P7 的抬窗仍在），不是命令"
    );
    assert_eq!(
        view(TrayAction::QuickCapture),
        view_of(tray::TRAY_VIEW_PAGE_INBOX, true),
        "快速捕获：切到收件箱**并聚焦捕获输入框** + 抬起主窗（P8 Task 7 的判据）"
    );
    assert_eq!(
        tray_dispatch(TrayAction::Pause),
        TrayDispatch::Pause,
        "暂停 → commands::tray_pause_impl（→ pause_timer_impl）"
    );
    assert_eq!(
        tray_dispatch(TrayAction::Finish),
        TrayDispatch::Finish,
        "完成 → commands::tray_finish_impl（→ transition_task_impl，P8 Task 2d）"
    );
    assert_eq!(
        tray_dispatch(TrayAction::Quit),
        TrayDispatch::Quit,
        "退出 → commands::tray_quit_impl（→ 显式退出入口）"
    );

    // 服务动作恰好三个，而且顺序固定：新增动作时这里会提醒补路由。
    let service: Vec<TrayDispatch> = TrayAction::ALL
        .iter()
        .map(|action| tray_dispatch(*action))
        .filter(|dispatch| !matches!(dispatch, TrayDispatch::View(_)))
        .collect();
    assert_eq!(
        service,
        vec![
            TrayDispatch::Pause,
            TrayDispatch::Finish,
            TrayDispatch::Quit
        ],
        "只有暂停、完成与退出是服务动作（其余是带视图的窗口动作）"
    );

    // 窗口动作恰好两个，而且**两个都要有视图**（P8 Task 7 把 P7 那句"只抬窗"收掉了）。
    let views: Vec<&'static str> = TrayAction::ALL
        .iter()
        .filter_map(|action| match tray_dispatch(*action) {
            TrayDispatch::View(Some(view)) => Some(view.page),
            TrayDispatch::View(None) => {
                panic!(
                    "{} 是窗口动作却没有视图：P8 Task 7 要求它跳转",
                    action.menu_id()
                )
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        views,
        vec![tray::TRAY_VIEW_PAGE_TIMER, tray::TRAY_VIEW_PAGE_INBOX],
        "窗口动作恰好两个（当前任务、快速捕获），各自带一个目标页"
    );
}

/// 造一个期望的 [`tray::TrayView`]（用例里不手抄字段名）。
fn view_of(page: &'static str, focus: bool) -> tray::TrayView {
    tray::TrayView { page, focus }
}

/// **视图跳转表本身**（P8 Task 7）：动作 → (页, 是否聚焦)。
///
/// 为什么单列一条：`tray_view_for` 是"点到什么 → 去哪个视图"的唯一一处，
/// 把两个动作对调（快速捕获跳去计时视图）不会让别的用例变红——而计划 §6.4-11 的判据
/// 正是这一对。非窗口动作返回 `None`（它们不该跳转）。
#[test]
fn only_the_two_window_actions_request_a_view_and_the_targets_are_fixed() {
    assert_eq!(
        tray::tray_view_for(TrayAction::QuickCapture),
        Some(view_of(tray::TRAY_VIEW_PAGE_INBOX, true)),
        "「快速捕获」⇒ 收件箱 + 聚焦捕获输入框（F-001）"
    );
    assert_eq!(
        tray::tray_view_for(TrayAction::CurrentTask),
        Some(view_of(tray::TRAY_VIEW_PAGE_TIMER, false)),
        "「当前任务」⇒ 计时视图（不要求聚焦）"
    );

    for action in [TrayAction::Pause, TrayAction::Finish, TrayAction::Quit] {
        assert_eq!(
            tray::tray_view_for(action),
            None,
            "{} 是服务动作，不该顺带切页",
            action.menu_id()
        );
    }
    // 聚焦只挂在「快速捕获」上：另外那一条明确**不**要求聚焦（否则每次点「当前任务」
    // 都会去动收件箱输入框的焦点，而那条路径根本不该碰它）。
    assert!(
        tray::tray_view_for(TrayAction::QuickCapture).unwrap().focus,
        "「快速捕获」要求聚焦输入框"
    );
    assert!(
        !tray::tray_view_for(TrayAction::CurrentTask).unwrap().focus,
        "「当前任务」不该去动输入框的焦点"
    );
}

/// **页名与前端 `PageKey` 逐字相同**（P8 Task 7 的跨语言契约）。
///
/// 跳转载荷里的 `page` 是前端 `src/state/pageRequest.ts` 那个联合类型的字符串——
/// 前端拿它直接调 `requestPage`，两侧对不上就是"点了托盘什么都不发生"（而且没有任何
/// 编译期报错）。所以这里**读两份前端源码**核对：
///
/// - `src/state/pageRequest.ts`：两个页名必须出现在 `PageKey` 联合类型**内部**；
/// - `src/trayViewRequests.ts`：`TRAY_VIEW_EVENT` 的**值**必须与 Rust 这边逐字相同
///   （前端按这个名字 `listen`；常量住在它那里，所以不在 `pageRequest.ts` 里找）。
///
/// 与前端 `src/types/__tests__/event-constants.test.ts` 是**同一份契约的两个方向**
/// （那边读本仓源码核对常量）。反向验证：把 `TRAY_VIEW_PAGE_INBOX` 改成 `"inbox2"`，
/// 或者改 `TRAY_VIEW_EVENT` 的名字只改一侧 ⇒ 这一条红。
///
/// 路径：前端在**仓库根**（`frontendDist: "../dist"`，即 `src-tauri/` 的上一级），
/// 所以是 `<manifest>/../src/...`（不是镜像里的 `worktrace-web/`）。
/// WSL 镜像的布局（前端独立目录 + 没有仓库根那一级）跑不了这一条 ⇒ **只在仓库侧跑**
/// （`p7t6b-web-gate.ps1` / 仓库里的 `cargo test`），这是 Ruling P8-3 同一条边界。
#[test]
fn the_tray_view_pages_are_the_frontend_page_keys_and_nothing_else() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let read_frontend = |relative: &str| -> String {
        let path = manifest.join("..").join(relative);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("读不到前端 {}：{error}", path.display()))
            .replace("\r\n", "\n")
    };
    let page_request = read_frontend("src/state/pageRequest.ts");
    let tray_requests = read_frontend("src/trayViewRequests.ts");

    // 负控：那两个页名不是随便什么字符串——它们就是 `PageKey` 联合类型里的成员。
    let union = page_request
        .split("export type PageKey =")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("pageRequest.ts 里应当有 `export type PageKey = …;`");
    for page in [tray::TRAY_VIEW_PAGE_INBOX, tray::TRAY_VIEW_PAGE_TIMER] {
        assert!(
            union.contains(&format!("\"{page}\"")),
            "`{page}` 必须在 `PageKey` 联合类型**内部**（不在类型外面的某句话里）"
        );
    }

    // 事件名：前端必须按同一个名字订阅（两个方向各写一份，靠这一条对齐）。
    assert!(
        tray_requests.contains(&format!("\"{}\"", tray::TRAY_VIEW_EVENT)),
        "前端（src/trayViewRequests.ts）必须按同一个事件名订阅托盘跳转：{}",
        tray::TRAY_VIEW_EVENT
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 窗口：关窗不退出 / 唤醒的决策（F-009 / F-016）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn closing_the_last_window_is_prevented_but_a_programmatic_exit_is_not() {
    assert!(
        window::should_prevent_exit(None),
        "`ExitRequested {{ code: None }}` = 用户关掉最后一个窗口 ⇒ 阻止退出：\
         托盘还得在、周期采样还得跑（F-009）"
    );
    assert!(
        !window::should_prevent_exit(Some(0)),
        "`code = Some(0)` = 托盘「退出」走的 `AppHandle::exit(0)` ⇒ 必须放行，\
         否则退出会变成一个关不掉的进程"
    );
    assert!(
        !window::should_prevent_exit(Some(1)),
        "退出事务失败时的非零码同样要放行"
    );
}

#[test]
fn an_activation_request_raises_or_rebuilds_and_nothing_else() {
    assert_eq!(
        window::plan_activation(false, true),
        ActivationPlan::Ignore,
        "没有请求时**什么都不做**：不能凭一个不存在的请求把窗口抬起来"
    );
    assert_eq!(
        window::plan_activation(false, false),
        ActivationPlan::Ignore,
        "没有请求、窗口也没了 ⇒ 仍然什么都不做（不凭空建窗）"
    );
    assert_eq!(
        window::plan_activation(true, true),
        ActivationPlan::Raise,
        "有请求且主窗还在 ⇒ 抬起/聚焦，不重建（重建会丢掉用户当前的界面状态）"
    );
    assert_eq!(
        window::plan_activation(true, false),
        ActivationPlan::Rebuild,
        "有请求但主窗已经关掉 ⇒ 按配置重建（新页面加载 ⇒ 立即拉快照）"
    );
}

#[test]
fn the_main_window_label_matches_the_config_and_the_capability() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = read_json(&manifest.join("tauri.conf.json"));
    let labels: Vec<&str> = config["app"]["windows"]
        .as_array()
        .expect("tauri.conf.json 的 app.windows 必须是数组")
        .iter()
        .filter_map(|window| window["label"].as_str())
        .collect();
    assert!(
        labels.contains(&window::MAIN_WINDOW_LABEL),
        "重建主窗是从 tauri.conf.json 找 label = `{}` 的那份配置；找不到就重建不出来（现有：{labels:?}）",
        window::MAIN_WINDOW_LABEL
    );

    let capabilities = read_json(&manifest.join("capabilities/default.json"));
    let windows: Vec<&str> = capabilities["windows"]
        .as_array()
        .expect("capabilities/default.json 的 windows 必须是数组")
        .iter()
        .filter_map(|label| label.as_str())
        .collect();
    assert!(
        windows.contains(&window::MAIN_WINDOW_LABEL),
        "重建出来的窗口 label 不变，权限名单必须覆盖它，否则窗口能开、JS 却调不动任何命令\
         （现有：{windows:?}）"
    );
}

fn read_json(path: &std::path::Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("读不到 {}：{error}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("{} 不是合法 JSON：{error}", path.display()))
}

/// 实验窗口 `sync-lab`（P7 Task 6a）：**不做成静态窗口**，但必须进权限名单，
/// 而且与主窗**同源**——入口 URL、标题、尺寸都来自主窗那一份配置，只改 label。
///
/// 三件事任缺其一，实机实验就废：不在名单里 ⇒ 它的 JS 没有 `core:` 权限（连事件都
/// 监听不了，§2.0 就卡住）；配置不同源 ⇒ 它跑的不是同一份前端，结论说明不了主窗的
/// 行为；做成静态窗口 ⇒ 启动就多一个窗，`manual-shell.md` 的既有验收步骤被污染。
#[test]
fn the_experiment_window_is_opened_on_demand_from_the_main_window_config() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    // ① `tauri.conf.json` 里**没有** `sync-lab`。窗口配置按 tauri 自己的类型解析
    //    （不是只看 label 字符串）：下面要拿它做「只改 label」的逐字段比对。
    let config = read_json(&manifest.join("tauri.conf.json"));
    let windows: Vec<tauri::utils::config::WindowConfig> = config["app"]["windows"]
        .as_array()
        .expect("tauri.conf.json 的 app.windows 必须是数组")
        .iter()
        .map(|window| {
            serde_json::from_value(window.clone())
                .expect("app.windows 的每一项都必须是合法的 WindowConfig")
        })
        .collect();
    assert!(
        window::window_config_for(&windows, sync_lab::SYNC_LAB_WINDOW_LABEL).is_none(),
        "`sync-lab` 必须是实验时由 Rust 侧创建的窗口，不做成 tauri.conf.json 的静态窗口"
    );

    // ② 实验窗口的配置 = 主窗那份**只改 label**（比 JSON 而不是逐字段抄一遍：
    //    以后给主窗加字段，这条断言自动跟上）。
    let main = window::window_config_for(&windows, window::MAIN_WINDOW_LABEL)
        .expect("tauri.conf.json 应当有 label = main 的窗口");
    let lab = sync_lab::lab_config_from(main);
    assert_eq!(lab.label, sync_lab::SYNC_LAB_WINDOW_LABEL);
    let mut expected = serde_json::to_value(main).expect("WindowConfig 应当可序列化");
    expected["label"] = serde_json::json!(sync_lab::SYNC_LAB_WINDOW_LABEL);
    assert_eq!(
        serde_json::to_value(&lab).expect("WindowConfig 应当可序列化"),
        expected,
        "除 label 外必须与主窗逐字段相同（入口 URL、标题、尺寸都是同一处来源）"
    );

    // ③ 权限名单覆盖它——否则窗口能开、JS 一条命令都调不了。
    let capabilities = read_json(&manifest.join("capabilities/default.json"));
    let labels: Vec<&str> = capabilities["windows"]
        .as_array()
        .expect("capabilities/default.json 的 windows 必须是数组")
        .iter()
        .filter_map(|label| label.as_str())
        .collect();
    assert!(
        labels.contains(&sync_lab::SYNC_LAB_WINDOW_LABEL),
        "实验窗口必须登记进权限名单，否则它的 JS 没有任何 `core:` 权限（现有：{labels:?}）"
    );
    assert!(
        labels.contains(&window::MAIN_WINDOW_LABEL),
        "主窗（以及按配置重建出来的那一份）照样要在名单里（现有：{labels:?}）"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 托盘动作：与 IPC 走同一批入口
// ─────────────────────────────────────────────────────────────────────────────

/// 假时钟的挂钟。两条路径用的是**同一个**时刻，所以「效果相等」比得了。
const WALL: i64 = 1_700_000_000_000;

/// 采样节拍：默认给一个**跑不起来**的值，免得采样线程在临界区外面偷偷加拍；
/// 需要观察采样本身的用例用 [`launch_with_interval`] 显式给一个小节拍。
const IDLE_SAMPLING_MS: u64 = 3_600_000;

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<EventEnvelope>>,
}

impl EventSink for RecordingSink {
    fn broadcast(&self, envelope: &EventEnvelope) -> Result<(), String> {
        self.events.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

/// 一个真应用（走 `services::bootstrap::startup`），一个 Ready 任务 `t1`。
struct Rig {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    running: Box<RunningApp>,
    sink: Arc<RecordingSink>,
    epoch: String,
}

fn launch() -> Rig {
    launch_with_interval(IDLE_SAMPLING_MS)
}

fn launch_with_interval(sampling_interval_ms: u64) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("worktrace.db");
    let lock_path = dir.path().join("instance.lock");

    {
        let mut db = Db::open(&db_path).unwrap();
        migrate(db.connection()).unwrap();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        init_meta(&tx).unwrap();
        tx.execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES('t1','任务一','Ready',0,1000,1000)",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let mut config = StartupConfig::new(&db_path, &lock_path);
    config.sampling_interval_ms = sampling_interval_ms;
    let sink = Arc::new(RecordingSink::default());
    let running = match startup(
        config,
        Box::new(FakeClock::new(WALL, 0)),
        Arc::clone(&sink) as Arc<dyn EventSink>,
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    };

    let epoch = running.data_epoch().to_string();
    Rig {
        _dir: dir,
        db_path,
        running,
        sink,
        epoch,
    }
}

impl Rig {
    /// 取串行边界。**同一个测试里只能取一次**（`Mutex` 不可重入）。
    fn state(&self) -> AppGuard<'_> {
        lock_app(self.running.app())
    }

    fn events(&self) -> Vec<EventEnvelope> {
        self.sink.events.lock().unwrap().clone()
    }

    /// 另一条连接：退出之后读库用（`shutdown` 自己会取那把锁，不能持锁调它）。
    fn db(&self) -> Db {
        Db::open(&self.db_path).unwrap()
    }
}

fn start_request(rig: &Rig, task_id: &str) -> StartTimerRequest {
    StartTimerRequest {
        expected_data_epoch: rig.epoch.clone(),
        task_id: task_id.to_string(),
        task_expected_version: 0,
        mode: "FOREGROUND".to_string(),
        timer_kind: "stopwatch".to_string(),
        target_duration_ms: None,
        expected_interval_ms: 1_000,
    }
}

fn revision_of(state: &AppState) -> i64 {
    state
        .db()
        .unwrap()
        .connection()
        .query_row(
            "SELECT revision FROM app_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn text_of(state: &AppState, sql: &str) -> String {
    state
        .db()
        .unwrap()
        .connection()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

/// 库里那条任务行的两个事实（状态 / `row_version`）：托盘「完成」写没写、写成什么样，
/// 都回到行上看。
fn task_facts(state: &AppState) -> (String, i64) {
    state
        .db()
        .unwrap()
        .connection()
        .query_row(
            "SELECT status, row_version FROM task WHERE id = 't1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

/// 库里会话的四个事实（状态 / `ended_at` / 区间时长 / 待确认）。
fn session_facts(state: &AppState) -> Vec<(String, i64, i64, i64)> {
    let mut statement = state
        .db()
        .unwrap()
        .connection()
        .prepare(
            "SELECT s.state, COALESCE(s.ended_at, -1),
                    COALESCE(i.duration_ms, -1), COALESCE(i.needs_review, 0)
             FROM work_session s JOIN work_interval i ON i.session_id = s.id
             ORDER BY s.id",
        )
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap();
    rows.map(|row| row.unwrap()).collect()
}

/// 界面点「暂停」时回传的正是**展示中的快照**字段；托盘那条入口用的是同一次读到的
/// 快照。两条路径因此可以直接比效果：响应、库里的行、revision、广播。
#[test]
fn the_tray_pause_lands_on_the_same_command_body_as_the_ipc_one() {
    let tray_rig = launch();
    let ipc_rig = launch();

    let tray_outcome = {
        let mut state = tray_rig.state();
        commands::start_timer_impl(
            &mut state,
            tray_rig.running.broadcaster(),
            start_request(&tray_rig, "t1"),
        )
        .unwrap();
        let revision_before = revision_of(&state);

        let paused = commands::tray_pause_impl(&mut state, tray_rig.running.broadcaster())
            .expect("运行中的会话必须能被托盘暂停");
        let TrayPause::Paused(outcome) = paused else {
            panic!("有运行中的会话时托盘暂停不能是「无事可做」");
        };
        assert_eq!(
            outcome.revision,
            revision_before + 1,
            "暂停是一次成功业务写 ⇒ 恰好推进一次 revision"
        );
        outcome
    };

    let ipc_outcome = {
        let mut state = ipc_rig.state();
        commands::start_timer_impl(
            &mut state,
            ipc_rig.running.broadcaster(),
            start_request(&ipc_rig, "t1"),
        )
        .unwrap();
        let revision_before = revision_of(&state);

        let snapshot = commands::timer_snapshot_impl(&mut state).unwrap();
        let outcome = commands::pause_timer_impl(
            &mut state,
            ipc_rig.running.broadcaster(),
            SessionRequest {
                expected_data_epoch: snapshot.data_epoch.clone(),
                session_id: snapshot.session_id.clone().expect("有会话"),
                session_expected_version: snapshot.session_version.expect("有会话版本"),
            },
        )
        .unwrap();
        assert_eq!(outcome.revision, revision_before + 1);
        outcome
    };

    // ① 响应：状态、版本、暂计、任务版本逐项相同（会话 id 是随机 uuid，不比）。
    assert_eq!(tray_outcome.snapshot.state, ipc_outcome.snapshot.state);
    assert_eq!(
        tray_outcome.snapshot.session_version,
        ipc_outcome.snapshot.session_version
    );
    assert_eq!(
        tray_outcome.snapshot.active_ms,
        ipc_outcome.snapshot.active_ms
    );
    assert_eq!(tray_outcome.revision, ipc_outcome.revision);
    assert_eq!(tray_outcome.task_version, ipc_outcome.task_version);

    // ② 库里的行：四条事实逐项相同。
    assert_eq!(
        session_facts(&tray_rig.state()),
        session_facts(&ipc_rig.state()),
        "托盘暂停与 IPC 暂停必须落下同一组事实（这才是「同一命令」）"
    );
    assert_eq!(
        session_facts(&tray_rig.state())[0].0,
        "paused",
        "暂停不是结束：会话进 paused，不写 ended_at"
    );

    // ③ 广播：同样一条 `domain.changed`、同一个 revision。
    let tray_events = tray_rig.events();
    let ipc_events = ipc_rig.events();
    assert_eq!(tray_events.len(), ipc_events.len());
    assert_eq!(tray_events.len(), 2, "start 一条 + pause 一条");
    let (tray_last, ipc_last) = (tray_events.last().unwrap(), ipc_events.last().unwrap());
    assert_eq!(tray_last.event, ipc_last.event);
    assert_eq!(tray_last.revision, ipc_last.revision);
    // `data_epoch` 是**每个库自己的**（两条夹具两个临时库），所以比的是
    // 「事件带的 epoch 就是本库的权威 epoch」，不是两个库相等。
    assert_eq!(tray_last.data_epoch, tray_rig.epoch);
    assert_eq!(ipc_last.data_epoch, ipc_rig.epoch);
    assert_eq!(
        tray_last.event,
        worktrace_lib::services::events::EVENT_DOMAIN_CHANGED
    );
}

#[test]
fn the_tray_pause_without_a_running_session_writes_nothing() {
    let rig = launch();
    let mut state = rig.state();

    // ① 完全没有会话：什么都不做。
    let revision_before = revision_of(&state);
    let changes_before = state.db().unwrap().connection().total_changes();
    assert_eq!(
        commands::tray_pause_impl(&mut state, rig.running.broadcaster()).unwrap(),
        TrayPause::NothingToPause
    );
    assert_eq!(
        revision_of(&state),
        revision_before,
        "没有会话时不能推进 revision"
    );
    assert_eq!(
        state.db().unwrap().connection().total_changes(),
        changes_before,
        "没有会话时不能写库"
    );
    assert!(rig.events().is_empty(), "没有业务写就不该有 domain.changed");

    // ② 会话已经暂停：不重复暂停（否则会撞 VERSION_CONFLICT，还会白广播一次）。
    commands::start_timer_impl(
        &mut state,
        rig.running.broadcaster(),
        start_request(&rig, "t1"),
    )
    .unwrap();
    let snapshot = commands::timer_snapshot_impl(&mut state).unwrap();
    commands::pause_timer_impl(
        &mut state,
        rig.running.broadcaster(),
        SessionRequest {
            expected_data_epoch: snapshot.data_epoch.clone(),
            session_id: snapshot.session_id.clone().expect("有会话"),
            session_expected_version: snapshot.session_version.expect("有会话版本"),
        },
    )
    .unwrap();

    let revision_after_pause = revision_of(&state);
    let changes_after_pause = state.db().unwrap().connection().total_changes();
    let events_after_pause = rig.events().len();

    assert_eq!(
        commands::tray_pause_impl(&mut state, rig.running.broadcaster()).unwrap(),
        TrayPause::NothingToPause
    );
    assert_eq!(revision_of(&state), revision_after_pause);
    assert_eq!(
        state.db().unwrap().connection().total_changes(),
        changes_after_pause,
        "重复暂停不能有任何写入"
    );
    assert_eq!(
        text_of(&state, "SELECT state FROM work_session"),
        "paused",
        "已经暂停的会话不该被再动一次"
    );
    drop(state);
    assert_eq!(
        rig.events().len(),
        events_after_pause,
        "无事可做 ⇒ 不广播（不空转制造通知）"
    );
}

/// 托盘「完成」必须落在 **IPC 的 `transition_task_impl`** 上（P8 Task 2d）：两条路径各起
/// 一个真应用、都先 `start_timer`，然后一条走 `tray_finish_impl`、一条走
/// `transition_task_impl`（请求用**快照**里的同一组字段，与托盘那条入口逐字同源）。
/// 效果逐项相等才算「同一批入口」——这是"托盘不另写业务逻辑"唯一能机器化的证据。
///
/// 反向验证：把 `tray_finish_impl` 改成自己开事务改状态（或改成先 `finish_timer` 再改
/// 状态），库里那条会话的四个事实与广播条数立刻对不上 IPC 那一侧。
#[test]
fn the_tray_finish_lands_on_the_same_command_body_as_the_ipc_one() {
    let tray_rig = launch();
    let ipc_rig = launch();

    let tray_report = {
        let mut state = tray_rig.state();
        commands::start_timer_impl(
            &mut state,
            tray_rig.running.broadcaster(),
            start_request(&tray_rig, "t1"),
        )
        .unwrap();
        let revision_before = revision_of(&state);
        // 会话 id 是随机 uuid（两条夹具各起一个库）⇒ 这里先记下**本夹具**那条，
        // 稍后核"结束的正是它自己那条"，而不是拿两个 uuid 互比。
        let session = commands::timer_snapshot_impl(&mut state)
            .unwrap()
            .session_id
            .expect("有会话");

        let finished = commands::tray_finish_impl(&mut state, tray_rig.running.broadcaster())
            .expect("有正在计时的任务时托盘完成必须成功");
        let TrayFinish::Finished(report) = finished else {
            panic!("正在计时 ⇒ 托盘完成不能是「无事可做」");
        };
        assert_eq!(
            report.revision,
            revision_before + 1,
            "完成是一次成功业务写 ⇒ 恰好推进一次 revision"
        );
        (report, session)
    };
    let (tray_report, tray_session) = tray_report;

    let (ipc_report, ipc_session) = {
        let mut state = ipc_rig.state();
        commands::start_timer_impl(
            &mut state,
            ipc_rig.running.broadcaster(),
            start_request(&ipc_rig, "t1"),
        )
        .unwrap();

        // 与 `tray_finish_impl` 同源：任务身份与版本取自**快照**（托盘拿不到别的东西），
        // 目标与原因按命令层的取值域给。
        let snapshot = commands::timer_snapshot_impl(&mut state).unwrap();
        let session = snapshot.session_id.clone().expect("有会话");
        let report = commands::transition_task_impl(
            &mut state,
            ipc_rig.running.broadcaster(),
            TransitionTaskRequest {
                expected_data_epoch: snapshot.data_epoch.clone(),
                task_id: snapshot.task_id.clone().expect("有会话就有任务"),
                expected_row_version: snapshot.task_row_version.expect("有会话就有任务版本"),
                target: "Done".to_string(),
                cause: "user".to_string(),
            },
        )
        .unwrap();
        (report, session)
    };

    // ① 响应：状态、版本、两个联动名单、revision 逐项相同。
    assert_eq!(tray_report.task.status.as_str(), "Done");
    assert_eq!(tray_report.task.status, ipc_report.task.status);
    assert_eq!(tray_report.task.row_version, ipc_report.task.row_version);
    // 会话 id 是随机 uuid，**不比**那两个字符串：比的是"各自结束了**自己那条**会话"
    // 这条事实（名单长度相同、库内事实在 ② 逐项相同）——paused 名单两边都是空，可以直接比。
    assert_eq!(
        tray_report.ended_sessions,
        vec![tray_session],
        "托盘完成结束的是它自己那条会话"
    );
    assert_eq!(
        ipc_report.ended_sessions,
        vec![ipc_session],
        "IPC 完成结束的是它自己那条会话"
    );
    assert_eq!(
        tray_report.ended_sessions.len(),
        ipc_report.ended_sessions.len(),
        "两条路径结束的会话条数相同"
    );
    assert_eq!(
        tray_report.paused_sessions, ipc_report.paused_sessions,
        "完成不动 paused 名单（它结束会话，不暂停）"
    );
    assert_eq!(tray_report.revision, ipc_report.revision);
    assert!(tray_report.paused_sessions.is_empty());

    // ② 库里的行：任务行与会话的四个事实逐项相同。
    assert_eq!(
        task_facts(&tray_rig.state()),
        task_facts(&ipc_rig.state()),
        "托盘完成与 IPC 完成必须落下同一条任务行"
    );
    assert_eq!(task_facts(&tray_rig.state()).0, "Done");
    assert_eq!(
        session_facts(&tray_rig.state()),
        session_facts(&ipc_rig.state()),
        "托盘完成与 IPC 完成必须落下同一组会话事实（这才是「同一命令」）"
    );
    assert_eq!(
        session_facts(&tray_rig.state())[0].0,
        "finished",
        "完成不是暂停：会话进 finished，`ended_at` 落上"
    );

    // ③ 广播：同样一条 `domain.changed`、同一个 revision（`transition_task_impl` 已按
    //    `Changed` 广播，托盘**不再自己广播一次**——多一条这里就红）。
    let tray_events = tray_rig.events();
    let ipc_events = ipc_rig.events();
    assert_eq!(tray_events.len(), ipc_events.len());
    assert_eq!(tray_events.len(), 2, "start 一条 + 完成一条");
    let (tray_last, ipc_last) = (tray_events.last().unwrap(), ipc_events.last().unwrap());
    assert_eq!(tray_last.event, ipc_last.event);
    assert_eq!(tray_last.revision, ipc_last.revision);
    assert_eq!(tray_last.data_epoch, tray_rig.epoch);
    assert_eq!(ipc_last.data_epoch, ipc_rig.epoch);
    assert_eq!(
        tray_last.event,
        worktrace_lib::services::events::EVENT_DOMAIN_CHANGED
    );
}

/// 没有**正在计时**的任务时，托盘「完成」不产生任何写（P8 Task 2d 的边界）：
/// 零广播、零 revision 变化、`total_changes` 不动，而且**不造**一条假任务、也不编一个
/// 版本去撞服务端的守卫——库里的行原样不动。
#[test]
fn the_tray_finish_without_a_running_timer_writes_nothing() {
    let rig = launch();
    let mut state = rig.state();

    // ① 完全没有会话（冷启动）：什么都不做。
    let revision_before = revision_of(&state);
    let changes_before = state.db().unwrap().connection().total_changes();
    assert_eq!(
        commands::tray_finish_impl(&mut state, rig.running.broadcaster()).unwrap(),
        TrayFinish::NothingToFinish
    );
    assert_eq!(
        revision_of(&state),
        revision_before,
        "没有正在计时时不能推进 revision"
    );
    assert_eq!(
        state.db().unwrap().connection().total_changes(),
        changes_before,
        "没有正在计时时不能写库"
    );
    assert_eq!(
        task_facts(&state).0,
        "Ready",
        "不造一个假任务：没有会话可归属，任务行原样停在 Ready"
    );
    assert!(rig.events().is_empty(), "没有业务写就不该有 domain.changed");

    // ② 会话已经暂停：**暂停不是「正在计时」**（判据与托盘暂停同一条 `is_running`）。
    commands::start_timer_impl(
        &mut state,
        rig.running.broadcaster(),
        start_request(&rig, "t1"),
    )
    .unwrap();
    let snapshot = commands::timer_snapshot_impl(&mut state).unwrap();
    commands::pause_timer_impl(
        &mut state,
        rig.running.broadcaster(),
        SessionRequest {
            expected_data_epoch: snapshot.data_epoch.clone(),
            session_id: snapshot.session_id.clone().expect("有会话"),
            session_expected_version: snapshot.session_version.expect("有会话版本"),
        },
    )
    .unwrap();

    let revision_after_pause = revision_of(&state);
    let changes_after_pause = state.db().unwrap().connection().total_changes();
    let events_after_pause = rig.events().len();

    assert_eq!(
        commands::tray_finish_impl(&mut state, rig.running.broadcaster()).unwrap(),
        TrayFinish::NothingToFinish
    );
    assert_eq!(revision_of(&state), revision_after_pause);
    assert_eq!(
        state.db().unwrap().connection().total_changes(),
        changes_after_pause,
        "暂停中的任务不归托盘「完成」管：一个字节都不写"
    );
    assert_eq!(
        task_facts(&state).0,
        "Doing",
        "任务状态不变（start_timer 把 Ready 推到了 Doing）"
    );
    assert_eq!(
        session_facts(&state)[0].0,
        "paused",
        "暂停的会话不该被托盘完成顺手结束"
    );
    drop(state);
    assert_eq!(
        rig.events().len(),
        events_after_pause,
        "无事可做 ⇒ 不广播（不空转制造通知）"
    );
}

/// 托盘「退出」必须落在 **Task 0 的显式退出入口**上：库内证据与 `tests/exit.rs`
/// 逐条一致（结束当前 run 的 running/paused、写 `clean_exit_at`、保留 recovering、
/// 先停定时器）。它证明退出不是 `std::process::exit`，也不是另一份实现。
#[test]
fn the_tray_quit_goes_through_the_explicit_exit_entry() {
    let rig = launch_with_interval(10);
    let session_id = {
        let mut state = rig.state();
        let started = commands::start_timer_impl(
            &mut state,
            rig.running.broadcaster(),
            start_request(&rig, "t1"),
        )
        .unwrap();
        started.snapshot.session_id.clone().expect("有会话")
    };

    // 另一条 recovering 会话（02 §4：退出**保留**它，不得顺手确认历史）。
    let run_id = rig.running.run_id().to_string();
    rig.db()
        .connection()
        .execute(
            "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,
                                      row_version,needs_review)
             VALUES('s-recovering','t1',?1,'FOREGROUND','recovering','stopwatch',1200,0,1)",
            rusqlite::params![run_id],
        )
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.running.sampling_ticks() < 2 {
        assert!(Instant::now() < deadline, "采样驱动没有跑起来");
        std::thread::sleep(Duration::from_millis(5));
    }

    // 注意：**不能持着 state 的锁**调退出（`shutdown` 自己会取那把锁）。
    let report = commands::tray_quit_impl(&rig.running).expect("托盘退出必须走显式退出入口");

    assert_eq!(report.run_id, run_id);
    assert_eq!(report.clean_exit_at, WALL, "退出时刻来自协调器的时钟采样");
    assert!(report.clean_exit_recorded);
    assert_eq!(report.sessions_ended, vec![session_id.clone()]);
    assert_eq!(report.recovering_kept, vec!["s-recovering".to_string()]);

    let db = rig.db();
    let conn = db.connection();
    assert_eq!(
        conn.query_row(
            "SELECT clean_exit_at FROM application_run WHERE id = ?1",
            [&run_id],
            |row| row.get::<_, Option<i64>>(0)
        )
        .unwrap(),
        Some(WALL),
        "`clean_exit_at` 落库——这就是「不是杀进程」的证据"
    );
    assert_eq!(
        conn.query_row(
            "SELECT state FROM work_session WHERE id = ?1",
            [&session_id],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "finished"
    );
    assert_eq!(
        conn.query_row(
            "SELECT state FROM work_session WHERE id = 's-recovering'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "recovering"
    );

    let ticks_after = rig.running.sampling_ticks();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        rig.running.sampling_ticks(),
        ticks_after,
        "退出先停定时器：之后不得再有采样触发"
    );
}

/// 关掉全部窗口**不清核心**：测试进程里本来就没有（也不可能有）窗口对象，
/// 采样照样在跑——F-009 的 Rust 半边。真实关窗行为见 `tests/manual-shell.md`。
#[test]
fn the_core_keeps_running_with_no_window_at_all() {
    let rig = launch_with_interval(10);
    let before = rig.running.sampling_ticks();
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.running.sampling_ticks() < before + 2 {
        assert!(
            Instant::now() < deadline,
            "没有窗口引用时周期采样仍必须被驱动（F-009）"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(
        window::should_prevent_exit(None),
        "而且这一刻的关窗事件（code=None）必须被拦下，否则进程会跟着最后一个窗口一起走"
    );
}

/// **自死锁防线**（fix round 1，评审 I1）：`RunningApp::shutdown` 放宽到 `&self` 之后，
/// 「先取锁、再调退出」也能编译；但退出要先 `join` 采样线程，而采样线程每一拍都要取
/// 那把锁 ⇒ 调用者自己持锁时 join 永远等不到头。
///
/// 这条用例就是那个环的**反向验证**：拆掉 `holds_app_lock` 那道检查，它会**卡死**
/// （不是变红——死锁不抛错），所以下面用 10ms 的采样节拍让采样线程很快堵在锁上。
#[test]
fn shutdown_refuses_to_run_on_the_thread_that_holds_the_lock() {
    let rig = launch_with_interval(10);

    // 先让采样真的跑起来（**必须在取锁之前**：取锁之后采样永远拿不到锁，
    // `sampling_ticks` 就不会再动，等待会变成必然超时）。
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.running.sampling_ticks() < 1 {
        assert!(Instant::now() < deadline, "采样驱动没有跑起来");
        std::thread::sleep(Duration::from_millis(5));
    }

    // 现在取锁，并留一拍的时间让采样线程真的堵在这把锁上（没有这一步，反向验证
    // 就没有判别力：采样可能正好在睡觉，拆掉防线也不会卡死）。
    let guard = rig.state();
    std::thread::sleep(Duration::from_millis(50));

    let error = rig
        .running
        .shutdown()
        .expect_err("持锁调用退出必须被拒（否则与采样线程互锁）");
    assert_eq!(error.code(), "STORAGE_ERROR");
    // `Storage` 的 `message()` 是固定的用户文案，细节在 `detail()` 里（P4 Task 6 的口径）：
    // 这条断言因此读 `detail()`，而托盘把它连同 `message()` 一起写进诊断。
    let detail = error.detail().unwrap_or_default();
    assert!(
        detail.contains("串行边界"),
        "诊断要说清是调用姿势的问题，实际：{detail}"
    );

    // 拒绝是**干净**的：采样没被停、没有事务、没有半退出状态。
    // （持锁期间采样本来就堵在锁上，所以「它还在跑」只能在放开锁之后观察。）
    drop(guard);
    let ticks_after_refusal = rig.running.sampling_ticks();
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.running.sampling_ticks() < ticks_after_refusal + 2 {
        assert!(
            Instant::now() < deadline,
            "被拒的退出必须什么都没碰：采样还得在跑"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // 放开锁之后照常退出（不是「拒过一次就永久拒」）。
    let report = rig.running.shutdown().expect("放开锁之后退出应当成功");
    assert_eq!(report.run_id, rig.running.run_id());
    assert!(report.clean_exit_recorded);
}
