//! 收款退款单（销售链 · SO-14 遗留卡 t_573bfe5b · 路线 A）。
//!
//! # 这张单据解决什么问题
//!
//! SO-14 遗留卡把「出库作废 → 应收冲销」钉在**一分没收**的 `OPEN` 应收上
//! （`receivable::cancel_for_stock_out`：`status = 'OPEN' AND received_amount = 0`），
//! 已收过款的应收被 409 拦住，文案要求「先冲收款」。本模块就是那条「冲收款」的路：
//!
//! ```text
//! 应收 PARTIAL/SETTLED（received_amount > 0）
//!   → 登记退款单 RR-yyyyMMdd-000001（DRAFT）
//!   → 退款核销 allocate（received_amount -= 金额 ⇒ 应收回到 PARTIAL / OPEN）
//!   → 确认（CONFIRMED）→ 过账（POSTED，生成凭证 借 1122 应收 / 贷 1002 银行存款）
//!   → 此时应收已是「OPEN 且 received_amount = 0」
//!   → 作废出库单就能走既有的 OPEN → CANCELLED（**CANCELLED 的语义不动**）
//! ```
//!
//! # 表结构（V0294，与 SO-16 的 `biz_receipt` 镜像但**不复用**）
//!
//! ```text
//! biz_receipt_refund              id, refund_no(RR-yyyyMMdd-000001), customer_id,
//!                                 source_receipt_id(弱引用，可空), refund_date, amount,
//!                                 allocated_amount, method, bank_account,
//!                                 status(DRAFT/CONFIRMED/POSTED/CANCELLED), voucher_id,
//!                                 reason(必填 >= 4 字), remark + 审计五件套
//!                                 CHECK amount > 0 AND 0 <= allocated_amount <= amount
//!                                 CHECK status <> 'CANCELLED' OR allocated_amount = 0
//!                                 CHECK length(btrim(reason)) >= 4
//!                                 UNIQUE (refund_no) WHERE deleted_at IS NULL
//! biz_receipt_refund_allocation   id, refund_id(CASCADE), receivable_id(RESTRICT),
//!                                 amount(>0，正数 = 本次冲减的已收款), remark, created_at, create_by
//!                                 UNIQUE (refund_id, receivable_id)   ← 重复核销 = 改金额（UPSERT）
//! ```
//!
//! **为什么另开一张表而不是给 `biz_receipt` 加 `receipt_type`**：
//! ① V0292 的 `ck_biz_rcpt_alloc_amount (amount > 0)` / `ck_biz_receipt_amount` 是已应用迁移的约束，
//! 同表混进退款要再发迁移 DROP/ADD 既有约束；② SO-17 的收款读侧（列表 / 详情 / 核销 / 过账）
//! 全部按「收款」假设写死，退款行会混进收款台账、并可能被再次核销到应收；
//! ③ 负金额（红字应收 / 负数核销）会污染 `ck_biz_ar_amount (amount > 0)`、账龄与报表。
//! 结论：**结构镜像、语义独立**，`amount` 一律正数，方向由单据类型表达。
//!
//! # 核销方向（与收款相反）
//!
//! ```text
//! 收款：received_amount += 核销额，状态 OPEN → PARTIAL → SETTLED
//! 退款：received_amount -= 冲减额，状态 SETTLED → PARTIAL → OPEN（本卡给 RECEIVABLE_EDGES
//!       补了 3 条「退款冲减」向下边，见 state_machine::RECEIVABLE_EDGES 的文档）
//! ```
//!
//! 可退上限 = 该应收当前的 `received_amount` **加上本退款此前已冲减它的金额**
//! （已冲减的那部分已经从 `received_amount` 里扣掉了，不先加回来会误拒「改金额」）。
//!
//! # 过账（POSTED）为什么要求「核销满」
//!
//! 凭证是卡面逐字的两行：**借 1122 应收账款 / 贷 1002 银行存款**，各 = 退款金额
//! （与 SO-17 收款凭证严格镜像）。若还有 `amount - allocated_amount > 0` 的未核销余额，
//! 把它借记 1122 就是凭空增加一笔不存在的应收账款（真实的「已付出但还没抵哪张应收」应记
//! 1123 预付账款，属 ARCH-7 §8.1 的预留项）。所以本卡口径：**未核销余额 > 0 时拒绝过账（409）**，
//! 让财务先核销（或作废重登）。这条同时把「退款只冲已收过的钱」钉死。
//!
//! # POSTED 是终态（刻意）
//!
//! 钱已付出且凭证已记账，退款单不能作废 —— 要抹掉只能由客户重新回一张收款单
//! （那才是「钱又回来了」），或在凭证层红冲（`voucher::reverse_posted`）。
//! 这与收款单「`CONFIRMED → CANCELLED` 也没有边」是同一套理由：资金单据过账即入账。
//!
//! # 事务边界
//!
//! 每个写入口（create / allocate / status）**自己开事务**：退款头、核销明细、应收回写、
//! 凭证四者同生共死。核销与过账都不许「一半成功」。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_receipt_refund / biz_receipt_refund_allocation 是销售台账（V0294 建表，
//!               无 dept_id / 归属人列），数据范围（SELF / DEPT_*）表达不出来；
//!               可见性由 business:erp:receipt-refund:* 权限码控制。
//! SCOPE-EXEMPT: biz_receivable 是应收台账、biz_customer 是客户主数据（全局），同样没有归属列。
//! SCOPE-EXEMPT: biz_receipt 是收款台账（SO-16 建表，无归属列），只用来校验来源收款单。
//! SCOPE-EXEMPT: biz_voucher / biz_seq 是会计台账与全局字典，没有归属列。
//! ```

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{DateTime, Datelike, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::model_base::normalize_page;
use crate::business::erp::period;
use crate::business::erp::receivable;
use crate::business::erp::voucher::{self, EntryLine, SeqSpec};
use crate::business::shared::doc_kind::VoucherSourceType;
use crate::business::shared::scope;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0295 的 sys_menu.auth_code 逐字一致） ====================

/// 列表。`business:erp:receipt-refund:list`
pub const PERM_LIST: &str = "business:erp:receipt-refund:list";
/// 详情。`business:erp:receipt-refund:detail`
pub const PERM_DETAIL: &str = "business:erp:receipt-refund:detail";
/// 登记退款。`business:erp:receipt-refund:create`
pub const PERM_CREATE: &str = "business:erp:receipt-refund:create";
/// 退款核销。`business:erp:receipt-refund:allocate`
pub const PERM_ALLOCATE: &str = "business:erp:receipt-refund:allocate";
/// 状态迁移。`business:erp:receipt-refund:status`
pub const PERM_STATUS: &str = "business:erp:receipt-refund:status";

// ==================== 常量 ====================

/// 退款单表。
pub const TABLE: &str = "biz_receipt_refund";
/// 退款核销明细表。
pub const ALLOC_TABLE: &str = "biz_receipt_refund_allocation";
/// 客户主数据表。
pub const CUSTOMER_TABLE: &str = "biz_customer";
/// 收款单表（只读校验来源收款单）。
pub const RECEIPT_TABLE: &str = "biz_receipt";

/// 状态：草稿。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：已确认（可核销）。
pub const STATUS_CONFIRMED: &str = "CONFIRMED";
/// 状态：已过账（凭证已生成，终态）。
pub const STATUS_POSTED: &str = "POSTED";
/// 状态：已作废（终态）。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 状态白名单（与 `ck_biz_receipt_refund_status` 逐字一致，单测钉住）。
pub const STATUSES: [&str; 4] = [
    STATUS_DRAFT,
    STATUS_CONFIRMED,
    STATUS_POSTED,
    STATUS_CANCELLED,
];

/// 退款方式：银行转账。
pub const METHOD_BANK_TRANSFER: &str = "BANK_TRANSFER";
/// 退款方式：现金。
pub const METHOD_CASH: &str = "CASH";
/// 退款方式：支票。
pub const METHOD_CHECK: &str = "CHECK";
/// 退款方式：其它。
pub const METHOD_OTHER: &str = "OTHER";

/// 退款方式白名单（与 `ck_biz_receipt_refund_method` 逐字一致）。
pub const METHODS: [&str; 4] = [
    METHOD_BANK_TRANSFER,
    METHOD_CASH,
    METHOD_CHECK,
    METHOD_OTHER,
];

/// 单号前缀（`RR-yyyyMMdd-000001`）。
pub const NO_PREFIX: &str = "RR";
/// `biz_seq.seq_type`（V0294 已种）。
pub const SEQ_TYPE: &str = "RR";

/// 退款单取号规则。
pub const REFUND_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: SEQ_TYPE,
    prefix: NO_PREFIX,
    seq_name: "收款退款单号",
    remark: "收款退款单号：RR-yyyyMMdd-000001，按天（CST）滚动（V0294）",
};

/// 凭证借方科目：1122 应收账款（退款 ⇒ 应收**恢复**）。
pub const DEBIT_CODE: &str = "1122";
/// 凭证贷方科目：1002 银行存款（钱出去）。
pub const CREDIT_CODE: &str = "1002";

/// 凭证来源类型（`VoucherSourceType::ReceiptRefund` 的落库值）。
pub const SOURCE_RECEIPT_REFUND: &str = VoucherSourceType::ReceiptRefund.as_str();

/// 状态机实体类型（= `DocKind::ReceiptRefund` 的落库值 `RECEIPT_REFUND`）。
pub const ENTITY: &str = state_machine::ENTITY_RECEIPT_REFUND;

/// 合法边（唯一真相在 `state_machine::RECEIPT_REFUND_EDGES`，这里只是别名）。
pub const LEGAL_EDGES: [(&str, &str); 3] = state_machine::RECEIPT_REFUND_EDGES;

/// 入账态（唯一真相在 `state_machine::RECEIPT_REFUND_LOCKED`）。
pub const LOCKED: [&str; 2] = state_machine::RECEIPT_REFUND_LOCKED;

/// 退款原因最短长度（「退货」「错了」不算可追溯依据）。
pub const REASON_MIN_CHARS: usize = 4;
/// 退款原因最长长度（`reason` 是 TEXT，这里只是不给「一大段粘贴」）。
pub const REASON_MAX_CHARS: usize = 500;

/// `biz_receipt_refund` 列清单（与 V0294 逐列对应）。
pub const COLUMNS: &str = "id, refund_no, customer_id, source_receipt_id, refund_date, amount, \
                           allocated_amount, method, bank_account, status, voucher_id, reason, \
                           remark, created_at, updated_at";

/// 分页列清单（连带客户名；别名 `c` = biz_customer）。
pub const LIST_COLUMNS: &str = "p.id, p.refund_no, p.customer_id, COALESCE(c.name, '') AS customer_name, \
                                p.source_receipt_id, p.refund_date, p.amount, p.allocated_amount, \
                                p.method, p.bank_account, p.status, p.voucher_id, p.reason, p.remark, \
                                p.created_at, p.updated_at";

/// 分页 FROM（客户左连：客户软删后历史退款单仍要能列出来）。
pub const LIST_FROM: &str =
    "FROM biz_receipt_refund p LEFT JOIN biz_customer c ON c.id = p.customer_id";

/// 分页排序（新建在前）。
pub const LIST_ORDER: &str = "ORDER BY p.created_at DESC, p.refund_no DESC";

/// 分页的表目标（未参与数据权限，见模块文档 SCOPE-EXEMPT）。
pub const LIST_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

// ==================== 纯逻辑（校验 / 金额 / 状态） ====================

/// 未冲减余额 = 退款金额 - 已冲减金额（`>= 0`，库层 `ck_biz_receipt_refund_amount` 兜底）。
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

/// 退款方式中文。
pub fn method_label(method: Option<&str>) -> &str {
    match method {
        Some(METHOD_BANK_TRANSFER) => "银行转账",
        Some(METHOD_CASH) => "现金",
        Some(METHOD_CHECK) => "支票",
        Some(METHOD_OTHER) => "其它",
        _ => "未指定",
    }
}

/// 退款金额校验：`> 0` 且不超过 `NUMERIC(18,4)` 的整数上限。
pub fn validate_amount(amount: Decimal) -> std::result::Result<Decimal, ApiError> {
    if amount <= Decimal::ZERO {
        return Err(bad_request(format!(
            "amount 必须大于 0（零元 / 负数退款没有业务含义），收到 {amount}"
        )));
    }
    if amount >= Decimal::from(10_000_000_000_000i64) {
        return Err(bad_request(format!(
            "amount 超出 NUMERIC(18,4) 上限（整数部分最多 14 位），收到 {amount}"
        )));
    }
    Ok(amount.round_dp(4))
}

/// 退款方式校验：`None` / 空串 = 未指定；其余必须是 4 值白名单。
pub fn validate_method(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if !METHODS.contains(&text) {
        return Err(bad_request(format!(
            "method 只认 {METHODS:?}（收到「{text}」）：银行转账 / 现金 / 支票之外的退款方式请用 OTHER"
        )));
    }
    Ok(Some(text.to_string()))
}

/// 退款原因校验（**建单即必填**、去空白、长度区间）。
pub fn validate_reason(raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim();
    if text.chars().count() < REASON_MIN_CHARS {
        return Err(bad_request(format!(
            "reason 至少 {REASON_MIN_CHARS} 个字（说明为什么退款：退货 / 记错账 / 合同变更…），收到「{text}」"
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

/// 会计期间：`yyyy-MM`（取退款日期所在月）。
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

/// 单条冲减的上限校验：本次冲减额 ≤ 该应收的「可退金额」。
///
/// `refundable` 由调用方算好 = `received_amount + 本退款此前已冲减它的金额`
/// （已冲减的部分已经从 `received_amount` 里扣掉，加回来才是「这笔应收一共能退多少」）。
pub fn validate_allocate_amount(
    ar_no: &str,
    amount: Decimal,
    refundable: Decimal,
) -> std::result::Result<(), ApiError> {
    if amount <= Decimal::ZERO {
        return Err(bad_request(format!(
            "冲减金额必须大于 0（应收 {ar_no}），收到 {amount}"
        )));
    }
    if amount > refundable {
        return Err(bad_request(format!(
            "本次冲减 {amount} 超过应收 {ar_no} 的可退金额 {refundable}\
             （该应收已收金额 + 本退款此前已冲减的金额）：退款只能冲已经收到的钱，请按可退金额或更小的金额退款"
        )));
    }
    Ok(())
}

/// 汇总校验：冲减总额 ≤ 退款金额。
pub fn validate_allocate_total(
    refund_no: &str,
    total: Decimal,
    amount: Decimal,
) -> std::result::Result<(), ApiError> {
    if total > amount {
        return Err(bad_request(format!(
            "冲减总额 {total} 超过退款金额 {amount}（退款单 {refund_no}）：\
             一张退款单只能退这么多钱，请减少冲减金额或分单"
        )));
    }
    Ok(())
}

// ==================== 错误出口（写路径统一收口） ====================

/// 400：入参不合法。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409：状态冲突。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 404：退款单不存在。
fn not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "收款退款单不存在或已删除".to_string(),
    )
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
            "唯一约束冲突（{}）：退款单号重复，或同一退款对同一应收已有冲减明细",
            constraint.as_deref().unwrap_or("<未知>")
        )),
        DbError::CheckViolation { constraint, .. } => bad_request(format!(
            "数据不满足库约束 {}（退款金额 > 0；0 ≤ 已冲减 ≤ 退款金额；作废时已冲减必须为 0；\
             退款原因 >= 4 字；冲减明细金额 > 0）",
            constraint.as_deref().unwrap_or("<未知>")
        )),
        DbError::InvalidValue(message) => bad_request(message),
        other => utils::db_error(other, ctx),
    }
}

// ==================== 行结构 ====================

/// 退款单行（`source_receipt_id` / `method` / `bank_account` / `voucher_id` 可空）。
#[derive(Debug, Clone)]
pub struct RefundRow {
    /// 主键。
    pub id: Uuid,
    /// 退款单号。
    pub refund_no: String,
    /// 客户。
    pub customer_id: Uuid,
    /// 来源收款单（可空）。
    pub source_receipt_id: Option<Uuid>,
    /// 退款日期。
    pub refund_date: NaiveDate,
    /// 退款金额。
    pub amount: Decimal,
    /// 已冲减金额。
    pub allocated_amount: Decimal,
    /// 退款方式。
    pub method: Option<String>,
    /// 付款账户。
    pub bank_account: Option<String>,
    /// 状态。
    pub status: String,
    /// 退款凭证。
    pub voucher_id: Option<Uuid>,
    /// 退款原因。
    pub reason: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: Option<DateTime<Utc>>,
    /// 更新时间。
    pub updated_at: Option<DateTime<Utc>>,
}

impl RefundRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            refund_no: row.get_typed("refund_no")?,
            customer_id: row.get_typed("customer_id")?,
            source_receipt_id: row.get_typed("source_receipt_id")?,
            refund_date: row.get_typed("refund_date")?,
            amount: row.get_typed("amount")?,
            allocated_amount: row.get_typed("allocated_amount")?,
            method: row.get_typed("method")?,
            bank_account: row.get_typed("bank_account")?,
            status: row.get_typed("status")?,
            voucher_id: row.get_typed("voucher_id")?,
            reason: row.get_typed("reason")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 未冲减余额。
    pub fn remaining(&self) -> Decimal {
        remaining_of(self.amount, self.allocated_amount)
    }
}

/// 冲减明细行（连带应收的关键金额，前端抽屉一次看全）。
#[derive(Debug, Clone)]
pub struct RefundAllocationRow {
    /// 主键。
    pub id: Uuid,
    /// 应收单。
    pub receivable_id: Uuid,
    /// 应收单号。
    pub ar_no: String,
    /// 应收金额。
    pub ar_amount: Decimal,
    /// 应收已收（冲减后）。
    pub ar_received: Decimal,
    /// 应收未收（冲减后）。
    pub ar_unsettled: Decimal,
    /// 应收状态（冲减后）。
    pub ar_status: String,
    /// 本次冲减金额。
    pub amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 冲减时间。
    pub created_at: Option<DateTime<Utc>>,
}

impl RefundAllocationRow {
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

/// 按主键取一张退款单（未软删）。
pub async fn find_by_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<RefundRow>> {
    // SCOPE-EXEMPT: 退款台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!("SELECT {COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"),
            vec![Value::from(&id)],
        )
        .await?;
    match row.as_ref() {
        Some(row) => Ok(Some(RefundRow::from_row(row)?)),
        None => Ok(None),
    }
}

/// 按主键锁一张退款单（`SELECT … FOR UPDATE`，核销 / 状态迁移的并发闸）。
pub async fn lock_by_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<RefundRow>> {
    // SCOPE-EXEMPT: 退款台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    match row.as_ref() {
        Some(row) => Ok(Some(RefundRow::from_row(row)?)),
        None => Ok(None),
    }
}

/// 锁一张应收单（`SELECT … FOR UPDATE`，与收款核销同口径的并发闸）。
pub async fn lock_receivable(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> DbResult<Option<receivable::ReceivableRow>> {
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
    match row.as_ref() {
        Some(row) => Ok(Some(receivable::ReceivableRow::from_row(row)?)),
        None => Ok(None),
    }
}

/// 一张退款单的冲减明细（连带应收金额与状态，按创建顺序）。
pub async fn allocations_of(
    ex: &impl SqlExecutor,
    refund_id: Uuid,
) -> DbResult<Vec<RefundAllocationRow>> {
    // SCOPE-EXEMPT: 退款台账无归属列（见模块文档）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT a.id, a.receivable_id, r.ar_no, r.amount AS ar_amount, \
                        r.received_amount AS ar_received, r.unsettled_amount AS ar_unsettled, \
                        r.status AS ar_status, a.amount, a.remark, a.created_at \
                   FROM {ALLOC_TABLE} a \
                   JOIN {} r ON r.id = a.receivable_id \
                  WHERE a.refund_id = ?::uuid \
                  ORDER BY a.created_at, a.id",
                receivable::TABLE
            ),
            vec![Value::from(&refund_id)],
        )
        .await?;
    rows.iter().map(RefundAllocationRow::from_row).collect()
}

/// 本退款单的冲减合计（**从库里 SUM**，不信内存累加）。
pub async fn allocated_sum(ex: &impl SqlExecutor, refund_id: Uuid) -> DbResult<Decimal> {
    // SCOPE-EXEMPT: 退款台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT COALESCE(SUM(amount), 0) AS total FROM {ALLOC_TABLE} WHERE refund_id = ?::uuid"
            ),
            vec![Value::from(&refund_id)],
        )
        .await?;
    match row.as_ref() {
        Some(row) => row.get_typed("total"),
        None => Ok(Decimal::ZERO),
    }
}

/// 本退款对某张应收已有的冲减金额（没有 → 0）。
async fn existing_allocation(
    ex: &impl SqlExecutor,
    refund_id: Uuid,
    receivable_id: Uuid,
) -> DbResult<Decimal> {
    // SCOPE-EXEMPT: 退款台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT amount FROM {ALLOC_TABLE} WHERE refund_id = ?::uuid AND receivable_id = ?::uuid"
            ),
            vec![Value::from(&refund_id), Value::from(&receivable_id)],
        )
        .await?;
    match row.as_ref() {
        Some(row) => row.get_typed("amount"),
        None => Ok(Decimal::ZERO),
    }
}

/// 客户是否存在且未软删。
pub async fn customer_exists(ex: &impl SqlExecutor, customer_id: Uuid) -> DbResult<bool> {
    // SCOPE-EXEMPT: 客户主数据全局（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT 1 AS hit FROM {CUSTOMER_TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![Value::from(&customer_id)],
        )
        .await?;
    Ok(row.is_some())
}

/// 客户名称（详情 / 列表兜底空串）。
pub async fn customer_name(ex: &impl SqlExecutor, customer_id: Uuid) -> DbResult<String> {
    // SCOPE-EXEMPT: 客户主数据全局（见模块文档）。
    let row = ex
        .query_first_sql(
            format!("SELECT name FROM {CUSTOMER_TABLE} WHERE id = ?::uuid"),
            vec![Value::from(&customer_id)],
        )
        .await?;
    match row.as_ref() {
        Some(row) => row.get_typed("name"),
        None => Ok(String::new()),
    }
}

/// 来源收款单的关键信息（客户 / 状态 / 金额），用于建单校验。
#[derive(Debug, Clone)]
pub struct ReceiptBrief {
    /// 收款单号。
    pub receipt_no: String,
    /// 客户。
    pub customer_id: Uuid,
    /// 收款金额。
    pub amount: Decimal,
    /// 状态。
    pub status: String,
}

/// 取一张收款单（未软删）；不存在 → `None`。
pub async fn find_receipt(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ReceiptBrief>> {
    // SCOPE-EXEMPT: 收款台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT receipt_no, customer_id, amount, status FROM {RECEIPT_TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    match row.as_ref() {
        Some(row) => Ok(Some(ReceiptBrief {
            receipt_no: row.get_typed("receipt_no")?,
            customer_id: row.get_typed("customer_id")?,
            amount: row.get_typed("amount")?,
            status: row.get_typed("status")?,
        })),
        None => Ok(None),
    }
}

/// 凭证号（详情展示；没有凭证 → `None`）。
pub async fn voucher_no_of(ex: &impl SqlExecutor, voucher_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 会计台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            "SELECT voucher_no FROM biz_voucher WHERE id = ?::uuid".to_string(),
            vec![Value::from(&voucher_id)],
        )
        .await?;
    match row.as_ref() {
        Some(row) => Ok(Some(row.get_typed("voucher_no")?)),
        None => Ok(None),
    }
}

/// 分页（全部状态；财务要对账，作废的也要看得见）。
pub async fn refund_page(q: &RefundFilter) -> DbResult<(Vec<(RefundRow, String)>, i64)> {
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
        sql.push_str(" AND (p.refund_no LIKE ? OR c.name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(from) = q.date_from {
        sql.push_str(" AND p.refund_date >= ?::text::date");
        params.push(Value::Text(from.to_string()));
    }
    if let Some(to) = q.date_to {
        sql.push_str(" AND p.refund_date <= ?::text::date");
        params.push(Value::Text(to.to_string()));
    }
    let result = scope::paginate_unscoped(
        LIST_TARGET,
        &sql,
        params,
        Some(LIST_ORDER),
        q.page,
        q.page_size,
    )
    .await?;
    let rows = result
        .list
        .iter()
        .map(|row| {
            let name: String = row.get_typed("customer_name")?;
            Ok((RefundRow::from_row(row)?, name))
        })
        .collect::<DbResult<Vec<(RefundRow, String)>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

// ==================== 入参 / 出参 ====================

/// 登记退款请求（`POST /business/erp/receipt-refund`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CreateRefundReq {
    /// 客户 id（必填）。
    pub customer_id: String,
    /// 来源收款单 id（可选，退的是哪一笔收款）。
    pub source_receipt_id: Option<String>,
    /// 退款日期（`yyyy-MM-dd`，缺省今天 CST）。
    pub refund_date: Option<String>,
    /// 退款金额（必填，> 0）。
    pub amount: Decimal,
    /// 退款方式（可选，4 值白名单）。
    pub method: Option<String>,
    /// 付款账户（可选）。
    pub bank_account: Option<String>,
    /// 退款原因（**必填**，>= 4 字）。
    pub reason: String,
    /// 备注（可选）。
    pub remark: Option<String>,
}

/// 一条冲减明细（`POST /business/erp/receipt-refund/{id}/allocate` 的 `allocations[]`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RefundAllocateLineReq {
    /// 应收单 id（必填）。
    pub receivable_id: String,
    /// 本次冲减金额（必填，> 0，且 <= 该应收已收）。
    pub amount: Decimal,
    /// 备注（可选）。
    pub remark: Option<String>,
}

/// 退款核销请求。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RefundAllocateReq {
    /// 冲减明细（至少 1 条）。
    pub allocations: Vec<RefundAllocateLineReq>,
}

/// 状态迁移请求。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RefundStatusReq {
    /// 目标状态（`CONFIRMED` / `POSTED` / `CANCELLED`）。
    pub status: String,
    /// 原因（作废必填，>= 4 字）。
    pub reason: Option<String>,
}

/// 分页查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RefundPageQuery {
    /// 页码（从 1 起）。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 状态（白名单）。
    pub status: Option<String>,
    /// 客户。
    pub customer_id: Option<String>,
    /// 退款单号 / 客户名（模糊）。
    pub refund_no: Option<String>,
    /// 退款方式（白名单）。
    pub method: Option<String>,
    /// 退款日期起。
    pub date_from: Option<String>,
    /// 退款日期止。
    pub date_to: Option<String>,
}

/// 退款单筛选（数据层形态）。
#[derive(Debug, Clone, Default)]
pub struct RefundFilter {
    /// 状态。
    pub status: Option<String>,
    /// 客户。
    pub customer_id: Option<Uuid>,
    /// 单号 / 客户名（模糊）。
    pub keyword: Option<String>,
    /// 退款方式。
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

impl RefundPageQuery {
    /// 四道入参校验（全部 400，绝不静默忽略）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(status) = self.status.as_deref().map(str::trim)
            && !status.is_empty()
            && !STATUSES.contains(&status)
        {
            return Err(bad_request(format!(
                "status 只认 {STATUSES:?}（收到「{status}」）：收款/应付的状态值在这里不合法"
            )));
        }
        if let Some(method) = self.method.as_deref().map(str::trim)
            && !method.is_empty()
            && !METHODS.contains(&method)
        {
            return Err(bad_request(format!(
                "method 只认 {METHODS:?}（收到「{method}」）"
            )));
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

    /// 转数据层筛选。
    pub fn to_filter(&self) -> std::result::Result<RefundFilter, ApiError> {
        let (page, page_size) =
            normalize_page(self.page.unwrap_or(1), self.page_size.unwrap_or(20));
        let mut filter = RefundFilter {
            page,
            page_size,
            ..RefundFilter::default()
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
        if let Some(raw) = self.refund_no.as_deref().map(str::trim)
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

/// 退款单出参（列表）。
#[derive(Debug, Clone, Serialize)]
pub struct RefundRes {
    /// 主键。
    pub id: String,
    /// 退款单号。
    pub refund_no: String,
    /// 客户。
    pub customer_id: String,
    /// 客户名称。
    pub customer_name: String,
    /// 来源收款单。
    pub source_receipt_id: Option<String>,
    /// 退款日期。
    pub refund_date: String,
    /// 退款金额。
    pub amount: Decimal,
    /// 已冲减金额。
    pub allocated_amount: Decimal,
    /// 未冲减余额（= amount - allocated_amount）。
    pub unallocated_amount: Decimal,
    /// 退款方式。
    pub method: Option<String>,
    /// 退款方式中文。
    pub method_label: String,
    /// 付款账户。
    pub bank_account: Option<String>,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 凭证 id。
    pub voucher_id: Option<String>,
    /// 退款原因。
    pub reason: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: Option<DateTime<Utc>>,
    /// 更新时间。
    pub updated_at: Option<DateTime<Utc>>,
}

impl RefundRes {
    /// 由行 + 客户名构造。
    pub fn from_row(row: &RefundRow, customer_name: &str) -> Self {
        Self {
            id: row.id.to_string(),
            refund_no: row.refund_no.clone(),
            customer_id: row.customer_id.to_string(),
            customer_name: customer_name.to_string(),
            source_receipt_id: row.source_receipt_id.map(|id| id.to_string()),
            refund_date: row.refund_date.to_string(),
            amount: row.amount,
            allocated_amount: row.allocated_amount,
            unallocated_amount: row.remaining(),
            method: row.method.clone(),
            method_label: method_label(row.method.as_deref()).to_string(),
            bank_account: row.bank_account.clone(),
            status: row.status.clone(),
            status_label: status_label(&row.status).to_string(),
            voucher_id: row.voucher_id.map(|id| id.to_string()),
            reason: row.reason.clone(),
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// 冲减明细出参。
#[derive(Debug, Clone, Serialize)]
pub struct RefundAllocationRes {
    /// 主键。
    pub id: String,
    /// 应收单。
    pub receivable_id: String,
    /// 应收单号。
    pub ar_no: String,
    /// 应收金额。
    pub ar_amount: Decimal,
    /// 应收已收（冲减后）。
    pub ar_received: Decimal,
    /// 应收未收（冲减后）。
    pub ar_unsettled: Decimal,
    /// 应收状态（冲减后）。
    pub ar_status: String,
    /// 应收状态中文。
    pub ar_status_label: String,
    /// 本次冲减金额。
    pub amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 冲减时间。
    pub created_at: Option<DateTime<Utc>>,
}

impl RefundAllocationRes {
    /// 由行构造。
    pub fn from_row(row: &RefundAllocationRow) -> Self {
        Self {
            id: row.id.to_string(),
            receivable_id: row.receivable_id.to_string(),
            ar_no: row.ar_no.clone(),
            ar_amount: row.ar_amount,
            ar_received: row.ar_received,
            ar_unsettled: row.ar_unsettled,
            ar_status: row.ar_status.clone(),
            ar_status_label: receivable::status_label(&row.ar_status).to_string(),
            amount: row.amount,
            remark: row.remark.clone(),
            created_at: row.created_at,
        }
    }
}

/// 退款单详情（头 + 冲减明细 + 凭证号）。
#[derive(Debug, Clone, Serialize)]
pub struct RefundDetailRes {
    /// 单头。
    #[serde(flatten)]
    pub header: RefundRes,
    /// 凭证号（未过账 → 空串）。
    pub voucher_no: String,
    /// 冲减明细。
    pub allocations: Vec<RefundAllocationRes>,
}

/// 详情（只读，`GET /business/erp/receipt-refund/{id}`）。
pub async fn refund_detail(id: Uuid) -> std::result::Result<RefundDetailRes, ApiError> {
    detail_with(db(), id).await
}

/// 详情（可放进事务）。
pub async fn detail_with(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> std::result::Result<RefundDetailRes, ApiError> {
    let row = find_by_id(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "查询收款退款单"))?
        .ok_or_else(not_found)?;
    let name = customer_name(ex, row.customer_id)
        .await
        .map_err(|error| utils::db_error(error, "查询客户名称"))?;
    let allocations = allocations_of(ex, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询退款核销明细"))?;
    let voucher_no = match row.voucher_id {
        Some(voucher_id) => voucher_no_of(ex, voucher_id)
            .await
            .map_err(|error| utils::db_error(error, "查询退款凭证号"))?
            .unwrap_or_default(),
        None => String::new(),
    };
    Ok(RefundDetailRes {
        header: RefundRes::from_row(&row, &name),
        voucher_no,
        allocations: allocations
            .iter()
            .map(RefundAllocationRes::from_row)
            .collect(),
    })
}

// ==================== 写：登记退款 ====================

/// 今日（CST）。
fn today_cst() -> NaiveDate {
    Utc::now()
        .with_timezone(
            &nik::prelude::chrono::FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定"),
        )
        .date_naive()
}

/// 登记一张退款单（`DRAFT`，已冲减 0），并写建单状态日志。
pub async fn create(
    operator: Option<Uuid>,
    req: CreateRefundReq,
) -> std::result::Result<RefundDetailRes, ApiError> {
    let customer_id = parse_uuid(&req.customer_id, "customer_id")?;
    let amount = validate_amount(req.amount)?;
    let reason = validate_reason(&req.reason)?;
    let method = validate_method(req.method.as_deref())?;
    let refund_date = match req.refund_date.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => parse_date_arg(raw, "refund_date")?,
        _ => today_cst(),
    };
    let period_code = period_code_of(refund_date);
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
    let source_receipt_id = match req.source_receipt_id.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(parse_uuid(raw, "source_receipt_id")?),
        _ => None,
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启退款登记事务"))?;
    let result = async {
        if !customer_exists(&tx, customer_id)
            .await
            .map_err(|error| utils::db_error(error, "校验客户"))?
        {
            return Err(ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                format!("客户 {customer_id} 不存在或已删除：退款单不能挂在不存在的客户上"),
            ));
        }
        // 来源收款单（可选）：必须是**同一个客户**、已过账、金额不超它
        if let Some(receipt_id) = source_receipt_id {
            let receipt = find_receipt(&tx, receipt_id)
                .await
                .map_err(|error| utils::db_error(error, "校验来源收款单"))?
                .ok_or_else(|| {
                    ApiError::new(
                        404,
                        error_code::RESOURCE_NOT_FOUND,
                        format!("来源收款单 {receipt_id} 不存在或已删除"),
                    )
                })?;
            if receipt.customer_id != customer_id {
                return Err(bad_request(format!(
                    "来源收款单 {} 的客户与退款单客户不一致：退款只能退这张收款所属客户的钱",
                    receipt.receipt_no
                )));
            }
            if receipt.status != "POSTED" {
                return Err(conflict(format!(
                    "来源收款单 {} 当前是「{}」：只有**已过账**的收款才有钱可退（未过账的收款作废即可）",
                    receipt.receipt_no, receipt.status
                )));
            }
            if amount > receipt.amount {
                return Err(bad_request(format!(
                    "退款金额 {amount} 超过来源收款单 {} 的收款金额 {}",
                    receipt.receipt_no, receipt.amount
                )));
            }
        }
        period::ensure_period_writable(&tx, &period_code, "登记收款退款").await?;

        let refund_no = voucher::next_doc_no(&tx, &REFUND_SEQ, now)
            .await
            .map_err(|error| utils::db_error(error, "取退款单号"))?;
        let id = Uuid::now_v7();
        tx.exec_sql(
            format!(
                "INSERT INTO {TABLE} (id, refund_no, customer_id, source_receipt_id, refund_date, \
                                     amount, allocated_amount, method, bank_account, status, reason, \
                                     remark, created_at, updated_at, create_by, update_by) \
                 VALUES (?::uuid, ?, ?::uuid, NULLIF(?::text,'')::uuid, ?::text::date, ?::text::numeric, 0, ?, ?, \
                         '{STATUS_DRAFT}', ?, ?, ?, ?, NULLIF(?::text,'')::uuid, \
                         NULLIF(?::text,'')::uuid)"
            ),
            vec![
                Value::from(&id),
                Value::Text(refund_no.clone()),
                Value::from(&customer_id),
                Value::Text(
                    source_receipt_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                ),
                Value::Text(refund_date.to_string()),
                Value::Text(amount.to_string()),
                Value::Text(method.clone().unwrap_or_default()),
                Value::Text(bank_account.clone().unwrap_or_default()),
                Value::Text(reason.clone()),
                Value::Text(remark.clone().unwrap_or_default()),
                Value::from(now),
                Value::from(now),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            ],
        )
        .await
        .map_err(|error| write_error(error, "写入退款单"))?;

        state_machine::log_created(
            &tx,
            ENTITY,
            id,
            &refund_no,
            STATUS_DRAFT,
            operator,
            Some(&reason),
        )
        .await?;

        detail_with(&tx, id).await
    }
    .await;
    match result {
        Ok(detail) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交退款登记事务"))?;
            Ok(detail)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== 写：退款核销（冲减应收已收） ====================

/// 核销计划的一行（锁完应收、算完前后金额后的形态）。
struct PlannedLine {
    receivable_id: Uuid,
    ar_no: String,
    amount: Decimal,
    previous: Decimal,
    remark: Option<String>,
    receivable: receivable::ReceivableRow,
}

/// 退款核销：把退款金额冲减到指定的应收（`received_amount -= 金额`）。
///
/// # 算法（与 `receipt::allocate` 严格镜像，方向相反）
///
/// ```text
/// ① 锁退款单（FOR UPDATE）→ 状态必须 DRAFT / CONFIRMED；期间未结账
/// ② 逐条明细：锁应收（FOR UPDATE，**按 receivable_id 排序**防死锁）
///    · 客户必须与退款单一致（退到别家的应收 = 把账坐错 → 400）
///    · 应收必须是 PARTIAL / SETTLED，或本退款此前已冲减过它（改金额时可能已回到 OPEN）
///    · 本次冲减 ≤ received_amount + 本退款此前已冲减它的金额（**已收的钱才能退**）
/// ③ 汇总校验：冲减总额 ≤ 退款金额
/// ④ 落明细：INSERT … ON CONFLICT (refund_id, receivable_id) DO UPDATE SET amount
/// ⑤ 回写应收：received_amount -= delta、unsettled_amount = amount - received，
///    状态 SETTLED→PARTIAL→OPEN 走 `transition_if_changed`（向下边，V0294 配套补进
///    `state_machine::RECEIVABLE_EDGES`）
/// ⑥ 回写退款头：allocated_amount = SUM(明细)（**从库里重新 SUM**）
/// ```
pub async fn allocate(
    operator: Option<Uuid>,
    id: &str,
    req: RefundAllocateReq,
) -> std::result::Result<RefundDetailRes, ApiError> {
    let id = parse_uuid(id, "id")?;
    if req.allocations.is_empty() {
        return Err(bad_request(
            "allocations 不能为空：退款必须指明冲减哪几张应收的已收款".to_string(),
        ));
    }
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启退款核销事务"))?;
    let result = async {
        let refund = lock_by_id(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定退款单"))?
            .ok_or_else(not_found)?;
        if LOCKED.contains(&refund.status.as_str()) {
            return Err(conflict(format!(
                "退款单 {} 当前是「{}」：已过账 / 已作废的退款单不能再核销",
                refund.refund_no,
                status_label(&refund.status)
            )));
        }
        period::ensure_period_writable(&tx, &period_code_of(refund.refund_date), "核销退款").await?;

        // ② 逐条锁应收（按 id 排序，避免与收款核销交叉死锁）
        let mut requested = req.allocations.clone();
        requested.sort_by(|a, b| a.receivable_id.cmp(&b.receivable_id));
        let mut planned: Vec<PlannedLine> = Vec::with_capacity(requested.len());
        for line in &requested {
            let receivable_id = parse_uuid(&line.receivable_id, "receivable_id")?;
            if planned
                .iter()
                .any(|item| item.receivable_id == receivable_id)
            {
                return Err(bad_request(format!(
                    "应收 {receivable_id} 在这次请求里出现了两次：同一张应收只允许一行冲减"
                )));
            }
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
            if receivable.customer_id != refund.customer_id {
                return Err(bad_request(format!(
                    "应收单 {} 的客户与退款单 {} 的客户不一致：退款只能冲本客户收到的钱",
                    receivable.ar_no, refund.refund_no
                )));
            }
            let previous = existing_allocation(&tx, refund.id, receivable_id)
                .await
                .map_err(|error| utils::db_error(error, "查询已有冲减明细"))?;
            let refundable = (receivable.received_amount + previous).round_dp(4);
            if receivable.received_amount <= Decimal::ZERO && previous <= Decimal::ZERO {
                return Err(conflict(format!(
                    "应收单 {} 当前是「{}」（已收 0）：一分没收过的应收没有可退的钱\
                     （要作废这条销售请直接作废出库单，走 OPEN → CANCELLED）",
                    receivable.ar_no,
                    receivable::status_label(&receivable.status)
                )));
            }
            validate_allocate_amount(&receivable.ar_no, line.amount, refundable)?;
            planned.push(PlannedLine {
                receivable_id,
                ar_no: receivable.ar_no.clone(),
                amount: line.amount.round_dp(4),
                previous,
                remark: line
                    .remark
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_string),
                receivable,
            });
        }

        // ③ 汇总校验
        let untouched = (refund.allocated_amount
            - planned
                .iter()
                .map(|item| item.previous)
                .fold(Decimal::ZERO, |acc, value| acc + value))
        .round_dp(4);
        let total = planned
            .iter()
            .map(|item| item.amount)
            .fold(Decimal::ZERO, |acc, value| acc + value)
            .round_dp(4);
        let projected = (untouched + total).round_dp(4);
        if projected < Decimal::ZERO {
            return Err(conflict(format!(
                "退款单 {} 的已冲减金额与冲减明细不一致（头 {} ≠ 明细合计）：库层数据漂移，请先排查",
                refund.refund_no, refund.allocated_amount
            )));
        }
        validate_allocate_total(&refund.refund_no, projected, refund.amount)?;

        // ④ 落明细 + ⑤ 回写应收
        let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();
        for item in &planned {
            // SCOPE-EXEMPT: 退款 / 应收台账无归属列（见模块文档）。
            tx.exec_sql(
                format!(
                    "INSERT INTO {ALLOC_TABLE} (refund_id, receivable_id, amount, remark, created_at, create_by) \
                     VALUES (?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, NULLIF(?::text,''), ?, \
                             NULLIF(?::text,'')::uuid) \
                     ON CONFLICT (refund_id, receivable_id) \
                     DO UPDATE SET amount = EXCLUDED.amount, remark = EXCLUDED.remark"
                ),
                vec![
                    Value::from(&refund.id),
                    Value::from(&item.receivable_id),
                    Value::Text(item.amount.to_string()),
                    Value::Text(item.remark.clone().unwrap_or_default()),
                    Value::from(now),
                    Value::Text(operator_text.clone()),
                ],
            )
            .await
            .map_err(|error| write_error(error, "落退款冲减明细"))?;

            let delta = (item.amount - item.previous).round_dp(4);
            if delta == Decimal::ZERO {
                continue;
            }
            let received = (item.receivable.received_amount - delta).round_dp(4);
            if received < Decimal::ZERO {
                return Err(bad_request(format!(
                    "本次冲减 {delta} 会让应收 {} 的已收金额变成负数（当前已收 {}）",
                    item.ar_no, item.receivable.received_amount
                )));
            }
            let unsettled = receivable::unsettled_of(item.receivable.amount, received);
            let next_status = receivable_status_of(item.receivable.amount, received);
            let reason = format!("收款退款 {} 冲减", refund.refund_no);
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
                Some((ENTITY, refund.refund_no.as_str())),
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
                                settled_at = CASE WHEN ?::text = '' THEN settled_at \
                                                  ELSE ?::text::timestamptz END, \
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
                    "应收单 {} 的已收金额在本次冲减期间被其它操作改动（并发收款 / 退款），本次核销已回滚，请重试",
                    item.ar_no
                )));
            }
        }

        // ⑥ 回写退款头：allocated_amount 以**库里的明细合计**为准
        let allocated = allocated_sum(&tx, refund.id)
            .await
            .map_err(|error| utils::db_error(error, "汇总冲减金额"))?;
        tx.exec_sql(
            format!(
                "UPDATE {TABLE} SET allocated_amount = NULLIF(?::text,'')::numeric, updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![
                Value::Text(allocated.to_string()),
                Value::from(now),
                Value::Text(operator_text),
                Value::from(&refund.id),
            ],
        )
        .await
        .map_err(|error| write_error(error, "回写退款单已冲减金额"))?;

        detail_with(&tx, refund.id).await
    }
    .await;
    match result {
        Ok(detail) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交退款核销事务"))?;
            Ok(detail)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== 写：状态迁移 ====================

/// 状态迁移（`DRAFT→CONFIRMED` / `CONFIRMED→POSTED` / `DRAFT→CANCELLED`）。
///
/// - `POSTED`：要求核销满（`allocated_amount == amount`），生成退款凭证
///   借 1122 应收账款 / 贷 1002 银行存款（各 = 退款金额），并回写 `voucher_id`；
/// - `CANCELLED`：要求一分没核销（库层 `ck_biz_receipt_refund_cancelled` 兜底），
///   原因必填（>= 4 字），并写进状态日志。
pub async fn change_status(
    operator: Option<Uuid>,
    id: &str,
    req: RefundStatusReq,
) -> std::result::Result<RefundDetailRes, ApiError> {
    let id = parse_uuid(id, "id")?;
    let target = req.status.trim().to_string();
    if !STATUSES.contains(&target.as_str()) {
        return Err(bad_request(format!(
            "status 只认 {STATUSES:?}（收到「{target}」）"
        )));
    }
    let reason = match req.reason.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(validate_reason(raw)?),
        _ => None,
    };
    let now = Utc::now();

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启退款状态迁移事务"))?;
    let result = async {
        let refund = lock_by_id(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定退款单"))?
            .ok_or_else(not_found)?;
        if refund.status == target {
            return Err(conflict(format!(
                "退款单 {} 已经是「{}」，没有状态变化",
                refund.refund_no,
                status_label(&target)
            )));
        }
        if !LEGAL_EDGES
            .iter()
            .any(|(from, to)| *from == refund.status && *to == target)
        {
            return Err(conflict(format!(
                "退款单 {} 不能从「{}」变成「{}」：合法去向是 {}",
                refund.refund_no,
                status_label(&refund.status),
                status_label(&target),
                describe_next(&refund.status)
            )));
        }
        match target.as_str() {
            STATUS_POSTED => {
                if refund.allocated_amount != refund.amount {
                    return Err(conflict(format!(
                        "退款单 {} 还有 {} 未冲减到应收（退款 {} / 已冲减 {}）：\
                         先核销满再过账，否则凭证会把还没抵哪张应收的钱借记成应收账款",
                        refund.refund_no,
                        refund.remaining(),
                        refund.amount,
                        refund.allocated_amount
                    )));
                }
                period::ensure_period_writable(
                    &tx,
                    &period_code_of(refund.refund_date),
                    "退款过账",
                )
                .await?;
            }
            STATUS_CANCELLED => {
                if refund.allocated_amount != Decimal::ZERO {
                    return Err(conflict(format!(
                        "退款单 {} 已冲减 {}，不能直接作废：作废会让应收的「已收款」凭空变回来。\
                         请先把冲减金额改成 0（删除冲减）再作废",
                        refund.refund_no, refund.allocated_amount
                    )));
                }
            }
            _ => {}
        }

        state_machine::transition(
            &tx,
            ENTITY,
            refund.id,
            &refund.refund_no,
            &LEGAL_EDGES,
            &refund.status,
            &target,
            operator,
            reason.as_deref(),
            None,
        )
        .await
        .map_err(|error| {
            write_error(
                DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)),
                "退款单状态变更",
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
                    Value::from(&refund.id),
                    Value::Text(refund.status.clone()),
                ],
            )
            .await
            .map_err(|error| write_error(error, "回写退款单状态"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "退款单 {} 的状态在本次迁移期间被其它操作改动，本次操作已回滚，请重试",
                refund.refund_no
            )));
        }

        if target == STATUS_POSTED {
            let voucher_id = post_refund_voucher(&tx, &refund, operator, now).await?;
            tx.exec_sql(
                format!(
                    "UPDATE {TABLE} SET voucher_id = ?::uuid, updated_at = ? WHERE id = ?::uuid"
                ),
                vec![
                    Value::from(&voucher_id),
                    Value::from(now),
                    Value::from(&refund.id),
                ],
            )
            .await
            .map_err(|error| write_error(error, "退款单回写凭证"))?;
        }

        detail_with(&tx, refund.id).await
    }
    .await;
    match result {
        Ok(detail) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交退款状态迁移事务"))?;
            Ok(detail)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 当前状态的合法去向（错误文案用）。
fn describe_next(status: &str) -> String {
    let next: Vec<String> = LEGAL_EDGES
        .iter()
        .filter(|(from, _)| *from == status)
        .map(|(_, to)| format!("「{}」", status_label(to)))
        .collect();
    if next.is_empty() {
        "（终态，没有去向）".to_string()
    } else {
        next.join(" / ")
    }
}

/// 生成退款凭证：**借 1122 应收账款 / 贷 1002 银行存款**（与收款凭证严格镜像）。
async fn post_refund_voucher(
    tx: &Transaction,
    refund: &RefundRow,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<Uuid, ApiError> {
    let receivable_account =
        voucher::account_required(tx, DEBIT_CODE, "退款凭证的借方科目 1122 应收账款")
            .await
            .map_err(|error| write_error(error, "解析退款凭证借方科目"))?;
    let bank = voucher::account_required(tx, CREDIT_CODE, "退款凭证的贷方科目 1002 银行存款")
        .await
        .map_err(|error| write_error(error, "解析退款凭证贷方科目"))?;
    let summary = format!("收款退款 {} 客户 {}", refund.refund_no, refund.customer_id);
    let lines = vec![
        EntryLine {
            line_no: 1,
            summary: format!("{summary} 恢复应收账款"),
            account: receivable_account,
            debit: refund.amount,
            credit: Decimal::ZERO,
            // 应收按客户挂辅助核算（与收款凭证的贷方行同口径：收款侧这里没挂客户，
            // 因为应收账款是客户维度，退款侧同样保持「科目即客户维度」的既有口径）
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        },
        EntryLine {
            line_no: 2,
            summary: format!("{summary} 银行付款"),
            account: bank,
            debit: Decimal::ZERO,
            credit: refund.amount,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            // 银行存款是公司级账户，不挂部门（与收款 / 付款凭证同口径）
            aux_dept_id: None,
        },
    ];
    voucher::post_voucher(
        tx,
        &voucher::PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_AUTO,
            source_type: SOURCE_RECEIPT_REFUND,
            source_id: Some(refund.id),
            source_no: Some(refund.refund_no.as_str()),
            voucher_date: refund.refund_date,
            summary,
            lines,
            remark: refund.remark.as_deref(),
            red_of: None,
            status: voucher::STATUS_POSTED,
        },
        operator,
        now,
    )
    .await
    .map_err(|error| write_error(error, "生成退款凭证"))
}

// ==================== 接口层 ====================

/// 登记退款。
#[nik::permission(
    path = "/business/erp/receipt-refund",
    method = "POST",
    permission = "business:erp:receipt-refund:create"
)]
#[nik::post("/business/erp/receipt-refund")]
pub async fn receipt_refund_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateRefundReq>,
) -> ResResult<RefundDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create(operator, req).await?))
}

/// 退款单详情。
#[nik::permission(
    path = "/business/erp/receipt-refund/{id}",
    method = "GET",
    permission = "business:erp:receipt-refund:detail"
)]
#[nik::get("/business/erp/receipt-refund/{id}")]
pub async fn receipt_refund_detail_handler(Path(id): Path<String>) -> ResResult<RefundDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(refund_detail(id).await?))
}

/// 退款单分页。
#[nik::permission(
    path = "/business/erp/receipt-refund/page",
    method = "GET",
    permission = "business:erp:receipt-refund:list"
)]
#[nik::get("/business/erp/receipt-refund/page")]
pub async fn receipt_refund_page_handler(
    Query(query): Query<RefundPageQuery>,
) -> ResResult<PageData<Vec<RefundRes>>> {
    query.validate()?;
    let filter = query.to_filter()?;
    let (rows, total) = refund_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询收款退款单"))?;
    let list = rows
        .iter()
        .map(|(row, name)| RefundRes::from_row(row, name))
        .collect();
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        filter.page,
        filter.page_size,
    ))
}

/// 退款核销。
#[nik::permission(
    path = "/business/erp/receipt-refund/{id}/allocate",
    method = "POST",
    permission = "business:erp:receipt-refund:allocate"
)]
#[nik::post("/business/erp/receipt-refund/{id}/allocate")]
pub async fn receipt_refund_allocate(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<RefundAllocateReq>,
) -> ResResult<RefundDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(allocate(operator, &id, req).await?))
}

/// 状态迁移（确认 / 过账 / 作废）。
#[nik::permission(
    path = "/business/erp/receipt-refund/{id}/status",
    method = "PATCH",
    permission = "business:erp:receipt-refund:status"
)]
#[nik::patch("/business/erp/receipt-refund/{id}/status")]
pub async fn receipt_refund_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<RefundStatusReq>,
) -> ResResult<RefundDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(change_status(operator, &id, req).await?))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(raw: &str) -> Decimal {
        Decimal::from_str(raw).expect("夹具金额非法")
    }

    // ---------- 纯逻辑用例（不连库） ----------

    #[test]
    fn status_and_method_vocabularies_match_the_db_constraints() {
        // 与 ck_biz_receipt_refund_status / ck_biz_receipt_refund_method 逐字一致
        assert_eq!(STATUSES, ["DRAFT", "CONFIRMED", "POSTED", "CANCELLED"]);
        assert_eq!(METHODS, ["BANK_TRANSFER", "CASH", "CHECK", "OTHER"]);
        assert_eq!(status_label(STATUS_POSTED), "已过账");
        assert_eq!(status_label("SETTLED"), "未知状态");
        assert_eq!(method_label(Some(METHOD_CASH)), "现金");
        assert_eq!(method_label(None), "未指定");
    }

    #[test]
    fn edges_are_the_registered_ones_and_posted_is_terminal() {
        // 与 state_machine 注册表逐条相等（transition 会核对，这里先钉住）
        assert_eq!(LEGAL_EDGES, state_machine::RECEIPT_REFUND_EDGES);
        assert_eq!(ENTITY, "RECEIPT_REFUND");
        assert!(LEGAL_EDGES.contains(&("DRAFT", "CONFIRMED")));
        assert!(LEGAL_EDGES.contains(&("CONFIRMED", "POSTED")));
        assert!(LEGAL_EDGES.contains(&("DRAFT", "CANCELLED")));
        // POSTED 是终态：没有任何从 POSTED 出发的边
        assert!(!LEGAL_EDGES.iter().any(|(from, _)| *from == STATUS_POSTED));
        assert!(LOCKED.contains(&STATUS_POSTED) && LOCKED.contains(&STATUS_CANCELLED));
        assert!(!LOCKED.contains(&STATUS_CONFIRMED));
    }

    #[test]
    fn voucher_source_is_registered_with_its_doc_kind() {
        assert_eq!(SOURCE_RECEIPT_REFUND, "RECEIPT_REFUND");
        assert_eq!(
            VoucherSourceType::ReceiptRefund.as_str(),
            SOURCE_RECEIPT_REFUND
        );
        assert_eq!(
            VoucherSourceType::ReceiptRefund.doc_kind(),
            Some(crate::business::shared::doc_kind::DocKind::ReceiptRefund)
        );
    }

    #[test]
    fn amount_validation_rejects_zero_and_negative() {
        assert!(validate_amount(Decimal::ZERO).is_err());
        assert!(validate_amount(dec("-1")).is_err());
        assert_eq!(validate_amount(dec("100.005")).unwrap(), dec("100.0050"));
        assert!(validate_amount(dec("10000000000000")).is_err());
    }

    #[test]
    fn reason_is_required_and_bounded() {
        // 空 / 太短 → 400（退款建单即必填，与收款单的「仅作废必填」不同）
        assert!(validate_reason("   ").is_err());
        assert!(validate_reason("退货").is_err()); // 2 个字 < 4
        assert_eq!(validate_reason(" 客户退货退款 ").unwrap(), "客户退货退款");
        assert!(validate_reason(&"长".repeat(REASON_MAX_CHARS + 1)).is_err());
    }

    #[test]
    fn method_must_be_in_whitelist() {
        assert_eq!(validate_method(None).unwrap(), None);
        assert_eq!(validate_method(Some("  ")).unwrap(), None);
        assert_eq!(
            validate_method(Some("BANK_TRANSFER")).unwrap(),
            Some("BANK_TRANSFER".to_string())
        );
        assert!(validate_method(Some("ALIPAY")).is_err());
    }

    #[test]
    fn refundable_cap_is_enforced() {
        assert!(validate_allocate_amount("AR-1", dec("100"), dec("100")).is_ok());
        assert!(validate_allocate_amount("AR-1", dec("100.0001"), dec("100")).is_err());
        assert!(validate_allocate_amount("AR-1", Decimal::ZERO, dec("100")).is_err());
        assert!(validate_allocate_total("RR-1", dec("100"), dec("100")).is_ok());
        assert!(validate_allocate_total("RR-1", dec("100.0001"), dec("100")).is_err());
    }

    #[test]
    fn receivable_status_follows_received_amount() {
        assert_eq!(receivable_status_of(dec("100"), Decimal::ZERO), "OPEN");
        assert_eq!(receivable_status_of(dec("100"), dec("40")), "PARTIAL");
        assert_eq!(receivable_status_of(dec("100"), dec("100")), "SETTLED");
        assert_eq!(remaining_of(dec("100"), dec("30")), dec("70"));
    }

    #[test]
    fn receivable_downward_edges_are_registered() {
        // 退款把应收往回推：三条向下边必须在注册表里（否则 transition 会 500）
        for edge in [
            ("SETTLED", "PARTIAL"),
            ("PARTIAL", "OPEN"),
            ("SETTLED", "OPEN"),
        ] {
            assert!(
                state_machine::RECEIVABLE_EDGES.contains(&edge),
                "应收状态机缺退款冲减边 {edge:?}"
            );
        }
        // 而「已收过钱 → 作废」这条仍然没有：CANCELLED 的语义没有被放宽
        assert!(!state_machine::RECEIVABLE_EDGES.contains(&("PARTIAL", "CANCELLED")));
        assert!(!state_machine::RECEIVABLE_EDGES.contains(&("SETTLED", "CANCELLED")));
    }

    #[test]
    fn page_query_validates_status_and_dates() {
        let bad_status = RefundPageQuery {
            status: Some("OPEN".to_string()),
            ..RefundPageQuery::default()
        };
        assert!(bad_status.validate().is_err());
        let good_status = RefundPageQuery {
            status: Some("POSTED".to_string()),
            ..RefundPageQuery::default()
        };
        assert!(good_status.validate().is_ok());
        let bad_date = RefundPageQuery {
            date_from: Some("2026-1-1".to_string()),
            ..RefundPageQuery::default()
        };
        assert!(bad_date.validate().is_err());
    }

    // ---------- 真库用例（`ERP_DB_URL` 开关；不设时 SKIP 假绿，交付前必须带库跑） ----------
    //
    // ⚠ 不设 `ERP_DB_URL` 时这些用例打印 SKIP 并 return —— libtest 记 passed。
    //   交付前显式带库跑一遍（命令记在证据 README 里）。

    /// 真库用例必须串行（共用全局 `db()` 与同一批 `RR14C-` 夹具）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    const FIX_CUSTOMER: &str = "d5950000-0000-4000-8000-000000000001";

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
                let connection = match Db::connect_with(&url, config).await {
                    Ok(connection) => connection,
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        return false;
                    }
                };
                match set_db(connection) {
                    Ok(()) => true,
                    Err(err) => {
                        println!("SKIP: set_db 失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn sql(statement: &str) {
        db().exec_sql(statement.to_string(), vec![])
            .await
            .unwrap_or_else(|error| panic!("执行失败：{error}\n{statement}"));
    }

    async fn scalar(statement: &str) -> String {
        // 把任意 `SELECT …` 包成标量子查询再转文本：一行一列，省掉每处起别名。
        let sql = format!("SELECT ({statement})::text AS value");
        let row = db()
            .query_first_sql(sql.clone(), vec![])
            .await
            .unwrap_or_else(|error| panic!("查询失败：{error}\n{sql}"));
        match row {
            Some(row) => row.get_typed::<String>("value").unwrap_or_default(),
            None => String::new(),
        }
    }

    /// 清场 + 夹具：一个客户 + 一张已收 100 的 PARTIAL 应收（1000 / 已收 100）。
    async fn setup() -> (Uuid, Uuid) {
        sql("DELETE FROM biz_receipt_refund_allocation").await;
        sql("DELETE FROM biz_receipt_refund").await;
        sql("DELETE FROM biz_status_log WHERE entity_type IN ('RECEIPT_REFUND','RECEIVABLE')")
            .await;
        sql("DELETE FROM biz_voucher WHERE source_type = 'RECEIPT_REFUND'").await;
        sql("DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-RR14C-%'").await;
        sql("DELETE FROM biz_customer WHERE code LIKE 'RR14C-%'").await;
        sql(&format!(
            "INSERT INTO biz_customer (id, code, name, credit_days, credit_limit, status) \
             VALUES ('{FIX_CUSTOMER}'::uuid, 'RR14C-CUST', 'RR14C 退款演示客户', 30, 0, 'ACTIVE')"
        ))
        .await;
        let ar_id = Uuid::now_v7();
        sql(&format!(
            "INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no, \
                                        amount, received_amount, unsettled_amount, credit_days, \
                                        period_code, due_date, status) \
             VALUES ('{ar_id}'::uuid, 'AR-RR14C-000001', '{FIX_CUSTOMER}'::uuid, 'MANUAL', \
                     gen_random_uuid(), 'MANUAL', 1000, 100, 900, 30, '2026-11', '2026-12-06', 'PARTIAL')"
        ))
        .await;
        (
            Uuid::parse_str(FIX_CUSTOMER).expect("夹具 UUID 非法"),
            ar_id,
        )
    }

    fn sample_req(customer: Uuid) -> CreateRefundReq {
        CreateRefundReq {
            customer_id: customer.to_string(),
            source_receipt_id: None,
            refund_date: Some("2026-11-06".to_string()),
            amount: dec("100"),
            method: Some("BANK_TRANSFER".to_string()),
            bank_account: Some("6222 0000 1111 2222".to_string()),
            reason: "客户退货，货款原路退回".to_string(),
            remark: Some("真库用例".to_string()),
        }
    }

    /// 主链：登记 → 核销（应收 PARTIAL 100 → OPEN 0）→ 确认 → 过账（生成凭证）→ 作废出库单前置条件成立。
    #[tokio::test]
    async fn refund_restores_receivable_to_open_and_posts_voucher() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let (customer, ar_id) = setup().await;

        // ① 登记
        let created = create(None, sample_req(customer))
            .await
            .expect("登记退款应成功");
        assert_eq!(created.header.status, STATUS_DRAFT);
        assert!(
            created.header.refund_no.starts_with("RR-"),
            "单号应带 RR- 前缀"
        );
        assert_eq!(created.header.allocated_amount, Decimal::ZERO);
        assert_eq!(created.header.reason, "客户退货，货款原路退回");
        let refund_id = Uuid::parse_str(&created.header.id).expect("id 非法");

        // ② 核销 100 → 应收回到 OPEN
        let allocated = allocate(
            None,
            &refund_id.to_string(),
            RefundAllocateReq {
                allocations: vec![RefundAllocateLineReq {
                    receivable_id: ar_id.to_string(),
                    amount: dec("100"),
                    remark: Some("冲减退货应收的已收款".to_string()),
                }],
            },
        )
        .await
        .expect("核销应成功");
        assert_eq!(allocated.header.allocated_amount, dec("100"));
        assert_eq!(allocated.allocations.len(), 1);
        assert_eq!(allocated.allocations[0].ar_status, "OPEN");
        assert_eq!(
            scalar(&format!(
                "SELECT status || '|' || received_amount::text || '|' || unsettled_amount::text \
                   FROM biz_receivable WHERE id = '{ar_id}'::uuid"
            ))
            .await,
            "OPEN|0.0000|1000.0000"
        );

        // ③ 确认 → 过账
        let confirmed = change_status(
            None,
            &refund_id.to_string(),
            RefundStatusReq {
                status: "CONFIRMED".to_string(),
                reason: None,
            },
        )
        .await
        .expect("确认应成功");
        assert_eq!(confirmed.header.status, STATUS_CONFIRMED);
        let posted = change_status(
            None,
            &refund_id.to_string(),
            RefundStatusReq {
                status: "POSTED".to_string(),
                reason: None,
            },
        )
        .await
        .expect("过账应成功");
        assert_eq!(posted.header.status, STATUS_POSTED);
        assert!(!posted.voucher_no.is_empty(), "过账必须生成凭证号");
        // 凭证方向：借 1122 / 贷 1002，各 100
        assert_eq!(
            scalar(&format!(
                "SELECT count(*) FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
                  WHERE v.source_type = 'RECEIPT_REFUND' AND v.source_id = '{refund_id}'::uuid \
                    AND e.account_code = '1122' AND e.debit_amount = 100 AND e.credit_amount = 0"
            ))
            .await,
            "1"
        );
        assert_eq!(
            scalar(&format!(
                "SELECT count(*) FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
                  WHERE v.source_type = 'RECEIPT_REFUND' AND v.source_id = '{refund_id}'::uuid \
                    AND e.account_code = '1002' AND e.credit_amount = 100 AND e.debit_amount = 0"
            ))
            .await,
            "1"
        );

        // ④ 已过账是终态：再作废 409
        let blocked = change_status(
            None,
            &refund_id.to_string(),
            RefundStatusReq {
                status: "CANCELLED".to_string(),
                reason: Some("过账后不该能作废".to_string()),
            },
        )
        .await;
        assert!(blocked.is_err(), "POSTED 后作废必须被拒");

        // ⑤ 状态日志：DRAFT→CONFIRMED→POSTED 各一条 + 建单日志
        assert_eq!(
            scalar(&format!(
                "SELECT count(*) FROM biz_status_log WHERE entity_type = 'RECEIPT_REFUND' \
                    AND entity_id = '{refund_id}'::uuid"
            ))
            .await,
            "3"
        );
    }

    /// 边界：冲减超过已收 → 400；核销到别家客户的应收 → 400；未核销满不许过账 → 409。
    #[tokio::test]
    async fn refund_rejects_over_refund_and_cross_customer() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let (customer, ar_id) = setup().await;
        let created = create(None, sample_req(customer))
            .await
            .expect("登记退款应成功");
        let refund_id = created.header.id.clone();

        // 冲减 100.0001 > 已收 100 → 400
        let over = allocate(
            None,
            &refund_id,
            RefundAllocateReq {
                allocations: vec![RefundAllocateLineReq {
                    receivable_id: ar_id.to_string(),
                    amount: dec("100.0001"),
                    remark: None,
                }],
            },
        )
        .await;
        let err = over.expect_err("超冲减必须被拒");
        assert_eq!(err.code, error_code::PARAM_INVALID);

        // 别家客户的应收 → 400
        let other_customer = Uuid::now_v7();
        sql(&format!(
            "INSERT INTO biz_customer (id, code, name, credit_days, credit_limit, status) \
             VALUES ('{other_customer}'::uuid, 'RR14C-OTHER', 'RR14C 别家客户', 30, 0, 'ACTIVE')"
        ))
        .await;
        let other_ar = Uuid::now_v7();
        sql(&format!(
            "INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no, \
                                        amount, received_amount, unsettled_amount, credit_days, \
                                        period_code, due_date, status) \
             VALUES ('{other_ar}'::uuid, 'AR-RR14C-000002', '{other_customer}'::uuid, 'MANUAL', \
                     gen_random_uuid(), 'MANUAL', 500, 200, 300, 30, '2026-11', '2026-12-06', 'PARTIAL')"
        ))
        .await;
        let cross = allocate(
            None,
            &refund_id,
            RefundAllocateReq {
                allocations: vec![RefundAllocateLineReq {
                    receivable_id: other_ar.to_string(),
                    amount: dec("100"),
                    remark: None,
                }],
            },
        )
        .await;
        let err = cross.expect_err("跨客户冲减必须被拒");
        assert_eq!(err.code, error_code::PARAM_INVALID);

        // 一分没收的应收 → 409（走 OPEN → CANCELLED 那条路，不是退款）
        let open_ar = Uuid::now_v7();
        sql(&format!(
            "INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no, \
                                        amount, received_amount, unsettled_amount, credit_days, \
                                        period_code, due_date, status) \
             VALUES ('{open_ar}'::uuid, 'AR-RR14C-000003', '{customer}'::uuid, 'MANUAL', \
                     gen_random_uuid(), 'MANUAL', 500, 0, 500, 30, '2026-11', '2026-12-06', 'OPEN')"
        ))
        .await;
        let nothing = allocate(
            None,
            &refund_id,
            RefundAllocateReq {
                allocations: vec![RefundAllocateLineReq {
                    receivable_id: open_ar.to_string(),
                    amount: dec("100"),
                    remark: None,
                }],
            },
        )
        .await;
        let err = nothing.expect_err("一分没收的应收不能退款");
        assert_eq!(err.code, error_code::RESOURCE_CONFLICT);

        // 未核销满 → 过账 409（先确认可以，过账不行）
        change_status(
            None,
            &refund_id,
            RefundStatusReq {
                status: "CONFIRMED".to_string(),
                reason: None,
            },
        )
        .await
        .expect("确认应成功");
        let not_full = change_status(
            None,
            &refund_id,
            RefundStatusReq {
                status: "POSTED".to_string(),
                reason: None,
            },
        )
        .await;
        let err = not_full.expect_err("未核销满不许过账");
        assert_eq!(err.code, error_code::RESOURCE_CONFLICT);

        sql("DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-RR14C-%'").await;
        sql("DELETE FROM biz_customer WHERE code LIKE 'RR14C-%'").await;
        sql("DELETE FROM biz_receipt_refund_allocation").await;
        sql("DELETE FROM biz_receipt_refund").await;
    }
}
