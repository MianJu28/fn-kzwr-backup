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
//! - 推块传输：宿主把 **age 密文**逐块喂给 `write_chunk`，本插件累积到临时文件，
//!   `write_end` 时一次性交给内部复用的异步 [`WebdavTarget`]（`write_stream_progress`）
//!   完成上传。`WebdavTarget` 自带 100MB 分片（`.partNNNN`）、PUT 重试、进度，
//!   全部沿用，零改动。
//! - 同步/异步桥：C ABI 回调是**同步**的，而 [`WebdavTarget`] 是异步的；本插件用
//!   一个模块级 [`OnceLock`] 惰性初始化多线程 tokio runtime，回调内 `block_on` 驱动。
//!   **RAM 代价**：恢复（读取）时 `read_begin` 会把整个文件密文读入内存缓冲
//!   （方案 A 的固有取舍，用户已确认方案 C 接受）。
//! - 错误码：与 `KzwrTargetAbi` 约定一致，`int < 0` 为失败，随后宿主调
//!   `last_error_json` 取详情。
//! - 字节复查：`write_end` 成功返回**实际上交的密文字节数**，并与宿主喂入总数比对
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
use crate::infra::storage_trait::{ProgressCb, StorageError, StorageResult, TargetStorage};
use crate::infra::target::webdav::{WebdavTarget, DEFAULT_URL};

use super::super::abi::{AbiTargetCaps, C_ABI_VERSION, KzwrTargetAbi};
use super::super::api::{PluginKind, PluginMeta, PluginUi, TargetPlugin};
use super::super::target_abi::CApiTarget;

/// 密文在喂给 `WebdavTarget` 前的切块大小（其内部会叠加自己的 100MB 分片）
const FEED_CHUNK: usize = 256 * 1024;

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

/// `target_open` 创建的实例句柄：持有异步 [`WebdavTarget`] 与最近错误
struct DavInstance {
    target: WebdavTarget,
    last_error: Mutex<String>,
}

/// `write_begin` 创建的一次传输句柄：密文累积到临时文件
struct FileWrite {
    rel: PathBuf,
    /// 累积密文的临时文件（`write_end` 时读回喂给 `WebdavTarget`）
    tmp: std::fs::File,
    tmp_path: PathBuf,
    /// 已喂入（= 已写）密文字节数
    fed: u64,
}

/// `read_begin` 创建的一次读取句柄：整个密文已读入内存
struct FileRead {
    /// 已读密文缓冲（恢复场景，方案 C 接受 RAM 代价）
    data: Vec<u8>,
    /// 本次 `read_chunk` 已读取偏移
    pos: usize,
}

impl DavInstance {
    /// 解析 `target_json`（含 url/username/password），与 `CApiTarget::target_json` 同形。
    /// 返回 `(url, username, password)`。
    fn target_json(raw: &str) -> Result<(String, String, String), String> {
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
        Ok((url, username, password))
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
    let (url, user, pass) = match DavInstance::target_json(&raw) {
        Ok(t) => t,
        Err(e) => {
            let inst = DavInstance {
                target: WebdavTarget::new(DEFAULT_URL, "", ""),
                last_error: Mutex::new(e),
            };
            return Box::into_raw(Box::new(inst)) as *mut c_void;
        }
    };
    let target = WebdavTarget::new(&url, user, pass);
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

extern "C" fn write_begin(th: *mut c_void, rel: *const c_char, _total: u64) -> *mut c_void {
    let Some(inst) = (unsafe { th.cast::<DavInstance>().as_ref() }) else {
        return std::ptr::null_mut();
    };
    let rel = match unsafe { CStr::from_ptr(rel) }.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };
    let rel = PathBuf::from(rel);
    // 创建临时累积文件并**持久化**：`keep()` 交付原可写句柄且不再自动删除；
    // 若只 try_clone 而让 NamedTempFile 析构，文件会被立刻删掉导致 write_end 读回失败。
    let tmpf = match tempfile::NamedTempFile::new() {
        Ok(t) => t,
        Err(e) => {
            inst.last_error_set(format!("创建密文缓冲临时文件失败: {e}"));
            return std::ptr::null_mut();
        }
    };
    let (tmp, tmp_path) = match tmpf.keep() {
        Ok(pair) => pair,
        Err(e) => {
            inst.last_error_set(format!("持久化密文缓冲临时文件失败: {e}"));
            return std::ptr::null_mut();
        }
    };
    let w = FileWrite { rel, tmp, tmp_path, fed: 0 };
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
    let chunk = unsafe { std::slice::from_raw_parts(buf, len) };
    use std::io::Write;
    if let Err(e) = w.tmp.write_all(chunk) {
        inst.last_error_set(format!("写入密文缓冲失败: {e}"));
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
    let w = unsafe { Box::from_raw(h.cast::<FileWrite>()) };
    // 读回整块密文
    use std::io::Read;
    let mut data = Vec::new();
    if let Err(e) = std::fs::File::open(&w.tmp_path).and_then(|mut f| f.read_to_end(&mut data)) {
        inst.last_error_set(format!("读回密文缓冲失败: {e}"));
        return -1;
    }
    let _ = std::fs::remove_file(&w.tmp_path);
    // 把密文切块后交给异步 WebdavTarget（沿用大文件分片 + PUT 重试 + 进度）
    let rel = w.rel.clone();
    let fed = w.fed;
    let chunks: Vec<Bytes> = data
        .chunks(FEED_CHUNK)
        .map(|c| Bytes::copy_from_slice(c))
        .collect();
    let noop: ProgressCb = Arc::new(|_, _, _| {});
    let target = inst.target.clone();
    let res = block_on_res("上传", async move {
        target
            .write_stream_progress(&rel, Box::new(futures::stream::iter(chunks)), noop)
            .await
    });
    match res {
        Ok(()) => {
            // 字节复查：实际上交的密文必须与宿主喂入的完全一致（防静默截断）
            if data.len() as u64 != fed {
                inst.last_error_set(format!(
                    "密文字节复查不一致：缓冲 {} 字节，宿主喂入 {} 字节",
                    data.len(),
                    fed
                ));
                return -1;
            }
            data.len() as i64
        }
        Err(e) => {
            inst.last_error_set(format!("上传失败: {e}"));
            -1
        }
    }
}

extern "C" fn write_abort(th: *mut c_void, h: *mut c_void) {
    let _inst = unsafe { th.cast::<DavInstance>().as_ref() };
    if !h.is_null() {
        // SAFETY: 由 write_begin 的 Box::into_raw 分配；丢弃半成品并清理临时文件
        let w = unsafe { Box::from_raw(h.cast::<FileWrite>()) };
        let _ = std::fs::remove_file(&w.tmp_path);
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
    // 把整个密文流收集进内存
    let res = block_on_res("读取", async move {
        let mut st = target.read_stream(&rel).await?;
        let mut v = Vec::new();
        while let Some(chunk) = st.next().await {
            v.extend_from_slice(&chunk?);
        }
        Ok(v)
    });
    match res {
        Ok(data) => {
            let r = FileRead { data, pos: 0 };
            Box::into_raw(Box::new(r)) as *mut c_void
        }
        Err(e) => {
            inst.last_error_set(format!("读取失败: {e}"));
            std::ptr::null_mut()
        }
    }
}

extern "C" fn read_chunk(th: *mut c_void, h: *mut c_void, buf: *mut u8, cap: u32) -> i32 {
    let _inst = unsafe { th.cast::<DavInstance>().as_ref() };
    let Some(r) = (unsafe { h.cast::<FileRead>().as_mut() }) else {
        return -1;
    };
    if buf.is_null() || cap == 0 {
        return 0;
    }
    let avail = r.data.len().checked_sub(r.pos).unwrap_or(0);
    if avail == 0 {
        return 0; // EOF
    }
    let cap = cap as usize;
    let n = avail.min(cap);
    let dst = unsafe { std::slice::from_raw_parts_mut(buf, n) };
    dst.copy_from_slice(&r.data[r.pos..r.pos + n]);
    r.pos += n;
    n as i32
}

extern "C" fn read_end(th: *mut c_void, h: *mut c_void) -> c_int {
    let _inst = unsafe { th.cast::<DavInstance>().as_ref() };
    if !h.is_null() {
        // SAFETY: 由 read_begin 的 Box::into_raw 分配
        unsafe {
            drop(Box::from_raw(h.cast::<FileRead>()));
        }
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
    let (url, user, pass) = match DavInstance::target_json(&raw) {
        Ok(t) => t,
        Err(e) => return cstring_ptr(Some(serde_json::json!({"error": e}).to_string())),
    };
    let target = WebdavTarget::new(&url, user, pass);
    match block_on_res("连通性测试", async move { target.ping().await }) {
        Ok(()) => cstring_ptr(Some(serde_json::json!({"url": url}).to_string())),
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

/// 并发回传的并发度上限（防止误设超大值把目标端压垮）
const MAX_PARALLEL: u32 = 8;

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
    let items: Vec<String> = serde_json::from_str::<serde_json::Value>(&raw)
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
        config_get: None,
        config_set: None,
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
        let ui = Some(PluginUi {
            section: "settings".to_string(),
            title: "备份目标（WebDAV）".to_string(),
            order: 10,
            component: Some("webdav".to_string()),
            blocks: Vec::new(),
        });
        // 并发回传**默认关闭**（保守优先）。实际并发度在 `build()` 里按用户配置的
        // `plugins.upload_parallel` 覆盖 —— 插件实例在启动时装配，那时配置尚未加载。
        Self {
            inner: CApiTarget::from_static(abi, meta, AbiTargetCaps::default(), ui),
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

    fn build(
        &self,
        target: &TargetConfig,
        mgr: &ConfigManager,
    ) -> Option<(Arc<dyn TargetStorage>, String)> {
        if !target.enabled || target.kind != "webdav" {
            return None;
        }
        // 并发度取自用户配置 `plugins.upload_parallel`（默认 0 = 顺序上传）
        let parallel = mgr.load().ok().map(|c| c.plugins.upload_parallel).unwrap_or(0);
        if parallel >= 2 {
            let mut caps = self.inner.caps();
            caps.supports_plan = true;
            caps.max_parallel = parallel.min(MAX_PARALLEL);
            return self.inner.with_caps(caps).build(target, mgr);
        }
        self.inner.build(target, mgr)
    }

    async fn verify(&self, url: Option<&str>, user: &str, pass: &str) -> Result<String, String> {
        self.inner.verify(url, user, pass).await
    }
}
