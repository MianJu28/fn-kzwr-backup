//! 存储抽象层（ADR-005：ACL 防腐层）
//!
//! 定义统一的 `SourceStorage` 与 `TargetStorage` trait，位于领域层。
//! 基础设施层为每种协议实现适配器（local/、kzwr/）。
//! 核心同步逻辑只依赖 trait，不感知具体协议。

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use thiserror::Error;

/// 存储层统一错误类型
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("路径不存在: {0}")]
    NotFound(String),
    #[error("无权限访问: {0}")]
    PermissionDenied(String),
    #[error("网络/协议错误: {0}")]
    Protocol(String),
    #[error("认证失败/凭证过期: {0}")]
    Auth(String),
    #[error("目标已存在: {0}")]
    AlreadyExists(String),
    #[error("其他错误: {0}")]
    Other(String),
}

/// 存储层通用结果
pub type StorageResult<T> = Result<T, StorageError>;

/// 文件描述符：扫描/列表返回的条目
#[derive(Debug, Clone)]
pub struct FileDescriptor {
    /// 相对路径（相对于源/目标根目录）
    pub rel_path: String,
    /// 文件大小（字节），文件夹为 0
    pub size: u64,
    /// 修改时间
    pub modified: Option<SystemTime>,
    /// 是否为文件夹
    pub is_dir: bool,
    /// 内容指纹（可选，严格模式用 BLAKE3）
    pub digest: Option<[u8; 32]>,
}

/// 文件元数据：stat 结果
#[derive(Debug, Clone)]
pub struct FileMeta {
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub is_dir: bool,
}

/// 数据源存储（只读）：飞牛 NAS 本地文件系统
///
/// 核心逻辑经此 trait 读取源数据，不感知底层协议。
#[async_trait]
pub trait SourceStorage: Send + Sync {
    /// 列出目录下所有条目
    async fn list(&self, path: &Path) -> StorageResult<Vec<FileDescriptor>>;

    /// 流式读取文件内容
    async fn read_stream(
        &self,
        path: &Path,
    ) -> StorageResult<Box<dyn Stream<Item = StorageResult<Bytes>> + Send + Unpin>>;

    /// 获取文件元数据
    async fn stat(&self, path: &Path) -> StorageResult<FileMeta>;
}

/// 目标存储（读写）：酷族网软等备份目标
///
/// 加密后的密文经此 trait 写入目标，核心逻辑不感知分块上传等协议细节。
#[async_trait]
pub trait TargetStorage: Send + Sync {
    /// 流式写入文件（覆盖语义）
    async fn write_stream(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
    ) -> StorageResult<()>;

    /// 流式读取文件
    async fn read_stream(
        &self,
        path: &Path,
    ) -> StorageResult<Box<dyn Stream<Item = StorageResult<Bytes>> + Send + Unpin>>;

    /// 删除文件
    async fn delete(&self, path: &Path) -> StorageResult<()>;

    /// 按前缀列出文件
    async fn list(&self, prefix: &str) -> StorageResult<Vec<FileDescriptor>>;

    /// 确保目录存在（含空目录；默认无操作，WebDAV 实现为逐级 MKCOL）。
    ///
    /// 用于保证「所选文件夹」本身及其空子目录在目标端被创建，而不仅仅依赖
    /// 上传文件时的父目录自动创建。
    async fn ensure_dir(&self, _path: &Path) -> StorageResult<()> {
        Ok(())
    }

    /// 测试连接与凭证是否有效
    async fn ping(&self) -> StorageResult<()>;
}

/// 未配置目标的占位适配器：所有操作返回明确的引导错误
///
/// WebDAV 为唯一目标（ADR-009）。未配置凭据时服务仍可启动（供 UI 配置），
/// 但备份/恢复等操作会以此错误提示用户先完成配置。
pub struct UnconfiguredTarget {
    pub message: String,
}

#[async_trait]
impl TargetStorage for UnconfiguredTarget {
    async fn write_stream(
        &self,
        _path: &Path,
        _stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
    ) -> StorageResult<()> {
        Err(StorageError::Auth(self.message.clone()))
    }

    async fn read_stream(
        &self,
        _path: &Path,
    ) -> StorageResult<Box<dyn Stream<Item = StorageResult<Bytes>> + Send + Unpin>> {
        Err(StorageError::Auth(self.message.clone()))
    }

    async fn delete(&self, _path: &Path) -> StorageResult<()> {
        Err(StorageError::Auth(self.message.clone()))
    }

    async fn list(&self, _prefix: &str) -> StorageResult<Vec<FileDescriptor>> {
        Err(StorageError::Auth(self.message.clone()))
    }

    async fn ping(&self) -> StorageResult<()> {
        Err(StorageError::Auth(self.message.clone()))
    }
}

/// 可热替换的目标存储
///
/// WebDAV 凭据经 UI 保存后无需重启即可切换实现（ADR-009）。
/// 委托当前内部实现；初始为 `UnconfiguredTarget`。
pub struct SwapTarget {
    inner: std::sync::RwLock<Arc<dyn TargetStorage>>,
}

impl SwapTarget {
    pub fn new(initial: Arc<dyn TargetStorage>) -> Self {
        Self {
            inner: std::sync::RwLock::new(initial),
        }
    }

    /// 替换当前实现
    pub fn swap(&self, next: Arc<dyn TargetStorage>) {
        *self.inner.write().unwrap() = next;
    }

    fn current(&self) -> Arc<dyn TargetStorage> {
        self.inner.read().unwrap().clone()
    }
}

#[async_trait]
impl TargetStorage for SwapTarget {
    async fn write_stream(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
    ) -> StorageResult<()> {
        self.current().write_stream(path, stream).await
    }

    async fn read_stream(
        &self,
        path: &Path,
    ) -> StorageResult<Box<dyn Stream<Item = StorageResult<Bytes>> + Send + Unpin>> {
        self.current().read_stream(path).await
    }

    async fn delete(&self, path: &Path) -> StorageResult<()> {
        self.current().delete(path).await
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<FileDescriptor>> {
        self.current().list(prefix).await
    }

    async fn ensure_dir(&self, path: &Path) -> StorageResult<()> {
        self.current().ensure_dir(path).await
    }

    async fn ping(&self) -> StorageResult<()> {
        self.current().ping().await
    }
}
