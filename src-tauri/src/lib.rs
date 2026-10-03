//! Worktrace 后端库。
//!
//! 分层（01 §2，总纲 §9）：`commands → services → {storage, domain, platform}`。
//! `commands` 不直连 SQL 也不接受 `Connection`；`storage` 不反向引用
//! `commands` 也不调用 `platform`；`domain` 不做 IO。
//!
//! P1 交付 `domain`/`platform`/`storage`/`commands` 的基座与 `error`；
//! `services` 自 P2 起逐份加入。`envelope` 与 `error` 一样住在 crate 根：
//! `storage` 与 `services` 都要用它，而它们不得依赖 `commands`。

pub mod commands;
pub mod domain;
pub mod envelope;
pub mod error;
pub mod platform;
pub mod services;
pub mod storage;

pub use error::AppError;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("你好，{}！Worktrace 的 Rust 后端已连接。", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running Worktrace");
}
