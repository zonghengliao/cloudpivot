//! 对象存储业务模块（批次 A · A1 底座）。
//!
//! # 模块结构
//!
//! | 文件 | 职责 |
//! |------|------|
//! | [`store`] | `ObjectStore` 契约 + 本地磁盘后端 + S3 后端（预签名） |
//! | [`sigv4`] | 自研 AWS SigV4 预签名（与官方 SDK 逐字节一致） |
//! | [`cipher`] | AK/SK 的 AES-256-GCM 加解密与主密钥校验 |
//! | [`router`] | `StorageRouter`：按 `config_code` 持有实例，按策略 / 默认选路 |
//! | [`model`] | `sys_storage_config` / `sys_storage_policy` 读写与策略校验 |
//! | [`dto`] | 配置 CRUD 与策略 DTO（**AK/SK 只写不读**） |
//! | [`handler`] | 配置管理接口（分页 / 增删改 / 设默认 / 测试连接 / 策略） |
//! | [`cleanup`] | 软删文件的对象回收任务（每日 04:30） |
//!
//! # 进程级单例
//!
//! 路由表用 [`OnceLock`] 持有（[`global_router`]），**懒装载**：首个真正需要后端的
//! 请求触发 [`ensure_router`]，配置写操作后由 [`reload_router`] 整表重建。
//! 之所以不放在 `bootstrap.rs`：启动钩子没有错误通道，且装载失败不应阻断启动
//! （架构文档 §2.3 的降级要求）；懒装载 + 失败重试同样满足「不阻断启动」，
//! 且不额外改动底座。

pub mod cipher;
pub mod cleanup;
pub mod dto;
pub mod handler;
pub mod model;
pub mod router;
pub mod sigv4;
pub mod store;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use nik::prelude::ApiError;

use crate::business::storage::cipher::MasterKey;
use crate::business::storage::router::StorageRouter;
use crate::shared::error_code;
use crate::utils;

/// 路由表是否已成功装载。
static ROUTER_LOADED: AtomicBool = AtomicBool::new(false);

/// 进程级路由表。
static ROUTER: OnceLock<StorageRouter> = OnceLock::new();

/// 取进程级路由表（未装载时为空表）。
///
/// # 返回值
///
/// 全局路由表单例。
pub fn global_router() -> &'static StorageRouter {
    ROUTER.get_or_init(StorageRouter::new)
}

/// 从数据库重建路由表。
///
/// # 返回值
///
/// 成功装载的后端数量。
///
/// # 异常情况
///
/// - 主密钥缺失 / 非法：500（消息说明如何注入，不含密钥内容）；
/// - 数据库异常：500。
pub async fn reload_router() -> std::result::Result<usize, ApiError> {
    let key = MasterKey::from_env()
        .map_err(|error| ApiError::new(500, error_code::STORAGE_CREDENTIAL_INVALID, error.to_string()))?;
    let configs = model::load_enabled_configs()
        .await
        .map_err(|error| utils::db_error(error, "装载存储后端配置"))?;
    let count = global_router().replace(&configs, &key);
    ROUTER_LOADED.store(true, Ordering::Release);
    tracing::info!(
        backends = count,
        default = ?global_router().default_code(),
        "存储路由表已装载"
    );
    Ok(count)
}

/// 确保路由表已装载（懒装载，失败时下次请求会重试）。
///
/// # 返回值
///
/// 装载成功（或此前已装载）返回 `Ok(())`。
///
/// # 异常情况
///
/// 同 [`reload_router`]。
pub async fn ensure_router() -> std::result::Result<(), ApiError> {
    if ROUTER_LOADED.load(Ordering::Acquire) {
        return Ok(());
    }
    reload_router().await.map(|_| ())
}

/// 写操作后尽力重建路由表（失败只记日志，不影响已经成功的数据库写入）。
///
/// # 说明
///
/// 配置已经落库，重建失败属于「运行期降级」：`ROUTER_LOADED` 保持 `false`，
/// 下一个需要后端的请求会再次尝试装载，因此不需要人工重启。
pub async fn reload_router_best_effort(context: &str) {
    if let Err(error) = reload_router().await {
        tracing::warn!(
            context = context,
            code = error.code,
            message = %error.msg,
            "存储路由表重建失败，将在下次请求时重试"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全局路由表单例：多次调用必须是同一实例。
    #[test]
    fn global_router_is_singleton() {
        let first = global_router() as *const StorageRouter;
        let second = global_router() as *const StorageRouter;
        assert_eq!(first, second);
    }
}
