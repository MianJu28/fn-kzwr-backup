//! 传输速度计量（只统计实际传输时段，支持多文件并发）
//!
//! 速度 = 累计**实际传输字节** ÷ 累计**实际传输耗时**。
//!
//! 关键点：
//! - 传输层在**响应返回后**会补一次「整次请求耗时」的观测，因此即便是小文件
//!   （body 被本地缓冲、瞬间写完）也能得到接近真实的速率；
//! - 忽略 `elapsed < MIN_OBSERVE_MS` 的观测，避免毫秒级抖动导致的虚高；
//! - 文件/请求之间的空闲不会计入耗时（无观测即无累加）；
//! - **并发模型**：共享的 [`SpeedMeter`] 只保存总字节/总耗时；
//!   每个在传文件持有独立的 [`FileMeter`]（记录该文件的上次观测基线），
//!   观测时算出增量汇入共享计量器——多路并发同时上报也不会互相污染基线。

/// 小于该耗时的观测直接忽略（毫秒）
const MIN_OBSERVE_MS: u64 = 50;

/// 速度计量器（字节/秒，共享总量）
///
/// 并发传输时多个文件同时汇入；`speed_bps()` 返回的是**所有在传/已传文件的
/// 合计速率**（与并发前单文件的口径一致，天然就是「总吞吐」）。
pub struct SpeedMeter {
    /// 累计实际传输字节
    bytes: u64,
    /// 累计实际传输耗时（毫秒）
    ms: u64,
}

/// 单文件的观测基线（每文件独立，消除并发下的基线互相污染）
pub struct FileMeter {
    /// 上次观测的累计已写字节（本文件内）
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
        Self { bytes: 0, ms: 0 }
    }

    /// 新文件开始：创建该文件的独立基线（并发安全：每文件一个）
    pub fn begin_file() -> FileMeter {
        FileMeter {
            last_written: 0,
            last_elapsed_ms: 0,
        }
    }

    /// 汇入一次有效观测增量
    fn add(&mut self, delta_bytes: u64, delta_ms: u64) {
        self.bytes = self.bytes.saturating_add(delta_bytes);
        self.ms = self.ms.saturating_add(delta_ms);
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

impl FileMeter {
    /// 观察一次进度，增量汇入共享计量器
    ///
    /// - `written`：本文件累计已写密文字节（多分片时跨分片连续）
    /// - `elapsed_ms`：本请求从开始到此刻的毫秒数（每次请求从 0 重新计）
    pub fn observe(&mut self, shared: &mut SpeedMeter, written: u64, elapsed_ms: u64) {
        if elapsed_ms < MIN_OBSERVE_MS {
            return; // 过短的观测（可能被本地缓冲）忽略，避免速度虚高
        }
        // 字节增量：written 回退说明换请求（重置），否则为同请求内的增量
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
        shared.add(db, dm);
        self.last_written = written;
        self.last_elapsed_ms = elapsed_ms;
    }
}
