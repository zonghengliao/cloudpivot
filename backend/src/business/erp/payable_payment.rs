//! 应付付款登记 + 应付作废（批次 2 · P7）：把「只管读」的应付变成**可写**。
//!
//! # 为什么单开一个模块（而不是塞进 `payable.rs` / `payable_query.rs`）
//!
//! * [`crate::business::erp::payable`] 是 P2-C1 的**写模型**（过账即生成应付、作废联动），
//! * [`crate::business::erp::payable_query`] 是 P2-C2 的**只读查询层**（文件头明写「没有写路径」），
//!
//! 付款 / 作废是「应付台账的第二个写入口」，两者都不合适：塞进 query 层会与它自己的模块文档
//! 互相矛盾，塞进 P2-C1 的 model 会把「过账路径」与「付款路径」搅在一起。
//! 另：P7 与 P5（`transfer_*` / `task_bridge` / `error_code`）、P6（`stockin_model`）并行开工，
//! **新文件不产生冲突**，改共享文件却会（见 skill `cloudpivot-backend-workflow` §2）。
//!
//! # 表结构（开工 `\d biz_payment` / `\d biz_payable` 实测）
//!
//! ```text
//! biz_payment  id, payment_no, payable_id, amount, pay_date, pay_method, voucher_id,
//!              remark, created_at, updated_at, create_by, update_by, deleted_at
//!              CHECK amount > 0
//!              UNIQUE (payment_no) WHERE deleted_at IS NULL
//!              INDEX (payable_id, pay_date)
//! ```
//!
//! 一个应付单可以有**多条**付款记录（部分付款），`biz_payable.paid_amount` 是它们的合计。
//! 恒等式 `unpaid_amount = amount - paid_amount` 由 V0148 的 CHECK 兜底，本模块只负责算对。
//!
//! # 付款语义（卡面 §一 ②）
//!
//! ```text
//! ① 状态必须 OPEN / PARTIAL_PAID（SETTLED / CANCELLED → 409）
//! ② amount > 0（400）
//! ③ amount <= unpaid_amount（400，文案点名「本次 X 超过未付余额 Y」）
//! ④ 事务内 SELECT ... FOR UPDATE 锁应付单（防并发重复付款）
//! 重算：paid += amount；unpaid = amount - paid；status = unpaid == 0 ? SETTLED : PARTIAL_PAID
//! 凭证（同事务，直接 POSTED）：借 2202 应付账款 / 贷 1002 银行存款，各 = 本次付款金额
//! ```
//!
//! # 状态日志：只写**真实变化**（LNK-P08 修的就是这条）
//!
//! 分期付款的第二期是 `PARTIAL_PAID → PARTIAL_PAID`（状态没变），第三期才 `→ SETTLED`。
//! 所以付款路径调的是 [`state_machine::transition_if_changed`] 而不是 `transition`：
//!
//! | 第几期 | 状态变化 | `biz_status_log` | 业务流水 / 凭证 |
//! |---|---|---|---|
//! | 1 | OPEN → PARTIAL_PAID | 1 条 | 各 1 条 |
//! | 2 | PARTIAL_PAID → PARTIAL_PAID | **0 条**（原地流转不是状态变更） | 各 1 条 |
//! | 3 | PARTIAL_PAID → SETTLED | 1 条 | 各 1 条 |
//!
//! `state_machine::transition` 的 `from == to` 409 护栏**不放宽**：它是五个实体共用的
//! 唯一入口，把「原地流转」判成合法会让所有调用点写错都变成静默空日志。
//! 「允许原地不动」由本模块用 `transition_if_changed` 显式声明。
//!
//! # 作废语义（卡面 §一 ②）
//!
//! ```text
//! ① 只有 OPEN 且 paid_amount = 0 才允许（已付款 → 409，文案说明）
//! ② reason 必填（400）
//! ③ status = CANCELLED；若这张应付已生成进项凭证 → 复用 voucher::reverse_posted 红冲
//! ```
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: biz_payment / biz_payable 是应付账款台账（V0148/V0158 建表，无 dept_id /
//! 归属人列），数据范围（SELF / DEPT_*）表达不出来；可见性由
//! business:erp:payable:pay / business:erp:payable:cancel 权限码控制。
//! SCOPE-EXEMPT: biz_seq / biz_account / biz_accounting_period / biz_voucher 是全局字典与
//! 会计台账，没有归属列。
//! ```
//!
//! # 事务边界
//!
//! 写函数吃 `&impl SqlExecutor`，**由 handler 开事务**：付款记录、应付回写、凭证三者必须
//! 同生共死（任何一步失败整笔回滚，不允许出现「钱记了、账没动」）。

use axum::Json;
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::payable::{self, PayableRow};
use crate::business::erp::payable_query::{self, PayableDetailRes};
use crate::business::erp::voucher::{self, EntryLine, ReversalInput, SeqSpec};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

/// 付款记录表。
pub const TABLE: &str = "biz_payment";

/// 付款单号前缀（`PM-yyyyMMdd-000001`）。
pub const NO_PREFIX: &str = "PM";
/// `biz_seq.seq_type`。
pub const SEQ_TYPE: &str = "PM";

/// 付款单取号规则。
pub const PAYMENT_SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: SEQ_TYPE,
    prefix: NO_PREFIX,
    seq_name: "付款单号",
    remark: "付款单号：PM-yyyyMMdd-000001，按天（CST）滚动",
};

/// 付款凭证借方科目（应付账款，冲减负债）。
pub const PAY_DEBIT_CODE: &str = "2202";
/// 付款凭证贷方科目（银行存款）。
pub const PAY_CREDIT_CODE: &str = "1002";

// ==================== 入参 / 出参 ====================

/// 登记付款入参。
#[derive(Debug, Clone, Deserialize)]
pub struct PayPayableReq {
    /// 本次付款金额（> 0，且不超过未付余额）。
    pub amount: Option<Decimal>,
    /// 付款日期（缺省 = 今天，CST）。
    pub pay_date: Option<NaiveDate>,
    /// 付款方式（银行转账 / 支票 / 现金 …，自由文本）。
    pub pay_method: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 作废应付单入参。
#[derive(Debug, Clone, Deserialize)]
pub struct CancelPayableReq {
    /// 作废原因（必填）。
    pub reason: Option<String>,
}

/// 一条付款记录（详情抽屉的时间线用）。
#[derive(Debug, Clone, Serialize)]
pub struct PaymentRes {
    /// 主键。
    pub id: String,
    /// 付款单号（`PM-yyyyMMdd-000001`）。
    pub payment_no: String,
    /// 所属应付单。
    pub payable_id: String,
    /// 本次付款金额。
    pub amount: Decimal,
    /// 付款日期。
    pub pay_date: NaiveDate,
    /// 付款方式。
    pub pay_method: Option<String>,
    /// 付款凭证 id。
    pub voucher_id: Option<String>,
    /// 付款凭证号（凭证已删 / 未生成时 `None`）。
    pub voucher_no: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 登记时间。
    pub created_at: DateTime<Utc>,
}

/// `biz_payment` 列清单。
const COLUMNS: &str = "id, payment_no, payable_id, amount, pay_date, pay_method, voucher_id, \
     remark, created_at";

/// 付款记录行。
#[derive(Debug, Clone)]
pub struct PaymentRow {
    /// 主键。
    pub id: Uuid,
    /// 付款单号。
    pub payment_no: String,
    /// 所属应付单。
    pub payable_id: Uuid,
    /// 本次付款金额。
    pub amount: Decimal,
    /// 付款日期。
    pub pay_date: NaiveDate,
    /// 付款方式。
    pub pay_method: Option<String>,
    /// 付款凭证。
    pub voucher_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 登记时间。
    pub created_at: Option<DateTime<Utc>>,
}

impl PaymentRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            payment_no: row.get_typed("payment_no")?,
            payable_id: row.get_typed("payable_id")?,
            amount: row.get_typed("amount")?,
            pay_date: row.get_typed("pay_date")?,
            pay_method: row.get_typed("pay_method")?,
            voucher_id: row.get_typed("voucher_id")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

// ==================== 错误收口 ====================

/// 400：入参不合法。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409：状态冲突（已结清 / 已作废 / 已付过款）。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 404：应付单不存在。
fn not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "应付单不存在或已删除".to_string(),
    )
}

/// 写路径的数据库错误收口：唯一冲突 → `409 + 66001`、CHECK 违反 → `400`。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "单号重复（唯一约束{}）：取号器与库里的号段不一致，请联系运维排查 biz_seq",
                constraint
                    .as_deref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default()
            ),
        ),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "数据不满足库约束 {}（付款金额 > 0；已付金额不超过应付金额；\
                 unpaid_amount = amount - paid_amount；SETTLED 时未付必须为 0）",
                constraint.as_deref().unwrap_or("<未知>")
            ),
        ),
        DbError::InvalidValue(message) => bad_request(message),
        other => utils::db_error(other, ctx),
    }
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

/// 操作人。
fn operator_of(claims: &TokenClaims) -> Option<Uuid> {
    utils::current_user_uuid(claims)
}

// ==================== 读：付款记录 ====================

/// 某应付单的全部付款记录（`pay_date` 升序，同日按登记时间）。
pub async fn payments_of(ex: &impl SqlExecutor, payable_id: Uuid) -> DbResult<Vec<PaymentRow>> {
    // SCOPE-EXEMPT: 付款台账无归属列（见模块文档）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {COLUMNS} FROM {TABLE} \
                  WHERE payable_id = ?::uuid AND deleted_at IS NULL \
                  ORDER BY pay_date ASC, created_at ASC"
            ),
            vec![Value::from(&payable_id)],
        )
        .await?;
    rows.iter().map(PaymentRow::from_row).collect()
}

/// 付款记录 → 出参（带付款凭证号）。
pub async fn payments_res(
    ex: &impl SqlExecutor,
    payable_id: Uuid,
) -> std::result::Result<Vec<PaymentRes>, ApiError> {
    let rows = payments_of(ex, payable_id)
        .await
        .map_err(|error| utils::db_error(error, "查询付款记录"))?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        let voucher_no = match row.voucher_id {
            Some(id) => payable_query::voucher_no_of(id)
                .await
                .map_err(|error| utils::db_error(error, "查询付款凭证号"))?,
            None => None,
        };
        list.push(PaymentRes {
            id: row.id.to_string(),
            payment_no: row.payment_no.clone(),
            payable_id: row.payable_id.to_string(),
            amount: row.amount,
            pay_date: row.pay_date,
            pay_method: row.pay_method.clone(),
            voucher_id: row.voucher_id.map(|id| id.to_string()),
            voucher_no,
            remark: row.remark.clone(),
            created_at: row.created_at.unwrap_or_else(Utc::now),
        });
    }
    Ok(list)
}

/// 锁住一张应付单（`SELECT ... FOR UPDATE`，防并发重复付款）。
///
/// 行锁顺序：付款路径只锁**一张**应付单（本单），不存在多行加锁顺序问题；
/// 与库存 / 调拨路径不共享行，卡面硬红线 8 的「按 id 升序」在这里天然满足。
async fn lock_payable(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<PayableRow>> {
    // SCOPE-EXEMPT: 应付台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {} FROM {} WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE",
                payable::COLUMNS,
                payable::TABLE
            ),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(payable_of_row).transpose()
}

/// 复用 [`payable::PayableRow`] 的字段映射（`from_row` 是私有的，这里按同一列清单重建）。
fn payable_of_row(row: &Row) -> DbResult<PayableRow> {
    Ok(PayableRow {
        id: row.get_typed("id")?,
        payable_no: row.get_typed("payable_no")?,
        supplier_id: row.get_typed("supplier_id")?,
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
        created_at: row.get_typed("created_at")?,
        updated_at: row.get_typed("updated_at")?,
    })
}

// ==================== 写：登记付款 ====================

/// 登记一笔付款（handler 与单测共用）。
///
/// 校验 → 锁 → 重算 → 付款记录 → 付款凭证 → 回写应付，**全部同一个事务**。
pub async fn pay_payable(
    id: Uuid,
    req: &PayPayableReq,
    operator: Option<Uuid>,
) -> std::result::Result<PayableDetailRes, ApiError> {
    let amount = req
        .amount
        .map(|value| value.round_dp(4))
        .ok_or_else(|| bad_request("amount 必填：本次付款金额（> 0，且不超过未付余额）"))?;
    if amount <= Decimal::ZERO {
        return Err(bad_request(format!(
            "本次付款金额必须大于 0，收到 {amount}"
        )));
    }

    let now = Utc::now();
    let pay_date = req.pay_date.unwrap_or_else(|| voucher::today_cst(now));
    let pay_method = trimmed(req.pay_method.as_deref());
    let remark = trimmed(req.remark.as_deref());

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启付款事务"))?;
    let input = PayInput {
        id,
        amount,
        pay_date,
        pay_method,
        remark,
        operator,
        now,
    };
    let result = pay_in_tx(&tx, input).await;
    match result {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交付款事务"))?;
            payable_query::payable_detail(id).await
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 付款事务的全部入参（结构体化，避开 `clippy::too_many_arguments`）。
#[derive(Debug, Clone, Copy)]
struct PayInput<'a> {
    /// 应付单 id。
    id: Uuid,
    /// 本次付款金额。
    amount: Decimal,
    /// 付款日期。
    pay_date: NaiveDate,
    /// 付款方式。
    pay_method: Option<&'a str>,
    /// 备注。
    remark: Option<&'a str>,
    /// 操作人。
    operator: Option<Uuid>,
    /// 业务时间（一次事务内同一个值，便于请求级追溯）。
    now: DateTime<Utc>,
}

/// 付款的事务体（便于单测直接复算）。
async fn pay_in_tx(tx: &Transaction, input: PayInput<'_>) -> std::result::Result<(), ApiError> {
    let PayInput {
        id,
        amount,
        pay_date,
        pay_method,
        remark,
        operator,
        now,
    } = input;
    let row = lock_payable(tx, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定应付单"))?
        .ok_or_else(not_found)?;

    if !matches!(
        row.status.as_str(),
        payable::STATUS_OPEN | payable::STATUS_PARTIAL_PAID
    ) {
        return Err(conflict(format!(
            "应付单 {} 当前是「{}」，只有未付 / 部分已付的应付单能登记付款：\
             已结清（SETTLED）或已作废（CANCELLED）的单据不能再付款",
            row.payable_no,
            payable_query::status_label(&row.status)
        )));
    }

    let unpaid = row.unpaid_amount.round_dp(4);
    if amount > unpaid {
        return Err(bad_request(format!(
            "本次付款 {amount} 超过未付余额 {unpaid}（应付单 {}，应付 {} / 已付 {}）：\
             请按未付余额或更小的金额登记",
            row.payable_no, row.amount, row.paid_amount
        )));
    }

    let paid = (row.paid_amount + amount).round_dp(4);
    let unpaid_after = payable::unpaid_of(row.amount, paid);
    if unpaid_after < Decimal::ZERO {
        return Err(bad_request(format!(
            "本次付款 {amount} 会导致已付金额 {paid} 超过应付金额 {}（应付单 {}）",
            row.amount, row.payable_no
        )));
    }
    let settled = unpaid_after == Decimal::ZERO;
    let status = if settled {
        payable::STATUS_SETTLED
    } else {
        payable::STATUS_PARTIAL_PAID
    };

    // ① 付款记录（先落记录，凭证要指向它）
    let payment_no = voucher::next_doc_no(tx, &PAYMENT_SEQ, now)
        .await
        .map_err(|error| utils::db_error(error, "取付款单号"))?;
    // SCOPE-EXEMPT: 付款台账无归属列（见模块文档）。
    let inserted = tx
        .query_first_sql(
            format!(
                "INSERT INTO {TABLE} \
                    (payment_no, payable_id, amount, pay_date, pay_method, remark, \
                     created_at, updated_at, create_by, update_by) \
                 VALUES (?, ?::uuid, NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::date, \
                         NULLIF(?::text,''), NULLIF(?::text,''), ?, ?, \
                         NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
                 RETURNING id"
            ),
            vec![
                Value::Text(payment_no.clone()),
                Value::from(&row.id),
                Value::Text(amount.to_string()),
                Value::Text(pay_date.to_string()),
                Value::Text(pay_method.unwrap_or_default().to_string()),
                Value::Text(remark.unwrap_or_default().to_string()),
                Value::from(now),
                Value::from(now),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            ],
        )
        .await
        .map_err(|error| write_error(error, "登记付款记录"))?;
    let payment_id: Uuid = match inserted.as_ref() {
        Some(row) => row
            .get_typed("id")
            .map_err(|error| utils::db_error(error, "读取付款记录 id"))?,
        None => {
            return Err(ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "付款记录写入失败：INSERT 没有返回 id".to_string(),
            ));
        }
    };

    // ② 付款凭证：借 2202 应付账款 / 贷 1002 银行存款（同事务，直接 POSTED）
    let payable_account =
        voucher::account_required(tx, PAY_DEBIT_CODE, "付款凭证的借方科目 2202 应付账款")
            .await
            .map_err(|error| write_error(error, "解析付款凭证借方科目"))?;
    let bank_account =
        voucher::account_required(tx, PAY_CREDIT_CODE, "付款凭证的贷方科目 1002 银行存款")
            .await
            .map_err(|error| write_error(error, "解析付款凭证贷方科目"))?;
    let summary = format!("付款 {} 应付单 {}", payment_no, row.payable_no);
    let lines = vec![
        EntryLine {
            line_no: 1,
            summary: format!("{summary} 冲减应付"),
            account: payable_account,
            debit: amount,
            credit: Decimal::ZERO,
            aux_supplier_id: Some(row.supplier_id),
            aux_material_id: None,
            aux_warehouse_id: None,
        },
        EntryLine {
            line_no: 2,
            summary: format!("{summary} 银行付款"),
            account: bank_account,
            debit: Decimal::ZERO,
            credit: amount,
            aux_supplier_id: Some(row.supplier_id),
            aux_material_id: None,
            aux_warehouse_id: None,
        },
    ];
    let voucher_id = voucher::post_voucher(
        tx,
        &voucher::PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_AUTO,
            source_type: voucher::SOURCE_PAYMENT,
            source_id: Some(payment_id),
            source_no: Some(&payment_no),
            voucher_date: pay_date,
            summary,
            lines,
            remark,
            red_of: None,
        },
        operator,
        now,
    )
    .await
    .map_err(|error| write_error(error, "生成付款凭证"))?;

    // ③ 付款记录回写凭证
    // SCOPE-EXEMPT: 付款台账无归属列（见模块文档）。
    tx.exec_sql(
        format!("UPDATE {TABLE} SET voucher_id = ?::uuid, updated_at = ? WHERE id = ?::uuid"),
        vec![
            Value::from(&voucher_id),
            Value::from(now),
            Value::from(&payment_id),
        ],
    )
    .await
    .map_err(|error| utils::db_error(error, "付款记录回写凭证"))?;

    // ④ 应付单重算（行锁在手，这里再按 paid_amount 做一次 CAS，兜住「外部绕过锁改库」）
    //    P10 铁律 ③：OPEN/PARTIAL_PAID → PARTIAL_PAID/SETTLED 也走状态机唯一入口。
    //    **状态没变就不写状态日志**（LNK-P08）：第二次部分付款是
    //    `PARTIAL_PAID → PARTIAL_PAID`，业务照做（付款记录 + 凭证 + 金额回写），
    //    但「原地流转」不是状态变更 —— 所以调 `transition_if_changed`，
    //    它在 `from == to` 时返回 `Ok(None)`、不写 `biz_status_log`。
    //    `transition` 的自环 409 护栏保持不动（那是五个实体共用的唯一入口）。
    //    返回的 `Option` 这里不用：轨迹以 biz_status_log 为准，日志条数由状态真实变化决定
    //    （OPEN→PARTIAL_PAID、PARTIAL_PAID→SETTLED 各一条）。
    let _status_log = state_machine::transition_if_changed(
        tx,
        state_machine::ENTITY_PAYABLE,
        row.id,
        &row.payable_no,
        &state_machine::PAYABLE_EDGES,
        &row.status,
        status,
        operator,
        Some(&format!("付款 {payment_no}")),
        None,
    )
    .await
    .map_err(|error| {
        write_error(
            DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)),
            "应付单状态变更",
        )
    })?;
    // SCOPE-EXEMPT: 应付台账无归属列（见模块文档）。
    let settled_at_text = if settled {
        now.to_rfc3339()
    } else {
        String::new()
    };
    let affected = tx
        .exec_sql(
            format!(
                "UPDATE {} \
                    SET paid_amount = NULLIF(?::text,'')::numeric, \
                        unpaid_amount = NULLIF(?::text,'')::numeric, \
                        status = ?, \
                        settled_at = CASE WHEN ?::text = '' THEN NULL ELSE ?::text::timestamptz END, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND deleted_at IS NULL \
                    AND paid_amount = NULLIF(?::text,'')::numeric",
                payable::TABLE
            ),
            vec![
                Value::Text(paid.to_string()),
                Value::Text(unpaid_after.to_string()),
                Value::Text(status.to_string()),
                Value::Text(settled_at_text.clone()),
                Value::Text(settled_at_text),
                Value::from(now),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(&row.id),
                Value::Text(row.paid_amount.to_string()),
            ],
        )
        .await
        .map_err(|error| write_error(error, "回写应付单"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "应付单 {} 的已付金额在本次付款期间被其它操作改动（并发付款），本次登记已回滚，请重试",
            row.payable_no
        )));
    }

    Ok(())
}

// ==================== 写：作废应付单 ====================

/// 作废一张应付单（handler 与单测共用）。
pub async fn cancel_payable(
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
) -> std::result::Result<PayableDetailRes, ApiError> {
    let reason = reason.map(str::trim).filter(|text| !text.is_empty());
    let Some(reason) = reason else {
        return Err(bad_request(
            "reason 必填：请写明作废原因（会计上作废必须留痕）",
        ));
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启应付作废事务"))?;
    let result = cancel_in_tx(&tx, id, reason, operator, now).await;
    match result {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交应付作废事务"))?;
            payable_query::payable_detail(id).await
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 作废的事务体。
async fn cancel_in_tx(
    tx: &Transaction,
    id: Uuid,
    reason: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    let row = lock_payable(tx, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定应付单"))?
        .ok_or_else(not_found)?;

    if row.status == payable::STATUS_CANCELLED {
        return Err(conflict(format!(
            "应付单 {} 已经作废过了，不能重复作废",
            row.payable_no
        )));
    }
    if row.paid_amount > Decimal::ZERO || row.status != payable::STATUS_OPEN {
        return Err(conflict(format!(
            "应付单 {} 当前是「{}」、已付 {}，不能作废：只有「未付且一分钱都没付」（OPEN + 已付 0）的\
             应付单能作废 —— 已经付过款的请走付款冲销 / 红冲凭证，不要作废单据",
            row.payable_no,
            payable_query::status_label(&row.status),
            row.paid_amount
        )));
    }

    // 进项凭证红冲（复用 voucher::reverse_posted；已红冲过的跳过，保证作废幂等）
    if let Some(voucher_id) = row.voucher_id
        && let Some(original) = voucher::find_voucher(tx, voucher_id)
            .await
            .map_err(|error| utils::db_error(error, "查询应付单的进项凭证"))?
        && original.reversed_by.is_none()
    {
        voucher::reverse_posted(
            tx,
            &original,
            ReversalInput {
                reason: Some(reason),
                voucher_date: voucher::today_cst(now),
            },
            operator,
            now,
        )
        .await
        .map_err(|error| write_error(error, "红冲应付单的进项凭证"))?;
    }

    // 作废：状态 + 原因写进 remark（表里没有独立的原因列）
    // SCOPE-EXEMPT: 应付台账无归属列（见模块文档）。
    let note = match row
        .remark
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(old) => format!("{old}\n作废原因：{reason}"),
        None => format!("作废原因：{reason}"),
    };
    // P10 铁律 ③：OPEN → CANCELLED 走状态机唯一入口（reason 必填由注册表把关）
    state_machine::transition(
        tx,
        state_machine::ENTITY_PAYABLE,
        row.id,
        &row.payable_no,
        &state_machine::PAYABLE_EDGES,
        &row.status,
        payable::STATUS_CANCELLED,
        operator,
        Some(reason),
        None,
    )
    .await
    .map_err(|error| {
        write_error(
            DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)),
            "应付单作废状态变更",
        )
    })?;
    let affected = tx
        .exec_sql(
            format!(
                "UPDATE {} \
                    SET status = ?, remark = ?, updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND deleted_at IS NULL AND status = '{}'",
                payable::TABLE,
                payable::STATUS_OPEN
            ),
            vec![
                Value::Text(payable::STATUS_CANCELLED.to_string()),
                Value::Text(note),
                Value::from(now),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(&row.id),
            ],
        )
        .await
        .map_err(|error| write_error(error, "作废应付单"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "应付单 {} 的状态在本次作废期间被其它操作改动，请刷新后重试",
            row.payable_no
        )));
    }

    Ok(())
}

/// 去掉首尾空白；空串归一成 `None`（写库走 `NULLIF(?::text,'')`）。
fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

// ==================== 接口层 ====================

/// 登记一笔付款（可多次，部分付款累计）。
#[nik::permission(
    path = "/business/erp/payable/{id}/pay",
    method = "POST",
    permission = "business:erp:payable:pay"
)]
#[nik::post("/business/erp/payable/{id}/pay")]
pub async fn payable_pay_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<PayPayableReq>,
) -> ResResult<PayableDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(pay_payable(id, &req, operator_of(&claims)).await?))
}

/// 作废一张应付单（仅 OPEN 且未付款）。
#[nik::permission(
    path = "/business/erp/payable/{id}/cancel",
    method = "POST",
    permission = "business:erp:payable:cancel"
)]
#[nik::post("/business/erp/payable/{id}/cancel")]
pub async fn payable_cancel_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CancelPayableReq>,
) -> ResResult<PayableDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(
        cancel_payable(id, req.reason.as_deref(), operator_of(&claims)).await?,
    ))
}

// ==================== 单测 ====================
//
// 真库用例（需要 `ERP_DB_URL` 指向「已应用 V0148 + V0158 的隔离库」，缺省则全部 SKIP）：
//
// ```bash
// export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_p7'
// cargo test --lib p7_
// ```
//
// 夹具全部带 `P7` 前缀（应付单号 `AP-P7-…`、付款单号由取号器产生 `PM-…`），
// 每次 `setup()` 先按前缀清掉上一轮，重复跑不积累。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::voucher::{self, entries_of, find_voucher};
    use nik::prelude::chrono::Duration;
    use std::str::FromStr;

    /// 夹具供应商。
    const SUPPLIER: &str = "e7e70001-0000-4000-8000-000000000001";
    /// 真库用例必须串行（共用全局 `db()` 与同一批 `biz_seq`）。
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

    async fn exec(sql: &str) {
        if let Err(err) = db().execute_with_params(sql, vec![]).await {
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试金额字面量非法")
    }

    /// 拿锁 + 连库 + 清上一轮 + 建夹具。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        let cleanups = [
            // 凭证 / 分录：**只删本模块自己造的行**
            //   ① 付款凭证：`source_id` 指向本夹具应付单的付款记录（不能按 `source_no LIKE 'PM-%'`
            //      全局删 —— 那会把同一个库里真实业务的付款凭证一起删掉，测试库尤其危险）
            //   ② 作废时红冲出来的那张进项凭证：`source_no = 'P7PAY-…'`
            // ⚠️ `voucher_write` 的单测平行跑，它也写 biz_voucher（手工凭证），用各自的夹具互不干扰。
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE source_no LIKE 'P7PAY-%' \
                OR source_id IN (SELECT id FROM biz_payment \
                                  WHERE payable_id IN (SELECT id FROM biz_payable \
                                                        WHERE payable_no LIKE 'AP-P7-%')))"
                .to_string(),
            "DELETE FROM biz_voucher WHERE source_no LIKE 'P7PAY-%' \
                OR source_id IN (SELECT id FROM biz_payment \
                                  WHERE payable_id IN (SELECT id FROM biz_payable \
                                                        WHERE payable_no LIKE 'AP-P7-%'))"
                .to_string(),
            // 付款记录 → 应付单（外键顺序：先子后父）
            "DELETE FROM biz_payment WHERE payable_id IN \
             (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-P7-%')"
                .to_string(),
            "DELETE FROM biz_payable WHERE payable_no LIKE 'AP-P7-%'".to_string(),
        ];
        for sql in cleanups {
            exec(&sql).await;
        }
        exec(&format!(
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status, credit_days) \
             VALUES ('{SUPPLIER}'::uuid, 'P7-SUP', 'P7 付款测试供应商', 1, 30) \
             ON CONFLICT DO NOTHING"
        ))
        .await;
        // 期间必须存在且 OPEN（手工 / 付款凭证都记到 2026-10）
        exec(
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
                                                start_date, end_date, status) \
             VALUES (2026, 10, '2026-10', DATE '2026-10-01', DATE '2026-10-31', 'OPEN') \
             ON CONFLICT (period_code) DO UPDATE SET status = 'OPEN'",
        )
        .await;
        // 付款凭证的两个科目必须是启用状态（V0158 补种 1002；这里兜住「上一轮测试禁用过」）
        exec("UPDATE biz_account SET status = 1 WHERE account_code IN ('2202', '1002')").await;
        Some(guard)
    }

    /// 造一张应付单（直接落库，绕过收货流程；`paid` 决定状态）。
    async fn new_payable(amount: Decimal, paid: Decimal, status: &str) -> Uuid {
        // ⚠️ 取 uuid 的**尾部** 8 位：v7 的高位是毫秒时间戳，同一毫秒内造两次会撞号
        let simple = Uuid::now_v7().simple().to_string();
        let no = format!("AP-P7-{}", &simple[simple.len() - 8..]);
        let unpaid = (amount - paid).round_dp(4);
        let due = voucher::today_cst(Utc::now()) + Duration::days(30);
        let row = db()
            .query_first_sql(
                "INSERT INTO biz_payable (payable_no, supplier_id, source_type, source_id, source_no, \
                                          currency_code, amount, paid_amount, unpaid_amount, credit_days, \
                                          due_date, period_code, status) \
                 VALUES (?, ?::uuid, 'STOCK_IN', gen_random_uuid(), ?, 'CNY', \
                         NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                         NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,'')::date, '2026-10', ?) \
                 RETURNING id"
                .to_string(),
                vec![
                    Value::Text(no),
                    Value::from(&Uuid::parse_str(SUPPLIER).unwrap()),
                    Value::Text("P7-IN".to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(paid.to_string()),
                    Value::Text(unpaid.to_string()),
                    Value::Text(due.to_string()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造应付单失败");
        row.expect("INSERT 没有返回 id")
            .get_typed("id")
            .expect("读取应付单 id 失败")
    }

    fn pay_req(amount: &str) -> PayPayableReq {
        PayPayableReq {
            amount: Some(dec(amount)),
            pay_date: None,
            pay_method: Some("银行转账".to_string()),
            remark: Some("P7 单测".to_string()),
        }
    }

    async fn row_of(id: Uuid) -> payable_query::PayableRow {
        payable_query::find_payable(id)
            .await
            .unwrap()
            .expect("应付单不存在")
    }

    // ---------- ① 全额付款 ----------

    /// ① 全额付款 → `SETTLED`、`unpaid = 0`、`settled_at` 有值、付款记录 1 条。
    #[tokio::test]
    async fn p7_pay_full_amount_settles_payable() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_payable(dec("226"), Decimal::ZERO, payable::STATUS_OPEN).await;
        let detail = pay_payable(id, &pay_req("226"), None)
            .await
            .unwrap_or_else(|err| panic!("全额付款失败：{} {}", err.code, err.msg));

        assert_eq!(detail.payable.status, payable::STATUS_SETTLED);
        assert_eq!(detail.payable.paid_amount, dec("226"));
        assert_eq!(detail.payable.unpaid_amount, Decimal::ZERO);
        let row = row_of(id).await;
        assert!(row.settled_at.is_some(), "结清必须写 settled_at");
        assert_eq!(detail.payments.len(), 1);
        assert_eq!(detail.payments[0].amount, dec("226"));
        assert!(
            detail.payments[0].payment_no.starts_with("PM-"),
            "{}",
            detail.payments[0].payment_no
        );
    }

    // ---------- ② 部分付款 ----------

    /// ② 部分付款 → `PARTIAL_PAID`，未付余额 = 应付 - 本次。
    #[tokio::test]
    async fn p7_partial_payment_marks_partial_and_keeps_unpaid() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_payable(dec("226"), Decimal::ZERO, payable::STATUS_OPEN).await;
        let detail = pay_payable(id, &pay_req("100"), None).await.unwrap();
        assert_eq!(detail.payable.status, payable::STATUS_PARTIAL_PAID);
        assert_eq!(detail.payable.paid_amount, dec("100"));
        assert_eq!(detail.payable.unpaid_amount, dec("126"));
        assert_eq!(row_of(id).await.settled_at, None, "未结清不能写 settled_at");
    }

    // ---------- ③ 两次部分付款累加 ----------

    /// ③ 两次部分付款：第一次 `PARTIAL_PAID`，第二次付完转 `SETTLED`，付款记录累计 2 条。
    #[tokio::test]
    async fn p7_two_partial_payments_accumulate_and_settle() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_payable(dec("226"), Decimal::ZERO, payable::STATUS_OPEN).await;
        pay_payable(id, &pay_req("100"), None).await.unwrap();
        let detail = pay_payable(id, &pay_req("126"), None).await.unwrap();
        assert_eq!(detail.payable.status, payable::STATUS_SETTLED);
        assert_eq!(detail.payable.paid_amount, dec("226"));
        assert_eq!(detail.payable.unpaid_amount, Decimal::ZERO);
        assert_eq!(detail.payments.len(), 2, "两次付款必须各留一条记录");

        let sum: Decimal = payments_of(db(), id)
            .await
            .unwrap()
            .iter()
            .map(|row| row.amount)
            .sum();
        assert_eq!(sum, dec("226"), "付款记录合计必须等于已付金额");
    }

    // ---------- ④ 超过未付余额 ----------

    /// ④ 付款超过未付余额 → 400，文案点名「本次 X 超过未付余额 Y」。
    #[tokio::test]
    async fn p7_payment_over_unpaid_is_rejected_with_amounts() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_payable(dec("226"), Decimal::ZERO, payable::STATUS_OPEN).await;
        pay_payable(id, &pay_req("100"), None).await.unwrap();

        let err = pay_payable(id, &pay_req("200"), None)
            .await
            .expect_err("超过未付余额必须被拒");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(
            err.msg.contains("本次付款 200") && err.msg.contains("未付余额 126"),
            "文案要点名两个金额：{}",
            err.msg
        );
        // 失败必须整笔回滚：状态与余额都不动，也不留付款记录
        let row = row_of(id).await;
        assert_eq!(row.paid_amount, dec("100"));
        assert_eq!(row.unpaid_amount, dec("126"));
        assert_eq!(row.status, payable::STATUS_PARTIAL_PAID);
        assert_eq!(payments_of(db(), id).await.unwrap().len(), 1);
    }

    // ---------- ⑤ 0 / 负数 ----------

    /// ⑤ 付款 0 或负数 → 400。
    #[tokio::test]
    async fn p7_zero_or_negative_payment_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_payable(dec("226"), Decimal::ZERO, payable::STATUS_OPEN).await;
        for amount in ["0", "-1", "-0.01"] {
            let err = pay_payable(id, &pay_req(amount), None)
                .await
                .expect_err("0 / 负数付款必须被拒");
            assert_eq!(err.status, 400, "{amount} → {}", err.msg);
            assert!(err.msg.contains("必须大于 0"), "{amount} → {}", err.msg);
        }
        assert_eq!(row_of(id).await.paid_amount, Decimal::ZERO);
        assert!(payments_of(db(), id).await.unwrap().is_empty());
    }

    // ---------- ⑥ 已结清 / 已作废再付款 ----------

    /// ⑥ 对 `SETTLED` / `CANCELLED` 的应付单再付款 → 409。
    #[tokio::test]
    async fn p7_pay_settled_or_cancelled_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let settled = new_payable(dec("100"), dec("100"), payable::STATUS_SETTLED).await;
        let err = pay_payable(settled, &pay_req("1"), None)
            .await
            .expect_err("已结清不能再付款");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("已结清"), "{}", err.msg);

        let cancelled = new_payable(dec("100"), Decimal::ZERO, payable::STATUS_CANCELLED).await;
        let err = pay_payable(cancelled, &pay_req("1"), None)
            .await
            .expect_err("已作废不能再付款");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("已作废"), "{}", err.msg);
    }

    // ---------- ⑦ 已付款的单作废 ----------

    /// ⑦ 已付过款的应付单作废 → 409（文案说明只能冲销不能作废）。
    #[tokio::test]
    async fn p7_cancel_paid_payable_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_payable(dec("226"), Decimal::ZERO, payable::STATUS_OPEN).await;
        pay_payable(id, &pay_req("100"), None).await.unwrap();

        let err = cancel_payable(id, Some("重复入库"), None)
            .await
            .expect_err("已付款的应付单不能作废");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("已付 100") && err.msg.contains("不能作废"),
            "{}",
            err.msg
        );
        assert_eq!(row_of(id).await.status, payable::STATUS_PARTIAL_PAID);
    }

    // ---------- ⑧ 付款凭证 ----------

    /// ⑧ 付款生成凭证：借 2202 = 贷 1002 = 付款金额，借贷平衡。
    #[tokio::test]
    async fn p7_payment_generates_balanced_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_payable(dec("226"), Decimal::ZERO, payable::STATUS_OPEN).await;
        let detail = pay_payable(id, &pay_req("126.5"), None).await.unwrap();

        let payment = &detail.payments[0];
        let voucher_id = Uuid::parse_str(payment.voucher_id.as_deref().expect("付款必须生成凭证"))
            .expect("凭证 id 非法");
        let voucher = find_voucher(db(), voucher_id)
            .await
            .unwrap()
            .expect("付款凭证不存在");
        assert_eq!(voucher.status, voucher::STATUS_POSTED, "付款凭证直接已记账");
        assert_eq!(voucher.voucher_type, voucher::VOUCHER_TYPE_AUTO);
        assert_eq!(voucher.source_type, voucher::SOURCE_PAYMENT);
        assert_eq!(voucher.is_red, voucher::IS_RED_NO);
        assert_eq!(voucher.total_debit, dec("126.5"));
        assert_eq!(voucher.total_credit, dec("126.5"));

        let entries = entries_of(db(), voucher_id).await.unwrap();
        assert_eq!(entries.len(), 2);
        let debit = entries
            .iter()
            .find(|row| row.account.code == PAY_DEBIT_CODE)
            .expect("缺借方 2202 应付账款");
        assert_eq!(debit.debit, dec("126.5"));
        assert_eq!(debit.credit, Decimal::ZERO);
        let credit = entries
            .iter()
            .find(|row| row.account.code == PAY_CREDIT_CODE)
            .expect("缺贷方 1002 银行存款");
        assert_eq!(credit.credit, dec("126.5"));
        assert_eq!(credit.debit, Decimal::ZERO);
        assert_eq!(debit.debit, credit.credit, "借贷必须平衡");
        assert_eq!(
            payment.voucher_no.as_deref(),
            Some(voucher.voucher_no.as_str())
        );
    }

    // ---------- ⑨（附加）未付款的应付单作废 → CANCELLED + 红冲进项凭证 ----------

    /// ⑨ 作废 `OPEN` 且未付款的应付单：状态 `CANCELLED`、原因进 remark、
    /// 关联的进项凭证被红冲（红字凭证 `reverse_of` 指向原凭证）。
    #[tokio::test]
    async fn p7_cancel_unpaid_payable_reverses_its_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_payable(dec("226"), Decimal::ZERO, payable::STATUS_OPEN).await;
        // 造一张「进项凭证」并把应付指向它（真实来源是收货过账，单测直接落库等价）
        let inventory_account = voucher::account_required(db(), "1405", "借方")
            .await
            .unwrap();
        let payable_account = voucher::account_required(db(), "2202", "贷方")
            .await
            .unwrap();
        let voucher_id = voucher::post_voucher(
            db(),
            &voucher::PostVoucher {
                voucher_type: voucher::VOUCHER_TYPE_AUTO,
                source_type: "STOCK_IN",
                source_id: None,
                source_no: Some("P7PAY-IN-0001"),
                voucher_date: voucher::today_cst(Utc::now()),
                summary: "P7 采购入库 价税合计 226".to_string(),
                lines: vec![
                    EntryLine {
                        line_no: 1,
                        summary: "P7 存货".to_string(),
                        account: inventory_account,
                        debit: dec("226"),
                        credit: Decimal::ZERO,
                        aux_supplier_id: None,
                        aux_material_id: None,
                        aux_warehouse_id: None,
                    },
                    EntryLine {
                        line_no: 2,
                        summary: "P7 应付".to_string(),
                        account: payable_account,
                        debit: Decimal::ZERO,
                        credit: dec("226"),
                        aux_supplier_id: None,
                        aux_material_id: None,
                        aux_warehouse_id: None,
                    },
                ],
                remark: None,
                red_of: None,
            },
            None,
            Utc::now(),
        )
        .await
        .unwrap();
        db().execute_with_params(
            "UPDATE biz_payable SET voucher_id = ?::uuid WHERE id = ?::uuid",
            vec![Value::from(&voucher_id), Value::from(&id)],
        )
        .await
        .unwrap();

        let detail = cancel_payable(id, Some("供应商取消订单"), None)
            .await
            .unwrap_or_else(|err| panic!("作废失败：{} {}", err.code, err.msg));
        assert_eq!(detail.payable.status, payable::STATUS_CANCELLED);
        assert!(
            detail
                .payable
                .remark
                .as_deref()
                .unwrap_or_default()
                .contains("作废原因：供应商取消订单"),
            "作废原因必须留痕：{:?}",
            detail.payable.remark
        );

        let original = find_voucher(db(), voucher_id).await.unwrap().unwrap();
        assert_eq!(
            original.status,
            voucher::STATUS_POSTED,
            "原凭证状态保持 POSTED，只打 reversed_by"
        );
        let red_id = original.reversed_by.expect("进项凭证必须被红冲");
        let red = find_voucher(db(), red_id).await.unwrap().unwrap();
        assert_eq!(red.is_red, voucher::IS_RED_YES);
        assert_eq!(red.reverse_of, Some(voucher_id));
        assert!(
            red.voucher_no.starts_with(voucher::RED_NO_PREFIX),
            "红字凭证走独立号段：{}",
            red.voucher_no
        );
    }

    /// 作废原因必填 → 400（不写库）。
    #[tokio::test]
    async fn p7_cancel_requires_reason() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_payable(dec("10"), Decimal::ZERO, payable::STATUS_OPEN).await;
        for reason in [None, Some("   ")] {
            let err = cancel_payable(id, reason, None)
                .await
                .expect_err("缺原因必须被拒");
            assert_eq!(err.status, 400, "{}", err.msg);
            assert!(err.msg.contains("reason 必填"), "{}", err.msg);
        }
        assert_eq!(row_of(id).await.status, payable::STATUS_OPEN);
    }
}
