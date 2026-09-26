# 插件接口契约（稳定 C ABI v1）

> 目标：**主程序升级不需要重新编译插件**。做法是把跨边界的数据从「Rust 类型」换成
> 「版本化的 C ABI + UTF-8 JSON」——宿主内部随便改，只要本文档的契约不变，插件就一直能用。
>
> **内置插件与外置插件走同一份契约**：内置插件（`webdav` 目标、`kzwr` 增强）同样是
> 「编译进主程序的 ABI 插件」，区别在于表是**编译期静态表**而非 `dlopen` 取到的。
> 因此本文档对内置插件同样有约束力。
>
> 代码位置：宿主 `backend/src/plugin/{abi.rs,cabi.rs,loader.rs,target_abi.rs,builtin/}`；
> 插件 SDK `plugins/sdk/`；示例 `plugins/example-hello/`。
>
> - 增强类能力（UI / 动作 / 体检 / 事件）：见 §2 主表
> - **自定义备份目标**（推块传输 / 并发回传）：见 **§9 目标能力表**

## 1. 插件机制

**现行机制只有一种：稳定 C ABI v1**。它同时支撑增强类能力与**自定义备份目标**（§9）。

| | **稳定 C ABI v1**（唯一机制） |
|---|---|
| 插件依赖 | 只依赖 `plugins/sdk`（零第三方依赖） |
| 宿主升级后 | **不用重编插件** |
| 能提供 | 增强类（UI 卡片 / 动作 / 体检 / 事件）+ **自定义备份目标**（§9 目标能力表） |
| 加载入口 | 增强：`fn_kzwr_plugin_abi_v1`；目标：`fn_kzwr_plugin_target_v1` |
| 诊断标记 | `/api/plugins` 里 `mechanism = "c-abi-v1"` |

> ~~**Rust 直连（进阶）**：`fn_kzwr_plugin_abi_version` + `_host_version` + `_create`，
> 传 Rust trait 对象，升级后必须重编。~~
> **已移除（2026-09-26，ADR-013 决策 1）**：Rust 无稳定 ABI，维护两条路径的收益为负。
> 删除面：`plugin/sdk.rs`、`plugins/example-rdirect/`、`export_plugin!` 宏、
> `FN_KZWR_PLUGINS_ALLOW_MISMATCH`。原「只有 Rust 直连能写自定义目标」的限制
> 已由 **§9 目标能力表**解除 —— 稳定 ABI 插件同样可以提供备份目标。

外置插件（`.so`）与内置插件（编译期静态表）共用同一份契约，宿主对二者一视同仁。

## 2. 契约（插件导出）

```c
// 唯一的入口符号
const KzwrPluginAbi *fn_kzwr_plugin_abi_v1(void);

typedef struct KzwrPluginAbi {
    uint32_t abi;          // 必须 == 1（C_ABI_VERSION）
    uint32_t size;         // sizeof(KzwrPluginAbi)
    char *(*describe_json)(void);
    char *(*available_json)(const char *cfg_json);
    char *(*action_json)(const char *action, const char *request_json);
    char *(*health_json)(const char *cfg_json);
    char *(*event_json)(const char *event, const char *cfg_json);   // 可选（可为 NULL）
    void  (*free_str)(char *p);
    void  (*destroy)(void);                                        // 可选（可为 NULL）
} KzwrPluginAbi;
```

结构体**只能尾部追加字段**；宿主按 `size` 判断可选字段是否存在（v1 所有回调均必需，`event_json`/`destroy` 用 `NULL` 表示未实现）。

## 3. 约定（违反会导致拒绝加载或崩溃）

- **字符串**：UTF-8 + NUL 结尾；用 SDK 的 `to_c_string()` 分配
- **所有权**：插件返回的字符串归宿主，宿主必须调用该表的 `free_str` 释放（SDK 里就是 `free_c_string`）
- **空指针**（`NULL` / `nullptr`）表示"无内容"，宿主按空处理，不会崩
- **不要 panic 跨 FFI**：Rust 插件请自行 `catch_unwind`（示例已演示）；宿主另有一层 `catch_unwind` 兜底
- **线程安全**：宿主可能从不同线程调用回调
- 回调应当**快速返回**（动作接口在请求线程里执行；长任务请自行异步化并尽快返回）
- ⚠️ **禁止在回调里 `runtime().block_on()` 启动/驱动新的 async runtime**
  （2026-09-26 血泪坑）：宿主常在 **tokio runtime 线程**上调用同步回调（如 axum handler
  里的 `test_json`）。此时 `block_on` 会 panic
  `Cannot start a runtime from within a runtime`，而 `extern "C"` 帧**不允许 unwind** →
  **整个进程 abort**（表现为"接口返回空 + 进程消失"）。
  正确做法：把阻塞工作派发到**专用线程**（`std::thread::spawn`）再 `block_on`，
  该线程没有 runtime 上下文，可安全阻塞。

## 4. JSON 契约

### 4.1 `describe_json`（启动时调用一次，必须稳定不变）

```json
{
  "id": "example",
  "name": "示例插件",
  "version": "1.0.0",
  "kind": "enhance",
  "description": "一句话说明",
  "caps": { "account": false, "quota": false, "recycle_bin": false, "notify": false },
  "ui": {
    "section": "settings",
    "title": "卡片标题",
    "order": 90,
    "component": null,
    "blocks": [
      { "type": "tips", "text": "说明文字" },
      { "type": "metric", "label": "指标名", "value": "值", "hint": "补充说明" },
      { "type": "button", "label": "按钮", "action": "/hello", "danger": false, "confirm": null },
      { "type": "text", "field": "token", "label": "输入项", "value": null, "placeholder": "提示", "secret": true, "action": "/save", "button": "保存" },
      { "type": "number", "field": "days", "label": "天数", "value": 7, "suffix": "天", "action": "/save", "button": "保存" },
      { "type": "toggle", "field": "enabled", "label": "开关", "value": false, "action": "/toggle" }
    ]
  }
}
```

- `kind`：v1 只接受 `"enhance"`（缺省视为 enhance）
- `ui` 可为 `null`（无界面插件）
- `blocks[].action` 是**相对插件前缀**的路径，建议写成 `/xxx`（宿主也容忍漏写 `/`）；前端把它拼成 `POST /api/p/<插件id>/<action>`
- 未知字段双方都应忽略（向前兼容）

### 4.2 `cfg_json`（宿主 → 插件：配置快照，**不含任何凭据**）

```json
{
  "abi_version": 1,
  "host_version": "0.4.0",
  "timezone": "CST (UTC+08:00)",
  "utc_offset_minutes": 480,
  "targets": [
    { "id": "default", "name": "默认目标（WebDAV）", "kind": "webdav",
      "url": "https://dav.kzwr.com/dav", "enabled": true, "ready": true, "primary": true }
  ],
  "tasks": [
    { "id": "default", "name": "默认任务", "enabled": true, "paths": ["/vol1/…"],
      "target_id": "default", "target_folder": "fn-backup", "schedule_cron": "0 2 * * *" }
  ],
  "enhance": { "kzwr_token_configured": true }
}
```

口令、access-token、账号名**永不外传**（`ready` 表示凭据是否齐备）。

### 4.3 `available_json` / `health_json` / `action_json` / `event_json`

| 回调 | 返回 |
|------|------|
| `available_json` | `{"available": true, "reason": null}` |
| `health_json` | `[{"key":"…","title":"…","status":"ok\|warn\|fail","detail":"…","hint":null}]` |
| `action_json` | 任意 JSON（推荐 `{"success":true,"message":"…"}` 或 `{"success":false,"error":"…"}`）；宿主原样回给前端 |
| `event_json` | 任意 JSON；`after_backup` 可用 `{"count": N}` 表示"处理了 N 项" |

`action_json` 的入参 `request_json`：

```json
{ "body": { "…前端提交的 JSON…" }, "cfg": { "…配置快照…" } }
```

事件名：`startup`（启动）、`patrol`（周期巡检，约 30 分钟）、`after_backup`（备份成功后）、`reload`（配置变更后）。

## 5. 版本演进规则

- **只增不改**：新增可选字段/新 `blocks` 类型 → 宿主与插件互相忽略未知字段，无需改 ABI 版本
- **破坏性改动**（改回调签名、改必需字段语义）→ `abi` 版本 +1（如 v2）；宿主可同时支持 v1/v2，旧插件继续可用
- 插件应把 `abi` 设为编译期常量（SDK 的 `ABI_VERSION`），宿主不匹配时**拒绝加载并说明原因**

## 6. 写一个插件（步骤）

```bash
# 1) 复制示例
cp -r plugins/example-hello plugins/my-plugin

# 2) Cargo.toml：只依赖 SDK
#    [lib] crate-type = ["cdylib"]
#    fn-kzwr-plugin-sdk = { path = "../sdk" }
#    serde_json = "1"

# 3) src/lib.rs：实现 describe/available/action/health 并导出
#    sdk::export_plugin_v1!(describe, available, action, health, Some(event));

# 4) 构建
bash Scripts/build_plugins.sh          # → dist/plugins/libmy_plugin.so

# 5) 安装：放进 $TRIM_PKGETC/plugins/，设置页开启「外置插件加载」后重启应用
```

## 7. 诊断

- `GET /api/plugins` 的 `external` 字段：`enabled` / `dirs`（扫描到的目录与来源）/ `reports[]`（每个 `.so` 的 `loaded`、`mechanism`、`id`、`abi`、`error`）
- 设置页「外置插件（动态库）」卡片直接展示上述结果，并用徽标标出 **稳定 ABI v1**（~~Rust 直连~~ 已移除）
- 日志：`fnos_backup::plugin::loader`（加载/跳过原因）；`fnos_backup::plugin::cabi`（C ABI 接管）
- 相关环境变量：`FN_KZWR_PLUGINS=1`（开启加载）、`FN_KZWR_PLUGIN_DIR`（指定目录）
  （~~`FN_KZWR_PLUGINS_ALLOW_MISMATCH=1`~~ 属已移除的 Rust 直连机制，今已不存在）

## 8. 安全边界

- 加载 `.so` 等价于**执行任意本地代码**：默认关闭，必须由用户在设置页显式开启
- 目前**没有签名校验**（只有 ABI/版本校验）→ 只放可信插件

---

## 9. 目标能力表 `KzwrTargetAbi`（自定义备份目标）

想提供**新的备份目的地**（对象存储 / 另一家网盘 / 自建协议）就实现这张表。
入口符号 `fn_kzwr_plugin_target_v1`；也可在 `describe_json.runtime.target` 里指定别的符号名。

### 9.1 设计前提：插件只碰密文

宿主的备份流水线是「读明文 → age 加密 → **把密文喂给插件**」，明文与密钥**永不离开宿主**，
因此不需要 host vtable、插件也无法解密。读取（恢复）同理：插件给密文，宿主解密写盘。

### 9.2 表结构

```c
typedef struct KzwrTargetAbi {
    uint32_t abi;            /* == 1 */
    uint32_t size;           /* sizeof(KzwrTargetAbi)，尾部追加用 */

    /* —— 实例生命周期 —— */
    void  *(*target_open)(const char *target_json);   /* 含该目标凭据；NULL = 配置无效 */
    void   (*target_close)(void *th);

    /* —— 传输：推块（宿主 → 插件，内容为 age 密文） —— */
    void  *(*write_begin)(void *th, const char *rel_path, uint64_t total);
    int    (*write_chunk)(void *th, void *h, const uint8_t *buf, uint32_t len); /* ≥0 实写；<0 错误码 */
    int64_t(*write_end)(void *th, void *h);           /* 返回本文件**实写**密文字节数 */
    void   (*write_abort)(void *th, void *h);         /* 可选 */

    /* —— 读取：恢复（插件 → 宿主，内容为 age 密文） —— */
    void  *(*read_begin)(void *th, const char *rel_path);
    int    (*read_chunk)(void *th, void *h, uint8_t *buf, uint32_t cap); /* >0 字节；0 = EOF；<0 错误 */
    int    (*read_end)(void *th, void *h);            /* 可选 */

    /* —— 目录与元数据 —— */
    char  *(*list_json)(void *th, const char *prefix); /* [{"rel_path","size","mtime_secs","is_dir"}] */
    int    (*delete)(void *th, const char *path);
    int    (*ensure_dir)(void *th, const char *path);  /* 可选 */
    int    (*ping)(void *th);                          /* 可选 */
    char  *(*test_json)(const char *target_json);      /* 可选：设置页「测试连接」 */

    /* —— 插件自管配置（宿主代加密，命名空间 = 插件 id） —— */
    char  *(*config_get)(const char *key);             /* 可选 */
    int    (*config_set)(const char *key, const char *value); /* 可选 */

    char  *(*last_error_json)(void *th);               /* 可选：错误详情 */

    /* —— 并发回传（可选能力，见 9.4） —— */
    void  *(*plan_begin)(void *th, const char *job_json);
    char  *(*plan_next)(void *th, void *ph);           /* ["目标端路径", …]；[] = 清单已空 */
    void   (*plan_end)(void *th, void *ph);

    void  (*free_str)(char *p);
} KzwrTargetAbi;
```

### 9.3 运行契约

| 事项 | 约定 |
|---|---|
| 实例句柄 `th` | 多目标/多任务下同一插件可有多实例，每个实例一套凭据与连接 |
| 传输句柄 `h` | 一次传输的上下文；`write_end` / `write_abort` / `read_end` 之后即失效 |
| 错误码 | `int < 0` = 失败，宿主随后调 `last_error_json` 取详情；`-2` 映射为认证错误 |
| **字节复查** | `write_end` 返回**实写密文字节数**；宿主与已喂出的字节数比对，不一致 → 该文件不落快照、任务标记失败（防静默截断） |
| 看门狗 | 宿主侧检测进度停滞（默认 120s）→ 判失败并告警。单次阻塞 FFI **无法强制中断**（同进程模型固有限制） |
| 快照粒度 | 按文件：每个 `write_end` 成功即 upsert 该文件的快照（真断点续传） |
| 路径口径 | **`rel_path` 一律是目标端路径**（含目标前缀）。多源备份时源内相对路径会重名，只有目标端路径唯一 |

### 9.4 并发回传（可选能力）

不给 `plan_*` 也能正常工作 —— 宿主会按清单**顺序推块**。声明支持后，
「传哪些、一次传几批」的决策权交给插件（目标端最清楚自己的限速与分片能力）。

在 `describe_json.target` 声明：

```json
"target": { "supports_plan": true, "max_parallel": 4, "preferred_chunk_kib": 1024 }
```

流程：

```
宿主: plan_begin(th, job_json)        // 提交待传清单
循环: batch = plan_next(th, ph)       // 取下一批；[] = 发完
      宿主编内并发上传该批（并发度 = max_parallel）
结束: plan_end(th, ph)                // 宿主在会话 Drop 时必定调用
```

`job_json`：

```json
{"upload":[{"rel_path":"/fn-backup/src-a/f1.bin","size":40000,"mtime_secs":1790435784}, …]}
```

要点：

- `plan_next` 返回的是**目标端路径数组**，必须与 `job_json.upload[].rel_path` 同口径
- 宿主会校验返回的路径**确实属于本次待传清单**（防越权写入）；不在清单内的会被忽略
- **是否启用由用户按插件配置**：设置项 `plugins.target_parallel[插件 id]`
  （缺省沿用插件声明；`0`/`1` = 关闭；`≥2` = 启用，上限 8），接口
  `POST /api/plugins/:id/parallel`，保存后热重建、无需重启
- 插件本身不支持时（未声明 `supports_plan`），即便用户配了并发度也不会启用

### 9.5 内置 WebDAV 目标即本契约的参考实现

`backend/src/plugin/builtin/webdav_abi.rs` 是完整的参考实现：编译期静态表 +
内部复用 `WebdavTarget` 做协议/分片/重试，对外只暴露推块接口；并实现了 `plan_*`
（每批 12 个）。写新目标插件时可直接照抄结构。

---

## 10. 诊断（目标插件）

- `GET /api/plugins`：每个插件带 `supports_plan`（能力声明）与 `parallel`（该插件的并发度配置）
- `POST /api/plugins/:id/parallel`：`{"parallel": N}` → 设置该插件的上传并发路数
- 日志：`fnos_backup::plugin::target_abi`（推块/复查/看门狗）、`fnos_backup::domain::backup`（是否走并发回传）
- 插件与宿主同进程、同权限运行；若不需要这一点，请勿启用
