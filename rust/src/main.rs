//! fnos 增量加密备份系统 · 主入口
//!
//! axum HTTP 服务启动，托管 REST API 与（后续）前端静态文件。

use std::net::SocketAddr;
use std::sync::Arc;

use fnos_backup::http;
use fnos_backup::infra;
use fnos_backup::AppState;
use tracing::info;

/// 默认监听端口
const DEFAULT_PORT: u16 = 8080;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging()?;

    // 飞牛路径用 TRIM_* 环境变量，禁止硬编码
    let port: u16 = std::env::var("TRIM_HTTP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    // 源目录：默认当前目录，实际由授权机制指定
    let source_root = std::env::var("TRIM_SOURCE_DIR").unwrap_or_else(|_| ".".to_string());
    let source_root = std::path::PathBuf::from(&source_root);
    let source = Arc::new(infra::source::local::LocalFsSource::new(&source_root));

    // 目标：kzwr 酷族网软，token 从 TRIM_KZWR_TOKEN 读取（实际由登录二进制产出 session 加密存储后注入）
    let target_token = std::env::var("TRIM_KZWR_TOKEN").unwrap_or_default();
    let base_url = std::env::var("TRIM_KZWR_BASE_URL")
        .unwrap_or_else(|_| "https://www.kzwr.com".to_string());
    let kzwr_client = {
        let mut c = infra::target::kzwr::client::KzwrClient::new(&base_url, 30);
        if !target_token.is_empty() {
            c.set_token(&target_token);
        }
        c
    };
    let target = Arc::new(infra::target::kzwr::storage::KzwrTarget::new(kzwr_client));

    // 加密：age 密钥对（Phase 5 前用内存生成；实际应从加密密钥库加载私钥）
    let keys = fnos_backup::domain::crypto::AgeKeys::generate();
    let crypto = fnos_backup::domain::crypto::CryptoSession::full(&keys);

    // 元数据快照库：存 $TRIM_PKGVAR（飞牛数据目录）
    let var_dir = std::env::var("TRIM_PKGVAR").unwrap_or_else(|_| ".".to_string());
    let store = std::sync::Arc::new(
        infra::persistence::snapshot::SnapshotStore::open(
            &std::path::PathBuf::from(&var_dir).join("meta.db"),
        )?,
    );

    // 目标前缀 + 任务 id
    let target_prefix = std::env::var("TRIM_KZWR_FOLDER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "fn-backup".to_string());
    let job_id = std::env::var("TRIM_JOB_ID").unwrap_or_else(|_| "default".to_string());

    let state = AppState {
        source,
        target,
        crypto,
        store,
        source_root,
        target_prefix: Some(target_prefix),
        job_id,
    };

    let app = http::routes::router(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    info!("fnos-backup 服务启动: http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// 初始化结构化日志（tracing）
fn init_logging() -> anyhow::Result<()> {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("fnos_backup=info,tower_http=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    Ok(())
}
