//! 插件契约：核心与插件共同依赖的 trait / 类型
//!
//! 这里只放"接口与数据"，不放任何具体实现，避免核心反向依赖插件。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::infra::config::{AppConfig, ConfigManager, TargetConfig};
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

    /// 用**一个具体的目标配置**构建实例（多目标：每个目标一份实例）。
    /// 凭据不全/配置不适用时返回 `None`（核心回退到占位适配器）。
    /// 返回 `(存储实现, 展示名)`。
    fn build(
        &self,
        target: &TargetConfig,
        mgr: &ConfigManager,
    ) -> Option<(Arc<dyn TargetStorage>, String)>;

    /// 保存配置前的连通性验证，返回实际使用的地址（基址由插件自行决定）；
    /// `url` 为 `None` 时用插件默认地址。
    async fn verify(&self, url: Option<&str>, user: &str, pass: &str) -> Result<String, String>;

    /// 设置页 UI 描述（默认不出现）
    fn ui(&self) -> Option<PluginUi> {
        None
    }
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

/// 插件 UI 描述：前端据此决定「设置页/概览页」显示哪些卡片、顺序如何。
///
/// 内置插件可以只填 `component`（前端有手写组件）；外置插件填 `blocks`，
/// 由前端通用渲染器渲染 —— 这样新增插件**不需要重新打包前端**。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginUi {
    /// 分区：`settings` | `dashboard`
    pub section: String,
    /// 卡片标题
    pub title: String,
    /// 排序（小的在前）
    #[serde(default)]
    pub order: i32,
    /// 内置组件名（如 `kzwr`）；前端认识时优先用它，不认识则回退到 `blocks`
    #[serde(default)]
    pub component: Option<String>,
    /// 通用渲染块
    #[serde(default)]
    pub blocks: Vec<UiBlock>,
}

/// 通用 UI 块（schema 驱动，外置插件用）
///
/// 这份 schema 同时是**稳定 C ABI（[`crate::plugin::abi`]）交换的 JSON 契约**，
/// 因此可选字段都带 `#[serde(default)]`：老插件少写字段、新宿主多认字段都不会失败。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiBlock {
    /// 只读指标（如空间用量）
    Metric {
        label: String,
        value: String,
        #[serde(default)]
        hint: Option<String>,
    },
    /// 文本/密码输入 + 提交按钮
    Text {
        field: String,
        label: String,
        #[serde(default)]
        value: Option<String>,
        #[serde(default)]
        placeholder: Option<String>,
        #[serde(default)]
        secret: bool,
        action: String,
        #[serde(default)]
        button: String,
    },
    /// 数字输入 + 提交按钮
    Number {
        field: String,
        label: String,
        #[serde(default)]
        value: Option<i64>,
        #[serde(default)]
        suffix: Option<String>,
        action: String,
        #[serde(default)]
        button: String,
    },
    /// 按钮（可带二次确认文案）
    Button {
        label: String,
        action: String,
        #[serde(default)]
        danger: bool,
        #[serde(default)]
        confirm: Option<String>,
    },
    /// 开关
    Toggle {
        field: String,
        label: String,
        value: bool,
        action: String,
    },
    /// 只读提示
    Tips { text: String },
}

/// 插件清单条目（供 `/api/plugins`；前端唯一的数据来源）
#[derive(Debug, Clone, Serialize)]
pub struct PluginEntry {
    #[serde(flatten)]
    pub meta: PluginMeta,
    /// 是否已可用（如 token 已配置）
    pub available: bool,
    /// 插件 API 前缀（前端拼请求用），如 `/api/p/kzwr`
    pub api_base: String,
    pub ui: Option<PluginUi>,
    /// 来源：`builtin`（随应用编译）| `external`（外置动态库，ADR-013 方案 B）
    #[serde(default)]
    pub source: String,
    /// 外置插件的动态库路径（内置为空）
    #[serde(default)]
    pub path: Option<String>,
}

/// 插件自检项（供「一键体检」汇总；由核心映射成 UI 的检查项）
#[derive(Debug, Clone, Serialize)]
pub struct CheckOutcome {
    /// 检查项标识（默认用插件 id；同一插件可返回多项）
    pub key: String,
    pub title: String,
    /// `ok` | `warn` | `fail`
    pub status: String,
    pub detail: String,
    pub hint: Option<String>,
}

/// 增强插件：非备份通道的可选能力
#[async_trait]
pub trait EnhancePlugin: Send + Sync {
    fn meta(&self) -> PluginMeta;
    fn caps(&self) -> EnhanceCaps;
    /// 是否已可用（如 access-token 已配置）；未就绪时 UI 隐藏相关区块
    fn available(&self, cfg: &AppConfig) -> bool;

    /// 插件自带的 HTTP 子路由；核心统一挂在 `/api/p/<插件id>` 下（默认空）
    fn routes(&self) -> axum::Router<crate::AppState> {
        axum::Router::new()
    }

    /// 启动自检（如 access-token 校验）；失败只告警，不影响启动
    async fn on_startup(&self, _state: &crate::AppState) {}

    /// 周期性巡检（如空间预警）；由后台定时任务驱动，默认 30 分钟一次
    async fn patrol(&self, _state: &crate::AppState) {}

    /// 备份成功后的可选动作（如按保留策略清空云端回收站），返回处理计数
    async fn after_backup(&self, _state: &crate::AppState) -> Option<u64> {
        None
    }

    /// 配置变更后重载插件自身状态（如配置导入/保存后刷新 token）；默认无操作
    async fn reload(&self, _state: &crate::AppState) {}

    /// 设置页/概览页的 UI 描述（默认不出现）
    fn ui(&self) -> Option<PluginUi> {
        None
    }

    /// 「一键体检」自检项（默认不参与）
    async fn health_check(
        &self,
        _state: &crate::AppState,
        _cfg: &AppConfig,
    ) -> Vec<CheckOutcome> {
        Vec::new()
    }
}
