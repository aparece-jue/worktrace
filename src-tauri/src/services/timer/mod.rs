//! 计时协调器（M04）。
//!
//! 协调器是**进程内唯一**持有计时内存状态的地方。所有采样、命令、查询与系统事件
//! 都串行进入它的执行边界。

pub mod anchor;
pub mod coordinator;
pub mod primitives;
pub mod snapshot;
