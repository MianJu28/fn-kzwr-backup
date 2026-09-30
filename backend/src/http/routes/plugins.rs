//! 插件管理（列表 / 启停 / 热重载 / 安装卸载 / 清除数据）
//!
//! 与插件**运行时**相关的一切：`/api/plugins*` 下的管理端点。
//! 插件动作分发（`/api/p/<id>/*`）在 [`super`] 的 `plugin_dispatch`。


use axum::extract::State;
use axum::response::Json;

use crate::AppState;

use super::common::err;
use super::types::*;

/// 插件清单（内置插件；供前端区块注册表与问题诊断）
///
/// 返回项包含 `available`（是否已可用）与 `ui`（设置页/概览页区块描述），
/// 前端据此决定渲染哪些卡片、顺序如何、用内置组件还是 `blocks` 通用渲染。
pub(super) async fn plugins_list(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mgr = state.config.lock().unwrap();
    let cfg = mgr.load().unwrap_or_default();
    let env_override = crate::plugin::loader::enabled_by_env();
    // 孤立数据检测：宿主**不再代存插件配置**（ADR-021），故恒为空列表。
    // 保留该响应字段是为了不破坏前端契约（前端仍会渲染「清理遗留配置」提示位）。
    let orphan_data: Vec<String> = Vec::new();
    Json(serde_json::json!({
        "plugins": state.plugins.describe(&cfg),
        // 有自管数据但插件未加载的 id（卸载残留；前端据此提示清理）
        "orphan_data": orphan_data,
        // 被禁用的插件 id（前端置灰 + 允许重新启用）
        "disabled": state.plugins.disabled_ids(),
        // 正在执行备份的任务 id（禁用插件时的保护对象；无则为 null）
        "running_task_id": state.running_task_id.read().unwrap().clone(),
        // 外置插件（ADR-013 方案 B：动态库）的开关/目录/加载诊断
        "external": {
            "configured": cfg.plugins.enabled,
            "env_override": env_override,
            "enabled": env_override.unwrap_or(cfg.plugins.enabled),
            // 展示用：只有一个「插件目录」（物理上还有个随应用分发目录，不展示给用户）
            "dir": crate::plugin::loader::plugin_dir_label(),
            "dirs": state.plugins.plugin_dirs()
                .iter()
                .map(|(p, s)| serde_json::json!({ "path": p, "source": s }))
                .collect::<Vec<_>>(),
            "reports": state.plugins.external_reports(),
        }
    }))
}

/// `POST /api/plugins/:id/parallel`：设置**该插件**的上传并发路数
///
/// ⚠️ 这是**旧的插件级**接口，仅作兼容保留：一个插件会被多个目标同时实例化
/// （多账号各一套凭据），按插件存一份会让「改一个目标、同类型目标全变」。
/// 新代码请用 **`POST /api/targets/:id/parallel`**（按目标配置）。
///
/// 插件级值仍会作为**回退**被使用：某目标自身未设置（`TargetConfig.parallel == None`）时
/// 沿用插件级值，因此老配置的并发设置不会突然失效。
pub(super) async fn plugin_parallel(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: Option<axum::extract::Json<PluginParallelRequest>>,
) -> Json<serde_json::Value> {
    let Some(plugin) = state.plugins.target_plugin(&id) else {
        return Json(err(format!("未注册目标插件 {id}")));
    };
    if !plugin.supports_plan() {
        return Json(err(format!("插件 {id} 不支持并发回传")));
    }
    let v = body
        .and_then(|b| b.0.parallel)
        .unwrap_or(0)
        .min(crate::plugin::target_abi::MAX_PARALLEL);
    let cfg = {
        let mgr = state.config.lock().unwrap();
        let mut cfg = mgr.load().unwrap_or_default();
        cfg.plugins.target_parallel.insert(id.clone(), v);
        if let Err(e) = mgr.save(&cfg) {
            return Json(err(format!("{:#}", e)));
        }
        cfg
    };
    // 目标实例按新并发度重建：无需重启
    state.reload_targets(&cfg);
    state.audit.record(
        "plugin.parallel",
        format!("设置插件 {id} 上传并发路数 {v}"),
        true,
        None,
    );
    Json(serde_json::json!({ "success": true, "parallel": v, "error": null }))
}

/// `POST /api/plugins/:id/purge`：卸载清除插件自管数据（ADR-013 决策 2）
///
/// 流程：引用检查（仍被目标 `kind` 或任务所引目标的 `kind` 引用 → 拒绝，并列出引用项）
/// → 调插件 `destroy`（若实现）→ 删除该插件的**私有数据目录**（`own_data_dir`，ADR-021）
/// → 记审计。动态库句柄由宿主保活到进程结束，此处不卸载 `.so` 本身。
pub(super) async fn plugin_purge(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<serde_json::Value> {
    let mgr = state.config.lock().unwrap();
    let mut cfg = mgr.load().unwrap_or_default();

    // 1) 引用检查：`TargetConfig.kind == id`，或某任务所引用目标的 `kind == id`
    let mut referencing = Vec::new();
    for t in &cfg.targets {
        if t.kind == id {
            let n = if t.name.is_empty() { t.id.clone() } else { t.name.clone() };
            referencing.push(format!("目标「{n}」"));
        }
    }
    for task in &cfg.tasks {
        if let Some(t) = cfg.target_by_id(&task.target_id) {
            if t.kind == id && !task.target_id.is_empty() {
                let n = if task.name.is_empty() { task.id.clone() } else { task.name.clone() };
                referencing.push(format!("任务「{n}」"));
            }
        }
    }
    referencing.sort();
    referencing.dedup();
    if !referencing.is_empty() {
        return Json(serde_json::json!({
            "success": false,
            "error": "插件仍被引用，拒绝卸载清除",
            "referenced_by": referencing,
        }));
    }

    // 2) 调插件 `destroy`（自身状态清理；动态库句柄仍由宿主保活）
    state.plugins.call_destroy(&id);

    // 2.5) 插件能力表 ctx 立即失效：数据都要清了，在途效果不能再落库
    state.host_effects.revoke(&id);

    // 3) 插件配置由插件自己保管（`own_data_dir`），宿主**不代存**（ADR-021）。
    // 卸载清除时宿主删掉那个目录（连同其中的配置），这才是「清除」的实际动作。
    let removed = remove_plugin_data_dir(&state, &id);
    let _ = &mut cfg;
    if let Err(e) = mgr.save(&cfg) {
        return Json(err(format!("{:#}", e)));
    }
    state.audit.record(
        "plugin.purge",
        format!("卸载清除插件 {id}（{}自管数据）", if removed { "删除了" } else { "无可删" }),
        true,
        None,
    );
    Json(serde_json::json!({
        "success": true,
        "removed": removed,
        "referenced_by": Vec::<String>::new(),
    }))
}

// ── 插件启停（运行时，无需重启）──────────────────────────────────────

/// `POST /api/plugins/:id/enable`：启用/禁用某个插件（按插件粒度）
///
/// body：`{"enabled": true|false}`（缺省 true）。
///
/// 语义与保护规则：
/// - **禁用**只做**逻辑摘除**（不卸载 `.so`，见 `PluginSettings::disabled`）：
///   立即从 `/api/plugins`、路由分发、目标装配与生命周期钩子中消失；
/// - 若该插件是**目标插件**且仍被任务引用，则**级联禁用**那些任务
///   （否则任务会立刻失败且用户不知道原因），并在响应里列出受影响的任务；
/// - **正在执行备份的任务不会被级联禁用**：那会打断正在进行的备份。
///   此时接口拒绝本次禁用，提示用户等备份结束后再操作；
/// - 禁用前会热刷新目标池，使改动立即生效。
pub(super) async fn plugin_set_enabled(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Json<serde_json::Value> {
    let enabled = body
        .as_ref()
        .and_then(|Json(v)| v.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    // 插件必须存在（含被禁用者，否则无法重新启用）
    let known = state.plugins.plugin_ids().iter().any(|x| x == &id);
    if !known {
        return Json(err(format!("插件不存在：{id}")));
    }

    // 正在执行的任务：禁用目标插件会打断它，故一律保护
    let running = state.running_task_id.read().unwrap().clone();

    let mgr = state.config.lock().unwrap();
    let mut cfg = mgr.load().unwrap_or_default();

    // 受影响（将被级联禁用）的任务：仅当禁用的是它们所用的**目标插件**
    let mut affected: Vec<String> = Vec::new();
    if !enabled {
        for task in &cfg.tasks {
            if task.enabled {
                if let Some(t) = cfg.target_by_id(&task.target_id) {
                    if t.kind == id {
                        let name = if task.name.is_empty() {
                            task.id.clone()
                        } else {
                            task.name.clone()
                        };
                        affected.push(name);
                    }
                }
            }
        }
        affected.sort();
        affected.dedup();

        // 正在跑的任务不能被级联禁用 —— 拒绝整次操作，让用户明确等待
        if let Some(run_id) = &running {
            let run_uses_this = cfg
                .task_by_id(run_id)
                .and_then(|t| cfg.target_by_id(&t.target_id))
                .map(|t| t.kind == id)
                .unwrap_or(false);
            if run_uses_this {
                let run_label = cfg
                    .task_by_id(run_id)
                    .map(|t| if t.name.is_empty() { t.id.clone() } else { t.name.clone() })
                    .unwrap_or_else(|| run_id.clone());
                return Json(serde_json::json!({
                    "success": false,
                    "error": format!(
                        "任务「{run_label}」正在执行备份，它使用该插件；\
                         现在禁用会打断备份。请等备份结束后再操作。"
                    ),
                    "running_task": run_label,
                    "affected_tasks": affected,
                }));
            }
        }

        // 级联禁用受影响的任务
        // 先算出「哪些任务引用了该插件」（不可变借用），再统一改（可变借用），
        // 避免在 iter_mut 中又去读 cfg。
        let task_ids: Vec<String> = cfg
            .tasks
            .iter()
            .filter(|task| {
                cfg.target_by_id(&task.target_id)
                    .map(|t| t.kind == id)
                    .unwrap_or(false)
            })
            .map(|task| task.id.clone())
            .collect();
        for task in cfg.tasks.iter_mut() {
            if task_ids.contains(&task.id) {
                task.enabled = false;
            }
        }
    }

    // 更新禁用集合
    let mut disabled = cfg.plugins.disabled.clone();
    disabled.retain(|x| x != &id);
    if !enabled {
        disabled.push(id.clone());
    }
    disabled.sort();
    disabled.dedup();
    cfg.plugins.disabled = disabled;

    if let Err(e) = mgr.save(&cfg) {
        return Json(err(format!("{:#}", e)));
    }
    drop(mgr);

    // 立即生效：重建目标池（内部会先同步禁用集合）+ 刷新主目标
    let ready = state.reload_targets(&cfg);

    // 插件能力表随启停收口：
    // - 禁用 → `revoke` 让 ctx 立即失效（在途效果一律不采纳）
    // - 启用 → 重新下发（`issue` 会先失效旧 ctx，再签发新的）
    if enabled {
        if let Some(p) = state.plugins.enhance_plugin_any(&id) {
            p.bind_host(&state);
        }
    } else {
        state.host_effects.revoke(&id);
    }

    state.audit.record(
        "plugin.set_enabled",
        if enabled {
            format!("启用插件 {id}")
        } else if affected.is_empty() {
            format!("禁用插件 {id}")
        } else {
            format!("禁用插件 {id}（同时停用任务：{}）", affected.join("、"))
        },
        true,
        None,
    );

    Json(serde_json::json!({
        "success": true,
        "id": id,
        "enabled": enabled,
        // 级联停用的任务名（前端提示用户）
        "affected_tasks": affected,
        // 目标池里仍就绪的目标数（便于前端立刻反映状态变化）
        "ready_targets": ready,
        "note": if enabled {
            serde_json::Value::Null
        } else {
            serde_json::json!(
                "插件已停用；其代码仍保留在内存中（卸载动态库会让运行中的引用悬空），\
                 重新启用无需重启应用。"
            )
        },
    }))
}

/// `POST /api/plugins/reload`：**热重加载**外置插件（无需重启应用）
///
/// 语义：按当前配置（`plugins.enabled`、目录、公钥映射）重新扫描并装载外置插件：
/// - `enabled=false` → 卸载全部外置插件（丢弃其动态库句柄）；
/// - `enabled=true`  → 重新扫描目录并装载（已有同名插件会被替换）。
///
/// 之后**重建目标池**，使新插件立刻可作为备份目标使用。
///
/// 安全边界（与「禁用」同样的理由）：**不主动 drop 仍被引用的旧插件**。
/// 旧的一组由 `Arc` 引用计数托管 —— 若此刻正在执行备份，它持有的 `Arc<dyn TargetStorage>`
/// 会让旧的动态库继续存活到该次备份结束，因此不会出现悬空指针。
pub(super) async fn plugin_reload(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mgr = state.config.lock().unwrap();
    let cfg = mgr.load().unwrap_or_default();
    
    // 走统一入口：内部会把旧 ctx 全部失效、对新插件重新下发能力表

    // 走统一入口：内部会把旧 ctx 全部失效、对新插件重新下发能力表
    apply_plugin_switch(&state, &cfg);
    let ready = state.reload_targets(&cfg);
    let enabled = crate::plugin::loader::enabled_by_env().unwrap_or(cfg.plugins.enabled);
    let reports = state.plugins.external_reports();
    let loaded = reports.iter().filter(|r| r.loaded).count();

    state.audit.record(
        "plugin.reload",
        if enabled {
            format!("热重加载外置插件：{loaded} 个已加载")
        } else {
            "热卸载全部外置插件".to_string()
        },
        true,
        None,
    );
    Json(serde_json::json!({
        "success": true,
        "enabled": enabled,
        "loaded": loaded,
        "total": reports.len(),
        "ready_targets": ready,
        "reports": reports,
    }))
}

// ── 插件安装（上传 .so + .sig，绑定该文件的公钥）──────────────────────

/// `POST /api/plugins/install`：安装一个外置插件
///
/// body（JSON，**刻意不用 multipart**）：`{ file_name, data_b64, sig_b64, pubkey }`
/// - 用 base64 而不是 multipart：`multer` 未在依赖里，也不想为一个上传引入新依赖；
///   `.so` 通常几百 KB，base64 开销可接受。
///
/// 流程（**先验签、再落盘**）：
/// 1. 校验文件名（必须是 `*.so`，且不含路径分隔符 —— 防目录穿越）；
/// 2. 校验公钥格式（base64 的 32 字节 Ed25519）；
/// 3. 用**该公钥**验签 `.so`；失败即拒绝（绝不写入未通过校验的文件）；
/// 4. 原子写入用户插件目录（`$TRIM_PKGETC/plugins`）的 `.so` 与 `.so.sig`；
/// 5. 把「文件名 → 公钥」写入 `plugins.plugin_pubkeys`（一插件一公钥）。
///
/// 注意：插件在**启动时**装配，故安装后需**重启应用**才会加载（响应里已说明）。
pub(super) async fn plugin_install(
    State(state): State<AppState>,
    Json(body): Json<PluginInstallRequest>,
) -> Json<serde_json::Value> {
    use base64::Engine as _;
    let eng = base64::engine::general_purpose::STANDARD;

    // 1) 文件名：只允许 *.so 且不得携带路径（防目录穿越写任意位置）
    let file_name = body.file_name.trim().to_string();
    if file_name.is_empty() {
        return Json(err("缺少文件名"));
    }
    if !file_name.to_ascii_lowercase().ends_with(".so") {
        return Json(err("只接受 .so 插件文件"));
    }
    if file_name.contains('/') || file_name.contains('\\') || file_name.contains("..") {
        return Json(err("文件名不合法（不得包含路径分隔符）"));
    }

    // 2) 公钥格式
    let pubkey = body.pubkey.trim().to_string();
    if pubkey.is_empty() {
        return Json(err("请提供用于校验该插件的公钥（base64 的 32 字节 Ed25519 公钥）"));
    }
    match eng.decode(&pubkey) {
        Ok(raw) if raw.len() == 32 => {}
        Ok(raw) => {
            return Json(err(format!(
                "公钥长度不对：Ed25519 公钥应为 32 字节，实际 {} 字节",
                raw.len()
            )))
        }
        Err(_) => return Json(err("公钥不是合法的 base64")),
    }

    // 3) 数据与签名
    let data = match eng.decode(body.data_b64.trim()) {
        Ok(d) => d,
        Err(_) => return Json(err("插件内容不是合法的 base64")),
    };
    let sig = match eng.decode(body.sig_b64.trim()) {
        Ok(d) => d,
        Err(_) => return Json(err("签名不是合法的 base64")),
    };
    if data.is_empty() {
        return Json(err("插件内容为空"));
    }
    if sig.len() != 64 {
        return Json(err(format!(
            "Ed25519 签名应为 64 字节，实际 {} 字节",
            sig.len()
        )));
    }

    // 3.1) **先验签**：不通过就绝不落盘（避免磁盘上出现无法加载的残留文件）
    {
        use ring::signature::{UnparsedPublicKey, ED25519};
        let raw = eng.decode(&pubkey).unwrap_or_default();
        let key = UnparsedPublicKey::new(&ED25519, raw);
        if key.verify(&data, &sig).is_err() {
            return Json(err(
                "签名校验未通过：该签名与提供的公钥不匹配（请确认用同一把私钥签名）",
            ));
        }
    }

    // 4) 安装目录：用户插件目录（持久且应用用户可写）
    let Some(etc) = std::env::var("TRIM_PKGETC").ok().filter(|s| !s.is_empty()) else {
        return Json(err("无法确定插件安装目录（缺少 TRIM_PKGETC）"));
    };
    let dir = std::path::Path::new(&etc).join("plugins");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Json(err(format!("创建插件目录失败：{e}")));
    }
    let so_path = dir.join(&file_name);
    let sig_path = crate::plugin::loader::sig_path_of(&so_path);

    // 原子写：先写同目录临时文件再 rename，避免半截文件被当成有效插件
    let write_atomic = |path: &std::path::Path, bytes: &[u8]| -> Result<(), String> {
        let tmp = path.with_extension("tmp-upload");
        std::fs::write(&tmp, bytes).map_err(|e| format!("写入 {} 失败：{e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("替换 {} 失败：{e}", path.display()))?;
        Ok(())
    };
    if let Err(e) = write_atomic(&so_path, &data).and_then(|_| write_atomic(&sig_path, &sig)) {
        return Json(err(e));
    }

    // 5) 记录「该文件 → 公钥」，实现一插件一公钥
    let mgr = state.config.lock().unwrap();
    let mut cfg = mgr.load().unwrap_or_default();
    cfg.plugins.plugin_pubkeys.insert(file_name.clone(), pubkey.clone());
    if let Err(e) = mgr.save(&cfg) {
        return Json(err(format!("保存公钥配置失败：{e:#}")));
    }
    drop(mgr);

    // 安装后**立即热加载**：把刚装的插件装进注册表并重建目标池（无需等重启）
    let (loaded_now, note) = {
        let cfg2 = { state.config.lock().unwrap().load().unwrap_or_default() };
        apply_plugin_switch(&state, &cfg2);
        let ready = state.reload_targets(&cfg2);
        let reports = state.plugins.external_reports();
        let ok = reports.iter().any(|r| r.file == file_name && r.loaded);
        (
            ok,
            if ok {
                format!("已安装并**立即加载**；目标池已重建（就绪目标 {ready} 个）。")
            } else {
                "已安装到插件目录，但本次加载失败（详见下方报告）；可点「重新加载」重试。".to_string()
            },
        )
    };

    state.audit.record(
        "plugin.install",
        format!("安装外置插件 {file_name}（签名校验通过，已绑定公钥；热加载结果：{loaded_now}）"),
        true,
        None,
    );
    Json(serde_json::json!({
        "success": true,
        "file": file_name,
        "dir": dir.to_string_lossy(),
        "path": so_path.to_string_lossy(),
        "note": note,
        "loaded": loaded_now,
    }))
}

/// `POST /api/plugins/:file/uninstall`：卸载一个**外置**插件（删 .so/.sig + 解绑公钥）
///
/// 与 `purge` 的分工：`purge` 清的是插件的**自管数据**；本接口删的是**插件文件本体**。
/// 内置插件不可卸载（它们的代码编译在宿主里）。
///
/// body（可选）：`{"purge_data": true|false}`
/// - 缺省/`false` = **保留**插件的数据目录（重装同一插件时配置与凭据仍在）；
/// - `true` = 一并删除（复用 `purge` 的语义：引用检查 + `remove_plugin_data_dir`）。
///
/// 为什么让调用方显式指定：**卸载**与**删数据**是两件事，后者不可恢复。
/// 前端据此做「无预选单选」，避免用户不假思索地把凭据一起删掉（评审决策 3）。
pub(super) async fn plugin_uninstall(
    State(state): State<AppState>,
    axum::extract::Path(file): axum::extract::Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Json<serde_json::Value> {
    let purge_data = body
        .as_ref()
        .and_then(|Json(v)| v.get("purge_data"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let file = file.trim().to_string();
    if file.is_empty() || file.contains('/') || file.contains("..") {
        return Json(err("文件名不合法"));
    }

    // 只允许删除**外置**插件（随包/内置的代码在宿主二进制里，删文件没有意义且危险）
    let Some(etc) = std::env::var("TRIM_PKGETC").ok().filter(|s| !s.is_empty()) else {
        return Json(err("无法确定插件目录（缺少 TRIM_PKGETC）"));
    };
    let dir = std::path::Path::new(&etc).join("plugins");
    let so_path = dir.join(&file);
    if !so_path.is_file() {
        return Json(err(format!(
            "该文件不在用户插件目录中，无法卸载：{}（内置/随包插件不能卸载）",
            so_path.display()
        )));
    }

    let sig = crate::plugin::loader::sig_path_of(&so_path);
    // 数据目录按**插件 id** 命名，而本接口拿到的是**文件名** —— 必须在删文件
    // **之前**从注册表的诊断报告里取出映射，否则删完就查不到了。
    let plugin_id = state
        .plugins
        .external_reports()
        .into_iter()
        .find(|r| r.file == file)
        .and_then(|r| r.id);
    let mut removed = Vec::new();
    for p in [&so_path, &sig] {
        if p.is_file() {
            if let Err(e) = std::fs::remove_file(p) {
                return Json(err(format!("删除 {} 失败：{e}", p.display())));
            }
            removed.push(p.to_string_lossy().into_owned());
        }
    }

    let mgr = state.config.lock().unwrap();
    let mut cfg = mgr.load().unwrap_or_default();
    let untied = cfg.plugins.plugin_pubkeys.remove(&file).is_some();
    if let Err(e) = mgr.save(&cfg) {
        return Json(err(format!("保存配置失败：{e:#}")));
    }
    drop(mgr);

    // 按用户选择决定数据去留（评审决策 3：卸载与删数据是两件事）
    let data_removed = if purge_data {
        match &plugin_id {
            Some(id) => {
                state.plugins.call_destroy(id);
                state.host_effects.revoke(id);
                remove_plugin_data_dir(&state, id)
            }
            // 拿不到 id（如插件本就未加载）：明确告知无法删数据，而不是静默跳过
            None => false,
        }
    } else {
        false
    };

    // 卸载后**立即热重加载**：让列表与目标池反映删除后的状态（无需重启）
    {
        let cfg2 = { state.config.lock().unwrap().load().unwrap_or_default() };
        apply_plugin_switch(&state, &cfg2);
        state.reload_targets(&cfg2);
    }

    state.audit.record(
        "plugin.uninstall",
        format!(
            "卸载外置插件 {file}（解绑公钥：{untied}；{}数据目录）",
            if purge_data {
                if data_removed {
                    "已删除"
                } else {
                    "未能删除"
                }
            } else {
                "保留"
            }
        ),
        true,
        None,
    );
    Json(serde_json::json!({
        "success": true,
        "file": file,
        "removed": removed,
        "unbound_pubkey": untied,
        "data_purged": purge_data,
        "data_removed": data_removed,
        "note": if purge_data {
            if data_removed {
                "已删除插件文件与数据目录，并重新加载插件列表。"
            } else {
                "已删除插件文件；数据目录未能删除（插件未加载时无法定位其数据目录，可稍后用「清除数据」）。"
            }
        } else {
            "已删除插件文件；配置数据已保留（重装同一插件后仍可用）。"
        },
    }))
}

/// 删除某插件的私有数据目录（卸载清除用）；返回是否真的删除了东西
///
/// 宿主不再代存插件配置（ADR-021），插件把配置写在自己的 `own_data_dir` ——
/// 所以「卸载清除」的实际动作就是删掉这个目录。
///
/// **只删插件自己的子目录**（`<plugins_dir>/<id>`），且校验 id 不含路径分隔符，
/// 避免 `../` 之类的 id 删到别处。
pub(super) fn remove_plugin_data_dir(state: &AppState, plugin_id: &str) -> bool {
    if plugin_id.is_empty()
        || plugin_id.contains('/')
        || plugin_id.contains('\\')
        || plugin_id.contains("..")
    {
        tracing::warn!(plugin = %plugin_id, "插件 id 含路径分隔符，拒绝删除其数据目录");
        return false;
    }
    let dir = state.var_dir.join("plugins").join(plugin_id);
    if !dir.is_dir() {
        return false;
    }
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {
            tracing::info!(plugin = %plugin_id, dir = %dir.display(), "已删除插件私有数据目录");
            true
        }
        Err(e) => {
            tracing::warn!(plugin = %plugin_id, dir = %dir.display(), err = %e, "删除插件数据目录失败");
            false
        }
    }
}

/// 让「外置插件加载」开关**立即生效**（无需重启）
///
/// 以前这个开关只落盘、要求重启 —— 因为插件在启动时装配。现在注册表支持热替换：
/// - 打开 → 扫描目录并装载外置插件；
/// - 关闭 → 卸载全部外置插件（丢弃动态库句柄）。
///
/// 注意：**卸载不等于立即释放内存**。若此刻有备份正在使用某插件，
/// 那个 `Arc<dyn TargetStorage>` 会让对应的动态库继续存活到本次备份结束
/// （引用计数保证内存安全，不会出现悬空指针 / 崩溃）。
pub(super) fn apply_plugin_switch(state: &AppState, cfg: &crate::infra::config::AppConfig) {
    let enabled = crate::plugin::loader::enabled_by_env().unwrap_or(cfg.plugins.enabled);
    if enabled {
        let dirs = crate::plugin::loader::plugin_dirs(cfg.plugins.dir.as_deref());
        state
            .plugins
            .load_external(&dirs, &cfg.plugins.plugin_pubkeys, &cfg.plugins.pubkeys);
        tracing::info!(enabled, "外置插件开关已热生效：已重新装载");
    } else {
        state.plugins.unload_external();
        tracing::info!("外置插件开关已热生效：已卸载全部外置插件");
    }

    // 插件对象整组换新 ⇒ 旧 ctx 的持有者已经不在服务了，新的一组需要重新绑定。
    // 先对所有当前 id 失效（覆盖「已被删除的插件」的残留 ctx），再下发新表。
    // `host_bind` 幂等：内部 `issue` 会先失效旧 ctx 再签发。
    let ids: Vec<String> = state
        .plugins
        .plugin_ids()
        .into_iter()
        .chain(
            // 卸载后插件可能已从列表消失，但它的 ctx 还在登记表里 —— `revoke` 全部
            // 已登记插件 id，确保旧 ctx 不会被误用。
            state.host_effects.known_plugin_ids(),
        )
        .collect();
    for id in ids {
        state.host_effects.revoke(&id);
    }
    let bound = state.plugins.bind_all_host(state);
    tracing::debug!(bound, "插件热重载后已重新下发宿主能力表");
}
