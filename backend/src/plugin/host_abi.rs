//! **宿主能力表实现**：插件 → 宿主回调（[`KzwrHostAbi`] 的宿主侧）
//!
//! 契约定义在 [`super::abi`]，这里只做实现。三条硬纪律（务必与 `abi.rs` 文档一致）：
//!
//! 1. **写操作入队**：插件的回调跑在 `spawn_blocking` 线程、甚至插件自己的 tokio
//!    runtime 里；而宿主的告警链路内部含 `tokio::spawn` → 在非 runtime 上下文直接调用
//!    会 panic「must be called from the context of a Tokio runtime」。故
//!    `log`/`audit`/`alert`/`resolve`/`progress`/`config_set`/`schedule` 一律只做一次
//!    `try_send`，由**唯一的**消费任务落到宿主各汇点。队列满 ⇒ 丢弃并计数（绝不打回插件线程）。
//! 2. **`config_get` 是唯一同步读**，且**绝不取宿主配置锁**：宿主存在「持配置锁跨 FFI」
//!    的既有调用路径（`/api/plugins` 就是先 `state.config.lock()` 再 `describe()`，
//!    而 `describe()` 会进插件 `available_json`）。若 `config_get` 也去拿同一把锁，
//!    插件在 `available_json` 里读自己的配置就会**自死锁**。因此每个 ctx 自带一份
//!    **只读镜像** + 一份**待落盘覆盖层**（写完立刻读能拿到自己的值）。
//! 3. **ctx 把能力限定到本插件**：所有命名空间都由 ctx 决定，参数里没有插件 id 可填。
//!    ctx 在插件被禁用/卸载时**失效**（`revoked`），之后的调用被静默丢弃。
//!
//! ## 生命周期与指针有效性
//! ctx 由 [`HostEffects::issue`] 以 `Arc::into_raw` 交出，进程内**永不释放**
//! （宿主从不 `dlclose` 插件，禁用只做逻辑摘除）。因此即便插件在失效后仍从工作线程
//! 调用，指针也不会悬空；`revoked` 只决定「效果是否被采纳」。这是有意的取舍：
//! 用一次进程生命周期的少量泄漏，换掉一整类 use-after-free。

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use chrono::Local;
use tokio::sync::{mpsc, oneshot};

use super::abi::{
    HostAlertFn, HostAuditFn, HostCfgGetFn, HostCfgSetFn, HostLogFn, HostProgressFn, HostResolveFn,
    HostScheduleFn, HostSealFn, HostUnsealFn, KzwrHostAbi, HOST_ABI_VERSION,
};

/// 队列容量：一次 `try_send` 的背压上限。插件爆发式上报时**宁可丢观测、不可拖慢插件**。
const QUEUE_CAP: usize = 4096;

/// 单条消息的最大字节数（超出按字符边界截断）
const MAX_MSG_BYTES: usize = 4 * 1024;

/// 单个配置值的最大字节数
const MAX_VALUE_BYTES: usize = 8 * 1024;

/// 每插件每秒最多受理的回调次数（固定窗口，够用且无锁竞争热点）
const RATE_PER_SEC: u32 = 512;

/// 定时器轮询粒度（秒）
const TIMER_TICK_SECS: u64 = 30;

/// 消费任务的类型别名（供 [`HostEffects::spawn_consumer`] 文档引用）
type EffectRx = mpsc::Receiver<Effect>;

// ── 队列句柄 ────────────────────────────────────────────────────────────

/// 入队句柄（每个 ctx 各持一份 clone，避免 `Ctx` ↔ `HostEffects` 形成引用环）
#[derive(Clone)]
struct Queue {
    tx: mpsc::Sender<Effect>,
    /// 因队列满/已关闭而丢弃的回调次数（消费任务周期性地汇报并清零）
    dropped: Arc<AtomicU64>,
}

impl Queue {
    /// 入队；返回 false = 被丢弃（满/关闭），调用方按「静默失败」处理
    fn push(&self, eff: Effect) -> bool {
        match self.tx.try_send(eff) {
            Ok(()) => true,
            Err(_) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }
}

// ── ctx ────────────────────────────────────────────────────────────────

/// 每插件的上下文（不透明句柄 `ctx` 的真实类型）
struct Ctx {
    plugin_id: String,
    queue: Queue,
    /// 本插件私有数据目录（`$TRIM_PKGVAR/plugins/<id>`，签发时已 `mkdir`）
    ///
    /// **插件配置的正式存放位置**：宿主不再代存插件配置（ADR-021）。
    own_dir: String,
    /// 失效标记：插件被禁用/卸载后置位，之后的回调效果一律不采纳
    revoked: AtomicBool,
    /// 限流窗口（(epoch 秒, 本窗口已受理数)）
    gate: Mutex<(u64, u32)>,
}

impl Ctx {
    fn id(&self) -> &str {
        &self.plugin_id
    }

    fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Relaxed)
    }

    /// 固定窗口限流：允许则返回 true
    fn allow(&self) -> bool {
        let now = now_secs();
        let mut g = self.gate.lock().unwrap();
        if g.0 != now {
            *g = (now, 0);
        }
        if g.1 >= RATE_PER_SEC {
            return false;
        }
        g.1 += 1;
        true
    }
}

// ── 效果 ────────────────────────────────────────────────────────────────

/// 待宿主落地的副作用（插件线程只入队，消费任务执行）
enum Effect {
    Log {
        ctx: Arc<Ctx>,
        level: i32,
        msg: String,
    },
    Audit {
        ctx: Arc<Ctx>,
        action: String,
        detail: String,
        ok: bool,
    },
    Alert {
        ctx: Arc<Ctx>,
        level: i32,
        msg: String,
    },
    Resolve {
        ctx: Arc<Ctx>,
        prefix: String,
    },
    Progress {
        ctx: Arc<Ctx>,
        label: String,
        done: u64,
        total: u64,
        detail: String,
    },
    Schedule {
        ctx: Arc<Ctx>,
        kind: String,
        cron: String,
    },
    /// **排空屏障**：消费任务处理到它时回一个信号，供宿主等待「同一次请求内产生的
    /// 告警/审计已可见」。
    Flush(oneshot::Sender<()>),
}

// ── 定时器 ──────────────────────────────────────────────────────────────

/// 一个已注册的插件定时器
#[derive(Clone)]
struct TimerEntry {
    cron: String,
    next: chrono::DateTime<Local>,
    plugin_id: String,
    kind: String,
}

// ── 宿主能力表 ──────────────────────────────────────────────────────────

/// 宿主侧的能力表实现与所有 ctx 的登记处
///
/// 由 `AppState::host_effects` 持有；FFI 入口只经由各 `Ctx` 访问队列与配置镜像。
pub struct HostEffects {
    queue: Queue,
    /// 下发（并永久持有）的能力表
    table: &'static KzwrHostAbi,
    /// ctx 登记表：`ctx 指针 → (插件 id, ctx)`。**只增不减**（见模块头「指针有效性」）
    contexts: RwLock<HashMap<usize, (String, Arc<Ctx>)>>,
    /// 插件定时器：`(插件 id, kind) → 定时器`（同键重复注册 = 覆盖）
    timers: Mutex<HashMap<(String, String), TimerEntry>>,
    /// 插件私有数据目录的根（`$TRIM_PKGVAR/plugins`）
    plugins_dir: PathBuf,
    /// 消费端接收器（由 [`HostEffects::spawn_consumer`] 取走；取走后为 `None`）
    rx: Mutex<Option<EffectRx>>,
}

impl HostEffects {
    /// 创建（`plugins_dir` 通常为 `$TRIM_PKGVAR/plugins`）
    ///
    /// 注意：队列的消费端**必须先经 [`spawn_consumer`](Self::spawn_consumer) 取走**，
    /// 否则入队的效果无人落地（`try_send` 在接收器存活时仍会成功，效果会被静默丢弃）。
    pub fn new(plugins_dir: PathBuf) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(QUEUE_CAP);
        let table = host_table();
        Arc::new(Self {
            queue: Queue {
                tx,
                dropped: Arc::new(AtomicU64::new(0)),
            },
            table,
            contexts: RwLock::new(HashMap::new()),
            timers: Mutex::new(HashMap::new()),
            plugins_dir,
            rx: Mutex::new(Some(rx)),
        })
    }

    /// 下发给插件的静态能力表（进程生命周期内有效，插件可长期持有）
    pub fn table(&self) -> *const KzwrHostAbi {
        self.table
    }

    /// 签发 ctx 并登记（重复签发同一插件会先失效旧 ctx）
    ///
    /// 返回的不透明指针交给插件，在其 `host_bind` 回调之后长期有效。
    pub fn issue(&self, plugin_id: &str) -> *mut c_void {
        // 先失效旧 ctx：热重载/重绑时旧的一律不再生效
        self.revoke(plugin_id);

        let own_dir = self.plugins_dir.join(plugin_id);
        // 目录创建失败不致命：插件拿到路径后自己也会 mkdir，或自行退到临时目录
        if let Err(e) = std::fs::create_dir_all(&own_dir) {
            tracing::warn!(plugin = %plugin_id, err = %e, "创建插件私有数据目录失败");
        }

        let ctx = Arc::new(Ctx {
            plugin_id: plugin_id.to_string(),
            queue: self.queue.clone(),
            own_dir: own_dir.to_string_lossy().into_owned(),
            revoked: AtomicBool::new(false),
            gate: Mutex::new((now_secs(), 0)),
        });
        let raw = Arc::into_raw(ctx.clone()) as *mut c_void;
        self.contexts
            .write()
            .unwrap()
            .insert(raw as usize, (plugin_id.to_string(), ctx));
        raw
    }

    /// 失效某插件的全部 ctx，并清掉它的定时器
    ///
    /// 用于禁用 / 卸载 / 卸载清除。**不释放 ctx 内存**（插件线程可能仍持有指针），
    /// 只让后续效果不被采纳。
    pub fn revoke(&self, plugin_id: &str) {
        let mut n = 0usize;
        for (_, (id, ctx)) in self.contexts.read().unwrap().iter() {
            if id == plugin_id {
                ctx.revoked.store(true, Ordering::Relaxed);
                n += 1;
            }
        }
        let timers = {
            let mut t = self.timers.lock().unwrap();
            let before = t.len();
            t.retain(|(pid, _), _| pid != plugin_id);
            before - t.len()
        };
        if n > 0 || timers > 0 {
            // 便于运维确认「禁用确实让插件能力失效」，而不是只看路由 404
            tracing::info!(plugin = %plugin_id, ctx = n, timers, "插件能力表 ctx 已失效");
        }
    }

    /// 已登记 ctx 的插件 id（去重）
    ///
    /// 插件被卸载后已从注册表消失，但 ctx 仍在登记表里；热重载时需要按这些 id
    /// 逐个 `revoke`，否则残留 ctx 会在插件重新加载前保持"有效"。
    pub fn known_plugin_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .contexts
            .read()
            .unwrap()
            .values()
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// **排空屏障**：等到此前入队的效果都被消费任务处理完（或超时）
    ///
    /// 让「插件在一次动作里上报的告警/审计」在同一个 HTTP 响应里就可见。
    pub async fn flush(&self, timeout: std::time::Duration) {
        let (tx, rx) = oneshot::channel();
        if !self.queue.push(Effect::Flush(tx)) {
            return;
        }
        let _ = tokio::time::timeout(timeout, rx).await;
    }

    /// 启动**唯一的**消费任务：把插件回调产生的效果落到宿主各汇点
    ///
    /// 必须在 tokio runtime 内调用（`main.rs` 在 `AppState` 建好后立即调用），
    /// 且只调用一次 —— 重复调用会因接收器已被取走而空转返回。
    pub fn spawn_consumer(self: &Arc<Self>, state: crate::AppState) {
        let Some(mut rx) = self.rx.lock().unwrap().take() else {
            tracing::warn!("插件能力表消费任务已启动过，忽略重复调用");
            return;
        };
        tokio::spawn(async move {
            while let Some(eff) = rx.recv().await {
                match eff {
                    Effect::Flush(ack) => {
                        let _ = ack.send(());
                    }
                    other => {
                        // 消费任务自身绝不能被单个坏效果拖垮：每个效果独立兜异常
                        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                            apply(&state, other);
                        }));
                    }
                }
            }
        });
    }

    /// 启动定时器轮询：到点回调插件的 `event_json("timer", cfg)`
    pub fn spawn_timer_loop(self: &Arc<Self>, state: crate::AppState) {
        let effects = self.clone();
        tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval(std::time::Duration::from_secs(TIMER_TICK_SECS));
            loop {
                ticker.tick().await;
                for entry in effects.due_timers() {
                    let Some(p) = state.plugins.enhance_plugin_enabled(&entry.plugin_id) else {
                        // 插件已被禁用/卸载：注册时已撤销，这里只是兜底
                        effects.remove_timer(&entry.plugin_id, &entry.kind);
                        continue;
                    };
                    p.timer(&state, &entry.kind).await;
                    effects.advance_timer(&entry);
                }
            }
        });
    }

    /// 取出所有已到期的定时器（快照；不持锁跨 await）
    fn due_timers(&self) -> Vec<TimerEntry> {
        let now = Local::now();
        self.timers
            .lock()
            .unwrap()
            .values()
            .filter(|t| t.next <= now)
            .cloned()
            .collect()
    }

    /// 把定时器推进到下一个触发点（cron 非法则移除）
    fn advance_timer(&self, entry: &TimerEntry) {
        match first_after(&entry.cron, Local::now()) {
            Some(next) => {
                if let Some(t) = self
                    .timers
                    .lock()
                    .unwrap()
                    .get_mut(&(entry.plugin_id.clone(), entry.kind.clone()))
                {
                    t.next = next;
                }
            }
            None => self.remove_timer(&entry.plugin_id, &entry.kind),
        }
    }

    fn remove_timer(&self, plugin_id: &str, kind: &str) {
        self.timers
            .lock()
            .unwrap()
            .remove(&(plugin_id.to_string(), kind.to_string()));
    }

    /// 注册一个定时器（消费任务调用）
    fn register_timer(&self, plugin_id: &str, kind: &str, cron: &str) -> bool {
        match first_after(cron, Local::now()) {
            Some(next) => {
                self.timers.lock().unwrap().insert(
                    (plugin_id.to_string(), kind.to_string()),
                    TimerEntry {
                        cron: cron.to_string(),
                        next,
                        plugin_id: plugin_id.to_string(),
                        kind: kind.to_string(),
                    },
                );
                true
            }
            None => {
                tracing::warn!(plugin = %plugin_id, kind = %kind, cron = %cron, "插件注册的 cron 非法，已拒绝");
                false
            }
        }
    }

    /// 自上次汇报以来被丢弃的回调数（并清零）
    pub fn take_dropped(&self) -> u64 {
        self.queue.dropped.swap(0, Ordering::Relaxed)
    }
}

/// cron 在 `after` 之后的首次触发（宿主本地时区；与备份调度器同一套解释）
fn first_after(expr: &str, after: chrono::DateTime<Local>) -> Option<chrono::DateTime<Local>> {
    let expr = expr.trim();
    if expr.is_empty() {
        return None;
    }
    croner::Cron::new(expr).parse().ok()?.iter_after(after).next()
}

// ── 静态能力表构造 ──────────────────────────────────────────────────────

/// 宿主版本串（静态内存；插件**不得**释放）
extern "C" fn host_version_cstr() -> *const c_char {
    static V: OnceLock<CString> = OnceLock::new();
    V.get_or_init(|| {
        CString::new(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| CString::new("unknown").unwrap())
    })
    .as_ptr()
}

/// 构造（并泄漏）能力表：必须 `'static`，因为插件会长期持有该指针
fn host_table() -> &'static KzwrHostAbi {
    let t = KzwrHostAbi {
        abi: HOST_ABI_VERSION,
        size: KzwrHostAbi::TABLE_SIZE,
        free_str: ffi_free_str,
        log: Some(ffi_log as HostLogFn),
        audit: Some(ffi_audit as HostAuditFn),
        alert: Some(ffi_alert as HostAlertFn),
        resolve_alerts: Some(ffi_resolve as HostResolveFn),
        config_get: Some(ffi_config_get as HostCfgGetFn),
        config_set: Some(ffi_config_set as HostCfgSetFn),
        host_version: Some(host_version_cstr),
        now_ms: Some(now_ms),
        own_data_dir: Some(ffi_own_data_dir),
        progress: Some(ffi_progress as HostProgressFn),
        schedule: Some(ffi_schedule as HostScheduleFn),
        // 加密原语：宿主不再代存插件配置，但密钥仍留在宿主手里
        seal: Some(ffi_seal as HostSealFn),
        unseal: Some(ffi_unseal as HostUnsealFn),
    };
    Box::leak(Box::new(t))
}

// ── 消费任务：效果落地 ──────────────────────────────────────────────────

/// 执行一个效果（在消费任务线程上；可安全使用 `tokio::spawn` 等宿主设施）
fn apply(state: &crate::AppState, eff: Effect) {
    match eff {
        Effect::Log { ctx, level, msg } => {
            if ctx.is_revoked() {
                return;
            }
            // 走宿主的 tracing 管线 ⇒ 与核心日志同一份 `app.log`、同一套轮转
            let id = ctx.id();
            match level {
                0 => tracing::trace!(plugin = %id, "{}", msg),
                1 => tracing::debug!(plugin = %id, "{}", msg),
                3 => tracing::warn!(plugin = %id, "{}", msg),
                4 => tracing::error!(plugin = %id, "{}", msg),
                _ => tracing::info!(plugin = %id, "{}", msg),
            }
        }
        Effect::Audit {
            ctx,
            action,
            detail,
            ok,
        } => {
            if ctx.is_revoked() {
                return;
            }
            // `action` 自带插件命名空间（如 `kzwr.trash.auto`），核心不猜语义
            state.audit.record(&action, detail, ok, None);
        }
        Effect::Alert { ctx, level, msg } => {
            if ctx.is_revoked() {
                return;
            }
            let level = if level == 0 {
                crate::domain::alerts::AlertLevel::Error
            } else {
                crate::domain::alerts::AlertLevel::Warn
            };
            // 与声明式通道**同一去重规则**（来源为插件、消息相同即视为重复），
            // 因此两条通道混用也不会产生重复告警。
            crate::http::routes::raise_alert_once(
                state,
                level,
                crate::domain::alerts::AlertSource::Plugin(ctx.id().to_string()),
                msg,
            );
        }
        Effect::Resolve { ctx, prefix } => {
            if ctx.is_revoked() {
                return;
            }
            let id = ctx.id().to_string();
            let removed = state.alerts.remove_where(|a| {
                a.source == crate::domain::alerts::AlertSource::Plugin(id.clone())
                    && a.message.starts_with(prefix.as_str())
            });
            if removed > 0 {
                tracing::info!(plugin = %id, removed, "插件经能力表消解的告警");
            }
        }
        Effect::Progress {
            ctx,
            label,
            done,
            total,
            detail,
        } => {
            if ctx.is_revoked() {
                return;
            }
            publish_progress(state, &ctx, &label, done, total, &detail);
        }
        Effect::Schedule { ctx, kind, cron } => {
            if ctx.is_revoked() {
                return;
            }
            let id = ctx.id().to_string();
            // 定时器登记在 HostEffects 上，经 ctx 的队列句柄无法反查 ⇒ 由宿主单例处理：
            // 见 `HostEffects::register_timer`（消费任务持有 &AppState，从中取单例）
            state.host_effects.register_timer(&id, &kind, &cron);
        }
        Effect::Flush(ack) => {
            let _ = ack.send(());
        }
    }
}

/// 把进度转发到 WebSocket 事件流（`kind = "plugin"`）
///
/// 前端会忽略插件事件对顶部「任务卡」的覆盖（见 `App.svelte`），
/// 因此插件进度不会劫持备份/恢复的实时状态。
fn publish_progress(
    state: &crate::AppState,
    ctx: &Ctx,
    label: &str,
    done: u64,
    total: u64,
    detail: &str,
) {
    let message = if detail.is_empty() {
        label.to_string()
    } else if label.is_empty() {
        detail.to_string()
    } else {
        format!("{label}：{detail}")
    };
    state.eventbus.publish(crate::eventbus::DomainEvent {
        kind: crate::eventbus::TaskKind::Plugin,
        status: crate::eventbus::TaskStatus::Progress,
        phase: None,
        job_id: format!("plugin:{}", ctx.id()),
        current_file: if label.is_empty() {
            None
        } else {
            Some(label.to_string())
        },
        done,
        total,
        bytes_done: 0,
        bytes_total: 0,
        elapsed_ms: 0,
        speed: 0,
        message: Some(message),
        ts: now_ms(),
    });
}

// ── FFI 入口（每个都 catch_unwind；绝不让 panic 越过 C 边界）──────────────

/// 取回 ctx 引用（NULL → None）。`revoked` 的判定留给消费/读取方，
/// 因为 `config_get` 等只读入口也统一在这里做空指针防护。
#[inline]
unsafe fn ctx_ref(p: *mut c_void) -> Option<&'static Ctx> {
    if p.is_null() {
        return None;
    }
    Some(&*(p as *const Ctx))
}

/// C 字符串 → `String`（NULL → None；非法 UTF-8 走**有损转换**，不 panic）
#[inline]
unsafe fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    Some(CStr::from_ptr(p).to_string_lossy().into_owned())
}

/// 按**字符边界**截断到不超过 `max` 字节（避免切断多字节字符）
fn clamp(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// 统一包裹：捕获 panic 并退到 `default`
fn guard<T>(default: T, f: impl FnOnce() -> T) -> T {
    std::panic::catch_unwind(AssertUnwindSafe(f)).unwrap_or(default)
}

/// 写入宿主运行日志（level：0 trace / 1 debug / 2 info / 3 warn / 4 error）
extern "C" fn ffi_log(ctx: *mut c_void, level: c_int, msg: *const c_char) {
    guard((), || unsafe {
        let (Some(c), Some(m)) = (ctx_ref(ctx), cstr(msg)) else {
            return;
        };
        if c.is_revoked() || !c.allow() {
            return;
        }
        let m = clamp(&m, MAX_MSG_BYTES);
        if m.is_empty() {
            return;
        }
        let eff = Effect::Log {
            ctx: ctx_arc(c),
            level: level as i32,
            msg: m,
        };
        c.queue.push(eff);
    });
}

/// 写入审计日志（`ok != 0` = 成功）
extern "C" fn ffi_audit(ctx: *mut c_void, action: *const c_char, detail: *const c_char, ok: c_int) {
    guard((), || unsafe {
        let (Some(c), Some(a)) = (ctx_ref(ctx), cstr(action)) else {
            return;
        };
        let d = cstr(detail).unwrap_or_default();
        if c.is_revoked() || !c.allow() {
            return;
        }
        let a = clamp(&a, 128);
        if a.is_empty() {
            return;
        }
        let eff = Effect::Audit {
            ctx: ctx_arc(c),
            action: a,
            detail: clamp(&d, MAX_MSG_BYTES),
            ok: ok != 0,
        };
        c.queue.push(eff);
    });
}

/// 上报一条告警（level：0 error / 1 warn）
extern "C" fn ffi_alert(ctx: *mut c_void, level: c_int, msg: *const c_char) {
    guard((), || unsafe {
        let (Some(c), Some(m)) = (ctx_ref(ctx), cstr(msg)) else {
            return;
        };
        if c.is_revoked() || !c.allow() {
            return;
        }
        let m = clamp(&m, MAX_MSG_BYTES);
        if m.is_empty() {
            return;
        }
        let eff = Effect::Alert {
            ctx: ctx_arc(c),
            level: level as i32,
            msg: m,
        };
        c.queue.push(eff);
    });
}

/// 消解本插件此前上报的告警（按消息前缀）
extern "C" fn ffi_resolve(ctx: *mut c_void, prefix: *const c_char) {
    guard((), || unsafe {
        let (Some(c), Some(p)) = (ctx_ref(ctx), cstr(prefix)) else {
            return;
        };
        if c.is_revoked() || !c.allow() || p.is_empty() {
            return;
        }
        let eff = Effect::Resolve {
            ctx: ctx_arc(c),
            prefix: clamp(&p, MAX_MSG_BYTES),
        };
        c.queue.push(eff);
    });
}

/// ~~同步读一个配置键~~ —— **已弃用**：宿主不再代存插件配置，恒返回 NULL
///
/// 保留实现（而非置为 `None`）是为了让老插件的调用**安全失败**：
/// 拿到 NULL 后按「无此配置」处理，而不是因为函数指针为 NULL 而崩溃。
extern "C" fn ffi_config_get(_ctx: *mut c_void, _key: *const c_char) -> *mut c_char {
    std::ptr::null_mut()
}

/// ~~写/删一个配置键~~ —— **已弃用**：恒拒绝（非 0）
///
/// 老插件据此得知「宿主不再代存」，应改为把配置写进自己的 `own_data_dir`
/// （敏感内容用 [`ffi_seal`] 加密）。
extern "C" fn ffi_config_set(_ctx: *mut c_void, _key: *const c_char, _value: *const c_char) -> c_int {
    // 返回 2（与「已失效」同码）而不是 1（入参错误）：插件据此可区分
    // 「我传错了」与「宿主不支持了」。
    2
}

/// 用宿主密钥加密明文（返回 base64 密文；NULL = 失败）
///
/// 插件拿不到密钥本身，只能要求宿主加解密 —— 这样「配置自管」不会降级为明文落盘。
extern "C" fn ffi_seal(ctx: *mut c_void, plain: *const c_char) -> *mut c_char {
    guard(std::ptr::null_mut(), || unsafe {
        let (Some(c), Some(p)) = (ctx_ref(ctx), cstr(plain)) else {
            return std::ptr::null_mut();
        };
        if c.is_revoked() || !c.allow() {
            return std::ptr::null_mut();
        }
        // 限长：能力表是给「配置片段」用的，不是给大文件加密用的
        if p.len() > MAX_VALUE_BYTES * 16 {
            return std::ptr::null_mut();
        }
        match super::crypto::seal(&p) {
            Some(sealed) => CString::new(sealed)
                .map(|s| s.into_raw())
                .unwrap_or(std::ptr::null_mut()),
            None => std::ptr::null_mut(),
        }
    })
}

/// 用宿主密钥解密 [`ffi_seal`] 的产物（NULL = 失败或非本宿主密钥加密）
extern "C" fn ffi_unseal(ctx: *mut c_void, sealed: *const c_char) -> *mut c_char {
    guard(std::ptr::null_mut(), || unsafe {
        let (Some(c), Some(s)) = (ctx_ref(ctx), cstr(sealed)) else {
            return std::ptr::null_mut();
        };
        if c.is_revoked() || !c.allow() {
            return std::ptr::null_mut();
        }
        // 解密失败一律 NULL（**绝不**回退成明文，见 crypto 模块说明）
        match super::crypto::unseal(&s) {
            Some(plain) => CString::new(plain)
                .map(|s| s.into_raw())
                .unwrap_or(std::ptr::null_mut()),
            None => std::ptr::null_mut(),
        }
    })
}

/// 上报进度（`total == 0` 表示总量未知）
extern "C" fn ffi_progress(
    ctx: *mut c_void,
    label: *const c_char,
    done: u64,
    total: u64,
    detail: *const c_char,
) {
    guard((), || unsafe {
        let Some(c) = ctx_ref(ctx) else {
            return;
        };
        if c.is_revoked() || !c.allow() {
            return;
        }
        let eff = Effect::Progress {
            ctx: ctx_arc(c),
            label: clamp(&cstr(label).unwrap_or_default(), 256),
            done,
            total,
            detail: clamp(&cstr(detail).unwrap_or_default(), MAX_MSG_BYTES),
        };
        c.queue.push(eff);
    });
}

/// 注册周期任务：0 = 已受理，非 0 = 拒绝（kind 为空 / cron 非法 / 队列满）
extern "C" fn ffi_schedule(ctx: *mut c_void, kind: *const c_char, cron: *const c_char) -> c_int {
    guard(1, || unsafe {
        let (Some(c), Some(k)) = (ctx_ref(ctx), cstr(kind)) else {
            return 1;
        };
        if c.is_revoked() || !c.allow() {
            return 2;
        }
        let cr = cstr(cron).unwrap_or_default();
        let k = clamp(&k, 64);
        if k.is_empty() || !cron_ok(&cr) {
            return 3;
        }
        let eff = Effect::Schedule {
            ctx: ctx_arc(c),
            kind: k,
            cron: cr,
        };
        if c.queue.push(eff) {
            0
        } else {
            4
        }
    })
}

/// 本插件私有数据目录（宿主分配；调用方须用 `free_str` 释放）
extern "C" fn ffi_own_data_dir(ctx: *mut c_void) -> *mut c_char {
    guard(std::ptr::null_mut(), || unsafe {
        let Some(c) = ctx_ref(ctx) else {
            return std::ptr::null_mut();
        };
        if c.is_revoked() {
            return std::ptr::null_mut();
        }
        CString::new(c.own_dir.clone())
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut())
    })
}

/// 释放宿主分配的字符串（只用于能力表返回的串）
extern "C" fn ffi_free_str(p: *mut c_char) {
    guard((), || {
        if p.is_null() {
            return;
        }
        unsafe {
            drop(CString::from_raw(p));
        }
    });
}

/// 当前毫秒时间戳（失败返回 -1，与契约一致）
extern "C" fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(-1)
}

/// 校验 cron（与备份调度器同一套解释；空串视为非法 —— 定时器必须有表达式）
fn cron_ok(expr: &str) -> bool {
    let e = expr.trim();
    !e.is_empty() && croner::Cron::new(e).parse().is_ok()
}

/// 把 `&'static Ctx` 变回 `Arc<Ctx>`（供效果持有所有权）
///
/// 入参来自 [`ctx_ref`] 的借用，其底层 `Arc` 强引用由 [`HostEffects::contexts`]
/// 永久持有 —— 进程内绝不释放，故 `Arc::increment_strong_count` 后 `from_raw` 是安全的。
fn ctx_arc(c: &'static Ctx) -> Arc<Ctx> {
    let p = c as *const Ctx;
    unsafe {
        Arc::increment_strong_count(p);
        Arc::from_raw(p)
    }
}

/// 当前 epoch 秒（限流窗口用）
fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_never_splits_multibyte_chars() {
        // 每个汉字 3 字节：限到 4 字节只能保留 1 个汉字（3 字节），不能切出半个字符
        let out = clamp("中文测试", 4);
        assert_eq!(out, "中");
        assert_eq!(clamp("abc", 10), "abc");
    }

    #[test]
    fn key_rule_matches_host_endpoint() {
        assert!(super::super::cabi::config_key_ok("kzwr_token"));
        assert!(!super::super::cabi::config_key_ok("a.b"), "点号必须被拒绝");
        assert!(!super::super::cabi::config_key_ok(""));
    }

    #[test]
    fn cron_ok_rejects_empty_and_garbage() {
        assert!(cron_ok("0 3 * * *"));
        assert!(!cron_ok(""));
        assert!(!cron_ok("not a cron"));
    }

    #[test]
    fn host_table_is_complete_and_versioned() {
        let t = host_table();
        assert_eq!(t.abi, HOST_ABI_VERSION);
        assert_eq!(t.size, KzwrHostAbi::TABLE_SIZE);
        // 全部能力都必须有实现（尾部追加演进时这里会提醒补齐）
        assert!(t.log.is_some() && t.audit.is_some() && t.alert.is_some());
        // config_get/set 保留为**已弃用桩**（老插件调用应安全失败，而非空指针崩溃）
        assert!(t.config_get.is_some() && t.config_set.is_some());
        assert!(t.own_data_dir.is_some() && t.free_str as usize != 0);
        // 加密原语：插件自管配置靠它避免明文落盘
        assert!(t.seal.is_some() && t.unseal.is_some(), "seal/unseal 必须实现");
        assert!(!host_version_cstr().is_null());
        assert!(now_ms() > 0);
    }

    #[test]
    fn ctx_issue_and_revoke() {
        let fx = HostEffects::new(std::env::temp_dir().join("kzwr-host-abi-test"));
        let raw = fx.issue("demo");
        let c = unsafe { ctx_ref(raw) }.expect("ctx");
        assert_eq!(c.id(), "demo");
        assert!(!c.is_revoked());
        // 私有目录应带插件 id（插件把配置写在这里）
        assert!(c.own_dir.ends_with("demo"), "own_dir 应含插件 id：{}", c.own_dir);

        fx.revoke("demo");
        assert!(c.is_revoked(), "revoke 后应标记失效");
        assert_eq!(fx.table().is_null(), false);
    }

    /// **已弃用的 config_get/config_set 必须安全失败**（而不是崩溃或静默成功）
    ///
    /// 宿主不再代存插件配置（ADR-021）：老插件若仍调用这两个入口，
    /// `config_get` 应得到 NULL（当作「无此配置」），`config_set` 应被拒绝（非 0），
    /// 从而让插件察觉「宿主不支持了」并改用 `own_data_dir`。
    #[test]
    fn deprecated_config_entries_fail_safely() {
        let fx = HostEffects::new(std::env::temp_dir().join("kzwr-host-abi-dep"));
        let raw = fx.issue("legacy");
        let key = CString::new("accounts").unwrap();
        let val = CString::new("[]").unwrap();
        // 读：恒 NULL
        assert!(
            ffi_config_get(raw, key.as_ptr()).is_null(),
            "config_get 应恒返回 NULL（宿主不再代存）"
        );
        // 写：恒拒绝（非 0）
        assert_ne!(
            ffi_config_set(raw, key.as_ptr(), val.as_ptr()),
            0,
            "config_set 应恒拒绝，让插件察觉宿主不再代存"
        );
    }

    /// **加密原语**：seal/unseal 往返，且坏输入不得回退成明文
    #[test]
    fn seal_unseal_through_ffi() {
        super::super::crypto::init(age::secrecy::SecretString::from("ffi-test".to_string()));
        let fx = HostEffects::new(std::env::temp_dir().join("kzwr-host-abi-seal"));
        let raw = fx.issue("sealer");
        let plain = CString::new("access-token-秘密").unwrap();

        let sealed = ffi_seal(raw, plain.as_ptr());
        assert!(!sealed.is_null(), "seal 应成功");
        let sealed_str = unsafe { CStr::from_ptr(sealed) }.to_string_lossy().into_owned();
        assert!(!sealed_str.contains("秘密"), "密文不得含明文片段");
        ffi_free_str(sealed);

        // 解回明文
        let c = CString::new(sealed_str).unwrap();
        let back = ffi_unseal(raw, c.as_ptr());
        assert!(!back.is_null());
        assert_eq!(
            unsafe { CStr::from_ptr(back) }.to_string_lossy(),
            "access-token-秘密"
        );
        ffi_free_str(back);

        // 坏输入 → NULL（**绝不**当明文返回）
        let junk = CString::new("plain-token").unwrap();
        assert!(ffi_unseal(raw, junk.as_ptr()).is_null(), "非密文必须被拒");
        // NULL 入参安全
        assert!(ffi_seal(std::ptr::null_mut(), plain.as_ptr()).is_null());
        assert!(ffi_unseal(raw, std::ptr::null()).is_null());
    }

    #[test]
    fn revoked_ctx_clears_timers() {
        let fx = HostEffects::new(std::env::temp_dir().join("kzwr-host-abi-test2"));
        fx.issue("p1");
        assert!(fx.register_timer("p1", "nightly", "0 3 * * *"));
        assert!(fx.register_timer("p2", "nightly", "0 4 * * *"));
        fx.revoke("p1");
        let timers = fx.timers.lock().unwrap();
        assert_eq!(timers.len(), 1, "只应留下 p2 的定时器");
        assert!(timers.contains_key(&("p2".to_string(), "nightly".to_string())));
    }

    #[test]
    fn register_timer_rejects_bad_cron() {
        let fx = HostEffects::new(std::env::temp_dir().join("kzwr-host-abi-test3"));
        assert!(!fx.register_timer("p", "k", "bogus"));
        assert!(fx.timers.lock().unwrap().is_empty());
    }

    #[test]
    fn due_timers_fire_then_advance_to_next_slot() {
        let fx = HostEffects::new(std::env::temp_dir().join("kzwr-host-abi-test8"));
        // 「每分钟」⇒ 刚注册时下一次触发在未来，故尚未到期
        assert!(fx.register_timer("p", "tick", "* * * * *"));
        assert!(
            fx.due_timers().is_empty(),
            "刚注册的定时器不应立刻到期（next 在未来）"
        );

        // 人为把它拨到过去 ⇒ 必须被判为到期，且只返回这一条
        {
            let mut t = fx.timers.lock().unwrap();
            let e = t.get_mut(&("p".to_string(), "tick".to_string())).unwrap();
            e.next = chrono::Local::now() - chrono::Duration::seconds(1);
        }
        let due = fx.due_timers();
        assert_eq!(due.len(), 1, "到期的定时器必须被取出");
        assert_eq!(due[0].kind, "tick");
        assert_eq!(due[0].plugin_id, "p");

        // 推进后应重新排到未来（不会在同一次轮询里反复触发）
        fx.advance_timer(&due[0]);
        assert!(fx.due_timers().is_empty(), "推进后不应再次到期");
        assert_eq!(fx.timers.lock().unwrap().len(), 1, "定时器应保留（周期任务）");
    }

    #[test]
    fn advance_timer_removes_entry_when_cron_becomes_unparseable() {
        let fx = HostEffects::new(std::env::temp_dir().join("kzwr-host-abi-test9"));
        assert!(fx.register_timer("p", "k", "* * * * *"));
        let mut entry = fx.timers.lock().unwrap().get(&("p".to_string(), "k".to_string())).cloned().unwrap();
        // 模拟「注册后被改坏」：推进时算不出下一次 → 应移除而不是卡死
        entry.cron = "not-a-cron".to_string();
        fx.advance_timer(&entry);
        assert!(fx.timers.lock().unwrap().is_empty(), "非法 cron 的定时器应被移除");
    }

    #[test]
    fn rate_gate_limits_bursts() {
        let fx = HostEffects::new(std::env::temp_dir().join("kzwr-host-abi-test4"));
        let raw = fx.issue("rl");
        let c = unsafe { ctx_ref(raw) }.unwrap();
        let mut ok = 0;
        for _ in 0..(RATE_PER_SEC + 50) {
            if c.allow() {
                ok += 1;
            }
        }
        assert_eq!(ok, RATE_PER_SEC, "固定窗口内应收紧到上限");
    }

    #[test]
    fn config_get_and_own_dir_survive_hostile_input() {
        let fx = HostEffects::new(std::env::temp_dir().join("kzwr-host-abi-test6"));
        let raw = fx.issue("hostile");
        // NULL ctx / NULL key 必须安全返回空指针，不得 panic
        assert!(ffi_config_get(std::ptr::null_mut(), std::ptr::null()).is_null());
        assert!(ffi_config_get(raw, std::ptr::null()).is_null());
        let missing = CString::new("nope").unwrap();
        assert!(ffi_config_get(raw, missing.as_ptr()).is_null());
        assert!(ffi_own_data_dir(std::ptr::null_mut()).is_null());

        // own_data_dir 返回宿主分配的串，必须能经 free_str 安全释放
        let p = ffi_own_data_dir(raw);
        assert!(!p.is_null());
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        assert!(s.ends_with("hostile"), "目录应带插件 id：{s}");
        ffi_free_str(p);
        ffi_free_str(std::ptr::null_mut()); // 幂等
    }

    #[test]
    fn revoked_ffi_entries_are_inert() {
        let fx = HostEffects::new(std::env::temp_dir().join("kzwr-host-abi-test7"));
        let raw = fx.issue("gone");
        fx.revoke("gone");
        let k = CString::new("k").unwrap();
        let v = CString::new("v").unwrap();
        assert_eq!(ffi_config_set(raw, k.as_ptr(), v.as_ptr()), 2, "失效 ctx 应拒绝写入");
        assert!(ffi_config_get(raw, k.as_ptr()).is_null());
        assert!(ffi_own_data_dir(raw).is_null());
        // 日志/审计/告警/进度/定时在失效后一律不入队
        ffi_log(raw, 2, v.as_ptr());
        ffi_audit(raw, v.as_ptr(), v.as_ptr(), 1);
        ffi_alert(raw, 1, v.as_ptr());
        ffi_resolve(raw, v.as_ptr());
        ffi_progress(raw, v.as_ptr(), 1, 2, v.as_ptr());
        assert_eq!(ffi_schedule(raw, v.as_ptr(), CString::new("0 3 * * *").unwrap().as_ptr()), 2);
        assert_eq!(fx.take_dropped(), 0, "失效调用不应污染丢弃计数");
    }
}
