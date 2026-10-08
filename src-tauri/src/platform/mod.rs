//! 平台层：OS 适配的叶子模块。不含业务规则。
//!
//! 这一层是最底层——`storage` 与 `domain` 都不得引用它（分层规则见总纲 §9）。
//! P7 Task 0 起是四个模块：路径推导、时钟接缝、单实例与周期采样驱动；
//! P7 Task 4 加上托盘与主窗生命周期（`tray` / `window`）——**托盘只装配 UI**，
//! 「动作 → 命令体」的映射在 `commands::` 那侧，由 `lib.rs` 接线（这一层不得反向
//! 引用上层，理由写在 `tray` 的模块头）。
//! P7 Task 6a 加上实验窗口 `sync-lab`（`sync_lab`）：同样只碰窗口对象，
//! 「什么时候开」由 debug 构建才存在的 dev 命令决定（理由写在 `sync_lab` 的模块头）。
//! P6 Task 2a 加上正式诊断日志的落点（`diagnostics`）：release 的 Windows 子系统
//! 没有控制台，维护态/故障态这类「内存态」的跃迁必须有落盘出口（理由写在模块头）。
//! P6 Task 2c 加上正式 OS 事件源（`system_events`）：锁屏/解锁、休眠/唤醒、系统改时
//! 的监听器（一个从不显示的顶层消息窗），只翻成事件与边界样本，怎么处理由上层决定。

pub mod clock;
// P6 Task 2a：一条一行地记诊断（append）。2b 的故障态进入/清除复用同一落点。
pub mod diagnostics;
pub mod paths;
pub mod scheduler;
pub mod single_instance;
// P6 Task 2c：锁屏/解锁、休眠/唤醒、系统改时的正式事件源（可注入 + 本平台实现）。
pub mod sync_lab;
pub mod system_events;
pub mod tray;
pub mod window;
