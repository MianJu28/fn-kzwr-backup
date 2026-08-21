//! 更新配置中的 kzwr token（加密存储）
//!
//! 从 session.json 读取新 token，用 ConfigManager 加密写入配置的 token_enc。
//! 用法：update_config_token <cfg_dir> <passphrase> <session.json>

use fnos_backup::infra::config::ConfigManager;
use fnos_backup::infra::keystore;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("用法: update_config_token <cfg_dir> <passphrase> <session.json>");
        std::process::exit(1);
    }
    let cfg_dir = std::path::PathBuf::from(&args[1]);
    let passphrase = keystore::secret(&args[2]);
    let session_path = std::path::PathBuf::from(&args[3]);

    // 读 session.json 的 token
    let content = std::fs::read_to_string(&session_path)?;
    let session: serde_json::Value = serde_json::from_str(&content)?;
    let token = session
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("session.json 无 access_token"))?;
    let username = session.get("email").and_then(|v| v.as_str()).unwrap_or("");

    let cm = ConfigManager::new(&cfg_dir, passphrase);
    let mut cfg = cm.load().unwrap_or_default();

    // 更新 token（加密）
    cfg.kzwr.token_enc = Some(cm.encrypt_field(token)?);
    if !username.is_empty() {
        cfg.kzwr.username_enc = Some(cm.encrypt_field(username)?);
    }
    cm.save(&cfg)?;
    println!("[+] 已更新配置 token（加密存储），长度 {}", token.len());
    Ok(())
}
