//! WebDAV Target（ADR-009）端到端测试
//!
//! 用真实 `WebdavTarget` 验证官方 WebDAV 全链路：
//!   1. ping（PROPFIND Depth 0）
//!   2. trait 直连 roundtrip（多级目录写入/读取/列表/删除）
//!   3. BackupJob 全量 → 增量 → 修改增量（age 加密 → WebDAV PUT）
//!   4. 下载解密校验（GET 302 → presigned 跟随）
//!   5. 清理测试目录
//!
//! 凭据通过环境变量注入（绝不写入代码/配置文件）：
//!   TRIM_DAV_URL（如 https://dav.kzwr.com/dav）
//!   TRIM_DAV_USER
//!   TRIM_DAV_PASS

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt;
use tempfile::TempDir;

use fnos_backup::domain::backup::BackupJob;
use fnos_backup::domain::crypto::{AgeKeys, CryptoSession};
use fnos_backup::infra::persistence::snapshot::SnapshotStore;
use fnos_backup::infra::source::local::LocalFsSource;
use fnos_backup::infra::storage_trait::TargetStorage;
use fnos_backup::infra::target::webdav::WebdavTarget;

/// 隔离的测试目录前缀（不污染 fn-backup）
const TARGET_FOLDER: &str = "fnos-dav-test";

#[tokio::main]
async fn main() -> Result<()> {
    let url = std::env::var("TRIM_DAV_URL").expect("请设置 TRIM_DAV_URL（如 https://dav.kzwr.com/dav）");
    let user = std::env::var("TRIM_DAV_USER").expect("请设置 TRIM_DAV_USER");
    let pass = std::env::var("TRIM_DAV_PASS").expect("请设置 TRIM_DAV_PASS");

    let target: Arc<dyn TargetStorage> = Arc::new(WebdavTarget::new(&url, user, pass));

    // 1) ping
    println!("=== 1. ping（PROPFIND Depth 0）===");
    target
        .ping()
        .await
        .expect("ping 失败：端点不可达或认证失败");
    println!("[+] ping 通过");

    // 2) trait 直连 roundtrip：多级目录 + 特殊字符文件名
    println!("=== 2. trait roundtrip（多级目录/特殊字符）===");
    let nested = Path::new(TARGET_FOLDER).join("lvl1/lvl2/文 件+名.txt");
    let payload = b"webdav roundtrip payload\n".to_vec();
    // write_stream 期望 Item = Bytes（不带 Result）
    let stream: Box<dyn futures::Stream<Item = bytes::Bytes> + Send + Unpin> =
        Box::new(futures::stream::iter(vec![bytes::Bytes::from(payload.clone())]));
    target.write_stream(&nested, stream).await?;
    println!("[+] 多级写入 OK: {}", nested.display());

    let mut got = target.read_stream(&nested).await?;
    let mut buf = Vec::new();
    while let Some(chunk) = got.next().await {
        buf.extend_from_slice(&chunk?);
    }
    assert_eq!(buf, payload, "roundtrip 内容应一致（含 302 重定向跟随）");
    println!("[+] 读取 OK（{} 字节，302→presigned 跟随成功）", buf.len());

    let listed = target.list(&format!("{}/lvl1/lvl2", TARGET_FOLDER)).await?;
    assert!(
        listed.iter().any(|e| !e.is_dir && e.rel_path.ends_with("文 件+名.txt")),
        "列表应包含特殊字符文件名，实际: {:?}",
        listed.iter().map(|e| &e.rel_path).collect::<Vec<_>>()
    );
    println!("[+] 列表 OK: {:?}", listed.iter().map(|e| &e.rel_path).collect::<Vec<_>>());

    // 3) BackupJob 端到端（age 加密 → WebDAV）
    let dir = TempDir::new()?;
    let src_root = dir.path().join("src");
    std::fs::create_dir_all(&src_root)?;
    std::fs::write(src_root.join("readme.txt"), "fnos backup test readme\n".as_bytes())?;
    std::fs::write(src_root.join("data.bin"), generate_bytes(300_000))?;
    std::fs::create_dir_all(src_root.join("docs/manual/chapter1"))?;
    std::fs::write(src_root.join("docs/note.md"), "# Backup Note\ncontent here\n".as_bytes())?;
    std::fs::write(
        src_root.join("docs/manual/chapter1/guide.txt"),
        "Chapter 1 guide deep content\n".as_bytes(),
    )?;

    let keys = AgeKeys::generate();
    let crypto = CryptoSession::full(&keys);
    let store = Arc::new(SnapshotStore::open(&dir.path().join("meta.db"))?);

    let job = BackupJob {
        job_id: "webdav-test".to_string(),
        account: None,
        source: Arc::new(LocalFsSource::new(&src_root)),
        target: target.clone(),
        crypto: crypto.clone(),
        store: store.clone(),
        target_prefix: Some(TARGET_FOLDER.to_string()),
        eventbus: None,
        retention: None,
    };

    println!("=== 3. 首次全量备份（4 个文件，含多级嵌套）===");
    let s1 = job.run(&src_root).await?;
    println!("[+] 首次: uploaded={} bytes={}", s1.uploaded, s1.uploaded_bytes);
    assert_eq!(s1.uploaded, 4, "首次应上传 4 个文件");

    println!("=== 4. 增量备份（无变化，应 0 上传）===");
    let s2 = job.run(&src_root).await?;
    println!("[+] 二次: uploaded={} unchanged={}", s2.uploaded, s2.unchanged);
    assert_eq!(s2.uploaded, 0, "无变化时应 0 上传");

    println!("=== 5. 修改文件后增量（只传 readme.txt）===");
    std::fs::write(src_root.join("readme.txt"), "fnos backup test readme v2\n".as_bytes())?;
    let s3 = job.run(&src_root).await?;
    println!("[+] 三次: uploaded={}", s3.uploaded);
    assert_eq!(s3.uploaded, 1, "只应上传修改的 readme.txt");

    // 6) 下载解密校验（覆盖 302→presigned 下载链路）
    println!("=== 6. 下载解密校验 ===");
    let expected: &[(&str, &[u8])] = &[
        ("readme.txt", b"fnos backup test readme v2\n"),
        ("data.bin", &generate_bytes(300_000)),
        ("docs/note.md", b"# Backup Note\ncontent here\n"),
        ("docs/manual/chapter1/guide.txt", b"Chapter 1 guide deep content\n"),
    ];
    for (rel, exp) in expected {
        let rel_path = Path::new(TARGET_FOLDER).join(rel);
        let mut stream = target.read_stream(&rel_path).await?;
        let mut enc = Vec::new();
        while let Some(chunk) = stream.next().await {
            enc.extend_from_slice(&chunk?);
        }
        let mut out = Vec::new();
        crypto.decrypt_stream(&enc[..], &mut out)?;
        assert_eq!(out.as_slice(), *exp, "{} 内容应一致", rel);
        println!("[+] {} 解密校验通过 ({} 字节)", rel, out.len());
    }

    // 7) 清理测试目录（delete 语义：文件与目录递归）
    println!("=== 7. 清理测试目录 ===");
    cleanup(target.as_ref(), TARGET_FOLDER).await;
    println!("[+] 清理完成");

    println!("\n=== WebDAV 端到端测试全部通过 ===");
    Ok(())
}

/// 递归清理 WebDAV 测试目录（先文件后目录，自底向上）
async fn cleanup(target: &dyn TargetStorage, prefix: &str) {
    match target.list(prefix).await {
        Ok(entries) => {
            for e in entries {
                if e.is_dir {
                    Box::pin(cleanup(target, &e.rel_path)).await;
                }
                if let Err(err) = target.delete(Path::new(&e.rel_path)).await {
                    println!("[!] 删除 {} 失败: {}", e.rel_path, err);
                }
            }
        }
        Err(err) => println!("[!] 列出 {} 失败: {}", prefix, err),
    }
    if let Err(err) = target.delete(Path::new(prefix)).await {
        println!("[!] 删除目录 {} 失败: {}", prefix, err);
    }
}

/// 生成可复现的伪随机字节
fn generate_bytes(size: usize) -> Vec<u8> {
    let mut seed: u64 = 42;
    let mut buf = vec![0u8; size];
    for b in buf.iter_mut() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        *b = (seed & 0xFF) as u8;
    }
    buf
}
