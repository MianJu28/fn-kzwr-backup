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

    /// 同 [`Self::verify`]，但额外带上该插件的**自管配置命名空间**（已解密）
    ///
    /// 目标插件可能把连接参数放在自管配置里（而不是 `url`），「测试连接」必须用与
    /// `build()` 相同的 `config` 才准确。默认实现忽略 `config`、保持向后兼容；
    /// 内置 WebDAV 无自管配置，因此无需覆盖。
    ///
    /// **调用方不得把 `config` 写入日志**（可能含插件自有凭据）。
    async fn verify_with_config(
        &self,
        url: Option<&str>,
        user: &str,
        pass: &str,
        _config: serde_json::Value,
    ) -> Result<String, String> {
        self.verify(url, user, pass).await
    }

    /// 设置页 UI 描述（默认不出现）
    fn ui(&self) -> Option<PluginUi> {
        None
    }

    /// 是否支持**并发回传**（计划式上传）
    ///
    /// 这是插件**自身的能力声明**（与是否启用无关）：宿主据此决定是否在插件卡片里
    /// 给出「上传并发路数」设置；真正是否启用由该插件**自己的**并发度配置决定
    /// （缺省沿用插件声明，0/1 = 关闭，≥2 = 启用）。
    fn supports_plan(&self) -> bool {
        false
    }

    /// **是否需要用户名/密码**（缺省 `true`，与既有行为一致）
    ///
    /// 有些目标根本不需要凭据 —— 典型是「本地目录」这类目标：它只认一个路径。
    /// 若宿主仍强制要求填账号密码，用户在「目标」页就**建不出**这种目标
    /// （旧行为正是如此：`target_save` 一律要求凭据，导致插件目标无法创建）。
    ///
    /// 声明为 `false` 时：新建目标允许凭据留空，且保存前**不**做连通性实测
    /// （插件通常没有可测的连接；`url` 的语义由插件自己解释，如本地路径）。
    fn needs_credentials(&self) -> bool {
        true
    }

    /// 目标地址字段的展示标签（缺省「地址」）
    ///
    /// 让插件说明 `url` 的实际含义：WebDAV 是「地址」，本地目录则是「目录路径」。
    fn url_label(&self) -> Option<String> {
        None
    }

    /// 目标地址字段的占位提示（缺省由前端按 WebDAV 处理）
    fn url_placeholder(&self) -> Option<String> {
        None
    }

    /// 目标地址字段的说明文字（缺省由前端按 WebDAV 处理）
    fn url_hint(&self) -> Option<String> {
        None
    }

    /// **「新建/编辑目标」弹窗的字段声明**（缺省 = 宿主给 WebDAV 默认表单）
    ///
    /// 宿主**只负责渲染与存取**，不预设字段语义 —— 与插件设置弹窗（`ui.blocks`）
    /// 同一套思路：新增目标类型不需要改前端。
    ///
    /// 三个 well-known 键（`url` / `username` / `password`）映射到既有存储，
    /// 其余任意键存入该目标自己的 `TargetConfig.fields`（加密）并注入
    /// `target_json.config`，插件从自己的命名空间读。
    fn form_fields(&self) -> Vec<crate::plugin::abi::AbiTargetField> {
        Vec::new()
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
    ///
    /// 注：`settings` 是**稳定 ABI 的既有取值**（外置插件已按此发送，不可改名）。
    /// 前端插件化重构后，它渲染在独立的**「插件」页**而非设置页 ——
    /// 该字段现在只表示「配置类卡片」，与具体页面解耦。
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
    ///
    /// `value` 是**静态**文案；若填了 `action`，前端会在卡片渲染时对该路径发起
    /// **GET** 请求，并用响应里的 `value` / `hint` 覆盖显示 —— 用于「需要实时查询」
    /// 的指标（如云端空间用量）。这样插件不必为了一个动态数字去写专用前端组件。
    Metric {
        label: String,
        #[serde(default)]
        value: String,
        #[serde(default)]
        hint: Option<String>,
        /// 可选：读取该路径（相对 `api_base`）取实时值，响应形如 `{"value":"...","hint":"..."}`
        #[serde(default)]
        action: Option<String>,
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
        /// 值由谁保管：`host` = 宿主代存（`plugin_data` 命名空间，加密落盘）；
        /// 缺省/其它 = 插件自己的路由处理
        #[serde(default)]
        scope: Option<String>,
        /// **回显**：渲染时 GET 该路径（相对 `api_base`）取真实值，覆盖 `value`。
        ///
        /// `value` 只是描述里的**静态默认值**，插件的持久化配置它并不知道；
        /// 没有 `echo` 的话表单永远显示默认值（如阈值恒为 85），用户改完再打开就"丢"了。
        ///
        /// 响应契约 `{"value": ..., "configured": bool, "hint": "..."}`：
        /// - `secret: true` 的字段**不回填明文**，只用 `configured` 显示
        ///   「已设置（留空则保持不变）」占位；
        /// - 普通字段用 `value` 回填；`hint` 可覆盖占位符。
        #[serde(default)]
        echo: Option<String>,
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
        /// 见 [`UiBlock::Text::scope`]
        #[serde(default)]
        scope: Option<String>,
        /// 见 [`UiBlock::Text::echo`]
        #[serde(default)]
        echo: Option<String>,
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
        /// 见 [`UiBlock::Text::scope`]
        #[serde(default)]
        scope: Option<String>,
        /// 见 [`UiBlock::Text::echo`]
        #[serde(default)]
        echo: Option<String>,
    },
    /// **多账号列表**（通用 CRUD 区块）
    ///
    /// 用于「一个插件管多份凭据」的场景（如 kzwr 多账号）：前端不需要为每个插件
    /// 写专属组件，只按下面的**数据契约**渲染增删改查界面。
    ///
    /// 数据契约（都以 `api_base` 为前缀，动作由插件的 `action_json` 实现）：
    /// - `GET  {list}` → `{"accounts":[{"id","name","configured":bool,"meta":{…}}]}`
    /// - `POST {add}`  body `{"name":…,"token":…}` → `{"success":bool,"message"|"error"}`
    /// - `POST {update}` body `{"id":…,"name":…,"token":…}`（token 空 = 不修改）
    /// - `POST {remove}` body `{"id":…}`
    ///
    /// 安全：**列表永不回传明文 token**，只给 `configured`；`secret_label` 决定输入框
    /// 标题（如「access-token」），留空即保持「已设置」占位。
    Accounts {
        /// 列表读取路径（相对 `api_base`，GET）
        list: String,
        /// 新增动作（POST）
        add: String,
        /// 修改动作（POST）
        update: String,
        /// 删除动作（POST）
        remove: String,
        /// 区块标题
        #[serde(default)]
        label: String,
        /// 凭据字段名（提交体里的键，如 `token`）
        #[serde(default = "default_account_credential_field")]
        credential_field: String,
        /// 凭据输入框标题
        #[serde(default)]
        credential_label: String,
        /// 凭据输入框占位提示（去哪拿、怎么拿）
        #[serde(default)]
        credential_placeholder: Option<String>,
        /// 是否允许添加多个（`false` = 只有一条时隐藏「新增」）
        #[serde(default = "default_true_for_accounts")]
        multiple: bool,
        /// 每项右侧的「其它编辑项」动作（如按账号设阈值）；留空则无
        #[serde(default)]
        edit_action: Option<String>,
        /// 该编辑动作提交的数值字段名（配合 `edit_action`，如 `percent`）
        #[serde(default = "default_account_edit_field")]
        edit_field: String,
        /// 编辑项的输入框标题（如「空间预警阈值」）
        #[serde(default)]
        edit_label: String,
        /// 编辑项单位后缀（如 `%`）
        #[serde(default)]
        edit_suffix: Option<String>,
        /// 编辑项的补充说明（如「0 = 关闭该账号的预警」）
        #[serde(default)]
        edit_hint: Option<String>,
        /// 编辑项取值范围（含端点；前端做原生约束）
        #[serde(default = "default_account_edit_min")]
        edit_min: u64,
        #[serde(default = "default_account_edit_max")]
        edit_max: u64,
    },
    /// 只读提示
    Tips { text: String },
}

fn default_true_for_accounts() -> bool {
    true
}

fn default_account_credential_field() -> String {
    "token".to_string()
}

/// 账号区块「编辑项」的缺省字段名与取值范围
fn default_account_edit_field() -> String {
    "percent".to_string()
}

fn default_account_edit_min() -> u64 {
    0
}

fn default_account_edit_max() -> u64 {
    100
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
    /// 是否支持并发回传（计划式上传；**插件自身能力声明**，非开关）
    #[serde(default)]
    pub supports_plan: bool,
    /// 该插件的上传并发路数（**用户配置，每插件独立**）
    ///
    /// `None` = 未配置（沿用插件声明）；`Some(0|1)` = 关闭并发；`Some(≥2)` = 启用并发。
    #[serde(default)]
    pub parallel: Option<u32>,
    /// 该插件是否被**按插件禁用**（运行时启停；见 `PluginSettings::disabled`）
    ///
    /// 被禁用时：`available` 恒为 `false`、不参与路由分发与目标装配，
    /// 但**仍会出现在清单里**，以便插件页把它列出来并允许重新启用。
    #[serde(default)]
    pub disabled: bool,
    /// 该目标插件**是否需要用户名/密码**（`kind=target` 时有意义）
    ///
    /// 前端据此决定新建目标表单是否显示账号/密码字段
    /// —— 有些目标（如本地目录）根本不用凭据，强制要求会导致**建不出目标**。
    #[serde(default = "default_true_entry")]
    pub needs_credentials: bool,
    /// 目标地址字段的展示标签（如「地址」/「目录路径」）
    #[serde(default)]
    pub url_label: Option<String>,
    /// 目标地址字段的占位提示
    #[serde(default)]
    pub url_placeholder: Option<String>,
    /// 目标地址字段的说明文字
    #[serde(default)]
    pub url_hint: Option<String>,
    /// **「新建/编辑目标」弹窗的字段声明**（`kind=target` 时有意义）
    ///
    /// 非空时前端按它渲染表单；为空则回退到宿主内置的 WebDAV 默认表单
    /// —— 保证老插件（未声明 `form`）行为不变。
    #[serde(default)]
    pub form: Vec<crate::plugin::abi::AbiTargetField>,
}

/// `needs_credentials` 的 serde 缺省：`true`（老前端/未知插件按需要凭据处理）
fn default_true_entry() -> bool {
    true
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
    ///
    /// `mgr` 用来解密**本插件自己的** `plugin_data`（注入快照的 `self_config`）。
    /// **调用方不得把注入结果写入日志**（含明文凭据）。
    fn available(&self, cfg: &AppConfig, mgr: &ConfigManager) -> bool;

    /// 插件自带的 HTTP 子路由；核心统一挂在 `/api/p/<插件id>` 下（默认空）
    fn routes(&self) -> axum::Router<crate::AppState> {
        axum::Router::new()
    }

    /// 启动自检（如 access-token 校验）；失败只告警，不影响启动
    async fn on_startup(&self, _state: &crate::AppState) {}

    /// 周期性巡检（如空间预警）；由后台定时任务驱动，默认 30 分钟一次
    async fn patrol(&self, _state: &crate::AppState) {}

    /// 备份成功后的可选动作（如按保留策略清空云端回收站），返回处理计数
    ///
    /// `task_id` 是**刚完成备份的那个任务**：回收站门槛按任务配置，
    /// 插件从快照的 `tasks[].recycle_*`（或顶层 `after_backup_task`）里取。
    async fn after_backup(&self, _state: &crate::AppState, _task_id: &str) -> Option<u64> {
        None
    }

    /// 配置变更后重载插件自身状态（如配置导入/保存后刷新 token）；默认无操作
    async fn reload(&self, _state: &crate::AppState) {}

    /// 插件自注册的定时任务到点回调（`kind` 为插件在能力表 `schedule` 里给的标识）
    ///
    /// 只有实现 `host_bind` 并调用 `schedule` 的插件才会收到；默认无操作。
    async fn timer(&self, _state: &crate::AppState, _kind: &str) {}

    /// 下发宿主能力表（仅外置 C ABI 插件需要；内置插件走 Rust 直连，默认无操作）
    ///
    /// **必须在 `AppState` 建好之后**调用：能力表的实现依赖 `AppState`。
    /// 返回 `true` = 插件接受了能力表。
    fn bind_host(&self, _state: &crate::AppState) -> bool {
        false
    }

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

    /// 卸载清除（ADR-013 决策 2）：宿主调用 `/api/plugins/:id/purge` 前调用，让插件释放
    /// **可选的**自身状态（动态库句柄由宿主保活到进程结束，不会在此卸载）。默认无操作。
    fn destroy(&self) {}
}
