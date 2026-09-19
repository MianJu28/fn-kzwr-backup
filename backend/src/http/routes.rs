//! REST 路由定义（axum）
//!
//! 目标存储为 kzwr 官方 WebDAV（ADR-009）。凭据经 /webdav/config 配置并加密存储。

use std::path::PathBuf;
use std::sync::Arc;

use age::secrecy::ExposeSecret;
use axum::extract::State;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};

use crate::domain::backup::BackupJob;
use crate::domain::restore::RestoreJob;
use crate::http::ws;
use crate::infra::storage_trait::TargetStorage;
use crate::AppState;

// ── 响应/请求结构 ──────────────────────────────

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

/// 用户信息响应（WebDAV 模式：仅本地配置的账号，无远端套餐/容量）
#[derive(Serialize, Default)]
pub struct UserInfoResponse {
    pub username: Option<String>,
    pub error: Option<String>,
}

/// WebDAV 配置保存请求
///
/// 地址固定为官方 WebDAV（`DEFAULT_URL`），不暴露给用户设置；url 字段可选（忽略）。
#[derive(Deserialize)]
pub struct WebdavSaveRequest {
    #[serde(default)]
    pub url: Option<String>,
    pub username: String,
    pub password: String,
}

/// WebDAV 配置保存响应（保存前先实测连通性）
#[derive(Serialize)]
pub struct WebdavSaveResponse {
    pub success: bool,
    pub url: Option<String>,
    pub error: Option<String>,
}

/// 配置响应（不回传敏感字段明文）
#[derive(Serialize)]
pub struct ConfigResponse {
    pub backup_paths: Vec<String>,
    pub target_folder: String,
    /// 定时备份 cron 表达式（空 = 未启用）
    pub schedule_cron: String,
    /// cron 表达式是否合法（供前端提示）
    pub schedule_cron_valid: bool,
    /// WebDAV 是否已配置
    pub webdav_configured: bool,
    /// 已配置的 WebDAV 地址（非敏感）
    pub webdav_url: Option<String>,
    /// 告警 Webhook 地址（空 = 不外发）
    pub webhook_url: Option<String>,
    /// Webhook 自定义请求头
    pub webhook_headers: Vec<crate::infra::config::WebhookHeader>,
    /// Webhook 自定义请求体模板（空 = 默认 JSON）
    pub webhook_body: Option<String>,
    /// 用户是否已确认备份 age 私钥（未确认时 UI 提示丢失风险）
    pub key_backed_up: bool,
    pub error: Option<String>,
}

/// 配置保存请求
#[derive(Deserialize)]
pub struct ConfigSaveRequest {
    pub backup_paths: Vec<String>,
    pub target_folder: Option<String>,
    /// 定时备份 cron 表达式（空 = 关闭定时）
    pub schedule_cron: Option<String>,
}

/// 备份响应
#[derive(Serialize)]
pub struct BackupResponse {
    pub uploaded: usize,
    pub uploaded_bytes: u64,
    pub deleted: usize,
    pub unchanged: usize,
    /// 保留策略清理的孤儿文件数
    pub orphan_removed: usize,
    pub error: Option<String>,
}

/// 恢复请求体
#[derive(Deserialize)]
pub struct RestoreRequest {
    /// 要恢复的文件相对路径列表；空 = 全量
    pub files: Option<Vec<String>>,
    /// 恢复目标根目录（未传则用配置的备份源路径，恢复到原位置）
    pub source_path: Option<String>,
}

/// 恢复响应
#[derive(Serialize)]
pub struct RestoreResponse {
    pub restored: usize,
    pub restored_bytes: u64,
    pub error: Option<String>,
}

/// 可恢复文件条目
#[derive(Serialize)]
pub struct RestorableFile {
    pub rel_path: String,
    pub size: u64,
    pub is_dir: bool,
}

/// 一个备份文件夹及其可恢复文件
#[derive(Serialize)]
pub struct RestorableFolder {
    /// 备份源路径（本地目录）
    pub path: String,
    /// 是否已有备份数据（SQLite 快照）
    pub has_backup: bool,
    /// 文件列表
    pub files: Vec<RestorableFile>,
}

/// 恢复文件列表响应
#[derive(Serialize)]
pub struct RestoreFilesResponse {
    pub folders: Vec<RestorableFolder>,
    pub error: Option<String>,
}

/// 密钥信息响应（永不回传私钥明文）
#[derive(Serialize)]
pub struct KeysInfoResponse {
    /// age 公钥（"age1..."）
    pub public_key: String,
    /// 首次启动自动生成时的私钥（仅此一次返回，提醒用户保存；之后恒为 None）
    pub private_key_once: Option<String>,
    pub error: Option<String>,
}

/// 设置自定义私钥请求
#[derive(Deserialize)]
pub struct KeysSetRequest {
    /// age 私钥（"AGE-SECRET-KEY-1..."）
    pub private_key: String,
}

/// 密钥变更响应（generate 成功时 private_key 仅此一次返回）
#[derive(Serialize)]
pub struct KeysChangeResponse {
    pub success: bool,
    pub public_key: Option<String>,
    pub private_key: Option<String>,
    pub error: Option<String>,
}

/// 告警列表响应（监控告警）
#[derive(Serialize)]
pub struct AlertsResponse {
    pub alerts: Vec<crate::domain::alerts::Alert>,
    pub error: Option<String>,
}

/// Webhook 配置请求
#[derive(Deserialize)]
pub struct WebhookSaveRequest {
    /// 告警 Webhook 地址（空 = 关闭外发）
    pub webhook_url: String,
    /// 自定义请求头
    #[serde(default)]
    pub headers: Vec<crate::infra::config::WebhookHeader>,
    /// 自定义请求体模板（空 = 默认 JSON）
    #[serde(default)]
    pub body_template: Option<String>,
}

/// Webhook 配置响应
#[derive(Serialize)]
pub struct WebhookSaveResponse {
    pub success: bool,
    pub webhook_url: Option<String>,
    pub headers: Vec<crate::infra::config::WebhookHeader>,
    pub body_template: Option<String>,
    pub error: Option<String>,
}

/// 私钥导出请求（需管理员口令校验）
#[derive(Deserialize)]
pub struct KeysExportRequest {
    /// 管理员口令（安装向导设置的应用口令）
    #[serde(default)]
    pub passphrase: String,
}

/// Webhook 连通性测试请求（可直接用表单中的当前值测试，无需先保存）
#[derive(Deserialize)]
pub struct WebhookTestRequest {
    #[serde(default)]
    pub webhook_url: String,
    #[serde(default)]
    pub headers: Vec<crate::infra::config::WebhookHeader>,
    #[serde(default)]
    pub body_template: Option<String>,
}

/// Webhook 测试响应
#[derive(Serialize)]
pub struct WebhookTestResponse {
    pub success: bool,
    pub status: Option<u16>,
    pub error: Option<String>,
}

/// 配置导出/导入包（含敏感项，导出需管理员口令）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigBundle {
    pub version: u32,
    #[serde(default)]
    pub exported_at: i64,
    #[serde(default)]
    pub backup_paths: Vec<String>,
    #[serde(default)]
    pub target_folder: String,
    #[serde(default)]
    pub schedule_cron: Option<String>,
    #[serde(default)]
    pub retention: Option<crate::infra::config::RetentionConfig>,
    #[serde(default)]
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub webhook_headers: Vec<crate::infra::config::WebhookHeader>,
    #[serde(default)]
    pub webhook_body: Option<String>,
    #[serde(default)]
    pub webdav_url: Option<String>,
    #[serde(default)]
    pub webdav_username: Option<String>,
    #[serde(default)]
    pub webdav_password: Option<String>,
    /// age 私钥（恢复配置后可继续解密既有备份）
    #[serde(default)]
    pub age_private_key: Option<String>,
    #[serde(default)]
    pub key_backed_up: bool,
}

/// 配置导出请求（需管理员口令）
#[derive(Deserialize)]
pub struct ConfigExportRequest {
    #[serde(default)]
    pub passphrase: String,
}

/// 配置导出响应
#[derive(Serialize)]
pub struct ConfigExportResponse {
    pub success: bool,
    /// 配置 JSON 文本
    pub config: Option<String>,
    pub error: Option<String>,
}

/// 配置导入请求（需管理员口令）
#[derive(Deserialize)]
pub struct ConfigImportRequest {
    #[serde(default)]
    pub passphrase: String,
    pub config: String,
}

/// 配置导入响应
#[derive(Serialize)]
pub struct ConfigImportResponse {
    pub success: bool,
    pub error: Option<String>,
}

/// 私钥导出响应（敏感：供用户另存备份）
#[derive(Serialize)]
pub struct KeysExportResponse {
    pub private_key: Option<String>,
    pub error: Option<String>,
}

/// 私钥备份确认响应
#[derive(Serialize)]
pub struct BackupAckResponse {
    pub success: bool,
    pub error: Option<String>,
}

/// 记录一条告警；若配置了 Webhook 则异步尽力外发（不阻塞主流程）
fn raise_alert(
    state: &AppState,
    level: crate::domain::alerts::AlertLevel,
    source: crate::domain::alerts::AlertSource,
    message: String,
) {
    let alert = state.alerts.push(level, source, message);
    let notify = {
        let mgr = state.config.lock().unwrap();
        mgr.load().ok().map(|c| c.notify)
    };
    if let Some(n) = notify {
        if let Some(url) = n.webhook_url.filter(|s| !s.trim().is_empty()) {
            let headers: Vec<(String, String)> = n
                .webhook_headers
                .iter()
                .map(|h| (h.name.clone(), h.value.clone()))
                .collect();
            tokio::spawn(crate::domain::alerts::dispatch_webhook(
                url,
                headers,
                n.webhook_body.clone(),
                alert,
            ));
        }
    }
}

/// 更新「私钥已备份」标记（失败仅记日志，不影响主流程）
fn set_key_backed_up(state: &AppState, backed_up: bool) {
    let guard = state.config.lock().unwrap();
    match guard.load() {
        Ok(mut cfg) => {
            cfg.keys.backed_up = backed_up;
            if let Err(e) = guard.save(&cfg) {
                tracing::warn!(err = %e, "更新私钥备份标记失败");
            }
        }
        Err(e) => tracing::warn!(err = %e, "读取配置失败，未更新私钥备份标记"),
    }
}

// ── Handler ────────────────────────────────────

async fn health(State(_state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// 保存 WebDAV 配置：先实测连通性（PROPFIND ping），通过后加密存储
async fn webdav_save(
    State(state): State<AppState>,
    Json(body): Json<WebdavSaveRequest>,
) -> Json<WebdavSaveResponse> {
    let url = crate::infra::target::webdav::DEFAULT_URL.to_string();
    if body.username.trim().is_empty() || body.password.is_empty() {
        return Json(WebdavSaveResponse {
            success: false,
            url: None,
            error: Some("用户名、密码均不能为空".to_string()),
        });
    }

    // 1) 实测连通性与凭据
    let probe = crate::infra::target::webdav::WebdavTarget::new(
        &url,
        body.username.trim(),
        &body.password,
    );
    if let Err(e) = probe.ping().await {
        return Json(WebdavSaveResponse {
            success: false,
            url: Some(url),
            error: Some(format!("WebDAV 连通性测试失败: {}", e)),
        });
    }

    // 2) 加密保存
    let saved = {
        let mgr = state.config.lock().unwrap();
        mgr.save_webdav(&url, body.username.trim(), &body.password)
    };
    match saved {
        Ok(_) => {
            // 热切换目标实现（无需重启服务）
            state.target.swap(Arc::new(crate::infra::target::webdav::WebdavTarget::new(
                &url,
                body.username.trim(),
                &body.password,
            )));
            Json(WebdavSaveResponse {
                success: true,
                url: Some(url),
                error: None,
            })
        }
        Err(e) => Json(WebdavSaveResponse {
            success: false,
            url: Some(url),
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 由配置构造响应（含 cron 校验）
fn config_response(
    cfg: &crate::infra::config::AppConfig,
    webdav_configured: bool,
    error: Option<String>,
) -> ConfigResponse {
    let schedule = cfg.backup.schedule_cron.clone().unwrap_or_default();
    let valid = crate::domain::scheduler::validate_cron(&schedule).is_ok();
    ConfigResponse {
        backup_paths: cfg.backup.paths.clone(),
        target_folder: cfg.backup.target_folder.clone(),
        schedule_cron: schedule,
        schedule_cron_valid: valid,
        webdav_configured,
        webdav_url: cfg.webdav.url.clone(),
        webhook_url: cfg.notify.webhook_url.clone(),
        webhook_headers: cfg.notify.webhook_headers.clone(),
        webhook_body: cfg.notify.webhook_body.clone(),
        key_backed_up: cfg.keys.backed_up,
        error,
    }
}

/// 判断 WebDAV 是否已配置（url + 用户名 + 密码齐全）
fn webdav_ready(cfg: &crate::infra::config::AppConfig) -> bool {
    cfg.webdav.url.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false)
        && cfg.webdav.username_enc.is_some()
        && cfg.webdav.password_enc.is_some()
}

async fn config_get(State(state): State<AppState>) -> Json<ConfigResponse> {
    let cfg_guard = state.config.lock().unwrap();
    match cfg_guard.load() {
        Ok(c) => Json(config_response(&c, webdav_ready(&c), None)),
        Err(e) => Json(ConfigResponse {
            backup_paths: Vec::new(),
            target_folder: state.target_folder.clone(),
            schedule_cron: String::new(),
            schedule_cron_valid: true,
            webdav_configured: false,
            webdav_url: None,
            webhook_url: None,
            webhook_headers: Vec::new(),
            webhook_body: None,
            key_backed_up: false,
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 保存配置（备份路径、目标文件夹、定时 cron）
async fn config_save(
    State(state): State<AppState>,
    Json(body): Json<ConfigSaveRequest>,
) -> Json<ConfigResponse> {
    let cfg_guard = state.config.lock().unwrap();
    let mut cfg = match cfg_guard.load() {
        Ok(c) => c,
        Err(_) => crate::infra::config::AppConfig::default(),
    };
    cfg.backup.paths = body.backup_paths.clone();
    if let Some(folder) = body.target_folder {
        if !folder.trim().is_empty() {
            cfg.backup.target_folder = folder;
        }
    }
    // 定时 cron：校验合法性；空串视为关闭
    if let Some(cron) = body.schedule_cron {
        let cron = cron.trim().to_string();
        if let Err(e) = crate::domain::scheduler::validate_cron(&cron) {
            let resp = config_response(&cfg, webdav_ready(&cfg), Some(e.to_string()));
            return Json(resp);
        }
        if cron.is_empty() {
            cfg.backup.schedule_cron = None;
        } else {
            cfg.backup.schedule_cron = Some(cron);
        }
    }
    match cfg_guard.save(&cfg) {
        Ok(_) => Json(config_response(&cfg, webdav_ready(&cfg), None)),
        Err(e) => Json(config_response(&cfg, false, Some(format!("{:#}", e)))),
    }
}

/// 当前 WebDAV 账号（从加密配置解密）
fn webdav_username(state: &AppState) -> Option<String> {
    let mgr = state.config.lock().unwrap();
    mgr.webdav_credentials().ok().and_then(|(u, _)| u)
}

/// 获取当前 WebDAV 账号信息
async fn user_info(State(state): State<AppState>) -> Json<UserInfoResponse> {
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

/// 触发备份：遍历配置的多备份路径
async fn backup_run(State(state): State<AppState>) -> Json<BackupResponse> {
    Json(run_backup_now(&state).await)
}

/// 执行一次备份（可被 HTTP handler 与定时调度器复用）
///
/// 返回 BackupResponse（含 uploaded/deleted/orphan_removed/error）。
pub async fn run_backup_now(state: &AppState) -> BackupResponse {
    // 1) 检查 WebDAV 是否已配置（未配置时 target 为占位适配器，会返回引导错误）
    let configured = state.target_ready
        || {
            let mgr = state.config.lock().unwrap();
            mgr.load().map(|c| webdav_ready(&c)).unwrap_or(false)
        };
    if !configured {
        raise_alert(
            state,
            crate::domain::alerts::AlertLevel::Warn,
            crate::domain::alerts::AlertSource::Config,
            "备份未执行：WebDAV 未配置".to_string(),
        );
        return BackupResponse {
            uploaded: 0,
            uploaded_bytes: 0,
            deleted: 0,
            unchanged: 0,
            orphan_removed: 0,
            error: Some("WebDAV 未配置，请先在设置中填写 WebDAV 地址与凭据".to_string()),
        };
    }

    // 2) 读取配置的备份路径（锁操作隔离在同步函数，避免跨 await）
    let (paths, target_folder, retention_cfg) = read_backup_config(state);

    if paths.is_empty() {
        raise_alert(
            state,
            crate::domain::alerts::AlertLevel::Warn,
            crate::domain::alerts::AlertSource::Backup,
            "备份未执行：未配置备份路径".to_string(),
        );
        return BackupResponse {
            uploaded: 0,
            uploaded_bytes: 0,
            deleted: 0,
            unchanged: 0,
            orphan_removed: 0,
            error: Some("未配置备份路径".to_string()),
        };
    }

    // 3) 执行多路径备份
    //    保留策略：启用时构造 RetentionPolicy，备份完成后清理目标端孤儿文件
    let retention = if retention_cfg.enabled && retention_cfg.cleanup_unmanaged {
        let rt = crate::domain::retention::RetentionPolicy::new(
            state.target.clone(),
            &target_folder,
        );
        let rt = if retention_cfg.min_age_days > 0 {
            rt.with_min_age_secs(retention_cfg.min_age_days * 86400)
        } else {
            rt
        };
        Some(rt)
    } else {
        None
    };

    let job = BackupJob {
        job_id: state.job_id.clone(),
        account: webdav_username(state),
        source: Arc::new(crate::infra::source::local::LocalFsSource::new(&paths[0])),
        target: state.target.clone(),
        crypto: state.crypto.get(),
        store: state.store.clone(),
        target_prefix: Some(target_folder),
        eventbus: Some(state.eventbus.clone()),
        retention,
    };
    match job.run_multi(&paths).await {
        Ok(summary) => BackupResponse {
            uploaded: summary.uploaded,
            uploaded_bytes: summary.uploaded_bytes,
            deleted: summary.deleted,
            unchanged: summary.unchanged,
            orphan_removed: summary.orphan_removed,
            error: None,
        },
        Err(e) => {
            let msg = format!("{:#}", e);
            raise_alert(
                state,
                crate::domain::alerts::AlertLevel::Error,
                crate::domain::alerts::AlertSource::Backup,
                format!("备份失败：{}", msg),
            );
            BackupResponse {
                uploaded: 0,
                uploaded_bytes: 0,
                deleted: 0,
                unchanged: 0,
                orphan_removed: 0,
                error: Some(msg),
            }
        }
    }
}

/// 读取备份配置（同步，锁在函数内释放）
fn read_backup_config(
    state: &AppState,
) -> (Vec<PathBuf>, String, crate::infra::config::RetentionConfig) {
    let cfg_guard = state.config.lock().unwrap();
    match cfg_guard.load() {
        Ok(cfg) => {
            let paths = cfg.backup.paths.iter().map(PathBuf::from).collect();
            let folder = cfg.backup.target_folder.clone();
            let retention = cfg.backup.retention.clone();
            (paths, folder, retention)
        }
        Err(_) => (
            Vec::new(),
            state.target_folder.clone(),
            crate::infra::config::RetentionConfig::default(),
        ),
    }
}

/// 列出配置的备份文件夹及其可恢复文件（从 SQLite 快照查询）
async fn restore_files(State(state): State<AppState>) -> Json<RestoreFilesResponse> {
    let paths = read_backup_config(&state).0;
    // 只展示当前账号的备份记录；未配置账号时归入默认 ''（旧数据）
    let account = webdav_username(&state).unwrap_or_default();
    let mut folders = Vec::new();

    for (i, path) in paths.iter().enumerate() {
        // 多路径备份时，每个路径的 job_id = "{base}-{i}"
        let job_id = format!("{}-{}", state.job_id, i);
        let entries = state.store.load_snapshot(&job_id, &account);

        let files = match entries {
            Ok(entries) => entries
                .iter()
                .map(|e| RestorableFile {
                    rel_path: e.rel_path.clone(),
                    size: e.size,
                    is_dir: e.is_dir,
                })
                .collect(),
            Err(_) => Vec::new(),
        };

        folders.push(RestorableFolder {
            path: path.to_string_lossy().into_owned(),
            has_backup: !files.is_empty(),
            files,
        });
    }

    Json(RestoreFilesResponse { folders, error: None })
}

/// 触发恢复
async fn restore_run(
    State(state): State<AppState>,
    body: Option<axum::extract::Json<RestoreRequest>>,
) -> Json<RestoreResponse> {
    let configured = state.target_ready
        || {
            let mgr = state.config.lock().unwrap();
            mgr.load().map(|c| webdav_ready(&c)).unwrap_or(false)
        };
    if !configured {
        raise_alert(
            &state,
            crate::domain::alerts::AlertLevel::Warn,
            crate::domain::alerts::AlertSource::Config,
            "恢复未执行：WebDAV 未配置".to_string(),
        );
        return Json(RestoreResponse {
            restored: 0,
            restored_bytes: 0,
            error: Some("WebDAV 未配置，请先在设置中填写 WebDAV 地址与凭据".to_string()),
        });
    }

    // 恢复目标根：优先用前端传入的 source_path（备份源路径，恢复到原位置），否则用默认目录
    let (files, restore_root) = match body {
        Some(Json(req)) => (
            req.files.unwrap_or_default(),
            req.source_path
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| state.default_restore_dir.to_string_lossy().into_owned()),
        ),
        None => (
            Vec::new(),
            state.default_restore_dir.to_string_lossy().into_owned(),
        ),
    };

    // 备份时每个源目录在目标端以其文件夹名分目录存放，恢复需还原该层级
    let source_root_name = std::path::Path::new(&restore_root)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned());

    // 定位该源路径对应的备份快照：优先精确路径匹配，其次按目录名匹配
    let paths = read_backup_config(&state).0;
    let root = std::path::Path::new(&restore_root);
    let idx = paths
        .iter()
        .position(|p| p.as_path() == root)
        .or_else(|| {
            root.file_name()
                .and_then(|n| paths.iter().position(|p| p.file_name() == Some(n)))
        });

    // 读取快照元数据：rel_path -> (明文大小, 原始 mtime 秒)
    // 用途：① 展示恢复总大小；② 恢复后回写快照，避免下次备份重复上传
    let mut meta: std::collections::HashMap<String, (u64, i64)> =
        std::collections::HashMap::new();
    let mut snapshot_target = None;
    if let Some(idx) = idx {
        let account = webdav_username(&state).unwrap_or_default();
        let job_id = format!("{}-{}", state.job_id, idx);
        if let Ok(entries) = state.store.load_snapshot(&job_id, &account) {
            for e in entries {
                if !e.is_dir {
                    meta.insert(e.rel_path.clone(), (e.size, e.mtime_secs));
                }
            }
        }
        snapshot_target = Some(crate::domain::restore::RestoreSnapshotTarget {
            store: state.store.clone(),
            job_id,
            account,
        });
    }

    let job = RestoreJob {
        target: state.target.clone(),
        crypto: state.crypto.get(),
        target_prefix: Some(state.target_folder.clone()),
        source_root_name,
        eventbus: Some(state.eventbus.clone()),
        meta,
        snapshot_target,
    };
    match job.run(&files, std::path::Path::new(&restore_root)).await {
        Ok(summary) => Json(RestoreResponse {
            restored: summary.restored,
            restored_bytes: summary.restored_bytes,
            error: None,
        }),
        Err(e) => {
            let msg = format!("{:#}", e);
            raise_alert(
                &state,
                crate::domain::alerts::AlertLevel::Error,
                crate::domain::alerts::AlertSource::Restore,
                format!("恢复失败：{}", msg),
            );
            Json(RestoreResponse {
                restored: 0,
                restored_bytes: 0,
                error: Some(msg),
            })
        }
    }
}

/// 当前 age 公钥（用于展示；永不回传私钥）
async fn keys_get(State(state): State<AppState>) -> Json<KeysInfoResponse> {
    let public_key = state.crypto.get().recipient.to_string();
    // 首次启动自动生成的私钥：一次性交给前端展示，读取后即清空
    let private_key_once = state.pending_key_reveal.lock().unwrap().take();
    Json(KeysInfoResponse {
        public_key,
        private_key_once,
        error: None,
    })
}

/// 更换密钥（用户自定义私钥）：校验格式 → 加密落盘密钥库 → 热切换加密会话
async fn keys_set(
    State(state): State<AppState>,
    Json(body): Json<KeysSetRequest>,
) -> Json<KeysChangeResponse> {
    let trimmed = body.private_key.trim();
    if trimmed.is_empty() {
        return Json(KeysChangeResponse {
            success: false,
            public_key: None,
            private_key: None,
            error: Some("私钥不能为空".to_string()),
        });
    }
    let keys = match crate::domain::crypto::AgeKeys::from_secret_key(trimmed) {
        Ok(k) => k,
        Err(e) => {
            return Json(KeysChangeResponse {
                success: false,
                public_key: None,
                private_key: None,
                error: Some(format!("{:#}", e)),
            })
        }
    };
    let public_key = keys.recipient_str();
    let ks_path = crate::infra::keystore::keystore_path(&state.cfg_dir);
    if let Err(e) = crate::infra::keystore::save_keystore(&keys, &state.passphrase, &ks_path) {
        return Json(KeysChangeResponse {
            success: false,
            public_key: Some(public_key),
            private_key: None,
            error: Some(format!("保存密钥库失败: {:#}", e)),
        });
    }
    state
        .crypto
        .swap(crate::domain::crypto::CryptoSession::full(&keys));
    // 用户粘贴了自己的私钥 ⇒ 其本人已持有，视为已备份
    set_key_backed_up(&state, true);
    tracing::info!("已更换 age 密钥（用户自定义私钥）");
    Json(KeysChangeResponse {
        success: true,
        public_key: Some(public_key),
        private_key: None,
        error: None,
    })
}

/// 自动生成新密钥对：加密落盘、热切换；私钥仅此一次返回（提醒用户保存）
async fn keys_generate(State(state): State<AppState>) -> Json<KeysChangeResponse> {
    let keys = crate::domain::crypto::AgeKeys::generate();
    let public_key = keys.recipient_str();
    let private_key = keys.to_secret_key();
    let ks_path = crate::infra::keystore::keystore_path(&state.cfg_dir);
    if let Err(e) = crate::infra::keystore::save_keystore(&keys, &state.passphrase, &ks_path) {
        return Json(KeysChangeResponse {
            success: false,
            public_key: Some(public_key),
            private_key: None,
            error: Some(format!("保存密钥库失败: {:#}", e)),
        });
    }
    state
        .crypto
        .swap(crate::domain::crypto::CryptoSession::full(&keys));
    // 新生成的私钥用户尚未保存 ⇒ 重置备份标记，UI 持续提示风险
    set_key_backed_up(&state, false);
    tracing::info!("已自动生成新的 age 密钥对");
    Json(KeysChangeResponse {
        success: true,
        public_key: Some(public_key),
        private_key: Some(private_key),
        error: None,
    })
}

/// 最近告警（监控告警：备份/恢复/配置异常）
async fn alerts_get(State(state): State<AppState>) -> Json<AlertsResponse> {
    Json(AlertsResponse {
        alerts: state.alerts.list(),
        error: None,
    })
}

/// 清空告警
async fn alerts_clear(State(state): State<AppState>) -> Json<AlertsResponse> {
    state.alerts.clear();
    Json(AlertsResponse {
        alerts: Vec::new(),
        error: None,
    })
}

/// 保存告警 Webhook 配置（地址/自定义请求头/请求体模板；地址空 = 关闭外发）
async fn webhook_save(
    State(state): State<AppState>,
    Json(body): Json<WebhookSaveRequest>,
) -> Json<WebhookSaveResponse> {
    let url = body.webhook_url.trim().to_string();
    // 过滤空请求头
    let headers: Vec<crate::infra::config::WebhookHeader> = body
        .headers
        .into_iter()
        .filter(|h| !h.name.trim().is_empty())
        .map(|h| crate::infra::config::WebhookHeader {
            name: h.name.trim().to_string(),
            value: h.value,
        })
        .collect();
    let body_template = body
        .body_template
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let guard = state.config.lock().unwrap();
    let mut cfg = match guard.load() {
        Ok(c) => c,
        Err(_) => crate::infra::config::AppConfig::default(),
    };
    cfg.notify.webhook_url = if url.is_empty() { None } else { Some(url) };
    cfg.notify.webhook_headers = headers;
    cfg.notify.webhook_body = body_template;
    match guard.save(&cfg) {
        Ok(_) => Json(WebhookSaveResponse {
            success: true,
            webhook_url: cfg.notify.webhook_url.clone(),
            headers: cfg.notify.webhook_headers.clone(),
            body_template: cfg.notify.webhook_body.clone(),
            error: None,
        }),
        Err(e) => Json(WebhookSaveResponse {
            success: false,
            webhook_url: None,
            headers: Vec::new(),
            body_template: None,
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 导出当前私钥明文（敏感操作：需管理员口令校验 + 前端二次确认）
async fn keys_export(
    State(state): State<AppState>,
    Json(body): Json<KeysExportRequest>,
) -> Json<KeysExportResponse> {
    // 校验管理员口令（安装向导设置的应用口令）
    if body.passphrase.as_str() != state.passphrase.expose_secret().as_str() {
        return Json(KeysExportResponse {
            private_key: None,
            error: Some("管理员口令错误".to_string()),
        });
    }
    let ks_path = crate::infra::keystore::keystore_path(&state.cfg_dir);
    match crate::infra::keystore::load_keystore(&state.passphrase, &ks_path) {
        Ok(keys) => {
            // 展示私钥即视为「需重新确认备份」：重置标记，使「我已妥善保存」按钮可再次点击
            set_key_backed_up(&state, false);
            Json(KeysExportResponse {
                private_key: Some(keys.to_secret_key()),
                error: None,
            })
        }
        Err(e) => Json(KeysExportResponse {
            private_key: None,
            error: Some(format!("读取密钥库失败: {:#}", e)),
        }),
    }
}

/// 确认已妥善备份私钥（消除 UI 的丢失风险提示）
async fn keys_backup_ack(State(state): State<AppState>) -> Json<BackupAckResponse> {
    set_key_backed_up(&state, true);
    Json(BackupAckResponse {
        success: true,
        error: None,
    })
}

/// 测试 Webhook 连通性：用请求中的当前表单配置发送一条测试告警
async fn webhook_test(Json(body): Json<WebhookTestRequest>) -> Json<WebhookTestResponse> {
    let url = body.webhook_url.trim().to_string();
    if url.is_empty() {
        return Json(WebhookTestResponse {
            success: false,
            status: None,
            error: Some("请先填写 Webhook 地址".to_string()),
        });
    }
    let headers: Vec<(String, String)> = body
        .headers
        .into_iter()
        .filter(|h| !h.name.trim().is_empty())
        .map(|h| (h.name.trim().to_string(), h.value))
        .collect();
    let body_template = body
        .body_template
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let alert = crate::domain::alerts::Alert {
        id: 0,
        level: crate::domain::alerts::AlertLevel::Warn,
        source: crate::domain::alerts::AlertSource::Config,
        message: "测试通知：fnos-backup Webhook 连通性测试".to_string(),
        ts,
    };
    match crate::domain::alerts::send_webhook(url, headers, body_template, alert).await {
        Ok(status) => Json(WebhookTestResponse {
            success: true,
            status: Some(status),
            error: None,
        }),
        Err(e) => Json(WebhookTestResponse {
            success: false,
            status: None,
            error: Some(e),
        }),
    }
}

/// 导出配置（含 WebDAV 凭据明文与 age 私钥；需管理员口令校验）
async fn config_export(
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
    let (cfg, creds) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        let creds = mgr.webdav_credentials().unwrap_or((None, None));
        (cfg, creds)
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
async fn config_import(
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
        if let Err(e) = mgr.save(&cfg) {
            return Json(ConfigImportResponse {
                success: false,
                error: Some(format!("保存配置失败: {:#}", e)),
            });
        }
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

    Json(ConfigImportResponse {
        success: true,
        error: None,
    })
}

/// 构建应用路由（不含 /api 前缀，由 main.rs nest("/api") 统一加前缀）
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws::ws_handler))
        .route("/webdav/config", post(webdav_save))
        .route("/user/info", get(user_info))
        .route("/config", get(config_get).post(config_save))
        .route("/config/export", post(config_export))
        .route("/config/import", post(config_import))
        .route("/backup/run", post(backup_run))
        .route("/restore/files", get(restore_files))
        .route("/restore/run", post(restore_run))
        .route("/keys", get(keys_get).post(keys_set))
        .route("/keys/generate", post(keys_generate))
        .route("/keys/export", post(keys_export))
        .route("/keys/backup-ack", post(keys_backup_ack))
        .route("/alerts", get(alerts_get).delete(alerts_clear))
        .route("/notify/webhook", post(webhook_save))
        .route("/notify/webhook/test", post(webhook_test))
        .with_state(state)
}
