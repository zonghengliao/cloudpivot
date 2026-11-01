//! 凭证红冲审批的**业务回调 + 内控闸 + 反向兜底**（P-FLOW-3B · 财务链第二环）。
//!
//! 与 `payable_bridge`（付款 / 应付作废）、`stockin_bridge`（收货过账 / 作废）、
//! `purchase_bridge`（采购订单）并列的又一类业务回调：审批单落终态之后，
//! 把结论翻译成**凭证红冲**的动作。彼此互不影响（按 `biz_type` 分流）。
//!
//! # 三个入口
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_reverse_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance}`（**审批事务已提交之后**） | `APPROVED` → 生成红字凭证 + 回写 `reversed_by` + 清 pending；其余终态 → 只清 pending，账一分不动 |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**进事务之前**） | 内控：审批人是这张凭证的**制单人**（或发起人）→ `403`「不能审批自己提交的红冲申请」 |
//! | [`cancel_approval_of`] | 凭证被别处红冲 / 作废联动等「其它路径」 | 把在跑的审批**终止**（实例 / 节点 / 待办一起 `CANCELLED`），清 pending |
//!
//! # 为什么「状态机表达不了」（卡面硬红线 3）
//!
//! 红冲的语义是「生成红字凭证，**原凭证保持 `POSTED`**」，所以审批中**不改** `biz_voucher.status`：
//! 四大报表 / 总账 / 余额表 / 利润表 / 现金流量表的取数全是
//! `WHERE v.status = 'POSTED' AND v.deleted_at IS NULL`。真把原凭证改成 `PENDING_REVERSE`，
//! 这张**已入账**的凭证会在所有报表里瞬间消失 —— 审批流程不该改变账务的存在性。
//!
//! 于是「审批中」用库列 `pending_reverse_instance_id` 表达（有值 = 正在审批红冲），
//! 由 `ck_biz_voucher_pending_reverse` 保证「pending 只可能挂在 `POSTED` 且未红冲的凭证上」。
//! 审批通过后：清 pending → 生成红字（`voucher::reverse_posted`）→ 回写 `reversed_by`。
//!
//! # 为什么要反向（P11 的教训）
//!
//! 单据被删 / 被别的路径红冲时没有撤审批单 → 审批中心留下孤儿待办，点进去 404。
//! 凭证这边同构的风险更大：孤儿 pending 会让这张凭证**再也提交不了红冲**
//! （入口 409「已在红冲审批中」且无法自愈）。所以三条反向路径都做了：
//!
//! ```text
//! · 凭证被联动红冲（收货单作废 → reverse_for_stock_in）→ cancel_approval_of（同一事务）
//! · 凭证被软删 / pending 被别处清掉                → reconcile_orphan_reverses（每 10 分钟）
//! · 发起人自己不想冲了                              → POST /voucher/{id}/withdraw-reverse
//! ```
//!
//! # 时序与失败口径（与 `payable_bridge` 同款）
//!
//! 回调跑在**审批事务提交之后**：审批结论已经落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None`。定时扫（[`reconcile_orphan_reverses`]）按「凭证实际状态」
//! 重算，天然幂等。
//!
//! # 与 P7 的关系（硬红线 5：复用，不重写）
//!
//! 红冲一行都不重写：直接调
//! [`crate::business::erp::voucher_write::reverse_after_approval`]，
//! 它内部就是 P7 的 `voucher::reverse_posted`（借贷互换 + 独立号段 PZR + 回写 `reversed_by`）。
//! 本模块只负责「什么时候该调它」。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::{engine, model as flow_model};
use crate::shared::error_code;

/// 红冲审批的业务类型（与 V0191 种子 / `voucher_write::REVERSE_BIZ_TYPE` 同口径）。
pub const BIZ_TYPE: &str = "VOUCHER_REVERSE";
/// 红冲审批的流程编码。
pub const FLOW_CODE: &str = "voucher_reverse";
/// 孤儿实例回收的定时任务名（`sys_job.invoke_target` 与调度名同值）。
pub const TASK_NAME: &str = "voucher_reverse_reconcile";
/// cron（每 10 分钟，与收货 3-59 / 付款 7-59 / 应付作废 9-59 错开）。
pub const CRON: &str = "5-59/10 * * * *";

/// 一条「已发布的凭证红冲审批流程」（发起审批要用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoucherFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`VOUCHER_REVERSE`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的凭证红冲审批流程」。
///
/// `None` = **没配流程**（不是错误）：调用方按 P-FLOW-3B 之前的行为**直接红冲**
/// 并打一条 `warn`，绝不因为没配流程就把红冲锁死。
///
/// 排序口径与 `payable_bridge::published_pay_flow` / `stockin_model::published_post_flow`
/// 完全一致（`biz_type` 精确命中优先 → `is_default` → 版本号 → 更新时间）。
pub async fn published_reverse_flow() -> DbResult<Option<VoucherFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的单据该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'VOUCHER_REVERSE' OR lower(d.flow_code) = 'voucher_reverse') \
              ORDER BY (d.biz_type = 'VOUCHER_REVERSE') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(VoucherFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

// ==================== 回调：审批结论 → 凭证 ====================

/// 红冲审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReverseVerdict {
    /// 同意 → 生成红字凭证（原凭证保持 `POSTED`）。
    Reverse,
    /// 驳回 / 撤回 / 终止 → 只清 pending，账一分没动。
    Restore,
}

impl ReverseVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            ReverseVerdict::Reverse => "REVERSE",
            ReverseVerdict::Restore => "RESTORE",
        }
    }
}

/// 实例状态 → 红冲审批的业务语义。`None` = 还在跑 / 不认识（一律不动凭证）。
pub fn reverse_verdict(instance_status: &str) -> Option<ReverseVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(ReverseVerdict::Reverse),
        // 驳回、发起人撤回、被系统终止 → 都是「红冲没成」
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(ReverseVerdict::Restore),
        _ => None,
    }
}

/// 审批单落终态 → 执行 / 放弃凭证红冲（`FlowCallback` 身份）。
///
/// | 实例状态 | 原凭证 | 账 |
/// |---|---|---|
/// | `APPROVED` | `status` 仍 `POSTED`、`reversed_by` 指向新红字 | 生成红字凭证（借贷互换） |
/// | `REJECTED` / `WITHDRAWN` / `CANCELLED` | 不变，只清 pending | 一分没动 |
/// | 其它（`PENDING` / 空） | 不动 | 不动 |
///
/// 返回受影响的凭证 id（没推进 / 不该推进 → `None`）。**不返回错误**：审批已落库，
/// 业务侧失败只记日志（与 `payable_bridge::on_cancel_settled` 同口径）。
pub async fn on_reverse_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = reverse_verdict(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(voucher_id)) => {
            tracing::info!(
                %instance_id,
                %voucher_id,
                verdict = verdict.as_str(),
                "凭证红冲审批结论已回调凭证"
            );
            Some(voucher_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "凭证红冲审批结论回调失败（不影响审批单，等待 reconcile 兜扫）"
            );
            None
        }
    }
}

/// [`on_reverse_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: ReverseVerdict) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是「凭证红冲」类的（别的业务类型与本回调无关）
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过凭证红冲回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(voucher_id) = instance.biz_id else {
        return Ok(None);
    };
    // ② 凭证必须还挂着**这一条**审批单（pending 已被别处清掉 / 换了实例 → 不归本回调管）
    let Some(row) = crate::business::erp::voucher::find_voucher(db(), voucher_id).await? else {
        tracing::warn!(%voucher_id, "凭证不存在或已删除，跳过红冲回调");
        return Ok(None);
    };
    match row.pending_reverse_instance_id {
        Some(pending) if pending == instance_id => {}
        Some(other) => {
            tracing::warn!(
                %voucher_id,
                instance_id = %instance_id,
                pending = %other,
                "凭证挂的是另一条红冲审批，跳过（数据异常请核查）"
            );
            return Ok(None);
        }
        None => {
            tracing::debug!(
                %voucher_id,
                verdict = verdict.as_str(),
                "凭证没有在跑的红冲审批（重复回调 / 已处理），跳过"
            );
            return Ok(None);
        }
    }
    // ③ 审批人（落 `update_by` / 红字凭证的 `posted_by`）：引擎把「谁点的」写在待办行
    let approver = latest_completed_assignee(instance_id).await?;
    let creator = internal_control_creator(Some(voucher_id)).await?;
    match verdict {
        ReverseVerdict::Reverse => {
            // 内控最后一道闸：**复核人不能是制单人**（节点 self_approval 已拦 + 生成待办前
            // 已排除，这里再核一次：真发生了就**不许记账**，留 WARN 让人看）
            if let (Some(approver), Some(creator)) = (approver, creator)
                && approver == creator
            {
                tracing::warn!(
                    %voucher_id,
                    instance_id = %instance_id,
                    approver = %approver,
                    "内控拦截：红冲复核人就是制单人本人，红字凭证**未**生成（请核查流程配置）"
                );
                return Ok(None);
            }
            match crate::business::erp::voucher_write::reverse_after_approval(voucher_id, approver)
                .await
            {
                Ok(red_id) => {
                    tracing::info!(%voucher_id, %red_id, "红冲审批已通过：红字凭证已生成");
                    Ok(Some(voucher_id))
                }
                Err(error) => {
                    tracing::warn!(
                        %voucher_id,
                        code = error.code,
                        msg = %error.msg,
                        "红冲审批已通过，但红字凭证生成失败（pending 未清，等待人工处置）"
                    );
                    Ok(None)
                }
            }
        }
        ReverseVerdict::Restore => {
            match crate::business::erp::voucher_write::restore_after_reverse_rejection(
                voucher_id, verdict, approver,
            )
            .await
            {
                Ok(()) => Ok(Some(voucher_id)),
                Err(error) => {
                    tracing::warn!(
                        %voucher_id,
                        code = error.code,
                        msg = %error.msg,
                        "红冲审批未通过，但凭证 pending 清理失败（等待 reconcile 兜扫）"
                    );
                    Ok(None)
                }
            }
        }
    }
}

// ==================== 内控：制单人不能批自己的红冲 ====================

/// 内控用：这张凭证的**制单人**（`biz_voucher.create_by`）。
///
/// 供 `flow::internal_control` 在**生成待办前**排除制单人（落 `SKIPPED` 留痕）。
/// 查不到凭证 → `None`（没人被排除，不锁流程）。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(voucher_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪（裁剪会让
    // 「看不到这张单的人」反而成为可审批人，与内控目的相反）。
    let row = db()
        .query_first_sql(
            "SELECT create_by FROM biz_voucher WHERE id = ?::uuid".to_string(),
            vec![Value::from(&voucher_id)],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let creator: Option<Uuid> = row.get_typed("create_by")?;
    Ok(creator)
}

/// 审批入口的内控闸（**事务之前**调）：红冲审批的制单人 / 发起人不能批自己那一票 → 403。
///
/// 判据只认 `VOUCHER_REVERSE` 类的实例，与其它业务的待办零影响。
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
    if instance.biz_type.trim().to_ascii_uppercase() != BIZ_TYPE {
        return Ok(());
    }
    let Some(voucher_id) = instance.biz_id else {
        return Ok(());
    };
    let creator = internal_control_creator(Some(voucher_id))
        .await
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("查询凭证制单人失败：{error}"),
            )
        })?;
    let Ok(Some(row)) = crate::business::erp::voucher::find_voucher(db(), voucher_id).await else {
        return Ok(());
    };
    if creator == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己提交的红冲申请：凭证 {} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理",
                row.voucher_no
            ),
        ));
    }
    Ok(())
}

// ==================== 反向：终止在跑的红冲审批 ====================

/// 终止某张凭证**在跑的红冲审批**（凭证被别处红冲 / 软删时用，与调用方同一事务）。
///
/// 幂等：没有在跑的实例返回 `Ok(None)`；返回 `Some(实例数)` 才表示真的终止了。
/// 同时清 `pending_reverse_instance_id`（不清就永远 409「已在红冲审批中」）。
pub async fn cancel_approval_of(
    ex: &impl SqlExecutor,
    voucher_id: Uuid,
    voucher_no: &str,
    reason: &str,
) -> DbResult<Option<u64>> {
    let cancelled = flow_model::cancel_pending_of_biz(ex, voucher_id, voucher_no, reason).await?;
    ex.exec_sql(
        "UPDATE biz_voucher SET pending_reverse_instance_id = NULL, updated_at = now() \
          WHERE id = ?::uuid AND pending_reverse_instance_id IS NOT NULL"
            .to_string(),
        vec![Value::from(&voucher_id)],
    )
    .await?;
    if cancelled.instances > 0 {
        tracing::info!(
            %voucher_id,
            voucher_no = %voucher_no,
            instances = cancelled.instances,
            tasks = cancelled.tasks,
            reason = reason,
            "凭证在跑的红冲审批已终止（账没动）"
        );
        Ok(Some(cancelled.instances))
    } else {
        Ok(None)
    }
}

/// 扫描并终止**孤儿红冲审批实例**（P11 教训的机器化兜底）。
///
/// 判据（三条同时成立才算孤儿）：
/// 1. 实例 `biz_type = 'VOUCHER_REVERSE'` 且 `status = 'PENDING'`（还在跑）；
/// 2. 凭证**已软删**，或 `pending_reverse_instance_id` 已不是这个实例（被别处清掉 / 改过）；
/// 3. `biz_id` 可解析（为空说明实例本身就没挂业务，交给上游维护）。
///
/// 返回终止的实例数。定时任务每 10 分钟跑一次；单测 / 运维脚本也可直接调。
pub async fn reconcile_orphan_reverses() -> DbResult<u64> {
    let rows = db()
        .find_with_params(
            "SELECT i.id AS instance_id, i.biz_id AS voucher_id, i.biz_no AS biz_no, \
                    v.voucher_no AS voucher_no, v.deleted_at AS voucher_deleted_at \
               FROM biz_flow_instance i \
               LEFT JOIN biz_voucher v ON v.id = i.biz_id \
              WHERE i.biz_type = ? AND i.status = 'PENDING' AND i.deleted_at IS NULL \
                AND i.biz_id IS NOT NULL \
                AND (v.id IS NULL OR v.deleted_at IS NOT NULL \
                     OR v.pending_reverse_instance_id IS DISTINCT FROM i.id) \
              ORDER BY i.started_at ASC \
              LIMIT 200",
            vec![Value::Text(BIZ_TYPE.to_string())],
        )
        .await?;

    let mut cancelled = 0u64;
    for row in rows {
        let instance_id: Uuid = row.get_typed("instance_id")?;
        let voucher_id: Uuid = row.get_typed("voucher_id")?;
        let voucher_no: Option<String> = row.get_typed("voucher_no")?;
        let voucher_no = voucher_no
            .or_else(|| row.get_typed("biz_no").ok().flatten())
            .unwrap_or_else(|| "(无单号)".to_string());
        let deleted: Option<DateTime<Utc>> =
            row.get_typed("voucher_deleted_at").ok().flatten();
        let reason = if deleted.is_some() {
            "凭证已删除，红冲审批自动终止"
        } else {
            "凭证已不处于红冲审批中，红冲审批自动终止"
        };
        match cancel_approval_of(db(), voucher_id, &voucher_no, reason).await {
            Ok(Some(_)) => cancelled += 1,
            Ok(None) => tracing::debug!(%instance_id, "孤儿实例判定后被别处处理，跳过"),
            Err(error) => tracing::warn!(%instance_id, %error, "终止孤儿红冲审批失败（下轮重试）"),
        }
    }
    if cancelled > 0 {
        tracing::info!(cancelled, "孤儿凭证红冲审批实例回收完成");
    }
    Ok(cancelled)
}

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

/// 在调度器上注册孤儿实例回收任务（与 `stockin_bridge::register_builtin` 同款）。
pub fn register_builtin(scheduler: &crate::task::Scheduler) {
    scheduler.register_cron(
        TASK_NAME,
        CRON,
        "系统任务：凭证红冲审批孤儿实例回收",
        || {
            tokio::spawn(async move {
                match reconcile_orphan_reverses().await {
                    Ok(0) => {}
                    Ok(count) => tracing::info!(count, "凭证红冲审批孤儿实例回收完成"),
                    Err(error) => tracing::error!(%error, "凭证红冲审批孤儿实例回收失败"),
                }
            })
        },
    );
}

/// 让引擎的 `ActionOut` 能直接喂给 [`on_reverse_settled`]（只取「落终态」这一层语义）。
pub async fn on_action_settled(out: &engine::ActionOut) -> Option<Uuid> {
    on_reverse_settled(out.instance_id, &out.instance_status).await
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// ① 实例状态 → 业务语义的映射口径（钉死：只有 APPROVED 才记账）。
    #[test]
    fn verdict_maps_only_final_states() {
        assert_eq!(reverse_verdict("APPROVED"), Some(ReverseVerdict::Reverse));
        assert_eq!(
            reverse_verdict(" approved "),
            Some(ReverseVerdict::Reverse)
        );
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
            assert_eq!(
                reverse_verdict(status),
                Some(ReverseVerdict::Restore),
                "{status} 必须只清 pending（账不动）"
            );
        }
        for status in ["PENDING", "", "APPROVED2", "POSTED"] {
            assert_eq!(reverse_verdict(status), None, "{status} 不该动凭证");
        }
    }

    /// ② 业务类型 / 流程编码必须与 V0191 种子同口径（错一个字就查不到流程）。
    #[test]
    fn biz_type_and_flow_code_match_seed() {
        assert_eq!(BIZ_TYPE, "VOUCHER_REVERSE");
        assert_eq!(FLOW_CODE, "voucher_reverse");
        assert_eq!(BIZ_TYPE, BIZ_TYPE.to_uppercase());
        assert_eq!(
            BIZ_TYPE,
            crate::business::erp::voucher_write::REVERSE_BIZ_TYPE
        );
        assert_eq!(
            FLOW_CODE,
            crate::business::erp::voucher_write::REVERSE_FLOW_CODE
        );
    }

    /// ③ 凭证状态常量不能写错；「审批中」**不是**一个状态（卡面硬红线 3）。
    #[test]
    fn voucher_status_is_never_pending_reverse() {
        assert_eq!(crate::business::erp::voucher::STATUS_POSTED, "POSTED");
        // 库里 ck_biz_voucher_status 只有 4 个值，没有 PENDING_REVERSE：
        // 报表取数只认 POSTED，加一个「审批中」状态会让已入账凭证从报表消失。
        assert!(
            !crate::business::shared::state_machine::VOUCHER_EDGES
                .iter()
                .any(|(_, to)| *to == "PENDING_REVERSE"),
            "凭证状态机里不许出现 PENDING_REVERSE：红冲用 pending_reverse_instance_id 表达"
        );
    }
}
