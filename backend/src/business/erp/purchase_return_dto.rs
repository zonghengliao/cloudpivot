//! 采购退货契约（P2-03）：建单 / 分页 / 提交审批 / 确认退货。
//!
//! 与 [`crate::business::erp::purchase_requisition_dto`] / [`crate::business::erp::purchase_dto`]
//! 同一套约定：主键与外键一律 `String`（`nik::uuid::Uuid` 没开 serde feature）、
//! 日期 `NaiveDate`（`YYYY-MM-DD`）、金额/数量 `Decimal`（**JSON 里是字符串**）、
//! 状态一律大写字符串且与 DDL 的 `CHECK` 逐字一致。
//!
//! # 四条硬规则（全部有纯函数 + 单测钉住，handler 只负责取数后调它们）
//!
//! ```text
//! a) 可退数量 ≤ 入库数量 − 已退数量        → check_returnable（防超退）
//! b) 退货单价 = 原入库单价（不许改）        → check_price_locked（防价格操纵）
//! c) 金额倒挤：with_tax = qty × price_incl；tax = with_tax − amount
//!    ⇒ 库里 `with_tax_amount = amount + tax_amount` 精确成立（V0332 的 CHECK）
//! d) 冲减应付：只冲「未付部分」，已付部分转供应商应退款
//!    → payable_after（amount 永不小于 paid_amount，库层 ck_biz_payable_paid 是物理下限）
//! ```
//!
//! # 权限码（与 `V0332__purchase_return.up.sql` 逐字一致）
//!
//! `business:erp:purchase-return:{list,create,submit,confirm}`。

use nik::prelude::chrono::{Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;

// ==================== 域常量 ====================

/// 草稿：建单，还没提交审批（可以改）。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 审批中：已发起审批单，等审批结论。
pub const STATUS_SUBMITTED: &str = "SUBMITTED";
/// 已批准：审批通过，可以确认退货（出库 + 冲应付 + 凭证）。
pub const STATUS_APPROVED: &str = "APPROVED";
/// 已确认：账已动过（出库单 / 应付冲减 / 凭证 全部落库），终态。
pub const STATUS_CONFIRMED: &str = "CONFIRMED";
/// 已作废：终态（作废不冲账 —— 只有没确认过的单据才能作废）。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 全部状态（与 `ck_biz_prt_status` 逐字一致）。
pub const STATUSES: [&str; 5] = [
    STATUS_DRAFT,
    STATUS_SUBMITTED,
    STATUS_APPROVED,
    STATUS_CONFIRMED,
    STATUS_CANCELLED,
];

/// 明细行上限（一次提交，防超大数组）。
pub const MAX_LINES: usize = 200;

/// 退货原因长度上限（`reason VARCHAR(255)`）。
pub const MAX_REASON_LEN: usize = 255;

/// 备注长度上限。
pub const MAX_REMARK_LEN: usize = 500;

/// 四个权限码（与 V0332 的 `sys_menu.auth_code` 逐字一致）。
pub const RETURN_PERMISSIONS: [&str; 4] = [
    "business:erp:purchase-return:list",
    "business:erp:purchase-return:create",
    "business:erp:purchase-return:submit",
    "business:erp:purchase-return:confirm",
];

// ==================== 错误出口 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 域值（状态 / 方向）乱码一律 400，不静默忽略。
fn normalize_status(raw: &str) -> std::result::Result<String, ApiError> {
    let upper = raw.trim().to_ascii_uppercase();
    if STATUSES.contains(&upper.as_str()) {
        Ok(upper)
    } else {
        Err(bad_request(format!(
            "status 只能是 {} 之一（收到「{raw}」）",
            STATUSES.join(" / ")
        )))
    }
}

// ==================== 纯函数 ====================

/// 今天的 CST 日期（退货日期缺省值）。
pub fn today_cst() -> NaiveDate {
    (Utc::now() + Duration::hours(8)).date_naive()
}

/// 状态中文名（错误文案 / 列表展示共用，与 `state_machine` 的 status_labels 同源）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SUBMITTED => "审批中",
        STATUS_APPROVED => "已批准",
        STATUS_CONFIRMED => "已确认",
        STATUS_CANCELLED => "已作废",
        _ => "未知状态",
    }
}

/// 一行退货的金额三连（**税额倒挤**，保证 `with_tax = amount + tax` 精确成立）。
///
/// ```text
/// amount   = round(qty × price, 4)        ← 不含税（入库成本口径）
/// with_tax = round(qty × price_incl, 4)   ← 价税合计（应付口径）
/// tax      = with_tax − amount            ← 倒挤（PRD §3.1.3）
/// ```
///
/// `price_incl` 缺失（老数据 0）时按 `price × (1 + tax_rate/100)` 现算 —— 这是
/// **唯一**允许的兜底，且只发生在来源入库明细自己就没存含税价的情况下。
pub fn line_amounts(
    qty: Decimal,
    price: Decimal,
    price_incl: Decimal,
    tax_rate: Decimal,
) -> (Decimal, Decimal, Decimal) {
    let amount = (qty * price).round_dp(4);
    let incl = if price_incl > Decimal::ZERO {
        price_incl
    } else {
        price * (Decimal::ONE + tax_rate / Decimal::from(100))
    };
    let with_tax = (qty * incl).round_dp(4);
    // 兜底：含税价低于不含税价（脏数据）时不允许出现负税额（库层 CHECK 也不允许）
    let with_tax = if with_tax < amount { amount } else { with_tax };
    (amount, with_tax - amount, with_tax)
}

/// 可退数量 = 入库数量 − 已退数量（不含本单）。
///
/// `already_returned` 由调用方用 SQL 汇总（**排除本单自己**，见 model 的 `returned_qty_of`）。
pub fn returnable_qty(received: Decimal, already_returned: Decimal) -> Decimal {
    let left = received - already_returned;
    if left < Decimal::ZERO {
        Decimal::ZERO
    } else {
        left
    }
}

/// **防超退**（卡面 §④ a，本卡的核心守卫）。
///
/// 判据：`requested ≤ received − returned`。相等允许（退满最后一件是正常业务），
/// 超过一律 [`error_code::ERP_PURCHASE_RETURN_OVER_RETURN`]（HTTP 400，报文里点明可退多少）。
pub fn check_returnable(
    requested: Decimal,
    received: Decimal,
    already_returned: Decimal,
    label: &str,
) -> std::result::Result<(), ApiError> {
    if requested <= Decimal::ZERO {
        return Err(bad_request(format!("{label} 的退货数量必须 > 0")));
    }
    let left = returnable_qty(received, already_returned);
    if requested > left {
        return Err(ApiError::new(
            400,
            error_code::ERP_PURCHASE_RETURN_OVER_RETURN,
            format!(
                "超退拦截：{label} 原入库 {received}，已退 {already_returned}，可退只剩 {left}，\
                 本次要退 {requested}。请把数量改到 {left} 以内（或换一张入库单）"
            ),
        ));
    }
    Ok(())
}

/// **价格锁死**（卡面 §④ b）：退货单价必须等于原入库单价。
///
/// 调用方（前端）传的 `unit_price` 只用于这条校验，服务端**从不**用它算金额 ——
/// 金额一律取入库明细快照。不传（`None`）= 不做校验（正常前端不传这个字段）。
pub fn check_price_locked(
    claimed: Option<Decimal>,
    price: Decimal,
    price_incl: Decimal,
    label: &str,
) -> std::result::Result<(), ApiError> {
    let Some(claimed) = claimed else {
        return Ok(());
    };
    let hit_excl = claimed.round_dp(6) == price.round_dp(6);
    let hit_incl = claimed.round_dp(6) == price_incl.round_dp(6);
    if hit_excl || hit_incl {
        return Ok(());
    }
    Err(ApiError::new(
        400,
        error_code::ERP_PURCHASE_RETURN_PRICE_LOCKED,
        format!(
            "退货单价不许改：{label} 的原入库单价是不含税 {price} / 含税 {price_incl}，\
             请求里给的是 {claimed}。退货金额一律按原入库单价计算（防价格操纵），\
             要改价请走采购价格调整 / 重新入库，不要在退货单上改"
        ),
    ))
}

/// 应付冲减口径（卡面 §④ c ③④ / d）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayableAfter {
    /// 冲减后的应付金额（`amount`，永不小于 `paid_amount`）。
    pub amount: Decimal,
    /// 冲减后的未付金额（`= amount − paid_amount`）。
    pub unpaid: Decimal,
    /// 冲减后的状态（`SETTLED` / `PARTIAL_PAID` / `OPEN`）。
    pub status: &'static str,
    /// 已付款部分转成的**供应商应退款**（> 0 才需要建 `biz_supplier_receivable`）。
    pub to_supplier_receivable: Decimal,
}

/// 按「退货含税金额」冲减应付（纯函数，单测钉死四种场景）。
///
/// # 口径（为什么只冲未付部分）
///
/// ```text
/// unpaid_part = min(退货含税金额, 应付未付金额)
/// 应付：amount -= unpaid_part，paid_amount 不变 ⇒ unpaid = amount − paid
/// 已付部分（退货含税金额 − unpaid_part）→ 供应商应退款
/// ```
///
/// 理由：`ck_biz_payable_paid CHECK (paid_amount <= amount)` 是物理下限，
/// 把 amount 减到 paid 以下插不进库；而且「钱已经付出去了」这件事在会计上
/// 不因为你退货就消失 —— 它变成了「供应商欠我们」，必须单独挂账（否则账外债权）。
pub fn payable_after(amount: Decimal, paid: Decimal, return_with_tax: Decimal) -> PayableAfter {
    let unpaid = (amount - paid).max(Decimal::ZERO);
    let unpaid_part = return_with_tax.min(unpaid);
    let to_supplier_receivable = (return_with_tax - unpaid_part).max(Decimal::ZERO);
    let new_amount = (amount - unpaid_part).max(Decimal::ZERO).round_dp(4);
    let new_paid = paid.min(new_amount);
    let new_unpaid = (new_amount - new_paid).round_dp(4);
    let status = if new_unpaid <= Decimal::ZERO {
        STATUS_SETTLED
    } else if new_paid > Decimal::ZERO {
        STATUS_PARTIAL_PAID
    } else {
        STATUS_OPEN
    };
    PayableAfter {
        amount: new_amount,
        unpaid: new_unpaid,
        status,
        to_supplier_receivable: to_supplier_receivable.round_dp(4),
    }
}

/// 应付状态常量（与 `payable.rs` 同值，这里独立声明以免 dto 反向依赖 model）。
pub const STATUS_OPEN: &str = "OPEN";
/// 应付部分已付。
pub const STATUS_PARTIAL_PAID: &str = "PARTIAL_PAID";
/// 应付已结清。
pub const STATUS_SETTLED: &str = "SETTLED";

/// 单头金额三连 = 明细求和。
pub fn totals_of(lines: &[(Decimal, Decimal, Decimal)]) -> (Decimal, Decimal, Decimal) {
    let mut amount = Decimal::ZERO;
    let mut tax = Decimal::ZERO;
    let mut with_tax = Decimal::ZERO;
    for (line_amount, line_tax, line_with_tax) in lines {
        amount += *line_amount;
        tax += *line_tax;
        with_tax += *line_with_tax;
    }
    (amount.round_dp(4), tax.round_dp(4), with_tax.round_dp(4))
}

/// 退货原因校验（必填，≤ 255 字）。
pub fn check_reason(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Err(bad_request(
            "reason（退货原因）必填：质量 / 短交 / 错发 / 价格 —— 供应商对账与审计都要能回答「为什么退」",
        ));
    };
    let count = text.chars().count();
    if count > MAX_REASON_LEN {
        return Err(bad_request(format!(
            "退货原因最多 {MAX_REASON_LEN} 字（收到 {count} 字）"
        )));
    }
    Ok(text.to_string())
}

/// 备注（可选，去空白，≤ 500 字）。
pub fn clean_remark(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let count = text.chars().count();
    if count > MAX_REMARK_LEN {
        return Err(bad_request(format!(
            "备注最多 {MAX_REMARK_LEN} 字（收到 {count} 字）"
        )));
    }
    Ok(Some(text.to_string()))
}

/// 数量校验（> 0，最多 4 位小数）。
pub fn check_qty(raw: Decimal, field: &str) -> std::result::Result<Decimal, ApiError> {
    let qty = raw.round_dp(4);
    if qty <= Decimal::ZERO {
        return Err(bad_request(format!("{field} 必须 > 0（收到 {raw}）")));
    }
    Ok(qty)
}

/// UUID 字段（必填）。
pub fn parse_uuid_field(raw: Option<&str>, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空（需要 UUID）")));
    }
    Uuid::parse_str(text)
        .map_err(|_| bad_request(format!("{field} 不是合法的 UUID（收到 {text}）")))
}

/// UUID 字段（可选；空串 = 没传）。
pub fn parse_optional_uuid(
    raw: Option<&str>,
    field: &str,
) -> std::result::Result<Option<Uuid>, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Ok(None);
    }
    Uuid::parse_str(text)
        .map(Some)
        .map_err(|_| bad_request(format!("{field} 不是合法的 UUID（收到 {text}）")))
}

// ==================== 入参 ====================

/// 新建退货单。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateReturnReq {
    /// 来源入库单（必填；可退明细从它的明细带出）。
    pub stock_in_id: String,
    /// 退货日期（缺省 = 今天 CST；决定会计期间）。
    pub return_date: Option<NaiveDate>,
    /// 退货原因（必填）。
    pub reason: Option<String>,
    /// 备注（可选）。
    pub remark: Option<String>,
    /// 明细（至少 1 行）。
    #[serde(default)]
    pub items: Vec<CreateReturnItemReq>,
}

/// 建单明细行。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateReturnItemReq {
    /// 来源入库明细（必填；单价 / 税率 / 库位全部取它的快照）。
    pub stock_in_item_id: String,
    /// 退货数量。
    pub qty: Decimal,
    /// 前端声称的单价（**只用于校验**：与入库快照不一致即 400，见 [`check_price_locked`]）。
    pub unit_price: Option<Decimal>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 提交审批。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SubmitReturnReq {
    /// 说明（可选，≤ 200 字）。
    pub reason: Option<String>,
    /// 显式跳过审批的请求片段（没有已发布流程时才可用，需要权限码 + ≥10 字理由）。
    #[serde(flatten)]
    pub skip: crate::business::erp::approval_gate::SkipApprovalReq,
}

/// 确认退货。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ConfirmReturnReq {
    /// 说明（可选；落 `remark` 追加，同时进凭证摘要的审计线索）。
    pub reason: Option<String>,
}

/// 作废。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CancelReturnReq {
    /// 作废原因（提交后 / 批准后作废必填）。
    pub reason: Option<String>,
}

/// 分页查询。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ReturnPageQuery {
    /// 页码（缺省 1）。
    pub page: Option<u64>,
    /// 每页条数（缺省 20，上限 200）。
    pub page_size: Option<u64>,
    /// 状态精确过滤。
    pub status: Option<String>,
    /// 关键字：退货单号 / 入库单号 / 供应商 / 原因 / 备注。
    pub keyword: Option<String>,
    /// 退货日期起（含）。
    pub date_from: Option<NaiveDate>,
    /// 退货日期止（含）。
    pub date_to: Option<NaiveDate>,
    /// 按来源入库单过滤。
    pub stock_in_id: Option<String>,
}

/// 数据层筛选条件（空白串 = 没传）。
#[derive(Debug, Clone, Default)]
pub struct ReturnFilter {
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 状态。
    pub status: Option<String>,
    /// 关键字。
    pub keyword: Option<String>,
    /// 日期起。
    pub date_from: Option<NaiveDate>,
    /// 日期止。
    pub date_to: Option<NaiveDate>,
    /// 来源入库单。
    pub stock_in_id: Option<Uuid>,
}

impl ReturnPageQuery {
    /// 入参校验 + 归一化（域值乱码一律 400）。
    pub fn filter(&self) -> std::result::Result<ReturnFilter, ApiError> {
        let status = match self
            .status
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(raw) => Some(normalize_status(raw)?),
            None => None,
        };
        if let (Some(from), Some(to)) = (self.date_from, self.date_to)
            && from > to
        {
            return Err(bad_request(format!(
                "date_from（{from}）不能晚于 date_to（{to}）"
            )));
        }
        let keyword = self
            .keyword
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let stock_in_id = parse_optional_uuid(self.stock_in_id.as_deref(), "stock_in_id")?;
        Ok(ReturnFilter {
            page: self.page.unwrap_or(1).max(1),
            page_size: self.page_size.unwrap_or(20).clamp(1, 200),
            status,
            keyword,
            date_from: self.date_from,
            date_to: self.date_to,
            stock_in_id,
        })
    }
}

// ==================== 出参 ====================

/// 列表行。
#[derive(Debug, Clone, Serialize)]
pub struct ReturnRowRes {
    /// 主键。
    pub id: String,
    /// 退货单号（`PRT-yyyyMMdd-000001`）。
    pub return_no: String,
    /// 来源入库单 id。
    pub stock_in_id: String,
    /// 来源入库单号。
    pub stock_in_no: String,
    /// 来源采购订单号（可空）。
    pub purchase_order_no: Option<String>,
    /// 供应商 id / 名称。
    pub supplier_id: String,
    /// 供应商名称。
    pub supplier_name: Option<String>,
    /// 仓库 id / 名称。
    pub warehouse_id: String,
    /// 仓库名称。
    pub warehouse_name: Option<String>,
    /// 退货日期。
    pub return_date: NaiveDate,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 数量合计。
    pub total_qty: Decimal,
    /// 不含税金额合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计。
    pub total_with_tax: Decimal,
    /// 退货原因。
    pub reason: String,
    /// 审批实例 id（可空）。
    pub approval_instance_id: Option<String>,
    /// 出库单号（确认后）。
    pub stock_out_no: Option<String>,
    /// 应付单号（被冲减的）。
    pub payable_no: Option<String>,
    /// 凭证号（确认后）。
    pub voucher_no: Option<String>,
    /// 供应商应退款单号（已付款场景）。
    pub supplier_receivable_no: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 明细行。
#[derive(Debug, Clone, Serialize)]
pub struct ReturnItemRes {
    /// 主键。
    pub id: String,
    /// 行号。
    pub line_no: i32,
    /// 来源入库明细 id。
    pub stock_in_item_id: String,
    /// 物料 id / 编码 / 名称。
    pub material_id: String,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 单位 id。
    pub unit_id: String,
    /// 单位名称。
    pub unit_name: Option<String>,
    /// 退货数量。
    pub qty: Decimal,
    /// 不含税单价（原入库快照）。
    pub price: Decimal,
    /// 含税单价（原入库快照）。
    pub price_incl: Decimal,
    /// 税率（%）。
    pub tax_rate: Decimal,
    /// 不含税金额。
    pub amount: Decimal,
    /// 税额。
    pub tax_amount: Decimal,
    /// 价税合计。
    pub with_tax_amount: Decimal,
    /// 原入库数量（便于前端显示「退多少 / 入库多少」）。
    pub received_qty: Decimal,
    /// 该入库明细已退数量（含本单）。
    pub returned_qty: Decimal,
    /// 库位 id。
    pub location_id: Option<String>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 详情（单头 + 明细）。
#[derive(Debug, Clone, Serialize)]
pub struct ReturnDetailRes {
    /// 单头。
    pub purchase_return: ReturnRowRes,
    /// 明细。
    pub items: Vec<ReturnItemRes>,
}

/// 可退明细（「选入库单带明细」接口的出参）。
#[derive(Debug, Clone, Serialize)]
pub struct ReturnableItemRes {
    /// 入库明细 id。
    pub stock_in_item_id: String,
    /// 行号。
    pub line_no: i32,
    /// 物料 id / 编码 / 名称。
    pub material_id: String,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 单位 id / 名称。
    pub unit_id: String,
    /// 单位名称。
    pub unit_name: Option<String>,
    /// 原入库数量。
    pub received_qty: Decimal,
    /// 已退数量（未作废的退货单累计）。
    pub returned_qty: Decimal,
    /// 可退数量（= 入库 − 已退）。
    pub returnable_qty: Decimal,
    /// 不含税单价。
    pub price: Decimal,
    /// 含税单价。
    pub price_incl: Decimal,
    /// 税率（%）。
    pub tax_rate: Decimal,
    /// 库位 id。
    pub location_id: Option<String>,
}

/// 可退明细汇总（入库单 + 明细）。
#[derive(Debug, Clone, Serialize)]
pub struct ReturnableSourceRes {
    /// 入库单 id / 单号。
    pub stock_in_id: String,
    /// 入库单号。
    pub stock_in_no: String,
    /// 入库日期。
    pub in_date: NaiveDate,
    /// 供应商 id / 名称。
    pub supplier_id: Option<String>,
    /// 供应商名称。
    pub supplier_name: Option<String>,
    /// 仓库 id。
    pub warehouse_id: String,
    /// 采购订单 id / 号（可空）。
    pub purchase_order_id: Option<String>,
    /// 采购订单号。
    pub purchase_order_no: Option<String>,
    /// 明细。
    pub items: Vec<ReturnableItemRes>,
}

#[cfg(test)]
mod tests {
    use super::*;
    // 本文件只用少量固定小数，避免新增 dev-dependency：用 `Decimal::from_str` 收口。
    fn d(text: &str) -> Decimal {
        text.parse::<Decimal>().expect("测试常量必须是合法小数")
    }

    /// ① 金额倒挤：`with_tax = amount + tax` 精确成立（库层 CHECK 的口径）。
    #[test]
    fn line_amounts_are_tax_derived() {
        // 100 个 × 10 元不含税、13% ⇒ 含税 1130
        let (amount, tax, with_tax) = line_amounts(d("100"), d("10"), d("11.3"), d("13"));
        assert_eq!(amount, d("1000"));
        assert_eq!(with_tax, d("1130"));
        assert_eq!(tax, d("130"));
        assert_eq!(with_tax, amount + tax, "价税合计必须精确等于不含税 + 税额");

        // 30 个的部分退货（验收场景 ②）
        let (amount, tax, with_tax) = line_amounts(d("30"), d("10"), d("11.3"), d("13"));
        assert_eq!(amount, d("300"));
        assert_eq!(with_tax, d("339"));
        assert_eq!(amount + tax, with_tax);
    }

    /// ② 含税价缺失时按税率现算（老数据兜底），且不产生负税额。
    #[test]
    fn line_amounts_fall_back_to_tax_rate() {
        let (amount, tax, with_tax) = line_amounts(d("10"), d("10"), d("0"), d("13"));
        assert_eq!(amount, d("100"));
        assert_eq!(with_tax, d("113"));
        assert_eq!(tax, d("13"));

        // 脏数据：含税价低于不含税价 ⇒ 税额夹到 0，不出现负数
        let (amount, tax, with_tax) = line_amounts(d("10"), d("10"), d("8"), d("13"));
        assert_eq!(amount, d("100"));
        assert_eq!(tax, Decimal::ZERO);
        assert_eq!(with_tax, amount);
    }

    /// ③ **防超退**（卡面 §④ a，硬要求单测①）。
    #[test]
    fn over_return_is_blocked() {
        // 入库 100，已退 0：退 100 允许（退满），退 101 拒绝
        assert!(check_returnable(d("100"), d("100"), d("0"), "M1").is_ok());
        let error = check_returnable(d("101"), d("100"), d("0"), "M1").expect_err("超退必须被拦");
        assert_eq!(error.code, error_code::ERP_PURCHASE_RETURN_OVER_RETURN);
        assert!(
            error.msg.contains("可退只剩 100"),
            "报文要指出可退多少：{}",
            error.msg
        );

        // 入库 100，已退 30：可退 70 —— 退 70 允许、退 80 拒绝（验收场景 ③）
        assert_eq!(returnable_qty(d("100"), d("30")), d("70"));
        assert!(check_returnable(d("70"), d("100"), d("30"), "M1").is_ok());
        let error = check_returnable(d("80"), d("100"), d("30"), "M1").expect_err("超退必须被拦");
        assert_eq!(error.code, error_code::ERP_PURCHASE_RETURN_OVER_RETURN);
        assert!(error.msg.contains("可退只剩 70"), "{}", error.msg);

        // 已退超过入库（历史脏数据）可退夹到 0，任何数量都被拦
        assert_eq!(returnable_qty(d("10"), d("12")), Decimal::ZERO);
        assert!(check_returnable(d("1"), d("10"), d("12"), "M1").is_err());

        // 数量必须 > 0
        assert!(check_returnable(Decimal::ZERO, d("10"), d("0"), "M1").is_err());
    }

    /// ④ 价格锁死：与原入库单价（不含税 / 含税任一）相等才放行。
    #[test]
    fn price_is_locked_to_stock_in_snapshot() {
        assert!(check_price_locked(None, d("10"), d("11.3"), "M1").is_ok());
        assert!(check_price_locked(Some(d("10")), d("10"), d("11.3"), "M1").is_ok());
        assert!(check_price_locked(Some(d("11.3")), d("10"), d("11.3"), "M1").is_ok());
        let error =
            check_price_locked(Some(d("9")), d("10"), d("11.3"), "M1").expect_err("改价必须被拦");
        assert_eq!(error.code, error_code::ERP_PURCHASE_RETURN_PRICE_LOCKED);
    }

    /// ⑤ 应付冲减：只冲未付部分（未付款场景，验收场景 ② / ④）。
    #[test]
    fn payable_offset_uses_unpaid_part_only() {
        // 应付 1130 一分未付，退 30 个（含税 339）
        let after = payable_after(d("1130"), d("0"), d("339"));
        assert_eq!(after.amount, d("791"));
        assert_eq!(after.unpaid, d("791"));
        assert_eq!(after.status, STATUS_OPEN);
        assert_eq!(after.to_supplier_receivable, Decimal::ZERO);

        // 全额退货 ⇒ 应付归零（验收场景 ④）
        let after = payable_after(d("1130"), d("0"), d("1130"));
        assert_eq!(after.amount, Decimal::ZERO);
        assert_eq!(after.unpaid, Decimal::ZERO);
        assert_eq!(after.status, STATUS_SETTLED);
    }

    /// ⑥ 应付冲减：已付款部分转供应商应退款（验收场景 ⑤）。
    #[test]
    fn payable_offset_turns_paid_part_into_supplier_receivable() {
        // 全额付款 1130 后退 30 个（含税 339）：应付不再冲减，全部转供应商应退款
        let after = payable_after(d("1130"), d("1130"), d("339"));
        assert_eq!(after.amount, d("1130"), "已付款的应付不能被减到 paid 以下");
        assert_eq!(after.unpaid, Decimal::ZERO);
        assert_eq!(after.status, STATUS_SETTLED);
        assert_eq!(after.to_supplier_receivable, d("339"));

        // 部分付款 500、退 800（含税）：先冲未付 630，剩下 170 转应退款
        let after = payable_after(d("1130"), d("500"), d("800"));
        assert_eq!(after.amount, d("500"));
        assert_eq!(after.unpaid, Decimal::ZERO);
        assert_eq!(after.status, STATUS_SETTLED);
        assert_eq!(after.to_supplier_receivable, d("170"));

        // 部分付款 500、退 300：未付够（630 ≥ 300），应付减到 830，仍部分已付
        let after = payable_after(d("1130"), d("500"), d("300"));
        assert_eq!(after.amount, d("830"));
        assert_eq!(after.unpaid, d("330"));
        assert_eq!(after.status, STATUS_PARTIAL_PAID);
        assert_eq!(after.to_supplier_receivable, Decimal::ZERO);
    }

    /// ⑦ 单头合计 = 明细求和（并且价税恒等）。
    #[test]
    fn totals_satisfy_exact_identity() {
        let lines = vec![
            line_amounts(d("30"), d("10"), d("11.3"), d("13")),
            line_amounts(d("5"), d("20"), d("22.6"), d("13")),
        ];
        let (amount, tax, with_tax) = totals_of(&lines);
        assert_eq!(amount, d("400"));
        assert_eq!(tax, d("52"));
        assert_eq!(with_tax, d("452"));
        assert_eq!(with_tax, amount + tax, "单头也必须精确恒等");
    }

    /// ⑧ 状态标签 / 域值校验（未知状态必须 400，不许静默）。
    #[test]
    fn status_domain_is_closed() {
        for status in STATUSES {
            assert_ne!(status_label(status), "未知状态");
        }
        assert_eq!(status_label("CONFIRMED"), "已确认");
        let query = ReturnPageQuery {
            status: Some("confirmed".to_string()),
            ..Default::default()
        };
        // 小写归一（前端下拉偶尔给小写）
        assert_eq!(
            query.filter().expect("小写应归一").status.as_deref(),
            Some("CONFIRMED")
        );
        let query = ReturnPageQuery {
            status: Some("POSTED".to_string()),
            ..Default::default()
        };
        assert!(query.filter().is_err(), "表外状态必须 400");
    }

    /// ⑨ 权限码常量与迁移逐字一致（改了一处就红，不等运行时 403）。
    #[test]
    fn permission_codes_match_migration() {
        assert_eq!(RETURN_PERMISSIONS.len(), 4);
        assert_eq!(RETURN_PERMISSIONS[0], "business:erp:purchase-return:list");
        assert_eq!(RETURN_PERMISSIONS[1], "business:erp:purchase-return:create");
        assert_eq!(RETURN_PERMISSIONS[2], "business:erp:purchase-return:submit");
        assert_eq!(
            RETURN_PERMISSIONS[3],
            "business:erp:purchase-return:confirm"
        );
    }
}
