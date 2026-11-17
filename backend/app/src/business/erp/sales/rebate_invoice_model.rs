//! 销售返利 · 红字发票数据层（M3-17）：`biz_invoice` 的写入 / 回读 + 结算单转 `SETTLED`。
//!
//! 表结构唯一依据：
//! * `backend/app/migrations/business/V0298__sales_invoice.up.sql`（`biz_invoice`）；
//! * `backend/app/migrations/business/V0440__sales_rebate_invoice.up.sql`（结算单 5 列 + 来源白名单）。
//!
//! # 三条铁律
//!
//! 1. **结算单状态只有一处能写 `SETTLED`**：[`mark_settled`]（`WHERE status = 'DRAFT'`），
//!    影响行数为 0 就说明有人在本次事务之外动过这张单 —— 调用方必须报 409，不能当成功。
//! 2. **金额绑参一律 `?::text::numeric`、日期 `?::text::date`**（驱动不吃 `Decimal` / `NaiveDate`
//!    直接绑；写错时 PG 只回「query failed: : db error」，指不出原因）。
//! 3. **发票金额三列 + `base_total_*` 三列一起写**：`ck_biz_invoice_base_amount` 对 `OUT_RED`
//!    要求本位币三列 ≤ 0 且 `total = amount + tax`；只写原币三列会让本位币列落到默认 0，
//!    多币种账套上「含税合计为 0」与「不含税为负」自相矛盾。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_invoice 是发票台账（无 dept_id / salesperson_id 归属列），
//!        可见性由 business:erp:rebate-settlement:invoice 权限码控制；
//!        本文件只写不产出行集给用户翻页。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::sales::rebate_invoice_dto::{INVOICE_SOURCE_REBATE, INVOICE_TYPE_OUT_RED, InvoiceSplit};
use crate::business::erp::sales::rebate_settle_dto::{STATUS_DRAFT, STATUS_SETTLED};
use crate::business::erp::sales::rebate_settle_model::{SettlementRow, lock_settlement, settlement_of};

/// 结算单表名。
pub const SETTLE_TABLE: &str = "biz_sales_rebate_settlement";

/// 事务内锁 + 读结算单（锁不到 / 已软删 → `None`）。
///
/// 锁必须在这里拿（`SELECT … FOR UPDATE`），否则两个并发「开票」会双双通过
/// `status = 'DRAFT'` 检查，各开一张红字发票 —— 返利付两遍、收入冲两遍。
pub async fn lock_and_load(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<SettlementRow>> {
    if !lock_settlement(ex, id).await? {
        return Ok(None);
    }
    settlement_of(ex, id).await
}

// ==================== 写：红字发票 ====================

/// 插入红字发票的入参。
#[derive(Debug, Clone, Copy)]
pub struct NewRedInvoice<'a> {
    /// 发票号（`IV-yyyyMMdd-000001`，调用方在**同一事务**里取号后传入）。
    pub invoice_no: &'a str,
    /// 客户 id（来自结算单）。
    pub customer_id: Uuid,
    /// 开票日期（凭证日期 / 期间闸都取它）。
    pub invoice_date: NaiveDate,
    /// 票面三列（全负）。
    pub split: InvoiceSplit,
    /// 税率（百分数）。
    pub tax_rate: Decimal,
    /// 本位币币种（结算金额本身是**本位币**口径，所以原币 = 本位币、汇率 1）。
    pub currency_code: &'a str,
    /// 来源单号 = 结算单号（`source_type = 'SALES_REBATE'`）。
    pub settlement_no: &'a str,
    /// 备注。
    pub remark: Option<&'a str>,
}

/// 插入一张 `OUT_RED` 发票（`DRAFT`；随后由 handler 推 `ISSUED`）。
pub async fn insert_red_invoice(
    ex: &impl SqlExecutor,
    new: &NewRedInvoice<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    let amount = new.split.amount.to_string();
    let tax = new.split.tax_amount.to_string();
    let total = new.split.total_with_tax.to_string();
    let row = ex
        .query_first_sql(
            "INSERT INTO biz_invoice \
                (invoice_no, invoice_type, invoice_code, customer_id, invoice_date, \
                 amount, tax_amount, total_with_tax, tax_rate, tax_rate_mode, status, \
                 receivable_id, source_type, source_no, remark, \
                 currency_code, exchange_rate, base_total_amount, base_total_tax, base_total_with_tax, \
                 created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, NULL, ?::uuid, ?::text::date, \
                 ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, 'PERCENT', ?, \
                 NULL, ?, ?, NULLIF(?::text,''), \
                 ?, 1, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                 ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
             RETURNING id"
                .to_string(),
            vec![
                Value::Text(new.invoice_no.to_string()),
                Value::Text(INVOICE_TYPE_OUT_RED.to_string()),
                Value::from(&new.customer_id),
                Value::Text(new.invoice_date.to_string()),
                Value::Text(amount.clone()),
                Value::Text(tax.clone()),
                Value::Text(total.clone()),
                Value::Text(new.tax_rate.to_string()),
                Value::Text(crate::business::erp::invoice::STATUS_DRAFT.to_string()),
                Value::Text(INVOICE_SOURCE_REBATE.to_string()),
                Value::Text(new.settlement_no.to_string()),
                Value::Text(new.remark.unwrap_or_default().to_string()),
                Value::Text(new.currency_code.to_string()),
                Value::Text(amount),
                Value::Text(tax),
                Value::Text(total),
                Value::from(now),
                Value::from(now),
                operator_text.clone(),
                operator_text,
            ],
        )
        .await?;
    let row = row.ok_or_else(|| DbError::InvalidValue("红字发票写入失败：INSERT 没有返回 id".to_string()))?;
    row.get_typed("id")
}

/// 发票状态推进：`DRAFT → ISSUED`（库里 `status` 与状态机日志同一事务、同一把锁）。
///
/// 返回影响行数（0 = 状态在本次事务内被改动，调用方按 409 处理）。
pub async fn mark_invoice_issued(
    ex: &impl SqlExecutor,
    invoice_id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    ex.exec_sql(
        "UPDATE biz_invoice SET status = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
          WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(crate::business::erp::invoice::STATUS_ISSUED.to_string()),
            Value::from(now),
            operator_text,
            Value::from(&invoice_id),
            Value::Text(crate::business::erp::invoice::STATUS_DRAFT.to_string()),
        ],
    )
    .await
}

/// 回写红字凭证 id（发票 `voucher_id`）。
pub async fn attach_voucher(
    ex: &impl SqlExecutor,
    invoice_id: Uuid,
    voucher_id: Uuid,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_invoice SET voucher_id = ?::uuid, updated_at = ? \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![Value::from(&voucher_id), Value::from(now), Value::from(&invoice_id)],
    )
    .await
}

// ==================== 写：结算单转 SETTLED ====================

/// `DRAFT → SETTLED`，同时落发票 / 票号 / 凭证 / 转结时间与人。
///
/// `WHERE status = 'DRAFT'` 是硬闸：影响行数 0 ⇒ 这张单已经不是 `DRAFT` 了
/// （并发开票 / 有人刚作废），调用方必须报 409 —— 不能当成功，否则会出现
/// 「票开了、结算单还是 DRAFT」的中间态（约定 11 的死单）。
pub async fn mark_settled(
    ex: &impl SqlExecutor,
    id: Uuid,
    invoice_id: Uuid,
    invoice_no: &str,
    voucher_id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "UPDATE {SETTLE_TABLE} \
                SET status = ?, invoice_id = ?::uuid, invoice_no = ?, voucher_id = ?::uuid, \
                    settled_at = ?, settled_by = NULLIF(?::text,'')::uuid, \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = ?"
        ),
        vec![
            Value::Text(STATUS_SETTLED.to_string()),
            Value::from(&invoice_id),
            Value::Text(invoice_no.to_string()),
            Value::from(&voucher_id),
            Value::from(now),
            operator_text.clone(),
            Value::from(now),
            operator_text,
            Value::from(&id),
            Value::Text(STATUS_DRAFT.to_string()),
        ],
    )
    .await
}

// ==================== 读：红字发票 ====================

/// `biz_invoice` 一行（红字发票回显所需的列）。
#[derive(Debug, Clone)]
pub struct RedInvoiceRow {
    /// 发票 id。
    pub id: Uuid,
    /// 发票号。
    pub invoice_no: String,
    /// 票种。
    pub invoice_type: String,
    /// 开票日期。
    pub invoice_date: NaiveDate,
    /// 不含税金额（负）。
    pub amount: Decimal,
    /// 税额（≤ 0）。
    pub tax_amount: Decimal,
    /// 价税合计（负）。
    pub total_with_tax: Decimal,
    /// 税率（百分数）。
    pub tax_rate: Decimal,
    /// 发票状态。
    pub status: String,
    /// 币种。
    pub currency_code: String,
    /// 红字凭证 id。
    pub voucher_id: Option<Uuid>,
}

impl RedInvoiceRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            invoice_no: row.get_typed("invoice_no")?,
            invoice_type: row.get_typed("invoice_type")?,
            invoice_date: row.get_typed("invoice_date")?,
            amount: row.get_typed("amount")?,
            tax_amount: row.get_typed("tax_amount")?,
            total_with_tax: row.get_typed("total_with_tax")?,
            tax_rate: row.get_typed("tax_rate")?,
            status: row.get_typed("status")?,
            currency_code: row.get_typed("currency_code")?,
            voucher_id: row.get_typed("voucher_id")?,
        })
    }
}

/// 按 id 取红字发票（不存在 / 已软删 → `None`）。
pub async fn invoice_of(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<RedInvoiceRow>> {
    // SCOPE-EXEMPT: 发票台账无归属列，按主键读单行。
    let rows = ex
        .query_sql(
            "SELECT id, invoice_no, invoice_type, invoice_date, amount, tax_amount, \
                    total_with_tax, tax_rate, status, currency_code, voucher_id \
               FROM biz_invoice WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(RedInvoiceRow::from_row).transpose()
}

/// 取凭证号（详情回显「红字凭证 PZ-…」；凭证不存在 → `None`）。
pub async fn voucher_no_of(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 凭证按主键取单号，不产出行集。
    let rows = ex
        .query_sql(
            "SELECT voucher_no FROM biz_voucher WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("voucher_no").map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 表名与写入的状态必须是结算单的域值（写错一个字母，转结就不落库）。
    #[test]
    fn domain_constants() {
        assert_eq!(SETTLE_TABLE, "biz_sales_rebate_settlement");
        assert_eq!(STATUS_DRAFT, "DRAFT");
        assert_eq!(STATUS_SETTLED, "SETTLED");
        assert_eq!(INVOICE_TYPE_OUT_RED, "OUT_RED");
        assert_eq!(INVOICE_SOURCE_REBATE, "SALES_REBATE");
        println!("[M3-17] 数据层域值一致 ✓");
    }
}
