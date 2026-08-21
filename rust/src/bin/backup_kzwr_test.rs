//! kzwr 真实增量备份端到端测试
//!
//! 用真实 KzwrTarget 验证增量备份上传到 fn-backup 文件夹：
//!   1. 首次全量备份（age 加密 → kzwr fn-backup 分片上传）
//!   2. 增量备份（mtime+size 差分）
//!   3. 从 kzwr 下载解密校验
//! 需要 TRIM_KZWR_TOKEN 环境变量（登录二进制产出）。

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
use fnos_backup::infra::target::kzwr::client::KzwrClient;
use fnos_backup::infra::target::kzwr::storage::KzwrTarget;

/// 目标文件夹前缀
const TARGET_FOLDER: &str = "fn-backup";

#[tokio::main]
async fn main() -> Result<()> {
    let token = std::env::var("TRIM_KZWR_TOKEN")
        .unwrap_or_else(|_| panic!("请设置 TRIM_KZWR_TOKEN 环境变量为 access-token"));
    let base_url = std::env::var("TRIM_KZWR_BASE_URL")
        .unwrap_or_else(|_| "https://www.kzwr.com".to_string());

    // 构建 kzwr Target（认证）
    let mut client = KzwrClient::new(&base_url, 30);
    client.set_token(&token);
    let target: Arc<dyn TargetStorage> = Arc::new(KzwrTarget::new(client));

    // 准备本地源目录 + 测试文件
    let dir = TempDir::new()?;
    let src_root = dir.path().join("src");
    std::fs::create_dir_all(&src_root)?;
    std::fs::write(src_root.join("readme.txt"), "fnos backup test readme\n".as_bytes()).unwrap();
    std::fs::write(src_root.join("data.bin"), generate_bytes(300_000)).unwrap();
    std::fs::create_dir_all(src_root.join("docs")).unwrap();
    std::fs::write(src_root.join("docs/note.md"), "# Backup Note\ncontent here\n".as_bytes()).unwrap();

    // 加密组件
    let keys = AgeKeys::generate();
    let crypto = CryptoSession::full(&keys);
    println!("[*] age 公钥: {}", keys.recipient_str());
    println!("[*] age 私钥(测试展示): {}", keys.to_secret_key());

    // 快照库
    let store = Arc::new(SnapshotStore::open(&dir.path().join("meta.db"))?);

    let job = BackupJob {
        job_id: "kzwr-demo".to_string(),
        source: Arc::new(LocalFsSource::new(&src_root)),
        target: target.clone(),
        crypto: crypto.clone(),
        store: store.clone(),
        target_prefix: Some(TARGET_FOLDER.to_string()),
    };

    // 1) 首次全量备份
    println!("=== 1. 首次全量备份 → kzwr/{TARGET_FOLDER} ===");
    let s1 = job.run(&src_root).await?;
    println!(
        "[+] 首次: uploaded={} bytes={} deleted={} unchanged={}",
        s1.uploaded, s1.uploaded_bytes, s1.deleted, s1.unchanged
    );
    assert!(s1.uploaded == 3, "首次应上传 3 个文件");

    // 2) 二次备份（无变化，应全跳过）
    println!("=== 2. 增量备份（无变化）===");
    let s2 = job.run(&src_root).await?;
    println!(
        "[+] 二次: uploaded={} deleted={} unchanged={}",
        s2.uploaded, s2.deleted, s2.unchanged
    );
    assert!(s2.uploaded == 0, "无变化时应 0 上传");

    // 3) 修改文件 → 只上传变化
    std::fs::write(src_root.join("readme.txt"), "fnos backup test readme v2\n".as_bytes()).unwrap();
    println!("=== 3. 增量备份（readme.txt 已修改）===");
    let s3 = job.run(&src_root).await?;
    println!(
        "[+] 三次: uploaded={} bytes={} unchanged={}",
        s3.uploaded, s3.uploaded_bytes, s3.unchanged
    );
    assert!(s3.uploaded == 1, "只应上传修改的 readme.txt");

    // 3b) 诊断：列出 /fn-backup 内容
    println!("=== 3b. 诊断 list(/fn-backup) ===");
    match target.list("/fn-backup").await {
        Ok(entries) => {
            println!("[+] /fn-backup 共 {} 条目:", entries.len());
            for e in entries {
                println!("    - [{}] {} (size={})", if e.is_dir { "DIR" } else { "FILE" }, e.rel_path, e.size);
            }
        }
        Err(e) => println!("[!] list(/fn-backup) 失败: {:?}", e),
    }

    // 4) 从 kzwr 下载解密校验
    println!("=== 4. 从 kzwr 下载解密校验 ===");
    for rel in ["readme.txt", "data.bin", "docs/note.md"] {
        let rel_path = Path::new(TARGET_FOLDER).join(rel);
        let mut stream = target.read_stream(&rel_path).await?;
        let mut enc = Vec::new();
        while let Some(chunk) = stream.next().await {
            enc.extend_from_slice(&chunk?);
        }
        // 解密
        let mut out = Vec::new();
        crypto.decrypt_stream(&enc[..], &mut out)?;
        println!("[+] {} 解密成功 ({} 字节)", rel, out.len());
    }

    // 5) 校验 readme.txt 是最新内容
    let rel_path = Path::new(TARGET_FOLDER).join("readme.txt");
    let mut stream = target.read_stream(&rel_path).await?;
    let mut enc = Vec::new();
    while let Some(chunk) = stream.next().await {
        enc.extend_from_slice(&chunk?);
    }
    let mut out = Vec::new();
    crypto.decrypt_stream(&enc[..], &mut out)?;
    assert_eq!(out, b"fnos backup test readme v2\n", "readme.txt 应为最新内容");
    println!("[+] readme.txt 内容校验通过: {}", String::from_utf8_lossy(&out));

    println!("\n=== kzwr 真实增量备份测试全部通过 ===");
    Ok(())
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
