//! 断点续传机制测试
//!
//! 验证：备份时逐文件即时保存快照（save_entry），
//! 模拟中断后（部分文件快照缺失），再次备份只上传缺失的文件，其余跳过。

use anyhow::Result;
use tempfile::TempDir;

use fnos_backup::infra::persistence::snapshot::{SnapshotEntry, SnapshotStore};
use fnos_backup::infra::storage_trait::FileDescriptor;

fn make_entry(rel: &str) -> SnapshotEntry {
    SnapshotEntry {
        rel_path: rel.to_string(),
        size: 100,
        mtime_secs: 1000,
        is_dir: false,
        digest: None,
    }
}

fn main() -> Result<()> {
    let dir = TempDir::new()?;
    let db_path = dir.path().join("meta.db");
    let store = SnapshotStore::open(&db_path)?;

    // 1) 模拟备份过程中，逐个文件 save_entry（断点续传）
    println!("=== 1. 逐文件保存快照 (save_entry) ===");
    store.save_entry("job-0", &make_entry("a.txt"))?;
    store.save_entry("job-0", &make_entry("b.txt"))?;
    // 模拟 c.txt 上传中断，未保存快照
    println!("[+] 已保存 a.txt, b.txt；c.txt 上传中断未保存");

    // 2) 再次备份时加载快照
    println!("=== 2. 加载快照 (断点续传基线) ===");
    let last = store.load_snapshot("job-0")?;
    println!("[+] 快照中有 {} 个文件", last.len());
    assert!(last.len() == 2, "应只有已完成的 2 个文件");

    // 3) 模拟中断后重跑：当前扫描有 a,b,c 三个文件
    let current = vec![
        FileDescriptor {
            rel_path: "a.txt".to_string(),
            size: 100,
            modified: None,
            is_dir: false,
            digest: None,
        },
        FileDescriptor {
            rel_path: "b.txt".to_string(),
            size: 100,
            modified: None,
            is_dir: false,
            digest: None,
        },
        FileDescriptor {
            rel_path: "c.txt".to_string(),
            size: 100,
            modified: None,
            is_dir: false,
            digest: None,
        },
    ];
    // 构建 mtime 匹配（make_entry 用 mtime_secs=1000，current modified 需匹配）
    let current: Vec<FileDescriptor> = current
        .iter()
        .map(|f| FileDescriptor {
            rel_path: f.rel_path.clone(),
            size: f.size,
            modified: Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000)),
            is_dir: false,
            digest: None,
        })
        .collect();

    println!("=== 3. 中断后重跑差分 ===");
    let cs = fnos_backup::domain::sync::SyncSession::diff(&current, &last);
    println!(
        "[+] upload={} unchanged={} (c.txt 应重新上传，a/b 跳过)",
        cs.upload.len(),
        cs.unchanged
    );
    // a.txt, b.txt 已保存快照且 mtime/size 匹配 → 跳过
    // c.txt 无快照 → 上传
    assert!(cs.upload.len() == 1, "只应重传 c.txt");
    assert!(cs.upload[0].rel_path == "c.txt", "重传的应是 c.txt");
    assert!(cs.unchanged == 2, "a/b 应跳过");
    println!("[+] 断点续传正确：只重传中断的 c.txt");

    println!("\n=== 断点续传测试通过 ===");
    Ok(())
}
