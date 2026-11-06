//! 应收单写侧（销售链 · SO-13）：**手工挂账** + **坏账核销**，镜像 `payable.rs`。
//!
//! 表结构以 `backend/migrations/business/V0287__sales_receivable.up.sql` 为准（SO-12 建表）：
//!
//! ```text
//! biz_receivable  id, ar_no, customer_id, source_type, source_id, source_no,
//!                 invoice_no, invoice_date, currency_code, amount, received_amount,
//!                 unsettled_amount, credit_days, due_date, period_code,
//!                 status(OPEN/PARTIAL/SETTLED/WRITTEN_OFF), settled_at, voucher_id, remark
//!                 + 审计五件套（created_at / updated_at / create_by / update_by / deleted_at）
//!                 CHECK unsettled_amount = amount - received_amount   ← 恒等（程序算错被拦）
//!                 CHECK amount > 0 AND 0 <= received_amount <= amount
//!                 CHECK status <> 'SETTLED' OR unsettled_amount = 0
//!                 UNIQUE (source_type, source_id) WHERE deleted_at IS NULL AND source_type <> 'MANUAL'
//! ```
//!
//! # 状态域（**不是** payable 的那一套）
//!
//! | 本表 | payable 对应值 | 含义 |
//! |------|----------------|------|
//! | `OPEN` | `OPEN` | 一分没收 |
//! | `PARTIAL` | `PARTIAL_PAID` | 部分已收 |
//! | `SETTLED` | `SETTLED` | 收清 |
//! | `WRITTEN_OFF` | `CANCELLED` | **坏账核销**（收不回来了，终态） |
//! | `CANCELLED` | （无） | **出库作废冲销**（来源出库单作废 ⇒ 这笔销售从未发生，终态；V0290） |
//!
//! 边表是 [`LEGAL_EDGES`]（= `state_machine::RECEIVABLE_EDGES`，6 条），全部经
//! `state_machine::transition` 落库并写 `biz_status_log`（`entity_type = RECEIVABLE`）。
//!
//! ⚠ `WRITTEN_OFF`（坏账）与 `CANCELLED`（作废）**不能混用**：
//! 坏账报表按 `status = 'WRITTEN_OFF'` 取数统计减值损失；作废单不进坏账报表，
//! 也不进账龄（账龄取 `OPEN` / `PARTIAL`，见 [`has_balance`]）。
//!
//! # 手工挂账（`POST /business/erp/receivable`）
//!
//! 给「没有销售出库单做来源」的应收补一张账：押金、代垫运费、无出库的历史欠款。
//!
//! ```text
//! 单号    biz_seq(seq_type='AR') → AR-yyyyMMdd-000001（CST 天滚动）
//! 来源    source_type='MANUAL'，source_id=服务端生成 UUID，source_no='MANUAL'
//!         （`uk_biz_ar_source` 对 MANUAL 例外 ⇒ 同一天可挂多笔手工应收）
//! 金额    amount 由请求给（**必须 > 0**，库层无默认值）；received_amount=0，
//!         unsettled_amount=amount（恒等式自己算好一起写，否则 23514）
//! 到期日  due_date 显式给优先，否则 = 挂账日（CST 今天）+ credit_days
//! 期间    period_code 显式给优先，否则 = 挂账日所属期间；**已结账期间 400**
//! 客户    必须存在且未软删；`FROZEN`（冻结）客户 → 409（与「禁开新单」同口径）
//! ```
//!
//! # 坏账核销（`PATCH /business/erp/receivable/{id}/write-off`）
//!
//! `OPEN` / `PARTIAL` → `WRITTEN_OFF`，**必须给原因**（`RECEIVABLE_REASON_EDGES`），
//! 落 `settled_at`（核销时间）。`SETTLED` / `WRITTEN_OFF` / `CANCELLED` 再核销 → 409（终态）。
//!
//! # 出库作废冲销（`cancel_for_stock_out`，由 `receivable_bridge` 调用）
//!
//! `OPEN`（且 `received_amount = 0`）→ `CANCELLED`，必须给原因（上游出库单的作废原因）。
//! 入口唯一：`stockout_handler::cancel_out_tx` 在「三本账回滚 + 红字凭证」之后的**同一个事务**里调用。
//! 已收过款的应收 → 409（先冲收款 / 收款退款单），库层 `ck_biz_ar_cancelled` 兜底。
//!
//! ## 本卡**不生成**核销凭证（边界，别当成漏了）
//!
//! ARCH-7 §7.2 ④ 的坏账分录（借 6702 资产减值损失 / 贷 1122 应收账款）要**先有**
//! `biz_voucher_rule` 的 6702 科目映射与「资产减值损失」科目，属于「收入/减值凭证」
//! 那一组卡（应收挂账凭证 + 收款凭证 + 发票凭证一起做，口径才统一）。
//! 本卡把 `voucher_id` 原样保留不动，核销只改状态与时间；
//! 在凭证卡落地前**不要**在这里手搓分录 —— 手搓出来的凭证过不了 `voucher::post_voucher`
//! 的科目映射与借贷平衡校验，只会给后面留一张脏凭证。
//!
//! # 事务边界
//!
//! 写函数吃 `&impl SqlExecutor`（insert / write_off 的落库那半段），
//! **由调用方（HTTP 入口 / SO-14 的出库过账）开事务**：应收与凭证、库存必须同生共死。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_receivable 是应收台账（V0287 建表，无 dept_id / 归属人列），
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:receivable:* 权限码控制。
//! SCOPE-EXEMPT: biz_customer 是客户主数据（全局），没有归属列。
//! ```

use axum::Json;
use axum::extract::Path;
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::period;
use crate::business::erp::voucher::{self, SeqSpec};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0288 的 sys_menu.auth_code 逐字一致） ====================

/// 手工挂账。
pub const PERM_CREATE: &str = "business:erp:receivable:create";
/// 坏账核销。
pub const PERM_WRITE_OFF: &str = "business:erp:receivable:write-off";

// ==================== 常量 ====================

/// 应收单表。
pub const TABLE: &str = "biz_receivable";

/// 客户主数据表。
pub const CUSTOMER_TABLE: &str = "biz_customer";

/// 状态：未收。
pub const STATUS_OPEN: &str = "OPEN";
/// 状态：部分已收。
pub const STATUS_PARTIAL: &str = "PARTIAL";
/// 状态：已收清。
pub const STATUS_SETTLED: &str = "SETTLED";
/// 状态：坏账核销（终态）。
pub const STATUS_WRITTEN_OFF: &str = "WRITTEN_OFF";
/// 状态：已作废（来源出库单作废 ⇒ 这笔销售从未发生，终态）。V0290。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 状态白名单（与 `ck_biz_ar_status` 逐字一致，单测钉住）。
pub const STATUSES: [&str; 5] = [
    STATUS_OPEN,
    STATUS_PARTIAL,
    STATUS_SETTLED,
    STATUS_WRITTEN_OFF,
    STATUS_CANCELLED,
];

/// 有余额的状态（`OPEN` / `PARTIAL`）—— **账龄 / 逾期 / 收款只看这两种**。
///
/// 三种终态各有各的「不再是应收资产」的理由：`SETTLED` 收清了、
/// `WRITTEN_OFF` 核销成损失了、`CANCELLED` 对应的销售根本没发生。
pub fn has_balance(status: &str) -> bool {
    status == STATUS_OPEN || status == STATUS_PARTIAL
}

/// 来源类型：手工挂账。
///
/// `uk_biz_ar_source … WHERE source_type <> 'MANUAL'` 对它例外 ⇒ 同一「来源」可挂多笔
/// （手工应收本来就没有业务来源单据，`source_id` 由服务端生成）。
pub const SOURCE_MANUAL: &str = "MANUAL";

/// 手动挂账的来源单号（与 `source_type = 'MANUAL'` 配套）。
pub const SOURCE_NO_MANUAL: &str = "MANUAL";

/// 单号前缀（`AR-yyyyMMdd-000001`）。
pub const NO_PREFIX: &str = "AR";
/// `biz_seq.seq_type`（V0287 已种）。
pub const SEQ_TYPE: &str = "AR";

/// 应收单取号规则。
pub const RECEIVABLE_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: SEQ_TYPE,
    prefix: NO_PREFIX,
    seq_name: "应收单号",
    remark: "应收单号：AR-yyyyMMdd-000001，按天（CST）滚动",
};

/// 合法边（唯一真相在 `state_machine::RECEIVABLE_EDGES`，这里只是别名）。
pub const LEGAL_EDGES: [(&str, &str); 6] = state_machine::RECEIVABLE_EDGES;

/// 入账态（唯一真相在 `state_machine::RECEIVABLE_LOCKED`）。
pub const LOCKED: [&str; 3] = state_machine::RECEIVABLE_LOCKED;

/// 核销原因最短长度（短于这个的「坏账」「无」不算可追溯依据）。
pub const REASON_MIN_CHARS: usize = 4;
/// 核销原因最长长度（`remark` 是 TEXT，这里只是不给「一大段粘贴」）。
pub const REASON_MAX_CHARS: usize = 500;

/// 出库作废冲销写进 `remark` 的前缀（与「[坏账核销]」同形，一眼能看出这条应收怎么终止的）。
pub const CANCEL_REASON_PREFIX: &str = "[出库作废冲销]";

/// `biz_receivable` 列清单（与 V0287 逐列对应）。
pub const COLUMNS: &str = "id, ar_no, customer_id, source_type, source_id, source_no, \
     invoice_no, invoice_date, currency_code::text AS currency_code, amount, received_amount, \
     unsettled_amount, credit_days, due_date, period_code, status, settled_at, voucher_id, remark, \
     created_at, updated_at, create_by, update_by, deleted_at";

// ==================== 取号 ====================

/// 取一个应收单号。
pub async fn next_receivable_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    voucher::next_doc_no(ex, &RECEIVABLE_SEQ, now).await
}

// ==================== 行结构 ====================

/// 应收单行（**不含**客户名称；带名称的读模型在 `receivable_query::ReceivableRow`）。
#[derive(Debug, Clone)]
pub struct ReceivableRow {
    /// 主键。
    pub id: Uuid,
    /// 应收单号。
    pub ar_no: String,
    /// 客户。
    pub customer_id: Uuid,
    /// 来源单据类型（`MANUAL` / `SALES_OUT` / `SHIPMENT`）。
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
    /// 收入确认凭证（本卡不写）。
    pub voucher_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: Option<DateTime<Utc>>,
    /// 更新时间。
    pub updated_at: Option<DateTime<Utc>>,
}

impl ReceivableRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            ar_no: row.get_typed("ar_no")?,
            customer_id: row.get_typed("customer_id")?,
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

/// 按主键取一张应收单（未软删）。
pub async fn find_by_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ReceivableRow>> {
    // SCOPE-EXEMPT: 应收台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!("SELECT {COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(ReceivableRow::from_row).transpose()
}

/// 按来源单据取应收单（`uk_biz_ar_source` 保证非 MANUAL 来源最多一张）。
///
/// SO-14（出库 → 应收挂账）的幂等闸就靠它：过账重试先查这里，命中即返回已有行。
pub async fn find_by_source(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Uuid,
) -> DbResult<Option<ReceivableRow>> {
    // SCOPE-EXEMPT: 应收台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {COLUMNS} FROM {TABLE} \
                  WHERE source_type = ? AND source_id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![
                Value::Text(source_type.to_string()),
                Value::from(&source_id),
            ],
        )
        .await?;
    row.as_ref().map(ReceivableRow::from_row).transpose()
}

// ==================== 纯逻辑（校验 / 金额 / 到期日） ====================

/// 未收金额恒等（`unsettled = amount - received`，与 `ck_biz_ar_unsettled` 同口径）。
pub fn unsettled_of(amount: Decimal, received: Decimal) -> Decimal {
    (amount - received).round_dp(4)
}

/// 到期日 = 挂账日 + 账期天数。
pub fn due_date_of(posting_date: NaiveDate, credit_days: i32) -> NaiveDate {
    posting_date + nik::prelude::chrono::Duration::days(i64::from(credit_days))
}

/// 请求里的金额校验：`> 0` 且不超过两位小数以上的精度上限（`NUMERIC(18,4)`）。
pub fn validate_amount(amount: Decimal) -> std::result::Result<Decimal, ApiError> {
    if amount <= Decimal::ZERO {
        return Err(bad_request(format!(
            "amount 必须大于 0（零元 / 负数应收没有业务含义），收到 {amount}"
        )));
    }
    // NUMERIC(18,4) 的整数部分最多 14 位
    if amount >= Decimal::from(10_000_000_000_000i64) {
        return Err(bad_request(format!(
            "amount 超出 NUMERIC(18,4) 上限（整数部分最多 14 位），收到 {amount}"
        )));
    }
    Ok(amount.round_dp(4))
}

/// 账期天数校验（`ck_biz_ar_credit`：`credit_days >= 0`）。
pub fn validate_credit_days(credit_days: i32) -> std::result::Result<i32, ApiError> {
    if credit_days < 0 {
        return Err(bad_request(format!(
            "credit_days 不能为负（到期日不能早于挂账日），收到 {credit_days}"
        )));
    }
    Ok(credit_days)
}

/// 核销原因校验（必填、去空白、长度区间）。
pub fn validate_reason(reason: &str) -> std::result::Result<String, ApiError> {
    let text = reason.trim();
    let chars = text.chars().count();
    if chars < REASON_MIN_CHARS {
        return Err(bad_request(format!(
            "坏账核销必须给出原因（至少 {REASON_MIN_CHARS} 个字，例如「客户已注销，经理批准核销」），收到「{text}」"
        )));
    }
    if chars > REASON_MAX_CHARS {
        return Err(bad_request(format!(
            "坏账核销原因不能超过 {REASON_MAX_CHARS} 个字，收到 {chars} 个字"
        )));
    }
    Ok(text.to_string())
}

/// `yyyy-MM` 期间校验（严格：长度 7、第 5 位是 `-`、月份 01–12）。
pub fn validate_period(raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim();
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 7 && bytes[4] == b'-';
    let digits_ok = shaped
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || byte.is_ascii_digit());
    let month_ok = digits_ok
        && text[5..7]
            .parse::<u32>()
            .map(|month| (1..=12).contains(&month))
            .unwrap_or(false);
    if !month_ok {
        return Err(bad_request(format!(
            "period_code 必须是 yyyy-MM（月份 01–12），收到「{text}」"
        )));
    }
    Ok(text.to_string())
}

/// `yyyy-MM-dd` 日期校验（严格长度与分隔符，`2026-1-1` 不认）。
pub fn parse_date_arg(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    let text = raw.trim();
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 10 && bytes[4] == b'-' && bytes[7] == b'-';
    let digits_ok = shaped
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
    if !digits_ok {
        return Err(bad_request(format!(
            "{field} 必须是 yyyy-MM-dd（例：2026-11-05），收到「{text}」"
        )));
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| bad_request(format!("{field} 不是合法日期：{text}")))
}

/// 参数类 400（`10001`）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 落库 ====================

/// 落库入参（**已校验**的形态；由 [`create_manual`] / SO-14 的过账挂账单构造）。
#[derive(Debug, Clone)]
pub struct NewReceivable<'a> {
    /// 客户。
    pub customer_id: Uuid,
    /// 来源类型（`MANUAL` / `SALES_OUT` / `SHIPMENT`）。
    pub source_type: &'a str,
    /// 来源单据 id（MANUAL 由调用方生成 UUID）。
    pub source_id: Uuid,
    /// 来源单号。
    pub source_no: &'a str,
    /// 应收金额（> 0）。
    pub amount: Decimal,
    /// 已收金额（新单恒 0）。
    pub received_amount: Decimal,
    /// 账期天数。
    pub credit_days: i32,
    /// 到期日。
    pub due_date: NaiveDate,
    /// 所属会计期间。
    pub period_code: &'a str,
    /// 发票号。
    pub invoice_no: Option<&'a str>,
    /// 发票日期。
    pub invoice_date: Option<NaiveDate>,
    /// 备注。
    pub remark: Option<&'a str>,
    /// 收入确认凭证（本卡手工挂账为 `None`）。
    pub voucher_id: Option<Uuid>,
}

/// 写一张应收单（`status = OPEN`），并写建单状态日志（`NULL → OPEN`，同一事务）。
///
/// 金额恒等式在这里算好一起写：`unsettled_amount = amount - received_amount`
/// （只给 amount 会让 unsettled 落到默认 0 而被 `ck_biz_ar_unsettled` 拒成 23514）。
///
/// 建单日志的来源说明固定写「手工挂账」—— 自动挂账（销售出库过账）走
/// [`insert_with_reason`]，别为了改一句日志文案去抄整个 INSERT。
pub async fn insert(
    ex: &impl SqlExecutor,
    new: &NewReceivable<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<(Uuid, String)> {
    insert_with_reason(ex, new, operator, now, CREATED_REASON_MANUAL).await
}

/// 建单状态日志的来源说明（手工挂账）。
pub const CREATED_REASON_MANUAL: &str = "手工挂账";

/// [`insert`] 的可定制说明版：`created_reason` 落 `biz_status_log.reason`
/// （`NULL → OPEN` 那条），用于区分「手工挂账」与「销售出库过账自动挂账」（SO-14）。
///
/// 除了那一句说明，两个入口的落库行为**完全一致**（同一个 INSERT、同一条状态日志）。
pub async fn insert_with_reason(
    ex: &impl SqlExecutor,
    new: &NewReceivable<'_>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
    created_reason: &str,
) -> DbResult<(Uuid, String)> {
    let ar_no = next_receivable_no(ex, now).await?;
    let amount = new.amount.round_dp(4);
    let received = new.received_amount.round_dp(4);
    let unsettled = unsettled_of(amount, received);
    let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();
    // SCOPE-EXEMPT: 应收台账无归属列（见模块文档）。
    let inserted = ex
        .query_first_sql(
            format!(
                "INSERT INTO {TABLE} \
                    (ar_no, customer_id, source_type, source_id, source_no, invoice_no, invoice_date, \
                     currency_code, amount, received_amount, unsettled_amount, credit_days, due_date, \
                     period_code, status, settled_at, voucher_id, remark, \
                     created_at, updated_at, create_by, update_by) \
                 VALUES (?, ?::uuid, ?, ?::uuid, ?, NULLIF(?::text,''), \
                         NULLIF(?::text,'')::date, 'CNY', NULLIF(?::text,'')::numeric, \
                         NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, ?, \
                         NULLIF(?::text,'')::date, ?, '{STATUS_OPEN}', NULL, \
                         NULLIF(?::text,'')::uuid, NULLIF(?::text,''), ?, ?, \
                         NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
                 RETURNING id"
            ),
            vec![
                Value::Text(ar_no.clone()),
                Value::from(&new.customer_id),
                Value::Text(new.source_type.to_string()),
                Value::from(&new.source_id),
                Value::Text(new.source_no.to_string()),
                Value::Text(new.invoice_no.unwrap_or_default().to_string()),
                Value::Text(new.invoice_date.map(|d| d.to_string()).unwrap_or_default()),
                Value::Text(amount.to_string()),
                Value::Text(received.to_string()),
                Value::Text(unsettled.to_string()),
                Value::from(new.credit_days),
                Value::Text(new.due_date.to_string()),
                Value::Text(new.period_code.to_string()),
                Value::Text(
                    new.voucher_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                ),
                Value::Text(new.remark.unwrap_or_default().to_string()),
                Value::from(now),
                Value::from(now),
                Value::Text(operator_text.clone()),
                Value::Text(operator_text),
            ],
        )
        .await?;
    match inserted.as_ref() {
        Some(row) => {
            let id: Uuid = row.get_typed("id")?;
            // P10 铁律 ③：建单留痕（`NULL → OPEN`），与建单同一事务
            state_machine::log_created(
                ex,
                state_machine::ENTITY_RECEIVABLE,
                id,
                &ar_no,
                STATUS_OPEN,
                operator,
                Some(created_reason),
            )
            .await
            .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
            Ok((id, ar_no))
        }
        None => Err(DbError::InvalidValue(format!(
            "应收单写入失败：INSERT 没有返回 id（来源 {}）",
            new.source_no
        ))),
    }
}

/// 坏账核销（`OPEN`/`PARTIAL` → `WRITTEN_OFF`），落 `settled_at` 与原因。
///
/// 两道：`state_machine::transition`（边表 + reason + `biz_status_log`）→
/// `UPDATE … WHERE status = from`（CAS，防并发核销把两次都当成功）。同一事务。
///
/// 返回核销后的行。
pub async fn write_off(
    ex: &impl SqlExecutor,
    id: Uuid,
    reason: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<ReceivableRow, ApiError> {
    let row = find_by_id(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "查询应收单"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                "应收单不存在或已删除".to_string(),
            )
        })?;
    if LOCKED.contains(&row.status.as_str()) {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "应收单 {} 当前是「{}」，不能再核销：{} 是终态（要冲回已收清的单子走收款红冲，不是坏账）",
                row.ar_no,
                status_label(&row.status),
                status_label(&row.status)
            ),
        ));
    }
    // 已结账期间不许做改账动作（核销会影响当期损益）
    period::ensure_period_writable(ex, &row.period_code, "核销应收").await?;

    state_machine::transition(
        ex,
        state_machine::ENTITY_RECEIVABLE,
        row.id,
        &row.ar_no,
        &LEGAL_EDGES,
        &row.status,
        STATUS_WRITTEN_OFF,
        operator,
        Some(reason),
        None,
    )
    .await?;

    let remark = match row.remark.as_deref().map(str::trim) {
        Some(old) if !old.is_empty() => format!("{old}\n[坏账核销] {reason}"),
        _ => format!("[坏账核销] {reason}"),
    };
    let updated = ex
        .exec_sql(
            format!(
                "UPDATE {TABLE} \
                    SET status = '{STATUS_WRITTEN_OFF}', settled_at = ?, updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid, remark = ? \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                Value::from(now),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(remark),
                Value::from(&row.id),
                Value::Text(row.status.clone()),
            ],
        )
        .await?;
    if updated == 0 {
        // CAS 失配：另一个会话刚把它核销 / 收清了（本事务整体会回滚，日志不会半条落地）
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "应收单 {} 已被其他操作改为「{}」，本次核销未生效（请刷新后重试）",
                row.ar_no,
                status_label(&row.status)
            ),
        ));
    }
    find_by_id(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "读取核销后的应收单"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                "应收单核销后消失（并发软删？）".to_string(),
            )
        })
}

/// 出库作废 ⇒ 冲销这张自动挂上的应收（`OPEN` → `CANCELLED`）。
///
/// # 只允许「一分没收」的应收
///
/// ```text
/// status = OPEN 且 received_amount = 0   → 允许作废（钱没动过，单据作废 = 销售没发生）
/// PARTIAL / SETTLED（收过钱）             → 409：钱已经进账，冲回必须走收款退款单
/// WRITTEN_OFF（已坏账核销）               → 409：核销凭证已出，先处理核销再作废
/// CANCELLED（已作废）                     → 409：重复作废（调用方先判幂等）
/// ```
///
/// 库层还有 `ck_biz_ar_cancelled`（`status <> 'CANCELLED' OR received_amount = 0`）兜底，
/// 程序算错 / 绕过 handler 也改不出「收过钱的已作废应收」。
///
/// # 事务边界（与出库作废同生共死）
///
/// 本函数**不开事务**，必须由 `stockout_handler::cancel_out_tx` 在它的事务里调用：
/// 三本账回滚 / 出库红字凭证 / 应收作废要么全成、要么全回滚。
/// 顺序与过账侧对称：过账是「三本账 → 成本凭证 → 挂应收」，作废是「回滚三本账 → 红字凭证 → 冲销应收」。
///
/// # 期间闸（由同一事务里的凭证层把住，别在这里重复加）
///
/// 作废一条已过账出库单时，`cancel_out_tx` 会先把成本结转凭证红冲：
/// `voucher::reverse_posted` 拒绝「原凭证所属期间已结账」的红冲（`is_period_closed`），
/// `post_voucher` 也拒绝往已结账期间记账。两步都在本函数**之前**的同一事务里发生，
/// 所以「期间已结账」时整笔作废（含应收冲销）根本走不到这里 —— 不需要在应收侧
/// 再查一次 `period::ensure_period_writable`（查了只会让「销售来源出库单」与
/// 「领用出库」的报错顺序不同）。
///
/// 唯一例外：出库单当时零成本、没生成成本结转凭证（`reverse_by_source` 返回 `None`），
/// 那作废本身就没有会计后果，把 `OPEN` 应收标成 `CANCELLED` 也不需要过期间闸
/// （这张应收此刻也不可能有收入凭证——`voucher_id` 仍为空）。
pub async fn cancel_for_stock_out(
    ex: &impl SqlExecutor,
    id: Uuid,
    reason: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<ReceivableRow, ApiError> {
    let row = find_by_id(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "查询应收单"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                "应收单不存在或已删除".to_string(),
            )
        })?;
    if row.status == STATUS_CANCELLED {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "应收单 {} 已经是「{}」",
                row.ar_no,
                status_label(&row.status)
            ),
        ));
    }
    if row.status != STATUS_OPEN || row.received_amount > Decimal::ZERO {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "应收单 {} 当前是「{}」（已收 {}）：已收过款的应收不能作出库作废冲销，\
                 请先冲收款（收款退款单）或先处理核销，再作废出库单",
                row.ar_no,
                status_label(&row.status),
                row.received_amount.round_dp(2)
            ),
        ));
    }
    let reason = validate_reason(reason)?;

    state_machine::transition(
        ex,
        state_machine::ENTITY_RECEIVABLE,
        row.id,
        &row.ar_no,
        &LEGAL_EDGES,
        &row.status,
        STATUS_CANCELLED,
        operator,
        Some(&reason),
        None,
    )
    .await?;

    let remark = match row.remark.as_deref().map(str::trim) {
        Some(old) if !old.is_empty() => format!("{old}\n{CANCEL_REASON_PREFIX} {reason}"),
        _ => format!("{CANCEL_REASON_PREFIX} {reason}"),
    };
    // CAS：`status = 'OPEN' AND received_amount = 0` —— 并发收款把状态推到 PARTIAL 时这里写 0 行，
    // 整笔出库作废回滚（不会出现「应收显示已作废、实际已收款」这种账）。
    let updated = ex
        .exec_sql(
            format!(
                "UPDATE {TABLE} \
                    SET status = '{STATUS_CANCELLED}', settled_at = ?, updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid, remark = ? \
                  WHERE id = ?::uuid AND status = '{STATUS_OPEN}' AND received_amount = 0 \
                    AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                Value::from(now),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(remark),
                Value::from(&row.id),
            ],
        )
        .await?;
    if updated == 0 {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "应收单 {} 在本次作废期间被收款 / 改动（不再是未收款 OPEN），出库作废已整笔回滚",
                row.ar_no
            ),
        ));
    }
    find_by_id(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "读取作废后的应收单"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                "应收单作废后消失（并发软删？）".to_string(),
            )
        })
}

// ==================== 入参 / 出参 ====================

/// 手工挂账请求（`POST /business/erp/receivable`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CreateReceivableReq {
    /// 客户 id（必填）。
    pub customer_id: String,
    /// 应收金额（必填，**> 0**）。
    pub amount: Option<Decimal>,
    /// 账期天数（默认 0 ⇒ 到期日 = 挂账日）。
    pub credit_days: Option<i32>,
    /// 到期日（`yyyy-MM-dd`，显式给优先于 `credit_days` 推算）。
    pub due_date: Option<String>,
    /// 所属会计期间（`yyyy-MM`，默认挂账日所属期间）。
    pub period_code: Option<String>,
    /// 发票号（可选，开票后回填也能用 PATCH 扩展）。
    pub invoice_no: Option<String>,
    /// 发票日期（`yyyy-MM-dd`）。
    pub invoice_date: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 坏账核销请求。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WriteOffReq {
    /// 核销原因（**必填**，≥ [`REASON_MIN_CHARS`] 字）。
    pub reason: String,
}

/// 手工挂账的回执（前端建完单要能立刻拿到单号 + 到期日 + 期间）。
#[derive(Debug, Clone, Serialize)]
pub struct CreateReceivableRes {
    /// 主键。
    pub id: String,
    /// 应收单号（`AR-yyyyMMdd-000001`）。
    pub ar_no: String,
    /// 客户。
    pub customer_id: String,
    /// 应收金额。
    pub amount: Decimal,
    /// 已收金额（新单 0）。
    pub received_amount: Decimal,
    /// 未收金额（= amount）。
    pub unsettled_amount: Decimal,
    /// 账期天数。
    pub credit_days: i32,
    /// 到期日。
    pub due_date: NaiveDate,
    /// 所属会计期间。
    pub period_code: String,
    /// 状态（恒 `OPEN`）。
    pub status: String,
    /// 状态中文标签。
    pub status_label: String,
}

/// 状态中文标签（未知状态原样返回，不 panic）。
pub fn status_label(status: &str) -> &str {
    match status {
        s if s == STATUS_OPEN => "未收",
        s if s == STATUS_PARTIAL => "部分已收",
        s if s == STATUS_SETTLED => "已收清",
        s if s == STATUS_WRITTEN_OFF => "坏账核销",
        s if s == STATUS_CANCELLED => "已作废",
        other => other,
    }
}

// ==================== 业务编排 ====================

/// 手工挂账（`POST /business/erp/receivable`）。
///
/// 校验顺序刻意「先纯参数、再库」：参数错不该先开事务。
pub async fn create_manual(
    operator: Option<Uuid>,
    req: CreateReceivableReq,
) -> std::result::Result<CreateReceivableRes, ApiError> {
    let customer_id = parse_uuid(&req.customer_id, "customer_id")?;
    let amount = validate_amount(req.amount.ok_or_else(|| {
        bad_request("amount 必填（biz_receivable.amount 没有默认值，必须显式给出）")
    })?)?;
    let credit_days = validate_credit_days(req.credit_days.unwrap_or(0))?;
    let due_date_explicit = match req.due_date.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => Some(parse_date_arg(text, "due_date")?),
        _ => None,
    };
    let period_explicit = match req.period_code.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => Some(validate_period(text)?),
        _ => None,
    };
    let invoice_date = match req.invoice_date.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => Some(parse_date_arg(text, "invoice_date")?),
        _ => None,
    };

    let now = Utc::now();
    let posting_date = voucher::today_cst(now);
    let due_date = due_date_explicit.unwrap_or_else(|| due_date_of(posting_date, credit_days));
    // 说明：`due_date < posting_date`（提前到期）**不拦** —— 预收冲抵、历史欠款补录都会出现，
    // 库层也只要求 `due_date` 是合法日期；账龄会把它算成「逾期」，那正是要看到的效果。

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启应收挂账事务"))?;
    let result = create_manual_in_tx(
        &tx,
        operator,
        customer_id,
        amount,
        credit_days,
        due_date,
        period_explicit,
        req,
        invoice_date,
        now,
    )
    .await;
    match result {
        Ok(res) => match tx.commit().await {
            Ok(()) => Ok(res),
            Err(error) => Err(utils::db_error(error, "提交应收挂账事务")),
        },
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 事务体内的挂账（`create_manual` 与真库单测共用，避免单测绕过校验顺序）。
#[allow(clippy::too_many_arguments)]
async fn create_manual_in_tx(
    tx: &Transaction,
    operator: Option<Uuid>,
    customer_id: Uuid,
    amount: Decimal,
    credit_days: i32,
    due_date: NaiveDate,
    period_explicit: Option<String>,
    req: CreateReceivableReq,
    invoice_date: Option<NaiveDate>,
    now: DateTime<Utc>,
) -> std::result::Result<CreateReceivableRes, ApiError> {
    let period_code = match period_explicit {
        Some(period) => period,
        None => voucher::period_of(tx, voucher::today_cst(now))
            .await
            .map_err(|error| utils::db_error(error, "解析挂账日所属会计期间"))?,
    };
    // 已封账的月份只读（ARCH-7 §8：结账后禁止新增应收）
    period::ensure_period_writable(tx, &period_code, "新增应收").await?;
    // 客户必须存在且未冻结（FROZEN = 禁开新单，历史单据不受影响）
    let customer_status: Option<String> = tx
        .query_first_sql(
            format!(
                "SELECT status FROM {CUSTOMER_TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![Value::from(&customer_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询客户"))?
        .as_ref()
        .map(|row| row.get_typed::<String>("status"))
        .transpose()
        .map_err(|error| utils::db_error(error, "读取客户状态"))?;
    match customer_status.as_deref() {
        None => {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!("客户不存在或已删除：{customer_id}"),
            ));
        }
        Some(status) if status != "ACTIVE" => {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!("客户 {customer_id} 当前状态是「{status}」，冻结客户不能新挂应收"),
            ));
        }
        Some(_) => {}
    }

    let source_id = Uuid::now_v7();
    let (id, ar_no) = insert(
        tx,
        &NewReceivable {
            customer_id,
            source_type: SOURCE_MANUAL,
            source_id,
            source_no: SOURCE_NO_MANUAL,
            amount,
            received_amount: Decimal::ZERO,
            credit_days,
            due_date,
            period_code: &period_code,
            invoice_no: req.invoice_no.as_deref(),
            invoice_date,
            remark: req.remark.as_deref(),
            voucher_id: None,
        },
        operator,
        now,
    )
    .await
    .map_err(|error| utils::db_error(error, "写应收单"))?;

    Ok(CreateReceivableRes {
        id: id.to_string(),
        ar_no,
        customer_id: customer_id.to_string(),
        amount,
        received_amount: Decimal::ZERO,
        unsettled_amount: amount,
        credit_days,
        due_date,
        period_code,
        status: STATUS_OPEN.to_string(),
        status_label: status_label(STATUS_OPEN).to_string(),
    })
}

/// 坏账核销的 HTTP 编排：开事务 → 落库 → 提交。
pub async fn write_off_api(
    operator: Option<Uuid>,
    id: &str,
    req: WriteOffReq,
) -> std::result::Result<ReceivableRow, ApiError> {
    let id = parse_uuid(id, "id")?;
    let reason = validate_reason(&req.reason)?;
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启坏账核销事务"))?;
    match write_off(&tx, id, &reason, operator, now).await {
        Ok(row) => match tx.commit().await {
            Ok(()) => Ok(row),
            Err(error) => Err(utils::db_error(error, "提交坏账核销事务")),
        },
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 路径 / 请求体里的 UUID：非法 → `400 + 66006`（绝不静默当作「没传」）。
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

// ==================== 接口层 ====================

/// 手工挂账。
#[nik::permission(
    path = "/business/erp/receivable",
    method = "POST",
    permission = "business:erp:receivable:create"
)]
#[nik::post("/business/erp/receivable")]
pub async fn receivable_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateReceivableReq>,
) -> ResResult<CreateReceivableRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_manual(operator, req).await?))
}

/// 坏账核销。
#[nik::permission(
    path = "/business/erp/receivable/{id}/write-off",
    method = "PATCH",
    permission = "business:erp:receivable:write-off"
)]
#[nik::patch("/business/erp/receivable/{id}/write-off")]
pub async fn receivable_write_off(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<WriteOffReq>,
) -> ResResult<crate::business::erp::receivable_query::ReceivableDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    let row = write_off_api(operator, &id, req).await?;
    // 回读完整详情（客户名 + 凭证号），前端核销后直接就地刷新卡片
    Ok(Res::ok(
        crate::business::erp::receivable_query::receivable_detail(row.id).await?,
    ))
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

    /// ① 状态域与库层 `ck_biz_ar_status` 逐字一致（payable 的 PARTIAL_PAID / PENDING_CANCEL 仍不许进来）。
    #[test]
    fn status_domain_matches_check_constraint() {
        assert_eq!(
            STATUSES,
            ["OPEN", "PARTIAL", "SETTLED", "WRITTEN_OFF", "CANCELLED"],
            "V0290 后应收域是 5 值（CANCELLED = 出库作废冲销）"
        );
        for wrong in ["PARTIAL_PAID", "PENDING_CANCEL"] {
            assert!(
                !STATUSES.contains(&wrong),
                "`{wrong}` 是 payable 的域值，写进 biz_receivable 会撞 ck_biz_ar_status（23514）"
            );
        }
        assert_eq!(status_label(STATUS_OPEN), "未收");
        assert_eq!(status_label(STATUS_PARTIAL), "部分已收");
        assert_eq!(status_label(STATUS_SETTLED), "已收清");
        assert_eq!(status_label(STATUS_WRITTEN_OFF), "坏账核销");
        assert_eq!(
            status_label(STATUS_CANCELLED),
            "已作废",
            "作废与坏账的标签必须能一眼分开（报表 / 台账都靠它）"
        );
        assert_eq!(
            status_label("WHATEVER"),
            "WHATEVER",
            "未知状态原样返回，不 panic"
        );
    }

    /// ①b 有余额的状态只有 OPEN / PARTIAL：账龄、逾期、收款都按这个判（V0290 后三终态全排除）。
    #[test]
    fn only_open_and_partial_have_balance() {
        assert!(has_balance(STATUS_OPEN));
        assert!(has_balance(STATUS_PARTIAL));
        for terminal in [STATUS_SETTLED, STATUS_WRITTEN_OFF, STATUS_CANCELLED] {
            assert!(!has_balance(terminal), "`{terminal}` 不是有余额状态");
        }
    }

    /// ② 边表与状态机注册表同源，且与 ARCH-7 §4.3 + V0290 的图一致（6 条边）。
    #[test]
    fn legal_edges_match_arch7_state_graph() {
        assert_eq!(LEGAL_EDGES, state_machine::RECEIVABLE_EDGES);
        assert_eq!(LEGAL_EDGES.len(), 6);
        let entity = state_machine::Entity::Receivable;
        assert!(entity.can_transition("OPEN", "PARTIAL"));
        assert!(entity.can_transition("OPEN", "SETTLED"));
        assert!(entity.can_transition("PARTIAL", "SETTLED"));
        assert!(entity.can_transition("OPEN", "WRITTEN_OFF"));
        assert!(entity.can_transition("PARTIAL", "WRITTEN_OFF"));
        assert!(entity.can_transition("OPEN", "CANCELLED"));
        // 终态没有出边；已收清不能走坏账；收过钱的不能走作废
        assert!(!entity.can_transition("SETTLED", "WRITTEN_OFF"));
        assert!(!entity.can_transition("WRITTEN_OFF", "OPEN"));
        assert!(
            !entity.can_transition("PARTIAL", "CANCELLED"),
            "已部分收款的应收没有作废边（先冲收款）"
        );
        for terminal in ["SETTLED", "WRITTEN_OFF", "CANCELLED"] {
            for target in STATUSES {
                assert!(
                    !entity.can_transition(terminal, target),
                    "终态 `{terminal}` 不许有出边（→ {target}）"
                );
            }
        }
        assert!(entity.needs_reason("OPEN", "WRITTEN_OFF"));
        assert!(entity.needs_reason("PARTIAL", "WRITTEN_OFF"));
        assert!(
            entity.needs_reason("OPEN", "CANCELLED"),
            "作废冲销必须留原因（上游出库单的作废理由）"
        );
        assert!(!entity.needs_reason("OPEN", "SETTLED"));
        assert!(entity.is_locked("SETTLED"));
        assert!(entity.is_locked("WRITTEN_OFF"));
        assert!(entity.is_locked("CANCELLED"));
        assert!(!entity.is_locked("PARTIAL"));
    }

    /// ③ 未收金额恒等 + 到期日推算（与库层约束同口径）。
    #[test]
    fn unsettled_and_due_date_follow_db_rules() {
        assert_eq!(
            unsettled_of(dec("2260.0000"), dec("1000.0000")),
            dec("1260.0000")
        );
        assert_eq!(
            unsettled_of(dec("100.0000"), dec("100.0000")),
            Decimal::ZERO
        );
        assert_eq!(
            due_date_of(date("2026-11-05"), 0),
            date("2026-11-05"),
            "账期 0 ⇒ 到期日 = 挂账日"
        );
        assert_eq!(due_date_of(date("2026-11-05"), 30), date("2026-12-05"));
        assert_eq!(
            due_date_of(date("2026-12-20"), 30),
            date("2027-01-19"),
            "跨年也要对"
        );
    }

    /// ④ 金额校验：0 / 负数 / 超精度一律 400。
    #[test]
    fn amount_validation_rejects_zero_and_negative() {
        assert_eq!(validate_amount(dec("1.0000")).unwrap(), dec("1.0000"));
        assert_eq!(
            validate_amount(dec("12.34567")).unwrap(),
            dec("12.3457"),
            "NUMERIC(18,4)：多出的小数位四舍五入，不静默截断"
        );
        for bad in ["0", "-1", "-0.0001"] {
            let err = validate_amount(dec(bad)).unwrap_err();
            assert_eq!(err.status, 400, "amount={bad} 必须 400");
        }
        let err = validate_amount(dec("10000000000000")).unwrap_err();
        assert_eq!(err.status, 400, "超过 NUMERIC(18,4) 整数位上限");
    }

    /// ⑤ 账期校验：负数 400（库层 `ck_biz_ar_credit`）。
    #[test]
    fn credit_days_rejects_negative() {
        assert_eq!(validate_credit_days(0).unwrap(), 0);
        assert_eq!(validate_credit_days(90).unwrap(), 90);
        assert_eq!(validate_credit_days(-1).unwrap_err().status, 400);
    }

    /// ⑥ 核销原因：必填、去空白、长度区间（状态机对 WRITTEN_OFF 边强制 reason）。
    #[test]
    fn write_off_reason_validation() {
        assert_eq!(validate_reason("  客户已注销  ").unwrap(), "客户已注销");
        assert_eq!(validate_reason("").unwrap_err().status, 400);
        assert_eq!(validate_reason("   ").unwrap_err().status, 400);
        assert_eq!(
            validate_reason("坏账").unwrap_err().status,
            400,
            "太短不算依据"
        );
        let long = "核".repeat(REASON_MAX_CHARS + 1);
        assert_eq!(validate_reason(&long).unwrap_err().status, 400);
    }

    /// ⑦ 期间 / 日期参数严格解析（`2026-1-1`、`2026-13` 不认）。
    #[test]
    fn period_and_date_parsing_is_strict() {
        assert_eq!(validate_period("2026-11").unwrap(), "2026-11");
        assert_eq!(validate_period("2026-12").unwrap(), "2026-12");
        for bad in ["2026-13", "2026-00", "2026-1", "26-11", "2026/11", ""] {
            assert!(validate_period(bad).is_err(), "period={bad} 必须 400");
        }
        assert_eq!(
            parse_date_arg("2026-11-05", "due_date").unwrap(),
            date("2026-11-05")
        );
        for bad in ["2026-1-1", "20261105", "2026-02-30"] {
            assert!(
                parse_date_arg(bad, "due_date").is_err(),
                "date={bad} 必须 400"
            );
        }
    }

    /// ⑧ 单号格式：`AR-yyyyMMdd-000001`（6 位补零）。
    #[test]
    fn ar_no_format_is_ar_prefix_with_six_digits() {
        assert_eq!(
            voucher::format_doc_no(NO_PREFIX, "20261106", 1),
            "AR-20261106-000001"
        );
        assert_eq!(RECEIVABLE_SEQ.prefix, "AR");
        assert_eq!(RECEIVABLE_SEQ.seq_type, "AR");
    }

    /// ⑨ 请求默认值：只给客户 + 金额时，账期 0、到期日 = 挂账日、期间 = 挂账月。
    #[test]
    fn defaults_are_applied_without_db() {
        let req = CreateReceivableReq {
            customer_id: "11111111-1111-1111-1111-111111111111".to_string(),
            amount: Some(dec("100.0000")),
            ..Default::default()
        };
        assert_eq!(req.credit_days.unwrap_or(0), 0);
        assert!(req.due_date.is_none(), "不传到期日 ⇒ 由账期推算");
        assert!(req.period_code.is_none(), "不传期间 ⇒ 取挂账日所属期间");
        assert!(parse_uuid(&req.customer_id, "customer_id").is_ok());
        assert_eq!(
            parse_uuid("not-a-uuid", "customer_id").unwrap_err().status,
            400
        );
    }

    // ---------- 真库用例（需 `ERP_DB_URL`，缺省 SKIP） ----------

    fn db_enabled() -> bool {
        std::env::var("ERP_DB_URL").is_ok()
    }

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

    /// 夹具客户（固定 UUID，每次先清后建）。
    const CUSTOMER_A: &str = "a13a0001-0000-4000-8000-000000000001";
    const CUSTOMER_FROZEN: &str = "a13a0001-0000-4000-8000-000000000002";

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        if !db_enabled() {
            return None;
        }
        let cleanup = [
            "DELETE FROM biz_status_log WHERE entity_type = 'RECEIVABLE'",
            "DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-%' AND remark LIKE 'SO13T%'",
            "DELETE FROM biz_customer WHERE code LIKE 'SO13T-%'",
        ];
        for sql in cleanup {
            if let Err(err) = db().execute_with_params(sql, vec![]).await {
                panic!("夹具清理失败：{err}\nSQL: {sql}");
            }
        }
        // uuid 一律内联成字面量：`Value::Text` 绑到 `?::uuid` 在 nik-db 上序列化失败（实测）
        for (id, code, status) in [
            (CUSTOMER_A, "SO13T-A", "ACTIVE"),
            (CUSTOMER_FROZEN, "SO13T-F", "FROZEN"),
        ] {
            let sql = format!(
                "INSERT INTO biz_customer (id, code, name, status) \
                 VALUES ('{id}'::uuid, '{code}', 'SO13T-{code}', '{status}')"
            );
            if let Err(err) = db().execute_with_params(&sql, vec![]).await {
                panic!("夹具客户写入失败：{err}");
            }
        }
        Some(guard)
    }

    fn manual_req(amount: &str, remark: &str) -> CreateReceivableReq {
        CreateReceivableReq {
            customer_id: CUSTOMER_A.to_string(),
            amount: Some(dec(amount)),
            remark: Some(format!("SO13T {remark}")),
            ..Default::default()
        }
    }

    /// ⑩ 真库：手工挂账落 24 列约束（恒等式 / 状态 / MANUAL 来源 / 建单日志）。
    #[tokio::test]
    async fn db_manual_posting_creates_open_row_with_log() {
        let Some(_guard) = setup().await else { return };
        let res = create_manual(None, manual_req("1130.0000", "手工"))
            .await
            .unwrap();
        assert_eq!(res.status, STATUS_OPEN);
        assert_eq!(res.received_amount, Decimal::ZERO);
        assert_eq!(
            res.unsettled_amount,
            dec("1130.0000"),
            "恒等式：未收 = 应收"
        );
        assert_eq!(res.credit_days, 0);
        assert_eq!(res.due_date, voucher::today_cst(Utc::now()));
        // AR- + 8 位日期 + - + 6 位流水 = 18 个字符
        assert!(
            res.ar_no.starts_with("AR-") && res.ar_no.len() == 18,
            "单号格式 AR-yyyyMMdd-000001（18 位），实际 {}",
            res.ar_no
        );

        let row = find_by_id(db(), Uuid::parse_str(&res.id).unwrap())
            .await
            .unwrap()
            .expect("刚建的单必须能读回");
        assert_eq!(row.source_type, SOURCE_MANUAL);
        assert_eq!(row.source_no, SOURCE_NO_MANUAL);
        assert_eq!(row.currency_code, "CNY");
        assert!(row.voucher_id.is_none(), "手工挂账不生成凭证（本卡边界）");
        assert!(row.settled_at.is_none());

        // 建单留痕：NULL → OPEN
        let log = db()
            .query_first_sql(
                "SELECT from_status, to_status, entity_no FROM biz_status_log \
                  WHERE entity_type = 'RECEIVABLE' AND entity_id = ?::uuid"
                    .to_string(),
                vec![Value::from(&row.id)],
            )
            .await
            .unwrap()
            .expect("建单必须写 biz_status_log");
        assert_eq!(
            log.get_typed::<Option<String>>("from_status").unwrap(),
            None
        );
        assert_eq!(log.get_typed::<String>("to_status").unwrap(), STATUS_OPEN);
        assert_eq!(log.get_typed::<String>("entity_no").unwrap(), res.ar_no);
    }

    /// ⑪ 真库：同一天可挂多笔 MANUAL（`uk_biz_ar_source` 对 MANUAL 例外）。
    #[tokio::test]
    async fn db_manual_source_allows_multiple_rows_per_day() {
        let Some(_guard) = setup().await else { return };
        let first = create_manual(None, manual_req("10.0000", "A"))
            .await
            .unwrap();
        let second = create_manual(None, manual_req("20.0000", "B"))
            .await
            .unwrap();
        assert_ne!(first.id, second.id);
        assert_ne!(first.ar_no, second.ar_no, "同一天两张单的流水号必须不同");
        assert!(second.ar_no > first.ar_no, "取号单调递增");
    }

    /// ⑫ 真库：核销 → `WRITTEN_OFF` + `settled_at` + 状态日志（含原因）。
    #[tokio::test]
    async fn db_write_off_sets_terminal_state_and_reason() {
        let Some(_guard) = setup().await else { return };
        let created = create_manual(None, manual_req("500.0000", "核销"))
            .await
            .unwrap();
        let id = Uuid::parse_str(&created.id).unwrap();
        let row = write_off_api(
            None,
            &created.id,
            WriteOffReq {
                reason: "客户已注销，经理批准核销".to_string(),
            },
        )
        .await
        .unwrap();
        assert_eq!(row.status, STATUS_WRITTEN_OFF);
        assert!(row.settled_at.is_some(), "核销必须落 settled_at");
        assert!(
            row.remark.as_deref().unwrap_or("").contains("客户已注销"),
            "核销原因要进 remark"
        );
        assert_eq!(
            row.unsettled_amount,
            dec("500.0000"),
            "坏账不冲金额，只改状态"
        );

        let log = db()
            .query_first_sql(
                "SELECT from_status, to_status, reason FROM biz_status_log \
                  WHERE entity_type = 'RECEIVABLE' AND entity_id = ?::uuid \
                    AND to_status = 'WRITTEN_OFF'"
                    .to_string(),
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .expect("核销必须写状态日志");
        assert_eq!(log.get_typed::<String>("from_status").unwrap(), STATUS_OPEN);
        assert!(
            log.get_typed::<String>("reason")
                .unwrap()
                .contains("客户已注销")
        );

        // 二次核销 → 409（终态）
        let again = write_off_api(
            None,
            &created.id,
            WriteOffReq {
                reason: "再来一次核销".to_string(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(again.status, 409);
    }

    /// ⑬ 真库：`SETTLED` 不能核销（409）；`PARTIAL` 可以（边表两条入边）。
    #[tokio::test]
    async fn db_write_off_respects_state_domain() {
        let Some(_guard) = setup().await else { return };
        let created = create_manual(None, manual_req("300.0000", "终态"))
            .await
            .unwrap();
        let id = Uuid::parse_str(&created.id).unwrap();
        // 直接造一张已收清的单（金额恒等式一起写，否则 23514）
        db().execute_with_params(
            "UPDATE biz_receivable SET received_amount = amount, unsettled_amount = 0, \
                    status = 'SETTLED', settled_at = now() WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
        let err = write_off_api(
            None,
            &created.id,
            WriteOffReq {
                reason: "已收清还想核销".to_string(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 409, "SETTLED 是终态，核销必须 409");

        let partial = create_manual(None, manual_req("400.0000", "部分"))
            .await
            .unwrap();
        let partial_id = Uuid::parse_str(&partial.id).unwrap();
        db().execute_with_params(
            "UPDATE biz_receivable SET received_amount = 100, unsettled_amount = 300, \
                    status = 'PARTIAL' WHERE id = ?::uuid",
            vec![Value::from(&partial_id)],
        )
        .await
        .unwrap();
        let row = write_off_api(
            None,
            &partial.id,
            WriteOffReq {
                reason: "余额确认收不回".to_string(),
            },
        )
        .await
        .unwrap();
        assert_eq!(row.status, STATUS_WRITTEN_OFF);
        assert_eq!(row.received_amount, dec("100.0000"), "已收部分不动");
        assert_eq!(row.unsettled_amount, dec("300.0000"));
    }

    /// ⑭ 真库：冻结客户 409、不存在的客户 400（客户闸只拦新挂账）。
    #[tokio::test]
    async fn db_customer_gate_blocks_frozen_and_missing() {
        let Some(_guard) = setup().await else { return };
        let mut req = manual_req("100.0000", "冻结");
        req.customer_id = CUSTOMER_FROZEN.to_string();
        let frozen = create_manual(None, req).await.unwrap_err();
        assert_eq!(frozen.status, 409);

        let mut req = manual_req("100.0000", "缺失");
        req.customer_id = "a13a0001-0000-4000-8000-00000000dead".to_string();
        let missing = create_manual(None, req).await.unwrap_err();
        assert_eq!(missing.status, 400);
    }

    /// ⑮ 真库：非法 amount / 缺 amount / 非法期间在开会话前就被 400 挡住。
    #[tokio::test]
    async fn db_param_validation_happens_before_any_write() {
        let Some(_guard) = setup().await else { return };
        let before: i64 = db()
            .scalar_sql(
                "SELECT count(*) FROM biz_receivable WHERE remark LIKE 'SO13T%'".to_string(),
                vec![],
            )
            .await
            .unwrap();

        let mut req = manual_req("0.0000", "零元");
        req.amount = Some(Decimal::ZERO);
        assert_eq!(create_manual(None, req).await.unwrap_err().status, 400);

        let mut req = manual_req("10.0000", "无金额");
        req.amount = None;
        assert_eq!(create_manual(None, req).await.unwrap_err().status, 400);

        let mut req = manual_req("10.0000", "坏期间");
        req.period_code = Some("2026-13".to_string());
        assert_eq!(create_manual(None, req).await.unwrap_err().status, 400);

        let after: i64 = db()
            .scalar_sql(
                "SELECT count(*) FROM biz_receivable WHERE remark LIKE 'SO13T%'".to_string(),
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(before, after, "参数错不许落任何行");
    }
}
