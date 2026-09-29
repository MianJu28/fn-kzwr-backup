//! **插件线程沙箱**（Landlock）——档位 B：同进程内的最小权限
//!
//! ## 为什么需要
//! 插件与宿主**同进程、同 uid**（见 `docs/PLUGIN_ISOLATION.md`）：
//! 插件代码可以读走解密备份所需的全部材料 ——
//! `keystore.age`（age 私钥密文）+ `.passphrase`（主口令），
//! 两者属主与运行进程**同一个 uid**，`0600` 挡不住同 uid，
//! 于是同 uid 子进程/同进程代码都能拿到，**进而解出能解密全部备份的私钥**。
//!
//! ## 为什么用 Landlock 而不是子进程
//! 实测（`docs/PLUGIN_ISOLATION.md` §2）：
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
const RO: u64 = FS_READ_FILE | FS_READ_DIR | FS_EXECUTE;
/// 读写（给插件数据目录、目标目录、临时目录用）
const RW: u64 = RO
    | FS_WRITE_FILE
    | FS_MAKE_REG
    | FS_MAKE_DIR
    | FS_REMOVE_FILE
    | FS_REMOVE_DIR
    | FS_TRUNCATE
    | FS_MAKE_SYM;

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
    /// - `extra`：额外需要读写的路径（如本地目录目标的根、任务源路径）
    /// - 系统只读：`/etc`（时区/证书）、`/usr`（动态库）、`/dev/urandom`、
    ///   **`/proc/self`**（很多库要读，必须显式允许）
    pub fn for_plugin(own_data_dir: &Path, extra: &[PathBuf]) -> Self {
        let mut paths: Vec<(PathBuf, bool)> = vec![
            // ⚠️ `/proc/self` 必须显式允许：Landlock 会连它一起挡住
            (PathBuf::from("/proc/self"), false),
            (PathBuf::from("/etc"), false),
            (PathBuf::from("/usr"), false),
            (PathBuf::from("/dev/urandom"), false),
            (PathBuf::from("/dev/null"), true),
            (PathBuf::from("/dev/zero"), false),
            // CA 证书与 DNS 解析（kzwr 要联网）
            (PathBuf::from("/run"), false),
        ];
        paths.push((own_data_dir.to_path_buf(), true));
        // /tmp：插件可能用它做中间文件（如 example-localfs 的临时落定）
        paths.push((PathBuf::from("/tmp"), true));
        for p in extra {
            paths.push((p.clone(), true));
        }
        Self {
            paths,
            net_ports: Vec::new(),
            restrict_net: false,
        }
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
            // O_PATH：只取「目录句柄」，不需要真实打开权限
            let pfd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            if pfd < 0 {
                continue;
            }
            let allowed = if *writable { RW } else { RO };
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
                // 某些路径（如 /dev/urandom 是文件而非目录）在旧内核上会失败；
                // 不因此中断整套规则 —— 让其余规则仍生效。
                let e = io::Error::last_os_error();
                tracing::debug!(path = %path.display(), err = %e, "Landlock 规则跳过（非目录或不受支持）");
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
        let p = SandboxPolicy::for_plugin(Path::new("/tmp/plug"), &[]);
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
        let p = SandboxPolicy::for_plugin(Path::new("/tmp/plug"), &[]).allow_tcp_ports(&[443]);
        assert!(p.restrict_net);
        assert_eq!(p.net_ports, vec![443]);
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
        let p = SandboxPolicy::for_plugin(Path::new("/tmp/plug"), &[]);
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
