//! 密钥管理 + 告警 + Webhook
//!
//! 密钥相关端点多数要求管理员口令（`ExposeSecret` 用于校验）。
//! 告警与 Webhook 放在一起：Webhook 是告警的**外发**通道。

use age::secrecy::ExposeSecret;
use axum::extract::State;
use axum::response::Json;

use crate::AppState;

use super::common::set_key_backed_up;
use super::types::*;

/// 当前 age 公钥（用于展示；永不回传私钥）
pub(super) async fn keys_get(State(state): State<AppState>) -> Json<KeysInfoResponse> {
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
pub(super) async fn keys_set(
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
    state.audit.record(
        "keys.set",
        format!("更换 age 私钥（新公钥 {}）", public_key),
        true,
        None,
    );
    tracing::info!("已更换 age 密钥（用户自定义私钥）");
    Json(KeysChangeResponse {
        success: true,
        public_key: Some(public_key),
        private_key: None,
        error: None,
    })
}

/// 自动生成新密钥对：加密落盘、热切换；私钥仅此一次返回（提醒用户保存）
pub(super) async fn keys_generate(State(state): State<AppState>) -> Json<KeysChangeResponse> {
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
    state.audit.record(
        "keys.generate",
        format!("生成新 age 密钥对（公钥 {}）", public_key),
        true,
        None,
    );
    tracing::info!("已自动生成新的 age 密钥对");
    Json(KeysChangeResponse {
        success: true,
        public_key: Some(public_key),
        private_key: Some(private_key),
        error: None,
    })
}

/// 最近告警（监控告警：备份/恢复/配置异常）
pub(super) async fn alerts_get(State(state): State<AppState>) -> Json<AlertsResponse> {
    Json(AlertsResponse {
        alerts: state.alerts.list(),
        error: None,
    })
}

/// 清空告警
pub(super) async fn alerts_clear(State(state): State<AppState>) -> Json<AlertsResponse> {
    state.alerts.clear();
    Json(AlertsResponse {
        alerts: Vec::new(),
        error: None,
    })
}

/// 保存告警 Webhook 配置（地址/自定义请求头/请求体模板；地址空 = 关闭外发）
pub(super) async fn webhook_save(
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
pub(super) async fn keys_export(
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
            state.audit.record(
                "keys.export",
                "导出 age 私钥明文（管理员口令校验通过）",
                true,
                None,
            );
            Json(KeysExportResponse {
                private_key: Some(keys.to_secret_key()),
                error: None,
            })
        }
        Err(e) => {
            state.audit.record(
                "keys.export",
                format!("导出 age 私钥失败：{:#}", e),
                false,
                None,
            );
            Json(KeysExportResponse {
                private_key: None,
                error: Some(format!("读取密钥库失败: {:#}", e)),
            })
        }
    }
}

/// 确认已妥善备份私钥（消除 UI 的丢失风险提示）
pub(super) async fn keys_backup_ack(State(state): State<AppState>) -> Json<BackupAckResponse> {
    set_key_backed_up(&state, true);
    state
        .audit
        .record("keys.backup_ack", "确认已妥善保存 age 私钥", true, None);
    Json(BackupAckResponse {
        success: true,
        error: None,
    })
}

/// 测试 Webhook 连通性：用请求中的当前表单配置发送一条测试告警
pub(super) async fn webhook_test(Json(body): Json<WebhookTestRequest>) -> Json<WebhookTestResponse> {
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
        message: "测试通知：fn-kzwr-backup Webhook 连通性测试".to_string(),
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
