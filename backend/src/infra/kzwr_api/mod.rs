//! 酷族网软（kzwr.com）REST API 客户端 —— **增强功能专用，不是备份通道**
//!
//! ADR-009 之后备份/恢复统一走官方 WebDAV（Basic 认证，文件传输链路）。
//! 但 WebDAV 无法提供账号级能力，本模块用于补齐这些「增强功能」：
//!   - 账号信息：存储空间/已用/套餐（WebDAV 无容量接口）
//!   - 回收站：列表与清空（跟随保留策略）
//!
//! 认证方式与历史不同：**不再依赖登录二进制**。用户在浏览器登录 kzwr 后，
//! 从站点的 Cookie 中复制 `access-token` 填入设置页，本客户端以
//! `access-token` 请求头调用官方私有 REST API。未配置 token 时所有增强
//! 功能优雅降级（返回提示，不影响备份/恢复主链路）。

pub mod client;

/// kzwr API 常量（与前端 JS 一致）
pub mod constants {
    /// 分片大小：4MB（历史 REST 上传用；客户端保留该常量）
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
