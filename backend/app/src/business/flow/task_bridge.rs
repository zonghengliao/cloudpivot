//! 任务 ⇄ 审批流 的**只读**入口（P2-38 · P2-B 解环：摘掉 `flow → task`）。
//!
//! # 这个文件还剩什么
//!
//! 只剩一件事：**把任务行上的 `approval_instance_id` 翻成审批单状态**（审批门闸的输入）。
//! 它只读 `biz_flow_instance`（本域的表），不碰 `biz_task`，因此不产生任何跨域依赖。
//!
//! # 其余的都去哪了
//!
//! | 原位置（本文件） | 现位置 | 为什么 |
//! |---|---|---|
//! | `settle` / `cascade_after_flow` / `callback_task_row`（审批结论推进任务） | `business/taskflow/bridge.rs` | 写 `biz_task` 的段落归中立接线域，`flow` 不再认识任务表 |
//! | `prepare_accept` / `start_in_tx`（提交验收发起审批） | `business/taskflow/bridge.rs` | 同上（任务 → 流程的写入口） |
//! | `on_task_status_changed` / `node_action_of` / `start_task_node_approval` | `business/taskflow/bridge.rs` | 需要 `task::state` 的类型，只能落在既认识任务又认识流程的地方 |
//! | `cancel_on_task_deleted` / `cancel_on_task_cancelled` | `business/taskflow/bridge.rs` | 任务删除 / 取消时撤审批，由 `taskflow` 承接 |
//! | `on_instance_settled` / `on_task_approval_settled` / `settle_flow_node` | `business/taskflow/bridge.rs` | 审批结论落地的编排（先任务、再流程节点），跨两个域 |
//!
//! 调用方向因此变成：
//!
//! ```text
//! flow ──trait + 注册表──→ (抽象：flow::task_port)          ← 本卡：flow 侧零 task 引用
//!                              ↑ 启动期注入
//!                     business::taskflow（中立接线域）
//!                        ├──→ flow（引擎 / 实例 / 节点）
//!                        └──→ task（任务表 / 状态机）
//! ```
//!
//! `taskflow` 是**叶子**：`flow` 与 `task` 都不依赖它，它只被 `bootstrap` 装进两边的注册表。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::model;

/// 任务验收类流程的业务类型（`biz_flow_def.biz_type`，`flow::dto::normalize_biz_type` 归一化后全大写）。
pub const TASK_ACCEPT_BIZ_TYPE: &str = "TASK_ACCEPT";

/// 流程生成的任务 `source` 值（`flow::task_node::FLOW_SOURCE` 的再导出）。
///
/// # 为什么要再导出
///
/// `biz_task.source` 的取值归任务表属主，但**流程侧写入**这个值时需要一个不跨域的常量
/// 名称。留一个再导出点，外部（`erp` 等已登记调用方）继续 `flow::task_bridge::FLOW_SOURCE`
/// 即可，不必自己抄字面量。
pub use crate::business::flow::task_node::FLOW_SOURCE;

/// 任务上的 `approval_instance_id` → 审批单状态（门闸的输入）。
///
/// # 为什么不报错
///
/// `None` / 查不到实例 → `Ok(None)`：门闸判据是「读不到实例状态 → 不拦」
/// （让任务永久卡在 `REVIEW` 比放行更坏）。实例本身的合法性归 flow 引擎
/// （`65024 FLOW_INSTANCE_NOT_PENDING` 等）自己判定，本函数只负责取状态。
///
/// 真实数据库异常仍然 `Err`（调用方转 500），不把「库挂了」伪装成「没有审批」。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn instance_status_of(approval_instance_id: Option<Uuid>) -> DbResult<Option<String>> {
    let Some(instance_id) = approval_instance_id else {
        return Ok(None);
    };
    Ok(model::find_instance(instance_id).await?.map(|row| row.status))
}
