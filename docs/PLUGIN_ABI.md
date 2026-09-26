# 外置插件接口契约（稳定 C ABI v1）

> 目标：**主程序升级不需要重新编译插件**。做法是把跨边界的数据从「Rust 类型」换成
> 「版本化的 C ABI + UTF-8 JSON」——宿主内部随便改，只要本文档的契约不变，插件就一直能用。
>
> 代码位置：宿主 `backend/src/plugin/{abi.rs,cabi.rs,loader.rs}`；插件 SDK `plugins/sdk/`；示例 `plugins/example-hello/`。

## 1. 两种插件机制（先选一种）

| | **稳定 C ABI v1**（推荐） | Rust 直连（进阶） |
|---|---|---|
| 插件依赖 | 只依赖 `plugins/sdk`（零第三方依赖） | 依赖宿主 crate `fn-kzwr-backup` |
| 宿主升级后 | **不用重编插件** | **必须重编**（Rust 无稳定 ABI，宿主会校验编译期版本） |
| 能提供 | 增强类：UI 卡片 / 动作接口 / 一键体检项 / 生命周期事件 | 全部，包括**自定义备份目标**（`TargetPlugin`）、自有 axum 路由、读 `AppState` |
| 加载入口 | `fn_kzwr_plugin_abi_v1` | `fn_kzwr_plugin_abi_version` + `fn_kzwr_plugin_host_version` + `fn_kzwr_plugin_create` |
| 诊断标记 | `/api/plugins` 里 `mechanism = "c-abi-v1"` | `mechanism = "rust-direct"` |

宿主加载顺序：**先找稳定入口**，没有才回退 Rust 直连。两者可同时存在于一个目录。

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
- 设置页「外置插件（动态库）」卡片直接展示上述结果，并会用徽标标出 **稳定 ABI v1** 或 **Rust 直连**
- 日志：`fnos_backup::plugin::loader`（加载/跳过原因）；`fnos_backup::plugin::cabi`（C ABI 接管）
- 相关环境变量：`FN_KZWR_PLUGINS=1`（开启加载）、`FN_KZWR_PLUGIN_DIR`（指定目录）、`FN_KZWR_PLUGINS_ALLOW_MISMATCH=1`（**仅 Rust 直连**用：强制忽略宿主版本校验）

## 8. 安全边界

- 加载 `.so` 等价于**执行任意本地代码**：默认关闭，必须由用户在设置页显式开启
- 目前**没有签名校验**（只有 ABI/版本校验）→ 只放可信插件
- 插件与宿主同进程、同权限运行；若不需要这一点，请勿启用
