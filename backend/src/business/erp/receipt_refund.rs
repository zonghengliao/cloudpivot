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
//! # 审批前置（SO-14 遗留 · D10，卡 t_b9840c8d）
//!
//! ```text
//! CONFIRMED ──POST /submit-approval──▶ 发起 receipt_refund 审批实例（退款单状态不变）
//!                │                        ├─ 审批通过 ──▶ 回调**直接过账**（本模块 post_after_approval）
//!                │                        └─ 驳回/撤回 ──▶ 仍是 CONFIRMED（账一分没动，可改完再提）
//!                └─ 没配流程 ──▶ 409 + 66033；持 business:erp:skip-approval + 理由 ≥10 字可显式跳过
//! CONFIRMED ──PATCH status=POSTED──▶ **必须先有 APPROVED 实例**，否则 409 + 66044
//! ```
//!
//! **审批不是一个状态**：`biz_receipt_refund.status` 的四个值语义没变
//! （`ck_biz_receipt_refund_status` 也没动）。审批是「钱能不能动」的前置条件，
//! 与凭证红冲审批同思路。因此前端判断「已提交待批」要读详情里的 `approval` 段
//! （最近一条实例的号 / 状态 / 待办数），**不能只看 status**。
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

use crate::business::erp::credit;
use crate::business::erp::model_base::normalize_page;
use crate::business::erp::period;
use crate::business::erp::receivable;
use crate::business::erp::voucher::{self, EntryLine, SeqSpec};
use crate::business::erp::{approval_gate, receipt_refund_bridge};
use crate::business::flow::{engine, model as flow_model};
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
pub const STATUSES: [&str; 4] = [STATUS_DRAFT, STATUS_CONFIRMED, STATUS_POSTED, STATUS_CANCELLED];

/// 退款方式：银行转账。
pub const METHOD_BANK_TRANSFER: &str = "BANK_TRANSFER";
/// 退款方式：现金。
pub const METHOD_CASH: &str = "CASH";
/// 退款方式：支票。
pub const METHOD_CHECK: &str = "CHECK";
/// 退款方式：其它。
pub const METHOD_OTHER: &str = "OTHER";

/// 退款方式白名单（与 `ck_biz_receipt_refund_method` 逐字一致）。
pub const METHODS: [&str; 4] = [METHOD_BANK_TRANSFER, METHOD_CASH, METHOD_CHECK, METHOD_OTHER];

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

/// `biz_receipt_refund` 列清单（与 V0294 逐列对应；`approval_instance_id` 由 V0361 补）。
pub const COLUMNS: &str = "id, refund_no, customer_id, source_receipt_id, refund_date, amount, \
                           allocated_amount, method, bank_account, status, voucher_id, reason, \
                           remark, approval_instance_id, created_at, updated_at, create_by";

/// 分页列清单（连带客户名 + **审批状态**；别名 `c` = biz_customer，`fi` = biz_flow_instance）。
///
/// **必须覆盖 [`RefundRow::from_row`] 读的每一列**：分页与详情共用同一个 Row 映射，
/// 少一列就是整页 500（E2E-02 实测：漏 `create_by` → `GET /receipt-refund/page` 报
/// `column create_by not found in row`）。单测 `list_columns_cover_row_fields` 钉住。
///
/// `approval_status` / `approval_instance_no` **只在本查询里读**（由 [`LIST_FROM`] 的
/// LEFT JOIN 带出），不进 `RefundRow` —— 详情路径另有 `latest_approval_of` 现查，
/// 两边共用的行映射不该依赖「某一条 SQL 恰好有这列」（E2E-FIX-8 ②）。
pub const LIST_COLUMNS: &str = "p.id, p.refund_no, p.customer_id, COALESCE(c.name, '') AS customer_name, \
                                p.source_receipt_id, p.refund_date, p.amount, p.allocated_amount, \
                                p.method, p.bank_account, p.status, p.voucher_id, p.reason, p.remark, \
                                p.approval_instance_id, fi.instance_no AS approval_instance_no, \
                                fi.status AS approval_status, \
                                p.created_at, p.updated_at, p.create_by";

/// 分页 FROM（客户左连：客户软删后历史退款单仍要能列出来；
/// 审批实例左连：一个 JOIN 带出审批状态，避免前端逐行回查详情）。
pub const LIST_FROM: &str = "FROM biz_receipt_refund p \
                             LEFT JOIN biz_customer c ON c.id = p.customer_id \
                             LEFT JOIN biz_flow_instance fi ON fi.id = p.approval_instance_id \
                                                          AND fi.deleted_at IS NULL";

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

/// 审批状态中文（前端列表 / 详情 tag 的唯一来源）。
///
/// 五个值与流程引擎 `biz_flow_instance.status` 逐字一致（与 `payable_payment::approval_status_label`
/// 同口径）。返回值给中文，`None`（从未提交）由调用方处理，**不在这里编造「无审批」**。
pub fn approval_status_label(status: &str) -> &str {
    match status {
        "PENDING" => "审批中",
        "APPROVED" => "已通过",
        "REJECTED" => "已驳回",
        "WITHDRAWN" => "已撤回",
        "CANCELLED" => "已终止",
        other => other,
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
pub fn validate_allocate_total(refund_no: &str, total: Decimal, amount: Decimal) -> std::result::Result<(), ApiError> {
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
    /// 最近一次提交的审批实例（弱引用 `biz_flow_instance.id`；`None` = 从未提交过审批）。
    ///
    /// 审批**不是单据状态**（`status` 仍是四个值），这一列只是「最近提的那一轮」的指针，
    /// 让列表页能一条 SQL 带出审批状态（V0361 / E2E-FIX-8 ②）。
    pub approval_instance_id: Option<Uuid>,
    /// 退款原因。
    pub reason: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: Option<DateTime<Utc>>,
    /// 更新时间。
    pub updated_at: Option<DateTime<Utc>>,
    /// 制单人（审批内控「制单人 ≠ 审批人」用的就是它）。
    pub create_by: Option<Uuid>,
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
            approval_instance_id: row.get_typed("approval_instance_id")?,
            reason: row.get_typed("reason")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
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
            format!("SELECT {COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"),
            vec![Value::from(&id)],
        )
        .await?;
    match row.as_ref() {
        Some(row) => Ok(Some(RefundRow::from_row(row)?)),
        None => Ok(None),
    }
}

/// 锁一张应收单（`SELECT … FOR UPDATE`，与收款核销同口径的并发闸）。
pub async fn lock_receivable(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<receivable::ReceivableRow>> {
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
pub async fn allocations_of(ex: &impl SqlExecutor, refund_id: Uuid) -> DbResult<Vec<RefundAllocationRow>> {
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
            format!("SELECT COALESCE(SUM(amount), 0) AS total FROM {ALLOC_TABLE} WHERE refund_id = ?::uuid"),
            vec![Value::from(&refund_id)],
        )
        .await?;
    match row.as_ref() {
        Some(row) => row.get_typed("total"),
        None => Ok(Decimal::ZERO),
    }
}

/// 本退款对某张应收已有的冲减金额（没有 → 0）。
async fn existing_allocation(ex: &impl SqlExecutor, refund_id: Uuid, receivable_id: Uuid) -> DbResult<Decimal> {
    // SCOPE-EXEMPT: 退款台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!("SELECT amount FROM {ALLOC_TABLE} WHERE refund_id = ?::uuid AND receivable_id = ?::uuid"),
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
            format!("SELECT 1 AS hit FROM {CUSTOMER_TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"),
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

/// 分页行（头 + 客户名 + **审批状态**）。
///
/// 审批两列只由列表 SQL 的 LEFT JOIN 带出，所以不进 [`RefundRow`]（详情路径没有这两列，
/// 共用行映射会把详情打 500）。把它们单独拎出来，映射点就只有一处。
#[derive(Debug, Clone)]
pub struct RefundListRow {
    /// 单头。
    pub row: RefundRow,
    /// 客户名称。
    pub customer_name: String,
    /// 审批单号（`FLW-…`；从未提交 / 实例被清理 → `None`）。
    pub approval_instance_no: Option<String>,
    /// 审批状态（`PENDING` / `APPROVED` / …；从未提交 → `None`）。
    pub approval_status: Option<String>,
}

impl RefundListRow {
    /// 转出参。
    pub fn into_res(self) -> RefundRes {
        RefundRes::from_row(&self.row, &self.customer_name)
            .with_approval_columns(self.approval_instance_no, self.approval_status)
    }
}

/// 分页（全部状态；财务要对账，作废的也要看得见）。
pub async fn refund_page(q: &RefundFilter) -> DbResult<(Vec<RefundListRow>, i64)> {
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
    let result = scope::paginate_unscoped(LIST_TARGET, &sql, params, Some(LIST_ORDER), q.page, q.page_size).await?;
    let rows = result
        .list
        .iter()
        .map(|row| {
            let name: String = row.get_typed("customer_name")?;
            Ok(RefundListRow {
                row: RefundRow::from_row(row)?,
                customer_name: name,
                approval_instance_no: row.get_typed("approval_instance_no")?,
                approval_status: row.get_typed("approval_status")?,
            })
        })
        .collect::<DbResult<Vec<RefundListRow>>>()?;
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

/// 提交审批请求（`POST /business/erp/receipt-refund/{id}/submit-approval`）。
///
/// `skip_approval` / `skip_reason` 是内控例外片段（与采购 / 收货 / 付款同款）：
/// **只有**「库里没有已发布的 receipt_refund 流程」时才用得上，
/// 需要 `business:erp:skip-approval` 权限码 + 理由 ≥ 10 字，理由写进 `biz_status_log`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RefundSubmitReq {
    /// 提交说明（写进状态日志；跳过审批时与跳过理由合并）。
    pub reason: Option<String>,
    /// 内控例外片段（`skip_approval` / `skip_reason`）。
    #[serde(flatten)]
    pub approval: approval_gate::SkipApprovalReq,
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
            return Err(bad_request(format!("method 只认 {METHODS:?}（收到「{method}」）")));
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
        let (page, page_size) = normalize_page(self.page.unwrap_or(1), self.page_size.unwrap_or(20));
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
    /// 最近一次提交的审批实例 id（`null` = 从未提交过审批）。
    pub approval_instance_id: Option<String>,
    /// 审批单号（`FLW-yyyyMMdd-XXXXXXXX`；实例被清理 / 从未提交时为 `null`）。
    pub approval_instance_no: Option<String>,
    /// 审批状态（`PENDING` / `APPROVED` / `REJECTED` / `WITHDRAWN` / `CANCELLED`；
    /// `null` = 从未提交过审批）。**列表页直接用这个字段显示审批状态**，不必回查详情。
    pub approval_status: Option<String>,
    /// 审批状态中文（与 `approval_status` 一一对应；`null` = 从未提交过审批）。
    pub approval_status_label: Option<String>,
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
            // 审批三段默认空：列表由 `refund_page` 用 JOIN 出来的两列补，
            // 详情由 `detail_with` 用 `latest_approval_of` 补（两条路都不做额外查询）
            approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
            approval_instance_no: None,
            approval_status: None,
            approval_status_label: None,
            reason: row.reason.clone(),
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }

    /// 用**列表 SQL JOIN 出来的两列**补审批状态（`refund_page` 用；不产生额外查询）。
    ///
    /// `instance_no` / `status` 都来自 `LEFT JOIN biz_flow_instance fi`：实例被清理或
    /// 从未提交时两列都是 `None` → 前端显示「未提交审批」。
    pub fn with_approval_columns(mut self, instance_no: Option<String>, status: Option<String>) -> Self {
        self.approval_status_label = status.as_deref().map(approval_status_label).map(str::to_string);
        self.approval_status = status;
        self.approval_instance_no = instance_no;
        self
    }

    /// 用审批桥的「最近一条实例」摘要补审批状态（详情路径用；不产生额外查询）。
    pub fn with_approval_brief(mut self, brief: Option<&receipt_refund_bridge::ApprovalBrief>) -> Self {
        match brief {
            None => {
                // 没提交过（或实例被清理）：三段归零，别留半截旧值
                self.approval_instance_id = None;
                self.approval_instance_no = None;
                self.approval_status = None;
                self.approval_status_label = None;
            }
            Some(brief) => {
                self.approval_instance_id = Some(brief.instance_id.to_string());
                self.approval_instance_no = Some(brief.instance_no.clone());
                self.approval_status = Some(brief.status.clone());
                self.approval_status_label = Some(approval_status_label(&brief.status).to_string());
            }
        }
        self
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

/// 退款单的审批进度摘要（发过几轮就有几行实例，这里只给**最近一条**）。
///
/// 为什么放在详情里：审批不是一个状态（`status` 仍是 `CONFIRMED`），前端要判断
/// 「已提交待批 / 被驳回过 / 从没提过」只能看这一段。
#[derive(Debug, Clone, Serialize)]
pub struct RefundApprovalRes {
    /// 审批实例 id。
    pub instance_id: String,
    /// 审批单号（引擎口径 `FLW-yyyyMMdd-XXXXXXXX`）。
    pub instance_no: String,
    /// 实例状态：`PENDING` 审批中 / `APPROVED` 已通过 / `REJECTED` 驳回 /
    /// `WITHDRAWN` 撤回 / `CANCELLED` 终止。
    pub status: String,
    /// 还在等几个人批（非 `PENDING` 恒为 0）。
    pub pending_tasks: i64,
}

impl RefundApprovalRes {
    /// 由审批桥的摘要构造。
    fn from_brief(brief: &receipt_refund_bridge::ApprovalBrief) -> Self {
        Self {
            instance_id: brief.instance_id.to_string(),
            instance_no: brief.instance_no.clone(),
            status: brief.status.clone(),
            pending_tasks: brief.pending_tasks,
        }
    }
}

/// 退款单详情（头 + 冲减明细 + 凭证号 + 审批进度）。
#[derive(Debug, Clone, Serialize)]
pub struct RefundDetailRes {
    /// 单头。
    #[serde(flatten)]
    pub header: RefundRes,
    /// 凭证号（未过账 → 空串）。
    pub voucher_no: String,
    /// 冲减明细。
    pub allocations: Vec<RefundAllocationRes>,
    /// 最近一条审批实例（从没提交过审批 → `None`）。
    pub approval: Option<RefundApprovalRes>,
}

/// 详情（只读，`GET /business/erp/receipt-refund/{id}`）。
pub async fn refund_detail(id: Uuid) -> std::result::Result<RefundDetailRes, ApiError> {
    detail_with(db(), id).await
}

/// 详情（可放进事务）。
pub async fn detail_with(ex: &impl SqlExecutor, id: Uuid) -> std::result::Result<RefundDetailRes, ApiError> {
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
    let approval = receipt_refund_bridge::latest_approval_of(ex, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询退款审批实例"))?;
    // 单头也带上审批三段（列表 / 详情字段集一致：前端一套渲染逻辑）
    let header = RefundRes::from_row(&row, &name).with_approval_brief(approval.as_ref());
    Ok(RefundDetailRes {
        header,
        voucher_no,
        allocations: allocations.iter().map(RefundAllocationRes::from_row).collect(),
        approval: approval.as_ref().map(RefundApprovalRes::from_brief),
    })
}

// ==================== 写：登记退款 ====================

/// 今日（CST）。
fn today_cst() -> NaiveDate {
    Utc::now()
        .with_timezone(&nik::prelude::chrono::FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定"))
        .date_naive()
}

/// 登记一张退款单（`DRAFT`，已冲减 0），并写建单状态日志。
pub async fn create(operator: Option<Uuid>, req: CreateRefundReq) -> std::result::Result<RefundDetailRes, ApiError> {
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
                Value::Text(source_receipt_id.map(|v| v.to_string()).unwrap_or_default()),
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

        state_machine::log_created(&tx, ENTITY, id, &refund_no, STATUS_DRAFT, operator, Some(&reason)).await?;

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
            if planned.iter().any(|item| item.receivable_id == receivable_id) {
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

        // ⑥ SO-38：退款核销 = 额度释放出口 ② 的反向边（钱退了，敞口又回来了）。
        // 与冲减同事务；一张退款单同样只冲同一客户的应收。
        if !planned.is_empty() {
            credit::sync(&tx, refund.customer_id, credit::CAUSE_AR_REVERSED, &refund.refund_no)
                .await
                .map_err(|error| utils::db_error(error, "同步客户信用额度（收款退款）"))?;
        }

        // ⑦ 回写退款头：allocated_amount 以**库里的明细合计**为准
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
/// - `POSTED`：**必须有已通过的审批实例**（`biz_flow_instance(biz_type='RECEIPT_REFUND',
///   biz_id=本单, status='APPROVED')`，否则 409 + `66044`；唯一例外是
///   [`change_status_with`] 的 `approval_override`（显式跳过审批的理由，只有
///   `submit_approval` 的 skip 分支会传），再要求核销满
///   （`allocated_amount == amount`），生成退款凭证 借 1122 应收账款 / 贷 1002 银行存款
///   （各 = 退款金额），并回写 `voucher_id`；
/// - `CANCELLED`：要求一分没核销（库层 `ck_biz_receipt_refund_cancelled` 兜底），
///   原因必填（>= 4 字），并写进状态日志。
///
/// 所有 HTTP 入口走本函数（`approval_override = None`）—— 换 URL 绕不过审批闸。
pub async fn change_status(
    operator: Option<Uuid>,
    id: &str,
    req: RefundStatusReq,
) -> std::result::Result<RefundDetailRes, ApiError> {
    change_status_with(operator, id, req, None).await
}

/// [`change_status`] 的实现体。
///
/// `approval_override` 只给「没配流程 + 持 `business:erp:skip-approval` +
/// 理由 ≥ 10 字」那条显式跳过的分支用（理由会并进 `biz_status_log`）。
/// 它是**内部参数**，不在 HTTP 请求体里 —— 任何从接口进来的调用都传 `None`。
async fn change_status_with(
    operator: Option<Uuid>,
    id: &str,
    req: RefundStatusReq,
    approval_override: Option<String>,
) -> std::result::Result<RefundDetailRes, ApiError> {
    let id = parse_uuid(id, "id")?;
    let target = req.status.trim().to_string();
    if !STATUSES.contains(&target.as_str()) {
        return Err(bad_request(format!("status 只认 {STATUSES:?}（收到「{target}」）")));
    }
    let mut reason = match req.reason.as_deref() {
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
                // ★ 审批前置（SO-14 遗留 · D10）：没有「已通过」的审批实例就不许过账。
                //   审批通过的回调走同一条路径（那时实例已是 APPROVED），所以这里只认
                //   实例状态、不认「谁在调」—— 人工过账与回调过账一个口径。
                let approved = receipt_refund_bridge::approved_instance_of(&tx, refund.id)
                    .await
                    .map_err(|error| utils::db_error(error, "查询退款审批实例"))?;
                match (approved, approval_override.as_deref()) {
                    (Some(instance), _) => {
                        // 人工过账（回调失败后的补记）时也把审批单号写进状态日志
                        if reason.is_none() {
                            reason = Some(format!("审批通过（{}）过账", instance.instance_no));
                        }
                    }
                    (None, Some(skip_reason)) => {
                        reason = Some(approval_gate::merge_reason(reason.as_deref(), skip_reason));
                    }
                    (None, None) => {
                        return Err(ApiError::new(
                            409,
                            error_code::ERP_RECEIPT_REFUND_APPROVAL_REQUIRED,
                            format!(
                                "退款单 {} 还没通过审批，不能过账：先调 POST /business/erp/receipt-refund/{}/submit-approval \
                                 发起「收款退款审批」，审批通过后系统会**自动过账**（不需要再手工点一次）。\
                                 若本环境没有已发布的 receipt_refund 流程：让管理员在「流程定义」里发布，\
                                 或由持 business:erp:skip-approval 的账号在提交审批时写明理由（≥10 字）显式跳过",
                                refund.refund_no, refund.id
                            ),
                        ));
                    }
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

// ==================== 写：审批（SO-14 遗留 · D10） ====================

/// 审批回调过账的结果（回调据此决定日志口径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostOutcome {
    /// 本次真的过账了（生成凭证）。
    Posted,
    /// 之前已经过账（重复回调）→ 幂等，不再记账。
    AlreadyPosted,
    /// 当前状态不是「已确认」，过不了账（被别的路径改过）→ 交人工处置。
    NotPostable {
        /// 当前状态。
        status: String,
    },
}

/// **审批通过后的过账执行体**（由 `receipt_refund_bridge::on_refund_settled` 调）。
///
/// 与人工 `PATCH status=POSTED` 走**同一条代码路径**（[`change_status`]：核销满校验 →
/// 过账闸 → 凭证），不另写一套记账逻辑 —— 两套写法迟早会在凭证科目或幂等上走岔。
/// 此刻审批实例已是 `APPROVED`，所以过账闸对它是放行的。
pub async fn post_after_approval(
    refund_id: Uuid,
    operator: Option<Uuid>,
    reason: &str,
) -> std::result::Result<PostOutcome, ApiError> {
    let Some(row) = find_by_id(db(), refund_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收款退款单"))?
    else {
        return Err(not_found());
    };
    match row.status.as_str() {
        STATUS_POSTED => return Ok(PostOutcome::AlreadyPosted),
        STATUS_CONFIRMED => {}
        other => {
            return Ok(PostOutcome::NotPostable {
                status: other.to_string(),
            });
        }
    }
    change_status(
        operator,
        &refund_id.to_string(),
        RefundStatusReq {
            status: STATUS_POSTED.to_string(),
            reason: Some(reason.to_string()),
        },
    )
    .await?;
    Ok(PostOutcome::Posted)
}

/// 提交审批（`POST /business/erp/receipt-refund/{id}/submit-approval` 的唯一入口）。
///
/// # 前置（缺一条就 409，报文点名缺什么）
///
/// 1. 状态必须是 `CONFIRMED`（草稿没确认就送审 = 财务还没认这笔退款）；
/// 2. 必须核销满（`allocated_amount == amount`）—— 审批通过后回调**直接过账**，
///    核销不满的单子批了也过不了账，那才是真正的坑；
/// 3. 同一张单不能有两轮在跑的审批（`biz_instance_running` + 库侧
///    `uk_biz_flow_instance_biz_active` 双保险）。
///
/// # 没配流程
///
/// `approved_refund_flow == None` → `approval_gate::authorize_missing_flow`：
/// 默认 409 + `66033`（fail-closed），只有持 `business:erp:skip-approval` 且理由
/// ≥ 10 字的账号才能显式跳过 —— 此时**直接过账**（理由写进状态日志），
/// 否则退款单会永远停在「已确认」且没人能记账。
pub async fn submit_approval(
    operator: Option<Uuid>,
    id_raw: &str,
    note: Option<&str>,
    gate: &approval_gate::ApprovalGate<'_>,
) -> std::result::Result<RefundDetailRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let row = find_by_id(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询收款退款单"))?
        .ok_or_else(not_found)?;
    if row.status != STATUS_CONFIRMED {
        return Err(conflict(format!(
            "退款单 {} 当前是「{}」（{}）：只有「{}」的退款单可以提交审批（先确认，再送审）",
            row.refund_no,
            status_label(&row.status),
            row.status,
            status_label(STATUS_CONFIRMED)
        )));
    }
    if row.allocated_amount != row.amount {
        return Err(conflict(format!(
            "退款单 {} 还有 {} 没冲减到应收（退款 {} / 已冲减 {}）：\
             审批通过后系统会直接过账，所以提交前必须核销满",
            row.refund_no,
            row.remaining(),
            row.amount,
            row.allocated_amount
        )));
    }
    if flow_model::biz_instance_running(receipt_refund_bridge::BIZ_TYPE, id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = receipt_refund_bridge::latest_approval_of(db(), id)
            .await
            .ok()
            .flatten()
            .map(|brief| brief.instance_no)
            .unwrap_or_else(|| "审批单".to_string());
        return Err(conflict(format!(
            "退款单 {} 已在审批中（{instance_no}）：请等审批结果，或从审批中心撤回后再提交",
            row.refund_no
        )));
    }
    match receipt_refund_bridge::published_refund_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询退款审批流程"))?
    {
        Some(flow) => {
            // 有流程就必须走流程：请求里带了 skip_approval 也要 400（不许绕过已配置的内控）
            approval_gate::ensure_not_skipped(receipt_refund_bridge::FLOW_CODE, gate)?;
            start_approval(&row, &flow, operator).await
        }
        None => {
            let skip_reason = approval_gate::authorize_missing_flow(
                receipt_refund_bridge::BIZ_TYPE,
                receipt_refund_bridge::FLOW_CODE,
                gate,
            )?;
            tracing::warn!(
                refund_id = %row.id,
                refund_no = %row.refund_no,
                %skip_reason,
                "收款退款提交审批：没有已发布的 receipt_refund 流程，按「显式跳过审批」直接过账（理由已写 biz_status_log）"
            );
            change_status_with(
                operator,
                id_raw,
                RefundStatusReq {
                    status: STATUS_POSTED.to_string(),
                    reason: note.map(str::trim).filter(|text| !text.is_empty()).map(str::to_string),
                },
                Some(skip_reason),
            )
            .await
        }
    }
}

/// [`submit_approval`] 的「有流程」分支：发起审批实例（退款单状态**不变**，仍是 `CONFIRMED`）。
#[allow(clippy::too_many_arguments)]
async fn start_approval(
    row: &RefundRow,
    flow: &receipt_refund_bridge::RefundFlow,
    operator: Option<Uuid>,
) -> std::result::Result<RefundDetailRes, ApiError> {
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 收款退款 {} 元", row.refund_no, row.amount)
        .chars()
        .take(200)
        .collect::<String>();
    let start = engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(row.id),
        biz_no: Some(row.refund_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: receipt_refund_bridge::form_data(row),
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启退款审批事务"))?;
    let mut out = match engine::start(&tx, &start).await {
        Ok(out) => out,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(if flow_model::is_biz_active_duplicate(&error) {
                flow_model::approval_in_progress("收款退款单", Some(row.refund_no.as_str()))
            } else {
                write_error(error, "发起收款退款审批单")
            });
        }
    };
    if let Some(rejected) = out.rejected.take() {
        let _ = tx.rollback().await;
        return Err(rejected);
    }
    if out.instance_id != Some(instance_id) {
        let _ = tx.rollback().await;
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            "审批单发起后返回的实例 id 与预生成的不一致（引擎内部错误）",
        ));
    }
    let auto_finished = out.instance_status.eq_ignore_ascii_case("APPROVED");
    if !auto_finished && !out.instance_status.eq_ignore_ascii_case("PENDING") {
        let status = out.instance_status.clone();
        let _ = tx.rollback().await;
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("退款审批单发起后状态是 {status}（应为 PENDING）：流程定义可能没有可执行的审批节点"),
        ));
    }
    // 把「最近一轮审批实例」写回单头（V0361）：列表页一条 SQL 就能显示审批状态，
    // 前端不必逐行回查详情。与实例**同一事务**，不会出现「实例建了、指针没落」。
    tx.execute_with_params(
        &format!("UPDATE {TABLE} SET approval_instance_id = ?::uuid WHERE id = ?::uuid AND deleted_at IS NULL"),
        vec![Value::from(&instance_id), Value::from(&row.id)],
    )
    .await
    .map_err(|error| utils::db_error(error, "回写退款单审批实例"))?;
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交退款审批事务"))?;
    // 锁外副作用（审批通知 / 客户自配的通知节点）：提交之后才发，失败只 WARN
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    tracing::info!(
        refund_id = %row.id,
        refund_no = %row.refund_no,
        %instance_id,
        flow_code = %start.flow_code,
        "收款退款单已提交审批（退款单仍是已确认，等审批通过后自动过账）"
    );
    // 流程里没有任何可执行的审批节点（全被跳空）→ 实例在同一个事务里就走完了，
    // 必须立刻跑一次「审批通过」的回调，否则退款单停在 CONFIRMED 且没有待办能推进它。
    if auto_finished {
        receipt_refund_bridge::on_refund_settled(Some(instance_id), "APPROVED").await;
    }
    detail_with(db(), row.id).await
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
    let receivable_account = voucher::account_required(tx, DEBIT_CODE, "退款凭证的借方科目 1122 应收账款")
        .await
        .map_err(|error| write_error(error, "解析退款凭证借方科目"))?;
    let bank = voucher::account_required(tx, CREDIT_CODE, "退款凭证的贷方科目 1002 银行存款")
        .await
        .map_err(|error| write_error(error, "解析退款凭证贷方科目"))?;
    // 摘要取客户**名称**（AGENTS.md 约定 13 / E2E-FIX-5 ①）：账面上写 UUID 财务看不懂也搜不到。
    let name = customer_name(tx, refund.customer_id)
        .await
        .map_err(|error| write_error(error, "读退款单客户名称"))?;
    let customer = voucher::party_label(Some(name), "客户", refund.customer_id);
    let summary = format!("收款退款 {} 客户 {}", refund.refund_no, customer);
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
            source_doc_no: None,
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
pub async fn receipt_refund_page_handler(Query(query): Query<RefundPageQuery>) -> ResResult<PageData<Vec<RefundRes>>> {
    query.validate()?;
    let filter = query.to_filter()?;
    let (rows, total) = refund_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询收款退款单"))?;
    let list = rows.into_iter().map(RefundListRow::into_res).collect();
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

/// 提交审批（`CONFIRMED` + 核销满 → 发起 `receipt_refund` 实例；审批通过后自动过账）。
///
/// 权限码复用 `business:erp:receipt-refund:status`（与 SO-06 的
/// `POST /sales-order/{id}/submit-approval` 复用 `:status` 同一口径：
/// 提交审批是「推进单据状态」的一个动作，不另开码也就不用再加一行菜单）。
#[nik::permission(
    path = "/business/erp/receipt-refund/{id}/submit-approval",
    method = "POST",
    permission = "business:erp:receipt-refund:status"
)]
#[nik::post("/business/erp/receipt-refund/{id}/submit-approval")]
pub async fn receipt_refund_submit_approval(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<RefundSubmitReq>>,
) -> ResResult<RefundDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    let req = body.map(|Json(req)| req).unwrap_or_default();
    let gate = approval_gate::ApprovalGate::new(&req.approval, &claims);
    Ok(Res::ok(
        submit_approval(operator, &id, req.reason.as_deref(), &gate).await?,
    ))
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
    use crate::business::erp::approval_gate;
    use crate::business::erp::receipt_refund_bridge;
    use crate::business::flow::engine;
    use std::str::FromStr;

    fn dec(raw: &str) -> Decimal {
        Decimal::from_str(raw).expect("夹具金额非法")
    }

    /// 内存里的退款单头（纯逻辑用例用；不落库，不依赖 ERP_DB_URL）。
    fn refund_row_for_test() -> RefundRow {
        RefundRow {
            id: Uuid::now_v7(),
            refund_no: "RR-20261109-000001".to_string(),
            customer_id: Uuid::now_v7(),
            source_receipt_id: None,
            refund_date: NaiveDate::from_ymd_opt(2026, 11, 9).expect("固定日期合法"),
            amount: dec("100"),
            allocated_amount: dec("100"),
            method: Some(METHOD_BANK_TRANSFER.to_string()),
            bank_account: None,
            status: STATUS_CONFIRMED.to_string(),
            voucher_id: None,
            approval_instance_id: None,
            reason: "客户退货，货款原路退回".to_string(),
            remark: None,
            created_at: None,
            updated_at: None,
            create_by: Some(Uuid::now_v7()),
        }
    }

    // ---------- 纯逻辑用例（不连库） ----------

    /// 分页列清单必须覆盖 `RefundRow::from_row` 读的每一列（漏一列 = 整页 500）。
    #[test]
    fn list_columns_cover_row_fields() {
        for col in [
            "id",
            "refund_no",
            "customer_id",
            "source_receipt_id",
            "refund_date",
            "amount",
            "allocated_amount",
            "method",
            "bank_account",
            "status",
            "voucher_id",
            "approval_instance_id",
            "reason",
            "remark",
            "created_at",
            "updated_at",
            "create_by",
        ] {
            assert!(
                LIST_COLUMNS.contains(col),
                "LIST_COLUMNS 缺列 {col}（分页会 500）：{LIST_COLUMNS}"
            );
        }
        assert!(LIST_COLUMNS.contains("customer_name"), "列表要带客户名");
        // E2E-FIX-8 ②：列表必须一条 SQL 就带出审批状态（否则前端逐行回查详情）
        assert!(
            LIST_COLUMNS.contains("approval_status") && LIST_COLUMNS.contains("approval_instance_no"),
            "列表要带审批状态/单号：{LIST_COLUMNS}"
        );
        assert!(
            LIST_FROM.contains("biz_flow_instance"),
            "审批状态必须由 LIST_FROM 的 LEFT JOIN 带出：{LIST_FROM}"
        );
    }

    /// 审批三段（E2E-FIX-8 ②）：列表 / 详情共用同一套中英文映射。
    #[test]
    fn approval_columns_are_filled_from_the_same_source_as_detail() {
        let row = refund_row_for_test();
        // 未提交过审批：三段全空（不编造「无审批」）
        let fresh = RefundRes::from_row(&row, "客户甲").with_approval_columns(None, None);
        assert!(fresh.approval_instance_no.is_none() && fresh.approval_status.is_none());
        assert!(fresh.approval_status_label.is_none());

        // 列表路径：JOIN 出来的两列
        let listed = RefundRes::from_row(&row, "客户甲")
            .with_approval_columns(Some("FLW-20261109-00000001".into()), Some("PENDING".into()));
        assert_eq!(listed.approval_status.as_deref(), Some("PENDING"));
        assert_eq!(listed.approval_status_label.as_deref(), Some("审批中"));
        assert_eq!(listed.approval_instance_no.as_deref(), Some("FLW-20261109-00000001"));

        // 详情路径：审批桥的摘要 → 同样的三段
        let brief = receipt_refund_bridge::ApprovalBrief {
            instance_id: row.approval_instance_id.unwrap_or_else(Uuid::now_v7),
            instance_no: "FLW-20261109-00000002".into(),
            status: "REJECTED".into(),
            pending_tasks: 0,
        };
        let detailed = RefundRes::from_row(&row, "客户甲").with_approval_brief(Some(&brief));
        assert_eq!(
            detailed.approval_instance_id.as_deref(),
            Some(brief.instance_id.to_string().as_str())
        );
        assert_eq!(detailed.approval_status_label.as_deref(), Some("已驳回"));
        assert_eq!(detailed.approval_instance_no.as_deref(), Some("FLW-20261109-00000002"));

        // 「实例被清理」→ 指针归零（不留半截旧值）
        let cleared = detailed.with_approval_brief(None);
        assert!(cleared.approval_instance_id.is_none() && cleared.approval_status.is_none());
    }

    #[test]
    fn approval_status_labels_cover_the_engine_vocabulary() {
        assert_eq!(approval_status_label("PENDING"), "审批中");
        assert_eq!(approval_status_label("APPROVED"), "已通过");
        assert_eq!(approval_status_label("REJECTED"), "已驳回");
        assert_eq!(approval_status_label("WITHDRAWN"), "已撤回");
        assert_eq!(approval_status_label("CANCELLED"), "已终止");
        // 未知值原样透出（不吞、不编造中文）
        assert_eq!(approval_status_label("SOMETHING_NEW"), "SOMETHING_NEW");
    }

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
        assert_eq!(VoucherSourceType::ReceiptRefund.as_str(), SOURCE_RECEIPT_REFUND);
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
        for edge in [("SETTLED", "PARTIAL"), ("PARTIAL", "OPEN"), ("SETTLED", "OPEN")] {
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
        // 审批运行时表先清（`biz_flow_instance.biz_id` 不是外键 → 换表时要自己清，
        // 否则上一轮的 APPROVED 实例会让本轮的「过账闸」用例假通过）
        sql("DELETE FROM biz_flow_task WHERE instance_id IN \
             (SELECT id FROM biz_flow_instance WHERE biz_type = 'RECEIPT_REFUND')")
        .await;
        sql("DELETE FROM biz_flow_instance WHERE biz_type = 'RECEIPT_REFUND'").await;
        sql("DELETE FROM biz_receipt_refund_allocation").await;
        sql("DELETE FROM biz_receipt_refund").await;
        sql("DELETE FROM biz_status_log WHERE entity_type IN ('RECEIPT_REFUND','RECEIVABLE')").await;
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
        (Uuid::parse_str(FIX_CUSTOMER).expect("夹具 UUID 非法"), ar_id)
    }

    /// 一个超管 id（`sys_role.role_code = 'SUPER_ADMIN'` 的成员）。
    ///
    /// 审批回调里的 `initiator_id` 有外键到 `sys_user`，必须给真用户；
    /// 没有超管的库（没跑种子）→ `None`，调用方 SKIP。
    async fn admin_id() -> Option<Uuid> {
        let raw = scalar(
            "SELECT u.id::text FROM sys_user u \
               JOIN sys_user_role ur ON ur.user_id = u.id \
               JOIN sys_role r ON r.id = ur.role_id \
              WHERE r.role_code = 'SUPER_ADMIN' AND u.deleted_at IS NULL \
              ORDER BY u.created_at LIMIT 1",
        )
        .await;
        Uuid::parse_str(&raw).ok()
    }

    /// 已发布的 `receipt_refund` 流程（`(def_id, version_id)`）。
    async fn refund_flow() -> Option<(Uuid, Uuid)> {
        let raw = scalar(
            "SELECT (current_version_id::text || '|' || id::text) FROM biz_flow_def \
              WHERE flow_code = 'receipt_refund' AND status = 'PUBLISHED' AND deleted_at IS NULL",
        )
        .await;
        let (version, def) = raw.split_once('|')?;
        Some((Uuid::parse_str(def).ok()?, Uuid::parse_str(version).ok()?))
    }

    /// 给退款单造一条 `APPROVED` 审批实例（**只**给「过账闸」用例用）。
    ///
    /// 真实审批链（发起 → 待办 → 同意 → 回调过账）由
    /// `refund_approval_chain_posts_on_approve_and_stays_confirmed_on_reject` 覆盖；
    /// 这里只回答「闸认不认这个实例」，所以直接落一行终态实例，不跑引擎。
    async fn seed_approved_instance(refund_id: &Uuid) -> Uuid {
        let admin = admin_id().await.expect("库里必须有超管（种子迁移）");
        let (def_id, version_id) = refund_flow().await.expect("库里必须有 receipt_refund 流程（V0297）");
        let instance_id = Uuid::now_v7();
        let short = instance_id.to_string().replace('-', "");
        sql(&format!(
            "INSERT INTO biz_flow_instance (id, instance_no, def_id, version_id, version_no, \
                                            flow_code, flow_name, biz_type, biz_id, biz_no, title, \
                                            status, initiator_id, form_data, started_at, ended_at) \
             VALUES ('{instance_id}'::uuid, 'FLOW-T-{}', '{def_id}'::uuid, '{version_id}'::uuid, 1, \
                     'receipt_refund', '收款退款审批（财务复核）', 'RECEIPT_REFUND', \
                     '{refund_id}'::uuid, NULL, '过账闸夹具', 'APPROVED', '{admin}'::uuid, '{{}}'::jsonb, \
                     now(), now())",
            &short[..12]
        ))
        .await;
        instance_id
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
        let created = create(None, sample_req(customer)).await.expect("登记退款应成功");
        assert_eq!(created.header.status, STATUS_DRAFT);
        assert!(created.header.refund_no.starts_with("RR-"), "单号应带 RR- 前缀");
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

        // ③' 审批前置（SO-14 遗留 · D10）：没有已通过的审批实例 → 409 + 66044，
        //     报文必须点出提交审批的入口（前端按码渲染「去发起审批」）
        let gated = change_status(
            None,
            &refund_id.to_string(),
            RefundStatusReq {
                status: "POSTED".to_string(),
                reason: None,
            },
        )
        .await;
        let err = gated.expect_err("没审批就过账必须被拒");
        assert_eq!(err.code, error_code::ERP_RECEIPT_REFUND_APPROVAL_REQUIRED);
        assert!(
            err.msg.contains("submit-approval"),
            "报文必须给出提交审批的入口：{}",
            err.msg
        );
        // 造一条已通过的实例（真审批链在同模块的 approval_chain 用例里跑）
        let instance_id = seed_approved_instance(&refund_id).await;

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
        let approval = posted.approval.as_ref().expect("详情必须带审批段");
        assert_eq!(approval.instance_id, instance_id.to_string());
        assert_eq!(approval.status, "APPROVED");
        assert_eq!(approval.pending_tasks, 0);
        assert_eq!(
            scalar(&format!(
                "SELECT count(*) FROM biz_status_log WHERE entity_type = 'RECEIPT_REFUND' \
                    AND entity_id = '{refund_id}'::uuid AND to_status = 'POSTED' \
                    AND reason LIKE '%审批通过%'"
            ))
            .await,
            "1",
            "人工过账（已有 APPROVED 实例）的状态日志要写明是哪一轮批的"
        );
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
        let created = create(None, sample_req(customer)).await.expect("登记退款应成功");
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

    /// 夹具制单人（**不是**超管：超管是兜底审批人，同一个人会撞内控）。
    const FIX_CREATOR: &str = "d5950000-0000-4000-8000-0000000000c1";

    /// 确保夹具制单人存在（`sys_user.username` 唯一 → 幂等 upsert）。
    async fn ensure_creator() -> Uuid {
        sql(&format!(
            "INSERT INTO sys_user (id, username, password, real_name) \
             VALUES ('{FIX_CREATOR}'::uuid, 'rr14_ap_creator', 'rr14-not-a-real-hash', 'RR14 退款制单人') \
             ON CONFLICT (id) DO UPDATE SET deleted_at = NULL"
        ))
        .await;
        Uuid::parse_str(FIX_CREATOR).expect("夹具 UUID 非法")
    }

    /// 该实例当前待办的指派人（`PENDING`，取第一条）。
    async fn pending_assignee(instance_id: Uuid) -> Vec<String> {
        let rows = db()
            .find_with_params(
                "SELECT assignee_id::text AS value FROM biz_flow_task \
                  WHERE instance_id = ?::uuid AND status = 'PENDING' \
                  ORDER BY created_at",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("查询待办失败");
        rows.iter()
            .map(|row| row.get_typed::<String>("value").unwrap_or_default())
            .collect()
    }

    /// 审批一条待办，并在**事务提交之后**跑业务回调（与 `flow::handler::approve_task` 同序）。
    async fn approve_task(task_id: Uuid, operator: Uuid) -> String {
        let out = db()
            .transaction(|tx| async move { engine::approve(&tx, task_id, operator, Some("D10 同意")).await })
            .await
            .expect("审批失败");
        if let Some(rejected) = out.rejected {
            panic!("审批被拒：{} {}", rejected.code, rejected.msg);
        }
        let status = out.instance_status.clone();
        receipt_refund_bridge::on_refund_settled(out.instance_id, &status).await;
        status
    }

    /// 驳回并终止（`REJECTED`），同样在提交后跑回调。
    async fn reject_task(task_id: Uuid, operator: Uuid) -> String {
        let out = db()
            .transaction(|tx| async move {
                engine::reject(&tx, task_id, operator, "D10 金额存疑，驳回", engine::ReturnTo::Final).await
            })
            .await
            .expect("驳回失败");
        if let Some(rejected) = out.rejected {
            panic!("驳回被拒：{} {}", rejected.code, rejected.msg);
        }
        let status = out.instance_status.clone();
        receipt_refund_bridge::on_refund_settled(out.instance_id, &status).await;
        status
    }

    /// 该实例的**第一条待办**与它自己的指派人（同一行取，多待办时也不会配错人）。
    async fn first_pending_task(instance_id: Uuid) -> Option<(Uuid, Uuid)> {
        let raw = scalar(&format!(
            "SELECT (id::text || '|' || assignee_id::text) FROM biz_flow_task \
              WHERE instance_id = '{instance_id}'::uuid AND status = 'PENDING' \
              ORDER BY created_at LIMIT 1"
        ))
        .await;
        let (task, assignee) = raw.split_once('|')?;
        Some((Uuid::parse_str(task).ok()?, Uuid::parse_str(assignee).ok()?))
    }

    /// V0297 种子的「快照 ↔ 节点/边行」必须一致（否则 `flow::maintain` 每日自检会报漂移）。
    ///
    /// 这条用例是给**种子迁移**上的保险：`graph_hash` 是手写进 SQL 的，
    /// 谁改了节点/边的任何一列（哪怕只是 `config`）都会让 `load_graph().graph_hash()`
    /// 与库里的 `graph_hash` 分叉 —— 那是「行被改过」的形态，必须在提交前红掉。
    #[tokio::test]
    async fn seeded_flow_graph_hash_matches_its_rows() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let Some((_def_id, version_id)) = refund_flow().await else {
            println!("SKIP: 库里没有已发布的 receipt_refund 流程（V0297 没跑）");
            return;
        };
        let graph = flow_model::load_graph(version_id)
            .await
            .expect("加载 receipt_refund 流程版本图失败");
        let stored = scalar(&format!(
            "SELECT btrim(graph_hash) FROM biz_flow_def_version WHERE id = '{version_id}'::uuid"
        ))
        .await;
        assert_eq!(
            graph.graph_hash(),
            stored,
            "V0297 种子的 graph_snapshot/graph_hash 与 biz_flow_node/biz_flow_edge 行不一致"
        );
        assert_eq!(graph.nodes.len(), 3, "种子图应为 START + 财务经理审批 + END");
        assert_eq!(graph.edges.len(), 2, "种子图应为 e_start + e_end");
    }

    /// **审批链主用例**：发起 → 待办（内控排除制单人）→ 同意 → 回调**自动过账**；
    /// 另一张单驳回 → 退款单留在 `CONFIRMED`（账一分没动，可重新提交）。
    #[tokio::test]
    async fn refund_approval_chain_posts_on_approve_and_stays_confirmed_on_reject() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        if refund_flow().await.is_none() {
            println!("SKIP: 库里没有已发布的 receipt_refund 流程（V0297 没跑）");
            return;
        }
        let Some(admin) = admin_id().await else {
            println!("SKIP: 库里没有超管（种子迁移没跑）");
            return;
        };
        let creator = ensure_creator().await;
        assert_ne!(creator, admin, "内控用例要求制单人与兜底审批人不同人");
        let (customer, ar_id) = setup().await;

        // ---------- ① 登记 + 核销 + 确认（制单人 = creator，不是超管） ----------
        let created = create(Some(creator), sample_req(customer))
            .await
            .expect("登记退款应成功");
        let refund_id = Uuid::parse_str(&created.header.id).expect("id 非法");
        assert!(created.approval.is_none(), "还没提交审批 → 审批段必须为空");
        allocate(
            Some(creator),
            &refund_id.to_string(),
            RefundAllocateReq {
                allocations: vec![RefundAllocateLineReq {
                    receivable_id: ar_id.to_string(),
                    amount: dec("100"),
                    remark: None,
                }],
            },
        )
        .await
        .expect("核销应成功");
        change_status(
            Some(creator),
            &refund_id.to_string(),
            RefundStatusReq {
                status: STATUS_CONFIRMED.to_string(),
                reason: None,
            },
        )
        .await
        .expect("确认应成功");

        // ---------- ② 提交审批（状态不变，实例 PENDING） ----------
        let submitted = submit_approval(
            Some(creator),
            &refund_id.to_string(),
            None,
            &approval_gate::ApprovalGate::none(),
        )
        .await
        .expect("提交审批应成功");
        assert_eq!(
            submitted.header.status, STATUS_CONFIRMED,
            "审批不是状态：提交后单据仍是已确认"
        );
        let approval = submitted.approval.as_ref().expect("详情必须带审批段");
        assert_eq!(approval.status, "PENDING");
        assert!(
            approval.pending_tasks >= 1,
            "至少 1 条待办（共用库里兜底超管可能有多个 → 多条待办是正常的），实际 {}",
            approval.pending_tasks
        );
        let instance_id = Uuid::parse_str(&approval.instance_id).expect("实例 id 非法");
        // E2E-FIX-8 ②：单头也要带上审批三段（详情一次查询搞定，前端不用再查实例）
        assert_eq!(
            submitted.header.approval_instance_id.as_deref(),
            Some(approval.instance_id.as_str()),
            "提交审批必须把实例 id 写回单头"
        );
        assert_eq!(submitted.header.approval_status.as_deref(), Some("PENDING"));
        assert_eq!(submitted.header.approval_status_label.as_deref(), Some("审批中"));
        // E2E-FIX-8 ② 靶心：**列表页**一条 SQL 就带出审批状态（不用逐行回查详情）
        let filter = RefundFilter {
            customer_id: Some(customer),
            page: 1,
            page_size: 50,
            ..RefundFilter::default()
        };
        let (listed, _total) = refund_page(&filter).await.expect("列表查询失败");
        let listed_row = listed
            .iter()
            .find(|item| item.row.id == refund_id)
            .expect("刚建的退款单必须出现在列表里");
        assert_eq!(
            listed_row.approval_status.as_deref(),
            Some("PENDING"),
            "列表必须带审批状态（否则前端要 N 次二次查询）"
        );
        assert_eq!(
            listed_row.approval_instance_no.as_deref(),
            Some(approval.instance_no.as_str()),
            "列表必须带审批单号"
        );

        // 审批中不许重复提交
        let again = submit_approval(
            Some(creator),
            &refund_id.to_string(),
            None,
            &approval_gate::ApprovalGate::none(),
        )
        .await;
        let err = again.expect_err("审批中重复提交必须被拒");
        assert_eq!(err.code, error_code::RESOURCE_CONFLICT);

        // 审批中也不许直接过账（网关只认 APPROVED 实例）
        let early = change_status(
            Some(creator),
            &refund_id.to_string(),
            RefundStatusReq {
                status: STATUS_POSTED.to_string(),
                reason: None,
            },
        )
        .await;
        let err = early.expect_err("审批中不许过账");
        assert_eq!(err.code, error_code::ERP_RECEIPT_REFUND_APPROVAL_REQUIRED);

        // ---------- ③ 内控：制单人被排除在审批人之外 ----------
        let assignees = pending_assignee(instance_id).await;
        assert!(!assignees.is_empty(), "审批节点必须有人可派");
        assert!(
            !assignees.contains(&creator.to_string()),
            "内控失败：制单人不能成为自己退款单的审批人（待办={assignees:?}）"
        );

        // ---------- ④ 同意 → 回调自动过账 ----------
        let (task_id, approver) = first_pending_task(instance_id)
            .await
            .expect("待办必须存在（待办 id + 指派人）");
        let status = approve_task(task_id, approver).await;
        assert_eq!(status, "APPROVED");
        assert_eq!(
            scalar(&format!(
                "SELECT status FROM biz_receipt_refund WHERE id = '{refund_id}'::uuid"
            ))
            .await,
            STATUS_POSTED,
            "审批通过后回调必须**直接过账**（不需要人再点一次）"
        );
        assert_eq!(
            scalar(&format!(
                "SELECT count(*) FROM biz_voucher v WHERE v.source_type = 'RECEIPT_REFUND' \
                    AND v.source_id = '{refund_id}'::uuid"
            ))
            .await,
            "1",
            "过账必须恰好生成 1 张退款凭证"
        );
        assert_eq!(
            scalar(&format!(
                "SELECT count(*) FROM biz_status_log WHERE entity_type = 'RECEIPT_REFUND' \
                    AND entity_id = '{refund_id}'::uuid AND to_status = 'POSTED' \
                    AND reason LIKE '审批通过%'"
            ))
            .await,
            "1"
        );

        // 重复回调（幂等）：不再记账、不再多出凭证
        receipt_refund_bridge::on_refund_settled(Some(instance_id), "APPROVED").await;
        assert_eq!(
            scalar(&format!(
                "SELECT count(*) FROM biz_voucher v WHERE v.source_type = 'RECEIPT_REFUND' \
                    AND v.source_id = '{refund_id}'::uuid"
            ))
            .await,
            "1",
            "重复回调不许重复记账"
        );

        // ---------- ⑤ 驳回 → 退款单留在 CONFIRMED，账一分没动 ----------
        let second = create(Some(creator), sample_req(customer))
            .await
            .expect("第二张退款单应登记成功");
        let second_id = Uuid::parse_str(&second.header.id).expect("id 非法");
        let second_ar = Uuid::now_v7();
        sql(&format!(
            "INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no, \
                                        amount, received_amount, unsettled_amount, credit_days, \
                                        period_code, due_date, status) \
             VALUES ('{second_ar}'::uuid, 'AR-RR14C-000009', '{customer}'::uuid, 'MANUAL', \
                     gen_random_uuid(), 'MANUAL', 100, 100, 0, 30, '2026-11', '2026-12-06', 'SETTLED')"
        ))
        .await;
        allocate(
            Some(creator),
            &second_id.to_string(),
            RefundAllocateReq {
                allocations: vec![RefundAllocateLineReq {
                    receivable_id: second_ar.to_string(),
                    amount: dec("100"),
                    remark: None,
                }],
            },
        )
        .await
        .expect("第二张核销应成功");
        change_status(
            Some(creator),
            &second_id.to_string(),
            RefundStatusReq {
                status: STATUS_CONFIRMED.to_string(),
                reason: None,
            },
        )
        .await
        .expect("第二张确认应成功");
        let second_submitted = submit_approval(
            Some(creator),
            &second_id.to_string(),
            None,
            &approval_gate::ApprovalGate::none(),
        )
        .await
        .expect("第二张提交审批应成功");
        let second_instance =
            Uuid::parse_str(&second_submitted.approval.as_ref().expect("审批段").instance_id).expect("实例 id 非法");
        let second_assignees = pending_assignee(second_instance).await;
        assert!(
            !second_assignees.contains(&creator.to_string()),
            "内控失败：制单人出现在第二张单的待办里（{second_assignees:?}）"
        );
        let (second_task, second_approver) = first_pending_task(second_instance)
            .await
            .expect("第二张单的待办必须存在");
        let status = reject_task(second_task, second_approver).await;
        assert_eq!(status, "REJECTED");
        assert_eq!(
            scalar(&format!(
                "SELECT status FROM biz_receipt_refund WHERE id = '{second_id}'::uuid"
            ))
            .await,
            STATUS_CONFIRMED,
            "驳回后单据必须仍是已确认（审批不是状态）"
        );
        assert_eq!(
            scalar(&format!(
                "SELECT count(*) FROM biz_voucher WHERE source_type = 'RECEIPT_REFUND' \
                    AND source_id = '{second_id}'::uuid"
            ))
            .await,
            "0",
            "驳回一分钱都不许记账"
        );
        // 驳回后可以再提交（新实例），说明「驳回 → 改完再提」这条路是通的
        let resubmitted = submit_approval(
            Some(creator),
            &second_id.to_string(),
            None,
            &approval_gate::ApprovalGate::none(),
        )
        .await
        .expect("驳回后应能重新提交");
        assert_eq!(resubmitted.approval.as_ref().expect("审批段").status, "PENDING");

        // ---------- ⑥ 清场 ----------
        sql("DELETE FROM biz_flow_task WHERE instance_id IN \
             (SELECT id FROM biz_flow_instance WHERE biz_type = 'RECEIPT_REFUND')")
        .await;
        sql("DELETE FROM biz_flow_instance WHERE biz_type = 'RECEIPT_REFUND'").await;
        sql("DELETE FROM biz_status_log WHERE entity_type = 'RECEIPT_REFUND'").await;
        // 顺序按外键：核销明细 → 退款单 → 凭证 → 应收（`biz_receipt_refund_allocation.receivable_id`
        // 是 RESTRICT，先删应收会撞约束，报文只有一句 `db error`）→ 客户
        sql("DELETE FROM biz_receipt_refund_allocation").await;
        sql("DELETE FROM biz_receipt_refund").await;
        sql("DELETE FROM biz_voucher WHERE source_type = 'RECEIPT_REFUND'").await;
        sql("DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-RR14C-%'").await;
        sql("DELETE FROM biz_customer WHERE code LIKE 'RR14C-%'").await;
    }
}
