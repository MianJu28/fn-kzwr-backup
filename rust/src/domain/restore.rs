//! 恢复编排（核心域：选择性恢复、完整性校验）
//!
//! 从目标存储读取加密数据 → age 解密 → 写回本地目录。
//! 支持选择性恢复（只恢复指定的文件）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt;
use tracing::info;

use crate::domain::crypto::CryptoSession;
use crate::infra::storage_trait::TargetStorage;

/// 恢复任务
pub struct RestoreJob {
    /// 目标存储（读取加密数据）
    pub target: Arc<dyn TargetStorage>,
    /// 解密会话（含 age 私钥）
    pub crypto: CryptoSession,
    /// 目标根前缀（备份时写入的前缀，如 "fn-backup"），与备份对应
    pub target_prefix: Option<String>,
    /// 内部事件总线（进度推送，可选）
    pub eventbus: Option<Arc<crate::eventbus::EventBus>>,
}

impl RestoreJob {
    /// 执行恢复
    ///
    /// - `rel_files`: 要恢复的文件相对路径列表（选择性恢复）。空列表 = 恢复全部已备份文件。
    /// - `restore_dir`: 本地恢复目标目录（明文写入位置）
    pub async fn run(&self, rel_files: &[String], restore_dir: &Path) -> Result<RestoreSummary> {
        std::fs::create_dir_all(restore_dir)?;

        let files_to_restore: Vec<String> = if rel_files.is_empty() {
            self.list_target_files().await?
        } else {
            rel_files.to_vec()
        };
        let total = files_to_restore.iter().filter(|r| !r.trim().is_empty()).count() as u64;

        self.publish(
            crate::eventbus::TaskStatus::Started,
            None,
            0,
            total,
            None,
        );

        let mut restored = 0usize;
        let mut restored_bytes = 0u64;
        for rel in &files_to_restore {
            if rel.trim().is_empty() {
                continue;
            }
            let n = self.restore_one(rel, restore_dir).await?;
            restored += 1;
            restored_bytes += n;
            self.publish(
                crate::eventbus::TaskStatus::Progress,
                Some(rel.clone()),
                restored as u64,
                total,
                None,
            );
            info!("已恢复: {} ({n} B)", rel);
        }

        self.publish(
            crate::eventbus::TaskStatus::Completed,
            None,
            restored as u64,
            total,
            None,
        );

        Ok(RestoreSummary {
            restored,
            restored_bytes,
        })
    }

    /// 发布进度事件（事件总线可选）
    fn publish(
        &self,
        status: crate::eventbus::TaskStatus,
        current_file: Option<String>,
        done: u64,
        total: u64,
        message: Option<String>,
    ) {
        if let Some(eb) = &self.eventbus {
            eb.task_event(
                crate::eventbus::TaskKind::Restore,
                status,
                "restore".to_string(),
                current_file,
                done,
                total,
                message,
            );
        }
    }

    /// 恢复单个文件：目标读密文 → age 解密 → 写本地
    async fn restore_one(&self, rel: &str, restore_dir: &Path) -> Result<u64> {
        // 目标路径 = prefix + rel（统一带前导斜杠，如 "/fn-backup/xxx"）
        let target_path = match &self.target_prefix {
            Some(p) if !p.trim().is_empty() => PathBuf::from(format!(
                "/{}/{}",
                p.trim_matches('/'),
                rel.trim_start_matches('/')
            )),
            _ => PathBuf::from(rel),
        };

        // 1) 从目标读取密文
        let mut stream = self.target.read_stream(&target_path).await?;
        let mut enc = Vec::new();
        while let Some(chunk) = stream.next().await {
            enc.extend_from_slice(&chunk?);
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

        Ok(plain.len() as u64)
    }

    /// 列出目标前缀下的所有文件（递归），返回相对路径
    async fn list_target_files(&self) -> Result<Vec<String>> {
        // 目标存储 key 为带前导斜杠格式，如 "/fn-backup/xxx"
        let root = match &self.target_prefix {
            Some(p) if !p.trim().is_empty() => format!("/{}", p.trim_matches('/')),
            _ => "/".to_string(),
        };

        let mut files = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let entries = self.target.list(&dir).await?;
            for e in entries {
                if e.is_dir {
                    stack.push(format!("/{}", e.rel_path.trim_start_matches('/')));
                } else {
                    files.push(strip_prefix(&e.rel_path, &self.target_prefix));
                }
            }
        }
        Ok(files)
    }
}

/// 从路径中去掉目标前缀，得到相对路径（兼容带/不带前导斜杠）
fn strip_prefix(path: &str, prefix: &Option<String>) -> String {
    match prefix {
        Some(p) if !p.trim().is_empty() => {
            let name = p.trim_matches('/');
            // 兼容 "/fn-backup/..." 与 "fn-backup/..." 两种存储格式
            let variants = [format!("/{}/", name), format!("{}/", name)];
            for v in &variants {
                if let Some(rest) = path.strip_prefix(v.as_str()) {
                    return rest.to_string();
                }
            }
            path.trim_start_matches('/').to_string()
        }
        _ => path.trim_start_matches('/').to_string(),
    }
}

/// 恢复结果摘要
#[derive(Debug, Default)]
pub struct RestoreSummary {
    pub restored: usize,
    pub restored_bytes: u64,
}
