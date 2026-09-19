//! 传输速度计量（只统计实际传输时段）
//!
//! 速度 = 累计**实际传输字节** ÷ 累计**实际传输耗时**。
//!
//! 关键点：
//! - 传输层在**响应返回后**会补一次「整次请求耗时」的观测，因此即便是小文件
//!   （body 被本地缓冲、瞬间写完）也能得到接近真实的速率；
//! - 忽略 `elapsed < MIN_OBSERVE_MS` 的观测，避免毫秒级抖动导致的虚高；
//! - 文件/请求之间的空闲不会计入耗时（无观测即无累加）。

/// 小于该耗时的观测直接忽略（毫秒）
const MIN_OBSERVE_MS: u64 = 50;

/// 速度计量器（字节/秒）
pub struct SpeedMeter {
    /// 累计实际传输字节
    bytes: u64,
    /// 累计实际传输耗时（毫秒）
    ms: u64,
    /// 上次观测的累计已写字节（当前文件内）
    last_written: u64,
    /// 上次观测时该请求已用时（毫秒）
    last_elapsed_ms: u64,
}

impl Default for SpeedMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeedMeter {
    pub fn new() -> Self {
        Self {
            bytes: 0,
            ms: 0,
            last_written: 0,
            last_elapsed_ms: 0,
        }
    }

    /// 新文件开始：重置字节基线（保留 elapsed 以便识别新请求）
    pub fn begin_file(&mut self) {
        self.last_written = 0;
    }

    /// 观察一次进度
    ///
    /// - `written`：当前文件累计已写字节（多分片时跨分片连续）
    /// - `elapsed_ms`：当前请求从开始到此刻的毫秒数（每次请求从 0 重新计）
    pub fn observe(&mut self, written: u64, elapsed_ms: u64) {
        if elapsed_ms < MIN_OBSERVE_MS {
            return; // 过短的观测（可能被本地缓冲）忽略，避免速度虚高
        }
        // 字节增量：written 回退说明换文件（重置），否则为同文件内的增量
        let db = if written >= self.last_written {
            written - self.last_written
        } else {
            written
        };
        // 耗时增量：elapsed 回退说明换请求（新请求从 0 起算）
        let dm = if elapsed_ms >= self.last_elapsed_ms {
            elapsed_ms - self.last_elapsed_ms
        } else {
            elapsed_ms
        };
        self.bytes = self.bytes.saturating_add(db);
        self.ms = self.ms.saturating_add(dm);
        self.last_written = written;
        self.last_elapsed_ms = elapsed_ms;
    }

    /// 当前速度（字节/秒；无有效样本时为 0）
    pub fn speed_bps(&self) -> u64 {
        if self.ms > 0 {
            self.bytes.saturating_mul(1000) / self.ms
        } else {
            0
        }
    }
}
