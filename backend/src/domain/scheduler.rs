//! 定时备份调度（应用编排层）
//!
//! 多任务（ADR-014）后：**每个启用的任务按各自的 cron 独立触发**。
//! 调度器维护 `任务 id → (cron, 下次触发时间)`，每轮 tick：
//! 1. 读取任务列表（支持热更新：新增/删除/停用/改 cron 即时生效）
//! 2. 重建/清理待触发表
//! 3. 到点调用 `run_task_now(state, task_id)`
//!
//! 并发由 `run_task_now` 内的全局标志（`AppState.backup_running`）保证：
//! 同一时刻只跑一个备份任务，重叠触发会返回 `skipped`。

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Local};
use tracing::{info, warn};

use crate::AppState;

/// 启动定时备份调度器（后台 tokio 任务）
///
/// `interval_secs`：轮询粒度（秒），cron 最小粒度为分钟，通常传 30。
pub fn spawn_scheduler(state: AppState, interval_secs: u64) {
    tokio::spawn(async move {
        scheduler_loop(state, interval_secs).await;
    });
    info!("定时备份调度器已启动（每任务独立 cron）");
}

/// 计算 cron 在 `after` 之后的首次触发时间（宿主本地时区）
fn first_after(expr: &str, after: DateTime<Local>) -> Option<DateTime<Local>> {
    croner::Cron::new(expr).parse().ok()?.iter_after(after).next()
}

/// 调度主循环
async fn scheduler_loop(state: AppState, interval_secs: u64) {
    let tick = Duration::from_secs(interval_secs.max(10));
    // 任务 id → (cron 表达式, 下次触发时间)
    let mut pending: HashMap<String, (String, DateTime<Local>)> = HashMap::new();

    loop {
        // 1) 读取任务列表（热更新）
        let tasks = {
            let guard = state.config.lock().unwrap();
            match guard.load() {
                Ok(cfg) => cfg.tasks,
                Err(e) => {
                    warn!(err = %e, "读取调度配置失败，稍后重试");
                    Vec::new()
                }
            }
        };

        // 启用且配置了 cron 的任务
        let active: HashMap<String, String> = tasks
            .iter()
            .filter(|t| t.enabled)
            .filter_map(|t| {
                let cron = t.schedule_cron.clone().unwrap_or_default();
                let cron = cron.trim().to_string();
                if cron.is_empty() {
                    None
                } else {
                    Some((t.id.clone(), cron))
                }
            })
            .collect();

        // 2) 清理：已删除/停用/改了 cron 的任务重新排队
        pending.retain(|id, (cron, _)| active.get(id).map(|c| c == cron).unwrap_or(false));

        // 3) 为新任务（或 cron 变更的任务）计算下次触发
        for (id, cron) in &active {
            if pending.contains_key(id) {
                continue;
            }
            match first_after(cron, Local::now()) {
                Some(next) => {
                    info!(task = %id, expr = %cron, next = %next, "已登记定时备份");
                    pending.insert(id.clone(), (cron.clone(), next));
                }
                None => warn!(task = %id, expr = %cron, "cron 表达式无效或永不触发，已跳过"),
            }
        }

        // 4) 到点触发
        let now = Local::now();
        let due: Vec<String> = pending
            .iter()
            .filter(|(_, (_, next))| *next <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            // 先把下次触发时间推进，避免执行耗时导致重复触发
            if let Some((cron, _)) = pending.get(&id).cloned() {
                if let Some(next) = first_after(&cron, now) {
                    pending.insert(id.clone(), (cron, next));
                } else {
                    pending.remove(&id);
                }
            }
            info!(task = %id, "定时备份触发，开始执行");
            let resp = crate::http::routes::run_task_now(&state, &id).await;
            if resp.skipped {
                warn!(task = %id, "已有备份任务正在执行，跳过本次定时触发");
            } else if let Some(err) = &resp.error {
                warn!(task = %id, err = %err, "定时备份执行失败");
            } else {
                info!(
                    task = %id,
                    uploaded = resp.uploaded,
                    deleted = resp.deleted,
                    "定时备份执行完成"
                );
            }
        }

        tokio::time::sleep(tick).await;
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
    let now = Local::now();
    let mut out: Vec<String> = Vec::new();
    for t in cron.iter_after(now).take(count) {
        out.push(t.format("%Y-%m-%d %H:%M").to_string());
    }
    Ok(out)
}

/// 服务器时区说明（供 UI 标注 cron 的解释基准）
pub fn timezone_label() -> String {
    Local::now().format("%Z (UTC%:z)").to_string()
}

/// 宿主时区相对 UTC 的分钟偏移（如东八区 = 480）。
///
/// 时间戳在传输层统一用 epoch（毫秒，与时区无关），由前端按**宿主时区**展示，
/// 避免浏览器时区与 NAS 不一致时显示成另一个时间。
pub fn utc_offset_minutes() -> i64 {
    Local::now().offset().local_minus_utc() as i64 / 60
}
