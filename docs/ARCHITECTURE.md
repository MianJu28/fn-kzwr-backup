# fnos 增量加密备份系统 · 架构设计文档

> 将飞牛 NAS 文件增量加密备份至酷族网软，支持 GUI 管理与选择性恢复。
> 本文档为长期维护的权威架构基线，所有重大技术决策以 ADR 形式记录。

---

## 1. 项目背景与约束

| 维度 | 约束 |
|------|------|
| 运行环境 | fnos（飞牛 NAS OS，嵌入式 Linux） |
| 部署形态 | 原生应用，随系统启动，单机运行 |
| 资源 | NAS 硬件资源有限（CPU/内存敏感） |
| 可用性 | 7×24 长期运行，需稳定可靠 |
| 数据源 | 飞牛 NAS（本地 FS） |
| 备份目标 | 酷族网软（自定义 REST API，不支持 WebDAV；登录用编译二进制，API 用 Rust 重写） |
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
| 核心域 | 恢复 | `RestoreJob` | 选择性恢复、版本回滚、完整性校验 |
| 支撑域 | 存储抽象 | — | Source/Target 适配器（ACL 防腐层） |
| 支撑域 | 元数据索引 | — | 文件快照、版本树、SQLite 持久化 |
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
- **酷族网软**：备份目标（读写），自定义 REST API（不支持 WebDAV）
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
                    (查上次快照 / 写新版本)

加密管道内部: 文件流 → 分块64MB → age 公钥加密(每 chunk 独立) → age 密文
登录/API: 编译二进制(kzwr_login)产出 session token → Rust API 客户端(reqwest) 调用酷族 REST API

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
| 源访问 | tokio::fs（本地 FS） | smb-rs / pavao | 仅需本地文件备份，不考虑 SMB/NFS，零依赖 |
| 目标访问 | reqwest（HTTP 客户端） | rust-s3 | 酷族自定义 REST API 已用 Rust 重写实现适配器 |
| 配置 | TOML + serde | YAML / JSON | 人类可读；Rust 生态一等支持 |
| 日志 | tracing + tracing-subscriber | log + env_logger | 结构化日志；span 追踪；异步友好 |
| 调度 | tokio-cron-scheduler | 系统 cron | 不依赖系统 cron，可移植；进程内调度 |

---

## 6. 架构决策记录 (ADR)

### ADR-001：采用模块化单体架构

**状态**：Accepted

**背景**：系统运行于 fnos 嵌入式 Linux，资源有限，需 7×24 稳定运行、随系统启动。团队规模小，单机部署，无需水平扩展。但备份/同步/加密/恢复各模块需独立演进。

**决策**：采用模块化单体。单进程内按限界上下文划分模块，模块间通过 trait 接口通信，共享 SQLite 但表归属明确。主流程同步执行保证强一致性；内部事件总线处理通知/审计旁路。

**后果**：
- (+) 单进程低资源占用，部署为单二进制 + Tauri 资源包
- (+) 模块边界清晰，未来可按上下文拆分为独立服务
- (+) 调试简单，事务边界清晰
- (-) 需自律与工具（如 `cargo-deny`、自定义 lint）维持模块边界
- (-) 共享数据库需约定表归属，禁止跨模块直接读写

### ADR-002：技术栈选型 Rust + axum + Svelte

**状态**：Accepted（修订：原 Tauri 方案已废弃，见下方背景）

**背景**：fnos 原生应用要求低资源占用、长期稳定。加密是核心能力，需内存安全。经查阅飞牛应用开发文档，飞牛 UI 通过桌面 iframe 加载 Web 页面（`app/ui/config` 声明入口），**不支持独立原生窗口**，因此 Tauri 的原生窗口模式不适用。

**决策**：后端 Rust + axum（HTTP 服务），前端 Svelte + Vite（SPA）。axum 既提供 REST API/WebSocket，又托管前端静态文件。应用以飞牛普通应用形态打包（`.fpk`），通过 `cmd/main` 脚本控制进程。

**后果**：
- (+) 契合飞牛端口服务入口模型（iframe 加载 `http://localhost:{port}/`）
- (+) 无 GC 暂停，流式加密稳定；内存安全防整类漏洞
- (+) 单二进制 + 静态前端资源，部署简单
- (+) axum 与 tokio 原生集成，WebSocket 支持状态推送
- (-) Rust 学习曲线陡，开发速度慢于 Go
- (-) 放弃 Tauri 的原生菜单/托盘等桌面集成能力（飞牛场景不需要）
- (-) 酷族自定义 API 已用 Rust 重写实现适配器（无官方 SDK）

### ADR-003：age 加密方案与分块策略

**状态**：Accepted（修订：原 Picocrypt/XChaCha20-Poly1305 + Argon2id 方案已废弃，改为 age 公私钥）

**背景**：用户指定加密改为 age（X25519 公私钥 + ChaCha20-Poly1305 AEAD）。需支持大文件流式加密与选择性恢复（随机访问特定 chunk 解密）。采用 age 后无需口令派生，改用标准公钥加密。

**决策**：
- 文件按 64MB 分块，每块用 age 公钥独立加密（每块生成独立文件密钥，age 标准头部含 ephemeral key，无碰撞风险）
- 加密方只持有 `age` 公钥即可备份；恢复方需 `age` 私钥
- 流式管道：`Source 流 → Chunker → Encryptor(age) → Target 流`，明文仅在内存当前 chunk
- `age` 私钥本身可被用户口令派生的密钥（Argon2id）再次加密后存密钥库，实现口令保护
- Rust crate：`age`（纯 Rust 实现，支持 musl 静态编译）

**后果**：
- (+) 支持随机访问恢复——只需解密目标 chunk，无需整文件
- (+) 流式处理，任意大小文件常量内存
- (+) age 为现代标准，纯 Rust 实现，避免 C 依赖；公钥加密免口令派生，加密侧更轻量
- (+) 公钥可安全公开，配合备份目标实现"只写不可读"（目标侧仅公钥加密，私钥本地私藏）
- (-) 64MB 分块在小文件场景有空间放大（需 padding 策略或小文件单独处理）
- (-) 私钥丢失则无法恢复，需私钥备份/恢复机制（Phase 5）

### ADR-004：增量检测双策略

**状态**：Accepted

**背景**：增量备份需平衡性能与精确性。mtime+size 快但可被欺骗；内容哈希精确但开销大。

**决策**：提供双策略，按任务可配置：
- **快速策略**（默认）：比较 `mtime + size`，命中即跳过。O(1)，覆盖 99% 场景。
- **严格策略**：对 mtime/size 变化的文件再算 BLAKE3 内容哈希确认。防 mtime 欺骗、支持跨文件去重。
- 大文件（>1GB）始终走严格策略 + 分块哈希，支持块级增量。

**后果**：
- (+) 默认场景高性能；需要时切严格模式
- (+) BLAKE3 树形哈希支持块级增量与并行计算
- (-) 双策略增加实现复杂度
- (-) 严格策略下首次全量哈希计算耗时

### ADR-005：存储抽象层（ACL 防腐）

**状态**：Accepted

**背景**：源仅本地 FS（`tokio::fs`），目标为酷族自定义 REST API（不支持 WebDAV）。酷族登录已由编译二进制完成，API 已用 Rust 重写。酷族 API 的细节（分块上传、session 认证、token 刷新）不应污染核心同步逻辑。

**决策**：定义统一的 `SourceStorage` 与 `TargetStorage` trait，位于领域层。基础设施层为每种协议实现适配器。核心逻辑只依赖 trait，不感知具体协议。新增协议只需实现 trait + 注册。

```rust
// 领域层定义（基础设施层实现）
trait SourceStorage {
    async fn list(&self, path: &Path) -> Result<Vec<FileDescriptor>>;
    async fn read_stream(&self, path: &Path) -> Result<impl Stream<Item = Result<Bytes>>>;
    async fn stat(&self, path: &Path) -> Result<FileMeta>;
}

trait TargetStorage {
    async fn write_stream(&self, path: &Path, stream: impl Stream<Item = Bytes>) -> Result<()>;
    async fn read_stream(&self, path: &Path) -> Result<impl Stream<Item = Result<Bytes>>>;
    async fn delete(&self, path: &Path) -> Result<()>;
    async fn list(&self, prefix: &str) -> Result<Vec<FileDescriptor>>;
}
```

**后果**：
- (+) 核心逻辑与协议解耦，换存储后端零改动核心
- (+) 新增协议（如 S3 源、阿里云 OSS 目标）成本低
- (+) 测试可用 mock trait，无需真实 NAS
- (-) 统一抽象可能屏蔽协议特有能力（如 S3 多播上传），需 trait 扩展点

### ADR-006：SQLite 作为元数据存储

**状态**：Accepted

**背景**：需持久化文件快照、版本树、任务状态、审计日志。嵌入式部署，无需独立数据库服务。

**决策**：SQLite + rusqlite，启用 WAL 模式。表按限界上下文归属命名（如 `sync_snapshots`、`crypto_chunks`、`backup_jobs`）。跨模块查询通过应用层服务，禁止跨模块直接 JOIN 他方表。

**后果**：
- (+) 嵌入式零配置，单文件易备份迁移
- (+) WAL 模式支持并发读（GUI 查询）+ 单写（备份任务），不阻塞 UI
- (+) SQL 表达力满足版本树、时间范围查询等复杂需求
- (-) 高写入吞吐下需批量提交（如 chunk 记录批量 insert）
- (-) 元数据膨胀需定期 VACUUM 与归档策略

### ADR-007：内部事件总线

**状态**：Accepted

**背景**：模块间需松耦合通知（如备份完成→通知 UI、审计）。但主流程需强一致性，不能用事件驱动核心路径。

**决策**：进程内事件总线（tokio::broadcast），领域事件发布后异步分发。订阅者：UI 状态广播、审计日志写入、fnos 通知。主流程不依赖事件确认——事件丢失不影响数据正确性，仅影响通知。

**后果**：
- (+) 模块解耦，新订阅者零侵入
- (+) 审计天然完整（订阅 AuditSubscriber）
- (-) 事件丢失仅影响通知，需明确告知用户
- (-) 不适合作为事务边界——主流程状态以数据库为准

### ADR-008：飞牛原生应用集成方案

**状态**：Accepted

**背景**：系统需以 fnos 原生应用形态部署。经查阅飞牛应用开发文档（developer.fnnas.com），飞牛应用有两种形态：普通应用（生命周期脚本 + 原生进程 + Web UI）与 Docker 应用（docker-compose）。飞牛 UI 通过桌面 iframe 加载 Web 页面，不支持独立原生窗口。

**决策**：采用**普通应用**形态（非 Docker），理由：
- 备份系统需直接访问飞牛本地文件系统，普通应用权限模型更直接（Docker 需挂载卷）
- 原生进程资源占用更低，契合 NAS 环境
- Rust 编译为 Linux 二进制（x86_64 + aarch64），放入 `target/` 目录

集成要点：
- **打包**：`fnpack build` 生成 `.fpk`；项目结构遵循飞牛规范（`app/`、`cmd/`、`config/`、`wizard/`、`manifest`）
- **生命周期**：`cmd/main` 脚本处理 `start`（启动 Rust 进程）/`stop`（优雅关闭）/`status`（检查存活）
- **UI 入口**：`app/ui/config` 声明 iframe 桌面入口，`type=iframe, protocol=http, port={wizard_port}, url=/`
- **权限**：`run-as=package`，专用用户 `fnosbackup`；通过 `config/resource` 声明共享目录或引导用户授权源目录
- **路径**：全部使用 `TRIM_*` 环境变量（`TRIM_APPDEST`/`TRIM_PKGETC`/`TRIM_PKGVAR`/`TRIM_APPTMP`），禁止硬编码
- **数据归属**：SQLite→`$TRIM_PKGVAR`，配置→`$TRIM_PKGETC`，密钥库→`$TRIM_PKGETC`，临时→`$TRIM_APPTMP`
- **安装向导**：`wizard/install` 收集 HTTP 端口、初始管理员口令

**后果**：
- (+) 原生访问文件系统，无需 Docker 卷挂载复杂度
- (+) 契合飞牛应用中心分发与升级流程（`upgrade_init`/`upgrade_callback` 处理数据迁移）
- (+) 用户通过飞牛桌面直接访问，体验原生
- (-) 需严格遵循飞牛目录与路径规范，移植性受限（但本系统专为 fnos 设计，可接受）
- (-) x86_64 与 aarch64 需分别编译二进制或交叉编译
- (-) 用户文件访问依赖授权机制，需设计清晰的授权引导 UI

---

## 7. 项目目录结构

项目遵循飞牛应用规范，Rust 源码与前端源码在开发期独立，打包时合入飞牛目录结构。

```
fnos-backup/
├── manifest                    # 飞牛应用元数据 (appname/version/platform/ctl_stop)
├── ICON.PNG / ICON_256.PNG     # 应用图标
├── app/
│   ├── ui/
│   │   ├── config              # 桌面入口 (iframe → http://localhost:{port}/)
│   │   └── images/             # 入口图标
│   └── www/                    # 前端构建产物 (Svelte → dist 拷贝至此)
├── cmd/                        # 飞牛生命周期脚本 (bash)
│   ├── main                    # start/stop/status 控制 Rust 进程
│   ├── install_init            # 首次安装初始化 (建目录/权限)
│   ├── install_callback
│   ├── upgrade_init            # 升级数据迁移
│   ├── upgrade_callback
│   ├── uninstall_init
│   ├── uninstall_callback
│   ├── config_init             # 配置变更处理
│   └── config_callback
├── config/
│   ├── privilege               # 运行权限 (run-as=package, user=fnosbackup)
│   └── resource                # 资源声明 (共享目录/端口)
├── wizard/                     # 安装/配置向导表单
│   ├── install                 # 收集端口/初始口令
│   └── config
├── target/                     # Rust 编译产物 (开发期构建后拷入)
│   └── bin/
│       ├── fnos-backup         # 主二进制
│       └── kzwr-login          # 酷族登录编译二进制 (源自 kzwr_login_turnstile.py, PyInstaller 产物)
├── rust/                       # Rust 源码 (开发期)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs             # 入口: axum HTTP 服务启动
│       ├── http/               # 接口层 (REST + WebSocket)
│       │   ├── routes.rs       # 路由定义
│       │   ├── handler/        # backup/restore/config handler
│       │   └── ws.rs           # 状态推送 WebSocket
│       ├── app/                # 应用编排层
│       │   ├── scheduling/     # 备份调度 (BackupJob 聚合)
│       │   └── restore/        # 恢复编排 (RestoreJob 聚合)
│       ├── domain/             # 领域核心层 (纯逻辑, 无 IO)
│       │   ├── sync/           # 增量同步 (SyncSession 聚合)
│       │   ├── crypto/         # 加密 (CryptoSession 聚合)
│       │   └── metadata/       # 元数据索引 (领域模型)
│       ├── infra/              # 基础设施层 (ACL 适配器)
│       │   ├── source/         # Source 适配器: local/ (仅本地FS)
│       │   ├── target/         # Target 适配器: kzwr/ (酷族自定义API, Rust 重写实现)
│       │   ├── storage_trait.rs
│       │   └── fnos/           # fnos 集成: 路径环境变量/通知
│       ├── persistence/        # 数据层
│       │   ├── sqlite/         # rusqlite + 迁移
│       │   ├── keystore/       # 密钥加密存储
│       │   └── config/         # TOML 配置 (读 TRIM_PKGETC)
│       ├── eventbus/           # 内部事件总线
│       └── logging/            # tracing 配置
├── frontend/                   # 前端源码 (开发期)
│   ├── package.json
│   ├── vite.config.ts
│   ├── src/
│   │   ├── App.svelte
│   │   ├── views/              # Dashboard/BackupConfig/RestoreWizard
│   │   └── stores/
│   └── dist/                   # 构建产物 → 拷贝至 app/www/
├── migrations/                 # SQLite 迁移脚本
└── docs/
    └── ARCHITECTURE.md         # 本文档
```

**构建流程**：`cargo build --release` → 二进制入 `target/bin/`；酷族登录脚本（`kzwr_login_turnstile.py`）经 PyInstaller 编译为 `kzwr-login` 二进制（x86_64+aarch64）入 `target/bin/`；`cd frontend && npm run build` → 产物入 `app/www/`；`fnpack build` → 生成 `.fpk`。

---

## 8. 演进路线图

| 阶段 | 交付物 | 关键风险 | 可逆性 |
|------|--------|----------|--------|
| **Phase 1 · MVP** | 全量备份 · 单源单目标 · 基础 Web UI · 本地 FS 源 · 酷族自定义 API 目标 · 飞牛 `.fpk` 打包 | 酷族 session 对接 · 飞牛生命周期集成 | 完全可逆 |
| **Phase 2 · 增量加密** | mtime 差分 · age 加密 · 流式管道 · 64MB 分块 · SQLite 元数据 | 私钥管理 · 大文件内存 | 完全可逆 |
| **Phase 3 · 恢复能力** | 选择性恢复 · 版本树浏览 · 恢复向导 UI · 完整性校验 · BLAKE3 严格模式 | 版本冲突 · 索引膨胀 | 部分可逆（元数据格式定型需迁移） |
| **Phase 4 · 生产强化** | 多目标支持 · 保留策略 · 断点续传 · 监控告警 · fnos 服务化 | 并发控制 · 资源争用 | 部分可逆 |
| **Phase 5 · 演进扩展** | 异地恢复 · 密钥轮换 · 插件化 · 可选分布式 | 跨节点一致性 | 视需求启用 |

**可逆性原则**：Phase 1-2 纯增量能力叠加，决策完全可逆；Phase 3-4 元数据格式定型后部分可逆（需写迁移脚本）；Phase 5 视实际需求启用，避免过早优化。

---

## 9. 质量属性分析

### 9.1 可扩展性
- 单进程内模块可独立演进，新增存储协议只需实现 trait
- SQLite 单机写入上限约数千 TPS，NAS 备份场景足够
- 若未来需多 NAS 集中备份，可将调度模块拆为独立服务（模块化单体的演进优势）

### 9.2 可靠性
- 断点续传：每 chunk 落盘后记录元数据，中断后从断点恢复
- 完整性校验：AEAD tag 自动验证；恢复后可选 BLAKE3 复核
- 故障隔离：单个备份任务失败不影响其他任务；事件总线故障不影响主流程

### 9.3 可维护性
- 模块边界清晰，依赖方向单向
- ADR 记录所有重大决策的"为什么"
- trait 抽象使核心逻辑可单元测试（mock storage）

### 9.4 可观测性
- tracing 结构化日志 + span 追踪跨模块调用链
- 事件总线天然提供审计流
- Web UI 实时聚合任务状态（通过 WebSocket 推送，飞牛 iframe 内支持）

### 9.5 安全性
- age 私钥永不明文落盘（可被口令派生密钥加密存储于密钥库）
- 明文不落临时盘（全程流式）
- age 底层 ChaCha20-Poly1305 AEAD 认证加密防篡改
- 每 chunk 独立文件密钥（age ephemeral key）防重放

---

## 10. 技术选型状态

> 详细选型分析见 `TECH_SELECTION.md`。以下标注选型结论与待验证项。

### 已选型（方案已定）

| 选型项 | 结论 | Phase |
|--------|------|-------|
| 酷族网软对接 | 登录用编译二进制产出 session token；API 已用 Rust 重写实现 Target 适配器 | 1-2 |
| 飞牛源访问 | 仅本地 FS（tokio::fs），不考虑 SMB/NFS | 1 |
| 双架构编译 | musl 静态链接 + cross 工具，全纯 Rust 依赖，GitHub Actions matrix | 1 |
| 源目录授权 | config/resource 声明 + 运行时引导，弃 root 模式 | 1 |
| UI 暴露认证 | 端口服务 + JWT（wizard 设管理员口令），WebSocket 状态推送 | 1 |
| 密钥管理 | age 公私钥（X25519）；备份用公钥加密、恢复用私钥解密；私钥可被口令派生密钥加密存储 | 2 |

### 待验证（需实际测试）

1. ⏳ **酷族 session token 对接**：编译二进制登录产出的 session token 需在本系统验证复用与过期处理（`TOKEN_EXPIRED` 暂停重登）
2. ⏳ **config/resource 格式**：查阅飞牛文档确认共享目录声明的具体字段
3. ⏳ **iframe 内 WebSocket**：验证飞牛 iframe CSP 是否允许 localhost WS 连接
4. ⏳ **大文件块级增量**：Phase 3 评估是否引入块级 BLAKE3 哈希
