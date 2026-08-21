//! fnos 增量加密备份系统 · 库入口
//!
//! 作为 lib 导出模块，供 `src/bin/` 下的独立二进制与集成测试复用。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub mod domain;
pub mod http;
pub mod infra;

use domain::crypto::CryptoSession;
use infra::config::ConfigManager;
use infra::kzwr_auth::KzwrAuthService;
use infra::persistence::snapshot::SnapshotStore;
use infra::storage_trait::TargetStorage;

/// 应用全局共享状态
#[derive(Clone)]
pub struct AppState {
    /// 目标存储适配器（kzwr 酷族网软）
    pub target: Arc<dyn TargetStorage>,
    /// 加密会话（备份加密/恢复解密）
    pub crypto: CryptoSession,
    /// 元数据快照库
    pub store: Arc<SnapshotStore>,
    /// 配置管理器（含多备份路径、kzwr 凭据）
    pub config: Arc<Mutex<ConfigManager>>,
    /// kzwr 认证服务（登录、token 自动重登）
    pub auth: Arc<KzwrAuthService>,
    /// 目标根前缀（如 "fn-backup"，来自配置）
    pub target_folder: String,
    /// 备份任务 id
    pub job_id: String,
    /// 数据目录（快照库位置）
    pub var_dir: PathBuf,
    /// 登录工作目录（session 临时）
    pub tmp_dir: PathBuf,
    /// 本地备份临时目录（上传前暂存）
    pub backup_tmp: PathBuf,
    /// 恢复默认目录
    pub default_restore_dir: PathBuf,
}
