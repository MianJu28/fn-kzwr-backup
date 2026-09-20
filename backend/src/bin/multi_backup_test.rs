//! 多路径备份 + 保留策略 + 并发传输 回归测试（本地模拟）
//!
//! 背景：保留策略曾把刚备份完的文件全部误判为孤儿删除——
//! 根因是受管理集合没带「源文件夹名」前缀（目标端布局为
//! `/<target_prefix>/<源文件夹名>/<rel>`）。本测试固化该修复。
//!
//! 同时覆盖 run_multi 的并发上传与 RestoreJob 的并发下载路径。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use tempfile::TempDir;

use fnos_backup::domain::backup::BackupJob;
use fnos_backup::domain::crypto::{AgeKeys, CryptoSession};
use fnos_backup::domain::restore::RestoreJob;
use fnos_backup::domain::retention::RetentionPolicy;
use fnos_backup::infra::persistence::snapshot::SnapshotStore;
use fnos_backup::infra::source::local::LocalFsSource;
use fnos_backup::infra::storage_trait::{
    FileDescriptor, StorageError, StorageResult, TargetStorage,
};

/// 内存 Target（测试用）：以完整路径为键存加密字节
#[derive(Clone, Default)]
struct MemTarget {
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

#[async_trait]
impl TargetStorage for MemTarget {
    async fn write_stream(
        &self,
        path: &Path,
        mut stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
    ) -> StorageResult<()> {
        let mut buf = Vec::new();
        while let Some(b) = stream.next().await {
            buf.extend_from_slice(&b);
        }
        self.files
            .lock()
            .unwrap()
            .insert(path.to_string_lossy().into_owned(), buf);
        Ok(())
    }

    async fn read_stream(
        &self,
        path: &Path,
    ) -> StorageResult<Box<dyn Stream<Item = StorageResult<Bytes>> + Send + Unpin>> {
        let data = self
            .files
            .lock()
            .unwrap()
            .get(&path.to_string_lossy().into_owned())
            .cloned()
            .ok_or_else(|| StorageError::NotFound(path.to_string_lossy().into_owned()))?;
        Ok(Box::new(futures::stream::iter(vec![Ok(Bytes::from(data))])))
    }

    async fn delete(&self, path: &Path) -> StorageResult<()> {
        self.files
            .lock()
            .unwrap()
            .remove(&path.to_string_lossy().into_owned());
        Ok(())
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<FileDescriptor>> {
        let files = self.files.lock().unwrap();
        Ok(files
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| FileDescriptor {
                rel_path: k.clone(),
                size: v.len() as u64,
                modified: None,
                is_dir: false,
                digest: None,
            })
            .collect())
    }

    async fn ping(&self) -> StorageResult<()> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let dir = TempDir::new()?;
    let root = dir.path();
    let photos = root.join("Photos");
    let docs = root.join("Docs");
    std::fs::create_dir_all(photos.join("raw"))?;
    std::fs::create_dir_all(&docs)?;
    std::fs::write(photos.join("p1.jpg"), b"photo one")?;
    std::fs::write(photos.join("raw/r1.raw"), b"raw data")?;
    std::fs::write(docs.join("d1.txt"), b"doc one")?;

    let keys = AgeKeys::generate();
    let crypto = CryptoSession::full(&keys);
    let target = Arc::new(MemTarget::default());
    let store = Arc::new(SnapshotStore::open(&root.join("meta.db"))?);

    // 预置一个「前次备份残留」的孤儿文件
    target
        .files
        .lock()
        .unwrap()
        .insert("/fn-backup/Photos/orphan.tmp".to_string(), b"stale".to_vec());

    let job = BackupJob {
        job_id: "multi-test".to_string(),
        account: None,
        source: Arc::new(LocalFsSource::new(&photos)),
        target: target.clone(),
        crypto: crypto.clone(),
        store: store.clone(),
        target_prefix: Some("fn-backup".to_string()),
        eventbus: None,
        retention: Some(RetentionPolicy::new(target.clone(), "fn-backup")),
    };

    println!("=== 1. 多路径备份（并发上传 + 保留策略清理）===");
    let s = job.run_multi(&[photos.clone(), docs.clone()]).await?;
    println!(
        "[+] uploaded={} uploaded_bytes={} orphan_removed={}",
        s.uploaded, s.uploaded_bytes, s.orphan_removed
    );
    assert_eq!(s.uploaded, 3, "首次应上传 3 个文件");
    assert_eq!(s.orphan_removed, 1, "应只清理预置的孤儿文件");

    {
        let files = target.files.lock().unwrap();
        assert!(
            files.contains_key("/fn-backup/Photos/p1.jpg"),
            "刚备份的文件不应被清理（回归：孤儿误删）"
        );
        assert!(files.contains_key("/fn-backup/Photos/raw/r1.raw"));
        assert!(files.contains_key("/fn-backup/Docs/d1.txt"));
        assert!(
            !files.contains_key("/fn-backup/Photos/orphan.tmp"),
            "孤儿文件应被清理"
        );
    }
    println!("[+] 孤儿判定正确：已备份文件保留，孤儿被清理");

    println!("=== 2. 二次备份（无变化、无孤儿）===");
    let s2 = job.run_multi(&[photos.clone(), docs.clone()]).await?;
    println!(
        "[+] uploaded={} unchanged={} orphan_removed={}",
        s2.uploaded, s2.unchanged, s2.orphan_removed
    );
    assert_eq!(s2.uploaded, 0, "无变化时应 0 上传");
    assert_eq!(s2.orphan_removed, 0, "无孤儿时不应误删");

    println!("=== 3. 并发恢复下载 ===");
    let mut meta = HashMap::new();
    for entry in store.load_snapshot("multi-test-0", "")? {
        if !entry.is_dir {
            meta.insert(entry.rel_path.clone(), (entry.size, entry.mtime_secs));
        }
    }
    let rjob = RestoreJob {
        target: target.clone(),
        crypto: crypto.clone(),
        target_prefix: Some("fn-backup".to_string()),
        source_root_name: Some("Photos".to_string()),
        eventbus: None,
        meta: meta.clone(),
        snapshot_target: None,
    };
    let dest = root.join("restore-out");
    let rs = rjob
        .run(&["p1.jpg".to_string(), "raw/r1.raw".to_string()], &dest)
        .await?;
    println!("[+] restored={} bytes={}", rs.restored, rs.restored_bytes);
    assert_eq!(rs.restored, 2);
    assert!(rs.missing.is_empty());
    assert_eq!(std::fs::read(dest.join("p1.jpg"))?, b"photo one");
    assert_eq!(std::fs::read(dest.join("raw/r1.raw"))?, b"raw data");
    println!("[+] 恢复内容一致");

    println!("=== 4. 云端文件被删：恢复应跳过缺失而非失败 ===");
    // 模拟用户直接在网盘上删除了 p1.jpg
    target
        .files
        .lock()
        .unwrap()
        .remove("/fn-backup/Photos/p1.jpg");
    let dest2 = root.join("restore-out-2");
    let rs2 = rjob
        .run(&["p1.jpg".to_string(), "raw/r1.raw".to_string()], &dest2)
        .await?;
    println!(
        "[+] restored={} missing={:?}",
        rs2.restored, rs2.missing
    );
    assert_eq!(rs2.restored, 1, "存在的文件应正常恢复");
    assert_eq!(rs2.missing, vec!["p1.jpg".to_string()], "缺失文件应被跳过并记录");
    assert_eq!(std::fs::read(dest2.join("raw/r1.raw"))?, b"raw data");

    println!("=== 5. 快照记录删除（清理缺失的核心操作）===");
    assert!(store.delete_entry("multi-test-0", "", "p1.jpg")?);
    assert!(!store.delete_entry("multi-test-0", "", "p1.jpg")?, "重复删除应返回 false");
    let left: Vec<String> = store
        .load_snapshot("multi-test-0", "")?
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| e.rel_path.clone())
        .collect();
    assert!(!left.contains(&"p1.jpg".to_string()), "已删记录不应再出现");
    assert_eq!(left.len(), 1, "只应剩 raw/r1.raw 一条文件记录");
    println!("[+] delete_entry 行为正确");

    println!("\n=== 多路径备份/保留策略/缺失容忍/并发传输 回归测试全部通过 ===");
    Ok(())
}
