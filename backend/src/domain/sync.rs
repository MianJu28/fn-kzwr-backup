//! 增量同步（ADR-004：增量检测双策略）
//!
//! 快速策略（默认）：比较 mtime + size，命中即跳过。O(1)。
//! 生成 ChangeSet：新增/修改的文件上传，删除的文件从目标移除。

use std::collections::HashMap;

use crate::infra::persistence::snapshot::SnapshotEntry;
use crate::infra::storage_trait::FileDescriptor;

/// 变更集：一次备份需要执行的操作
#[derive(Debug, Default)]
pub struct ChangeSet {
    /// 需要上传的文件（新增或修改）
    pub upload: Vec<FileDescriptor>,
    /// 需要从目标删除的文件（本地已删除）
    pub delete: Vec<String>,
    /// 未变化、无需操作的文件
    pub unchanged: usize,
}

/// 快速策略的差分结果
#[derive(Debug, Clone, PartialEq)]
pub enum DiffAction {
    /// 新增或内容变化，需上传
    Upload,
    /// 未变化，跳过
    Skip,
    /// 本地已删除，需从目标删除
    Delete,
}

/// 增量检测：对比当前扫描结果与上次快照，生成变更集
pub struct SyncSession;

impl SyncSession {
    /// 执行差分（快速策略 mtime+size）
    pub fn diff(
        current: &[FileDescriptor],
        last_snapshot: &[SnapshotEntry],
    ) -> ChangeSet {
        Self::diff_inner(current, last_snapshot, &|_| None)
    }

    /// 执行差分（严格策略）
    ///
    /// 在 mtime/size 变化时，再调用 `hasher` 计算 BLAKE3 内容哈希确认：
    /// - mtime/size 变了但内容哈希相同 → 跳过（防 mtime 欺骗导致的误判）
    /// - mtime 被欺骗改回旧值但内容变了 → 比较哈希能发现（需 last.digest 非空）
    /// hasher 需返回该文件的 BLAKE3 内容哈希（hex 字符串）。
    pub fn diff_strict<F>(
        current: &[FileDescriptor],
        last_snapshot: &[SnapshotEntry],
        hasher: F,
    ) -> ChangeSet
    where
        F: Fn(&FileDescriptor) -> Option<String>,
    {
        Self::diff_inner(current, last_snapshot, &hasher)
    }

    /// 差分核心实现，`hasher` 用于严格模式下对可疑文件算内容哈希
    fn diff_inner<F>(
        current: &[FileDescriptor],
        last_snapshot: &[SnapshotEntry],
        hasher: &F,
    ) -> ChangeSet
    where
        F: Fn(&FileDescriptor) -> Option<String>,
    {
        // 上次快照映射：rel_path -> entry
        let mut last_map: HashMap<&str, &SnapshotEntry> = HashMap::new();
        for e in last_snapshot {
            last_map.insert(e.rel_path.as_str(), e);
        }

        // 本次扫描映射：rel_path -> fd（用于识别已删除文件）
        let mut current_map: HashMap<&str, &FileDescriptor> = HashMap::new();
        for fd in current {
            current_map.insert(fd.rel_path.as_str(), fd);
        }

        let mut cs = ChangeSet::default();
        for fd in current {
            let action = match last_map.get(fd.rel_path.as_str()) {
                // 上次不存在 → 新增
                None => DiffAction::Upload,
                Some(last) => {
                    let mtime_changed = last.mtime_secs != mtime_secs(fd);
                    let size_changed = last.size != fd.size;
                    // 严格模式：优先用内容哈希判断（可发现 mtime/size 欺骗）
                    let cur_digest = hasher(fd);
                    if let Some(cur_d) = &cur_digest {
                        match &last.digest {
                            // 上次也有哈希 → 直接比较内容哈希（权威判定）
                            Some(last_d) if last_d == cur_d => DiffAction::Skip,
                            _ => DiffAction::Upload,
                        }
                    } else {
                        // 无法算哈希（目录等）→ 回退到 mtime/size
                        if mtime_changed || size_changed {
                            DiffAction::Upload
                        } else {
                            DiffAction::Skip
                        }
                    }
                }
            };
            match action {
                DiffAction::Upload => cs.upload.push(fd.clone()),
                DiffAction::Skip => cs.unchanged += 1,
                DiffAction::Delete => {}
            }
        }

        // 上次快照中有、但本次扫描没有的 → 已删除
        for last in last_snapshot {
            if !current_map.contains_key(last.rel_path.as_str()) {
                cs.delete.push(last.rel_path.clone());
            }
        }
        cs
    }
}

/// 计算文件的 BLAKE3 内容哈希（hex 字符串）
pub fn blake3_hex(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

impl SyncSession {
    /// BLAKE3 hex（供严格模式差分使用）
    pub fn blake3_hex(data: &[u8]) -> String {
        blake3::hash(data).to_hex().to_string()
    }
}

/// 提取文件 mtime 秒级时间戳
pub fn mtime_secs(fd: &FileDescriptor) -> i64 {
    fd.modified
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
