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
- (-) 酷族对接需自建适配器（现已迁移官方 WebDAV，见 ADR-009）

### ADR-003：age 加密方案与分块策略

**状态**：Accepted（修订：原 Picocrypt/XChaCha20-Poly1305 + Argon2id 主密钥方案已废弃，改为 age 公私钥；私钥保护改用 age 内置 scrypt）

**背景**：用户指定加密改为 age（X25519 公私钥 + ChaCha20-Poly1305 AEAD）。需支持大文件流式加密与选择性恢复（随机访问特定 chunk 解密）。采用 age 后无需口令派生，改用标准公钥加密。

**决策**：
- 文件按 64MB 分块，每块用 age 公钥独立加密（每块生成独立文件密钥，age 标准头部含 ephemeral key，无碰撞风险）
- 加密方只持有 `age` 公钥即可备份；恢复方需 `age` 私钥
- 流式管道：`Source 流 → Chunker → Encryptor(age) → Target 流`，明文仅在内存当前 chunk
- `age` 私钥本身可被管理员口令（**age 内置 scrypt** 派生密钥）再次加密后存密钥库（`keystore.age`），实现口令保护
- Rust crate：`age`（纯 Rust 实现，支持 musl 静态编译）

**后果**：
- (+) 支持随机访问恢复——只需解密目标 chunk，无需整文件
- (+) 流式处理，任意大小文件常量内存
- (+) age 为现代标准，纯 Rust 实现，避免 C 依赖；公钥加密免口令派生，加密侧更轻量
- (+) 公钥可安全公开，配合备份目标实现"只写不可读"（目标侧仅公钥加密，私钥本地私藏）
- (-) 64MB 分块在小文件场景有空间放大（需 padding 策略或小文件单独处理）
- (-) ~~私钥丢失则无法恢复，需私钥备份/恢复机制（Phase 5）~~ → **已缓解（2026-09-19）**：设置页可口令校验后导出私钥另存，并「我已妥善保存」确认；未确认时持续提示丢失风险

### ADR-004：增量检测双策略

**状态**：Accepted

**背景**：增量备份需平衡性能与精确性。mtime+size 快但可被欺骗；内容哈希精确但开销大。

**决策**：提供双策略，按任务可配置：
- **快速策略**（默认）：比较 `mtime + size`，命中即跳过。O(1)，覆盖 99% 场景。
- **严格策略**：对 mtime/size 变化的文件再算 BLAKE3 内容哈希确认。防 mtime 欺骗、支持跨文件去重。
- ~~大文件（>1GB）始终走严格策略 + 分块哈希，支持块级增量~~ → **块级增量不做**（用户决策，2026-09-19）：维持整文件差分，严格策略仅按任务配置启用、不按文件大小强制。

**后果**：
- (+) 默认场景高性能；需要时切严格模式
- (+) BLAKE3 树形哈希支持流式/并行计算（块级增量未采用，见上）
- (-) 双策略增加实现复杂度
- (-) 严格策略下首次全量哈希计算耗时

### ADR-005：存储抽象层（ACL 防腐）

**状态**：Accepted

**背景**：源仅本地 FS（`tokio::fs`）。目标为酷族官方 WebDAV（ADR-009，逆向 REST API 已移除）。WebDAV 凭据管理不应污染核心同步逻辑。

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

**背景**：需持久化文件快照、任务状态、审计日志。嵌入式部署，无需独立数据库服务。快照仅用于增量差分（kzwr 与本地保持镜像一致，不做多版本历史）。

**决策**：SQLite + rusqlite，启用 WAL 模式。表按限界上下文归属命名（如 `sync_snapshots`、`backup_jobs`）。跨模块查询通过应用层服务，禁止跨模块直接 JOIN 他方表。

**后果**：
- (+) 嵌入式零配置，单文件易备份迁移
- (+) WAL 模式支持并发读（GUI 查询）+ 单写（备份任务），不阻塞 UI
- (+) SQL 表达力满足快照查询等需求
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
- **UI 入口**：`app/ui/config` 声明 iframe 桌面入口；**v0.3.2 起改用飞牛统一网关**（`protocol=""` + `gatewayPrefix=/app/fn-kzwr-backup` + `gatewaySocket=app.sock` + `url=/app/fn-kzwr-backup`），`service_port` 端口服务保留用于直连访问与 `checkport` 健康检查（详见 ADR-012）
- **权限**：`run-as=package`，专用用户 `fnosbackup`；通过 `config/resource` 声明共享目录或引导用户授权源目录
- **路径**：全部使用 `TRIM_*` 环境变量（`TRIM_APPDEST`/`TRIM_PKGETC`/`TRIM_PKGVAR`/`TRIM_PKGTMP`），禁止硬编码
- **数据归属**：SQLite→`$TRIM_PKGVAR`，配置→`$TRIM_PKGETC`，密钥库→`$TRIM_PKGETC`，临时→`$TRIM_PKGTMP`
- **安装向导**：`wizard/install` 收集 HTTP 端口、初始管理员口令

**后果**：
- (+) 原生访问文件系统，无需 Docker 卷挂载复杂度
- (+) 契合飞牛应用中心分发与升级流程（`upgrade_init`/`upgrade_callback` 处理数据迁移）
- (+) 用户通过飞牛桌面直接访问，体验原生
- (-) 需严格遵循飞牛目录与路径规范，移植性受限（但本系统专为 fnos 设计，可接受）
- (-) x86_64 与 aarch64 需分别编译二进制或交叉编译
- (-) 用户文件访问依赖授权机制，需设计清晰的授权引导 UI

### ADR-009：kzwr 文件管理迁移至官方 WebDAV，弃用逆向 REST API

**状态**：Accepted（已完全实施，2026-09-18：WebDAV 适配器上线，逆向 REST 适配器与登录二进制已从代码库完全移除）

**背景**：项目初期酷族网软（kzwr.com）不支持标准协议，遂逆向其自定义 REST API（v1/v2/v3）用 Rust 重写了文件管理能力（分块上传/presigned-url/SHA1+SHA256 哈希对齐/session token 认证/两阶段物理删除/文件夹 CRUD）。该逆向 API 无稳定性保证，随前端版本演进可能变动，哈希对齐逻辑脆弱、维护成本高。**酷族官方现已支持 WebDAV**，提供稳定的标准协议。

**决策**：
- 文件管理（上传/下载/删除/列目录/建目录）全部迁移至 kzwr 官方 **WebDAV**
- 逆向 REST API 适配器（`infra/target/kzwr/` client/storage/upload）**已从代码库完全移除**，无回退路径
- 登录二进制（CloakBrowser/Camoufox + PyInstaller）及登录环境子系统（Xvfb/uBlock/镜像下载）**一并移除**；WebDAV 走独立专用凭据（HTTP Basic，已实测验证）
- 用户信息不再依赖 REST API（get_member 移除）；WebDAV 无配额属性，UI 仅展示本地配置账号

**验证记录（2026-09-18，WSL curl 实测）**：
- HTTP Basic 认证可用：`PROPFIND`（Depth 0/1，207）、`MKCOL`（201）、`PUT`（201）、`DELETE`（204，文件/目录）全部通过
- 下载链路：`GET /dav/<path>` 返回 **302** → `storage-na.kzwr.net` 的 S3 风格 presigned URL（约 300s 有效），**跟随重定向即可取回内容（200），无需二次认证**——Rust 侧 reqwest 需允许跨域重定向
- WebDAV 专用凭据不落文档/代码/仓库，运行时经加密配置或密钥库提供

**实现与端到端实测（2026-09-18，WSL）**：
- 适配器：`infra/target/webdav.rs` `WebdavTarget`（实现 `TargetStorage`，核心逻辑零改动）；PUT 前逐级 MKCOL 确保父目录，下载跟随 302
- 后端选择：`TRIM_DAV_*` 环境变量或加密配置 `[webdav]` 段；缺失时启动占位适配器（操作返回引导错误），UI 保存配置后经 `SwapTarget` 热切换生效（无需重启）
- 端到端测试（`bin/webdav_backup_test.rs`，真实服务器）：ping ✓、多级目录+特殊字符文件名 roundtrip ✓、BackupJob 全量 4 上传/增量 0/修改 1 ✓、下载解密校验 4/4 ✓、清理 ✓

**大文件分片与地址固定（2026-09-18 补充）**：
- 网站限制单次上传 100MB（实测：不分片上传 120MB 被 Cloudflare 返回 `413 Payload Too Large`）→ 超过 `PART_SIZE`（默认 **90MiB**，即限制的 90%；`FNOS_DAV_PART_SIZE` 可覆盖）的密文文件自动拆分为 `<path>.part0001…` 依次 PUT；下载按序拼接、删除清理全部分片、列表将分片合并为逻辑文件（对核心逻辑透明）
- WebDAV 地址固定为官方地址（`DEFAULT_URL`），UI 仅填用户名/密码；`TRIM_DAV_URL` 环境变量仍可用于开发覆盖
- 实测坑：服务端/链路对长时 HTTP/2 上传不稳定（~20s 即 PROTOCOL_ERROR）→ 客户端强制 HTTP/1.1；PUT 带 30 分钟总超时 + 4 次重试（5xx/408/429/网络错误可重试）

**迁移步骤**：
1. 验证 WebDAV 端点、认证方式与流式 PUT/GET 行为
2. 实现 `WebdavTargetStorage` 适配器（实现既有 `TargetStorage` trait，核心同步/加密逻辑零改动）
3. 端到端回归：备份/恢复/删除/多级文件夹/保留策略/定时备份
4. ✅ 逆向 REST API 适配器与登录二进制相关代码已完全移除（2026-09-18，含 routes 登录环境子系统、packaging/CI 引用、sha1/sha2/zip 依赖）

**后果**：
- (+) 基于官方稳定协议，不再随前端版本漂移
- (+) 大幅简化适配器：标准协议，去除哈希对齐/presigned 分片等脆弱逻辑
- (+) trait 抽象（ADR-005）使替换 Target 适配器不影响核心逻辑
- (-) 需重写 Target 适配器并完整回归测试
- (-) ~~WebDAV 认证方式与加密密文流式 PUT 的性能需实测（64MB 分块策略是否保留待验证）~~ → **已解除**：HTTP Basic 与密文流式 PUT 端到端实测通过；>90MiB 密文自动分片上传（`PART_SIZE`，见上「大文件分片与地址固定」），64MB 加密分块策略保持不变

---

### ADR-010：备份目标端按源文件夹名分层

**背景**：早期实现把多个源目录直接平铺到目标前缀下（`/目标文件夹/<相对路径>`）。配置多个源文件夹时，不同源目录中的同名文件/目录会在同一层互相覆盖；且「所选文件夹」本身在网盘不可见（只存在其内容），用户难以把网盘目录对应回本地来源。

**决策**：每个源目录在目标端以其**文件夹名**单独建目录，即 `/目标文件夹/<源文件夹名>/<相对路径>`；备份时显式创建该目录及其空子目录。

**实现**：
- 备份：`BackupJob::run_multi` 用 `join_root_prefix()` 为每个源目录生成独立 `target_prefix`；`TargetStorage::ensure_dir`（WebDAV 实现为逐级 `MKCOL`）创建目录，空目录也会创建
- 恢复：`RestoreJob.source_root_name` 还原同一层级（`/目标文件夹/<源文件夹名>/…`），并按该源路径对应的快照展示总大小
- 事件：`run_multi` 以基准 `job_id` 发布**合并后**的整体进度

**后果**：
- (+) 多源目录互不干扰，网盘目录结构与本地来源一一对应
- (+) 空文件夹也能在网盘保留
- (-) 与 v0.1.3 之前的目标布局不兼容：旧备份需重新执行一次备份（或手工整理目录）
- (-) 目录创建会多出少量 `MKCOL` 请求（已对「会被文件上传覆盖的父目录」跳过）

---

### ADR-011：重新引入 kzwr REST API 作为**可选增强功能**（非备份通道）

**背景**：ADR-009 曾将逆向 REST API 与登录二进制完全移除，备份/恢复统一走官方 WebDAV。但 WebDAV 无法提供账号级能力：存储空间/套餐信息、回收站查看与清空等。且登录二进制（Camoufox/Playwright）已被证实无法在飞牛原生环境运行。

**决策**：
- 仅恢复 REST **客户端**（`infra/kzwr_api/client.rs`，取自提交 `f8141d5`），**不恢复** storage/upload 适配器与登录二进制——备份/恢复通道仍是 WebDAV
- 认证改为**用户手动提供 access-token**：浏览器登录酷族后从 Cookie 复制填入设置页（age 加密存储、永不回显、保存前实测、热更新）
- 未配置 token 时所有增强接口优雅降级，不影响备份/恢复主链路

**能力**：账号信息（存储空间/套餐/详情）、空间占用预警（阈值可配）、回收站清空（可跟随保留策略：占用门槛 + 最小保留天数，无法解析时间的条目保守保留）。

**后果**：
- (+) 无需登录二进制即可获得账号级增强能力，实现成本低（复用既有客户端）
- (+) 主链路零依赖：不配置 token 完全不影响备份/恢复
- (-) access-token 有效期有限，过期需重新复制（已用告警 + 启动/使用时校验缓解）
- (-) 逆向接口非官方契约，字段可能变化（回收站条目的 size/时间字段做了多候选兼容，解析失败保守保留）

---

### ADR-012：页面访问改为飞牛统一网关（Unix Socket + `/app/{appname}` 前缀）

**状态**：Accepted（v0.3.2 起实施，v0.3.4 起为默认分发形态）

**背景**：桌面 iframe 原先以 `type=iframe, protocol=http, port=8080, url=/` 直连应用端口。当用户以 **https** 访问飞牛桌面时，`https` 页面里加载 `http://<nas>:8080/` 的 iframe 属于**混合内容，部分浏览器直接拦截**（页面空白）。备选方案均不理想：给应用自建 TLS 需要证书（飞牛未向应用提供证书，且自签会被浏览器拦）；改用 `index.cgi` 由宿主托管则**不支持 WebSocket**（本应用的实时进度依赖 WS）。

**决策**：改用飞牛官方**统一网关**（`docs/fnnas-dev-docs/core-concepts/08-gateway-registration.md`）：
- `app/ui/config` 声明 `protocol=""`、`gatewayPrefix="/app/fn-kzwr-backup"`、`gatewaySocket="app.sock"`、`url="/app/fn-kzwr-backup"`；网关入口会忽略 `protocol`/`port`
- 应用监听 `$TRIM_APPDEST/app.sock`（Unix Socket），由 fnOS 校验 NAS 登录态后**同源反代**（转发时保留前缀）；`cmd/main` 通过 `GATEWAY_PREFIX`/`TRIM_APP_SOCK` 注入，并在启动前与停止后清理残留 socket 文件
- 前端资源用**绝对前缀**引用（`vite.config.js` 的 `base` + `lib/appBase.js`），API 与 WebSocket 同样带前缀（`${APP_BASE}/api/...`、`ws(s)://host${APP_BASE}/api/ws`）
- 后端同一套路由同时挂在**根路径与前缀**下；网关连接进入时**先剥离前缀**再交给路由（手写泛型 `tower::Service` 包装器）
- **保留** `service_port` 端口监听：直连访问（`http://<nas>:8080/`）与 `checkport` 健康检查继续可用

**实现说明（axum 0.7 约束）**：
- `axum::serve` 只接受 `TcpListener`，Unix Socket 需自行用 `hyper` + `hyper-util` 驱动（`TokioIo` + `TowerToHyperService` + `http1::Builder::serve_connection(...).with_upgrades()`，`with_upgrades` 是 WebSocket 101 的必要条件）
- `Router::nest` 对 `/prefix` 与 `/prefix/` 的匹配行为不一致（后者返回 404），因此**不依赖 nest 语义**，改为在网关连接入口剥离前缀
- `Router::merge` 在双方均有 fallback 时会 panic——路由改为「同一 Router 内挂两个 nest + 一个根 fallback」的方式组装

**后果**：
- (+) https 访问飞牛桌面时页面不再被拦（iframe 与桌面同源同协议）
- (+) WebSocket 经网关可用；网关先校验登录态，多一层访问控制
- (+) 端口直连方式保留，调试与兼容旧书签不受影响
- (-) 应用需适配「带前缀路由 + Unix Socket 监听」，前端资源与 API 必须使用同一前缀（四处需保持一致：`ui/config` 的 `gatewayPrefix`、`cmd/main` 的 `GATEWAY_PREFIX`、后端 `gateway_prefix()`、前端 `lib/appBase.js` + `vite.config.js`）
- (-) 前缀变更需**升级安装**才生效（入口声明在安装/升级时注册）

---

## 7. 项目目录结构

项目遵循飞牛应用规范，Rust 源码与前端源码在开发期独立，打包时合入飞牛目录结构。

> 以下为**目标结构**（飞牛应用部署形态）。开发期 `backend/` 与 `frontend/` 独立演进，打包时合入飞牛目录。**当前实际 Rust 源码结构**见文末"项目进度"一节。

```
fn-kzwr-backup/
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
│       └── fn-kzwr-backup         # 主二进制
├── backend/                    # Rust 后端源码 (开发期)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs             # 入口: axum HTTP 服务启动
│       ├── lib.rs              # 库入口 (AppState 等)
│       ├── http/               # 接口层 (REST + WebSocket)
│       │   ├── routes.rs       # 路由 + 各 handler (backup/restore/config/health)
│       │   └── ws.rs           # 状态推送 WebSocket
│       ├── domain/             # 领域核心层 (纯逻辑, 无 IO)
│       │   ├── backup.rs       # 备份调度 (BackupJob 聚合)
│       │   ├── sync.rs         # 增量同步 (SyncSession 聚合)
│       │   ├── crypto.rs       # 加密 (CryptoSession 聚合)
│       │   ├── restore.rs      # 恢复编排 (RestoreJob 聚合)
│       │   ├── pace.rs         # 传输速度计量 (SpeedMeter)
│       │   └── retention.rs    # 保留策略 (孤儿文件清理)
│       ├── infra/              # 基础设施层 (ACL 适配器)
│       │   ├── source/local/   # Source 适配器: local/ (仅本地FS)
│       │   ├── target/webdav.rs # Target 适配器: WebdavTarget (官方 WebDAV)
│       │   ├── persistence/    # snapshot.rs (SQLite 快照)
│       │   ├── config.rs       # TOML 配置 (读 TRIM_PKGETC)
│       │   ├── keystore.rs     # 密钥加密存储
│       │   └── storage_trait.rs
│       ├── bin/                # 测试二进制 (开发期, 不入生产)
│       └── eventbus.rs         # 内部事件总线
├── frontend/                   # 前端源码 (开发期)
│   ├── package.json
│   ├── vite.config.ts
│   ├── src/
│   │   ├── App.svelte
│   │   └── ...
│   └── dist/                   # 构建产物 → 拷贝至 app/www/
├── bin/                        # 开发期测试/启动脚本 (不入库, gitignored)
└── docs/
    ├── ARCHITECTURE.md         # 本文档
    └── TECH_SELECTION.md       # 技术选型分析
```

**构建流程**：`cargo build --release` → 二进制入 `target/bin/`；`cd frontend && npm run build` → 产物入 `app/www/`；`fnpack build` → 生成 `.fpk`。

> 注：开发期构建**通过 SSH 在飞牛 NAS 上进行**，实际源码以 Windows 侧 `backend/`、`frontend/` 为准，构建前用 `pscp`/tar 同步至 NAS `/vol1/1000/Docker/kuzu-backup`。WSL 已废弃（上行仅 ~4KB/s、后台进程随会话被回收）。

---

## 8. 演进路线图

> 状态图例：✅ 已完成 · 🔶 部分完成 · ⏳ 规划中

| 阶段 | 交付物 | 状态 | 关键风险 | 可逆性 |
|------|--------|------|----------|--------|
| **Phase 1 · MVP** | 全量备份 · 单源单目标 · 基础 Web UI · 本地 FS 源 · 酷族官方 WebDAV 目标（ADR-009） · 飞牛 `.fpk` 打包 | ✅ 完成（x86 飞牛设备安装/运行实测通过；aarch64 待测） | WebDAV 对接（已解决）· 飞牛生命周期集成 | 完全可逆 |
| **Phase 2 · 增量加密** | mtime 差分 · age 加密 · 流式管道 · 64MB 分块 · SQLite 元数据 | ✅ 完成 | 私钥管理（已用密钥库解决）· 大文件内存 | 完全可逆 |
| **Phase 3 · 恢复能力** | 选择性恢复 · 恢复向导 UI · 完整性校验 · BLAKE3 严格模式 | ✅ 完成 | 索引膨胀（结合保留策略缓解） | 部分可逆（元数据格式定型需迁移） |
| **Phase 4 · 生产强化** | 多目标支持 · 保留策略 · 断点续传 · 监控告警 · fnos 服务化 | ✅ 保留策略 / 断点续传 / WebSocket 监控 / `.fpk` 打包 / 监控告警 / 飞牛设备实测均已完成（多目标按用户决策放弃） | 并发控制 · 资源争用 | 部分可逆 |
| **Phase 5 · 演进扩展** | 异地恢复 · 密钥轮换 · 插件化 · 可选分布式 | ⏳ 规划中 | 跨节点一致性 | 视需求启用 |

**可逆性原则**：Phase 1-2 纯增量能力叠加，决策完全可逆；Phase 3-4 元数据格式定型后部分可逆（需写迁移脚本）；Phase 5 视实际需求启用，避免过早优化。

**实际完成功能清单**（按 git 提交历史梳理）：
- ✅ 增量加密备份（mtime+size 差分、age 加密、断点续传每文件即时快照）
- ✅ BLAKE3 严格模式差分（内容哈希确认，ADR-004）
- ✅ age 公私钥密钥库持久化（私钥被口令派生密钥加密存储）
- ✅ kzwr 官方 WebDAV Target 适配器（`WebdavTarget`：MKCOL/PUT/GET 302 跟随/DELETE/PROPFIND，ADR-009 端到端实测通过）
- ❌ kzwr 逆向 REST API 适配器与登录二进制（分块上传/下载/删除、session 认证、Camoufox 登录环境）——**已从代码库完全移除**（ADR-009，2026-09-18）
- ✅ 恢复编排（RestoreJob）+ 恢复到源路径 + 多路径多 job 快照
- ✅ 备份/恢复 HTTP API + Svelte Web UI（多路径配置、恢复树形视图；登录页随登录二进制一并移除，前端无登录态）
- ✅ WebSocket 实时任务监控（ADR-007 事件总线）：多路径进度合并统计、**上传/下载完成后才计数**、后端计量的实时速度（仅统计实际传输时段）、明文总量与已传量展示
- ✅ 保留策略：目标端孤儿文件清理（Phase 4）
- ✅ 备份目标端**按源文件夹名分层**（`/目标文件夹/<源文件夹名>/…`，含空目录显式创建，ADR-010）
- ✅ 大文件分片上传/下载与清理；分片请求使用**自定义带精确长度的流式请求体**（保留 `Content-Length` 且可上报进度）
- ✅ 配置导入/导出（`/api/config/export`、`/api/config/import`，含 WebDAV 凭据与 age 私钥，需管理员口令）
- ✅ 监控告警 Webhook 自定义（请求头 + 请求体模板 + 连通性测试）
- ✅ 恢复后回写快照（size + 实际 mtime），**避免下次增量备份重复上传**
- 🔶 飞牛 `.fpk` 打包部署：x86 设备实测通过；aarch64 待测

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

| 选型项 | 结论 | Phase | 状态 |
|--------|------|-------|------|
| 酷族网软对接 | 官方 WebDAV（Basic 凭据，加密存储，保存时 ping 验证并热切换） | 1-2 | ✅ WebDAV 适配器已实现并端到端实测；REST 适配器与登录二进制已移除（ADR-009） |
| 飞牛源访问 | 仅本地 FS（tokio::fs），不考虑 SMB/NFS | 1 | ✅ 已实现 |
| 双架构编译 | musl 静态链接 + cross 工具，全纯 Rust 依赖，GitHub Actions matrix | 1 | ✅ 已实现（本地 musl 构建验证） |
| 源目录授权 | config/resource 声明（`data-share`）+ 运行时引导，弃 root 模式 | 1 | ✅ `config/resource` 声明 + `disable_authorization_path=false`；x86 实测授权目录可读（`run-as=package`） |
| UI 暴露认证 | 端口服务 + **敏感操作口令校验**（未采用 JWT/全站登录） | 1 | ✅ 端口服务与 iframe 内 WebSocket 已在 x86 实测；导私钥/配置导入导出等敏感操作校验管理员口令；**不做全站登录** |
| 密钥管理 | age 公私钥（X25519）；备份用公钥加密、恢复用私钥解密；私钥可被管理员口令（age scrypt）加密存储 | 2 | ✅ 已实现（`keystore.age` 加密持久化，口令热切换） |

### 待验证（需实际测试）

1. ❌ **酷族 session token 对接**：**已作废**——属逆向 REST API 能力，随 ADR-009 从代码库移除（WebDAV 走 HTTP Basic 认证，无 session 复用/过期重登概念）
2. ✅ **config/resource 格式**：共享目录声明（`data-share`）已在 x86 飞牛设备实测可用（授权目录读取正常）
3. ✅ **iframe 内 WebSocket**：飞牛 iframe 内 localhost WebSocket 实测正常（2026-09-19 设备实测）
4. ❌ **大文件块级增量**：**不做**（用户决策，2026-09-19）；维持整文件差分
5. ✅ **飞牛 `.fpk` 打包部署**：x86 飞牛设备安装与运行实测通过（见 11.6）；aarch64 待测

---

## 11. 项目进度（实现状态）

> 本节为**滚动更新的当前进度基线**，随每次功能迭代更新。状态图例：✅ 已完成并实测 · 🔶 部分完成 · ⏳ 规划中。

### 11.1 当前开发状态

**核心备份/恢复主链路已完成并在 x86 飞牛设备实测通过**；当前功能版本 **v0.3.8**（0.2.0 起项目更名 `fn-kzwr-backup`；0.2.1 恢复页懒加载与「全部恢复」；0.2.2 设置项回显；0.2.3 按 ADR-011 引入 kzwr REST 增强功能：存储空间/空间预警/回收站门槛清理，及定时可视化、一键体检、操作审计、账号一致性校验；0.2.4 修复飞牛目录选择器误报取消、操作审计独立成页；0.2.5 修复目录选择器 1003103——config/resource 权限声明键修正为官方的 api-scope；0.2.6 修复保留策略误删刚备份文件、上传/下载 4 路并发并重写速度计量、回收站年龄门槛按宿主时区、凭据解密缓存与恢复树聚合索引提速；0.2.7 恢复容忍云端缺失并新增「清理缺失」按钮、任务失败正确显示失败态；0.2.8 账号信息整合至侧边栏（可刷新）、网络请求自动重试 3 次、调试日志开关、依据实测修正回收站字段 length/deletedDate 使保留策略清空回收站全链路生效——已用真实 token 实测清理 70 项；0.2.9 传输改为顺序执行并保留网络自动重试、日志页（查看/清空/下载 + 调试开关）、审计页清空；0.2.10 调试开关迁移至日志页、需要管理员口令的操作统一改为弹窗输入；0.3.0 实时任务面板展示完整流程阶段（准备/传输/收尾，含扫描差分与保留策略清理）、侧栏卡片间距与 WebDAV 文案优化；0.3.1 修复口令弹窗与通知设置输入框无法输入（Svelte 响应式语句回写绑定变量）；0.3.2 **接入飞牛统一网关修复 https 混合内容拦截**（ADR-012）；0.3.3 应用介绍改用 HTML 富文本并接入宣传图；0.3.4 介绍精简为一句一行；**0.3.8 修复日志 ANSI 乱码、空间预警改为后台定期巡检并进入「消息提醒」（回落自动消解）、异常提示统一到「消息提醒」；宣传图显示修复：`desc` 的 `<img>` 只保留 src/alt/width，`style` 属性会被宿主转义成纯文本；文档同步**）。构建与测试**统一通过 SSH 在飞牛 NAS 上进行**（WSL 已废弃：上行仅 ~4KB/s、后台进程随会话被回收）；源码从 Windows 侧经 `pscp`/tar 同步至 NAS 后执行 `Scripts/build_fnos_app.sh`，运行编译好的二进制或通过 HTTP API 测试。

### 11.2 已实现功能（按模块）

| 模块 | 功能 | 状态 | 说明 |
|------|------|------|------|
| **备份调度** | 增量备份（mtime+size 差分） | ✅ | `BackupJob::run` / `run_strict` / `run_multi` |
| | 断点续传 | ✅ | 每文件上传后即时存快照，中断可续 |
| | 多路径备份 | ✅ | 每个源路径独立 job_id 快照，共享 target_prefix |
| | 目标端分层与空目录 | ✅ | 每个源文件夹在目标端以其**文件夹名**建目录（`/目标文件夹/<源文件夹名>/…`），并显式创建该目录及其空子目录（ADR-010） |
| | 保留策略（孤儿清理） | ✅ | `domain/retention.rs`，备份后自动清理目标端孤儿文件 |
| | 定时备份（cron） | ✅ | `domain/scheduler.rs`（`croner` 解析），cron 表达式到点触发、配置热更新；**全局运行互斥**（`AppState.backup_running` CAS），已有备份在跑时跳过本次触发 |
| | 定时任务可视化 | ✅ | `POST /api/schedule/preview`：cron → 未来 5 次触发时间；**按服务器本地时区解释**（调度器已从 UTC 修正，`0 0 * * *` 即本地零点） |
| **增强功能** | access-token（可选，ADR-011） | ✅ | `infra/kzwr_api`：REST 客户端；token 加密存储、保存前实测、热更新；未配置时降级，不影响备份/恢复 |
| | 账号信息与空间 | ✅ | `GET /api/kzwr/user`：存储空间/套餐/UID/单文件上限/地区/IP 等；占用达阈值（默认 85%，0=关闭）生成空间预警 |
| | 回收站清理 | ✅ | 设置页手动清空（无门槛）；可跟随保留策略自动清理：占用 ≥N GB 才清 + 仅清理 N 天前条目（解析不出时间的保守保留）；`BackupResponse.trash_emptied` |
| | 账号一致性校验 | ✅ | WebDAV 凭据 / access-token 保存时交叉比对账号（email/name 包含匹配），不一致 → 页面提醒 + 告警 |
| | token 失效告警 | ✅ | 启动与使用时校验登录态（官方 API 对无效 token 仍返回 200，须查 `isLogin`），失效生成 kzwr 告警 |
| **可观测** | 一键体检 | ✅ | `GET /api/setup/check`：服务/WebDAV 实连/路径+快照数/私钥确认/定时/增强 token 实连/空间阈值，逐项带修复建议 |
| | 操作审计 | ✅ | `$TRIM_PKGVAR/audit.log`（JSON Lines，512KB 自动裁剪保留 1000 行）：凭据/密钥/配置/备份/恢复/回收站操作留痕；`GET /api/audit`；独立「审计」页查看（侧边导航入口） |
| **增量同步** | 双策略差分 | ✅ | 快速 mtime+size / 严格 BLAKE3（ADR-004） |
| **加密** | age 公私钥加密 | ✅ | 64MB 分块，公钥加密/私钥解密（ADR-003） |
| | 密钥库持久化 | ✅ | 私钥被口令派生密钥加密存储，跨重启可用 |
| **恢复** | 恢复编排 | ✅ | `RestoreJob`，选择性恢复、恢复到源路径（按备份源文件夹名还原目标子目录层级） |
| | 完整性校验 | ✅ | age AEAD tag 自动验证；恢复后内容对比校验 |
| | 恢复后防重传 | ✅ | 恢复写完文件后回写该文件快照（size + 实际 mtime），下次增量备份命中「未变化」不再重复上传 |
| | 恢复树懒加载与聚合计数 | ✅ | `GET /api/restore/files` 返回每个备份文件夹的 `{file_count, dir_count, total_bytes}`（不再下发整棵树）；`GET /api/restore/tree?source&dir` 按目录返回**直接子项**（目录附递归统计），前端展开时才加载。目录树由后端从快照推导（`snapshot_aggregate`/`snapshot_children`），不依赖快照行序、不怕缺目录条目 |
| | 全部恢复 | ✅ | `POST /api/restore/run` 支持 `all` 与 `dir`：`all=true` 用快照全部文件（`dir` 可限定子目录前缀），实现「全部恢复」与「恢复整个目录」一次请求 |
| **kzwr 目标** | 官方 WebDAV 适配器 | ✅ | `WebdavTarget`：MKCOL/PUT/GET(302 跟随)/DELETE/PROPFIND；凭据加密存储，保存时 ping 验证并热切换（ADR-009） |
| | 大文件分片上传 | ✅ | 超过 `PART_SIZE`（默认 90MiB = 100MB 网站限制的 90%）自动拆分为 `.part0001…` 依次 PUT；`FNOS_DAV_PART_SIZE` 可覆盖 |
| | 带长度的流式请求体 | ✅ | 分片 PUT 使用自定义 `http_body`（精确 `size_hint`）——保留 `Content-Length` 的同时按 256KiB 分块上报进度；修复「分片请求误用整文件长度导致 Cloudflare 413」 |
| | 分片下载拼接与清理 | ✅ | 读取时逻辑文件 404 则按序拼接分片；删除同时清理逻辑文件与全部分片；列表将分片合并为逻辑文件 |
| | 目录与删除 | ✅ | 写入前逐级 `MKCOL` 确保父目录；`DELETE` 直接删除（无回收站，两阶段物理删除已随 REST 移除） |
| **存储抽象** | Source/Target trait | ✅ | `storage_trait.rs`（ADR-005），ACL 防腐层 |
| **元数据** | SQLite 快照 | ✅ | `sync_snapshots` 表，WAL 模式（ADR-006） |
| **事件总线** | 内部事件总线 | ✅ | tokio::broadcast，备份/恢复进度事件（ADR-007） |
| **WebSocket** | 实时状态推送 | ✅ | `/api/ws`，前端实时进度条，断线重连；事件携带 `bytes_done`/`bytes_total`/`elapsed_ms`/`speed` |
| | 任务阶段（phase） | ✅ | 事件新增 `phase`：`prepare`（扫描源目录/差分、列取云端文件）→ `transfer`（上传/下载）→ `cleanup`（删除云端多余文件、保留策略清理、落盘快照）；面板展示完整流程；保留策略改在 `Completed` 之前完成 |
| **访问方式** | 飞牛统一网关（ADR-012） | ✅ | `app/ui/config` 声明 `gatewayPrefix`/`gatewaySocket`；后端监听 `$TRIM_APPDEST/app.sock` 并在连接入口剥离前缀；端口直连与 `checkport` 保留；前端资源/API/WS 统一带前缀 |
| **运行日志** | 日志页（查看/清空/下载） | ✅ | `GET /api/logs?tail=N`（末尾 N 行）、`POST /api/logs/clear`、`GET /api/logs/download`；文件超 5MB 轮转为 `app.old.log`；调试日志开关（`POST /api/config` 的 `debug`）即时热生效 |
| | 日志无 ANSI 颜色码 | ✅ | tracing 的 stdout 与文件两层均 `with_ansi(false)`；读取/下载时再剥离历史 ANSI 序列（旧日志也不会显示 `[2m`/`[32m`） |
| **性能** | 凭据解密缓存 | ✅ | `ConfigManager::decrypt_field` 以**密文**为键缓存解密结果（age scrypt 单次数百毫秒，凭据属热路径） |
| | 恢复树聚合索引 | ✅ | `SnapshotAgg` 一次遍历建索引：`/api/restore/files`、`/api/restore/tree` 由 O(条目×节点) 降为 O(1) 查询 |
| **可靠性** | 网络失败自动重试 | ✅ | 上传/下载按 `NETWORK_RETRY_ATTEMPTS`（3 次、线性退避）重试；认证/不存在类错误不重试；恢复遇云端缺失跳过并提示「清理缺失记录」 |
| **应用元数据** | HTML 应用介绍 | ✅ | `manifest.desc` 使用 HTML（`<b>`/`<br>`/`<a>`/`<img>`）：一句一行、含官网与反馈渠道、图床宣传图自适应宽度 |
| **Web UI** | Svelte 前端 | ✅ | 导航栏多页面（概览/备份/恢复/设置）；views+components 分层 |
| | 用户信息 | ✅ | WebDAV 账号卡片（UserCard 组件；WebDAV 无套餐/容量接口，不展示容量条） |
| **HTTP API** | 备份/恢复/配置 | ✅ | `http/routes.rs`，axum 路由 |
| | 用户信息 | ✅ | `/api/user/info` 返回本地配置的 WebDAV 账号（WebDAV 无配额/套餐属性） |
| | 密钥管理 | ✅ | `GET/POST /api/keys`（查公钥 / 自定义私钥）、`POST /api/keys/generate`（自动生成并一次性回传私钥；密钥热切换无需重启） |
| **配置** | 加密 TOML 配置 | ✅ | WebDAV 用户名/密码以 `enc:<age密文>` 形式加密存储（age scrypt 口令派生；REST 时代的密码/token 字段已不存在） |
| | 记录账号 | ✅ | 配置解密 username_enc（`webdav_credentials()`） |
| | 导入/导出 | ✅ | `POST /api/config/export`、`POST /api/config/import`（均需管理员口令）：导出备份路径/定时/通知/WebDAV 凭据/age 私钥为 JSON；导入后重新加密凭据并热切换密钥与目标 |
| **测试** | 端到端测试 | ✅ | 真实 kzwr 备份/恢复/删除/多级文件夹/物理删除/保留策略/定时触发 |
| **飞牛部署** | `.fpk` 打包 | ✅ | 完整包结构 + 生命周期脚本 + wizard + GitHub Actions 双架构构建（见 11.6） |
| **监控告警** | 失败通知/告警 | ✅ | 备份/恢复失败与配置缺失生成告警：应用内横幅展示 + 可选 Webhook 外发（`domain/alerts.rs`、`/api/alerts`、`/api/notify/webhook`） |
| | Webhook 自定义 | ✅ | 支持自定义请求头与请求体模板（占位符 `{{message}}`/`{{level}}`/`{{source}}`/`{{ts}}`/`{{id}}`）；`POST /api/notify/webhook/test` 可用当前表单值直接测连通性 |
| **密钥管理** | age 密钥查看/更换 | ✅ | `GET/POST /api/keys`、`POST /api/keys/generate`；密钥热切换无需重启（设置页 KeySection） |
| | 私钥备份/恢复 | ✅ | `POST /api/keys/export`（**需管理员口令校验**后导出另存，导出即重置为未确认）、`POST /api/keys/backup-ack`（备份确认）；未确认备份时设置页持续提示「私钥丢失将无法恢复」 |
| **多目标** | 备份到多个目标 | ❌ 放弃 | 按用户决策，保留策略实现，多目标不做 |

### 11.3 当前实际 Rust 源码结构

```
backend/src/
├── main.rs              # 入口: axum HTTP 服务启动
├── lib.rs               # 库入口 (AppState 等)
├── http/
│   ├── mod.rs
│   ├── routes.rs        # 路由 + 各 handler (backup/restore/config/health/user_info/keys)
│   └── ws.rs            # WebSocket 状态推送
├── domain/
│   ├── mod.rs
│   ├── alerts.rs        # AlertSink / Alert（监控告警，含 Webhook 外发）
│   ├── backup.rs        # BackupJob (备份调度 + 保留策略接入)
│   ├── sync.rs          # SyncSession (差分)
│   ├── crypto.rs        # CryptoSession (age 加密) + AgeKeys
│   ├── pace.rs          # SpeedMeter（传输速度计量：累计实际字节/实际传输耗时）
│   ├── restore.rs       # RestoreJob (恢复编排 + 恢复后回写快照)
│   ├── retention.rs     # RetentionPolicy (孤儿文件清理)
│   └── scheduler.rs     # Scheduler (cron 定时备份调度)
├── infra/
│   ├── mod.rs
│   ├── storage_trait.rs # TargetStorage/SourceStorage trait + SwapTarget/UnconfiguredTarget
│   ├── source/local/    # LocalFsSource
│   ├── target/webdav.rs # WebdavTarget (官方 WebDAV，ADR-009)
│   ├── persistence/snapshot.rs  # SnapshotStore (SQLite)
│   ├── config.rs        # ConfigManager (TOML，含 WebDAV 凭据加解密)
│   └── keystore.rs      # 密钥库
├── bin/                 # 测试二进制 (开发期)
├── eventbus.rs          # EventBus (tokio::broadcast)
└── ...
```

### 11.3.1 当前前端结构

```
frontend/src/
├── App.svelte              # 应用壳：导航 + 页面切换 + 全局状态/WebSocket
├── main.js                 # Svelte 挂载入口
├── views/                  # 页面级组件
│   ├── DashboardPage.svelte  # 概览：UserCard + Overview（实时任务为右侧常驻面板）
│   ├── BackupPage.svelte     # 备份：配置 + 定时 + 执行
│   ├── RestorePage.svelte    # 恢复
│   └── SettingsPage.svelte   # 设置：UserCard + WebDAV 配置 + 密钥管理 + 通知 + 配置备份/恢复
├── components/             # 功能区块组件
│   ├── LiveStatus.svelte      # 实时任务状态（右侧常驻面板：文件进度/明文大小/速度/用时 + 空闲态）
│   ├── OverviewSection.svelte # 配置概览（网格卡片）
│   ├── UserCard.svelte        # WebDAV 账号（WebDAV 无容量/套餐接口）
│   ├── WebdavSection.svelte   # WebDAV 凭据配置（ping 验证后加密保存）
│   ├── KeySection.svelte      # age 密钥管理（公钥展示 / 自定义私钥 / 自动生成 / 口令校验后显示私钥 / 备份确认）
│   ├── AlertBanner.svelte     # 监控告警横幅（失败/异常列表 + 清空）
│   ├── NotifySection.svelte   # 通知设置（Webhook 地址 + 自定义请求头/请求体模板 + 连通性测试）
│   ├── ConfigSection.svelte   # 配置备份/恢复（导出/复制/下载 JSON；粘贴或选文件导入）
│   ├── BackupConfigSection.svelte # 备份路径（增删即自动保存）+ 定时 cron（手动保存）
│   ├── BackupSection.svelte   # 备份执行
│   └── RestoreSection.svelte  # 恢复：文件夹概况 + 「全部恢复」+ 懒加载目录树
└── TreeNode.svelte          # 目录树节点（子项由父级懒加载后经 cache 传入；目录显示文件数/文件夹数与直接恢复按钮）
```

### 11.4 关键决策落地说明

- **镜像一致而非多版本**：目标端与本地保持一致（`8ee9b0a` 移除版本树），不做多版本历史，简化恢复与保留语义
- **保留策略语义**：因无多版本，保留策略聚焦"目标端孤儿文件清理"（不在任何 job 快照中的残留），防目标空间膨胀
- **恢复目标**：支持恢复到配置源路径（原位置）或指定目录；目录用"新建/覆盖"按钮控制
- **配置热切换**：UI 保存 WebDAV 凭据后 `SwapTarget` 即时切换目标实现，无需重启（`storage_trait.rs`）
- **定时备份**：cron 表达式到点触发，运行中改配置热更新（`domain/scheduler.rs`）；调度循环 await 备份完成后才排下一轮
- **备份运行互斥**：定时调度与手动触发共用 `AppState.backup_running`（`AtomicBool`，`run_backup_now` 入口 CAS 抢占 + RAII 守卫复位），已有备份在执行时第二次触发立即返回 `skipped = true` 与提示文案，避免并发备份争抢带宽与快照写入
- **用户信息**：账号记录在配置（username_enc 解密），`/api/user/info` 返回本地账号；WebDAV 无套餐/容量接口
- **备份目标布局**（ADR-010）：每个所选源文件夹在目标端以其**文件夹名**分目录存放，避免多路径在同一层互相覆盖，并保证「所选文件夹」本身在网盘可见
- **进度口径**：上传/下载的「大小」按**明文**展示（总量来自扫描/快照），完成数在**单个文件传输完成后**才 +1
- **速度口径**：由后端 `domain/pace.rs::SpeedMeter` 计量（累计实际传输字节 ÷ 累计实际传输耗时），文件/请求之间的空闲不计入，前端只展示不重算
- **恢复防重传**：恢复写盘后回写该文件快照（size + 实际 mtime），下次增量差分命中「未变化」，避免恢复后又全量重传

### 11.5 后续待办（按优先级）

1. ✅ **飞牛生产环境回归**（WebDAV 模式）：`.fpk` 安装启动、WebDAV 凭据配置与热切换、备份/恢复/保留策略/定时全链路已在 x86 飞牛设备实测通过
2. ✅ **飞牛 `.fpk` 打包 + GitHub Actions 双架构构建**（见 11.6）；x86 实测通过
3. ✅ **监控告警**：备份/恢复失败与配置缺失生成告警，应用内横幅展示 + 可选 Webhook 外发
4. ✅ **密钥丢失恢复流程**：设置页可随时「显示私钥」另存备份，并可「我已妥善保存」确认；未备份时持续提示丢失风险（`POST /api/keys/export`、`POST /api/keys/backup-ack`）
5. ❌ **大文件块级增量**：**不做**（用户决策，2026-09-19）——维持整文件差分（mtime+size / 严格 BLAKE3），不引入块级哈希
6. ✅ **交互与能力补齐（v0.1.4 → v0.1.9）**：备份目标按源文件夹名分层（ADR-010）、实时任务合并统计与后端计量速度、上传/下载显示明文总量与已传量、Webhook 自定义请求头/请求体模板与连通性测试、配置导入/导出、显示私钥需管理员口令校验、恢复后回写快照防重复上传、修复分片请求 413
7. **aarch64 设备实测**：CI 已产出双架构包，需在 aarch64 飞牛设备上验证二进制可用性（**当前唯一遗留项**）

### 11.6 飞牛应用打包实现（基于抓取到的飞牛开发文档）

> 已依据 `docs/fnnas-dev-docs/`（抓取自 developer.fnnas.com）完成 `.fpk` 打包结构。

**打包源目录**：`packaging/fn-kzwr-backup-app/`（可提交，CI 与本地构建共用）；构建产物与 `.fpk` 输出至 `dist/fn-kzwr-backup-app/`（gitignored，脚本 `Scripts/build_fnos_app.sh`）

```
packaging/fn-kzwr-backup-app/
├── manifest                    # 元数据：platform=x86, ctl_stop=true, service_port=8080
├── ICON.PNG / ICON_256.PNG     # 128/256 图标
├── app/                        # → $TRIM_APPDEST（安装后为 /var/apps/{appname}/target）
│   ├── ui/config               # 桌面入口：统一网关（gatewayPrefix=/app/fn-kzwr-backup, gatewaySocket=app.sock）
│   ├── ui/images/              # 入口图标
│   ├── bin/                    # fn-kzwr-backup（Rust）
│   └── www/                    # 前端构建产物（Svelte dist）
├── cmd/                        # main/install/upgrade/uninstall/config 生命周期脚本
├── config/
│   ├── privilege               # run-as=package, user/group=fnosbackup
│   └── resource                # data-share: fn-kzwr-backup/restore
└── wizard/                     # install/config/upgrade/uninstall（JSON 步骤数组）
```

**关键落地点（对照飞牛规范）**：
- **应用形态**：普通应用（非 Docker）；UI 经**统一网关**（`/app/fn-kzwr-backup` + Unix Socket）暴露，端口服务 `service_port=8080` 保留作直连与健康检查（ADR-008 / ADR-012 / 选型 5）
- **路径**：全部使用 `TRIM_*` 环境变量（`TRIM_APPDEST`/`TRIM_PKGETC`/`TRIM_PKGVAR`/`TRIM_PKGTMP`/`TRIM_SERVICE_PORT`/`TRIM_USERNAME`），禁止硬编码
- **权限**：`run-as=package` 专用用户 `fnosbackup`；`cmd/main` 用 `runuser -u $TRIM_USERNAME` 降权启动服务进程
- **源目录授权**：`disable_authorization_path=false`，用户在应用设置授权备份源目录
- **端口**：`manifest.service_port` + 安装向导收集（wizard/install），写入 `$PKGETC/.port`，`cmd/main` 优先读取
- **口令**：安装向导收集管理员口令 → `$PKGETC/.passphrase`（权限 600），用作配置/age 密钥库加密
- **数据归属**：快照→`$TRIM_PKGVAR`；配置/密钥库→`$TRIM_PKGETC`
- **错误输出**：生命周期脚本失败时写 `TRIM_TEMP_LOGFILE`
- **升级**：`upgrade_init` 备份快照/配置/密钥库，支持回滚
- **卸载**：默认保留数据；`wizard/uninstall` 勾选清除时删除

**构建与 CI**：
- 本地脚本 `Scripts/build_fnos_app.sh`：`cargo build --release` + `npm run build` + 组装包 + `fnpack build`（产物输出至 `dist/`）
- GitHub Actions `.github/workflows/build-fnos-app.yml`：`x86_64-unknown-linux-musl` + `aarch64-unknown-linux-musl` 双架构交叉编译、前端构建、fnpack 打包、artifact 上传

**早期 WSL 构建测试（2026-08-22，构建环境已废弃，仅存档）**：
- 后端 `cargo build --release` 编译成功（2m02s，4 个 warning）
- 前端 `vite build` 产物生成（52KB JS + 10.6KB CSS）
- 后端运行实测：`/api/health`、`/api/config`、`/api/user/info` 返回 200；前端 SPA 静态托管正常；WebSocket `/api/ws` 握手 `101 Switching Protocols`
- `fnpack build` 生成 `fn-kzwr-backup.fpk`（gzip 格式，3.6MB），包内 manifest/cmd/config/wizard/app.tgz 结构完整、脚本可执行

**已知限制**：fnpack v1.2.3 校验 wizard 时**不支持 `checkbox`/`switch` 字段类型**（文档虽列出但实际打包会失败），需用 `radio`/`select` 替代。本应用卸载确认已改用 `select`（keep/purge）。

**设备实测结果（2026-09-19，x86 飞牛设备）**：`.fpk` 安装与启动正常；iframe 内 WebSocket 实时状态正常；WebDAV 凭据配置与热切换正常；备份/恢复/保留策略/定时触发全链路验证通过；`run-as=package` 读取授权目录正常。

**待实测**：aarch64 架构二进制在对应飞牛设备上的可用性（GitHub Actions 已产出双架构包）。

---

> 本文档为权威架构基线。功能迭代时同步更新第 8 节（路线图状态）、第 10 节（选型状态）与第 11 节（项目进度）。
