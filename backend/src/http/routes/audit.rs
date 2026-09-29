//! 审计日志（查询 / 清空）+ 一键体检 + 定时预览

use age::secrecy::ExposeSecret;
use crate::infra::storage_trait::TargetStorage;
use axum::extract::State;
use axum::response::Json;

use crate::AppState;

use super::types::*;

pub(super) async fn audit_clear(
    State(state): State<AppState>,
    body: Option<Json<AuditClearRequest>>,
) -> Json<AuditClearResponse> {
    // 破坏性操作：需管理员口令校验（与导出私钥/配置导入导出同口径）
    let passphrase = body
        .and_then(|Json(b)| Some(b.passphrase))
        .unwrap_or_default();
    if passphrase.as_str() != state.passphrase.expose_secret().as_str() {
        return Json(AuditClearResponse {
            cleared: 0,
            error: Some("管理员口令错误".to_string()),
        });
    }
    let cleared = state.audit.clear();
    state.audit.record(
        "audit.clear",
        format!("清空审计日志（{cleared} 条，管理员口令校验通过）"),
        true,
        None,
    );
    Json(AuditClearResponse {
        cleared,
        error: None,
    })
}

/// 定时任务预览：校验 cron 并给出未来 5 次触发时间（服务器本地时区）
pub(super) async fn schedule_preview(
    Json(body): Json<SchedulePreviewRequest>,
) -> Json<SchedulePreviewResponse> {
    let tz = crate::domain::scheduler::timezone_label();
    let cron = body.cron.trim();
    if cron.is_empty() {
        return Json(SchedulePreviewResponse {
            valid: true,
            next: Vec::new(),
            timezone: tz,
            error: None,
        });
    }
    match crate::domain::scheduler::next_runs(cron, 5) {
        Ok(next) => Json(SchedulePreviewResponse {
            valid: true,
            next,
            timezone: tz,
            error: None,
        }),
        Err(e) => Json(SchedulePreviewResponse {
            valid: false,
            next: Vec::new(),
            timezone: tz,
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 一键体检：逐项检查配置与连通性，给出可操作建议
pub(super) async fn setup_check(State(state): State<AppState>) -> Json<SetupCheckResponse> {
    let mut items: Vec<CheckItem> = Vec::new();

    // 1) 服务
    items.push(CheckItem {
        key: "service".to_string(),
        title: "服务运行状态".to_string(),
        status: "ok".to_string(),
        detail: format!("版本 v{}", env!("CARGO_PKG_VERSION")),
        hint: None,
    });

    // 2) 目标配置 + 实连（多目标：逐个检查）
    let (cfg, target_creds) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        let mut map = std::collections::HashMap::new();
        for t in &cfg.targets {
            map.insert(t.id.clone(), mgr.target_credentials(t).unwrap_or((None, None)));
        }
        (cfg, map)
    };
    if cfg.targets.iter().all(|t| !t.enabled) {
        items.push(CheckItem {
            key: "webdav".to_string(),
            title: "备份目标".to_string(),
            status: "fail".to_string(),
            detail: "尚未启用任何备份目标，备份与恢复不可用".to_string(),
            hint: Some(
                "前往「目标管理」新增目标并填写账号与应用密码；应用密码请在 https://www.kzwr.com/account/apps 创建，选择「永不过期」与读写权限"
                    .to_string(),
            ),
        });
    } else {
        let mut ok_list: Vec<String> = Vec::new();
        let mut fail_list: Vec<String> = Vec::new();
        for t in cfg.targets.iter().filter(|t| t.enabled) {
            let label = if t.name.is_empty() {
                t.id.clone()
            } else {
                t.name.clone()
            };
            let (user, pass) = target_creds.get(&t.id).cloned().unwrap_or((None, None));
            match (user, pass) {
                (Some(u), Some(p)) => {
                    let url = t
                        .url
                        .clone()
                        .unwrap_or_else(|| crate::infra::target::webdav::DEFAULT_URL.to_string());
                    match crate::infra::target::webdav::WebdavTarget::new(&url, &u, &p)
                        .ping()
                        .await
                    {
                        Ok(_) => ok_list.push(format!("{label}（{url}）")),
                        Err(e) => fail_list.push(format!("{label}：{e:#}")),
                    }
                }
                _ => fail_list.push(format!("{label}：未配置凭据")),
            }
        }
        let (status, detail, hint) = if fail_list.is_empty() {
            (
                "ok",
                format!("已连接 {} 个目标：{}", ok_list.len(), ok_list.join("、")),
                None,
            )
        } else if ok_list.is_empty() {
            (
                "fail",
                format!("所有目标均不可用：{}", fail_list.join("；")),
                Some("请确认应用密码未过期且有读写权限（可在 https://www.kzwr.com/account/apps 重新创建）".to_string()),
            )
        } else {
            (
                "warn",
                format!(
                    "{} 个目标可用；{} 个异常：{}",
                    ok_list.len(),
                    fail_list.len(),
                    fail_list.join("；")
                ),
                Some("前往「目标管理」逐个测试并修复异常目标".to_string()),
            )
        };
        items.push(CheckItem {
            key: "webdav".to_string(),
            title: "备份目标".to_string(),
            status: status.to_string(),
            detail,
            hint,
        });
    }

    // 3) 备份路径 + 已备份文件数（多任务：按任务各自的目标账号统计）
    let total_paths: usize = cfg.tasks.iter().map(|t| t.paths.len()).sum();
    if total_paths == 0 {
        items.push(CheckItem {
            key: "paths".to_string(),
            title: "备份任务".to_string(),
            status: "fail".to_string(),
            detail: "尚未添加任何备份任务/路径".to_string(),
            hint: Some("前往「任务管理」新建任务并添加要备份的文件夹".to_string()),
        });
    } else {
        let mut files = 0usize;
        let mut tasks_with_backup = 0usize;
        for task in &cfg.tasks {
            let account = target_creds
                .get(&task.target_id)
                .and_then(|(u, _)| u.clone())
                .unwrap_or_default();
            let mut task_files = 0usize;
            for i in 0..task.paths.len() {
                let job_id = format!("{}-{}", task.id, i);
                if let Ok(entries) = state.store.load_snapshot(&job_id, &account) {
                    task_files += entries.iter().filter(|e| !e.is_dir).count();
                }
            }
            files += task_files;
            if task_files > 0 {
                tasks_with_backup += 1;
            }
        }
        items.push(CheckItem {
            key: "paths".to_string(),
            title: "备份任务".to_string(),
            status: if files > 0 { "ok" } else { "warn" }.to_string(),
            detail: if files > 0 {
                format!(
                    "{} 个任务 / {} 个路径；其中 {} 个任务已备份，共 {} 个文件",
                    cfg.tasks.len(),
                    total_paths,
                    tasks_with_backup,
                    files
                )
            } else {
                format!("{} 个任务 / {} 个路径，但还没有备份记录", cfg.tasks.len(), total_paths)
            },
            hint: if files > 0 {
                None
            } else {
                Some("前往「任务管理」执行一次备份".to_string())
            },
        });
    }

    // 4) 私钥备份确认
    items.push(CheckItem {
        key: "key".to_string(),
        title: "私钥备份".to_string(),
        status: if cfg.keys.backed_up { "ok" } else { "warn" }.to_string(),
        detail: if cfg.keys.backed_up {
            "已确认妥善保存 age 私钥".to_string()
        } else {
            "尚未确认私钥已备份；私钥丢失将无法恢复数据".to_string()
        },
        hint: if cfg.keys.backed_up {
            None
        } else {
            Some("前往「设置 → 加密密钥」导出私钥并确认已保存".to_string())
        },
    });

    // 5) 定时备份（多任务：汇总各任务启用的 cron）
    let scheduled: Vec<(String, String)> = cfg
        .tasks
        .iter()
        .filter(|t| t.enabled)
        .filter_map(|t| {
            let cron = t.schedule_cron.clone().unwrap_or_default();
            let cron = cron.trim().to_string();
            if cron.is_empty() {
                None
            } else {
                Some((
                    if t.name.is_empty() {
                        t.id.clone()
                    } else {
                        t.name.clone()
                    },
                    cron,
                ))
            }
        })
        .collect();
    if scheduled.is_empty() {
        items.push(CheckItem {
            key: "schedule".to_string(),
            title: "定时备份".to_string(),
            status: "warn".to_string(),
            detail: "所有任务均未启用定时（仅手动备份）".to_string(),
            hint: Some("如需无人值守，可在「任务管理」为任务设置 cron 表达式".to_string()),
        });
    } else {
        let detail = scheduled
            .iter()
            .map(|(name, cron)| {
                let next = crate::domain::scheduler::next_runs(cron, 1)
                    .unwrap_or_default()
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "—".to_string());
                format!("{name}: {cron}（下次 {next}）")
            })
            .collect::<Vec<_>>()
            .join("；");
        items.push(CheckItem {
            key: "schedule".to_string(),
            title: "定时备份".to_string(),
            status: "ok".to_string(),
            detail: format!(
                "{} 个任务已启用（{}）；{}",
                scheduled.len(),
                crate::domain::scheduler::timezone_label(),
                detail
            ),
            hint: None,
        });
    }

    // 6) 增强插件自检（如 kzwr：access-token 与云端空间）——检查项由插件自己产出
    for p in state.plugins.enhance_plugins() {
        for o in p.health_check(&state, &cfg).await {
            items.push(CheckItem {
                key: o.key,
                title: o.title,
                status: o.status,
                detail: o.detail,
                hint: o.hint,
            });
        }
    }

    let ok_count = items.iter().filter(|i| i.status == "ok").count();
    let warn_count = items.iter().filter(|i| i.status == "warn").count();
    let fail_count = items.iter().filter(|i| i.status == "fail").count();
    Json(SetupCheckResponse {
        items,
        ok_count,
        warn_count,
        fail_count,
        version: env!("CARGO_PKG_VERSION").to_string(),
        error: None,
    })
}

/// 审计日志查询（最新在前）
pub(super) async fn audit_list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<AuditQuery>,
) -> Json<AuditResponse> {
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    Json(AuditResponse {
        entries: state.audit.recent(limit),
        error: None,
    })
}
