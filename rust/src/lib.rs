//! fnos 增量加密备份系统 · 库入口
//!
//! 作为 lib 导出模块，供 `src/bin/` 下的独立二进制与集成测试复用。

use std::sync::Arc;

pub mod domain;
pub mod http;
pub mod infra;

/// 应用全局共享状态
#[derive(Clone)]
pub struct AppState {
    /// 源存储适配器（本地 FS）
    pub source: Arc<dyn infra::storage_trait::SourceStorage>,
    /// 目标存储适配器（kzwr 酷族网软）
    pub target: Arc<dyn infra::storage_trait::TargetStorage>,
}
