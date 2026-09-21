//! 定时备份调度（应用编排层）
//!
//! 读取配置中的 cron 表达式，后台任务循环计算下次触发时间，
//! 到点后调用 `run_backup_now` 执行增量备份。
//! 支持运行中热更新配置（每次循环重新读取 cron，无需重启）。

use std::time::Duration;

use anyhow::Result;
use tracing::{info, warn};

use crate::AppState;

/// 启动定时备份调度器（后台 tokio 任务）
///
/// `interval`：cron 触发时间粒度（秒），用于轮询是否到点，
/// 通常传 60（cron 最小粒度是分钟）。
pub fn spawn_scheduler(state: AppState, interval_secs: u64) {
    tokio::spawn(async move {
        scheduler_loop(state, interval_secs).await;
    });
    info!("定时备份调度器已启动");
}

/// 调度主循环
async fn scheduler_loop(state: AppState, interval_secs: u64) {
    let tick = Duration::from_secs(interval_secs.max(10));

    loop {
        // 1) 读取当前 cron 配置（支持热更新）
        let cron_expr = {
            let guard = state.config.lock().unwrap();
            match guard.load() {
                Ok(cfg) => cfg.backup.schedule_cron.unwrap_or_default(),
                Err(e) => {
                    warn!(err = %e, "读取调度配置失败，稍后重试");
                    String::new()
                }
            }
        };

        let cron_expr = cron_expr.trim();
        if cron_expr.is_empty() {
            // 未配置定时 → 空闲轮询等待配置
            tokio::time::sleep(tick).await;
            continue;
        }

        // 2) 解析 cron，计算下次触发时间
        let parsed = match croner::Cron::new(cron_expr).parse() {
            Ok(c) => c,
            Err(e) => {
                warn!(expr = cron_expr, err = %e, "cron 表达式无效，等待配置修正");
                tokio::time::sleep(tick).await;
                continue;
            }
        };

        let now = chrono::Local::now();
        let next = match parsed.iter_after(now).next() {
            Some(t) => t,
            None => {
                warn!(expr = cron_expr, "无法计算下次触发时间（cron 永不触发？）");
                tokio::time::sleep(tick).await;
                continue;
            }
        };
        let wait = (next - chrono::Local::now()).to_std().unwrap_or(Duration::from_secs(60));
        info!(expr = cron_expr, next = %next, wait_secs = wait.as_secs(), "下次定时备份");

        // 3) 等到点（分小段 sleep 以便及时响应配置变更）
        let mut remaining = wait;
        while remaining > tick {
            tokio::time::sleep(tick).await;
            remaining = remaining.saturating_sub(tick);
            // 中途检查 cron 是否变更（配置热更新）
            let current = current_cron(&state);
            if current != cron_expr {
                info!("cron 配置已变更，重新调度");
                remaining = Duration::ZERO;
                break;
            }
        }
        if remaining > Duration::ZERO {
            tokio::time::sleep(remaining).await;
        }

        // 4) 到点触发备份
        //
        // 运行互斥由 `run_backup_now` 内的全局标志（`AppState.backup_running`）保证：
        // 若手动触发或上一轮备份仍在执行，本次返回 `skipped`，不会并发跑两份备份。
        info!("定时备份触发，开始执行");
        let resp = crate::http::routes::run_backup_now(&state).await;
        if resp.skipped {
            warn!("已有备份正在执行，跳过本次定时触发");
        } else if let Some(err) = &resp.error {
            warn!(err = %err, "定时备份执行失败");
        } else {
            info!(
                uploaded = resp.uploaded,
                deleted = resp.deleted,
                orphan_removed = resp.orphan_removed,
                "定时备份执行完成"
            );
        }
    }
}

/// 读取当前 cron 配置
fn current_cron(state: &AppState) -> String {
    let guard = state.config.lock().unwrap();
    match guard.load() {
        Ok(cfg) => cfg.backup.schedule_cron.unwrap_or_default(),
        Err(_) => String::new(),
    }
}

/// 校验 cron 表达式是否合法（供配置 API 使用）
pub fn validate_cron(expr: &str) -> Result<()> {
    if expr.trim().is_empty() {
        return Ok(());
    }
    croner::Cron::new(expr)
        .parse()
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("cron 表达式无效: {e}"))
}

/// 计算未来 `count` 次触发时间（**服务器本地时区**，与调度器一致）
///
/// 返回 `("2026-09-21 00:00", ...)` 形式的字符串列表，供 UI 展示「下次运行」。
pub fn next_runs(expr: &str, count: usize) -> Result<Vec<String>> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Ok(Vec::new());
    }
    let cron = croner::Cron::new(expr)
        .parse()
        .map_err(|e| anyhow::anyhow!("cron 表达式无效: {e}"))?;
    // croner 的时区由传入的 DateTime 决定：用 Local 让表达式按 NAS 本地时间解释
    let now = chrono::Local::now();
    let mut out: Vec<String> = Vec::new();
    for t in cron.iter_after(now).take(count) {
        out.push(t.format("%Y-%m-%d %H:%M").to_string());
    }
    Ok(out)
}

/// 服务器时区说明（供 UI 标注 cron 的解释基准）
pub fn timezone_label() -> String {
    chrono::Local::now().format("%Z (UTC%:z)").to_string()
}

/// 宿主时区相对 UTC 的分钟偏移（如东八区 = 480）。
///
/// 时间戳在传输层统一用 epoch（毫秒，与时区无关），由前端按**宿主时区**展示，
/// 避免浏览器时区与 NAS 不一致时显示成另一个时间。
pub fn utc_offset_minutes() -> i64 {
    chrono::Local::now().offset().local_minus_utc() as i64 / 60
}
