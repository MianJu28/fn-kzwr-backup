//! 配置管理（数据归属：配置 → $TRIM_PKGETC）
//!
//! TOML 配置存储，敏感字段（kzwr 用户名/密码/token）用口令派生密钥加密后存储。
//! 支持多备份路径。

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
    /// kzwr REST API 增强功能配置（可选，非备份通道）
    #[serde(default)]
    pub kzwr: KzwrConfig,
    /// 调试日志：开启后输出详细日志（请求/响应明细等），便于问题定位。
    /// 运行时切换即时生效（日志过滤器热更新），并持久化到配置。
    #[serde(default)]
    pub debug: bool,
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

/// kzwr REST API 增强功能配置（可选）
///
/// 备份/恢复仍走官方 WebDAV（ADR-009）；此处的 `access-token` 仅用于
/// WebDAV 提供不了的增强能力：账号存储空间信息、回收站查看/清空等。
/// 用户在浏览器登录 kzwr 后，从 Cookie 的 `access-token` 复制值填入设置页。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KzwrConfig {
    /// access-token（加密存储）
    #[serde(default)]
    pub access_token_enc: Option<String>,
    /// 云端空间占用预警阈值（百分比，0 = 关闭）；达到该值生成告警
    #[serde(default = "default_quota_warn_percent")]
    pub quota_warn_percent: u64,
}

fn default_quota_warn_percent() -> u64 {
    85
}

impl Default for KzwrConfig {
    /// 手写 Default：与 BackupConfig 同理，`#[derive(Default)]` 不会采用
    /// serde 的 default 函数，会让预警阈值默认成 0（= 关闭）。
    fn default() -> Self {
        Self {
            access_token_enc: None,
            quota_warn_percent: default_quota_warn_percent(),
        }
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

    /// 读取 kzwr access-token（解密；未配置或为空返回 None）
    pub fn kzwr_token(&self) -> Result<Option<String>> {
        let cfg = self.load()?;
        Ok(self
            .decrypt_field(&cfg.kzwr.access_token_enc)?
            .filter(|s| !s.is_empty()))
    }

    /// 保存 kzwr access-token（加密存储；传空串则清除）
    pub fn save_kzwr_token(&self, token: &str) -> Result<()> {
        let mut cfg = self.load().unwrap_or_default();
        let token = token.trim();
        if token.is_empty() {
            cfg.kzwr.access_token_enc = None;
        } else {
            cfg.kzwr.access_token_enc = Some(self.encrypt_field(token)?);
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
