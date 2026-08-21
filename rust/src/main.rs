//! fnos 增量加密备份系统 · 主入口
//!
//! axum HTTP 服务启动，托管 REST API 与前端静态文件。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

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

    // 数据目录（快照）与配置目录（配置/密钥库）
    let var_dir = std::env::var("TRIM_PKGVAR").unwrap_or_else(|_| ".".to_string());
    let cfg_dir = std::env::var("TRIM_PKGETC").unwrap_or_else(|_| ".".to_string());
    let tmp_dir = std::env::var("TRIM_APPTMP").unwrap_or_else(|_| ".".to_string());
    let var_dir = std::path::PathBuf::from(&var_dir);
    let cfg_dir = std::path::PathBuf::from(&cfg_dir);
    let tmp_dir = std::path::PathBuf::from(&tmp_dir);
    let backup_tmp = tmp_dir.join("staging");
    std::fs::create_dir_all(&backup_tmp).ok();

    // 元数据快照库：存 $TRIM_PKGVAR
    let store = Arc::new(infra::persistence::snapshot::SnapshotStore::open(
        &var_dir.join("meta.db"),
    )?);

    // 口令：用于敏感字段（kzwr 凭据、密钥库）加密
    let passphrase = std::env::var("TRIM_PASSPHRASE").unwrap_or_else(|_| "change-me".to_string());
    let passphrase = infra::keystore::secret(&passphrase);

    // 加密会话（age 密钥对，从密钥库加载或生成）
    let keys = {
        let ks_path = infra::keystore::keystore_path(&cfg_dir);
        match infra::keystore::load_keystore(&passphrase, &ks_path) {
            Ok(k) => {
                info!("已从密钥库加载 age 私钥");
                k
            }
            Err(_) => {
                let k = fnos_backup::domain::crypto::AgeKeys::generate();
                infra::keystore::save_keystore(&k, &passphrase, &ks_path)?;
                info!("已生成并加密存储 age 密钥对");
                k
            }
        }
    };
    let crypto = fnos_backup::domain::crypto::CryptoSession::full(&keys);

    // 配置管理器
    let config_mgr = Arc::new(Mutex::new(infra::config::ConfigManager::new(
        &cfg_dir,
        passphrase,
    )));

    // kzwr 目标客户端（token 由认证服务管理）
    let base_url = std::env::var("TRIM_KZWR_BASE_URL")
        .unwrap_or_else(|_| "https://www.kzwr.com".to_string());
    let kzwr_client = infra::target::kzwr::client::KzwrClient::new(&base_url, 30);
    let token_store = kzwr_client.token_store();
    let target: Arc<dyn fnos_backup::infra::storage_trait::TargetStorage> =
        Arc::new(infra::target::kzwr::storage::KzwrTarget::new(kzwr_client));

    // 认证服务
    let login_bin_dir = std::env::var("TRIM_LOGIN_BIN_DIR").unwrap_or_else(|_| ".".to_string());
    let auth = Arc::new(infra::kzwr_auth::KzwrAuthService::new(
        &std::path::PathBuf::from(&login_bin_dir),
        &tmp_dir,
        config_mgr.clone(),
        token_store,
    ));
    // 启动时从配置加载已保存的 token（避免每次重启都重新登录）
    auth.init_from_config();

    // 目标文件夹 + 任务 id（默认值，实际由配置决定）
    let target_folder = std::env::var("TRIM_KZWR_FOLDER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "fn-backup".to_string());
    let job_id = std::env::var("TRIM_JOB_ID").unwrap_or_else(|_| "default".to_string());
    let default_restore_dir = std::env::var("TRIM_RESTORE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| var_dir.join("restore"));

    // 内部事件总线（状态推送）
    let eventbus = Arc::new(fnos_backup::eventbus::EventBus::new());

    let state = AppState {
        target,
        crypto,
        store,
        eventbus,
        config: config_mgr,
        auth,
        target_folder,
        job_id,
        var_dir,
        tmp_dir,
        backup_tmp,
        default_restore_dir,
    };

    // 前端静态资源目录
    let www_dir = std::env::var("TRIM_WWW_DIR").unwrap_or_else(|_| "www".to_string());
    let www_dir = std::path::PathBuf::from(&www_dir);

    let api_router = http::routes::router(state);
    let app = axum::Router::new()
        .nest("/api", api_router)
        .fallback_service(
            tower_http::services::ServeDir::new(&www_dir)
                .not_found_service(tower_http::services::ServeFile::new(www_dir.join("index.html"))),
        );
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
