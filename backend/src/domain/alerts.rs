//! 告警（监控告警）
//!
//! 备份/恢复/定时备份失败时生成告警，保存最近若干条供 UI 查询；
//! 若配置了 Webhook 地址则尽力外发（失败仅记日志，不影响主流程，符合 ADR-007 事件旁路语义）。

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;

/// 告警级别
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AlertLevel {
    Error,
    Warn,
}

/// 告警来源
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AlertSource {
    Backup,
    Restore,
    Scheduler,
    Config,
}

/// 告警条目
#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub id: u64,
    pub level: AlertLevel,
    pub source: AlertSource,
    pub message: String,
    /// 时间戳（毫秒）
    pub ts: i64,
}

struct Inner {
    items: VecDeque<Alert>,
    next_id: u64,
    capacity: usize,
}

/// 告警汇聚点（进程内环形缓冲，超出容量丢弃最旧的）
pub struct AlertSink {
    inner: Mutex<Inner>,
}

impl AlertSink {
    /// 创建（capacity：最多保留条数）
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                items: VecDeque::new(),
                next_id: 1,
                capacity: capacity.max(1),
            }),
        }
    }

    /// 记录一条告警并返回它
    pub fn push(&self, level: AlertLevel, source: AlertSource, message: String) -> Alert {
        let mut guard = self.inner.lock().unwrap();
        let alert = Alert {
            id: guard.next_id,
            level,
            source,
            message,
            ts: now_ms(),
        };
        guard.next_id += 1;
        guard.items.push_back(alert.clone());
        while guard.items.len() > guard.capacity {
            guard.items.pop_front();
        }
        alert
    }

    /// 列出全部告警（按时间正序）
    pub fn list(&self) -> Vec<Alert> {
        self.inner.lock().unwrap().items.iter().cloned().collect()
    }

    /// 清空告警
    pub fn clear(&self) {
        self.inner.lock().unwrap().items.clear();
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// 外发告警到 Webhook（尽力而为：失败仅记录日志）
///
/// - `headers`：自定义请求头（键值对，空键忽略）
/// - `body_template`：自定义请求体模板，支持占位符 `{{message}}`/`{{level}}`/
///   `{{source}}`/`{{ts}}`/`{{id}}`；留空则发送默认 JSON
pub async fn dispatch_webhook(
    url: String,
    headers: Vec<(String, String)>,
    body_template: Option<String>,
    alert: Alert,
) {
    match send_webhook(url, headers, body_template, alert).await {
        Ok(_) => {}
        Err(e) => tracing::warn!(err = %e, "Webhook 外发失败"),
    }
}

/// 实际发送 Webhook 请求，返回 HTTP 状态码（供 /notify/webhook/test 反馈连通性）
pub async fn send_webhook(
    url: String,
    headers: Vec<(String, String)>,
    body_template: Option<String>,
    alert: Alert,
) -> Result<u16, String> {
    if url.trim().is_empty() {
        return Err("Webhook 地址为空".to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("构建 Webhook 客户端失败: {e}"))?;

    let mut req = client.post(&url);
    let mut has_content_type = false;
    for (name, value) in &headers {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        if name.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        req = req.header(name, value);
    }

    let template = body_template.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let body = match template {
        Some(t) => render_template(t, &alert),
        None => serde_json::json!({
            "id": alert.id,
            "level": alert.level,
            "source": alert.source,
            "message": alert.message,
            "ts": alert.ts,
        })
        .to_string(),
    };
    if !has_content_type {
        req = req.header("Content-Type", "application/json; charset=utf-8");
    }

    let resp = req
        .body(body)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(format!("服务器返回 {}", status));
    }
    Ok(status)
}

/// 渲染请求体模板：替换内置占位符
fn render_template(template: &str, alert: &Alert) -> String {
    template
        .replace("{{message}}", &alert.message)
        .replace("{{level}}", level_name(alert.level))
        .replace("{{source}}", source_name(alert.source))
        .replace("{{ts}}", &alert.ts.to_string())
        .replace("{{id}}", &alert.id.to_string())
}

/// 告警级别短名（与序列化保持一致）
fn level_name(level: AlertLevel) -> &'static str {
    match level {
        AlertLevel::Error => "error",
        AlertLevel::Warn => "warn",
    }
}

/// 告警来源短名（与序列化保持一致）
fn source_name(source: AlertSource) -> &'static str {
    match source {
        AlertSource::Backup => "backup",
        AlertSource::Restore => "restore",
        AlertSource::Scheduler => "scheduler",
        AlertSource::Config => "config",
    }
}
