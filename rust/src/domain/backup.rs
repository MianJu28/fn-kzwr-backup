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
}

impl BackupJob {
    /// 执行一次增量备份，返回上传/删除统计
    pub async fn run(&self, source_root: &Path) -> Result<BackupSummary> {
        // 1) 扫描源目录
        let current = scan_all(&*self.source, source_root).await?;
        info!(count = current.len(), "源扫描完成");

        // 2) 加载上次快照 + 差分
        let last = self.store.load_snapshot(&self.job_id)?;
        let changeset = SyncSession::diff(&current, &last);
        info!(
            upload = changeset.upload.len(),
            delete = changeset.delete.len(),
            unchanged = changeset.unchanged,
            "差分完成"
        );

        // 3) 上传新增/修改文件（目录不实际上传，仅记录）
        let mut uploaded = 0usize;
        let mut uploaded_bytes = 0u64;
        for fd in &changeset.upload {
            if fd.is_dir {
                continue;
            }
            let n = self.upload_one(&fd.rel_path).await?;
            uploaded += 1;
            uploaded_bytes += n;
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

        // 5) 保存新快照
        let snapshot: Vec<SnapshotEntry> = current.iter().map(SnapshotEntry::from_fd).collect();
        self.store.save_snapshot(&self.job_id, &snapshot)?;

        Ok(BackupSummary {
            uploaded,
            uploaded_bytes,
            deleted,
            unchanged: changeset.unchanged,
        })
    }

    /// 上传单个文件：源流 → age 加密 → 目标
    async fn upload_one(&self, rel_path: &str) -> Result<u64> {
        let src = PathBuf::from(rel_path);
        let mut stream = self.source.read_stream(&src).await?;

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

    /// 计算目标路径：把相对路径拼上 target_prefix 前缀
    fn target_path(&self, rel_path: &str) -> PathBuf {
        match &self.target_prefix {
            Some(prefix) => {
                let prefix = prefix.trim_matches('/');
                if prefix.is_empty() {
                    PathBuf::from(rel_path)
                } else {
                    PathBuf::from(format!("{}/{}", prefix, rel_path.trim_start_matches('/')))
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
