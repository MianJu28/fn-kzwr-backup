//! 酷族网盘（kzwr.com）API 客户端 —— **插件自带**
//!
//! 迁移前这块代码在宿主里（`backend/src/infra/kzwr_api/`），现在完整搬到插件内：
//! 核心不再知道 kzwr 的存在，只按稳定 C ABI 调本插件。
//!
//! 只保留增强功能**真正用到**的四个端点（原客户端还有登录/上传/下载/分享等
//! 几十个方法，那些属于 WebDAV 主链路，与本插件无关）：
//!
//! - `GET  /api/v2/member`                → 是否已登录 + 空间用量（total/use）
//! - `GET  /api/v2/user/trash?page=N`     → 回收站分页列表
//! - `POST /api/v2/files/delete/physical` → 物理删除文件（body `{"Pids":[…]}`）
//! - `POST /api/v2/folder/delete/physical`→ 物理删除文件夹（body `{"FolderIds":[…]}`）
//!
//! 认证：请求头 `access-token`（浏览器登录后从 Cookie 复制）。

use std::time::Duration;

use serde_json::{json, Value};

/// kzwr 站点地址（原 `infra/kzwr_api/constants.rs::BASE_URL`）
const BASE_URL: &str = "https://www.kzwr.com";

/// 全局请求超时。原宿主取 3600s 是为大文件分片 PUT 服务的，
/// 本插件只有轻量的 JSON 查询/删除，用 60s 更能及时暴露网络问题。
const TIMEOUT_SECS: u64 = 60;

/// 网络错误 / 5xx 的重试次数（4xx 是确定性错误，不重试）
const RETRY_ATTEMPTS: u32 = 3;

/// 单个端点的调用结果
pub type ApiResult<T> = Result<T, String>;

/// 客户端（无状态：token 由调用方按账号传入，故可全局共享一个连接池）
///
/// `Clone` 是廉价的：`reqwest::Client` 内部就是 `Arc`，克隆共享同一个连接池。
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
}

impl Client {
    /// 取**全局共享**的客户端
    ///
    /// 为什么不能每次 `new()`：`reqwest::Client` 内部持有连接池与 DNS 缓存，
    /// 每个动作新建一个就让 keep-alive 完全失效（每次请求都要重新 TCP+TLS 握手）。
    /// 而本插件是「一个动作里对多个账号各发若干个请求」的模式，
    /// 建连开销会成倍放大；`rt.rs` 的注释也明确要求「建一次全局复用」。
    ///
    /// 本结构无状态（token 按账号现传），因此天然可共享。
    pub fn new() -> Self {
        static SHARED: std::sync::OnceLock<Client> = std::sync::OnceLock::new();
        SHARED
            .get_or_init(Self::build)
            .clone()
    }

    fn build() -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .default_headers({
                let mut h = reqwest::header::HeaderMap::new();
                h.insert(
                    reqwest::header::CONTENT_TYPE,
                    "application/json; charset=utf-8".parse().unwrap(),
                );
                h.insert(
                    reqwest::header::ACCEPT,
                    "application/json".parse().unwrap(),
                );
                h.insert(
                    reqwest::header::USER_AGENT,
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                     (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36"
                        .parse()
                        .unwrap(),
                );
                h
            })
            .build()
            .expect("kzwr 插件：构建 HTTP 客户端失败");
        Self { http }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", BASE_URL, path)
    }

    /// `GET /api/v2/member`：登录态 + 空间用量
    pub async fn get_member(&self, token: &str) -> ApiResult<Value> {
        self.get_json("/api/v2/member", token, &[]).await
    }

    /// `GET /api/v2/user/trash?page=N`：回收站分页
    pub async fn get_trash(&self, token: &str, page: u32) -> ApiResult<Value> {
        self.get_json("/api/v2/user/trash", token, &[("page", page.to_string())])
            .await
    }

    /// 物理删除回收站里的**文件**（用条目的 `encodedId`，不是普通列表的 `sid`）
    pub async fn delete_trash_files(&self, token: &str, pids: &[String]) -> ApiResult<Value> {
        self.post_json("/api/v2/files/delete/physical", token, json!({ "Pids": pids }))
            .await
    }

    /// 物理删除回收站里的**文件夹**
    pub async fn delete_trash_folders(
        &self,
        token: &str,
        folder_ids: &[String],
    ) -> ApiResult<Value> {
        self.post_json(
            "/api/v2/folder/delete/physical",
            token,
            json!({ "FolderIds": folder_ids }),
        )
        .await
    }

    async fn get_json(
        &self,
        path: &str,
        token: &str,
        params: &[(&str, String)],
    ) -> ApiResult<Value> {
        let what = format!("GET {path}");
        self.with_retry(&what, || {
            let mut req = self.http.get(self.url(path));
            for (k, v) in params {
                req = req.query(&[(k, v.as_str())]);
            }
            req.header("access-token", token)
        })
        .await
    }

    async fn post_json(&self, path: &str, token: &str, body: Value) -> ApiResult<Value> {
        let what = format!("POST {path}");
        self.with_retry(&what, || {
            self.http
                .post(self.url(path))
                .header("access-token", token)
                .json(&body)
        })
        .await
    }

    /// 发送并在网络错误 / 5xx 时重试（线性退避）
    async fn with_retry<F>(&self, what: &str, build: F) -> ApiResult<Value>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            // 闭包只做请求构造（同步、不 panic）；发送与解析的错误都走下面的分支
            // 重试判定必须**结构化**，不能靠错误字符串前缀：
            // 5xx 若带 JSON 消息，`parse_response` 返回的是那个消息文本（不以
            // "HTTP 5" 开头）⇒ 旧逻辑会**漏掉**本该重试的服务端故障；
            // 反过来「读响应失败」这类明确网络错误又不在前缀表里 ⇒ 也不重试。
            let (result, retryable): (ApiResult<Value>, bool) = match build().send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.as_u16() >= 500 {
                        // 消费掉响应体让连接可复用（否则每次重试都要重新建连）
                        let _ = resp.bytes().await;
                        (Err(format!("服务端错误 HTTP {status}")), true)
                    } else {
                        // 4xx 是确定性错误（token 失效/参数错），重试没有意义
                        let retryable = false;
                        (parse_response(resp).await, retryable)
                    }
                }
                // 构造/发送失败：网络类，可重试
                Err(e) => (Err(format!("网络请求失败: {e}")), true),
            };
            match result {
                Ok(v) => return Ok(v),
                Err(msg) => {
                    if retryable && attempt < RETRY_ATTEMPTS {
                        tokio::time::sleep(Duration::from_millis(500_u64 * attempt as u64)).await;
                        continue;
                    }
                    return Err(format!("{what} 失败: {msg}"));
                }
            }
        }
    }
}

/// 解析响应：>=400 转成错误（401 明确提示 token 无效），其它原样返回 JSON
async fn parse_response(resp: reqwest::Response) -> ApiResult<Value> {
    let status = resp.status();
    let body_text = match resp.text().await {
        Ok(t) => t,
        Err(e) => return Err(format!("读取响应失败: {e}")),
    };
    // 2xx 但**不是 JSON** ⇒ 一定是异常（站点改版 / 风控页 / 网关 HTML 错误页）。
    //
    // 旧实现把它包成 `Value::String` 继续往上走，于是：
    // - `member_is_login` 取不到 `data.isLogin` → 保守返回 `true`（当成已登录）；
    // - `member_space` 取不到字段 → 全是 0；
    // ⇒ 健康检查输出「token 有效；空间已用 0%」而巡检因 `total == 0` 直接跳过。
    // 结果是**站点坏了但界面全绿、空间预警彻底失效且无人知晓**。
    if status.as_u16() < 400 && serde_json::from_str::<Value>(&body_text).is_err() {
        let head: String = body_text.chars().take(120).collect();
        return Err(format!(
            "响应不是 JSON（可能是站点改版或风控页，HTTP {status}）：{head}"
        ));
    }
    let data: Value =
        serde_json::from_str(&body_text).unwrap_or_else(|_| Value::String(body_text.clone()));

    if status.as_u16() >= 400 {
        let err_msg = match &data {
            Value::Object(map) => ["error", "message", "errorMessage"]
                .iter()
                .find_map(|k| map.get(*k).and_then(|v| v.as_str()))
                .unwrap_or_default()
                .to_string(),
            other => other
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect(),
        };
        let msg = if status.as_u16() == 401 {
            format!("未授权(access-token 无效): {err_msg}")
        } else if err_msg.is_empty() {
            format!("HTTP {}", status)
        } else {
            err_msg
        };
        return Err(msg);
    }
    Ok(data)
}

/// 是否处于「未登录」状态（酷族对无效 token 返回 200 + `data.isLogin=false`）
///
/// 取不到该字段时**保守认为已登录**（避免把站点改版误报成 token 失效）。
pub fn member_is_login(v: &Value) -> bool {
    v.get("data")
        .and_then(|d| d.get("isLogin"))
        .and_then(|x| x.as_bool())
        .unwrap_or(true)
}

/// 从 `/api/v2/member` 响应里取空间用量（`total`/`capacity` 取大者，`use`）
pub fn member_space(v: &Value) -> (u64, u64) {
    let data = v.get("data").cloned().unwrap_or_default();
    let num = |key: &str| data.get(key).and_then(|x| x.as_u64()).unwrap_or(0);
    (num("total").max(num("capacity")), num("use"))
}

/// 回收站条目数组（站点字段名随版本而异，逐个尝试兼容）
pub fn trash_items(v: &Value) -> Vec<Value> {
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

/// 条目字节数（字段名不确定，逐个尝试；字符串数字也接受）
pub fn trash_item_size(it: &Value) -> Option<u64> {
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

/// 条目的**删除时间**（毫秒）。解析不出时返回 `None`，调用方**保守保留**该条目。
///
/// ## 为什么只认「删除语义」的键
/// 保留策略的语义是「只清理**删除**时间早于 N 天的条目」，用来防止刚删的东西
/// 被立刻物理销毁（用户可能想反悔）。
///
/// 曾经这里还包含 `updateTime` / `time` / `createTime`：那是**创建/修改**时间，
/// 语义完全不同。一个 3 年前的旧文件今天被删，`createTime` 让它**立刻**满足
/// 「年龄 ≥ 7 天」而被物理删除 —— 恰好是保留策略要防的那一幕，且不可恢复。
///
/// 拿不到删除时间时返回 `None`，调用方走「无法判定 ⇒ 保留」分支（保守）。
pub fn trash_item_deleted_ms(it: &Value) -> Option<i64> {
    const KEYS: [&str; 5] = [
        "deletedDate",
        "deleteTime",
        "deletedAt",
        "deleteAt",
        "deleteDate",
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
            // 无时区的裸日期时间（如 "2026-09-20 12:34:56"）：酷族返回的是
            // 服务器本地时间，按本机时区解释，否则年龄门槛会偏差一个时区偏移量
            if let Some(ms) = parse_naive_local_ms(s) {
                return Some(ms);
            }
        }
    }
    None
}

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

/// 条目是否为文件夹（字段名兼容多种写法）
pub fn trash_is_folder(it: &Value) -> bool {
    for k in ["isFolder", "is_folder", "dir", "isDir"] {
        if let Some(b) = it.get(k).and_then(|x| x.as_bool()) {
            return b;
        }
    }
    for k in ["type", "fileType", "category"] {
        if let Some(s) = it.get(k).and_then(|x| x.as_str()) {
            let l = s.to_ascii_lowercase();
            if l.contains("folder") || l.contains("dir") {
                return true;
            }
        }
    }
    false
}

/// 条目的物理删除用 id（回收站一律用 `encodedId`）
pub fn trash_item_id(it: &Value) -> Option<String> {
    for k in ["encodedId", "encoded_id", "encodeId", "fileCode", "code"] {
        if let Some(s) = it.get(k).and_then(|x| x.as_str()) {
            if !s.trim().is_empty() {
                return Some(s.to_string());
            }
        }
    }
    // 数字型 encodedId 亦常见
    for k in ["encodedId", "encoded_id", "encodeId"] {
        if let Some(n) = it.get(k).and_then(|x| x.as_i64()) {
            return Some(n.to_string());
        }
    }
    None
}

/// 单页批量删除（文件/文件夹自动分组）
/// 删除回收站条目；返回**实际提交删除的条数**
///
/// 返回数量而不是 `()`：调用方（`empty_trash_gated`）用它累加「已清理」计数。
/// 旧实现返回 `()`，调用方按 `deletable.len()` 计数 —— 而本函数用 `filter_map`
/// **丢弃**提不出 id 的条目，于是计数虚高（前端显示删了 N 项，实际少删），
/// 且提不出 id 的条目下一轮又会被翻到，循环一路空转到轮次上限。
pub async fn delete_trash_items(client: &Client, token: &str, items: &[Value]) -> ApiResult<usize> {
    let file_pids: Vec<String> = items
        .iter()
        .filter(|it| !trash_is_folder(it))
        .filter_map(trash_item_id)
        .collect();
    let folder_ids: Vec<String> = items
        .iter()
        .filter(|it| trash_is_folder(it))
        .filter_map(trash_item_id)
        .collect();

    // 一个 id 都没提取到：必须报错，否则空转删除会让「已清理」计数虚高而实际什么都没删
    if file_pids.is_empty() && folder_ids.is_empty() {
        let first = items
            .first()
            .map(|v| truncate_json(v, 300))
            .unwrap_or_else(|| "(无条目)".to_string());
        return Err(format!(
            "回收站条目格式不识别（无法提取 id），已保守取消删除；首条目: {first}"
        ));
    }
    if !file_pids.is_empty() {
        client.delete_trash_files(token, &file_pids).await?;
    }
    if !folder_ids.is_empty() {
        client.delete_trash_folders(token, &folder_ids).await?;
    }
    Ok(file_pids.len() + folder_ids.len())
}

/// 截断 JSON 原文（日志/错误信息用，避免刷出超大响应）
pub fn truncate_json(v: &Value, max: usize) -> String {
    let s = v.to_string();
    if s.len() <= max {
        s
    } else {
        format!("{}…(截断)", s.chars().take(max).collect::<String>())
    }
}

/// 人类可读字节数
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{:.0} {}", v, UNITS[i])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

#[cfg(test)]
mod delete_time_tests {
    use super::*;
    use serde_json::json;

    /// **误删防护回归**：只有「删除时间」才能用于保留天数判定
    ///
    /// 曾经把 `createTime`/`updateTime`/`time` 也当成删除时间：一个 3 年前的旧文件
    /// 今天被删，`createTime` 会让它立刻满足「年龄 ≥ 7 天」→ 被物理删除，
    /// 恰好是保留策略要防的场景（不可恢复）。
    #[test]
    fn create_time_is_not_treated_as_delete_time() {
        let old_create = json!({
            "createTime": 1_600_000_000_000i64, // 2020 年的创建时间
            "name": "旧文件"
        });
        assert!(
            trash_item_deleted_ms(&old_create).is_none(),
            "只有创建时间时必须返回 None（让调用方保守保留），不能当成「很老」"
        );

        // 同理：updateTime / time 都不是删除时间
        assert!(trash_item_deleted_ms(&json!({"updateTime": 1_600_000_000_000i64})).is_none());
        assert!(trash_item_deleted_ms(&json!({"time": 1_600_000_000_000i64})).is_none());
    }

    /// 真正的删除时间字段必须能被识别（否则保留策略永远无法生效）
    #[test]
    fn delete_time_fields_are_recognized() {
        for k in ["deletedDate", "deleteTime", "deletedAt", "deleteAt", "deleteDate"] {
            let it = json!({ k: 1_700_000_000_000i64 });
            assert_eq!(
                trash_item_deleted_ms(&it),
                Some(1_700_000_000_000),
                "删除时间字段 {k} 必须被识别"
            );
        }
    }

    /// 秒级时间戳要能自动转毫秒
    #[test]
    fn second_precision_is_normalized_to_ms() {
        let it = json!({ "deleteTime": 1_700_000_000i64 }); // 秒
        assert_eq!(
            trash_item_deleted_ms(&it),
            Some(1_700_000_000_000),
            "秒级时间戳应换算成毫秒"
        );
    }
}
