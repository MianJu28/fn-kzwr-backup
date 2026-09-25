//! fnos 增量加密备份系统 · 库入口
//!
//! 作为 lib 导出模块，供 `src/bin/` 下的独立二进制与集成测试复用。

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

pub mod domain;
pub mod eventbus;
pub mod http;
pub mod infra;
pub mod plugin;

use age::secrecy::SecretString;
use domain::alerts::AlertSink;
use domain::crypto::CryptoSession;
use eventbus::EventBus;
use infra::config::{AppConfig, ConfigManager};
use infra::persistence::snapshot::SnapshotStore;
use infra::storage_trait::{SwapTarget, TargetPool, TargetStorage};

/// 可热替换的加密会话（用户在设置中更换 age 密钥后无需重启服务）
pub struct CryptoSwap {
    inner: RwLock<CryptoSession>,
}

/// 日志过滤器热更新句柄（设置页切换「调试日志」时即时生效，无需重启）
///
/// main 初始化日志时装入；config 保存 debug 开关时经 `modify` 切换级别。
pub static LOG_HANDLE: std::sync::OnceLock<
    tracing_subscriber::reload::Handle<tracing_subscriber::EnvFilter, tracing_subscriber::Registry>,
> = std::sync::OnceLock::new();

/// 应用调试日志过滤器（debug=true 输出 fnos_backup 详细日志）
pub fn apply_log_debug(debug: bool) {
    use tracing_subscriber::EnvFilter;
    let filter = if debug {
        "fnos_backup=debug,tower_http=info"
    } else {
        "fnos_backup=info,tower_http=info"
    };
    if let Some(h) = LOG_HANDLE.get() {
        let _ = h.modify(|l| *l = EnvFilter::new(filter));
    }
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
    /// **主目标**适配器（首个启用的目标；全局能力如 kzwr 增强、兼容接口用它）
    pub target: Arc<SwapTarget>,
    /// **多目标池**：`目标 id → 适配器`（任务按 `target_id` 取自己的实例）
    pub targets: Arc<TargetPool>,
    /// 主目标 id（随配置刷新）
    pub primary_target_id: Arc<RwLock<String>>,
    /// 主目标是否已配置（凭据就绪；未配置时 target 为占位适配器）
    pub target_ready: Arc<AtomicBool>,
    /// 备份运行标志（定时调度与手动触发共用：true = 有备份正在执行，并发触发直接跳过）
    pub backup_running: Arc<AtomicBool>,
    /// 加密会话（备份加密/恢复解密；密钥变更后可热替换）
    pub crypto: Arc<CryptoSwap>,
    /// 应用口令（密钥库/配置敏感字段加密；更换密钥时需复用它重新加密落盘）
    pub passphrase: Arc<SecretString>,
    /// 配置目录（config.toml / 密钥库所在）
    pub cfg_dir: PathBuf,
    /// 待一次性展示的自动生成私钥（首次启动自动生成时设置，读取后清空）
    pub pending_key_reveal: Arc<Mutex<Option<String>>>,
    /// 告警汇聚点（备份/恢复失败等，供 UI 查询与 Webhook 外发）
    pub alerts: Arc<AlertSink>,
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
    /// 运行日志文件路径（日志页查看/清空/下载）
    pub log_file: PathBuf,
    /// 插件注册表（远程目标与增强插件的唯一装配点；详见 `plugin` 模块）
    pub plugins: Arc<crate::plugin::registry::PluginRegistry>,
    /// kzwr REST 客户端（**增强功能**：账号存储空间、回收站清理等，非备份通道）
    ///
    /// access-token 从配置解密后注入；未配置时调用返回认证提示，不影响备份/恢复。
    pub kzwr: Arc<crate::infra::kzwr_api::client::KzwrClient>,
    /// 操作审计日志（敏感/破坏性操作留痕，存 $TRIM_PKGVAR/audit.log）
    pub audit: Arc<crate::domain::audit::AuditLog>,
}

impl AppState {
    /// 按配置重建目标池（启动时、目标/任务配置变更后调用）
    ///
    /// 同步更新：目标池、主目标适配器、主目标 id、`target_ready`。
    /// 返回已就绪（凭据齐备）的目标数量。
    pub fn reload_targets(&self, cfg: &AppConfig) -> usize {
        let built = {
            let mgr = self.config.lock().unwrap();
            self.plugins.build_targets(cfg, &mgr)
        };
        let primary_id = cfg
            .primary_target()
            .map(|t| t.id.clone())
            .unwrap_or_default();

        let mut items: Vec<(String, Arc<dyn TargetStorage>, String)> = Vec::new();
        let mut ready_count = 0usize;
        let mut primary: Option<(Arc<dyn TargetStorage>, String, bool)> = None;
        for b in built {
            if b.id == primary_id {
                primary = Some((b.storage.clone(), b.name.clone(), b.ready));
            }
            if b.ready {
                ready_count += 1;
            }
            items.push((b.id, b.storage, b.name));
        }
        self.targets.replace_all(items);

        let primary_ready = match primary {
            Some((storage, name, ready)) => {
                self.target.swap(storage);
                tracing::info!(target = %primary_id, backend = %name, ready, "主目标已刷新");
                ready
            }
            None => {
                tracing::info!("配置中没有任何目标（等待用户在「目标管理」中添加）");
                false
            }
        };
        *self.primary_target_id.write().unwrap() = primary_id;
        self.target_ready
            .store(primary_ready, std::sync::atomic::Ordering::Relaxed);
        ready_count
    }

    /// 主目标 id（kzwr 增强等全局能力的绑定对象）
    pub fn primary_target_id(&self) -> String {
        self.primary_target_id.read().unwrap().clone()
    }

    /// 取某个目标的适配器（未装配 → 占位适配器，调用即返回配置提示）
    pub fn target_for(&self, target_id: &str) -> Arc<dyn TargetStorage> {
        self.targets.get(target_id)
    }

    /// 主目标是否已就绪
    pub fn primary_ready(&self) -> bool {
        self.target_ready.load(std::sync::atomic::Ordering::Relaxed)
    }
}
