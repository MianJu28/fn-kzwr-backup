# fn-kzwr-backup — 飞牛 NAS 增量加密备份

基于酷族网软（kzwr.com）官方 **WebDAV** 的**增量加密备份**工具，运行于飞牛 fnOS。

将本地文件夹增量、加密备份至 kzwr 云盘，支持选择性恢复，并提供 Web 管理界面。

## ✨ 功能特性

- **增量备份**：mtime+size 快速差分，可选 BLAKE3 严格模式（内容哈希确认）
- **age 加密**：X25519 公私钥，公钥加密/私钥解密，私钥可被口令派生密钥加密存储（密钥库）
- **断点续传**：每文件上传后即时写快照，中断可续
- **多路径备份**：多个源目录独立快照，各自在网盘以**源文件夹名**建目录（`/目标文件夹/<源文件夹名>/…`，含空目录）
- **大文件分片**：超过 90MiB 自动拆分为 `.part0001…` 依次上传（规避站点 100MB 限制）；流式请求体保留 `Content-Length` 并实时上报进度
- **保留策略**：备份后自动清理目标端孤儿文件，防空间膨胀
- **定时备份**：cron 表达式定时自动触发，运行中热更新；与手动触发**全局互斥**，不并发执行
- **选择性恢复**：Web 端树形目录浏览，按文件/目录恢复；恢复后自动回写快照，**下次备份不会重复上传**
- **kzwr 官方 WebDAV 目标**（ADR-009）：文件管理全部走官方 WebDAV（Basic 认证，下载 302 → presigned 跟随）；早期逆向 REST API 适配器与登录二进制已完全移除
- **配置导入/导出**：一键导出/导入全部设置（备份路径、定时、Webhook、WebDAV 凭据、age 私钥），便于重装或换机恢复
- **监控告警**：备份/恢复失败与配置缺失生成告警（应用内横幅 + 可选 Webhook 外发，支持自定义请求头与请求体模板，并可一键测试连通性）
- **密钥安全**：私钥可随时导出另存（需管理员口令校验），未确认备份时持续提示丢失风险
- **Web UI**：导航栏多页面，实时进度（WebSocket，含速度/大小/用时）、WebDAV 凭据配置（保存前自动实测连通性）

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
export TRIM_HTTP_PORT=8098
./target/release/fn-kzwr-backup
```

WebDAV 凭据可在 Web 界面「设置」中配置，或用环境变量注入：

```bash
export TRIM_DAV_URL=https://dav.kzwr.com/dav
export TRIM_DAV_USER=your-account
export TRIM_DAV_PASS=your-password
```

打开 `http://<nas>:8098` 进入 Web 界面。

### 配置（config.toml）

```toml
[backup]
paths = ["/volume1/data", "/volume1/docs"]
target_folder = "fn-backup"

[backup.retention]
enabled = true
cleanup_unmanaged = true   # 备份后清理目标端孤儿文件
min_age_days = 0
schedule_cron = "0 2 * * *"  # 可选：定时备份

[webdav]                   # WebDAV 凭据（UI 保存后自动生成，敏感字段已加密）
url = "https://dav.kzwr.com/dav"
username_enc = "enc:..."
password_enc = "enc:..."
```

## 📄 文档

- `docs/ARCHITECTURE.md` — 架构与项目进度
- `docs/TECH_SELECTION.md` — 技术选型分析

## 🗺️ 路线图

- ✅ Phase 1-3：MVP、增量加密、恢复能力
- ✅ Phase 4：保留策略 / 断点续传 / WebSocket 监控 / 定时备份 / WebDAV 目标 / 监控告警 / 飞牛 `.fpk` 打包与 x86 设备实测
- ⏳ Phase 5：aarch64 设备实测、密钥轮换、异地恢复
