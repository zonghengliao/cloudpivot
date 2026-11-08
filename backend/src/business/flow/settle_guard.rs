//! 审批结论「落目标单据状态」前的统一前置校验（C1 · t_1e174431）。
//!
//! # 为什么需要这一层
//!
//! 所有审批回调（[`super::task_bridge`] 的任务 / 调拨、`erp::purchase_bridge` 采购订单、
//! `erp::stockin_bridge` 作废 / 过账、`erp::payable_bridge` 应付作废 …）此前只做一件事：
//! **看审批实例的结论**，然后写目标单据的状态。单据侧的「现在还能不能收这个结论」的判断
//! 散落在各个文件里，而且**不在预期状态时一律 `debug` 静默返回 `None`**。实测事故
//! （TSK-202610-000813）：
//!
//! ```text
//! 20:00:26 任务状态 → CANCELLED
//! 20:09:06 审批实例状态 → APPROVED
//! ```
//!
//! 审批单 `APPROVED` 与任务 `CANCELLED` 并存：回调在单据侧被静默跳过，**既没有 WARN
//! 也没有留痕**，只能靠人肉对时间线发现。本模块把「判定 → 日志 → 留痕」收成一条通道，
//! 让所有审批回调共用同一份口径（少修一处就会漏一处，见卡面第 3 条）。
//!
//! # 三条出口（本模块的唯一职责）
//!
//! | 单据当前状态 | 判定 | 动作 |
//! |---|---|---|
//! | 仍在「等这个审批」的状态 + 边合法 | [`SettleCheck::Apply`] | 调用方照常落目标状态 |
//! | 已经是目标状态 | [`SettleCheck::AlreadySettled`] | `debug` 后静默跳过（重复回调 / 幂等重放，不留痕，否则重放会刷满假告警） |
//! | 其它（已取消 / 已作废 / 被别的路径推走 / 边被配置禁用） | [`SettleCheck::Moved`] | `WARN` 全字段日志 + 审批实例上写一条留痕，**单据状态一个字节都不动** |
//!
//! 审批单自身的状态**不在这里改**：GUE 走完就是 `APPROVED`，这一层只决定
//! 「这条结论还要不要落到单据上」。
//!
//! # 为什么不写 if-else 硬判
//!
//! 「当前状态 → 目标状态」是否合法一律问状态机（卡面明令禁止硬编码流程）：
//!
//! - 业务单据：[`crate::business::shared::state_machine::effective_edges`]
//!   （`BASE_EDGES ∩ biz_flow_state_edge 配置`，读配置失败自动退化到 BASE_EDGES）；
//! - 任务：[`crate::business::task::state::callback_transition`]
//!   （`FlowCallback` 身份只开 `REVIEW → DONE|DOING`，`TRANSITIONS` 一行没动）。
//!
//! 这里额外声明的只有一件事：**这张单据「等这个审批」时应该是哪个状态**（`awaiting`）——
//! 那本来就是各调用点里已有的 `if row.status != STATUS_XXX` 常量，本模块把它收成显式参数，
//! 好让「判定」与「留痕」走同一条通道，而不是每个文件各写一份。
//!
//! # 留痕落在哪
//!
//! `biz_flow_task` 的一条 `SKIPPED` 行：`is_auto=true`、`assignee=发起人`、
//! `comment` 写明「目标已不可改，跳过落状态」+ 单据号 + 两个状态 + 时间。
//! 与「自审 SKIP / 内控排除」同一套留痕口径（`assignee.rs` / `internal_control.rs`），
//! 审批详情的时间线（[`crate::business::flow::model::timeline`]）能直接看到；
//! 同一个实例只留一条（重复回调不刷屏）。
//!
//! # 红线
//!
//! 1. **不改审批实例状态**（卡面）：`SettleCheck` 只影响「要不要写单据」，实例是 APPROVED 就是 APPROVED；
//! 2. **留痕失败不许影响审批结论**：写痕迹失败只记 `warn`，返回值仍是 `false`（= 不落单据状态）；
//! 3. **幂等不刷屏**：`AlreadySettled` 只 `debug`，不写痕迹。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::model as flow_model;
use crate::business::flow::model::InstanceRow;
use crate::business::shared::state_machine::{self, Entity};
use crate::business::task::state::{self as task_state, TaskStatus};

/// 任务在「等验收审批」时的状态（`TASK_ACCEPT` 的 awaiting 口径）。
///
/// 由 `task::state::callback_transition` 自己钉住（它只认 `REVIEW`），这里只是日志 / 留痕
/// 展示用的一份字面量，**不参与判定**。
pub const TASK_AWAITING: &str = "REVIEW";

/// 留痕行的 `comment` 前缀（同时充当「这个实例已留过痕」的幂等判据）。
pub const SKIP_TRACE_PREFIX: &str = "审批结论未落单据";

/// 留痕行的 `comment` 列宽（`biz_flow_task.comment` 是 `varchar(1000)`）。
const COMMENT_MAX_CHARS: usize = 1000;

/// 「这条审批结论还能不能落到单据上」的判定结果（纯数据，便于单测钉住）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettleCheck {
    /// 单据仍在「等这个审批」的状态，且 `当前 → 目标` 是状态机里的合法边 → 照常落状态。
    Apply,
    /// 单据**已经是目标状态**：重复回调 / 重放（幂等）→ `debug` 后静默跳过，不写痕迹。
    AlreadySettled,
    /// 单据已不在能接受这个审批结论的路径上 → **不落状态**，`WARN` + 留痕。
    ///
    /// `reason` 是给人看的一句话（写进日志与留痕），说明为什么不在路径上
    /// （「不是审批中状态」或「边被配置禁用」）。
    Moved { reason: String },
}

impl SettleCheck {
    /// 是否可以继续落目标状态。
    pub fn is_apply(&self) -> bool {
        matches!(self, SettleCheck::Apply)
    }
}

/// 业务单据的判定：读**状态机有效边**，不看任何硬编码流程。
///
/// # 参数
///
/// - `entity`：单据在 [`crate::business::shared::state_machine`] 注册表里的实体；
/// - `current`：单据**此刻**的状态（调用方刚从库里读出来的那一行）；
/// - `awaiting`：这张单据「等这个审批」时的状态（各模块自己的常量，如 `SUBMITTED` / `PENDING_CANCEL`）；
/// - `target`：这条审批结论要落的目标状态。
///
/// # 异常情况
///
/// 数据库异常不会冒出来：[`state_machine::effective_edges`] 读配置失败会退化到 BASE_EDGES
/// （状态机是主链路，不许因为配置表读不到就把审批回调打死）。
pub async fn check(
    ex: &impl SqlExecutor,
    entity: Entity,
    current: &str,
    awaiting: &str,
    target: &str,
) -> SettleCheck {
    let cur = norm(current);
    let tgt = norm(target);
    let wait = norm(awaiting);

    // ① 已经是目标状态 → 幂等重放（重复回调 / 双点 approve）
    if cur == tgt {
        return SettleCheck::AlreadySettled;
    }
    // ② 不在「等这个审批」的状态 → 单据已经被别的路径推走了
    if cur != wait {
        return SettleCheck::Moved {
            reason: format!("单据当前状态 {cur} 不是「等审批」状态 {wait}"),
        };
    }
    // ③ 在等审批 → 再用状态机边复核一次（可选边可被配置禁用 = 只许加严）
    //
    // 这里要区分两种「边表里没有」：
    //   a) **注册表里也没登记**：历史实现是直接写 SQL 的（例：调拨单驳回 `PENDING → DRAFT`
    //      走 `transfer_write::settle_approval`，`TRANSFER_EDGES` 里没有这条边）。
    //      这类边**不能拦**——拦住就等于打断生产上可用的回退路径；这里只记一条 debug，
    //      提醒后续补登记（登记它会让「人工把 PENDING 改成 DRAFT」也合法，属于另一张卡）。
    //   b) **注册表里有、有效边里没有**：说明被 `biz_flow_state_edge` 配置禁用了
    //      （D1 只许加严）→ 按「边被配置禁用」走不可改通道。
    let declared = entity
        .edges()
        .iter()
        .any(|(from, to)| *from == cur.as_str() && *to == tgt.as_str());
    if !declared {
        tracing::debug!(
            context = "flow.settle_guard",
            entity = entity.as_str(),
            current = cur,
            target = tgt,
            "状态机注册表未登记这条边（历史实现直接写 SQL），按「等审批状态」放行"
        );
        return SettleCheck::Apply;
    }
    let edges = state_machine::effective_edges(ex, entity).await;
    if edges
        .iter()
        .any(|edge| edge.from == cur.as_str() && edge.to == tgt.as_str())
    {
        SettleCheck::Apply
    } else {
        SettleCheck::Moved {
            reason: format!(
                "状态机里 {} 的 {cur} → {tgt} 不是一条有效边（可能被边配置禁用）",
                entity.label()
            ),
        }
    }
}

/// 任务的判定：复用 [`task_state::callback_transition`]（`FlowCallback` 身份的状态机分支）。
///
/// | 当前 | 目标 | 结果 |
/// |---|---|---|
/// | `REVIEW` | `DONE` / `DOING` | [`SettleCheck::Apply`] |
/// | `DONE` / `DOING` / …（== 目标） | 同左 | [`SettleCheck::AlreadySettled`] |
/// | `CANCELLED` / `TODO` / `DOING`（≠ 目标） | — | [`SettleCheck::Moved`] |
///
/// 最后一行就是本卡要修的形态：**`CANCELLED` 的任务不会被一条审批通过改回 `DONE`**。
/// 同时也**不放宽** `callback_transition` 的 `from = REVIEW` 约束：审批结论只对
/// 「等待验收」有意义，放开 `from` 就等于让一条审批通过把任意任务跳级推到 `DONE`
/// （见该函数的文档）。
pub fn check_task(current: &str, target: &str) -> SettleCheck {
    let cur = norm(current);
    let tgt = norm(target);
    if cur == tgt {
        return SettleCheck::AlreadySettled;
    }
    let (Some(from), Some(to)) = (TaskStatus::parse(&cur), TaskStatus::parse(&tgt)) else {
        // 库里的状态不在任务 5 态之内：数据异常，走「不可改」通道留痕（不 panic）
        return SettleCheck::Moved {
            reason: format!("任务状态不在 5 态之内（{cur} → {tgt}）"),
        };
    };
    match task_state::callback_transition(from, to) {
        Ok(_) => SettleCheck::Apply,
        Err(_) => SettleCheck::Moved {
            reason: format!(
                "任务的 {cur} → {tgt} 不在审批回调的合法路径上（只认 REVIEW → DONE/DOING）"
            ),
        },
    }
}

/// 业务单据的统一入口：判定 + （不合法时）`WARN` + 留痕。返回 `true` = 调用方继续落状态。
///
/// 调用点写法（每个审批回调各一处，4 行）：
///
/// ```ignore
/// if !settle_guard::gate(&instance, Entity::PurchaseOrder, &row.order_no,
///                        &row.status, STATUS_SUBMITTED, to_status).await {
///     return Ok(None);
/// }
/// ```
pub async fn gate(
    instance: &InstanceRow,
    entity: Entity,
    biz_no: &str,
    current: &str,
    awaiting: &str,
    target: &str,
) -> bool {
    let verdict = check(db(), entity, current, awaiting, target).await;
    apply(
        instance,
        entity.as_str(),
        biz_no,
        current,
        awaiting,
        target,
        verdict,
    )
    .await
}

/// 任务侧的统一入口（`awaiting` 固定 `REVIEW`，判定走任务状态机）。
pub async fn gate_task(instance: &InstanceRow, biz_no: &str, current: &str, target: &str) -> bool {
    let verdict = check_task(current, target);
    apply(
        instance,
        "biz_task",
        biz_no,
        current,
        TASK_AWAITING,
        target,
        verdict,
    )
    .await
}

/// 判定结果 → 日志 + 留痕（三个出口唯一的落地处）。
async fn apply(
    instance: &InstanceRow,
    entity_label: &str,
    biz_no: &str,
    current: &str,
    awaiting: &str,
    target: &str,
    verdict: SettleCheck,
) -> bool {
    match verdict {
        SettleCheck::Apply => true,
        SettleCheck::AlreadySettled => {
            tracing::debug!(
                context = "flow.settle_guard",
                instance_no = %instance.instance_no,
                instance_status = %instance.status,
                entity = entity_label,
                biz_no,
                current,
                target,
                "单据已是目标状态（重复回调，幂等跳过，不落状态也不留痕）"
            );
            false
        }
        SettleCheck::Moved { reason } => {
            let at = chrono::Utc::now().to_rfc3339();
            skip(
                instance,
                SkipNote {
                    entity: entity_label,
                    biz_no,
                    current,
                    awaiting,
                    target,
                    instance_no: &instance.instance_no,
                    reason: &reason,
                    at: &at,
                },
            )
            .await;
            false
        }
    }
}

/// 一次「跳过落状态」的上下文（打包成结构体：clippy 的 `too_many_arguments` 阈值是 7）。
///
/// 字段就是卡面要求 WARN 与留痕都必须带上的那几个：单据号 / 当前状态 / 目标状态 / 时间。
struct SkipNote<'a> {
    /// 单据类型（`biz_task` / `STOCK_IN` / …），日志与留痕共用的展示名。
    entity: &'a str,
    /// 单据号（人对得上的那个号）。
    biz_no: &'a str,
    /// 单据此刻的状态。
    current: &'a str,
    /// 「等这个审批」应有的状态。
    awaiting: &'a str,
    /// 这条审批结论要落的目标状态。
    target: &'a str,
    /// 审批实例号。
    instance_no: &'a str,
    /// 为什么不在合法路径上（一句话）。
    reason: &'a str,
    /// 判定时间（RFC3339）。
    at: &'a str,
}

impl SkipNote<'_> {
    /// 留痕正文（**纯函数**，单测钉住「四个要素都在」）。
    ///
    /// 四个要素：单据号 / 当前状态 / 目标状态 / 时间（卡面要求 WARN 与留痕都含这四个）。
    fn text(&self) -> String {
        let text = format!(
            "{SKIP_TRACE_PREFIX}（目标已不可改，跳过落状态）：系统留痕 · {} {} \
             当前状态 {}，审批结论要落的 {}（等审批状态应为 {}）；\
             审批单 {} 已按结论结束，单据状态保持不变；原因：{}；时间 {}",
            self.entity,
            self.biz_no,
            self.current,
            self.target,
            self.awaiting,
            self.instance_no,
            self.reason,
            self.at
        );
        text.chars().take(COMMENT_MAX_CHARS).collect()
    }
}

/// 「不在合法路径上」的落点：`WARN` 全字段日志 + 审批实例留痕（留痕失败只记 `warn`）。
async fn skip(instance: &InstanceRow, note: SkipNote<'_>) {
    tracing::warn!(
        context = "flow.settle_guard",
        instance_id = %instance.id,
        instance_no = %instance.instance_no,
        instance_status = %instance.status,
        biz_type = %instance.biz_type,
        entity = note.entity,
        biz_no = note.biz_no,
        current = note.current,
        awaiting = note.awaiting,
        target = note.target,
        reason = note.reason,
        at = note.at,
        "审批结论未落单据：目标单据当前状态已不可改，跳过落状态（审批单本身已按结论结束）"
    );
    if let Err(error) = record_trace(instance, &note).await {
        // 留痕失败不许影响审批结论：WARN 已经落了，这里只补一条「痕迹没写成」
        tracing::warn!(
            context = "flow.settle_guard",
            instance_id = %instance.id,
            instance_no = %instance.instance_no,
            %error,
            "跳过落状态的留痕写入失败（WARN 日志已记，审批单与单据都不受影响）"
        );
    }
}

/// 在审批实例上写一条 `SKIPPED` 待办留痕（幂等：同一实例只留一条）。
///
/// 返回 `Ok(false)` = 该实例已经留过（重复回调），本次不重复写。
async fn record_trace(instance: &InstanceRow, note: &SkipNote<'_>) -> DbResult<bool> {
    if flow_model::has_skip_trace(instance.id, SKIP_TRACE_PREFIX).await? {
        return Ok(false);
    }
    let Some(anchor) = flow_model::last_node_anchor(instance.id).await? else {
        return Err(DbError::InvalidValue(
            "审批实例没有任何节点行，无处挂留痕（历史/异常实例）".to_string(),
        ));
    };
    let new = flow_model::NewTask {
        id: Uuid::now_v7(),
        instance_id: instance.id,
        instance_node_id: anchor.id,
        node_key: anchor.node_key.clone(),
        round: anchor.round,
        task_kind: "APPROVAL".to_string(),
        // 留痕挂在发起人名下：他不是审批人，所以 `is_self_approval=false`，
        // 真正说明身份的是 `is_auto=true` + comment 里的「系统留痕」字样。
        assignee_id: instance.initiator_id,
        assignee_name: instance.initiator_name.clone(),
        assignee_source: "INITIATOR".to_string(),
        assignee_source_ref: None,
        is_added_sign: false,
        is_self_approval: false,
        is_auto: true,
        status: "SKIPPED".to_string(),
        comment: Some(note.text()),
        due_at: None,
        transferred_from: None,
        assignee_rule_id: None,
        escalated_from_role: None,
        escalate_reason: None,
    };
    db().transaction(move |tx| async move { flow_model::insert_task(&tx, &new).await })
        .await?;
    tracing::info!(
        context = "flow.settle_guard",
        instance_id = %instance.id,
        instance_no = %instance.instance_no,
        node_key = %anchor.node_key,
        round = anchor.round,
        "已在审批实例上留痕：目标已不可改，跳过落状态"
    );
    Ok(true)
}

/// 状态归一化（库里存的就是大写，但配置 / 入参可能带空格或小写）。
fn norm(raw: &str) -> String {
    raw.trim().to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 卡面头号形态：任务已取消 + 审批通过 → **不许**改回 DONE。
    #[test]
    fn cancelled_task_cannot_be_revived_by_approval() {
        assert_eq!(
            check_task("CANCELLED", "DONE"),
            SettleCheck::Moved {
                reason:
                    "任务的 CANCELLED → DONE 不在审批回调的合法路径上（只认 REVIEW → DONE/DOING）"
                        .to_string()
            }
        );
    }

    /// 正常路径不误伤：任务在等验收（REVIEW）+ 审批通过 → 落 DONE。
    #[test]
    fn review_task_accepts_approval() {
        assert!(check_task("REVIEW", "DONE").is_apply());
        assert!(check_task("review", "done").is_apply());
    }

    /// 驳回同理：REVIEW → DOING 可以，CANCELLED → DOING 不行。
    #[test]
    fn cancelled_task_cannot_be_revived_by_rejection() {
        assert!(check_task("REVIEW", "DOING").is_apply());
        assert!(!check_task("CANCELLED", "DOING").is_apply());
    }

    /// 重复回调（已在目标状态）是幂等，不是异常：只 debug，不写痕迹。
    #[test]
    fn already_settled_is_idempotent() {
        assert_eq!(check_task("DONE", "DONE"), SettleCheck::AlreadySettled);
        assert_eq!(check_task("DOING", "DOING"), SettleCheck::AlreadySettled);
    }

    /// 跳级不落：审批回调只认 REVIEW（`callback_transition` 的红线一行没动）。
    #[test]
    fn approval_does_not_jump_from_doing_to_done() {
        assert!(!check_task("DOING", "DONE").is_apply());
        assert!(!check_task("TODO", "DONE").is_apply());
    }

    /// 库里的脏状态不 panic，走「不可改」通道。
    #[test]
    fn unknown_status_is_moved_not_panic() {
        assert!(!check_task("SOMETHING_ELSE", "DONE").is_apply());
    }

    /// 留痕正文的四个要素必须在（单据号 / 当前状态 / 目标状态 / 时间）。
    #[test]
    fn skip_trace_carries_all_required_fields() {
        let text = SkipNote {
            entity: "biz_task",
            biz_no: "TSK-202610-000813",
            current: "CANCELLED",
            awaiting: "REVIEW",
            target: "DONE",
            instance_no: "FLW-202610-000017",
            reason: "单据当前状态 CANCELLED 不是「等审批」状态 REVIEW",
            at: "2026-11-03T20:09:06+00:00",
        }
        .text();
        for needle in [
            SKIP_TRACE_PREFIX,
            "TSK-202610-000813",
            "CANCELLED",
            "DONE",
            "FLW-202610-000017",
            "2026-11-03T20:09:06+00:00",
        ] {
            assert!(text.contains(needle), "留痕正文必须含 {needle}：{text}");
        }
        assert!(
            text.chars().count() <= COMMENT_MAX_CHARS,
            "留痕正文不许超列宽"
        );
    }
}
