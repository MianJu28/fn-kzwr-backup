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

/// **外置插件快照**：可整体热替换的一组已加载插件
///
/// 之所以要整组替换（而不是逐个增删）：动态库句柄 `lib` 必须与其插件对象**同生共死** ——
/// 句柄 drop 后，插件 vtable 指向的内存会失效。整组替换能保证
/// 「旧的一组先全部托管、再由新的一组接替」，不会出现半新半旧的中间态。
struct ExternalSet {
    targets: Vec<Arc<dyn TargetPlugin>>,
    enhances: Vec<Arc<dyn EnhancePlugin>>,
    /// 动态库句柄：与上面两组插件**配套**保活（Drop 会让 vtable 悬空）
    libs: Vec<libloading::Library>,
    /// 加载报告（供 `/api/plugins` 诊断）
    reports: Vec<ExternalPluginReport>,
    /// 外置插件 id → 动态库路径（标注来源）
    paths: HashMap<String, String>,
    /// 扫描过的插件目录（(路径, 来源说明)）
    dirs: Vec<(String, String)>,
}

pub struct PluginRegistry {
    /// 内置插件（编译期固定，不可增删）
    builtin_targets: Vec<Arc<dyn TargetPlugin>>,
    builtin_enhances: Vec<Arc<dyn EnhancePlugin>>,
    /// **外置插件**（可热替换：`RwLock<Option<ExternalSet>>`）
    ///
    /// 用 `Option` 表达「尚未加载/已关闭」；用 `RwLock` 是因为注册表被
    /// `Arc<PluginRegistry>` 共享，而热加载接口只有 `&self`。
    external: std::sync::RwLock<Option<ExternalSet>>,
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
    ///
    /// 增强类插件**一律外置**（走稳定 C ABI 的 `.so`，见 `plugins/kzwr`）：
    /// 核心不掺任何厂商专属逻辑，kzwr 的凭据、动作、告警全在插件自己那侧。
    pub fn builtin() -> Self {
        Self {
            builtin_targets: vec![Arc::new(builtin::webdav_abi::WebdavAbiPlugin::new())],
            builtin_enhances: Vec::new(),
            external: std::sync::RwLock::new(None),
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
    /// `plugin_pubkeys` 为「文件名 → 公钥」映射（一插件一公钥）；
    /// `legacy_pubkeys` 是旧的扁平列表，仅对未登记文件回退使用。
    /// 失败只记录诊断，不影响内置能力与其它插件。
    pub fn load_external(
        &self,
        dirs: &[(PathBuf, String)],
        plugin_pubkeys: &std::collections::BTreeMap<String, String>,
        legacy_pubkeys: &[String],
    ) {
        let dirs_only: Vec<PathBuf> = dirs.iter().map(|(p, _)| p.clone()).collect();
        let outcome = super::loader::load_external(&dirs_only, plugin_pubkeys, legacy_pubkeys);
        let mut paths = HashMap::new();
        for r in &outcome.reports {
            if let (Some(id), true) = (r.id.clone(), r.loaded) {
                paths.insert(id, r.path.clone());
            }
        }
        let set = ExternalSet {
            targets: outcome.targets,
            enhances: outcome.enhances,
            libs: outcome.libs,
            reports: outcome.reports,
            paths,
            dirs: dirs
                .iter()
                .map(|(p, s)| (p.to_string_lossy().into_owned(), s.clone()))
                .collect(),
        };
        // 整组替换：旧的 ExternalSet（含其动态库句柄）在离开作用域时才 drop，
        // 此时已没有任何活动引用指向它 —— 见结构体注释。
        *self.external.write().unwrap() = Some(set);
    }

    /// **关闭**外置插件（丢弃整组外置插件与其动态库句柄）
    ///
    /// 等价于 `plugins.enabled=false` 的热生效版本。注意：已派发出去的
    /// `Arc` 仍会存活到其使用者结束（Rust 的引用计数保证内存安全）。
    pub fn unload_external(&self) {
        *self.external.write().unwrap() = None;
    }

    /// 外置插件是否已加载（对应「外置插件加载」开关的运行时状态）
    pub fn external_loaded(&self) -> bool {
        self.external.read().unwrap().is_some()
    }

    /// 外置插件加载报告（诊断；未加载则为空）
    pub fn external_reports(&self) -> Vec<ExternalPluginReport> {
        self.external
            .read()
            .unwrap()
            .as_ref()
            .map(|s| s.reports.clone())
            .unwrap_or_default()
    }

    /// 扫描过的插件目录（(路径, 来源说明)；未加载则为空）
    pub fn plugin_dirs(&self) -> Vec<(String, String)> {
        self.external
            .read()
            .unwrap()
            .as_ref()
            .map(|s| s.dirs.clone())
            .unwrap_or_default()
    }

    /// 按插件 id（目标配置里的 `kind`）取目标插件
    pub fn target_plugin(&self, kind: &str) -> Option<Arc<dyn TargetPlugin>> {
        if self.is_disabled(kind) {
            return None;
        }
        self.builtin_targets
            .iter()
            .find(|p| p.meta().id == kind)
            .cloned()
            .or_else(|| {
                self.external
                    .read()
                    .unwrap()
                    .as_ref()
                    .and_then(|s| s.targets.iter().find(|p| p.meta().id == kind).cloned())
            })
    }

    /// 全部**启用中**的目标插件（被禁用者已过滤）
    pub fn target_plugins(&self) -> Vec<Arc<dyn TargetPlugin>> {
        let ext = self.external.read().unwrap();
        self.builtin_targets
            .iter()
            .chain(ext.iter().flat_map(|s| s.targets.iter()))
            .filter(|p| !self.is_disabled(&p.meta().id))
            .cloned()
            .collect()
    }

    /// 全部**启用中**的增强插件（被禁用者已过滤）
    ///
    /// 生命周期钩子（`on_startup`/`patrol`/`after_backup`）与路由挂载都走这里，
    /// 因此禁用后插件立即不再被调用。
    pub fn enhance_plugins(&self) -> Vec<Arc<dyn EnhancePlugin>> {
        let ext = self.external.read().unwrap();
        self.builtin_enhances
            .iter()
            .chain(ext.iter().flat_map(|s| s.enhances.iter()))
            .filter(|p| !self.is_disabled(&p.meta().id))
            .cloned()
            .collect()
    }

    /// 全部**已加载**的增强插件（**含被禁用者**）
    ///
    /// 用于 `host_bind` 下发：能力表与"是否被调用"无关 —— 禁用只做逻辑摘除，
    /// 插件对象仍存活；绑定让插件在重新启用后立刻可用，且禁用时用 `revoke` 收口。
    pub fn all_enhance_plugins(&self) -> Vec<Arc<dyn EnhancePlugin>> {
        let ext = self.external.read().unwrap();
        self.builtin_enhances
            .iter()
            .chain(ext.iter().flat_map(|s| s.enhances.iter()))
            .cloned()
            .collect()
    }

    /// 向所有已加载的增强插件下发宿主能力表，返回接受者数量
    ///
    /// **必须在 `AppState` 建好之后调用**（能力表实现依赖 `AppState`）。
    /// 跨 FFI 前不持任何锁：`bind_host` 内部自行取/放配置锁。
    pub fn bind_all_host(&self, state: &crate::AppState) -> usize {
        self.all_enhance_plugins()
            .iter()
            .filter(|p| p.bind_host(state))
            .count()
    }

    /// 按 id 取**启用中**的增强插件（路由分发用；被禁用 → `None`）
    pub fn enhance_plugin_enabled(&self, id: &str) -> Option<Arc<dyn EnhancePlugin>> {
        if self.is_disabled(id) {
            return None;
        }
        self.builtin_enhances
            .iter()
            .find(|p| p.meta().id == id)
            .cloned()
            .or_else(|| {
                self.external
                    .read()
                    .unwrap()
                    .as_ref()
                    .and_then(|s| s.enhances.iter().find(|p| p.meta().id == id).cloned())
            })
    }

    /// 按 id 取增强插件（**含被禁用者**；诊断/卸载等管理操作用）
    pub fn enhance_plugin_any(&self, id: &str) -> Option<Arc<dyn EnhancePlugin>> {
        self.builtin_enhances
            .iter()
            .find(|p| p.meta().id == id)
            .cloned()
            .or_else(|| {
                self.external
                    .read()
                    .unwrap()
                    .as_ref()
                    .and_then(|s| s.enhances.iter().find(|p| p.meta().id == id).cloned())
            })
    }

    /// 按 id 取目标插件（**含被禁用者**；诊断与管理操作用）
    pub fn target_plugin_any(&self, id: &str) -> Option<Arc<dyn TargetPlugin>> {
        self.builtin_targets
            .iter()
            .find(|p| p.meta().id == id)
            .cloned()
            .or_else(|| {
                self.external
                    .read()
                    .unwrap()
                    .as_ref()
                    .and_then(|s| s.targets.iter().find(|p| p.meta().id == id).cloned())
            })
    }

    /// 全部已加载插件 id（目标 + 增强；含内置与外部）
    ///
    /// **不过滤** disabled：被禁用的插件其 `plugin_data` 仍应被视为「有归属」，
    /// 不该被孤立数据检测提示成遗留配置。
    pub fn plugin_ids(&self) -> Vec<String> {
        let ext = self.external.read().unwrap();
        let mut ids: Vec<String> = self
            .builtin_targets
            .iter()
            .chain(ext.iter().flat_map(|s| s.targets.iter()))
            .map(|p| p.meta().id.clone())
            .chain(
                self.builtin_enhances
                    .iter()
                    .chain(ext.iter().flat_map(|s| s.enhances.iter()))
                    .map(|p| p.meta().id.clone()),
            )
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// 调某插件的 `destroy` 钩子（卸载清除时用；找不到则无操作）
    pub fn call_destroy(&self, id: &str) {
        if let Some(p) = self.enhance_plugin_any(id) {
            p.destroy();
        }
    }

    /// 插件清单（供 `/api/plugins`；**前端唯一的区块数据来源**）
    ///
    /// **包含被禁用的插件**（`disabled: true`）：插件页需要列出它们以便重新启用。
    /// 其它消费方（目标装配、路由分发、生命周期钩子）走的是过滤后的接口。
    pub fn describe(&self, cfg: &AppConfig, mgr: &ConfigManager) -> Vec<PluginEntry> {
        let ext = self.external.read().unwrap();
        let ext_paths = ext.as_ref().map(|s| &s.paths);
        let mut out = Vec::new();
        for p in self
            .builtin_targets
            .iter()
            .chain(ext.iter().flat_map(|s| s.targets.iter()))
        {
            let meta = p.meta();
            let path = ext_paths.and_then(|m| m.get(&meta.id)).cloned();
            // 并发回传：能力由插件声明；并发度**按目标**存（这里是插件级旧值，仅作回退展示）
            let supports_plan = p.supports_plan();
            let parallel = cfg.plugins.target_parallel.get(&meta.id).copied();
            let disabled = self.is_disabled(&meta.id);
            // 表单形态由插件声明：有些目标不用凭据（如本地目录），地址字段的语义也不同。
            // 前端据此渲染「新建目标」表单，否则这类目标根本建不出来。
            let needs_credentials = p.needs_credentials();
            let (url_label, url_placeholder, url_hint) =
                (p.url_label(), p.url_placeholder(), p.url_hint());
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
                needs_credentials,
                url_label,
                url_placeholder,
                url_hint,
            });
        }
        for p in self
            .builtin_enhances
            .iter()
            .chain(ext.iter().flat_map(|s| s.enhances.iter()))
        {
            let meta = p.meta();
            let path = ext_paths.and_then(|m| m.get(&meta.id)).cloned();
            let disabled = self.is_disabled(&meta.id);
            out.push(PluginEntry {
                api_base: format!("/api/p/{}", meta.id),
                source: if path.is_some() { "external" } else { "builtin" }.to_string(),
                path,
                available: !disabled && p.available(cfg, mgr),
                disabled,
                ui: p.ui(),
                meta,
                supports_plan: false,
                parallel: None,
                // 增强插件不是备份目标：这些表单字段无意义（前端只对 kind=target 使用）
                needs_credentials: true,
                url_label: None,
                url_placeholder: None,
                url_hint: None,
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

    /// 临时目录的 `ConfigManager`（口令任意，测试内自洽）
    fn mgr() -> (tempfile::TempDir, ConfigManager) {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = ConfigManager::new(
            dir.path(),
            age::secrecy::SecretString::from("test-pass".to_string()),
        );
        (dir, m)
    }

    /// 按插件禁用应把插件从「活动集合」中摘除，但**保留在清单里**以便重新启用
    ///
    /// 这是运行时启停的核心语义：查询接口过滤、`describe` 保留并标注。
    /// 增强插件已全部外置（`.so`），故这里只用内置 webdav 目标验证启停语义。
    #[test]
    fn disable_filters_queries_but_keeps_entry_in_list() {
        let (_dir, mgr) = mgr();
        let reg = PluginRegistry::builtin();
        let cfg = AppConfig::default();

        // 内置 webdav（目标）默认可用；增强插件不再由核心内置
        assert!(reg.target_plugin("webdav").is_some());
        assert_eq!(reg.target_plugins().len(), 1);
        assert!(
            reg.enhance_plugins().is_empty(),
            "核心不应内置任何增强插件（kzwr 已外置为 .so）"
        );

        // 禁用
        let n = reg.set_disabled(&["webdav".into()]);
        assert_eq!(n, 1);

        // 查询接口：视为不存在
        assert!(reg.target_plugin("webdav").is_none(), "被禁用的目标插件不应可取到");
        assert!(reg.target_plugins().is_empty());

        // 管理接口：仍可取到（否则无法重新启用 / 诊断）
        assert!(reg.target_plugin_any("webdav").is_some());

        // 清单：仍在，且标注 disabled
        let list = reg.describe(&cfg, &mgr);
        assert_eq!(list.len(), 1, "被禁用的插件仍要出现在清单里");
        for e in &list {
            assert!(e.disabled, "{} 应标注 disabled", e.meta.id);
        }

        // 孤立数据检测不应把被禁用插件的数据当成遗留：plugin_ids 含禁用者
        assert!(reg.plugin_ids().contains(&"webdav".to_string()));

        // 重新启用 → 立即恢复
        reg.set_disabled(&[]);
        assert!(reg.target_plugin("webdav").is_some());
        assert!(reg.describe(&cfg, &mgr).iter().all(|e| !e.disabled));
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
