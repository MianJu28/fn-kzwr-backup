//! 密钥库（ADR-003：私钥加密存储）
//!
//! age 私钥用口令（经 scrypt 派生密钥）加密后落盘，永不明文存储。
//! 数据归属：密钥库 → $TRIM_PKGETC。

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use age::secrecy::{ExposeSecret, SecretString};
use anyhow::{Context, Result};

use crate::domain::crypto::AgeKeys;

/// 密钥库文件扩展名
pub const KEYSTORE_FILE: &str = "keystore.age";

/// 用口令加密保存 age 私钥到指定路径
pub fn save_keystore(keys: &AgeKeys, passphrase: &SecretString, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    // 用 age passphrase 加密私钥字符串
    let secret_key = keys.to_secret_key();
    let encryptor = age::Encryptor::with_user_passphrase(passphrase.clone());
    let mut out = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut out)
        .context("创建 age 加密流失败")?;
    writer.write_all(secret_key.as_bytes()).context("写入私钥失败")?;
    writer.finish().context("完成加密失败")?;
    write_private(path, &out)?;
    Ok(())
}

/// 以 **0600** 写入（仅属主可读写）
///
/// `std::fs::write` 走的是默认 mode 0666，再被 umask（通常 022）削成 **0644** ——
/// 结果是密钥库对本机**任何用户**可读。虽然内容已用口令加密，但「密文 + 口令」
/// 两份材料都在同 uid 下（见 `docs/PLUGIN_ISOLATION.md` §3），
/// 权限越紧越好：0644 → 0600 至少挡住其它用户。
///
/// 用 `OpenOptions::mode` 而**不是**「先写再 chmod」：后者有一个短暂窗口，
/// 文件已存在且权限宽松（本地磁盘上几乎不可利用，但没有理由留这个窗口）。
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .context("打开密钥库文件失败")?;
    f.write_all(data).context("写入密钥库文件失败")?;
    // 已存在的文件不会被 `mode` 改动（它只管新建），故显式收紧一次
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms).context("收紧密钥库权限失败")?;
    Ok(())
}

/// 从路径加载并解密 age 私钥
pub fn load_keystore(passphrase: &SecretString, path: &Path) -> Result<AgeKeys> {
    let data = std::fs::read(path).context("读取密钥库文件失败")?;
    let decryptor = age::Decryptor::new(&data[..]).context("创建解密器失败")?;
    let decryptor = match decryptor {
        age::Decryptor::Passphrase(d) => d,
        _ => return Err(anyhow::anyhow!("密钥库不是口令加密格式")),
    };
    let mut reader = decryptor
        .decrypt(&passphrase, None)
        .context("解密私钥失败(口令可能错误)")?;
    let mut plain = String::new();
    reader
        .read_to_string(&mut plain)
        .context("读取私钥失败")?;
    AgeKeys::from_secret_key(&plain)
}

/// 获取密钥库路径（基于数据目录）
pub fn keystore_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(KEYSTORE_FILE)
}

/// 简化辅助：SecretString 构造
pub fn secret(passphrase: &str) -> SecretString {
    SecretString::from(passphrase.to_string())
}

/// 安全清理内存中的密钥
#[allow(dead_code)]
fn zeroize_secret(s: &mut SecretString) {
    // SecretString 在 drop 时自动 zeroize，无需手动
    let _ = s.expose_secret();
}
