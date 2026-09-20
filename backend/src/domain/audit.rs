//! 操作审计日志（本地 JSON Lines，append-only）
//!
//! 记录敏感 / 破坏性 / 配置类操作，便于事后追溯：
//! WebDAV 凭据变更、密钥更换与导出、配置导入、access-token 变更、
//! 清空回收站、手动备份与恢复等。
//!
//! 存储：`$TRIM_PKGVAR/audit.log`，每行一个 JSON 对象。
//! 文件超过阈值时自动裁剪（保留最近 `KEEP_LINES` 行），避免无限增长。
//! 本模块不做鉴权（应用本身仅在飞牛 iframe 内访问），IP 字段供追溯来源。

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// 单条审计记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// 时间戳（毫秒）
    pub ts: i64,
    /// 操作标识（如 `webdav.credentials`、`keys.export`、`kzwr.trash.empty`）
    pub action: String,
    /// 明细（人类可读）
    pub detail: String,
    /// 是否成功
    pub ok: bool,
    /// 来源 IP（取不到则为 None）
    pub ip: Option<String>,
}

/// 文件超过该大小时裁剪
const MAX_BYTES: u64 = 512 * 1024;
/// 裁剪后保留的行数
const KEEP_LINES: usize = 1000;

/// 审计日志（线程安全）
pub struct AuditLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl AuditLog {
    /// 在数据目录下创建（`<dir>/audit.log`）
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join("audit.log"),
            lock: Mutex::new(()),
        }
    }

    /// 记录一条（写失败仅记日志，不影响主流程）
    pub fn record(&self, action: &str, detail: impl Into<String>, ok: bool, ip: Option<String>) {
        let entry = AuditEntry {
            ts: now_ms(),
            action: action.to_string(),
            detail: detail.into(),
            ok,
            ip,
        };
        let _guard = self.lock.lock().unwrap();
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let line = match serde_json::to_string(&entry) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(err = %e, "序列化审计记录失败");
                return;
            }
        };
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| f.write_all(format!("{}\n", line).as_bytes()));
        if let Err(e) = appended {
            tracing::warn!(err = %e, "写入审计日志失败");
            return;
        }
        if std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0) > MAX_BYTES {
            self.trim();
        }
    }

    /// 读取最近 `limit` 条（**倒序**，最新在前）
    pub fn recent(&self, limit: usize) -> Vec<AuditEntry> {
        let _guard = self.lock.lock().unwrap();
        let file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        let mut items: Vec<AuditEntry> = BufReader::new(file)
            .lines()
            .map_while(|l| l.ok())
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<AuditEntry>(&l).ok())
            .collect();
        items.reverse();
        items.truncate(limit.max(1));
        items
    }

    /// 清空审计日志，返回清除的条数
    pub fn clear(&self) -> usize {
        let _guard = self.lock.lock().unwrap();
        let n = std::fs::File::open(&self.path)
            .map(|f| BufReader::new(f).lines().count())
            .unwrap_or(0);
        if let Err(e) = std::fs::write(&self.path, "") {
            tracing::warn!(err = %e, "清空审计日志失败");
            return 0;
        }
        n
    }

    /// 裁剪日志文件（保留最近 KEEP_LINES 行）
    fn trim(&self) {
        let file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return,
        };
        let lines: Vec<String> = BufReader::new(file)
            .lines()
            .map_while(|l| l.ok())
            .collect();
        if lines.len() <= KEEP_LINES {
            return;
        }
        let keep = &lines[lines.len() - KEEP_LINES..];
        let content = format!("{}\n", keep.join("\n"));
        if let Err(e) = std::fs::write(&self.path, content) {
            tracing::warn!(err = %e, "裁剪审计日志失败");
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
