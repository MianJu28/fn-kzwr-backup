//! 存储抽象层（ADR-005：ACL 防腐层）
//!
//! 定义统一的 `SourceStorage` 与 `TargetStorage` trait，位于领域层。
//! 基础设施层为每种协议实现适配器（local/、kzwr/）。
//! 核心同步逻辑只依赖 trait，不感知具体协议。

use std::path::Path;
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

    /// 测试连接与凭证是否有效
    async fn ping(&self) -> StorageResult<()>;
}
