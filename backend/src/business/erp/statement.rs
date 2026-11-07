//! 客户对账单（销售链 · SO-43 / ARCH-7 §15.7）：**查询 + Excel 导出 + 对账确认**。
//!
//! # 这张单回答什么
//!
//! 「把某客户某期间的对账单发给客户财务，让他核一下——期初欠多少、本期发生了什么、期末欠多少」。
//! 数据全部来自既有台账，不新增业务表（ARCH-7 §15.7.2「不做新表，用查询生成对账单」）；
//! 只有**确认留痕**落一张 `biz_statement_confirm`（§15.7.3）。
//!
//! ```text
//! 期初 = 期间起始日之前的所有事件合计
//! 明细 = [period_from, period_to] 内的事件，按日期排序 + 逐笔 running balance
//! 期末 = 期初 + Σ 明细           ← 卡面硬要求：opening + sum(items) = closing（自校验）
//! ```
//!
//! # 事件口径（**为什么要「事件流」而不是直接读 `biz_receivable.unsettled_amount`**）
//!
//! `unsettled_amount` 只有「此刻」的余额，回答不了「9 月 30 日那天他欠我多少」。
//! 而且退货过账会**原地改小** `biz_receivable.amount`（见 `sales_return_posting.rs`：
//! 冲减应收 = `amount -= 退货含税`），坏账核销只改 `status` 不改金额 —— 单看应收行
//! 无法还原历史余额。因此对账单按**单据事件**重放：
//!
//! | 事件 | 来源 | 日期 | 金额（原币） |
//! |------|------|------|--------------|
//! | `INVOICE` 挂账 | `biz_receivable` | `created_at`（CST 日期，= 出库过账挂账当天） | `+（amount + 该应收已过账退货合计）` |
//! | `RETURN` 退货冲减 | `biz_sales_return`（`POSTED` 且有 `receivable_id`） | `return_date` | `− total_with_tax` |
//! | `RECEIPT` 收款核销 | `biz_receipt_allocation` ⋈ `biz_receipt` | `receipt_date` | `− 核销额` |
//! | `REFUND` 收款退款 | `biz_receipt_refund_allocation` ⋈ `biz_receipt_refund` | `refund_date` | `+ 冲减额` |
//! | `WRITE_OFF` 坏账核销 | `biz_receivable.status='WRITTEN_OFF'` | `settled_at`（CST 日期） | `− unsettled_amount` |
//!
//! ## 为什么 `INVOICE` 要加回「已过账退货」：
//!
//! 部分退货是**原地扣减** `amount` 的（100 挂账、退 30 → 库里 `amount` 只剩 70）。
//! 若挂账事件直接用 70，退货事件再记 −30，期末就少 30。所以挂账事件取
//! 「当前金额 + 该应收已过账退货合计」= 原始挂账额，退货事件单独列示 —— 客户看到的
//! 是「开票 100 → 退 30 → 欠 70」，这才是对账单该有的样子。
//!
//! ## 为什么作废应收与它的退货**整笔剔除**：
//!
//! `CANCELLED` 有两来路：① 出库单作废（`cancel_for_stock_out`，只改状态、**金额不动**）；
//! ② 全额退货（金额也不动，靠状态表示账已清）。两种情形下「原始挂账额 + 当前金额」都推不出
//! 一致的口径（①金额该归零、②靠退货事件抵零），强行混算会让期末凭空多出一笔。
//! 作废 = 这笔销售没发生，因此**应收本身、它的退货、挂在它上面的核销**一律不进对账单，
//! 对账单只对「真实发生的往来」负责。
//!
//! # 自校验（卡面硬要求）
//!
//! 期末余额有**两条互不依赖的路径**，任一对不上即 500（`STATEMENT_IDENTITY_BROKEN`）：
//!
//! 1. 明细路径：`opening + Σ items`（Rust 侧按事件流逐笔算，用户看到的就是这个数）；
//! 2. 台账路径：同一条事件视图上 `SELECT SUM(amt) WHERE dt <= period_to`（SQL 聚合，
//!    不经过明细列表、不受明细排序/截断影响）。
//!
//! 期初同样两条路径对照（事件流累加 vs SQL 聚合 `dt < period_from`）。
//!
//! 容差 0.01（分），超出即判定对账单不可信 —— 宁可 500 让用户报障，也不出一份数字打架的对账单。
//!
//! # 币种
//!
//! 事件金额一律是**单据原币**（不做换算：对账单是对给客户的往来核对单，换算会引入
//! 汇率差异争议）。跨币种混加没有意义，因此：
//!
//! * 客户只有一种币种的往来 → 自动用该币种；
//! * 客户有多币种往来 → **必须显式传 `currency`**，否则 400 并列出可选币种；
//! * 传了 `currency` 但该客户没有这个币种的应收 → 400（不静默返回空单）。
//!
//! # 接口
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/receivable/statement` | `business:erp:statement:view` | 对账单（期初 / 明细 / 期末 / 发生额） |
//! | GET | `/business/erp/receivable/statement/export` | `business:erp:statement:export` | 同一份对账单导出 xlsx（复用 `impexp` 写表能力） |
//! | GET | `/business/erp/receivable/statement/confirm/page` | `business:erp:statement:view` | 对账确认台账分页 |
//! | POST | `/business/erp/receivable/statement/confirm` | `business:erp:statement:send` | 签发对账单（**服务端重算并快照**，`SENT`） |
//! | PUT | `/business/erp/receivable/statement/confirm/{id}/confirm` | `business:erp:statement:confirm` | 客户确认（`SENT → CONFIRMED`） |
//! | PUT | `/business/erp/receivable/statement/confirm/{id}/dispute` | `business:erp:statement:dispute` | 客户提出异议（`SENT → DISPUTED`，必须给原因） |
//!
//! # 为什么「签发」要快照金额
//!
//! 客户手里的对账单是**一份文件**。若签发后财务又做了一笔收款，回读时数字变了，
//! 客户核对的口径就与系统不一致 —— 对账永远对不完。所以签发时把
//! `opening / closing / 发生额 / 笔数` 一并写进 `biz_statement_confirm`，
//! 之后无论账怎么动，`GET .../confirm/page` 看到的仍是**当时签发的那一版**。
//! 只有 `SENT` 状态允许重新签发（覆盖快照）；`CONFIRMED` / `DISPUTED` 是留痕结论，重发 409。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_receivable / biz_receipt / biz_sales_return / biz_statement_confirm
//! 均无 dept_id / 归属人列（V0287 / V0292 / V0303 建表），数据范围表达不出来；
//! 可见性由 business:erp:statement:* 权限码控制。
//! SCOPE-EXEMPT: biz_customer 是客户主数据（全局），只取 code / name / tax_no 三个展示字段。
//! ```

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query};
use axum::response::Response;
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::asset_dto::normalize_page;
use crate::shared::error_code;
use crate::utils;

// ==================== 表名 / 常量 ====================

/// 应收账款表（挂账事件源）。
pub const AR_TABLE: &str = "biz_receivable";
/// 销售退货表（退货冲减事件源）。
pub const RETURN_TABLE: &str = "biz_sales_return";
/// 收款单头（收款核销事件的日期来源）。
pub const RECEIPT_TABLE: &str = "biz_receipt";
/// 收款核销明细（核销额）。
pub const ALLOC_TABLE: &str = "biz_receipt_allocation";
/// 收款退款单头（退款事件的日期来源）。
pub const REFUND_TABLE: &str = "biz_receipt_refund";
/// 收款退款核销明细（冲减额）。
pub const REFUND_ALLOC_TABLE: &str = "biz_receipt_refund_allocation";
/// 客户主数据（只取展示三字段）。
pub const CUSTOMER_TABLE: &str = "biz_customer";
/// 对账确认台账。
pub const CONFIRM_TABLE: &str = "biz_statement_confirm";

/// 事件类型：挂账（应收发生）。
pub const KIND_INVOICE: &str = "INVOICE";
/// 事件类型：退货冲减。
pub const KIND_RETURN: &str = "RETURN";
/// 事件类型：收款核销。
pub const KIND_RECEIPT: &str = "RECEIPT";
/// 事件类型：收款退款冲回（已核销的钱退了，应收回到未收）。
pub const KIND_REFUND: &str = "REFUND";
/// 事件类型：坏账核销。
pub const KIND_WRITE_OFF: &str = "WRITE_OFF";

/// 事件类型白名单（出参过滤 / 单测用）。
pub const KINDS: [&str; 5] = [
    KIND_INVOICE,
    KIND_RETURN,
    KIND_RECEIPT,
    KIND_REFUND,
    KIND_WRITE_OFF,
];

/// 对账确认状态：已签发。
pub const STATUS_SENT: &str = "SENT";
/// 对账确认状态：客户已确认。
pub const STATUS_CONFIRMED: &str = "CONFIRMED";
/// 对账确认状态：客户提出异议。
pub const STATUS_DISPUTED: &str = "DISPUTED";

/// 对账确认状态白名单（与 `ck_biz_statement_status` 逐字一致）。
pub const CONFIRM_STATUSES: [&str; 3] = [STATUS_SENT, STATUS_CONFIRMED, STATUS_DISPUTED];

/// 导出文件名前缀。
pub const EXPORT_PREFIX: &str = "客户对账单";

/// 导出表头（列顺序即文件列顺序）。
pub const EXPORT_HEADERS: [&str; 6] = ["日期", "类型", "单据号", "摘要", "发生额", "余额"];

/// 自校验容差：1 分。DB 的 `NUMERIC(18,4)` 与两侧汇总顺序无关，正常必然精确相等，
/// 留 1 分只是为了不被四舍五入的极端尾差误伤。
pub fn identity_tolerance() -> Decimal {
    Decimal::new(1, 2)
}

/// 金额归一：两分位（对账单是给人看的，四位小数的尾数只会引发争议）。
///
/// `rescale(2)` 是必须的：`Decimal::ZERO` 的标度是 0，只 `round_dp(2)` 仍会序列化成 `"0"`，
/// 一家客户「期初 0」、另一家「期初 0.00」的 API 契约不可接受。
pub fn money(value: Decimal) -> Decimal {
    let mut out = value.round_dp(2);
    out.rescale(2);
    out
}

/// 金额文本：**不带千分位**，便于客户在 Excel 里直接求和（`write_sheet` 只能写字符串）。
pub fn money_text(value: Decimal) -> String {
    format!("{:.2}", money(value))
}

/// 事件类型中文名。
pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        KIND_INVOICE => "挂账",
        KIND_RETURN => "退货冲减",
        KIND_RECEIPT => "收款",
        KIND_REFUND => "收款退款",
        KIND_WRITE_OFF => "坏账核销",
        _ => "其他",
    }
}

/// 对账确认状态中文名。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_SENT => "已签发",
        STATUS_CONFIRMED => "客户已确认",
        STATUS_DISPUTED => "客户已提异议",
        _ => "未知",
    }
}

// ==================== 入参 ====================

/// 对账单查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StatementQuery {
    /// 客户 id（UUID，必填）。
    pub customer_id: String,
    /// 期间起（`yyyy-MM-dd`，必填，含当天）。
    pub period_from: String,
    /// 期间止（`yyyy-MM-dd`，缺省 = 今天 CST，含当天）。
    pub period_to: Option<String>,
    /// 币种（ISO 4217；客户多币种往来时必填）。
    pub currency: Option<String>,
}

/// 签发对账单入参（客户 + 期间 + 币种；金额**不接受客户端传**，一律服务端重算）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StatementSendReq {
    /// 客户 id。
    pub customer_id: String,
    /// 期间起。
    pub period_from: String,
    /// 期间止（缺省 = 今天）。
    pub period_to: Option<String>,
    /// 币种。
    pub currency: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 客户确认入参。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StatementConfirmReq {
    /// 客户方确认人（必填，去空白后 ≥ 2 字）。
    pub confirmed_by: String,
    /// 客户盖章回传的对账单文件 id。
    pub attachment_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 客户提异议入参。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StatementDisputeReq {
    /// 异议原因（必填，去空白后 ≥ 4 字）。
    pub dispute_reason: String,
    /// 客户回传的差异说明 / 对账单文件 id。
    pub attachment_id: Option<String>,
}

/// 对账确认台账分页参数。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StatementConfirmPageQuery {
    /// 客户 id。
    pub customer_id: Option<String>,
    /// 状态（`SENT` / `CONFIRMED` / `DISPUTED`）。
    pub status: Option<String>,
    /// 期间起下界（含）。
    pub period_from: Option<String>,
    /// 期间止上界（含）。
    pub period_to: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

// ==================== 出参 ====================

/// 客户信息（对账单表头）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct StatementCustomerRes {
    /// 客户 id。
    pub customer_id: String,
    /// 客户编码。
    pub code: String,
    /// 客户名称。
    pub name: String,
    /// 纳税人识别号（对账单必需：客户财务要拿它核主体）。
    pub tax_no: String,
}

/// 期间信息。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct StatementPeriodRes {
    /// 期间起。
    pub from: NaiveDate,
    /// 期间止。
    pub to: NaiveDate,
    /// 期间天数（含首尾）。
    pub days: i64,
}

/// 明细一行。
#[derive(Debug, Clone, Serialize)]
pub struct StatementItemRes {
    /// 序号（从 1 开始，按展示顺序）。
    pub seq: usize,
    /// 业务日期。
    pub date: NaiveDate,
    /// 事件类型（`INVOICE` / `RETURN` / `RECEIPT` / `REFUND` / `WRITE_OFF`）。
    pub kind: String,
    /// 事件类型中文。
    pub kind_label: String,
    /// 单据号。
    pub doc_no: String,
    /// 摘要。
    pub memo: String,
    /// 发生额（带符号：挂账 +、冲减 −）。
    pub amount: Decimal,
    /// 本笔之后的对账余额。
    pub balance: Decimal,
}

/// 自校验结果（卡面「opening + sum(items) = closing」的机器证据，随响应一起返回）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct StatementIdentityRes {
    /// 是否通过。
    pub passed: bool,
    /// 期初（按事件流累加）。
    pub opening_balance: Decimal,
    /// 期初（独立 SQL 聚合，`dt < period_from`）。
    pub opening_by_ledger: Decimal,
    /// 期末（= 期初 + Σ 明细，用户看到的数）。
    pub closing_balance: Decimal,
    /// 期末（独立 SQL 聚合，`dt <= period_to`）。
    pub closing_by_ledger: Decimal,
    /// 两条路径的最大差额。
    pub diff: Decimal,
    /// 容差。
    pub tolerance: Decimal,
}

/// 期间发生额汇总。
#[derive(Debug, Clone, Default, Serialize)]
pub struct StatementDetailRes {
    /// 挂账合计（正数）。
    pub total_invoiced: Decimal,
    /// 收款合计（正数，绝对值）。
    pub total_received: Decimal,
    /// 退货冲减合计（正数）。
    pub total_returned: Decimal,
    /// 收款退款合计（正数）。
    pub total_refunded: Decimal,
    /// 坏账核销合计（正数）。
    pub total_written_off: Decimal,
    /// 期间净发生额（= Σ 明细发生额 = 期末 − 期初）。
    pub net_change: Decimal,
    /// 明细笔数。
    pub item_count: usize,
    /// 自校验。
    pub identity: StatementIdentityRes,
}

/// 对账单出参（卡面 §15.7.2 的字段 + 币种 / 自校验）。
#[derive(Debug, Clone, Serialize)]
pub struct StatementRes {
    /// 客户。
    pub customer: StatementCustomerRes,
    /// 期间。
    pub period: StatementPeriodRes,
    /// 币种（金额的口径）。
    pub currency: String,
    /// 期初余额。
    pub opening_balance: Decimal,
    /// 明细（按日期排序，逐笔余额）。
    pub items: Vec<StatementItemRes>,
    /// 期末余额。
    pub closing_balance: Decimal,
    /// 期间发生额汇总 + 自校验。
    pub detail: StatementDetailRes,
    /// 编制时间。
    pub generated_at: DateTime<Utc>,
    /// 口径提示。
    pub hints: Vec<String>,
}

/// 对账确认台账一行。
#[derive(Debug, Clone, Serialize)]
pub struct StatementConfirmRes {
    /// 主键。
    pub id: String,
    /// 客户 id。
    pub customer_id: String,
    /// 客户名称。
    pub customer_name: String,
    /// 期间起。
    pub period_from: NaiveDate,
    /// 期间止。
    pub period_to: NaiveDate,
    /// 币种。
    pub currency: String,
    /// 对账总额（= 签发时的期末余额）。
    pub total_amount: Decimal,
    /// 签发时的期初余额。
    pub opening_balance: Decimal,
    /// 签发时的期末余额。
    pub closing_balance: Decimal,
    /// 签发时的挂账合计。
    pub total_invoiced: Decimal,
    /// 签发时的收款合计。
    pub total_received: Decimal,
    /// 签发时的退货合计。
    pub total_returned: Decimal,
    /// 签发时的明细笔数。
    pub item_count: i32,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 签发时间。
    pub sent_at: DateTime<Utc>,
    /// 客户确认时间。
    pub confirmed_at: Option<DateTime<Utc>>,
    /// 客户方确认人。
    pub confirmed_by: Option<String>,
    /// 异议原因。
    pub dispute_reason: Option<String>,
    /// 附件 id。
    pub attachment_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 对账确认台账分页出参。
#[derive(Debug, Clone, Serialize)]
pub struct StatementConfirmPageRes {
    /// 总数。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 当前页。
    pub items: Vec<StatementConfirmRes>,
}

// ==================== 事件（内部形态） ====================

/// 一条对账事件（已带符号）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementEvent {
    /// 业务日期。
    pub date: NaiveDate,
    /// 同日期内的排序权重（挂账 1 < 退货 2 < 收款 3 < 退款 4 < 坏账 5）。
    pub rank: i32,
    /// 事件类型。
    pub kind: String,
    /// 单据号。
    pub doc_no: String,
    /// 摘要。
    pub memo: String,
    /// 带符号金额。
    pub amount: Decimal,
}

/// 事件排序键：日期 → 类型权重 → 单据号（确定性：同一天挂账先于冲减，冲减先于收款）。
pub fn sort_events(events: &mut [StatementEvent]) {
    events.sort_by(|left, right| {
        left.date
            .cmp(&right.date)
            .then_with(|| left.rank.cmp(&right.rank))
            .then_with(|| left.doc_no.cmp(&right.doc_no))
    });
}

/// 事件流在 `date < before` 上的合计（纯逻辑，与 SQL 聚合互为对照）。
pub fn sum_before(events: &[StatementEvent], before: NaiveDate) -> Decimal {
    events
        .iter()
        .filter(|event| event.date < before)
        .map(|event| event.amount)
        .sum()
}

/// 把 `[from, to]` 的事件拍成明细（逐笔算运行余额）。
pub fn build_items(
    opening: Decimal,
    events: &[StatementEvent],
    from: NaiveDate,
    to: NaiveDate,
) -> Vec<StatementItemRes> {
    let mut balance = opening;
    let mut items = Vec::new();
    for event in events {
        if event.date < from || event.date > to {
            continue;
        }
        balance += event.amount;
        items.push(StatementItemRes {
            seq: items.len() + 1,
            date: event.date,
            kind: event.kind.clone(),
            kind_label: kind_label(&event.kind).to_string(),
            doc_no: event.doc_no.clone(),
            memo: event.memo.clone(),
            amount: money(event.amount),
            balance: money(balance),
        });
    }
    items
}

/// 期间发生额汇总（纯逻辑）。
pub fn summarise(items: &[StatementItemRes]) -> (Decimal, Decimal, Decimal, Decimal, Decimal) {
    let mut invoiced = Decimal::ZERO;
    let mut received = Decimal::ZERO;
    let mut returned = Decimal::ZERO;
    let mut refunded = Decimal::ZERO;
    let mut written_off = Decimal::ZERO;
    for item in items {
        match item.kind.as_str() {
            KIND_INVOICE => invoiced += item.amount,
            KIND_RECEIPT => received += -item.amount,
            KIND_RETURN => returned += -item.amount,
            KIND_REFUND => refunded += item.amount,
            KIND_WRITE_OFF => written_off += -item.amount,
            _ => {}
        }
    }
    (
        money(invoiced),
        money(received),
        money(returned),
        money(refunded),
        money(written_off),
    )
}

/// 自校验：明细路径（期初 + Σ 明细）与台账路径（两条独立 SQL 聚合）逐项对照。
pub fn build_identity(
    opening: Decimal,
    opening_by_ledger: Decimal,
    items: &[StatementItemRes],
    closing_by_ledger: Decimal,
) -> StatementIdentityRes {
    let sum_items: Decimal = items.iter().map(|item| item.amount).sum();
    let closing = money(opening + sum_items);
    let opening = money(opening);
    let opening_by_ledger = money(opening_by_ledger);
    let closing_by_ledger = money(closing_by_ledger);
    let diff = ((closing - closing_by_ledger).abs()).max((opening - opening_by_ledger).abs());
    let tolerance = identity_tolerance();
    StatementIdentityRes {
        passed: money(diff) <= tolerance,
        opening_balance: opening,
        opening_by_ledger,
        closing_balance: closing,
        closing_by_ledger,
        diff: money(diff),
        tolerance,
    }
}

// ==================== 入参校验（纯逻辑） ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// UUID 参数解析（非法 → 400，不静默忽略条件）。
pub fn parse_uuid_param(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    Uuid::parse_str(text).map_err(|_| bad_request(format!("{field} 不是合法的 UUID：{text}")))
}

/// 日期参数解析（`yyyy-MM-dd`，非法 → 400）。
pub fn parse_date_param(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空（格式 yyyy-MM-dd）")));
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| bad_request(format!("{field} 不是合法日期（格式 yyyy-MM-dd）：{text}")))
}

/// 期间解析：`period_from` 必填、`period_to` 缺省取 `today`，且 `from <= to`。
pub fn parse_period(
    period_from: &str,
    period_to: Option<&str>,
    today: NaiveDate,
) -> std::result::Result<(NaiveDate, NaiveDate), ApiError> {
    let from = parse_date_param(period_from, "period_from")?;
    let to = match period_to.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => parse_date_param(text, "period_to")?,
        None => today,
    };
    if from > to {
        return Err(bad_request(format!(
            "期间起点 {from} 晚于期间终点 {to}：对账单的期间不能倒置"
        )));
    }
    Ok((from, to))
}

/// 币种解析：单一币种自动取；多币种必须显式指定；指定了但不存在 → 400。
pub fn resolve_currency(
    available: &[String],
    requested: Option<&str>,
) -> std::result::Result<String, ApiError> {
    let requested = requested
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_uppercase);
    match requested {
        Some(code) => {
            if available.iter().any(|item| item == &code) {
                Ok(code)
            } else if available.is_empty() {
                Err(bad_request(format!(
                    "该客户没有任何应收往来，无法出具币种 {code} 的对账单"
                )))
            } else {
                Err(bad_request(format!(
                    "该客户没有币种 {code} 的应收往来（现有币种：{}）",
                    available.join(" / ")
                )))
            }
        }
        None => match available.len() {
            0 => Ok("CNY".to_string()),
            1 => Ok(available[0].clone()),
            _ => Err(bad_request(format!(
                "该客户有多个币种的应收往来（{}）：对账单按单一币种出具，请用 currency 指定",
                available.join(" / ")
            ))),
        },
    }
}

/// 客户方确认人校验（去空白后 ≥ 2 字）。
pub fn validate_confirmed_by(raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim();
    if text.chars().count() < 2 {
        return Err(bad_request(
            "confirmed_by 必填：客户方确认人姓名至少 2 个字".to_string(),
        ));
    }
    if text.chars().count() > 64 {
        return Err(bad_request("confirmed_by 最多 64 个字符".to_string()));
    }
    Ok(text.to_string())
}

/// 异议原因校验（去空白后 ≥ 4 字；`biz_statement_confirm.dispute_reason` 是 TEXT，无长度上限）。
pub fn validate_dispute_reason(raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim();
    if text.chars().count() < 4 {
        return Err(bad_request(
            "dispute_reason 必填：异议原因至少 4 个字（客户凭什么不认这张对账单，要能追溯）"
                .to_string(),
        ));
    }
    Ok(text.to_string())
}

/// 状态流转闸：只有 `SENT` 能被确认 / 提异议（`CONFIRMED` / `DISPUTED` 是留痕结论，不可改写）。
pub fn ensure_pending(status: &str, action: &str) -> std::result::Result<(), ApiError> {
    if status == STATUS_SENT {
        return Ok(());
    }
    Err(conflict(format!(
        "该对账单当前是「{}」，不能再{}：只有「已签发」的对账单等待客户回执",
        status_label(status),
        action
    )))
}

/// 附件 id 归一（空白 = 无附件；非法 UUID → 400）。
pub fn parse_optional_uuid(
    raw: Option<&str>,
    field: &str,
) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => Ok(Some(parse_uuid_param(text, field)?)),
        None => Ok(None),
    }
}

// ==================== 组装（纯逻辑，可单测） ====================

/// 组装入参（数据层取数结果 + 期间 + 客户）。
#[derive(Debug, Clone)]
pub struct StatementInputs {
    /// 客户。
    pub customer: StatementCustomerRes,
    /// 期间起。
    pub from: NaiveDate,
    /// 期间止。
    pub to: NaiveDate,
    /// 币种。
    pub currency: String,
    /// 全量事件（任意日期，本函数负责按期间切分）。
    pub events: Vec<StatementEvent>,
    /// 期初（独立 SQL 聚合 `dt < from`）。
    pub opening_by_ledger: Decimal,
    /// 期末（独立 SQL 聚合 `dt <= to`）。
    pub closing_by_ledger: Decimal,
    /// 编制时间。
    pub generated_at: DateTime<Utc>,
}

/// 把取数结果组装成对账单；自校验不过 → 500（绝不出数字打架的对账单）。
pub fn assemble(mut inputs: StatementInputs) -> std::result::Result<StatementRes, ApiError> {
    sort_events(&mut inputs.events);
    let opening = sum_before(&inputs.events, inputs.from);
    let items = build_items(opening, &inputs.events, inputs.from, inputs.to);
    let identity = build_identity(
        opening,
        inputs.opening_by_ledger,
        &items,
        inputs.closing_by_ledger,
    );
    if !identity.passed {
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!(
                "对账单自校验失败（期初 + Σ明细 ≠ 期末）：期初 {} / 明细合计 {} / 期末 {}；\
                 台账口径 期初 {} / 期末 {}，差额 {} 超过容差 {}。\
                 不会输出对不上的对账单，请联系财务核对台账",
                identity.opening_balance,
                identity.closing_balance - identity.opening_balance,
                identity.closing_balance,
                identity.opening_by_ledger,
                identity.closing_by_ledger,
                identity.diff,
                identity.tolerance
            ),
        ));
    }
    let (invoiced, received, returned, refunded, written_off) = summarise(&items);
    let closing = identity.closing_balance;
    let mut hints = vec![format!(
        "口径：金额单位为「{0}」原币，不做汇率换算；期初 = 期间起始日之前的全部往来合计，\
         期末 = 期初 + 本期发生（自校验差额 {1}，容差 {2}）",
        inputs.currency, identity.diff, identity.tolerance
    )];
    hints.push(
        "已作废的应收单（出库作废 / 全额退货）连同其退货、核销整笔剔除：那笔销售没有发生，\
         不进对账单"
            .to_string(),
    );
    if items.is_empty() {
        hints.push(format!(
            "该客户在 {} ~ {} 期间没有往来发生（没有数据不是出错）",
            inputs.from, inputs.to
        ));
    }
    Ok(StatementRes {
        customer: inputs.customer,
        period: StatementPeriodRes {
            from: inputs.from,
            to: inputs.to,
            days: (inputs.to - inputs.from).num_days() + 1,
        },
        currency: inputs.currency,
        opening_balance: identity.opening_balance,
        items,
        closing_balance: closing,
        detail: StatementDetailRes {
            total_invoiced: invoiced,
            total_received: received,
            total_returned: returned,
            total_refunded: refunded,
            total_written_off: written_off,
            net_change: money(closing - identity.opening_balance),
            item_count: 0,
            identity,
        },
        generated_at: inputs.generated_at,
        hints,
    })
    .map(|mut statement| {
        statement.detail.item_count = statement.items.len();
        statement
    })
}

// ==================== 取数 ====================

/// 客户一行（只取展示字段）。
#[derive(Debug, Clone)]
pub struct StatementCustomerRow {
    /// 客户 id。
    pub id: Uuid,
    /// 编码。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 纳税人识别号。
    pub tax_no: String,
}

/// 事件视图（五路 UNION ALL，**全模块唯一一处口径定义**；查询与聚合共用它）。
fn events_cte() -> String {
    format!(
        "WITH ev AS ( \
           SELECT (r.created_at AT TIME ZONE 'Asia/Shanghai')::date AS dt, 1 AS rank, \
                  '{KIND_INVOICE}'::text AS kind, r.ar_no AS doc_no, r.currency_code::text AS currency, \
                  (r.amount + COALESCE(ret.amt, 0))::numeric AS amt, \
                  (CASE WHEN r.source_type = 'MANUAL' THEN '手工挂账 ' ELSE '销售出库挂账 ' END \
                        || r.source_no) AS memo \
             FROM {AR_TABLE} r \
             LEFT JOIN (SELECT sr.receivable_id, SUM(sr.total_with_tax) AS amt \
                          FROM {RETURN_TABLE} sr \
                         WHERE sr.status = 'POSTED' AND sr.deleted_at IS NULL \
                           AND sr.receivable_id IS NOT NULL \
                         GROUP BY sr.receivable_id) ret ON ret.receivable_id = r.id \
            WHERE r.customer_id = ?::uuid AND r.deleted_at IS NULL \
              AND r.status <> '{CANCELLED}' AND r.currency_code::text = ? \
           UNION ALL \
           SELECT sr.return_date, 2, '{KIND_RETURN}', sr.return_no, r.currency_code::text, \
                  (-sr.total_with_tax)::numeric, \
                  ('退货冲减（' || sr.return_type || '）' || COALESCE(NULLIF(sr.remark, ''), '')) \
             FROM {RETURN_TABLE} sr \
             JOIN {AR_TABLE} r ON r.id = sr.receivable_id AND r.deleted_at IS NULL \
                        AND r.status <> '{CANCELLED}' \
            WHERE sr.customer_id = ?::uuid AND sr.status = 'POSTED' AND sr.deleted_at IS NULL \
              AND sr.receivable_id IS NOT NULL AND r.currency_code::text = ? \
           UNION ALL \
           SELECT rc.receipt_date, 3, '{KIND_RECEIPT}', rc.receipt_no, r.currency_code::text, \
                  (-a.amount)::numeric, ('核销 ' || r.ar_no) \
             FROM {ALLOC_TABLE} a \
             JOIN {RECEIPT_TABLE} rc ON rc.id = a.receipt_id AND rc.deleted_at IS NULL \
                        AND rc.status <> 'CANCELLED' \
             JOIN {AR_TABLE} r ON r.id = a.receivable_id AND r.deleted_at IS NULL \
                        AND r.status <> '{CANCELLED}' \
            WHERE rc.customer_id = ?::uuid AND r.currency_code::text = ? \
           UNION ALL \
           SELECT rf.refund_date, 4, '{KIND_REFUND}', rf.refund_no, r.currency_code::text, \
                  a.amount::numeric, ('退款冲回 ' || r.ar_no) \
             FROM {REFUND_ALLOC_TABLE} a \
             JOIN {REFUND_TABLE} rf ON rf.id = a.refund_id AND rf.deleted_at IS NULL \
                        AND rf.status <> 'CANCELLED' \
             JOIN {AR_TABLE} r ON r.id = a.receivable_id AND r.deleted_at IS NULL \
                        AND r.status <> '{CANCELLED}' \
            WHERE rf.customer_id = ?::uuid AND r.currency_code::text = ? \
           UNION ALL \
           SELECT (r.settled_at AT TIME ZONE 'Asia/Shanghai')::date, 5, '{KIND_WRITE_OFF}', \
                  r.ar_no, r.currency_code::text, (-r.unsettled_amount)::numeric, '坏账核销' \
             FROM {AR_TABLE} r \
            WHERE r.customer_id = ?::uuid AND r.deleted_at IS NULL \
              AND r.status = 'WRITTEN_OFF' AND r.settled_at IS NOT NULL \
              AND r.currency_code::text = ? \
         )",
        CANCELLED = crate::business::erp::receivable::STATUS_CANCELLED,
    )
}

/// 事件视图的前十个参数（五个分支各一对 customer / currency）。
fn event_params(customer: Uuid, currency: &str) -> Vec<Value> {
    let mut params = Vec::with_capacity(10);
    for _ in 0..5 {
        params.push(Value::from(customer));
        params.push(Value::Text(currency.to_string()));
    }
    params
}

/// 读该客户某币种的**全部**事件（日期升序、类型权重、单据号）。
pub async fn load_events(
    ex: &impl SqlExecutor,
    customer: Uuid,
    currency: &str,
) -> DbResult<Vec<StatementEvent>> {
    let sql = format!(
        "{cte} SELECT ev.dt, ev.rank, ev.kind, ev.doc_no, ev.memo, ev.amt FROM ev \
         ORDER BY ev.dt, ev.rank, ev.doc_no",
        cte = events_cte()
    );
    let rows = ex.query_sql(sql, event_params(customer, currency)).await?;
    let mut events = Vec::with_capacity(rows.len());
    for row in &rows {
        events.push(StatementEvent {
            date: row.get_typed::<NaiveDate>("dt")?,
            rank: row.get_typed::<i32>("rank")?,
            kind: row.get_typed::<String>("kind")?,
            doc_no: row.get_typed::<String>("doc_no")?,
            memo: row.get_typed::<String>("memo")?,
            amount: row.get_typed::<Decimal>("amt")?,
        });
    }
    Ok(events)
}

/// 台账口径的余额：`dt < = until` 的合计（独立 SQL 聚合，供自校验对照）。
pub async fn sum_until(
    ex: &impl SqlExecutor,
    customer: Uuid,
    currency: &str,
    until: NaiveDate,
) -> DbResult<Decimal> {
    let sql = format!(
        "{cte} SELECT COALESCE(SUM(ev.amt), 0) AS total FROM ev WHERE ev.dt <= ?::date",
        cte = events_cte()
    );
    let mut params = event_params(customer, currency);
    params.push(Value::from(until));
    let row = ex.query_first_sql(sql, params).await?;
    match row.as_ref() {
        Some(row) => row.get_typed::<Decimal>("total"),
        None => Ok(Decimal::ZERO),
    }
}

/// 该客户有哪些币种的应收往来（作废单不算：它不进对账单）。
pub async fn customer_currencies(ex: &impl SqlExecutor, customer: Uuid) -> DbResult<Vec<String>> {
    // SCOPE-EXEMPT: 应收台账无归属列（见模块文档）。
    let sql = format!(
        "SELECT DISTINCT r.currency_code::text AS code FROM {AR_TABLE} r \
          WHERE r.customer_id = ?::uuid AND r.deleted_at IS NULL AND r.status <> ? \
          ORDER BY code"
    );
    let rows = ex
        .query_sql(
            sql,
            vec![
                Value::from(customer),
                Value::Text(crate::business::erp::receivable::STATUS_CANCELLED.to_string()),
            ],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(row.get_typed::<String>("code")?);
    }
    Ok(out)
}

/// 读客户展示信息（不存在 / 已删 → 404）。
pub async fn load_customer(
    ex: &impl SqlExecutor,
    customer: Uuid,
) -> DbResult<Option<StatementCustomerRow>> {
    // SCOPE-EXEMPT: 客户主数据（全局），无归属列。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT id, code, name, COALESCE(tax_no, '') AS tax_no FROM {CUSTOMER_TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![Value::from(customer)],
        )
        .await?;
    match row.as_ref() {
        Some(row) => Ok(Some(StatementCustomerRow {
            id: row.get_typed::<Uuid>("id")?,
            code: row.get_typed::<String>("code")?,
            name: row.get_typed::<String>("name")?,
            tax_no: row.get_typed::<String>("tax_no")?,
        })),
        None => Ok(None),
    }
}

// ==================== 组装入口（查询 / 导出 / 签发共用） ====================

/// 计算对账单（查询、导出、签发三条路径的唯一入口，口径不可能分叉）。
pub async fn build_statement(
    query: &StatementQuery,
) -> std::result::Result<StatementRes, ApiError> {
    let customer_id = parse_uuid_param(&query.customer_id, "customer_id")?;
    let today = crate::business::erp::voucher::today_cst(Utc::now());
    let (from, to) = parse_period(&query.period_from, query.period_to.as_deref(), today)?;
    let currency = resolve_currency(
        &customer_currencies(db(), customer_id)
            .await
            .map_err(|error| utils::db_error(error, "读取客户币种"))?,
        query.currency.as_deref(),
    )?;
    let customer = load_customer(db(), customer_id)
        .await
        .map_err(|error| utils::db_error(error, "读取客户"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                format!("客户 {customer_id} 不存在或已删除"),
            )
        })?;
    let events = load_events(db(), customer_id, &currency)
        .await
        .map_err(|error| utils::db_error(error, "读取对账事件"))?;
    let opening_by_ledger = sum_until(
        db(),
        customer_id,
        &currency,
        from.pred_opt().unwrap_or(from),
    )
    .await
    .map_err(|error| utils::db_error(error, "聚合期初余额"))?;
    let closing_by_ledger = sum_until(db(), customer_id, &currency, to)
        .await
        .map_err(|error| utils::db_error(error, "聚合期末余额"))?;
    assemble(StatementInputs {
        customer: StatementCustomerRes {
            customer_id: customer.id.to_string(),
            code: customer.code,
            name: customer.name,
            tax_no: customer.tax_no,
        },
        from,
        to,
        currency,
        events,
        opening_by_ledger,
        closing_by_ledger,
        generated_at: Utc::now(),
    })
}

/// 导出表：期初行 + 明细 + 期末行（脱离页面也能看懂：客户 / 期间 / 币种写在第一行摘要里）。
pub fn export_table(statement: &StatementRes) -> (Vec<&'static str>, Vec<HashMap<String, String>>) {
    let headers: Vec<&'static str> = EXPORT_HEADERS.to_vec();
    let mut rows: Vec<HashMap<String, String>> = Vec::with_capacity(statement.items.len() + 3);

    let mut head: HashMap<String, String> = HashMap::with_capacity(EXPORT_HEADERS.len());
    head.insert(
        "日期".to_string(),
        format!(
            "客户：{} {}",
            statement.customer.code, statement.customer.name
        ),
    );
    head.insert("类型".to_string(), "对账单".to_string());
    head.insert("单据号".to_string(), statement.customer.tax_no.clone());
    head.insert(
        "摘要".to_string(),
        format!(
            "期间：{} ~ {}（{} 天）；币种：{}",
            statement.period.from, statement.period.to, statement.period.days, statement.currency
        ),
    );
    head.insert("发生额".to_string(), String::new());
    head.insert("余额".to_string(), String::new());
    rows.push(head);

    let mut opening: HashMap<String, String> = HashMap::with_capacity(EXPORT_HEADERS.len());
    opening.insert("日期".to_string(), statement.period.from.to_string());
    opening.insert("类型".to_string(), "期初".to_string());
    opening.insert("单据号".to_string(), String::new());
    opening.insert("摘要".to_string(), "期初余额".to_string());
    opening.insert("发生额".to_string(), String::new());
    opening.insert("余额".to_string(), money_text(statement.opening_balance));
    rows.push(opening);

    for item in &statement.items {
        let mut row: HashMap<String, String> = HashMap::with_capacity(EXPORT_HEADERS.len());
        row.insert("日期".to_string(), item.date.to_string());
        row.insert("类型".to_string(), item.kind_label.clone());
        row.insert("单据号".to_string(), item.doc_no.clone());
        row.insert("摘要".to_string(), item.memo.clone());
        row.insert("发生额".to_string(), money_text(item.amount));
        row.insert("余额".to_string(), money_text(item.balance));
        rows.push(row);
    }

    let mut closing: HashMap<String, String> = HashMap::with_capacity(EXPORT_HEADERS.len());
    closing.insert("日期".to_string(), statement.period.to.to_string());
    closing.insert("类型".to_string(), "期末".to_string());
    closing.insert("单据号".to_string(), String::new());
    closing.insert(
        "摘要".to_string(),
        format!(
            "期末余额（期初 + 本期发生 = {} 笔）",
            statement.detail.item_count
        ),
    );
    closing.insert(
        "发生额".to_string(),
        money_text(statement.detail.net_change),
    );
    closing.insert("余额".to_string(), money_text(statement.closing_balance));
    rows.push(closing);
    (headers, rows)
}

// ==================== 接口层：查询 / 导出 ====================

/// 客户对账单。
///
/// `GET /business/erp/receivable/statement?customer_id=&period_from=&period_to=&currency=`
#[nik::permission(
    path = "/business/erp/receivable/statement",
    method = "GET",
    permission = "business:erp:statement:view"
)]
#[nik::get("/business/erp/receivable/statement")]
pub async fn statement_handler(Query(query): Query<StatementQuery>) -> ResResult<StatementRes> {
    Ok(Res::ok(build_statement(&query).await?))
}

/// 客户对账单导出 xlsx（复用 `impexp` 的写表能力，与利润表导出同一套写法）。
#[nik::permission(
    path = "/business/erp/receivable/statement/export",
    method = "GET",
    permission = "business:erp:statement:export"
)]
#[nik::get("/business/erp/receivable/statement/export")]
pub async fn statement_export_handler(
    Query(query): Query<StatementQuery>,
) -> std::result::Result<Response, ApiError> {
    let statement = build_statement(&query).await?;
    let (headers, rows) = export_table(&statement);
    let bytes = crate::business::impexp::engine::write_sheet(&headers, &rows)?;
    Ok(crate::business::impexp::handler::xlsx_response(
        bytes,
        &format!(
            "{EXPORT_PREFIX}_{}_{}_{}.xlsx",
            statement.customer.code,
            statement.period.from.format("%Y%m%d"),
            statement.period.to.format("%Y%m%d")
        ),
    ))
}

// ==================== 接口层：对账确认 ====================

/// 签发对账单（服务端重算并快照金额；`SENT` 可重发，`CONFIRMED`/`DISPUTED` 409）。
#[nik::permission(
    path = "/business/erp/receivable/statement/confirm",
    method = "POST",
    permission = "business:erp:statement:send"
)]
#[nik::post("/business/erp/receivable/statement/confirm")]
pub async fn statement_send_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<StatementSendReq>,
) -> ResResult<StatementConfirmRes> {
    let operator = utils::current_user_uuid(&claims);
    let statement = build_statement(&StatementQuery {
        customer_id: req.customer_id.clone(),
        period_from: req.period_from.clone(),
        period_to: req.period_to.clone(),
        currency: req.currency.clone(),
    })
    .await?;
    let customer_id = parse_uuid_param(&req.customer_id, "customer_id")?;
    let remark = req
        .remark
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let id = upsert_confirm(customer_id, &statement, remark.as_deref(), operator).await?;
    Ok(Res::ok(load_confirm(id).await?))
}

/// 客户确认（`SENT → CONFIRMED`）。
#[nik::permission(
    path = "/business/erp/receivable/statement/confirm/{id}/confirm",
    method = "PUT",
    permission = "business:erp:statement:confirm"
)]
#[nik::put("/business/erp/receivable/statement/confirm/{id}/confirm")]
pub async fn statement_confirm_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<StatementConfirmReq>,
) -> ResResult<StatementConfirmRes> {
    let operator = utils::current_user_uuid(&claims);
    let id = parse_uuid_param(&id, "id")?;
    let confirmed_by = validate_confirmed_by(&req.confirmed_by)?;
    let attachment = parse_optional_uuid(req.attachment_id.as_deref(), "attachment_id")?;
    let current = load_confirm(id).await?;
    ensure_pending(&current.status, "确认")?;
    let affected = db()
        .execute_with_params(
            &format!(
                "UPDATE {CONFIRM_TABLE} SET status = ?, confirmed_at = now(), confirmed_by = ?, \
                        attachment_id = COALESCE(NULLIF(?::text, '')::uuid, attachment_id), \
                        remark = COALESCE(NULLIF(?::text, ''), remark), \
                        updated_at = now(), update_by = NULLIF(?::text, '')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::Text(STATUS_CONFIRMED.to_string()),
                Value::Text(confirmed_by),
                Value::Text(attachment.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(
                    req.remark
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or_default()
                        .to_string(),
                ),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(id),
                Value::Text(STATUS_SENT.to_string()),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "确认对账单"))?;
    if affected == 0 {
        return Err(conflict(
            "该对账单状态已被改动（并发回执）：请刷新后重试".to_string(),
        ));
    }
    Ok(Res::ok(load_confirm(id).await?))
}

/// 客户提出异议（`SENT → DISPUTED`，必须给原因）。
#[nik::permission(
    path = "/business/erp/receivable/statement/confirm/{id}/dispute",
    method = "PUT",
    permission = "business:erp:statement:dispute"
)]
#[nik::put("/business/erp/receivable/statement/confirm/{id}/dispute")]
pub async fn statement_dispute_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<StatementDisputeReq>,
) -> ResResult<StatementConfirmRes> {
    let operator = utils::current_user_uuid(&claims);
    let id = parse_uuid_param(&id, "id")?;
    let reason = validate_dispute_reason(&req.dispute_reason)?;
    let attachment = parse_optional_uuid(req.attachment_id.as_deref(), "attachment_id")?;
    let current = load_confirm(id).await?;
    ensure_pending(&current.status, "提出异议")?;
    let affected = db()
        .execute_with_params(
            &format!(
                "UPDATE {CONFIRM_TABLE} SET status = ?, dispute_reason = ?, \
                        attachment_id = COALESCE(NULLIF(?::text, '')::uuid, attachment_id), \
                        updated_at = now(), update_by = NULLIF(?::text, '')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::Text(STATUS_DISPUTED.to_string()),
                Value::Text(reason),
                Value::Text(attachment.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(id),
                Value::Text(STATUS_SENT.to_string()),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "登记对账异议"))?;
    if affected == 0 {
        return Err(conflict(
            "该对账单状态已被改动（并发回执）：请刷新后重试".to_string(),
        ));
    }
    Ok(Res::ok(load_confirm(id).await?))
}

/// 对账确认台账分页。
#[nik::permission(
    path = "/business/erp/receivable/statement/confirm/page",
    method = "GET",
    permission = "business:erp:statement:view"
)]
#[nik::get("/business/erp/receivable/statement/confirm/page")]
pub async fn statement_confirm_page_handler(
    Query(query): Query<StatementConfirmPageQuery>,
) -> ResResult<StatementConfirmPageRes> {
    Ok(Res::ok(confirm_page(&query).await?))
}

// ==================== 对账确认台账（数据层） ====================

/// `biz_statement_confirm` 的列清单（含客户名称）。
const CONFIRM_COLUMNS: &str = "p.id, p.customer_id, COALESCE(c.name, '') AS customer_name, \
     p.period_from, p.period_to, p.currency_code::text AS currency_code, p.total_amount, \
     p.opening_balance, p.closing_balance, p.total_invoiced, p.total_received, p.total_returned, \
     p.item_count, p.status, p.sent_at, p.confirmed_at, p.confirmed_by, p.dispute_reason, \
     p.attachment_id, p.remark, p.created_at, p.updated_at";

/// 读一行台账（含客户名）。
fn confirm_from_row(row: &Row) -> DbResult<StatementConfirmRes> {
    let status: String = row.get_typed("status")?;
    Ok(StatementConfirmRes {
        id: row.get_typed::<Uuid>("id")?.to_string(),
        customer_id: row.get_typed::<Uuid>("customer_id")?.to_string(),
        customer_name: row.get_typed::<String>("customer_name")?,
        period_from: row.get_typed("period_from")?,
        period_to: row.get_typed("period_to")?,
        currency: row.get_typed("currency_code")?,
        total_amount: money(row.get_typed("total_amount")?),
        opening_balance: money(row.get_typed("opening_balance")?),
        closing_balance: money(row.get_typed("closing_balance")?),
        total_invoiced: money(row.get_typed("total_invoiced")?),
        total_received: money(row.get_typed("total_received")?),
        total_returned: money(row.get_typed("total_returned")?),
        item_count: row.get_typed("item_count")?,
        status_label: status_label(&status).to_string(),
        status,
        sent_at: row.get_typed("sent_at")?,
        confirmed_at: row.get_typed("confirmed_at")?,
        confirmed_by: row.get_typed("confirmed_by")?,
        dispute_reason: row.get_typed("dispute_reason")?,
        attachment_id: row
            .get_typed::<Option<Uuid>>("attachment_id")?
            .map(|value| value.to_string()),
        remark: row.get_typed("remark")?,
        created_at: row.get_typed("created_at")?,
        updated_at: row.get_typed("updated_at")?,
    })
}

/// 按 id 读一行台账（不存在 → 404）。
async fn load_confirm(id: Uuid) -> std::result::Result<StatementConfirmRes, ApiError> {
    // SCOPE-EXEMPT: 对账确认台账无归属列（见模块文档）。
    let sql = format!(
        "SELECT {CONFIRM_COLUMNS} FROM {CONFIRM_TABLE} p \
           LEFT JOIN {CUSTOMER_TABLE} c ON c.id = p.customer_id AND c.deleted_at IS NULL \
          WHERE p.id = ?::uuid AND p.deleted_at IS NULL"
    );
    let row = db()
        .query_first_sql(sql, vec![Value::from(id)])
        .await
        .map_err(|error| utils::db_error(error, "读取对账确认台账"))?;
    match row.as_ref() {
        Some(row) => {
            confirm_from_row(row).map_err(|error| utils::db_error(error, "读对账确认台账"))
        }
        None => Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            format!("对账确认记录 {id} 不存在或已删除"),
        )),
    }
}

/// 签发：同一 (客户, 期间, 币种) 已存在 `SENT` 则覆盖快照；已确认 / 已提异议 → 409。
async fn upsert_confirm(
    customer_id: Uuid,
    statement: &StatementRes,
    remark: Option<&str>,
    operator: Option<Uuid>,
) -> std::result::Result<Uuid, ApiError> {
    // SCOPE-EXEMPT: 对账确认台账无归属列（见模块文档）。
    let existing = db()
        .query_first_sql(
            format!(
                "SELECT id, status FROM {CONFIRM_TABLE} \
                  WHERE customer_id = ?::uuid AND period_from = ?::date AND period_to = ?::date \
                    AND currency_code = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(customer_id),
                Value::from(statement.period.from),
                Value::from(statement.period.to),
                Value::Text(statement.currency.clone()),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询已有对账确认记录"))?;
    if let Some(row) = existing.as_ref() {
        let id = row
            .get_typed::<Uuid>("id")
            .map_err(|error| utils::db_error(error, "读对账确认记录 id"))?;
        let status = row
            .get_typed::<String>("status")
            .map_err(|error| utils::db_error(error, "读对账确认状态"))?;
        ensure_pending(&status, "重新签发")?;
        db().execute_with_params(
            &format!(
                "UPDATE {CONFIRM_TABLE} SET total_amount = ?::text::numeric, \
                        opening_balance = ?::text::numeric, closing_balance = ?::text::numeric, \
                        total_invoiced = ?::text::numeric, total_received = ?::text::numeric, \
                        total_returned = ?::text::numeric, item_count = ?, sent_at = now(), \
                        remark = COALESCE(NULLIF(?::text, ''), remark), updated_at = now(), \
                        update_by = NULLIF(?::text, '')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::Text(statement.closing_balance.to_string()),
                Value::Text(statement.opening_balance.to_string()),
                Value::Text(statement.closing_balance.to_string()),
                Value::Text(statement.detail.total_invoiced.to_string()),
                Value::Text(statement.detail.total_received.to_string()),
                Value::Text(statement.detail.total_returned.to_string()),
                Value::I32(i32::try_from(statement.detail.item_count).unwrap_or(i32::MAX)),
                Value::Text(remark.unwrap_or_default().to_string()),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(id),
                Value::Text(STATUS_SENT.to_string()),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "重新签发对账单"))?;
        return Ok(id);
    }
    let row = db()
        .query_first_sql(
            format!(
                "INSERT INTO {CONFIRM_TABLE} \
                  (customer_id, period_from, period_to, currency_code, total_amount, \
                   opening_balance, closing_balance, total_invoiced, total_received, \
                   total_returned, item_count, status, sent_at, remark, create_by, update_by) \
                 VALUES (?::uuid, ?::date, ?::date, ?, ?::text::numeric, ?::text::numeric, \
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                         ?, '{STATUS_SENT}', now(), NULLIF(?::text, ''), \
                         NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid) \
                 RETURNING id"
            ),
            vec![
                Value::from(customer_id),
                Value::from(statement.period.from),
                Value::from(statement.period.to),
                Value::Text(statement.currency.clone()),
                Value::Text(statement.closing_balance.to_string()),
                Value::Text(statement.opening_balance.to_string()),
                Value::Text(statement.closing_balance.to_string()),
                Value::Text(statement.detail.total_invoiced.to_string()),
                Value::Text(statement.detail.total_received.to_string()),
                Value::Text(statement.detail.total_returned.to_string()),
                Value::I32(i32::try_from(statement.detail.item_count).unwrap_or(i32::MAX)),
                Value::Text(remark.unwrap_or_default().to_string()),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "签发对账单"))?;
    row.as_ref()
        .and_then(|row| row.get_typed::<Uuid>("id").ok())
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "签发对账单后没有拿到记录 id".to_string(),
            )
        })
}

/// 分页查询台账。
pub async fn confirm_page(
    query: &StatementConfirmPageQuery,
) -> std::result::Result<StatementConfirmPageRes, ApiError> {
    let (page, page_size) = normalize_page(query.page, query.page_size);
    let mut where_sql = String::from(" WHERE p.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(customer) = query
        .customer_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        let id = parse_uuid_param(customer, "customer_id")?;
        where_sql.push_str(" AND p.customer_id = ?::uuid");
        params.push(Value::from(id));
    }
    if let Some(status) = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_uppercase)
    {
        if !CONFIRM_STATUSES.contains(&status.as_str()) {
            return Err(bad_request(format!(
                "status 只认 {}（收到 {status}）",
                CONFIRM_STATUSES.join(" / ")
            )));
        }
        where_sql.push_str(" AND p.status = ?");
        params.push(Value::Text(status));
    }
    if let Some(raw) = query
        .period_from
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        let date = parse_date_param(raw, "period_from")?;
        where_sql.push_str(" AND p.period_from >= ?::date");
        params.push(Value::from(date));
    }
    if let Some(raw) = query
        .period_to
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        let date = parse_date_param(raw, "period_to")?;
        where_sql.push_str(" AND p.period_to <= ?::date");
        params.push(Value::from(date));
    }

    // SCOPE-EXEMPT: 对账确认台账无归属列（见模块文档）。
    let count_sql = format!("SELECT COUNT(*) AS total FROM {CONFIRM_TABLE} p{where_sql}");
    let count_row = db()
        .query_first_sql(count_sql, params.clone())
        .await
        .map_err(|error| utils::db_error(error, "统计对账确认台账"))?;
    let total: i64 = match count_row.as_ref() {
        Some(row) => row
            .get_typed("total")
            .map_err(|error| utils::db_error(error, "读对账确认台账总数"))?,
        None => 0,
    };

    let mut list_params = params;
    list_params.push(Value::I64(i64::try_from(page_size).unwrap_or(i64::MAX)));
    list_params.push(Value::I64(
        i64::try_from((page - 1) * page_size).unwrap_or(0),
    ));
    let list_sql = format!(
        "SELECT {CONFIRM_COLUMNS} FROM {CONFIRM_TABLE} p \
           LEFT JOIN {CUSTOMER_TABLE} c ON c.id = p.customer_id AND c.deleted_at IS NULL \
           {where_sql} \
          ORDER BY p.period_from DESC, p.period_to DESC, p.created_at DESC \
          LIMIT ? OFFSET ?"
    );
    let rows = db()
        .query_sql(list_sql, list_params)
        .await
        .map_err(|error| utils::db_error(error, "查询对账确认台账"))?;
    let mut items = Vec::with_capacity(rows.len());
    for row in &rows {
        items
            .push(confirm_from_row(row).map_err(|error| utils::db_error(error, "读对账确认台账"))?);
    }
    Ok(StatementConfirmPageRes {
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
        items,
    })
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

    fn event(day: &str, kind: &'static str, doc_no: &str, amount: Decimal) -> StatementEvent {
        StatementEvent {
            date: date(day),
            rank: match kind {
                KIND_INVOICE => 1,
                KIND_RETURN => 2,
                KIND_RECEIPT => 3,
                KIND_REFUND => 4,
                _ => 5,
            },
            kind: kind.to_string(),
            doc_no: doc_no.to_string(),
            memo: String::new(),
            amount,
        }
    }

    fn customer() -> StatementCustomerRes {
        StatementCustomerRes {
            customer_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string(),
            code: "CUS-000001".to_string(),
            name: "北京测试客户".to_string(),
            tax_no: "91110000TEST".to_string(),
        }
    }

    /// ① 事件排序：日期 → 类型权重 → 单据号（同一天先挂账后冲减）。
    #[test]
    fn events_sort_by_date_then_kind_rank() {
        let mut events = vec![
            event(
                "2026-10-01",
                KIND_RECEIPT,
                "RC-20261001-000001",
                dec("-100"),
            ),
            event("2026-10-01", KIND_INVOICE, "AR-20261001-000002", dec("500")),
            event("2026-09-30", KIND_INVOICE, "AR-20260930-000001", dec("900")),
            event("2026-10-01", KIND_INVOICE, "AR-20261001-000001", dec("300")),
        ];
        sort_events(&mut events);
        let docs: Vec<&str> = events.iter().map(|item| item.doc_no.as_str()).collect();
        assert_eq!(
            docs,
            [
                "AR-20260930-000001",
                "AR-20261001-000001",
                "AR-20261001-000002",
                "RC-20261001-000001"
            ],
            "同一天挂账必须排在收款之前"
        );
    }

    /// ② 逐笔余额：期初 + 每笔 = running balance；期末 = 期初 + Σ 明细。
    #[test]
    fn running_balance_accumulates_per_item() {
        let events = vec![
            event("2026-09-20", KIND_INVOICE, "AR-1", dec("1000")),
            event("2026-10-05", KIND_INVOICE, "AR-2", dec("500")),
            event("2026-10-08", KIND_RECEIPT, "RC-1", dec("-300")),
            event("2026-10-12", KIND_RETURN, "SR-1", dec("-120")),
        ];
        let opening = sum_before(&events, date("2026-10-01"));
        assert_eq!(money(opening), dec("1000.00"));
        let items = build_items(opening, &events, date("2026-10-01"), date("2026-10-31"));
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].seq, 1);
        assert_eq!(items[0].amount, dec("500.00"));
        assert_eq!(items[0].balance, dec("1500.00"));
        assert_eq!(items[1].balance, dec("1200.00"));
        assert_eq!(items[2].balance, dec("1080.00"));
        let closing = opening + items.iter().map(|item| item.amount).sum::<Decimal>();
        assert_eq!(money(closing), dec("1080.00"));
        // 期间外的挂账不进明细，但计入期初
        assert!(items.iter().all(|item| item.doc_no != "AR-1"));
        // 发生额汇总：挂账 500 / 收款 300 / 退货 120
        let (invoiced, received, returned, refunded, written) = summarise(&items);
        assert_eq!(invoiced, dec("500.00"));
        assert_eq!(received, dec("300.00"));
        assert_eq!(returned, dec("120.00"));
        assert_eq!(refunded, Decimal::ZERO);
        assert_eq!(written, Decimal::ZERO);
    }

    /// ③ 自校验：两条路径一致 → passed；台账路径差 1 分以内也算过，差 1 毛必须红。
    #[test]
    fn identity_check_catches_divergence() {
        let items = build_items(
            dec("1000.00"),
            &[
                event("2026-10-05", KIND_INVOICE, "AR-2", dec("500")),
                event("2026-10-08", KIND_RECEIPT, "RC-1", dec("-300")),
            ],
            date("2026-10-01"),
            date("2026-10-31"),
        );
        let ok = build_identity(dec("1000"), dec("1000"), &items, dec("1200"));
        assert!(ok.passed, "期初/期末两条路径一致应通过");
        assert_eq!(ok.closing_balance, dec("1200.00"));
        assert_eq!(ok.diff, Decimal::ZERO);

        // 容差内（1 分）通过
        let edge = build_identity(dec("1000"), dec("1000"), &items, dec("1200.01"));
        assert!(edge.passed, "1 分以内属容差");

        // 差 1 毛 → 必须红（宁可 500 也不出对不上的对账单）
        let broken = build_identity(dec("1000"), dec("1000"), &items, dec("1200.10"));
        assert!(!broken.passed);
        assert_eq!(broken.diff, dec("0.10"));

        // 期初不一致同样要红
        let opening_broken = build_identity(dec("1000"), dec("999.00"), &items, dec("1200"));
        assert!(!opening_broken.passed);
        assert_eq!(opening_broken.diff, dec("1.00"));
    }

    /// ④ 组装：自校验不过 → 500（不返回对账单）；通过 → 字段齐全、笔数正确。
    #[test]
    fn assemble_rejects_broken_identity() {
        let events = vec![
            event("2026-09-15", KIND_INVOICE, "AR-1", dec("1000")),
            event("2026-10-05", KIND_INVOICE, "AR-2", dec("500")),
        ];
        let inputs = StatementInputs {
            customer: customer(),
            from: date("2026-10-01"),
            to: date("2026-10-31"),
            currency: "CNY".to_string(),
            events: events.clone(),
            opening_by_ledger: dec("1000"),
            closing_by_ledger: dec("1500"),
            generated_at: Utc::now(),
        };
        let statement = assemble(inputs).expect("自校验通过应成功");
        assert_eq!(statement.opening_balance, dec("1000.00"));
        assert_eq!(statement.closing_balance, dec("1500.00"));
        assert_eq!(statement.detail.net_change, dec("500.00"));
        assert_eq!(statement.period.days, 31);
        assert_eq!(statement.detail.item_count, 1);
        assert!(statement.detail.identity.passed);

        let broken = StatementInputs {
            customer: customer(),
            from: date("2026-10-01"),
            to: date("2026-10-31"),
            currency: "CNY".to_string(),
            events,
            opening_by_ledger: dec("1000"),
            closing_by_ledger: dec("1400"),
            generated_at: Utc::now(),
        };
        let error = assemble(broken).expect_err("期末对不上必须 500");
        assert_eq!(error.status, 500);
        assert!(
            error.msg.contains("自校验失败"),
            "报错要说清是对账单自校验失败，实际：{}",
            error.msg
        );
    }

    /// ⑤ 期间校验：倒置 400、缺起日 400、非法日期 400、缺省止日 = 今天。
    #[test]
    fn period_validation() {
        let today = date("2026-11-07");
        assert_eq!(
            parse_period("2026-10-01", None, today).unwrap(),
            (date("2026-10-01"), today)
        );
        assert_eq!(
            parse_period("2026-10-01", Some("2026-10-31"), today).unwrap(),
            (date("2026-10-01"), date("2026-10-31"))
        );
        assert_eq!(
            parse_period("2026-10-01", Some("   "), today).unwrap().1,
            today,
            "空白串当没传"
        );
        for bad in [
            ("", None),
            ("2026-10-31", Some("2026-10-01")),
            ("2026-13-01", None),
            ("2026/10/01", None),
        ] {
            let error = parse_period(bad.0, bad.1, today).expect_err("非法期间必须 400");
            assert_eq!(error.status, 400, "{} / {:?}", bad.0, bad.1);
        }
        assert!(
            parse_period("2026-10-31", Some("2026-10-01"), today)
                .unwrap_err()
                .msg
                .contains("不能倒置")
        );
    }

    /// ⑥ 币种解析：单一自动取 / 多币种必须指定 / 指定不存在 400 / 无往来默认 CNY。
    #[test]
    fn currency_resolution() {
        let one = vec!["CNY".to_string()];
        assert_eq!(resolve_currency(&one, None).unwrap(), "CNY");
        assert_eq!(
            resolve_currency(&one, Some("cny")).unwrap(),
            "CNY",
            "大小写不敏感"
        );

        let multi = vec!["CNY".to_string(), "USD".to_string()];
        let error = resolve_currency(&multi, None).expect_err("多币种必须显式指定");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("CNY"), "要把可选币种列出来");
        assert!(error.msg.contains("USD"));
        assert_eq!(resolve_currency(&multi, Some("USD")).unwrap(), "USD");

        let error =
            resolve_currency(&multi, Some("EUR")).expect_err("没有的币种要 400，不能返回空单");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("EUR"));

        assert_eq!(
            resolve_currency(&[], None).unwrap(),
            "CNY",
            "零往来给 CNY 空单"
        );
        assert_eq!(
            resolve_currency(&[], Some("CNY"))
                .expect_err("零往来不能指定币种")
                .status,
            400
        );
    }

    /// ⑦ 状态闸：只有 SENT 能确认 / 提异议；终态 409 且文案点名当前状态。
    #[test]
    fn pending_status_gate() {
        assert!(ensure_pending(STATUS_SENT, "确认").is_ok());
        for terminal in [STATUS_CONFIRMED, STATUS_DISPUTED] {
            let error = ensure_pending(terminal, "确认").expect_err("终态不能再改");
            assert_eq!(error.status, 409);
            assert!(
                error.msg.contains(status_label(terminal)),
                "报错要带当前状态中文，实际：{}",
                error.msg
            );
        }
    }

    /// ⑧ 回执入参：确认人 ≥ 2 字、异议原因 ≥ 4 字、附件 id 可空但要合法。
    #[test]
    fn receipt_validation() {
        assert_eq!(validate_confirmed_by("  张三 ").unwrap(), "张三");
        assert_eq!(
            validate_confirmed_by("张").expect_err("1 个字太短").status,
            400
        );
        assert_eq!(
            validate_confirmed_by("   ").expect_err("空要 400").status,
            400
        );

        assert_eq!(validate_dispute_reason("金额差 200").unwrap(), "金额差 200");
        assert_eq!(validate_dispute_reason("差").expect_err("太短").status, 400);

        assert_eq!(parse_optional_uuid(None, "attachment_id").unwrap(), None);
        assert_eq!(
            parse_optional_uuid(Some("  "), "attachment_id").unwrap(),
            None
        );
        assert!(
            parse_optional_uuid(
                Some("0b9d5c62-0000-4000-8000-000000000001"),
                "attachment_id"
            )
            .unwrap()
            .is_some()
        );
        assert_eq!(
            parse_optional_uuid(Some("not-a-uuid"), "attachment_id")
                .expect_err("非法 uuid 400")
                .status,
            400
        );
    }

    /// ⑨ 导出表：期初行 + 明细 + 期末行；余额列与接口返回逐笔一致，金额不带千分位。
    #[test]
    fn export_table_layout() {
        let events = vec![
            event("2026-09-15", KIND_INVOICE, "AR-1", dec("1000")),
            event("2026-10-05", KIND_INVOICE, "AR-2", dec("1234.5")),
            event("2026-10-08", KIND_RECEIPT, "RC-1", dec("-234.5")),
        ];
        let statement = assemble(StatementInputs {
            customer: customer(),
            from: date("2026-10-01"),
            to: date("2026-10-31"),
            currency: "CNY".to_string(),
            events,
            opening_by_ledger: dec("1000"),
            closing_by_ledger: dec("2000"),
            generated_at: Utc::now(),
        })
        .expect("自校验通过");
        let (headers, rows) = export_table(&statement);
        assert_eq!(headers, EXPORT_HEADERS.to_vec());
        assert_eq!(
            rows.len(),
            statement.items.len() + 3,
            "信息行 + 期初 + 明细 + 期末"
        );

        let text =
            |row: &HashMap<String, String>, key: &str| row.get(key).cloned().unwrap_or_default();
        assert!(text(&rows[0], "日期").contains("CUS-000001"));
        assert!(text(&rows[0], "摘要").contains("2026-10-01 ~ 2026-10-31"));
        assert_eq!(text(&rows[1], "类型"), "期初");
        assert_eq!(text(&rows[1], "余额"), "1000.00");
        assert_eq!(text(&rows[2], "类型"), "挂账");
        assert_eq!(text(&rows[2], "发生额"), "1234.50", "金额不带千分位");
        assert_eq!(text(&rows[2], "余额"), "2234.50");
        assert_eq!(text(&rows[3], "发生额"), "-234.50", "冲减是负数");
        let last = rows.last().expect("有期末行");
        assert_eq!(text(last, "类型"), "期末");
        assert_eq!(text(last, "余额"), "2000.00");
        assert_eq!(text(last, "发生额"), "1000.00", "本期发生 = 期末 − 期初");
    }

    /// ⑩ 事件视图 SQL：五个分支都在、作废单剔除、金额口径（挂账加回已过账退货）。
    #[test]
    fn events_cte_shape() {
        let cte = events_cte();
        for table in [
            AR_TABLE,
            RETURN_TABLE,
            RECEIPT_TABLE,
            ALLOC_TABLE,
            REFUND_TABLE,
            REFUND_ALLOC_TABLE,
        ] {
            assert!(cte.contains(table), "事件视图缺 {table} 分支");
        }
        for kind in KINDS {
            assert!(cte.contains(kind), "事件视图缺 {kind} 事件");
        }
        assert!(
            cte.contains("(r.amount + COALESCE(ret.amt, 0))"),
            "挂账金额必须加回已过账退货（退货是原地扣减 amount 的）"
        );
        assert_eq!(
            cte.matches("r.status <> 'CANCELLED'").count(),
            4,
            "挂账 / 退货 / 收款 / 退款四路都要剔除作废应收（坏账那路只取 WRITTEN_OFF）"
        );
        assert!(
            cte.contains("r.status = 'WRITTEN_OFF'"),
            "坏账核销事件只认 WRITTEN_OFF"
        );
        assert!(cte.contains("sr.status = 'POSTED'"), "只认已过账退货");
        assert!(!cte.contains("'DRAFT'"), "草稿退货/收款不许进对账单");
        // 参数个数与五个分支对齐（每分支 customer + currency）
        assert_eq!(event_params(Uuid::nil(), "CNY").len(), 10);
    }

    /// ⑪ 类型 / 状态中文名全覆盖（前后端文案唯一来源）。
    #[test]
    fn labels_are_complete() {
        for kind in KINDS {
            assert_ne!(kind_label(kind), "其他", "{kind} 缺中文名");
        }
        for status in CONFIRM_STATUSES {
            assert_ne!(status_label(status), "未知", "{status} 缺中文名");
        }
        assert_eq!(KINDS.len(), 5);
        assert_eq!(CONFIRM_STATUSES, ["SENT", "CONFIRMED", "DISPUTED"]);
    }

    /// ⑫ 金额标度：零金额也必须序列化成 `0.00`（API 契约不能时而 `0` 时而 `0.00`）。
    #[test]
    fn money_keeps_two_decimals_for_zero() {
        assert_eq!(money(Decimal::ZERO).to_string(), "0.00");
        assert_eq!(money(dec("0.004")).to_string(), "0.00");
        assert_eq!(money(dec("1234.5")).to_string(), "1234.50");
        assert_eq!(money(dec("-234.5678")).to_string(), "-234.57");
        assert_eq!(money_text(Decimal::ZERO), "0.00");
        assert_eq!(money_text(dec("11500")), "11500.00");
        // 空事件流的合计 = ZERO，经过 money() 后仍是两位小数
        let empty = sum_before(&[], date("2026-10-01"));
        assert_eq!(money(empty).to_string(), "0.00");
    }
}
