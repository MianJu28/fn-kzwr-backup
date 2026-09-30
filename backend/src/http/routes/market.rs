//! 插件市场端点（浏览 / 安装 / 检查更新 / 刷新索引）
//!
//! ## 信任边界（改动前必读）
//!
//! 本模块**不建立信任**，只是"发现与运输"：
//! 信任判定由 `plugin::market` 的验签流程 + 加载期的 `loader::verify_plugin` 完成。
//! 这里唯一要守住的是**顺序**：先比 sha256、再验签（见 `market::download_artifact`）。
//!
//! ## 关闭时的行为
//!
//! `market.enabled = false` 时：本模块**不发起任何网络请求**，列表直接返回
//! 空 + `enabled: false`。这是"不在用户未要求时联网"的硬要求。

use axum::extract::State;
use axum::response::Json;

use crate::plugin::market;
use crate::AppState;

use super::common::err;
use super::types::{MarketCatalogQuery, MarketInstallRequest};

/// `GET /api/market/catalog`：插件列表（含宿主侧的兼容性判定）
///
/// `?refresh=1` 强制重新拉取索引；否则先用缓存（缓存每次读取都重新验签）。
pub(super) async fn market_catalog(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<MarketCatalogQuery>,
) -> Json<serde_json::Value> {
    let (enabled, max_mb) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        (cfg.market.enabled, cfg.market.max_artifact_mb)
    };
    if !enabled {
        // 关闭时**不联网**：只告知前端折叠市场页
        return Json(serde_json::json!({
            "enabled": false,
            "plugins": [],
            "catalog_version": null,
            "generated_at": null,
            "fetched_at": null,
            "source": null,
            "error": null,
        }));
    }
    let _ = max_mb; // 列表阶段不需要，安装时才用

    let refresh = q.refresh == "1" || q.refresh == "true";

    // 取索引：优先缓存（总是重新验签）；refresh 或缓存缺失时才联网
    let got = if refresh {
        state.market.refresh(&state.var_dir).await.ok()
    } else {
        match state.market.cached(&state.var_dir) {
            Some(c) => Some(c),
            None => state.market.refresh(&state.var_dir).await.ok(),
        }
    };

    let Some((catalog, source)) = got else {
        return Json(serde_json::json!({
            "enabled": true,
            "plugins": [],
            "catalog_version": null,
            "generated_at": null,
            "fetched_at": null,
            "source": null,
            "error": "索引不可用：所有来源均未能通过验签或无法访问",
        }));
    };

    // 已装版本：`plugin_pubkeys` 的键是文件名，插件 id 从注册表取
    let installed = installed_versions(&state);
    let items = market::build_items(&catalog, &installed);

    Json(serde_json::json!({
        "enabled": true,
        "plugins": items,
        "catalog_version": catalog.catalog_version,
        "generated_at": catalog.generated_at,
        "fetched_at": chrono::Utc::now().to_rfc3339(),
        "source": source,
        "error": null,
    }))
}

/// `POST /api/market/install`：从市场安装一个插件
///
/// 流程（顺序不可调换）：
/// 1. 取**已验签**的索引；2. 再算一次兼容性；3. 同名文件公钥冲突检查；
/// 4. 下载；5. **先比 sha256**；6. **再验签**；7. 原子落盘 + 绑定公钥；
/// 8. 热加载；9. 审计。
pub(super) async fn market_install(
    State(state): State<AppState>,
    body: Option<axum::extract::Json<MarketInstallRequest>>,
) -> Json<serde_json::Value> {
    let cfg = {
        let mgr = state.config.lock().unwrap();
        mgr.load().unwrap_or_default()
    };
    if !cfg.market.enabled {
        return Json(err("市场未启用：请先在设置中开启"));
    }

    let req = match body {
        Some(axum::extract::Json(b)) if !b.id.trim().is_empty() => b,
        _ => return Json(err("缺少插件 id")),
    };

    // 1) 已验签的索引（缓存也会重新验签；没有就现拉一次）
    let catalog = match state.market.cached(&state.var_dir) {
        Some((c, _)) => c,
        None => match state.market.refresh(&state.var_dir).await {
            Ok((c, _)) => c,
            Err(e) => return Json(err(format!("索引不可用：{e}"))),
        },
    };

    // 2) 找到插件与目标版本
    let Some(plugin) = catalog.plugins.iter().find(|p| p.id == req.id) else {
        return Json(err(format!("索引中没有插件 {}", req.id)));
    };
    let installed = installed_versions(&state);
    let target = if req.version.trim().is_empty() {
        // 未指定版本：取最新的**兼容**版本
        market::build_items(&catalog, &installed)
            .into_iter()
            .find(|i| i.plugin.id == req.id)
            .and_then(|i| i.latest)
    } else {
        plugin
            .versions
            .iter()
            .find(|v| v.version == req.version.trim())
            .cloned()
    };
    let Some(v) = target else {
        return Json(err(format!("未找到 {} 的可安装版本", req.id)));
    };

    // 3) 再算一次兼容性（不信任前端传的判断）
    if !market::compatibility(&v, installed.get(&req.id).map(String::as_str)).is_ok() {
        let b = market::compatibility(&v, installed.get(&req.id).map(String::as_str));
        return Json(err(format!("该版本无法安装：{}", b.reason())));
    }
    if market::is_revoked(&catalog, &v) {
        return Json(err("该版本已被撤销，拒绝安装"));
    }

    // 4) 同名文件若已绑定**其它**公钥 ⇒ 拒绝（防覆盖已信任的其它来源文件）
    let dir = match market::install_dir() {
        Ok(d) => d,
        Err(e) => return Json(err(e)),
    };
    let existing_key = cfg.plugins.plugin_pubkeys.get(&v.file_name).cloned();
    if let Some(k) = existing_key {
        // 市场插件由官方密钥签名：已存在的绑定若不是官方公钥，说明该文件
        // 来自别的渠道（用户自签），不允许被市场条目覆盖。
        let is_official = crate::plugin::loader::OFFICIAL_PUBKEYS.contains(&k.as_str());
        if !is_official {
            return Json(err(format!(
                "文件 {} 已绑定非官方公钥，拒绝覆盖（请先手动卸载该插件）",
                v.file_name
            )));
        }
    }

    // 5+6) 下载 → 先比 sha256 → 再验签
    let (so, sig) = match market::download_artifact(&v, cfg.market.max_artifact_mb).await {
        Ok(x) => x,
        Err(e) => return Json(err(format!("下载或校验失败：{e}"))),
    };

    // 7) 原子落盘 + 绑定公钥（与手动安装同一条路径的语义）
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Json(err(format!("创建插件目录失败：{e}")));
    }
    let so_path = dir.join(&v.file_name);
    let sig_path = market::installed_sig_path(&dir, &v.file_name);
    if let Err(e) = atomic_write(&so_path, &so).and_then(|_| atomic_write(&sig_path, &sig)) {
        return Json(err(e));
    }

    // 绑定官方公钥（一插件一公钥：键为文件名）
    {
        let mgr = state.config.lock().unwrap();
        let mut cfg2 = mgr.load().unwrap_or_default();
        let official = crate::plugin::loader::OFFICIAL_PUBKEYS
            .first()
            .copied()
            .unwrap_or_default()
            .to_string();
        cfg2.plugins
            .plugin_pubkeys
            .insert(v.file_name.clone(), official);
        if let Err(e) = mgr.save(&cfg2) {
            return Json(err(format!("保存公钥配置失败：{e:#}")));
        }
    }

    // 8) 热加载（与手动安装/热重载同一入口）
    let cfg2 = {
        let mgr = state.config.lock().unwrap();
        mgr.load().unwrap_or_default()
    };
    super::plugins::apply_plugin_switch(&state, &cfg2);
    let ready = state.reload_targets(&cfg2);
    let loaded = state
        .plugins
        .external_reports()
        .iter()
        .any(|r| r.file == v.file_name && r.loaded);

    // 9) 审计（含 source_commit，便于事后溯源到确切源码）
    state.audit.record(
        "plugin.market_install",
        format!(
            "从市场安装插件 {} {}（文件 {}，来源 commit {}，加载：{}）",
            req.id, v.version, v.file_name, v.source_commit, loaded
        ),
        loaded,
        None,
    );

    Json(serde_json::json!({
        "success": true,
        "id": req.id,
        "version": v.version,
        "file_name": v.file_name,
        "path": so_path.to_string_lossy(),
        "loaded": loaded,
        "ready_targets": ready,
        "source_commit": v.source_commit,
        "note": if loaded {
            "已安装并立即加载".to_string()
        } else {
            "已安装，但本次加载失败（详见插件页诊断）".to_string()
        },
        "error": null,
    }))
}

/// `POST /api/market/check-updates`：检查已装插件是否有新版本
pub(super) async fn market_check_updates(State(state): State<AppState>) -> Json<serde_json::Value> {
    let enabled = {
        let mgr = state.config.lock().unwrap();
        mgr.load().unwrap_or_default().market.enabled
    };
    if !enabled {
        return Json(serde_json::json!({ "success": true, "updates": [], "error": null }));
    }
    let catalog = match state.market.cached(&state.var_dir) {
        Some((c, _)) => c,
        None => match state.market.refresh(&state.var_dir).await {
            Ok((c, _)) => c,
            Err(e) => return Json(err(format!("索引不可用：{e}"))),
        },
    };
    let installed = installed_versions(&state);
    let mut updates = Vec::new();
    for (id, cur) in &installed {
        let Some(p) = catalog.plugins.iter().find(|p| p.id == *id) else {
            continue;
        };
        for v in &p.versions {
            if v.arch != market::current_arch() || v.yanked {
                continue;
            }
            if market::compare_version(&v.version, cur) > 0 {
                updates.push(serde_json::json!({
                    "id": id, "installed": cur, "latest": v.version, "yanked": false,
                }));
            }
        }
    }
    Json(serde_json::json!({ "success": true, "updates": updates, "error": null }))
}

/// `POST /api/market/refresh`：强制刷新索引
pub(super) async fn market_refresh(State(state): State<AppState>) -> Json<serde_json::Value> {
    let enabled = {
        let mgr = state.config.lock().unwrap();
        mgr.load().unwrap_or_default().market.enabled
    };
    if !enabled {
        return Json(err("市场未启用"));
    }
    match state.market.refresh(&state.var_dir).await {
        Ok((c, source)) => Json(serde_json::json!({
            "success": true,
            "catalog_version": c.catalog_version,
            "generated_at": c.generated_at,
            "source": source,
            "error": null,
        })),
        Err(e) => Json(err(e)),
    }
}

// ── 辅助 ──────────────────────────────────────────────────────────────────

/// 已装插件的 `id → version`（来自注册表，不依赖文件名）
fn installed_versions(state: &AppState) -> std::collections::HashMap<String, String> {
    let cfg = {
        let mgr = state.config.lock().unwrap();
        mgr.load().unwrap_or_default()
    };
    state
        .plugins
        .describe(&cfg)
        .into_iter()
        .map(|e| (e.meta.id, e.meta.version))
        .collect()
}

/// 原子写：先写同目录临时文件再 rename，避免半截文件被当成有效产物
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp-market");
    std::fs::write(&tmp, bytes).map_err(|e| format!("写入 {} 失败：{e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("替换 {} 失败：{e}", path.display()))?;
    Ok(())
}
