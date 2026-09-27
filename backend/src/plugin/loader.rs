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

/// 插件签名校验状态（供 `/api/plugins` 诊断与前端徽标）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureStatus {
    /// 验签通过（配置了公钥且签名匹配）
    Verified,
    /// 未签名：已由 `FN_KZWR_PLUGINS_ALLOW_UNSIGNED` 显式放行（仅本机调试）
    Unsigned,
    /// 验签失败/缺签名/未配公钥（该插件已被拒绝加载）
    Failed,
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
    /// 签名校验状态（`verified` | `unsigned` | `failed`）
    pub signature: SignatureStatus,
    /// 是否存在同目录同名 `<so>.sig` 文件（供前端区分「未签名」与「签名不匹配」）
    pub sig_file: bool,
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

/// 内置**官方发布公钥**（编译期内置的信任锚）
///
/// 随包分发的插件（`$TRIM_APPDEST/plugins/*.so`）由发布流程用**官方私钥**签名，
/// 公钥硬编码在此，用户**无需做任何配置**即可让随包插件通过强制验签
/// （否则「默认强制验签 + 默认无公钥」会让随包示例插件一律加载失败）。
///
/// 与 `plugins.pubkeys` 的关系：**取并集**。任一来源验签通过即放行，因此
/// - 官方插件：靠此内置公钥；
/// - 用户自签插件：把自签公钥填进设置页「插件公钥」。
///
/// 安全边界：内置公钥只验证「是否由官方私钥签发」，**不等于**官方审计过插件代码；
/// 用户目录（`$TRIM_PKGETC/plugins`，应用用户可写）里的插件仍必须自带有效签名，
/// 否则一样被拒——这正是签名闸门要防的「本地落一个 .so 就被执行」。
pub const OFFICIAL_PUBKEYS: &[&str] = &[
    // 发布签名公钥（base64 的 32 字节裸 Ed25519 公钥）
    "Ao+3UdUTLFLv3TA5Oc2PRQhmzQHWG7OAZxdAToyATw8=",
];

/// 是否放行**未签名**插件（仅本机调试用）
///
/// `FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1` 时跳过全部签名校验。默认**不放行**：ADR-013
/// 决策 3 要求外置插件默认强制验签（`plugins.pubkeys` 未配置时也拒绝加载，避免
/// 「忘了配公钥 = 静默无校验」）。正式安装请勿设置该变量。
pub fn allow_unsigned_by_env() -> bool {
    matches!(
        std::env::var("FN_KZWR_PLUGINS_ALLOW_UNSIGNED")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// 插件签名文件路径：同目录、同 basename + `.sig`
pub fn sig_path_of(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_os_string();
    p.push(".sig");
    PathBuf::from(p)
}

/// 解析 base64 公钥列表为 `ring` 公钥（保持顺序，跳过空串）
fn parse_pubkeys<'a, I>(keys_b64: I) -> Result<Vec<ring::signature::UnparsedPublicKey<Vec<u8>>>, String>
where
    I: IntoIterator<Item = &'a str>,
{
    use ring::signature::{UnparsedPublicKey, ED25519};
    let mut keys = Vec::new();
    for b64 in keys_b64 {
        let b64 = b64.trim();
        if b64.is_empty() {
            continue;
        }
        let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
            .map_err(|_| format!("插件公钥不是合法 base64：{b64}"))?;
        if raw.len() != 32 {
            return Err(format!("插件公钥必须是 32 字节 Ed25519 公钥，实际 {} 字节", raw.len()));
        }
        keys.push(UnparsedPublicKey::new(&ED25519, raw));
    }
    Ok(keys)
}

/// 校验插件动态库的 Ed25519 签名（ADR-013 决策 3）——返回 `Ok(())` 或拒绝原因
///
/// - **信任锚 = 内置官方公钥 `OFFICIAL_PUBKEYS` ∪ 配置的 `plugins.pubkeys`**，
///   任一公钥验签通过即放行（内置公钥让随包插件开箱即用，无需用户配置）；
/// - 签名文件为同目录同名的 `<so>.sig`（`.so` 原始字节的 64 字节裸签名，
///   由 `Scripts/sign_plugin.sh sign` 用 openssl 生成，与 `ring` 验签格式互通）；
/// - **默认强制**：即使一个公钥都没配，签名缺失/不匹配一样拒绝加载；
///   仅当设置 `FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1`（本机调试）才跳过校验；
/// - 验签失败/缺 `.sig` → `Err`，上层记入诊断并跳过该插件。
fn verify_plugin(
    path: &Path,
    plugin_pubkeys: &std::collections::BTreeMap<String, String>,
    legacy_pubkeys: &[String],
) -> Result<(), String> {
    // 调试逃生舱：显式放行未签名插件（打醒目警告，避免误以为已受校验保护）
    if allow_unsigned_by_env() {
        tracing::warn!(
            plugin = %path.display(),
            "FN_KZWR_PLUGINS_ALLOW_UNSIGNED 已启用：跳过插件签名校验（仅限本机调试）"
        );
        return Ok(());
    }

    // 信任锚 = 内置官方公钥 ∪ **该文件自己的**公钥（一插件一公钥）。
    //
    // 关键安全性质：**只查本文件的公钥**，不再让所有插件共用一个公钥池 ——
    // 否则插件 A 的密钥可以签出能通过校验的插件 B（越权信任）。
    // 内置官方公钥仍对所有插件有效（随包插件开箱即用）。
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let own_key = plugin_pubkeys.get(&file_name);

    // 兼容旧配置的边界：**只有整个 `plugin_pubkeys` 都为空**时才回退到扁平列表
    // （即「还没迁移到一插件一公钥」的旧安装）。
    //
    // 若已经用了新模型，则**未登记的文件就是没有授权公钥** —— 绝不能回退，
    // 否则「某插件登记了公钥」会让其它未登记插件继续被这个公钥池放行，
    // 一插件一公钥的隔离性就失效了（这正是本函数要先做隔离的原因）。
    let migrated = !plugin_pubkeys.is_empty();
    let fallback: &[String] = if migrated { &[] } else { legacy_pubkeys };

    let mut keys = parse_pubkeys(OFFICIAL_PUBKEYS.iter().copied())?;
    let official_n = keys.len();
    if let Some(k) = own_key {
        keys.extend(parse_pubkeys(std::iter::once(k.as_str()))?);
    } else {
        keys.extend(parse_pubkeys(fallback.iter().map(|s| s.as_str()))?);
    }
    let configured_n = keys.len() - official_n;

    // 同目录、同 basename + `.sig`
    let sig_path = sig_path_of(path);
    if !sig_path.is_file() {
        return Err(format!(
            "启用了签名校验，但缺少签名文件 {}（自签插件请执行 Scripts/sign_plugin.sh sign）",
            sig_path.display()
        ));
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
    Err(format!(
        "插件签名校验失败：签名与内置官方公钥、以及为「{file_name}」配置的 {configured_n} 个公钥均不匹配\
         （一个插件只认它自己的公钥；请在「插件」页为该文件填写正确公钥并用同一私钥签名）"
    ))
}

/// 加载全部目录中的外置插件
pub fn load_external(
    dirs: &[PathBuf],
    plugin_pubkeys: &std::collections::BTreeMap<String, String>,
    legacy_pubkeys: &[String],
) -> LoadOutcome {
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
                signature: SignatureStatus::Failed,
                // 与验签结果无关：先如实记录是否存在 `.sig`，前端据此区分
                // 「签名不匹配」与「根本没签名」（写反会把被篡改的插件显示成未签名）
                sig_file: sig_path_of(&path).is_file(),
                error: None,
            };
            // 签名防线：默认强制验签，失败即拒绝加载（不触碰动态库）
            if let Err(e) = verify_plugin(&path, plugin_pubkeys, legacy_pubkeys) {
                tracing::warn!(file = %file, err = %e, "外置插件签名校验未通过（已跳过）");
                report.error = Some(format!("签名校验未通过：{e}"));
                out.reports.push(report);
                continue;
            }
            // 走到这里说明验签通过（或逃生舱放行）
            report.signature = if allow_unsigned_by_env() {
                SignatureStatus::Unsigned
            } else {
                SignatureStatus::Verified
            };
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

    /// 串行化会读写进程级环境变量的测试（`cargo test` 默认多线程）
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
        L.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 已配置公钥时的验签测试不受逃生舱影响（显式清掉，避免外部环境串味）
    fn no_escape_hatch() -> std::sync::MutexGuard<'static, ()> {
        let g = env_lock();
        std::env::remove_var("FN_KZWR_PLUGINS_ALLOW_UNSIGNED");
        g
    }

    /// 测试辅助：按**扁平列表**（旧语义，回退用）验签
    fn verify_legacy(path: &Path, pubkeys: &[String]) -> Result<(), String> {
        verify_plugin(path, &std::collections::BTreeMap::new(), pubkeys)
    }

    /// 测试辅助：把公钥绑到某个文件名上（一插件一公钥）
    fn map_of(key: &str) -> std::collections::BTreeMap<String, String> {
        let mut m = std::collections::BTreeMap::new();
        m.insert("libstage.so".to_string(), key.to_string());
        m
    }

    /// 测试辅助：按**一插件一公钥**登记（文件名为键）
    fn verify_for(path: &Path, key: &str) -> Result<(), String> {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut m = std::collections::BTreeMap::new();
        m.insert(name, key.to_string());
        verify_plugin(path, &m, &[])
    }

    #[test]
    fn verify_signature_accepts_valid_and_rejects_tampered() {
        let _g = no_escape_hatch();
        let dir = tempfile::tempdir().expect("tempdir");
        let (pubk, kp) = test_key();
        let keys = vec![pubk];

        let so = dir.path().join("libfkplug.so");
        let data = b"fake plugin bytes v1".to_vec();
        std::fs::write(&so, &data).unwrap();

        // 正确签名 → 通过
        let sig = kp.sign(&data);
        std::fs::write(dir.path().join("libfkplug.so.sig"), sig.as_ref()).unwrap();
        assert!(verify_legacy(&so, &keys).is_ok());

        // 篡改插件内容 → 拒绝（签名不再匹配）
        let data2 = b"fake plugin bytes v2".to_vec();
        std::fs::write(&so, &data2).unwrap();
        assert!(verify_legacy(&so, &keys).is_err());

        // 缺签名文件 → 拒绝
        std::fs::remove_file(dir.path().join("libfkplug.so.sig")).unwrap();
        assert!(verify_legacy(&so, &keys).is_err());
    }

    /// 决策 3：默认强制验签 —— 一个公钥都没配、也没签名，仍然**拒绝**（不是静默跳过）
    ///
    /// 内置官方公钥不等于「放行一切」：它只让**官方签名**有效，未签名插件照旧被拒。
    #[test]
    fn verify_rejects_unsigned_without_configured_pubkeys() {
        let _g = env_lock();
        std::env::remove_var("FN_KZWR_PLUGINS_ALLOW_UNSIGNED");
        let dir = tempfile::tempdir().expect("tempdir");
        let so = dir.path().join("x.so");
        std::fs::write(&so, b"whatever").unwrap();
        let e = verify_legacy(&so, &[]).expect_err("未签名插件必须拒绝");
        // 无 `.sig` → 在签名闸门被拒（默认拒绝，不因「没配公钥」而放开）
        assert!(e.contains("缺少签名文件"), "应提示缺签名文件：{e}");
        // 不能断言插件「未签名」之外的东西：文案不该让人去查一个不存在的公钥配置
        assert!(!e.contains("未配置插件公钥"), "内置公钥已存在，不该再报「未配置公钥」：{e}");
    }

    /// 内置官方公钥必须是**格式正确**的 32 字节 Ed25519 公钥
    ///
    /// 它是随包插件的唯一信任锚：写错一个字符 → 所有随包插件静默加载失败，
    /// 而现象只是「插件没出现」，极难定位。这里把格式钉死在测试里。
    #[test]
    fn official_pubkeys_are_wellformed() {
        use base64::Engine as _;
        assert!(!OFFICIAL_PUBKEYS.is_empty(), "必须内置至少一把官方公钥");
        for k in OFFICIAL_PUBKEYS {
            let raw = base64::engine::general_purpose::STANDARD
                .decode(k)
                .unwrap_or_else(|_| panic!("官方公钥不是合法 base64：{k}"));
            assert_eq!(raw.len(), 32, "官方公钥必须是 32 字节：{k}");
        }
        // 能被 ring 接受（构造不 panic 即说明长度/格式可用）
        assert_eq!(parse_pubkeys(OFFICIAL_PUBKEYS.iter().copied()).unwrap().len(), OFFICIAL_PUBKEYS.len());
    }

    /// 信任锚是**并集**：用户自签公钥与内置官方公钥都有效，且空串被忽略
    #[test]
    fn trust_anchor_is_union_of_official_and_configured() {
        let _g = no_escape_hatch();
        use base64::Engine as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let so = dir.path().join("libunion.so");
        // 必须与下方固定向量签名的**原始内容**逐字节一致（签名是对内容做的）
        let data = b"openssl-signed plugin payload".to_vec();
        std::fs::write(&so, &data).unwrap();

        // 用 openssl 固定向量当「用户自签」：官方公钥验不过它，用户公钥可以
        let user_pub = "YCzDjlN5uEHPulgwyGWnZYpYV3P7O1xPNpTT0zAkv+A=";
        let sig_b64 = "xP25Ugz2aLM0pR9N/ZKDnRyMuM5tpoO/1YdR4bQCFYlNdygKg6udM0MW9KwyPjT7zopp5MnFM4YwJ6+fNX5WDg==";
        std::fs::write(
            dir.path().join("libunion.so.sig"),
            base64::engine::general_purpose::STANDARD.decode(sig_b64).unwrap(),
        )
        .unwrap();

        // 只配用户公钥 → 通过（内置官方公钥不影响用户自签）
        assert!(verify_legacy(&so, &[user_pub.to_string()]).is_ok());
        // 空串/空白项被忽略，不会当成「无效公钥」而报错
        assert!(verify_legacy(&so, &["".to_string(), "   ".to_string(), user_pub.to_string()]).is_ok());
        // 既不给用户公钥、签名也不属于官方 → 拒绝
        assert!(verify_legacy(&so, &[]).is_err(), "非官方签名不得通过内置公钥");
    }

    /// **一插件一公钥**：A 的公钥不能验过 B（消除越权信任）
    ///
    /// 旧的扁平列表语义是「任一公钥可验任一插件」—— 若插件 A 的私钥泄露，
    /// 攻击者能用它签出恶意插件 B 并被接受。按文件名绑定公钥后不再成立。
    #[test]
    fn per_plugin_key_cannot_verify_other_plugin() {
        use base64::Engine as _;
        use ring::signature::KeyPair as _;
        let _g = no_escape_hatch();

        let dir = tempfile::tempdir().expect("tempdir");
        let (pub_b64, kp) = test_key();

        // 插件 A：用 kp 签名
        let so_a = dir.path().join("liba.so");
        let data_a = b"plugin A payload".to_vec();
        std::fs::write(&so_a, &data_a).unwrap();
        std::fs::write(
            dir.path().join("liba.so.sig"),
            kp.sign(&data_a).as_ref(),
        )
        .unwrap();

        // 插件 B：同一份字节，但文件名不同 → 未登记公钥
        let so_b = dir.path().join("libb.so");
        std::fs::write(&so_b, &data_a).unwrap();
        std::fs::write(dir.path().join("libb.so.sig"), kp.sign(&data_a).as_ref()).unwrap();

        // 只给 A（liba.so）登记公钥，B 完全不登记
        let mut m = std::collections::BTreeMap::new();
        m.insert("liba.so".to_string(), pub_b64.clone());

        assert!(
            verify_plugin(&so_a, &m, &[]).is_ok(),
            "A 已登记公钥，应通过"
        );
        assert!(
            verify_plugin(&so_b, &m, &[]).is_err(),
            "B 未登记公钥：不能被 A 的公钥放行（这正是「一插件一公钥」要防的越权信任）"
        );
        // 对照：旧的扁平语义下 B 会被放行（说明隔离确实由新逻辑提供）
        assert!(
            verify_legacy(&so_b, &[pub_b64.clone()]).is_ok(),
            "旧的扁平列表语义确实会放行 B —— 所以隔离是新增的保护"
        );
    }

    #[test]
    fn verify_allows_unsigned_with_env_escape_hatch() {
        // `FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1` 时放行未签名插件（本机调试）
        let _g = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let so = dir.path().join("y.so");
        std::fs::write(&so, b"whatever").unwrap();
        std::env::set_var("FN_KZWR_PLUGINS_ALLOW_UNSIGNED", "1");
        let r = verify_legacy(&so, &[]);
        std::env::remove_var("FN_KZWR_PLUGINS_ALLOW_UNSIGNED");
        assert!(r.is_ok(), "逃生舱应放行：{r:?}");
    }

    /// 与 `Scripts/sign_plugin.sh`（openssl Ed25519）的**互操作性**：
    /// 脚本产出的是 32 字节裸公钥的 base64 与 64 字节裸签名，必须能被 `ring` 验过。
    #[test]
    fn verify_accepts_openssl_ed25519_signature() {
        use base64::Engine as _;
        let _g = no_escape_hatch();
        let dir = tempfile::tempdir().expect("tempdir");
        let so = dir.path().join("libosign.so");
        let data = b"openssl-signed plugin payload".to_vec();
        std::fs::write(&so, &data).unwrap();

        // 固定向量：由 `openssl genpkey -algorithm ED25519` + `pkeyutl -sign -rawin` 生成，
        // 公钥为 `openssl pkey -pubout -outform DER` 末 32 字节的 base64（脚本同一算法）。
        let pubk = "YCzDjlN5uEHPulgwyGWnZYpYV3P7O1xPNpTT0zAkv+A=";
        let sig_b64 = "xP25Ugz2aLM0pR9N/ZKDnRyMuM5tpoO/1YdR4bQCFYlNdygKg6udM0MW9KwyPjT7zopp5MnFM4YwJ6+fNX5WDg==";
        std::fs::write(
            dir.path().join("libosign.so.sig"),
            base64::engine::general_purpose::STANDARD
                .decode(sig_b64)
                .expect("decode sig"),
        )
        .unwrap();
        assert!(verify_legacy(&so, &[pubk.to_string()]).is_ok());
    }

    /// 端到端穿过 `load_external`：验证「先验签、后 dlopen」的顺序
    ///
    /// 用假 `.so`（不是真动态库）+ 真签名，观察两种结果：
    /// - 公钥正确 → 过了签名闸门，之后才因「不是动态库」失败（错误里应出现 dlopen 相关字样）；
    /// - 不配公钥 → 在签名闸门就被拦下，**根本不会尝试 dlopen**（错误里不应有 dlopen 字样）。
    fn staged_dir() -> (tempfile::TempDir, String) {
        use base64::Engine as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let so = dir.path().join("libstage.so");
        std::fs::write(&so, b"openssl-signed plugin payload").unwrap();
        let sig_b64 = "xP25Ugz2aLM0pR9N/ZKDnRyMuM5tpoO/1YdR4bQCFYlNdygKg6udM0MW9KwyPjT7zopp5MnFM4YwJ6+fNX5WDg==";
        std::fs::write(
            dir.path().join("libstage.so.sig"),
            base64::engine::general_purpose::STANDARD.decode(sig_b64).expect("decode sig"),
        )
        .unwrap();
        (dir, "YCzDjlN5uEHPulgwyGWnZYpYV3P7O1xPNpTT0zAkv+A=".to_string())
    }

    #[test]
    fn load_external_verifies_before_dlopen() {
        let _g = no_escape_hatch();
        let (dir, pubk) = staged_dir();
        let out = load_external(&[dir.path().to_path_buf()], &map_of(&pubk), &[]);
        assert_eq!(out.reports.len(), 1);
        let r = &out.reports[0];
        assert!(!r.loaded, "假 .so 不该加载成功");
        // 签名字段应为 verified：说明它**通过了**签名闸门，是后来 dlopen 才失败的
        assert_eq!(r.signature, SignatureStatus::Verified, "应通过验签：{:?}", r.error);
        assert!(r.sig_file);
        let e = r.error.clone().unwrap_or_default();
        assert!(
            e.contains("动态库") || e.contains("dlopen") || e.contains("打开"),
            "错误应来自 dlopen 阶段：{e}"
        );
    }

    #[test]
    fn load_external_rejects_at_signature_gate() {
        let _g = no_escape_hatch();
        let (dir, _pubk) = staged_dir();
        // 只不配**用户**公钥：签名既非官方、也无用户公钥可验 → 默认强制验签，必须在**接触动态库之前**就拒绝
        let out = load_external(&[dir.path().to_path_buf()], &std::collections::BTreeMap::new(), &[]);
        assert_eq!(out.reports.len(), 1);
        let r = &out.reports[0];
        assert!(!r.loaded);
        assert_eq!(r.signature, SignatureStatus::Failed);
        let e = r.error.clone().unwrap_or_default();
        assert!(e.contains("签名校验未通过"), "应被签名闸门拦下：{e}");
        assert!(
            !e.contains("动态库") && !e.contains("dlopen"),
            "不该走到 dlopen：{e}"
        );
        assert!(out.targets.is_empty() && out.enhances.is_empty());
    }

    /// 验签失败时 `sig_file` 必须如实反映**文件是否存在**
    ///
    /// 前端用 `sig_file` 区分「签名不匹配（可能被篡改）」与「压根没签名」。
    /// 若这里恒为 false，被篡改的插件会被显示成「未签名」——恰好在最需要
    /// 提示风险时给出误导性文案。
    #[test]
    fn report_marks_sig_file_even_when_verification_fails() {
        use base64::Engine as _;
        let _g = no_escape_hatch();
        let (dir, _pubk) = staged_dir();
        let so = dir.path().join("libstage.so");
        // 用「另一把」公钥去验：签名文件存在，但内容不匹配 → 验签失败
        // （该公钥由 `openssl genpkey -algorithm ED25519` 另生成，与签名用的不是同一把）
        let other = "8rjSFauUw9jl/Qu+OvEV5YOZNwL9ZFB2n9eFMaNtwZ4=";
        let out = load_external(&[dir.path().to_path_buf()], &map_of(other), &[]);
        let r = &out.reports[0];
        assert!(!r.loaded, "公钥不匹配应拒绝加载");
        assert_eq!(r.signature, SignatureStatus::Failed);
        assert!(r.sig_file, "`.sig` 确实存在，不能报成「未签名」");
        assert!(so.exists() && sig_path_of(&so).exists());
        // 顺带确认签名文件确实是 64 字节裸签名（与文档/脚本契约一致）
        let raw = std::fs::read(sig_path_of(&so)).unwrap();
        assert_eq!(raw.len(), 64);
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(other).unwrap().len(), 32);
    }
}
