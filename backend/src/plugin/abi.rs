//! **稳定插件 ABI v1**（外置与内置插件共用的「冻结契约」）
//!
//! ## 为什么需要它
//!
//! 直接传 Rust trait 对象虽然省事，但 Rust **没有稳定 ABI**：vtable 布局、结构体字段排布
//! 都随编译器与源码变化 → 插件必须与宿主同版本编译。本模块把跨边界契约降级为
//! **版本化的 C ABI + UTF-8 JSON**：
//!
//! - 插件只依赖这份契约（`plugins/sdk`，零第三方依赖），**宿主升级不需要重编插件**
//! - 数据一律用 JSON 字符串传递（宿主与插件各自用自己的类型解析，未知字段互相忽略）
//! - **内置插件也走这套契约**（编译进主程序，直接提供静态表）→ 内置/外置零差异
//!
//! ## 契约总览
//!
//! | 符号 | 说明 |
//! |------|------|
//! | `fn_kzwr_plugin_abi_v1` | 入口：`extern "C" fn() -> *const KzwrPluginAbi`（**唯一必需导出**） |
//! | `fn_kzwr_plugin_target_v1` | 目标能力入口（可选）：`extern "C" fn() -> *const KzwrTargetAbi` |
//!
//! 主表 [`KzwrPluginAbi`] 回调（`available`/`action`/`health`/`event` 允许为 NULL = 未实现）：
//!
//! | 回调 | 入参 | 返回 JSON |
//! |------|------|-----------|
//! | `describe_json` | — | `{"id","name","version","kind","description","caps":{…},"runtime":{…},"target":{…},"ui":{…}｜null}` |
//! | `available_json` | `cfg_json` | `{"available":bool,"reason":string｜null}` |
//! | `action_json` | `action`, `request_json` | 任意 JSON（如 `{"success":true,"message":"…"}` / `{"error":"…"}`） |
//! | `health_json` | `cfg_json` | `[{"key","title","status":"ok|warn|fail","detail","hint"}]` |
//! | `event_json` | `event`, `cfg_json` | `{"count":u64?, "alerts":[{"level","message"}]?}` |
//! | `free_str` | — | 释放插件返回的字符串（宿主必须调用） |
//! | `destroy` | — | 卸载插件自身状态（可选） |
//!
//! `request_json`（动作入参）固定为：
//!
//! ```json
//! { "body": { …前端提交的 JSON（GET 时是 query 键值对）… }, "cfg": { …配置快照… } }
//! ```
//!
//! **告警是「声明式回传」**：插件不回调宿主，而是在 `health_json` / `event_json` 的返回值里
//! 带上 `alerts` 数组，由宿主负责去重与落库（见 `cabi::apply_alerts`）。
//!
//! ## 约定
//!
//! - 字符串一律 **UTF-8 + NUL 结尾**；返回空指针（NULL）表示"无内容"
//! - **所有权**：插件返回的字符串归宿主，宿主必须调用该表的 `free_str` 释放
//! - **不要 panic 跨 FFI**（Rust 插件请自行 `catch_unwind`）；宿主也会兜一层 `catch_unwind`
//! - **`size` 语义**：主表**冻结**（`KzwrPluginAbi` 不再增长），新增能力一律走**独立能力表**
//!   （如 [`KzwrTargetAbi`]），因此 `size` 只需 ≥ [`KzwrPluginAbi::REQUIRED_SIZE`]
//! - **`abi`**：破坏性改动才 +1（如改回调签名、改必需字段语义）
//!
//! 同样的契约在插件侧由 `plugins/sdk` 提供（`KzwrPluginAbi` + `export_plugin_v1!` / `export_target_v1!` 宏）。

use std::ffi::c_void;
use std::os::raw::c_char;

use serde::Deserialize;

/// C ABI 版本：**仅破坏性改动 +1**；不变则插件无需随宿主升级重编译
pub const C_ABI_VERSION: u32 = 1;

/// 宿主版本（插件可用于日志/兼容判断；`cfg_json` 里也会给）
pub const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 入口符号名（插件导出，必需）
pub const SYM_ENTRY_V1: &[u8] = b"fn_kzwr_plugin_abi_v1\0";

/// 目标能力入口符号名（插件导出，可选；`describe_json.runtime.target` 可覆盖）
pub const SYM_TARGET_V1: &str = "fn_kzwr_plugin_target_v1";

/// 回调类型别名（返回 JSON 字符串，宿主用 `free_str` 释放）
pub type JsonFn0 = extern "C" fn() -> *mut c_char;
pub type JsonFn1 = extern "C" fn(*const c_char) -> *mut c_char;
pub type JsonFn2 = extern "C" fn(*const c_char, *const c_char) -> *mut c_char;

/// 插件提供的静态函数表（`#[repr(C)]`：布局固定，**冻结不再增长**）
///
/// 只提供目标能力的插件只需实现 `describe_json` + `free_str`（其余回调留 NULL）。
#[repr(C)]
pub struct KzwrPluginAbi {
    /// 必须 == [`C_ABI_VERSION`]
    pub abi: u32,
    /// 本结构体字节大小（宿主据此校验前缀是否齐全）
    pub size: u32,
    /// 插件元信息、UI 描述、能力声明（**必需**）
    pub describe_json: JsonFn0,
    /// 是否可用（依赖配置时用）
    pub available_json: Option<JsonFn1>,
    /// 动作：`/api/p/<插件id>/<action>`（`action` 可为多段，如 `trash/empty`）
    pub action_json: Option<JsonFn2>,
    /// 「一键体检」自检项
    pub health_json: Option<JsonFn1>,
    /// 生命周期事件（`startup`/`patrol`/`after_backup`/`reload`）
    pub event_json: Option<JsonFn2>,
    /// 释放插件返回的字符串（**必需**）
    pub free_str: extern "C" fn(*mut c_char),
    /// 卸载/清理（可选；宿主删除插件前调用）
    pub destroy: Option<extern "C" fn()>,
}

impl KzwrPluginAbi {
    /// 宿主期望的主表长度（主表冻结 ⇒ 等于必需前缀长度）
    pub const REQUIRED_SIZE: u32 = std::mem::size_of::<Self>() as u32;
}

// ── 目标能力表（独立符号 → 独立演进）──────────────────────────────────────

/// 目标能力表：让插件提供**自定义备份目标**
///
/// ## 数据流（推块模式）
///
/// 宿主读明文 → age 加密 → `write_begin` / `write_chunk` / `write_end` 把**密文**喂给插件；
/// 插件**不回调宿主**（故无需 host vtable），明文与密钥永不离开宿主。
///
/// ## 关键约定
///
/// - **实例句柄 `th`**：多目标/多任务下同一插件可有多实例，`target_open` 每实例调用一次
/// - **文件句柄 `h`**：一次传输的上下文；同一 `th` 下允许多个 `h` **并发**（上限见 `describe.target.max_parallel`）
/// - **错误码**：`int < 0` 表示失败，宿主随后调用 `last_error_json` 取详情
/// - **字节上报**：`write_end` 返回该文件**实际写入的密文字节数**，宿主会与已喂出的字节数比对
/// - `list_json` 返回 `[{"rel_path","size","mtime_secs","is_dir"}]`
/// - 返回字符串同样由 `free_str` 释放；`*mut c_void` 句柄由插件负责释放
#[repr(C)]
pub struct KzwrTargetAbi {
    /// 本表 ABI 版本（与 [`C_ABI_VERSION`] 同规则）
    pub abi: u32,
    /// 本表字节大小（尾部追加字段时用于探测，宿主按需读取）
    pub size: u32,

    // ── 实例生命周期 ────────────────────────────────────────────────
    /// 用 `target_json`（含该目标凭据与插件自管配置）创建实例；返回 NULL = 配置无效
    pub target_open: extern "C" fn(*const c_char) -> *mut c_void,
    /// 释放实例（宿主刷新目标池/退出时调用）
    pub target_close: Option<extern "C" fn(*mut c_void)>,

    // ── 传输（宿主 → 插件，密文） ──────────────────────────────────
    /// 开始写入一个文件；`rel_path` 为目标端相对路径（源根下），`total` 为密文总字节数（未知为 0）
    pub write_begin: extern "C" fn(*mut c_void, *const c_char, u64) -> *mut c_void,
    /// 推入一块密文；返回写入字节数（≥0）或负错误码
    pub write_chunk: extern "C" fn(*mut c_void, *mut c_void, *const u8, u32) -> i32,
    /// 结束写入；返回**实际写入的密文字节数**（<0 = 错误码）
    pub write_end: extern "C" fn(*mut c_void, *mut c_void) -> i64,
    /// 中止写入（丢弃半成品；可选）
    pub write_abort: Option<extern "C" fn(*mut c_void, *mut c_void)>,

    // ── 读取（插件 → 宿主，密文；用于恢复） ────────────────────────
    pub read_begin: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    /// 读一块密文；返回读到的字节数（>0）、0 = EOF、<0 = 错误码
    pub read_chunk: extern "C" fn(*mut c_void, *mut c_void, *mut u8, u32) -> i32,
    /// 结束读取（可选）
    pub read_end: Option<extern "C" fn(*mut c_void, *mut c_void) -> i32>,

    // ── 目录与元数据 ───────────────────────────────────────────────
    /// 按前缀列出条目（JSON 数组）
    pub list_json: extern "C" fn(*mut c_void, *const c_char) -> *mut c_char,
    /// 删除文件/目录：0 = 成功，非 0 = 失败
    pub delete: extern "C" fn(*mut c_void, *const c_char) -> i32,
    /// 确保目录存在（可选）
    pub ensure_dir: Option<extern "C" fn(*mut c_void, *const c_char) -> i32>,
    /// 连通性测试（可选）：0 = 成功
    pub ping: Option<extern "C" fn(*mut c_void) -> i32>,
    /// 保存前的连通性测试（实例尚未建立时用；入参 `target_json`）
    pub test_json: Option<extern "C" fn(*const c_char) -> *mut c_char>,

    // ── 插件自管配置（宿主代加密存储，命名空间 = 插件 id） ──────────
    /// 读一个键（返回 JSON 字符串或裸字符串；NULL = 不存在）
    pub config_get: Option<extern "C" fn(*const c_char) -> *mut c_char>,
    /// 写一个键：0 = 成功
    pub config_set: Option<extern "C" fn(*const c_char, *const c_char) -> i32>,

    /// 最近一次错误的详情（JSON）
    pub last_error_json: Option<extern "C" fn(*mut c_void) -> *mut c_char>,

    // ── 并发回传（可选能力：`describe.target.supports_plan = true` 时宿主采用）──
    /// 开始规划：`job_json.upload` 给出待传清单（rel_path/size/mtime）
    pub plan_begin: Option<extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>,
    /// 下一批要传的文件（JSON 数组，如 `["a.txt","b/c.bin"]`；`[]` = 清单已空）
    pub plan_next: Option<extern "C" fn(*mut c_void, *mut c_void) -> *mut c_char>,
    /// 结束规划
    pub plan_end: Option<extern "C" fn(*mut c_void, *mut c_void)>,

    /// 释放本表返回的字符串（必需）
    pub free_str: extern "C" fn(*mut c_char),
}

impl KzwrTargetAbi {
    /// 本表必需前缀长度
    pub const REQUIRED_SIZE: u32 = std::mem::size_of::<Self>() as u32;
}

// ── describe_json 解析 ────────────────────────────────────────────────────

/// `describe_json` 的解析结果
#[derive(Debug, Clone, Deserialize)]
pub struct AbiDescribe {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// `enhance`（缺省）| `target`
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub caps: AbiCaps,
    /// 能力表发现（当前只用到 `target`）
    #[serde(default)]
    pub runtime: AbiRuntime,
    /// 目标能力声明
    #[serde(default)]
    pub target: Option<AbiTargetCaps>,
    #[serde(default)]
    pub ui: Option<crate::plugin::api::PluginUi>,
}

/// 能力表及其入口符号名（缺省用 [`SYM_TARGET_V1`]）
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AbiRuntime {
    #[serde(default)]
    pub target: Option<String>,
}

impl AbiRuntime {
    /// 目标能力入口符号名
    pub fn target_symbol(&self) -> &str {
        self.target.as_deref().unwrap_or(SYM_TARGET_V1)
    }
}

/// 目标能力声明（`describe_json.target`）
#[derive(Debug, Clone, Deserialize)]
pub struct AbiTargetCaps {
    /// 是否支持并发回传（`plan_*`）；false/缺省 = 宿主按顺序推块
    #[serde(default)]
    pub supports_plan: bool,
    /// 并发上限（`supports_plan=true` 时生效；0 = 不限）
    #[serde(default)]
    pub max_parallel: u32,
    /// 宿主推荐的推送块大小（KiB；0 = 宿主默认 1024）
    #[serde(default)]
    pub preferred_chunk_kib: u32,
}

impl Default for AbiTargetCaps {
    fn default() -> Self {
        Self {
            supports_plan: false,
            max_parallel: 1,
            preferred_chunk_kib: 1024,
        }
    }
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

// ── 配置快照（cfg_json） ──────────────────────────────────────────────────

/// 传给插件的配置快照（`cfg_json`）
///
/// **不含密码 / token**：只给「是否启用、是否就绪」与用户名（供界面展示绑定账号）。
/// 插件自有凭据请走 [`KzwrTargetAbi::config_set`] 或 `target_json`。
#[derive(Default, Clone)]
pub struct CfgSnapshot {
    /// 宿主版本（插件可用于日志/兼容判断）
    pub host_version: String,
    /// 宿主时区说明
    pub timezone: String,
    /// 宿主时区相对 UTC 的分钟偏移
    pub utc_offset_minutes: i64,
    /// 目标（id/名称/类型/地址/用户名/是否启用/是否就绪/是否主目标）
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
    /// 解密后的用户名（**不含密码**；无法解密时为空串）
    pub username: String,
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
    ///
    /// 用户名需要解密，故此处留空；调用方可随后用 [`Self::with_target_usernames`] 填。
    pub fn from_config(cfg: &crate::infra::config::AppConfig) -> Self {
        let primary = cfg
            .primary_target()
            .map(|t| t.id.clone())
            .unwrap_or_default();
        Self {
            host_version: HOST_VERSION.to_string(),
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
                    username: String::new(),
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

    /// 填入目标用户名（`目标 id → 用户名`；不在表里的保持空串）
    pub fn with_target_usernames(
        mut self,
        users: &std::collections::HashMap<String, String>,
    ) -> Self {
        for t in &mut self.targets {
            if let Some(u) = users.get(&t.id) {
                t.username = u.clone();
            }
        }
        self
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
                    "username": t.username,
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
