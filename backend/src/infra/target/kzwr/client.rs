//! 酷族网软文件管理 API 客户端（认证/列表/文件操作/分享/回收站）
//!
//! Rust 重写自 `kzwr/kzwr_api.py` 的 `KzwrClient`。reqwest 替代 requests，
//! 认证头 `access-token`。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::constants::BASE_URL;
use super::{KzwrError, KzwrResult};

/// 原始 PUT 响应（分片上传用）
#[derive(Debug)]
pub struct RawPutResponse {
    pub etag: Option<String>,
}

impl RawPutResponse {
    pub fn get_etag(&self) -> Option<String> {
        self.etag.clone()
    }
}

/// kzwr.com 文件管理 API 客户端
#[derive(Debug, Clone)]
pub struct KzwrClient {
    base_url: String,
    timeout: std::time::Duration,
    http: reqwest::Client,
    /// access-token（登录后设置，Arc 共享支持运行时更新）
    pub access_token: Arc<Mutex<Option<String>>>,
}

impl Default for KzwrClient {
    fn default() -> Self {
        Self::new(BASE_URL, 30)
    }
}

impl KzwrClient {
    /// 创建客户端
    pub fn new(base_url: &str, timeout_secs: u64) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            // 默认请求头与前端 http.Options() 一致
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
            .expect("构建 HTTP 客户端失败");

        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            timeout: std::time::Duration::from_secs(timeout_secs),
            http,
            access_token: Arc::new(Mutex::new(None)),
        }
    }

    /// 从 session 设置 access-token（Arc 共享，运行时可更新）
    pub fn set_token(&self, token: impl Into<String>) {
        *self.access_token.lock().unwrap() = Some(token.into());
    }

    /// 获取当前 access-token
    pub fn get_token(&self) -> Option<String> {
        self.access_token.lock().unwrap().clone()
    }

    /// 获取共享的 token 存储（供认证管理器更新）
    pub fn token_store(&self) -> Arc<Mutex<Option<String>>> {
        self.access_token.clone()
    }

    /// 构造完整 URL
    fn url(&self, path: &str) -> String {
        if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{}{}", self.base_url, path)
        }
    }

    /// 带认证头的请求头构造
    fn auth_headers(&self) -> HashMap<String, String> {
        let mut h = HashMap::new();
        if let Some(token) = self.access_token.lock().unwrap().clone() {
            h.insert("access-token".to_string(), token);
        }
        h
    }

    /// 统一 GET 请求（JSON）
    async fn get_json(&self, path: &str, params: &[(&str, String)]) -> KzwrResult<Value> {
        let mut req = self.http.get(self.url(path));
        for (k, v) in params {
            req = req.query(&[(k, v.as_str())]);
        }
        for (k, v) in self.auth_headers() {
            req = req.header(&k, v);
        }
        let resp = req.send().await?;
        Self::parse_response(resp).await
    }

    /// 统一 POST 请求（JSON body）
    pub(crate) async fn post_json(&self, path: &str, body: Value) -> KzwrResult<Value> {
        let mut req = self.http.post(self.url(path)).json(&body);
        for (k, v) in self.auth_headers() {
            req = req.header(&k, v);
        }
        let resp = req.send().await?;
        Self::parse_response(resp).await
    }

    /// 原始 GET 下载内容（带认证头），返回字节
    pub(crate) async fn raw_get_bytes(&self, url: &str) -> KzwrResult<bytes::Bytes> {
        let mut req = self.http.get(self.url(url));
        for (k, v) in self.auth_headers() {
            req = req.header(&k, v);
        }
        let resp = req.send().await?;
        if resp.status().as_u16() >= 400 {
            return Err(KzwrError::Api(format!(
                "下载失败: HTTP {}",
                resp.status()
            )));
        }
        Ok(resp.bytes().await?)
    }

    /// 二进制 PUT 到预签名 URL，返回带 ETag 的响应
    pub(crate) async fn raw_put(&self, url: &str, data: &[u8]) -> KzwrResult<RawPutResponse> {
        let resp = self
            .http
            .put(url)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(data.to_vec())
            .send()
            .await?;
        let status = resp.status().as_u16();
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches('"').to_string());
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(KzwrError::Api(format!(
                "PUT 失败: HTTP {} {}",
                status,
                body.chars().take(200).collect::<String>()
            )));
        }
        Ok(RawPutResponse { etag })
    }

    /// 统一响应解析与错误处理（对照 _request）
    async fn parse_response(resp: reqwest::Response) -> KzwrResult<Value> {
        let status = resp.status();
        let body_text = resp.text().await?;
        let data: Value = serde_json::from_str(&body_text).unwrap_or_else(|_| {
            // 非 JSON 时按字符串处理
            Value::String(body_text.clone())
        });

        if status.as_u16() >= 400 {
            let err_msg = if let Value::Object(map) = &data {
                ["error", "message", "errorMessage"]
                    .iter()
                    .find_map(|k| map.get(*k).and_then(|v| v.as_str()))
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            } else {
                data.as_str().unwrap_or_default().chars().take(200).collect()
            };

            let msg = if status.as_u16() == 401 {
                format!("未授权(access-token 无效): {}", err_msg)
            } else if err_msg.is_empty() {
                format!("HTTP {}", status)
            } else {
                err_msg
            };
            return Err(KzwrError::Api(msg));
        }
        Ok(data)
    }

    // ── 认证 ──────────────────────────────────────────────

    /// 登录，返回 access-token。pre_verification_token 为空可能被拒。
    pub async fn login(
        &mut self,
        identifier: &str,
        password: &str,
        pre_verification_token: Option<&str>,
    ) -> KzwrResult<String> {
        let body = json!({
            "Identifier": identifier,
            "Password": password,
            "PreVerificationToken": pre_verification_token.unwrap_or(""),
        });
        let data = self.post_json("/api/v2/login", body).await?;
        let token = data
            .get("data")
            .and_then(|d| d.get("token"))
            .and_then(|t| t.as_str())
            .ok_or_else(|| KzwrError::Api(format!("登录失败: 响应中无 token -> {}", data)))?;
        self.set_token(token.to_string());
        Ok(token.to_string())
    }

    /// 通过 Turnstile token 登录
    pub async fn login_with_turnstile(
        &mut self,
        identifier: &str,
        password: &str,
        turnstile_token: &str,
    ) -> KzwrResult<String> {
        let pvt = self.exchange_turnstile_token(turnstile_token).await?;
        self.login(identifier, password, Some(&pvt)).await
    }

    /// 把 Turnstile 原始 token 换为 preVerificationToken
    pub async fn exchange_turnstile_token(&self, turnstile_token: &str) -> KzwrResult<String> {
        let data = self
            .post_json("/api/v1/turnstile", json!({ "Token": turnstile_token }))
            .await?;
        data.get("preVerificationToken")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| KzwrError::Api(format!("/api/v1/turnstile 未返回 preVerificationToken -> {}", data)))
    }

    /// 获取当前登录用户信息
    pub async fn get_member(&self) -> KzwrResult<Value> {
        self.get_json("/api/v2/member", &[]).await
    }

    /// 切换语言
    pub async fn set_language(&self, language: &str) -> KzwrResult<Value> {
        self.post_json("/api/v2/user/language", json!({ "language": language }))
            .await
    }

    /// 退出登录
    pub async fn logout(&self) -> KzwrResult<Value> {
        self.post_json("/api/logout", Value::Null).await
    }

    // ── 文件 / 文件夹列表 ────────────────────────────────

    /// 获取指定路径下的文件列表（含文件夹）
    pub async fn list_files(
        &self,
        path: &str,
        page: u32,
        include_folders: bool,
    ) -> KzwrResult<Value> {
        let params = [
            ("path", path.to_string()),
            ("page", page.to_string()),
            ("includeFolders", include_folders.to_string()),
        ];
        self.get_json("/api/v3/user/files", &params).await
    }

    /// 获取文件夹列表。folder_id 为 None 时返回根级
    pub async fn list_folders(&self, folder_id: Option<&str>) -> KzwrResult<Value> {
        match folder_id {
            Some(id) => self
                .get_json(&format!("/api/v1/user/folders/{}", id), &[])
                .await,
            None => self.get_json("/api/v1/user/folders", &[]).await,
        }
    }

    // ── 文件夹操作 ───────────────────────────────────────

    /// 创建文件夹，返回新文件夹路径
    pub async fn create_folder(&self, name: &str, path: &str) -> KzwrResult<String> {
        let data = self
            .post_json(
                "/api/v1/user/folder/create",
                json!({ "name": name, "path": path }),
            )
            .await?;
        Ok(data
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| data.get("path").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| path.to_string()))
    }

    /// 重命名文件夹
    pub async fn rename_folder(
        &self,
        new_name: &str,
        folder_id: &str,
        old_name: &str,
        origin_path: &str,
    ) -> KzwrResult<Value> {
        self.post_json(
            "/api/v1/user/folder/rename",
            json!({
                "NewName": new_name,
                "FolderId": folder_id,
                "OldName": old_name,
                "OriginPath": origin_path,
            }),
        )
        .await
    }

    /// 移动文件夹
    pub async fn move_folder(&self, target_folder_id: &str, source_folder_id: &str) -> KzwrResult<Value> {
        self.post_json(
            "/api/v1/user/folders/move",
            json!({
                "TargetFolderId": target_folder_id,
                "SourceFolderId": source_folder_id,
            }),
        )
        .await
    }

    /// 删除文件夹（physical=true 物理，false 逻辑）
    pub async fn delete_folder(&self, folder_encoded_ids: &[String], physical: bool) -> KzwrResult<Value> {
        let endpoint = if physical {
            "/api/v2/folder/delete/physical"
        } else {
            "/api/v2/folder/delete/logical"
        };
        self.post_json(endpoint, json!({ "FolderIds": folder_encoded_ids }))
            .await
    }

    /// 从回收站恢复文件夹
    pub async fn restore_folder(&self, folder_encoded_ids: &[String]) -> KzwrResult<Value> {
        self.post_json("/api/v2/folder/restore", json!({ "FolderIds": folder_encoded_ids }))
            .await
    }

    // ── 文件操作 ─────────────────────────────────────────

    /// 获取文件详情。响应 data.internalDownloadLink 即下载链接
    pub async fn get_file_detail(
        &self,
        code: &str,
        verify_password_token: Option<&str>,
    ) -> KzwrResult<Value> {
        let mut path = format!("/api/v3/file/{}", code);
        if let Some(tok) = verify_password_token {
            path.push_str(&format!("?verifyPasswordToken={}", tok));
        }
        self.get_json(&path, &[]).await
    }

    /// 获取文件下载链接
    pub async fn get_download_link(
        &self,
        code: &str,
        verify_password_token: Option<&str>,
    ) -> KzwrResult<String> {
        let detail = self.get_file_detail(code, verify_password_token).await?;
        let link = detail
            .get("data")
            .and_then(|d| d.get("internalDownloadLink"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                KzwrError::Api(format!("文件 {} 详情中无 internalDownloadLink -> {}", code, detail))
            })?;
        Ok(link.to_string())
    }

    /// 重命名文件
    pub async fn rename_file(&self, sid: &str, new_name: &str) -> KzwrResult<Value> {
        self.post_json(
            "/api/v1/user/files/rename",
            json!({ "FileName": new_name, "Sid": sid }),
        )
        .await
    }

    /// 移动文件到指定文件夹
    pub async fn move_file(&self, sid: &str, folder_id: &str) -> KzwrResult<Value> {
        self.post_json(
            "/api/v1/user/files/move",
            json!({ "FolderId": folder_id, "Sid": sid }),
        )
        .await
    }

    /// 复制文件（他人分享的文件）
    pub async fn copy_file(&self, file_code: &str, token: &str) -> KzwrResult<Value> {
        self.post_json(
            "/api/v2/file/copy",
            json!({ "fileCode": file_code, "token": token }),
        )
        .await
    }

    /// 删除文件（physical=true 物理，false 逻辑）
    pub async fn delete_file(&self, sids: &[String], physical: bool) -> KzwrResult<Value> {
        let endpoint = if physical {
            "/api/v2/files/delete/physical"
        } else {
            "/api/v2/files/delete/logical"
        };
        self.post_json(endpoint, json!({ "sids": sids })).await
    }

    /// 从回收站恢复文件
    pub async fn restore_file(&self, pids: &[String]) -> KzwrResult<Value> {
        self.post_json("/api/v2/files/restore", json!({ "Pids": pids })).await
    }

    // ── 分享 ─────────────────────────────────────────────

    /// 获取文件分享信息
    pub async fn get_share_info(&self, file_code: &str) -> KzwrResult<Value> {
        self.get_json(&format!("/api/v2/files/{}/share", file_code), &[])
            .await
    }

    // ── 回收站 ───────────────────────────────────────────

    /// 获取回收站列表
    pub async fn get_trash(&self, page: u32) -> KzwrResult<Value> {
        let params = [("page", page.to_string())];
        self.get_json("/api/v2/user/trash", &params).await
    }

    /// 从回收站永久删除文件(物理删除)。
    /// pids 用回收站条目的 encodedId(不是普通列表的 sid)。
    /// 端点: POST /api/v2/files/delete/physical, body: {Pids: [...]}
    pub async fn delete_trash_files(&self, pids: &[String]) -> KzwrResult<Value> {
        self.post_json("/api/v2/files/delete/physical", json!({ "Pids": pids }))
            .await
    }

    /// 从回收站永久删除文件夹(物理删除)。
    /// folder_encoded_ids 用回收站条目的 encodedId。
    /// 端点: POST /api/v2/folder/delete/physical, body: {FolderIds: [...]}
    pub async fn delete_trash_folders(&self, folder_encoded_ids: &[String]) -> KzwrResult<Value> {
        self.post_json(
            "/api/v2/folder/delete/physical",
            json!({ "FolderIds": folder_encoded_ids }),
        )
        .await
    }

    /// 从回收站批量永久删除条目(文件+文件夹自动区分)。
    /// items 为 get_trash() 返回的 items 数组，按 itemType 分发。
    pub async fn delete_trash_items(&self, items: &[Value]) -> KzwrResult<Vec<Value>> {
        let file_pids: Vec<String> = items
            .iter()
            .filter(|it| it.get("itemType").and_then(|v| v.as_str()) != Some("folder"))
            .filter_map(|it| it.get("encodedId").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect();
        let folder_ids: Vec<String> = items
            .iter()
            .filter(|it| it.get("itemType").and_then(|v| v.as_str()) == Some("folder"))
            .filter_map(|it| it.get("encodedId").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect();

        let mut results = Vec::new();
        if !file_pids.is_empty() {
            results.push(self.delete_trash_files(&file_pids).await?);
        }
        if !folder_ids.is_empty() {
            results.push(self.delete_trash_folders(&folder_ids).await?);
        }
        Ok(results)
    }
}
