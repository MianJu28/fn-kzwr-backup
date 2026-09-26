# fn-kzwr-backup — 飞牛 NAS 增量加密备份

基于酷族网软（kzwr.com）官方 **WebDAV** 的**增量加密备份**工具，运行于飞牛 fnOS。

将本地文件夹增量、加密备份至 kzwr 云盘，支持选择性恢复，并提供 Web 管理界面。

## ✨ 功能特性

- **增量备份**：mtime+size 快速差分，可选 BLAKE3 严格模式（内容哈希确认）
- **age 加密**：X25519 公私钥，公钥加密/私钥解密，私钥可被口令派生密钥加密存储（密钥库）
- **断点续传**：每文件上传后即时写快照，中断可续
- **多路径备份**：多个源目录独立快照，各自在网盘以**源文件夹名**建目录（`/目标文件夹/<源文件夹名>/…`，含空目录）
- **多目标 · 多任务**（v0.4.0）：可配置**多个备份目标**（各自独立的 WebDAV 地址与账号凭据，凭据加密存储、保存前实测连通性），并把「源文件夹 + 目标 + 定时 + 保留策略」组合成**多个备份任务**——每个任务在每个目标上都有**独立快照、独立增量与独立保留策略**，某个目标异常不影响其它任务；同一个源可同时分别备份到不同账号（异地/多账号容灾）。升级时旧配置自动迁移为默认任务/目标，既有备份记录继续增量、不会重传
- **大文件分片**：超过 90MiB 自动拆分为 `.part0001…` 依次上传（规避站点 100MB 限制）；流式请求体保留 `Content-Length` 并实时上报进度
- **保留策略**：备份后自动清理目标端孤儿文件，防空间膨胀；可选**清空云端回收站**（占用/时间门槛可调）
- **定时备份**：cron 表达式定时自动触发，运行中热更新；与手动触发**全局互斥**，不并发执行；修改即预览**接下来 5 次运行时间**（服务器本地时区）
- **增强功能（可选）**：浏览器登录酷族后从 Cookie 复制 `access-token` 填入设置页，即可查看**存储空间/账号详情**、**清空云端回收站**、**空间占用预警**（阈值可配）；token 失效自动告警。不影响备份/恢复（始终走 WebDAV）
- **一键体检**：概览页逐项检查 WebDAV 连通性（实连）、私钥备份确认、备份路径、定时任务与增强功能，并给出修复建议
- **操作审计**：凭据变更、密钥导出、配置导入、备份/恢复、清空回收站等操作本地留痕（`audit.log`，自动裁剪）
- **选择性恢复**：Web 端树形目录浏览，按文件/目录恢复；恢复后自动回写快照，**下次备份不会重复上传**
- **kzwr 官方 WebDAV 目标**（ADR-009）：文件管理全部走官方 WebDAV（Basic 认证，下载 302 → presigned 跟随）；早期逆向 REST API 适配器与登录二进制已完全移除
- **配置导入/导出**：一键导出/导入全部设置（备份路径、定时、Webhook、WebDAV 凭据、age 私钥），便于重装或换机恢复
- **监控告警**：备份/恢复失败与配置缺失生成告警（应用内横幅 + 可选 Webhook 外发，支持自定义请求头与请求体模板，并可一键测试连通性）
- **密钥安全**：私钥可随时导出另存（需管理员口令校验），未确认备份时持续提示丢失风险
- **插件化扩展（ADR-013）**：备份目标与增强能力都是**插件**（内置：`webdav` 目标、`kzwr` 增强）；另支持**外置插件（动态库 `*.so`，默认关闭）**——把插件放进 `$TRIM_PKGETC/plugins/` 并在设置页开启即可扩展新卡片/新接口，**无需重新打包主程序与前端**。
  - **稳定 C ABI（推荐）**：跨边界只传 `repr(C)` 函数表 + JSON（契约见 `docs/PLUGIN_ABI.md`），插件只依赖零第三方依赖的 `plugins/sdk` → **主程序升级不需要重编插件**
  - **Rust 直连（进阶）**：能写自定义备份目标，但 Rust 无稳定 ABI → 宿主校验「编译期宿主版本」，升级后需重编
  - 单个插件加载失败只进诊断、不影响核心；设置页可见加载机制与结果
- **Web UI**：导航栏多页面，实时进度（WebSocket，含速度/大小/用时）、WebDAV 凭据配置（保存前自动实测连通性）
- **任务阶段**：实时任务面板展示 **准备 → 传输 → 收尾** 全流程（扫描差分、上传/下载、删除多余文件与保留策略清理），不再只有传输过程
- **运行日志**：日志页查看/清空/下载运行日志，调试日志开关即时生效（倒序显示、最新在上，已去除 ANSI 颜色码，时间按宿主 NAS 时区，历史 UTC 行自动换算）
- **访问方式**：飞牛桌面经**统一网关**打开（`/app/fn-kzwr-backup`，宿主同源反代到应用 Unix Socket）——以 https 访问飞牛时不再被浏览器混合内容拦截；**应用不监听 TCP 端口**（唯一入口为网关，需直连调试时手动设 `FN_KZWR_DEBUG_PORT`）

## 🏗️ 架构

| 目录 | 说明 |
|------|------|
| `backend/` | Rust 后端（axum HTTP API + 备份/恢复核心） |
| `frontend/` | Svelte 前端（vite 构建，多页面导航） |

后端采用分层架构（ADR）：

```
domain/  领域核心（备份/同步/加密/恢复/保留/调度，纯逻辑）
infra/   基础设施（source/target 适配器、SQLite 快照、密钥库、TOML 配置）
http/    接口层（REST + WebSocket）
eventbus/ 内部事件总线（tokio::broadcast）
```

## 🔐 安全模型

- 备份数据在本地用 age **公钥加密**后上传，kzwr 只存储密文
- 恢复时用 **私钥解密**；私钥存于密钥库 `keystore.age`（`$TRIM_PKGETC`），由管理员口令（age scrypt 派生密钥）加密存储，永不明文落盘
- WebDAV 凭据加密存储于配置（age scrypt），UI 保存前先实测连通性

## 🚀 快速开始

### 依赖

- Rust（后端）
- Node.js 20+（前端构建）

### 构建

```bash
# 后端
cd backend && cargo build --release

# 前端
cd frontend && npm install && npm run build
```

> 开发期构建**统一通过 SSH 在飞牛 NAS 上进行**：将 Windows 侧源码打包（`backend/`、`frontend/`、`packaging/`、`Scripts/`）经 `pscp`/tar 同步到 NAS 后执行 `Scripts/build_fnos_app.sh`。WSL 已废弃（上行仅 ~4KB/s、后台进程随会话被回收）。

### 运行

```bash
export TRIM_PKGVAR=$HOME/rf-var      # 数据目录（SQLite/密钥库）
export TRIM_PKGETC=$HOME/rf-cfg      # 配置目录（config.toml）
export TRIM_PASSPHRASE=your-pass     # 密钥库口令
export TRIM_WWW_DIR=frontend/dist    # 前端产物
export TRIM_APP_SOCK=/tmp/fn-kzwr-backup.sock   # 统一网关 Socket（也可用 FN_KZWR_DEBUG_PORT 临时开端口）
./target/release/fn-kzwr-backup
```

WebDAV 凭据可在 Web 界面「设置」中配置，或用环境变量注入：

```bash
export TRIM_DAV_URL=https://dav.kzwr.com/dav
export TRIM_DAV_USER=your-account
export TRIM_DAV_PASS=your-password
```

命令行运行时可设 `FN_KZWR_DEBUG_PORT=8098` 临时开一个本地端口（`http://<nas>:8098`）方便联调，生产不设置。

> 装机后（`.fpk`）**只**由**飞牛统一网关**提供页面访问：`https://<nas>/app/fn-kzwr-backup`（宿主同源反代，https 桌面下不会触发混合内容拦截，WebSocket 同样走该前缀）。应用不再声明 `service_port`、也不监听 TCP 端口。

### 配置（config.toml）

> v0.4.0 起主数据为 **`targets`（目标）+ `tasks`（任务）**；旧的 `[backup]`/`[webdav]` 段会自动迁移为 `default` 任务/目标，并作为兼容镜像继续回写（降级旧版本仍可读）。日常无需手改——「任务」「目标」页即可管理。

```toml
# 目标：一个目的地 = 一个地址 + 一套凭据（可被多个任务共用）
[[targets]]
id = "default"                     # 任务的 target_id 引用它
name = "默认目标（WebDAV）"
kind = "webdav"
url = "https://dav.kzwr.com/dav"
username_enc = "enc:..."           # 账号/密码加密存储（age scrypt）
password_enc = "enc:..."
enabled = true

# 任务：源路径集 + 目标 + 调度 + 保留策略（各自独立增量与快照）
[[tasks]]
id = "default"                     # 快照 key = "{id}-{源序号}"（default-0…）
name = "默认任务"
enabled = true
paths = ["/volume1/data", "/volume1/docs"]
target_id = "default"
target_folder = "fn-backup"
schedule_cron = "0 2 * * *"        # 可选：定时备份（宿主本地时区）

[tasks.retention]
enabled = true
cleanup_unmanaged = true           # 备份后清理该目标上的孤儿文件
min_age_days = 0

# —— 旧字段（自动迁移 / 兼容镜像，可忽略）——
[backup]
paths = ["/volume1/data", "/volume1/docs"]
target_folder = "fn-backup"
[webdav]
url = "https://dav.kzwr.com/dav"
```

## 📄 文档

- `docs/ARCHITECTURE.md` — 架构与项目进度
- `docs/TECH_SELECTION.md` — 技术选型分析
- `docs/PLUGIN_ABI.md` — **外置插件接口契约（稳定 C ABI v1）**：符号、JSON schema、版本演进规则、安全边界

## 🗺️ 路线图

- ✅ Phase 1-3：MVP、增量加密、恢复能力
- ✅ Phase 4：保留策略 / 断点续传 / WebSocket 监控 / 定时备份 / WebDAV 目标 / 监控告警 / 飞牛 `.fpk` 打包与 x86 设备实测 / **多目标 · 多任务**（v0.4.0）
- 🔶 Phase 5：插件化（**内置插件 + 外置动态库加载均已落地**）、aarch64 设备实测、密钥轮换、异地恢复
