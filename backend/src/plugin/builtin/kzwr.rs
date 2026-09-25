//! 内置增强插件：kzwr REST 账号增强（账号信息 / 存储空间与预警 / 云端回收站）
//!
//! **非备份通道**：备份与恢复始终走 WebDAV 目标插件（ADR-009 / ADR-013）。
//! 本文件由 `http/routes.rs` 整体迁出：插件自带路由（挂在 `/api/p/kzwr/`）
//! 与生命周期钩子（启动自检 / 周期巡检 / 备份后动作），核心不再含 kzwr 逻辑。

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::http::routes::{human_bytes, raise_alert_once, webdav_username};
use crate::infra::config::AppConfig;
use crate::plugin::api::{CheckOutcome, EnhanceCaps, EnhancePlugin, PluginKind, PluginMeta};
use crate::AppState;

/// kzwr 增强插件
pub struct KzwrPlugin;

#[async_trait::async_trait]
impl EnhancePlugin for KzwrPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            id: "kzwr".to_string(),
            name: "酷族账号增强".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            kind: PluginKind::Enhance,
            builtin: true,
            description: "账号信息、存储空间与预警、云端回收站清理（需 access-token）".to_string(),
        }
    }

    fn caps(&self) -> EnhanceCaps {
        EnhanceCaps {
            account: true,
            quota: true,
            recycle_bin: true,
            notify: false,
        }
    }

    fn available(&self, cfg: &AppConfig) -> bool {
        cfg.kzwr.access_token_enc.is_some()
    }

    /// 插件自带路由（核心统一挂在 `/api/p/<id>` 下）
    fn routes(&self) -> axum::Router<AppState> {
        axum::Router::new()
            .route("/user", axum::routing::get(kzwr_user))
            .route("/token", axum::routing::post(kzwr_token_save))
            .route("/trash/empty", axum::routing::post(kzwr_trash_empty))
    }

    /// 启动自检：token 失效则告警
    async fn on_startup(&self, state: &AppState) {
        check_kzwr_token(state).await;
    }

    /// 周期巡检：空间预警（由 main.rs 的定时任务驱动）
    async fn patrol(&self, state: &AppState) {
        check_kzwr_quota(state).await;
    }

    /// 备份后动作：按保留策略清空云端回收站（返回清理计数）
    async fn after_backup(&self, state: &AppState) -> Option<u64> {
        Some(empty_recycle_bin_if_configured(state).await as u64)
    }

    /// 配置变更后刷新内存中的 access-token（热更新，无需重启）
    async fn reload(&self, state: &AppState) {
        match kzwr_token_of(state) {
            Some(t) => state.kzwr.set_token(t),
            None => state.kzwr.clear_token(),
        }
    }

    /// 一键体检：access-token 与云端空间
    async fn health_check(&self, state: &AppState, cfg: &AppConfig) -> Vec<CheckOutcome> {
        let mut out = Vec::new();
        let Some(token) = kzwr_token_of(state) else {
            out.push(CheckOutcome {
                key: "kzwr".to_string(),
                title: "增强功能".to_string(),
                status: "warn".to_string(),
                detail: "未配置 access-token（可选）：无存储空间信息与回收站清理".to_string(),
                hint: Some(
                    "如需存储空间预警/清空回收站：浏览器登录酷族 → F12 → Application → Cookies → www.kzwr.com → 复制 access-token 填入设置页"
                        .to_string(),
                ),
            });
            return out;
        };
        state.kzwr.set_token(token);
        match state.kzwr.get_member().await {
            Ok(v) if !member_is_login(&v) => {
                restore_saved_kzwr_token(state);
                raise_alert_once(
                    state,
                    crate::domain::alerts::AlertLevel::Warn,
                    crate::domain::alerts::AlertSource::Kzwr,
                    KZWR_TOKEN_INVALID.to_string(),
                );
                out.push(CheckOutcome {
                    key: "kzwr".to_string(),
                    title: "增强功能".to_string(),
                    status: "fail".to_string(),
                    detail: KZWR_TOKEN_INVALID.to_string(),
                    hint: Some("重新从浏览器 Cookie 复制 access-token".to_string()),
                });
            }
            Ok(v) => {
                let data = v.get("data").cloned().unwrap_or_default();
                let num = |key: &str| data.get(key).and_then(|x| x.as_u64()).unwrap_or(0);
                let total = num("total").max(num("capacity"));
                let used = num("use");
                let pct = if total > 0 {
                    (used as f64 / total as f64 * 100.0).round() as u64
                } else {
                    0
                };
                let threshold = cfg.kzwr.quota_warn_percent;
                let over = total > 0 && threshold > 0 && pct >= threshold;
                maybe_warn_quota(state, total, used);
                out.push(CheckOutcome {
                    key: "kzwr".to_string(),
                    title: "增强功能".to_string(),
                    status: "ok".to_string(),
                    detail: format!("access-token 有效；空间已用 {}%", pct),
                    hint: None,
                });
                out.push(CheckOutcome {
                    key: "quota".to_string(),
                    title: "云端空间".to_string(),
                    status: if over { "warn" } else { "ok" }.to_string(),
                    detail: format!(
                        "{} / {}（{}%）{}",
                        human_bytes(used),
                        human_bytes(total),
                        pct,
                        if threshold > 0 {
                            format!("，预警阈值 {}%", threshold)
                        } else {
                            "，未启用预警".to_string()
                        }
                    ),
                    hint: if over {
                        Some("空间接近上限，建议清理回收站或扩容".to_string())
                    } else {
                        None
                    },
                });
            }
            Err(e) => out.push(CheckOutcome {
                key: "kzwr".to_string(),
                title: "增强功能".to_string(),
                status: "warn".to_string(),
                detail: format!("账号信息获取失败（网络问题？）：{e}"),
                hint: None,
            }),
        }
        out
    }
}

// ── 以下为 kzwr REST 实现（自 routes.rs 迁入，逻辑未改）──────────────────

// ===== 账号一致性 =====

/// 账号一致性检查：WebDAV 账号 vs 已配置的 access-token 所属账号
///
/// 返回 `Some(提醒)` 表示不一致（同时生成告警），`None` 表示一致/无法判定/未配置 token。
pub(crate) async fn check_account_consistency(state: &AppState) -> Option<String> {
    let user = webdav_username(state)?;
    let token = kzwr_token_of(state)?;
    state.kzwr.set_token(token);
    match state.kzwr.get_member().await {
        Ok(v) if member_is_login(&v) => {
            let member = v.get("data").cloned().unwrap_or_default();
            let mismatch = kzwr_account_mismatch(&user, &member);
            if let Some(msg) = &mismatch {
                raise_alert_once(
                    state,
                    crate::domain::alerts::AlertLevel::Warn,
                    crate::domain::alerts::AlertSource::Config,
                    msg.clone(),
                );
            }
            mismatch
        }
        _ => None,
    }
}

// ===== token 与登录态 =====

/// 读取配置中的 kzwr access-token（内部锁 config；同步、不跨 await 持有）
fn kzwr_token_of(state: &AppState) -> Option<String> {
    state.config.lock().unwrap().kzwr_token().ok().flatten()
}

/// 用配置里已保存的 token 重置内存副本（token 变更校验失败时回滚用）
fn restore_saved_kzwr_token(state: &AppState) {
    match kzwr_token_of(state) {
        Some(t) => state.kzwr.set_token(t),
        None => state.kzwr.clear_token(),
    }
}

/// 判定 `/api/v2/member` 是否处于登录态。
///
/// 实测：access-token 无效/过期时官方 API 仍返回 **HTTP 200**，只是
/// `data.isLogin = false` / `uid = -1` / 容量为 0，因此不能只看 HTTP 状态码。
fn member_is_login(v: &serde_json::Value) -> bool {
    v.get("data")
        .and_then(|d| d.get("isLogin"))
        .and_then(|x| x.as_bool())
        .unwrap_or(true)
}

/// token 无效/过期时的统一提示
const KZWR_TOKEN_INVALID: &str =
    "access-token 无效或已过期（酷族返回未登录状态），请在浏览器重新登录后从 Cookie 复制";

// ===== 空间预警与巡检 =====

/// 空间预警告警文案前缀（用于「占用回落」时自动消解同前缀的告警）
const QUOTA_ALERT_PREFIX: &str = "云端存储空间已用";

/// 云端空间达到阈值时告警（同文案去重，避免每次刷新都推送）
///
/// 占用回落到阈值以下时，把之前的空间预警**自动移除**（消息提醒里不留旧账）。
fn maybe_warn_quota(state: &AppState, total: u64, used: u64) {
    if total == 0 {
        return;
    }
    let threshold = state
        .config
        .lock()
        .unwrap()
        .load()
        .map(|c| c.kzwr.quota_warn_percent)
        .unwrap_or(85);
    if threshold == 0 || threshold > 100 {
        return;
    }
    let pct = (used as f64 / total as f64 * 100.0).round() as u64;
    if pct >= threshold {
        raise_alert_once(
            state,
            crate::domain::alerts::AlertLevel::Warn,
            crate::domain::alerts::AlertSource::Kzwr,
            format!(
                "{} {}%（{} / {}），达到预警阈值 {}%，请及时清理以免备份失败",
                QUOTA_ALERT_PREFIX,
                pct,
                human_bytes(used),
                human_bytes(total),
                threshold
            ),
        );
    } else {
        let removed = state.alerts.remove_where(|a| {
            a.source == crate::domain::alerts::AlertSource::Kzwr
                && a.message.starts_with(QUOTA_ALERT_PREFIX)
        });
        if removed > 0 {
            tracing::info!(removed, "空间占用已回落至阈值以下，空间预警自动解除");
        }
    }
}

/// 判断 kzwr API 账号与 WebDAV 账号是否一致（邮箱/昵称任一匹配即视为一致）
///
/// 返回 `Some(提醒文案)` 表示不一致，`None` 表示一致或无法判定。
fn kzwr_account_mismatch(webdav_user: &str, member_data: &serde_json::Value) -> Option<String> {
    let w = webdav_user.trim().to_lowercase();
    if w.is_empty() {
        return None;
    }
    let get = |k: &str| {
        member_data
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_lowercase()
    };
    let email = get("email");
    let name = get("name");
    if email.is_empty() && name.is_empty() {
        return None; // 拿不到账号信息，无法判定
    }
    let hit = |a: &str| !a.is_empty() && (a == w || a.contains(&w) || w.contains(a));
    if hit(&email) || hit(&name) {
        return None;
    }
    Some(format!(
        "账号不一致：WebDAV 账号「{}」与 access-token 所属账号「{}」不是同一个；\
         两者指向同一酷族账号时，备份数据与增强功能才会落在同一份数据上，建议改为一致",
        webdav_user.trim(),
        if email.is_empty() { name } else { email }
    ))
}

/// 启动/按需校验 kzwr access-token：失效则告警提醒用户重新从 Cookie 获取
pub async fn check_kzwr_token(state: &AppState) {
    let Some(token) = kzwr_token_of(state) else {
        return;
    };
    state.kzwr.set_token(token);
    match state.kzwr.get_member().await {
        Ok(v) if !member_is_login(&v) => {
            restore_saved_kzwr_token(state);
            raise_alert_once(
                state,
                crate::domain::alerts::AlertLevel::Warn,
                crate::domain::alerts::AlertSource::Kzwr,
                KZWR_TOKEN_INVALID.to_string(),
            );
            tracing::warn!("kzwr access-token 已失效，已生成告警提醒用户重新获取");
        }
        Ok(_) => tracing::info!("kzwr access-token 校验通过（增强功能可用）"),
        Err(e) => tracing::warn!(err = %e, "kzwr access-token 校验请求失败（网络问题？）"),
    }
}

/// 后台空间巡检：拉取账号空间并评估是否需要「空间预警」告警。
///
/// 由 main.rs 的定时任务周期调用（**不依赖用户打开界面**），备份成功后也会触发一次；
/// 占用回落到阈值以下时同一条预警会自动消解。
pub async fn check_kzwr_quota(state: &AppState) {
    let Some(token) = kzwr_token_of(state) else {
        return;
    };
    state.kzwr.set_token(token);
    match state.kzwr.get_member().await {
        Ok(v) if !member_is_login(&v) => {
            restore_saved_kzwr_token(state);
            raise_alert_once(
                state,
                crate::domain::alerts::AlertLevel::Warn,
                crate::domain::alerts::AlertSource::Kzwr,
                KZWR_TOKEN_INVALID.to_string(),
            );
        }
        Ok(v) => {
            let data = v.get("data").cloned().unwrap_or_default();
            let num = |key: &str| data.get(key).and_then(|x| x.as_u64()).unwrap_or(0);
            let total = num("total").max(num("capacity"));
            let used = num("use");
            maybe_warn_quota(state, total, used);
        }
        Err(e) => tracing::warn!(err = %e, "空间巡检请求失败（网络问题？）"),
    }
}

// ===== 数据模型（账号信息）=====

/// kzwr 账号信息响应（含存储空间与账号详情）
#[derive(Serialize, Default)]
pub struct KzwrUserResponse {
    /// 是否已配置 access-token
    pub configured: bool,
    pub email: Option<String>,
    pub name: Option<String>,
    pub avatar: Option<String>,
    /// 套餐名
    pub plan: Option<String>,
    /// 总容量（字节）
    pub total: u64,
    /// 已用容量（字节）
    pub used: u64,
    /// 已用百分比（如 "8.38%"）
    pub percentage: Option<String>,
    /// 用户 ID
    pub uid: Option<i64>,
    /// 单文件大小上限（字节）
    pub max_file_size: u64,
    /// 是否正在升级
    pub upgrading: bool,
    /// 地区
    pub country: Option<String>,
    /// 登录 IP
    pub ip: Option<String>,
    /// 语言
    pub language: Option<String>,
    /// 是否属于家庭组
    pub in_family: bool,
    /// 站内公告（非空时 UI 可提示）
    pub announcement: Option<String>,
    pub error: Option<String>,
}

// ===== 数据模型、回收站与 HTTP handler =====

/// 保存 / 清除 access-token 请求（空串 = 清除）
#[derive(Deserialize, Default)]
pub struct KzwrTokenRequest {
    #[serde(default)]
    pub access_token: String,
}

/// access-token 保存结果
#[derive(Serialize, Default)]
pub struct KzwrTokenResponse {
    pub success: bool,
    pub configured: bool,
    /// 账号一致性提醒（如 API 账号与 WebDAV 账号不同）
    pub warning: Option<String>,
    pub error: Option<String>,
}

/// 清空回收站结果
#[derive(Serialize, Default)]
pub struct KzwrTrashResponse {
    /// 已永久删除的条目数
    pub emptied: usize,
    /// 因不满足年龄门槛而保留的条目数
    pub kept: usize,
    /// 无法解析删除时间的条目数（保守起见保留）
    pub unknown_age: usize,
    /// 回收站总占用（字节；解析不到则为已知部分之和）
    pub total_bytes: u64,
    /// 未执行清空时的原因说明
    pub reason: Option<String>,
    pub error: Option<String>,
}

/// 取出响应里的回收站条目数组
///
/// 字段名随版本而异，逐个尝试常见位置（`data.items` / `data.list` /
/// `data.records` / `data.files` / 根 `items` / `data` 本身为数组）。
fn trash_items(v: &serde_json::Value) -> Vec<serde_json::Value> {
    for (obj, key) in [
        (v.get("data"), "items"),
        (v.get("data"), "list"),
        (v.get("data"), "records"),
        (v.get("data"), "files"),
        (Some(v), "items"),
    ] {
        if let Some(arr) = obj.and_then(|d| d.get(key)).and_then(|i| i.as_array()) {
            if !arr.is_empty() {
                return arr.clone();
            }
        }
    }
    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        return arr.clone();
    }
    Vec::new()
}

/// 解析回收站条目大小（字节）：字段名随不同版本而异，逐个尝试
/// （实测 kzwr 返回 `length`；其余为兼容候选）
fn trash_item_size(it: &serde_json::Value) -> Option<u64> {
    for k in ["length", "size", "fileSize", "sizeBytes", "bytes", "totalSize"] {
        let Some(v) = it.get(k) else { continue };
        if let Some(n) = v.as_u64() {
            return Some(n);
        }
        if let Some(s) = v.as_str() {
            if let Ok(n) = s.parse::<u64>() {
                return Some(n);
            }
        }
    }
    None
}

/// 解析回收站条目的删除时间（毫秒时间戳）：兼容秒级数值与 RFC3339 字符串
fn trash_item_deleted_ms(it: &serde_json::Value) -> Option<i64> {
    const KEYS: [&str; 8] = [
        "deletedDate",
        "deleteTime",
        "deletedAt",
        "deleteAt",
        "deleteDate",
        "updateTime",
        "time",
        "createTime",
    ];
    for k in KEYS {
        let Some(v) = it.get(k) else { continue };
        if let Some(n) = v.as_i64() {
            return Some(if n < 10_000_000_000 { n * 1000 } else { n });
        }
        if let Some(s) = v.as_str() {
            if let Ok(n) = s.parse::<i64>() {
                return Some(if n < 10_000_000_000 { n * 1000 } else { n });
            }
            if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
                return Some(d.timestamp_millis());
            }
            // 无时区的裸日期时间（如 "2026-09-20 12:34:56"）：kzwr 返回的是
            // 服务器本地时间，按**宿主本地时区**解释（与调度器口径一致），
            // 否则年龄门槛会偏差一个时区偏移量
            if let Some(ms) = parse_naive_local_ms(s) {
                return Some(ms);
            }
        }
    }
    None
}

/// 解析无时区的日期时间字符串，按宿主本地时区解释为毫秒时间戳
fn parse_naive_local_ms(s: &str) -> Option<i64> {
    use chrono::TimeZone;
    if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return chrono::Local
            .from_local_datetime(&nd)
            .single()
            .map(|d| d.timestamp_millis());
    }
    if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return chrono::Local
            .from_local_datetime(&nd)
            .single()
            .map(|d| d.timestamp_millis());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let nd = d.and_hms_opt(0, 0, 0)?;
        return chrono::Local
            .from_local_datetime(&nd)
            .single()
            .map(|d| d.timestamp_millis());
    }
    None
}

/// 清空回收站的结果明细
#[derive(Debug, Default)]
struct TrashOutcome {
    emptied: usize,
    kept: usize,
    unknown_age: usize,
    total_bytes: u64,
    reason: Option<String>,
}

/// 清理回收站（支持两种门槛）
///
/// - `max_gb`：回收站（首页采样）占用低于该 GB 数时**整体跳过**（0 = 不限制）
/// - `min_age_days`：只删除删除时间早于该天数的条目（0 = 不限制）；
///   **无法解析时间的条目一律保留**（永久删除不可逆，宁可少删）
async fn empty_recycle_bin_gated(
    client: &crate::infra::kzwr_api::client::KzwrClient,
    max_gb: u64,
    min_age_days: u64,
) -> Result<TrashOutcome, String> {
    let mut out = TrashOutcome::default();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    for round in 0..200 {
        let v = client.get_trash(1).await.map_err(|e| format!("{e}"))?;
        if round == 0 {
            // debug 日志记录首页原始响应（结构不确定，便于排查「没清理」类问题）
            tracing::debug!("回收站首页原始响应: {}", v.to_string().chars().take(1200).collect::<String>());
        }
        let items = trash_items(&v);
        if items.is_empty() {
            if round == 0 {
                out.reason = Some("回收站为空".to_string());
            }
            return Ok(out);
        }

        // 仅首轮做占用门槛判断（首页采样；后续轮次已在执行清理中）
        if round == 0 {
            out.total_bytes = items.iter().filter_map(trash_item_size).sum();
            if max_gb > 0 && out.total_bytes < max_gb * 1024 * 1024 * 1024 {
                out.kept = items.len();
                out.reason = Some(format!(
                    "回收站占用 {} MB 不足 {} GB，本次未清空",
                    out.total_bytes / (1024 * 1024),
                    max_gb
                ));
                return Ok(out);
            }
        }

        // 年龄门槛：解析不出时间的条目保守保留
        let mut deletable: Vec<serde_json::Value> = Vec::new();
        for it in &items {
            match trash_item_deleted_ms(it) {
                Some(ts) => {
                    let age_days = (now_ms - ts).max(0) / 86_400_000;
                    if min_age_days == 0 || age_days as u64 >= min_age_days {
                        deletable.push(it.clone());
                    } else {
                        out.kept += 1;
                    }
                }
                None => {
                    if min_age_days == 0 {
                        deletable.push(it.clone());
                    } else {
                        out.unknown_age += 1;
                        out.kept += 1;
                    }
                }
            }
        }
        if deletable.is_empty() {
            out.reason = Some(if out.unknown_age > 0 {
                format!(
                    "本页无可删除条目（{} 项无法解析删除时间，已保守保留）",
                    out.unknown_age
                )
            } else {
                "剩余条目均未达到最小保留天数".to_string()
            });
            return Ok(out);
        }
        let n = deletable.len();
        client
            .delete_trash_items(&deletable)
            .await
            .map_err(|e| format!("删除回收站条目失败: {e}"))?;
        out.emptied += n;
    }
    Ok(out)
}

/// 保留策略联动：按配置门槛清理回收站（未配置 token 或失败仅记日志，不影响备份结果）
async fn empty_recycle_bin_if_configured(state: &AppState) -> usize {
    let Some(token) = kzwr_token_of(state) else {
        tracing::warn!("保留策略要求清空回收站，但未配置 kzwr access-token，已跳过");
        return 0;
    };
    let (max_gb, min_age_days) = {
        let cfg = state.config.lock().unwrap().load().unwrap_or_default();
        (
            cfg.backup.retention.recycle_max_gb,
            cfg.backup.retention.recycle_min_age_days,
        )
    };
    state.kzwr.set_token(token);
    match empty_recycle_bin_gated(&state.kzwr, max_gb, min_age_days).await {
        Ok(o) => {
            tracing::info!(
                emptied = o.emptied,
                kept = o.kept,
                reason = o.reason.clone().unwrap_or_default(),
                "保留策略：回收站清理完成"
            );
            // 结果留痕到审计（含未清理原因），用户可从审计页看到「为什么没清理」
            let reason = o.reason.clone().unwrap_or_default();
            let summary = if o.emptied > 0 {
                format!(
                    "自动清空回收站：清理 {} 项、保留 {} 项{}",
                    o.emptied,
                    o.kept,
                    if reason.is_empty() { String::new() } else { format!("；{reason}") }
                )
            } else {
                format!(
                    "自动清空回收站：未删除任何条目（保留 {} 项）{}",
                    o.kept,
                    if reason.is_empty() { String::new() } else { format!("：{reason}") }
                )
            };
            state.audit.record("kzwr.trash.auto", summary, true, None);
            o.emptied
        }
        Err(e) => {
            tracing::warn!("保留策略：清空回收站失败：{}", e);
            state.audit.record(
                "kzwr.trash.auto",
                format!("自动清空回收站失败：{e}"),
                false,
                None,
            );
            raise_alert_once(
                state,
                crate::domain::alerts::AlertLevel::Warn,
                crate::domain::alerts::AlertSource::Kzwr,
                format!("备份后自动清空回收站失败：{e}"),
            );
            0
        }
    }
}

/// kzwr 增强：账号信息（存储空间 / 套餐 / 邮箱）
async fn kzwr_user(State(state): State<AppState>) -> Json<KzwrUserResponse> {
    let Some(token) = kzwr_token_of(&state) else {
        return Json(KzwrUserResponse {
            configured: false,
            error: Some(
                "未配置 access-token：请在浏览器登录酷族后，从开发者工具 → 应用（Application）→ Cookies → www.kzwr.com 复制 access-token 填入"
                    .to_string(),
            ),
            ..Default::default()
        });
    };
    state.kzwr.set_token(token);
    match state.kzwr.get_member().await {
        Ok(v) if !member_is_login(&v) => {
            restore_saved_kzwr_token(&state);
            // access-token 失效：告警提醒用户重新从 Cookie 获取（同文案去重）
            raise_alert_once(
                &state,
                crate::domain::alerts::AlertLevel::Warn,
                crate::domain::alerts::AlertSource::Kzwr,
                KZWR_TOKEN_INVALID.to_string(),
            );
            Json(KzwrUserResponse {
                configured: kzwr_token_of(&state).is_some(),
                error: Some(KZWR_TOKEN_INVALID.to_string()),
                ..Default::default()
            })
        }
        Ok(v) => {
            let data = v.get("data").cloned().unwrap_or_default();
            let num = |key: &str| data.get(key).and_then(|x| x.as_u64()).unwrap_or(0);
            let opt_str =
                |key: &str| data.get(key).and_then(|x| x.as_str()).map(|s| s.to_string());
            let total = num("total").max(num("capacity"));
            let used = num("use");
            // 空间预警：达到阈值时生成告警（去重）
            maybe_warn_quota(&state, total, used);
            Json(KzwrUserResponse {
                configured: true,
                email: opt_str("email"),
                name: opt_str("name"),
                avatar: opt_str("avatar"),
                plan: opt_str("plan"),
                total,
                used,
                percentage: opt_str("percentage"),
                uid: data.get("uid").and_then(|x| x.as_i64()),
                max_file_size: num("maxFileSize"),
                upgrading: data.get("upgrading").and_then(|x| x.as_i64()).unwrap_or(0) != 0,
                country: opt_str("country"),
                ip: opt_str("ip"),
                language: opt_str("language"),
                in_family: data.get("inFamily").and_then(|x| x.as_bool()).unwrap_or(false),
                announcement: opt_str("announcement").filter(|s| !s.trim().is_empty()),
                error: None,
            })
        }
        Err(e) => {
            // 请求本身失败：回滚成配置里的 token（若有），避免内存副本被污染
            restore_saved_kzwr_token(&state);
            Json(KzwrUserResponse {
                configured: kzwr_token_of(&state).is_some(),
                error: Some(format!("获取账号信息失败：{e}")),
                ..Default::default()
            })
        }
    }
}

/// kzwr 增强：保存 / 清除 access-token（保存前先实测，避免存入无效 token）
async fn kzwr_token_save(
    State(state): State<AppState>,
    Json(body): Json<KzwrTokenRequest>,
) -> Json<KzwrTokenResponse> {
    let token = body.access_token.trim().to_string();

    if token.is_empty() {
        state.kzwr.clear_token();
        let saved = state.config.lock().unwrap().save_kzwr_token("");
        return Json(match saved {
            Ok(_) => {
                state
                    .audit
                    .record("kzwr.token.clear", "清除 access-token", true, None);
                KzwrTokenResponse {
                    success: true,
                    configured: false,
                    warning: None,
                    error: None,
                }
            }
            Err(e) => KzwrTokenResponse {
                success: false,
                configured: false,
                warning: None,
                error: Some(format!("清除失败: {:#}", e)),
            },
        });
    }

    // 先热更新内存副本并实测；通过后才落盘
    state.kzwr.set_token(token.clone());
    match state.kzwr.get_member().await {
        Ok(v) if member_is_login(&v) => {
            let member = v.get("data").cloned().unwrap_or_default();
            // 账号一致性：API 账号应与 WebDAV 账号一致，否则提醒用户修改
            let warning = webdav_username(&state)
                .and_then(|u| kzwr_account_mismatch(&u, &member));
            let saved = state.config.lock().unwrap().save_kzwr_token(&token);
            Json(match saved {
                Ok(_) => {
                    state.audit.record(
                        "kzwr.token.save",
                        format!(
                            "保存 access-token（账号 {}）{}",
                            member
                                .get("email")
                                .and_then(|x| x.as_str())
                                .or_else(|| member.get("name").and_then(|x| x.as_str()))
                                .unwrap_or("未知"),
                            if warning.is_some() { "；账号与 WebDAV 不一致" } else { "" }
                        ),
                        true,
                        None,
                    );
                    KzwrTokenResponse {
                        success: true,
                        configured: true,
                        warning,
                        error: None,
                    }
                }
                Err(e) => KzwrTokenResponse {
                    success: false,
                    configured: true,
                    warning,
                    error: Some(format!("保存失败: {:#}", e)),
                },
            })
        }
        Ok(_) => {
            // 无效 token：官方接口仍返回 200，只能靠 isLogin 判定
            restore_saved_kzwr_token(&state);
            raise_alert_once(
                &state,
                crate::domain::alerts::AlertLevel::Warn,
                crate::domain::alerts::AlertSource::Kzwr,
                KZWR_TOKEN_INVALID.to_string(),
            );
            Json(KzwrTokenResponse {
                success: false,
                configured: kzwr_token_of(&state).is_some(),
                warning: None,
                error: Some(KZWR_TOKEN_INVALID.to_string()),
            })
        }
        Err(e) => {
            restore_saved_kzwr_token(&state);
            Json(KzwrTokenResponse {
                success: false,
                configured: kzwr_token_of(&state).is_some(),
                warning: None,
                error: Some(format!(
                    "access-token 校验未通过（请确认网络可达酷族后重试）：{e}"
                )),
            })
        }
    }
}

/// kzwr 增强：手动清空回收站
///
/// 显式操作 → **忽略保留策略门槛**（占用/年龄），直接清空；物理删除不可恢复，
/// 前端有二次确认。
async fn kzwr_trash_empty(State(state): State<AppState>) -> Json<KzwrTrashResponse> {
    let Some(token) = kzwr_token_of(&state) else {
        return Json(KzwrTrashResponse {
            error: Some("未配置 access-token，无法操作回收站".to_string()),
            ..Default::default()
        });
    };
    state.kzwr.set_token(token);

    // 先确认登录态：无效 token 时官方接口会返回空列表，避免误报「回收站为空」
    match state.kzwr.get_member().await {
        Ok(v) if !member_is_login(&v) => {
            restore_saved_kzwr_token(&state);
            raise_alert_once(
                &state,
                crate::domain::alerts::AlertLevel::Warn,
                crate::domain::alerts::AlertSource::Kzwr,
                KZWR_TOKEN_INVALID.to_string(),
            );
            return Json(KzwrTrashResponse {
                error: Some(KZWR_TOKEN_INVALID.to_string()),
                ..Default::default()
            });
        }
        Ok(_) => {}
        Err(e) => {
            return Json(KzwrTrashResponse {
                error: Some(format!("校验 access-token 失败：{e}")),
                ..Default::default()
            })
        }
    }

    match empty_recycle_bin_gated(&state.kzwr, 0, 0).await {
        Ok(o) => {
            state.audit.record(
                "kzwr.trash.empty",
                format!(
                    "手动清空回收站：删除 {} 项（占用 {}）",
                    o.emptied,
                    human_bytes(o.total_bytes)
                ),
                true,
                None,
            );
            Json(KzwrTrashResponse {
                emptied: o.emptied,
                kept: o.kept,
                unknown_age: o.unknown_age,
                total_bytes: o.total_bytes,
                reason: o.reason,
                error: None,
            })
        }
        Err(e) => {
            state.audit.record(
                "kzwr.trash.empty",
                format!("清空回收站失败：{e}"),
                false,
                None,
            );
            Json(KzwrTrashResponse {
                error: Some(e),
                ..Default::default()
            })
        }
    }
}
