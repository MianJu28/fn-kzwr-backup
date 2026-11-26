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

use age::secrecy::{ExposeSecret, SecretString};
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
    /// 上次装配时每个目标的**配置快照**（`目标 id → TargetConfig`）
    ///
    /// 用于 `reload_targets` 判断「哪些目标的配置真的变了」：没变的直接复用已有实例。
    /// 重建一个实例意味着新建沙箱线程池 + 调插件 `target_open`（最坏等 30s），
    /// 而用户改一个**无关目标**或只改全局设置时，其它目标的配置根本没动 ——
    /// 此前却全部重建，这是「保存个配置要等好久」的主要来源之一。
    pub target_snapshots: Arc<std::sync::RwLock<std::collections::HashMap<String, crate::infra::config::TargetConfig>>>,
    /// 主目标 id（随配置刷新）
    pub primary_target_id: Arc<RwLock<String>>,
    /// 主目标是否已配置（凭据就绪；未配置时 target 为占位适配器）
    pub target_ready: Arc<AtomicBool>,
    /// 备份运行标志（定时调度与手动触发共用：true = 有备份正在执行，并发触发直接跳过）
    pub backup_running: Arc<AtomicBool>,
    /// **正在执行备份的任务 id**（无备份时为 `None`）
    ///
    /// `backup_running` 只回答「有没有在跑」，禁用一个插件时需要知道
    /// **具体是哪个任务**在跑（该任务不能被级联禁用，否则会打断正在进行的备份）。
    pub running_task_id: Arc<RwLock<Option<String>>>,
    /// **取消请求标志**（备份 / 恢复共用）
    ///
    /// 置位后正在跑的任务会在**下一个可中断点**停下（文件之间、等待下一个文件时），
    /// 而不是立刻强杀：a. 备份已上传的文件快照已落盘，中断不会破坏一致性；
    /// b. 单个文件的传输在 FFI 里推进，无法从外部打断（同进程模型的固有限制）。
    /// 任务开始前会清零，避免上一次的取消请求把新任务立刻终止。
    pub cancel_requested: Arc<AtomicBool>,
    /// 当前正在跑的是哪类任务（`None` = 空闲），供取消接口判断该不该置位
    pub running_kind: Arc<RwLock<Option<crate::eventbus::TaskKind>>>,
    /// 加密会话（备份加密/恢复解密；密钥变更后可热替换）
    pub crypto: Arc<CryptoSwap>,
    /// 应用口令（密钥库/配置敏感字段加密；更换密钥时需复用它重新加密落盘）
    pub passphrase: Arc<SecretString>,
    /// **管理员口令校验闸门**（常数时间比较 + 失败限速，见 `domain::passphrase`）
    ///
    /// 所有敏感操作（导出私钥 / 导出导入配置 / 查看审计）共用这一个闸门，
    /// 以免各处各写一行 `!=` 而漏掉限速。
    pub passphrase_gate: Arc<crate::domain::passphrase::PassphraseGate>,
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
    /// 插件能力表（宿主 → 插件回调：日志/审计/告警/自配置/进度/定时；详见 `plugin::host_abi`）
    ///
    /// 由插件在 `host_bind` 回调中接收；所有效果经**唯一消费任务**落地。
    pub host_effects: Arc<crate::plugin::host_abi::HostEffects>,
    /// 操作审计日志（敏感/破坏性操作留痕，存 $TRIM_PKGVAR/audit.log）
    pub audit: Arc<crate::domain::audit::AuditLog>,
    /// 插件市场客户端（索引缓存；**默认关闭**，见 `infra::config::MarketSettings`）
    pub market: Arc<crate::plugin::market::Market>,
}

/// 口令校验失败时给用户的统一文案
///
/// 冷却中额外提示剩余秒数 —— 否则用户只会看到「口令错误」，不知道自己被限速了，
/// 反而会反复重试（而重试本身又会被记为失败，陷入死循环）。
pub fn passphrase_err(state: &AppState) -> String {
    match state.passphrase_gate.lock_remaining_secs() {
        Some(secs) => format!("口令错误次数过多，请 {secs} 秒后再试"),
        None => "管理员口令错误".to_string(),
    }
}

impl AppState {
    /// 校验管理员口令（**常数时间比较 + 失败限速**）
    ///
    /// 统一入口：所有敏感操作都走这里，避免各处各写一行 `!=` 而漏掉限速。
    pub fn check_passphrase(&self, input: &str) -> bool {
        self.passphrase_gate
            .verify(input, self.passphrase.expose_secret().as_str())
    }

    /// 按配置重建目标池（启动时、目标/任务配置变更后调用）
    ///
    /// 同步更新：目标池、主目标适配器、主目标 id、`target_ready`。
    /// 返回已就绪（凭据齐备）的目标数量。
    pub fn reload_targets(&self, cfg: &AppConfig) -> usize {
        // 插件启停先于目标装配生效：被禁用的目标插件会让对应目标落到「未配置」分支，
        // 从而给出明确提示，而不是静默使用一个用户已禁用的插件。
        self.plugins.set_disabled(&cfg.plugins.disabled);

        // **只重建配置真正变化的目标**（核心优化）
        //
        // `build` 会为每个目标新建沙箱线程池线程并调用插件 `target_open`
        // （可能等 DNS/远程配置，最长 30s）。而 `reload_targets` 有 13 处调用点
        // （保存目标/任务/插件/市场/开关…），其中绝大多数只改动**一个**目标
        // 或纯全局设置 —— 此前却把**全部**目标都重建一遍，这就是
        // 「保存个配置要等好久」的主要来源。
        //
        // 判据：`TargetConfig` 完全相等 ⇒ 复用池中已有实例。
        // 含凭据**密文**：改了账号/密码密文必变，因此不会误用旧连接。
        let prev = self
            .target_snapshots
            .read()
            .map(|g| g.clone())
            .unwrap_or_default();
        let mut reused: Vec<(String, String, bool)> = Vec::new();
        let mut to_build: Vec<crate::infra::config::TargetConfig> = Vec::new();
        let unchanged_map = {
            let mgr = self.config.lock().unwrap();
            let mut m = std::collections::HashMap::new();
            for t in &cfg.targets {
                if let Some(old) = prev.get(&t.id) {
                    // 用**语义等价**而非 `==`：加密是随机化的（见 TargetConfig::same_as）
                    m.insert(t.id.clone(), t.same_as(old, &mgr));
                }
            }
            m
        };
        for t in &cfg.targets {
            let unchanged = unchanged_map.get(&t.id).copied().unwrap_or(false);
            // 只在「配置没变」且「池里已装配过（不是占位适配器）」时复用
            if unchanged && self.targets.is_ready(&t.id) {
                reused.push((
                    t.id.clone(),
                    self.targets.describe(&t.id).unwrap_or_default(),
                    true,
                ));
            } else {
                to_build.push(t.clone());
            }
        }
        let built = {
            let mgr = self.config.lock().unwrap();
            self.plugins.build_targets_for(&to_build, cfg, &mgr)
        };

        let primary_id = cfg
            .primary_target()
            .map(|t| t.id.clone())
            .unwrap_or_default();

        let mut items: Vec<(String, Arc<dyn TargetStorage>, String, bool)> = Vec::new();
        let mut ready_count = 0usize;
        let mut primary: Option<(Arc<dyn TargetStorage>, String, bool)> = None;
        // 先放入复用的（配置未变，实例原样保留，不重建线程池）
        for (id, name, ready) in reused {
            let storage = self.targets.get(&id);
            if id == primary_id {
                primary = Some((storage.clone(), name.clone(), ready));
            }
            if ready {
                ready_count += 1;
            }
            items.push((id, storage, name, ready));
        }
        for b in built {
            if b.id == primary_id {
                primary = Some((b.storage.clone(), b.name.clone(), b.ready));
            }
            if b.ready {
                ready_count += 1;
            }
            items.push((b.id, b.storage, b.name, b.ready));
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
        // 更新快照：下次 reload 据此判断「没变就复用」
        if let Ok(mut g) = self.target_snapshots.write() {
            *g = cfg.targets.iter().map(|t| (t.id.clone(), t.clone())).collect();
        }
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
