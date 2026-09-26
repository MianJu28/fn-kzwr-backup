//! 目标能力表（[`KzwrTargetAbi`]）的宿主适配器
//!
//! 把插件导出的 C 函数表包装成宿主内部的 [`TargetPlugin`] / [`TargetStorage`]，从而：
//! - **备份流水线（扫描/diff/加密/快照/保留/恢复）一行不需改动** —— 这里只是"函数指针版"的存储
//! - 外置与内置目标插件**走同一套契约**，宿主差异仅为"dlopen 取表" vs "编译期静态表"
//!
//! ## 关键机制
//!
//! - **目标实例句柄 `th`**：`target_open(target_json)` 每个目标一份；宿主侧 [`AbiTargetStorage`]
//!   只持有 `th` 与函数表。契约约定：**同一 job 内多路并发 handle** 由插件支持并发；读写同一文件
//!   不交叉由上层保证。
//! - **推块传输**：宿主在 `spawn_blocking` 里把密文逐块喂给 `write_chunk`，块间做超时判断
//!   （**看门狗**）；单次阻塞 FFI 卡死无法强杀，只能判失败并泄漏该线程（契约已明示）。
//! - **字节复查**：`write_end` 返回实写密文字节数，宿主与该文件喂出的字节累加比对；不一致按失败
//!   处理（防静默截断/篡改）。
//!
//! ## 安全
//!
//! FFI 本身是 `unsafe` 的信任边界：插件即任意本机代码。宿主只做契约约定内的操作，并假定插件遵守
//! "字符串 UTF-8/NUL 结尾、返回字符串用 `free_str` 释放"等约定。加载前另有签名校验（见签名模块）。

use std::ffi::c_void;
use std::ffi::CString;
use std::os::raw::c_char;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde::Deserialize;

use crate::infra::config::{ConfigManager, TargetConfig};
use crate::infra::storage_trait::{
    FileDescriptor, PlanSession, PlanUpload, ProgressCb, StorageError, StorageResult, TargetStorage,
};
use crate::plugin::abi::{AbiDescribe, AbiTargetCaps, KzwrTargetAbi};
use crate::plugin::api::{PluginMeta, PluginUi, TargetPlugin};

/// 从插件返回的裸指针取 UTF-8 字符串并用其 `free_str` 释放；NULL → None
fn take_cstring(p: *mut c_char, abi: &'static KzwrTargetAbi) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: 契约要求返回以 NUL 结尾的 UTF-8 字符串
    let s = unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned();
    unsafe { (abi.free_str)(p) };
    Some(s)
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 取插件最近一次错误的详情；未实现 `last_error_json` 时给 fallback
fn plugin_detail(abi: &'static KzwrTargetAbi, th: *mut c_void, fallback: String) -> String {
    abi.last_error_json
        .and_then(|le| {
            let p = unsafe { le(th) };
            take_cstring(p, abi)
        })
        .unwrap_or(fallback)
}

/// 并发回传的并发度上限（防止误设超大值把目标端压垮）
pub const MAX_PARALLEL: u32 = 8;

/// 目标插件：包装 `export_target_v1` 导出的静态表
pub struct CApiTarget {
    meta: PluginMeta,
    abi: &'static KzwrTargetAbi,
    caps: AbiTargetCaps,
    ui: Option<PluginUi>,
}

impl CApiTarget {
    pub fn from_static(
        abi: &'static KzwrTargetAbi,
        meta: PluginMeta,
        caps: AbiTargetCaps,
        ui: Option<PluginUi>,
    ) -> Self {
        Self { meta, abi, caps, ui }
    }

    /// 是否支持并发回传：插件声明支持，且确实实现了 `plan_begin` / `plan_next`
    pub fn supports_plan(&self) -> bool {
        self.caps.supports_plan
            && self.abi.plan_begin.is_some()
            && self.abi.plan_next.is_some()
    }

    /// 按**本插件**的用户配置覆盖并发回传能力
    ///
    /// 并发回传是每插件各自的能力与开关（`plugins.target_parallel[插件id]`）：
    /// - 缺省（未配置）= 沿用插件自身声明；
    /// - `0` / `1` = 关闭并发回传（顺序上传）；
    /// - `≥2` = 启用，该值即并发路数（宿主上限 8）。
    ///
    /// 在 `build()` 而非构造时读取：插件实例在启动时装配，那时配置尚未加载。
    fn caps_for(&self, mgr: &ConfigManager) -> AbiTargetCaps {
        let mut caps = self.caps.clone();
        if let Some(v) = mgr
            .load()
            .ok()
            .and_then(|c| c.plugins.target_parallel.get(&self.meta.id).copied())
        {
            caps.max_parallel = v.min(MAX_PARALLEL);
            // 插件本身不支持时，即便用户配置了并发度也不启用
            caps.supports_plan = caps.supports_plan && v >= 2;
        }
        caps
    }

    /// 校验并从 describe 构造目标插件（由加载器对新发现的目标能力调用）
    pub unsafe fn adopt(
        table: *const KzwrTargetAbi,
        describe: AbiDescribe,
        origin: &str,
    ) -> Result<Self, String> {
        if table.is_null() {
            return Err(format!("{origin}: 目标能力入口返回了空指针"));
        }
        let t: &'static KzwrTargetAbi = &*table;
        if t.abi != crate::plugin::abi::C_ABI_VERSION {
            return Err(format!(
                "{origin}: 目标能力表 ABI 版本为 {}，宿主支持 {}",
                t.abi,
                crate::plugin::abi::C_ABI_VERSION
            ));
        }
        let need = KzwrTargetAbi::REQUIRED_SIZE as usize;
        if (t.size as usize) < need {
            return Err(format!(
                "{origin}: 目标能力表长度 {} 小于宿主要求的必需前缀 {}",
                t.size, need
            ));
        }
        let caps = describe.target.clone().unwrap_or_default();
        let meta = PluginMeta {
            id: describe.id.clone(),
            name: describe.name.clone(),
            version: describe.version.clone(),
            kind: crate::plugin::api::PluginKind::Target,
            builtin: false,
            description: describe.description.clone(),
        };
        Ok(Self { meta, abi: t, caps, ui: describe.ui.clone() })
    }

    fn chunk_size(&self) -> usize {
        let kib = if self.caps.preferred_chunk_kib == 0 {
            1024
        } else {
            self.caps.preferred_chunk_kib as usize
        };
        (kib * 1024).clamp(64 * 1024, 16 * 1024 * 1024)
    }

    /// 构造 `target_json`（含该目标凭据，仅传给 `target_open`/`test_json`，不落日志）
    fn target_json(&self, target: &TargetConfig, user: &str, pass: &str) -> String {
        serde_json::json!({
            "id": target.id,
            "name": target.name,
            "kind": target.kind,
            "url": target.url.clone().unwrap_or_default(),
            "username": user,
            "password": pass,
            "config": {}, // 插件命名空间键值（宿主代存）在实例建立前注入的位置
        })
        .to_string()
    }

    /// 已建立实例后，从插件取最近错误并映射为 `StorageError`
    fn err_from(&self, th: *mut c_void, code: i32, what: &str) -> StorageError {
        let detail = plugin_detail(self.abi, th, format!("{}: 错误码 {}", what, code));
        if code == -2 {
            StorageError::Auth(detail)
        } else {
            StorageError::Protocol(detail)
        }
    }
}

impl CApiTarget {
    /// 实例建立后（`build` 成功），在传输回调里把错误码映射成 `StorageError`。
    fn map_err(&self, th: *mut c_void, code: i32, what: &str) -> StorageError {
        self.err_from(th, code, what)
    }
}

#[async_trait]
impl TargetPlugin for CApiTarget {
    fn meta(&self) -> PluginMeta {
        self.meta.clone()
    }
    fn ui(&self) -> Option<PluginUi> {
        self.ui.clone()
    }
    fn supports_plan(&self) -> bool {
        CApiTarget::supports_plan(self)
    }
    fn build(&self, target: &TargetConfig, mgr: &ConfigManager) -> Option<(Arc<dyn TargetStorage>, String)> {
        // 解密在调用方/mgr 内完成（与内置目标一致）
        let creds = mgr.target_credentials(target).ok().unwrap_or((None, None));
        let (user, pass) = (creds.0.as_deref().unwrap_or(""), creds.1.as_deref().unwrap_or(""));
        let json = self.target_json(target, user, pass);
        let c = CString::new(json).ok()?;
        let th = unsafe { (self.abi.target_open)(c.as_ptr()) };
        if th.is_null() {
            return None;
        }
        let storage = AbiTargetStorage {
            abi: self.abi,
            caps: self.caps_for(mgr),
            th: th as usize,
            name: self.meta.name.clone(),
            chunk_size: self.chunk_size(),
            target_name: target.name.clone(),
        };
        Some((Arc::new(storage), target.name.clone()))
    }
    async fn verify(&self, url: Option<&str>, user: &str, pass: &str) -> Result<String, String> {
        if let Some(test) = self.abi.test_json {
            let json = serde_json::json!({
                "url": url.unwrap_or_default(),
                "username": user,
                "password": pass,
                "config": {},
            })
            .to_string();
            let c = CString::new(json).map_err(|e| e.to_string())?;
            let p = unsafe { test(c.as_ptr()) };
            let body = take_cstring(p, self.abi).unwrap_or_default();
            if body.is_empty() {
                return Ok(url.unwrap_or_default().to_string());
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
                    return Err(e.to_string());
                }
                if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
                    return Ok(u.to_string());
                }
            }
            Ok(url.unwrap_or_default().to_string())
        } else {
            Ok(url.unwrap_or_default().to_string())
        }
    }
}

/// 目标存储：函数指针版 [`TargetStorage`]
struct AbiTargetStorage {
    abi: &'static KzwrTargetAbi,
    caps: AbiTargetCaps,
    /// 目标实例句柄，以地址 `usize` 存储（裸指针非 `Send`，`usize` 可安全跨线程传递）
    th: usize,
    name: String,
    chunk_size: usize,
    target_name: String,
}

// SAFETY: `th` 为插件句柄；线程安全由契约约定（插件自负），宿主按约定调度
unsafe impl Send for AbiTargetStorage {}
unsafe impl Sync for AbiTargetStorage {}

impl AbiTargetStorage {
    fn rel_cstr(&self, path: &Path) -> Result<CString, StorageError> {
        CString::new(path.as_os_str().to_string_lossy().as_bytes())
            .map_err(|_| StorageError::Protocol(format!("非法相对路径: {}", path.display())))
    }

    fn err_from(&self, th: *mut c_void, code: i32, what: &str) -> StorageError {
        let detail = plugin_detail(self.abi, th, format!("{}: 错误码 {}", what, code));
        if code == -2 {
            StorageError::Auth(detail)
        } else {
            StorageError::Protocol(detail)
        }
    }

    /// 在阻塞线程里完成一次完整写入：open → 推块 → end → 字节复查。
    fn write_blocking(
        abi: &'static KzwrTargetAbi,
        th: usize,
        rel: CString,
        rx: tokio::sync::mpsc::Receiver<Bytes>,
        chunk_size: usize,
        progress: ProgressCb,
        abort: Arc<AtomicBool>,
        last_tick: Arc<AtomicU64>,
        name: &str,
    ) -> Result<u64, StorageError> {
        let mut rx = rx;
        let th = th as *mut c_void;
        let h = unsafe { (abi.write_begin)(th, rel.as_ptr(), 0) };
        if h.is_null() {
            return Err(StorageError::Protocol(format!("{}: write_begin 失败", name)));
        }
        last_tick.store(now_epoch_ms(), Ordering::Relaxed);
        let mut fed: u64 = 0;
        let started = std::time::Instant::now();
        loop {
            if abort.load(Ordering::Relaxed) {
                if let Some(a) = abi.write_abort {
                    unsafe { a(th, h) };
                }
                return Err(StorageError::Protocol(format!(
                    "{}: 传输看门狗超时（>120s 无推进）",
                    name
                )));
            }
            let chunk = match rx.blocking_recv() {
                Some(c) => c,
                None => break,
            };
            last_tick.store(now_epoch_ms(), Ordering::Relaxed);
            let n = unsafe { (abi.write_chunk)(th, h, chunk.as_ptr(), chunk.len() as u32) };
            if n < 0 {
                let d = plugin_detail(abi, th, format!("write_chunk 错误码 {}", n));
                return Err(StorageError::Protocol(d));
            }
            if n as usize != chunk.len() {
                return Err(StorageError::Protocol(format!(
                    "{}: write_chunk 只写入 {} 字节，调用给了 {} 字节",
                    name,
                    n,
                    chunk.len()
                )));
            }
            fed += n as u64;
            progress(fed, 0, started.elapsed().as_millis() as u64);
            let _ = chunk_size;
        }
        // write_end + 字节复查
        let total = unsafe { (abi.write_end)(th, h) };
        if total < 0 {
            let d = plugin_detail(abi, th, format!("write_end 错误码 {}", total));
            return Err(StorageError::Protocol(d));
        }
        let actual = total as u64;
        if actual != fed {
            return Err(StorageError::Protocol(format!(
                "{}: 插件上报实写 {} 字节，但宿主喂出 {} 字节（可能被截断/篡改）",
                name, actual, fed
            )));
        }
        Ok(fed)
    }

    /// 读取（用于恢复）：`read_begin`/`read_chunk` 在阻塞线程推进，经 channel 变回异步流。
    fn read_blocking(
        abi: &'static KzwrTargetAbi,
        th: usize,
        rel: CString,
        chunk_size: usize,
        name: String,
    ) -> tokio::sync::mpsc::Receiver<StorageResult<Bytes>> {
        let (tx, rx) = tokio::sync::mpsc::channel::<StorageResult<Bytes>>(8);
        std::thread::spawn(move || {
            let th = th as *mut c_void;
            let h = unsafe { (abi.read_begin)(th, rel.as_ptr()) };
            if h.is_null() {
                let _ = tx.blocking_send(Err(StorageError::NotFound(format!("{}: 打开失败", name))));
                return;
            }
            let mut buf = vec![0u8; chunk_size];
            loop {
                let n = unsafe { (abi.read_chunk)(th, h, buf.as_mut_ptr(), buf.len() as u32) };
                if n > 0 {
                    let b = Bytes::copy_from_slice(&buf[..n as usize]);
                    if tx.blocking_send(Ok(b)).is_err() {
                        break;
                    }
                } else if n == 0 {
                    break; // EOF：关闭通道即结束流
                } else {
                    let d = plugin_detail(abi, th, format!("read_chunk 错误码 {}", n));
                    let _ = tx.blocking_send(Err(StorageError::Protocol(d)));
                    break;
                }
            }
            if let Some(e) = abi.read_end {
                unsafe { e(th, h) };
            }
        });
        rx
    }
}

#[async_trait]
impl TargetStorage for AbiTargetStorage {
    async fn write_stream(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
    ) -> StorageResult<()> {
        let noop: ProgressCb = Arc::new(|_, _, _| {});
        self.write_stream_progress(path, stream, noop).await
    }

    async fn write_stream_progress(
        &self,
        path: &Path,
        stream: Box<dyn Stream<Item = Bytes> + Send + Unpin>,
        progress: ProgressCb,
    ) -> StorageResult<()> {
        let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(8);
        let producer = async move {
            futures::pin_mut!(stream);
            while let Some(chunk) = stream.next().await {
                if tx.send(chunk).await.is_err() {
                    break;
                }
            }
        };

        let abi = self.abi;
        let th = self.th;
        let rel = self.rel_cstr(path)?;
        let chunk_size = self.chunk_size;
        let name = self.name.clone();

        // 看门狗共享状态：块间隙超过 120s → 置 abort
        let abort = Arc::new(AtomicBool::new(false));
        let last_tick = Arc::new(AtomicU64::new(now_epoch_ms()));
        let wd_abort = abort.clone();
        let wd_last = last_tick.clone();
        let watchdog = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if now_epoch_ms().saturating_sub(wd_last.load(Ordering::Relaxed)) > 120_000 {
                    wd_abort.store(true, Ordering::Relaxed);
                    break;
                }
            }
        });

        let b_abort = abort.clone();
        let b_tick = last_tick.clone();
        let b_name = name.clone();
        let blocking_fut = tokio::task::spawn_blocking(move || {
            Self::write_blocking(abi, th, rel, rx, chunk_size, progress, b_abort, b_tick, &b_name)
        });

        // 同时推进 producer 与 blocking，结果取 blocking；producer 自然结束会关闭通道
        let pd = tokio::spawn(async move { producer.await });
        let out = blocking_fut.await;
        watchdog.abort();
        pd.abort();
        out.map_err(|e| StorageError::Other(e.to_string()))??;
        Ok(())
    }

    async fn read_stream(
        &self,
        path: &Path,
    ) -> StorageResult<Box<dyn Stream<Item = StorageResult<Bytes>> + Send + Unpin>> {
        let rel = self.rel_cstr(path)?;
        let rx = Self::read_blocking(self.abi, self.th, rel, self.chunk_size, self.name.clone());
        let s = futures::stream::unfold(rx, |mut rx| async move {
            match rx.recv().await {
                Some(v) => Some((v, rx)),
                None => None,
            }
        });
        Ok(Box::new(Box::pin(s)))
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<FileDescriptor>> {
        let abi = self.abi;
        let th = self.th;
        let c = CString::new(prefix).map_err(|_| StorageError::Protocol("非法前缀".into()))?;
        // (abi.list_json) 返回值是托管字符串指针（非 Send），在闭包内转成 String 再跨线程返回
        let body = tokio::task::spawn_blocking(move || {
            let th = th as *mut c_void;
            let p = unsafe { (abi.list_json)(th, c.as_ptr()) };
            take_cstring(p, abi).unwrap_or_default()
        })
        .await
        .map_err(|e| StorageError::Other(e.to_string()))?;
        let v: Vec<AbiEntry> = serde_json::from_str(&body).unwrap_or_default();
        Ok(v.into_iter().map(|e| e.into_fd()).collect())
    }

    async fn delete(&self, path: &Path) -> StorageResult<()> {
        let abi = self.abi;
        let th = self.th;
        let rel = self.rel_cstr(path)?;
        let code = tokio::task::spawn_blocking(move || {
            let th = th as *mut c_void;
            unsafe { (abi.delete)(th, rel.as_ptr()) }
        })
        .await
        .map_err(|e| StorageError::Other(e.to_string()))?;
        if code != 0 {
            return Err(self.err_from(self.th as *mut c_void, code, "delete"));
        }
        Ok(())
    }

    async fn ensure_dir(&self, path: &Path) -> StorageResult<()> {
        if let Some(ed) = self.abi.ensure_dir {
            let th = self.th;
            let rel = self.rel_cstr(path)?;
            let code = tokio::task::spawn_blocking(move || {
                let th = th as *mut c_void;
                unsafe { ed(th, rel.as_ptr()) }
            })
            .await
            .map_err(|e| StorageError::Other(e.to_string()))?;
            if code != 0 {
                return Err(self.err_from(self.th as *mut c_void, code, "ensure_dir"));
            }
        }
        Ok(())
    }

    async fn ping(&self) -> StorageResult<()> {
        if let Some(p) = self.abi.ping {
            let th = self.th;
            let code = tokio::task::spawn_blocking(move || {
                let th = th as *mut c_void;
                unsafe { p(th) }
            })
            .await
            .map_err(|e| StorageError::Other(e.to_string()))?;
            if code != 0 {
                return Err(self.err_from(self.th as *mut c_void, code, "ping"));
            }
        }
        Ok(())
    }

    fn plan_upload(&self) -> Option<&dyn PlanUpload> {
        // 启用并发回传的三要件：插件声明支持、实现了 plan_begin/plan_next、
        // 且并发度 ≥2（并发度来自本插件的用户配置，见 `CApiTarget::caps_for`）。
        if self.caps.supports_plan
            && self.caps.max_parallel >= 2
            && self.abi.plan_begin.is_some()
            && self.abi.plan_next.is_some()
        {
            Some(self)
        } else {
            None
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 并发回传（计划式）：把「传哪些、一次传几批」的决定权交给目标插件
// ════════════════════════════════════════════════════════════════════════════

impl PlanUpload for AbiTargetStorage {
    fn max_parallel(&self) -> usize {
        if self.caps.max_parallel == 0 {
            4
        } else {
            self.caps.max_parallel as usize
        }
    }

    fn begin(&self, manifest: &[FileDescriptor]) -> StorageResult<Box<dyn PlanSession>> {
        let begin = match (self.abi.plan_begin, self.abi.plan_next) {
            (Some(b), Some(_)) => b,
            _ => {
                return Err(StorageError::Protocol(format!(
                    "{}: 目标声明 supports_plan 但未实现 plan_begin/plan_next",
                    self.name
                )))
            }
        };
        // 清单：只给待传文件（目录不传），带 size/mtime 供插件做批次规划
        let items: Vec<serde_json::Value> = manifest
            .iter()
            .filter(|fd| !fd.is_dir)
            .map(|fd| {
                let mtime = fd
                    .modified
                    .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                serde_json::json!({
                    "rel_path": fd.rel_path,
                    "size": fd.size,
                    "mtime_secs": mtime,
                })
            })
            .collect();
        let job = serde_json::json!({ "upload": items }).to_string();
        let c =
            CString::new(job).map_err(|_| StorageError::Protocol("job_json 含非法字节".into()))?;
        let th = self.th as *mut c_void;
        // SAFETY: plan_begin 由插件导出；th 是 target_open 发放的实例句柄
        let ph = unsafe { begin(th, c.as_ptr()) };
        if ph.is_null() {
            return Err(self.err_from(th, -1, "plan_begin"));
        }
        Ok(Box::new(AbiPlanSession {
            abi: self.abi,
            th: self.th,
            // 句柄以 usize 保存：裸指针非 Send，而会话需跨 await 持有（见 PlanSession）
            ph: ph as usize,
        }))
    }
}

/// 一次规划会话：持有插件发放的计划句柄，**Drop 时自动 `plan_end`**
/// （提前 break / 出错返回也能结束规划，避免插件侧状态泄漏）
struct AbiPlanSession {
    abi: &'static KzwrTargetAbi,
    th: usize,
    ph: usize,
}

impl PlanSession for AbiPlanSession {
    fn next_batch(&mut self) -> StorageResult<Vec<String>> {
        let Some(next) = self.abi.plan_next else {
            return Ok(Vec::new());
        };
        let th = self.th as *mut c_void;
        // SAFETY: ph 由 plan_begin 发放，本会话是其唯一持有者
        let p = unsafe { next(th, self.ph as *mut c_void) };
        if p.is_null() {
            return Ok(Vec::new());
        }
        let body = take_cstring(p, self.abi).unwrap_or_default();
        if body.is_empty() {
            return Ok(Vec::new());
        }
        serde_json::from_str::<Vec<String>>(&body)
            .map_err(|e| StorageError::Protocol(format!("plan_next 返回无法解析: {e}")))
    }
}

impl Drop for AbiPlanSession {
    fn drop(&mut self) {
        if let Some(end) = self.abi.plan_end {
            let th = self.th as *mut c_void;
            // SAFETY: ph 由 plan_begin 发放，Drop 是最后一次使用
            unsafe { end(th, self.ph as *mut c_void) };
        }
    }
}

/// `list_json` 单条条目
#[derive(Debug, Default, Deserialize)]
struct AbiEntry {
    #[serde(default)]
    rel_path: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    mtime_secs: u64,
    #[serde(default)]
    is_dir: bool,
}

impl AbiEntry {
    fn into_fd(self) -> FileDescriptor {
        FileDescriptor {
            rel_path: self.rel_path,
            size: self.size,
            modified: (self.mtime_secs > 0)
                .then(|| SystemTime::UNIX_EPOCH + Duration::from_secs(self.mtime_secs)),
            is_dir: self.is_dir,
            digest: None,
        }
    }
}
