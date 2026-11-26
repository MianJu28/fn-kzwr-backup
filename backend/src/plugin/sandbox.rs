//! **插件线程沙箱**（Landlock）——档位 B：同进程内的最小权限
//!
//! ## 为什么需要
//! 插件与宿主**同进程、同 uid**（见 `docs/memory/dev/PLUGIN_ISOLATION.md`）：
//! 插件代码可以读走解密备份所需的全部材料 ——
//! `keystore.age`（age 私钥密文）+ `.passphrase`（主口令），
//! 两者属主与运行进程**同一个 uid**，`0600` 挡不住同 uid，
//! 于是同 uid 子进程/同进程代码都能拿到，**进而解出能解密全部备份的私钥**。
//!
//! ## 为什么用 Landlock 而不是子进程
//! 实测（`docs/memory/dev/PLUGIN_ISOLATION.md` §2）：
//! - 应用 `CapEff=0` ⇒ 不能 `mount`/`chroot`/建 netns；
//! - 非特权 `unshare` 返回 `EPERM` ⇒ 容器方案不可用；
//! - **Landlock ABI v7 无需任何特权即可用**，且能限制文件路径与 TCP 端口。
//!
//! 进程隔离（档位 C）收益更大但代价高（需新产物「插件运行器」+ 传输路径改 fd 传递）；
//! Landlock 是**零契约变更**就能拿到「插件碰不到密钥与口令」的最小代价方案。
//!
//! ## 三条硬约束（来自实测，违反会引发难查的故障）
//!
//! 1. **Landlock 是 per-thread 的**。多线程进程必须在**每个**要受限的线程上分别施加；
//!    已存在的线程**不受影响**。
//! 2. **不可逆**。一旦施加就无法放宽（再施加更宽的 ruleset 也无效）。
//!    ⇒ **绝不能**施加在 tokio 的共享 `spawn_blocking` 池线程上：那些线程会被
//!    **永久污染**，之后宿主的其它阻塞任务（配置读写、快照操作）复用到它就莫名失败。
//!    ⇒ 因此本模块为**每个插件**开**专属线程**，插件调用只跑在那里。
//! 3. **`/proc/self` 也会被挡**。插件运行需要它，白名单里必须**显式允许**。
//!    （好消息：`/proc/<宿主 pid>/mem` 同样被挡 —— 那是同 uid 读内存的经典通道。）
//!
//! ## 已知缺口：Unix socket 不受 Landlock 约束（2026-11-24 实测）
//!
//! Landlock 的路径规则只作用于 `open()` 族系统调用（含 `O_PATH`），
//! **不作用于 `connect(2)`**。实测（两份等价 C 程序，均在 ABI v7 内核上）：
//!
//! | 场景 | `connect()` 到 Unix socket |
//! |------|---------------------------|
//! | socket 所在目录**在**只读白名单里 | **成功** |
//! | socket 所在目录**不在**白名单里 | **仍然成功** |
//!
//! 即：**本沙箱无法阻止插件连接宿主机上的任何 Unix socket**。
//! 能挡住的只有 DAC 文件权限 —— 例如 `/run/docker.sock` 是 `root:docker`，
//! 插件跑在应用用户下连不上，那是**权限位**的功劳，不是沙箱的。
//!
//! 影响面：`/run` 下还有 `trim_share.sock`、`com.trim.app.center.sock` 等
//! `root:root` 的 socket，同样靠 DAC 挡住。若某个 socket 被放宽到
//! 应用用户可写，插件即可通过它获得越过沙箱的能力。
//!
//! 彻底修复需要以下之一（均超出当前范围，需产品决策）：
//! 1. Landlock 未来的 `scope` 支持（当前内核版的 `scoped` 字段不覆盖 socket）；
//! 2. 用 seccomp 过滤 `connect(AF_UNIX, ...)` 的 `sun_path` 白名单；
//! 3. 进程隔离（档位 C：独立进程 + fd 传递，见 `PLUGIN_ISOLATION.md`）。
//!
//! 因此本模块的加固**只覆盖文件读写**；排查隔离问题时请不要假设
//! 「施加了 Landlock 就碰不到 socket」。
//!
//! ## 降级策略
//! 内核不支持 Landlock（`landlock_create_ruleset` 返回 `ENOSYS`）或施加失败时：
//! **记警告并继续运行**（不阻断插件），因为「可用性」优先于「加固」；
//! 但会**明确记录**该插件未受沙箱保护，便于诊断。

use std::ffi::CString;
use std::io;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};

// ── 内核 UAPI（libc 只给了 syscall 号，未给结构体与常量，故本地声明）────────
//
// 布局取自 `/usr/include/linux/landlock.h`（稳定 UAPI，不可随意改动）。
// 用 `#[repr(C)]` 保证与内核一致。

/// `struct landlock_ruleset_attr`（3 个字段，**一个都不能少**）
#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

/// `struct landlock_path_beneath_attr`（注意 `parent_fd` 是 `__s32`）
#[repr(C)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// `struct landlock_net_port_attr`
#[repr(C)]
struct LandlockNetPortAttr {
    allowed_access: u64,
    port: u64,
}

// 文件系统访问位（`landlock.h` 的 Filesystem flags）
const FS_EXECUTE: u64 = 1 << 0;
const FS_WRITE_FILE: u64 = 1 << 1;
const FS_READ_FILE: u64 = 1 << 2;
const FS_READ_DIR: u64 = 1 << 3;
const FS_REMOVE_DIR: u64 = 1 << 4;
const FS_REMOVE_FILE: u64 = 1 << 5;
const FS_MAKE_DIR: u64 = 1 << 7;
const FS_MAKE_REG: u64 = 1 << 8;
const FS_MAKE_SYM: u64 = 1 << 12;
const FS_TRUNCATE: u64 = 1 << 14;

// 网络访问位
const NET_CONNECT_TCP: u64 = 1 << 1;

// `landlock_create_ruleset` 的 flags
const CREATE_RULESET_VERSION: u32 = 1 << 0;
// `landlock_add_rule` 的 rule_type
const RULE_PATH_BENEATH: i32 = 1;
const RULE_NET_PORT: i32 = 2;

// `prctl`
const PR_SET_NO_NEW_PRIVS: i32 = 38;

/// 只读 + 列目录（给系统目录用）
///
/// ⚠️ `RO`/`RW` 含 `FS_READ_DIR`，那是**目录专属**权限位。
/// Landlock 对**普通文件**的 `path_beneath` 规则若带目录专属位，内核直接返回
/// `EINVAL`，规则**静默不生效** —— 实测 `/dev/urandom`、`/dev/null`、`/dev/zero`
/// 三条规则从未生效过（只是恰好被其它规则覆盖，靠运气）。
/// 因此下面另给 `*_FILE` 变体，按对象类型选位。
const RO: u64 = FS_READ_FILE | FS_READ_DIR | FS_EXECUTE;
/// 只读**文件**（不含目录位，用于 `/dev/urandom` 这类设备/普通文件）
const RO_FILE: u64 = FS_READ_FILE | FS_EXECUTE;
/// 只**列目录**（不放行目录内文件的读取，用于 `/proc/self` 这类必须收窄的目录）
const RO_DIR_ONLY: u64 = FS_READ_DIR | FS_EXECUTE;
/// 读写（给插件数据目录、目标目录、临时目录用）
const RW: u64 = RO
    | FS_WRITE_FILE
    | FS_MAKE_REG
    | FS_MAKE_DIR
    | FS_REMOVE_FILE
    | FS_REMOVE_DIR
    | FS_TRUNCATE
    | FS_MAKE_SYM;
/// 读写**文件**（不含目录位）
const RW_FILE: u64 = FS_READ_FILE
    | FS_WRITE_FILE
    | FS_TRUNCATE
    | FS_EXECUTE;

#[inline]
fn sys_landlock_create_ruleset(attr: *const LandlockRulesetAttr, size: usize, flags: u32) -> i64 {
    unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, attr, size, flags as libc::c_long) }
}

#[inline]
fn sys_landlock_add_rule(
    fd: RawFd,
    rule_type: i32,
    rule_attr: *const std::ffi::c_void,
    flags: u32,
) -> i64 {
    unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            fd as libc::c_long,
            rule_type as libc::c_long,
            rule_attr,
            flags as libc::c_long,
        )
    }
}

#[inline]
fn sys_landlock_restrict_self(fd: RawFd, flags: u32) -> i64 {
    unsafe {
        libc::syscall(
            libc::SYS_landlock_restrict_self,
            fd as libc::c_long,
            flags as libc::c_long,
        )
    }
}

/// 查询 Landlock ABI 版本（`None` = 内核不支持）
///
/// 用 `landlock_create_ruleset(NULL, 0, VERSION)` 探测 —— 这是官方推荐方式，
/// 不需要任何特权。
pub fn abi_version() -> Option<u32> {
    let r = sys_landlock_create_ruleset(std::ptr::null(), 0, CREATE_RULESET_VERSION);
    if r < 0 {
        None
    } else {
        Some(r as u32)
    }
}

/// 沙箱的允许清单
///
/// 语义：**默认拒绝，只放行列出的路径与端口**（Landlock 是白名单模型）。
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    /// `(路径, 是否可写)`；路径不存在时**跳过**（不报错 —— 插件不该因缺目录而起不来）
    pub paths: Vec<(PathBuf, bool)>,
    /// 允许 `connect()` 的 TCP 端口；**空 = 不限制网络**（仅当 `restrict_net` 为真才生效）
    pub net_ports: Vec<u16>,
    /// 是否启用网络限制（`false` = 完全不管网络，保持插件原有联网能力）
    pub restrict_net: bool,
}

impl SandboxPolicy {
    /// 构建「插件专用」策略：给它自己的数据目录、目标需要的路径、以及必要系统目录
    ///
    /// - `own_data_dir`：插件私有目录（读写）——它的配置与缓存都在这
    /// - `plugin_id`：用于派生**该插件专属**的临时目录（见下）
    /// - `extra`：额外需要读写的路径（如本地目录目标的根、任务源路径）
    /// - 系统只读：`/etc`（时区/证书）、`/usr`（动态库）、`/dev/urandom`、
    ///   **`/proc/self`**（很多库要读，必须显式允许）
    ///
    /// ## 为什么临时目录要按插件隔离（而不是共享 `/tmp`）
    ///
    /// 曾经把 `/tmp` 整目录设为**可写**。问题：所有插件进程内共享同一个 `/tmp`，
    /// 于是插件 A 能写文件、插件 B 读走 —— **插件之间存在一条宿主看不见的信道**。
    /// 市场场景下装多个第三方插件是常态，这条信道没有正当用途、只有风险
    /// （还附带符号链接/竞态一类的共享临时目录问题）。
    ///
    /// 改为 `/tmp/fn-kzwr-plugin-<id>/`（**只**给本插件读写），既保留插件做中间
    /// 文件的能力（`example-localfs` 的临时落定等），又切断插件间信道。
    pub fn for_plugin(plugin_id: &str, own_data_dir: &Path, extra: &[PathBuf]) -> Self {
        let mut paths: Vec<(PathBuf, bool)> = vec![
            // `/proc/self` **只给「列目录」，不给「读文件」**。
            //
            // ⚠️ 这里是整个沙箱最要紧的一处。实测（Landlock ABI v7）：
            // 若按 `RO`（含 `FS_READ_FILE`）放行 `/proc/self`，那么插件线程里的
            // `/proc/self` 指向**本进程**，于是 `/proc/self/mem` 可读 ——
            // 插件扫 `/proc/self/maps` 拿到地址后 `pread` 就能直接读出内存里的
            // `AGE-SECRET-KEY-*`，**完全绕过** `keystore.age` 的加密与 0600 权限。
            // `/proc/self/environ` 同理会泄漏 `TRIM_PASSPHRASE`。
            //
            // 收窄后（实测）：`status`/`stat`/`maps`/`fd` 放行，
            // `environ`/`mem`/`cmdline` 一律 Permission denied。
            (PathBuf::from("/proc/self"), false),
            (PathBuf::from("/etc"), false),
            (PathBuf::from("/usr"), false),
            // CA 证书与 DNS 解析（kzwr 要联网）。
            //
            // 用**精确子路径**而不是整目录 `/run`：Landlock 不拦 `connect()`
            // （见模块头「已知缺口」），整目录放行仍会暴露 `/run` 下的普通文件。
            (PathBuf::from("/run/systemd/resolve"), false),
            (PathBuf::from("/run/nscd"), false),
        ];
        // 逐文件放行 `/proc/self` 下插件真正需要的那几项。
        //
        // 为什么要逐条给：目录规则只给 `FS_READ_DIR` 后，目录内**所有文件**都读不了；
        // 而很多运行时/库会读 `status`、`maps`、`fd/`。这几项不含敏感内容。
        // 目录在 `/proc` 上是魔法文件系统，`metadata().is_dir()` 对 `/proc/self`
        // 会解析成真实目录，故下面按「是否目录」自动选权限位（见 `add_rules_and_restrict`）。
        // ⚠️ **只放行纯信息类文件**，绝不放行这几类：
        // - `/proc/self/mem`：直接读进程内存（会泄漏 age 私钥）—— 本就不能给。
        // - `/proc/self/environ`：泄露 `TRIM_PASSPHRASE` 等环境变量。
        // - `/proc/self/root`：是指向 `/` 的**目录**，放行它等于把整个根目录
        //   重新开成可读，前面所有收窄都白做。
        // - `/proc/self/cwd`：同理，指向进程工作目录。
        // - `/proc/self/fd`：是目录，且 `fd/N` 可重开已打开的文件（含密钥库）。
        // - `/proc/self/task`：是目录，内含每个线程的 `mem`/`environ`，等价绕过。
        for f in ["status", "stat", "statm", "maps", "mountinfo", "limits"] {
            paths.push((PathBuf::from("/proc/self").join(f), false));
        }
        // 设备文件用**文件位**（目录位会让内核返回 EINVAL，规则静默失效）
        paths.push((PathBuf::from("/dev/urandom"), false));
        paths.push((PathBuf::from("/dev/null"), true));
        paths.push((PathBuf::from("/dev/zero"), false));
        paths.push((own_data_dir.to_path_buf(), true));
        // 插件专属临时目录（替代共享可写的 /tmp）
        paths.push((Self::plugin_tmp_dir(plugin_id), true));
        for p in extra {
            paths.push((p.clone(), true));
        }
        Self {
            paths,
            net_ports: Vec::new(),
            restrict_net: false,
        }
    }

    /// 该插件专属的临时目录：`/tmp/fn-kzwr-plugin-<id>`
    ///
    /// `id` 里的路径分隔符会被替换成 `_`：插件 id 来自插件自报的 `describe_json`
    /// （第三方可控），绝不能让它靠 `../` 之类把白名单指到别处。
    ///
    /// 目录在**施加沙箱之前**创建（Landlock 白名单路径必须已存在，否则规则会被跳过，
    /// 表现为插件写临时文件失败）。创建失败不阻断 —— 白名单仍然生效，
    /// 只是该目录可能因不存在而被跳过，插件会拿到「写不了」的错误。
    pub fn plugin_tmp_dir(plugin_id: &str) -> PathBuf {
        let safe: String = plugin_id
            .chars()
            .map(|c| if c == '/' || c == '\\' || c == '.' { '_' } else { c })
            .collect();
        let dir = std::env::temp_dir().join(format!("fn-kzwr-plugin-{safe}"));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(
                plugin = %plugin_id,
                dir = %dir.display(),
                err = %e,
                "创建插件专属临时目录失败（该插件写临时文件会失败）"
            );
        }
        dir
    }

    /// 限制网络为「只允许连这些端口」（kzwr 只需 443）
    pub fn allow_tcp_ports(mut self, ports: &[u16]) -> Self {
        self.net_ports = ports.to_vec();
        self.restrict_net = true;
        self
    }

    /// 施加到**当前线程**（调用后本线程永久受限，见模块头约束 2）
    ///
    /// 返回 `Err` 表示施加失败；调用方应记警告并继续（降级而非阻断）。
    ///
    /// # Safety-ish
    /// 本函数通过 `prctl` + `landlock_restrict_self` 改变**当前线程**的安全状态，
    /// 且**不可逆**。只应在插件专属线程上调用。
    pub fn apply_to_current_thread(&self) -> io::Result<()> {
        let version = abi_version().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Unsupported, "内核不支持 Landlock（无 landlock_create_ruleset）")
        })?;

        // `handled_access_*` = 「我要管哪些权限」；未被管的不受限制。
        // 我们管全部文件写入类 + 读取，以及（可选）TCP 连接。
        let handled_fs = RW | FS_EXECUTE;
        let handled_net = if self.restrict_net { NET_CONNECT_TCP } else { 0 };
        // ABI < 4 的内核不理解 handled_access_net（字段也不存在，是个 0 值扩展）；
        // 此时明确拒绝启用网络限制，避免"以为限了其实没限"。
        if self.restrict_net && version < 4 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("Landlock ABI v{version} < 4，不支持网络限制"),
            ));
        }

        let attr = LandlockRulesetAttr {
            handled_access_fs: handled_fs,
            handled_access_net: handled_net,
            scoped: 0,
        };
        let fd = sys_landlock_create_ruleset(&attr, std::mem::size_of::<LandlockRulesetAttr>(), 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = fd as RawFd;
        // 从此刻起无论成功失败都要关掉 fd
        let result = self.add_rules_and_restrict(fd);
        unsafe { libc::close(fd) };
        result
    }

    fn add_rules_and_restrict(&self, fd: RawFd) -> io::Result<()> {
        for (path, writable) in &self.paths {
            // 路径不存在就跳过：插件不该因为宿主没建某个目录而无法启动
            let Ok(c) = CString::new(path.as_os_str().as_encoded_bytes()) else {
                continue;
            };
            // O_PATH：只取「句柄」，不需要真实打开权限
            let pfd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            if pfd < 0 {
                continue;
            }
            // **按对象类型选权限位**（这不是细节，错了规则会静默失效）：
            //
            // Landlock 对「普通文件」的 `path_beneath` 规则若带目录专属位
            // （`FS_READ_DIR` / `FS_MAKE_DIR` / `FS_REMOVE_DIR` 等），内核直接返回
            // `EINVAL`。实测：`/dev/urandom` 按 `RO` 加规则 ⇒ `Invalid argument`，
            // 规则从未生效；去掉目录位后 OK。
            //
            // 反过来，只给文件位而不给 `FS_READ_DIR` 的目录规则，会让目录内所有
            // 文件都读不到 —— 这也是为什么 `/proc/self` 用 `RO_DIR_ONLY` 时必须
            // 再逐文件放行。
            let is_dir = unsafe {
                let mut st: libc::stat = std::mem::zeroed();
                libc::fstat(pfd, &mut st) == 0 && (st.st_mode & libc::S_IFMT) == libc::S_IFDIR
            };
            // 目录默认可「列 + 读内部文件」；只有 `/proc/self` 这类必须收窄的
            // 才用 `RO_DIR_ONLY`（只列不放读）。用「路径是否以 /proc/self 开头」
            // 判断，而不是再加一个布尔字段 —— 白名单里只有这一处需要这种语义。
            let narrow = path == std::path::Path::new("/proc/self");
            let allowed = match (is_dir, *writable) {
                (true, true) => RW,
                (true, false) if narrow => RO_DIR_ONLY,
                (true, false) => RO,
                (false, true) => RW_FILE,
                (false, false) => RO_FILE,
            };
            let rule = LandlockPathBeneathAttr {
                allowed_access: allowed,
                parent_fd: pfd,
            };
            let added = sys_landlock_add_rule(
                fd,
                RULE_PATH_BENEATH,
                &rule as *const _ as *const std::ffi::c_void,
                0,
            );
            unsafe { libc::close(pfd) };
            if added < 0 {
                let e = io::Error::last_os_error();
                // **必须是 warn 而非 debug**：默认日志级别是 info，
                // debug 会让「规则悄悄没生效」完全不可见 —— 这正是本文件
                // 曾长期存在 `/dev/*` 规则失效却无人发现的原因。
                // `EINVAL` = 规则写错（权限位不匹配对象类型），一定要修；
                // `ENOENT` = 路径不存在（可忽略）。
                if e.raw_os_error() == Some(libc::ENOENT) {
                    tracing::debug!(path = %path.display(), "Landlock 跳过（路径不存在）");
                } else {
                    tracing::warn!(
                        path = %path.display(),
                        is_dir,
                        writable,
                        err = %e,
                        "Landlock 规则添加失败（该路径未受保护）"
                    );
                }
            }
        }

        for port in &self.net_ports {
            let rule = LandlockNetPortAttr {
                allowed_access: NET_CONNECT_TCP,
                port: *port as u64,
            };
            let added = sys_landlock_add_rule(
                fd,
                RULE_NET_PORT,
                &rule as *const _ as *const std::ffi::c_void,
                0,
            );
            if added < 0 {
                return Err(io::Error::last_os_error());
            }
        }

        // `no_new_privs` 是 `restrict_self` 的前置条件（防权限提升绕过沙箱）
        if unsafe { libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if sys_landlock_restrict_self(fd, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// 该线程是否已被沙箱永久限制（诊断用；Landlock 无查询接口，故由调用方记录）
pub fn is_supported() -> bool {
    abi_version().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_probe_does_not_panic() {
        // 不支持的内核上应返回 None 而不是 panic
        let v = abi_version();
        println!("Landlock ABI: {v:?}");
    }

    #[test]
    fn policy_builder_includes_proc_self() {
        let p = SandboxPolicy::for_plugin("test-plugin", Path::new("/tmp/plug"), &[]);
        assert!(
            p.paths.iter().any(|(path, _)| path == Path::new("/proc/self")),
            "必须显式允许 /proc/self —— Landlock 会连它一起挡住"
        );
        assert!(
            p.paths.iter().any(|(path, w)| path == Path::new("/tmp/plug") && *w),
            "插件私有目录应可写"
        );
    }

    #[test]
    fn allow_tcp_ports_enables_net_restriction() {
        let p = SandboxPolicy::for_plugin("test-plugin", Path::new("/tmp/plug"), &[]).allow_tcp_ports(&[443]);
        assert!(p.restrict_net);
        assert_eq!(p.net_ports, vec![443]);
    }

    /// 临时目录必须**按插件隔离**，且不再把共享的 `/tmp` 整目录设为可写
    ///
    /// 共享可写 `/tmp` 会让插件之间形成一条宿主看不见的信道
    /// （A 写文件、B 读走）。市场场景下多插件共存是常态，这条信道没有正当用途。
    #[test]
    fn tmp_dir_is_per_plugin_not_shared() {
        let a = SandboxPolicy::for_plugin("plug-a", Path::new("/tmp/a"), &[]);
        let b = SandboxPolicy::for_plugin("plug-b", Path::new("/tmp/b"), &[]);

        // 1) 共享的 /tmp 不应出现在白名单里
        for p in [&a, &b] {
            assert!(
                !p.paths.iter().any(|(path, w)| path == Path::new("/tmp") && *w),
                "不应把共享的 /tmp 整目录设为可写（会形成插件间信道）"
            );
        }
        // 2) 各自拿到不同的、可写的专属目录
        let ta = a
            .paths
            .iter()
            .find(|(path, w)| *w && path.to_string_lossy().contains("fn-kzwr-plugin-"))
            .map(|(path, _)| path.clone())
            .expect("应有插件专属临时目录");
        let tb = b
            .paths
            .iter()
            .find(|(path, w)| *w && path.to_string_lossy().contains("fn-kzwr-plugin-"))
            .map(|(path, _)| path.clone())
            .expect("应有插件专属临时目录");
        assert_ne!(ta, tb, "不同插件的临时目录必须不同");
    }

    /// 插件 id 含路径分隔符时不得逃出白名单目录（`../` 之类）
    #[test]
    fn plugin_tmp_dir_neutralizes_path_separators() {
        let d = SandboxPolicy::plugin_tmp_dir("../../etc");
        let s = d.to_string_lossy();
        assert!(
            !s.contains("..") && !s.contains("/etc"),
            "插件 id 里的分隔符必须被中和，实际得到：{s}"
        );
    }

    /// 施加沙箱后：**未列入白名单**的路径不可读，列入的可读
    ///
    /// 这是本模块存在的**唯一理由**，故必须实证。
    /// 用**显式构造**的策略（而不是 `for_plugin` 的默认清单）来测，
    /// 否则 `/tmp` 之类默认白名单会把测试用的"机密"目录一并放行
    /// —— 那会让测试**假通过**（测出"能读"却以为是沙箱失效）。
    ///
    /// 必须在**独立线程**里施加：Landlock per-thread 且**不可逆**，
    /// 在测试主线程施加会污染同进程的其它测试。
    #[test]
    fn sandbox_blocks_non_whitelisted_paths() {
        if abi_version().is_none() {
            eprintln!("跳过：本内核不支持 Landlock");
            return;
        }
        // 用两个**平级**的临时目录，只把其中一个放进白名单
        let base = std::env::temp_dir().join(format!("kzwr-sbx-{}", std::process::id()));
        let allowed = base.join("allowed");
        let secret = base.join("secret");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&secret).unwrap();
        let allowed_file = allowed.join("ok.txt");
        let secret_file = secret.join("keystore.age");
        std::fs::write(&allowed_file, b"fine").unwrap();
        std::fs::write(&secret_file, b"AGE-SECRET-KEY-1FAKE").unwrap();

        assert!(std::fs::read(&secret_file).is_ok(), "施加前应能读");

        let (af, sf, ad) = (allowed_file.clone(), secret_file.clone(), allowed.clone());
        let (allowed_ok, secret_ok) = std::thread::spawn(move || {
            // **显式**白名单：只有 allowed 目录（不含 /tmp、不含 base）
            let policy = SandboxPolicy {
                paths: vec![(ad.clone(), true)],
                net_ports: Vec::new(),
                restrict_net: false,
            };
            policy.apply_to_current_thread().expect("施加沙箱应成功");
            (std::fs::read(&af).is_ok(), std::fs::read(&sf).is_ok())
        })
        .join()
        .unwrap();

        assert!(allowed_ok, "白名单内的文件应仍可读");
        assert!(
            !secret_ok,
            "**未列入白名单**的密钥文件必须不可读 —— 否则沙箱形同虚设"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// `for_plugin` 的默认策略应**不含**密钥所在目录（配置目录）
    ///
    /// 回归保护：若将来有人把 `/vol1/@appconf` 之类加进默认白名单，
    /// 沙箱就白做了 —— 这条测试会立刻报错。
    #[test]
    fn default_policy_does_not_whitelist_config_dir() {
        let p = SandboxPolicy::for_plugin("test-plugin", Path::new("/tmp/plug"), &[]);
        for (path, _) in &p.paths {
            let s = path.to_string_lossy();
            assert!(
                !s.contains("@appconf") && !s.contains("@appdata"),
                "默认白名单不应包含宿主配置/数据目录（否则插件能读密钥库）：{s}"
            );
        }
    }
}

#[cfg(test)]
mod isolation_tests {
    //! **端到端隔离验证**：沙箱线程必须读不到宿主密钥
    use super::*;

    /// 沙箱线程读 `/vol1/@appconf`（密钥库与主口令所在）应失败
    ///
    /// 这条测试直接验证本模块的**存在理由**：修复前插件能读走
    /// `keystore.age`（私钥密文）+ `.passphrase`（主口令）⇒ 解出能解密
    /// 全部备份的私钥。沙箱后必须挡住。
    ///
    /// 注意：路径可能不存在（开发机 vs NAS），因此**同时**验证
    /// 「自建的等价文件」被挡 —— 后者不依赖部署环境。
    #[test]
    fn sandbox_blocks_host_config_dir_and_lookalike() {
        if abi_version().is_none() {
            eprintln!("跳过：本内核不支持 Landlock");
            return;
        }
        let base = std::env::temp_dir().join(format!("kzwr-iso-{}", std::process::id()));
        let fake_conf = base.join("@appconf-like");
        std::fs::create_dir_all(&fake_conf).unwrap();
        let keystore = fake_conf.join("keystore.age");
        let passphrase = fake_conf.join(".passphrase");
        std::fs::write(&keystore, b"age-encryption.org/v1\nFAKE").unwrap();
        std::fs::write(&passphrase, b"master-pass").unwrap();

        let (ks, pp) = (keystore.clone(), passphrase.clone());
        let own = base.join("plugin-data");
        std::fs::create_dir_all(&own).unwrap();

        let (ks_ok, pp_ok, own_ok) = std::thread::spawn(move || {
            // 插件视角的沙箱：只给自己的数据目录
            let policy = SandboxPolicy {
                paths: vec![(own.clone(), true)],
                net_ports: vec![],
                restrict_net: false,
            };
            policy.apply_to_current_thread().expect("施加沙箱");
            (
                std::fs::read(&ks).is_ok(),
                std::fs::read(&pp).is_ok(),
                std::fs::write(own.join("ok"), b"x").is_ok(),
            )
        })
        .join()
        .unwrap();

        assert!(!ks_ok, "**沙箱必须挡住 keystore.age**（否则能解出备份私钥）");
        assert!(!pp_ok, "**沙箱必须挡住 .passphrase**（主口令是真正的钥匙）");
        assert!(own_ok, "插件自己的数据目录应可写（否则插件无法工作）");

        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod memory_isolation_tests {
    use super::*;

    /// **沙箱必须挡住 `/proc/self/mem`**（否则私钥可被插件直接读走）
    ///
    /// ## 这条为什么最重要
    /// 插件与宿主**同进程**。若插件能读 `/proc/self/mem`，它就能：
    /// 1. 读 `/proc/self/maps` 拿到 `state.crypto` / `passphrase` 的地址；
    /// 2. `pread(/proc/self/mem)` 直接取出内存里的 `AGE-SECRET-KEY-*`；
    /// ⇒ **完全绕过** `keystore.age` 的加密与 0600 权限，全部备份可被解密。
    ///
    /// 旧实现给 `/proc/self` 加的是 `RO`（含 `FS_READ_FILE`），实测 mem/environ
    /// 全部可读 —— 沙箱的存在理由被架空。现改为「目录只给 READ_DIR + 逐文件放行」。
    #[test]
    fn sandbox_blocks_proc_self_mem_and_environ() {
        let own = std::env::temp_dir().join(format!("kzwr-memtest-{}", std::process::id()));
        std::fs::create_dir_all(&own).ok();
        let policy = SandboxPolicy::for_plugin("memtest", &own, &[]);
        if policy.apply_to_current_thread().is_err() {
            return; // 内核不支持 Landlock：本环境无法验证（不留假绿）
        }
        // 沙箱生效后探测
        let mem = std::fs::File::open("/proc/self/mem");
        assert!(
            mem.is_err(),
            "/proc/self/mem 必须被拒绝（否则插件可直接读出内存里的 age 私钥）"
        );
        let env = std::fs::File::open("/proc/self/environ");
        assert!(
            env.is_err(),
            "/proc/self/environ 必须被拒绝（否则会泄漏 TRIM_PASSPHRASE 等环境变量）"
        );
        // 但插件正常需要的几项必须仍然可读（不能把功能挡死）
        assert!(
            std::fs::File::open("/proc/self/status").is_ok(),
            "/proc/self/status 应保持可读（很多运行时依赖它）"
        );
        assert!(
            std::fs::File::open("/dev/urandom").is_ok(),
            "/dev/urandom 必须可读（否则随机数失败，插件无法工作）"
        );
    }
}

#[cfg(test)]
mod socket_gap_tests {
    use super::*;

    /// **记录已知缺口**：Landlock 不拦 Unix socket 的 `connect()`
    ///
    /// 这不是「期望行为」，而是把实测结论固化成测试：
    /// 若将来换用 seccomp / 进程隔离补上这个洞，本测试会失败 —— 那时应当
    /// 把它改成「必须被拒绝」，正好是升级的验收信号。
    ///
    /// 之所以值得单独测：`PLUGIN_ISOLATION.md` 与模块注释都容易让人误以为
    /// 「施加了 Landlock 就碰不到 socket」。这个误会会让后续的安全评估失真。
    #[test]
    fn landlock_does_not_block_unix_socket_connect() {
        // 在施加沙箱**之前**建好一个可连的 unix socket（放在不被白名单覆盖的目录）
        let dir = std::env::temp_dir().join(format!(
            "kzwr-sockgap-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).ok();
        let sp = dir.join("t.sock");
        let _ = std::fs::remove_file(&sp);
        let sp_c = CString::new(sp.as_os_str().as_encoded_bytes()).unwrap();

        // bind + listen
        let srv = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(srv >= 0, "创建 socket 失败");
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (i, b) in sp_c.as_bytes().iter().enumerate() {
            addr.sun_path[i] = *b as libc::c_char;
        }
        let alen = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
        let r = unsafe { libc::bind(srv, &addr as *const _ as *const libc::sockaddr, alen) };
        assert_eq!(r, 0, "bind 失败: {}", std::io::Error::last_os_error());
        unsafe { libc::listen(srv, 1) };
        // 放宽权限，排除 DAC 干扰（本测试只想看 Landlock 的行为）
        let _ = std::fs::set_permissions(&sp, std::os::unix::fs::PermissionsExt::from_mode(0o777));

        // 施加一份**故意不含该目录**的白名单策略
        let policy = SandboxPolicy {
            paths: vec![
                (PathBuf::from("/proc/self"), false),
                (PathBuf::from("/usr"), false),
            ],
            net_ports: Vec::new(),
            restrict_net: false,
        };
        if policy.apply_to_current_thread().is_err() {
            // 内核不支持 Landlock：本环境无法验证，直接跳过（不留假绿）
            unsafe { libc::close(srv) };
            return;
        }

        // 沙箱生效后尝试 connect
        let c = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(c >= 0);
        let rc = unsafe { libc::connect(c, &addr as *const _ as *const libc::sockaddr, alen) };
        let err = std::io::Error::last_os_error();
        unsafe {
            libc::close(c);
            libc::close(srv);
        }
        let _ = std::fs::remove_file(&sp);
        let _ = std::fs::remove_dir(&dir);

        // 断言「当前仍是缺口」：connect 成功。若这里失败，说明已经补上，应改注释与文档。
        assert_eq!(
            rc, 0,
            "connect 被拒绝了（{err}）—— 说明 Unix socket 缺口已被补上，             请更新 sandbox.rs 模块头的「已知缺口」说明与本测试"
        );
    }
}
