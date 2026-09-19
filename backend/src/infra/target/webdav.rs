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

/// 单分片大小：8 MiB（默认）
///
/// 网站限制单次上传 100MB，8MiB 留有充足余量；小分片在慢链路下
/// 进度损失小、单请求耗时短，配合 HTTP/1.1 + 超时 + 重试最稳健。
/// 默认分片大小：100MB 网站上传限制（Cloudflare 返回 413 Payload Too Large）的 90%，即 90 MiB。
/// 单分片必须小于该限制才能绕过；可用环境变量 FNOS_DAV_PART_SIZE 覆盖（字节，供测试调小验证分片逻辑）。
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

/// PUT 最大尝试次数
const PUT_MAX_ATTEMPTS: usize = 4;

/// 进度上报的块大小（256 KiB）：兼顾粒度与事件频率
const PROGRESS_CHUNK: usize = 256 * 1024;

/// 带精确长度的流式请求体：边发送边回调进度
///
/// 实现 `http_body::Body` 并给出精确 `size_hint`，使 reqwest 仍以
/// `Content-Length` 发送（而非 chunked），同时按块上报已写字节。
///
/// 注意：`len` 必须是**本请求体自身的长度**（多分片时即分片大小），
/// 否则 Content-Length 会被设成整个文件大小而触发 413。
struct ProgressBody {
    chunks: std::vec::IntoIter<Bytes>,
    /// 本请求体总长度（供 size_hint / Content-Length 使用）
    len: u64,
    /// 本次请求已发送的字节数
    sent: u64,
    /// 本请求之前的累计字节（多分片时用于拼接整体进度）
    offset: u64,
    /// 进度总量（整文件大小，仅用于上报）
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
        match this.chunks.next() {
            Some(b) => {
                this.sent += b.len() as u64;
                (this.on_progress)(
                    this.offset + this.sent,
                    this.total,
                    this.started.elapsed().as_millis() as u64,
                );
                Poll::Ready(Some(Ok(Frame::data(b))))
            }
            None => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.chunks.len() == 0
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.len)
    }
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
}

/// PROPFIND multistatus 中的单条目（href 已解码）
struct DavEntry {
    href: String,
    is_dir: bool,
    size: u64,
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

impl WebdavTarget {
    /// 创建适配器。`base_url` 如 `https://dav.kzwr.com/dav`。
    pub fn new(base_url: &str, username: impl Into<String>, password: impl Into<String>) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        let base_path = reqwest::Url::parse(&base_url)
            .ok()
            .map(|u| u.path().to_string())
            .filter(|s| !s.is_empty() && s != "/")
            .unwrap_or_default();
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            // 强制 HTTP/1.1：服务端/中间层对长时 HTTP/2 上传流不稳定
            // （实测 ~21s 即 HTTP/2 PROTOCOL_ERROR 掐断）
            .http1_only()
            .build()
            .expect("reqwest client 构建失败");
        Self {
            client,
            base_url,
            base_path,
            username: username.into(),
            password: password.into(),
        }
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
                .request(Method::from_bytes(b"MKCOL").unwrap(), &url)
                .send()
                .await
                .map_err(|e| StorageError::Protocol(format!("MKCOL {} 失败: {}", url, e)))?;
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
            .request(Method::from_bytes(b"PROPFIND").unwrap(), &url)
            .header("Depth", depth)
            .body("")
            .send()
            .await
            .map_err(|e| StorageError::Protocol(format!("PROPFIND {} 失败: {}", url, e)))?;
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

    /// PUT 数据（带总超时与重试：网络错误与 5xx/408/429 均可重试）
    ///
    /// `progress` 为 `(回调, 偏移, 总字节)`；非空时以带精确长度的流式 body 发送，
    /// 按块回调进度（仍保留 Content-Length，不走 chunked）。
    async fn put_bytes_retry(
        &self,
        url: &str,
        data: Vec<u8>,
        progress: Option<(ProgressCb, u64, u64)>,
    ) -> StorageResult<()> {
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            // 本次请求的起始时刻（body 内进度与「响应后补报」都用它计时）
            let req_started = std::time::Instant::now();
            let body = match &progress {
                Some((cb, offset, total)) => {
                    let chunks: Vec<Bytes> = data
                        .chunks(PROGRESS_CHUNK)
                        .map(Bytes::copy_from_slice)
                        .collect();
                    reqwest::Body::wrap(ProgressBody {
                        chunks: chunks.into_iter(),
                        len: data.len() as u64,
                        sent: 0,
                        offset: *offset,
                        total: *total,
                        started: req_started,
                        on_progress: cb.clone(),
                    })
                }
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
                            cb(
                                offset + data.len() as u64,
                                *total,
                                req_started.elapsed().as_millis() as u64,
                            );
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

    /// 写入核心实现：`progress` 非空时按块上报上传进度（保留 Content-Length）
    async fn write_inner(
        &self,
        path: &Path,
        mut stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
        progress: Option<ProgressCb>,
    ) -> StorageResult<()> {
        // 暂存密文到临时文件（与分段上传解耦，且带 Content-Length 更稳）
        use std::io::{Read, Write};
        let mut tmp = tempfile::NamedTempFile::new().map_err(StorageError::Io)?;
        while let Some(chunk) = stream.next().await {
            tmp.write_all(&chunk).map_err(StorageError::Io)?;
        }
        tmp.flush().map_err(StorageError::Io)?;
        let size = tmp.as_file().metadata().map_err(StorageError::Io)?.len();

        self.ensure_parents(path).await?;

        if size <= part_size() {
            // 小文件：整体 PUT 到逻辑路径；顺带清理可能残留的旧分片
            self.cleanup_parts(path).await?;
            let url = self.url_for(&path.to_string_lossy());
            let mut data = Vec::with_capacity(size as usize);
            std::fs::File::open(tmp.path())
                .map_err(StorageError::Io)?
                .read_to_end(&mut data)
                .map_err(StorageError::Io)?;
            let prog = progress.map(|cb| (cb, 0u64, size));
            self.put_bytes_retry(&url, data, prog).await
        } else {
            // 大文件：拆分为 .part0001… 依次上传；先删旧逻辑文件（覆盖小文件场景）
            let _ = self.delete_raw(path).await;
            let mut file = std::fs::File::open(tmp.path()).map_err(StorageError::Io)?;
            let mut idx = 1usize;
            let mut written: u64 = 0;
            loop {
                let mut buf = Vec::with_capacity(part_size() as usize);
                let mut take = part_size() as usize;
                while take > 0 {
                    let chunk = {
                        let mut slice = vec![0u8; take.min(1024 * 1024)];
                        let n = file.read(&mut slice).map_err(StorageError::Io)?;
                        slice.truncate(n);
                        slice
                    };
                    if chunk.is_empty() {
                        break;
                    }
                    take -= chunk.len();
                    buf.extend_from_slice(&chunk);
                }
                if buf.is_empty() {
                    break;
                }
                let part_bytes = buf.len() as u64;
                let part = self.part_path(path, idx);
                let url = self.url_for(&part.to_string_lossy());
                // 每个分片的进度偏移 = 之前已上传字节，总长 = 文件总大小
                let prog = progress.as_ref().map(|cb| (cb.clone(), written, size));
                self.put_bytes_retry(&url, buf, prog).await?;
                written += part_bytes;
                idx += 1;
            }
            Ok(())
        }
    }

    /// 删除单个原始路径（404 视为成功）
    async fn delete_raw(&self, path: &Path) -> StorageResult<()> {
        let url = self.url_for(&path.to_string_lossy());
        let resp = self
            .request(Method::DELETE, &url)
            .send()
            .await
            .map_err(|e| StorageError::Protocol(format!("DELETE {} 失败: {}", url, e)))?;
        match resp.status() {
            s if s.is_success() => Ok(()),
            StatusCode::NOT_FOUND => Ok(()),
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

    /// 从 .part0001 起逐个删除分片，直到 404
    async fn cleanup_parts(&self, path: &Path) -> StorageResult<()> {
        for idx in 1.. {
            let part = self.part_path(path, idx);
            let url = self.url_for(&part.to_string_lossy());
            let resp = match self.request(Method::DELETE, &url).send().await {
                Ok(r) => r,
                Err(e) => {
                    return Err(StorageError::Protocol(format!("DELETE {} 失败: {}", url, e)))
                }
            };
            match resp.status() {
                s if s.is_success() => continue,
                StatusCode::NOT_FOUND => return Ok(()),
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
        Ok(())
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
        let url = self.url_for(&path.to_string_lossy());
        let resp = self
            .request(Method::GET, &url)
            .send()
            .await
            .map_err(|e| StorageError::Protocol(format!("GET {} 失败: {}", url, e)))?;
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
        let stream = resp
            .bytes_stream()
            .map(|r| r.map_err(|e| StorageError::Protocol(format!("下载中断: {}", e))));
        Ok(Box::new(stream))
    }

    /// 把服务端 href（如 /dav/fn-backup/a.txt）转为相对条目路径
    fn href_to_rel(&self, href: &str) -> Option<String> {
        let mut p = percent_decode_path(href);
        // 去掉查询串（极端情况）
        if let Some(i) = p.find('?') {
            p.truncate(i);
        }
        let is_dir = p.ends_with('/');
        let p = p.trim_end_matches('/');
        let base = self.base_path.as_str();
        let rel = if !base.is_empty() {
            p.strip_prefix(base)?
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
        self.write_inner(path, stream, None).await
    }

    async fn write_stream_progress(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
        progress: ProgressCb,
    ) -> StorageResult<()> {
        self.write_inner(path, stream, Some(progress)).await
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
                    for idx in 1.. {
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
                                break;
                            }
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                return;
                            }
                        }
                    }
                });
                Ok(Box::new(tokio_stream::wrappers::ReceiverStream::new(rx)))
            }
            Err(e) => Err(e),
        }
    }

    async fn delete(&self, path: &Path) -> StorageResult<()> {
        // 删除逻辑文件 + 全部分片（各自 404 均视为成功）
        self.delete_raw(path).await?;
        self.cleanup_parts(path).await
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<FileDescriptor>> {
        // Depth 1：列出 prefix 目录的直系子项
        let dir = prefix.trim_matches('/');
        let entries = self.propfind(dir, "1").await?;
        let self_rel = format!("{}/", dir);
        let mut out = Vec::new();
        // 分片合并表：逻辑名 → 累计大小
        let mut part_map: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
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
                    modified: None,
                    is_dir: true,
                    digest: None,
                });
                continue;
            }
            // 分片文件合并进逻辑条目
            if let Some((logical, _)) = split_part_name(&rel) {
                *part_map.entry(logical).or_insert(0) += e.size;
                continue;
            }
            out.push(FileDescriptor {
                rel_path: rel,
                size: e.size,
                modified: None,
                is_dir: false,
                digest: None,
            });
        }
        // 分片合并为逻辑文件（若同名逻辑文件已存在则以其为准，忽略残留分片）
        for (logical, size) in part_map {
            if out.iter().any(|f| f.rel_path == logical) {
                continue;
            }
            out.push(FileDescriptor {
                rel_path: logical,
                size,
                modified: None,
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

/// 解析 PROPFIND multistatus XML（容错解析，不依赖 XML 库）
///
/// 提取每个 `<D:response>` 块中的 href / collection / getcontentlength。
/// 对命名空间前缀大小写不敏感（D:/d:/DAV:/无前缀）。
fn parse_multistatus(xml: &str) -> Vec<DavEntry> {
    let mut entries = Vec::new();
    let mut cur_href: Option<String> = None;
    let mut cur_dir = false;
    let mut cur_size: u64 = 0;

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
                    });
                }
                cur_dir = false;
                cur_size = 0;
            }
            "href" if !closing && !self_closing => {
                cur_href = Some(percent_decode_path(rest.trim()));
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
mod tests {
    use super::*;

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
        assert_eq!(entries[2].href, "/dav/fn-backup/a b.age");
        assert!(!entries[2].is_dir);
        assert_eq!(entries[2].size, 1024);
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
