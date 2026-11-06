//! 应收单查询（销售链 · SO-13）：**只读**接口 —— 应收分页 + 应收详情（含关联凭证号）。
//!
//! 镜像 `payable_query.rs`，表结构以 `backend/migrations/business/V0287__sales_receivable.up.sql`
//! 为准（24 列，与 `biz_payable` 逐列同型）：
//!
//! ```text
//! biz_receivable  id, ar_no, customer_id, source_type, source_id, source_no,
//!                 invoice_no, invoice_date, currency_code, amount, received_amount,
//!                 unsettled_amount, credit_days, due_date, period_code,
//!                 status(OPEN/PARTIAL/SETTLED/WRITTEN_OFF), settled_at, voucher_id, remark
//!                 + 审计五件套
//!                 CHECK unsettled_amount = amount - received_amount        ← 恒等
//!                 CHECK amount > 0 AND 0 <= received_amount <= amount
//!                 CHECK status <> 'SETTLED' OR unsettled_amount = 0
//!                 UNIQUE (source_type, source_id) WHERE deleted_at IS NULL AND source_type <> 'MANUAL'
//! ```
//!
//! # 接口
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/receivable/page` | `business:erp:receivable:list` | 分页（状态 / 客户 / 单号 / 来源 / 到期日区间） |
//! | GET | `/business/erp/receivable/{id}` | `business:erp:receivable:detail` | 详情 + **关联凭证号** |
//!
//! # 只读
//!
//! 本模块**没有**写路径：手工挂账与坏账核销在 [`crate::business::erp::receivable`]，
//! 出库过账自动挂账在 SO-14（`receivable_bridge.rs`）。
//!
//! # 四道入参校验（全部 400，绝不静默忽略）
//!
//! 1. `status` 只认 4 值白名单（`PARTIAL_PAID` / `CANCELLED` 这类 payable 的值会被明确拒掉）；
//! 2. `customer_id` 必须是合法 UUID（非法 = 400，不静默忽略）；
//! 3. `due_date_from` / `due_date_to` 必须是 `YYYY-MM-DD`；
//! 4. 空白串一律当「没传」（`?status=` 不该变成「筛状态 = 空串」）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_receivable 是应收台账（V0287 建表，无 dept_id / 归属人列），数据范围
//! （SELF / DEPT_*）表达不出来；可见性由 business:erp:receivable:* 权限码控制。
//! SCOPE-EXEMPT: biz_customer 是客户主数据（全局），没有归属列。
//! SCOPE-EXEMPT: biz_voucher 是会计台账（V0149 建表），详情取凭证号用，同样没有归属列。
//! ```

use axum::extract::{Path, Query};
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::model::normalize_page;
use crate::business::erp::receivable;
use crate::business::shared::scope;
use crate::shared::error_code;
use crate::utils;

// ==================== 表名 / 常量 ====================

/// 应收单主表。
pub const TABLE: &str = "biz_receivable";

/// 客户主数据表。
pub const CUSTOMER_TABLE: &str = "biz_customer";

/// 凭证表（详情取凭证号）。
pub const VOUCHER_TABLE: &str = "biz_voucher";

/// 状态白名单（与 `ck_biz_ar_status` 逐字一致；**不含** payable 的 PARTIAL_PAID / CANCELLED）。
pub const STATUSES: [&str; 4] = receivable::STATUSES;

/// 列表选择列表：应收列 + 客户名称（LEFT JOIN，客户被软删时回落空串）。
pub const LIST_COLUMNS: &str = "p.id, p.ar_no, p.customer_id, \
     COALESCE(c.name, '') AS customer_name, \
     p.source_type, p.source_id, p.source_no, p.invoice_no, p.invoice_date, \
     p.currency_code::text AS currency_code, p.amount, p.received_amount, p.unsettled_amount, \
     p.credit_days, p.due_date, p.period_code, p.status, p.settled_at, p.voucher_id, p.remark, \
     p.created_at, p.updated_at";

/// 列表 FROM 子句（别名 `p` / `c` 必须与 [`LIST_COLUMNS`] 一致）。
pub const LIST_FROM: &str = "FROM biz_receivable p \
     LEFT JOIN biz_customer c ON c.id = p.customer_id AND c.deleted_at IS NULL";

/// 分页排序：创建时间倒序，同一时刻按单号倒序（最新应收在前，排序稳定）。
pub const LIST_ORDER: &str = "ORDER BY p.created_at DESC, p.ar_no DESC";

/// 分页目标（`unscoped`：财务台账无归属列，理由见模块文档的 `SCOPE-EXEMPT`）。
pub const LIST_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

// ==================== 入参 ====================

/// 应收列表筛选（数据层形态）。
#[derive(Debug, Clone, Default)]
pub struct ReceivableFilter {
    /// 状态（精确，白名单）。
    pub status: Option<String>,
    /// 客户（精确 UUID）。
    pub customer_id: Option<Uuid>,
    /// 应收单号（**模糊** `%x%`，同时匹配来源单号）。
    pub ar_no: Option<String>,
    /// 来源单据类型（`MANUAL` / `SALES_OUT`，精确）。
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

/// 应收列表 Query（`page` / `page_size` 缺失时由 [`normalize_page`] 规整）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ReceivablePageQuery {
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 状态。
    pub status: Option<String>,
    /// 客户 id。
    pub customer_id: Option<String>,
    /// 应收单号（模糊）。
    pub ar_no: Option<String>,
    /// 来源单据类型。
    pub source_type: Option<String>,
    /// 到期日下界 `YYYY-MM-DD`（含）。
    pub due_date_from: Option<String>,
    /// 到期日上界 `YYYY-MM-DD`（含）。
    pub due_date_to: Option<String>,
}

impl ReceivablePageQuery {
    /// 入参校验：状态白名单 + 客户 UUID 合法 + 日期合法（非法一律 400）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(status) = blank_to_none(self.status.as_deref())
            && !STATUSES.contains(&status.as_str())
        {
            return Err(bad_request(format!(
                "status 只允许 {}，收到「{status}」\
                 （PARTIAL_PAID / CANCELLED 是应付的域值，应收对应 PARTIAL / WRITTEN_OFF）",
                STATUSES.join(" / ")
            )));
        }
        if let Some(customer_id) = blank_to_none(self.customer_id.as_deref()) {
            parse_uuid(&customer_id, "customer_id")?;
        }
        if let Some(raw) = blank_to_none(self.due_date_from.as_deref()) {
            parse_date(&raw, "due_date_from")?;
        }
        if let Some(raw) = blank_to_none(self.due_date_to.as_deref()) {
            parse_date(&raw, "due_date_to")?;
        }
        Ok(())
    }

    /// 转数据层筛选条件（`None` / 空串统一成「不筛」）。
    pub fn to_filter(&self) -> std::result::Result<ReceivableFilter, ApiError> {
        let customer_id = match blank_to_none(self.customer_id.as_deref()) {
            Some(raw) => Some(parse_uuid(&raw, "customer_id")?),
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
        Ok(ReceivableFilter {
            status: blank_to_none(self.status.as_deref()),
            customer_id,
            ar_no: blank_to_none(self.ar_no.as_deref()),
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
    raw.map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// 参数类 400（`10001`）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 读：行结构 ====================

/// `biz_receivable` 行 + 客户名称。
#[derive(Debug, Clone)]
pub struct ReceivableRow {
    /// 主键。
    pub id: Uuid,
    /// 应收单号。
    pub ar_no: String,
    /// 客户。
    pub customer_id: Uuid,
    /// 客户名称（LEFT JOIN，缺失为空串）。
    pub customer_name: String,
    /// 来源单据类型。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Uuid,
    /// 来源单号。
    pub source_no: String,
    /// 发票号。
    pub invoice_no: Option<String>,
    /// 发票日期。
    pub invoice_date: Option<NaiveDate>,
    /// 币种。
    pub currency_code: String,
    /// 应收金额。
    pub amount: Decimal,
    /// 已收金额。
    pub received_amount: Decimal,
    /// 未收金额（= amount - received_amount）。
    pub unsettled_amount: Decimal,
    /// 账期天数快照。
    pub credit_days: i32,
    /// 到期日。
    pub due_date: NaiveDate,
    /// 所属会计期间。
    pub period_code: String,
    /// 状态。
    pub status: String,
    /// 结清 / 核销时间。
    pub settled_at: Option<DateTime<Utc>>,
    /// 收入确认凭证。
    pub voucher_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl ReceivableRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            ar_no: row.get_typed("ar_no")?,
            customer_id: row.get_typed("customer_id")?,
            customer_name: row.get_typed("customer_name")?,
            source_type: row.get_typed("source_type")?,
            source_id: row.get_typed("source_id")?,
            source_no: row.get_typed("source_no")?,
            invoice_no: row.get_typed("invoice_no")?,
            invoice_date: row.get_typed("invoice_date")?,
            currency_code: row.get_typed("currency_code")?,
            amount: row.get_typed("amount")?,
            received_amount: row.get_typed("received_amount")?,
            unsettled_amount: row.get_typed("unsettled_amount")?,
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

// ==================== 出参 ====================

/// 应收（列表行 / 详情单头）。
#[derive(Debug, Clone, Serialize)]
pub struct ReceivableRes {
    /// 主键。
    pub id: String,
    /// 应收单号。
    pub ar_no: String,
    /// 客户。
    pub customer_id: String,
    /// 客户名称。
    pub customer_name: String,
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
    /// 应收金额。
    pub amount: Decimal,
    /// 已收金额。
    pub received_amount: Decimal,
    /// 未收金额。
    pub unsettled_amount: Decimal,
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
    /// 是否逾期（`due_date < 今天` 且仍有未收金额且未核销）。
    pub overdue: bool,
    /// 结清 / 核销时间。
    pub settled_at: Option<DateTime<Utc>>,
    /// 关联凭证。
    pub voucher_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl ReceivableRes {
    /// 由行构造（`today` 由调用方给，纯函数便于单测）。
    pub fn from_row(row: &ReceivableRow, today: NaiveDate) -> Self {
        Self {
            id: row.id.to_string(),
            ar_no: row.ar_no.clone(),
            customer_id: row.customer_id.to_string(),
            customer_name: row.customer_name.clone(),
            source_type: row.source_type.clone(),
            source_id: row.source_id.to_string(),
            source_no: row.source_no.clone(),
            invoice_no: row.invoice_no.clone(),
            invoice_date: row.invoice_date,
            currency_code: row.currency_code.clone(),
            amount: row.amount,
            received_amount: row.received_amount,
            unsettled_amount: row.unsettled_amount,
            credit_days: row.credit_days,
            due_date: row.due_date,
            period_code: row.period_code.clone(),
            status: row.status.clone(),
            status_label: receivable::status_label(&row.status).to_string(),
            overdue: is_overdue(row, today),
            settled_at: row.settled_at,
            voucher_id: row.voucher_id.map(|id| id.to_string()),
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// 应收详情：单头（展开）+ 关联凭证号。
#[derive(Debug, Clone, Serialize)]
pub struct ReceivableDetailRes {
    /// 单头。
    #[serde(flatten)]
    pub receivable: ReceivableRes,
    /// 关联凭证号（`voucher_id` 为空或凭证已删时是 `None`）。
    pub voucher_no: Option<String>,
}

// ==================== 派生 ====================

/// 是否逾期：到期日早于 `today`、仍有未收金额、且未核销。
///
/// 与账龄同口径：**到期当天（0 天）不算逾期**（`due_date < today` 而非 `<=`）。
pub fn is_overdue(row: &ReceivableRow, today: NaiveDate) -> bool {
    row.status != receivable::STATUS_WRITTEN_OFF
        && row.unsettled_amount > Decimal::ZERO
        && row.due_date < today
}

// ==================== 数据层 ====================

/// 应收分页（**全部状态**；默认不按状态筛，核销单也要看得见 —— 财务要对账）。
pub async fn receivable_page(q: &ReceivableFilter) -> DbResult<(Vec<ReceivableRow>, i64)> {
    let mut sql = format!("SELECT {LIST_COLUMNS} {LIST_FROM} WHERE p.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND p.status = ?");
        params.push(Value::from(status));
    }
    if let Some(customer_id) = q.customer_id {
        sql.push_str(" AND p.customer_id = ?::uuid");
        params.push(Value::from(&customer_id));
    }
    if let Some(source_type) = q.source_type.as_deref() {
        sql.push_str(" AND p.source_type = ?");
        params.push(Value::from(source_type));
    }
    if let Some(pattern) = utils::like_pattern(q.ar_no.as_deref()) {
        sql.push_str(" AND (p.ar_no LIKE ? OR p.source_no LIKE ?)");
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
    let result =
        scope::paginate_unscoped(LIST_TARGET, &sql, params, Some(LIST_ORDER), page, page_size)
            .await?;
    let rows = result
        .list
        .iter()
        .map(ReceivableRow::from_row)
        .collect::<DbResult<Vec<ReceivableRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 单张应收单（未删除）。
pub async fn find_receivable(id: Uuid) -> DbResult<Option<ReceivableRow>> {
    let sql =
        format!("SELECT {LIST_COLUMNS} {LIST_FROM} WHERE p.id = ?::uuid AND p.deleted_at IS NULL");
    let row = db().query_first_sql(sql, vec![Value::from(id)]).await?;
    row.as_ref().map(ReceivableRow::from_row).transpose()
}

/// 凭证号（凭证已软删 / 不存在 → `None`，不阻断详情）。
pub async fn voucher_no_of(voucher_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: biz_voucher 是会计台账，没有归属列（见模块文档）。
    let sql =
        format!("SELECT voucher_no FROM {VOUCHER_TABLE} WHERE id = ?::uuid AND deleted_at IS NULL");
    let row = db()
        .query_first_sql(sql, vec![Value::from(voucher_id)])
        .await?;
    match row.as_ref() {
        Some(row) => Ok(Some(row.get_typed("voucher_no")?)),
        None => Ok(None),
    }
}

/// 应收详情：单头 + 关联凭证号；不存在 / 已删除 → `404 + 10002`。
pub async fn receivable_detail(id: Uuid) -> std::result::Result<ReceivableDetailRes, ApiError> {
    let row = find_receivable(id)
        .await
        .map_err(|error| utils::db_error(error, "查询应收单"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                "应收单不存在或已删除".to_string(),
            )
        })?;
    let voucher_no = match row.voucher_id {
        Some(voucher_id) => voucher_no_of(voucher_id)
            .await
            .map_err(|error| utils::db_error(error, "查询应收单关联凭证"))?,
        None => None,
    };
    Ok(ReceivableDetailRes {
        receivable: ReceivableRes::from_row(&row, today_cst()),
        voucher_no,
    })
}

/// 今天（CST）：与取号 / 账期同口径（`AR-yyyyMMdd` 按 CST 滚动）。
fn today_cst() -> NaiveDate {
    Utc::now()
        .with_timezone(
            &nik::prelude::chrono::FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定"),
        )
        .date_naive()
}

// ==================== 接口层 ====================

/// 应收单分页。
#[nik::permission(
    path = "/business/erp/receivable/page",
    method = "GET",
    permission = "business:erp:receivable:list"
)]
#[nik::get("/business/erp/receivable/page")]
pub async fn receivable_page_handler(
    Query(query): Query<ReceivablePageQuery>,
) -> ResResult<PageData<Vec<ReceivableRes>>> {
    query.validate()?;
    let filter = query.to_filter()?;
    let (rows, total) = receivable_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询应收单"))?;
    let today = today_cst();
    let list = rows
        .iter()
        .map(|row| ReceivableRes::from_row(row, today))
        .collect();
    let (page, page_size) = normalize_page(query.page, query.page_size);
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    ))
}

/// 应收单详情（含关联凭证号）。
#[nik::permission(
    path = "/business/erp/receivable/{id}",
    method = "GET",
    permission = "business:erp:receivable:detail"
)]
#[nik::get("/business/erp/receivable/{id}")]
pub async fn receivable_detail_handler(Path(id): Path<String>) -> ResResult<ReceivableDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(receivable_detail(id).await?))
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
///
/// 刻意先做**形状**校验再 `parse_from_str`：chrono 的 `%m` / `%d` 接受不补零的写法
/// （`2026-1-1` 也能 parse），只靠 parse 会让「前端传错格式」静默变成一次查询。
fn parse_date(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    let bytes = raw.as_bytes();
    let shaped = bytes.len() == 10 && bytes[4] == b'-' && bytes[7] == b'-';
    let digits_ok = shaped
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
    if !digits_ok {
        return Err(bad_request(format!(
            "{field} 必须是 YYYY-MM-DD（补零，例：2026-11-05），收到「{raw}」"
        )));
    }
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map_err(|_| bad_request(format!("{field} 不是合法日期：{raw}")))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
    }

    fn sample_row(status: &str, amount: &str, received: &str, due: &str) -> ReceivableRow {
        ReceivableRow {
            id: Uuid::parse_str("a13a1001-0000-4000-8000-000000000001").unwrap(),
            ar_no: "AR-20261106-000001".to_string(),
            customer_id: Uuid::parse_str("a13a0001-0000-4000-8000-000000000001").unwrap(),
            customer_name: "驼铃贸易".to_string(),
            source_type: receivable::SOURCE_MANUAL.to_string(),
            source_id: Uuid::parse_str("a13a1001-0000-4000-8000-0000000000f1").unwrap(),
            source_no: "MANUAL".to_string(),
            invoice_no: None,
            invoice_date: None,
            currency_code: "CNY".to_string(),
            amount: dec(amount),
            received_amount: dec(received),
            unsettled_amount: dec(amount) - dec(received),
            credit_days: 30,
            due_date: date(due),
            period_code: "2026-10".to_string(),
            status: status.to_string(),
            settled_at: None,
            voucher_id: None,
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// ① 逾期口径：到期当天不算逾期；核销 / 收清的永不算逾期。
    #[test]
    fn overdue_boundary_is_strict_less_than() {
        let today = date("2026-11-05");
        assert!(
            !is_overdue(&sample_row("OPEN", "100", "0", "2026-11-05"), today),
            "到期当天不算逾期（账龄同口径）"
        );
        assert!(is_overdue(
            &sample_row("OPEN", "100", "0", "2026-11-04"),
            today
        ));
        assert!(
            !is_overdue(&sample_row("SETTLED", "100", "100", "2026-10-01"), today),
            "收清了即使早就到期也不算逾期"
        );
        assert!(
            !is_overdue(&sample_row("WRITTEN_OFF", "100", "0", "2026-10-01"), today),
            "已核销不再算逾期"
        );
        assert!(!is_overdue(
            &sample_row("OPEN", "100", "0", "2026-12-01"),
            today
        ));
    }

    /// ② Res 映射：状态标签取应收域（不是 payable 的「部分已付 / 已作废」）。
    #[test]
    fn res_maps_status_label_and_amounts() {
        let res = ReceivableRes::from_row(
            &sample_row("PARTIAL", "1000", "400", "2026-10-01"),
            date("2026-11-05"),
        );
        assert_eq!(res.status_label, "部分已收");
        assert_eq!(res.unsettled_amount, dec("600"));
        assert!(res.overdue);
        let res = ReceivableRes::from_row(
            &sample_row("WRITTEN_OFF", "1000", "0", "2026-10-01"),
            date("2026-11-05"),
        );
        assert_eq!(res.status_label, "坏账核销");
        assert!(!res.overdue);
    }

    /// ③ 状态白名单只认 4 值：payable 的 PARTIAL_PAID / CANCELLED 明确 400。
    #[test]
    fn status_filter_rejects_payable_domain_values() {
        let mut query = ReceivablePageQuery {
            status: Some("OPEN".to_string()),
            ..Default::default()
        };
        assert!(query.validate().is_ok());
        for good in receivable::STATUSES {
            query.status = Some(good.to_string());
            assert!(query.validate().is_ok(), "{good} 应通过");
        }
        for bad in ["PARTIAL_PAID", "CANCELLED", "PENDING_CANCEL", "open", ""] {
            query.status = Some(bad.to_string());
            let outcome = query.validate();
            if bad.is_empty() {
                assert!(outcome.is_ok(), "空白串 = 不筛，不是非法值");
            } else {
                assert!(outcome.is_err(), "`{bad}` 必须 400");
                assert_eq!(outcome.unwrap_err().status, 400);
            }
        }
    }

    /// ④ 入参校验：UUID / 日期非法一律 400，不静默退化成「不筛」。
    #[test]
    fn filter_validation_rejects_bad_uuid_and_date() {
        let query = ReceivablePageQuery {
            customer_id: Some("not-a-uuid".to_string()),
            ..Default::default()
        };
        assert_eq!(query.validate().unwrap_err().status, 400);

        let query = ReceivablePageQuery {
            due_date_from: Some("2026-1-1".to_string()),
            ..Default::default()
        };
        assert_eq!(query.validate().unwrap_err().status, 400);

        let query = ReceivablePageQuery {
            due_date_to: Some("2026-02-30".to_string()),
            ..Default::default()
        };
        assert_eq!(query.validate().unwrap_err().status, 400);

        let query = ReceivablePageQuery {
            customer_id: Some("   ".to_string()),
            due_date_from: Some("".to_string()),
            status: Some(" OPEN ".to_string()),
            ..Default::default()
        };
        assert!(query.validate().is_ok(), "全是空白 ⇒ 等价于不筛");
        let filter = query.to_filter().unwrap();
        assert!(filter.customer_id.is_none());
        assert!(filter.due_date_from.is_none());
        assert_eq!(filter.status.as_deref(), Some("OPEN"), "两侧空白要 trim");
    }

    // ---------- 真库用例（需 `ERP_DB_URL`，缺省 SKIP） ----------

    const CUSTOMER_A: &str = "a13a1002-0000-4000-8000-000000000001";
    const CUSTOMER_B: &str = "a13a1002-0000-4000-8000-000000000002";
    const VOUCHER_1: &str = "a13a1002-0000-4000-8000-0000000000a1";

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn ensure_db() -> bool {
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

    async fn exec(sql: &str, params: Vec<Value>) {
        if let Err(err) = db().execute_with_params(sql, params).await {
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    /// 夹具：2 个客户 + 1 张凭证 + 5 张应收（单号 `Q1301-*` 前缀，`ar_no` 手工指定）。
    ///
    /// | # | 客户 | 状态 | amount | received | unsettled | due_date |
    /// |---|------|------|--------|----------|-----------|----------|
    /// | 0001 | 甲 | OPEN | 226 | 0 | 226 | 2026-10-01（挂凭证 `Q1301-V-1`）|
    /// | 0002 | 甲 | PARTIAL | 100 | 40 | 60 | 2026-10-20 |
    /// | 0003 | 乙 | SETTLED | 50 | 50 | 0 | 2026-09-01 |
    /// | 0004 | 乙 | WRITTEN_OFF | 80 | 0 | 80 | 2026-08-01 |
    /// | 0005 | 甲 | OPEN | 30 | 0 | 30 | 2026-11-30 |
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        exec(
            "DELETE FROM biz_receivable WHERE ar_no LIKE 'Q1301-%'",
            vec![],
        )
        .await;
        exec(
            "DELETE FROM biz_voucher WHERE voucher_word = 'Q1301'",
            vec![],
        )
        .await;
        exec("DELETE FROM biz_customer WHERE code LIKE 'Q1301-%'", vec![]).await;
        // uuid 一律内联成字面量：`Value::Text` 绑到 `?::uuid` 在 nik-db 上序列化失败（实测）
        for (id, code, name) in [
            (CUSTOMER_A, "Q1301-A", "甲客户"),
            (CUSTOMER_B, "Q1301-B", "乙客户"),
        ] {
            exec(
                &format!(
                    "INSERT INTO biz_customer (id, code, name, status) \
                     VALUES ('{id}'::uuid, '{code}', '{name}', 'ACTIVE')"
                ),
                vec![],
            )
            .await;
        }
        // 真凭证行：`biz_voucher` 的期间列名是 `period`，且 ck_biz_voucher_entries 要求
        // entry_count >= 2、ck_biz_voucher_balance 要求借贷相等（夹具用 0/0 仍满足 >= 0）
        exec(
            &format!(
                "INSERT INTO biz_voucher \
                    (id, voucher_word, voucher_no, period, voucher_date, voucher_type, source_type, \
                     summary, total_debit, total_credit, entry_count, status) \
                 VALUES ('{VOUCHER_1}'::uuid, 'Q1301', 'Q1301-V-1', '2026-10', '2026-10-01'::date, \
                         'MANUAL', 'MANUAL', '应收夹具凭证', 0, 0, 2, 'POSTED')"
            ),
            vec![],
        )
        .await;
        let rows = [
            (
                "a13a1002-0000-4000-8000-000000000101",
                "Q1301-0001",
                CUSTOMER_A,
                "OPEN",
                "226",
                "0",
                "2026-10-01",
                "2026-10",
                Some(VOUCHER_1),
            ),
            (
                "a13a1002-0000-4000-8000-000000000102",
                "Q1301-0002",
                CUSTOMER_A,
                "PARTIAL",
                "100",
                "40",
                "2026-10-20",
                "2026-10",
                None,
            ),
            (
                "a13a1002-0000-4000-8000-000000000103",
                "Q1301-0003",
                CUSTOMER_B,
                "SETTLED",
                "50",
                "50",
                "2026-09-01",
                "2026-09",
                None,
            ),
            (
                "a13a1002-0000-4000-8000-000000000104",
                "Q1301-0004",
                CUSTOMER_B,
                "WRITTEN_OFF",
                "80",
                "0",
                "2026-08-01",
                "2026-08",
                None,
            ),
            (
                "a13a1002-0000-4000-8000-000000000105",
                "Q1301-0005",
                CUSTOMER_A,
                "OPEN",
                "30",
                "0",
                "2026-11-30",
                "2026-11",
                None,
            ),
        ];
        for (id, no, customer, status, amount, received, due, period, voucher) in rows {
            let unsettled: Decimal = dec(amount) - dec(received);
            let voucher_lit = match voucher {
                Some(value) => format!("'{value}'::uuid"),
                None => "NULL".to_string(),
            };
            exec(
                &format!(
                    "INSERT INTO biz_receivable \
                        (id, ar_no, customer_id, source_type, source_id, source_no, currency_code, \
                         amount, received_amount, unsettled_amount, credit_days, due_date, period_code, \
                         status, settled_at, voucher_id, created_at, updated_at, remark) \
                     VALUES ('{id}'::uuid, '{no}', '{customer}'::uuid, 'MANUAL', gen_random_uuid(), \
                             'MANUAL', 'CNY', {amount}::numeric, {received}::numeric, \
                             {unsettled}::numeric, 30, '{due}'::date, '{period}', '{status}', \
                             CASE WHEN '{status}' = 'OPEN' THEN NULL ELSE now() END, \
                             {voucher_lit}, now(), now(), 'Q1301 夹具')"
                ),
                vec![],
            )
            .await;
        }
        Some(guard)
    }

    fn fixture_filter() -> ReceivableFilter {
        ReceivableFilter {
            page: 1,
            page_size: 10,
            ..Default::default()
        }
    }

    /// ⑤ 真库：正常分页（总数 / 排序 / 客户名 / 金额），并验证状态筛选。
    #[tokio::test]
    async fn db_page_returns_fixtures_with_total() {
        let Some(_guard) = setup().await else { return };
        let mut filter = fixture_filter();
        filter.ar_no = Some("Q1301-".to_string());
        let (rows, total) = receivable_page(&filter).await.unwrap();
        assert_eq!(total, 5, "5 张夹具单全部可见");
        assert_eq!(rows.len(), 5);
        assert!(
            rows.iter().all(|row| !row.customer_name.is_empty()),
            "客户名要 JOIN 上"
        );
        // 排序：created_at DESC, ar_no DESC —— 同一时刻插入 ⇒ 单号倒序
        assert_eq!(rows[0].ar_no, "Q1301-0005");
        assert_eq!(rows[4].ar_no, "Q1301-0001");

        // 状态筛选：OPEN 只有 2 张
        filter.status = Some("OPEN".to_string());
        let (rows, total) = receivable_page(&filter).await.unwrap();
        assert_eq!(total, 2);
        assert!(rows.iter().all(|row| row.status == "OPEN"));

        // 客户筛选：乙客户 2 张
        let mut filter = fixture_filter();
        filter.ar_no = Some("Q1301-".to_string());
        filter.customer_id = Some(Uuid::parse_str(CUSTOMER_B).unwrap());
        let (_, total) = receivable_page(&filter).await.unwrap();
        assert_eq!(total, 2);

        // 到期日区间（含两端）
        let mut filter = fixture_filter();
        filter.ar_no = Some("Q1301-".to_string());
        filter.due_date_from = Some(date("2026-10-01"));
        filter.due_date_to = Some(date("2026-10-31"));
        let (rows, total) = receivable_page(&filter).await.unwrap();
        assert_eq!(total, 2);
        assert!(
            rows.iter()
                .all(|row| row.due_date.format("%Y-%m").to_string() == "2026-10"),
            "到期日区间含两端：只剩 10 月的两张"
        );
    }

    /// ⑥ 真库：分页边界（第 2 页、page_size=2）+ 空结果不是错误。
    #[tokio::test]
    async fn db_page_pagination_is_stable() {
        let Some(_guard) = setup().await else { return };
        let mut filter = fixture_filter();
        filter.ar_no = Some("Q1301-".to_string());
        filter.page_size = 2;
        let (page1, total) = receivable_page(&filter).await.unwrap();
        assert_eq!(total, 5);
        assert_eq!(page1.len(), 2);
        filter.page = 3;
        let (page3, _) = receivable_page(&filter).await.unwrap();
        assert_eq!(page3.len(), 1, "5 条 / 每页 2 ⇒ 第 3 页 1 条");
        filter.page = 9;
        let (page9, _) = receivable_page(&filter).await.unwrap();
        assert!(page9.is_empty(), "越界页返回空列表，不报错");

        let mut filter = fixture_filter();
        filter.ar_no = Some("NO-SUCH-".to_string());
        let (rows, total) = receivable_page(&filter).await.unwrap();
        assert_eq!(total, 0);
        assert!(rows.is_empty());
    }

    /// ⑦ 真库：详情带客户名 + 凭证号（凭证软删后回落 None）。
    #[tokio::test]
    async fn db_detail_includes_customer_and_voucher_no() {
        let Some(_guard) = setup().await else { return };
        let id = Uuid::parse_str("a13a1002-0000-4000-8000-000000000101").unwrap();
        let detail = receivable_detail(id).await.unwrap();
        assert_eq!(detail.receivable.ar_no, "Q1301-0001");
        assert_eq!(detail.receivable.customer_name, "甲客户");
        assert_eq!(detail.voucher_no.as_deref(), Some("Q1301-V-1"));
        assert_eq!(detail.receivable.unsettled_amount, dec("226"));
        assert!(detail.receivable.overdue, "2026-10-01 到期、今天更晚");

        exec(
            &format!("UPDATE biz_voucher SET deleted_at = now() WHERE id = '{VOUCHER_1}'::uuid"),
            vec![],
        )
        .await;
        assert!(
            find_receivable(id)
                .await
                .unwrap()
                .unwrap()
                .voucher_id
                .is_some(),
            "单据上的 voucher_id 不因凭证软删而丢"
        );
        let detail = receivable_detail(id).await.unwrap();
        assert_eq!(
            detail.voucher_no, None,
            "凭证软删后详情只回落凭证号，不 404"
        );

        let err =
            receivable_detail(Uuid::parse_str("a13a1002-0000-4000-8000-00000000dead").unwrap())
                .await
                .unwrap_err();
        assert_eq!(err.status, 404);
    }
}
