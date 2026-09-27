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
    /// **被禁用的插件 id**（运行时启停）
    ///
    /// 禁用只做**逻辑摘除**：插件对象仍在此结构中（动态库也仍映射在内存），
    /// 但所有查询接口都会把它过滤掉。这样飞行中的 `Arc` 引用不会悬空。
    ///
    /// 用 `RwLock` 而非普通字段：注册表被 `Arc<PluginRegistry>` 共享，
    /// 启停接口只有 `&self`（与 `TargetPool` 同样的内部可变性做法）。
    disabled: std::sync::RwLock<std::collections::HashSet<String>>,
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
            disabled: std::sync::RwLock::new(std::collections::HashSet::new()),
        }
    }

    /// 设置**被禁用**的插件 id 集合（整体替换；配置变更时调用，立即生效）
    ///
    /// 只影响查询结果，不卸载任何动态库（见结构体 `disabled` 字段的说明）。
    /// 返回本次实际生效的禁用数量。
    pub fn set_disabled(&self, ids: &[String]) -> usize {
        let next: std::collections::HashSet<String> = ids
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let n = next.len();
        *self.disabled.write().unwrap() = next;
        n
    }

    /// 某插件当前是否被禁用
    pub fn is_disabled(&self, id: &str) -> bool {
        self.disabled.read().unwrap().contains(id)
    }

    /// 被禁用的插件 id（排序后，供 `/api/plugins` 回显）
    pub fn disabled_ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.disabled.read().unwrap().iter().cloned().collect();
        v.sort();
        v
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
        if self.is_disabled(kind) {
            return None;
        }
        self.targets.iter().find(|p| p.meta().id == kind)
    }

    /// 全部**启用中**的目标插件（被禁用者已过滤）
    pub fn target_plugins(&self) -> Vec<&Arc<dyn TargetPlugin>> {
        self.targets
            .iter()
            .filter(|p| !self.is_disabled(&p.meta().id))
            .collect()
    }

    /// 全部**启用中**的增强插件（被禁用者已过滤）
    ///
    /// 生命周期钩子（`on_startup`/`patrol`/`after_backup`）与路由挂载都走这里，
    /// 因此禁用后插件立即不再被调用。
    pub fn enhance_plugins(&self) -> Vec<&Arc<dyn EnhancePlugin>> {
        self.enhances
            .iter()
            .filter(|p| !self.is_disabled(&p.meta().id))
            .collect()
    }

    /// 按 id 取**启用中**的增强插件（路由分发用；被禁用 → `None`）
    pub fn enhance_plugin_enabled(&self, id: &str) -> Option<&Arc<dyn EnhancePlugin>> {
        if self.is_disabled(id) {
            return None;
        }
        self.enhances.iter().find(|p| p.meta().id == id)
    }

    /// 按 id 取增强插件（**含被禁用者**；诊断/卸载等管理操作用）
    pub fn enhance_plugin_any(&self, id: &str) -> Option<&Arc<dyn EnhancePlugin>> {
        self.enhances.iter().find(|p| p.meta().id == id)
    }

    /// 按 id 取目标插件（**含被禁用者**；诊断与管理操作用）
    pub fn target_plugin_any(&self, id: &str) -> Option<&Arc<dyn TargetPlugin>> {
        self.targets.iter().find(|p| p.meta().id == id)
    }

    /// 全部已加载插件 id（目标 + 增强；含内置与外部）
    ///
    /// **不过滤** disabled：被禁用的插件其 `plugin_data` 仍应被视为「有归属」，
    /// 不该被孤立数据检测提示成遗留配置。
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
    ///
    /// **包含被禁用的插件**（`disabled: true`）：插件页需要列出它们以便重新启用。
    /// 其它消费方（目标装配、路由分发、生命周期钩子）走的是过滤后的接口。
    pub fn describe(&self, cfg: &AppConfig) -> Vec<PluginEntry> {
        let mut out = Vec::new();
        for p in &self.targets {
            let meta = p.meta();
            let path = self.external_paths.get(&meta.id).cloned();
            // 并发回传：能力由插件声明，并发度由用户**按插件**配置（缺省沿用声明）
            let supports_plan = p.supports_plan();
            let parallel = cfg.plugins.target_parallel.get(&meta.id).copied();
            let disabled = self.is_disabled(&meta.id);
            out.push(PluginEntry {
                api_base: format!("/api/p/{}", meta.id),
                source: if path.is_some() { "external" } else { "builtin" }.to_string(),
                path,
                meta,
                // 被禁用 → 恒为不可用，前端据此置灰并显示「已禁用」
                available: !disabled,
                disabled,
                ui: p.ui(),
                supports_plan,
                parallel,
            });
        }
        for p in &self.enhances {
            let meta = p.meta();
            let path = self.external_paths.get(&meta.id).cloned();
            let disabled = self.is_disabled(&meta.id);
            out.push(PluginEntry {
                api_base: format!("/api/p/{}", meta.id),
                source: if path.is_some() { "external" } else { "builtin" }.to_string(),
                path,
                available: !disabled && p.available(cfg),
                disabled,
                ui: p.ui(),
                meta,
                supports_plan: false,
                parallel: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 按插件禁用应把插件从「活动集合」中摘除，但**保留在清单里**以便重新启用
    ///
    /// 这是运行时启停的核心语义：查询接口过滤、`describe` 保留并标注。
    #[test]
    fn disable_filters_queries_but_keeps_entry_in_list() {
        let reg = PluginRegistry::builtin();
        let cfg = AppConfig::default();

        // 内置 webdav（目标）与 kzwr（增强）默认都可用
        assert!(reg.target_plugin("webdav").is_some());
        assert!(reg.enhance_plugin_enabled("kzwr").is_some());
        assert_eq!(reg.target_plugins().len(), 1);
        assert_eq!(reg.enhance_plugins().len(), 1);

        // 禁用两者
        let n = reg.set_disabled(&["webdav".into(), "kzwr".into()]);
        assert_eq!(n, 2);

        // 查询接口：视为不存在
        assert!(reg.target_plugin("webdav").is_none(), "被禁用的目标插件不应可取到");
        assert!(
            reg.enhance_plugin_enabled("kzwr").is_none(),
            "被禁用的增强插件不应参与路由分发"
        );
        assert!(reg.target_plugins().is_empty());
        assert!(reg.enhance_plugins().is_empty(), "生命周期钩子不应再被调用");

        // 管理接口：仍可取到（否则无法重新启用 / 诊断）
        assert!(reg.target_plugin_any("webdav").is_some());
        assert!(reg.enhance_plugin_any("kzwr").is_some());

        // 清单：仍在，且标注 disabled
        let list = reg.describe(&cfg);
        assert_eq!(list.len(), 2, "被禁用的插件仍要出现在清单里");
        for e in &list {
            assert!(e.disabled, "{} 应标注 disabled", e.meta.id);
            assert!(!e.available, "{} 被禁用时 available 应为 false", e.meta.id);
        }

        // 孤立数据检测不应把被禁用插件的数据当成遗留：plugin_ids 含禁用者
        let ids = reg.plugin_ids();
        assert!(ids.contains(&"webdav".to_string()));
        assert!(ids.contains(&"kzwr".to_string()));

        // 重新启用 → 立即恢复
        reg.set_disabled(&[]);
        assert!(reg.target_plugin("webdav").is_some());
        assert!(reg.enhance_plugin_enabled("kzwr").is_some());
        assert!(reg.describe(&cfg).iter().all(|e| !e.disabled));
    }

    /// 空串与空白项应被忽略（前端传空行不应误禁用某插件）
    #[test]
    fn set_disabled_ignores_blank_ids() {
        let reg = PluginRegistry::builtin();
        let n = reg.set_disabled(&["".into(), "   ".into(), "kzwr".into()]);
        assert_eq!(n, 1);
        assert_eq!(reg.disabled_ids(), vec!["kzwr".to_string()]);
    }
}
