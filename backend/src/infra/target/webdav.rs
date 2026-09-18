//! WebDAV Target 适配器（ADR-009：kzwr 官方 WebDAV）
//!
//! 实现统一的 `TargetStorage` trait，核心同步/加密逻辑零改动。
//! 已实测（2026-09-18，WSL curl）：
//! - HTTP Basic 认证：PROPFIND(207) / MKCOL(201) / PUT(201) / DELETE(204)
//! - `GET` 下载返回 302 → S3 风格 presigned URL（约 300s 有效），
//!   跟随重定向即可取回内容，无需二次认证（reqwest 默认跟随，跨域自动去凭据）
//!
//! 凭据永不硬编码，由调用方从加密配置或环境变量注入。

use std::path::Path;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use reqwest::{Client, Method, StatusCode};

use crate::infra::storage_trait::{
    FileDescriptor, StorageError, StorageResult, TargetStorage,
};

/// WebDAV 目标适配器
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

    /// 逐级 MKCOL 确保父目录存在（已存在视为成功）
    async fn ensure_parents(&self, path: &Path) -> StorageResult<()> {
        let mut cur = String::new();
        for comp in path.parent().into_iter().flat_map(|p| p.components()) {
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
        mut stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
    ) -> StorageResult<()> {
        // 与 KzwrTarget 一致：先落临时文件（密文），再整体 PUT。
        // 避免服务器不接受 chunked 编码，且 PUT 带 Content-Length 更稳。
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().map_err(StorageError::Io)?;
        while let Some(chunk) = stream.next().await {
            tmp.write_all(&chunk).map_err(StorageError::Io)?;
        }
        tmp.flush().map_err(StorageError::Io)?;

        self.ensure_parents(path).await?;

        let url = self.url_for(&path.to_string_lossy());
        let file = tokio::fs::File::open(tmp.path())
            .await
            .map_err(StorageError::Io)?;
        let resp = self
            .request(Method::PUT, &url)
            .body(file)
            .send()
            .await
            .map_err(|e| StorageError::Protocol(format!("PUT {} 失败: {}", url, e)))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(StorageError::Protocol(format!(
                "PUT {} 返回 {}: {}",
                url, status, body
            )));
        }
        Ok(())
    }

    async fn read_stream(
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

    async fn delete(&self, path: &Path) -> StorageResult<()> {
        let url = self.url_for(&path.to_string_lossy());
        let resp = self
            .request(Method::DELETE, &url)
            .send()
            .await
            .map_err(|e| StorageError::Protocol(format!("DELETE {} 失败: {}", url, e)))?;
        match resp.status() {
            s if s.is_success() => Ok(()),
            // 已不存在视为成功（镜像同步/保留策略语义）
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

    async fn list(&self, prefix: &str) -> StorageResult<Vec<FileDescriptor>> {
        // Depth 1：列出 prefix 目录的直系子项
        let dir = prefix.trim_matches('/');
        let entries = self.propfind(dir, "1").await?;
        let self_rel = format!("{}/", dir);
        let mut out = Vec::new();
        for e in entries {
            let Some(rel) = self.href_to_rel(&e.href) else {
                continue;
            };
            let is_dir = e.is_dir || rel.ends_with('/');
            let rel = rel.trim_end_matches('/').to_string();
            if rel == self_rel.trim_end_matches('/') || rel.is_empty() {
                continue; // 跳过目录自身
            }
            out.push(FileDescriptor {
                rel_path: rel,
                size: if is_dir { 0 } else { e.size },
                modified: None,
                is_dir,
                digest: None,
            });
        }
        Ok(out)
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
}
