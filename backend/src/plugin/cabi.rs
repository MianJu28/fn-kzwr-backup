//! 把**稳定 C ABI v1** 的外置插件适配成宿主内部使用的 [`EnhancePlugin`]
//!
//! 这样核心与前端完全不用知道插件是用什么语言/什么版本编译的：
//! 宿主只按 [`crate::plugin::abi`] 的契约调用回调、解析 JSON。
//! 关键收益：**宿主升级不需要重编插件**（只要 `C_ABI_VERSION` 不变）。

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use async_trait::async_trait;
use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::Router;
use serde_json::Value;

use crate::infra::config::ConfigManager;
use crate::AppState;

use super::abi::{
    AbiDescribe, CfgSnapshot, JsonFn0, JsonFn1, JsonFn2, KzwrPluginAbi, C_ABI_VERSION,
};
use super::api::{
    CheckOutcome, EnhanceCaps, EnhancePlugin, PluginKind, PluginMeta, PluginUi,
};

/// 插件提供的静态函数表（`&'static` + Send/Sync 包装，便于在路由闭包里捕获）
#[derive(Clone, Copy)]
pub struct AbiTable(pub &'static KzwrPluginAbi);

// SAFETY: 表由插件静态持有、跨线程只读；回调自身要求线程安全（见 ABI 文档约定）
unsafe impl Send for AbiTable {}
unsafe impl Sync for AbiTable {}

/// 稳定 C ABI 插件 → 宿主内部增强插件
pub struct CApiEnhance {
    table: AbiTable,
    describe: AbiDescribe,
    /// 动态库路径（诊断用）
    pub path: String,
}

/// 插件表是否覆盖到 `host_bind` 字段（尾部追加 ⇒ 老插件的表可能短到这里之前）
///
/// 用 `offset_of` + 字段大小比较，而不是拿 `TABLE_SIZE` 硬比：后者会把
/// 「只差尾部一两个可选字段」的老插件误判成没实现。
pub fn has_host_bind(t: &KzwrPluginAbi) -> bool {
    let need = std::mem::offset_of!(KzwrPluginAbi, host_bind)
        + std::mem::size_of::<Option<super::abi::HostBindFn>>();
    (t.size as usize) >= need
}

/// 校验插件主表（空指针 / `abi` 版本 / `size` 必需前缀）
///
/// 主表**尾部追加演进**（不再冻结）：`size` 只需覆盖到 `free_str` 为止的必需前缀
/// （[`KzwrPluginAbi::MIN_SIZE`]）。老插件表短一截是**合法**的 —— 宿主按「未实现」
/// 处理后面的可选字段（`destroy` / `host_bind`），插件继续走声明式回传通道。
pub unsafe fn validate_table(
    table: *const KzwrPluginAbi,
) -> Result<&'static KzwrPluginAbi, String> {
    if table.is_null() {
        return Err("fn_kzwr_plugin_abi_v1 返回了空指针".to_string());
    }
    let t: &'static KzwrPluginAbi = &*table;
    if t.abi != C_ABI_VERSION {
        return Err(format!(
            "插件 ABI 版本为 {}，宿主支持 {C_ABI_VERSION}：请更新插件（或宿主）",
            t.abi
        ));
    }
    let need = KzwrPluginAbi::MIN_SIZE as usize;
    if (t.size as usize) < need {
        return Err(format!(
            "插件表长度 {} 小于必需的公开前缀 {need}（describe_json/事件/动作/free_str 必须齐全）",
            t.size
        ));
    }
    Ok(t)
}

/// 解析并校验插件的 `describe_json`
pub fn describe_of(t: &'static KzwrPluginAbi) -> Result<AbiDescribe, String> {
    let raw = call0_opt(AbiTable(t), t.describe_json)
        .ok_or_else(|| "describe_json 未返回有效 JSON".to_string())?;
    let describe: AbiDescribe = serde_json::from_str(&raw)
        .map_err(|e| format!("describe_json 解析失败：{e}"))?;
    if describe.id.trim().is_empty() {
        return Err("describe_json 缺少 id".to_string());
    }
    Ok(describe)
}

impl CApiEnhance {
    /// 校验并接管插件主表（enhance 面）
    ///
    /// 目标能力由 [`super::target_abi::CApiTarget::adopt`] 另行接管。
    pub unsafe fn adopt(
        table: *const KzwrPluginAbi,
        path: String,
    ) -> Result<Self, String> {
        let t = validate_table(table)?;
        let table = AbiTable(t);
        let describe = describe_of(t)?;
        Ok(Self { table, describe, path })
    }

    /// 调用 `available_json`（未实现/失败保守视为不可用，避免误判为可用）
    fn call_available(&self, cfg_json: &str) -> bool {
        let Some(f) = self.table.0.available_json else {
            return false;
        };
        let raw = match call1(self.table, f, &cfg_json) {
            Some(s) => s,
            None => return false,
        };
        serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|v| v.get("available").and_then(|x| x.as_bool()))
            .unwrap_or(false)
    }

    /// 调用生命周期事件（`event_json` 可选实现）
    ///
    /// 放到**阻塞线程池**执行：插件常在回调里对自己的 runtime 调 `block_on`
    /// （例如发 HTTP 请求），在 tokio worker 线程上这样做会直接 panic。
    async fn call_event(&self, event: &str, cfg_json: &str) -> Option<String> {
        let f = self.table.0.event_json?;
        let table = self.table;
        let event = event.to_string();
        let cfg_json = cfg_json.to_string();
        // 投递到**该插件的专属线程**（其上已施加 Landlock 沙箱，见 `plugin/worker.rs`）。
        // 用专属线程而非 `spawn_blocking`：Landlock 不可逆，池线程会被永久污染。
        self.call_on_worker(move || call2(table, f, &event, &cfg_json))
            .await
            .flatten()
    }

    /// 把同步 FFI 调用投递到**本插件的专属线程**并在其上执行
    ///
    /// 为什么不直接用 `spawn_blocking`：Landlock 沙箱是 **per-thread 且不可逆**的，
    /// 施加到 tokio 共享池线程会把它**永久污染**，导致宿主其它阻塞任务
    /// （配置读写、快照落盘）莫名失败。专属线程只跑本插件的回调。
    ///
    /// 返回 `None` = 沙箱线程不可用或回调 panic（**内层** `Option` 是 FFI 自身的
    /// 「未实现/NULL」，故调用点通常 `.flatten()`）。
    async fn call_on_worker<F, T>(&self, f: F) -> Option<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let id = self.describe.id.clone();
        super::worker::run_on_plugin_thread(&id, super::worker::policy_for(&id, &[]), f).await
    }

    /// 下发宿主能力表：签发 ctx 并调用插件的 `host_bind`（未实现则是无操作）
    ///
    /// **必须在 `AppState` 建好之后**才能调用（能力表的实现依赖 `AppState`：
    /// 审计/告警/事件总线都在里面）。因此加载期只接管主表，绑定单独一步。
    ///
    /// 返回 `true` = 插件接受了能力表；`false` = 未实现 / 拒绝 / 调用异常。
    /// 三种情况都不影响插件继续用声明式回传通道。
    pub fn bind_host(&self, state: &AppState) -> bool {
        if !has_host_bind(self.table.0) {
            return false;
        }
        let Some(f) = self.table.0.host_bind else {
            return false;
        };
        let id = self.describe.id.clone();

        // 宿主不再代存插件配置（ADR-021）：插件用自己的 `own_data_dir`，
        // 敏感内容经能力表的 `seal`/`unseal` 加密。故这里无需再注入配置镜像。
        let ctx = state.host_effects.issue(&id);
        let table = state.host_effects.table();

        // **必须走该插件的专属线程（已施加沙箱）**：`host_bind` 是插件启动时
        // 唯一的、最容易被滥用的入口（它能在这里读任何文件）。
        // 若直接在调用线程上跑，沙箱就被绕过了。
        // 同时兜异常：绑定失败绝不能影响加载与其它插件。
        let policy = super::worker::policy_for(&id, &[]);
        // 裸指针不是 `Send`，但这两个指针都由宿主签发、**进程内永不释放**
        // （见 `host_abi` 模块头「指针有效性」），跨线程使用是契约允许的。
        // 用局部 newtype 显式承担这个保证，而不是靠 `unsafe impl Send` 放宽全局。
        struct SendPtr(*const super::abi::KzwrHostAbi, *mut std::os::raw::c_void);
        // 闭包只捕获 SendPtr 本身
        impl SendPtr {
            fn parts(&self) -> (*const super::abi::KzwrHostAbi, *mut std::os::raw::c_void) {
                (self.0, self.1)
            }
        }
        // SAFETY: 能力表是 `&'static`；ctx 由 `HostEffects::contexts` 永久持有。
        unsafe impl Send for SendPtr {}
        let sp = SendPtr(table, ctx);
        let ok = super::worker::run_on_plugin_thread_blocking(&id, policy, move || {
            // 在闭包**内部**取出裸指针：闭包只捕获 `sp`（Send），
            // 裸指针不跨线程边界出现在闭包的捕获列表里。
            let (table, ctx) = sp.parts();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(table, ctx))).unwrap_or(-1)
        })
        .unwrap_or(-1);
        if ok == 0 {
            tracing::info!(plugin = %id, "插件已接受宿主能力表（日志/审计/告警/自配置/进度/定时）");
            true
        } else {
            // 拒绝 ⇒ 立刻失效 ctx，避免"半绑定"状态下产生效果
            state.host_effects.revoke(&id);
            tracing::warn!(plugin = %id, code = ok, "插件拒绝了宿主能力表，继续使用声明式回传");
            false
        }
    }
}

/// 处理插件返回值里的**声明式副作用**：告警、告警消解、自配置回写
///
/// 插件不回调宿主（ABI 里没有宿主 vtable），而是把「希望宿主做的事」随返回值
/// 一起带回来，由宿主执行。三条路径共用：事件、体检、动作（动作里报障最及时：
/// 用户点一下就看到）。返回 JSON 里的可选字段：
///
/// ```json
/// {
///   "alerts":  [{"level":"warn|error","message":"…"}],
///   "resolve": ["消息前缀"],
///   "config":  {"set":{"k":"v"}, "remove":["k"]}
/// }
/// ```
///
/// - `alerts`：按 (来源, 消息) 去重后落库；
/// - `resolve`：删除本插件中**消息以这些前缀开头**的告警 —— 用于「条件恢复后自动消解」
///   （如空间占用回落到阈值以下，之前的空间预警应自行消失，而不是一直挂着）；
/// - ~~`config`~~：**已移除（ADR-021）** —— 宿主不再代存插件配置。
///   插件改用 `own_data_dir` 自管（敏感内容经能力表 `seal`/`unseal` 加密）。
///   该字段现在被**忽略**（不报错，便于老插件平滑过渡）；
/// - `audit`：`[{"action","detail","ok"}]` 写入审计日志（如「自动清空回收站」这类
///   用户看不见的后台动作必须可追溯）。`action` 由插件自带命名空间（如
///   `kzwr.trash.auto`），核心不猜语义。
pub fn apply_side_effects(state: &AppState, plugin_id: &str, raw: &str) {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    if let Some(alerts) = v.get("alerts").and_then(|x| x.as_array()) {
        for a in alerts {
            let Some(msg) = a.get("message").and_then(|x| x.as_str()) else {
                continue;
            };
            let level = match a.get("level").and_then(|x| x.as_str()) {
                Some("error") => crate::domain::alerts::AlertLevel::Error,
                _ => crate::domain::alerts::AlertLevel::Warn,
            };
            crate::http::routes::raise_alert_once(
                state,
                level,
                crate::domain::alerts::AlertSource::Plugin(plugin_id.to_string()),
                msg.to_string(),
            );
        }
    }
    if let Some(prefixes) = v.get("resolve").and_then(|x| x.as_array()) {
        let prefixes: Vec<String> = prefixes
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .filter(|s| !s.is_empty())
            .collect();
        if !prefixes.is_empty() {
            let removed = state.alerts.remove_where(|a| {
                a.source
                    == crate::domain::alerts::AlertSource::Plugin(plugin_id.to_string())
                    && prefixes.iter().any(|p| a.message.starts_with(p.as_str()))
            });
            if removed > 0 {
                tracing::info!(plugin = %plugin_id, removed, "插件声明的告警已消解");
            }
        }
    }
    // `config` 字段已不再处理（宿主不代存插件配置）；仅保留审计通道
    apply_audit_records(state, plugin_id, &v);
}

/// **配置键名是否合法**（插件自管配置、目标自定义字段、声明式回写共用同一规则）
///
/// 非空、≤64 字符、只允许 `[A-Za-z0-9_-]`（**不允许点号**）。
/// 点号曾被用来做命名空间（`percent.<id>`），导致整批键被拒而插件以为写成功
/// —— 详见 `docs/PLUGIN_ABI.md` §4.4。
pub fn config_key_ok(k: &str) -> bool {
    !k.trim().is_empty()
        && k.len() <= 64
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 声明式审计写入（见 [`apply_side_effects`] 的 `audit` 字段）
fn apply_audit_records(state: &AppState, plugin_id: &str, v: &Value) {
    let Some(items) = v.get("audit").and_then(|x| x.as_array()) else {
        return;
    };
    for a in items {
        let Some(action) = a.get("action").and_then(|x| x.as_str()) else {
            continue;
        };
        let detail = a.get("detail").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let ok = a.get("ok").and_then(|x| x.as_bool()).unwrap_or(true);
        state.audit.record(action, detail, ok, None);
    }
    tracing::debug!(plugin = %plugin_id, n = items.len(), "插件声明的审计条目已写入");
}

/// 从 `AppState` 读出**指定插件视角**的配置快照 JSON
///
/// 快照**不再注入插件自配置**（`self_config`，ADR-021：宿主不代存插件配置）——
/// 插件改用能力表的 `own_data_dir` 自管。快照里只剩宿主可公开的配置
/// （目标/任务/时区等），**不含任何凭据**。
///
/// `after_backup_task` 仅 `after_backup` 事件传（告诉插件刚完成的是哪个任务）。
///
/// `plugin_id` 保留在签名里（调用点众多），但**已不再参与**：宿主不代存插件配置，
/// 快照对任何插件都相同。
pub fn cfg_json_for(state: &AppState, _plugin_id: &str, after_backup_task: Option<&str>) -> String {
    let mgr = state.config.lock().unwrap();
    let cfg = mgr.load().unwrap_or_default();
    let snap = CfgSnapshot::from_config(&cfg);
    match after_backup_task {
        Some(t) => snap.with_after_backup_task(t).to_json(),
        None => snap.to_json(),
    }
}

#[async_trait]
impl EnhancePlugin for CApiEnhance {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            id: self.describe.id.clone(),
            name: self.describe.name.clone(),
            version: if self.describe.version.is_empty() {
                "0.0.0".to_string()
            } else {
                self.describe.version.clone()
            },
            kind: PluginKind::Enhance,
            // 外置插件：恒为非内置
            builtin: false,
            description: self.describe.description.clone(),
        }
    }

    fn caps(&self) -> EnhanceCaps {
        self.describe.caps.into()
    }

    fn available(&self, cfg: &crate::infra::config::AppConfig, _mgr: &ConfigManager) -> bool {
        let snap = CfgSnapshot::from_config(cfg);
        self.call_available(&snap.to_json())
    }

    fn ui(&self) -> Option<PluginUi> {
        self.describe.ui.clone()
    }

    /// 动作路由：`/api/p/<插件id>/<action...>`（GET 的 query 与 POST 的 body 都按 JSON 传给插件）
    ///
    /// 用 `/*action` 通配支持**多段动作名**（如 `trash/empty`），前端 api 拼接零改动。
    fn routes(&self) -> Router<AppState> {
        let table = self.table;
        let plugin_id = self.describe.id.clone();
        let get_handler = {
            let plugin_id = plugin_id.clone();
            move |Path(action): Path<String>,
                  State(state): State<AppState>,
                  Query(q): Query<HashMap<String, String>>| async move {
                let action = action.trim_start_matches('/').to_string();
                let body = serde_json::to_string(&q).unwrap_or_else(|_| "{}".to_string());
                axum::Json(action_call(state, table, plugin_id, action, body, "GET").await)
            }
        };
        let post_handler = move |Path(action): Path<String>,
                                 State(state): State<AppState>,
                                 body: Option<axum::Json<Value>>| async move {
            let action = action.trim_start_matches('/').to_string();
            let body = body
                .map(|axum::Json(v)| v.to_string())
                .unwrap_or_else(|| "{}".to_string());
            axum::Json(action_call(state, table, plugin_id, action, body, "POST").await)
        };
        Router::new().route("/*action", get(get_handler).post(post_handler))
    }

    async fn on_startup(&self, state: &AppState) {
        let cfg_json = cfg_json_for(state, &self.describe.id, None);
        if let Some(raw) = self.call_event("startup", &cfg_json).await {
            apply_side_effects(state, &self.describe.id, &raw);
        }
    }

    async fn patrol(&self, state: &AppState) {
        let cfg_json = cfg_json_for(state, &self.describe.id, None);
        if let Some(raw) = self.call_event("patrol", &cfg_json).await {
            apply_side_effects(state, &self.describe.id, &raw);
        }
    }

    async fn after_backup(&self, state: &AppState, task_id: &str) -> Option<u64> {
        let cfg_json = cfg_json_for(state, &self.describe.id, Some(task_id));
        let raw = self.call_event("after_backup", &cfg_json).await?;
        apply_side_effects(state, &self.describe.id, &raw);
        serde_json::from_str::<Value>(&raw)
            .ok()?
            .get("count")?
            .as_u64()
    }

    async fn reload(&self, state: &AppState) {
        let cfg_json = cfg_json_for(state, &self.describe.id, None);
        if let Some(raw) = self.call_event("reload", &cfg_json).await {
            apply_side_effects(state, &self.describe.id, &raw);
        }
    }

    /// 插件自注册的定时任务到点回调
    ///
    /// 契约：事件名恒为 `"timer"`，`kind` 通过快照顶层的 `timer_kind` 字段给出 ——    /// 这样插件只需一个分支处理「定时器」，不必按 kind 去拼事件名。
    async fn timer(&self, state: &AppState, kind: &str) {
        let base = cfg_json_for(state, &self.describe.id, None);
        // 把 kind 注入快照（解析失败则退化为原样字符串：插件仍能收到 timer 事件）
        let cfg_json = match serde_json::from_str::<Value>(&base) {
            Ok(Value::Object(mut m)) => {
                m.insert("timer_kind".to_string(), Value::String(kind.to_string()));
                Value::Object(m).to_string()
            }
            _ => base,
        };
        if let Some(raw) = self.call_event("timer", &cfg_json).await {
            apply_side_effects(state, &self.describe.id, &raw);
        }
    }

    /// 下发宿主能力表（外置 C ABI 插件的真实实现；见固有方法 [`CApiEnhance::bind_host`]）
    fn bind_host(&self, state: &AppState) -> bool {
        CApiEnhance::bind_host(self, state)
    }

    async fn health_check(
        &self,
        state: &AppState,
        cfg: &crate::infra::config::AppConfig,
    ) -> Vec<CheckOutcome> {
        let Some(f) = self.table.0.health_json else {
            return Vec::new();
        };
        // 快照不含插件配置（ADR-021）⇒ 无需解密，也就不必取配置锁
        let cfg_json = CfgSnapshot::from_config(cfg).to_json();
        let table = self.table;
        let raw = match self
            .call_on_worker(move || call1(table, f, &cfg_json))
            .await
            .flatten()
        {
            Some(s) => s,
            None => return Vec::new(),
        };
        apply_side_effects(state, &self.describe.id, &raw);
        parse_health(&raw)
            .into_iter()
            .map(CheckOutcome::from)
            .collect()
    }

    /// 卸载清除：调用插件主表的 `destroy` 回调（若实现），供插件清理自身状态。
    fn destroy(&self) {
        let Some(f) = self.table.0.destroy else {
            return;
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f()));
        tracing::info!(plugin = %self.describe.id, "插件 destroy 回调已调用（自管数据由宿主随后清除）");
    }
}

/// 体检返回值：兼容两种形状
///
/// - 裸数组 `[{"key","title","status","detail","hint"}, …]`（最初的形式）；
/// - 对象 `{"checks":[…], "alerts":[…], "resolve":[…]}` —— 让体检也能声明告警
///   （副作用由 [`apply_side_effects`] 在解析前统一处理，这里只取 `checks`）。
///
/// 解析失败一律退化成空表：体检接口不该因为某个插件返回值走形而 500。
pub fn parse_health(raw: &str) -> Vec<AbiCheck> {
    match serde_json::from_str::<Value>(raw) {
        Ok(v) if v.is_array() => serde_json::from_value::<Vec<AbiCheck>>(v).unwrap_or_default(),
        Ok(v) => v
            .get("checks")
            .cloned()
            .and_then(|c| serde_json::from_value::<Vec<AbiCheck>>(c).ok())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// 体检项 JSON（与 `CheckOutcome` 同形，单独定义以固定契约）
#[derive(serde::Deserialize)]
pub struct AbiCheck {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub hint: Option<String>,
}

impl From<AbiCheck> for CheckOutcome {
    fn from(c: AbiCheck) -> Self {
        Self {
            key: c.key,
            title: c.title,
            status: if c.status.is_empty() {
                "ok".to_string()
            } else {
                c.status
            },
            detail: c.detail,
            hint: c.hint,
        }
    }
}

/// 动作调用
///
/// 契约：宿主把 `request_json = {"body": <前端提交的 JSON>, "cfg": <配置快照>}`
/// 交给插件的 `action_json(action, request_json)`，插件返回任意 JSON（原样回给前端）。
/// 动作调用（FFI 在**阻塞线程池**上执行，理由同 [`CApiEnhance::call_event`]）
async fn action_call(
    state: AppState,
    table: AbiTable,
    plugin_id: String,
    action: String,
    body: String,
    method: &str,
) -> Value {
    let body_value: Value = serde_json::from_str(&body).unwrap_or_else(|_| serde_json::json!({}));
    let request = serde_json::json!({
        "body": body_value,
        "cfg": serde_json::from_str::<Value>(&cfg_json_for(&state, &plugin_id, None))
            .unwrap_or_else(|_| serde_json::json!({})),
        // HTTP 动词：插件据此区分「读」（GET，用于表单回显 `echo`）
        // 与「写」（POST，保存）。缺了它插件无法实现回显契约。
        "method": method,
    })
    .to_string();

    let Some(f) = table.0.action_json else {
        return serde_json::json!({
            "success": false,
            "error": format!("该插件未实现动作入口（action_json）：{action}"),
        });
    };
    let action_for_err = action.clone();
    // 同样走该插件的专属线程（沙箱载体）
    let text = super::worker::run_on_plugin_thread(
        &plugin_id,
        super::worker::policy_for(&plugin_id, &[]),
        move || call2(table, f, &action, &request),
    )
    .await
    .flatten();
    match text {
        Some(text) => {
            // 动作返回值同样支持声明式告警（A 方案）：token 失效之类的故障，
            // 用户点「检查」时就能进告警流，不必等下一次巡检。
            apply_side_effects(&state, &plugin_id, &text);
            // 排空屏障：插件在本次动作里经能力表上报的告警/审计也应立刻可见
            // （否则用户点完「检查」，告警要等一下才出现在消息提醒里）。
            state
                .host_effects
                .flush(std::time::Duration::from_millis(500))
                .await;
            serde_json::from_str::<Value>(&text).unwrap_or_else(|e| {
                serde_json::json!({
                    "success": false,
                    "error": format!("插件返回的不是合法 JSON：{e}"),
                    "raw": text.chars().take(500).collect::<String>(),
                })
            })
        }
        None => serde_json::json!({
            "success": false,
            "error": format!("插件未处理该动作：{action_for_err}"),
        }),
    }
}

// ── FFI 调用包装（catch_unwind + C 字符串 + 由插件释放）────────────────────

/// 读取并释放插件返回的字符串（NULL → None）
fn take_string(p: *mut c_char, free: extern "C" fn(*mut c_char)) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    free(p);
    Some(s)
}

/// 调用无参回调；插件 panic 时记录并当作"无内容"（不让它跨 FFI 展开）
fn call0_opt(table: AbiTable, f: JsonFn0) -> Option<String> {
    let free = table.0.free_str;
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f())).ok()?;
    take_string(r, free)
}

fn call1(table: AbiTable, f: JsonFn1, a: &str) -> Option<String> {
    let ca = CString::new(a).ok()?;
    let free = table.0.free_str;
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(ca.as_ptr()))).ok()?;
    take_string(r, free)
}

fn call2(table: AbiTable, f: JsonFn2, a: &str, b: &str) -> Option<String> {
    let ca = CString::new(a).ok()?;
    let cb = CString::new(b).ok()?;
    let free = table.0.free_str;
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(ca.as_ptr(), cb.as_ptr())))
        .ok()?;
    take_string(r, free)
}
