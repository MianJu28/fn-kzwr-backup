//! age 加密层（ADR-003：age 加密方案与分块策略）
//!
//! 采用 age（X25519 公私钥 + ChaCha20-Poly1305）对备份数据分块加密。
//! 备份用公钥加密，恢复用私钥解密；私钥可被口令派生密钥加密存储。
//!
//! 密文格式（支持选择性恢复的随机定位）：
//!   [u32 小端: chunk密文长度][age加密的 chunk 明文]
//!   (每个 chunk 是独立 age 加密流，拥有独立文件密钥)

use std::io::{Read, Write};
use std::str::FromStr;

use age::secrecy::ExposeSecret;
use age::x25519::{Identity, Recipient};
use anyhow::{Context, Result};

/// 分块大小：64MB（与架构 ADR-003 一致）
pub const CHUNK_SIZE: usize = 64 * 1024 * 1024;

/// age 密钥对（内存态，绝不落盘明文）
pub struct AgeKeys {
    pub identity: Identity,
}

impl std::fmt::Debug for AgeKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgeKeys").finish_non_exhaustive()
    }
}

impl AgeKeys {
    /// 生成新的 age 密钥对
    pub fn generate() -> Self {
        Self {
            identity: Identity::generate(),
        }
    }

    /// 从私钥字符串解析（age 标准格式 "AGE-SECRET-KEY-1..."）
    pub fn from_secret_key(s: &str) -> Result<Self> {
        let identity = Identity::from_str(s).map_err(|e| anyhow::anyhow!("解析 age 私钥失败: {}", e))?;
        Ok(Self { identity })
    }

    /// 导出私钥字符串（用于加密存储/备份）
    pub fn to_secret_key(&self) -> String {
        self.identity.to_string().expose_secret().to_string()
    }

    /// 获取对应的 age 公钥字符串（"age1..."）
    pub fn recipient_str(&self) -> String {
        self.identity.to_public().to_string()
    }
}

// 手动实现 Clone（Identity 可 clone）
impl Clone for AgeKeys {
    fn clone(&self) -> Self {
        Self {
            identity: self.identity.clone(),
        }
    }
}

/// 加密会话：持公钥用于备份加密，持私钥用于恢复解密
#[derive(Clone)]
pub struct CryptoSession {
    pub recipient: Recipient,
    pub identity: Option<Identity>,
}

impl CryptoSession {
    /// 创建仅加密会话（只需公钥，适合无人值守备份）
    pub fn for_encrypt(recipient: Recipient) -> Self {
        Self {
            recipient,
            identity: None,
        }
    }

    /// 创建完整会话（含私钥，可加解密）
    pub fn full(keys: &AgeKeys) -> Self {
        Self {
            recipient: keys.identity.to_public(),
            identity: Some(keys.identity.clone()),
        }
    }

    /// 从公钥字符串创建加密会话
    pub fn encrypt_with_pubkey(pubkey: &str) -> Result<Self> {
        let recipient = Recipient::from_str(pubkey)
            .map_err(|e| anyhow::anyhow!("解析 age 公钥失败: {}", e))?;
        Ok(Self::for_encrypt(recipient))
    }

    /// 流式分块加密：读入明文，写出 [len][age密文] 格式
    pub fn encrypt_stream(&self, mut reader: impl Read, writer: impl Write) -> Result<()> {
        let mut writer = writer;
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            let n = reader.read(&mut buf).context("读取明文失败")?;
            if n == 0 {
                break;
            }
            // 每个 chunk 独立 age 加密
            let encrypted = self.encrypt_chunk(&buf[..n])?;
            // 写 [u32 小端长度][密文]
            let len = (encrypted.len() as u32).to_le_bytes();
            writer.write_all(&len).context("写入块长度失败")?;
            writer.write_all(&encrypted).context("写入密文失败")?;
        }
        writer.flush().context("刷写密文失败")?;
        Ok(())
    }

    /// 加密单个 chunk（age 内存加密，输出完整 age 密文）
    pub fn encrypt_chunk(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let encryptor = age::Encryptor::with_recipients(vec![Box::new(self.recipient.clone())])
            .expect("至少提供一个接收者");
        let mut out = Vec::new();
        let mut writer = encryptor
            .wrap_output(&mut out)
            .context("创建 age 加密流失败")?;
        writer.write_all(plaintext).context("写入明文失败")?;
        writer.finish().context("完成 age 加密失败")?;
        Ok(out)
    }

    /// 解密单个 chunk（age 完整密文 → 明文）
    pub fn decrypt_chunk(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let identity = self
            .identity
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无私钥，无法解密"))?;

        let decryptor = age::Decryptor::new(ciphertext).context("创建 age 解密器失败")?;
        let decryptor = match decryptor {
            age::Decryptor::Recipients(d) => d,
            _ => return Err(anyhow::anyhow!("密文格式不支持(非接收者加密)")),
        };
        let mut reader = decryptor
            .decrypt(std::iter::once(identity as &dyn age::Identity))
            .context("age 解密失败")?;

        let mut plaintext = Vec::with_capacity(ciphertext.len());
        reader.read_to_end(&mut plaintext).context("读取明文失败")?;
        Ok(plaintext)
    }

    /// 流式分块解密：读入 [len][age密文] 格式，写出明文
    pub fn decrypt_stream(&self, mut reader: impl Read, mut writer: impl Write) -> Result<()> {
        let identity = self
            .identity
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无私钥，无法解密"))?;
        let mut len_buf = [0u8; 4];
        loop {
            // 读块长度
            let mut read_len = 0;
            while read_len < 4 {
                let n = reader.read(&mut len_buf[read_len..]).context("读取块长度失败")?;
                if n == 0 {
                    return Ok(()); // EOF
                }
                read_len += n;
            }
            let chunk_len = u32::from_le_bytes(len_buf) as usize;
            let mut ciphertext = vec![0u8; chunk_len];
            reader.read_exact(&mut ciphertext).context("读取密文失败")?;

            let decryptor = age::Decryptor::new(&ciphertext[..]).context("创建解密器失败")?;
            let decryptor = match decryptor {
                age::Decryptor::Recipients(d) => d,
                _ => return Err(anyhow::anyhow!("密文格式不支持(非接收者加密)")),
            };
            let mut r = decryptor
                .decrypt(std::iter::once(identity as &dyn age::Identity))
                .context("解密失败")?;
            let mut buf = Vec::new();
            r.read_to_end(&mut buf).context("读取明文失败")?;
            writer.write_all(&buf).context("写出明文失败")?;
        }
    }
}

impl std::str::FromStr for AgeKeys {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Self::from_secret_key(s)
    }
}
