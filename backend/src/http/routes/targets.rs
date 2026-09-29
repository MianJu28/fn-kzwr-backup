//! 目标管理（多目标，ADR-014 / 019 / 020）
//!
//! 每个目标 = 一个目标插件实例 + 一套凭据 + **自己的**自定义字段与并发度。
//! `target_view` 是回显的唯一出口：凭据**永不**回传明文。

use axum::extract::State;
use axum::response::Json;

use crate::AppState;

use super::common::{err, new_id};
use super::types::*;

/// 目标视图（供 `/api/targets`；凭据永不返回，密码只回显「是否已设置」）
pub(super) fn target_view(
    state: &AppState,
    t: &crate::infra::config::TargetConfig,
) -> serde_json::Value {
    let (user, pass_set) = {
        let mgr = state.config.lock().unwrap();
        match mgr.target_credentials(t) {
            Ok((u, p)) => (u, p.is_some()),
            Err(_) => (None, false),
        }
    };
    // 插件自定义字段回显：**敏感字段只回布尔**（明文绝不回传前端，本项目硬约束）
    let field_values: serde_json::Map<String, serde_json::Value> = {
        let plugin = state.plugins.target_plugin(&t.kind);
        let declared = plugin.map(|p| p.form_fields()).unwrap_or_default();
        let mgr = state.config.lock().unwrap();
        let plain = t.custom_fields_plain(&mgr);
        let mut m = serde_json::Map::new();
        for f in &declared {
            if matches!(f.key.as_str(), "url" | "username" | "password") {
                continue; // 这三个走上面既有字段
            }
            let has = t.fields.contains_key(&f.key);
            if f.is_secret() {
                // 只给「是否已设置」，绝不回传明文
                m.insert(f.key.clone(), serde_json::Value::Bool(has));
            } else {
                m.insert(
                    f.key.clone(),
                    match plain.get(&f.key) {
                        Some(v) => serde_json::Value::String(v.clone()),
                        None => serde_json::Value::Null,
                    },
                );
            }
        }
        // 未被当前插件声明的历史字段也带上（避免插件改声明后前端看不到）
        for k in t.fields.keys() {
            if !m.contains_key(k) && !matches!(k.as_str(), "url" | "username" | "password") {
                let secret = declared.iter().find(|f| &f.key == k).map(|f| f.is_secret());
                if secret == Some(true) {
                    m.insert(k.clone(), serde_json::Value::Bool(true));
                } else {
                    m.insert(
                        k.clone(),
                        match plain.get(k) {
                            Some(v) => serde_json::Value::String(v.clone()),
                            None => serde_json::Value::Null,
                        },
                    );
                }
            }
        }
        m
    };
    serde_json::json!({
        "id": t.id,
        "name": t.name,
        "kind": t.kind,
        "url": t.url.clone().unwrap_or_default(),
        "username": user,
        "password_set": pass_set,
        "enabled": t.enabled,
        "ready": state.targets.is_ready(&t.id),
        "backend": state.targets.describe(&t.id),
        // **本目标**的上传并发度（0/1 = 顺序；≥2 = 并发路数；null = 未设置，沿用插件级/插件声明）
        "parallel": t.parallel,
        // 该目标所用插件是否支持并发回传（能力由插件声明，决定是否显示并发输入框）
        "supports_plan": state
            .plugins
            .target_plugin(&t.kind)
            .map(|p| p.supports_plan())
            .unwrap_or(false),
        // 插件自定义字段的值（敏感字段为 `true`/`false` 表示「是否已设置」）
        "fields": field_values,
        "tasks": 0, // 由调用方填充（引用该目标的任务数）
    })
}

// ── 目标管理 API（多目标，ADR-014）────────────────────────────────────

/// `GET /api/targets`：目标列表（凭据不回传；含就绪状态与引用它的任务数）
pub(super) async fn targets_list(State(state): State<AppState>) -> Json<serde_json::Value> {
    let (targets, tasks) = {
        let mgr = state.config.lock().unwrap();
        match mgr.load() {
            Ok(c) => (c.targets, c.tasks),
            Err(_) => (Vec::new(), Vec::new()),
        }
    };
    let list: Vec<serde_json::Value> = targets
        .iter()
        .map(|t| {
            let mut v = target_view(&state, t);
            let used = tasks.iter().filter(|x| x.target_id == t.id).count();
            v["tasks"] = serde_json::json!(used);
            v
        })
        .collect();
    Json(serde_json::json!({ "targets": list }))
}

/// `POST /api/targets`：新建/更新目标（保存前实测连通性；`password` 缺省 = 不修改）
pub(super) async fn target_save(
    State(state): State<AppState>,
    Json(body): Json<TargetSaveRequest>,
) -> Json<serde_json::Value> {
    let kind = body.kind.clone().unwrap_or_else(|| "webdav".to_string());
    let Some(plugin) = state.plugins.target_plugin(&kind) else {
        return Json(err(format!("未注册类型为 {kind} 的目标插件")));
    };

    let (mut cfg, existing) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        let existing = body
            .id
            .as_deref()
            .and_then(|id| cfg.target_by_id(id).cloned());
        (cfg, existing)
    };

    let id = existing
        .as_ref()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| new_id("t"));
    // well-known 键（url/username/password）**两处都收**：
    // - 顶层字段是既有契约（老前端 / 直接调 API 的脚本用）；
    // - `fields` 里同名键是插件声明式表单的提交路径（`target.form` 把 url 声明成普通字段）。
    // 两者取「本次请求真正提供了值的那个」，都没有才沿用原值。
    let url = body
        .url
        .clone()
        .or_else(|| body.fields.get("url").cloned())
        .unwrap_or_else(|| existing.as_ref().and_then(|t| t.url.clone()).unwrap_or_default());
    let name = body
        .name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| existing.as_ref().map(|t| t.name.clone()))
        .unwrap_or_else(|| format!("目标 {id}"));

    // 凭据：本次提供则实测 + 加密；未提供则沿用原凭据（编辑场景不改密码）
    //
    // 注意：实测是 await，**绝不能持有 config 锁**（MutexGuard 跨 await 不但阻塞其它请求，
    // 且再次加锁会自锁），因此这里按「先实测、后加密」两步走。
    // 该插件是否用凭据：不用凭据的目标（如本地目录）允许留空，且不做连通性实测
    // —— 否则用户在「目标」页根本建不出这类目标（旧行为正是一律强制要求账号密码）。
    let needs_creds = plugin.needs_credentials();
    // 与 url 同理：顶层字段与 `fields` 同名键都收（插件声明式表单走后者）
    let provided_user = body
        .username
        .clone()
        .or_else(|| body.fields.get("username").cloned())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let provided_pass = body
        .password
        .clone()
        .or_else(|| body.fields.get("password").cloned());
    let (enc_user, enc_pass, warning) = match provided_user {
        Some(user) => {
            let pass = provided_pass.unwrap_or_default();
            if pass.is_empty() {
                return Json(err("新填写用户名时必须同时提供密码"));
            }
            let mut warning = None;
            // 实测只对「用凭据」的目标有意义：不用凭据的插件没有可测的连接，
            // 其 `url` 语义由插件自己解释（如本地路径），实测只会误报失败。
            if body.test && needs_creds {
                // 宿主不再代存插件配置（ADR-021）⇒ 没有插件级 config 可注入。
                // 目标级自定义字段由 `build()` 从 `TargetConfig.fields` 合并注入；
                // 「保存前实测」时目标尚未落库，故只带空对象测 url/凭据本身。
                let plugin_cfg = serde_json::Value::Object(Default::default());
                match plugin
                    .verify_with_config(Some(&url), &user, &pass, plugin_cfg)
                    .await
                {
                    Ok(real) => warning = Some(format!("连接成功：{real}")),
                    Err(e) => return Json(err(e)),
                }
            }
            let mgr = state.config.lock().unwrap();
            match (mgr.encrypt_field(&user), mgr.encrypt_field(&pass)) {
                (Ok(u), Ok(p)) => (Some(u), Some(p), warning),
                _ => return Json(err("凭据加密失败")),
            }
        }
        None => {
            let old = existing.as_ref();
            // 不用凭据的插件：允许「本来就没有凭据」，不报错
            if !needs_creds {
                (
                    old.and_then(|t| t.username_enc.clone()),
                    old.and_then(|t| t.password_enc.clone()),
                    None,
                )
            } else if old.map(|t| t.configured()).unwrap_or(false) {
                (
                    old.and_then(|t| t.username_enc.clone()),
                    old.and_then(|t| t.password_enc.clone()),
                    None,
                )
            } else {
                return Json(err("新目标必须填写用户名与密码"));
            }
        }
    };

    // ── 插件自定义字段 ────────────────────────────────────────────────
    //
    // 键名由插件的 `describe_json.target.form` 声明，宿主**不解释语义**：
    // - well-known 键（url/username/password）已由上面的逻辑归位到既有存储，这里跳过；
    // - 其余键：按字段声明的 `secret` 决定是否加密，存入该目标自己的 `fields`。
    //
    // 未出现在本次请求里的键**保持原值**（与 `password` 缺省不改语义一致），
    // 这样前端可以只提交改动的字段。
    let declared = plugin.form_fields();
    let mut new_fields = existing
        .as_ref()
        .map(|t| t.fields.clone())
        .unwrap_or_default();
    {
        let mgr = state.config.lock().unwrap();
        for f in &declared {
            // well-known 键走既有存储，不重复落 fields
            if matches!(f.key.as_str(), "url" | "username" | "password") {
                continue;
            }
            let Some(raw) = body.fields.get(&f.key) else {
                continue; // 未提交 = 保持原值
            };
            if !crate::plugin::cabi::config_key_ok(&f.key) {
                return Json(err(format!(
                    "非法字段键 {}（只允许 [A-Za-z0-9_-]，且不超过 64 字符、不含点号）",
                    f.key
                )));
            }
            // 空串 = 清除该字段（与旧 `plugin_data` 的「空值即删除」同语义）
            if raw.is_empty() {
                new_fields.remove(&f.key);
                continue;
            }
            let stored = if f.is_secret() {
                match mgr.encrypt_field(raw) {
                    Ok(v) => v,
                    Err(e) => return Json(err(format!("字段 {} 加密失败：{e:#}", f.key))),
                }
            } else {
                raw.clone()
            };
            new_fields.insert(f.key.clone(), stored);
        }
    }

    let target = crate::infra::config::TargetConfig {
        id: id.clone(),
        name,
        kind,
        url: Some(url.trim_end_matches('/').to_string()),
        username_enc: enc_user,
        password_enc: enc_pass,
        enabled: body.enabled.unwrap_or(true),
        // 编辑目标时**保留**该目标自己的并发度（并发度在目标页单独编辑，
        // 不该因为改了地址/凭据而被重置）
        parallel: existing.as_ref().and_then(|t| t.parallel),
        fields: new_fields,
    };
    match cfg.targets.iter_mut().find(|t| t.id == id) {
        Some(slot) => *slot = target,
        None => cfg.targets.push(target),
    }
    let saved = { state.config.lock().unwrap().save(&cfg) };
    if let Err(e) = saved {
        return Json(err(format!("{:#}", e)));
    }
    state.reload_targets(&cfg);
    let t = cfg.target_by_id(&id).cloned().unwrap_or_default();
    state.audit.record(
        "target.save",
        format!("保存目标「{}」（{}）", t.name, t.url.clone().unwrap_or_default()),
        true,
        None,
    );
    Json(serde_json::json!({ "success": true, "target": target_view(&state, &t), "warning": warning, "error": null }))
}

/// `POST /api/targets/:id/delete`：删除目标（被任务引用时拒绝）
pub(super) async fn target_delete(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<serde_json::Value> {
    let mut cfg = { state.config.lock().unwrap().load().unwrap_or_default() };
    let using = cfg.tasks_using_target(&id);
    if !using.is_empty() {
        return Json(err(format!(
            "该目标正被任务使用（{}），请先修改或删除这些任务",
            using.join("、")
        )));
    }
    let before = cfg.targets.len();
    cfg.targets.retain(|t| t.id != id);
    if cfg.targets.len() == before {
        return Json(err("目标不存在"));
    }
    if let Err(e) = state.config.lock().unwrap().save(&cfg) {
        return Json(err(format!("{:#}", e)));
    }
    state.reload_targets(&cfg);
    state.audit.record("target.delete", format!("删除目标 {id}"), true, None);
    Json(serde_json::json!({ "success": true, "error": null }))
}

/// `POST /api/targets/:id/test`：用已保存的凭据实测连通性
pub(super) async fn target_test(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<serde_json::Value> {
    let (target, creds) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        let Some(t) = cfg.target_by_id(&id).cloned() else {
            return Json(err("目标不存在"));
        };
        let creds = mgr.target_credentials(&t).unwrap_or((None, None));
        (t, creds)
    };
    let Some(plugin) = state.plugins.target_plugin(&target.kind) else {
        return Json(err(format!("未注册类型为 {} 的目标插件", target.kind)));
    };
    let (Some(user), Some(pass)) = creds else {
        return Json(err("该目标尚未配置用户名/密码"));
    };
    // 宿主不再代存插件配置（ADR-021）⇒ 没有插件级 config 可注入。
    // 目标级的自定义字段随目标配置解密注入（见 `CApiTarget::build`），
    // 但「测试连接」用的是已保存的 target，这里带上它自己的字段才准确。
    let plugin_cfg = {
        let mgr = state.config.lock().unwrap();
        serde_json::Value::Object(
            target
                .custom_fields_plain(&mgr)
                .into_iter()
                .map(|(k, v)| (k, serde_json::Value::String(v)))
                .collect(),
        )
    };
    match plugin
        .verify_with_config(target.url.as_deref(), &user, &pass, plugin_cfg)
        .await
    {
        Ok(url) => Json(serde_json::json!({ "success": true, "url": url, "error": null })),
        Err(e) => Json(err(e)),
    }
}

/// `POST /api/targets/:id/parallel`：设置**该目标**的上传并发路数
///
/// 并发度**按目标**存储（`TargetConfig.parallel`）—— 同一个插件（如 webdav）会被多个目标
/// 同时实例化（多账号各一套凭据、各自的网络条件），此前只有插件级一份，
/// 改一个目标会让所有同类型目标跟着变。
///
/// 只有该目标所用插件声明了 `supports_plan` 才可设置；保存后目标池热重建，下次备份即生效。
pub(super) async fn target_parallel(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: Option<axum::extract::Json<PluginParallelRequest>>,
) -> Json<serde_json::Value> {
    let kind = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        let Some(t) = cfg.target_by_id(&id) else {
            return Json(err(format!("目标不存在：{id}")));
        };
        t.kind.clone()
    };
    // 并发回传能力由插件声明（与插件级接口同一判据）
    let Some(plugin) = state.plugins.target_plugin(&kind) else {
        return Json(err(format!("未注册类型为 {kind} 的目标插件")));
    };
    if !plugin.supports_plan() {
        return Json(err(format!("目标 {id} 所用插件 {kind} 不支持并发回传")));
    }
    let v = body
        .and_then(|b| b.0.parallel)
        .unwrap_or(0)
        .min(crate::plugin::target_abi::MAX_PARALLEL);
    let cfg = {
        let mgr = state.config.lock().unwrap();
        let mut cfg = mgr.load().unwrap_or_default();
        let Some(t) = cfg.target_by_id_mut(&id) else {
            return Json(err(format!("目标不存在：{id}")));
        };
        t.parallel = Some(v);
        if let Err(e) = mgr.save(&cfg) {
            return Json(err(format!("{:#}", e)));
        }
        cfg
    };
    state.reload_targets(&cfg);
    state.audit.record(
        "target.parallel",
        format!("设置目标 {id} 上传并发路数 {v}"),
        true,
        None,
    );
    Json(serde_json::json!({ "success": true, "parallel": v, "error": null }))
}
