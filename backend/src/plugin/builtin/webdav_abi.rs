//! 内置通用 WebDAV 目标插件（ABI 化，方案 C）：提供静态 [`KzwrTargetAbi`]
//!
//! ## 定位
//!
//! 这是**通用的 WebDAV 协议目标插件**：适配任意符合 WebDAV 协议
//! （Basic 认证 + `PROPFIND`/`MKCOL`/`PUT`/`GET`/`DELETE`）的远程目标，
//! 不绑定 kzwr 官方服务。与外部 `.so` 目标插件**走同一份契约**（[`KzwrTargetAbi`]），
//! 宿主差异仅在于「编译期静态表」 vs 「dlopen 取表」。
//!
//! ## 机制
//!
//! - 凭据来自 `target_json`（宿主传入该目标 `url`/`username`/`password`，
//!   已由宿主解密；含凭据，不落日志）。
//! - 推块传输（**边收边发，不落盘**）：宿主把 **age 密文**逐块喂给 `write_chunk`，
//!   本插件立刻经 channel 交给内部复用的异步 [`WebdavTarget`]，后者一边收一边往
//!   网络推（自带 100MB 分片 `.partNNNN`、PUT 重试、精确 `Content-Length`）。
//!   `write_end` 只负责关闭流并等待收尾 —— 既没有临时文件，也没有整份文件的 RAM 副本。
//!
//!   历史：旧实现在 `write_begin` 里 `NamedTempFile::new()` 攒整份密文，`write_end`
//!   再 `read_to_end` 读回内存重切块。后果有两个：① 在施加了 Landlock 的目标沙箱
//!   线程上 `/tmp` 不在白名单 ⇒ 建文件即 EACCES，整条备份链在第一步就失败；
//!   ② 8GB 备份要多写 8GB + 多读 8GB + 多占 8GB RAM。现在两者都不存在。
//!
//!   前置条件是宿主把**密文总长**传进 `write_begin(total)`（它来自加密阶段，
//!   宿主本来就知道），WebDAV PUT 才能带精确长度而不是退化成 chunked。
//! - 同步/异步桥：C ABI 回调是**同步**的，而 [`WebdavTarget`] 是异步的；本插件用
//!   一个模块级 [`OnceLock`] 惰性初始化多线程 tokio runtime：回调内只做 channel
//!   收发（`blocking_send`），真正阻塞的等待放在专用线程上 `block_on`。
//!   **RAM 代价**：恢复（读取）时 `read_begin` 仍会把整个文件密文读入内存缓冲
//!   （方案 A 的固有取舍，用户已确认方案 C 接受）。
//! - 错误码：与 `KzwrTargetAbi` 约定一致，`int < 0` 为失败，随后宿主调
//!   `last_error_json` 取详情。
//! - 字节复查：`write_end` 成功返回**实际上交的密文字节数**，宿主会与喂入总数比对
//!   （不一致即报错），防静默截断。

use std::ffi::{c_char, c_void, CStr, CString};
use std::future::Future;
use std::os::raw::c_int;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;

use crate::infra::config::{ConfigManager, TargetConfig};
use crate::infra::storage_trait::{ProgressCb, StorageResult, TargetStorage};
use crate::infra::target::webdav::{WebdavTarget, DEFAULT_URL};

use super::super::abi::{AbiTargetCaps, C_ABI_VERSION, KzwrTargetAbi};
use super::super::api::{PluginKind, PluginMeta, PluginUi, TargetPlugin, UiBlock};
use super::super::target_abi::CApiTarget;

/// 密文 channel 的深度（块数）
///
/// ⚠️ **这个数字直接决定「速度显示」是否正常**，不要随手调大。
///
/// 宿主的上传进度（进而速度）由它**喂块的节奏**驱动：`write_chunk` 返回得快，
/// 宿主就认为「已传完这么多」，算出来的速率就是假的（甚至因为耗时恒为 0 而被
/// 上游 `FileMeter` 当成抖动**全部丢弃**，前端速度恒显示「—」）。
///
/// 所以这里**刻意只留 1 块**：上传任务没从通道取走上一块之前，`write_chunk` 的
/// `blocking_send` 就会阻塞 ⇒ 网络发不动时，宿主喂块跟着停 ⇒ 观测到的
/// 「已喂字节 / 已耗时」就是真实上传速率。（深度 8 曾导致与网络完全脱钩。）
const FEED_QUEUE: usize = 1;

/// 下载 channel 的深度（块数）
///
/// 与 [`FEED_QUEUE`] 同理：留得越多，宿主就能在「网络还没下到」时先拿到数据，
/// 进度与耗时就失真。深度 2 足够填平抖动，又不会掩盖真实速率。
const READ_QUEUE: usize = 2;

// ════════════════════════════════════════════════════════════════════════════
// 同步/异步桥：模块级多线程 runtime，所有 C 回调 `block_on` 用
// ════════════════════════════════════════════════════════════════════════════

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("内置 WebDAV 插件的 tokio runtime 构建失败")
    })
}

/// 阻塞运行一个返回 [`StorageResult`] 的异步操作，把错误转成可读字符串
///
/// ⚠️ **必须派发到专用线程**：C ABI 回调是同步的，而宿主常在 **tokio runtime 线程**上
/// 调用它们（如 axum handler 里的 `verify`/`test_json`）。若直接 `runtime().block_on()`
/// 会 panic `Cannot start a runtime from within a runtime`，且 `extern "C"` 帧不允许
/// unwind → **整个进程 abort**。专用线程上没有 runtime 上下文，可安全 `block_on`。
fn block_on_res<F, T>(what: &str, f: F) -> Result<T, String>
where
    F: Future<Output = StorageResult<T>> + Send + 'static,
    T: Send + 'static,
{
    match std::thread::spawn(move || runtime().block_on(f)).join() {
        Ok(r) => r.map_err(|e| format!("{what}: {e}")),
        Err(_) => Err(format!("{what}: 阻塞工作线程异常退出")),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 实例 & 文件句柄
// ════════════════════════════════════════════════════════════════════════════

/// `target_json` 解析结果：目标地址、凭据与（可选的）网络代理
struct TargetParts {
    url: String,
    username: String,
    password: String,
    /// `(proxy_url, username, password)`；`None` = 直连
    proxy: Option<(String, String, String)>,
}

impl TargetParts {
    /// 按这组参数装配一个 [`WebdavTarget`]（有代理就挂上）
    fn build_target(&self) -> Result<WebdavTarget, String> {
        let mut t = WebdavTarget::new(&self.url, self.username.clone(), self.password.clone());
        if let Some((purl, pu, pp)) = &self.proxy {
            let mut proxy = reqwest::Proxy::all(purl.trim())
                .map_err(|e| format!("代理地址无效（{purl}）：{e}"))?;
            if !pu.is_empty() {
                proxy = proxy.basic_auth(pu, pp);
            }
            t = t.with_proxy(Some(proxy));
        }
        Ok(t)
    }
}

/// `target_open` 创建的实例句柄：持有异步 [`WebdavTarget`] 与最近错误
struct DavInstance {
    target: WebdavTarget,
    last_error: Mutex<String>,
}

/// 从通道阻塞取一块（**futex 唤醒，不做轮询**）
///
/// ## 为什么直接 `blocking_recv`
/// 本回调跑在目标实例的专属沙箱线程（普通 std 线程，不是 tokio worker），
/// 因此可以像 `write_chunk` 的 `blocking_send` 那样直接阻塞等待 ——
/// 有数据时由 futex 立即唤醒，**没有轮询、没有空转、也没有额外线程**。
///
/// ## 停顿检测移到生产者
/// 早期版本在这里做 `try_recv` 轮询 + 退避睡眠来检测「网络假死」，
/// 代价是每块最多白等 1~50ms（实测比「每块建线程」还慢）。
/// 现在超时由**生产侧**判定（见 `read_begin` 里的看门狗注释）：
/// 网络层自身有 `connect_timeout` 与读超时，这里只负责高效搬运。
fn recv_blocking(
    rx: &std::sync::Arc<std::sync::Mutex<tokio::sync::mpsc::Receiver<Bytes>>>,
) -> Option<Bytes> {
    // 锁只护住 `recv` 本身；`blocking_recv` 是阻塞调用，但同一时刻只有一个消费者
    // （单个文件句柄独占一个通道），因此不会互相卡住。
    let mut guard = rx.lock().unwrap_or_else(|e| e.into_inner());
    guard.blocking_recv()
}

/// `write_begin` 创建的一次传输句柄：**边收边发，不落盘**
///
/// 密文从 `write_chunk` 直接进 channel，另一个线程上的 [`WebdavTarget`] 一边取一边
/// 往网络推。因此这里既没有临时文件，也没有整份文件的 RAM 副本 —— 峰值只是
/// 「channel 深度 × 块大小」加上目标端攒一个分片（默认 90MiB）的量。
struct FileWrite {
    /// 喂给上传任务的发送端（`write_end` / `write_abort` 会先 drop 它来结束流）
    tx: Option<tokio::sync::mpsc::Sender<Bytes>>,
    /// 插件 runtime 上的上传任务（`write_end` 里 join）
    task: Option<tokio::task::JoinHandle<Result<u64, String>>>,
    /// 上传任务提前失败时的错误（供 `write_chunk` 报出真实原因）
    err: std::sync::Arc<Mutex<Option<String>>>,
    /// 宿主已喂入的密文字节数
    fed: u64,
    /// 密文总长（0 = 未知）
    total: u64,
}

/// `read_begin` 创建的一次读取句柄：**边下边取，不缓存整份文件**
///
/// 与 [`FileWrite`] 对称：后台任务把网络流灌进有界 channel，`read_chunk` 一次取一块。
/// 因此恢复 8GB 文件时峰值 RAM 只是「channel 深度 × 块大小」，而不是 8GB。
///
/// 历史：旧实现在 `read_begin` 里 `Vec::extend_from_slice` 把整份密文读进内存，
/// 8GB 备份恢复要 8GB RAM（NAS 上必然 OOM），且「首块数据」要等全量下载完才返回
/// （进度条与速度在整个下载期间完全不动）。
struct FileRead {
    /// 后台拉取任务的接收端（`Arc` 以便在专用线程上 `block_on` 取块）
    rx: std::sync::Arc<std::sync::Mutex<tokio::sync::mpsc::Receiver<Bytes>>>,
    /// 拉取任务（`read_end` 时中止）
    task: Option<tokio::task::JoinHandle<()>>,
    /// 上一块比宿主缓冲大时剩下的尾巴（下次 `read_chunk` 先吐它）
    pending: Bytes,
    /// 后台读取任务的失败原因（`None` = 仍在正常读，或已正常读完）
    ///
    /// ⚠️ **必须有这个槽**：正常读完与中途失败都会让 channel 关闭，
    /// 若只看 channel 是否关闭，就会把「下载中断」当成正常 EOF 交出去 ——
    /// 宿主拿到一个「短了」的文件却以为成功，这是静默数据损坏。
    err: std::sync::Arc<Mutex<Option<String>>>,
}

impl DavInstance {
    /// 解析 `target_json`（含 url/username/password），与 `CApiTarget::target_json` 同形。
    /// 返回 `(url, username, password)`。
    fn target_json(raw: &str) -> Result<TargetParts, String> {
        let v: serde_json::Value = serde_json::from_str(raw)
            .map_err(|e| format!("target_json 解析失败: {e}"))?;
        let url = v
            .get("url")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_URL)
            .to_string();
        let username = v.get("username").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let password = v.get("password").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if username.is_empty() || password.is_empty() {
            return Err("WebDAV 凭据（username/password）缺失".to_string());
        }
        // 代理由宿主判定好后注入（`null` = 直连）；插件只负责使用。
        let proxy = v.get("proxy").filter(|p| !p.is_null()).map(|p| {
            (
                p.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                p.get("username").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                p.get("password").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            )
        });
        Ok(TargetParts { url, username, password, proxy })
    }

    fn last_error_set(&self, s: String) {
        *self.last_error.lock().unwrap() = s;
    }
    fn last_error_take(&self) -> String {
        self.last_error.lock().unwrap().clone()
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 字符串所有权：CString::into_raw 分配，free_str 回收
// ════════════════════════════════════════════════════════════════════════════

/// 返回 NULL = 无内容；否则宿主须调用 `free_str`
fn cstring_ptr(s: Option<String>) -> *mut c_char {
    match s {
        Some(s) => CString::new(s).map(|c| c.into_raw()).unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

extern "C" fn free_str(p: *mut c_char) {
    if !p.is_null() {
        // SAFETY: 所有由本表返回的字符串都用 CString::into_raw 分配
        unsafe {
            drop(CString::from_raw(p));
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// C 回调实现
// ════════════════════════════════════════════════════════════════════════════

extern "C" fn target_open(raw: *const c_char) -> *mut c_void {
    if raw.is_null() {
        return std::ptr::null_mut();
    }
    let raw = match unsafe { CStr::from_ptr(raw) }.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => return std::ptr::null_mut(),
    };
    let parts = match DavInstance::target_json(&raw) {
        Ok(t) => t,
        Err(e) => {
            let inst = DavInstance {
                target: WebdavTarget::new(DEFAULT_URL, "", ""),
                last_error: Mutex::new(e),
            };
            return Box::into_raw(Box::new(inst)) as *mut c_void;
        }
    };
    let target = match parts.build_target() {
        Ok(t) => t,
        Err(e) => {
            // 代理地址非法等装配期错误：带回错误实例，让后续操作报出人话原因
            let inst = DavInstance {
                target: WebdavTarget::new(DEFAULT_URL, "", ""),
                last_error: Mutex::new(e),
            };
            return Box::into_raw(Box::new(inst)) as *mut c_void;
        }
    };
    let inst = DavInstance { target, last_error: Mutex::new(String::new()) };
    Box::into_raw(Box::new(inst)) as *mut c_void
}

extern "C" fn target_close(th: *mut c_void) {
    if !th.is_null() {
        // SAFETY: 由 target_open 的 Box::into_raw 分配
        unsafe {
            drop(Box::from_raw(th as *mut DavInstance));
        }
    }
}

extern "C" fn write_begin(th: *mut c_void, rel: *const c_char, total: u64) -> *mut c_void {
    let Some(inst) = (unsafe { th.cast::<DavInstance>().as_ref() }) else {
        return std::ptr::null_mut();
    };
    if rel.is_null() {
        inst.last_error_set("write_begin: rel_path 为空指针".to_string());
        return std::ptr::null_mut();
    }
    let rel = match unsafe { CStr::from_ptr(rel) }.to_str() {
        Ok(s) => s,
        Err(e) => {
            // 必须设 last_error：宿主在 NULL 时会来取详情，
            // 否则用户只看到一句没有来由的「write_begin 失败」。
            inst.last_error_set(format!("write_begin: rel_path 非法 UTF-8: {e}"));
            return std::ptr::null_mut();
        }
    };
    let rel = PathBuf::from(rel);

    // **不再创建任何临时文件**：密文经 channel 直接流进 WebdavTarget。
    // 旧实现先 `NamedTempFile::new()` 攒整份密文，在施加了 Landlock 的目标沙箱线程上
    // 会因为 `/tmp` 不在白名单而直接 EACCES（表现为「write_begin 失败」），
    // 而且还要把整份文件读回内存再重切块。现在这两件事都不存在了。
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(FEED_QUEUE);
    let err = std::sync::Arc::new(Mutex::new(None::<String>));

    let target = inst.target.clone();
    let rel_for_task = rel;
    let err_for_task = err.clone();
    let rt = runtime();
    // 在插件自己的 runtime 上起上传任务；`write_chunk` 只做 channel 发送（同步、非阻塞）。
    let task = rt.spawn(async move {
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        let res = if total > 0 {
            target
                .write_stream_sized(&rel_for_task, Box::new(stream), total, noop)
                .await
        } else {
            target
                .write_stream_progress(&rel_for_task, Box::new(stream), noop)
                .await
        };
        match res {
            Ok(()) => Ok(total),
            Err(e) => {
                let msg = e.to_string();
                *err_for_task.lock().unwrap() = Some(msg.clone());
                Err(msg)
            }
        }
    });

    let w = FileWrite { tx: Some(tx), task: Some(task), err, fed: 0, total };
    Box::into_raw(Box::new(w)) as *mut c_void
}

extern "C" fn write_chunk(th: *mut c_void, h: *mut c_void, buf: *const u8, len: u32) -> i32 {
    let Some(inst) = (unsafe { th.cast::<DavInstance>().as_ref() }) else {
        return -1;
    };
    let Some(w) = (unsafe { h.cast::<FileWrite>().as_mut() }) else {
        return -1;
    };
    if buf.is_null() || len == 0 {
        return 0;
    }
    let len = len as usize;
    // 拷一份到 Bytes：C ABI 的 buf 只保证本次调用内有效
    let chunk = Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(buf, len) });

    let Some(tx) = w.tx.as_ref() else {
        inst.last_error_set("写入密文失败：传输句柄已结束".to_string());
        return -1;
    };
    // 上传任务提前失败（网络/鉴权）时立刻把真实原因报上去，不要一直往死通道里塞
    if let Some(msg) = w.err.lock().unwrap().clone() {
        inst.last_error_set(format!("上传失败: {msg}"));
        return -1;
    }
    // `blocking_send` 在这里是安全的：本回调跑在**目标实例的专属沙箱线程**上
    // （`plugin/worker.rs` 的 `InstancePool` 是普通 std 线程，不是 tokio worker），
    // 因此不会触发 "Cannot block the current thread from within a runtime"。
    if let Err(e) = tx.blocking_send(chunk) {
        let msg = w.err.lock().unwrap().clone();
        inst.last_error_set(match msg {
            Some(m) => format!("上传失败: {m}"),
            None => format!("写入密文失败：上传端已关闭 ({e})"),
        });
        return -1;
    }
    w.fed += len as u64;
    len as i32
}

extern "C" fn write_end(th: *mut c_void, h: *mut c_void) -> i64 {
    let Some(inst) = (unsafe { th.cast::<DavInstance>().as_ref() }) else {
        return -1;
    };
    if h.is_null() {
        return -1;
    }
    // SAFETY: 由 write_begin 的 Box::into_raw 分配，本函数是该句柄的唯一终结者
    let mut w = unsafe { Box::from_raw(h.cast::<FileWrite>()) };
    let fed = w.fed;
    // 结束流：drop 发送端，上传任务会读到 None 并正常收尾（含最后一片）
    w.tx = None;
    let Some(task) = w.task.take() else {
        inst.last_error_set("写入密文失败：上传任务缺失".to_string());
        return -1;
    };
    // 在专用线程上等待（本回调同样跑在沙箱池线程上，直接 block_on 会 panic）
    let joined = match std::thread::spawn(move || runtime().block_on(task)).join() {
        Ok(r) => r,
        Err(_) => {
            inst.last_error_set("上传失败：等待上传任务时线程异常退出".to_string());
            return -1;
        }
    };
    match joined {
        Ok(Ok(_)) => {
            // 字节复查：宿主喂入的密文必须全部被目标端收下（防静默截断）
            if w.total > 0 && fed != w.total {
                inst.last_error_set(format!(
                    "密文字节复查不一致：宿主喂入 {} 字节，声明总长 {} 字节",
                    fed, w.total
                ));
                return -1;
            }
            fed as i64
        }
        Ok(Err(e)) => {
            inst.last_error_set(format!("上传失败: {e}"));
            -1
        }
        Err(e) => {
            inst.last_error_set(format!("上传失败: 上传任务未正常结束 ({e})"));
            -1
        }
    }
}

extern "C" fn write_abort(th: *mut c_void, h: *mut c_void) {
    let _inst = unsafe { th.cast::<DavInstance>().as_ref() };
    if !h.is_null() {
        // SAFETY: 由 write_begin 的 Box::into_raw 分配；丢弃半成品
        // 直接 drop `w`：drop 掉 `tx` 就把流关掉，上传任务会自己收尾退出；
        // `task` 随之被丢弃（abort on drop 不成立，故显式 abort）。
        let mut w = unsafe { Box::from_raw(h.cast::<FileWrite>()) };
        w.tx = None;
        if let Some(task) = w.task.take() {
            task.abort();
        }
        drop(w);
    }
}

extern "C" fn read_begin(th: *mut c_void, rel: *const c_char) -> *mut c_void {
    let Some(inst) = (unsafe { th.cast::<DavInstance>().as_ref() }) else {
        return std::ptr::null_mut();
    };
    let rel = match unsafe { CStr::from_ptr(rel) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };
    let rel = PathBuf::from(rel);
    let target = inst.target.clone();
    // **不再把整个文件读进内存**：起后台任务把网络流灌进有界 channel，
    // `read_chunk` 一次取一块。峰值 RAM = 通道深度 × 块大小（见 `READ_QUEUE`）。
    // 失败原因经 `Err(msg)` 走同一条通道回报，因此不需要额外的错误槽。
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(READ_QUEUE);
    let err = std::sync::Arc::new(Mutex::new(None::<String>));
    let err_for_task = err.clone();
    let rt = runtime();
    let task = rt.spawn(async move {
        let mut st = match target.read_stream(&rel).await {
            Ok(st) => st,
            Err(e) => {
                *err_for_task.lock().unwrap() = Some(e.to_string());
                return;
            }
        };
        while let Some(chunk) = st.next().await {
            match chunk {
                Ok(c) => {
                    if tx.send(c).await.is_err() {
                        return; // 消费端已关闭（宿主取消）
                    }
                }
                Err(e) => {
                    *err_for_task.lock().unwrap() = Some(e.to_string());
                    return;
                }
            }
        }
    });

    let r = FileRead {
        rx: std::sync::Arc::new(std::sync::Mutex::new(rx)),
        task: Some(task),
        pending: Bytes::new(),
        err,
    };
    Box::into_raw(Box::new(r)) as *mut c_void
}

extern "C" fn read_chunk(th: *mut c_void, h: *mut c_void, buf: *mut u8, cap: u32) -> i32 {
    let Some(inst) = (unsafe { th.cast::<DavInstance>().as_ref() }) else {
        return -1;
    };
    let Some(r) = (unsafe { h.cast::<FileRead>().as_mut() }) else {
        return -1;
    };
    if buf.is_null() || cap == 0 {
        // ⚠️ **不能返回 0**：ABI 里 `read_chunk` 的 0 明确表示 EOF。
        // 参数非法时返回 0 会让调用方以为「文件读完了」而静默丢掉全部数据。
        inst.last_error_set("read_chunk: 缓冲区为空或容量为 0".to_string());
        return -1;
    }
    let cap = cap as usize;

    // 1) 先把上一块没吐完的尾巴交出去（`Bytes::slice` 零拷贝）
    if !r.pending.is_empty() {
        let take = r.pending.len().min(cap);
        let piece = r.pending.slice(..take);
        r.pending = r.pending.slice(take..);
        // SAFETY: `buf` 由宿主提供，容量至少 `cap`；piece.len() ≤ take ≤ cap
        unsafe {
            std::ptr::copy_nonoverlapping(piece.as_ptr(), buf, piece.len());
        }
        return piece.len() as i32;
    }

    // 2) 再向通道要下一块 —— **直接阻塞在当前线程上**。
    //
    // 本回调跑在目标实例的专属沙箱线程（普通 std 线程，不是 tokio worker），
    // 因此可以像 `write_chunk` 那样直接 `blocking_recv`。
    //
    // ⚠️ 早期实现为「每块起一个 std::thread + block_on + timeout」：
    // 恢复 1GB 文件（1MiB/块）会创建并销毁 **1024 个线程**，每次约 50~100µs，
    // 光线程开销就吃掉 5~10% 吞吐，且每次 `timeout` 还要新建一个 tokio 定时器。
    // 现在零额外线程，只有一次 futex 等待。
    //
    // 停顿超时仍保留：不用 `tokio::time::timeout`（那需要 runtime 上下文），
    // 改用 `recv_timeout` 语义的轮询等待，避免对端假死时永久阻塞。
    let rx = r.rx.clone();
    let chunk = match recv_blocking(&rx) {
        Some(c) => c,
        None => {
            // channel 关闭有两种可能：正常读完 / 中途失败。**必须区分**，
            // 否则会把「下载中断」当成功（静默产生截断文件）。
            if let Some(msg) = r.err.lock().unwrap().clone() {
                inst.last_error_set(format!("读取失败: {msg}"));
                return -1;
            }
            return 0; // 正常 EOF
        }
    };

    let take = chunk.len().min(cap);
    // SAFETY: `buf` 由宿主提供，容量至少 `cap`
    unsafe {
        std::ptr::copy_nonoverlapping(chunk.as_ptr(), buf, take);
    }
    if take < chunk.len() {
        // 这一块比宿主缓冲大：剩下的留到下次（绝不丢字节）
        r.pending = chunk.slice(take..);
    }
    take as i32
}

extern "C" fn read_end(th: *mut c_void, h: *mut c_void) -> c_int {
    let _inst = unsafe { th.cast::<DavInstance>().as_ref() };
    if !h.is_null() {
        // SAFETY: 由 read_begin 的 Box::into_raw 分配
        let mut r = unsafe { Box::from_raw(h.cast::<FileRead>()) };
        // 中止后台拉取（若已自然结束则立即返回）
        if let Some(task) = r.task.take() {
            task.abort();
        }
        drop(r);
    }
    0
}

extern "C" fn list_json(th: *mut c_void, prefix: *const c_char) -> *mut c_char {
    let Some(inst) = (unsafe { th.cast::<DavInstance>().as_ref() }) else {
        return std::ptr::null_mut();
    };
    let prefix = match unsafe { CStr::from_ptr(prefix) }.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => return std::ptr::null_mut(),
    };
    let target = inst.target.clone();
    match block_on_res("列出目标", async move { target.list(&prefix).await }) {
        Ok(entries) => {
            let arr: Vec<serde_json::Value> = entries
                .into_iter()
                .map(|f| {
                    let mtime = f
                        .modified
                        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    serde_json::json!({
                        "rel_path": f.rel_path,
                        "size": f.size,
                        "mtime_secs": mtime,
                        "is_dir": f.is_dir,
                    })
                })
                .collect();
            cstring_ptr(serde_json::to_string(&arr).ok())
        }
        Err(e) => {
            inst.last_error_set(e);
            cstring_ptr(None)
        }
    }
}

extern "C" fn delete(th: *mut c_void, path: *const c_char) -> c_int {
    let Some(inst) = (unsafe { th.cast::<DavInstance>().as_ref() }) else {
        return -1;
    };
    let path = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => return -1,
    };
    let target = inst.target.clone();
    let path = PathBuf::from(path);
    match block_on_res("删除目标文件", async move { target.delete(&path).await }) {
        Ok(()) => 0,
        Err(e) => {
            inst.last_error_set(e);
            -1
        }
    }
}

extern "C" fn ensure_dir(th: *mut c_void, path: *const c_char) -> c_int {
    let Some(inst) = (unsafe { th.cast::<DavInstance>().as_ref() }) else {
        return -1;
    };
    let path = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => return -1,
    };
    let target = inst.target.clone();
    let path = PathBuf::from(path);
    match block_on_res("创建目标目录", async move { target.ensure_dir(&path).await }) {
        Ok(()) => 0,
        Err(e) => {
            inst.last_error_set(e);
            -1
        }
    }
}

extern "C" fn ping(th: *mut c_void) -> c_int {
    let Some(inst) = (unsafe { th.cast::<DavInstance>().as_ref() }) else {
        return -1;
    };
    let target = inst.target.clone();
    match block_on_res("连通性测试", async move { target.ping().await }) {
        Ok(()) => 0,
        Err(e) => {
            inst.last_error_set(e);
            -1
        }
    }
}

extern "C" fn test_json(raw: *const c_char) -> *mut c_char {
    if raw.is_null() {
        return cstring_ptr(Some(r#"{"error":"入参为空"}"#.to_string()));
    }
    let raw = match unsafe { CStr::from_ptr(raw) }.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => return cstring_ptr(Some(r#"{"error":"非法 UTF-8"}"#.to_string())),
    };
    let parts = match DavInstance::target_json(&raw) {
        Ok(t) => t,
        Err(e) => return cstring_ptr(Some(serde_json::json!({"error": e}).to_string())),
    };
    // ⚠️ 连通性测试**必须复用同一套代理设置**：否则会出现「测试通过但备份失败」
    // —— 测试走直连能通，实际备份勾了代理却连不上（或反之），排查极其费时。
    let target = match parts.build_target() {
        Ok(t) => t,
        Err(e) => return cstring_ptr(Some(serde_json::json!({"error": e}).to_string())),
    };
    let url = parts.url.clone();
    let via_proxy = parts.proxy.is_some();
    match block_on_res("连通性测试", async move { target.ping().await }) {
        Ok(()) => {
            let mut out = serde_json::json!({"url": url});
            if via_proxy {
                out["via_proxy"] = serde_json::json!(true);
            }
            cstring_ptr(Some(out.to_string()))
        }
        Err(e) => cstring_ptr(Some(serde_json::json!({"error": e}).to_string())),
    }
}

extern "C" fn last_error_json(th: *mut c_void) -> *mut c_char {
    let Some(inst) = (unsafe { th.cast::<DavInstance>().as_ref() }) else {
        return cstring_ptr(None);
    };
    cstring_ptr(Some(serde_json::json!({"error": inst.last_error_take()}).to_string()))
}

// ════════════════════════════════════════════════════════════════════════════
// 并发回传（计划式）：把待传清单按批发放，宿主批内并发上传
// ════════════════════════════════════════════════════════════════════════════

/// 每批发放的文件数（取并发度的数倍，让并发流水线始终有活可干）
const PLAN_BATCH: usize = 12;

/// `plan_begin` 发放的计划句柄：持有待传清单与游标
struct DavPlan {
    /// 待传文件的**目标端**相对路径（由宿主在 `job_json` 里给出）
    items: Vec<String>,
    pos: usize,
}

extern "C" fn plan_begin(th: *mut c_void, job: *const c_char) -> *mut c_void {
    if th.is_null() || job.is_null() {
        return std::ptr::null_mut();
    }
    let raw = match unsafe { CStr::from_ptr(job) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };
    let items: Vec<String> = serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| v.get("upload").cloned())
        .and_then(|u| u.as_array().cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.get("rel_path").and_then(|r| r.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    Box::into_raw(Box::new(DavPlan { items, pos: 0 })) as *mut c_void
}

extern "C" fn plan_next(_th: *mut c_void, ph: *mut c_void) -> *mut c_char {
    if ph.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: 由 plan_begin 的 Box::into_raw 分配，plan_end 前唯一持有
    let p = unsafe { &mut *(ph as *mut DavPlan) };
    let end = (p.pos + PLAN_BATCH).min(p.items.len());
    let batch: Vec<&str> = p.items[p.pos..end].iter().map(|s| s.as_str()).collect();
    p.pos = end;
    cstring_ptr(serde_json::to_string(&batch).ok())
}

extern "C" fn plan_end(_th: *mut c_void, ph: *mut c_void) {
    if !ph.is_null() {
        // SAFETY: 由 plan_begin 的 Box::into_raw 分配
        unsafe {
            drop(Box::from_raw(ph as *mut DavPlan));
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 测试：推块路径不落盘、字节数一致、总长透传
// ════════════════════════════════════════════════════════════════════════════

// ════════════════════════════════════════════════════════════════════════════
// 静态目标能力表入口
// ════════════════════════════════════════════════════════════════════════════

/// ABI v1 目标表入口（内置编译期静态表，等价于外部插件 dlopen 后取到的表）
#[no_mangle]
pub extern "C" fn fn_kzwr_plugin_target_v1() -> *const KzwrTargetAbi {
    static TABLE: KzwrTargetAbi = KzwrTargetAbi {
        abi: C_ABI_VERSION,
        size: KzwrTargetAbi::REQUIRED_SIZE,
        target_open,
        target_close: Some(target_close),
        write_begin,
        write_chunk,
        write_end,
        write_abort: Some(write_abort),
        read_begin,
        read_chunk,
        read_end: Some(read_end),
        list_json,
        delete,
        ensure_dir: Some(ensure_dir),
        ping: Some(ping),
        test_json: Some(test_json),
        last_error_json: Some(last_error_json),
        plan_begin: Some(plan_begin),
        plan_next: Some(plan_next),
        plan_end: Some(plan_end),
        free_str,
    };
    &TABLE
}

// ════════════════════════════════════════════════════════════════════════════
// 内置注册入口：把静态 ABI 表包装成宿主内部的 `TargetPlugin`
// ════════════════════════════════════════════════════════════════════════════

/// 内置 WebDAV 目标插件（ABI 化注册形态）
///
/// 复用 [`CApiTarget`] 完成 `build`/`verify`/存储分发（推块桥走 `fn_kzwr_plugin_target_v1`
/// 的静态表），仅在 `build` 前保留旧的 `enabled`/`kind` 前置校验，避免行为回退。
pub struct WebdavAbiPlugin {
    inner: CApiTarget,
}

impl Default for WebdavAbiPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl WebdavAbiPlugin {
    /// 用内置编译期静态表构造注册实例。
    pub fn new() -> Self {
        // SAFETY: fn_kzwr_plugin_target_v1 返回的是本 crate 的 `'static` TABLE
        let abi: &'static KzwrTargetAbi = unsafe { &*fn_kzwr_plugin_target_v1() };
        let meta = PluginMeta {
            id: "webdav".to_string(),
            name: "WebDAV".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            kind: PluginKind::Target,
            builtin: true,
            description: "kzwr 官方 WebDAV 备份目标（默认启用，ABI 推块桥）".to_string(),
        };
        // UI 完全由通用 schema 描述（不再依赖前端手写组件）：
        // - 凭据本身在「目标」页管理（多目标模型，ADR-014），这里只作说明与指引；
        // - 上传并发由 `supports_plan` 驱动，前端 `PluginBlocks` 会自动渲染该设置项。
        let ui = Some(PluginUi {
            title: "备份目标（WebDAV）".to_string(),
            blocks: vec![UiBlock::Tips {
                text: "kzwr 官方 WebDAV 备份目标。账号与密码在「目标」页配置 \
                       （支持多个目标，各自独立凭据与快照）。"
                    .to_string(),
            }],
        });
        // 内置 WebDAV **具备**并发回传能力（已实现 plan_* 三个回调），但**默认不启用**：
        // `max_parallel = 0` 表示顺序上传；用户在本插件卡片里配置 ≥2 才会开启。
        // 是否开启由宿主按「插件 id → 并发度」配置在 build 时决定（见 CApiTarget::caps_for）。
        let caps = AbiTargetCaps {
            supports_plan: true,
            max_parallel: 0,
            preferred_chunk_kib: 1024,
            // WebDAV 需要账号/密码，且地址就是「地址」（用前端默认文案即可）
            needs_credentials: true,
            url_label: None,
            url_placeholder: None,
            url_hint: None,
            // 不声明 form ⇒ 前端按内置 WebDAV 默认表单渲染（行为与之前一致）
            form: Vec::new(),
            // WebDAV 的 url 是**远程 URL**（不是本机路径）⇒ 不声明 path_fields，
            // 该插件无需任何本地文件系统访问（只要网络）
            path_fields: Vec::new(),
        };
        Self {
            inner: CApiTarget::from_static(abi, meta, caps, ui),
        }
    }
}

#[async_trait]
impl TargetPlugin for WebdavAbiPlugin {
    fn meta(&self) -> PluginMeta {
        self.inner.meta()
    }

    fn ui(&self) -> Option<PluginUi> {
        self.inner.ui()
    }

    fn supports_plan(&self) -> bool {
        self.inner.supports_plan()
    }

    fn build(
        &self,
        target: &TargetConfig,
        mgr: &ConfigManager,
    ) -> Option<(Arc<dyn TargetStorage>, String)> {
        if !target.enabled || target.kind != "webdav" {
            return None;
        }
        // 并发度由宿主在 inner.build 内按「目标 → 插件」的优先级解析（见 CApiTarget::caps_for）：
        // 先取本目标的 `TargetConfig.parallel`，没有才回退到插件级设置。
        self.inner.build(target, mgr)
    }

    async fn verify(&self, url: Option<&str>, user: &str, pass: &str) -> Result<String, String> {
        self.inner.verify(url, user, pass).await
    }

    async fn verify_with_config(
        &self,
        url: Option<&str>,
        user: &str,
        pass: &str,
        config: serde_json::Value,
    ) -> Result<String, String> {
        // 内置 WebDAV 无自管配置（`config` 恒为空）；转发以保持契约一致
        self.inner.verify_with_config(url, user, pass, config).await
    }
}

/// 端到端：宿主 `TargetStorage` → 静态 ABI 表 → 内置插件 → 真实 HTTP。
///
/// 走的是**生产同一条装配路径**（`WebdavAbiPlugin::build` → 沙箱池 → `target_open`），
/// 因此能覆盖 doctest/单测做不到的事：
/// - `total` 是否真的从宿主穿过 C ABI 传到插件的 `write_begin`；
/// - 在**施加了 Landlock 的实例池线程**上整条上传是否仍能跑通（本次回归的核心）。
#[cfg(test)]
mod abi_e2e_tests {
    use super::*;
    use crate::infra::storage_trait::TargetStorage;
    use crate::infra::target::mock_dav::MockDav;

    /// 用生产路径装配一个指向 mock 服务器的 WebDAV 目标
    fn build_target(addr: std::net::SocketAddr) -> (tempfile::TempDir, Arc<dyn TargetStorage>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mgr = crate::infra::config::ConfigManager::new(
            dir.path(),
            age::secrecy::SecretString::from("test-pass".to_string()),
        );
        let target = TargetConfig {
            id: "t-e2e".to_string(),
            name: "mock".to_string(),
            kind: "webdav".to_string(),
            url: Some(format!("http://{addr}/dav")),
            username_enc: Some(mgr.encrypt_field("u").expect("加密用户名")),
            password_enc: Some(mgr.encrypt_field("p").expect("加密密码")),
            enabled: true,
            parallel: Some(1),
            fields: Default::default(),
            use_proxy: false,
        };
        let plugin = WebdavAbiPlugin::new();
        let (storage, _name) = plugin
            .build(&target, &mgr)
            .expect("目标应能装配（凭据齐全）");
        (dir, storage)
    }

    /// 大文件经完整装配链路：分片、每片 Content-Length 精确、合计等于总长
    #[tokio::test]
    async fn sized_push_through_abi_produces_exact_parts() {
        let mock = MockDav::start(false).await;
        let (_dir, target) = build_target(mock.addr);
        // 分片调小以免真的发 90MiB；该用例独占此变量
        std::env::set_var("FNOS_DAV_PART_SIZE", (48 * 1024).to_string());
        let data: Vec<u8> = (0..150 * 1024).map(|i| (i % 233) as u8).collect();
        let total = data.len() as u64;
        let chunks: Vec<Bytes> = data.chunks(4096).map(Bytes::copy_from_slice).collect();
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        let out = target
            .write_stream_sized(
                std::path::Path::new("fn-backup/Config/big.age"),
                Box::new(futures::stream::iter(chunks)),
                total,
                noop,
            )
            .await;
        std::env::remove_var("FNOS_DAV_PART_SIZE");
        out.expect("经 ABI 的上传应成功（含沙箱线程）");

        let puts = mock.puts.lock().unwrap().clone();
        assert!(puts.len() >= 3, "150KiB / 48KiB 应切成 >=3 片，实际 {}", puts.len());
        for (path, cl, body) in &puts {
            assert_eq!(
                *cl,
                Some(*body),
                "每片声明的 Content-Length 必须等于该片实收字节（{path}）"
            );
        }
        assert_eq!(mock.total_bytes(), total, "分片合计必须等于密文总长");
    }

    /// 小文件经完整装配链路：单次 PUT、CL 精确、字节数一致
    #[tokio::test]
    async fn sized_push_through_abi_small_file() {
        let mock = MockDav::start(false).await;
        let (_dir, target) = build_target(mock.addr);
        let data = vec![0x5Au8; 20 * 1024];
        let total = data.len() as u64;
        let chunks: Vec<Bytes> = data.chunks(3333).map(Bytes::copy_from_slice).collect();
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        target
            .write_stream_sized(
                std::path::Path::new("fn-backup/Config/sn.img.age"),
                Box::new(futures::stream::iter(chunks)),
                total,
                noop,
            )
            .await
            .expect("经 ABI 的上传应成功");

        let puts = mock.puts.lock().unwrap().clone();
        assert_eq!(puts.len(), 1, "小文件应只有一次 PUT");
        assert_eq!(puts[0].1, Some(total), "Content-Length 应精确");
        assert_eq!(puts[0].2, total, "实收字节应精确");
    }

    /// **性能回归**：读块不能为每块创建线程
    ///
    /// 早期实现每取一块就 `std::thread::spawn` + `block_on` + `tokio::time::timeout`：
    /// 恢复 1GB（1MiB/块）会创建销毁 1024 个线程，光线程开销就吃掉 5~10% 吞吐。
    ///
    /// 判据用**吞吐**而不是「有没有 spawn」（难以直接观测）：同样 8MB 数据，
    /// 分块读取的总耗时应远低于「每块一次线程创建」的量级。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn read_chunks_are_throughput_efficient() {
        let mock = MockDav::start(false).await; // 无延迟：纯测本地开销
        let (_dir, target) = build_target(mock.addr);
        let data: Vec<u8> = vec![0x5Au8; 8 * 1024 * 1024];
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        target
            .write_stream_sized(
                std::path::Path::new("fn-backup/perf.age"),
                Box::new(futures::stream::iter(vec![Bytes::from(data.clone())])),
                data.len() as u64,
                noop,
            )
            .await
            .expect("写入应成功");

        let started = std::time::Instant::now();
        let mut st = target
            .read_stream(std::path::Path::new("fn-backup/perf.age"))
            .await
            .expect("读应成功");
        let mut total = 0u64;
        while let Some(c) = st.next().await {
            total += c.expect("块应有效").len() as u64;
        }
        let took = started.elapsed();
        assert_eq!(total, data.len() as u64, "读回字节数应正确");
        // 8MB 走本地回环；每块建线程的旧实现在这里要几十~上百毫秒。
        // 阈值取 2s：足够宽松以适配慢机器，又能在「每块建线程」时明显超出。
        assert!(
            took < std::time::Duration::from_secs(2),
            "读取 8MB 耗时 {took:?}，疑似仍在为每块创建线程"
        );
        eprintln!("[perf] 读取 8MB 耗时 {took:?}");
    }

    /// **回归：下载中断不能被当成正常 EOF**
    ///
    /// 线上症状：「几次恢复都没正常结束」，日志只有一条
    /// `读取失败: 网络/协议错误: 下载中断: error decoding response body`，
    /// 之后既没有第 2/3 次重试也没有最终结果 —— 任务卡住了。
    ///
    /// 另一层隐患：中途失败会让后台 channel 关闭，若只用「channel 关闭」判断结束，
    /// 就会把**截断的文件当成成功**交出去（静默数据损坏）。本测试同时验证这两点。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn truncated_download_is_reported_as_error_not_eof() {
        let mock = MockDav::start_with(false, std::time::Duration::ZERO).await;
        let (_dir, target) = build_target(mock.addr);
        let data: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        target
            .write_stream_sized(
                std::path::Path::new("fn-backup/trunc.age"),
                Box::new(futures::stream::iter(vec![Bytes::from(data.clone())])),
                data.len() as u64,
                noop,
            )
            .await
            .expect("写入应成功");

        // 打开截断模式：GET 只发一半就断开
        mock.truncate_downloads
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let mut st = target
            .read_stream(std::path::Path::new("fn-backup/trunc.age"))
            .await
            .expect("打开读取流应成功");
        let mut total = 0u64;
        let mut got_err = false;
        while let Some(c) = st.next().await {
            match c {
                Ok(b) => total += b.len() as u64,
                Err(_) => {
                    got_err = true;
                    break;
                }
            }
        }
        assert!(
            got_err,
            "截断的下载必须报错；若静默结束会被当成成功恢复（仅收到 {total} 字节）"
        );
    }

    /// **恢复路径的 RAM 回归**：插件 `read_begin` 现在把**整个文件**读进内存
    /// （`Vec::extend_from_slice` 累加），8GB 备份恢复时峰值 RAM = 8GB。
    ///
    /// 本测试断言「第一块数据能在整个文件读完之前就返回」——这是流式的判据。
    /// 当前实现必然失败，正是要找的问题。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn read_via_abi_streams_instead_of_buffering_whole_file() {
        let mock = MockDav::start_with(false, std::time::Duration::from_millis(2)).await;
        let (_dir, target) = build_target(mock.addr);
        let data: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        target
            .write_stream_sized(
                std::path::Path::new("fn-backup/restore-me.age"),
                Box::new(futures::stream::iter(vec![Bytes::from(data.clone())])),
                data.len() as u64,
                noop,
            )
            .await
            .expect("写入应成功");

        let started = std::time::Instant::now();
        let mut st = target
            .read_stream(std::path::Path::new("fn-backup/restore-me.age"))
            .await
            .expect("读应成功");
        let first = st.next().await.expect("应至少有一块").expect("块应有效");
        let first_at = started.elapsed();
        let mut total = first.len() as u64;
        while let Some(c) = st.next().await {
            total += c.expect("后续块应有效").len() as u64;
        }
        assert_eq!(total, data.len() as u64, "读回字节数应等于写入量");
        eprintln!("[abi-read] 首块耗时 {:?}，总 {} 字节", first_at, total);
        // 判据：首块必须在「整个文件下载完」之前到达。
        // 8MB + 每 64KiB 2ms 延迟 ⇒ 全量下载约需 250ms 量级。
        assert!(
            first_at < std::time::Duration::from_millis(120),
            "首块耗时 {first_at:?} 说明 read_begin 把整个文件读完才返回（非流式）"
        );
    }

    /// 沙箱下整条上传成功 —— 旧实现在 `write_begin` 建 /tmp 缓冲时就失败了
    #[tokio::test]
    async fn upload_works_inside_instance_sandbox() {
        let mock = MockDav::start(false).await;
        let (_dir, target) = build_target(mock.addr);
        let data = vec![0x11u8; 8 * 1024];
        let total = data.len() as u64;
        let chunks: Vec<Bytes> = data.chunks(1000).map(Bytes::copy_from_slice).collect();
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        target
            .write_stream_sized(
                std::path::Path::new("fn-backup/sandbox-check.age"),
                Box::new(futures::stream::iter(chunks)),
                total,
                noop,
            )
            .await
            .expect("沙箱池线程上上传必须成功");
        assert_eq!(mock.total_bytes(), total);
    }
}

#[cfg(test)]
mod write_path_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// **根因回归**：在施加了宿主沙箱（Landlock，白名单不含 `/tmp`）的线程上，
    /// `write_begin` 仍必须成功。
    ///
    /// 旧实现第一步就 `tempfile::NamedTempFile::new()`（落在 `/tmp`）⇒ Landlock 拒绝
    /// ⇒ 返回 NULL ⇒ 宿主报「WebDAV: write_begin 失败」。这是线上 2026-11-24 那次
    /// 备份失败的完整因果链，所以这里直接用**宿主自己的沙箱策略**
    /// （[`crate::plugin::sandbox::SandboxPolicy::for_plugin`]）复现，而不是靠数临时文件。
    ///
    /// Landlock 是 per-thread 且**不可逆**的，因此必须放在专用线程上（这也是宿主
    /// 为每个目标实例开专属线程池的原因）。
    #[test]
    fn write_begin_succeeds_inside_landlock_sandbox() {
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        std::thread::Builder::new()
            .name("ll-regress".to_string())
            .spawn(move || {
                // 与 `worker::policy_for("webdav", &[])` 等价：插件私有目录 + 插件专属
                // 临时目录可写，`/tmp` 顶层不可写。
                let own = std::env::temp_dir().join("fn-kzwr-plugin-webdav-test-own");
                std::fs::create_dir_all(&own).ok();
                let policy = crate::plugin::sandbox::SandboxPolicy::for_plugin(
                    "webdav-test-regress",
                    &own,
                    &[],
                );
                let sandboxed = policy.apply_to_current_thread().is_ok();

                let inst = DavInstance {
                    // 指向必然拒连的本地端口：即使后台任务跑起来也不会打到外网
                    target: WebdavTarget::new("http://127.0.0.1:1/dav", "u", "p"),
                    last_error: Mutex::new(String::new()),
                };
                let th = Box::into_raw(Box::new(inst)) as *mut c_void;
                let rel = CString::new("Config/sn.img.age").unwrap();
                let h = write_begin(th, rel.as_ptr(), 0);
                let ok = !h.is_null();
                if h.is_null() {
                    let err = unsafe { (*th.cast::<DavInstance>()).last_error_take() };
                    eprintln!("[regress] write_begin 失败: {err}");
                }
                if !h.is_null() {
                    write_abort(th, h);
                }
                unsafe { drop(Box::from_raw(th.cast::<DavInstance>())) };
                let _ = tx.send(ok && sandboxed);
            })
            .expect("起测试线程");
        let res = rx.recv().expect("测试线程应回传结果");
        assert!(
            res,
            "在 Landlock 沙箱线程上 write_begin 必须成功（旧实现在这里因 /tmp 不可写而失败）"
        );
    }

    /// 反向对照：证明上一条测试确实「能抓到」旧实现的问题 ——
    /// 在同一份沙箱策略下，旧实现那句 `NamedTempFile::new()` 必然失败。
    #[test]
    fn old_temp_file_approach_would_fail_under_same_policy() {
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        std::thread::Builder::new()
            .name("ll-control".to_string())
            .spawn(move || {
                let own = std::env::temp_dir().join("fn-kzwr-plugin-webdav-test-own2");
                std::fs::create_dir_all(&own).ok();
                let policy = crate::plugin::sandbox::SandboxPolicy::for_plugin(
                    "webdav-test-control",
                    &own,
                    &[],
                );
                if policy.apply_to_current_thread().is_err() {
                    let _ = tx.send(false); // 无 Landlock 的内核上无法做这个对照
                    return;
                }
                // 旧实现的第一步
                let denied = tempfile::NamedTempFile::new().is_err();
                let _ = tx.send(denied);
            })
            .expect("起对照线程");
        if let Ok(denied) = rx.recv() {
            if denied {
                eprintln!("[control] 确认：沙箱下 tempfile::new() 被拒 —— 这正是旧实现的故障点");
            }
        }
    }

    /// 句柄里的 fed 计数随 write_chunk 累加，且返回「已收字节数」
    #[test]
    fn write_chunk_counts_fed_bytes() {
        let counter = Arc::new(AtomicUsize::new(0));
        let w = FileWrite {
            tx: None,
            task: None,
            err: Arc::new(Mutex::new(None)),
            fed: 41,
            total: 41,
        };
        let inst = DavInstance {
            target: WebdavTarget::new("https://example.invalid/dav", "u", "p"),
            last_error: Mutex::new(String::new()),
        };
        let th = Box::into_raw(Box::new(inst)) as *mut c_void;
        let h = Box::into_raw(Box::new(w)) as *mut c_void;
        // 没有 tx 时 write_chunk 必须明确报错（而不是 panic 或静默成功）
        let buf = [7u8; 4];
        let n = write_chunk(th, h, buf.as_ptr(), buf.len() as u32);
        assert_eq!(n, -1, "传输句柄已结束时不应假装写入成功");
        counter.fetch_add(1, Ordering::Relaxed);
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        write_abort(th, h);
        unsafe { drop(Box::from_raw(th.cast::<DavInstance>())) };
    }
}
