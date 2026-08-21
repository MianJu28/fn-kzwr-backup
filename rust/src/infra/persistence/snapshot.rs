//! 文件快照元数据存储（ADR-006：SQLite + WAL）
//!
//! 持久化每次备份的文件快照，作为增量差分（mtime+size）的基线。
//! 表：sync_snapshots（任务维度）

use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::infra::storage_trait::FileDescriptor;

/// 一条文件快照记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub rel_path: String,
    pub size: u64,
    /// mtime 秒级时间戳（0 表示未知）
    pub mtime_secs: i64,
    pub is_dir: bool,
    /// BLAKE3 内容哈希（严格模式用，hex 字符串；目录为 None）
    pub digest: Option<String>,
}

/// 快照存储（线程安全，单写多读）
pub struct SnapshotStore {
    conn: Mutex<Connection>,
}

impl SnapshotStore {
    /// 打开（或创建）数据库并初始化表结构，启用 WAL
    pub fn open(db_path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS sync_snapshots (
                job_id     TEXT NOT NULL,
                rel_path   TEXT NOT NULL,
                size       INTEGER NOT NULL,
                mtime_secs INTEGER NOT NULL,
                is_dir     INTEGER NOT NULL,
                digest     TEXT,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (job_id, rel_path)
             );
             CREATE INDEX IF NOT EXISTS idx_snapshots_job ON sync_snapshots(job_id);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 保存整个快照（事务批量 upsert）
    pub fn save_snapshot(&self, job_id: &str, entries: &[SnapshotEntry]) -> rusqlite::Result<()> {
        let now = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO sync_snapshots (job_id, rel_path, size, mtime_secs, is_dir, digest, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(job_id, rel_path) DO UPDATE SET
                    size = excluded.size,
                    mtime_secs = excluded.mtime_secs,
                    is_dir = excluded.is_dir,
                    digest = excluded.digest,
                    updated_at = excluded.updated_at",
            )?;
            for e in entries {
                stmt.execute(rusqlite::params![
                    job_id,
                    e.rel_path,
                    e.size as i64,
                    e.mtime_secs,
                    e.is_dir as i64,
                    e.digest,
                    now
                ])?;
            }
        }
        tx.commit()
    }

    /// 加载某个任务的完整快照（用于差分基线）
    pub fn load_snapshot(&self, job_id: &str) -> rusqlite::Result<Vec<SnapshotEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT rel_path, size, mtime_secs, is_dir, digest FROM sync_snapshots WHERE job_id = ?1",
        )?;
        let rows = stmt.query_map([job_id], |r| {
            Ok(SnapshotEntry {
                rel_path: r.get(0)?,
                size: r.get::<_, i64>(1)? as u64,
                mtime_secs: r.get(2)?,
                is_dir: r.get::<_, i64>(3)? != 0,
                digest: r.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// 获取单个文件的快照记录（可选）
    pub fn get_entry(&self, job_id: &str, rel_path: &str) -> rusqlite::Result<Option<SnapshotEntry>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT rel_path, size, mtime_secs, is_dir, digest FROM sync_snapshots WHERE job_id=?1 AND rel_path=?2",
                [job_id, rel_path],
                |r| {
                    Ok(SnapshotEntry {
                        rel_path: r.get(0)?,
                        size: r.get::<_, i64>(1)? as u64,
                        mtime_secs: r.get(2)?,
                        is_dir: r.get::<_, i64>(3)? != 0,
                        digest: r.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }
}

impl SnapshotEntry {
    /// 从 FileDescriptor 构建快照条目
    pub fn from_fd(fd: &FileDescriptor) -> Self {
        let mtime_secs = fd
            .modified
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Self {
            rel_path: fd.rel_path.clone(),
            size: fd.size,
            mtime_secs,
            is_dir: fd.is_dir,
            digest: fd
                .digest
                .map(|d| d.iter().map(|b| format!("{:02x}", b)).collect()),
        }
    }
}
