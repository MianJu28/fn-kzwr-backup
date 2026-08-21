//! BLAKE3 严格模式差分测试
//!
//! 验证 SyncSession::diff_strict：
//!   1. mtime 变了但内容没变 → 严格模式跳过（快速模式会重传）
//!   2. mtime 被欺骗改回旧值但内容变了 → 严格模式能发现（快速模式会误判跳过）

use std::time::{Duration, UNIX_EPOCH};

use fnos_backup::domain::sync::SyncSession;
use fnos_backup::infra::persistence::snapshot::SnapshotEntry;
use fnos_backup::infra::storage_trait::FileDescriptor;

fn make_fd(rel: &str, size: u64, mtime: i64, digest: Option<[u8; 32]>) -> FileDescriptor {
    FileDescriptor {
        rel_path: rel.to_string(),
        size,
        modified: Some(UNIX_EPOCH + Duration::from_secs(mtime as u64)),
        is_dir: false,
        digest,
    }
}

fn make_entry(rel: &str, size: u64, mtime: i64, digest: Option<String>) -> SnapshotEntry {
    SnapshotEntry {
        rel_path: rel.to_string(),
        size,
        mtime_secs: mtime,
        is_dir: false,
        digest,
    }
}

fn main() {
    let content_v1 = b"some content version 1";
    let content_v2 = b"some content version 2";
    let digest_v1 = SyncSession::blake3_hex(content_v1);
    let digest_bytes_v1: [u8; 32] = blake3::hash(content_v1).into();

    // ── 场景 1：mtime 变了但内容没变 ──
    println!("=== 场景1: mtime 变了但内容没变 ===");
    // 上次快照：mtime=100, digest=v1
    let last = vec![make_entry("f.txt", content_v1.len() as u64, 100, Some(digest_v1.clone()))];
    // 当前：mtime=200（变了）, size 相同, 内容仍是 v1
    let current = vec![make_fd("f.txt", content_v1.len() as u64, 200, Some(digest_bytes_v1))];

    // 快速模式：mtime 变了 → 判定上传（误判，因为内容没变）
    let fast = SyncSession::diff(&current, &last);
    println!("[快速模式] upload={} (mtime变了会误传)", fast.upload.len());
    assert!(fast.upload.len() == 1, "快速模式 mtime 变应判定上传");

    // 严格模式：hasher 算出 v1，与 last.digest(v1) 相同 → 跳过
    let strict = SyncSession::diff_strict(&current, &last, |_| {
        Some(SyncSession::blake3_hex(content_v1))
    });
    println!("[严格模式] upload={} unchanged={} (内容未变应跳过)", strict.upload.len(), strict.unchanged);
    assert!(strict.upload.is_empty(), "严格模式内容未变应跳过");

    // ── 场景 2：mtime 被欺骗改回旧值但内容变了 ──
    println!("=== 场景2: mtime 被欺骗改回旧值但内容变了 ===");
    // 上次快照：mtime=100, digest=v1
    let last2 = vec![make_entry("f.txt", content_v1.len() as u64, 100, Some(digest_v1.clone()))];
    // 当前：mtime=100(欺骗改回旧值), 但 size 也变了(内容v2更长) → size 变化能发现
    let current2 = vec![make_fd("f.txt", content_v2.len() as u64, 100, Some(digest_bytes_v1))];

    // 严格模式：size 变了 → hasher 算出 v2，与 last.digest(v1) 不同 → 上传
    let strict2 = SyncSession::diff_strict(&current2, &last2, |_| {
        Some(SyncSession::blake3_hex(content_v2))
    });
    println!("[严格模式] upload={} (内容变了应上传)", strict2.upload.len());
    assert!(strict2.upload.len() == 1, "内容变了严格模式应上传");

    // ── 场景 3：mtime 和 size 都相同，但内容被替换（mtime 欺骗最隐蔽场景）──
    println!("=== 场景3: mtime+size 相同但内容替换(欺骗) ===");
    // 用两个不同内容但长度相同
    let a = b"AAAAAAA"; // 7 bytes
    let b2 = b"BBBBBBB"; // 7 bytes 同长度
    let dig_a = SyncSession::blake3_hex(a);
    let dig_b = SyncSession::blake3_hex(b2);
    let last3 = vec![make_entry("g.txt", 7, 50, Some(dig_a.clone()))];
    // 当前：mtime=50(同), size=7(同), 但内容被替换为 b
    let current3 = vec![make_fd("g.txt", 7, 50, Some(blake3::hash(b2).into()))];

    // 快速模式：mtime+size 都相同 → 跳过（误判，内容其实变了）
    let fast3 = SyncSession::diff(&current3, &last3);
    println!("[快速模式] upload={} (被欺骗跳过,内容变了却漏传)", fast3.upload.len());
    assert!(fast3.upload.is_empty(), "快速模式会漏判欺骗");

    // 严格模式：digest(b) != digest(a) → 上传（发现欺骗）
    let strict3 = SyncSession::diff_strict(&current3, &last3, |_| {
        Some(dig_b.clone())
    });
    println!("[严格模式] upload={} (发现内容替换应上传)", strict3.upload.len());
    assert!(strict3.upload.len() == 1, "严格模式应发现内容替换");

    println!("\n=== BLAKE3 严格模式测试全部通过 ===");
}
