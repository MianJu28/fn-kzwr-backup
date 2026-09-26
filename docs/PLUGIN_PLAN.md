# 插件方案（评审后定稿 · 待实现）

> 状态：**已按 2026-09-26 评审决策定稿，尚未实现**。实现完成后并入 `docs/PLUGIN_ABI.md`。
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
  "ui": { "section": "settings", "title": "…", "order": 20, "component": "kzwr", "blocks": [ /* … */ ] }
}
```

**动作路由**：宿主把 `/api/p/<id>/*action` 的**剩余路径**（去掉前导 `/`，可含 `/`）作为动作名，
因此既有 `/api/p/kzwr/trash/empty` **无需改名、前端零改动**。

**告警声明式回传**（避免插件回调宿主）：`event_json` / `health_json` 返回值扩展为
`{"count":N, "alerts":[{"level":"warn","message":"…","dedup_key":"kzwr.quota"}]}`，
宿主负责去重与落告警（等价于现有 `raise_alert_once` 语义）。

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

- 文件约定：`<plugin>.so` + `<plugin>.so.sig`（**Ed25519 签名，覆盖 .so 的 SHA-256**，hex 编码）
- 公钥来源：配置 `plugins.pubkeys: [hex…]`（支持多个，便于官方公钥 + 用户自签）+ 可选编译期内置官方公钥
- 加载顺序：**先验签，通过才 dlopen**；失败 → `loaded=false, error="签名校验失败"`，不影响其它插件与核心
- 逃生阀：`FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1`（仅本地开发）
- 依赖：**零新增下载** —— `ring`（Ed25519 verify）与 `sha2`（哈希）已在 `Cargo.lock` 中，
  只需提升为直接依赖（`ed25519-dalek` 不在 lock 内，不引入）
- 工具：`Scripts/sign_plugin.sh`（openssl / ssh-keygen 生成密钥与签名）+ 发布流程写入文档

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

### 5.2 kzwr（增强插件）—— 依赖宿主能力的替代方案

| 现在依赖 | 行号 | ABI 化替代 |
|---|---|---|
| `state.kzwr`（`KzwrClient`） | `kzwr.rs:107-108,128-129,210-211,239-240,345-346,370-371,675-676,734-735,800,824-825,904,907,930` | 插件**自建** client（token 从 `config_get` 读）；因是内置插件，仍可 `use` 宿主内部库 |
| `state.config` 读写 token / 阈值 / 保留策略 | `kzwr.rs:233,271-277,669,801,831` | `config_get/config_set`（命名空间 `kzwr`）；保留策略相关改由宿主传入 `cfg_json.tasks[]` |
| `state.audit.record(...)` | `kzwr.rs:700,705,806,834,932,952` | 宿主统一记审计（`plugin.<id>.<action>`），插件不感知 |
| `raise_alert_once` / `state.alerts` | `kzwr.rs:132,216,283,349,374,711,739,866,910` | 改为**声明式回传**（§3.1 `alerts[]`，带 `dedup_key`），宿主落告警 |
| `human_bytes` | `kzwr.rs:172-173,291-292,937` | 插件内部自带（或 SDK 提供） |
| `webdav_username(state)` | `kzwr.rs:208,829` | 待定：`cfg_json.targets[].username`（见 §10 第 1 条） |
| `routes()`（axum Router，`core.nest`） | `kzwr.rs:48-53`；挂载 `routes.rs:3486` | 改为 3 个动作：`user` / `token` / `trash/empty`（多段动作名，见 §3.1） |
| `health_check` 返回 `Vec<CheckOutcome>` | `kzwr.rs:113-197` | 改为 `health_json` 返回同形 JSON（宿主已有转换层 `cabi.rs:198-238`） |

> 迁移后 `kzwr.rs` 内的类型（`KzwrUserResponse` 等）保留，只把入口换成 `extern "C"` 回调 + JSON 收发。

---

## 6. 插件自管数据（决策 2）

- 落点：`AppConfig` 新增 `plugin_data: BTreeMap<String, BTreeMap<String, String>>`（外层键 = 插件 id），
  值用现有 `ConfigManager::encrypt_field`（`enc:` 前缀）加密存储，复用同一口令派生。
- 读取：`config_get(key)` → 解密后返回明文给插件；`config_set(key, value)` → 加密落盘。
- **卸载清除**：新增 `POST /api/plugins/:id/purge`（二次确认）→ 调用插件 `destroy`（若有）→ 删除
  该 id 的 `plugin_data` 命名空间 → 记审计。若仍有目标（`TargetConfig.kind`）或任务引用该插件 →
  **拒绝卸载**并列出引用项（与 `target_delete` 的保护语义一致）。
- **孤立数据检测**：启动时比对"有 `plugin_data` 但没有对应已加载插件"的 id → 在设置页提示"清理遗留配置"。
- **导入导出**：`ConfigBundle`（`routes.rs:429-469`）新增 `plugin_data` 字段，
  `config_export`（`:3191-3209`）/ `config_import`（`:3246-3364`）同步，否则换机会丢插件配置。

---

## 7. 前端改动

| 位置 | 改动 |
|---|---|
| `components/PluginSection.svelte:148-156` | 机制徽标（`c-abi-v1` / `rust-direct`）→ 改为**签名状态**徽标（`已签名/未签名/验签失败`）+ 卸载按钮 |
| `lib/plugins.js` | 无语义改动（`ui` / `api_base` 驱动不变）；`BUILTIN_COMPONENTS` 保留 |
| `lib/api.js:109-113` | kzwr 三个方法**URL 不变**（多段动作名兼容）；可后续统一收敛到 `pluginGet/pluginPost` |
| `views/SettingsPage.svelte:77-99` | 内置组件（`webdav`/`kzwr`）与 `PluginBlocks` 回退逻辑保留 |

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

---

## 9. 实施顺序与估时

| # | 内容 | 估时 |
|---|---|---|
| 1 | ABI 整改（`size` 语义、删 kind 硬拒、`runtime` 发现、多段动作名、声明式 alerts）+ 删 Rust 直连（§8） | 0.5 天 |
| 2 | 目标能力表 + 实例句柄 + `AbiTargetStorage`（`spawn_blocking`、进度换算、错误码映射、字节复查、看门狗、`plan_*` 并发） | 1.2 天 |
| 3 | 签名校验（`ring`+`sha2`、`.sig` 约定、公钥配置、`Scripts/sign_plugin.sh`） | 0.5 天 |
| 4 | 插件自管数据（`plugin_data` + `config_get/set` + 卸载清除 + 孤立检测 + 导入导出） | 0.4 天 |
| 5 | 内置插件 ABI 化：webdav（目标表）+ kzwr（动作/体检/事件/自管配置/告警） | 1.0 天 |
| 6 | 前端（签名徽标、卸载按钮）+ SDK `export_target_v1!` + 示范插件（本地目录） | 0.5 天 |
| 7 | 文档合并（`PLUGIN_ABI.md`）+ ADR 补充 | 0.3 天 |
| | **合计** | **~4.5 天**（含真机端到端） |

核心收益不变：`AbiTargetStorage` 只是"函数指针版"的 `TargetStorage`，
**备份流水线（扫描 / 差分 / 加密 / 快照 / 保留 / 恢复编排）结构不动**。

---

## 10. 剩余待定（3 条，均为小决策；我给出的默认可直接采用）

1. **`cfg_json` 是否包含目标用户名**：kzwr 插件需要展示"当前绑定账号"（现用 `webdav_username(state)`）。
   默认建议：**只给 username，不给密码**（目标管理页本来就回显用户名）。
2. **卸载与引用冲突**：插件仍被目标/任务引用时，默认**拒绝卸载**（列出引用项）。
3. **签名是否强制**：默认**强制**（未签名即拒载），仅 `FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1` 放行本地开发。

---

## 11. 风险

- **静默数据损坏**：插件 bug 可能少传/截断 → 靠决策 7（字节复查）+ 保留策略 `list` 兜底，仍非 100% 防护。
- **卡死不可中断**：同进程 .so 无法强杀，看门狗只能"判失败 + 泄漏线程"（决策 6 的固有局限）。
- **实现重复**：每个目标插件各自实现分片/重试/进度（这是"下放传输"的固有代价，换来宿主不背协议包袱）。
- **内置插件 ABI 化的复杂度**：kzwr 从"直接用 `AppState`"改为"JSON + 命名空间配置"，需要把告警/审计/请求客户端
  改为声明式或自建；这是本次最大的改造面（§5.2）。
