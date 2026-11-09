//! 应付单查询（批次 D · P2-C2）：**只读**接口 —— 应付分页 + 应付详情（含关联凭证号）。
//!
//! 表结构以 `backend/migrations/business/V0148__erp_payable.up.sql` 为准
//! （2026-10-31 开工 `\d biz_payable` 实测，24 列）：
//!
//! ```text
//! biz_payable  id, payable_no, supplier_id, source_type, source_id, source_no,
//!              invoice_no, invoice_date, currency_code, amount, paid_amount, unpaid_amount,
//!              credit_days, due_date, period_code, status(OPEN/PARTIAL_PAID/SETTLED/CANCELLED),
//!              settled_at, voucher_id, remark + 审计五件套
//!              CHECK unpaid_amount = amount - paid_amount            ← 恒等
//!              CHECK paid_amount BETWEEN 0 AND amount
//!              CHECK status <> 'SETTLED' OR unpaid_amount = 0
//!              UNIQUE (source_type, source_id) WHERE deleted_at IS NULL
//! ```
//!
//! # 接口（卡面 §二 ①②）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/payable/page` | `business:erp:payable:list` | 分页（状态 / 供应商 / 单号 / 来源类型 / 到期日区间） |
//! | GET | `/business/erp/payable/{id}` | `business:erp:payable:detail` | 详情 + **关联凭证号** |
//!
//! # 只读
//!
//! 本模块**没有**写路径：应付由 P2-C1 的 [`crate::business::erp::payable`] 在收货过账的同一事务里生成，
//! 作废也由那边联动。付款 / 作废应付（P7）在
//! [`crate::business::erp::payable_payment`]，本模块只负责「详情里把付款记录一起读出来」。
//!
//! # 与卡面的三处口径差异
//!
//! 1. **列表分页走 `scope::paginate_unscoped`**（卡面 §四 要求「走 scope::paginate」）：
//!    应付是财务台账，V0148 无 `dept_id` / 归属人列，数据范围（SELF / DEPT_*）表达不出来，
//!    与 P2-D1 的 [`crate::business::erp::voucher_query`] 同口径。
//! 2. **`status` 只做白名单校验**（`OPEN/PARTIAL_PAID/SETTLED/CANCELLED`）：非法值返回 `400 + 10001`，
//!    不静默返回空列表 —— 空列表会让前端以为「这单不存在」，是静默错数据。
//! 3. **`supplier_id` 非法 UUID 同样 400**（卡面没提，但静默忽略等于「按供应商筛」退化成「全部」）。
//! 4. **多两个卡面没列的筛选：`due_date_from` / `due_date_to`（到期日区间，含两端）**。
//!    这是 P2-E2 的前端应付页（`frontend/src/views/business/erp/payable/payable-logic.ts`）
//!    真在发的参数 —— 后端不接就是「前端筛了等于没筛」的静默错数据。非法日期同样 400。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: biz_payable 是应付账款台账（V0148 建表，无 dept_id / 归属人列），数据范围
//! （SELF / DEPT_*）表达不出来；可见性由 business:erp:payable:* 权限码控制（本卡 V0151 落地）。
//! SCOPE-EXEMPT: biz_supplier 是供应商主数据（全局），没有归属列（与 payable.rs 同口径）。
//! SCOPE-EXEMPT: biz_voucher 是会计台账（V0149 建表），详情取凭证号用，同样没有归属列。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::model::normalize_page;
use crate::business::erp::payable;
use crate::business::shared::scope;
use crate::shared::error_code;
use crate::utils;

// ==================== 表名 / 常量 ====================

/// 应付单主表。
pub const TABLE: &str = "biz_payable";

/// 供应商主数据表。
pub const SUPPLIER_TABLE: &str = "biz_supplier";

/// 凭证表（详情取凭证号）。
pub const VOUCHER_TABLE: &str = "biz_voucher";

/// 状态白名单（与 `ck_biz_payable_status` 逐字一致）。P-FLOW-3B 加 `PENDING_CANCEL`。
pub const STATUSES: [&str; 5] = [
    payable::STATUS_OPEN,
    payable::STATUS_PARTIAL_PAID,
    payable::STATUS_SETTLED,
    payable::STATUS_CANCELLED,
    payable::STATUS_PENDING_CANCEL,
];

/// 列表选择列表：应付列 + 供应商名称（LEFT JOIN，供应商被软删时回落空串）。
pub const LIST_COLUMNS: &str = "p.id, p.payable_no, p.supplier_id, \
     COALESCE(s.supplier_name, '') AS supplier_name, \
     p.source_type, p.source_id, p.source_no, p.invoice_no, p.invoice_date, \
     p.currency_code::text AS currency_code, p.amount, p.paid_amount, p.unpaid_amount, \
     p.credit_days, p.due_date, p.period_code, p.status, p.settled_at, p.voucher_id, p.remark, \
     p.cancel_instance_id, p.cancel_reason, \
     p.created_at, p.updated_at";

/// 列表 FROM 子句（别名 `p` / `s` 必须与 [`LIST_COLUMNS`] 一致）。
pub const LIST_FROM: &str = "FROM biz_payable p \
     LEFT JOIN biz_supplier s ON s.id = p.supplier_id AND s.deleted_at IS NULL";

/// 分页排序：创建时间倒序，同一时刻按单号倒序（最新应付在前，排序稳定）。
pub const LIST_ORDER: &str = "ORDER BY p.created_at DESC, p.payable_no DESC";

/// 分页目标（`unscoped`：财务台账无归属列，理由见模块文档的 `SCOPE-EXEMPT`）。
pub const LIST_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

// ==================== 入参 ====================

/// 应付列表筛选（数据层形态）。
#[derive(Debug, Clone, Default)]
pub struct PayableFilter {
    /// 状态（精确，白名单）。
    pub status: Option<String>,
    /// 供应商（精确 UUID）。
    pub supplier_id: Option<Uuid>,
    /// 应付单号（**模糊** `%x%`）。
    pub payable_no: Option<String>,
    /// 来源单据类型（`STOCK_IN` …，精确）。
    pub source_type: Option<String>,
    /// 到期日下界（含）。
    pub due_date_from: Option<NaiveDate>,
    /// 到期日上界（含）。
    pub due_date_to: Option<NaiveDate>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 应付列表 Query（`page` / `page_size` 缺失时由 [`normalize_page`] 规整）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PayablePageQuery {
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 状态。
    pub status: Option<String>,
    /// 供应商 id。
    pub supplier_id: Option<String>,
    /// 应付单号（模糊）。
    pub payable_no: Option<String>,
    /// 来源单据类型。
    pub source_type: Option<String>,
    /// 到期日下界 `YYYY-MM-DD`（含，P2-E2 前端「到期日区间」在用）。
    pub due_date_from: Option<String>,
    /// 到期日上界 `YYYY-MM-DD`（含）。
    pub due_date_to: Option<String>,
}

impl PayablePageQuery {
    /// 入参校验：状态白名单 + 供应商 UUID 合法（非法一律 400，绝不静默忽略）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(status) = blank_to_none(self.status.as_deref())
            && !STATUSES.contains(&status.as_str())
        {
            return Err(bad_request(format!(
                "status 只允许 {}，收到「{status}」",
                STATUSES.join(" / ")
            )));
        }
        if let Some(supplier_id) = blank_to_none(self.supplier_id.as_deref()) {
            parse_uuid(&supplier_id, "supplier_id")?;
        }
        if let Some(raw) = blank_to_none(self.due_date_from.as_deref()) {
            parse_date(&raw, "due_date_from")?;
        }
        if let Some(raw) = blank_to_none(self.due_date_to.as_deref()) {
            parse_date(&raw, "due_date_to")?;
        }
        Ok(())
    }

    /// 转数据层筛选条件（`None` / 空串统一成「不筛」，空白串不当成条件）。
    pub fn to_filter(&self) -> std::result::Result<PayableFilter, ApiError> {
        let supplier_id = match blank_to_none(self.supplier_id.as_deref()) {
            Some(raw) => Some(parse_uuid(&raw, "supplier_id")?),
            None => None,
        };
        let due_date_from = match blank_to_none(self.due_date_from.as_deref()) {
            Some(raw) => Some(parse_date(&raw, "due_date_from")?),
            None => None,
        };
        let due_date_to = match blank_to_none(self.due_date_to.as_deref()) {
            Some(raw) => Some(parse_date(&raw, "due_date_to")?),
            None => None,
        };
        Ok(PayableFilter {
            status: blank_to_none(self.status.as_deref()),
            supplier_id,
            payable_no: blank_to_none(self.payable_no.as_deref()),
            source_type: blank_to_none(self.source_type.as_deref()),
            due_date_from,
            due_date_to,
            page: self.page,
            page_size: self.page_size,
        })
    }
}

/// 空白串 → `None`（`?status=` 不该变成「筛状态 = 空串」）。
fn blank_to_none(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

/// 参数类 400（`10001`）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 读：行结构 ====================

/// `biz_payable` 行 + 供应商名称。
#[derive(Debug, Clone)]
pub struct PayableRow {
    /// 主键。
    pub id: Uuid,
    /// 应付单号。
    pub payable_no: String,
    /// 供应商。
    pub supplier_id: Uuid,
    /// 供应商名称（LEFT JOIN，缺失为空串）。
    pub supplier_name: String,
    /// 来源单据类型。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Uuid,
    /// 来源单号。
    pub source_no: String,
    /// 发票号（批次 2 回填）。
    pub invoice_no: Option<String>,
    /// 发票日期。
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
    /// 作废审批实例 id（P-FLOW-3B）：`PENDING_CANCEL` 时必有值（库侧 `ck_biz_payable_pending_cancel`）。
    pub cancel_instance_id: Option<Uuid>,
    /// 作废原因（P-FLOW-3B）：送审时填的那段文字。
    pub cancel_reason: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl PayableRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            payable_no: row.get_typed("payable_no")?,
            supplier_id: row.get_typed("supplier_id")?,
            supplier_name: row.get_typed("supplier_name")?,
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
            cancel_instance_id: row.get_typed("cancel_instance_id")?,
            cancel_reason: row.get_typed("cancel_reason")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

// ==================== 出参 ====================

/// 应付（列表行 / 详情单头）。
#[derive(Debug, Clone, Serialize)]
pub struct PayableRes {
    /// 主键。
    pub id: String,
    /// 应付单号。
    pub payable_no: String,
    /// 供应商。
    pub supplier_id: String,
    /// 供应商名称。
    pub supplier_name: String,
    /// 来源单据类型。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: String,
    /// 来源单号。
    pub source_no: String,
    /// 发票号。
    pub invoice_no: Option<String>,
    /// 发票日期。
    pub invoice_date: Option<NaiveDate>,
    /// 币种。
    pub currency_code: String,
    /// 应付金额（含税）。
    pub amount: Decimal,
    /// 已付金额。
    pub paid_amount: Decimal,
    /// 未付金额。
    pub unpaid_amount: Decimal,
    /// 账期天数。
    pub credit_days: i32,
    /// 到期日。
    pub due_date: NaiveDate,
    /// 会计期间。
    pub period_code: String,
    /// 状态。
    pub status: String,
    /// 状态中文标签。
    pub status_label: String,
    /// 是否逾期（`due_date < 今天` 且仍有未付金额且未作废）。
    pub overdue: bool,
    /// 结清时间。
    pub settled_at: Option<DateTime<Utc>>,
    /// 关联凭证。
    pub voucher_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 作废审批实例 id（P-FLOW-3B）：`PENDING_CANCEL` 时有值，撤回 / 审批结束后清空。
    ///
    /// 前端用它显示「作废审批进度」并允许撤回 —— 没有它时只能在**送审那一刻**用响应里的
    /// `approval_instance_id` 显示，重新打开详情抽屉后进度区就消失了。
    pub cancel_instance_id: Option<String>,
    /// 作废原因（P-FLOW-3B）：送审时填的那段文字（审批通过后同时写进 `remark`）。
    pub cancel_reason: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl PayableRes {
    /// 由行构造（`today` 由调用方给，纯函数便于单测）。
    pub fn from_row(row: &PayableRow, today: NaiveDate) -> Self {
        Self {
            id: row.id.to_string(),
            payable_no: row.payable_no.clone(),
            supplier_id: row.supplier_id.to_string(),
            supplier_name: row.supplier_name.clone(),
            source_type: row.source_type.clone(),
            source_id: row.source_id.to_string(),
            source_no: row.source_no.clone(),
            invoice_no: row.invoice_no.clone(),
            invoice_date: row.invoice_date,
            currency_code: row.currency_code.clone(),
            amount: row.amount,
            paid_amount: row.paid_amount,
            unpaid_amount: row.unpaid_amount,
            credit_days: row.credit_days,
            due_date: row.due_date,
            period_code: row.period_code.clone(),
            status: row.status.clone(),
            status_label: status_label(&row.status).to_string(),
            overdue: is_overdue(row, today),
            settled_at: row.settled_at,
            voucher_id: row.voucher_id.map(|id| id.to_string()),
            remark: row.remark.clone(),
            cancel_instance_id: row.cancel_instance_id.map(|id| id.to_string()),
            cancel_reason: row.cancel_reason.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// 应付详情：单头（展开）+ 关联凭证号。
#[derive(Debug, Clone, Serialize)]
pub struct PayableDetailRes {
    /// 单头。
    #[serde(flatten)]
    pub payable: PayableRes,
    /// 关联凭证号（`voucher_id` 为空或凭证已删时是 `None`）。
    pub voucher_no: Option<String>,
    /// 付款记录时间线（P7 起：部分付款可以有多条，`pay_date` 升序）。
    #[serde(default)]
    pub payments: Vec<crate::business::erp::payable_payment::PaymentRes>,
}

// ==================== 标签 / 派生 ====================

/// 应付状态中文标签（未知状态原样返回，不 panic）。
pub fn status_label(status: &str) -> &str {
    match status {
        s if s == payable::STATUS_OPEN => "未付",
        s if s == payable::STATUS_PARTIAL_PAID => "部分已付",
        s if s == payable::STATUS_SETTLED => "已结清",
        s if s == payable::STATUS_CANCELLED => "已作废",
        s if s == payable::STATUS_PENDING_CANCEL => "作废审批中",
        other => other,
    }
}

/// 是否逾期：到期日早于 `today`、仍有未付金额、且未作废。
pub fn is_overdue(row: &PayableRow, today: NaiveDate) -> bool {
    row.status != payable::STATUS_CANCELLED && row.unpaid_amount > Decimal::ZERO && row.due_date < today
}

// ==================== 数据层 ====================

/// 应付分页（**全部状态**；默认不按状态筛，作废单也要看得见 —— 财务要对账）。
pub async fn payable_page(q: &PayableFilter) -> DbResult<(Vec<PayableRow>, i64)> {
    let mut sql = format!("SELECT {LIST_COLUMNS} {LIST_FROM} WHERE p.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND p.status = ?");
        params.push(Value::from(status));
    }
    if let Some(supplier_id) = q.supplier_id {
        sql.push_str(" AND p.supplier_id = ?::uuid");
        params.push(Value::from(&supplier_id));
    }
    if let Some(source_type) = q.source_type.as_deref() {
        sql.push_str(" AND p.source_type = ?");
        params.push(Value::from(source_type));
    }
    if let Some(pattern) = utils::like_pattern(q.payable_no.as_deref()) {
        sql.push_str(" AND (p.payable_no LIKE ? OR p.source_no LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(from) = q.due_date_from {
        sql.push_str(" AND p.due_date >= ?::text::date");
        params.push(Value::Text(from.to_string()));
    }
    if let Some(to) = q.due_date_to {
        sql.push_str(" AND p.due_date <= ?::text::date");
        params.push(Value::Text(to.to_string()));
    }
    let (page, page_size) = normalize_page(q.page, q.page_size);
    let result = scope::paginate_unscoped(LIST_TARGET, &sql, params, Some(LIST_ORDER), page, page_size).await?;
    let rows = result
        .list
        .iter()
        .map(PayableRow::from_row)
        .collect::<DbResult<Vec<PayableRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 单张应付单（未删除）。
pub async fn find_payable(id: Uuid) -> DbResult<Option<PayableRow>> {
    let sql = format!("SELECT {LIST_COLUMNS} {LIST_FROM} WHERE p.id = ?::uuid AND p.deleted_at IS NULL");
    let row = db().query_first_sql(sql, vec![Value::from(id)]).await?;
    row.as_ref().map(PayableRow::from_row).transpose()
}

/// 凭证号（凭证已软删 / 不存在 → `None`，不阻断详情）。
pub async fn voucher_no_of(voucher_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: biz_voucher 是会计台账，没有归属列（见模块文档）。
    let sql = format!("SELECT voucher_no FROM {VOUCHER_TABLE} WHERE id = ?::uuid AND deleted_at IS NULL");
    let row = db().query_first_sql(sql, vec![Value::from(voucher_id)]).await?;
    match row.as_ref() {
        Some(row) => Ok(Some(row.get_typed("voucher_no")?)),
        None => Ok(None),
    }
}

/// 应付详情：单头 + 关联凭证号；不存在 / 已删除 → `404 + 10002`。
pub async fn payable_detail(id: Uuid) -> std::result::Result<PayableDetailRes, ApiError> {
    let row = find_payable(id)
        .await
        .map_err(|error| utils::db_error(error, "查询应付单"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "应付单不存在或已删除".to_string()))?;
    let voucher_no = match row.voucher_id {
        Some(voucher_id) => voucher_no_of(voucher_id)
            .await
            .map_err(|error| utils::db_error(error, "查询应付单关联凭证"))?,
        None => None,
    };
    // 付款记录时间线（P7）：空列表 = 还没付过款，前端据此隐藏/置灰付款按钮。
    let payments = crate::business::erp::payable_payment::payments_res(db(), id).await?;
    Ok(PayableDetailRes {
        payable: PayableRes::from_row(&row, today_cst()),
        voucher_no,
        payments,
    })
}

/// 今天（CST）：与取号 / 账期同口径（`AP-yyyyMMdd` 按 CST 滚动）。
fn today_cst() -> NaiveDate {
    Utc::now()
        .with_timezone(&nik::prelude::chrono::FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定"))
        .date_naive()
}

// ==================== 接口层 ====================

/// 应付单分页。
#[nik::permission(
    path = "/business/erp/payable/page",
    method = "GET",
    permission = "business:erp:payable:list"
)]
#[nik::get("/business/erp/payable/page")]
pub async fn payable_page_handler(Query(query): Query<PayablePageQuery>) -> ResResult<PageData<Vec<PayableRes>>> {
    query.validate()?;
    let filter = query.to_filter()?;
    let (rows, total) = payable_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询应付单"))?;
    let today = today_cst();
    let list = rows.iter().map(|row| PayableRes::from_row(row, today)).collect();
    let (page, page_size) = normalize_page(query.page, query.page_size);
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 应付单详情（含关联凭证号）。
#[nik::permission(
    path = "/business/erp/payable/{id}",
    method = "GET",
    permission = "business:erp:payable:detail"
)]
#[nik::get("/business/erp/payable/{id}")]
pub async fn payable_detail_handler(Path(id): Path<String>) -> ResResult<PayableDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(payable_detail(id).await?))
}

/// 路径 / 查询里的 UUID：非法 → `400 + 66006`（绝不静默当作「没传」）。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不能为空"),
        ));
    }
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        )
    })
}

/// 日期参数：必须是 `YYYY-MM-DD`，非法 → `400 + 10001`（不静默忽略筛选条件）。
fn parse_date(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map_err(|_| bad_request(format!("{field} 必须是 YYYY-MM-DD，收到「{raw}」")))
}

// ==================== 单测 ====================
//
// 纯逻辑（标签 / 逾期 / 入参校验）不连库；真库用例需要 `ERP_DB_URL` 指向
// 「已应用 V0148 的隔离库」，缺省则全部 `SKIP`：
//
// ```bash
// export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_p2c2'
// cargo test --lib p2c2_
// ```
//
// 夹具前缀 `P2C2-`（单号 / 供应商编码）与固定 UUID 段 `c2c20001-…`；
// 每次 `setup()` 先按前缀清掉上一轮，重复跑不积累。
#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 供应商甲（夹具）。
    pub(crate) const SUPPLIER_A: &str = "c2c20001-0000-4000-8000-000000000001";
    /// 供应商乙（夹具）。
    pub(crate) const SUPPLIER_B: &str = "c2c20001-0000-4000-8000-000000000002";
    /// 关联凭证（夹具）。
    pub(crate) const VOUCHER_1: &str = "c2c20001-0000-4000-8000-0000000000a1";

    /// 真库用例必须串行（共用全局 `db()` 与同一批夹具）。
    pub(crate) static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    pub(crate) async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                // 每个 `#[tokio::test]` 是独立 runtime，`db()` 是进程级全局池：
                // 把池子寿命压到 0，避免借到上一个 runtime 的死连接（W1b 实测）。
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    pub(crate) async fn exec(sql: &str) {
        if let Err(err) = db().execute_with_params(sql, vec![]).await {
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    /// 清上一轮 → 建 2 个供应商 + 1 张凭证 + 5 张应付单。
    ///
    /// | # | 供应商 | 状态 | amount | paid | unpaid | created_at |
    /// |---|--------|------|--------|------|--------|------------|
    /// | 0001 | 甲 | OPEN | 226 | 0 | 226 | now-5d（挂凭证 `P2C2-V-0001`）|
    /// | 0002 | 甲 | PARTIAL_PAID | 100 | 40 | 60 | now-4d |
    /// | 0003 | 乙 | SETTLED | 50 | 50 | 0 | now-3d |
    /// | 0004 | 乙 | CANCELLED | 80 | 0 | 80 | now-2d |
    /// | 0005 | 甲 | OPEN | 30 | 0 | 30 | now-1d |
    pub(crate) async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        exec("DELETE FROM biz_payable WHERE payable_no LIKE 'P2C2-%'").await;
        exec("DELETE FROM biz_voucher WHERE voucher_word = 'P2C2'").await;
        exec("DELETE FROM biz_supplier WHERE supplier_code LIKE 'P2C2-%'").await;
        exec(&format!(
            "INSERT INTO {SUPPLIER_TABLE} (id, supplier_code, supplier_name, credit_days) \
             VALUES ('{SUPPLIER_A}', 'P2C2-SUP-A', 'P2C2 供应商甲', 30), \
                    ('{SUPPLIER_B}', 'P2C2-SUP-B', 'P2C2 供应商乙', 0) \
             ON CONFLICT DO NOTHING"
        ))
        .await;
        exec(&format!(
            "INSERT INTO {VOUCHER_TABLE} (id, voucher_word, voucher_no, period, voucher_date, \
             voucher_type, source_type, summary, total_debit, total_credit, entry_count, status) \
             VALUES ('{VOUCHER_1}', 'P2C2', 'P2C2-V-0001', '2026-03', '2026-03-15', 'AUTO', \
                     'STOCK_IN', 'P2C2 测试凭证', 226, 226, 2, 'POSTED')"
        ))
        .await;
        // (单号后缀, 供应商, 状态, amount, paid, unpaid, 天数偏移, voucher_id, 来源单号)
        let rows = [
            (
                "0001",
                SUPPLIER_A,
                "OPEN",
                "226",
                "0",
                "226",
                5,
                format!("'{VOUCHER_1}'::uuid"),
                "P2C2-IN-0001",
            ),
            (
                "0002",
                SUPPLIER_A,
                "PARTIAL_PAID",
                "100",
                "40",
                "60",
                4,
                "NULL".to_string(),
                "P2C2-IN-0002",
            ),
            (
                "0003",
                SUPPLIER_B,
                "SETTLED",
                "50",
                "50",
                "0",
                3,
                "NULL".to_string(),
                "P2C2-IN-0003",
            ),
            (
                "0004",
                SUPPLIER_B,
                "CANCELLED",
                "80",
                "0",
                "80",
                2,
                "NULL".to_string(),
                "P2C2-IN-0004",
            ),
            (
                "0005",
                SUPPLIER_A,
                "OPEN",
                "30",
                "0",
                "30",
                1,
                "NULL".to_string(),
                "P2C2-IN-0005",
            ),
        ];
        for (no, supplier, status, amount, paid, unpaid, days_ago, voucher, source_no) in rows {
            let id = format!("c2c20001-0000-4000-8000-0000c2c2{no}");
            let source_id = format!("c2c20001-0000-4000-8000-00000002{no}");
            let settled = if status == "SETTLED" { "now()" } else { "NULL" };
            // 到期日：乙的两张（SETTLED / CANCELLED）落在 2026-10-31，甲的三张 2026-11-30
            // —— 这样「到期日区间」用例能区分上下界，且不会与别的模块夹具混在一起。
            let due_date = if supplier == SUPPLIER_B {
                "2026-10-31"
            } else {
                "2026-11-30"
            };
            exec(&format!(
                "INSERT INTO {TABLE} (id, payable_no, supplier_id, source_type, source_id, \
                 source_no, currency_code, amount, paid_amount, unpaid_amount, credit_days, \
                 due_date, period_code, status, settled_at, voucher_id, created_at, updated_at) \
                 VALUES ('{id}', 'P2C2-AP-{no}', '{supplier}', 'STOCK_IN', '{source_id}', \
                         '{source_no}', 'CNY', {amount}, {paid}, {unpaid}, 30, '{due_date}', \
                         '2026-10', '{status}', {settled}, {voucher}, \
                         now() - interval '{days_ago} days', now())"
            ))
            .await;
        }
        Some(guard)
    }

    /// ① 列表：不带筛选返回全部夹具（5 张），供应商名称来自 LEFT JOIN。
    #[tokio::test]
    async fn p2c2_payable_page_returns_all_by_default() {
        let Some(_guard) = setup().await else {
            return;
        };
        let filter = PayableFilter {
            page: 1,
            page_size: 100,
            ..PayableFilter::default()
        };
        let (rows, total) = payable_page(&filter).await.expect("默认列表");
        assert!(total >= 5, "默认列表应至少含 5 张 P2C2 应付单，实际 {total}");
        let fixtures = rows
            .iter()
            .filter(|row| row.payable_no.starts_with("P2C2-AP-"))
            .collect::<Vec<_>>();
        assert_eq!(fixtures.len(), 5, "5 张夹具应全部返回");
        let first = fixtures
            .iter()
            .find(|row| row.payable_no == "P2C2-AP-0001")
            .expect("0001 应在列表里");
        assert_eq!(first.supplier_name, "P2C2 供应商甲", "LEFT JOIN 供应商名称");
        assert_eq!(first.amount, Decimal::new(2260000, 4));
        assert_eq!(first.unpaid_amount, Decimal::new(2260000, 4));
        assert_eq!(first.source_no, "P2C2-IN-0001");
        println!("[P2C2-①] 默认列表 5 张 + 供应商名称 JOIN ✓");
    }

    /// ② 列表：`status=OPEN` 只回未付单（供应商甲下 0001 / 0005）。
    #[tokio::test]
    async fn p2c2_payable_page_filters_by_status() {
        let Some(_guard) = setup().await else {
            return;
        };
        let filter = PayableFilter {
            status: Some("OPEN".to_string()),
            supplier_id: Some(Uuid::parse_str(SUPPLIER_A).expect("夹具 UUID")),
            page: 1,
            page_size: 100,
            ..PayableFilter::default()
        };
        let (rows, total) = payable_page(&filter).await.expect("状态筛选");
        assert_eq!(total, 2, "供应商甲下 OPEN 应有 2 张");
        assert!(rows.iter().all(|row| row.status == "OPEN"));
        let mut nos = rows.iter().map(|row| row.payable_no.as_str()).collect::<Vec<_>>();
        nos.sort_unstable();
        assert_eq!(nos, vec!["P2C2-AP-0001", "P2C2-AP-0005"]);
        println!("[P2C2-②] status=OPEN 筛选 ✓");
    }

    /// ③ 列表：`supplier_id` 精确筛选（供应商乙 0003 / 0004）。
    #[tokio::test]
    async fn p2c2_payable_page_filters_by_supplier() {
        let Some(_guard) = setup().await else {
            return;
        };
        let filter = PayableFilter {
            supplier_id: Some(Uuid::parse_str(SUPPLIER_B).expect("夹具 UUID")),
            page: 1,
            page_size: 100,
            ..PayableFilter::default()
        };
        let (rows, total) = payable_page(&filter).await.expect("供应商筛选");
        assert_eq!(total, 2, "供应商乙应有 2 张");
        assert!(rows.iter().all(|row| row.supplier_name == "P2C2 供应商乙"));
        assert!(rows.iter().all(|row| row.supplier_id.to_string() == SUPPLIER_B));
        println!("[P2C2-③] supplier_id 筛选 ✓");
    }

    /// ④ 详情：完整字段 + 关联凭证号；不存在 → 404。
    #[tokio::test]
    async fn p2c2_payable_detail_returns_voucher_no() {
        let Some(_guard) = setup().await else {
            return;
        };
        let detail = payable_detail(Uuid::parse_str("c2c20001-0000-4000-8000-0000c2c20001").expect("夹具 UUID"))
            .await
            .expect("应付详情");
        assert_eq!(detail.payable.payable_no, "P2C2-AP-0001");
        assert_eq!(detail.payable.supplier_id, SUPPLIER_A);
        assert_eq!(detail.payable.supplier_name, "P2C2 供应商甲");
        assert_eq!(detail.payable.source_type, "STOCK_IN");
        assert_eq!(detail.payable.source_no, "P2C2-IN-0001");
        assert_eq!(detail.payable.amount, Decimal::new(2260000, 4));
        assert_eq!(detail.payable.paid_amount, Decimal::ZERO);
        assert_eq!(detail.payable.unpaid_amount, Decimal::new(2260000, 4));
        assert_eq!(detail.payable.status, "OPEN");
        assert_eq!(detail.payable.status_label, "未付");
        assert_eq!(detail.payable.voucher_id.as_deref(), Some(VOUCHER_1));
        assert_eq!(detail.voucher_no.as_deref(), Some("P2C2-V-0001"), "详情带关联凭证号");
        // 没有凭证的那张：voucher_no 为 None，不报错。
        let plain = payable_detail(Uuid::parse_str("c2c20001-0000-4000-8000-0000c2c20002").expect("夹具 UUID"))
            .await
            .expect("应付详情（无凭证）");
        assert!(plain.voucher_no.is_none());
        assert_eq!(plain.payable.status_label, "部分已付");
        // 不存在 / 已删除 → 404。
        let err = payable_detail(Uuid::parse_str("c2c20001-0000-4000-8000-00000000ffff").expect("夹具 UUID"))
            .await
            .expect_err("不存在的应付单");
        assert_eq!(err.status, 404);
        println!("[P2C2-④] 详情完整字段 + 凭证号 + 404 ✓");
    }

    /// ⑤ 分页：`page` / `page_size` 生效，排序按创建时间倒序。
    #[tokio::test]
    async fn p2c2_payable_page_paginates() {
        let Some(_guard) = setup().await else {
            return;
        };
        let supplier_a = Some(Uuid::parse_str(SUPPLIER_A).expect("夹具 UUID"));
        let page1 = payable_page(&PayableFilter {
            supplier_id: supplier_a,
            page: 1,
            page_size: 2,
            ..PayableFilter::default()
        })
        .await
        .expect("第 1 页");
        assert_eq!(page1.1, 3, "供应商甲共 3 张");
        assert_eq!(page1.0.len(), 2, "page_size=2");
        let page2 = payable_page(&PayableFilter {
            supplier_id: supplier_a,
            page: 2,
            page_size: 2,
            ..PayableFilter::default()
        })
        .await
        .expect("第 2 页");
        assert_eq!(page2.1, 3);
        assert_eq!(page2.0.len(), 1, "第 2 页只剩 1 张");
        // 不重不漏 + 创建时间倒序（0005 最新 → 0002 → 0001）。
        let mut all = page1
            .0
            .iter()
            .chain(page2.0.iter())
            .map(|row| row.payable_no.clone())
            .collect::<Vec<_>>();
        all.sort();
        assert_eq!(all, vec!["P2C2-AP-0001", "P2C2-AP-0002", "P2C2-AP-0005"]);
        assert_eq!(page1.0[0].payable_no, "P2C2-AP-0005", "第 1 页首行是最新的");
        assert_eq!(page1.0[1].payable_no, "P2C2-AP-0002");
        println!("[P2C2-⑤] 分页 page/page_size + 排序 ✓");
    }

    /// ⑦ 列表：到期日区间（`due_date_from` / `due_date_to`，两端都含）。
    ///
    /// 夹具：供应商甲 3 张到期日 2026-11-30；供应商乙 2 张到期日 2026-10-31。
    /// 用供应商过滤把范围钉死，避免别的模块的夹具漂数。
    #[tokio::test]
    async fn p2c2_payable_page_filters_by_due_date_range() {
        let Some(_guard) = setup().await else {
            return;
        };
        let supplier_a = Some(Uuid::parse_str(SUPPLIER_A).expect("夹具 UUID"));
        let supplier_b = Some(Uuid::parse_str(SUPPLIER_B).expect("夹具 UUID"));
        let nov1 = NaiveDate::from_ymd_opt(2026, 11, 1).expect("日期");
        let nov30 = NaiveDate::from_ymd_opt(2026, 11, 30).expect("日期");
        let oct31 = NaiveDate::from_ymd_opt(2026, 10, 31).expect("日期");
        // 甲 + 2026-11-01..2026-11-30 → 3 张，且到期日都落在区间内。
        let filter = PayableFilter {
            supplier_id: supplier_a,
            due_date_from: Some(nov1),
            due_date_to: Some(nov30),
            page: 1,
            page_size: 100,
            ..PayableFilter::default()
        };
        let (rows, total) = payable_page(&filter).await.expect("到期日区间");
        assert_eq!(total, 3, "供应商甲 3 张都在 11 月到期");
        assert!(rows.iter().all(|row| row.due_date == nov30));
        // 上界含当天：乙 + 上界 2026-10-31 → 2 张。
        let filter = PayableFilter {
            supplier_id: supplier_b,
            due_date_to: Some(oct31),
            page: 1,
            page_size: 100,
            ..PayableFilter::default()
        };
        let (_, total) = payable_page(&filter).await.expect("到期日上界");
        assert_eq!(total, 2, "上界 2026-10-31 含当天");
        // 下界排除：乙 + 下界 2026-11-01 → 0 张。
        let filter = PayableFilter {
            supplier_id: supplier_b,
            due_date_from: Some(nov1),
            page: 1,
            page_size: 100,
            ..PayableFilter::default()
        };
        let (_, total) = payable_page(&filter).await.expect("到期日下界");
        assert_eq!(total, 0, "乙的到期日都早于 2026-11-01");
        println!("[P2C2-⑦] 到期日区间筛选（两端含）✓");
    }

    /// ⑥ 入参校验：非法状态 / 非法供应商 UUID / 非法到期日一律 400；空白串不算条件；状态标签。
    #[test]
    fn p2c2_query_validation_and_labels() {
        let bad_status = PayablePageQuery {
            status: Some("PAID".to_string()),
            ..PayablePageQuery::default()
        };
        assert_eq!(bad_status.validate().expect_err("非法状态").status, 400);
        let bad_supplier = PayablePageQuery {
            supplier_id: Some("not-a-uuid".to_string()),
            ..PayablePageQuery::default()
        };
        assert_eq!(bad_supplier.validate().expect_err("非法供应商").status, 400);
        assert!(bad_supplier.to_filter().is_err());
        let bad_date = PayablePageQuery {
            due_date_from: Some("2026-13-01".to_string()),
            ..PayablePageQuery::default()
        };
        assert_eq!(bad_date.validate().expect_err("非法到期日").status, 400);
        assert!(bad_date.to_filter().is_err());
        let bad_date_to = PayablePageQuery {
            due_date_to: Some("2026/11/30".to_string()),
            ..PayablePageQuery::default()
        };
        assert_eq!(bad_date_to.validate().expect_err("非法到期日").status, 400);
        // 空白串不算条件，合法 UUID 能解析。
        let blank = PayablePageQuery {
            status: Some("  ".to_string()),
            supplier_id: Some(String::new()),
            payable_no: Some(" ".to_string()),
            due_date_from: Some(String::new()),
            due_date_to: Some(" ".to_string()),
            ..PayablePageQuery::default()
        };
        assert!(blank.validate().is_ok());
        let filter = blank.to_filter().expect("空白串应放行");
        assert!(filter.status.is_none() && filter.supplier_id.is_none());
        assert!(filter.payable_no.is_none());
        assert!(filter.due_date_from.is_none() && filter.due_date_to.is_none());
        let ok = PayablePageQuery {
            supplier_id: Some(SUPPLIER_A.to_string()),
            status: Some("OPEN".to_string()),
            ..PayablePageQuery::default()
        };
        assert!(ok.validate().is_ok());
        assert_eq!(
            ok.to_filter().expect("合法").supplier_id.map(|id| id.to_string()),
            Some(SUPPLIER_A.to_string())
        );
        // 状态标签。
        assert_eq!(status_label("OPEN"), "未付");
        assert_eq!(status_label("SETTLED"), "已结清");
        assert_eq!(status_label("CANCELLED"), "已作废");
        assert_eq!(status_label("WEIRD"), "WEIRD");
        println!("[P2C2-⑥] 入参校验 + 状态标签 ✓");
    }

    // ---------- P-FLOW-3B 补漏：PayableRes 透出作废审批字段 ----------

    /// ④ `PayableRes::from_row` 必须透出 `cancel_instance_id` / `cancel_reason`
    /// （纯内存：前端重开详情抽屉后靠它显示「作废审批进度 + 原因」，不再依赖送审那一刻的响应）。
    #[test]
    fn pflow3b_res_carries_cancel_instance_and_reason() {
        let instance = Uuid::now_v7();
        let row = PayableRow {
            id: Uuid::now_v7(),
            payable_no: "AP-P3BGAP-000001".to_string(),
            supplier_id: Uuid::now_v7(),
            supplier_name: "P3B 补漏供应商".to_string(),
            source_type: "STOCK_IN".to_string(),
            source_id: Uuid::now_v7(),
            source_no: "IN-P3BGAP-000001".to_string(),
            invoice_no: None,
            invoice_date: None,
            currency_code: "CNY".to_string(),
            amount: Decimal::from(800),
            paid_amount: Decimal::ZERO,
            unpaid_amount: Decimal::from(800),
            credit_days: 30,
            due_date: NaiveDate::from_ymd_opt(2026, 12, 2).expect("固定日期"),
            period_code: "2026-11".to_string(),
            status: payable::STATUS_PENDING_CANCEL.to_string(),
            settled_at: None,
            voucher_id: None,
            remark: None,
            cancel_instance_id: Some(instance),
            cancel_reason: Some("供应商发票开错".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let today = NaiveDate::from_ymd_opt(2026, 11, 2).expect("固定日期");
        let res = PayableRes::from_row(&row, today);
        assert_eq!(res.cancel_instance_id.as_deref(), Some(instance.to_string().as_str()));
        assert_eq!(res.cancel_reason.as_deref(), Some("供应商发票开错"));

        // JSON 契约：两个键必须在（值为 null 也要在），前端按可选字段接
        let json = serde_json::to_value(&res).expect("序列化");
        assert_eq!(
            json.get("cancel_instance_id").and_then(|v| v.as_str()),
            Some(instance.to_string().as_str())
        );
        assert_eq!(
            json.get("cancel_reason").and_then(|v| v.as_str()),
            Some("供应商发票开错")
        );

        // 未在审批中的单：字段存在且为 null（不是缺席）
        let mut plain = row.clone();
        plain.status = payable::STATUS_OPEN.to_string();
        plain.cancel_instance_id = None;
        plain.cancel_reason = None;
        let json = serde_json::to_value(PayableRes::from_row(&plain, today)).expect("序列化");
        assert!(json.get("cancel_instance_id").is_some());
        assert!(json["cancel_instance_id"].is_null());
        assert!(json.get("cancel_reason").is_some());
        assert!(json["cancel_reason"].is_null());
    }
}
