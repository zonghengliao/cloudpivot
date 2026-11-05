//! 费用报销数据层（ARCH-6 §4.2，V0242 建表）：费用类型 / 报销单 / 明细 / 付款。
//!
//! # 本文件守的三条铁律
//!
//! 1. **明细与单头永远同事务重建**：改草稿 = 先删明细再插（`biz_expense_item` 没有
//!    `deleted_at`，与 `biz_purchase_order_item` 同款）；单头的 `item_count /
//!    total_*` 由调用方按 `expense_compute` 算好一起写，**不存在「明细已改、单头还是旧数」**。
//! 2. **状态只由 [`crate::business::shared::state_machine::transition`] 写**：本文件的
//!    `report_mark_*` 只改业务列（金额 / 凭证 / 时间戳），状态的 `from → to` 由调用方先过状态机。
//! 3. **付款必须行锁**（[`report_lock_in`]）：付款是**拒绝型**操作，不锁就会两张付款单
//!    同时通过 `unpaid_amount` 校验，最终 `paid_amount > total_with_tax` 撞库 CHECK。
//!    这是 ARCH-6 里唯一需要行锁的地方（§4.8.3）。
//!
//! # 数据权限
//!
//! 报销单按 `dept_id` 做行级范围（`ScopeTarget::new`），与预算同款；
//! 费用类型是运维级主数据（`unscoped`）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::expense_dto::{ReportFilter, TypeFilter};
use crate::business::erp::voucher::SeqSpec;
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;

/// 费用类型表名。
pub const TYPE_TABLE: &str = "biz_expense_type";
/// 报销单主表名。
pub const TABLE: &str = "biz_expense_report";
/// 明细表名。
pub const ITEM_TABLE: &str = "biz_expense_item";
/// 付款记录表名。
pub const PAY_TABLE: &str = "biz_expense_payment";

/// 报销单号号段（`EXP-yyyyMMdd-000001`，与其它单据同款按天滚动）。
pub const SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: "EXP",
    prefix: "EXP",
    seq_name: "报销单号",
    remark: "报销单号：EXP-yyyyMMdd-000001，按天（CST）滚动",
};

/// 付款单号号段（`PAY-EXP-yyyyMMdd-000001`；与应付付款的 `PAY` 段分开，
/// 免得「今天付了几笔」在同一个号段里分不清是付供应商还是付员工）。
pub const PAY_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: "EXP_PAY",
    prefix: "PAY-EXP",
    seq_name: "报销付款单号",
    remark: "报销付款单号：PAY-EXP-yyyyMMdd-000001，按天（CST）滚动",
};

/// 报销单分页的数据权限锚点：归部门（预算维度）。
pub const REPORT_TARGET: scope::ScopeTarget =
    scope::ScopeTarget::new(TABLE, "e", Some("dept_id"), None);

/// 费用类型分页锚点：运维级主数据，不做数据裁剪。
pub const TYPE_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TYPE_TABLE);

/// 报销单列表默认排序（新的在前；同期内按单号倒序保证翻页稳定）。
pub const REPORT_ORDER: &str = "ORDER BY e.expense_date DESC, e.report_no DESC, e.id DESC";

fn parse_amount(text: &str) -> Decimal {
    text.parse::<Decimal>().unwrap_or(Decimal::ZERO)
}

fn opt_text(value: Option<&str>) -> Value {
    Value::Text(value.map(str::trim).unwrap_or_default().to_string())
}

fn opt_uuid(value: Option<&Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

fn bool_int(value: bool) -> i16 {
    i16::from(value)
}

// ==================== ① 费用类型 ====================

const TYPE_COLUMNS: &str = "t.id, t.type_code, t.type_name, t.account_id, t.account_code, \
     t.tax_deductible, t.budget_controlled, t.need_invoice_no, t.amount_limit::text AS amount_limit, \
     t.sort_order, t.status, t.remark, coalesce(a.account_name, '') AS account_name, \
     coalesce(a.balance_direction, '') AS account_direction";

/// 费用类型行（含科目名快照：落库时取科目名写进 `remark` 之外的一列 —— 见 [`type_insert`]）。
#[derive(Debug, Clone)]
pub struct TypeRow {
    /// 主键。
    pub id: Uuid,
    /// 编码。
    pub type_code: String,
    /// 名称。
    pub type_name: String,
    /// 借方科目 id。
    pub account_id: Uuid,
    /// 借方科目编码快照。
    pub account_code: String,
    /// 可抵扣。
    pub tax_deductible: bool,
    /// 受预算控制。
    pub budget_controlled: bool,
    /// 是否必填发票号。
    pub need_invoice_no: bool,
    /// 单笔上限（0 = 不限）。
    pub amount_limit: Decimal,
    /// 排序。
    pub sort_order: i32,
    /// 状态。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 借方科目名称（查询时 join `biz_account` 取，不落冗余列 —— 类型表没有
    /// `account_name` 列，设计 §4.2.1 只存了 `account_code` 快照）。
    pub account_name: String,
    /// 科目方向（`DEBIT` / `CREDIT`，报表 / 校验用）。
    pub account_direction: String,
}

impl TypeRow {
    fn from_row(row: &Row) -> DbResult<TypeRow> {
        Ok(TypeRow {
            id: row.get_typed("id")?,
            type_code: row.get_typed("type_code")?,
            type_name: row.get_typed("type_name")?,
            account_id: row.get_typed("account_id")?,
            account_code: row.get_typed("account_code")?,
            tax_deductible: row.get_typed::<i16>("tax_deductible")? == 1,
            budget_controlled: row.get_typed::<i16>("budget_controlled")? == 1,
            need_invoice_no: row.get_typed::<i16>("need_invoice_no")? == 1,
            amount_limit: parse_amount(&row.get_typed::<String>("amount_limit")?),
            sort_order: row.get_typed("sort_order")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            account_name: row.get_typed("account_name")?,
            account_direction: row.get_typed("account_direction")?,
        })
    }
}

/// 费用类型分页（软删不返回）。
pub async fn type_page(q: &TypeFilter, page: u64, page_size: u64) -> DbResult<(Vec<TypeRow>, i64)> {
    let mut sql = format!(
        "SELECT {TYPE_COLUMNS} FROM {TYPE_TABLE} t \
           LEFT JOIN biz_account a ON a.id = t.account_id \
          WHERE t.deleted_at IS NULL"
    );
    let mut params: Vec<Value> = Vec::new();
    if let Some(keyword) = q
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        sql.push_str(" AND (t.type_code ILIKE ? OR t.type_name ILIKE ?)");
        let like = format!("%{keyword}%");
        params.push(Value::Text(like.clone()));
        params.push(Value::Text(like));
    }
    if let Some(status) = q.status {
        sql.push_str(" AND t.status = ?::int");
        params.push(Value::from(i32::from(status)));
    }
    let order = "ORDER BY t.sort_order ASC, t.type_code ASC";
    let result =
        scope::paginate_unscoped(TYPE_TARGET, &sql, params, Some(order), page, page_size).await?;
    let rows = result
        .list
        .iter()
        .map(TypeRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 启用中的费用类型（建单下拉，不分页）。
pub async fn type_active() -> DbResult<Vec<TypeRow>> {
    let rows = db()
        .query_sql(
            format!(
                "SELECT {TYPE_COLUMNS} FROM {TYPE_TABLE} t \
                   LEFT JOIN biz_account a ON a.id = t.account_id \
                  WHERE t.deleted_at IS NULL AND t.status = 1 \
                  ORDER BY t.sort_order ASC, t.type_code ASC"
            ),
            vec![],
        )
        .await?;
    rows.iter().map(TypeRow::from_row).collect()
}

/// 按 id 取费用类型（软删视为不存在）。
pub async fn type_find(id: Uuid) -> DbResult<Option<TypeRow>> {
    type_find_in(db(), id).await
}

/// 按 id 取费用类型（同事务版）。
pub async fn type_find_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<TypeRow>> {
    // SCOPE-EXEMPT: 主键精确取 1 行。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {TYPE_COLUMNS} FROM {TYPE_TABLE} t \
                   LEFT JOIN biz_account a ON a.id = t.account_id \
                  WHERE t.id = ?::uuid AND t.deleted_at IS NULL"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(TypeRow::from_row).transpose()
}

/// 按编码取费用类型。
pub async fn type_find_by_code(ex: &impl SqlExecutor, code: &str) -> DbResult<Option<TypeRow>> {
    // SCOPE-EXEMPT: 编码唯一（`uk_biz_exp_type_code`），取 1 行。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {TYPE_COLUMNS} FROM {TYPE_TABLE} t \
                   LEFT JOIN biz_account a ON a.id = t.account_id \
                  WHERE t.type_code = ? AND t.deleted_at IS NULL"
            ),
            vec![Value::Text(code.trim().to_ascii_uppercase())],
        )
        .await?;
    rows.first().map(TypeRow::from_row).transpose()
}

/// 该类型被多少张**未作废**报销单引用（删除前判 409）。
pub async fn type_used_count(ex: &impl SqlExecutor, type_id: Uuid) -> DbResult<i64> {
    // SCOPE-EXEMPT: 按类型 id 计数，供 409 判重。
    let rows = ex
        .query_sql(
            "SELECT count(*) AS n FROM biz_expense_item i \
               JOIN biz_expense_report r ON r.id = i.report_id \
              WHERE i.expense_type_id = ?::uuid \
                AND r.deleted_at IS NULL AND r.status <> 'CANCELLED'"
                .to_string(),
            vec![Value::from(&type_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(0),
        Some(row) => row.get_typed("n"),
    }
}

/// 新建费用类型。
pub async fn type_insert(
    ex: &impl SqlExecutor,
    id: Uuid,
    spec: &crate::business::erp::expense_dto::TypeSpec,
    account_code: &str,
    operator: Option<Uuid>,
) -> DbResult<()> {
    // SCOPE-EXEMPT: 主数据写入，无归属列。
    ex.exec_sql(
        format!(
            "INSERT INTO {TYPE_TABLE} \
                (id, type_code, type_name, account_id, account_code, tax_deductible, \
                 budget_controlled, need_invoice_no, amount_limit, sort_order, status, remark, \
                 create_by, update_by) \
             VALUES (?::uuid, ?, ?, ?::uuid, ?, ?::int, ?::int, ?::int, ?::text::numeric, \
                     ?::int, ?::int, NULLIF(?::text,''), NULLIF(?::text,'')::uuid, \
                     NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(spec.type_code.clone()),
            Value::Text(spec.type_name.clone()),
            Value::from(&spec.account_id),
            Value::Text(account_code.to_string()),
            Value::from(i32::from(bool_int(spec.tax_deductible))),
            Value::from(i32::from(bool_int(spec.budget_controlled))),
            Value::from(i32::from(bool_int(spec.need_invoice_no))),
            Value::Text(spec.amount_limit.to_string()),
            Value::from(spec.sort_order),
            Value::from(i32::from(spec.status)),
            opt_text(spec.remark.as_deref()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
        ],
    )
    .await?;
    Ok(())
}

/// 更新费用类型（返回受影响行数）。
pub async fn type_update(
    ex: &impl SqlExecutor,
    id: Uuid,
    spec: &crate::business::erp::expense_dto::TypeSpec,
    account_code: &str,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主数据写入。
    ex.exec_sql(
        format!(
            "UPDATE {TYPE_TABLE} SET type_name = ?, account_id = ?::uuid, account_code = ?, \
                    tax_deductible = ?::int, budget_controlled = ?::int, need_invoice_no = ?::int, \
                    amount_limit = ?::text::numeric, sort_order = ?::int, status = ?::int, \
                    remark = NULLIF(?::text,''), updated_at = now(), \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(spec.type_name.clone()),
            Value::from(&spec.account_id),
            Value::Text(account_code.to_string()),
            Value::from(i32::from(bool_int(spec.tax_deductible))),
            Value::from(i32::from(bool_int(spec.budget_controlled))),
            Value::from(i32::from(bool_int(spec.need_invoice_no))),
            Value::Text(spec.amount_limit.to_string()),
            Value::from(spec.sort_order),
            Value::from(i32::from(spec.status)),
            opt_text(spec.remark.as_deref()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删费用类型（调用方先判引用数）。
pub async fn type_soft_delete(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主数据写入。
    ex.exec_sql(
        format!(
            "UPDATE {TYPE_TABLE} SET deleted_at = now(), updated_at = now(), \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== ② 报销单 ====================

const REPORT_COLUMNS: &str = "e.id, e.report_no, e.reporter_id, e.reporter_name, e.dept_id, \
     e.dept_name, e.title, e.expense_date, e.period_code, e.total_amount::text AS total_amount, \
     e.total_tax::text AS total_tax, e.total_with_tax::text AS total_with_tax, e.item_count, \
     e.over_budget, e.over_amount::text AS over_amount, e.budget_mode, e.status, \
     e.approval_instance_id, e.voucher_id, e.paid_amount::text AS paid_amount, \
     e.unpaid_amount::text AS unpaid_amount, e.pay_status, e.last_pay_date, e.submitted_at, \
     e.submitted_by, e.approved_at, e.approved_by, e.cancel_reason, e.cancelled_at, \
     e.cancelled_by, e.remark, e.created_at, e.create_by";

/// 报销单行（全列，一处映射）。
#[derive(Debug, Clone)]
pub struct ReportRow {
    /// 主键。
    pub id: Uuid,
    /// 单号。
    pub report_no: String,
    /// 报销人。
    pub reporter_id: Uuid,
    /// 报销人姓名快照。
    pub reporter_name: String,
    /// 部门。
    pub dept_id: Uuid,
    /// 部门名快照。
    pub dept_name: String,
    /// 事由。
    pub title: String,
    /// 费用发生日。
    pub expense_date: NaiveDate,
    /// 会计期间。
    pub period_code: String,
    /// 不含税合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计。
    pub total_with_tax: Decimal,
    /// 明细条数。
    pub item_count: i32,
    /// 提交时点是否超预算。
    pub over_budget: bool,
    /// 提交时点超支额。
    pub over_amount: Decimal,
    /// 提交时的控制策略快照。
    pub budget_mode: Option<String>,
    /// 状态。
    pub status: String,
    /// 审批实例。
    pub approval_instance_id: Option<Uuid>,
    /// 计提凭证。
    pub voucher_id: Option<Uuid>,
    /// 已付金额。
    pub paid_amount: Decimal,
    /// 未付金额。
    pub unpaid_amount: Decimal,
    /// 付款状态。
    pub pay_status: String,
    /// 最近付款日。
    pub last_pay_date: Option<NaiveDate>,
    /// 提交时间。
    pub submitted_at: Option<DateTime<Utc>>,
    /// 提交人。
    pub submitted_by: Option<Uuid>,
    /// 审批通过时间。
    pub approved_at: Option<DateTime<Utc>>,
    /// 审批通过操作人。
    pub approved_by: Option<Uuid>,
    /// 作废原因。
    pub cancel_reason: Option<String>,
    /// 作废时间。
    pub cancelled_at: Option<DateTime<Utc>>,
    /// 作废人。
    pub cancelled_by: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 制单人。
    pub create_by: Option<Uuid>,
}

impl ReportRow {
    fn from_row(row: &Row) -> DbResult<ReportRow> {
        Ok(ReportRow {
            id: row.get_typed("id")?,
            report_no: row.get_typed("report_no")?,
            reporter_id: row.get_typed("reporter_id")?,
            reporter_name: row.get_typed("reporter_name")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            title: row.get_typed("title")?,
            expense_date: row.get_typed("expense_date")?,
            period_code: row.get_typed("period_code")?,
            total_amount: parse_amount(&row.get_typed::<String>("total_amount")?),
            total_tax: parse_amount(&row.get_typed::<String>("total_tax")?),
            total_with_tax: parse_amount(&row.get_typed::<String>("total_with_tax")?),
            item_count: row.get_typed("item_count")?,
            over_budget: row.get_typed::<i16>("over_budget")? == 1,
            over_amount: parse_amount(&row.get_typed::<String>("over_amount")?),
            budget_mode: row.get_typed("budget_mode")?,
            status: row.get_typed("status")?,
            approval_instance_id: row.get_typed("approval_instance_id")?,
            voucher_id: row.get_typed("voucher_id")?,
            paid_amount: parse_amount(&row.get_typed::<String>("paid_amount")?),
            unpaid_amount: parse_amount(&row.get_typed::<String>("unpaid_amount")?),
            pay_status: row.get_typed("pay_status")?,
            last_pay_date: row.get_typed("last_pay_date")?,
            submitted_at: row.get_typed("submitted_at")?,
            submitted_by: row.get_typed("submitted_by")?,
            approved_at: row.get_typed("approved_at")?,
            approved_by: row.get_typed("approved_by")?,
            cancel_reason: row.get_typed("cancel_reason")?,
            cancelled_at: row.get_typed("cancelled_at")?,
            cancelled_by: row.get_typed("cancelled_by")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            create_by: row.get_typed("create_by")?,
        })
    }
}

/// 报销单分页（软删不返回；按 `dept_id` 做行级范围）。
pub async fn report_page(
    q: &ReportFilter,
    page: u64,
    page_size: u64,
    principal: &ScopedPrincipal,
) -> DbResult<(Vec<ReportRow>, i64)> {
    let mut sql = format!("SELECT {REPORT_COLUMNS} FROM {TABLE} e WHERE e.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(keyword) = q
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        sql.push_str(
            " AND (e.report_no ILIKE ? OR e.title ILIKE ? OR e.reporter_name ILIKE ? \
               OR e.dept_name ILIKE ?)",
        );
        let like = format!("%{keyword}%");
        for _ in 0..4 {
            params.push(Value::Text(like.clone()));
        }
    }
    if let Some(status) = q
        .status
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        sql.push_str(" AND e.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(pay_status) = q
        .pay_status
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        sql.push_str(" AND e.pay_status = ?");
        params.push(Value::Text(pay_status.to_string()));
    }
    for (raw, column) in [
        (q.reporter_id.as_deref(), "e.reporter_id"),
        (q.dept_id.as_deref(), "e.dept_id"),
    ] {
        if let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) {
            let id = Uuid::parse_str(text).map_err(|err| {
                DbError::InvalidValue(format!("筛选条件 {column} 不是合法 UUID（{text}）：{err}"))
            })?;
            sql.push_str(&format!(" AND {column} = ?::uuid"));
            params.push(Value::from(&id));
        }
    }
    if let Some(period) = q
        .period_code
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        sql.push_str(" AND e.period_code = ?");
        params.push(Value::Text(period.to_string()));
    }
    if let Some(from) = q.date_from {
        sql.push_str(" AND e.expense_date >= ?::text::date");
        params.push(Value::Text(from.to_string()));
    }
    if let Some(to) = q.date_to {
        sql.push_str(" AND e.expense_date <= ?::text::date");
        params.push(Value::Text(to.to_string()));
    }
    let result = scope::paginate(
        REPORT_TARGET,
        &sql,
        params,
        Some(REPORT_ORDER),
        page,
        page_size,
        principal,
    )
    .await?;
    let rows = result
        .list
        .iter()
        .map(ReportRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 「待付款」列表：已审批未付清（`APPROVED`，`pay_status <> 'PAID'`）。
pub async fn report_payable_page(
    page: u64,
    page_size: u64,
    principal: &ScopedPrincipal,
) -> DbResult<(Vec<ReportRow>, i64)> {
    let sql = format!(
        "SELECT {REPORT_COLUMNS} FROM {TABLE} e \
          WHERE e.deleted_at IS NULL AND e.status = 'APPROVED' AND e.pay_status <> 'PAID'"
    );
    let result = scope::paginate(
        REPORT_TARGET,
        &sql,
        vec![],
        Some("ORDER BY e.approved_at ASC, e.report_no ASC"),
        page,
        page_size,
        principal,
    )
    .await?;
    let rows = result
        .list
        .iter()
        .map(ReportRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 按 id 取报销单。
pub async fn report_find(id: Uuid) -> DbResult<Option<ReportRow>> {
    report_find_in(db(), id).await
}

/// 按 id 取报销单（同事务版）。
pub async fn report_find_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ReportRow>> {
    // SCOPE-EXEMPT: 主键精确取 1 行（数据范围在列表 / 详情入口已判）。
    let rows = ex
        .query_sql(
            format!("SELECT {REPORT_COLUMNS} FROM {TABLE} e WHERE e.id = ?::uuid AND e.deleted_at IS NULL"),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(ReportRow::from_row).transpose()
}

/// 按 id 取报销单并**加行锁**（付款 / 入账 / 作废必须先锁单）。
///
/// 锁的理由见模块文档第 3 条：付款是拒绝型操作，不锁会两笔付款同时通过 `unpaid` 校验。
pub async fn report_lock_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ReportRow>> {
    // SCOPE-EXEMPT: 主键精确取 1 行 + 行锁。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {REPORT_COLUMNS} FROM {TABLE} e \
                  WHERE e.id = ?::uuid AND e.deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(ReportRow::from_row).transpose()
}

/// 按审批实例取报销单（回调入口：实例 → 单据）。
pub async fn report_by_instance(
    ex: &impl SqlExecutor,
    instance_id: Uuid,
) -> DbResult<Option<ReportRow>> {
    // SCOPE-EXEMPT: 实例 id 精确取 1 行。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {REPORT_COLUMNS} FROM {TABLE} e \
                  WHERE e.approval_instance_id = ?::uuid AND e.deleted_at IS NULL"
            ),
            vec![Value::from(&instance_id)],
        )
        .await?;
    rows.first().map(ReportRow::from_row).transpose()
}

/// 新建报销单的写库载荷（单头 + 明细一次给全）。
#[derive(Debug, Clone)]
pub struct ReportInsert {
    /// 主键。
    pub id: Uuid,
    /// 单号。
    pub report_no: String,
    /// 报销人。
    pub reporter_id: Uuid,
    /// 报销人姓名。
    pub reporter_name: String,
    /// 部门。
    pub dept_id: Uuid,
    /// 部门名。
    pub dept_name: String,
    /// 事由。
    pub title: String,
    /// 费用发生日。
    pub expense_date: NaiveDate,
    /// 会计期间（提交前 = 费用发生日所属期间；入账时按审批通过日重算）。
    pub period_code: String,
    /// 不含税合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计。
    pub total_with_tax: Decimal,
    /// 明细条数。
    pub item_count: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 明细（已拆价税）。
    pub items: Vec<ItemInsert>,
}

/// 一条明细的写库载荷（类型 / 科目快照已由 handler 填好）。
#[derive(Debug, Clone)]
pub struct ItemInsert {
    /// 行号（从 1 开始）。
    pub line_no: i32,
    /// 费用发生日。
    pub expense_date: NaiveDate,
    /// 费用类型 id。
    pub expense_type_id: Uuid,
    /// 类型编码快照。
    pub type_code: String,
    /// 类型名快照。
    pub type_name: String,
    /// 借方科目 id。
    pub account_id: Uuid,
    /// 借方科目编码快照。
    pub account_code: String,
    /// 借方科目名快照。
    pub account_name: String,
    /// 不含税金额。
    pub amount: Decimal,
    /// 税率。
    pub tax_rate: Decimal,
    /// 税额。
    pub tax_amount: Decimal,
    /// 价税合计。
    pub with_tax_amount: Decimal,
    /// 发票号。
    pub invoice_no: Option<String>,
    /// 开票日期。
    pub invoice_date: Option<NaiveDate>,
    /// 发票类型。
    pub invoice_type: Option<String>,
    /// 行摘要。
    pub summary: String,
    /// 行备注。
    pub remark: Option<String>,
}

/// 插单头（明细另插，见 [`items_insert_many`]；两者必须同一事务）。
pub async fn report_insert(
    ex: &impl SqlExecutor,
    insert: &ReportInsert,
    operator: Option<Uuid>,
) -> DbResult<()> {
    // SCOPE-EXEMPT: 主表写入。
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                (id, report_no, reporter_id, reporter_name, dept_id, dept_name, title, \
                 expense_date, period_code, total_amount, total_tax, total_with_tax, item_count, \
                 status, paid_amount, unpaid_amount, pay_status, remark, create_by, update_by) \
             VALUES (?::uuid, ?, ?::uuid, ?, ?::uuid, ?, ?, ?::text::date, ?, ?::text::numeric, \
                     ?::text::numeric, ?::text::numeric, ?::int, 'DRAFT', 0, ?::text::numeric, \
                     'UNPAID', NULLIF(?::text,''), NULLIF(?::text,'')::uuid, \
                     NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&insert.id),
            Value::Text(insert.report_no.clone()),
            Value::from(&insert.reporter_id),
            Value::Text(insert.reporter_name.clone()),
            Value::from(&insert.dept_id),
            Value::Text(insert.dept_name.clone()),
            Value::Text(insert.title.clone()),
            Value::Text(insert.expense_date.to_string()),
            Value::Text(insert.period_code.clone()),
            Value::Text(insert.total_amount.to_string()),
            Value::Text(insert.total_tax.to_string()),
            Value::Text(insert.total_with_tax.to_string()),
            Value::from(insert.item_count),
            Value::Text(insert.total_with_tax.to_string()),
            opt_text(insert.remark.as_deref()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
        ],
    )
    .await?;
    Ok(())
}

/// 改草稿单头（只允许 `DRAFT`；金额三件套与条数一起换）。
pub async fn report_update_draft(
    ex: &impl SqlExecutor,
    id: Uuid,
    insert: &ReportInsert,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入（状态 CAS 在 WHERE 里）。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET reporter_id = ?::uuid, reporter_name = ?, dept_id = ?::uuid, \
                    dept_name = ?, title = ?, expense_date = ?::text::date, period_code = ?, \
                    total_amount = ?::text::numeric, total_tax = ?::text::numeric, \
                    total_with_tax = ?::text::numeric, item_count = ?::int, \
                    unpaid_amount = ?::text::numeric, remark = NULLIF(?::text,''), \
                    updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = 'DRAFT'"
        ),
        vec![
            Value::from(&insert.reporter_id),
            Value::Text(insert.reporter_name.clone()),
            Value::from(&insert.dept_id),
            Value::Text(insert.dept_name.clone()),
            Value::Text(insert.title.clone()),
            Value::Text(insert.expense_date.to_string()),
            Value::Text(insert.period_code.clone()),
            Value::Text(insert.total_amount.to_string()),
            Value::Text(insert.total_tax.to_string()),
            Value::Text(insert.total_with_tax.to_string()),
            Value::from(insert.item_count),
            // 草稿未付款：unpaid 恒等于价税合计（`ck_biz_exp_report_paid` 要求三者自洽）
            Value::Text(insert.total_with_tax.to_string()),
            opt_text(insert.remark.as_deref()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

/// 批量插明细。
pub async fn items_insert_many(
    ex: &impl SqlExecutor,
    report_id: Uuid,
    items: &[ItemInsert],
) -> DbResult<()> {
    for item in items {
        // SCOPE-EXEMPT: 子表写入（随主单）。
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_TABLE} \
                    (report_id, line_no, expense_date, expense_type_id, type_code, type_name, \
                     account_id, account_code, account_name, amount, tax_rate, tax_amount, \
                     with_tax_amount, invoice_no, invoice_date, invoice_type, summary, remark) \
                 VALUES (?::uuid, ?::int, ?::text::date, ?::uuid, ?, ?, ?::uuid, ?, ?, \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                         NULLIF(?::text,''), NULLIF(?::text,'')::text::date, \
                         NULLIF(?::text,''), ?, NULLIF(?::text,''))"
            ),
            vec![
                Value::from(&report_id),
                Value::from(item.line_no),
                Value::Text(item.expense_date.to_string()),
                Value::from(&item.expense_type_id),
                Value::Text(item.type_code.clone()),
                Value::Text(item.type_name.clone()),
                Value::from(&item.account_id),
                Value::Text(item.account_code.clone()),
                Value::Text(item.account_name.clone()),
                Value::Text(item.amount.to_string()),
                Value::Text(item.tax_rate.to_string()),
                Value::Text(item.tax_amount.to_string()),
                Value::Text(item.with_tax_amount.to_string()),
                opt_text(item.invoice_no.as_deref()),
                Value::Text(item.invoice_date.map(|d| d.to_string()).unwrap_or_default()),
                opt_text(item.invoice_type.as_deref()),
                Value::Text(item.summary.clone()),
                opt_text(item.remark.as_deref()),
            ],
        )
        .await?;
    }
    Ok(())
}

/// 删掉某张单的全部明细（改草稿时「先删后插」的前半步）。
pub async fn items_delete_of(ex: &impl SqlExecutor, report_id: Uuid) -> DbResult<u64> {
    // SCOPE-EXEMPT: 子表写入（随主单）。
    ex.exec_sql(
        format!("DELETE FROM {ITEM_TABLE} WHERE report_id = ?::uuid"),
        vec![Value::from(&report_id)],
    )
    .await
}

const ITEM_COLUMNS: &str = "i.id, i.line_no, i.expense_date, i.expense_type_id, i.type_code, \
     i.type_name, i.account_id, i.account_code, i.account_name, i.amount::text AS amount, \
     i.tax_rate::text AS tax_rate, i.tax_amount::text AS tax_amount, \
     i.with_tax_amount::text AS with_tax_amount, i.invoice_no, i.invoice_date, i.invoice_type, \
     i.summary, i.remark";

/// 明细行。
#[derive(Debug, Clone)]
pub struct ItemRow {
    /// 主键。
    pub id: Uuid,
    /// 行号。
    pub line_no: i32,
    /// 费用发生日。
    pub expense_date: NaiveDate,
    /// 费用类型 id。
    pub expense_type_id: Uuid,
    /// 类型编码快照。
    pub type_code: String,
    /// 类型名快照。
    pub type_name: String,
    /// 借方科目 id。
    pub account_id: Uuid,
    /// 借方科目编码快照。
    pub account_code: String,
    /// 借方科目名快照。
    pub account_name: String,
    /// 不含税金额。
    pub amount: Decimal,
    /// 税率。
    pub tax_rate: Decimal,
    /// 税额。
    pub tax_amount: Decimal,
    /// 价税合计。
    pub with_tax_amount: Decimal,
    /// 发票号。
    pub invoice_no: Option<String>,
    /// 开票日期。
    pub invoice_date: Option<NaiveDate>,
    /// 发票类型。
    pub invoice_type: Option<String>,
    /// 行摘要。
    pub summary: String,
    /// 行备注。
    pub remark: Option<String>,
}

impl ItemRow {
    fn from_row(row: &Row) -> DbResult<ItemRow> {
        Ok(ItemRow {
            id: row.get_typed("id")?,
            line_no: row.get_typed("line_no")?,
            expense_date: row.get_typed("expense_date")?,
            expense_type_id: row.get_typed("expense_type_id")?,
            type_code: row.get_typed("type_code")?,
            type_name: row.get_typed("type_name")?,
            account_id: row.get_typed("account_id")?,
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            amount: parse_amount(&row.get_typed::<String>("amount")?),
            tax_rate: parse_amount(&row.get_typed::<String>("tax_rate")?),
            tax_amount: parse_amount(&row.get_typed::<String>("tax_amount")?),
            with_tax_amount: parse_amount(&row.get_typed::<String>("with_tax_amount")?),
            invoice_no: row.get_typed("invoice_no")?,
            invoice_date: row.get_typed("invoice_date")?,
            invoice_type: row.get_typed("invoice_type")?,
            summary: row.get_typed("summary")?,
            remark: row.get_typed("remark")?,
        })
    }
}

/// 某张单的明细（按行号）。
pub async fn items_of(ex: &impl SqlExecutor, report_id: Uuid) -> DbResult<Vec<ItemRow>> {
    // SCOPE-EXEMPT: 子表按主键读（主单已在入口判过范围）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {ITEM_COLUMNS} FROM {ITEM_TABLE} i \
                  WHERE i.report_id = ?::uuid ORDER BY i.line_no"
            ),
            vec![Value::from(&report_id)],
        )
        .await?;
    rows.iter().map(ItemRow::from_row).collect()
}

/// 明细合计与条数（巡检 L1 / 提交前复算）。
pub async fn items_sum(
    ex: &impl SqlExecutor,
    report_id: Uuid,
) -> DbResult<(Decimal, Decimal, i32)> {
    // SCOPE-EXEMPT: 子表聚合。
    let rows = ex
        .query_sql(
            "SELECT coalesce(sum(with_tax_amount),0)::text AS total, \
                    coalesce(sum(tax_amount),0)::text AS tax, count(*)::int AS n \
               FROM biz_expense_item WHERE report_id = ?::uuid"
                .to_string(),
            vec![Value::from(&report_id)],
        )
        .await?;
    match rows.first() {
        None => Ok((Decimal::ZERO, Decimal::ZERO, 0)),
        Some(row) => Ok((
            parse_amount(&row.get_typed::<String>("total")?),
            parse_amount(&row.get_typed::<String>("tax")?),
            row.get_typed("n")?,
        )),
    }
}

/// 提交：置 `SUBMITTED` + 挂实例 + 冻结预算快照（**只允许从 DRAFT**）。
#[allow(clippy::too_many_arguments)]
pub async fn report_mark_submitted(
    ex: &impl SqlExecutor,
    id: Uuid,
    instance_id: Uuid,
    over_budget: bool,
    over_amount: Decimal,
    budget_mode: Option<&str>,
    operator: Uuid,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入（状态 CAS 在 WHERE 里）。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET status = 'SUBMITTED', approval_instance_id = ?::uuid, \
                    over_budget = ?::int, over_amount = ?::text::numeric, budget_mode = ?, \
                    submitted_at = ?::text::timestamptz, submitted_by = ?::uuid, \
                    updated_at = now(), update_by = ?::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = 'DRAFT'"
        ),
        vec![
            Value::from(&instance_id),
            Value::from(i32::from(bool_int(over_budget))),
            Value::Text(over_amount.to_string()),
            opt_text(budget_mode),
            Value::Text(now.to_rfc3339()),
            Value::from(&operator),
            Value::from(&operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 驳回 / 撤回：回 `DRAFT` 并清空实例，意见追加到备注（**只允许从 SUBMITTED**）。
pub async fn report_mark_draft(
    ex: &impl SqlExecutor,
    id: Uuid,
    append_remark: Option<&str>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入（状态 CAS 在 WHERE 里）。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET status = 'DRAFT', approval_instance_id = NULL, \
                    over_budget = 0, over_amount = 0, budget_mode = NULL, \
                    remark = CASE WHEN NULLIF(?::text,'') IS NULL THEN remark \
                                  ELSE coalesce(remark || E'\\n', '') || ?::text END, \
                    updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = 'SUBMITTED'"
        ),
        vec![
            opt_text(append_remark),
            opt_text(append_remark),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

/// 审批通过：写入计提凭证 + 入账期间（**只允许从 SUBMITTED**）。
pub async fn report_mark_approved(
    ex: &impl SqlExecutor,
    id: Uuid,
    voucher_id: Uuid,
    period_code: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入（状态 CAS 在 WHERE 里）。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET status = 'APPROVED', voucher_id = ?::uuid, period_code = ?, \
                    approved_at = ?::text::timestamptz, approved_by = NULLIF(?::text,'')::uuid, \
                    updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = 'SUBMITTED'"
        ),
        vec![
            Value::from(&voucher_id),
            Value::Text(period_code.to_string()),
            Value::Text(now.to_rfc3339()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

/// 作废：写原因 / 时间 / 人（状态由调用方先过状态机；`PAID` 不许作废）。
///
/// `from_status` 是调用方判定的当前状态（`DRAFT` / `SUBMITTED` / `APPROVED`），
/// 写进 `WHERE` 做 CAS：两个人同时点作废时第二个人影响 0 行 → 调用方报 409。
pub async fn report_cancel_from(
    ex: &impl SqlExecutor,
    id: Uuid,
    from_status: &str,
    reason: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入（状态 CAS 在 WHERE 里）。
    ex.exec_sql(
        format!(
            // `approval_instance_id` 必须一起清空：`ck_biz_exp_report_inst` 要求
            // 「非 SUBMITTED/APPROVED/PAID 的单必须没有实例」（保留实例只对已入账终态成立）。
            // 审计轨迹不会丢：审批实例本身、`biz_status_log` 与 `biz_flow_instance` 都还在。
            "UPDATE {TABLE} SET status = 'CANCELLED', cancel_reason = ?, \
                    approval_instance_id = NULL, \
                    cancelled_at = ?::text::timestamptz, cancelled_by = NULLIF(?::text,'')::uuid, \
                    updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = ?"
        ),
        vec![
            Value::Text(reason.to_string()),
            Value::Text(now.to_rfc3339()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
            Value::Text(from_status.to_string()),
        ],
    )
    .await
}

/// 软删草稿（只有 DRAFT 能删；已提交 / 已入账的走作废）。
pub async fn report_soft_delete(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入（状态 CAS 在 WHERE 里）。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET deleted_at = now(), updated_at = now(), \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = 'DRAFT'"
        ),
        vec![
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
        ],
    )
    .await
}

/// 付款回写：`paid_amount += x`、`unpaid -= x`、`pay_status` 重算、付清则状态 `PAID`。
///
/// **调用方必须已持有行锁**（[`report_lock_in`]）并在同一事务里先落付款记录 + 付款凭证。
/// `new_status` 由调用方按 [`crate::business::erp::expense_compute::pay_status_of`] 算好，
/// 状态机流转（`APPROVED → PAID`）另外走 `transition_if_changed`。
pub async fn report_apply_payment(
    ex: &impl SqlExecutor,
    id: Uuid,
    amount: Decimal,
    pay_date: NaiveDate,
    new_pay_status: &str,
    new_status: &str,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主表写入（金额自洽由库 CHECK 兜底）。
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET paid_amount = paid_amount + ?::text::numeric, \
                    unpaid_amount = unpaid_amount - ?::text::numeric, \
                    pay_status = ?, status = ?, last_pay_date = ?::text::date, \
                    updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL \
                AND paid_amount + ?::text::numeric <= total_with_tax"
        ),
        vec![
            Value::Text(amount.to_string()),
            Value::Text(amount.to_string()),
            Value::Text(new_pay_status.to_string()),
            Value::Text(new_status.to_string()),
            Value::Text(pay_date.to_string()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            Value::from(&id),
            Value::Text(amount.to_string()),
        ],
    )
    .await
}

// ==================== ③ 付款记录 ====================

const PAY_COLUMNS: &str = "p.id, p.payment_no, p.report_id, p.amount::text AS amount, \
     p.pay_date, p.pay_method, p.pay_account_id, p.voucher_id, p.period_code, p.status, \
     p.remark, p.created_at, a.account_code AS pay_account_code";

/// 付款记录行。
#[derive(Debug, Clone)]
pub struct PaymentRow {
    /// 主键。
    pub id: Uuid,
    /// 付款单号。
    pub payment_no: String,
    /// 报销单。
    pub report_id: Uuid,
    /// 金额。
    pub amount: Decimal,
    /// 付款日期。
    pub pay_date: NaiveDate,
    /// 付款方式。
    pub pay_method: Option<String>,
    /// 付款账户 id。
    pub pay_account_id: Option<Uuid>,
    /// 付款凭证。
    pub voucher_id: Option<Uuid>,
    /// 会计期间。
    pub period_code: String,
    /// 状态。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 付款账户科目编码（join 出来，详情页展示）。
    pub pay_account_code: Option<String>,
}

impl PaymentRow {
    fn from_row(row: &Row) -> DbResult<PaymentRow> {
        Ok(PaymentRow {
            id: row.get_typed("id")?,
            payment_no: row.get_typed("payment_no")?,
            report_id: row.get_typed("report_id")?,
            amount: parse_amount(&row.get_typed::<String>("amount")?),
            pay_date: row.get_typed("pay_date")?,
            pay_method: row.get_typed("pay_method")?,
            pay_account_id: row.get_typed("pay_account_id")?,
            voucher_id: row.get_typed("voucher_id")?,
            period_code: row.get_typed("period_code")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            pay_account_code: row.get_typed("pay_account_code")?,
        })
    }
}

/// 付款记录写库载荷。
#[derive(Debug, Clone)]
pub struct PaymentInsert {
    /// 主键。
    pub id: Uuid,
    /// 付款单号。
    pub payment_no: String,
    /// 金额。
    pub amount: Decimal,
    /// 付款日期。
    pub pay_date: NaiveDate,
    /// 付款方式。
    pub pay_method: String,
    /// 付款账户 id。
    pub pay_account_id: Option<Uuid>,
    /// 付款凭证。
    pub voucher_id: Option<Uuid>,
    /// 会计期间。
    pub period_code: String,
    /// 备注。
    pub remark: Option<String>,
}

/// 插一条付款记录。
pub async fn payment_insert(
    ex: &impl SqlExecutor,
    report_id: Uuid,
    insert: &PaymentInsert,
    operator: Option<Uuid>,
) -> DbResult<()> {
    // SCOPE-EXEMPT: 子表写入（随主单）。
    ex.exec_sql(
        format!(
            "INSERT INTO {PAY_TABLE} \
                (id, payment_no, report_id, amount, pay_date, pay_method, pay_account_id, \
                 voucher_id, period_code, status, remark, create_by, update_by) \
             VALUES (?::uuid, ?, ?::uuid, ?::text::numeric, ?::text::date, ?, \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, ?, 'PAID', \
                     NULLIF(?::text,''), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&insert.id),
            Value::Text(insert.payment_no.clone()),
            Value::from(&report_id),
            Value::Text(insert.amount.to_string()),
            Value::Text(insert.pay_date.to_string()),
            opt_text(Some(insert.pay_method.as_str())),
            opt_uuid(insert.pay_account_id.as_ref()),
            opt_uuid(insert.voucher_id.as_ref()),
            Value::Text(insert.period_code.clone()),
            opt_text(insert.remark.as_deref()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
            opt_text(operator.map(|id| id.to_string()).as_deref()),
        ],
    )
    .await?;
    Ok(())
}

/// 某张单的付款记录（按付款日期）。
pub async fn payments_of(ex: &impl SqlExecutor, report_id: Uuid) -> DbResult<Vec<PaymentRow>> {
    // SCOPE-EXEMPT: 子表按主键读。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {PAY_COLUMNS} FROM {PAY_TABLE} p \
                   LEFT JOIN biz_account a ON a.id = p.pay_account_id \
                  WHERE p.report_id = ?::uuid AND p.deleted_at IS NULL \
                  ORDER BY p.pay_date ASC, p.created_at ASC"
            ),
            vec![Value::from(&report_id)],
        )
        .await?;
    rows.iter().map(PaymentRow::from_row).collect()
}

/// 某张单**未红冲**的计提凭证 id（作废时要逐张红冲）。
pub async fn blue_voucher_ids(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Uuid,
) -> DbResult<Vec<Uuid>> {
    // SCOPE-EXEMPT: 凭证按来源反查（红冲用）。
    let rows = ex
        .query_sql(
            "SELECT id FROM biz_voucher \
              WHERE source_type = ? AND source_id = ?::uuid \
                AND is_red = 0 AND reversed_by IS NULL AND deleted_at IS NULL \
              ORDER BY created_at ASC"
                .to_string(),
            vec![
                Value::Text(source_type.to_string()),
                Value::from(&source_id),
            ],
        )
        .await?;
    rows.iter().map(|row| row.get_typed::<Uuid>("id")).collect()
}

/// 已审批未付清的报销单（提醒 / 巡检用；`days` 天前审批通过的）。
pub async fn payable_older_than(ex: &impl SqlExecutor, days: i32) -> DbResult<Vec<ReportRow>> {
    // SCOPE-EXEMPT: 定时任务 / 巡检全库扫描。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {REPORT_COLUMNS} FROM {TABLE} e \
                  WHERE e.deleted_at IS NULL AND e.status = 'APPROVED' \
                    AND e.pay_status <> 'PAID' \
                    AND e.approved_at IS NOT NULL \
                    AND e.approved_at < now() - (?::int || ' days')::interval \
                  ORDER BY e.approved_at ASC"
            ),
            vec![Value::from(days)],
        )
        .await?;
    rows.iter().map(ReportRow::from_row).collect()
}

#[cfg(test)]
#[path = "expense_model_tests.rs"]
mod expense_model_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// ① 两个号段的形态（`format_doc_no` 的口径）：EXP / PAY-EXP 分开滚。
    #[test]
    fn seq_specs_are_distinct() {
        assert_eq!(SEQ.seq_type, "EXP");
        assert_eq!(SEQ.prefix, "EXP");
        assert_eq!(PAY_SEQ.seq_type, "EXP_PAY");
        assert_eq!(PAY_SEQ.prefix, "PAY-EXP");
        assert_ne!(SEQ.seq_type, PAY_SEQ.seq_type);
    }

    /// ② 数据权限锚点必须是 `dept_id`（报销按部门裁）；类型表不裁。
    #[test]
    fn scope_targets() {
        assert!(REPORT_TARGET.is_scoped());
        assert!(!TYPE_TARGET.is_scoped());
    }
}
