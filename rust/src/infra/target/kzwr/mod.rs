//! 酷族网软（kzwr.com）Target 适配器
//!
//! Rust 重写自参考项目 `kzwr/kzwr_api.py`（Python `KzwrClient`）。
//! 认证方式：登录后携带 `access-token` Header 请求酷族自定义 REST API。

pub mod client;
pub mod storage;
pub mod upload;

/// kzwr API 常量（与前端 JS 一致）
pub mod constants {
    /// 分片大小：4MB
    pub const SHARD_SIZE: usize = 1024 * 1024 * 4;
    /// 分片哈希采样：仅取每分片前 64KB 计算 SHA256
    pub const HASH_SAMPLE_SIZE: usize = 64 * 1024;
    /// 首/尾 4MB 采样（First4MBHash / Last4MBHash）
    pub const HASH_SAMPLE_END_SIZE: usize = 1024 * 1024 * 4;
    /// 默认 API 基址
    pub const BASE_URL: &str = "https://www.kzwr.com";
}

/// kzwr API 错误
#[derive(Debug, thiserror::Error)]
pub enum KzwrError {
    #[error("网络请求失败: {0}")]
    Network(#[from] reqwest::Error),
    #[error("认证失败: {0}")]
    Auth(String),
    #[error("API 错误: {0}")]
    Api(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("其他错误: {0}")]
    Other(String),
}

/// kzwr 操作结果
pub type KzwrResult<T> = Result<T, KzwrError>;
