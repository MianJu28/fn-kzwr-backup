//! kzwr 认证服务
//!
//! 整合：登录二进制调用、凭据加密存储、token 共享管理、过期自动重登。
//!
//! 登录二进制调用方式：
//!   kzwr_login_turnstile-linux-x64 <邮箱> <密码>
//! 产出 session.json：{"access_token": "...", "email": "...", "via": "..."}

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::info;

use crate::infra::config::ConfigManager;

/// 登录二进制文件名
pub const LOGIN_BIN: &str = "kzwr_login_turnstile-linux-x64";

/// 登录二进制产出的 session
#[derive(Debug, Deserialize)]
struct Session {
    access_token: String,
    email: Option<String>,
    via: Option<String>,
}

/// kzwr 认证服务
pub struct KzwrAuthService {
    /// 登录二进制目录
    bin_dir: PathBuf,
    /// 登录工作目录（产出 session.json）
    work_dir: PathBuf,
    /// 配置管理器（凭据加密存储）
    config: Arc<Mutex<ConfigManager>>,
    /// 共享 token 存储（与 KzwrClient 共享）
    token_store: Arc<Mutex<Option<String>>>,
    /// 当前登录用户名（内存缓存）
    username: Mutex<Option<String>>,
}

impl KzwrAuthService {
    /// 创建认证服务
    pub fn new(
        bin_dir: &Path,
        work_dir: &Path,
        config: Arc<Mutex<ConfigManager>>,
        token_store: Arc<Mutex<Option<String>>>,
    ) -> Self {
        std::fs::create_dir_all(work_dir).ok();
        Self {
            bin_dir: bin_dir.to_path_buf(),
            work_dir: work_dir.to_path_buf(),
            config,
            token_store,
            username: Mutex::new(None),
        }
    }

    /// 启动时从配置加载已保存的 token 到共享 token 存储（避免每次重启都重新登录）
    pub fn init_from_config(&self) {
        let cfg_guard = self.config.lock().unwrap();
        if let Ok(cfg) = cfg_guard.load() {
            if let Ok(Some(token)) = cfg_guard.decrypt_field(&cfg.kzwr.token_enc) {
                if !token.is_empty() {
                    *self.token_store.lock().unwrap() = Some(token);
                    info!("已从配置加载 kzwr token");
                }
            }
        }
    }

    /// 登录二进制是否存在
    pub fn bin_exists(&self) -> bool {
        self.bin_dir.join(LOGIN_BIN).exists()
    }

    /// 判断配置中是否已保存凭据（接收已加载的配置，避免重复加锁死锁）
    pub fn has_credentials(cfg: &crate::infra::config::AppConfig) -> bool {
        cfg.kzwr.username_enc.is_some() && cfg.kzwr.password_enc.is_some()
    }

    /// 是否已保存凭据（自行加载配置，供不持锁的调用方使用）
    pub fn has_credentials_self(&self) -> bool {
        let cfg_guard = self.config.lock().unwrap();
        match cfg_guard.load() {
            Ok(c) => Self::has_credentials(&c),
            Err(_) => false,
        }
    }

    /// 显式登录：保存凭据（加密）并获取 token
    pub fn login_with_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<String> {
        // 1) 调用登录二进制
        let token = self.run_login_bin(username, password)?;

        // 2) 加密保存凭据到配置
        {
            let cfg_guard = self.config.lock().unwrap();
            let mut cfg = cfg_guard.load()?;
            cfg.kzwr.username_enc = Some(cfg_guard.encrypt_field(username)?);
            cfg.kzwr.password_enc = Some(cfg_guard.encrypt_field(password)?);
            cfg.kzwr.token_enc = Some(cfg_guard.encrypt_field(&token)?);
            cfg_guard.save(&cfg)?;
        }

        // 3) 更新共享 token 和内存用户名
        *self.token_store.lock().unwrap() = Some(token.clone());
        *self.username.lock().unwrap() = Some(username.to_string());
        info!("已登录 kzwr: {}", username);
        Ok(token)
    }

    /// 确保有有效 token：无则用保存凭据自动重登
    pub fn ensure_login(&self) -> Result<String> {
        // 已有 token 直接返回
        if let Some(t) = self.token_store.lock().unwrap().clone() {
            return Ok(t);
        }
        // 无 token，用保存的凭据重登
        let (username, password) = self.load_credentials()?;
        info!("token 缺失/失效，尝试重新登录...");
        let token = self.run_login_bin(&username, &password)?;
        *self.token_store.lock().unwrap() = Some(token.clone());
        *self.username.lock().unwrap() = Some(username);
        Ok(token)
    }

    /// 读取已保存的凭据（解密）
    fn load_credentials(&self) -> Result<(String, String)> {
        let cfg_guard = self.config.lock().unwrap();
        let cfg = cfg_guard.load()?;
        let username = cfg_guard
            .decrypt_field(&cfg.kzwr.username_enc)?
            .ok_or_else(|| anyhow::anyhow!("未配置 kzwr 用户名，请先登录"))?;
        let password = cfg_guard
            .decrypt_field(&cfg.kzwr.password_enc)?
            .ok_or_else(|| anyhow::anyhow!("未配置 kzwr 密码，请先登录"))?;
        Ok((username, password))
    }

    /// 调用登录二进制获取 token
    fn run_login_bin(&self, username: &str, password: &str) -> Result<String> {
        let bin = self.bin_dir.join(LOGIN_BIN);
        if !bin.exists() {
            return Err(anyhow::anyhow!(
                "登录二进制不存在: {}",
                bin.display()
            ));
        }
        // 清掉旧 session
        let _ = std::fs::remove_file(self.work_dir.join("session.json"));

        info!("调用 kzwr 登录二进制...");
        let status = Command::new(&bin)
            .arg(username)
            .arg(password)
            .current_dir(&self.work_dir)
            .status()
            .context("启动登录二进制失败")?;

        if !status.success() {
            return Err(anyhow::anyhow!(
                "kzwr 登录失败（退出码 {:?}），可能凭据错误或验证码未通过",
                status.code()
            ));
        }

        let session_path = self.work_dir.join("session.json");
        let content = std::fs::read_to_string(&session_path).context("读取 session.json 失败")?;
        let session: Session = serde_json::from_str(&content).context("解析 session.json 失败")?;
        if session.access_token.is_empty() {
            return Err(anyhow::anyhow!("登录未返回 access_token"));
        }
        info!("kzwr 登录成功");
        Ok(session.access_token)
    }
}
