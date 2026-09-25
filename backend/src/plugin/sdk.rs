//! 外置插件 SDK（ADR-013 **方案 B：动态库**）
//!
//! 外置插件编译为 `cdylib`（`*.so`），宿主用 `libloading` 在启动时加载。
//! 双方只通过 **3 个 C ABI 符号**交接，trait 对象仍走 Rust 侧类型（因此要求
//! 插件与宿主**用同一套源码/toolchain 编译**，见下文的版本校验）：
//!
//! | 符号 | 签名 | 用途 |
//! |------|------|------|
//! | `fn_kzwr_plugin_abi_version` | `() -> u32` | ABI 版本，宿主必须匹配 `PLUGIN_ABI_VERSION` |
//! | `fn_kzwr_plugin_host_version` | `() -> *const c_char` | 插件编译时链接的宿主版本（`fn-kzwr-backup` crate 版本） |
//! | `fn_kzwr_plugin_create` | `() -> *mut PluginHandle` | 创建插件实例（宿主接管所有权） |
//!
//! # 为什么校验「宿主版本」
//!
//! `PluginHandle` 里装的是 `Box<dyn EnhancePlugin>` 这类 Rust 侧对象，其 vtable 与结构
//! 布局**不是稳定 ABI**：插件若用另一版宿主（可能改了 trait 方法/字段布局）编译，
//! 加载后调用即可能崩溃。因此宿主会拒绝「编译期版本 ≠ 运行版本」的插件，并提示重新编译。
//! 升级主程序后请重新编译外置插件（`Scripts/build_plugins.sh`）。
//!
//! # 最小插件示例
//!
//! ```ignore
//! fn create() -> fnos_backup::plugin::sdk::PluginHandle {
//!     fnos_backup::plugin::sdk::PluginHandle {
//!         target: None,
//!         enhance: Some(Box::new(MyPlugin)),
//!     }
//! }
//! fnos_backup::export_plugin!(create);
//! ```

use std::os::raw::c_char;

/// 插件 ABI 版本：**破坏性改动必须 +1**（宿主拒绝不匹配的插件）
pub const PLUGIN_ABI_VERSION: u32 = 1;

/// 插件编译时链接的宿主版本（`fn-kzwr-backup` crate 版本）
///
/// 由 [`export_plugin!`] 自动导出；宿主比对运行版本，不一致则拒绝加载（避免 ABI 漂移）。
pub const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 导出符号：ABI 版本
pub const SYM_ABI_VERSION: &[u8] = b"fn_kzwr_plugin_abi_version\0";
/// 导出符号：宿主版本
pub const SYM_HOST_VERSION: &[u8] = b"fn_kzwr_plugin_host_version\0";
/// 导出符号：创建实例
pub const SYM_CREATE: &[u8] = b"fn_kzwr_plugin_create\0";

/// 外置插件实例句柄：宿主接收后转换为 `Arc<dyn …>` 注册进注册表
///
/// 两个字段至少有一个为 `Some`；同时提供时，同一个动态库既可充当备份目标、
/// 也可提供增强能力。
pub struct PluginHandle {
    pub target: Option<Box<dyn crate::plugin::api::TargetPlugin>>,
    pub enhance: Option<Box<dyn crate::plugin::api::EnhancePlugin>>,
}

impl PluginHandle {
    /// 是否什么都没提供（宿主视为加载失败并给出诊断）
    pub fn is_empty(&self) -> bool {
        self.target.is_none() && self.enhance.is_none()
    }

    /// 便捷构造：仅增强插件
    pub fn enhance_only(p: impl crate::plugin::api::EnhancePlugin + 'static) -> Self {
        Self {
            target: None,
            enhance: Some(Box::new(p)),
        }
    }

    /// 便捷构造：仅目标插件
    pub fn target_only(p: impl crate::plugin::api::TargetPlugin + 'static) -> Self {
        Self {
            target: Some(Box::new(p)),
            enhance: None,
        }
    }
}

/// 宿主版本字符串（NUL 结尾，静态存储，供插件导出符号返回）
pub fn host_version_cstr() -> *const c_char {
    static V: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");
    V.as_ptr() as *const c_char
}

/// 导出一个动态库插件的三个 C ABI 符号
///
/// 用法（外置插件 crate 里，**放在 crate 根部**）：
///
/// ```ignore
/// fn create() -> fnos_backup::plugin::sdk::PluginHandle { /* … */ }
/// fnos_backup::export_plugin!(create);
/// ```
#[macro_export]
macro_rules! export_plugin {
    ($ctor:expr) => {
        /// ABI 版本（宿主校验；不匹配则拒绝加载）
        #[no_mangle]
        pub extern "C" fn fn_kzwr_plugin_abi_version() -> u32 {
            $crate::plugin::sdk::PLUGIN_ABI_VERSION
        }

        /// 插件编译时链接的宿主版本（宿主比对运行版本）
        #[no_mangle]
        pub extern "C" fn fn_kzwr_plugin_host_version() -> *const ::std::os::raw::c_char {
            $crate::plugin::sdk::host_version_cstr()
        }

        /// 创建插件实例（宿主用 `Box::from_raw` 接管所有权）
        #[no_mangle]
        pub extern "C" fn fn_kzwr_plugin_create() -> *mut $crate::plugin::sdk::PluginHandle {
            let handle: $crate::plugin::sdk::PluginHandle = ($ctor)();
            ::std::boxed::Box::into_raw(::std::boxed::Box::new(handle))
        }
    };
}
