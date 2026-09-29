# fn-kzwr-backup — 飞牛 NAS 增量加密备份

基于酷族网软（kzwr.com）官方 **WebDAV** 的**增量加密备份**工具，运行于飞牛 fnOS。

将本地文件夹增量、加密备份至 kzwr 云盘，支持选择性恢复，并提供 Web 管理界面。

## ✨ 功能特性

- **增量备份**：mtime+size 快速差分，可选 BLAKE3 严格模式（内容哈希确认）
- **age 加密**：X25519 公私钥，公钥加密/私钥解密，私钥可被口令派生密钥加密存储（密钥库）
- **断点续传**：每文件上传后即时写快照，中断可续
- **多路径备份**：多个源目录独立快照，各自在网盘以**源文件夹名**建目录（`/目标文件夹/<源文件夹名>/…`，含空目录）
- **多目标 · 多任务**：可配置**多个备份目标**（各自独立的 WebDAV 地址与账号凭据，凭据加密存储、保存前实测连通性），并把「源文件夹 + 目标 + 定时 + 保留策略」组合成**多个备份任务**——每个任务在每个目标上都有**独立快照、独立增量与独立保留策略**，某个目标异常不影响其它任务；同一个源可同时分别备份到不同账号（异地/多账号容灾）。升级时旧配置自动迁移为默认任务/目标，既有备份记录继续增量、不会重传
- **大文件分片**：超过 90MiB 自动拆分为 `.part0001…` 依次上传（规避站点 100MB 限制）；流式请求体保留 `Content-Length` 并实时上报进度
- **独立「插件」页**：插件能力卡片与外置插件（动态库）管理集中在一页，界面**由插件自身声明**（通用 UI Schema）驱动 → 新增插件无需重新打包应用与前端。随包插件由官方私钥签名、公钥内置，**开箱即用**；自签插件在插件页填公钥即可。**插件配置由插件自管**（ADR-021）：写入自己的数据目录，敏感内容用宿主提供的 `seal`/`unseal` 加密原语（密钥不交给插件）；卸载时若仍被任务/目标引用则拒绝
- **插件运行时启停 + 独立设置弹窗**：每个插件可单独**启用/停用**，立即生效**无需重启**（停用是逻辑摘除，代码仍驻留内存）；点卡片「设置」在**弹窗**内配置，内容由插件自己声明。安全保护：停用仍被任务使用的插件会**级联停用**那些任务并告知；若有任务正在备份，则**拒绝**停用以避免打断
- **保留策略**：备份后自动清理目标端孤儿文件，防空间膨胀；可选**清空云端回收站**（占用/时间门槛可调）
- **定时备份**：cron 表达式定时自动触发，运行中热更新；与手动触发**全局互斥**，不并发执行；修改即预览**接下来 5 次运行时间**（服务器本地时区）
- **增强功能（可选）**：浏览器登录酷族后从 Cookie 复制 `access-token` 填入「插件」页，即可查看**存储空间/账号详情**、**清空云端回收站**、**空间占用预警**（阈值可配）；token 失效自动告警。不影响备份/恢复（始终走 WebDAV）
- **一键体检**：概览页逐项检查 WebDAV 连通性（实连）、私钥备份确认、备份路径、定时任务与增强功能，并给出修复建议
- **操作审计**：凭据变更、密钥导出、配置导入、备份/恢复、清空回收站等操作本地留痕（`audit.log`，自动裁剪）
- **选择性恢复**：Web 端树形目录浏览，按文件/目录恢复；恢复后自动回写快照，**下次备份不会重复上传**
- **kzwr 官方 WebDAV 目标**（ADR-009）：文件管理全部走官方 WebDAV（Basic 认证，下载 302 → presigned 跟随）；早期逆向 REST API 适配器与登录二进制已完全移除
- **配置导入/导出**：一键导出/导入全部设置（备份路径、定时、Webhook、WebDAV 凭据、age 私钥），便于重装或换机恢复
- **监控告警**：备份/恢复失败与配置缺失生成告警（应用内横幅 + 可选 Webhook 外发，支持自定义请求头与请求体模板，并可一键测试连通性）
- **密钥安全**：私钥可随时导出另存（需管理员口令校验），未确认备份时持续提示丢失风险
- **插件化扩展（ADR-013）**：备份目标与增强能力都是**插件**，且**全部外置**（ADR-015）——核心不含任何厂商专属逻辑；`webdav` 用同一套 ABI 表内置实现，`kzwr` 为外置 `.so`。外置插件（动态库 `*.so`，默认关闭）：把插件放进 `$TRIM_PKGETC/plugins/` 并在「插件」页开启即可扩展新卡片/新接口，**无需重新打包主程序与前端**。
  - **稳定 C ABI（唯一机制）**：跨边界只传 `repr(C)` 函数表 + JSON（契约见 `docs/PLUGIN_ABI.md`），插件只依赖零第三方依赖的 `plugins/sdk` → **主程序升级不需要重编插件**。**内置插件与外置插件走同一份契约**
  - **自定义备份目标**：通过**目标能力表** `KzwrTargetAbi` 提供新目的地（对象存储/自建协议等）；推块模式下插件**只接触 age 密文**，明文与密钥永不离开宿主；宿主做字节复查与看门狗。**外置 `.so` 插件同样可以提供备份目标**（SDK 的 `export_target_v1!`，示范见 `plugins/example-localfs/`）
  - **并发回传**：支持的目标可自己决定「传哪些、一次传几批」，宿主按批并发推送；开关与并发度**按目标存储**（ADR-018）——同一个插件的多个目标各配各的（按插件存一份会导致「改一个目标、同类型目标全变」），保存即生效、无需重启
  - ~~Rust 直连（进阶）~~ 已移除：Rust 无稳定 ABI、升级必重编；原「只有它能写自定义目标」已由目标能力表解除
  - 单个插件加载失败只进诊断、不影响核心；设置页可见加载机制与结果
- **Web UI**：导航栏多页面（概览/任务/目标/插件/恢复/设置/审计/日志），实时进度（WebSocket，含速度/大小/用时）；凭据在「目标」页按目标配置（保存前自动实测连通性）
- **任务阶段**：实时任务面板展示 **准备 → 传输 → 收尾** 全流程（扫描差分、上传/下载、删除多余文件与保留策略清理），不再只有传输过程
- **运行日志**：日志页查看/清空/下载运行日志，调试日志开关即时生效（倒序显示、最新在上，已去除 ANSI 颜色码，时间按宿主 NAS 时区，历史 UTC 行自动换算）
- **目标表单弹窗化**：新建/编辑目标在弹窗内完成，字段**完全由插件声明**（`target.form[]`：文本/密码/数字/开关/下拉）→ 新增目标类型无需改前端（ADR-019 / ADR-020）
- **插件沙箱**：每插件跑在**专属线程**上并施加 **Landlock 白名单**（ADR-023）——插件读不到 `keystore.age`、主口令与宿主配置目录，也读不到 `/proc/<宿主pid>/mem`；内核不支持时自动降级（功能不受影响），可用 `FN_KZWR_NO_SANDBOX=1` 临时关闭
- **访问方式**：飞牛桌面经**统一网关**打开（`/app/fn-kzwr-backup`，宿主同源反代到应用 Unix Socket）——以 https 访问飞牛时不再被浏览器混合内容拦截；**应用不监听 TCP 端口**（唯一入口为网关，需直连调试时手动设 `FN_KZWR_DEBUG_PORT`）

## 🏗️ 架构

| 目录 | 说明 |
|------|------|
| `backend/` | Rust 后端（axum API + 备份/恢复核心 + 插件宿主） |
| `frontend/` | Svelte 前端（vite 构建，多页面导航，界面完全声明式） |
| `plugins/` | 插件源码：`sdk/`（零依赖契约）+ `kzwr/`（增强）+ `example-localfs/`（目标示范） |
| `packaging/` | 飞牛应用**打包源**（manifest / cmd / config / wizard） |
| `Scripts/` | 构建、插件构建与签名脚本 |

后端分层（`backend/src/`）：

```
main.rs / lib.rs  入口与 AppState（配置·插件·目标池·事件总线·告警·审计）
http/             接口层（routes/ 按职责拆分 + ws.rs 状态推送）
domain/           领域核心（备份/同步/加密/恢复/保留/调度/告警/审计，纯逻辑）
plugin/           插件层（稳定 C ABI 契约 + 适配器 + 能力表 + 沙箱 + 加载器）
infra/            基础设施（source/target 适配器、SQLite 快照、密钥库、TOML 配置）
eventbus.rs       内部事件总线（tokio::broadcast）
```

完整目录结构与模块职责见 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) §7。

## 🔐 安全模型

- 备份数据在本地用 age **公钥加密**后上传，kzwr 只存储密文；**全程流式，明文不落盘**
- 恢复时用 **私钥解密**；私钥存于 `keystore.age`（`$TRIM_PKGETC`，权限 **0600**），由管理员口令（age scrypt 派生密钥）加密存储，永不明文落盘
- **主口令**由安装向导写入 `$PKGETC/.passphrase`（0600），它是配置凭据与密钥库的共同钥匙
- WebDAV 凭据以 `enc:` 密文存储于配置（age scrypt），凭据**永不回传前端**；UI 保存前先实测连通性
- **插件配置由插件自管**（ADR-021）：宿主不代存，敏感内容经宿主 `seal`/`unseal` 加密（密钥不交给插件）
- **插件沙箱**（ADR-023）：每插件专属线程 + Landlock 白名单，插件读不到密钥库、主口令与宿主配置目录
- 敏感操作（导出私钥 / 配置导入导出）需管理员口令校验

> **已知边界**：插件与宿主**同进程**，沙箱挡住磁盘上的密钥与口令，但**同进程内存仍可读**；
> 插件卡死无法强杀。详见 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) §6。

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

> 主数据为 **`targets`（目标）+ `tasks`（任务）**；旧的 `[backup]`/`[webdav]` 段会自动迁移为 `default` 任务/目标，并作为兼容镜像继续回写（降级旧版本仍可读）。日常无需手改——「任务」「目标」页即可管理。

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

仓库内只保留**描述当前实现**的文档：

- `docs/ARCHITECTURE.md` — **架构基线**：领域模型、C4 视图、技术栈、**安全模型**、目录结构、质量属性、选型结论、决策记录索引
- `docs/PLUGIN_ABI.md` — **插件接口契约（稳定 C ABI v1）**：符号、JSON schema、**目标能力表（自定义备份目标）**、并发回传、版本演进规则、安全边界（内置与外置插件同契约）
- `docs/fnnas-dev-docs/` — 抓取的飞牛官方文档镜像（只读参考）

> **开发过程档案**（架构决策记录 ADR、演进路线图、逐版本进度、插件方案评审、
> 技术选型分析、隔离方案评估）在 `docs/memory/`，该目录**不入版本库**（`.gitignore` 排除），
> 仅作本地开发记录。

## 📦 产物与仓库约定

**二进制产物一律不入库**（`.gitignore` 已拦截 `*.fpk` / `*.xpi` / `kzwr_login_*`）：

- 前端产物与 `.fpk` 输出到 **`dist/`**（整个目录被忽略）；插件 `.so` 由
  `Scripts/build_plugins.sh` 产出到 `dist/plugins/`。
- **签名私钥**（`Scripts/keys/sign.key`）绝不入库，仓库只保留公钥。
- 需要分发安装包时走 **GitHub Releases / CI 制品**，不要提交进仓库。

> **教训（ADR-017）**：历史上 `bin/` 下曾误提交 fpk 与登录器二进制（16 个 blob、解压共
> 1.16 GiB）。删掉文件**并不能**缩小仓库 —— 它们永久留在 Git 历史里，最终把 `.git` 撑到
> **670 MB**（源码仅 2 MB），只能靠改写 95 个提交的 hash 才清除干净。现已补齐规则防止重犯。

## 🗺️ 路线图

- ✅ **MVP → 增量加密 → 恢复能力**
- ✅ **保留策略 / 断点续传 / WebSocket 监控 / 定时备份 / WebDAV 目标 / 监控告警 / 飞牛 `.fpk` 打包与 x86 设备实测**
- ✅ **多目标 · 多任务**：多目标各自凭据与快照、每任务独立调度与保留策略
- ✅ **插件化**：稳定 C ABI（内置与外置同契约）+ 目标能力表（自定义备份目标）+ 并发回传 +
  签名强制校验（内置官方公钥，随包插件开箱即用）+ 声明式插件界面与目标表单 +
  配置由插件自管（宿主提供 `seal`/`unseal`）+ 宿主能力表 `host_bind` + **插件 Landlock 沙箱**

**后续**：aarch64 设备实测、密钥轮换、异地恢复、（可选）插件 sha256 白名单与插件市场。

> **插件签名（发布相关）**：宿主**默认强制验签**，随包插件由官方私钥签名、公钥编译进宿主
> （`backend/src/plugin/loader.rs` 的 `OFFICIAL_PUBKEYS`），用户零配置即可加载。
> 仓库内**只有公钥**：私钥为 `Scripts/keys/sign.key`（被 `.gitignore` 排除），
> CI 发布需在 Secrets 配置 `PLUGIN_SIGN_KEY_B64`（`base64 -w0 Scripts/keys/sign.key`）。
> 契约细节见 `docs/PLUGIN_ABI.md`。
>
> **链接方式**：发布包为 **glibc 动态链接**（非 musl 静态）——musl 不支持 `cdylib`、
> 静态 musl 也无法 `dlopen`，与外置插件互斥（见 `docs/ARCHITECTURE.md` §7.2）。
