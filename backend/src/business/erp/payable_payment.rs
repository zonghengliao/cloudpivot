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
//! # 可付信息（D5：应付单 → 发起付款）
//!
//! `GET /business/erp/payable/{id}/payment-info` —— 应付单详情抽屉点「发起付款」前的那一次读：
//! 一次给全「这张单能付多少 / 能不能付 / 为什么不能付 / 已付过哪些」，前端**不用自己算**默认金额。
//!
//! | 字段 | 口径（卡面 F2 / O1） |
//! |---|---|
//! | `payable.amount` / `paid_amount` / `unpaid_amount` | 直接来自 `biz_payable`（不重算，不四舍五入到元） |
//! | `payable.default_amount` | **= 未付金额**（不是应付总额）：前端把它填进付款金额输入框 |
//! | `payable.can_pay` / `blocked_reason` | 见 [`payment_capability`] 的判定表（纯函数，单测直接打） |
//! | `payable.pending_amount` / `overpay_risk` | 审批中的那笔 + 「现在看已经超付」的提示（卡面 ③ 反向） |
//! | `payments` | [`payments_res`]（含 `approval_status` / `approval_status_label` / 付款凭证号） |
//!
//! **写路径一行没改**：这个接口只读，真正付款仍走 [`submit_payment`]（P17：有已发布流程就发起审批，
//! 没有才降级直付），校验仍是 [`validate_payment_target`]（`0 < amount ≤ unpaid`，超付 400 点名两个金额）。
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

use crate::business::erp::form_snapshot;
use crate::business::erp::payable::{self, PayableRow};
use crate::business::erp::payable_bridge;
use crate::business::erp::payable_query::{self, PayableDetailRes};
use crate::business::erp::voucher::{self, EntryLine, ReversalInput, SeqSpec};
use crate::business::flow::{engine, model as flow_model};
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

// ---------- 付款审批（P17）----------

/// `approval_status`：未配流程 / 历史直付（P7 老行为）。
pub const APPROVAL_NONE: &str = "NONE";
/// `approval_status`：审批中（**账一分没动**）。
pub const APPROVAL_PENDING: &str = "PENDING";
/// `approval_status`：审批通过且已执行（应付回写 + 付款凭证）。
pub const APPROVAL_APPROVED: &str = "APPROVED";
/// `approval_status`：驳回 / 撤回（账一分不动，可重新提交）。
pub const APPROVAL_REJECTED: &str = "REJECTED";
/// `approval_status`：因单据作废等外部原因终止（账一分不动）。
pub const APPROVAL_CANCELLED: &str = "CANCELLED";

/// 付款审批的业务类型（`biz_flow_def.biz_type`，与 V0175 种子同口径）。
pub const PAY_BIZ_TYPE: &str = "PAYABLE_PAY";
/// 付款审批的流程编码（`biz_flow_def.flow_code`）。
pub const PAY_FLOW_CODE: &str = "payable_pay";

/// 提交结果：仍在审批中（HTTP 202）。
pub const OUTCOME_PENDING: &str = "PENDING_APPROVAL";
/// 提交结果：未配流程，已直接生效（HTTP 200）。
pub const OUTCOME_PAID: &str = "PAID";

/// `approval_status` 的中文标签（前端 tag 文案的唯一来源）。
pub fn approval_status_label(status: &str) -> &str {
    match status {
        APPROVAL_PENDING => "审批中",
        APPROVAL_APPROVED => "已通过",
        APPROVAL_REJECTED => "已驳回",
        APPROVAL_CANCELLED => "已终止",
        APPROVAL_NONE => "已生效",
        other => other,
    }
}

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
    /// 审批状态（`NONE` / `PENDING` / `APPROVED` / `REJECTED` / `CANCELLED`）。
    pub approval_status: String,
    /// 审批状态中文标签（前端 tag 直接用，避免两端各写一份映射）。
    pub approval_status_label: String,
    /// 审批实例 id（有流程时才有；详情抽屉据此显示审批进度）。
    pub approval_instance_id: Option<String>,
    /// 是否**尚未生效**（`PENDING`）：前端把它当「橙色待审」，金额也不计入已付。
    pub pending: bool,
}

/// 提交付款的结果（`POST /payable/{id}/pay` 的出参）。
///
/// * `outcome = PENDING_APPROVAL`（HTTP **202**）：已发起审批，**账还没动**；
/// * `outcome = PAID`（HTTP 200）：没配流程，按 P7 老行为直接生效。
#[derive(Debug, Clone, Serialize)]
pub struct PaySubmitRes {
    /// `PENDING_APPROVAL` / `PAID`。
    pub outcome: String,
    /// 给前端直接展示的一句话。
    pub message: String,
    /// 本次付款金额。
    pub amount: Decimal,
    /// 付款记录 id（审批通过后也用它查进度）。
    pub payment_id: Option<String>,
    /// 付款单号（`PM-yyyyMMdd-000001`）。
    pub payment_no: Option<String>,
    /// 审批实例 id（未配流程时为 `None`）。
    pub approval_instance_id: Option<String>,
    /// 应付单最新详情（含全部付款记录）。
    pub detail: PayableDetailRes,
}

/// `biz_payment` 列清单。
const COLUMNS: &str = "id, payment_no, payable_id, amount, pay_date, pay_method, voucher_id, \
     remark, created_at, approval_status, approval_instance_id";

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
    /// 审批状态。
    pub approval_status: String,
    /// 审批实例。
    pub approval_instance_id: Option<Uuid>,
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
            approval_status: row.get_typed("approval_status")?,
            approval_instance_id: row.get_typed("approval_instance_id")?,
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
            approval_status: row.approval_status.clone(),
            approval_status_label: approval_status_label(&row.approval_status).to_string(),
            approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
            pending: row.approval_status == APPROVAL_PENDING,
        });
    }
    Ok(list)
}

// ==================== 读：可付信息（D5）====================

/// 付款能力判定（**纯函数**，单测直接打；不碰库、不看时间）。
///
/// 判定顺序 —— 先问「这张单还能不能付款」，再问「有没有别的款在审批中」：
///
/// | # | 条件 | `can_pay` | `blocked_reason` |
/// |---|---|---|---|
/// | ① | `status = CANCELLED` | `false` | 已作废 |
/// | ② | `status = PENDING_CANCEL` | `false` | 正在作废审批中（带审批单号），请等审批结束或先撤回 |
/// | ③ | `status = SETTLED` | `false` | 已结清（卡面 L9：结清的单不显示按钮）|
/// | ④ | `unpaid_amount <= 0` | `false` | 未付金额为 0（理论上被 ①②③ 覆盖，兜「状态没跟上金额」的脏数据）|
/// | ⑤ | 有一笔 `PENDING` 付款 | `false` | 该笔在审批中（`submit_payment` 同样 409）|
/// | ⑥ | 其余（`OPEN` / `PARTIAL_PAID`） | `true` | `None` |
///
/// ② 是 P-FLOW-3B 补漏：作废审批中的单**账上还没作废**（`unpaid_amount` 仍是原值），
/// 所以「按金额判可付」的消费方（报表 / 批量 / 将来的移动端）会得到「能付」的错误结论，
/// 真点下去被 `validate_payment_target` 409。闸门放在这里，所有消费方自动一致。
/// 审批单号由调用方查好传进来（`cancel_instance_no`，纯函数不碰库）；
/// 不是 `PENDING_CANCEL` 的单传 `None` 即可。
///
/// ⑤ 是**体验**护栏而不是新的业务规则：后端 `submit_for_approval_tx` 对「已有审批中的付款」
/// 返回 409（`uk_biz_payment_pending` 的软闸），不在这里挡住的话用户填完金额才被拒一次。
pub fn payment_capability(
    row: &payable_query::PayableRow,
    pending: Option<(&str, Decimal)>,
    cancel_instance_no: Option<&str>,
) -> (bool, Option<String>) {
    let unpaid = row.unpaid_amount.round_dp(4);
    if row.status == payable::STATUS_CANCELLED {
        return (
            false,
            Some(format!(
                "应付单 {} 已作废（CANCELLED），不能再付款",
                row.payable_no
            )),
        );
    }
    if row.status == payable::STATUS_PENDING_CANCEL {
        let instance = cancel_instance_no.unwrap_or("审批中的作废单");
        return (
            false,
            Some(format!(
                "应付单 {} 正在作废审批中（{instance}），请等审批结束或先撤回",
                row.payable_no
            )),
        );
    }
    if row.status == payable::STATUS_SETTLED {
        return (
            false,
            Some(format!(
                "应付单 {} 已结清（未付金额 {unpaid}），无需再付款",
                row.payable_no
            )),
        );
    }
    if unpaid <= Decimal::ZERO {
        return (
            false,
            Some(format!(
                "应付单 {} 未付金额为 {unpaid}，没有可付的余额",
                row.payable_no
            )),
        );
    }
    if let Some((payment_no, amount)) = pending {
        return (
            false,
            Some(format!(
                "该应付单已有一笔付款在审批中（{payment_no}，金额 {amount}），\
                 请等审批结果后再发起（驳回或撤回后可重新提交）"
            )),
        );
    }
    (true, None)
}

/// 「审批中那笔现在看已经超付」的提示（卡面 ③ 反向：审批期间应付单被另一笔付完）。
///
/// 只是**提示**，不拦人（真正拦截在 `settle_approved_in_tx`：执行时 `amount > unpaid`
/// 会把付款记录标 `REJECTED` 且账一分不动）。返回 `None` 表示没有这种风险。
pub fn overpay_risk_of(
    row: &payable_query::PayableRow,
    pending: Option<(&str, Decimal)>,
) -> Option<String> {
    let (payment_no, amount) = pending?;
    let unpaid = row.unpaid_amount.round_dp(4);
    let amount = amount.round_dp(4);
    if amount <= unpaid {
        return None;
    }
    Some(format!(
        "审批中的付款 {payment_no}（{amount}）已超过当前未付余额 {unpaid}：\
         应付单 {} 可能已被其它付款结清，审批通过时这笔会被拒绝（账不会动），请复核后再付款",
        row.payable_no
    ))
}

/// 可付信息的单头（`GET /payable/{id}/payment-info` 的 `payable` 节点）。
///
/// 刻意**不**复用 [`payable_query::PayableRes`]：那个结构是列表行契约（含 `voucher_id` /
/// `remark` / 审计时间），多给前端几个用不上的字段不如明确列一版「付款要用的那几个」。
#[derive(Debug, Clone, Serialize)]
pub struct PaymentInfoPayable {
    /// 应付单 id。
    pub id: String,
    /// 应付单号 `AP-yyyyMMdd-000001`。
    pub payable_no: String,
    /// 供应商 id。
    pub supplier_id: String,
    /// 供应商名称（主数据被软删时为空串）。
    pub supplier_name: String,
    /// 来源单据类型（`STOCK_IN` …）。
    pub source_type: String,
    /// 来源单据 id（前端「点来源单号跳收货单」用）。
    pub source_id: String,
    /// 来源单号。
    pub source_no: String,
    /// 币种（当前恒为 `CNY`）。
    pub currency_code: String,
    /// 应付金额（含税）。
    pub amount: Decimal,
    /// 已付金额。
    pub paid_amount: Decimal,
    /// 未付金额（= `amount - paid_amount`，库侧 CHECK 兜底）。
    pub unpaid_amount: Decimal,
    /// 状态（`OPEN` / `PARTIAL_PAID` / `SETTLED` / `CANCELLED`）。
    pub status: String,
    /// 状态中文标签。
    pub status_label: String,
    /// 到期日。
    pub due_date: NaiveDate,
    /// 是否逾期（到期日早于今天 + 仍有未付 + 未作废）。
    pub overdue: bool,
    /// 会计期间 `yyyy-MM`。
    pub period_code: String,
    /// 能不能发起付款（卡面 O1/L9：`true` 才显示按钮）。
    pub can_pay: bool,
    /// 不能付款的原因（`can_pay = true` 时是 `None`）。
    pub blocked_reason: Option<String>,
    /// **默认付款金额 = 未付金额**（卡面 F2：带未付、不是应付总额；前端不要自己算）。
    pub default_amount: Decimal,
    /// 审批中的付款单号（没有则为 `None`）。
    pub pending_payment_no: Option<String>,
    /// 审批中的付款金额（没有则为 `None`）。
    pub pending_amount: Option<Decimal>,
    /// 「审批中那笔现在看已经超付」的提示（见 [`overpay_risk_of`]）。
    pub overpay_risk: Option<String>,
}

impl PaymentInfoPayable {
    /// 由行 + 审批中的那笔构造（`today` / `cancel_instance_no` 由调用方给，纯函数便于单测）。
    ///
    /// `cancel_instance_no`：本单是 `PENDING_CANCEL` 时传审批单号（`FLW-…`），否则 `None`。
    fn from_row(
        row: &payable_query::PayableRow,
        pending: Option<(&str, Decimal)>,
        today: NaiveDate,
        cancel_instance_no: Option<&str>,
    ) -> Self {
        let (can_pay, blocked_reason) = payment_capability(row, pending, cancel_instance_no);
        let unpaid = row.unpaid_amount.round_dp(4);
        Self {
            id: row.id.to_string(),
            payable_no: row.payable_no.clone(),
            supplier_id: row.supplier_id.to_string(),
            supplier_name: row.supplier_name.clone(),
            source_type: row.source_type.clone(),
            source_id: row.source_id.to_string(),
            source_no: row.source_no.clone(),
            currency_code: row.currency_code.clone(),
            amount: row.amount,
            paid_amount: row.paid_amount,
            unpaid_amount: unpaid,
            status: row.status.clone(),
            status_label: payable_query::status_label(&row.status).to_string(),
            due_date: row.due_date,
            overdue: payable_query::is_overdue(row, today),
            period_code: row.period_code.clone(),
            can_pay,
            blocked_reason,
            // ⚠️ 卡面 F2 / O1 的唯一落点：默认金额取**未付**，不是 `amount`
            default_amount: if can_pay { unpaid } else { Decimal::ZERO },
            pending_payment_no: pending.map(|(no, _)| no.to_string()),
            pending_amount: pending.map(|(_, amount)| amount),
            overpay_risk: overpay_risk_of(row, pending),
        }
    }
}

/// 可付信息（`GET /payable/{id}/payment-info` 的出参）。
#[derive(Debug, Clone, Serialize)]
pub struct PaymentInfoRes {
    /// 单头 + 付款能力。
    pub payable: PaymentInfoPayable,
    /// 已登记的全部付款记录（`pay_date` 升序，含审批状态）。
    pub payments: Vec<PaymentRes>,
}

/// 可付信息：单头 + 付款能力 + 付款记录；不存在 / 已删除 → `404 + 10002`。
///
/// # 为什么不开事务 / 不加锁
///
/// 这是一次**只读快照**：`payable` 与 `payments` 之间允许有毫秒级的并发差（有人同时在付款），
/// 真正的护栏在写路径（`lock_payable` + `validate_payment_target`）。
/// 加锁读会让「打开抽屉」和「正在付款」互相等，收益为零。
pub async fn payment_info(id: Uuid) -> std::result::Result<PaymentInfoRes, ApiError> {
    let row = payable_query::find_payable(id)
        .await
        .map_err(|error| utils::db_error(error, "查询应付单"))?
        .ok_or_else(not_found)?;
    let pending = pending_payment_of(db(), id).await?;
    let pending_ref = pending
        .as_ref()
        .map(|(payment_no, amount)| (payment_no.as_str(), *amount));
    // 作废审批中的单才去查审批单号（多一次 DB 往返只为一句更准的文案，其它状态不付这个成本）
    let cancel_instance_no = if row.status == payable::STATUS_PENDING_CANCEL {
        Some(instance_no_of(row.cancel_instance_id).await?)
    } else {
        None
    };
    let payments = payments_res(db(), id).await?;
    Ok(PaymentInfoRes {
        payable: PaymentInfoPayable::from_row(
            &row,
            pending_ref,
            voucher::today_cst(Utc::now()),
            cancel_instance_no.as_deref(),
        ),
        payments,
    })
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

/// 按 id 取一张应付单（**不加锁**，只读快照）。
///
/// 与 [`lock_payable`] 同一套列与映射（`payable::COLUMNS` → [`payable::PayableRow`]），
/// 差别只是没有 `FOR UPDATE`：入口校验用它，真正写之前再 `lock_payable` 复核一次。
async fn find_payable_row(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<PayableRow>> {
    // SCOPE-EXEMPT: 应付台账无归属列（见 payable 模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {} FROM {} WHERE id = ?::uuid AND deleted_at IS NULL",
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
        cancel_instance_id: row.get_typed("cancel_instance_id")?,
        cancel_reason: row.get_typed("cancel_reason")?,
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
/// 付款的事务体（直付路径：未配流程 / 降级）。便于单测直接复算。
///
/// 与审批路径（[`settle_payment_in_tx`]）**共用同一段记账逻辑**：先落付款记录，再走
/// 「凭证 → 回写 → 应付重算」。两条路径唯一的差别是付款记录落库时的 `approval_status`
/// （直付 = `NONE`，审批通过 = 把已有的 `PENDING` 翻成 `APPROVED`）。
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
    validate_payment_target(&row, amount)?;

    // ① 付款记录（先落记录，凭证要指向它）—— 直付没有审批实例
    let (payment_id, payment_no) = insert_payment_row(
        tx,
        &row,
        amount,
        pay_date,
        pay_method,
        remark,
        operator,
        now,
        APPROVAL_NONE,
        None,
    )
    .await?;

    // ②③④ 凭证 + 回写 + 应付重算（P7 的唯一实现，审批路径同样调它）
    settle_payment_in_tx(
        tx,
        SettleInput {
            payable: &row,
            payment_id,
            payment_no: &payment_no,
            amount,
            pay_date,
            pay_method,
            remark,
            operator,
            now,
        },
    )
    .await
}

// ==================== 共用：校验 / 落记录 / 记账 ====================

/// 付款前的两条硬校验（直付与审批两条路径共用，**P7 的口径一行没放松**）。
///
/// ① 状态必须是 `OPEN` / `PARTIAL_PAID`（`SETTLED` / `CANCELLED` → 409）；
/// ② `0 < amount ≤ unpaid_amount`（400，文案点名两个金额）。
fn validate_payment_target(row: &PayableRow, amount: Decimal) -> std::result::Result<(), ApiError> {
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
    Ok(())
}

/// 落一条付款记录（返回 `(id, 付款单号)`）。直付与审批发起共用。
#[allow(clippy::too_many_arguments)]
async fn insert_payment_row(
    tx: &Transaction,
    row: &PayableRow,
    amount: Decimal,
    pay_date: NaiveDate,
    pay_method: Option<&str>,
    remark: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
    approval_status: &str,
    approval_instance_id: Option<Uuid>,
) -> std::result::Result<(Uuid, String), ApiError> {
    let payment_no = voucher::next_doc_no(tx, &PAYMENT_SEQ, now)
        .await
        .map_err(|error| utils::db_error(error, "取付款单号"))?;
    let operator_text = operator.map(|v| v.to_string()).unwrap_or_default();
    // SCOPE-EXEMPT: 付款台账无归属列（见模块文档）。
    let inserted = tx
        .query_first_sql(
            format!(
                "INSERT INTO {TABLE} \
                    (payment_no, payable_id, amount, pay_date, pay_method, remark, \
                     created_at, updated_at, create_by, update_by, approval_status, approval_instance_id) \
                 VALUES (?, ?::uuid, NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::date, \
                         NULLIF(?::text,''), NULLIF(?::text,''), ?, ?, \
                         NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, ?, NULLIF(?::text,'')::uuid) \
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
                Value::Text(operator_text.clone()),
                Value::Text(operator_text),
                Value::Text(approval_status.to_string()),
                Value::Text(
                    approval_instance_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                ),
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
    Ok((payment_id, payment_no))
}

/// 一次记账的全部入参（结构体化，避开 `clippy::too_many_arguments`）。
struct SettleInput<'a> {
    /// 已加行锁的应付单。
    payable: &'a PayableRow,
    /// 付款记录 id。
    payment_id: Uuid,
    /// 付款单号。
    payment_no: &'a str,
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
    /// 业务时间。
    now: DateTime<Utc>,
}

/// **记账的唯一实现**（P7 逻辑原样复用，审批路径不许另写一份）。
///
/// 三步：
/// 1. 付款凭证：借 `2202` 应付账款 / 贷 `1002` 银行存款，各 = 本次付款金额（同事务，直接 POSTED）；
/// 2. 付款记录回写 `voucher_id`；
/// 3. 应付单重算：`paid += amount`、`unpaid = amount - paid`、
///    `status = unpaid == 0 ? SETTLED : PARTIAL_PAID`，走状态机唯一入口
///    （`transition_if_changed`：`PARTIAL_PAID → PARTIAL_PAID` 不写状态日志）。
///
/// 调用前提：调用方已 `SELECT ... FOR UPDATE` 锁住应付单，且 `amount ≤ unpaid`。
async fn settle_payment_in_tx(
    tx: &Transaction,
    input: SettleInput<'_>,
) -> std::result::Result<(), ApiError> {
    let SettleInput {
        payable: row,
        payment_id,
        payment_no,
        amount,
        pay_date,
        pay_method: _pay_method,
        remark,
        operator,
        now,
    } = input;
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

    // ② 付款凭证：借 2202 应付账款 / 贷 1002 银行存款（同事务，直接 POSTED）
    let payable_account =
        voucher::account_required(tx, PAY_DEBIT_CODE, "付款凭证的借方科目 2202 应付账款")
            .await
            .map_err(|error| write_error(error, "解析付款凭证借方科目"))?;
    let bank_account =
        voucher::account_required(tx, PAY_CREDIT_CODE, "付款凭证的贷方科目 1002 银行存款")
            .await
            .map_err(|error| write_error(error, "解析付款凭证贷方科目"))?;
    let summary = format!("付款 {payment_no} 应付单 {}", row.payable_no);
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
            source_no: Some(payment_no),
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

// ==================== 写：提交付款（审批前置） ====================

/// 提交一笔付款（**handler 的唯一入口**）：有已发布的付款流程 → 发起审批；没有 → 降级直付。
///
/// 卡面 ③：「`POST /payable/{id}/pay` 改成先发起审批，返回 202」。
/// 降级是**显式**的（`outcome = PAID` + `message` 说明），不是静默改行为。
pub async fn submit_payment(
    id: Uuid,
    req: &PayPayableReq,
    operator: Option<Uuid>,
) -> std::result::Result<PaySubmitRes, ApiError> {
    let amount = req
        .amount
        .map(|value| value.round_dp(4))
        .ok_or_else(|| bad_request("amount 必填：本次付款金额（> 0，且不超过未付余额）"))?;
    if amount <= Decimal::ZERO {
        return Err(bad_request(format!(
            "本次付款金额必须大于 0，收到 {amount}"
        )));
    }

    let flow = crate::business::erp::payable_bridge::published_pay_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询已发布的付款流程"))?;
    let Some(flow) = flow else {
        // 未配流程 → 保持 P7 老行为（直接付款）。**这不是失败**，只是没启用审批。
        tracing::warn!(
            payable_id = %id,
            "没有配置已发布的 payable_pay 付款审批流程：本次付款**降级**为直接生效（无审批），请尽快补配流程"
        );
        let detail = pay_payable(id, req, operator).await?;
        return Ok(PaySubmitRes {
            outcome: OUTCOME_PAID.to_string(),
            message: "未配置付款审批流程，本次付款已直接生效".to_string(),
            amount,
            payment_id: detail.payments.last().map(|row| row.id.clone()),
            payment_no: detail.payments.last().map(|row| row.payment_no.clone()),
            approval_instance_id: None,
            detail,
        });
    };

    let outcome = submit_for_approval(id, req, amount, operator, &flow).await?;
    Ok(outcome)
}

/// 发起付款审批（一个事务里：落 `PENDING` 付款记录 → `engine::start` → 双表回写引用）。
#[allow(clippy::too_many_arguments)]
async fn submit_for_approval(
    id: Uuid,
    req: &PayPayableReq,
    amount: Decimal,
    operator: Option<Uuid>,
    flow: &crate::business::erp::payable_bridge::PayFlow,
) -> std::result::Result<PaySubmitRes, ApiError> {
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "发起付款审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let now = Utc::now();
    let pay_date = req.pay_date.unwrap_or_else(|| voucher::today_cst(now));
    let pay_method = trimmed(req.pay_method.as_deref());
    let remark = trimmed(req.remark.as_deref());

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启付款审批事务"))?;
    match submit_for_approval_tx(
        &tx,
        id,
        amount,
        pay_date,
        pay_method,
        remark,
        operator_id,
        flow,
        now,
    )
    .await
    {
        Ok((payment_id, payment_no, instance_id)) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交付款审批事务"))?;
            tracing::info!(
                payable_id = %id,
                %payment_id,
                payment_no = %payment_no,
                %instance_id,
                amount = %amount,
                flow_code = %flow.flow_code,
                "应付单已发起付款审批（账未动，等待审批）"
            );
            let detail = payable_query::payable_detail(id).await?;
            Ok(PaySubmitRes {
                outcome: OUTCOME_PENDING.to_string(),
                message: format!(
                    "已提交审批（付款单 {payment_no}，金额 {amount}），等待审批通过后才会记账"
                ),
                amount,
                payment_id: Some(payment_id.to_string()),
                payment_no: Some(payment_no),
                approval_instance_id: Some(instance_id.to_string()),
                detail,
            })
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 付款审批的**表单变量**（P1-FIX：显示名也一起冻结）。
///
/// ⚠️ `amount` 必须是 **JSON 数字**：条件节点用 `cmp(lt)` 比较，字符串会 TypeMismatch → 静默走兜底边。
/// 显示名（`supplier_name`）是给审批人看的：审批岗位角色没有 `business:erp:*` 读权限，
/// 前端「ID→名称」必然降级成 `(ID: …)`。键名契约见 [`crate::business::erp::form_snapshot`]。
async fn payment_form_data(
    row: &PayableRow,
    payment_id: Uuid,
    payment_no: &str,
    amount: Decimal,
    pay_date: NaiveDate,
    operator: Uuid,
) -> serde_json::Value {
    let mut form = serde_json::json!({
        "payment_id": payment_id.to_string(),
        "payment_no": payment_no,
        "payable_id": row.id.to_string(),
        "payable_no": row.payable_no,
        "supplier_id": row.supplier_id.to_string(),
        "amount": amount_number(amount),
        "amount_text": amount.to_string(),
        "unpaid_amount": amount_number(row.unpaid_amount),
        "pay_date": pay_date.to_string(),
        // 内控留痕：审批人 ≠ 制单人（真正的拦截在 payable_bridge::guard_self_approval）
        "create_by": operator.to_string(),
    });
    // 查不到就不写这个键（前端按 `supplier_id` 兜底），不写空串冒充「查到了」。
    match form_snapshot::supplier_name(db(), row.supplier_id).await {
        Ok(Some(name)) => {
            if let Some(object) = form.as_object_mut() {
                object.insert("supplier_name".to_string(), serde_json::Value::String(name));
            }
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(payable_id = %row.id, %error, "审批快照：供应商名查询失败，降级");
        }
    }
    form
}

/// [`submit_for_approval`] 的事务体。
#[allow(clippy::too_many_arguments)]
async fn submit_for_approval_tx(
    tx: &Transaction,
    id: Uuid,
    amount: Decimal,
    pay_date: NaiveDate,
    pay_method: Option<&str>,
    remark: Option<&str>,
    operator: Uuid,
    flow: &crate::business::erp::payable_bridge::PayFlow,
    now: DateTime<Utc>,
) -> std::result::Result<(Uuid, String, Uuid), ApiError> {
    // ① 锁应付单 + 校验（P7 的两条校验一条都不能丢）
    let row = lock_payable(tx, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定应付单"))?
        .ok_or_else(not_found)?;
    validate_payment_target(&row, amount)?;

    // ② 审批中不能重复付款（先查，别等部分唯一索引 uk_biz_payment_pending 抛 23505）
    if let Some((payment_no, pending_amount)) = pending_payment_of(tx, id).await? {
        return Err(conflict(format!(
            "该应付单已有一笔付款在审批中（{payment_no}，金额 {pending_amount}），请等待审批结果\
             （驳回或撤回后可重新提交）"
        )));
    }

    // ③ 落付款记录（PENDING）：实例 id 先算好，与记录同一条 INSERT（库侧约束 PENDING ⟹ 有实例）
    let instance_id = Uuid::now_v7();
    let (payment_id, payment_no) = insert_payment_row(
        tx,
        &row,
        amount,
        pay_date,
        pay_method,
        remark,
        Some(operator),
        now,
        APPROVAL_PENDING,
        Some(instance_id),
    )
    .await?;

    // ④ 发起审批（表单数据冻结：金额分级条件边就是读这里的 form.amount）
    let Some((initiator_name, initiator_dept_id)) = flow_model::user_profile(operator)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
    else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "发起付款审批失败：当前用户不存在或已停用",
        ));
    };
    let title = format!("{} 付款 {} 元", row.payable_no, amount)
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
        biz_id: Some(payment_id),
        biz_no: Some(payment_no.clone()),
        title,
        initiator_id: operator,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: payment_form_data(&row, payment_id, &payment_no, amount, pay_date, operator)
            .await,
    };
    let out = engine::start(tx, &start)
        .await
        .map_err(|error| write_error(error, "发起付款审批单"))?;
    if let Some(rejected) = out.rejected {
        let hint = if rejected.code == error_code::FLOW_SELF_APPROVAL_FORBIDDEN {
            "（内控要求：付款审批人不能是制单人本人。请让另一个账号审批）"
        } else if rejected.code == error_code::FLOW_NO_ASSIGNEE {
            "（流程节点没有有效审批人：请在「流程定义 → 应付付款审批」里配置审批人或开启超管兜底）"
        } else {
            ""
        };
        return Err(ApiError::new(
            rejected.status,
            rejected.code,
            format!("{}{hint}", rejected.msg),
        ));
    }
    if out.instance_id != Some(instance_id) {
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            "审批单发起后返回的实例 id 与预生成的不一致（引擎内部错误）",
        ));
    }
    if out.instance_status != "PENDING" {
        // 自动通过 / 直接被驳回：都不该发生在「至少一个审批节点」的付款流程上
        return Err(ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!(
                "付款审批单发起后状态是 {}（应为 PENDING）：流程定义可能没有可执行的审批节点",
                out.instance_status
            ),
        ));
    }

    // ⑤ 双表回写引用：付款记录已带实例 id（③ 一步到位），这里补应付单的弱引用
    // SCOPE-EXEMPT: 应付台账无归属列（见模块文档）。
    tx.exec_sql(
        format!(
            "UPDATE {} SET payment_approval_instance_id = ?::uuid, updated_at = ? \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            payable::TABLE
        ),
        vec![
            Value::from(&instance_id),
            Value::from(now),
            Value::from(&row.id),
        ],
    )
    .await
    .map_err(|error| utils::db_error(error, "回写应付单的审批实例引用"))?;

    Ok((payment_id, payment_no, instance_id))
}

/// 取该应付单**在审批中**的付款记录（单号 + 金额）；没有 → `None`。
async fn pending_payment_of(
    ex: &impl SqlExecutor,
    payable_id: Uuid,
) -> std::result::Result<Option<(String, Decimal)>, ApiError> {
    // SCOPE-EXEMPT: 付款台账无归属列（见模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT payment_no, amount FROM {TABLE} \
                  WHERE payable_id = ?::uuid AND approval_status = '{APPROVAL_PENDING}' \
                    AND deleted_at IS NULL \
                  ORDER BY created_at DESC LIMIT 1"
            ),
            vec![Value::from(&payable_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询在审批中的付款"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let payment_no: String = row.get_typed("payment_no")?;
    let amount: Decimal = row.get_typed("amount")?;
    Ok(Some((payment_no, amount)))
}

/// `Decimal` → JSON **数字**（条件节点的 `cmp` 只认 `Number`；字符串会 TypeMismatch）。
///
/// `rust_decimal` 在本仓没开 `maths` feature（没有 `to_f64`），所以走字符串解析：
/// 金额是定点数，`"5000.0000" -> 5000.0` 这一步对「比较阈值」足够精确（4 位小数 × 1e15 量级内无精度损失）。
pub(crate) fn amount_number(amount: Decimal) -> serde_json::Value {
    let value = amount.to_string().parse::<f64>().unwrap_or_default();
    serde_json::Number::from_f64(value)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null)
}

// ==================== 写：审批结论 → 记账 / 放弃 ====================

/// 审批通过后的执行结果（回调据此决定日志与通知）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettleOutcome {
    /// 记账完成（应付回写 + 付款凭证）。
    Paid,
    /// 付款记录已是终态（重复回调 / 已被别处处理）。
    AlreadySettled,
    /// 应付单已作废 → 拒绝执行，付款记录标 `CANCELLED`。
    Cancelled {
        /// 拒绝原因（落日志 / 通知用）。
        reason: String,
    },
    /// 执行条件不再成立（如未付余额已被别处变小）→ 付款记录标 `REJECTED`。
    Rejected {
        /// 拒绝原因。
        reason: String,
    },
}

/// 执行一笔**已审批通过**的付款（卡面 ⑩⑪⑫⑬⑯）。
///
/// 一个事务里：锁付款记录 → 锁应付单 → 复核「应付单没被作废 / 未付余额还够」→
/// 记账（复用 [`settle_payment_in_tx`]）→ 付款记录翻 `APPROVED` → 清应付单的实例引用。
pub async fn settle_approved_payment(
    payment_id: Uuid,
    operator: Option<Uuid>,
) -> std::result::Result<SettleOutcome, ApiError> {
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启付款执行事务"))?;
    match settle_approved_in_tx(&tx, payment_id, operator, now).await {
        Ok(outcome) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交付款执行事务"))?;
            if matches!(outcome, SettleOutcome::Paid) {
                tracing::info!(%payment_id, "付款审批通过，付款已记账（应付回写 + 付款凭证）");
            }
            Ok(outcome)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// [`settle_approved_payment`] 的事务体。
async fn settle_approved_in_tx(
    tx: &Transaction,
    payment_id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<SettleOutcome, ApiError> {
    // ① 锁付款记录（锁序：付款记录 → 应付单，与发起路径一致）
    let Some(payment) = lock_payment(tx, payment_id).await? else {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            "付款记录不存在或已删除".to_string(),
        ));
    };
    if payment.approval_status != APPROVAL_PENDING {
        tracing::info!(
            %payment_id,
            status = %payment.approval_status,
            "付款记录已是终态（重复回调 / 已被别处处理），跳过"
        );
        return Ok(SettleOutcome::AlreadySettled);
    }

    // ② 锁应付单 + 复核「还能不能付款」
    let Some(row) = lock_payable(tx, payment.payable_id)
        .await
        .map_err(|error| utils::db_error(error, "锁定应付单"))?
    else {
        return Err(not_found());
    };
    if row.status == payable::STATUS_CANCELLED {
        mark_payment_terminal(tx, payment_id, APPROVAL_CANCELLED, now).await?;
        clear_payable_approval_ref(tx, row.id, now).await?;
        return Ok(SettleOutcome::Cancelled {
            reason: format!("应付单 {} 已作废", row.payable_no),
        });
    }
    if !matches!(
        row.status.as_str(),
        payable::STATUS_OPEN | payable::STATUS_PARTIAL_PAID
    ) {
        let reason = format!(
            "应付单 {} 当前是「{}」，付款审批通过时已不该再付款",
            row.payable_no,
            payable_query::status_label(&row.status)
        );
        mark_payment_terminal(tx, payment_id, APPROVAL_REJECTED, now).await?;
        clear_payable_approval_ref(tx, row.id, now).await?;
        return Ok(SettleOutcome::Rejected { reason });
    }
    let amount = payment.amount.round_dp(4);
    if amount > row.unpaid_amount.round_dp(4) {
        let reason = format!(
            "本次付款 {amount} 超过未付余额 {}（应付单 {}）：审批期间未付余额已变",
            row.unpaid_amount, row.payable_no
        );
        mark_payment_terminal(tx, payment_id, APPROVAL_REJECTED, now).await?;
        clear_payable_approval_ref(tx, row.id, now).await?;
        return Ok(SettleOutcome::Rejected { reason });
    }

    // ③ 真记账（P7 的唯一实现）+ 付款记录翻 APPROVED + 清应付单引用
    settle_payment_in_tx(
        tx,
        SettleInput {
            payable: &row,
            payment_id,
            payment_no: &payment.payment_no,
            amount,
            pay_date: payment.pay_date,
            pay_method: payment.pay_method.as_deref(),
            remark: payment.remark.as_deref(),
            operator,
            now,
        },
    )
    .await?;
    mark_payment_terminal(tx, payment_id, APPROVAL_APPROVED, now).await?;
    clear_payable_approval_ref(tx, row.id, now).await?;
    Ok(SettleOutcome::Paid)
}

/// 驳回 / 撤回：付款记录标 `REJECTED`，账一分不动，可重新提交。
///
/// 返回 `true` = 这次真的翻了状态；`false` = 不是 `PENDING`（重复回调 / 已被别处处理）。
pub async fn abort_payment(payment_id: Uuid, note: &str) -> std::result::Result<bool, ApiError> {
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启付款驳回事务"))?;
    let outcome = async {
        let Some(payment) = lock_payment(&tx, payment_id).await? else {
            return Ok::<bool, ApiError>(false);
        };
        if payment.approval_status != APPROVAL_PENDING {
            return Ok(false);
        }
        mark_payment_terminal(&tx, payment_id, APPROVAL_REJECTED, now).await?;
        clear_payable_approval_ref(&tx, payment.payable_id, now).await?;
        Ok(true)
    }
    .await;
    match outcome {
        Ok(flag) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交付款驳回事务"))?;
            if flag {
                tracing::info!(%payment_id, note = note, "付款记录已标 REJECTED（账一分没动，可重新提交）");
            }
            Ok(flag)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 锁一条付款记录（`SELECT ... FOR UPDATE`）。
async fn lock_payment(
    tx: &Transaction,
    payment_id: Uuid,
) -> std::result::Result<Option<PaymentRow>, ApiError> {
    // SCOPE-EXEMPT: 付款台账无归属列（见模块文档）。
    let row = tx
        .query_first_sql(
            format!(
                "SELECT {COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&payment_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "锁定付款记录"))?;
    row.as_ref()
        .map(PaymentRow::from_row)
        .transpose()
        .map_err(|error| utils::db_error(error, "读取付款记录"))
}

/// 把付款记录翻到终态（`APPROVED` / `REJECTED` / `CANCELLED`）。
///
/// 只翻 `PENDING` 的行（CAS）：并发下第二个回调拿 0 行 → 记 WARN，不覆盖已落的结果。
async fn mark_payment_terminal(
    tx: &Transaction,
    payment_id: Uuid,
    status: &str,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    // SCOPE-EXEMPT: 付款台账无归属列（见模块文档）。
    let affected = tx
        .exec_sql(
            format!(
                "UPDATE {TABLE} SET approval_status = ?, updated_at = ? \
                  WHERE id = ?::uuid AND approval_status = '{APPROVAL_PENDING}'"
            ),
            vec![
                Value::Text(status.to_string()),
                Value::from(now),
                Value::from(&payment_id),
            ],
        )
        .await
        .map_err(|error| write_error(error, "回写付款记录的审批状态"))?;
    if affected == 0 {
        tracing::warn!(
            %payment_id,
            status = status,
            "付款记录已被别的路径推进到终态（CAS 落空），本次不覆盖"
        );
    }
    Ok(())
}

/// 清掉应付单上的「在跑付款审批」弱引用（终态后必须清）。
async fn clear_payable_approval_ref(
    tx: &Transaction,
    payable_id: Uuid,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    // SCOPE-EXEMPT: 应付台账无归属列（见模块文档）。
    tx.exec_sql(
        format!(
            "UPDATE {} SET payment_approval_instance_id = NULL, updated_at = ? \
              WHERE id = ?::uuid AND payment_approval_instance_id IS NOT NULL",
            payable::TABLE
        ),
        vec![Value::from(now), Value::from(&payable_id)],
    )
    .await
    .map_err(|error| utils::db_error(error, "清空应付单的审批实例引用"))?;
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
    if row.status == payable::STATUS_PENDING_CANCEL {
        return Err(conflict(format!(
            "应付单 {} 正在作废审批中：请等审批结束，或先撤回审批",
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
    cancel_body_in_tx(tx, &row, reason, operator, now).await
}

/// 作废的**事务体**（不含入口校验，`from` 取单据当前状态）。
///
/// 两条入口共用：
/// * [`cancel_in_tx`]（起手作废：`OPEN` + 已付 0）；
/// * [`cancel_after_approval`]（作废审批通过：`PENDING_CANCEL`）。
///
/// 差别只有「进来时是什么状态」，之后做的事一字不差：
/// 终止在跑的付款审批 → 红冲进项凭证 → 状态机 → CAS 回写状态与备注。
async fn cancel_body_in_tx(
    tx: &Transaction,
    row: &PayableRow,
    reason: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    let from_status = row.status.clone();
    // ★ P17 反向（硬红线 9）：付款审批中把应付单作废 → **先撤付款审批**再作废单据。
    //   PENDING 的付款记录不动账（paid 仍是 0），所以这里不会和「已付款不能作废」冲突；
    //   但审批实例必须终止 —— 否则它会顶住 uk_biz_payment_pending，让这张单永远付不出钱。
    let revoked = payable_bridge::cancel_pending_of_payable(
        tx,
        row.id,
        "应付单已作废，付款审批自动终止（账一分没动）",
    )
    .await
    .map_err(|error| utils::db_error(error, "终止在跑的付款审批"))?;
    if revoked > 0 {
        tracing::info!(
            payable_id = %row.id,
            payable_no = %row.payable_no,
            revoked,
            "应付单作废：在跑的付款审批已终止（付款记录标 CANCELLED）"
        );
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
    // P10 铁律 ③：走状态机唯一入口（reason 必填由注册表把关）
    state_machine::transition(
        tx,
        state_machine::ENTITY_PAYABLE,
        row.id,
        &row.payable_no,
        &state_machine::PAYABLE_EDGES,
        &from_status,
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
    // `cancel_instance_id` 必须一起摘掉：`ck_biz_payable_pending_cancel` 要求
    // 「非 PENDING_CANCEL ⟹ 没有实例引用」，不摘就是 23514。
    let affected = tx
        .exec_sql(
            format!(
                "UPDATE {} \
                    SET status = ?, remark = ?, cancel_instance_id = NULL, updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND deleted_at IS NULL AND status = ?",
                payable::TABLE,
            ),
            vec![
                Value::Text(payable::STATUS_CANCELLED.to_string()),
                Value::Text(note),
                Value::from(now),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(&row.id),
                Value::Text(from_status),
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

// ==================== P-FLOW-3B：应付单作废审批 ====================

/// 作废提交的出参（与付款的 [`PaySubmitRes`] 同款：`outcome` 区分「送审」与「已作废」）。
#[derive(Debug, Clone, Serialize)]
pub struct CancelSubmitRes {
    /// `PENDING_APPROVAL`（已送审，欠款还在） / `CANCELLED`（未配流程，直接作废）。
    pub outcome: String,
    /// 给用户看的一句话。
    pub message: String,
    /// 审批实例 id（未配流程时为 `None`）。
    pub approval_instance_id: Option<String>,
    /// 应付单最新详情（flatten：老前端读 `status` / `payable_no` 照样拿得到）。
    #[serde(flatten)]
    pub detail: PayableDetailRes,
}

/// 作废提交的 outcome：已送审（单据进 `PENDING_CANCEL`，欠款一分没动）。
pub const OUTCOME_CANCEL_PENDING: &str = "PENDING_APPROVAL";
/// 作废提交的 outcome：未配流程，按老行为**直接作废**。
pub const OUTCOME_CANCELLED: &str = "CANCELLED";

/// 提交应付单作废（**唯一对外入口**，P-FLOW-3B）。
///
/// 配了已发布的 `payable_cancel` 流程 → 先送审（`OPEN → PENDING_CANCEL`，账不动）；
/// 没配流程 → 按 P-FLOW-3B 之前的行为**直接作废**（并留一条 `warn`，绝不把作废锁死）。
///
/// 为什么入口收在这里、而不是让「作废」和「送审」各开一个接口：
/// 那样审批可以被换一个 URL 绕过（与 `stockin_handler::submit_post` 同一个理由）。
pub async fn submit_cancel(
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
) -> std::result::Result<CancelSubmitRes, ApiError> {
    let reason = reason.map(str::trim).filter(|text| !text.is_empty());
    let Some(reason) = reason else {
        return Err(bad_request(
            "reason 必填：请写明作废原因（会计上作废必须留痕，财务经理据此复核）",
        ));
    };
    match payable_bridge::published_cancel_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询应付作废审批流程"))?
    {
        Some(flow) => submit_cancel_for_approval(id, reason, operator, &flow).await,
        None => {
            tracing::warn!(
                payable_id = %id,
                "没有配置已发布的 payable_cancel 应付作废审批流程：本次作废**降级**为直接执行（无复核），\
                 请尽快补配流程"
            );
            let detail = cancel_payable(id, Some(reason), operator).await?;
            Ok(CancelSubmitRes {
                outcome: OUTCOME_CANCELLED.to_string(),
                message: "未配置应付作废审批流程，本次作废已直接生效".to_string(),
                approval_instance_id: None,
                detail,
            })
        }
    }
}

/// 发起作废审批（`OPEN → PENDING_CANCEL`，只挂审批单，欠款一分没动）。
async fn submit_cancel_for_approval(
    id: Uuid,
    reason: &str,
    operator: Option<Uuid>,
    flow: &payable_bridge::PayFlow,
) -> std::result::Result<CancelSubmitRes, ApiError> {
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交作废审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let row = find_payable_row(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询应付单"))?
        .ok_or_else(not_found)?;
    // ① 状态闸：只有 OPEN + 一分钱没付的能送审（与直接作废同一口径，双保险）
    if row.status == payable::STATUS_PENDING_CANCEL {
        let instance_no = instance_no_of(row.cancel_instance_id).await?;
        return Err(conflict(format!(
            "应付单 {} 已在作废审批中（{instance_no}）：请等审批结束，或先撤回审批",
            row.payable_no
        )));
    }
    if row.status == payable::STATUS_CANCELLED {
        return Err(conflict(format!(
            "应付单 {} 已经作废过了，不能重复提交",
            row.payable_no
        )));
    }
    if row.paid_amount > Decimal::ZERO || row.status != payable::STATUS_OPEN {
        return Err(conflict(format!(
            "应付单 {} 当前是「{}」、已付 {}，不能提交作废审批：只有「未付且一分钱都没付」\
             （OPEN + 已付 0）的应付单能作废 —— 已部分付款 / 已结清的请走付款冲销，不要作废单据",
            row.payable_no,
            payable_query::status_label(&row.status),
            row.paid_amount
        )));
    }
    // ② 幂等：同一单已有在跑的作废审批 → 409（先查，别等库层
    //    uk_biz_flow_instance_biz_active）
    if flow_model::biz_instance_running(payable::CANCEL_BIZ_TYPE, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = instance_no_of(row.cancel_instance_id).await?;
        return Err(conflict(format!(
            "应付单 {} 已在作废审批中（{instance_no}）：请等审批结束，或从审批中心撤回后再提交",
            row.payable_no
        )));
    }
    let Some((initiator_name, initiator_dept_id)) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
    else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "发起应付作废审批失败：当前用户不存在或已停用",
        ));
    };
    let instance_id = Uuid::now_v7();
    let title = format!("{} 应付单作废", row.payable_no)
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
        biz_no: Some(row.payable_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: cancel_form_data(&row, reason, operator_id).await,
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启作废审批事务"))?;
    match submit_cancel_tx(&tx, &row, reason, operator_id, &start, now).await {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交作废审批事务"))?;
            tracing::info!(
                payable_id = %row.id,
                payable_no = %row.payable_no,
                %instance_id,
                flow_code = %start.flow_code,
                "应付单已提交作废审批（状态 PENDING_CANCEL，欠款未动）"
            );
            let detail = payable_query::payable_detail(id).await?;
            Ok(CancelSubmitRes {
                outcome: OUTCOME_CANCEL_PENDING.to_string(),
                message: format!(
                    "已提交作废审批（应付单 {}），等待财务经理复核；审批通过后才会真正作废",
                    row.payable_no
                ),
                approval_instance_id: Some(instance_id.to_string()),
                detail,
            })
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// [`submit_cancel_for_approval`] 的事务体：状态机过闸 → 发起审批单 → CAS 回写。
///
/// 三步同一个事务：**不会出现「单据在审批中但审批单没了」**，也不会出现
/// 「审批单在跑但单据还是 OPEN」（后者会让它被别处再发起一次）。
async fn submit_cancel_tx(
    tx: &Transaction,
    row: &PayableRow,
    reason: &str,
    operator: Uuid,
    start: &engine::StartReq,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    // ① 状态机唯一入口：边表校验 + 写 biz_status_log（reason 必填由注册表把关）
    state_machine::transition(
        tx,
        state_machine::ENTITY_PAYABLE,
        row.id,
        &row.payable_no,
        &state_machine::PAYABLE_EDGES,
        payable::STATUS_OPEN,
        payable::STATUS_PENDING_CANCEL,
        Some(operator),
        Some(reason),
        None,
    )
    .await?;
    // ② 发起审批单（引擎在同一事务里建实例 + 待办 + 节点）
    let mut out = engine::start(tx, start)
        .await
        .map_err(|error| write_error(error, "发起应付作废审批单"))?;
    if let Some(rejected) = out.rejected.take() {
        return Err(ApiError::new(
            rejected.status,
            rejected.code,
            format!(
                "{}（内控要求：作废复核人不能是发起人本人。请由他人提交，或让另一个账号具备复核权限后再审）",
                rejected.msg
            ),
        ));
    }
    let instance_id = out
        .instance_id
        .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
    // ③ CAS 回写单据：状态 + 实例 id + 原因（三者同一笔 UPDATE，否则撞
    //    ck_biz_payable_pending_cancel）
    // SCOPE-EXEMPT: 应付台账无归属列（见 payable 模块文档）。
    let affected = tx
        .exec_sql(
            "UPDATE biz_payable \
                SET status = ?, cancel_instance_id = ?::uuid, cancel_reason = ?, \
                    updated_at = ?, update_by = ?::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = ?"
                .to_string(),
            vec![
                Value::Text(payable::STATUS_PENDING_CANCEL.to_string()),
                Value::from(&instance_id),
                Value::Text(reason.to_string()),
                Value::from(now),
                Value::from(&operator),
                Value::from(&row.id),
                Value::Text(payable::STATUS_OPEN.to_string()),
            ],
        )
        .await
        .map_err(|error| write_error(error, "应付单进入作废审批中"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "应付单 {} 的状态在本次提交期间已被改动（期望 OPEN），请刷新后重试",
            row.payable_no
        )));
    }
    Ok(())
}

/// 作废审批的**表单变量**（与付款审批同口径：显示名也一起冻结）。
///
/// `amount` 必须是 **JSON 数字**（虽然 `payable_cancel` 流程没有金额分级，
/// 但审批详情页与将来的「大额加签」都读这个键，口径统一）。
async fn cancel_form_data(row: &PayableRow, reason: &str, operator: Uuid) -> serde_json::Value {
    let mut form = serde_json::json!({
        "payable_id": row.id.to_string(),
        "payable_no": row.payable_no,
        "supplier_id": row.supplier_id.to_string(),
        "source_no": row.source_no,
        "amount": amount_number(row.amount),
        "amount_text": row.amount.to_string(),
        "paid_amount": amount_number(row.paid_amount),
        "unpaid_amount": amount_number(row.unpaid_amount),
        "due_date": row.due_date.to_string(),
        "cancel_reason": reason,
        // 内控留痕：审批人 ≠ 制单人（真正的拦截在 payable_bridge::guard_self_approval）
        "create_by": operator.to_string(),
    });
    // 查不到就不写这个键（前端按 `supplier_id` 兜底），不写空串冒充「查到了」。
    match form_snapshot::supplier_name(db(), row.supplier_id).await {
        Ok(Some(name)) => {
            if let Some(object) = form.as_object_mut() {
                object.insert("supplier_name".to_string(), serde_json::Value::String(name));
            }
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(payable_id = %row.id, %error, "审批快照：供应商名查询失败，降级");
        }
    }
    form
}

/// 撤回在跑的作废审批（发起人自己撤，走 `engine::withdraw`，与审批中心撤回同一引擎）。
///
/// 撤回后单据回 `OPEN`（回调 `payable_bridge::on_cancel_settled(…, "WITHDRAWN")`）。
pub async fn withdraw_cancel(
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
) -> std::result::Result<PayableDetailRes, ApiError> {
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "撤回应付作废审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let row = find_payable_row(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询应付单"))?
        .ok_or_else(not_found)?;
    if row.status != payable::STATUS_PENDING_CANCEL {
        return Err(conflict(format!(
            "应付单 {} 当前是「{}」，没有在跑的作废审批可撤回",
            row.payable_no,
            payable_query::status_label(&row.status)
        )));
    }
    let Some(instance_id) = row.cancel_instance_id else {
        return Err(conflict(format!(
            "应付单 {} 处于作废审批中，但库里的审批实例引用为空（数据异常，请联系运维核查）",
            row.payable_no
        )));
    };
    let mut out = db()
        .transaction(|tx| async move { engine::withdraw(&tx, instance_id, operator_id).await })
        .await
        .map_err(|error| utils::db_error(error, "撤回作废审批"))?;
    if let Some(error) = out.rejected.take() {
        return Err(ApiError::new(error.status, error.code, error.msg));
    }
    // 与审批中心撤回同口径：审批结论回调（单据回 OPEN + 摘实例引用）
    payable_bridge::on_cancel_settled(Some(instance_id), "WITHDRAWN").await;
    tracing::info!(
        payable_id = %row.id,
        payable_no = %row.payable_no,
        %instance_id,
        reason = reason.unwrap_or("(未填)"),
        "应付作废审批已撤回（单据回 OPEN，欠款未动）"
    );
    payable_query::payable_detail(id).await
}

/// 作废审批通过后的**执行体**（`payable_bridge::on_cancel_settled` 调）。
///
/// 与 [`cancel_in_tx`] 的差别只有入口状态：这里是 `PENDING_CANCEL`（审批已通过），
/// 里子完全一样（终止在跑的付款审批 → 红冲进项凭证 → 状态机 → 回写）。
pub async fn cancel_after_approval(
    id: Uuid,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启作废执行事务"))?;
    let result = async {
        let row = lock_payable(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定应付单"))?
            .ok_or_else(not_found)?;
        if row.status != payable::STATUS_PENDING_CANCEL {
            return Err(conflict(format!(
                "应付单 {} 当前是「{}」，不在作废审批中，拒绝执行作废",
                row.payable_no,
                payable_query::status_label(&row.status)
            )));
        }
        let reason = row
            .cancel_reason
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("作废审批通过")
            .to_string();
        cancel_body_in_tx(&tx, &row, &reason, operator, now).await
    }
    .await;
    match result {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交作废执行事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 作废审批**驳回 / 撤回**后的回位体：`PENDING_CANCEL → OPEN`（账一分没动）。
pub async fn restore_after_cancel_rejection(
    id: Uuid,
    verdict: payable_bridge::CancelVerdict,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启作废回位事务"))?;
    let result = async {
        let row = lock_payable(&tx, id)
            .await
            .map_err(|error| utils::db_error(error, "锁定应付单"))?
            .ok_or_else(not_found)?;
        if row.status != payable::STATUS_PENDING_CANCEL {
            tracing::debug!(
                payable_id = %id,
                status = %row.status,
                verdict = verdict.as_str(),
                "应付单不在 PENDING_CANCEL（重复回调 / 已作废），跳过回位"
            );
            return Ok(());
        }
        state_machine::transition(
            &tx,
            state_machine::ENTITY_PAYABLE,
            row.id,
            &row.payable_no,
            &state_machine::PAYABLE_EDGES,
            payable::STATUS_PENDING_CANCEL,
            payable::STATUS_OPEN,
            operator,
            Some("作废审批未通过，单据回到未付"),
            None,
        )
        .await?;
        // SCOPE-EXEMPT: 应付台账无归属列（见 payable 模块文档）。
        let affected = tx
            .exec_sql(
                "UPDATE biz_payable \
                    SET status = ?, cancel_instance_id = NULL, updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND deleted_at IS NULL AND status = ?"
                    .to_string(),
                vec![
                    Value::Text(payable::STATUS_OPEN.to_string()),
                    Value::from(now),
                    Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                    Value::from(&row.id),
                    Value::Text(payable::STATUS_PENDING_CANCEL.to_string()),
                ],
            )
            .await
            .map_err(|error| write_error(error, "应付单回到未付"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "应付单 {} 的状态在本次回位期间被其它操作改动，请刷新后重试",
                row.payable_no
            )));
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交作废回位事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 审批单号（用于 409 文案；取不到就退回 id 文本）。
async fn instance_no_of(instance_id: Option<Uuid>) -> std::result::Result<String, ApiError> {
    let Some(instance_id) = instance_id else {
        return Ok("审批中".to_string());
    };
    let rows = db()
        .find_with_params(
            "SELECT instance_no FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询审批单号"))?;
    Ok(rows
        .first()
        .and_then(|row| row.get_typed::<String>("instance_no").ok())
        .unwrap_or_else(|| instance_id.to_string()))
}

/// 去掉首尾空白；空串归一成 `None`（写库走 `NULLIF(?::text,'')`）。
fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

// ==================== 接口层 ====================

/// 提交一笔付款：**有已发布的付款流程就走审批（202）**，没配流程就降级直付（200）。
///
/// # 出参
///
/// * `outcome = PENDING_APPROVAL` + **HTTP 202**：已发起审批，账还没动（等到审批通过才记账）；
/// * `outcome = PAID` + HTTP 200：未配置流程，按 P7 老行为直接生效。
///
/// # 异常情况
///
/// * 400 + `10001`：金额缺失 / ≤ 0 / 超过未付余额；
/// * 404 + `10002`：应付单不存在；
/// * 409 + `10003`：已结清 / 已作废 / **已有一笔付款在审批中**；
/// * 422 + `65019` / `65018`：流程侧拒绝（自审被禁 / 节点没有审批人）。
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
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let parsed = parse_uuid(&id, "id");
    let outcome = match parsed {
        Ok(id) => submit_payment(id, &req, operator_of(&claims)).await,
        Err(error) => Err(error),
    };
    match outcome {
        Ok(result) => {
            // 审批中 = 202（「已受理但未生效」），直付生效 = 200（P7 老行为）
            let status = if result.outcome == OUTCOME_PENDING {
                axum::http::StatusCode::ACCEPTED
            } else {
                axum::http::StatusCode::OK
            };
            let message = result.message.clone();
            (status, Json(Res::ok_with_msg(result, message))).into_response()
        }
        Err(error) => error.into_response(),
    }
}

/// 应付单可付信息（D5：详情抽屉「发起付款」前的唯一读数）。
///
/// # 出参
///
/// `payable`（单头 + `can_pay` / `blocked_reason` / `default_amount` = 未付）+ `payments`。
///
/// # 权限
///
/// 用 `business:erp:payable:detail`（**不新种权限码**）：这就是「看这张应付单的详情 + 它的付款记录」，
/// 与 `GET /business/erp/payable/{id}` 同权；能看详情的人才能看到「发起付款」按钮，
/// 点下去那一步仍由 `POST /payable/{id}/pay` 的 `business:erp:payable:pay` 把关（403 不会绕过）。
///
/// # 异常情况
///
/// * 400 + `66006`：路径 id 不是合法 UUID；
/// * 404 + `10002`：应付单不存在 / 已删除。
#[nik::permission(
    path = "/business/erp/payable/{id}/payment-info",
    method = "GET",
    permission = "business:erp:payable:detail"
)]
#[nik::get("/business/erp/payable/{id}/payment-info")]
pub async fn payable_payment_info_handler(Path(id): Path<String>) -> ResResult<PaymentInfoRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(payment_info(id).await?))
}

/// 作废一张应付单（仅 OPEN 且未付款）。
///
/// **P-FLOW-3B 起走审批**：配了已发布的 `payable_cancel` 流程 → 单据进 `PENDING_CANCEL`
/// 等财务经理复核（`outcome = PENDING_APPROVAL`）；没配流程 → 按老行为直接作废
/// （`outcome = CANCELLED`）。
///
/// 为什么老入口也改成送审：否则审批可以被换一个 URL 绕过（与 `stockin_handler::submit_post`
/// 把 `PATCH /status` 和 `POST /submit-post` 收到同一个函数是同一个理由）。
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
) -> ResResult<CancelSubmitRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(
        submit_cancel(id, req.reason.as_deref(), operator_of(&claims)).await?,
    ))
}

/// 提交应付单作废审批（P-FLOW-3B 的显式入口，与 `POST /payable/{id}/cancel` 同一函数）。
///
/// # 异常情况
///
/// * 400 + `10001`：`reason` 为空；
/// * 404 + `10002`：应付单不存在 / 已删除；
/// * 409 + `10003`：已作废 / 已在作废审批中 / **已付过款（PARTIAL_PAID / SETTLED 不许作废）**；
/// * 422 + `65019`：流程侧内控拒绝（复核人 = 发起人）。
#[nik::permission(
    path = "/business/erp/payable/{id}/submit-cancel",
    method = "POST",
    permission = "business:erp:payable:submit-cancel"
)]
#[nik::post("/business/erp/payable/{id}/submit-cancel")]
pub async fn payable_submit_cancel_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CancelPayableReq>,
) -> ResResult<CancelSubmitRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(
        submit_cancel(id, req.reason.as_deref(), operator_of(&claims)).await?,
    ))
}

/// 撤回在跑的应付作废审批（发起人自己撤 → 单据回 `OPEN`，欠款一分没动）。
#[nik::permission(
    path = "/business/erp/payable/{id}/withdraw-cancel",
    method = "POST",
    permission = "business:erp:payable:withdraw-cancel"
)]
#[nik::post("/business/erp/payable/{id}/withdraw-cancel")]
pub async fn payable_withdraw_cancel_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<CancelPayableReq>>,
) -> ResResult<PayableDetailRes> {
    let id = parse_uuid(&id, "id")?;
    let reason = body.and_then(|Json(req)| req.reason);
    Ok(Res::ok(
        withdraw_cancel(id, reason.as_deref(), operator_of(&claims)).await?,
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

    // ==================== D5：应付单 → 发起付款（可付信息）====================
    //
    // 夹具前缀 `AP-D5-`（与 P7 的 `AP-P7-` / P17 的 `AP-P17-` 互不重叠：清理只按自己的
    // 外键链走，绝不按 `PM-` / `AP-` 这种生产号段模糊删）。
    // **共用 P7 的 `DB_LOCK`**（同一批表：biz_payable / biz_payment / biz_voucher），
    // 审批路径的用例会留下 biz_flow_*，`d5_setup` 按「本夹具应付单 → 付款记录」清掉。

    /// D5 夹具的应付单号前缀。
    const D5_PREFIX: &str = "AP-D5-";
    /// D5 付款提交人（普通账号，见 `d5_setup` 的内控说明）。
    const D5_OPERATOR: &str = "d5d50001-0000-4000-8000-000000000001";

    /// 拿锁 + 连库 + 清上一轮 + 备好「付款能用」的环境（供应商 / 期间 / 科目 / 流程）。
    async fn d5_setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        let cleanups = [
            // 通知 → 审批实例 → 凭证分录 → 凭证 → 状态日志 → 付款记录 → 应付单
            "DELETE FROM sys_notice WHERE biz_type = 'flow' AND biz_id IN \
             (SELECT i.id::text FROM biz_flow_instance i \
               WHERE i.biz_type = 'PAYABLE_PAY' AND i.biz_id IN \
               (SELECT id FROM biz_payment WHERE payable_id IN \
                (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-D5-%')))"
                .to_string(),
            "DELETE FROM biz_flow_instance WHERE biz_type = 'PAYABLE_PAY' AND biz_id IN \
             (SELECT id FROM biz_payment WHERE payable_id IN \
              (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-D5-%'))"
                .to_string(),
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE source_id IN \
              (SELECT id FROM biz_payment WHERE payable_id IN \
               (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-D5-%')))"
                .to_string(),
            "DELETE FROM biz_voucher WHERE source_id IN \
             (SELECT id FROM biz_payment WHERE payable_id IN \
              (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-D5-%'))"
                .to_string(),
            "DELETE FROM biz_status_log WHERE entity_id IN \
             (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-D5-%')"
                .to_string(),
            "DELETE FROM biz_payment WHERE payable_id IN \
             (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-D5-%')"
                .to_string(),
            format!("DELETE FROM biz_payable WHERE payable_no LIKE '{D5_PREFIX}%'"),
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
        // 与 P7 同口径：付款日期固定 2026-10，用例不依赖「今天是哪个月」
        exec(
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
                                                start_date, end_date, status) \
             VALUES (2026, 10, '2026-10', DATE '2026-10-01', DATE '2026-10-31', 'OPEN') \
             ON CONFLICT (period_code) DO UPDATE SET status = 'OPEN'",
        )
        .await;
        exec("UPDATE biz_account SET status = 1 WHERE account_code IN ('2202', '1002')").await;
        // 审批路径要有**已发布**的付款流程（P17 的种子；别的用例可能临时关过）
        exec(
            "UPDATE biz_flow_def SET status = 'PUBLISHED', deleted_at = NULL \
              WHERE flow_code = 'payable_pay'",
        )
        .await;
        // P0-FIX 内控：`payable_pay` 的审批人来源是 `ADMIN`（= 全部超管），而内控规定
        // 「制单人不可自审」。夹具库里只有一个超管（admin）——若让 admin 自己提交付款，
        // 他就是**唯一**审批人、又被内控排除 → 节点按「空审批人」自动通过，
        // 「配了流程必须走审批」就断言不到了（生产有 4 个超管，不会出现这种形态）。
        // 所以付款的**提交人**用一张普通账号（财务经办），审批人仍是超管 —— 与生产同形。
        exec(&format!(
            "INSERT INTO sys_user (id, username, password, real_name, status) \
             VALUES ('{D5_OPERATOR}'::uuid, 'd5.operator', 'x', 'D5 财务经办', 1) \
             ON CONFLICT DO NOTHING"
        ))
        .await;
        Some(guard)
    }

    /// D5 付款的提交人（普通账号：不是超管 ⇒ 不进 `ADMIN` 审批人列表 ⇒ 不触发内控排除）。
    fn d5_operator() -> Uuid {
        Uuid::parse_str(D5_OPERATOR).expect("D5_OPERATOR 不是合法 UUID")
    }

    /// 造一张 D5 夹具应付单（`paid` 直接落库，只为构造状态；不走付款路径）。
    async fn new_d5_payable(amount: Decimal, paid: Decimal, status: &str) -> Uuid {
        let simple = Uuid::now_v7().simple().to_string();
        let suffix = &simple[simple.len() - 8..];
        let no = format!("{D5_PREFIX}{suffix}");
        let source_no = format!("IN-D5-{suffix}");
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
                    Value::Text(source_no),
                    Value::Text(amount.to_string()),
                    Value::Text(paid.to_string()),
                    Value::Text(unpaid.to_string()),
                    Value::Text(due.to_string()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .expect("造 D5 应付单失败");
        row.expect("INSERT 没有返回 id")
            .get_typed("id")
            .expect("读取 D5 应付单 id 失败")
    }

    /// D5 用例的操作人：**库里真实存在的 admin**（`engine::start` 要查发起人档案）。
    async fn admin_uuid() -> Uuid {
        let text: String = db()
            .scalar_with_params(
                "SELECT id::text FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL LIMIT 1",
                vec![],
            )
            .await
            .expect("查询 admin 账号失败");
        Uuid::parse_str(&text).expect("admin id 不是合法 UUID")
    }

    /// D5 的付款入参（付款日期固定 2026-10，避免「今天」不在已开期间）。
    fn d5_pay_req(amount: &str) -> PayPayableReq {
        PayPayableReq {
            amount: Some(dec(amount)),
            pay_date: Some(NaiveDate::from_ymd_opt(2026, 10, 29).expect("固定日期")),
            pay_method: Some("银行转账".to_string()),
            remark: Some("D5 单测：从应付单发起付款".to_string()),
        }
    }

    /// 纯函数用例用的假行（不碰库）。
    fn fake_row(status: &str, amount: Decimal, paid: Decimal) -> payable_query::PayableRow {
        let unpaid = (amount - paid).round_dp(4);
        payable_query::PayableRow {
            id: Uuid::now_v7(),
            payable_no: "AP-D5-FAKE".to_string(),
            supplier_id: Uuid::nil(),
            supplier_name: "D5 假供应商".to_string(),
            source_type: "STOCK_IN".to_string(),
            source_id: Uuid::nil(),
            source_no: "IN-D5-FAKE".to_string(),
            invoice_no: None,
            invoice_date: None,
            currency_code: "CNY".to_string(),
            amount,
            paid_amount: paid,
            unpaid_amount: unpaid,
            credit_days: 30,
            due_date: NaiveDate::from_ymd_opt(2026, 11, 30).expect("固定日期"),
            period_code: "2026-10".to_string(),
            status: status.to_string(),
            settled_at: None,
            voucher_id: None,
            remark: None,
            cancel_instance_id: None,
            cancel_reason: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ---------- ⓪ 纯函数：判定表（不需要 ERP_DB_URL，永远真跑）----------

    /// ⓪ `payment_capability` 判定表：OPEN / PARTIAL_PAID 可付；SETTLED / CANCELLED / 审批中不可付。
    #[test]
    fn d5_capability_table_without_db() {
        let open = fake_row(payable::STATUS_OPEN, dec("226"), Decimal::ZERO);
        assert_eq!(payment_capability(&open, None, None), (true, None));

        let partial = fake_row(payable::STATUS_PARTIAL_PAID, dec("226"), dec("100"));
        assert_eq!(payment_capability(&partial, None, None), (true, None));

        let (can_pay, reason) =
            payment_capability(&open, Some(("PM-20261029-000001", dec("30"))), None);
        assert!(!can_pay, "审批中不能再发起第二笔");
        assert!(
            reason.unwrap_or_default().contains("审批中"),
            "⑤ 的文案要点明「审批中」"
        );

        let settled = fake_row(payable::STATUS_SETTLED, dec("226"), dec("226"));
        let (can_pay, reason) = payment_capability(&settled, None, None);
        assert!(!can_pay);
        assert!(reason.unwrap_or_default().contains("已结清"));

        let cancelled = fake_row(payable::STATUS_CANCELLED, dec("226"), Decimal::ZERO);
        let (can_pay, reason) = payment_capability(&cancelled, None, None);
        assert!(!can_pay);
        assert!(reason.unwrap_or_default().contains("已作废"));
    }

    /// ⓪② `PENDING_CANCEL`（作废审批中）**不能付款**，且 `blocked_reason` 必须带审批单号。
    ///
    /// 这条是 P-FLOW-3B 的补漏用例：补之前 `PENDING_CANCEL` 会落到最后的 `(true, None)`，
    /// `GET /payable/{id}/payment-info` 对作废审批中的单给 `can_pay: true` + 未付金额，
    /// 其它消费方（报表 / 批量 / 移动端）没有前端那道闸，真点下去被 409。
    #[test]
    fn pflow3b_pending_cancel_blocks_payment_with_instance_no() {
        // 作废审批中的单：账上还没作废，未付金额仍是原值 —— 正是「按金额判可付」会判错的那类
        let row = fake_row(payable::STATUS_PENDING_CANCEL, dec("800"), Decimal::ZERO);
        assert!(row.unpaid_amount > Decimal::ZERO, "前置：未付金额不为 0");

        let (can_pay, reason) = payment_capability(&row, None, Some("FLW-20261102-000001"));
        assert!(!can_pay, "作废审批中一律不能付款");
        let reason = reason.unwrap_or_default();
        assert!(
            reason.contains("正在作废审批中"),
            "文案要点明作废审批中：{reason}"
        );
        assert!(
            reason.contains("FLW-20261102-000001"),
            "文案必须带审批单号（否则用户不知道去哪撤回）：{reason}"
        );
        assert!(
            reason.contains(&row.payable_no),
            "文案必须带应付单号：{reason}"
        );
        assert!(
            reason.contains("撤回"),
            "文案要给出出路（撤回后的再次付款）：{reason}"
        );

        // 拿不到审批单号时文案仍要成立，不能 panic、也不能漏成「可付」
        let (can_pay, reason) = payment_capability(&row, None, None);
        assert!(!can_pay);
        assert!(
            reason.unwrap_or_default().contains("正在作废审批中"),
            "缺单号时也要拦住"
        );

        // 与「已有付款在审批中」同时成立时，作废审批的文案优先（② 在 ⑤ 之前）
        let (can_pay, reason) =
            payment_capability(&row, Some(("PM-20261102-000002", dec("10"))), Some("FLW-X"));
        assert!(!can_pay);
        assert!(reason.unwrap_or_default().contains("正在作废审批中"));
    }

    /// ⓪② 作废审批中的单 `default_amount` 必须是 `0`（不能给未付金额，否则前端预填就能付）。
    #[test]
    fn pflow3b_pending_cancel_default_amount_is_zero() {
        let row = fake_row(payable::STATUS_PENDING_CANCEL, dec("800"), Decimal::ZERO);
        let today = NaiveDate::from_ymd_opt(2026, 11, 2).expect("固定日期");
        let info = PaymentInfoPayable::from_row(&row, None, today, Some("FLW-20261102-000001"));
        assert!(!info.can_pay);
        assert_eq!(info.default_amount, Decimal::ZERO);
        assert_eq!(
            info.unpaid_amount,
            dec("800"),
            "未付金额照实下发，只拦「可付」"
        );
        assert!(
            info.blocked_reason
                .unwrap_or_default()
                .contains("FLW-20261102-000001")
        );
    }

    /// ⓪ `default_amount` 取**未付**（卡面 F2 / O1），不是应付总额。
    #[test]
    fn d5_default_amount_is_unpaid_not_total() {
        let row = fake_row(payable::STATUS_PARTIAL_PAID, dec("500"), dec("200"));
        let today = NaiveDate::from_ymd_opt(2026, 11, 1).expect("固定日期");
        let info = PaymentInfoPayable::from_row(&row, None, today, None);
        assert_eq!(info.amount, dec("500"));
        assert_eq!(info.unpaid_amount, dec("300"));
        assert_eq!(
            info.default_amount,
            dec("300"),
            "默认付款金额必须等于未付 300，不是应付总额 500"
        );
        assert_ne!(info.default_amount, info.amount);
        assert!(info.can_pay);
        assert_eq!(info.blocked_reason, None);
    }

    /// ⓪ 超付风险只在「审批中金额 > 当前未付」时出现（卡面 ③ 反向）。
    #[test]
    fn d5_overpay_risk_only_when_pending_exceeds_unpaid() {
        let ok = fake_row(payable::STATUS_OPEN, dec("500"), Decimal::ZERO);
        assert_eq!(overpay_risk_of(&ok, Some(("PM-1", dec("400")))), None);

        let shrunk = fake_row(payable::STATUS_PARTIAL_PAID, dec("500"), dec("450"));
        let risk =
            overpay_risk_of(&shrunk, Some(("PM-2", dec("400")))).expect("400 > 未付 50 必须有提示");
        assert!(risk.contains("PM-2"), "{risk}");
        assert!(risk.contains("超过当前未付余额 50"), "{risk}");
    }

    // ---------- ① 应付 OPEN → can_pay=true，unpaid = amount ----------

    /// ① 全新 OPEN 应付：`can_pay = true`、`unpaid = amount`、默认金额 = 未付、已付记录为空。
    #[tokio::test]
    async fn d5_open_payable_can_pay_with_full_unpaid() {
        let _db = match d5_setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_d5_payable(dec("226"), Decimal::ZERO, payable::STATUS_OPEN).await;
        let info = payment_info(id).await.expect("查可付信息失败");
        let info = info.payable;

        assert!(info.can_pay, "OPEN 必须能付");
        assert_eq!(info.blocked_reason, None);
        assert_eq!(info.status, payable::STATUS_OPEN);
        assert_eq!(info.status_label, "未付");
        assert_eq!(info.amount, dec("226"));
        assert_eq!(info.paid_amount, Decimal::ZERO);
        assert_eq!(info.unpaid_amount, dec("226"));
        assert_eq!(info.default_amount, info.unpaid_amount, "O1：默认 = 未付");
        assert_eq!(info.supplier_name, "P7 付款测试供应商");
        assert!(info.source_no.starts_with("IN-D5-"), "{}", info.source_no);
        assert_eq!(info.pending_payment_no, None);
        assert_eq!(info.overpay_risk, None);

        let payments = payment_info(id).await.expect("查可付信息失败").payments;
        assert!(payments.is_empty(), "还没付过款，付款记录必须为空");
    }

    // ---------- ② 部分已付 → unpaid = amount - paid ----------

    /// ② 部分已付：`unpaid = amount - paid`、`can_pay` 仍为 true、默认金额 = 剩余未付。
    #[tokio::test]
    async fn d5_partial_paid_unpaid_is_amount_minus_paid() {
        let _db = match d5_setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_d5_payable(dec("226"), dec("100"), payable::STATUS_PARTIAL_PAID).await;
        let info = payment_info(id).await.expect("查可付信息失败").payable;

        assert!(info.can_pay, "部分已付还能继续付");
        assert_eq!(info.status, payable::STATUS_PARTIAL_PAID);
        assert_eq!(info.status_label, "部分已付");
        assert_eq!(info.paid_amount, dec("100"));
        assert_eq!(info.unpaid_amount, dec("126"));
        assert_eq!(info.default_amount, dec("126"), "F2：带出未付，不是 226");
    }

    // ---------- ③ 已结清 SETTLED → can_pay=false + blocked_reason ----------

    /// ③ 已结清：`can_pay = false` + `blocked_reason` 说明、默认金额归 0（前端不显示按钮）。
    #[tokio::test]
    async fn d5_settled_payable_is_blocked_with_reason() {
        let _db = match d5_setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_d5_payable(dec("226"), dec("226"), payable::STATUS_SETTLED).await;
        let info = payment_info(id).await.expect("查可付信息失败").payable;

        assert!(!info.can_pay, "L9：已结清不显示按钮");
        let reason = info.blocked_reason.expect("必须给不能付的原因");
        assert!(reason.contains("已结清"), "{reason}");
        assert_eq!(info.unpaid_amount, Decimal::ZERO);
        assert_eq!(
            info.default_amount,
            Decimal::ZERO,
            "不能付时默认金额必须是 0"
        );
    }

    // ---------- ④ 已作废 CANCELLED → can_pay=false ----------

    /// ④ 已作废：`can_pay = false` + 原因点名「已作废」。
    #[tokio::test]
    async fn d5_cancelled_payable_is_blocked_with_reason() {
        let _db = match d5_setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_d5_payable(dec("226"), Decimal::ZERO, payable::STATUS_CANCELLED).await;
        let info = payment_info(id).await.expect("查可付信息失败").payable;

        assert!(!info.can_pay);
        let reason = info.blocked_reason.expect("必须给不能付的原因");
        assert!(reason.contains("已作废"), "{reason}");
        assert_eq!(info.default_amount, Decimal::ZERO);
    }

    // ---------- ⑤ 从应付发起付款 → 付款单 payable_id 回填 ----------

    /// ⑤ 从应付单发起付款（走 P17 审批）：付款记录回填 `payable_id`、`approval_status = PENDING`，
    /// 且**账一分没动**（`paid_amount` 仍为 0）。
    #[tokio::test]
    async fn d5_pay_from_payable_backfills_payable_id() {
        let _db = match d5_setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 提交人用普通账号（内控：超管自己提交会把自己排除掉，见 d5_setup 说明）
        let operator = d5_operator();
        let id = new_d5_payable(dec("226"), Decimal::ZERO, payable::STATUS_OPEN).await;

        let res = submit_payment(id, &d5_pay_req("226"), Some(operator))
            .await
            .unwrap_or_else(|err| panic!("从应付单发起付款失败：{} {}", err.code, err.msg));
        assert_eq!(res.outcome, OUTCOME_PENDING, "P17：配了流程必须走审批");
        assert_eq!(res.amount, dec("226"));

        let rows = payments_of(db(), id).await.expect("查付款记录失败");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].payable_id, id, "付款单必须回填 payable_id");
        assert_eq!(rows[0].amount, dec("226"));
        assert_eq!(rows[0].approval_status, APPROVAL_PENDING);
        assert_eq!(
            res.payment_no.as_deref(),
            Some(rows[0].payment_no.as_str()),
            "接口返回的付款单号必须与库里的那一条一致"
        );

        let row = row_of(id).await;
        assert_eq!(row.paid_amount, Decimal::ZERO, "审批中账不能动");
        assert_eq!(row.status, payable::STATUS_OPEN);
        assert_eq!(row.unpaid_amount, dec("226"));
    }

    // ---------- ⑥ 付款金额默认 = unpaid ----------

    /// ⑥ 默认付款金额跟着未付走：付掉 100 以后 `payment_info` 的 `default_amount` 变 126，
    /// 前端不需要自己算（也不再带出应付总额 226）。
    #[tokio::test]
    async fn d5_default_amount_follows_unpaid_after_payment() {
        let _db = match d5_setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_d5_payable(dec("226"), Decimal::ZERO, payable::STATUS_OPEN).await;
        assert_eq!(
            payment_info(id).await.unwrap().payable.default_amount,
            dec("226")
        );

        // 直付路径（未配流程时的降级行为），把「已付 100」落成真账
        pay_payable(id, &d5_pay_req("100"), None)
            .await
            .unwrap_or_else(|err| panic!("直付失败：{} {}", err.code, err.msg));

        let info = payment_info(id).await.unwrap().payable;
        assert_eq!(info.unpaid_amount, dec("126"));
        assert_eq!(info.default_amount, dec("126"), "第二次发起要预填剩余 126");
        assert!(info.can_pay);
        let payments = payment_info(id).await.unwrap().payments;
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].approval_status, APPROVAL_NONE);
        assert_eq!(payments[0].approval_status_label, "已生效");
    }

    // ---------- ⑦ 超付 → 400 + 精确文案 ----------

    /// ⑦ 超付：`本次 126.01 > 未付 126` → 400，文案点名两个金额；单子与付款记录都不动。
    #[tokio::test]
    async fn d5_overpay_is_rejected_with_exact_message() {
        let _db = match d5_setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_d5_payable(dec("226"), dec("100"), payable::STATUS_PARTIAL_PAID).await;

        let err = submit_payment(id, &d5_pay_req("126.01"), Some(d5_operator()))
            .await
            .expect_err("超付必须被拒");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(
            err.msg.contains("本次付款 126.01 超过未付余额 126.0000"),
            "文案要点名「本次 X 超过未付余额 Y」：{}",
            err.msg
        );
        // 失败不留痕：没有付款记录，余额原样
        assert!(payments_of(db(), id).await.unwrap().is_empty());
        let row = row_of(id).await;
        assert_eq!(row.paid_amount, dec("100"));
        assert_eq!(row.unpaid_amount, dec("126"));
        // 也不能把「刚好等于未付」算成超付
        assert!(payment_info(id).await.unwrap().payable.can_pay);
    }

    // ---------- ⑧ 应付作废 → 撤未完成付款审批 ----------

    /// ⑧ 付款审批中把应付单作废：付款记录翻 `CANCELLED`、审批实例终止、可付信息变成「已作废」。
    #[tokio::test]
    async fn d5_cancel_payable_revokes_pending_payment() {
        let _db = match d5_setup().await {
            Some(guard) => guard,
            None => return,
        };
        let admin = admin_uuid().await;
        let id = new_d5_payable(dec("226"), Decimal::ZERO, payable::STATUS_OPEN).await;

        let res = submit_payment(id, &d5_pay_req("226"), Some(d5_operator()))
            .await
            .expect("审批中不能算失败");
        assert_eq!(res.outcome, OUTCOME_PENDING);
        let payment = payments_of(db(), id).await.unwrap().remove(0);
        let instance_id = payment.approval_instance_id.expect("审批中必须带实例 id");

        let detail = cancel_payable(id, Some("供应商退货，应付单作废"), Some(admin))
            .await
            .unwrap_or_else(|err| panic!("作废失败：{} {}", err.code, err.msg));
        assert_eq!(detail.payable.status, payable::STATUS_CANCELLED);

        let row = payments_of(db(), id).await.unwrap().remove(0);
        assert_eq!(
            row.approval_status, APPROVAL_CANCELLED,
            "未完成的付款审批必须被撤（标 CANCELLED）"
        );
        let instance_status: String = db()
            .scalar_with_params(
                "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("查审批实例失败");
        assert_eq!(instance_status, "CANCELLED", "审批实例必须终止");

        let info = payment_info(id).await.unwrap().payable;
        assert!(!info.can_pay);
        assert!(info.blocked_reason.unwrap_or_default().contains("已作废"));
        let payments = payment_info(id).await.unwrap().payments;
        assert_eq!(payments[0].approval_status_label, "已终止");
        assert_eq!(row_of(id).await.paid_amount, Decimal::ZERO, "账一分没动");
    }

    // ---------- ⑨ 审批中不能再发起第二笔 ----------

    /// ⑨ 审批中：`can_pay = false` + 原因点名「审批中」，并给出那笔的单号与金额；
    /// 真的再提交一次仍是 409（后端硬闸，不靠前端置灰）。
    #[tokio::test]
    async fn d5_pending_payment_blocks_second_submit() {
        let _db = match d5_setup().await {
            Some(guard) => guard,
            None => return,
        };
        let operator = d5_operator();
        let id = new_d5_payable(dec("500"), Decimal::ZERO, payable::STATUS_OPEN).await;
        let first = submit_payment(id, &d5_pay_req("100"), Some(operator))
            .await
            .unwrap();
        let first_no = first.payment_no.clone().expect("审批中必须有付款单号");

        let info = payment_info(id).await.unwrap().payable;
        assert!(!info.can_pay);
        let reason = info.blocked_reason.unwrap_or_default();
        assert!(
            reason.contains("审批中") && reason.contains(&first_no),
            "{reason}"
        );
        assert_eq!(info.pending_payment_no.as_deref(), Some(first_no.as_str()));
        assert_eq!(info.pending_amount, Some(dec("100")));
        assert_eq!(info.overpay_risk, None, "100 <= 未付 500，不该报超付风险");

        let err = submit_payment(id, &d5_pay_req("100"), Some(operator))
            .await
            .expect_err("审批中重复提交必须被拒");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("审批中"), "{}", err.msg);
    }

    // ---------- ⑩ 审批中应付单被别处付掉 → 超付提示 ----------

    /// ⑩ 审批期间未付余额被外部改小（别处付掉）：`overpay_risk` 明确提示「审批中那笔会超付」，
    /// 但**不阻断**展示（这条只是提示，真正的拦截在执行时）。
    #[tokio::test]
    async fn d5_overpay_risk_when_unpaid_shrinks_during_approval() {
        let _db = match d5_setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = new_d5_payable(dec("500"), Decimal::ZERO, payable::STATUS_OPEN).await;
        submit_payment(id, &d5_pay_req("400"), Some(d5_operator()))
            .await
            .unwrap();

        // 模拟「另一笔付款把它付掉 450」：未付从 500 掉到 50，审批中的 400 已经超付
        exec(&format!(
            "UPDATE biz_payable SET paid_amount = NULLIF('450','')::numeric, \
                                    unpaid_amount = NULLIF('50','')::numeric, status = 'PARTIAL_PAID' \
              WHERE id = '{id}'::uuid"
        ))
        .await;

        let info = payment_info(id).await.unwrap().payable;
        assert_eq!(info.unpaid_amount, dec("50"));
        let risk = info.overpay_risk.expect("审批中 400 > 未付 50 必须提示");
        assert!(risk.contains("超过当前未付余额 50"), "{risk}");
        assert!(
            risk.contains(&info.pending_payment_no.unwrap_or_default()),
            "提示里要点名是哪一笔：{risk}"
        );
    }

    // ---------- ⑪ 不存在的应付单 → 404 ----------

    /// ⑪ 应付单不存在 / 已删：`404 + 10002`（不静默返回空壳）。
    #[tokio::test]
    async fn d5_missing_payable_is_not_found() {
        let _db = match d5_setup().await {
            Some(guard) => guard,
            None => return,
        };
        let err = payment_info(Uuid::now_v7())
            .await
            .expect_err("不存在的应付单必须 404");
        assert_eq!(err.status, 404, "{}", err.msg);
        assert_eq!(err.code, error_code::RESOURCE_NOT_FOUND);
    }
}
