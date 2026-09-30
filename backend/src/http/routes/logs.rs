//! 运行日志（查看 / 清空 / 下载）


use axum::extract::State;
use axum::response::Json;

use crate::AppState;

use super::common::{localize_log_time, strip_ansi};
use super::types::*;

/// 读取运行日志末尾（文件超过 5MB 时自动轮转，故整读安全）
pub(super) async fn logs_get(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<LogsQuery>,
) -> Json<LogsResponse> {
    let path = state.log_file.clone();
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        return Json(LogsResponse {
            lines: Vec::new(),
            truncated: false,
            size: 0,
            error: None,
        });
    }
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return Json(LogsResponse {
                lines: Vec::new(),
                truncated: false,
                size,
                error: Some(format!("读取日志失败: {e}")),
            })
        }
    };
    let all: Vec<String> = String::from_utf8_lossy(&bytes)
        .lines()
        .map(strip_ansi)
        .map(|l| localize_log_time(&l))
        .collect();
    let total = all.len();
    let tail = q.tail.unwrap_or(800).min(5000);
    let start = total.saturating_sub(tail);
    let lines = all[start..].to_vec();
    Json(LogsResponse {
        truncated: total > tail,
        lines,
        size,
        error: None,
    })
}

/// 清空运行日志
pub(super) async fn logs_clear(State(state): State<AppState>) -> Json<serde_json::Value> {
    let res = std::fs::write(&state.log_file, b"");
    match res {
        Ok(_) => {
            state.audit.record("logs.clear", "清空运行日志", true, None);
            Json(serde_json::json!({ "ok": true, "error": null }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "error": format!("清空失败: {e}") })),
    }
}

/// 下载运行日志（text/plain 附件）
pub(super) async fn logs_download(State(state): State<AppState>) -> axum::response::Response {
    use axum::response::IntoResponse;
    match std::fs::read(&state.log_file) {
        Ok(bytes) => {
            // 下载件同样去掉 ANSI 颜色码，并把历史 UTC 时间戳换算成宿主本地时间
            let raw = strip_ansi(&String::from_utf8_lossy(&bytes));
            let text = raw
                .lines()
                .map(localize_log_time)
                .collect::<Vec<String>>()
                .join("\n");
            let mut resp = (axum::http::StatusCode::OK, text).into_response();
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            if let Ok(v) = axum::http::HeaderValue::from_str("attachment; filename=\"app.log\"") {
                resp.headers_mut()
                    .insert(axum::http::header::CONTENT_DISPOSITION, v);
            }
            resp
        }
        Err(e) => (
            axum::http::StatusCode::NOT_FOUND,
            format!("日志文件不存在: {e}"),
        )
            .into_response(),
    }
}
