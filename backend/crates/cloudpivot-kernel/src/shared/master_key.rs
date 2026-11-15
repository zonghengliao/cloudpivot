//! 存储主密钥：环境变量解析 + 十六进制校验（P2-59 · C1 解环）。
//!
//! # 为什么要从 `business::storage::cipher` 上提到基座
//!
//! 主密钥的**解析**被两处共用：启动守卫（[`crate::shared::startup_guard`] 在
//! `App::new()` 之前就要判定「主密钥缺失 / 长度不符 → 拒绝启动」）与存储域的
//! AES 加解密。启动守卫属基座，抽包后直接从 `business::storage::cipher` 取类型
//! 就是 `cloudpivot-kernel → storage` 的包级环。
//!
//! 所以：**解析上提、加解密留下**。
//!
//! | 项 | 位置 | 理由 |
//! |----|------|------|
//! | [`MasterKey`] / [`MasterKeyError`] / 三个常量 | 本文件（基座） | 启动期就要判定，且与加密算法无关 |
//! | AES-256-GCM 的 `encrypt` / `decrypt` / `mask_secret` | `business::storage::cipher` | 只有存储域用；`Crypto` 是框架能力，基座不该替业务域选算法 |
//!
//! `business::storage::cipher` 对本文件的符号做 `pub use` 转发，所以
//! `use crate::business::storage::cipher::MasterKey` 这类既有调用点一行都不用改。
//!
//! # 安全口径（三条不变）
//!
//! 1. [`MasterKeyError`] **绝不包含密钥内容**，只带「缺失 / 长度 / 非十六进制」定位信息；
//! 2. [`MasterKey`] 的 `Debug` 只打印字节数（`redacted`），`{:?}` 到日志也不泄露；
//! 3. 环境变量名 [`ENV_STORAGE_MASTER_KEY`] 单一出处 —— 部署脚本、启动守卫、存储域共用。

use std::fmt;

/// 主密钥环境变量名。
pub const ENV_STORAGE_MASTER_KEY: &str = "CLOUDPIVOT_STORAGE_MASTER_KEY";

/// 主密钥十六进制长度（64 位 = 32 字节 = AES-256）。
pub const MASTER_KEY_HEX_LEN: usize = 64;

/// 主密钥字节长度（AES-256）。
pub const MASTER_KEY_BYTES: usize = 32;

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
