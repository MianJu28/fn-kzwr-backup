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
| 源访问 | tokio::fs（本地 FS） | smb-rs / pavao | 仅需本地文件备份，不考虑 SMB/NFS，零依赖 |
| 目标访问 | reqwest（WebDAV 客户端） | rust-s3 | 酷族官方 WebDAV（ADR-009）；逆向 REST API 已移除 |
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
- (-) 酷族对接需自建适配器（现已迁移官方 WebDAV，见 ADR-009）

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
- **UI 入口**：`app/ui/config` 声明 iframe 桌面入口，`type=iframe, protocol=http, port={wizard_port}, url=/`
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
- (-) WebDAV 认证方式与加密密文流式 PUT 的性能需实测（64MB 分块策略是否保留待验证）

---

## 7. 项目目录结构

项目遵循飞牛应用规范，Rust 源码与前端源码在开发期独立，打包时合入飞牛目录结构。

> 以下为**目标结构**（飞牛应用部署形态）。开发期 `backend/` 与 `frontend/` 独立演进，打包时合入飞牛目录。**当前实际 Rust 源码结构**见文末"项目进度"一节。

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
│       └── fnos-backup         # 主二进制
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
| **Phase 1 · MVP** | 全量备份 · 单源单目标 · 基础 Web UI · 本地 FS 源 · 酷族官方 WebDAV 目标（ADR-009） · 飞牛 `.fpk` 打包 | ✅ 核心完成（.fpk 打包待部署） | WebDAV 对接（已解决）· 飞牛生命周期集成 | 完全可逆 |
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
- ✅ 备份/恢复 HTTP API + Svelte Web UI（登录页、多路径配置、恢复树形视图）
- ✅ WebSocket 实时任务监控（ADR-007 事件总线）
- ✅ 保留策略：目标端孤儿文件清理（Phase 4）
- 🔶 飞牛 `.fpk` 打包部署（待验证）

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
| 源目录授权 | config/resource 声明 + 运行时引导，弃 root 模式 | 1 | 🔶 开发期用环境变量；飞牛部署待验证 |
| UI 暴露认证 | 端口服务 + JWT（wizard 设管理员口令），WebSocket 状态推送 | 1 | 🔶 WebSocket✅；JWT 认证待部署 |
| 密钥管理 | age 公私钥（X25519）；备份用公钥加密、恢复用私钥解密；私钥可被口令派生密钥加密存储 | 2 | ✅ 已实现（keystore 加密持久化） |

### 待验证（需实际测试）

1. ❌ **酷族 session token 对接**：**已作废**——属逆向 REST API 能力，随 ADR-009 从代码库移除（WebDAV 走 HTTP Basic 认证，无 session 复用/过期重登概念）
2. ⏳ **config/resource 格式**：查阅飞牛文档确认共享目录声明的具体字段（.fpk 部署阶段）
3. ⏳ **iframe 内 WebSocket**：WebSocket 本地已实测通过；飞牛 iframe CSP 是否允许 localhost WS 需部署验证
4. ⏳ **大文件块级增量**：未引入块级 BLAKE3 哈希（当前整文件差分，块级留待 Phase 5 评估）
5. 🔶 **飞牛 `.fpk` 打包部署**：打包结构 + 双架构构建工作流已完成（11.6），待飞牛设备实测安装

---

## 11. 项目进度（实现状态）

> 本节为**滚动更新的当前进度基线**，随每次功能迭代更新。状态图例：✅ 已完成并实测 · 🔶 部分完成 · ⏳ 规划中。

### 11.1 当前开发状态

**核心备份/恢复主链路已完成并实测通过**，进入 Phase 4 生产强化收尾阶段。构建与测试**统一通过 SSH 在飞牛 NAS 上进行**（WSL 已废弃：上行仅 ~4KB/s、后台进程随会话被回收）；源码从 Windows 侧经 `pscp`/tar 同步至 NAS 后 `cargo build`，运行编译好的二进制或通过 HTTP API 测试。

### 11.2 已实现功能（按模块）

| 模块 | 功能 | 状态 | 说明 |
|------|------|------|------|
| **备份调度** | 增量备份（mtime+size 差分） | ✅ | `BackupJob::run` / `run_strict` / `run_multi` |
| | 断点续传 | ✅ | 每文件上传后即时存快照，中断可续 |
| | 多路径备份 | ✅ | 每个源路径独立 job_id 快照，共享 target_prefix |
| | 保留策略（孤儿清理） | ✅ | `domain/retention.rs`，备份后自动清理目标端孤儿文件 |
| | 定时备份（cron） | ✅ | `domain/scheduler.rs`，cron 表达式到点触发，配置热更新、防重入 |
| **增量同步** | 双策略差分 | ✅ | 快速 mtime+size / 严格 BLAKE3（ADR-004） |
| **加密** | age 公私钥加密 | ✅ | 64MB 分块，公钥加密/私钥解密（ADR-003） |
| | 密钥库持久化 | ✅ | 私钥被口令派生密钥加密存储，跨重启可用 |
| **恢复** | 恢复编排 | ✅ | `RestoreJob`，选择性恢复、恢复到源路径 |
| | 完整性校验 | ✅ | age AEAD tag 自动验证；恢复后内容对比校验 |
| **kzwr 目标** | 官方 WebDAV 适配器 | ✅ | `WebdavTarget`：MKCOL/PUT/GET(302 跟随)/DELETE/PROPFIND；凭据加密存储，保存时 ping 验证并热切换（ADR-009） |
| | 大文件分片上传 | ✅ | 超过 `PART_SIZE`（默认 90MiB = 100MB 网站限制的 90%）自动拆分为 `.part0001…` 依次 PUT；`FNOS_DAV_PART_SIZE` 可覆盖 |
| | 分片下载拼接与清理 | ✅ | 读取时逻辑文件 404 则按序拼接分片；删除同时清理逻辑文件与全部分片；列表将分片合并为逻辑文件 |
| | 目录与删除 | ✅ | 写入前逐级 `MKCOL` 确保父目录；`DELETE` 直接删除（无回收站，两阶段物理删除已随 REST 移除） |
| **存储抽象** | Source/Target trait | ✅ | `storage_trait.rs`（ADR-005），ACL 防腐层 |
| **元数据** | SQLite 快照 | ✅ | `sync_snapshots` 表，WAL 模式（ADR-006） |
| **事件总线** | 内部事件总线 | ✅ | tokio::broadcast，备份/恢复进度事件（ADR-007） |
| **WebSocket** | 实时状态推送 | ✅ | `/api/ws`，前端实时进度条，断线重连 |
| **Web UI** | Svelte 前端 | ✅ | 导航栏多页面（概览/备份/恢复/设置）；views+components 分层 |
| | 用户信息 | ✅ | WebDAV 账号卡片（UserCard 组件；WebDAV 无套餐/容量接口，不展示容量条） |
| **HTTP API** | 备份/恢复/配置 | ✅ | `http/routes.rs`，axum 路由 |
| | 用户信息 | ✅ | `/api/user/info` 返回本地配置的 WebDAV 账号（WebDAV 无配额/套餐属性） |
| | 密钥管理 | ✅ | `GET/POST /api/keys`（查公钥 / 自定义私钥）、`POST /api/keys/generate`（自动生成并一次性回传私钥；密钥热切换无需重启） |
| **配置** | 加密 TOML 配置 | ✅ | kzwr 凭据/密码/token 加密存储（age scrypt） |
| | 记录账号 | ✅ | 配置解密 username_enc（`webdav_credentials()`） |
| **测试** | 端到端测试 | ✅ | 真实 kzwr 备份/恢复/删除/多级文件夹/物理删除/保留策略/定时触发 |
| **飞牛部署** | `.fpk` 打包 | ✅ | 完整包结构 + 生命周期脚本 + wizard + GitHub Actions 双架构构建（见 11.6） |
| **监控告警** | 失败通知/告警 | ✅ | 备份/恢复失败与配置缺失生成告警：应用内横幅展示 + 可选 Webhook 外发（`domain/alerts.rs`、`/api/alerts`、`/api/notify/webhook`） |
| **密钥管理** | age 密钥查看/更换 | ✅ | `GET/POST /api/keys`、`POST /api/keys/generate`；密钥热切换无需重启（设置页 KeySection） |
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
│   ├── restore.rs       # RestoreJob (恢复编排)
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
│   └── SettingsPage.svelte   # 设置：UserCard + WebDAV 配置 + 密钥管理
├── components/             # 功能区块组件
│   ├── LiveStatus.svelte      # 实时任务状态（右侧常驻面板，WebSocket 进度 + 空闲态）
│   ├── OverviewSection.svelte # 配置概览（网格卡片）
│   ├── UserCard.svelte        # WebDAV 账号（WebDAV 无容量/套餐接口）
│   ├── WebdavSection.svelte   # WebDAV 凭据配置（ping 验证后加密保存）
│   ├── KeySection.svelte      # age 密钥管理（公钥展示 / 自定义私钥 / 自动生成并提醒保存）
│   ├── AlertBanner.svelte     # 监控告警横幅（失败/异常列表 + 清空）
│   ├── NotifySection.svelte   # 通知设置（告警 Webhook 地址）
│   ├── BackupConfigSection.svelte # 备份路径 + 定时 cron
│   ├── BackupSection.svelte   # 备份执行
│   └── RestoreSection.svelte  # 恢复目录树
└── TreeNode.svelte          # 目录树递归节点
```

### 11.4 关键决策落地说明

- **镜像一致而非多版本**：目标端与本地保持一致（`8ee9b0a` 移除版本树），不做多版本历史，简化恢复与保留语义
- **保留策略语义**：因无多版本，保留策略聚焦"目标端孤儿文件清理"（不在任何 job 快照中的残留），防目标空间膨胀
- **恢复目标**：支持恢复到配置源路径（原位置）或指定目录；目录用"新建/覆盖"按钮控制
- **配置热切换**：UI 保存 WebDAV 凭据后 `SwapTarget` 即时切换目标实现，无需重启（`storage_trait.rs`）
- **定时备份**：cron 表达式到点触发，运行中改配置热更新（`domain/scheduler.rs`）
- **用户信息**：账号记录在配置（username_enc 解密），`/api/user/info` 返回本地账号；WebDAV 无套餐/容量接口

### 11.5 后续待办（按优先级）

1. ✅ **飞牛生产环境回归**（WebDAV 模式）：`.fpk` 安装启动、WebDAV 凭据配置与热切换、备份/恢复/保留策略/定时全链路已在 x86 飞牛设备实测通过
2. ✅ **飞牛 `.fpk` 打包 + GitHub Actions 双架构构建**（见 11.6）；x86 实测通过
3. ✅ **监控告警**：备份/恢复失败与配置缺失生成告警，应用内横幅展示 + 可选 Webhook 外发
4. **密钥丢失恢复流程**：私钥备份/恢复引导（Phase 5 备用）
5. **大文件块级增量**：按需评估（Phase 5）
6. **aarch64 设备实测**：CI 已产出双架构包，需在 aarch64 飞牛设备上验证二进制可用性

### 11.6 飞牛应用打包实现（基于抓取到的飞牛开发文档）

> 已依据 `docs/fnnas-dev-docs/`（抓取自 developer.fnnas.com）完成 `.fpk` 打包结构。

**打包源目录**：`packaging/fnos-backup-app/`（可提交，CI 与本地构建共用）；构建产物与 `.fpk` 输出至 `dist/fnos-backup-app/`（gitignored，脚本 `Scripts/build_fnos_app.sh`）

```
packaging/fnos-backup-app/
├── manifest                    # 元数据：platform=x86, ctl_stop=true, service_port=8080
├── ICON.PNG / ICON_256.PNG     # 128/256 图标
├── app/                        # → $TRIM_APPDEST（安装后为 /var/apps/{appname}/target）
│   ├── ui/config               # 桌面入口：iframe → http://localhost:8080/，allUsers=true
│   ├── ui/images/              # 入口图标
│   ├── bin/                    # fnos-backup（Rust）
│   └── www/                    # 前端构建产物（Svelte dist）
├── cmd/                        # main/install/upgrade/uninstall/config 生命周期脚本
├── config/
│   ├── privilege               # run-as=package, user/group=fnosbackup
│   └── resource                # data-share: fnos-backup/restore
└── wizard/                     # install/config/upgrade/uninstall（JSON 步骤数组）
```

**关键落地点（对照飞牛规范）**：
- **应用形态**：普通应用（非 Docker），端口服务暴露 UI（ADR-008 / 选型 5）
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

**WSL 构建测试已通过（2026-08-22）**：
- 后端 `cargo build --release` 编译成功（2m02s，4 个 warning）
- 前端 `vite build` 产物生成（52KB JS + 10.6KB CSS）
- 后端运行实测：`/api/health`、`/api/config`、`/api/user/info` 返回 200；前端 SPA 静态托管正常；WebSocket `/api/ws` 握手 `101 Switching Protocols`
- `fnpack build` 生成 `fnos-backup.fpk`（gzip 格式，3.6MB），包内 manifest/cmd/config/wizard/app.tgz 结构完整、脚本可执行

**已知限制**：fnpack v1.2.3 校验 wizard 时**不支持 `checkbox`/`switch` 字段类型**（文档虽列出但实际打包会失败），需用 `radio`/`select` 替代。本应用卸载确认已改用 `select`（keep/purge）。

**设备实测结果（2026-09-19，x86 飞牛设备）**：`.fpk` 安装与启动正常；iframe 内 WebSocket 实时状态正常；WebDAV 凭据配置与热切换正常；备份/恢复/保留策略/定时触发全链路验证通过；`run-as=package` 读取授权目录正常。

**待实测**：aarch64 架构二进制在对应飞牛设备上的可用性（GitHub Actions 已产出双架构包）。

---

> 本文档为权威架构基线。功能迭代时同步更新第 8 节（路线图状态）、第 10 节（选型状态）与第 11 节（项目进度）。
