//! 插件层（ADR-013）
//!
//! **核心只依赖 [`api`] 里的 trait，不感知任何具体远程目标/增强实现**；
//! 具体实现由插件提供，装配统一走 [`registry::PluginRegistry`]。
//!
//! 首期采用「编译期内置插件」（方案 D）：插件与核心一同编译、由注册表装配，
//! 先把**接口与装配点定型**；后续若改为外置加载（子进程/动态库/WASM），
//! 只需替换 `PluginRegistry::builtin()` 的来源，核心与 trait 不用再动。

pub mod api;
pub mod builtin;
pub mod registry;

pub use api::{EnhanceCaps, EnhancePlugin, PluginKind, PluginMeta, TargetPlugin};
pub use registry::PluginRegistry;
