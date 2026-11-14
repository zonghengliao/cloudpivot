//! 审批落终态 → 推进**库存调拨单**（P2-34 从 `flow::task_bridge` 搬来）。
//!
//! # 为什么它属于 ERP
//!
//! 这段逻辑写 `biz_transfer` 的 6 态、跑调拨的联动规则（`transfer_linkage`）—— 全是 ERP 的业务。
//! 它原来住在 `flow::task_bridge` 里，只因为「审批回调」这个身份；代价是 `flow` 必须
//! `use crate::business::erp::transfer::{transfer_linkage, transfer_model}`，那条边正是 P2-B 要摘的环。
//!
//! # 现在谁调它
//!
//! 它实现了 [`DocReader::on_settled`](crate::business::flow::doc_reader::DocReader::on_settled)
//! （`biz_type = STOCK_TRANSFER`，注册在 `erp::doc_reader_impl`），由
//! `flow::doc_reader::dispatch_settled` 在审批事务提交后扇出调起 —— 与 ERP 另外 16 条审批线同一入口。
//!
//! # 时序与失败口径（**与搬到 `flow` 之前逐字一致**）
//!
//! - 审批结论已落库，业务侧失败只记 `warn`、绝不回滚审批单；
//! - `APPROVED` → 调拨单 `PENDING → APPROVED`；`REJECTED` → `PENDING → DRAFT`（打回）；
//!   其它实例状态（`PENDING` / `CANCELLED` / `WITHDRAWN`）不动；
//! - 写目标状态前先问状态机（`flow::settle_guard::gate`，`PENDING → APPROVED|DRAFT` 的边），
//!   不在路径上 → 不落状态 + `WARN` + 审批实例留痕；
//! - 幂等：重复回调时调拨单已不在 `PENDING` → `WHERE status = 'PENDING'` 命中 0 行 → 返回 `None`。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::linkage;
use crate::business::erp::transfer::{transfer_linkage, transfer_model as erp_transfer};
use crate::business::flow::{model, settle_guard};

/// 审批实例的两个终态（与 `flow::ENGINE` 的落库原值同口径）。
const INSTANCE_APPROVED: &str = "APPROVED";
const INSTANCE_REJECTED: &str = "REJECTED";

// ==================== 第二类业务单据：库存调拨单（批次 D · W3b） ====================

/// 库存调拨单的业务类型（`biz_flow_def.biz_type`，与
/// [`crate::business::erp::transfer::transfer_model::published_transfer_flow`] 的查询条件同口径）。
pub const STOCK_TRANSFER_BIZ_TYPE: &str = "STOCK_TRANSFER";

/// 审批单落终态后推进**库存调拨单**（`FlowCallback` 身份）。
///
/// 与任务那条（[`on_instance_settled`]）完全并列的第二类业务回调，**两者互不影响**：
///
/// | 实例状态 | 调拨单 | 依据 |
/// |---|---|---|
/// | `APPROVED` | `PENDING → APPROVED` | 审批通过 = 可以发货 |
/// | `REJECTED` | `PENDING → DRAFT`（打回） | 审批意见写 `reason`，`submitted_at` / `approved_at` 清回 NULL（`ck_biz_transfer_state` 的 DRAFT 分支要求时间戳全空） |
/// | 其它（`PENDING` / `CANCELLED` / `WITHDRAWN`） | 不动 | 还在跑 / 已撤回 |
///
/// # 为什么不让它走 [`settle`]
///
/// [`settle`] 的判据是 `biz_type == TASK_ACCEPT` 且目标状态是任务 5 态；
/// 调拨单是另一张表、另一套 6 态、另一套时间戳约束。把两者塞进一个函数只会让
/// 「谁的 biz_id 指到谁」变成运行期猜谜。共同点是**时序**：都跑在审批事务提交之后、
/// 失败只记 `warn`（审批结论已落库，不许因业务侧失败而回滚审批单）。
///
/// # 返回值
///
/// 被推进的调拨单 id（没推进 / 不该推进 → `None`）。本函数**不返回错误**。
///
/// # 幂等
///
/// 重复回调时调拨单已不在 `PENDING` → [`crate::business::erp::transfer::transfer_model::settle_approval`]
/// 的 `WHERE status = 'PENDING'` 命中 0 行 → 返回 `None`，不报错。
pub async fn on_transfer_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let approved = match instance_status.trim().to_ascii_uppercase().as_str() {
        INSTANCE_APPROVED => true,
        INSTANCE_REJECTED => false,
        _ => return None,
    };
    match settle_transfer(instance_id, approved).await {
        Ok(Some(transfer_id)) => {
            tracing::info!(
                %instance_id,
                %transfer_id,
                approved,
                "审批结论已回调调拨单状态"
            );
            Some(transfer_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%instance_id, %error, "审批结论回调调拨单失败（不影响审批单）");
            None
        }
    }
}

/// [`on_transfer_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle_transfer(instance_id: Uuid, approved: bool) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是调拨类的（别的业务类型与本回调无关）
    let Some(instance) = model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过调拨单回调");
        return Ok(None);
    };
    if instance.biz_type != STOCK_TRANSFER_BIZ_TYPE {
        return Ok(None);
    }
    let Some(transfer_id) = instance.biz_id else {
        return Ok(None);
    };

    // ② 调拨单当前态（只推进 PENDING）
    let Some(row) = erp_transfer::find_by_id(transfer_id).await? else {
        tracing::warn!(%transfer_id, "调拨单不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    // ★ C1（t_1e174431）：写目标状态前先校验调拨单当前状态还在不在「能接受这个审批结论」
    //   的合法路径上（状态机边 `PENDING → APPROVED|DRAFT`，不写 if-else 硬判）。
    //   不在路径上（已取消 / 已被别的路径推走）→ 不落状态 + WARN + 审批实例留痕。
    let target = if approved {
        erp_transfer::STATUS_APPROVED
    } else {
        erp_transfer::STATUS_DRAFT
    };
    if !settle_guard::gate(
        &instance,
        crate::business::shared::state_machine::Entity::Transfer,
        &row.transfer_no,
        &row.status,
        erp_transfer::STATUS_PENDING,
        target,
    )
    .await
    {
        return Ok(None);
    }

    // ③ 驳回意见：实例行的 `result_comment` 常为空（引擎把意见写在待办行），读不到就回退
    let opinion = match instance
        .result_comment
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        Some(text) => Some(text.to_string()),
        None => latest_verdict_comment(instance_id).await?,
    };
    let opinion_for_tx = opinion.clone();
    // 审批人（`approver_id` 落库用）：引擎把「谁点的同意」写在待办行的 `assignee_id`，
    // 实例行没有这个字段 —— 从最近一条已完成的待办里取（多节点流程取最后一票）。
    let approver = latest_completed_assignee(instance_id).await?;
    // 时间点在 Rust 侧算好（与日志 / 详情同一个值）
    let now = chrono::Utc::now();
    // 联动上下文要用的快照（事务闭包里按值捕获，避免闭包内外借同一份 row）
    let transfer_no_for_linkage = row.transfer_no.clone();
    let transfer_no_in_tx = transfer_no_for_linkage.clone();
    let creator = row.create_by;
    let (affected, linkage_run) = db()
        .transaction(move |tx| async move {
            let affected =
                erp_transfer::settle_approval(&tx, transfer_id, approved, opinion_for_tx.as_deref(), approver, now)
                    .await?;
            if affected == 0 {
                // CAS 落空（单据已被别的路径推走）→ 不触发联动，与「状态没变就不通知」同口径
                return Ok((0u64, linkage::LinkageRun::default()));
            }
            // LNK-01：审批回调是**独立于 `change_transfer_status` 的状态变更点**。
            // 只接人工流转会让「配了已发布流程」的生产环境里 STOCK_TRANSFER/APPROVED
            // 的规则永远不触发（P1-FIX-5C 那类静默失效），所以这里同样走通用引擎，
            // 排除清单与人工流转**共用** `transfer_linkage` 里的那一份。
            let run = transfer_linkage::run_in_tx(&tx, transfer_id, &transfer_no_in_tx, target, approver, creator)
                .await
                .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
            Ok((affected, run))
        })
        .await?;
    if affected == 0 {
        return Ok(None);
    }
    // 提交**之后**才跑 AfterCommit 动作（发通知 / 建任务）：失败只记日志，不回滚审批结论。
    transfer_linkage::run_deferred_after_commit(
        transfer_id,
        &transfer_no_for_linkage,
        target,
        approver,
        creator,
        linkage_run,
    )
    .await;
    if !approved && let Some(opinion) = opinion.as_deref() {
        tracing::info!(%transfer_id, opinion, "调拨单被审批驳回，已打回草稿");
    }
    Ok(Some(transfer_id))
}

/// 最近一条「已完成待办」的处理人（审批通过时写进 `biz_stock_transfer.approver_id`）。
///
/// 多节点流程取 `completed_at` 最新的一票（就是让实例落终态的那个人）。
/// 查不到（历史数据/被清理）→ `None`，调用方按「保留原值」处理。
// SCOPE-EXEMPT: 按 `instance_id` 精确取**单条**待办（`LIMIT 1`），不是列表查询。
async fn latest_completed_assignee(instance_id: Uuid) -> DbResult<Option<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT t.assignee_id FROM biz_flow_task t \
              WHERE t.instance_id = ?::uuid AND t.status = 'APPROVED' AND t.assignee_id IS NOT NULL \
              ORDER BY coalesce(t.completed_at, t.created_at) DESC LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("assignee_id")?),
    }
}

/// 最近一条「有意见的待办行」的审批意见（驳回原因的回退来源）。
///
/// 引擎把同意 / 驳回意见写在 `biz_flow_task.comment`（待办行），实例行的 `result_comment`
/// 往往为空。回调要拿「为什么被驳回」当调拨单打回原因，所以按 `completed_at` 倒序取第一条非空。
// SCOPE-EXEMPT: 按 `instance_id` 精确取**单条**审批意见（`LIMIT 1`），不是列表查询。
async fn latest_verdict_comment(instance_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT t.comment FROM biz_flow_task t \
              WHERE t.instance_id = ?::uuid AND t.comment IS NOT NULL AND t.comment <> '' \
              ORDER BY coalesce(t.completed_at, t.created_at) DESC LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => {
            let comment: String = row.get_typed("comment")?;
            Ok(Some(comment))
        }
    }
}
