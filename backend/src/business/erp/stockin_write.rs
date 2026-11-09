// ============================================================================
// stockin_write.rs —— 写入：建单/改单/软删/过账（库存加法 + 库位账 + 流水 + 应付凭证联动）
//
// 由 `stockin_model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::shared::state_machine;

use super::inventory_writer::{self, CostDelta, TxnRecord};
use super::stockin_base::*;

/// 插入明细（建单 / 改单共用），返回明细 id（按行序）。
pub(super) async fn insert_items(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
    lines: &[NewStockInLine],
) -> DbResult<Vec<Uuid>> {
    let mut ids = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        let id = Uuid::now_v7();
        let cost = line.cost();
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_TABLE} \
                    (id, stock_in_id, line_no, source_item_id, material_id, unit_id, qty, price, \
                     price_incl, tax_rate, amount, tax_amount, with_tax_amount, cost_price, \
                     location_id, location_dict_id, remark) \
                 VALUES (?, ?::uuid, ?, NULLIF(?::text,'')::uuid, ?::uuid, ?::uuid, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, NULLIF(?::text,''))"
            ),
            vec![
                Value::from(&id),
                Value::from(&stock_in_id),
                Value::from(i32::try_from(index + 1).unwrap_or(i32::MAX)),
                Value::Text(line.source_item_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(&line.material_id),
                Value::from(&line.unit_id),
                Value::Text(line.qty.round_dp(4).to_string()),
                Value::Text(line.price.to_string()),
                Value::Text(line.price_incl.to_string()),
                Value::Text(line.tax_rate.to_string()),
                Value::Text(line.amount.round_dp(4).to_string()),
                Value::Text(line.tax_amount.round_dp(4).to_string()),
                Value::Text(line.with_tax_amount.round_dp(4).to_string()),
                Value::Text(cost.unit_cost.to_string()),
                Value::Text(line.location_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(line.location_dict_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(line.remark.clone().unwrap_or_default()),
            ],
        )
        .await?;
        ids.push(id);
    }
    Ok(ids)
}

/// 建单（`status = DRAFT`）：**取号 → 单头 → 明细 → 建单日志**，调用方负责事务边界。
pub async fn create(
    ex: &impl SqlExecutor,
    new: &NewStockIn<'_>,
    lines: &[NewStockInLine],
    totals: &Totals,
    operator: state_machine::Operator,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let in_no = next_in_no(ex, now).await?;
    let id = Uuid::now_v7();
    let operator_text = Value::Text(operator.id.map(|v| v.to_string()).unwrap_or_default());
    // `in_type` 由「是否无源」唯一决定（FIX-7）：无源落 SOURCELESS，有采购单落 PURCHASE。
    // 之前这里把 `PURCHASE` 直接拼进 SQL，无源单（is_sourceless=true）跟着一起冒充采购入库。
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                (id, in_no, in_type, source_order_id, supplier_id, is_sourceless, warehouse_id, in_date, status, \
                 total_qty, total_amount, total_tax, total_with_tax, invoice_received, remark, \
                 created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, ?, NULLIF(?::text,'')::uuid, ?::uuid, ?, ?::uuid, \
                 NULLIF(?::text,'')::date, '{STATUS_DRAFT}', \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, ?, NULLIF(?::text,''), \
                 ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(in_no.clone()),
            Value::from(in_type_of(new.is_sourceless)),
            Value::Text(new.source_order_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&new.supplier_id),
            Value::from(new.is_sourceless),
            Value::from(&new.warehouse_id),
            Value::Text(new.in_date.to_string()),
            Value::Text(totals.qty.round_dp(4).to_string()),
            Value::Text(totals.amount.round_dp(4).to_string()),
            Value::Text(totals.tax.round_dp(4).to_string()),
            Value::Text(totals.with_tax.round_dp(4).to_string()),
            Value::from(new.invoice_received),
            Value::Text(new.remark.unwrap_or_default().to_string()),
            Value::from(now),
            Value::from(now),
            operator_text.clone(),
            operator_text,
        ],
    )
    .await?;
    insert_items(ex, id, lines).await?;
    // P10 铁律 ③：建单留痕（`from_status = NULL → DRAFT`），与建单**同一事务**。
    // 失败即整单回滚：宁可建不出单，也不要「有单没轨迹」。
    state_machine::log_created(
        ex,
        state_machine::ENTITY_STOCK_IN,
        id,
        &in_no,
        STATUS_DRAFT,
        operator.clone(),
        None,
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    Ok(id)
}

/// 改单（**仅 DRAFT 由调用方保证**）：单头整单覆盖 + 明细「删旧插新」。返回 `affected`。
pub async fn update(
    ex: &impl SqlExecutor,
    id: Uuid,
    new: &NewStockIn<'_>,
    lines: &[NewStockInLine],
    totals: &Totals,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|v| v.to_string()).unwrap_or_default());
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {TABLE} \
                    SET in_type = ?, source_order_id = NULLIF(?::text,'')::uuid, supplier_id = ?::uuid, \
                        is_sourceless = ?, \
                        warehouse_id = ?::uuid, in_date = NULLIF(?::text,'')::date, \
                        total_qty = NULLIF(?::text,'')::numeric, \
                        total_amount = NULLIF(?::text,'')::numeric, \
                        total_tax = NULLIF(?::text,'')::numeric, \
                        total_with_tax = NULLIF(?::text,'')::numeric, invoice_received = ?, \
                        remark = NULLIF(?::text,''), updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = '{STATUS_DRAFT}' AND deleted_at IS NULL"
            ),
            vec![
                // 草稿改单允许在「有源 / 无源」之间切换，`in_type` 跟着来源一起刷新（FIX-7）
                Value::from(in_type_of(new.is_sourceless)),
                Value::Text(new.source_order_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(&new.supplier_id),
                Value::from(new.is_sourceless),
                Value::from(&new.warehouse_id),
                Value::Text(new.in_date.to_string()),
                Value::Text(totals.qty.round_dp(4).to_string()),
                Value::Text(totals.amount.round_dp(4).to_string()),
                Value::Text(totals.tax.round_dp(4).to_string()),
                Value::Text(totals.with_tax.round_dp(4).to_string()),
                Value::from(new.invoice_received),
                Value::Text(new.remark.unwrap_or_default().to_string()),
                Value::from(now),
                operator_text,
                Value::from(&id),
            ],
        )
        .await?;
    if affected == 0 {
        return Ok(0);
    }
    // 明细没有 deleted_at，也不该留历史行：整单重建（行号从 1 连续）
    ex.exec_sql(
        format!("DELETE FROM {ITEM_TABLE} WHERE stock_in_id = ?::uuid"),
        vec![Value::from(&id)],
    )
    .await?;
    insert_items(ex, id, lines).await?;
    Ok(affected)
}

/// 只改备注（**入账后唯一允许改的业务列**，卡面 §二 ③ 白名单）。
///
/// `remark` 不进任何账务口径（不参与成本、不进凭证），所以 `POSTED` / `RECEIVED` 之后
/// 仍可补一句「这单是 XX 客户急件」；改它必须走这个函数，**不能**顺手把金额带过去。
pub async fn update_remark(
    ex: &impl SqlExecutor,
    id: Uuid,
    remark: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET remark = NULLIF(?::text,''), updated_at = ?, \
                update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(remark.unwrap_or_default().to_string()),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

/// [`apply_status`] 的入参（`ex` 之外的 6 项）。
///
/// 与 [`ApplyStatusWithInstanceInput`] 同形，只是**没有** `approval_instance_id`：
/// 本包装恒不挂审批实例。与 `purchase_model::ApplyStatusInput` 保持同一形态
/// （两个同名包装函数读起来一致）。字段语义与 [`ApplyStatusWithInstanceInput`] 逐项一致。
#[derive(Debug, Clone, Copy)]
pub struct ApplyStatusInput<'a> {
    /// 收货入库单主键（`WHERE id = ?::uuid`）。
    pub id: Uuid,
    /// 期望的当前态（CAS：`WHERE status = ?`；与库中不符则 `affected = 0`）。
    pub from: &'a str,
    /// 目标态（`POSTED` / `PENDING_CANCEL` / `PENDING_POST` / `DRAFT` / `CANCELLED`）。
    pub to: &'a str,
    /// 申请 / 作废理由（`PENDING_CANCEL` 落 `cancel_reason`，`CANCELLED` 落 `cancel_reason`）。
    pub reason: Option<&'a str>,
    /// 操作人（落 `update_by` / `posted_by` / `cancelled_by`；`None` = 系统 / 测试）。
    pub operator: Option<Uuid>,
    /// 业务时间（落 `posted_at` / `cancelled_at` / `updated_at` 等时间列）。
    pub now: DateTime<Utc>,
}

/// 状态流转（CAS：`WHERE id = ? AND status = from`），返回 `affected`。
///
/// * `DRAFT → POSTED`：写 `posted_at` / `posted_by`。**库存增加不在这里做** ——
///   由 [`apply_cost`] / [`post_stock_in`] 在同一事务里完成。
/// * `DRAFT → CANCELLED`：写 `cancelled_at/by/reason`，不动库存（从未加过）。
/// * `POSTED → CANCELLED`：只改状态 + 原因；库存冲回由调用方（`cancel_in_tx`）在同事务做。
/// * `POSTED → PENDING_CANCEL`（P16）：写状态 + `cancel_reason`（申请理由）+
///   `approval_instance_id`；**账一动没动**。
/// * `PENDING_CANCEL → CANCELLED`（P16）：写 `cancelled_at/by`，保留 `cancel_reason`
///   （红冲凭证摘要用它），并**清空 `approval_instance_id`**（`ck_biz_si_pending_cancel`
///   要求非审批中状态不得挂实例）。
/// * `PENDING_CANCEL → POSTED`（P16 驳回 / 撤回）：状态回滚 + 清空实例与申请理由。
pub async fn apply_status(ex: &impl SqlExecutor, input: ApplyStatusInput<'_>) -> DbResult<u64> {
    let ApplyStatusInput {
        id,
        from,
        to,
        reason,
        operator,
        now,
    } = input;
    apply_status_with_instance(
        ex,
        ApplyStatusWithInstanceInput {
            id,
            from,
            to,
            reason,
            operator,
            approval_instance_id: None,
            now,
        },
    )
    .await
}

/// [`apply_status_with_instance`] 的入参（`ex` 之外的 7 项）。
///
/// 同 `sales_order_model` 的同名入参：把散参数收成一块，调用点读起来就是
/// 「哪张单、从哪个态到哪个态、谁、何时、挂哪个审批实例」。
#[derive(Debug, Clone, Copy)]
pub struct ApplyStatusWithInstanceInput<'a> {
    /// 收货入库单主键（`WHERE id = ?::uuid`）。
    pub id: Uuid,
    /// 期望的当前态（CAS：`WHERE status = ?`；与库中不符则 `affected = 0`）。
    pub from: &'a str,
    /// 目标态（`POSTED` / `PENDING_CANCEL` / `PENDING_POST` / `DRAFT` / `CANCELLED`）。
    pub to: &'a str,
    /// 申请 / 作废理由（`PENDING_CANCEL` 落 `cancel_reason`，`CANCELLED` 落 `cancel_reason`）。
    pub reason: Option<&'a str>,
    /// 操作人（落 `update_by` / `posted_by` / `cancelled_by`；`None` = 系统 / 测试）。
    pub operator: Option<Uuid>,
    /// 本次审批实例 id：`PENDING_POST` / `PENDING_CANCEL` **必填**（库约束要求），
    /// 其它目标态一律置 `NULL`。
    pub approval_instance_id: Option<Uuid>,
    /// 业务时间（落 `updated_at` 等时间列）。
    pub now: DateTime<Utc>,
}

/// 同 [`apply_status`]，但能**同时**写 `approval_instance_id`（P16 发起作废审批用）。
///
/// 拆成两个入口是因为「写不写实例 id」是两件事：普通流转必须保持 `NULL`
/// （否则 `ck_biz_si_pending_cancel` 当场 23514），只有发起审批那次才带值。
pub async fn apply_status_with_instance(
    ex: &impl SqlExecutor,
    input: ApplyStatusWithInstanceInput<'_>,
) -> DbResult<u64> {
    let ApplyStatusWithInstanceInput {
        id,
        from,
        to,
        reason,
        operator,
        approval_instance_id,
        now,
    } = input;
    let operator_text = Value::Text(operator.map(|v| v.to_string()).unwrap_or_default());
    let reason_text = reason.unwrap_or_default().to_string();
    let (sql, params) = match to {
        STATUS_POSTED => (
            format!(
                "UPDATE {TABLE} \
                    SET status = '{STATUS_POSTED}', posted_at = ?, \
                        posted_by = NULLIF(?::text,'')::uuid, updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid, \
                        approval_instance_id = NULL, cancel_reason = NULL \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                operator_text.clone(),
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_PENDING_CANCEL => {
            // 审批中没有实例 id 就等于「永远等不到回调」：库约束和代码两头都挡
            let Some(instance_id) = approval_instance_id else {
                return Err(DbError::InvalidValue(
                    "发起作废审批必须同时写 approval_instance_id（ck_biz_si_pending_cancel）".to_string(),
                ));
            };
            (
                format!(
                    "UPDATE {TABLE} \
                        SET status = '{STATUS_PENDING_CANCEL}', cancel_reason = ?::text, \
                            approval_instance_id = ?::uuid, updated_at = ?, \
                            update_by = NULLIF(?::text,'')::uuid \
                      WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
                ),
                vec![
                    Value::Text(reason_text),
                    Value::from(&instance_id),
                    Value::from(now),
                    operator_text,
                    Value::from(&id),
                    Value::Text(from.to_string()),
                ],
            )
        }
        // P-FLOW-3A：提交过账审批（`DRAFT → PENDING_POST`）。只挂审批单，账一分没动。
        STATUS_PENDING_POST => {
            let Some(instance_id) = approval_instance_id else {
                return Err(DbError::InvalidValue(
                    "提交过账审批必须同时写 approval_instance_id（ck_biz_si_pending_approval）".to_string(),
                ));
            };
            (
                format!(
                    "UPDATE {TABLE} \
                        SET status = '{STATUS_PENDING_POST}', approval_instance_id = ?::uuid, \
                            updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                      WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
                ),
                vec![
                    Value::from(&instance_id),
                    Value::from(now),
                    operator_text,
                    Value::from(&id),
                    Value::Text(from.to_string()),
                ],
            )
        }
        // P-FLOW-3A：过账审批驳回 / 撤回（`PENDING_POST → DRAFT`）。清实例，账一分没动。
        STATUS_DRAFT => (
            format!(
                "UPDATE {TABLE} \
                    SET status = '{STATUS_DRAFT}', approval_instance_id = NULL, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_CANCELLED => (
            format!(
                "UPDATE {TABLE} \
                    SET status = '{STATUS_CANCELLED}', cancelled_at = ?, \
                        cancelled_by = NULLIF(?::text,'')::uuid, cancel_reason = ?::text, \
                        approval_instance_id = NULL, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                operator_text.clone(),
                Value::Text(reason_text),
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        other => {
            return Err(DbError::InvalidValue(format!(
                "状态流转的目标 `{other}` 不在 3 个可写目标里（DRAFT 只由建单产生）"
            )));
        }
    };
    ex.exec_sql(sql, params).await
}

/// 软删（**仅 DRAFT / PENDING_POST 由调用方保证**），返回 `affected`。
///
/// P-FLOW-3A：`PENDING_POST`（过账审批中）也允许删 —— 前提是调用方**先**终止在跑的
/// 审批单（[`crate::business::erp::stockin_bridge::cancel_approval_of`]，同一事务），
/// 否则删掉的单据会留下孤儿审批（P11 的教训）。删的同时清空 `approval_instance_id`。
pub async fn soft_delete(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>, now: DateTime<Utc>) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET deleted_at = ?, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid, approval_instance_id = NULL \
              WHERE id = ?::uuid AND status IN ('{STATUS_DRAFT}', '{STATUS_PENDING_POST}') \
                AND deleted_at IS NULL"
        ),
        vec![
            Value::from(now),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== ★ 过账：库存加法 + 流水 ====================

/// 锁读 `(物料, 仓库, 库位)` 的库存行（`FOR UPDATE`，PRD §3.2.6）。
///
/// `location_id` 为 `None` 时按 `COALESCE(location_id, 零 uuid) = 零 uuid` 命中「未分配库位汇总桶」，
/// 与 `uk_biz_inv_mat_wh_loc` 的表达式索引完全一致（否则走不上索引、也锁不到同一行）。
pub(super) async fn lock_inventory(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
) -> DbResult<Option<InventorySnapshot>> {
    // P2-5A：锁读 SQL 收口到唯一写入口 [`inventory_writer`]。
    inventory_writer::lock_cost(ex, material_id, warehouse_id, location_id).await
}

/// **一条明细的过账成本落地**（PRD §3.2.3）：加数量 + 加金额 + 重算移动加权单价。
///
/// 两步：
/// 1. `SELECT … FOR UPDATE` 锁住库存行（行不存在则无锁可拿）；
/// 2. `INSERT … ON CONFLICT (material, warehouse, COALESCE(location)) DO UPDATE` 做**加法**，
///    `RETURNING` 回读真实落库值。
///
/// 为什么第 2 步用 upsert 而不是「读到就 UPDATE、读不到就 INSERT」：两个收货单并发收同一物料、
/// 且库存行**还不存在**时，两边都读到「无行」→ 两边 INSERT → 后到的撞唯一键（42P10/23505）。
/// upsert 让后到的那笔走 `DO UPDATE` 分支，加法照做，数字不丢。
///
/// `avg_cost` 的算子在 SQL 里（不是在 Rust 里）：`ROUND` 是 PG 的 numeric 语义，
/// 与库内既有 `avg_cost` 的构造方式同源（W3c 踩过「Rust 与 PG 舍入差 1 分钱」）。
pub async fn apply_cost(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    cost: CostBasis,
    now: DateTime<Utc>,
) -> DbResult<AppliedInventory> {
    // P2-5A：成本账加法（锁读 + upsert）收口到 [`inventory_writer::add_cost`]；
    // 「变动前」快照仍按原口径取：能锁到就用锁到的，锁不到（并发新建）就从加法反推。
    let locked = inventory_writer::lock_cost(ex, material_id, warehouse_id, location_id).await?;
    let after = inventory_writer::add_cost(
        ex,
        &CostDelta {
            material_id,
            warehouse_id,
            location_id,
            qty: cost.qty,
            unit_cost: cost.unit_cost,
            amount: cost.amount,
        },
        now,
    )
    .await?;
    let before = match locked {
        Some(snapshot) => snapshot,
        // 无行 → 并发插入分支：从「变动后」精确反推（加法可逆）
        None => InventorySnapshot::derived_before(after, cost.qty, cost.amount),
    };
    Ok(AppliedInventory { before, after })
}

// ==================== ★ 过账：库位账（数量账）加法 ====================

/// 库位账一次加法前后的数量（`biz_inventory_location`）。
#[derive(Debug, Clone, Copy)]
pub struct AppliedLocation {
    /// 加之前（行不存在 = 0）。
    pub before_qty: Decimal,
    /// 加之后（`RETURNING` 回读的真实落库值）。
    pub after_qty: Decimal,
}

/// **一条明细的库位账落地**（P6）：把 `qty` 加到 `biz_inventory_location` 的对应行。
///
/// 过账原来只写成本账（`biz_inventory`）+ 流水，**库位账一行不写** → 「即时库存」与
/// 「库位库存」两个 Tab 永远对不上（违反不变式③）。本函数补上那一半，且由
/// [`crate::business::erp::stockin_handler`] 的过账事务在**同一事务**里调用（失败整单回滚）。
///
/// # 两种分支（`location_id` 在本仓是 `biz_inventory_location.id`，不是 `biz_location.id`）
///
/// V0139 文件头 ① 与 `serial_model` 模块头把这条口径钉死了：ERP 里凡是 `location_id`
/// 都指**库位库存行 id**（`biz_inventory.location_id` / `biz_stock_in_item.location_id` /
/// `biz_material_serial.location_id` 三处外键都指向 `biz_inventory_location(id)`）。
/// 而 `biz_inventory_location.location_id` 这一列存的才是**库位字典 id**（`biz_location.id`，弱引用）。
/// 两条分支就是从这个「两个 id 不能混用」里长出来的：
///
/// * `Some(row_id)`：**按主键 UPDATE 这一行**，并校验它的 `material_id` / `warehouse_id`
///   与本次明细一致（库位下拉里可能选到别的物料／别的仓的库位行）；
/// * `None`：往 `(material, warehouse, location_id IS NULL)` 这个**未分配库位桶**加
///   （`uk_biz_inv_loc … NULLS NOT DISTINCT` 保证最多一行，NULL 也占一行、不会重复）。
///
/// 为什么有行 id 时不走「三列 upsert」：那会把库位行 id 当成库位字典 id 塞进
/// `biz_inventory_location.location_id`，**新建**一行（qty 从 0 开始），库位账合计立刻大于成本账。
///
/// # 锁顺序（PRD §3.2.6，防死锁）
///
/// 先 `SELECT … FOR UPDATE` 拿 before 快照，再加法。全局只有一种加锁顺序：
/// **明细按 `material_id` 升序 → 成本账先锁 → 库位账后锁**。所以两个收货单并发收同一物料时
/// 只会在成本账行上排队，库位账那一把锁永远在对方放手之后才拿，不会交叉等待。
///
/// # `locked_qty`
///
/// 收货不入锁定量：新建行显式写 `locked_qty = 0`，已有行**不动**（`ck_biz_inv_loc_locked` 要求 ≥ 0）。
///
/// P2-2 定稿：`locked_qty` 是**预留列** —— 全仓没有任何占用/拣货流程写它（V0215 列注释、
/// `docs/ERP-摆设字段口径.md`），所以这里也不为它预留扩展点。等「销售订单占用 / 生产预占」
/// 落地时再改这一句，别提前造一个没人读的数字。
pub async fn apply_location_qty(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<AppliedLocation> {
    // P2-5A：库位账加法（两条分支 + 库位行归属校验）收口到 [`inventory_writer::add_location_qty`]。
    let (before_qty, after_qty) =
        inventory_writer::add_location_qty(ex, material_id, warehouse_id, location_id, qty, now).await?;
    Ok(AppliedLocation { before_qty, after_qty })
}

/// 写一条 `IN` 流水（**只增不改**，`qty` 恒正、方向看 `txn_type`）。
#[allow(clippy::too_many_arguments)]
pub async fn write_txn(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    line_no: i32,
    cost: CostBasis,
    applied: &AppliedInventory,
    doc_id: Uuid,
    doc_no: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<()> {
    // P2-5A：流水 SQL 收口到 [`inventory_writer::insert_txn`]（22 列只写一份）。
    inventory_writer::insert_txn(
        ex,
        &TxnRecord {
            material_id,
            warehouse_id,
            txn_type: TXN_TYPE_IN,
            biz_type: BIZ_TYPE_PURCHASE_IN,
            doc_type: DOC_TYPE_STOCK_IN,
            doc_id,
            doc_no,
            line_no,
            qty: cost.qty.round_dp(4),
            unit_cost: cost.unit_cost,
            amount: cost.amount.round_dp(4),
            before: applied.before,
            after: applied.after,
            operator,
            period_code: None,
            remark: None,
            now,
        },
    )
    .await
    .map(|_| ())
}

// ==================== ★ 作废：库存反向冲减（P9） ====================

/// 回写凭证 / 应付单引用（P2-C1：入库即生成）。
///
/// `voucher_id` / `payable_id` 是**弱引用**（V0145 建表时就有这两列、可空、无外键），
/// 由过账事务在同一 `ex` 里回写（卡面 §三 ⑤）。
pub async fn link_voucher_and_payable(
    ex: &impl SqlExecutor,
    id: Uuid,
    voucher_id: Uuid,
    payable_id: Uuid,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} \
                SET voucher_id = ?::uuid, payable_id = ?::uuid, updated_at = ? \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::from(&voucher_id),
            Value::from(&payable_id),
            Value::from(now),
            Value::from(&id),
        ],
    )
    .await
}
