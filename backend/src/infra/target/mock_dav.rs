//! 测试用的最小 WebDAV 服务器（**仅测试**）
//!
//! 真实服务（kzwr）的行为要点：PUT 带 Basic 认证、MKCOL 逐级建目录、
//! `cleanup_parts` 会一路 DELETE 分片直到 404。这里实现这三件事即可覆盖
//! 「密文如何落到目标端」的全部逻辑，尤其是 **Content-Length 必须精确等于
//! 该请求体长度** 这条硬约束（分片上传若把整文件长度写进 CL，服务端会 413）。
//!
//! 记录每个 PUT 的形势：路径、声明的 Content-Length、实际收到的字节数 ——
//! 三者不一致就是 bug。

#![cfg(test)]

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 一次落库的 PUT 记录：(路径, Content-Length, 实收字节数)
pub type PutRecord = (String, Option<u64>, u64);

/// 一个够用的假 WebDAV 服务器：记录每个 PUT 的路径、Content-Length 与实收字节数
pub struct MockDav {
    pub addr: std::net::SocketAddr,
    /// (路径, Content-Length, 实收字节数)
    pub puts: Arc<std::sync::Mutex<Vec<PutRecord>>>,
    /// 是否需要 Basic 认证（true 时缺凭据一律 401）
    pub require_auth: bool,
    /// 读取 body 时每块的延迟（模拟真实带宽，让进度观测有可测的耗时）
    pub chunk_delay: std::time::Duration,
    /// 已落库的对象内容（供 GET/read_stream 回放）
    pub objects: Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    /// 为 `true` 时 GET 只发一半数据就断开（模拟下载中断）
    ///
    /// 用来验证「中途失败不能被当成正常 EOF」——那会静默产生一个截断的文件。
    pub truncate_downloads: Arc<std::sync::atomic::AtomicBool>,
    /// 为 `true` 时 DELETE **分片文件**一律返回 500（模拟服务端拒绝清理）
    ///
    /// 用来验证「删除成功后若残留分片没清掉，必须上报而不是静默成功」——
    /// 否则 `list()` 会把残留分片拼回逻辑文件，恢复出被截断/拼错的数据。
    pub fail_delete_parts: Arc<std::sync::atomic::AtomicBool>,
}

impl MockDav {
    /// 默认实例：不额外延迟（本地回环，瞬间完成）
    pub async fn start(require_auth: bool) -> Self {
        Self::start_with(require_auth, std::time::Duration::ZERO).await
    }

    /// 带 body 读取延迟的实例（模拟真实网络速率）
    pub async fn start_with(require_auth: bool, chunk_delay: std::time::Duration) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let puts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let puts_for_srv = puts.clone();
        let objects = Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::<String, Vec<u8>>::new(),
        ));
        let objects_for_srv = objects.clone();
        let truncate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let truncate_for_srv = truncate.clone();
        let fail_delete_parts = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fail_delete_parts_for_srv = fail_delete_parts.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let puts = puts_for_srv.clone();
                let objects = objects_for_srv.clone();
                let truncate = truncate_for_srv.clone();
                let fail_delete_parts = fail_delete_parts_for_srv.clone();
                tokio::spawn(async move {
                    let mut buf: Vec<u8> = Vec::new();
                    let mut tmp = vec![0u8; 64 * 1024];
                    // 一条连接上可连续处理多个请求（HTTP/1.1 keep-alive）。
                    // 真实 WebDAV 服务就是这样复用的；若只处理一个请求就关连接，
                    // 上层的连接池会拿到已关闭的 socket，表现为「网络错误」。
                    'conn: loop {
                        // 读到头部结束（可能这次读里已经带上了 body 的前半段）
                        let (head, body_start) = loop {
                            if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                                break (
                                    String::from_utf8_lossy(&buf[..pos]).to_string(),
                                    pos + 4,
                                );
                            }
                            let n = match sock.read(&mut tmp).await {
                                Ok(0) | Err(_) => break 'conn,
                                Ok(n) => n,
                            };
                            buf.extend_from_slice(&tmp[..n]);
                            if buf.len() > 1 << 20 {
                                break 'conn;
                            }
                        };
                        let mut lines = head.lines();
                        let request_line = lines.next().unwrap_or_default().to_string();
                        let mut content_length: Option<u64> = None;
                        for l in lines {
                            if let Some((k, v)) = l.split_once(':') {
                                if k.trim().eq_ignore_ascii_case("content-length") {
                                    content_length = v.trim().parse().ok();
                                }
                            }
                        }
                        let method = request_line.split(' ').next().unwrap_or("").to_string();
                        let path = request_line.split(' ').nth(1).unwrap_or("").to_string();
                        // **对象存储的统一键**：剥掉 `/dav` 基址的相对路径。
                        //
                        // 之前这里不一致：PUT/GET/DELETE 用绝对路径当键，PROPFIND 与
                        // 测试里直接插入的对象却用相对路径 ⇒ 同一个文件在两套键空间里，
                        // DELETE 永远「找不到」而返回 404（等于**从不真正删除**），
                        // 把「删除会不会残留旧分片」这类行为完全掩盖了。
                        let key = path
                            .strip_prefix("/dav/")
                            .unwrap_or_else(|| path.trim_start_matches('/'))
                            .to_string();

                        // 取走本请求的 body（按 Content-Length 精确消费，多读的留给下个请求）
                        let mut body = buf[body_start..].to_vec();
                        if let Some(cl) = content_length {
                            while (body.len() as u64) < cl {
                                match sock.read(&mut tmp).await {
                                    Ok(0) | Err(_) => break 'conn,
                                    Ok(n) => {
                                        body.extend_from_slice(&tmp[..n]);
                                        if !chunk_delay.is_zero() {
                                            tokio::time::sleep(chunk_delay).await;
                                        }
                                    }
                                }
                            }
                            body.truncate(cl as usize);
                        }
                        // 剩余的字节属于下一个请求
                        let consumed = body_start + body.len();
                        buf = buf[consumed.min(buf.len())..].to_vec();

                        let body_len = body.len() as u64;
                        let mut owned: Vec<u8> = Vec::new();
                        let resp: &[u8] = match method.as_str() {
                            "PUT" => {
                                puts.lock().unwrap().push((path.clone(), content_length, body_len));
                                // 落库：把分片按逻辑路径拼接，供后续 GET 回放
                                // （`.partNNNN` 追加到同名逻辑对象上）
                                let logical = strip_part_suffix(&key);
                                objects
                                    .lock()
                                    .unwrap()
                                    .entry(logical)
                                    .or_default()
                                    .extend_from_slice(&body);
                                b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n"
                            }
                            "GET" => {
                                match objects.lock().unwrap().get(&key) {
                                    Some(v) => {
                                        owned = v.clone();
                                        b""
                                    }
                                    None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
                                }
                            }
                            // 真实语义：存在则删掉并返回 204；不存在才 404。
                            // 早期这里一律返回 404，等于**掩盖了「旧分片未清理」这类 bug**
                            // （cleanup_parts / cleanup_parts_from 永远「删成功」但其实没删）。
                            "DELETE" => {
                                // 键形态与 PUT/GET 一致：**绝对路径**（含 `/dav` 基址）。
                                //
                                // 模拟「服务端拒绝删除分片」：逻辑文件照删，分片删不掉。
                                // 对应真实场景：中间层对 DELETE 返回 5xx / 只读挂载 / 权限不足。
                                let is_part = key.contains(".part");
                                if is_part
                                    && fail_delete_parts.load(std::sync::atomic::Ordering::SeqCst)
                                {
                                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n"
                                } else {
                                    let removed = objects.lock().unwrap().remove(&key).is_some();
                                    if removed {
                                        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n"
                                    } else {
                                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"
                                    }
                                }
                            }
                            "MKCOL" => b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n",
                            // 最小可用的 PROPFIND：把 objects 里落在该目录下的条目列出来。
                            // 只实现 Depth=1 的「直系子项」，够 `WebdavTarget::list` 用。
                            "PROPFIND" => {
                                // 注意：PROPFIND 的请求行是绝对路径（含 /dav 基址），
                                // 而 objects 的键是**相对路径**（不含基址），需剥掉。
                                let rel = key.clone();
                                let prefix = if rel.is_empty() {
                                    String::new()
                                } else if rel.ends_with('/') {
                                    rel.to_string()
                                } else {
                                    format!("{rel}/")
                                };
                                let mut xml = String::from(
                                    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">",
                                );
                                let objs = objects.lock().unwrap();
                                for (k, v) in objs.iter() {
                                    let Some(rest) = k.strip_prefix(&prefix) else { continue };
                                    if rest.is_empty() || rest.contains('/') {
                                        continue; // 只要直系子项
                                    }
                                    // href 必须带基址 `/dav/`（真实服务如此）：
                                    // `WebdavTarget::href_to_rel` 会按 base_path 剥离，
                                    // 不带前缀的 href 会被判为「不在目标下」而丢弃。
                                    xml.push_str(&format!(
                                        "<D:response><D:href>/dav/{}</D:href><D:propstat><D:prop>\
                                         <D:getcontentlength>{}</D:getcontentlength>\
                                         <D:resourcetype/></D:prop></D:propstat></D:response>",
                                        k,
                                        v.len()
                                    ));
                                }
                                xml.push_str("</D:multistatus>");
                                owned = xml.into_bytes();
                                b""
                            }
                            _ => b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
                        };
                        if owned.is_empty() {
                            if sock.write_all(resp).await.is_err() {
                                break 'conn;
                            }
                        } else {
                            // GET 命中 / PROPFIND 有结果：先发头（带精确 Content-Length），
                            // 再发体。状态码按方法区分 —— PROPFIND 必须是 **207 Multi-Status**，
                            // 返回 200 会让客户端判为协议错误。
                            let status = if method == "PROPFIND" {
                                "207 Multi-Status"
                            } else {
                                "200 OK"
                            };
                            let head = format!(
                                "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n",
                                owned.len()
                            );
                            if sock.write_all(head.as_bytes()).await.is_err() {
                                break 'conn;
                            }
                            // 按真实带宽分块发送，让「边下边处理」与「全下完再返回」
                            // 在耗时上真正区分开（否则客户端瞬间收完，测不出流式）
                            let truncating =
                                truncate.load(std::sync::atomic::Ordering::Relaxed);
                            let full_len = owned.len();
                            // 截断模式：声明完整长度，但只发一半就断开
                            let limit = if truncating { full_len / 2 } else { full_len };
                            let mut sent = 0usize;
                            for piece in owned.chunks(64 * 1024) {
                                if sent >= limit {
                                    break;
                                }
                                let take = (limit - sent).min(piece.len());
                                if sock.write_all(&piece[..take]).await.is_err() {
                                    break 'conn;
                                }
                                sent += take;
                                if !chunk_delay.is_zero() {
                                    tokio::time::sleep(chunk_delay).await;
                                }
                            }
                            if truncating {
                                // 直接断开：客户端应报「读取中断」，绝不能被当成 EOF
                                break 'conn;
                            }
                        }
                        let _ = sock.flush().await;
                    }
                });
            }
        });
        Self {
            addr,
            puts,
            require_auth,
            chunk_delay,
            objects,
            truncate_downloads: truncate,
            fail_delete_parts,
        }
    }

pub fn total_bytes(&self) -> u64 {
        self.puts.lock().unwrap().iter().map(|(_, _, n)| *n).sum()
    }
}

/// 去掉 `.partNNNN` 后缀，得到逻辑路径（分片在服务端表现为同一逻辑对象）
fn strip_part_suffix(path: &str) -> String {
    match path.rsplit_once(".part") {
        Some((base, tail))
            if tail.len() == 4 && tail.chars().all(|c| c.is_ascii_digit()) =>
        {
            base.to_string()
        }
        _ => path.to_string(),
    }
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
