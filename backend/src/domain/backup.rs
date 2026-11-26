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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    FileDescriptor, PlanSession, ProgressCb, SourceStorage, StorageError, TargetStorage,
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
    /// **共用同一「目标 + 目标文件夹」的其它任务的 `(job_id, 源文件夹名)`**
    ///
    /// 保留策略扫描的是**整个** `/<target_folder>` 前缀，而这些任务的文件与本次
    /// 备份的落在**同一棵树**里（ADR-014 允许一个目标被多个任务共用）。
    /// 若受管理集合只算本任务，就会把别的任务刚备份的文件判为孤儿删除 ——
    /// 且对方快照仍认为文件存在，不会重传，属**静默数据丢失**。
    /// 详见 [`RetentionPolicy`] 与 `AppConfig::tasks_sharing_target_folder`。
    pub sibling_jobs: Vec<(String, String)>,
    /// 严格模式：mtime/size 变化时再用 BLAKE3 内容哈希确认（ADR-004）
    pub strict: bool,
    /// 密文暂存目录（上传前落盘用；`None` 时用内存缓冲）
    ///
    /// 大文件流式加密需要一个「可回读」的密文落脚点才能交给目标适配器
    /// （`TargetStorage::write_stream` 要求已知长度的流，且 PUT 失败要能重传，
    /// 因此密文不能只存在内存里随读随弃）。给 `Some(dir)` 时密文件写到该目录的
    /// 临时文件，内存占用恒定为一个 64MB chunk；`None` 时退化为内存缓冲
    /// （仅测试/小文件用）。
    pub staging_dir: Option<std::path::PathBuf>,
    /// 取消标志（可选）：置位后**在下一个文件边界**停止
    ///
    /// 为什么不在文件中间停：单文件的传输在 FFI 里推进（`write_chunk` 循环），
    /// 从外部打断会让插件留下半截分片；而「文件之间」停是安全的 ——
    /// 已完成的文件快照已落盘，未开始的文件原样留着，下次备份接着传。
    pub cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
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

        let meter = Arc::new(Mutex::new(SpeedMeter::new()));
        let started = Instant::now();

        // 0) 准备阶段：先推一条事件，让面板在「扫描 / 差分」期间也有明确反馈
        self.publish(
            &self.job_id,
            crate::eventbus::TaskStatus::Started,
            Some(crate::eventbus::TaskPhase::Prepare),
            None,
            0,
            0,
            0,
            0,
            0,
            0,
            Some("准备中：扫描源目录并与上次快照比对，计算本次增量…".to_string()),
        );

        // 1) 预扫描 + 差分：汇总总文件数与总字节数（明文）
        let mut prepared: Vec<Prepared> = Vec::new();
        let mut grand_total_files: u64 = 0;
        let mut grand_total_bytes: u64 = 0;
        for (i, root) in roots.iter().enumerate() {
            let job_id = format!("{}-{}", self.job_id, i);
            let source: Arc<dyn SourceStorage> =
                Arc::new(crate::infra::source::local::LocalFsSource::new(root));
            // 严格模式（ADR-004）：先算 BLAKE3 内容哈希，mtime/size 变化时用哈希确认，
            // 防止 mtime 被改写回旧值导致内容变化被发现不了。
            let current = if self.strict {
                scan_all_strict(&*source, root).await?
            } else {
                scan_all(&*source, root).await?
            };
            let last = self.store.load_snapshot(&job_id, self.account())?;
            let changeset = if self.strict {
                let root_owned = root.clone();
                let hasher = move |fd: &FileDescriptor| -> Option<String> {
                    if fd.is_dir {
                        return None;
                    }
                    let path = root_owned.join(&fd.rel_path);
                    let data = std::fs::read(&path).ok()?;
                    Some(SyncSession::blake3_hex(&data))
                };
                SyncSession::diff_strict(&current, &last, hasher)
            } else {
                SyncSession::diff(&current, &last)
            };
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

        // 准备完成 → 进入传输阶段（带总数，前端进度条从 0/N 起走）
        self.publish(
            &self.job_id,
            crate::eventbus::TaskStatus::Progress,
            Some(crate::eventbus::TaskPhase::Transfer),
            None,
            0,
            grand_total_files,
            0,
            grand_total_bytes,
            started.elapsed().as_millis() as u64,
            0,
            None,
        );

        let mut summary = BackupSummary::default();
        let mut job_ids: Vec<String> = Vec::new();

        for p in &prepared {
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
                // 建目录失败不立即中止：空目录本身不是必需（文件上传会逐级 MKCOL），
                // 但**必须留下明确的警告** —— 否则用户只会看到 N 个「上传失败」，
                // 完全不知道根因是目录建不出来（权限/配额/路径非法）。
                if let Err(e) = self.target.ensure_dir(&root_path).await {
                    tracing::warn!(
                        path = %root_path.display(),
                        err = %e,
                        "创建目标根目录失败，后续上传可能逐个失败（这才是根因）"
                    );
                }
            }
            for dir in p.current.iter().filter(|e| e.is_dir) {
                if covered.contains(&dir.rel_path) {
                    continue; // 会被文件上传时的父目录创建覆盖
                }
                let tp = target_path_with(p.target_prefix.as_deref(), &dir.rel_path);
                if let Err(e) = self.target.ensure_dir(&tp).await {
                    tracing::warn!(
                        path = %tp.display(), err = %e,
                        "创建目标空目录失败（该目录可能不会出现在云端）"
                    );
                }
            }

            // 2) 上传清单在循环**之后**统一处理（见下方 `upload_all`）：
            //    这样目标能拿到跨所有源的完整清单，做全局并发回传规划。
        }

        // 2) 上传新增/修改文件（顺序 or 并发回传；断点续传：每上传完一个即时存快照）
        let ctx = Arc::new(UploadCtx {
            shared: Arc::new(Mutex::new((0u64, 0u64))),
            total_files: grand_total_files,
            total_bytes: grand_total_bytes,
            meter,
            started,
            files: AtomicUsize::new(0),
            bytes: AtomicU64::new(0),
        });
        let items: Vec<UploadItem> = prepared
            .iter()
            .flat_map(|p| {
                p.changeset
                    .upload
                    .iter()
                    .filter(|fd| !fd.is_dir)
                    .map(move |fd| UploadItem {
                        source: p.source.clone(),
                        job_id: p.job_id.clone(),
                        prefix: p.target_prefix.clone(),
                        fd: fd.clone(),
                        target_rel: target_path_with(p.target_prefix.as_deref(), &fd.rel_path)
                            .to_string_lossy()
                            .into_owned(),
                    })
            })
            .collect();
        self.upload_all(items, &ctx, &self.job_id).await?;
        let done = ctx.files.load(Ordering::Relaxed) as u64;
        let bytes_done = ctx.bytes.load(Ordering::Relaxed);
        summary.uploaded = ctx.files.load(Ordering::Relaxed);
        summary.uploaded_bytes = bytes_done;

        // 3) 收尾阶段：删除云端多余文件 → 落盘快照 → 保留策略清理孤儿
        //    （单独切到 cleanup 阶段，面板显示「收尾/清理」而不是一直停在传输）
        self.publish(
            &self.job_id,
            crate::eventbus::TaskStatus::Progress,
            Some(crate::eventbus::TaskPhase::Cleanup),
            None,
            done,
            grand_total_files,
            bytes_done,
            grand_total_bytes,
            started.elapsed().as_millis() as u64,
            ctx.meter.lock().unwrap().speed_bps(),
            Some("收尾：清理云端多余文件并保存快照…".to_string()),
        );

        for p in &prepared {
            // 删除云端已不存在的文件
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

            // 保存该路径的新快照
            let snapshot: Vec<SnapshotEntry> =
                p.current.iter().map(SnapshotEntry::from_fd).collect();
            self.store
                .save_snapshot(&p.job_id, self.account(), &snapshot)?;
            summary.unchanged += p.changeset.unchanged;
            job_ids.push(p.job_id.clone());
        }

        // 保留策略：清理目标端孤儿文件（属于收尾阶段）
        if let Some(rt) = &self.retention {
            // 受管理路径必须带上「源文件夹名」前缀：目标端布局是
            // /<target_prefix>/<源文件夹名>/<rel>，只拼 rel 会把刚备份完的
            // 文件全部误判为孤儿（曾导致「备份完就被清空」）。
            let mut jobs: Vec<(String, String)> = job_ids
                .iter()
                .zip(roots.iter())
                .map(|(id, r)| {
                    let name = r
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    (id.clone(), name)
                })
                .collect();
            // 共用「同一目标 + 同一目标文件夹」的**其它任务**的快照也必须算受管理：
            // 它们在云端与本次备份处在同一棵树里，漏掉就会把对方刚备份的文件
            // 当孤儿删掉，而对方快照仍认为文件存在、不会重传 ⇒ 静默数据丢失。
            // 见 AppConfig::tasks_sharing_target_folder 的说明。
            if !self.sibling_jobs.is_empty() {
                info!(
                    siblings = self.sibling_jobs.len(),
                    "保留策略：并入共用同一目标目录的其它任务快照"
                );
                jobs.extend(self.sibling_jobs.iter().cloned());
            }
            let managed = self.managed_paths(&jobs)?;
            // **保留策略失败不应让整次备份失败**：数据此刻已全部上传成功并落盘快照，
            // 只因「清理孤儿」这一步列目录超时/网络抖动就报整单失败，会让用户以为
            // 备份没做成而反复重跑（重复上传）。孤儿清理是可选的卫生工作，
            // 失败就记告警并跳过，下次备份再清。
            let report = match rt.cleanup_unmanaged(&managed).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        err = %e,
                        "保留策略清理失败（已跳过，不影响本次备份结果；下次备份会再尝试）"
                    );
                    crate::domain::retention::CleanupReport::default()
                }
            };
            summary.orphan_removed += report.removed;
            info!(
                removed = report.removed,
                failed = report.failed,
                scanned = report.scanned_files,
                "保留策略：孤儿文件清理完成"
            );
            self.publish(
                &self.job_id,
                crate::eventbus::TaskStatus::Progress,
                Some(crate::eventbus::TaskPhase::Cleanup),
                None,
                done,
                grand_total_files,
                bytes_done,
                grand_total_bytes,
                started.elapsed().as_millis() as u64,
                0,
                Some(format!("保留策略：已清理 {} 个云端孤儿文件", report.removed)),
            );
        }

        self.publish(
            &self.job_id,
            crate::eventbus::TaskStatus::Completed,
            None,
            None,
            done,
            grand_total_files,
            bytes_done,
            grand_total_bytes,
            started.elapsed().as_millis() as u64,
            ctx.meter.lock().unwrap().speed_bps(),
            None,
        );

        Ok(summary)
    }

    /// 汇总多个 job 的快照，构造受管理的相对路径集合（用于保留策略）
    ///
    /// 目标端布局为 `/<target_prefix>/<源文件夹名>/<rel>`（与 run_multi 的
    /// `join_root_prefix` 对应），因此受管理集合必须带**源文件夹名**前缀；
    /// 只拼 rel 会让保留策略把刚备份的文件全部判为孤儿并删除。
    fn managed_paths(&self, jobs: &[(String, String)]) -> Result<std::collections::HashSet<String>> {
        let mut set = std::collections::HashSet::new();
        for (job_id, root_name) in jobs {
            let prefix = if root_name.is_empty() {
                String::new()
            } else {
                format!("{}/", root_name)
            };
            for entry in self.store.load_snapshot(job_id, self.account())? {
                if !entry.is_dir {
                    set.insert(format!("{}{}", prefix, entry.rel_path));
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
        let meter = Arc::new(Mutex::new(SpeedMeter::new()));
        let started = Instant::now();

        // 0) 准备阶段：先推一条事件，让面板在「扫描 / 差分」期间也有明确反馈
        self.publish(
            job_id,
            crate::eventbus::TaskStatus::Started,
            Some(crate::eventbus::TaskPhase::Prepare),
            None,
            0,
            0,
            0,
            0,
            0,
            0,
            Some("准备中：扫描源目录并与上次快照比对，计算本次增量…".to_string()),
        );

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
        // 准备完成 → 进入传输阶段
        self.publish(
            job_id,
            crate::eventbus::TaskStatus::Progress,
            Some(crate::eventbus::TaskPhase::Transfer),
            None,
            0,
            total_upload,
            0,
            total_bytes,
            started.elapsed().as_millis() as u64,
            0,
            None,
        );

        let ctx = Arc::new(UploadCtx {
            shared: Arc::new(Mutex::new((0u64, 0u64))),
            total_files: total_upload,
            total_bytes,
            meter,
            started,
            files: AtomicUsize::new(0),
            bytes: AtomicU64::new(0),
        });
        let items: Vec<UploadItem> = changeset
            .upload
            .iter()
            .filter(|fd| !fd.is_dir)
            .map(|fd| UploadItem {
                source: self.source.clone(),
                job_id: job_id.to_string(),
                prefix: self.target_prefix.clone(),
                fd: fd.clone(),
                target_rel: target_path_with(self.target_prefix.as_deref(), &fd.rel_path)
                    .to_string_lossy()
                    .into_owned(),
            })
            .collect();
        self.upload_all(items, &ctx, job_id).await?;
        let uploaded = ctx.files.load(Ordering::Relaxed);
        let uploaded_bytes = ctx.bytes.load(Ordering::Relaxed);

        // 4) 收尾阶段：删除云端多余文件 → 保存快照
        self.publish(
            job_id,
            crate::eventbus::TaskStatus::Progress,
            Some(crate::eventbus::TaskPhase::Cleanup),
            None,
            uploaded as u64,
            total_upload,
            uploaded_bytes,
            total_bytes,
            started.elapsed().as_millis() as u64,
            ctx.meter.lock().unwrap().speed_bps(),
            Some("收尾：清理云端多余文件并保存快照…".to_string()),
        );

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
            None,
            uploaded as u64,
            total_upload,
            uploaded_bytes,
            total_bytes,
            started.elapsed().as_millis() as u64,
            ctx.meter.lock().unwrap().speed_bps(),
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
    ///
    /// 并发安全：完成数/已完成字节从共享计数器 `shared` 现读（多路并发时
    /// 基准始终是「已完成的合计」，而不是创建回调那一刻的旧值）；
    /// 速度计量用每文件独立的 [`FileMeter`] 基线汇入共享 [`SpeedMeter`]。
    #[allow(clippy::too_many_arguments)]
    fn plain_progress_cb(
        &self,
        job_id: &str,
        file: &str,
        shared: Arc<Mutex<(u64, u64)>>,
        total_files: u64,
        total_plain: u64,
        file_plain: u64,
        meter: Arc<Mutex<SpeedMeter>>,
        started: Instant,
    ) -> PlainProgressCb {
        let eb = self.eventbus.clone();
        let job_id = job_id.to_string();
        let file = file.to_string();
        let file_meter = Arc::new(Mutex::new(SpeedMeter::begin_file()));
        Arc::new(move |plain_in_file: u64, written: u64, req_elapsed_ms: u64| {
            let speed = {
                let mut m = meter.lock().unwrap();
                file_meter
                    .lock()
                    .unwrap()
                    .observe(&mut m, written, req_elapsed_ms);
                m.speed_bps()
            };
            let (done, base) = {
                let s = shared.lock().unwrap();
                (s.0, s.1)
            };
            if let Some(eb) = &eb {
                eb.task_event(
                    crate::eventbus::TaskKind::Backup,
                    crate::eventbus::TaskStatus::Progress,
                    Some(crate::eventbus::TaskPhase::Transfer),
                    job_id.clone(),
                    Some(file.clone()),
                    done,
                    total_files,
                    base + plain_in_file.min(file_plain),
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
        phase: Option<crate::eventbus::TaskPhase>,
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
                phase,
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

    /// 上传单个文件：源流 → age 分块加密 → 目标（可选按块上报明文进度）
    ///
    /// ## 内存口径（关键）
    /// 明文按 [`CHUNK_SIZE`]（64MB）分块，**每满一块就加密并落暂存**，因此峰值内存
    /// 恒定为一个 chunk 的量级，**与文件大小无关**。
    ///
    /// 早期实现先把整个源文件读进 `Vec<u8>` 再整体加密，峰值 ≈ 2× 文件大小
    /// （明文 + 密文同时驻留）；备份 8GB 视频需要约 16GB 内存，在 NAS 上必然 OOM，
    /// 也与文档「全程流式、内存仅当前 chunk」的承诺不符。
    ///
    /// 密文必须落**暂存文件**而不是随产随传：`TargetStorage::write_stream` 的失败
    /// 重试需要能重读 body，且（WebDAV）PUT 要带精确 `Content-Length`。
    /// 暂存目录由 [`BackupJob::staging_dir`] 提供（`$TRIM_PKGTMP/staging`）。
    async fn upload_one(
        &self,
        source: &dyn SourceStorage,
        rel_path: &str,
        prefix: Option<&str>,
        progress: Option<PlainProgressCb>,
    ) -> Result<u64> {
        let src = PathBuf::from(rel_path);
        let mut stream = source.read_stream(&src).await?;

        // 1) 源流 → 逐 chunk 加密 → 暂存（内存恒定一个 chunk）
        let crypto = self.crypto.clone();
        let staging_dir = self.staging_dir.clone();
        let (cipher_path, plain_len, enc_len) =
            encrypt_stream_to_staging(&mut stream, &crypto, staging_dir.as_deref()).await?;

        // 2) 暂存密文 → 流式交给目标；进度按密文字节比例换算为明文增量
        //
        // ⚠️ 必须走 `write_stream_sized`：`enc_len` 就是密文总长，目标端（WebDAV PUT）
        // 靠它才能带精确 `Content-Length` 边收边发。少传这一个数字，插件就只能把整份
        // 密文先攒下来（旧实现攒到临时文件，正是 P1-11 与沙箱冲突的根源）。
        let enc_total = enc_len;
        let plain_total = plain_len;
        let target_path = target_path_with(prefix, rel_path);
        let adapter: ProgressCb = {
            let outer = progress.clone();
            Arc::new(move |written: u64, _total: u64, elapsed_ms: u64| {
                let plain_in_file = if enc_total > 0 {
                    ((written.min(enc_total) as f64 / enc_total as f64) * plain_total as f64) as u64
                } else {
                    plain_total
                };
                if let Some(outer) = outer.as_ref() {
                    outer(plain_in_file, written, elapsed_ms);
                }
            })
        };
        let s = read_file_stream(&cipher_path, 1024 * 1024)?;
        let result = self
            .target
            .write_stream_sized(&target_path, s, enc_len, adapter)
            .await;
        // 无论成功失败都清理暂存（失败时重试会重新生成）
        let _ = std::fs::remove_file(&cipher_path);
        result?;
        Ok(plain_len)
    }

    // ── 上传调度：顺序 or 并发回传 ────────────────────────────────────────

    /// 上传单个文件：重试 → 快照落盘 → 进度发布 → 计数累加（**并发安全**）
    async fn upload_one_task(
        &self,
        item: &UploadItem,
        ctx: &UploadCtx,
        event_job: &str,
    ) -> Result<u64> {
        let cb = self.plain_progress_cb(
            event_job,
            &item.fd.rel_path,
            ctx.shared.clone(),
            ctx.total_files,
            ctx.total_bytes,
            item.fd.size,
            ctx.meter.clone(),
            ctx.started,
        );
        // 网络类失败自动重试（最多 3 次，线性退避；认证/不存在类不重试）
        let plain_n = {
            let mut out: Result<u64, anyhow::Error> = Err(anyhow::anyhow!("上传未执行"));
            for attempt in 1..=crate::domain::NETWORK_RETRY_ATTEMPTS {
                out = self
                    .upload_one(
                        &*item.source,
                        &item.fd.rel_path,
                        item.prefix.as_deref(),
                        Some(cb.clone()),
                    )
                    .await;
                match &out {
                    Ok(_) => break,
                    Err(e) => {
                        tracing::warn!(
                            "上传 {} 第 {attempt}/{} 次失败: {e:#}",
                            item.fd.rel_path,
                            crate::domain::NETWORK_RETRY_ATTEMPTS
                        );
                        if !crate::domain::is_retryable_err(e)
                            || attempt == crate::domain::NETWORK_RETRY_ATTEMPTS
                        {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64))
                            .await;
                    }
                }
            }
            out?
        };
        // 上传**完成后**才计入（进度回调现读 shared，基准恒为「已完成合计」）
        {
            let mut s = ctx.shared.lock().unwrap();
            s.0 += 1;
            s.1 += plain_n;
        }
        let done_files = ctx.files.fetch_add(1, Ordering::Relaxed) as u64 + 1;
        let done_bytes = ctx.bytes.fetch_add(plain_n, Ordering::Relaxed) + plain_n;
        self.store
            .save_entry(&item.job_id, self.account(), &SnapshotEntry::from_fd(&item.fd))?;
        self.publish(
            event_job,
            crate::eventbus::TaskStatus::Progress,
            Some(crate::eventbus::TaskPhase::Transfer),
            Some(item.fd.rel_path.clone()),
            done_files,
            ctx.total_files,
            done_bytes,
            ctx.total_bytes,
            ctx.started.elapsed().as_millis() as u64,
            ctx.meter.lock().unwrap().speed_bps(),
            None,
        );
        info!("已上传: {} ({plain_n} B)", item.fd.rel_path);
        Ok(plain_n)
    }

    /// 上传一批文件：目标**支持并发回传**时按其分批并发推送，否则顺序上传
    ///
    /// 并发回传下「传哪些、一次传几批」由目标决定（见 [`PlanUpload`]），宿主只按批
    /// 并发推送；目标不支持或规划失败时自动回退顺序，行为与旧版完全一致。
    async fn upload_all(
        &self,
        items: Vec<UploadItem>,
        ctx: &UploadCtx,
        event_job: &str,
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        // 清单用**目标端路径**作键：多源备份时源内相对路径会重名，
        // 目标端路径才是唯一键；插件原样返回后宿主再反查。
        let manifest: Vec<FileDescriptor> = items
            .iter()
            .map(|it| FileDescriptor {
                rel_path: it.target_rel.clone(),
                size: it.fd.size,
                modified: it.fd.modified,
                is_dir: false,
                digest: None,
            })
            .collect();

        let mut planned: Option<Box<dyn PlanSession>> = None;
        let mut parallel = 0usize;
        if let Some(p) = self.target.plan_upload() {
            match p.begin(&manifest) {
                Ok(s) => {
                    parallel = p.max_parallel().max(1);
                    planned = Some(s);
                    info!(
                        parallel,
                        count = manifest.len(),
                        "目标支持并发回传：已提交待传清单"
                    );
                }
                Err(e) => tracing::warn!("并发回传规划失败，回退顺序上传: {e:#}"),
            }
        }

        // 取消检查（文件边界）：返回 true 表示用户已请求取消
        //
        // 放在这里而不是文件传输中途：单文件的 `write_chunk` 循环在 FFI 里推进，
        // 从外部打断会让插件留下半截分片；而文件边界停下是安全的 ——
        // 已完成的文件快照已落盘，下次备份接着传。
        macro_rules! cancelled {
            () => {
                self.cancel
                    .as_ref()
                    .map(|c| c.load(Ordering::Relaxed))
                    .unwrap_or(false)
            };
        }

        let Some(mut session) = planned else {
            // 顺序模式（默认）：失败即终止，与旧版一致
            for it in &items {
                if cancelled!() {
                    tracing::warn!(job = %event_job, "收到取消请求，在文件边界停止备份");
                    return Err(anyhow::anyhow!("任务已被用户取消"));
                }
                self.upload_one_task(it, ctx, event_job).await?;
            }
            return Ok(());
        };

        // 并发模式：按目标发放的批次并发推送（并发度由目标声明）
        let by_target: std::collections::HashMap<&str, usize> = items
            .iter()
            .enumerate()
            .map(|(i, it)| (it.target_rel.as_str(), i))
            .collect();
        loop {
            if self
                .cancel
                .as_ref()
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(false)
            {
                tracing::warn!(job = %event_job, "收到取消请求，在批次边界停止备份");
                return Err(anyhow::anyhow!("任务已被用户取消"));
            }
            let batch = match session.next_batch() {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("取下一批失败，结束并发回传: {e:#}");
                    break;
                }
            };
            if batch.is_empty() {
                break; // 清单已发完
            }
            let picked: Vec<usize> = batch
                .iter()
                .filter_map(|rp| by_target.get(rp.as_str()).copied())
                .collect();
            if picked.is_empty() {
                continue;
            }
            // 先构造好 future 再交给 buffer_unordered：既能限制并发度，
            // 又避开闭包泛型的高阶生命周期推断问题
            let items_ref = &items;
            let mut futs = Vec::with_capacity(picked.len());
            for i in picked {
                futs.push(async move { self.upload_one_task(&items_ref[i], ctx, event_job).await });
            }
            let results = futures::stream::iter(futs)
                .buffer_unordered(parallel)
                .collect::<Vec<Result<u64>>>()
                .await;
            // 与顺序模式保持同一失败语义：任一文件失败即终止本次备份
            if let Some(e) = results.into_iter().find_map(|r| r.err()) {
                return Err(e);
            }
        }
        Ok(())
    }
}

/// 上传阶段共享上下文（**并发安全**）：完成计数 + 进度基准 + 速度计量
struct UploadCtx {
    /// 进度基准 `(已完成文件数, 已完成明文字节)`，由进度回调现读
    shared: Arc<Mutex<(u64, u64)>>,
    total_files: u64,
    total_bytes: u64,
    meter: Arc<Mutex<SpeedMeter>>,
    started: Instant,
    files: AtomicUsize,
    bytes: AtomicU64,
}

/// 一个待传文件及其所属子任务上下文
///
/// 字段全部**自有**（不带生命周期）：借用版会让传给 `stream::iter(...).map(..)` 的
/// 闭包触发高阶生命周期（HRTB）推断失败（`FnOnce is not general enough`）。
struct UploadItem {
    /// 源存储（多路径时每个源一份）
    source: Arc<dyn SourceStorage>,
    /// 快照归属的子任务 id（多路径时为 `{job_id}-{序号}`）
    job_id: String,
    /// 目标端前缀
    prefix: Option<String>,
    fd: FileDescriptor,
    /// 目标端相对路径（并发回传清单的唯一键）
    target_rel: String,
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

// ── 流式加密与暂存（P0-2：消除「整文件进内存」）────────────────────────────

/// 密文流向：优先应用暂存目录，未配置时退回系统临时目录
enum CipherSink {
    File(std::fs::File, PathBuf),
}

/// 把源流**逐 chunk 加密**并写入暂存文件，返回 `(密文路径, 明文长度, 密文长度)`
///
/// ## 为什么是「读满一块就加密」而不是整体加密
/// `CryptoSession::encrypt_stream` 本身流式，但它要 `impl Read`，而源侧给的是
/// async `Stream`。早期实现为了迁就同步接口，先把整个源文件读进 `Vec<u8>` ——
/// 峰值内存 ≈ 明文 + 密文 = **2× 文件大小**，8GB 文件要 16GB 内存，NAS 上必然 OOM。
///
/// 这里按 [`crate::domain::crypto::CHUNK_SIZE`] 累积明文，**满一块就交 `encrypt_chunk`**
/// （age 加密是 CPU 密集的同步操作，放进 `spawn_blocking` 不阻塞 runtime），
/// 密文立刻写进暂存文件后释放该块明文 ⇒ 峰值内存恒定为一个 chunk 的量级。
///
/// ## 为什么密文要落盘而不是随产随传
/// `TargetStorage::write_stream` 的失败重试需要能重读 body，且 WebDAV 的 PUT
/// 要带精确 `Content-Length`；一次性流无法回退。
///
/// ## 为什么优先 `staging_dir`（`$TRIM_PKGTMP/staging`）
/// 飞牛应用有自己规划的临时目录；系统 `/tmp` 可能是 tmpfs（吃内存）或被挤占。
/// 未配置时退回系统临时目录（仅供测试/小文件）。
pub(crate) async fn encrypt_stream_to_staging(
    stream: &mut (dyn futures::Stream<Item = Result<Bytes, StorageError>> + Send + Unpin),
    crypto: &CryptoSession,
    staging_dir: Option<&std::path::Path>,
) -> Result<(PathBuf, u64, u64)> {
    const CHUNK: usize = crate::domain::crypto::CHUNK_SIZE;

    // 1) 建暂存文件
    let (mut file, path) = match staging_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .map_err(|e| anyhow::anyhow!("创建密文暂存目录失败 {}: {e}", dir.display()))?;
            let name = format!(
                "kzwr-{}-{}.age",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            let path = dir.join(name);
            let f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| anyhow::anyhow!("创建密文暂存文件失败 {}: {e}", path.display()))?;
            (f, path)
        }
        None => {
            let tf = tempfile::NamedTempFile::new()
                .map_err(|e| anyhow::anyhow!("创建临时密文文件失败: {e}"))?;
            // keep()：后续要按路径回读，不能让析构把文件删掉
            let (f, path) = tf
                .keep()
                .map_err(|e| anyhow::anyhow!("保留临时密文文件失败: {:?}", e))?;
            (f, path)
        }
    };

    // 2) 逐块加密落盘（**流水线并行，但严格保序**）
    //
    // ## 为什么不是「读一块 → 加密一块 → 写一块」
    // age 加密是纯 CPU 密集（每块一次 X25519 + ChaCha20-Poly1305），
    // 串行执行时源盘读取与磁盘写入的等待全被浪费，且只用一个核。
    //
    // ## 为什么要保序
    // 密文是 `[u32 长度][age 密文]` 的**有序拼接**，解密按顺序逐块读。
    // 若多个块并行加密后按「谁先完成谁先写」，块顺序就会乱 —— 解密时会用
    // 错误的长度前缀去切分，直接失败（或更糟：解出乱码而不报错）。
    //
    // ## 实现
    // 并行加密 + 按**序号**回收：每块带一个递增 seq，写盘侧维护「下一个期望序号」，
    // 已完成的乱序块先放进小缓存（BTreeMap，按 seq 排序），等齐了再连续写出。
    // 这样既拿到多核加速，又保证字节顺序与原串行实现完全一致。
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(usize, Vec<u8>)>(ENC_PIPELINE_DEPTH);

    // 写盘放在**独立 blocking 线程**上：实测 async 写 + channel 比同步 BufWriter
    // 慢近 30%（303ms vs 391ms / 512MB），因为每块都要跨一次调度。
    let writer = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut file = std::io::BufWriter::with_capacity(8 * 1024 * 1024, file);
        let mut enc_total: u64 = 0;
        let mut out: Result<()> = Ok(());
        let mut next_seq: usize = 0;
        // 乱序到达的块：seq → 密文（只在并行度范围内，很小）
        let mut pending: std::collections::BTreeMap<usize, Vec<u8>> =
            std::collections::BTreeMap::new();
        while let Some((seq, enc)) = rx.blocking_recv() {
            pending.insert(seq, enc);
            // 连续写出所有「轮到的」块
            while let Some(data) = pending.remove(&next_seq) {
                let len = (data.len() as u32).to_le_bytes();
                if let Err(e) = file.write_all(&len).and_then(|_| file.write_all(&data)) {
                    out = Err(anyhow::anyhow!("写入密文暂存失败: {e}"));
                    break;
                }
                enc_total += 4 + data.len() as u64;
                next_seq += 1;
            }
            if out.is_err() {
                break;
            }
        }
        if out.is_ok() {
            if !pending.is_empty() {
                // 队列已关闭但还有缺口 ⇒ 中间某块加密失败，绝不能写出残缺密文
                out = Err(anyhow::anyhow!(
                    "加密流水线中断：缺少 {} 个分块，已放弃该文件",
                    pending.len()
                ));
            } else if let Err(e) = file.flush() {
                out = Err(anyhow::anyhow!("刷新密文暂存失败: {e}"));
            } else if let Err(e) = file.get_ref().sync_all() {
                // 刷盘失败（磁盘满/IO 错误）必须报错：否则上层以为备份成功
                out = Err(anyhow::anyhow!("密文写入磁盘失败: {e}"));
            }
        }
        (out, enc_total)
    });

    // ## 缓冲策略（这是吞吐的关键）
    // 早期写法用 `split_off(CHUNK)` + `mem::replace` 把整块「切」出来，
    // 每次都要**搬动整个 64MB 尾部**；实测这让流水线比串行还慢 7 倍。
    // 现在改为「攒满即移交」：`plain_buf` 恰好攒到 CHUNK 就把整个 Vec 的所有权
    // 交给加密任务（`mem::take` 只需换一个空 Vec，零拷贝），下一块用新 Vec 重新攒。
    // 单次 `extend_from_slice` 只复制源数据本身，没有额外的整块搬运。
    let mut plain_buf: Vec<u8> = Vec::with_capacity(CHUNK);
    let mut plain_total: u64 = 0;
    let mut seq: usize = 0;
    // 加密失败的传播通道：任一块失败就把原因记在这里，主循环据此中止并上抛
    let enc_fail: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // 直接 spawn_blocking（**不再套一层 tokio::spawn**）：后者只是把一个已经很短的
    // 任务再调度一次，实测带来可观的额外开销，而没有任何收益。
    let mut inflight: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        plain_total += chunk.len() as u64;
        let mut src = &chunk[..];
        while !src.is_empty() {
            let need = CHUNK - plain_buf.len();
            let take = need.min(src.len());
            plain_buf.extend_from_slice(&src[..take]);
            src = &src[take..];
            // 攒满一块：整体移交（`mem::take` 零拷贝）
            if plain_buf.len() == CHUNK {
                let block = std::mem::take(&mut plain_buf);
                let crypto = crypto.clone();
                let tx2 = tx.clone();
                let fail = enc_fail.clone();
                let my_seq = seq;
                seq += 1;
                inflight.push(tokio::task::spawn_blocking(move || {
                    match crypto.encrypt_chunk(&block) {
                        Ok(v) => {
                            // 用 blocking_send：本任务在 blocking 池线程上，
                            // 且发送是短暂操作，避免为一次 send 混入 async 调度
                            let _ = tx2.blocking_send((my_seq, v));
                        }
                        Err(e) => {
                            *fail.lock().unwrap() = Some(format!("{e:#}"));
                        }
                    }
                }));
                // 回收已完成的句柄，防止 `inflight` 无限增长（它只是句柄，很小）
                if inflight.len() >= 64 {
                    inflight.retain(|h| !h.is_finished());
                }
                if let Some(e) = enc_fail.lock().unwrap().clone() {
                    return Err(anyhow::anyhow!("加密失败: {e}"));
                }
            }
        }
    }

    // 收尾：不足一块的剩余明文（序号接在最后，顺序自然正确）
    if !plain_buf.is_empty() {
        let take = std::mem::take(&mut plain_buf);
        let crypto = crypto.clone();
        let enc = tokio::task::spawn_blocking(move || crypto.encrypt_chunk(&take))
            .await
            .map_err(|e| anyhow::anyhow!("加密任务异常终止: {e}"))??;
        tx.send((seq, enc))
            .await
            .map_err(|_| anyhow::anyhow!("密文写入任务已退出"))?;
        seq += 1;
    }
    // 等所有并行加密任务结束（它们持有 tx 的克隆，必须全部退出才能关闭 channel）
    for h in inflight {
        let _ = h.await;
    }
    drop(tx);
    let (write_res, enc_total) = writer
        .await
        .map_err(|e| anyhow::anyhow!("密文写入任务异常终止: {e}"))?;
    write_res?;
    if let Some(e) = enc_fail.lock().unwrap().clone() {
        return Err(anyhow::anyhow!("加密失败: {e}"));
    }
    let _ = seq;

    Ok((path, plain_total, enc_total))
}

/// 加密流水线里「已加密待写盘」的块数上限
///
/// 越大越能吸收源盘抖动，但内存 = CHUNK × 深度（64MB × N）。取 3 已足够
/// 盖住「写盘比加密慢」的波动，峰值约 192MB，对 NAS 可接受。
const ENC_PIPELINE_DEPTH: usize = 3;

/// 按 `chunk_size` 粒度把文件读成字节流
///
/// ⚠️ 旧实现是 `std::fs::read(path)` 一次性读全量再切片 —— 注释写着「避免一次性把
/// 整个密文读进内存」，实际做的恰好相反：8GB 备份在这里要多占 8GB RAM。
/// 现在改成 `tokio::fs::File` + `ReaderStream`：内存只占一个 `chunk_size` 块。
pub(crate) fn read_file_stream(
    path: &std::path::Path,
    chunk_size: usize,
) -> Result<Box<dyn futures::Stream<Item = Bytes> + Send + Unpin>> {
    let p = path.to_path_buf();
    // 打开失败要立刻报（而不是等流被消费时才炸），故用 std 先验证一次可读性
    std::fs::File::open(&p)
        .map_err(|e| anyhow::anyhow!("读取密文暂存文件失败 {}: {e}", p.display()))?;
    let stream = async_stream_file(p, chunk_size.max(1));
    Ok(Box::new(Box::pin(stream)))
}

/// 惰性打开文件并以 [`Bytes`] 块的形式产出（在流被轮询时才真正开始读）
fn async_stream_file(
    path: std::path::PathBuf,
    chunk_size: usize,
) -> impl futures::Stream<Item = Bytes> + Send {
    futures::stream::unfold(
        (path, None::<tokio::fs::File>, false),
        move |(path, file, done)| async move {
            if done {
                return None;
            }
            let mut f = match file {
                Some(f) => f,
                None => match tokio::fs::File::open(&path).await {
                    Ok(f) => f,
                    Err(e) => {
                        // 打开失败：以空流结束（前置的 std::fs::File::open 已把错误报出）
                        tracing::error!(path = %path.display(), err = %e, "打开密文流失败");
                        return None;
                    }
                },
            };
            use tokio::io::AsyncReadExt;
            let mut buf = vec![0u8; chunk_size];
            match f.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    Some((Bytes::from(buf), (path, Some(f), false)))
                }
                Err(e) => {
                    tracing::error!(path = %path.display(), err = %e, "读取密文流失败");
                    None
                }
            }
        },
    )
}

#[cfg(test)]
mod encrypt_pipeline_tests {
    use super::*;
    use crate::domain::crypto::AgeKeys;

    /// **顺序正确性（最关键）**：并行加密后，密文块顺序必须与原串行一致
    ///
    /// 密文格式是 `[u32 长度][age 密文]` 的有序拼接，解密按序逐块读。
    /// 若并行后按「完成先后」写盘，块顺序会乱 → 解密立刻失败。
    /// 这个测试用**多块大文件**（>3×CHUNK 强制并行）验证「加密→解密」字节级往返。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn parallel_encryption_preserves_block_order() {
        let keys = AgeKeys::generate();
        let crypto = crate::domain::crypto::CryptoSession::full(&keys);

        // 3.5 个 CHUNK：强制多块并行，且带一个不足整块的尾块
        let chunk = crate::domain::crypto::CHUNK_SIZE;
        let total = chunk * 3 + 12345;
        // 用可校验的伪随机内容（每字节可独立验算）
        let data: Vec<u8> = (0..total).map(|i| ((i * 31 + 7) % 251) as u8).collect();

        let stream = futures::stream::iter(
            data.chunks(1024 * 1024)
                .map(|c| Ok::<_, crate::infra::storage_trait::StorageError>(Bytes::copy_from_slice(c)))
                .collect::<Vec<_>>(),
        );
        let mut boxed: std::pin::Pin<Box<dyn futures::Stream<Item = _> + Send>> = Box::pin(stream);

        let dir = tempfile::tempdir().unwrap();
        let (path, plain_len, enc_len) =
            encrypt_stream_to_staging(&mut boxed, &crypto, Some(dir.path()))
                .await
                .expect("加密应成功");
        assert_eq!(plain_len, total as u64, "明文总长应一致");

        // 解密回来，必须与原数据逐字节相同
        let mut rdr = std::io::BufReader::new(std::fs::File::open(&path).unwrap());
        let mut out = Vec::new();
        crypto
            .decrypt_stream(&mut rdr, &mut out)
            .expect("解密应成功（顺序错了这里必然失败）");
        let _ = enc_len;
        assert_eq!(out.len(), data.len(), "解密后长度应一致");
        assert!(out == data, "解密内容必须与原数据逐字节相同（顺序/边界正确）");
    }

    /// **同口径**对比：串行（加密+写盘） vs 流水线（并行加密 与 写盘重叠）
    ///
    /// 早前版本拿「只加密」和「加密+写盘」比，得出「流水线慢 7 倍」的错误结论。
    /// 真实收益取决于两者谁更慢：
    /// - 加密慢（弱 CPU / 大 chunk）⇒ 并行收益明显；
    /// - 写盘慢（机械盘 / 大 fsync）⇒ 加密被完全隐藏，收益≈0 但**不应更慢**。
    ///
    /// 因此本基准只断言「流水线不显著慢于串行」，并把实测加速比打印出来。
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[ignore = "基准测试：cargo test --release -- --ignored --nocapture 手动运行"]
    async fn bench_pipelined_vs_serial_end_to_end() {
        let keys = AgeKeys::generate();
        let crypto = crate::domain::crypto::CryptoSession::full(&keys);
        let chunk = crate::domain::crypto::CHUNK_SIZE;
        let n = 8;
        let data = vec![0x7Bu8; chunk * n];

        let mk_stream = |d: &Vec<u8>| {
            let chunks: Vec<_> = d
                .chunks(chunk)
                .map(|c| Ok::<_, crate::infra::storage_trait::StorageError>(Bytes::copy_from_slice(c)))
                .collect();
            let s: std::pin::Pin<Box<dyn futures::Stream<Item = _> + Send>> =
                Box::pin(futures::stream::iter(chunks));
            s
        };

        // 串行基准：逐块加密后立刻写盘（模拟旧实现）
        let dir_a = tempfile::tempdir().unwrap();
        let path_a = dir_a.path().join("serial.age");
        let t = std::time::Instant::now();
        {
            use std::io::Write;
            let mut f = std::io::BufWriter::with_capacity(8 * 1024 * 1024,
                std::fs::File::create(&path_a).unwrap());
            for i in 0..n {
                let s = data[i * chunk..(i + 1) * chunk].to_vec();
                let c = crypto.clone();
                let enc = tokio::task::spawn_blocking(move || c.encrypt_chunk(&s))
                    .await.unwrap().unwrap();
                f.write_all(&(enc.len() as u32).to_le_bytes()).unwrap();
                f.write_all(&enc).unwrap();
            }
            f.flush().unwrap();
            f.get_ref().sync_all().ok();
        }
        let serial = t.elapsed();

        // 流水线：encrypt_stream_to_staging（并行加密 + 后台写盘）
        let dir_b = tempfile::tempdir().unwrap();
        let t = std::time::Instant::now();
        encrypt_stream_to_staging(&mut mk_stream(&data), &crypto, Some(dir_b.path()))
            .await
            .unwrap();
        let pipelined = t.elapsed();

        let ratio = serial.as_secs_f64() / pipelined.as_secs_f64();
        eprintln!(
            "[bench-e2e] 串行 {serial:?} / 流水线 {pipelined:?} ⇒ {ratio:.2}×（{} 核）",
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0)
        );
        // 不设硬门槛（磁盘波动可达数倍），只在明显退化时失败提示人工复核
        assert!(
            ratio > 0.5,
            "流水线比串行慢了一倍以上（{ratio:.2}×），需要复核：写盘是否被串行化？"
        );
    }

    /// 并行加密相对串行的加速比（信息性基准，release 下跑才有意义）
    ///
    /// 不算断言硬门槛（CI 机器负载波动大），但会把实测值打印出来，
    /// 便于确认「流水线确实并行」而不是退化成了串行。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "基准测试：cargo test --release -- --ignored --nocapture 手动运行"]
    async fn bench_parallel_vs_serial_encryption() {
        let keys = AgeKeys::generate();
        let crypto = crate::domain::crypto::CryptoSession::full(&keys);
        let chunk = crate::domain::crypto::CHUNK_SIZE;
        let n_blocks = 8; // 8 × 64MB = 512MB 明文
        let data = vec![0x7Bu8; chunk * n_blocks];

        // 串行：逐块 encrypt_chunk
        let t0 = std::time::Instant::now();
        for i in 0..n_blocks {
            let slice = data[i * chunk..(i + 1) * chunk].to_vec();
            let c = crypto.clone();
            tokio::task::spawn_blocking(move || c.encrypt_chunk(&slice))
                .await
                .unwrap()
                .unwrap();
        }
        let serial = t0.elapsed();

        // 并行：走 encrypt_stream_to_staging（内部流水线）
        let stream = futures::stream::iter(
            data.chunks(4 * 1024 * 1024)
                .map(|c| Ok::<_, crate::infra::storage_trait::StorageError>(Bytes::copy_from_slice(c)))
                .collect::<Vec<_>>(),
        );
        let mut boxed: std::pin::Pin<Box<dyn futures::Stream<Item = _> + Send>> = Box::pin(stream);
        let dir = tempfile::tempdir().unwrap();
        let t1 = std::time::Instant::now();
        encrypt_stream_to_staging(&mut boxed, &crypto, Some(dir.path()))
            .await
            .unwrap();
        let pipelined = t1.elapsed();

        let ratio = serial.as_secs_f64() / pipelined.as_secs_f64();
        eprintln!(
            "[bench] 串行 {serial:?} / 流水线 {pipelined:?} ⇒ 加速 {ratio:.2}× \
             （{} 核可用）",
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0)
        );
    }

    /// 尾块处理：总长恰好是 CHUNK 整数倍时，不能多写一个空块
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn exact_multiple_of_chunk_has_no_empty_tail_block() {
        let keys = AgeKeys::generate();
        let crypto = crate::domain::crypto::CryptoSession::full(&keys);
        let chunk = crate::domain::crypto::CHUNK_SIZE;
        let data = vec![0x42u8; chunk]; // 恰好 1 块

        let stream = futures::stream::iter(
            data.chunks(512 * 1024)
                .map(|c| Ok::<_, crate::infra::storage_trait::StorageError>(Bytes::copy_from_slice(c)))
                .collect::<Vec<_>>(),
        );
        let mut boxed: std::pin::Pin<Box<dyn futures::Stream<Item = _> + Send>> = Box::pin(stream);
        let dir = tempfile::tempdir().unwrap();
        let (path, plain_len, _) = encrypt_stream_to_staging(&mut boxed, &crypto, Some(dir.path()))
            .await
            .expect("加密应成功");
        assert_eq!(plain_len, chunk as u64);

        let mut rdr = std::io::BufReader::new(std::fs::File::open(&path).unwrap());
        let mut out = Vec::new();
        crypto.decrypt_stream(&mut rdr, &mut out).expect("解密应成功");
        assert_eq!(out.len(), chunk, "恰好一块时不应多出空块");
        assert!(out == data);
    }
}

#[cfg(test)]
mod cancel_tests {
    use super::*;
    use crate::domain::crypto::AgeKeys;
    use crate::infra::target::mock_dav::MockDav;
    use crate::plugin::api::TargetPlugin;
    use std::sync::atomic::AtomicBool;

    fn build_target(
        addr: std::net::SocketAddr,
    ) -> (tempfile::TempDir, Arc<dyn TargetStorage>) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = crate::infra::config::ConfigManager::new(
            dir.path(),
            age::secrecy::SecretString::from("t".to_string()),
        );
        let t = crate::infra::config::TargetConfig {
            id: "t".into(),
            name: "m".into(),
            kind: "webdav".into(),
            url: Some(format!("http://{addr}/dav")),
            username_enc: Some(mgr.encrypt_field("u").unwrap()),
            password_enc: Some(mgr.encrypt_field("p").unwrap()),
            enabled: true,
            parallel: Some(1),
            fields: Default::default(),
            use_proxy: false,
        };
        let plugin = crate::plugin::builtin::webdav_abi::WebdavAbiPlugin::new();
        let (storage, _) = plugin.build(&t, &mgr).unwrap();
        (dir, storage)
    }

    /// **取消必须真的让备份停下**（而不是只把按钮点亮）
    ///
    /// 用「开跑前就把取消标志置位」来验证：任务应在**第一个文件之前**就退出，
    /// 且目标端一个字节都不该收到（不会留下半截上传）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancel_before_start_stops_backup_without_uploading() {
        let mock = MockDav::start(false).await;
        let (_dir, target) = build_target(mock.addr);
        let keys = AgeKeys::generate();
        let crypto = crate::domain::crypto::CryptoSession::full(&keys);
        let store_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            crate::infra::persistence::snapshot::SnapshotStore::open(
                &store_dir.path().join("meta.db"),
            )
            .unwrap(),
        );
        // 造一个源目录（2 个文件）
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), b"aaaa").unwrap();
        std::fs::write(src.path().join("b.txt"), b"bbbb").unwrap();

        let cancel = Arc::new(AtomicBool::new(true)); // 开跑前就请求取消
        let job = BackupJob {
            job_id: "cancel-test".to_string(),
            account: None,
            source: Arc::new(crate::infra::source::local::LocalFsSource::new(src.path())),
            target: target.clone(),
            crypto,
            store,
            target_prefix: Some("fn-backup".to_string()),
            eventbus: None,
            retention: None,
            sibling_jobs: Vec::new(),
            strict: false,
            staging_dir: None,
            cancel: Some(cancel),
        };
        let out = job.run(src.path()).await;
        assert!(out.is_err(), "已请求取消时备份必须返回错误，而不是静默成功");
        let msg = format!("{:#}", out.unwrap_err());
        assert!(
            msg.contains("取消"),
            "错误信息应说明是被取消：{msg}"
        );
        assert_eq!(
            mock.total_bytes(),
            0,
            "取消后不该有任何字节上传到目标端"
        );
    }

    /// 对照组：不取消时必须正常完成（证明上面的测试确实在测取消）
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn without_cancel_backup_completes() {
        let mock = MockDav::start(false).await;
        let (_dir, target) = build_target(mock.addr);
        let keys = AgeKeys::generate();
        let crypto = crate::domain::crypto::CryptoSession::full(&keys);
        let store_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            crate::infra::persistence::snapshot::SnapshotStore::open(
                &store_dir.path().join("meta.db"),
            )
            .unwrap(),
        );
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), b"aaaa").unwrap();

        let job = BackupJob {
            job_id: "ok-test".to_string(),
            account: None,
            source: Arc::new(crate::infra::source::local::LocalFsSource::new(src.path())),
            target: target.clone(),
            crypto,
            store,
            target_prefix: Some("fn-backup".to_string()),
            eventbus: None,
            retention: None,
            sibling_jobs: Vec::new(),
            strict: false,
            staging_dir: None,
            cancel: Some(Arc::new(AtomicBool::new(false))),
        };
        let summary = job.run(src.path()).await.expect("不取消时应成功");
        assert_eq!(summary.uploaded, 1);
        assert!(mock.total_bytes() > 0, "应确实上传了数据");
    }
}
