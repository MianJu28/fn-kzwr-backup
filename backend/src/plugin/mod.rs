//! 插件层（ADR-013）
//!
//! **核心只依赖 [`api`] 里的 trait，不感知任何具体远程目标/增强实现**；
//! 具体实现由插件提供，装配统一走 [`registry::PluginRegistry`]。
//!
//! 装配方式：
//! - **内置插件**（方案 D）：与核心一同编译；**同样走稳定 ABI**（见 [`abi`]），
//!   由 `PluginRegistry::builtin()` 注册 —— 与外置插件零差异
//! - **外置插件 · 稳定 C ABI**：插件只依赖冻结的 C ABI + JSON 契约（[`abi`]/[`cabi`]），
//!   `plugins/sdk` 提供导出宏（零第三方依赖），**宿主升级不需要重编插件**；
//!   一个 `.so` 可同时提供增强能力（`fn_kzwr_plugin_abi_v1`）与目标能力
//!   （[`target_abi`] / `fn_kzwr_plugin_target_v1`）
//!
//! **Rust 直连路径已删除**（产品未发布、无兼容包袱）：稳定 C ABI 是唯一加载机制。
//! 加载由 [`loader`] 统一处理：启动扫描插件目录、`libloading` 打开、接表校验；
//! 默认关闭（配置 `plugins.enabled`），单个插件失败只记诊断、不影响核心。

pub mod abi;
pub mod api;
pub mod builtin;
pub mod cabi;
pub mod loader;
pub mod registry;
pub mod target_abi;

pub use api::{EnhanceCaps, EnhancePlugin, PluginKind, PluginMeta, TargetPlugin};
pub use loader::ExternalPluginReport;
pub use registry::PluginRegistry;
