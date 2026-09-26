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
    fn call_available(&self, cfg: &crate::infra::config::AppConfig) -> bool {
        let Some(f) = self.table.0.available_json else {
            return false;
        };
        let cfg_json = CfgSnapshot::from_config(cfg).to_json();
        let raw = match call1(self.table, f, &cfg_json) {
            Some(s) => s,
            None => return false,
        };
        serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|v| v.get("available").and_then(|x| x.as_bool()))
            .unwrap_or(false)
    }

    /// 处理事件/体检返回中的**声明式告警**（`alerts: [{level,message}]`）
    ///
    /// 插件不再回调宿主，而是把要报的告警随着返回值一起带回来，由宿主去重并落库。
    fn apply_alerts(&self, state: &AppState, raw: &str) {
        let Ok(v) = serde_json::from_str::<Value>(raw) else {
            return;
        };
        let Some(alerts) = v.get("alerts").and_then(|x| x.as_array()) else {
            return;
        };
        let id = self.describe.id.clone();
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
                crate::domain::alerts::AlertSource::Plugin(id.clone()),
                msg.to_string(),
            );
        }
    }

    /// 调用生命周期事件（`event_json` 可选实现）
    fn call_event(&self, event: &str, cfg: &crate::infra::config::AppConfig) -> Option<String> {
        let f = self.table.0.event_json?;
        let cfg_json = CfgSnapshot::from_config(cfg).to_json();
        call2(self.table, f, event, &cfg_json)
    }

    /// 从 `AppState` 读配置（事件/体检等入口用）
    fn cfg_of(state: &AppState) -> crate::infra::config::AppConfig {
        let mgr = state.config.lock().unwrap();
        mgr.load().unwrap_or_default()
    }
}

/// 从 `AppState` 读配置并构造快照 JSON（动作接口用，保持与 available/health 同口径）
fn cfg_json_of(state: &AppState) -> String {
    let cfg = CApiEnhance::cfg_of(state);
    CfgSnapshot::from_config(&cfg).to_json()
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

    fn available(&self, cfg: &crate::infra::config::AppConfig) -> bool {
        self.call_available(cfg)
    }

    fn ui(&self) -> Option<PluginUi> {
        self.describe.ui.clone()
    }

    /// 动作路由：`/api/p/<插件id>/<action...>`（GET 的 query 与 POST 的 body 都按 JSON 传给插件）
    ///
    /// 用 `/*action` 通配支持**多段动作名**（如 `trash/empty`），前端 api 拼接零改动。
    fn routes(&self) -> Router<AppState> {
        let table = self.table;
        let get_handler = move |Path(action): Path<String>,
                                State(state): State<AppState>,
                                Query(q): Query<HashMap<String, String>>| async move {
            let action = action.trim_start_matches('/').to_string();
            let body = serde_json::to_string(&q).unwrap_or_else(|_| "{}".to_string());
            axum::Json(action_call(&state, table, &action, &body))
        };
        let post_handler = move |Path(action): Path<String>,
                                 State(state): State<AppState>,
                                 body: Option<axum::Json<Value>>| async move {
            let action = action.trim_start_matches('/').to_string();
            let body = body
                .map(|axum::Json(v)| v.to_string())
                .unwrap_or_else(|| "{}".to_string());
            axum::Json(action_call(&state, table, &action, &body))
        };
        Router::new().route("/*action", get(get_handler).post(post_handler))
    }

    async fn on_startup(&self, state: &AppState) {
        if let Some(raw) = self.call_event("startup", &Self::cfg_of(state)) {
            self.apply_alerts(state, &raw);
        }
    }

    async fn patrol(&self, state: &AppState) {
        if let Some(raw) = self.call_event("patrol", &Self::cfg_of(state)) {
            self.apply_alerts(state, &raw);
        }
    }

    async fn after_backup(&self, state: &AppState) -> Option<u64> {
        let raw = self.call_event("after_backup", &Self::cfg_of(state))?;
        self.apply_alerts(state, &raw);
        serde_json::from_str::<Value>(&raw)
            .ok()?
            .get("count")?
            .as_u64()
    }

    async fn reload(&self, state: &AppState) {
        if let Some(raw) = self.call_event("reload", &Self::cfg_of(state)) {
            self.apply_alerts(state, &raw);
        }
    }

    async fn health_check(
        &self,
        state: &AppState,
        cfg: &crate::infra::config::AppConfig,
    ) -> Vec<CheckOutcome> {
        let cfg_json = CfgSnapshot::from_config(cfg).to_json();
        let Some(f) = self.table.0.health_json else {
            return Vec::new();
        };
        let raw = match call1(self.table, f, &cfg_json) {
            Some(s) => s,
            None => return Vec::new(),
        };
        self.apply_alerts(state, &raw);
        serde_json::from_str::<Vec<AbiCheck>>(&raw)
            .map(|v| v.into_iter().map(CheckOutcome::from).collect())
            .unwrap_or_default()
    }

    /// 卸载清除：调用插件主表的 `destroy` 回调（若实现），供插件清理自身状态。
    fn destroy(&self) {
        let Some(f) = self.table.0.destroy else {
            return;
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        tracing::info!(plugin = %self.describe.id, "插件 destroy 回调已调用（自管数据由宿主随后清除）");
    }
}

/// 体检项 JSON（与 `CheckOutcome` 同形，单独定义以固定契约）
#[derive(serde::Deserialize)]
struct AbiCheck {
    #[serde(default)]
    key: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    hint: Option<String>,
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
fn action_call(state: &AppState, table: AbiTable, action: &str, body: &str) -> Value {
    let body_value: Value = serde_json::from_str(body).unwrap_or_else(|_| serde_json::json!({}));
    let request = serde_json::json!({
        "body": body_value,
        "cfg": serde_json::from_str::<Value>(&cfg_json_of(state)).unwrap_or_else(|_| serde_json::json!({})),
    })
    .to_string();

    let Some(f) = table.0.action_json else {
        return serde_json::json!({
            "success": false,
            "error": format!("该插件未实现动作入口（action_json）：{action}"),
        });
    };
    match call2(table, f, action, &request) {
        Some(text) => serde_json::from_str::<Value>(&text).unwrap_or_else(|e| {
            serde_json::json!({
                "success": false,
                "error": format!("插件返回的不是合法 JSON：{e}"),
                "raw": text.chars().take(500).collect::<String>(),
            })
        }),
        None => serde_json::json!({
            "success": false,
            "error": format!("插件未处理该动作：{action}"),
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
