//! 单测共用的「把审批实例推到终态」夹具（`#[cfg(test)]`，不进生产二进制）。
//!
//! # 为什么需要它（TEST-FIX · 收货单作废链路 57 条红）
//!
//! 种子里 `assignee_type = 'ADMIN'` 的审批节点在**运行时展开全部超管**（`stock_in_cancel`
//! 的 `n_review` 就是，见 V0174），而 `approval_mode` 空 / `join_mode=AND` ⇒ 会签。
//! 于是**同一份代码、同一个提交**在两种库状态下生成的待办数不同：
//!
//! - 库里只有 `admin` 一个超管 → 1 条待办；
//! - 库里另有别的超管夹具用户（例如 `payable_bridge` 的 `p17_ap1/p17_ap2` 没被清场）
//!   → 3 条待办 = **3 人会签**。
//!
//! 老夹具的口径是「只批第一条待办」，在第二种库里实例会停在 `PENDING`：业务桥
//! `on_cancel_settled` 只在实例落终态时回调，于是单据卡在 `PENDING_CANCEL`，
//! 断言报「复核通过后单据必须真的作废」。**生产语义没问题（作废本来就该多人复核），
//! 错的是夹具对「审批人恒为 1 人」的假设。**
//!
//! 所以单测的口径必须是「**谁在审批人里就批到终态**」：
//! [`approve_all_pending`] 把实例上所有 `PENDING` 待办逐条同意（各自独立事务、提交后
//! 返回出参，调用方再按生产口径跑业务回调），最后用 [`assert_instance_status`] 钉住
//! 「实例确实到终态」——回调本来就不该在 `PENDING` 时触发，用例要在这里更早失败。

use nik::prelude::*;
use nik::uuid::Uuid;

use super::engine;

/// 某实例当前的 `PENDING` 待办：`(task_id, assignee_id)`。
///
/// 排序固定 `(node_key, assignee_id)`：多个节点的待办并存时（并行网关）也稳定，
/// 用例不会因为 PostgreSQL 的行序抖动而吵架。
pub(crate) async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, Uuid)> {
    let rows = db()
        .find_with_params(
            "SELECT id, assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING' \
              ORDER BY node_key, assignee_id",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查审批待办失败");
    rows.iter()
        .map(|row| {
            (
                row.get_typed("id").expect("待办 id"),
                row.get_typed("assignee_id").expect("审批人"),
            )
        })
        .collect()
}

/// 平台超管人数。
///
/// `assignee_type = 'ADMIN'` 的节点按这个人数展开待办（`assignee::load_admins` 的口径：
/// `role_code = 'SUPER_ADMIN'` 且未删），`join_mode = AND` 时会签 ⇒ **待办数 = 超管数**。
/// 用例里凡是断言「待办恰好 1 条」的地方都该改成拿这个数比，否则「库里多一个超管就红」。
pub(crate) async fn super_admin_count() -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT COUNT(DISTINCT u.id) FROM sys_user u \
           JOIN sys_user_role ur ON ur.user_id = u.id \
           JOIN sys_role r ON r.id = ur.role_id \
          WHERE r.role_code = 'SUPER_ADMIN' AND u.deleted_at IS NULL",
        Vec::new(),
    )
    .await
    .expect("统计超管人数失败")
}

/// 实例状态（行不存在 → `None`）。
pub(crate) async fn instance_status(instance_id: Uuid) -> Option<String> {
    db().scalar_with_params::<Option<String>>(
        "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
        vec![Value::from(&instance_id)],
    )
    .await
    .expect("查审批实例状态失败")
}

/// 断言实例到了期望状态（`expected` 传 `"APPROVED"` / `"REJECTED"` 等）。
pub(crate) async fn assert_instance_status(instance_id: Uuid, expected: &str) {
    let actual = instance_status(instance_id).await;
    assert_eq!(
        actual.as_deref(),
        Some(expected),
        "审批实例 {instance_id} 必须到 {expected}（实际 {actual:?}）；\
         没到终态就说明还有 {n} 条 PENDING 待办没批，业务回调本就不该触发",
        n = pending_tasks(instance_id).await.len()
    );
}

/// 把实例上**所有** `PENDING` 待办逐条同意，直到没有待办或实例离开 `PENDING`。
///
/// 返回**每次动作**的出参（顺序 = 动作顺序），调用方在拿到出参后跑自己的业务桥
/// （`on_cancel_settled` / `on_transfer_settled` …），与 `flow::handler::approve_task`
/// 同序：**事务先提交，再回调**。中间状态（`PENDING`）的回调本来就是 no-op
/// （`settle_verdict` 只认终态），所以会签场景下真正干活的是最后那一条。
///
/// 上限 `MAX_ROUNDS` 只是防呆：正常最多「节点数 × 审批人数」，32 轮足够；
/// 真撞上说明库里待办在自我增殖（例如流程定义被写成环），该红而不是死循环。
pub(crate) async fn approve_all_pending(instance_id: Uuid, comment: &str) -> Vec<engine::ActionOut> {
    const MAX_ROUNDS: usize = 32;
    let mut outs = Vec::new();
    for round in 0..MAX_ROUNDS {
        if instance_status(instance_id).await.as_deref() != Some("PENDING") {
            break;
        }
        let Some((task_id, assignee)) = pending_tasks(instance_id).await.first().copied() else {
            // 没有待办却仍在 PENDING：节点在等人（例如 `SKIPPED` 之外的空指派），
            // 这不是夹具能推进的，交回调用方断言。
            break;
        };
        let tx = db().begin().await.expect("开审批事务");
        let out = engine::approve(&tx, task_id, assignee, Some(comment))
            .await
            .unwrap_or_else(|err| panic!("引擎同意失败（第 {round} 轮 task={task_id}）：{err}"));
        tx.commit().await.expect("提交审批事务");
        if let Some(rejected) = out.rejected {
            panic!("审批被拒：{} {}", rejected.code, rejected.msg);
        }
        // 与生产同序：提交之后 flush 通知/草稿副作用
        let mut out = out;
        engine::flush_effects(out.effects.take()).await;
        engine::flush_drafts(out.effects.take_drafts()).await;
        outs.push(out);
    }
    outs
}
