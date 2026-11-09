//! 收款单（销售链 · SO-17）：**收款登记 → 核销到应收 → 确认 → 过账（生成凭证）**。
//!
//! 表结构见 `backend/migrations/business/V0292__sales_receipt.up.sql`（SO-16）：
//!
//! ```text
//! biz_receipt             id, receipt_no(RC-yyyyMMdd-000001), customer_id, receipt_date,
//!                         amount, allocated_amount, method, bank_account,
//!                         status(DRAFT/CONFIRMED/POSTED/CANCELLED), voucher_id, remark + 审计五件套
//!                         CHECK amount > 0 AND 0 <= allocated_amount <= amount
//!                         CHECK status <> 'CANCELLED' OR allocated_amount = 0
//!                         UNIQUE (receipt_no) WHERE deleted_at IS NULL
//! biz_receipt_allocation  id, receipt_id(CASCADE), receivable_id(RESTRICT), amount(>0), remark,
//!                         created_at, create_by
//!                         UNIQUE (receipt_id, receivable_id)   ← 重复核销 = 改金额（UPSERT）
//! ```
//!
//! # 接口（ARCH-7 §5.5 的 5 个）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|
//! | POST | `/business/erp/receipt` | `business:erp:receipt:create` | 登记收款（DRAFT） |
//! | GET | `/business/erp/receipt/{id}` | `business:erp:receipt:detail` | 详情 + 核销明细 |
//! | GET | `/business/erp/receipt/page` | `business:erp:receipt:list` | 分页 |
//! | POST | `/business/erp/receipt/{id}/allocate` | `business:erp:receipt:allocate` | 核销到应收 |
//! | PATCH | `/business/erp/receipt/{id}/status` | `business:erp:receipt:status` | 状态迁移 |
//!
//! # 核销算法（照搬 `payable_payment.rs` 的付款记账骨架，方向相反）
//!
//! 付款是「一张应付 ← 多条付款」，收款是「**一条收款 → 多张应收**」（卡面规则）。
//! 所以 `payable_payment::settle_payment_in_tx` 的三步被拆成「逐张应收循环 + 一次汇总」：
//!
//! ```text
//! ① 锁收款单（SELECT … FOR UPDATE）→ 状态必须 DRAFT / CONFIRMED
//! ② 逐条明细：锁应收（FOR UPDATE，**按 receivable_id 排序**防死锁）
//!    · 客户必须与收款单一致（核到别家的应收 = 把账坐错，400）
//!    · 应收必须是 OPEN / PARTIAL（SETTLED / WRITTEN_OFF / CANCELLED → 409）
//!    · 本次核销 ≤ 该应收的「可核销余额」= unsettled_amount + 本收款已核过它的金额
//!      （本收款此前核过的那部分已经计入 received_amount，不先加上就会误拒）
//! ③ 汇总校验：核销总额 ≤ 收款金额（400，文案点名两个金额）
//! ④ 落明细：INSERT … ON CONFLICT (receipt_id, receivable_id) DO UPDATE SET amount
//! ⑤ 回写应收：received_amount += delta、unsettled_amount = amount - received，
//!    状态 OPEN→PARTIAL→SETTLED 走 `state_machine::transition_if_changed`（原地流转不写日志）
//! ⑥ 回写收款头：allocated_amount = SUM(明细)（**从库里重新 SUM**，不信内存累加）
//! ```
//!
//! # 过账（POSTED）为什么要求「核销满」
//!
//! 凭证是卡面逐字的两行：**借 1002 银行存款 / 贷 1122 应收账款**，各 = 收款金额。
//! 若还有 `amount - allocated_amount > 0` 的未核销余额，把它贷记 1122 就是凭空冲减
//! 一笔不存在的应收账款（真实的未核销收款应贷 2203 预收账款，属 ARCH-7 §8.1 的「预留」项）。
//! 所以本卡的口径：**未核销余额 > 0 时拒绝过账（409，点名余额）**，让财务先核销（或作废重登）。
//! 这条口径同时把 ARCH-7 §9.2 的不变量 S5（`sum(allocation) = allocated_amount`）钉死。
//!
//! # 事务边界
//!
//! 每个写入口（create / allocate / status）**自己开事务**：收款头、核销明细、应收回写、
//! 凭证四者同生共死。核销与过账都不许「一半成功」。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_receipt / biz_receipt_allocation 是销售台账（V0292 建表，无 dept_id /
//! 归属人列），数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:receipt:* 权限码控制。
//! SCOPE-EXEMPT: biz_receivable 是应收台账、biz_customer 是客户主数据（全局），同样没有归属列。
//! SCOPE-EXEMPT: biz_voucher / biz_seq 是会计台账与全局字典，没有归属列。
//! ```

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{DateTime, Datelike, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::attachment::model::DocKind as AttachDocKind;
use crate::business::attachment::rule::{self as attach_rule, RequiredAt as AttachAt};
use crate::business::erp::credit;
use crate::business::erp::currency;
use crate::business::erp::form_snapshot;
use crate::business::erp::model::normalize_page;
use crate::business::erp::period;
use crate::business::erp::receivable::{self, ReceivableRow};
use crate::business::erp::voucher::{self, EntryLine, SeqSpec};
use crate::business::shared::doc_kind::VoucherSourceType;
use crate::business::shared::scope;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0293 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 / 详情。
pub const PERM_LIST: &str = "business:erp:receipt:list";
/// 详情。
pub const PERM_DETAIL: &str = "business:erp:receipt:detail";
/// 登记收款。
pub const PERM_CREATE: &str = "business:erp:receipt:create";
/// 核销到应收。
pub const PERM_ALLOCATE: &str = "business:erp:receipt:allocate";
/// 状态迁移（确认 / 过账 / 作废）。
pub const PERM_STATUS: &str = "business:erp:receipt:status";

// ==================== 常量 ====================

/// 收款单表。
pub const TABLE: &str = "biz_receipt";
/// 核销明细表。
pub const ALLOC_TABLE: &str = "biz_receipt_allocation";
/// 客户主数据表。
pub const CUSTOMER_TABLE: &str = "biz_customer";

/// 状态：草稿。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：已确认（可核销）。
pub const STATUS_CONFIRMED: &str = "CONFIRMED";
/// 状态：已过账（凭证已生成）。
pub const STATUS_POSTED: &str = "POSTED";
/// 状态：已作废（终态）。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 状态白名单（与 `ck_biz_receipt_status` 逐字一致，单测钉住）。
pub const STATUSES: [&str; 4] = [STATUS_DRAFT, STATUS_CONFIRMED, STATUS_POSTED, STATUS_CANCELLED];

/// 可以核销的状态（`DRAFT` 允许先挂核销、`CONFIRMED` 是常规入口）。
///
/// `POSTED` 之后金额与核销都被凭证锁死；`CANCELLED` 是终态。
pub fn is_allocatable(status: &str) -> bool {
    status == STATUS_DRAFT || status == STATUS_CONFIRMED
}

/// 收款方式：银行转账。
pub const METHOD_BANK_TRANSFER: &str = "BANK_TRANSFER";
/// 收款方式：现金。
pub const METHOD_CASH: &str = "CASH";
/// 收款方式：支票。
pub const METHOD_CHECK: &str = "CHECK";
/// 收款方式：其它。
pub const METHOD_OTHER: &str = "OTHER";

/// 收款方式白名单（与 `ck_biz_receipt_method` 逐字一致）。
pub const METHODS: [&str; 4] = [METHOD_BANK_TRANSFER, METHOD_CASH, METHOD_CHECK, METHOD_OTHER];

/// 单号前缀（`RC-yyyyMMdd-000001`）。
pub const NO_PREFIX: &str = "RC";
/// `biz_seq.seq_type`（V0292 已种）。
pub const SEQ_TYPE: &str = "RC";

/// 收款单取号规则。
pub const RECEIPT_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: SEQ_TYPE,
    prefix: NO_PREFIX,
    seq_name: "收款单号",
    remark: "收款单号：RC-yyyyMMdd-000001，按天（CST）滚动",
};

/// 收款凭证借方科目（银行存款，收到钱）。
pub const DEBIT_CODE: &str = "1002";
/// 收款凭证贷方科目（应收账款，冲减债权）。
pub const CREDIT_CODE: &str = "1122";

/// 凭证来源（唯一真相在 [`VoucherSourceType::Receipt`]）。
pub const SOURCE_RECEIPT: &str = VoucherSourceType::Receipt.as_str();

/// 实体名（唯一真相在 `state_machine::ENTITY_RECEIPT`）。
pub const ENTITY: &str = state_machine::ENTITY_RECEIPT;
/// 合法边（唯一真相在 `state_machine::RECEIPT_EDGES`）。
pub const LEGAL_EDGES: [(&str, &str); 3] = state_machine::RECEIPT_EDGES;
/// 入账态（唯一真相在 `state_machine::RECEIPT_LOCKED`）。
pub const LOCKED: [&str; 2] = state_machine::RECEIPT_LOCKED;

/// 作废原因最短长度（与应收坏账核销同口径：太短的不算可追溯依据）。
pub const REASON_MIN_CHARS: usize = 4;
/// 作废原因最长长度。
pub const REASON_MAX_CHARS: usize = 500;

/// `biz_receipt` 列清单（与 V0292 + V0314 逐列对应；`method` 可空）。
pub const COLUMNS: &str = "id, receipt_no, customer_id, receipt_date, amount, allocated_amount, \
     method, bank_account, status, voucher_id, remark, \
     currency_code, exchange_rate, base_total_amount, base_total_tax, base_total_with_tax, \
     created_at, updated_at";

/// 列表列（带客户名，别名 `p` / `c`）。
pub const LIST_COLUMNS: &str = "p.id, p.receipt_no, p.customer_id, COALESCE(c.name, '') AS customer_name, \
     p.receipt_date, p.amount, p.allocated_amount, p.method, p.bank_account, p.status, p.voucher_id, \
     p.remark, p.currency_code, p.exchange_rate, p.base_total_amount, p.base_total_tax, \
     p.base_total_with_tax, p.created_at, p.updated_at";

/// 列表 FROM 子句。
pub const LIST_FROM: &str = "FROM biz_receipt p \
     LEFT JOIN biz_customer c ON c.id = p.customer_id AND c.deleted_at IS NULL";

/// 列表排序：最新登记在前，同一时刻按单号倒序（稳定）。
pub const LIST_ORDER: &str = "ORDER BY p.created_at DESC, p.receipt_no DESC";

/// 分页目标（`unscoped`：销售台账无归属列，理由见模块文档的 `SCOPE-EXEMPT`）。
pub const LIST_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

// ==================== 纯逻辑（校验 / 金额 / 状态） ====================

/// 未核销余额 = 收款金额 - 已核销金额（`>= 0`，库层 `ck_biz_receipt_amount` 兜底）。
pub fn remaining_of(amount: Decimal, allocated: Decimal) -> Decimal {
    (amount - allocated).round_dp(4)
}

/// 状态中文（错误文案 / 前端 tag 的唯一来源）。
pub fn status_label(status: &str) -> &str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_CONFIRMED => "已确认",
        STATUS_POSTED => "已过账",
        STATUS_CANCELLED => "已作废",
        _ => "未知状态",
    }
}

/// 收款方式中文。
pub fn method_label(method: Option<&str>) -> &str {
    match method {
        Some(METHOD_BANK_TRANSFER) => "银行转账",
        Some(METHOD_CASH) => "现金",
        Some(METHOD_CHECK) => "支票",
        Some(METHOD_OTHER) => "其它",
        _ => "未指定",
    }
}

/// 请求里的收款金额校验：`> 0` 且不超过 `NUMERIC(18,4)` 的整数上限。
pub fn validate_amount(amount: Decimal) -> std::result::Result<Decimal, ApiError> {
    if amount <= Decimal::ZERO {
        return Err(bad_request(format!(
            "amount 必须大于 0（零元 / 负数收款没有业务含义），收到 {amount}"
        )));
    }
    if amount >= Decimal::from(10_000_000_000_000i64) {
        return Err(bad_request(format!(
            "amount 超出 NUMERIC(18,4) 上限（整数部分最多 14 位），收到 {amount}"
        )));
    }
    Ok(amount.round_dp(4))
}

/// 收款方式校验：`None` / 空串 = 未指定；其余必须是 4 值白名单（其余一律 400）。
pub fn validate_method(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if !METHODS.contains(&text) {
        return Err(bad_request(format!(
            "method 只认 {METHODS:?}（收到「{text}」）：银行卡 / 现金 / 支票之外的收款方式请用 OTHER"
        )));
    }
    Ok(Some(text.to_string()))
}

/// 会计期间：`yyyy-MM`（取收款日期所在月）。
pub fn period_code_of(date: NaiveDate) -> String {
    format!("{}-{:02}", date.year(), date.month())
}

/// `yyyy-MM-dd` 日期校验（严格形状，`2026-1-1` 不收）。
pub fn parse_date_arg(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    receivable::parse_date_arg(raw, field)
}

/// 应收在「已收 X」之后该处的状态（OPEN / PARTIAL / SETTLED）。
///
/// 与 `ck_biz_ar_status` 的域值一致 —— **不能**返回 payable 的 `PARTIAL_PAID`。
pub fn receivable_status_of(amount: Decimal, received: Decimal) -> &'static str {
    if received <= Decimal::ZERO {
        receivable::STATUS_OPEN
    } else if received >= amount {
        receivable::STATUS_SETTLED
    } else {
        receivable::STATUS_PARTIAL
    }
}

/// 单条核销的上限校验：本次核销额 ≤ 该应收的可核销余额。
///
/// `allocatable` 由调用方算好 = `unsettled_amount + 本收款此前已核过它的金额`。
pub fn validate_allocation_amount(
    ar_no: &str,
    amount: Decimal,
    allocatable: Decimal,
) -> std::result::Result<(), ApiError> {
    if amount <= Decimal::ZERO {
        return Err(bad_request(format!(
            "核销金额必须大于 0（应收 {ar_no}），收到 {amount}"
        )));
    }
    if amount > allocatable {
        return Err(bad_request(format!(
            "本次核销 {amount} 超过应收 {ar_no} 的可核销余额 {allocatable}\
             （未收金额 + 本收款此前已核销的金额）：请按余额或更小的金额核销"
        )));
    }
    Ok(())
}

/// 汇总校验：核销总额 ≤ 收款金额。
pub fn validate_allocation_total(
    receipt_no: &str,
    total: Decimal,
    amount: Decimal,
) -> std::result::Result<(), ApiError> {
    if total > amount {
        return Err(bad_request(format!(
            "核销总额 {total} 超过收款金额 {amount}（收款单 {receipt_no}）：\
             超收的部分核不出去，请减少核销金额"
        )));
    }
    Ok(())
}

/// 作废原因校验（必填、去空白、长度区间）。
pub fn validate_reason(raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim();
    if text.chars().count() < REASON_MIN_CHARS {
        return Err(bad_request(format!(
            "reason 至少 {REASON_MIN_CHARS} 个字（说明为什么作废这张收款单），收到「{text}」"
        )));
    }
    if text.chars().count() > REASON_MAX_CHARS {
        return Err(bad_request(format!(
            "reason 最多 {REASON_MAX_CHARS} 个字，收到 {} 个字",
            text.chars().count()
        )));
    }
    Ok(text.to_string())
}

/// 400：入参不合法。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409：状态冲突。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 404：收款单不存在。
fn not_found() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "收款单不存在或已删除".to_string())
}

/// 路径 / 请求体里的 UUID：非法 → `400 + 66006`。
pub fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
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

/// 写路径的数据库错误收口：唯一冲突 → 409、CHECK 违反 → 400。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match err {
        DbError::UniqueViolation { constraint, .. } => conflict(format!(
            "唯一约束冲突（{}）：收款单号重复或同一收款对同一应收已有核销明细",
            constraint.as_deref().unwrap_or("<未知>")
        )),
        DbError::CheckViolation { constraint, .. } => bad_request(format!(
            "数据不满足库约束 {}（金额 > 0；已核销 0 ≤ 已核销金额 ≤ 收款金额；\
             作废时已核销必须为 0；核销明细金额 > 0）",
            constraint.as_deref().unwrap_or("<未知>")
        )),
        DbError::InvalidValue(message) => bad_request(message),
        other => utils::db_error(other, ctx),
    }
}

// ==================== 行结构 ====================

/// 收款单行（`method` / `bank_account` / `voucher_id` 可空）。
#[derive(Debug, Clone)]
pub struct ReceiptRow {
    /// 主键。
    pub id: Uuid,
    /// 收款单号。
    pub receipt_no: String,
    /// 客户。
    pub customer_id: Uuid,
    /// 收款日期。
    pub receipt_date: NaiveDate,
    /// 收款金额。
    pub amount: Decimal,
    /// 已核销金额。
    pub allocated_amount: Decimal,
    /// 收款方式。
    pub method: Option<String>,
    /// 收款账户。
    pub bank_account: Option<String>,
    /// 状态。
    pub status: String,
    /// 收款凭证。
    pub voucher_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 原币币种（V0314 / SO-40）。
    pub currency_code: String,
    /// 登记时锁定的汇率（`1 原币 = exchange_rate 本位币`）。
    pub exchange_rate: Decimal,
    /// 本位币收款金额。
    pub base_total_amount: Decimal,
    /// 本位币税额（收款无税额拆分，恒 0）。
    pub base_total_tax: Decimal,
    /// 本位币价税合计。
    pub base_total_with_tax: Decimal,
    /// 创建时间。
    pub created_at: Option<DateTime<Utc>>,
    /// 更新时间。
    pub updated_at: Option<DateTime<Utc>>,
}

impl ReceiptRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            receipt_no: row.get_typed("receipt_no")?,
            customer_id: row.get_typed("customer_id")?,
            receipt_date: row.get_typed("receipt_date")?,
            amount: row.get_typed("amount")?,
            allocated_amount: row.get_typed("allocated_amount")?,
            method: row.get_typed("method")?,
            bank_account: row.get_typed("bank_account")?,
            status: row.get_typed("status")?,
            voucher_id: row.get_typed("voucher_id")?,
            remark: row.get_typed("remark")?,
            currency_code: row.get_typed("currency_code")?,
            exchange_rate: row.get_typed("exchange_rate")?,
            base_total_amount: row.get_typed("base_total_amount")?,
            base_total_tax: row.get_typed("base_total_tax")?,
            base_total_with_tax: row.get_typed("base_total_with_tax")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 未核销余额。
    pub fn remaining(&self) -> Decimal {
        remaining_of(self.amount, self.allocated_amount)
    }
}

/// 核销明细行（连带应收的关键金额，前端抽屉一次看全）。
#[derive(Debug, Clone)]
pub struct AllocationRow {
    /// 主键。
    pub id: Uuid,
    /// 应收单。
    pub receivable_id: Uuid,
    /// 应收单号。
    pub ar_no: String,
    /// 应收金额。
    pub ar_amount: Decimal,
    /// 应收已收。
    pub ar_received: Decimal,
    /// 应收未收。
    pub ar_unsettled: Decimal,
    /// 应收状态。
    pub ar_status: String,
    /// 本次核销金额。
    pub amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 核销时间。
    pub created_at: Option<DateTime<Utc>>,
}

impl AllocationRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            receivable_id: row.get_typed("receivable_id")?,
            ar_no: row.get_typed("ar_no")?,
            ar_amount: row.get_typed("ar_amount")?,
            ar_received: row.get_typed("ar_received")?,
            ar_unsettled: row.get_typed("ar_unsettled")?,
            ar_status: row.get_typed("ar_status")?,
            amount: row.get_typed("amount")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// 按主键取一张收款单（未软删）。
pub async fn find_by_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ReceiptRow>> {
    // SCOPE-EXEMPT: 收款台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!("SELECT {COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(ReceiptRow::from_row).transpose()
}

/// 取一张收款单并**加行锁**（`FOR UPDATE`，核销 / 状态迁移必须先锁）。
pub async fn lock_by_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ReceiptRow>> {
    // SCOPE-EXEMPT: 收款台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!("SELECT {COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(ReceiptRow::from_row).transpose()
}

/// 锁一张应收单（`FOR UPDATE`）。
async fn lock_receivable(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ReceivableRow>> {
    // SCOPE-EXEMPT: 应收台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {} FROM {} WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE",
                receivable::COLUMNS,
                receivable::TABLE
            ),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(ReceivableRow::from_row).transpose()
}

/// 某张收款单的全部核销明细（连带应收金额 / 状态）。
pub async fn allocations_of(ex: &impl SqlExecutor, receipt_id: Uuid) -> DbResult<Vec<AllocationRow>> {
    // SCOPE-EXEMPT: 收款 / 应收台账无归属列（见模块文档）。
    let sql = format!(
        "SELECT a.id, a.receivable_id, a.amount, a.remark, a.created_at, \
                r.ar_no AS ar_no, r.amount AS ar_amount, r.received_amount AS ar_received, \
                r.unsettled_amount AS ar_unsettled, r.status AS ar_status \
           FROM {ALLOC_TABLE} a \
           JOIN {} r ON r.id = a.receivable_id \
          WHERE a.receipt_id = ?::uuid \
          ORDER BY a.created_at, a.id",
        receivable::TABLE
    );
    let rows = ex.query_sql(sql, vec![Value::from(&receipt_id)]).await?;
    rows.iter().map(AllocationRow::from_row).collect()
}

/// 库里真实的核销总额（不信任内存累加：明细可能被并发改动）。
pub async fn allocated_sum(ex: &impl SqlExecutor, receipt_id: Uuid) -> DbResult<Decimal> {
    // SCOPE-EXEMPT: 收款台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!("SELECT COALESCE(SUM(amount), 0) AS total FROM {ALLOC_TABLE} WHERE receipt_id = ?::uuid"),
            vec![Value::from(&receipt_id)],
        )
        .await?;
    match row.as_ref() {
        Some(row) => row.get_typed("total"),
        None => Ok(Decimal::ZERO),
    }
}

/// 凭证号（凭证不存在 / 软删 → `None`，不阻断详情）。
pub async fn voucher_no_of(ex: &impl SqlExecutor, voucher_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: biz_voucher 是会计台账，没有归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            "SELECT voucher_no FROM biz_voucher WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&voucher_id)],
        )
        .await?;
    row.as_ref()
        .map(|row| row.get_typed::<String>("voucher_no"))
        .transpose()
}

/// 客户是否存在且未软删（弱引用，库层无外键 ⇒ 建单时必须自己校验）。
pub async fn customer_exists(ex: &impl SqlExecutor, customer_id: Uuid) -> DbResult<bool> {
    // SCOPE-EXEMPT: biz_customer 是客户主数据（全局），没有归属列。
    let row = ex
        .query_first_sql(
            format!("SELECT 1 AS hit FROM {CUSTOMER_TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"),
            vec![Value::from(&customer_id)],
        )
        .await?;
    Ok(row.is_some())
}

// ==================== 读模型 ====================

/// 收款单（列表 / 详情共用）。
#[derive(Debug, Clone, Serialize)]
pub struct ReceiptRes {
    /// 主键。
    pub id: String,
    /// 收款单号。
    pub receipt_no: String,
    /// 客户。
    pub customer_id: String,
    /// 客户名称。
    pub customer_name: String,
    /// 收款日期。
    pub receipt_date: String,
    /// 收款金额。
    pub amount: Decimal,
    /// 已核销金额。
    pub allocated_amount: Decimal,
    /// 未核销余额（= amount - allocated_amount）。
    pub unallocated_amount: Decimal,
    /// 收款方式。
    pub method: Option<String>,
    /// 收款方式中文。
    pub method_label: String,
    /// 收款账户。
    pub bank_account: Option<String>,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 凭证 id。
    pub voucher_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 原币币种（V0314 / SO-40）。
    pub currency_code: String,
    /// 登记时锁定的汇率。
    pub exchange_rate: Decimal,
    /// 本位币收款金额。
    pub base_total_amount: Decimal,
    /// 本位币税额。
    pub base_total_tax: Decimal,
    /// 本位币价税合计。
    pub base_total_with_tax: Decimal,
    /// 创建时间。
    pub created_at: Option<DateTime<Utc>>,
    /// 更新时间。
    pub updated_at: Option<DateTime<Utc>>,
}

impl ReceiptRes {
    /// 由行 + 客户名构造。
    pub fn from_row(row: &ReceiptRow, customer_name: &str) -> Self {
        Self {
            id: row.id.to_string(),
            receipt_no: row.receipt_no.clone(),
            customer_id: row.customer_id.to_string(),
            customer_name: customer_name.to_string(),
            receipt_date: row.receipt_date.to_string(),
            amount: row.amount,
            allocated_amount: row.allocated_amount,
            unallocated_amount: row.remaining(),
            method: row.method.clone(),
            method_label: method_label(row.method.as_deref()).to_string(),
            bank_account: row.bank_account.clone(),
            status: row.status.clone(),
            status_label: status_label(&row.status).to_string(),
            voucher_id: row.voucher_id.map(|id| id.to_string()),
            remark: row.remark.clone(),
            currency_code: row.currency_code.clone(),
            exchange_rate: row.exchange_rate,
            base_total_amount: row.base_total_amount,
            base_total_tax: row.base_total_tax,
            base_total_with_tax: row.base_total_with_tax,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// 核销明细（详情里给前端抽屉用）。
#[derive(Debug, Clone, Serialize)]
pub struct AllocationRes {
    /// 主键。
    pub id: String,
    /// 应收单 id。
    pub receivable_id: String,
    /// 应收单号。
    pub ar_no: String,
    /// 应收金额。
    pub ar_amount: Decimal,
    /// 应收已收。
    pub ar_received_amount: Decimal,
    /// 应收未收。
    pub ar_unsettled_amount: Decimal,
    /// 应收状态。
    pub ar_status: String,
    /// 应收状态中文。
    pub ar_status_label: String,
    /// 本次核销金额。
    pub amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 核销时间。
    pub created_at: Option<DateTime<Utc>>,
}

impl AllocationRes {
    /// 由行构造。
    pub fn from_row(row: &AllocationRow) -> Self {
        Self {
            id: row.id.to_string(),
            receivable_id: row.receivable_id.to_string(),
            ar_no: row.ar_no.clone(),
            ar_amount: row.ar_amount,
            ar_received_amount: row.ar_received,
            ar_unsettled_amount: row.ar_unsettled,
            ar_status: row.ar_status.clone(),
            ar_status_label: receivable::status_label(&row.ar_status).to_string(),
            amount: row.amount,
            remark: row.remark.clone(),
            created_at: row.created_at,
        }
    }
}

/// 收款单详情。
#[derive(Debug, Clone, Serialize)]
pub struct ReceiptDetailRes {
    /// 单头。
    #[serde(flatten)]
    pub receipt: ReceiptRes,
    /// 凭证号（`voucher_id` 为空或凭证已删时 `None`）。
    pub voucher_no: Option<String>,
    /// 核销明细。
    pub allocations: Vec<AllocationRes>,
}

/// 收款单详情（客户名 / 凭证号 / 核销明细一次取全）。
pub async fn receipt_detail(id: Uuid) -> std::result::Result<ReceiptDetailRes, ApiError> {
    detail_with(db(), id).await
}

/// [`receipt_detail`] 的 executor 版（事务内回读用，看得到本事务未提交的写入）。
pub async fn detail_with(ex: &impl SqlExecutor, id: Uuid) -> std::result::Result<ReceiptDetailRes, ApiError> {
    // SCOPE-EXEMPT: 收款 / 客户 / 凭证均无归属列（见模块文档）。
    let sql = format!("SELECT {LIST_COLUMNS} {LIST_FROM} WHERE p.id = ?::uuid AND p.deleted_at IS NULL");
    let row = ex
        .query_first_sql(sql, vec![Value::from(&id)])
        .await
        .map_err(|error| utils::db_error(error, "查询收款单"))?
        .ok_or_else(not_found)?;
    let receipt = ReceiptRow::from_row(&row).map_err(|error| utils::db_error(error, "解析收款单"))?;
    let customer_name: String = row
        .get_typed("customer_name")
        .map_err(|error| utils::db_error(error, "读取客户名称"))?;
    let voucher_no = match receipt.voucher_id {
        Some(voucher_id) => voucher_no_of(ex, voucher_id)
            .await
            .map_err(|error| utils::db_error(error, "查询收款凭证号"))?,
        None => None,
    };
    let allocations = allocations_of(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "查询核销明细"))?
        .iter()
        .map(AllocationRes::from_row)
        .collect();
    Ok(ReceiptDetailRes {
        receipt: ReceiptRes::from_row(&receipt, &customer_name),
        voucher_no,
        allocations,
    })
}

// ==================== 入参 / 出参 ====================

/// 登记收款请求。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateReceiptReq {
    /// 客户。
    pub customer_id: String,
    /// 收款日期（缺省 = 今天 CST）。
    pub receipt_date: Option<String>,
    /// 收款金额（> 0）。
    pub amount: Decimal,
    /// 收款方式（4 值白名单，可空）。
    pub method: Option<String>,
    /// 收款账户。
    pub bank_account: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 币种（ISO 4217 三位大写，缺省 = 账套本位币）。V0314 / SO-40：
    /// 非本位币 ⇒ 按 `receipt_date` 取最近一条 SPOT 汇率并锁定，取不到一律 400。
    pub currency_code: Option<String>,
}

/// 一条核销明细请求。
#[derive(Debug, Clone, Deserialize)]
pub struct AllocateItemReq {
    /// 应收单 id。
    pub receivable_id: String,
    /// 本次核销金额（> 0，≤ 该应收可核销余额）。
    pub amount: Decimal,
    /// 备注（可选）。
    pub remark: Option<String>,
}

/// 核销请求（**一次多张应收**；同一条明细重复提交 = 改金额）。
#[derive(Debug, Clone, Deserialize)]
pub struct AllocateReq {
    /// 核销明细（至少一条）。
    pub items: Vec<AllocateItemReq>,
}

/// 状态迁移请求。
#[derive(Debug, Clone, Deserialize)]
pub struct StatusReq {
    /// 目标状态（`CONFIRMED` / `POSTED` / `CANCELLED`）。
    pub status: String,
    /// 作废原因（`CANCELLED` 必填，≥4 字）。
    pub reason: Option<String>,
}

/// 分页查询。
#[derive(Debug, Clone, Deserialize)]
pub struct ReceiptPageQuery {
    /// 页码（从 1 起）。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 状态（白名单）。
    pub status: Option<String>,
    /// 客户。
    pub customer_id: Option<String>,
    /// 收款单号 / 客户名（模糊）。
    pub receipt_no: Option<String>,
    /// 收款方式（白名单）。
    pub method: Option<String>,
    /// 收款日期起。
    pub date_from: Option<String>,
    /// 收款日期止。
    pub date_to: Option<String>,
}

/// 收款单筛选（数据层形态）。
#[derive(Debug, Clone, Default)]
pub struct ReceiptFilter {
    /// 状态。
    pub status: Option<String>,
    /// 客户。
    pub customer_id: Option<Uuid>,
    /// 单号 / 客户名（模糊）。
    pub keyword: Option<String>,
    /// 收款方式。
    pub method: Option<String>,
    /// 日期起。
    pub date_from: Option<NaiveDate>,
    /// 日期止。
    pub date_to: Option<NaiveDate>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl ReceiptPageQuery {
    /// 四道入参校验（全部 400，绝不静默忽略）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(status) = self.status.as_deref().map(str::trim)
            && !status.is_empty()
            && !STATUSES.contains(&status)
        {
            return Err(bad_request(format!(
                "status 只认 {STATUSES:?}（收到「{status}」）：OPEN / PARTIAL / PAID 这类\
                     应收 / 应付的域值不是收款单的状态"
            )));
        }
        if let Some(method) = self.method.as_deref().map(str::trim)
            && !method.is_empty()
        {
            validate_method(Some(method))?;
        }
        if let Some(raw) = self.customer_id.as_deref().map(str::trim)
            && !raw.is_empty()
        {
            parse_uuid(raw, "customer_id")?;
        }
        if let Some(raw) = self.date_from.as_deref().map(str::trim)
            && !raw.is_empty()
        {
            parse_date_arg(raw, "date_from")?;
        }
        if let Some(raw) = self.date_to.as_deref().map(str::trim)
            && !raw.is_empty()
        {
            parse_date_arg(raw, "date_to")?;
        }
        Ok(())
    }

    /// 转数据层筛选（空白串 = 没传）。
    pub fn to_filter(&self) -> std::result::Result<ReceiptFilter, ApiError> {
        let (page, page_size) = normalize_page(self.page.unwrap_or(1), self.page_size.unwrap_or(0));
        let mut filter = ReceiptFilter {
            page,
            page_size,
            ..ReceiptFilter::default()
        };
        if let Some(status) = self.status.as_deref().map(str::trim)
            && !status.is_empty()
        {
            filter.status = Some(status.to_string());
        }
        if let Some(method) = self.method.as_deref().map(str::trim)
            && !method.is_empty()
        {
            filter.method = Some(method.to_string());
        }
        if let Some(raw) = self.customer_id.as_deref().map(str::trim)
            && !raw.is_empty()
        {
            filter.customer_id = Some(parse_uuid(raw, "customer_id")?);
        }
        if let Some(raw) = self.receipt_no.as_deref().map(str::trim)
            && !raw.is_empty()
        {
            filter.keyword = Some(raw.to_string());
        }
        if let Some(raw) = self.date_from.as_deref().map(str::trim)
            && !raw.is_empty()
        {
            filter.date_from = Some(parse_date_arg(raw, "date_from")?);
        }
        if let Some(raw) = self.date_to.as_deref().map(str::trim)
            && !raw.is_empty()
        {
            filter.date_to = Some(parse_date_arg(raw, "date_to")?);
        }
        Ok(filter)
    }
}

/// 收款单分页（全部状态；财务要对账，作废的也要看得见）。
pub async fn receipt_page(q: &ReceiptFilter) -> DbResult<(Vec<(ReceiptRow, String)>, i64)> {
    let mut sql = format!("SELECT {LIST_COLUMNS} {LIST_FROM} WHERE p.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND p.status = ?");
        params.push(Value::from(status));
    }
    if let Some(method) = q.method.as_deref() {
        sql.push_str(" AND p.method = ?");
        params.push(Value::from(method));
    }
    if let Some(customer_id) = q.customer_id {
        sql.push_str(" AND p.customer_id = ?::uuid");
        params.push(Value::from(&customer_id));
    }
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (p.receipt_no LIKE ? OR c.name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(from) = q.date_from {
        sql.push_str(" AND p.receipt_date >= ?::text::date");
        params.push(Value::Text(from.to_string()));
    }
    if let Some(to) = q.date_to {
        sql.push_str(" AND p.receipt_date <= ?::text::date");
        params.push(Value::Text(to.to_string()));
    }
    let result = scope::paginate_unscoped(LIST_TARGET, &sql, params, Some(LIST_ORDER), q.page, q.page_size).await?;
    let rows = result
        .list
        .iter()
        .map(|row| {
            let name: String = row.get_typed("customer_name")?;
            Ok((ReceiptRow::from_row(row)?, name))
        })
        .collect::<DbResult<Vec<(ReceiptRow, String)>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

// ==================== 写：登记收款 ====================

/// 今日（CST）。
fn today_cst() -> NaiveDate {
    Utc::now()
        .with_timezone(&nik::prelude::chrono::FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定"))
        .date_naive()
}

/// 登记一张收款单（`DRAFT`，已核销 0），并写建单状态日志。
pub async fn create(operator: Option<Uuid>, req: CreateReceiptReq) -> std::result::Result<ReceiptDetailRes, ApiError> {
    let customer_id = parse_uuid(&req.customer_id, "customer_id")?;
    let amount = validate_amount(req.amount)?;
    let method = validate_method(req.method.as_deref())?;
    let receipt_date = match req.receipt_date.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => parse_date_arg(raw, "receipt_date")?,
        _ => today_cst(),
    };
    let period_code = period_code_of(receipt_date);
    let bank_account = req
        .bank_account
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let remark = req
        .remark
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    // SO-40：登记即锁定汇率（收款无税额拆分 ⇒ base_tax = 0，含税 = base_amount）
    let locked = currency::lock(db(), req.currency_code.as_deref(), receipt_date)
        .await
        .map_err(|error| utils::db_error(error, "取收款汇率"))?
        .into_locked("收款单")?;
    let base_amounts = currency::BaseAmounts::of(amount, Decimal::ZERO, locked.exchange_rate);
    let now = Utc::now();

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启收款登记事务"))?;
    let result = async {
        if !customer_exists(&tx, customer_id)
            .await
            .map_err(|error| utils::db_error(error, "校验客户"))?
        {
            return Err(bad_request(format!(
                "customer_id {customer_id} 不存在或已删除：收款单必须挂在一个有效客户上"
            )));
        }
        period::ensure_period_writable(&tx, &period_code, "登记收款").await?;
        let receipt_no = voucher::next_doc_no(&tx, &RECEIPT_SEQ, now)
            .await
            .map_err(|error| utils::db_error(error, "取收款单号"))?;
        let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();
        // SCOPE-EXEMPT: 收款台账无归属列（见模块文档）。
        let inserted = tx
            .query_first_sql(
                format!(
                    "INSERT INTO {TABLE} \
                        (receipt_no, customer_id, receipt_date, amount, allocated_amount, \
                         method, bank_account, status, remark, \
                         currency_code, exchange_rate, base_total_amount, base_total_tax, base_total_with_tax, \
                         created_at, updated_at, create_by, update_by) \
                     VALUES (?, ?::uuid, ?::text::date, NULLIF(?::text,'')::numeric, 0, \
                             NULLIF(?::text,''), NULLIF(?::text,''), '{STATUS_DRAFT}', \
                             NULLIF(?::text,''), \
                             ?, ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                             ?, ?, NULLIF(?::text,'')::uuid, \
                             NULLIF(?::text,'')::uuid) \
                     RETURNING id"
                ),
                vec![
                    Value::Text(receipt_no.clone()),
                    Value::from(&customer_id),
                    Value::Text(receipt_date.to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(method.clone().unwrap_or_default()),
                    Value::Text(bank_account.clone().unwrap_or_default()),
                    Value::Text(remark.clone().unwrap_or_default()),
                    Value::Text(locked.currency_code.clone()),
                    Value::Text(locked.exchange_rate.to_string()),
                    Value::Text(base_amounts.amount.to_string()),
                    Value::Text(base_amounts.tax.to_string()),
                    Value::Text(base_amounts.with_tax.to_string()),
                    Value::from(now),
                    Value::from(now),
                    Value::Text(operator_text.clone()),
                    Value::Text(operator_text),
                ],
            )
            .await
            .map_err(|error| write_error(error, "登记收款单"))?
            .ok_or_else(|| {
                ApiError::new(
                    500,
                    error_code::INTERNAL_ERROR,
                    "收款单写入失败：INSERT 没有返回 id".to_string(),
                )
            })?;
        let id: Uuid = inserted
            .get_typed("id")
            .map_err(|error| utils::db_error(error, "读取收款单 id"))?;
        state_machine::log_created(&tx, ENTITY, id, &receipt_no, STATUS_DRAFT, operator, Some("收款登记"))
            .await
            .map_err(|error| {
                write_error(
                    DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)),
                    "收款单建单日志",
                )
            })?;
        detail_with(&tx, id).await
    }
    .await;
    match result {
        Ok(detail) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交收款登记事务"))?;
            Ok(detail)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== 写：核销 ====================

/// 核销请求的「已校验」形态。
#[derive(Debug, Clone)]
struct PlannedItem {
    /// 应收单 id。
    receivable_id: Uuid,
    /// 应收单号（锁住后回填）。
    ar_no: String,
    /// 本次核销金额。
    amount: Decimal,
    /// 该收款此前对这张应收核过的金额。
    previous: Decimal,
    /// 锁住后的应收行。
    receivable: ReceivableRow,
    /// 备注。
    remark: Option<String>,
}

/// 把一条收款核销到多张应收（**同一事务**：明细 / 应收回写 / 汇总同生共死）。
pub async fn allocate(
    operator: Option<Uuid>,
    id: &str,
    req: AllocateReq,
) -> std::result::Result<ReceiptDetailRes, ApiError> {
    let id = parse_uuid(id, "id")?;
    if req.items.is_empty() {
        return Err(bad_request(
            "items 不能为空：核销至少要指定一张应收（反核销请用金额 0 之外的方式，本卡未开放）".to_string(),
        ));
    }
    // ① 请求内重复应收 → 400（同一 (receipt, receivable) 只有一行，重复会让 SUM 失真）
    let mut raw_items: Vec<(Uuid, Decimal, Option<String>)> = Vec::with_capacity(req.items.len());
    for item in &req.items {
        let receivable_id = parse_uuid(&item.receivable_id, "receivable_id")?;
        if raw_items.iter().any(|(existing, _, _)| *existing == receivable_id) {
            return Err(bad_request(format!(
                "items 里应收 {receivable_id} 出现了多次：同一张收款对同一笔应收只能有一行核销\
                 （要改金额请只提交一条）"
            )));
        }
        let amount = item.amount.round_dp(4);
        if amount <= Decimal::ZERO {
            return Err(bad_request(format!(
                "核销金额必须大于 0（应收 {receivable_id}），收到 {amount}"
            )));
        }
        let remark = item
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        raw_items.push((receivable_id, amount, remark));
    }
    // ② 锁顺序固定：按 receivable_id 排序后再逐把锁（并发两笔收款核同一批应收不会交叉死锁）
    raw_items.sort_by_key(|(receivable_id, _, _)| *receivable_id);

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启核销事务"))?;
    let result = async {
        let receipt = lock_by_id(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定收款单"))?
            .ok_or_else(not_found)?;
        if !is_allocatable(&receipt.status) {
            return Err(conflict(format!(
                "收款单 {} 当前是「{}」，只有草稿 / 已确认的收款单能核销：\
                 已过账（POSTED）的金额与核销被凭证锁定，已作废（CANCELLED）的更不能再核",
                receipt.receipt_no,
                status_label(&receipt.status)
            )));
        }
        period::ensure_period_writable(&tx, &period_code_of(receipt.receipt_date), "核销收款").await?;

        // ③ 逐条：锁应收 → 校验客户 / 状态 / 单张上限
        let mut planned: Vec<PlannedItem> = Vec::with_capacity(raw_items.len());
        for (receivable_id, amount, remark) in raw_items {
            let receivable = lock_receivable(&tx, receivable_id)
                .await
                .map_err(|error| utils::db_error(error, "锁定应收单"))?
                .ok_or_else(|| {
                    ApiError::new(
                        404,
                        error_code::RESOURCE_NOT_FOUND,
                        format!("应收单 {receivable_id} 不存在或已删除"),
                    )
                })?;
            if receivable.customer_id != receipt.customer_id {
                return Err(bad_request(format!(
                    "应收单 {} 的客户是 {}，与收款单 {} 的客户 {} 不一致：\
                     收款只能核销同一个客户名下的应收",
                    receivable.ar_no, receivable.customer_id, receipt.receipt_no, receipt.customer_id
                )));
            }
            if !receivable::has_balance(&receivable.status) {
                return Err(conflict(format!(
                    "应收单 {} 当前是「{}」，不能核销（只有未收 / 部分已收的应收能收款）：\
                     已结清 / 已坏账核销 / 已作废的应收都不能再收款",
                    receivable.ar_no,
                    receivable::status_label(&receivable.status)
                )));
            }
            let previous = existing_allocation(&tx, receipt.id, receivable_id)
                .await
                .map_err(|error| utils::db_error(error, "查询已有核销明细"))?;
            // 本收款此前核过它的那部分已经计入 received_amount ⇒ 先加回来才是本次可用余额
            let allocatable = (receivable.unsettled_amount + previous).round_dp(4);
            validate_allocation_amount(&receivable.ar_no, amount, allocatable)?;
            planned.push(PlannedItem {
                receivable_id,
                ar_no: receivable.ar_no.clone(),
                amount,
                previous,
                receivable,
                remark,
            });
        }

        // ④ 汇总校验：合计 ≤ 收款金额（不在本次明细里的既有核销也要算进来）
        let mut total = Decimal::ZERO;
        for item in &planned {
            total += item.amount;
        }
        let untouched = (receipt.allocated_amount
            - planned
                .iter()
                .map(|item| item.previous)
                .fold(Decimal::ZERO, |acc, value| acc + value))
        .round_dp(4);
        let projected = (untouched + total).round_dp(4);
        if projected < Decimal::ZERO {
            return Err(conflict(format!(
                "收款单 {} 的已核销金额与核销明细不一致（头 {} ≠ 明细合计）：库层数据漂移，请先排查",
                receipt.receipt_no, receipt.allocated_amount
            )));
        }
        validate_allocation_total(&receipt.receipt_no, projected, receipt.amount)?;

        // ⑤ 落明细 + 回写应收
        let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();
        for item in &planned {
            // SCOPE-EXEMPT: 收录 / 应收台账无归属列（见模块文档）。
            tx.exec_sql(
                format!(
                    "INSERT INTO {ALLOC_TABLE} (receipt_id, receivable_id, amount, remark, created_at, create_by) \
                     VALUES (?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, NULLIF(?::text,''), ?, \
                             NULLIF(?::text,'')::uuid) \
                     ON CONFLICT (receipt_id, receivable_id) \
                     DO UPDATE SET amount = EXCLUDED.amount, remark = EXCLUDED.remark"
                ),
                vec![
                    Value::from(&receipt.id),
                    Value::from(&item.receivable_id),
                    Value::Text(item.amount.to_string()),
                    Value::Text(item.remark.clone().unwrap_or_default()),
                    Value::from(now),
                    Value::Text(operator_text.clone()),
                ],
            )
            .await
            .map_err(|error| write_error(error, "落核销明细"))?;

            let delta = (item.amount - item.previous).round_dp(4);
            if delta == Decimal::ZERO {
                continue;
            }
            let received = (item.receivable.received_amount + delta).round_dp(4);
            if received < Decimal::ZERO {
                return Err(bad_request(format!(
                    "本次核销 {delta} 会让应收 {} 的已收金额变成负数（当前已收 {}）",
                    item.ar_no, item.receivable.received_amount
                )));
            }
            let unsettled = receivable::unsettled_of(item.receivable.amount, received);
            if unsettled < Decimal::ZERO {
                return Err(bad_request(format!(
                    "本次核销 {delta} 会导致应收 {} 的已收 {received} 超过应收金额 {}",
                    item.ar_no, item.receivable.amount
                )));
            }
            let next_status = receivable_status_of(item.receivable.amount, received);
            let reason = format!("收款 {} 核销", receipt.receipt_no);
            state_machine::transition_if_changed(
                &tx,
                state_machine::ENTITY_RECEIVABLE,
                item.receivable_id,
                &item.ar_no,
                &state_machine::RECEIVABLE_EDGES,
                &item.receivable.status,
                next_status,
                operator,
                Some(&reason),
                Some((ENTITY, receipt.receipt_no.as_str())),
            )
            .await
            .map_err(|error| {
                write_error(
                    DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)),
                    "应收单状态变更",
                )
            })?;
            // CAS：仍按读到的 received_amount 做条件，兜住「外部绕过锁改库」
            let affected = tx
                .exec_sql(
                    format!(
                        "UPDATE {} \
                            SET received_amount = NULLIF(?::text,'')::numeric, \
                                unsettled_amount = NULLIF(?::text,'')::numeric, \
                                status = ?, \
                                settled_at = CASE WHEN ?::text = '' THEN settled_at ELSE ?::text::timestamptz END, \
                                updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                          WHERE id = ?::uuid AND deleted_at IS NULL \
                            AND received_amount = NULLIF(?::text,'')::numeric",
                        receivable::TABLE
                    ),
                    vec![
                        Value::Text(received.to_string()),
                        Value::Text(unsettled.to_string()),
                        Value::Text(next_status.to_string()),
                        Value::Text(if unsettled == Decimal::ZERO {
                            now.to_rfc3339()
                        } else {
                            String::new()
                        }),
                        Value::Text(now.to_rfc3339()),
                        Value::from(now),
                        Value::Text(operator_text.clone()),
                        Value::from(&item.receivable_id),
                        Value::Text(item.receivable.received_amount.to_string()),
                    ],
                )
                .await
                .map_err(|error| write_error(error, "回写应收单"))?;
            if affected == 0 {
                return Err(conflict(format!(
                    "应收单 {} 的已收金额在本次核销期间被其它操作改动（并发收款），本次核销已回滚，请重试",
                    item.ar_no
                )));
            }
        }

        // ⑤ SO-38 / ARCH-7 §15.5.2 ②：**应收核销 = 信用额度释放的主出口**。
        // 与核销同事务 —— 钱收到了，额度必须当场还回去，不能等每日兜底重算。
        // 一张收款单只允许核销同一客户的应收（上面已校验），所以这里同步一个客户即可。
        if !planned.is_empty() {
            credit::sync(&tx, receipt.customer_id, credit::CAUSE_AR_SETTLED, &receipt.receipt_no)
                .await
                .map_err(|error| utils::db_error(error, "同步客户信用额度（收款核销）"))?;
        }

        // ⑥ 回写收款头：allocated_amount 以**库里的明细合计**为准
        let allocated = allocated_sum(&tx, receipt.id)
            .await
            .map_err(|error| utils::db_error(error, "汇总核销金额"))?;
        tx.exec_sql(
            format!(
                "UPDATE {TABLE} SET allocated_amount = NULLIF(?::text,'')::numeric, updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![
                Value::Text(allocated.to_string()),
                Value::from(now),
                Value::Text(operator_text.clone()),
                Value::from(&receipt.id),
            ],
        )
        .await
        .map_err(|error| write_error(error, "回写收款单已核销金额"))?;

        detail_with(&tx, receipt.id).await
    }
    .await;
    match result {
        Ok(detail) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交核销事务"))?;
            Ok(detail)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 本收款对某张应收已有的核销金额（没有 → 0）。
async fn existing_allocation(ex: &impl SqlExecutor, receipt_id: Uuid, receivable_id: Uuid) -> DbResult<Decimal> {
    // SCOPE-EXEMPT: 收款台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!("SELECT amount FROM {ALLOC_TABLE} WHERE receipt_id = ?::uuid AND receivable_id = ?::uuid"),
            vec![Value::from(&receipt_id), Value::from(&receivable_id)],
        )
        .await?;
    match row.as_ref() {
        Some(row) => row.get_typed("amount"),
        None => Ok(Decimal::ZERO),
    }
}

// ==================== 写：状态迁移 ====================

/// 状态迁移（`DRAFT→CONFIRMED` / `CONFIRMED→POSTED` / `DRAFT→CANCELLED`）。
///
/// - `POSTED`：要求核销满（`allocated_amount == amount`），生成收款凭证
///   借 1002 银行存款 / 贷 1122 应收账款（各 = 收款金额），并回写 `voucher_id`；
/// - `CANCELLED`：要求一分没核销（`allocated_amount = 0`，库层 `ck_biz_receipt_cancelled` 兜底），
///   原因必填（≥4 字），并写进状态日志。
pub async fn change_status(
    operator: Option<Uuid>,
    id: &str,
    req: StatusReq,
) -> std::result::Result<ReceiptDetailRes, ApiError> {
    let id = parse_uuid(id, "id")?;
    let target = req.status.trim().to_string();
    if !STATUSES.contains(&target.as_str()) {
        return Err(bad_request(format!("status 只认 {STATUSES:?}（收到「{target}」）")));
    }
    let reason = match req.reason.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(validate_reason(raw)?),
        _ => None,
    };
    let now = Utc::now();

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启收款状态迁移事务"))?;
    let result = async {
        let receipt = lock_by_id(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定收款单"))?
            .ok_or_else(not_found)?;
        if receipt.status == target {
            return Err(conflict(format!(
                "收款单 {} 已经是「{}」，没有状态变化",
                receipt.receipt_no,
                status_label(&target)
            )));
        }
        if !LEGAL_EDGES
            .iter()
            .any(|(from, to)| *from == receipt.status && *to == target)
        {
            return Err(conflict(format!(
                "收款单 {} 不能从「{}」变成「{}」：合法去向是 {}",
                receipt.receipt_no,
                status_label(&receipt.status),
                status_label(&target),
                describe_next(&receipt.status)
            )));
        }
        // ATT-06：收款单没有独立的「提交」态，CONFIRMED（确认收款）就是提交动作 —— 银行回单必传。
        if target == STATUS_CONFIRMED {
            attach_rule::ensure_attachments(AttachDocKind::Receipt, id, AttachAt::Submit).await?;
        }
        match target.as_str() {
            STATUS_POSTED => {
                if receipt.allocated_amount != receipt.amount {
                    return Err(conflict(format!(
                        "收款单 {} 还有 {} 未核销到应收（收款 {} / 已核销 {}）：\
                         先核销满再过账，否则凭证会把未核销的钱贷记成应收账款收回",
                        receipt.receipt_no,
                        receipt.remaining(),
                        receipt.amount,
                        receipt.allocated_amount
                    )));
                }
                period::ensure_period_writable(&tx, &period_code_of(receipt.receipt_date), "收款过账").await?;
            }
            STATUS_CANCELLED => {
                if receipt.allocated_amount != Decimal::ZERO {
                    return Err(conflict(format!(
                        "收款单 {} 已核销 {}，不能直接作废：\
                         作废会让应收的「已收款」凭空多出来。请先反核销（删明细 + 回滚应收）再作废",
                        receipt.receipt_no, receipt.allocated_amount
                    )));
                }
            }
            _ => {}
        }

        state_machine::transition(
            &tx,
            ENTITY,
            receipt.id,
            &receipt.receipt_no,
            &LEGAL_EDGES,
            &receipt.status,
            &target,
            operator,
            reason.as_deref(),
            None,
        )
        .await
        .map_err(|error| {
            write_error(
                DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)),
                "收款单状态变更",
            )
        })?;

        let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();
        let affected = tx
            .exec_sql(
                format!(
                    "UPDATE {TABLE} SET status = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                      WHERE id = ?::uuid AND deleted_at IS NULL AND status = ?"
                ),
                vec![
                    Value::Text(target.clone()),
                    Value::from(now),
                    Value::Text(operator_text.clone()),
                    Value::from(&receipt.id),
                    Value::Text(receipt.status.clone()),
                ],
            )
            .await
            .map_err(|error| write_error(error, "回写收款单状态"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "收款单 {} 的状态在本次迁移期间被其它操作改动，本次操作已回滚，请重试",
                receipt.receipt_no
            )));
        }

        if target == STATUS_POSTED {
            let voucher_id = post_receipt_voucher(&tx, &receipt, operator, now).await?;
            tx.exec_sql(
                format!("UPDATE {TABLE} SET voucher_id = ?::uuid, updated_at = ? WHERE id = ?::uuid"),
                vec![Value::from(&voucher_id), Value::from(now), Value::from(&receipt.id)],
            )
            .await
            .map_err(|error| write_error(error, "收款单回写凭证"))?;
        }

        detail_with(&tx, receipt.id).await
    }
    .await;
    match result {
        Ok(detail) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交收款状态迁移事务"))?;
            Ok(detail)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 当前状态能去哪（错误文案里用）。
pub fn describe_next(status: &str) -> String {
    let next: Vec<&str> = LEGAL_EDGES
        .iter()
        .filter(|(from, _)| *from == status)
        .map(|(_, to)| *to)
        .collect();
    if next.is_empty() {
        "无（终态）".to_string()
    } else {
        next.join(" / ")
    }
}

/// 收款凭证：借 1002 银行存款 / 贷 1122 应收账款，各 = 收款金额（同事务，直接 POSTED）。
async fn post_receipt_voucher(
    tx: &Transaction,
    receipt: &ReceiptRow,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<Uuid, ApiError> {
    let bank = voucher::account_required(tx, DEBIT_CODE, "收款凭证的借方科目 1002 银行存款")
        .await
        .map_err(|error| write_error(error, "解析收款凭证借方科目"))?;
    let receivable_account = voucher::account_required(tx, CREDIT_CODE, "收款凭证的贷方科目 1122 应收账款")
        .await
        .map_err(|error| write_error(error, "解析收款凭证贷方科目"))?;
    // 摘要在账面上是给人看的（AGENTS.md 约定 13）：客户取**名称**，不写 UUID。
    // 主数据被物理删了退化成「客户已删除(短 id)」，绝不回落整段 36 位 UUID（E2E-FIX-5 ①）。
    let customer = voucher::party_label(
        form_snapshot::customer_name(tx, receipt.customer_id)
            .await
            .map_err(|error| write_error(error, "读收款单客户名称"))?,
        "客户",
        receipt.customer_id,
    );
    let summary = format!("收款 {} 客户 {}", receipt.receipt_no, customer);
    let lines = vec![
        EntryLine {
            line_no: 1,
            summary: format!("{summary} 银行收款"),
            account: bank,
            debit: receipt.amount,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            // 银行存款是公司级账户，不挂部门（与付款凭证同口径）
            aux_dept_id: None,
        },
        EntryLine {
            line_no: 2,
            summary: format!("{summary} 冲减应收账款"),
            account: receivable_account,
            debit: Decimal::ZERO,
            credit: receipt.amount,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        },
    ];
    voucher::post_voucher(
        tx,
        &voucher::PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_AUTO,
            source_type: SOURCE_RECEIPT,
            source_id: Some(receipt.id),
            source_no: Some(receipt.receipt_no.as_str()),
            source_doc_no: None,
            voucher_date: receipt.receipt_date,
            summary,
            lines,
            remark: receipt.remark.as_deref(),
            red_of: None,
            status: voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await
    .map_err(|error| write_error(error, "生成收款凭证"))
}

// ==================== 接口层 ====================

/// 登记收款。
#[nik::permission(
    path = "/business/erp/receipt",
    method = "POST",
    permission = "business:erp:receipt:create"
)]
#[nik::post("/business/erp/receipt")]
pub async fn receipt_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateReceiptReq>,
) -> ResResult<ReceiptDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create(operator, req).await?))
}

/// 收款单详情。
#[nik::permission(
    path = "/business/erp/receipt/{id}",
    method = "GET",
    permission = "business:erp:receipt:detail"
)]
#[nik::get("/business/erp/receipt/{id}")]
pub async fn receipt_detail_handler(Path(id): Path<String>) -> ResResult<ReceiptDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(receipt_detail(id).await?))
}

/// 收款单分页。
#[nik::permission(
    path = "/business/erp/receipt/page",
    method = "GET",
    permission = "business:erp:receipt:list"
)]
#[nik::get("/business/erp/receipt/page")]
pub async fn receipt_page_handler(Query(query): Query<ReceiptPageQuery>) -> ResResult<PageData<Vec<ReceiptRes>>> {
    query.validate()?;
    let filter = query.to_filter()?;
    let (rows, total) = receipt_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询收款单"))?;
    let list = rows.iter().map(|(row, name)| ReceiptRes::from_row(row, name)).collect();
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        filter.page,
        filter.page_size,
    ))
}

/// 核销到应收。
#[nik::permission(
    path = "/business/erp/receipt/{id}/allocate",
    method = "POST",
    permission = "business:erp:receipt:allocate"
)]
#[nik::post("/business/erp/receipt/{id}/allocate")]
pub async fn receipt_allocate(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<AllocateReq>,
) -> ResResult<ReceiptDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(allocate(operator, &id, req).await?))
}

/// 状态迁移（确认 / 过账 / 作废）。
#[nik::permission(
    path = "/business/erp/receipt/{id}/status",
    method = "PATCH",
    permission = "business:erp:receipt:status"
)]
#[nik::patch("/business/erp/receipt/{id}/status")]
pub async fn receipt_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<StatusReq>,
) -> ResResult<ReceiptDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(change_status(operator, &id, req).await?))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::rust_decimal::prelude::FromPrimitive;

    fn dec(value: f64) -> Decimal {
        Decimal::from_f64(value).expect("小数可转 Decimal")
    }

    #[test]
    fn status_domain_matches_arch7() {
        assert_eq!(
            STATUSES,
            ["DRAFT", "CONFIRMED", "POSTED", "CANCELLED"],
            "收款单状态域（ARCH-7 §4.4）不许混入应收 / 应付的值"
        );
        for status in STATUSES {
            assert_ne!(status_label(status), "未知状态", "{status} 缺中文标签");
        }
        assert_eq!(status_label("OPEN"), "未知状态", "应收的 OPEN 不是收款状态");
        assert_eq!(
            status_label("PARTIAL_PAID"),
            "未知状态",
            "应付的 PARTIAL_PAID 不是收款状态"
        );
    }

    #[test]
    fn allocatable_statuses_are_draft_and_confirmed() {
        assert!(is_allocatable(STATUS_DRAFT));
        assert!(is_allocatable(STATUS_CONFIRMED));
        assert!(!is_allocatable(STATUS_POSTED), "过账后金额被凭证锁定");
        assert!(!is_allocatable(STATUS_CANCELLED));
    }

    #[test]
    fn edges_are_registered_and_consistent() {
        // 边表 = 共享注册表（改一处必须改两处，否则这里红）
        assert_eq!(LEGAL_EDGES, state_machine::RECEIPT_EDGES);
        assert_eq!(
            state_machine::Entity::Receipt.edges(),
            &state_machine::RECEIPT_EDGES[..]
        );
        assert_eq!(
            state_machine::Entity::Receipt.locked_statuses(),
            &state_machine::RECEIPT_LOCKED[..]
        );
        assert_eq!(state_machine::Entity::Receipt.as_str(), "RECEIPT");
        assert_eq!(state_machine::Entity::Receipt.label(), "收款单");
        assert_eq!(
            state_machine::Entity::parse("RECEIPT"),
            Some(state_machine::Entity::Receipt)
        );
        // 每条边都在状态域内，且没有入边到 DRAFT
        for (from, to) in LEGAL_EDGES {
            assert!(STATUSES.contains(&from), "边起点 {from} 不在状态域");
            assert!(STATUSES.contains(&to), "边终点 {to} 不在状态域");
            assert_ne!(to, STATUS_DRAFT, "没有任何边回到草稿");
        }
        // reason 边必须同时是合法边
        for edge in state_machine::RECEIPT_REASON_EDGES {
            assert!(LEGAL_EDGES.contains(&edge), "reason 边 {edge:?} 不在合法边表里");
        }
        assert_eq!(describe_next(STATUS_DRAFT), "CONFIRMED / CANCELLED");
        assert_eq!(describe_next(STATUS_CONFIRMED), "POSTED");
        assert_eq!(describe_next(STATUS_POSTED), "无（终态）");
    }

    #[test]
    fn voucher_source_is_registered() {
        assert_eq!(VoucherSourceType::Receipt.as_str(), SOURCE_RECEIPT);
        assert_eq!(
            VoucherSourceType::parse(SOURCE_RECEIPT),
            Some(VoucherSourceType::Receipt)
        );
        assert_eq!(
            VoucherSourceType::Receipt.doc_kind(),
            Some(crate::business::shared::doc_kind::DocKind::Receipt)
        );
        assert_eq!(crate::business::shared::doc_kind::DocKind::Receipt.table(), Some(TABLE));
    }

    #[test]
    fn amount_and_remaining_math() {
        assert_eq!(remaining_of(dec(1000.0), dec(400.0)), dec(600.0));
        assert_eq!(remaining_of(dec(1000.0), dec(1000.0)), Decimal::ZERO);
        assert_eq!(remaining_of(dec(0.0004), dec(0.0002)), dec(0.0002));
        assert_eq!(validate_amount(dec(0.0001)).expect("正数可过"), dec(0.0001));
        assert!(validate_amount(Decimal::ZERO).is_err(), "0 元收款无业务含义");
        assert!(validate_amount(dec(-1.0)).is_err(), "负数收款要拒");
        assert!(
            validate_amount(Decimal::from(10_000_000_000_000i64)).is_err(),
            "超出 NUMERIC(18,4) 整数上限要拒"
        );
    }

    #[test]
    fn method_whitelist() {
        assert_eq!(validate_method(None).expect("可空"), None);
        assert_eq!(validate_method(Some("  ")).expect("空白=未指定"), None);
        for method in METHODS {
            assert_eq!(
                validate_method(Some(method)).expect("白名单可过"),
                Some(method.to_string())
            );
        }
        let err = validate_method(Some("ALIPAY")).expect_err("支付宝不在白名单");
        assert!(err.msg.contains("ALIPAY"), "文案要点名非法值：{}", err.msg);
        assert_eq!(method_label(Some(METHOD_CASH)), "现金");
        assert_eq!(method_label(None), "未指定");
    }

    #[test]
    fn allocation_line_cap() {
        validate_allocation_amount("AR-1", dec(600.0), dec(600.0)).expect("刚好等于余额可以");
        let err = validate_allocation_amount("AR-1", dec(600.0001), dec(600.0)).expect_err("超过未收余额必须拒");
        assert!(err.msg.contains("AR-1"), "文案要点名单号：{}", err.msg);
        assert!(err.msg.contains("600.0001"), "文案要点名本次金额：{}", err.msg);
        let zero = validate_allocation_amount("AR-1", Decimal::ZERO, dec(600.0)).expect_err("0 核销要拒");
        assert!(zero.msg.contains("大于 0"), "{}", zero.msg);
    }

    #[test]
    fn allocation_total_cap() {
        validate_allocation_total("RC-1", dec(1000.0), dec(1000.0)).expect("刚好核销满可以");
        let err =
            validate_allocation_total("RC-1", dec(1000.0001), dec(1000.0)).expect_err("核销总额超过收款金额必须拒");
        assert!(err.msg.contains("RC-1"), "文案要点名单号：{}", err.msg);
        assert!(err.msg.contains("超过收款金额"), "{}", err.msg);
    }

    #[test]
    fn receivable_status_transitions_follow_ar_domain() {
        assert_eq!(receivable_status_of(dec(1000.0), Decimal::ZERO), "OPEN");
        assert_eq!(receivable_status_of(dec(1000.0), dec(400.0)), "PARTIAL");
        assert_eq!(receivable_status_of(dec(1000.0), dec(1000.0)), "SETTLED");
        // 收多了也应落到 SETTLED（真正的越界由调用方先拦住）
        assert_eq!(receivable_status_of(dec(1000.0), dec(1001.0)), "SETTLED");
    }

    #[test]
    fn period_code_and_date_shape() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 6).expect("合法日期");
        assert_eq!(period_code_of(date), "2026-11");
        let january = NaiveDate::from_ymd_opt(2027, 1, 5).expect("合法日期");
        assert_eq!(period_code_of(january), "2027-01", "月份必须补零");
        assert!(parse_date_arg("2026-11-06", "receipt_date").is_ok());
        assert!(
            parse_date_arg("2026-1-6", "receipt_date").is_err(),
            "不补零的日期要拒（口径统一 yyyy-MM-dd）"
        );
    }

    #[test]
    fn reason_bounds() {
        assert!(validate_reason("  无  ").is_err(), "太短不算依据");
        assert_eq!(
            validate_reason("  客户退单，银行退回  ").expect("正常原因"),
            "客户退单，银行退回"
        );
        assert!(validate_reason(&"字".repeat(REASON_MAX_CHARS + 1)).is_err(), "超长要拒");
    }
}
