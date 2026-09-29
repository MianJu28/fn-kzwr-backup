//! **宿主密钥的加解密原语**（供插件自管配置使用）
//!
//! ## 为什么需要它
//! 宿主**不再代存插件配置**（见 ADR-021）：插件把配置写进自己的 `own_data_dir`。
//! 但 kzwr 的 `access-token` 这类凭据此前是宿主以 age/scrypt 加密落盘的；
//! 若插件改为直接写文件，就会**从「加密存储」降级为「明文存储」**——
//! 只靠目录权限（0700）保护，一旦目录被读走凭据即泄漏。
//!
//! 因此宿主把**密钥留在自己手里**，只暴露「封 / 解」两个纯计算入口：
//! 插件拿不到密钥，只能请求宿主对一段明文加解密。
//!
//! ## 与 `ConfigManager::encrypt_field` 的关系
//! 两者共用同一口令（`TRIM_PASSPHRASE`）与同一套 age scrypt 实现，
//! 因此格式一致（都是 age passphrase 密文）—— 但**互不依赖**：
//! 这里不持有 `ConfigManager`（它需要配置目录），只持有口令，
//! 便于在没有配置上下文的场合（如单元测试）单独使用。
//!
//! ## 安全边界
//! - 密文格式与 `enc:` 字段相同（age scrypt），**同一口令**可互相解开；
//! - 解密失败一律返回 `None`（**绝不**回退成「当明文用」）——
//!   否则一个被篡改的密文会被当作明文配置，产生难以察觉的错误行为。

use std::io::{Read, Write};
use std::sync::OnceLock;

use age::secrecy::SecretString;

/// 进程级口令（由 `main.rs` 在启动时注入一次）
static PASSPHRASE: OnceLock<SecretString> = OnceLock::new();

/// 注入宿主口令（启动时调用一次；重复调用以首次为准）
///
/// 之所以用进程级 `OnceLock` 而不是随 `HostEffects` 传递：能力表是 `'static` 的，
/// 其 FFI 入口无法携带额外上下文。
pub fn init(passphrase: SecretString) {
    let _ = PASSPHRASE.set(passphrase);
}

/// 口令是否已注入（未注入时 `seal`/`unseal` 一律失败，而不是退化为明文）
pub fn ready() -> bool {
    PASSPHRASE.get().is_some()
}

/// 加密明文 → base64 密文（失败返回 `None`）
///
/// 输出是 age passphrase 密文的 base64 文本，可直接存进插件的配置文件。
pub fn seal(plain: &str) -> Option<String> {
    let pass = PASSPHRASE.get()?;
    let encryptor = age::Encryptor::with_user_passphrase(pass.clone());
    let mut out = Vec::new();
    let mut w = encryptor.wrap_output(&mut out).ok()?;
    w.write_all(plain.as_bytes()).ok()?;
    w.finish().ok()?;
    Some(base64_encode(&out))
}

/// 解密 [`seal`] 的产物（失败返回 `None`）
///
/// **失败绝不回退成明文**：调用方必须把 `None` 当作「无此配置」处理。
pub fn unseal(sealed: &str) -> Option<String> {
    let pass = PASSPHRASE.get()?;
    let data = base64_decode(sealed.trim())?;
    let decryptor = age::Decryptor::new(&data[..]).ok()?;
    let decryptor = match decryptor {
        age::Decryptor::Passphrase(d) => d,
        // 非口令格式（如收件人公钥加密）不属于本原语的产物
        _ => return None,
    };
    let mut reader = decryptor.decrypt(pass, None).ok()?;
    let mut out = Vec::new();
    reader.read_to_end(&mut out).ok()?;
    String::from_utf8(out).ok()
}

// ── base64（避免为一个编码再引依赖：项目里已有 base64 0.21）──────────────

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_once() {
        // 测试间共享进程口令；`OnceLock` 保证只生效一次
        init(SecretString::from("test-passphrase".to_string()));
    }

    #[test]
    fn seal_unseal_round_trip() {
        init_once();
        let plain = "s3cr3t-token-中文-🙂";
        let sealed = seal(plain).expect("加密应成功");
        assert_ne!(sealed, plain, "密文不得等于明文");
        assert!(!sealed.contains("s3cr3t"), "密文不应含明文片段");
        assert_eq!(unseal(&sealed).as_deref(), Some(plain), "应能解回原文");
    }

    #[test]
    fn unseal_rejects_garbage_without_falling_back_to_plaintext() {
        init_once();
        // 关键安全属性：坏输入必须返回 None，**绝不能**当作明文返回
        assert_eq!(unseal("not-base64!!!"), None);
        assert_eq!(unseal(""), None);
        // 合法 base64 但不是 age 密文
        assert_eq!(unseal("aGVsbG8gd29ybGQ="), None);
        // 明文 token 本身（插件若误把明文当密文读）也必须被拒
        assert_eq!(unseal("plain-token"), None);
    }

    #[test]
    fn seal_is_nondeterministic_but_decryptable() {
        init_once();
        let a = seal("same-input").unwrap();
        let b = seal("same-input").unwrap();
        // age 每次用随机盐/文件密钥 ⇒ 同一明文两次密文不同（避免密文比对泄漏信息）
        assert_ne!(a, b, "两次加密应不同（含随机盐）");
        assert_eq!(unseal(&a).as_deref(), Some("same-input"));
        assert_eq!(unseal(&b).as_deref(), Some("same-input"));
    }
}
