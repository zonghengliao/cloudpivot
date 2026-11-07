// ============================================================================
// stockin_purchase.rs —— 采购单联动：已收数量回写、采购单状态同步、可入库判据
//
// 由 `stockin_model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model::{PurchaseOrderItemRow, PurchaseOrderRow};
use crate::business::erp::serial_model;
use crate::business::shared::state_machine;

use super::stockin_cancel::PO_RACE_PREFIX;

// P2-3：作废已过账收货单时**不再回退任何「已收数量」列**。
//
// 以前这里有一对镜像写：过账 `add_received_qty`（+N，条件 `received_qty + N <= qty`）、
// 作废 `revert_received_qty`（−N）。列已由迁移 V0217 删除，已收数量改由
// `purchase_model::POSTED_RECEIVED_SQL` 现算：本单状态一旦变成 `CANCELLED`
// （作废事务的 ⓪ 步，早于调用点），聚合就被 `s.status = 'POSTED'` 过滤掉，
// 已收数量自动回退 —— 少一次写，也就少一次「写了忘了减」的漂移机会。
//
// 超收闸随之**只剩一处**：过账事务 ①b 的 `guard_purchase_before_post`
// （锁采购单头 FOR UPDATE + `order_item_balances` 待收额度复核，出口 409 + 66032）。
// 它读的就是同一个派生口径，所以「页面说还能收多少」= 「过账放行多少」。
//
// 保留下来的动作只有「重算采购单状态」（`sync_purchase_status`）。

// ==================== 写：建单 / 改单 / 状态 / 删 ====================

/// 按明细已收情况推进采购单状态（`PARTIAL_RECEIVED` / `RECEIVED`）。
///
/// # P10 改造（铁律 ③：状态机不许绕过）
///
/// 改造前这里是一句 `UPDATE … SET status = CASE WHEN NOT EXISTS(未收完) THEN 'RECEIVED'
/// ELSE 'PARTIAL_RECEIVED' END` —— **完全绕过 `can_transition()`**：采购订单的
/// 状态机等于名存实亡（卡面 §一 ①）。
///
/// 现在拆成三步，全部在收货过账的同一个事务里：
///
/// 1. `SELECT … FOR UPDATE` 锁采购单，读「当前状态 + 单号」；
/// 2. 算目标状态 `target`（还有未收明细 → `PARTIAL_RECEIVED`，否则 `RECEIVED`）；
/// 3. 调 [`state_machine::transition_if_changed`]：`target != 当前态` 才写 `biz_status_log`
///    （`biz_ref = ("STOCK_IN", 收货单号)`），再 CAS 改状态。
///
/// `PARTIAL_RECEIVED → PARTIAL_RECEIVED`（继续部分收货）**不写状态日志**：
/// 原地不动不是状态变更，否则每次收货都多一条无意义日志（卡面 §三 的 ⚠️）。
/// `transition_if_changed` 把「没变」表达成 `Ok(None)`，本函数据此直接返回 0。
///
/// 只对「已提交 / 已审批 / 部分收货」推进；`DRAFT` / `CLOSED` / `CANCELLED` /
/// `RECEIVED` 一律不动（返回 0）。
///
/// # D4：`RECEIVED` 只在**回退**方向动
///
/// 收货单作废会把本单从已过账集合里摘出去（状态先变 `CANCELLED`，见
/// `cancel_in_tx` ⓪ 步），再调本函数重算：此时采购单可能还是 `RECEIVED`，
/// 必须能退到 `PARTIAL_RECEIVED`（边表第 11 条），
/// 否则「已收货却没有已收数量」的单既不能收也不能改。正向收货撞不到这条路径
/// （`RECEIVED` 不在 [`purchase_receivable`] 白名单里）。
///
/// `action` 只进状态日志的 reason 文案（`收货过账` / `收货作废`），不影响判定。
pub async fn sync_purchase_status(
    ex: &impl SqlExecutor,
    order_id: Uuid,
    stock_in_no: &str,
    action: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
    outbox: &mut crate::business::erp::purchase_model::LinkageOutbox,
) -> DbResult<u64> {
    // ① 锁单 + 读当前状态（`FOR UPDATE` 让并发两张收货单串行推进同一采购单）
    let header = ex
        .query_sql(
            "SELECT order_no, status FROM biz_purchase_order \
              WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
                .to_string(),
            vec![Value::from(&order_id)],
        )
        .await?;
    let Some(row) = header.first() else {
        return Ok(0);
    };
    let order_no: String = row.get_typed("order_no")?;
    let current: String = row.get_typed("status")?;
    // `RECEIVED` 也在白名单里：**只对「收货单作废回退」这一条路径有意义**
    // （D4）。正向收货在 `require_purchase_order` 就被拦住了（`RECEIVED` 不可收），
    // 所以走到这里的 `RECEIVED` 只可能是作废触发的 `RECEIVED → PARTIAL_RECEIVED` 回退。
    if !matches!(
        current.as_str(),
        "SUBMITTED" | "APPROVED" | "PARTIAL_RECEIVED" | "RECEIVED"
    ) {
        return Ok(0);
    }

    // ② 收货进度 → 目标状态
    //
    // P2-3：判据用**派生口径**（`POSTED_RECEIVED_SQL`），不再读已被删除的
    // `received_qty` 列。这一步与「待收明细」页面、「过账闸」读的是同一段 SQL，
    // 「页面显示已收完」与「状态推进成 RECEIVED」永远不会打架。
    let sql = format!(
        "SELECT count(*) FROM biz_purchase_order_item i \
          WHERE i.order_id = ?::uuid AND ({}) < i.qty",
        crate::business::erp::purchase_model::POSTED_RECEIVED_SQL
    );
    let pending: i64 = ex.scalar_sql(sql, vec![Value::from(&order_id)]).await?;
    let target = if pending == 0 {
        "RECEIVED"
    } else {
        "PARTIAL_RECEIVED"
    };

    // ③④ `transition_if_changed`：状态没变（继续部分收货）→ 不写状态日志、返回 Ok(0)，
    //     只留收货流水；变了 → 边表校验 + reason 校验 + 写 biz_status_log（关联收货单号）。
    let reason = format!("{action} {stock_in_no} 自动推进");
    let changed = state_machine::transition_if_changed(
        ex,
        state_machine::ENTITY_PURCHASE_ORDER,
        order_id,
        &order_no,
        &crate::business::erp::purchase_model::LEGAL_EDGES,
        &current,
        target,
        operator,
        Some(&reason),
        Some((state_machine::ENTITY_STOCK_IN, stock_in_no)),
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    if changed.is_none() {
        return Ok(0);
    }

    // ⑤ CAS 改状态（`WHERE status = 当前态` 防止把别人改过的单覆盖掉）
    let affected = ex
        .exec_sql(
            "UPDATE biz_purchase_order SET status = ?, updated_at = ?, \
                update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::Text(target.to_string()),
                Value::from(now),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(&order_id),
                Value::Text(current.clone()),
            ],
        )
        .await?;
    if affected == 0 {
        // ④ 已经把日志写进事务了；CAS 落空 = 别人先推走了。**必须报错让整笔回滚**：
        // 不报错就会留下「日志写了 → 单据没改」的假轨迹（P10 单测 ⑦ 的同款要求）。
        return Err(DbError::InvalidValue(format!(
            "{PO_RACE_PREFIX}采购单 {order_no} 的状态在推进期间被并发改动\
             （期望 {current}），本次过账已整笔回滚，请刷新后重试"
        )));
    }
    // ⑥ LNK-02：状态**真的**变了才跑联动（`PARTIAL_RECEIVED → PARTIAL_RECEIVED`
    //    继续部分收货不会走到这里 —— 上面 `changed.is_none()` 已经返回）。
    //
    // 为什么「部分收货」和「全部收货」都必须接在这：这条边由本函数推进，
    // 完全不经过 `purchase_model::apply_status`。少了这一处，
    // 「部分收货 → 全部收货」在联动配置里就是一条永远不响的规则。
    //
    // 采购单自己不落账（库存 / 流水 / 凭证 / 已收数量都在收货过账的事务里写完了），
    // 所以排除清单会把那 4 类动作交给专用路径（`spec::PURCHASE_EXCLUDED_ACTIONS`）。
    crate::business::erp::purchase_model::purchase_linkage::run_in_tx(
        ex, order_id, target, operator, outbox,
    )
    .await?;
    Ok(affected)
}

/// 采购单是否「可收货」（状态白名单）。
///
/// **D4 起不含 `RECEIVED`**：卡面 ⑦「采购单已 RECEIVED → 400（已收完）」。
/// 旧白名单把 `RECEIVED` 也算可收（当时靠 `pull_lines_from_purchase` 的
/// 「每行都收满 → 400」兜底），结果是**已收满的单还能建出一张 0 行/无意义的收货单**，
/// 或者收出超收后再被过账闸拦下 —— 报错点太靠后、文案也不对。
/// 现在前置拦：`RECEIVED` 直接判不可收货，文案说清「已收完」。
pub fn purchase_receivable(status: &str) -> bool {
    matches!(status, "SUBMITTED" | "APPROVED" | "PARTIAL_RECEIVED")
}

/// 采购单的一条明细的「未收数量」文本（`qty - received_qty`，负数按 0）。
///
/// `received_qty` 是派生值（P2-3：`POSTED_RECEIVED_SQL`），所以这里算出来的
/// 「未收」与「待收明细」页面、「过账闸」的可用量是同一个数。
pub fn remaining_qty(item: &PurchaseOrderItemRow) -> Decimal {
    let remaining = item.qty - item.received_qty;
    if remaining < Decimal::ZERO {
        Decimal::ZERO
    } else {
        remaining
    }
}

/// 采购单引用（先确认单子存在且可收）。
pub async fn purchase_order_ref(order_id: Uuid) -> DbResult<Option<PurchaseOrderRow>> {
    crate::business::erp::model::purchase_order_find_by_id(order_id).await
}

// ==================== 过账闸：来源采购单状态 + 待收额度（DC-03） ====================

/// 过账闸用：锁采购单头（`FOR UPDATE`）并返回 `(order_no, status)`；不存在 / 已软删 → `None`。
///
/// # 为什么是 `FOR UPDATE` 而不是 `FOR SHARE`
///
/// 两种锁都能挡住「取消采购单」的普通 `UPDATE status`。选 `FOR UPDATE` 是为了让
/// **两张收货单收同一张采购单**也互斥：`FOR SHARE` 彼此相容，两个过账事务会同时通过
/// 待收额度复核，最后只能靠（已删除的）条件更新兜底 —— P2-3 之后兜底没了，
/// 本函数的 `FOR UPDATE` 是**唯一**的并发互斥点。
///
/// # 加锁顺序（P2 收口：两条路径已统一）
///
/// 全序固定为 **收货单头 → 采购单头 → 库存行**：
///
/// * 过账 [`crate::business::erp::stockin_handler`] 的 `post_in_tx`：①锁收货单头 →
///   ①b 本函数锁采购单头 → ③库存行；
/// * 作废 `cancel_in_tx`：①锁收货单头 → ⓪ 本函数锁采购单头 → ③库存行。
///
/// 作废路径原本把采购单头留到最后（`sync_purchase_status` 才锁），与过账构成环
/// （过账持采购单行等库存行、作废持库存行等采购单行 → PG 判 `40P01`）。
/// 现在作废**提前**锁同一行，稍后 `sync_purchase_status` 里那句 `FOR UPDATE` 是同一事务的
/// 重入（不再等待）。回归证据见 `docs/evidence/20261103-p2-收货过账作废加锁顺序.md`。
///
/// ⚠️ 新增任何「先库存行、后采购单头」的路径都会把环放回来：改动作废 / 过账链路时，
/// 采购单头的 `FOR UPDATE` 必须始终**早于**第一次碰 `biz_inventory`。
///
/// DC-03 要求「过账时再复核来源采购单状态」。只读不锁的话，
/// 「读到 SUBMITTED → 库存加完 → 采购单被取消」这条时序仍然成立
/// （取消采购单只写状态，不回头看有没有在途收货单）。拿到 `FOR UPDATE` 后：
///
/// - 并发「取消 / 关闭采购单」必须等本次过账事务提交或回滚 ⇒ 复核到的状态在提交前有效；
/// - 两张收货单收同一张采购单也会在这里串行化 ⇒ 待收额度复核
///   （[`order_item_balances`]）成为**权威判据**（也是唯一判据：P2-3 之后
///   `add_received_qty` 的条件更新已随列一起删除）。
pub async fn lock_purchase_order(
    ex: &impl SqlExecutor,
    order_id: Uuid,
) -> DbResult<Option<(String, String)>> {
    let rows = ex
        .query_sql(
            "SELECT order_no, status FROM biz_purchase_order \
              WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
                .to_string(),
            vec![Value::from(&order_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some((row.get_typed("order_no")?, row.get_typed("status")?)))
}

/// 采购单某一明细行的「订单量 / 已过账收货量」。
#[derive(Debug, Clone, Copy)]
pub struct OrderItemBalance {
    /// 采购明细行 id。
    pub item_id: Uuid,
    /// 行号（1 起，文案用）。
    pub line_no: i32,
    /// 采购行数量。
    pub order_qty: Decimal,
    /// 已过账收货量（只算 `POSTED` 且未软删的收货单）。
    pub posted_received: Decimal,
}

impl OrderItemBalance {
    /// 可用待收量 = 订单量 − 已过账收货量（脏数据不许给出负数）。
    pub fn available(&self) -> Decimal {
        let pending = self.order_qty - self.posted_received;
        if pending < Decimal::ZERO {
            Decimal::ZERO
        } else {
            pending
        }
    }
}

/// 过账闸用：某张采购单每行「订单量 / 已过账收货量」。
///
/// 已收口径**复用** [`crate::business::erp::purchase_model::POSTED_RECEIVED_SQL`]
/// （与「待收明细」页 `pending_receive_lines` 同一个聚合表达式），不写第二套：
/// 一旦分叉，就会出现「页面说还能收 40、过账放行 60」这类最难查的账实不符。
pub async fn order_item_balances(
    ex: &impl SqlExecutor,
    order_id: Uuid,
) -> DbResult<Vec<OrderItemBalance>> {
    let sql = format!(
        "SELECT i.id AS item_id, i.line_no, i.qty AS order_qty, \
                ({}::numeric(18,4)) AS posted_received \
           FROM biz_purchase_order_item i \
          WHERE i.order_id = ?::uuid \
          ORDER BY i.line_no ASC",
        crate::business::erp::purchase_model::POSTED_RECEIVED_SQL
    );
    let rows = ex.query_sql(sql, vec![Value::from(&order_id)]).await?;
    rows.iter()
        .map(|row| {
            Ok(OrderItemBalance {
                item_id: row.get_typed("item_id")?,
                line_no: row.get_typed("line_no")?,
                order_qty: row.get_typed("order_qty")?,
                posted_received: row.get_typed("posted_received")?,
            })
        })
        .collect()
}

/// 物料引用（判 `serial_required` 与带出 `unit_id`）。
pub async fn material_serial_required(material_id: Uuid) -> DbResult<Option<bool>> {
    serial_model::material_serial_required(material_id).await
}

// ==================== 单测（纯逻辑，不连库） ====================
//
// 真库用例在 `stockin_handler::tests`（需要 `ERP_DB_URL`）；这里放**不依赖数据库**的口径与状态机，
// 保证「没配 ERP_DB_URL 的 CI」也能覆盖成本口径与 3 条合法边。
