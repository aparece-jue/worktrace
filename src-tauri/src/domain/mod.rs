//! 领域层：实体、跃迁表与校验。
//!
//! 禁止 IO：不得引用数据库驱动、文件系统、平台层或存储层。
//! 这里只放**纯规则**——能被穷举测试、不依赖时钟也不依赖数据库。

pub mod error;
pub mod interval;
pub mod session;
pub mod task;
