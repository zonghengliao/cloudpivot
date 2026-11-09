//! 销售发票（销售链 · SO-20）：**开票 → 收入凭证 → 回写应收 → 归档 → 作废（红冲）**。
//!
//! 表结构见 `backend/migrations/business/V0298__sales_invoice.up.sql`（SO-19）：
//!
//! ```text
//! biz_invoice  id, invoice_no(IV-yyyyMMdd-000001 或手填税控号), invoice_type(OUT/OUT_RED),
//!              invoice_code, is_electronic(电子票/纸质票，V0348), customer_id(弱引用),
//!              invoice_date, amount(不含税),
//!              tax_amount, total_with_tax, tax_rate, status(DRAFT/ISSUED/POSTED/VOID),
//!              receivable_id(FK RESTRICT，可空), source_type, source_no, voucher_id, remark
//!              + 审计五件套
//!              CHECK total_with_tax = amount + tax_amount          (ck_biz_invoice_amount)
//!              CHECK OUT: amount>0 且 tax>=0 / OUT_RED: amount<0 且 tax<=0 (ck_biz_invoice_sign)
//!              UNIQUE (invoice_no) WHERE deleted_at IS NULL
//! ```
//!
//! `is_electronic`（**ATT-07**，迁移 V0348）是票面形式：`true` 电子票 / `false` 纸质票。
//! 它不只用于展示 —— 「开票（DRAFT→ISSUED）」时的附件必传校验按它分支：
//! 电子票只认 pdf/ofd/xml（数电票），纸质票认 jpg/png/pdf（扫描件）。
//! 规则本身在 `biz_attachment_rule`（`condition_sql = 'is_electronic IS TRUE|FALSE'`）。
//!
//! # 接口（ARCH-7 §5.6 的 4 个）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|
//! | POST | `/business/erp/invoice` | `business:erp:invoice:create` | 开票（从应收 / 手工），落 `DRAFT` |
//! | GET | `/business/erp/invoice/{id}` | `business:erp:invoice:detail` | 详情（客户名 / 应收 / 凭证号） |
//! | GET | `/business/erp/invoice/page` | `business:erp:invoice:list` | 分页 |
//! | PATCH | `/business/erp/invoice/{id}/status` | `business:erp:invoice:status` | 状态迁移（ISSUED / POSTED / VOID） |
//!
//! # 状态机（ARCH-7 §4.5，边表在 `state_machine::INVOICE_EDGES`）
//!
//! ```text
//! DRAFT ──开票──▶ ISSUED ──归档──▶ POSTED
//!   │                │
//!   └────作废────────┴──▶ VOID（红字凭证）
//! ```
//!
//! - **`ISSUED` 是「开票」这一步**：生成收入凭证（借 1122 应收账款，贷 5001 主营业务收入
//!   与 2221.02 应交税费-销项税），并回写 `biz_receivable.invoice_no / invoice_date`。
//!   凭证挂在这一步而不是 `POSTED`，是因为 **ARCH-7 §4.5 只允许 `DRAFT/ISSUED → VOID`**：
//!   若凭证到 `POSTED` 才生成，`VOID` 永远没有「可红冲的蓝字凭证」，
//!   卡面「作废时生成红字凭证」就无从落地。
//! - **`POSTED` 是归档**（发票已交付 / 已报税留档），不产生第二张凭证，终态。
//! - **`VOID`**：`DRAFT → VOID` 时账上什么都没动（没有凭证 ⇒ 不红冲），
//!   `ISSUED → VOID` 时红冲收入凭证（`voucher::reverse_posted`，红字凭证 + 原凭证打
//!   `reversed_by`），并把应收的 `invoice_no / invoice_date` **按号撤掉**
//!   （只在它确实指回本发票时才清，别把别人的发票号擦了）；撤完再把指针**改指**
//!   本应收下仍在生效的最新一张票（[`surviving_pointer`]）—— 一张应收分次开票、
//!   作废其中一张时，另几张的线索不能跟着丢。
//!
//! # 「开票金额不能超过应收金额」（跨表条件，库层无 TRIGGER）
//!
//! `ck_biz_invoice_*` 只能管单行自洽，管不了「这批发票加起来别超过那张应收」。
//! 所以本模块在**同一事务**里：
//!
//! ```text
//! ① SELECT … FOR UPDATE 锁应收（锁序：发票头 → 应收，与收款核销相反方向，不会成环）
//! ② 已开票合计 = SUM(total_with_tax) WHERE receivable_id = ? AND status <> 'VOID'
//!                （作废的票不再占用额度；排除自身，防「改单」把自己算两遍）
//! ③ 本次价税合计 ≤ 应收金额 - 已开票合计，超了 409 并点名三个数字
//! ④ ISSUED 时**再判一次**（从建单到开票之间可能有人抢开），同一把锁串行化
//! ```
//!
//! 手工开票（无 `receivable_id`）没有应收可比，`receivable_id` 留空即可。
//!
//! # 金额三列的口径
//!
//! - **税率是百分数**（FIX-6 · V0327）：`tax_rate = 13` 表示 13%，与
//!   `biz_sales_order_item` / `biz_stock_in_item` 同口径；历史的小数入参（`0.13`）由
//!   [`tax_rate`] 的过渡期规则自动 ×100 并打 WARN，实际采用的标度落 `tax_rate_mode`。
//! - 用户可给 `amount`（不含税）、`tax_amount`、`total_with_tax`、`tax_rate` 的**任意组合**，
//!   缺的由已知量推：
//!   `tax = round(amount × rate / 100, 4)`、`total = amount + tax`、
//!   `amount = round(total / (1 + rate/100), 4)`；
//! - 三列**都给了**就必须自洽（`total == amount + tax`，误差 0，四位小数内逐分相等），
//!   否则 400 —— 别等库层 `ck_biz_invoice_amount` 抛 23514（报错只给约束名，看不出是哪个数错）。
//! - 从应收开票时 `total_with_tax` 缺省 = **应收金额全额**（卡面「自动带出金额」）。
//!
//! # 科目（ARCH-7 §8.1，按仓库实际科目表落地）
//!
//! 稿面写 `6001 主营业务收入`，但本仓 `biz_account` 的损益类科目是 `5xxx`
//! （`V0183` 种的是 **`5001 主营业务收入`**，`6001` 在科目表里不存在；`6401` 才是
//! 主营业务成本）。用 6001 会让 `account_required` 直接报「科目不存在」。
//! ⇒ 收入科目取 **`5001`**，销项税取 **`2221.02`**（叶子科目，`2221` 是汇总科目不许记账）。
//!
//! # 事务边界
//!
//! 每个写入口（create / status）自己开事务：发票行、应收回写、凭证、状态日志四者同生共死。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_invoice 是销售台账（V0298 建表，无 dept_id / 归属人列），
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:invoice:* 权限码控制。
//! SCOPE-EXEMPT: biz_receivable / biz_customer / biz_voucher / biz_seq 均无归属列。
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
use crate::business::erp::currency;
use crate::business::erp::form_snapshot;
use crate::business::erp::model::normalize_page;
use crate::business::erp::period;
use crate::business::erp::receivable::{self, ReceivableRow};
use crate::business::erp::tax_rate as tax_rate_mod;
use crate::business::erp::voucher::{self, ReversalInput, SeqSpec};
use crate::business::shared::doc_kind::VoucherSourceType;
use crate::business::shared::scope;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0299 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 / 分页。
pub const PERM_LIST: &str = "business:erp:invoice:list";
/// 详情。
pub const PERM_DETAIL: &str = "business:erp:invoice:detail";
/// 开票（新建）。
pub const PERM_CREATE: &str = "business:erp:invoice:create";
/// 状态迁移（开票确认 / 归档 / 作废）。
pub const PERM_STATUS: &str = "business:erp:invoice:status";

// ==================== 常量 ====================

/// 发票表。
pub const TABLE: &str = "biz_invoice";
/// 客户主数据表。
pub const CUSTOMER_TABLE: &str = "biz_customer";

/// 状态：草稿（已取号 / 已校验金额，账上没动）。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：已开票（收入凭证已生成 + 应收已回写）。
pub const STATUS_ISSUED: &str = "ISSUED";
/// 状态：已归档（终态，不再产生凭证）。
pub const STATUS_POSTED: &str = "POSTED";
/// 状态：已作废（终态，ISSUED 来的带红字凭证）。
pub const STATUS_VOID: &str = "VOID";

/// 状态白名单（与 `ck_biz_invoice_status` 逐字一致，单测钉住）。
pub const STATUSES: [&str; 4] = [STATUS_DRAFT, STATUS_ISSUED, STATUS_POSTED, STATUS_VOID];

/// 票种：销项正票。
pub const TYPE_OUT: &str = "OUT";
/// 票种：销项红字票（金额为负，由退货流程生成，本卡不开放手工创建）。
pub const TYPE_OUT_RED: &str = "OUT_RED";

/// 票种白名单（与 `ck_biz_invoice_type` 逐字一致）。
pub const TYPES: [&str; 2] = [TYPE_OUT, TYPE_OUT_RED];

/// 来源类型白名单（与 `ck_biz_invoice_source` 逐字一致）。
pub const SOURCE_TYPES: [&str; 4] = ["SALES_ORDER", "SHIPMENT", "SALES_OUT", "MANUAL"];

/// 默认税率 13%（百分数口径，FIX-6 起 `biz_invoice.tax_rate` 存 `13.0000`）。
pub fn default_tax_rate() -> Decimal {
    Decimal::from(13)
}

/// 发票号前缀 / `biz_seq` 号段（SO-19 已种 `seq_type='IV'`）。
pub const NO_PREFIX: &str = "IV";
/// 取号类型。
pub const SEQ_TYPE: &str = "IV";

/// 发票取号规则：`IV-yyyyMMdd-000001`（按 CST 天滚动，六位补零由 model 决定）。
pub const INVOICE_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: SEQ_TYPE,
    prefix: NO_PREFIX,
    seq_name: "发票号",
    remark: "发票号：IV-yyyyMMdd-000001，按天（CST）滚动；手填税控号时不取号",
};

/// 收入凭证借方：应收账款（唯一真相在 [`voucher::ACCOUNT_AR`]）。
pub const ACCOUNT_AR: &str = voucher::ACCOUNT_AR;
/// 收入凭证贷方：主营业务收入（V0183 的损益类科目是 5xxx，稿面 6001 不存在）。
///
/// 唯一真相在 [`voucher::ACCOUNT_REVENUE`]（SO-22 把收入确认凭证的科目收口到 `voucher`）。
pub const ACCOUNT_REVENUE: &str = voucher::ACCOUNT_REVENUE;
/// 收入凭证贷方：应交税费-销项税（叶子科目；2221 是汇总科目，记账会被 `post_voucher` 拒）。
pub const ACCOUNT_OUTPUT_TAX: &str = voucher::ACCOUNT_OUTPUT_TAX;

/// 凭证来源（`biz_voucher.source_type`）。
pub const SOURCE_INVOICE: &str = VoucherSourceType::Invoice.as_str();

/// 状态机实体（`biz_status_log.entity_type`）。
pub const ENTITY: &str = state_machine::ENTITY_INVOICE;

/// 合法边（唯一真相在 `state_machine`）。
pub const LEGAL_EDGES: [(&str, &str); 4] = state_machine::INVOICE_EDGES;

/// 入账态。
pub const LOCKED: [&str; 3] = state_machine::INVOICE_LOCKED;

/// 作废原因最短字数。
pub const REASON_MIN_CHARS: usize = 4;
/// 作废原因最长字数。
pub const REASON_MAX_CHARS: usize = 500;
/// 发票号最长（与 `invoice_no VARCHAR(64)` 一致）。
pub const NO_MAX_CHARS: usize = 64;
/// 发票代码最长（与 `invoice_code VARCHAR(32)` 一致）。
pub const CODE_MAX_CHARS: usize = 32;

/// `biz_invoice` 列清单（与 V0298 + V0314 + V0348 逐列对应）。
pub const COLUMNS: &str = "id, invoice_no, invoice_type, invoice_code, is_electronic, customer_id, invoice_date, \
     amount, tax_amount, total_with_tax, tax_rate, tax_rate_mode, status, receivable_id, source_type, source_no, \
     voucher_id, remark, \
     currency_code, exchange_rate, base_total_amount, base_total_tax, base_total_with_tax, \
     created_at, updated_at";

/// 列表列（带客户名，别名 `p` / `c`）。
pub const LIST_COLUMNS: &str = "p.id, p.invoice_no, p.invoice_type, p.invoice_code, p.is_electronic, p.customer_id, \
     COALESCE(c.name, '') AS customer_name, p.invoice_date, p.amount, p.tax_amount, \
     p.total_with_tax, p.tax_rate, p.tax_rate_mode, p.status, p.receivable_id, p.source_type, p.source_no, \
     p.voucher_id, p.remark, p.currency_code, p.exchange_rate, p.base_total_amount, \
     p.base_total_tax, p.base_total_with_tax, p.created_at, p.updated_at";

/// 列表 FROM 子句。
pub const LIST_FROM: &str = "FROM biz_invoice p \
     LEFT JOIN biz_customer c ON c.id = p.customer_id AND c.deleted_at IS NULL";

/// 列表排序：最新开票在前，同一时刻按发票号倒序（稳定）。
pub const LIST_ORDER: &str = "ORDER BY p.invoice_date DESC, p.created_at DESC, p.invoice_no DESC";

/// 分页目标（`unscoped`：销售台账无归属列，理由见模块文档的 `SCOPE-EXEMPT`）。
pub const LIST_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

// ==================== 纯逻辑（金额 / 校验 / 状态） ====================

/// 状态中文（错误文案 / 前端 tag 的唯一来源）。
pub fn status_label(status: &str) -> &str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_ISSUED => "已开票",
        STATUS_POSTED => "已归档",
        STATUS_VOID => "已作废",
        _ => "未知状态",
    }
}

/// 票种中文。
pub fn type_label(invoice_type: &str) -> &str {
    match invoice_type {
        TYPE_OUT => "销项正票",
        TYPE_OUT_RED => "销项红字票",
        _ => "未知票种",
    }
}

/// 是否还能继续做账务动作（只有 DRAFT / ISSUED 可以）。
pub fn is_actionable(status: &str) -> bool {
    matches!(status, STATUS_DRAFT | STATUS_ISSUED)
}

/// 由不含税金额与税率（**百分数**）算税额（四位小数定标，与 `ck_biz_invoice_amount` 同精度）。
pub fn tax_of(amount: Decimal, tax_rate: Decimal) -> Decimal {
    tax_rate_mod::tax_of(amount, tax_rate)
}

/// 价税合计 = 不含税 + 税额。
pub fn total_of(amount: Decimal, tax_amount: Decimal) -> Decimal {
    (amount + tax_amount).round_dp(4)
}

/// 由价税合计与税率（**百分数**）倒推 `(不含税, 税额)`。
///
/// 先定不含税（`total / (1 + rate/100)` 四位定标）、再让税额 = 合计 - 不含税，
/// 这样**两者相加恒等于合计**（反过来先定税额会有 1 分尾差，撞 `ck_biz_invoice_amount`）。
pub fn split_tax(total_with_tax: Decimal, tax_rate: Decimal) -> (Decimal, Decimal) {
    tax_rate_mod::split(total_with_tax, tax_rate)
}

/// 税额是否与「不含税 × 税率」自洽（1 分以内即视为自洽，手工票允许四舍五入差）。
pub fn tax_is_consistent(amount: Decimal, tax_amount: Decimal, tax_rate: Decimal) -> bool {
    let expected = tax_of(amount, tax_rate);
    let diff = (tax_amount - expected).abs();
    diff <= Decimal::new(1, 2)
}

/// 税率校验（**百分数** `0 ~ 100`，`13` = 13%），并定标到四位。
///
/// FIX-6 起 `biz_invoice.tax_rate` 与订单 / 收货明细同口径。入参归一（含过渡期小数兼容）
/// 走 [`tax_rate::normalize_with_mode`]；本函数只做「已是百分数」的区间判定。
pub fn validate_tax_rate(rate: Decimal) -> std::result::Result<Decimal, ApiError> {
    if rate < Decimal::ZERO || rate > tax_rate_mod::PERCENT_MAX {
        return Err(bad_request(format!(
            "tax_rate 必须落在 0~100（百分数，13 表示 13%），收到 {rate}"
        )));
    }
    Ok(rate.round_dp(4))
}

/// 价税合计校验：`> 0` 且不超过 `NUMERIC(18,4)` 的整数上限。
pub fn validate_total(total: Decimal) -> std::result::Result<Decimal, ApiError> {
    if total <= Decimal::ZERO {
        return Err(bad_request(format!(
            "价税合计必须大于 0（零元发票没有业务含义），收到 {total}"
        )));
    }
    if total >= Decimal::from(10_000_000_000_000i64) {
        return Err(bad_request(format!(
            "价税合计超出 NUMERIC(18,4) 上限（整数部分最多 14 位），收到 {total}"
        )));
    }
    Ok(total.round_dp(4))
}

/// 金额符号规则（`ck_biz_invoice_sign` 的 Rust 镜像，先在本层拦，报错才说得清）。
pub fn validate_sign(invoice_type: &str, amount: Decimal, tax_amount: Decimal) -> std::result::Result<(), ApiError> {
    match invoice_type {
        TYPE_OUT => {
            if amount <= Decimal::ZERO {
                return Err(bad_request(format!(
                    "销项正票（OUT）的不含税金额必须大于 0（红冲请用 OUT_RED），收到 {amount}"
                )));
            }
            if tax_amount < Decimal::ZERO {
                return Err(bad_request(format!("销项正票（OUT）的税额不能为负，收到 {tax_amount}")));
            }
            Ok(())
        }
        TYPE_OUT_RED => {
            if amount >= Decimal::ZERO {
                return Err(bad_request(format!(
                    "红字票（OUT_RED）的不含税金额必须为负（ARCH-7 §15.2.3），收到 {amount}"
                )));
            }
            if tax_amount > Decimal::ZERO {
                return Err(bad_request(format!(
                    "红字票（OUT_RED）的税额必须 ≤ 0，收到 {tax_amount}"
                )));
            }
            Ok(())
        }
        other => Err(bad_request(format!("invoice_type 只认 {TYPES:?}，收到「{other}」"))),
    }
}

/// 三列金额的互算 + 自洽校验。
///
/// 入参是**用户给的任意组合**（都可空）；返回 `(不含税, 税额, 价税合计)` 三个都已定标四位。
/// 规则：
/// - 都不给 → 400（没法凭空造金额）；
/// - 给了 `total` → 有 `amount` 就用 `total - amount` 当税额，否则按税率倒推；
/// - 只给 `amount` → `tax = round(amount × rate)`、`total = amount + tax`；
/// - 给了 `tax` 没给 `total` → `total = amount + tax`。
pub fn resolve_amounts(
    amount: Option<Decimal>,
    tax_amount: Option<Decimal>,
    total_with_tax: Option<Decimal>,
    tax_rate: Decimal,
) -> std::result::Result<(Decimal, Decimal, Decimal), ApiError> {
    let amount = amount.map(|value| value.round_dp(4));
    let tax_amount = tax_amount.map(|value| value.round_dp(4));
    let total_with_tax = total_with_tax.map(|value| value.round_dp(4));
    match (amount, tax_amount, total_with_tax) {
        (None, None, None) => Err(bad_request(
            "金额一个都没给：至少给 total_with_tax（价税合计），或 amount（不含税）".to_string(),
        )),
        // 只给不含税：按税率算税、合计由两者相加
        (Some(amount), None, None) => {
            let tax_amount = tax_of(amount, tax_rate);
            Ok((amount, tax_amount, total_of(amount, tax_amount)))
        }
        // 不含税 + 税额（合计可给可不给，给了就必须等于两者之和）
        (Some(amount), Some(tax_amount), given_total) => {
            let total_with_tax = total_of(amount, tax_amount);
            ensure_total_matches(given_total, total_with_tax)?;
            Ok((amount, tax_amount, total_with_tax))
        }
        // 不含税 + 合计：税额取差，差额必须让「合计 = 不含税 + 税额」成立（1 分以内）
        (Some(amount), None, Some(given_total)) => {
            let tax_amount = (given_total - amount).round_dp(4);
            ensure_total_matches(Some(given_total), total_of(amount, tax_amount))?;
            Ok((amount, tax_amount, given_total))
        }
        // 只给税额：推不出不含税（税额是「不含税 × 税率」的结果，不是输入）
        (None, Some(_), None) => Err(bad_request(
            "只给 tax_amount 推不出不含税金额：请再给 amount 或 total_with_tax".to_string(),
        )),
        // 税额 + 合计：不含税取差
        (None, Some(tax_amount), Some(given_total)) => {
            let amount = (given_total - tax_amount).round_dp(4);
            Ok((amount, tax_amount, given_total))
        }
        // 只给合计：按税率倒推（先定不含税，税额取差 ⇒ 恒等必然成立）
        (None, None, Some(given_total)) => {
            let (amount, tax_amount) = split_tax(given_total, tax_rate);
            Ok((amount, tax_amount, given_total))
        }
    }
}

/// 用户给了价税合计时，算出来的合计必须与它逐分相等（不静默改用户的数）。
fn ensure_total_matches(given: Option<Decimal>, computed: Decimal) -> std::result::Result<(), ApiError> {
    match given {
        Some(given) if given != computed => Err(bad_request(format!(
            "金额三列不自洽：total_with_tax({given}) ≠ amount + tax_amount({computed})：\
             请只给其中两个，或让第三个数等于前两者之和"
        ))),
        _ => Ok(()),
    }
}

/// 某张应收还剩多少可开票额度（`应收金额 - 已开票价税合计`，可为 0，负数说明库层已漂移）。
pub fn headroom_of(receivable_amount: Decimal, invoiced_total: Decimal) -> Decimal {
    (receivable_amount - invoiced_total).round_dp(4)
}

/// 开票额度校验：本次价税合计 ≤ 可开票额度（超限是**业务冲突**，409 点名三个数字）。
pub fn validate_headroom(
    ar_no: &str,
    total_with_tax: Decimal,
    invoiced_total: Decimal,
    receivable_amount: Decimal,
) -> std::result::Result<(), ApiError> {
    let headroom = headroom_of(receivable_amount, invoiced_total);
    if headroom < Decimal::ZERO {
        return Err(conflict(format!(
            "应收 {ar_no} 的已开票金额 {invoiced_total} 已超过应收金额 {receivable_amount}\
             （库层数据漂移）：请先排查再开票"
        )));
    }
    if total_with_tax > headroom {
        return Err(conflict(format!(
            "本次开票价税合计 {total_with_tax} 超过应收 {ar_no} 的可开票额度 {headroom}\
             （应收金额 {receivable_amount} - 已开票 {invoiced_total}）：\
             开票金额不能超过应收金额，请分次开票或减少金额"
        )));
    }
    Ok(())
}

/// 发票号校验：空白 = 自动取号（`None`）；否则去掉首尾空白、长度 ≤ 64。
///
/// **只在为空时取号**（SO-19 的硬口径）：先取号再让手填覆盖会白烧号，单号出现空洞。
pub fn validate_invoice_no(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if text.chars().count() > NO_MAX_CHARS {
        return Err(bad_request(format!(
            "invoice_no 最多 {NO_MAX_CHARS} 个字符，收到 {} 个",
            text.chars().count()
        )));
    }
    if text.chars().any(char::is_whitespace) {
        return Err(bad_request(format!(
            "invoice_no 不能包含空白字符（税控号是连续号码），收到「{text}」"
        )));
    }
    Ok(Some(text.to_string()))
}

/// 发票代码校验（可空，≤ 32 字符）。
pub fn validate_invoice_code(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if text.chars().count() > CODE_MAX_CHARS {
        return Err(bad_request(format!(
            "invoice_code 最多 {CODE_MAX_CHARS} 个字符，收到 {} 个",
            text.chars().count()
        )));
    }
    Ok(Some(text.to_string()))
}

/// 票面形式标签（`true` = 电子票 / `false` = 纸质票）。
///
/// ATT-07：票面形式决定发票影像的格式白名单（电子票 pdf/ofd/xml，纸质票 jpg/png/pdf），
/// 规则在 `biz_attachment_rule` 里按 `is_electronic` 分支（V0348）。这里只给中文标签，
/// 供错误文案 / 前端兜底共用。
pub fn electronic_label(is_electronic: bool) -> &'static str {
    if is_electronic { "电子票" } else { "纸质票" }
}

/// 票面形式归一：请求不传（`None`）= **电子票**（数电票为主，与 V0348 默认值一致）。
///
/// 显式传 `false` 才是纸质票 —— 「没选」与「选了纸质」不能混为一谈，
/// 所以这里不用 `unwrap_or_default()` 之外的任何推断（尤其**不**看有没有 `invoice_code`：
/// 纸质票可以先不填税控代码，电子票也可能带代码）。
pub fn resolve_is_electronic(raw: Option<bool>) -> bool {
    raw.unwrap_or(true)
}

/// 票种校验：本卡只开放 `OUT`（红字票由退货流程生成，ARCH-7 §15.2.3）。
pub fn validate_type(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Ok(TYPE_OUT.to_string());
    }
    if !TYPES.contains(&text) {
        return Err(bad_request(format!("invoice_type 只认 {TYPES:?}，收到「{text}」")));
    }
    if text == TYPE_OUT_RED {
        return Err(bad_request(
            "本接口不开放手工建红字票（OUT_RED）：红字发票由退货流程按 ARCH-7 §15.2.3 生成，\
             作废一张发票请用 PATCH /invoice/{id}/status 打 VOID（系统自动红冲其收入凭证）"
                .to_string(),
        ));
    }
    Ok(text.to_string())
}

/// 来源类型归一：来源必须落在 `ck_biz_invoice_source` 的 4 值里，其余归 `MANUAL`。
pub fn normalize_source_type(raw: Option<&str>) -> Option<String> {
    let text = raw.map(str::trim).filter(|text| !text.is_empty())?;
    if SOURCE_TYPES.contains(&text) {
        Some(text.to_string())
    } else {
        // 应收的来源可能是上游模块的其它值（SHIPMENT / SALES_OUT / MANUAL 之外的），
        // 一律归 MANUAL：宁可丢细分来源，也不能撞库层 CHECK（23514）。
        Some("MANUAL".to_string())
    }
}

/// 会计期间：`yyyy-MM`（取开票日期所在月）。
pub fn period_code_of(date: NaiveDate) -> String {
    format!("{}-{:02}", date.year(), date.month())
}

/// `yyyy-MM-dd` 日期校验（严格形状，`2026-1-1` 不收）。
pub fn parse_date_arg(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    receivable::parse_date_arg(raw, field)
}

/// 作废原因校验（必填、去空白、长度区间）。
pub fn validate_reason(raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim();
    if text.chars().count() < REASON_MIN_CHARS {
        return Err(bad_request(format!(
            "reason 至少 {REASON_MIN_CHARS} 个字（说明为什么作废这张发票），收到「{text}」"
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

/// 400：入参不合法。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409：业务冲突（额度超限 / 状态不允许）。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 404：发票不存在。
fn not_found() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "发票不存在或已删除".to_string())
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
            "唯一约束冲突（{}）：发票号重复（手填税控号请核对；自动取号撞号属异常，请重试）",
            constraint.as_deref().unwrap_or("<未知>")
        )),
        DbError::CheckViolation { constraint, .. } => {
            let hint = match constraint.as_deref().unwrap_or("") {
                "ck_biz_invoice_amount" => "合计必须等于不含税 + 税额（金额三列不自洽）",
                "ck_biz_invoice_sign" => "正票金额必须 > 0 / 红字票必须 < 0（符号搞反了）",
                "ck_biz_invoice_tax_rate" => "税率必须落在 0~100（百分数，13 = 13%）",
                "ck_biz_invoice_status" => "状态只认 DRAFT / ISSUED / POSTED / VOID",
                "ck_biz_invoice_type" => "票种只认 OUT / OUT_RED",
                "ck_biz_invoice_source" => "来源只认 SALES_ORDER / SHIPMENT / SALES_OUT / MANUAL",
                _ => "数据不满足库层约束",
            };
            bad_request(format!(
                "数据不满足库约束 {}（{hint}）",
                constraint.as_deref().unwrap_or("<未知>")
            ))
        }
        DbError::InvalidValue(message) => bad_request(message),
        other => utils::db_error(other, ctx),
    }
}

// ==================== 行结构 ====================

/// 发票行（`invoice_code` / `receivable_id` / `source_*` / `voucher_id` / `remark` 可空）。
#[derive(Debug, Clone)]
pub struct InvoiceRow {
    /// 主键。
    pub id: Uuid,
    /// 发票号 / 单据号。
    pub invoice_no: String,
    /// 票种。
    pub invoice_type: String,
    /// 发票代码。
    pub invoice_code: Option<String>,
    /// 票面形式：`true` = 电子票（数电票），`false` = 纸质票（V0348 / ATT-07）。
    pub is_electronic: bool,
    /// 客户。
    pub customer_id: Uuid,
    /// 开票日期。
    pub invoice_date: NaiveDate,
    /// 不含税金额。
    pub amount: Decimal,
    /// 税额。
    pub tax_amount: Decimal,
    /// 价税合计。
    pub total_with_tax: Decimal,
    /// 税率（百分数 0–100，FIX-6 起与订单 / 收货同口径）。
    pub tax_rate: Decimal,
    /// 税率标度（`PERCENT` / `DECIMAL`；FIX-6 过渡期审计位）。
    pub tax_rate_mode: Option<String>,
    /// 状态。
    pub status: String,
    /// 关联应收。
    pub receivable_id: Option<Uuid>,
    /// 来源类型。
    pub source_type: Option<String>,
    /// 来源单号。
    pub source_no: Option<String>,
    /// 收入 / 红冲凭证。
    pub voucher_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 原币币种（V0314 / SO-40）。
    pub currency_code: String,
    /// 开票时锁定的汇率。
    pub exchange_rate: Decimal,
    /// 本位币不含税金额。
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

impl InvoiceRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            invoice_no: row.get_typed("invoice_no")?,
            invoice_type: row.get_typed("invoice_type")?,
            invoice_code: row.get_typed("invoice_code")?,
            is_electronic: row.get_typed("is_electronic")?,
            customer_id: row.get_typed("customer_id")?,
            invoice_date: row.get_typed("invoice_date")?,
            amount: row.get_typed("amount")?,
            tax_amount: row.get_typed("tax_amount")?,
            total_with_tax: row.get_typed("total_with_tax")?,
            tax_rate: row.get_typed("tax_rate")?,
            tax_rate_mode: row.get_typed("tax_rate_mode").ok(),
            status: row.get_typed("status")?,
            receivable_id: row.get_typed("receivable_id")?,
            source_type: row.get_typed("source_type")?,
            source_no: row.get_typed("source_no")?,
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
}

/// 按主键取一张发票（未软删）。
pub async fn find_by_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<InvoiceRow>> {
    // SCOPE-EXEMPT: 销售台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!("SELECT {COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(InvoiceRow::from_row).transpose()
}

/// 取一张发票并**加行锁**（`FOR UPDATE`，状态迁移必须先锁）。
pub async fn lock_by_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<InvoiceRow>> {
    // SCOPE-EXEMPT: 销售台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!("SELECT {COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(InvoiceRow::from_row).transpose()
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

/// 某张应收的**已开票价税合计**（作废票不占额度；可排除自身，防「重算时把自己算两遍」）。
pub async fn invoiced_total(
    ex: &impl SqlExecutor,
    receivable_id: Uuid,
    exclude_invoice_id: Option<Uuid>,
) -> DbResult<Decimal> {
    // SCOPE-EXEMPT: 销售台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT COALESCE(SUM(total_with_tax), 0) AS total FROM {TABLE} \
                  WHERE receivable_id = ?::uuid AND deleted_at IS NULL AND status <> ? \
                    AND id <> COALESCE(NULLIF(?::text, '')::uuid, \
                                       '00000000-0000-0000-0000-000000000000'::uuid)"
            ),
            vec![
                Value::from(&receivable_id),
                Value::Text(STATUS_VOID.to_string()),
                Value::Text(exclude_invoice_id.map(|id| id.to_string()).unwrap_or_default()),
            ],
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

/// 发票（列表 / 详情共用）。
#[derive(Debug, Clone, Serialize)]
pub struct InvoiceRes {
    /// 主键。
    pub id: String,
    /// 发票号。
    pub invoice_no: String,
    /// 票种。
    pub invoice_type: String,
    /// 票种中文。
    pub invoice_type_label: String,
    /// 发票代码。
    pub invoice_code: Option<String>,
    /// 票面形式：`true` = 电子票（数电票，影像只认 pdf/ofd/xml）/ `false` = 纸质票（扫描件 jpg/png/pdf）。
    ///
    /// ATT-07：开票时由用户选择，落库 `biz_invoice.is_electronic`（V0348），
    /// 附件必传规则按它分支（改票面形式即改校验口径）。
    pub is_electronic: bool,
    /// 客户。
    pub customer_id: String,
    /// 客户名称。
    pub customer_name: String,
    /// 开票日期。
    pub invoice_date: String,
    /// 不含税金额。
    pub amount: Decimal,
    /// 税额。
    pub tax_amount: Decimal,
    /// 价税合计。
    pub total_with_tax: Decimal,
    /// 税率（百分数 0–100，`13` = 13%）。
    pub tax_rate: Decimal,
    /// 税率标度（`PERCENT` / `DECIMAL`；FIX-6 过渡期审计位，老前端按小数提交时记 `DECIMAL`）。
    pub tax_rate_mode: Option<String>,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 关联应收。
    pub receivable_id: Option<String>,
    /// 来源类型。
    pub source_type: Option<String>,
    /// 来源单号。
    pub source_no: Option<String>,
    /// 凭证 id。
    pub voucher_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 原币币种（V0314 / SO-40）。
    pub currency_code: String,
    /// 开票时锁定的汇率。
    pub exchange_rate: Decimal,
    /// 本位币不含税金额。
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

impl InvoiceRes {
    /// 由行 + 客户名构造。
    pub fn from_row(row: &InvoiceRow, customer_name: &str) -> Self {
        Self {
            id: row.id.to_string(),
            invoice_no: row.invoice_no.clone(),
            invoice_type: row.invoice_type.clone(),
            invoice_type_label: type_label(&row.invoice_type).to_string(),
            invoice_code: row.invoice_code.clone(),
            is_electronic: row.is_electronic,
            customer_id: row.customer_id.to_string(),
            customer_name: customer_name.to_string(),
            invoice_date: row.invoice_date.to_string(),
            amount: row.amount,
            tax_amount: row.tax_amount,
            total_with_tax: row.total_with_tax,
            tax_rate: row.tax_rate,
            tax_rate_mode: row.tax_rate_mode.clone(),
            status: row.status.clone(),
            status_label: status_label(&row.status).to_string(),
            receivable_id: row.receivable_id.map(|id| id.to_string()),
            source_type: row.source_type.clone(),
            source_no: row.source_no.clone(),
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

/// 发票详情（连带应收与凭证信息，前端抽屉一次看全）。
#[derive(Debug, Clone, Serialize)]
pub struct InvoiceDetailRes {
    /// 单头。
    #[serde(flatten)]
    pub invoice: InvoiceRes,
    /// 收入凭证号（`voucher_id` 为空或凭证已删时 `None`）。
    pub voucher_no: Option<String>,
    /// 关联应收单号。
    pub receivable_no: Option<String>,
    /// 关联应收金额。
    pub receivable_amount: Option<Decimal>,
    /// 关联应收状态。
    pub receivable_status: Option<String>,
    /// 关联应收状态中文。
    pub receivable_status_label: Option<String>,
    /// 这张应收还剩多少可开票额度（手工开票 / 应收已删时为 `None`）。
    pub receivable_headroom: Option<Decimal>,
}

/// 发票详情（客户名 / 应收 / 凭证号一次取全）。
pub async fn invoice_detail(id: Uuid) -> std::result::Result<InvoiceDetailRes, ApiError> {
    detail_with(db(), id).await
}

/// [`invoice_detail`] 的 executor 版（事务内回读用，看得到本事务未提交的写入）。
pub async fn detail_with(ex: &impl SqlExecutor, id: Uuid) -> std::result::Result<InvoiceDetailRes, ApiError> {
    // SCOPE-EXEMPT: 发票 / 客户 / 应收 / 凭证均无归属列（见模块文档）。
    let sql = format!("SELECT {LIST_COLUMNS} {LIST_FROM} WHERE p.id = ?::uuid AND p.deleted_at IS NULL");
    let row = ex
        .query_first_sql(sql, vec![Value::from(&id)])
        .await
        .map_err(|error| utils::db_error(error, "查询发票"))?
        .ok_or_else(not_found)?;
    let invoice = InvoiceRow::from_row(&row).map_err(|error| utils::db_error(error, "解析发票"))?;
    let customer_name: String = row
        .get_typed("customer_name")
        .map_err(|error| utils::db_error(error, "读取客户名称"))?;
    let voucher_no = match invoice.voucher_id {
        Some(voucher_id) => voucher_no_of(ex, voucher_id)
            .await
            .map_err(|error| utils::db_error(error, "查询发票凭证号"))?,
        None => None,
    };
    let (receivable_no, receivable_amount, receivable_status, headroom) = match invoice.receivable_id {
        Some(receivable_id) => {
            match receivable::find_by_id(ex, receivable_id)
                .await
                .map_err(|error| utils::db_error(error, "查询关联应收"))?
            {
                Some(ar) => {
                    let invoiced = invoiced_total(ex, receivable_id, Some(invoice.id))
                        .await
                        .map_err(|error| utils::db_error(error, "统计应收已开票金额"))?;
                    // 详情里的额度 = 应收金额 - **其它**存活发票（含本张），
                    // 即「不算本张」的剩余额度，前端据此提示还能开多少。
                    let others = if invoice.status == STATUS_VOID {
                        invoiced
                    } else {
                        (invoiced + invoice.total_with_tax).round_dp(4)
                    };
                    (
                        Some(ar.ar_no.clone()),
                        Some(ar.amount),
                        Some(ar.status.clone()),
                        Some(headroom_of(ar.amount, others)),
                    )
                }
                None => (None, None, None, None),
            }
        }
        None => (None, None, None, None),
    };
    Ok(InvoiceDetailRes {
        invoice: InvoiceRes::from_row(&invoice, &customer_name),
        voucher_no,
        receivable_no,
        receivable_amount,
        receivable_status_label: receivable_status
            .as_deref()
            .map(|status| receivable::status_label(status).to_string()),
        receivable_status,
        receivable_headroom: headroom,
    })
}

// ==================== 入参 ====================

/// 开票请求：**从应收开票**（给 `receivable_id`）或**手工开票**（给 `customer_id` + 金额）。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateInvoiceReq {
    /// 关联应收（给了就按应收带出金额 / 客户 / 来源，并受「不超过应收」约束）。
    pub receivable_id: Option<String>,
    /// 客户（从应收开票时可省，必须与应收的客户一致）。
    pub customer_id: Option<String>,
    /// 开票日期（缺省 = 今天 CST）。
    pub invoice_date: Option<String>,
    /// 发票号 / 税控号（空 = 自动取 `IV-yyyyMMdd-000001`）。
    pub invoice_no: Option<String>,
    /// 发票代码（可空）。
    pub invoice_code: Option<String>,
    /// 票面形式（缺省 = `true` 电子票）：电子票影像只认 pdf/ofd/xml，纸质票认 jpg/png/pdf。
    ///
    /// 开票时选择的票面形式会落库 `biz_invoice.is_electronic`（V0348），
    /// 之后「开票（DRAFT→ISSUED）」动作的附件必传校验按它分支 —— 传错格式 400，
    /// 想改口径就重新建单 / 改库里的这一列（本卡没有「改票面形式」接口）。
    pub is_electronic: Option<bool>,
    /// 票种（只认 `OUT`；红字票由退货流程生成）。
    pub invoice_type: Option<String>,
    /// 不含税金额。
    pub amount: Option<Decimal>,
    /// 税额。
    pub tax_amount: Option<Decimal>,
    /// 价税合计（从应收开票时缺省 = 应收金额）。
    pub total_with_tax: Option<Decimal>,
    /// 税率（**百分数** `0–100`，缺省 13 = 13%；FIX-6 起与订单 / 收货同口径）。
    pub tax_rate: Option<Decimal>,
    /// 税率标度声明（FIX-6 过渡期）：`PERCENT`（13 = 13%）/ `DECIMAL`（0.13 = 13%）/ 缺省。
    ///
    /// 缺省时按过渡期规则自动识别：`0 < 税率 ≤ 1` 视为小数并 ×100（打 WARN 日志），
    /// 其余按百分数。要表达「1%」请显式传 `PERCENT`（否则 `1` 会被当成 100%）。
    pub tax_rate_mode: Option<String>,
    /// 来源类型（从应收开票时自动带出）。
    pub source_type: Option<String>,
    /// 来源单号（从应收开票时自动带出）。
    pub source_no: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 币种（ISO 4217 三位大写；缺省 = 应收的币种，再缺省 = 账套本位币）。V0314 / SO-40。
    ///
    /// 非本位币 ⇒ 按 `invoice_date` 取最近一条 SPOT 汇率并锁定，取不到一律 400。
    pub currency_code: Option<String>,
}

/// 状态迁移请求。
#[derive(Debug, Clone, Deserialize)]
pub struct StatusReq {
    /// 目标状态（`ISSUED` / `POSTED` / `VOID`）。
    pub status: String,
    /// 作废原因（`VOID` 必填，≥4 字）。
    pub reason: Option<String>,
}

/// 分页查询。
#[derive(Debug, Clone, Deserialize)]
pub struct InvoicePageQuery {
    /// 页码（从 1 起）。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 状态（白名单）。
    pub status: Option<String>,
    /// 票种（白名单）。
    pub invoice_type: Option<String>,
    /// 客户。
    pub customer_id: Option<String>,
    /// 关联应收。
    pub receivable_id: Option<String>,
    /// 发票号 / 客户名（模糊）。
    pub invoice_no: Option<String>,
    /// 开票日期起。
    pub date_from: Option<String>,
    /// 开票日期止。
    pub date_to: Option<String>,
}

/// 发票筛选（数据层形态）。
#[derive(Debug, Clone, Default)]
pub struct InvoiceFilter {
    /// 状态。
    pub status: Option<String>,
    /// 票种。
    pub invoice_type: Option<String>,
    /// 客户。
    pub customer_id: Option<Uuid>,
    /// 关联应收。
    pub receivable_id: Option<Uuid>,
    /// 发票号 / 客户名（模糊）。
    pub keyword: Option<String>,
    /// 日期起。
    pub date_from: Option<NaiveDate>,
    /// 日期止。
    pub date_to: Option<NaiveDate>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl InvoicePageQuery {
    /// 五道入参校验（全部 400，绝不静默忽略）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(status) = self.status.as_deref().map(str::trim)
            && !status.is_empty()
            && !STATUSES.contains(&status)
        {
            return Err(bad_request(format!(
                "status 只认 {STATUSES:?}（收到「{status}」）：CANCELLED 是收款单 / 应收的域值，\
                 发票用 VOID"
            )));
        }
        if let Some(invoice_type) = self.invoice_type.as_deref().map(str::trim)
            && !invoice_type.is_empty()
            && !TYPES.contains(&invoice_type)
        {
            return Err(bad_request(format!(
                "invoice_type 只认 {TYPES:?}（收到「{invoice_type}」）"
            )));
        }
        for (field, raw) in [
            ("customer_id", self.customer_id.as_deref()),
            ("receivable_id", self.receivable_id.as_deref()),
        ] {
            if let Some(raw) = raw.map(str::trim)
                && !raw.is_empty()
            {
                parse_uuid(raw, field)?;
            }
        }
        for (field, raw) in [
            ("date_from", self.date_from.as_deref()),
            ("date_to", self.date_to.as_deref()),
        ] {
            if let Some(raw) = raw.map(str::trim)
                && !raw.is_empty()
            {
                parse_date_arg(raw, field)?;
            }
        }
        Ok(())
    }

    /// 转数据层筛选（空白串 = 没传）。
    pub fn to_filter(&self) -> std::result::Result<InvoiceFilter, ApiError> {
        let (page, page_size) = normalize_page(self.page.unwrap_or(1), self.page_size.unwrap_or(0));
        let mut filter = InvoiceFilter {
            page,
            page_size,
            ..InvoiceFilter::default()
        };
        if let Some(status) = self.status.as_deref().map(str::trim)
            && !status.is_empty()
        {
            filter.status = Some(status.to_string());
        }
        if let Some(invoice_type) = self.invoice_type.as_deref().map(str::trim)
            && !invoice_type.is_empty()
        {
            filter.invoice_type = Some(invoice_type.to_string());
        }
        if let Some(raw) = self.customer_id.as_deref().map(str::trim)
            && !raw.is_empty()
        {
            filter.customer_id = Some(parse_uuid(raw, "customer_id")?);
        }
        if let Some(raw) = self.receivable_id.as_deref().map(str::trim)
            && !raw.is_empty()
        {
            filter.receivable_id = Some(parse_uuid(raw, "receivable_id")?);
        }
        if let Some(raw) = self.invoice_no.as_deref().map(str::trim)
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

/// 发票分页（全部状态；财务要对账，作废的也要看得见）。
pub async fn invoice_page(q: &InvoiceFilter) -> DbResult<(Vec<(InvoiceRow, String)>, i64)> {
    let mut sql = format!("SELECT {LIST_COLUMNS} {LIST_FROM} WHERE p.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND p.status = ?");
        params.push(Value::from(status));
    }
    if let Some(invoice_type) = q.invoice_type.as_deref() {
        sql.push_str(" AND p.invoice_type = ?");
        params.push(Value::from(invoice_type));
    }
    if let Some(customer_id) = q.customer_id {
        sql.push_str(" AND p.customer_id = ?::uuid");
        params.push(Value::from(&customer_id));
    }
    if let Some(receivable_id) = q.receivable_id {
        sql.push_str(" AND p.receivable_id = ?::uuid");
        params.push(Value::from(&receivable_id));
    }
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (p.invoice_no LIKE ? OR c.name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(from) = q.date_from {
        sql.push_str(" AND p.invoice_date >= ?::text::date");
        params.push(Value::Text(from.to_string()));
    }
    if let Some(to) = q.date_to {
        sql.push_str(" AND p.invoice_date <= ?::text::date");
        params.push(Value::Text(to.to_string()));
    }
    let result = scope::paginate_unscoped(LIST_TARGET, &sql, params, Some(LIST_ORDER), q.page, q.page_size).await?;
    let rows = result
        .list
        .iter()
        .map(|row| {
            let name: String = row.get_typed("customer_name")?;
            Ok((InvoiceRow::from_row(row)?, name))
        })
        .collect::<DbResult<Vec<(InvoiceRow, String)>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

// ==================== 写：开票 ====================

/// 今日（CST）。
fn today_cst() -> NaiveDate {
    Utc::now()
        .with_timezone(&nik::prelude::chrono::FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定"))
        .date_naive()
}

/// 建一张发票（`DRAFT`，账上一分没动），并写建单状态日志。
///
/// 从应收开票时在同一事务里**锁应收**并校验「开票金额 ≤ 应收金额 - 已开票金额」。
pub async fn create(operator: Option<Uuid>, req: CreateInvoiceReq) -> std::result::Result<InvoiceDetailRes, ApiError> {
    let receivable_id = match req.receivable_id.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(parse_uuid(raw, "receivable_id")?),
        _ => None,
    };
    let customer_id = match req.customer_id.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(parse_uuid(raw, "customer_id")?),
        _ => None,
    };
    if receivable_id.is_none() && customer_id.is_none() {
        return Err(bad_request(
            "receivable_id 与 customer_id 至少给一个：从应收开票给前者，手工开票给后者".to_string(),
        ));
    }
    let invoice_type = validate_type(req.invoice_type.as_deref())?;
    let invoice_no = validate_invoice_no(req.invoice_no.as_deref())?;
    let invoice_code = validate_invoice_code(req.invoice_code.as_deref())?;
    // ATT-07：票面形式（缺省电子票）。它决定「开票」动作时发票影像的格式白名单
    //         （电子票 pdf/ofd/xml / 纸质票 jpg/png/pdf，规则见 biz_attachment_rule）。
    //         刻意**不**从 invoice_code 推断：纸质票可以现场开完再补录税控代码。
    let is_electronic = resolve_is_electronic(req.is_electronic);
    // FIX-6：税率一律按**百分数**落库。老前端传小数（0.13）时按过渡期规则 ×100 并打 WARN，
    //        实际采用的标度写进 `tax_rate_mode` 供审计（PRD：「过渡期后删除」）。
    let tax_mode = tax_rate_mod::TaxRateMode::parse(req.tax_rate_mode.as_deref())?;
    let normalized = match req.tax_rate {
        Some(rate) => tax_rate_mod::normalize_with_mode(rate, tax_mode, "开票 tax_rate")?,
        None => tax_rate_mod::Normalized {
            percent: default_tax_rate(),
            mode: tax_rate_mod::MODE_PERCENT,
        },
    };
    let tax_rate = normalized.percent;
    let tax_rate_mode = normalized.mode_text();
    let invoice_date = match req.invoice_date.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => parse_date_arg(raw, "invoice_date")?,
        _ => today_cst(),
    };
    let period_code = period_code_of(invoice_date);
    let remark = req
        .remark
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let req_source_type = req
        .source_type
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let req_source_no = req
        .source_no
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let now = Utc::now();

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启开票事务"))?;
    let result = async {
        period::ensure_period_writable(&tx, &period_code, "开票").await?;

        // ① 从应收带出：锁应收 → 客户 / 金额 / 来源
        let mut resolved_customer = customer_id;
        let mut resolved_source_type = req_source_type.clone();
        let mut resolved_source_no = req_source_no.clone();
        let mut receivable_total: Option<Decimal> = None;
        // SO-40：从应收开票时继承应收的**币种**（汇率仍按开票日重新锁定）
        let mut receivable_currency: Option<String> = None;
        if let Some(receivable_id) = receivable_id {
            let ar = lock_receivable(&tx, receivable_id)
                .await
                .map_err(|error| utils::db_error(error, "锁定应收单"))?
                .ok_or_else(|| {
                    ApiError::new(
                        404,
                        error_code::RESOURCE_NOT_FOUND,
                        format!("应收单 {receivable_id} 不存在或已删除"),
                    )
                })?;
            match ar.status.as_str() {
                receivable::STATUS_CANCELLED => {
                    return Err(conflict(format!(
                        "应收单 {} 已作废（CANCELLED）：作废的应收不能再开票",
                        ar.ar_no
                    )));
                }
                receivable::STATUS_WRITTEN_OFF => {
                    return Err(conflict(format!(
                        "应收单 {} 已坏账核销（WRITTEN_OFF）：核销掉的应收不能再开票",
                        ar.ar_no
                    )));
                }
                _ => {}
            }
            if let Some(customer) = resolved_customer
                && customer != ar.customer_id
            {
                return Err(bad_request(format!(
                    "customer_id {customer} 与应收单 {} 的客户 {} 不一致：\
                     发票客户必须与应收客户相同",
                    ar.ar_no, ar.customer_id
                )));
            }
            resolved_customer = Some(ar.customer_id);
            resolved_source_type =
                resolved_source_type.or_else(|| normalize_source_type(Some(ar.source_type.as_str())));
            resolved_source_no = resolved_source_no.or(Some(ar.source_no.clone()));
            receivable_total = Some(ar.amount);
            receivable_currency = Some(ar.currency_code.clone());
        }
        let customer_id = resolved_customer.expect("上面已保证至少有一个来源给客户");

        if !customer_exists(&tx, customer_id)
            .await
            .map_err(|error| utils::db_error(error, "校验客户"))?
        {
            return Err(bad_request(format!(
                "customer_id {customer_id} 不存在或已删除：发票必须挂在一个有效客户上"
            )));
        }

        // ② 金额：从应收开票时价税合计缺省 = 应收金额全额（卡面「自动带出金额」）
        let total_with_tax = req.total_with_tax.or_else(|| {
            // 只给 amount / tax 的从应收开票：合计由两列算出，不套用应收金额
            if req.amount.is_none() && req.tax_amount.is_none() {
                receivable_total
            } else {
                None
            }
        });
        let (amount, tax_amount, total_with_tax) =
            resolve_amounts(req.amount, req.tax_amount, total_with_tax, tax_rate)?;
        validate_total(total_with_tax)?;
        validate_sign(&invoice_type, amount, tax_amount)?;

        // ③ 开票额度：本次 ≤ 应收金额 - 已开票合计（作废票不占额度）
        if let Some(receivable_id) = receivable_id {
            let ar_amount = receivable_total.expect("有应收就带出了金额");
            let invoiced = invoiced_total(&tx, receivable_id, None)
                .await
                .map_err(|error| utils::db_error(error, "统计应收已开票金额"))?;
            let ar_no = {
                let ar = receivable::find_by_id(&tx, receivable_id)
                    .await
                    .map_err(|error| utils::db_error(error, "回读应收单"))?;
                ar.map(|row| row.ar_no).unwrap_or_default()
            };
            validate_headroom(&ar_no, total_with_tax, invoiced, ar_amount)?;
        }

        // ④ 取号：**只在 invoice_no 为空时**取（手填税控号不烧号）
        let invoice_no = match invoice_no {
            Some(no) => no,
            None => voucher::next_doc_no(&tx, &INVOICE_SEQ, now)
                .await
                .map_err(|error| utils::db_error(error, "取发票号"))?,
        };

        // ④.5 SO-40：币种 = 显式传入 > 应收的币种 > 账套本位币；汇率按**开票日** SPOT 锁定。
        //     从应收开票时不继承应收的汇率、只继承币种：发票是独立凭证，
        //     它的汇率口径是「开票当天」（§15.6.4 ① 锁的是「建单时点」）。
        let requested_currency = req
            .currency_code
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .or_else(|| receivable_currency.clone());
        let locked = currency::lock(&tx, requested_currency.as_deref(), invoice_date)
            .await
            .map_err(|error| utils::db_error(error, "取开票汇率"))?
            .into_locked("发票")?;
        let base_amounts = currency::BaseAmounts::of(amount, tax_amount, locked.exchange_rate);

        let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();
        // SCOPE-EXEMPT: 销售台账无归属列（见模块文档）。
        let inserted = tx
            .query_first_sql(
                format!(
                    "INSERT INTO {TABLE} \
                        (invoice_no, invoice_type, invoice_code, is_electronic, customer_id, invoice_date, \
                         amount, tax_amount, total_with_tax, tax_rate, tax_rate_mode, status, receivable_id, \
                         source_type, source_no, remark, \
                         currency_code, exchange_rate, base_total_amount, base_total_tax, base_total_with_tax, \
                         created_at, updated_at, create_by, update_by) \
                     VALUES (?, ?, NULLIF(?::text,''), ?, ?::uuid, ?::text::date, \
                             NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                             NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, ?, \
                             '{STATUS_DRAFT}', NULLIF(?::text,'')::uuid, \
                             NULLIF(?::text,''), NULLIF(?::text,''), NULLIF(?::text,''), \
                             ?, ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                             ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
                     RETURNING id"
                ),
                vec![
                    Value::Text(invoice_no.clone()),
                    Value::Text(invoice_type.clone()),
                    Value::Text(invoice_code.clone().unwrap_or_default()),
                    Value::Bool(is_electronic),
                    Value::from(&customer_id),
                    Value::Text(invoice_date.to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(tax_amount.to_string()),
                    Value::Text(total_with_tax.to_string()),
                    Value::Text(tax_rate.to_string()),
                    Value::Text(tax_rate_mode.to_string()),
                    Value::Text(receivable_id.map(|id| id.to_string()).unwrap_or_default()),
                    Value::Text(resolved_source_type.clone().unwrap_or_default()),
                    Value::Text(resolved_source_no.clone().unwrap_or_default()),
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
            .map_err(|error| write_error(error, "开票"))?
            .ok_or_else(|| {
                ApiError::new(
                    500,
                    error_code::INTERNAL_ERROR,
                    "发票写入失败：INSERT 没有返回 id".to_string(),
                )
            })?;
        let id: Uuid = inserted
            .get_typed("id")
            .map_err(|error| utils::db_error(error, "读取发票 id"))?;

        let created_reason = if receivable_id.is_some() {
            "从应收开票"
        } else {
            "手工开票"
        };
        state_machine::log_created(
            &tx,
            ENTITY,
            id,
            &invoice_no,
            STATUS_DRAFT,
            operator,
            Some(created_reason),
        )
        .await
        .map_err(|error| {
            write_error(
                DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)),
                "发票建单日志",
            )
        })?;
        detail_with(&tx, id).await
    }
    .await;
    match result {
        Ok(detail) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交开票事务"))?;
            Ok(detail)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== 写：状态迁移 ====================

/// 状态迁移（`DRAFT→ISSUED` 开票 / `ISSUED→POSTED` 归档 / `DRAFT|ISSUED→VOID` 作废）。
///
/// - `ISSUED`：再判一次开票额度（防并发抢开），生成收入凭证（借 1122 / 贷 5001 + 2221.02）、
///   回写 `biz_receivable.invoice_no / invoice_date`；
/// - `POSTED`：归档（要求已有凭证），不产生第二张凭证；
/// - `VOID`：原因必填（≥4 字）；有凭证就红冲（`voucher::reverse_posted` + 红字凭证），
///   并按号撤回应收的发票号回写。
pub async fn change_status(
    operator: Option<Uuid>,
    id: &str,
    req: StatusReq,
) -> std::result::Result<InvoiceDetailRes, ApiError> {
    let id = parse_uuid(id, "id")?;
    let target = req.status.trim().to_string();
    if !STATUSES.contains(&target.as_str()) {
        return Err(bad_request(format!("status 只认 {STATUSES:?}（收到「{target}」）")));
    }
    if target == STATUS_DRAFT {
        return Err(bad_request(
            "没有任何边回到草稿（DRAFT）：发票一旦删号就不能原地重开，请作废后重新开票".to_string(),
        ));
    }
    let reason = match req.reason.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(validate_reason(raw)?),
        _ => None,
    };
    if target == STATUS_VOID && reason.is_none() {
        return Err(bad_request(
            "作废（VOID）必须给 reason（≥4 字）：审计要能回答「为什么作废这张发票」".to_string(),
        ));
    }
    let now = Utc::now();

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启发票状态迁移事务"))?;
    let result = async {
        // 锁序：先锁发票头，再锁应收（与收款核销「先收款后应收」同向，不会成环）
        let invoice = lock_by_id(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定发票"))?
            .ok_or_else(not_found)?;
        if invoice.status == target {
            return Err(conflict(format!(
                "发票 {} 已经是「{}」，没有状态变化",
                invoice.invoice_no,
                status_label(&target)
            )));
        }
        if !LEGAL_EDGES
            .iter()
            .any(|(from, to)| *from == invoice.status && *to == target)
        {
            return Err(conflict(format!(
                "发票 {} 不能从「{}」变成「{}」：合法去向是 {}",
                invoice.invoice_no,
                status_label(&invoice.status),
                status_label(&target),
                describe_next(&invoice.status)
            )));
        }
        // ATT-06：开票（DRAFT → ISSUED）即发票的「提交」动作 —— 发票影像必传。
        // 规则来自 `biz_attachment_rule`（改 enabled / remark / allow_exts 立即生效，不重启）。
        if target == STATUS_ISSUED {
            attach_rule::ensure_attachments(AttachDocKind::Invoice, id, AttachAt::Submit).await?;
        }
        match target.as_str() {
            STATUS_ISSUED | STATUS_POSTED => {
                period::ensure_period_writable(&tx, &period_code_of(invoice.invoice_date), "发票开票").await?;
            }
            STATUS_VOID => {
                // 作废要把凭证红冲掉，红字凭证落在**作废当天**的期间
                period::ensure_period_writable(&tx, &period_code_of(voucher::today_cst(now)), "发票作废").await?;
            }
            _ => {}
        }
        if target == STATUS_POSTED && invoice.voucher_id.is_none() {
            return Err(conflict(format!(
                "发票 {} 还没有收入凭证（状态「{}」）：先打 ISSUED 开票生成凭证，再归档",
                invoice.invoice_no,
                status_label(&invoice.status)
            )));
        }

        // 开票额度：ISSUED 之前再判一次（建单到开票之间可能有人抢开了同一张应收）
        if target == STATUS_ISSUED
            && let Some(receivable_id) = invoice.receivable_id
        {
            let ar = receivable::find_by_id(&tx, receivable_id)
                .await
                .map_err(|error| utils::db_error(error, "回读关联应收"))?
                .ok_or_else(|| {
                    ApiError::new(
                        404,
                        error_code::RESOURCE_NOT_FOUND,
                        format!("发票关联的应收 {receivable_id} 不存在或已删除"),
                    )
                })?;
            let invoiced = invoiced_total(&tx, receivable_id, Some(invoice.id))
                .await
                .map_err(|error| utils::db_error(error, "统计应收已开票金额"))?;
            validate_headroom(&ar.ar_no, invoice.total_with_tax, invoiced, ar.amount)?;
        }

        state_machine::transition(
            &tx,
            ENTITY,
            invoice.id,
            &invoice.invoice_no,
            &LEGAL_EDGES,
            &invoice.status,
            &target,
            operator,
            reason.as_deref(),
            None,
        )
        .await
        .map_err(|error| {
            write_error(
                DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)),
                "发票状态变更",
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
                    Value::from(&invoice.id),
                    Value::Text(invoice.status.clone()),
                ],
            )
            .await
            .map_err(|error| write_error(error, "回写发票状态"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "发票 {} 的状态在本次迁移期间被其它操作改动，本次操作已回滚，请重试",
                invoice.invoice_no
            )));
        }

        match target.as_str() {
            STATUS_ISSUED => {
                if let Some(voucher_id) = post_invoice_voucher(&tx, &invoice, operator, now).await? {
                    tx.exec_sql(
                        format!(
                            "UPDATE {TABLE} SET voucher_id = ?::uuid, updated_at = ? \
                              WHERE id = ?::uuid"
                        ),
                        vec![Value::from(&voucher_id), Value::from(now), Value::from(&invoice.id)],
                    )
                    .await
                    .map_err(|error| write_error(error, "发票回写凭证"))?;
                    // 收入确认凭证同时回填应收单的 `voucher_id`（SO-22）：出库作废冲销
                    // （`receivable::cancel_for_stock_out`）靠它判断「这条应收已有收入凭证，
                    // 必须一起红冲」，否则会出现「应收冲销了、总账上 1122 还挂着」。
                    writeback_receivable(&tx, &invoice, Some(voucher_id), operator, now).await?;
                }
            }
            STATUS_VOID => {
                reverse_invoice_voucher(&tx, &invoice, reason.as_deref(), operator, now).await?;
                clear_receivable_writeback(&tx, &invoice, operator, now).await?;
                // 撤号只撤「作废这张」：同一应收下还有生效中的票时，指针要改指那一张
                // （否则一张「开了 2260 / 还差 3390」的应收会显示成「未开票」）。
                restore_receivable_pointer(&tx, &invoice, operator, now).await?;
            }
            _ => {}
        }

        detail_with(&tx, invoice.id).await
    }
    .await;
    match result {
        Ok(detail) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交发票状态迁移事务"))?;
            Ok(detail)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 发票收入凭证：借 `1122` 应收账款（价税合计）/ 贷 `5001` 主营业务收入（不含税）
/// + 贷 `2221.02` 应交税费-销项税（税额；为 0 时不出这一行，零金额分录会被 `post_voucher` 拒）。
///
/// **分录构造不再写在这里**：SO-22 把它收口到 [`voucher::create_for_sales_revenue`]
/// （合并 / 排序 / 尾差吸收 / 借贷平衡 / 期间闸都在那里，一份实现）。
/// 本函数只负责把发票行翻译成入参，并把「价税合计 ≤ 0 时没有凭证」如实返回。
async fn post_invoice_voucher(
    tx: &Transaction,
    invoice: &InvoiceRow,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<Option<Uuid>, ApiError> {
    // 摘要取客户**名称**（AGENTS.md 约定 13 / E2E-FIX-5 ①）：账面上写 UUID 财务看不懂也搜不到。
    let customer = voucher::party_label(
        form_snapshot::customer_name(tx, invoice.customer_id)
            .await
            .map_err(|error| write_error(error, "读发票客户名称"))?,
        "客户",
        invoice.customer_id,
    );
    let summary = format!("开票 {} 客户 {}", invoice.invoice_no, customer);
    voucher::create_for_sales_revenue(
        tx,
        &voucher::SalesRevenueVoucher {
            invoice_id: invoice.id,
            invoice_no: invoice.invoice_no.as_str(),
            voucher_date: invoice.invoice_date,
            amount: invoice.amount,
            tax_amount: invoice.tax_amount,
            total_with_tax: invoice.total_with_tax,
            // 发票没有商品明细行（biz_invoice 是「一张票一行金额」的粒度）⇒ 贷方收入单行汇总。
            // 明细级收入拆分由退货 / 多行票卡在 `material_lines` 上接（同一 builder）。
            material_lines: Vec::new(),
            summary,
            remark: invoice.remark.as_deref(),
            // **按本位币入账**（SO-41 / ARCH-7 §15.6.4 ②）：外币发票的凭证金额取建单时
            // 按锁定汇率算出的 base_* 三列，而不是原币 —— 总账只认本位币。
            base: Some(voucher::BaseAmounts {
                amount: invoice.base_total_amount,
                tax: invoice.base_total_tax,
                with_tax: invoice.base_total_with_tax,
            }),
        },
        operator,
        now,
    )
    .await
    .map_err(|error| write_error(error, "生成发票收入凭证"))
}
/// 作废时红冲收入凭证（没有凭证 / 已红冲过 → 静默跳过，作废幂等）。
async fn reverse_invoice_voucher(
    tx: &Transaction,
    invoice: &InvoiceRow,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    let Some(voucher_id) = invoice.voucher_id else {
        // DRAFT → VOID：账上什么都没动，没有可红冲的凭证
        return Ok(());
    };
    let Some(original) = voucher::find_voucher(tx, voucher_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发票的收入凭证"))?
    else {
        return Ok(());
    };
    if original.reversed_by.is_some() {
        // 已红冲过（重复作废 / 外部先冲了）⇒ 不再冲第二张，避免一票两冲
        return Ok(());
    }
    voucher::reverse_posted(
        tx,
        &original,
        ReversalInput {
            reason,
            voucher_date: voucher::today_cst(now),
        },
        operator,
        now,
    )
    .await
    .map_err(|error| write_error(error, "红冲发票的收入凭证"))?;
    Ok(())
}

/// 回写应收的发票号 / 开票日期 / **收入确认凭证**（拷贝列，权威始终在 `biz_invoice`）。
///
/// `voucher_id` 回填（SO-22）是「出库作废 → 应收冲销」能把 1122 一起红冲的前提：
/// `receivable::cancel_for_stock_out` 只对 `voucher_id IS NOT NULL`（= 收入已确认过）
/// 的应收生成红字凭证，为空时什么都不做（挂账未开票的应收本来就没收入凭证）。
async fn writeback_receivable(
    tx: &Transaction,
    invoice: &InvoiceRow,
    voucher_id: Option<Uuid>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    let Some(receivable_id) = invoice.receivable_id else {
        return Ok(());
    };
    // SCOPE-EXEMPT: 应收台账无归属列（见模块文档）。
    tx.exec_sql(
        format!(
            "UPDATE {} SET invoice_no = ?, invoice_date = ?::text::date, \
                    voucher_id = COALESCE(NULLIF(?::text,'')::uuid, voucher_id), updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            receivable::TABLE
        ),
        vec![
            Value::Text(invoice.invoice_no.clone()),
            Value::Text(invoice.invoice_date.to_string()),
            Value::Text(voucher_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&receivable_id),
        ],
    )
    .await
    .map_err(|error| write_error(error, "回写应收的发票号"))?;
    Ok(())
}

/// 作废时按号撤回应收的发票号回写（只清「确实指回本发票」的那一行，别擦掉别人的号）。
///
/// `voucher_id` 一起清（SO-22）：蓝字收入凭证已经被红冲了，应收单上再挂着一个
/// 「已红冲的凭证号」会让 `cancel_for_stock_out` 拿它去第二次红冲。
///
/// 清空之后由 [`restore_receivable_pointer`] 把指针改指「本应收下仍在生效的最新一张票」——
/// 只撤不清会留下一张「有票却显示无票」的应收（V-18 审计口径要求指针可回溯）。
async fn clear_receivable_writeback(
    tx: &Transaction,
    invoice: &InvoiceRow,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    let Some(receivable_id) = invoice.receivable_id else {
        return Ok(());
    };
    // SCOPE-EXEMPT: 应收台账无归属列（见模块文档）。
    tx.exec_sql(
        format!(
            "UPDATE {} SET invoice_no = NULL, invoice_date = NULL, voucher_id = NULL, \
                    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND invoice_no = ?",
            receivable::TABLE
        ),
        vec![
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&receivable_id),
            Value::Text(invoice.invoice_no.clone()),
        ],
    )
    .await
    .map_err(|error| write_error(error, "撤回应收的发票号"))?;
    Ok(())
}

/// 一张候选发票的「指针三元组」——[`surviving_pointer`] 的入参。
///
/// 不直接吃 [`InvoiceRow`]：指针选择是纯逻辑（排序 + 过滤），单测不该为了一个排序
/// 把 25 个字段的行结构造一遍；生产路径由 [`restore_receivable_pointer`] 映射过来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerCandidate {
    /// 发票 id。
    pub id: Uuid,
    /// 发票号。
    pub invoice_no: String,
    /// 开票日期。
    pub invoice_date: NaiveDate,
    /// 状态。
    pub status: String,
    /// 收入凭证（`ISSUED` 才有；`NULL` = 这张票没进过账）。
    pub voucher_id: Option<Uuid>,
    /// 创建时间（同日多张票时的次级排序键）。
    pub created_at: Option<DateTime<Utc>>,
}

/// 作废一张发票后，应收单的发票指针该指向哪一张：同一应收下**已开票（ISSUED / POSTED）**
/// 的最新一张 —— 开票日新→旧，同日按创建时间新→旧。
///
/// 返回 `None` = 这张应收名下再没有生效过的发票，指针清空（与改造前一致）。
///
/// 为什么只认 `ISSUED` / `POSTED`：
/// * `DRAFT` 还没开票（没有凭证、没有收入），把它的号写进应收等于「虚报已开票」；
/// * `VOID` 已作废（凭证已红冲），更不能当指针。
///
/// `exclude` 是刚作废的那张（调用方在事务里已经把它的 `status` 改成 `VOID`，
/// 这里再显式排除一次，避免「状态还没落库」的调用顺序把它选回来）。
pub fn surviving_pointer<'a>(candidates: &'a [PointerCandidate], exclude: Uuid) -> Option<&'a PointerCandidate> {
    candidates
        .iter()
        .filter(|row| row.id != exclude && matches!(row.status.as_str(), STATUS_ISSUED | STATUS_POSTED))
        .max_by_key(|row| (row.invoice_date, row.created_at))
}

/// 作废一张发票后，把应收单上的发票指针（`invoice_no` / `invoice_date` / `voucher_id`
/// 三个拷贝列）改指「本应收下仍在生效的最新一张票」；没有就保持清空。
///
/// 只对「指针确实被本次作废清成 `NULL`」的行生效（`AND invoice_no IS NULL`）：
/// 指针本来指着别的发票时不做任何改动 —— 那说明有人按号写了别的票，别覆盖。
///
/// 为什么要回填而不是留空：`biz_receivable.invoice_no` 是「本笔应收开过哪张票」的唯一线索
/// （作废撤号只该撤掉**作废那张**）；留空会让一张「已开票 2260 / 未开票 3390」的应收
/// 在界面上显示成「未开票」，且 `receivable::cancel_for_stock_out` 靠 `voucher_id`
/// 判断「要不要一起红冲收入凭证」，留空会让出库作废漏冲 1122。
async fn restore_receivable_pointer(
    tx: &Transaction,
    invoice: &InvoiceRow,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    let Some(receivable_id) = invoice.receivable_id else {
        return Ok(());
    };
    // SCOPE-EXEMPT: 发票 / 应收台账均无归属列（见模块文档）。
    let rows = tx
        .query_sql(
            format!(
                "SELECT id, invoice_no, invoice_date, status, voucher_id, created_at \
                   FROM {TABLE} WHERE receivable_id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![Value::from(&receivable_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询同应收下的其它发票"))?;
    let mut candidates = Vec::with_capacity(rows.len());
    for row in &rows {
        candidates.push(PointerCandidate {
            id: row
                .get_typed("id")
                .map_err(|error| utils::db_error(error, "读取候选发票 id"))?,
            invoice_no: row
                .get_typed("invoice_no")
                .map_err(|error| utils::db_error(error, "读取候选发票号"))?,
            invoice_date: row
                .get_typed("invoice_date")
                .map_err(|error| utils::db_error(error, "读取候选开票日"))?,
            status: row
                .get_typed("status")
                .map_err(|error| utils::db_error(error, "读取候选发票状态"))?,
            voucher_id: row
                .get_typed("voucher_id")
                .map_err(|error| utils::db_error(error, "读取候选凭证号"))?,
            created_at: row
                .get_typed("created_at")
                .map_err(|error| utils::db_error(error, "读取候选创建时间"))?,
        });
    }
    let Some(picked) = surviving_pointer(&candidates, invoice.id) else {
        // 没有生效过的票 ⇒ 指针保持清空（clear_receivable_writeback 已经清过了）
        return Ok(());
    };
    tx.exec_sql(
        format!(
            "UPDATE {} SET invoice_no = ?, invoice_date = ?::text::date, \
                    voucher_id = NULLIF(?::text,'')::uuid, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND invoice_no IS NULL",
            receivable::TABLE
        ),
        vec![
            Value::Text(picked.invoice_no.clone()),
            Value::Text(picked.invoice_date.to_string()),
            Value::Text(picked.voucher_id.map(|value| value.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::Text(operator.map(|value| value.to_string()).unwrap_or_default()),
            Value::from(&receivable_id),
        ],
    )
    .await
    .map_err(|error| write_error(error, "回填应收的发票指针"))?;
    Ok(())
}

// ==================== 接口层 ====================

/// 开票（从应收 / 手工），落 `DRAFT`。
#[nik::permission(
    path = "/business/erp/invoice",
    method = "POST",
    permission = "business:erp:invoice:create"
)]
#[nik::post("/business/erp/invoice")]
pub async fn invoice_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateInvoiceReq>,
) -> ResResult<InvoiceDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create(operator, req).await?))
}

/// 发票详情。
#[nik::permission(
    path = "/business/erp/invoice/{id}",
    method = "GET",
    permission = "business:erp:invoice:detail"
)]
#[nik::get("/business/erp/invoice/{id}")]
pub async fn invoice_detail_handler(Path(id): Path<String>) -> ResResult<InvoiceDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(invoice_detail(id).await?))
}

/// 发票分页。
#[nik::permission(
    path = "/business/erp/invoice/page",
    method = "GET",
    permission = "business:erp:invoice:list"
)]
#[nik::get("/business/erp/invoice/page")]
pub async fn invoice_page_handler(Query(query): Query<InvoicePageQuery>) -> ResResult<PageData<Vec<InvoiceRes>>> {
    query.validate()?;
    let filter = query.to_filter()?;
    let (rows, total) = invoice_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询发票"))?;
    let list = rows.iter().map(|(row, name)| InvoiceRes::from_row(row, name)).collect();
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        filter.page,
        filter.page_size,
    ))
}

/// 状态迁移（开票 / 归档 / 作废）。
#[nik::permission(
    path = "/business/erp/invoice/{id}/status",
    method = "PATCH",
    permission = "business:erp:invoice:status"
)]
#[nik::patch("/business/erp/invoice/{id}/status")]
pub async fn invoice_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<StatusReq>,
) -> ResResult<InvoiceDetailRes> {
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
            ["DRAFT", "ISSUED", "POSTED", "VOID"],
            "发票状态域（ARCH-7 §4.5）不许混入应收的 OPEN / 收款的 CANCELLED"
        );
        for status in STATUSES {
            assert_ne!(status_label(status), "未知状态", "{status} 缺中文标签");
        }
        assert_eq!(status_label("OPEN"), "未知状态", "应收的 OPEN 不是发票状态");
        assert_eq!(
            status_label("CANCELLED"),
            "未知状态",
            "收款的 CANCELLED 不是发票状态（发票用 VOID）"
        );
        assert!(is_actionable(STATUS_DRAFT));
        assert!(is_actionable(STATUS_ISSUED));
        assert!(!is_actionable(STATUS_POSTED), "归档是终态");
        assert!(!is_actionable(STATUS_VOID), "作废是终态");
    }

    #[test]
    fn type_domain_and_labels() {
        assert_eq!(TYPES, ["OUT", "OUT_RED"]);
        assert_eq!(type_label(TYPE_OUT), "销项正票");
        assert_eq!(type_label(TYPE_OUT_RED), "销项红字票");
        assert_eq!(type_label("IN"), "未知票种");
        assert_eq!(validate_type(None).expect("缺省 OUT"), TYPE_OUT);
        assert_eq!(validate_type(Some(" OUT ")).expect("可带空白"), TYPE_OUT);
        let red = validate_type(Some("OUT_RED")).expect_err("本卡不开放手工建红字票");
        assert!(red.msg.contains("退货流程"), "文案要说明红字票从哪来：{}", red.msg);
        let bad = validate_type(Some("INVOICE")).expect_err("非法票种要拒");
        assert!(bad.msg.contains("INVOICE"), "{}", bad.msg);
    }

    #[test]
    fn edges_are_registered_and_consistent() {
        // 边表 = 共享注册表（改一处必须改两处，否则这里红）
        assert_eq!(LEGAL_EDGES, state_machine::INVOICE_EDGES);
        assert_eq!(
            state_machine::Entity::Invoice.edges(),
            &state_machine::INVOICE_EDGES[..]
        );
        assert_eq!(
            state_machine::Entity::Invoice.locked_statuses(),
            &state_machine::INVOICE_LOCKED[..]
        );
        assert_eq!(
            state_machine::Entity::Invoice.reason_edges(),
            &state_machine::INVOICE_REASON_EDGES[..]
        );
        assert_eq!(state_machine::Entity::Invoice.as_str(), "INVOICE");
        assert_eq!(state_machine::Entity::Invoice.label(), "销售发票");
        assert_eq!(
            state_machine::Entity::parse("INVOICE"),
            Some(state_machine::Entity::Invoice)
        );
        // 每条边都在状态域内，且没有入边回 DRAFT
        for (from, to) in LEGAL_EDGES {
            assert!(STATUSES.contains(&from), "边起点 {from} 不在状态域");
            assert!(STATUSES.contains(&to), "边终点 {to} 不在状态域");
            assert_ne!(to, STATUS_DRAFT, "没有任何边回到草稿");
        }
        // ARCH-7 §4.5 的四条边一条不少、一条不多
        assert_eq!(
            LEGAL_EDGES,
            [
                ("DRAFT", "ISSUED"),
                ("ISSUED", "POSTED"),
                ("DRAFT", "VOID"),
                ("ISSUED", "VOID"),
            ]
        );
        // reason 边必须同时是合法边（作废必须给原因）
        for edge in state_machine::INVOICE_REASON_EDGES {
            assert!(LEGAL_EDGES.contains(&edge), "reason 边 {edge:?} 不在合法边表里");
        }
        assert_eq!(
            state_machine::INVOICE_REASON_EDGES,
            [("DRAFT", "VOID"), ("ISSUED", "VOID")],
            "两条作废边都要原因"
        );
        assert_eq!(describe_next(STATUS_DRAFT), "ISSUED / VOID");
        assert_eq!(describe_next(STATUS_ISSUED), "POSTED / VOID");
        assert_eq!(describe_next(STATUS_POSTED), "无（终态）");
        assert_eq!(describe_next(STATUS_VOID), "无（终态）");
    }

    #[test]
    fn voucher_source_and_doc_kind_are_registered() {
        assert_eq!(VoucherSourceType::Invoice.as_str(), SOURCE_INVOICE);
        assert_eq!(SOURCE_INVOICE, "INVOICE");
        assert_eq!(
            VoucherSourceType::parse(SOURCE_INVOICE),
            Some(VoucherSourceType::Invoice)
        );
        assert_eq!(
            VoucherSourceType::Invoice.doc_kind(),
            Some(crate::business::shared::doc_kind::DocKind::Invoice)
        );
        assert_eq!(crate::business::shared::doc_kind::DocKind::Invoice.table(), Some(TABLE));
        assert_eq!(
            crate::business::shared::doc_kind::DocKind::Invoice.route(),
            Some("invoice")
        );
        assert!(crate::business::shared::doc_kind::DocKind::Invoice.is_document());
        assert_eq!(crate::business::shared::doc_kind::DocKind::Invoice.label(), "销售发票");
    }

    #[test]
    fn tax_and_total_math_is_self_consistent() {
        // 13%（百分数）正票：1000 不含税 → 130 税 → 1130 合计
        assert_eq!(tax_of(dec(1000.0), dec(13.0)), dec(130.0));
        assert_eq!(total_of(dec(1000.0), dec(130.0)), dec(1130.0));
        // 价税合计倒推：1130 / 1.13 = 1000
        let (amount, tax) = split_tax(dec(1130.0), dec(13.0));
        assert_eq!(amount, dec(1000.0));
        assert_eq!(tax, dec(130.0));
        assert_eq!(total_of(amount, tax), dec(1130.0), "倒推后必须恒等");
        // 除不尽的尾差落到税（合计恒等，库层 ck_biz_invoice_amount 才不会炸）
        let (amount, tax) = split_tax(dec(100.0), dec(13.0));
        assert_eq!(total_of(amount, tax), dec(100.0));
        // 0 税率：全额是收入
        let (amount, tax) = split_tax(dec(500.0), Decimal::ZERO);
        assert_eq!(amount, dec(500.0));
        assert_eq!(tax, Decimal::ZERO);
        // FIX-6 回归：百分数口径下 0.13 是「0.13%」（1000 × 0.13% = 1.30），与 13（1000 × 13% = 130）差 100 倍
        assert_eq!(tax_of(dec(1000.0), Decimal::new(13, 2)), dec(1.3));
    }

    #[test]
    fn resolve_amounts_covers_every_combination() {
        let rate = dec(13.0);
        // 只给合计：按税率倒推
        let (amount, tax, total) = resolve_amounts(None, None, Some(dec(1130.0)), rate).expect("只给合计可推");
        assert_eq!((amount, tax, total), (dec(1000.0), dec(130.0), dec(1130.0)));
        // 只给不含税：按税率算税
        let (amount, tax, total) = resolve_amounts(Some(dec(1000.0)), None, None, rate).expect("只给不含税可推");
        assert_eq!((amount, tax, total), (dec(1000.0), dec(130.0), dec(1130.0)));
        // 不含税 + 合计：税额取差
        let (amount, tax, total) =
            resolve_amounts(Some(dec(1000.0)), None, Some(dec(2260.0)), rate).expect("给两个可推");
        assert_eq!((amount, tax, total), (dec(1000.0), dec(1260.0), dec(2260.0)));
        // 三列都给且自洽
        let (_, _, total) =
            resolve_amounts(Some(dec(1000.0)), Some(dec(130.0)), Some(dec(1130.0)), rate).expect("三列自洽可过");
        assert_eq!(total, dec(1130.0));
        // 三列不自洽 → 400（别等库层 23514）
        let err =
            resolve_amounts(Some(dec(1000.0)), Some(dec(130.0)), Some(dec(1200.0)), rate).expect_err("不自洽要拒");
        assert!(err.msg.contains("不自洽"), "{}", err.msg);
        // 一个都不给 → 400
        let err = resolve_amounts(None, None, None, rate).expect_err("没金额要拒");
        assert!(err.msg.contains("金额一个都没给"), "{}", err.msg);
        // 只给税额 → 400（税额是结果不是输入）
        let err = resolve_amounts(None, Some(dec(130.0)), None, rate).expect_err("只给税额推不出不含税");
        assert!(err.msg.contains("推不出"), "{}", err.msg);
        // 税额 + 合计 → 不含税取差
        let (amount, tax, total) =
            resolve_amounts(None, Some(dec(130.0)), Some(dec(1130.0)), rate).expect("税额+合计可推");
        assert_eq!((amount, tax, total), (dec(1000.0), dec(130.0), dec(1130.0)));
    }

    #[test]
    fn tax_rate_bounds() {
        // FIX-6：百分数口径 0–100；13 = 13%，1 = 1%，100 = 100%
        assert_eq!(validate_tax_rate(dec(13.0)).expect("13% 可过"), dec(13.0));
        assert_eq!(validate_tax_rate(Decimal::ZERO).expect("0 税率可过"), Decimal::ZERO);
        assert_eq!(validate_tax_rate(Decimal::ONE).expect("1 = 1% 可过"), Decimal::ONE);
        assert_eq!(
            validate_tax_rate(Decimal::ONE_HUNDRED).expect("100% 可过"),
            Decimal::ONE_HUNDRED
        );
        assert!(validate_tax_rate(dec(100.01)).is_err(), "超过 100% 要拒");
        assert!(validate_tax_rate(dec(-0.01)).is_err(), "负税率要拒");
        assert_eq!(default_tax_rate(), dec(13.0), "默认 13%（V0327 的库层默认值）");
    }

    #[test]
    fn total_and_sign_rules() {
        assert_eq!(validate_total(dec(1130.0)).expect("正数可过"), dec(1130.0));
        assert!(validate_total(Decimal::ZERO).is_err(), "0 元发票无业务含义");
        assert!(validate_total(dec(-1.0)).is_err(), "负合计要拒");
        assert!(
            validate_total(Decimal::from(10_000_000_000_000i64)).is_err(),
            "超出 NUMERIC(18,4) 整数上限要拒"
        );
        validate_sign(TYPE_OUT, dec(1000.0), dec(130.0)).expect("正票正常");
        validate_sign(TYPE_OUT, dec(1000.0), Decimal::ZERO).expect("零税额正票合法");
        assert!(validate_sign(TYPE_OUT, Decimal::ZERO, Decimal::ZERO).is_err());
        assert!(validate_sign(TYPE_OUT, dec(1000.0), dec(-1.0)).is_err());
        // 红字票：金额为负、税 ≤ 0（与 ck_biz_invoice_sign 同口径）
        validate_sign(TYPE_OUT_RED, dec(-1000.0), dec(-130.0)).expect("红字票合法");
        assert!(validate_sign(TYPE_OUT_RED, dec(1000.0), dec(-130.0)).is_err());
        assert!(validate_sign(TYPE_OUT_RED, dec(-1000.0), dec(130.0)).is_err());
        assert!(validate_sign("IN", dec(1.0), Decimal::ZERO).is_err());
    }

    #[test]
    fn headroom_rejects_over_invoicing() {
        // 应收 1130，已开 0 → 可开 1130
        assert_eq!(headroom_of(dec(1130.0), Decimal::ZERO), dec(1130.0));
        validate_headroom("AR-1", dec(1130.0), Decimal::ZERO, dec(1130.0)).expect("刚好开满可以");
        validate_headroom("AR-1", dec(300.0), dec(830.0), dec(1130.0)).expect("分次开票（300 + 830 = 1130）可以");
        // 超额：1130.0001 > 1130 - 0
        let err =
            validate_headroom("AR-1", dec(1130.0001), Decimal::ZERO, dec(1130.0)).expect_err("超过应收金额必须拒");
        assert_eq!(err.code, error_code::RESOURCE_CONFLICT, "额度超限是 409");
        assert!(err.msg.contains("AR-1"), "文案要点名应收单号：{}", err.msg);
        assert!(err.msg.contains("1130.0001"), "文案要点名本次金额：{}", err.msg);
        assert!(err.msg.contains("可开票额度"), "{}", err.msg);
        // 已开满再开 1 分也要拒
        let err = validate_headroom("AR-1", dec(0.01), dec(1130.0), dec(1130.0)).expect_err("额度用完要拒");
        assert!(err.msg.contains("0"), "额度 0 要点名：{}", err.msg);
        // 库层已漂移（已开 > 应收）：先排查，别继续开
        let err = validate_headroom("AR-1", dec(1.0), dec(1200.0), dec(1130.0)).expect_err("已开超应收要拒");
        assert!(err.msg.contains("漂移"), "{}", err.msg);
    }

    #[test]
    fn invoice_no_shape() {
        assert_eq!(validate_invoice_no(None).expect("空 = 自动取号"), None);
        assert_eq!(validate_invoice_no(Some("   ")).expect("空白 = 自动取号"), None);
        assert_eq!(
            validate_invoice_no(Some(" 25337000000012345678 ")).expect("手填税控号"),
            Some("25337000000012345678".to_string())
        );
        let err = validate_invoice_no(Some("2533 7000")).expect_err("带空格要拒");
        assert!(err.msg.contains("空白"), "{}", err.msg);
        assert!(
            validate_invoice_no(Some(&"1".repeat(NO_MAX_CHARS + 1))).is_err(),
            "超过 VARCHAR(64) 要拒"
        );
        assert_eq!(validate_invoice_code(None).expect("可空"), None);
        assert!(validate_invoice_code(Some(&"9".repeat(CODE_MAX_CHARS + 1))).is_err());
    }

    #[test]
    fn source_type_is_normalized_into_check_domain() {
        for value in SOURCE_TYPES {
            assert_eq!(
                normalize_source_type(Some(value)),
                Some(value.to_string()),
                "{value} 是白名单值，不许被改"
            );
        }
        assert_eq!(
            normalize_source_type(Some("PURCHASE_ORDER")),
            Some("MANUAL".to_string()),
            "域外来源归 MANUAL（否则撞 ck_biz_invoice_source）"
        );
        assert_eq!(normalize_source_type(None), None);
        assert_eq!(normalize_source_type(Some("  ")), None);
    }

    #[test]
    fn period_code_and_date_shape() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 6).expect("合法日期");
        assert_eq!(period_code_of(date), "2026-11");
        let january = NaiveDate::from_ymd_opt(2027, 1, 5).expect("合法日期");
        assert_eq!(period_code_of(january), "2027-01", "月份必须补零");
        assert!(parse_date_arg("2026-11-06", "invoice_date").is_ok());
        assert!(
            parse_date_arg("2026-1-6", "invoice_date").is_err(),
            "不补零的日期要拒（口径统一 yyyy-MM-dd）"
        );
    }

    #[test]
    fn reason_bounds() {
        assert!(validate_reason("  无  ").is_err(), "太短不算依据");
        assert_eq!(
            validate_reason("  客户要求重开，金额开错  ").expect("正常原因"),
            "客户要求重开，金额开错"
        );
        assert!(validate_reason(&"字".repeat(REASON_MAX_CHARS + 1)).is_err(), "超长要拒");
    }

    #[test]
    fn accounts_are_the_ones_that_actually_exist() {
        // 稿面 6001 在本仓科目表里不存在（V0183 种的是 5xxx 损益类）
        assert_eq!(ACCOUNT_REVENUE, "5001");
        assert_eq!(ACCOUNT_AR, "1122");
        assert_eq!(ACCOUNT_OUTPUT_TAX, "2221.02", "2221 是汇总科目，记账必须落叶子");
        assert_eq!(SEQ_TYPE, "IV");
        assert_eq!(NO_PREFIX, "IV");
        assert_eq!(INVOICE_SEQ.seq_type, SEQ_TYPE);
    }

    /// ATT-07：票面形式归一 —— 不传 = 电子票；显式 false 才是纸质票。
    ///
    /// 这里刻意钉住「不从 invoice_code 推断」：纸质票可以现场开完再补录税控代码，
    /// 电子票也可能带代码，用代码有无反推票面形式会静默改掉用户的附件口径。
    #[test]
    fn is_electronic_defaults_to_electronic() {
        assert!(resolve_is_electronic(None), "不传 is_electronic = 电子票");
        assert!(resolve_is_electronic(Some(true)));
        assert!(!resolve_is_electronic(Some(false)), "显式 false 是纸质票");
        assert_eq!(electronic_label(true), "电子票");
        assert_eq!(electronic_label(false), "纸质票");
    }

    /// 列清单必须带 `is_electronic`：漏了它 `InvoiceRow::from_row` 会在运行期报
    /// `column "is_electronic" not found`（编译期抓不到，所以用文本断言钉住）。
    #[test]
    fn column_lists_carry_is_electronic() {
        for (name, columns) in [("COLUMNS", COLUMNS), ("LIST_COLUMNS", LIST_COLUMNS)] {
            let found = columns
                .split(',')
                .map(str::trim)
                .any(|column| column == "is_electronic" || column.strip_prefix("p.") == Some("is_electronic"));
            assert!(found, "{name} 缺 is_electronic：{columns}");
        }
        assert!(
            LIST_COLUMNS.contains("p.is_electronic"),
            "列表 SQL 用别名 p，必须写 p.is_electronic（写成裸列会与 JOIN 的 customer 列歧义）"
        );
    }

    /// 作废后的应收指针选择：只认 ISSUED / POSTED，按（开票日, 创建时间）取最新。
    ///
    /// 钉住三件事：
    /// 1. 作废的那张（即使状态还没落库）不会被选回来；
    /// 2. `DRAFT` 不能被当成「已开票」写进应收（否则应收虚报已开票）；
    /// 3. 同日多张票时，创建时间更新的是更「晚」的那张。
    #[test]
    fn surviving_pointer_skips_void_and_draft() {
        let day = |text: &str| NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期");
        let at = |secs: i64| DateTime::<Utc>::from_timestamp(secs, 0);
        let candidate = |no: &str, date: &str, status: &str, created: Option<DateTime<Utc>>| PointerCandidate {
            id: Uuid::new_v4(),
            invoice_no: no.to_string(),
            invoice_date: day(date),
            status: status.to_string(),
            voucher_id: None,
            created_at: created,
        };
        let void_one = candidate("IV-A", "2026-11-09", STATUS_VOID, at(3));
        let draft = candidate("IV-B", "2026-11-08", STATUS_DRAFT, at(2));
        let posted = candidate("IV-C", "2026-11-07", STATUS_POSTED, at(1));
        let issued = candidate("IV-D", "2026-11-09", STATUS_ISSUED, at(2));
        let all = vec![void_one.clone(), draft.clone(), posted.clone(), issued.clone()];

        let picked = surviving_pointer(&all, void_one.id).expect("还有生效中的票");
        assert_eq!(
            picked.invoice_no, "IV-D",
            "同日更晚创建的那张 + 状态必须是 ISSUED/POSTED 才算"
        );

        // 只剩 VOID / DRAFT ⇒ 没有可指的票（指针应该清空）
        let dead_only = vec![void_one.clone(), draft];
        let none = surviving_pointer(&dead_only, void_one.id);
        assert!(none.is_none(), "VOID / DRAFT 都不能当应收的发票指针");

        // 显式排除：即便被排除的那张是唯一一张 ISSUED，也不能选它
        let only_issued = vec![issued.clone()];
        assert!(
            surviving_pointer(&only_issued, issued.id).is_none(),
            "刚作废的那张不能被自己的 restore 选回来"
        );
        println!("[INV01-①] 作废后指针选择：跳过 VOID/DRAFT + 同日取最新 ✓");
    }
}
