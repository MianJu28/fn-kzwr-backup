# fn-kzwr-backup · 架构设计文档

> 将飞牛 NAS 文件增量加密备份至酷族网软（官方 WebDAV），支持 GUI 管理与选择性恢复。

> **文档定位**：本文件描述**当前已实现**的系统 —— 架构、模块、目录结构、技术栈、
> 质量属性与安全模型。**开发过程档案**（架构决策记录 ADR、演进路线图、逐版本进度、
> 方案评审、选型分析）已移至 `docs/memory/dev/`，不入版本库；
> 其中每条决策的**编号**在下文仍被引用，索引见文末「决策记录索引」。

---

## 1. 项目背景与约束

| 维度 | 约束 |
|------|------|
| 运行环境 | fnos（飞牛 NAS OS，嵌入式 Linux） |
| 部署形态 | 原生应用，随系统启动，单机运行 |
| 资源 | NAS 硬件资源有限（CPU/内存敏感） |
| 可用性 | 7×24 长期运行，需稳定可靠 |
| 数据源 | 飞牛 NAS（本地 FS） |
| 备份目标 | 酷族网软（官方 WebDAV，Basic 认证；逆向 REST API 与登录二进制已完全移除） |
| 核心能力 | 增量备份 · age 公私钥加密 · GUI 管理 · 选择性恢复 |
| 安全要求 | 私钥永不明文落盘；明文不落临时盘 |

---

## 2. 领域模型

### 2.1 限界上下文映射

系统划分为 7 个限界上下文，按战略价值分三类：

| 分类 | 上下文 | 聚合根 | 职责 |
|------|--------|--------|------|
| 核心域 | 备份调度 | `BackupJob` | 任务生命周期、Cron 调度、保留策略 |
| 核心域 | 增量同步 | `SyncSession` | 文件扫描、差分、ChangeSet 生成 |
| 核心域 | 加密 | `CryptoSession` | age 分块加密、密钥会话管理 |
| 核心域 | 恢复 | `RestoreJob` | 选择性恢复、完整性校验 |
| 支撑域 | 存储抽象 | — | Source/Target 适配器（ACL 防腐层） |
| 支撑域 | 元数据索引 | — | 文件快照、SQLite 持久化 |
| 通用域 | 管理 UI/API | — | 任务 CRUD、状态聚合、恢复向导 |

### 2.2 上下文关系

```
管理UI → 备份调度 → 增量同步 → (加密 ‖ 存储) → 元数据索引
恢复 ← 元数据索引 + 存储(目标) + 解密
飞牛NAS = 上游(只读) · 酷族网软 = 下游(读写) · 存储层用 ACL 隔离双方协议
```

### 2.3 关键领域规则

- **增量检测双策略**：快速路径 `mtime + size`（O(1)，覆盖 99% 场景）；严格路径 `BLAKE3` 内容哈希（防 mtime 欺骗、精确去重）。用户可按任务配置。
- **加密分块**：64MB 独立 chunk，每个 chunk 用 age 独立加密。这让"选择性恢复"能随机定位 chunk 解密，无需从头解密整文件。
- **密钥不变量**：age 私钥永不明文落盘；备份用 `age` 公钥加密、恢复用 `age` 私钥解密；私钥本身可再被口令派生密钥加密存储于密钥库。

---

## 3. 架构模式：模块化单体

### 3.1 选型结论

**模块化单体（Modular Monolith）+ 内部事件总线（旁路）**。

### 3.2 权衡

| 模式 | 结论 | 理由 |
|------|------|------|
| 模块化单体 | ✓ 采纳 | 单进程低开销、部署简单、契合 fnos 原生形态；模块边界清晰可独立演进；后续可拆分 |
| 微服务 | ✗ 否决 | NAS 单机无需水平扩展；多进程内存与 IPC 开销是纯负担；运维复杂度过高 |
| 事件驱动 | ~ 局部采用 | 主流程需强一致性（每块加密必须确认落盘）；仅用于通知/审计/状态广播等旁路 |

### 3.3 代价

- 需团队自律维持模块边界（通过 lint、依赖检查工具强制）
- 共享 SQLite 需约定表归属，避免跨模块直接读写他方表
- 单模块无法独立水平扩展（NAS 场景下不是问题）

---

## 4. C4 架构视图

### 4.1 Level 1 · 系统上下文

系统与四个外部实体交互：
- **用户**：通过飞牛桌面入口（iframe）访问 Web UI，配置任务、监控状态、执行恢复
- **飞牛 NAS**：数据源（只读），仅本地 FS；需用户授权目录访问
- **酷族网软**：备份目标（读写），官方 WebDAV 协议（逆向 REST API 已从代码库移除，ADR-009）
- **fnos OS**：提供应用生命周期框架（`cmd/main` 脚本 start/stop/status）、桌面入口注册、权限模型（run-as=package）、标准目录结构、日志、`.fpk` 包管理

### 4.2 Level 2 · 模块化单体内部架构

单进程 Rust HTTP 服务 + Svelte SPA，自上而下六层。应用以飞牛**普通应用**形态部署（非 Docker），通过 `cmd/main` 脚本控制进程生命周期，HTTP 端口服务暴露 UI。

| 层 | 模块 | 技术栈 |
|----|------|--------|
| 表现层 | Svelte SPA（桌面入口 iframe 加载） | Svelte + Vite 构建产物，由 axum 静态托管 |
| 接口层 | REST API + WebSocket（状态推送） | axum · tower · serde_json |
| 应用编排层 | 备份调度 / 恢复编排 | Rust + tokio |
| 领域核心层 | 增量同步 / 加密 / 元数据索引 | Rust（纯领域逻辑，无 IO 依赖） |
| 基础设施层 | Source 适配器 / Target 适配器 / fnos 集成 | Rust + 协议 SDK · 生命周期脚本 |
| 数据层 | SQLite / 配置+密钥库 / 结构化日志 | rusqlite(WAL) · TOML · tracing · 路径用 `TRIM_*` 环境变量 |

**飞牛集成关键点**：
- 进程由 `cmd/main` 的 `start`/`stop`/`status` 控制，非 systemd unit
- 路径禁止硬编码，使用 `TRIM_APPDEST`(应用文件)/`TRIM_PKGETC`(配置)/`TRIM_PKGVAR`(持久数据)/`TRIM_APPTMP`(临时) 等环境变量
- 运行身份 `run-as=package`（专用用户 `fnosbackup`），最小权限
- 用户文件访问需明确授权（`config/resource` 声明共享目录或用户在设置中授权）
- 桌面入口 `app/ui/config` 声明 iframe 指向 `http://localhost:{port}/`

### 4.3 增量备份数据流

```
飞牛NAS →[Source流式读]→ 扫描+差分 →[ChangeSet]→ 加密管道 →[密文流]→ 酷族网软
                              ↑↓
                    元数据索引(SQLite WAL)
                    (查上次快照 / 写新快照, 用于增量差分)

加密管道内部: 文件流 → 分块64MB → age 公钥加密(每 chunk 独立) → age 密文
目标: Rust WebDAV 客户端(reqwest) 读写酷族官方 WebDAV
      (HTTP Basic 凭据，UI 配置后加密存储并热切换生效；逆向 REST API 与登录二进制已移除，见 ADR-009)

事件旁路(不阻塞主流程): FileSynced / BackupCompleted → 通知用户 / 审计日志
```

**安全属性**：全程流式，明文不落盘（内存仅当前 chunk）；元数据反馈环闭合；事件旁路解耦。

---

## 5. 技术栈

| 层 | 选型 | 备选 | 选择理由 |
|----|------|------|----------|
| 后端语言 | Rust | Go | 内存安全无 GC 暂停；加密场景关键；单二进制部署 |
| HTTP 后端 | axum | actix-web / rocket | tokio 原生集成；类型安全路由；既提供 API 又托管前端静态文件 |
| 前端框架 | Svelte + Vite | Vue / React | 体积小；编译时优化；构建产物放 `app/www/` 由 axum 静态服务 |
| 实时通信 | WebSocket（axum） | SSE / 轮询 | 任务状态、进度推送；飞牛 iframe 内支持 |
| 加密 | age（X25519 公私钥 + ChaCha20-Poly1305） | Picocrypt / GPG | 用户指定；现代公钥加密标准；流式友好；`age` crate 纯 Rust |
| 哈希 | BLAKE3 | SHA-256 | 速度更快（SIMD）；树形结构支持并行/流式 |
| 异步运行时 | tokio | async-std | 生态成熟；流式 IO 与调度器支持完善 |
| 数据库 | SQLite (rusqlite + WAL) | sled / 嵌入式 KV | 嵌入式零配置；SQL 表达力强；WAL 支持并发读写 |
| 飞牛打包 | fnpack → `.fpk` | Docker 镜像 | 普通应用形态；原生访问文件系统；x86_64+aarch64 双架构 |
| 页面访问 | 飞牛统一网关（Unix Socket）+ hyper/hyper-util | 独立 HTTP 端口 | 宿主同源反代到 `/app/{appname}`：跟随宿主 http/https（无混合内容拦截）、支持 WebSocket、可复用 NAS 登录态（ADR-012） |
| 终端日志 | tracing-subscriber（**关闭 ANSI**） | 默认带颜色码输出 | 日志文件会被日志页读取/下载，颜色码会显示成 `[2m`/`[32m` 乱码，故 `with_ansi(false)`（读取侧再兜一层剥离） |
| 源访问 | tokio::fs（本地 FS） | smb-rs / pavao | 仅需本地文件备份，不考虑 SMB/NFS，零依赖 |
| 目标访问 | reqwest（WebDAV 客户端） | rust-s3 | 酷族官方 WebDAV（ADR-009）；逆向 REST API 已移除 |
| 大文件上传 | 自定义 `http_body`（精确 `size_hint`） | 纯流式 body（chunked） | 分片 PUT 保留 `Content-Length` 的同时按块上报进度；chunked 可能被站点/网关拒绝（413） |
| 配置 | TOML + serde | YAML / JSON | 人类可读；Rust 生态一等支持 |
| 日志 | tracing + tracing-subscriber | log + env_logger | 结构化日志；span 追踪；异步友好 |
| 调度 | `croner`（cron 解析）+ 自建 tokio 轮询循环 | tokio-cron-scheduler / 系统 cron | 不依赖系统 cron，可移植；进程内调度，便于中途检测配置热更新 |

---
---

## 6. 安全模型

| 资产 | 保护方式 |
|---|---|
| **备份明文** | 全程流式，不落盘（内存仅当前 chunk）；age 公钥加密后才离开本机 |
| **age 私钥** | 永不明文落盘：经口令（age scrypt）派生密钥加密后存 `keystore.age`（权限 0600） |
| **主口令** | 由安装向导写入 `$TRIM_PKGETC/.passphrase`（权限 0600），启动时经环境变量注入进程 |
| **WebDAV 凭据** | TOML 中以 `enc:<age密文>` 存储；保存时 ping 验证并热切换 |
| **插件配置** | **宿主不再代存**（ADR-021）：插件写自己的 `own_data_dir`，敏感内容经宿主能力表 `seal`/`unseal` 加密 |
| **插件代码** | Ed25519 强制验签（内置官方公钥 + 可配公钥）；无签名则不加载 |
| **插件运行时** | 每插件**专属线程 + Landlock 沙箱**（ADR-023）：白名单模式，`keystore.age`/`.passphrase`/宿主配置目录均不可读，并顺带挡住 `/proc/<宿主pid>/mem` |
| **敏感操作** | 导出私钥 / 配置导入导出 / 锁屏等需管理员口令校验；凭据**永不回传前端**（只回显 `configured` 布尔） |
| **审计** | 凭据/密钥/配置/备份/恢复/回收站操作写入 `audit.log`（JSON Lines，512KB 自动裁剪） |

> **已知边界**：插件与宿主**同进程**。沙箱挡住磁盘上的密钥与口令，但
> **同进程内存仍可读**（age 私钥与主口令在宿主内存中）。彻底隔离需子进程模型，
> 触发条件与方案见 `docs/memory/dev/PLUGIN_ISOLATION.md`。
> 插件卡死无法强杀，看门狗只能判失败并泄漏一个专属线程。

---
## 7. 项目目录结构

项目采用**开发期两段式**：`backend/`（Rust）与 `frontend/`（Svelte）独立演进；
打包时由 `Scripts/build_fnos_app.sh` 合入飞牛应用目录（源在 `packaging/`）。

### 7.1 仓库结构（开发期，对应本仓库）

```
fn-kzwr-backup/
├── backend/                    # Rust 后端
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs             # 入口：启动 axum / 绑 Unix Socket / 加载插件
│       ├── lib.rs              # AppState（配置·插件·目标池·事件总线·告警·审计）
│       ├── eventbus.rs         # 内部事件总线（tokio::broadcast）
│       ├── http/               # 接口层
│       │   ├── routes/         # 路由（按职责拆分，见 7.3）
│       │   └── ws.rs           # WebSocket 状态推送
│       ├── domain/             # 领域层（纯逻辑）
│       │   ├── backup.rs       # 备份调度 BackupJob
│       │   ├── sync.rs         # 增量差分 SyncSession
│       │   ├── crypto.rs       # age 加密 CryptoSession + AgeKeys
│       │   ├── restore.rs      # 恢复编排 RestoreJob
│       │   ├── retention.rs    # 保留策略（孤儿清理）
│       │   ├── scheduler.rs    # **每任务**独立 cron 调度
│       │   ├── pace.rs         # SpeedMeter 速度计量
│       │   ├── alerts.rs       # 告警 + Webhook 外发
│       │   └── audit.rs        # 操作审计
│       ├── plugin/             # 插件层（ADR-013）
│       │   ├── api.rs          # 内部契约 TargetPlugin / EnhancePlugin / PluginUi
│       │   ├── abi.rs          # **稳定 C ABI v1 契约**（主表 + 目标能力表）
│       │   ├── cabi.rs         # C ABI → EnhancePlugin 适配（catch_unwind 兜底）
│       │   ├── target_abi.rs   # C ABI → TargetStorage 适配（含并发回传）
│       │   ├── host_abi.rs     # 宿主能力表实现（日志/审计/告警/进度/定时/seal）
│       │   ├── sandbox.rs      # **Landlock 沙箱**（ADR-023）
│       │   ├── worker.rs       # **插件专属线程 / 目标实例线程池**（沙箱载体）
│       │   ├── crypto.rs       # seal/unseal 原语（主口令派生）
│       │   ├── loader.rs       # 目录扫描 + libloading + 验签 + 失败隔离
│       │   └── registry.rs     # 唯一装配点
│       └── infra/              # 基础设施（ACL 防腐）
│           ├── storage_trait.rs # Source/Target trait + TargetPool + SwapTarget
│           ├── config.rs       # TOML：targets/tasks + 凭据加解密 + 旧字段迁移
│           ├── keystore.rs     # 密钥库（0600）
│           ├── source/local/   # LocalFsSource
│           └── target/webdav.rs # WebdavTarget（官方 WebDAV）
├── frontend/                   # Svelte 前端
│   └── src/
│       ├── App.svelte          # 应用壳：导航 + 全局状态 + WebSocket
│       ├── views/              # 页面（见 7.4）
│       ├── components/         # 功能区块（见 7.4）
│       └── lib/                # api / 工具
├── plugins/                    # 插件源码（独立 crate）
│   ├── sdk/                    # **插件 SDK**（零第三方依赖；稳定 ABI 的唯一依赖面）
│   ├── kzwr/                   # 增强插件（外置）：空间/账号/回收站，自管配置
│   ├── example-localfs/        # 目标插件示范：本地目录目标（声明 target.form + path_fields）
│   └── example-hello/          # 最小示例
├── packaging/fn-kzwr-backup-app/ # 飞牛应用**打包源**（见 7.2）
├── Scripts/                    # build_fnos_app.sh / build_plugins.sh / sign_plugin.sh
└── docs/
    ├── ARCHITECTURE.md         # 本文件：当前实现的架构基线
    ├── PLUGIN_ABI.md           # 插件接口契约（稳定 C ABI v1）
    ├── fnnas-dev-docs/         # 抓取的飞牛官方文档镜像（只读参考）
    └── promo/                  # 应用介绍用宣传图
```

### 7.2 飞牛应用部署形态（打包产物）

`Scripts/build_fnos_app.sh` 把后端二进制、前端产物与插件合入 `packaging/fn-kzwr-backup-app/`
（**打包源已入库**，产物输出到 `dist/`，不入库）：

```
packaging/fn-kzwr-backup-app/
├── manifest                    # platform=x86, ctl_stop=true, checkport=false（无 service_port）
├── ICON.PNG / ICON_256.PNG
├── app/                        # → $TRIM_APPDEST
│   ├── ui/config               # 桌面入口：统一网关（gatewayPrefix + gatewaySocket=app.sock）
│   ├── bin/fn-kzwr-backup      # 后端二进制（构建时拷入）
│   ├── plugins/*.so (+ .sig)   # 外置插件与其签名（构建时拷入）
│   └── www/                    # 前端产物（构建时拷入）
├── cmd/                        # 生命周期脚本 main/install_*/upgrade_*/uninstall_*/config_*
├── config/
│   ├── privilege               # run-as=package, user/group=fnosbackup
│   └── resource                # data-share + api-scope 声明
└── wizard/                     # install / config / uninstall（JSON 步骤数组）
```

**关键约束**（对照飞牛规范）：
- **无 TCP 端口**：UI 经统一网关（`/app/fn-kzwr-backup` + Unix Socket）暴露（ADR-012）
- **路径全部用 `TRIM_*` 环境变量**，禁止硬编码
- **权限**：`run-as=package` 专用用户；`disable_authorization_path=false`，源目录由用户授权
- **口令**：安装向导写入 `$PKGETC/.passphrase`（0600），用于配置与密钥库加密
- **数据归属**：快照 → `$TRIM_PKGVAR`；配置 / 密钥库 → `$TRIM_PKGETC`
- **链接方式**：**glibc 动态链接**（musl 静态不支持 `cdylib` 且无法 `dlopen`，与外置插件互斥）

**构建**：`cargo build --release` + `npm run build` → `Scripts/build_fnos_app.sh` 组装
（含插件构建与签名）→ `fnpack build` 产出 `.fpk` 至 `dist/`。
CI 见 `.github/workflows/build-fnos-app.yml`（x86_64 / aarch64 双架构，tag 发布强制签名）。

### 7.3 后端路由模块划分

原 `http/routes.rs` 单文件 4464 行，已按职责拆分（ADR-022）：

| 模块 | 内容 |
|---|---|
| `mod.rs` | `router` 装配 + 插件动作分发（唯一通配路由） |
| `types.rs` | 请求/响应 DTO |
| `common.rs` | 错误响应 / `new_id` / 告警 / 日志清洗 |
| `plugins.rs` | 插件管理（列表/启停/热重载/安装卸载/清除数据） |
| `targets.rs` | 目标管理（凭据永不回传明文） |
| `tasks.rs` | 任务管理 + `run_backup_now` / `run_task_now` |
| `restore.rs` | 恢复（列表/目录树/执行/清理缺失） |
| `config.rs` | 配置读写 + 导出导入（需口令） |
| `logs.rs` | 运行日志（查看/清空/下载） |
| `audit.rs` | 审计 + 一键体检 + 定时预览 |
| `keys.rs` | 密钥 + 告警 + Webhook |

### 7.4 前端结构

```
frontend/src/
├── App.svelte              # 应用壳：导航 + 页面切换 + 全局状态/WebSocket
├── views/                  # 页面级组件
│   ├── DashboardPage.svelte   # 概览：一键体检 + 按任务/目标聚合统计
│   ├── TasksPage.svelte       # 任务管理（多任务）
│   ├── TargetsPage.svelte     # 目标管理（多目标）
│   ├── PluginsPage.svelte     # 插件页：插件卡片（通用 UI Schema）+ 外置插件管理
│   ├── RestorePage.svelte     # 恢复
│   ├── AuditPage.svelte       # 操作审计
│   ├── LogsPage.svelte        # 日志（倒序、宿主时区）
│   └── SettingsPage.svelte    # 设置：age 密钥 / 通知 / 配置迁移
├── components/             # 功能区块组件
│   ├── PluginBlocks.svelte      # **通用 UI Schema 渲染**（所有插件卡片）
│   ├── TargetEditModal.svelte   # **通用目标表单弹窗**（字段由插件 target.form 声明）
│   ├── PluginSettingsModal.svelte # 通用插件设置弹窗
│   ├── LiveStatus.svelte        # 实时任务状态（右侧常驻面板）
│   ├── KeySection.svelte        # age 密钥管理
│   ├── MessagesPanel.svelte     # 「消息提醒」留存型通知出口
│   ├── PluginSection.svelte     # 外置插件管理（开关/目录/公钥/诊断/卸载）
│   ├── NotifySection.svelte     # 通知设置（Webhook + 模板 + 连通性测试）
│   ├── ConfigSection.svelte     # 配置导出/导入
│   ├── AuditSection.svelte / SetupCheckSection.svelte / RestoreSection.svelte
│   └── ConfirmDialog / Toast / Icon / Logo / PluginAccounts / PluginInstallModal
└── TreeNode.svelte         # 目录树节点（懒加载）
```

> **前端完全声明式**：插件界面由 `ui.blocks[]` 驱动 `PluginBlocks.svelte`，
> 目标表单由 `target.form[]` 驱动 `TargetEditModal.svelte` ——
> **新增插件无需改前端、无需重打包**（ADR-019 / ADR-020）。

---

## 8. 质量属性分析

### 8.1 可扩展性
- 单进程内模块可独立演进，新增存储协议只需实现 trait
- SQLite 单机写入上限约数千 TPS，NAS 备份场景足够
- 若未来需多 NAS 集中备份，可将调度模块拆为独立服务（模块化单体的演进优势）

### 8.2 可靠性
- 断点续传：每 chunk 落盘后记录元数据，中断后从断点恢复
- 完整性校验：AEAD tag 自动验证；恢复后可选 BLAKE3 复核
- 故障隔离：单个备份任务失败不影响其他任务；事件总线故障不影响主流程

### 8.3 可维护性
- 模块边界清晰，依赖方向单向
- ADR 记录所有重大决策的"为什么"
- trait 抽象使核心逻辑可单元测试（mock storage）

### 8.4 可观测性
- tracing 结构化日志 + span 追踪跨模块调用链
- 事件总线天然提供审计流
- Web UI 实时聚合任务状态（通过 WebSocket 推送，飞牛 iframe 内支持）

### 8.5 安全性
- age 私钥永不明文落盘（可被口令派生密钥加密存储于密钥库）
- 明文不落临时盘（全程流式）
- age 底层 ChaCha20-Poly1305 AEAD 认证加密防篡改
- 每 chunk 独立文件密钥（age ephemeral key）防重放

## 9. 技术选型状态

> 详细选型分析（候选方案、推荐理由、验证步骤）见 `docs/memory/dev/TECH_SELECTION.md`（开发期存档）。

### 已定选型

| 选型项 | 结论 | 依据 |
|--------|------|------|
| 酷族网软对接 | 官方 WebDAV（Basic 凭据，加密存储，保存时 ping 验证并热切换） | `infra/target/webdav.rs`；REST 与登录器已移除（ADR-009） |
| 飞牛源访问 | 仅本地 FS（`tokio::fs`），不考虑 SMB/NFS | `infra/source/local/` |
| 双架构编译 | **glibc 动态链接** + cross 工具，Actions matrix（x86_64 / aarch64） | **须**动态链接：musl 静态不支持 `cdylib` 且无法 `dlopen`，与外置插件互斥（见 §7.2） |
| 源目录授权 | `config/resource` 声明（`data-share`）+ 运行时引导，弃 root | `packaging/.../config/resource`；`run-as=package` |
| UI 暴露认证 | 统一网关 + **敏感操作口令校验**（未采用 JWT/全站登录） | 网关见 ADR-012；导出私钥/配置导入导出校验管理员口令 |
| 密钥管理 | age 公私钥（X25519）；备份用公钥加密、恢复用私钥解密；私钥经口令（age scrypt）加密存储 | `infra/keystore.rs`（`keystore.age`，0600）；见 §6 |

---

## 10. 决策记录索引

> 完整 ADR（背景、取舍过程、后果）见 `docs/memory/dev/ADR.md`（开发期存档，不入版本库）。
> 此表只保留**编号 → 决策 → 对当前实现的影响**，供正文引用时查阅。

| 编号 | 决策 | 对当前实现的影响 |
|---|---|---|
| ADR-001 | 模块化单体架构 | 单进程部署；模块间 trait 通信 |
| ADR-002 | 技术栈 Rust + axum + Svelte | 见 §5 |
| ADR-003 | age 加密方案与 64MB 分块 | 加密管道（§4.3） |
| ADR-004 | 增量检测双策略 | 快速 mtime+size / 严格 BLAKE3 |
| ADR-005 | 存储抽象层（ACL 防腐） | `infra/storage_trait.rs` |
| ADR-006 | SQLite 元数据 | `sync_snapshots` 表，WAL |
| ADR-007 | 内部事件总线 | tokio::broadcast，进度旁路 |
| ADR-008 | 飞牛原生应用集成 | `packaging/` + 生命周期脚本 |
| ADR-009 | 文件管理迁移至官方 WebDAV | `infra/target/webdav.rs`；逆向 REST 与登录器已移除 |
| ADR-010 | 备份目标按源文件夹名分层 | 目标端 `/目标文件夹/<源文件夹名>/…` |
| ADR-011 | 重新引入 kzwr REST 作可选增强 | 现为外置 `plugins/kzwr/` |
| ADR-012 | 页面访问改用飞牛统一网关 | Unix Socket + `/app/{appname}`，无 TCP 端口 |
| ADR-013 | 远程目标与增强功能插件化 | `plugin/` 层 + 稳定 C ABI（`PLUGIN_ABI.md`） |
| ADR-014 | 多目标 · 多任务 | `targets`/`tasks` 两列表；`job_id={task.id}-{源序号}` |
| ADR-015 | 增强类插件一律外置 | 核心无厂商专属逻辑；kzwr 完全外置 |
| ADR-016 | 宿主能力表 `host_bind` | 主表尾部追加；插件可写日志/审计/告警/进度/定时 |
| ADR-017 | 二进制产物不入库 | `.gitignore` 拦截；产物走 Releases |
| ADR-018 | 上传并发度按**目标**存储 | `TargetConfig.parallel` |
| ADR-019 | 目标表单由插件声明 | `describe_json.target`（打通插件目标创建） |
| ADR-020 | 目标编辑改为弹窗、表单完全由插件声明 | `target.form[]` → `TargetEditModal.svelte` |
| ADR-021 | 宿主不再代存插件配置 | 插件自管 `own_data_dir`；宿主提供 `seal`/`unseal` |
| ADR-022 | 拆分 `http/routes.rs` | 4464 行 → 11 个模块（见 §7） |
| ADR-023 | 实施插件沙箱 | 每插件专属线程 + Landlock（见 §6） |

---

> 本文件为**当前实现**的架构基线。功能迭代时同步更新本文件；
> 决策的**过程记录**写入 `docs/memory/dev/ADR.md`，进度写入 `docs/memory/dev/PROGRESS.md`。
