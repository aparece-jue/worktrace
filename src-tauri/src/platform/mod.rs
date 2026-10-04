//! 平台层：OS 适配的叶子模块。不含业务规则。
//!
//! 这一层是最底层——`storage` 与 `domain` 都不得引用它（分层规则见总纲 §9）。
//! P7 Task 0 起是四个模块：路径推导、时钟接缝、单实例与周期采样驱动；
//! P7 Task 4 加上托盘与主窗生命周期（`tray` / `window`）——**托盘只装配 UI**，
//! 「动作 → 命令体」的映射在 `commands::` 那侧，由 `lib.rs` 接线（这一层不得反向
//! 引用上层，理由写在 `tray` 的模块头）。
//! P7 Task 6a 加上实验窗口 `sync-lab`（`sync_lab`）：同样只碰窗口对象，
//! 「什么时候开」由 debug 构建才存在的 dev 命令决定（理由写在 `sync_lab` 的模块头）。

pub mod clock;
pub mod paths;
pub mod scheduler;
pub mod single_instance;
pub mod sync_lab;
pub mod tray;
pub mod window;
