//! 领域层（核心域）
//!
//! 不依赖具体基础设施实现，只依赖抽象 trait 与标准库。

pub mod alerts;
pub mod audit;
pub mod backup;
pub mod crypto;
pub mod pace;
pub mod restore;
pub mod retention;
pub mod scheduler;
pub mod sync;

/// 网络类操作重试次数（含首次；WebDAV 请求层与任务层共用此口径）
pub const NETWORK_RETRY_ATTEMPTS: u32 = 3;

/// 判断错误是否值得重试（网络/服务端类瞬态错误）
///
/// 认证失败、权限不足、目标不存在属于确定性错误，重试无意义。
pub fn is_retryable_err(e: &anyhow::Error) -> bool {
    use crate::infra::storage_trait::StorageError;
    !matches!(
        e.downcast_ref::<StorageError>(),
        Some(StorageError::Auth(_))
            | Some(StorageError::PermissionDenied(_))
            | Some(StorageError::NotFound(_))
    )
}
