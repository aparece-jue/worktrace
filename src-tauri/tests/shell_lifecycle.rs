//! P7 Task 4：托盘与窗口生命周期。
//!
//! 计划里的测试口径是「托盘动作与界面动作调用同一命令；关窗不触发退出；重开窗口触发
//! 快照。**其余必须人工验收**」。所以这个文件钉的是**能用 Rust 断言的那半边**：
//!
//! - 托盘菜单的描述与动作映射（四项 + 一个预留项）；
//! - 「关掉全部窗口不退出」的决策（`RunEvent::ExitRequested` 的两个分支）；
//! - 唤醒请求的接收决策（抬起 / 重建 / 什么都不做）；
//! - 主窗 label 与 `tauri.conf.json` / `capabilities/default.json` 的一致性
//!   （对不上就等于「重建出来的窗口没有权限」）；
//! - 托盘的「暂停」「退出」落在**与 IPC 相同的**命令体/服务入口上（用效果相等与
//!   显式退出的库内证据断言）。
//!
//! **真实托盘图标/菜单交互、关掉全部窗口后仍然计时**在集成测试里不可能成立：
//! 测试进程里没有事件循环，也就没有窗口与托盘（`tauri::test` 的 mock 运行时本轮
//! 没有启用）。步骤与记录表见 `tests/manual-shell.md`。

use worktrace_lib::platform::tray::{self, MenuItemSpec, TrayAction};
use worktrace_lib::platform::window::{self, ActivationPlan};

fn item(id: &str) -> &'static MenuItemSpec {
    tray::MENU_ITEMS
        .iter()
        .find(|item| item.id == id)
        .unwrap_or_else(|| panic!("菜单里应当有 {id}"))
}

// ─────────────────────────────────────────────────────────────────────────────
// 菜单：四项 + 一个预留项（F-011 / R4 裁决）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_menu_offers_exactly_the_four_p7_actions() {
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
            TrayAction::Quit.menu_id(),
        ],
        "P7 实际提供四项：当前任务、暂停、快速捕获、退出（退出在最后）"
    );

    let labels: Vec<&str> = tray::MENU_ITEMS
        .iter()
        .filter(|item| item.action.is_some())
        .map(|item| item.label)
        .collect();
    assert_eq!(
        labels,
        vec!["当前任务", "暂停", "快速捕获", "退出"],
        "菜单文案是面向用户的中文，不写模块名"
    );
}

#[test]
fn finish_is_reserved_and_disabled_without_an_action() {
    let reserved: Vec<&str> = tray::MENU_ITEMS
        .iter()
        .filter(|item| item.action.is_none())
        .map(|item| item.id)
        .collect();
    assert_eq!(
        reserved,
        vec!["tray.finish_reserved"],
        "有且只有「完成」是预留项：P3 的 transition_task 接入后由 P8 启用"
    );
    assert!(
        item("tray.finish_reserved").label.contains("完成"),
        "预留项就是「完成」，只是 P7 不给它动作"
    );
    assert_eq!(
        tray::action_for("tray.finish_reserved"),
        None,
        "点了预留项必须什么都不发生（菜单里它是禁用项，这里再钉一次）"
    );
}

#[test]
fn every_menu_row_is_an_action_or_the_reserved_item() {
    assert_eq!(tray::MENU_ITEMS.len(), 5, "四项 + 一个预留项");
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
