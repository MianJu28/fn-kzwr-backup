//! 内置目标插件：kzwr 官方 WebDAV（ADR-009，默认启用）
//!
//! 备份主通道。凭据来源优先级：`TRIM_DAV_*` 环境变量 > 加密配置 `[webdav]` 段；
//! 地址缺省时回退官方默认地址。

use std::sync::Arc;

use async_trait::async_trait;

use crate::infra::config::ConfigManager;
use crate::infra::storage_trait::TargetStorage;
use crate::infra::target::webdav::{WebdavTarget, DEFAULT_URL};
use crate::plugin::api::{PluginKind, PluginMeta, TargetPlugin};

pub struct WebdavPlugin;

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

/// 解析凭据与地址：环境变量 > 加密配置 > 官方默认地址
fn resolve(mgr: &ConfigManager) -> Option<(String, String, String)> {
    let cfg = mgr.load().ok()?;
    let (cfg_user, cfg_pass) = mgr.webdav_credentials().ok()?;
    let user = env_nonempty("TRIM_DAV_USER")
        .or(cfg_user)
        .filter(|s| !s.is_empty())?;
    let pass = env_nonempty("TRIM_DAV_PASS")
        .or(cfg_pass)
        .filter(|s| !s.is_empty())?;
    let url = env_nonempty("TRIM_DAV_URL")
        .or_else(|| cfg.webdav.url.clone().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    Some((url, user, pass))
}

#[async_trait]
impl TargetPlugin for WebdavPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            id: "webdav".to_string(),
            name: "WebDAV".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            kind: PluginKind::Target,
            builtin: true,
            description: "kzwr 官方 WebDAV 备份目标（默认启用）".to_string(),
        }
    }

    fn build(&self, mgr: &ConfigManager) -> Option<(Arc<dyn TargetStorage>, String)> {
        let (url, user, pass) = resolve(mgr)?;
        Some((
            Arc::new(WebdavTarget::new(&url, &user, &pass)),
            format!("WebDAV（{}）", url),
        ))
    }

    async fn verify(&self, user: &str, pass: &str) -> Result<String, String> {
        let url = env_nonempty("TRIM_DAV_URL").unwrap_or_else(|| DEFAULT_URL.to_string());
        WebdavTarget::new(&url, user, pass)
            .ping()
            .await
            .map(|_| url)
            .map_err(|e| format!("WebDAV 连通性测试失败: {}", e))
    }
}
