//! 平台层：OS 适配的叶子模块。不含业务规则。
//!
//! 这一层是最底层——`storage` 与 `domain` 都不得引用它（分层规则见总纲 §9）。
//! 目前只有两个模块：路径推导与时钟接缝。

pub mod clock;
pub mod paths;
