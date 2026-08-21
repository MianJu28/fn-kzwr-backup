//! 备份调度（核心域：增量备份主流程）
//!
//! 把 Source → 差分 → age 加密 → Target 串成端到端管道。
//! 流程：
//!   1. 递归扫描源目录
//!   2. 加载上次快照，SyncSession::diff 生成 ChangeSet
//!   3. 上传文件：源流式读取 → age 分块加密 → 目标写入
//!   4. 删除文件：从目标移除
//!   5. 保存新快照

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use futures::StreamExt;
use tracing::info;

use crate::domain::crypto::CryptoSession;
use crate::domain::sync::SyncSession;
use crate::infra::persistence::snapshot::{SnapshotEntry, SnapshotStore};
use crate::infra::storage_trait::{
    FileDescriptor, SourceStorage, StorageError, TargetStorage,
};

/// 备份任务
pub struct BackupJob {
    pub job_id: String,
    pub source: Arc<dyn SourceStorage>,
    pub target: Arc<dyn TargetStorage>,
    pub crypto: CryptoSession,
    pub store: Arc<SnapshotStore>,
    /// 目标根目录前缀（如 "/fn-backup"），为 None 时写入目标根
    pub target_prefix: Option<String>,
    /// 内部事件总线（进度推送，可选）
    pub eventbus: Option<Arc<crate::eventbus::EventBus>>,
}

impl BackupJob {
    /// 执行一次增量备份（快速模式 mtime+size）
    pub async fn run(&self, source_root: &Path) -> Result<BackupSummary> {
        self.run_inner(&*self.source, source_root, &self.job_id, false)
            .await
    }

    /// 执行一次增量备份（严格模式，BLAKE3 内容哈希确认）
    pub async fn run_strict(&self, source_root: &Path) -> Result<BackupSummary> {
        self.run_inner(&*self.source, source_root, &self.job_id, true)
            .await
    }

    /// 多路径备份：遍历多个源目录，各自增量备份到目标
    pub async fn run_multi(&self, roots: &[std::path::PathBuf]) -> Result<BackupSummary> {
        let mut total = BackupSummary::default();
        for (i, root) in roots.iter().enumerate() {
            // 每个源路径对应独立 job 快照（用序号作 job_id 后缀，避免快照互相覆盖）
            let job_id = format!("{}-{}", self.job_id, i);
            let source = crate::infra::source::local::LocalFsSource::new(root);
            let sub = self.run_inner(&source, root, &job_id, false).await?;
            total.uploaded += sub.uploaded;
            total.uploaded_bytes += sub.uploaded_bytes;
            total.deleted += sub.deleted;
            total.unchanged += sub.unchanged;
            info!(root = %root.display(), "子路径备份完成");
        }
        Ok(total)
    }

    /// 备份核心实现
    async fn run_inner(
        &self,
        source: &dyn SourceStorage,
        source_root: &Path,
        job_id: &str,
        strict: bool,
    ) -> Result<BackupSummary> {
        // 1) 扫描源目录（严格模式时计算 BLAKE3 内容哈希）
        let current = if strict {
            scan_all_strict(source, source_root).await?
        } else {
            scan_all(source, source_root).await?
        };
        info!(count = current.len(), strict, "源扫描完成");

        // 2) 加载上次快照 + 差分
        let last = self.store.load_snapshot(job_id)?;
        let changeset = if strict {
            // 严格模式：对 mtime/size 变化的文件算哈希确认
            let root = source_root.to_path_buf();
            let hasher = move |fd: &FileDescriptor| -> Option<String> {
                if fd.is_dir {
                    return None;
                }
                let path = root.join(&fd.rel_path);
                let data = std::fs::read(&path).ok()?;
                Some(SyncSession::blake3_hex(&data))
            };
            SyncSession::diff_strict(&current, &last, hasher)
        } else {
            SyncSession::diff(&current, &last)
        };
        info!(
            upload = changeset.upload.len(),
            delete = changeset.delete.len(),
            unchanged = changeset.unchanged,
            strict,
            "差分完成"
        );

        // 3) 上传新增/修改文件（目录不实际上传，仅记录）
        //    断点续传：每上传完一个文件即时保存其快照，中断后下次可从断点继续
        let total_upload = changeset.upload.iter().filter(|fd| !fd.is_dir).count() as u64;
        self.publish(
            job_id,
            crate::eventbus::TaskStatus::Started,
            None,
            0,
            total_upload,
            None,
        );

        let mut uploaded = 0usize;
        let mut uploaded_bytes = 0u64;
        for fd in &changeset.upload {
            if fd.is_dir {
                continue;
            }
            let n = self.upload_one(source, &fd.rel_path).await?;
            uploaded += 1;
            uploaded_bytes += n;
            // 即时记录已上传文件的快照（断点续传关键）
            self.store.save_entry(job_id, &SnapshotEntry::from_fd(fd))?;
            self.publish(
                job_id,
                crate::eventbus::TaskStatus::Progress,
                Some(fd.rel_path.clone()),
                uploaded as u64,
                total_upload,
                None,
            );
            info!("已上传: {} ({n} B)", fd.rel_path);
        }

        // 4) 删除目标中已不存在的文件
        let mut deleted = 0usize;
        for rel in &changeset.delete {
            let target_path = self.target_path(rel);
            match self.target.delete(&target_path).await {
                Ok(()) => {
                    deleted += 1;
                    info!("已删除: {}", rel);
                }
                Err(StorageError::NotFound(_)) => {}
                Err(e) => info!(err = %e, "删除失败(跳过): {}", rel),
            }
        }

        // 5) 保存新快照（完整覆盖，含未变化文件与删除后的状态）
        let snapshot: Vec<SnapshotEntry> = current.iter().map(SnapshotEntry::from_fd).collect();
        self.store.save_snapshot(job_id, &snapshot)?;

        self.publish(
            job_id,
            crate::eventbus::TaskStatus::Completed,
            None,
            uploaded as u64,
            total_upload,
            None,
        );

        Ok(BackupSummary {
            uploaded,
            uploaded_bytes,
            deleted,
            unchanged: changeset.unchanged,
        })
    }

    /// 发布进度事件（事件总线可选）
    fn publish(
        &self,
        job_id: &str,
        status: crate::eventbus::TaskStatus,
        current_file: Option<String>,
        done: u64,
        total: u64,
        message: Option<String>,
    ) {
        if let Some(eb) = &self.eventbus {
            eb.task_event(
                crate::eventbus::TaskKind::Backup,
                status,
                job_id.to_string(),
                current_file,
                done,
                total,
                message,
            );
        }
    }

    /// 上传单个文件：源流 → age 加密 → 目标
    async fn upload_one(&self, source: &dyn SourceStorage, rel_path: &str) -> Result<u64> {
        let src = PathBuf::from(rel_path);
        let mut stream = source.read_stream(&src).await?;

        // 收集源流字节（Phase 2 简化：整文件收集后分块加密）
        let mut plain = Vec::new();
        while let Some(chunk) = stream.next().await {
            plain.extend_from_slice(&chunk?);
        }

        // age 分块加密（同步阻塞，放入 spawn_blocking 避免阻塞 async runtime）
        let crypto = self.crypto.clone();
        let plain_len = plain.len();
        let encrypted = tokio::task::spawn_blocking(move || {
            let mut writer = std::io::Cursor::new(Vec::new());
            crypto
                .encrypt_stream(&plain[..], &mut writer)
                .map(|_| writer.into_inner())
        })
        .await??;

        // 写入目标（带 target_prefix）
        let bytes = Bytes::from(encrypted);
        let stream = Box::new(futures::stream::iter(vec![bytes]));
        let target_path = self.target_path(rel_path);
        self.target.write_stream(&target_path, stream).await?;
        Ok(plain_len as u64)
    }

    /// 计算目标路径：把相对路径拼上 target_prefix 前缀（统一带前导斜杠，如 "/fn-backup/xxx"）
    fn target_path(&self, rel_path: &str) -> PathBuf {
        match &self.target_prefix {
            Some(prefix) => {
                let prefix = prefix.trim_matches('/');
                if prefix.is_empty() {
                    PathBuf::from(rel_path)
                } else {
                    PathBuf::from(format!("/{}/{}", prefix, rel_path.trim_start_matches('/')))
                }
            }
            None => PathBuf::from(rel_path),
        }
    }
}

/// 备份结果摘要
#[derive(Debug, Default)]
pub struct BackupSummary {
    pub uploaded: usize,
    pub uploaded_bytes: u64,
    pub deleted: usize,
    pub unchanged: usize,
}

/// 递归扫描源目录（使用相对路径，Source 内部解析到自身 root）
async fn scan_all(
    source: &dyn SourceStorage,
    _root: &Path,
) -> Result<Vec<FileDescriptor>> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let entries = source.list(&rel).await?;
        for e in entries {
            out.push(e.clone());
            if e.is_dir {
                // 相对路径递归
                let child_rel = PathBuf::from(&e.rel_path);
                stack.push(child_rel);
            }
        }
    }
    Ok(out)
}

/// 递归扫描源目录（严格模式：为文件计算 BLAKE3 内容哈希存入 digest）
async fn scan_all_strict(
    source: &dyn SourceStorage,
    root: &Path,
) -> Result<Vec<FileDescriptor>> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let entries = source.list(&rel).await?;
        for mut e in entries {
            if !e.is_dir {
                // 计算 BLAKE3 内容哈希
                if let Some(digest) = read_blake3(root, &e.rel_path) {
                    e.digest = Some(digest);
                }
            }
            out.push(e.clone());
            if e.is_dir {
                let child_rel = PathBuf::from(&e.rel_path);
                stack.push(child_rel);
            }
        }
    }
    Ok(out)
}

/// 读取文件并计算 BLAKE3 内容哈希（32 字节）
fn read_blake3(root: &Path, rel: &str) -> Option<[u8; 32]> {
    let path = root.join(rel);
    let data = std::fs::read(&path).ok()?;
    Some(blake3::hash(&data).into())
}
