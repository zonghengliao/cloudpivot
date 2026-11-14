//! 任务侧审批接缝的**唯一实现**（P2-37 · P2-B 解环）。
//!
//! # 这个文件解决什么
//!
//! `task::approval_port` 定义了「任务需要审批引擎做什么」（trait + 注册表），
//! 本文件把它落到 `flow::task_bridge` 上并**在启动期注入**。依赖方向因此变成：
//!
//! ```text
//! task ──trait/注册表──→ (抽象) ←──实现 + 启动期注册── flow::task_approval_impl
//! ```
//!
//! `task` 一行都不 import `flow`（闸门 `task::approval_port_tests` 基线 0），
//! 而 `flow → task` 维持原样（`task_bridge` 要写 `biz_task`，那半边是 P2-38 的活）。
//!
//! # 为什么实现放这儿而不是放 `task_bridge.rs`
//!
//! 与 `erp/doc_reader_impl.rs`、`hr/employee_reader_impl.rs` 同一形态：
//! 接缝的**实现在能力属主的域**、单独一个文件，一眼能看出「谁实现了谁」。
//! `task_bridge.rs` 已经 2800 行，再塞适配层会把「桥本身」和「桥的注册适配」搅在一起。
//!
//! # 本文件只做**搬运**，不改语义
//!
//! 每个方法都是一次直接委派（`pub async fn start_in_tx(..)` → `task_bridge::start_in_tx(..)`），
//! 唯一的加工是把审批域的枚举翻成任务侧的枚举（[`ApprovalOutcome`] / [`CancelledCounts`]）。
//! **没有**新增查询、没有改事务边界、没有改错误口径 —— P2-37 是搬家卡。
//!
//! # 一处必须留意的等价性
//!
//! `CancelCause::Deleted` 与 `Cancelled` 走的是 `task_bridge` 的两个不同入口
//! （文案不同：`任务已删除…` / `任务已取消…`），**不许合并**：审批中心里这两句话
//! 是不同的动作证据（T12 / P11 的 e2e 按文案做精确断言）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::engine;
use crate::business::flow::model;
use crate::business::flow::task_bridge::{
    self, AcceptStart, FlowTaskOutcome, TASK_CANCEL_COMMENT, TASK_DELETE_CANCEL_COMMENT,
};
use crate::business::task::approval_port::{
    self, AcceptRequest, ApprovalOutcome, CancelCause, CancelledCounts, PreparedAccept, TaskApprovalPort,
    TaskStatusChange,
};

/// 任务侧审批接缝的实现（委派给 `flow::task_bridge`）。
#[derive(Debug, Default)]
pub struct TaskApprovalBridge;

/// 启动期把本实现注册进 `task::approval_port`（`bootstrap::register_task_approval_port` 调用）。
///
/// # 异常情况
///
/// 重复安装 → `Err`（`bootstrap` 按编程错误处理：prod `exit(1)`、其它 profile `panic`）。
pub fn install() -> std::result::Result<(), String> {
    approval_port::install(Arc::new(TaskApprovalBridge))
}

#[async_trait]
impl TaskApprovalPort for TaskApprovalBridge {
    fn name(&self) -> &'static str {
        "flow::task_bridge"
    }

    async fn instance_status_of(&self, approval_instance_id: Option<Uuid>) -> DbResult<Option<String>> {
        task_bridge::instance_status_of(approval_instance_id).await
    }

    async fn prepare_accept(&self, req: AcceptRequest) -> DbResult<Option<PreparedAccept>> {
        let prepared =
            task_bridge::prepare_accept(req.task_id, &req.task_no, &req.title, req.initiator, req.form_data).await?;
        // 句柄里装的是审批域自己的发起参数；任务侧不解释它（见 `PreparedAccept` 文档）。
        Ok(prepared.map(PreparedAccept::wrap))
    }

    async fn start_in_tx(
        &self,
        tx: &Transaction,
        prepared: &PreparedAccept,
        task_id: Uuid,
        rejected: &Mutex<Option<ApiError>>,
    ) -> DbResult<Uuid> {
        let Some(start) = prepared.downcast_ref::<AcceptStart>() else {
            // 句柄不是本实现造的：只可能是「装了第二个实现又混用」，按编程错误报出来。
            return Err(DbError::InvalidValue(
                "审批发起句柄不是 flow::task_bridge 造的（PreparedAccept 类型不符）".to_string(),
            ));
        };
        task_bridge::start_in_tx(tx, start, task_id, rejected).await
    }

    async fn on_status_changed(&self, tx: &Transaction, event: TaskStatusChange<'_>) -> DbResult<ApprovalOutcome> {
        let outcome = task_bridge::on_task_status_changed(
            tx,
            task_bridge::TaskStatusChanged {
                task_id: event.task_id,
                instance_id: event.instance_id,
                node_key: event.node_key,
                round: event.round,
                from: event.from,
                to: event.to,
                operator: event.operator,
                operator_name: event.operator_name,
                reason: event.reason,
            },
        )
        .await?;
        Ok(into_outcome(outcome))
    }

    async fn cancel_pending(
        &self,
        tx: &Transaction,
        task_id: Uuid,
        task_no: &str,
        cause: CancelCause,
    ) -> DbResult<CancelledCounts> {
        let out = match cause {
            CancelCause::Deleted => task_bridge::cancel_on_task_deleted(tx, task_id, task_no).await?,
            CancelCause::Cancelled => task_bridge::cancel_on_task_cancelled(tx, task_id, task_no).await?,
        };
        Ok(into_counts(out))
    }

    fn is_start_rejected(&self, error: &DbError) -> bool {
        engine::is_start_rejected(error)
    }

    fn cancel_comment(&self, cause: CancelCause) -> &'static str {
        match cause {
            CancelCause::Deleted => TASK_DELETE_CANCEL_COMMENT,
            CancelCause::Cancelled => TASK_CANCEL_COMMENT,
        }
    }
}

/// 审批域的结果枚举 → 任务侧的结果枚举（逐值对应，**不加不减**）。
fn into_outcome(outcome: FlowTaskOutcome) -> ApprovalOutcome {
    match outcome {
        FlowTaskOutcome::NotFlowTask => ApprovalOutcome::NotFlowTask,
        FlowTaskOutcome::Advanced { next_nodes } => ApprovalOutcome::Advanced { next_nodes },
        FlowTaskOutcome::Ignored { instance_status } => ApprovalOutcome::Ignored { instance_status },
        FlowTaskOutcome::Conflict(error) => ApprovalOutcome::Conflict(error),
    }
}

/// 三张表的终止行数 → 任务侧的计数（字段逐个搬，口径不变）。
fn into_counts(out: model::CancelledOfBiz) -> CancelledCounts {
    CancelledCounts {
        instances: out.instances,
        tasks: out.tasks,
        nodes: out.nodes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_req() -> engine::StartReq {
        engine::StartReq {
            instance_id: Uuid::now_v7(),
            def_id: Uuid::now_v7(),
            version_id: Uuid::now_v7(),
            version_no: 3,
            flow_code: "p237_flow".to_string(),
            flow_name: "P2-37 夹具流程".to_string(),
            biz_type: task_bridge::TASK_ACCEPT_BIZ_TYPE.to_string(),
            biz_id: Some(Uuid::now_v7()),
            biz_no: Some("P237-0001".to_string()),
            title: "P237-0001 占位标题".to_string(),
            initiator_id: Uuid::now_v7(),
            initiator_name: Some("张三".to_string()),
            initiator_dept_id: None,
            form_data: serde_json::json!({ "task_id": "x" }),
        }
    }

    /// 句柄必须能取回**同一个**发起参数（实例 id 与表单快照逐字相等）——
    /// 这条是 `PreparedAccept` 那层 `Any` 唯一真正危险的地方。
    #[test]
    fn prepared_accept_round_trip_keeps_the_request() {
        let req = start_req();
        let expected_instance = req.instance_id;
        let expected_form = req.form_data.clone();
        let handle = PreparedAccept::wrap(AcceptStart {
            instance_id: req.instance_id,
            req,
        });
        let back = handle.downcast_ref::<AcceptStart>().expect("必须是本实现造的句柄");
        assert_eq!(back.instance_id, expected_instance);
        assert_eq!(back.req.form_data, expected_form);
        assert_eq!(back.req.biz_type, "TASK_ACCEPT");
    }

    /// 类型不符 → `None`（不许 panic，也不许「猜一个」）。
    #[test]
    fn prepared_accept_wrong_type_is_none() {
        let handle = PreparedAccept::wrap(42_u32);
        assert!(handle.downcast_ref::<AcceptStart>().is_none());
        assert_eq!(handle.downcast_ref::<u32>(), Some(&42));
    }

    /// 四种结果逐值对应：少一个分支就会让「任务完成但流程没动」静默通过。
    #[test]
    fn flow_outcome_maps_one_to_one() {
        assert!(matches!(
            into_outcome(FlowTaskOutcome::NotFlowTask),
            ApprovalOutcome::NotFlowTask
        ));
        match into_outcome(FlowTaskOutcome::Advanced {
            next_nodes: vec!["n2".to_string()],
        }) {
            ApprovalOutcome::Advanced { next_nodes } => assert_eq!(next_nodes, vec!["n2".to_string()]),
            other => panic!("Advanced 应映射成 Advanced，实际 {other:?}"),
        }
        match into_outcome(FlowTaskOutcome::Ignored {
            instance_status: "PENDING".to_string(),
        }) {
            ApprovalOutcome::Ignored { instance_status } => assert_eq!(instance_status, "PENDING"),
            other => panic!("Ignored 应映射成 Ignored，实际 {other:?}"),
        }
        match into_outcome(FlowTaskOutcome::Conflict(ApiError::new(409, 65038, "冲突"))) {
            ApprovalOutcome::Conflict(error) => {
                assert_eq!(error.status, 409);
                assert_eq!(error.code, 65038);
            }
            other => panic!("Conflict 应映射成 Conflict，实际 {other:?}"),
        }
    }

    /// 三张表的行数逐个搬，不许「加总成一个数」（日志与 e2e 断言按字段 grep）。
    #[test]
    fn cancelled_counts_keep_three_columns() {
        let counts = into_counts(model::CancelledOfBiz {
            instances: 1,
            tasks: 2,
            nodes: 3,
        });
        assert_eq!(
            counts,
            CancelledCounts {
                instances: 1,
                tasks: 2,
                nodes: 3
            }
        );
        assert!(!counts.is_empty());
        assert!(CancelledCounts::default().is_empty());
    }

    /// 两条撤销入口的审计文案**不许合并**（审批中心里它们是不同的动作证据）。
    #[test]
    fn cancel_comments_are_distinct() {
        let bridge = TaskApprovalBridge;
        assert_eq!(
            bridge.cancel_comment(CancelCause::Deleted),
            "任务已删除，审批单自动终止"
        );
        assert_eq!(
            bridge.cancel_comment(CancelCause::Cancelled),
            "任务已取消，审批单自动终止"
        );
    }

    /// `name()` 是排障锚点（启动摘要里给运维看），改名会让「装了谁」查不到。
    #[test]
    fn name_is_stable() {
        assert_eq!(TaskApprovalBridge.name(), "flow::task_bridge");
    }

    /// `source='FLOW'` 的**值**在两个域各有一份常量（任务侧读、审批侧写）。
    ///
    /// 漂移的后果是**静默**的：任务侧 `is_flow_task` 会判成 `false`，流程任务的
    /// 「完成 → 推进节点」再也不触发，而任务状态照样落库。
    /// 这条用例把「两份字面量必须相等」变成编译后立刻能跑出来的事实。
    /// 收口到一份是 P2-38 的活（等 `flow` 不再写 `biz_task` 时才能只留任务侧那份）。
    #[test]
    fn flow_source_literal_matches_task_side() {
        assert_eq!(
            crate::business::flow::task_node::FLOW_SOURCE,
            crate::business::task::dto::SOURCE_FLOW,
            "FLOW_SOURCE（审批侧写入）与 SOURCE_FLOW（任务侧判断）漂移了"
        );
    }
}
