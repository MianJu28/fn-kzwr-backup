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
    // 实际生效的地址：url 留空时内置 WebDAV 会回退到官方默认地址。
    // 分开回传，前端才能在「留空」时告诉用户真正连的是哪里
    // （否则用户看到一片空白，误以为没配置）。
    let effective_url = if t.kind == "webdav" {
        crate::infra::target::webdav::effective_url(t.url.as_deref())
    } else {
        t.url.clone().unwrap_or_default()
    };
    serde_json::json!({
        "id": t.id,
        "name": t.name,
        "kind": t.kind,
        "url": t.url.clone().unwrap_or_default(),
        // 实际生效的地址：url 留空时插件会回退到官方默认地址。
        // 分开回传，前端才能在「留空」时告诉用户真正连的是哪里
        // （否则用户看到一片空白，误以为没配置）。
        "effective_url": effective_url,
        "username": user,
        "password_set": pass_set,
        "enabled": t.enabled,
        // 本目标是否走全局代理（前端据此在卡片上显示「经代理」并回显勾选）
        "use_proxy": t.use_proxy,
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

    // 第一次 load：只为拿到 `existing`（决定是否新建、沿用哪些原值）。
    // 真正的落库在下面**单次加锁**里重新 load 一次 —— 中间有 await（连通性实测），
    // 期间配置可能被别的请求改过，必须重新读。
    let existing = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        body.id
            .as_deref()
            .and_then(|id| cfg.target_by_id(id).cloned())
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
    // 判断「用户名是否真的改了」：前端编辑目标时会**回显**已有用户名（不清空），
    // 若只看「提交了用户名」就要求密码，那么目标一旦配好凭据，之后改名称/地址/
    // 并发/代理等任何字段都会被「必须同时提供密码」拦下 —— 用户根本没改用户名。
    //
    // 比较必须在**明文**上做（密文含随机 IV，直接比字符串永远不相等）。
    let same_username_as_saved = match (&provided_user, &existing) {
        (Some(user), Some(t)) => {
            let mgr = state.config.lock().unwrap();
            mgr.decrypt_field(&t.username_enc)
                .ok()
                .flatten()
                .map(|old| &old == user)
                .unwrap_or(false)
        }
        _ => false,
    };
    // 用户名没变且没填密码 ⇒ 视为「不改凭据」，整体跳过凭据更新与连通性实测
    let provided_user = if credential_is_unchanged(
        provided_user.as_deref(),
        provided_pass.as_deref(),
        same_username_as_saved,
    ) {
        None
    } else {
        provided_user
    };

    let (creds_plain, warning): (Option<(String, String)>, Option<String>) = match provided_user {
        Some(user) => {
            let pass = provided_pass.unwrap_or_default();
            if pass.is_empty() {
                return Json(err("修改用户名时必须同时提供密码"));
            }
            let mut warning = None;
            // 实测只对「用凭据」的目标有意义：不用凭据的插件没有可测的连接，
            // 其 `url` 语义由插件自己解释（如本地路径），实测只会误报失败。
            if body.test && needs_creds {
                // 宿主不再代存插件配置（ADR-021）⇒ 没有插件级 config 可注入。
                // 目标级自定义字段由 `build()` 从 `TargetConfig.fields` 合并注入；
                // 「保存前实测」时目标尚未落库，故只带空对象测 url/凭据本身。
                //
                // 但**代理必须带上**：用户本次勾了「使用代理」，实测却不走代理，
                // 就会出现「提示连接成功、保存后备份却连不上」的错配。
                let plugin_cfg = {
                    let mut obj = serde_json::Map::new();
                    let use_proxy = body.use_proxy.unwrap_or(false);
                    // ⚠️ 只取一次锁（`ConfigManager` 内部有缓存，但重复 lock 会阻塞自己）
                    if let Ok(guard) = state.config.lock() {
                        if let Ok(cfg) = guard.load() {
                            if let Some(p) = cfg.proxy.resolve_for(&guard, &url, use_proxy) {
                                obj.insert("__proxy".to_string(), p);
                            }
                        }
                    }
                    serde_json::Value::Object(obj)
                };
                // 空 url 传 `None`：让插件用自己的默认地址，并把实际使用的地址
                // 回显在 `warning` 里（避免「测通了但不知道测的是哪个地址」）。
                match plugin
                    .verify_with_config(
                        if url.trim().is_empty() { None } else { Some(&url) },
                        &user,
                        &pass,
                        plugin_cfg,
                    )
                    .await
                {
                    Ok(real) => warning = Some(format!("连接成功：{real}")),
                    Err(e) => return Json(err(e)),
                }
            }
            // **只做实测，不落库**：明文凭据带回上层，交给下面那一次加锁统一加密保存
            (Some((user, pass)), warning)
        }
        // 未提交凭据：沿用原值。是否需要凭据取决于 `needs_creds`，这一判断放在
        // 落库段（拿到最新配置后）做，因为 `existing` 可能已被并发请求改写。
        None => (None, None),
    };

    // ── 落库（**单次加锁**：load → 改 → save → reload 全在同一把锁内）──────
    //
    // 曾经是 4 次独立加锁（load / 加密凭据 / 加密字段 / save），两次并发保存会各自
    // 读到同一份旧配置再互相覆盖 —— 界面上连点两次「保存目标」就静默丢一次修改。
    // 实测连通性是 await，**不能**持锁做（MutexGuard 跨 await 会阻塞所有其它请求），
    // 所以结构固定为「先无锁实测 → 再单次加锁落库」，这是与并发性不冲突的最紧写法。
    let mut guard = state.config.lock().unwrap();
    // 重新 load：上面 await 期间别的请求可能已经改过配置（正是旧写法丢失修改的窗口）
    let mut cfg = guard.load().unwrap_or_default();
    let existing = body
        .id
        .as_deref()
        .and_then(|id| cfg.target_by_id(id).cloned());

    // 凭据：本次提供了明文 → 加密；否则沿用（重新 load 到的）原值
    let (enc_user, enc_pass) = match creds_plain {
        Some((user, pass)) => match (guard.encrypt_field(&user), guard.encrypt_field(&pass)) {
            (Ok(u), Ok(p)) => (Some(u), Some(p)),
            _ => return Json(err("凭据加密失败")),
        },
        None => {
            let old = existing.as_ref();
            if !needs_creds || old.map(|t| t.configured()).unwrap_or(false) {
                (
                    old.and_then(|t| t.username_enc.clone()),
                    old.and_then(|t| t.password_enc.clone()),
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
            match guard.encrypt_field(raw) {
                Ok(v) => v,
                Err(e) => return Json(err(format!("字段 {} 加密失败：{e:#}", f.key))),
            }
        } else {
            raw.clone()
        };
        new_fields.insert(f.key.clone(), stored);
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
        // `None` = 保持原值（老前端不传该字段时不能悄悄清掉用户的勾选）
        use_proxy: body
            .use_proxy
            .unwrap_or_else(|| existing.as_ref().map(|t| t.use_proxy).unwrap_or(false)),
    };
    match cfg.targets.iter_mut().find(|t| t.id == id) {
        Some(slot) => *slot = target,
        None => cfg.targets.push(target),
    }
    // 仍在锁内：save 与上面的 load 构成同一个原子步骤
    if let Err(e) = guard.save(&cfg) {
        return Json(err(format!("{:#}", e)));
    }
    let t = cfg.target_by_id(&id).cloned().unwrap_or_default();
    // ⚠️ **先放锁，再重建目标池**。
    //
    // `reload_targets` 会为每个目标重建实例池（新建线程）+ 调插件 `target_open`
    // （最坏等 30s）。此前它在持有 config 锁时执行 ⇒ 这段时间内**所有**请求
    // （读配置、看进度、健康检查、点其它按钮）全部排队 —— 用户感知就是
    // 「保存个配置要等好久，整个界面卡住」。
    // 放锁后再做，保存请求本身仍是秒回，重建在后台推进。
    drop(guard);
    state.reload_targets(&cfg);
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
        let mut obj: serde_json::Map<String, serde_json::Value> = target
            .custom_fields_plain(&mgr)
            .into_iter()
            .map(|(k, v)| (k, serde_json::Value::String(v)))
            .collect();
        // 代理与 `target_open` 用同一份判定（`ProxySettings::resolve_for`），
        // 否则「测试通过但备份失败」会变成常态。保留键 `__proxy` 由宿主剥离。
        if let Ok(cfg) = mgr.load() {
            if let Some(p) = cfg.proxy.resolve_for(
                &mgr,
                target.url.as_deref().unwrap_or(""),
                target.use_proxy,
            ) {
                obj.insert("__proxy".to_string(), p);
            }
        }
        serde_json::Value::Object(obj)
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

// ════════════════════════════════════════════════════════════════════════════
// 网络代理（全局一份；目标级开关在 `/api/targets` 的 use_proxy）
// ════════════════════════════════════════════════════════════════════════════

/// `GET /api/proxy`：读取当前代理设置（**不回传凭据明文**）
pub(super) async fn proxy_get(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mgr = state.config.lock().unwrap();
    let cfg = mgr.load().unwrap_or_default();
    let p = &cfg.proxy;
    // 只回「是否已设置」，明文永不回传（与 WebDAV 凭据同样的纪律）
    let user_set = mgr.decrypt_field(&p.username_enc).ok().flatten().is_some_and(|s| !s.is_empty());
    Json(serde_json::json!({
        "enabled": p.enabled,
        "url": p.url,
        "username_set": user_set,
        "no_proxy": p.no_proxy,
        "usable": p.usable(),
    }))
}

/// `POST /api/proxy`：保存代理设置
///
/// ## 凭据的「缺省不改」语义
/// `username` / `password` 为 `None` 时**保持原值** —— 前端回显只给
/// `username_set`，用户不改密码时不会传这两个字段。若把 `None` 当成「清空」，
/// 每次改 `no_proxy` 都会把密码抹掉。
pub(super) async fn proxy_save(
    State(state): State<AppState>,
    Json(body): Json<ProxySaveRequest>,
) -> Json<serde_json::Value> {
    let url = body.url.trim().to_string();
    // 基本校验：启用时必须给合法地址
    if body.enabled && url.is_empty() {
        return Json(serde_json::json!({
            "success": false, "error": Some("启用代理时必须填写代理地址"),
        }));
    }
    if !url.is_empty() {
        // 用严格校验而非 `reqwest::Proxy::all`：后者对非法串不报错（把整串当主机名），
        // 会让用户以为保存成功，直到备份时才失败。
        if let Err(e) = crate::infra::target::webdav::validate_proxy_url(&url) {
            return Json(serde_json::json!({ "success": false, "error": Some(e) }));
        }
    }

    let mgr = state.config.lock().unwrap();
    let mut cfg = mgr.load().unwrap_or_default();
    // 凭据：None = 保持原值
    let mut user_enc = cfg.proxy.username_enc.clone();
    let mut pass_enc = cfg.proxy.password_enc.clone();
    if let Some(u) = &body.username {
        user_enc = if u.trim().is_empty() {
            None
        } else {
            match mgr.encrypt_field(u.trim()) {
                Ok(v) => Some(v),
                Err(e) => {
                    return Json(serde_json::json!({
                        "success": false, "error": Some(format!("代理用户名加密失败：{e:#}")),
                    }))
                }
            }
        };
        // 清空用户名时一并清密码（否则留下一个孤立的密码密文）
        if u.trim().is_empty() {
            pass_enc = None;
        }
    }
    if let Some(p) = &body.password {
        if !p.is_empty() {
            match mgr.encrypt_field(p) {
                Ok(v) => pass_enc = Some(v),
                Err(e) => {
                    return Json(serde_json::json!({
                        "success": false, "error": Some(format!("代理密码加密失败：{e:#}")),
                    }))
                }
            }
        } else {
            pass_enc = None;
        }
    }
    cfg.proxy = crate::infra::config::ProxySettings {
        enabled: body.enabled,
        url,
        username_enc: user_enc,
        password_enc: pass_enc,
        no_proxy: body.no_proxy.trim().to_string(),
    };
    if let Err(e) = mgr.save(&cfg) {
        return Json(serde_json::json!({
            "success": false, "error": Some(format!("保存失败：{e:#}")),
        }));
    }
    drop(mgr);
    // 代理变更会影响所有目标的连接方式 ⇒ 重建目标池（下次备份即用新代理）
    state.reload_targets(&cfg);
    state.audit.record(
        "proxy.save",
        if cfg.proxy.usable() {
            format!("保存网络代理（{}）", cfg.proxy.url)
        } else {
            "保存网络代理（已关闭）".to_string()
        },
        true,
        None,
    );
    Json(serde_json::json!({ "success": true, "error": null }))
}

/// 判定「本次提交是否等于『不改凭据』」
///
/// 抽成纯函数是为了能直接测：这是「编辑目标时被要求重填密码」那个 bug 的核心判断，
/// 埋在 axum handler 里靠读代码保证正确性太脆弱。
///
/// 语义：
/// - 没提交用户名（`user == None`）⇒ 不改凭据；
/// - 提交了用户名且**与已保存的一致**，又没填密码 ⇒ 不改凭据
///   （前端回显用户名会带上来，用户实际只改了别的字段）；
/// - 其余情况都是「要改凭据」，必须带密码。
fn credential_is_unchanged(
    user: Option<&str>,
    pass: Option<&str>,
    same_as_saved: bool,
) -> bool {
    match user {
        None => true,
        Some(_) => same_as_saved && pass.map(|p| p.trim().is_empty()).unwrap_or(true),
    }
}

#[cfg(test)]
mod credential_tests {
    use super::credential_is_unchanged;

    /// **回归**：只改名称/地址等字段时，回显的用户名 + 空密码**不能**被判成「改凭据」
    ///
    /// 线上症状：编辑目标时没改用户名，却报「新填写用户名时必须同时提供密码」，
    /// 导致目标一旦配好凭据就再也改不了任何设置。
    #[test]
    fn unchanged_username_with_empty_password_is_not_a_credential_change() {
        // 前端回显了用户名，密码栏留空（表示不修改）→ 应视为不改凭据
        assert!(credential_is_unchanged(
            Some("kzwr-user"),
            Some(""),
            true
        ));
        // 密码字段完全没提交（老前端）→ 同样视为不改
        assert!(credential_is_unchanged(Some("kzwr-user"), None, true));
        // 完全没提交用户名 → 不改凭据
        assert!(credential_is_unchanged(None, None, false));
    }

    /// 真正的凭据变更必须要求密码（否则会把密码存成空串）
    #[test]
    fn real_credential_change_still_requires_password() {
        // 用户名改了，但没填密码 → 必须拦下
        assert!(!credential_is_unchanged(Some("new-user"), Some(""), false));
        assert!(!credential_is_unchanged(Some("new-user"), None, false));
        // 用户名没变但明确填了新密码 → 这是「只改密码」，算凭据变更
        assert!(!credential_is_unchanged(Some("kzwr-user"), Some("newpass"), true));
    }
}

/// `POST /api/proxy/test`：测试代理连通性
///
/// ## 为什么单独做一个接口
/// 用户填完代理地址后，最需要确认的是「这个代理能不能真的带我把请求发出去」。
/// 仅校验地址格式（`validate_proxy_url`）不能回答这一点 —— 地址可以完全合法
/// 但代理进程没在跑、或端口不对外。所以这里**真的经代理发一次请求**。
///
/// ## 测什么目标
/// 优先用请求体里传入的 `target_url`（用户在目标页填的地址）；
/// 没传则用「当前已配置的任一 WebDAV 目标地址」，再没有就用官方默认地址。
/// 这样测试结果与用户实际使用场景一致，而不是测一个无关的站点。
///
/// ## 故意不要求目标凭据
/// 只看「能否经代理到达并收到 HTTP 响应」：即便返回 401/403 也说明
/// **代理链路是通的**（请求确实出去了并拿到了响应）。这点必须在结果里说清楚，
/// 否则用户会误以为代理坏了。
pub(super) async fn proxy_test(
    State(state): State<AppState>,
    body: Option<Json<ProxyTestRequest>>,
) -> Json<serde_json::Value> {
    let (enabled, proxy_url, user, pass, no_proxy, target_url) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        let p = &cfg.proxy;
        // 请求体可覆盖（用户可能还没保存就想试）
        let body = body.map(|b| b.0).unwrap_or_default();
        let enabled = body.enabled.unwrap_or(p.enabled);
        let proxy_url = body.url.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| p.url.clone());
        // 凭据：请求体传了就用，否则解密已保存的
        let user = match &body.username {
            Some(u) => u.clone(),
            None => mgr.decrypt_field(&p.username_enc).ok().flatten().unwrap_or_default(),
        };
        let pass = match &body.password {
            Some(pw) => pw.clone(),
            None => mgr.decrypt_field(&p.password_enc).ok().flatten().unwrap_or_default(),
        };
        let no_proxy = body.no_proxy.clone().unwrap_or_else(|| p.no_proxy.clone());
        // 目标地址：请求体 > 已配置目标 > 官方默认
        let target_url = body
            .target_url
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                cfg.targets
                    .iter()
                    .filter(|t| t.enabled && t.kind == "webdav")
                    .filter_map(|t| t.url.clone())
                    .find(|u| !u.trim().is_empty())
            })
            .unwrap_or_else(|| crate::infra::target::webdav::DEFAULT_URL.to_string());
        (enabled, proxy_url, user, pass, no_proxy, target_url)
    };

    if proxy_url.trim().is_empty() {
        return Json(serde_json::json!({
            "success": false,
            "error": Some("请先填写代理地址".to_string()),
        }));
    }
    if let Err(e) = crate::infra::target::webdav::validate_proxy_url(&proxy_url) {
        return Json(serde_json::json!({ "success": false, "error": Some(e) }));
    }
    if !enabled {
        return Json(serde_json::json!({
            "success": false,
            "error": Some("代理功能未启用，请先勾选「启用网络代理」".to_string()),
        }));
    }
    // no_proxy 命中：如实告知「按当前设置不会走代理」，而不是假装测通
    if crate::infra::target::webdav::host_matches_no_proxy(&target_url, &no_proxy) {
        return Json(serde_json::json!({
            "success": false,
            "error": Some(format!(
                "目标地址 {target_url} 命中「不走代理的地址」列表，当前设置下不会使用代理"
            )),
        }));
    }

    // 组装代理并**真的发一次请求**
    let proxy = match reqwest::Proxy::all(proxy_url.trim()) {
        Ok(p) => {
            if user.is_empty() {
                p
            } else {
                p.basic_auth(&user, &pass)
            }
        }
        Err(e) => {
            return Json(serde_json::json!({
                "success": false,
                "error": Some(format!("代理配置无效：{e}")),
            }))
        }
    };
    let client = match reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        // 只测「能否经代理到达」：整体超时给足，但别让用户等太久
        .timeout(std::time::Duration::from_secs(20))
        .http1_only()
        .proxy(proxy)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Json(serde_json::json!({
                "success": false,
                "error": Some(format!("构建测试客户端失败：{e}")),
            }))
        }
    };

    let started = std::time::Instant::now();
    // 用 HEAD/GET 发到目标地址：只要拿到**任何** HTTP 响应就说明代理链路通了
    let resp = client.head(&target_url).send().await;
    let elapsed = started.elapsed().as_millis() as u64;
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            Json(serde_json::json!({
                "success": true,
                "elapsed_ms": elapsed,
                "status": status,
                "target_url": target_url,
                "message": format!(
                    "代理可用：经 {} 访问 {} 收到 HTTP {}（{} ms）",
                    proxy_url.trim(),
                    target_url,
                    status,
                    elapsed
                ),
                // 401/403 也代表「链路通了」，单独提示避免误解
                "note": if status == 401 || status == 403 {
                    Some("返回 401/403 属正常：说明请求确实经代理到达了服务端，只是没带凭据".to_string())
                } else {
                    None
                },
            }))
        }
        Err(e) => {
            // 给出可操作的排障建议，而不是只抛一句 reqwest 内部文案
            let hint = if e.is_connect() || e.is_timeout() {
                "代理可能未运行、端口不对，或该代理不允许本机访问"
            } else if e.is_request() {
                "代理拒绝了请求，请检查认证用户名/密码"
            } else {
                "请确认代理地址、协议（http/https/socks5/socks5h）与认证信息"
            };
            Json(serde_json::json!({
                "success": false,
                "elapsed_ms": elapsed,
                "target_url": target_url,
                "error": Some(format!("经代理访问失败：{e}")),
                "hint": Some(hint.to_string()),
            }))
        }
    }
}
