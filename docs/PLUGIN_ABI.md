# 插件接口契约（稳定 C ABI v1）

> 目标：**主程序升级不需要重新编译插件**。做法是把跨边界的数据从「Rust 类型」换成
> 「版本化的 C ABI + UTF-8 JSON」——宿主内部随便改，只要本文档的契约不变，插件就一直能用。
>
> **内置插件与外置插件走同一份契约**：内置插件（当前只剩 `webdav` **目标**）同样是
> 「编译进主程序的 ABI 插件」，区别在于表是**编译期静态表**而非 `dlopen` 取到的。
> 因此本文档对内置插件同样有约束力。
>
> **增强类插件一律外置**：核心不掺任何厂商专属逻辑（v0.4.5 起 `kzwr` 也已从核心删除，
> 外置为 `plugins/kzwr/`）。它是本文档各条约定的**参考实现**，多凭据界面见 §4.5，
> 声明式副作用见 §4.4。
>
> 代码位置：宿主 `backend/src/plugin/{abi.rs,cabi.rs,loader.rs,target_abi.rs,builtin/}`；
> 插件 SDK `plugins/sdk/`；示例 `plugins/example-hello/`；kzwr `plugins/kzwr/`。
> 契约回归测试：`backend/src/plugin/contract_tests.rs`（**直接读各插件的 `describe.json`
> 用真实宿主类型反序列化**，新增插件自动纳入校验）。
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
    char *(*event_json)(const char *event, const char *cfg_json);   // 槽位必需，值可为 NULL
    void  (*free_str)(char *p);
    void  (*destroy)(void);                                        // 可选（可为 NULL）
    int   (*host_bind)(const KzwrHostAbi *host, void *ctx);         // 可选（可为 NULL）；见 §4.6
} KzwrPluginAbi;
```

结构体**只能尾部追加字段**；宿主按 `size` 判断尾部字段是否存在。
**必需前缀**只到 `free_str` 为止（`KzwrPluginAbi::MIN_SIZE`）；`destroy` 与 `host_bind` 在它之后，
是纯尾部可选字段 —— 因此**老插件表短一截仍能加载**，只是拿不到新能力。
`event_json` 的**槽位**在必需前缀内（一直都有），但它的**值**可以是 `NULL` 表示未实现。

## 3. 约定（违反会导致拒绝加载或崩溃）

- **字符串**：UTF-8 + NUL 结尾；用 SDK 的 `to_c_string()` 分配
- **所有权**：插件返回的字符串归宿主，宿主必须调用该表的 `free_str` 释放（SDK 里就是 `free_c_string`）
- **空指针**（`NULL` / `nullptr`）表示"无内容"，宿主按空处理，不会崩
- **不要 panic 跨 FFI**：Rust 插件请自行 `catch_unwind`（示例已演示）；宿主另有一层 `catch_unwind` 兜底
- **线程安全**：宿主可能从不同线程调用回调
- 回调应当**快速返回**；耗时的工作请自行控制超时（宿主动作路由是**一请求一次调用**，
  插件不返回则前端一直转）。

#### 线程与 async（`block_on` 的正确姿势）

`extern "C"` 帧**不允许 unwind**：panic 跨过它 = 整个进程 abort（症状是「接口返回空 +
进程消失」）。而在 **tokio runtime 线程**上 `block_on` 会 panic
`Cannot start a runtime from within a runtime` —— 这正是最容易踩的组合。

- **增强插件（`KzwrPluginAbi`）：宿主已把每个回调放进 `tokio::task::spawn_blocking`**
  （`cabi.rs` 的 `call1`/`call2`：`available` / `action` / `health` / `event` / `reload` /
  `on_startup` / `patrol` / `after_backup` 全部覆盖）。`spawn_blocking` 线程**不是** async
  上下文，因此插件**可以**在自己的 runtime 上 `block_on` 发 HTTP 请求 —— 这是增强插件
  唯一的等待方式（没有宿主回调可以借用）。要点：
  1. runtime 用 `OnceLock` 之类的**全局单例**，别每次调用新建（线程数会爆）；
  2. 仍然自己 `catch_unwind`（见 §8），宿主另有一层兜底，但**别依赖它**：跨 ABI 的
     `extern "C"` unwind 在 release 优化下不保证还能被兜住；
  3. 不要在 `block_on` 里跑无上限的循环，宿主的 blocking 池是有容量上限的；
  4. 想写日志/报进度/留审计不必"先返回"——用 §4.6 的能力表即可（`host_bind` 之后随时可调，
     且那些入口都是**入队**，不会阻塞插件的 runtime）。
- **目标插件（`KzwrTargetAbi`）：仍然可能在 runtime 线程上被调用** —— 只有分块收发
  （`write_stream`/`read_stream`）与 `list`/`delete`/`ensure_dir`/`ping` 走了
  `spawn_blocking`；**`test_json`（目标页「测试连接」）与 `target_open`（装配目标时）是
  直接在 async 线程上调的** —— 2026-09-26 那次 abort 就是 `test_json` 里 `block_on` 引发的。
  所以目标插件**禁止**在回调里 `block_on`；要阻塞就派发到自己的**专用线程**
  （`std::thread::spawn` 后再 `block_on`，该线程没有 runtime 上下文）。

> 两类插件规则不同是**历史事实**：增强插件可以 `block_on`，是因为宿主把增强表的每个回调
> 都放进了 `spawn_blocking`（目标表没有这层改造）。新增**目标**插件时不要照抄 kzwr 的
> `block_on` 写法。

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
    "title": "卡片标题",
    "blocks": [
      { "type": "tips", "text": "说明文字" },
      { "type": "metric", "label": "指标名", "value": "值", "hint": "补充说明" },
      { "type": "button", "label": "按钮", "action": "/hello", "danger": false, "confirm": null },
      { "type": "text", "field": "token", "label": "输入项", "value": null, "placeholder": "提示", "secret": true, "action": "/save", "button": "保存" },
      { "type": "number", "field": "days", "label": "天数", "value": 7, "suffix": "天", "action": "/save", "button": "保存" },
      { "type": "toggle", "field": "enabled", "label": "开关", "value": false, "action": "/toggle" },
      { "type": "accounts", "label": "账号", "list": "/accounts", "add": "/accounts/add",
        "update": "/accounts/update", "remove": "/accounts/remove",
        "credential_field": "token", "credential_label": "访问令牌", "multiple": true,
        "edit_action": "/accounts/percent", "edit_field": "percent", "edit_label": "预警阈值",
        "edit_suffix": "%", "edit_min": 0, "edit_max": 100 }
    ]
  }
}
```

- `kind`：v1 只接受 `"enhance"`（缺省视为 enhance）
- `ui` 可为 `null`（无界面插件）
- `blocks[].action` 是**相对插件前缀**的路径，建议写成 `/xxx`（宿主也容忍漏写 `/`）；前端把它拼成 `POST /api/p/<插件id>/<action>`
- 未知字段双方都应忽略（向前兼容）
- **`component` 已弃用**：前端不再有「内置组件」分支，所有插件一律用 `blocks` 渲染，
  **包括需要多账号界面的插件**（用 `accounts` 块，见 §4.5）。请把界面**完整**写进
  `blocks`，不要依赖 `component`：新增插件不该要求改前端。
- **渲染位置**：插件页卡片只显示只读概览（`metric` / `tips`）；可编辑项
  （`text` / `number` / `toggle` / `button`）放在**插件设置弹窗**里（点卡片「设置」打开），
  仍由同一份 `blocks` 驱动 —— 插件无需关心自己显示在卡片还是弹窗
  （`accounts` 同属可编辑项，出现在弹窗里；卡片只显示 `metric`/`tips` 概览）。
- **表单回显（`echo`，强烈建议实现）**：`value` 只是描述里的**静态默认值** ——
  插件并不知道自己持久化过什么，所以只靠 `value` 会让表单**永远显示默认值**
  （典型症状：用户把阈值改成 42，重新打开又显示 85，像是没保存）。
  给 `text` / `number` / `toggle` 块加 `echo: "/路径"`，前端渲染时会
  `GET` 该路径取真实值，响应契约：

  ```json
  { "value": 42, "configured": true, "hint": "可选，覆盖占位符" }
  ```

  - 普通字段用 `value` 回填；读不到（`error`）则保留静态 `value` 兜底；
  - **`secret: true` 的字段必须只回 `configured`**，`value` 留 `null`
    （凭据永不回显是本项目的硬约束），前端会显示「已设置（留空则保持不变）」；
  - 同一条路径可同时服务 POST（保存）与 GET（回显），如 kzwr 的 `/quota`。
  - **如何区分读与写**：`action_json` 的信封里带 `method`（`"GET"` / `"POST"`）。
    同一个 action 路径在「渲染回显」与「用户点保存」时都会被调用，插件**必须**按
    `method` 分流，否则 GET 会被当成写入。示例见 `plugins/example-hello/` 的
    `/greeting`（GET 只读、POST 保存）。

    ```json
    { "body": { … }, "cfg": { … }, "method": "GET" }
    ```

    > `method` 是 2026-09-27 补入的：在此之前信封只有 `body`/`cfg`，
    > 外置插件**无法**区分读写，因而无法实现上面的 `echo` 契约。
- **动态 `metric`**：`metric` 块可带 `action`（如 `"/space"`），前端渲染时会 `GET`
  该路径，并用响应里的 `{"value": "...", "hint": "..."}` 覆盖静态文案 ——
  用于「空间用量」这类**实时数字**，避免为一个数字写专用前端组件。
  读取失败时保留静态 `value`（可当作兜底文案）。
- ~~`section` 取值 `settings`/`dashboard`~~ **已移除**（`ui.section`/`ui.order`/`ui.component`
  随插件页重构一并删除）：插件卡片一律渲染在**「插件」页**，不再有分区概念，
  顺序以 `/api/plugins` 返回为准。老插件的 `describe` 里若仍带这些键，
  宿主 serde 会忽略未知字段，不影响解析，但建议删掉以免误导。

### 4.2 `cfg_json`（宿主 → 插件：配置快照，**不含任何凭据**）

```json
{
  "abi_version": 1,
  "host_version": "0.4.0",
  "timezone": "CST (UTC+08:00)",
  "utc_offset_minutes": 480,
  "targets": [
    { "id": "default", "name": "默认目标（WebDAV）", "kind": "webdav",
      "url": "https://dav.kzwr.com/dav", "username": "me@example.com",
      "enabled": true, "ready": true, "primary": true }
  ],
  "tasks": [
    { "id": "default", "name": "默认任务", "enabled": true, "paths": ["/vol1/…"],
      "target_id": "default", "target_folder": "fn-backup", "schedule_cron": "0 2 * * *",
      "empty_recycle_bin": true, "recycle_max_gb": 20, "recycle_min_age_days": 7 }
  ],
  // 已弃用（ADR-021）：宿主不再代存插件配置，此字段恒为 {}
  "self_config": {},
  "after_backup_task": "default"
}
```

- 口令、age 私钥等**宿主自己的**凭据永不外传（`ready` 只表示凭据是否齐备）。
- `username` **会**传给插件（目标管理页本来就回显它）——这是决策 10-1 的定案。
- `tasks[].empty_recycle_bin` / `recycle_max_gb` / `recycle_min_age_days` 是**任务级**保留策略：
  宿主只提供数值，具体清理动作由具备该能力的插件实现（插件不该自己拍脑袋定门槛）。
- `after_backup_task` **只有 `after_backup` 事件**非 `null`：告诉插件「刚完成的是哪个任务」，
  它才能套用**该任务自己**的门槛。其它事件为 `null`（JSON 里字段恒在，用 `Option`）。

#### ~~`self_config`~~：**已弃用**（宿主不再代存插件配置）

**ADR-021（2026-09-28）起，宿主不再代存插件配置。** 该字段**恒为空对象**
（保留仅为 ABI 兼容，老插件读到空对象后应回退到默认值）。

插件改用**能力表**自管配置：

| 环节 | 机制 |
|---|---|
| 目录 | `own_data_dir`（能力表）→ `$TRIM_PKGVAR/plugins/<插件 id>/`，宿主已 `mkdir` |
| 加密 | **`seal` / `unseal`**（能力表）：用**宿主密钥**加密，插件拿不到密钥本身 |
| 存储 | 插件自行决定格式（建议「先写临时文件再改名」，避免半个文件） |
| 卸载 | `POST /api/plugins/<id>/purge` 会**删除该目录** |

> **为什么要提供 `seal`**：若不提供，插件自管配置就会从「宿主 age 加密」**降级为明文落盘**
> —— 只靠目录权限（0700）保护，目录被读走凭据即泄漏。密钥留在宿主手里、只暴露
> 「封/解」两个纯计算入口，就能既让插件自管、又不降级安全。
>
> **解密失败绝不回退成明文**：`unseal` 对非本宿主密钥加密的内容返回 NULL，
> 插件必须当作「无此配置」处理（否则一个被篡改的密文会被当明文用）。

**兼容**：老宿主（未下发能力表 / 无 `own_data_dir`）上，插件应退回**声明式 `config` 回写**
（见 §4.4）—— 宿主仍接受该字段，但**新宿主会忽略它**。

### 4.2.1 `target_json.config`：**目标自己的**字段（ADR-021 起）

宿主不再代存**插件级**配置，但**目标级**的自定义字段仍然注入 `target_json.config`：
它们由目标插件在 `describe_json.target.form` 里声明（见 §9.4.1），
随该目标存储在 `TargetConfig.fields`（`secret` 的加密落盘）。

| 环节 | 机制 |
|---|---|
| 前端声明 | `describe_json.target.form[]`（字段、类型、是否敏感）→ 「目标」页弹窗按声明渲染 |
| 写入 | `POST /api/targets`（body 的 `fields` 对象），按声明加密后存进该目标的 `fields` |
| 读取 | 打开实例时解密并**合并进 `target_json.config`**（同时单列在 `target_json.fields`） |
| 卸载 | 目标删除即随之消失（它是目标自己的属性） |

> `KzwrTargetAbi.config_get` / `config_set` 两个函数指针**仍是预留位**
> （SDK 导出时为 `None`）—— 目标级字段走上表通道即可。

### 4.3 `available_json` / `health_json` / `action_json` / `event_json`

| 回调 | 返回 |
|------|------|
| `available_json` | `{"available": true, "reason": null}` |
| `health_json` | 体检项数组，或 `{"checks":[…], "alerts":[…], "resolve":[…]}`（见下） |
| `action_json` | 任意 JSON（推荐 `{"success":true,"message":"…"}` 或 `{"success":false,"error":"…"}`）；宿主原样回给前端 |
| `event_json` | 任意 JSON；`after_backup` 可用 `{"count": N}` 表示"处理了 N 项" |

`action_json` 的入参 `request_json`：

```json
{ "body": { "…前端提交的 JSON…" }, "cfg": { "…配置快照…" }, "method": "POST" }
```

- **`method`**：`"GET"` / `"POST"`。同一条 action 路径既被前端**回显**（GET）又被**保存**（POST）
  时调用，插件必须按 `method` 分流，否则 GET 会被当成写入。
- **GET 的查询串进 `body`**：`GET /api/p/kzwr/accounts?fresh=1` 时，宿主把 query 序列化后
  放入 `body`，即 `{"fresh": "1"}`。⚠️ **值一律是字符串**（HTTP query 没有类型），
  判断布尔请用「真值」写法（`"1"/"true"/"yes"/"on"`），不要 `body["fresh"] == true`。
- 布尔判断请写成 `is_truthy(body.get("fresh"))` 之类：前端也可能传 `?fresh`（值为空串）。

事件名：`startup`（启动）、`patrol`（周期巡检，约 30 分钟）、`after_backup`（备份成功后）、`reload`（配置变更后）。

`health_json` 的两种形状宿主**都接受**（宿主 `cabi::parse_health`）：

```json
[ { "key": "kzwr.a1", "title": "酷族账号「主账号」", "status": "warn",
    "detail": "云端空间 92%（1.8 TB / 2 TB）", "hint": "阈值 90%，建议清理" } ]
```

```json
{ "checks": [ …同上… ],
  "alerts":  [ { "level": "warn", "message": "…" } ],
  "resolve": [ "云端存储空间已用「主账号」" ] }
```

- 体检项 `key` 的命名约定：`<插件id>` 或 `<插件id>.<子项/账号>`。**带点号即插件产物**，
  前端据此把「前往处理」按钮指向插件页（不需要为任何插件写专属映射）。
- `status` 省略时按 `ok` 处理；返回值**不是合法 JSON** 时退化成空表（体检不会因某个
  插件走形而 500）。

### 4.4 声明式副作用：插件「让宿主做事」的默认通道

**老插件不能调宿主**（它们的主表里没有宿主函数表）。插件想告警、
想清告警、想持久化配置、想留审计，就把意图**随返回值一起带回来**，由宿主执行。
`health_json` / `event_json` / `action_json` 的返回值里都识别这 4 个可选字段
（宿主入口：`cabi::apply_side_effects`；顺序固定 **alerts → resolve → config → audit**，
这样「同一轮里既报新障又消旧障」的结果是确定的）：

```json
{
  "success": true,
  "alerts":  [ { "level": "warn", "message": "云端存储空间已用「主账号」 92%，达到阈值 90%" } ],
  "resolve": [ "云端存储空间已用「主账号」" ],
  "config":  { "set": { "accounts": "[…]" }, "remove": ["token"] },
  "audit":   [ { "action": "kzwr.accounts.add", "detail": "新增账号「主账号」", "ok": true } ]
}
```

| 字段 | 宿主行为 | 要点 |
|---|---|---|
| `alerts[]` | 以来源 `Plugin(插件id)` 落库，**同来源+同文案去重**（不重复外发 Webhook） | `level` 只认 `"error"`，其它一律 `warn`；**消解靠文案**，所以要把变化量（账号名）写进文案，且别写时间戳——否则每次都不重复、越积越多 |
| `resolve[]` | 删除**本插件**中消息以这些**前缀**开头的告警 | 用于「条件恢复后自动消解」（空间回落）。前缀必须是 `alerts` 文案的开头若干字符；空串被忽略 |
| ~~`config`~~ | **已移除（ADR-021）**：宿主不再代存插件配置，该字段被**忽略**（不报错）。插件改用 `own_data_dir` + `seal`/`unseal` | 键名规则仍适用于**目标自定义字段**（§9.4.1） |
| `audit[]` | `AuditLog.record(action, detail, ok, None)` | 让用户**看不见**的后台动作可追溯（如备份后自动清理）。`action` 自带插件命名空间，核心不猜语义 |

#### `config` 键名规则（踩过的坑，必读）

- 非空、`≤ 64` 字符、只允许 **`[A-Za-z0-9_-]`** —— **点号 `.` 不允许**。
- 校验是 **整批生效或整批拒绝**：任一非法键名 → 本次 `config` 全部不写（宿主只留一条 warn 日志）。
  曾经 kzwr 用 `percent.<id>`，被整批拒绝且插件以为保存成功，症状是「阈值改了不生效」。
  现在按账号存阈值一律用 **`percent-<id>`**。
- 值：非字符串会被 `to_string()` 后存入；**空串 = 删除该键**（`remove` 也走同一路径）。
- 想在一个键里存结构化数据（如账号列表）就存 JSON 字符串——整个命名空间一起加密，
  比拆成 `account.1.token` 之类更安全也更简单（且拆出来的键名还得不含点号）。

> 副作用字段是**新增的可选字段**：不认识它们的旧插件照常工作（§5 的「只增不改」）。

### 4.5 `accounts` 区块：多凭据插件的通用界面（前端零改动）

一个插件管多份凭据（多账号、多站点）时，**不要**写前端专属组件，声明 `accounts` 块即可，
前端 `PluginAccounts.svelte` 按声明渲染。数据契约（路径都相对 `api_base`，即 `/api/p/<id>`）：

| 操作 | 请求 | 响应 |
|---|---|---|
| 列表 | `GET {list}` | `{"accounts":[{"id","name","configured","meta":{…}}]}` |
| 加 `fresh=1` | `GET {list}?fresh=1` | 同上；插件此时**才可以**访问外部接口取实时状态 |
| 新增 | `POST {add}` `{"name","<credential_field>"}` | `{"success":true,"message":"…","config":{…}}` |
| 修改 | `POST {update}` `{"id","name","<credential_field>"}` | 同上；**凭据字段留空 = 保持原值** |
| 删除 | `POST {remove}` `{"id"}` | 同上 |
| 每项数值设置 | `POST {edit_action}` `{"id","<edit_field>": number}` | 同上（可选：不声明 `edit_action` 就没有这个按钮） |

字段语义：

- `credential_field` / `credential_label` / `credential_placeholder`：凭据字段名与输入框文案。
- `multiple`：`false` 时前端隐藏「添加」按钮（仍显示列表与编辑）。
- `edit_action` / `edit_field` / `edit_label` / `edit_suffix` / `edit_hint` / `edit_min` / `edit_max`：
  每项数值设置（如预警阈值）的动作、字段名与取值范围；前端在提交前按 `edit_min`/`edit_max` 校验。
- `meta`：**只放可显示的状态**（用量、是否已设置、提示文案…），供前端渲染成一行小字。
  ⚠️ **`list` 响应永不返回凭据明文**，也不要用 `configured: false` 之外的方式表达「没设置」：
  凭据明文从插件出前端即算泄漏（本项目硬约束）。
- 新增/修改后前端会自动刷新列表并回调页面刷新指标（`metric` 块重新 GET）。

### 4.6 宿主能力表：插件**回调**宿主（`host_bind`，可选）

上面的声明式通道有个固有限制：**插件必须"先返回"才能上报**。于是——
动作跑到一半想写日志、多账号循环想报进度、后台想自己定个周期任务——都做不到。
主表尾部新增的可选回调 `host_bind` 解决了这件事：

```rust
// 插件主表尾部（SDK 的 export_plugin_v1! 已自动填好，插件无需自己实现）
pub host_bind: Option<extern "C" fn(*const KzwrHostAbi, *mut c_void) -> c_int>,
```

加载后宿主调用它一次，把**静态能力表** `KzwrHostAbi` 和一个**不透明句柄 `ctx`** 交给插件：

```rust
// 插件侧（用 SDK 的安全 API，不要自己解引用能力表）
if sdk::host::available() {
    sdk::host::log(sdk::LOG_INFO, "开始清理");          // → 宿主 app.log
    sdk::host::progress("清空回收站", 1, 3, "账号 A");   // → WebSocket 事件流
    sdk::host::audit("kzwr.trash.auto", "删除 12 项", true);
    sdk::host::alert(sdk::ALERT_WARN, "空间已用 92%");
    sdk::host::resolve_alerts("云端存储空间已用");
    if let Some(t) = sdk::host::config_get("token") { /* 明文 */ }
    sdk::host::config_set("percent-a1f3", "90");
    sdk::host::schedule("nightly", "0 3 * * *");        // 到点回调 event_json("timer", cfg)
}
```

| 能力 | 语义 | 关键约束 |
|---|---|---|
| `log` | 进宿主的 `app.log`（带时间戳/级别/`plugin=<id>`） | **入队**；队列满则丢弃并计数，绝不打回插件线程 |
| `audit` | 与宿主敏感操作**同一份** `audit.log` | 同上；`action` 自带插件命名空间 |
| `alert` | 来源 `Plugin(id)`，与声明式 `alerts` **同一去重规则** | 两条通道混用**不会**产生重复告警 |
| `resolve_alerts` | 按消息前缀消解**本插件**的告警 | 与声明式 `resolve` 同一实现 |
| ~~`config_get`~~ / ~~`config_set`~~ | **已弃用（ADR-021）**：宿主不再代存插件配置 | 实现改为「读恒 NULL / 写恒拒绝」，让老插件**安全失败**并改用 `own_data_dir` |
| `host_version` / `now_ms` / `own_data_dir` | 宿主版本串、毫秒时间戳、**插件私有目录** | 前两者是纯读；目录在 `$TRIM_PKGVAR/plugins/<id>`（宿主已 `mkdir`）——**插件配置的正式存放位置** |
| **`seal` / `unseal`** | 用**宿主密钥**加密/解密密文（返回 base64） | 插件自管配置时用它保护凭据；**密钥留在宿主**，插件拿不到。解密失败返回 NULL，**绝不回退成明文** |
| `progress` | 转发到 WebSocket，`kind="plugin"` | 前端**忽略**它对顶部任务卡的覆盖，不会顶掉备份状态 |
| `schedule` | 注册周期任务（cron，宿主本地时区） | 到点回调 `event_json("timer", cfg)`，`cfg.timer_kind` = 注册的 `kind` |

#### 三条硬纪律（插件必须知道）

1. **写操作是"入队"而非同步落库**：`log`/`audit`/`alert`/`resolve`/`progress`/`schedule`
   只做一次 `try_send`，由宿主唯一的消费任务落库。队列满 ⇒ **丢弃**并计数
   （宿主宁可丢观测，也不让插件线程被阻塞）。因此这些调用**不保证**在同一毫秒内出现在日志里；
   但一次动作用 `action_json` 返回后，宿主会做一次**排空屏障**，告警与审计在同一个响应里就可见。
2. **`seal`/`unseal` 是同步的纯计算**（无落库、无副作用），因此宿主保证「**不跨 FFI 持锁**」。
   配置的**读写时机由插件自己掌握**（写自己的文件），宿主不参与。
3. **能力被 `ctx` 限定到本插件**：所有命名空间（配置键、告警、定时器、数据目录）都由 `ctx`
   决定，参数里**没有**插件 id 可填 ⇒ 改不了、也读不到别的插件。插件被**禁用/卸载后 `ctx`
   立即失效**，之后的调用被静默丢弃（不会崩，也**不会**再产生任何效果）。

#### 兼容性（两侧都能优雅降级）

- **老插件 + 新宿主**：主表短一截 ⇒ 宿主判定「未实现 `host_bind`」，不调用；插件继续用声明式通道。
- **新插件 + 老宿主**：没收到 `host_bind` ⇒ `sdk::host::available()` 为 `false`，
  SDK 的每个 API 都退化成**安全空操作**，插件行为与以前一致（不崩）。
- 能力表**逐字段**探测 `size`：老宿主的表短一截时，插件仍接受整表、只跳过缺失的能力
  （`free_str` 因此在必需前缀里，见 `KzwrHostAbi::MIN_SIZE`）。
- **老宿主没有 `seal`/`own_data_dir`** ⇒ 插件应退回**声明式 `config` 回写**（宿主代存）。
  新宿主会**忽略**该字段，因此两条路径可以同时写在插件里、由插件按能力探测决定走哪条
  （kzwr 即如此：见 `plugins/kzwr/src/store.rs` 与 `state.rs::Writeback`）。

> **安全取舍**：`ctx` 是不透明指针而非密码学凭证——理论上知道了别人的 `ctx` 地址就能冒用。
> 这是个**有意的**取舍：宿主从不 `dlclose` 插件、进程内插件互不信任程度有限；
> 真要强隔离得上进程级方案（**评估见 `docs/memory/dev/PLUGIN_ISOLATION.md`**：
> 含 Landlock 实测、四个档位对比与推荐路线）。宿主侧不变式（不跨 FFI 持锁、每入口 `catch_unwind`、
> 入参校验 + 4 KiB 截断 + 令牌桶限流）已由 `plugin/host_abi.rs` 落实，并有单元测试钉住。

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

# 5) 签名（强制验签，必做）
#    官方私钥（仓库/Scripts/keys/sign.key）不存在时，build_plugins.sh 会提示未签名；
#    自签流程：
bash Scripts/sign_plugin.sh keygen                      # 生成 Scripts/keys/sign.key 并打印公钥
bash Scripts/sign_plugin.sh sign dist/plugins           # 生成 <so>.sig（64 字节裸 Ed25519 签名）
#    再把打印出的公钥填进「插件」页 → 外置插件管理的「插件公钥」（plugins.pubkeys）

# 6) 安装：放进 $TRIM_PKGETC/plugins/，在「插件」页开启「外置插件加载」后重启应用
```

> 随包插件（`$TRIM_APPDEST/plugins/`）由发布流程用**官方私钥**签名，宿主内置对应公钥
> （`loader::OFFICIAL_PUBKEYS`）→ 开箱即用，用户无需配置任何公钥。
> 自签插件则必须把公钥填进 `plugins.pubkeys`（内置公钥与用户公钥是**并集**关系）。

## 7. 诊断

- `GET /api/plugins` 的 `external` 字段：`enabled` / `dirs`（扫描到的目录与来源）/ `reports[]`
  （每个 `.so` 的 `loaded`、`mechanism`、`id`、`abi`、`error`、
  **`signature`**（`verified` / `unsigned` / `failed`）、**`sig_file`**（`.sig` 是否存在））
- 「插件」页的「外置插件（动态库）」卡片直接展示上述结果，并用徽标标出 **稳定 ABI v1**
  （~~Rust 直连~~ 已移除）与**签名三态**（已签名 / 未签名 / 验签失败）
- `sig_file=false` + `signature=failed` 是「压根没签名」；`sig_file=true` + `signature=failed`
  是「签名对不上」（可能被篡改）——两者风险不同，前端文案必须区分
- 日志：`fnos_backup::plugin::loader`（加载/跳过原因、验签结果）；`fnos_backup::plugin::cabi`（C ABI 接管）
- 相关环境变量：`FN_KZWR_PLUGINS=1`（开启加载）、`FN_KZWR_PLUGIN_DIR`（指定目录）、
  `FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1`（**仅本机调试**：跳过验签）
  （~~`FN_KZWR_PLUGINS_ALLOW_MISMATCH=1`~~ 属已移除的 Rust 直连机制，今已不存在）

## 8. 安全边界

- 加载 `.so` 等价于**执行任意本地代码**：默认关闭，必须由用户在设置页显式开启
- **默认强制验签**（ADR-013 决策 9）：Ed25519 签名覆盖 `.so` **原始字节**，
  签名文件 `<so>.sig`（64 字节裸签名），公钥为 base64 的 32 字节裸 Ed25519 公钥；
  **先验签、通过才 `dlopen`**，验签失败的插件绝不进入动态库加载阶段
- 信任锚 = 内置官方公钥 ∪ `plugins.pubkeys`。注意内置公钥**只证明签发者**，
  不代表代码被审计；用户放置目录里的插件一样要自带有效签名才加载
- 唯一逃生阀 `FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1` 仅用于本机调试（正式包的生命周期脚本
  `env -i` 白名单启动，不会透传该变量）
- **一插件一公钥**（2026-09-27）：`plugins.plugin_pubkeys` 是「**文件名 → 公钥**」映射，
  验签时**只查该文件自己的公钥**（外加内置官方公钥）。旧的 `plugins.pubkeys`
  扁平列表语义是「任一公钥可验任一插件」—— 存在**越权信任**：插件 A 的私钥泄露后，
  攻击者可用它签出能通过校验的插件 B。改为按文件绑定后不再成立。
  - 为什么用**文件名**而不是插件 id：验签发生在 `dlopen` **之前**，那时只有路径可用。
  - `plugins.pubkeys` 保留为**兼容回退**：仅当 `plugin_pubkeys` **整体为空**
    （即尚未迁移的旧安装）时才生效；一旦用了新模型，**未登记的文件就是没有授权公钥**。
- **界面安装**：`POST /api/plugins/install`（base64 JSON，非 multipart）→
  校验文件名/公钥格式 → **先用提供的公钥验签** → 通过后原子写入用户插件目录 →
  把绑定关系写入 `plugin_pubkeys`。验签失败**绝不落盘**。配套
  `POST /api/plugins/:file/uninstall` 删除 `.so`/`.sig` 并解绑公钥（内置/随包插件不可删）。
  - 上传用 base64 而非 multipart：未引入 `multer`，不想为一个上传新增依赖。
- **热加载**（2026-09-27）：`POST /api/plugins/reload` 按当前配置重新扫描装载；
  保存开关、安装、卸载后都会**自动**触发，无需重启应用。
  注意「卸载不等于立即释放内存」：若正在执行的备份持有某插件派生的
  `Arc<dyn TargetStorage>`，对应动态库会存活到该次备份结束（引用计数保证安全）。
- **运行时启停**（按插件粒度，无需重启）：`POST /api/plugins/<id>/enable`
  `{"enabled":false}` → 写入 `plugins.disabled`，插件立即从 `/api/plugins`、路由分发、
  目标装配与生命周期钩子中消失；`true` 立即恢复。
  **停用 ≠ 卸载**：插件返回的 vtable 被宿主按 `&'static` 持有，飞行中的备份也可能
  仍持有由它派生的 `Arc<dyn TargetStorage>`，卸载会让这些引用悬空 → 进程崩溃。
  故停用只做**逻辑摘除**，动态库句柄仍保活到进程结束（真正释放需重启应用）。
  配套保护：停用仍被任务引用的目标插件会**级联停用**那些任务（响应里回报
  `affected_tasks`）；若正在执行备份的任务使用该插件，接口**拒绝**本次操作
  （返回 `error` + `running_task`），避免打断备份。
- 私钥 `Scripts/keys/sign.key` 被 `.gitignore` 排除，只应存在于发布机；仓库内只有公钥

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
| 看门狗 | 宿主侧检测进度停滞（默认 120s）→ 判失败并告警。单次阻塞 FFI **无法强制中断**（同进程模型固有限制；子进程方案才可强杀，见 `docs/memory/dev/PLUGIN_ISOLATION.md`） |
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
- **是否启用由用户按「目标」配置**：设置项 `TargetConfig.parallel`
  （缺省沿用插件声明；`0`/`1` = 关闭；`≥2` = 启用，上限 8），接口
  `POST /api/targets/:id/parallel`，保存后热重建、无需重启
  - **必须按目标而非按插件**：同一个插件（如 webdav）会被多个目标同时实例化
    （多账号各一套凭据、各自网络条件），按插件存一份会让「改一个目标、同类型目标全变」。
  - 优先级：`TargetConfig.parallel` → `plugins.target_parallel[插件 id]`（**旧配置的兼容回退**）
    → 插件自身声明。老配置的插件级值会在启动迁移时**继承**到当时存在的各目标上，
    因此升级不会让已配置的并发静默失效。
  - `POST /api/plugins/:id/parallel` 仍保留（写插件级回退值），但新代码请用按目标的接口。
- 插件本身不支持时（未声明 `supports_plan`），即便用户配了并发度也不会启用

### 9.4.1 目标表单的形态由插件声明（`describe_json.target`）

「目标」页的**新建表单**是通用的：字段形态由目标插件声明，前端据此渲染。
不声明也能用（按 WebDAV 语义渲染），但**不用凭据**的目标必须显式声明，
否则宿主会强制要求账号密码 —— 用户**根本建不出**这类目标。

```json
"target": {
  "needs_credentials": false,
  "url_label": "目录路径",
  "url_placeholder": "/vol1/backup/kzwr-localfs",
  "url_hint": "本机目录的绝对路径；插件只写入 age 密文"
}
```

| 字段 | 缺省 | 含义 |
|---|---|---|
| `needs_credentials` | **`true`** | 是否需要用户名/密码。`false` = 新建目标允许凭据留空，且保存前**不**做连通性实测（插件通常没有可测的连接） |
| `url_label` | `"地址"` | `url` 字段的展示标签。让插件说明其含义（WebDAV 是「地址」，本地目录是「目录路径」） |
| `url_placeholder` | WebDAV 官方地址 | 输入框占位提示 |
| `url_hint` | WebDAV 说明 | 输入框下方的说明文字 |

要点：

- **`needs_credentials` 缺省为 `true`**（不是 `false`）：老插件不写该字段时行为**完全不变**，
  不会因为升级而突然跳过凭据校验。
- `url` 的语义由插件自己解释：WebDAV 是服务地址，本地目录是绝对路径 ——
  宿主只做「非空」校验并原样透传（`target_json.url`）。
- 前端在「新建目标」时**只能选 `kind == "target"` 的插件**（增强类插件不出现在类型下拉里）；
  **类型创建后不可更改**（换类型等于换一种存储，凭据与语义都不同）。
- 插件自己的设置（如本地目录的 `root`）走 §4.4 的自管配置通道
  （用上面的 `target.form` 声明，值存该目标自己的 `TargetConfig.fields` 并注入
  `target_json.config`）。**不再**使用 `ui.blocks` 的 `scope: "host"`（已弃用，ADR-021）。

### 9.4.1 「新建/编辑目标」弹窗的表单由插件声明（`describe_json.target`）

「目标」页的新建/编辑走**弹窗**，表单形态**完全由目标插件声明** —— 宿主只负责渲染与存取，
不预设任何字段语义。这与插件设置弹窗（§4.4 的 `ui.blocks`）是同一套思路：
**新增目标类型不需要改前端**。

```json
"target": {
  "needs_credentials": false,
  "url_label": "目录路径",
  "url_placeholder": "/vol1/backup/kzwr-localfs",
  "url_hint": "本机目录的绝对路径；插件只写入 age 密文",
  "form": [
    { "key": "url", "label": "目录路径", "kind": "text", "required": true,
      "placeholder": "/vol1/backup", "hint": "绝对路径" },
    { "key": "subdir", "label": "子目录（可选）", "kind": "text", "default": "" },
    { "key": "token", "label": "访问令牌", "kind": "password" },
    { "key": "keep_local", "label": "保留本机副本", "kind": "toggle" },
    { "key": "mode", "label": "模式", "kind": "select",
      "options": [{ "value": "fast" }, { "value": "safe", "label": "安全模式" }] }
  ]
}
```

| 字段 | 缺省 | 含义 |
|---|---|---|
| `key` | — | 字段键（提交体与 `target_json.config` 里的键名） |
| `label` | — | 展示标签 |
| `kind` | `"text"` | `text` \| `password` \| `number` \| `toggle` \| `select` |
| `required` | `false` | 前端校验必填 |
| `secret` | 按 `kind == "password"` 推断 | **是否敏感**：加密落盘，回显只给布尔 |
| `placeholder` / `hint` / `default` | 空 | 占位、说明、新建时的预填值 |
| `options` | 空 | `select` 的选项（`label` 省略则用 `value`） |

#### 键名与存储位置

| 键 | 存储 | 注入 `target_json` |
|---|---|---|
| **`url`** | `TargetConfig.url` | `url` |
| **`username`** / **`password`** | `username_enc` / `password_enc`（加密） | `username` / `password` |
| **其余任意键** | `TargetConfig.fields`（**按目标**、按 `secret` 加密） | 合并进 `config`（同名时**目标级优先**），同时单列在 `fields` |

要点：

- **三个 well-known 键**（`url`/`username`/`password`）映射到既有存储，
  是为了不破坏已发布插件的读取位置（它们一直读 `target_json.url` 等）。
- **其余键按目标存储**（`TargetConfig.fields`），**不用**插件级存储 ——
  后者会让同一插件的多个目标互相覆盖（与 §9.4 并发度踩过的坑同类；
  旧的插件级 `plugin_data` 通道已随 ADR-021 移除）。
- **敏感字段永不回传明文**：`GET /api/targets` 里该键是 `true`/`false`（是否已设置）。
  留空提交 = **不修改**（与 `password` 既有语义一致）。
- 空串提交 = **清除**该字段。
- 键名规则同 §4.4（非空、≤64、`[A-Za-z0-9_-]`，**不允许点号**），非法键整次保存被拒。
- 插件**不声明** `form` 时，前端回退到内置的 WebDAV 默认表单（`url` + `username` + `password`）
  —— 老插件零改动，行为与之前一致。
- `url_label` / `url_placeholder` / `url_hint` 仍然有效：它们是**回退表单**的文案
  （未声明 `form` 时用）；声明了 `form` 就在字段里自己写 `label`/`hint`。

### 9.5 写一个目标插件（SDK）

SDK 提供了目标表与导出宏，用法与增强插件一致：

```bash
cp -r plugins/example-localfs plugins/my-target   # 复制示范（本地目录目标）
bash Scripts/build_plugins.sh                     # → dist/plugins/libmy_target.so
```

```rust,ignore
use fn_kzwr_plugin_sdk as sdk;

// 1) describe_json 里声明这是**目标**插件，并给出目标能力
//    "kind": "target",
//    "runtime": { "target": "fn_kzwr_plugin_target_v1" },
//    "target":  { "supports_plan": true, "max_parallel": 4, "preferred_chunk_kib": 1024 }

// 2) 实现回调（详见下方清单），全部用 catch_unwind 包住

// 3) 导出两张表：主表（元信息/UI）+ 目标表（传输能力）
sdk::export_plugin_v1!(describe, available, action, health);

sdk::export_target_v1!(
    my_open,           Some(my_close),
    my_write_begin,    my_write_chunk, my_write_end,    Some(my_write_abort),
    my_read_begin,     my_read_chunk,  Some(my_read_end),
    my_list_json,      my_delete,      Some(my_ensure_dir), Some(my_ping), Some(my_test_json),
    Some(my_last_error),
    Some(my_plan_begin), Some(my_plan_next), Some(my_plan_end),  // 不支持并发回传就传 None
);
```

必填回调：`target_open` / `write_begin` / `write_chunk` / `write_end` /
`read_begin` / `read_chunk` / `list_json` / `delete`；其余可选（传 `None`）。

⚠️ **务必做路径校验**：`rel_path` 来自清单，插件必须自己拒绝 `..` 之类的越权路径
（示范插件的 `resolve()` 就是最小实现）。宿主也会校验 `plan_next` 返回的路径属于本次清单，
但插件侧不能依赖这一点。

### 9.6 参考实现

| 实现 | 位置 | 说明 |
|---|---|---|
| 内置 WebDAV 目标 | `backend/src/plugin/builtin/webdav_abi.rs` | 编译期静态表；内部复用 `WebdavTarget` 做协议/分片/重试，对外只暴露推块接口；实现了 `plan_*`（每批 12 个） |
| 外置「本地目录」目标 | `plugins/example-localfs/` | **外置插件提供备份目标的示范**：写本地目录，演示路径越权防护、临时文件改名落定、并发回传（小文件优先，每批 10 个） |

`plugins/example-localfs/` 的用法：构建后放进插件目录并开启外置加载，
然后在「目标」页新建目标、类型选 `example-localfs`、**地址填一个本机目录**
（本插件把 `url` 当目录用，不需要真实账号）。

---

## 10. 诊断（目标插件）

- `GET /api/plugins`：每个插件带 `kind`（`target` / `enhance`）、`supports_plan`（能力声明）、
  `parallel`（**插件级**并发度，仅作回退）与 `needs_credentials` / `url_label` 等表单声明
- `POST /api/targets/:id/parallel`：`{"parallel": N}` → 设置**该目标**的上传并发路数（推荐）
- `POST /api/plugins/:id/parallel`：同上，但写的是**插件级回退值**（旧接口，仅兼容保留）
- `POST /api/targets`：新建/更新目标。`kind` 必须是一个**已注册的目标插件 id**；
  `needs_credentials=false` 的插件允许不带账号密码
- 日志：`fnos_backup::plugin::target_abi`（推块/复查/看门狗）、`fnos_backup::domain::backup`（是否走并发回传）
- 插件与宿主同进程、同权限运行（**隔离评估与缓解路线见 `docs/memory/dev/PLUGIN_ISOLATION.md`**）；
  若不需要这一点，请勿启用

### 10.1 「插件目标建不出来」的排查顺序

1. **插件是否加载**：`GET /api/plugins` 里有没有它？`external.reports` 里的签名/加载诊断怎么说？
2. **`kind` 是否为 `target`**：增强类插件（`kind: "enhance"`）**不能**作为备份目标，
   也不会出现在「新建目标」的类型下拉里。
3. **是否被禁用**：`disabled: true` 的插件不会出现在类型下拉里（禁用了就装配不出来）。
4. **是否声明了 `needs_credentials: false`**：若插件不用凭据却没声明，保存会被
   「新目标必须填写用户名与密码」拒绝 —— 这是**最常见**的原因。
5. **`url` 是否非空**：宿主只校验非空，语义由插件解释（本地目录要填**绝对路径**）。
