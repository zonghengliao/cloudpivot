//! 存储密钥（AK/SK）加解密：独立主密钥 + AES-256-GCM + 逐行 nonce + 密钥版本。
//!
//! # 方案（对应架构文档 §2.5）
//!
//! | 决策 | 做法 | 原因 |
//! |------|------|------|
//! | 主密钥来源 | 环境变量 [`ENV_STORAGE_MASTER_KEY`]（64 位十六进制 = 32 字节） | 与 JWT 密钥**分开**注入、分开备份：轮换 JWT 不会让全部存储配置失效 |
//! | 加密算法 | AES-256-GCM（框架 [`Crypto::aes_encrypt`]） | 自带完整性认证，密文改一位即解密失败 |
//! | nonce | 每条密文独立随机 12 字节（随密文同列存 Base64） | GCM 要求 nonce 不重用 |
//! | 版本号 | `sys_storage_config.key_version` | 支持主密钥轮换：旧行保留旧版本仍可解 |
//! | 对外暴露 | 只回 `ak_masked` + `has_secret` | 「只写不读」：前端拿不到任何明文 |
//!
//! # 已知局限（坦诚记录，不假装做了）
//!
//! 框架的 `Crypto::aes_encrypt` 不支持 AAD，因此密文**没有**与 `config_code` 绑定：
//! 把 A 行的 SK 密文搬到 B 行在功能上等价于「管理员改了配置」，不构成额外攻击面。
//! 需要 AAD 时改本文件一个函数即可（KMS/HSM 方案也留在这里替换）。
//!
//! # 主密钥丢失的后果
//!
//! 主密钥丢失 = 所有 S3 后端的 AK/SK 解不开 = 需要人工重填。启动守卫
//! （[`crate::shared::startup_guard`]）会在主密钥缺失 / 长度不符时**拒绝启动**，
//! 避免「先启动、用到才发现」。

use std::fmt;

use nik::prelude::Crypto;

/// 主密钥环境变量名。
pub const ENV_STORAGE_MASTER_KEY: &str = "CLOUDPIVOT_STORAGE_MASTER_KEY";

/// 主密钥十六进制长度（64 位 = 32 字节 = AES-256）。
pub const MASTER_KEY_HEX_LEN: usize = 64;

/// 主密钥字节长度（AES-256）。
pub const MASTER_KEY_BYTES: usize = 32;

/// 掩码前缀保留字符数。
pub const MASK_KEEP_PREFIX: usize = 4;

/// 掩码后缀保留字符数。
pub const MASK_KEEP_SUFFIX: usize = 4;

/// 掩码中段的固定占位符。
pub const MASK_FILLER: &str = "****";

/// 主密钥校验错误。
///
/// 错误只携带「缺失 / 长度 / 非十六进制」这类定位信息，**绝不包含密钥内容**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MasterKeyError {
    /// 环境变量未注入或为空。
    Missing,
    /// 长度不是 [`MASTER_KEY_HEX_LEN`]。
    BadLength {
        /// 实际字符数。
        len: usize,
    },
    /// 含非十六进制字符。
    BadHex,
}

impl fmt::Display for MasterKeyError {
    /// 输出可直接贴进日志的中文说明（含修复步骤，不含密钥）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(
                f,
                "存储主密钥缺失：环境变量 {ENV_STORAGE_MASTER_KEY} 未注入。\n\
                 危害：存储后端配置里的 AK/SK 无法加解密，S3 后端全部不可用。\n\
                 修复：在各环境的 /etc/cloudpivot/*.env 中写入 \
                 {ENV_STORAGE_MASTER_KEY}=\"$(openssl rand -hex 32)\"（每环境不同值，权限 600）。"
            ),
            Self::BadLength { len } => write!(
                f,
                "存储主密钥长度不合法：当前 {len} 个字符，要求恰好 {MASTER_KEY_HEX_LEN} 个\
                 （32 字节的十六进制）。\n\
                 修复：用 openssl rand -hex 32 重新生成后写入 {ENV_STORAGE_MASTER_KEY}。"
            ),
            Self::BadHex => write!(
                f,
                "存储主密钥不是十六进制：{ENV_STORAGE_MASTER_KEY} 只允许 0-9 / a-f。\n\
                 修复：用 openssl rand -hex 32 重新生成（不要手写、不要带引号或前缀）。"
            ),
        }
    }
}

impl std::error::Error for MasterKeyError {}

/// 存储主密钥（32 字节 AES-256 密钥）。
///
/// `Debug` 刻意**不打印密钥内容**：万一被 `{:?}` 到日志也不会泄露。
#[derive(Clone, PartialEq, Eq)]
pub struct MasterKey([u8; MASTER_KEY_BYTES]);

impl MasterKey {
    /// 从十六进制字符串解析主密钥。
    ///
    /// # 参数
    ///
    /// - `raw`：64 位十六进制字符串（允许首尾空白，内部空白不允许）。
    ///
    /// # 返回值
    ///
    /// 合法时返回主密钥。
    ///
    /// # 异常情况
    ///
    /// 长度不符返回 [`MasterKeyError::BadLength`]；含非十六进制字符返回
    /// [`MasterKeyError::BadHex`]。
    pub fn parse(raw: &str) -> std::result::Result<Self, MasterKeyError> {
        let trimmed = raw.trim();
        if trimmed.len() != MASTER_KEY_HEX_LEN {
            return Err(MasterKeyError::BadLength {
                len: trimmed.chars().count(),
            });
        }

        let mut bytes = [0u8; MASTER_KEY_BYTES];
        let raw_bytes = trimmed.as_bytes();
        for (index, slot) in bytes.iter_mut().enumerate() {
            let pair = &raw_bytes[index * 2..index * 2 + 2];
            let text = std::str::from_utf8(pair).map_err(|_| MasterKeyError::BadHex)?;
            *slot = u8::from_str_radix(text, 16).map_err(|_| MasterKeyError::BadHex)?;
        }
        Ok(Self(bytes))
    }

    /// 从环境变量读取主密钥。
    ///
    /// # 返回值
    ///
    /// 环境变量存在且合法时返回主密钥；缺失返回 [`MasterKeyError::Missing`]。
    ///
    /// # 异常情况
    ///
    /// 长度 / 字符集不合法时返回对应 [`MasterKeyError`]。
    pub fn from_env() -> std::result::Result<Self, MasterKeyError> {
        match std::env::var(ENV_STORAGE_MASTER_KEY) {
            Ok(value) if !value.trim().is_empty() => Self::parse(&value),
            _ => Err(MasterKeyError::Missing),
        }
    }

    /// 直接以 32 字节构造（供单测与将来的 KMS 解密结果使用）。
    ///
    /// # 参数
    ///
    /// - `bytes`：32 字节密钥。
    ///
    /// # 返回值
    ///
    /// 主密钥实例。
    pub fn from_bytes(bytes: [u8; MASTER_KEY_BYTES]) -> Self {
        Self(bytes)
    }

    /// 取字节视图（仅供加解密内部使用）。
    ///
    /// # 返回值
    ///
    /// 32 字节切片。
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for MasterKey {
    /// 只打印长度，不打印密钥内容。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MasterKey(<{} bytes redacted>)", self.0.len())
    }
}

/// 加密一个凭据字段。
///
/// # 参数
///
/// - `key`：主密钥。
/// - `plaintext`：明文（AK 或 SK）。
///
/// # 返回值
///
/// `(Base64 密文, Base64 nonce)`：两列必须同写同读（见 `sys_storage_config` 的四列）。
///
/// # 异常情况
///
/// 框架加密失败（理论上仅当密钥长度错误）时返回 `None`，调用方须按「加密失败」
/// 报错，**绝不允许回退明文落库**。
pub fn encrypt(key: &MasterKey, plaintext: &str) -> Option<(String, String)> {
    Crypto::aes_encrypt(key.as_bytes(), plaintext)
}

/// 解密一个凭据字段。
///
/// # 参数
///
/// - `key`：主密钥。
/// - `ciphertext_b64`：Base64 密文。
/// - `nonce_b64`：Base64 nonce。
///
/// # 返回值
///
/// 解密成功返回明文；密钥不匹配、密文被篡改、Base64 损坏一律返回 `None`
/// （GCM 的完整性校验在此体现：改一位就解不开）。
pub fn decrypt(key: &MasterKey, ciphertext_b64: &str, nonce_b64: &str) -> Option<String> {
    Crypto::aes_decrypt(key.as_bytes(), ciphertext_b64, nonce_b64)
}

/// 生成对外展示用的掩码。
///
/// # 参数
///
/// - `value`：明文凭据（AK）。
///
/// # 返回值
///
/// - 长度 > 8：保留前 4 与后 4，中段替换为 `****`（如 `CPAK****9f2c`）；
/// - 长度 ≤ 8：整体替换为 `****`（短串保留任何字符都可能等于泄露）。
pub fn mask_secret(value: &str) -> String {
    let chars: Vec<char> = value.trim().chars().collect();
    if chars.len() <= MASK_KEEP_PREFIX + MASK_KEEP_SUFFIX {
        return MASK_FILLER.to_string();
    }
    let prefix: String = chars[..MASK_KEEP_PREFIX].iter().collect();
    let suffix: String = chars[chars.len() - MASK_KEEP_SUFFIX..].iter().collect();
    format!("{prefix}{MASK_FILLER}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单测用主密钥（**非真实密钥**）。
    fn test_key() -> MasterKey {
        MasterKey::parse("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .expect("测试主密钥应合法")
    }

    /// 正常路径：加密 → 解密往返一致，且密文与 nonce 都不是明文。
    #[test]
    fn crypto_roundtrip() {
        let key = test_key();
        let (cipher, nonce) = encrypt(&key, "CPAKFixtureTestValue").expect("加密应成功");
        assert_ne!(cipher, "CPAKFixtureTestValue");
        assert!(!cipher.contains("CPAKFixture"));
        assert_eq!(
            decrypt(&key, &cipher, &nonce).as_deref(),
            Some("CPAKFixtureTestValue")
        );
    }

    /// 边界：同一明文两次加密必须得到不同密文（nonce 随机）。
    #[test]
    fn crypto_nonce_is_unique_per_call() {
        let key = test_key();
        let (c1, n1) = encrypt(&key, "same-plaintext").expect("加密应成功");
        let (c2, n2) = encrypt(&key, "same-plaintext").expect("加密应成功");
        assert_ne!(n1, n2, "nonce 必须每条独立");
        assert_ne!(c1, c2, "nonce 不同则密文必然不同");
        assert_eq!(decrypt(&key, &c1, &n1).as_deref(), Some("same-plaintext"));
        assert_eq!(decrypt(&key, &c2, &n2).as_deref(), Some("same-plaintext"));
    }

    /// 错误分支：密文被篡改一位必须解密失败（GCM 完整性）。
    #[test]
    fn crypto_tamper_rejected() {
        let key = test_key();
        let (cipher, nonce) = encrypt(&key, "secret-key-value").expect("加密应成功");

        // 篡改密文：把首字符换掉（Base64 字符集内替换，保证仍是合法 Base64）
        let mut tampered: Vec<char> = cipher.chars().collect();
        tampered[0] = if tampered[0] == 'A' { 'B' } else { 'A' };
        let tampered: String = tampered.into_iter().collect();
        assert_eq!(
            decrypt(&key, &tampered, &nonce),
            None,
            "篡改密文必须解密失败"
        );

        // 篡改 nonce：GCM 认证同样失败
        let mut nonce_chars: Vec<char> = nonce.chars().collect();
        nonce_chars[0] = if nonce_chars[0] == 'A' { 'B' } else { 'A' };
        let bad_nonce: String = nonce_chars.into_iter().collect();
        assert_eq!(
            decrypt(&key, &cipher, &bad_nonce),
            None,
            "篡改 nonce 必须失败"
        );
    }

    /// 错误分支：换一把主密钥必须解不开（密钥轮换的语义基础）。
    #[test]
    fn crypto_wrong_key_rejected() {
        let key = test_key();
        let other =
            MasterKey::parse("fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210")
                .expect("测试主密钥应合法");
        let (cipher, nonce) = encrypt(&key, "another-secret").expect("加密应成功");
        assert_eq!(decrypt(&other, &cipher, &nonce), None);
    }

    /// 错误分支：主密钥长度 / 字符集校验。
    #[test]
    fn crypto_key_length_validation() {
        // 63 位：长度不符
        let short = "0".repeat(MASTER_KEY_HEX_LEN - 1);
        assert_eq!(
            MasterKey::parse(&short),
            Err(MasterKeyError::BadLength {
                len: MASTER_KEY_HEX_LEN - 1
            })
        );
        // 64 位但非十六进制
        let bad = "z".repeat(MASTER_KEY_HEX_LEN);
        assert_eq!(MasterKey::parse(&bad), Err(MasterKeyError::BadHex));
        // 合法 64 位（含首尾空白也应通过）
        let ok = format!("  {}  ", "a1".repeat(32));
        assert!(MasterKey::parse(&ok).is_ok());
    }

    /// 安全：主密钥的 Debug 输出不得包含密钥内容。
    #[test]
    fn crypto_master_key_debug_is_redacted() {
        let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let key = MasterKey::parse(hex).expect("测试主密钥应合法");
        let debug = format!("{key:?}");
        assert!(!debug.contains(hex), "Debug 输出泄露了主密钥: {debug}");
        assert!(debug.contains("redacted"), "{debug}");
    }

    /// 安全：AK 掩码保留首尾、隐藏中段；短串整体掩码。
    #[test]
    fn crypto_mask_ak_never_leaks_full_secret() {
        let ak = "CPAKFixtureMasked9f2c";
        let masked = mask_secret(ak);
        assert_eq!(masked, "CPAK****9f2c");
        assert!(!masked.contains("5tAbCdEfGhIjKl"));

        // 短串（≤8）整体打码，不保留任何字符
        assert_eq!(mask_secret("abcd1234"), "****");
        assert_eq!(mask_secret(""), "****");
        // 恰好 9 位：仍按首尾各 4 保留
        assert_eq!(mask_secret("123456789"), "1234****6789");
    }
}
