//! 跨模块共享的辅助（错误响应 / id 生成 / 告警 / 日志清洗）
//!
//! 这些函数被**多个** handler 文件调用，因此放共享模块；
//! 只被单文件使用的辅助留在各自文件里（避免这里变成杂物间）。


use crate::AppState;

/// 记录一条告警；若配置了 Webhook 则异步尽力外发（不阻塞主流程）
///
/// `pub(crate)`：增强插件（如 kzwr）也用它上报告警。
pub(super) fn raise_alert(
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
pub(super) fn set_key_backed_up(state: &AppState, backed_up: bool) {
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

// ── 多任务 / 多目标（ADR-014）辅助 ──────────────────────────────────────
/// 通用 JSON 错误响应（`{success:false, error}`）
pub(super) fn err(e: impl std::fmt::Display) -> serde_json::Value {
    serde_json::json!({ "success": false, "error": e.to_string() })
}

/// 生成稳定 id（前缀 + 时间戳/序号十六进制，无需第三方 uuid 依赖）
pub(super) fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{:x}{:02x}", ms & 0xffff_ffff, seq & 0xff)
}

/// 默认任务 id：首个启用的任务，否则首个任务
pub(super) fn default_task_id(cfg: &crate::infra::config::AppConfig) -> Option<String> {
    cfg.tasks
        .iter()
        .find(|t| t.enabled)
        .or_else(|| cfg.tasks.first())
        .map(|t| t.id.clone())
}

/// 去掉 ANSI 转义序列（CSI：`ESC [ 参数… 终止字节`）
///
/// 旧版本日志里带着 tracing 的终端颜色码（`\x1b[2m`、`\x1b[32m` 等），直接展示会变成
/// `[2m2026-...` 这类乱码；新版本已在写入端关闭 ANSI，这里再兜一层保证历史日志也干净。
pub(super) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                // 参数字节（0x30-0x3F）与中间字节（0x20-0x2F）之后是终止字节（0x40-0x7E）
                for c2 in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c2) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// 把行首的 UTC 时间戳换算成宿主本地时间。
///
/// tracing 默认写 UTC（`2026-09-21T04:14:21.827270Z`），在东八区看起来比本地少 8 小时；
/// 新版本写入端已改为直接写宿主本地时间（无 `Z` 后缀），这里负责把**历史行**也换算过来，
/// 因此「日志页 / 下载的 app.log」时间口径一致。
pub(super) fn localize_log_time(line: &str) -> String {
    let (ts, rest) = match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], &line[i..]),
        None => (line, ""),
    };
    if !ts.ends_with('Z') {
        return line.to_string();
    }
    match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => format!(
            "{}{}",
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S%.3f"),
            rest
        ),
        Err(_) => line.to_string(),
    }
}

// ── 增强功能公共辅助 ──────────────────────────────────────────────────

/// 去重告警：同来源 + 同文案已存在时不再重复记录与外发
pub(crate) fn raise_alert_once(
    state: &AppState,
    level: crate::domain::alerts::AlertLevel,
    source: crate::domain::alerts::AlertSource,
    message: String,
) {
    let dup = state
        .alerts
        .list()
        .iter()
        .any(|a| a.source == source && a.message == message);
    if dup {
        return;
    }
    raise_alert(state, level, source, message);
}

/// 人类可读的字节数（用于告警文案）
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}
