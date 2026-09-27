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

/// 校验插件主表（空指针 / `abi` 版本 / `size` 必需前缀）
///
/// 主表**冻结**（能力用独立表承载），故 `size` 只需 ≥ [`KzwrPluginAbi::REQUIRED_SIZE`]；
/// 新增能力不要求插件重编。
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
    let need = KzwrPluginAbi::REQUIRED_SIZE as usize;
    if (t.size as usize) < need {
        return Err(format!(
            "插件表长度 {} 小于宿主要求的必需前缀 {}（主表冻结：新增能力走独立能力表）",
            t.size, need
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
        tokio::task::spawn_blocking(move || call2(table, f, &event, &cfg_json))
            .await
            .ok()
            .flatten()
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
/// - `config`：写入本插件的 `plugin_data` 命名空间（age 加密落盘 + 审计只记键名）。
///   这让增强插件能持久化自己的配置（多账号、阈值…）而**不需要任何回调**，
///   下一次调用起 `cfg.self_config` 就能读回明文；
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
    apply_config_writeback(state, plugin_id, &v);
    apply_audit_records(state, plugin_id, &v);
}

/// 解析插件返回值里的 `config` 字段为 (set, remove)
///
/// 独立成函数是为了**可单测**：键名规则是插件与宿主之间最容易出错的地方
/// （曾经的 `percent.<id>` 就是因为点号被整批拒绝，而插件以为写成功了）。
///
/// 语义：**整批校验、整批失败** —— 任一非法键名直接报错，不部分生效，
/// 否则插件会误以为配置已持久化。
pub fn parse_writeback(v: &Value) -> Result<(Vec<(String, String)>, Vec<String>), (String, String)> {
    let Some(cfg_obj) = v.get("config") else {
        return Ok((Vec::new(), Vec::new()));
    };
    let set: Vec<(String, String)> = cfg_obj
        .get("set")
        .and_then(|x| x.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, val)| {
                    (
                        k.clone(),
                        match val {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let remove: Vec<String> = cfg_obj
        .get("remove")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    // 键名规则与宿主代存端点一致（命名空间键会被注入快照，必须严格限定）
    for k in set.iter().map(|(k, _)| k).chain(remove.iter()) {
        if !writeback_key_ok(k) {
            return Err((k.clone(), "空串/超过 64 字符/含 [A-Za-z0-9_-] 之外的字符（**点号不允许**）".to_string()));
        }
    }
    Ok((set, remove))
}

/// 回写键名是否合法（见 [`parse_writeback`]）
pub fn writeback_key_ok(k: &str) -> bool {
    !k.trim().is_empty()
        && k.len() <= 64
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 把已校验的 set/remove 写入指定插件的 `plugin_data` 命名空间（只改内存中的 cfg）
///
/// 与落盘/审计分开，是为了能在只有 `ConfigManager` 的单元测试里验证**隔离性**：
/// 写入只落在 `plugin_id` 自己的命名空间，别的插件读不到。
/// `remove` 用空串删除（与 [`crate::infra::config::ConfigManager::plugin_data_set`] 同语义）。
pub fn writeback_to_manager(
    mgr: &crate::infra::config::ConfigManager,
    cfg: &mut crate::infra::config::AppConfig,
    plugin_id: &str,
    set: &[(String, String)],
    remove: &[String],
) -> Result<(), String> {
    for (k, val) in set {
        if !writeback_key_ok(k) {
            return Err(format!("非法配置键：{k}"));
        }
        if let Err(e) = mgr.plugin_data_set(cfg, plugin_id, k, val) {
            return Err(format!("写入 {k} 失败：{e}"));
        }
    }
    for k in remove {
        if !writeback_key_ok(k) {
            return Err(format!("非法配置键：{k}"));
        }
        if let Err(e) = mgr.plugin_data_set(cfg, plugin_id, k, "") {
            return Err(format!("删除 {k} 失败：{e}"));
        }
    }
    Ok(())
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

/// 声明式自配置回写（见 [`apply_side_effects`] 的 `config` 字段）
fn apply_config_writeback(state: &AppState, plugin_id: &str, v: &Value) {
    let (set, remove) = match parse_writeback(v) {
        Ok(x) => x,
        Err((key, reason)) => {
            // 与宿主代存端点一致：非法键名一律拒绝
            tracing::warn!(plugin = %plugin_id, key = %key, reason = %reason, "插件回写了非法配置键，已忽略整批");
            return;
        }
    };
    if set.is_empty() && remove.is_empty() {
        return;
    }
    let mgr = state.config.lock().unwrap();
    let mut cfg = match mgr.load() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(plugin = %plugin_id, err = %e, "插件回写自配置失败：读取配置出错");
            return;
        }
    };
    if let Err(e) = writeback_to_manager(&mgr, &mut cfg, plugin_id, &set, &remove) {
        tracing::warn!(plugin = %plugin_id, err = %e, "插件回写自配置失败");
        return;
    }
    if let Err(e) = mgr.save(&cfg) {
        tracing::warn!(plugin = %plugin_id, err = %e, "插件回写自配置落盘失败");
        return;
    }
    drop(mgr);
    // 审计只记**键名与数量**，绝不记值（可能含凭据）
    let keys: Vec<&str> = set.iter().map(|(k, _)| k.as_str()).collect();
    state.audit.record(
        "plugin.data",
        format!(
            "插件 {plugin_id} 回写自配置（{} 项：{}{}）",
            keys.len() + remove.len(),
            keys.join("、"),
            if remove.is_empty() {
                String::new()
            } else {
                format!("；删除 {}", remove.join("、"))
            }
        ),
        true,
        None,
    );
}

/// 从 `AppState` 读出**指定插件视角**的配置快照 JSON
///
/// 快照里会注入该插件自己的明文自配置（`self_config`）——隔离由 `plugin_id` 保证：
/// 只取 `plugin_data[<plugin_id>]` 这一个命名空间，插件看不到其它插件的键。
/// **返回值含凭据，调用方不得写入日志。**
///
/// `after_backup_task` 仅 `after_backup` 事件传（告诉插件刚完成的是哪个任务）。
pub fn cfg_json_for(state: &AppState, plugin_id: &str, after_backup_task: Option<&str>) -> String {
    let mgr = state.config.lock().unwrap();
    let cfg = mgr.load().unwrap_or_default();
    let snap = CfgSnapshot::from_config(&cfg).with_self_config(&cfg, plugin_id, &mgr);
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

    fn available(&self, cfg: &crate::infra::config::AppConfig, mgr: &ConfigManager) -> bool {
        let snap = CfgSnapshot::from_config(cfg).with_self_config(cfg, &self.describe.id, mgr);
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

    async fn health_check(
        &self,
        state: &AppState,
        cfg: &crate::infra::config::AppConfig,
    ) -> Vec<CheckOutcome> {
        let Some(f) = self.table.0.health_json else {
            return Vec::new();
        };
        // 快照在**独立作用域**里构造：`MutexGuard` 不是 `Send`，
        // 必须确保它不跨越下面的 `.await`（否则 future 不 Send）。
        let cfg_json = {
            let mgr = state.config.lock().unwrap();
            CfgSnapshot::from_config(cfg)
                .with_self_config(cfg, &self.describe.id, &mgr)
                .to_json()
        };
        let table = self.table;
        let raw = match tokio::task::spawn_blocking(move || call1(table, f, &cfg_json))
            .await
            .ok()
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
    let text = tokio::task::spawn_blocking(move || call2(table, f, &action, &request))
        .await
        .ok()
        .flatten();
    match text {
        Some(text) => {
            // 动作返回值同样支持声明式告警（A 方案）：token 失效之类的故障，
            // 用户点「检查」时就能进告警流，不必等下一次巡检。
            apply_side_effects(&state, &plugin_id, &text);
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
