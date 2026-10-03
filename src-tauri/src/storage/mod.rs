//! 存储层：SQLite、迁移与仓储。
//!
//! 不得调用 `platform::`，也不得反向引用 `commands::`。
//! 写入一律接受调用方的 `&Transaction`，由服务层拥有事务并决定何时加 `revision`。

pub mod checkpoint_repo;
pub mod db;
pub mod guards;
pub mod meta;
pub mod migrations;
pub mod schema_v1;
pub mod session_repo;
pub mod task_repo;
