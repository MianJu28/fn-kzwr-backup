//! fnos 增量加密备份系统 · 库入口
//!
//! 作为 lib 导出模块，供 `src/bin/` 下的独立二进制与集成测试复用。

use std::path::PathBuf;
use std::sync::Arc;

pub mod domain;
pub mod http;
pub mod infra;

use domain::crypto::CryptoSession;
use infra::persistence::snapshot::SnapshotStore;
use infra::storage_trait::{SourceStorage, TargetStorage};

/// 应用全局共享状态
#[derive(Clone)]
pub struct AppState {
    /// 源存储适配器（本地 FS）
    pub source: Arc<dyn SourceStorage>,
    /// 目标存储适配器（kzwr 酷族网软）
    pub target: Arc<dyn TargetStorage>,
    /// 加密会话（备份加密/恢复解密）
    pub crypto: CryptoSession,
    /// 元数据快照库
    pub store: Arc<SnapshotStore>,
    /// 源目录根路径
    pub source_root: PathBuf,
    /// 目标根前缀（如 "fn-backup"）
    pub target_prefix: Option<String>,
    /// 备份任务 id
    pub job_id: String,
}
