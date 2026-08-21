//! 保留策略（核心域：目标端孤儿文件清理）
//!
//! 当前系统为镜像同步（目标与源保持一致，无历史版本快照）。
//! 保留策略聚焦于防止目标端空间膨胀：
//!   - 清理目标端残留的、不在任何备份任务快照管理下的孤儿文件。
//!
//! 不改动备份主流程，仅作为备份后的可选清理步骤。
//! 典型触发场景：job_id 变更、断点续传快照残缺、历史测试残留。

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use crate::infra::storage_trait::{StorageError, TargetStorage};

/// 孤儿文件清理报告
#[derive(Debug, Default, Clone)]
pub struct CleanupReport {
    /// 目标端扫描到的文件总数
    pub scanned_files: usize,
    /// 判定为孤儿并删除的文件数
    pub removed: usize,
    /// 删除失败跳过数
    pub failed: usize,
}

/// 保留策略（孤儿文件清理）
pub struct RetentionPolicy {
    target: Arc<dyn TargetStorage>,
    /// 目标根前缀，如 "/fn-backup"；为空表示目标根
    target_prefix: String,
    /// 可选：只清理早于该秒数的文件（0 表示不限制）
    min_age_secs: u64,
}

impl RetentionPolicy {
    /// 创建保留策略
    pub fn new(target: Arc<dyn TargetStorage>, target_prefix: &str) -> Self {
        Self {
            target,
            target_prefix: target_prefix.trim_matches('/').to_string(),
            min_age_secs: 0,
        }
    }

    /// 设置最小年龄（秒），早于该年龄才清理；0 表示不限制
    pub fn with_min_age_secs(mut self, secs: u64) -> Self {
        self.min_age_secs = secs;
        self
    }

    /// 清理目标端未管理（孤儿）文件
    ///
    /// `managed` 是受管理的相对路径集合（通常来自快照合并，
    /// 相对源目录，如 "app.conf"）。目标端存在但不在 `managed`
    /// 集合中的文件将被删除。
    pub async fn cleanup_unmanaged(&self, managed: &HashSet<String>) -> Result<CleanupReport> {
        let mut report = CleanupReport::default();
        let mut to_remove: Vec<String> = Vec::new();

        // 1) 递归列出目标端前缀下所有文件
        let root = format!("/{}", self.target_prefix);
        self.walk(&root, managed, &mut to_remove, &mut report).await?;

        // 2) 逐个删除孤儿文件
        for rel in &to_remove {
            match self.target.delete(Path::new(rel)).await {
                Ok(()) => {
                    report.removed += 1;
                    info!("[retention] 已删除孤儿文件: {}", rel);
                }
                Err(StorageError::NotFound(_)) => {
                    // 已被删或并发清理，忽略
                    report.removed += 1;
                }
                Err(e) => {
                    report.failed += 1;
                    warn!(err = %e, "[retention] 删除孤儿文件失败(跳过): {}", rel);
                }
            }
        }

        info!(
            scanned = report.scanned_files,
            removed = report.removed,
            failed = report.failed,
            "[retention] 孤儿文件清理完成"
        );
        Ok(report)
    }

    /// 递归遍历目标端目录（显式栈迭代，避免 async 递归 future），收集孤儿文件
    async fn walk(
        &self,
        root: &str,
        managed: &HashSet<String>,
        to_remove: &mut Vec<String>,
        report: &mut CleanupReport,
    ) -> Result<()> {
        let mut stack: Vec<String> = vec![root.to_string()];
        while let Some(dir) = stack.pop() {
            let entries = self.target.list(&dir).await?;
            for e in entries {
                let rel = &e.rel_path;
                if e.is_dir {
                    // 子目录入栈（kzwr list 返回带前缀的完整路径）
                    let sub = format!("/{}", rel.trim_matches('/'));
                    stack.push(sub);
                } else {
                    report.scanned_files += 1;
                    // 把目标端路径去掉前缀，得到源相对路径
                    let managed_rel = strip_prefix(rel, &self.target_prefix);
                    // 检查是否受管理；若非受管理且满足年龄要求，判为孤儿
                    if !managed.contains(&managed_rel) && self.age_ok(e.modified.as_ref()) {
                        to_remove.push(rel.clone());
                    }
                }
            }
        }
        Ok(())
    }

    /// 判断文件是否满足最小年龄要求（早于 min_age_secs 才清理）
    fn age_ok(&self, modified: Option<&std::time::SystemTime>) -> bool {
        if self.min_age_secs == 0 {
            return true;
        }
        let Some(mtime) = modified else { return true };
        match std::time::SystemTime::now().duration_since(*mtime) {
            Ok(age) => age.as_secs() >= self.min_age_secs,
            Err(_) => false,
        }
    }
}

/// 去掉目标路径的前缀，返回源相对路径。
/// 例：target_prefix="fn-backup"，rel="/fn-backup/app.conf" → "app.conf"
///     若 rel 不在前缀下，原样返回。
fn strip_prefix(rel: &str, prefix: &str) -> String {
    let rel_trimmed = rel.trim_matches('/');
    let p = prefix.trim_matches('/');
    if p.is_empty() {
        return rel_trimmed.to_string();
    }
    match rel_trimmed.strip_prefix(&format!("{}/", p)) {
        Some(rest) => rest.to_string(),
        None => rel_trimmed.to_string(),
    }
}

/// 由快照条目集合构造受管理相对路径集合
pub fn managed_set(
    snapshots: &[crate::infra::persistence::snapshot::SnapshotEntry],
) -> HashSet<String> {
    snapshots
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| e.rel_path.clone())
        .collect()
}
