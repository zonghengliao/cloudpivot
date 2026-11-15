//! 系统参数（`sys_config`）读取接缝（P2-59 · C1 解环）。
//!
//! # 为什么有此接缝
//!
//! 基座里有多处「读一个 `sys_config` 键」的判定（空闲超时 [`crate::shared::idle_session`]、
//! 成本核算总开关 [`crate::business::shared::cost_switch`]、ERP 内控开关
//! [`crate::business::shared::separation`]）。参数的**存储与快照**属 `support::config`
//! （`SysConfigModel` + 进程内快照 + 60 s TTL），而这些判定属基座 ——
//! 直接 `use crate::support::config::model::SysConfigModel` 会让基座反向依赖平台层，
//! 抽包后就是**包级环**（`cloudpivot-kernel → support → cloudpivot-kernel`）。
//!
//! 所以：基座只认这张注册表，实现在 `support::config::model` 里，启动期由 `bootstrap`
//! 安装（与 `voucher_port` / `doc_reader` 同一套先例）。
//!
//! # 未安装时的语义（重要）
//!
//! 读参数是**建议性**的：所有调用点都有明确兜底（空闲超时回默认值、开关回 `false`）。
//! 因此未安装时**不 panic、不报错**，返回 `None` 并打一次 `warn` —— 语义等于「参数不存在」。
//! 这与写路径接缝（`voucher_port` 未安装 = 刺眼 500）刻意不同：那是账务，这是配置。
//!
//! # 三条纪律
//!
//! 1. **不许跨 `await` 持锁**：先克隆 `Arc` 再 `await`；
//! 2. **读锁 poisoning 直接放行**（`into_inner`）：注册期 panic 不该让之后每次读参数都失败；
//! 3. **只有一个写入口** [`install`]，调用方固定是启动链（`bootstrap::register_kernel_ports`）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;

/// 「按 `config_key` 读一个参数文本值」的能力（**唯一实现在 `support::config`**）。
///
/// 语义与 `SysConfigModel::read_text` 完全一致：参数存在、启用且值非空时返回 `Some`，
/// 否则 `None`（调用方据 `None` 回退内置默认值）。
#[async_trait]
pub trait ConfigReader: Send + Sync {
    /// 读取参数文本值（命中进程内快照则不查库）。
    async fn read_text(&self, key: &str) -> Option<String>;

    /// 实现方自报名字（启动日志 / 排障用）。
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

/// 进程级注册表（同一时刻只有一个实现）。
struct Registry {
    reader: RwLock<Option<Arc<dyn ConfigReader>>>,
}

impl Registry {
    fn install(&self, reader: Arc<dyn ConfigReader>) -> std::result::Result<(), String> {
        let mut slot = self.reader.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_some() {
            return Err("配置读取接缝已经安装过实现（重复安装是编程错误）".to_string());
        }
        *slot = Some(reader);
        Ok(())
    }

    /// 取当前实现（**不跨 `await` 持锁**：克隆 `Arc` 后立刻释放读锁）。
    fn current(&self) -> Option<Arc<dyn ConfigReader>> {
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

/// 「未安装」只告警一次，避免高频读路径刷屏。
static MISSING_WARNED: AtomicBool = AtomicBool::new(false);

/// 安装实现（**唯一写入口**，只该在启动链里调一次）。
///
/// # 异常情况
///
/// 重复安装返回 `Err`：两个配置读取实现同时存在一定是编程错误。
pub fn install(reader: Arc<dyn ConfigReader>) -> std::result::Result<(), String> {
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

/// 读取参数文本值。
///
/// # 参数
///
/// - `key`：参数键（`sys_config.config_key`）。
///
/// # 返回值
///
/// 参数存在、启用且值非空时返回 `Some`，否则 `None`（含「接缝未安装」）。
pub async fn read_text(key: &str) -> Option<String> {
    let Some(reader) = registry().current() else {
        if !MISSING_WARNED.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                config_key = key,
                "配置读取接缝未安装（启动链漏了一步）：本次及后续读参数一律回退内置默认值"
            );
        }
        return None;
    };
    reader.read_text(key).await
}

/// 读取整数型参数（与 `SysConfigModel::read_i64` 同口径：文本再解析为 `i64`）。
///
/// # 参数
///
/// - `key`：参数键。
///
/// # 返回值
///
/// 可解析为 `i64` 时返回 `Some`，否则 `None`。
pub async fn read_i64(key: &str) -> Option<i64> {
    read_text(key).await?.trim().parse::<i64>().ok()
}

/// 解析布尔型参数文本（`true` / `1` 为真，`false` / `0` 为假）。
///
/// **唯一实现**：`support::config::model::parse_bool` 已改成对本函数的转发，
/// 基座与平台层共用同一份口径（避免两处对 `yes` / ` True ` 这类脏值判法不一致）。
///
/// # 参数
///
/// - `raw`：参数文本（大小写不敏感，首尾空白忽略）。
///
/// # 返回值
///
/// 可识别为布尔值时返回 `Some`，否则 `None`。
pub fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! 接缝自身的纪律用例（离线）：注册表行为 + 未安装时的语义。
    //!
    //! 用**局部** `Registry` 而不是进程级那一份：全局注册表被别的用例装过一次之后
    //! 就再也测不了「未安装」，而进程级状态在同一个测试二进制里是共享的。

    use super::*;

    /// 假实现：值表由用例给定，证明「调用方真的穿过了接缝」。
    struct FakeReader {
        values: Vec<(&'static str, &'static str)>,
    }

    #[async_trait]
    impl ConfigReader for FakeReader {
        async fn read_text(&self, key: &str) -> Option<String> {
            self.values
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    fn fake(values: Vec<(&'static str, &'static str)>) -> Arc<dyn ConfigReader> {
        Arc::new(FakeReader { values })
    }

    /// ① 未安装：返回 `None`（不 panic、不报错）。
    #[tokio::test]
    async fn registry_returns_none_when_not_installed() {
        let registry = Registry {
            reader: RwLock::new(None),
        };
        assert!(registry.current().is_none());
    }

    /// ② 安装后读到实现的值；重复安装必须报错（两个实现同时存在是编程错误）。
    #[tokio::test]
    async fn install_once_then_reads_through() {
        let registry = Registry {
            reader: RwLock::new(None),
        };
        registry
            .install(fake(vec![("demo.key", "42")]))
            .expect("首次安装应成功");
        let reader = registry.current().expect("安装后应能取到实现");
        assert_eq!(reader.read_text("demo.key").await.as_deref(), Some("42"));
        assert_eq!(reader.read_text("missing.key").await, None);
        assert!(
            registry.install(fake(vec![])).is_err(),
            "重复安装必须报错（否则谁生效取决于启动顺序）"
        );
    }

    /// ③ 整数解析口径：脏值 / 空白 / 非数字一律 `None`（回退默认值，不 panic）。
    #[tokio::test]
    async fn read_i64_parses_text_and_rejects_dirt() {
        let registry = Registry {
            reader: RwLock::new(None),
        };
        registry
            .install(fake(vec![("int.ok", " 30 "), ("int.dirty", "30m"), ("int.empty", "")]))
            .expect("安装应成功");
        let reader = registry.current().expect("实现应存在");
        let parse = |raw: Option<String>| raw?.trim().parse::<i64>().ok();
        assert_eq!(parse(reader.read_text("int.ok").await), Some(30));
        assert_eq!(parse(reader.read_text("int.dirty").await), None);
        assert_eq!(parse(reader.read_text("int.empty").await), None);
        assert_eq!(parse(reader.read_text("int.absent").await), None);
    }

    /// ④ 布尔口径（与 `support::config::model::parse_bool` 逐条一致）。
    #[test]
    fn parse_bool_matches_historic_semantics() {
        for raw in ["true", "TRUE", " True ", "1"] {
            assert_eq!(parse_bool(raw), Some(true), "raw = {raw:?}");
        }
        for raw in ["false", "FALSE", " False ", "0"] {
            assert_eq!(parse_bool(raw), Some(false), "raw = {raw:?}");
        }
        for raw in ["", " ", "yes", "no", "True!", "2"] {
            assert_eq!(parse_bool(raw), None, "raw = {raw:?} 应判为不可识别");
        }
    }
}
