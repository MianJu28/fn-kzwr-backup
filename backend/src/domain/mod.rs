//! 领域层（核心域）
//!
//! 不依赖具体基础设施实现，只依赖抽象 trait 与标准库。

pub mod alerts;
pub mod backup;
pub mod crypto;
pub mod pace;
pub mod restore;
pub mod retention;
pub mod scheduler;
pub mod sync;
