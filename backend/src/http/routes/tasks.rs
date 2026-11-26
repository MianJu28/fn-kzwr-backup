//! 任务管理（多任务，ADR-014）+ 立即执行
//!
//! 任务 = 源路径集 + 目标 + cron + 保留策略。`run_backup_now` / `run_task_now`
//! 也被调度器（`domain::scheduler`）调用，故为 `pub`。

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;

use crate::domain::backup::BackupJob;
use crate::AppState;

use super::common::{default_task_id, err};
use super::common::{human_bytes, new_id, raise_alert};
use super::types::*;

/// 任务运行上下文：目标适配器 + 快照分桶用的账号
pub(super) struct TaskContext {
    pub task: crate::infra::config::TaskConfig,
    pub target: Arc<dyn crate::infra::storage_trait::TargetStorage>,
    /// 目标任务凭据的用户名（快照按此分桶，实现「每目标独立快照」）
    pub account: Option<String>,
    /// 目标任务是否已就绪（凭据齐备）
    pub ready: bool,
}

/// 取某个任务的运行上下文（任务不存在/未启用/目标未就绪 → Err(提示)）
pub(super) fn task_context(state: &AppState, task_id: &str) -> Result<TaskContext, String> {
    let (task, target_cfg) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().map_err(|e| format!("{:#}", e))?;
        let task = cfg
            .task_by_id(task_id)
            .cloned()
            .ok_or_else(|| format!("任务不存在：{task_id}"))?;
        let tcfg = cfg.target_by_id(&task.target_id).cloned();
        (task, tcfg)
    };
    if !task.enabled {
        return Err(format!(
            "任务「{}」已停用",
            if task.name.is_empty() { &task.id } else { &task.name }
        ));
    }
    let target_cfg = target_cfg.ok_or_else(|| {
        format!(
            "任务「{}」引用的目标不存在（{}），请在「任务管理」中重新选择",
            if task.name.is_empty() { &task.id } else { &task.name },
            task.target_id
        )
    })?;
    let account = {
        let mgr = state.config.lock().unwrap();
        mgr.target_credentials(&target_cfg)
            .ok()
            .and_then(|(u, _)| u)
    };
    let ready = target_cfg.enabled && state.targets.is_ready(&task.target_id);
    Ok(TaskContext {
        target: state.target_for(&task.target_id),
        task,
        account,
        ready,
    })
}

/// 任务视图（供 `/api/tasks`）
pub(super) fn task_view(state: &AppState, t: &crate::infra::config::TaskConfig) -> serde_json::Value {
    let target_name = {
        let mgr = state.config.lock().unwrap();
        mgr.load()
            .ok()
            .and_then(|c| c.target_by_id(&t.target_id).map(|x| x.name.clone()))
            .unwrap_or_default()
    };
    let next = crate::domain::scheduler::next_runs(
        t.schedule_cron.as_deref().unwrap_or_default(),
        3,
    )
    .unwrap_or_default();
    serde_json::json!({
        "id": t.id,
        "name": t.name,
        "enabled": t.enabled,
        "paths": t.paths,
        "target_id": t.target_id,
        "target_name": target_name,
        "target_ready": state.targets.is_ready(&t.target_id),
        "target_folder": t.target_folder,
        "schedule_cron": t.schedule_cron.clone().unwrap_or_default(),
        "schedule_next": next,
        "retention": RetentionView::from(&t.retention),
        "strict": t.strict,
    })
}

/// 触发备份：遍历配置的多备份路径
pub(super) async fn backup_run(State(state): State<AppState>) -> Json<BackupResponse> {
    let resp = run_backup_now(&state).await;
    state.audit.record(
        "backup.run",
        match (&resp.error, resp.skipped) {
            (Some(e), _) => format!("手动备份失败：{e}"),
            (None, true) => "手动备份跳过（已有备份在执行）".to_string(),
            (None, false) => format!(
                "手动备份完成：上传 {} 个文件（{}），清理孤儿 {} 个，回收站 {} 项",
                resp.uploaded,
                human_bytes(resp.uploaded_bytes),
                resp.orphan_removed,
                resp.trash_emptied
            ),
        },
        resp.error.is_none(),
        None,
    );
    Json(resp)
}

// ── 任务管理 API（多任务，ADR-014）────────────────────────────────────

/// `GET /api/tasks`：任务列表（含目标名、就绪状态、下次触发时间）
pub(super) async fn tasks_list(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cfg = { state.config.lock().unwrap().load().unwrap_or_default() };
    let list: Vec<serde_json::Value> = cfg.tasks.iter().map(|t| task_view(&state, t)).collect();
    Json(serde_json::json!({
        "tasks": list,
        "targets": cfg.targets.iter().map(|t| serde_json::json!({
            "id": t.id, "name": t.name, "ready": state.targets.is_ready(&t.id), "enabled": t.enabled,
        })).collect::<Vec<_>>(),
    }))
}

/// `POST /api/tasks`：新建/更新任务（未传字段保持原值）
pub(super) async fn task_save(
    State(state): State<AppState>,
    Json(body): Json<TaskSaveRequest>,
) -> Json<serde_json::Value> {
    // **整段持有同一把锁**：load → 校验 → 改 → save 必须在一次临界区内完成。
    //
    // 曾经是「先 load（锁 A 用完即放）… 末尾再 save（锁 B）」，两个并发的保存请求
    // 会各自读到同一份旧配置，后写的覆盖先写的 —— 用户在界面上连点两次「保存」
    // 就静默丢一次修改，而且没有任何日志。config_save 一直是单 guard 的正确写法，
    // 这里照它改。
    let mut guard = state.config.lock().unwrap();
    let mut cfg = guard.load().unwrap_or_default();
    // cron 先校验（错误时直接返回，不动配置）
    if let Some(cron) = &body.schedule_cron {
        if let Err(e) = crate::domain::scheduler::validate_cron(cron) {
            return Json(err(e.to_string()));
        }
    }
    let existing = body
        .id
        .as_deref()
        .and_then(|id| cfg.task_by_id(id).cloned());
    let id = existing
        .as_ref()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| new_id("k"));
    let target_id = body
        .target_id
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| existing.as_ref().map(|t| t.target_id.clone()))
        .or_else(|| cfg.primary_target().map(|t| t.id.clone()))
        .unwrap_or_else(|| crate::infra::config::DEFAULT_TARGET_ID.to_string());
    if cfg.target_by_id(&target_id).is_none() {
        return Json(err(format!("目标不存在：{target_id}")));
    }
    let task = crate::infra::config::TaskConfig {
        id: id.clone(),
        name: body
            .name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| existing.as_ref().map(|t| t.name.clone()))
            .unwrap_or_else(|| format!("任务 {id}")),
        enabled: body
            .enabled
            .unwrap_or_else(|| existing.as_ref().map(|t| t.enabled).unwrap_or(true)),
        paths: {
            let p = body
                .paths
                .clone()
                .or_else(|| existing.as_ref().map(|t| t.paths.clone()))
                .unwrap_or_default();
            // **新建任务时至少要有 1 个备份路径**。
            //
            // 旧实现直接 `unwrap_or_default()`：前端漏传（或直接调 API 传 `{}`）
            // 就会创建一个「没有任何路径」的任务 —— 它显示在列表里，
            // 跑起来却什么都没备份，用户很容易误以为「已配好」。
            // 编辑已有任务时允许保持原值（老配置可能就是空的，不该卡住保存）。
            if p.is_empty() && existing.is_none() {
                return Json(err("请至少添加一个要备份的文件夹"));
            }
            p
        },
        target_id,
        target_folder: body
            .target_folder
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| existing.as_ref().map(|t| t.target_folder.clone()))
            .unwrap_or_else(|| "fn-backup".to_string()),
        schedule_cron: match &body.schedule_cron {
            Some(cron) => {
                let cron = cron.trim().to_string();
                if cron.is_empty() {
                    None
                } else {
                    Some(cron)
                }
            }
            None => existing.as_ref().and_then(|t| t.schedule_cron.clone()),
        },
        retention: body
            .retention
            .as_ref()
            .map(crate::infra::config::RetentionConfig::from)
            .or_else(|| existing.as_ref().map(|t| t.retention.clone()))
            .unwrap_or_default(),
        // 严格模式：缺省保持原值（与 `paths`/`retention` 的「未传不改」语义一致）
        strict: body
            .strict
            .or_else(|| existing.as_ref().map(|t| t.strict))
            .unwrap_or(false),
    };
    match cfg.tasks.iter_mut().find(|t| t.id == id) {
        Some(slot) => *slot = task,
        None => cfg.tasks.push(task),
    }
    // 仍在锁内：save 与上面的 load/校验构成同一个原子步骤
    if let Err(e) = guard.save(&cfg) {
        return Json(err(format!("{:#}", e)));
    }
    let saved = cfg.task_by_id(&id).cloned().unwrap_or_default();
    // 放锁后再记审计/组响应：task_view 内部自己会取锁，这里必须已经释放
    drop(guard);
    state.audit.record(
        "task.save",
        format!(
            "保存任务「{}」：{} 个源，目标 {}，定时 {}",
            saved.name,
            saved.paths.len(),
            saved.target_id,
            saved.schedule_cron.clone().unwrap_or_else(|| "未启用".to_string())
        ),
        true,
        None,
    );
    // 软校验：路径不存在不阻断保存（外接盘可能暂时未挂载、网络盘可能掉线），
    // 但要把话说明白 —— 否则用户保存成功、等到定时任务跑了才看到「路径不存在」告警。
    let missing: Vec<String> = saved
        .paths
        .iter()
        .filter(|p| !std::path::Path::new(p.as_str()).exists())
        .cloned()
        .collect();
    let warnings: Vec<String> = if missing.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "这些路径当前不存在，备份时会失败并被跳过：{}",
            missing.join("、")
        )]
    };
    Json(serde_json::json!({
        "success": true,
        "task": task_view(&state, &saved),
        "warnings": warnings,
        "error": null
    }))
}

/// `POST /api/tasks/:id/delete`：删除任务（`purge=true` 同时删除它的快照记录）
pub(super) async fn task_delete(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: Option<Json<TaskDeleteRequest>>,
) -> Json<serde_json::Value> {
    let purge = body.map(|b| b.purge).unwrap_or(false);
    let mut cfg = { state.config.lock().unwrap().load().unwrap_or_default() };
    let before = cfg.tasks.len();
    cfg.tasks.retain(|t| t.id != id);
    if cfg.tasks.len() == before {
        return Json(err("任务不存在"));
    }
    if let Err(e) = state.config.lock().unwrap().save(&cfg) {
        return Json(err(format!("{:#}", e)));
    }
    let purged = if purge {
        // 快照 key = "{task_id}-{源序号}"：按前缀清理该任务的全部记录
        state.store.delete_task_snapshots(&id).unwrap_or(0)
    } else {
        0
    };
    // 事件总线的「最后一帧」也要跟着清：否则「概览 → 实时任务」会继续展示
    // 这个已经不存在的任务上次的结果（实测会让人误以为故障仍在）。
    let cleared = state
        .eventbus
        .clear_if_task(crate::eventbus::TaskKind::Backup, &id);
    state.audit.record(
        "task.delete",
        format!("删除任务 {id}（清理快照记录 {purged} 条）"),
        true,
        None,
    );
    Json(serde_json::json!({
        "success": true,
        "purged": purged,
        "live_cleared": cleared,
        "error": null
    }))
}

/// `POST /api/tasks/:id/run`：立即执行该任务
pub(super) async fn task_run(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<BackupResponse> {
    let resp = run_task_now(&state, &id).await;
    state.audit.record(
        "task.run",
        match (&resp.error, resp.skipped) {
            (Some(e), _) => format!("任务 {id} 备份失败：{e}"),
            (None, true) => format!("任务 {id} 备份跳过（已有备份在执行）"),
            (None, false) => format!(
                "任务 {id} 备份完成：上传 {} 个文件（{}）",
                resp.uploaded,
                human_bytes(resp.uploaded_bytes)
            ),
        },
        resp.error.is_none(),
        None,
    );
    Json(resp)
}

/// 备份运行标志的 RAII 守卫：离开作用域时复位（覆盖提前 return 与 panic 展开）
///
/// 同时维护 `running_task_id`：禁用插件时需要知道**具体哪个任务**在跑，
/// 才能保证不打断正在进行的备份。两者必须一起设置/清除，故合并在同一守卫里。
pub(super) struct BackupRunFlag<'a> {
    flag: &'a std::sync::atomic::AtomicBool,
    task_id: &'a std::sync::RwLock<Option<String>>,
    kind: &'a std::sync::RwLock<Option<crate::eventbus::TaskKind>>,
    cancel: &'a std::sync::atomic::AtomicBool,
    /// 事件总线：用于兜底补发终态（见 Drop）
    eventbus: &'a crate::eventbus::EventBus,
}

impl Drop for BackupRunFlag<'_> {
    fn drop(&mut self) {
        // **兜底发布终态**：备份若因取消、重试耗尽、panic 等路径离开，
        // 都不会走到正常的「发布 Completed/Failed」代码，事件总线会永远停在
        // `status=progress`。前端「实时任务」卡片于是永久显示「进行中」——
        // 即使 `busy` 已是 false（实测：备份收尾阶段结束后卡片仍转圈）。
        // 与恢复侧同一处理（`restore.rs` 的 RunFlag::drop）。
        self.eventbus.finalize_if_unfinished(
            crate::eventbus::TaskKind::Backup,
            "任务异常结束（未收到终态事件，请查看运行日志）",
        );
        *self.task_id.write().unwrap() = None;
        *self.kind.write().unwrap() = None;
        // **必须清零取消标志**：否则上一次取消会立刻终止下一次任务
        self.cancel.store(false, std::sync::atomic::Ordering::SeqCst);
        self.flag.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// 执行一次备份（兼容入口：跑**默认任务**）
pub async fn run_backup_now(state: &AppState) -> BackupResponse {
    let task_id = {
        let mgr = state.config.lock().unwrap();
        mgr.load().ok().and_then(|c| default_task_id(&c))
    };
    match task_id {
        Some(id) => run_task_now(state, &id).await,
        None => {
            raise_alert(
                state,
                crate::domain::alerts::AlertLevel::Warn,
                crate::domain::alerts::AlertSource::Backup,
                "备份未执行：尚未创建任何备份任务".to_string(),
            );
            BackupResponse {
                error: Some("尚未创建任何备份任务，请先在「任务管理」中新建".to_string()),
                ..Default::default()
            }
        }
    }
}

/// 执行**指定任务**的一次备份（可被 HTTP handler 与定时调度器复用）
///
/// 返回 BackupResponse（含 uploaded/deleted/orphan_removed/skipped/error）。
///
/// 快照隔离：`job_id = "{task.id}-{源序号}"`、`account = 目标任务凭据的用户名`，
/// 因此同一份源在不同任务/目标上互不干扰，各自增量。
///
/// **并发互斥**：所有任务共用 `AppState.backup_running`，同一时刻只允许一个备份执行
/// （避免 NAS 带宽/IO 争用）；已有备份在跑时立即返回 `skipped = true`。
pub async fn run_task_now(state: &AppState, task_id: &str) -> BackupResponse {
    // 0) 抢占运行标志（CAS）；失败说明已有备份在执行，直接跳过
    use std::sync::atomic::Ordering;
    if state
        .backup_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return BackupResponse {
            skipped: true,
            error: Some("已有备份任务正在执行，本次触发已跳过".to_string()),
            ..Default::default()
        };
    }
    let _running = BackupRunFlag {
        flag: &state.backup_running,
        task_id: &state.running_task_id,
        kind: &state.running_kind,
        cancel: &state.cancel_requested,
        eventbus: &state.eventbus,
    };
    // 记录本任务 id（在 CAS 成功之后、真正开跑之前）：
    // 插件启停接口据此保护「正在进行的任务」，不把它级联禁用。
    *state.running_task_id.write().unwrap() = Some(task_id.to_string());
    // 标记运行类型（取消接口据此区分备份/恢复）+ **清零上次的取消标志**
    *state.running_kind.write().unwrap() = Some(crate::eventbus::TaskKind::Backup);
    state
        .cancel_requested
        .store(false, std::sync::atomic::Ordering::SeqCst);

    // 1) 解析任务与它的目标（未就绪 → 明确提示，不静默失败）
    let ctx = match task_context(state, task_id) {
        Ok(c) => c,
        Err(e) => {
            raise_alert(
                state,
                crate::domain::alerts::AlertLevel::Warn,
                crate::domain::alerts::AlertSource::Config,
                format!("备份未执行：{e}"),
            );
            return BackupResponse {
                error: Some(e),
                ..Default::default()
            };
        }
    };
    let task_label = if ctx.task.name.is_empty() {
        ctx.task.id.clone()
    } else {
        ctx.task.name.clone()
    };
    if !ctx.ready {
        let msg = format!("任务「{task_label}」的目标未配置凭据，请在「目标管理」中完善后重试");
        raise_alert(
            state,
            crate::domain::alerts::AlertLevel::Warn,
            crate::domain::alerts::AlertSource::Config,
            msg.clone(),
        );
        return BackupResponse {
            error: Some(msg),
            ..Default::default()
        };
    }

    let paths: Vec<PathBuf> = ctx.task.paths.iter().map(PathBuf::from).collect();
    if paths.is_empty() {
        let msg = format!("任务「{task_label}」未配置备份路径");
        raise_alert(
            state,
            crate::domain::alerts::AlertLevel::Warn,
            crate::domain::alerts::AlertSource::Backup,
            msg.clone(),
        );
        return BackupResponse {
            error: Some(msg),
            ..Default::default()
        };
    }

    // 2) 保留策略：启用时构造 RetentionPolicy，备份完成后清理该目标上的孤儿文件
    let target_folder = ctx.task.target_folder.clone();
    let retention_cfg = ctx.task.retention.clone();
    let retention = if retention_cfg.enabled && retention_cfg.cleanup_unmanaged {
        let rt = crate::domain::retention::RetentionPolicy::new(ctx.target.clone(), &target_folder);
        Some(if retention_cfg.min_age_days > 0 {
            rt.with_min_age_secs(retention_cfg.min_age_days * 86400)
        } else {
            rt
        })
    } else {
        None
    };

    // 3) 执行多路径备份（job_id 前缀 = 任务 id → 每任务独立快照）
    let job_id = ctx.task.id.clone();
    let account = ctx.account.clone();
    //
    // 保留策略启用时才需要「兄弟任务」：共用同一「目标 + 目标文件夹」的其它任务，
    // 其文件与本任务在云端处在同一棵树里，必须一并算作受管理，否则会被误删。
    //
    // 读取配置失败时**不放弃备份**：备份本身与保留策略无关，只是本次清理
    // 无法拿到完整受管理集合 —— 此时**关掉保留策略**是最安全的降级
    // （宁可不清理，也不能把兄弟任务的文件当孤儿删掉）。
    let retention = if retention.is_some() {
        let siblings = {
            let mgr = state.config.lock().unwrap();
            mgr.load().ok().map(|cfg| {
                cfg.tasks_sharing_target_folder(&ctx.task.target_id, &target_folder, &ctx.task.id)
                    .into_iter()
                    .flat_map(crate::infra::config::AppConfig::job_roots_of)
                    .collect::<Vec<(String, String)>>()
            })
        };
        match siblings {
            Some(s) => Some((retention.expect("retention 已确认非空"), s)),
            None => {
                tracing::warn!(
                    task = %ctx.task.id,
                    "读取配置失败：本次备份跳过保留策略清理（受管理集合不完整，清理会误删）"
                );
                None
            }
        }
    } else {
        None
    };
    let (retention, sibling_jobs) = match retention {
        Some((rt, s)) => (Some(rt), s),
        None => (None, Vec::new()),
    };
    let job = BackupJob {
        job_id: job_id.clone(),
        account: account.clone(),
        source: Arc::new(crate::infra::source::local::LocalFsSource::new(&paths[0])),
        target: ctx.target.clone(),
        crypto: state.crypto.get(),
        store: state.store.clone(),
        target_prefix: Some(target_folder),
        eventbus: Some(state.eventbus.clone()),
        retention,
        sibling_jobs,
        strict: ctx.task.strict,
        // 密文暂存目录：`$TRIM_PKGTMP/staging`（流式加密大文件的落点，见 backup.rs）
        staging_dir: Some(state.backup_tmp.clone()),
        // 取消标志：用户在界面上点「取消」时，任务在**下一个文件边界**停下
        cancel: Some(state.cancel_requested.clone()),
    };
    tracing::info!(task = %task_label, target = %ctx.task.target_id, paths = paths.len(), "开始执行备份任务");
    match job.run_multi(&paths).await {
        Ok(summary) => {
            // 保留策略可选：跟随清空云端回收站（由增强插件实现，如 kzwr）
            let trash_emptied = if retention_cfg.empty_recycle_bin {
                let mut n = 0u64;
                for p in state.plugins.enhance_plugins() {
                    if let Some(c) = p.after_backup(state, &ctx.task.id).await {
                        n += c;
                    }
                }
                n as usize
            } else {
                0
            };
            // 备份后云端占用会变化：异步触发一次插件巡检（空间预警，不阻塞本次响应）
            {
                let quota_state = state.clone();
                tokio::spawn(async move {
                    for p in quota_state.plugins.enhance_plugins() {
                        p.patrol(&quota_state).await;
                    }
                });
            }
            tracing::info!(
                task = %task_label,
                uploaded = summary.uploaded,
                deleted = summary.deleted,
                "备份任务完成"
            );
            BackupResponse {
                uploaded: summary.uploaded,
                uploaded_bytes: summary.uploaded_bytes,
                deleted: summary.deleted,
                unchanged: summary.unchanged,
                orphan_removed: summary.orphan_removed,
                trash_emptied,
                ..Default::default()
            }
        }
        Err(e) => {
            let msg = format!("{:#}", e);
            raise_alert(
                state,
                crate::domain::alerts::AlertLevel::Error,
                crate::domain::alerts::AlertSource::Backup,
                format!("备份失败（任务 {task_label}）：{msg}"),
            );
            // 发布 Failed 终态事件：否则前端任务面板停留在「进行中」永不结束
            state.eventbus.task_event(
                crate::eventbus::TaskKind::Backup,
                crate::eventbus::TaskStatus::Failed,
                None,
                job_id,
                None,
                0,
                0,
                0,
                0,
                0,
                0,
                Some(msg.clone()),
            );
            BackupResponse {
                error: Some(msg),
                ..Default::default()
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 实时任务：查询当前状态 / 取消
// ════════════════════════════════════════════════════════════════════════════

/// `GET /api/tasks/live`：当前任务状态快照
///
/// 前端刷新页面或**退出应用再进来**（WebView 重建、WS 重连）时，之前通过 WS
/// 收到的事件全部丢失，界面会显示「没有任务在跑」，而后台其实还在跑。
/// 事件总线保留了最后一帧，这里把它一并返回，前端首屏即可还原进度。
pub(super) async fn tasks_live(State(state): State<AppState>) -> Json<serde_json::Value> {
    let running = state
        .running_kind
        .read()
        .map(|g| g.clone())
        .unwrap_or(None);
    let backup = state.eventbus.last_snapshot(crate::eventbus::TaskKind::Backup);
    let restore = state
        .eventbus
        .last_snapshot(crate::eventbus::TaskKind::Restore);
    Json(serde_json::json!({
        "busy": state.backup_running.load(std::sync::atomic::Ordering::SeqCst)
            || running.is_some(),
        "running_kind": running,
        "running_task_id": state.running_task_id.read().map(|g| g.clone()).unwrap_or(None),
        "cancel_requested": state
            .cancel_requested
            .load(std::sync::atomic::Ordering::SeqCst),
        "backup": backup.as_ref().map(|e| live_view(e)),
        "restore": restore.as_ref().map(|e| live_view(e)),
    }))
}

/// 把最后一帧事件转成前端可直接渲染的形状（与 WS 的 `event` 同构）
fn live_view(e: &crate::eventbus::DomainEvent) -> serde_json::Value {
    serde_json::json!({
        "type": "event",
        "kind": e.kind,
        "status": e.status,
        "phase": e.phase,
        "job_id": e.job_id,
        "current_file": e.current_file,
        "done": e.done,
        "total": e.total,
        "bytes_done": e.bytes_done,
        "bytes_total": e.bytes_total,
        "elapsed_ms": e.elapsed_ms,
        "speed": e.speed,
        "message": e.message,
        "ts": e.ts,
    })
}

/// `POST /api/tasks/cancel`：请求取消当前正在执行的任务
///
/// ## 取消是「协作式」的，不是强杀
/// 置位标志后，任务在**下一个文件边界**停止。原因：
/// - 单文件的传输在插件 FFI 里推进（`write_chunk` / `read_chunk` 循环），
///   同进程模型下无法从外部打断（否则会留下半截分片）；
/// - 文件边界停下是安全的：已完成的文件快照已落盘，下次备份/恢复接着处理未完成的。
///
/// 因此接口返回 `accepted`，并说明「将在当前文件结束后停止」，而不是假装已终止。
pub(super) async fn tasks_cancel(State(state): State<AppState>) -> Json<serde_json::Value> {
    let busy = state.backup_running.load(std::sync::atomic::Ordering::SeqCst)
        || state.running_kind.read().map(|g| g.is_some()).unwrap_or(false);
    if !busy {
        return Json(serde_json::json!({
            "success": false,
            "error": Some("当前没有正在执行的任务".to_string()),
        }));
    }
    if state
        .cancel_requested
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Json(serde_json::json!({
            "success": true,
            "already": true,
            "message": "取消请求已提交，正在等待当前文件结束",
        }));
    }
    state
        .cancel_requested
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let kind = state.running_kind.read().map(|g| g.clone()).unwrap_or(None);
    let what = match kind {
        Some(crate::eventbus::TaskKind::Backup) => "备份",
        Some(crate::eventbus::TaskKind::Restore) => "恢复",
        _ => "任务",
    };
    tracing::info!(kind = ?kind, "收到取消{}请求", what);
    state.audit.record("task.cancel", format!("请求取消{what}任务"), true, None);
    Json(serde_json::json!({
        "success": true,
        "message": format!("已请求取消{what}：将在当前文件传输结束后停止（已完成的文件不会回退）"),
    }))
}
