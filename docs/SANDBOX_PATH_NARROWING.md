# 沙箱白名单路径收窄评估

> 状态：**已评估，未实施** · 日期 2026-11-24 · 关联 `backend/src/plugin/sandbox.rs`
> 结论：**技术可行**，但需逐插件验证后再落地。

## 1. 评估对象

`SandboxPolicy::for_plugin` 目前对 `/etc` 与 `/usr` 做**整目录只读放行**：

```rust
(PathBuf::from("/etc"), false),
(PathBuf::from("/usr"), false),
```

`/etc` 下有 `passwd` / `shadow` / 各类服务凭据；`/usr` 下有全部二进制与库。
插件只需要其中很小一部分（证书、DNS、时区、动态库）。

## 2. 实测方法

复刻 `sandbox.rs` 的 ruleset（Landlock ABI v7，内核 `6.18.18.c1186-trim`），
把 `/etc`、`/usr` 换成精确子路径后，在沙箱内探测插件运行时依赖。

> ⚠️ 评估脚本第一版有 bug：给**普通文件**用含目录位（`FS_READ_DIR`）的规则
> 会被内核判 `EINVAL`，导致 `/etc/hosts` 等"看起来被拒"。
> 修正为「按 `fstat` 判断对象类型选权限位」后结果才可信
> —— 这与 `sandbox.rs` 里已修复的 `/dev/urandom` 是同一个坑。

## 3. 评估结果

拟收窄清单（全部**路径存在时**才加规则）：

```
/etc/ssl/certs          TLS 根证书
/etc/localtime          本地时区
/etc/nsswitch.conf      名字服务
/etc/resolv.conf        DNS（软链 → /run/resolvconf/resolv.conf）
/etc/hosts              hosts
/etc/alternatives       候选链接
/usr/lib                动态库
/usr/lib64              动态库（部分发行版）
/usr/share/zoneinfo     时区库
```

探测结果：

| 依赖 | 收窄后 |
|---|---|
| `/etc/ssl/certs/ca-certificates.crt`（TLS 根证书） | ✅ ALLOWED |
| `/etc/ssl/certs`（证书目录） | ✅ ALLOWED |
| `/etc/localtime`（本地时区） | ✅ ALLOWED |
| `/etc/resolv.conf`（DNS） | ✅ ALLOWED |
| `/etc/hosts` | ✅ ALLOWED |
| `/etc/nsswitch.conf` | ✅ ALLOWED |
| `/usr/lib/x86_64-linux-gnu/libc.so.6`（动态库） | ✅ ALLOWED |
| `/usr/share/zoneinfo/Asia/Shanghai`（时区库） | ✅ ALLOWED |
| `/etc/passwd`（**收窄后应拒**） | ✅ Permission denied |

**规则采纳率 6/6（`/etc`）、3/3（`/usr`）**，无一条被内核拒绝。

## 4. 两个必须注意的细节

### 4.1 `/etc/resolv.conf` 是符号链接

```
/etc/resolv.conf -> /run/resolvconf/resolv.conf
```

`O_PATH` 打开会**跟随软链**，规则实际落在真实路径上。实测确认：
通过软链路径访问同样被放行（内核解析后落到同一 inode），**无需额外处理**。

但若某个发行版的 `resolv.conf` 指向插件数据目录之外的白名单未覆盖位置，
应把**真实路径**也加入清单。

### 4.2 权限位必须按对象类型选

给普通文件加目录专属位（`FS_READ_DIR` / `FS_MAKE_DIR` / `FS_REMOVE_DIR`）
会被内核判 `EINVAL`，规则**静默失效**。这正是 `/dev/urandom` 长期失效的原因
（见 `sandbox.rs` 的 `RO_FILE` / `RW_FILE` 常量）。

## 5. 实施建议

**不建议直接改**，理由：

1. **有把插件弄坏的风险**。现有插件（kzwr / wopan）与未来第三方插件可能依赖
   `/etc` 或 `/usr` 下的其它路径（如 `/usr/bin/` 下的外部命令、`/etc/` 下的
   自定义 CA `ca-certificates` 之外的 bundle）。规则漏了，插件拿到的是
   `EACCES`，而错误来源很难定位。
2. **收窄的收益有限**。Linux 上 `/etc/shadow`、`/etc/gshadow` 本就是 `root:shadow 0640`，
   沙箱即使放行整目录，DAC 仍然挡住。真正被整目录暴露的是
   `/etc/passwd`（0644）、`/etc/group`（0644）、`/proc` 挂载信息等**低敏**内容。
3. **当前更值得收窄的是 `/proc/self`**，那条已经做了（`RO_DIR_ONLY` + 逐文件放行），
   它挡住的是 `mem`/`environ` 这条**真正能读走私钥**的通道。

### 落地路径（若仍要做）

1. 先加一个**只告警不拦截**的观测模式：在插件回调里记录它实际 `open()` 成功过
   哪些 `/etc`、`/usr` 路径（可用 `strace -f -e trace=openat` 在测试环境采样）。
2. 依实测结果补齐白名单，并在三种插件上跑完整备份/恢复。
3. 提供 `FN_KZWR_SANDBOX_WIDE_ETC=1` 逃生舱，出问题可立刻恢复。

## 6. 一句话结论

**技术可行、实测通过**，但收益（挡住 `/etc/passwd` 这类公开文件）与风险
（插件因缺路径而 `EACCES`）不成正比。建议**暂不收窄**，把力气放在
`/proc/self`（已做）与「插件专属临时目录」这两处真正敏感的边界上。
