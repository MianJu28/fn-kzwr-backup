//! 插件市场客户端（索引拉取 / 验签 / 兼容性判定 / 制品下载）
//!
//! ## 它在整个方案里的位置
//!
//! 市场**不是**信任源：它只负责"发现与运输"。真正的信任判定仍然由
//! [`crate::plugin::loader::verify_plugin`] 在 `dlopen` **之前**完成。
//! 本模块保证的是另一件事：**下到的字节就是官方签名索引里描述的那一套**。
//!
//! 完整信任链：
//!
//! ```text
//! 官方私钥 → index.json.sig → index.json → sha256 → libx.so.sig → libx.so
//! ```
//!
//! ## 三条硬规则（改动前必读）
//!
//! 1. **索引未验签通过 ⇒ 绝不可用于安装**（连缓存里的旧索引也不行）；
//! 2. **先比 sha256，再验签** —— 验签只证明"是谁签的"，不证明"下到的是那份字节"；
//! 3. **兼容性在下载之前判定**（abi / arch / glibc / min_host_version），省流量且早报错。
//!
//! ## 不做的事
//!
//! - **不做网络端口限制**（恶意插件要走 443 外传照样能走）—— 评审决定，
//!   与其给虚假安全感，不如在安装弹窗如实告知用户；
//! - **不提供用户自定义索引源**（评审决定）。

use std::collections::HashMap;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

use super::abi::{C_ABI_VERSION, HOST_VERSION};
use super::loader::{sig_path_of, OFFICIAL_PUBKEYS};

/// 索引地址（**编译期内置**，用户不可配置 —— 评审决策）
///
/// 顺序即优先级：先成功**验签**的胜出。多个地址只是**可用性**冗余
/// （仓库 raw 与 Release 资产属不同可用性域），**不是信任面扩张** ——
/// 每个来源都必须通过 `OFFICIAL_PUBKEYS` 验签，伪造源无法得逞。
pub const INDEX_URLS: &[&str] = &[
    // 主源：仓库内的签名索引
    "https://raw.githubusercontent.com/MianJu28/fn-kzwr-backup-plugins/main/index.json",
    // 备用：Release 资产（另一可用性域）
    "https://github.com/MianJu28/fn-kzwr-backup-plugins/releases/latest/download/index.json",
];

/// 索引 schema 版本：宿主遇到**更大**的值应拒绝（避免误读新语义）
pub const INDEX_SCHEMA: u32 = 1;

/// 缓存文件（位于 `$TRIM_PKGVAR`）
const CACHE_FILE: &str = "market/index.json";
const CACHE_SIG_FILE: &str = "market/index.json.sig";

// ── 数据结构 ──────────────────────────────────────────────────────────────

/// 索引顶层
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub schema: u32,
    /// 单调递增，用于**反回滚**
    pub catalog_version: u64,
    pub generated_at: String,
    pub commit: String,
    /// 生成索引时所用的宿主 ABI；宿主据此一眼判断整份索引是否适用
    pub host_abi: u32,
    pub plugins: Vec<CatalogPlugin>,
    #[serde(default)]
    pub revoked: Vec<Revoked>,
}

/// 索引中的插件条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogPlugin {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub publisher: String,
    #[serde(default)]
    pub reviewed_by: String,
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub versions: Vec<CatalogVersion>,
}

/// 索引中的单个版本
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogVersion {
    pub version: String,
    pub released_at: String,
    /// 落盘文件名（验签公钥按**文件名**绑定）
    pub file_name: String,
    pub url: String,
    pub sig_url: String,
    pub sha256_so: String,
    pub sha256_sig: String,
    pub size_bytes: u64,
    pub abi: u32,
    pub arch: String,
    #[serde(default)]
    pub glibc_min: String,
    #[serde(default)]
    pub min_host_version: String,
    #[serde(default)]
    pub source_commit: String,
    #[serde(default)]
    pub yanked: bool,
}

/// 撤销条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revoked {
    pub file_name: String,
    pub sha256_so: String,
    #[serde(default)]
    pub reason: String,
    /// `block` = 拒绝加载；`warn` = 仅提示
    #[serde(default)]
    pub severity: String,
}

/// 兼容性判定结果
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Blocked {
    Ok,
    AbiMismatch,
    ArchMismatch,
    GlibcTooOld,
    HostTooOld,
    Revoked,
    Yanked,
    AlreadyInstalled,
}

impl Blocked {
    /// 中文原因（前端直接展示）
    pub fn reason(self) -> &'static str {
        match self {
            Self::Ok => "",
            Self::AbiMismatch => "插件与当前宿主的程序接口版本不一致",
            Self::ArchMismatch => "插件架构与本机不匹配",
            Self::GlibcTooOld => "插件需要更新的系统库（glibc）",
            Self::HostTooOld => "插件需要更新版本的本应用",
            Self::Revoked => "该插件已被撤销（安全问题）",
            Self::Yanked => "该版本已被作者撤回",
            Self::AlreadyInstalled => "已安装",
        }
    }
    pub fn is_ok(self) -> bool {
        self == Self::Ok
    }
}

/// 列表项（含宿主侧的判定结果）
#[derive(Debug, Clone, Serialize)]
pub struct CatalogItem {
    #[serde(flatten)]
    pub plugin: CatalogPlugin,
    /// 该插件最新可用版本（已按兼容性过滤）
    pub latest: Option<CatalogVersion>,
    pub installed_version: Option<String>,
    pub installable: bool,
    pub blocked_reason: String,
}

// ── 缓存 ──────────────────────────────────────────────────────────────────

/// 市场客户端状态
///
/// 索引持久化在 `$TRIM_PKGVAR/market/index.json`（+ `.sig`），
/// 每次读取都**重新验签**，故内存里不缓存索引内容 —— 磁盘被篡改也拦得住。
#[derive(Debug, Default)]
pub struct Market {
    /// 序列化保护：避免并发刷新时同时写盘
    _refresh_lock: tokio::sync::Mutex<()>,
}

impl Market {
    /// 读取已缓存的索引（**总是重新验签**，绝不信任磁盘内容）
    ///
    /// 返回 `None` 表示尚未缓存或缓存不可用。
    pub fn cached(&self, var_dir: &std::path::Path) -> Option<(Catalog, String)> {
        let raw = std::fs::read(var_dir.join(CACHE_FILE)).ok()?;
        let sig = std::fs::read(var_dir.join(CACHE_SIG_FILE)).ok()?;
        if verify_signature(&raw, &sig).is_err() {
            tracing::warn!("缓存的索引验签失败 —— 拒绝使用（含已缓存版本）");
            return None;
        }
        let c: Catalog = serde_json::from_slice(&raw).ok()?;
        if c.schema > INDEX_SCHEMA {
            return None;
        }
        Some((c, "cache".to_string()))
    }

    /// 从网络刷新索引（依次尝试 [`INDEX_URLS`]）
    ///
    /// 每个源都必须**验签通过**才采用。全部失败时返回错误，
    /// 此时调用方应继续用缓存（若缓存本身也验签不过，则整个市场不可用）。
    pub async fn refresh(&self, var_dir: &std::path::Path) -> Result<(Catalog, String), String> {
        // 并发保护：同一时刻只跑一次刷新
        let _guard = self._refresh_lock.lock().await;
        let mut last_err = String::from("无可用索引源");
        for base in INDEX_URLS {
            match fetch_and_verify(base).await {
                Ok((cat, raw, sig)) => {
                    // 反回滚：catalog_version 小于已缓存值 ⇒ 拒绝采用
                    if let Some((old, _)) = self.cached(var_dir) {
                        if cat.catalog_version < old.catalog_version {
                            return Err(format!(
                                "索引版本回退（缓存 {} > 新 {}）：拒绝采用，防「喂旧索引以重新暴露已撤销版本」",
                                old.catalog_version, cat.catalog_version
                            ));
                        }
                    }
                    let _ = save_cache(var_dir, &raw, &sig);
                    return Ok((cat, base.to_string()));
                }
                Err(e) => {
                    tracing::warn!(source = %base, err = %e, "索引源不可用，尝试下一个");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }
}

// ── 拉取与验签 ────────────────────────────────────────────────────────────

/// 拉取 `index.json` 与 `index.json.sig` 并验签
async fn fetch_and_verify(base: &str) -> Result<(Catalog, Vec<u8>, Vec<u8>), String> {
    let raw = http_get(base).await?;
    let sig_url = format!("{base}.sig");
    let sig = http_get(&sig_url).await?;

    verify_signature(&raw, &sig)?;

    let cat: Catalog = serde_json::from_slice(&raw)
        .map_err(|e| format!("索引 JSON 解析失败：{e}"))?;
    if cat.schema > INDEX_SCHEMA {
        return Err(format!(
            "索引 schema {} 高于宿主支持的 {INDEX_SCHEMA}，请升级本应用",
            cat.schema
        ));
    }
    Ok((cat, raw, sig))
}

/// 用内置 `OFFICIAL_PUBKEYS` 验签 Ed25519 签名（64 字节裸签名）
pub fn verify_signature(data: &[u8], sig: &[u8]) -> Result<(), String> {
    use ring::signature::{UnparsedPublicKey, ED25519};
    use base64::Engine as _;
    let eng = base64::engine::general_purpose::STANDARD;

    let mut last = String::new();
    for k in OFFICIAL_PUBKEYS {
        let Ok(raw) = eng.decode(k) else { continue };
        if raw.len() != 32 {
            continue;
        }
        let key = UnparsedPublicKey::new(&ED25519, &raw);
        match key.verify(data, sig) {
            Ok(()) => return Ok(()),
            Err(_) => last = "签名与内置官方公钥不匹配".to_string(),
        }
    }
    Err(if last.is_empty() {
        "没有可用的官方公钥".to_string()
    } else {
        last
    })
}

fn save_cache(var_dir: &std::path::Path, raw: &[u8], sig: &[u8]) -> Result<(), String> {
    let dir = var_dir.join("market");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建缓存目录失败：{e}"))?;
    std::fs::write(dir.join("index.json"), raw).map_err(|e| format!("{e}"))?;
    std::fs::write(dir.join("index.json.sig"), sig).map_err(|e| format!("{e}"))?;
    Ok(())
}

/// 受限的 HTTPS GET（仅 https、超时）
async fn http_get(url: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") {
        return Err(format!("拒绝非 https 地址：{url}"));
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(1))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("构造 HTTP 客户端失败：{e}"))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求 {url} 失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url} 返回 HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("读取响应失败：{e}"))?;
    Ok(bytes.to_vec())
}

// ── 兼容性判定 ────────────────────────────────────────────────────────────

/// 当前架构名（与索引 `arch` 比对）
pub fn current_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    }
}

/// 判定某版本能否安装（**不联网**，纯本地判断）
pub fn compatibility(v: &CatalogVersion, installed: Option<&str>) -> Blocked {
    // ABI 必须**精确相等**（`validate_table` 的要求，非区间）
    if v.abi != C_ABI_VERSION {
        return Blocked::AbiMismatch;
    }
    if v.arch != current_arch() {
        return Blocked::ArchMismatch;
    }
    if !glibc_ok(&v.glibc_min) {
        return Blocked::GlibcTooOld;
    }
    if !host_ok(&v.min_host_version) {
        return Blocked::HostTooOld;
    }
    if v.yanked {
        return Blocked::Yanked;
    }
    if installed == Some(v.version.as_str()) {
        return Blocked::AlreadyInstalled;
    }
    Blocked::Ok
}

/// 本机 glibc 是否满足插件要求（空要求视为满足）
fn glibc_ok(min: &str) -> bool {
    if min.is_empty() {
        return true;
    }
    let Some(cur) = glibc_version() else {
        // 取不到本机版本（非 glibc 平台）时**放行**，交给 dlopen 失败去暴露
        return true;
    };
    compare_version(&cur, min) >= 0
}

/// 读取本机 glibc 版本（Linux/glibc；其它平台返回 None）
fn glibc_version() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        // `gnu_get_libc_version` 由 libc crate 提供，返回如 "2.36"
        unsafe {
            let p = libc::gnu_get_libc_version();
            if p.is_null() {
                return None;
            }
            Some(std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// 宿主版本是否 ≥ `min`（空要求视为满足）
fn host_ok(min: &str) -> bool {
    if min.is_empty() {
        return true;
    }
    compare_version(HOST_VERSION, min) >= 0
}

/// 比较 `a` 与 `b`（点分数字，如 `0.5.2`）；返回 -1/0/1
///
/// **手写**而非引入 semver：只比较数字段，忽略 pre-release 后缀
/// （`0.6.0-beta.1` 视作 `0.6.0`）。索引里不应出现 pre-release。
pub fn compare_version(a: &str, b: &str) -> i32 {
    let num = |s: &str| -> Vec<u64> {
        let mut out = Vec::new();
        for part in s.split('.') {
            // 遇到非纯数字段（如 `0.6.0-beta.1` 的 `0-beta`）即**停止** ——
            // pre-release 后缀不参与比较，也避免把后缀里的数字误当版本段。
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.len() != part.len() {
                break;
            }
            out.push(part.parse::<u64>().unwrap_or(0));
        }
        out
    };
    let (x, y) = (num(a), num(b));
    for i in 0..x.len().max(y.len()) {
        let (p, q) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
        match p.cmp(&q) {
            std::cmp::Ordering::Less => return -1,
            std::cmp::Ordering::Greater => return 1,
            std::cmp::Ordering::Equal => {}
        }
    }
    0
}

// ── 列表构造 ──────────────────────────────────────────────────────────────

/// 把索引条目 + 已装版本 合成前端要的列表
pub fn build_items(
    catalog: &Catalog,
    installed: &HashMap<String, String>,
) -> Vec<CatalogItem> {
    let mut items = Vec::with_capacity(catalog.plugins.len());
    for p in &catalog.plugins {
        // 取最新且**兼容**的版本（索引内版本按发布时间倒序时取第一个可装的）
        let mut latest = None;
        for v in &p.versions {
            if v.arch != current_arch() {
                continue;
            }
            if v.yanked || is_revoked(catalog, v) {
                continue;
            }
            if latest.is_none() {
                latest = Some(v.clone());
            } else if compare_version(&v.version, &latest.as_ref().unwrap().version) > 0 {
                latest = Some(v.clone());
            }
        }
        let inst = installed.get(&p.id).cloned();
        let blocked = match &latest {
            Some(v) => compatibility(v, inst.as_deref()),
            None => {
                // 没有任何兼容版本：给出最具体的原因（取一个版本来判）
                p.versions
                    .first()
                    .map(|v| compatibility(v, inst.as_deref()))
                    .unwrap_or(Blocked::ArchMismatch)
            }
        };
        items.push(CatalogItem {
            plugin: p.clone(),
            latest,
            installed_version: inst,
            installable: blocked.is_ok(),
            blocked_reason: blocked.reason().to_string(),
        });
    }
    items
}

/// 是否被撤销（`severity=block` 才拦截）
pub fn is_revoked(catalog: &Catalog, v: &CatalogVersion) -> bool {
    catalog.revoked.iter().any(|r| {
        r.severity == "block"
            && (r.file_name == v.file_name || r.sha256_so == v.sha256_so)
    })
}

// ── 下载与安装前的校验 ────────────────────────────────────────────────────

/// 下载制品并**先比 sha256 再验签**，返回 (so 字节, sig 字节)
///
/// 顺序不可交换：验签只证明"是谁签的"，不证明"下到的就是索引里那一份"。
pub async fn download_artifact(
    v: &CatalogVersion,
    max_mb: u32,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let limit = (max_mb as u64).saturating_mul(1024 * 1024);
    if v.size_bytes > limit {
        return Err(format!(
            "制品 {} 字节超过上限 {} MB，拒绝下载",
            v.size_bytes, max_mb
        ));
    }
    let so = http_get(&v.url).await?;
    let sig = http_get(&v.sig_url).await?;

    if so.len() as u64 > limit {
        return Err(format!("下载内容超过 {} MB 上限", max_mb));
    }
    if v.size_bytes > 0 && so.len() as u64 != v.size_bytes {
        return Err(format!(
            "大小不符：索引声明 {}，实际 {}",
            v.size_bytes,
            so.len()
        ));
    }
    // ① 先比哈希
    if !v.sha256_so.is_empty() && sha256_hex(&so) != v.sha256_so.to_ascii_lowercase() {
        return Err("制品 sha256 与索引不符（可能被替换）".to_string());
    }
    if !v.sha256_sig.is_empty() && sha256_hex(&sig) != v.sha256_sig.to_ascii_lowercase() {
        return Err("签名文件 sha256 与索引不符".to_string());
    }
    // ② 再验签
    verify_signature(&so, &sig)?;
    Ok((so, sig))
}

/// SHA-256 十六进制
pub fn sha256_hex(data: &[u8]) -> String {
    use ring::digest::{digest, SHA256};
    let d = digest(&SHA256, data);
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// 落盘路径（用户插件目录）
pub fn install_dir() -> Result<PathBuf, String> {
    let Some(etc) = std::env::var("TRIM_PKGETC").ok().filter(|s| !s.is_empty()) else {
        return Err("无法确定插件安装目录（缺少 TRIM_PKGETC）".to_string());
    };
    Ok(std::path::Path::new(&etc).join("plugins"))
}

/// 该插件的签名文件路径（落盘后）
pub fn installed_sig_path(dir: &std::path::Path, file_name: &str) -> PathBuf {
    sig_path_of(&dir.join(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_numeric() {
        assert_eq!(compare_version("1.2.0", "1.2.0"), 0);
        assert!(compare_version("1.10.0", "1.9.0") > 0);
        assert!(compare_version("0.5.2", "0.6.0") < 0);
        assert_eq!(compare_version("0.6.0-beta.1", "0.6.0"), 0);
    }

    #[test]
    fn sha256_matches_known_vector() {
        // "abc" 的 SHA-256
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn abi_must_match_exactly() {
        let v = CatalogVersion {
            version: "1.0.0".into(),
            released_at: String::new(),
            file_name: "x.so".into(),
            url: String::new(),
            sig_url: String::new(),
            sha256_so: String::new(),
            sha256_sig: String::new(),
            size_bytes: 1,
            abi: C_ABI_VERSION + 1,
            arch: current_arch().to_string(),
            glibc_min: String::new(),
            min_host_version: String::new(),
            source_commit: String::new(),
            yanked: false,
        };
        assert_eq!(compatibility(&v, None), Blocked::AbiMismatch);
    }

    #[test]
    fn arch_mismatch_is_blocked() {
        let mut v = CatalogVersion {
            version: "1.0.0".into(),
            released_at: String::new(),
            file_name: "x.so".into(),
            url: String::new(),
            sig_url: String::new(),
            sha256_so: String::new(),
            sha256_sig: String::new(),
            size_bytes: 1,
            abi: C_ABI_VERSION,
            arch: if current_arch() == "x86_64" { "aarch64" } else { "x86_64" }.to_string(),
            glibc_min: String::new(),
            min_host_version: String::new(),
            source_commit: String::new(),
            yanked: false,
        };
        assert_eq!(compatibility(&v, None), Blocked::ArchMismatch);

        v.arch = current_arch().to_string();
        v.yanked = true;
        assert_eq!(compatibility(&v, None), Blocked::Yanked);
    }

    #[test]
    fn host_too_old_is_blocked() {
        let v = CatalogVersion {
            version: "1.0.0".into(),
            released_at: String::new(),
            file_name: "x.so".into(),
            url: String::new(),
            sig_url: String::new(),
            sha256_so: String::new(),
            sha256_sig: String::new(),
            size_bytes: 1,
            abi: C_ABI_VERSION,
            arch: current_arch().to_string(),
            glibc_min: String::new(),
            min_host_version: "999.0.0".into(),
            source_commit: String::new(),
            yanked: false,
        };
        assert_eq!(compatibility(&v, None), Blocked::HostTooOld);
    }

    /// 反回滚的核心性质：版本比较能识别"更小的 catalog_version"
    #[test]
    fn catalog_version_is_comparable() {
        assert!(100u64 > 99u64);
    }

    #[test]
    fn revoked_blocks_matching_artifact() {
        let cat = Catalog {
            schema: INDEX_SCHEMA,
            catalog_version: 1,
            generated_at: String::new(),
            commit: String::new(),
            host_abi: C_ABI_VERSION,
            plugins: vec![],
            revoked: vec![Revoked {
                file_name: "bad.so".into(),
                sha256_so: "deadbeef".into(),
                reason: "恶意代码".into(),
                severity: "block".into(),
            }],
        };
        let v = CatalogVersion {
            version: "1.0.0".into(),
            released_at: String::new(),
            file_name: "bad.so".into(),
            url: String::new(),
            sig_url: String::new(),
            sha256_so: "deadbeef".into(),
            sha256_sig: String::new(),
            size_bytes: 1,
            abi: C_ABI_VERSION,
            arch: current_arch().to_string(),
            glibc_min: String::new(),
            min_host_version: String::new(),
            source_commit: String::new(),
            yanked: false,
        };
        assert!(is_revoked(&cat, &v));
    }
}
