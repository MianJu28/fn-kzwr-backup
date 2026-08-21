//! kzwr 真实恢复端到端测试
//!
//! 用真实 KzwrTarget 验证：备份 → 修改源 → RestoreJob 恢复备份版本 → 校验。
//! 需要 TRIM_KZWR_TOKEN 环境变量。

use std::sync::Arc;

use anyhow::Result;
use tempfile::TempDir;

use fnos_backup::domain::backup::BackupJob;
use fnos_backup::domain::crypto::{AgeKeys, CryptoSession};
use fnos_backup::domain::restore::RestoreJob;
use fnos_backup::infra::persistence::snapshot::SnapshotStore;
use fnos_backup::infra::source::local::LocalFsSource;
use fnos_backup::infra::storage_trait::TargetStorage;
use fnos_backup::infra::target::kzwr::client::KzwrClient;
use fnos_backup::infra::target::kzwr::storage::KzwrTarget;

const TARGET_FOLDER: &str = "fn-backup";

#[tokio::main]
async fn main() -> Result<()> {
    let token = std::env::var("TRIM_KZWR_TOKEN")
        .unwrap_or_else(|_| panic!("请设置 TRIM_KZWR_TOKEN 环境变量为 access-token"));
    let base_url = std::env::var("TRIM_KZWR_BASE_URL")
        .unwrap_or_else(|_| "https://www.kzwr.com".to_string());

    let mut client = KzwrClient::new(&base_url, 30);
    client.set_token(&token);
    let target: Arc<dyn TargetStorage> = Arc::new(KzwrTarget::new(client));

    // 本地源
    let dir = TempDir::new()?;
    let src_root = dir.path().join("src");
    std::fs::create_dir_all(&src_root)?;
    std::fs::write(src_root.join("app.conf"), "version=1.0\n".as_bytes()).unwrap();
    std::fs::write(src_root.join("photo.jpg"), vec![0u8; 500_000]).unwrap();
    std::fs::create_dir_all(src_root.join("data/db")).unwrap();
    std::fs::write(src_root.join("data/db/records.db"), b"db backup snapshot").unwrap();

    let keys = AgeKeys::generate();
    let crypto = CryptoSession::full(&keys);
    let store = Arc::new(SnapshotStore::open(&dir.path().join("meta.db"))?);

    // 1) 备份到 kzwr
    println!("=== 1. 备份到 kzwr/{TARGET_FOLDER} ===");
    let backup_job = BackupJob {
        job_id: "kzwr-restore".to_string(),
        source: Arc::new(LocalFsSource::new(&src_root)),
        target: target.clone(),
        crypto: crypto.clone(),
        store: store.clone(),
        target_prefix: Some(TARGET_FOLDER.to_string()),
    };
    let s = backup_job.run(&src_root).await?;
    println!("[+] 备份: uploaded={}", s.uploaded);
    assert!(s.uploaded == 3, "应备份 3 个文件");

    // 2) 修改源（备份后变化）
    std::fs::write(src_root.join("app.conf"), b"version=2.0\n").unwrap();
    println!("=== 2. 修改源 app.conf（备份后变化）===");

    // 3) 恢复备份版本到 restore 目录
    println!("=== 3. 从 kzwr 恢复备份版本 ===");
    let restore_dir = dir.path().join("restore");
    let restore_job = RestoreJob {
        target: target.clone(),
        crypto: crypto.clone(),
        target_prefix: Some(TARGET_FOLDER.to_string()),
    };
    let r = restore_job.run(&[], &restore_dir).await?;
    println!("[+] 恢复: restored={} bytes={}", r.restored, r.restored_bytes);
    assert!(r.restored == 3, "应恢复 3 个文件");

    // 4) 校验恢复的是备份版本（version=1.0，而非修改后的 2.0）
    let conf = std::fs::read_to_string(restore_dir.join("app.conf"))?;
    assert_eq!(conf, "version=1.0\n", "app.conf 应恢复为备份版本 v1.0");
    println!("[+] app.conf 恢复为备份版本: {}", conf.trim());
    assert_eq!(
        std::fs::read(restore_dir.join("photo.jpg"))?,
        vec![0u8; 500_000]
    );
    assert_eq!(
        std::fs::read_to_string(restore_dir.join("data/db/records.db"))?,
        "db backup snapshot"
    );
    println!("[+] 多级文件 data/db/records.db 恢复校验通过");
    println!("[+] photo.jpg 恢复校验通过 (500000 字节)");

    println!("\n=== kzwr 真实恢复测试全部通过 ===");
    Ok(())
}
