//! **每插件专属执行线程** —— 沙箱的载体
//!
//! ## 为什么需要「专属线程」而不是用 `spawn_blocking`
//! Landlock 的两条实测性质（`docs/PLUGIN_ISOLATION.md` §2）决定了这件事：
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
    sandboxed: bool,
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

/// 该插件的专属线程是否已施加沙箱（诊断用）
pub fn is_sandboxed(plugin_id: &str) -> Option<bool> {
    workers()
        .lock()
        .ok()?
        .get(plugin_id)
        .map(|w| w.sandboxed)
}

/// 已建专属线程的插件 id 列表（诊断用）
pub fn sandboxed_plugins() -> Vec<(String, bool)> {
    let mut v: Vec<(String, bool)> = workers()
        .lock()
        .map(|m| m.iter().map(|(k, w)| (k.clone(), w.sandboxed)).collect())
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
            let w = Arc::new(Worker {
                tx,
                sandboxed: sandboxed.load(std::sync::atomic::Ordering::Relaxed),
            });
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
    SandboxPolicy::for_plugin(&own, extra)
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
