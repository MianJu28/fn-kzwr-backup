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
    /// 是否启用外置插件（动态库）加载
    pub plugins_enabled: bool,
    /// 自定义插件目录（空 = 用默认目录）
    pub plugins_dir: String,
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

impl From<&RetentionView> for crate::infra::config::RetentionConfig {
    fn from(v: &RetentionView) -> Self {
        Self {
            enabled: v.enabled,
            cleanup_unmanaged: v.cleanup_unmanaged,
            min_age_days: v.min_age_days,
            empty_recycle_bin: v.empty_recycle_bin,
            recycle_max_gb: v.recycle_max_gb,
            recycle_min_age_days: v.recycle_min_age_days,
        }
    }
}

/// 目标保存请求（`id` 缺省 = 新建；`password` 缺省 = 不修改凭据）
#[derive(Deserialize)]
pub struct TargetSaveRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 保存前是否实测连通性（默认 true）
    #[serde(default = "default_test_true")]
    pub test: bool,
}

fn default_test_true() -> bool {
    true
}

/// 任务保存请求（未传字段保持原值；`id` 缺省 = 新建）
#[derive(Deserialize)]
pub struct TaskSaveRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub target_folder: Option<String>,
    #[serde(default)]
    pub schedule_cron: Option<String>,
    #[serde(default)]
    pub retention: Option<RetentionView>,
}

/// 任务删除请求（`purge=true` 同时清理该任务的快照记录）
#[derive(Deserialize, Default)]
pub struct TaskDeleteRequest {
    #[serde(default)]
    pub purge: bool,
}

/// 配置保存请求
#[derive(Deserialize)]
pub struct ConfigSaveRequest {
    /// 备份源路径（**不传 = 保持原值**：插件开关等局部保存不应误清空路径）
    #[serde(default)]
    pub backup_paths: Option<Vec<String>>,
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
    /// 外置插件加载开关（不传则保持原值；**改动需重启应用生效**）
    #[serde(default)]
    pub plugins_enabled: Option<bool>,
    /// 自定义插件目录（`:` 分隔多个；空串 = 用默认目录）
    #[serde(default)]
    pub plugins_dir: Option<String>,
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
    /// 指定任务 id（缺省 = 自动按源路径在所有任务中查找）
    #[serde(default)]
    pub task: Option<String>,
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
    /// 所属任务 id（多任务）
    #[serde(default)]
    pub task_id: String,
    /// 所属任务名
    #[serde(default)]
    pub task_name: String,
    /// 该任务的目标名
    #[serde(default)]
    pub target_name: String,
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
    /// 备份源路径（本地目录，须在某个任务的源列表中）
    pub source: String,
    /// 要展开的目录相对路径（空 / 缺省 = 根层级）
    #[serde(default)]
    pub dir: String,
    /// 指定任务 id（缺省 = 自动按源路径在所有任务中查找）
    #[serde(default)]
    pub task: Option<String>,
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
    /// 多目标（含明文凭据；导入时用当前口令重新加密）
    #[serde(default)]
    pub targets: Vec<BundleTarget>,
    /// 多任务（源路径 + 目标引用 + cron + 保留策略）
    #[serde(default)]
    pub tasks: Vec<crate::infra::config::TaskConfig>,
    /// 插件自管数据（明文键值对；敏感，导出需管理员口令，导入重新加密）
    #[serde(default)]
    pub plugin_data: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, String>,
    >,
}

/// 导出/导入包里的单个目标（**含明文凭据**，仅存于导出的 JSON 文本）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BundleTarget {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default = "default_test_true")]
    pub enabled: bool,
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
///
/// `pub(crate)`：增强插件（如 kzwr）也用它上报告警。
pub(crate) fn raise_alert(
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
///
/// 返回项包含 `available`（是否已可用）与 `ui`（设置页/概览页区块描述），
/// 前端据此决定渲染哪些卡片、顺序如何、用内置组件还是 `blocks` 通用渲染。
async fn plugins_list(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mgr = state.config.lock().unwrap();
    let cfg = mgr.load().unwrap_or_default();
    let env_override = crate::plugin::loader::enabled_by_env();
    // 孤立数据检测：`plugin_data` 里有记录但没有对应已加载插件的 id → 前端提示「清理遗留配置」
    let loaded = state.plugins.plugin_ids();
    let orphan_data: Vec<String> = mgr
        .plugin_data_ids(&cfg)
        .into_iter()
        .filter(|id| !loaded.contains(id))
        .collect();
    Json(serde_json::json!({
        "plugins": state.plugins.describe(&cfg),
        // 有自管数据但插件未加载的 id（卸载残留；前端据此提示清理）
        "orphan_data": orphan_data,
        // 外置插件（ADR-013 方案 B：动态库）的开关/目录/加载诊断
        "external": {
            "configured": cfg.plugins.enabled,
            "env_override": env_override,
            "enabled": env_override.unwrap_or(cfg.plugins.enabled),
            "dir": cfg.plugins.dir.clone().unwrap_or_default(),
            "dirs": state.plugins.plugin_dirs()
                .iter()
                .map(|(p, s)| serde_json::json!({ "path": p, "source": s }))
                .collect::<Vec<_>>(),
            "reports": state.plugins.external_reports(),
        }
    }))
}

/// 设置某个目标插件的上传并发路数（并发回传是**每插件**各自的能力与开关）
#[derive(Deserialize, Default)]
pub struct PluginParallelRequest {
    /// 0/1 = 关闭并发回传（顺序上传）；≥2 = 启用，该值即并发路数
    #[serde(default)]
    pub parallel: Option<u32>,
}

/// `POST /api/plugins/:id/parallel`：设置**该插件**的上传并发路数
///
/// 并发回传按插件分别配置（`plugins.target_parallel[插件 id]`），只有声明支持
/// `supports_plan` 的目标插件可设置。保存后目标池热重建，下次备份即生效。
async fn plugin_parallel(
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
/// → 调插件 `destroy`（若实现，插件自身状态清理）→ 删除该 id 的 `plugin_data` 命名空间
/// → 记审计。动态库句柄由宿主保活到进程结束，此处不卸载 `.so` 本身。
async fn plugin_purge(
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

    // 3) 删除该 id 的 `plugin_data` 命名空间
    let removed = mgr.plugin_data_remove(&mut cfg, &id);
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

// ── 多任务 / 多目标（ADR-014）辅助 ──────────────────────────────────────

/// 通用 JSON 错误响应（`{success:false, error}`）
fn err(e: impl std::fmt::Display) -> serde_json::Value {
    serde_json::json!({ "success": false, "error": e.to_string() })
}

/// 生成稳定 id（前缀 + 时间戳/序号十六进制，无需第三方 uuid 依赖）
fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{:x}{:02x}", ms & 0xffff_ffff, seq & 0xff)
}

/// 默认任务 id：首个启用的任务，否则首个任务
fn default_task_id(cfg: &crate::infra::config::AppConfig) -> Option<String> {
    cfg.tasks
        .iter()
        .find(|t| t.enabled)
        .or_else(|| cfg.tasks.first())
        .map(|t| t.id.clone())
}

/// 任务运行上下文：目标适配器 + 快照分桶用的账号
pub(crate) struct TaskContext {
    pub task: crate::infra::config::TaskConfig,
    pub target: Arc<dyn crate::infra::storage_trait::TargetStorage>,
    /// 目标任务凭据的用户名（快照按此分桶，实现「每目标独立快照」）
    pub account: Option<String>,
    /// 目标任务是否已就绪（凭据齐备）
    pub ready: bool,
}

/// 取某个任务的运行上下文（任务不存在/未启用/目标未就绪 → Err(提示)）
pub(crate) fn task_context(state: &AppState, task_id: &str) -> Result<TaskContext, String> {
    let (task, target_cfg) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().map_err(|e| format!("{:#}", e))?;
        let task = cfg
            .task_by_id(task_id)
            .cloned()
            .ok_or_else(|| format!("任务不存在：{task_id}"))?;
        let tcfg = cfg.target_by_id(&task.target_id).cloned();
        (task, tcfg)
    };
    if !task.enabled {
        return Err(format!(
            "任务「{}」已停用",
            if task.name.is_empty() { &task.id } else { &task.name }
        ));
    }
    let target_cfg = target_cfg.ok_or_else(|| {
        format!(
            "任务「{}」引用的目标不存在（{}），请在「任务管理」中重新选择",
            if task.name.is_empty() { &task.id } else { &task.name },
            task.target_id
        )
    })?;
    let account = {
        let mgr = state.config.lock().unwrap();
        mgr.target_credentials(&target_cfg)
            .ok()
            .and_then(|(u, _)| u)
    };
    let ready = target_cfg.enabled && state.targets.is_ready(&task.target_id);
    Ok(TaskContext {
        target: state.target_for(&task.target_id),
        task,
        account,
        ready,
    })
}

/// 目标视图（供 `/api/targets`；凭据永不返回，密码只回显「是否已设置」）
fn target_view(
    state: &AppState,
    t: &crate::infra::config::TargetConfig,
) -> serde_json::Value {
    let (user, pass_set) = {
        let mgr = state.config.lock().unwrap();
        match mgr.target_credentials(t) {
            Ok((u, p)) => (u, p.is_some()),
            Err(_) => (None, false),
        }
    };
    serde_json::json!({
        "id": t.id,
        "name": t.name,
        "kind": t.kind,
        "url": t.url.clone().unwrap_or_default(),
        "username": user,
        "password_set": pass_set,
        "enabled": t.enabled,
        "ready": state.targets.is_ready(&t.id),
        "backend": state.targets.describe(&t.id),
        "tasks": 0, // 由调用方填充（引用该目标的任务数）
    })
}

/// 任务视图（供 `/api/tasks`）
fn task_view(state: &AppState, t: &crate::infra::config::TaskConfig) -> serde_json::Value {
    let target_name = {
        let mgr = state.config.lock().unwrap();
        mgr.load()
            .ok()
            .and_then(|c| c.target_by_id(&t.target_id).map(|x| x.name.clone()))
            .unwrap_or_default()
    };
    let next = crate::domain::scheduler::next_runs(
        t.schedule_cron.as_deref().unwrap_or_default(),
        3,
    )
    .unwrap_or_default();
    serde_json::json!({
        "id": t.id,
        "name": t.name,
        "enabled": t.enabled,
        "paths": t.paths,
        "target_id": t.target_id,
        "target_name": target_name,
        "target_ready": state.targets.is_ready(&t.target_id),
        "target_folder": t.target_folder,
        "schedule_cron": t.schedule_cron.clone().unwrap_or_default(),
        "schedule_next": next,
        "retention": RetentionView::from(&t.retention),
    })
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
    let Some(plugin) = state.plugins.target_plugin("webdav") else {
        return Json(WebdavSaveResponse {
            success: false,
            url: None,
            warning: None,
            error: Some("未注册 webdav 目标插件".to_string()),
        });
    };
    let url = match plugin
        .verify(body.url.as_deref(), body.username.trim(), &body.password)
        .await
    {
        Ok(u) => u,
        Err(e) => {
            return Json(WebdavSaveResponse {
                success: false,
                url: None,
                warning: None,
                error: Some(e),
            })
        }
    };

    // 2) 加密保存（写入**主目标**；保存后同步兼容镜像）
    let saved = {
        let mgr = state.config.lock().unwrap();
        mgr.save_webdav(&url, body.username.trim(), &body.password)
    };
    match saved {
        Ok(_) => {
            // 热刷新目标池（无需重启服务）：由注册表按当前配置重新装配全部目标
            let cfg = { state.config.lock().unwrap().load().unwrap_or_default() };
            state.reload_targets(&cfg);
            // 账号一致性：已配置 access-token 时，核对 WebDAV 账号与 API 账号
            // （该能力属于 kzwr 增强插件）
            let warning = crate::plugin::builtin::kzwr::check_account_consistency(&state).await;
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


/// 由配置构造响应（含 cron 校验）
fn config_response(
    cfg: &crate::infra::config::AppConfig,
    webdav_configured: bool,
    webdav_username: Option<String>,
    kzwr_token_configured: bool,
    error: Option<String>,
) -> ConfigResponse {
    // 兼容视图：以「首个任务」为默认任务（多任务管理走 `/api/tasks`）
    let task = cfg.tasks.first();
    let schedule = task
        .and_then(|t| t.schedule_cron.clone())
        .unwrap_or_default();
    let valid = crate::domain::scheduler::validate_cron(&schedule).is_ok();
    let schedule_next = crate::domain::scheduler::next_runs(&schedule, 5).unwrap_or_default();
    let retention = task
        .map(|t| &t.retention)
        .unwrap_or(&cfg.backup.retention);
    ConfigResponse {
        backup_paths: task.map(|t| t.paths.clone()).unwrap_or_default(),
        target_folder: task
            .map(|t| t.target_folder.clone())
            .unwrap_or_else(|| cfg.backup.target_folder.clone()),
        schedule_cron: schedule,
        schedule_cron_valid: valid,
        webdav_configured,
        webdav_url: cfg.primary_target().and_then(|t| t.url.clone()),
        webdav_username,
        retention: RetentionView::from(retention),
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
        plugins_enabled: cfg.plugins.enabled,
        plugins_dir: cfg.plugins.dir.clone().unwrap_or_default(),
        error,
    }
}

/// 判断**主目标**是否已配置（启用 + 用户名 + 密码齐全）
fn webdav_ready(cfg: &crate::infra::config::AppConfig) -> bool {
    cfg.primary_target()
        .map(|t| t.enabled && t.configured())
        .unwrap_or(false)
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
            plugins_enabled: false,
            plugins_dir: String::new(),
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
    // 兼容接口：写「首个任务」（不存在则创建默认任务）；多任务走 `/api/tasks`
    if cfg.tasks.is_empty() {
        let mut t = crate::infra::config::TaskConfig::default();
        t.id = crate::infra::config::DEFAULT_TASK_ID.to_string();
        t.name = "默认任务".to_string();
        t.target_id = cfg
            .primary_target()
            .map(|x| x.id.clone())
            .unwrap_or_else(|| crate::infra::config::DEFAULT_TARGET_ID.to_string());
        t.target_folder = cfg.backup.target_folder.clone();
        cfg.tasks.push(t);
    }
    // 定时 cron：先校验（错误时需借用 cfg 生成响应，故在可变借用前完成）
    let cron_update: Option<Option<String>> = match body.schedule_cron {
        Some(cron) => {
            let cron = cron.trim().to_string();
            if let Err(e) = crate::domain::scheduler::validate_cron(&cron) {
                let resp =
                    config_response(&cfg, webdav_ready(&cfg), wuser, kzwr_on, Some(e.to_string()));
                return Json(resp);
            }
            Some(if cron.is_empty() { None } else { Some(cron) })
        }
        None => None,
    };

    {
        let task = cfg.tasks.first_mut().expect("刚保证非空");
        if let Some(paths) = body.backup_paths.clone() {
            task.paths = paths;
        }
        if let Some(folder) = body.target_folder {
            if !folder.trim().is_empty() {
                task.target_folder = folder;
            }
        }
        // 保留策略：仅更新传入的字段（未传保持原值）
        if let Some(v) = body.retention_enabled {
            task.retention.enabled = v;
        }
        if let Some(v) = body.retention_cleanup_unmanaged {
            task.retention.cleanup_unmanaged = v;
        }
        if let Some(v) = body.retention_min_age_days {
            task.retention.min_age_days = v;
        }
        if let Some(v) = body.retention_empty_recycle_bin {
            task.retention.empty_recycle_bin = v;
        }
        if let Some(v) = body.retention_recycle_max_gb {
            task.retention.recycle_max_gb = v;
        }
        if let Some(v) = body.retention_recycle_min_age_days {
            task.retention.recycle_min_age_days = v;
        }
        if let Some(cron) = cron_update {
            task.schedule_cron = cron;
        }
    }
    if let Some(v) = body.kzwr_quota_warn_percent {
        // 合法区间 0..=100（0 = 关闭预警）
        cfg.kzwr.quota_warn_percent = v.min(100);
    }
    // 调试日志开关：保存并即时生效（日志过滤器热更新）
    if let Some(v) = body.debug {
        cfg.debug = v;
    }
    // 外置插件开关/目录：只落盘（插件在启动时装配，重启后生效）
    if let Some(v) = body.plugins_enabled {
        cfg.plugins.enabled = v;
    }
    if let Some(v) = body.plugins_dir {
        let v = v.trim().to_string();
        cfg.plugins.dir = if v.is_empty() { None } else { Some(v) };
    }
    match cfg_guard.save(&cfg) {
        Ok(_) => {
            let t = cfg.tasks.first().cloned().unwrap_or_default();
            state.audit.record(
                "config.save",
                format!(
                    "保存配置（任务 {}）：目标目录 {}，路径 {} 个，定时 {}，保留策略[启用={} 孤儿={} 天数={} 回收站={} ≥{}GB >{}天]",
                    t.name,
                    t.target_folder,
                    t.paths.len(),
                    t.schedule_cron
                        .clone()
                        .unwrap_or_else(|| "未启用".to_string()),
                    t.retention.enabled,
                    t.retention.cleanup_unmanaged,
                    t.retention.min_age_days,
                    t.retention.empty_recycle_bin,
                    t.retention.recycle_max_gb,
                    t.retention.recycle_min_age_days,
                ),
                true,
                None,
            );
            // 调试日志开关热更新（保存成功后立即切换日志级别）
            crate::apply_log_debug(cfg.debug);
            drop(cfg_guard);
            state.reload_targets(&cfg);
            Json(config_response(&cfg, webdav_ready(&cfg), wuser, kzwr_on, None))
        }
        Err(e) => Json(config_response(&cfg, false, wuser, kzwr_on, Some(format!("{:#}", e)))),
    }
}

/// 当前 WebDAV 账号：取**主目标**的解密用户名
pub(crate) fn webdav_username(state: &AppState) -> Option<String> {
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

// ── 目标管理 API（多目标，ADR-014）────────────────────────────────────

/// `GET /api/targets`：目标列表（凭据不回传；含就绪状态与引用它的任务数）
async fn targets_list(State(state): State<AppState>) -> Json<serde_json::Value> {
    let (targets, tasks) = {
        let mgr = state.config.lock().unwrap();
        match mgr.load() {
            Ok(c) => (c.targets, c.tasks),
            Err(_) => (Vec::new(), Vec::new()),
        }
    };
    let list: Vec<serde_json::Value> = targets
        .iter()
        .map(|t| {
            let mut v = target_view(&state, t);
            let used = tasks.iter().filter(|x| x.target_id == t.id).count();
            v["tasks"] = serde_json::json!(used);
            v
        })
        .collect();
    Json(serde_json::json!({ "targets": list }))
}

/// `POST /api/targets`：新建/更新目标（保存前实测连通性；`password` 缺省 = 不修改）
async fn target_save(
    State(state): State<AppState>,
    Json(body): Json<TargetSaveRequest>,
) -> Json<serde_json::Value> {
    let kind = body.kind.clone().unwrap_or_else(|| "webdav".to_string());
    let Some(plugin) = state.plugins.target_plugin(&kind) else {
        return Json(err(format!("未注册类型为 {kind} 的目标插件")));
    };

    let (mut cfg, existing) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        let existing = body
            .id
            .as_deref()
            .and_then(|id| cfg.target_by_id(id).cloned());
        (cfg, existing)
    };

    let id = existing
        .as_ref()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| new_id("t"));
    let url = body
        .url
        .clone()
        .unwrap_or_else(|| existing.as_ref().and_then(|t| t.url.clone()).unwrap_or_default());
    let name = body
        .name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| existing.as_ref().map(|t| t.name.clone()))
        .unwrap_or_else(|| format!("目标 {id}"));

    // 凭据：本次提供则实测 + 加密；未提供则沿用原凭据（编辑场景不改密码）
    //
    // 注意：实测是 await，**绝不能持有 config 锁**（MutexGuard 跨 await 不但阻塞其它请求，
    // 且再次加锁会自锁），因此这里按「先实测、后加密」两步走。
    let provided_user = body
        .username
        .as_deref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let (enc_user, enc_pass, warning) = match provided_user {
        Some(user) => {
            let pass = body.password.clone().unwrap_or_default();
            if pass.is_empty() {
                return Json(err("新填写用户名时必须同时提供密码"));
            }
            let mut warning = None;
            if body.test {
                match plugin.verify(Some(&url), &user, &pass).await {
                    Ok(real) => warning = Some(format!("连接成功：{real}")),
                    Err(e) => return Json(err(e)),
                }
            }
            let mgr = state.config.lock().unwrap();
            match (mgr.encrypt_field(&user), mgr.encrypt_field(&pass)) {
                (Ok(u), Ok(p)) => (Some(u), Some(p), warning),
                _ => return Json(err("凭据加密失败")),
            }
        }
        None => {
            let old = existing.as_ref();
            if old.map(|t| t.configured()).unwrap_or(false) {
                (
                    old.and_then(|t| t.username_enc.clone()),
                    old.and_then(|t| t.password_enc.clone()),
                    None,
                )
            } else {
                return Json(err("新目标必须填写用户名与密码"));
            }
        }
    };

    let target = crate::infra::config::TargetConfig {
        id: id.clone(),
        name,
        kind,
        url: Some(url.trim_end_matches('/').to_string()),
        username_enc: enc_user,
        password_enc: enc_pass,
        enabled: body.enabled.unwrap_or(true),
    };
    match cfg.targets.iter_mut().find(|t| t.id == id) {
        Some(slot) => *slot = target,
        None => cfg.targets.push(target),
    }
    let saved = { state.config.lock().unwrap().save(&cfg) };
    if let Err(e) = saved {
        return Json(err(format!("{:#}", e)));
    }
    state.reload_targets(&cfg);
    let t = cfg.target_by_id(&id).cloned().unwrap_or_default();
    state.audit.record(
        "target.save",
        format!("保存目标「{}」（{}）", t.name, t.url.clone().unwrap_or_default()),
        true,
        None,
    );
    Json(serde_json::json!({ "success": true, "target": target_view(&state, &t), "warning": warning, "error": null }))
}

/// `POST /api/targets/:id/delete`：删除目标（被任务引用时拒绝）
async fn target_delete(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<serde_json::Value> {
    let mut cfg = { state.config.lock().unwrap().load().unwrap_or_default() };
    let using = cfg.tasks_using_target(&id);
    if !using.is_empty() {
        return Json(err(format!(
            "该目标正被任务使用（{}），请先修改或删除这些任务",
            using.join("、")
        )));
    }
    let before = cfg.targets.len();
    cfg.targets.retain(|t| t.id != id);
    if cfg.targets.len() == before {
        return Json(err("目标不存在"));
    }
    if let Err(e) = state.config.lock().unwrap().save(&cfg) {
        return Json(err(format!("{:#}", e)));
    }
    state.reload_targets(&cfg);
    state.audit.record("target.delete", format!("删除目标 {id}"), true, None);
    Json(serde_json::json!({ "success": true, "error": null }))
}

/// `POST /api/targets/:id/test`：用已保存的凭据实测连通性
async fn target_test(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<serde_json::Value> {
    let (target, creds) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        let Some(t) = cfg.target_by_id(&id).cloned() else {
            return Json(err("目标不存在"));
        };
        let creds = mgr.target_credentials(&t).unwrap_or((None, None));
        (t, creds)
    };
    let Some(plugin) = state.plugins.target_plugin(&target.kind) else {
        return Json(err(format!("未注册类型为 {} 的目标插件", target.kind)));
    };
    let (Some(user), Some(pass)) = creds else {
        return Json(err("该目标尚未配置用户名/密码"));
    };
    match plugin.verify(target.url.as_deref(), &user, &pass).await {
        Ok(url) => Json(serde_json::json!({ "success": true, "url": url, "error": null })),
        Err(e) => Json(err(e)),
    }
}

// ── 任务管理 API（多任务，ADR-014）────────────────────────────────────

/// `GET /api/tasks`：任务列表（含目标名、就绪状态、下次触发时间）
async fn tasks_list(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cfg = { state.config.lock().unwrap().load().unwrap_or_default() };
    let list: Vec<serde_json::Value> = cfg.tasks.iter().map(|t| task_view(&state, t)).collect();
    Json(serde_json::json!({
        "tasks": list,
        "targets": cfg.targets.iter().map(|t| serde_json::json!({
            "id": t.id, "name": t.name, "ready": state.targets.is_ready(&t.id), "enabled": t.enabled,
        })).collect::<Vec<_>>(),
    }))
}

/// `POST /api/tasks`：新建/更新任务（未传字段保持原值）
async fn task_save(
    State(state): State<AppState>,
    Json(body): Json<TaskSaveRequest>,
) -> Json<serde_json::Value> {
    let mut cfg = { state.config.lock().unwrap().load().unwrap_or_default() };
    // cron 先校验（错误时直接返回，不动配置）
    if let Some(cron) = &body.schedule_cron {
        if let Err(e) = crate::domain::scheduler::validate_cron(cron) {
            return Json(err(e.to_string()));
        }
    }
    let existing = body
        .id
        .as_deref()
        .and_then(|id| cfg.task_by_id(id).cloned());
    let id = existing
        .as_ref()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| new_id("k"));
    let target_id = body
        .target_id
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| existing.as_ref().map(|t| t.target_id.clone()))
        .or_else(|| cfg.primary_target().map(|t| t.id.clone()))
        .unwrap_or_else(|| crate::infra::config::DEFAULT_TARGET_ID.to_string());
    if cfg.target_by_id(&target_id).is_none() {
        return Json(err(format!("目标不存在：{target_id}")));
    }
    let task = crate::infra::config::TaskConfig {
        id: id.clone(),
        name: body
            .name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| existing.as_ref().map(|t| t.name.clone()))
            .unwrap_or_else(|| format!("任务 {id}")),
        enabled: body
            .enabled
            .unwrap_or_else(|| existing.as_ref().map(|t| t.enabled).unwrap_or(true)),
        paths: body
            .paths
            .clone()
            .or_else(|| existing.as_ref().map(|t| t.paths.clone()))
            .unwrap_or_default(),
        target_id,
        target_folder: body
            .target_folder
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| existing.as_ref().map(|t| t.target_folder.clone()))
            .unwrap_or_else(|| "fn-backup".to_string()),
        schedule_cron: match &body.schedule_cron {
            Some(cron) => {
                let cron = cron.trim().to_string();
                if cron.is_empty() {
                    None
                } else {
                    Some(cron)
                }
            }
            None => existing.as_ref().and_then(|t| t.schedule_cron.clone()),
        },
        retention: body
            .retention
            .as_ref()
            .map(crate::infra::config::RetentionConfig::from)
            .or_else(|| existing.as_ref().map(|t| t.retention.clone()))
            .unwrap_or_default(),
    };
    match cfg.tasks.iter_mut().find(|t| t.id == id) {
        Some(slot) => *slot = task,
        None => cfg.tasks.push(task),
    }
    if let Err(e) = state.config.lock().unwrap().save(&cfg) {
        return Json(err(format!("{:#}", e)));
    }
    let saved = cfg.task_by_id(&id).cloned().unwrap_or_default();
    state.audit.record(
        "task.save",
        format!(
            "保存任务「{}」：{} 个源，目标 {}，定时 {}",
            saved.name,
            saved.paths.len(),
            saved.target_id,
            saved.schedule_cron.clone().unwrap_or_else(|| "未启用".to_string())
        ),
        true,
        None,
    );
    Json(serde_json::json!({ "success": true, "task": task_view(&state, &saved), "error": null }))
}

/// `POST /api/tasks/:id/delete`：删除任务（`purge=true` 同时删除它的快照记录）
async fn task_delete(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: Option<Json<TaskDeleteRequest>>,
) -> Json<serde_json::Value> {
    let purge = body.map(|b| b.purge).unwrap_or(false);
    let mut cfg = { state.config.lock().unwrap().load().unwrap_or_default() };
    let before = cfg.tasks.len();
    cfg.tasks.retain(|t| t.id != id);
    if cfg.tasks.len() == before {
        return Json(err("任务不存在"));
    }
    if let Err(e) = state.config.lock().unwrap().save(&cfg) {
        return Json(err(format!("{:#}", e)));
    }
    let purged = if purge {
        // 快照 key = "{task_id}-{源序号}"：按前缀清理该任务的全部记录
        state.store.delete_task_snapshots(&id).unwrap_or(0)
    } else {
        0
    };
    state.audit.record(
        "task.delete",
        format!("删除任务 {id}（清理快照记录 {purged} 条）"),
        true,
        None,
    );
    Json(serde_json::json!({ "success": true, "purged": purged, "error": null }))
}

/// `POST /api/tasks/:id/run`：立即执行该任务
async fn task_run(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<BackupResponse> {
    let resp = run_task_now(&state, &id).await;
    state.audit.record(
        "task.run",
        match (&resp.error, resp.skipped) {
            (Some(e), _) => format!("任务 {id} 备份失败：{e}"),
            (None, true) => format!("任务 {id} 备份跳过（已有备份在执行）"),
            (None, false) => format!(
                "任务 {id} 备份完成：上传 {} 个文件（{}）",
                resp.uploaded,
                human_bytes(resp.uploaded_bytes)
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

/// 执行一次备份（兼容入口：跑**默认任务**）
pub async fn run_backup_now(state: &AppState) -> BackupResponse {
    let task_id = {
        let mgr = state.config.lock().unwrap();
        mgr.load().ok().and_then(|c| default_task_id(&c))
    };
    match task_id {
        Some(id) => run_task_now(state, &id).await,
        None => {
            raise_alert(
                state,
                crate::domain::alerts::AlertLevel::Warn,
                crate::domain::alerts::AlertSource::Backup,
                "备份未执行：尚未创建任何备份任务".to_string(),
            );
            BackupResponse {
                error: Some("尚未创建任何备份任务，请先在「任务管理」中新建".to_string()),
                ..Default::default()
            }
        }
    }
}

/// 执行**指定任务**的一次备份（可被 HTTP handler 与定时调度器复用）
///
/// 返回 BackupResponse（含 uploaded/deleted/orphan_removed/skipped/error）。
///
/// 快照隔离：`job_id = "{task.id}-{源序号}"`、`account = 目标任务凭据的用户名`，
/// 因此同一份源在不同任务/目标上互不干扰，各自增量。
///
/// **并发互斥**：所有任务共用 `AppState.backup_running`，同一时刻只允许一个备份执行
/// （避免 NAS 带宽/IO 争用）；已有备份在跑时立即返回 `skipped = true`。
pub async fn run_task_now(state: &AppState, task_id: &str) -> BackupResponse {
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

    // 1) 解析任务与它的目标（未就绪 → 明确提示，不静默失败）
    let ctx = match task_context(state, task_id) {
        Ok(c) => c,
        Err(e) => {
            raise_alert(
                state,
                crate::domain::alerts::AlertLevel::Warn,
                crate::domain::alerts::AlertSource::Config,
                format!("备份未执行：{e}"),
            );
            return BackupResponse {
                error: Some(e),
                ..Default::default()
            };
        }
    };
    let task_label = if ctx.task.name.is_empty() {
        ctx.task.id.clone()
    } else {
        ctx.task.name.clone()
    };
    if !ctx.ready {
        let msg = format!("任务「{task_label}」的目标未配置凭据，请在「目标管理」中完善后重试");
        raise_alert(
            state,
            crate::domain::alerts::AlertLevel::Warn,
            crate::domain::alerts::AlertSource::Config,
            msg.clone(),
        );
        return BackupResponse {
            error: Some(msg),
            ..Default::default()
        };
    }

    let paths: Vec<PathBuf> = ctx.task.paths.iter().map(PathBuf::from).collect();
    if paths.is_empty() {
        let msg = format!("任务「{task_label}」未配置备份路径");
        raise_alert(
            state,
            crate::domain::alerts::AlertLevel::Warn,
            crate::domain::alerts::AlertSource::Backup,
            msg.clone(),
        );
        return BackupResponse {
            error: Some(msg),
            ..Default::default()
        };
    }

    // 2) 保留策略：启用时构造 RetentionPolicy，备份完成后清理该目标上的孤儿文件
    let target_folder = ctx.task.target_folder.clone();
    let retention_cfg = ctx.task.retention.clone();
    let retention = if retention_cfg.enabled && retention_cfg.cleanup_unmanaged {
        let rt = crate::domain::retention::RetentionPolicy::new(ctx.target.clone(), &target_folder);
        Some(if retention_cfg.min_age_days > 0 {
            rt.with_min_age_secs(retention_cfg.min_age_days * 86400)
        } else {
            rt
        })
    } else {
        None
    };

    // 3) 执行多路径备份（job_id 前缀 = 任务 id → 每任务独立快照）
    let job_id = ctx.task.id.clone();
    let account = ctx.account.clone();
    let job = BackupJob {
        job_id: job_id.clone(),
        account: account.clone(),
        source: Arc::new(crate::infra::source::local::LocalFsSource::new(&paths[0])),
        target: ctx.target.clone(),
        crypto: state.crypto.get(),
        store: state.store.clone(),
        target_prefix: Some(target_folder),
        eventbus: Some(state.eventbus.clone()),
        retention,
    };
    tracing::info!(task = %task_label, target = %ctx.task.target_id, paths = paths.len(), "开始执行备份任务");
    match job.run_multi(&paths).await {
        Ok(summary) => {
            // 保留策略可选：跟随清空云端回收站（由增强插件实现，如 kzwr）
            let trash_emptied = if retention_cfg.empty_recycle_bin {
                let mut n = 0u64;
                for p in state.plugins.enhance_plugins() {
                    if let Some(c) = p.after_backup(state).await {
                        n += c;
                    }
                }
                n as usize
            } else {
                0
            };
            // 备份后云端占用会变化：异步触发一次插件巡检（空间预警，不阻塞本次响应）
            {
                let quota_state = state.clone();
                tokio::spawn(async move {
                    for p in quota_state.plugins.enhance_plugins() {
                        p.patrol(&quota_state).await;
                    }
                });
            }
            tracing::info!(
                task = %task_label,
                uploaded = summary.uploaded,
                deleted = summary.deleted,
                "备份任务完成"
            );
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
                format!("备份失败（任务 {task_label}）：{msg}"),
            );
            // 发布 Failed 终态事件：否则前端任务面板停留在「进行中」永不结束
            state.eventbus.task_event(
                crate::eventbus::TaskKind::Backup,
                crate::eventbus::TaskStatus::Failed,
                None,
                job_id,
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

/// 恢复范围：把「源路径」定位到某个任务内的某个源（多任务下同一路径可能出现在多个任务）
struct RestoreScope {
    task: crate::infra::config::TaskConfig,
    job_id: String,
    /// 快照分桶账号 = 该任务目标的用户名（未配置时 ''，兼容旧数据）
    account: String,
    target: Arc<dyn crate::infra::storage_trait::TargetStorage>,
    target_folder: String,
    ready: bool,
}

/// 定位「源路径 → 恢复范围」
///
/// 匹配顺序：指定任务（`task_id`）→ 按启用任务遍历 → 按全部任务遍历；
/// 同一任务内先精确匹配路径、再按目录名匹配。
fn find_restore_scope(
    state: &AppState,
    source: &str,
    task_id: Option<&str>,
) -> Result<RestoreScope, String> {
    let (cfg, creds_by_target) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().map_err(|e| format!("{:#}", e))?;
        let mut creds = std::collections::HashMap::new();
        for t in &cfg.targets {
            let user = mgr
                .target_credentials(t)
                .ok()
                .and_then(|(u, _)| u)
                .unwrap_or_default();
            creds.insert(t.id.clone(), user);
        }
        (cfg, creds)
    };

    let root = std::path::Path::new(source);
    let match_idx = |t: &crate::infra::config::TaskConfig| -> Option<usize> {
        t.paths
            .iter()
            .position(|p| std::path::Path::new(p) == root)
            .or_else(|| {
                root.file_name()
                    .and_then(|n| t.paths.iter().position(|p| std::path::Path::new(p).file_name() == Some(n)))
            })
    };

    let candidates: Vec<&crate::infra::config::TaskConfig> = match task_id {
        Some(id) => cfg
            .tasks
            .iter()
            .filter(|t| t.id == id)
            .collect(),
        None => cfg
            .tasks
            .iter()
            .filter(|t| t.enabled)
            .chain(cfg.tasks.iter().filter(|t| !t.enabled))
            .collect(),
    };
    let task = candidates
        .into_iter()
        .find(|t| match_idx(t).is_some())
        .ok_or_else(|| "该路径不在任何备份任务的源列表中".to_string())?;
    let idx = match_idx(task).expect("find 已保证命中");
    tracing::debug!(task = %task.id, idx, source, "定位恢复范围");
    Ok(RestoreScope {
        job_id: format!("{}-{}", task.id, idx),
        account: creds_by_target
            .get(&task.target_id)
            .cloned()
            .unwrap_or_default(),
        target: state.target_for(&task.target_id),
        target_folder: task.target_folder.clone(),
        ready: state.targets.is_ready(&task.target_id),
        task: task.clone(),
    })
}

/// 按备份源路径取出其快照条目（多任务：可传 `task_id` 指定任务）
fn snapshot_entries_for_source(
    state: &AppState,
    source: &str,
    task_id: Option<&str>,
) -> Result<Vec<crate::infra::persistence::snapshot::SnapshotEntry>, String> {
    let scope = find_restore_scope(state, source, task_id)?;
    state
        .store
        .load_snapshot(&scope.job_id, &scope.account)
        .map_err(|e| format!("读取备份快照失败: {:#}", e))
}

/// 列出全部任务的全部源文件夹及其可恢复概况（计数来自 SQLite 快照，明细按需懒加载）
async fn restore_files(State(state): State<AppState>) -> Json<RestoreFilesResponse> {
    let cfg = { state.config.lock().unwrap().load().unwrap_or_default() };
    let mut folders = Vec::new();

    for task in &cfg.tasks {
        let account = {
            let mgr = state.config.lock().unwrap();
            cfg.target_by_id(&task.target_id)
                .and_then(|t| mgr.target_credentials(t).ok())
                .and_then(|(u, _)| u)
                .unwrap_or_default()
        };
        for (i, path) in task.paths.iter().enumerate() {
            // 每个任务、每个源的快照 key = "{task.id}-{i}"
            let job_id = format!("{}-{}", task.id, i);
            let entries = state.store.load_snapshot(&job_id, &account).unwrap_or_default();
            let agg = SnapshotAgg::build(&entries);
            let (file_count, dir_count, total_bytes) = agg.get("");
            folders.push(RestorableFolder {
                path: path.clone(),
                has_backup: file_count > 0,
                file_count,
                dir_count,
                total_bytes,
                task_id: task.id.clone(),
                task_name: if task.name.is_empty() {
                    task.id.clone()
                } else {
                    task.name.clone()
                },
                target_name: cfg
                    .target_by_id(&task.target_id)
                    .map(|t| t.name.clone())
                    .unwrap_or_default(),
            });
        }
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
    let entries = match snapshot_entries_for_source(&state, &q.source, q.task.as_deref()) {
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
    // 恢复目标根：优先用前端传入的 source_path（备份源路径，恢复到原位置），否则用默认目录
    let (mut files, restore_root, restore_all, restore_dir, task_hint) = match body {
        Some(Json(req)) => (
            req.files.unwrap_or_default(),
            req.source_path
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| state.default_restore_dir.to_string_lossy().into_owned()),
            req.all,
            req.dir
                .map(|d| d.trim_matches('/').to_string())
                .filter(|d| !d.is_empty()),
            req.task.clone(),
        ),
        None => (
            Vec::new(),
            state.default_restore_dir.to_string_lossy().into_owned(),
            false,
            None,
            None,
        ),
    };

    // 备份时每个源目录在目标端以其文件夹名分目录存放，恢复需还原该层级
    let source_root_name = std::path::Path::new(&restore_root)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned());

    // 定位该源路径所属的任务与目标（多任务：可指定 task_id）
    let scope = find_restore_scope(&state, &restore_root, task_hint.as_deref()).ok();

    // 目标就绪校验：按实际命中的任务目标判断（多目标）；未命中任务时退回主目标
    let ready = match &scope {
        Some(s) => s.ready,
        None => {
            state.primary_ready()
                || {
                    let mgr = state.config.lock().unwrap();
                    mgr.load().map(|c| webdav_ready(&c)).unwrap_or(false)
                }
        }
    };
    if !ready {
        raise_alert(
            &state,
            crate::domain::alerts::AlertLevel::Warn,
            crate::domain::alerts::AlertSource::Config,
            "恢复未执行：目标未配置".to_string(),
        );
        return Json(RestoreResponse {
            restored: 0,
            restored_bytes: 0,
            error: Some("目标未配置，请先在「目标管理」中填写地址与凭据".to_string()),
            missing: Vec::new(),
        });
    }

    // 读取快照元数据：rel_path -> (明文大小, 原始 mtime 秒)
    // 用途：① 展示恢复总大小；② 恢复后回写快照，避免下次备份重复上传
    let mut meta: std::collections::HashMap<String, (u64, i64)> =
        std::collections::HashMap::new();
    let mut snapshot_target = None;
    if let Some(scope) = &scope {
        if let Ok(entries) = state.store.load_snapshot(&scope.job_id, &scope.account) {
            for e in entries {
                if !e.is_dir {
                    meta.insert(e.rel_path.clone(), (e.size, e.mtime_secs));
                }
            }
        }
        snapshot_target = Some(crate::domain::restore::RestoreSnapshotTarget {
            store: state.store.clone(),
            job_id: scope.job_id.clone(),
            account: scope.account.clone(),
        });
    }

    // 「全部恢复」：忽略前端传入的文件列表，取该源路径（可限定子目录）快照中的全部文件
    if restore_all {
        if scope.is_none() {
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

    // 目标与目标端前缀按命中的任务取（未命中任务时退回主目标/默认前缀）
    let (restore_target, restore_prefix): (
        Arc<dyn crate::infra::storage_trait::TargetStorage>,
        String,
    ) = match &scope {
        Some(s) => (s.target.clone(), s.target_folder.clone()),
        None => (state.target.clone(), state.target_folder.clone()),
    };
    let job = RestoreJob {
        target: restore_target,
        crypto: state.crypto.get(),
        target_prefix: Some(restore_prefix),
        source_root_name,
        eventbus: Some(state.eventbus.clone()),
        meta,
        snapshot_target,
    };
    match job.run(&files, std::path::Path::new(&restore_root)).await {
        Ok(summary) => {
            let task_label = scope
                .as_ref()
                .map(|s| {
                    if s.task.name.is_empty() {
                        s.task.id.clone()
                    } else {
                        s.task.name.clone()
                    }
                })
                .unwrap_or_else(|| "(未归属任务)".to_string());
            state.audit.record(
                "restore.run",
                format!(
                    "恢复到 {}（任务 {}）：{} 个文件（{}）",
                    restore_root,
                    task_label,
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
    /// 指定任务 id（缺省 = 自动按源路径在所有任务中查找）
    #[serde(default)]
    pub task: Option<String>,
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

    // 定位该源路径所属的任务与目标（多任务：可指定 task_id）
    let scope = match find_restore_scope(&state, &body.source_path, body.task.as_deref()) {
        Ok(s) => s,
        Err(e) => return Json(err(e)),
    };
    if !scope.ready {
        return Json(err("该任务的目标未配置凭据，请先在「目标管理」中完善"));
    }
    let root = std::path::Path::new(&body.source_path);
    let target_folder = scope.target_folder.clone();
    let job_id = scope.job_id.clone();
    let account = scope.account.clone();
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
pub(crate) fn raise_alert_once(
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
pub(crate) fn human_bytes(bytes: u64) -> String {
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

    // 2) 目标配置 + 实连（多目标：逐个检查）
    let (cfg, target_creds) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        let mut map = std::collections::HashMap::new();
        for t in &cfg.targets {
            map.insert(t.id.clone(), mgr.target_credentials(t).unwrap_or((None, None)));
        }
        (cfg, map)
    };
    if cfg.targets.iter().all(|t| !t.enabled) {
        items.push(CheckItem {
            key: "webdav".to_string(),
            title: "备份目标".to_string(),
            status: "fail".to_string(),
            detail: "尚未启用任何备份目标，备份与恢复不可用".to_string(),
            hint: Some(
                "前往「目标管理」新增目标并填写账号与应用密码；应用密码请在 https://www.kzwr.com/account/apps 创建，选择「永不过期」与读写权限"
                    .to_string(),
            ),
        });
    } else {
        let mut ok_list: Vec<String> = Vec::new();
        let mut fail_list: Vec<String> = Vec::new();
        for t in cfg.targets.iter().filter(|t| t.enabled) {
            let label = if t.name.is_empty() {
                t.id.clone()
            } else {
                t.name.clone()
            };
            let (user, pass) = target_creds.get(&t.id).cloned().unwrap_or((None, None));
            match (user, pass) {
                (Some(u), Some(p)) => {
                    let url = t
                        .url
                        .clone()
                        .unwrap_or_else(|| crate::infra::target::webdav::DEFAULT_URL.to_string());
                    match crate::infra::target::webdav::WebdavTarget::new(&url, &u, &p)
                        .ping()
                        .await
                    {
                        Ok(_) => ok_list.push(format!("{label}（{url}）")),
                        Err(e) => fail_list.push(format!("{label}：{e:#}")),
                    }
                }
                _ => fail_list.push(format!("{label}：未配置凭据")),
            }
        }
        let (status, detail, hint) = if fail_list.is_empty() {
            (
                "ok",
                format!("已连接 {} 个目标：{}", ok_list.len(), ok_list.join("、")),
                None,
            )
        } else if ok_list.is_empty() {
            (
                "fail",
                format!("所有目标均不可用：{}", fail_list.join("；")),
                Some("请确认应用密码未过期且有读写权限（可在 https://www.kzwr.com/account/apps 重新创建）".to_string()),
            )
        } else {
            (
                "warn",
                format!(
                    "{} 个目标可用；{} 个异常：{}",
                    ok_list.len(),
                    fail_list.len(),
                    fail_list.join("；")
                ),
                Some("前往「目标管理」逐个测试并修复异常目标".to_string()),
            )
        };
        items.push(CheckItem {
            key: "webdav".to_string(),
            title: "备份目标".to_string(),
            status: status.to_string(),
            detail,
            hint,
        });
    }

    // 3) 备份路径 + 已备份文件数（多任务：按任务各自的目标账号统计）
    let total_paths: usize = cfg.tasks.iter().map(|t| t.paths.len()).sum();
    if total_paths == 0 {
        items.push(CheckItem {
            key: "paths".to_string(),
            title: "备份任务".to_string(),
            status: "fail".to_string(),
            detail: "尚未添加任何备份任务/路径".to_string(),
            hint: Some("前往「任务管理」新建任务并添加要备份的文件夹".to_string()),
        });
    } else {
        let mut files = 0usize;
        let mut tasks_with_backup = 0usize;
        for task in &cfg.tasks {
            let account = target_creds
                .get(&task.target_id)
                .and_then(|(u, _)| u.clone())
                .unwrap_or_default();
            let mut task_files = 0usize;
            for i in 0..task.paths.len() {
                let job_id = format!("{}-{}", task.id, i);
                if let Ok(entries) = state.store.load_snapshot(&job_id, &account) {
                    task_files += entries.iter().filter(|e| !e.is_dir).count();
                }
            }
            files += task_files;
            if task_files > 0 {
                tasks_with_backup += 1;
            }
        }
        items.push(CheckItem {
            key: "paths".to_string(),
            title: "备份任务".to_string(),
            status: if files > 0 { "ok" } else { "warn" }.to_string(),
            detail: if files > 0 {
                format!(
                    "{} 个任务 / {} 个路径；其中 {} 个任务已备份，共 {} 个文件",
                    cfg.tasks.len(),
                    total_paths,
                    tasks_with_backup,
                    files
                )
            } else {
                format!("{} 个任务 / {} 个路径，但还没有备份记录", cfg.tasks.len(), total_paths)
            },
            hint: if files > 0 {
                None
            } else {
                Some("前往「任务管理」执行一次备份".to_string())
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

    // 5) 定时备份（多任务：汇总各任务启用的 cron）
    let scheduled: Vec<(String, String)> = cfg
        .tasks
        .iter()
        .filter(|t| t.enabled)
        .filter_map(|t| {
            let cron = t.schedule_cron.clone().unwrap_or_default();
            let cron = cron.trim().to_string();
            if cron.is_empty() {
                None
            } else {
                Some((
                    if t.name.is_empty() {
                        t.id.clone()
                    } else {
                        t.name.clone()
                    },
                    cron,
                ))
            }
        })
        .collect();
    if scheduled.is_empty() {
        items.push(CheckItem {
            key: "schedule".to_string(),
            title: "定时备份".to_string(),
            status: "warn".to_string(),
            detail: "所有任务均未启用定时（仅手动备份）".to_string(),
            hint: Some("如需无人值守，可在「任务管理」为任务设置 cron 表达式".to_string()),
        });
    } else {
        let detail = scheduled
            .iter()
            .map(|(name, cron)| {
                let next = crate::domain::scheduler::next_runs(cron, 1)
                    .unwrap_or_default()
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "—".to_string());
                format!("{name}: {cron}（下次 {next}）")
            })
            .collect::<Vec<_>>()
            .join("；");
        items.push(CheckItem {
            key: "schedule".to_string(),
            title: "定时备份".to_string(),
            status: "ok".to_string(),
            detail: format!(
                "{} 个任务已启用（{}）；{}",
                scheduled.len(),
                crate::domain::scheduler::timezone_label(),
                detail
            ),
            hint: None,
        });
    }

    // 6) 增强插件自检（如 kzwr：access-token 与云端空间）——检查项由插件自己产出
    for p in state.plugins.enhance_plugins() {
        for o in p.health_check(&state, &cfg).await {
            items.push(CheckItem {
                key: o.key,
                title: o.title,
                status: o.status,
                detail: o.detail,
                hint: o.hint,
            });
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
    let (cfg, creds, kzwr_token, bundle_targets) = {
        let mgr = state.config.lock().unwrap();
        let cfg = mgr.load().unwrap_or_default();
        let creds = mgr.webdav_credentials().unwrap_or((None, None));
        let kzwr_token = mgr.kzwr_token().unwrap_or(None);
        // 全部目标的明文凭据（导出文件本身即敏感件，此函数入口已校验管理员口令）
        let bundle_targets: Vec<BundleTarget> = cfg
            .targets
            .iter()
            .map(|t| {
                let (u, p) = mgr.target_credentials(t).unwrap_or((None, None));
                BundleTarget {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    kind: t.kind.clone(),
                    url: t.url.clone(),
                    username: u,
                    password: p,
                    enabled: t.enabled,
                }
            })
            .collect();
        (cfg, creds, kzwr_token, bundle_targets)
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
        targets: bundle_targets,
        tasks: cfg.tasks.clone(),
        plugin_data: (|| {
            let mgr_ref = state.config.lock().unwrap();
            let mut out = std::collections::BTreeMap::new();
            for p in mgr_ref.plugin_data_ids(&cfg) {
                if let Ok(kv) = mgr_ref.plugin_data_export(&cfg, &p) {
                    if !kv.is_empty() {
                        out.insert(p, kv);
                    }
                }
            }
            out
        })(),
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
        // 多目标（凭据用当前口令重新加密）
        if !bundle.targets.is_empty() {
            let mut targets = Vec::new();
            for t in &bundle.targets {
                let enc = |v: &Option<String>| -> Result<Option<String>, String> {
                    match v.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                        Some(s) => mgr
                            .encrypt_field(&s)
                            .map(Some)
                            .map_err(|_| "目标凭据加密失败".to_string()),
                        None => Ok(None),
                    }
                };
                let username_enc = match enc(&t.username) {
                    Ok(v) => v,
                    Err(e) => return Json(ConfigImportResponse { success: false, error: Some(e) }),
                };
                let password_enc = match enc(&t.password) {
                    Ok(v) => v,
                    Err(e) => return Json(ConfigImportResponse { success: false, error: Some(e) }),
                };
                targets.push(crate::infra::config::TargetConfig {
                    id: if t.id.is_empty() { new_id("t") } else { t.id.clone() },
                    name: t.name.clone(),
                    kind: if t.kind.is_empty() {
                        "webdav".to_string()
                    } else {
                        t.kind.clone()
                    },
                    url: t.url.clone(),
                    username_enc,
                    password_enc,
                    enabled: t.enabled,
                });
            }
            cfg.targets = targets;
        }
        // 多任务（target_id 失效时回落到首个目标，避免导入后任务不可运行）
        if !bundle.tasks.is_empty() {
            let ids: Vec<String> = cfg.targets.iter().map(|t| t.id.clone()).collect();
            let fallback = ids
                .first()
                .cloned()
                .unwrap_or_else(|| crate::infra::config::DEFAULT_TARGET_ID.to_string());
            cfg.tasks = bundle
                .tasks
                .iter()
                .map(|t| {
                    let mut t = t.clone();
                    if !ids.contains(&t.target_id) {
                        t.target_id = fallback.clone();
                    }
                    t
                })
                .collect();
        }
        // 插件自管数据（明文键值对重新用当前口令加密；未携带则不覆盖）
        if !bundle.plugin_data.is_empty() {
            cfg.plugin_data.clear();
            for (plugin, kv) in &bundle.plugin_data {
                for (k, v) in kv {
                    if let Err(e) = mgr.plugin_data_set(&mut cfg, plugin, k, v) {
                        return Json(ConfigImportResponse {
                            success: false,
                            error: Some(format!("插件自管数据加密失败: {:#}", e)),
                        });
                    }
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
    for p in state.plugins.enhance_plugins() {
        p.reload(&state).await;
    }

    // 1.6) 目标池重建（导入可能改了目标/任务）
    {
        let cfg = { state.config.lock().unwrap().load().unwrap_or_default() };
        state.reload_targets(&cfg);
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
///
/// 核心只挂**自身**路由；增强功能的接口由插件自带（见文件末尾对
/// `EnhancePlugin::routes()` 的挂载，统一前缀 `/api/p/<插件id>`）。
pub fn router(state: AppState) -> Router {
    let mut core = Router::new()
        .route("/health", get(health))
        .route("/plugins", get(plugins_list))
        .route("/plugins/:id/purge", post(plugin_purge))
        .route("/plugins/:id/parallel", post(plugin_parallel))
        .route("/ws", get(ws::ws_handler))
        .route("/webdav/config", post(webdav_save))
        .route("/user/info", get(user_info))
        .route("/config", get(config_get).post(config_save))
        .route("/config/export", post(config_export))
        .route("/config/import", post(config_import))
        // 多目标 / 多任务（ADR-014）
        .route("/targets", get(targets_list).post(target_save))
        .route("/targets/:id/delete", post(target_delete))
        .route("/targets/:id/test", post(target_test))
        .route("/tasks", get(tasks_list).post(task_save))
        .route("/tasks/:id/delete", post(task_delete))
        .route("/tasks/:id/run", post(task_run))
        .route("/backup/run", post(backup_run))
        .route("/restore/files", get(restore_files))
        .route("/restore/tree", get(restore_tree))
        .route("/restore/run", post(restore_run))
        .route("/restore/prune", post(restore_prune))
        .route("/logs", get(logs_get))
        .route("/logs/clear", post(logs_clear))
        .route("/logs/download", get(logs_download))
        .route("/audit/clear", post(audit_clear))
        .route("/schedule/preview", post(schedule_preview))
        .route("/setup/check", get(setup_check))
        .route("/audit", get(audit_list))
        .route("/keys", get(keys_get).post(keys_set))
        .route("/keys/generate", post(keys_generate))
        .route("/keys/export", post(keys_export))
        .route("/keys/backup-ack", post(keys_backup_ack))
        .route("/alerts", get(alerts_get).delete(alerts_clear))
        .route("/notify/webhook", post(webhook_save))
        .route("/notify/webhook/test", post(webhook_test));

    // 插件路由：`/p/<插件id>/…`（如 /p/kzwr/user）；核心不感知具体插件的路径
    for p in state.plugins.enhance_plugins() {
        core = core.nest(&format!("/p/{}", p.meta().id), p.routes());
    }
    core.with_state(state)
}
