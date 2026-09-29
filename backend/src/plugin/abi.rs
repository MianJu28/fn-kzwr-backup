//! **稳定插件 ABI v1**（外置与内置插件共用的「冻结契约」）
//!
//! ## 为什么需要它
//!
//! 直接传 Rust trait 对象虽然省事，但 Rust **没有稳定 ABI**：vtable 布局、结构体字段排布
//! 都随编译器与源码变化 → 插件必须与宿主同版本编译。本模块把跨边界契约降级为
//! **版本化的 C ABI + UTF-8 JSON**：
//!
//! - 插件只依赖这份契约（`plugins/sdk`，零第三方依赖），**宿主升级不需要重编插件**
//! - 数据一律用 JSON 字符串传递（宿主与插件各自用自己的类型解析，未知字段互相忽略）
//! - **内置插件也走这套契约**（编译进主程序，直接提供静态表）→ 内置/外置零差异
//!
//! ## 契约总览
//!
//! | 符号 | 说明 |
//! |------|------|
//! | `fn_kzwr_plugin_abi_v1` | 入口：`extern "C" fn() -> *const KzwrPluginAbi`（**唯一必需导出**） |
//! | `fn_kzwr_plugin_target_v1` | 目标能力入口（可选）：`extern "C" fn() -> *const KzwrTargetAbi` |
//!
//! 主表 [`KzwrPluginAbi`] 回调（`available`/`action`/`health`/`event` 允许为 NULL = 未实现）：
//!
//! | 回调 | 入参 | 返回 JSON |
//! |------|------|-----------|
//! | `describe_json` | — | `{"id","name","version","kind","description","caps":{…},"runtime":{…},"target":{…},"ui":{…}｜null}` |
//! | `available_json` | `cfg_json` | `{"available":bool,"reason":string｜null}` |
//! | `action_json` | `action`, `request_json` | 任意 JSON（如 `{"success":true,"message":"…"}` / `{"error":"…"}`） |
//! | `health_json` | `cfg_json` | `[{"key","title","status":"ok|warn|fail","detail","hint"}]` |
//! | `event_json` | `event`, `cfg_json` | `{"count":u64?, "alerts":[{"level","message"}]?}` |
//! | `free_str` | — | 释放插件返回的字符串（宿主必须调用） |
//! | `destroy` | — | 卸载插件自身状态（可选） |
//! | `host_bind` | `host`, `ctx` | 可选；宿主下发能力表（见 [`KzwrHostAbi`]），返回 0 = 接受 |
//!
//! `request_json`（动作入参）固定为：
//!
//! ```json
//! { "body": { …前端提交的 JSON（GET 时是 query 键值对）… }, "cfg": { …配置快照… },
//!   "method": "GET" | "POST" }
//! ```
//!
//! ## 插件如何影响宿主：两条通道并存
//!
//! 1. **声明式回传**（始终可用，无需 `host_bind`）：在 `health_json` / `event_json` /
//!    `action_json` 的返回值里带上 `alerts` / `resolve` / `audit` / `config`，由宿主统一
//!    落库（见 `cabi::apply_side_effects`）。**老插件零改动继续工作。**
//! 2. **宿主能力表回调**（可选，需实现 `host_bind`）：日志实时入 `app.log`、长任务里
//!    逐条审计/告警、同步读写自己的配置、上报进度、注册定时器。
//!
//! 两条通道写同一个落库点、同一套去重规则，所以**混用不会重复**（如同一条告警
//! 既 `return alerts` 又 `host.alert(...)` 也只会出现一次）。第 2 条通道的纪律是：
//! 写操作一律**入队**（由宿主唯一的消费任务落库）、能力按 ctx **限定到本插件**、
//! 插件被禁用/卸载后 ctx 立即**失效**。详见 [`KzwrHostAbi`] 与 `plugin/host_abi.rs`。
//!
//! `cfg.self_config` 是**本插件自己的**明文配置（隔离：只含本插件命名空间，不含其它
//! 插件的键，也不含宿主凭据）；`cfg.after_backup_task` 只在 `after_backup` 事件里有值，
//! 表示「刚才完成的是哪个任务」，插件据此套用该任务自己的回收站门槛。
//!
//! ## 约定
//!
//! - 字符串一律 **UTF-8 + NUL 结尾**；返回空指针（NULL）表示"无内容"
//! - **所有权**：插件返回的字符串归宿主，宿主必须调用该表的 `free_str` 释放
//! - **不要 panic 跨 FFI**（Rust 插件请自行 `catch_unwind`）；宿主也会兜一层 `catch_unwind`
//! - **`size` 语义 = 前缀探测**：主表 [`KzwrPluginAbi`] 采用**尾部追加 + 可选字段**演进
//!   （新字段一律 `Option`，老插件的表比宿主短 ⇒ 读不到该字段 ⇒ 按未实现处理，见
//!   [`KzwrPluginAbi::MIN_SIZE`]）。必需前缀仍只有 `describe_json` + `free_str`
//! - **`abi`**：破坏性改动才 +1（如改回调签名、改必需字段语义、删除字段）
//!
//! 同样的契约在插件侧由 `plugins/sdk` 提供（`KzwrPluginAbi` / `KzwrHostAbi` +
//! `export_plugin_v1!` / `export_target_v1!` 宏）。
//!
//! ⚠️ `plugins/sdk/src/lib.rs` 里的镜像结构体必须与本页**逐字段同序同类型**：
//! 两边是独立定义、靠 `#[repr(C)]` 对齐，漂移 = 内存踩踏。
//! `plugin::contract_tests` 会逐字段比对两侧源码，改这里请同步改 SDK。

use std::ffi::c_void;
use std::os::raw::{c_char, c_int};

use serde::{Deserialize, Serialize};

/// C ABI 版本：**仅破坏性改动 +1**；不变则插件无需随宿主升级重编译
pub const C_ABI_VERSION: u32 = 1;

/// 宿主版本（插件可用于日志/兼容判断；`cfg_json` 里也会给）
pub const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 入口符号名（插件导出，必需）
pub const SYM_ENTRY_V1: &[u8] = b"fn_kzwr_plugin_abi_v1\0";

/// 目标能力入口符号名（插件导出，可选；`describe_json.runtime.target` 可覆盖）
pub const SYM_TARGET_V1: &str = "fn_kzwr_plugin_target_v1";

/// 回调类型别名（返回 JSON 字符串，宿主用 `free_str` 释放）
pub type JsonFn0 = extern "C" fn() -> *mut c_char;
pub type JsonFn1 = extern "C" fn(*const c_char) -> *mut c_char;
pub type JsonFn2 = extern "C" fn(*const c_char, *const c_char) -> *mut c_char;

/// 宿主能力表的下发回调（插件实现，可选）
///
/// 宿主在采纳插件主表后调用一次：`host` 指向宿主提供的**静态**能力表（进程生命周期内
/// 有效、永不释放，插件可长期持有该指针），`ctx` 是宿主签发的**不透明句柄**，把之后
/// 每一次回调**限定到本插件**（插件无法指定别人的插件 id）。
/// 返回 0 = 插件接受能力表；非 0 = 插件拒绝（宿主按未绑定继续，不影响加载）。
pub type HostBindFn = extern "C" fn(*const KzwrHostAbi, *mut c_void) -> c_int;

/// 插件提供的静态函数表（`#[repr(C)]`：布局固定，**尾部追加演进**）
///
/// 只提供目标能力的插件只需实现 `describe_json` + `free_str`（其余回调留 NULL）。
#[repr(C)]
pub struct KzwrPluginAbi {
    /// 必须 == [`C_ABI_VERSION`]
    pub abi: u32,
    /// 本结构体字节大小（宿主据此探测尾部可选字段是否存在）
    pub size: u32,
    /// 插件元信息、UI 描述、能力声明（**必需**）
    pub describe_json: JsonFn0,
    /// 是否可用（依赖配置时用）
    pub available_json: Option<JsonFn1>,
    /// 动作：`/api/p/<插件id>/<action>`（`action` 可为多段，如 `trash/empty`）
    pub action_json: Option<JsonFn2>,
    /// 「一键体检」自检项
    pub health_json: Option<JsonFn1>,
    /// 生命周期事件（`startup`/`patrol`/`after_backup`/`reload`/`timer`）
    pub event_json: Option<JsonFn2>,
    /// 释放插件返回的字符串（**必需**）
    pub free_str: extern "C" fn(*mut c_char),
    /// 卸载/清理（可选；宿主删除插件前调用）
    pub destroy: Option<extern "C" fn()>,
    /// 宿主能力表下发（可选；见 [`HostBindFn`]）
    ///
    /// **尾部追加字段**：老插件的表短到这里之前 ⇒ 宿主按未实现处理，
    /// 插件继续用声明式回传通道。
    pub host_bind: Option<HostBindFn>,
}

impl KzwrPluginAbi {
    /// 当前宿主定义的完整表长（= SDK 镜像表长度；哨兵测试据此发现漂移）
    pub const TABLE_SIZE: u32 = std::mem::size_of::<Self>() as u32;

    /// **必需**前缀长度：到 `free_str` 为止（`destroy`/`host_bind` 可选）
    ///
    /// 用 `offset_of` 精确算出，是为了让「老插件表短一截」这种合法情况
    /// 仍被接受 —— 否则每次给主表加字段都会踢掉所有旧插件。
    pub const MIN_SIZE: u32 = std::mem::offset_of!(Self, free_str) as u32
        + std::mem::size_of::<extern "C" fn(*mut c_char)>() as u32;
}

// ── 宿主能力表（宿主 → 插件下发；见 [`HostBindFn`]）────────────────────────

/// 宿主能力表 ABI 版本（独立于主表 `C_ABI_VERSION` 计数）
pub const HOST_ABI_VERSION: u32 = 1;

/// 宿主能力表的回调类型别名（首个参数一律是宿主签发的 `ctx`）
/// 日志：`level` = 0 trace / 1 debug / 2 info / 3 warn / 4 error
pub type HostLogFn = extern "C" fn(*mut c_void, i32, *const c_char);
/// 审计：`action` 短标识，`detail` 描述，`ok` 非 0 = 成功
pub type HostAuditFn = extern "C" fn(*mut c_void, *const c_char, *const c_char, i32);
/// 告警：`level` = 0 error / 1 warn（与宿主 `AlertLevel` 一致）
pub type HostAlertFn = extern "C" fn(*mut c_void, i32, *const c_char);
/// 消解告警：按前缀批量消解本插件此前上报的告警
pub type HostResolveFn = extern "C" fn(*mut c_void, *const c_char);
/// 读自己的配置（返回 NULL = 不存在；非空归**插件**释放，用 `free_str`）
pub type HostCfgGetFn = extern "C" fn(*mut c_void, *const c_char) -> *mut c_char;
/// 写/删自己的配置：0 = 已受理（异部落盘），非 0 = 拒绝（非法键名/队列满/已失效）
pub type HostCfgSetFn = extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
/// 进度上报：`done/total`（total=0 表示未知），`label` 可为 NULL
pub type HostProgressFn = extern "C" fn(*mut c_void, *const c_char, u64, u64, *const c_char);
/// 注册定时器：`kind` 非空，`cron` 本地时区；0 = 已受理
pub type HostScheduleFn = extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
/// 释放宿主分配的字符串（只用于 `config_get`/`own_data_dir` 的返回值）
pub type HostFreeStrFn = extern "C" fn(*mut c_char);

/// **宿主能力表**：宿主提供给插件回调使用（方向与 [`KzwrPluginAbi`] 相反）
///
/// ## 为什么每个能力字段都是 `Option`
/// 本表同样按**尾部追加**演进：宿主可能比插件旧（字段还没出现）或比插件新。
/// 取到 `None` 就必须退回到「声明式回传」或本地默认行为，**不能崩**。
///
/// ## 为什么 `free_str` 紧跟在 `size` 之后（必需前缀）
/// 它是唯一**非可选**的入口（插件要用它释放 `config_get` / `own_data_dir` 返回的串）。
/// 放进必需前缀，[`KzwrHostAbi::MIN_SIZE`] 才能只覆盖 `abi + size + free_str`——
/// 于是**新插件遇到老宿主**（表更短、缺若干能力字段）时仍然接受整表，
/// 只是逐字段探测后跳过缺失的能力；否则就得整表拒绝，白白丢掉所有可用能力。
///
/// ## 三条硬纪律（宿主实现据此设计，插件必须知道）
///
/// 1. **写操作入队，不同步落库**。插件回调常跑在 `spawn_blocking` 线程（甚至插件
///    自己的 tokio runtime 里），而宿主的告警链路含 `tokio::spawn` → 直接调用会
///    panic「`must be called from the context of a Tokio runtime`」。故 `log`/`audit`/
///    `alert`/`resolve`/`progress`/`config_set`/`schedule` 只做一次 `try_send`，由宿主
///    唯一的消费任务落库。队列满 ⇒ 丢弃并计数（绝不打回插件线程）。
/// 2. **`config_get` 是唯一同步读**（无锁、无副作用），因此必须满足宿主不变式：
///    **宿主不得跨 FFI 持任何锁**。写完立刻读能拿到自己的值（待落盘覆盖层）。
/// 3. **能力被 ctx 限定到本插件**：所有命名空间（配置键、告警、定时器、数据目录）
///    都由 ctx 决定，参数里**没有**插件 id 可填 → 改不了、也读不到别的插件。
///    ctx 在插件被禁用/卸载时失效，之后的调用被静默丢弃。
///
/// ## 线程与安全边界
/// 每个入口都 `catch_unwind` + 校验入参（NULL / 非法 UTF-8 / 超长 → 截断或拒绝），
/// 并受每插件令牌桶限流。宿主**不**回调进插件，也不在持锁时调用本表的实现。
#[repr(C)]
pub struct KzwrHostAbi {
    /// 必须 == [`HOST_ABI_VERSION`]
    pub abi: u32,
    /// 本表字节大小（插件据此探测尾部字段；用 `offset_of` 校验，勿硬编码）
    pub size: u32,
    /// 释放宿主分配的字符串（**必需**；只用于本表返回的串）
    pub free_str: HostFreeStrFn,

    // ── 观测（入队） ────────────────────────────────────────────────
    /// 写宿主运行日志（进 `$TRIM_PKGVAR/logs/app.log`，带时间戳/级别/插件前缀）
    pub log: Option<HostLogFn>,
    /// 写审计日志（与宿主敏感操作同一份 `audit.log`，来源标记为本插件）
    pub audit: Option<HostAuditFn>,
    /// 上报一条告警（与声明式 `alerts` 同一去重规则）
    pub alert: Option<HostAlertFn>,
    /// 消解本插件此前上报的告警（按消息前缀）
    pub resolve_alerts: Option<HostResolveFn>,

    // ── 本插件自管配置 ─────────────────────────────────────────────
    /// 同步读一个键（明文；NULL = 不存在）
    pub config_get: Option<HostCfgGetFn>,
    /// 写一个键（值空串 = 删除该键）；0 = 已受理
    pub config_set: Option<HostCfgSetFn>,

    // ── 环境（纯读，无副作用） ─────────────────────────────────────
    /// 宿主版本串（**静态内存，插件不得释放**）
    pub host_version: Option<extern "C" fn() -> *const c_char>,
    /// 当前毫秒时间戳（与宿主同一时基；负数 = 不可用）
    pub now_ms: Option<extern "C" fn() -> i64>,
    /// 本插件私有数据目录（宿主已 `mkdir`；返回串**必须**用 `free_str` 释放）
    pub own_data_dir: Option<extern "C" fn(*mut c_void) -> *mut c_char>,

    // ── 长任务与调度（入队） ───────────────────────────────────────
    /// 上报进度（转发到 WebSocket 事件流，`kind="plugin"`）
    pub progress: Option<HostProgressFn>,
    /// 注册周期任务：到点宿主回调 `event_json("timer", cfg)`，其中 `cfg.timer_kind` = 注册时的 `kind`
    pub schedule: Option<HostScheduleFn>,
}

impl KzwrHostAbi {
    /// 本表完整长度（哨兵测试据此发现 SDK 镜像漂移）
    pub const TABLE_SIZE: u32 = std::mem::size_of::<Self>() as u32;

    /// **必需**前缀长度：`abi` + `size` + `free_str`（其余能力字段可选）
    ///
    /// 只覆盖前三个字段是刻意的：老宿主的表更短时，插件仍应接受整表、
    /// 逐字段探测后跳过缺失的能力（见本结构体的布局说明）。
    pub const MIN_SIZE: u32 = std::mem::offset_of!(Self, free_str) as u32
        + std::mem::size_of::<HostFreeStrFn>() as u32;
}

// ── 目标能力表（独立符号 → 独立演进）──────────────────────────────────────

/// 目标能力表：让插件提供**自定义备份目标**
///
/// ## 数据流（推块模式）
///
/// 宿主读明文 → age 加密 → `write_begin` / `write_chunk` / `write_end` 把**密文**喂给插件；
/// 插件**不回调宿主**（故无需 host vtable），明文与密钥永不离开宿主。
///
/// ## 关键约定
///
/// - **实例句柄 `th`**：多目标/多任务下同一插件可有多实例，`target_open` 每实例调用一次
/// - **文件句柄 `h`**：一次传输的上下文；同一 `th` 下允许多个 `h` **并发**（上限见 `describe.target.max_parallel`）
/// - **错误码**：`int < 0` 表示失败，宿主随后调用 `last_error_json` 取详情
/// - **字节上报**：`write_end` 返回该文件**实际写入的密文字节数**，宿主会与已喂出的字节数比对
/// - `list_json` 返回 `[{"rel_path","size","mtime_secs","is_dir"}]`
/// - 返回字符串同样由 `free_str` 释放；`*mut c_void` 句柄由插件负责释放
#[repr(C)]
pub struct KzwrTargetAbi {
    /// 本表 ABI 版本（与 [`C_ABI_VERSION`] 同规则）
    pub abi: u32,
    /// 本表字节大小（尾部追加字段时用于探测，宿主按需读取）
    pub size: u32,

    // ── 实例生命周期 ────────────────────────────────────────────────
    /// 用 `target_json`（含该目标凭据与插件自管配置）创建实例；返回 NULL = 配置无效
    pub target_open: extern "C" fn(*const c_char) -> *mut c_void,
    /// 释放实例（宿主刷新目标池/退出时调用）
    pub target_close: Option<extern "C" fn(*mut c_void)>,

    // ── 传输（宿主 → 插件，密文） ──────────────────────────────────
    /// 开始写入一个文件；`rel_path` 为目标端相对路径（源根下），`total` 为密文总字节数（未知为 0）
    pub write_begin: extern "C" fn(*mut c_void, *const c_char, u64) -> *mut c_void,
    /// 推入一块密文；返回写入字节数（≥0）或负错误码
    pub write_chunk: extern "C" fn(*mut c_void, *mut c_void, *const u8, u32) -> i32,
    /// 结束写入；返回**实际写入的密文字节数**（<0 = 错误码）
    pub write_end: extern "C" fn(*mut c_void, *mut c_void) -> i64,
    /// 中止写入（丢弃半成品；可选）
    pub write_abort: Option<extern "C" fn(*mut c_void, *mut c_void)>,

    // ── 读取（插件 → 宿主，密文；用于恢复） ────────────────────────
    pub read_begin: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    /// 读一块密文；返回读到的字节数（>0）、0 = EOF、<0 = 错误码
    pub read_chunk: extern "C" fn(*mut c_void, *mut c_void, *mut u8, u32) -> i32,
    /// 结束读取（可选）
    pub read_end: Option<extern "C" fn(*mut c_void, *mut c_void) -> i32>,

    // ── 目录与元数据 ───────────────────────────────────────────────
    /// 按前缀列出条目（JSON 数组）
    pub list_json: extern "C" fn(*mut c_void, *const c_char) -> *mut c_char,
    /// 删除文件/目录：0 = 成功，非 0 = 失败
    pub delete: extern "C" fn(*mut c_void, *const c_char) -> i32,
    /// 确保目录存在（可选）
    pub ensure_dir: Option<extern "C" fn(*mut c_void, *const c_char) -> i32>,
    /// 连通性测试（可选）：0 = 成功
    pub ping: Option<extern "C" fn(*mut c_void) -> i32>,
    /// 保存前的连通性测试（实例尚未建立时用；入参 `target_json`）
    pub test_json: Option<extern "C" fn(*const c_char) -> *mut c_char>,

    // ── 插件自管配置（宿主代加密存储，命名空间 = 插件 id） ──────────
    /// 读一个键（返回 JSON 字符串或裸字符串；NULL = 不存在）
    pub config_get: Option<extern "C" fn(*const c_char) -> *mut c_char>,
    /// 写一个键：0 = 成功
    pub config_set: Option<extern "C" fn(*const c_char, *const c_char) -> i32>,

    /// 最近一次错误的详情（JSON）
    pub last_error_json: Option<extern "C" fn(*mut c_void) -> *mut c_char>,

    // ── 并发回传（可选能力：`describe.target.supports_plan = true` 时宿主采用）──
    /// 开始规划：`job_json.upload` 给出待传清单（rel_path/size/mtime）
    pub plan_begin: Option<extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>,
    /// 下一批要传的文件（JSON 数组，如 `["a.txt","b/c.bin"]`；`[]` = 清单已空）
    pub plan_next: Option<extern "C" fn(*mut c_void, *mut c_void) -> *mut c_char>,
    /// 结束规划
    pub plan_end: Option<extern "C" fn(*mut c_void, *mut c_void)>,

    /// 释放本表返回的字符串（必需）
    pub free_str: extern "C" fn(*mut c_char),
}

impl KzwrTargetAbi {
    /// 本表必需前缀长度
    pub const REQUIRED_SIZE: u32 = std::mem::size_of::<Self>() as u32;
}

// ── describe_json 解析 ────────────────────────────────────────────────────

/// `describe_json` 的解析结果
#[derive(Debug, Clone, Deserialize)]
pub struct AbiDescribe {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// `enhance`（缺省）| `target`
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub caps: AbiCaps,
    /// 能力表发现（当前只用到 `target`）
    #[serde(default)]
    pub runtime: AbiRuntime,
    /// 目标能力声明
    #[serde(default)]
    pub target: Option<AbiTargetCaps>,
    #[serde(default)]
    pub ui: Option<crate::plugin::api::PluginUi>,
}

/// 能力表及其入口符号名（缺省用 [`SYM_TARGET_V1`]）
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AbiRuntime {
    #[serde(default)]
    pub target: Option<String>,
}

impl AbiRuntime {
    /// 目标能力入口符号名
    pub fn target_symbol(&self) -> &str {
        self.target.as_deref().unwrap_or(SYM_TARGET_V1)
    }
}

/// 目标能力声明（`describe_json.target`）
#[derive(Debug, Clone, Deserialize)]
pub struct AbiTargetCaps {
    /// 是否支持并发回传（`plan_*`）；false/缺省 = 宿主按顺序推块
    #[serde(default)]
    pub supports_plan: bool,
    /// 并发上限（`supports_plan=true` 时生效；0 = 不限）
    #[serde(default)]
    pub max_parallel: u32,
    /// 宿主推荐的推送块大小（KiB；0 = 宿主默认 1024）
    #[serde(default)]
    pub preferred_chunk_kib: u32,
    /// 是否需要用户名/密码（缺省 `true`，与既有插件行为一致）
    ///
    /// 声明 `false` 表示该目标不用凭据（如「本地目录」只认一个路径）。
    /// 否则宿主会强制要求填账号密码，用户在「目标」页**建不出**这类目标。
    /// 同时这也意味着保存前不做连通性实测（没有可测的连接）。
    #[serde(default = "default_true_caps")]
    pub needs_credentials: bool,
    /// 目标地址字段的展示标签（缺省「地址」）
    ///
    /// 让插件说明 `url` 的实际含义：WebDAV 是「地址」，本地目录则是「目录路径」。
    /// 前端据此渲染标签与占位提示，避免用户对着「地址」输入框填不出本地路径。
    #[serde(default)]
    pub url_label: Option<String>,
    /// 地址字段的占位提示（缺省按 WebDAV 的官方地址）
    #[serde(default)]
    pub url_placeholder: Option<String>,
    /// 地址字段的说明文字（缺省按 WebDAV 的语义）
    #[serde(default)]
    pub url_hint: Option<String>,

    /// **「新建/编辑目标」表单的字段声明**（缺省 = 宿主按 WebDAV 语义给默认表单）
    ///
    /// 这是「弹窗由插件自定义」的落点：插件声明要哪些字段、什么类型、是否必填、
    /// 是否敏感，宿主**只负责渲染与存取**，不预设任何字段语义
    /// —— 与插件设置弹窗（`ui.blocks`）同一套思路。
    #[serde(default)]
    pub form: Vec<AbiTargetField>,
}

/// 目标表单里的一个字段（插件声明，宿主渲染）
///
/// ## 键名约定
/// 三个**well-known 键**由宿主映射到既有存储（`target_json` 的固定字段）：
///
/// | 键 | 宿主存储 | 注入 `target_json` |
/// |---|---|---|
/// | `url` | `TargetConfig.url` | `url` |
/// | `username` | `username_enc`（加密） | `username` |
/// | `password` | `password_enc`（加密） | `password` |
///
/// **其余任意键**存入 `TargetConfig.fields`（**按目标**、加密存储），
/// 并注入 `target_json.config` —— 插件从自己的命名空间读，宿主不解释其含义。
///
/// 用 well-known 键映射而不是「所有字段都进 fields」，是为了不破坏既有契约：
/// `target_json.url` / `username` / `password` 是已发布插件的读取位置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbiTargetField {
    /// 字段键（提交体与 `target_json.config` 里的键名）
    pub key: String,
    /// 展示标签
    pub label: String,
    /// 控件类型：`text`（缺省）| `password` | `number` | `toggle` | `select`
    #[serde(default = "default_field_kind")]
    pub kind: String,
    /// 是否必填（前端校验；后端只校验 `url` 这类宿主必需的键）
    #[serde(default)]
    pub required: bool,
    /// 是否敏感：**加密存储**，回显时只给 `configured` 布尔、绝不回传明文
    ///
    /// 缺省时按 `kind == "password"` 推断（密码框默认敏感）。
    #[serde(default)]
    pub secret: Option<bool>,
    /// 占位提示
    #[serde(default)]
    pub placeholder: Option<String>,
    /// 字段下方的说明文字
    #[serde(default)]
    pub hint: Option<String>,
    /// 缺省值（新建时预填）
    #[serde(default)]
    pub default: Option<String>,
    /// `kind == "select"` 时的选项
    #[serde(default)]
    pub options: Vec<AbiTargetFieldOption>,
}

impl AbiTargetField {
    /// 是否敏感（显式声明优先；缺省按 password 类型推断）
    pub fn is_secret(&self) -> bool {
        self.secret.unwrap_or(self.kind == "password")
    }
}

/// `select` 字段的一个选项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbiTargetFieldOption {
    /// 提交值
    pub value: String,
    /// 展示文案（缺省用 `value`）
    #[serde(default)]
    pub label: Option<String>,
}

fn default_field_kind() -> String {
    "text".to_string()
}

/// `#[serde(default)]` 的 bool 缺省值：`true`
///
/// 注意与 [`Default`] 实现配合：**反序列化时**缺省为 `true`（老插件不写该字段
/// 就按「需要凭据」处理，行为不变），因此不能直接用 `#[derive(Default)]` 的 `false`。
fn default_true_caps() -> bool {
    true
}

impl Default for AbiTargetCaps {
    fn default() -> Self {
        Self {
            supports_plan: false,
            max_parallel: 1,
            preferred_chunk_kib: 1024,
            needs_credentials: true,
            url_label: None,
            url_placeholder: None,
            url_hint: None,
            form: Vec::new(),
        }
    }
}

/// `caps` 字段（与 [`crate::plugin::api::EnhanceCaps`] 同形，单独定义以免依赖宿主内部类型布局）
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct AbiCaps {
    #[serde(default)]
    pub account: bool,
    #[serde(default)]
    pub quota: bool,
    #[serde(default)]
    pub recycle_bin: bool,
    #[serde(default)]
    pub notify: bool,
}

impl From<AbiCaps> for crate::plugin::api::EnhanceCaps {
    fn from(c: AbiCaps) -> Self {
        Self {
            account: c.account,
            quota: c.quota,
            recycle_bin: c.recycle_bin,
            notify: c.notify,
        }
    }
}

// ── 配置快照（cfg_json） ──────────────────────────────────────────────────

/// 传给插件的配置快照（`cfg_json`）
///
/// 除 `self_config`（**本插件自己的**明文配置）外**不含密码 / token**：
/// 其它凭据一律不给，插件自有凭据走 `self_config` 或 [`KzwrTargetAbi::config_set`]。
#[derive(Default, Clone)]
pub struct CfgSnapshot {
    /// 宿主版本（插件可用于日志/兼容判断）
    pub host_version: String,
    /// 宿主时区说明
    pub timezone: String,
    /// 宿主时区相对 UTC 的分钟偏移
    pub utc_offset_minutes: i64,
    /// 目标（id/名称/类型/地址/用户名/是否启用/是否就绪/是否主目标）
    pub targets: Vec<CfgTarget>,
    /// 任务（id/名称/是否启用/源路径/目标/目录/定时）
    pub tasks: Vec<CfgTask>,
    /// **本插件自己的**配置（明文键值对，已解密）
    ///
    /// 这是「插件读取自身设置的唯一受支持路径」对**增强插件**的对应物
    /// （目标插件走 `target_json.config`）。
    ///
    /// **隔离性由宿主保证**：只填 `cfg.plugin_data[<本插件 id>]`，
    /// **绝不包含**其它插件的键，也不包含密码等宿主私有凭据。
    /// 调用方（宿主）注入，插件侧只读。
    ///
    /// 注意：这里**含明文**（含插件自有凭据），**调用方不得写入日志**。
    pub self_config: std::collections::BTreeMap<String, String>,
    /// 触发本次 `after_backup` 事件的任务 id（其它事件为 `None`）
    ///
    /// 有了它，插件才能知道「刚才那次备份是哪个任务」，从而套用**该任务自己的**
    /// 回收站门槛（`recycle_max_gb` / `recycle_min_age_days`），而不是所有任务共用一套。
    pub after_backup_task: Option<String>,
}

/// 配置快照里的目标
#[derive(Default, Clone)]
pub struct CfgTarget {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub url: String,
    /// 解密后的用户名（**不含密码**；无法解密时为空串）
    pub username: String,
    pub enabled: bool,
    pub ready: bool,
    pub primary: bool,
}

/// 配置快照里的任务
#[derive(Default, Clone)]
pub struct CfgTask {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub paths: Vec<String>,
    pub target_id: String,
    pub target_folder: String,
    pub schedule_cron: String,
    /// 保留策略：备份后是否清空云端回收站（由支持该能力的增强插件实现）
    pub empty_recycle_bin: bool,
    /// 回收站占用超过该 GB 数才清理（0 = 不限制，总是清理）
    pub recycle_max_gb: u64,
    /// 只清理删除时间早于该天数的回收站条目（0 = 不限制）
    pub recycle_min_age_days: u64,
}

impl CfgSnapshot {
    /// 由配置构造快照（只看配置本身，不读运行时状态 → 各处调用口径一致）
    ///
    /// 用户名需要解密，故此处留空；调用方可随后用 [`Self::with_target_usernames`] 填。
    pub fn from_config(cfg: &crate::infra::config::AppConfig) -> Self {
        let primary = cfg
            .primary_target()
            .map(|t| t.id.clone())
            .unwrap_or_default();
        Self {
            host_version: HOST_VERSION.to_string(),
            timezone: crate::domain::scheduler::timezone_label(),
            utc_offset_minutes: crate::domain::scheduler::utc_offset_minutes(),
            targets: cfg
                .targets
                .iter()
                .map(|t| CfgTarget {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    kind: t.kind.clone(),
                    url: t.url.clone().unwrap_or_default(),
                    username: String::new(),
                    enabled: t.enabled,
                    // 「就绪」= 启用且凭据齐备（与 /api/targets 的语义一致）
                    ready: t.enabled && t.configured(),
                    primary: t.id == primary,
                })
                .collect(),
            tasks: cfg
                .tasks
                .iter()
                .map(|t| CfgTask {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    enabled: t.enabled,
                    paths: t.paths.clone(),
                    target_id: t.target_id.clone(),
                    target_folder: t.target_folder.clone(),
                    schedule_cron: t.schedule_cron.clone().unwrap_or_default(),
                    empty_recycle_bin: t.retention.empty_recycle_bin,
                    recycle_max_gb: t.retention.recycle_max_gb,
                    recycle_min_age_days: t.retention.recycle_min_age_days,
                })
                .collect(),
            // 明文自配置由调用方随后用 `with_self_config` 填（需要 ConfigManager 才能解密）
            self_config: std::collections::BTreeMap::new(),
            after_backup_task: None,
        }
    }

    /// 填入**本插件自己的**配置（明文）
    ///
    /// `plugin_id` 用来定位命名空间 —— 这是隔离的关键：只有该插件自己的键值会被放入，
    /// 因此插件**无法**通过 `cfg` 读到其它插件的配置（它们根本不在这份快照里）。
    pub fn with_self_config(mut self, cfg: &crate::infra::config::AppConfig, plugin_id: &str, mgr: &crate::infra::config::ConfigManager) -> Self {
        // 解密失败 → 空配置（与 plugin_data_json 同口径：插件应回退到默认值）
        if let Ok(kv) = mgr.plugin_data_export(cfg, plugin_id) {
            self.self_config = kv;
        }
        self
    }

    /// 标记「本次 `after_backup` 事件由哪个任务触发」
    pub fn with_after_backup_task(mut self, task_id: &str) -> Self {
        self.after_backup_task = Some(task_id.to_string());
        self
    }

    /// 填入目标用户名（`目标 id → 用户名`；不在表里的保持空串）
    pub fn with_target_usernames(
        mut self,
        users: &std::collections::HashMap<String, String>,
    ) -> Self {
        for t in &mut self.targets {
            if let Some(u) = users.get(&t.id) {
                t.username = u.clone();
            }
        }
        self
    }

    /// 序列化为稳定的 `cfg_json`（字段只增不改；插件忽略不认识的键即可）
    pub fn to_json(&self) -> String {
        let targets: Vec<serde_json::Value> = self
            .targets
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "name": t.name,
                    "kind": t.kind,
                    "url": t.url,
                    "username": t.username,
                    "enabled": t.enabled,
                    "ready": t.ready,
                    "primary": t.primary,
                })
            })
            .collect();
        let tasks: Vec<serde_json::Value> = self
            .tasks
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "name": t.name,
                    "enabled": t.enabled,
                    "paths": t.paths,
                    "target_id": t.target_id,
                    "target_folder": t.target_folder,
                    "schedule_cron": t.schedule_cron,
                    "empty_recycle_bin": t.empty_recycle_bin,
                    "recycle_max_gb": t.recycle_max_gb,
                    "recycle_min_age_days": t.recycle_min_age_days,
                })
            })
            .collect();
        serde_json::json!({
            "abi_version": C_ABI_VERSION,
            "host_version": self.host_version,
            "timezone": self.timezone,
            "utc_offset_minutes": self.utc_offset_minutes,
            "targets": targets,
            "tasks": tasks,
            // 本插件自己的明文配置（隔离：只含本插件命名空间；**含凭据，禁止写日志**）
            "self_config": self.self_config,
            // 仅 after_backup 事件有值：告诉插件「刚才完成的是哪个任务」
            "after_backup_task": self.after_backup_task,
        })
        .to_string()
    }
}
