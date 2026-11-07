//! 采购退货数据层（P2-03 / V0332）：取号 / 建单 / 分页 / 明细 / 可退量守卫 / 状态 CAS
//! / 供应商应退款。
//!
//! 表结构以 `V0332__purchase_return.up.sql` 为准：
//!
//! ```text
//! biz_purchase_return        id, return_no, stock_in_id, purchase_order_id, supplier_id,
//!                            warehouse_id, return_date, status, total_qty/amount/tax/with_tax,
//!                            reason, approval_instance_id, stock_out_id, payable_id,
//!                            supplier_receivable_id, voucher_id, confirmed_at/by,
//!                            cancelled_at/by, cancel_reason, remark + 审计五件套
//! biz_purchase_return_item   id, return_id, line_no, stock_in_item_id, purchase_order_item_id,
//!                            material_id, unit_id, qty, price, price_incl, tax_rate,
//!                            amount, tax_amount, with_tax_amount, location_id, remark
//! biz_supplier_receivable    id, sr_no, supplier_id, source_type, source_id, source_no,
//!                            amount, received_amount, unsettled_amount, due_date, period_code,
//!                            status, ...
//! ```
//!
//! # 两个容易踩的点
//!
//! 1. `db().find_with_params` 只接受 `&str`，`exec_sql` / `query_sql` 只接受 `String`
//!    （trait 方法 vs 固有方法，签名不同）；
//! 2. **可退量守卫必须能排除本单自己**：一张「退满 100」的单提交时重跑守卫，
//!    若不排除自己，`已退` 会把自己算进去 ⇒ 可退 0 ⇒ 单子永远提交不了。
//!    口径见 [`returned_qty_of`] 的 `exclude` 参数（哨兵 nil = 不排除）。

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::purchase_return_dto::{
    ReturnFilter, ReturnItemRes, ReturnRowRes, STATUS_APPROVED, STATUS_CANCELLED, STATUS_CONFIRMED,
    STATUS_DRAFT,
};
use crate::business::erp::voucher::{self, SeqSpec};
use crate::business::shared::scope;

// ==================== 表名 / 常量 ====================

/// 单头表。
pub const TABLE: &str = "biz_purchase_return";
/// 明细表（无 `deleted_at`，主单软删后明细留在库里）。
pub const ITEM_TABLE: &str = "biz_purchase_return_item";
/// 供应商应退款表。
pub const SR_TABLE: &str = "biz_supplier_receivable";

/// 退货单号前缀。
pub const NO_PREFIX: &str = "PRT";
/// `biz_seq.seq_type`。
pub const SEQ_TYPE: &str = "PRT";
/// 供应商应退款单号前缀。
pub const SR_NO_PREFIX: &str = "SR";
/// `biz_seq.seq_type`（供应商应退款）。
pub const SR_SEQ_TYPE: &str = "SR";

/// 退货单取号规则（`PRT-yyyyMMdd-000001`）。
pub const RETURN_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: SEQ_TYPE,
    prefix: NO_PREFIX,
    seq_name: "采购退货单号",
    remark: "采购退货单号：PRT-yyyyMMdd-000001，按天（CST）滚动",
};

/// 供应商应退款取号规则（`SR-yyyyMMdd-000001`）。
pub const SR_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: SR_SEQ_TYPE,
    prefix: SR_NO_PREFIX,
    seq_name: "供应商应退款单号",
    remark: "供应商应退款单号：SR-yyyyMMdd-000001，按天（CST）滚动",
};

/// 分页目标（`SCOPE-EXEMPT`：ERP 单据表无归属列，可见性靠权限码）。
pub const LIST_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

// ==================== 行结构 ====================

/// 退货单主行（含展示用联表字段）。
#[derive(Debug, Clone)]
pub struct ReturnRow {
    /// 主键。
    pub id: Uuid,
    /// 单号。
    pub return_no: String,
    /// 来源入库单。
    pub stock_in_id: Uuid,
    /// 来源入库单号。
    pub stock_in_no: String,
    /// 来源采购订单。
    pub purchase_order_id: Option<Uuid>,
    /// 采购订单号。
    pub purchase_order_no: Option<String>,
    /// 供应商。
    pub supplier_id: Uuid,
    /// 供应商名称。
    pub supplier_name: Option<String>,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 仓库名称。
    pub warehouse_name: Option<String>,
    /// 退货日期。
    pub return_date: NaiveDate,
    /// 状态。
    pub status: String,
    /// 数量合计。
    pub total_qty: Decimal,
    /// 不含税金额合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计。
    pub total_with_tax: Decimal,
    /// 退货原因。
    pub reason: String,
    /// 审批实例。
    pub approval_instance_id: Option<Uuid>,
    /// 出库单。
    pub stock_out_id: Option<Uuid>,
    /// 出库单号。
    pub stock_out_no: Option<String>,
    /// 应付单。
    pub payable_id: Option<Uuid>,
    /// 应付单号。
    pub payable_no: Option<String>,
    /// 供应商应退款。
    pub supplier_receivable_id: Option<Uuid>,
    /// 供应商应退款单号。
    pub supplier_receivable_no: Option<String>,
    /// 凭证。
    pub voucher_id: Option<Uuid>,
    /// 凭证号。
    pub voucher_no: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 制单人。
    pub create_by: Option<Uuid>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 退货明细行。
#[derive(Debug, Clone)]
pub struct ItemRow {
    /// 主键。
    pub id: Uuid,
    /// 行号。
    pub line_no: i32,
    /// 来源入库明细。
    pub stock_in_item_id: Uuid,
    /// 源头采购明细。
    pub purchase_order_item_id: Option<Uuid>,
    /// 物料。
    pub material_id: Uuid,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 单位。
    pub unit_id: Uuid,
    /// 单位名称。
    pub unit_name: Option<String>,
    /// 数量。
    pub qty: Decimal,
    /// 不含税单价。
    pub price: Decimal,
    /// 含税单价。
    pub price_incl: Decimal,
    /// 税率（%）。
    pub tax_rate: Decimal,
    /// 不含税金额。
    pub amount: Decimal,
    /// 税额。
    pub tax_amount: Decimal,
    /// 价税合计。
    pub with_tax_amount: Decimal,
    /// 库位。
    pub location_id: Option<Uuid>,
    /// 行备注。
    pub remark: Option<String>,
    /// 原入库数量（联表带出，便于展示）。
    pub received_qty: Decimal,
}

/// 建单明细入参（校验通过后由 handler 组好）。
#[derive(Debug, Clone)]
pub struct NewItem {
    /// 行号。
    pub line_no: i32,
    /// 来源入库明细。
    pub stock_in_item_id: Uuid,
    /// 源头采购明细。
    pub purchase_order_item_id: Option<Uuid>,
    /// 物料。
    pub material_id: Uuid,
    /// 单位。
    pub unit_id: Uuid,
    /// 数量。
    pub qty: Decimal,
    /// 不含税单价（快照）。
    pub price: Decimal,
    /// 含税单价（快照）。
    pub price_incl: Decimal,
    /// 税率（%）。
    pub tax_rate: Decimal,
    /// 不含税金额。
    pub amount: Decimal,
    /// 税额。
    pub tax_amount: Decimal,
    /// 价税合计。
    pub with_tax_amount: Decimal,
    /// 库位。
    pub location_id: Option<Uuid>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 建单单头。
#[derive(Debug, Clone)]
pub struct NewReturn {
    /// 来源入库单。
    pub stock_in_id: Uuid,
    /// 来源采购订单。
    pub purchase_order_id: Option<Uuid>,
    /// 被冲减的应付单（建单时从来源入库单的 `payable_id` 快照）。
    pub payable_id: Option<Uuid>,
    /// 供应商。
    pub supplier_id: Uuid,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 退货日期。
    pub return_date: NaiveDate,
    /// 数量合计。
    pub total_qty: Decimal,
    /// 不含税金额合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计。
    pub total_with_tax: Decimal,
    /// 退货原因。
    pub reason: String,
    /// 备注。
    pub remark: Option<String>,
}

/// 来源入库明细快照（可退量校验的唯一取数口）。
#[derive(Debug, Clone)]
pub struct SourceItem {
    /// 入库明细 id。
    pub id: Uuid,
    /// 行号。
    pub line_no: i32,
    /// 源头采购明细。
    pub source_item_id: Option<Uuid>,
    /// 物料。
    pub material_id: Uuid,
    /// 单位。
    pub unit_id: Uuid,
    /// 入库数量。
    pub qty: Decimal,
    /// 不含税单价。
    pub price: Decimal,
    /// 含税单价。
    pub price_incl: Decimal,
    /// 税率（%）。
    pub tax_rate: Decimal,
    /// 库位。
    pub location_id: Option<Uuid>,
}

// ==================== 列清单 ====================

/// 主表 + 展示联表的共同 SELECT（page / find 共用，改一处两处一起变）。
fn head_select_sql() -> String {
    format!(
        "SELECT r.id, r.return_no, r.stock_in_id, COALESCE(si.in_no, '') AS stock_in_no, \
                r.purchase_order_id, po.order_no AS purchase_order_no, \
                r.supplier_id, s.supplier_name, r.warehouse_id, w.warehouse_name, \
                r.return_date, r.status, r.total_qty, r.total_amount, r.total_tax, \
                r.total_with_tax, r.reason, r.approval_instance_id, \
                r.stock_out_id, so.out_no AS stock_out_no, \
                r.payable_id, ap.payable_no, \
                r.supplier_receivable_id, sr.sr_no AS supplier_receivable_no, \
                r.voucher_id, v.voucher_no, r.remark, r.create_by, r.created_at \
           FROM {TABLE} r \
           LEFT JOIN biz_stock_in si ON si.id = r.stock_in_id \
           LEFT JOIN biz_purchase_order po ON po.id = r.purchase_order_id \
           LEFT JOIN biz_supplier s ON s.id = r.supplier_id \
           LEFT JOIN biz_warehouse w ON w.id = r.warehouse_id \
           LEFT JOIN biz_stock_out so ON so.id = r.stock_out_id \
           LEFT JOIN biz_payable ap ON ap.id = r.payable_id \
           LEFT JOIN {SR_TABLE} sr ON sr.id = r.supplier_receivable_id \
           LEFT JOIN biz_voucher v ON v.id = r.voucher_id \
          WHERE r.deleted_at IS NULL"
    )
}

fn read_head(row: &Row) -> DbResult<ReturnRow> {
    Ok(ReturnRow {
        id: row.get_typed("id")?,
        return_no: row.get_typed("return_no")?,
        stock_in_id: row.get_typed("stock_in_id")?,
        stock_in_no: row.get_typed("stock_in_no")?,
        purchase_order_id: row.get_typed("purchase_order_id")?,
        purchase_order_no: row.get_typed("purchase_order_no")?,
        supplier_id: row.get_typed("supplier_id")?,
        supplier_name: row.get_typed("supplier_name")?,
        warehouse_id: row.get_typed("warehouse_id")?,
        warehouse_name: row.get_typed("warehouse_name")?,
        return_date: row.get_typed("return_date")?,
        status: row.get_typed("status")?,
        total_qty: row.get_typed("total_qty")?,
        total_amount: row.get_typed("total_amount")?,
        total_tax: row.get_typed("total_tax")?,
        total_with_tax: row.get_typed("total_with_tax")?,
        reason: row.get_typed("reason")?,
        approval_instance_id: row.get_typed("approval_instance_id")?,
        stock_out_id: row.get_typed("stock_out_id")?,
        stock_out_no: row.get_typed("stock_out_no")?,
        payable_id: row.get_typed("payable_id")?,
        payable_no: row.get_typed("payable_no")?,
        supplier_receivable_id: row.get_typed("supplier_receivable_id")?,
        supplier_receivable_no: row.get_typed("supplier_receivable_no")?,
        voucher_id: row.get_typed("voucher_id")?,
        voucher_no: row.get_typed("voucher_no")?,
        remark: row.get_typed("remark")?,
        create_by: row.get_typed("create_by")?,
        created_at: row.get_typed("created_at")?,
    })
}

// ==================== 查询 ====================

/// 取号（`PRT-yyyyMMdd-000001`）。
pub async fn next_return_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    voucher::next_doc_no(ex, &RETURN_SEQ, now).await
}

/// 取号（`SR-yyyyMMdd-000001`）。
pub async fn next_sr_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    voucher::next_doc_no(ex, &SR_SEQ, now).await
}

/// 按主键取单头（含联表展示字段）。
pub async fn find_return(id: Uuid) -> DbResult<Option<ReturnRow>> {
    // SCOPE-EXEMPT: 按主键取 1 行，ERP 单据表无归属列。
    let rows = db()
        .find_with_params(
            &format!("{} AND r.id = ?::uuid", head_select_sql()),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(read_head).transpose()
}

/// 按主键锁单头（`SELECT … FOR UPDATE`，确认 / 提交 / 作废都要先锁）。
pub async fn lock_return(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<String>> {
    let row = ex
        .query_first_sql(
            format!(
                "SELECT status FROM {TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some(row.get_typed("status")?)),
    }
}

/// 分页。
pub async fn page_returns(filter: &ReturnFilter) -> DbResult<(Vec<ReturnRow>, u64)> {
    let mut sql = head_select_sql();
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND r.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(stock_in_id) = filter.stock_in_id {
        sql.push_str(" AND r.stock_in_id = ?::uuid");
        params.push(Value::from(&stock_in_id));
    }
    if let Some(from) = filter.date_from {
        sql.push_str(" AND r.return_date >= NULLIF(?::text,'')::date");
        params.push(Value::Text(from.to_string()));
    }
    if let Some(to) = filter.date_to {
        sql.push_str(" AND r.return_date <= NULLIF(?::text,'')::date");
        params.push(Value::Text(to.to_string()));
    }
    if let Some(pattern) = crate::utils::like_pattern(filter.keyword.as_deref()) {
        let like = Value::Text(pattern);
        sql.push_str(
            " AND (r.return_no LIKE ? OR COALESCE(si.in_no, '') LIKE ? \
                OR COALESCE(s.supplier_name, '') LIKE ? OR r.reason LIKE ? \
                OR COALESCE(r.remark, '') LIKE ? OR COALESCE(ap.payable_no, '') LIKE ?)",
        );
        for _ in 0..6 {
            params.push(like.clone());
        }
    }
    let page = scope::paginate_unscoped(
        LIST_TARGET,
        &sql,
        params,
        Some("ORDER BY r.return_date DESC, r.return_no DESC"),
        filter.page,
        filter.page_size,
    )
    .await?;
    let rows = page
        .list
        .iter()
        .map(read_head)
        .collect::<DbResult<Vec<_>>>()?;
    Ok((rows, page.total))
}

/// 明细（含物料 / 单位 / 原入库数量）。
pub async fn items_of(return_id: Uuid) -> DbResult<Vec<ItemRow>> {
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT it.id, it.line_no, it.stock_in_item_id, it.purchase_order_item_id, \
                        it.material_id, COALESCE(m.material_code, '') AS material_code, \
                        COALESCE(m.material_name, '') AS material_name, \
                        it.unit_id, COALESCE(u.unit_name, '') AS unit_name, \
                        it.qty, it.price, it.price_incl, it.tax_rate, it.amount, \
                        it.tax_amount, it.with_tax_amount, it.location_id, it.remark, \
                        COALESCE(sii.qty, 0) AS received_qty \
                   FROM {ITEM_TABLE} it \
                   LEFT JOIN biz_material m ON m.id = it.material_id \
                   LEFT JOIN biz_unit u ON u.id = it.unit_id \
                   LEFT JOIN biz_stock_in_item sii ON sii.id = it.stock_in_item_id \
                  WHERE it.return_id = ?::uuid \
                  ORDER BY it.line_no"
            ),
            vec![Value::from(&return_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(ItemRow {
                id: row.get_typed("id")?,
                line_no: row.get_typed("line_no")?,
                stock_in_item_id: row.get_typed("stock_in_item_id")?,
                purchase_order_item_id: row.get_typed("purchase_order_item_id")?,
                material_id: row.get_typed("material_id")?,
                material_code: row.get_typed("material_code").ok(),
                material_name: row.get_typed("material_name").ok(),
                unit_id: row.get_typed("unit_id")?,
                unit_name: row.get_typed("unit_name").ok(),
                qty: row.get_typed("qty")?,
                price: row.get_typed("price")?,
                price_incl: row.get_typed("price_incl")?,
                tax_rate: row.get_typed("tax_rate")?,
                amount: row.get_typed("amount")?,
                tax_amount: row.get_typed("tax_amount")?,
                with_tax_amount: row.get_typed("with_tax_amount")?,
                location_id: row.get_typed("location_id")?,
                remark: row.get_typed("remark")?,
                received_qty: row.get_typed("received_qty")?,
            })
        })
        .collect()
}

/// 锁读来源入库明细（`FOR UPDATE`：防两个退货单并发把同一行退超）。
pub async fn lock_source_items(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
) -> DbResult<Vec<SourceItem>> {
    let rows = ex
        .query_sql(
            "SELECT id, line_no, source_item_id, material_id, unit_id, qty, price, \
                    price_incl, tax_rate, location_id \
               FROM biz_stock_in_item \
              WHERE stock_in_id = ?::uuid \
              ORDER BY line_no \
                FOR UPDATE"
                .to_string(),
            vec![Value::from(&stock_in_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(SourceItem {
                id: row.get_typed("id")?,
                line_no: row.get_typed("line_no")?,
                source_item_id: row.get_typed("source_item_id")?,
                material_id: row.get_typed("material_id")?,
                unit_id: row.get_typed("unit_id")?,
                qty: row.get_typed("qty")?,
                price: row.get_typed("price")?,
                price_incl: row.get_typed("price_incl")?,
                tax_rate: row.get_typed("tax_rate")?,
                location_id: row.get_typed("location_id")?,
            })
        })
        .collect()
}

/// 某入库明细的**已退数量**（只算未作废的退货单）。
///
/// `exclude` = 要排除的退货单 id（提交 / 确认时传本单 id，避免把自己算进去）。
/// 用 nil uuid 当「不排除」的哨兵，一条语句覆盖两种语义（`r.id <> nil` 恒真）。
pub async fn returned_qty_of(
    ex: &impl SqlExecutor,
    stock_in_item_id: Uuid,
    exclude: Option<Uuid>,
) -> DbResult<Decimal> {
    let exclude = exclude.unwrap_or_else(Uuid::nil);
    let row = ex
        .query_first_sql(
            format!(
                "SELECT COALESCE(SUM(it.qty), 0)::numeric(18,4) AS returned \
                   FROM {ITEM_TABLE} it \
                   JOIN {TABLE} r ON r.id = it.return_id \
                  WHERE it.stock_in_item_id = ?::uuid \
                    AND r.id <> ?::uuid \
                    AND r.status <> '{STATUS_CANCELLED}' \
                    AND r.deleted_at IS NULL"
            ),
            vec![Value::from(&stock_in_item_id), Value::from(&exclude)],
        )
        .await?;
    match row {
        None => Ok(Decimal::ZERO),
        Some(row) => row.get_typed("returned"),
    }
}

/// 一张入库单下**所有**明细的已退数量（可退明细接口用，一次查完）。
pub async fn returned_qty_map(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
    exclude: Option<Uuid>,
) -> DbResult<HashMap<Uuid, Decimal>> {
    let exclude = exclude.unwrap_or_else(Uuid::nil);
    let rows = ex
        .query_sql(
            format!(
                "SELECT it.stock_in_item_id AS item_id, \
                        SUM(it.qty)::numeric(18,4) AS returned \
                   FROM {ITEM_TABLE} it \
                   JOIN {TABLE} r ON r.id = it.return_id \
                  WHERE r.stock_in_id = ?::uuid \
                    AND r.id <> ?::uuid \
                    AND r.status <> '{STATUS_CANCELLED}' \
                    AND r.deleted_at IS NULL \
                  GROUP BY it.stock_in_item_id"
            ),
            vec![Value::from(&stock_in_id), Value::from(&exclude)],
        )
        .await?;
    let mut map = HashMap::new();
    for row in &rows {
        let item_id: Uuid = row.get_typed("item_id")?;
        let returned: Decimal = row.get_typed("returned")?;
        map.insert(item_id, returned);
    }
    Ok(map)
}

// ==================== 写 ====================

/// 建单（单头 + 明细；**调用方开事务**，与守卫的 `FOR UPDATE` 同一事务）。
pub async fn insert_return(
    ex: &impl SqlExecutor,
    head: &NewReturn,
    items: &[NewItem],
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<(Uuid, String)> {
    let id = Uuid::now_v7();
    let return_no = next_return_no(ex, now).await?;
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                 (id, return_no, stock_in_id, purchase_order_id, supplier_id, warehouse_id, \
                  return_date, status, total_qty, total_amount, total_tax, total_with_tax, \
                  reason, remark, payable_id, create_by, update_by, created_at, updated_at) \
             VALUES (?::uuid, ?, ?::uuid, NULLIF(?::text,'')::uuid, ?::uuid, ?::uuid, \
                  NULLIF(?::text,'')::date, '{STATUS_DRAFT}', NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                  NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, ?, ?, \
                  NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, ?, ?)"
        ),
        vec![
            Value::from(&id),
            Value::from(return_no.as_str()),
            Value::from(&head.stock_in_id),
            Value::Text(
                head.purchase_order_id
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            ),
            Value::from(&head.supplier_id),
            Value::from(&head.warehouse_id),
            Value::Text(head.return_date.to_string()),
            Value::Text(head.total_qty.to_string()),
            Value::Text(head.total_amount.to_string()),
            Value::Text(head.total_tax.to_string()),
            Value::Text(head.total_with_tax.to_string()),
            Value::from(head.reason.as_str()),
            Value::Text(head.remark.clone().unwrap_or_default()),
            Value::Text(
                head.payable_id
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            ),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::from(now),
        ],
    )
    .await?;

    for item in items {
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_TABLE} \
                     (id, return_id, line_no, stock_in_item_id, purchase_order_item_id, \
                      material_id, unit_id, qty, price, price_incl, tax_rate, amount, \
                      tax_amount, with_tax_amount, location_id, remark, created_at, updated_at) \
                 VALUES (?::uuid, ?::uuid, ?, ?::uuid, NULLIF(?::text,'')::uuid, \
                      ?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                      NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                      NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                      NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::uuid, ?, ?, ?)"
            ),
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(&id),
                Value::from(item.line_no),
                Value::from(&item.stock_in_item_id),
                Value::Text(
                    item.purchase_order_item_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                ),
                Value::from(&item.material_id),
                Value::from(&item.unit_id),
                Value::Text(item.qty.to_string()),
                Value::Text(item.price.to_string()),
                Value::Text(item.price_incl.to_string()),
                Value::Text(item.tax_rate.to_string()),
                Value::Text(item.amount.to_string()),
                Value::Text(item.tax_amount.to_string()),
                Value::Text(item.with_tax_amount.to_string()),
                Value::Text(item.location_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(item.remark.clone().unwrap_or_default()),
                Value::from(now),
                Value::from(now),
            ],
        )
        .await?;
    }
    Ok((id, return_no))
}

/// 状态 CAS：`DRAFT → SUBMITTED`，同时落审批实例 id。
pub async fn mark_submitted(
    ex: &impl SqlExecutor,
    id: Uuid,
    instance_id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET status = 'SUBMITTED', approval_instance_id = ?::uuid, \
                    update_by = NULLIF(?::text,'')::uuid, updated_at = ? \
              WHERE id = ?::uuid AND status = '{STATUS_DRAFT}' AND deleted_at IS NULL"
        ),
        vec![
            Value::from(&instance_id),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::from(&id),
        ],
    )
    .await
}

/// 状态 CAS：`SUBMITTED → APPROVED`（审批通过 / 跳过审批）。
pub async fn mark_approved(
    ex: &impl SqlExecutor,
    id: Uuid,
    note: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET status = '{STATUS_APPROVED}', \
                    remark = CASE WHEN NULLIF(?::text,'') IS NULL THEN remark \
                                  ELSE COALESCE(remark || E'\\n', '') || ?::text END, \
                    update_by = NULLIF(?::text,'')::uuid, updated_at = ? \
              WHERE id = ?::uuid AND status = 'SUBMITTED' AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(note.unwrap_or_default().to_string()),
            Value::Text(note.unwrap_or_default().to_string()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::from(&id),
        ],
    )
    .await
}

/// 状态 CAS：`SUBMITTED → DRAFT`（审批驳回 / 撤回；不是状态机登记的边，只写轨迹）。
pub async fn mark_back_to_draft(
    ex: &impl SqlExecutor,
    id: Uuid,
    note: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET status = '{STATUS_DRAFT}', approval_instance_id = NULL, \
                    remark = CASE WHEN NULLIF(?::text,'') IS NULL THEN remark \
                                  ELSE COALESCE(remark || E'\\n', '') || ?::text END, \
                    update_by = NULLIF(?::text,'')::uuid, updated_at = ? \
              WHERE id = ?::uuid AND status = 'SUBMITTED' AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(note.unwrap_or_default().to_string()),
            Value::Text(note.unwrap_or_default().to_string()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::from(&id),
        ],
    )
    .await
}

/// 状态 CAS：`APPROVED → CONFIRMED`，回写四张下游单据 id。
#[derive(Debug, Clone, Copy)]
pub struct ConfirmLinks {
    /// 生成的出库单。
    pub stock_out_id: Uuid,
    /// 被冲减的应付单。
    pub payable_id: Option<Uuid>,
    /// 生成的供应商应退款。
    pub supplier_receivable_id: Option<Uuid>,
    /// 生成的凭证。
    pub voucher_id: Uuid,
}

/// 状态 CAS：`APPROVED → CONFIRMED`。
pub async fn mark_confirmed(
    ex: &impl SqlExecutor,
    id: Uuid,
    links: ConfirmLinks,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET status = '{STATUS_CONFIRMED}', stock_out_id = ?::uuid, \
                    payable_id = NULLIF(?::text,'')::uuid, \
                    supplier_receivable_id = NULLIF(?::text,'')::uuid, \
                    voucher_id = ?::uuid, confirmed_at = ?, \
                    confirmed_by = NULLIF(?::text,'')::uuid, \
                    update_by = NULLIF(?::text,'')::uuid, updated_at = ? \
              WHERE id = ?::uuid AND status = '{STATUS_APPROVED}' AND deleted_at IS NULL"
        ),
        vec![
            Value::from(&links.stock_out_id),
            Value::Text(links.payable_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(
                links
                    .supplier_receivable_id
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            ),
            Value::from(&links.voucher_id),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::from(&id),
        ],
    )
    .await
}

/// 状态 CAS：`{from} → CANCELLED`（`from` 由调用方传入并在 SQL 里断言）。
pub async fn mark_cancelled(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    reason: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET status = '{STATUS_CANCELLED}', cancelled_at = ?, \
                    cancelled_by = NULLIF(?::text,'')::uuid, cancel_reason = ?, \
                    update_by = NULLIF(?::text,'')::uuid, updated_at = ? \
              WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
        ),
        vec![
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(reason),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::from(&id),
            Value::Text(from.to_string()),
        ],
    )
    .await
}

/// 供应商应退款建单（已付款部分）。
#[derive(Debug, Clone)]
pub struct NewSupplierReceivable {
    /// 供应商。
    pub supplier_id: Uuid,
    /// 来源退货单。
    pub source_id: Uuid,
    /// 来源单号。
    pub source_no: String,
    /// 应退金额（> 0）。
    pub amount: Decimal,
    /// 到期日（= 退货日期，未约定账期）。
    pub due_date: NaiveDate,
    /// 期间码 `yyyy-MM`。
    pub period_code: String,
    /// 备注。
    pub remark: Option<String>,
}

/// 建供应商应退款（`status = OPEN`、`received = 0`）。
pub async fn insert_supplier_receivable(
    ex: &impl SqlExecutor,
    new: &NewSupplierReceivable,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<(Uuid, String)> {
    let id = Uuid::now_v7();
    let sr_no = next_sr_no(ex, now).await?;
    ex.exec_sql(
        format!(
            "INSERT INTO {SR_TABLE} \
                 (id, sr_no, supplier_id, source_type, source_id, source_no, amount, \
                  received_amount, unsettled_amount, due_date, period_code, status, remark, \
                  create_by, update_by, created_at, updated_at) \
             VALUES (?::uuid, ?, ?::uuid, 'PURCHASE_RETURN', ?::uuid, ?, \
                  NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::date, ?, \
                  'OPEN', ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, ?, ?)"
        ),
        vec![
            Value::from(&id),
            Value::from(sr_no.as_str()),
            Value::from(&new.supplier_id),
            Value::from(&new.source_id),
            Value::from(new.source_no.as_str()),
            Value::Text(new.amount.to_string()),
            Value::Text(new.amount.to_string()),
            Value::Text(new.due_date.to_string()),
            Value::from(new.period_code.as_str()),
            Value::Text(new.remark.clone().unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::from(now),
        ],
    )
    .await?;
    Ok((id, sr_no))
}

// ==================== 出参组装 ====================

/// 主行 → 出参。
pub fn to_row_res(row: &ReturnRow) -> ReturnRowRes {
    ReturnRowRes {
        id: row.id.to_string(),
        return_no: row.return_no.clone(),
        stock_in_id: row.stock_in_id.to_string(),
        stock_in_no: row.stock_in_no.clone(),
        purchase_order_no: row.purchase_order_no.clone(),
        supplier_id: row.supplier_id.to_string(),
        supplier_name: row.supplier_name.clone(),
        warehouse_id: row.warehouse_id.to_string(),
        warehouse_name: row.warehouse_name.clone(),
        return_date: row.return_date,
        status: row.status.clone(),
        status_label: crate::business::erp::purchase_return_dto::status_label(&row.status)
            .to_string(),
        total_qty: row.total_qty,
        total_amount: row.total_amount,
        total_tax: row.total_tax,
        total_with_tax: row.total_with_tax,
        reason: row.reason.clone(),
        approval_instance_id: row.approval_instance_id.map(|v| v.to_string()),
        stock_out_no: row.stock_out_no.clone(),
        payable_no: row.payable_no.clone(),
        voucher_no: row.voucher_no.clone(),
        supplier_receivable_no: row.supplier_receivable_no.clone(),
        remark: row.remark.clone(),
        created_at: row.created_at,
    }
}

/// 明细 → 出参（`returned_qty` 由调用方传入：含本单的累计已退）。
pub fn to_item_res(item: &ItemRow, returned_qty: Decimal) -> ReturnItemRes {
    ReturnItemRes {
        id: item.id.to_string(),
        line_no: item.line_no,
        stock_in_item_id: item.stock_in_item_id.to_string(),
        material_id: item.material_id.to_string(),
        material_code: item.material_code.clone(),
        material_name: item.material_name.clone(),
        unit_id: item.unit_id.to_string(),
        unit_name: item.unit_name.clone(),
        qty: item.qty,
        price: item.price,
        price_incl: item.price_incl,
        tax_rate: item.tax_rate,
        amount: item.amount,
        tax_amount: item.tax_amount,
        with_tax_amount: item.with_tax_amount,
        received_qty: item.received_qty,
        returned_qty,
        location_id: item.location_id.map(|v| v.to_string()),
        remark: item.remark.clone(),
    }
}
