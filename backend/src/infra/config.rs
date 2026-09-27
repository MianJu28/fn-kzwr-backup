//! 配置管理（数据归属：配置 → $TRIM_PKGETC）
//!
//! TOML 配置存储，敏感字段（目标凭据、插件自管数据 `plugin_data`）用口令派生密钥加密后存储。
//! 支持多备份路径。

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use age::secrecy::{ExposeSecret, SecretString};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 配置文件
pub const CONFIG_FILE: &str = "config.toml";

/// 应用配置
///
/// ## 多任务 / 多目标（2026-09-26 起）
///
/// 主数据是 [`AppConfig::targets`]（远程目的地列表）与 [`AppConfig::tasks`]
/// （备份任务列表：源路径集 + 目标 + 调度 + 保留策略）。
/// 旧的单实例字段 [`AppConfig::backup`] / [`AppConfig::webdav`] **保留为兼容镜像**：
/// - 载入时若 `tasks`/`targets` 为空 → 由旧字段自动迁移（[`AppConfig::migrate`]）
/// - 保存时把「首个任务/目标」回写进旧字段，便于降级到 0.3.x 仍可读
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    /// ⚠️ 兼容镜像（旧版单任务配置）：新代码请用 [`AppConfig::tasks`]
    #[serde(default)]
    pub backup: BackupConfig,
    /// ⚠️ 兼容镜像（旧版单目标配置）：新代码请用 [`AppConfig::targets`]
    #[serde(default)]
    pub webdav: WebdavConfig,
    /// 远程目标列表（每个目标一套地址 + 凭据，可被多个任务引用）
    #[serde(default)]
    pub targets: Vec<TargetConfig>,
    /// 备份任务列表（每个任务 = 源路径集 + 目标 + cron + 保留策略）
    #[serde(default)]
    pub tasks: Vec<TaskConfig>,
    /// 通知配置（监控告警）
    #[serde(default)]
    pub notify: NotifyConfig,
    /// 密钥配置（私钥备份状态）
    #[serde(default)]
    pub keys: KeyConfig,

    /// 调试日志：开启后输出详细日志（请求/响应明细等），便于问题定位。
    /// 运行时切换即时生效（日志过滤器热更新），并持久化到配置。
    #[serde(default)]
    pub debug: bool,
    /// 外置插件（动态库）设置
    #[serde(default)]
    pub plugins: PluginSettings,
    /// 插件自管数据（ADR-013 决策 2，宿主代加密存储）
    ///
    /// 外层键 = 插件 id；内层键值 = 该插件的键值对。值经 [`ConfigManager::encrypt_field`]
    /// 以 `enc:` 前缀加密后落盘（复用同一口令），读取时由宿主解密后再交给插件。
    /// 卸载插件（`/api/plugins/:id/purge`）时删除该 id 的整个命名空间。
    #[serde(default)]
    pub plugin_data: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
}

/// 外置插件设置（ADR-013 方案 B：动态库）
///
/// **默认关闭**：加载 `*.so` 等价于执行任意本地代码，必须由用户显式开启。
/// 开关变更需**重启应用**生效（插件在启动时装配，运行中不热加载）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSettings {
    /// 是否加载外置插件
    #[serde(default)]
    pub enabled: bool,
    /// 额外插件目录（`:` 分隔多个）；留空则用默认目录
    /// （`$TRIM_PKGETC/plugins`、`$TRIM_APPDEST/plugins`）
    #[serde(default)]
    pub dir: Option<String>,
    /// **每个插件文件一个公钥**（文件名 → base64 的 32 字节 Ed25519 公钥）
    ///
    /// 键为插件文件名（如 `libmy_plugin.so`）。加载该文件时**只用它自己的公钥**
    /// （以及内置官方公钥）验签 —— 这就是「一插件一公钥」：
    /// 插件 A 的公钥**无法**验过插件 B，因此单个插件密钥泄露不会波及其它插件。
    ///
    /// 为什么用**文件名**而不是插件 id：验签发生在 `dlopen` **之前**，
    /// 那时还拿不到插件自报的 id，只有路径/文件名可用。
    #[serde(default)]
    pub plugin_pubkeys: std::collections::BTreeMap<String, String>,
    /// ~~扁平公钥列表~~（**已弃用**，保留仅为兼容旧配置）
    ///
    /// 语义是「任一公钥可验任一插件」，存在越权信任问题（A 的密钥可签 B）。
    /// 新配置请用 [`PluginSettings::plugin_pubkeys`]；这里仅作为**未在
    /// `plugin_pubkeys` 中登记的文件**的回退，避免升级后已有插件全部拒绝加载。
    #[serde(default)]
    pub pubkeys: Vec<String>,
    /// **每个目标插件**各自的上传并发路数（插件 id → 路数）
    ///
    /// 并发回传是插件各自的能力，故按插件 id 分别配置：
    /// 缺省（该 id 不在表中）= 沿用插件自身声明；`0` 或 `1` = 关闭并发回传（顺序上传）；
    /// `≥2` = 启用并发回传，该值即并发路数（宿主上限 8）。
    /// 并发会同时占用多条连接，对 NAS 上行与目标服务端压力更大，故内置插件默认不并发。
    #[serde(default)]
    pub target_parallel: std::collections::BTreeMap<String, u32>,
    /// **按插件禁用**的插件 id 集合（运行时启停，无需重启）
    ///
    /// 与 [`PluginSettings::enabled`]（全局总开关）的分工：
    /// - `enabled=false` → 根本不扫描/加载任何外置插件；
    /// - 本字段 → 插件**已加载**（代码仍在内存中），但从活动集合里摘除：
    ///   不再出现在 `/api/plugins`、不参与路由分发、不再被装配为备份目标。
    ///
    /// 为什么禁用 ≠ 卸载动态库：插件返回的 vtable 被宿主按 `&'static` 持有，
    /// 飞行中的备份也可能仍持有由它派生的 `Arc<dyn TargetStorage>`，
    /// 卸载会让这些引用悬空 → 进程崩溃。故「禁用」只做**逻辑摘除**，
    /// 真正释放内存需重启应用（详见 `docs/PLUGIN_ABI.md`）。
    ///
    /// 内置插件（webdav/kzwr）同样可被禁用；禁用一个仍被任务引用的插件会
    /// **级联禁用**那些任务（由接口负责，并如实回报受影响的任务）。
    #[serde(default)]
    pub disabled: Vec<String>,
}

impl Default for PluginSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            dir: None,
            plugin_pubkeys: std::collections::BTreeMap::new(),
            pubkeys: Vec::new(),
            target_parallel: std::collections::BTreeMap::new(),
            disabled: Vec::new(),
        }
    }
}

/// 迁移后的默认目标 id（旧配置升级时创建）
pub const DEFAULT_TARGET_ID: &str = "default";
/// 迁移后的默认任务 id
///
/// 刻意取 `default`：它与旧版的 `AppState.job_id`（`TRIM_JOB_ID`，默认 "default"）
/// 一致，因此快照 key 仍是 `default-0`、`default-1`…——**升级后不会全量重传**。
pub const DEFAULT_TASK_ID: &str = "default";

/// 远程目标：一个目的地 = 一个目标插件实例 + 一套凭据
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TargetConfig {
    /// 稳定 id（任务通过它引用目标；创建后不再改变）
    pub id: String,
    /// 显示名（如「酷族主账号」）
    #[serde(default)]
    pub name: String,
    /// 目标插件 id（`plugin::api::TargetPlugin::meta().id`，如 `webdav`）
    #[serde(default = "default_target_kind")]
    pub kind: String,
    /// 目标基址（如 https://dav.kzwr.com/dav）
    #[serde(default)]
    pub url: Option<String>,
    /// 用户名（加密存储）
    #[serde(default)]
    pub username_enc: Option<String>,
    /// 密码（加密存储）
    #[serde(default)]
    pub password_enc: Option<String>,
    /// 是否启用（禁用后引用它的任务不可运行）
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_target_kind() -> String {
    "webdav".to_string()
}

fn default_true() -> bool {
    true
}

impl TargetConfig {
    /// 凭据是否已配置（用户名 + 密码齐备）
    pub fn configured(&self) -> bool {
        self.username_enc.is_some() && self.password_enc.is_some()
    }
}

/// 备份任务：源路径集 + 目标 + 调度 + 保留策略
///
/// 快照隔离：`job_id = "{task.id}-{源序号}"`、`account = 该目标任务凭据的用户名`，
/// 因此**每个任务在每个目标上都有独立的快照、增量与保留策略**。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskConfig {
    /// 稳定 id（快照 job_id 前缀，创建后不可改变）
    pub id: String,
    /// 任务名（如「文档备份」）
    #[serde(default)]
    pub name: String,
    /// 是否启用（停用后不参与调度，手动触发也会被拒）
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 备份源路径列表
    #[serde(default)]
    pub paths: Vec<String>,
    /// 目标 id（引用 [`AppConfig::targets`]）
    #[serde(default)]
    pub target_id: String,
    /// 目标端前缀文件夹（如 "fn-backup"）
    #[serde(default = "default_target_folder")]
    pub target_folder: String,
    /// 定时备份 cron 表达式（None/空 = 不启用）
    #[serde(default)]
    pub schedule_cron: Option<String>,
    /// 保留策略（孤儿文件清理等）
    #[serde(default)]
    pub retention: RetentionConfig,
}

impl Default for TaskConfig {
    /// 手写 Default：与 `BackupConfig` 同理，`derive(Default)` 不会采用 serde 的
    /// `default_target_folder()`，会让 `target_folder` 变空串。
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: true,
            paths: Vec::new(),
            target_id: DEFAULT_TARGET_ID.to_string(),
            target_folder: default_target_folder(),
            schedule_cron: None,
            retention: RetentionConfig::default(),
        }
    }
}

impl AppConfig {
    /// 旧配置 → 多任务/多目标模型（幂等；返回是否发生了变更）
    ///
    /// 迁移规则：
    /// - 无 `targets`：用旧 `[webdav]` 造一个 id=`default` 的目标（含凭据密文，不重新加密）
    /// - 无 `tasks` 且旧 `backup.paths` 非空：造一个 id=`default` 的任务
    ///
    /// 迁移**不删除**旧字段（继续作为兼容镜像由 [`AppConfig::sync_legacy_mirror`] 更新）。
    pub fn migrate(&mut self) -> bool {
        let mut changed = false;
        if self.targets.is_empty() {
            self.targets.push(TargetConfig {
                id: DEFAULT_TARGET_ID.to_string(),
                name: "默认目标（WebDAV）".to_string(),
                kind: default_target_kind(),
                url: self.webdav.url.clone(),
                username_enc: self.webdav.username_enc.clone(),
                password_enc: self.webdav.password_enc.clone(),
                enabled: true,
            });
            changed = true;
        }
        if self.tasks.is_empty() && !self.backup.paths.is_empty() {
            self.tasks.push(TaskConfig {
                id: DEFAULT_TASK_ID.to_string(),
                name: "默认任务".to_string(),
                enabled: true,
                paths: self.backup.paths.clone(),
                target_id: DEFAULT_TARGET_ID.to_string(),
                target_folder: self.backup.target_folder.clone(),
                schedule_cron: self.backup.schedule_cron.clone(),
                retention: self.backup.retention.clone(),
            });
            changed = true;
        }
        changed
    }

    /// 把「首个任务/目标」回写进旧字段（兼容镜像；降级到 0.3.x 也能读到）
    pub fn sync_legacy_mirror(&mut self) {
        if let Some(t) = self.targets.first() {
            self.webdav.url = t.url.clone();
            self.webdav.username_enc = t.username_enc.clone();
            self.webdav.password_enc = t.password_enc.clone();
        }
        if let Some(t) = self.tasks.first() {
            self.backup.paths = t.paths.clone();
            self.backup.target_folder = t.target_folder.clone();
            self.backup.schedule_cron = t.schedule_cron.clone();
            self.backup.retention = t.retention.clone();
        }
    }

    pub fn target_by_id(&self, id: &str) -> Option<&TargetConfig> {
        self.targets.iter().find(|t| t.id == id)
    }

    pub fn task_by_id(&self, id: &str) -> Option<&TaskConfig> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// 主目标：首个启用的目标（kzwr 增强等全局能力绑定它）
    pub fn primary_target(&self) -> Option<&TargetConfig> {
        self.targets.iter().find(|t| t.enabled).or(self.targets.first())
    }

    /// 引用了该目标的任务名（删除目标前检查）
    pub fn tasks_using_target(&self, target_id: &str) -> Vec<String> {
        self.tasks
            .iter()
            .filter(|t| t.target_id == target_id)
            .map(|t| {
                if t.name.is_empty() {
                    t.id.clone()
                } else {
                    t.name.clone()
                }
            })
            .collect()
    }
}


/// 备份配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupConfig {
    /// 备份源路径列表（支持多个）
    #[serde(default)]
    pub paths: Vec<String>,
    /// 目标文件夹前缀
    #[serde(default = "default_target_folder")]
    pub target_folder: String,
    /// 保留策略（孤儿文件清理）
    #[serde(default)]
    pub retention: RetentionConfig,
    /// 定时备份 cron 表达式（如 "0 0 * * *" 每天零点；None/空 = 不启用）
    #[serde(default)]
    pub schedule_cron: Option<String>,
}

impl Default for BackupConfig {
    /// 手写 Default：`#[derive(Default)]` 不会采用 serde 的
    /// `default_target_folder()`，会让全新配置的 `target_folder` 变成空串，
    /// 导致首次备份直接落在目标根目录而非 `fn-backup/`。
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            target_folder: default_target_folder(),
            retention: RetentionConfig::default(),
            schedule_cron: None,
        }
    }
}

/// 保留策略配置
///
/// 当前系统为镜像同步（目标与源保持一致），保留策略聚焦于
/// 目标端孤儿文件清理：清理目标端残留的、不在任何备份任务
/// 快照管理下的文件，防止目标空间无限膨胀。不改动备份逻辑。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// 是否启用保留策略
    #[serde(default)]
    pub enabled: bool,
    /// 清理未管理（孤儿）文件：删除目标端存在但不在任何快照中的文件
    #[serde(default)]
    pub cleanup_unmanaged: bool,
    /// 可选：只清理创建时间早于该天数（0 表示不限制）
    #[serde(default)]
    pub min_age_days: u64,
    /// 备份后清空云端回收站（需配置 kzwr access-token，走 REST 增强功能）
    #[serde(default)]
    pub empty_recycle_bin: bool,
    /// 回收站占用超过该 GB 数才清理（0 = 不限制，总是清理）
    #[serde(default)]
    pub recycle_max_gb: u64,
    /// 只清理删除时间早于该天数的回收站条目（0 = 不限制）
    #[serde(default)]
    pub recycle_min_age_days: u64,
}

fn default_target_folder() -> String {
    "fn-backup".to_string()
}

/// WebDAV 目标配置（ADR-009）
///
/// 凭据加密存储（格式 "enc:<age密文>"）。
/// 也可用环境变量 TRIM_DAV_URL / TRIM_DAV_USER / TRIM_DAV_PASS 覆盖（优先）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebdavConfig {
    /// WebDAV 基址（如 https://dav.kzwr.com/dav）
    #[serde(default)]
    pub url: Option<String>,
    /// 用户名（加密存储）
    #[serde(default)]
    pub username_enc: Option<String>,
    /// 密码（加密存储）
    #[serde(default)]
    pub password_enc: Option<String>,
}

/// 通知配置（监控告警）
///
/// `webhook_url` 为空表示不外发，告警仅在应用内展示。
/// 支持自定义请求头（`webhook_headers`）与请求体模板（`webhook_body`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotifyConfig {
    /// 告警 Webhook 地址（空 = 不外发）
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// 自定义请求头（如 Authorization、Content-Type）
    #[serde(default)]
    pub webhook_headers: Vec<WebhookHeader>,
    /// 自定义请求体模板（支持占位符 {{message}}/{{level}}/{{source}}/{{ts}}/{{id}}；
    /// 留空则发送默认 JSON）
    #[serde(default)]
    pub webhook_body: Option<String>,
}

/// Webhook 自定义请求头（键值对）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookHeader {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// 密钥配置（age 私钥备份状态）
///
/// `backed_up` 表示用户是否已确认妥善保存私钥；未提供备份确认时 UI 会持续提示风险。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeyConfig {
    /// 用户是否已确认备份私钥
    #[serde(default)]
    pub backed_up: bool,
}

/// 配置管理器
pub struct ConfigManager {
    /// 配置文件路径
    path: PathBuf,
    /// 加密口令（派生密钥加密敏感字段）
    passphrase: SecretString,
    /// 解密结果缓存（键 = 密文全文）
    ///
    /// age scrypt 解密单次耗时可达数百毫秒，而 `webdav_username`/`kzwr_token`
    /// 在每个请求热路径上都会解密。以**密文本身**作缓存键：用户保存新凭据时
    /// 密文随之改变、旧键自然失效，无需手动清理。
    cache: std::sync::Mutex<std::collections::HashMap<String, Option<String>>>,
}

impl ConfigManager {
    /// 创建配置管理器，passphrase 用于敏感字段加解密
    pub fn new(cfg_dir: &Path, passphrase: SecretString) -> Self {
        Self {
            path: cfg_dir.join(CONFIG_FILE),
            passphrase,
            cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// 加载配置（不存在则返回默认）；**顺带做旧配置迁移（仅内存，不落盘）**
    pub fn load(&self) -> Result<AppConfig> {
        if !self.path.exists() {
            let mut cfg = AppConfig::default();
            cfg.migrate();
            return Ok(cfg);
        }
        let content = std::fs::read_to_string(&self.path).context("读取配置文件失败")?;
        let mut cfg: AppConfig = toml::from_str(&content).context("解析配置失败")?;
        cfg.migrate();
        Ok(cfg)
    }

    /// 加载配置；若发生了旧配置迁移则立即落盘（供启动时调用一次）
    pub fn load_and_persist_migration(&self) -> Result<AppConfig> {
        if !self.path.exists() {
            return self.load();
        }
        let content = std::fs::read_to_string(&self.path).context("读取配置文件失败")?;
        let mut cfg: AppConfig = toml::from_str(&content).context("解析配置失败")?;
        if cfg.migrate() {
            tracing::info!(
                targets = cfg.targets.len(),
                tasks = cfg.tasks.len(),
                "检测到旧版单任务配置，已迁移为多任务/多目标模型"
            );
            self.save(&cfg)?;
        }
        Ok(cfg)
    }

    /// 保存配置（同时刷新旧字段兼容镜像）
    pub fn save(&self, config: &AppConfig) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut cfg = config.clone();
        cfg.sync_legacy_mirror();
        let content = toml::to_string_pretty(&cfg).context("序列化配置失败")?;
        std::fs::write(&self.path, content).context("写入配置文件失败")?;
        Ok(())
    }

    /// 解密某个目标的凭据
    ///
    /// 返回 (用户名, 密码)，任一缺失/为空则为 None。
    pub fn target_credentials(
        &self,
        target: &TargetConfig,
    ) -> Result<(Option<String>, Option<String>)> {
        let user = self
            .decrypt_field(&target.username_enc)?
            .filter(|s| !s.is_empty());
        let pass = self
            .decrypt_field(&target.password_enc)?
            .filter(|s| !s.is_empty());
        Ok((user, pass))
    }

    /// 读取**主目标**的凭据（解密；无目标时返回 None）
    pub fn webdav_credentials(&self) -> Result<(Option<String>, Option<String>)> {
        let cfg = self.load()?;
        match cfg.primary_target() {
            Some(t) => self.target_credentials(t),
            None => Ok((None, None)),
        }
    }

    /// 保存主目标的 WebDAV 配置（url + 加密凭据）；无目标则创建默认目标
    pub fn save_webdav(&self, url: &str, username: &str, password: &str) -> Result<()> {
        let mut cfg = self.load().unwrap_or_default();
        let enc_user = self.encrypt_field(username)?;
        let enc_pass = self.encrypt_field(password)?;
        let url = url.trim_end_matches('/').to_string();
        if cfg.targets.is_empty() {
            cfg.targets.push(TargetConfig {
                id: DEFAULT_TARGET_ID.to_string(),
                name: "默认目标（WebDAV）".to_string(),
                kind: default_target_kind(),
                url: Some(url),
                username_enc: Some(enc_user),
                password_enc: Some(enc_pass),
                enabled: true,
            });
        } else {
            // 主目标（首个启用者）就地更新
            let idx = cfg
                .targets
                .iter()
                .position(|t| t.enabled)
                .unwrap_or(0);
            let t = &mut cfg.targets[idx];
            t.url = Some(url);
            t.username_enc = Some(enc_user);
            t.password_enc = Some(enc_pass);
        }
        self.save(&cfg)
    }



    /// 加密敏感字段（返回 "enc:<密文>"）
    pub fn encrypt_field(&self, plain: &str) -> Result<String> {
        let encrypted = self.encrypt(plain.as_bytes())?;
        Ok(format!("enc:{}", base64(&encrypted)))
    }

    /// 解密敏感字段（支持 "enc:xxx" 或明文；enc 结果按密文缓存，避免重复 scrypt）
    pub fn decrypt_field(&self, field: &Option<String>) -> Result<Option<String>> {
        match field {
            None => Ok(None),
            Some(f) => {
                if let Some(rest) = f.strip_prefix("enc:") {
                    if let Some(hit) = self.cache.lock().unwrap().get(f) {
                        return Ok(hit.clone());
                    }
                    let data = decode_base64(rest)?;
                    let plain = self.decrypt(&data)?;
                    let s = String::from_utf8(plain).context("解密结果非 UTF-8")?;
                    self.cache
                        .lock()
                        .unwrap()
                        .insert(f.clone(), Some(s.clone()));
                    Ok(Some(s))
                } else {
                    // 明文（旧配置或未加密）
                    Ok(Some(f.clone()))
                }
            }
        }
    }

    /// 读取某插件的自管配置值（解密后返回明文；不存在/未配置 → None）
    ///
    /// 内部值以 `enc:` 前缀加密存储，这里复用 `decrypt_field` 解密。
    pub fn plugin_data_get(&self, cfg: &AppConfig, plugin: &str, key: &str) -> Result<Option<String>> {
        let enc = cfg
            .plugin_data
            .get(plugin)
            .and_then(|m| m.get(key))
            .cloned();
        self.decrypt_field(&enc)
    }

    /// 写入某插件的自管配置值（加密后落库存入 `cfg.plugin_data`；值空则删除该键）
    ///
    /// 调用方随后需 `save(&cfg)` 落盘。
    pub fn plugin_data_set(
        &self,
        cfg: &mut AppConfig,
        plugin: &str,
        key: &str,
        value: &str,
    ) -> Result<()> {
        let entry = cfg.plugin_data.entry(plugin.to_string()).or_default();
        if value.is_empty() {
            entry.remove(key);
            if entry.is_empty() {
                cfg.plugin_data.remove(plugin);
            }
        } else {
            entry.insert(key.to_string(), self.encrypt_field(value)?);
        }
        Ok(())
    }

    /// 删除某插件的整个自管配置命名空间（卸载清除用）；返回是否删除了东西
    pub fn plugin_data_remove(&self, cfg: &mut AppConfig, plugin: &str) -> bool {
        cfg.plugin_data.remove(plugin).is_some()
    }

    /// 当前有自管数据的插件 id 列表（孤立数据检测用）
    pub fn plugin_data_ids(&self, cfg: &AppConfig) -> Vec<String> {
        cfg.plugin_data.keys().cloned().collect()
    }

    /// 导出用：把指定插件的自管配置全部解密为明文键值对（换机/备份携带；敏感，需管理员口令）
    pub fn plugin_data_export(
        &self,
        cfg: &AppConfig,
        plugin: &str,
    ) -> Result<BTreeMap<String, String>> {
        let mut out = BTreeMap::new();
        if let Some(m) = cfg.plugin_data.get(plugin) {
            for (k, enc) in m {
                if let Some(v) = self.decrypt_field(&Some(enc.clone()))? {
                    out.insert(k.clone(), v);
                }
            }
        }
        Ok(out)
    }

    /// 把某插件的自管配置解密为 **JSON 对象**（注入目标 `target_json.config`）
    ///
    /// 这是插件读取自管配置的**唯一受支持路径**：宿主在建立实例前把命名空间解密后
    /// 放进 `target_json.config`，插件用 `config.root` 这类键读取（见 `example-localfs`）。
    /// 解密失败/无数据 → 空对象，插件应回退到 `url` 等既有字段。
    ///
    /// **返回值含插件自有凭据，调用方不得写入日志。**
    pub fn plugin_data_json(&self, cfg: &AppConfig, plugin: &str) -> serde_json::Value {
        match self.plugin_data_export(cfg, plugin) {
            Ok(kv) => serde_json::Value::Object(
                kv.into_iter()
                    .map(|(k, v)| (k, serde_json::Value::String(v)))
                    .collect(),
            ),
            Err(e) => {
                // 只记插件 id 与错误，**不记键值**
                tracing::warn!(plugin = %plugin, err = %e, "插件自管配置解密失败，按空配置注入");
                serde_json::Value::Object(serde_json::Map::new())
            }
        }
    }

    /// 导入用：把明文键值对重新加密写入某插件的命名空间（调用方随后 `save`）
    pub fn plugin_data_import(
        &self,
        cfg: &mut AppConfig,
        plugin: &str,
        kv: &BTreeMap<String, String>,
    ) -> Result<()> {
        for (k, v) in kv {
            self.plugin_data_set(cfg, plugin, k, v)?;
        }
        Ok(())
    }

    /// 用 passphrase 加密（age scrypt）
    fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        let encryptor = age::Encryptor::with_user_passphrase(self.passphrase.clone());
        let mut out = Vec::new();
        let mut writer = encryptor
            .wrap_output(&mut out)
            .context("创建加密流失败")?;
        writer.write_all(data).context("加密写入失败")?;
        writer.finish().context("完成加密失败")?;
        Ok(out)
    }

    /// 用 passphrase 解密
    fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        let decryptor = age::Decryptor::new(data).context("创建解密器失败")?;
        let decryptor = match decryptor {
            age::Decryptor::Passphrase(d) => d,
            _ => return Err(anyhow::anyhow!("非口令加密格式")),
        };
        let mut reader = decryptor
            .decrypt(&self.passphrase, None)
            .context("解密失败")?;
        let mut out = Vec::new();
        reader.read_to_end(&mut out).context("解密读取失败")?;
        Ok(out)
    }
}

/// base64 编码
fn base64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// base64 解码
fn decode_base64(s: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .context("base64 解码失败")
}

// 避免未使用警告
#[allow(dead_code)]
fn _use_secret(s: &SecretString) {
    let _ = s.expose_secret();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一个临时配置目录的 `ConfigManager`（口令任意，测试内自洽）
    fn mgr() -> (tempfile::TempDir, ConfigManager) {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = ConfigManager::new(dir.path(), SecretString::from("test-pass".to_string()));
        (dir, m)
    }

    /// 0.4.4 的真实配置文件必须能被 0.4.5 载入（升级即服务启动，不能解析失败）
    ///
    /// 迁移把 `[kzwr]` 段连同 `access_token_enc` / `quota_warn_percent` 一并从宿主删除，
    /// 但**已安装用户的 config.toml 里还留着这一段**。若解析器严格拒绝未知段，
    /// 升级后服务直接起不来 —— 这是本次迁移最致命的一种回归，故用**完整形状**钉死：
    /// 除 `[kzwr]` 外，其余各段（目标/任务/通知/插件数据）必须**一个都不丢**。
    #[test]
    fn legacy_kzwr_section_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[webdav]
url = "https://dav.example.com/dav"
username = "enc:AAAA"
password = "enc:BBBB"

[[targets]]
id = "default"
name = "默认目标（WebDAV）"
kind = "webdav"
base_url = "https://dav.example.com/dav"
enabled = true

[[tasks]]
id = "default"
name = "默认任务"
paths = ["/vol1/1000/video"]
target_id = "default"
target_folder = "fn-backup"
schedule_cron = "0 2 * * *"

[tasks.retention]
enabled = true
empty_recycle_bin = true
recycle_max_gb = 20
recycle_min_age_days = 7

[notify]
webhook_url = "https://example.com/hook"

[keys]
backed_up = true

[plugins]
enabled = true

# ── 0.4.4 遗留：增强功能的宿主配置段，0.4.5 起归插件（必须被忽略） ──
[kzwr]
access_token_enc = "enc:ZZZZ"
quota_warn_percent = 85

[plugin_data.example]
greeting = "enc:CCCC"
"#,
        )
        .expect("write");
        let m = ConfigManager::new(dir.path(), SecretString::from("p".to_string()));
        let cfg = m.load().expect("含遗留 [kzwr] 段的配置应能载入");
        assert_eq!(cfg.webdav.url.as_deref(), Some("https://dav.example.com/dav"));
        // 其余各段完好
        assert_eq!(cfg.targets.len(), 1, "目标列表应完好");
        assert_eq!(cfg.tasks.len(), 1, "任务列表应完好");
        assert_eq!(cfg.tasks[0].retention.recycle_max_gb, 20, "任务级回收站门槛应完好");
        assert!(cfg.tasks[0].retention.empty_recycle_bin, "保留策略开关应完好");
        assert!(cfg.plugins.enabled, "外置插件开关应完好");
        assert_eq!(
            cfg.plugin_data.keys().collect::<Vec<_>>(),
            vec!["example"],
            "其它插件的自管数据不得受迁移影响"
        );
        // 迁移是**不搬运**旧 token 的（用户重填）：这里确认宿主确实不再持有该段，
        // 而不是「还在但没人读」——留着只会让导出包继续带敏感明文。
        let text = toml::to_string(&cfg).expect("重新序列化");
        assert!(!text.contains("quota_warn_percent"), "遗留 [kzwr] 段应在下次保存时消失：{text}");
    }

    /// A1+A2 闭环：`plugin_data_set` 加密落库 → `plugin_data_json` 解密注入 `target_json.config`
    ///
    /// 这条链路此前是**断的**：路由能写入、`target_json` 却硬编码 `"config": {}`，
    /// 于是插件永远读到空配置。这里把「写入 → 落盘 → 重新加载 → 注入」全程走一遍。
    #[test]
    fn plugin_data_round_trips_into_target_json_config() {
        let (_d, m) = mgr();
        let mut cfg = AppConfig::default();

        m.plugin_data_set(&mut cfg, "example-localfs", "root", "/vol1/backup")
            .expect("set root");
        m.plugin_data_set(&mut cfg, "example-localfs", "token", "s3cr3t")
            .expect("set token");

        // 落库的值必须是密文（不能明文躺在配置文件里）
        let stored = cfg.plugin_data.get("example-localfs").expect("namespace");
        assert_ne!(stored.get("token").unwrap(), "s3cr3t", "凭据必须加密存储");
        assert!(stored.get("token").unwrap().starts_with("enc:"), "应带 enc: 前缀");

        // 解密导出 → 明文键值对
        let kv = m.plugin_data_export(&cfg, "example-localfs").expect("export");
        assert_eq!(kv.get("root").map(String::as_str), Some("/vol1/backup"));
        assert_eq!(kv.get("token").map(String::as_str), Some("s3cr3t"));

        // 注入形态：JSON 对象（插件侧读 config.root）
        let j = m.plugin_data_json(&cfg, "example-localfs");
        assert_eq!(j.get("root").and_then(|v| v.as_str()), Some("/vol1/backup"));
        assert_eq!(j.get("token").and_then(|v| v.as_str()), Some("s3cr3t"));

        // 落盘 → 重新加载后依然可解密（确保口令/盐随文件持久化正确）
        m.save(&cfg).expect("save");
        let reloaded = m.load().expect("load");
        let j2 = m.plugin_data_json(&reloaded, "example-localfs");
        assert_eq!(
            j2.get("root").and_then(|v| v.as_str()),
            Some("/vol1/backup"),
            "重启后插件仍应读到配置"
        );
    }

    /// 空值 = 删除该键；命名空间空了则整体移除（避免留下空壳被当成「孤立数据」）
    #[test]
    fn plugin_data_set_empty_value_deletes_key() {
        let (_d, m) = mgr();
        let mut cfg = AppConfig::default();
        m.plugin_data_set(&mut cfg, "p", "k", "v").unwrap();
        assert!(cfg.plugin_data.contains_key("p"));

        m.plugin_data_set(&mut cfg, "p", "k", "").unwrap();
        assert!(!cfg.plugin_data.contains_key("p"), "空值应删除键并清掉空命名空间");
    }

    /// 未配置的插件 → 空对象（插件据此回退到 `url` 等既有字段，而不是报错）
    #[test]
    fn plugin_data_json_empty_for_unknown_plugin() {
        let (_d, m) = mgr();
        let cfg = AppConfig::default();
        let j = m.plugin_data_json(&cfg, "nope");
        assert!(j.as_object().expect("object").is_empty());
    }

    /// `plugin_data_remove`（卸载清除）返回是否真的删了东西
    #[test]
    fn plugin_data_remove_reports_whether_it_deleted() {
        let (_d, m) = mgr();
        let mut cfg = AppConfig::default();
        m.plugin_data_set(&mut cfg, "p", "k", "v").unwrap();
        assert!(m.plugin_data_remove(&mut cfg, "p"), "首次删除应返回 true");
        assert!(!m.plugin_data_remove(&mut cfg, "p"), "再次删除应返回 false");
    }
}
