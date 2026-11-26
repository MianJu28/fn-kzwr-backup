//! **每插件专属执行线程** —— 沙箱的载体
//!
//! ## 为什么需要「专属线程」而不是用 `spawn_blocking`
//! Landlock 的两条实测性质（`docs/memory/dev/PLUGIN_ISOLATION.md` §2）决定了这件事：
//!
//! 1. **per-thread**：多线程进程必须在每个要受限的线程上分别施加；
//! 2. **不可逆**：收紧后无法放宽。
//!
//! 而 tokio 的 `spawn_blocking` 是**共享线程池**（线程复用）。若在池线程上施加
//! Landlock，该线程会被**永久污染** —— 之后宿主的其它阻塞任务
//! （配置读写、快照操作、日志落盘）只要复用到这个线程就会莫名失败。
//! 这类故障极难定位（表现为随机 I/O 错误）。
//!
//! ⇒ 因此每个插件开**一条专属线程**：它的**第一次**使用就施加沙箱，
//! 之后该插件的所有回调都只跑在这条线程上，**永不归还给公共池**。
//!
//! ## 为什么是「每插件一条」而不是「每插件一个池」
//! 插件回调是**同步阻塞**的（会 `block_on` 自己的 runtime 发 HTTP）。
//! 同一插件的两个回调并发执行并没好处，反而让沙箱状态更难推理
//! （虽然 Landlock 是 per-thread，多线程也能各自施加，但没必要）。
//! 一条线程 + 一个队列 = 语义最简单，且天然串行化，与插件「无回调」模型一致。
//!
//! ## 超时与卡死
//! 任务用 `std::sync::mpsc` 投递，调用方 `await` 一个 `oneshot`。
//! **卡死的插件会让该线程永久占用**（同进程模型固有限制：无法强杀）——
//! 这与改造前的行为一致（原先卡死的是池线程），但**影响面更小**：
//! 只污染该插件自己的线程，不再消耗共享池。

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};

use super::sandbox::SandboxPolicy;

/// 投递给专属线程的任务：在**沙箱线程内**执行，结果经 oneshot 回传
type Job = Box<dyn FnOnce() + Send + 'static>;

/// 单个插件的专属线程句柄
struct Worker {
    tx: mpsc::Sender<Job>,
    /// 该线程是否已成功施加沙箱（诊断用）
    ///
    /// 用 `Arc<AtomicBool>` **共享**线程内部的标志，而不是在 spawn 后立即读一份快照：
    /// 线程这时往往还没被调度，`flag.store(true)` 尚未执行，读到的必然是 `false`，
    /// 且此后永不更新 —— 结果是诊断接口恒报「未受沙箱保护」，排障时被严重误导。
    sandboxed: Arc<std::sync::atomic::AtomicBool>,
}

/// 全部插件的专属线程（进程级单例；插件只增不减，与 ctx 同理）
static WORKERS: OnceLock<Mutex<HashMap<String, Arc<Worker>>>> = OnceLock::new();

fn workers() -> &'static Mutex<HashMap<String, Arc<Worker>>> {
    WORKERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取得（或创建）某插件的专属线程并在其上执行 `f`，异步等待结果
///
/// `policy` 只在该线程**首次**创建时使用（Landlock 不可逆，之后无法改）。
/// `f` 会在线程内同步执行 —— 因此它可以安全地 `block_on`。
///
/// 返回 `None` 表示专属线程已不可用（线程 panic 退出/通道关闭），
/// 调用方应退化为「跳过本次调用」而不是让宿主崩。
pub async fn run_on_plugin_thread<F, T>(plugin_id: &str, policy: SandboxPolicy, f: F) -> Option<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let worker = get_or_spawn(plugin_id, policy)?;
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<T>();
    // 包一层 catch_unwind：插件 panic 绝不能掀翻这条线程（否则该插件永久失效）
    let job: Job = Box::new(move || {
        let out = std::panic::catch_unwind(AssertUnwindSafe(f));
        match out {
            Ok(v) => {
                let _ = done_tx.send(v);
            }
            Err(_) => {
                tracing::error!("插件回调 panic（已兜住，专属线程存活）");
                // 不发送 ⇒ 调用方看到 RecvError，按「本次调用失败」处理
            }
        }
    });
    if worker.tx.send(job).is_err() {
        tracing::warn!(plugin = %plugin_id, "插件专属线程已退出，本次调用跳过");
        return None;
    }
    done_rx.await.ok()
}

/// **同步**版本：在专属线程上执行并**阻塞等待**结果
///
/// 用于本来就在同步上下文的调用点（如 `host_bind` 在启动时下发能力表）。
/// 避免为此把整条调用链改成 async（那会让 axum handler 的 future 突然要求 `Send`，
/// 波及一片）。
///
/// ⚠️ 不要在 tokio worker 线程上跑长任务 —— 它会阻塞该 worker。
/// `host_bind` 只做初始化（存表 + 建 runtime），耗时可控。
pub fn run_on_plugin_thread_blocking<F, T>(plugin_id: &str, policy: SandboxPolicy, f: F) -> Option<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let worker = get_or_spawn(plugin_id, policy)?;
    let (done_tx, done_rx) = mpsc::channel::<T>();
    let job: Job = Box::new(move || {
        // 与异步版一致：panic 兜住，不让线程死掉
        match std::panic::catch_unwind(AssertUnwindSafe(f)) {
            Ok(v) => {
                let _ = done_tx.send(v);
            }
            Err(_) => {
                tracing::error!("插件回调 panic（已兜住，专属线程存活）");
            }
        }
    });
    if worker.tx.send(job).is_err() {
        tracing::warn!(plugin = %plugin_id, "插件专属线程已退出，本次调用跳过");
        return None;
    }
    done_rx.recv().ok()
}

/// 该插件的专属线程是否已施加沙箱（诊断用）
pub fn is_sandboxed(plugin_id: &str) -> Option<bool> {
    workers()
        .lock()
        .ok()?
        .get(plugin_id)
        .map(|w| w.sandboxed.load(std::sync::atomic::Ordering::Relaxed))
}

/// 回收某个插件的专属线程（插件卸载/重装时调用）
///
/// `WORKERS` 是只增不减的进程级表：插件被卸载、或换 id 重装后，
/// 旧线程会**永远**活着（每条占一个 kernel 线程 + 一份 Landlock 规则集），
/// 一直空转在 `rx.recv()` 上直到进程退出。插件生态下频繁装卸会累积几十条僵尸线程。
///
/// 移除表项后：若没有其它 `Arc<Worker>` 在飞，`tx` 随之 drop ⇒ 线程 `recv` 返回
/// `Err` ⇒ 循环退出 ⇒ 线程结束。在飞的调用会自然结束，不会被强杀。
pub fn forget(plugin_id: &str) {
    if let Ok(mut m) = workers().lock() {
        if let Some(w) = m.remove(plugin_id) {
            tracing::debug!(plugin = %plugin_id, "回收插件专属线程");
            drop(w);
        }
    }
}

/// 已建专属线程的插件 id 列表（诊断用）
pub fn sandboxed_plugins() -> Vec<(String, bool)> {
    let mut v: Vec<(String, bool)> = workers()
        .lock()
        .map(|m| {
            m.iter()
                .map(|(k, w)| {
                    (k.clone(), w.sandboxed.load(std::sync::atomic::Ordering::Relaxed))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn get_or_spawn(plugin_id: &str, policy: SandboxPolicy) -> Option<Arc<Worker>> {
    let mut map = workers().lock().ok()?;
    if let Some(w) = map.get(plugin_id) {
        return Some(w.clone());
    }
    let (tx, rx) = mpsc::channel::<Job>();
    let id = plugin_id.to_string();
    let sandboxed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = sandboxed.clone();

    let spawned = std::thread::Builder::new()
        .name(format!("plugin-{id}"))
        .spawn(move || {
            // **第一件事**就是施加沙箱：必须在跑任何插件代码之前
            //（Landlock 对已加载的库无影响，但插件可能读文件，越早越好）
            if disabled_by_env() {
                tracing::warn!(
                    plugin = %id,
                    "FN_KZWR_NO_SANDBOX 已设置 ⇒ 该插件**未受沙箱保护**（仅用于排障）"
                );
                while let Ok(job) = rx.recv() {
                    job();
                }
                return;
            }
            let ok = match policy.apply_to_current_thread() {
                Ok(()) => {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    tracing::info!(plugin = %id, "插件专属线程已施加 Landlock 沙箱");
                    true
                }
                Err(e) => {
                    // 降级而非阻断：内核不支持/规则失败时插件照常跑，但明确记录未受保护
                    tracing::warn!(
                        plugin = %id, err = %e,
                        "Landlock 沙箱施加失败，该插件**未受沙箱保护**（功能不受影响）"
                    );
                    false
                }
            };
            let _ = ok;
            // 任务循环：直到所有发送端（Worker）被 drop
            while let Ok(job) = rx.recv() {
                job();
            }
            tracing::debug!(plugin = %id, "插件专属线程退出");
        });

    match spawned {
        Ok(_handle) => {
            // 注意：**不保存 JoinHandle**。线程的生命周期由通道决定
            // （所有 Worker Arc 释放 ⇒ tx 关闭 ⇒ 循环退出）。
            // 保存 handle 也没用：我们从不 join（进程退出时随之一并结束）。
            // 共享同一个 `sandboxed` 原子标志：线程真正施加沙箱后会置 true，
            // 诊断接口因此能反映**当前真实状态**（而不是 spawn 瞬间的快照）
            let w = Arc::new(Worker { tx, sandboxed });
            map.insert(plugin_id.to_string(), w.clone());
            Some(w)
        }
        Err(e) => {
            tracing::error!(plugin = %plugin_id, err = %e, "创建插件专属线程失败");
            None
        }
    }
}

/// 插件私有目录的根（`$TRIM_PKGVAR/plugins`），启动时注入一次
///
/// 用进程级 `OnceLock` 而不是到处传 `var_dir`：插件调用点在 `cabi.rs` /
/// `target_abi.rs` 的深层 FFI 包装里，逐层传参噪音太大；这与
/// [`crate::plugin::crypto`] 注入口令是同一做法。
static PLUGINS_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// 注入插件私有目录的根（`main.rs` 启动时调用一次）
pub fn init(plugins_root: PathBuf) {
    let _ = PLUGINS_ROOT.set(plugins_root);
}

/// 为插件构建沙箱策略：私有数据目录 + 额外的可读写路径
///
/// `extra` 由调用方提供（如目标实例需要访问的目录）。
/// 未注入根目录时退化为「只允许插件 id 同名目录」——不应发生（`main.rs` 会注入）。
pub fn policy_for(plugin_id: &str, extra: &[PathBuf]) -> SandboxPolicy {
    let own = match PLUGINS_ROOT.get() {
        Some(root) => root.join(plugin_id),
        None => PathBuf::from("/nonexistent-uninitialized"),
    };
    SandboxPolicy::for_plugin(plugin_id, &own, extra)
}

/// 逃生舱：`FN_KZWR_NO_SANDBOX=1` 时不施加沙箱（**仅排障用**）
///
/// 与其它 `FN_KZWR_*` 逃生舱同一风格。存在的理由：沙箱一旦在**真实 NAS** 上
/// 误伤某个插件的合法 I/O（例如插件需要访问白名单外的路径），
/// 用户必须能立刻恢复可用，而不是等我们发新版。
/// 启用时**明确记警告**，便于事后定位。
pub fn disabled_by_env() -> bool {
    std::env::var("FN_KZWR_NO_SANDBOX")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 专属线程能执行闭包并把结果带回（异步等待）
    #[tokio::test]
    async fn runs_job_on_dedicated_thread_and_returns_value() {
        let p = SandboxPolicy { paths: vec![], net_ports: vec![], restrict_net: false };
        let out = run_on_plugin_thread("t-basic", p, || 6 * 7).await;
        assert_eq!(out, Some(42));
    }

    /// **同一插件复用同一条线程**（这是沙箱安全的前提：
    /// 若每次新建线程，Landlock 的"不可逆"就失去了意义，且线程会无限增长）
    #[tokio::test]
    async fn reuses_same_thread_for_same_plugin() {
        let p = SandboxPolicy { paths: vec![], net_ports: vec![], restrict_net: false };
        let a = run_on_plugin_thread("t-reuse", p.clone(), || {
            std::thread::current().id()
        })
        .await
        .unwrap();
        let b = run_on_plugin_thread("t-reuse", p, || std::thread::current().id())
            .await
            .unwrap();
        assert_eq!(a, b, "同一插件的两次调用必须落在同一条线程上");
    }

    /// 不同插件用**不同**线程（隔离的前提）
    #[tokio::test]
    async fn different_plugins_get_different_threads() {
        let p = SandboxPolicy { paths: vec![], net_ports: vec![], restrict_net: false };
        let a = run_on_plugin_thread("t-iso-a", p.clone(), || std::thread::current().id())
            .await
            .unwrap();
        let b = run_on_plugin_thread("t-iso-b", p, || std::thread::current().id())
            .await
            .unwrap();
        assert_ne!(a, b, "不同插件必须各自独占线程");
    }

    /// 插件 panic 不能掀翻专属线程：后续调用仍能成功
    #[tokio::test]
    async fn plugin_panic_does_not_kill_worker() {
        let p = SandboxPolicy { paths: vec![], net_ports: vec![], restrict_net: false };
        let bad = run_on_plugin_thread("t-panic", p.clone(), || -> i32 {
            panic!("插件内部 panic");
        })
        .await;
        assert!(bad.is_none(), "panic 的那次调用应返回 None");
        // 线程仍活着
        let good = run_on_plugin_thread("t-panic", p, || 1).await;
        assert_eq!(good, Some(1), "panic 后专属线程应仍可用");
    }

    /// 记录了沙箱状态（诊断可查；本测试不依赖内核是否支持 Landlock）
    #[tokio::test]
    async fn records_sandbox_status() {
        let p = SandboxPolicy { paths: vec![], net_ports: vec![], restrict_net: false };
        let _ = run_on_plugin_thread("t-status", p, || ()).await;
        assert!(
            is_sandboxed("t-status").is_some(),
            "应能在诊断接口里查到该插件的沙箱状态"
        );
    }
}


// ── 目标实例的沙箱线程（**每实例一个小池**）──────────────────────────────
//
// 为什么不是「每插件一条」：并发回传（ADR-013 决策 4）会**同时**调用同一个
// 目标实例的 `write_stream`（`buffer_unordered(parallel)`，最多 `MAX_PARALLEL=8`）。
// 若这些调用挤在一条线程上，`write_blocking` 的阻塞喂块会被**串行化** ——
// 并发回传直接失效（性能回退，且与用户配置的并发度不符）。
//
// ⇒ 每个**目标实例**建一个大小 = 并发度的小池，池内每条线程各自施加沙箱。
// 池随实例存在（实例由 `Arc<AbiTargetStorage>` 保活），实例销毁即池销毁。

/// 目标实例的沙箱线程池
pub struct InstancePool {
    tx: crossbeam_like::Sender,
    size: usize,
}

/// 一个极简的多生产者/多消费者通道（避免为此引入 crossbeam 依赖）
///
/// 实现：`Mutex<VecDeque>` + `Condvar`。分配一次、长期复用，
/// 开销远小于每次 `spawn_blocking`。
mod crossbeam_like {
    use std::collections::VecDeque;
    use std::sync::{Condvar, Mutex};

    pub struct Sender {
        inner: std::sync::Arc<Inner>,
    }
    struct Inner {
        q: Mutex<VecDeque<super::Job>>,
        cv: Condvar,
        closed: Mutex<bool>,
    }

    impl Sender {
        pub fn send(&self, job: super::Job) -> Result<(), ()> {
            // ⚠️ **必须在持锁状态下 notify**。
            //
            // 曾经是「先 unlock 再 notify_one」，那是经典的 Condvar 丢失唤醒：
            // 消费者可能刚 `pop_front()` 拿到 None、还没进入 `wait()`，此时生产者的
            // notify 到达 —— 由于消费者尚未 wait，这次通知被丢弃；随后消费者进入
            // `wait()` 永久阻塞，尽管队列里已经有任务了。
            // 表现：备份任务无声卡死，日志停在「已提交待传清单」之后。
            let mut q = self.inner.q.lock().unwrap();
            q.push_back(job);
            self.inner.cv.notify_one();
            Ok(())
        }
        pub fn close(&self) {
            *self.inner.closed.lock().unwrap() = true;
            // 同理：`notify_all` 在 closed 锁内（顺序无关，但保持一致性）
            self.inner.cv.notify_all();
        }
    }

    pub fn channel() -> (Sender, Receiver) {
        let inner = std::sync::Arc::new(Inner {
            q: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            closed: Mutex::new(false),
        });
        (
            Sender { inner: inner.clone() },
            Receiver { inner },
        )
    }

    pub struct Receiver {
        inner: std::sync::Arc<Inner>,
    }
    impl Receiver {
        /// 取一个任务；`None` = 已关闭且队列已空
        pub fn recv(&self) -> Option<super::Job> {
            let mut q = self.inner.q.lock().unwrap();
            loop {
                if let Some(job) = q.pop_front() {
                    return Some(job);
                }
                if *self.inner.closed.lock().unwrap() {
                    return None;
                }
                q = self.inner.cv.wait(q).unwrap();
            }
        }
    }
}

impl InstancePool {
    /// 建立一个 `size` 条线程的池，每条线程各自施加 `policy` 沙箱
    ///
    /// `size` 由调用方按该实例的并发度给出（至少 1）。
    pub fn new(label: &str, policy: SandboxPolicy, size: usize) -> Option<Arc<Self>> {
        let size = size.max(1);
        let label = label.to_string();
        let (tx, rx) = crossbeam_like::channel();
        let rx = Arc::new(rx);
        let mut spawned = 0usize;
        for i in 0..size {
            let rx = rx.clone();
            let policy = policy.clone();
            let name = format!("target-{label}-{i}");
            let label = label.clone();
            let ok = std::thread::Builder::new()
                .name(name)
                .spawn(move || {
                    // 每条线程**各自**施加沙箱（Landlock per-thread）
                    if let Err(e) = policy.apply_to_current_thread() {
                        tracing::warn!(
                            target = %label, idx = i, err = %e,
                            "目标实例沙箱线程施加 Landlock 失败（该线程未受保护）"
                        );
                    }
                    while let Some(job) = rx.recv() {
                        job();
                    }
                })
                .is_ok();
            if ok {
                spawned += 1;
            }
        }
        if spawned == 0 {
            return None;
        }
        tracing::info!(target = %label, threads = spawned, "目标实例沙箱线程池已就绪");
        Some(Arc::new(Self { tx, size: spawned }))
    }

    /// 池大小（诊断用）
    pub fn size(&self) -> usize {
        self.size
    }

    /// 投递任务（不等待）
    pub fn submit(&self, job: Job) -> bool {
        self.tx.send(job).is_ok()
    }

    /// 当前在跑的线程数（诊断用；`None` = 池已关闭）
    pub fn shutdown(&self) {
        self.tx.close();
    }
}

impl Drop for InstancePool {
    fn drop(&mut self) {
        // 唤醒所有等待的线程，让它们看到 closed 后退出
        self.tx.close();
    }
}

#[cfg(test)]
mod condvar_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AOrd};
    use std::sync::Arc as StdArc;

    /// **丢失唤醒回归**：生产者「解锁后再 notify」时，消费者可能错过通知而永久阻塞。
    ///
    /// 本测试用高并发反复压测 `send`/`recv`：修复前（notify 在锁外）在压力下
    /// 有一定概率卡住；修复后（持锁 notify）必须全部跑完。
    #[test]
    fn high_contention_send_recv_never_deadlocks() {
        let (tx, rx) = crossbeam_like::channel();
        let rx = StdArc::new(rx);
        let done = StdArc::new(AtomicUsize::new(0));
        const N: usize = 5000;

        // 一个消费者线程：直到全部收到
        let rx2 = rx.clone();
        let done2 = done.clone();
        let consumer = std::thread::spawn(move || {
            while done2.load(AOrd::Relaxed) < N {
                if rx2.recv().is_none() {
                    break;
                }
                done2.fetch_add(1, AOrd::Relaxed);
            }
        });

        // 生产者在另一个线程里连续投递
        let producer = std::thread::spawn(move || {
            for _ in 0..N {
                let _ = tx.send(Box::new(|| {}));
            }
        });

        producer.join().expect("生产者不应 panic");
        // 给消费者一个合理的完成窗口；卡住即视为丢失唤醒
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while done.load(AOrd::Relaxed) < N && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let got = done.load(AOrd::Relaxed);
        // 关闭通道让消费者退出（若它卡住，join 会失败）
        drop(consumer);
        assert_eq!(got, N, "只收到 {got}/{N}，疑似丢失唤醒导致消费者永久阻塞");
    }
}
