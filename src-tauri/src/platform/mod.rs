//! 平台层：OS 适配的叶子模块。不含业务规则。
//!
//! 这一层是最底层——`storage` 与 `domain` 都不得引用它（分层规则见总纲 §9）。
//! P7 Task 0 起是四个模块：路径推导、时钟接缝、单实例与周期采样驱动。

pub mod clock;
pub mod paths;
pub mod scheduler;
pub mod single_instance;
