//! 插件层（ADR-013）
//!
//! **核心只依赖 [`api`] 里的 trait，不感知任何具体远程目标/增强实现**；
//! 具体实现由插件提供，装配统一走 [`registry::PluginRegistry`]。
//!
//! 三种装配方式并存：
//! - **内置插件**（方案 D）：与核心一同编译，`PluginRegistry::builtin()` 注册
//! - **外置插件 · 稳定 C ABI**（推荐，[`abi`] / [`cabi`]）：插件只依赖冻结的
//!   C ABI + JSON 契约（`plugins/sdk`，零第三方依赖），**宿主升级不需要重编插件**
//! - **外置插件 · Rust 直连**（进阶，[`sdk`]）：直接传 Rust trait 对象，能力最全
//!   （可写备份目标），但 Rust ABI 不稳定 → 必须与宿主同版本编译
//!
//! 外置加载由 [`loader`] 统一处理：启动时扫描插件目录、`libloading` 打开、
//! 优先走稳定 C ABI，否则回退 Rust 直连并做版本校验；默认关闭（配置 `plugins.enabled`），
//! 单个插件失败只记诊断、不影响核心

pub mod abi;
pub mod api;
pub mod builtin;
pub mod cabi;
pub mod loader;
pub mod registry;
pub mod sdk;

pub use api::{EnhanceCaps, EnhancePlugin, PluginKind, PluginMeta, TargetPlugin};
pub use loader::ExternalPluginReport;
pub use registry::PluginRegistry;
