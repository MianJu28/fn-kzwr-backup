//! 插件层（ADR-013）
//!
//! **核心只依赖 [`api`] 里的 trait，不感知任何具体远程目标/增强实现**；
//! 具体实现由插件提供，装配统一走 [`registry::PluginRegistry`]。
//!
//! 两种装配方式并存：
//! - **内置插件**（方案 D）：与核心一同编译，`PluginRegistry::builtin()` 注册
//! - **外置插件**（**方案 B：动态库 `*.so`**）：`PluginRegistry::load_external()` 在启动时
//!   扫描 `$TRIM_PKGETC/plugins`（用户目录）与 `$TRIM_APPDEST/plugins`（随包分发），
//!   用 `libloading` 加载并做 ABI/宿主版本校验；默认关闭（配置 `plugins.enabled`），
//!   单个插件失败只记诊断、不影响核心 —— 详见 [`sdk`] 与 [`loader`]

pub mod api;
pub mod builtin;
pub mod loader;
pub mod registry;
pub mod sdk;

pub use api::{EnhanceCaps, EnhancePlugin, PluginKind, PluginMeta, TargetPlugin};
pub use loader::ExternalPluginReport;
pub use registry::PluginRegistry;
