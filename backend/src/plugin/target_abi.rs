//! 目标能力表（[`KzwrTargetAbi`]）的宿主适配器
//!
//! 把插件导出的 C 函数表包装成宿主内部的 [`TargetPlugin`] / [`TargetStorage`]，从而：
//! - **备份流水线（扫描/diff/加密/快照/保留/恢复）一行不需改动** —— 这里只是"函数指针版"的存储
//! - 外置与内置目标插件**走同一套契约**，宿主差异仅为"dlopen 取表" vs "编译期静态表"
//!
//! ## 关键机制
//!
//! - **目标实例句柄 `th`**：`target_open(target_json)` 每个目标一份；宿主侧 [`AbiTargetStorage`]
//!   只持有 `th` 与函数表。契约约定：**同一 job 内多路并发 handle** 由插件支持并发；读写同一文件
//!   不交叉由上层保证。
//! - **推块传输**：宿主在 `spawn_blocking` 里把密文逐块喂给 `write_chunk`，块间做超时判断
//!   （**看门狗**）；单次阻塞 FFI 卡死无法强杀，只能判失败并泄漏该线程（契约已明示）。
//! - **字节复查**：`write_end` 返回实写密文字节数，宿主与该文件喂出的字节累加比对；不一致按失败
//!   处理（防静默截断/篡改）。
//!
//! ## 安全
//!
//! FFI 本身是 `unsafe` 的信任边界：插件即任意本机代码。宿主只做契约约定内的操作，并假定插件遵守
//! "字符串 UTF-8/NUL 结尾、返回字符串用 `free_str` 释放"等约定。加载前另有签名校验（见签名模块）。

use std::ffi::c_void;
use std::ffi::CString;
use std::os::raw::c_char;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde::Deserialize;

use crate::infra::config::{ConfigManager, TargetConfig};
use crate::infra::storage_trait::{
    FileDescriptor, PlanSession, PlanUpload, ProgressCb, StorageError, StorageResult, TargetStorage,
};
use crate::plugin::abi::{AbiDescribe, AbiTargetCaps, KzwrTargetAbi};
use crate::plugin::api::{PluginMeta, PluginUi, TargetPlugin};

/// 从插件返回的裸指针取 UTF-8 字符串并用其 `free_str` 释放；NULL → None
fn take_cstring(p: *mut c_char, abi: &'static KzwrTargetAbi) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: 契约要求返回以 NUL 结尾的 UTF-8 字符串
    let s = unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned();
    unsafe { (abi.free_str)(p) };
    Some(s)
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 取插件最近一次错误的详情；未实现 `last_error_json` 时给 fallback
fn plugin_detail(abi: &'static KzwrTargetAbi, th: *mut c_void, fallback: String) -> String {
    abi.last_error_json
        .and_then(|le| {
            let p = unsafe { le(th) };
            take_cstring(p, abi)
        })
        .unwrap_or(fallback)
}

/// 并发回传的并发度上限（防止误设超大值把目标端压垮）
pub const MAX_PARALLEL: u32 = 8;

/// 目标插件：包装 `export_target_v1` 导出的静态表
pub struct CApiTarget {
    meta: PluginMeta,
    abi: &'static KzwrTargetAbi,
    caps: AbiTargetCaps,
    ui: Option<PluginUi>,
}

impl CApiTarget {
    pub fn from_static(
        abi: &'static KzwrTargetAbi,
        meta: PluginMeta,
        caps: AbiTargetCaps,
        ui: Option<PluginUi>,
    ) -> Self {
        Self { meta, abi, caps, ui }
    }

    /// 该目标是否需要用户名/密码（插件在 `describe_json.target.needs_credentials` 声明）
    pub fn needs_credentials(&self) -> bool {
        self.caps.needs_credentials
    }

    /// 目标地址字段的展示标签（插件可自定义，如「目录路径」）
    pub fn url_label(&self) -> Option<String> {
        self.caps.url_label.clone()
    }

    /// 目标地址字段的占位提示
    pub fn url_placeholder(&self) -> Option<String> {
        self.caps.url_placeholder.clone()
    }

    /// 目标地址字段的说明文字
    pub fn url_hint(&self) -> Option<String> {
        self.caps.url_hint.clone()
    }

    /// 是否支持并发回传：插件声明支持，且确实实现了 `plan_begin` / `plan_next`
    pub fn supports_plan(&self) -> bool {
        self.caps.supports_plan
            && self.abi.plan_begin.is_some()
            && self.abi.plan_next.is_some()
    }

    /// 按**本目标**的用户配置覆盖并发回传能力（回退到插件级，再回退到插件声明）
    ///
    /// 优先级：`TargetConfig.parallel`（按目标）→ `plugins.target_parallel[插件id]`
    /// （旧配置的兼容回退）→ 插件自身声明的 `caps`。
    ///
    /// 必须**按目标**取值：一个插件会被多个目标同时实例化（多账号各一套凭据），
    /// 它们网络条件不同 —— 曾经只读插件级那份，改一个目标导致同类型目标全变。
    ///
    /// 取值语义：`None` = 未设置（继续回退）；`0`/`1` = 顺序上传；`≥2` = 并发路数（上限 8）。
    /// 用 `Option` 而非 `0` 表示未设置，才能区分「显式设为顺序」与「跟随插件默认」。
    ///
    /// 在 `build()` 而非构造时读取：插件实例在启动时装配，那时配置尚未加载。
    fn caps_for(&self, target: &TargetConfig, mgr: &ConfigManager) -> AbiTargetCaps {
        let mut caps = self.caps.clone();
        // 第一优先：本目标自己的设置
        let v = target.parallel.or_else(|| {
            // 回退：旧的插件级设置（配置迁移会把旧值继承到目标上，这里是给未迁移/新目标兜底）
            mgr.load()
                .ok()
                .and_then(|c| c.plugins.target_parallel.get(&self.meta.id).copied())
        });
        if let Some(v) = v {
            caps.max_parallel = v.min(MAX_PARALLEL);
            // 插件本身不支持时，即便用户配置了并发度也不启用
            caps.supports_plan = caps.supports_plan && v >= 2;
        }
        caps
    }

    /// 校验并从 describe 构造目标插件（由加载器对新发现的目标能力调用）
    pub unsafe fn adopt(
        table: *const KzwrTargetAbi,
        describe: AbiDescribe,
        origin: &str,
    ) -> Result<Self, String> {
        if table.is_null() {
            return Err(format!("{origin}: 目标能力入口返回了空指针"));
        }
        let t: &'static KzwrTargetAbi = &*table;
        if t.abi != crate::plugin::abi::C_ABI_VERSION {
            return Err(format!(
                "{origin}: 目标能力表 ABI 版本为 {}，宿主支持 {}",
                t.abi,
                crate::plugin::abi::C_ABI_VERSION
            ));
        }
        let need = KzwrTargetAbi::REQUIRED_SIZE as usize;
        if (t.size as usize) < need {
            return Err(format!(
                "{origin}: 目标能力表长度 {} 小于宿主要求的必需前缀 {}",
                t.size, need
            ));
        }
        let caps = describe.target.clone().unwrap_or_default();
        let meta = PluginMeta {
            id: describe.id.clone(),
            name: describe.name.clone(),
            version: describe.version.clone(),
            kind: crate::plugin::api::PluginKind::Target,
            builtin: false,
            description: describe.description.clone(),
        };
        Ok(Self { meta, abi: t, caps, ui: describe.ui.clone() })
    }

    fn chunk_size(&self) -> usize {
        let kib = if self.caps.preferred_chunk_kib == 0 {
            1024
        } else {
            self.caps.preferred_chunk_kib as usize
        };
        (kib * 1024).clamp(64 * 1024, 16 * 1024 * 1024)
    }

    /// 构造 `target_json`（含该目标凭据，仅传给 `target_open`，不落日志）
    ///
    /// `config` 是**本目标自己的**自定义字段（`TargetConfig.fields`，按目标加密存储、此处已解密）：插件从这里读
    /// 自己的设置（如 `config.root`）。**整体不得写日志。**
    #[allow(clippy::too_many_arguments)]
    fn target_json(
        &self,
        target: &TargetConfig,
        user: &str,
        pass: &str,
        config: serde_json::Value,
        fields: serde_json::Value,
        proxy: Option<serde_json::Value>,
    ) -> String {
        serde_json::json!({
            "id": target.id,
            "name": target.name,
            "kind": target.kind,
            "url": target.url.clone().unwrap_or_default(),
            "username": user,
            "password": pass,
            // 本目标的自定义字段（ADR-021：宿主不再代存**插件级**配置）
            "config": config,
            // **本目标**的插件自定义字段（按目标存储、已解密）
            //
            // 与 `config` 分开给出，是为了让插件能区分「这个目标自己的值」与
            // 「该插件的全局值」；但为了让既有插件（读 `config.xxx`）无需改动，
            // 调用方已把本目标的键**合并进 `config`**（同名时目标级优先）。
            "fields": fields,
            // 本目标该用的网络代理（`null` = 直连）
            //
            // 由宿主决定（总开关 + 本目标勾选 + no_proxy），插件**只负责使用**：
            // 代理地址/凭据属于环境配置，不该让每个插件各自再实现一遍解析与匹配。
            "proxy": proxy,
        })
        .to_string()
    }

    /// 解析本目标该用的代理（供 `target_json` 注入）
    ///
    /// 具体判定委托给 [`crate::infra::config::ProxySettings::resolve_for`]，
    /// 保证与「保存前实测」「连通性测试」用的是同一份逻辑。
    fn resolve_proxy_json(
        &self,
        target: &TargetConfig,
        mgr: &ConfigManager,
    ) -> Option<serde_json::Value> {
        let cfg = mgr.load().ok()?;
        let url = target.url.clone().unwrap_or_default();
        cfg.proxy.resolve_for(mgr, &url, target.use_proxy)
    }

    /// 从「插件声明的路径字段」取出本实例的实际路径（供沙箱白名单）
    ///
    /// 只看 `caps.path_fields` 里列出的键 —— 宿主**不猜**哪个字段是路径。
    /// `url` 是 well-known 键（存 `TargetConfig.url`），其余从该目标自己的
    /// `fields` 取。
    fn instance_paths(
        &self,
        target: &TargetConfig,
        fields: &std::collections::BTreeMap<String, String>,
    ) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for key in &self.caps.path_fields {
            let v = if key == "url" {
                target.url.clone()
            } else {
                fields.get(key).cloned()
            };
            if let Some(p) = v {
                let p = p.trim();
                // 只接受**绝对路径**：相对路径无法安全地映射到白名单
                // （它相对谁？），宁可不开这个口子，让插件显式给绝对路径。
                if !p.is_empty() && p.starts_with('/') {
                    out.push(std::path::PathBuf::from(p));
                } else if !p.is_empty() {
                    tracing::warn!(
                        target = %target.id, key = %key, value = %p,
                        "path_fields 声明的值不是绝对路径，沙箱白名单已忽略该值"
                    );
                }
            }
        }
        out
    }

}

#[async_trait]
impl TargetPlugin for CApiTarget {
    fn meta(&self) -> PluginMeta {
        self.meta.clone()
    }
    fn ui(&self) -> Option<PluginUi> {
        self.ui.clone()
    }
    fn supports_plan(&self) -> bool {
        CApiTarget::supports_plan(self)
    }
    /// 由插件在 `describe_json.target.needs_credentials` 声明（缺省 `true`）
    fn needs_credentials(&self) -> bool {
        CApiTarget::needs_credentials(self)
    }
    fn url_label(&self) -> Option<String> {
        CApiTarget::url_label(self)
    }
    fn url_placeholder(&self) -> Option<String> {
        CApiTarget::url_placeholder(self)
    }
    fn url_hint(&self) -> Option<String> {
        CApiTarget::url_hint(self)
    }
    /// 由插件在 `describe_json.target.form` 声明（缺省空 ⇒ 前端按 WebDAV 渲染）
    fn form_fields(&self) -> Vec<crate::plugin::abi::AbiTargetField> {
        self.caps.form.clone()
    }
    /// 由插件在 `describe_json.target.path_fields` 声明（供沙箱白名单）
    fn path_fields(&self) -> Vec<String> {
        self.caps.path_fields.clone()
    }
    fn build(&self, target: &TargetConfig, mgr: &ConfigManager) -> Option<(Arc<dyn TargetStorage>, String)> {
        // 停用的目标不装配（回退占位适配器 ⇒「未就绪」）。
        // 旧的内置实现有此判断，ABI 化时漏了 ⇒ 停用目标仍显示「已就绪」。
        if !target.enabled {
            return None;
        }
        // 解密在调用方/mgr 内完成（与内置目标一致）
        let creds = mgr.target_credentials(target).ok().unwrap_or((None, None));
        let (user, pass) = (creds.0.as_deref().unwrap_or(""), creds.1.as_deref().unwrap_or(""));
        // **凭据完整性**：插件声明 `needs_credentials` 时，用户名/密码缺一不可。
        //
        // 为什么必须在这里判：插件的 `target_open` 对缺失凭据是**刻意宽容**的 ——
        // 它返回一个「带 last_error 的实例」好在后续操作时报出人话错误
        // （见 `builtin/webdav_abi.rs`：凭据缺失时用空凭据建实例并把错误记进 last_error）。
        // 于是 `target_open` 恒返回非空句柄 ⇒ 这里若只看句柄，就会把
        // **完全没配凭据的默认目标**报成「已就绪」（用户实际看到的就是这个 bug）。
        if self.caps.needs_credentials && (user.trim().is_empty() || pass.is_empty()) {
            tracing::info!(
                target = %target.id,
                "目标缺少凭据，回退占位适配器（未就绪）"
            );
            return None;
        }
        // 插件自管配置（命名空间 = 插件 id）解密后注入 `target_json.config`。
        // 注意：此处在 config 锁内（`reload_targets` 持锁调用），只能读、不能写。
        // 宿主不再代存插件配置（ADR-021）⇒ `config` 里**只有本目标自己的字段**。
        let mut config = serde_json::Value::Object(serde_json::Map::new());
        // **本目标**的自定义字段（按目标存储、已解密）：
        // - 合并进 `config`，让既有插件读 `config.xxx` 无需改动；
        // - 同时单独放在 `fields` 里，便于插件区分来源。
        let target_fields = target.custom_fields_plain(mgr);
        let fields_json = serde_json::Value::Object(
            target_fields
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect(),
        );
        if let (Some(cfg_obj), Some(f_obj)) = (config.as_object_mut(), fields_json.as_object()) {
            for (k, v) in f_obj {
                cfg_obj.insert(k.clone(), v.clone());
            }
        }
        // 代理由宿主解析好再交给插件（总开关 + 目标勾选 + no_proxy 都在这里判定）
        let proxy_json = self.resolve_proxy_json(target, mgr);
        let json = self.target_json(target, user, pass, config, fields_json, proxy_json);

        // **先建池、再开放实例**：`target_open` 与 `host_bind` 同类 ——
        // 它是插件在本实例上**最早**执行的代码，能在里面读任意文件。
        // 若先 open 再建池，沙箱就漏了实例初始化那一刻。
        //
        // 池大小 = 该实例并发度（并发回传会并行调用 `write_stream`）。
        let caps = self.caps_for(target, mgr);
        let parallel = if caps.supports_plan {
            caps.max_parallel.max(1) as usize
        } else {
            1
        };
        // 沙箱白名单：插件在 `path_fields` 里声明的那些字段的**本机路径值**
        // （宿主不猜哪个字段是路径；`example-localfs` 声明 url，`webdav` 不声明）
        let extra = self.instance_paths(target, &target_fields);
        let label = format!("{}-{}", self.meta.id, target.id);
        let policy = super::worker::policy_for(&self.meta.id, &extra);
        let pool = super::worker::InstancePool::new(&label, policy, parallel);

        // `target_open` 走池线程（已施加沙箱）。池不可用则退回直调
        // （功能不变，但**无沙箱** —— 建池失败时已记警告）。
        let json_clone = json.clone();
        let th = match pool.as_ref() {
            Some(p) => {
                let (tx, rx) = std::sync::mpsc::channel();
                let abi = self.abi;
                let submitted = p.submit(Box::new(move || {
                    let c = match CString::new(json_clone) {
                        Ok(c) => c,
                        Err(_) => {
                            let _ = tx.send(0usize);
                            return;
                        }
                    };
                    // SAFETY: 同步 FFI，参数在本次调用内有效
                    let h = unsafe { (abi.target_open)(c.as_ptr()) } as usize;
                    let _ = tx.send(h);
                }));
                if submitted {
                    // 池线程执行；等结果（实例初始化通常很快，且此处本就必须同步拿到句柄）
                    //
                    // ⚠️ **必须带超时**：`build()` 由 `reload_targets` 在**持有 config 锁**
                    // 时调用，而 `reload_targets` 是同步函数、被多个 axum handler 直接调用。
                    // 若插件的 `target_open` 卡住（DNS 慢、远程配置、死锁），这里会无限等待
                    // ⇒ 整个 AppState 的配置锁被长期占用 ⇒ 所有接口（读配置、健康检查、
                    // 保存目标）全部阻塞，应用表现为「整体假死」。
                    // 超时按「未就绪」处理：宁可这个目标暂时不可用，也不能锁死全应用。
                    match rx.recv_timeout(std::time::Duration::from_secs(30)) {
                        Ok(h) => h,
                        Err(_) => {
                            tracing::warn!(
                                target = %target.id,
                                "插件 target_open 超过 30s 未返回，按未就绪处理（该池线程可能已泄漏）"
                            );
                            0
                        }
                    }
                } else {
                    0
                }
            }
            None => {
                let c = CString::new(json).ok()?;
                unsafe { (self.abi.target_open)(c.as_ptr()) as usize }
            }
        };
        if th == 0 {
            return None;
        }
        let storage = AbiTargetStorage {
            abi: self.abi,
            caps,
            th: th as usize,
            name: self.meta.name.clone(),
            chunk_size: self.chunk_size(),
            pool,
        };
        Some((Arc::new(storage), target.name.clone()))
    }
    async fn verify(&self, url: Option<&str>, user: &str, pass: &str) -> Result<String, String> {
        // 无自管配置上下文时按空配置测试（兼容既有调用方）
        self.verify_with_config(url, user, pass, serde_json::Value::Object(Default::default()))
            .await
    }
    async fn verify_with_config(
        &self,
        url: Option<&str>,
        user: &str,
        pass: &str,
        config: serde_json::Value,
    ) -> Result<String, String> {
        if let Some(test) = self.abi.test_json {
            // 代理由**调用方**解析好放进 `config.__proxy`（trait 签名不变，
            // 避免波及所有插件实现）。调用方持有 `AppState`，能读全局代理设置；
            // 本层拿不到 `ConfigManager`，无法自行解析。
            //
            // ⚠️ 这里与 `target_open` 用的是同一份判定结果，否则会出现
            // 「测试通过但备份失败」（测试直连、备份走代理，或反之）。
            let proxy = config.get("__proxy").cloned().filter(|p| !p.is_null());
            let config = match config {
                serde_json::Value::Object(mut m) => {
                    m.remove("__proxy"); // 插件不该看到宿主内部保留键
                    serde_json::Value::Object(m)
                }
                other => other,
            };
            let json = serde_json::json!({
                "url": url.unwrap_or_default(),
                "username": user,
                "password": pass,
                // 插件自管配置（命名空间 = 插件 id，已解密）——与 `target_open` 同一契约
                "config": config,
                // 与 `target_open` 同一判定结果（`null` = 直连）
                "proxy": proxy,
            })
            .to_string();
            let c = CString::new(json).map_err(|e| e.to_string())?;
            // **必须投递到插件专属线程（已施加沙箱）**，不能在本线程裸调。
            //
            // 本函数由 axum handler 调用，运行在 tokio worker 上；裸调有三个问题：
            // 1. **绕过沙箱**：这是「测试连接」入口，用户点一次按钮就会触发插件代码，
            //    而它是唯一没走 `run_on_plugin_thread` 的插件入口 —— 沙箱就不再是
            //    「插件的所有入口」；
            // 2. **占死 worker**：插件若阻塞（DNS/远程配置），整个 tokio worker 被卡；
            // 3. 与 `target_open` 的语义不一致（那边有沙箱 + 30s 超时）。
            let id = self.meta.id.clone();
            let policy = super::worker::policy_for(&id, &[]);
            let abi_static = self.abi;
            let body = super::worker::run_on_plugin_thread(&id, policy, move || {
                let p = unsafe { test(c.as_ptr()) };
                take_cstring(p, abi_static)
            })
            .await
            .ok_or_else(|| "测试连接失败：插件线程不可用".to_string())?
            .unwrap_or_default();
            if body.is_empty() {
                return Ok(url.unwrap_or_default().to_string());
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
                    return Err(e.to_string());
                }
                if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
                    return Ok(u.to_string());
                }
            }
            Ok(url.unwrap_or_default().to_string())
        } else {
            Ok(url.unwrap_or_default().to_string())
        }
    }
}

/// 解析插件 `list_json` 的返回体
///
/// ## 为什么单独抽出来
/// 「NULL / 坏 JSON」与「空目录」必须区分开，这是**数据正确性**判断：
/// 一旦把失败当成空列表，`restore` 会认为「云端没有要恢复的文件」而**报告成功**
/// （用户看到恢复 0 个文件，却没有任何错误），`retention` 也会跳过整个子树。
/// 抽成纯函数才能直接覆盖这三种输入。
fn parse_list_body(body: &str) -> Result<Vec<AbiEntry>, StorageError> {
    if body.trim().is_empty() {
        return Ok(Vec::new()); // 合法的空目录
    }
    serde_json::from_str::<Vec<AbiEntry>>(body).map_err(|e| {
        StorageError::Protocol(format!(
            "list_json 返回无法解析为 JSON: {e}（原文前 200 字节：{}）",
            body.chars().take(200).collect::<String>()
        ))
    })
}

/// 目标存储：函数指针版 [`TargetStorage`]
struct AbiTargetStorage {
    abi: &'static KzwrTargetAbi,
    caps: AbiTargetCaps,
    /// 目标实例句柄，以地址 `usize` 存储（裸指针非 `Send`，`usize` 可安全跨线程传递）
    th: usize,
    name: String,
    chunk_size: usize,
    /// **本实例的沙箱线程池**（每实例一小池；并发回传会在池内并行）
    ///
    /// `None` = 池建立失败（线程创建失败），此时退化为原有的 `spawn_blocking`
    /// 行为 —— 功能不受影响，只是没有沙箱保护（会记警告）。
    pool: Option<Arc<super::worker::InstancePool>>,
}

// SAFETY: `th` 为插件句柄；线程安全由契约约定（插件自负），宿主按约定调度
unsafe impl Send for AbiTargetStorage {}
unsafe impl Sync for AbiTargetStorage {}

impl Drop for AbiTargetStorage {
    /// 归还插件实例：调用 `target_close` 释放 `target_open` 分配的句柄
    ///
    /// **这是 `target_open` 的配对方**（`docs/PLUGIN_ABI.md` 把它列为契约字段，
    /// 各插件也都实现了 `Box::from_raw` 释放）。此前宿主**从不调用**它 ⇒
    /// 每次 `reload_targets`（保存配置即触发）都会泄漏一份插件实例。
    ///
    /// 与 [`AbiPlanSession`] 的 `Drop` → `plan_end` 是同一模式。
    /// 注意：`target_close` 是可选字段，且 `th == 0` 表示从未成功打开
    /// （构建失败时也会构造本结构，故必须判零）。
    fn drop(&mut self) {
        let (Some(close), th) = (self.abi.target_close, self.th) else {
            return;
        };
        if th == 0 {
            return;
        }
        // SAFETY: th 由 target_open 发放；Drop 是最后一次使用，
        // 且 AbiTargetStorage 独占该句柄（每实例一个，不共享）。
        unsafe { close(th as *mut c_void) };
    }
}

impl AbiTargetStorage {
    /// 把阻塞闭包投递到**本实例的沙箱线程池**，返回可 `.await` 的句柄
    ///
    /// 池不可用（建立失败）时退回 `tokio::task::spawn_blocking` —— 功能完全一致，
    /// 只是没有沙箱保护（建池时已记过警告）。
    ///
    /// 返回 `JoinHandle`，因此调用点的 `.await` / `map_err` 写法与原来一致。
    fn submit_blocking<F, T>(&self, f: F) -> tokio::task::JoinHandle<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let Some(pool) = self.pool.clone() else {
            return tokio::task::spawn_blocking(f);
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<T>();
        let job: Box<dyn FnOnce() + Send + 'static> = Box::new(move || {
            // panic 兜住：池线程必须存活（否则该实例的后续调用全废）
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
                Ok(v) => {
                    let _ = tx.send(v);
                }
                Err(_) => {
                    tracing::error!("目标插件回调 panic（已兜住，池线程存活）");
                }
            }
        });
        if !pool.submit(job) {
            // 池已关闭（实例正在销毁）：如实把「本次调用失败」传给调用方。
            //
            // 旧代码注释写「退回公共池，保证功能不中断」，实现却是
            // `spawn_blocking(|| panic!(...))` —— 注释与行为完全相反，
            // 调用方只会看到一个伪造的 `task panicked`，真实原因（池已关闭）
            // 被彻底掩盖。这里改为如实返回 `JoinError`，让上层报出可诊断的错误。
            return tokio::task::spawn_blocking(|| {
                panic!("目标插件沙箱池已关闭，本次调用失败（实例可能正在销毁）")
            });
        }
        tokio::spawn(async move {
            // 与 JoinHandle<T> 语义对齐：拿不到结果就 panic 成 JoinError
            rx.await.expect("目标插件回调未返回结果（panic 或池关闭）")
        })
    }

    fn rel_cstr(&self, path: &Path) -> Result<CString, StorageError> {
        CString::new(path.as_os_str().to_string_lossy().as_bytes())
            .map_err(|_| StorageError::Protocol(format!("非法相对路径: {}", path.display())))
    }

    fn err_from(&self, th: *mut c_void, code: i32, what: &str) -> StorageError {
        let detail = plugin_detail(self.abi, th, format!("{}: 错误码 {}", what, code));
        if code == -2 {
            StorageError::Auth(detail)
        } else {
            StorageError::Protocol(detail)
        }
    }

    /// 在阻塞线程里完成一次完整写入：open → 推块 → end → 字节复查。
    fn write_blocking(
        abi: &'static KzwrTargetAbi,
        th: usize,
        rel: CString,
        declared_total: u64,
        rx: tokio::sync::mpsc::Receiver<Bytes>,
        progress: ProgressCb,
        abort: Arc<AtomicBool>,
        last_tick: Arc<AtomicU64>,
        name: &str,
    ) -> Result<u64, StorageError> {
        let mut rx = rx;
        let th = th as *mut c_void;
        // `total` = 密文总字节数（0 = 未知）。插件据此决定能否流式直发；
        // 旧实现一律传 0，导致插件只能先把整份密文攒到临时文件再上传。
        let h = unsafe { (abi.write_begin)(th, rel.as_ptr(), declared_total) };
        if h.is_null() {
            // ⚠️ 必须取插件详情：此处曾只报「write_begin 失败」，把插件 last_error_json
            // 里的真实原因（如建缓冲文件被沙箱拒绝）整个吞掉，故障排查只能靠猜。
            let d = plugin_detail(abi, th, format!("{}: write_begin 失败", name));
            return Err(StorageError::Protocol(d));
        }
        last_tick.store(now_epoch_ms(), Ordering::Relaxed);
        let mut fed: u64 = 0;
        let started = std::time::Instant::now();
        // 进度上报的最小间隔（毫秒）
        //
        // `ProgressCb` 的 elapsed 是**本文件累计**毫秒（`domain::pace::FileMeter` 靠
        // 相邻两次累计值算增量）。若每块都报，快链路上相邻两次的差值可能只有几毫秒，
        // 全部低于 `MIN_OBSERVE_MS`(50ms) 而被当成抖动丢弃 —— 速度就恒为 0。
        // 因此这里按时间节流：距上次上报 ≥100ms 才报，保证每个样本都足够「长」。
        const MIN_REPORT_INTERVAL_MS: u64 = 100;
        let mut last_report_ms: u64 = 0;
        loop {
            if abort.load(Ordering::Relaxed) {
                if let Some(a) = abi.write_abort {
                    unsafe { a(th, h) };
                }
                return Err(StorageError::Protocol(format!(
                    "{}: 传输看门狗超时（>120s 无推进）",
                    name
                )));
            }
            let chunk = match rx.blocking_recv() {
                Some(c) => c,
                None => break,
            };
            last_tick.store(now_epoch_ms(), Ordering::Relaxed);
            let n = unsafe { (abi.write_chunk)(th, h, chunk.as_ptr(), chunk.len() as u32) };
            // ⚠️ 任何 chunk 级失败都必须先 `write_abort` 再返回。
            //
            // 插件侧的传输句柄（`Box<FileWrite>` + 正在跑的上传任务 + channel）
            // 只能由 `write_end` 或 `write_abort` 之一释放。此前这两条错误分支
            // 直接 `return`，于是每次 chunk 失败都泄漏一份插件上下文，
            // 且那个上传任务会继续在后台跑到把 channel 读空 —— 表现为
            // 「内存/线程缓慢增长」+「远端留下半截分片」。
            if n < 0 || n as usize != chunk.len() {
                let detail = if n < 0 {
                    plugin_detail(abi, th, format!("write_chunk 错误码 {}", n))
                } else {
                    format!(
                        "{}: write_chunk 只写入 {} 字节，调用给了 {} 字节",
                        name,
                        n,
                        chunk.len()
                    )
                };
                if let Some(a) = abi.write_abort {
                    unsafe { a(th, h) };
                }
                return Err(StorageError::Protocol(detail));
            }
            fed += n as u64;
            let now_ms = started.elapsed().as_millis() as u64;
            if now_ms.saturating_sub(last_report_ms) >= MIN_REPORT_INTERVAL_MS {
                last_report_ms = now_ms;
                // 传**真实总长**而不是 0：`ProgressCb` 契约里 0 表示「未知」，
                // 而这里明明知道（来自 `write_stream_sized` 的 enc_len）。
                // 当前调用方只用了 written，但传对才能让未来的进度条算出百分比。
                progress(fed, declared_total, now_ms);
            }
        }
        // write_end + 字节复查
        let total = unsafe { (abi.write_end)(th, h) };
        if total < 0 {
            let d = plugin_detail(abi, th, format!("write_end 错误码 {}", total));
            return Err(StorageError::Protocol(d));
        }
        // 收尾补一次，保证进度最终落到全量
        progress(fed, declared_total, started.elapsed().as_millis() as u64);
        let actual = total as u64;
        if actual != fed {
            return Err(StorageError::Protocol(format!(
                "{}: 插件上报实写 {} 字节，但宿主喂出 {} 字节（可能被截断/篡改）",
                name, actual, fed
            )));
        }
        Ok(fed)
    }

    /// 读取（用于恢复）：`read_begin`/`read_chunk` 在**本实例的沙箱线程池**上推进，
    /// 经 channel 变回异步流。
    ///
    /// 为什么不再用裸 `std::thread::spawn`：写路径（`write_blocking`）已经在实例池上跑，
    /// 读路径却另起裸线程 ⇒ 同一条数据通路的隔离模型不一致（读时可绕过池的约束）。
    /// 用 `submit_blocking` 投递到池里，读写走同一套线程与沙箱。
    fn read_blocking(
        storage: &AbiTargetStorage,
        rel: CString,
        chunk_size: usize,
        name: String,
        abort: Arc<AtomicBool>,
        last_tick: Arc<AtomicU64>,
    ) -> tokio::sync::mpsc::Receiver<StorageResult<Bytes>> {
        let (tx, rx) = tokio::sync::mpsc::channel::<StorageResult<Bytes>>(8);
        let abi = storage.abi;
        let th = storage.th;
        let job = move || {
            let th = th as *mut c_void;
            let h = unsafe { (abi.read_begin)(th, rel.as_ptr()) };
            if h.is_null() {
                let _ = tx.blocking_send(Err(StorageError::NotFound(format!("{}: 打开失败", name))));
                return;
            }
            let mut buf = vec![0u8; chunk_size];
            loop {
                // **读路径看门狗**：写路径早有 120s 推进检测，读路径此前完全没有。
                // 缺少它的后果不是「慢」，而是**池线程被永久占住** —— 该实例后续的
                // write/delete/list/ping 全部排队排不上，整个任务看起来只是「卡住」。
                if abort.load(Ordering::Relaxed) {
                    let _ = tx.blocking_send(Err(StorageError::Protocol(format!(
                        "{name}: 读取看门狗超时（>120s 无推进）"
                    ))));
                    break;
                }
                let n = unsafe { (abi.read_chunk)(th, h, buf.as_mut_ptr(), buf.len() as u32) };
                if n > 0 {
                    last_tick.store(now_epoch_ms(), Ordering::Relaxed);
                    let b = Bytes::copy_from_slice(&buf[..n as usize]);
                    if tx.blocking_send(Ok(b)).is_err() {
                        break;
                    }
                } else if n == 0 {
                    break; // EOF：关闭通道即结束流
                } else {
                    let d = plugin_detail(abi, th, format!("read_chunk 错误码 {}", n));
                    let _ = tx.blocking_send(Err(StorageError::Protocol(d)));
                    break;
                }
            }
            if let Some(e) = abi.read_end {
                unsafe { e(th, h) };
            }
        };
        // 投递到本实例的沙箱池（不可用时退回 spawn_blocking，与写路径一致）
        // 显式丢弃句柄：读取端只关心 channel，不 join 这个后台任务
        drop(storage.submit_blocking(job));
        rx
    }
}

impl AbiTargetStorage {
    /// 推块写入的公共实现：`total` 透传给插件（0 = 未知）
    async fn push_stream(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
        total: u64,
        progress: ProgressCb,
    ) -> StorageResult<()> {
        let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(8);
        let producer = async move {
            futures::pin_mut!(stream);
            while let Some(chunk) = stream.next().await {
                if tx.send(chunk).await.is_err() {
                    break;
                }
            }
        };

        let abi = self.abi;
        let th = self.th;
        let rel = self.rel_cstr(path)?;
        let name = self.name.clone();

        // 看门狗共享状态：块间隙超过 120s → 置 abort
        let abort = Arc::new(AtomicBool::new(false));
        let last_tick = Arc::new(AtomicU64::new(now_epoch_ms()));
        let wd_abort = abort.clone();
        let wd_last = last_tick.clone();
        let watchdog = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if now_epoch_ms().saturating_sub(wd_last.load(Ordering::Relaxed)) > 120_000 {
                    wd_abort.store(true, Ordering::Relaxed);
                    break;
                }
            }
        });

        let b_abort = abort.clone();
        let b_tick = last_tick.clone();
        let b_name = name.clone();
        // 走**本实例的沙箱线程池**：并发回传会在池内并行（池大小 = 并发度），
        // 因此不会退化成串行。池不可用时退回 spawn_blocking（功能不变，无沙箱）。
        let blocking_fut = self.submit_blocking(move || {
            Self::write_blocking(abi, th, rel, total, rx, progress, b_abort, b_tick, &b_name)
        });

        // 同时推进 producer 与 blocking，结果取 blocking；producer 自然结束会关闭通道
        let pd = tokio::spawn(async move { producer.await });
        let out = blocking_fut.await;
        watchdog.abort();
        pd.abort();
        out.map_err(|e| StorageError::Other(e.to_string()))??;
        Ok(())
    }
}

#[async_trait]
impl TargetStorage for AbiTargetStorage {
    async fn write_stream(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
    ) -> StorageResult<()> {
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        self.push_stream(path, stream, 0, noop).await
    }

    async fn write_stream_progress(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
        progress: ProgressCb,
    ) -> StorageResult<()> {
        self.push_stream(path, stream, 0, progress).await
    }

    /// 已知密文总长：把 `total` 一路带到插件的 `write_begin`，插件据此**边收边发**
    async fn write_stream_sized(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
        total: u64,
        progress: ProgressCb,
    ) -> StorageResult<()> {
        self.push_stream(path, stream, total, progress).await
    }

    async fn read_stream(
        &self,
        path: &Path,
    ) -> StorageResult<Box<dyn Stream<Item = StorageResult<Bytes>> + Send + Unpin>> {
        let rel = self.rel_cstr(path)?;
        // 读看门狗：与写路径同一套（块间隙 >120s 判失败）。
        // 必须在 `read_blocking` 之前创建，才能覆盖「read_begin 就卡住」的情形。
        let abort = Arc::new(AtomicBool::new(false));
        let last_tick = Arc::new(AtomicU64::new(now_epoch_ms()));
        // 看门狗的「流已结束」标志：流正常读完或被 drop 时置位，让后台任务退出
        let wd_done = Arc::new(AtomicBool::new(false));
        {
            let wd_abort = abort.clone();
            let wd_last = last_tick.clone();
            let wd_name = self.name.clone();
            let wd_done = wd_done.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    // **读流正常结束/被丢弃后必须退出**，否则这个后台任务会永久每 2s
                    // 空转，并在 120s 后误报「读取超过 120s 无推进，判失败」——
                    // 那条警告跟当前任务毫无关系，纯属上个流遗留的噪音，
                    // 还会误导排查（实测：备份收尾阶段反复出现该警告）。
                    if wd_done.load(Ordering::Relaxed) {
                        break;
                    }
                    if now_epoch_ms().saturating_sub(wd_last.load(Ordering::Relaxed)) > 120_000 {
                        tracing::warn!(target = %wd_name, "读取超过 120s 无推进，判失败");
                        wd_abort.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            });
        }
        let rx = Self::read_blocking(self, rel, self.chunk_size, self.name.clone(), abort, last_tick);
        // 流状态带上 wd_done：流**自然读完**或**被提前 drop** 时都要让看门狗退出。
        // 自然读完：recv 返回 None 时置位；
        // 提前 drop：靠 Drop guard（见下）置位。
        struct DoneGuard(Arc<AtomicBool>);
        impl Drop for DoneGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let guard = DoneGuard(wd_done);
        let s = futures::stream::unfold((rx, guard), |(mut rx, guard)| async move {
            match rx.recv().await {
                Some(v) => Some((v, (rx, guard))),
                None => {
                    // guard 在此 drop ⇒ 置位 wd_done，看门狗随即退出
                    drop(guard);
                    None
                }
            }
        });
        Ok(Box::new(Box::pin(s)))
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<FileDescriptor>> {
        let abi = self.abi;
        let th = self.th;
        let c = CString::new(prefix).map_err(|_| StorageError::Protocol("非法前缀".into()))?;
        // (abi.list_json) 返回值是托管字符串指针（非 Send），在闭包内转成 String 再跨线程返回
        // ⚠️ **列目录必须有超时**：读写路径早有 120s 推进看门狗，`list` 之前完全没有。
        //
        // `list_json` 是**同步阻塞**调用（内部走插件自己的 HTTP 客户端）。若云端
        // 连接半死（TCP 已建立但对端不回包），这里会永久阻塞：
        // - 池线程被永久占住（`threads=1` 时该目标的后续调用全废）；
        // - 备份收尾阶段的保留策略 `walk` 正是靠它递归遍历云端 ⇒ **收尾永久卡住**，
        //   界面上「实时任务」停在「收尾…」不动，连 WS 推送都因线程被占而断开。
        //   实测：日志在「已删除: xxx」后再无任何输出，界面转了 6 分钟以上。
        //
        // 超时后按「列目录失败」返回错误（而不是空列表）：上层会把它当作失败处理，
        // 保留策略随之跳过本次清理（宁可不清，也不能因为不知道有哪些文件就误删）。
        const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
        let body = match tokio::time::timeout(LIST_TIMEOUT, self.submit_blocking(move || {
            let th = th as *mut c_void;
            let p = unsafe { (abi.list_json)(th, c.as_ptr()) };
            // `None` = 插件返回 NULL，即**列目录失败**（插件已把原因写进 last_error）
            take_cstring(p, abi)
        }))
        .await
        {
            Ok(joined) => joined.map_err(|e| StorageError::Other(e.to_string()))?,
            Err(_elapsed) => {
                tracing::warn!(
                    prefix,
                    "列目录超过 {}s 无响应（网络/插件阻塞），判失败",
                    LIST_TIMEOUT.as_secs()
                );
                return Err(StorageError::Protocol(format!(
                    "列目录超时：{prefix} 在 {}s 内无响应（链路或插件阻塞）",
                    LIST_TIMEOUT.as_secs()
                )));
            }
        };

        // ⚠️ **NULL 与"空目录"必须区分开**。
        //
        // 旧写法是 `unwrap_or_default()` 一律变成空列表，parse 失败也一样 → 空 Vec。
        // 后果极严重：`restore` 会据此认为「云端没有要恢复的文件」而**报告成功**
        // （用户看到恢复 0 个文件却没有任何错误），`retention` 也会跳过整个子树。
        // 宁可让本次列目录失败（上层会重试/报错），也不能假报「没有文件」。
        let body = match body {
            Some(b) => b,
            None => {
                let d = plugin_detail(abi, self.th as *mut c_void, "list_json 返回 NULL".to_string());
                return Err(StorageError::Protocol(d));
            }
        };
        parse_list_body(&body).map(|v| v.into_iter().map(|e| e.into_fd()).collect())
    }

    async fn delete(&self, path: &Path) -> StorageResult<()> {
        let abi = self.abi;
        let th = self.th;
        let rel = self.rel_cstr(path)?;
        let code = self.submit_blocking(move || {
            let th = th as *mut c_void;
            unsafe { (abi.delete)(th, rel.as_ptr()) }
        })
        .await
        .map_err(|e| StorageError::Other(e.to_string()))?;
        if code != 0 {
            return Err(self.err_from(self.th as *mut c_void, code, "delete"));
        }
        Ok(())
    }

    async fn ensure_dir(&self, path: &Path) -> StorageResult<()> {
        if let Some(ed) = self.abi.ensure_dir {
            let th = self.th;
            let rel = self.rel_cstr(path)?;
            let code = self.submit_blocking(move || {
                let th = th as *mut c_void;
                unsafe { ed(th, rel.as_ptr()) }
            })
            .await
            .map_err(|e| StorageError::Other(e.to_string()))?;
            if code != 0 {
                return Err(self.err_from(self.th as *mut c_void, code, "ensure_dir"));
            }
        }
        Ok(())
    }

    async fn ping(&self) -> StorageResult<()> {
        if let Some(p) = self.abi.ping {
            let th = self.th;
            let code = self.submit_blocking(move || {
                let th = th as *mut c_void;
                unsafe { p(th) }
            })
            .await
            .map_err(|e| StorageError::Other(e.to_string()))?;
            if code != 0 {
                return Err(self.err_from(self.th as *mut c_void, code, "ping"));
            }
        }
        Ok(())
    }

    fn plan_upload(&self) -> Option<&dyn PlanUpload> {
        // 启用并发回传的三要件：插件声明支持、实现了 plan_begin/plan_next、
        // 且并发度 ≥2（并发度来自本插件的用户配置，见 `CApiTarget::caps_for`）。
        if self.caps.supports_plan
            && self.caps.max_parallel >= 2
            && self.abi.plan_begin.is_some()
            && self.abi.plan_next.is_some()
        {
            Some(self)
        } else {
            None
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 并发回传（计划式）：把「传哪些、一次传几批」的决定权交给目标插件
// ════════════════════════════════════════════════════════════════════════════

impl PlanUpload for AbiTargetStorage {
    fn max_parallel(&self) -> usize {
        if self.caps.max_parallel == 0 {
            4
        } else {
            self.caps.max_parallel as usize
        }
    }

    fn begin(&self, manifest: &[FileDescriptor]) -> StorageResult<Box<dyn PlanSession>> {
        let begin = match (self.abi.plan_begin, self.abi.plan_next) {
            (Some(b), Some(_)) => b,
            _ => {
                return Err(StorageError::Protocol(format!(
                    "{}: 目标声明 supports_plan 但未实现 plan_begin/plan_next",
                    self.name
                )))
            }
        };
        // 清单：只给待传文件（目录不传），带 size/mtime 供插件做批次规划
        let items: Vec<serde_json::Value> = manifest
            .iter()
            .filter(|fd| !fd.is_dir)
            .map(|fd| {
                let mtime = fd
                    .modified
                    .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                serde_json::json!({
                    "rel_path": fd.rel_path,
                    "size": fd.size,
                    "mtime_secs": mtime,
                })
            })
            .collect();
        let job = serde_json::json!({ "upload": items }).to_string();
        let c =
            CString::new(job).map_err(|_| StorageError::Protocol("job_json 含非法字节".into()))?;
        let th = self.th as *mut c_void;
        // SAFETY: plan_begin 由插件导出；th 是 target_open 发放的实例句柄
        let ph = unsafe { begin(th, c.as_ptr()) };
        if ph.is_null() {
            return Err(self.err_from(th, -1, "plan_begin"));
        }
        Ok(Box::new(AbiPlanSession {
            abi: self.abi,
            th: self.th,
            // 句柄以 usize 保存：裸指针非 Send，而会话需跨 await 持有（见 PlanSession）
            ph: ph as usize,
        }))
    }
}

/// 一次规划会话：持有插件发放的计划句柄，**Drop 时自动 `plan_end`**
/// （提前 break / 出错返回也能结束规划，避免插件侧状态泄漏）
struct AbiPlanSession {
    abi: &'static KzwrTargetAbi,
    th: usize,
    ph: usize,
}

impl PlanSession for AbiPlanSession {
    fn next_batch(&mut self) -> StorageResult<Vec<String>> {
        let Some(next) = self.abi.plan_next else {
            return Ok(Vec::new());
        };
        let th = self.th as *mut c_void;
        // SAFETY: ph 由 plan_begin 发放，本会话是其唯一持有者
        let p = unsafe { next(th, self.ph as *mut c_void) };
        if p.is_null() {
            return Ok(Vec::new());
        }
        let body = take_cstring(p, self.abi).unwrap_or_default();
        if body.is_empty() {
            return Ok(Vec::new());
        }
        serde_json::from_str::<Vec<String>>(&body)
            .map_err(|e| StorageError::Protocol(format!("plan_next 返回无法解析: {e}")))
    }
}

impl Drop for AbiPlanSession {
    fn drop(&mut self) {
        if let Some(end) = self.abi.plan_end {
            let th = self.th as *mut c_void;
            // SAFETY: ph 由 plan_begin 发放，Drop 是最后一次使用
            unsafe { end(th, self.ph as *mut c_void) };
        }
    }
}

/// `list_json` 单条条目
#[derive(Debug, Default, Deserialize)]
struct AbiEntry {
    #[serde(default)]
    rel_path: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    mtime_secs: u64,
    #[serde(default)]
    is_dir: bool,
}

impl AbiEntry {
    fn into_fd(self) -> FileDescriptor {
        FileDescriptor {
            rel_path: self.rel_path,
            size: self.size,
            modified: (self.mtime_secs > 0)
                .then(|| SystemTime::UNIX_EPOCH + Duration::from_secs(self.mtime_secs)),
            is_dir: self.is_dir,
            digest: None,
        }
    }
}

#[cfg(test)]
mod tests {
    //! 并发度解析的单元测试（这里正是「改一个目标、同类型目标全变」的 bug 所在处）
    use super::*;
    use crate::infra::config::{AppConfig, ConfigManager, TargetConfig};
    use crate::plugin::abi::{KzwrTargetAbi, C_ABI_VERSION};

    // ── 最小桩：只为让 CApiTarget 能构造（不真的调用任何回调）─────────────
    extern "C" fn stub_open(_: *const std::os::raw::c_char) -> *mut std::os::raw::c_void {
        std::ptr::null_mut()
    }
    extern "C" fn stub_write_begin(
        _: *mut std::os::raw::c_void,
        _: *const std::os::raw::c_char,
        _: u64,
    ) -> *mut std::os::raw::c_void {
        std::ptr::null_mut()
    }
    extern "C" fn stub_write_chunk(
        _: *mut std::os::raw::c_void,
        _: *mut std::os::raw::c_void,
        _: *const u8,
        _: u32,
    ) -> i32 {
        0
    }
    extern "C" fn stub_write_end(_: *mut std::os::raw::c_void, _: *mut std::os::raw::c_void) -> i64 {
        0
    }
    extern "C" fn stub_read_begin(
        _: *mut std::os::raw::c_void,
        _: *const std::os::raw::c_char,
    ) -> *mut std::os::raw::c_void {
        std::ptr::null_mut()
    }
    extern "C" fn stub_read_chunk(
        _: *mut std::os::raw::c_void,
        _: *mut std::os::raw::c_void,
        _: *mut u8,
        _: u32,
    ) -> i32 {
        0
    }
    extern "C" fn stub_list(_: *mut std::os::raw::c_void, _: *const std::os::raw::c_char) -> *mut std::os::raw::c_char {
        std::ptr::null_mut()
    }
    extern "C" fn stub_delete(_: *mut std::os::raw::c_void, _: *const std::os::raw::c_char) -> i32 {
        0
    }
    extern "C" fn stub_plan_begin(
        _: *mut std::os::raw::c_void,
        _: *const std::os::raw::c_char,
    ) -> *mut std::os::raw::c_void {
        std::ptr::null_mut()
    }
    extern "C" fn stub_plan_next(_: *mut std::os::raw::c_void, _: *mut std::os::raw::c_void) -> *mut std::os::raw::c_char {
        std::ptr::null_mut()
    }
    /// 释放桩返回的字符串（本测试不真的返回字符串，故只做空指针防护）
    extern "C" fn stub_free_str(p: *mut std::os::raw::c_char) {
        if !p.is_null() {
            unsafe { drop(std::ffi::CString::from_raw(p)) };
        }
    }

    fn stub_table() -> &'static KzwrTargetAbi {
        static T: std::sync::OnceLock<KzwrTargetAbi> = std::sync::OnceLock::new();
        T.get_or_init(|| KzwrTargetAbi {
            abi: C_ABI_VERSION,
            size: KzwrTargetAbi::REQUIRED_SIZE,
            target_open: stub_open,
            target_close: None,
            write_begin: stub_write_begin,
            write_chunk: stub_write_chunk,
            write_end: stub_write_end,
            write_abort: None,
            read_begin: stub_read_begin,
            read_chunk: stub_read_chunk,
            read_end: None,
            list_json: stub_list,
            delete: stub_delete,
            ensure_dir: None,
            ping: None,
            test_json: None,
            last_error_json: None,
            plan_begin: Some(stub_plan_begin),
            plan_next: Some(stub_plan_next),
            plan_end: None,
            free_str: stub_free_str,
        })
    }

    fn target(id: &str, parallel: Option<u32>) -> TargetConfig {
        TargetConfig {
            id: id.to_string(),
            kind: "webdav".to_string(),
            parallel,
            ..Default::default()
        }
    }

    fn cfg_with(plugin_parallel: Option<u32>) -> (tempfile::TempDir, ConfigManager, AppConfig) {
        let d = tempfile::tempdir().expect("tempdir");
        let m = ConfigManager::new(d.path(), crate::infra::keystore::secret("test-pass"));
        let mut cfg = AppConfig::default();
        if let Some(v) = plugin_parallel {
            cfg.plugins.target_parallel.insert("webdav".to_string(), v);
        }
        m.save(&cfg).expect("save");
        (d, m, cfg)
    }

    /// **核心回归**：同一个插件的两个目标，并发度必须各自独立
    #[test]
    fn two_targets_of_same_plugin_get_independent_parallel() {
        let (_d, m, _cfg) = cfg_with(Some(4));
        let t = CApiTarget::from_static(
            stub_table(),
            crate::plugin::api::PluginMeta {
                id: "webdav".to_string(),
                name: "WebDAV".to_string(),
                version: "1".to_string(),
                kind: crate::plugin::api::PluginKind::Target,
                builtin: true,
                description: String::new(),
            },
            AbiTargetCaps {
                supports_plan: true,
                max_parallel: 0,
                preferred_chunk_kib: 1024,
                needs_credentials: true,
                url_label: None,
                url_placeholder: None,
                url_hint: None,
                form: Vec::new(),
                path_fields: Vec::new(),
            },
            None,
        );
        let a = t.caps_for(&target("a", Some(2)), &m);
        let b = t.caps_for(&target("b", Some(6)), &m);
        assert_eq!(a.max_parallel, 2, "目标 a 应用自己的 2");
        assert_eq!(b.max_parallel, 6, "目标 b 应用自己的 6");
        assert_ne!(a.max_parallel, b.max_parallel, "两个目标必须互不影响");
        assert!(a.supports_plan && b.supports_plan, "≥2 都应启用并发回传");
    }

    /// 目标未设置 → 回退到插件级旧值（保证老配置的并发设置不失效）
    #[test]
    fn unset_target_falls_back_to_plugin_level() {
        let (_d, m, _cfg) = cfg_with(Some(3));
        let t = CApiTarget::from_static(
            stub_table(),
            crate::plugin::api::PluginMeta {
                id: "webdav".to_string(),
                name: "WebDAV".to_string(),
                version: "1".to_string(),
                kind: crate::plugin::api::PluginKind::Target,
                builtin: true,
                description: String::new(),
            },
            AbiTargetCaps {
                supports_plan: true,
                max_parallel: 0,
                preferred_chunk_kib: 1024,
                needs_credentials: true,
                url_label: None,
                url_placeholder: None,
                url_hint: None,
                form: Vec::new(),
                path_fields: Vec::new(),
            },
            None,
        );
        let c = t.caps_for(&target("x", None), &m);
        assert_eq!(c.max_parallel, 3, "未设置时应回退到插件级");
    }

    /// 目标显式设为 0（顺序上传）时，不能因为插件级是 4 就被改成并发
    #[test]
    fn explicit_zero_beats_plugin_level_fallback() {
        let (_d, m, _cfg) = cfg_with(Some(4));
        let t = CApiTarget::from_static(
            stub_table(),
            crate::plugin::api::PluginMeta {
                id: "webdav".to_string(),
                name: "WebDAV".to_string(),
                version: "1".to_string(),
                kind: crate::plugin::api::PluginKind::Target,
                builtin: true,
                description: String::new(),
            },
            AbiTargetCaps {
                supports_plan: true,
                max_parallel: 0,
                preferred_chunk_kib: 1024,
                needs_credentials: true,
                url_label: None,
                url_placeholder: None,
                url_hint: None,
                form: Vec::new(),
                path_fields: Vec::new(),
            },
            None,
        );
        let c = t.caps_for(&target("x", Some(0)), &m);
        assert_eq!(c.max_parallel, 0, "显式 0 应生效（顺序上传）");
        assert!(!c.supports_plan, "0/1 应关闭并发回传");
    }

    /// 并发度不得超过宿主上限（防插件被配置成几百路把连接打满）
    #[test]
    fn parallel_is_capped_at_max() {
        let (_d, m, _cfg) = cfg_with(None);
        let t = CApiTarget::from_static(
            stub_table(),
            crate::plugin::api::PluginMeta {
                id: "webdav".to_string(),
                name: "WebDAV".to_string(),
                version: "1".to_string(),
                kind: crate::plugin::api::PluginKind::Target,
                builtin: true,
                description: String::new(),
            },
            AbiTargetCaps {
                supports_plan: true,
                max_parallel: 0,
                preferred_chunk_kib: 1024,
                needs_credentials: true,
                url_label: None,
                url_placeholder: None,
                url_hint: None,
                form: Vec::new(),
                path_fields: Vec::new(),
            },
            None,
        );
        let c = t.caps_for(&target("x", Some(999)), &m);
        assert_eq!(c.max_parallel, MAX_PARALLEL, "应被夹到上限 8");
    }
}

#[cfg(test)]
mod list_parse_tests {
    use super::*;

    /// **恢复假成功回归**：坏 JSON 必须报错，不能当空列表
    ///
    /// 旧实现 `serde_json::from_str(...).unwrap_or_default()` 会把解析失败
    /// 变成空 `Vec`，于是 `restore` 认为「云端没有文件」并**报告成功**。
    #[test]
    fn malformed_json_is_an_error_not_an_empty_list() {
        let r = parse_list_body("{这不是合法 JSON");
        assert!(r.is_err(), "坏 JSON 必须报错（否则恢复会假成功）");
        let msg = r.unwrap_err().to_string();
        assert!(msg.contains("无法解析"), "错误信息应说明解析失败：{msg}");
    }

    /// 空体是合法的「空目录」，不该报错
    #[test]
    fn empty_body_is_a_valid_empty_directory() {
        assert!(parse_list_body("").unwrap().is_empty());
        assert!(parse_list_body("   ").unwrap().is_empty());
    }

    /// 正常数组能解析出条目
    #[test]
    fn valid_array_parses() {
        let body = r#"[{"rel_path":"a.age","size":10,"mtime_secs":0,"is_dir":false}]"#;
        let v = parse_list_body(body).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rel_path, "a.age");
    }
}
