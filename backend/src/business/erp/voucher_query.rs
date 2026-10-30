//! 凭证查询（批次 D · P2-D1）：**只读**接口 —— 凭证分页 + 凭证详情（含全部分录）。
//!
//! 表结构以 `backend/migrations/business/V0149__erp_voucher.up.sql` 为准
//! （2026-10-31 开工 `\d biz_voucher` / `\d biz_voucher_entry` 实测，`cloudpivot_dev` 已到 V0149）：
//!
//! ```text
//! biz_voucher        id, voucher_word(默认「记」), voucher_no, period(yyyy-MM), voucher_date,
//!                    voucher_type(AUTO/MANUAL), source_type, source_id, source_no, summary,
//!                    total_debit, total_credit, entry_count, status(POSTED/DRAFT/AUDITED/CANCELLED),
//!                    is_red, reverse_of, reversed_by, posted_at, posted_by, remark + 审计五件套
//! biz_voucher_entry  id, voucher_id, line_no, summary, account_id, account_code, account_name,
//!                    debit_amount, credit_amount, aux_supplier_id, aux_material_id,
//!                    aux_warehouse_id, aux_dept_id, aux_qty, aux_extra, created_at
//! ```
//!
//! # 接口（卡面 §二 ①②）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/voucher/page` | `business:erp:voucher:list` | 分页（期间 / 状态 / 凭证类型 / 来源类型 / 凭证号） |
//! | GET | `/business/erp/voucher/{id}` | `business:erp:voucher:detail` | 详情 + **全部分录**（科目编码 / 名称已快照在分录行上） |
//!
//! # 只读，不改任何账
//!
//! 本模块**没有**写路径：凭证由 P2-C1 的 [`crate::business::erp::voucher`] 在收货过账的同一事务里生成，
//! 红冲也由那边负责。这里只查（卡面：「只读接口，不做前端」）。
//!
//! # 与卡面的三处口径差异（都写在这里，免得下游照卡面踩）
//!
//! 1. **列表分页走 `scope::paginate_unscoped`**（卡面硬红线 6）：凭证是会计台账，
//!    V0149 无 `dept_id` / 归属人列，数据范围表达不出来，与 `business/erp/model.rs` 同口径。
//! 2. **`status` 只做白名单校验**（`POSTED/DRAFT/AUDITED/CANCELLED`）：非法值返回 400 而不是
//!    「静默返回空列表」——静默空列表会让前端以为「本期没凭证」，是静默错数据。
//!    凭证**默认展示全部状态**（列表是账务人员的核对入口，草稿也要看得见）；
//!    「只算 POSTED」是**总账 / 余额表**的口径（见 [`crate::business::erp::ledger`]），不是列表的口径。
//! 3. **`voucher_no` 是模糊匹配**（`LIKE %x%`）：精确号也在其中，前端搜索框直接可用。
//!
//! # 详情的一致性闸：分录合计必须等于单头合计
//!
//! `biz_voucher` 有 `CHECK (total_debit = total_credit)`，但**没有**「分录合计 = 单头合计」的约束
//! （分录可以事后被别处改坏）。详情接口在返回前把分录合计与单头再比一次，不等 → `500 + 66009`
//! 并把差额写进消息（卡面硬红线 3 的同一口径：账不平不许装作没事）。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: biz_voucher / biz_voucher_entry 是会计台账（V0149 建表，无 dept_id / 归属人列），
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:voucher:* 权限码控制。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::model::normalize_page;
use crate::business::shared::scope;
use crate::shared::error_code;
use crate::utils;

// ==================== 表名 / 常量 ====================

/// 凭证主表。
pub const TABLE: &str = "biz_voucher";

/// 凭证分录表。
pub const ENTRY_TABLE: &str = "biz_voucher_entry";

/// 状态白名单（与 `ck_biz_voucher_status` 逐字一致）。
pub const STATUSES: [&str; 4] = ["POSTED", "DRAFT", "AUDITED", "CANCELLED"];

/// 凭证类型白名单（与 `ck_biz_voucher_type` 逐字一致）。
pub const VOUCHER_TYPES: [&str; 2] = ["AUTO", "MANUAL"];

/// `biz_voucher` 列清单。
pub const HEADER_COLUMNS: &str = "id, voucher_word, voucher_no, period, voucher_date, voucher_type, \
     source_type, source_id, source_no, summary, total_debit, total_credit, entry_count, status, \
     is_red, reverse_of, reversed_by, posted_at, posted_by, remark, created_at, updated_at";

/// `biz_voucher_entry` 列清单（`line_no` 升序即凭证内顺序）。
pub const ENTRY_COLUMNS: &str = "id, voucher_id, line_no, summary, account_id, account_code, \
     account_name, debit_amount, credit_amount, aux_supplier_id, aux_material_id, \
     aux_warehouse_id, aux_dept_id, aux_qty";

/// 分页排序：日期倒序，同日按凭证字号倒序（最新凭证在前）。
pub const LIST_ORDER: &str = "ORDER BY voucher_date DESC, voucher_word DESC, voucher_no DESC";

/// 分页目标（`unscoped`：台账无归属列，理由见模块文档的 `SCOPE-EXEMPT`）。
pub const LIST_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

// ==================== 入参 ====================

/// 凭证列表筛选（数据层形态）。
#[derive(Debug, Clone, Default)]
pub struct VoucherFilter {
    /// 会计期间（`yyyy-MM`，精确）。
    pub period: Option<String>,
    /// 凭证状态（精确，白名单）。
    pub status: Option<String>,
    /// 凭证类型（`AUTO` / `MANUAL`，精确）。
    pub voucher_type: Option<String>,
    /// 来源单据类型（`STOCK_IN` / `MANUAL` …，精确）。
    pub source_type: Option<String>,
    /// 凭证号（**模糊**，`%x%`）。
    pub voucher_no: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 凭证列表 Query（`page` / `page_size` 缺失时由 [`normalize_page`] 规整）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VoucherPageQuery {
    /// 会计期间（`yyyy-MM`）。
    pub period: Option<String>,
    /// 凭证状态。
    pub status: Option<String>,
    /// 凭证类型。
    pub voucher_type: Option<String>,
    /// 来源单据类型。
    pub source_type: Option<String>,
    /// 凭证号（模糊）。
    pub voucher_no: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl VoucherPageQuery {
    /// 转数据层筛选条件（`None` / 空串统一成「不筛」，空白串不当成条件）。
    pub fn to_filter(&self) -> VoucherFilter {
        VoucherFilter {
            period: blank_to_none(self.period.as_deref()),
            status: blank_to_none(self.status.as_deref()),
            voucher_type: blank_to_none(self.voucher_type.as_deref()),
            source_type: blank_to_none(self.source_type.as_deref()),
            voucher_no: blank_to_none(self.voucher_no.as_deref()),
            page: self.page,
            page_size: self.page_size,
        }
    }

    /// 入参校验：状态 / 类型 / 期间格式非法一律 400（绝不静默忽略）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(status) = blank_to_none(self.status.as_deref())
            && !STATUSES.contains(&status.as_str())
        {
            return Err(bad_request(format!(
                "status 只允许 {}，收到「{status}」",
                STATUSES.join(" / ")
            )));
        }
        if let Some(voucher_type) = blank_to_none(self.voucher_type.as_deref())
            && !VOUCHER_TYPES.contains(&voucher_type.as_str())
        {
            return Err(bad_request(format!(
                "voucher_type 只允许 {}，收到「{voucher_type}」",
                VOUCHER_TYPES.join(" / ")
            )));
        }
        if let Some(period) = blank_to_none(self.period.as_deref())
            && !is_month_key(&period)
        {
            return Err(bad_request(format!(
                "period 必须是 yyyy-MM，收到「{period}」"
            )));
        }
        Ok(())
    }
}

/// 空白串 → `None`（`?period=` 不该变成「筛期间 = 空串」）。
fn blank_to_none(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// `yyyy-MM` 形态校验（长度 7、第 5 位是 `-`、月份 01–12）。
pub fn is_month_key(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 7 || bytes[4] != b'-' {
        return false;
    }
    let year_ok = bytes[..4].iter().all(u8::is_ascii_digit);
    let month_ok = bytes[5..].iter().all(u8::is_ascii_digit);
    if !(year_ok && month_ok) {
        return false;
    }
    matches!(text[5..].parse::<u8>(), Ok(month) if (1..=12).contains(&month))
}

/// 参数类 400（`10001`）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 读：行结构 ====================

/// `biz_voucher` 行。
#[derive(Debug, Clone)]
pub struct VoucherRow {
    /// 主键。
    pub id: Uuid,
    /// 凭证字（默认「记」）。
    pub voucher_word: String,
    /// 凭证号。
    pub voucher_no: String,
    /// 会计期间 `yyyy-MM`。
    pub period: String,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 凭证类型（`AUTO` / `MANUAL`）。
    pub voucher_type: String,
    /// 来源单据类型。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Option<Uuid>,
    /// 来源单据号。
    pub source_no: Option<String>,
    /// 摘要。
    pub summary: String,
    /// 借方合计。
    pub total_debit: Decimal,
    /// 贷方合计。
    pub total_credit: Decimal,
    /// 分录行数。
    pub entry_count: i32,
    /// 状态。
    pub status: String,
    /// 是否红字凭证（`1` = 红冲）。
    pub is_red: i16,
    /// 被红冲的原凭证。
    pub reverse_of: Option<Uuid>,
    /// 红冲它的凭证。
    pub reversed_by: Option<Uuid>,
    /// 记账时间。
    pub posted_at: Option<DateTime<Utc>>,
    /// 记账人。
    pub posted_by: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl VoucherRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            voucher_word: row.get_typed("voucher_word")?,
            voucher_no: row.get_typed("voucher_no")?,
            period: row.get_typed("period")?,
            voucher_date: row.get_typed("voucher_date")?,
            voucher_type: row.get_typed("voucher_type")?,
            source_type: row.get_typed("source_type")?,
            source_id: row.get_typed("source_id")?,
            source_no: row.get_typed("source_no")?,
            summary: row.get_typed("summary")?,
            total_debit: row.get_typed("total_debit")?,
            total_credit: row.get_typed("total_credit")?,
            entry_count: row.get_typed("entry_count")?,
            status: row.get_typed("status")?,
            is_red: row.get_typed("is_red")?,
            reverse_of: row.get_typed("reverse_of")?,
            reversed_by: row.get_typed("reversed_by")?,
            posted_at: row.get_typed("posted_at")?,
            posted_by: row.get_typed("posted_by")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// `biz_voucher_entry` 行（科目编码 / 名称是 P2-C1 落库时的**快照**）。
#[derive(Debug, Clone)]
pub struct VoucherEntryRow {
    /// 主键。
    pub id: Uuid,
    /// 所属凭证。
    pub voucher_id: Uuid,
    /// 行号（凭证内从 1 起）。
    pub line_no: i32,
    /// 摘要。
    pub summary: String,
    /// 科目 id。
    pub account_id: Uuid,
    /// 科目编码（快照）。
    pub account_code: String,
    /// 科目名称（快照）。
    pub account_name: String,
    /// 借方金额（与贷方互斥，见 `ck_biz_voucher_entry_side`）。
    pub debit_amount: Decimal,
    /// 贷方金额。
    pub credit_amount: Decimal,
    /// 辅助核算：供应商。
    pub aux_supplier_id: Option<Uuid>,
    /// 辅助核算：物料。
    pub aux_material_id: Option<Uuid>,
    /// 辅助核算：仓库。
    pub aux_warehouse_id: Option<Uuid>,
    /// 辅助核算：部门。
    pub aux_dept_id: Option<Uuid>,
    /// 辅助核算：数量。
    pub aux_qty: Option<Decimal>,
}

impl VoucherEntryRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            voucher_id: row.get_typed("voucher_id")?,
            line_no: row.get_typed("line_no")?,
            summary: row.get_typed("summary")?,
            account_id: row.get_typed("account_id")?,
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            debit_amount: row.get_typed("debit_amount")?,
            credit_amount: row.get_typed("credit_amount")?,
            aux_supplier_id: row.get_typed("aux_supplier_id")?,
            aux_material_id: row.get_typed("aux_material_id")?,
            aux_warehouse_id: row.get_typed("aux_warehouse_id")?,
            aux_dept_id: row.get_typed("aux_dept_id")?,
            aux_qty: row.get_typed("aux_qty")?,
        })
    }
}

// ==================== 出参 ====================

/// 凭证（列表行）。
#[derive(Debug, Clone, Serialize)]
pub struct VoucherRes {
    /// 主键。
    pub id: String,
    /// 凭证字。
    pub voucher_word: String,
    /// 凭证号。
    pub voucher_no: String,
    /// 会计期间。
    pub period: String,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 凭证类型。
    pub voucher_type: String,
    /// 凭证类型中文标签。
    pub voucher_type_label: String,
    /// 来源单据类型。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Option<String>,
    /// 来源单据号。
    pub source_no: Option<String>,
    /// 摘要。
    pub summary: String,
    /// 借方合计。
    pub total_debit: Decimal,
    /// 贷方合计。
    pub total_credit: Decimal,
    /// 分录行数。
    pub entry_count: i32,
    /// 状态。
    pub status: String,
    /// 状态中文标签。
    pub status_label: String,
    /// 是否红字凭证。
    pub is_red: bool,
    /// 被红冲的原凭证。
    pub reverse_of: Option<String>,
    /// 红冲它的凭证。
    pub reversed_by: Option<String>,
    /// 记账时间。
    pub posted_at: Option<DateTime<Utc>>,
    /// 记账人。
    pub posted_by: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

impl VoucherRes {
    /// 由行构造。
    pub fn from_row(row: &VoucherRow) -> Self {
        Self {
            id: row.id.to_string(),
            voucher_word: row.voucher_word.clone(),
            voucher_no: row.voucher_no.clone(),
            period: row.period.clone(),
            voucher_date: row.voucher_date,
            voucher_type: row.voucher_type.clone(),
            voucher_type_label: voucher_type_label(&row.voucher_type).to_string(),
            source_type: row.source_type.clone(),
            source_id: row.source_id.map(|id| id.to_string()),
            source_no: row.source_no.clone(),
            summary: row.summary.clone(),
            total_debit: row.total_debit,
            total_credit: row.total_credit,
            entry_count: row.entry_count,
            status: row.status.clone(),
            status_label: status_label(&row.status).to_string(),
            is_red: row.is_red == 1,
            reverse_of: row.reverse_of.map(|id| id.to_string()),
            reversed_by: row.reversed_by.map(|id| id.to_string()),
            posted_at: row.posted_at,
            posted_by: row.posted_by.map(|id| id.to_string()),
            remark: row.remark.clone(),
            created_at: row.created_at,
        }
    }
}

/// 分录（详情内嵌）。
#[derive(Debug, Clone, Serialize)]
pub struct VoucherEntryRes {
    /// 主键。
    pub id: String,
    /// 行号。
    pub line_no: i32,
    /// 摘要。
    pub summary: String,
    /// 科目 id。
    pub account_id: String,
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 借方金额。
    pub debit_amount: Decimal,
    /// 贷方金额。
    pub credit_amount: Decimal,
    /// 辅助核算：供应商。
    pub aux_supplier_id: Option<String>,
    /// 辅助核算：物料。
    pub aux_material_id: Option<String>,
    /// 辅助核算：仓库。
    pub aux_warehouse_id: Option<String>,
    /// 辅助核算：部门。
    pub aux_dept_id: Option<String>,
    /// 辅助核算：数量。
    pub aux_qty: Option<Decimal>,
}

impl VoucherEntryRes {
    /// 由行构造。
    pub fn from_row(row: &VoucherEntryRow) -> Self {
        Self {
            id: row.id.to_string(),
            line_no: row.line_no,
            summary: row.summary.clone(),
            account_id: row.account_id.to_string(),
            account_code: row.account_code.clone(),
            account_name: row.account_name.clone(),
            debit_amount: row.debit_amount,
            credit_amount: row.credit_amount,
            aux_supplier_id: row.aux_supplier_id.map(|id| id.to_string()),
            aux_material_id: row.aux_material_id.map(|id| id.to_string()),
            aux_warehouse_id: row.aux_warehouse_id.map(|id| id.to_string()),
            aux_dept_id: row.aux_dept_id.map(|id| id.to_string()),
            aux_qty: row.aux_qty,
        }
    }
}

/// 凭证详情：单头 + 全部分录 + 一致性闸的结论。
#[derive(Debug, Clone, Serialize)]
pub struct VoucherDetailRes {
    /// 单头。
    #[serde(flatten)]
    pub voucher: VoucherRes,
    /// 全部分录（`line_no` 升序）。
    pub entries: Vec<VoucherEntryRes>,
    /// 分录借方合计（与 [`VoucherRes::total_debit`] 必须相等）。
    pub entry_total_debit: Decimal,
    /// 分录贷方合计（与 [`VoucherRes::total_credit`] 必须相等）。
    pub entry_total_credit: Decimal,
    /// 单头与分录是否一致（恒为 `true`：不一致已在 [`voucher_detail`] 里被 500 拦掉）。
    pub balanced: bool,
}

// ==================== 标签 ====================

/// 凭证状态中文标签（未知状态原样返回，不 panic）。
pub fn status_label(status: &str) -> &str {
    match status {
        "POSTED" => "已记账",
        "DRAFT" => "草稿",
        "AUDITED" => "已审核",
        "CANCELLED" => "已作废",
        other => other,
    }
}

/// 凭证类型中文标签。
pub fn voucher_type_label(voucher_type: &str) -> &str {
    match voucher_type {
        "AUTO" => "自动",
        "MANUAL" => "手工",
        other => other,
    }
}

// ==================== 数据层 ====================

/// 凭证分页（**全部状态**；只算 POSTED 是总账口径，不是列表口径）。
pub async fn voucher_page(q: &VoucherFilter) -> DbResult<(Vec<VoucherRow>, i64)> {
    let mut sql = format!("SELECT {HEADER_COLUMNS} FROM {TABLE} WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(period) = q.period.as_deref() {
        sql.push_str(" AND period = ?");
        params.push(Value::from(period));
    }
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    if let Some(voucher_type) = q.voucher_type.as_deref() {
        sql.push_str(" AND voucher_type = ?");
        params.push(Value::from(voucher_type));
    }
    if let Some(source_type) = q.source_type.as_deref() {
        sql.push_str(" AND source_type = ?");
        params.push(Value::from(source_type));
    }
    if let Some(pattern) = utils::like_pattern(q.voucher_no.as_deref()) {
        sql.push_str(" AND (voucher_no LIKE ? OR source_no LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    let (page, page_size) = normalize_page(q.page, q.page_size);
    let result =
        scope::paginate_unscoped(LIST_TARGET, &sql, params, Some(LIST_ORDER), page, page_size)
            .await?;
    let rows = result
        .list
        .iter()
        .map(VoucherRow::from_row)
        .collect::<DbResult<Vec<VoucherRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 单张凭证（未删除）。
pub async fn find_voucher(id: Uuid) -> DbResult<Option<VoucherRow>> {
    let sql =
        format!("SELECT {HEADER_COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL");
    let row = db().query_first_sql(sql, vec![Value::from(id)]).await?;
    row.as_ref().map(VoucherRow::from_row).transpose()
}

/// 某张凭证的全部分录（`line_no` 升序）。
pub async fn entries_of(voucher_id: Uuid) -> DbResult<Vec<VoucherEntryRow>> {
    let sql = format!(
        "SELECT {ENTRY_COLUMNS} FROM {ENTRY_TABLE} WHERE voucher_id = ?::uuid ORDER BY line_no"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(voucher_id)])
        .await?;
    rows.iter().map(VoucherEntryRow::from_row).collect()
}

/// 凭证详情：单头 + 全部分录；分录合计 ≠ 单头合计 → `500 + 66009`。
pub async fn voucher_detail(id: Uuid) -> std::result::Result<VoucherDetailRes, ApiError> {
    let row = find_voucher(id)
        .await
        .map_err(|error| utils::db_error(error, "查询凭证"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                "凭证不存在或已删除".to_string(),
            )
        })?;
    let entries = entries_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询凭证分录"))?;
    let entry_total_debit = entries
        .iter()
        .fold(Decimal::ZERO, |acc, line| acc + line.debit_amount);
    let entry_total_credit = entries
        .iter()
        .fold(Decimal::ZERO, |acc, line| acc + line.credit_amount);
    if entry_total_debit != row.total_debit || entry_total_credit != row.total_credit {
        return Err(unbalanced_error(
            "凭证",
            &row.voucher_no,
            entry_total_debit,
            entry_total_credit,
            row.total_debit,
            row.total_credit,
        ));
    }
    Ok(VoucherDetailRes {
        voucher: VoucherRes::from_row(&row),
        entries: entries.iter().map(VoucherEntryRes::from_row).collect(),
        entry_total_debit,
        entry_total_credit,
        balanced: true,
    })
}

/// 账不平统一 `500 + 66009`（消息里带齐四个数，运维照抄就能定位）。
pub fn unbalanced_error(
    what: &str,
    key: &str,
    debit: Decimal,
    credit: Decimal,
    expected_debit: Decimal,
    expected_credit: Decimal,
) -> ApiError {
    ApiError::new(
        500,
        error_code::ERP_LEDGER_UNBALANCED,
        format!(
            "{what}「{key}」借贷不平衡：Σ借方={debit} Σ贷方={credit}（单头 {expected_debit} / {expected_credit}，\
             差额 {}）",
            debit - credit
        ),
    )
}

// ==================== 接口层 ====================

/// 凭证分页。
#[nik::permission(
    path = "/business/erp/voucher/page",
    method = "GET",
    permission = "business:erp:voucher:list"
)]
#[nik::get("/business/erp/voucher/page")]
pub async fn voucher_page_handler(
    Query(query): Query<VoucherPageQuery>,
) -> ResResult<PageData<Vec<VoucherRes>>> {
    query.validate()?;
    let (rows, total) = voucher_page(&query.to_filter())
        .await
        .map_err(|error| utils::db_error(error, "分页查询凭证"))?;
    let list = rows.iter().map(VoucherRes::from_row).collect();
    let (page, page_size) = normalize_page(query.page, query.page_size);
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    ))
}

/// 凭证详情（含全部分录）。
#[nik::permission(
    path = "/business/erp/voucher/{id}",
    method = "GET",
    permission = "business:erp:voucher:detail"
)]
#[nik::get("/business/erp/voucher/{id}")]
pub async fn voucher_detail_handler(Path(id): Path<String>) -> ResResult<VoucherDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(voucher_detail(id).await?))
}

/// 路径里的 UUID：非法 → `400 + 66006`（绝不静默当作「没传」）。
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

// ==================== 单测 ====================
//
// 纯逻辑（月份键 / 标签）不连库；真库用例需要 `ERP_DB_URL` 指向「已应用 V0149 的隔离库」，
// 缺省则全部 `SKIP`：
//
// ```bash
// export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_p2d1'
// cargo test --lib p2d1_
// ```
//
// 夹具前缀 `P2D1-` / 科目 `P2D1A`…；每次 `setup()` 先按 `voucher_word='P2D1'` 清掉上一轮的凭证
// （分录靠 `ON DELETE CASCADE`），所以重复跑不积累。
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::str::FromStr;

    /// 科目 A（资产 / 借方向）。
    pub(crate) const ACCOUNT_A: &str = "d2d10001-0000-4000-8000-000000000001";
    /// 科目 B（负债 / 贷方向）。
    pub(crate) const ACCOUNT_B: &str = "d2d10001-0000-4000-8000-000000000002";

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

    /// 清上一轮 → 建科目 → 建 6 张凭证（含 DRAFT / 红冲 / 跨期间）。
    ///
    /// | # | 日期 | 期间 | 状态 | 分录 |
    /// |---|------|------|------|------|
    /// | 1 | 2026-09-10 | 2026-09 | POSTED | 借 A 100 / 贷 B 100 |
    /// | 2 | 2026-10-05 | 2026-10 | POSTED | 借 A 50 / 贷 B 50 |
    /// | 3 | 2026-10-08 | 2026-10 | POSTED | 借 A 30 / 贷 B 30 |
    /// | 4 | 2026-10-12 | 2026-10 | **DRAFT** | 借 A 999 / 贷 B 999 |
    /// | 5 | 2026-10-15 | 2026-10 | POSTED | **红字**（2 号的借贷互换）：贷 A 50 / 借 B 50 |
    /// | 6 | 2026-11-03 | 2026-11 | POSTED | 借 A 7 / 贷 B 7 |
    pub(crate) async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        exec("DELETE FROM biz_voucher WHERE voucher_word = 'P2D1'").await;
        let fixtures = [
            format!(
                "INSERT INTO biz_account (id, account_code, account_name, parent_id, level, \
                 account_category, balance_direction, is_leaf, status) \
                 VALUES ('{ACCOUNT_A}', 'P2D1A', 'P2D1 测试现金', NULL, 1, 'ASSET', 'DEBIT', 1, 1), \
                        ('{ACCOUNT_B}', 'P2D1B', 'P2D1 测试应付', NULL, 1, 'LIABILITY', 'CREDIT', 1, 1) \
                 ON CONFLICT DO NOTHING"
            ),
            // 期间表：2026-10 有正式期间行，2026-11 故意不建（走「按月份推导」兜底）。
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
             start_date, end_date, status) \
             VALUES (2026, 10, '2026-10', '2026-10-01', '2026-10-31', 'OPEN') \
             ON CONFLICT DO NOTHING"
                .to_string(),
        ];
        for sql in &fixtures {
            exec(sql).await;
        }
        for (no, date, period, status, debit_a, credit_a, debit_b, credit_b, red, reverse) in [
            (
                "0001",
                "2026-09-10",
                "2026-09",
                "POSTED",
                100,
                0,
                0,
                100,
                0,
                "",
            ),
            (
                "0002",
                "2026-10-05",
                "2026-10",
                "POSTED",
                50,
                0,
                0,
                50,
                0,
                "",
            ),
            (
                "0003",
                "2026-10-08",
                "2026-10",
                "POSTED",
                30,
                0,
                0,
                30,
                0,
                "",
            ),
            (
                "0004",
                "2026-10-12",
                "2026-10",
                "DRAFT",
                999,
                0,
                0,
                999,
                0,
                "",
            ),
            (
                "0005",
                "2026-10-15",
                "2026-10",
                "POSTED",
                0,
                50,
                50,
                0,
                1,
                "0002",
            ),
            ("0006", "2026-11-03", "2026-11", "POSTED", 7, 0, 0, 7, 0, ""),
        ] {
            let id = format!("d2d10001-0000-4000-8000-00000000{no}");
            let reverse_of = if reverse.is_empty() {
                "NULL".to_string()
            } else {
                format!("'d2d10001-0000-4000-8000-00000000{reverse}'::uuid")
            };
            let total = if debit_a > 0 { debit_a } else { credit_a };
            exec(&format!(
                "INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, \
                 voucher_type, source_type, source_no, summary, total_debit, total_credit, \
                 entry_count, status, is_red, reverse_of) \
                 VALUES ('{id}', 'P2D1', 'P2D1-{no}', '{period}', '{date}', 'AUTO', 'STOCK_IN', \
                         'P2D1-SRC-{no}', 'P2D1 测试凭证 {no}', {total}, {total}, 2, '{status}', \
                         {red}, {reverse_of})"
            ))
            .await;
            exec(&format!(
                "INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, \
                 account_code, account_name, debit_amount, credit_amount) \
                 VALUES ('{id}', 1, 'P2D1 行 1', '{ACCOUNT_A}', 'P2D1A', 'P2D1 测试现金', \
                         {debit_a}, {credit_a}), \
                        ('{id}', 2, 'P2D1 行 2', '{ACCOUNT_B}', 'P2D1B', 'P2D1 测试应付', \
                         {debit_b}, {credit_b})"
            ))
            .await;
        }
        Some(guard)
    }

    pub(crate) fn uid(raw: &str) -> Uuid {
        Uuid::from_str(raw).expect("夹具 UUID 写错了")
    }

    /// ① 列表：按 `period` / `status` / `source_type` / `voucher_no` 筛选。
    #[tokio::test]
    async fn p2d1_voucher_page_filters() {
        let Some(_guard) = setup().await else {
            return;
        };
        // 期间：2026-10 有 4 张（含 DRAFT 与红字）。
        let filter = VoucherFilter {
            period: Some("2026-10".to_string()),
            page: 1,
            page_size: 50,
            ..VoucherFilter::default()
        };
        let (rows, total) = voucher_page(&filter).await.expect("期间筛选");
        assert_eq!(total, 4, "2026-10 应有 4 张 P2D1 凭证");
        assert!(rows.iter().all(|row| row.period == "2026-10"));
        // 只看 POSTED：DRAFT 被筛掉 → 3 张。
        let filter = VoucherFilter {
            period: Some("2026-10".to_string()),
            status: Some("POSTED".to_string()),
            page: 1,
            page_size: 50,
            ..VoucherFilter::default()
        };
        let (rows, total) = voucher_page(&filter).await.expect("状态筛选");
        assert_eq!(total, 3, "2026-10 的 POSTED 应有 3 张");
        assert!(rows.iter().all(|row| row.status == "POSTED"));
        // 来源类型：本夹具全是 STOCK_IN；换成 MANUAL 应 0 行。
        let filter = VoucherFilter {
            source_type: Some("MANUAL".to_string()),
            page: 1,
            page_size: 50,
            ..VoucherFilter::default()
        };
        let (_, total) = voucher_page(&filter).await.expect("来源筛选");
        assert_eq!(total, 0, "没有 MANUAL 来源的 P2D1 凭证");
        // 凭证号模糊：P2D1-0002（含红冲它的 0005 之外只应命中 1 张）。
        let filter = VoucherFilter {
            voucher_no: Some("P2D1-0002".to_string()),
            page: 1,
            page_size: 50,
            ..VoucherFilter::default()
        };
        let (rows, total) = voucher_page(&filter).await.expect("凭证号筛选");
        assert_eq!(total, 1, "凭证号精确串应只命中 1 张");
        assert_eq!(rows[0].voucher_no, "P2D1-0002");
        println!("[P2D1-①] 期间/状态/来源/凭证号筛选 ✓");
    }

    /// ② 详情：含全部分录，借贷合计等于单头 `total_debit` / `total_credit`。
    #[tokio::test]
    async fn p2d1_voucher_detail_matches_totals() {
        let Some(_guard) = setup().await else {
            return;
        };
        let detail = voucher_detail(uid("d2d10001-0000-4000-8000-000000000002"))
            .await
            .expect("凭证详情");
        assert_eq!(detail.entries.len(), 2, "夹具凭证有 2 行分录");
        assert_eq!(detail.entries[0].line_no, 1, "分录按 line_no 升序");
        assert_eq!(detail.entry_total_debit, detail.voucher.total_debit);
        assert_eq!(detail.entry_total_credit, detail.voucher.total_credit);
        assert_eq!(detail.entry_total_debit, Decimal::from(50));
        assert!(detail.balanced);
        assert_eq!(detail.entries[0].account_code, "P2D1A");
        assert_eq!(detail.entries[1].account_name, "P2D1 测试应付");
        // 不存在 / 已删除 → 404。
        let err = voucher_detail(uid("d2d10001-0000-4000-8000-00000000ffff"))
            .await
            .expect_err("不存在的凭证");
        assert_eq!(err.status, 404);
        println!("[P2D1-②] 详情分录 + 借贷合计一致 + 404 ✓");
    }

    /// ③ 红字凭证在详情里可辨认（`is_red` / `reverse_of`）。
    #[tokio::test]
    async fn p2d1_red_voucher_detail_flags() {
        let Some(_guard) = setup().await else {
            return;
        };
        let detail = voucher_detail(uid("d2d10001-0000-4000-8000-000000000005"))
            .await
            .expect("红字凭证详情");
        assert!(detail.voucher.is_red, "0005 是红字凭证");
        assert_eq!(
            detail.voucher.reverse_of.as_deref(),
            Some("d2d10001-0000-4000-8000-000000000002")
        );
        // 借贷互换：A 科目这张凭证是**贷** 50。
        assert_eq!(detail.entries[0].debit_amount, Decimal::ZERO);
        assert_eq!(detail.entries[0].credit_amount, Decimal::from(50));
        println!("[P2D1-③] 红字凭证 is_red / reverse_of / 借贷互换 ✓");
    }

    /// ④ 入参校验：非法状态 / 类型 / 期间一律 400（不静默忽略）。
    #[test]
    fn p2d1_query_validation() {
        let bad_status = VoucherPageQuery {
            status: Some("POSTEDD".to_string()),
            ..VoucherPageQuery::default()
        };
        assert_eq!(bad_status.validate().expect_err("非法状态").status, 400);
        let bad_type = VoucherPageQuery {
            voucher_type: Some("SYSTEM".to_string()),
            ..VoucherPageQuery::default()
        };
        assert_eq!(bad_type.validate().expect_err("非法类型").status, 400);
        let bad_period = VoucherPageQuery {
            period: Some("2026-13".to_string()),
            ..VoucherPageQuery::default()
        };
        assert_eq!(bad_period.validate().expect_err("非法期间").status, 400);
        // 空白串不算条件。
        let blank = VoucherPageQuery {
            status: Some("  ".to_string()),
            period: Some(String::new()),
            ..VoucherPageQuery::default()
        };
        assert!(blank.validate().is_ok());
        let filter = blank.to_filter();
        assert!(filter.status.is_none() && filter.period.is_none());
        assert!(is_month_key("2026-10") && !is_month_key("2026-1"));
        println!("[P2D1-④] 入参校验（状态/类型/期间）✓");
    }
}
