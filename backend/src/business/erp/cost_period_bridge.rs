//! 成本结算审批的**业务回调**（`COST_SETTLE` / `cost_settle_approve`）—— B3-3 接线。
//!
//! 设计：`docs/architecture/ARCH-9-成本核算升级设计.md` §4.2（审批流怎么接）。
//!
//! # 四个入口（与 `budget_bridge` / `expense_bridge` / `purchase_return_bridge` 同款分工）
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_cost_settle_decided`] | `flow::handler::{approve_task, reject_task, withdraw_instance, cancel_instance}`
//!   （**审批事务已提交之后**） | `APPROVED` → `SUBMITTED → APPROVED`（实例 id **保留**作审计）；
//!   `REJECTED` / `WITHDRAWN` / `CANCELLED` → `SUBMITTED → DRAFT` + **清空审批痕迹**（`approval_instance_id = NULL`）+ 意见追加 `remark` |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**事务之前**） | 制单人 / 发起人不能批自己这一票 → `403 + 65019` |
//! | [`internal_control_creator`] | `flow::internal_control`（**生成待办之前**） | 把制单人从审批人列表里排除（落 `SKIPPED` 留痕），否则财务经理本人制单 = 看得见点不动 = 死单 |
//! | [`published_cost_settle_flow`] | `cost_period_handler::submit_period` | 取已发布的 `cost_settle_approve`；没有 ⇒ 409 + `66033`（**默认拒绝，不静默直过**） |
//!
//! # 为什么「驳回」必须清空 `approval_instance_id`
//!
//! 卡面验收：*驳回 → 状态回 `DRAFT` 且清空审批痕迹*。三条理由：
//!
//! 1. **库层约束**：`ck_biz_cost_period_appr` 只要求 `SUBMITTED` 及之后的状态非空，
//!    `DRAFT` 允许 `NULL` —— 清空不破坏约束；
//! 2. **重提**：`uk_biz_flow_instance_biz_active` 只挡 `status='PENDING'` 的实例，
//!    但单据上留着一个 `REJECTED` 的旧实例 id 会让详情页显示「审批中 FLW-xxx」，
//!    与「草稿」自相矛盾（采购退货 V0332 / 合同 R-P1-1 都是这么修的）；
//! 3. **可再提交**：提交前 `biz_instance_running` 判重只看实例表，清不清都不撞，
//!    但清掉之后的单据语义是干净的「草稿」——**审批痕迹留在审批中心**（实例与待办
//!    一条都不删），不是靠单据上那个指针。
//!
//! # 失败口径（与其它 bridge 一致）
//!
//! 回调跑在审批事务**提交之后**：审批结论已落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None`。最坏情形是「批过了但批次还停在 `SUBMITTED`」——
//! 这时**审批中心仍能撤回 / 再驳回**，不构成死单（约定 11 的判据是「用户能从界面继续操作」）。
//! 结算（`APPROVED → SETTLED`）不在这里做：那是 B3-4 的事务（入库 + 三张凭证 + 6 条断言）。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_period_dto::{self, STATUS_APPROVED, STATUS_DRAFT, STATUS_SUBMITTED};
use crate::business::erp::cost_period_model as model;
use crate::business::erp::separation;
use crate::business::flow::model as flow_model;
use crate::business::flow::settle_guard;
use crate::business::shared::state_machine;
use crate::shared::error_code;

/// 成本结算审批的业务类型（与 `V0381` 种子 / `submit_period` 同口径）。
pub const BIZ_TYPE: &str = "COST_SETTLE";
/// 成本结算审批的流程编码（`V0381` 种子的 `flow_code`）。
pub const FLOW_CODE: &str = "cost_settle_approve";
/// 状态日志里「审批驳回」的备注前缀（与 V0332 采购退货同款）。
pub const REJECT_NOTE_PREFIX: &str = "审批驳回：";
/// 状态日志里「提交人撤回」的备注前缀。
pub const WITHDRAW_NOTE_PREFIX: &str = "审批撤回：";

/// 已发布的成本结算审批流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostSettleFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的成本结算审批流程」。`None` = 没配（调用方 409 并指名去发布流程）。
pub async fn published_cost_settle_flow() -> DbResult<Option<CostSettleFlow>> {
    // SCOPE-EXEMPT: 流程定义是全局配置，按 biz_type / flow_code 精确取 1 行。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'COST_SETTLE' OR lower(d.flow_code) = 'cost_settle_approve') \
              ORDER BY (d.biz_type = 'COST_SETTLE') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(CostSettleFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

/// 审批结论 → 业务语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 审批通过：`SUBMITTED → APPROVED`（批次数据冻结，等结算）。
    Approve,
    /// 驳回 / 撤回 / 终止：`SUBMITTED → DRAFT`（清空审批痕迹，可改后重提）。
    BackToDraft,
}

impl Verdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Verdict::Approve => "APPROVE",
            Verdict::BackToDraft => "BACK_TO_DRAFT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动批次）。
///
/// ⚠️ **必须是白名单**，不能写成 `!is_approved()`：两级审批批完第一站时实例还是
/// `PENDING`，用「非 APPROVED 就打回」会把刚批完财务经理的在途批次打回草稿
/// （FIN-3 真机首跑踩过的同款）。
pub fn verdict_of(instance_status: &str) -> Option<Verdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(Verdict::Approve),
        "REJECTED" | "REJECTED_TERMINATED" | "WITHDRAWN" | "CANCELLED" => Some(Verdict::BackToDraft),
        _ => None,
    }
}

/// 审批单落终态 → 推进成本核算批次状态（`FlowCallback` 身份）。
pub async fn on_cost_settle_decided(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = verdict_of(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(period_id)) => {
            tracing::info!(
                %instance_id,
                %period_id,
                verdict = verdict.as_str(),
                instance_status,
                "成本结算审批结论已回调批次"
            );
            Some(period_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                code = error.code,
                msg = %error.msg,
                "成本结算审批结论回调失败（不影响审批单；批次仍在审批中，可再撤回 / 驳回）"
            );
            None
        }
    }
}

/// 审批结论的实现体：自己开事务（回调路径用）。
async fn settle(instance_id: Uuid, verdict: Verdict) -> std::result::Result<Option<Uuid>, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| internal(error, "开启审批回调事务"))?;
    match settle_in(&tx, instance_id, Some(verdict)).await {
        Ok(value) => {
            tx.commit().await.map_err(|error| internal(error, "提交审批回调事务"))?;
            Ok(value)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 在**调用方事务内**按实例当前结论回写批次。
///
/// `cost_period_handler::withdraw_period` 用它把「终止实例」与「批次回草稿」
/// 放进**同一个事务**：撤回成功但回写失败会留下「实例 `WITHDRAWN`、批次仍 `SUBMITTED`」
/// 的死单 —— 那正是约定 11 禁止的形态（用户只能改库才能恢复）。
///
/// 结论从 `tx` 里**锁读**实例得出（同一事务自己写过的状态自己看得见，不会被别的连接读到旧值）。
pub async fn apply_decided_in_tx(tx: &Transaction, instance_id: Uuid) -> std::result::Result<Option<Uuid>, ApiError> {
    settle_in(tx, instance_id, None).await
}

/// [`settle`] / [`apply_decided_in_tx`] 的共同实现（事务由调用方给）。
async fn settle_in(
    tx: &Transaction,
    instance_id: Uuid,
    forced: Option<Verdict>,
) -> std::result::Result<Option<Uuid>, ApiError> {
    // ① 审批单必须是「成本结算」类的（其它业务的待办零影响）
    let Some(instance) = flow_model::lock_instance(tx, instance_id)
        .await
        .map_err(|error| internal(error, "锁读审批单"))?
    else {
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(period_id) = instance.biz_id else {
        return Ok(None);
    };
    let Some(row) = model::find_by_id(period_id)
        .await
        .map_err(|error| internal(error, "查询成本核算批次"))?
    else {
        tracing::warn!(%period_id, "成本核算批次不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    let verdict = match forced {
        Some(verdict) => verdict,
        None => match verdict_of(&instance.status) {
            Some(verdict) => verdict,
            // 实例还在跑：单据一个字节都不动
            None => return Ok(None),
        },
    };
    let to_status = match verdict {
        Verdict::Approve => STATUS_APPROVED,
        Verdict::BackToDraft => STATUS_DRAFT,
    };
    // ② 批次还在不在「等这个审批」的状态上（幂等重放 / 已被别的路径推走 → 不动）
    if !settle_guard::gate(
        &instance,
        state_machine::Entity::CostPeriod,
        &row.batch_no,
        &row.status,
        STATUS_SUBMITTED,
        to_status,
    )
    .await
    {
        return Ok(None);
    }
    let approver = latest_completed_assignee(tx, instance_id)
        .await
        .map_err(|error| internal(error, "查询审批人"))?;
    let comment = latest_reject_comment(tx, instance_id)
        .await
        .map_err(|error| internal(error, "查询审批意见"))?;
    let (reason, note) = match verdict {
        Verdict::Approve => ("成本结算审批通过（数据冻结，等待结算）".to_string(), None),
        Verdict::BackToDraft => {
            let (prefix, text) = match instance.status.as_str() {
                "WITHDRAWN" => (WITHDRAW_NOTE_PREFIX, "提交人撤回，审批终止".to_string()),
                _ => (
                    REJECT_NOTE_PREFIX,
                    comment
                        .as_deref()
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .unwrap_or("未填写意见")
                        .to_string(),
                ),
            };
            let reason = format!("{prefix}{text}");
            (reason.clone(), Some(reason))
        }
    };
    // ③ 状态机唯一入口（边表核对 + 写 biz_status_log，带审批单号）
    state_machine::transition(
        tx,
        state_machine::ENTITY_COST_PERIOD,
        row.id,
        &row.batch_no,
        &model::LEGAL_EDGES,
        STATUS_SUBMITTED,
        to_status,
        approver,
        Some(reason.as_str()),
        Some((BIZ_TYPE, instance.instance_no.as_str())),
    )
    .await?;
    // ④ CAS 回写：通过保留实例 id（审计），驳回 / 撤回清空审批痕迹 + 意见进 remark
    let affected = match verdict {
        Verdict::Approve => model::mark_approved(tx, row.id, approver).await,
        Verdict::BackToDraft => model::mark_back_to_draft(tx, row.id, note.as_deref(), approver).await,
    }
    .map_err(|error| internal(error, "回写批次状态"))?;
    if affected == 0 {
        return Err(cost_period_dto::conflict(format!(
            "成本核算批次 {} 的状态在审批回调期间被改动，请刷新后重试",
            row.batch_no
        )));
    }
    Ok(Some(row.id))
}

/// 最近一个已完成的审批人（写进状态日志的 `operator`）。
async fn latest_completed_assignee(ex: &impl SqlExecutor, instance_id: Uuid) -> DbResult<Option<Uuid>> {
    let rows = ex
        .query_sql(
            "SELECT assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status IN ('APPROVED','REJECTED') \
                AND assignee_id IS NOT NULL \
              ORDER BY completed_at DESC NULLS LAST LIMIT 1"
                .to_string(),
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("assignee_id").ok()),
    }
}

/// 最近一条驳回意见（拼进备注，审计据此知道为什么被打回）。
async fn latest_reject_comment(ex: &impl SqlExecutor, instance_id: Uuid) -> DbResult<Option<String>> {
    let rows = ex
        .query_sql(
            "SELECT comment FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'REJECTED' AND comment IS NOT NULL \
              ORDER BY completed_at DESC NULLS LAST LIMIT 1"
                .to_string(),
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("comment").ok()),
    }
}

// ==================== 内控：制单人不能批自己的结算 ====================

/// 内控用：这个批次的**制单人**（`biz_cost_period.create_by`）。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(period_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_cost_period WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&period_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("create_by").ok()),
    }
}

/// 审批入口的内控闸（**事务之前**调）：制单人 / 发起人不能批自己那一票 → 403 + 65019。
///
/// 与 `purchase_return_bridge::guard_self_approval` 同口径：流程节点写 `self_approval=ALLOW`
/// （待办可见、留痕），真正的否决在这里；与本业务无关的待办直接放行（零影响）。
/// 「制单与审批分离」开关关掉时（小团队）一律放行 —— 与 `internal_control` 读同一个键，
/// 不会出现「待办被 SKIP 了但闸说可以批」。
pub async fn guard_self_approval(task_id: Uuid, operator: Uuid) -> std::result::Result<(), ApiError> {
    let Some(task) = flow_model::find_task(task_id)
        .await
        .map_err(|error| internal(error, "查询待办"))?
    else {
        return Ok(());
    };
    let Some(instance) = flow_model::find_instance(task.instance_id)
        .await
        .map_err(|error| internal(error, "查询审批单"))?
    else {
        return Ok(());
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(());
    }
    if !separation::enabled(separation::Duty::Approve).await {
        return Ok(());
    }
    let Some(period_id) = instance.biz_id else {
        return Ok(());
    };
    let Ok(Some(row)) = model::find_by_id(period_id).await else {
        return Ok(());
    };
    if row.create_by == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己制单的成本核算批次：{} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理{}",
                row.batch_no,
                separation::approval_hint(separation::Duty::Approve)
            ),
        ));
    }
    Ok(())
}

/// `DbError` → 500（回调里的内部故障一律这个出口，与 `expense_bridge::internal` 同款）。
fn internal(error: DbError, what: &str) -> ApiError {
    ApiError::new(500, error_code::INTERNAL_ERROR, format!("{what}失败：{error}"))
}

// ==================== 单测（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// `V0381` 的种子（**已应用迁移**，只读不改）：flow_code / biz_type 的唯一来源。
    const FLOW_SEED: &str = include_str!("../../../migrations/business/V0381__cost_flow_def.up.sql");
    /// 审批中心的三处回调点 + 内控闸都在这里注册（接线漏一处 = 单据永久停在中间态）。
    const FLOW_HANDLER: &str = include_str!("../flow/handler.rs");
    /// 内控排除表（制单人不能批自己的单，否则财务经理本人制单 = 死单）。
    const INTERNAL_CONTROL: &str = include_str!("../flow/internal_control.rs");

    #[test]
    fn flow_constants_match_v0381_seed() {
        assert_eq!(BIZ_TYPE, "COST_SETTLE");
        assert_eq!(FLOW_CODE, "cost_settle_approve");
        assert!(
            FLOW_SEED.contains("'cost_settle_approve'"),
            "V0381 里必须有 cost_settle_approve 这条流程定义"
        );
        assert!(
            FLOW_SEED.contains("'COST_SETTLE'"),
            "V0381 的 biz_type 必须是 COST_SETTLE（审批回调靠它认领实例）"
        );
        // 两级审批的两个角色（本卡的自审闸 / 内控靠它们展开待办）
        for role in [
            "20000000-0000-4000-8000-0000000f0005",
            "20000000-0000-4000-8000-0000000f0006",
        ] {
            assert!(FLOW_SEED.contains(role), "V0381 缺审批角色 {role}");
        }
    }

    #[test]
    fn verdict_mapping_is_whitelist_only() {
        assert_eq!(verdict_of("APPROVED"), Some(Verdict::Approve));
        // 大小写 / 空白不敏感（引擎回调时状态串来自库，写死大写也留余地）
        assert_eq!(verdict_of(" approved "), Some(Verdict::Approve));
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED", "REJECTED_TERMINATED"] {
            assert_eq!(verdict_of(status), Some(Verdict::BackToDraft), "{status} 必须打回草稿");
        }
        // ⚠️ 中间态一律 None：两级流程批完第一站时实例还是 PENDING，
        //    打成 BackToDraft 会把刚批完财务经理的在途批次打回草稿。
        for status in ["PENDING", "RUNNING", "SKIPPED", ""] {
            assert_eq!(verdict_of(status), None, "{status} 不该动批次");
        }
    }

    #[test]
    fn verdict_labels_are_stable() {
        assert_eq!(Verdict::Approve.as_str(), "APPROVE");
        assert_eq!(Verdict::BackToDraft.as_str(), "BACK_TO_DRAFT");
    }

    #[test]
    fn note_prefixes_are_auditable() {
        // 备注前缀是审计能搜到的关键词（巡检 / 客服按它找「这单为什么被打回」）
        assert!(REJECT_NOTE_PREFIX.contains("驳回"));
        assert!(WITHDRAW_NOTE_PREFIX.contains("撤回"));
    }

    #[test]
    fn wired_into_flow_handler() {
        // 三个终态出口（同意 / 驳回 / 撤回）+ 终止 + 自审闸，少一个就有单据卡中间态
        let calls = FLOW_HANDLER
            .matches("cost_period_bridge::on_cost_settle_decided")
            .count();
        assert!(
            calls >= 4,
            "flow/handler.rs 里成本结算回调只出现 {calls} 次（同意 / 驳回 / 撤回 / 终止各一处，至少 4）"
        );
        assert!(
            FLOW_HANDLER.contains("cost_period_bridge::guard_self_approval"),
            "flow/handler.rs 必须调 guard_self_approval（制单人不能批自己的结算）"
        );
    }

    #[test]
    fn wired_into_internal_control() {
        assert!(
            INTERNAL_CONTROL.contains("cost_period_bridge::internal_control_creator"),
            "flow/internal_control.rs 必须排除制单人（否则财务经理本人制单 = 看得见点不动）"
        );
    }
}
