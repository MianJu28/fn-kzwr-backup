//! 恢复编排（核心域：选择性恢复、完整性校验）
//!
//! 从目标存储读取加密数据 → age 解密 → 写回本地目录。
//! 支持选择性恢复（只恢复指定的文件）。
//!
//! 目标端布局：`/<target_prefix>/<source_root_name>/<rel>`，其中 `source_root_name`
//! 为备份源文件夹名（与备份侧 `run_multi` 的分目录策略对应）。
//!
//! 恢复后会**把本地文件 mtime 回写为快照中记录的原始 mtime**，使下次增量备份
//! 判定为「未变化」而跳过，避免恢复后又被重新上传。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use futures::StreamExt;
use tracing::info;

use crate::domain::crypto::CryptoSession;
use crate::domain::pace::SpeedMeter;
use crate::infra::persistence::snapshot::{SnapshotEntry, SnapshotStore};
use crate::infra::storage_trait::TargetStorage;

/// 恢复后需要同步的快照目标（用于把恢复结果写回快照，避免下次备份重复上传）
pub struct RestoreSnapshotTarget {
    pub store: Arc<SnapshotStore>,
    pub job_id: String,
    pub account: String,
}

/// 恢复任务
pub struct RestoreJob {
    /// 目标存储（读取加密数据）
    pub target: Arc<dyn TargetStorage>,
    /// 解密会话（含 age 私钥）
    pub crypto: CryptoSession,
    /// 目标根前缀（备份时写入的前缀，如 "fn-backup"），与备份对应
    pub target_prefix: Option<String>,
    /// 备份源文件夹名（备份时在目标前缀下多分的目录，如 "Photos"）
    pub source_root_name: Option<String>,
    /// 内部事件总线（进度推送，可选）
    pub eventbus: Option<Arc<crate::eventbus::EventBus>>,
    /// 快照元数据：rel_path -> (明文大小, 原始 mtime 秒)
    pub meta: HashMap<String, (u64, i64)>,
    /// 恢复结果写回快照的目标（仅在恢复到原备份位置时提供）
    pub snapshot_target: Option<RestoreSnapshotTarget>,
}

impl RestoreJob {
    /// 目标端根路径：`/<prefix>[/<source_root_name>]`
    fn base_path(&self) -> String {
        let mut segs: Vec<String> = Vec::new();
        if let Some(p) = &self.target_prefix {
            let p = p.trim_matches('/');
            if !p.is_empty() {
                segs.push(p.to_string());
            }
        }
        if let Some(r) = &self.source_root_name {
            let r = r.trim_matches('/');
            if !r.is_empty() {
                segs.push(r.to_string());
            }
        }
        if segs.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", segs.join("/"))
        }
    }

    /// 执行恢复
    ///
    /// - `rel_files`: 要恢复的文件相对路径列表（选择性恢复）。空列表 = 恢复全部已备份文件。
    /// - `restore_dir`: 本地恢复目标目录（明文写入位置）
    pub async fn run(&self, rel_files: &[String], restore_dir: &Path) -> Result<RestoreSummary> {
        std::fs::create_dir_all(restore_dir)?;

        let meter = Arc::new(Mutex::new(SpeedMeter::new()));
        let started = Instant::now();

        // 准备阶段：先推一条事件，避免「列取云端文件」期间面板毫无反馈
        self.publish(
            crate::eventbus::TaskStatus::Started,
            Some(crate::eventbus::TaskPhase::Prepare),
            None,
            0,
            0,
            0,
            0,
            started.elapsed().as_millis() as u64,
            0,
            Some("准备中：读取快照并列出云端待恢复文件…".to_string()),
        );

        let files_to_restore: Vec<String> = if rel_files.is_empty() {
            self.list_target_files().await?
        } else {
            rel_files.to_vec()
        };
        let total = files_to_restore.iter().filter(|r| !r.trim().is_empty()).count() as u64;
        // 待恢复的明文总大小（来自快照；未知则为 0）
        let total_bytes: u64 = files_to_restore
            .iter()
            .map(|r| self.meta.get(r).map(|(s, _)| *s).unwrap_or(0))
            .sum();

        // 准备完成 → 进入传输（下载）阶段
        self.publish(
            crate::eventbus::TaskStatus::Started,
            Some(crate::eventbus::TaskPhase::Transfer),
            None,
            0,
            total,
            0,
            total_bytes,
            started.elapsed().as_millis() as u64,
            0,
            None,
        );

        let mut restored = 0usize;
        let mut restored_bytes = 0u64;
        let mut missing: Vec<String> = Vec::new();
        // 顺序下载；共享计数器供进度回调读取基准（完成数含缺失跳过）
        let shared = Arc::new(Mutex::new((0u64, 0u64)));
        for rel in files_to_restore.into_iter().filter(|r| !r.trim().is_empty()) {
            let file_plain = self.meta.get(&rel).map(|(s, _)| *s).unwrap_or(0);
            // 文件内实时进度：按下载密文字节（≈明文）换算为明文已传量
            let fm = Arc::new(Mutex::new(SpeedMeter::begin_file()));
            let cb: Option<crate::infra::storage_trait::ProgressCb> = {
                let eb = self.eventbus.clone();
                let file = rel.clone();
                let meter = meter.clone();
                let shared = shared.clone();
                Some(std::sync::Arc::new(
                    move |enc_read: u64, _t: u64, elapsed_ms: u64| {
                        let speed = {
                            let mut m = meter.lock().unwrap();
                            fm.lock().unwrap().observe(&mut m, enc_read, elapsed_ms);
                            m.speed_bps()
                        };
                        let (done, base) = {
                            let s = shared.lock().unwrap();
                            (s.0, s.1)
                        };
                        let plain_in_file = if file_plain > 0 {
                            enc_read.min(file_plain)
                        } else {
                            enc_read
                        };
                        if let Some(eb) = &eb {
                            eb.task_event(
                                crate::eventbus::TaskKind::Restore,
                                crate::eventbus::TaskStatus::Progress,
                                Some(crate::eventbus::TaskPhase::Transfer),
                                "restore".to_string(),
                                Some(file.clone()),
                                done,
                                total,
                                base + plain_in_file,
                                total_bytes,
                                started.elapsed().as_millis() as u64,
                                speed,
                                None,
                            );
                        }
                    },
                ))
            };
            // 网络类失败自动重试（最多 3 次，线性退避）；
            // 云端文件不存在 → 标记为缺失并继续；其余错误照常中止
            let (n, is_missing) = {
                let mut out: Result<(u64, bool), anyhow::Error> =
                    Err(anyhow::anyhow!("恢复未执行"));
                for attempt in 1..=crate::domain::NETWORK_RETRY_ATTEMPTS {
                    out = match self.restore_one(&rel, restore_dir, cb.clone()).await {
                        Ok(n) => Ok((n, false)),
                        Err(e) => {
                            let not_found = e
                                .downcast_ref::<crate::infra::storage_trait::StorageError>()
                                .map(|se| {
                                    matches!(
                                        se,
                                        crate::infra::storage_trait::StorageError::NotFound(_)
                                    )
                                })
                                .unwrap_or(false);
                            if not_found {
                                tracing::warn!("云端文件不存在，恢复时跳过: {}", rel);
                                Ok((0u64, true))
                            } else {
                                Err(e)
                            }
                        }
                    };
                    match &out {
                        Ok(_) => break,
                        Err(e) => {
                            tracing::warn!(
                                "恢复 {} 第 {attempt}/{} 次失败: {e:#}",
                                rel,
                                crate::domain::NETWORK_RETRY_ATTEMPTS
                            );
                            if !crate::domain::is_retryable_err(e)
                                || attempt == crate::domain::NETWORK_RETRY_ATTEMPTS
                            {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(
                                500 * attempt as u64,
                            ))
                            .await;
                        }
                    }
                }
                out?
            };
            if is_missing {
                // 云端文件已被删除：跳过并记录，进度照常推进到 100%
                // 注意：进度完成数（done，含缺失跳过）与恢复成功数（restored）分开统计
                missing.push(rel.clone());
                let d = {
                    let mut s = shared.lock().unwrap();
                    s.0 += 1;
                    s.0
                };
                self.publish(
                    crate::eventbus::TaskStatus::Progress,
                    Some(crate::eventbus::TaskPhase::Transfer),
                    Some(rel.clone()),
                    d,
                    total,
                    restored_bytes,
                    total_bytes,
                    started.elapsed().as_millis() as u64,
                    meter.lock().unwrap().speed_bps(),
                    None,
                );
                continue;
            }
            let d = {
                let mut s = shared.lock().unwrap();
                s.0 += 1;
                s.1 += n;
                s.0
            };
            restored += 1;
            restored_bytes += n;
            self.publish(
                crate::eventbus::TaskStatus::Progress,
                Some(crate::eventbus::TaskPhase::Transfer),
                Some(rel.clone()),
                d,
                total,
                restored_bytes,
                total_bytes,
                started.elapsed().as_millis() as u64,
                meter.lock().unwrap().speed_bps(),
                None,
            );
            info!("已恢复: {} ({n} B)", rel);
        }

        // 有缺失文件时在完成事件中说明（任务面板显示提示，而不是永远「进行中」）
        let done_msg = if missing.is_empty() {
            None
        } else {
            Some(format!(
                "恢复完成；{} 个文件在云端已不存在，已跳过（可用「清理缺失记录」移除快照中的失效条目）",
                missing.len()
            ))
        };
        self.publish(
            crate::eventbus::TaskStatus::Completed,
            None,
            None,
            restored as u64,
            total,
            restored_bytes,
            total_bytes,
            started.elapsed().as_millis() as u64,
            meter.lock().unwrap().speed_bps(),
            done_msg,
        );

        Ok(RestoreSummary {
            restored,
            restored_bytes,
            missing,
        })
    }

    /// 发布进度事件（事件总线可选）
    #[allow(clippy::too_many_arguments)]
    fn publish(
        &self,
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
                crate::eventbus::TaskKind::Restore,
                status,
                phase,
                "restore".to_string(),
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

    /// 恢复单个文件：目标读密文 → age 解密 → 写本地（按 1MiB 粒度上报进度）
    async fn restore_one(
        &self,
        rel: &str,
        restore_dir: &Path,
        progress: Option<crate::infra::storage_trait::ProgressCb>,
    ) -> Result<u64> {
        // 目标路径 = base_path + rel（统一带前导斜杠，如 "/fn-backup/Photos/xxx"）
        let base = self.base_path();
        let target_path = if base == "/" {
            PathBuf::from(rel)
        } else {
            PathBuf::from(format!("{}/{}", base, rel.trim_start_matches('/')))
        };

        // 1) 从目标读取密文（按 1MiB 粒度上报进度，避免事件过密）
        let file_started = Instant::now();
        let mut stream = self.target.read_stream(&target_path).await?;
        let mut enc = Vec::new();
        let mut last_report: u64 = 0;
        while let Some(chunk) = stream.next().await {
            enc.extend_from_slice(&chunk?);
            if let Some(cb) = &progress {
                let total_read = enc.len() as u64;
                if total_read - last_report >= 1024 * 1024 {
                    last_report = total_read;
                    cb(total_read, 0, file_started.elapsed().as_millis() as u64);
                }
            }
        }
        if let Some(cb) = &progress {
            cb(enc.len() as u64, 0, file_started.elapsed().as_millis() as u64);
        }

        // 2) age 解密（同步阻塞，放入 spawn_blocking）
        let crypto = self.crypto.clone();
        let plain = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            crypto.decrypt_stream(&enc[..], &mut out)?;
            Ok::<Vec<u8>, anyhow::Error>(out)
        })
        .await??;

        // 3) 写本地文件
        let local_path = restore_dir.join(rel);
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&local_path, &plain)?;

        // 4) 回写原始 mtime（来自快照）：使下次增量备份判定为未变化，避免重复上传
        if let Some((_size, mtime_secs)) = self.meta.get(rel) {
            if *mtime_secs > 0 {
                let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(*mtime_secs as u64);
                match std::fs::File::options().write(true).open(&local_path) {
                    Ok(f) => {
                        if let Err(e) = f.set_times(std::fs::FileTimes::new().set_modified(when)) {
                            tracing::warn!(err = %e, "回写文件 mtime 失败: {}", local_path.display());
                        }
                    }
                    Err(e) => {
                        tracing::warn!(err = %e, "打开文件以回写 mtime 失败: {}", local_path.display());
                    }
                }
            }
        }

        // 5) 把恢复结果写回快照：以文件**当前实际** mtime 为准，
        //    确保下次增量备份必然命中「未变化」，不再重复上传。
        if let Some(t) = &self.snapshot_target {
            let mtime_secs = std::fs::metadata(&local_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let entry = SnapshotEntry {
                rel_path: rel.to_string(),
                size: plain.len() as u64,
                mtime_secs,
                is_dir: false,
                digest: None,
            };
            match t.store.save_entry(&t.job_id, &t.account, &entry) {
                Ok(()) => info!(
                    "已回写快照: {} (size={}, mtime={})",
                    rel, entry.size, mtime_secs
                ),
                Err(e) => tracing::warn!(err = %e, "回写快照失败: {}", rel),
            }
        }

        Ok(plain.len() as u64)
    }

    /// 列出目标前缀下的所有文件（递归），返回相对路径（相对 base_path）
    async fn list_target_files(&self) -> Result<Vec<String>> {
        let base = self.base_path();

        let mut files = Vec::new();
        let mut stack = vec![base.clone()];
        while let Some(dir) = stack.pop() {
            let entries = self.target.list(&dir).await?;
            for e in entries {
                if e.is_dir {
                    stack.push(format!("/{}", e.rel_path.trim_start_matches('/')));
                } else {
                    files.push(strip_base(&e.rel_path, &base));
                }
            }
        }
        Ok(files)
    }
}

/// 从路径中去掉 base 前缀，得到相对路径
fn strip_base(path: &str, base: &str) -> String {
    let base = base.trim_matches('/');
    if base.is_empty() {
        return path.trim_start_matches('/').to_string();
    }
    let variants = [format!("/{}/", base), format!("{}/", base)];
    for v in &variants {
        if let Some(rest) = path.strip_prefix(v.as_str()) {
            return rest.to_string();
        }
    }
    path.trim_start_matches('/').to_string()
}

/// 恢复结果摘要
#[derive(Debug, Default)]
pub struct RestoreSummary {
    pub restored: usize,
    pub restored_bytes: u64,
    /// 云端已不存在而被跳过的文件（快照中仍有记录）
    pub missing: Vec<String>,
}
