//! **稳定插件 ABI v1**（外置插件的「冻结契约」）
//!
//! ## 为什么需要它
//!
//! 直接传 Rust trait 对象（`Box<dyn EnhancePlugin>`）虽然省事，但 Rust **没有稳定 ABI**：
//! vtable 布局、结构体字段排布都随编译器与源码变化 → 插件必须与宿主同版本编译，
//! 每次升级主程序都要重编插件。本模块把跨边界契约降级为**版本化的 C ABI + UTF-8 JSON**：
//!
//! - 插件只依赖这份契约（`plugins/sdk`，零第三方依赖），**宿主升级不需要重编插件**
//! - 数据一律用 JSON 字符串传递（宿主与插件各自用自己的类型解析，未知字段互相忽略）
//! - 只要 [`C_ABI_VERSION`] 不变，兼容性就成立；破坏性改动才 +1（届时宿主可同时支持 v1/v2）
//!
//! ## 契约（插件导出）
//!
//! | 符号 | 说明 |
//! |------|------|
//! | `fn_kzwr_plugin_abi_v1` | 入口：`extern "C" fn() -> *const KzwrPluginAbi`，返回**插件持有的静态表** |
//!
//! 表内回调（均可选实现，返回 `char*` 时宿主负责用 `free_str` 释放）：
//!
//! | 回调 | 入参 | 返回 JSON |
//! |------|------|-----------|
//! | `describe_json` | — | `{"id","name","version","kind","description","caps":{…},"ui":{…}｜null}` |
//! | `available_json` | `cfg_json` | `{"available":bool,"reason":string｜null}` |
//! | `action_json` | `action`, `request_json` | 任意 JSON（如 `{"success":true,"message":"…"}` / `{"error":"…"}`） |
//! | `health_json` | `cfg_json` | `[{"key","title","status":"ok|warn|fail","detail","hint"}]` |
//! | `event_json`（可选） | `event`, `cfg_json` | 任意 JSON（`startup`/`patrol`/`after_backup`/`reload`） |
//! | `free_str` | — | 释放插件返回的字符串（宿主必须调用） |
//! | `destroy`（可选） | — | 卸载插件自身状态 |
//!
//! `request_json`（动作入参）固定为：
//!
//! ```json
//! { "body": { …前端提交的 JSON（GET 时是 query 键值对）… }, "cfg": { …配置快照… } }
//! ```
//!
//! ## 约定
//!
//! - 字符串一律 **UTF-8 + NUL 结尾**；返回空指针（NULL）表示"无内容"
//! - **所有权**：插件返回的字符串归宿主，宿主必须调用该表的 `free_str` 释放
//! - **不要 panic 跨 FFI**（Rust 插件请自行 `catch_unwind`）；宿主也会兜一层 `catch_unwind`
//! - `size` 用于向前兼容：结构体只能**追加**字段，宿主按自己的已知长度读取
//! - v1 只支持**增强插件**（UI 卡片/动作/体检/事件）；自定义备份目标需要用 Rust 直连路径
//!
//! 同样的契约在插件侧由 `plugins/sdk` 提供（`KzwrPluginAbi` + `export_plugin_v1!` 宏）。

use std::os::raw::c_char;

use serde::Deserialize;

/// C ABI 版本：**仅破坏性改动 +1**；不变则插件无需随宿主升级重编译
pub const C_ABI_VERSION: u32 = 1;

/// 入口符号名（插件导出）
pub const SYM_ENTRY_V1: &[u8] = b"fn_kzwr_plugin_abi_v1\0";

/// 回调类型别名（返回 JSON 字符串，宿主用 `free_str` 释放）
pub type JsonFn0 = extern "C" fn() -> *mut c_char;
pub type JsonFn1 = extern "C" fn(*const c_char) -> *mut c_char;
pub type JsonFn2 = extern "C" fn(*const c_char, *const c_char) -> *mut c_char;

/// 插件提供的静态函数表（`#[repr(C)]`：布局固定，只允许**尾部追加**字段）
#[repr(C)]
pub struct KzwrPluginAbi {
    /// 必须 == [`C_ABI_VERSION`]
    pub abi: u32,
    /// 本结构体字节大小（宿主据此判断尾部可选字段是否存在）
    pub size: u32,
    /// 插件元信息与 UI 描述
    pub describe_json: JsonFn0,
    /// 是否可用（依赖配置时用）
    pub available_json: JsonFn1,
    /// 动作：`POST/GET /api/p/<插件id>/<action>`
    pub action_json: JsonFn2,
    /// 「一键体检」自检项
    pub health_json: JsonFn1,
    /// 生命周期事件（可选）
    pub event_json: Option<JsonFn2>,
    /// 释放插件返回的字符串
    pub free_str: extern "C" fn(*mut c_char),
    /// 卸载/清理（可选）
    pub destroy: Option<extern "C" fn()>,
}

impl KzwrPluginAbi {
    /// 宿主已知的表长度（判断可选字段是否可用；本版全部字段均为必需，`destroy` 用 `Option`）
    pub const KNOWN_SIZE: u32 = std::mem::size_of::<Self>() as u32;
}

/// `describe_json` 的解析结果
#[derive(Debug, Clone, Deserialize)]
pub struct AbiDescribe {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// v1 仅支持 `enhance`（缺省视为 enhance）
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub caps: AbiCaps,
    #[serde(default)]
    pub ui: Option<crate::plugin::api::PluginUi>,
}

/// `caps` 字段（与 [`crate::plugin::api::EnhanceCaps`] 同形，单独定义以免依赖宿主内部类型布局）
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct AbiCaps {
    #[serde(default)]
    pub account: bool,
    #[serde(default)]
    pub quota: bool,
    #[serde(default)]
    pub recycle_bin: bool,
    #[serde(default)]
    pub notify: bool,
}

impl From<AbiCaps> for crate::plugin::api::EnhanceCaps {
    fn from(c: AbiCaps) -> Self {
        Self {
            account: c.account,
            quota: c.quota,
            recycle_bin: c.recycle_bin,
            notify: c.notify,
        }
    }
}

/// 传给插件的配置快照（`cfg_json`）
///
/// 刻意**不含任何凭据**（口令 / token / 账号名都不传）：目标只给「是否启用、是否已就绪」，
/// 插件确有需要时应引导用户在自己的配置里单独填写。
#[derive(Default, Clone)]
pub struct CfgSnapshot {
    /// 宿主版本（插件可用于日志/兼容判断）
    pub host_version: String,
    /// 宿主时区说明
    pub timezone: String,
    /// 宿主时区相对 UTC 的分钟偏移
    pub utc_offset_minutes: i64,
    /// 目标（id/名称/类型/地址/是否启用/是否就绪/是否主目标/账号名）
    pub targets: Vec<CfgTarget>,
    /// 任务（id/名称/是否启用/源路径/目标/目录/定时）
    pub tasks: Vec<CfgTask>,
    /// kzwr access-token 是否已配置（增强能力是否可用）
    pub kzwr_token_configured: bool,
}

/// 配置快照里的目标
#[derive(Default, Clone)]
pub struct CfgTarget {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub url: String,
    pub enabled: bool,
    pub ready: bool,
    pub primary: bool,
}

/// 配置快照里的任务
#[derive(Default, Clone)]
pub struct CfgTask {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub paths: Vec<String>,
    pub target_id: String,
    pub target_folder: String,
    pub schedule_cron: String,
}

impl CfgSnapshot {
    /// 由配置构造快照（只看配置本身，不读运行时状态 → 各处调用口径一致）
    pub fn from_config(cfg: &crate::infra::config::AppConfig) -> Self {
        let primary = cfg
            .primary_target()
            .map(|t| t.id.clone())
            .unwrap_or_default();
        Self {
            host_version: crate::plugin::sdk::HOST_VERSION.to_string(),
            timezone: crate::domain::scheduler::timezone_label(),
            utc_offset_minutes: crate::domain::scheduler::utc_offset_minutes(),
            targets: cfg
                .targets
                .iter()
                .map(|t| CfgTarget {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    kind: t.kind.clone(),
                    url: t.url.clone().unwrap_or_default(),
                    enabled: t.enabled,
                    // 「就绪」= 启用且凭据齐备（与 /api/targets 的语义一致）
                    ready: t.enabled && t.configured(),
                    primary: t.id == primary,
                })
                .collect(),
            tasks: cfg
                .tasks
                .iter()
                .map(|t| CfgTask {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    enabled: t.enabled,
                    paths: t.paths.clone(),
                    target_id: t.target_id.clone(),
                    target_folder: t.target_folder.clone(),
                    schedule_cron: t.schedule_cron.clone().unwrap_or_default(),
                })
                .collect(),
            kzwr_token_configured: cfg.kzwr.access_token_enc.is_some(),
        }
    }

    /// 序列化为稳定的 `cfg_json`（字段只增不改；插件忽略不认识的键即可）
    pub fn to_json(&self) -> String {
        let targets: Vec<serde_json::Value> = self
            .targets
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "name": t.name,
                    "kind": t.kind,
                    "url": t.url,
                    "enabled": t.enabled,
                    "ready": t.ready,
                    "primary": t.primary,
                })
            })
            .collect();
        let tasks: Vec<serde_json::Value> = self
            .tasks
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "name": t.name,
                    "enabled": t.enabled,
                    "paths": t.paths,
                    "target_id": t.target_id,
                    "target_folder": t.target_folder,
                    "schedule_cron": t.schedule_cron,
                })
            })
            .collect();
        serde_json::json!({
            "abi_version": C_ABI_VERSION,
            "host_version": self.host_version,
            "timezone": self.timezone,
            "utc_offset_minutes": self.utc_offset_minutes,
            "targets": targets,
            "tasks": tasks,
            "enhance": { "kzwr_token_configured": self.kzwr_token_configured },
        })
        .to_string()
    }
}
