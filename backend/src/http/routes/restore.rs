//! 恢复（文件列表 / 目录树 / 执行 / 清理缺失）
//!
//! 恢复要跨任务定位「这份源路径属于哪个目标」，故有 `find_restore_scope`
//! 与快照聚合（`SnapshotAgg`）这两块辅助。

use crate::infra::storage_trait::TargetStorage;
use axum::extract::State;
use std::sync::Arc;
use axum::response::Json;

use crate::domain::restore::RestoreJob;
use crate::AppState;

use super::common::{human_bytes, raise_alert};
use super::config::webdav_ready;
use super::types::*;

/// 快照聚合索引：**一次遍历**构建全部前缀的统计，之后 O(1) 查询
///
/// 原实现（`snapshot_aggregate`）对每个目录节点全量扫描一遍快照，
/// 树接口整体 O(条目数 × 节点数)；大快照下是响应慢的主因之一。
/// 这里一趟完成：
/// - 文件条目：把 (1, size) 累加到**各级祖先前缀**（含根 ""）；
/// - 目录宇宙（目录条目 + 文件的各级祖先）：为每个目录向其**所有真前缀**（含根）
///   计 1 —— 与原实现「BTreeSet 去重后数集合」语义一致；
/// - 文件大小表：文件路径 → 明文字节（树接口文件节点 O(1) 取大小）。
pub(super) struct SnapshotAgg {
    /// 前缀（"" 表示根）→ (递归文件数, 递归明文字节)
    files: std::collections::HashMap<String, (usize, u64)>,
    /// 前缀（"" 表示根）→ 严格位于该前缀下的目录数
    dirs: std::collections::HashMap<String, usize>,
    /// 文件相对路径 → 明文大小
    sizes: std::collections::HashMap<String, u64>,
}

impl SnapshotAgg {
    fn build(
        entries: &[crate::infra::persistence::snapshot::SnapshotEntry],
    ) -> Self {
        let mut files: std::collections::HashMap<String, (usize, u64)> =
            std::collections::HashMap::new();
        let mut dir_universe: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut sizes: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

        let push_dir = |universe: &mut std::collections::HashSet<String>, acc: &mut String, p: &str| {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(p);
            universe.insert(acc.clone());
        };

        for e in entries {
            let parts: Vec<&str> = e.rel_path.split('/').collect();
            if e.is_dir {
                let mut acc = String::new();
                for p in &parts {
                    push_dir(&mut dir_universe, &mut acc, p);
                }
            } else {
                sizes.insert(e.rel_path.clone(), e.size);
                // 各级祖先（i = 0..len-1；i=0 即根 ""）
                let mut acc = String::new();
                for p in &parts[..parts.len().saturating_sub(1)] {
                    let v = files.entry(acc.clone()).or_insert((0, 0));
                    v.0 += 1;
                    v.1 += e.size;
                    push_dir(&mut dir_universe, &mut acc, p);
                }
                let v = files.entry(acc.clone()).or_insert((0, 0));
                v.0 += 1;
                v.1 += e.size;
            }
        }

        // 每个目录向其所有真前缀（含根 ""）计 1
        let mut dirs: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for d in &dir_universe {
            let parts: Vec<&str> = d.split('/').collect();
            let mut acc = String::new();
            for p in &parts[..parts.len() - 1] {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(p);
                *dirs.entry(acc.clone()).or_insert(0) += 1;
            }
            *dirs.entry(String::new()).or_insert(0) += 1;
        }

        Self {
            files,
            dirs,
            sizes,
        }
    }

    /// 查询某前缀下的 (递归文件数, 递归子目录数, 递归明文字节)
    fn get(&self, prefix: &str) -> (usize, usize, u64) {
        let p = prefix.trim_matches('/');
        let (f, b) = self.files.get(p).copied().unwrap_or((0, 0));
        let d = self.dirs.get(p).copied().unwrap_or(0);
        (f, d, b)
    }

    /// 文件相对路径 → 明文大小（未知为 0）
    fn size_of(&self, rel_path: &str) -> u64 {
        self.sizes.get(rel_path).copied().unwrap_or(0)
    }
}

/// 列出某前缀下的直接子项（名称 + 是否目录），目录优先、其余按名称排序
pub(super) fn snapshot_children(
    entries: &[crate::infra::persistence::snapshot::SnapshotEntry],
    prefix: &str,
) -> Vec<(String, bool)> {
    let mut map: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
    for e in entries {
        let Some(rest) = e.rel_path.strip_prefix(prefix) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let (name, is_dir) = match rest.split_once('/') {
            Some((head, _)) => (head.to_string(), true),
            None => (rest.to_string(), e.is_dir),
        };
        map.entry(name).and_modify(|d| *d = *d || is_dir).or_insert(is_dir);
    }
    let mut out: Vec<(String, bool)> = map.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// 恢复范围：把「源路径」定位到某个任务内的某个源（多任务下同一路径可能出现在多个任务）
pub(super) struct RestoreScope {
    task: crate::infra::config::TaskConfig,
    job_id: String,
    /// 快照分桶账号 = 该任务目标的用户名（未配置时 ''，兼容旧数据）
    account: String,
    target: Arc<dyn crate::infra::storage_trait::TargetStorage>,
    target_folder: String,
    ready: bool,
}

/// 定位「源路径 → 恢复范围」
///
/// 匹配顺序：指定任务（`task_id`）→ 按启用任务遍历 → 按全部任务遍历；
/// 同一任务内先精确匹配路径、再按目录名匹配。
pub(super) fn find_restore_scope(
    state: &AppState,
    source: &str,
    task_id: Option<&str>,
) -> Result<RestoreScope, String> {
    let (cfg, creds_by_target) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().map_err(|e| format!("{:#}", e))?;
        let mut creds = std::collections::HashMap::new();
        for t in &cfg.targets {
            let user = mgr
                .target_credentials(t)
                .ok()
                .and_then(|(u, _)| u)
                .unwrap_or_default();
            creds.insert(t.id.clone(), user);
        }
        (cfg, creds)
    };

    let root = std::path::Path::new(source);
    let match_idx = |t: &crate::infra::config::TaskConfig| -> Option<usize> {
        t.paths
            .iter()
            .position(|p| std::path::Path::new(p) == root)
            .or_else(|| {
                root.file_name()
                    .and_then(|n| t.paths.iter().position(|p| std::path::Path::new(p).file_name() == Some(n)))
            })
    };

    let candidates: Vec<&crate::infra::config::TaskConfig> = match task_id {
        Some(id) => cfg
            .tasks
            .iter()
            .filter(|t| t.id == id)
            .collect(),
        None => cfg
            .tasks
            .iter()
            .filter(|t| t.enabled)
            .chain(cfg.tasks.iter().filter(|t| !t.enabled))
            .collect(),
    };
    let task = candidates
        .into_iter()
        .find(|t| match_idx(t).is_some())
        .ok_or_else(|| "该路径不在任何备份任务的源列表中".to_string())?;
    let idx = match_idx(task).expect("find 已保证命中");
    tracing::debug!(task = %task.id, idx, source, "定位恢复范围");
    Ok(RestoreScope {
        job_id: format!("{}-{}", task.id, idx),
        account: creds_by_target
            .get(&task.target_id)
            .cloned()
            .unwrap_or_default(),
        target: state.target_for(&task.target_id),
        target_folder: task.target_folder.clone(),
        ready: state.targets.is_ready(&task.target_id),
        task: task.clone(),
    })
}

/// 按备份源路径取出其快照条目（多任务：可传 `task_id` 指定任务）
pub(super) fn snapshot_entries_for_source(
    state: &AppState,
    source: &str,
    task_id: Option<&str>,
) -> Result<Vec<crate::infra::persistence::snapshot::SnapshotEntry>, String> {
    let scope = find_restore_scope(state, source, task_id)?;
    state
        .store
        .load_snapshot(&scope.job_id, &scope.account)
        .map_err(|e| format!("读取备份快照失败: {:#}", e))
}

/// 列出全部任务的全部源文件夹及其可恢复概况（计数来自 SQLite 快照，明细按需懒加载）
pub(super) async fn restore_files(State(state): State<AppState>) -> Json<RestoreFilesResponse> {
    let cfg = { state.config.lock().unwrap().load().unwrap_or_default() };
    let mut folders = Vec::new();

    for task in &cfg.tasks {
        let account = {
            let mgr = state.config.lock().unwrap();
            cfg.target_by_id(&task.target_id)
                .and_then(|t| mgr.target_credentials(t).ok())
                .and_then(|(u, _)| u)
                .unwrap_or_default()
        };
        for (i, path) in task.paths.iter().enumerate() {
            // 每个任务、每个源的快照 key = "{task.id}-{i}"
            let job_id = format!("{}-{}", task.id, i);
            let entries = state.store.load_snapshot(&job_id, &account).unwrap_or_default();
            let agg = SnapshotAgg::build(&entries);
            let (file_count, dir_count, total_bytes) = agg.get("");
            folders.push(RestorableFolder {
                path: path.clone(),
                has_backup: file_count > 0,
                file_count,
                dir_count,
                total_bytes,
                task_id: task.id.clone(),
                task_name: if task.name.is_empty() {
                    task.id.clone()
                } else {
                    task.name.clone()
                },
                target_name: cfg
                    .target_by_id(&task.target_id)
                    .map(|t| t.name.clone())
                    .unwrap_or_default(),
            });
        }
    }

    Json(RestoreFilesResponse { folders, error: None })
}

/// 按目录懒加载恢复树：只返回指定目录的**直接子项**，目录附递归统计
///
/// 前端展开某个文件夹/子目录时才调用，避免一次性下发整棵树（大备份下可达数万条）。
pub(super) async fn restore_tree(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<RestoreTreeQuery>,
) -> Json<RestoreTreeResponse> {
    let entries = match snapshot_entries_for_source(&state, &q.source, q.task.as_deref()) {
        Ok(e) => e,
        Err(msg) => {
            return Json(RestoreTreeResponse {
                nodes: Vec::new(),
                error: Some(msg),
            })
        }
    };

    let dir = q.dir.trim_matches('/').to_string();
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{}/", dir)
    };

    // 一次遍历建索引：目录统计与文件大小查询均为 O(1)
    let agg = SnapshotAgg::build(&entries);

    let mut nodes = Vec::new();
    for (name, is_dir) in snapshot_children(&entries, &prefix) {
        let rel_path = if dir.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", dir, name)
        };
        if is_dir {
            let (file_count, dir_count, total_bytes) = agg.get(&rel_path);
            nodes.push(RestoreNode {
                name,
                rel_path,
                is_dir: true,
                size: 0,
                file_count,
                dir_count,
                total_bytes,
            });
        } else {
            let size = agg.size_of(&rel_path);
            nodes.push(RestoreNode {
                name,
                rel_path,
                is_dir: false,
                size,
                file_count: 0,
                dir_count: 0,
                total_bytes: 0,
            });
        }
    }

    Json(RestoreTreeResponse { nodes, error: None })
}

/// 触发恢复
pub(super) async fn restore_run(
    State(state): State<AppState>,
    body: Option<axum::extract::Json<RestoreRequest>>,
) -> Json<RestoreResponse> {
    // 恢复目标根：优先用前端传入的 source_path（备份源路径，恢复到原位置），否则用默认目录
    let (mut files, restore_root, restore_all, restore_dir, task_hint) = match body {
        Some(Json(req)) => (
            req.files.unwrap_or_default(),
            req.source_path
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| state.default_restore_dir.to_string_lossy().into_owned()),
            req.all,
            req.dir
                .map(|d| d.trim_matches('/').to_string())
                .filter(|d| !d.is_empty()),
            req.task.clone(),
        ),
        None => (
            Vec::new(),
            state.default_restore_dir.to_string_lossy().into_owned(),
            false,
            None,
            None,
        ),
    };

    // 备份时每个源目录在目标端以其文件夹名分目录存放，恢复需还原该层级
    let source_root_name = std::path::Path::new(&restore_root)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned());

    // 定位该源路径所属的任务与目标（多任务：可指定 task_id）
    let scope = find_restore_scope(&state, &restore_root, task_hint.as_deref()).ok();

    // 目标就绪校验：按实际命中的任务目标判断（多目标）；未命中任务时退回主目标
    let ready = match &scope {
        Some(s) => s.ready,
        None => {
            state.primary_ready()
                || {
                    let mgr = state.config.lock().unwrap();
                    mgr.load().map(|c| webdav_ready(&c)).unwrap_or(false)
                }
        }
    };
    if !ready {
        raise_alert(
            &state,
            crate::domain::alerts::AlertLevel::Warn,
            crate::domain::alerts::AlertSource::Config,
            "恢复未执行：目标未配置".to_string(),
        );
        return Json(RestoreResponse {
            restored: 0,
            restored_bytes: 0,
            error: Some("目标未配置，请先在「目标管理」中填写地址与凭据".to_string()),
            missing: Vec::new(),
        });
    }

    // 读取快照元数据：rel_path -> (明文大小, 原始 mtime 秒)
    // 用途：① 展示恢复总大小；② 恢复后回写快照，避免下次备份重复上传
    let mut meta: std::collections::HashMap<String, (u64, i64)> =
        std::collections::HashMap::new();
    let mut snapshot_target = None;
    if let Some(scope) = &scope {
        if let Ok(entries) = state.store.load_snapshot(&scope.job_id, &scope.account) {
            for e in entries {
                if !e.is_dir {
                    meta.insert(e.rel_path.clone(), (e.size, e.mtime_secs));
                }
            }
        }
        snapshot_target = Some(crate::domain::restore::RestoreSnapshotTarget {
            store: state.store.clone(),
            job_id: scope.job_id.clone(),
            account: scope.account.clone(),
        });
    }

    // 「全部恢复」：忽略前端传入的文件列表，取该源路径（可限定子目录）快照中的全部文件
    if restore_all {
        if scope.is_none() {
            return Json(RestoreResponse {
                restored: 0,
                restored_bytes: 0,
                error: Some("未找到该路径的备份快照，请先执行一次备份".to_string()),
                missing: Vec::new(),
            });
        }
        let prefix = restore_dir.as_ref().map(|d| format!("{}/", d));
        files = meta
            .keys()
            .filter(|p| match &prefix {
                Some(pfx) => p.starts_with(pfx.as_str()),
                None => true,
            })
            .cloned()
            .collect();
        files.sort();
        if files.is_empty() {
            return Json(RestoreResponse {
                restored: 0,
                restored_bytes: 0,
                error: Some(match &restore_dir {
                    Some(d) => format!("目录 {d} 下暂无可恢复的文件"),
                    None => "该路径暂无可恢复的文件".to_string(),
                }),
                missing: Vec::new(),
            });
        }
        tracing::info!(
            "全部恢复：源 {}，目录 {}，共 {} 个文件",
            restore_root,
            restore_dir.as_deref().unwrap_or("(根)"),
            files.len()
        );
    }

    // 目标与目标端前缀按命中的任务取（未命中任务时退回主目标/默认前缀）
    let (restore_target, restore_prefix): (
        Arc<dyn crate::infra::storage_trait::TargetStorage>,
        String,
    ) = match &scope {
        Some(s) => (s.target.clone(), s.target_folder.clone()),
        None => (state.target.clone(), state.target_folder.clone()),
    };
    let job = RestoreJob {
        target: restore_target,
        crypto: state.crypto.get(),
        target_prefix: Some(restore_prefix),
        source_root_name,
        eventbus: Some(state.eventbus.clone()),
        meta,
        snapshot_target,
    };
    match job.run(&files, std::path::Path::new(&restore_root)).await {
        Ok(summary) => {
            let task_label = scope
                .as_ref()
                .map(|s| {
                    if s.task.name.is_empty() {
                        s.task.id.clone()
                    } else {
                        s.task.name.clone()
                    }
                })
                .unwrap_or_else(|| "(未归属任务)".to_string());
            state.audit.record(
                "restore.run",
                format!(
                    "恢复到 {}（任务 {}）：{} 个文件（{}）",
                    restore_root,
                    task_label,
                    summary.restored,
                    human_bytes(summary.restored_bytes)
                ),
                true,
                None,
            );
            // 云端缺失的文件截断到 200 条，避免超大响应
            let mut missing = summary.missing;
            missing.truncate(200);
            Json(RestoreResponse {
                restored: summary.restored,
                restored_bytes: summary.restored_bytes,
                error: None,
                missing,
            })
        }
        Err(e) => {
            let msg = format!("{:#}", e);
            raise_alert(
                &state,
                crate::domain::alerts::AlertLevel::Error,
                crate::domain::alerts::AlertSource::Restore,
                format!("恢复失败：{}", msg),
            );
            // 发布 Failed 终态事件：否则前端任务面板停留在「进行中」永不结束
            state.eventbus.task_event(
                crate::eventbus::TaskKind::Restore,
                crate::eventbus::TaskStatus::Failed,
                None,
                "restore".to_string(),
                None,
                0,
                0,
                0,
                0,
                0,
                0,
                Some(msg.clone()),
            );
            Json(RestoreResponse {
                restored: 0,
                restored_bytes: 0,
                error: Some(msg),
                missing: Vec::new(),
            })
        }
    }
}

/// 清理快照中云端已不存在的文件记录
///
/// 做法：递归列出目标端 `/<target_folder>/<源文件夹名>` 下的实际文件，
/// 与快照对比；快照里有、云端没有的记录即为失效，逐条从快照删除。
/// **只动快照元数据，不删云端任何文件。**
pub(super) async fn restore_prune(
    State(state): State<AppState>,
    Json(body): Json<PruneMissingRequest>,
) -> Json<PruneMissingResponse> {
    fn resp(checked: usize, removed: usize, files: Vec<String>, error: Option<String>) -> PruneMissingResponse {
        PruneMissingResponse {
            checked,
            removed,
            files,
            error,
        }
    }
    fn err(e: impl std::fmt::Display) -> PruneMissingResponse {
        resp(0, 0, Vec::new(), Some(e.to_string()))
    }

    // 定位该源路径所属的任务与目标（多任务：可指定 task_id）
    let scope = match find_restore_scope(&state, &body.source_path, body.task.as_deref()) {
        Ok(s) => s,
        Err(e) => return Json(err(e)),
    };
    if !scope.ready {
        return Json(err("该任务的目标未配置凭据，请先在「目标管理」中完善"));
    }
    let root = std::path::Path::new(&body.source_path);
    let target_folder = scope.target_folder.clone();
    let job_id = scope.job_id.clone();
    let account = scope.account.clone();
    let entries = match state.store.load_snapshot(&job_id, &account) {
        Ok(e) => e,
        Err(e) => return Json(err(format!("读取备份快照失败: {e:#}"))),
    };
    let snapshot_files: Vec<String> = entries
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| e.rel_path.clone())
        .collect();
    if snapshot_files.is_empty() {
        return Json(resp(0, 0, Vec::new(), None));
    }

    // 目标端基路径：/<target_folder>[/<源文件夹名>]（与备份/恢复的布局一致）
    let source_root_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut segs: Vec<String> = Vec::new();
    let tf = target_folder.trim_matches('/');
    if !tf.is_empty() {
        segs.push(tf.to_string());
    }
    let rn = source_root_name.trim_matches('/');
    if !rn.is_empty() {
        segs.push(rn.to_string());
    }
    if segs.is_empty() {
        return Json(err("目标目录为空，无法清理"));
    }
    let base = format!("/{}", segs.join("/"));
    let base_prefix = format!("{}/", base.trim_matches('/'));

    // 递归列出云端实际文件（显式栈遍历；分片文件由 list 合并为逻辑条目）
    let mut cloud: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stack: Vec<String> = vec![base.clone()];
    while let Some(dir) = stack.pop() {
        let entries = match state.target.list(&dir).await {
            Ok(e) => e,
            Err(e) => return Json(err(format!("列出云端目录失败: {e}"))),
        };
        for e in entries {
            if e.is_dir {
                stack.push(format!("/{}", e.rel_path.trim_matches('/')));
            } else {
                let rel = e
                    .rel_path
                    .trim_start_matches('/')
                    .strip_prefix(base_prefix.as_str())
                    .unwrap_or(e.rel_path.trim_start_matches('/'));
                cloud.insert(rel.to_string());
            }
        }
    }

    // 对比并删除失效记录（快照有、云端没有）
    let mut removed_files: Vec<String> = Vec::new();
    for rel in &snapshot_files {
        if !cloud.contains(rel) {
            match state.store.delete_entry(&job_id, &account, rel) {
                Ok(true) => removed_files.push(rel.clone()),
                Ok(false) => {}
                Err(e) => return Json(err(format!("删除快照记录失败: {e}"))),
            }
        }
    }
    let checked = snapshot_files.len();
    let removed = removed_files.len();
    let mut files = removed_files;
    files.truncate(200);
    if removed > 0 {
        state.audit.record(
            "restore.prune",
            format!(
                "清理云端缺失记录（{}）：移除 {} 条失效快照",
                body.source_path, removed
            ),
            true,
            None,
        );
    }
    Json(resp(checked, removed, files, None))
}
