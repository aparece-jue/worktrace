//! 命令层：IPC 边界。
//!
//! 不直连 SQL，也不接受 `Connection`（总纲 §9）：命令层只做参数反序列化、
//! 调用服务、把 `Result<T, AppError>` 映射成前端可用的形状。
//! 事务与 epoch/version 校验由服务层与 `storage::guards` 负责。
//! 请求信封 [`crate::envelope::WriteEnvelope`] 与错误 [`crate::error::AppError`]
//! 都住在 crate 根（`storage`/`services` 也要用，而它们不得依赖 `commands`），
//! 命令层直接引用它们。
//!
//! P7 在这里加 IPC 命令。P4 Task 6 删掉了 `commands::envelope` 转发路径：
//! 生产代码零调用，只由一条转发用例撑着（本仓库不留「定义了没人调」的 API）。
