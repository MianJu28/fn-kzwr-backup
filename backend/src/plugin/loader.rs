//! 外置插件加载器（ADR-013 方案 B：动态库 `*.so`）
//!
//! 目录优先级（先命中先加载；同名文件以先出现的目录为准）：
//! 1. 环境变量 `FN_KZWR_PLUGIN_DIR`（`:` 分隔，支持多个；便于开发与临时覆盖）
//! 2. 配置项 `plugins.dir`（设置页「插件目录」，`:` 分隔多个）
//! 3. `$TRIM_PKGETC/plugins`（用户放置目录，持久且应用用户可写）
//! 4. `$TRIM_APPDEST/plugins`（随 `.fpk` 分发的内置插件目录）
//!
//! 安全与隔离：
//! - **默认不加载**（配置 `plugins.enabled = true` 或环境变量 `FN_KZWR_PLUGINS=1` 才启用）；
//!   加载动态库等价于执行任意本地代码，因此必须由用户显式开启
//! - 单个插件失败（ABI/版本不匹配、符号缺失、创建返回空）**只影响它自己**：
//!   记录到诊断列表（`/api/plugins` 的 `external.reports`），宿主照常启动
//! - 动态库句柄必须**保活到进程结束**（卸载会让已注册的 vtable 悬空），因此由注册表持有

use std::ffi::CStr;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::sdk::{PluginHandle, PLUGIN_ABI_VERSION, SYM_ABI_VERSION, SYM_CREATE, SYM_HOST_VERSION};

/// 单个动态库的加载结果（供 `/api/plugins` 诊断与前端展示）
#[derive(Debug, Clone, Serialize)]
pub struct ExternalPluginReport {
    /// 文件名（如 `libfn_kzwr_plugin_example.so`）
    pub file: String,
    /// 绝对路径
    pub path: String,
    /// 是否加载成功
    pub loaded: bool,
    /// 成功时的插件 id
    pub id: Option<String>,
    /// 成功时的插件名
    pub name: Option<String>,
    /// `target` | `enhance`
    pub kind: Option<String>,
    /// 失败原因（`loaded=false` 时）
    pub error: Option<String>,
}

/// 插件目录列表（去重、仅保留存在的目录），并附带来源说明
///
/// `configured` 为配置项 `plugins.dir`（`:` 分隔多个，可空）。
pub fn plugin_dirs(configured: Option<&str>) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    let mut push = |p: PathBuf, src: &str| {
        if p.as_os_str().is_empty() || !p.is_dir() {
            return;
        }
        if out.iter().any(|(x, _)| *x == p) {
            return;
        }
        out.push((p, src.to_string()));
    };

    if let Ok(v) = std::env::var("FN_KZWR_PLUGIN_DIR") {
        for part in v.split(':').filter(|s| !s.trim().is_empty()) {
            push(PathBuf::from(part.trim()), "环境变量 FN_KZWR_PLUGIN_DIR");
        }
    }
    if let Some(v) = configured {
        for part in v.split(':').filter(|s| !s.trim().is_empty()) {
            push(PathBuf::from(part.trim()), "设置页「插件目录」");
        }
    }
    if let Ok(etc) = std::env::var("TRIM_PKGETC") {
        push(Path::new(&etc).join("plugins"), "配置目录 $TRIM_PKGETC/plugins");
    }
    if let Ok(dest) = std::env::var("TRIM_APPDEST") {
        push(Path::new(&dest).join("plugins"), "应用目录 $TRIM_APPDEST/plugins");
    }
    out
}

/// 是否启用外置插件加载
///
/// `FN_KZWR_PLUGINS=1`（或 `true`）可强制开启（开发/调试用），否则看配置项 `plugins.enabled`。
pub fn enabled_by_env() -> Option<bool> {
    match std::env::var("FN_KZWR_PLUGINS").ok()?.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// 扫描目录并按文件名排序返回 `*.so`
fn scan(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .map(|e| e.eq_ignore_ascii_case("so"))
                    .unwrap_or(false)
        })
        .collect();
    files.sort();
    files
}

/// 加载结果：装载好的插件 + 需保活的动态库 + 诊断报告
pub struct LoadOutcome {
    pub targets: Vec<std::sync::Arc<dyn super::api::TargetPlugin>>,
    pub enhances: Vec<std::sync::Arc<dyn super::api::EnhancePlugin>>,
    /// 必须保活（Drop 会让已注册的 vtable 悬空）
    pub libs: Vec<libloading::Library>,
    pub reports: Vec<ExternalPluginReport>,
    /// 出现过的插件 id（供注册表标记来源）
    pub ids: Vec<String>,
}

/// 加载全部目录中的外置插件
///
/// `allow_version_mismatch`：允许「插件编译期宿主版本 ≠ 运行版本」（仅建议开发时用，
/// 默认从环境变量 `FN_KZWR_PLUGINS_ALLOW_MISMATCH=1` 读取）。
pub fn load_external(dirs: &[PathBuf]) -> LoadOutcome {
    let allow_mismatch = std::env::var("FN_KZWR_PLUGINS_ALLOW_MISMATCH")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(false);

    let mut out = LoadOutcome {
        targets: Vec::new(),
        enhances: Vec::new(),
        libs: Vec::new(),
        reports: Vec::new(),
        ids: Vec::new(),
    };

    let mut seen: Vec<String> = Vec::new();
    for dir in dirs {
        for path in scan(dir) {
            let file = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            // 同名的先出现目录优先（用户目录覆盖随包分发的同名插件）
            if seen.iter().any(|f| *f == file) {
                continue;
            }
            seen.push(file.clone());
            let mut report = ExternalPluginReport {
                file: file.clone(),
                path: path.to_string_lossy().into_owned(),
                loaded: false,
                id: None,
                name: None,
                kind: None,
                error: None,
            };
            match load_one(&path, allow_mismatch) {
                Ok((handle, lib, id, name, kind)) => {
                    if let Some(t) = handle.target {
                        out.targets.push(std::sync::Arc::from(t));
                    }
                    if let Some(e) = handle.enhance {
                        out.enhances.push(std::sync::Arc::from(e));
                    }
                    out.libs.push(lib);
                    out.ids.push(id.clone());
                    report.loaded = true;
                    report.id = Some(id);
                    report.name = Some(name);
                    report.kind = Some(kind);
                    tracing::info!(file = %file, plugin = %report.id.clone().unwrap_or_default(), "外置插件已加载");
                }
                Err(e) => {
                    tracing::warn!(file = %file, err = %e, "外置插件加载失败（已跳过，不影响其它插件）");
                    report.error = Some(e);
                }
            }
            out.reports.push(report);
        }
    }
    out
}

/// 加载单个动态库：校验 ABI/宿主版本 → 创建实例
fn load_one(
    path: &Path,
    allow_mismatch: bool,
) -> Result<
    (
        PluginHandle,
        libloading::Library,
        String,
        String,
        String,
    ),
    String,
> {
    // SAFETY: 以下均是对**用户显式启用**的本地动态库的调用；符号签名由 ABI 常量约定，
    // 并在调用前完成 ABI/版本校验。加载不可信代码的风险由用户开启开关时承担（见模块文档）。
    unsafe {
        let lib = libloading::Library::new(path)
            .map_err(|e| format!("打开动态库失败：{e}"))?;

        let abi = lib
            .get::<extern "C" fn() -> u32>(SYM_ABI_VERSION)
            .map_err(|e| format!("缺少符号 fn_kzwr_plugin_abi_version：{e}"))?;
        let abi = abi();
        if abi != PLUGIN_ABI_VERSION {
            return Err(format!(
                "ABI 版本不匹配（插件 {abi}，宿主 {PLUGIN_ABI_VERSION}）：请用当前版本源码重新编译插件"
            ));
        }

        let host_version = lib
            .get::<extern "C" fn() -> *const std::os::raw::c_char>(SYM_HOST_VERSION)
            .map_err(|e| format!("缺少符号 fn_kzwr_plugin_host_version：{e}"))?;
        let raw = host_version();
        if raw.is_null() {
            return Err("fn_kzwr_plugin_host_version 返回了空指针".to_string());
        }
        let built_for = CStr::from_ptr(raw).to_string_lossy().into_owned();
        let running = super::sdk::HOST_VERSION;
        if built_for != running && !allow_mismatch {
            return Err(format!(
                "插件编译时链接的宿主版本为 {built_for}，当前运行版本为 {running}：\
                 Rust ABI 不稳定，请重新编译插件（或设 FN_KZWR_PLUGINS_ALLOW_MISMATCH=1 强制加载）"
            ));
        }
        if built_for != running {
            tracing::warn!(
                built_for = %built_for,
                running = %running,
                "外置插件版本与宿主不一致，已按 FN_KZWR_PLUGINS_ALLOW_MISMATCH 强制加载"
            );
        }

        let create = lib
            .get::<extern "C" fn() -> *mut PluginHandle>(SYM_CREATE)
            .map_err(|e| format!("缺少符号 fn_kzwr_plugin_create：{e}"))?;
        let raw_handle = create();
        if raw_handle.is_null() {
            return Err("fn_kzwr_plugin_create 返回了空指针".to_string());
        }
        let handle = Box::from_raw(raw_handle);
        if handle.is_empty() {
            return Err("插件未提供任何实现（target/enhance 均为空）".to_string());
        }

        // 元信息（取自插件自己声明的 meta；目标插件与增强插件二选一即可）
        let (id, name, kind) = if let Some(t) = &handle.target {
            let m = t.meta();
            (m.id, m.name, "target".to_string())
        } else if let Some(e) = &handle.enhance {
            let m = e.meta();
            (m.id, m.name, "enhance".to_string())
        } else {
            return Err("插件未提供任何实现".to_string());
        };

        Ok((*handle, lib, id, name, kind))
    }
}
