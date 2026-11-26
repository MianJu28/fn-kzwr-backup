//! 本地文件系统 Source 适配器（tokio::fs）
//!
//! 源仅限飞牛 NAS 本地文件系统，零 C 依赖，流式读取。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use tokio::fs;
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;

use super::super::storage_trait::{
    FileDescriptor, FileMeta, SourceStorage, StorageError, StorageResult,
};

/// 本地文件系统源
pub struct LocalFsSource {
    /// 已授权的根目录
    root: PathBuf,
}

impl LocalFsSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 把相对路径解析到根目录内的绝对路径，防止路径穿越
    ///
    /// 两层防线，**缺一不可**：
    /// 1. 拒绝绝对路径 —— 否则 `root.join()` 会直接忽略 root（`join` 遇到绝对路径
    ///    时返回后者），`/etc/shadow` 之类的路径就能绕出授权目录；
    /// 2. 逐段拒绝 `..` —— 光有第 1 条不够，`a/../../etc/passwd` 是相对路径，
    ///    join 之后照样逃出 root。
    ///
    /// 为什么按**段**而不是查子串：子串匹配会把正常文件名（`..报告.pdf`、
    /// `foo..bar`）误杀，而 `..` 只有作为完整一段出现时才有穿越含义。
    fn resolve(&self, rel: &Path) -> StorageResult<PathBuf> {
        if rel.is_absolute() {
            return Err(StorageError::Other(format!(
                "不允许绝对路径: {}",
                rel.display()
            )));
        }
        for comp in rel.components() {
            if matches!(comp, std::path::Component::ParentDir) {
                return Err(StorageError::Other(format!(
                    "不允许跨越上级目录（..）: {}",
                    rel.display()
                )));
            }
        }
        Ok(self.root.join(rel))
    }
}

#[async_trait]
impl SourceStorage for LocalFsSource {
    async fn list(&self, path: &Path) -> StorageResult<Vec<FileDescriptor>> {
        let abs = self.resolve(path)?;
        let mut rd = fs::read_dir(&abs)
            .await
            .map_err(|e| map_io(e, &abs))?;

        let mut entries = Vec::new();
        while let Some(entry) = rd.next_entry().await.map_err(map_io_err)? {
            let ft = entry.file_type().await.map_err(map_io_err)?;
            let name = entry.file_name();
            let child_rel = if path.as_os_str().is_empty() {
                PathBuf::from(&name)
            } else {
                path.join(&name)
            };
            let meta = entry.metadata().await.map_err(map_io_err)?;

            entries.push(FileDescriptor {
                rel_path: child_rel.to_string_lossy().into_owned(),
                size: meta.len(),
                modified: meta.modified().ok(),
                is_dir: ft.is_dir(),
                digest: None,
            });
        }
        Ok(entries)
    }

    async fn read_stream(
        &self,
        path: &Path,
    ) -> StorageResult<Box<dyn Stream<Item = StorageResult<Bytes>> + Send + Unpin>> {
        let abs = self.resolve(path)?;
        let file = fs::File::open(&abs)
            .await
            .map_err(|e| map_io(e, &abs))?;
        // **按 CHUNK 大小直接出块**（而不是 `ReaderStream` 默认的 8KB）
        //
        // 为什么这很关键：下游 `encrypt_stream_to_staging` 要把数据攒成 CHUNK 再加密。
        // 若上游按 8KB 出块，攒满一个 64MB 块需要 8000 次 `extend_from_slice`
        // —— 实测光这次的「缓冲搬运」就占 188ms/512MB，把并行加密的 4 倍收益全吃掉。
        // 改为一次读满 CHUNK 后，下游几乎零搬运，直接移交所有权给加密任务。
        let stream = ReaderStream::with_capacity(file, crate::domain::crypto::CHUNK_SIZE)
            .map(|r| r.map_err(map_io_err));
        Ok(Box::new(stream))
    }

    async fn stat(&self, path: &Path) -> StorageResult<FileMeta> {
        let abs = self.resolve(path)?;
        let meta = fs::metadata(&abs)
            .await
            .map_err(|e| map_io(e, &abs))?;
        Ok(FileMeta {
            size: meta.len(),
            modified: meta.modified().ok(),
            is_dir: meta.is_dir(),
        })
    }
}

/// 将 io::Error 映射为 StorageError，区分 NotFound / PermissionDenied
fn map_io(e: std::io::Error, path: &Path) -> StorageError {
    match e.kind() {
        std::io::ErrorKind::NotFound => {
            StorageError::NotFound(path.display().to_string())
        }
        std::io::ErrorKind::PermissionDenied => {
            StorageError::PermissionDenied(path.display().to_string())
        }
        _ => StorageError::Io(e),
    }
}

fn map_io_err(e: std::io::Error) -> StorageError {
    StorageError::Io(e)
}
