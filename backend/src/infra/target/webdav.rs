//! WebDAV Target 适配器（ADR-009：kzwr 官方 WebDAV）
//!
//! 实现统一的 `TargetStorage` trait，核心同步/加密逻辑零改动。
//! 已实测（2026-09-18，WSL curl）：
//! - HTTP Basic 认证：PROPFIND(207) / MKCOL(201) / PUT(201) / DELETE(204)
//! - `GET` 下载返回 302 → S3 风格 presigned URL（约 300s 有效），
//!   跟随重定向即可取回内容，无需二次认证（reqwest 默认跟随，跨域自动去凭据）
//!
//! 大文件分片（2026-09-18）：网站对单次上传限制 100MB，超过 `PART_SIZE`
//! 的文件自动拆分为 `<path>.part0001`、`.part0002`… 依次 PUT；
//! 下载按序拼接、删除清理全部分片、列表将分片合并为逻辑文件。
//!
//! 凭据永不硬编码，由调用方从加密配置或环境变量注入。
//! WebDAV 地址固定为官方地址（`DEFAULT_URL`），UI 不暴露设置项。

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use http_body::{Body as HttpBody, Frame, SizeHint};
use reqwest::{Client, Method, StatusCode};

use crate::infra::storage_trait::{
    FileDescriptor, ProgressCb, StorageError, StorageResult, TargetStorage,
};

/// 官方 WebDAV 地址（UI 固定使用，不提供设置项）
pub const DEFAULT_URL: &str = "https://dav.kzwr.com/dav";

/// 解析目标实际生效的地址。
///
/// 内置 WebDAV 不在 UI 暴露地址，配置里存下来的常常是 `Some("")` 而不是
/// `None`。只判 `None` 会得到空串，`WebdavTarget::new("")` 拼出的 URL 在请求
/// 构造阶段就失败，报 `PROPFIND  失败: builder error`（注意双空格 ⇒ url 为空）。
/// 所有读取目标地址的地方都应走这里，保证与插件装配路径一致。
pub fn effective_url(raw: Option<&str>) -> String {
    raw.filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_URL)
        .to_string()
}

/// 单分片大小：90 MiB（默认）
///
/// 网站限制单次上传 100MB（实测：不分片上传 120MB 被 Cloudflare 返回
/// `413 Payload Too Large`）；默认取该限制的 90% 即 90 MiB，留有余量。
/// 单分片必须小于该限制才能绕过；可用环境变量 `FNOS_DAV_PART_SIZE`（字节）
/// 覆盖，便于测试时调小以验证分片拆分/合并/清理逻辑。
pub const PART_SIZE: u64 = 90 * 1024 * 1024;

/// 读取生效的分片大小（环境变量 FNOS_DAV_PART_SIZE 可覆盖）
pub fn part_size() -> u64 {
    std::env::var("FNOS_DAV_PART_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&v| v > 0)
        .unwrap_or(PART_SIZE)
}

/// 单次 PUT 的总超时（慢链路下单请求可能持续很久，放宽到 30 分钟）
const PUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

/// 建立连接后的**首包等待**上限（秒）
///
/// ⚠️ 这个超时是「反卡死」的关键，别删。此前 GET 路径完全没有超时：
/// `connect_timeout` 只管 TCP 建连，一旦连上而服务端不返回数据（跨境链路常见），
/// 请求会**无限等待** —— 表现为恢复任务既不成功也不失败，日志停在
/// 「GET ... 第 1/3 次失败」之后不再前进（线上 18:18 那次即如此）。
///
/// 取值 120s：正常首包远小于它；真卡住时也能在 2 分钟内失败并重试，
/// 而不是让用户等一个永远不会来的响应。
const FIRST_BYTE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// 下载响应体时，**相邻两块数据之间**允许的最长停顿
///
/// `FIRST_BYTE_TIMEOUT` 只管「拿到响应头」；响应体传到一半对端静默，
/// 同样会永久挂住。60s 足够容忍慢链路的一块间隔，又不会让用户干等。
const READ_BODY_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// PUT 最大尝试次数
const PUT_MAX_ATTEMPTS: usize = 4;

/// 进度上报的块大小（256 KiB）：兼顾粒度与事件频率
const PROGRESS_CHUNK: usize = 256 * 1024;

/// 带精确长度的缓冲式请求体（`total` 未知时用，见 [`StreamBody`] 的对比说明）
struct ProgressBody {
    /// 本请求要发的整块数据（缓冲路径用）
    data: Bytes,
    /// 已发送到的偏移
    pos: usize,
    /// 本请求之前的累计字节（多分片时用于拼接整体进度）
    offset: u64,
    /// 进度总量（整文件密文长度；0 = 未知，仅用于上报）
    total: u64,
    /// 本请求开始发送的时刻（用于上报请求内耗时）
    started: std::time::Instant,
    on_progress: ProgressCb,
}

impl HttpBody for ProgressBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.pos >= this.data.len() {
            return Poll::Ready(None);
        }
        let end = (this.pos + PROGRESS_CHUNK).min(this.data.len());
        let piece = this.data.slice(this.pos..end);
        this.pos = end;
        (this.on_progress)(
            this.offset + this.pos as u64,
            this.total,
            this.started.elapsed().as_millis() as u64,
        );
        Poll::Ready(Some(Ok(Frame::data(piece))))
    }

    fn is_end_stream(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.data.len() as u64)
    }
}

/// 从**异步**字节流里按精确长度出帧的请求体（流式路径用，`total` 已知时）
///
/// 与 [`ProgressBody`] 的区别：数据不是一次性备好的，而是每次 `poll_frame` 才向
/// 上游要一块。上游（插件 channel）在无数据时返回 `Pending`，于是 reqwest 会在
/// 网络发不动时停止拉取 ⇒ **背压一路传回宿主的喂块循环** ⇒ 宿主观测到的
/// 「已喂字节 / 已耗时间」就是真实上传速率。
///
/// 这正是「速度显示」的关键：若让插件先把整个分片攒进内存再发，宿主会在
/// 几微秒内喂完 90MiB，所有进度观测的 elapsed 都是 0，速度必然显示不出来。
struct StreamBody {
    /// 上游字节流（按需拉取）
    ///
    /// 用 `Mutex` 包起来只是为了满足 `reqwest::Body: Send + Sync` 的约束 ——
    /// `poll_frame` 天然串行，锁不会产生竞争。
    inner: std::sync::Mutex<Pin<Box<dyn Stream<Item = Bytes> + Send>>>,
    /// 本请求声明要发的总长度（= Content-Length）
    len: u64,
    /// 已发出字节
    sent: Arc<std::sync::atomic::AtomicU64>,
    /// 本请求之前的累计字节
    offset: u64,
    /// 整文件密文长度（进度总量）
    total: u64,
    started: std::time::Instant,
    on_progress: ProgressCb,
}

impl HttpBody for StreamBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let mut guard = this.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut().poll_next(cx) {
            Poll::Ready(Some(b)) => {
                let n = this.sent.fetch_add(b.len() as u64, Ordering::Relaxed) + b.len() as u64;
                (this.on_progress)(
                    this.offset + n,
                    this.total,
                    this.started.elapsed().as_millis() as u64,
                );
                Poll::Ready(Some(Ok(Frame::data(b))))
            }
            Poll::Ready(None) => {
                // 上游提前结束但声明的 Content-Length 还没发够 ⇒ 必须报错。
                // 若放任返回 None，reqwest 会发现「实际发的比声明的少」并报一个
                // 含糊的网络错误，掩盖真实原因（流被截断）。
                let sent = this.sent.load(std::sync::atomic::Ordering::Relaxed);
                if sent < this.len {
                    return Poll::Ready(Some(Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!(
                            "上传流提前结束：声明 {} 字节，只收到 {} 字节",
                            this.len, sent
                        ),
                    ))));
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        false
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.len)
    }
}

/// 上游流的「分片喂料器」：把一条流按**精确长度**切成连续的分片
///
/// 为什么不用「先攒满一片到内存再发」：那样宿主会在几微秒内把整片塞给插件，
/// 进度观测的耗时恒为 0（上游 `FileMeter` 会把这些样本全当抖动丢掉），
/// 于是速度**永远显示不出来**。
///
/// 这里用**容量 2 的有界通道**：拉取任务与 PUT 消费端之间最多在途 2 块，
/// PUT 发不动 ⇒ 通道满 ⇒ 拉取停 ⇒ 背压经插件 channel 传回宿主喂块循环，
/// 宿主量到的「已喂字节 / 已耗时」就是真实上传速率。
struct PartFeeder {
    /// 通道接收端（容量 2，串行消费：同一时刻只有一个分片在发）
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Bytes>>>,
    /// 上一片多读出来的尾巴（下一片先用它）
    carry: Arc<std::sync::Mutex<Option<Bytes>>>,
}

impl PartFeeder {
    /// 启动喂料任务：把 `stream` 灌进有界通道（通道满时自然阻塞，形成背压）
    ///
    /// 容量同样取 1：每多留一块，宿主就能多「凭空」喂一块而不用等网络，
    /// 进度观测的时间基准就越失真（见 `FEED_QUEUE` 的说明）。
    fn spawn(mut stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(1);
        tokio::spawn(async move {
            while let Some(c) = stream.next().await {
                if tx.send(c).await.is_err() {
                    break; // 消费端已结束（出错/中止）
                }
            }
        });
        Self {
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            carry: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// 取接下来**恰好** `len` 字节的流（供一次 PUT 使用）
    ///
    /// 多读出来的部分存进 `carry`，由下一次调用先消费 —— 因此分片边界精确，
    /// 且整条流只被顺序读一遍。
    fn next_part(&self, len: usize) -> Box<dyn Stream<Item = Bytes> + Send + Unpin> {
        let rx = self.rx.clone();
        let carry = self.carry.clone();
        let remaining = Arc::new(std::sync::atomic::AtomicUsize::new(len));
        let s = futures::stream::unfold((), move |()| {
            let rx = rx.clone();
            let carry = carry.clone();
            let remaining = remaining.clone();
            async move {
                let left = remaining.load(Ordering::Relaxed);
                if left == 0 {
                    return None;
                }
                // 先用手上的尾巴
                //
                // ⚠️ **必须先把锁的作用域收窄**：`if let Some(x) = mutex.lock().take()`
                // 里的临时 `MutexGuard` 在 edition 2021 下活到整个 `if let` 体结束，
                // 若体内再 `carry.lock()` 就是同一线程二次加锁 —— `std::sync::Mutex`
                // 非重入，会**永久阻塞**（不是 panic，任务无声卡死）。
                let pending = { carry.lock().unwrap_or_else(|e| e.into_inner()).take() };
                if let Some(c) = pending {
                    if c.len() <= left {
                        remaining.fetch_sub(c.len(), Ordering::Relaxed);
                        return Some((c, ()));
                    }
                    let head = c.slice(..left);
                    let tail = c.slice(left..);
                    *carry.lock().unwrap_or_else(|e| e.into_inner()) = Some(tail);
                    remaining.fetch_sub(left, Ordering::Relaxed);
                    return Some((head, ()));
                }
                // 再从通道取；通道空则等（背压就在这儿）
                let c = {
                    let mut guard = rx.lock().await;
                    guard.recv().await
                }?;
                if c.len() <= left {
                    remaining.fetch_sub(c.len(), Ordering::Relaxed);
                    Some((c, ()))
                } else {
                    let head = c.slice(..left);
                    let tail = c.slice(left..);
                    *carry.lock().unwrap_or_else(|e| e.into_inner()) = Some(tail);
                    remaining.fetch_sub(left, Ordering::Relaxed);
                    Some((head, ()))
                }
            }
        });
        Box::new(Box::pin(s))
    }
}

/// 把一块 `Bytes` 包成带进度的请求体（`offset` = 本块之前的累计字节）
fn progress_body(data: Bytes, offset: u64, total: u64, on_progress: ProgressCb) -> reqwest::Body {
    reqwest::Body::wrap(ProgressBody {
        data,
        pos: 0,
        offset,
        total,
        started: std::time::Instant::now(),
        on_progress,
    })
}

/// WebDAV 目标适配器
#[derive(Clone)]
pub struct WebdavTarget {
    client: Client,
    /// WebDAV 基址（如 https://dav.kzwr.com/dav，无尾斜杠）
    base_url: String,
    /// 基址的 URL 路径前缀（如 "/dav"），用于从 href 计算相对路径
    base_path: String,
    username: String,
    password: String,
    /// 网络代理（`None` = 直连）
    ///
    /// 存**已解析**的 reqwest Proxy：构建 client 时就要用，事后无法改。
    /// 因此本结构一旦带代理就是「固定走代理」，与目标配置里的勾选一一对应。
    proxy: Option<reqwest::Proxy>,
}

/// PROPFIND multistatus 中的单条目（href 已解码）
struct DavEntry {
    href: String,
    is_dir: bool,
    size: u64,
    /// 最后修改时间（来自 `<D:getlastmodified>`，RFC 1123）
    ///
    /// 保留策略的「只清理 N 天以前的孤儿」依赖它；此前恒为 `None`，
    /// 导致该保护**完全不生效**（见 `retention::age_ok`）。
    modified: Option<std::time::SystemTime>,
}

/// 生成第 i 个分片的后缀（.part0001 起）
fn part_suffix(i: usize) -> String {
    format!(".part{:04}", i)
}

/// 识别分片文件名：`xxx.part0001` → (xxx, 1)；非分片返回 None
fn split_part_name(name: &str) -> Option<(String, usize)> {
    if name.len() < 10 || !name.is_char_boundary(name.len() - 9) {
        return None;
    }
    let split = name.len() - 9;
    let digits = name[split..].strip_prefix(".part")?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((name[..split].to_string(), digits.parse().ok()?))
}

/// 校验代理地址写法（比 `reqwest::Proxy::all` 更严格）
///
/// ## 为什么不能只靠 `Proxy::all`
/// 实测 `reqwest::Proxy::all("不是地址")` **不会报错** —— 它把整串当成主机名。
/// 于是用户填错协议（如漏了 `http://`、或写成 `192.168.1.2:7890`）时，
/// 保存阶段一切正常，直到真正备份才出现「代理连不上」，而且报错信息完全指不到
/// 是代理写错了。这里在**保存时**就拦下，给出可操作的提示。
///
/// 允许的 scheme：`http`/`https`（普通代理）、`socks5`/`socks5h`
/// （后者由代理解析域名，跨境场景常用，避免本地 DNS 污染）。
pub fn validate_proxy_url(url: &str) -> Result<(), String> {
    let u = url.trim();
    if u.is_empty() {
        return Err("代理地址不能为空".to_string());
    }
    let parsed = reqwest::Url::parse(u).map_err(|_| {
        format!("代理地址无效：{u}（需要带 scheme，如 http://192.168.1.2:7890）")
    })?;
    match parsed.scheme() {
        "http" | "https" | "socks5" | "socks5h" => {}
        other => {
            return Err(format!(
                "不支持的代理协议：{other}（仅支持 http / https / socks5 / socks5h）"
            ))
        }
    }
    if parsed.host_str().is_none() || parsed.host_str() == Some("") {
        return Err(format!("代理地址缺少主机名：{u}"));
    }
    // 端口缺失时给出提示但不拒绝：部分场景走默认端口（http 8080 之类），
    // 拒绝会造成「明明能用却存不进去」。这里只在明显写错时提示。
    Ok(())
}

/// 由应用配置解析出「本目标该用的代理」
///
/// ## 生效条件（三者同时满足才走代理）
/// 1. 设置里的总开关 `proxy.enabled` 打开，且填了地址；
/// 2. 本目标勾选了 `use_proxy`；
/// 3. 本目标的主机名不在 `proxy.no_proxy` 里。
///
/// 第 3 条的存在理由：`no_proxy` 是用户对「这些地址永远直连」的显式声明
/// （如 `localhost` 或内网域名），不该被某个目标的勾选推翻 —— 否则把一个局域网
/// WebDAV 勾上代理，请求会被发到出口，内网地址在公网根本不可达。
pub fn resolve_proxy(
    cfg: &crate::infra::config::ProxySettings,
    target_url: &str,
    use_proxy: bool,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<Option<reqwest::Proxy>, String> {
    if !use_proxy || !cfg.usable() {
        return Ok(None);
    }
    let raw = cfg.url.trim();
    // `no_proxy` 命中 → 直连
    if host_matches_no_proxy(target_url, &cfg.no_proxy) {
        return Ok(None);
    }
    let mut proxy = reqwest::Proxy::all(raw)
        .map_err(|e| format!("代理地址无效（{raw}）：{e}"))?;
    if let (Some(u), Some(p)) = (username, password) {
        if !u.is_empty() {
            proxy = proxy.basic_auth(u, p);
        }
    }
    Ok(Some(proxy))
}

/// 判断目标 URL 的主机名是否命中 `no_proxy` 列表
///
/// 支持三种写法（与主流 `NO_PROXY` 实现一致）：
/// - 精确主机名：`localhost`
/// - 后缀通配：`*.local`、`.local`（匹配 `nas.local`、`a.b.local`）
/// - 带端口：`localhost:8080`（只比主机名，端口忽略 —— reqwest 的代理是按主机择的）
pub fn host_matches_no_proxy(target_url: &str, no_proxy: &str) -> bool {
    let host = match reqwest::Url::parse(target_url) {
        Ok(u) => match u.host_str() {
            Some(h) => h.to_ascii_lowercase(),
            None => return false,
        },
        Err(_) => return false,
    };
    for raw in no_proxy.split(',') {
        let pat = raw.trim().to_ascii_lowercase();
        if pat.is_empty() {
            continue;
        }
        // 去掉可能带的端口（端口不参与匹配）
        let pat = pat.split(':').next().unwrap_or("").to_string();
        if pat.is_empty() {
            continue;
        }
        if let Some(suffix) = pat.strip_prefix("*.") {
            if host.ends_with(&format!(".{suffix}")) || host == suffix {
                return true;
            }
        } else if let Some(suffix) = pat.strip_prefix('.') {
            if host.ends_with(&format!(".{suffix}")) {
                return true;
            }
        } else if host == pat {
            return true;
        }
    }
    false
}

/// 构建 HTTP 客户端（可选挂代理）
///
/// 抽成独立函数是为了让「首次构建」与「注入代理后重建」共用同一套参数，
/// 避免两处参数漂移（曾经 http1_only 只在一处设置过）。
fn build_client(proxy: Option<&reqwest::Proxy>) -> Client {
    let mut b = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        // 强制 HTTP/1.1：服务端/中间层对长时 HTTP/2 上传流不稳定
        // （实测 ~21s 即 HTTP/2 PROTOCOL_ERROR 掐断）
        .http1_only();
    if let Some(p) = proxy {
        b = b.proxy(p.clone());
    }
    // 不再 `expect`：构建失败了在请求路径上 panic，会直接打断整个备份任务，
    // 且 panic 信息对用户毫无意义。失败时退回一个最小可用 client（无代理），
    // 让「代理配错」降级为「本次不走代理」，而不是让应用崩掉。
    match b.build() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(err = %e, "构建 HTTP 客户端失败，退回直连客户端");
            Client::builder()
                .connect_timeout(std::time::Duration::from_secs(15))
                .http1_only()
                .build()
                .unwrap_or_else(|_| Client::new())
        }
    }
}

impl WebdavTarget {
    /// 创建适配器。`base_url` 如 `https://dav.kzwr.com/dav`。
    pub fn new(base_url: &str, username: impl Into<String>, password: impl Into<String>) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        let base_path = reqwest::Url::parse(&base_url)
            .ok()
            .map(|u| u.path().to_string())
            .filter(|s| !s.is_empty() && s != "/")
            .unwrap_or_default();
        let client = build_client(None);
        Self {
            client,
            base_url,
            base_path,
            username: username.into(),
            password: password.into(),
            proxy: None,
        }
    }

    /// 指定网络代理（`None` = 直连）
    ///
    /// 用 builder 而不是改 `new()` 签名：`new` 被 13 处调用（插件、连通性探测、
    /// 测试），改签名要动一大片；代理是**可选**能力，追加注入更合适。
    ///
    /// ## 为什么必须在构造期决定
    /// reqwest 的代理绑在 `Client` 上，client 构建后无法再改。所以本方法会
    /// **重建 client**，必须在 `new()` 之后、首次发请求之前调用。
    pub fn with_proxy(mut self, proxy: Option<reqwest::Proxy>) -> Self {
        if proxy.is_some() {
            self.client = build_client(proxy.as_ref());
            self.proxy = proxy;
        }
        self
    }

    /// 逻辑相对路径 → 完整 URL（逐段百分号编码）
    fn url_for(&self, rel: &str) -> String {
        let rel = rel.trim_start_matches('/');
        if rel.is_empty() {
            self.base_url.clone()
        } else {
            format!("{}/{}", self.base_url, percent_encode_path(rel))
        }
    }

    fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .basic_auth(&self.username, Some(&self.password))
    }

    /// 发送请求并在**网络错误 / 5xx** 时重试（最多 3 次，线性退避 500ms*n）
    ///
    /// 仅用于幂等的小请求（PROPFIND / MKCOL / DELETE / GET；body 为空或小字符串，
    /// 可被 `try_clone` 复制）。PUT 大 body 走 [`put_bytes_retry`]，不经过这里。
    async fn send_retry(
        &self,
        rb: reqwest::RequestBuilder,
        what: &str,
    ) -> StorageResult<reqwest::Response> {
        self.send_retry_with(rb, what, FIRST_BYTE_TIMEOUT).await
    }

    /// [`Self::send_retry`] 的本体，首包超时可注入（测试用极短值验证真的会超时）
    async fn send_retry_with(
        &self,
        rb: reqwest::RequestBuilder,
        what: &str,
        first_byte: std::time::Duration,
    ) -> StorageResult<reqwest::Response> {
        const RETRY_ATTEMPTS: u32 = 3;
        for attempt in 1..=RETRY_ATTEMPTS {
            let req = match rb.try_clone() {
                Some(r) => r,
                None => {
                    // body 不可克隆时退化为单次发送
                    return rb
                        .send()
                        .await
                        .map_err(|e| StorageError::Protocol(format!("{what} 失败: {e}")));
                }
            };
            // ⚠️ 超时只包裹 `send()` —— 即「建连 + 拿到响应头」。
            //
            // 不能给整个请求加 `.timeout()`：GET 的响应体可能几个 GB，
            // 整体超时会在下载中途掐断大文件（比原 bug 更糟）。
            // 响应体由调用方以流的方式读取，那里的停顿由 `FIRST_BYTE_TIMEOUT`
            // 之外的机制（读流的超时/重试）保证。
            let sent = tokio::time::timeout(first_byte, req.send()).await;
            match sent {
                Err(_elapsed) => {
                    // 超时也重试：跨境链路的首包超时往往是瞬时的
                    if attempt < RETRY_ATTEMPTS {
                        tracing::warn!(
                            "{what} 第 {attempt}/{RETRY_ATTEMPTS} 次失败: {:?} 内未返回响应头",
                            first_byte
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64))
                            .await;
                        continue;
                    }
                    return Err(StorageError::Protocol(format!(
                        "{what} 超时：{first_byte:?} 内未返回响应头（服务端无响应或链路中断）"
                    )));
                }
                Ok(Ok(resp)) => {
                    let status = resp.status();
                    if status.is_server_error() && attempt < RETRY_ATTEMPTS {
                        tracing::warn!("{what} 第 {attempt}/{RETRY_ATTEMPTS} 次失败: HTTP {status}");
                        tokio::time::sleep(std::time::Duration::from_millis(
                            500 * attempt as u64,
                        ))
                        .await;
                        continue;
                    }
                    return Ok(resp);
                }
                Ok(Err(e)) if attempt < RETRY_ATTEMPTS => {
                    tracing::warn!("{what} 第 {attempt}/{RETRY_ATTEMPTS} 次失败: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64))
                        .await;
                }
                Ok(Err(e)) => {
                    return Err(StorageError::Protocol(format!("{what} 失败: {e}")));
                }
            }
        }
        Err(StorageError::Protocol(format!("{what} 重试次数耗尽")))
    }

    /// 逐级 MKCOL 创建路径上的所有目录（从根到叶子；已存在视为成功）
    async fn mkcol_chain(&self, path: &Path) -> StorageResult<()> {
        let mut cur = String::new();
        for comp in path.components() {
            let name = comp.as_os_str().to_string_lossy();
            if name.is_empty() || name == "/" {
                continue;
            }
            if cur.is_empty() {
                cur = name.to_string();
            } else {
                cur = format!("{}/{}", cur, name);
            }
            let url = self.url_for(&cur);
            let resp = self
                .send_retry(
                    self.request(Method::from_bytes(b"MKCOL").unwrap(), &url),
                    &format!("MKCOL {url}"),
                )
                .await?;
            let status = resp.status();
            // 201 创建成功；405/301 已存在（kzwr 服务器对已存在目录返回 301）
            if status != StatusCode::CREATED
                && status != StatusCode::METHOD_NOT_ALLOWED
                && status != StatusCode::MOVED_PERMANENTLY
            {
                let body = resp.text().await.unwrap_or_default();
                return Err(StorageError::Protocol(format!(
                    "MKCOL {} 返回 {}: {}",
                    url, status, body
                )));
            }
        }
        Ok(())
    }

    /// 逐级 MKCOL 确保父目录存在（已存在视为成功）
    async fn ensure_parents(&self, path: &Path) -> StorageResult<()> {
        match path.parent() {
            Some(p) => self.mkcol_chain(p).await,
            None => Ok(()),
        }
    }

    /// PROPFIND 列目录（不存在返回空列表）
    async fn propfind(&self, rel: &str, depth: &str) -> StorageResult<Vec<DavEntry>> {
        let url = self.url_for(rel);
        let resp = self
            .send_retry(
                self.request(Method::from_bytes(b"PROPFIND").unwrap(), &url)
                    .header("Depth", depth)
                    .body(""),
                &format!("PROPFIND {url}"),
            )
            .await?;
        match resp.status() {
            StatusCode::MULTI_STATUS => {}
            StatusCode::NOT_FOUND => return Ok(Vec::new()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(StorageError::Auth(format!("PROPFIND {} 认证失败", url)))
            }
            s => {
                let body = resp.text().await.unwrap_or_default();
                return Err(StorageError::Protocol(format!(
                    "PROPFIND {} 返回 {}: {}",
                    url, s, body
                )));
            }
        }
        let xml = resp
            .text()
            .await
            .map_err(|e| StorageError::Protocol(format!("读取 PROPFIND 响应失败: {}", e)))?;
        Ok(parse_multistatus(&xml))
    }

    /// PUT 一个已攒好的分片（带总超时与重试：网络错误与 5xx/408/429 均可重试）
    ///
    /// `data` 是内存里的一块（≤ `part_size()`），重试时直接重放 —— 因此**不需要**
    /// 像旧实现那样先把密文落到暂存文件再回读。`ProgressCb` 的 `total` 为 0 表示未知。
    ///
    /// 进度口径见 [`ProgressBody`]：按**真实出帧节奏**节流上报，不是「瞬间全交出去」。
    async fn put_buf_retry(
        &self,
        url: &str,
        data: Bytes,
        progress: Option<(ProgressCb, u64, u64)>,
    ) -> StorageResult<()> {
        let len = data.len() as u64;
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            // 本次请求的起始时刻（body 内进度与「响应后补报」都用它计时）
            let req_started = std::time::Instant::now();
            // `Bytes::clone` 只是引用计数 +1，重试不会复制负载
            let body = match &progress {
                Some((cb, offset, total)) => progress_body(data.clone(), *offset, *total, cb.clone()),
                None => reqwest::Body::from(data.clone()),
            };
            let resp = self
                .request(Method::PUT, url)
                .timeout(PUT_TIMEOUT)
                .body(body)
                .send()
                .await;
            match resp {
                Ok(r) => {
                    let status = r.status();
                    if status.is_success() {
                        // 补一次「整次请求耗时」的观测：小文件被本地缓冲时，
                        // body 会在极短时间写完，只有到这里才能拿到真实耗时。
                        if let Some((cb, offset, total)) = &progress {
                            cb(offset + len, *total, req_started.elapsed().as_millis() as u64);
                        }
                        return Ok(());
                    }
                    let retryable = status.is_server_error()
                        || status == StatusCode::REQUEST_TIMEOUT
                        || status == StatusCode::TOO_MANY_REQUESTS;
                    let body = r.text().await.unwrap_or_default();
                    if retryable && attempt < PUT_MAX_ATTEMPTS {
                        tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
                        continue;
                    }
                    return Err(StorageError::Protocol(format!(
                        "PUT {} 返回 {}: {}",
                        url, status, body
                    )));
                }
                Err(e) => {
                    if attempt < PUT_MAX_ATTEMPTS {
                        tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
                        continue;
                    }
                    return Err(StorageError::Protocol(format!(
                        "PUT {} 网络错误: {}",
                        url, e
                    )));
                }
            }
        }
    }

    /// 流式 PUT：直接把上游流交给 reqwest（带精确 Content-Length），边发边报进度
    ///
    /// 与 [`Self::put_buf_retry`] 的关键区别是**不预先缓冲**：上游（插件 channel）
    /// 没有数据时 body 返回 `Pending`，reqwest 就不再拉取 ⇒ socket 背压一路传回
    /// 宿主的喂块循环 ⇒ 宿主观测到的速度就是真实速率。
    ///
    /// ## 为什么不在这里重试
    /// 流一旦被消费就无法回放（这是流式传输的固有代价）。分片级重试要求能重读数据，
    /// 那就必须把整片缓存下来 —— 而缓存正是「速度不显示」的根因（宿主几微秒就喂完
    /// 一片，耗时为 0）。因此这里把重试责任交给上层 [`crate::domain::backup`] 的
    /// 整文件重试：它会重新加密并重传，语义正确，代价是浪费已传的分片。
    /// 权衡结果：**优先保证进度真实**，瞬时故障由整文件重试兜底。
    async fn put_stream_retry(
        &self,
        url: &str,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
        len: u64,
        offset: u64,
        total: u64,
        progress: Option<(ProgressCb, u64, u64)>,
    ) -> StorageResult<()> {
        let sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let started = std::time::Instant::now();
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        let (cb, off, tot) = match progress {
            Some((cb, o, t)) => (cb, o, t),
            None => (noop, offset, total.max(len)),
        };
        let body = reqwest::Body::wrap(StreamBody {
            inner: std::sync::Mutex::new(Box::pin(stream)),
            len,
            sent: sent.clone(),
            offset: off,
            total: tot,
            started,
            on_progress: cb.clone(),
        });
        let resp = self
            .request(Method::PUT, url)
            .timeout(PUT_TIMEOUT)
            .body(body)
            .send()
            .await;
        match resp {
            Ok(r) => {
                let status = r.status();
                if status.is_success() {
                    // 补一次「整次请求耗时」的观测（小分片可能一帧就发完）
                    cb(offset + len, tot, started.elapsed().as_millis() as u64);
                    return Ok(());
                }
                let body = r.text().await.unwrap_or_default();
                Err(StorageError::Protocol(format!(
                    "PUT {} 返回 {}: {}",
                    url, status, body
                )))
            }
            Err(e) => {
                // 优先暴露「流被截断」这类真实原因：reqwest 会把 body 层的
                // UnexpectedEof 压成一句笼统的网络错误，掩盖问题根因。
                let sent = sent.load(Ordering::Relaxed);
                if sent < len {
                    return Err(StorageError::Protocol(format!(
                        "PUT {} 上传流提前结束：声明 {} 字节，只发出 {} 字节（与总长不一致）",
                        url, len, sent
                    )));
                }
                Err(StorageError::Protocol(format!("PUT {} 网络错误: {}", url, e)))
            }
        }
    }

    /// 写入核心实现：**边收边发，全程不落盘**。
    ///
    /// ## 为什么不落盘
    /// 旧实现先把整份密文写进暂存文件，再整份（小文件）或按分片（大文件）读回来上传。
    /// 而密文在宿主侧本来就已经落过一次盘（`encrypt_stream_to_staging`，为的是拿到
    /// 精确长度），这里再落一次纯属多余：8GB 备份要多写 8GB、多读 8GB，还占着
    /// 数据盘；`/tmp` 若是 tmpfs 就直接吃内存。
    ///
    /// ## 怎么做到「不落盘还能带精确 Content-Length」
    /// WebDAV 只接受带精确长度的 PUT（否则只能 chunked，服务端行为未知），而
    /// 分片方案又要求把文件切成 ≤100MB 的 `.partNNNN`。做法是**在内存里攒满一个
    /// 分片再发**：攒够 `part_size()` 的字节数即为该分片的精确长度；最后一片则是
    /// 流结束时剩余的那点字节。RAM 峰值因此是 `part_size()`（默认 90MiB，可由
    /// `FNOS_DAV_PART_SIZE` 调小），**与文件大小无关**。
    ///
    /// `total` 已知时（宿主从加密阶段就能给出密文总长）用于进度百分比与结尾校验；
    /// 未知时传 `None`，行为不变。
    async fn write_inner(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
        total: Option<u64>,
        progress: Option<ProgressCb>,
    ) -> StorageResult<()> {
        self.write_inner_with_part(path, stream, total, progress, part_size() as usize)
            .await
    }

    /// [`Self::write_inner`] 的本体，分片大小由参数给出
    ///
    /// 单独抽出来是为了可测：测试要用小分片验证切分逻辑，若靠改
    /// `FNOS_DAV_PART_SIZE` 环境变量，并行跑测试时会互相污染（已踩过）。
    ///
    /// ## 两条路径
    /// - **`total` 已知**（生产路径）：数据**全程流式**，每个分片都不在本地攒满，
    ///   而是把上游流切成「恰好一片长度」的子流直接喂给 reqwest（[`StreamBody`]）。
    ///   上游没数据时就 `Pending`，于是背压一路传回宿主喂块循环 —— 宿主看到的
    ///   进度与耗时就是真实上传节奏，速度才显示得出来。
    /// - **`total` 未知**（外部插件/兼容调用）：只能先攒满一片再发（[`ProgressBody`]），
    ///   行为与上一版一致。
    async fn write_inner_with_part(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
        total: Option<u64>,
        progress: Option<ProgressCb>,
        part_limit: usize,
    ) -> StorageResult<()> {
        // 先确保目标端父目录存在（逐级 MKCOL）；空文件与小文件也走这条路径
        self.ensure_parents(path).await?;
        let part_limit = part_limit.max(1) as u64;

        let Some(total) = total.filter(|t| *t > 0) else {
            // 未知总长：退回「攒一片发一片」（无法带精确 CL，也无法流式切分）
            return self
                .write_inner_buffered(path, stream, None, progress, part_limit as usize)
                .await;
        };

        // ── 已知总长：流式直发 ─────────────────────────────────────────────
        let feeder = PartFeeder::spawn(stream);
        if total <= part_limit {
            // 小文件：一次 PUT 到逻辑路径，顺带清掉可能残留的旧分片
            self.cleanup_parts(path).await?;
            let url = self.url_for(&path.to_string_lossy());
            let prog = progress.as_ref().map(|cb| (cb.clone(), 0u64, total));
            let piece = feeder.next_part(total as usize);
            return self
                .put_stream_retry(&url, piece, total, 0, total, prog)
                .await;
        }

        // 大文件：先删掉可能存在的旧逻辑文件（覆盖「上次是小文件」的场景）
        //
        // **不能吞掉这个错误**：删除失败意味着「逻辑文件」与「新分片」会同时存在，
        // 而 `read_stream` / `list` 都优先取逻辑文件 ⇒ 备份报成功、恢复却拿到上一版
        // 的旧内容。宁可本次失败（上层会重试），也不要产生这种错乱。
        // 覆盖写前清理旧逻辑文件：它本来就可能不存在（首次上传），故用 if_exists
        self.delete_if_exists(path).await?;
        let mut remaining = total;
        let mut idx = 1usize;
        let mut uploaded: u64 = 0;
        while remaining > 0 {
            let this_len = remaining.min(part_limit);
            let part = self.part_path(path, idx);
            let url = self.url_for(&part.to_string_lossy());
            let prog = progress.as_ref().map(|cb| (cb.clone(), uploaded, total));
            let piece = feeder.next_part(this_len as usize);
            self.put_stream_retry(&url, piece, this_len, uploaded, total, prog)
                .await?;
            uploaded += this_len;
            remaining -= this_len;
            idx += 1;
        }
        // **尾部残留清理**：文件变小后（曾 5 片、现在 3 片），旧 part0004/0005
        // 必须删掉。否则 `read_stream` 会「从 1 逐个 GET 到 404」把旧片一起拼上，
        // 恢复时得到拼接错误的密文 —— 备份成功、数据却是坏的（静默损坏）。
        self.cleanup_parts_from(path, idx).await?;
        // 总量复核（与缓冲路径对称）：防止上游比声明的 total 还多时被静默截断
        if uploaded != total {
            return Err(StorageError::Protocol(format!(
                "{}: 实发 {} 字节，与声明的总长 {} 字节不一致",
                path.display(),
                uploaded,
                total
            )));
        }
        Ok(())
    }

    /// 攒一片发一片的旧路径（`total` 未知时用）
    async fn write_inner_buffered(
        &self,
        path: &Path,
        mut stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
        total: Option<u64>,
        progress: Option<ProgressCb>,
        part_limit: usize,
    ) -> StorageResult<()> {
        let part_limit = part_limit.max(1);
        let mut carry: Option<Bytes> = None;
        let mut eof = false;
        let mut idx = 1usize;
        let mut uploaded: u64 = 0;
        let mut first = true;

        loop {
            let mut buf: Vec<u8> = Vec::with_capacity(part_limit);
            while buf.len() < part_limit {
                let chunk = match carry.take() {
                    Some(c) => c,
                    None => match stream.next().await {
                        Some(c) => c,
                        None => {
                            eof = true;
                            break;
                        }
                    },
                };
                let need = part_limit - buf.len();
                if chunk.len() <= need {
                    buf.extend_from_slice(&chunk);
                } else {
                    buf.extend_from_slice(&chunk[..need]);
                    carry = Some(chunk.slice(need..));
                    break;
                }
            }

            if first {
                first = false;
                if eof && buf.is_empty() {
                    self.cleanup_parts(path).await?;
                    let url = self.url_for(&path.to_string_lossy());
                    let prog = progress.as_ref().map(|cb| (cb.clone(), 0u64, total.unwrap_or(0)));
                    self.put_buf_retry(&url, Bytes::new(), prog).await?;
                    return Ok(());
                }
            } else if buf.is_empty() {
                break;
            }

            if uploaded == 0 && idx == 1 && eof {
                self.cleanup_parts(path).await?;
                let url = self.url_for(&path.to_string_lossy());
                let n = buf.len() as u64;
                let prog = progress
                    .as_ref()
                    .map(|cb| (cb.clone(), 0u64, total.unwrap_or(n)));
                self.put_buf_retry(&url, Bytes::from(buf), prog).await?;
                uploaded = n;
                break;
            }

            // 大文件：首片前先删掉可能存在的旧逻辑文件（覆盖小文件场景）
            if idx == 1 {
                // 同流式路径：删除失败必须上报（否则逻辑文件与分片并存）
                self.delete_if_exists(path).await?;
            }
            let part = self.part_path(path, idx);
            let url = self.url_for(&part.to_string_lossy());
            let n = buf.len() as u64;
            let prog = progress
                .as_ref()
                .map(|cb| (cb.clone(), uploaded, total.unwrap_or(0)));
            self.put_buf_retry(&url, Bytes::from(buf), prog).await?;
            uploaded += n;
            idx += 1;
            if eof {
                break;
            }
        }

        if let Some(t) = total {
            if uploaded != t {
                return Err(StorageError::Protocol(format!(
                    "{}: 实收 {} 字节，与声明的总长 {} 字节不一致（上传流被截断）",
                    path.display(),
                    uploaded,
                    t
                )));
            }
        }
        // 缓冲路径同样要清尾部残留：文件变小后旧的 .partNNNN 会污染下次读取
        if idx > 1 {
            self.cleanup_parts_from(path, idx).await?;
        }
        Ok(())
    }

    /// 删除单个原始路径；404 返回 [`StorageError::NotFound`]
    ///
    /// **保留 404 语义**：对「保留策略清理孤儿」这类破坏性操作，
    /// 「确实删掉一个」与「本来就不存在」必须能分开统计。
    ///
    /// 「覆盖前清理旧文件」的场景请用 [`Self::delete_if_exists`] —— 那里 404 是常态。
    async fn delete_raw(&self, path: &Path) -> StorageResult<()> {
        let url = self.url_for(&path.to_string_lossy());
        let resp = self
            .send_retry(self.request(Method::DELETE, &url), &format!("DELETE {url}"))
            .await?;
        match resp.status() {
            s if s.is_success() => Ok(()),
            // **404 不再折叠成成功**：对「保留策略清理」这类破坏性操作，
            // 「真的删掉一个」与「本来就不存在」必须分开统计 ——
            // 否则用户看到的「已清理 N 个孤儿」里混着大量从未存在的条目，
            // 真正该关注的「清理失败」反而被掩盖。
            //
            // 调用方都显式处理了 `NotFound`（backup.rs / retention.rs 各有分支），
            // 因此这里上抛是安全的。
            StatusCode::NOT_FOUND => Err(StorageError::NotFound(url.clone())),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err(StorageError::Auth(format!("DELETE {} 认证失败", url)))
            }
            s => {
                let body = resp.text().await.unwrap_or_default();
                Err(StorageError::Protocol(format!(
                    "DELETE {} 返回 {}: {}",
                    url, s, body
                )))
            }
        }
    }

    /// 从第 `from` 片起删除残留分片（删到 404 为止）
    ///
    /// 与 [`Self::cleanup_parts`] 的区别：只清「多余的尾巴」，用于覆盖写时文件变小。
    async fn cleanup_parts_from(&self, path: &Path, from: usize) -> StorageResult<()> {
        // 上限防御：若服务端对「不存在的分片」返回 2xx 而非 404，无限循环会挂住任务
        const MAX_PARTS: usize = 100_000;
        for idx in from..=MAX_PARTS {
            let part = self.part_path(path, idx);
            let url = self.url_for(&part.to_string_lossy());
            let resp = self
                .send_retry(self.request(Method::DELETE, &url), &format!("DELETE {url}"))
                .await?;
            match resp.status() {
                s if s.is_success() => continue,
                StatusCode::NOT_FOUND => return Ok(()),
                s => {
                    // 清理失败不能忽略：留着旧分片会导致下次恢复拼错数据
                    let body = resp.text().await.unwrap_or_default();
                    return Err(StorageError::Protocol(format!(
                        "清理残留分片 {url} 返回 {}: {}",
                        s, body
                    )));
                }
            }
        }
        Err(StorageError::Protocol(format!(
            "{}: 残留分片数量超过上限 {MAX_PARTS}（服务端可能对缺失分片未返回 404）",
            path.display()
        )))
    }

    /// 删除路径，**404 视为成功**（用于「覆盖写前清理旧文件」）
    ///
    /// 与 [`Self::delete_raw`] 的区别：覆盖写的语义是「确保它不存在」，
    /// 本来就不存在正是想要的结果，不该当错误。
    async fn delete_if_exists(&self, path: &Path) -> StorageResult<()> {
        match self.delete_raw(path).await {
            Ok(()) | Err(StorageError::NotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// 从 .part0001 起逐个删除分片，直到 404
    /// 删除全部分片，返回**实际删掉的分片数**
    ///
    /// 返回数量让调用方能区分「确实删掉了东西」与「本来就没有」——
    /// 保留策略的清理统计需要这个区别才能可信。
    async fn cleanup_parts(&self, path: &Path) -> StorageResult<usize> {
        // 上限防御：唯一终止条件是 404。若某个 WebDAV 实现/中间层对
        // 「不存在的资源」返回 2xx 而非 404，这里会无限发 DELETE 把任务挂死。
        const MAX_PARTS: usize = 100_000;
        let mut removed = 0usize;
        for idx in 1..=MAX_PARTS {
            let part = self.part_path(path, idx);
            let url = self.url_for(&part.to_string_lossy());
            let resp = self
                .send_retry(self.request(Method::DELETE, &url), &format!("DELETE {url}"))
                .await?;
            match resp.status() {
                s if s.is_success() => {
                    removed += 1;
                    continue;
                }
                StatusCode::NOT_FOUND => return Ok(removed),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    return Err(StorageError::Auth(format!("DELETE {} 认证失败", url)))
                }
                s => {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(StorageError::Protocol(format!(
                        "DELETE {} 返回 {}: {}",
                        url, s, body
                    )));
                }
            }
        }
        Err(StorageError::Protocol(format!(
            "{}: 分片数量超过上限 {MAX_PARTS}（服务端可能对缺失分片未返回 404）",
            path.display()
        )))
    }

    /// 第 idx 个分片的路径
    fn part_path(&self, path: &Path, idx: usize) -> PathBuf {
        let s = path.to_string_lossy();
        PathBuf::from(format!("{}{}", s, part_suffix(idx)))
    }

    /// GET 单个路径，返回字节流（不跟随分片逻辑）
    async fn get_stream_raw(
        &self,
        path: &Path,
    ) -> StorageResult<Box<dyn Stream<Item = StorageResult<Bytes>> + Send + Unpin>> {
        self.get_stream_raw_with_stall(path, READ_BODY_STALL_TIMEOUT).await
    }

    /// [`Self::get_stream_raw`] 的本体，体停顿超时可注入（测试用极短值验证真的会报错）
    async fn get_stream_raw_with_stall(
        &self,
        path: &Path,
        stall: std::time::Duration,
    ) -> StorageResult<Box<dyn Stream<Item = StorageResult<Bytes>> + Send + Unpin>> {
        let url = self.url_for(&path.to_string_lossy());
        let resp = self
            .send_retry(self.request(Method::GET, &url), &format!("GET {url}"))
            .await?;
        let status = resp.status();
        match status {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND => {
                return Err(StorageError::NotFound(path.to_string_lossy().into_owned()))
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(StorageError::Auth(format!("GET {} 认证失败", url)))
            }
            s => {
                let body = resp.text().await.unwrap_or_default();
                return Err(StorageError::Protocol(format!(
                    "GET {} 返回 {}: {}",
                    url, s, body
                )));
            }
        }
        // 302 → presigned URL 由 reqwest 自动跟随（默认策略），此处已是最终 200 响应
        //
        // **每块读也加停顿超时**：`FIRST_BYTE_TIMEOUT` 只保护「拿到响应头」，
        // 而响应体传到一半对端不再发数据同样会无限等待（跨境链路的典型故障）。
        // 这里对每次 `bytes_stream().next()` 包一层超时，卡住即报错 ——
        // 上层（恢复任务）的重试逻辑才能接手。
        let body = resp.bytes_stream();
        let stream = futures::stream::unfold(
            (body, false),
            move |(mut body, done)| async move {
                if done {
                    return None;
                }
                let next = tokio::time::timeout(stall, body.next()).await;
                match next {
                    Ok(Some(Ok(b))) => Some((Ok(b), (body, false))),
                    Ok(Some(Err(e))) => Some((
                        Err(StorageError::Protocol(format!("下载中断: {e}"))),
                        (body, true),
                    )),
                    Ok(None) => None,
                    Err(_elapsed) => Some((
                        Err(StorageError::Protocol(format!(
                            "下载停顿：{stall:?} 内未收到数据（链路中断或对端无响应）"
                        ))),
                        (body, true),
                    )),
                }
            },
        );
        let boxed: Box<dyn Stream<Item = StorageResult<Bytes>> + Send + Unpin> =
            Box::new(Box::pin(stream));
        Ok(boxed)
    }

    /// 把服务端 href（如 /dav/fn-backup/a.txt）转为相对条目路径
    ///
    /// ## 两个必须注意的坑（都曾导致文件名被篡改）
    /// 1. **不要在这里再解码一次**。`parse_multistatus` 已经把 href 解码过了；
    ///    二次解码会把 `%2541`（原文件名里的 `%41`）变成 `A`，于是
    ///    `report%2541.txt` 被认成 `reportA.txt` —— 保留策略会误删「找不到」的文件。
    /// 2. **先去查询串再解码**。顺序反了的话，文件名里合法的 `%3F`（即 `?`）
    ///    会被当成查询串截断，名字被切掉一半。
    fn href_to_rel(&self, href: &str) -> Option<String> {
        // 先剥查询串（此时还是百分号编码态，`?` 只可能来自服务端而非文件名）
        let href = match href.find('?') {
            Some(i) => &href[..i],
            None => href,
        };
        // **唯一的解码点**：href 到这里仍是 percent-encoded 形态
        let p = percent_decode_path(href);
        let p = p.as_str();
        let is_dir = p.ends_with('/');
        let p = p.trim_end_matches('/');
        let base = self.base_path.as_str();
        let rel = if !base.is_empty() {
            // 必须连 `/` 一起剥：否则 base="/dav" 时 "/dav-other/x" 会被剥成 "-other/x"
            let with_slash = format!("{base}/");
            if p == base {
                return None; // 就是前缀本身（根目录）
            }
            p.strip_prefix(&with_slash)?
        } else {
            p
        };
        let rel = rel.trim_start_matches('/');
        if rel.is_empty() {
            return None; // 根自身
        }
        let mut rel = rel.to_string();
        if is_dir {
            rel.push('/');
        }
        Some(rel)
    }
}

#[async_trait]
impl TargetStorage for WebdavTarget {
    async fn write_stream(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
    ) -> StorageResult<()> {
        self.write_inner(path, stream, None, None).await
    }

    async fn write_stream_progress(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
        progress: ProgressCb,
    ) -> StorageResult<()> {
        self.write_inner(path, stream, None, Some(progress)).await
    }

    /// 已知密文总长：**边收边发，全程不落盘**（唯一会真正走上分片/精确长度校验的入口）
    async fn write_stream_sized(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
        total: u64,
        progress: ProgressCb,
    ) -> StorageResult<()> {
        self.write_inner(path, stream, Some(total), Some(progress)).await
    }

    async fn read_stream(
        &self,
        path: &Path,
    ) -> StorageResult<Box<dyn Stream<Item = StorageResult<Bytes>> + Send + Unpin>> {
        // 先尝试逻辑文件（小文件）；404 则按序拼接 .part0001… 分片
        match self.get_stream_raw(path).await {
            Ok(stream) => Ok(stream),
            Err(StorageError::NotFound(_)) => {
                let this = self.clone();
                let path = path.to_path_buf();
                let (tx, rx) = tokio::sync::mpsc::channel::<StorageResult<Bytes>>(4);
                tokio::spawn(async move {
                    // 上限防御：拼接靠「GET 到 404」终止。若服务端对缺失分片返回 2xx，
                    // 这里会无限 GET 直到任务被外部杀掉（同 cleanup_parts 的风险）。
                    const MAX_PARTS: usize = 100_000;
                    // ⚠️ 必须用「是否因 404 正常收尾」区分两种走完循环的情况：
                    //
                    // 旧写法是 `for idx in 1..=MAX_PARTS { ... }` 后**无条件**发一条
                    // 「分片数量超过上限」错误。于是任何**正常结束**的多分片文件
                    // （拼到最后一个分片、下一次 GET 命中 404 而 `break`）在读完数据后
                    // 仍会收到这条错误 —— 表现为大文件恢复必然失败在最后一片，
                    // 报「分片数量超过上限 100000」，而文件其实已经完整读到了。
                    let mut ended_by_not_found = false;
                    for idx in 1..=MAX_PARTS {
                        let part = this.part_path(&path, idx);
                        match this.get_stream_raw(&part).await {
                            Ok(mut st) => {
                                while let Some(chunk) = st.next().await {
                                    if tx.send(chunk).await.is_err() {
                                        return; // 接收端已关闭
                                    }
                                }
                            }
                            Err(StorageError::NotFound(_)) => {
                                if idx == 1 {
                                    // 无逻辑文件也无分片：确为不存在
                                    let _ = tx
                                        .send(Err(StorageError::NotFound(
                                            path.to_string_lossy().into_owned(),
                                        )))
                                        .await;
                                }
                                ended_by_not_found = true;
                                break;
                            }
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                return;
                            }
                        }
                    }
                    // 只有**真正把 idx 跑到上限**（从未命中 404）才是「分片过多」。
                    if !ended_by_not_found {
                        let _ = tx
                            .send(Err(StorageError::Protocol(format!(
                                "{}: 分片数量超过上限 {MAX_PARTS}（服务端可能对缺失分片未返回 404）",
                                path.display()
                            ))))
                            .await;
                    }
                });
                Ok(Box::new(tokio_stream::wrappers::ReceiverStream::new(rx)))
            }
            Err(e) => Err(e),
        }
    }

    async fn delete(&self, path: &Path) -> StorageResult<()> {
        // 删除逻辑文件 + 全部分片。
        //
        // 语义：**只有「逻辑文件与分片都不存在」才返回 `NotFound`** ——
        // 保留策略据此区分「真的删掉了一个」与「本来就没有」（用于可信的清理统计）。
        //
        // 注意不能「逻辑文件 404 就提前 return」：那样会漏清分片，
        // 而分片存在时该文件其实仍在云端（读路径会把它拼回来）。
        let logical = self.delete_raw(path).await;
        // 分片清理失败不应掩盖「逻辑文件已删」这件事，但也必须报出来
        let parts = self.cleanup_parts(path).await;
        match (logical, parts) {
            // **逻辑文件删成功，但分片清理失败 ⇒ 必须上报**。
            //
            // 旧写法是 `(Ok(()), _) => Ok(())` —— 那条通配 `_` 会把
            // `cleanup_parts` 的 `Err` 一起吞掉（编译器为此报了
            // `unreachable pattern`：最后一行 `(_, Err(e))` 永远走不到）。
            // 后果与「覆盖写残留旧分片」完全一样：云端仍留着 part000N，
            // `list()` 会把它们拼回这个逻辑文件，恢复时得到**被截断或拼错的数据**，
            // 而删除操作却回报成功。
            (Ok(()), Ok(_)) => Ok(()),
            (Ok(()), Err(e)) => Err(e),
            // 逻辑文件删失败，但确实清掉了若干分片 ⇒ 视为存在过并已清理
            (_, Ok(n)) if n > 0 => Ok(()),
            // 逻辑文件不存在，且一个分片都没删到 ⇒ 确实不存在
            (Err(StorageError::NotFound(_)), Ok(0)) => {
                Err(StorageError::NotFound(path.to_string_lossy().into_owned()))
            }
            (Err(e), _) => Err(e),
        }
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<FileDescriptor>> {
        // Depth 1：列出 prefix 目录的直系子项
        let dir = prefix.trim_matches('/');
        let entries = self.propfind(dir, "1").await?;
        let self_rel = format!("{}/", dir);
        let mut out = Vec::new();
        // 分片合并表：逻辑名 → 累计大小
        // 逻辑名 → 分片聚合信息
        struct PartAgg {
            /// 累计大小
            size: u64,
            /// 各分片里最新的 mtime
            mtime: Option<std::time::SystemTime>,
            /// 是否见过 `part0001`（我们自己写的分片序列必从它开始）
            has_first: bool,
            /// 原始条目（用于「孤立分片」时原样吐出，避免把用户文件吞掉）
            raw: Vec<FileDescriptor>,
        }
        let mut part_map: std::collections::BTreeMap<String, PartAgg> =
            std::collections::BTreeMap::new();
        for e in entries {
            let Some(rel) = self.href_to_rel(&e.href) else {
                continue;
            };
            let is_dir = e.is_dir || rel.ends_with('/');
            let rel = rel.trim_end_matches('/').to_string();
            if rel == self_rel.trim_end_matches('/') || rel.is_empty() {
                continue; // 跳过目录自身
            }
            if is_dir {
                out.push(FileDescriptor {
                    rel_path: rel,
                    size: 0,
                    modified: e.modified,
                    is_dir: true,
                    digest: None,
                });
                continue;
            }
            // 分片文件合并进逻辑条目
            if let Some((logical, idx)) = split_part_name(&rel) {
                let slot = part_map.entry(logical).or_insert(PartAgg {
                    size: 0,
                    mtime: None,
                    has_first: false,
                    raw: Vec::new(),
                });
                slot.size += e.size;
                if idx == 1 {
                    slot.has_first = true;
                }
                // 取各分片里最新的 mtime：等价于「这个逻辑文件最后被写的时间」
                if let Some(m) = e.modified {
                    slot.mtime = Some(match slot.mtime {
                        Some(prev) if prev >= m => prev,
                        _ => m,
                    });
                }
                // 留一份原始条目：万一是「孤立分片」，要原样吐出去
                slot.raw.push(FileDescriptor {
                    rel_path: rel,
                    size: e.size,
                    modified: e.modified,
                    is_dir: false,
                    digest: None,
                });
                continue;
            }
            out.push(FileDescriptor {
                rel_path: rel,
                size: e.size,
                modified: e.modified,
                is_dir: false,
                digest: None,
            });
        }
        // 分片合并为逻辑文件
        //
        // ## 为什么要求「必须见过 part0001」
        // 折叠的判据此前是「文件名形如 `xxx.partNNNN`」。这会把用户真实存在的
        // 同名普通文件（如他自己放的 `report.part0001`）当成我们的分片吞掉 ——
        // 它从列表里消失，恢复时按 `report` 去 GET 必然 404，`delete(report)`
        // 还会误删它。
        //
        // 我们写的分片序列**永远从 part0001 开始**，所以「存在 part0001」
        // 是可靠判据；没有它的孤立分片一律当普通文件原样保留。
        for (logical, agg) in part_map {
            // 已有同名逻辑文件（小文件直传的形态）⇒ 以它为准，忽略残留分片
            if out.iter().any(|f| f.rel_path == logical) {
                continue;
            }
            if !agg.has_first {
                // 孤立分片：原样保留（这是用户的文件，不是我们的分片）
                out.extend(agg.raw);
                continue;
            }
            out.push(FileDescriptor {
                rel_path: logical,
                size: agg.size,
                modified: agg.mtime,
                is_dir: false,
                digest: None,
            });
        }
        Ok(out)
    }

    async fn ensure_dir(&self, path: &Path) -> StorageResult<()> {
        self.mkcol_chain(path).await
    }

    async fn ping(&self) -> StorageResult<()> {
        self.propfind("", "0").await.map(|_| ())
    }
}

/// 路径百分号编码（保留 unreserved 与 '/'）
fn percent_encode_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 百分号解码（%XX → 字节，按 UTF-8 还原）
fn percent_decode_path(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 解析 HTTP 日期（RFC 1123，如 `Wed, 21 Oct 2015 07:28:00 GMT`）
///
/// 只用于保留策略判年龄，容错即可（解析失败返回 `None`，由调用方保守处理）。
/// 用 `chrono` 解析：仓库已依赖它，不必新增 `httpdate`。
fn parse_http_date(s: &str) -> Option<std::time::SystemTime> {
    let dt = chrono::DateTime::parse_from_rfc2822(s)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(s))
        .ok()?;
    let secs = dt.timestamp();
    if secs < 0 {
        return None;
    }
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64))
}

/// 解析 PROPFIND multistatus XML（容错解析，不依赖 XML 库）
///
/// 提取每个 `<D:response>` 块中的 href / collection / getcontentlength。
/// 对命名空间前缀大小写不敏感（D:/d:/DAV:/无前缀）。
fn parse_multistatus(xml: &str) -> Vec<DavEntry> {
    let mut entries = Vec::new();
    let mut cur_href: Option<String> = None;
    let mut cur_dir = false;
    let mut cur_size: u64 = 0;
    let mut cur_mtime: Option<std::time::SystemTime> = None;

    for tok in xml.split('<') {
        let Some(i) = tok.find('>') else { continue };
        let raw = tok[..i].trim();
        let rest = &tok[i + 1..];
        if raw.starts_with('?') || raw.starts_with('!') {
            continue; // <?xml ...?> 或 <!-- -->
        }
        let closing = raw.starts_with('/');
        let self_closing = raw.ends_with('/');
        let name = raw
            .trim_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let name = match name.strip_prefix("dav:") {
            Some(n) => n.to_string(),
            None => match name.strip_prefix("d:") {
                Some(n) => n.to_string(),
                None => name,
            },
        };
        match name.as_str() {
            "response" if closing => {
                if let Some(href) = cur_href.take() {
                    entries.push(DavEntry {
                        href,
                        is_dir: cur_dir,
                        size: cur_size,
                        modified: cur_mtime.take(),
                    });
                }
                cur_dir = false;
                cur_size = 0;
                cur_mtime = None;
            }
            "href" if !closing && !self_closing => {
                // **保留原始（percent-encoded）形态**，解码统一放在 `href_to_rel`。
                //
                // 这里是「只解码一次」这个不变式的唯一保证点：若在此处解码、
                // `href_to_rel` 又解一次，`%2541`（文件名里的 `%41`）会被解成 `A`，
                // 文件名被篡改，保留策略随后会把它当作孤儿删除。
                cur_href = Some(rest.trim().to_string());
            }
            "getlastmodified" if !closing && !self_closing => {
                cur_mtime = parse_http_date(rest.trim());
            }
            "collection" if !closing => {
                cur_dir = true;
            }
            "getcontentlength" if !closing && !self_closing => {
                cur_size = rest.trim().parse().unwrap_or(0);
            }
            _ => {}
        }
    }
    entries
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use crate::infra::target::mock_dav::MockDav;
    use std::sync::Arc;

    fn target_for(mock: &MockDav) -> WebdavTarget {
        // `require_auth` 只影响真实服务的行为；这里凭据随便给（Basic 头总会带上）
        let _ = mock.require_auth;
        WebdavTarget::new(&format!("http://{}/dav", mock.addr), "u", "p")
    }

    fn chunks_of(data: &[u8], n: usize) -> Box<dyn Stream<Item = Bytes> + Send + Unpin> {
        let v: Vec<Bytes> = data.chunks(n).map(Bytes::copy_from_slice).collect();
        Box::new(futures::stream::iter(v))
    }

    /// 小文件：一次 PUT 到逻辑路径，`Content-Length` 精确等于流的总长
    #[tokio::test]
    async fn small_file_streams_with_exact_content_length() {
        let mock = MockDav::start(false).await;
        let target = target_for(&mock);
        let data = vec![0xABu8; 300 * 1024];
        let total = data.len() as u64;
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        target
            .write_stream_sized(
                Path::new("fn-backup/Config/sn.img.age"),
                chunks_of(&data, 7 * 1024),
                total,
                noop,
            )
            .await
            .expect("上传应成功");

        let puts = mock.puts.lock().unwrap().clone();
        assert_eq!(puts.len(), 1, "小文件应只有一次 PUT（不该产生分片）");
        let (path, cl, body) = &puts[0];
        assert!(path.ends_with("Config/sn.img.age"), "逻辑路径错了: {path}");
        assert_eq!(*cl, Some(total), "Content-Length 必须精确等于密文总长");
        assert_eq!(*body, total, "实收字节数必须与总长一致");
    }

    /// 大文件：按 part_size 切片，每片 Content-Length 精确，合计等于总长
    #[tokio::test]
    async fn large_file_is_partitioned_without_staging_file() {
        let mock = MockDav::start(false).await;
        let target = target_for(&mock);
        // 直接给 64KiB 分片，避免真的发 90MiB（也避免改环境变量污染并行测试）
        let data: Vec<u8> = (0..200 * 1024).map(|i| (i % 251) as u8).collect();
        let total = data.len() as u64;
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        target
            .write_inner_with_part(
                Path::new("fn-backup/big.bin"),
                chunks_of(&data, 3000),
                Some(total),
                Some(noop),
                64 * 1024,
            )
            .await
            .expect("上传应成功");

        let puts = mock.puts.lock().unwrap().clone();
        assert!(puts.len() >= 3, "200KiB / 64KiB 应切成 >=3 片，实际 {}", puts.len());
        for (path, cl, body) in &puts {
            assert!(path.contains(".part"), "大文件必须走分片路径: {path}");
            assert_eq!(*cl, Some(*body), "每片 Content-Length 必须等于该片实际长度");
            assert!(*body <= 64 * 1024, "单片不该超过 part_size");
        }
        assert_eq!(mock.total_bytes(), total, "所有分片合计必须等于总长");
    }

    /// 流被截断（少于声明总长）时必须报错，而不是静默写少
    #[tokio::test]
    async fn truncated_stream_is_rejected() {
        let mock = MockDav::start(false).await;
        let target = target_for(&mock);
        let data = vec![1u8; 10 * 1024];
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        let err = target
            .write_stream_sized(
                Path::new("fn-backup/x.age"),
                chunks_of(&data, 1024),
                20 * 1024, // 声明的总长 > 实际流
                noop,
            )
            .await
            .expect_err("总长与实收不一致必须报错");
        let msg = err.to_string();
        assert!(msg.contains("不一致"), "错误信息应指出不一致: {msg}");
    }

    /// **回归**：真实带宽下，进度回调必须在传输**过程中**就不断到达。
    ///
    /// 这对应线上「备份成功但速度不显示」：宿主的上游 `SpeedMeter` 只统计收到的
    /// 进度观测，若目标端在整片上传期间一次都不回调，整个分片的耗时只能等
    /// PUT 返回后才被记为一笔 —— 期间速度恒为 0（前端显示「—」）。
    #[tokio::test]
    async fn progress_arrives_during_transfer_not_only_at_end() {
        // 每 64KiB 延迟 20ms：256KiB 分片约需 80ms，足以产生多次中途观测
        let mock = MockDav::start_with(false, std::time::Duration::from_millis(20)).await;
        let target = target_for(&mock);
        let data = vec![3u8; 256 * 1024];
        let total = data.len() as u64;

        let observations = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
        let obs = observations.clone();
        let cb: ProgressCb = Arc::new(move |written, _t, _ms| {
            obs.lock().unwrap().push(written);
        });
        target
            .write_inner_with_part(
                Path::new("fn-backup/prog.bin"),
                chunks_of(&data, 16 * 1024),
                Some(total),
                Some(cb),
                256 * 1024,
            )
            .await
            .expect("上传应成功");

        let seen = observations.lock().unwrap().clone();
        assert!(
            seen.len() > 1,
            "传输过程中应有多次进度观测，实际只有 {} 次：{seen:?}",
            seen.len()
        );
        // 中途出现的观测必须严格小于总长（说明不是「结束后补一笔」）
        let mid: Vec<u64> = seen.iter().copied().filter(|w| *w < total).collect();
        assert!(
            !mid.is_empty(),
            "必须在传输过程中就回调（而不是只在结束时补一次），实际观测：{seen:?}"
        );
        assert_eq!(*seen.last().unwrap(), total, "最后一次必须是总长");
    }

    /// 进度回调单调递增，且最终等于总长
    #[tokio::test]
    async fn progress_reaches_total() {
        let mock = MockDav::start(false).await;
        let target = target_for(&mock);
        let data = vec![9u8; 100 * 1024];
        let total = data.len() as u64;
        let seen = Arc::new(AtomicU64::new(0));
        let seen_cb = seen.clone();
        let cb: ProgressCb = Arc::new(move |written, _t, _ms| {
            seen_cb.fetch_max(written, Ordering::Relaxed);
        });
        target
            .write_stream_sized(Path::new("fn-backup/p.bin"), chunks_of(&data, 4096), total, cb)
            .await
            .expect("上传应成功");
        assert_eq!(seen.load(Ordering::Relaxed), total, "进度最终值应等于总长");
    }
}

#[cfg(test)]
mod proxy_e2e_tests {
    use super::*;
    use crate::infra::target::mock_dav::MockDav;
    use std::sync::Arc;

    /// 极简 HTTP 转发代理（够用即可）
    ///
    /// 为什么不能只回一个假响应：reqwest 走代理时会发**绝对 URI**，
    /// 且 PUT 带 body；假代理若不读完 body、不转发，客户端会卡在等响应上
    /// （实测会一直挂到测试超时）。所以这里真的转发到目标服务器。
    ///
    /// 记录每条请求的首行，用于断言「流量确实经过代理」。
    async fn start_recording_proxy() -> (std::net::SocketAddr, Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_for = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let seen = seen_for.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut head = Vec::new();
                    let mut tmp = vec![0u8; 32 * 1024];
                    // 读请求头
                    let (text, body_start) = loop {
                        let n = match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        head.extend_from_slice(&tmp[..n]);
                        if let Some(p) = head.windows(4).position(|w| w == b"\r\n\r\n") {
                            break (String::from_utf8_lossy(&head[..p]).to_string(), p + 4);
                        }
                        if head.len() > 1 << 20 {
                            return;
                        }
                    };
                    let first = text.lines().next().unwrap_or("").to_string();
                    seen.lock().unwrap().push(first.clone());

                    // `PUT http://host:port/path HTTP/1.1` → 取出目标
                    let mut parts = first.split_whitespace();
                    let _method = parts.next().unwrap_or("");
                    let abs = parts.next().unwrap_or("");
                    let url = match reqwest::Url::parse(abs) {
                        Ok(u) => u,
                        Err(_) => {
                            let _ = sock
                                .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
                                .await;
                            return;
                        }
                    };
                    let host = url.host_str().unwrap_or("127.0.0.1");
                    let port = url.port_or_known_default().unwrap_or(80);
                    // 取 body（按 Content-Length 读完，否则客户端会等）
                    let cl: usize = text
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.trim().eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    let mut body = head[body_start..].to_vec();
                    while body.len() < cl {
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => body.extend_from_slice(&tmp[..n]),
                        }
                    }
                    body.truncate(cl);

                    // 真的转发给目标
                    let mut out = match tokio::net::TcpStream::connect((host, port)).await {
                        Ok(c) => c,
                        Err(_) => {
                            let _ = sock
                                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                                .await;
                            return;
                        }
                    };
                    // 转成 origin-form 再发给目标
                    let path = if url.query().is_some() {
                        format!("{}?{}", url.path(), url.query().unwrap())
                    } else {
                        url.path().to_string()
                    };
                    let mut fwd = format!("{_method} {path} HTTP/1.1\r\n");
                    for l in text.lines().skip(1) {
                        let keep = !l.to_ascii_lowercase().starts_with("proxy-");
                        if keep && !l.is_empty() {
                            fwd.push_str(l);
                            fwd.push_str("\r\n");
                        }
                    }
                    fwd.push_str("Connection: close\r\n\r\n");
                    if out.write_all(fwd.as_bytes()).await.is_err() {
                        return;
                    }
                    if out.write_all(&body).await.is_err() {
                        return;
                    }
                    let _ = out.flush().await;
                    // 回传目标响应。**不能**用 `read_to_end`：目标用 keep-alive 时不会关
                    // 连接，`read_to_end` 会一直等到超时（实测直接挂住整个测试）。
                    // 这里按响应的 Content-Length 精确读。
                    let mut resp_head = Vec::new();
                    let (rhead, rbody_start) = loop {
                        let n = match out.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        resp_head.extend_from_slice(&tmp[..n]);
                        if let Some(p) = resp_head.windows(4).position(|w| w == b"\r\n\r\n") {
                            break (String::from_utf8_lossy(&resp_head[..p]).to_string(), p + 4);
                        }
                    };
                    let rcl: usize = rhead
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    let mut rbody = resp_head[rbody_start..].to_vec();
                    while rbody.len() < rcl {
                        match out.read(&mut tmp).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => rbody.extend_from_slice(&tmp[..n]),
                        }
                    }
                    rbody.truncate(rcl);
                    // 统一改成 Connection: close，避免客户端复用这条已转发过的连接
                    let mut fixed = String::new();
                    for l in rhead.lines() {
                        if !l.to_ascii_lowercase().starts_with("connection:") {
                            fixed.push_str(l);
                            fixed.push_str("\r\n");
                        }
                    }
                    fixed.push_str("Connection: close\r\n\r\n");
                    let _ = sock.write_all(fixed.as_bytes()).await;
                    let _ = sock.write_all(&rbody).await;
                    let _ = sock.flush().await;
                });
            }
        });
        (addr, seen)
    }

    /// 配了代理的 WebdavTarget 必须把请求发给代理（而不是直连目标）
    ///
    /// 用 `ping()`（小请求、响应体短）而不是大 PUT：本测试只关心「流量是否经过代理」
    /// 这一条，不必把分片上传也卷进来（那会让假代理的实现复杂度盖过被测逻辑）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn proxied_target_sends_traffic_through_proxy() {
        let (proxy_addr, seen) = start_recording_proxy().await;
        let mock = MockDav::start(false).await;
        let proxy = reqwest::Proxy::all(format!("http://{proxy_addr}")).expect("代理地址应合法");
        let target = WebdavTarget::new(&format!("http://{}/dav", mock.addr), "u", "p")
            .with_proxy(Some(proxy));

        let _ = target.ping().await;

        let hits = seen.lock().unwrap().clone();
        assert!(!hits.is_empty(), "流量没有经过代理（代理未收到任何请求）");
        // 走代理时请求行用的是**绝对 URI**，这是最硬的特征
        assert!(
            hits[0].contains("http://"),
            "代理收到的首行不是绝对 URI，说明没有真正走代理：{}",
            hits[0]
        );
    }

    /// 未配代理时必须直连（对照组，证明上面的测试确实在测代理）
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn direct_target_bypasses_proxy() {
        let (proxy_addr, seen) = start_recording_proxy().await;
        let mock = MockDav::start(false).await;
        let _ = proxy_addr;
        let target = WebdavTarget::new(&format!("http://{}/dav", mock.addr), "u", "p");
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        let data = b"hello-direct".to_vec();
        target
            .write_stream_sized(
                Path::new("fn-backup/y.age"),
                Box::new(futures::stream::iter(vec![Bytes::from(data.clone())])),
                data.len() as u64,
                noop,
            )
            .await
            .expect("直连上传应成功");
        assert_eq!(mock.total_bytes(), data.len() as u64, "直连应打到 mock");
        assert!(seen.lock().unwrap().is_empty(), "未配代理时不该有流量经过代理");
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;
    use crate::infra::target::mock_dav::MockDav;
    use std::sync::Arc;

    /// **反卡死回归**：服务端接受连接但**永不回响应**时，请求必须超时失败
    ///
    /// 线上症状：恢复任务停在「GET ... 第 1/3 次失败」之后不再前进，既没成功也没失败。
    /// 根因是 `connect_timeout` 只管建连，连上后不响应就无限等待。
    ///
    /// 这里注入 300ms 的首包超时（真实值 120s，测试不能等那么久），
    /// 断言它在**远小于外层 10s** 的时间内返回错误 —— 证明超时链路真的生效。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn first_byte_timeout_actually_fires() {
        // 假服务端：accept 后攥住连接、永不回响应
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = listener.accept().await {
                held.push(s);
            }
        });

        let target = WebdavTarget::new(&format!("http://{addr}/dav"), "u", "p");
        let started = std::time::Instant::now();
        let rb = target.request(Method::from_bytes(b"PROPFIND").unwrap(), &format!("http://{addr}/dav"));
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            target.send_retry_with(rb, "PROPFIND", std::time::Duration::from_millis(300)),
        )
        .await;
        let took = started.elapsed();
        assert!(out.is_ok(), "外层 10s 就超时了，说明内层超时没生效（仍在无限等待）");
        let err = out.unwrap().expect_err("服务端不响应时必须报错");
        assert!(
            err.to_string().contains("超时"),
            "错误信息应指出超时：{err}"
        );
        // 3 次重试 × 300ms + 退避 ≈ 1.5s 内应结束
        assert!(took < std::time::Duration::from_secs(5), "耗时 {took:?} 偏长");
    }

    /// **响应体停顿回归**：头拿到了、体传到一半对端静默，必须报错而非永久挂住
    ///
    /// 这是「卡住」的第二个来源：`FIRST_BYTE_TIMEOUT` 只保护拿响应头，
    /// 体传输中途停顿此前完全没有超时。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stalled_download_body_errors_instead_of_hanging() {
        // 假服务端：发完响应头就停止发数据（体只发一半）
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut held = Vec::new();
            while let Ok((mut s, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = s.read(&mut buf).await; // 读掉请求头
                // 声称 1MB，但只发 1KB 就静默
                let _ = s
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\n\r\n",
                    )
                    .await;
                let _ = s.write_all(&vec![0u8; 1024]).await;
                let _ = s.flush().await;
                held.push(s); // 攥住不关，体永远传不完
            }
        });

        let target = WebdavTarget::new(&format!("http://{addr}/dav"), "u", "p");
        // 注入 500ms 的体停顿超时（真实值 60s，测试不能等那么久）
        let mut st = target
            .get_stream_raw_with_stall(
                Path::new("fn-backup/x.age"),
                std::time::Duration::from_millis(500),
            )
            .await
            .expect("拿响应头应成功");

        // 逐块读：先读到那 1KB，随后应因停顿报错（而不是永久挂住或静默结束）
        let mut got_err = false;
        let mut silent_end = false;
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), st.next()).await {
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(e))) => {
                    got_err = true;
                    assert!(
                        e.to_string().contains("停顿") || e.to_string().contains("中断"),
                        "错误信息应指出停顿/中断：{e}"
                    );
                    break;
                }
                Ok(None) => {
                    silent_end = true;
                    break;
                }
                Err(_) => break, // 外层 5s 超时：说明内层没生效
            }
        }
        assert!(!silent_end, "体停顿不能静默结束（会被当作正常 EOF，产生截断文件）");
        assert!(got_err, "体停顿必须产生错误，而不是永久挂住");
    }

    /// 对照：大文件 GET 不应被首包超时误伤
    ///
    /// 这条防的是「修反卡死时把超时加在整个请求上」——那会在下载中途掐断大文件。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn large_download_is_not_cut_by_first_byte_timeout() {
        // 每 64KiB 延迟 5ms：8MB 约需 640ms，跨越多个读块
        let mock = MockDav::start_with(false, std::time::Duration::from_millis(5)).await;
        let target = WebdavTarget::new(&format!("http://{}/dav", mock.addr), "u", "p");
        let data: Vec<u8> = vec![0x11u8; 8 * 1024 * 1024];
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        target
            .write_stream_sized(
                Path::new("fn-backup/big.age"),
                Box::new(futures::stream::iter(vec![Bytes::from(data.clone())])),
                data.len() as u64,
                noop,
            )
            .await
            .expect("写入应成功");

        let mut st = target
            .read_stream(Path::new("fn-backup/big.age"))
            .await
            .expect("读应成功");
        let mut total = 0u64;
        while let Some(c) = st.next().await {
            total += c.expect("块应有效").len() as u64;
        }
        assert_eq!(total, data.len() as u64, "大文件必须完整读回，不能被超时截断");
    }
}

#[cfg(test)]
mod overwrite_tests {
    use super::*;
    use crate::infra::target::mock_dav::MockDav;
    use std::sync::Arc;

    fn target_for(mock: &MockDav) -> WebdavTarget {
        WebdavTarget::new(&format!("http://{}/dav", mock.addr), "u", "p")
    }

    /// **文件名保真回归**：含 `%` 与 `?` 的文件名不能被篡改
    ///
    /// 两个曾经存在的缺陷：
    /// 1. href 被**解码两次** ⇒ `report%41.txt` 变成 `reportA.txt`
    ///    （保留策略随后会把它当作「快照里没有」的孤儿删掉）
    /// 2. **先解码再剥查询串** ⇒ 文件名里合法的 `%3F`(即 `?`) 被切断
    #[test]
    fn href_with_percent_and_question_mark_is_preserved() {
        let t = WebdavTarget::new("https://dav.example.com/dav", "u", "p");

        // 真实文件名是 `report%41.txt`，服务端 href 会把它编码成 `report%2541.txt`
        //（`%` → `%25`）。解码**恰好一次**后应得回 `report%41.txt`；
        // 若解码两次就会变成 `reportA.txt`（文件名被篡改）。
        assert_eq!(
            t.href_to_rel("/dav/fn-backup/report%2541.txt").as_deref(),
            Some("fn-backup/report%41.txt"),
            "href 必须只解码一次：%2541 → %41（不能变成 A）"
        );
        // 文件名含 `?` 时，服务端 href 会编码成 `%3F`；剥查询串必须在解码**之前**，
        // 否则解码出来的 `?` 会被误当成查询串而起名字被切断。
        assert_eq!(
            t.href_to_rel("/dav/fn-backup/what%3F.txt").as_deref(),
            Some("fn-backup/what?.txt"),
            "文件名里的 %3F 解码后是 ?，属于文件名，不能被截断"
        );
        // 真正的查询串仍应被剥掉
        assert_eq!(
            t.href_to_rel("/dav/fn-backup/a.txt?version=2").as_deref(),
            Some("fn-backup/a.txt")
        );
        // 前缀边界：/dav-other 不该被当成 /dav 之下
        assert_eq!(t.href_to_rel("/dav-other/x.txt"), None);
        // 普通路径照常
        assert_eq!(
            t.href_to_rel("/dav/fn-backup/a b.txt").as_deref(),
            Some("fn-backup/a b.txt")
        );
    }

    /// **数据损坏回归**：大文件变小后再备份，旧分片必须被清掉
    ///
    /// 场景：文件曾 5 片，被截断后变成 3 片。若只覆盖 part0001..part0003 而不删
    /// part0004/part0005，`read_stream` 会「从 1 逐个 GET 到 404」把 5 片拼起来 ——
    /// 备份显示成功，恢复却拿到**拼接错误的密文**，解密失败或产出错误明文。
    ///
    /// 注意：这是 large→smaller-large 的路径，此前的清理只覆盖了
    /// large→small文件 与 small→large文件 两种。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shrinking_large_file_removes_stale_tail_parts() {
        let mock = MockDav::start(false).await;
        let target = target_for(&mock);
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        let part = 64 * 1024usize; // 用 64KiB 分片便于测试

        // 第一次：5 片（= 320KiB）
        let big: Vec<u8> = vec![0xAAu8; part * 5];
        target
            .write_inner_with_part(
                Path::new("fn-backup/shrink.bin"),
                Box::new(futures::stream::iter(vec![Bytes::from(big.clone())])),
                Some(big.len() as u64),
                Some(noop.clone()),
                part,
            )
            .await
            .expect("第一次上传应成功");
        assert_eq!(mock.total_bytes(), big.len() as u64);

        // 第二次：同样是大文件，但只有 3 片（= 192KiB）
        let small: Vec<u8> = vec![0xBBu8; part * 3];
        target
            .write_inner_with_part(
                Path::new("fn-backup/shrink.bin"),
                Box::new(futures::stream::iter(vec![Bytes::from(small.clone())])),
                Some(small.len() as u64),
                Some(noop),
                part,
            )
            .await
            .expect("第二次上传应成功");

        // 云端现在只应保有 3 片：读回来的内容必须与第二次写的完全一致
        let mut st = target
            .read_stream(Path::new("fn-backup/shrink.bin"))
            .await
            .expect("读应成功");
        let mut got = Vec::new();
        while let Some(c) = st.next().await {
            got.extend_from_slice(&c.expect("块应有效"));
        }
        assert_eq!(
            got.len(),
            small.len(),
            "读回长度 {} 与最新内容 {} 不符 —— 旧分片未被清理（会把旧数据拼进来）",
            got.len(),
            small.len()
        );
        assert!(got == small, "读回内容必须是最后一次写入的数据");
    }
}

#[cfg(test)]
mod proxy_test_endpoint_tests {
    use super::*;
    use crate::infra::target::mock_dav::MockDav;
    use std::sync::Arc;

    /// 「测试代理」的核心断言：请求**确实经过了代理**
    ///
    /// 这是该按钮存在的唯一意义 —— 只校验地址格式回答不了
    /// 「代理进程在不在、端口通不通、认证对不对」。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_endpoint_traffic_goes_through_proxy() {
        let (proxy_addr, seen) = start_recording_proxy().await;
        let mock = MockDav::start(false).await;
        let target = format!("http://{}/dav", mock.addr);

        // 模拟 proxy_test 的客户端构建方式
        let proxy = reqwest::Proxy::all(format!("http://{proxy_addr}")).expect("代理应合法");
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(20))
            .http1_only()
            .proxy(proxy)
            .build()
            .expect("客户端应能构建");

        // 与实现一致：发 HEAD，拿到任何响应即算链路通
        let _ = client.head(&target).send().await;

        let hits = seen.lock().unwrap().clone();
        assert!(
            !hits.is_empty(),
            "测试请求没有经过代理（代理未收到任何请求）"
        );
        // 走代理时请求行使用绝对 URI
        assert!(
            hits[0].contains("http://"),
            "代理收到的首行不是绝对 URI：{}",
            hits[0]
        );
    }

    /// 代理**不可用**时应当报错，而不是误报成功
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_endpoint_fails_when_proxy_unreachable() {
        // 指向一个必然连不上的端口
        let proxy = reqwest::Proxy::all("http://127.0.0.1:1").expect("地址格式合法");
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(2))
            .timeout(std::time::Duration::from_secs(3))
            .http1_only()
            .proxy(proxy)
            .build()
            .expect("客户端应能构建");
        let r = client.head("http://example.invalid/dav").send().await;
        assert!(r.is_err(), "代理不可用时必须报错（否则用户会以为配好了）");
    }

    /// 复用既有的「记录型转发代理」测试工具
    async fn start_recording_proxy(
    ) -> (std::net::SocketAddr, Arc<std::sync::Mutex<Vec<String>>>) {
        // 直接复用 proxy_e2e_tests 里的实现思路：极简转发代理
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_for = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let seen = seen_for.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 16 * 1024];
                    let n = match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    let text = String::from_utf8_lossy(&buf[..n]).to_string();
                    let first = text.lines().next().unwrap_or("").to_string();
                    seen.lock().unwrap().push(first.clone());
                    // 回一个最小合法响应即可（本测试只关心「是否经过代理」）
                    let _ = sock
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                    let _ = sock.flush().await;
                });
            }
        });
        (addr, seen)
    }
}

#[cfg(test)]
mod proxy_validation_tests {
    use super::*;

    /// 代理地址校验：格式必须严格到足以拦住「漏写 scheme」这类常见错误
    ///
    /// 背景：`reqwest::Proxy::all` 对非法串**不报错**（把整串当主机名），
    /// 所以「保存时能通过、备份时才失败」曾是常态。测试接口要靠这个校验
    /// 在用户点「测试」时立刻给出可操作的提示。
    #[test]
    fn validate_rejects_common_mistakes() {
        // 缺 scheme：最常见的错法
        assert!(validate_proxy_url("192.168.1.2:7890").is_err());
        assert!(validate_proxy_url("127.0.0.1:1080").is_err());
        // 不支持的协议
        assert!(validate_proxy_url("ftp://1.2.3.4:21").is_err());
        // 空与纯空白
        assert!(validate_proxy_url("").is_err());
        assert!(validate_proxy_url("   ").is_err());
        // 合法写法（四种协议都要支持）
        assert!(validate_proxy_url("http://192.168.1.2:7890").is_ok());
        assert!(validate_proxy_url("https://proxy.example.com:8443").is_ok());
        assert!(validate_proxy_url("socks5://127.0.0.1:1080").is_ok());
        assert!(validate_proxy_url("socks5h://127.0.0.1:1080").is_ok());
    }
}

#[cfg(test)]
mod list_folding_tests {
    use super::*;
    use crate::infra::target::mock_dav::MockDav;

    /// **用户文件不被吞掉**：没有 part0001 的孤立 `.partNNNN` 必须原样列出
    ///
    /// 折叠判据此前是「文件名形如 xxx.partNNNN」。用户若真有一个叫
    /// `report.part0001` 的普通文件，它会被当成 `report` 的分片从列表里消失
    /// （恢复时 GET `report` 必然 404），`delete(report)` 还会误删它。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn orphan_part_file_is_listed_as_plain_file() {
        let mock = MockDav::start(false).await;
        let target = WebdavTarget::new(&format!("http://{}/dav", mock.addr), "u", "p");
        {
            let mut objs = mock.objects.lock().unwrap();
            // 用户自己的文件：形似分片但没有 part0001
            objs.insert("fn-backup/report.part0002".to_string(), vec![1u8; 100]);
            // 我们自己写的分片序列：从 part0001 开始
            objs.insert("fn-backup/big.age.part0001".to_string(), vec![2u8; 60]);
            objs.insert("fn-backup/big.age.part0002".to_string(), vec![3u8; 40]);
        }
        let list = target.list("fn-backup").await.expect("列出应成功");
        let names: Vec<&str> = list.iter().map(|f| f.rel_path.as_str()).collect();

        assert!(
            names.contains(&"fn-backup/report.part0002"),
            "孤立分片必须原样保留（否则用户文件被吞掉）：{names:?}"
        );
        assert!(
            names.contains(&"fn-backup/big.age"),
            "从 part0001 开始的真实分片序列应折叠为逻辑文件：{names:?}"
        );
        assert!(
            !names.contains(&"fn-backup/big.age.part0001"),
            "真实分片不应以原始名出现：{names:?}"
        );
        // 折叠后的逻辑大小 = 各分片之和
        let big = list.iter().find(|f| f.rel_path == "fn-backup/big.age").unwrap();
        assert_eq!(big.size, 100, "折叠大小应为分片之和");
    }

    /// **删除成功但残留分片没清掉 ⇒ 必须报错**
    ///
    /// 旧的 match 写的是 `(Ok(()), _) => Ok(())`，那条通配 `_` 把
    /// `cleanup_parts` 的 `Err` 一起吞掉了（编译器报 `unreachable pattern`：
    /// 末尾 `(_, Err(e))` 永不可达）。后果与「覆盖写残留旧分片」一样：
    /// 云端留着 part000N，`list()` 会把它们拼回逻辑文件，
    /// 恢复时得到**被截断或拼错的数据**，而删除操作却回报成功。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn delete_reports_error_when_parts_cleanup_fails() {
        let mock = MockDav::start(false).await;
        let target = WebdavTarget::new(&format!("http://{}/dav", mock.addr), "u", "p");
        {
            let mut objs = mock.objects.lock().unwrap();
            // ⚠️ 逻辑文件**必须存在**：否则 `delete_raw` 先返回 `NotFound`，
            // match 会走 `(Err(e), _)` 分支，测不到真正要防的
            // `(Ok(()), Err(e))`（逻辑文件删成功、分片清理失败）。
            objs.insert("fn-backup/big.age".to_string(), vec![9u8; 1]);
            objs.insert("fn-backup/big.age.part0001".to_string(), vec![1u8; 60]);
            objs.insert("fn-backup/big.age.part0002".to_string(), vec![1u8; 40]);
        }
        // 让服务端拒绝删除分片
        mock.fail_delete_parts
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let r = target.delete(std::path::Path::new("fn-backup/big.age")).await;

        assert!(
            r.is_err(),
            "分片没清掉却回报成功会导致恢复出错误数据，必须上报错误"
        );
        // 分片确实还在（证明上面失败的是清理，而不是别的环节）
        let objs = mock.objects.lock().unwrap();
        assert!(
            objs.contains_key("fn-backup/big.age.part0001"),
            "分片应仍残留（模拟服务端拒绝删除）"
        );
    }
}

#[cfg(test)]
mod proxy_tests {
    use super::*;
    use crate::infra::config::ProxySettings;

    /// `no_proxy` 的三种写法都要能命中（与主流 NO_PROXY 语义一致）
    #[test]
    fn no_proxy_matches_host_exactly_suffix_and_port() {
        // 精确主机
        assert!(host_matches_no_proxy("https://dav.kzwr.com/dav", "dav.kzwr.com"));
        // 大小写不敏感
        assert!(host_matches_no_proxy("https://DAV.KZWR.COM/dav", "dav.kzwr.com"));
        // 逗号分隔 + 前后空格
        assert!(host_matches_no_proxy("http://nas.local/x", "localhost, nas.local"));
        // 后缀通配
        assert!(host_matches_no_proxy("http://a.b.local/x", "*.local"));
        assert!(host_matches_no_proxy("http://b.local/x", "*.local"));
        assert!(host_matches_no_proxy("http://b.local/x", ".local"));
        // 带端口写法只比主机名
        assert!(host_matches_no_proxy("http://localhost:8080/x", "localhost:8080"));
        // 不命中
        assert!(!host_matches_no_proxy("https://dav.kzwr.com/dav", "example.com"));
        assert!(!host_matches_no_proxy("https://dav.kzwr.com/dav", ""));
        // 后缀不能误伤同后缀的其它域（`notlocal` 不该被 `.local` 命中）
        assert!(!host_matches_no_proxy("http://notlocal/x", ".local"));
    }

    /// 代理生效需要三个条件同时满足
    #[test]
    fn resolve_proxy_requires_switch_target_flag_and_no_proxy_miss() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = crate::infra::config::ConfigManager::new(
            dir.path(),
            age::secrecy::SecretString::from("t".to_string()),
        );
        let p = ProxySettings {
            enabled: true,
            url: "http://127.0.0.1:7890".to_string(),
            username_enc: None,
            password_enc: None,
            no_proxy: "localhost,*.local".to_string(),
        };
        // 目标勾选 + 不在 no_proxy ⇒ 走代理
        assert!(p.resolve_for(&mgr, "https://dav.kzwr.com/dav", true).is_some());
        // 目标没勾选 ⇒ 直连
        assert!(p.resolve_for(&mgr, "https://dav.kzwr.com/dav", false).is_none());
        // 命中 no_proxy ⇒ 直连（即便目标勾了）
        assert!(p.resolve_for(&mgr, "http://nas.local/dav", true).is_none());
        assert!(p.resolve_for(&mgr, "http://localhost:8080/dav", true).is_none());
        // 总开关关掉 ⇒ 全部直连
        let off = ProxySettings { enabled: false, ..p.clone() };
        assert!(off.resolve_for(&mgr, "https://dav.kzwr.com/dav", true).is_none());
        // 地址为空 ⇒ 直连（即使 enabled=true）
        let blank = ProxySettings { url: String::new(), ..p.clone() };
        assert!(blank.resolve_for(&mgr, "https://dav.kzwr.com/dav", true).is_none());
    }

    /// 注入给插件的代理 JSON 形状（插件侧按这三个键读）
    #[test]
    fn resolve_proxy_json_shape() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = crate::infra::config::ConfigManager::new(
            dir.path(),
            age::secrecy::SecretString::from("t".to_string()),
        );
        let p = ProxySettings {
            enabled: true,
            url: "socks5h://127.0.0.1:1080".to_string(),
            username_enc: Some(mgr.encrypt_field("pu").unwrap()),
            password_enc: Some(mgr.encrypt_field("pp").unwrap()),
            no_proxy: String::new(),
        };
        let v = p.resolve_for(&mgr, "https://dav.kzwr.com/dav", true).unwrap();
        assert_eq!(v["url"], "socks5h://127.0.0.1:1080");
        assert_eq!(v["username"], "pu");
        assert_eq!(v["password"], "pp");
    }

    /// 带代理构建 client 不应 panic，且代理地址非法要在构建期报错
    #[test]
    fn webdav_target_accepts_proxy_and_rejects_garbage() {
        let ok = reqwest::Proxy::all("http://127.0.0.1:7890").expect("合法代理应能解析");
        let t = WebdavTarget::new("https://dav.kzwr.com/dav", "u", "p").with_proxy(Some(ok));
        assert!(!t.url_for("a.age").is_empty());
        // ⚠️ `Proxy::all` 对非法串**不会报错**（会把整串当主机名），
        // 所以保存时必须走更严格的 `validate_proxy_url`。
        assert!(validate_proxy_url("不是地址").is_err());
        assert!(validate_proxy_url("192.168.1.2:7890").is_err(), "缺 scheme 要拦下");
        assert!(validate_proxy_url("ftp://1.2.3.4:21").is_err(), "不支持的协议要拦下");
        assert!(validate_proxy_url("http://192.168.1.2:7890").is_ok());
        assert!(validate_proxy_url("socks5h://127.0.0.1:1080").is_ok());
    }
}

#[cfg(test)]
mod read_stream_tests {
    use super::*;
    use crate::infra::target::mock_dav::MockDav;
    use std::sync::Arc;

    fn target_for(mock: &MockDav) -> WebdavTarget {
        WebdavTarget::new(&format!("http://{}/dav", mock.addr), "u", "p")
    }

    /// 恢复路径：大文件必须能**分块流式**读出，而不是一次性全塞进内存。
    ///
    /// 现状（待修）：插件的 `read_begin` 会 `Vec::extend_from_slice` 整个文件，
    /// 8GB 备份恢复时峰值 RAM = 8GB —— 这在 NAS 上必 OOM。
    /// 本测试断言「第一块数据能在整个文件读完之前就拿到」，正是流式的判据。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn read_stream_yields_early_chunks_without_buffering_all() {
        let mock = MockDav::start_with(false, std::time::Duration::from_millis(2)).await;
        let target = target_for(&mock);
        let data: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        // 先放进去（走 PUT 落库，小文件直传）
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        target
            .write_stream_sized(
                Path::new("fn-backup/big.age"),
                Box::new(futures::stream::iter(vec![Bytes::from(data.clone())])),
                data.len() as u64,
                noop,
            )
            .await
            .expect("写入应成功");

        let started = std::time::Instant::now();
        let mut st = target.read_stream(Path::new("fn-backup/big.age")).await.expect("读应成功");
        let first = st.next().await.expect("应至少有一块").expect("块应有效");
        let first_at = started.elapsed();
        assert!(!first.is_empty(), "第一块不应为空");
        // 读完剩余，确认总量正确
        let mut total = first.len() as u64;
        while let Some(c) = st.next().await {
            total += c.expect("后续块应有效").len() as u64;
        }
        assert_eq!(total, data.len() as u64, "读回的字节数应等于写入量");
        eprintln!("[read] 首块耗时 {:?}，总 {}", first_at, total);
    }

    /// **正常的多分片读取不能报「分片数量超过上限」**
    ///
    /// 旧实现把「for idx in 1..=MAX_PARTS 正常走完」和「真的超过上限」混为一谈：
    /// 循环体拼到最后一个分片后，下一次 GET 命中 404 就 `break`，
    /// 但 break 出循环后仍**无条件**再发一条 `分片数量超过上限 100000` 的错误。
    /// 结果：任何 >90MiB（会被分成 2 片以上）的文件，恢复时数据已经完整读到，
    /// 末尾却必然收到这条错误 ⇒ 大文件恢复必定失败。
    ///
    /// 本测试直接构造「只有分片、没有逻辑文件」的布局（正是大文件的云端形态），
    /// 断言读取成功且字节数正确。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_part_read_does_not_report_part_limit_error() {
        let mock = MockDav::start(false).await;
        let target = target_for(&mock);
        // 云端只存分片（不含逻辑文件），与真实大文件布局一致
        {
            let mut objs = mock.objects.lock().unwrap();
            objs.insert("fn-backup/big.age.part0001".to_string(), vec![0x11u8; 1000]);
            objs.insert("fn-backup/big.age.part0002".to_string(), vec![0x22u8; 1000]);
            objs.insert("fn-backup/big.age.part0003".to_string(), vec![0x33u8; 500]);
        }

        let mut st = target
            .read_stream(Path::new("fn-backup/big.age"))
            .await
            .expect("多分片读取应能开始");
        let mut total = 0usize;
        while let Some(chunk) = st.next().await {
            let c = chunk.expect("不应出现任何错误：数据已完整，不能报「分片数量超过上限」");
            total += c.len();
        }
        assert_eq!(total, 2500, "三个分片应完整拼回（1000+1000+500）");
    }
}

#[cfg(test)]
mod speed_regression_tests {
    use super::*;
    use crate::infra::target::mock_dav::MockDav;
    use crate::plugin::api::TargetPlugin;
    use std::sync::{Arc, Mutex};

    /// **回归**：备份过程中速度必须真的算得出来（非 0）。
    ///
    /// 线上症状：「备份成功，但速度不显示（恒为 —）」。
    /// 根因链：目标端把上游数据瞬间抽干 ⇒ 进度观测的 `elapsed_ms` 恒为 0
    /// ⇒ 上游 `FileMeter` 的 `MIN_OBSERVE_MS` 判定全为抖动丢弃 ⇒ `SpeedMeter`
    /// 一个样本都没有 ⇒ 前端 `fmtBps(0)` 落到 `'—'`。
    ///
    /// 本测试走**生产同一条装配路径**（插件 build → 沙箱池 → 插件 → WebdavTarget），
    /// 用带延迟的假服务器模拟真实带宽，断言「中途就出现了正的速率」。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn speed_becomes_visible_during_transfer() {
        let mock = MockDav::start_with(false, std::time::Duration::from_millis(1)).await;
        let dir = tempfile::tempdir().unwrap();
        let mgr = crate::infra::config::ConfigManager::new(
            dir.path(),
            age::secrecy::SecretString::from("t".to_string()),
        );
        let t = crate::infra::config::TargetConfig {
            id: "t".into(),
            name: "m".into(),
            kind: "webdav".into(),
            url: Some(format!("http://{}/dav", mock.addr)),
            username_enc: Some(mgr.encrypt_field("u").unwrap()),
            password_enc: Some(mgr.encrypt_field("p").unwrap()),
            enabled: true,
            parallel: Some(1),
            fields: Default::default(),
            use_proxy: false,
        };
        let plugin = crate::plugin::builtin::webdav_abi::WebdavAbiPlugin::new();
        let (storage, _) = plugin.build(&t, &mgr).unwrap();

        // 与 `BackupJob` 相同的接线：SpeedMeter + 每文件 FileMeter
        let meter = Arc::new(Mutex::new(crate::domain::pace::SpeedMeter::new()));
        let file_meter = Arc::new(Mutex::new(crate::domain::pace::SpeedMeter::begin_file()));
        let speeds = Arc::new(Mutex::new(Vec::<u64>::new()));
        let (m2, f2, sp) = (meter.clone(), file_meter.clone(), speeds.clone());
        let cb: ProgressCb = Arc::new(move |written, _t, elapsed_ms| {
            let speed = {
                let mut m = m2.lock().unwrap();
                f2.lock().unwrap().observe(&mut m, written, elapsed_ms);
                m.speed_bps()
            };
            sp.lock().unwrap().push(speed);
        });

        let data = vec![7u8; 24 * 1024 * 1024];
        let total = data.len() as u64;
        let chunks: Vec<Bytes> = data
            .chunks(1024 * 1024)
            .map(Bytes::copy_from_slice)
            .collect();
        storage
            .write_stream_sized(
                std::path::Path::new("fn-backup/p.bin"),
                Box::new(futures::stream::iter(chunks)),
                total,
                cb,
            )
            .await
            .expect("上传应成功");

        let seen = speeds.lock().unwrap().clone();
        let positive: Vec<u64> = seen.iter().copied().filter(|s| *s > 0).collect();
        assert!(
            !positive.is_empty(),
            "传输过程中必须出现过正的速率（否则前端速度恒显示 —）。观测：{seen:?}"
        );
        // 收敛性：最后一个正速率不应比峰值还离谱（防止「瞬时虚高永不回落」）
        let peak = positive.iter().copied().max().unwrap();
        assert!(
            peak > 0,
            "峰值速率应为正数，实际观测：{seen:?}"
        );
    }

    /// 反向对照：若目标端瞬间抽干数据（无延迟、纯内存），观测耗时恒为 0，
    /// 速率必须**仍然能算出来** —— 靠 `FileMeter` 把被跳过的字节补回去。
    #[tokio::test]
    async fn speed_survives_instant_drain() {
        let mut meter = crate::domain::pace::SpeedMeter::new();
        let mut fm = crate::domain::pace::SpeedMeter::begin_file();
        // 前 9 次观测耗时都是 0（会被阈值跳过），第 10 次才有一个有效耗时
        for i in 1..=9u64 {
            fm.observe(&mut meter, i * 1024 * 1024, 0);
        }
        fm.observe(&mut meter, 10 * 1024 * 1024, 100);
        let bps = meter.speed_bps();
        assert!(
            bps > 0,
            "被阈值跳过的字节必须在后续有效观测里补回来（否则速度恒为 0）"
        );
        // 10MiB / 100ms ≈ 100MiB/s 量级
        assert!(
            (50 * 1024 * 1024..200 * 1024 * 1024).contains(&bps),
            "速率数量级应接近 100MiB/s，实际 {bps} B/s"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内置 WebDAV 不在 UI 暴露地址，配置里存的是 `Some("")`；
    /// 只判 `None` 会得到空串并让请求在构造阶段就以 `builder error` 失败。
    #[test]
    fn effective_url_falls_back_on_empty_and_missing() {
        assert_eq!(effective_url(None), DEFAULT_URL);
        assert_eq!(effective_url(Some("")), DEFAULT_URL);
        assert_eq!(effective_url(Some("   ")), DEFAULT_URL);
        assert_eq!(effective_url(Some("https://x.test/dav")), "https://x.test/dav");
    }

    #[test]
    fn test_percent_encode_decode() {
        assert_eq!(percent_encode_path("a/b/c.txt"), "a/b/c.txt");
        assert_eq!(percent_encode_path("目录/文 件.txt"), "%E7%9B%AE%E5%BD%95/%E6%96%87%20%E4%BB%B6.txt");
        assert_eq!(
            percent_decode_path(&percent_encode_path("目录/文 件.txt")),
            "目录/文 件.txt"
        );
        assert_eq!(percent_decode_path("/dav/fn-backup/a%20b.txt"), "/dav/fn-backup/a b.txt");
    }

    #[test]
    fn test_parse_multistatus() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response><D:href>/dav/</D:href><D:propstat><D:prop>
    <D:resourcetype><D:collection/></D:resourcetype>
  </D:prop></D:propstat></D:response>
  <D:response><D:href>/dav/fn-backup/</D:href><D:propstat><D:prop>
    <D:resourcetype><D:collection/></D:resourcetype>
  </D:prop></D:propstat></D:response>
  <D:response><D:href>/dav/fn-backup/a%20b.age</D:href><D:propstat><D:prop>
    <D:getcontentlength>1024</D:getcontentlength>
    <D:resourcetype/>
  </D:prop></D:propstat></D:response>
</D:multistatus>"#;
        let entries = parse_multistatus(xml);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].href, "/dav/");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].href, "/dav/fn-backup/");
        assert!(entries[1].is_dir);
        // href **保留编码态**（`a%20b.age`）：解码只允许在 `href_to_rel` 里做一次，
        // 否则含 `%` 的真实文件名会被二次解码篡改。
        assert_eq!(entries[2].href, "/dav/fn-backup/a%20b.age");
        assert!(!entries[2].is_dir);
        assert_eq!(entries[2].size, 1024);
        // 经 href_to_rel 后才是解码后的可读路径
        let t = WebdavTarget::new("https://dav.kzwr.com/dav", "u", "p");
        assert_eq!(
            t.href_to_rel(&entries[2].href).as_deref(),
            Some("fn-backup/a b.age")
        );
    }

    #[test]
    fn test_href_to_rel() {
        let t = WebdavTarget::new("https://dav.kzwr.com/dav", "u", "p");
        assert_eq!(t.href_to_rel("/dav/fn-backup/").as_deref(), Some("fn-backup/"));
        assert_eq!(t.href_to_rel("/dav/fn-backup/a.age").as_deref(), Some("fn-backup/a.age"));
        assert_eq!(t.href_to_rel("/dav/"), None);
        assert_eq!(t.url_for("fn-backup/a b.age"), "https://dav.kzwr.com/dav/fn-backup/a%20b.age");
    }

    #[test]
    fn test_split_part_name() {
        assert_eq!(
            split_part_name("data.age.part0001"),
            Some(("data.age".to_string(), 1))
        );
        assert_eq!(
            split_part_name("data.age.part0042"),
            Some(("data.age".to_string(), 42))
        );
        assert_eq!(split_part_name("data.age"), None);
        assert_eq!(split_part_name("data.age.part"), None);
        assert_eq!(split_part_name("data.age.partabcd"), None);
        assert_eq!(split_part_name("data.age.part12"), None);
    }

    #[test]
    fn test_part_suffix_and_path() {
        assert_eq!(part_suffix(1), ".part0001");
        assert_eq!(part_suffix(42), ".part0042");
        let t = WebdavTarget::new("https://dav.kzwr.com/dav", "u", "p");
        assert_eq!(
            t.part_path(Path::new("fn-backup/a.age"), 3),
            PathBuf::from("fn-backup/a.age.part0003")
        );
    }
}

