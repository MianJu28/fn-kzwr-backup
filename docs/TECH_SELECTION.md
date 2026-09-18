# fnos 备份系统 · 技术选型文档

> 覆盖架构文档中全部待决技术选型点。每项含候选方案、权衡对比、推荐与验证步骤。  
> 配套文档：`ARCHITECTURE.md`（架构基线）

---

## 选型 1：酷族网软对接方案

### 背景

Target 适配器需将加密文件写入酷族网软（kzwr.com）。酷族最初为 Angular SPA + 自定义 REST API（v1/v2/v3），登录有 reCAPTCHA v2/Turnstile 验证，认证用 token（`access-token` Header），当时**确认不支持 WebDAV**，因此逆向 REST API 用 Rust 重写。

> **修订（2026-09）**：酷族官方已支持 WebDAV。文件管理（上传/下载/删除/目录操作）**迁移至官方 WebDAV**，逆向 REST API **弃用**（决策详见 `ARCHITECTURE.md` ADR-009）。

### 修订方案：官方 WebDAV（当前选型，已实施）

- 文件管理（上传/下载/删除/列目录/建目录）全部走 kzwr 官方 WebDAV，Rust 侧以 reqwest 实现 WebDAV 客户端（`infra/target/webdav.rs`）
- 认证方式**已验证（2026-09-18）**：HTTP Basic（WebDAV 专用账号密码）；`GET` 下载返回 302 → S3 风格 presigned URL（约 300s 有效），跟随重定向即可取回，无需二次认证
- 逆向 REST API 的分块上传、presigned-url、SHA1/SHA256 哈希对齐逻辑随代码一并移除（sha1/sha2/zip 依赖已删）
- 用户信息不再依赖 REST API（get_member 移除）；WebDAV 无配额属性，UI 仅展示本地配置账号
- 登录二进制及其登录环境子系统（Xvfb/uBlock/Camoufox 下载）一并移除；凭据经 UI 配置（保存前 ping 验证、加密存储、`SwapTarget` 热切换）

**迁移步骤**：
1. ✅ 验证 WebDAV 端点、认证方式与流式 PUT/GET 行为（2026-09-18）
2. ✅ 实现 `WebdavTarget` 适配器（`infra/target/webdav.rs`，实现既有 `TargetStorage` trait，核心逻辑零改动）
3. ✅ 端到端测试（`bin/webdav_backup_test.rs`，真实服务器）：备份/增量/多级目录/下载解密校验通过
4. ✅ 逆向 REST API 适配器与登录二进制相关代码已完全移除（2026-09-18）

### 历史方案：编译二进制登录 + 逆向 REST API Rust 重写（已弃用，代码已移除，仅存档）

酷族当时无标准协议支持（WebDAV 已排除），登录与 API 能力已在参考项目 `kzwr`（`E:\Projects\Git\kzwr`）中实现：

**登录（编译二进制）**：
- 登录实现位于参考项目 `kzwr/kzwr_login_turnstile.py`，用 CloakBrowser（反检测 Chromium）+ Playwright 模拟真人浏览器完成登录
- 验证码走 reCAPTCHA v3，失败时 fallback 到 Cloudflare Turnstile；Turnstile 验证后 POST `/api/v1/turnstile` 换 `preVerificationToken`，最终 POST `/api/v2/login` 获取 `access-token`
- 脚本经 **PyInstaller 编译为单文件二进制**（x86_64 + arm64），本系统以子进程方式调用，产出 `session.json`（`{"access_token", "email", "via"}`）
- **不自动化绕过验证码**——登录由独立二进制模拟真人完成，本系统只复用产出 token

**API（Rust 重写）**：
- API 客户端已用 Rust 重写，对照参考项目 `kzwr/kzwr_api.py` 的 `KzwrClient` 实现（reqwest 替代 requests，sha1/sha2 替代 hashlib）
- 认证方式：每个请求携带 `access-token: {token}` Header
- 分块上传：`POST /api/v2/upload/index` → 获取 presigned-url → `PUT` 上传分片（取 ETag）→ `/upload/chunk` 上报 → `/upload/complete`；分片 4MB、并发 3
- 哈希对齐：文件 SHA1 + 首/尾 4MB SHA256 + 每分片前 64KB SHA256（与前端 chunk JS 一致）
- 下载：`GET /api/v1/object?fid={flagName}&token={token}`

### session token 处理

- session token 由登录二进制产出，存储于 `$TRIM_PKGETC`（加密），备份任务复用
- token 过期（`TOKEN_EXPIRED`）时暂停任务，UI 提示用户重新触发登录二进制
- Rust API 客户端直接读取会话 token，无需手动 reCAPTCHA

---

## 选型 2：飞牛 NAS 源访问方案

### 背景

Source 适配器需读取飞牛 NAS 上的文件。**仅需支持本地文件备份，不考虑 SMB/NFS 等挂载协议。**

### 确定方案：本地 FS 直读（唯一方案）

| 方案       | 库           | C 依赖 | 说明       |
| -------- | ----------- | ---- | -------- |
| 本地 FS 直读 | `tokio::fs` | 无    | 唯一方案，生产级 |

**实现**：

- `tokio::fs` 流式读取用户授权的源目录
- `run-as=package` 权限下读取授权目录（见选型 4）
- Source 适配器只需 `LocalFsSource` 一种实现

**架构影响（简化）**：

- 移除 smb-rs 依赖，零 C 依赖更彻底
- Source 适配器代码量大幅减少
- `Storage` trait 仍保留接口抽象（为未来扩展预留，但不实现 SMB/NFS）
- 增量同步模块直接对接 `LocalFsSource`，无需协议适配层

### 验证步骤

1. 验证 `tokio::fs` 在飞牛 `run-as=package` 下读取授权目录的权限
2. 验证大文件流式读取的内存占用（应常量内存）

---

## 选型 3：双架构编译与打包方案

### 背景

飞牛设备覆盖 x86_64 与 aarch64。需 CI 自动产出双架构二进制并打包 `.fpk`。

### 候选方案

| 方案                    | 描述                                                    | 优点             | 缺点                             |
| --------------------- | ----------------------------------------------------- | -------------- | ------------------------------ |
| A. musl 静态链接 + cross  | `aarch64-unknown-linux-musl` + `cross` 工具（Docker）     | 完全独立二进制，无运行时依赖 | 体积稍大；musl 偶有 C 库不兼容            |
| B. glibc 动态链接 + 交叉工具链 | `aarch64-unknown-linux-gnu` + `gcc-aarch64-linux-gnu` | 体积小，兼容性好       | 依赖目标 glibc 版本                  |
| C. Docker 多阶段构建       | 在 aarch64 容器内原生编译                                     | 无交叉编译问题        | 需 aarch64 环境（QEMU 慢或原生 ARM CI） |

### 推荐：方案 A（musl 静态链接 + cross 工具）

**核心原则：避免一切 C 依赖**，使交叉编译透明化。

依赖选型约束：

- `rusqlite` 启用 `bundled` 特性（静态编译 SQLite，无系统 libsqlite3）
- HTTP 用 `reqwest` + `rustls`（非 native-tls/openssl）
- 加密用 `age` crate（纯 Rust，内部基于 ChaCha20-Poly1305/X25519）
- 私钥保护用 `argon2`（纯 Rust）

CI 流程（GitHub Actions）：

```yaml
jobs:
  build:
    strategy:
      matrix:
        target: [x86_64-unknown-linux-musl, aarch64-unknown-linux-musl]
    steps:
      - uses: actions/checkout@v4
      - uses: houseabsolute/actions-rust-cross@v0
        with:
          target: ${{ matrix.target }}
          args: --release
      - uses: actions/setup-node@v4
      - run: cd frontend && npm ci && npm run build
      - run: cp frontend/dist/* app/www/
      - run: cp target/*/release/fnos-backup target/bin/
      - run: fnpack build  # 产出 .fpk
```

**manifest 的 `platform` 处理**：两个选项——

1. 分架构打包：`platform="x86_64"` 与 `platform="aarch64"` 各一个 `.fpk`
2. 单包双架构：若飞牛支持，在 `install_init` 脚本中按 `uname -m` 选择对应二进制

### 验证步骤

1. 本地用 `cross build --target aarch64-unknown-linux-musl` 验证编译
2. `file target/.../fnos-backup` 确认 ELF aarch64
3. 在飞牛 aarch64 测试设备安装 `.fpk` 验证运行

---

## 选型 4：源目录授权方案

### 背景

飞牛普通应用 `run-as=package` 默认无权访问用户文件。需设计授权机制让应用读取飞牛 NAS 源目录。

### 候选方案

| 方案                    | 描述                          | 安全性        | 用户体验       |
| --------------------- | --------------------------- | ---------- | ---------- |
| A. config/resource 声明 | 安装时声明共享目录，用户在应用设置授权         | 高（声明式）     | 中（需用户手动授权） |
| B. wizard 安装引导        | 安装向导中收集源目录路径并授权             | 高          | 好（一次配置）    |
| C. 运行时动态授权            | 备份配置时引导用户授权新目录              | 高（最小权限）    | 好（按需）      |
| D. run-as=root + 降权   | 以 root 运行绕过权限，服务降权到 package | 低（root 风险） | 差（违反最小权限）  |

### 推荐：A + C 组合

**方案 A（config/resource）**：声明应用需要访问共享目录的能力。这是飞牛标准机制，应用安装后用户在飞牛应用设置中授权目录。

**方案 C（运行时动态授权）**：用户在备份软件 UI 中添加新备份任务时，选择源目录。若目录未授权，UI 引导用户跳转飞牛应用设置授权，或调用飞牛提供的授权 API（若有）。

**避免方案 D**：root 模式违反最小权限原则，飞牛文档明确不推荐长期运行进程用 root。

### 授权状态管理

- SQLite 记录每个备份任务关联的源目录路径与授权状态
- 启动时检查授权目录是否仍有效（权限被撤销时提示用户）
- `config_init`/`config_callback` 脚本在授权变更时通知应用

### 验证步骤

1. 查阅飞牛 `config/resource` 文档确认共享目录声明格式
2. 测试 `run-as=package` 用户读取授权目录的权限行为
3. 验证未授权目录的报错信息可被应用捕获并引导

---

## 选型 5：UI 暴露与应用认证方案

### 背景

应用需通过飞牛桌面暴露 UI，并实现自身认证（管理员口令保护备份配置）。

### UI 暴露选型

| 方案      | 描述                     | 认证集成       | WebSocket |
| ------- | ---------------------- | ---------- | --------- |
| A. 端口服务 | 应用监听 HTTP 端口，iframe 加载 | 独立认证（自建）   | 支持        |
| B. 统一网关 | 复用飞牛系统域名与登录态           | 飞牛 NAS 登录态 | 支持（网关透传）  |
| C. CGI  | 轻量入口，系统域名 + 登录态        | 飞牛登录态      | 不支持       |

### 推荐：方案 A（端口服务）

**MVP 阶段选端口服务**：

- 实现最简单，与飞牛登录态解耦
- iframe 内 WebSocket 可正常工作（同源 localhost）
- 应用自带管理员口令认证（wizard 安装时设置）

**应用自身认证设计**：

- 首次安装 wizard 收集管理员口令（Argon2id 哈希存储）
- HTTP 请求需携带 JWT（登录后签发，存 sessionStorage）
- axum 中间件校验 JWT，未认证返回 401
- WebSocket 连接建立时校验 JWT

**何时评估方案 B（统一网关）**：

- 若需复用飞牛 NAS 登录态（用户不想再登录一次）
- 若需获取飞牛用户上下文（多用户场景）
- Phase 4 生产强化时评估

### 验证步骤

1. 验证飞牛 iframe 内 WebSocket 连接（可能受 CSP 限制）
2. 验证 localhost 端口在飞牛环境下的网络可访问性
3. 测试 JWT 在 iframe sessionStorage 中的持久性

---

## 选型 6：密钥管理方案

### 背景

加密已改为 age（X25519 公私钥）。age 采用公钥加密、私钥解密，无需口令派生主密钥。需安全存储私钥、支持私钥备份与恢复。

### 候选方案

| 方案             | 密钥存储                                   | 轮换       | 恢复        | 安全性   |
| -------------- | -------------------------------------- | -------- | --------- | ----- |
| A. age 公私钥   | 备份用公钥加密；私钥明文存 `$TRIM_PKGETC`          | 换公钥即可（旧私钥仍可解旧档） | 私钥丢失则无法恢复 | 中（依赖文件权限） |
| B. age + 口令保护私钥 | 备份用公钥加密；私钥被口令派生密钥加密后存 `$TRIM_PKGETC` | 支持（重新加密） | 口令遗忘则无法恢复 | 高     |
| C. age + 口令 + 私钥备份 | 公钥加密 + 口令保护私钥 + 备份私钥副本离线保存 | 支持       | 备份私钥解密     | 高     |

### 推荐：方案 B（age 公钥加密 + 口令保护私钥），可选 C

**密钥层次**：

```
备份:  明文 ──age 公钥──→ 密文 (每 chunk 独立文件密钥, age 标准 ephemeral key)
恢复:  密文 ──age 私钥──→ 明文

私钥保护: 用户口令 ──Argon2id──→ 密钥加密密钥(KEK)
                                     └── 加密 age 私钥，存 $TRIM_PKGETC/keystore

私钥备份(可选, C): 备份 age 私钥副本离线保存，用于口令遗忘时恢复
```

**Argon2id 参数**（仅用于保护私钥，OWASP 推荐基线）：

- memory: 256 MiB
- iterations: 3
- parallelism: 4
- output: 32 bytes

**操作流程**：

1. **初始化**：wizard 生成 age 密钥对 → 导出公钥给备份任务 → 用户设口令派生 KEK → 加密 age 私钥存 keystore
2. **备份**：仅需公钥加密，无需口令（适合无人值守调度）
3. **恢复**：用户输入口令 → 派生 KEK → 解密 age 私钥（仅存内存）→ 解密密文
4. **轮换**：生成新 age 密钥对 → 后续备份用新公钥（旧私钥仍可解历史档）
5. **恢复（可选 C）**：口令遗忘 → 用离线备份的私钥副本解密

**安全保证**：

- age 私钥永不明文落盘（口令派生 KEK 加密存储，`zeroize` 清零内存）
- 备份侧仅需公钥，私钥本地私藏——目标存储即使泄露也无法解密
- 口令哈希用 Argon2id（抗 GPU/ASIC 暴力破解）
- 公钥可安全公开，甚至随备份元数据一并存储

### 验证步骤

1. 验证 `age` crate 在飞牛设备上流式加密/解密性能
2. 验证 Argon2id 保护私钥的耗时（应 <2s）
3. 测试私钥恢复流程的完整性与边界情况
4. 验证 `zeroize` 在进程崩溃时不残留密钥

---

## 选型总结

| 选型项     | 确定方案                    | 关键理由                                      | Phase |
| ------- | ----------------------- | ----------------------------------------- | ----- |
| 酷族网软对接  | 官方 WebDAV 文件管理（Basic 凭据；逆向 REST API 与登录二进制已移除） | 官方 WebDAV 为稳定标准协议，凭据简单可靠 | 1-2   |
| 飞牛源访问   | 仅本地 FS（tokio::fs）       | 只需本地文件备份，不考虑 SMB/NFS，零依赖                  | 1     |
| 双架构编译   | musl 静态 + cross 工具      | 无运行时依赖，避免 C 交叉编译                          | 1     |
| 源目录授权   | config/resource + 运行时引导 | 飞牛标准机制，最小权限                               | 1     |
| UI 暴露认证 | 端口服务 + JWT              | 简单独立，WebSocket 可用                         | 1     |
| 密钥管理    | age 公私钥 + 口令保护私钥       | 公钥加密/私钥解密，私钥口令保护，符合现代加密标准               | 2     |

### 贯穿性原则

1. **避免 C 依赖**——所有库优先选纯 Rust 实现（rustls、rusqlite bundled、age），使 musl 静态交叉编译透明化
2. **最小权限**——`run-as=package`，目录授权制，不碰 root
3. **不自动化绕过验证码**——登录由编译二进制模拟真人完成，本系统只复用 session token，过期重登
4. **凭据最小化**——WebDAV 专用凭据加密存储，UI 保存前实测连通性；逆向 API 与登录二进制成果已弃用移除
