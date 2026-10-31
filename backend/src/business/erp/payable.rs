//! 应付单（批次 D · P2-C1）：**入库过账即生成应付**，作废即作废应付。
//!
//! 表结构以 `backend/migrations/business/V0148__erp_payable.up.sql` 为准（架构 §2.4.1）：
//!
//! ```text
//! biz_payable   id, payable_no, supplier_id, source_type, source_id, source_no,
//!               invoice_no, invoice_date, currency_code, amount, paid_amount, unpaid_amount,
//!               credit_days, due_date, period_code, status(OPEN/PARTIAL_PAID/SETTLED/CANCELLED),
//!               settled_at, voucher_id, remark + 审计五件套
//!               CHECK unpaid_amount = amount - paid_amount     ← 恒等（程序算错会被拦）
//!               UNIQUE (source_type, source_id) WHERE deleted_at IS NULL  ← 幂等铁律
//! ```
//!
//! # 金额口径
//!
//! `amount = biz_stock_in.total_with_tax`（**价税合计**，卡面 §三 ② / 硬红线 5：取自单据不重算）。
//! `paid_amount = 0`、`unpaid_amount = amount`、`status = OPEN`。
//!
//! # 账期与到期日
//!
//! `credit_days` 取供应商档案的账期快照，`due_date = 入库日期 + credit_days`
//! （架构 §2.4.1：「到期日 = 入库日期 + 账期」）。供应商缺失时按 0 天（`due_date = in_date`）。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: 应付单是财务台账（V0148 建表，无 dept_id / 归属人列），数据范围
//! （SELF / DEPT_*）表达不出来；可见性由 business:erp:payable:* 权限码控制（P2-D1 落地）。
//! SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
//! SCOPE-EXEMPT: biz_supplier 是供应商主数据（全局），没有归属列。
//! ```
//!
//! # 事务边界
//!
//! 写函数吃 `&impl SqlExecutor`，**由调用方（过账 / 作废事务）开事务**：应付与凭证、库存
//! 必须同生共死（卡面硬红线 2 / ⑭）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::stockin_model::StockInRow;
use crate::business::erp::voucher::{self, SeqSpec};
use crate::business::shared::state_machine;

/// 应付单表。
pub const TABLE: &str = "biz_payable";

/// 状态：未付。
pub const STATUS_OPEN: &str = "OPEN";
/// 状态：部分已付（批次 2 付款回写）。
pub const STATUS_PARTIAL_PAID: &str = "PARTIAL_PAID";
/// 状态：已结清。
pub const STATUS_SETTLED: &str = "SETTLED";
/// 状态：已作废（收货单作废时联动）。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 来源类型：收货入库单。
pub const SOURCE_STOCK_IN: &str = "STOCK_IN";

/// 单号前缀（`AP-yyyyMMdd-000001`）。
pub const NO_PREFIX: &str = "AP";
/// `biz_seq.seq_type`。
pub const SEQ_TYPE: &str = "AP";

/// 应付单取号规则。
pub const PAYABLE_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: SEQ_TYPE,
    prefix: NO_PREFIX,
    seq_name: "应付单号",
    remark: "应付单号：AP-yyyyMMdd-000001，按天（CST）滚动",
};

/// 取一个应付单号。
pub async fn next_payable_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    voucher::next_doc_no(ex, &PAYABLE_SEQ, now).await
}

/// `biz_payable` 列清单。
pub const COLUMNS: &str = "id, payable_no, supplier_id, source_type, source_id, source_no, \
     invoice_no, invoice_date, currency_code::text AS currency_code, amount, paid_amount, \
     unpaid_amount, credit_days, due_date, period_code, status, settled_at, voucher_id, remark, \
     created_at, updated_at, create_by, update_by, deleted_at";

/// 应付单行。
#[derive(Debug, Clone)]
pub struct PayableRow {
    /// 主键。
    pub id: Uuid,
    /// 应付单号。
    pub payable_no: String,
    /// 供应商。
    pub supplier_id: Uuid,
    /// 来源单据类型。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Uuid,
    /// 来源单号。
    pub source_no: String,
    /// 发票号（批次 2 回填）。
    pub invoice_no: Option<String>,
    /// 发票日期（批次 2 回填）。
    pub invoice_date: Option<NaiveDate>,
    /// 币种。
    pub currency_code: String,
    /// 应付金额（含税）。
    pub amount: Decimal,
    /// 已付金额。
    pub paid_amount: Decimal,
    /// 未付金额（= amount - paid_amount）。
    pub unpaid_amount: Decimal,
    /// 账期天数快照。
    pub credit_days: i32,
    /// 到期日。
    pub due_date: NaiveDate,
    /// 所属会计期间。
    pub period_code: String,
    /// 状态。
    pub status: String,
    /// 结清时间。
    pub settled_at: Option<DateTime<Utc>>,
    /// 关联凭证。
    pub voucher_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: Option<DateTime<Utc>>,
    /// 更新时间。
    pub updated_at: Option<DateTime<Utc>>,
}

impl PayableRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            payable_no: row.get_typed("payable_no")?,
            supplier_id: row.get_typed("supplier_id")?,
            source_type: row.get_typed("source_type")?,
            source_id: row.get_typed("source_id")?,
            source_no: row.get_typed("source_no")?,
            invoice_no: row.get_typed("invoice_no")?,
            invoice_date: row.get_typed("invoice_date")?,
            currency_code: row.get_typed("currency_code")?,
            amount: row.get_typed("amount")?,
            paid_amount: row.get_typed("paid_amount")?,
            unpaid_amount: row.get_typed("unpaid_amount")?,
            credit_days: row.get_typed("credit_days")?,
            due_date: row.get_typed("due_date")?,
            period_code: row.get_typed("period_code")?,
            status: row.get_typed("status")?,
            settled_at: row.get_typed("settled_at")?,
            voucher_id: row.get_typed("voucher_id")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// 按来源单据取应付单（`uk_biz_payable_source` 保证最多一张）。
pub async fn find_by_source(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Uuid,
) -> DbResult<Option<PayableRow>> {
    // SCOPE-EXEMPT: 应付台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {COLUMNS} FROM {TABLE} \
                  WHERE source_type = ? AND source_id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![
                Value::Text(source_type.to_string()),
                Value::from(&source_id),
            ],
        )
        .await?;
    row.as_ref().map(PayableRow::from_row).transpose()
}

/// 供应商的账期天数（缺供应商 → 0）。
async fn credit_days_of(ex: &impl SqlExecutor, supplier_id: Uuid) -> DbResult<i32> {
    // SCOPE-EXEMPT: biz_supplier 是供应商主数据（全局），没有归属列。
    let days: i32 = ex
        .scalar_sql(
            "SELECT COALESCE((SELECT credit_days FROM biz_supplier \
                               WHERE id = ?::uuid AND deleted_at IS NULL), 0)"
                .to_string(),
            vec![Value::from(&supplier_id)],
        )
        .await?;
    Ok(days.max(0))
}

/// 应付单落库入参。
#[derive(Debug, Clone)]
pub struct NewPayable<'a> {
    /// 供应商。
    pub supplier_id: Uuid,
    /// 来源单据类型。
    pub source_type: &'a str,
    /// 来源单据 id。
    pub source_id: Uuid,
    /// 来源单号。
    pub source_no: &'a str,
    /// 应付金额（含税）。
    pub amount: Decimal,
    /// 账期天数。
    pub credit_days: i32,
    /// 到期日。
    pub due_date: NaiveDate,
    /// 所属会计期间。
    pub period_code: &'a str,
    /// 关联凭证。
    pub voucher_id: Option<Uuid>,
}

/// 写一张应付单（`paid_amount = 0`、`unpaid_amount = amount`、`status = OPEN`）。
pub async fn insert(
    ex: &impl SqlExecutor,
    new: &NewPayable<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let payable_no = next_payable_no(ex, now).await?;
    let amount = new.amount.round_dp(4);
    let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();
    // SCOPE-EXEMPT: 应付台账无归属列（见模块文档）。
    let inserted = ex
        .query_first_sql(
            format!(
                "INSERT INTO {TABLE} \
                    (payable_no, supplier_id, source_type, source_id, source_no, currency_code, \
                     amount, paid_amount, unpaid_amount, credit_days, due_date, period_code, \
                     status, voucher_id, created_at, updated_at, create_by, update_by) \
                 VALUES (?, ?::uuid, ?, ?::uuid, ?, 'CNY', NULLIF(?::text,'')::numeric, 0, \
                         NULLIF(?::text,'')::numeric, ?, NULLIF(?::text,'')::date, ?, '{STATUS_OPEN}', \
                         NULLIF(?::text,'')::uuid, ?, ?, NULLIF(?::text,'')::uuid, \
                         NULLIF(?::text,'')::uuid) \
                 RETURNING id"
            ),
            vec![
                Value::Text(payable_no.clone()),
                Value::from(&new.supplier_id),
                Value::Text(new.source_type.to_string()),
                Value::from(&new.source_id),
                Value::Text(new.source_no.to_string()),
                Value::Text(amount.to_string()),
                Value::Text(amount.to_string()),
                Value::from(new.credit_days),
                Value::Text(new.due_date.to_string()),
                Value::Text(new.period_code.to_string()),
                Value::Text(
                    new.voucher_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                ),
                Value::from(now),
                Value::from(now),
                Value::Text(operator_text.clone()),
                Value::Text(operator_text),
            ],
        )
        .await?;
    match inserted.as_ref() {
        Some(row) => {
            let id: Uuid = row.get_typed("id")?;
            // P10 铁律 ③：应付单建单留痕（`NULL → OPEN`），与建单同一事务
            state_machine::log_created(
                ex,
                state_machine::ENTITY_PAYABLE,
                id,
                &payable_no,
                STATUS_OPEN,
                operator,
                None,
            )
            .await
            .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
            Ok(id)
        }
        None => Err(DbError::InvalidValue(format!(
            "应付单写入失败：INSERT 没有返回 id（来源 {}）",
            new.source_no
        ))),
    }
}

/// 收货单过账时生成应付单（卡面 §三 ②）。
///
/// `payable_id` 由本函数返回，调用方用
/// [`crate::business::erp::stockin_model::link_voucher_and_payable`] 回写收货单。
pub async fn create_for_stock_in(
    ex: &impl SqlExecutor,
    header: &StockInRow,
    voucher_id: Option<Uuid>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let supplier_id = header.supplier_id.ok_or_else(|| {
        DbError::InvalidValue(format!(
            "应付单生成失败：收货单 {} 没有供应商（in_type={}）",
            header.in_no, header.in_type
        ))
    })?;
    let credit_days = credit_days_of(ex, supplier_id).await?;
    let period_code = voucher::period_of(ex, header.in_date).await?;
    insert(
        ex,
        &NewPayable {
            supplier_id,
            source_type: SOURCE_STOCK_IN,
            source_id: header.id,
            source_no: &header.in_no,
            amount: header.total_with_tax,
            credit_days,
            due_date: header.in_date + chrono_days(credit_days),
            period_code: &period_code,
            voucher_id,
        },
        operator,
        now,
    )
    .await
}

fn chrono_days(days: i32) -> nik::prelude::chrono::Duration {
    nik::prelude::chrono::Duration::days(i64::from(days))
}

/// 收货单作废时联动作废应付单（卡面 §五 ③）。
///
/// 只保留「非 CANCELLED」的行（重复作废幂等：第二次 affected = 0）。
pub async fn cancel_for_stock_in(
    ex: &impl SqlExecutor,
    source_id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    // P10 铁律 ③：作废联动也必须走状态机唯一入口（写 biz_status_log）。
    // 先读出待作废的应付单（`OPEN` 之外的理论上不存在，CAS 会兜住），再逐张 transition。
    // SCOPE-EXEMPT: 应付台账无归属列（见模块文档）。
    let pending = ex
        .query_sql(
            format!(
                "SELECT id, payable_no, status FROM {TABLE} \
                  WHERE source_type = '{SOURCE_STOCK_IN}' AND source_id = ?::uuid \
                    AND status <> '{STATUS_CANCELLED}' AND deleted_at IS NULL"
            ),
            vec![Value::from(&source_id)],
        )
        .await?;
    for row in &pending {
        let id: Uuid = row.get_typed("id")?;
        let payable_no: String = row.get_typed("payable_no")?;
        let status: String = row.get_typed("status")?;
        // ★ P17 反向：收货单作废 → 这张应付单在跑的**付款审批**也必须终止
        //   （否则付款审批会对着已作废的应付单批下去，只能靠回调兜底拒绝；提前终止更干净）
        crate::business::erp::payable_bridge::cancel_pending_of_payable(
            ex,
            id,
            "收货单作废联动作废应付单，付款审批自动终止",
        )
        .await?;
        state_machine::transition(
            ex,
            state_machine::ENTITY_PAYABLE,
            id,
            &payable_no,
            &state_machine::PAYABLE_EDGES,
            &status,
            STATUS_CANCELLED,
            operator,
            Some("收货单作废联动"),
            None,
        )
        .await
        .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    }

    // SCOPE-EXEMPT: 应付台账无归属列（见模块文档）。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} \
                SET status = '{STATUS_CANCELLED}', updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE source_type = '{SOURCE_STOCK_IN}' AND source_id = ?::uuid \
                AND status <> '{STATUS_CANCELLED}' AND deleted_at IS NULL"
        ),
        vec![
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&source_id),
        ],
    )
    .await
}

/// 未付金额恒等（`unpaid = amount - paid`），供付款模块与勾稽检查复用。
pub fn unpaid_of(amount: Decimal, paid: Decimal) -> Decimal {
    (amount - paid).round_dp(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单号格式：`AP-yyyyMMdd-000001`。
    #[test]
    fn payable_no_format_is_ap_prefix_with_six_digits() {
        assert_eq!(
            voucher::format_doc_no(NO_PREFIX, "20261031", 1),
            "AP-20261031-000001"
        );
        assert_eq!(PAYABLE_SEQ.prefix, "AP");
        assert_eq!(PAYABLE_SEQ.seq_type, "AP");
    }

    /// 未付金额恒等（与库里的 `ck_biz_payable_unpaid` 同口径）。
    #[test]
    fn unpaid_amount_follows_check_constraint() {
        assert_eq!(
            unpaid_of(Decimal::new(2260000, 4), Decimal::new(1000000, 4)),
            Decimal::new(1260000, 4)
        );
        assert_eq!(
            unpaid_of(Decimal::new(10000, 4), Decimal::new(10000, 4)),
            Decimal::ZERO
        );
    }
}
