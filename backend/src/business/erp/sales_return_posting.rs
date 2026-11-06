// ============================================================================
// sales_return_posting.rs —— 销售退货过账（SO-30 / ARCH-7 §15.2.3）
//
// # 一个事务里做完六件事（顺序不能换）
//
// ```text
// ①  锁退货单 + 重跑超退闸（此时本单还没算进「已退」，必须 exclude 自己）
// ②  期间闸（退货日期所在会计期间必须未结账）
// ③  建退货入库单 biz_stock_in(in_type='RETURN', source_type='SALES_RETURN')
//     + 明细 → inventory_writer 回库（成本账 / 库位账 / 流水三本账，唯一写入口）
// ④  红字入库凭证：借 1405 库存商品 / 贷 6401 主营业务成本（冲减成本）
// ⑤  冲减应收：调原 receivable 的 amount/unsettled_amount（全额退且一分没收 → 作废）
//     已开票 → 造红字发票 OUT_RED（amount 为负）+ 收入冲减凭证（借 5001 / 借 2221.02 / 贷 1122）
// ⑥  回写 biz_sales_order_item.delivered_qty（减少）+ 释放客户信用额度
// ```
//
// # 为什么回库不走 `stockin_handler::post_in_tx`
//
// `post_in_tx` 是**采购收货**的过账事务：它有采购单状态回写（③⑥）、
// 「入库即生成应付单」（⑦ `payable::create_for_stock_in`）与 ERP 应付科目
// （借 1405 + 借进项税 / 贷 2202）。退货入库没有供应商、没有应付、没有进项税，
// 硬走那条路会凭空生成一张应付单 —— 账直接错。所以本模块**只复用它的内核**：
// 三本账加法全部经 `stockin_write::apply_cost` / `apply_location_qty` →
// `inventory_writer`（唯一写入口），本文件一行三本账 SQL 都没有
// （`scripts/verify/inventory_write_gate.py` 会在 CI 里验这一点）。
//
// # 成本口径：按**原出库成本**回库，不按退货单价
//
// 退货回库要冲减的是当初结转出去的成本，不是销售价：
// 单价取原销售出库流水（`biz_inventory_txn`，`doc_id` = 发货单的 `stock_out_id`，
// `txn_type='OUT'`）的加权单价；查不到（手工出库 / 零成本存量）才回落到
// `biz_inventory.avg_cost`，再查不到按 0（只回数量、不产生成本凭证，记 warn）。
// 销售价与销项税留在退货单上（`biz_sales_return_item`），入库单不带进项税
// （退货不是采购，没有可抵扣进项）。
//
// # 应收口径（`biz_receivable` 的库层 CHECK 决定了只能用「调原单」这一条路）
//
// `ck_biz_ar_amount: amount > 0 AND received_amount <= amount` +
// `ck_biz_ar_unsettled: unsettled_amount = amount - received_amount`
// ⇒ **负向应收根本插不进去**（卡面给的第二个选项物理不可行），所以一律调原单：
//
// ```text
// 退税额 <= 未收金额           → amount -= 退货含税；unsettled = amount - received
// 全额退 + 一分没收            → 走 receivable::cancel_for_stock_out（OPEN → CANCELLED）
// 退税额 >  未收金额（钱收多了）→ 409 点名「收款退款单」：已收的钱不能凭空少
// ```
//
// # 原子性
//
// 全部动作跑在**调用方（`sales_return_handler::change_return_status`）的同一个事务**里：
// 任何一步 Err 都由调用方 `rollback()`，库存 / 库位账 / 流水 / 入库单 / 凭证 /
// 应收 / 红字发票 / delivered_qty / 状态日志一起消失。本文件自己没有 `commit()`，也不该有。
//
// # 幂等（三道闸）
//
// ① 退货单状态 CAS（`PENDING_POST → POSTED`，第二次 0 行）；
// ② 本模块入口的 `stock_in_id IS NOT NULL` 防御（已过账的退货绝不再回一次库）；
// ③ 库层部分唯一索引 `uk_biz_si_source_doc (source_type, source_id)`（V0305，
//    并发下最后一道，撞了就是 409）。
// ============================================================================

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_writer::{self, TxnRecord};
use crate::business::erp::invoice;
use crate::business::erp::period;
use crate::business::erp::receivable::{self, ReceivableRow};
use crate::business::erp::sales_return_model::{self as model, ReturnItemRow, ReturnRow};
use crate::business::erp::stockin_base::{
    AppliedInventory, CostBasis, DOC_TYPE_STOCK_IN, TXN_TYPE_IN,
};
use crate::business::erp::stockout_base::{DOC_TYPE_STOCK_OUT, TXN_TYPE_OUT};
use crate::business::erp::stockout_model::{self as stock_out};
use crate::business::erp::voucher::{
    self, ACCOUNT_AR, ACCOUNT_MAIN_COST, ACCOUNT_OUTPUT_TAX, ACCOUNT_REVENUE, EntryLine,
    PostVoucher, VOUCHER_TYPE_AUTO,
};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 常量 ====================

/// 退货入库单的 `in_type`（既有域值，见 `ck_biz_si_type`）。
///
/// 复用 `RETURN` 而不是新增域值：一张销售退货入库和一张手工退货入库在**入库性质**上
/// 完全一样，来源由 `source_type='SALES_RETURN'` 表达（V0305）。
pub const IN_TYPE_RETURN: &str = "RETURN";

/// 退货入库单的 `biz_stock_in.source_type`（V0305）。
pub const SOURCE_TYPE_SALES_RETURN: &str = "SALES_RETURN";

/// 库存流水的业务类型（`biz_inventory_txn.biz_type`，VARCHAR(24) 无 CHECK，
/// 词表口径见 V0137 列注释：与 `PURCHASE_IN` / `TRANSFER_IN` 同族）。
pub const BIZ_TYPE_SALES_RETURN_IN: &str = "SALES_RETURN_IN";

/// 入库单状态：过账后的终态（与 `stockin_base::STATUS_POSTED` 同值，这里独立声明
/// 是因为本模块只写这一个状态，钉死它在本卡的语义）。
pub const STOCK_IN_POSTED: &str = "POSTED";

/// 红字发票的票种（V0295 `ck_biz_invoice_type` 只认 OUT / OUT_RED）。
pub const INVOICE_TYPE_OUT_RED: &str = "OUT_RED";

/// 红字发票写进 `biz_invoice.source_type` 的来源值（V0305 补进白名单）。
pub const INVOICE_SOURCE_SALES_RETURN: &str = "SALES_RETURN";

// ==================== 纯函数（单测钉死） ====================

/// 入库单事由（出库列表 / 库存流水一眼能看出这是销售退货）。
pub fn stock_in_reason(return_no: &str) -> String {
    format!("销售退货 {return_no}")
}

/// 成本冲减凭证摘要。
pub fn cost_voucher_summary(return_no: &str) -> String {
    format!("销售退货 {return_no} 成本冲减")
}

/// 收入冲减凭证摘要（红字发票场景）。
pub fn revenue_voucher_summary(return_no: &str) -> String {
    format!("销售退货 {return_no} 收入冲减")
}

/// 应收冲减备注（写进 `biz_receivable.remark` 的追加行，审计靠它）。
pub fn receivable_note(return_no: &str, amount: Decimal) -> String {
    format!(
        "[退货冲减] {return_no} 冲减 {}（SO-30 过账自动冲减）",
        amount.round_dp(2)
    )
}

/// 一条退货行的回库成本（数量 × 单价，四舍五入到 4 位）。
///
/// 单价**不由本函数决定**（取原出库成本，见 [`original_unit_cost`]）；
/// 这里只负责「金额 = 数量 × 单价」这一条口径，避免两处算法漂移。
pub fn cost_amount_of(qty: Decimal, unit_cost: Decimal) -> Decimal {
    (qty * unit_cost).round_dp(4)
}

/// 加权单价 = 金额 / 数量（数量为 0 → `None`：除零没有业务含义）。
pub fn unit_cost_of(amount: Decimal, qty: Decimal) -> Option<Decimal> {
    if qty <= Decimal::ZERO {
        return None;
    }
    Some((amount / qty).round_dp(6))
}

/// 冲减后的应收三值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceivableAfter {
    /// 冲减后的应收金额。
    pub amount: Decimal,
    /// 冲减后的未收金额（= amount - received）。
    pub unsettled: Decimal,
    /// `true` = 全额退且一分没收 → 整张应收作废（`OPEN → CANCELLED`）。
    pub cancel: bool,
    /// `true` = 冲减后恰好收清（`amount == received`）→ `SETTLED`。
    pub settled: bool,
}

/// 冲减应收的**纯判定**（不碰库，单测逐档钉死）。
///
/// 返回 `Err(String)` = 业务拒绝（409 文案），三种情形：
/// 1. 退货含税金额 ≤ 0（零元退货没有可冲的应收）；
/// 2. 应收已是 `CANCELLED` / `WRITTEN_OFF`（账已经冲了 / 已核销）；
/// 3. 退货金额 > 未收金额（钱已经收进口袋，冲不回来 —— 要走收款退款单）。
pub fn receivable_after(
    ar_no: &str,
    status: &str,
    amount: Decimal,
    received: Decimal,
    return_taxed: Decimal,
) -> std::result::Result<ReceivableAfter, String> {
    if return_taxed <= Decimal::ZERO {
        return Err(format!(
            "退货含税金额必须大于 0（收到 {return_taxed}）：零元退货没有可冲的应收"
        ));
    }
    match status {
        receivable::STATUS_CANCELLED => {
            return Err(format!(
                "应收单 {ar_no} 已作废（CANCELLED）：这笔销售已经冲过，不能再冲第二遍"
            ));
        }
        receivable::STATUS_WRITTEN_OFF => {
            return Err(format!(
                "应收单 {ar_no} 已坏账核销（WRITTEN_OFF）：核销掉的应收没有余额可冲"
            ));
        }
        _ => {}
    }
    let unsettled = (amount - received).round_dp(4);
    if return_taxed > unsettled {
        return Err(format!(
            "退货含税 {} 超过应收单 {ar_no} 的未收金额 {}（应收 {} − 已收 {}）：\
             已收的钱不能凭空少 —— 请先做收款退款单，再退这一笔",
            return_taxed.round_dp(2),
            unsettled.round_dp(2),
            amount.round_dp(2),
            received.round_dp(2)
        ));
    }
    let after_amount = (amount - return_taxed).round_dp(4);
    if after_amount <= Decimal::ZERO {
        // 全额退：只有「一分没收」才允许整张作废（已收款的应收不能作废）
        if received > Decimal::ZERO {
            return Err(format!(
                "应收单 {ar_no} 已收 {}，整单退货会把它冲成 0：已收款的应收不能作废，\
                 请先做收款退款单",
                received.round_dp(2)
            ));
        }
        return Ok(ReceivableAfter {
            amount: Decimal::ZERO,
            unsettled: Decimal::ZERO,
            cancel: true,
            settled: false,
        });
    }
    let after_unsettled = (after_amount - received).round_dp(4);
    Ok(ReceivableAfter {
        amount: after_amount,
        unsettled: after_unsettled,
        cancel: false,
        settled: after_unsettled <= Decimal::ZERO,
    })
}

/// 红字发票三金额（全部取负；任一为 0 的金额会让 `ck_biz_invoice_sign` 拒收 →
/// 返回 `None` 表示「这张退货不造红票」）。
///
/// 卡面逐字：「amount 为负，type=OUT_RED」。红票的价税合计同样是负数，
/// `ck_biz_invoice_amount: total_with_tax = amount + tax_amount` 在负数下仍然成立。
pub fn red_invoice_amounts(
    total_amount: Decimal,
    total_tax: Decimal,
    total_with_tax: Decimal,
) -> Option<(Decimal, Decimal, Decimal)> {
    let (amount, tax, total) = (
        -total_amount.round_dp(4),
        -total_tax.round_dp(4),
        -total_with_tax.round_dp(4),
    );
    // `ck_biz_invoice_sign`：OUT_RED 要求 amount < 0 且 tax_amount <= 0
    if amount >= Decimal::ZERO || tax > Decimal::ZERO {
        return None;
    }
    if total != amount + tax {
        return None;
    }
    Some((amount, tax, total))
}

/// 红票税率（`biz_invoice.tax_rate` 是 0–1 的小数，退货单上是百分数）。
///
/// 按「税额 / 不含税金额」反算，四舍五入到 4 位并夹在 `[0, 1]`
/// （`ck_biz_invoice_tax_rate: 0 <= tax_rate <= 1`）。
pub fn invoice_tax_rate(total_amount: Decimal, total_tax: Decimal) -> Decimal {
    if total_amount <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let rate = (total_tax / total_amount).round_dp(4);
    if rate < Decimal::ZERO {
        Decimal::ZERO
    } else if rate > Decimal::ONE {
        Decimal::ONE
    } else {
        rate
    }
}

/// 冲减后应收状态应该是什么（`SETTLED` / 保持原状态）。
pub fn receivable_status_after(current: &str, after: &ReceivableAfter) -> &'static str {
    if after.cancel {
        receivable::STATUS_CANCELLED
    } else if after.settled {
        receivable::STATUS_SETTLED
    } else if current == receivable::STATUS_PARTIAL {
        receivable::STATUS_PARTIAL
    } else {
        receivable::STATUS_OPEN
    }
}

// ==================== 过账结果 ====================

/// 过账成功后的业务结果（供 handler 打日志 / 回显 / 单测断言）。
#[derive(Debug, Clone)]
pub struct ReturnPostOutcome {
    /// 生成的退货入库单 id（已过账）。
    pub stock_in_id: Uuid,
    /// 入库单号（`IN-yyyyMMdd-000001`）。
    pub stock_in_no: String,
    /// 回库成本合计（= 红字入库凭证金额）。
    pub cost_total: Decimal,
    /// 红字入库凭证（借 1405 / 贷 6401）；成本为 0 时为 `None`。
    pub cost_voucher_id: Option<Uuid>,
    /// 冲减的应收单（无原发货单 / 无应收时为 `None`）。
    pub receivable_id: Option<Uuid>,
    /// 冲减后的未收金额（`None` = 没冲应收）。
    pub receivable_unsettled: Option<Decimal>,
    /// 红字发票（未开票 / 零金额时为 `None`）。
    pub red_invoice_id: Option<Uuid>,
    /// 红字发票号。
    pub red_invoice_no: Option<String>,
    /// 收入冲减凭证（只在造了红票时存在）。
    pub revenue_voucher_id: Option<Uuid>,
    /// 回写的订单行数。
    pub delivered_lines: usize,
}

// ==================== 错误口径 ====================

/// 写库错误 → ApiError（唯一口径：撞来源唯一索引按幂等 409 翻译）。
fn write_error(error: DbError, ctx: &str) -> ApiError {
    if let DbError::UniqueViolation { constraint, .. } = &error
        && constraint
            .as_deref()
            .is_some_and(|name| name.contains("uk_biz_si_source_doc"))
    {
        return ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!("{ctx}：该退货单已经生成过入库单（过账幂等闸），不会重复回库"),
        );
    }
    utils::db_error(error, ctx)
}

fn conflict(message: String) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message)
}

fn bad_request(message: String) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message)
}

fn not_found(message: String) -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, message)
}

// ==================== 读 ====================

/// 锁退货单（并发过账在这里串行化）。
async fn lock_return(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> std::result::Result<Option<ReturnRow>, ApiError> {
    // SCOPE-EXEMPT: 退货单是 ERP 单据表，无归属列（可见性靠权限码）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {} FROM {} WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE",
                model::RETURN_COLUMNS,
                model::TABLE
            ),
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "锁定销售退货单"))?;
    rows.first()
        .map(|row| ReturnRow::from_row(row).map_err(|error| utils::db_error(error, "读退货单")))
        .transpose()
}

/// 原发货单的（出库单 id, 单号）；发货单不存在 → `None`。
async fn shipment_out(
    ex: &impl SqlExecutor,
    shipment_id: Uuid,
) -> std::result::Result<Option<(Option<Uuid>, String)>, ApiError> {
    let rows = ex
        .query_sql(
            "SELECT stock_out_id, ship_no FROM biz_shipment \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&shipment_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询原发货单"))?;
    rows.first()
        .map(|row| {
            let stock_out_id: Option<Uuid> = row
                .get_typed("stock_out_id")
                .map_err(|error| utils::db_error(error, "读发货单出库单引用"))?;
            let ship_no: String = row
                .get_typed("ship_no")
                .map_err(|error| utils::db_error(error, "读发货单号"))?;
            Ok((stock_out_id, ship_no))
        })
        .transpose()
}

/// 原销售出库的加权成本单价（`biz_inventory_txn`，只读）。
///
/// 一条 SQL 汇总「该出库单该物料的出库流水」，用 `Σ金额 / Σ数量` 反算单价：
/// 同一物料在同一出库单里可能拆成多行（不同库位），逐行取会丢掉加权口径。
async fn original_unit_cost(
    ex: &impl SqlExecutor,
    stock_out_id: Uuid,
    material_id: Uuid,
) -> std::result::Result<Option<Decimal>, ApiError> {
    let rows = ex
        .query_sql(
            "SELECT COALESCE(SUM(amount), 0)::text AS amount, \
                    COALESCE(SUM(qty), 0)::text    AS qty \
               FROM biz_inventory_txn \
              WHERE doc_type = ? AND doc_id = ?::uuid AND material_id = ?::uuid \
                AND txn_type = ?"
                .to_string(),
            vec![
                Value::Text(DOC_TYPE_STOCK_OUT.to_string()),
                Value::from(&stock_out_id),
                Value::from(&material_id),
                Value::Text(TXN_TYPE_OUT.to_string()),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询原出库成本流水"))?;
    let parse = |column: &str| -> Decimal {
        rows.first()
            .and_then(|row| row.get_typed::<String>(column).ok())
            .and_then(|text| text.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO)
    };
    Ok(unit_cost_of(parse("amount"), parse("qty")))
}

/// 账面移动加权单价（原出库流水查不到时的回落口径；仍查不到 → `None`）。
async fn ledger_avg_cost(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
) -> std::result::Result<Option<Decimal>, ApiError> {
    let rows = ex
        .query_sql(
            "SELECT avg_cost FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
              ORDER BY qty DESC LIMIT 1"
                .to_string(),
            vec![Value::from(&material_id), Value::from(&warehouse_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询账面移动加权单价"))?;
    rows.first()
        .map(|row| {
            row.get_typed::<Decimal>("avg_cost")
                .map_err(|error| utils::db_error(error, "读移动加权单价"))
        })
        .transpose()
}

// ==================== 写：入库单 ====================

/// 一条待建入库明细（成本已在外面算好）。
struct StockInLine {
    line_no: i32,
    material_id: Uuid,
    unit_id: Uuid,
    qty: Decimal,
    unit_cost: Decimal,
    amount: Decimal,
    so_item_id: Option<Uuid>,
    location_dict_id: Option<Uuid>,
    remark: Option<String>,
}

/// 建退货入库单（单头 + 明细，**直接落 `POSTED`**）。
///
/// 不复用 `stockin_write::create`：它把 `in_type` 写死 `PURCHASE`（V0145 的
/// `ck_biz_si_supplier` 会要求供应商），也没有来源列；退货入库既没有供应商也不走采购链。
/// 三本账一行都不在这里 —— 建单只是业务单据，库存由 [`post_inventory`] 经
/// `inventory_writer` 落。
async fn create_stock_in(
    ex: &impl SqlExecutor,
    row: &ReturnRow,
    lines: &[StockInLine],
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
) -> std::result::Result<(Uuid, String), ApiError> {
    let in_no = crate::business::erp::stockin_model::next_in_no(ex, now)
        .await
        .map_err(|error| utils::db_error(error, "取退货入库单号"))?;
    let id = Uuid::now_v7();
    let total_qty: Decimal = lines.iter().map(|line| line.qty).sum();
    let total_amount: Decimal = lines.iter().map(|line| line.amount).sum();
    let operator_text = operator
        .id
        .map(|value| value.to_string())
        .unwrap_or_default();
    let remark = format!("销售退货入库 {}（客户 {}）", row.return_no, row.customer_id);
    ex.exec_sql(
        "INSERT INTO biz_stock_in \
            (id, in_no, in_type, source_order_id, supplier_id, is_sourceless, warehouse_id, in_date, \
             status, total_qty, total_amount, total_tax, total_with_tax, invoice_received, \
             source_type, source_id, remark, posted_at, posted_by, \
             created_at, updated_at, create_by, update_by) \
         VALUES (?, ?, ?, NULL, NULL, TRUE, ?::uuid, ?::text::date, \
             ?, ?::text::numeric, ?::text::numeric, 0, ?::text::numeric, FALSE, \
             ?, ?::uuid, ?, ?, NULLIF(?::text,'')::uuid, \
             ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::Text(in_no.clone()),
            Value::Text(IN_TYPE_RETURN.to_string()),
            Value::from(&row.warehouse_id),
            Value::Text(row.return_date.to_string()),
            Value::Text(STOCK_IN_POSTED.to_string()),
            Value::Text(total_qty.round_dp(4).to_string()),
            Value::Text(total_amount.round_dp(4).to_string()),
            Value::Text(total_amount.round_dp(4).to_string()),
            Value::Text(SOURCE_TYPE_SALES_RETURN.to_string()),
            Value::from(&row.id),
            Value::Text(remark),
            // $13 posted_at / $14 posted_by / $15 created_at / $16 updated_at
            // / $17 create_by / $18 update_by —— 顺序与列清单逐位对齐
            Value::from(now),
            Value::Text(operator_text.clone()),
            Value::from(now),
            Value::from(now),
            Value::Text(operator_text.clone()),
            Value::Text(operator_text),
        ],
    )
    .await
    .map_err(|error| write_error(error, "生成退货入库单"))?;

    for line in lines {
        ex.exec_sql(
            "INSERT INTO biz_stock_in_item \
                (stock_in_id, line_no, source_item_id, material_id, unit_id, qty, price, price_incl, \
                 tax_rate, amount, tax_amount, with_tax_amount, cost_price, location_id, remark, \
                 created_at, updated_at) \
             VALUES (?::uuid, ?, NULLIF(?::text,'')::uuid, ?::uuid, ?::uuid, \
                 ?::text::numeric, ?::text::numeric, ?::text::numeric, 0, \
                 ?::text::numeric, 0, ?::text::numeric, ?::text::numeric, \
                 NULLIF(?::text,'')::uuid, NULLIF(?::text,''), ?, ?)"
                .to_string(),
        vec![
            Value::from(&id),
            Value::from(line.line_no),
            Value::Text(
                line.so_item_id
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            Value::from(&line.material_id),
            Value::from(&line.unit_id),
            Value::Text(line.qty.round_dp(4).to_string()),
            Value::Text(line.unit_cost.to_string()),
            Value::Text(line.unit_cost.to_string()),
            Value::Text(line.amount.round_dp(4).to_string()),
            Value::Text(line.amount.round_dp(4).to_string()),
            Value::Text(line.unit_cost.to_string()),
            Value::Text(line.location_dict_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(line.remark.clone().unwrap_or_default()),
            Value::from(now),
            Value::from(now),
        ],
        )
        .await
        .map_err(|error| write_error(error, "生成退货入库明细"))?;
    }
    Ok((id, in_no))
}

// ==================== 写：三本账（经唯一写入口） ====================

/// 回库：成本账 + 库位账 + 流水（**全部经 `inventory_writer`**）。
///
/// 锁顺序按 PRD §3.2.6：明细按 `(material_id, 库位账行)` 升序 → 成本账先锁 → 库位账后锁，
/// 与收货 / 出库同一条全局顺序，不会交叉等待。
async fn post_inventory(
    ex: &impl SqlExecutor,
    row: &ReturnRow,
    in_id: Uuid,
    in_no: &str,
    lines: &[StockInLine],
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    // ① 先把库位**落定**：明细上的 `location_id` 是库位字典（`biz_location.id`），
    //    库存账行是 `biz_inventory_location.id` —— 两个 id 不能混用（混了就是记错桶）。
    let mut plan: Vec<(Uuid, Option<Uuid>)> = Vec::with_capacity(lines.len());
    for line in lines.iter() {
        let location_row = match line.location_dict_id {
            Some(dict_id) => Some(
                crate::business::erp::stockin_model::ensure_location_row(
                    ex,
                    line.material_id,
                    row.warehouse_id,
                    dict_id,
                    now,
                )
                .await
                .map_err(|error| write_error(error, "回库：按库位建账"))?,
            ),
            None => None,
        };
        plan.push((line.material_id, location_row));
    }
    // ② 按锁顺序过账
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by_key(|index| (plan[*index].0, plan[*index].1, lines[*index].line_no));
    for index in order {
        let (material_id, location_row) = plan[index];
        let line = &lines[index];
        let cost = CostBasis {
            qty: line.qty.round_dp(4),
            amount: line.amount.round_dp(4),
            unit_cost: line.unit_cost,
        };
        let applied: AppliedInventory = crate::business::erp::stockin_model::apply_cost(
            ex,
            material_id,
            row.warehouse_id,
            location_row,
            cost,
            now,
        )
        .await
        .map_err(|error| write_error(error, "回库：成本账加法"))?;
        crate::business::erp::stockin_model::apply_location_qty(
            ex,
            material_id,
            row.warehouse_id,
            location_row,
            line.qty,
            now,
        )
        .await
        .map_err(|error| write_error(error, "回库：库位账加法"))?;
        // 流水：业务类型是本卡自己的 `SALES_RETURN_IN`（`stockin_write::write_txn` 写死
        // `PURCHASE_IN`，用它会把这批回库记成采购入库 —— 追溯给假答案）。
        inventory_writer::insert_txn(
            ex,
            &TxnRecord {
                material_id,
                warehouse_id: row.warehouse_id,
                txn_type: TXN_TYPE_IN,
                biz_type: BIZ_TYPE_SALES_RETURN_IN,
                doc_type: DOC_TYPE_STOCK_IN,
                doc_id: in_id,
                doc_no: in_no,
                line_no: line.line_no,
                qty: line.qty.round_dp(4),
                unit_cost: line.unit_cost,
                amount: line.amount.round_dp(4),
                before: applied.before,
                after: applied.after,
                operator: operator.id,
                period_code: None,
                remark: Some("销售退货回库"),
                now,
            },
        )
        .await
        .map_err(|error| write_error(error, "回库：写库存流水"))?;
    }
    Ok(())
}

// ==================== 写：凭证 ====================

/// 红字入库凭证：借 1405 库存商品 / 贷 6401 主营业务成本（逐物料分行，按物料聚合）。
///
/// 成本合计为 0（零成本存量退货）→ `Ok(None)`：`ck_biz_voucher` 不收零金额分录，
/// 硬造一张全 0 凭证只会让试算平衡表多一行噪音（与 `create_for_stock_out` 同口径）。
async fn write_cost_voucher(
    ex: &impl SqlExecutor,
    row: &ReturnRow,
    in_id: Uuid,
    in_no: &str,
    lines: &[StockInLine],
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
) -> std::result::Result<Option<Uuid>, ApiError> {
    // 按物料聚合（同一物料多行 → 一行借贷，行数与采购侧 `VoucherMaterialLine::normalize` 同口径）
    let mut per_material: Vec<(Uuid, Decimal, i32, i32)> = Vec::new();
    let mut index_of: HashMap<Uuid, usize> = HashMap::new();
    for line in lines {
        if line.amount <= Decimal::ZERO {
            continue;
        }
        match index_of.get(&line.material_id) {
            Some(&index) => {
                per_material[index].1 += line.amount;
                per_material[index].3 = line.line_no;
            }
            None => {
                index_of.insert(line.material_id, per_material.len());
                per_material.push((line.material_id, line.amount, line.line_no, line.line_no));
            }
        }
    }
    let cost_total: Decimal = per_material.iter().map(|item| item.1).sum();
    if cost_total <= Decimal::ZERO {
        tracing::warn!(
            return_no = %row.return_no,
            in_no = %in_no,
            "退货过账：回库成本合计为 0（零成本存量），跳过红字入库凭证，只回库与流水"
        );
        return Ok(None);
    }
    let inventory = voucher::account_required(ex, voucher::DEFAULT_INVENTORY_CODE, "库存商品")
        .await
        .map_err(|error| utils::db_error(error, "取库存商品科目 1405"))?;
    let cogs = voucher::account_required(ex, ACCOUNT_MAIN_COST, "主营业务成本")
        .await
        .map_err(|error| utils::db_error(error, "取主营业务成本科目 6401"))?;
    let summary = cost_voucher_summary(&row.return_no);

    let mut entries: Vec<EntryLine> = Vec::with_capacity(per_material.len() * 2);
    for (material_id, amount, line_from, line_to) in &per_material {
        let amount = amount.round_dp(4);
        let label = if line_from == line_to {
            format!("第 {line_from} 行")
        } else {
            format!("第 {line_from}–{line_to} 行")
        };
        // 借：库存商品（回库，挂物料 + 仓库辅助核算）
        entries.push(EntryLine {
            line_no: 0,
            summary: format!("{summary} 回库 {label}"),
            account: inventory.clone(),
            debit: amount,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: Some(*material_id),
            aux_warehouse_id: Some(row.warehouse_id),
            aux_dept_id: None,
        });
        // 贷：主营业务成本（冲减，全公司口径不挂仓库）
        entries.push(EntryLine {
            line_no: 0,
            summary: format!("{summary} 冲减成本 {label}"),
            account: cogs.clone(),
            debit: Decimal::ZERO,
            credit: amount,
            aux_supplier_id: None,
            aux_material_id: Some(*material_id),
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.line_no = i32::try_from(index + 1).unwrap_or(0);
    }
    let voucher_id = voucher::post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type: voucher::SOURCE_STOCK_IN,
            source_id: Some(in_id),
            source_no: Some(in_no),
            voucher_date: row.return_date,
            summary: summary.clone(),
            lines: entries,
            remark: Some("销售退货过账：库存回库 + 成本冲减（借 1405 / 贷 6401）"),
            red_of: None,
            status: voucher::STATUS_POSTED,
        },
        operator.id,
        now,
    )
    .await
    .map_err(|error| write_error(error, "生成红字入库凭证"))?;
    // 入库单 → 凭证回链（与采购过账 `link_voucher_and_payable` 同口径：只回写 voucher_id，
    // 退货入库没有应付单）
    ex.exec_sql(
        "UPDATE biz_stock_in SET voucher_id = ?::uuid, updated_at = ? WHERE id = ?::uuid"
            .to_string(),
        vec![
            Value::from(&voucher_id),
            Value::from(now),
            Value::from(&in_id),
        ],
    )
    .await
    .map_err(|error| write_error(error, "回写入库单凭证"))?;
    tracing::info!(
        return_no = %row.return_no,
        in_no = %in_no,
        voucher_id = %voucher_id,
        cost_total = %cost_total.round_dp(2),
        "退货过账：红字入库凭证已生成（借 1405 / 贷 6401）"
    );
    Ok(Some(voucher_id))
}

/// 收入冲减凭证：借 5001 主营业务收入 + 借 2221.02 销项税 / 贷 1122 应收账款。
///
/// **只有在造了红字发票时**才写：收入是开票时确认的（`create_for_sales_revenue`），
/// 未开票的退货在总账里根本没有收入可冲 —— 那时只冲应收台账（明细账），不碰总账。
async fn write_revenue_voucher(
    ex: &impl SqlExecutor,
    row: &ReturnRow,
    invoice_id: Uuid,
    invoice_no: &str,
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
) -> std::result::Result<Option<Uuid>, ApiError> {
    let total = row.total_with_tax.round_dp(4);
    let revenue = row.total_amount.round_dp(4);
    let tax = row.total_tax.round_dp(4);
    if total <= Decimal::ZERO || revenue <= Decimal::ZERO {
        return Ok(None);
    }
    let ar_account = voucher::account_required(ex, ACCOUNT_AR, "应收账款")
        .await
        .map_err(|error| utils::db_error(error, "取应收账款科目 1122"))?;
    let revenue_account = voucher::account_required(ex, ACCOUNT_REVENUE, "主营业务收入")
        .await
        .map_err(|error| utils::db_error(error, "取主营业务收入科目 5001"))?;
    let summary = revenue_voucher_summary(&row.return_no);
    let mut entries = vec![
        // 借：主营业务收入（红冲）
        EntryLine {
            line_no: 1,
            summary: format!("{summary} 冲减收入"),
            account: revenue_account,
            debit: revenue,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        },
    ];
    if tax > Decimal::ZERO {
        let tax_account = voucher::account_required(ex, ACCOUNT_OUTPUT_TAX, "销项税额")
            .await
            .map_err(|error| utils::db_error(error, "取销项税额科目 2221.02"))?;
        entries.push(EntryLine {
            line_no: 0,
            summary: format!("{summary} 冲减销项税"),
            account: tax_account,
            debit: tax,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }
    // 贷：应收账款（价税合计）
    entries.push(EntryLine {
        line_no: 0,
        summary: format!("{summary} 冲减应收"),
        account: ar_account,
        debit: Decimal::ZERO,
        credit: total,
        aux_supplier_id: None,
        aux_material_id: None,
        aux_warehouse_id: None,
        aux_dept_id: None,
    });
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.line_no = i32::try_from(index + 1).unwrap_or(0);
    }
    let voucher_id = voucher::post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type: voucher::SOURCE_INVOICE,
            source_id: Some(invoice_id),
            source_no: Some(invoice_no),
            voucher_date: row.return_date,
            summary: summary.clone(),
            lines: entries,
            remark: Some("销售退货过账：红字发票对应的收入冲减"),
            red_of: None,
            status: voucher::STATUS_POSTED,
        },
        operator.id,
        now,
    )
    .await
    .map_err(|error| write_error(error, "生成收入冲减凭证"))?;
    Ok(Some(voucher_id))
}

// ==================== 写：应收 / 发票 / 订单行 ====================

/// 锁应收单（并发收款 / 反复冲减在这里串行化）。
async fn lock_receivable(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> std::result::Result<Option<ReceivableRow>, ApiError> {
    // SCOPE-EXEMPT: 应收台账无归属列（见 receivable 模块文档）。
    let rows = ex
        .query_sql(
            "SELECT * FROM biz_receivable \
              WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "锁定应收单"))?;
    rows.first()
        .map(|row| ReceivableRow::from_row(row).map_err(|error| utils::db_error(error, "读应收单")))
        .transpose()
}

/// 冲减原应收：改金额与未收，或（全额退且一分没收）作废。
async fn offset_receivable(
    ex: &impl SqlExecutor,
    row: &ReturnRow,
    stock_out_id: Option<Uuid>,
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
) -> std::result::Result<Option<(Uuid, Decimal)>, ApiError> {
    let Some(stock_out_id) = stock_out_id else {
        tracing::info!(
            return_no = %row.return_no,
            "退货过账：原发货单没有销售出库单（未过账），没有应收可冲"
        );
        return Ok(None);
    };
    let Some(ar) = receivable::find_by_source(
        ex,
        crate::business::erp::receivable_bridge::SOURCE_TYPE_SALES_OUT,
        stock_out_id,
    )
    .await
    .map_err(|error| utils::db_error(error, "查询出库单挂的应收"))?
    else {
        tracing::warn!(
            return_no = %row.return_no,
            stock_out_id = %stock_out_id,
            "退货过账：原销售出库单没有挂应收（零元 / 老单），本次不冲应收"
        );
        return Ok(None);
    };
    let locked = lock_receivable(ex, ar.id)
        .await?
        .ok_or_else(|| not_found(format!("应收单 {} 不存在或已删除", ar.id)))?;

    let after = receivable_after(
        &locked.ar_no,
        &locked.status,
        locked.amount,
        locked.received_amount,
        row.total_with_tax,
    )
    .map_err(conflict)?;

    if after.cancel {
        // 全额退 + 一分没收：整张应收作废（复用 SO-12/14 的作废口径，不另写一套）
        receivable::cancel_for_stock_out(
            ex,
            locked.id,
            &format!("销售退货 {} 全额冲减", row.return_no),
            operator.id,
            now,
        )
        .await?;
        tracing::info!(
            return_no = %row.return_no,
            ar_no = %locked.ar_no,
            "退货过账：全额退货 → 应收单已作废（OPEN → CANCELLED）"
        );
        return Ok(Some((locked.id, Decimal::ZERO)));
    }

    let target_status = receivable_status_after(&locked.status, &after);
    if target_status != locked.status {
        state_machine::transition(
            ex,
            state_machine::ENTITY_RECEIVABLE,
            locked.id,
            &locked.ar_no,
            &receivable::LEGAL_EDGES,
            &locked.status,
            target_status,
            operator.clone(),
            None,
            None,
        )
        .await?;
    }
    let note = receivable_note(&row.return_no, row.total_with_tax);
    let remark = match locked.remark.as_deref().map(str::trim) {
        Some(old) if !old.is_empty() => format!("{old}\n{note}"),
        _ => note,
    };
    let settled_at = if target_status == receivable::STATUS_SETTLED {
        Value::from(now)
    } else {
        Value::Null
    };
    let affected = ex
        .exec_sql(
            "UPDATE biz_receivable \
                SET amount = ?::text::numeric, unsettled_amount = ?::text::numeric, \
                    status = ?, settled_at = COALESCE(?, settled_at), \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid, remark = ? \
              WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::Text(after.amount.to_string()),
                Value::Text(after.unsettled.to_string()),
                Value::Text(target_status.to_string()),
                settled_at,
                Value::from(now),
                Value::Text(operator.id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(remark),
                Value::from(&locked.id),
                Value::Text(locked.status.clone()),
            ],
        )
        .await
        .map_err(|error| write_error(error, "冲减应收"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "应收单 {} 的金额在本次过账期间被改动（期望状态 {}），请刷新后重试",
            locked.ar_no, locked.status
        )));
    }
    tracing::info!(
        return_no = %row.return_no,
        ar_no = %locked.ar_no,
        before_amount = %locked.amount.round_dp(2),
        after_amount = %after.amount.round_dp(2),
        after_unsettled = %after.unsettled.round_dp(2),
        status = target_status,
        "退货过账：应收已冲减"
    );
    Ok(Some((locked.id, after.unsettled)))
}

/// 已开票金额（`status <> VOID` 的发票合计；红票也算在内 —— 负数自然抵减）。
async fn invoiced_of(
    ex: &impl SqlExecutor,
    receivable_id: Uuid,
) -> std::result::Result<Decimal, ApiError> {
    invoice::invoiced_total(ex, receivable_id, None)
        .await
        .map_err(|error| utils::db_error(error, "统计应收已开票金额"))
}

/// 造红字发票（`OUT_RED`，金额全负）+ 收入冲减凭证，并把它推到 `ISSUED`。
///
/// 只在「原应收已开过票」时调用：未开票 → 总账里没有收入，不需要红票也不需要收入凭证。
async fn create_red_invoice(
    ex: &impl SqlExecutor,
    row: &ReturnRow,
    receivable_id: Uuid,
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
) -> std::result::Result<Option<(Uuid, String, Option<Uuid>)>, ApiError> {
    let invoiced = invoiced_of(ex, receivable_id).await?;
    if invoiced <= Decimal::ZERO {
        tracing::info!(
            return_no = %row.return_no,
            receivable_id = %receivable_id,
            "退货过账：原应收没有有效发票（未开票 / 全部作废），不造红字发票"
        );
        return Ok(None);
    }
    let Some((amount, tax, total)) =
        red_invoice_amounts(row.total_amount, row.total_tax, row.total_with_tax)
    else {
        tracing::warn!(
            return_no = %row.return_no,
            total_amount = %row.total_amount,
            "退货过账：退货不含税金额为 0，红票金额会撞 ck_biz_invoice_sign，跳过红字发票"
        );
        return Ok(None);
    };
    let invoice_no = voucher::next_doc_no(ex, &invoice::INVOICE_SEQ, now)
        .await
        .map_err(|error| utils::db_error(error, "取红字发票号"))?;
    let tax_rate = invoice_tax_rate(row.total_amount, row.total_tax);
    let operator_text = operator
        .id
        .map(|value| value.to_string())
        .unwrap_or_default();
    let remark = format!(
        "销售退货 {} 红冲（原应收 {}）",
        row.return_no,
        row.total_with_tax.round_dp(2)
    );
    let inserted = ex
        .query_first_sql(
            "INSERT INTO biz_invoice \
                (invoice_no, invoice_type, invoice_code, customer_id, invoice_date, \
                 amount, tax_amount, total_with_tax, tax_rate, status, receivable_id, \
                 source_type, source_no, remark, created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, NULL, ?::uuid, ?::text::date, \
                 ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, ?, \
                 ?::uuid, ?, ?, ?, ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
             RETURNING id"
                .to_string(),
            vec![
                Value::Text(invoice_no.clone()),
                Value::Text(INVOICE_TYPE_OUT_RED.to_string()),
                Value::from(&row.customer_id),
                Value::Text(row.return_date.to_string()),
                Value::Text(amount.to_string()),
                Value::Text(tax.to_string()),
                Value::Text(total.to_string()),
                Value::Text(tax_rate.to_string()),
                Value::Text(invoice::STATUS_DRAFT.to_string()),
                Value::from(&receivable_id),
                Value::Text(INVOICE_SOURCE_SALES_RETURN.to_string()),
                Value::Text(row.return_no.clone()),
                Value::Text(remark),
                Value::from(now),
                Value::from(now),
                Value::Text(operator_text.clone()),
                Value::Text(operator_text.clone()),
            ],
        )
        .await
        .map_err(|error| write_error(error, "生成红字发票"))?
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "红字发票写入失败：INSERT 没有返回 id".to_string(),
            )
        })?;
    let invoice_id: Uuid = inserted
        .get_typed("id")
        .map_err(|error| utils::db_error(error, "读取红字发票 id"))?;

    // 发票的状态机留痕：DRAFT 建单 + DRAFT → ISSUED（与 `invoice::change_status` 同一条边表）
    state_machine::log_created(
        ex,
        invoice::ENTITY,
        invoice_id,
        &invoice_no,
        invoice::STATUS_DRAFT,
        operator.id,
        Some("销售退货红冲票"),
    )
    .await
    .map_err(|error| ApiError::new(error.status, error.code, error.msg))?;
    state_machine::transition(
        ex,
        invoice::ENTITY,
        invoice_id,
        &invoice_no,
        &invoice::LEGAL_EDGES,
        invoice::STATUS_DRAFT,
        invoice::STATUS_ISSUED,
        operator.clone(),
        None,
        Some((state_machine::ENTITY_SALES_RETURN, &row.return_no)),
    )
    .await?;
    let affected = ex
        .exec_sql(
            "UPDATE biz_invoice SET status = ?, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::Text(invoice::STATUS_ISSUED.to_string()),
                Value::from(now),
                Value::Text(operator_text),
                Value::from(&invoice_id),
                Value::Text(invoice::STATUS_DRAFT.to_string()),
            ],
        )
        .await
        .map_err(|error| write_error(error, "红字发票开票改状态"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "红字发票 {invoice_no} 的状态在本次过账期间被改动，请重试"
        )));
    }
    let voucher_id = write_revenue_voucher(ex, row, invoice_id, &invoice_no, operator, now).await?;
    if let Some(voucher_id) = voucher_id {
        ex.exec_sql(
            "UPDATE biz_invoice SET voucher_id = ?::uuid, updated_at = ? WHERE id = ?::uuid"
                .to_string(),
            vec![
                Value::from(&voucher_id),
                Value::from(now),
                Value::from(&invoice_id),
            ],
        )
        .await
        .map_err(|error| write_error(error, "红字发票回写凭证"))?;
    }
    tracing::info!(
        return_no = %row.return_no,
        invoice_no = %invoice_no,
        amount = %amount,
        tax_amount = %tax,
        total_with_tax = %total,
        "退货过账：红字发票已开具（OUT_RED）"
    );
    Ok(Some((invoice_id, invoice_no, voucher_id)))
}

/// 回写订单行已发数量（`delivered_qty -= qty`，数量不足即 409 → 整单回滚）。
///
/// **不改订单状态**（有意为之）：`SALES_ORDER_EDGES` 里没有 `OUTBOUND → PARTIAL_OUT`
/// 这条边 —— 已出库订单退货后不自动「退回部分出库」，那是凭空造边。
/// `delivered_qty` 才是账实相符的真相（未发量 = `qty - delivered_qty` 自然变大），
/// 订单状态由跟单员按业务决定是否关闭（`OUTBOUND → CLOSED` 不存在，需人工处理）。
async fn reduce_delivered(
    ex: &impl SqlExecutor,
    items: &[ReturnItemRow],
    now: DateTime<Utc>,
) -> std::result::Result<usize, ApiError> {
    let mut count = 0_usize;
    for item in items {
        let Some(so_item_id) = item.so_item_id else {
            continue; // 无原单行（OTHER 退货）：没有 delivered_qty 可回写
        };
        let affected = ex
            .exec_sql(
                "UPDATE biz_sales_order_item \
                    SET delivered_qty = delivered_qty - ?::text::numeric, updated_at = ? \
                  WHERE id = ?::uuid AND delivered_qty >= ?::text::numeric"
                    .to_string(),
                vec![
                    Value::Text(item.qty.to_string()),
                    Value::from(now),
                    Value::from(&so_item_id),
                    Value::Text(item.qty.to_string()),
                ],
            )
            .await
            .map_err(|error| utils::db_error(error, "回写订单行已发数量"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "订单行 {so_item_id} 的已发数量不足以扣回本次退货 {}（并发发货 / 数据不自洽）：\
                 本单的库存、凭证与应收已整笔回滚",
                item.qty
            )));
        }
        count += 1;
    }
    Ok(count)
}

/// 回填退货单的三个回链列（入库单 / 应收 / 红字发票）。
async fn link_returns(
    ex: &impl SqlExecutor,
    id: Uuid,
    stock_in_id: Uuid,
    receivable_id: Option<Uuid>,
    red_invoice_id: Option<Uuid>,
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    ex.exec_sql(
        format!(
            "UPDATE {} SET stock_in_id = ?::uuid, \
                    receivable_id = COALESCE(NULLIF(?::text,'')::uuid, receivable_id), \
                    red_invoice_id = COALESCE(NULLIF(?::text,'')::uuid, red_invoice_id), \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            model::TABLE
        ),
        vec![
            Value::from(&stock_in_id),
            Value::Text(receivable_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(red_invoice_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::Text(operator.id.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
    .map_err(|error| write_error(error, "回填退货单过账引用"))?;
    Ok(())
}

// ==================== 主入口 ====================

/// **退货过账的执行体**：调用方必须已经在自己事务里把退货单 CAS 到 `POSTED`。
///
/// 参数 `row` 是**锁后重读**的单头（`lock_return` 的返回值），不是调用方手里那份
/// 快照 —— 并发改单后拿旧快照算账是本模块最不想出现的事故。
pub async fn post_in_tx(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
) -> std::result::Result<ReturnPostOutcome, ApiError> {
    // ① 锁单头（与状态 CAS 同一把锁：并发两次过账，后到的那次在这里排队后被幂等闸拦下）
    let row = lock_return(ex, id)
        .await?
        .ok_or_else(|| not_found("销售退货单不存在或已删除".to_string()))?;
    // 防御性幂等：已回填过入库单 = 已经动过账，绝不再回一次库
    if let Some(existing) = row.stock_in_id {
        return Err(conflict(format!(
            "退货单 {} 已经生成过入库单 {existing}（过账幂等闸）：库存只回一次，\
             重复过账请查单据而不是重退",
            row.return_no
        )));
    }
    let items = model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售退货单明细"))?;
    if items.is_empty() {
        return Err(bad_request(format!(
            "退货单 {} 没有明细，不能过账",
            row.return_no
        )));
    }

    // ② 超退闸重跑（此时本单还没算「已退」，必须 exclude 自己 —— 与提单时同一口径）
    for item in &items {
        let Some(so_item_id) = item.so_item_id else {
            continue;
        };
        model::ensure_qty_within_returnable(
            ex,
            row.shipment_id,
            so_item_id,
            item.qty,
            Some(row.id),
        )
        .await
        .map_err(|error| match error {
            DbError::InvalidValue(message) => conflict(format!(
                "退货单 {} 过账前复核超退失败：{message}",
                row.return_no
            )),
            other => utils::db_error(other, "复核退货可退量"),
        })?;
    }

    // ③ 期间闸（凭证日期 = 退货日期，已结账的月份不许再落账）
    let period_code = voucher::period_of(ex, row.return_date)
        .await
        .map_err(|error| utils::db_error(error, "解析退货日期所属会计期间"))?;
    period::ensure_period_writable(ex, &period_code, "销售退货过账").await?;

    // ④ 原发货单 / 出库单（决定成本来源与应收台账）
    let (stock_out_id, ship_no) = match row.shipment_id {
        Some(shipment_id) => match shipment_out(ex, shipment_id).await? {
            Some(found) => found,
            None => {
                return Err(not_found(format!(
                    "退货单 {} 关联的原发货单 {shipment_id} 不存在或已删除",
                    row.return_no
                )));
            }
        },
        None => (None, "（无原发货单）".to_string()),
    };

    // ⑤ 逐行算成本（原出库成本优先 → 账面均价 → 0）与物料信息
    let mut lines: Vec<StockInLine> = Vec::with_capacity(items.len());
    let mut cost_total = Decimal::ZERO;
    for (index, item) in items.iter().enumerate() {
        let info = stock_out::material_info(ex, item.material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询退货明细的物料"))?
            .ok_or_else(|| {
                bad_request(format!(
                    "退货明细引用的物料不存在或已删除：{}",
                    item.material_id
                ))
            })?;
        if info.serial_required {
            return Err(bad_request(format!(
                "物料 {} 是一物一码物料，退货回库要走序列号状态流转（本卡只做普通物料回库）",
                info.label()
            )));
        }
        let mut unit_cost = match stock_out_id {
            Some(stock_out_id) => original_unit_cost(ex, stock_out_id, item.material_id).await?,
            None => None,
        };
        if unit_cost.is_none() {
            unit_cost = ledger_avg_cost(ex, item.material_id, row.warehouse_id).await?;
        }
        let unit_cost = unit_cost.unwrap_or(Decimal::ZERO);
        if unit_cost <= Decimal::ZERO {
            tracing::warn!(
                return_no = %row.return_no,
                material_id = %item.material_id,
                "退货过账：查不到成本单价（原出库流水与账面均价都没有），按 0 成本回库"
            );
        }
        let amount = cost_amount_of(item.qty, unit_cost);
        cost_total += amount;
        lines.push(StockInLine {
            line_no: i32::try_from(index + 1).unwrap_or(0),
            material_id: item.material_id,
            unit_id: info.unit_id,
            qty: item.qty,
            unit_cost,
            amount,
            so_item_id: item.so_item_id,
            location_dict_id: item.location_id,
            remark: item.remark.clone(),
        });
    }

    // ⑥ 建退货入库单（POSTED）
    let (stock_in_id, stock_in_no) = create_stock_in(ex, &row, &lines, operator, now).await?;
    post_inventory(ex, &row, stock_in_id, &stock_in_no, &lines, operator, now).await?;
    tracing::info!(
        return_no = %row.return_no,
        in_no = %stock_in_no,
        ship_no = %ship_no,
        lines = lines.len(),
        cost_total = %cost_total.round_dp(2),
        "退货过账：入库单与三本账（成本 / 库位 / 流水）已同事务落库"
    );

    // ⑦ 红字入库凭证（借 1405 / 贷 6401）
    let cost_voucher_id =
        write_cost_voucher(ex, &row, stock_in_id, &stock_in_no, &lines, operator, now).await?;

    // ⑧ 冲减应收（无原出库单 / 没挂应收 → 跳过）
    let (receivable_id, receivable_unsettled) =
        match offset_receivable(ex, &row, stock_out_id, operator, now).await? {
            Some((id, unsettled)) => (Some(id), Some(unsettled)),
            None => (None, None),
        };

    // ⑨ 已开票 → 红字发票 + 收入冲减凭证
    let (red_invoice_id, red_invoice_no, revenue_voucher_id) = match receivable_id {
        Some(receivable_id) => {
            match create_red_invoice(ex, &row, receivable_id, operator, now).await? {
                Some((id, no, voucher_id)) => (Some(id), Some(no), voucher_id),
                None => (None, None, None),
            }
        }
        None => (None, None, None),
    };

    // ⑩ 回写订单行已发数量（delivered_qty 减少 = 释放客户占用）
    let delivered_lines = reduce_delivered(ex, &items, now).await?;

    // ⑪ 回填退货单三个回链列
    link_returns(
        ex,
        id,
        stock_in_id,
        receivable_id,
        red_invoice_id,
        operator,
        now,
    )
    .await?;

    tracing::info!(
        return_no = %row.return_no,
        in_no = %stock_in_no,
        receivable = ?receivable_id,
        red_invoice = ?red_invoice_no,
        delivered_lines,
        "退货过账完成：回库 / 红字入库凭证 / 应收冲减 / 红字发票 / delivered_qty 同一事务落库"
    );
    Ok(ReturnPostOutcome {
        stock_in_id,
        stock_in_no,
        cost_total: cost_total.round_dp(4),
        cost_voucher_id,
        receivable_id,
        receivable_unsettled,
        red_invoice_id,
        red_invoice_no,
        revenue_voucher_id,
        delivered_lines,
    })
}

/// 退货日期 → 会计期间（供 handler 回显 / 单测断言，避免两处各算一遍）。
pub fn period_code_of(date: NaiveDate) -> String {
    date.format("%Y-%m").to_string()
}

// ==================== 单测（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// 定点字面量（浮点会丢精度，一律走字符串）。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    /// ① 入库单事由 / 凭证摘要 / 应收备注都要能反查退货单号（对账靠它）。
    #[test]
    fn labels_carry_return_no() {
        assert_eq!(
            stock_in_reason("SR-20261107-000001"),
            "销售退货 SR-20261107-000001"
        );
        assert!(cost_voucher_summary("SR-1").contains("SR-1"));
        assert!(cost_voucher_summary("SR-1").contains("成本冲减"));
        assert!(revenue_voucher_summary("SR-1").contains("收入冲减"));
        let note = receivable_note("SR-1", dec("113"));
        assert!(note.contains("SR-1"), "{note}");
        assert!(note.contains("113"), "{note}");
    }

    /// ② 成本金额 = 数量 × 单价（4 位），单价不在这里决定。
    #[test]
    fn cost_amount_is_qty_times_unit_cost() {
        // 3 × 66.666667 = 200.000001 → 4 位四舍五入 = 200.0000
        assert_eq!(cost_amount_of(dec("3"), dec("66.666667")), dec("200"));
        assert_eq!(cost_amount_of(dec("10"), dec("60")), dec("600"));
        assert_eq!(cost_amount_of(dec("0.5"), dec("12.345")), dec("6.1725"));
        // 零成本存量：数量照回，金额 0（凭证会跳过）
        assert_eq!(cost_amount_of(dec("5"), Decimal::ZERO), Decimal::ZERO);
    }

    /// ③ 加权单价：金额 / 数量（6 位）；数量 ≤ 0 一律 `None`（不做除零）。
    #[test]
    fn unit_cost_division_guards_zero() {
        assert_eq!(unit_cost_of(dec("600"), dec("10")), Some(dec("60.000000")));
        assert_eq!(unit_cost_of(dec("100"), dec("3")), Some(dec("33.333333")));
        assert_eq!(unit_cost_of(Decimal::ZERO, Decimal::ZERO), None);
        assert_eq!(unit_cost_of(dec("10"), dec("-1")), None);
    }

    /// ④ 部分退货：应收金额与未收同额减少，状态不越级。
    #[test]
    fn receivable_partial_offset() {
        let after = receivable_after(
            "AR-1",
            receivable::STATUS_OPEN,
            dec("1130"),
            Decimal::ZERO,
            dec("339"),
        )
        .expect("部分退货应放行");
        assert_eq!(after.amount, dec("791"));
        assert_eq!(after.unsettled, dec("791"));
        assert!(!after.cancel && !after.settled);
        assert_eq!(
            receivable_status_after(receivable::STATUS_OPEN, &after),
            receivable::STATUS_OPEN
        );
        // 一分没收就是 OPEN，不许因为冲过一笔就变成 PARTIAL（PARTIAL 的语义是「收过钱」）
        assert_eq!(
            receivable_status_after(receivable::STATUS_PARTIAL, &after),
            receivable::STATUS_PARTIAL,
            "已部分收款的应收冲减后仍是 PARTIAL"
        );
    }

    /// ⑤ 全额退货 + 一分没收 → 整张应收作废（`OPEN → CANCELLED`，只有这条边）。
    #[test]
    fn receivable_full_offset_cancels_when_unpaid() {
        let after = receivable_after(
            "AR-1",
            receivable::STATUS_OPEN,
            dec("1130"),
            Decimal::ZERO,
            dec("1130"),
        )
        .expect("全额退货应放行");
        assert!(after.cancel, "全额退货必须走作废分支");
        assert_eq!(after.amount, Decimal::ZERO);
        assert_eq!(
            receivable_status_after(receivable::STATUS_OPEN, &after),
            receivable::STATUS_CANCELLED
        );
        // 收过钱的整单退货不许作废：退货额 1130 > 未收 1030（应收 1130 − 已收 100）
        let err = receivable_after(
            "AR-1",
            receivable::STATUS_PARTIAL,
            dec("1130"),
            dec("100"),
            dec("1130"),
        )
        .unwrap_err();
        assert!(err.contains("收款退款单"), "{err}");
    }

    /// ⑥ 退货金额超过未收：409 文案要点名「已收多少 / 未收多少 / 走退款单」。
    #[test]
    fn receivable_rejects_over_unsettled() {
        let err = receivable_after(
            "AR-9",
            receivable::STATUS_PARTIAL,
            dec("1130"),
            dec("1000"),
            dec("339"),
        )
        .unwrap_err();
        assert!(err.contains("AR-9"), "{err}");
        assert!(err.contains("未收金额"), "{err}");
        assert!(err.contains("收款退款单"), "{err}");
    }

    /// ⑦ 已作废 / 已核销的应收不能再冲；零元退货直接拒。
    #[test]
    fn receivable_rejects_terminal_states_and_zero() {
        for status in [receivable::STATUS_CANCELLED, receivable::STATUS_WRITTEN_OFF] {
            let err =
                receivable_after("AR-1", status, dec("100"), Decimal::ZERO, dec("10")).unwrap_err();
            assert!(err.contains("AR-1"), "{err}");
        }
        assert!(
            receivable_after(
                "AR-1",
                receivable::STATUS_OPEN,
                dec("100"),
                Decimal::ZERO,
                Decimal::ZERO
            )
            .is_err()
        );
        assert!(
            receivable_after(
                "AR-1",
                receivable::STATUS_OPEN,
                dec("100"),
                Decimal::ZERO,
                dec("-1")
            )
            .is_err()
        );
    }

    /// ⑧ 冲减后恰好收清 → SETTLED（`PARTIAL → SETTLED` 是合法边）。
    #[test]
    fn receivable_offset_can_settle() {
        // 应收 1000，已收 400，退货 600 → amount 400 = received 400
        let after = receivable_after(
            "AR-1",
            receivable::STATUS_PARTIAL,
            dec("1000"),
            dec("400"),
            dec("600"),
        )
        .expect("冲减后正好收清");
        assert_eq!(after.amount, dec("400"));
        assert_eq!(after.unsettled, Decimal::ZERO);
        assert!(after.settled && !after.cancel);
        assert_eq!(
            receivable_status_after(receivable::STATUS_PARTIAL, &after),
            receivable::STATUS_SETTLED
        );
    }

    /// ⑨ 红字发票三金额全负，且价税恒等；零金额（会撞 `ck_biz_invoice_sign`）一律不造票。
    #[test]
    fn red_invoice_amounts_are_negative_and_balanced() {
        let (amount, tax, total) =
            red_invoice_amounts(dec("1000"), dec("130"), dec("1130")).expect("正常红票");
        assert_eq!(amount, dec("-1000"));
        assert_eq!(tax, dec("-130"));
        assert_eq!(total, dec("-1130"));
        assert!(
            amount < Decimal::ZERO && tax <= Decimal::ZERO,
            "OUT_RED 的符号约束"
        );
        assert_eq!(total, amount + tax, "ck_biz_invoice_amount 恒等");

        // 不含税金额为 0（纯税额退货 / 零价退货）→ 不造票
        assert!(red_invoice_amounts(Decimal::ZERO, dec("10"), dec("10")).is_none());
        // 负税（脏数据）→ 不造票（OUT_RED 要求 tax_amount <= 0，负负得正会撞 CHECK）
        assert!(red_invoice_amounts(dec("100"), dec("-13"), dec("87")).is_none());
        // 价税不恒等（脏数据）→ 不造票
        assert!(red_invoice_amounts(dec("100"), dec("13"), dec("120")).is_none());
    }

    /// ⑩ 红票税率按「税额 / 不含税金额」反算（0–1），并夹在合法区间。
    #[test]
    fn invoice_tax_rate_is_fraction_and_clamped() {
        assert_eq!(invoice_tax_rate(dec("1000"), dec("130")), dec("0.13"));
        assert_eq!(invoice_tax_rate(dec("100"), dec("6")), dec("0.06"));
        assert_eq!(invoice_tax_rate(Decimal::ZERO, dec("13")), Decimal::ZERO);
        // 脏数据把税率算成 > 1 → 夹到 1（否则 ck_biz_invoice_tax_rate 会炸）
        assert_eq!(invoice_tax_rate(dec("10"), dec("30")), Decimal::ONE);
    }

    /// ⑪ 信用额度释放：已过账退货从在手占用里抵减，抵减到底不出负数。
    #[test]
    fn credit_release_never_goes_negative() {
        use crate::business::erp::sales_order_model::credited_after_returns;
        assert_eq!(
            credited_after_returns(dec("2429.5"), dec("339")),
            dec("2090.5")
        );
        assert_eq!(
            credited_after_returns(dec("100"), dec("100")),
            Decimal::ZERO
        );
        assert_eq!(
            credited_after_returns(dec("100"), dec("130")),
            Decimal::ZERO,
            "退货比在手订单还多时不能给客户负占用（那等于白送额度）"
        );
    }

    /// ⑫ 常量口径：入库性质 / 来源 / 流水业务类型 / 红票票种，一字不改地钉死。
    #[test]
    fn constants_match_database_domains() {
        // 入库性质仍在既有域值里（ck_biz_si_type 只认 PURCHASE/RETURN/OTHER）
        assert_eq!(IN_TYPE_RETURN, "RETURN");
        assert_ne!(
            IN_TYPE_RETURN,
            crate::business::erp::stockin_base::IN_TYPE_PURCHASE
        );
        // 来源类型不是 in_type（两个维度，不许互相替代）
        assert_eq!(SOURCE_TYPE_SALES_RETURN, "SALES_RETURN");
        assert_ne!(SOURCE_TYPE_SALES_RETURN, IN_TYPE_RETURN);
        // 流水业务类型与收货 / 调拨同族（VARCHAR(24) 无 CHECK，但词表要一致）
        assert_eq!(BIZ_TYPE_SALES_RETURN_IN, "SALES_RETURN_IN");
        assert_eq!(DOC_TYPE_STOCK_IN, "STOCK_IN");
        assert_eq!(TXN_TYPE_IN, "IN");
        // 红票票种在发票域里，且发票状态机的入口态叫 DRAFT
        assert_eq!(INVOICE_TYPE_OUT_RED, "OUT_RED");
        assert!(invoice::STATUSES.contains(&invoice::STATUS_ISSUED));
        assert!(
            invoice::LEGAL_EDGES.contains(&(invoice::STATUS_DRAFT, invoice::STATUS_ISSUED)),
            "红票建单后推到 ISSUED 必须有这条边"
        );
        // 退货入库的序列号来源映射不需要改（RETURN → RETURN）
        assert_eq!(
            crate::business::erp::stockin_base::serial_source_of_in_type(IN_TYPE_RETURN),
            Some("RETURN")
        );
    }

    /// ⑬ 会计期间码（与 `voucher::period_of` 同格式，`yyyy-MM`）。
    #[test]
    fn period_code_format() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 7).expect("合法日期");
        assert_eq!(period_code_of(date), "2026-11");
        assert_eq!(period_code_of(date).len(), 7);
    }
}
