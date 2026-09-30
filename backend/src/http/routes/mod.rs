//! REST 路由（axum）——**按职责拆分**后的入口模块
//!
//! 这里只做三件事：声明子模块、组装 [`router`]、以及**增强插件动作分发**
//! （它是唯一的通配路由，逻辑上属于路由装配）。
//!
//! 拆分原则：DTO 集中在 [`types`]，跨模块共享的辅助在 [`common`]，
//! 其余按**路由前缀/职责**分文件（plugins / targets / tasks / restore /
//! config / logs / audit / keys）。原先单文件 4464 行，改动一处要在一屏里找半天。
//!
//! 公开面保持不变：`http::routes::router` 仍为 `pub`，`run_task_now` 与
//! `raise_alert_once` 仍从本模块 `pub(crate)` re-export（外部调用点零改动）。

mod audit;
mod common;
mod config;
mod keys;
mod logs;
mod market;
mod plugins;
mod restore;
mod targets;
mod tasks;
mod types;

// 外部（main.rs / scheduler.rs / plugin 层）依赖的入口，统一从这里导出
pub(crate) use common::raise_alert_once;
pub(crate) use tasks::run_task_now;

use types::HealthResponse;
use axum::extract::State;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;

use crate::http::ws;
use crate::AppState;

// 各子模块的 handler（按职责拆分后，router 在这里统一装配）
use audit::{audit_clear, audit_list, schedule_preview, setup_check};
use config::{config_export, config_get, config_import, config_save, user_info, webdav_save};
use keys::{
    alerts_clear, alerts_get, keys_backup_ack, keys_export, keys_generate, keys_get, keys_set,
    webhook_save, webhook_test,
};
use logs::{logs_clear, logs_download, logs_get};
use plugins::{
    plugin_install, plugin_parallel, plugin_purge, plugin_reload, plugin_set_enabled,
    plugin_uninstall, plugins_list,
};
use restore::{restore_files, restore_prune, restore_run, restore_tree};
use targets::{target_delete, target_parallel, target_save, target_test, targets_list};
use tasks::{backup_run, task_delete, task_run, task_save, tasks_list};

// ── Handler ────────────────────────────────────

async fn health(State(_state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// 构建应用路由（不含 /api 前缀，由 main.rs nest("/api") 统一加前缀）
///
/// 核心只挂**自身**路由；增强功能的接口由插件自带（见文件末尾对
/// `EnhancePlugin::routes()` 的挂载，统一前缀 `/api/p/<插件id>`）。
pub fn router(state: AppState) -> Router {
    let mut core = Router::new()
        .route("/health", get(health))
        .route("/plugins", get(plugins_list))
        .route("/plugins/reload", post(plugin_reload))
        .route("/plugins/install", post(plugin_install))
        .route("/plugins/:file/uninstall", post(plugin_uninstall))
        .route("/plugins/:id/purge", post(plugin_purge))
        // 按插件启用/禁用（运行时生效，无需重启）
        .route("/plugins/:id/enable", post(plugin_set_enabled))
        .route("/plugins/:id/parallel", post(plugin_parallel))
        // ── 插件市场（默认关闭；关闭时不发起任何网络请求）──
        .route("/market/catalog", get(market::market_catalog))
        .route("/market/install", post(market::market_install))
        .route("/market/check-updates", post(market::market_check_updates))
        .route("/market/refresh", post(market::market_refresh))
        .route("/ws", get(ws::ws_handler))
        .route("/webdav/config", post(webdav_save))
        .route("/user/info", get(user_info))
        .route("/config", get(config_get).post(config_save))
        .route("/config/export", post(config_export))
        .route("/config/import", post(config_import))
        // 多目标 / 多任务（ADR-014）
        .route("/targets", get(targets_list).post(target_save))
        .route("/targets/:id/delete", post(target_delete))
        .route("/targets/:id/test", post(target_test))
        // 并发度**按目标**配置（同一个插件可被多个目标实例化，不能共用一份）
        .route("/targets/:id/parallel", post(target_parallel))
        .route("/tasks", get(tasks_list).post(task_save))
        .route("/tasks/:id/delete", post(task_delete))
        .route("/tasks/:id/run", post(task_run))
        .route("/backup/run", post(backup_run))
        .route("/restore/files", get(restore_files))
        .route("/restore/tree", get(restore_tree))
        .route("/restore/run", post(restore_run))
        .route("/restore/prune", post(restore_prune))
        .route("/logs", get(logs_get))
        .route("/logs/clear", post(logs_clear))
        .route("/logs/download", get(logs_download))
        .route("/audit/clear", post(audit_clear))
        .route("/schedule/preview", post(schedule_preview))
        .route("/setup/check", get(setup_check))
        .route("/audit", get(audit_list))
        .route("/keys", get(keys_get).post(keys_set))
        .route("/keys/generate", post(keys_generate))
        .route("/keys/export", post(keys_export))
        .route("/keys/backup-ack", post(keys_backup_ack))
        .route("/alerts", get(alerts_get).delete(alerts_clear))
        .route("/notify/webhook", post(webhook_save))
        .route("/notify/webhook/test", post(webhook_test));

    // 插件路由：`/p/<插件id>/<动作…>`（如 /p/kzwr/user、/p/kzwr/trash/empty）
    //
    // **不在启动时逐插件嵌套**，而是用一条兜底路由在**请求时**查注册表：
    // 这样才能支持运行时启停插件（禁用后立即 404、重新启用立即恢复），
    // 也让新加载的插件无需重启即可提供服务。核心仍不感知具体插件的路径。
    core = core.route("/p/:plugin_id/*action", axum::routing::any(plugin_dispatch));
    core.with_state(state)
}

/// 把 `/api/p/<插件id>/<动作…>` 分发给该插件自己的 `routes()`
///
/// 分发方式：从注册表取插件 → 构造其子路由 → 用 `tower::ServiceExt::oneshot`
/// 直接在当前请求上执行一次。这样插件侧仍是普通的 axum `Router`（写起来不变），
/// 而宿主获得「按 id 动态查找」的能力。
///
/// 插件不存在 / 被禁用 → 404（不泄漏内部原因，但日志留痕）。
async fn plugin_dispatch(
    State(state): State<AppState>,
    axum::extract::Path((plugin_id, action)): axum::extract::Path<(String, String)>,
    req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use tower::ServiceExt;

    // 被禁用的插件按「不存在」处理：禁用应立即生效（前端会拿到 404）
    let Some(plugin) = state.plugins.enhance_plugin_enabled(&plugin_id) else {
        tracing::debug!(plugin = %plugin_id, "插件路由请求：插件不存在或已被禁用");
        return (
            axum::http::StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({
                "error": format!("插件 {plugin_id} 不存在或已被禁用")
            })),
        )
            .into_response();
    };

    // 还原子路由内部的相对路径：把 `/p/<id>` 前缀剥掉，保留动作部分（含多段）与查询串
    let sub_path = format!("/{}", action.trim_start_matches('/'));
    let query = req.uri().query().map(|s| format!("?{s}")).unwrap_or_default();
    let new_pq = match format!("{sub_path}{query}").parse::<axum::http::uri::PathAndQuery>() {
        Ok(pq) => pq,
        Err(_) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": "插件动作路径非法" })),
            )
                .into_response()
        }
    };
    let (mut parts, body) = req.into_parts();
    parts.uri = {
        let mut u = parts.uri.into_parts();
        u.path_and_query = Some(new_pq);
        axum::http::Uri::from_parts(u).unwrap_or_else(|_| axum::http::Uri::from_static("/"))
    };
    // 关键：清掉外层路由写入的 request extensions。
    //
    // axum 的 `Path` 提取器从 **extensions** 里读 `UrlParams`，而外层路由已经写了
    // `:plugin_id` + `*action` 两个参数。若不清理，插件侧的 `Path<String>` 会看到
    // 3 个参数（2 外层 + 1 内层），直接报
    // 「Wrong number of path arguments for `Path`. Expected 1 but got 3」。
    // 分发相当于「重新进入一次路由」，故把上一层的参数痕迹（含 MatchedPath/OriginalUri）全部抹掉。
    parts.extensions.clear();
    let sub_req = axum::http::Request::from_parts(parts, body);

    match plugin.routes().with_state(state).oneshot(sub_req).await {
        Ok(resp) => resp.into_response(),
        Err(e) => {
            // `oneshot` 的 Infallible 错误在实践中不会出现；留个兜底避免 panic
            tracing::warn!(plugin = %plugin_id, err = ?e, "插件路由分发失败");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({ "error": "插件路由分发失败" })),
            )
                .into_response()
        }
    }
}
