//! 命令层：IPC 边界的信封与错误。
//!
//! 不直连 SQL，也不接受 `Connection`（总纲 §9）：命令层只做参数反序列化、
//! 调用服务、把 `Result<T, AppError>` 映射成前端可用的形状。
//! 事务与 epoch/version 校验由服务层与 `storage::guards` 负责。

pub mod envelope;
