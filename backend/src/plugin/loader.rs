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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use super::abi::{KzwrPluginAbi, KzwrTargetAbi, C_ABI_VERSION, SYM_ENTRY_V1};
use super::api::{EnhancePlugin, TargetPlugin};

/// 单个动态库加载后的插件形态
///
/// 稳定 C ABI 是**唯一**加载机制（已删 Rust 直连：产品未发布、无兼容包袱）。
/// 一个 `.so` 可以同时提供增强能力与目标能力：
/// - `enhance`：`fn_kzwr_plugin_abi_v1` 导出的主表（kind 为 `enhance`/缺省时）
/// - `target`：`fn_kzwr_plugin_target_v1`（或 describe.runtime.target）导出的目标能力表
#[derive(Default)]
pub struct Loaded {
    /// 增强能力（可选：kind 为 `target` 的纯目标插件为空）
    pub enhance: Option<std::sync::Arc<dyn EnhancePlugin>>,
    /// 目标能力（可选：导出了目标能力表才有）
    pub target: Option<std::sync::Arc<dyn TargetPlugin>>,
    pub id: String,
    pub name: String,
    /// 插件声明的 ABI 版本（本次加载强制等于 C_ABI_VERSION）
    pub abi: u32,
}

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
    /// 加载机制：稳定 C ABI 是唯一机制，恒为 `c-abi-v1`（保留字段便于前端迁移）
    pub mechanism: Option<String>,
    /// 插件声明的 ABI 版本（仅稳定 C ABI 有）
    pub abi: Option<u32>,
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

/// 校验单个插件的 Ed25519 签名（ADR-013 安全防线）——返回 `Ok(())` 或拒绝原因。
///
/// - `pubkeys` 非空才强制校验：`<so> 同目录下必须有 <so>.sig`
///   （`.so` 原始字节的 Ed25519 签名，64 字节），且用任一公钥验签通过；
/// - `pubkeys` 为空 = 信任已受控目录，跳过硬校验（仍会尝试解析 `.sig`，失败不阻断）；
/// - 验签失败/缺 `.sig` → `Err`，上层记入诊断并跳过该插件。
fn verify_plugin(path: &Path, pubkeys: &[String]) -> Result<(), String> {
    use ring::signature::{UnparsedPublicKey, ED25519};

    // 无配置公钥 → 不强制校验（默认行为，向后兼容）
    let mut keys: Vec<UnparsedPublicKey<Vec<u8>>> = Vec::new();
    for b64 in pubkeys {
        let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
            .map_err(|_| format!("插件公钥不是合法 base64：{b64}"))?;
        if raw.len() != 32 {
            return Err(format!("插件公钥必须是 32 字节 Ed25519 公钥，实际 {} 字节", raw.len()));
        }
        keys.push(UnparsedPublicKey::new(&ED25519, raw));
    }
    if keys.is_empty() {
        return Ok(());
    }

    // 同目录、同 basename + `.sig`
    let mut sig_path = path.as_os_str().to_os_string();
    sig_path.push(".sig");
    let sig_path = PathBuf::from(sig_path);
    if !sig_path.is_file() {
        return Err(format!("启用了签名校验，但缺少签名文件 {}", sig_path.display()));
    }
    let data = std::fs::read(path)
        .map_err(|e| format!("读取插件 {} 失败：{e}", path.display()))?;
    let sig = std::fs::read(&sig_path)
        .map_err(|e| format!("读取签名 {} 失败：{e}", sig_path.display()))?;

    for k in &keys {
        if k.verify(&data, &sig).is_ok() {
            tracing::debug!(plugin = %path.display(), "插件签名校验通过");
            return Ok(());
        }
    }
    Err("插件签名校验失败：签名与任一配置公钥均不匹配".to_string())
}

/// 加载全部目录中的外置插件
pub fn load_external(dirs: &[PathBuf], pubkeys: &[String]) -> LoadOutcome {
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
                mechanism: None,
                abi: None,
                error: None,
            };
            // 签名防线：配置了公钥则强制验签，失败即拒绝加载（不触碰动态库）
            if let Err(e) = verify_plugin(&path, pubkeys) {
                tracing::warn!(file = %file, err = %e, "外置插件签名校验未通过（已跳过）");
                report.error = Some(format!("签名校验未通过：{e}"));
                out.reports.push(report);
                continue;
            }
            match load_one(&path) {
                Ok((loaded, lib)) => {
                    let kind = match (loaded.enhance.is_some(), loaded.target.is_some()) {
                        (true, true) => "both",
                        (true, false) => "enhance",
                        (false, true) => "target",
                        (false, false) => {
                            report.error = Some("插件未提供任何能力（enhance 或 target）".to_string());
                            out.reports.push(report);
                            continue;
                        }
                    };
                    if let Some(e) = loaded.enhance {
                        out.enhances.push(e);
                    }
                    if let Some(t) = loaded.target {
                        out.targets.push(t);
                    }
                    out.libs.push(lib);
                    out.ids.push(loaded.id.clone());
                    report.loaded = true;
                    report.id = Some(loaded.id.clone());
                    report.name = Some(loaded.name.clone());
                    report.kind = Some(kind.to_string());
                    report.mechanism = Some("c-abi-v1".to_string());
                    report.abi = Some(loaded.abi);
                    tracing::info!(file = %file, plugin = %loaded.id, "外置插件已按稳定 C ABI v1 加载");
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

/// 加载单个动态库：优先稳定 C ABI v1，回退 Rust 直连
///
/// 返回 (插件形态, 动态库句柄)；动态库句柄需由调用方保活。
fn load_one(path: &Path) -> Result<(Loaded, libloading::Library), String> {
    unsafe {
        let lib = libloading::Library::new(path)
            .map_err(|e| format!("打开动态库失败：{e}"))?;

        let entry = lib
            .get::<extern "C" fn() -> *const KzwrPluginAbi>(SYM_ENTRY_V1)
            .map_err(|e| format!("缺少稳定入口 fn_kzwr_plugin_abi_v1：{e}"))?;
        let table_ptr = entry();
        let table_ref = super::cabi::validate_table(table_ptr)?;
        let describe = super::cabi::describe_of(table_ref)?;
        let origin = path.to_string_lossy().into_owned();

        // 增强能力：kind 为 `target` 的纯目标插件不建 enhance 适配器
        let enhance: Option<Arc<dyn super::api::EnhancePlugin>> =
            if matches!(describe.kind.as_deref(), None | Some("enhance")) {
                Some(Arc::new(super::cabi::CApiEnhance::adopt(table_ptr, origin.clone())?)
                    as Arc<dyn super::api::EnhancePlugin>)
            } else {
                None
            };

        // 目标能力：describe_json.runtime.target 声明的符号（缺省 fn_kzwr_plugin_target_v1）
        let target: Option<Arc<dyn super::api::TargetPlugin>> = {
            let sym = describe.runtime.target_symbol();
            match lib.get::<extern "C" fn() -> *const KzwrTargetAbi>(sym.as_bytes()) {
                Ok(f) => {
                    let t = f();
                    if t.is_null() {
                        None
                    } else {
                        Some(Arc::new(super::target_abi::CApiTarget::adopt(
                            t,
                            describe.clone(),
                            &origin,
                        )?) as Arc<dyn super::api::TargetPlugin>)
                    }
                }
                Err(_) => None,
            }
        };

        if enhance.is_none() && target.is_none() {
            return Err("插件未提供任何能力（enhance 或 target）".to_string());
        }

        Ok((
            Loaded {
                enhance,
                target,
                id: describe.id.clone(),
                name: describe.name.clone(),
                abi: C_ABI_VERSION,
            },
            lib,
        ))
    }
}

/// 生成测试用 Ed25519 密钥对，返回 (base64 公钥, 私钥)
#[cfg(test)]
fn test_key() -> (String, ring::signature::Ed25519KeyPair) {
    use ring::signature::KeyPair as _;
    use base64::Engine as _;
    let rng = ring::rand::SystemRandom::new();
    let doc = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("generate");
    let kp = ring::signature::Ed25519KeyPair::from_pkcs8(doc.as_ref()).expect("from_pkcs8");
    let pubk = base64::engine::general_purpose::STANDARD.encode(kp.public_key().as_ref());
    (pubk, kp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_signature_accepts_valid_and_rejects_tampered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (pubk, kp) = test_key();
        let keys = vec![pubk];

        let so = dir.path().join("libfkplug.so");
        let data = b"fake plugin bytes v1".to_vec();
        std::fs::write(&so, &data).unwrap();

        // 正确签名 → 通过
        let sig = kp.sign(&data);
        std::fs::write(dir.path().join("libfkplug.so.sig"), sig.as_ref()).unwrap();
        assert!(verify_plugin(&so, &keys).is_ok());

        // 篡改插件内容 → 拒绝（签名不再匹配）
        let data2 = b"fake plugin bytes v2".to_vec();
        std::fs::write(&so, &data2).unwrap();
        assert!(verify_plugin(&so, &keys).is_err());

        // 缺签名文件 → 拒绝
        std::fs::remove_file(dir.path().join("libfkplug.so.sig")).unwrap();
        assert!(verify_plugin(&so, &keys).is_err());
    }

    #[test]
    fn verify_skips_when_no_pubkeys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let so = dir.path().join("x.so");
        std::fs::write(&so, b"whatever").unwrap();
        // 未配置公钥 → 不强制校验
        let keys: Vec<String> = Vec::new();
        assert!(verify_plugin(&so, &keys).is_ok());
    }
}
