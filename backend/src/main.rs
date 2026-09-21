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

    // 口令：用于敏感字段（WebDAV 凭据、密钥库）加密
    let passphrase_str =
        std::env::var("TRIM_PASSPHRASE").unwrap_or_else(|_| "change-me".to_string());
    let passphrase = Arc::new(infra::keystore::secret(&passphrase_str));

    // 配置管理器（提前创建：日志初始化需读取 debug 开关）
    let config_mgr = Arc::new(Mutex::new(infra::config::ConfigManager::new(
        &cfg_dir,
        infra::keystore::secret(&passphrase_str),
    )));

    // 运行日志文件（日志页查看/清空/下载；超过 5MB 轮转为 .old）
    let log_file = var_dir.join("logs").join("app.log");
    let log_writer = open_log_file(&log_file)?;

    // 日志初始化（调试开关从已持久化配置读取；运行时可经设置页热切换）
    let debug_on = config_mgr
        .lock()
        .unwrap()
        .load()
        .map(|c| c.debug)
        .unwrap_or(false);
    init_logging(debug_on, log_writer)?;

    // 元数据快照库：存 $TRIM_PKGVAR
    let store = Arc::new(infra::persistence::snapshot::SnapshotStore::open(
        &var_dir.join("meta.db"),
    )?);

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
        log_file,
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

    // 后台空间巡检：每 30 分钟（首次 20 秒后）检查云端占用是否达到空间预警阈值。
    // 不依赖用户打开界面 —— 预警会直接进入「消息提醒」，占用回落时自动消解。
    {
        let quota_state = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            let mut ticker =
                tokio::time::interval(std::time::Duration::from_secs(30 * 60));
            loop {
                ticker.tick().await;
                http::routes::check_kzwr_quota(&quota_state).await;
            }
        });
    }

    // 前端静态资源目录
    let www_dir = std::env::var("TRIM_WWW_DIR").unwrap_or_else(|_| "www".to_string());
    let www_dir = std::path::PathBuf::from(&www_dir);

    // ── 路由 ──────────────────────────────────────────────────────────────
    //
    // 飞牛桌面可能用 https 访问，而应用若只以 http 端口提供服务，iframe 会因
    // **混合内容**被浏览器拦截。官方解法是「统一网关」：应用监听
    // `$TRIM_APPDEST/app.sock`，由 fnOS 以 `/app/<appname>` 前缀反代到该 Socket
    // （与桌面同源同协议 → 自动跟随 https，且支持 WebSocket）。
    //
    // 网关转发时**保留前缀**（如 /app/fn-kzwr-backup/api/health），所以整套路由
    // 同时注册在「根」与「前缀」两处：端口直连（http://nas:8080/）与网关访问都可用。
    let prefix = gateway_prefix();
    let api_router = http::routes::router(state);

    // 端口直连：根路径（`/api` + 静态资源）同时保留前缀路径
    //（前端资源在打包时写死为 `/app/<appname>/...` 绝对路径，端口访问也要能取到）
    let mut root = axum::Router::new();
    if !prefix.is_empty() {
        root = root.nest(&prefix, app_router(api_router.clone(), &www_dir));
    }
    let root = root
        .nest("/api", api_router)
        .fallback_service(
            tower_http::services::ServeDir::new(&www_dir)
                .not_found_service(tower_http::services::ServeFile::new(www_dir.join("index.html"))),
        );

    // 统一网关：监听 Unix Socket（失败仅告警，端口直连仍可用）
    if let Some(sock) = gateway_socket() {
        let sock_log = sock.display().to_string();
        let prefix_log = prefix.clone();
        let gw = root.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_unix(gw, sock, &prefix_log).await {
                tracing::warn!(err = %e, sock = %sock_log, "统一网关监听失败（仅端口直连可用）");
            }
        });
    }

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("酷族备份（fn-kzwr-backup）服务启动: http://{addr}（网关前缀 {prefix}）");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, root).await?;
    Ok(())
}

/// 去掉统一网关前缀：`/app/<appname>/api/health` → `/api/health`
///
/// 网关转发会保留前缀，剥掉后与端口直连走**完全相同**的路由。
/// 不依赖 axum `nest` 的匹配细节（它对 `/prefix` 与 `/prefix/` 表现不一致）。
fn strip_gateway_prefix<B>(req: &mut axum::http::Request<B>, prefix: &str) {
    if prefix.is_empty() {
        return;
    }
    let uri = req.uri().clone();
    let path = uri.path();
    let rest = match path.strip_prefix(prefix) {
        // 只接受「正好是前缀」或「前缀 + /」，避免误伤 /app/<app>xxx
        Some(r) if r.is_empty() || r.starts_with('/') => r,
        _ => return,
    };
    let new_path = if rest.is_empty() { "/" } else { rest };
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let Ok(pq) = format!("{new_path}{query}").parse::<axum::http::uri::PathAndQuery>() else {
        return;
    };
    let mut parts = uri.into_parts();
    parts.path_and_query = Some(pq);
    if let Ok(new_uri) = axum::http::Uri::from_parts(parts) {
        *req.uri_mut() = new_uri;
    }
}

/// 网关前缀剥离服务：类型完全透传（响应/错误/Future 与内部服务一致），
/// 避免用 `map_request` 时闭包参数类型与 hyper 连接类型对不上。
#[derive(Clone)]
struct StripGatewayPrefix<S> {
    inner: S,
    prefix: String,
}

impl<S, B> tower::Service<axum::http::Request<B>> for StripGatewayPrefix<S>
where
    S: tower::Service<axum::http::Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: axum::http::Request<B>) -> Self::Future {
        strip_gateway_prefix(&mut req, &self.prefix);
        self.inner.call(req)
    }
}

/// 构建一份应用路由（`/api` + 静态资源，SPA 回退到 index.html）
fn app_router(api: axum::Router, www_dir: &std::path::Path) -> axum::Router {
    axum::Router::new()
        .nest("/api", api)
        .fallback_service(
            tower_http::services::ServeDir::new(www_dir)
                .not_found_service(tower_http::services::ServeFile::new(www_dir.join("index.html"))),
        )
}

/// 统一网关前缀：`GATEWAY_PREFIX` > `/app/<TRIM_APPNAME>`（默认 `/app/fn-kzwr-backup`）
///
/// 空串表示禁用前缀（仅端口直连）。
fn gateway_prefix() -> String {
    if let Some(p) = env_nonempty("GATEWAY_PREFIX") {
        return p.trim().trim_end_matches('/').to_string();
    }
    let appname = env_nonempty("TRIM_APPNAME").unwrap_or_else(|| "fn-kzwr-backup".to_string());
    format!("/app/{appname}")
}

/// 统一网关 Socket 路径：`TRIM_APP_SOCK` > `GATEWAY_SOCKET` > `$TRIM_APPDEST/app.sock`
fn gateway_socket() -> Option<std::path::PathBuf> {
    if let Some(p) = env_nonempty("TRIM_APP_SOCK").or_else(|| env_nonempty("GATEWAY_SOCKET")) {
        return Some(std::path::PathBuf::from(p));
    }
    env_nonempty("TRIM_APPDEST").map(|d| std::path::PathBuf::from(d).join("app.sock"))
}

/// 在 Unix Socket 上提供服务（hyper 驱动；`with_upgrades` 保证 WebSocket 升级可用）
///
/// 请求进入前先剥掉网关前缀，因此与端口直连共用同一套路由。
async fn serve_unix(
    root: axum::Router,
    sock: std::path::PathBuf,
    prefix: &str,
) -> anyhow::Result<()> {
    // 残留的 socket 文件会导致 bind 失败（EADDRINUSE）
    std::fs::remove_file(&sock).ok();
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let listener = tokio::net::UnixListener::bind(&sock)?;
    info!("统一网关已监听: {} （前缀 {}）", sock.display(), prefix);

    let svc = StripGatewayPrefix {
        inner: root,
        prefix: prefix.to_string(),
    };

    loop {
        let (stream, _) = listener.accept().await?;
        let svc = svc.clone();
        tokio::spawn(async move {
            let io = hyper_util::rt::TokioIo::new(stream);
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, hyper_util::service::TowerToHyperService::new(svc))
                .with_upgrades()
                .await
            {
                tracing::debug!(err = %e, "网关连接结束");
            }
        });
    }
}

/// 初始化结构化日志（tracing；过滤器可经 [`fnos_backup::apply_log_debug`] 热更新）
///
/// 同时输出到 stdout 与日志文件（日志页查看/下载）。
fn init_logging(debug: bool, file: LogFileWriter) -> anyhow::Result<()> {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, reload};
    let base = if debug {
        "fnos_backup=debug,tower_http=info"
    } else {
        "fnos_backup=info,tower_http=info"
    };
    // TRIM_LOG=debug 环境变量仍可强制覆盖初始级别
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(base));
    let (filter_layer, handle) = reload::Layer::new(filter);
    let _ = fnos_backup::LOG_HANDLE.set(handle);
    tracing_subscriber::registry()
        .with(filter_layer)
        // 关闭 ANSI 颜色码：stdout 会被生命周期脚本重定向进同一个日志文件，
        // 带颜色码时日志页/下载的 app.log 会混入 [2m[32m 之类的乱码
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_timer(LocalTimer),
        ) // stdout（生命周期脚本会把它重定向进同一个 app.log，时间戳口径要一致）
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                // 时间戳按宿主本地时区输出（默认是 UTC，在东八区差 8 小时）
                .with_timer(LocalTimer)
                .with_writer(file),
        ) // 日志文件
        .init();
    Ok(())
}

/// 日志时间戳：宿主本地时区，格式 `YYYY-MM-DD HH:MM:SS.mmm`
///
/// tracing 默认用 UTC（`2026-09-21T04:14:21.827270Z`），与 NAS 上看到的时间不一致；
/// 这里用 chrono::Local 渲染，历史行由 `routes.rs::localize_log_time` 在读侧兜底转换。
struct LocalTimer;

impl tracing_subscriber::fmt::time::FormatTime for LocalTimer {
    fn format_time(
        &self,
        w: &mut tracing_subscriber::fmt::format::Writer<'_>,
    ) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"))
    }
}

/// 追加写日志文件的 MakeWriter（供 tracing fmt 层使用）
struct LogFileWriter(std::sync::Mutex<std::fs::File>);

/// MutexGuard 不实现 io::Write，包一层转发
struct LogFileGuard<'a>(std::sync::MutexGuard<'a, std::fs::File>);

impl std::io::Write for LogFileGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogFileWriter {
    type Writer = LogFileGuard<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        LogFileGuard(self.0.lock().unwrap())
    }
}

/// 打开（或轮转后创建）日志文件；超过 5MB 时移为 `app.old.log`
fn open_log_file(path: &std::path::Path) -> anyhow::Result<LogFileWriter> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > 5 * 1024 * 1024 {
        let old = path.with_extension("old.log");
        std::fs::remove_file(&old).ok();
        std::fs::rename(path, &old).ok();
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    Ok(LogFileWriter(std::sync::Mutex::new(f)))
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
