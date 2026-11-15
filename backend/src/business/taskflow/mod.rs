//! 任务 ⇄ 审批流 的**接线域**（P2-38 · P2-B 解环的中立落点）。
//!
//! # 为什么需要第三个域
//!
//! `task`（任务表 / 状态机）与 `flow`（审批引擎）之间有四条硬连接，全都同时读写两边：
//!
//! | 时机 | 写谁 | 读谁 |
//! |---|---|---|
//! | 任务提交验收 | `biz_flow_instance` / `biz_flow_task`（建审批单） | `biz_task`（回写 `approval_instance_id`） |
//! | 审批结论落地 | `biz_task`（状态 + 时间线） | `biz_flow_instance`（结论 / 意见） |
//! | 任务状态变更 | `biz_flow_*`（节点推进） | `biz_task`（来源实例 / 节点 / 轮次） |
//! | 任务删除 / 取消 | `biz_flow_*`（撤审批） | `biz_task` |
//!
//! 放在任何一边，那一边就必然 `import` 另一边 —— `task ⇄ flow` 的模块环摘不掉
//! （P2-37 摘掉了 `task → flow`，`flow → task` 就是本域要接的那半）。
//!
//! 于是把它搬到 `business::taskflow`：**两边都认识、两边都不认识它**，只在 `bootstrap`
//! 启动期把自己装进两边的注册表。
//!
//! ```text
//! flow ──flow::task_port──→ (抽象) ←──实现── taskflow::spawn
//! task ──task::approval_port──→ (抽象) ←──实现── taskflow::approval_impl
//! taskflow ──→ flow（引擎 / 实例 / 节点 / 门闸）
//! taskflow ──→ task（任务表 / 状态机）
//! ```
//!
//! # 边界（改这里之前先读）
//!
//! - **本域是叶子**：`flow` 与 `task` 都不许 `import crate::business::taskflow`；
//! - **只做接线**：业务判据归各自的域（负责人解析在 `flow::assignee`、状态机在
//!   `task::state`），本域不另立一套；
//! - **事务边界照旧**：`start_in_tx` / `on_status_changed` / `cancel_pending` / `spawn`
//!   都收调用方的 `&Transaction`，不许自己开事务（AGENTS 约定 11 的唯一防线）。

pub mod approval_impl;
pub mod bridge;
pub mod spawn;

#[cfg(test)]
mod delete_cancel_tests; // T12/P11 真库用例（TASK_DB_URL）：任务删除 / 取消 → 审批单终止

/// 启动期把本域的实现装进两边的注册表（`bootstrap` 调用）。
///
/// # 异常情况
///
/// 任一接缝重复安装 → `Err`（`bootstrap` 按编程错误处理：prod `exit(1)`、其它 profile `panic`）。
pub fn install_ports() -> std::result::Result<(), String> {
    spawn::install()?;
    approval_impl::install()
}
