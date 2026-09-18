//! fnos 增量加密备份系统 · 库入口
//!
//! 作为 lib 导出模块，供 `src/bin/` 下的独立二进制与集成测试复用。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

pub mod domain;
pub mod eventbus;
pub mod http;
pub mod infra;

use age::secrecy::SecretString;
use domain::crypto::CryptoSession;
use eventbus::EventBus;
use infra::config::ConfigManager;
use infra::persistence::snapshot::SnapshotStore;
use infra::storage_trait::SwapTarget;

/// 可热替换的加密会话（用户在设置中更换 age 密钥后无需重启服务）
pub struct CryptoSwap {
    inner: RwLock<CryptoSession>,
}

impl CryptoSwap {
    /// 创建（初始会话）
    pub fn new(session: CryptoSession) -> Self {
        Self {
            inner: RwLock::new(session),
        }
    }

    /// 取当前会话快照（Clone，供单次备份/恢复使用）
    pub fn get(&self) -> CryptoSession {
        self.inner.read().unwrap().clone()
    }

    /// 替换会话
    pub fn swap(&self, session: CryptoSession) {
        *self.inner.write().unwrap() = session;
    }
}

/// 应用全局共享状态
#[derive(Clone)]
pub struct AppState {
    /// 目标存储适配器（kzwr 官方 WebDAV，ADR-009；配置保存后可热替换）
    pub target: Arc<SwapTarget>,
    /// 目标是否已配置（凭据就绪；未配置时 target 为占位适配器）
    pub target_ready: bool,
    /// 加密会话（备份加密/恢复解密；密钥变更后可热替换）
    pub crypto: Arc<CryptoSwap>,
    /// 应用口令（密钥库/配置敏感字段加密；更换密钥时需复用它重新加密落盘）
    pub passphrase: Arc<SecretString>,
    /// 配置目录（config.toml / 密钥库所在）
    pub cfg_dir: PathBuf,
    /// 待一次性展示的自动生成私钥（首次启动自动生成时设置，读取后清空）
    pub pending_key_reveal: Arc<Mutex<Option<String>>>,
    /// 内部事件总线（状态推送）
    pub eventbus: Arc<EventBus>,
    /// 元数据快照库
    pub store: Arc<SnapshotStore>,
    /// 配置管理器（含多备份路径、WebDAV 凭据）
    pub config: Arc<Mutex<ConfigManager>>,
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
