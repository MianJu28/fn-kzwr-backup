# fnos-backup — 飞牛 NAS 增量加密备份

基于酷族网软（kzwr.com）官方 **WebDAV** 的**增量加密备份**工具，运行于飞牛 fnOS。

将本地文件夹增量、加密备份至 kzwr 云盘，支持选择性恢复，并提供 Web 管理界面。

## ✨ 功能特性

- **增量备份**：mtime+size 快速差分，可选 BLAKE3 严格模式（内容哈希确认）
- **age 加密**：X25519 公私钥，公钥加密/私钥解密，私钥可被口令派生密钥加密存储（密钥库）
- **断点续传**：每文件上传后即时写快照，中断可续
- **多路径备份**：多个源目录独立快照，共享目标前缀
- **保留策略**：备份后自动清理目标端孤儿文件，防空间膨胀
- **定时备份**：cron 表达式定时自动触发，运行中热更新
- **选择性恢复**：Web 端树形目录浏览，按文件/目录恢复
- **kzwr 官方 WebDAV 目标**（ADR-009）：文件管理全部走官方 WebDAV（Basic 认证，下载 302 → presigned 跟随）；早期逆向 REST API 适配器与登录二进制已完全移除
- **Web UI**：导航栏多页面，实时进度（WebSocket）、WebDAV 凭据配置（保存前自动实测连通性）

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
- 恢复时用 **私钥解密**；私钥（`agekeys.txt`）被管理员口令派生密钥加密存储
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

> 开发期通过 WSL 构建，将 Windows 侧 `backend/src/` 同步到 WSL `$HOME/fnos-backup/src` 后 `cargo build`。

### 运行

```bash
export TRIM_PKGVAR=$HOME/rf-var      # 数据目录（SQLite/密钥库）
export TRIM_PKGETC=$HOME/rf-cfg      # 配置目录（config.toml）
export TRIM_PASSPHRASE=your-pass     # 密钥库口令
export TRIM_WWW_DIR=frontend/dist    # 前端产物
export TRIM_HTTP_PORT=8098
./target/release/fnos-backup
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
- 🔶 Phase 4：保留策略 / 断点续传 / WebSocket 监控 / 定时备份 / WebDAV 目标已完成；飞牛 `.fpk` 部署实测待完成
- ⏳ Phase 5：监控告警、密钥轮换、异地恢复
