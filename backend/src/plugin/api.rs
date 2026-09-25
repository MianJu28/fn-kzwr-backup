//! 插件契约：核心与插件共同依赖的 trait / 类型
//!
//! 这里只放"接口与数据"，不放任何具体实现，避免核心反向依赖插件。

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;

use crate::infra::config::{AppConfig, ConfigManager};
use crate::infra::storage_trait::TargetStorage;

/// 插件类别
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    /// 远程备份目标（实现 `TargetStorage`）
    Target,
    /// 增强功能：账号/空间/回收站/通知等非备份通道能力
    Enhance,
}

/// 插件元信息（供 `/api/plugins` 与前端区块注册表使用）
#[derive(Debug, Clone, Serialize)]
pub struct PluginMeta {
    /// 插件 id（同时用作配置段名与路由前缀 `/api/p/<id>`）
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: PluginKind,
    /// 是否内置（随应用一同编译分发）
    pub builtin: bool,
    pub description: String,
}

/// 目标插件：提供远程备份目标
#[async_trait]
pub trait TargetPlugin: Send + Sync {
    fn meta(&self) -> PluginMeta;

    /// 按当前配置构建目标实例；凭据不全时返回 `None`（核心回退到占位适配器）。
    /// 返回 `(存储实现, 展示名)`。
    fn build(&self, mgr: &ConfigManager) -> Option<(Arc<dyn TargetStorage>, String)>;

    /// 保存配置前的连通性验证，返回实际使用的地址（基址由插件自行决定）
    async fn verify(&self, user: &str, pass: &str) -> Result<String, String>;
}

/// 增强插件提供的能力（前端据此决定是否渲染对应区块）
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct EnhanceCaps {
    /// 账号信息（邮箱 / 套餐等）
    pub account: bool,
    /// 存储空间与空间预警
    pub quota: bool,
    /// 云端回收站清理
    pub recycle_bin: bool,
    /// 通知外发
    pub notify: bool,
}

/// 增强插件：非备份通道的可选能力
#[async_trait]
pub trait EnhancePlugin: Send + Sync {
    fn meta(&self) -> PluginMeta;
    fn caps(&self) -> EnhanceCaps;
    /// 是否已可用（如 access-token 已配置）；未就绪时 UI 隐藏相关区块
    fn available(&self, cfg: &AppConfig) -> bool;
}
