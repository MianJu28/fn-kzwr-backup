//! 酷族网软分片上传（对照前端 chunk-EURGQOYU.js）
//!
//! 流程：
//!   1. POST /api/v2/upload/index 创建上传索引
//!   2. 每个分片：SHA1 -> POST /api/v1/upload/presigned-url 获取预签名 PUT URL
//!      -> PUT 分片数据 -> 取响应头 ETag -> POST /api/v2/upload/chunk 上报
//!   3. POST /api/v2/upload/complete 完成上传

use std::io::{Read, Seek};
use std::path::Path;

use futures::StreamExt;
use serde_json::{json, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};

use super::client::KzwrClient;
use super::constants::{HASH_SAMPLE_SIZE, SHARD_SIZE};
use super::{KzwrError, KzwrResult};

/// 文件哈希与分片信息（对照 `_hash_first_last_and_pieces`）
pub struct FileHashInfo {
    pub file_sha1: String,
    pub start_sha256: String,
    pub end_sha256: String,
    pub pieces_sha256: Vec<String>,
    pub shard_count: usize,
    pub size: u64,
}

/// 计算文件上传所需哈希（与前端 createFileIndex 一致）
///
/// - file_sha1: 整个文件 SHA1
/// - start_sha256: 前 4MB（或整个文件）SHA256
/// - end_sha256: 后 4MB SHA256
/// - pieces_sha256: 每分片前 64KB 的 SHA256 列表
pub fn hash_first_last_and_pieces(path: &Path) -> KzwrResult<FileHashInfo> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| KzwrError::Io(e))?;
    let size = file.metadata().map_err(KzwrError::Io)?.len();
    let shard_count = if size == 0 {
        0
    } else {
        (size as usize).div_ceil(SHARD_SIZE)
    };

    let mut file_sha1 = Sha1::new();
    let mut start_sha256 = String::new();
    let mut pieces_sha256 = Vec::new();
    let mut buf = vec![0u8; SHARD_SIZE];
    let mut offset: u64 = 0;

    loop {
        let n = file.read(&mut buf).map_err(KzwrError::Io)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        file_sha1.update(chunk);

        // 分片哈希只取前 64KB
        let sample_len = chunk.len().min(HASH_SAMPLE_SIZE);
        let mut h = Sha256::new();
        h.update(&chunk[..sample_len]);
        pieces_sha256.push(hex(&h.finalize()));

        // 首分片哈希 = 文件前 min(size, shard_size) 的 SHA256
        if offset == 0 {
            let mut hs = Sha256::new();
            hs.update(chunk);
            start_sha256 = hex(&hs.finalize());
        }
        offset += n as u64;
    }

    // 尾分片哈希 = 文件最后 min(size, 4MB) 的 SHA256
    let end_size = (size as usize).min(SHARD_SIZE) as u64;
    let end_sha256 = if end_size > 0 {
        let mut end_bytes = vec![0u8; end_size as usize];
        file.seek(std::io::SeekFrom::End(-(end_size as i64)))
            .map_err(KzwrError::Io)?;
        file.read_exact(&mut end_bytes).map_err(KzwrError::Io)?;
        let mut he = Sha256::new();
        he.update(&end_bytes);
        hex(&he.finalize())
    } else {
        String::new()
    };

    Ok(FileHashInfo {
        file_sha1: hex(&file_sha1.finalize()),
        start_sha256,
        end_sha256,
        pieces_sha256,
        shard_count,
        size,
    })
}

/// SHA1 十六进制
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// 分片上传文件到 kzwr 指定文件夹
///
/// - file_path: 本地文件路径
/// - folder: 目标文件夹（"/" 或 "/Software"）
/// - scene: 场景（默认 "Auto"）
/// - region: 区域（0 = Auto）
/// - concurrency: 并发分片数（前端默认 3）
///
/// 返回 /api/v2/upload/complete 的响应
pub async fn upload_file(
    client: &KzwrClient,
    file_path: &Path,
    folder: &str,
    scene: &str,
    region: u64,
    concurrency: usize,
    target_name: Option<&str>,
) -> KzwrResult<Value> {
    if !file_path.is_file() {
        return Err(KzwrError::Other(format!(
            "文件不存在: {}",
            file_path.display()
        )));
    }
    // 目标文件名：优先用显式 target_name（如逻辑路径文件名），否则取文件路径名
    let file_name = match target_name {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => file_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| KzwrError::Other("无法获取文件名".to_string()))?
            .to_string(),
    };

    // 1) 计算哈希并创建上传索引
    let h = hash_first_last_and_pieces(file_path)?;
    let index_body = json!({
        "TotalChunks": h.shard_count,
        "UploadFolder": folder,
        "First4MBHash": h.start_sha256,
        "Last4MBHash": h.end_sha256,
        "FileSize": h.size,
        "FileSha1": h.file_sha1,
        "FileName": file_name,
        "Region": region,
        "Scene": scene,
    });
    let index_resp = client.post_json("/api/v2/upload/index", index_body).await?;

    let file_identifier = index_resp
        .get("fileIdentifier")
        .or_else(|| index_resp.get("data").and_then(|d| d.get("fileIdentifier")))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            KzwrError::Api(format!(
                "/api/v2/upload/index 未返回 fileIdentifier -> {}",
                index_resp
            ))
        })?
        .to_string();

    let current_chunk: usize = index_resp
        .get("currentChunkIndex")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    // 2) 逐片并发上传
    let total_chunks = h.shard_count;
    let total_bytes = h.size;
    let mut upload_results = Vec::new();

    let tasks: Vec<usize> = (current_chunk..total_chunks).collect();
    let mut futures = futures::stream::iter(tasks.into_iter().map(|ci| {
        upload_one_chunk(
            client,
            file_path,
            folder,
            &file_name,
            &file_identifier,
            ci,
            total_bytes,
        )
    }))
    .buffer_unordered(concurrency);

    while let Some(res) = futures.next().await {
        upload_results.push(res);
    }

    // 收集错误
    let errors: Vec<(usize, KzwrError)> = upload_results
        .into_iter()
        .enumerate()
        .filter_map(|(i, r)| r.err().map(|e| (current_chunk + i, e)))
        .collect();
    if !errors.is_empty() {
        return Err(KzwrError::Other(format!(
            "{} 个分片上传失败, 首个错误: {:?}",
            errors.len(),
            errors[0].1
        )));
    }

    // 3) 完成上传
    client
        .post_json(
            "/api/v2/upload/complete",
            json!({
                "FileIdentifier": file_identifier,
                "Scene": scene,
            }),
        )
        .await
}

/// 上传单个分片
async fn upload_one_chunk(
    client: &KzwrClient,
    file_path: &Path,
    folder: &str,
    file_name: &str,
    file_identifier: &str,
    chunk_idx: usize,
    total_bytes: u64,
) -> KzwrResult<()> {
    // 读取分片数据
    let start = chunk_idx as u64 * SHARD_SIZE as u64;
    let end = total_bytes.min(start + SHARD_SIZE as u64);
    let data = read_range(file_path, start, end)?;

    // 分片 SHA1
    let chunk_sha1 = sha1_hex(&data);

    // 获取预签名 PUT URL
    let pre = client
        .post_json(
            "/api/v1/upload/presigned-url",
            json!({
                "fileName": file_name,
                "folder": folder,
                "chunkSha1": chunk_sha1,
                "chunkSize": data.len(),
                "fileIdentifier": file_identifier,
                "currentChunk": chunk_idx,
            }),
        )
        .await?;

    // presigned-url 直接返回预签名 PUT URL 字符串（实测）
    let put_url = pre
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| pre.get("url").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .or_else(|| pre.get("data").and_then(|d| d.get("url")).and_then(|v| v.as_str()).map(|s| s.to_string()))
        .ok_or_else(|| KzwrError::Api(format!("presigned-url 未返回有效 url -> {}", pre)))?;
    if !put_url.starts_with("http") {
        return Err(KzwrError::Api(format!(
            "presigned-url 未返回有效 url -> {}",
            put_url
        )));
    }

    // PUT 分片数据到预签名 URL，取 ETag
    let put_resp = client
        .raw_put(&put_url, &data)
        .await
        .map_err(|e| KzwrError::Api(format!("分片 {} PUT 失败: {}", chunk_idx, e)))?;
    let etag = put_resp
        .get_etag()
        .ok_or_else(|| KzwrError::Api(format!("分片 {} PUT 响应无 ETag", chunk_idx)))?;

    // 上报分片完成
    client
        .post_json(
            "/api/v2/upload/chunk",
            json!({
                "ETag": etag,
                "FileIdentifier": file_identifier,
                "CurrentChunkIndex": chunk_idx,
            }),
        )
        .await?;
    Ok(())
}

/// 读取文件指定范围 [start, end)
fn read_range(path: &Path, start: u64, end: u64) -> KzwrResult<Vec<u8>> {
    use std::io::Seek;
    let mut f = std::fs::File::open(path).map_err(KzwrError::Io)?;
    f.seek(std::io::SeekFrom::Start(start)).map_err(KzwrError::Io)?;
    let len = (end - start) as usize;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf).map_err(KzwrError::Io)?;
    Ok(buf)
}

/// SHA1 十六进制
fn sha1_hex(data: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(data);
    h.finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}
