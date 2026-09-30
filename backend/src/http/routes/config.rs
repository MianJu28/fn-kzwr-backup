//! 配置读写（WebDAV 旧入口 / 设置页 / 用户信息 / 导出导入）
//!
//! 注意：配置导入导出**含明文凭据**（导出文件本身即敏感件），
//! 入口一律要求管理员口令，且**不得**写日志。

use std::sync::Arc;

use age::secrecy::ExposeSecret;
use axum::extract::State;
use axum::response::Json;

use crate::AppState;

use super::common::new_id;
use super::plugins::apply_plugin_switch;
use super::types::*;

/// 保存 WebDAV 配置：先实测连通性（PROPFIND ping），通过后加密存储
pub(super) async fn webdav_save(
    State(state): State<AppState>,
    Json(body): Json<WebdavSaveRequest>,
) -> Json<WebdavSaveResponse> {
    if body.username.trim().is_empty() || body.password.is_empty() {
        return Json(WebdavSaveResponse {
            success: false,
            url: None,
            error: Some("用户名、密码均不能为空".to_string()),
        });
    }

    // 1) 实测连通性与凭据：交给**目标插件**判定（地址与协议由插件决定）
    let Some(plugin) = state.plugins.target_plugin("webdav") else {
        return Json(WebdavSaveResponse {
            success: false,
            url: None,
            error: Some("未注册 webdav 目标插件".to_string()),
        });
    };
    let url = match plugin
        .verify(body.url.as_deref(), body.username.trim(), &body.password)
        .await
    {
        Ok(u) => u,
        Err(e) => {
            return Json(WebdavSaveResponse {
                success: false,
                url: None,
                    error: Some(e),
            })
        }
    };

    // 2) 加密保存（写入**主目标**；保存后同步兼容镜像）
    let saved = {
        let mgr = state.config.lock().unwrap();
        mgr.save_webdav(&url, body.username.trim(), &body.password)
    };
    match saved {
        Ok(_) => {
            // 热刷新目标池（无需重启服务）：由注册表按当前配置重新装配全部目标
            let cfg = { state.config.lock().unwrap().load().unwrap_or_default() };
            state.reload_targets(&cfg);
            state.audit.record(
                "webdav.credentials",
                format!("保存 WebDAV 凭据（账号 {}）", body.username.trim()),
                true,
                None,
            );
            Json(WebdavSaveResponse {
                success: true,
                url: Some(url),
                error: None,
            })
        }
        Err(e) => {
            state.audit.record(
                "webdav.credentials",
                format!("保存 WebDAV 凭据失败：{:#}", e),
                false,
                None,
            );
            Json(WebdavSaveResponse {
                success: false,
                url: Some(url),
                    error: Some(format!("{:#}", e)),
            })
        }
    }
}

/// 由配置构造响应（含 cron 校验）
pub(super) fn config_response(
    cfg: &crate::infra::config::AppConfig,
    webdav_configured: bool,
    webdav_username: Option<String>,

    error: Option<String>,
) -> ConfigResponse {
    // 兼容视图：以「首个任务」为默认任务（多任务管理走 `/api/tasks`）
    let task = cfg.tasks.first();
    let schedule = task
        .and_then(|t| t.schedule_cron.clone())
        .unwrap_or_default();
    let valid = crate::domain::scheduler::validate_cron(&schedule).is_ok();
    let schedule_next = crate::domain::scheduler::next_runs(&schedule, 5).unwrap_or_default();
    let retention = task
        .map(|t| &t.retention)
        .unwrap_or(&cfg.backup.retention);
    ConfigResponse {
        backup_paths: task.map(|t| t.paths.clone()).unwrap_or_default(),
        target_folder: task
            .map(|t| t.target_folder.clone())
            .unwrap_or_else(|| cfg.backup.target_folder.clone()),
        schedule_cron: schedule,
        schedule_cron_valid: valid,
        webdav_configured,
        webdav_url: cfg.primary_target().and_then(|t| t.url.clone()),
        webdav_username,
        retention: RetentionView::from(retention),
        schedule_next,
        schedule_timezone: crate::domain::scheduler::timezone_label(),
        host_utc_offset_minutes: crate::domain::scheduler::utc_offset_minutes(),
        webhook_url: cfg.notify.webhook_url.clone(),
        webhook_headers: cfg.notify.webhook_headers.clone(),
        webhook_body: cfg.notify.webhook_body.clone(),
        key_backed_up: cfg.keys.backed_up,
        debug: cfg.debug,
        plugins_enabled: cfg.plugins.enabled,
        plugins_dir: cfg.plugins.dir.clone().unwrap_or_default(),
        plugins_pubkeys: cfg.plugins.pubkeys.clone(),
        plugins_allow_unsigned: crate::plugin::loader::allow_unsigned_by_env(),
        plugins_disabled: cfg.plugins.disabled.clone(),
        plugins_plugin_pubkeys: cfg.plugins.plugin_pubkeys.clone(),
        market_enabled: cfg.market.enabled,
        market_auto_check: cfg.market.auto_check,
        market_max_artifact_mb: cfg.market.max_artifact_mb,
        error,
    }
}

/// 判断**主目标**是否已配置（启用 + 用户名 + 密码齐全）
pub(super) fn webdav_ready(cfg: &crate::infra::config::AppConfig) -> bool {
    cfg.primary_target()
        .map(|t| t.enabled && t.configured())
        .unwrap_or(false)
}

pub(super) async fn config_get(State(state): State<AppState>) -> Json<ConfigResponse> {
    // 注意：config_echo 内部会锁 config，故须在下面加锁前先取，避免同一 Mutex 重入死锁
    let wuser = config_echo(&state);
    let cfg_guard = state.config.lock().unwrap();
    match cfg_guard.load() {
        Ok(c) => Json(config_response(&c, webdav_ready(&c), wuser, None)),
        Err(e) => Json(ConfigResponse {
            backup_paths: Vec::new(),
            target_folder: state.target_folder.clone(),
            schedule_cron: String::new(),
            schedule_cron_valid: true,
            webdav_configured: false,
            webdav_url: None,
            webdav_username: None,
            retention: RetentionView::default(),
            schedule_next: Vec::new(),
            schedule_timezone: crate::domain::scheduler::timezone_label(),
            host_utc_offset_minutes: crate::domain::scheduler::utc_offset_minutes(),
            plugins_enabled: false,
            plugins_dir: String::new(),
            plugins_pubkeys: Vec::new(),
            plugins_allow_unsigned: crate::plugin::loader::allow_unsigned_by_env(),
            plugins_disabled: Vec::new(),
            plugins_plugin_pubkeys: std::collections::BTreeMap::new(),
            market_enabled: false,
            market_auto_check: false,
            market_max_artifact_mb: 32,
            webhook_url: None,
            webhook_headers: Vec::new(),
            webhook_body: None,
            key_backed_up: false,
            debug: false,
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 保存配置（备份路径、目标文件夹、定时 cron）
pub(super) async fn config_save(
    State(state): State<AppState>,
    Json(body): Json<ConfigSaveRequest>,
) -> Json<ConfigResponse> {
    // 同 config_get：先取回显字段再锁配置，避免 Mutex 重入死锁
    let wuser = config_echo(&state);
    let cfg_guard = state.config.lock().unwrap();
    let mut cfg = match cfg_guard.load() {
        Ok(c) => c,
        Err(_) => crate::infra::config::AppConfig::default(),
    };
    // 兼容接口：写「首个任务」（不存在则创建默认任务）；多任务走 `/api/tasks`
    if cfg.tasks.is_empty() {
        let mut t = crate::infra::config::TaskConfig::default();
        t.id = crate::infra::config::DEFAULT_TASK_ID.to_string();
        t.name = "默认任务".to_string();
        t.target_id = cfg
            .primary_target()
            .map(|x| x.id.clone())
            .unwrap_or_else(|| crate::infra::config::DEFAULT_TARGET_ID.to_string());
        t.target_folder = cfg.backup.target_folder.clone();
        cfg.tasks.push(t);
    }
    // 定时 cron：先校验（错误时需借用 cfg 生成响应，故在可变借用前完成）
    let cron_update: Option<Option<String>> = match body.schedule_cron {
        Some(cron) => {
            let cron = cron.trim().to_string();
            if let Err(e) = crate::domain::scheduler::validate_cron(&cron) {
                let resp =
                    config_response(&cfg, webdav_ready(&cfg), wuser, Some(e.to_string()));
                return Json(resp);
            }
            Some(if cron.is_empty() { None } else { Some(cron) })
        }
        None => None,
    };

    {
        let task = cfg.tasks.first_mut().expect("刚保证非空");
        if let Some(paths) = body.backup_paths.clone() {
            task.paths = paths;
        }
        if let Some(folder) = body.target_folder {
            if !folder.trim().is_empty() {
                task.target_folder = folder;
            }
        }
        // 保留策略：仅更新传入的字段（未传保持原值）
        if let Some(v) = body.retention_enabled {
            task.retention.enabled = v;
        }
        if let Some(v) = body.retention_cleanup_unmanaged {
            task.retention.cleanup_unmanaged = v;
        }
        if let Some(v) = body.retention_min_age_days {
            task.retention.min_age_days = v;
        }
        if let Some(v) = body.retention_empty_recycle_bin {
            task.retention.empty_recycle_bin = v;
        }
        if let Some(v) = body.retention_recycle_max_gb {
            task.retention.recycle_max_gb = v;
        }
        if let Some(v) = body.retention_recycle_min_age_days {
            task.retention.recycle_min_age_days = v;
        }
        if let Some(cron) = cron_update {
            task.schedule_cron = cron;
        }
    }
    // 调试日志开关：保存并即时生效（日志过滤器热更新）
    if let Some(v) = body.debug {
        cfg.debug = v;
    }
    // 外置插件开关：落盘**并热生效**（不再要求重启 —— 见 `apply_plugin_switch`）
    if let Some(v) = body.plugins_enabled {
        cfg.plugins.enabled = v;
    }
    if let Some(v) = body.plugins_dir {
        let v = v.trim().to_string();
        cfg.plugins.dir = if v.is_empty() { None } else { Some(v) };
    }
    // 插件签名公钥：**先校验格式**再落盘。若存入非法公钥，加载器会认为「已配置」
    // 而逐个验签失败，等于把插件全锁死；这里提前拦下并给出明确原因。
    if let Some(v) = body.plugins_pubkeys {
        use base64::Engine as _;
        let mut cleaned: Vec<String> = Vec::new();
        let mut invalid: Option<String> = None;
        for (i, k) in v.iter().enumerate() {
            let k = k.trim();
            if k.is_empty() {
                continue; // 前端可能留空行，忽略
            }
            match base64::engine::general_purpose::STANDARD.decode(k) {
                Ok(raw) if raw.len() == 32 => cleaned.push(k.to_string()),
                Ok(raw) => {
                    invalid = Some(format!(
                        "第 {} 个插件公钥长度不对：Ed25519 公钥应为 32 字节，实际 {} 字节",
                        i + 1,
                        raw.len()
                    ));
                    break;
                }
                Err(_) => {
                    invalid = Some(format!("第 {} 个插件公钥不是合法 base64", i + 1));
                    break;
                }
            }
        }
        if let Some(e) = invalid {
            let resp = config_response(&cfg, webdav_ready(&cfg), wuser, Some(e));
            return Json(resp);
        }
        cleaned.dedup();
        cfg.plugins.pubkeys = cleaned;
    }
    // 插件市场：落盘即生效。**打开时只允许一次显式动作** —— 不在这里自动联网，
    // 用户点「刷新」或进市场页时才拉索引（"不在用户未要求时联网"）。
    if let Some(v) = body.market_enabled {
        cfg.market.enabled = v;
    }
    if let Some(v) = body.market_auto_check {
        cfg.market.auto_check = v;
    }
    if let Some(v) = body.market_max_artifact_mb {
        // 下限 1MB、上限 512MB：过小会把正常插件挡掉，过大失去防打爆的意义
        cfg.market.max_artifact_mb = v.clamp(1, 512);
    }
    match cfg_guard.save(&cfg) {
        Ok(_) => {
            let t = cfg.tasks.first().cloned().unwrap_or_default();
            state.audit.record(
                "config.save",
                format!(
                    "保存配置（任务 {}）：目标目录 {}，路径 {} 个，定时 {}，保留策略[启用={} 孤儿={} 天数={} 回收站={} ≥{}GB >{}天]",
                    t.name,
                    t.target_folder,
                    t.paths.len(),
                    t.schedule_cron
                        .clone()
                        .unwrap_or_else(|| "未启用".to_string()),
                    t.retention.enabled,
                    t.retention.cleanup_unmanaged,
                    t.retention.min_age_days,
                    t.retention.empty_recycle_bin,
                    t.retention.recycle_max_gb,
                    t.retention.recycle_min_age_days,
                ),
                true,
                None,
            );
            // 调试日志开关热更新（保存成功后立即切换日志级别）
            crate::apply_log_debug(cfg.debug);
            drop(cfg_guard);
            // 外置插件开关：**热生效**（装载/卸载外置插件，无需重启应用）
            apply_plugin_switch(&state, &cfg);
            state.reload_targets(&cfg);
            Json(config_response(&cfg, webdav_ready(&cfg), wuser, None))
        }
        Err(e) => Json(config_response(&cfg, false, wuser, Some(format!("{:#}", e)))),
    }
}

/// 当前 WebDAV 账号：取**主目标**的解密用户名
pub(super) fn webdav_username(state: &AppState) -> Option<String> {
    let mgr = state.config.lock().unwrap();
    mgr.webdav_credentials().ok().and_then(|(u, _)| u)
}

/// 设置页回显所需的非敏感信息（内部会锁 config，**必须在加锁前调用**）
///
/// 返回 WebDAV 用户名（凭据本身永不返回）。
pub(super) fn config_echo(state: &AppState) -> Option<String> {
    let mgr = state.config.lock().unwrap();
    mgr.webdav_credentials().ok().and_then(|(u, _)| u)
}

/// 获取当前 WebDAV 账号信息
pub(super) async fn user_info(State(state): State<AppState>) -> Json<UserInfoResponse> {
    if let Some(u) = webdav_username(&state) {
        return Json(UserInfoResponse {
            username: Some(u),
            error: None,
        });
    }
    let configured = {
        let mgr = state.config.lock().unwrap();
        mgr.load().map(|c| webdav_ready(&c)).unwrap_or(false)
    };
    Json(UserInfoResponse {
        username: configured.then(|| "已配置".to_string()),
        error: (!configured).then(|| "WebDAV 未配置".to_string()),
    })
}

/// 导出配置（含 WebDAV 凭据明文与 age 私钥；需管理员口令校验）
pub(super) async fn config_export(
    State(state): State<AppState>,
    Json(body): Json<ConfigExportRequest>,
) -> Json<ConfigExportResponse> {
    if body.passphrase.as_str() != state.passphrase.expose_secret().as_str() {
        return Json(ConfigExportResponse {
            success: false,
            config: None,
            error: Some("管理员口令错误".to_string()),
        });
    }
    let (cfg, creds, bundle_targets) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        let creds = mgr.webdav_credentials().unwrap_or((None, None));
        // 全部目标的明文凭据（导出文件本身即敏感件，此函数入口已校验管理员口令）
        let bundle_targets: Vec<BundleTarget> = cfg
            .targets
            .iter()
            .map(|t| {
                let (u, p) = mgr.target_credentials(t).unwrap_or((None, None));
                BundleTarget {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    kind: t.kind.clone(),
                    url: t.url.clone(),
                    username: u,
                    password: p,
                    enabled: t.enabled,
                    parallel: t.parallel,
                }
            })
            .collect();
        (cfg, creds, bundle_targets)
    };
    let ks_path = crate::infra::keystore::keystore_path(&state.cfg_dir);
    let age_private_key = crate::infra::keystore::load_keystore(&state.passphrase, &ks_path)
        .ok()
        .map(|k| k.to_secret_key());
    let exported_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let bundle = ConfigBundle {
        version: 1,
        exported_at,
        backup_paths: cfg.backup.paths.clone(),
        target_folder: cfg.backup.target_folder.clone(),
        schedule_cron: cfg.backup.schedule_cron.clone(),
        retention: Some(cfg.backup.retention.clone()),
        webhook_url: cfg.notify.webhook_url.clone(),
        webhook_headers: cfg.notify.webhook_headers.clone(),
        webhook_body: cfg.notify.webhook_body.clone(),
        webdav_url: cfg.webdav.url.clone(),
        webdav_username: creds.0,
        webdav_password: creds.1,

        age_private_key,
        key_backed_up: cfg.keys.backed_up,
        targets: bundle_targets,
        tasks: cfg.tasks.clone(),
        // 插件配置由插件自管（`own_data_dir`），**不随导出包携带**（ADR-021），
        // 因此 bundle 里根本没有该字段；换机时由用户在插件页重新填写。
    };
    match serde_json::to_string_pretty(&bundle) {
        Ok(text) => Json(ConfigExportResponse {
            success: true,
            config: Some(text),
            error: None,
        }),
        Err(e) => Json(ConfigExportResponse {
            success: false,
            config: None,
            error: Some(format!("序列化配置失败: {e}")),
        }),
    }
}

/// 导入配置（覆盖备份/通知/WebDAV 凭据，可选恢复 age 私钥；需管理员口令校验）
pub(super) async fn config_import(
    State(state): State<AppState>,
    Json(body): Json<ConfigImportRequest>,
) -> Json<ConfigImportResponse> {
    if body.passphrase.as_str() != state.passphrase.expose_secret().as_str() {
        return Json(ConfigImportResponse {
            success: false,
            error: Some("管理员口令错误".to_string()),
        });
    }
    let bundle: ConfigBundle = match serde_json::from_str(&body.config) {
        Ok(b) => b,
        Err(e) => {
            return Json(ConfigImportResponse {
                success: false,
                error: Some(format!("配置解析失败: {e}")),
            })
        }
    };

    // 1) 更新配置（保留未提供的字段；WebDAV 凭据用当前口令重新加密）
    {
        let mgr = state.config.lock().unwrap();
        let mut cfg = mgr.load().unwrap_or_default();
        cfg.backup.paths = bundle.backup_paths.clone();
        if !bundle.target_folder.trim().is_empty() {
            cfg.backup.target_folder = bundle.target_folder.clone();
        }
        cfg.backup.schedule_cron = bundle
            .schedule_cron
            .clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(r) = bundle.retention.clone() {
            cfg.backup.retention = r;
        }
        cfg.notify.webhook_url = bundle
            .webhook_url
            .clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        cfg.notify.webhook_headers = bundle.webhook_headers.clone();
        cfg.notify.webhook_body = bundle
            .webhook_body
            .clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        cfg.keys.backed_up = bundle.key_backed_up;
        if let (Some(u), Some(p)) = (
            bundle.webdav_username.as_ref().filter(|s| !s.is_empty()),
            bundle.webdav_password.as_ref().filter(|s| !s.is_empty()),
        ) {
            match (mgr.encrypt_field(u), mgr.encrypt_field(p)) {
                (Ok(eu), Ok(ep)) => {
                    cfg.webdav.username_enc = Some(eu);
                    cfg.webdav.password_enc = Some(ep);
                }
                _ => {
                    return Json(ConfigImportResponse {
                        success: false,
                        error: Some("WebDAV 凭据加密失败".to_string()),
                    })
                }
            }
        }
        if let Some(url) = bundle
            .webdav_url
            .as_ref()
            .filter(|s| !s.trim().is_empty())
        {
            cfg.webdav.url = Some(url.trim_end_matches('/').to_string());
        }
        // 多目标（凭据用当前口令重新加密）
        if !bundle.targets.is_empty() {
            let mut targets = Vec::new();
            for t in &bundle.targets {
                let enc = |v: &Option<String>| -> Result<Option<String>, String> {
                    match v.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                        Some(s) => mgr
                            .encrypt_field(&s)
                            .map(Some)
                            .map_err(|_| "目标凭据加密失败".to_string()),
                        None => Ok(None),
                    }
                };
                let username_enc = match enc(&t.username) {
                    Ok(v) => v,
                    Err(e) => return Json(ConfigImportResponse { success: false, error: Some(e) }),
                };
                let password_enc = match enc(&t.password) {
                    Ok(v) => v,
                    Err(e) => return Json(ConfigImportResponse { success: false, error: Some(e) }),
                };
                targets.push(crate::infra::config::TargetConfig {
                    id: if t.id.is_empty() { new_id("t") } else { t.id.clone() },
                    name: t.name.clone(),
                    kind: if t.kind.is_empty() {
                        "webdav".to_string()
                    } else {
                        t.kind.clone()
                    },
                    url: t.url.clone(),
                    username_enc,
                    password_enc,
                    enabled: t.enabled,
                    parallel: t.parallel,
                    fields: std::collections::BTreeMap::new(),
                });
            }
            cfg.targets = targets;
        }
        // 多任务（target_id 失效时回落到首个目标，避免导入后任务不可运行）
        if !bundle.tasks.is_empty() {
            let ids: Vec<String> = cfg.targets.iter().map(|t| t.id.clone()).collect();
            let fallback = ids
                .first()
                .cloned()
                .unwrap_or_else(|| crate::infra::config::DEFAULT_TARGET_ID.to_string());
            cfg.tasks = bundle
                .tasks
                .iter()
                .map(|t| {
                    let mut t = t.clone();
                    if !ids.contains(&t.target_id) {
                        t.target_id = fallback.clone();
                    }
                    t
                })
                .collect();
        }
        // 导入包若带着老版本的 `plugin_data` 键：serde 会直接忽略（字段已删）。
        // 宿主不再代存插件配置 —— 插件把配置写进自己的 `own_data_dir`。
        if let Err(e) = mgr.save(&cfg) {
            return Json(ConfigImportResponse {
                success: false,
                error: Some(format!("保存配置失败: {:#}", e)),
            });
        }
    }
    for p in state.plugins.enhance_plugins() {
        p.reload(&state).await;
    }

    // 1.6) 目标池重建（导入可能改了目标/任务）
    {
        let cfg = { state.config.lock().unwrap().load().unwrap_or_default() };
        state.reload_targets(&cfg);
    }

    // 2) 可选：恢复 age 私钥（热切换，无需重启）
    if let Some(pk) = bundle
        .age_private_key
        .as_ref()
        .filter(|s| !s.trim().is_empty())
    {
        match crate::domain::crypto::AgeKeys::from_secret_key(pk.trim()) {
            Ok(keys) => {
                let ks_path = crate::infra::keystore::keystore_path(&state.cfg_dir);
                if let Err(e) =
                    crate::infra::keystore::save_keystore(&keys, &state.passphrase, &ks_path)
                {
                    return Json(ConfigImportResponse {
                        success: false,
                        error: Some(format!("保存密钥库失败: {:#}", e)),
                    });
                }
                state
                    .crypto
                    .swap(crate::domain::crypto::CryptoSession::full(&keys));
                tracing::info!("已从导入配置恢复 age 密钥");
            }
            Err(e) => {
                return Json(ConfigImportResponse {
                    success: false,
                    error: Some(format!("私钥无效: {:#}", e)),
                })
            }
        }
    }

    // 3) 热切换 WebDAV 目标
    if let (Some(u), Some(p)) = (
        bundle.webdav_username.clone().filter(|s| !s.is_empty()),
        bundle.webdav_password.clone().filter(|s| !s.is_empty()),
    ) {
        let url = bundle
            .webdav_url
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| crate::infra::target::webdav::DEFAULT_URL.to_string());
        state
            .target
            .swap(Arc::new(crate::infra::target::webdav::WebdavTarget::new(
                &url, u, p,
            )));
        tracing::info!("已从导入配置切换 WebDAV 目标");
    }

    state.audit.record(
        "config.import",
        "导入配置包（备份路径/目标/定时/通知/WebDAV 凭据/插件自管数据）",
        true,
        None,
    );
    Json(ConfigImportResponse {
        success: true,
        error: None,
    })
}
