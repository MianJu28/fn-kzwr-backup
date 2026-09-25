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

    /// 按插件 id（目标配置里的 `kind`）取目标插件
    pub fn target_plugin(&self, kind: &str) -> Option<&Arc<dyn TargetPlugin>> {
        self.targets.iter().find(|p| p.meta().id == kind)
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

    /// 装配配置中的**全部目标**（多目标：每个目标一份独立实例）
    ///
    /// 凭据不全/插件不支持/已禁用 → `ready=false` + 占位适配器（服务照常启动，
    /// 调用该目标时返回明确的配置提示，不影响其它目标）。
    pub fn build_targets(&self, cfg: &AppConfig, mgr: &ConfigManager) -> Vec<BuiltTarget> {
        cfg.targets
            .iter()
            .map(|t| {
                let built = self
                    .target_plugin(&t.kind)
                    .and_then(|p| p.build(t, mgr));
                match built {
                    Some((storage, name)) => {
                        // 注：日志要打在**库 crate** 里才会被 `fnos_backup=info` 过滤器放行
                        // （main.rs 属于二进制 crate，其 info! 默认被过滤）
                        tracing::info!(target = %t.id, backend = %name, "目标已装配");
                        BuiltTarget {
                            id: t.id.clone(),
                            storage,
                            name,
                            ready: true,
                        }
                    }
                    None => {
                        let name = format!("未配置（{}）", t.name);
                        tracing::info!(
                            target = %t.id,
                            kind = %t.kind,
                            enabled = t.enabled,
                            "目标未装配，回退占位适配器（等待用户配置）"
                        );
                        BuiltTarget {
                            id: t.id.clone(),
                            storage: Arc::new(UnconfiguredTarget {
                                message: format!(
                                    "目标「{}」未配置凭据，请在「目标管理」中完善后重试",
                                    if t.name.is_empty() { &t.id } else { &t.name }
                                ),
                            }),
                            name,
                            ready: false,
                        }
                    }
                }
            })
            .collect()
    }
}

/// 单个目标的装配结果
pub struct BuiltTarget {
    pub id: String,
    pub storage: Arc<dyn TargetStorage>,
    /// 后端描述（日志与 UI 展示）
    pub name: String,
    /// 是否已装配可写（凭据齐备且插件可用）
    pub ready: bool,
}
