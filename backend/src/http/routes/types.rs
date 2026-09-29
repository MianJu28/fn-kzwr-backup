//! HTTP 请求 / 响应 DTO
//!
//! 集中放这里的原因：它们**成对**使用（一个请求配一个响应），
//! 散落在各 handler 文件里时，改字段要在多个文件之间跳。
//! 全部 `pub(super)`：只给 `routes` 子树用，不对外暴露。

use serde::{Deserialize, Serialize};

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
    /// 插件签名公钥（base64 的 32 字节 Ed25519 公钥，可多个）
    ///
    /// 公钥不是秘密（可公开分发），故明文回显，便于用户在设置页增删。
    pub plugins_pubkeys: Vec<String>,
    /// 是否放行未签名插件（仅本机调试：来自 `FN_KZWR_PLUGINS_ALLOW_UNSIGNED`）
    ///
    /// 只读项：仅回显**环境变量**状态，不提供前端开关（决策 3 要求默认强制验签，
    /// 否则一次误操作就可能让验签形同虚设）。
    pub plugins_allow_unsigned: bool,
    /// 被**按插件禁用**的插件 id（运行时启停；见 `PluginSettings::disabled`）
    pub plugins_disabled: Vec<String>,
    /// **每个插件文件对应的公钥**（文件名 → base64 公钥；一插件一公钥）
    pub plugins_plugin_pubkeys: std::collections::BTreeMap<String, String>,
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
    /// **插件自定义字段**（键 → 值）
    ///
    /// 键名由目标插件的 `describe_json.target.form` 声明；`url`/`username`/`password`
    /// 三个 well-known 键也可以从这里传（会被归位到既有存储），其余键存入
    /// 该目标自己的 `TargetConfig.fields`。
    ///
    /// 未出现在本次请求里的键**保持原值**（与 `password` 缺省不改语义一致）。
    #[serde(default)]
    pub fields: std::collections::BTreeMap<String, String>,
}

pub(super) fn default_test_true() -> bool {
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
    /// 调试日志开关（不传则保持原值）
    #[serde(default)]
    pub debug: Option<bool>,
    /// 外置插件加载开关（不传则保持原值；**改动需重启应用生效**）
    #[serde(default)]
    pub plugins_enabled: Option<bool>,
    /// 自定义插件目录（`:` 分隔多个；空串 = 用默认目录）
    #[serde(default)]
    pub plugins_dir: Option<String>,
    /// 插件签名公钥（base64 的 32 字节 Ed25519 公钥；**改动需重启应用生效**）
    ///
    /// 传空数组 = 清空公钥（此时默认策略会拒绝加载任何外置插件，除非设了
    /// `FN_KZWR_PLUGINS_ALLOW_UNSIGNED=1`）。不传则保持原值。
    #[serde(default)]
    pub plugins_pubkeys: Option<Vec<String>>,
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
    /// 该目标的上传并发度（`null` = 未设置）；随包导出/导入，避免换机后退回默认值
    #[serde(default)]
    pub parallel: Option<u32>,
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

/// 设置某个目标插件的上传并发路数（并发回传是**每插件**各自的能力与开关）
#[derive(Deserialize, Default)]
pub struct PluginParallelRequest {
    /// 0/1 = 关闭并发回传（顺序上传）；≥2 = 启用，该值即并发路数
    #[serde(default)]
    pub parallel: Option<u32>,
}

/// 安装请求（内容与签名都用 base64，避免引入 multipart 依赖）
#[derive(Deserialize, Default)]
pub struct PluginInstallRequest {
    /// 目标文件名，必须是 `*.so`（如 `libmy_plugin.so`）
    #[serde(default)]
    pub file_name: String,
    /// `.so` 内容的 base64
    #[serde(default)]
    pub data_b64: String,
    /// `<so>.sig` 内容的 base64（64 字节裸 Ed25519 签名）
    #[serde(default)]
    pub sig_b64: String,
    /// 用于校验的公钥（base64 的 32 字节 Ed25519 公钥）
    #[serde(default)]
    pub pubkey: String,
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
