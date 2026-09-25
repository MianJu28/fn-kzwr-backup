//! 内置增强插件：kzwr REST 账号增强（账号信息 / 存储空间与预警 / 云端回收站）
//!
//! **非备份通道**：备份与恢复始终走 WebDAV 目标插件。
//! 本步（P2）先完成"声明与能力登记"；下一步（P3）会把 `/kzwr/*` 路由整体迁到
//! `/api/p/kzwr/*` 并挂到插件的 `routes()`，届时核心路由表里不再有增强功能的代码。

use crate::infra::config::AppConfig;
use crate::plugin::api::{EnhanceCaps, EnhancePlugin, PluginKind, PluginMeta};

pub struct KzwrPlugin;

impl EnhancePlugin for KzwrPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            id: "kzwr".to_string(),
            name: "酷族账号增强".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            kind: PluginKind::Enhance,
            builtin: true,
            description: "账号信息、存储空间与预警、云端回收站清理（需 access-token）".to_string(),
        }
    }

    fn caps(&self) -> EnhanceCaps {
        EnhanceCaps {
            account: true,
            quota: true,
            recycle_bin: true,
            notify: false,
        }
    }

    fn available(&self, cfg: &AppConfig) -> bool {
        cfg.kzwr.access_token_enc.is_some()
    }
}
