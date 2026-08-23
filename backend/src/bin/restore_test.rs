//! 恢复编排端到端测试（本地模拟）
//!
//! 验证：
//!   1. 备份（age 加密 → 内存 target）
//!   2. 修改源文件（模拟备份后变化）
//!   3. RestoreJob 恢复备份版本到新目录
//!   4. 校验恢复的是备份时的版本（而非修改后的）
//!   5. 选择性恢复（只恢复部分文件）

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
use fnos_backup::infra::persistence::snapshot::SnapshotStore;
use fnos_backup::infra::source::local::LocalFsSource;
use fnos_backup::infra::storage_trait::{
    FileDescriptor, StorageError, StorageResult, TargetStorage,
};

/// 内存 Target（测试用）
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

const PREFIX: &str = "fn-backup";

#[tokio::main]
async fn main() -> Result<()> {
    let dir = TempDir::new()?;
    let src_root = dir.path().join("src");
    std::fs::create_dir_all(&src_root)?;
    std::fs::write(src_root.join("file_a.txt"), b"backup version of A").unwrap();
    std::fs::write(src_root.join("file_b.txt"), b"backup version of B").unwrap();
    std::fs::create_dir_all(src_root.join("sub")).unwrap();
    std::fs::write(src_root.join("sub/file_c.txt"), b"backup version of C").unwrap();

    // 加密组件 + 存储
    let keys = AgeKeys::generate();
    let crypto = CryptoSession::full(&keys);
    let source = Arc::new(LocalFsSource::new(&src_root));
    let target = Arc::new(MemTarget::default());
    let store = Arc::new(SnapshotStore::open(&dir.path().join("meta.db"))?);

    // 1) 备份
    let backup_job = BackupJob {
        job_id: "restore-test".to_string(),
        account: None,
        source: source.clone(),
        target: target.clone(),
        crypto: crypto.clone(),
        store: store.clone(),
        target_prefix: Some(PREFIX.to_string()),
        eventbus: None,
        retention: None,
    };
    println!("=== 1. 备份 ===");
    let s = backup_job.run(&src_root).await?;
    println!("[+] 备份: uploaded={}", s.uploaded);
    assert!(s.uploaded == 3);

    // 2) 修改源文件（模拟备份后变化，恢复到时应是备份版本）
    std::fs::write(src_root.join("file_a.txt"), b"MODIFIED AFTER BACKUP").unwrap();
    std::fs::write(src_root.join("sub/file_c.txt"), b"MODIFIED C AFTER BACKUP").unwrap();
    println!("=== 2. 修改源文件（备份后变化）===");

    // 3) 恢复全部到 restore1 目录
    println!("=== 3. 全量恢复 ===");
    let restore_dir1 = dir.path().join("restore1");
    let restore_job = RestoreJob {
        target: target.clone(),
        crypto: crypto.clone(),
        target_prefix: Some(PREFIX.to_string()),
        eventbus: None,
    };
    let r1 = restore_job.run(&[], &restore_dir1).await?;
    println!("[+] 恢复: restored={} bytes={}", r1.restored, r1.restored_bytes);
    assert!(r1.restored == 3, "应恢复 3 个文件");

    // 校验恢复的是备份版本（不是修改后）
    assert_eq!(
        std::fs::read(restore_dir1.join("file_a.txt"))?,
        b"backup version of A",
        "file_a.txt 恢复应为备份版本"
    );
    assert_eq!(
        std::fs::read(restore_dir1.join("sub/file_c.txt"))?,
        b"backup version of C",
        "sub/file_c.txt 恢复应为备份版本"
    );
    assert_eq!(
        std::fs::read(restore_dir1.join("file_b.txt"))?,
        b"backup version of B"
    );
    println!("[+] 全量恢复校验通过（恢复的是备份版本）");

    // 4) 选择性恢复：只恢复 file_b.txt
    println!("=== 4. 选择性恢复（只恢复 file_b.txt）===");
    let restore_dir2 = dir.path().join("restore2");
    let sel = vec!["file_b.txt".to_string()];
    let r2 = restore_job.run(&sel, &restore_dir2).await?;
    println!("[+] 选择性恢复: restored={}", r2.restored);
    assert!(r2.restored == 1, "选择性恢复只应恢复 1 个文件");
    assert_eq!(
        std::fs::read(restore_dir2.join("file_b.txt"))?,
        b"backup version of B"
    );
    assert!(
        !restore_dir2.join("file_a.txt").exists(),
        "未选择文件不应被恢复"
    );
    println!("[+] 选择性恢复校验通过");

    println!("\n=== 恢复编排测试全部通过 ===");
    Ok(())
}
