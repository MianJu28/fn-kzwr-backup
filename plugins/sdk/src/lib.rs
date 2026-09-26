//! 酷族备份 · 外置插件**稳定 C ABI** SDK（v1）
//!
//! 用这个 crate 写插件可以做到：**宿主升级后不需要重新编译插件**（只要 ABI v1 不变）。
//! 原因：跨边界只传 **C 类型 + UTF-8 JSON**，不传 Rust trait 对象（Rust 没有稳定 ABI）。
//!
//! 本 crate **零第三方依赖**，把契约钉死在下面这一份结构体里：
//!
//! ```text
//! 插件（cdylib）                          宿主
//! ─────────────────────────────────────────────────────────────
//! fn_kzwr_plugin_abi_v1() ──► *const KzwrPluginAbi ──► 校验 abi/size
//!        describe_json()  ◄── 调用 ──  解析 {id,name,ui,caps}
//!        available_json(cfg)             {"available":bool}
//!        action_json(action, request)    任意 JSON（原样回给前端）
//!        health_json(cfg)                [{key,title,status,detail,hint}]
//!        event_json(event, cfg)          任意 JSON（可选实现）
//!        free_str(ptr)        ◄── 宿主释放插件返回的字符串
//! ```
//!
//! ## 最小示例
//!
//! ```ignore
//! use fn_kzwr_plugin_sdk as sdk;
//!
//! extern "C" fn describe() -> *mut std::os::raw::c_char {
//!     sdk::to_c_string(r#"{"id":"hello","name":"示例","ui":{"section":"settings","title":"示例","order":90,"blocks":[]}}"#)
//! }
//! extern "C" fn available(_cfg: *const std::os::raw::c_char) -> *mut std::os::raw::c_char {
//!     sdk::to_c_string(r#"{"available":true}"#)
//! }
//! extern "C" fn action(_a: *const std::os::raw::c_char, _r: *const std::os::raw::c_char) -> *mut std::os::raw::c_char {
//!     sdk::to_c_string(r#"{"success":true,"message":"hi"}"#)
//! }
//! extern "C" fn health(_cfg: *const std::os::raw::c_char) -> *mut std::os::raw::c_char {
//!     sdk::to_c_string("[]")
//! }
//!
//! sdk::export_plugin_v1!(describe, available, action, health);
//! ```
//!
//! ## 约定（违反会导致宿主拒绝或崩溃）
//!
//! - 字符串一律 **UTF-8 + NUL 结尾**，用 [`to_c_string`] 分配、由宿主的 `free_str` 释放
//! - 返回**空指针**表示"无内容"（宿主按空处理，不会崩）
//! - **不要 panic 跨 FFI**：Rust 插件请自行 `catch_unwind`，或保证不 panic
//! - 结构体字段**只能追加**（`size` 会告诉宿主实际长度）；宿主按自己的已知长度读取
//! - 回调需线程安全（宿主可能从不同线程调用）
//!
//! JSON 契约细节见仓库 `docs/PLUGIN_ABI.md`。

use std::ffi::CString;
use std::os::raw::c_char;

/// ABI 版本（与宿主保持一致；破坏性改动才会 +1）
pub const ABI_VERSION: u32 = 1;

/// 返回 JSON 字符串的无参回调
pub type JsonFn0 = extern "C" fn() -> *mut c_char;
/// 返回 JSON 字符串的单参回调（入参：`cfg_json`）
pub type JsonFn1 = extern "C" fn(*const c_char) -> *mut c_char;
/// 返回 JSON 字符串的双参回调（如 `action_json(action, request_json)`）
pub type JsonFn2 = extern "C" fn(*const c_char, *const c_char) -> *mut c_char;

/// 插件提供的静态函数表（**布局必须与宿主完全一致**）
#[repr(C)]
pub struct KzwrPluginAbi {
    /// 必须为 [`ABI_VERSION`]
    pub abi: u32,
    /// 本结构体字节大小
    pub size: u32,
    /// 元信息 + UI 描述（JSON）
    pub describe_json: JsonFn0,
    /// 是否可用（JSON：`{"available":true}`）
    pub available_json: JsonFn1,
    /// 动作：`POST/GET /api/p/<插件id>/<action>`（入参 `request_json` = `{"body":…,"cfg":…}`）
    pub action_json: JsonFn2,
    /// 「一键体检」自检项（JSON 数组）
    pub health_json: JsonFn1,
    /// 生命周期事件（`startup` / `patrol` / `after_backup` / `reload`），可选
    pub event_json: Option<JsonFn2>,
    /// 释放插件返回的字符串（宿主调用）
    pub free_str: extern "C" fn(*mut c_char),
    /// 卸载时清理，可选
    pub destroy: Option<extern "C" fn()>,
}

/// 把字符串转成插件返回给宿主的 C 字符串（内部 NUL 会被剔除）
pub fn to_c_string(s: impl AsRef<str>) -> *mut c_char {
    let cleaned: Vec<u8> = s
        .as_ref()
        .as_bytes()
        .iter()
        .copied()
        .filter(|b| *b != 0)
        .collect();
    match CString::new(cleaned) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// 释放插件返回的字符串（由宿主通过 `free_str` 回调调用）
///
/// # Safety
/// `p` 必须是 [`to_c_string`] 返回且**尚未释放**的指针，或空指针。
pub extern "C" fn free_c_string(p: *mut c_char) {
    if p.is_null() {
        return;
    }
    unsafe {
        drop(CString::from_raw(p));
    }
}

/// 读取宿主传入的 C 字符串（空指针 → 空串）
///
/// # Safety
/// `p` 必须是宿主传入的合法 NUL 结尾字符串，或空指针。
pub unsafe fn from_c_str(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
}

/// 导出插件（不带生命周期事件）
#[macro_export]
macro_rules! export_plugin_v1 {
    ($describe:path, $available:path, $action:path, $health:path $(,)?) => {
        $crate::export_plugin_v1!($describe, $available, $action, $health, None);
    };
    ($describe:path, $available:path, $action:path, $health:path, $event:expr $(,)?) => {
        /// 稳定 C ABI v1 入口：返回插件持有的静态函数表
        #[no_mangle]
        pub extern "C" fn fn_kzwr_plugin_abi_v1() -> *const $crate::KzwrPluginAbi {
            static TABLE: $crate::KzwrPluginAbi = $crate::KzwrPluginAbi {
                abi: $crate::ABI_VERSION,
                size: std::mem::size_of::<$crate::KzwrPluginAbi>() as u32,
                describe_json: $describe,
                available_json: $available,
                action_json: $action,
                health_json: $health,
                event_json: $event,
                free_str: $crate::free_c_string,
                destroy: None,
            };
            &TABLE
        }
    };
}

// ── 目标能力表（自定义备份目标）────────────────────────────────────────────
//
// 想提供新的备份目的地（对象存储 / 另一家网盘 / 本地目录 …）就实现这张表，
// 并用 [`export_target_v1!`] 导出 `fn_kzwr_plugin_target_v1`。
//
// ⚠️ **字段顺序与类型必须和宿主 `backend/src/plugin/abi.rs` 的 `KzwrTargetAbi` 完全一致**
// （两边独立定义，靠 `#[repr(C)]` 布局对齐；只能在尾部追加字段）。
//
// 数据模型：宿主读明文 → age 加密 → 把**密文**推给插件（推块），
// 插件**不接触明文与密钥**，也不需要回调宿主。恢复时插件给密文、宿主解密。

use std::os::raw::c_void;

/// 目标能力表（**布局与宿主严格一致**）
#[repr(C)]
pub struct KzwrTargetAbi {
    /// 必须为 [`ABI_VERSION`]
    pub abi: u32,
    /// 本结构体字节大小
    pub size: u32,

    // ── 实例生命周期 ──
    /// 用 `target_json`（含该目标凭据与插件自管配置）创建实例；返回 NULL = 配置无效
    pub target_open: extern "C" fn(*const c_char) -> *mut c_void,
    pub target_close: Option<extern "C" fn(*mut c_void)>,

    // ── 传输（宿主 → 插件，内容为 age 密文） ──
    /// `rel_path` 为**目标端**相对路径，`total` 为密文总字节（未知为 0）
    pub write_begin: extern "C" fn(*mut c_void, *const c_char, u64) -> *mut c_void,
    /// 返回实写字节数（≥0）或负错误码
    pub write_chunk: extern "C" fn(*mut c_void, *mut c_void, *const u8, u32) -> i32,
    /// 返回**实写密文字节总数**（宿主会与喂出的字节比对，防静默截断）
    pub write_end: extern "C" fn(*mut c_void, *mut c_void) -> i64,
    pub write_abort: Option<extern "C" fn(*mut c_void, *mut c_void)>,

    // ── 读取（插件 → 宿主，内容为 age 密文；用于恢复） ──
    pub read_begin: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    /// 返回读到的字节数（>0）、0 = EOF、<0 = 错误码
    pub read_chunk: extern "C" fn(*mut c_void, *mut c_void, *mut u8, u32) -> i32,
    pub read_end: Option<extern "C" fn(*mut c_void, *mut c_void) -> i32>,

    // ── 目录与元数据 ──
    /// `[{"rel_path","size","mtime_secs","is_dir"}]`
    pub list_json: extern "C" fn(*mut c_void, *const c_char) -> *mut c_char,
    pub delete: extern "C" fn(*mut c_void, *const c_char) -> i32,
    pub ensure_dir: Option<extern "C" fn(*mut c_void, *const c_char) -> i32>,
    pub ping: Option<extern "C" fn(*mut c_void) -> i32>,
    /// 设置页「测试连接」（实例尚未建立时）
    pub test_json: Option<extern "C" fn(*const c_char) -> *mut c_char>,

    // ── 插件自管配置（宿主代加密存储，命名空间 = 插件 id） ──
    pub config_get: Option<extern "C" fn(*const c_char) -> *mut c_char>,
    pub config_set: Option<extern "C" fn(*const c_char, *const c_char) -> i32>,

    /// 最近一次错误的详情（JSON）
    pub last_error_json: Option<extern "C" fn(*mut c_void) -> *mut c_char>,

    // ── 并发回传（可选；describe 的 `target.supports_plan=true` 时宿主采用） ──
    /// `job_json` = `{"upload":[{"rel_path","size","mtime_secs"}]}`
    pub plan_begin: Option<extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>,
    /// 下一批要传的**目标端路径**（`[]` = 清单已空）
    pub plan_next: Option<extern "C" fn(*mut c_void, *mut c_void) -> *mut c_char>,
    pub plan_end: Option<extern "C" fn(*mut c_void, *mut c_void)>,

    /// 释放本表返回的字符串（宿主调用）
    pub free_str: extern "C" fn(*mut c_char),
}

/// 导出**目标能力表**（自定义备份目标）
///
/// 参数按结构体字段顺序，**可选回调传 `None`**：
///
/// ```ignore
/// sdk::export_target_v1!(
///     my_open, Some(my_close),
///     my_write_begin, my_write_chunk, my_write_end, Some(my_write_abort),
///     my_read_begin, my_read_chunk, Some(my_read_end),
///     my_list_json, my_delete, Some(my_ensure_dir), Some(my_ping), Some(my_test_json),
///     Some(my_last_error),
///     Some(my_plan_begin), Some(my_plan_next), Some(my_plan_end),
/// );
/// ```
#[macro_export]
macro_rules! export_target_v1 {
    (
        $open:expr, $close:expr,
        $write_begin:expr, $write_chunk:expr, $write_end:expr, $write_abort:expr,
        $read_begin:expr, $read_chunk:expr, $read_end:expr,
        $list_json:expr, $delete:expr, $ensure_dir:expr, $ping:expr, $test_json:expr,
        $last_error_json:expr,
        $plan_begin:expr, $plan_next:expr, $plan_end:expr $(,)?
    ) => {
        /// 目标能力入口：返回插件持有的静态目标表
        #[no_mangle]
        pub extern "C" fn fn_kzwr_plugin_target_v1() -> *const $crate::KzwrTargetAbi {
            static TABLE: $crate::KzwrTargetAbi = $crate::KzwrTargetAbi {
                abi: $crate::ABI_VERSION,
                size: std::mem::size_of::<$crate::KzwrTargetAbi>() as u32,
                target_open: $open,
                target_close: $close,
                write_begin: $write_begin,
                write_chunk: $write_chunk,
                write_end: $write_end,
                write_abort: $write_abort,
                read_begin: $read_begin,
                read_chunk: $read_chunk,
                read_end: $read_end,
                list_json: $list_json,
                delete: $delete,
                ensure_dir: $ensure_dir,
                ping: $ping,
                test_json: $test_json,
                config_get: None,
                config_set: None,
                last_error_json: $last_error_json,
                plan_begin: $plan_begin,
                plan_next: $plan_next,
                plan_end: $plan_end,
                free_str: $crate::free_c_string,
            };
            &TABLE
        }
    };
}

/// 常用动作结果构造（避免插件各自手拼 JSON）
pub mod json {
    /// `{"success":true,"message":…}`
    pub fn ok_message(message: &str) -> String {
        format!(
            r#"{{"success":true,"message":{}}}"#,
            escape(message)
        )
    }

    /// `{"success":false,"error":…}`
    pub fn error(message: &str) -> String {
        format!(
            r#"{{"success":false,"error":{}}}"#,
            escape(message)
        )
    }

    /// JSON 字符串转义（仅用于上面两个便捷函数；复杂结构请自备 serde_json）
    pub fn escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}
