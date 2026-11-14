//! 审批实例落终态的**外部订阅**接缝（P2-34 · P2-B 解环）。
//!
//! # 这个文件解决什么
//!
//! 审批单落终态（`APPROVED` / `REJECTED` / `WITHDRAWN`）之后，除了业务单据要跟着动账，
//! 还要触发「联动规则」（`FLOW_INSTANCE` 触发点：发通知 / 建任务 …）。那套规则的动作与
//! 数据全在 ERP 侧，于是 `flow::handler` / `flow::timeout` 的 4 个调用点被迫直连
//! `erp::linkage` —— 这就是 `flow → erp` 环的另一半。
//!
//! 本模块把「谁想听终态结论」抽象成 [`InstanceSettledHook`] + 注册表：
//!
//! | 谁 | 依赖方向 | 说明 |
//! |---|---|---|
//! | `flow`（本模块） | → 框架 | **不 import 任何业务域**：本文件里不出现业务域路径 |
//! | `erp::linkage` | → `flow` | 实现 [`InstanceSettledHook`]，启动期注册进来 |
//!
//! # 为什么不做成「直接调 ERP 的那一个函数」
//!
//! 同一个道理与 `flow::doc_reader` 一致：`flow` 是**引擎**，它只该知道「结论落库了」，
//! 不该知道「谁会因为这条结论发站内信」。以后 `hr` / `task` 想订阅同一件事，注册一个实现即可，
//! 不用再回来改 `flow`。
//!
//! # 失败口径（与业务桥同款）
//!
//! [`fire_after_commit`] **永不返回 `Err`、永不 panic**：调用点在审批事务**提交之后**，
//! 订阅方失败只记 `WARN`（订阅方自己负责打日志），绝不把「配置写错」变成「审批接口 500」
//! —— 审批结论已经落库，回滚它比漏一次通知严重得多。
//!
//! # 注册时机
//!
//! 启动期（`bootstrap` 的 `register_flow_hooks`）一次性注册，请求期只读：
//! 注册表是 `OnceLock<RwLock<Vec<_>>>`，**没有**运行期改写路径。

use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use nik::uuid::Uuid;

/// 「审批实例落终态」的订阅者（一个业务域一个实现）。
///
/// # 实现方约定
///
/// 1. **自己判状态**：`status` 传的是动作之后的实例状态原值（可能仍是 `PENDING` ——
///    中间票也会调过来，订阅方按自己的触发面过滤，不要指望调用方先筛）；
/// 2. **永不返回 `Err`**：签名没有错误通道是刻意的 —— 订阅是旁路，失败只能记日志，
///    不允许影响审批结论（见模块注释「失败口径」）；
/// 3. 返回实际执行的条数（只用于日志 / 测试断言，不参与业务判定）。
#[async_trait]
pub trait InstanceSettledHook: Send + Sync {
    /// 订阅者名字（日志与启动摘要里给人看，`snake_case`）。
    fn name(&self) -> &'static str;

    /// 审批单落终态后做自己的事。
    async fn fire(&self, instance_id: Option<Uuid>, status: &str, operator: Option<Uuid>) -> usize;
}

/// 订阅者注册表（启动期写、请求期读）。
static REGISTRY: OnceLock<RwLock<Vec<Arc<dyn InstanceSettledHook>>>> = OnceLock::new();

fn registry() -> &'static RwLock<Vec<Arc<dyn InstanceSettledHook>>> {
    REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

/// 注册一个订阅者。
///
/// # 错误
///
/// 同名订阅者已注册 → `Err`（不静默覆盖：两个实现抢一个名字时「谁生效取决于注册顺序」，
/// 这种不确定性必须在启动期暴露）。
pub fn register(hook: Arc<dyn InstanceSettledHook>) -> std::result::Result<(), String> {
    let mut hooks = registry()
        .write()
        .map_err(|_| "审批终态订阅表写锁中毒（前一次注册 panic 了）".to_string())?;
    let name = hook.name();
    if hooks.iter().any(|existing| existing.name() == name) {
        return Err(format!("审批终态订阅者 `{name}` 重复注册"));
    }
    hooks.push(hook);
    Ok(())
}

/// 已注册订阅者的名字（启动摘要 / 测试断言用）。
pub fn registered() -> Vec<&'static str> {
    match registry().read() {
        Ok(hooks) => hooks.iter().map(|hook| hook.name()).collect(),
        Err(_) => Vec::new(),
    }
}

/// 已注册数量。
pub fn len() -> usize {
    registry().read().map(|hooks| hooks.len()).unwrap_or(0)
}

/// 审批实例落终态后，把结论扇出给**所有**订阅者（`handler` 的 4 处 + `timeout` 1 处的替换点）。
///
/// # 返回值
///
/// 所有订阅者执行的条数之和（日志 / 测试断言用，不参与业务判定）。
///
/// # 失败
///
/// 没有任何错误通道：订阅方自己记 `WARN` 并返回 0。锁在 `await` 之前放掉
/// （先把 `Arc` 们克隆出来）—— 跨 `await` 持锁会把全进程的审批收尾串行化。
pub async fn fire_after_commit(instance_id: Option<Uuid>, status: &str, operator: Option<Uuid>) -> usize {
    let hooks: Vec<Arc<dyn InstanceSettledHook>> = match registry().read() {
        Ok(hooks) => hooks.iter().cloned().collect(),
        Err(_) => return 0,
    };
    let mut total = 0usize;
    for hook in &hooks {
        let name = hook.name();
        let executed = hook.fire(instance_id, status, operator).await;
        if executed > 0 {
            tracing::info!(hook = name, executed, %status, "审批终态订阅者已执行");
        }
        total += executed;
    }
    total
}

/// 清空进程级注册表（**仅供单测**）。
#[cfg(test)]
pub fn reset_for_tests() {
    if let Ok(mut hooks) = registry().write() {
        hooks.clear();
    }
}

/// 动注册表的单测串行锁（与 `flow::doc_reader::GLOBAL_LOCK` 同一个理由：
/// 注册表是进程级单例，不加锁时断言随执行顺序漂移）。
#[cfg(test)]
pub static GLOBAL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
