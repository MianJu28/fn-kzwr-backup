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
    /// 不在目标前缀下、无法判断归属而**保守跳过**的文件数
    ///
    /// 这类文件不会被删：路径穿越/软链导致的越界条目一律当「外来文件」放着。
    pub foreign_skipped: usize,
    /// 判定为孤儿但**实际已不存在**（并发清理过/从未落盘）
    ///
    /// 必须与 `removed` 分开：把「本来就没有」计成「删掉了」会让
    /// 「已清理 N 个孤儿」这个数字失真，用户据此判断「是否发生了大规模误删」时
    /// 会被误导。
    pub already_absent: usize,
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

        // ⚠️ **空受管理集合 = 拒绝执行**（最危险的一条护栏）
        //
        // 判据是「不在 `managed` 里的就删」。若 `managed` 为空（快照读失败、
        // 任务刚建还没备份过、`load_snapshot` 返回默认值…），**目标端所有文件
        // 都会被判为孤儿并逐个删除** —— 这是不可恢复的静默数据丢失。
        //
        // 调用链上游（`BackupJob::managed_paths`）在快照为空时正是返回空集合，
        // 没有任何保护。这里做最后一道拦截：宁可跳过清理（用户可手动处理），
        // 也绝不能因为「不知道哪些受管理」就把整棵树删了。
        if managed.is_empty() {
            tracing::warn!(
                prefix = %self.target_prefix,
                "保留策略：受管理文件集合为空，跳过孤儿清理（避免误删全部文件）"
            );
            return Ok(report);
        }

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
                    // 目标端本就不存在（并发清理过，或从未真正落盘）：
                    // **不计入 removed**，单独统计（见 `already_absent` 的说明）
                    report.already_absent += 1;
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
                    // 无法判断归属（不在目标前缀下）⇒ **保守跳过**，不当孤儿
                    let Some(managed_rel) = strip_prefix(rel, &self.target_prefix) else {
                        report.foreign_skipped += 1;
                        continue;
                    };
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
        // ⚠️ **不知道年龄时保守处理：不删**。
        //
        // 曾经这里是 `let Some(mtime) = modified else { return true }`，
        // 即「拿不到 mtime 就当作够老」—— 而 WebDAV 的 `list()` 此前恒不返回
        // mtime，于是用户设置的「只清理 N 天以前的孤儿」**完全不生效**，
        // 刚备份出来的文件可能被立刻判为可清理而删掉（不可恢复的误删）。
        // 宁可漏清理（用户可手动处理），也不能误删。
        let Some(mtime) = modified else {
            tracing::debug!("保留策略：缺少 mtime，跳过该条目（保守不删）");
            return false;
        };
        match std::time::SystemTime::now().duration_since(*mtime) {
            Ok(age) => age.as_secs() >= self.min_age_secs,
            Err(_) => false,
        }
    }
}

/// 去掉目标路径的前缀，返回源相对路径。
/// 例：target_prefix="fn-backup"，rel="/fn-backup/app.conf" → "app.conf"
///     若 rel 不在前缀下，原样返回。
fn strip_prefix(rel: &str, prefix: &str) -> Option<String> {
    let rel_trimmed = rel.trim_matches('/');
    let p = prefix.trim_matches('/');
    if p.is_empty() {
        return Some(rel_trimmed.to_string());
    }
    // **不在前缀下要返回 None**（而不是原样返回）。
    //
    // 原样返回的后果：该路径必然不匹配 `managed` 集合 ⇒ 被判定为孤儿 ⇒ 删除。
    // 虽然 `walk` 的起点就是 `/<prefix>` 正常不会跨界，但这个默认值方向是**反的**：
    // 「无法判断归属」时应当保守跳过，而不是当成可删。
    rel_trimmed
        .strip_prefix(&format!("{}/", p))
        .map(|rest| rest.to_string())
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

#[cfg(test)]
mod count_accuracy_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// 假目标：`absent` 里的路径返回 NotFound（模拟「本来就不存在」）
    struct PartiallyAbsent {
        files: Vec<String>,
        absent: Vec<String>,
        deleted: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl crate::infra::storage_trait::TargetStorage for PartiallyAbsent {
        async fn write_stream(
            &self,
            _p: &std::path::Path,
            _s: Box<dyn futures::Stream<Item = bytes::Bytes> + Send + Unpin>,
        ) -> crate::infra::storage_trait::StorageResult<()> {
            Ok(())
        }
        async fn read_stream(
            &self,
            _p: &std::path::Path,
        ) -> crate::infra::storage_trait::StorageResult<
            Box<
                dyn futures::Stream<
                        Item = crate::infra::storage_trait::StorageResult<bytes::Bytes>,
                    > + Send
                    + Unpin,
            >,
        > {
            Ok(Box::new(futures::stream::empty()))
        }
        async fn delete(
            &self,
            p: &std::path::Path,
        ) -> crate::infra::storage_trait::StorageResult<()> {
            let s = p.to_string_lossy().into_owned();
            if self.absent.iter().any(|a| s.ends_with(a.as_str())) {
                return Err(crate::infra::storage_trait::StorageError::NotFound(s));
            }
            self.deleted.lock().unwrap().push(s);
            Ok(())
        }
        async fn list(
            &self,
            _prefix: &str,
        ) -> crate::infra::storage_trait::StorageResult<
            Vec<crate::infra::storage_trait::FileDescriptor>,
        > {
            Ok(self
                .files
                .iter()
                .map(|f| crate::infra::storage_trait::FileDescriptor {
                    rel_path: f.clone(),
                    size: 1,
                    modified: Some(std::time::SystemTime::now()),
                    is_dir: false,
                    digest: None,
                })
                .collect())
        }
        async fn ping(&self) -> crate::infra::storage_trait::StorageResult<()> {
            Ok(())
        }
    }

    /// **统计可信性**：「本来就不存在」不能计成「删掉了」
    ///
    /// 保留策略是破坏性操作，用户靠 `removed` 判断「是否发生了大规模误删」。
    /// 若把 404 也算成功，这个数字会虚高、掩盖真实情况。
    #[tokio::test]
    async fn already_absent_is_counted_separately() {
        let deleted = Arc::new(Mutex::new(Vec::new()));
        let target = Arc::new(PartiallyAbsent {
            files: vec![
                "fn-backup/Photos/exists.jpg".to_string(),
                "fn-backup/Photos/gone.jpg".to_string(),
            ],
            absent: vec!["gone.jpg".to_string()],
            deleted: deleted.clone(),
        });
        let policy = RetentionPolicy::new(target, "fn-backup");
        let mut managed = std::collections::HashSet::new();
        managed.insert("Photos/keep.jpg".to_string()); // 非空，避免触发空集合护栏
        let r = policy.cleanup_unmanaged(&managed).await.expect("应成功");

        assert_eq!(r.removed, 1, "只有真正删掉的那个计入 removed");
        assert_eq!(r.already_absent, 1, "本来不存在的要单独计");
        assert_eq!(r.failed, 0);
        assert_eq!(deleted.lock().unwrap().len(), 1);
    }
}

#[cfg(test)]
mod empty_managed_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// 记录被删除路径的假目标
    struct RecordingTarget {
        deleted: Arc<Mutex<Vec<String>>>,
        /// list 返回的文件（模拟云端已有文件）
        files: Vec<String>,
    }

    #[async_trait::async_trait]
    impl crate::infra::storage_trait::TargetStorage for RecordingTarget {
        async fn write_stream(
            &self,
            _p: &std::path::Path,
            _s: Box<dyn futures::Stream<Item = bytes::Bytes> + Send + Unpin>,
        ) -> crate::infra::storage_trait::StorageResult<()> {
            Ok(())
        }
        async fn read_stream(
            &self,
            _p: &std::path::Path,
        ) -> crate::infra::storage_trait::StorageResult<
            Box<
                dyn futures::Stream<
                        Item = crate::infra::storage_trait::StorageResult<bytes::Bytes>,
                    > + Send
                    + Unpin,
            >,
        > {
            Ok(Box::new(futures::stream::empty()))
        }
        async fn delete(
            &self,
            p: &std::path::Path,
        ) -> crate::infra::storage_trait::StorageResult<()> {
            self.deleted.lock().unwrap().push(p.to_string_lossy().into_owned());
            Ok(())
        }
        async fn list(
            &self,
            _prefix: &str,
        ) -> crate::infra::storage_trait::StorageResult<
            Vec<crate::infra::storage_trait::FileDescriptor>,
        > {
            Ok(self
                .files
                .iter()
                .map(|f| crate::infra::storage_trait::FileDescriptor {
                    rel_path: f.clone(),
                    size: 10,
                    modified: Some(std::time::SystemTime::now()),
                    is_dir: false,
                    digest: None,
                })
                .collect())
        }
        async fn ping(&self) -> crate::infra::storage_trait::StorageResult<()> {
            Ok(())
        }
    }

    /// **误删防护（最危险的一条）**：受管理集合为空时一个文件都不能删
    ///
    /// 若快照读失败/任务刚建，`managed` 会是空集；而判据是「不在集合里就删」，
    /// 于是目标端**所有文件**都会被当孤儿删除 —— 不可恢复的数据丢失。
    #[tokio::test]
    async fn empty_managed_set_deletes_nothing() {
        let deleted = Arc::new(Mutex::new(Vec::new()));
        let target = Arc::new(RecordingTarget {
            deleted: deleted.clone(),
            files: vec![
                "fn-backup/Photos/a.jpg".to_string(),
                "fn-backup/Config/b.yml".to_string(),
            ],
        });
        let policy = RetentionPolicy::new(target, "fn-backup");
        let report = policy
            .cleanup_unmanaged(&std::collections::HashSet::new())
            .await
            .expect("不应报错（而是保守跳过）");

        assert_eq!(
            deleted.lock().unwrap().len(),
            0,
            "受管理集合为空时必须一个文件都不删（否则整棵树被清空）"
        );
        assert_eq!(report.removed, 0);
    }

    /// 对照组：非空集时正常删除孤儿（证明护栏没有把功能关死）
    #[tokio::test]
    async fn non_empty_managed_set_still_removes_orphans() {
        let deleted = Arc::new(Mutex::new(Vec::new()));
        let target = Arc::new(RecordingTarget {
            deleted: deleted.clone(),
            files: vec![
                "fn-backup/Photos/a.jpg".to_string(),  // 受管理 → 保留
                "fn-backup/Photos/old.jpg".to_string(), // 孤儿 → 删除
            ],
        });
        let policy = RetentionPolicy::new(target, "fn-backup");
        let mut managed = std::collections::HashSet::new();
        managed.insert("Photos/a.jpg".to_string());
        let report = policy.cleanup_unmanaged(&managed).await.expect("应成功");

        let d = deleted.lock().unwrap().clone();
        assert_eq!(d.len(), 1, "只应删除孤儿：{d:?}");
        assert!(d[0].contains("old.jpg"), "删错文件：{d:?}");
        assert_eq!(report.removed, 1);
    }
}

#[cfg(test)]
mod age_tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// 构造一个只用于测 `age_ok` 的策略（不需要真实目标）
    fn policy(min_age_secs: u64) -> RetentionPolicy {
        // 用一个永不调用的占位目标：这些测试只碰 `age_ok`
        struct NoopTarget;
        #[async_trait::async_trait]
        impl crate::infra::storage_trait::TargetStorage for NoopTarget {
            async fn write_stream(
                &self,
                _p: &std::path::Path,
                _s: Box<dyn futures::Stream<Item = bytes::Bytes> + Send + Unpin>,
            ) -> crate::infra::storage_trait::StorageResult<()> {
                Ok(())
            }
            async fn read_stream(
                &self,
                _p: &std::path::Path,
            ) -> crate::infra::storage_trait::StorageResult<
                Box<
                    dyn futures::Stream<
                            Item = crate::infra::storage_trait::StorageResult<bytes::Bytes>,
                        > + Send
                        + Unpin,
                >,
            > {
                Ok(Box::new(futures::stream::empty()))
            }
            async fn delete(&self, _p: &std::path::Path) -> crate::infra::storage_trait::StorageResult<()> {
                Ok(())
            }
            async fn list(
                &self,
                _prefix: &str,
            ) -> crate::infra::storage_trait::StorageResult<
                Vec<crate::infra::storage_trait::FileDescriptor>,
            > {
                Ok(Vec::new())
            }
            async fn ping(&self) -> crate::infra::storage_trait::StorageResult<()> {
                Ok(())
            }
        }
        RetentionPolicy::new(Arc::new(NoopTarget), "fn-backup")
            .with_min_age_secs(min_age_secs)
    }

    /// **误删防护回归**：拿不到 mtime 时**不能**判为「够老」
    ///
    /// 曾经 `None => true`，而 WebDAV 的 `list()` 恒不返回 mtime，
    /// 于是「只清理 N 天以前的孤儿」完全失效，刚备份的文件也可能被立刻删掉。
    #[test]
    fn unknown_mtime_is_treated_as_too_new() {
        let p = policy(7 * 24 * 3600);
        assert!(
            !p.age_ok(None),
            "缺少 mtime 时必须保守不删（否则会误删刚备份的文件）"
        );
    }

    /// 正常语义：够老的删、太新的留
    #[test]
    fn old_files_pass_young_files_do_not() {
        let p = policy(7 * 24 * 3600);
        let old = SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
        let young = SystemTime::now() - Duration::from_secs(3600);
        assert!(p.age_ok(Some(&old)), "30 天前的文件应满足最小年龄");
        assert!(!p.age_ok(Some(&young)), "1 小时前的文件不该被清理");
    }

    /// `min_age_secs == 0` 表示不限制，任何文件都可清（含未知 mtime）
    #[test]
    fn zero_min_age_disables_the_guard() {
        let p = policy(0);
        assert!(p.age_ok(None));
        assert!(p.age_ok(Some(&SystemTime::now())));
    }
}
