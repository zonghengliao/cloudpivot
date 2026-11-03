//! 合同审批的**业务回调 + 反向兜底**（ARCH-3 §6 R-P1-1 · 断链 DC-02）。
//!
//! 与 `purchase_bridge` / `stockin_bridge` / `payable_bridge` 并列的一类业务回调：
//! 审批单落终态之后，把结论翻译成合同的状态。**彼此互不影响**（同一条实例按
//! `biz_type` 分流，别的业务的桥直接 return）。
//!
//! # 为什么合同也要这一层（问题原样）
//!
//! R-P1-1 之前：`biz_contract` 没有 `approval_instance_id`，合同在列表里点「登记签订」
//! （`DRAFT → SIGNED`）一步就变成**可被采购单引用、可用于付款**的已签状态，
//! 全程没有任何人审批。而采购单自己有 `po_approve` 复核 —— 结果就是「给合同付钱」
//! 这件事只审了单、没审合同，内控在最上游漏了一整环。
//!
//! 现在合同的状态链是 `DRAFT → PENDING_APPROVAL → SIGNED → ACTIVE`：
//!
//! ```text
//! PENDING_APPROVAL --APPROVED------------------▶ SIGNED（三个日期必填，审批人落 approved_by）
//! PENDING_APPROVAL --REJECTED/WITHDRAWN/CANCELLED--▶ DRAFT（清实例与提交痕迹，可改后重提）
//! ```
//!
//! # 三个入口
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_contract_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance}`（**审批事务已提交之后**） | `APPROVED` → `PENDING_APPROVAL → SIGNED`；其余终态 → `PENDING_APPROVAL → DRAFT` |
//! | [`cancel_approval_of`] | 合同软删 / 审批中直接改状态等「其它路径」 | 把在跑的审批**终止**（`biz_flow_task` / 节点 / 主表一起翻 `CANCELLED`），不留孤儿待办 |
//! | [`internal_control_creator`] | `flow::internal_control`（生成待办**之前**） | 提供「制单人」，把他从审批人列表里排除（职责分离） |
//!
//! # 时序与失败口径（与 `purchase_bridge` 同款）
//!
//! 回调跑在**审批事务提交之后**：审批结论已经落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None`。合同停在 `PENDING_APPROVAL` 由人工或下一轮处置
//! （合同没有账，最坏情况是「审批过了、合同显示审批中」，不会产生错账）——
//! 这正是不变量 H4（`flow_instance.status = APPROVED` 而单据仍在待审批态）要抓的形态。
//!
//! # 边界（刻意不做的事）
//!
//! * **不改状态机边表**：`PENDING_APPROVAL → SIGNED/DRAFT` 两条边在
//!   `state_machine::CONTRACT_EDGES` 里，本模块只按 `Entity::Contract` 过闸后调用。
//! * **不写 `remark`**：驳回意见写在 `biz_status_log.reason`（状态机负责），
//!   合同 `remark` 是用户自己的备注，审批意见往里塞会污染用户的原文
//!   （采购单往里追加是因为它没有别的留痕位，合同有 `biz_status_log`）。
//! * **不自动补流程**：`contract_approve` 由迁移 `V0209` 种；客户删了/停用了流程，
//!   提交入口按 `approval_gate` 的 fail-closed 口径拒绝（409 + 点名流程编码），
//!   不在这里兜底成「直接生效」。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::contract_model::{
    self, STATUS_DRAFT, STATUS_PENDING_APPROVAL, STATUS_SIGNED,
};
use crate::business::flow::model as flow_model;
use crate::business::flow::settle_guard;
use crate::business::shared::state_machine;
use crate::shared::error_code;

/// 合同审批的业务类型（与 `V0209` 种子 / `contract_handler::submit_approval` 同口径）。
pub const BIZ_TYPE: &str = "CONTRACT_APPROVE";
/// 合同审批的流程编码（**一个字都不能错**：查不到流程 = 提交入口 409）。
pub const FLOW_CODE: &str = "contract_approve";

/// 已发布的合同审批流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`CONTRACT_APPROVE`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的合同审批流程」。`None` = **没配流程**（不是错误，由调用方按
/// `approval_gate` 的 fail-closed 口径处理）。
///
/// 排序口径与 `purchase_bridge::published_po_flow` 完全一致（`biz_type` 精确命中优先 →
/// `is_default` → 版本号 → 更新时间）。
pub async fn published_contract_flow() -> DbResult<Option<ContractFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的合同该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'CONTRACT_APPROVE' OR lower(d.flow_code) = 'contract_approve') \
              ORDER BY (d.biz_type = 'CONTRACT_APPROVE') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(ContractFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

// ==================== 回调：审批结论 → 合同 ====================

/// 审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractVerdict {
    /// 同意 → `PENDING_APPROVAL → SIGNED`（合同变成「已签订」，可被采购单引用）。
    Approve,
    /// 驳回 / 撤回 / 终止 → `PENDING_APPROVAL → DRAFT`（回位，附意见；实例清空）。
    BackToDraft,
}

impl ContractVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            ContractVerdict::Approve => "APPROVE",
            ContractVerdict::BackToDraft => "BACK_TO_DRAFT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动合同）。
pub fn contract_verdict(instance_status: &str) -> Option<ContractVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(ContractVerdict::Approve),
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(ContractVerdict::BackToDraft),
        _ => None,
    }
}

/// 审批单落终态 → 推进合同状态（`FlowCallback` 身份）。
///
/// | 实例状态 | 合同 | 说明 |
/// |---|---|---|
/// | `APPROVED` | `PENDING_APPROVAL → SIGNED` | 已签订；之后才能「生效」并被采购单引用 |
/// | `REJECTED` / `WITHDRAWN` / `CANCELLED` | `PENDING_APPROVAL → DRAFT` | 回位草稿，实例清空，可改后重提 |
/// | 其它（`PENDING` / 空） | 不动 | 还在跑 |
///
/// 返回受影响的合同 id（没推进 / 不该推进 → `None`）。**不返回错误**：审批已落库，
/// 业务侧失败只记日志（与 `purchase_bridge::on_po_settled` 同口径）。
pub async fn on_contract_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = contract_verdict(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(contract_id)) => {
            tracing::info!(
                %instance_id,
                %contract_id,
                verdict = verdict.as_str(),
                "合同审批结论已回调单据"
            );
            Some(contract_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "合同审批结论回调失败（不影响审批单，合同仍是审批中）"
            );
            None
        }
    }
}

/// [`on_contract_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: ContractVerdict) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是「合同审批」类的（别的业务类型与本回调无关）
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过合同审批回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(contract_id) = instance.biz_id else {
        return Ok(None);
    };
    // ② 合同必须存在（能不能收这个结论交给 ④ 的统一前置校验）
    let Some(row) = contract_model::contract_find(contract_id).await? else {
        tracing::warn!(%contract_id, "合同不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    // ③ 审批人（落库用）：引擎把「谁点的」写在待办行，实例行没有该字段
    let approver = latest_completed_assignee(instance_id).await?;
    // ④ 审批意见（驳回时写进 biz_status_log.reason，让人一眼看到为什么被打回）
    let comment = latest_reject_comment(instance_id).await?;
    let (to_status, reason) = match verdict {
        ContractVerdict::Approve => (STATUS_SIGNED, "合同审批通过（已签订）".to_string()),
        ContractVerdict::BackToDraft => (
            STATUS_DRAFT,
            match comment.as_deref().map(str::trim) {
                Some(text) if !text.is_empty() => format!("审批驳回：{text}"),
                _ => "审批驳回（未填写意见）".to_string(),
            },
        ),
    };
    // ⑤ 写目标状态前先校验合同当前状态还在「能接受这个审批结论」的合法路径上
    //    （`PENDING_APPROVAL → SIGNED|DRAFT`）。合同已被别的路径推走 / 已作废 →
    //    不落状态 + WARN + 审批实例留痕（审批单本身仍是它自己的结论）。
    if !settle_guard::gate(
        &instance,
        state_machine::Entity::Contract,
        &row.contract_no,
        &row.status,
        STATUS_PENDING_APPROVAL,
        to_status,
    )
    .await
    {
        return Ok(None);
    }
    // ⑥ 审批通过 → 已签订：DDL 的 `ck_biz_contract_signed_dates` 要求三个日期齐全。
    //    提交审批时已经校验过一次，但「草稿能改」意味着这期间可能被改薄 —— 这里再核一次，
    //    否则 DB CHECK 会以 500 的形式炸在回调里（审批已通过、合同没动，最难排查）。
    if to_status == STATUS_SIGNED {
        let missing: Vec<&str> = [
            ("sign_date", row.sign_date.is_none()),
            ("start_date", row.start_date.is_none()),
            ("end_date", row.end_date.is_none()),
        ]
        .into_iter()
        .filter_map(|(name, absent)| absent.then_some(name))
        .collect();
        if !missing.is_empty() {
            tracing::warn!(
                %contract_id,
                %instance_id,
                missing = %missing.join("/"),
                "合同审批通过但三个日期不齐，跳过状态回写（合同停在审批中，等人工补日期）"
            );
            return Ok(None);
        }
    }
    let tx = db().begin().await?;
    let outcome = async {
        state_machine::transition(
            &tx,
            state_machine::ENTITY_CONTRACT,
            contract_id,
            &row.contract_no,
            &contract_model::LEGAL_EDGES,
            STATUS_PENDING_APPROVAL,
            to_status,
            approver,
            Some(&reason),
            Some((BIZ_TYPE, instance.instance_no.as_str())),
        )
        .await?;
        // 通过：实例保留作审计（详情显示「审批通过 FLW-xxx」）；
        // 回位：实例必须清空，否则下次提交会撞 `uk_biz_flow_instance_biz_active`。
        let instance_ref = match verdict {
            ContractVerdict::Approve => Some(instance_id),
            ContractVerdict::BackToDraft => None,
        };
        let affected = contract_model::contract_apply_approval_transition(
            &tx,
            contract_id,
            STATUS_PENDING_APPROVAL,
            to_status,
            approver,
            instance_ref,
        )
        .await
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("合同状态回写失败：{error}"),
            )
        })?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                "合同状态在审批回调期间已被改动，请刷新后重试",
            ));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(Some(contract_id))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            // 回调失败不回滚审批单：合同停在 PENDING_APPROVAL 等人工处置
            tracing::warn!(
                %contract_id,
                code = error.code,
                msg = %error.msg,
                "合同审批结论回写失败"
            );
            Ok(None)
        }
    }
}

// ==================== 反向：合同删掉 / 撤回 → 终止在跑的审批 ====================

/// 把某张合同**在跑的审批**终止掉（`biz_flow_task` / 节点 / 主表一起翻 `CANCELLED`）。
///
/// 谁调：`contract_handler::contract_delete`（审批中删单是合法反向路径）、
/// `contract_handler::contract_change_status`（审批中直接 PATCH 回 `DRAFT` = 撤回）。
///
/// 为什么必须有：审批实例的 `biz_id` 是**弱引用**，合同删掉/回位而实例还在跑，
/// 就会留下一条永远等不到回调的孤儿实例 —— 并且 `uk_biz_flow_instance_biz_active`
/// 会把这张合同**下一次提交审批**永久堵死（409 且无法自愈）。
///
/// 返回被终止的实例数（`None` = 本来没有在跑的审批）。
pub async fn cancel_approval_of(
    ex: &impl SqlExecutor,
    contract_id: Uuid,
    contract_no: &str,
    reason: &str,
) -> DbResult<Option<u64>> {
    let cancelled = flow_model::cancel_pending_of_biz(ex, contract_id, contract_no, reason).await?;
    if cancelled.instances > 0 {
        tracing::info!(
            %contract_id,
            contract_no = contract_no,
            instances = cancelled.instances,
            tasks = cancelled.tasks,
            reason = reason,
            "合同在跑的审批已终止（不再留孤儿待办）"
        );
        Ok(Some(cancelled.instances))
    } else {
        Ok(None)
    }
}

// ==================== 内控：制单人不能批自己的合同 ====================
/// 审批入口的内控闸（**事务之前**调，越早拒绝越好）。
///
/// # 判据（任一命中即 403）
///
/// 1. `operator` == `biz_contract.create_by`（**制单人**）；
/// 2. `operator` == 审批单的 `initiator_id`（发起人；代提场景下发起人 ≠ 制单人）。
///
/// # 为什么不放在流程节点的 `self_approval=FORBID`
///
/// 与 `purchase_bridge::guard_self_approval` 同款：`FORBID` 在**发起时**就 `422 + 65019`
/// （整单起不来），而内控要的是「提交成功、但**这一票**点不动」。所以流程节点写 `ALLOW`
/// （待办照常可见、留痕），真正的否决在这里。
///
/// 与本业务无关的待办（其它 `biz_type`）直接放行，零影响。
pub async fn guard_self_approval(
    task_id: Uuid,
    operator: Uuid,
) -> std::result::Result<(), ApiError> {
    let Some(task) = flow_model::find_task(task_id).await.map_err(|error| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("查询待办失败：{error}"),
        )
    })?
    else {
        // 待办不存在交给后续流程报 404（这里不抢它的错）
        return Ok(());
    };
    let Some(instance) = flow_model::find_instance(task.instance_id)
        .await
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("查询审批单失败：{error}"),
            )
        })?
    else {
        return Ok(());
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(());
    }
    // ERP-B2：「制单与审批分离」开关（默认开 = 改动前的内控闸；关掉 = 小团队允许本人自审）
    if !crate::business::erp::separation::enabled(crate::business::erp::separation::Duty::Approve)
        .await
    {
        return Ok(());
    }
    let Some(contract_id) = instance.biz_id else {
        return Ok(());
    };
    let Ok(Some(row)) = contract_model::contract_find(contract_id).await else {
        return Ok(());
    };
    if row.create_by == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己提交的合同：合同 {} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理{}",
                row.contract_no,
                crate::business::erp::separation::approval_hint(
                    crate::business::erp::separation::Duty::Approve
                )
            ),
        ));
    }
    Ok(())
}

/// 内控用：这张合同的**制单人**（`biz_contract.create_by`）。
///
/// 供 `flow::internal_control` 在**生成待办前**把制单人从审批人列表里排除
/// （落 `SKIPPED` 留痕，而不是让人看到待办再 403）。
/// 查不到单 / 单没有 `create_by`（历史行 / 外部导入）→ `None`：只是没人被排除，
/// **不影响**审批能不能发起（内控不能因为取不到数据就把流程锁死）。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(contract_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪（裁剪会让
    // 「看不到这张单的人」反而成为可审批人，与内控目的相反）。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_contract WHERE id = ?::uuid",
            vec![Value::from(&contract_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => {
            let creator: Option<Uuid> = row.get_typed("create_by")?;
            Ok(creator)
        }
    }
}

// ==================== 小工具：审批人 / 驳回意见 ====================

/// 最近一条**已完成**待办的审批人（多节点取最后一票）。读不到 → `None`。
async fn latest_completed_assignee(instance_id: Uuid) -> DbResult<Option<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status IN ('APPROVED','REJECTED') \
              ORDER BY COALESCE(completed_at, created_at) DESC, created_at DESC \
              LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("assignee_id").ok().flatten()),
    }
}

/// 最近一条**驳回**意见（写进 `biz_status_log.reason`，让人看到被打回的理由）。
async fn latest_reject_comment(instance_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT comment FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'REJECTED' \
              ORDER BY COALESCE(completed_at, created_at) DESC, created_at DESC \
              LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("comment").ok().flatten()),
    }
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// ① 实例状态 → 业务语义的映射口径（钉死：只有 APPROVED 会让合同变成「已签订」）。
    #[test]
    fn verdict_maps_only_final_states() {
        assert_eq!(contract_verdict("APPROVED"), Some(ContractVerdict::Approve));
        assert_eq!(
            contract_verdict(" approved "),
            Some(ContractVerdict::Approve)
        );
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
            assert_eq!(
                contract_verdict(status),
                Some(ContractVerdict::BackToDraft),
                "{status} 必须打回草稿（不是关闭、不是作废）"
            );
        }
        for status in ["PENDING", "", "APPROVED2", "DRAFT"] {
            assert_eq!(contract_verdict(status), None, "{status} 不该推进合同");
        }
    }

    /// ② 业务类型 / 流程编码 / 回调三态必须与状态机的边**逐字对齐**。
    ///
    /// 这条用例的价值：回调里写死的 `STATUS_PENDING_APPROVAL → SIGNED/DRAFT`
    /// 一旦有人改边表（比如把回位边删了），这里第一时间红 —— 而不是等到真机审批
    /// 回调 500 才发现。
    #[test]
    fn verdict_targets_are_legal_contract_edges() {
        assert_eq!(BIZ_TYPE, "CONTRACT_APPROVE");
        assert_eq!(FLOW_CODE, "contract_approve");
        let entity = state_machine::Entity::Contract;
        for (status, verdict) in [
            ("APPROVED", ContractVerdict::Approve),
            ("REJECTED", ContractVerdict::BackToDraft),
            ("WITHDRAWN", ContractVerdict::BackToDraft),
            ("CANCELLED", ContractVerdict::BackToDraft),
        ] {
            let parsed = contract_verdict(status).expect("终态必须能翻译");
            assert_eq!(parsed, verdict);
            let target = match verdict {
                ContractVerdict::Approve => STATUS_SIGNED,
                ContractVerdict::BackToDraft => STATUS_DRAFT,
            };
            assert!(
                entity.can_transition(STATUS_PENDING_APPROVAL, target),
                "{status} → {target} 不在合同合法边表里（回调会 500）"
            );
        }
        // 回调绝不写这两个状态：合同没有 CANCELLED，也不许从审批中直接生效
        assert!(!entity.can_transition(STATUS_PENDING_APPROVAL, "ACTIVE"));
        assert!(!entity.can_transition(STATUS_PENDING_APPROVAL, "CANCELLED"));
    }
}
