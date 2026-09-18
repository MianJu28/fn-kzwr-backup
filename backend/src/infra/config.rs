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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub backup: BackupConfig,
    /// WebDAV 目标配置（ADR-009：官方 WebDAV，唯一文件管理通道）
    #[serde(default)]
    pub webdav: WebdavConfig,
    /// 通知配置（监控告警）
    #[serde(default)]
    pub notify: NotifyConfig,
    /// 密钥配置（私钥备份状态）
    #[serde(default)]
    pub keys: KeyConfig,
}

/// 备份配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotifyConfig {
    /// 告警 Webhook 地址（空 = 不外发）
    #[serde(default)]
    pub webhook_url: Option<String>,
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
}

impl ConfigManager {
    /// 创建配置管理器，passphrase 用于敏感字段加解密
    pub fn new(cfg_dir: &Path, passphrase: SecretString) -> Self {
        Self {
            path: cfg_dir.join(CONFIG_FILE),
            passphrase,
        }
    }

    /// 加载配置（不存在则返回默认）
    pub fn load(&self) -> Result<AppConfig> {
        if !self.path.exists() {
            return Ok(AppConfig::default());
        }
        let content = std::fs::read_to_string(&self.path).context("读取配置文件失败")?;
        toml::from_str(&content).context("解析配置失败")
    }

    /// 保存配置
    pub fn save(&self, config: &AppConfig) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let content = toml::to_string_pretty(config).context("序列化配置失败")?;
        std::fs::write(&self.path, content).context("写入配置文件失败")?;
        Ok(())
    }

    /// 读取 WebDAV 凭据（解密）
    ///
    /// 返回 (用户名, 密码)，任一缺失/为空则为 None。
    pub fn webdav_credentials(&self) -> Result<(Option<String>, Option<String>)> {
        let cfg = self.load()?;
        let user = self
            .decrypt_field(&cfg.webdav.username_enc)?
            .filter(|s| !s.is_empty());
        let pass = self
            .decrypt_field(&cfg.webdav.password_enc)?
            .filter(|s| !s.is_empty());
        Ok((user, pass))
    }

    /// 保存 WebDAV 配置（url + 加密凭据）
    pub fn save_webdav(&self, url: &str, username: &str, password: &str) -> Result<()> {
        let mut cfg = self.load().unwrap_or_default();
        cfg.webdav.url = Some(url.trim_end_matches('/').to_string());
        cfg.webdav.username_enc = Some(self.encrypt_field(username)?);
        cfg.webdav.password_enc = Some(self.encrypt_field(password)?);
        self.save(&cfg)
    }

    /// 加密敏感字段（返回 "enc:<密文>"）
    pub fn encrypt_field(&self, plain: &str) -> Result<String> {
        let encrypted = self.encrypt(plain.as_bytes())?;
        Ok(format!("enc:{}", base64(&encrypted)))
    }

    /// 解密敏感字段（支持 "enc:xxx" 或明文）
    pub fn decrypt_field(&self, field: &Option<String>) -> Result<Option<String>> {
        match field {
            None => Ok(None),
            Some(f) => {
                if let Some(rest) = f.strip_prefix("enc:") {
                    let data = decode_base64(rest)?;
                    let plain = self.decrypt(&data)?;
                    Ok(Some(String::from_utf8(plain).context("解密结果非 UTF-8")?))
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
