//! 插件注册表：唯一的装配点
//!
//! 替代原先硬编码的 `main.rs::build_target()` 与 `routes.rs` 里写死的 `/kzwr/*` 路由。
//! 核心只问注册表要"当前目标"与"已启用的增强插件"。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::infra::config::{AppConfig, ConfigManager};
use crate::infra::storage_trait::{TargetStorage, UnconfiguredTarget};

use super::api::{EnhancePlugin, PluginEntry, TargetPlugin};
use super::builtin;
use super::loader::ExternalPluginReport;

pub struct PluginRegistry {
    targets: Vec<Arc<dyn TargetPlugin>>,
    enhances: Vec<Arc<dyn EnhancePlugin>>,
    /// 外置动态库句柄：**必须保活到进程结束**（Drop 会让已注册的 vtable 悬空）
    external_libs: Vec<libloading::Library>,
    /// 外置插件加载报告（供 `/api/plugins` 诊断）
    external_reports: Vec<ExternalPluginReport>,
    /// 外置插件 id → 动态库路径（标注来源）
    external_paths: HashMap<String, String>,
    /// 扫描过的插件目录（(路径, 来源说明)）
    plugin_dirs: Vec<(String, String)>,
}

impl PluginRegistry {
    /// 载入内置插件（编译期固定）
    pub fn builtin() -> Self {
        Self {
            targets: vec![Arc::new(builtin::webdav_abi::WebdavAbiPlugin::new())],
            enhances: vec![Arc::new(builtin::kzwr::KzwrPlugin)],
            external_libs: Vec::new(),
            external_reports: Vec::new(),
            external_paths: HashMap::new(),
            plugin_dirs: Vec::new(),
        }
    }

    /// 加载外置插件（ADR-013 方案 B：动态库）
    ///
    /// `pubkeys` 为配置的插件签名公钥（base64，Ed25519）；非空时强制验签每个 `*.so`。
    /// 失败只记录诊断，不影响内置能力与其它插件。
    pub fn load_external(&mut self, dirs: &[(PathBuf, String)], pubkeys: &[String]) {
        self.plugin_dirs = dirs
            .iter()
            .map(|(p, s)| (p.to_string_lossy().into_owned(), s.clone()))
            .collect();
        let dirs_only: Vec<PathBuf> = dirs.iter().map(|(p, _)| p.clone()).collect();
        let outcome = super::loader::load_external(&dirs_only, pubkeys);
        self.targets.extend(outcome.targets);
        self.enhances.extend(outcome.enhances);
        self.external_libs.extend(outcome.libs);
        for r in &outcome.reports {
            if let (Some(id), true) = (r.id.clone(), r.loaded) {
                self.external_paths.insert(id, r.path.clone());
            }
        }
        self.external_reports = outcome.reports;
    }

    /// 外置插件加载报告（诊断）
    pub fn external_reports(&self) -> &[ExternalPluginReport] {
        &self.external_reports
    }

    /// 扫描过的插件目录（(路径, 来源说明)）
    pub fn plugin_dirs(&self) -> &[(String, String)] {
        &self.plugin_dirs
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

    /// 全部已加载插件 id（目标 + 增强；含内置与外部）——孤立数据检测用
    pub fn plugin_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .targets
            .iter()
            .map(|p| p.meta().id.clone())
            .chain(self.enhances.iter().map(|p| p.meta().id.clone()))
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// 调某插件的 `destroy` 钩子（卸载清除时用；找不到则无操作）
    pub fn call_destroy(&self, id: &str) {
        if let Some(p) = self.enhances.iter().find(|p| p.meta().id == id) {
            p.destroy();
        }
    }

    /// 插件清单（供 `/api/plugins`；**前端唯一的区块数据来源**）
    pub fn describe(&self, cfg: &AppConfig) -> Vec<PluginEntry> {
        let mut out = Vec::new();
        for p in &self.targets {
            let meta = p.meta();
            let path = self.external_paths.get(&meta.id).cloned();
            out.push(PluginEntry {
                api_base: format!("/api/p/{}", meta.id),
                source: if path.is_some() { "external" } else { "builtin" }.to_string(),
                path,
                meta,
                available: true,
                ui: p.ui(),
            });
        }
        for p in &self.enhances {
            let meta = p.meta();
            let path = self.external_paths.get(&meta.id).cloned();
            out.push(PluginEntry {
                api_base: format!("/api/p/{}", meta.id),
                source: if path.is_some() { "external" } else { "builtin" }.to_string(),
                path,
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
