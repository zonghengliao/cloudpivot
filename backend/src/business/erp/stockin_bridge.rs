//! 收货单作废审批的**业务回调 + 反向兜底**（批次 2 · P16）。
//!
//! 与 `flow::task_bridge` / `flow::task_bridge::on_transfer_settled` 完全并列的第三类业务回调：
//! 审批单落终态之后，把结论翻译成收货单的状态与六本账动作。**两者互不影响**。
//!
//! # 三个入口
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_cancel_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance}`（**审批事务已提交之后**） | `APPROVED` → 执行真作废（六本账回滚）；`REJECTED` / `WITHDRAWN` / `CANCELLED` → 单据回到 `POSTED` |
//! | [`cancel_approval_of`] | 收货单删除等「其它路径」 | 把在跑的作废审批**终止**（`biz_flow_task` / 节点 / 主表一起翻 `CANCELLED`） |
//! | [`reconcile_orphan_cancels`] | 定时任务 `stockin_cancel_reconcile`（每 10 分钟） | 扫描「审批还在跑、单据却已不是作废审批中」的**孤儿实例**，逐个终止 |
//!
//! # 为什么必须有第三条（P11 的教训）
//!
//! P11 实测：任务被取消时没有撤审批单 → 审批中心留下 8 条孤儿待办，点进去 404。
//! 收货单这边同构的风险更大 —— 孤儿实例不仅留待办，还会在 `uk_biz_flow_instance_biz_active`
//! 上把**下一次**发起作废永久堵死（`409` 且无法自愈）。所以本模块不只做「审批 → 单据」的正向回调，
//! 还做「单据 → 审批」的反向终止：
//!
//! ```text
//! · 收货单被删（软删）              → cancel_approval_of（delete_stock_in 同一事务里调）
//! · 单据被别的路径改掉状态          → on_cancel_settled 的 POSTED 分支 + reconcile 兜扫
//! · 同一单重复发起                  → 发起前查 biz_instance_running → 409
//! ```
//!
//! # 时序与失败口径（与 `on_transfer_settled` 同款）
//!
//! 回调跑在**审批事务提交之后**：审批结论已经落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None`。下一次 `reconcile_orphan_cancels` 会把状态补齐
//! （它按「单据实际状态」重算，天然幂等）。
//!
//! # 与 P9 的关系（硬红线 5）
//!
//! 六本账回滚**一行都不重写**：直接调 [`stockin_handler::cancel_in_tx`]，
//! 它内部复用 P9 的 `cancel_plan_of` / `reverse_cost` / `reverse_location_qty` /
//! `write_cancel_txn` / `void_serials_of`。本模块只负责「什么时候该调它」。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::stockin_handler;
use crate::business::erp::stockin_model::{self, STATUS_PENDING_CANCEL};
use crate::business::flow::{engine, model as flow_model};

/// 作废审批的业务类型（与 V0174 种子 / `stockin_model::published_cancel_flow` 同口径）。
pub const BIZ_TYPE: &str = stockin_model::CANCEL_BIZ_TYPE;

/// 定时任务名（`sys_job.invoke_target` 与调度名同值）。
pub const TASK_NAME: &str = "stockin_cancel_reconcile";
/// 定时任务 cron（每 10 分钟，与审批超时扫描错开 3 分钟）。
pub const CRON: &str = "3-59/10 * * * *";

/// 审批单落终态 → 执行 / 撤销收货单作废（`FlowCallback` 身份）。
///
/// | 实例状态 | 收货单 | 账 |
/// |---|---|---|
/// | `APPROVED` | `PENDING_CANCEL → CANCELLED` | **六本账全部回滚**（P9 逻辑） |
/// | `REJECTED` / `WITHDRAWN` / `CANCELLED` | `PENDING_CANCEL → POSTED` | 一动没动 |
/// | 其它（`PENDING` / 空） | 不动 | 不动 |
///
/// 返回受影响的收货单 id（没推进 / 不该推进 → `None`）。**不返回错误**：审批已落库，
/// 业务侧失败只记日志（与 `task_bridge::on_transfer_settled` 同口径）。
pub async fn on_cancel_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = settle_verdict(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(stock_in_id)) => {
            tracing::info!(
                %instance_id,
                %stock_in_id,
                verdict = verdict.as_str(),
                "作废审批结论已回调收货单"
            );
            Some(stock_in_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "作废审批结论回调收货单失败（不影响审批单，等待 reconcile 兜扫）"
            );
            None
        }
    }
}

/// 审批结论的**业务语义**（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 同意 → 真作废，执行六本账回滚。
    Cancel,
    /// 不同意 / 撤回 / 终止 → 打回已过账，账不动。
    Restore,
}

impl Verdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Verdict::Cancel => "CANCEL",
            Verdict::Restore => "RESTORE",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动单据）。
pub fn settle_verdict(instance_status: &str) -> Option<Verdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(Verdict::Cancel),
        // 驳回、发起人撤回、被系统终止（单据删除 / 其它路径）→ 都是「作废没成」
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(Verdict::Restore),
        _ => None,
    }
}

/// [`on_cancel_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: Verdict) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是「收货单作废」类的（别的业务类型与本回调无关）
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过收货单作废回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(stock_in_id) = instance.biz_id else {
        return Ok(None);
    };

    // ② 单据必须是「作废审批中」，否则就是不归本回调管的重复回调 / 已被别处推进
    let Some(row) = stockin_model::find_stock_in(stock_in_id).await? else {
        tracing::warn!(%stock_in_id, "收货单不存在或已删除，跳过作废回调");
        return Ok(None);
    };
    if row.status != STATUS_PENDING_CANCEL {
        tracing::debug!(
            %stock_in_id,
            status = %row.status,
            verdict = verdict.as_str(),
            "收货单不在 PENDING_CANCEL（重复回调 / 已撤回 / 已作废），跳过"
        );
        return Ok(None);
    }

    // ③ 审批人（`cancelled_by` 落库用）：引擎把「谁点的」写在待办行，实例行没有该字段
    let approver = latest_completed_assignee(instance_id).await?;

    match verdict {
        Verdict::Cancel => {
            // 内控最后一道闸：**复核人不能是制单人**。
            // 正常路径上流程节点 `self_approval=FORBID` 已经拦住了自审，
            // 这里再核一次是因为「制单人 ≠ 发起人」的场景（别人代提，制单人自己批）
            // 引擎的 FORBID 管不到 —— 真发生了就**不许动账**，留 WARN 让人看。
            if let (Some(approver), Some(creator)) = (approver, row.create_by)
                && approver == creator
            {
                tracing::warn!(
                    %stock_in_id,
                    instance_id = %instance_id,
                    approver = %approver,
                    "内控拦截：作废复核人就是制单人本人，六本账**未**回滚（self_approval=FORBID 应已拦住，请核查流程配置）"
                );
                return Ok(None);
            }
            // 真作废：复用 P9 的 cancel_in_tx（六本账回滚），一个事务里做完
            let reason = row
                .cancel_reason
                .clone()
                .unwrap_or_else(|| "作废审批通过".to_string());
            let operator = approver.or(row.create_by);
            let outcome =
                stockin_handler::cancel_after_approval(stock_in_id, &row, &reason, operator).await;
            match outcome {
                Ok(()) => Ok(Some(stock_in_id)),
                Err(error) => {
                    tracing::warn!(
                        %stock_in_id,
                        code = error.code,
                        msg = %error.msg,
                        "作废审批已通过，但六本账回滚失败（单据仍是作废审批中，等待人工处置）"
                    );
                    Ok(None)
                }
            }
        }
        Verdict::Restore => {
            // 驳回 / 撤回 / 终止：单据回到已过账，账一动没动
            let outcome = stockin_handler::restore_after_cancel_approval(
                stock_in_id,
                &row,
                verdict,
                approver,
            )
            .await;
            match outcome {
                Ok(()) => Ok(Some(stock_in_id)),
                Err(error) => {
                    tracing::warn!(
                        %stock_in_id,
                        code = error.code,
                        msg = %error.msg,
                        "作废审批未通过，但收货单打回失败（等待 reconcile 兜扫）"
                    );
                    Ok(None)
                }
            }
        }
    }
}

/// 终止某张收货单的**在跑作废审批**（单据删除 / 其它路径改状态时用）。
///
/// 幂等：没有在跑的实例返回 `Ok(None)`；返回 `Some(实例数)` 才表示真的终止了。
pub async fn cancel_approval_of(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
    in_no: &str,
    reason: &str,
) -> DbResult<Option<u64>> {
    let cancelled = flow_model::cancel_pending_of_biz(ex, stock_in_id, in_no, reason).await?;
    if cancelled.instances > 0 {
        tracing::info!(
            %stock_in_id,
            in_no = %in_no,
            instances = cancelled.instances,
            tasks = cancelled.tasks,
            reason = reason,
            "收货单在跑的作废审批已终止（不再留孤儿待办）"
        );
        Ok(Some(cancelled.instances))
    } else {
        Ok(None)
    }
}

/// 扫描并终止**孤儿作废审批实例**（P11 教训的机器化兜底）。
///
/// 判据（三条同时成立才算孤儿）：
/// 1. 实例 `biz_type = 'STOCK_IN_CANCEL'` 且 `status = 'PENDING'`（还在跑）；
/// 2. 单据**已软删**，或单据状态已**不是** `PENDING_CANCEL`；
/// 3. 单据 id 可解析（`biz_id` 为空说明实例本身就没挂业务，交给上游维护）。
///
/// 返回终止的实例数。定时任务每 10 分钟跑一次；单测 / 运维脚本也可直接调。
pub async fn reconcile_orphan_cancels() -> DbResult<u64> {
    let rows = db()
        .find_with_params(
            "SELECT i.id AS instance_id, i.biz_id AS stock_in_id, \
                    i.biz_no AS biz_no, s.status AS stock_status, s.deleted_at AS stock_deleted_at \
               FROM biz_flow_instance i \
               LEFT JOIN biz_stock_in s ON s.id = i.biz_id \
              WHERE i.biz_type = ? AND i.status = 'PENDING' AND i.deleted_at IS NULL \
                AND i.biz_id IS NOT NULL \
                AND (s.id IS NULL OR s.deleted_at IS NOT NULL OR s.status <> ?) \
              ORDER BY i.started_at ASC \
              LIMIT 200",
            vec![
                Value::Text(BIZ_TYPE.to_string()),
                Value::Text(STATUS_PENDING_CANCEL.to_string()),
            ],
        )
        .await?;

    let mut cancelled = 0u64;
    for row in rows {
        let instance_id: Uuid = row.get_typed("instance_id")?;
        let stock_in_id: Uuid = row.get_typed("stock_in_id")?;
        let in_no: Option<String> = row.get_typed("biz_no")?;
        let in_no = in_no.unwrap_or_else(|| "(无单号)".to_string());
        let reason = if row
            .get_typed::<Option<DateTime<Utc>>>("stock_deleted_at")?
            .is_some()
        {
            "收货单已删除，作废审批自动终止"
        } else {
            "收货单已不处于作废审批中，作废审批自动终止"
        };
        // 单条失败不影响其它实例（下一轮还会再扫到）
        match cancel_approval_of(db(), stock_in_id, &in_no, reason).await {
            Ok(Some(_)) => cancelled += 1,
            Ok(None) => {
                tracing::debug!(%instance_id, "孤儿实例判定后被别处处理，跳过");
            }
            Err(error) => {
                tracing::warn!(%instance_id, %error, "终止孤儿作废审批失败（下轮重试）");
            }
        }
    }
    if cancelled > 0 {
        tracing::info!(cancelled, "孤儿作废审批实例回收完成");
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

/// 在调度器上注册孤儿实例回收任务（与 `transfer_overdue::register_builtin` 同款）。
pub fn register_builtin(scheduler: &crate::task::Scheduler) {
    scheduler.register_cron(
        TASK_NAME,
        CRON,
        "系统任务：收货作废审批孤儿实例回收",
        || {
            tokio::spawn(async move {
                match reconcile_orphan_cancels().await {
                    Ok(0) => {}
                    Ok(count) => tracing::info!(count, "收货作废审批孤儿实例回收完成"),
                    Err(error) => tracing::error!(%error, "收货作废审批孤儿实例回收失败"),
                }
            })
        },
    );
}

/// 让引擎的 `ActionOut` 能直接喂给 [`on_cancel_settled`]（只取「落终态」这一层语义）。
///
/// 审批处理器拿到的是 `ActionOut { instance_id, instance_status }`，
/// 这层薄封装只是让调用点更短；逻辑仍在 [`on_cancel_settled`]。
pub async fn on_action_settled(out: &engine::ActionOut) -> Option<Uuid> {
    on_cancel_settled(out.instance_id, &out.instance_status).await
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::stockin_model::{STATUS_CANCELLED, STATUS_POSTED};

    /// ⑮ 实例状态 → 业务语义的映射口径（钉死：只有 APPROVED 能动账）。
    #[test]
    fn verdict_maps_only_final_states() {
        assert_eq!(settle_verdict("APPROVED"), Some(Verdict::Cancel));
        assert_eq!(settle_verdict(" approved "), Some(Verdict::Cancel));
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
            assert_eq!(
                settle_verdict(status),
                Some(Verdict::Restore),
                "{status} 必须打回已过账（账不动）"
            );
        }
        // 还在跑 / 空 / 不认识 → 不动单据
        for status in ["PENDING", "", "APPROVED2", "DRAFT"] {
            assert_eq!(settle_verdict(status), None, "{status} 不该推进收货单");
        }
    }

    /// ⑯ 业务类型与流程编码必须与种子迁移同口径（全大写下划线）。
    #[test]
    fn biz_type_matches_seed() {
        assert_eq!(BIZ_TYPE, "STOCK_IN_CANCEL");
        assert_eq!(stockin_model::CANCEL_BIZ_TYPE, BIZ_TYPE);
        assert_eq!(stockin_model::CANCEL_FLOW_CODE, "stock_in_cancel");
        assert_eq!(BIZ_TYPE, BIZ_TYPE.to_uppercase());
    }

    /// ⑰ 状态常量不能写错（错一个字就是库层 23514）。
    #[test]
    fn status_constants_are_exact() {
        assert_eq!(STATUS_PENDING_CANCEL, "PENDING_CANCEL");
        assert_eq!(STATUS_POSTED, "POSTED");
        assert_eq!(STATUS_CANCELLED, "CANCELLED");
    }
}
