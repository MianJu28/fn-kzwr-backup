//! 增量备份端到端测试（本地模拟）
//!
//! 用 LocalFsSource + 内存 Target 验证：
//!   1. 首次全量备份（age 加密）
//!   2. 增量备份（mtime+size 差分，未变化文件跳过）
//!   3. 恢复（下载解密，内容一致性校验）
//!   4. 删除同步（源删除后，目标也删除）

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
use fnos_backup::infra::persistence::snapshot::SnapshotStore;
use fnos_backup::infra::source::local::LocalFsSource;
use fnos_backup::infra::storage_trait::{
    FileDescriptor, StorageError, StorageResult, TargetStorage,
};

/// 内存 Target（测试用）：以 rel_path 为键存加密字节
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
    // 准备临时源目录 + 快照库
    let dir = TempDir::new()?;
    let src_root = dir.path().join("src");
    std::fs::create_dir_all(&src_root)?;

    // 创建源文件
    std::fs::write(src_root.join("a.txt"), b"hello world aaa").unwrap();
    std::fs::write(src_root.join("b.txt"), b"hello world bbb").unwrap();
    std::fs::create_dir_all(src_root.join("sub")).unwrap();
    std::fs::write(src_root.join("sub/c.txt"), "nested content 12345".as_bytes()).unwrap();

    // 构建组件
    let keys = AgeKeys::generate();
    let crypto = CryptoSession::full(&keys);
    let source = Arc::new(LocalFsSource::new(&src_root));
    let target = Arc::new(MemTarget::default());
    let store = Arc::new(SnapshotStore::open(&dir.path().join("meta.db"))?);

    let job = BackupJob {
        job_id: "test-job".to_string(),
        source,
        target: target.clone(),
        crypto: crypto.clone(),
        store: store.clone(),
        target_prefix: None,
    };

    // 1) 首次全量备份
    println!("=== 1. 首次全量备份 ===");
    let s1 = job.run(&src_root).await?;
    println!(
        "[+] 首次: uploaded={} bytes={} deleted={} unchanged={}",
        s1.uploaded, s1.uploaded_bytes, s1.deleted, s1.unchanged
    );
    assert!(s1.uploaded == 3, "首次应上传 3 个文件");

    // 2) 验证目标里是加密数据（不是明文）
    let enc_a = target.files.lock().unwrap().get("a.txt").unwrap().clone();
    assert!(enc_a != b"hello world aaa", "目标应为加密数据");

    // 3) 二次备份（无变化，应全跳过）
    println!("=== 2. 增量备份（无变化）===");
    let s2 = job.run(&src_root).await?;
    println!(
        "[+] 二次: uploaded={} bytes={} deleted={} unchanged={}",
        s2.uploaded, s2.uploaded_bytes, s2.deleted, s2.unchanged
    );
    assert!(s2.uploaded == 0, "无变化时应 0 上传");

    // 4) 修改文件 → 增量只上传变化文件
    std::fs::write(src_root.join("a.txt"), b"hello world MODIFIED").unwrap();
    println!("=== 3. 增量备份（a.txt 已修改）===");
    let s3 = job.run(&src_root).await?;
    println!(
        "[+] 三次: uploaded={} bytes={} unchanged={}",
        s3.uploaded, s3.uploaded_bytes, s3.unchanged
    );
    assert!(s3.uploaded == 1, "只应上传修改的 a.txt");

    // 5) 恢复：解密目标，对比内容
    println!("=== 4. 恢复校验 ===");
    for rel in ["a.txt", "b.txt", "sub/c.txt"] {
        let enc = target.files.lock().unwrap().get(rel).unwrap().clone();
        let plain = crypto.decrypt_stream_full(&enc)?;
        let expected: &[u8] = match rel {
            "a.txt" => b"hello world MODIFIED",
            "b.txt" => b"hello world bbb",
            "sub/c.txt" => b"nested content 12345",
            _ => unreachable!(),
        };
        assert_eq!(plain, expected, "{} 恢复内容应与最新源一致", rel);
        println!("[+] 恢复校验通过: {} ({})", rel, String::from_utf8_lossy(&plain));
    }

    // 6) 删除同步：删掉 b.txt，备份后目标也应删除
    std::fs::remove_file(src_root.join("b.txt")).unwrap();
    println!("=== 5. 删除同步 ===");
    let s5 = job.run(&src_root).await?;
    println!(
        "[+] 四次: uploaded={} deleted={} unchanged={}",
        s5.uploaded, s5.deleted, s5.unchanged
    );
    assert!(s5.deleted == 1, "应删除 1 个文件");
    assert!(
        !target.files.lock().unwrap().contains_key("b.txt"),
        "目标中 b.txt 应已删除"
    );
    println!("[+] 删除同步成功");

    println!("\n=== 端到端增量备份测试全部通过 ===");
    Ok(())
}

/// 辅助：解密完整密文流（[len][age密文] 格式）
trait CryptoExt {
    fn decrypt_stream_full(&self, encrypted: &[u8]) -> Result<Vec<u8>>;
}
impl CryptoExt for CryptoSession {
    fn decrypt_stream_full(&self, encrypted: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.decrypt_stream(&encrypted[..], &mut out)?;
        Ok(out)
    }
}
