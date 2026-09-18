//! REST 路由定义（axum）
//!
//! 目标存储为 kzwr 官方 WebDAV（ADR-009）。凭据经 /webdav/config 配置并加密存储。

use std::path::PathBuf;
use std::sync::Arc;

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
#[derive(Deserialize)]
pub struct WebdavSaveRequest {
    /// WebDAV 基址（如 https://dav.kzwr.com/dav）
    pub url: String,
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
    let url = body.url.trim().trim_end_matches('/').to_string();
    if url.is_empty() || body.username.trim().is_empty() || body.password.is_empty() {
        return Json(WebdavSaveResponse {
            success: false,
            url: None,
            error: Some("地址、用户名、密码均不能为空".to_string()),
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
        crypto: state.crypto.clone(),
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
        Err(e) => BackupResponse {
            uploaded: 0,
            uploaded_bytes: 0,
            deleted: 0,
            unchanged: 0,
            orphan_removed: 0,
            error: Some(format!("{:#}", e)),
        },
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

    let job = RestoreJob {
        target: state.target.clone(),
        crypto: state.crypto.clone(),
        target_prefix: Some(state.target_folder.clone()),
        eventbus: Some(state.eventbus.clone()),
    };
    match job.run(&files, std::path::Path::new(&restore_root)).await {
        Ok(summary) => Json(RestoreResponse {
            restored: summary.restored,
            restored_bytes: summary.restored_bytes,
            error: None,
        }),
        Err(e) => Json(RestoreResponse {
            restored: 0,
            restored_bytes: 0,
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 构建应用路由（不含 /api 前缀，由 main.rs nest("/api") 统一加前缀）
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws::ws_handler))
        .route("/webdav/config", post(webdav_save))
        .route("/user/info", get(user_info))
        .route("/config", get(config_get).post(config_save))
        .route("/backup/run", post(backup_run))
        .route("/restore/files", get(restore_files))
        .route("/restore/run", post(restore_run))
        .with_state(state)
}
