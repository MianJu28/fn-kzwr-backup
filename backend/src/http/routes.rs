//! REST 路由定义（axum）
//!
//! 目标存储为 kzwr 官方 WebDAV（ADR-009）。凭据经 /webdav/config 配置并加密存储。

use std::path::PathBuf;
use std::sync::Arc;

use age::secrecy::ExposeSecret;
use axum::extract::State;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};

use crate::domain::backup::BackupJob;
use crate::domain::restore::RestoreJob;
use crate::http::ws;
use crate::infra::storage_trait::TargetStorage;
use crate::AppState;

// ── 响应/请求结构 ──────────────────────────────

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

/// 用户信息响应（WebDAV 模式：仅本地配置的账号，无远端套餐/容量）
#[derive(Serialize, Default)]
pub struct UserInfoResponse {
    pub username: Option<String>,
    pub error: Option<String>,
}

/// WebDAV 配置保存请求
///
/// 地址固定为官方 WebDAV（`DEFAULT_URL`），不暴露给用户设置；url 字段可选（忽略）。
#[derive(Deserialize)]
pub struct WebdavSaveRequest {
    #[serde(default)]
    pub url: Option<String>,
    pub username: String,
    pub password: String,
}

/// WebDAV 配置保存响应（保存前先实测连通性）
#[derive(Serialize)]
pub struct WebdavSaveResponse {
    pub success: bool,
    pub url: Option<String>,
    /// 账号一致性提醒（如 WebDAV 账号与已配置的 kzwr access-token 账号不同）
    pub warning: Option<String>,
    pub error: Option<String>,
}

/// 配置响应（不回传敏感字段明文）
#[derive(Serialize)]
pub struct ConfigResponse {
    pub backup_paths: Vec<String>,
    pub target_folder: String,
    /// 定时备份 cron 表达式（空 = 未启用）
    pub schedule_cron: String,
    /// cron 表达式是否合法（供前端提示）
    pub schedule_cron_valid: bool,
    /// WebDAV 是否已配置
    pub webdav_configured: bool,
    /// 已配置的 WebDAV 地址（非敏感）
    pub webdav_url: Option<String>,
    /// 已配置的 WebDAV 用户名（非敏感，供设置页回显；密码永不返回）
    pub webdav_username: Option<String>,
    /// 保留策略（目标端孤儿文件清理，非敏感）
    pub retention: RetentionView,
    /// 是否已配置 kzwr access-token（增强功能；token 本身永不返回）
    pub kzwr_token_configured: bool,
    /// 云端空间占用预警阈值（百分比，0 = 关闭）
    pub kzwr_quota_warn_percent: u64,
    /// 定时任务未来 5 次触发时间（服务器本地时区；空 = 未启用）
    pub schedule_next: Vec<String>,
    /// 服务器时区说明（cron 按此时区解释）
    pub schedule_timezone: String,
    /// 宿主时区相对 UTC 的分钟偏移（前端据此把时间戳按宿主时区展示）
    pub host_utc_offset_minutes: i64,
    /// 告警 Webhook 地址（空 = 不外发）
    pub webhook_url: Option<String>,
    /// Webhook 自定义请求头
    pub webhook_headers: Vec<crate::infra::config::WebhookHeader>,
    /// Webhook 自定义请求体模板（空 = 默认 JSON）
    pub webhook_body: Option<String>,
    /// 用户是否已确认备份 age 私钥（未确认时 UI 提示丢失风险）
    pub key_backed_up: bool,
    /// 调试日志开关（开启后输出详细日志，便于问题定位）
    pub debug: bool,
    pub error: Option<String>,
}

/// 保留策略视图（非敏感，供设置页回显与修改）
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct RetentionView {
    /// 是否启用保留策略
    pub enabled: bool,
    /// 清理目标端不在任何快照中的孤儿文件
    pub cleanup_unmanaged: bool,
    /// 仅清理早于该天数的文件（0 = 不限制）
    pub min_age_days: u64,
    /// 备份后清空云端回收站（需 kzwr access-token）
    pub empty_recycle_bin: bool,
    /// 回收站占用超过该 GB 数才清理（0 = 不限制）
    pub recycle_max_gb: u64,
    /// 只清理删除时间早于该天数的回收站条目（0 = 不限制）
    pub recycle_min_age_days: u64,
}

impl From<&crate::infra::config::RetentionConfig> for RetentionView {
    fn from(r: &crate::infra::config::RetentionConfig) -> Self {
        Self {
            enabled: r.enabled,
            cleanup_unmanaged: r.cleanup_unmanaged,
            min_age_days: r.min_age_days,
            empty_recycle_bin: r.empty_recycle_bin,
            recycle_max_gb: r.recycle_max_gb,
            recycle_min_age_days: r.recycle_min_age_days,
        }
    }
}

/// 配置保存请求
#[derive(Deserialize)]
pub struct ConfigSaveRequest {
    pub backup_paths: Vec<String>,
    pub target_folder: Option<String>,
    /// 定时备份 cron 表达式（空 = 关闭定时）
    pub schedule_cron: Option<String>,
    /// 保留策略：以下三项不传则保持原值不变
    #[serde(default)]
    pub retention_enabled: Option<bool>,
    #[serde(default)]
    pub retention_cleanup_unmanaged: Option<bool>,
    #[serde(default)]
    pub retention_min_age_days: Option<u64>,
    #[serde(default)]
    pub retention_empty_recycle_bin: Option<bool>,
    #[serde(default)]
    pub retention_recycle_max_gb: Option<u64>,
    #[serde(default)]
    pub retention_recycle_min_age_days: Option<u64>,
    /// kzwr 云端空间预警阈值（百分比，0 = 关闭）
    #[serde(default)]
    pub kzwr_quota_warn_percent: Option<u64>,
    /// 调试日志开关（不传则保持原值）
    #[serde(default)]
    pub debug: Option<bool>,
}

/// 备份响应
#[derive(Serialize, Default)]
pub struct BackupResponse {
    pub uploaded: usize,
    pub uploaded_bytes: u64,
    pub deleted: usize,
    pub unchanged: usize,
    /// 保留策略清理的孤儿文件数
    pub orphan_removed: usize,
    /// 跟随保留策略清空的回收站条目数（未启用/未配置 token 时为 0）
    pub trash_emptied: usize,
    /// 因已有备份正在执行而跳过本次触发（`error` 同时给出原因）
    pub skipped: bool,
    pub error: Option<String>,
}

/// 恢复请求体
#[derive(Deserialize)]
pub struct RestoreRequest {
    /// 要恢复的文件相对路径列表；空 = 全量
    pub files: Option<Vec<String>>,
    /// 恢复目标根目录（未传则用配置的备份源路径，恢复到原位置）
    pub source_path: Option<String>,
    /// 「全部恢复」：忽略 `files`，取该源路径快照中的全部文件
    #[serde(default)]
    pub all: bool,
    /// 与 `all` 搭配：只恢复该相对目录（含子目录）下的文件；空 = 整个源路径
    #[serde(default)]
    pub dir: Option<String>,
}

/// 恢复响应
#[derive(Serialize)]
pub struct RestoreResponse {
    pub restored: usize,
    pub restored_bytes: u64,
    pub error: Option<String>,
    /// 云端已不存在而被跳过的文件（最多 200 条；为空不序列化）
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub missing: Vec<String>,
}

/// 一个备份文件夹的可恢复概况（文件明细改为按目录懒加载）
#[derive(Serialize)]
pub struct RestorableFolder {
    /// 备份源路径（本地目录）
    pub path: String,
    /// 是否已有备份数据（SQLite 快照）
    pub has_backup: bool,
    /// 递归文件总数（不含目录）
    pub file_count: usize,
    /// 递归子目录总数
    pub dir_count: usize,
    /// 递归明文总字节数
    pub total_bytes: u64,
}

/// 恢复文件列表响应
#[derive(Serialize)]
pub struct RestoreFilesResponse {
    pub folders: Vec<RestorableFolder>,
    pub error: Option<String>,
}

/// 目录树查询参数（懒加载：一次只取一层）
#[derive(Deserialize)]
pub struct RestoreTreeQuery {
    /// 备份源路径（本地目录，须在备份配置中）
    pub source: String,
    /// 要展开的目录相对路径（空 / 缺省 = 根层级）
    #[serde(default)]
    pub dir: String,
}

/// 目录树节点（目录带递归统计，便于前端直接展示「N 文件 / M 文件夹」）
#[derive(Serialize)]
pub struct RestoreNode {
    pub name: String,
    pub rel_path: String,
    pub is_dir: bool,
    /// 文件：明文大小；目录：0（看 `total_bytes`）
    pub size: u64,
    /// 目录：递归文件数；文件：0
    pub file_count: usize,
    /// 目录：递归子目录数；文件：0
    pub dir_count: usize,
    /// 目录：递归明文总字节；文件：0
    pub total_bytes: u64,
}

/// 目录树响应（指定目录的直接子项）
#[derive(Serialize)]
pub struct RestoreTreeResponse {
    pub nodes: Vec<RestoreNode>,
    pub error: Option<String>,
}

/// 密钥信息响应（永不回传私钥明文）
#[derive(Serialize)]
pub struct KeysInfoResponse {
    /// age 公钥（"age1..."）
    pub public_key: String,
    /// 首次启动自动生成时的私钥（仅此一次返回，提醒用户保存；之后恒为 None）
    pub private_key_once: Option<String>,
    pub error: Option<String>,
}

/// 设置自定义私钥请求
#[derive(Deserialize)]
pub struct KeysSetRequest {
    /// age 私钥（"AGE-SECRET-KEY-1..."）
    pub private_key: String,
}

/// 密钥变更响应（generate 成功时 private_key 仅此一次返回）
#[derive(Serialize)]
pub struct KeysChangeResponse {
    pub success: bool,
    pub public_key: Option<String>,
    pub private_key: Option<String>,
    pub error: Option<String>,
}

/// 告警列表响应（监控告警）
#[derive(Serialize)]
pub struct AlertsResponse {
    pub alerts: Vec<crate::domain::alerts::Alert>,
    pub error: Option<String>,
}

/// Webhook 配置请求
#[derive(Deserialize)]
pub struct WebhookSaveRequest {
    /// 告警 Webhook 地址（空 = 关闭外发）
    pub webhook_url: String,
    /// 自定义请求头
    #[serde(default)]
    pub headers: Vec<crate::infra::config::WebhookHeader>,
    /// 自定义请求体模板（空 = 默认 JSON）
    #[serde(default)]
    pub body_template: Option<String>,
}

/// Webhook 配置响应
#[derive(Serialize)]
pub struct WebhookSaveResponse {
    pub success: bool,
    pub webhook_url: Option<String>,
    pub headers: Vec<crate::infra::config::WebhookHeader>,
    pub body_template: Option<String>,
    pub error: Option<String>,
}

/// 私钥导出请求（需管理员口令校验）
#[derive(Deserialize)]
pub struct KeysExportRequest {
    /// 管理员口令（安装向导设置的应用口令）
    #[serde(default)]
    pub passphrase: String,
}

/// Webhook 连通性测试请求（可直接用表单中的当前值测试，无需先保存）
#[derive(Deserialize)]
pub struct WebhookTestRequest {
    #[serde(default)]
    pub webhook_url: String,
    #[serde(default)]
    pub headers: Vec<crate::infra::config::WebhookHeader>,
    #[serde(default)]
    pub body_template: Option<String>,
}

/// Webhook 测试响应
#[derive(Serialize)]
pub struct WebhookTestResponse {
    pub success: bool,
    pub status: Option<u16>,
    pub error: Option<String>,
}

/// 配置导出/导入包（含敏感项，导出需管理员口令）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigBundle {
    pub version: u32,
    #[serde(default)]
    pub exported_at: i64,
    #[serde(default)]
    pub backup_paths: Vec<String>,
    #[serde(default)]
    pub target_folder: String,
    #[serde(default)]
    pub schedule_cron: Option<String>,
    #[serde(default)]
    pub retention: Option<crate::infra::config::RetentionConfig>,
    #[serde(default)]
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub webhook_headers: Vec<crate::infra::config::WebhookHeader>,
    #[serde(default)]
    pub webhook_body: Option<String>,
    #[serde(default)]
    pub webdav_url: Option<String>,
    #[serde(default)]
    pub webdav_username: Option<String>,
    #[serde(default)]
    pub webdav_password: Option<String>,
    /// kzwr access-token（增强功能：存储空间/回收站；可选）
    #[serde(default)]
    pub kzwr_access_token: Option<String>,
    /// age 私钥（恢复配置后可继续解密既有备份）
    #[serde(default)]
    pub age_private_key: Option<String>,
    #[serde(default)]
    pub key_backed_up: bool,
}

/// 配置导出请求（需管理员口令）
#[derive(Deserialize)]
pub struct ConfigExportRequest {
    #[serde(default)]
    pub passphrase: String,
}

/// 配置导出响应
#[derive(Serialize)]
pub struct ConfigExportResponse {
    pub success: bool,
    /// 配置 JSON 文本
    pub config: Option<String>,
    pub error: Option<String>,
}

/// 配置导入请求（需管理员口令）
#[derive(Deserialize)]
pub struct ConfigImportRequest {
    #[serde(default)]
    pub passphrase: String,
    pub config: String,
}

/// 配置导入响应
#[derive(Serialize)]
pub struct ConfigImportResponse {
    pub success: bool,
    pub error: Option<String>,
}

/// 私钥导出响应（敏感：供用户另存备份）
#[derive(Serialize)]
pub struct KeysExportResponse {
    pub private_key: Option<String>,
    pub error: Option<String>,
}

/// 私钥备份确认响应
#[derive(Serialize)]
pub struct BackupAckResponse {
    pub success: bool,
    pub error: Option<String>,
}

/// 记录一条告警；若配置了 Webhook 则异步尽力外发（不阻塞主流程）
fn raise_alert(
    state: &AppState,
    level: crate::domain::alerts::AlertLevel,
    source: crate::domain::alerts::AlertSource,
    message: String,
) {
    let alert = state.alerts.push(level, source, message);
    let notify = {
        let mgr = state.config.lock().unwrap();
        mgr.load().ok().map(|c| c.notify)
    };
    if let Some(n) = notify {
        if let Some(url) = n.webhook_url.filter(|s| !s.trim().is_empty()) {
            let headers: Vec<(String, String)> = n
                .webhook_headers
                .iter()
                .map(|h| (h.name.clone(), h.value.clone()))
                .collect();
            tokio::spawn(crate::domain::alerts::dispatch_webhook(
                url,
                headers,
                n.webhook_body.clone(),
                alert,
            ));
        }
    }
}

/// 更新「私钥已备份」标记（失败仅记日志，不影响主流程）
fn set_key_backed_up(state: &AppState, backed_up: bool) {
    let guard = state.config.lock().unwrap();
    match guard.load() {
        Ok(mut cfg) => {
            cfg.keys.backed_up = backed_up;
            if let Err(e) = guard.save(&cfg) {
                tracing::warn!(err = %e, "更新私钥备份标记失败");
            }
        }
        Err(e) => tracing::warn!(err = %e, "读取配置失败，未更新私钥备份标记"),
    }
}

// ── Handler ────────────────────────────────────

async fn health(State(_state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// 插件清单（内置插件；供前端区块注册表与问题诊断）
async fn plugins_list(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "plugins": state.plugins.list() }))
}

/// 保存 WebDAV 配置：先实测连通性（PROPFIND ping），通过后加密存储
async fn webdav_save(
    State(state): State<AppState>,
    Json(body): Json<WebdavSaveRequest>,
) -> Json<WebdavSaveResponse> {
    if body.username.trim().is_empty() || body.password.is_empty() {
        return Json(WebdavSaveResponse {
            success: false,
            url: None,
            warning: None,
            error: Some("用户名、密码均不能为空".to_string()),
        });
    }

    // 1) 实测连通性与凭据：交给**目标插件**判定（地址与协议由插件决定）
    let url = match state.plugins.default_target() {
        Some(p) => match p.verify(body.username.trim(), &body.password).await {
            Ok(u) => u,
            Err(e) => {
                return Json(WebdavSaveResponse {
                    success: false,
                    url: None,
                    warning: None,
                    error: Some(e),
                })
            }
        },
        None => {
            return Json(WebdavSaveResponse {
                success: false,
                url: None,
                warning: None,
                error: Some("未注册任何备份目标插件".to_string()),
            })
        }
    };

    // 2) 加密保存
    let saved = {
        let mgr = state.config.lock().unwrap();
        mgr.save_webdav(&url, body.username.trim(), &body.password)
    };
    match saved {
        Ok(_) => {
            // 热切换目标实现（无需重启服务）：由注册表按当前配置重新装配
            let (next, _name, _ready) = {
                let mgr = state.config.lock().unwrap();
                state.plugins.build_target(&mgr)
            };
            state.target.swap(next);
            // 账号一致性：已配置 access-token 时，核对 WebDAV 账号与 API 账号
            let warning = check_account_consistency(&state).await;
            state.audit.record(
                "webdav.credentials",
                format!(
                    "保存 WebDAV 凭据（账号 {}）{}",
                    body.username.trim(),
                    if warning.is_some() { "；账号与 access-token 不一致" } else { "" }
                ),
                true,
                None,
            );
            Json(WebdavSaveResponse {
                success: true,
                url: Some(url),
                warning,
                error: None,
            })
        }
        Err(e) => {
            state.audit.record(
                "webdav.credentials",
                format!("保存 WebDAV 凭据失败：{:#}", e),
                false,
                None,
            );
            Json(WebdavSaveResponse {
                success: false,
                url: Some(url),
                warning: None,
                error: Some(format!("{:#}", e)),
            })
        }
    }
}

/// 账号一致性检查：WebDAV 账号 vs 已配置的 access-token 所属账号
///
/// 返回 `Some(提醒)` 表示不一致（同时生成告警），`None` 表示一致/无法判定/未配置 token。
async fn check_account_consistency(state: &AppState) -> Option<String> {
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

/// 由配置构造响应（含 cron 校验）
fn config_response(
    cfg: &crate::infra::config::AppConfig,
    webdav_configured: bool,
    webdav_username: Option<String>,
    kzwr_token_configured: bool,
    error: Option<String>,
) -> ConfigResponse {
    let schedule = cfg.backup.schedule_cron.clone().unwrap_or_default();
    let valid = crate::domain::scheduler::validate_cron(&schedule).is_ok();
    let schedule_next = crate::domain::scheduler::next_runs(&schedule, 5).unwrap_or_default();
    ConfigResponse {
        backup_paths: cfg.backup.paths.clone(),
        target_folder: cfg.backup.target_folder.clone(),
        schedule_cron: schedule,
        schedule_cron_valid: valid,
        webdav_configured,
        webdav_url: cfg.webdav.url.clone(),
        webdav_username,
        retention: RetentionView::from(&cfg.backup.retention),
        kzwr_token_configured,
        kzwr_quota_warn_percent: cfg.kzwr.quota_warn_percent,
        schedule_next,
        schedule_timezone: crate::domain::scheduler::timezone_label(),
        host_utc_offset_minutes: crate::domain::scheduler::utc_offset_minutes(),
        webhook_url: cfg.notify.webhook_url.clone(),
        webhook_headers: cfg.notify.webhook_headers.clone(),
        webhook_body: cfg.notify.webhook_body.clone(),
        key_backed_up: cfg.keys.backed_up,
        debug: cfg.debug,
        error,
    }
}

/// 判断 WebDAV 是否已配置（url + 用户名 + 密码齐全）
fn webdav_ready(cfg: &crate::infra::config::AppConfig) -> bool {
    cfg.webdav.url.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false)
        && cfg.webdav.username_enc.is_some()
        && cfg.webdav.password_enc.is_some()
}

async fn config_get(State(state): State<AppState>) -> Json<ConfigResponse> {
    // 注意：config_echo 内部会锁 config，故须在下面加锁前先取，避免同一 Mutex 重入死锁
    let (wuser, kzwr_on) = config_echo(&state);
    let cfg_guard = state.config.lock().unwrap();
    match cfg_guard.load() {
        Ok(c) => Json(config_response(&c, webdav_ready(&c), wuser, kzwr_on, None)),
        Err(e) => Json(ConfigResponse {
            backup_paths: Vec::new(),
            target_folder: state.target_folder.clone(),
            schedule_cron: String::new(),
            schedule_cron_valid: true,
            webdav_configured: false,
            webdav_url: None,
            webdav_username: None,
            retention: RetentionView::default(),
            kzwr_token_configured: false,
            kzwr_quota_warn_percent: 85,
            schedule_next: Vec::new(),
            schedule_timezone: crate::domain::scheduler::timezone_label(),
            host_utc_offset_minutes: crate::domain::scheduler::utc_offset_minutes(),
            webhook_url: None,
            webhook_headers: Vec::new(),
            webhook_body: None,
            key_backed_up: false,
            debug: false,
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 保存配置（备份路径、目标文件夹、定时 cron）
async fn config_save(
    State(state): State<AppState>,
    Json(body): Json<ConfigSaveRequest>,
) -> Json<ConfigResponse> {
    // 同 config_get：先取回显字段再锁配置，避免 Mutex 重入死锁
    let (wuser, kzwr_on) = config_echo(&state);
    let cfg_guard = state.config.lock().unwrap();
    let mut cfg = match cfg_guard.load() {
        Ok(c) => c,
        Err(_) => crate::infra::config::AppConfig::default(),
    };
    cfg.backup.paths = body.backup_paths.clone();
    if let Some(folder) = body.target_folder {
        if !folder.trim().is_empty() {
            cfg.backup.target_folder = folder;
        }
    }
    // 保留策略：仅更新传入的字段（未传保持原值）
    if let Some(v) = body.retention_enabled {
        cfg.backup.retention.enabled = v;
    }
    if let Some(v) = body.retention_cleanup_unmanaged {
        cfg.backup.retention.cleanup_unmanaged = v;
    }
    if let Some(v) = body.retention_min_age_days {
        cfg.backup.retention.min_age_days = v;
    }
    if let Some(v) = body.retention_empty_recycle_bin {
        cfg.backup.retention.empty_recycle_bin = v;
    }
    if let Some(v) = body.retention_recycle_max_gb {
        cfg.backup.retention.recycle_max_gb = v;
    }
    if let Some(v) = body.retention_recycle_min_age_days {
        cfg.backup.retention.recycle_min_age_days = v;
    }
    if let Some(v) = body.kzwr_quota_warn_percent {
        // 合法区间 0..=100（0 = 关闭预警）
        cfg.kzwr.quota_warn_percent = v.min(100);
    }
    // 调试日志开关：保存并即时生效（日志过滤器热更新）
    if let Some(v) = body.debug {
        cfg.debug = v;
    }
    // 定时 cron：校验合法性；空串视为关闭
    if let Some(cron) = body.schedule_cron {
        let cron = cron.trim().to_string();
        if let Err(e) = crate::domain::scheduler::validate_cron(&cron) {
            let resp = config_response(&cfg, webdav_ready(&cfg), wuser, kzwr_on, Some(e.to_string()));
            return Json(resp);
        }
        if cron.is_empty() {
            cfg.backup.schedule_cron = None;
        } else {
            cfg.backup.schedule_cron = Some(cron);
        }
    }
    match cfg_guard.save(&cfg) {
        Ok(_) => {
            state.audit.record(
                "config.save",
                format!(
                    "保存配置：目标 {}，路径 {} 个，定时 {}，保留策略[启用={} 孤儿={} 天数={} 回收站={} ≥{}GB >{}天]",
                    cfg.backup.target_folder,
                    cfg.backup.paths.len(),
                    cfg.backup
                        .schedule_cron
                        .clone()
                        .unwrap_or_else(|| "未启用".to_string()),
                    cfg.backup.retention.enabled,
                    cfg.backup.retention.cleanup_unmanaged,
                    cfg.backup.retention.min_age_days,
                    cfg.backup.retention.empty_recycle_bin,
                    cfg.backup.retention.recycle_max_gb,
                    cfg.backup.retention.recycle_min_age_days,
                ),
                true,
                None,
            );
            // 调试日志开关热更新（保存成功后立即切换日志级别）
            crate::apply_log_debug(cfg.debug);
            Json(config_response(&cfg, webdav_ready(&cfg), wuser, kzwr_on, None))
        }
        Err(e) => Json(config_response(&cfg, false, wuser, kzwr_on, Some(format!("{:#}", e)))),
    }
}

/// 当前 WebDAV 账号（从加密配置解密）
fn webdav_username(state: &AppState) -> Option<String> {
    let mgr = state.config.lock().unwrap();
    mgr.webdav_credentials().ok().and_then(|(u, _)| u)
}

/// 设置页回显所需的非敏感信息（内部会锁 config，**必须在加锁前调用**）
///
/// 返回 (WebDAV 用户名, 是否已配置 kzwr access-token)；token 本身永不返回。
fn config_echo(state: &AppState) -> (Option<String>, bool) {
    let mgr = state.config.lock().unwrap();
    let user = mgr.webdav_credentials().ok().and_then(|(u, _)| u);
    let kzwr = mgr.kzwr_token().ok().flatten().is_some();
    (user, kzwr)
}

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

/// 获取当前 WebDAV 账号信息
async fn user_info(State(state): State<AppState>) -> Json<UserInfoResponse> {
    if let Some(u) = webdav_username(&state) {
        return Json(UserInfoResponse {
            username: Some(u),
            error: None,
        });
    }
    let configured = {
        let mgr = state.config.lock().unwrap();
        mgr.load().map(|c| webdav_ready(&c)).unwrap_or(false)
    };
    Json(UserInfoResponse {
        username: configured.then(|| "已配置".to_string()),
        error: (!configured).then(|| "WebDAV 未配置".to_string()),
    })
}

/// 触发备份：遍历配置的多备份路径
async fn backup_run(State(state): State<AppState>) -> Json<BackupResponse> {
    let resp = run_backup_now(&state).await;
    state.audit.record(
        "backup.run",
        match (&resp.error, resp.skipped) {
            (Some(e), _) => format!("手动备份失败：{e}"),
            (None, true) => "手动备份跳过（已有备份在执行）".to_string(),
            (None, false) => format!(
                "手动备份完成：上传 {} 个文件（{}），清理孤儿 {} 个，回收站 {} 项",
                resp.uploaded,
                human_bytes(resp.uploaded_bytes),
                resp.orphan_removed,
                resp.trash_emptied
            ),
        },
        resp.error.is_none(),
        None,
    );
    Json(resp)
}

/// 备份运行标志的 RAII 守卫：离开作用域时复位（覆盖提前 return 与 panic 展开）
struct BackupRunFlag<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for BackupRunFlag<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// 执行一次备份（可被 HTTP handler 与定时调度器复用）
///
/// 返回 BackupResponse（含 uploaded/deleted/orphan_removed/skipped/error）。
///
/// **并发互斥**：定时调度与手动触发共用 `AppState.backup_running`，
/// 同一时刻只允许一个备份执行；已有备份在跑时立即返回 `skipped = true`。
pub async fn run_backup_now(state: &AppState) -> BackupResponse {
    // 0) 抢占运行标志（CAS）；失败说明已有备份在执行，直接跳过
    use std::sync::atomic::Ordering;
    if state
        .backup_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return BackupResponse {
            skipped: true,
            error: Some("已有备份任务正在执行，本次触发已跳过".to_string()),
            ..Default::default()
        };
    }
    let _running = BackupRunFlag(&state.backup_running);

    // 1) 检查 WebDAV 是否已配置（未配置时 target 为占位适配器，会返回引导错误）
    let configured = state.target_ready
        || {
            let mgr = state.config.lock().unwrap();
            mgr.load().map(|c| webdav_ready(&c)).unwrap_or(false)
        };
    if !configured {
        raise_alert(
            state,
            crate::domain::alerts::AlertLevel::Warn,
            crate::domain::alerts::AlertSource::Config,
            "备份未执行：WebDAV 未配置".to_string(),
        );
        return BackupResponse {
            error: Some("WebDAV 未配置，请先在设置中填写 WebDAV 地址与凭据".to_string()),
            ..Default::default()
        };
    }

    // 2) 读取配置的备份路径（锁操作隔离在同步函数，避免跨 await）
    let (paths, target_folder, retention_cfg) = read_backup_config(state);

    if paths.is_empty() {
        raise_alert(
            state,
            crate::domain::alerts::AlertLevel::Warn,
            crate::domain::alerts::AlertSource::Backup,
            "备份未执行：未配置备份路径".to_string(),
        );
        return BackupResponse {
            error: Some("未配置备份路径".to_string()),
            ..Default::default()
        };
    }

    // 3) 执行多路径备份
    //    保留策略：启用时构造 RetentionPolicy，备份完成后清理目标端孤儿文件
    let retention = if retention_cfg.enabled && retention_cfg.cleanup_unmanaged {
        let rt = crate::domain::retention::RetentionPolicy::new(
            state.target.clone(),
            &target_folder,
        );
        let rt = if retention_cfg.min_age_days > 0 {
            rt.with_min_age_secs(retention_cfg.min_age_days * 86400)
        } else {
            rt
        };
        Some(rt)
    } else {
        None
    };

    let job = BackupJob {
        job_id: state.job_id.clone(),
        account: webdav_username(state),
        source: Arc::new(crate::infra::source::local::LocalFsSource::new(&paths[0])),
        target: state.target.clone(),
        crypto: state.crypto.get(),
        store: state.store.clone(),
        target_prefix: Some(target_folder),
        eventbus: Some(state.eventbus.clone()),
        retention,
    };
    match job.run_multi(&paths).await {
        Ok(summary) => {
            // 保留策略可选：跟随清空云端回收站（需配置 kzwr access-token）
            let trash_emptied = if retention_cfg.empty_recycle_bin {
                empty_recycle_bin_if_configured(state).await
            } else {
                0
            };
            // 备份后云端占用会变化：异步巡检一次空间预警（不阻塞本次响应）
            {
                let quota_state = state.clone();
                tokio::spawn(async move { check_kzwr_quota(&quota_state).await });
            }
            BackupResponse {
                uploaded: summary.uploaded,
                uploaded_bytes: summary.uploaded_bytes,
                deleted: summary.deleted,
                unchanged: summary.unchanged,
                orphan_removed: summary.orphan_removed,
                trash_emptied,
                ..Default::default()
            }
        }
        Err(e) => {
            let msg = format!("{:#}", e);
            raise_alert(
                state,
                crate::domain::alerts::AlertLevel::Error,
                crate::domain::alerts::AlertSource::Backup,
                format!("备份失败：{}", msg),
            );
            // 发布 Failed 终态事件：否则前端任务面板停留在「进行中」永不结束
            state.eventbus.task_event(
                crate::eventbus::TaskKind::Backup,
                crate::eventbus::TaskStatus::Failed,
                None,
                state.job_id.clone(),
                None,
                0,
                0,
                0,
                0,
                0,
                0,
                Some(msg.clone()),
            );
            BackupResponse {
                error: Some(msg),
                ..Default::default()
            }
        }
    }
}

/// 读取备份配置（同步，锁在函数内释放）
fn read_backup_config(
    state: &AppState,
) -> (Vec<PathBuf>, String, crate::infra::config::RetentionConfig) {
    let cfg_guard = state.config.lock().unwrap();
    match cfg_guard.load() {
        Ok(cfg) => {
            let paths = cfg.backup.paths.iter().map(PathBuf::from).collect();
            let folder = cfg.backup.target_folder.clone();
            let retention = cfg.backup.retention.clone();
            (paths, folder, retention)
        }
        Err(_) => (
            Vec::new(),
            state.target_folder.clone(),
            crate::infra::config::RetentionConfig::default(),
        ),
    }
}

/// 快照聚合索引：**一次遍历**构建全部前缀的统计，之后 O(1) 查询
///
/// 原实现（`snapshot_aggregate`）对每个目录节点全量扫描一遍快照，
/// 树接口整体 O(条目数 × 节点数)；大快照下是响应慢的主因之一。
/// 这里一趟完成：
/// - 文件条目：把 (1, size) 累加到**各级祖先前缀**（含根 ""）；
/// - 目录宇宙（目录条目 + 文件的各级祖先）：为每个目录向其**所有真前缀**（含根）
///   计 1 —— 与原实现「BTreeSet 去重后数集合」语义一致；
/// - 文件大小表：文件路径 → 明文字节（树接口文件节点 O(1) 取大小）。
struct SnapshotAgg {
    /// 前缀（"" 表示根）→ (递归文件数, 递归明文字节)
    files: std::collections::HashMap<String, (usize, u64)>,
    /// 前缀（"" 表示根）→ 严格位于该前缀下的目录数
    dirs: std::collections::HashMap<String, usize>,
    /// 文件相对路径 → 明文大小
    sizes: std::collections::HashMap<String, u64>,
}

impl SnapshotAgg {
    fn build(
        entries: &[crate::infra::persistence::snapshot::SnapshotEntry],
    ) -> Self {
        let mut files: std::collections::HashMap<String, (usize, u64)> =
            std::collections::HashMap::new();
        let mut dir_universe: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut sizes: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

        let push_dir = |universe: &mut std::collections::HashSet<String>, acc: &mut String, p: &str| {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(p);
            universe.insert(acc.clone());
        };

        for e in entries {
            let parts: Vec<&str> = e.rel_path.split('/').collect();
            if e.is_dir {
                let mut acc = String::new();
                for p in &parts {
                    push_dir(&mut dir_universe, &mut acc, p);
                }
            } else {
                sizes.insert(e.rel_path.clone(), e.size);
                // 各级祖先（i = 0..len-1；i=0 即根 ""）
                let mut acc = String::new();
                for p in &parts[..parts.len().saturating_sub(1)] {
                    let v = files.entry(acc.clone()).or_insert((0, 0));
                    v.0 += 1;
                    v.1 += e.size;
                    push_dir(&mut dir_universe, &mut acc, p);
                }
                let v = files.entry(acc.clone()).or_insert((0, 0));
                v.0 += 1;
                v.1 += e.size;
            }
        }

        // 每个目录向其所有真前缀（含根 ""）计 1
        let mut dirs: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for d in &dir_universe {
            let parts: Vec<&str> = d.split('/').collect();
            let mut acc = String::new();
            for p in &parts[..parts.len() - 1] {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(p);
                *dirs.entry(acc.clone()).or_insert(0) += 1;
            }
            *dirs.entry(String::new()).or_insert(0) += 1;
        }

        Self {
            files,
            dirs,
            sizes,
        }
    }

    /// 查询某前缀下的 (递归文件数, 递归子目录数, 递归明文字节)
    fn get(&self, prefix: &str) -> (usize, usize, u64) {
        let p = prefix.trim_matches('/');
        let (f, b) = self.files.get(p).copied().unwrap_or((0, 0));
        let d = self.dirs.get(p).copied().unwrap_or(0);
        (f, d, b)
    }

    /// 文件相对路径 → 明文大小（未知为 0）
    fn size_of(&self, rel_path: &str) -> u64 {
        self.sizes.get(rel_path).copied().unwrap_or(0)
    }
}

/// 列出某前缀下的直接子项（名称 + 是否目录），目录优先、其余按名称排序
fn snapshot_children(
    entries: &[crate::infra::persistence::snapshot::SnapshotEntry],
    prefix: &str,
) -> Vec<(String, bool)> {
    let mut map: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
    for e in entries {
        let Some(rest) = e.rel_path.strip_prefix(prefix) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let (name, is_dir) = match rest.split_once('/') {
            Some((head, _)) => (head.to_string(), true),
            None => (rest.to_string(), e.is_dir),
        };
        map.entry(name).and_modify(|d| *d = *d || is_dir).or_insert(is_dir);
    }
    let mut out: Vec<(String, bool)> = map.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// 按备份源路径取出其快照条目（精确路径匹配优先，其次按目录名匹配）
fn snapshot_entries_for_source(
    state: &AppState,
    source: &str,
) -> Result<Vec<crate::infra::persistence::snapshot::SnapshotEntry>, String> {
    let paths = read_backup_config(state).0;
    let root = std::path::Path::new(source);
    let idx = paths
        .iter()
        .position(|p| p.as_path() == root)
        .or_else(|| {
            root.file_name()
                .and_then(|n| paths.iter().position(|p| p.file_name() == Some(n)))
        })
        .ok_or_else(|| "该路径不在备份配置中".to_string())?;
    // 只展示当前账号的备份记录；未配置账号时归入默认 ''（旧数据）
    let account = webdav_username(state).unwrap_or_default();
    let job_id = format!("{}-{}", state.job_id, idx);
    state
        .store
        .load_snapshot(&job_id, &account)
        .map_err(|e| format!("读取备份快照失败: {:#}", e))
}

/// 列出配置的备份文件夹及其可恢复概况（计数来自 SQLite 快照，文件明细按需懒加载）
async fn restore_files(State(state): State<AppState>) -> Json<RestoreFilesResponse> {
    let paths = read_backup_config(&state).0;
    // 只展示当前账号的备份记录；未配置账号时归入默认 ''（旧数据）
    let account = webdav_username(&state).unwrap_or_default();
    let mut folders = Vec::new();

    for (i, path) in paths.iter().enumerate() {
        // 多路径备份时，每个路径的 job_id = "{base}-{i}"
        let job_id = format!("{}-{}", state.job_id, i);
        let entries = state.store.load_snapshot(&job_id, &account).unwrap_or_default();
        let agg = SnapshotAgg::build(&entries);
        let (file_count, dir_count, total_bytes) = agg.get("");

        folders.push(RestorableFolder {
            path: path.to_string_lossy().into_owned(),
            has_backup: file_count > 0,
            file_count,
            dir_count,
            total_bytes,
        });
    }

    Json(RestoreFilesResponse { folders, error: None })
}

/// 按目录懒加载恢复树：只返回指定目录的**直接子项**，目录附递归统计
///
/// 前端展开某个文件夹/子目录时才调用，避免一次性下发整棵树（大备份下可达数万条）。
async fn restore_tree(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<RestoreTreeQuery>,
) -> Json<RestoreTreeResponse> {
    let entries = match snapshot_entries_for_source(&state, &q.source) {
        Ok(e) => e,
        Err(msg) => {
            return Json(RestoreTreeResponse {
                nodes: Vec::new(),
                error: Some(msg),
            })
        }
    };

    let dir = q.dir.trim_matches('/').to_string();
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{}/", dir)
    };

    // 一次遍历建索引：目录统计与文件大小查询均为 O(1)
    let agg = SnapshotAgg::build(&entries);

    let mut nodes = Vec::new();
    for (name, is_dir) in snapshot_children(&entries, &prefix) {
        let rel_path = if dir.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", dir, name)
        };
        if is_dir {
            let (file_count, dir_count, total_bytes) = agg.get(&rel_path);
            nodes.push(RestoreNode {
                name,
                rel_path,
                is_dir: true,
                size: 0,
                file_count,
                dir_count,
                total_bytes,
            });
        } else {
            let size = agg.size_of(&rel_path);
            nodes.push(RestoreNode {
                name,
                rel_path,
                is_dir: false,
                size,
                file_count: 0,
                dir_count: 0,
                total_bytes: 0,
            });
        }
    }

    Json(RestoreTreeResponse { nodes, error: None })
}

/// 触发恢复
async fn restore_run(
    State(state): State<AppState>,
    body: Option<axum::extract::Json<RestoreRequest>>,
) -> Json<RestoreResponse> {
    let configured = state.target_ready
        || {
            let mgr = state.config.lock().unwrap();
            mgr.load().map(|c| webdav_ready(&c)).unwrap_or(false)
        };
    if !configured {
        raise_alert(
            &state,
            crate::domain::alerts::AlertLevel::Warn,
            crate::domain::alerts::AlertSource::Config,
            "恢复未执行：WebDAV 未配置".to_string(),
        );
        return Json(RestoreResponse {
            restored: 0,
            restored_bytes: 0,
            error: Some("WebDAV 未配置，请先在设置中填写 WebDAV 地址与凭据".to_string()),
            missing: Vec::new(),
        });
    }

    // 恢复目标根：优先用前端传入的 source_path（备份源路径，恢复到原位置），否则用默认目录
    let (mut files, restore_root, restore_all, restore_dir) = match body {
        Some(Json(req)) => (
            req.files.unwrap_or_default(),
            req.source_path
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| state.default_restore_dir.to_string_lossy().into_owned()),
            req.all,
            req.dir
                .map(|d| d.trim_matches('/').to_string())
                .filter(|d| !d.is_empty()),
        ),
        None => (
            Vec::new(),
            state.default_restore_dir.to_string_lossy().into_owned(),
            false,
            None,
        ),
    };

    // 备份时每个源目录在目标端以其文件夹名分目录存放，恢复需还原该层级
    let source_root_name = std::path::Path::new(&restore_root)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned());

    // 定位该源路径对应的备份快照：优先精确路径匹配，其次按目录名匹配
    let paths = read_backup_config(&state).0;
    let root = std::path::Path::new(&restore_root);
    let idx = paths
        .iter()
        .position(|p| p.as_path() == root)
        .or_else(|| {
            root.file_name()
                .and_then(|n| paths.iter().position(|p| p.file_name() == Some(n)))
        });

    // 读取快照元数据：rel_path -> (明文大小, 原始 mtime 秒)
    // 用途：① 展示恢复总大小；② 恢复后回写快照，避免下次备份重复上传
    let mut meta: std::collections::HashMap<String, (u64, i64)> =
        std::collections::HashMap::new();
    let mut snapshot_target = None;
    if let Some(idx) = idx {
        let account = webdav_username(&state).unwrap_or_default();
        let job_id = format!("{}-{}", state.job_id, idx);
        if let Ok(entries) = state.store.load_snapshot(&job_id, &account) {
            for e in entries {
                if !e.is_dir {
                    meta.insert(e.rel_path.clone(), (e.size, e.mtime_secs));
                }
            }
        }
        snapshot_target = Some(crate::domain::restore::RestoreSnapshotTarget {
            store: state.store.clone(),
            job_id,
            account,
        });
    }

    // 「全部恢复」：忽略前端传入的文件列表，取该源路径（可限定子目录）快照中的全部文件
    if restore_all {
        if idx.is_none() {
            return Json(RestoreResponse {
                restored: 0,
                restored_bytes: 0,
                error: Some("未找到该路径的备份快照，请先执行一次备份".to_string()),
                missing: Vec::new(),
            });
        }
        let prefix = restore_dir.as_ref().map(|d| format!("{}/", d));
        files = meta
            .keys()
            .filter(|p| match &prefix {
                Some(pfx) => p.starts_with(pfx.as_str()),
                None => true,
            })
            .cloned()
            .collect();
        files.sort();
        if files.is_empty() {
            return Json(RestoreResponse {
                restored: 0,
                restored_bytes: 0,
                error: Some(match &restore_dir {
                    Some(d) => format!("目录 {d} 下暂无可恢复的文件"),
                    None => "该路径暂无可恢复的文件".to_string(),
                }),
                missing: Vec::new(),
            });
        }
        tracing::info!(
            "全部恢复：源 {}，目录 {}，共 {} 个文件",
            restore_root,
            restore_dir.as_deref().unwrap_or("(根)"),
            files.len()
        );
    }

    let job = RestoreJob {
        target: state.target.clone(),
        crypto: state.crypto.get(),
        target_prefix: Some(state.target_folder.clone()),
        source_root_name,
        eventbus: Some(state.eventbus.clone()),
        meta,
        snapshot_target,
    };
    match job.run(&files, std::path::Path::new(&restore_root)).await {
        Ok(summary) => {
            state.audit.record(
                "restore.run",
                format!(
                    "恢复到 {}：{} 个文件（{}）",
                    restore_root,
                    summary.restored,
                    human_bytes(summary.restored_bytes)
                ),
                true,
                None,
            );
            // 云端缺失的文件截断到 200 条，避免超大响应
            let mut missing = summary.missing;
            missing.truncate(200);
            Json(RestoreResponse {
                restored: summary.restored,
                restored_bytes: summary.restored_bytes,
                error: None,
                missing,
            })
        }
        Err(e) => {
            let msg = format!("{:#}", e);
            raise_alert(
                &state,
                crate::domain::alerts::AlertLevel::Error,
                crate::domain::alerts::AlertSource::Restore,
                format!("恢复失败：{}", msg),
            );
            // 发布 Failed 终态事件：否则前端任务面板停留在「进行中」永不结束
            state.eventbus.task_event(
                crate::eventbus::TaskKind::Restore,
                crate::eventbus::TaskStatus::Failed,
                None,
                "restore".to_string(),
                None,
                0,
                0,
                0,
                0,
                0,
                0,
                Some(msg.clone()),
            );
            Json(RestoreResponse {
                restored: 0,
                restored_bytes: 0,
                error: Some(msg),
                missing: Vec::new(),
            })
        }
    }
}

// ── 运行日志（日志页） ─────────────────────────────────────────────

/// 日志查询参数
#[derive(Deserialize)]
pub struct LogsQuery {
    /// 末尾行数（默认 800，上限 5000）
    #[serde(default)]
    pub tail: Option<usize>,
}

/// 日志查询响应
#[derive(Serialize)]
pub struct LogsResponse {
    pub lines: Vec<String>,
    /// 文件行数是否超过返回内容（前端提示「仅显示末尾 N 行」）
    pub truncated: bool,
    /// 日志文件大小（字节）
    pub size: u64,
    pub error: Option<String>,
}

/// 去掉 ANSI 转义序列（CSI：`ESC [ 参数… 终止字节`）
///
/// 旧版本日志里带着 tracing 的终端颜色码（`\x1b[2m`、`\x1b[32m` 等），直接展示会变成
/// `[2m2026-...` 这类乱码；新版本已在写入端关闭 ANSI，这里再兜一层保证历史日志也干净。
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                // 参数字节（0x30-0x3F）与中间字节（0x20-0x2F）之后是终止字节（0x40-0x7E）
                for c2 in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c2) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// 把行首的 UTC 时间戳换算成宿主本地时间。
///
/// tracing 默认写 UTC（`2026-09-21T04:14:21.827270Z`），在东八区看起来比本地少 8 小时；
/// 新版本写入端已改为直接写宿主本地时间（无 `Z` 后缀），这里负责把**历史行**也换算过来，
/// 因此「日志页 / 下载的 app.log」时间口径一致。
fn localize_log_time(line: &str) -> String {
    let (ts, rest) = match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], &line[i..]),
        None => (line, ""),
    };
    if !ts.ends_with('Z') {
        return line.to_string();
    }
    match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => format!(
            "{}{}",
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S%.3f"),
            rest
        ),
        Err(_) => line.to_string(),
    }
}

/// 读取运行日志末尾（文件超过 5MB 时自动轮转，故整读安全）
async fn logs_get(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<LogsQuery>,
) -> Json<LogsResponse> {
    let path = state.log_file.clone();
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        return Json(LogsResponse {
            lines: Vec::new(),
            truncated: false,
            size: 0,
            error: None,
        });
    }
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return Json(LogsResponse {
                lines: Vec::new(),
                truncated: false,
                size,
                error: Some(format!("读取日志失败: {e}")),
            })
        }
    };
    let all: Vec<String> = String::from_utf8_lossy(&bytes)
        .lines()
        .map(strip_ansi)
        .map(|l| localize_log_time(&l))
        .collect();
    let total = all.len();
    let tail = q.tail.unwrap_or(800).min(5000);
    let start = total.saturating_sub(tail);
    let lines = all[start..].to_vec();
    Json(LogsResponse {
        truncated: total > tail,
        lines,
        size,
        error: None,
    })
}

/// 清空运行日志
async fn logs_clear(State(state): State<AppState>) -> Json<serde_json::Value> {
    let res = std::fs::write(&state.log_file, b"");
    match res {
        Ok(_) => {
            state.audit.record("logs.clear", "清空运行日志", true, None);
            Json(serde_json::json!({ "ok": true, "error": null }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "error": format!("清空失败: {e}") })),
    }
}

/// 下载运行日志（text/plain 附件）
async fn logs_download(State(state): State<AppState>) -> axum::response::Response {
    use axum::response::IntoResponse;
    match std::fs::read(&state.log_file) {
        Ok(bytes) => {
            // 下载件同样去掉 ANSI 颜色码，并把历史 UTC 时间戳换算成宿主本地时间
            let raw = strip_ansi(&String::from_utf8_lossy(&bytes));
            let text = raw
                .lines()
                .map(localize_log_time)
                .collect::<Vec<String>>()
                .join("\n");
            let mut resp = (axum::http::StatusCode::OK, text).into_response();
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            if let Ok(v) = axum::http::HeaderValue::from_str("attachment; filename=\"app.log\"") {
                resp.headers_mut()
                    .insert(axum::http::header::CONTENT_DISPOSITION, v);
            }
            resp
        }
        Err(e) => (
            axum::http::StatusCode::NOT_FOUND,
            format!("日志文件不存在: {e}"),
        )
            .into_response(),
    }
}

/// 清空审计日志请求（需管理员口令校验）
#[derive(Deserialize)]
pub struct AuditClearRequest {
    /// 管理员口令（安装向导设置的应用口令）
    #[serde(default)]
    pub passphrase: String,
}

/// 清空审计日志响应
#[derive(Serialize)]
pub struct AuditClearResponse {
    pub cleared: usize,
    pub error: Option<String>,
}

async fn audit_clear(
    State(state): State<AppState>,
    body: Option<Json<AuditClearRequest>>,
) -> Json<AuditClearResponse> {
    // 破坏性操作：需管理员口令校验（与导出私钥/配置导入导出同口径）
    let passphrase = body
        .and_then(|Json(b)| Some(b.passphrase))
        .unwrap_or_default();
    if passphrase.as_str() != state.passphrase.expose_secret().as_str() {
        return Json(AuditClearResponse {
            cleared: 0,
            error: Some("管理员口令错误".to_string()),
        });
    }
    let cleared = state.audit.clear();
    state.audit.record(
        "audit.clear",
        format!("清空审计日志（{cleared} 条，管理员口令校验通过）"),
        true,
        None,
    );
    Json(AuditClearResponse {
        cleared,
        error: None,
    })
}

// ── 云端缺失记录清理 ────────────────────────────────────────────────

/// 清理请求体
#[derive(Deserialize)]
pub struct PruneMissingRequest {
    /// 备份源路径（与恢复页一致）
    pub source_path: String,
}

/// 清理响应
#[derive(Serialize)]
pub struct PruneMissingResponse {
    /// 检查的快照文件数
    pub checked: usize,
    /// 已从快照移除的失效记录数
    pub removed: usize,
    /// 被移除的文件（最多 200 条）
    pub files: Vec<String>,
    pub error: Option<String>,
}

/// 清理快照中云端已不存在的文件记录
///
/// 做法：递归列出目标端 `/<target_folder>/<源文件夹名>` 下的实际文件，
/// 与快照对比；快照里有、云端没有的记录即为失效，逐条从快照删除。
/// **只动快照元数据，不删云端任何文件。**
async fn restore_prune(
    State(state): State<AppState>,
    Json(body): Json<PruneMissingRequest>,
) -> Json<PruneMissingResponse> {
    fn resp(checked: usize, removed: usize, files: Vec<String>, error: Option<String>) -> PruneMissingResponse {
        PruneMissingResponse {
            checked,
            removed,
            files,
            error,
        }
    }
    fn err(e: impl std::fmt::Display) -> PruneMissingResponse {
        resp(0, 0, Vec::new(), Some(e.to_string()))
    }

    let configured = state.target_ready
        || {
            let mgr = state.config.lock().unwrap();
            mgr.load().map(|c| webdav_ready(&c)).unwrap_or(false)
        };
    if !configured {
        return Json(err("WebDAV 未配置，请先在设置中填写 WebDAV 地址与凭据"));
    }

    // 定位该源路径对应的快照（与恢复页一致的两级匹配）
    let (paths, target_folder, _) = read_backup_config(&state);
    let root = std::path::Path::new(&body.source_path);
    let idx = paths
        .iter()
        .position(|p| p.as_path() == root)
        .or_else(|| {
            root.file_name()
                .and_then(|n| paths.iter().position(|p| p.file_name() == Some(n)))
        });
    let Some(idx) = idx else {
        return Json(err("该路径不在备份配置中"));
    };
    let account = webdav_username(&state).unwrap_or_default();
    let job_id = format!("{}-{}", state.job_id, idx);
    let entries = match state.store.load_snapshot(&job_id, &account) {
        Ok(e) => e,
        Err(e) => return Json(err(format!("读取备份快照失败: {e:#}"))),
    };
    let snapshot_files: Vec<String> = entries
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| e.rel_path.clone())
        .collect();
    if snapshot_files.is_empty() {
        return Json(resp(0, 0, Vec::new(), None));
    }

    // 目标端基路径：/<target_folder>[/<源文件夹名>]（与备份/恢复的布局一致）
    let source_root_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut segs: Vec<String> = Vec::new();
    let tf = target_folder.trim_matches('/');
    if !tf.is_empty() {
        segs.push(tf.to_string());
    }
    let rn = source_root_name.trim_matches('/');
    if !rn.is_empty() {
        segs.push(rn.to_string());
    }
    if segs.is_empty() {
        return Json(err("目标目录为空，无法清理"));
    }
    let base = format!("/{}", segs.join("/"));
    let base_prefix = format!("{}/", base.trim_matches('/'));

    // 递归列出云端实际文件（显式栈遍历；分片文件由 list 合并为逻辑条目）
    let mut cloud: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stack: Vec<String> = vec![base.clone()];
    while let Some(dir) = stack.pop() {
        let entries = match state.target.list(&dir).await {
            Ok(e) => e,
            Err(e) => return Json(err(format!("列出云端目录失败: {e}"))),
        };
        for e in entries {
            if e.is_dir {
                stack.push(format!("/{}", e.rel_path.trim_matches('/')));
            } else {
                let rel = e
                    .rel_path
                    .trim_start_matches('/')
                    .strip_prefix(base_prefix.as_str())
                    .unwrap_or(e.rel_path.trim_start_matches('/'));
                cloud.insert(rel.to_string());
            }
        }
    }

    // 对比并删除失效记录（快照有、云端没有）
    let mut removed_files: Vec<String> = Vec::new();
    for rel in &snapshot_files {
        if !cloud.contains(rel) {
            match state.store.delete_entry(&job_id, &account, rel) {
                Ok(true) => removed_files.push(rel.clone()),
                Ok(false) => {}
                Err(e) => return Json(err(format!("删除快照记录失败: {e}"))),
            }
        }
    }
    let checked = snapshot_files.len();
    let removed = removed_files.len();
    let mut files = removed_files;
    files.truncate(200);
    if removed > 0 {
        state.audit.record(
            "restore.prune",
            format!(
                "清理云端缺失记录（{}）：移除 {} 条失效快照",
                body.source_path, removed
            ),
            true,
            None,
        );
    }
    Json(resp(checked, removed, files, None))
}

// ── 增强功能公共辅助 ──────────────────────────────────────────────────

/// 去重告警：同来源 + 同文案已存在时不再重复记录与外发
fn raise_alert_once(
    state: &AppState,
    level: crate::domain::alerts::AlertLevel,
    source: crate::domain::alerts::AlertSource,
    message: String,
) {
    let dup = state
        .alerts
        .list()
        .iter()
        .any(|a| a.source == source && a.message == message);
    if dup {
        return;
    }
    raise_alert(state, level, source, message);
}

/// 人类可读的字节数（用于告警文案）
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

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

// ── kzwr REST 增强功能（可选，依赖用户提供的 access-token） ──────────────

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

/// 审计日志查询参数
#[derive(Deserialize, Default)]
pub struct AuditQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

/// 定时任务预览请求
#[derive(Deserialize, Default)]
pub struct SchedulePreviewRequest {
    #[serde(default)]
    pub cron: String,
}

/// 定时任务预览响应
#[derive(Serialize, Default)]
pub struct SchedulePreviewResponse {
    pub valid: bool,
    /// 未来 5 次触发时间（服务器本地时区）
    pub next: Vec<String>,
    /// 服务器时区说明
    pub timezone: String,
    pub error: Option<String>,
}

/// 一键体检中的单项
#[derive(Serialize, Default)]
pub struct CheckItem {
    /// 机器可读标识
    pub key: String,
    pub title: String,
    /// ok | warn | fail | skip
    pub status: String,
    pub detail: String,
    /// 修复建议（可空）
    pub hint: Option<String>,
}

/// 一键体检响应
#[derive(Serialize, Default)]
pub struct SetupCheckResponse {
    pub items: Vec<CheckItem>,
    pub ok_count: usize,
    pub warn_count: usize,
    pub fail_count: usize,
    pub version: String,
    pub error: Option<String>,
}

/// 审计日志响应
#[derive(Serialize, Default)]
pub struct AuditResponse {
    pub entries: Vec<crate::domain::audit::AuditEntry>,
    pub error: Option<String>,
}

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

/// 定时任务预览：校验 cron 并给出未来 5 次触发时间（服务器本地时区）
async fn schedule_preview(
    Json(body): Json<SchedulePreviewRequest>,
) -> Json<SchedulePreviewResponse> {
    let tz = crate::domain::scheduler::timezone_label();
    let cron = body.cron.trim();
    if cron.is_empty() {
        return Json(SchedulePreviewResponse {
            valid: true,
            next: Vec::new(),
            timezone: tz,
            error: None,
        });
    }
    match crate::domain::scheduler::next_runs(cron, 5) {
        Ok(next) => Json(SchedulePreviewResponse {
            valid: true,
            next,
            timezone: tz,
            error: None,
        }),
        Err(e) => Json(SchedulePreviewResponse {
            valid: false,
            next: Vec::new(),
            timezone: tz,
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 一键体检：逐项检查配置与连通性，给出可操作建议
async fn setup_check(State(state): State<AppState>) -> Json<SetupCheckResponse> {
    let mut items: Vec<CheckItem> = Vec::new();

    // 1) 服务
    items.push(CheckItem {
        key: "service".to_string(),
        title: "服务运行状态".to_string(),
        status: "ok".to_string(),
        detail: format!("版本 v{}", env!("CARGO_PKG_VERSION")),
        hint: None,
    });

    // 2) WebDAV 配置 + 实连
    let (cfg, creds) = {
        let mgr = state.config.lock().unwrap();
        (
            mgr.load().unwrap_or_default(),
            mgr.webdav_credentials().unwrap_or((None, None)),
        )
    };
    if !webdav_ready(&cfg) {
        items.push(CheckItem {
            key: "webdav".to_string(),
            title: "WebDAV 目标".to_string(),
            status: "fail".to_string(),
            detail: "尚未配置 WebDAV 凭据，备份与恢复不可用".to_string(),
            hint: Some(
                "前往「设置 → WebDAV 目标」填写账号与应用密码；应用密码请在 https://www.kzwr.com/account/apps 创建，选择「永不过期」与读写权限"
                    .to_string(),
            ),
        });
    } else {
        let url = cfg
            .webdav
            .url
            .clone()
            .unwrap_or_else(|| crate::infra::target::webdav::DEFAULT_URL.to_string());
        let target = crate::infra::target::webdav::WebdavTarget::new(
            &url,
            creds.0.clone().unwrap_or_default().as_str(),
            creds.1.clone().unwrap_or_default().as_str(),
        );
        match target.ping().await {
            Ok(_) => items.push(CheckItem {
                key: "webdav".to_string(),
                title: "WebDAV 目标".to_string(),
                status: "ok".to_string(),
                detail: format!("已连接 {}", url),
                hint: None,
            }),
            Err(e) => items.push(CheckItem {
                key: "webdav".to_string(),
                title: "WebDAV 目标".to_string(),
                status: "fail".to_string(),
                detail: format!("连通性测试失败：{:#}", e),
                hint: Some(
                    "请确认应用密码未过期且有读写权限（可在 https://www.kzwr.com/account/apps 重新创建）"
                        .to_string(),
                ),
            }),
        }
    }

    // 3) 备份路径 + 已备份文件数
    let paths: Vec<String> = cfg.backup.paths.clone();
    if paths.is_empty() {
        items.push(CheckItem {
            key: "paths".to_string(),
            title: "备份路径".to_string(),
            status: "fail".to_string(),
            detail: "尚未添加任何备份路径".to_string(),
            hint: Some("前往「备份」页添加要备份的文件夹".to_string()),
        });
    } else {
        let account = webdav_username(&state).unwrap_or_default();
        let mut files = 0usize;
        for (i, _) in paths.iter().enumerate() {
            let job_id = format!("{}-{}", state.job_id, i);
            if let Ok(entries) = state.store.load_snapshot(&job_id, &account) {
                files += entries.iter().filter(|e| !e.is_dir).count();
            }
        }
        items.push(CheckItem {
            key: "paths".to_string(),
            title: "备份路径".to_string(),
            status: if files > 0 { "ok" } else { "warn" }.to_string(),
            detail: if files > 0 {
                format!("{} 个路径，已备份 {} 个文件", paths.len(), files)
            } else {
                format!("{} 个路径，但还没有备份记录", paths.len())
            },
            hint: if files > 0 {
                None
            } else {
                Some("前往「备份」页执行一次备份".to_string())
            },
        });
    }

    // 4) 私钥备份确认
    items.push(CheckItem {
        key: "key".to_string(),
        title: "私钥备份".to_string(),
        status: if cfg.keys.backed_up { "ok" } else { "warn" }.to_string(),
        detail: if cfg.keys.backed_up {
            "已确认妥善保存 age 私钥".to_string()
        } else {
            "尚未确认私钥已备份；私钥丢失将无法恢复数据".to_string()
        },
        hint: if cfg.keys.backed_up {
            None
        } else {
            Some("前往「设置 → 加密密钥」导出私钥并确认已保存".to_string())
        },
    });

    // 5) 定时备份
    let cron = cfg.backup.schedule_cron.clone().unwrap_or_default();
    if cron.trim().is_empty() {
        items.push(CheckItem {
            key: "schedule".to_string(),
            title: "定时备份".to_string(),
            status: "warn".to_string(),
            detail: "未启用（仅手动备份）".to_string(),
            hint: Some("如需无人值守，可在「备份」页设置 cron 表达式".to_string()),
        });
    } else {
        let next = crate::domain::scheduler::next_runs(&cron, 1).unwrap_or_default();
        items.push(CheckItem {
            key: "schedule".to_string(),
            title: "定时备份".to_string(),
            status: "ok".to_string(),
            detail: format!(
                "{}（{}，下次 {})",
                cron,
                crate::domain::scheduler::timezone_label(),
                next.first().cloned().unwrap_or_else(|| "—".to_string())
            ),
            hint: None,
        });
    }

    // 6) 增强功能（token）与云端空间
    match kzwr_token_of(&state) {
        None => items.push(CheckItem {
            key: "kzwr".to_string(),
            title: "增强功能".to_string(),
            status: "warn".to_string(),
            detail: "未配置 access-token（可选）：无存储空间信息与回收站清理".to_string(),
            hint: Some(
                "如需存储空间预警/清空回收站：浏览器登录酷族 → F12 → Application → Cookies → www.kzwr.com → 复制 access-token 填入设置页"
                    .to_string(),
            ),
        }),
        Some(token) => {
            state.kzwr.set_token(token);
            match state.kzwr.get_member().await {
                Ok(v) if !member_is_login(&v) => {
                    restore_saved_kzwr_token(&state);
                    raise_alert_once(
                        &state,
                        crate::domain::alerts::AlertLevel::Warn,
                        crate::domain::alerts::AlertSource::Kzwr,
                        KZWR_TOKEN_INVALID.to_string(),
                    );
                    items.push(CheckItem {
                        key: "kzwr".to_string(),
                        title: "增强功能".to_string(),
                        status: "fail".to_string(),
                        detail: KZWR_TOKEN_INVALID.to_string(),
                        hint: Some("重新从浏览器 Cookie 复制 access-token".to_string()),
                    });
                }
                Ok(v) => {
                    let data = v.get("data").cloned().unwrap_or_default();
                    let num =
                        |key: &str| data.get(key).and_then(|x| x.as_u64()).unwrap_or(0);
                    let total = num("total").max(num("capacity"));
                    let used = num("use");
                    let pct = if total > 0 {
                        (used as f64 / total as f64 * 100.0).round() as u64
                    } else {
                        0
                    };
                    let threshold = cfg.kzwr.quota_warn_percent;
                    let over = total > 0 && threshold > 0 && pct >= threshold;
                    maybe_warn_quota(&state, total, used);
                    items.push(CheckItem {
                        key: "kzwr".to_string(),
                        title: "增强功能".to_string(),
                        status: "ok".to_string(),
                        detail: format!("access-token 有效；空间已用 {}%", pct),
                        hint: None,
                    });
                    items.push(CheckItem {
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
                Err(e) => items.push(CheckItem {
                    key: "kzwr".to_string(),
                    title: "增强功能".to_string(),
                    status: "warn".to_string(),
                    detail: format!("账号信息获取失败（网络问题？）：{e}"),
                    hint: None,
                }),
            }
        }
    }

    let ok_count = items.iter().filter(|i| i.status == "ok").count();
    let warn_count = items.iter().filter(|i| i.status == "warn").count();
    let fail_count = items.iter().filter(|i| i.status == "fail").count();
    Json(SetupCheckResponse {
        items,
        ok_count,
        warn_count,
        fail_count,
        version: env!("CARGO_PKG_VERSION").to_string(),
        error: None,
    })
}

/// 审计日志查询（最新在前）
async fn audit_list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<AuditQuery>,
) -> Json<AuditResponse> {
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    Json(AuditResponse {
        entries: state.audit.recent(limit),
        error: None,
    })
}

/// 当前 age 公钥（用于展示；永不回传私钥）
async fn keys_get(State(state): State<AppState>) -> Json<KeysInfoResponse> {
    let public_key = state.crypto.get().recipient.to_string();
    // 首次启动自动生成的私钥：一次性交给前端展示，读取后即清空
    let private_key_once = state.pending_key_reveal.lock().unwrap().take();
    Json(KeysInfoResponse {
        public_key,
        private_key_once,
        error: None,
    })
}

/// 更换密钥（用户自定义私钥）：校验格式 → 加密落盘密钥库 → 热切换加密会话
async fn keys_set(
    State(state): State<AppState>,
    Json(body): Json<KeysSetRequest>,
) -> Json<KeysChangeResponse> {
    let trimmed = body.private_key.trim();
    if trimmed.is_empty() {
        return Json(KeysChangeResponse {
            success: false,
            public_key: None,
            private_key: None,
            error: Some("私钥不能为空".to_string()),
        });
    }
    let keys = match crate::domain::crypto::AgeKeys::from_secret_key(trimmed) {
        Ok(k) => k,
        Err(e) => {
            return Json(KeysChangeResponse {
                success: false,
                public_key: None,
                private_key: None,
                error: Some(format!("{:#}", e)),
            })
        }
    };
    let public_key = keys.recipient_str();
    let ks_path = crate::infra::keystore::keystore_path(&state.cfg_dir);
    if let Err(e) = crate::infra::keystore::save_keystore(&keys, &state.passphrase, &ks_path) {
        return Json(KeysChangeResponse {
            success: false,
            public_key: Some(public_key),
            private_key: None,
            error: Some(format!("保存密钥库失败: {:#}", e)),
        });
    }
    state
        .crypto
        .swap(crate::domain::crypto::CryptoSession::full(&keys));
    // 用户粘贴了自己的私钥 ⇒ 其本人已持有，视为已备份
    set_key_backed_up(&state, true);
    state.audit.record(
        "keys.set",
        format!("更换 age 私钥（新公钥 {}）", public_key),
        true,
        None,
    );
    tracing::info!("已更换 age 密钥（用户自定义私钥）");
    Json(KeysChangeResponse {
        success: true,
        public_key: Some(public_key),
        private_key: None,
        error: None,
    })
}

/// 自动生成新密钥对：加密落盘、热切换；私钥仅此一次返回（提醒用户保存）
async fn keys_generate(State(state): State<AppState>) -> Json<KeysChangeResponse> {
    let keys = crate::domain::crypto::AgeKeys::generate();
    let public_key = keys.recipient_str();
    let private_key = keys.to_secret_key();
    let ks_path = crate::infra::keystore::keystore_path(&state.cfg_dir);
    if let Err(e) = crate::infra::keystore::save_keystore(&keys, &state.passphrase, &ks_path) {
        return Json(KeysChangeResponse {
            success: false,
            public_key: Some(public_key),
            private_key: None,
            error: Some(format!("保存密钥库失败: {:#}", e)),
        });
    }
    state
        .crypto
        .swap(crate::domain::crypto::CryptoSession::full(&keys));
    // 新生成的私钥用户尚未保存 ⇒ 重置备份标记，UI 持续提示风险
    set_key_backed_up(&state, false);
    state.audit.record(
        "keys.generate",
        format!("生成新 age 密钥对（公钥 {}）", public_key),
        true,
        None,
    );
    tracing::info!("已自动生成新的 age 密钥对");
    Json(KeysChangeResponse {
        success: true,
        public_key: Some(public_key),
        private_key: Some(private_key),
        error: None,
    })
}

/// 最近告警（监控告警：备份/恢复/配置异常）
async fn alerts_get(State(state): State<AppState>) -> Json<AlertsResponse> {
    Json(AlertsResponse {
        alerts: state.alerts.list(),
        error: None,
    })
}

/// 清空告警
async fn alerts_clear(State(state): State<AppState>) -> Json<AlertsResponse> {
    state.alerts.clear();
    Json(AlertsResponse {
        alerts: Vec::new(),
        error: None,
    })
}

/// 保存告警 Webhook 配置（地址/自定义请求头/请求体模板；地址空 = 关闭外发）
async fn webhook_save(
    State(state): State<AppState>,
    Json(body): Json<WebhookSaveRequest>,
) -> Json<WebhookSaveResponse> {
    let url = body.webhook_url.trim().to_string();
    // 过滤空请求头
    let headers: Vec<crate::infra::config::WebhookHeader> = body
        .headers
        .into_iter()
        .filter(|h| !h.name.trim().is_empty())
        .map(|h| crate::infra::config::WebhookHeader {
            name: h.name.trim().to_string(),
            value: h.value,
        })
        .collect();
    let body_template = body
        .body_template
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let guard = state.config.lock().unwrap();
    let mut cfg = match guard.load() {
        Ok(c) => c,
        Err(_) => crate::infra::config::AppConfig::default(),
    };
    cfg.notify.webhook_url = if url.is_empty() { None } else { Some(url) };
    cfg.notify.webhook_headers = headers;
    cfg.notify.webhook_body = body_template;
    match guard.save(&cfg) {
        Ok(_) => Json(WebhookSaveResponse {
            success: true,
            webhook_url: cfg.notify.webhook_url.clone(),
            headers: cfg.notify.webhook_headers.clone(),
            body_template: cfg.notify.webhook_body.clone(),
            error: None,
        }),
        Err(e) => Json(WebhookSaveResponse {
            success: false,
            webhook_url: None,
            headers: Vec::new(),
            body_template: None,
            error: Some(format!("{:#}", e)),
        }),
    }
}

/// 导出当前私钥明文（敏感操作：需管理员口令校验 + 前端二次确认）
async fn keys_export(
    State(state): State<AppState>,
    Json(body): Json<KeysExportRequest>,
) -> Json<KeysExportResponse> {
    // 校验管理员口令（安装向导设置的应用口令）
    if body.passphrase.as_str() != state.passphrase.expose_secret().as_str() {
        return Json(KeysExportResponse {
            private_key: None,
            error: Some("管理员口令错误".to_string()),
        });
    }
    let ks_path = crate::infra::keystore::keystore_path(&state.cfg_dir);
    match crate::infra::keystore::load_keystore(&state.passphrase, &ks_path) {
        Ok(keys) => {
            // 展示私钥即视为「需重新确认备份」：重置标记，使「我已妥善保存」按钮可再次点击
            set_key_backed_up(&state, false);
            state.audit.record(
                "keys.export",
                "导出 age 私钥明文（管理员口令校验通过）",
                true,
                None,
            );
            Json(KeysExportResponse {
                private_key: Some(keys.to_secret_key()),
                error: None,
            })
        }
        Err(e) => {
            state.audit.record(
                "keys.export",
                format!("导出 age 私钥失败：{:#}", e),
                false,
                None,
            );
            Json(KeysExportResponse {
                private_key: None,
                error: Some(format!("读取密钥库失败: {:#}", e)),
            })
        }
    }
}

/// 确认已妥善备份私钥（消除 UI 的丢失风险提示）
async fn keys_backup_ack(State(state): State<AppState>) -> Json<BackupAckResponse> {
    set_key_backed_up(&state, true);
    state
        .audit
        .record("keys.backup_ack", "确认已妥善保存 age 私钥", true, None);
    Json(BackupAckResponse {
        success: true,
        error: None,
    })
}

/// 测试 Webhook 连通性：用请求中的当前表单配置发送一条测试告警
async fn webhook_test(Json(body): Json<WebhookTestRequest>) -> Json<WebhookTestResponse> {
    let url = body.webhook_url.trim().to_string();
    if url.is_empty() {
        return Json(WebhookTestResponse {
            success: false,
            status: None,
            error: Some("请先填写 Webhook 地址".to_string()),
        });
    }
    let headers: Vec<(String, String)> = body
        .headers
        .into_iter()
        .filter(|h| !h.name.trim().is_empty())
        .map(|h| (h.name.trim().to_string(), h.value))
        .collect();
    let body_template = body
        .body_template
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let alert = crate::domain::alerts::Alert {
        id: 0,
        level: crate::domain::alerts::AlertLevel::Warn,
        source: crate::domain::alerts::AlertSource::Config,
        message: "测试通知：fn-kzwr-backup Webhook 连通性测试".to_string(),
        ts,
    };
    match crate::domain::alerts::send_webhook(url, headers, body_template, alert).await {
        Ok(status) => Json(WebhookTestResponse {
            success: true,
            status: Some(status),
            error: None,
        }),
        Err(e) => Json(WebhookTestResponse {
            success: false,
            status: None,
            error: Some(e),
        }),
    }
}

/// 导出配置（含 WebDAV 凭据明文与 age 私钥；需管理员口令校验）
async fn config_export(
    State(state): State<AppState>,
    Json(body): Json<ConfigExportRequest>,
) -> Json<ConfigExportResponse> {
    if body.passphrase.as_str() != state.passphrase.expose_secret().as_str() {
        return Json(ConfigExportResponse {
            success: false,
            config: None,
            error: Some("管理员口令错误".to_string()),
        });
    }
    let (cfg, creds, kzwr_token) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        let creds = mgr.webdav_credentials().unwrap_or((None, None));
        let kzwr_token = mgr.kzwr_token().unwrap_or(None);
        (cfg, creds, kzwr_token)
    };
    let ks_path = crate::infra::keystore::keystore_path(&state.cfg_dir);
    let age_private_key = crate::infra::keystore::load_keystore(&state.passphrase, &ks_path)
        .ok()
        .map(|k| k.to_secret_key());
    let exported_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let bundle = ConfigBundle {
        version: 1,
        exported_at,
        backup_paths: cfg.backup.paths.clone(),
        target_folder: cfg.backup.target_folder.clone(),
        schedule_cron: cfg.backup.schedule_cron.clone(),
        retention: Some(cfg.backup.retention.clone()),
        webhook_url: cfg.notify.webhook_url.clone(),
        webhook_headers: cfg.notify.webhook_headers.clone(),
        webhook_body: cfg.notify.webhook_body.clone(),
        webdav_url: cfg.webdav.url.clone(),
        webdav_username: creds.0,
        webdav_password: creds.1,
        kzwr_access_token: kzwr_token,
        age_private_key,
        key_backed_up: cfg.keys.backed_up,
    };
    match serde_json::to_string_pretty(&bundle) {
        Ok(text) => Json(ConfigExportResponse {
            success: true,
            config: Some(text),
            error: None,
        }),
        Err(e) => Json(ConfigExportResponse {
            success: false,
            config: None,
            error: Some(format!("序列化配置失败: {e}")),
        }),
    }
}

/// 导入配置（覆盖备份/通知/WebDAV 凭据，可选恢复 age 私钥；需管理员口令校验）
async fn config_import(
    State(state): State<AppState>,
    Json(body): Json<ConfigImportRequest>,
) -> Json<ConfigImportResponse> {
    if body.passphrase.as_str() != state.passphrase.expose_secret().as_str() {
        return Json(ConfigImportResponse {
            success: false,
            error: Some("管理员口令错误".to_string()),
        });
    }
    let bundle: ConfigBundle = match serde_json::from_str(&body.config) {
        Ok(b) => b,
        Err(e) => {
            return Json(ConfigImportResponse {
                success: false,
                error: Some(format!("配置解析失败: {e}")),
            })
        }
    };

    // 1) 更新配置（保留未提供的字段；WebDAV 凭据用当前口令重新加密）
    {
        let mgr = state.config.lock().unwrap();
        let mut cfg = mgr.load().unwrap_or_default();
        cfg.backup.paths = bundle.backup_paths.clone();
        if !bundle.target_folder.trim().is_empty() {
            cfg.backup.target_folder = bundle.target_folder.clone();
        }
        cfg.backup.schedule_cron = bundle
            .schedule_cron
            .clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(r) = bundle.retention.clone() {
            cfg.backup.retention = r;
        }
        cfg.notify.webhook_url = bundle
            .webhook_url
            .clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        cfg.notify.webhook_headers = bundle.webhook_headers.clone();
        cfg.notify.webhook_body = bundle
            .webhook_body
            .clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        cfg.keys.backed_up = bundle.key_backed_up;
        if let (Some(u), Some(p)) = (
            bundle.webdav_username.as_ref().filter(|s| !s.is_empty()),
            bundle.webdav_password.as_ref().filter(|s| !s.is_empty()),
        ) {
            match (mgr.encrypt_field(u), mgr.encrypt_field(p)) {
                (Ok(eu), Ok(ep)) => {
                    cfg.webdav.username_enc = Some(eu);
                    cfg.webdav.password_enc = Some(ep);
                }
                _ => {
                    return Json(ConfigImportResponse {
                        success: false,
                        error: Some("WebDAV 凭据加密失败".to_string()),
                    })
                }
            }
        }
        if let Some(url) = bundle
            .webdav_url
            .as_ref()
            .filter(|s| !s.trim().is_empty())
        {
            cfg.webdav.url = Some(url.trim_end_matches('/').to_string());
        }
        // kzwr access-token（增强功能，可选）
        if let Some(token) = bundle.kzwr_access_token.as_ref().filter(|s| !s.trim().is_empty()) {
            match mgr.encrypt_field(token.trim()) {
                Ok(e) => cfg.kzwr.access_token_enc = Some(e),
                Err(_) => {
                    return Json(ConfigImportResponse {
                        success: false,
                        error: Some("kzwr access-token 加密失败".to_string()),
                    })
                }
            }
        }
        if let Err(e) = mgr.save(&cfg) {
            return Json(ConfigImportResponse {
                success: false,
                error: Some(format!("保存配置失败: {:#}", e)),
            });
        }
    }

    // 1.5) 同步内存中的 kzwr access-token（热更新，无需重启）
    match kzwr_token_of(&state) {
        Some(t) => state.kzwr.set_token(t),
        None => state.kzwr.clear_token(),
    }

    // 2) 可选：恢复 age 私钥（热切换，无需重启）
    if let Some(pk) = bundle
        .age_private_key
        .as_ref()
        .filter(|s| !s.trim().is_empty())
    {
        match crate::domain::crypto::AgeKeys::from_secret_key(pk.trim()) {
            Ok(keys) => {
                let ks_path = crate::infra::keystore::keystore_path(&state.cfg_dir);
                if let Err(e) =
                    crate::infra::keystore::save_keystore(&keys, &state.passphrase, &ks_path)
                {
                    return Json(ConfigImportResponse {
                        success: false,
                        error: Some(format!("保存密钥库失败: {:#}", e)),
                    });
                }
                state
                    .crypto
                    .swap(crate::domain::crypto::CryptoSession::full(&keys));
                tracing::info!("已从导入配置恢复 age 密钥");
            }
            Err(e) => {
                return Json(ConfigImportResponse {
                    success: false,
                    error: Some(format!("私钥无效: {:#}", e)),
                })
            }
        }
    }

    // 3) 热切换 WebDAV 目标
    if let (Some(u), Some(p)) = (
        bundle.webdav_username.clone().filter(|s| !s.is_empty()),
        bundle.webdav_password.clone().filter(|s| !s.is_empty()),
    ) {
        let url = bundle
            .webdav_url
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| crate::infra::target::webdav::DEFAULT_URL.to_string());
        state
            .target
            .swap(Arc::new(crate::infra::target::webdav::WebdavTarget::new(
                &url, u, p,
            )));
        tracing::info!("已从导入配置切换 WebDAV 目标");
    }

    state.audit.record(
        "config.import",
        "导入配置包（备份路径/目标/定时/通知/WebDAV 凭据/kzwr token）",
        true,
        None,
    );
    Json(ConfigImportResponse {
        success: true,
        error: None,
    })
}

/// 构建应用路由（不含 /api 前缀，由 main.rs nest("/api") 统一加前缀）
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/plugins", get(plugins_list))
        .route("/ws", get(ws::ws_handler))
        .route("/webdav/config", post(webdav_save))
        .route("/user/info", get(user_info))
        .route("/config", get(config_get).post(config_save))
        .route("/config/export", post(config_export))
        .route("/config/import", post(config_import))
        .route("/backup/run", post(backup_run))
        .route("/restore/files", get(restore_files))
        .route("/restore/tree", get(restore_tree))
        .route("/restore/run", post(restore_run))
        .route("/restore/prune", post(restore_prune))
        .route("/logs", get(logs_get))
        .route("/logs/clear", post(logs_clear))
        .route("/logs/download", get(logs_download))
        .route("/audit/clear", post(audit_clear))
        .route("/kzwr/user", get(kzwr_user))
        .route("/kzwr/token", post(kzwr_token_save))
        .route("/kzwr/trash/empty", post(kzwr_trash_empty))
        .route("/schedule/preview", post(schedule_preview))
        .route("/setup/check", get(setup_check))
        .route("/audit", get(audit_list))
        .route("/keys", get(keys_get).post(keys_set))
        .route("/keys/generate", post(keys_generate))
        .route("/keys/export", post(keys_export))
        .route("/keys/backup-ack", post(keys_backup_ack))
        .route("/alerts", get(alerts_get).delete(alerts_clear))
        .route("/notify/webhook", post(webhook_save))
        .route("/notify/webhook/test", post(webhook_test))
        .with_state(state)
}
