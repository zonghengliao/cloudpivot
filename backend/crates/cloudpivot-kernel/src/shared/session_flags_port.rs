//! 会话安全字段（`token_version` / 首登改密标记）读取接缝（P2-59 · C1 解环）。
//!
//! # 为什么有此接缝
//!
//! [`crate::shared::session_cache`] 是「60 s 缓存 + 变更即失效」的会话字段缓存层，
//! 它**未命中时回源数据库**，而那条 SQL 属 `system::user`（`sys_user` 表的属主）。
//! 基座直接 `use crate::system::user::model::load_session_flags` 会让
//! `cloudpivot-kernel → system` 成为抽包后的包级环；这里只留注册表，
//! 实现在 `system::user::model`，启动期由 `bootstrap` 安装。
//!
//! # 为什么不用「基座直接写一句 SQL」
//!
//! 那句话会成为 `sys_user` 的**第二份读取口径**（列名 / 软删过滤 / 类型映射各一份）。
//! 鉴权字段是最不该有第二份口径的东西：两份一旦分叉（比如某处漏了 `deleted_at IS NULL`），
//! 删号用户的旧令牌就会在缓存未命中的那一半请求上继续有效。
//!
//! # 未安装时的语义
//!
//! 与 `config_port` 不同，**这里必须大声失败**：读不到会话字段 = 无法判定令牌版本，
//! 静默当成「无字段」会让鉴权降级成「只验签」。所以未安装返回
//! [`DbError::InvalidValue`]，缓存层按「查库失败」处理（拒绝放行，而不是当成没数据）。

use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use nik::prelude::{DbError, DbResult};
use nik::uuid::Uuid;

/// 「按 `user_id` 读会话安全字段」的能力（**唯一实现在 `system::user::model`**）。
///
/// 返回值口径与 `system::user::model::load_session_flags` 完全一致：
/// `Ok(None)` = 用户不存在或已软删；`Ok(Some((token_version, must_change_password)))` = 命中。
#[async_trait]
pub trait SessionFlagsReader: Send + Sync {
    /// 读取会话字段。
    async fn load_session_flags(&self, user_id: &Uuid) -> DbResult<Option<(i32, bool)>>;

    /// 实现方自报名字（启动日志 / 排障用）。
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

/// 进程级注册表（同一时刻只有一个实现）。
struct Registry {
    reader: RwLock<Option<Arc<dyn SessionFlagsReader>>>,
}

impl Registry {
    fn install(&self, reader: Arc<dyn SessionFlagsReader>) -> std::result::Result<(), String> {
        let mut slot = self.reader.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_some() {
            return Err("会话字段读取接缝已经安装过实现（重复安装是编程错误）".to_string());
        }
        *slot = Some(reader);
        Ok(())
    }

    /// 取当前实现（**不跨 `await` 持锁**）。
    fn current(&self) -> Option<Arc<dyn SessionFlagsReader>> {
        self.reader
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// 取进程级注册表（首次调用时初始化为空）。
fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Registry {
        reader: RwLock::new(None),
    })
}

/// 安装实现（**唯一写入口**，只该在启动链里调一次）。
///
/// # 异常情况
///
/// 重复安装返回 `Err`：两个实现同时存在一定是编程错误。
pub fn install(reader: Arc<dyn SessionFlagsReader>) -> std::result::Result<(), String> {
    registry().install(reader)
}

/// 是否已安装（排障 / 用例用）。
pub fn is_installed() -> bool {
    registry().current().is_some()
}

/// 已安装实现的名字（启动摘要里打一行，排障时能一眼看出装的是谁）。
pub fn port_name() -> Option<&'static str> {
    registry().current().map(|reader| reader.name())
}

/// 读取会话安全字段（缓存未命中时的回源路径）。
///
/// # 参数
///
/// - `user_id`：用户主键。
///
/// # 返回值
///
/// `Ok(None)` = 用户不存在或已软删；否则为 `(token_version, must_change_password)`。
///
/// # 异常情况
///
/// 接缝未安装（启动链漏了一步）或实现方查库失败时返回 [`DbError`]。
pub async fn load_session_flags(user_id: &Uuid) -> DbResult<Option<(i32, bool)>> {
    let Some(reader) = registry().current() else {
        return Err(DbError::InvalidValue(
            "读取会话安全字段失败：会话字段读取接缝未安装实现。这是启动链漏了一步（编程错误）—— \
             生产环境应由 bootstrap 的 register_kernel_ports 在启动期安装，装不上就直接退出。"
                .to_string(),
        ));
    };
    reader.load_session_flags(user_id).await
}

#[cfg(test)]
mod tests {
    //! 接缝自身的纪律用例（离线）：注册表行为 + 未安装时**大声失败**。

    use super::*;

    /// 假实现：返回固定字段，证明调用方真的穿过了接缝。
    struct FakeReader {
        flags: Option<(i32, bool)>,
    }

    #[async_trait]
    impl SessionFlagsReader for FakeReader {
        async fn load_session_flags(&self, _user_id: &Uuid) -> DbResult<Option<(i32, bool)>> {
            Ok(self.flags)
        }
    }

    /// ① 未安装：局部注册表取不到实现（进程级入口按契约报错）。
    #[test]
    fn registry_is_empty_before_install() {
        let registry = Registry {
            reader: RwLock::new(None),
        };
        assert!(registry.current().is_none());
    }

    /// ② 安装后透传实现返回值；重复安装必须报错。
    #[tokio::test]
    async fn install_once_then_reads_through() {
        let registry = Registry {
            reader: RwLock::new(None),
        };
        registry
            .install(Arc::new(FakeReader { flags: Some((7, true)) }))
            .expect("首次安装应成功");
        let reader = registry.current().expect("安装后应能取到实现");
        assert_eq!(
            reader.load_session_flags(&Uuid::nil()).await.expect("应成功"),
            Some((7, true))
        );
        assert!(
            registry.install(Arc::new(FakeReader { flags: None })).is_err(),
            "重复安装必须报错"
        );
    }
}
