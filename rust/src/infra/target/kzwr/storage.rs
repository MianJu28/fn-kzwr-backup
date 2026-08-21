//! kzwr `TargetStorage` trait 实现
//!
//! 把 kzwr 自定义 REST API 封装为统一的 `TargetStorage`，供核心同步逻辑使用。
//! 底层复用 `KzwrClient` + 分片上传。

use std::path::Path;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};

use super::client::KzwrClient;
use super::upload::upload_file;
use crate::infra::storage_trait::{
    FileDescriptor, StorageError, StorageResult, TargetStorage,
};

/// kzwr Target 适配器
pub struct KzwrTarget {
    client: KzwrClient,
}

impl KzwrTarget {
    /// 从已登录的 KzwrClient 创建适配器
    pub fn new(client: KzwrClient) -> Self {
        Self { client }
    }

    /// 直接使用 access-token 创建
    pub fn with_token(base_url: &str, token: impl Into<String>) -> Self {
        let mut client = KzwrClient::new(base_url, 30);
        client.set_token(token);
        Self { client }
    }

    /// 访问底层客户端
    pub fn client(&self) -> &KzwrClient {
        &self.client
    }

    /// 把 kzwr 返回的条目映射为统一 FileDescriptor
    fn map_entry(rel: &str, item: &serde_json::Value, is_dir: bool) -> FileDescriptor {
        let size = if is_dir {
            0
        } else {
            item.get("size").and_then(|v| v.as_u64()).unwrap_or(0)
        };
        let name = item
            .get("name")
            .or_else(|| item.get("fileName"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // 根目录（空或 "/"）直接返回名字，否则拼接路径
        let child = if rel.is_empty() || rel == "/" {
            name.to_string()
        } else {
            format!("{}/{}", rel, name)
        };
        FileDescriptor {
            rel_path: child,
            size,
            modified: None,
            is_dir,
            digest: None,
        }
    }

    /// 通过父目录列表 + 文件名定位文件条目（提取 code/sid）
    async fn find_entry(
        &self,
        path: &Path,
    ) -> StorageResult<serde_json::Value> {
        let parent = path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string());
        let parent = if parent.is_empty() { "/".to_string() } else { parent };
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| StorageError::Other("无法获取文件名".to_string()))?;

        let list = self
            .client
            .list_files(&parent, 1, true)
            .await
            .map_err(|e| StorageError::Protocol(format!("列表失败: {}", e)))?;
        list.get("files")
            .and_then(|f| f.as_array())
            .and_then(|arr| {
                arr.iter().find(|it| {
                    it.get("name")
                        .or_else(|| it.get("fileName"))
                        .and_then(|v| v.as_str())
                        == Some(file_name)
                })
            })
            .cloned()
            .ok_or_else(|| StorageError::NotFound(path.to_string_lossy().into_owned()))
    }
}

#[async_trait]
impl TargetStorage for KzwrTarget {
    async fn write_stream(
        &self,
        path: &Path,
        mut stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
    ) -> StorageResult<()> {
        // 暂存流到临时文件，再走 kzwr 分片上传（同步磁盘写，无需 tokio 异步）
        // TODO: 未来可做流式分片直接上传，避免落临时盘
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().map_err(StorageError::Io)?;
        while let Some(chunk) = stream.next().await {
            tmp.write_all(&chunk).map_err(StorageError::Io)?;
        }
        tmp.flush().map_err(StorageError::Io)?;

        let folder = path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string());
        let folder = if folder.is_empty() { "/".to_string() } else { folder };

        upload_file(&self.client, tmp.path(), &folder, "Auto", 0, 3)
            .await
            .map_err(|e| StorageError::Protocol(e.to_string()))?;
        Ok(())
    }

    async fn read_stream(
        &self,
        path: &Path,
    ) -> StorageResult<Box<dyn Stream<Item = StorageResult<Bytes>> + Send + Unpin>> {
        // 通过文件详情获取 internalDownloadLink 后下载
        let entry = self.find_entry(path).await?;
        let code = entry
            .get("code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| StorageError::Other("条目中无 code".to_string()))?;
        let link = self
            .client
            .get_download_link(code, None)
            .await
            .map_err(|e| StorageError::Protocol(e.to_string()))?;
        let bytes = self
            .client
            .raw_get_bytes(&link)
            .await
            .map_err(|e| StorageError::Protocol(e.to_string()))?;
        Ok(Box::new(futures::stream::iter(vec![Ok(bytes)])))
    }

    async fn delete(&self, path: &Path) -> StorageResult<()> {
        let entry = self.find_entry(path).await?;
        let sid = entry
            .get("sid")
            .or_else(|| entry.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| StorageError::Other("条目中无 sid".to_string()))?;
        self.client
            .delete_file(&[sid.to_string()], true)
            .await
            .map_err(|e| StorageError::Protocol(e.to_string()))?;
        Ok(())
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<FileDescriptor>> {
        let data = self
            .client
            .list_files(prefix, 1, true)
            .await
            .map_err(|e| StorageError::Protocol(e.to_string()))?;
        let files = data
            .get("files")
            .and_then(|f| f.as_array())
            .cloned()
            .unwrap_or_default();
        let folders = data
            .get("folders")
            .and_then(|f| f.as_array())
            .cloned()
            .unwrap_or_default();

        let mut out = Vec::new();
        for f in &folders {
            out.push(Self::map_entry(prefix, f, true));
        }
        for f in &files {
            out.push(Self::map_entry(prefix, f, false));
        }
        Ok(out)
    }

    async fn ping(&self) -> StorageResult<()> {
        self.client
            .get_member()
            .await
            .map(|_| ())
            .map_err(|e| StorageError::Auth(e.to_string()))
    }
}
