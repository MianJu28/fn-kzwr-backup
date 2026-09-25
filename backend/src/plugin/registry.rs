//! 插件注册表：唯一的装配点
//!
//! 替代原先硬编码的 `main.rs::build_target()` 与 `routes.rs` 里写死的 `/kzwr/*` 路由。
//! 核心只问注册表要"当前目标"与"已启用的增强插件"。

use std::sync::Arc;

use crate::infra::config::{AppConfig, ConfigManager};
use crate::infra::storage_trait::{TargetStorage, UnconfiguredTarget};

use super::api::{EnhancePlugin, PluginEntry, TargetPlugin};
use super::builtin;

pub struct PluginRegistry {
    targets: Vec<Arc<dyn TargetPlugin>>,
    enhances: Vec<Arc<dyn EnhancePlugin>>,
}

impl PluginRegistry {
    /// 载入内置插件（编译期固定；后续改外置加载只需替换这里）
    pub fn builtin() -> Self {
        Self {
            targets: vec![Arc::new(builtin::webdav::WebdavPlugin)],
            enhances: vec![Arc::new(builtin::kzwr::KzwrPlugin)],
        }
    }

    /// 当前默认目标插件（设计为单目标，ADR-009）
    pub fn default_target(&self) -> Option<&Arc<dyn TargetPlugin>> {
        self.targets.first()
    }

    pub fn target_plugins(&self) -> &[Arc<dyn TargetPlugin>] {
        &self.targets
    }

    pub fn enhance_plugins(&self) -> &[Arc<dyn EnhancePlugin>] {
        &self.enhances
    }

    /// 插件清单（供 `/api/plugins`；**前端唯一的区块数据来源**）
    pub fn describe(&self, cfg: &AppConfig) -> Vec<PluginEntry> {
        let mut out = Vec::new();
        for p in &self.targets {
            let meta = p.meta();
            out.push(PluginEntry {
                api_base: format!("/api/p/{}", meta.id),
                meta,
                available: true,
                ui: p.ui(),
            });
        }
        for p in &self.enhances {
            let meta = p.meta();
            out.push(PluginEntry {
                api_base: format!("/api/p/{}", meta.id),
                available: p.available(cfg),
                ui: p.ui(),
                meta,
            });
        }
        out
    }

    /// 构建当前备份目标：依次询问目标插件，第一个可用者即当前目标；
    /// 全部不可用 → 占位适配器（服务照常启动，供 UI 完成配置）
    pub fn build_target(&self, mgr: &ConfigManager) -> (Arc<dyn TargetStorage>, String, bool) {
        for p in &self.targets {
            if let Some((t, name)) = p.build(mgr) {
                // 注：日志要打在**库 crate** 里才会被 `fnos_backup=info` 过滤器放行
                // （main.rs 属于二进制 crate，其 info! 默认被过滤）
                tracing::info!(plugin = %p.meta().id, backend = %name, "目标插件已装配");
                return (t, name, true);
            }
        }
        let msg = "备份目标未配置，请在设置中填写 WebDAV 地址与凭据".to_string();
        tracing::info!("无可用目标插件，回退占位适配器（等待用户配置）");
        (
            Arc::new(UnconfiguredTarget {
                message: msg.clone(),
            }),
            msg,
            false,
        )
    }
}
