//! REST 路由定义（axum）

use axum::extract::State;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};

use crate::domain::backup::BackupJob;
use crate::domain::restore::RestoreJob;
use crate::AppState;

/// 健康检查
#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

/// 备份响应
#[derive(Serialize)]
pub struct BackupResponse {
    pub job_id: String,
    pub uploaded: usize,
    pub uploaded_bytes: u64,
    pub deleted: usize,
    pub unchanged: usize,
    pub error: Option<String>,
}

/// 恢复请求体（files 为空表示全量恢复）
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

async fn health(State(_state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// 触发一次备份
async fn backup_run(State(state): State<AppState>) -> Json<BackupResponse> {
    let job = BackupJob {
        job_id: state.job_id.clone(),
        source: state.source.clone(),
        target: state.target.clone(),
        crypto: state.crypto.clone(),
        store: state.store.clone(),
        target_prefix: state.target_prefix.clone(),
    };
    match job.run(&state.source_root).await {
        Ok(summary) => Json(BackupResponse {
            job_id: state.job_id.clone(),
            uploaded: summary.uploaded,
            uploaded_bytes: summary.uploaded_bytes,
            deleted: summary.deleted,
            unchanged: summary.unchanged,
            error: None,
        }),
        Err(e) => Json(BackupResponse {
            job_id: state.job_id.clone(),
            uploaded: 0,
            uploaded_bytes: 0,
            deleted: 0,
            unchanged: 0,
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 触发一次恢复
async fn restore_run(
    State(state): State<AppState>,
    body: Option<axum::extract::Json<RestoreRequest>>,
) -> Json<RestoreResponse> {
    let (files, restore_dir) = match body {
        Some(Json(req)) => (req.files.unwrap_or_default(), req.restore_dir),
        None => (Vec::new(), std::env::temp_dir().join("fnos-restore").to_string_lossy().into_owned()),
    };

    let job = RestoreJob {
        target: state.target.clone(),
        crypto: state.crypto.clone(),
        target_prefix: state.target_prefix.clone(),
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

/// 构建应用路由
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/backup/run", post(backup_run))
        .route("/api/restore/run", post(restore_run))
        .with_state(state)
}
