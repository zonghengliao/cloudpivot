//! 采购退货**确认过账**（P2-03）：出库回冲 + 应付红字 + 供应商应退款 + 冲销凭证。
//!
//! # 一个事务里做完六件事（顺序不能换）
//!
//! ```text
//! ①  期间闸（退货日期所在会计期间必须未结账）
//! ②  重跑超退守卫（exclude 本单：本单数量已落库，不排除就会「自己把自己算进已退」）
//! ③  建出库单 biz_stock_out（out_type=OTHER + source_type=PURCHASE_RETURN）
//!     + 明细 → 三本账减法（成本账 / 库位账 / 流水，唯一写入口在 inventory_writer）
//! ④  冲减应付：只冲未付部分；已付部分转 `biz_supplier_receivable`（供应商应退款）
//! ⑤  冲销凭证：借 2202 应付账款 / 贷 1405 库存商品 + 贷 2221.01 进项税转出
//!     （成本与不含税金额的差额进 6401 主营业务成本，保证借贷精确平衡）
//! ⑥  已收数量：`POSTED_RECEIVED_SQL` 已减去已确认退货（P2-03），
//!     采购单状态按需从 RECEIVED 回退到 PARTIAL_RECEIVED（由 handler 在状态日志里留痕）
//! ```
//!
//! # 为什么「冲减应付」只冲未付部分
//!
//! `ck_biz_payable_paid CHECK (paid_amount <= amount)` 是物理下限；而且钱已经付出去
//! 这件事不会因为退货而消失 —— 它变成「供应商欠我们」，必须单独挂账
//! （否则就是账外债权）。纯函数 [`crate::business::erp::purchase_return_dto::payable_after`]
//! 把这条口径钉死（含 3 个单测）。
//!
//! # 三本账只复用，不重写
//!
//! 与出库单过账同源：`stockout_model::{lock_bucket, deduct_cost, lock_location_row,
//! deduct_location_qty}` + `inventory_writer::insert_txn`。本文件**没有任何**
//! 直接写 `biz_inventory` / `biz_inventory_location` / `biz_inventory_txn` 的 SQL
//! （`scripts/verify/inventory_write_gate.py` 会在 CI 里验这一点）。
//!
//! # 原子性
//!
//! 全部动作跑在调用方（`purchase_return_handler::confirm_return`）的事务里：
//! 任何一步 Err 由调用方 rollback，库存 / 库位账 / 流水 / 出库单 / 应付 / 应退款 /
//! 凭证 / 状态一起消失。本文件自己不 commit，也不该 commit。
//!
//! # 幂等（三道闸）
//!
//! ① 退货单状态 CAS（`APPROVED → CONFIRMED`，第二次 0 行）；
//! ② `biz_stock_out` 的部分唯一索引 `uk_biz_so_source (source_type, source_id)`；
//! ③ `biz_supplier_receivable` 的 `uk_biz_sr_source (source_type, source_id)`。

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::form_snapshot;
use crate::business::erp::inventory_writer::{self, TxnRecord};
use crate::business::erp::period;
use crate::business::erp::purchase_return_dto::{self as dto, PayableAfter, STATUS_CONFIRMED};
use crate::business::erp::purchase_return_model::{
    self as model, ConfirmLinks, ItemRow, NewSupplierReceivable, ReturnRow,
};
use crate::business::erp::stockin_base::InventorySnapshot;
use crate::business::erp::stockout_base;
use crate::business::erp::stockout_model as stock_out;
use crate::business::erp::voucher::{self, ACCOUNT_MAIN_COST, AccountRef, EntryLine, PostVoucher, VOUCHER_TYPE_AUTO};
use crate::business::shared::doc_kind::TxnBizType;
use crate::shared::error_code;
use crate::utils;

// ==================== 常量 ====================

/// 出库单的 `out_type`：`OTHER`（四值域 ISSUE/SCRAP/LOSS/OTHER 里没有「退货」，
/// 与销售出库同一决策：**来源**由 `source_type` 表达，`out_type` 只表达领用性质）。
pub const STOCK_OUT_OUT_TYPE: &str = stockout_base::OUT_TYPE_OTHER;

/// 出库单的 `source_type`：采购退货（V0332 起本表写入）。
pub const STOCK_OUT_SOURCE_TYPE: &str = "PURCHASE_RETURN";

/// 库存流水的 `biz_type`：`PURCHASE_RETURN`（词表 `TxnBizType::PurchaseReturn`，
/// V0332 之前就已在词表里，本卡只是第一个真正写它的模块）。
pub const TXN_BIZ_TYPE: &str = TxnBizType::PurchaseReturn.as_str();

/// 出库单事由（列表 / 流水一眼能看出这是采购退货）。
pub fn stock_out_reason(return_no: &str) -> String {
    format!("采购退货 {return_no}")
}

/// 凭证摘要（供应商取**名称**：摘要里不许出现 UUID，AGENTS.md 约定 13）。
pub fn voucher_summary(return_no: &str, in_no: &str, supplier: &str) -> String {
    format!("采购退货 {return_no} 供应商 {supplier}（原入库单 {in_no}）冲减应付与存货")
}

/// 期间码（`yyyy-MM`）。
pub fn period_code_of(date: NaiveDate) -> String {
    date.format("%Y-%m").to_string()
}

// ==================== 凭证分录（纯函数，单测钉死借贷平衡）====================

/// 凭证的三项金额依据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoucherBasis {
    /// 冲减的应付（价税合计，借 2202）。
    pub with_tax: Decimal,
    /// 进项税转出（贷 2221.01）。
    pub tax: Decimal,
    /// 存货实际出库成本（贷 1405；来自三本账的真实扣减额，不是 `qty × price`）。
    pub cost: Decimal,
}

impl VoucherBasis {
    /// 成本与「不含税金额」的差额：按物料计价差异处理（进 6401 主营业务成本）。
    ///
    /// 正数 = 账上成本高于不含税采购价（贷方多出来 ⇒ 要补一笔借方）；
    /// 负数 = 成本低于不含税价（借方多出来 ⇒ 要补一笔贷方）。
    pub fn cost_variance(&self) -> Decimal {
        let excl = self.with_tax - self.tax;
        (self.cost - excl).round_dp(4)
    }
}

/// 凭证要用的四个科目（收成结构体：`voucher_lines` 的参数从 9 个降到 6 个，
/// 且调用点自解释「哪一行是哪个科目」）。
#[derive(Debug, Clone)]
pub struct VoucherAccounts {
    /// 借：应付账款（2202）。
    pub payable: AccountRef,
    /// 贷：库存商品（1405）。
    pub inventory: AccountRef,
    /// 贷：应交税费-进项税额转出（2221.01；系统没有该科目时为 `None`）。
    pub tax: Option<AccountRef>,
    /// 借/贷：主营业务成本（6401，只用于计价差异行）。
    pub cost: AccountRef,
}

/// 构造冲销凭证的分录（**唯一实现**，确认过账与单测共用）。
///
/// ```text
/// 借 2202 应付账款          with_tax（挂供应商辅助核算）
/// 贷 1405 库存商品          cost
/// 贷 2221.01 应交税费-进项税额转出  tax
/// 借/贷 6401 主营业务成本   |cost − (with_tax − tax)|   ← 只在有差额时出现
/// ```
///
/// 借贷平衡由构造保证（差额行把两边拉平）；`post_voucher` 还会再校验一次
/// （借贷合计不等直接拒写），两道口径一致。
pub fn voucher_lines(
    return_no: &str,
    in_no: &str,
    supplier_id: Uuid,
    warehouse_id: Uuid,
    basis: VoucherBasis,
    accounts: &VoucherAccounts,
) -> Vec<EntryLine> {
    let payable = &accounts.payable;
    let inventory = &accounts.inventory;
    let tax_account = accounts.tax.as_ref();
    let cost_account = &accounts.cost;
    let mut lines: Vec<EntryLine> = Vec::with_capacity(4);
    lines.push(EntryLine {
        line_no: 0,
        summary: format!("{return_no} 冲减应付（原入库单 {in_no}）"),
        account: payable.clone(),
        debit: basis.with_tax,
        credit: Decimal::ZERO,
        aux_supplier_id: Some(supplier_id),
        aux_material_id: None,
        aux_warehouse_id: None,
        aux_dept_id: None,
    });
    let cost = basis.cost.max(Decimal::ZERO);
    if cost > Decimal::ZERO {
        lines.push(EntryLine {
            line_no: 0,
            summary: format!("{return_no} 冲减存货（退货出库）"),
            account: inventory.clone(),
            debit: Decimal::ZERO,
            credit: cost,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: Some(warehouse_id),
            aux_dept_id: None,
        });
    }
    if basis.tax > Decimal::ZERO
        && let Some(tax_account) = tax_account
    {
        lines.push(EntryLine {
            line_no: 0,
            summary: format!("{return_no} 进项税额转出"),
            account: tax_account.clone(),
            debit: Decimal::ZERO,
            credit: basis.tax,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }
    let variance = basis.cost_variance();
    if variance > Decimal::ZERO {
        // 账上成本高于不含税采购价：借方补一笔（把多贷的成本拉平）
        lines.push(EntryLine {
            line_no: 0,
            summary: format!("{return_no} 出库成本高于采购价（计价差异）"),
            account: cost_account.clone(),
            debit: variance,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    } else if variance < Decimal::ZERO {
        lines.push(EntryLine {
            line_no: 0,
            summary: format!("{return_no} 出库成本低于采购价（计价差异）"),
            account: cost_account.clone(),
            debit: Decimal::ZERO,
            credit: -variance,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }
    for (index, line) in lines.iter_mut().enumerate() {
        line.line_no = i32::try_from(index + 1).unwrap_or(0);
    }
    lines
}

/// 分录借贷合计（单测与日志共用）。
pub fn totals_of_lines(lines: &[EntryLine]) -> (Decimal, Decimal) {
    let mut debit = Decimal::ZERO;
    let mut credit = Decimal::ZERO;
    for line in lines {
        debit += line.debit;
        credit += line.credit;
    }
    (debit.round_dp(4), credit.round_dp(4))
}

// ==================== 确认结果 ====================

/// 确认过账的结果（handler 用来写日志 / 回写状态）。
#[derive(Debug, Clone)]
pub struct PostedReturn {
    /// 出库单 id / 单号。
    pub stock_out_id: Uuid,
    /// 出库单号。
    pub stock_out_no: String,
    /// 出库成本合计（三本账真实扣减额）。
    pub cost_total: Decimal,
    /// 被冲减的应付。
    pub payable_id: Option<Uuid>,
    /// 冲减后应付金额（`None` = 没有可冲的应付）。
    pub payable_amount_after: Option<Decimal>,
    /// 生成的供应商应退款（已付款部分）。
    pub supplier_receivable_id: Option<Uuid>,
    /// 供应商应退款单号。
    pub supplier_receivable_no: Option<String>,
    /// 供应商应退款金额。
    pub supplier_receivable_amount: Decimal,
    /// 凭证 id / 号。
    pub voucher_id: Uuid,
    /// 凭证号。
    pub voucher_no: String,
}

/// 应付快照（冲减前的锁读结果）。
#[derive(Debug, Clone)]
struct PayableSnapshot {
    id: Uuid,
    payable_no: String,
    amount: Decimal,
    paid_amount: Decimal,
    status: String,
}

// ==================== 主流程 ====================

/// 确认退货（**调用方的事务**；不 commit）。
///
/// `row` 必须是 `APPROVED`（handler 已锁读并校验）；`items` 是本单明细。
pub async fn confirm_tx(
    ex: &impl SqlExecutor,
    row: &ReturnRow,
    items: &[ItemRow],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<PostedReturn, ApiError> {
    if items.is_empty() {
        return Err(bad_request(format!("退货单 {} 没有明细，不能确认", row.return_no)));
    }
    // ① 期间闸：已结账的月份不能动账（凭证写入也会拦，这里提前给人话）
    let period_code = period_code_of(row.return_date);
    period::ensure_period_writable(ex, &period_code, "确认采购退货").await?;

    // ② 重跑超退守卫（exclude 本单：本单数量已落库）
    for item in items {
        let returned = model::returned_qty_of(ex, item.stock_in_item_id, Some(row.id))
            .await
            .map_err(|error| utils::db_error(error, "统计已退数量"))?;
        dto::check_returnable(
            item.qty,
            item.received_qty,
            returned,
            &format!("第 {} 行（物料 {}）", item.line_no, item.material_id),
        )?;
    }

    // ③ 出库单 + 三本账
    let stock_out_no = voucher::next_doc_no(ex, &stockout_base::STOCK_OUT_SEQ, now)
        .await
        .map_err(|error| utils::db_error(error, "取出库单号"))?;
    let stock_out_id = Uuid::now_v7();
    ex.exec_sql(
        "INSERT INTO biz_stock_out \
             (id, out_no, out_type, warehouse_id, out_date, status, total_qty, total_amount, \
              source_type, source_id, reason, remark, create_by, update_by, created_at, updated_at) \
         VALUES (?::uuid, ?, ?, ?::uuid, NULLIF(?::text,'')::date, 'DRAFT', 0, 0, ?, ?::uuid, ?, ?, \
              NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, ?, ?)"
            .to_string(),
        vec![
            Value::from(&stock_out_id),
            Value::from(stock_out_no.as_str()),
            Value::from(STOCK_OUT_OUT_TYPE),
            Value::from(&row.warehouse_id),
            Value::Text(row.return_date.to_string()),
            Value::from(STOCK_OUT_SOURCE_TYPE),
            Value::from(&row.id),
            Value::from(stock_out_reason(&row.return_no).as_str()),
            Value::Text(row.remark.clone().unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::from(now),
        ],
    )
    .await
    .map_err(|error| utils::db_error(error, "建采购退货出库单"))?;

    let mut cost_total = Decimal::ZERO;
    for item in items {
        let item_sql = "INSERT INTO biz_stock_out_item \
                 (id, stock_out_id, line_no, material_id, unit_id, qty, unit_cost, amount, \
                  location_id, remark, created_at, updated_at) \
             VALUES (?::uuid, ?::uuid, ?, ?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, 0, 0, \
                  NULLIF(?::text,'')::uuid, ?, ?, ?)"
            .to_string();
        let item_id = Uuid::now_v7();
        ex.exec_sql(
            item_sql,
            vec![
                Value::from(&item_id),
                Value::from(&stock_out_id),
                Value::from(item.line_no),
                Value::from(&item.material_id),
                Value::from(&item.unit_id),
                Value::Text(item.qty.to_string()),
                Value::Text(item.location_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(format!("采购退货 {}", item.line_no)),
                Value::from(now),
                Value::from(now),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "建退货出库明细"))?;

        // 成本账（负库存闸）
        let Some(bucket) = stock_out::lock_bucket(ex, item.material_id, row.warehouse_id, item.location_id)
            .await
            .map_err(|error| utils::db_error(error, "锁定库存行"))?
        else {
            return Err(negative_stock(format!(
                "库存不足，退货确认已拒绝：物料 {} 在退货仓库没有库存行（可退数量与在库数量不一致）。\
                 请先核对库存（可用库存巡检），或把该行退货数量改小 / 换到有货的仓库",
                material_label(item)
            )));
        };
        if bucket.qty < item.qty {
            return Err(negative_stock(format!(
                "库存不足，退货确认已拒绝：物料 {} 账面只有 {}，本单要退 {}。\
                 货可能已通过其它单据出库（领用 / 调拨 / 另一张退货单），\
                 请先核对即时库存再确认退货",
                material_label(item),
                bucket.qty.round_dp(4),
                item.qty.round_dp(4)
            )));
        }
        let applied = stock_out::deduct_cost(ex, &bucket, item.qty, now)
            .await
            .map_err(|error| utils::db_error(error, "扣减成本账"))?;

        // 库位账（与成本账同事务同步减）
        let Some((location_row_id, location_qty)) =
            stock_out::lock_location_row(ex, item.material_id, row.warehouse_id, item.location_id)
                .await
                .map_err(|error| utils::db_error(error, "锁定库位账行"))?
        else {
            return Err(negative_stock(format!(
                "物料 {} 的库位账缺行（明细没指定库位且该仓没有未分配库位桶）：\
                 账账已经不一致，请先跑库存巡检修复库位账再确认退货",
                material_label(item)
            )));
        };
        if location_qty < item.qty {
            return Err(negative_stock(format!(
                "库存不足，退货确认已拒绝：物料 {} 该库位账只有 {}，本单要退 {}",
                material_label(item),
                location_qty.round_dp(4),
                item.qty.round_dp(4)
            )));
        }
        stock_out::deduct_location_qty(ex, location_row_id, item.qty, now)
            .await
            .map_err(|error| utils::db_error(error, "扣减库位账"))?;

        // 流水（OUT，数量恒正；before/after 用回读的真实值）
        inventory_writer::insert_txn(
            ex,
            &TxnRecord {
                material_id: item.material_id,
                warehouse_id: row.warehouse_id,
                txn_type: stockout_base::TXN_TYPE_OUT,
                biz_type: TXN_BIZ_TYPE,
                doc_type: stockout_base::DOC_TYPE_STOCK_OUT,
                doc_id: stock_out_id,
                doc_no: &stock_out_no,
                line_no: item.line_no,
                qty: item.qty.round_dp(4),
                unit_cost: bucket.avg_cost,
                amount: applied.amount,
                before: InventorySnapshot {
                    qty: bucket.qty,
                    amount: bucket.amount,
                    avg_cost: bucket.avg_cost,
                },
                after: InventorySnapshot {
                    qty: applied.after_qty,
                    amount: applied.after_amount,
                    avg_cost: applied.after_cost,
                },
                operator,
                period_code: Some(&period_code),
                remark: Some("采购退货出库"),
                now,
            },
        )
        .await
        .map_err(|error| utils::db_error(error, "写库存流水"))?;

        // 明细回写成本快照（作废时按它原路退回）
        ex.exec_sql(
            "UPDATE biz_stock_out_item SET unit_cost = NULLIF(?::text,'')::numeric, \
                    amount = NULLIF(?::text,'')::numeric, updated_at = ? \
              WHERE id = ?::uuid"
                .to_string(),
            vec![
                Value::Text(bucket.avg_cost.round_dp(6).to_string()),
                Value::Text(applied.amount.round_dp(4).to_string()),
                Value::from(now),
                Value::from(&item_id),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "回写出库明细成本"))?;
        cost_total += applied.amount;
    }
    cost_total = cost_total.round_dp(4);

    // 出库单：合计 + 已过账
    ex.exec_sql(
        "UPDATE biz_stock_out SET total_qty = NULLIF(?::text,'')::numeric, \
                total_amount = NULLIF(?::text,'')::numeric, status = 'POSTED', \
                posted_at = ?, posted_by = NULLIF(?::text,'')::uuid, updated_at = ? \
          WHERE id = ?::uuid"
            .to_string(),
        vec![
            Value::Text(row.total_qty.to_string()),
            Value::Text(cost_total.to_string()),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::from(&stock_out_id),
        ],
    )
    .await
    .map_err(|error| utils::db_error(error, "回写出库单合计"))?;

    // ④ 冲减应付（只冲未付；已付部分转供应商应退款）
    let mut payable_id: Option<Uuid> = None;
    let mut payable_amount_after: Option<Decimal> = None;
    let mut to_supplier_receivable = row.total_with_tax;
    if let Some(payable) = lock_payable(ex, row)
        .await
        .map_err(|error| utils::db_error(error, "锁定应付单"))?
    {
        if payable.status == "CANCELLED" {
            tracing::warn!(
                return_no = %row.return_no,
                payable_no = %payable.payable_no,
                "退货确认：原应付单已作废，退货金额全额转供应商应退款"
            );
        } else {
            let after: PayableAfter = dto::payable_after(payable.amount, payable.paid_amount, row.total_with_tax);
            ex.exec_sql(
                "UPDATE biz_payable SET amount = NULLIF(?::text,'')::numeric, \
                        unpaid_amount = NULLIF(?::text,'')::numeric, status = ?, \
                        updated_at = ? \
                  WHERE id = ?::uuid"
                    .to_string(),
                vec![
                    Value::Text(after.amount.to_string()),
                    Value::Text(after.unpaid.to_string()),
                    Value::from(after.status),
                    Value::from(now),
                    Value::from(&payable.id),
                ],
            )
            .await
            .map_err(|error| utils::db_error(error, "冲减应付"))?;
            payable_id = Some(payable.id);
            payable_amount_after = Some(after.amount);
            to_supplier_receivable = after.to_supplier_receivable;
            tracing::info!(
                return_no = %row.return_no,
                payable_no = %payable.payable_no,
                before_amount = %payable.amount,
                after_amount = %after.amount,
                to_supplier_receivable = %after.to_supplier_receivable,
                "退货确认：应付已按「只冲未付」口径冲减"
            );
        }
    } else {
        tracing::warn!(
            return_no = %row.return_no,
            "退货确认：来源入库单没有关联应付（尚未过账 / 手工入库），退货金额全额转供应商应退款"
        );
    }

    let mut supplier_receivable_id = None;
    let mut supplier_receivable_no = None;
    if to_supplier_receivable > Decimal::ZERO {
        let (id, no) = model::insert_supplier_receivable(
            ex,
            &NewSupplierReceivable {
                supplier_id: row.supplier_id,
                source_id: row.id,
                source_no: row.return_no.clone(),
                amount: to_supplier_receivable.round_dp(4),
                due_date: row.return_date,
                period_code: period_code.clone(),
                remark: Some(format!("采购退货 {} 已付款部分的供应商应退", row.return_no)),
            },
            operator,
            now,
        )
        .await
        .map_err(|error| utils::db_error(error, "建供应商应退款"))?;
        supplier_receivable_id = Some(id);
        supplier_receivable_no = Some(no);
    }

    // ⑤ 冲销凭证
    let (voucher_id, voucher_no) =
        write_voucher(ex, row, stock_out_id, &stock_out_no, cost_total, operator, now).await?;

    Ok(PostedReturn {
        stock_out_id,
        stock_out_no,
        cost_total,
        payable_id,
        payable_amount_after,
        supplier_receivable_id,
        supplier_receivable_no,
        supplier_receivable_amount: to_supplier_receivable.round_dp(4),
        voucher_id,
        voucher_no,
    })
}

/// 锁读原应付（按 `row.payable_id`；没关联应付时返回 `None`）。
async fn lock_payable(ex: &impl SqlExecutor, row: &ReturnRow) -> DbResult<Option<PayableSnapshot>> {
    let Some(payable_id) = row.payable_id else {
        return Ok(None);
    };
    let rows = ex
        .query_sql(
            "SELECT id, payable_no, amount, paid_amount, status FROM biz_payable \
              WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
                .to_string(),
            vec![Value::from(&payable_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(found) => Ok(Some(PayableSnapshot {
            id: found.get_typed("id")?,
            payable_no: found.get_typed("payable_no")?,
            amount: found.get_typed("amount")?,
            paid_amount: found.get_typed("paid_amount")?,
            status: found.get_typed("status")?,
        })),
    }
}

/// 冲销凭证：借 2202 / 贷 1405 + 贷 2221.01（± 6401 计价差异）。
async fn write_voucher(
    ex: &impl SqlExecutor,
    row: &ReturnRow,
    stock_out_id: Uuid,
    stock_out_no: &str,
    cost_total: Decimal,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<(Uuid, String), ApiError> {
    let payable_acct = voucher::account_required(ex, voucher::DEFAULT_PAYABLE_CODE, "应付账款")
        .await
        .map_err(|error| utils::db_error(error, "取应付账款科目 2202"))?;
    let inventory_acct = voucher::account_required(ex, voucher::DEFAULT_INVENTORY_CODE, "库存商品")
        .await
        .map_err(|error| utils::db_error(error, "取库存商品科目 1405"))?;
    let tax_acct = voucher::account_required(ex, voucher::DEFAULT_TAX_CODE, "应交税费-进项税额")
        .await
        .ok();
    let cost_acct = voucher::account_required(ex, ACCOUNT_MAIN_COST, "主营业务成本")
        .await
        .map_err(|error| utils::db_error(error, "取主营业务成本科目 6401"))?;

    let basis = VoucherBasis {
        with_tax: row.total_with_tax.round_dp(4),
        tax: row.total_tax.round_dp(4),
        cost: cost_total.round_dp(4),
    };
    let lines = voucher_lines(
        &row.return_no,
        &row.stock_in_no,
        row.supplier_id,
        row.warehouse_id,
        basis,
        &VoucherAccounts {
            payable: payable_acct,
            inventory: inventory_acct,
            tax: tax_acct,
            cost: cost_acct,
        },
    );
    let (debit, credit) = totals_of_lines(&lines);
    if debit != credit {
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("采购退货凭证构造失败：借贷不平衡（借 {debit} / 贷 {credit}）—— 这是代码缺陷，请报障"),
        ));
    }
    // 摘要取供应商**名称**（AGENTS.md 约定 13 / E2E-FIX-5 ①）：账面上写 UUID 财务看不懂也搜不到。
    let supplier = voucher::party_label(
        form_snapshot::supplier_name(ex, row.supplier_id)
            .await
            .map_err(|error| utils::db_error(error, "读退货单供应商名称"))?,
        "供应商",
        row.supplier_id,
    );
    let summary = voucher_summary(&row.return_no, &row.stock_in_no, &supplier);
    let voucher_id = voucher::post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            // 凭证来源 = 退货生成的出库单（`biz_voucher.source_type` 的值域是
            // `VoucherSourceType`，没有“退货”这一项；出库单是本次账务的真实来源单据）
            source_type: voucher::SOURCE_STOCK_OUT,
            source_id: Some(stock_out_id),
            source_no: Some(stock_out_no),
            // 业务单据号 = **退货单号**（出库单号只是记账依据），否则按 PR-… 查凭证 0 行
            // （E2E-FIX-5 ② / V0357）。
            source_doc_no: Some(row.return_no.as_str()),
            voucher_date: row.return_date,
            summary: summary.clone(),
            lines,
            remark: Some("采购退货确认：冲减应付与存货（借 2202 / 贷 1405 + 贷 2221.01 进项转出）"),
            red_of: None,
            status: voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await
    .map_err(|error| utils::db_error(error, "生成采购退货冲销凭证"))?;

    // 回写出库单 → 凭证（与销售侧 link 同口径；本模块只写这一个引用）
    ex.exec_sql(
        "UPDATE biz_stock_out SET voucher_id = ?::uuid, updated_at = ? WHERE id = ?::uuid".to_string(),
        vec![Value::from(&voucher_id), Value::from(now), Value::from(&stock_out_id)],
    )
    .await
    .map_err(|error| utils::db_error(error, "回写出库单凭证"))?;

    let voucher_no = ex
        .query_first_sql(
            "SELECT voucher_no FROM biz_voucher WHERE id = ?::uuid".to_string(),
            vec![Value::from(&voucher_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "读出库凭证号"))?
        .map(|found| found.get_typed::<String>("voucher_no"))
        .transpose()?
        .unwrap_or_default();
    Ok((voucher_id, voucher_no))
}

// ==================== 小工具 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 负库存 / 缺行 → 409（与出库单过账同码：`ERP_STOCK_OUT_NEGATIVE`）。
fn negative_stock(message: String) -> ApiError {
    ApiError::new(409, error_code::ERP_STOCK_OUT_NEGATIVE, message)
}

fn material_label(item: &ItemRow) -> String {
    match (item.material_code.as_deref(), item.material_name.as_deref()) {
        (Some(code), Some(name)) if !code.is_empty() && !name.is_empty() => {
            format!("{code} {name}")
        }
        _ => item.material_id.to_string(),
    }
}

/// 已确认退货的 `received_qty` 口径说明（与 `purchase_model::POSTED_RECEIVED_SQL` 同源）。
///
/// 这里保留一个函数给单测引用，避免「改了派生 SQL 却没人记得口径在哪」：
/// 回读采购明细的 received_qty 必须等于 `POSTED_RECEIVED_SQL` 的结果。
pub fn confirmed_status() -> &'static str {
    STATUS_CONFIRMED
}

/// 供应商应退款与退货单的关联 id（handler 日志用）。
pub fn links_of(posted: &PostedReturn) -> ConfirmLinks {
    ConfirmLinks {
        stock_out_id: posted.stock_out_id,
        payable_id: posted.payable_id,
        supplier_receivable_id: posted.supplier_receivable_id,
        voucher_id: posted.voucher_id,
    }
}

/// `HashMap` 复用（避免 handler 重复导入）。
pub fn returned_map_len(map: &HashMap<Uuid, Decimal>) -> usize {
    map.len()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Decimal {
        text.parse::<Decimal>().expect("测试常量必须是合法小数")
    }

    fn account(code: &str, name: &str) -> AccountRef {
        AccountRef {
            id: Uuid::now_v7(),
            code: code.to_string(),
            name: name.to_string(),
        }
    }

    /// **凭证平衡**（卡面硬要求单测②）：三种成本口径下借贷必须精确相等。
    #[test]
    fn voucher_is_always_balanced() {
        let payable = account("2202", "应付账款");
        let inventory = account("1405", "库存商品");
        let tax = account("2221.01", "应交税费-进项税额转出");
        let cogs = account("6401", "主营业务成本");
        let supplier = Uuid::now_v7();
        let warehouse = Uuid::now_v7();

        // ① 成本与不含税金额相等（最干净的情况）：2 行
        let basis = VoucherBasis {
            with_tax: d("339"),
            tax: d("39"),
            cost: d("300"),
        };
        let accounts = VoucherAccounts {
            payable: payable.clone(),
            inventory: inventory.clone(),
            tax: Some(tax.clone()),
            cost: cogs.clone(),
        };
        let lines = voucher_lines("PRT-T-0001", "IN-T-0001", supplier, warehouse, basis, &accounts);
        assert_eq!(lines.len(), 3, "借 1 行 + 贷 2 行（存货 / 税）");
        let (debit, credit) = totals_of_lines(&lines);
        assert_eq!(debit, credit, "借贷必须精确相等");
        assert_eq!(debit, d("339"));

        // ② 账上成本高于不含税采购价：借方补 6401
        let basis = VoucherBasis {
            with_tax: d("339"),
            tax: d("39"),
            cost: d("320"),
        };
        let lines = voucher_lines("PRT-T-0002", "IN-T-0001", supplier, warehouse, basis, &accounts);
        let (debit, credit) = totals_of_lines(&lines);
        assert_eq!(debit, credit);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[3].debit, d("20"), "差异 20 进 6401 借方");

        // ③ 账上成本低于不含税采购价：贷方补 6401
        let basis = VoucherBasis {
            with_tax: d("339"),
            tax: d("39"),
            cost: d("280"),
        };
        let lines = voucher_lines("PRT-T-0003", "IN-T-0001", supplier, warehouse, basis, &accounts);
        let (debit, credit) = totals_of_lines(&lines);
        assert_eq!(debit, credit);
        assert_eq!(lines[3].credit, d("20"), "差异 20 记 6401 贷方");

        // ④ 零成本存量（税额 0、成本 0）：只有借 2202 一行 + 差额行 → 仍需 ≥ 2 行
        let basis = VoucherBasis {
            with_tax: d("100"),
            tax: Decimal::ZERO,
            cost: Decimal::ZERO,
        };
        let lines = voucher_lines(
            "PRT-T-0004",
            "IN-T-0001",
            supplier,
            warehouse,
            basis,
            &VoucherAccounts {
                payable: payable.clone(),
                inventory: inventory.clone(),
                tax: None,
                cost: cogs.clone(),
            },
        );
        let (debit, credit) = totals_of_lines(&lines);
        assert_eq!(debit, credit, "零成本也要平衡");
        assert!(lines.len() >= 2, "凭证至少 2 行");
    }

    /// 成本差异的方向（正 = 成本高于不含税价）。
    #[test]
    fn cost_variance_direction() {
        let basis = VoucherBasis {
            with_tax: d("1130"),
            tax: d("130"),
            cost: d("1000"),
        };
        assert_eq!(basis.cost_variance(), Decimal::ZERO);
        let basis = VoucherBasis {
            with_tax: d("1130"),
            tax: d("130"),
            cost: d("1050"),
        };
        assert_eq!(basis.cost_variance(), d("50"));
        let basis = VoucherBasis {
            with_tax: d("1130"),
            tax: d("130"),
            cost: d("950"),
        };
        assert_eq!(basis.cost_variance(), d("-50"));
    }

    /// 期间码格式（`yyyy-MM`，与 `biz_accounting_period` 同口径）。
    #[test]
    fn period_code_format() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 11, 8).expect("合法日期");
        assert_eq!(period_code_of(date), "2026-11");
    }

    /// 常量与词表 / 数据库域值一致（避免「改了词表忘了这里」）。
    #[test]
    fn constants_match_domains() {
        assert_eq!(STOCK_OUT_OUT_TYPE, "OTHER");
        assert!(stockout_base::OUT_TYPES.contains(&STOCK_OUT_OUT_TYPE));
        assert_eq!(STOCK_OUT_SOURCE_TYPE, "PURCHASE_RETURN");
        assert_eq!(TXN_BIZ_TYPE, "PURCHASE_RETURN");
        assert!(
            crate::business::shared::doc_kind::TxnBizType::ALL
                .iter()
                .any(|value| value.as_str() == TXN_BIZ_TYPE)
        );
        assert_eq!(confirmed_status(), "CONFIRMED");
    }
}
