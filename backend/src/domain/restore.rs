//! 恢复编排（核心域：选择性恢复、完整性校验）
//!
//! 从目标存储读取加密数据 → age 解密 → 写回本地目录。
//! 支持选择性恢复（只恢复指定的文件）。
//!
//! 目标端布局：`/<target_prefix>/<source_root_name>/<rel>`，其中 `source_root_name`
//! 为备份源文件夹名（与备份侧 `run_multi` 的分目录策略对应）。
//!
//! 恢复后会**把本地文件 mtime 回写为快照中记录的原始 mtime**，使下次增量备份
//! 判定为「未变化」而跳过，避免恢复后又被重新上传。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use bytes::Bytes;
use futures::StreamExt;
use tracing::info;

use crate::domain::crypto::CryptoSession;
use crate::domain::pace::SpeedMeter;
use crate::infra::persistence::snapshot::{SnapshotEntry, SnapshotStore};
use crate::infra::storage_trait::TargetStorage;

/// 恢复后需要同步的快照目标（用于把恢复结果写回快照，避免下次备份重复上传）
pub struct RestoreSnapshotTarget {
    pub store: Arc<SnapshotStore>,
    pub job_id: String,
    pub account: String,
}

/// 恢复任务
pub struct RestoreJob {
    /// 目标存储（读取加密数据）
    pub target: Arc<dyn TargetStorage>,
    /// 解密会话（含 age 私钥）
    pub crypto: CryptoSession,
    /// 目标根前缀（备份时写入的前缀，如 "fn-backup"），与备份对应
    pub target_prefix: Option<String>,
    /// 备份源文件夹名（备份时在目标前缀下多分的目录，如 "Photos"）
    pub source_root_name: Option<String>,
    /// 内部事件总线（进度推送，可选）
    pub eventbus: Option<Arc<crate::eventbus::EventBus>>,
    /// 快照元数据：rel_path -> (明文大小, 原始 mtime 秒)
    pub meta: HashMap<String, (u64, i64)>,
    /// 恢复结果写回快照的目标（仅在恢复到原备份位置时提供）
    pub snapshot_target: Option<RestoreSnapshotTarget>,
    /// 密文暂存目录（`$TRIM_PKGTMP/staging`；`None` = 退回系统临时目录）
    ///
    /// 恢复大文件时密文必须先落盘而不是进内存（P0-2）。
    pub staging_dir: Option<std::path::PathBuf>,
    /// 取消标志（可选）：置位后**在当前文件结束后**停止
    ///
    /// 与备份同理：单文件的下载在 FFI 里推进，文件边界停下才是安全的
    /// （已恢复的文件保留，未开始的跳过，重跑即可续）。
    pub cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
}

/// 把异步 channel 适配成同步 `std::io::Read`
///
/// 用途：恢复时「异步下载」与「同步 age 解密」需要对接 —— `decrypt_stream`
/// 要求 `impl Read`，而下载是 `Stream`。用有界 channel 桥接的好处是
/// **两种节奏自动匹配**：解密慢则 channel 满、下载自动等（背压），
/// 因此内存恒定在「通道深度 × 块大小」，不需要把整份密文落盘。
struct ChannelReader {
    rx: tokio::sync::mpsc::Receiver<Result<Bytes, String>>,
    /// 当前块未读完的部分
    cur: Option<Bytes>,
    /// 是否已出错/结束（后续 read 立即返回 0）
    done: bool,
}

impl ChannelReader {
    fn new(rx: tokio::sync::mpsc::Receiver<Result<Bytes, String>>) -> Self {
        Self { rx, cur: None, done: false }
    }
}

impl std::io::Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if let Some(c) = self.cur.take() {
                let n = c.len().min(buf.len());
                buf[..n].copy_from_slice(&c[..n]);
                if n < c.len() {
                    self.cur = Some(c.slice(n..));
                }
                return Ok(n);
            }
            if self.done {
                return Ok(0);
            }
            // 从异步侧阻塞取下一块。本函数在 `spawn_blocking` 线程上执行，
            // 那里没有 tokio runtime 上下文，因此 `blocking_recv` 是安全的。
            match self.rx.blocking_recv() {
                Some(Ok(b)) => {
                    if b.is_empty() {
                        continue;
                    }
                    self.cur = Some(b);
                }
                Some(Err(msg)) => {
                    self.done = true;
                    // 把下载错误**转成 io::Error** 传上去：decrypt_stream 会带着
                    // 上下文返回，恢复任务据此判失败并重试，而不是当作正常 EOF
                    // 产出一个被截断的明文文件。
                    return Err(std::io::Error::other(format!("下载中断: {msg}")));
                }
                None => {
                    self.done = true;
                    return Ok(0);
                }
            }
        }
    }
}

/// 下载通道深度（块数）：越小内存越省，越大越能吸收网络抖动
const DL_CHANNEL_DEPTH: usize = 4;

impl RestoreJob {
    /// 目标端根路径：`/<prefix>[/<source_root_name>]`
    fn base_path(&self) -> String {
        let mut segs: Vec<String> = Vec::new();
        if let Some(p) = &self.target_prefix {
            let p = p.trim_matches('/');
            if !p.is_empty() {
                segs.push(p.to_string());
            }
        }
        if let Some(r) = &self.source_root_name {
            let r = r.trim_matches('/');
            if !r.is_empty() {
                segs.push(r.to_string());
            }
        }
        if segs.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", segs.join("/"))
        }
    }

    /// 执行恢复
    ///
    /// - `rel_files`: 要恢复的文件相对路径列表（选择性恢复）。空列表 = 恢复全部已备份文件。
    /// - `restore_dir`: 本地恢复目标目录（明文写入位置）
    pub async fn run(&self, rel_files: &[String], restore_dir: &Path) -> Result<RestoreSummary> {
        std::fs::create_dir_all(restore_dir)?;

        let meter = Arc::new(Mutex::new(SpeedMeter::new()));
        let started = Instant::now();

        // 准备阶段：先推一条事件，避免「列取云端文件」期间面板毫无反馈
        self.publish(
            crate::eventbus::TaskStatus::Started,
            Some(crate::eventbus::TaskPhase::Prepare),
            None,
            0,
            0,
            0,
            0,
            started.elapsed().as_millis() as u64,
            0,
            Some("准备中：读取快照并列出云端待恢复文件…".to_string()),
        );

        let files_to_restore: Vec<String> = if rel_files.is_empty() {
            self.list_target_files().await?
        } else {
            rel_files.to_vec()
        };
        let total = files_to_restore.iter().filter(|r| !r.trim().is_empty()).count() as u64;
        // 待恢复的明文总大小（来自快照；未知则为 0）
        let total_bytes: u64 = files_to_restore
            .iter()
            .map(|r| self.meta.get(r).map(|(s, _)| *s).unwrap_or(0))
            .sum();

        // 准备完成 → 进入传输（下载）阶段
        self.publish(
            crate::eventbus::TaskStatus::Started,
            Some(crate::eventbus::TaskPhase::Transfer),
            None,
            0,
            total,
            0,
            total_bytes,
            started.elapsed().as_millis() as u64,
            0,
            None,
        );

        let mut restored = 0usize;
        let mut restored_bytes = 0u64;
        let mut missing: Vec<String> = Vec::new();
        // 顺序下载；共享计数器供进度回调读取基准（完成数含缺失跳过）
        let shared = Arc::new(Mutex::new((0u64, 0u64)));
        for rel in files_to_restore.into_iter().filter(|r| !r.trim().is_empty()) {
            // 取消检查（文件边界）：已恢复的文件保留，未开始的跳过
            if self
                .cancel
                .as_ref()
                .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
                .unwrap_or(false)
            {
                tracing::warn!("收到取消请求，在文件边界停止恢复");
                return Err(anyhow::anyhow!("任务已被用户取消"));
            }
            let file_plain = self.meta.get(&rel).map(|(s, _)| *s).unwrap_or(0);
            // 文件内实时进度：按下载密文字节（≈明文）换算为明文已传量
            let fm = Arc::new(Mutex::new(SpeedMeter::begin_file()));
            let cb: Option<crate::infra::storage_trait::ProgressCb> = {
                let eb = self.eventbus.clone();
                let file = rel.clone();
                let meter = meter.clone();
                let shared = shared.clone();
                Some(std::sync::Arc::new(
                    move |enc_read: u64, _t: u64, elapsed_ms: u64| {
                        let speed = {
                            let mut m = meter.lock().unwrap();
                            fm.lock().unwrap().observe(&mut m, enc_read, elapsed_ms);
                            m.speed_bps()
                        };
                        let (done, base) = {
                            let s = shared.lock().unwrap();
                            (s.0, s.1)
                        };
                        let plain_in_file = if file_plain > 0 {
                            enc_read.min(file_plain)
                        } else {
                            enc_read
                        };
                        if let Some(eb) = &eb {
                            eb.task_event(
                                crate::eventbus::TaskKind::Restore,
                                crate::eventbus::TaskStatus::Progress,
                                Some(crate::eventbus::TaskPhase::Transfer),
                                "restore".to_string(),
                                Some(file.clone()),
                                done,
                                total,
                                base + plain_in_file,
                                total_bytes,
                                started.elapsed().as_millis() as u64,
                                speed,
                                None,
                            );
                        }
                    },
                ))
            };
            // 网络类失败自动重试（最多 3 次，线性退避）；
            // 云端文件不存在 → 标记为缺失并继续；其余错误照常中止
            let (n, is_missing) = {
                let mut out: Result<(u64, bool), anyhow::Error> =
                    Err(anyhow::anyhow!("恢复未执行"));
                for attempt in 1..=crate::domain::NETWORK_RETRY_ATTEMPTS {
                    out = match self.restore_one(&rel, restore_dir, cb.clone()).await {
                        Ok(n) => Ok((n, false)),
                        Err(e) => {
                            // 「云端文件不存在」要能被识别成**可跳过**（记为 missing），
                            // 否则一个早就被删掉的孤岛条目会让整次恢复失败 ——
                            // 实测：12 个文件里 10 个已成功恢复，仅因快照残留的
                            // `mid-test.bin`（云端已无此文件）报
                            //   读取块长度失败: 下载中断: 网络/协议错误:
                            //   {"error":"读取失败: 路径不存在: /fn-backup/test/mid-test.bin"}
                            // 就整单判失败，用户看到「10/12 已恢复」却被标成任务失败。
                            //
                            // 插件侧把「路径不存在」包装成了 `Protocol("下载中断: ...")`，
                            // NotFound 语义在这一层丢了，所以还要按**消息文本**兜一层。
                            let not_found = e
                                .downcast_ref::<crate::infra::storage_trait::StorageError>()
                                .map(|se| {
                                    matches!(
                                        se,
                                        crate::infra::storage_trait::StorageError::NotFound(_)
                                    )
                                })
                                .unwrap_or(false)
                                || {
                                    let msg = format!("{e:#}");
                                    msg.contains("路径不存在")
                                        || msg.contains("404")
                                        || msg.to_ascii_lowercase().contains("not found")
                                };
                            if not_found {
                                tracing::warn!("云端文件不存在，恢复时跳过: {}", rel);
                                Ok((0u64, true))
                            } else {
                                Err(e)
                            }
                        }
                    };
                    match &out {
                        Ok(_) => break,
                        Err(e) => {
                            tracing::warn!(
                                "恢复 {} 第 {attempt}/{} 次失败: {e:#}",
                                rel,
                                crate::domain::NETWORK_RETRY_ATTEMPTS
                            );
                            if !crate::domain::is_retryable_err(e)
                                || attempt == crate::domain::NETWORK_RETRY_ATTEMPTS
                            {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(
                                500 * attempt as u64,
                            ))
                            .await;
                        }
                    }
                }
                out?
            };
            if is_missing {
                // 云端文件已被删除：跳过并记录，进度照常推进到 100%
                // 注意：进度完成数（done，含缺失跳过）与恢复成功数（restored）分开统计
                missing.push(rel.clone());
                let d = {
                    let mut s = shared.lock().unwrap();
                    s.0 += 1;
                    s.0
                };
                self.publish(
                    crate::eventbus::TaskStatus::Progress,
                    Some(crate::eventbus::TaskPhase::Transfer),
                    Some(rel.clone()),
                    d,
                    total,
                    restored_bytes,
                    total_bytes,
                    started.elapsed().as_millis() as u64,
                    meter.lock().unwrap().speed_bps(),
                    None,
                );
                continue;
            }
            let d = {
                let mut s = shared.lock().unwrap();
                s.0 += 1;
                s.1 += n;
                s.0
            };
            restored += 1;
            restored_bytes += n;
            self.publish(
                crate::eventbus::TaskStatus::Progress,
                Some(crate::eventbus::TaskPhase::Transfer),
                Some(rel.clone()),
                d,
                total,
                restored_bytes,
                total_bytes,
                started.elapsed().as_millis() as u64,
                meter.lock().unwrap().speed_bps(),
                None,
            );
            info!("已恢复: {} ({n} B)", rel);
        }

        // 有缺失文件时在完成事件中说明（任务面板显示提示，而不是永远「进行中」）
        let done_msg = if missing.is_empty() {
            None
        } else {
            Some(format!(
                "恢复完成；{} 个文件在云端已不存在，已跳过（可用「清理缺失记录」移除快照中的失效条目）",
                missing.len()
            ))
        };
        self.publish(
            crate::eventbus::TaskStatus::Completed,
            None,
            None,
            restored as u64,
            total,
            restored_bytes,
            total_bytes,
            started.elapsed().as_millis() as u64,
            meter.lock().unwrap().speed_bps(),
            done_msg,
        );

        Ok(RestoreSummary {
            restored,
            restored_bytes,
            missing,
        })
    }

    /// 发布进度事件（事件总线可选）
    #[allow(clippy::too_many_arguments)]
    fn publish(
        &self,
        status: crate::eventbus::TaskStatus,
        phase: Option<crate::eventbus::TaskPhase>,
        current_file: Option<String>,
        done: u64,
        total: u64,
        bytes_done: u64,
        bytes_total: u64,
        elapsed_ms: u64,
        speed: u64,
        message: Option<String>,
    ) {
        if let Some(eb) = &self.eventbus {
            eb.task_event(
                crate::eventbus::TaskKind::Restore,
                status,
                phase,
                "restore".to_string(),
                current_file,
                done,
                total,
                bytes_done,
                bytes_total,
                elapsed_ms,
                speed,
                message,
            );
        }
    }

    /// 恢复单个文件：目标读密文 → age 解密 → 写本地（按 1MiB 粒度上报进度）
    async fn restore_one(
        &self,
        rel: &str,
        restore_dir: &Path,
        progress: Option<crate::infra::storage_trait::ProgressCb>,
    ) -> Result<u64> {
        // 目标路径 = base_path + rel（统一带前导斜杠，如 "/fn-backup/Photos/xxx"）
        let base = self.base_path();
        let target_path = if base == "/" {
            PathBuf::from(rel)
        } else {
            PathBuf::from(format!("{}/{}", base, rel.trim_start_matches('/')))
        };

        // 1) 打开云端密文流（随后与解密并发推进，见第 2 步）
        //
        // 演进：整份进内存（2× 文件大小，必 OOM）→ 先落盘再读回（多 2 倍 IO）
        // → 现在 channel 直连：内存恒定「通道深度 × 块大小」，且不产生暂存文件。
        let file_started = Instant::now();
        let mut stream = self.target.read_stream(&target_path).await?;
        // 上次上报进度时的**文件内已用毫秒**（用于按时间节流，见下）
        //
        // `ProgressCb` 的 elapsed 契约是「本次请求从开始到此刻的**累计**毫秒数」
        // （`domain::pace::FileMeter` 靠相邻两次累计值算增量）。因此这里必须传累计值，
        // 但**不能每 1MiB 就传一次** —— 快盘上几十毫秒就攒够好几个 MiB，观测会密集到
        // 每两次之间的耗时远小于 `MIN_OBSERVE_MS`(50ms)，被计量器全部判为抖动丢掉，
        // 速度就恒为 0。故改为**按时间节流**：距上次上报 ≥100ms 才报一次。
        const MIN_REPORT_INTERVAL_MS: u64 = 100;
        // 2) 边下载边解密写本地：**消除「先落盘再读回」的双重 IO**
        //
        // 旧实现：下载全量密文 → 写暂存文件 → 重新打开读回 → 解密 → 写明文。
        // 同一份数据落盘两次、读一次；8GB 备份要多做 16GB 的读写。
        //
        // 现在用一条**有界 channel** 把「异步下载」与「同步解密」对接：
        // - 下载任务把密文块塞进 channel（容量小 ⇒ 天然背压，内存恒定）；
        // - 解密任务在 `spawn_blocking` 里用 `Read` 适配器从 channel 取数据，
        //   边解边写明文文件。
        // 峰值内存 = 通道深度 × 块大小（约几 MiB），且不再产生暂存密文文件。
        //
        // `rel` 来自云端目录列表（属于外部数据），必须挡住 `..` 穿越。
        let local_path = safe_join(restore_dir, rel)?;
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // 通道：`Result<Bytes, String>` 以便把下载错误传进解密任务的读取器
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, String>>(DL_CHANNEL_DEPTH);
        let progress_cb = progress.clone();
        let cb_for_dl = progress_cb;
        // 下载任务：逐块喂给 channel，并顺带报进度
        let dl = tokio::spawn(async move {
            let mut n: u64 = 0u64;
            let mut last_report: u64 = 0;
            let mut last_report_ms: u64 = 0;
            while let Some(item) = stream.next().await {
                let chunk = match item {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Err(format!("{e}"))).await;
                        return n;
                    }
                };
                n += chunk.len() as u64;
                if tx.send(Ok(chunk)).await.is_err() {
                    return n; // 解密侧已结束（出错/取消）
                }
                if let Some(cb) = &cb_for_dl {
                    let now_ms = file_started.elapsed().as_millis() as u64;
                    if n - last_report >= 256 * 1024
                        && now_ms.saturating_sub(last_report_ms) >= MIN_REPORT_INTERVAL_MS
                    {
                        last_report = n;
                        last_report_ms = now_ms;
                        cb(n, 0, now_ms);
                    }
                }
            }
            n
        });

        // 解密任务：从 channel 读（同步 Read 适配），边解边写明文
        let crypto = self.crypto.clone();
        let plain_path = local_path.clone();
        // 同目录临时文件：`rename` 在同一文件系统内是原子的，
        // 于是「要么是完整新文件，要么是原文件」——不存在中间态。
        let tmp_path = {
            let mut p = local_path.clone();
            let name = local_path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "restore".to_string());
            p.set_file_name(format!(".{name}.kzwr-restore-tmp"));
            p
        };
        let tmp_for_cleanup = tmp_path.clone();
        // 原始 mtime 要在闭包里用（在临时文件上回写）
        let orig_mtime_for_tmp = self.meta.get(rel).map(|(_, m)| *m).unwrap_or(0);
        let dec = tokio::task::spawn_blocking(move || -> Result<u64> {
            let reader = ChannelReader::new(rx);
            let mut rdr = std::io::BufReader::with_capacity(1024 * 1024, reader);
            // **绝不先删原文件**：写入中途失败（下载中断、解密出错、磁盘满）
            // 会让「原本还完好的文件」彻底消失 —— 恢复功能的底线是不能把人
            // 仅剩的数据也弄丢。这里统一先写同目录临时文件，成功后再原子替换。
            let w = std::fs::File::create(&tmp_path)?;
            let mut w = std::io::BufWriter::with_capacity(1024 * 1024, w);
            crypto.decrypt_stream(&mut rdr, &mut w)?;
            use std::io::Write;
            w.flush()?;
            w.get_ref().sync_all().ok();
            // 在**临时文件**上回写 mtime：它是我们自己新建的，属主即当前进程，
            // 因此 `futimens` 不会因「不是属主」被拒（EPERM）。
            // 顺序很关键 —— 先设时间戳再 rename，这样正式文件一出现就带着正确 mtime，
            // 不存在「先落地再改时间」的中间窗口（那窗口里若被扫描到会误判为已修改）。
            if orig_mtime_for_tmp > 0 {
                use std::io::Write as _;
                let when = std::time::UNIX_EPOCH
                    + std::time::Duration::from_secs(orig_mtime_for_tmp as u64);
                if let Err(e) = w
                    .get_ref()
                    .set_times(std::fs::FileTimes::new().set_modified(when))
                {
                    tracing::warn!(
                        err = %e,
                        "临时文件回写 mtime 失败（将由快照兜底，不会导致重复备份）: {}",
                        tmp_path.display()
                    );
                }
            }
            drop(w);
            // 原子替换：此刻才动用户的正式文件。
            // 若 rename 失败（跨设备/权限），临时文件保留下来便于排查，
            // 但**原文件始终没被删除**。
            if let Err(e) = std::fs::rename(&tmp_path, &plain_path) {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(e).map_err(|e| anyhow::anyhow!("替换目标文件失败: {e}"));
            }
            // 原子替换：此刻才动用户的正式文件。
            // 若 rename 失败（跨设备/权限），临时文件保留下来便于排查，
            // 但**原文件始终没被删除**。

            Ok(0)
        });

        // **RAII 清理守卫**：只要没走到「rename 成功」，离开作用域时就把临时文件删掉。
        //
        // 旧写法是「先 `dec.await?`、再 `dl.await?`，最后才显式 remove_file」，
        // 于是 `dl.await` 的 `?` 一旦提前返回（下载中断是最常见的失败），
        // 清理代码根本走不到，磁盘上留下 `.xxx.kzwr-restore-tmp` 残骸
        // （实测：一次失败的恢复在 /vol1/1000/test 里留下了 0 字节的
        //  `.mid-test.bin.kzwr-restore-tmp`）。用 guard 就不会漏。
        struct TmpCleanup {
            path: std::path::PathBuf,
            armed: bool,
        }
        impl TmpCleanup {
            /// rename 成功后调用：临时文件已改名为正式文件，不能再删
            fn disarm(&mut self) {
                self.armed = false;
            }
        }
        impl Drop for TmpCleanup {
            fn drop(&mut self) {
                if self.armed {
                    let _ = std::fs::remove_file(&self.path);
                }
            }
        }
        let mut tmp_guard = TmpCleanup {
            path: tmp_for_cleanup.clone(),
            armed: true,
        };

        // 两者并发推进：先取解密结果（它依赖下载完成），再取下载统计的字节数
        let dec_res = dec.await.map_err(|e| anyhow::anyhow!("解密任务异常终止: {e}"))?;
        let enc_len = dl.await.map_err(|e| anyhow::anyhow!("下载任务异常终止: {e}"))?;
        // 收尾补一次进度（保证最终落到全量）
        if let Some(cb) = &progress {
            cb(enc_len, 0, file_started.elapsed().as_millis() as u64);
        }
        if let Err(e) = dec_res {
            // 解密失败：guard 离开作用域时会清掉半成品，**原文件保持不变**
            return Err(e);
        }
        // 走到这里说明 rename 已成功，临时文件已变成正式文件，不能删
        tmp_guard.disarm();
        // 3) 回写原始 mtime（来自快照）：使下次增量备份判定为未变化，避免重复上传
        // mtime 已在上面的**临时文件**阶段回写完成（属主是本进程，不会 EPERM）。
        // 这里只判断结果，供快照决定记什么值。
        let orig_mtime = orig_mtime_for_tmp;
        let mtime_restored = orig_mtime > 0
            && std::fs::metadata(&local_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64 == orig_mtime)
                .unwrap_or(false);

        // 4) 把恢复结果写回快照。
        //
        // **必须分两种情况**，否则「恢复一次就多一份重复备份」：
        // - mtime 回写成功 ⇒ 磁盘 mtime == 原始 mtime，快照记**磁盘实际值**即可；
        // - mtime 回写失败 ⇒ 磁盘 mtime 是「恢复时刻」，而下次备份扫描到的仍是它，
        //   若这里也照抄磁盘值，快照与磁盘一致、下次确实不会重传；
        //   但一旦之后用户触碰过文件、或换个任务重扫，原始 mtime 与快照就会错位。
        //   更稳妥的是**直接记原始 mtime**：那是「内容对应的真实时间」，
        //   与源端不变时的扫描结果一致，从根上避免重复上传。
        if let Some(t) = &self.snapshot_target {
            let mtime_secs = if mtime_restored || orig_mtime <= 0 {
                // 回写成功：磁盘就是原始值；或本就没有原始值可用 ⇒ 取磁盘实际
                std::fs::metadata(&local_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            } else {
                // 回写失败：以快照里的原始 mtime 为准，别把「恢复时刻」写成预期值
                orig_mtime
            };
            // 明文大小以落盘结果为准（不再持有整份明文）
            let plain_len = std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(0);
            let entry = SnapshotEntry {
                rel_path: rel.to_string(),
                size: plain_len,
                mtime_secs,
                is_dir: false,
                digest: None,
            };
            match t.store.save_entry(&t.job_id, &t.account, &entry) {
                Ok(()) => info!(
                    "已回写快照: {} (size={}, mtime={})",
                    rel, entry.size, mtime_secs
                ),
                Err(e) => tracing::warn!(err = %e, "回写快照失败: {}", rel),
            }
        }

        // 返回明文大小（用于统计）：以落盘文件为准
        Ok(std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(enc_len))
    }

    /// 列出目标前缀下的所有文件（递归），返回相对路径（相对 base_path）
    async fn list_target_files(&self) -> Result<Vec<String>> {
        let base = self.base_path();

        let mut files = Vec::new();
        let mut stack = vec![base.clone()];
        while let Some(dir) = stack.pop() {
            let entries = self.target.list(&dir).await?;
            for e in entries {
                if e.is_dir {
                    stack.push(format!("/{}", e.rel_path.trim_start_matches('/')));
                } else {
                    files.push(strip_base(&e.rel_path, &base));
                }
            }
        }
        Ok(files)
    }
}

/// 从路径中去掉 base 前缀，得到相对路径
fn strip_base(path: &str, base: &str) -> String {
    let base = base.trim_matches('/');
    if base.is_empty() {
        return path.trim_start_matches('/').to_string();
    }
    let variants = [format!("/{}/", base), format!("{}/", base)];
    for v in &variants {
        if let Some(rest) = path.strip_prefix(v.as_str()) {
            return rest.to_string();
        }
    }
    path.trim_start_matches('/').to_string()
}

/// 作用域结束时清掉恢复用的暂存密文（成功/失败/panic 展开都执行）
///
/// 密文落盘是为了不撑爆内存，但必须在恢复完成后删掉 —— 否则恢复 10 个 8GB 文件
/// 就在暂存目录留 80GB 密文。
struct RestoreTmpGuard {
    /// 暂存的密文文件（**始终删除**）
    file: std::path::PathBuf,
    /// 回退路径下自建的临时目录（`TempDir` 必须持有到此处，否则一创建就被删）
    dir: Option<tempfile::TempDir>,
}

impl Drop for RestoreTmpGuard {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.file) {
            tracing::debug!(path = %self.file.display(), err = %e, "清理暂存密文失败");
        }
        // TempDir 自身的 Drop 会连目录一起删掉
        drop(self.dir.take());
    }
}

/// 把来自云端的 `rel` 安全地拼到 `base` 下（**拒绝逃出 base**）
///
/// 拒绝条件与 `infra::source::local::resolve` 一致：绝对路径、`..` 段。
/// `rel` 是外部数据（云端目录列表），不能假设它守规矩。
fn safe_join(base: &Path, rel: &str) -> Result<PathBuf> {
    let rel = rel.trim_start_matches('/');
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        anyhow::bail!("恢复路径不允许为绝对路径: {rel}");
    }
    for comp in rel_path.components() {
        if matches!(comp, std::path::Component::ParentDir) {
            anyhow::bail!("恢复路径不允许跨越上级目录（..）: {rel}");
        }
    }
    Ok(base.join(rel))
}

/// 恢复结果摘要
#[derive(Debug, Default)]
pub struct RestoreSummary {
    pub restored: usize,
    pub restored_bytes: u64,
    /// 云端已不存在而被跳过的文件（快照中仍有记录）
    pub missing: Vec<String>,
}

#[cfg(test)]
mod restore_speed_tests {
    use super::*;
    use crate::domain::crypto::AgeKeys;
    use crate::infra::storage_trait::TargetStorage;
    use crate::infra::target::mock_dav::MockDav;
    use crate::plugin::api::TargetPlugin;
    use std::sync::Arc;

    /// 用生产装配路径构造一个指向 mock 的 WebDAV 目标
    fn build_target(
        addr: std::net::SocketAddr,
    ) -> (tempfile::TempDir, Arc<dyn TargetStorage>) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = crate::infra::config::ConfigManager::new(
            dir.path(),
            age::secrecy::SecretString::from("t".to_string()),
        );
        let t = crate::infra::config::TargetConfig {
            id: "t".into(),
            name: "m".into(),
            kind: "webdav".into(),
            url: Some(format!("http://{addr}/dav")),
            username_enc: Some(mgr.encrypt_field("u").unwrap()),
            password_enc: Some(mgr.encrypt_field("p").unwrap()),
            enabled: true,
            parallel: Some(1),
            fields: Default::default(),
            use_proxy: false,
        };
        let plugin = crate::plugin::builtin::webdav_abi::WebdavAbiPlugin::new();
        let (storage, _) = plugin.build(&t, &mgr).unwrap();
        (dir, storage)
    }

    /// **流式恢复的大文件正确性**：多块 + 跨 channel 边界，明文必须逐字节一致
    ///
    /// 恢复从「落盘再读回」改成「channel 边下边解」后，最容易出错的正是
    /// 块边界拼接（`ChannelReader` 的 `cur` 残余处理）。用**非对齐长度**
    /// （刻意不是块大小整数倍）验证不会丢字节或多字节。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn streamed_restore_preserves_bytes_across_chunk_boundaries() {
        let mock = MockDav::start_with(false, std::time::Duration::ZERO).await;
        let (_dir, target) = build_target(mock.addr);

        let keys = AgeKeys::generate();
        let crypto = crate::domain::crypto::CryptoSession::full(&keys);
        // 刻意取非对齐长度 + 多块（约 3MB，跨多个下载块）
        let plain: Vec<u8> = (0..3 * 1024 * 1024 + 12345)
            .map(|i| ((i * 17 + 3) % 251) as u8)
            .collect();
        let mut enc = Vec::new();
        crypto
            .encrypt_stream(&mut std::io::Cursor::new(&plain), &mut enc)
            .expect("加密应成功");
        let enc_len = enc.len() as u64;

        let noop: crate::infra::storage_trait::ProgressCb = Arc::new(|_, _, _| {});
        target
            .write_stream_sized(
                std::path::Path::new("fn-backup/big.age"),
                Box::new(futures::stream::iter(vec![bytes::Bytes::from(enc)])),
                enc_len,
                noop,
            )
            .await
            .expect("密文写入应成功");

        let out = tempfile::tempdir().unwrap();
        let mut meta = std::collections::HashMap::new();
        meta.insert("big.age".to_string(), (plain.len() as u64, 0i64));
        let job = RestoreJob {
            target,
            crypto,
            target_prefix: Some("fn-backup".to_string()),
            source_root_name: None,
            eventbus: None,
            meta,
            snapshot_target: None,
            staging_dir: Some(out.path().to_path_buf()),
            cancel: None,
        };
        job.run(&["big.age".to_string()], out.path())
            .await
            .expect("恢复应成功");

        let restored = std::fs::read(out.path().join("big.age")).expect("应能读到文件");
        assert_eq!(restored.len(), plain.len(), "长度必须一致");
        assert!(restored == plain, "内容必须逐字节一致（块边界拼接正确）");
    }

    /// **恢复失败绝不能毁掉原文件**
    ///
    /// 这是恢复功能的底线：用户触发恢复，往往正是因为本地文件已经丢了；
    /// 若「覆盖写」先截断/删除原文件、随后下载或解密失败，就会把用户
    /// **仅剩的那份数据也弄没**。
    ///
    /// 曾试过「先删原文件再新建」来拿到 inode 属主（为了让 mtime 可写），
    /// 但那会让失败路径直接丢数据 —— 本测试锁死这条底线：
    /// 现在实现改为「写同目录临时文件 → 成功后原子 rename」，
    /// 失败时原文件必须**逐字节不变**，且不残留临时文件。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn restore_failure_keeps_original_file_intact() {
        let mock = MockDav::start(false).await;
        let (_dir, target) = build_target(mock.addr);

        let keys = AgeKeys::generate();
        let crypto = crate::domain::crypto::CryptoSession::full(&keys);
        let plain: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
        let mut enc = Vec::new();
        crypto
            .encrypt_stream(&mut std::io::Cursor::new(&plain), &mut enc)
            .expect("加密应成功");
        let enc_len = enc.len() as u64;
        let noop: crate::infra::storage_trait::ProgressCb = Arc::new(|_, _, _| {});
        target
            .write_stream_sized(
                std::path::Path::new("fn-backup/keep.age"),
                Box::new(futures::stream::iter(vec![bytes::Bytes::from(enc)])),
                enc_len,
                noop,
            )
            .await
            .expect("密文写入应成功");

        // 原文件：内容与云端版本**不同**，用来判断它有没有被就地破坏
        let out = tempfile::tempdir().unwrap();
        let original: Vec<u8> = b"USER-ONLY-COPY-must-not-be-destroyed".repeat(64);
        std::fs::write(out.path().join("keep.age"), &original).expect("预置原文件");

        // 让下载中途中断：GET 只发一半就断开
        mock.truncate_downloads
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let mut meta = std::collections::HashMap::new();
        meta.insert("keep.age".to_string(), (plain.len() as u64, 0i64));
        let job = RestoreJob {
            target,
            crypto,
            target_prefix: Some("fn-backup".to_string()),
            source_root_name: None,
            eventbus: None,
            meta,
            snapshot_target: None,
            staging_dir: Some(out.path().to_path_buf()),
            cancel: None,
        };
        let r = job.run(&["keep.age".to_string()], out.path()).await;
        assert!(r.is_err(), "下载中断时恢复必须报错，而不是静默成功");

        // 核心断言：原文件完好
        let after = std::fs::read(out.path().join("keep.age")).expect("原文件必须仍然存在");
        assert_eq!(
            after, original,
            "恢复失败后原文件必须逐字节不变（否则用户仅剩的数据会被毁掉）"
        );

        // 不能留下半成品临时文件
        let leftovers: Vec<String> = std::fs::read_dir(out.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("kzwr-restore-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "失败后不应残留临时文件：{leftovers:?}");
    }

    /// **云端缺失的文件必须被跳过（记入 missing），不能让整单恢复失败**
    ///
    /// 实测：12 个文件里 10 个已成功恢复，仅因快照里残留了一条「云端已不存在」
    /// 的孤岛条目，整次恢复被判失败，界面上写着「已失败」——
    /// 而用户真正关心的是那 10 个已恢复的文件。
    ///
    /// 插件把「路径不存在」包装成了 `Protocol("下载中断: ...路径不存在...")`，
    /// NotFound 语义在那一层丢了，所以判定必须同时看**错误消息**。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn missing_remote_file_is_skipped_not_fatal() {
        let mock = MockDav::start(false).await;
        let (_dir, target) = build_target(mock.addr);

        let keys = AgeKeys::generate();
        let crypto = crate::domain::crypto::CryptoSession::full(&keys);
        // 只把 a.age 真正放到云端；b.age 是快照里的「孤岛条目」
        let plain: Vec<u8> = vec![7u8; 4096];
        let mut enc = Vec::new();
        crypto
            .encrypt_stream(&mut std::io::Cursor::new(&plain), &mut enc)
            .expect("加密应成功");
        let enc_len = enc.len() as u64;
        let noop: crate::infra::storage_trait::ProgressCb = Arc::new(|_, _, _| {});
        target
            .write_stream_sized(
                std::path::Path::new("fn-backup/a.age"),
                Box::new(futures::stream::iter(vec![bytes::Bytes::from(enc)])),
                enc_len,
                noop,
            )
            .await
            .expect("密文写入应成功");

        let out = tempfile::tempdir().unwrap();
        let mut meta = std::collections::HashMap::new();
        meta.insert("a.age".to_string(), (plain.len() as u64, 0i64));
        meta.insert("b.age".to_string(), (100u64, 0i64)); // 云端不存在
        let job = RestoreJob {
            target,
            crypto,
            target_prefix: Some("fn-backup".to_string()),
            source_root_name: None,
            eventbus: None,
            meta,
            snapshot_target: None,
            staging_dir: Some(out.path().to_path_buf()),
            cancel: None,
        };

        let r = job
            .run(&["a.age".to_string(), "b.age".to_string()], out.path())
            .await;

        let summary = r.expect("缺失文件应被跳过，整单仍算成功");
        assert_eq!(summary.restored, 1, "存在的那个文件应恢复成功");
        assert!(
            summary.missing.contains(&"b.age".to_string()),
            "缺失文件应记入 missing，实际：{:?}",
            summary.missing
        );
        // a.age 内容正确
        let got = std::fs::read(out.path().join("a.age")).expect("a.age 应存在");
        assert_eq!(got, plain);
        // b.age 不应被创建成空文件
        assert!(
            !out.path().join("b.age").exists(),
            "缺失文件不应留下空文件"
        );
    }

    /// **恢复速度回归**：恢复过程中必须能算出正的速率，且中途就有观测。
    ///
    /// 线上症状与备份侧同源：速度恒显示「—」。根因是
    /// ① 插件 `read_begin` 把整份文件读完才返回 ⇒ 所有 data 一次性涌出；
    /// ② 传给计量器的 elapsed 是累计值而非增量 ⇒ 耗时增量恒为 0 被阈值丢弃。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn restore_reports_positive_speed() {
        let mock = MockDav::start_with(false, std::time::Duration::from_millis(1)).await;
        let (_dir, target) = build_target(mock.addr);

        // 1) 按真实加密格式造一份密文并写到 mock（走 PUT 落库）
        let keys = AgeKeys::generate();
        let crypto = crate::domain::crypto::CryptoSession::full(&keys);
        let plain: Vec<u8> = (0..6 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let mut enc = Vec::new();
        crypto
            .encrypt_stream(&mut std::io::Cursor::new(&plain), &mut enc)
            .expect("加密应成功");
        let enc_len = enc.len() as u64;
        let noop: crate::infra::storage_trait::ProgressCb = Arc::new(|_, _, _| {});
        target
            .write_stream_sized(
                std::path::Path::new("fn-backup/Photos/pic.age"),
                Box::new(futures::stream::iter(vec![bytes::Bytes::from(enc)])),
                enc_len,
                noop,
            )
            .await
            .expect("密文写入应成功");

        // 2) 走完整恢复流程，并订阅事件总线抓真实上报的速度
        let bus = Arc::new(crate::eventbus::EventBus::new());
        let mut sub = bus.subscribe();
        let speeds = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
        let speeds_cb = speeds.clone();
        let collector = tokio::spawn(async move {
            while let Ok(ev) = sub.recv().await {
                if matches!(ev.kind, crate::eventbus::TaskKind::Restore)
                    && matches!(ev.status, crate::eventbus::TaskStatus::Progress)
                {
                    speeds_cb.lock().unwrap().push(ev.speed);
                }
            }
        });

        let out = tempfile::tempdir().unwrap();
        let mut meta = std::collections::HashMap::new();
        meta.insert("Photos/pic.age".to_string(), (plain.len() as u64, 0i64));
        let job = RestoreJob {
            target,
            crypto,
            target_prefix: Some("fn-backup".to_string()),
            source_root_name: None,
            eventbus: Some(bus),
            meta,
            snapshot_target: None,
            staging_dir: Some(out.path().to_path_buf()),
            cancel: None,
        };
        let files = vec!["Photos/pic.age".to_string()];
        let summary = job.run(&files, out.path()).await.expect("恢复应成功");
        assert!(summary.restored_bytes > 0, "应有恢复字节");

        // 3) 速度断言：必须出现过正速率（否则前端恢复时速度恒显示 —）
        collector.abort();
        let seen = speeds.lock().unwrap().clone();
        let positive: Vec<u64> = seen.iter().copied().filter(|s| *s > 0).collect();
        assert!(
            !positive.is_empty(),
            "恢复过程中必须出现过正的速率，实际观测：{seen:?}"
        );
        eprintln!("[restore-speed] 观测 {} 次，其中正速率 {} 次，峰值 {} B/s",
            seen.len(), positive.len(), positive.iter().copied().max().unwrap_or(0));

        // 4) 校验明文还原正确
        let restored = std::fs::read(out.path().join("Photos/pic.age")).expect("应能读到恢复文件");
        assert_eq!(restored, plain, "恢复出的明文应与原始一致");
    }
}
