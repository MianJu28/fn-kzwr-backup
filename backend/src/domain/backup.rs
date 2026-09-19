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
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use bytes::Bytes;
use futures::StreamExt;
use tracing::info;

use crate::domain::crypto::CryptoSession;
use crate::domain::pace::SpeedMeter;
use crate::domain::retention::RetentionPolicy;
use crate::domain::sync::SyncSession;
use crate::infra::persistence::snapshot::{SnapshotEntry, SnapshotStore};
use crate::infra::storage_trait::{
    FileDescriptor, ProgressCb, SourceStorage, StorageError, TargetStorage,
};

/// 上传进度回调（明文口径）：(本文件已上传明文, 本文件已传输密文, 本请求已用时毫秒)
type PlainProgressCb = Arc<dyn Fn(u64, u64, u64) + Send + Sync>;

/// 备份任务
pub struct BackupJob {
    pub job_id: String,
    /// 备份所属账号（区分不同账号的快照，为 None 时归入默认 ''）
    pub account: Option<String>,
    pub source: Arc<dyn SourceStorage>,
    pub target: Arc<dyn TargetStorage>,
    pub crypto: CryptoSession,
    pub store: Arc<SnapshotStore>,
    /// 目标根目录前缀（如 "/fn-backup"），为 None 时写入目标根
    pub target_prefix: Option<String>,
    /// 内部事件总线（进度推送，可选）
    pub eventbus: Option<Arc<crate::eventbus::EventBus>>,
    /// 保留策略（可选）：备份完成后清理目标端孤儿文件
    pub retention: Option<RetentionPolicy>,
}

impl BackupJob {
    /// 当前账号标识（空字符串表示未区分账号）
    fn account(&self) -> &str {
        self.account.as_deref().unwrap_or("")
    }

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
    ///
    /// 关键行为：
    /// - 每个源路径在目标端以其**文件夹名**分目录存放（如 `/fn-backup/Photos/...`）；
    /// - 进度按**全部文件夹合并**统计（总文件数/总字节）；
    /// - 字节进度按**明文**展示（总大小 = 扫描得到的明文总量）；
    /// - 完成数只在**单个文件上传完成后**才 +1；
    /// - 速度由后端按实际传输时段计量（空闲/文件间隙不变化）。
    pub async fn run_multi(&self, roots: &[std::path::PathBuf]) -> Result<BackupSummary> {
        /// 单个已预扫描的子任务
        struct Prepared {
            job_id: String,
            source: Arc<dyn SourceStorage>,
            current: Vec<FileDescriptor>,
            changeset: crate::domain::sync::ChangeSet,
            target_prefix: Option<String>,
        }

        // 1) 预扫描 + 差分：汇总总文件数与总字节数（明文）
        let mut prepared: Vec<Prepared> = Vec::new();
        let mut grand_total_files: u64 = 0;
        let mut grand_total_bytes: u64 = 0;
        for (i, root) in roots.iter().enumerate() {
            let job_id = format!("{}-{}", self.job_id, i);
            let source: Arc<dyn SourceStorage> =
                Arc::new(crate::infra::source::local::LocalFsSource::new(root));
            let current = scan_all(&*source, root).await?;
            let last = self.store.load_snapshot(&job_id, self.account())?;
            let changeset = SyncSession::diff(&current, &last);
            grand_total_files += changeset.upload.iter().filter(|fd| !fd.is_dir).count() as u64;
            grand_total_bytes += changeset
                .upload
                .iter()
                .filter(|fd| !fd.is_dir)
                .map(|fd| fd.size)
                .sum::<u64>();
            let root_name = root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let target_prefix = self.join_root_prefix(&root_name);
            prepared.push(Prepared {
                job_id,
                source,
                current,
                changeset,
                target_prefix,
            });
        }

        let meter = Arc::new(Mutex::new(SpeedMeter::new()));
        let started = Instant::now();
        self.publish(
            &self.job_id,
            crate::eventbus::TaskStatus::Started,
            None,
            0,
            grand_total_files,
            0,
            grand_total_bytes,
            0,
            0,
            None,
        );

        let mut done: u64 = 0;
        let mut bytes_done: u64 = 0; // 明文
        let mut summary = BackupSummary::default();
        let mut job_ids: Vec<String> = Vec::new();

        for p in prepared {
            // 1) 显式创建目标端目录结构（含空目录），保证「所选文件夹」及其各级子目录存在
            let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
            for fd in p.current.iter().filter(|e| !e.is_dir) {
                let mut rel = fd.rel_path.as_str();
                while let Some(idx) = rel.rfind('/') {
                    rel = &rel[..idx];
                    covered.insert(rel.to_string());
                }
            }
            if let Some(prefix) = &p.target_prefix {
                let root_path = PathBuf::from(format!("/{}", prefix.trim_matches('/')));
                let _ = self.target.ensure_dir(&root_path).await;
            }
            for dir in p.current.iter().filter(|e| e.is_dir) {
                if covered.contains(&dir.rel_path) {
                    continue; // 会被文件上传时的父目录创建覆盖
                }
                let tp = target_path_with(p.target_prefix.as_deref(), &dir.rel_path);
                let _ = self.target.ensure_dir(&tp).await;
            }

            // 2) 上传新增/修改文件（断点续传：每上传完一个文件即时保存其快照）
            for fd in &p.changeset.upload {
                if fd.is_dir {
                    continue;
                }
                let file_plain = fd.size;
                meter.lock().unwrap().begin_file();
                let cb = self.plain_progress_cb(
                    &self.job_id,
                    &fd.rel_path,
                    done,
                    grand_total_files,
                    bytes_done,
                    grand_total_bytes,
                    file_plain,
                    meter.clone(),
                    started,
                );
                let plain_n = self
                    .upload_one(&*p.source, &fd.rel_path, p.target_prefix.as_deref(), Some(cb))
                    .await?;
                // 上传**完成后**才计入完成数（避免「未完成就 +1」）
                done += 1;
                bytes_done += plain_n;
                summary.uploaded += 1;
                summary.uploaded_bytes += plain_n;
                self.store
                    .save_entry(&p.job_id, self.account(), &SnapshotEntry::from_fd(fd))?;
                self.publish(
                    &self.job_id,
                    crate::eventbus::TaskStatus::Progress,
                    Some(fd.rel_path.clone()),
                    done,
                    grand_total_files,
                    bytes_done,
                    grand_total_bytes,
                    started.elapsed().as_millis() as u64,
                    meter.lock().unwrap().speed_bps(),
                    None,
                );
                info!("已上传: {} ({plain_n} B)", fd.rel_path);
            }

            // 3) 删除目标中已不存在的文件
            for rel in &p.changeset.delete {
                let target_path = target_path_with(p.target_prefix.as_deref(), rel);
                match self.target.delete(&target_path).await {
                    Ok(()) => {
                        summary.deleted += 1;
                        info!("已删除: {}", rel);
                    }
                    Err(StorageError::NotFound(_)) => {}
                    Err(e) => info!(err = %e, "删除失败(跳过): {}", rel),
                }
            }

            // 4) 保存该路径的新快照
            let snapshot: Vec<SnapshotEntry> =
                p.current.iter().map(SnapshotEntry::from_fd).collect();
            self.store
                .save_snapshot(&p.job_id, self.account(), &snapshot)?;
            summary.unchanged += p.changeset.unchanged;
            job_ids.push(p.job_id);
        }

        self.publish(
            &self.job_id,
            crate::eventbus::TaskStatus::Completed,
            None,
            done,
            grand_total_files,
            bytes_done,
            grand_total_bytes,
            started.elapsed().as_millis() as u64,
            meter.lock().unwrap().speed_bps(),
            None,
        );

        // 保留策略：备份完成后清理目标端孤儿文件
        if let Some(rt) = &self.retention {
            let managed = self.managed_paths(&job_ids)?;
            let report = rt.cleanup_unmanaged(&managed).await?;
            summary.orphan_removed += report.removed;
            info!(
                removed = report.removed,
                failed = report.failed,
                scanned = report.scanned_files,
                "保留策略：孤儿文件清理完成"
            );
        }

        Ok(summary)
    }

    /// 汇总多个 job 的快照，构造受管理的相对路径集合（用于保留策略）
    fn managed_paths(&self, job_ids: &[String]) -> Result<std::collections::HashSet<String>> {
        let mut set = std::collections::HashSet::new();
        for job_id in job_ids {
            for entry in self.store.load_snapshot(job_id, self.account())? {
                if !entry.is_dir {
                    set.insert(entry.rel_path);
                }
            }
        }
        Ok(set)
    }

    /// 在目标根前缀下追加「所选文件夹名」，作为该路径的独立目标目录
    fn join_root_prefix(&self, root_name: &str) -> Option<String> {
        let name = root_name.trim_matches('/');
        if name.is_empty() {
            return self.target_prefix.clone();
        }
        match self.target_prefix.as_deref() {
            Some(base) if !base.trim_matches('/').is_empty() => {
                Some(format!("{}/{}", base.trim_matches('/'), name))
            }
            _ => Some(name.to_string()),
        }
    }

    /// 备份核心实现（单路径）
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
        let last = self.store.load_snapshot(job_id, self.account())?;
        let changeset = if strict {
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

        // 3) 上传新增/修改文件
        let total_upload = changeset.upload.iter().filter(|fd| !fd.is_dir).count() as u64;
        let total_bytes: u64 = changeset
            .upload
            .iter()
            .filter(|fd| !fd.is_dir)
            .map(|fd| fd.size)
            .sum();
        let meter = Arc::new(Mutex::new(SpeedMeter::new()));
        let started = Instant::now();
        self.publish(
            job_id,
            crate::eventbus::TaskStatus::Started,
            None,
            0,
            total_upload,
            0,
            total_bytes,
            0,
            0,
            None,
        );

        let mut uploaded = 0usize;
        let mut uploaded_bytes = 0u64;
        for fd in &changeset.upload {
            if fd.is_dir {
                continue;
            }
            meter.lock().unwrap().begin_file();
            let cb = self.plain_progress_cb(
                job_id,
                &fd.rel_path,
                uploaded as u64,
                total_upload,
                uploaded_bytes,
                total_bytes,
                fd.size,
                meter.clone(),
                started,
            );
            let plain_n = self
                .upload_one(source, &fd.rel_path, self.target_prefix.as_deref(), Some(cb))
                .await?;
            uploaded += 1;
            uploaded_bytes += plain_n;
            self.store
                .save_entry(job_id, self.account(), &SnapshotEntry::from_fd(fd))?;
            self.publish(
                job_id,
                crate::eventbus::TaskStatus::Progress,
                Some(fd.rel_path.clone()),
                uploaded as u64,
                total_upload,
                uploaded_bytes,
                total_bytes,
                started.elapsed().as_millis() as u64,
                meter.lock().unwrap().speed_bps(),
                None,
            );
            info!("已上传: {} ({plain_n} B)", fd.rel_path);
        }

        // 4) 删除目标中已不存在的文件
        let mut deleted = 0usize;
        for rel in &changeset.delete {
            let target_path = target_path_with(self.target_prefix.as_deref(), rel);
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
        self.store.save_snapshot(job_id, self.account(), &snapshot)?;

        self.publish(
            job_id,
            crate::eventbus::TaskStatus::Completed,
            None,
            uploaded as u64,
            total_upload,
            uploaded_bytes,
            total_bytes,
            started.elapsed().as_millis() as u64,
            meter.lock().unwrap().speed_bps(),
            None,
        );

        Ok(BackupSummary {
            uploaded,
            uploaded_bytes,
            deleted,
            unchanged: changeset.unchanged,
            orphan_removed: 0,
        })
    }

    /// 构造上传进度回调（明文口径）：换算明文增量、更新速度、发布事件
    #[allow(clippy::too_many_arguments)]
    fn plain_progress_cb(
        &self,
        job_id: &str,
        file: &str,
        done_files: u64,
        total_files: u64,
        base_plain: u64,
        total_plain: u64,
        file_plain: u64,
        meter: Arc<Mutex<SpeedMeter>>,
        started: Instant,
    ) -> PlainProgressCb {
        let eb = self.eventbus.clone();
        let job_id = job_id.to_string();
        let file = file.to_string();
        Arc::new(move |plain_in_file: u64, written: u64, req_elapsed_ms: u64| {
            let speed = {
                let mut m = meter.lock().unwrap();
                m.observe(written, req_elapsed_ms);
                m.speed_bps()
            };
            if let Some(eb) = &eb {
                eb.task_event(
                    crate::eventbus::TaskKind::Backup,
                    crate::eventbus::TaskStatus::Progress,
                    job_id.clone(),
                    Some(file.clone()),
                    done_files,
                    total_files,
                    base_plain + plain_in_file.min(file_plain),
                    total_plain,
                    started.elapsed().as_millis() as u64,
                    speed,
                    None,
                );
            }
        })
    }

    /// 发布进度事件（事件总线可选）
    #[allow(clippy::too_many_arguments)]
    fn publish(
        &self,
        job_id: &str,
        status: crate::eventbus::TaskStatus,
        current_file: Option<String>,
        done: u64,
        total: u64,
        bytes_done: u64,
        bytes_total: u64,
        elapsed_ms: u64,
        speed: u64,
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
                bytes_done,
                bytes_total,
                elapsed_ms,
                speed,
                message,
            );
        }
    }

    /// 上传单个文件：源流 → age 加密 → 目标（可选按块上报明文进度）
    async fn upload_one(
        &self,
        source: &dyn SourceStorage,
        rel_path: &str,
        prefix: Option<&str>,
        progress: Option<PlainProgressCb>,
    ) -> Result<u64> {
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

        // 写入目标；进度按**密文**字节从传输层上报，此处按比例换算为**明文**增量
        let enc_total = encrypted.len() as u64;
        let plain_total = plain_len as u64;
        let bytes = Bytes::from(encrypted);
        let stream = Box::new(futures::stream::iter(vec![bytes]));
        let target_path = target_path_with(prefix, rel_path);
        match progress {
            Some(outer) => {
                let adapter: ProgressCb =
                    Arc::new(move |written: u64, _total: u64, elapsed_ms: u64| {
                        let plain_in_file = if enc_total > 0 {
                            ((written.min(enc_total) as f64 / enc_total as f64)
                                * plain_total as f64) as u64
                        } else {
                            plain_total
                        };
                        outer(plain_in_file, written, elapsed_ms);
                    });
                self.target
                    .write_stream_progress(&target_path, stream, adapter)
                    .await?;
            }
            None => {
                self.target.write_stream(&target_path, stream).await?;
            }
        }
        Ok(plain_len as u64)
    }
}

/// 计算目标路径：把相对路径拼上前缀（统一带前导斜杠，如 "/fn-backup/Photos/xxx"）
fn target_path_with(prefix: Option<&str>, rel_path: &str) -> PathBuf {
    match prefix {
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

/// 备份结果摘要
#[derive(Debug, Default)]
pub struct BackupSummary {
    pub uploaded: usize,
    pub uploaded_bytes: u64,
    pub deleted: usize,
    pub unchanged: usize,
    /// 保留策略清理的孤儿文件数
    pub orphan_removed: usize,
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
