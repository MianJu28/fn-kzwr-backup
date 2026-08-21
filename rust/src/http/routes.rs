//! REST 路由定义（axum）

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};

use crate::domain::backup::BackupJob;
use crate::domain::restore::RestoreJob;
use crate::AppState;

// ── 响应/请求结构 ──────────────────────────────

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

/// kzwr 登录请求
#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// 登录响应
#[derive(Serialize)]
pub struct LoginResponse {
    pub success: bool,
    pub username: Option<String>,
    pub error: Option<String>,
}

/// 配置响应（不回传敏感字段明文）
#[derive(Serialize)]
pub struct ConfigResponse {
    pub backup_paths: Vec<String>,
    pub target_folder: String,
    pub logged_in: bool,
    pub login_bin_available: bool,
    pub error: Option<String>,
}

/// 配置保存请求
#[derive(Deserialize)]
pub struct ConfigSaveRequest {
    pub backup_paths: Vec<String>,
    pub target_folder: Option<String>,
}

/// 备份响应
#[derive(Serialize)]
pub struct BackupResponse {
    pub uploaded: usize,
    pub uploaded_bytes: u64,
    pub deleted: usize,
    pub unchanged: usize,
    pub error: Option<String>,
}

/// 恢复请求体
#[derive(Deserialize)]
pub struct RestoreRequest {
    /// 要恢复的文件相对路径列表；空 = 全量
    pub files: Option<Vec<String>>,
    /// 本地恢复目标目录
    pub restore_dir: String,
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

/// 登录 kzwr：保存凭据（加密）并获取 token
async fn auth_login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Json<LoginResponse> {
    match state
        .auth
        .login_with_credentials(&body.username, &body.password)
    {
        Ok(_) => Json(LoginResponse {
            success: true,
            username: Some(body.username),
            error: None,
        }),
        Err(e) => Json(LoginResponse {
            success: false,
            username: None,
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 读取配置（不含敏感字段明文）
async fn config_get(State(state): State<AppState>) -> Json<ConfigResponse> {
    let cfg_guard = state.config.lock().unwrap();
    let cfg = match cfg_guard.load() {
        Ok(c) => c,
        Err(e) => {
            return Json(ConfigResponse {
                backup_paths: Vec::new(),
                target_folder: state.target_folder.clone(),
                logged_in: false,
                login_bin_available: state.auth.bin_exists(),
                error: Some(format!("{:#}", e)),
            })
        }
    };
    let logged_in = cfg.kzwr.username_enc.is_some() && state.auth.has_credentials();
    Json(ConfigResponse {
        backup_paths: cfg.backup.paths.clone(),
        target_folder: cfg.backup.target_folder.clone(),
        logged_in,
        login_bin_available: state.auth.bin_exists(),
        error: None,
    })
}

/// 保存配置（备份路径、目标文件夹）
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
    match cfg_guard.save(&cfg) {
        Ok(_) => Json(ConfigResponse {
            backup_paths: cfg.backup.paths.clone(),
            target_folder: cfg.backup.target_folder.clone(),
            logged_in: cfg.kzwr.username_enc.is_some(),
            login_bin_available: state.auth.bin_exists(),
            error: None,
        }),
        Err(e) => Json(ConfigResponse {
            backup_paths: body.backup_paths,
            target_folder: cfg.backup.target_folder.clone(),
            logged_in: false,
            login_bin_available: state.auth.bin_exists(),
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 触发备份：确保登录 + 遍历配置的多备份路径
async fn backup_run(State(state): State<AppState>) -> Json<BackupResponse> {
    // 1) 确保已登录（token 过期自动重登）
    if let Err(e) = state.auth.ensure_login() {
        return Json(BackupResponse {
            uploaded: 0,
            uploaded_bytes: 0,
            deleted: 0,
            unchanged: 0,
            error: Some(format!("登录失败: {:#}", e)),
        });
    }

    // 2) 读取配置的备份路径（锁操作隔离在同步函数，避免跨 await）
    let (paths, target_folder) = read_backup_config(&state);

    if paths.is_empty() {
        return Json(BackupResponse {
            uploaded: 0,
            uploaded_bytes: 0,
            deleted: 0,
            unchanged: 0,
            error: Some("未配置备份路径".to_string()),
        });
    }

    // 3) 执行多路径备份
    let job = BackupJob {
        job_id: state.job_id.clone(),
        source: Arc::new(crate::infra::source::local::LocalFsSource::new(&paths[0])),
        target: state.target.clone(),
        crypto: state.crypto.clone(),
        store: state.store.clone(),
        target_prefix: Some(target_folder),
    };
    match job.run_multi(&paths).await {
        Ok(summary) => Json(BackupResponse {
            uploaded: summary.uploaded,
            uploaded_bytes: summary.uploaded_bytes,
            deleted: summary.deleted,
            unchanged: summary.unchanged,
            error: None,
        }),
        Err(e) => Json(BackupResponse {
            uploaded: 0,
            uploaded_bytes: 0,
            deleted: 0,
            unchanged: 0,
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 读取备份配置（同步，锁在函数内释放）
fn read_backup_config(state: &AppState) -> (Vec<PathBuf>, String) {
    let cfg_guard = state.config.lock().unwrap();
    match cfg_guard.load() {
        Ok(cfg) => {
            let paths = cfg.backup.paths.iter().map(PathBuf::from).collect();
            let folder = cfg.backup.target_folder.clone();
            (paths, folder)
        }
        Err(_) => (Vec::new(), state.target_folder.clone()),
    }
}

/// 列出配置的备份文件夹及其可恢复文件（从 SQLite 快照查询）
async fn restore_files(State(state): State<AppState>) -> Json<RestoreFilesResponse> {
    let paths = read_backup_config(&state).0;
    let mut folders = Vec::new();

    for (i, path) in paths.iter().enumerate() {
        // 多路径备份时，每个路径的 job_id = "{base}-{i}"
        let job_id = format!("{}-{}", state.job_id, i);
        let entries = state.store.load_snapshot(&job_id);

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
    let (files, restore_dir) = match body {
        Some(Json(req)) => (
            req.files.unwrap_or_default(),
            if req.restore_dir.trim().is_empty() {
                state.default_restore_dir.to_string_lossy().into_owned()
            } else {
                req.restore_dir
            },
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
    };
    match job.run(&files, std::path::Path::new(&restore_dir)).await {
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
        .route("/auth/login", post(auth_login))
        .route("/config", get(config_get).post(config_save))
        .route("/backup/run", post(backup_run))
        .route("/restore/files", get(restore_files))
        .route("/restore/run", post(restore_run))
        .with_state(state)
}
