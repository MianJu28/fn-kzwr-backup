# 插件方案（评审后定稿 · 分批实施中）

> 状态：**已按 2026-09-26 评审决策定稿**。实施进度见 §9「实施顺序」：
>
> - ✅ **Step 1**（ABI 整改 + 删 Rust 直连）、**Step 2**（目标能力表 + `AbiTargetStorage` 适配器）、
>   **Step 5-webdav**（内置 webdav 目标 ABI 化，方案 C）、**并发回传**（`plan_*` 接线 +
>   每插件独立开关）均已完成并通过 NAS 端到端实测；契约已并入 `docs/PLUGIN_ABI.md`。
> - ✅ **Step 3**（签名校验 + 内置官方公钥 + 随包插件默认签名）、
>   **Step 4**（插件自管数据：`plugin_data` + `/api/plugins/:id/data` + `/purge` + 孤立检测）、
>   **Step 6**（前端签名徽标 / 卸载按钮 / SDK `export_target_v1!` / `example-localfs` 示范插件）
>   均已完成（2026-09-27）。
> - ✅ **Step 5-kzwr**（增强插件 ABI 化，见 §5.2）**已完成**（2026-09-28，v0.4.5）：
>   kzwr 的功能与配置**全部移出核心**，宿主不再内置任何增强插件；`plugins/kzwr/` 外置 .so
>   随包分发并默认签名。剩余项均非改造（见 §9）。
>
> 前置事实：产品未发布、无历史插件 → **直接修改 v1 契约本身**，不做 v1/v2 并存。
>
> 相关代码：`backend/src/plugin/{abi,cabi,loader,registry,api}.rs`、`plugins/sdk/`、
> `backend/src/infra/{config,storage_trait}.rs`、`backend/src/domain/backup.rs`、
> `docs/ARCHITECTURE.md`（ADR-013 / ADR-014）。

## 评审决策记录（2026-09-26）

| # | 决策 |
|---|---|
| 1 | **不保留 Rust 直连路径**；内置插件（webdav / kzwr）**也用 ABI 重写** |
| 2 | 插件自管配置由**宿主代加密存储**；**卸载插件时清除其配置** |
| 8 | （2026-09-28）kzwr 迁移**不搬运旧配置**：用户重新填写 access-token，换取核心彻底解耦 |
| 9 | （2026-09-28）**完全删除**宿主 kzwr（含 `enhance.kzwr_token_configured`），不留「还在但没人读」的东西 |
| 10 | （2026-09-28）**去掉账号一致性检查**（备份目标与酷族账号可以不是同一账号） |
| 11 | （2026-09-28）告警走**方案 A**：返回值 `alerts[]` → 宿主 `raise_alert_once`，**通用能力**，非 kzwr 特例 |
| 12 | （2026-09-28）多账号用**通用 `UiBlock::Accounts` + 通用前端渲染器**；阈值**按账号**存储，默认 90% |
| 13 | （2026-09-28）**主表不再冻结**：新增能力直接**尾部追加**到 `KzwrPluginAbi`，用 `MIN_SIZE` 必需前缀 + `size` 逐字段探测保证「老插件短表仍能加载」；`C_ABI_VERSION` 保持 1（未发布，无兼容包袱） |
| 14 | （2026-09-28）需要中途日志/进度/定时/同步读配置的插件走**宿主能力表**：主表尾部可选回调 `host_bind`，宿主下发 `KzwrHostAbi` + **按插件限定的 `ctx`**；**声明式通道保留且始终可用**，两条通道共用同一套汇点与去重规则 |
| 15 | （2026-09-28）能力表**写操作一律入队**（单消费任务落库，满则丢弃并计数）、**仅 `config_get` 同步**（迫使宿主「不跨 FFI 持锁」，ctx 自带只读镜像 + 待落盘覆盖层）；`ctx` **有意泄漏**且禁用/卸载即 `revoke` |
| 4 | 并发为**可选项**；开启并发后**由插件回传**决定传哪些（pull 调度） |
| 6 | 加**心跳**（卡死检测） |
| 7 | 插件**上报实写字节**，宿主复查 |
| 9 | 加**签名校验** |
---

## 0. 一句话

宿主保留「**决策 + 密钥**」，插件负责「**执行 + 传输**」；插件**只接触密文**。
**所有插件（含内置）统一通过同一份 ABI 契约被宿主消费**，契约只增不改。

---

## 1. 不变式

1. **密钥与明文只存在于核心**：插件拿到的一律是 age 密文；插件无法解密，也无法介入加密流程。
2. **SQLite 快照是唯一真相源**：哪个文件成功上传由宿主判定并落库 → 决定下次增量、恢复页、占用统计。
3. **单插件失败不得影响核心与其它插件**（崩溃 / 卡死 / 符号缺失 / 验签失败均隔离为该插件的加载或运行错误）。
4. **契约只增不改**：新增字段尾部追加、按 `size` 探测；破坏性改动才升 `abi`。
5. **内置与外置同契约**：内置插件不得直接持有 `AppState`、不得实现仅供内部使用的 trait；
   它是"同进程内、用宿主内部库实现的 ABI 插件"（见 §5）。

---

## 2. 插件形态（定稿：只剩两种）

| 形态 | 入口 | 说明 |
|---|---|---|
| 内置 ABI 插件 | 静态 `KzwrPluginAbi` 表（`extern "C"` 回调，编译进主程序） | 实现内部可自由 `use` 宿主内部库（如 `WebdavTarget`、`KzwrClient`），但**对外只暴露 ABI** |
| 外置 ABI 插件 | `fn_kzwr_plugin_abi_v1` → `*const KzwrPluginAbi`（dlopen） | 只依赖 `plugins/sdk`；需通过**签名校验** |

- 能力：`kind: "enhance" | "target" | 两者兼有`
- 能力表由独立入口符号承载（§3.2），主表不随能力膨胀
- **取消**：`Loaded::RustTarget / RustEnhance`、`PluginHandle`、`export_plugin!`、
  `fn_kzwr_plugin_create / _abi_version / _host_version`、宿主版本闸、`FN_KZWR_PLUGINS_ALLOW_MISMATCH`
  （删除清单见 §8）

---

## 3. ABI 形态

### 3.1 主表（增强 + 生命周期）

```c
typedef struct KzwrPluginAbi {
    uint32_t abi;          /* = 1（未发布，直接改本版契约） */
    uint32_t size;         /* sizeof(KzwrPluginAbi) */
    char *(*describe_json)(void);
    char *(*available_json)(const char *cfg_json);
    char *(*action_json)(const char *action, const char *request_json);
    char *(*health_json)(const char *cfg_json);
    char *(*event_json)(const char *event, const char *cfg_json);  /* 可 NULL */
    void  (*free_str)(char *);
    void  (*destroy)(void);                                        /* 可 NULL，卸载前调用 */
} KzwrPluginAbi;
```

三处改动：

1. **`size` 语义修正**：现 `plugin/cabi.rs:61` 为 `size >= sizeof(整表)`（= "插件表不得短于宿主"，方向反了，
   宿主加字段即拒载所有插件）。改为：接受门槛 `size >= REQUIRED_PREFIX`（必需字段前缀长度常量），
   尾部字段逐项探测 `offset + sizeof <= size`，缺失视为 NULL。不使用 `std::mem::offset_of!`（无 MSRV 需求）。
2. **删除 `plugin/cabi.rs:76-83` 的 `kind != "enhance"` 硬拒**。
3. `describe_json` 增加 `runtime` 段（能力发现，Vulkan/EGL 风格）：

```json
{
  "id": "kzwr", "kind": "enhance", "version": "1.0.0",
  "runtime": { "target": "fn_kzwr_plugin_target_v1" },
  "target": { "write": "push", "supports_plan": true, "max_parallel": 4, "preferred_chunk_kib": 1024 },
  "ui": { "section": "settings", "title": "…", "order": 20, "blocks": [ /* … */ ] }
}
```

**动作路由**：宿主把 `/api/p/<id>/*action` 的**剩余路径**（去掉前导 `/`，可含 `/`）作为动作名，
因此既有 `/api/p/kzwr/trash/empty` **无需改名、前端零改动**。

**告警声明式回传**（避免插件回调宿主）：`event_json` / `health_json` 返回值扩展为
`{"count":N, "alerts":[{"level":"warn","message":"…"}]}`，
宿主负责去重与落告警（等价于现有 `raise_alert_once` 语义）。

> 实现修正：原计划里的 `dedup_key` 字段**没有做**——去重直接按「告警来源 + 消息全文」
> 精确匹配（`AlertSource::Plugin(id)`），比另起一个键更简单且无第二事实来源。
> 代价：消息里若要区分账号，就得把账号名写进消息（kzwr 的配额告警即如此），
> 恢复时按 `resolve:[前缀]` 前缀匹配消解。**最终形态以 `PLUGIN_ABI.md` §4.4 为准。**

### 3.2 目标能力表（独立入口符号 → 独立演进）

```c
typedef struct KzwrTargetAbi {
    uint32_t abi; uint32_t size;

    /* —— 实例生命周期 —— */
    void  *(*target_open)(const char *target_json);   /* 见 §3.3，含该目标凭据；NULL = 配置无效 */
    void   (*target_close)(void *th);

    /* —— 传输：推块（宿主 → 插件，内容为 age 密文） —— */
    void  *(*write_begin)(void *th, const char *job_json, const char *rel_path, uint64_t plain_len);
    int    (*write_chunk)(void *th, void *h, const uint8_t *buf, uint32_t len); /* >=0 实写；<0 错误码 */
    int    (*write_end)(void *th, void *h);           /* 返回：本文件实写密文字节数 */
    void   (*write_abort)(void *th, void *h);

    /* —— 读取：恢复（插件 → 宿主，内容为 age 密文） —— */
    void  *(*read_begin)(void *th, const char *job_json, const char *rel_path);
    int    (*read_chunk)(void *th, void *h, uint8_t *buf, uint32_t cap); /* >0 字节；0 EOF；<0 错误 */
    int    (*read_end)(void *th, void *h);

    /* —— 并发回传（可选能力，supports_plan=true 时宿主改用） —— */
    void  *(*plan_begin)(void *th, const char *job_json);   /* job_json.upload = 待传清单 */
    char  *(*plan_next)(void *th, void *ph);                /* ["rel_path", …]；[] = 清单已空 */
    void   (*plan_end)(void *th, void *ph);

    /* —— 目录与元数据（JSON） —— */
    char  *(*list_json)(void *th, const char *prefix);  /* [{"rel_path","size","mtime_secs","is_dir"}] */
    int    (*delete)(void *th, const char *path);
    int    (*ensure_dir)(void *th, const char *path);
    int    (*ping)(void *th);
    char  *(*test_json)(const char *target_json);       /* 设置页「测试连接」（实例尚未建立时） */

    /* —— 插件自管配置（宿主代加密，命名空间 = 插件 id） —— */
    char  *(*config_get)(const char *key);
    int    (*config_set)(const char *key, const char *value);

    char  *(*last_error_json)(void *th);                /* 错误详情（避免每块回传 JSON） */
} KzwrTargetAbi;
```

设计要点：

- **推块而非拉块**：宿主读明文 → 加密 → 喂密文。插件**不回调宿主**，因此不需要 host vtable。
- **目标实例句柄 `th`**：多目标/多任务下同一插件可有多实例，每个实例一套凭据与连接（由 `target_open` 创建）。
- **错误码契约**：`int < 0` 为失败，随后 `last_error_json` 取详情；宿主映射到
  `StorageError{Io, NotFound, PermissionDenied, Protocol, Auth, Other}`，保留重试与告警分级。
- **`handle` 必须支持同一 job 内多路并发**（上限由 `max_parallel` 决定）。

### 3.3 两级配置传入（关键区分）

| 输入 | 内容 | 是否含凭据 |
|---|---|---|
| `cfg_json`（全局快照） | host_version、时区、targets（id/name/kind/url/**username**/enabled/ready/primary）、tasks、enhance 状态 | **不含密码/token**（沿用现有约定） |
| `target_json`（实例级） | 该目标的 id/name/kind/enabled/url/username/password + 该插件命名空间下的全部自管键值 | **含**（仅传给该目标的 `target_open`/`test_json`，不落日志） |

> 这是"插件自管配置"与"宿主通用凭据字段"的衔接点：宿主仍保留 `TargetConfig{url, username, password}`
> 通用三字段（目标管理页表单与"是否就绪"判定用），插件特有字段（bucket/region/私钥）放命名空间。

### 3.4 签名校验（决策 9）

> **实现状态：已完成**（`backend/src/plugin/loader.rs` + `Scripts/sign_plugin.sh`）。
> 下文的算法/编码以**实现为准**修正过一次：初版设计写的是「对 .so 的 SHA-256 摘要签名、hex 公钥」，
> 落地时简化为**直接对 .so 原始字节签名、base64 公钥**（少一层哈希，openssl 与 ring 直接互通）。

- 文件约定：`<plugin>.so` + `<plugin>.so.sig`
  —— **Ed25519 签名，直接覆盖 `.so` 的原始字节**，`.sig` 为 **64 字节裸签名**
- 公钥编码：`plugins.pubkeys` 里是 **base64 的 32 字节裸 Ed25519 公钥**（不是 hex、不是 PEM）
- 公钥来源：**内置官方公钥（编译期常量 `loader::OFFICIAL_PUBKEYS`）∪ 配置 `plugins.pubkeys`**
  - 取**并集**、任一验签通过即放行：随包插件靠内置公钥**开箱即用**（用户零配置），
    用户自签插件只需把自己的公钥填进设置页
  - 内置公钥只证明「由官方私钥签发」，**不等于**官方审计过插件代码；用户目录里的插件仍需自带有效签名
- 加载顺序：**先验签，通过才 dlopen**（有测试钉住：验签失败的插件绝不触发 `dlopen`）；
  失败 → `loaded=false, error="签名校验未通过：…"`，不影响其它插件与核心
- **默认强制**：`pubkeys` 为空也照样拒绝未签名/验签失败的插件
  （否则「忘记配公钥」会静默退化成无校验）
- 逃生阀：`FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1`（仅本地开发；生产 `cmd/main` 用 `env -i` 白名单启动，
  该变量不会被透传，因此无法在正式安装里误开）
- 依赖：**零新增下载** —— 只用已在依赖树里的 `ring`（Ed25519 verify）与 `base64`；
  `ed25519-dalek` **未引入**
- 工具链：`Scripts/sign_plugin.sh`（`keygen|sign|verify|pubkey`，基于 `openssl pkeyutl -sign -rawin`）
  + `Scripts/build_plugins.sh` **默认签名**（私钥 `Scripts/keys/sign.key` 存在即启用），
  并核对私钥与内置官方公钥是否配对（不配对直接报错，避免打出「全部加载失败」的包）
- 私钥保管：`Scripts/keys/sign.key` 由 `.gitignore`（`*.key`）排除，**只存在于发布机**；
  仓库里只有**公钥**（内置锚点）

---

### 3.4.1 随包插件「默认签名」的信任锚（实现补充）

**问题**：§3.4 的「默认强制验签」与「随包示例插件」直接冲突 ——
出厂状态下 `plugins.pubkeys` 是空的，用户一开启插件加载，**随包插件会全部被拒绝**，
而现象只是「插件列表里什么都没有」，用户无从判断是没签名、没公钥、还是文件没放对。

**方案**：宿主内置**官方发布公钥**作为信任锚，`build_plugins.sh` 默认用官方私钥签名。

| 环节 | 做法 |
|---|---|
| 信任锚 | `loader::OFFICIAL_PUBKEYS`（编译期常量，base64 裸 Ed25519 公钥）。**已验证**：空 `pubkeys` 下随包插件 `signature=verified`、`loaded=true` |
| 私钥 | `Scripts/keys/sign.key`，**不入库**（`.gitignore` 的 `*.key` 命中）。仓库里只有公钥 |
| 签名时机 | `Scripts/build_plugins.sh` 默认签名（私钥存在即启用）；`SKIP_SIGN=1` 显式跳过；`build_fnos_app.sh` 打包后会检查 `app/plugins/*.so` 是否都有 `.sig` 并告警 |
| 漂移守卫 | 构建前核对「私钥公钥 ↔ `OFFICIAL_PUBKEYS`」是否配对，不配对**直接报错**（否则会打出插件全失效的包，且难定位）。第三方自建可设 `ALLOW_KEY_MISMATCH=1` 绕开，把公钥交给用户填进设置页 |
| 用户自签 | 公钥填 `plugins.pubkeys`；与内置公钥**取并集**，任一验签通过即放行 |

**为什么不放「公钥文件」到 `$TRIM_PKGETC/plugins/` 旁边**：该目录在 `install_init` 里被
`chown -R` 给应用用户，**应用用户可写** → 把锚点放在那里等于没有锚点
（能落 `.so` 的攻击者也能顺手改掉公钥文件）。公钥必须编译进二进制才可信。

**边界声明**：内置公钥只证明「由官方私钥签发」，**不等于官方审计过插件代码**。
用户目录（`$TRIM_PKGETC/plugins`）里的插件仍必须自带有效签名才加载 —— 这正是签名闸门
要防的「本地落一个 .so 就被执行」。

### 3.4.2 已知限制

- ~~**`plugin_data` 不随配置导入导出**~~：**已实现**（此前文档误标为未落地，实际在
  Step 5-webdav 提交中已加入 `ConfigBundle.plugin_data`）。
  导出：`config_export` 遍历 `plugin_data_ids` 逐个 `plugin_data_export`（明文导出）；
  导入：用**当前口令重新加密**后写回（未携带则不覆盖）。
  ⚠️ 注意导出的是**明文**：换机迁移时口令保护强度取决于导出文件本身的保密性。
- **插件启用状态与公钥改动需重启应用**：`load_external` 只在启动时调用一次（无热重载）。
- **ARM 交叉编译未经真机验证**：CI 的 `aarch64-unknown-linux-gnu` 腿已按标准交叉编译配置写好
  （`gcc-aarch64-linux-gnu` + `CARGO_TARGET_*_LINKER`/`CC_*`），但手头没有 ARM 设备可验证。
- ~~**CI 不构建插件**~~：**已修复（2026-09-27）**。CI 改为统一调用 `Scripts/build_fnos_app.sh`
  （单一事实来源，修掉了手工组装时写错的 `cd bin/fn-kzwr-backup-app` 路径），
  并从 `secrets.PLUGIN_SIGN_KEY_B64` 注入私钥、构建并签名插件、打包后自检
  「平台字段 / 动态链接 / 插件与 `.sig` 数量一致」。未配置私钥时：手动触发 → 告警且不含插件；
  tag 发布 → 直接失败（避免发出版本里随包插件凭空消失）。

### 3.4.3 关键约束：与 musl 静态链接互斥（2026-09-27 实测）

外置插件（ADR-013）要求宿主**必须**是 glibc 动态链接，两条原因：

1. **musl 目标不支持 `cdylib`**（本机实测）：对 `plugins/example-hello` 执行
   `cargo build --target x86_64-unknown-linux-musl` 直接报
   `the target ... does not support these crate types` —— 插件根本编译不出来。
2. **静态链接的二进制没有动态装载器**：`dlopen` 不可用，`libloading` 必然失败，
   即便插件用 gnu 目标编出来也加载不了。
   （注：本条为已知机制，**未在本机实测**——`static.rust-lang.org` 在此网络不可达、装不上 musl std；
   第 1 条已足以否掉该组合。）

因此 2026-09-27 把发布链接方式从 musl 静态改为 **glibc 动态**：

| 项 | 原 | 现 |
|---|---|---|
| 后端目标 | `x86_64-unknown-linux-musl` | `x86_64-unknown-linux-gnu` |
| 插件目标 | 不可能（cdylib 不支持） | 与后端同一 `TARGET_TRIPLE` |
| `build_fnos_app.sh` 默认 | `MUSL_TARGET=1` | glibc（`MUSL_TARGET=0`） |

- 兼容性佐证：线上已安装的 v0.3.9 本就是动态链接 glibc（`U dlopen@GLIBC_2.34`），
  该路径已在真机长期运行。
- 代价与缓解：需目标机 glibc ≥ 构建机。在较旧的构建镜像里编译即可
  （当前线上二进制只要求 GLIBC ≤ 2.34，NAS 是 2.36）。
- 脚本会阻止危险组合：`MUSL_TARGET=1` 且未设 `SKIP_PLUGINS=1` → **直接报错**，
  避免打出「宿主静态、插件全废」的包。要纯静态包就得显式放弃插件。

---

## 4. 目标插件运行契约

| 事项 | 约定 |
|---|---|
| 谁读源文件 | 宿主（扫描 + 加密），插件不接触明文 |
| 谁决定传哪些 | 宿主（SQLite 差分）；**开启并发时改由插件通过 `plan_next` 回传** |
| 谁做分片 / 重试 / 协议适配 | **插件**（宿主按 `max_parallel`、`preferred_chunk_kib` 驱动） |
| 并发（决策 4） | 默认**串行**（宿主按清单顺序）；插件声明 `supports_plan=true` 且用户开启并发 → 宿主循环 `plan_next`，对返回路径并行开 handle（≤ `max_parallel`），传完再问下一批 |
| 安全校验 | 宿主必须校验 `plan_next`/`write_begin` 的 `rel_path` **属于本 job 的待传清单**（防越权写入） |
| 快照粒度 | **升级为按文件**：每个 `write_end` 成功即 upsert 该文件快照（比现状"按源路径"更细 → 真断点续传） |
| 字节复查（决策 7） | 宿主统计已喂出密文字节数，与 `write_end` 回报的实写字节比对；**不一致 → 告警 + 该文件不落快照 + 任务标记失败**；必要时用 `list_json` 复核 |
| 心跳（决策 6） | 宿主侧**看门狗**：记录每次 FFI 调用开始/返回时间，进度停滞超过阈值（默认 120s，可配置）→ 告警 + 审计 + 任务失败，并标注"插件卡死" |
| 超时/取消限制 | 单次阻塞 FFI 调用**无法强制中断**（同进程 .so）。看门狗只能"停止等待并判失败"，会泄漏一个阻塞线程 —— 如实写入文档；若将来需要真隔离，另立"子进程插件模型"需求 |
| 保留策略 / 孤儿清理 | 宿主（依赖 `list_json`） |
| 恢复 | 宿主编排：密文来自 `read_*`，宿主解密后写盘 |
| 后续工作（清回收站、空间巡检） | 已是插件钩子：`event_json(after_backup / patrol)`，结果用 §3.1 的声明式 `alerts` 回传 |

> 附带修正：`domain/backup.rs:177` 注释写"每上传完一个文件即时保存其快照"，实际 `save_snapshot` 只在
> **每个源路径整段上传完之后**调用一次（`backup.rs:283-287`）。本次改造后按文件 upsert，注释同步改正。

---

## 5. 内置插件 ABI 化迁移清单（决策 1）

### 5.1 webdav（目标插件）

| 现在 | 改后 |
|---|---|
| 实现 `TargetPlugin`（`build`/`verify`/`ui`） | 提供 `KzwrTargetAbi` 静态表：`target_open`/`write_*`/`read_*`/`list_json`/… |
| `build(TargetConfig, ConfigManager)` 内部 `resolve()` 读配置 + `TRIM_DAV_*` 环境变量 | 凭据从 `target_json` 取（宿主传入）；`TRIM_DAV_*` 仅对 `default` 目标生效（保留调试能力） |
| 直接返回 `Arc<dyn TargetStorage>`（`WebdavTarget`） | 内部仍复用 `WebdavTarget`，但对外只暴露推块接口（`write_inner` 的流改为"宿主喂块"驱动） |

### 5.2 kzwr（增强插件）—— **已完成**（v0.4.5，2026-09-28）

目标（用户原话）：「将之前 kzwr 遗留的功能和配置完全迁移到插件当中，不和宿主混在一起」。
**宿主侧 `builtin/kzwr.rs` 与 `infra/kzwr_api/` 已整体删除**，核心不再引用任何酷族专属
概念（`registry.rs` 有一条不变式测试钉住这一点）。迁移后全部代码在 `plugins/kzwr/`。

原表里的「依赖宿主能力」逐条落地结果：

| 原依赖 | 最终替代（已实现） |
|---|---|
| `state.kzwr`（`KzwrClient`） | 插件自带 `src/api.rs`（reqwest + `access-token` 头），`src/rt.rs` 里的 `OnceLock` runtime 承载异步；宿主把每个增强回调放进 `spawn_blocking`，故插件可安全 `block_on`（详见 `PLUGIN_ABI.md` §3） |
| `state.config` 读写 token / 阈值 | 读：`cfg.self_config`（宿主只注入**本插件**命名空间）；写：返回值里的 `config:{set,remove}` 声明式回写 → `plugin_data["kzwr"]`（age 加密） |
| `state.audit.record(...)` | 返回值 `audit:[{action,detail,ok}]` → 宿主落 `state.audit`；`plugin.data`（回写键名，不含值）由宿主自动补记 |
| `raise_alert_once` / `state.alerts` | 返回值 `alerts:[{level,message}]` → 宿主 `raise_alert_once`（按 来源+消息 精确去重）；`resolve:[前缀]` → 条件恢复后消解该插件的旧告警 |
| `human_bytes` | 插件内部自带 |
| `webdav_username(state)`（账号一致性检查） | **已去掉**（用户决策）：备份目标与酷族账号不必是同一账号，跨账号检查是错的 |
| `routes()`（axum Router） | 动作名沿用原路径 → `/api/p/kzwr/*` 前端 URL **零改动**；`accounts`、`accounts/{add,update,remove,percent}`、`user`、`space`、`quota`、`trash`、`trash/empty` |
| `health_check` → `Vec<CheckOutcome>` | `health_json` 返回 `{checks:[{key,title,status,detail,hint}],alerts:[…],resolve:[…]}`（裸数组也兼容）；体检键 `kzwr.<账号id>` 让前端「前往处理」自动路由到插件页 |

**四项关键决策（均为用户确认）**：

1. **旧配置不迁移** —— 宿主 `enhance.kzwr_token_configured` 等一并删除，用户升级后需在
   「插件」页**重新填写** access-token（一次性成本，换核心彻底解耦）。
2. **完全删除**核心 kzwr 代码，不留「还在但没人读」的配置段（否则导出包会持续携带敏感明文）。
3. **去掉账号一致性检查**（见上表）。
4. **告警走通用声明式通道**（`alerts`/`resolve`），任何插件都能用，不为 kzwr 开特例。

**新增的通用能力（不属于 kzwr，属于框架）**：

- `UiBlock::Accounts` —— 多凭据账号列表的**声明式**界面区块；前端通用渲染器
  `PluginAccounts.svelte` **零插件专属逻辑**。契约见 `PLUGIN_ABI.md` §4.5。
  这样「一个插件需要多账号」不再要求改前端。
- 声明式副作用通道 `alerts` / `resolve` / `config` / `audit`（§4.4），主表保持**冻结**。
- 契约回归测试 `backend/src/plugin/contract_tests.rs`：**直接读各插件的 `describe.json`
  用真实宿主类型反序列化**，并遍历 `plugins/*/describe.json` —— 新增插件自动纳入校验，
  漂移即编译期失败。

**迁移中踩到并已修正的实现细节**（都有测试钉住）：

- 配置键名不允许点号 → 按账号阈值用 `percent-<id>`；且宿主对一批键是
  **整批生效或整批拒绝**，一个坏键会让「阈值改了不生效」且插件自认为成功。
- `0 = 关闭预警` 必须**显式写 0**，不能表达成「删键」（删键 = 未单独设置 = 回落到全局值，
  表现为设 0 后立刻弹回、且下轮巡检继续报警）。
- 越界阈值（>100）**报错**而不是夹到 100（夹住会让提示文案与实际存储矛盾）。
- `/accounts` 回显的 `default_percent` 用**生效值**而不是编译期常量。

> 遗留：`[kzwr]` 段在旧 `config.toml` 里仍可能存在，由 `infra/config.rs` 的
> `legacy_kzwr_section_is_ignored` 钉住「必须能载入且其余各段不丢」——这是升级即服务
> 启动的前提。

---

## 6. 插件自管数据（决策 2）—— **已实现**

- 落点：`AppConfig` 新增 `plugin_data: BTreeMap<String, BTreeMap<String, String>>`（外层键 = 插件 id），
  值用现有 `ConfigManager::encrypt_field`（`enc:` 前缀）加密存储，复用同一口令派生。
- **读写通道（实现口径）**：`KzwrTargetAbi` 上的 `config_get` / `config_set` 两个函数指针**仍是预留位**
  （SDK 导出为 `None`，宿主未接回调）。实际通道是**宿主代存 + 配置注入**：
  - 前端：`describe_json.ui.blocks[]` 里给 `text`/`number` 块标 **`scope: "host"`** →
    `PluginBlocks.svelte` 改投 `POST /api/plugins/<id>/data`
  - 写入：`plugin_data_set`（`{"fields": {...}, "remove": [...]}`）→ 加密落盘 → 记审计 `plugin.data`
  - 读取：装配目标/任务实例时按命名空间解密 → 注入 `target_json.config`（插件读 `config.<键>`）；
    前端回显走 `GET /api/plugins/<id>/data`，值**脱敏**（只答 `redacted`/是否已设置）
  - 改完调 `state.reload_targets(&cfg)`，无需重启应用
- **卸载清除**：`POST /api/plugins/:id/purge`（二次确认）→ 调用插件 `destroy`（若有）→ 删除
  该 id 的 `plugin_data` 命名空间 → 记审计。若仍有目标（`TargetConfig.kind`）或任务引用该插件 →
  **拒绝卸载**并列出引用项（`目标「…」` / `任务「…」`，与 `target_delete` 的保护语义一致）。
- **孤立数据检测**：启动时比对"有 `plugin_data` 但没有对应已加载插件"的 id → `orphan_data[]`
  （`GET /api/plugins`）→ 设置页提示"清理遗留配置"。
- **导入导出**：**已实现**（此前文档误标为未落地）。`ConfigBundle` 含 `plugin_data` 字段；
  导出时以**明文**写出（`plugin_data_export`），导入时用**当前口令重新加密**写回，
  未携带则不覆盖。⚠️ 导出的明文意味着导出文件需自行保密。

---

## 7. 前端改动 —— **已实现**

| 位置 | 改动 | 状态 |
|---|---|---|
| `components/PluginSection.svelte` | 机制徽标（`c-abi-v1` / `rust-direct`）→ **签名状态**徽标（`已签名/未签名/验签失败`）+ 公钥编辑框 + 卸载按钮 + 孤立数据提示 | ✅ |
| `components/PluginBlocks.svelte` | 新增 `scope: "host"` 分支：该字段不投插件 `action`，改投 `POST /api/plugins/<id>/data`；打开时回显（值已脱敏 → 占位「已设置（留空则保持不变…）」） | ✅ |
| `lib/api.js` | 新增 `pluginData` / `pluginDataSet` / `pluginPurge`；kzwr 三个方法 URL 不变（多段动作名兼容） | ✅ |

### 7.1 前端页面重构（2026-09-27，插件化收尾）

插件化改动很大，前端仍留着一整套**插件化前**的遗留形态，故做了一次结构性重构：

| 变更 | 说明 |
|---|---|
| **新增「插件」页**（`views/PluginsPage.svelte`） | 插件卡片区（通用 UI Schema）+ 外置插件管理（开关/目录/公钥/诊断/卸载）。插件不再混在设置页 |
| **设置页精简** | 只留核心项：age 密钥 / 通知 / 配置迁移（+ 日志页的调试开关） |
| **删除「备份」页** | `BackupPage`/`BackupConfigSection`/`BackupSection`：编辑的全局 `backup_paths`/定时在后端**只写第一个任务**（`cfg.tasks.first_mut()`），多任务下语义误导；任务页已完整覆盖 |
| **删除 `WebdavSection`** | 凭据改在「目标」页按**多目标**管理（ADR-014），设置页那份会绕过多目标模型 |
| **删除 `KzwrSection`** | kzwr 改为**插件 schema 渲染**；为补回原组件独有的能力，后端新增 `/quota`（空间预警阈值）与 `/space`（动态指标）两条插件路由 |
| **删除 `RetentionSection`** | 保留策略是**任务级**配置（备份流水线读 `ctx.task.retention`），设置页那份全局配置实际不生效 → 统一在任务页编辑 |
| **删除 `RailAccount`** | 侧栏常驻账号卡（依赖可选 kzwr token）；账号信息在插件卡片内已有 |
| **`OverviewSection` 并入 `DashboardPage`** | 概览改为**按任务/目标聚合统计**（任务数/就绪数/目标数/云端文件/下次触发），不再展示遗留全局字段 |
| **顶栏「立即备份」移除** | `run_backup_now` 只跑 `default_task_id`，语义含糊；任务页已有**每任务**「立即备份」 |
| **`component` 分支彻底移除** | 内置 webdav/kzwr 的 UI 也改为完整 `blocks` 描述（后端 `component: None`）→ 前端不再有插件专用手写组件，`BUILTIN_COMPONENTS` 删除 |
| **动态 `metric`** | `metric` 块支持 `action`：前端渲染时 `GET` 该路径取实时值（用于云端空间用量），失败则保留静态文案 |

净减约 2000 行（19 文件，+441/−2461）。删除的组件均已在重构前确认无引用。

### 7.7 kzwr 完全外置：功能与配置都不再混在宿主里（2026-09-28，v0.4.5）

| 反馈 / 目标 | 处理 |
|---|---|
| 「将之前 kzwr 遗留的功能和配置完全迁移到插件当中，不和宿主混在一起」 | 删除 `plugin/builtin/kzwr.rs`、`infra/kzwr_api/`（整个模块）、`AlertSource::Kzwr`、`AppState.kzwr`、`enhance.kzwr_token_configured`；能力全部落在 `plugins/kzwr/`（外置 .so，随包分发并默认签名） |
| 一个插件需要管多个凭据，却要改前端 | 新增 `UiBlock::Accounts` + 通用渲染器 `PluginAccounts.svelte`（**零插件专属逻辑**）：列表 / 新增 / 编辑 / 删除 / 每项一个数值调参，全部由插件 `describe.json` 声明路径与字段 |
| 插件想发告警、想持久化自己的配置、想留审计，却没有宿主回调 | 新增**声明式副作用通道**（主表保持冻结）：返回值 `alerts[]` / `resolve[]` / `config{set,remove}` / `audit[]`，由 `cabi::apply_side_effects` 按固定顺序落地。任何插件可用 |
| 契约漂移（文档写的类型和代码不一致、新增插件前端不认） | 新增 `backend/src/plugin/contract_tests.rs`：用**真实宿主类型**反序列化各插件 `describe.json`，并遍历 `plugins/*/describe.json` 自动纳入；写回解析/键名规则/健康两种形状都抽成可测函数并覆盖 |
| 体检「前往处理」只认几个写死的 key | `SetupCheckSection.pageOf()` 改为通用规则：带点的 key 一律路由到插件页；`AuditSection.actionText()` 对 `kzwr.*` 这类插件动作给通用中文标签；`MessagesPanel` 认 `{"plugin":"<id>"}` 形态的告警来源 |
| 旧单 token 配置 | **不迁移**（用户决策）：升级后需在「插件」页重新填写。旧 `[kzwr]` 段仍能被载入（`legacy_kzwr_section_is_ignored` 钉住「载入成功且其余各段不丢失」），下次保存自然消失 |
| 多账号实时用量在卡片上看不到（迁移后一度只剩静态文案） | 插件卡片按声明的 `metric.action` 通用拉取实时值（与设置弹窗同一契约），任何插件声明即可用；`accounts` 块计入「N 项设置」 |

**迁移过程中修掉的真实缺陷**（均由测试钉住，见 §5.2 末尾）：阈值 `0`（关闭预警）
被表达成删键 → 设 0 后弹回全局值并继续报警；越界阈值被静默夹到 100 却回显原值；
`/accounts` 的 `default_percent` 回显编译期常量而非生效值。

### 7.6 外置插件热加载 + 交互收敛（2026-09-27，v0.4.4）

| 反馈 | 处理 |
|---|---|
| 「启用外置插件还要重启吗」 | **改为热加载，无需重启**。注册表把外置插件整体装入 `RwLock<Option<ExternalSet>>`（含配套的动态库句柄），可整组替换；新增 `POST /api/plugins/reload`，并在保存开关、安装、卸载后**自动**触发 |
| 安装用弹窗 + 不输入文件名 | 新增 `PluginInstallModal.svelte`：只需选 `.so` 与 `.so.sig`、填该插件公钥；文件名直接用所选 `.so` 原名，不提供输入框 |
| 扫描目录怎么有两个 | 物理上仍需扫两处（用户安装目录 + 随应用分发目录，后者放随包插件），但**界面只展示一个**「插件目录」（新增 `plugin_dir_label()`）。内部 `dirs` 仅用于诊断 |
| 启用用按钮不要复选框 | 开关改为「启用 / 停用」按钮，放在独立的 `.switch-bar` 中（语义是动作而非勾选项） |
| target/enhance 不统一 | 插件列表行统一用 `.row-item`（自带边框+背景），并按类型给左侧色条（备份目标=品牌色、增强能力=信息色），卡片侧也有图标+文字区分 |

**热加载的内存安全边界（重要）**

外置插件的 vtable 由宿主按 `&'static` 持有，且飞行中的备份可能持有其派生的
`Arc<dyn TargetStorage>`。因此：
- **卸载 ≠ 立即释放内存**：整组替换时，旧 `ExternalSet` 靠 `Arc` 引用计数托管 ——
  若此刻有备份在用它，对应动态库会存活到本次备份结束，不会出现悬空指针/崩溃；
- 这也正是「禁用」（逻辑摘除）与「卸载」（删文件）分开的原因，UI 文案已如实说明。

### 7.5 外置插件：界面安装 + 一插件一公钥（2026-09-27，v0.4.3）

按使用反馈重做外置插件区的交互，并顺带修掉一个**越权信任**问题。

| 变更 | 说明 |
|---|---|
| **开关折叠** | 未启用「外置插件加载」时，下方安装/管理/诊断全部折叠，只留开关与一句说明 |
| **去掉目录配置** | 插件目录固定用默认位置（`$TRIM_PKGETC/plugins`、`$TRIM_APPDEST/plugins`）。少一个配置项就少一类「填错就静默不加载」的故障 |
| **界面安装** | 新增「安装插件」：选 `.so` + `.so.sig`，填该插件公钥 → 后端**先验签再落盘**（失败绝不写盘），原子写入并登记绑定 |
| **界面卸载** | 每个外置插件行加「卸载插件」：删 `.so`/`.sig` + 解绑公钥；内置/随包插件不在用户目录，删不掉 |
| **一插件一公钥** | 见下（安全修复） |

**安全修复：从「公钥池」改为「一插件一公钥」**

原先 `plugins.pubkeys: Vec<String>` 是扁平列表，验签时对**所有**插件依次尝试全部公钥
（「任一匹配即通过」）。这意味着**插件 A 的公钥能验过插件 B**：一旦 A 的私钥泄露，
攻击者用它签出的恶意插件 B 同样会被接受 —— 签名隔离形同虚设。

现改为 `plugins.plugin_pubkeys: BTreeMap<文件名, 公钥>`，验签**只查该文件自己的公钥**
（外加内置官方公钥）。用文件名而非插件 id 作键，是因为验签发生在 `dlopen` 之前、
拿不到插件自报的 id。旧字段保留为**兼容回退**，仅在 `plugin_pubkeys` 整体为空时生效 ——
否则「某插件登记了公钥」会让其它未登记插件继续被公钥池放行，隔离性又失效。

有一条测试专门钉住这个边界（`per_plugin_key_cannot_verify_other_plugin`）；
写它的时候还**抓到了我第一版的实现 bug**：当时未登记文件会回退到扁平列表，
隔离在「部分登记」场景下并不成立。

### 7.4 `echo` 示范与 ABI 补强（2026-09-27，v0.4.3）

给 `plugins/example-hello/` 补了**可照抄的 `echo` 示范**：`/greeting` 一个路径同时服务
GET（读，供回显）与 POST（写，保存），并在 `ui.blocks` 里声明 `echo: "/greeting"`。

写这个示范时发现**我自己的契约有缺口**：`action_json` 的信封原先只有 `body` + `cfg`，
外置插件**无法区分 GET 与 POST** —— 也就是说 §7.3 新增的 `echo` 契约对外置插件
**根本无法实现**。已补入 `method` 字段（`"GET"` / `"POST"`），示范即按它分流。

顺带修掉一个脚本缺陷：`build_plugins.sh` 里的 `PLUGIN_TARGET` 在**直接调用**该脚本时
（不经 `build_fnos_app.sh`）未定义，裸引用会因 `set -u` 报 `unbound variable` 而中断构建。

### 7.3 体验修正（2026-09-27，v0.4.2）

| 项 | 处理 |
|---|---|
| **插件设置不回显**（功能缺陷） | 根因：`blocks[].value` 只是描述里的**静态默认值**，插件并不知道自己持久化过什么（阈值恒显示 85），而后端也只有 POST 没有可读接口。新增 `UiBlock::{Text,Number,Toggle}.echo` 契约：前端渲染时 GET 该路径取真实值。kzwr 补 `/quota`、`/token` 的 GET（密钥只回 `configured`，绝不回明文） |
| **并发设置位置不当** | 上传并发原在插件页的插件卡片里，但它是「这个目标用几条连接」的属性，与地址/凭据同类。移到**「目标」页**每个目标下方（按目标类型取 `supports_plan`/`parallel`，值仍按插件配置、同类型目标共用）；`PluginBlocks` 里的并发设置与相关状态一并移除 |
| **插件页观感** | 卡片改为自适应网格布局：顶部渐变强调条 + 图标（按 kind 区分目标/增强）+ 状态胶囊（运行中/待配置/已停用，带呼吸点）+ 指标网格块（等宽数字）+ 底部操作条（设置项计数 + 设置/启停）。停用态降饱和；窄屏单列 |

### 7.2 运行时启停与插件设置弹窗（2026-09-27）

插件改动很大后，前端仍要求「改开关/公钥 → 重启应用」才生效，且插件设置与其它配置
混在一页。本次补齐运行时启停与独立设置弹窗。

| 能力 | 实现 |
|---|---|
| **按插件禁用** | 新增 `plugins.disabled: Vec<String>`；`PluginRegistry` 用 `RwLock<HashSet>` 持有（注册表是 `Arc` 共享，启停接口只有 `&self`），所有查询接口过滤被禁用者 |
| **运行时生效** | 路由从「启动时逐插件 `nest`」改为**请求时按 id 分发**：`/p/:plugin_id/*action` → 查注册表 → `tower::ServiceExt::oneshot` 执行该插件的 `routes()`。这样禁用立即 404、启用立即恢复，新加载插件也无需重启（`tower` 的 `util` feature 已在依赖里，零新增） |
| **停用≠卸载** | 插件 vtable 被宿主按 `&'static` 持有，飞行中备份也可能持有其派生的 `Arc<dyn TargetStorage>`；卸载会让引用悬空 → 崩溃。故停用只做**逻辑摘除**，句柄保活到进程结束（UI 文案如实说明「真正释放需重启」） |
| **级联保护** | 停用仍被任务引用的目标插件 → **级联停用**那些任务并在响应回报 `affected_tasks`；**正在执行备份的任务**（新增 `AppState.running_task_id` 追踪）使用该插件时 → **拒绝**本次停用，提示等备份结束 |
| **设置弹窗** | 新增 `PluginSettingsModal.svelte`：点插件卡片「设置」打开，内容由插件自己的 `ui.blocks` 渲染（`PluginBlocks` 新增 `embedded` 形态，去掉卡片外壳）。卡片上只留只读概览（`metric`/`tips`） |

**顺带修复的既有缺陷**：
- `TargetPool::is_ready()` 原先只看 key 是否存在，但未配置的目标也会放进池中（占位适配器）
  → 未配凭据的目标被错误显示为「已就绪」。现单独记录 `ready` 集合，语义正确。
- 请求时分发需要清理外层路由写入的 `request extensions`：axum 的 `Path` 从
  extensions 读 `UrlParams`，外层已写 `:plugin_id` + `*action` 两个参数，
  不清理会让插件侧 `Path<String>` 看到 3 个参数并报
  「Wrong number of path arguments for `Path`. Expected 1 but got 3」（实测踩到）。

---

## 8. 删除清单（决策 1 的清理面）

- `backend/src/plugin/sdk.rs`（整个模块）+ `plugin/mod.rs` 的 `pub mod sdk;`
- `plugin/loader.rs`：`Loaded::RustTarget / RustEnhance`、`load_rust_direct`、sdk import、`mechanism` 字段
- `plugin/registry.rs`：`external_libs` 保活逻辑保留（dlopen 仍需），但 Rust 直连分支删除
- `plugins/example-rdirect/`（整个 crate）
- `Scripts/build_plugins.sh` 中关于"必须重编"的注释
- `FN_KZWR_PLUGINS_ALLOW_MISMATCH` 相关分支
- 文档：`docs/ARCHITECTURE.md:476-477,505,508`、`docs/PLUGIN_ABI.md:15,154`
- 保留：`plugins/sdk/`、`plugins/example-hello/`（稳定 ABI 示例）

### 8.1 Step 5-kzwr 的删除面（v0.4.5）

- `backend/src/plugin/builtin/kzwr.rs`（整个模块，约 1000 行）+ `builtin/mod.rs` 的注册
- `backend/src/infra/kzwr_api/`（整个模块：client / constants / models）
- `AppState.kzwr`（`KzwrClient` 实例）与 `AlertSource::Kzwr`
- `AppConfig.kzwr`（`KzwrConfig{access_token_enc, quota_warn_percent}`）与其默认值函数
- `enhance.kzwr_token_configured`（体检项）与前端 `KzwrSection`（Step 5 前已删，本次删净其后端）
- 前端硬编码的 `'kzwr'` 来源标签、`PAGE_OF.kzwr/quota`、`AuditSection` 里 4 条 `kzwr.*` 标签、
  `lib/plugins.js` 的 kzwr 兜底卡片项
- 不变式测试：`registry.rs` 断言**核心不内置任何增强插件**（`builtin` 只剩 webdav 目标）
- 保留：`plugins/kzwr/`（迁移后的全部实现）、`[kzwr]` 旧段的**载入容错**（不是保留功能，
  是不让老配置炸掉服务）

---

## 9. 实施顺序与估时

| # | 内容 | 估时 | 状态 |
|---|---|---|---|
| 1 | ABI 整改（`size` 语义、删 kind 硬拒、`runtime` 发现、多段动作名、声明式 alerts）+ 删 Rust 直连（§8） | 0.5 天 | ✅ 已完成 |
| 2 | 目标能力表 + 实例句柄 + `AbiTargetStorage`（`spawn_blocking`、进度换算、错误码映射、字节复查、看门狗、`plan_*` 并发） | 1.2 天 | ✅ 已完成 |
| 3 | 签名校验（`ring` + `.sig` 约定 + 公钥配置 + 内置官方公钥 + `Scripts/sign_plugin.sh`） | 0.5 天 | ✅ 已完成（算法按实现修正：对 .so 原始字节签名、base64 公钥，见 §3.4） |
| 4 | 插件自管数据（`plugin_data` + `plugin_data_json` + 卸载清除 + 孤立检测 + 导入导出） | 0.4 天 | ✅ 已完成（`/api/plugins/:id/data`、`/purge`，前端设置页可编辑） |
| 5 | 内置插件 ABI 化：webdav（目标表） | 0.4 天 | ✅ 已完成（方案 C：静态表 + `WebdavAbiPlugin` 组合 `CApiTarget`） |
| 5b | kzwr **完全外置**（动作/体检/事件/自管配置/告警） | 0.6 天 | ✅ 已完成（2026-09-28，v0.4.5；比原计划更进一步——不是「内置的 ABI 版」而是**移出核心成为外置 .so**，见 §5.2 / §7.7 / §8.1） |
| 6 | 前端（签名徽标、卸载按钮）+ SDK `export_target_v1!` + 示范插件（本地目录） | 0.5 天 | ✅ 已完成（签名徽标含「已签名/未签名/验签失败」三态；`plugins/example-localfs` 示范目标插件） |
| 7 | 文档合并（`PLUGIN_ABI.md`）+ ADR 补充 | 0.3 天 | ✅ 已完成（§9 目标能力表 + §10 诊断） |
| | **合计** | **~4.5 天**（含真机端到端） | 改造项 1–7 **全部完成**；剩余为真机回归与 §11 的运维项 |

> 5b 的落地形态与原计划不同：原写「内置插件 ABI 化」，实际按用户要求做成**外置插件**
> （核心不含 kzwr 一行代码）。原表 §5.2 里「因是内置插件，仍可 `use` 宿主内部库」这一
> 前提已不成立，也不再需要——插件自带 HTTP 客户端与字节格式化。

### 已完成部分的实测结论（2026-09-26，NAS）

| 项 | 结果 |
|---|---|
| 内置 webdav 走 ABI 推块桥 | 插件注册正常；目标保存（ABI `test_json`）成功 |
| 备份 | 多文件、跨 256KiB 分块；字节数与源文件一致 |
| 恢复 | 源文件移走后恢复，`diff -r` 逐字节一致 |
| 并发回传 | 设某插件并发 3 → 日志出现并发回传；设回 0 → 回到顺序；不支持的插件拒绝设置 |
| 稳定性 | panic 计数 0 |

### 实施中踩到的坑（已写入 `docs/PLUGIN_ABI.md` §3）

1. **同步回调内禁止 `block_on` 新 runtime** → `extern "C"` 不可 unwind → **进程 abort**。
   必须派发到专用线程。
2. 回调签名必须与契约的**原始类型**一致（`write_chunk`/`read_chunk` 的长度是 `uint32_t`，
   写成 `c_int` 会「expected fn pointer, found fn item」）。
3. `PlanSession` 必须 `Send`，否则备份 future 失去 `Send`，axum `Handler` 与 `tokio::spawn`
   连锁失败（报错指向 routes/调度器，根因却在插件层）。
4. 上传项结构体必须**自有数据**，借用版会触发 HRTB（`FnOnce is not general enough`）。
5. 并发度只能在 `build()` 读配置 —— 插件实例在启动时装配，那时配置尚未加载。

核心收益不变：`AbiTargetStorage` 只是"函数指针版"的 `TargetStorage`，
**备份流水线（扫描 / 差分 / 加密 / 快照 / 保留 / 恢复编排）结构不动**。

---

## 10. 剩余待定（3 条，均为小决策；**已全部采纳下文默认值并落地**）

> 状态：**已定案**（2026-09-27）。三条默认值均已在代码中实现，并在真机/本地实例上验证：
> 1. `cfg_json`/`target_json` 含 `username`、**绝不含密码**；
> 2. 插件被目标/任务引用时 `/api/plugins/:id/purge` **拒绝卸载**，并列出引用项（`目标「…」`/`任务「…」`）；
> 3. 签名**默认强制**（空 `pubkeys` 也拒绝），唯一逃生阀是 `FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1`。

1. **`cfg_json` 是否包含目标用户名**：kzwr 插件需要展示"当前绑定账号"（现用 `webdav_username(state)`）。
   定案：**只给 username，不给密码**（目标管理页本来就回显用户名）。
2. **卸载与引用冲突**：插件仍被目标/任务引用时，**拒绝卸载**（列出引用项）。
3. **签名是否强制**：**强制**（未签名即拒载），仅 `FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1` 放行本地开发。
   配套：随包插件由 `Scripts/build_plugins.sh` **默认用官方私钥签名**，
   宿主内置官方公钥（`loader::OFFICIAL_PUBKEYS`）作为信任锚 → 用户零配置即可加载；
   发布流程会核对「签名私钥 ↔ 内置公钥」配对（不配对直接报错，避免打出插件全失效的包）。

---

## 11. 风险

- **静默数据损坏**：插件 bug 可能少传/截断 → 靠决策 7（字节复查）+ 保留策略 `list` 兜底，仍非 100% 防护。
- **卡死不可中断**：同进程 .so 无法强杀，看门狗只能"判失败 + 泄漏线程"（决策 6 的固有局限）。
- **实现重复**：每个目标插件各自实现分片/重试/进度（这是"下放传输"的固有代价，换来宿主不背协议包袱）。
- **内置插件 ABI 化的复杂度**：kzwr 从"直接用 `AppState`"改为"JSON + 命名空间配置"，需要把告警/审计/请求客户端
  改为声明式或自建；这是本次最大的改造面（§5.2）。
