//! fnos 增量加密备份系统 · 主入口
//!
//! axum HTTP 服务启动，托管 REST API 与前端静态文件。
//! 目标存储：kzwr 官方 WebDAV（ADR-009，逆向 REST API 已移除）。

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
    let tmp_dir = std::env::var("TRIM_PKGTMP").unwrap_or_else(|_| ".".to_string());
    let var_dir = std::path::PathBuf::from(&var_dir);
    let cfg_dir = std::path::PathBuf::from(&cfg_dir);
    let tmp_dir = std::path::PathBuf::from(&tmp_dir);
    let backup_tmp = tmp_dir.join("staging");
    std::fs::create_dir_all(&backup_tmp).ok();

    // 元数据快照库：存 $TRIM_PKGVAR
    let store = Arc::new(infra::persistence::snapshot::SnapshotStore::open(
        &var_dir.join("meta.db"),
    )?);

    // 口令：用于敏感字段（WebDAV 凭据、密钥库）加密
    let passphrase_str =
        std::env::var("TRIM_PASSPHRASE").unwrap_or_else(|_| "change-me".to_string());
    let passphrase = Arc::new(infra::keystore::secret(&passphrase_str));

    // 加密会话（age 密钥对：从密钥库加载；不存在则自动生成并提示用户保存）
    let pending_reveal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
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
                info!("已生成并加密存储 age 密钥对（请在 Web 界面妥善保存私钥）");
                // 首次自动生成：把私钥经 /api/keys 一次性推送给前端展示
                *pending_reveal.lock().unwrap() = Some(k.to_secret_key());
                k
            }
        }
    };
    let crypto = Arc::new(fnos_backup::CryptoSwap::new(
        fnos_backup::domain::crypto::CryptoSession::full(&keys),
    ));

    // 配置管理器
    let config_mgr = Arc::new(Mutex::new(infra::config::ConfigManager::new(
        &cfg_dir,
        infra::keystore::secret(&passphrase_str),
    )));

    // kzwr REST 增强客户端（账号存储空间/回收站清理；与备份通道无关）
    let kzwr = Arc::new(infra::kzwr_api::client::KzwrClient::default());
    match config_mgr.lock().unwrap().kzwr_token() {
        Ok(Some(t)) => {
            kzwr.set_token(t);
            info!("kzwr access-token 已加载（增强功能可用：存储空间/回收站）");
        }
        _ => info!("未配置 kzwr access-token（增强功能降级，不影响备份/恢复）"),
    }

    // 目标存储：kzwr 官方 WebDAV（唯一目标，ADR-009）
    let (target, backend_name, target_ready) = build_target(&config_mgr)?;
    let target = Arc::new(infra::storage_trait::SwapTarget::new(target));
    info!("目标存储后端: {}（ready={target_ready}）", backend_name);

    // 目标文件夹 + 任务 id（默认值，实际由配置决定）
    let target_folder = std::env::var("TRIM_KZWR_FOLDER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "fn-backup".to_string());
    let job_id = std::env::var("TRIM_JOB_ID").unwrap_or_else(|_| "default".to_string());
    let default_restore_dir = std::env::var("TRIM_RESTORE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| var_dir.join("restore"));

    // 告警汇聚点（监控告警，最多保留 50 条）
    let alerts = Arc::new(fnos_backup::domain::alerts::AlertSink::new(50));

    // 内部事件总线（状态推送）
    let eventbus = Arc::new(fnos_backup::eventbus::EventBus::new());

    let audit = Arc::new(fnos_backup::domain::audit::AuditLog::new(&var_dir));

    let state = AppState {
        target,
        target_ready,
        kzwr,
        audit,
        backup_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        crypto,
        passphrase: passphrase.clone(),
        cfg_dir,
        pending_key_reveal: pending_reveal,
        store,
        alerts,
        eventbus,
        config: config_mgr,
        target_folder,
        job_id,
        var_dir,
        tmp_dir,
        backup_tmp,
        default_restore_dir,
    };

    // 定时备份调度器（后台任务，到点触发备份）
    let scheduler_state = state.clone();
    fnos_backup::domain::scheduler::spawn_scheduler(scheduler_state, 60);

    // 启动时校验 kzwr access-token（已配置时；失效则生成告警提醒用户重新获取）
    {
        let check_state = state.clone();
        tokio::spawn(async move {
            http::routes::check_kzwr_token(&check_state).await;
        });
    }

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

    info!("酷族备份（fn-kzwr-backup）服务启动: http://{addr}");
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

/// 构建目标存储：WebDAV 凭据来源优先级 TRIM_DAV_* 环境变量 > 加密配置 [webdav] 段。
///
/// 未配置时返回占位适配器（服务照常启动供 UI 配置，操作返回引导错误）。
fn build_target(
    config_mgr: &Arc<Mutex<infra::config::ConfigManager>>,
) -> anyhow::Result<(
    Arc<dyn infra::storage_trait::TargetStorage>,
    String,
    bool,
)> {
    // 1) 环境变量
    let env_url = env_nonempty("TRIM_DAV_URL");
    let env_user = env_nonempty("TRIM_DAV_USER");
    let env_pass = env_nonempty("TRIM_DAV_PASS");

    // 2) 加密配置
    let (cfg_url, cfg_user, cfg_pass) = {
        let mgr = config_mgr.lock().unwrap();
        let (user, pass) = mgr.webdav_credentials().unwrap_or((None, None));
        let url = mgr
            .load()
            .ok()
            .and_then(|c| c.webdav.url)
            .filter(|s| !s.is_empty());
        (url, user, pass)
    };

    let user = env_user.or(cfg_user);
    let pass = env_pass.or(cfg_pass);
    // 地址：环境变量 > 配置 > 官方默认（凭据存在时）
    let url = env_url.or(cfg_url).or_else(|| {
        user.as_ref()
            .map(|_| infra::target::webdav::DEFAULT_URL.to_string())
    });

    match (url, user, pass) {
        (Some(url), Some(user), Some(pass)) if !user.is_empty() && !pass.is_empty() => Ok((
            Arc::new(infra::target::webdav::WebdavTarget::new(&url, user, pass)),
            format!("WebDAV（{}）", url),
            true,
        )),
        _ => {
            let msg = "WebDAV 未配置，请在设置中填写 WebDAV 地址与凭据".to_string();
            info!("{}", msg);
            Ok((
                Arc::new(infra::storage_trait::UnconfiguredTarget { message: msg }),
                "未配置".to_string(),
                false,
            ))
        }
    }
}

/// 读取非空环境变量
fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}
