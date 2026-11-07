//! 采购申请契约（P2-02）：建申请 / 提交审批 / 合并转采购订单。
//!
//! 与 [`crate::business::erp::price_inquiry_dto`] / [`crate::business::erp::purchase_dto`]
//! 同一套约定：主键与外键一律 `String`（`nik::uuid::Uuid` 没开 serde feature）、
//! 日期 `NaiveDate`（`YYYY-MM-DD`）、金额/数量 `Decimal`（**JSON 里是字符串**）、
//! 状态一律大写字符串且与 DDL 的 `CHECK` 逐字一致。
//!
//! # 合并采购的口径（本卡的核心规则，全部有纯函数 + 单测钉住）
//!
//! ```text
//! ① 同物料相加：多张申请单里 material_id 相同的行合并成**一行**，qty 求和；
//! ② 单价取加权平均：Σ(qty × est_price) / Σqty（6 位舍入）——
//!    两张单一个 10 元一个 12 元合起来不是「随便挑一个」，而是按各自数量分摊；
//! ③ 税率必须一致：同一物料税率不同（13% 与 9%）时合并会让采购单金额口径分叉，
//!    直接 409 让人回申请单改，不替客户猜；
//! ④ 需求日期取最早：合并后要满足最急的那个部门；
//! ⑤ 供应商：请求里给了就用请求的；没给时必须「所有建议供应商一致」（或全都没提），
//!    否则 409 列出候选让人选（合并单只能是同一个供应商的）。
//! ```
//!
//! # 权限码（与 `V0331__purchase_requisition.up.sql` 逐字一致）
//!
//! `business:erp:purchase-requisition:{list,create,submit,to-po}`。

use std::collections::BTreeMap;

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
/// 已通过：审批通过，可以转采购订单。
pub const STATUS_APPROVED: &str = "APPROVED";
/// 已转单：已生成采购订单（幂等闸，不能再转第二次）。
pub const STATUS_CONVERTED: &str = "CONVERTED";

/// 全部状态（与 `ck_biz_prq_status` 逐字一致）。
pub const STATUSES: [&str; 4] = [
    STATUS_DRAFT,
    STATUS_SUBMITTED,
    STATUS_APPROVED,
    STATUS_CONVERTED,
];

/// 明细分行上限（一次提交，防超大数组）。
pub const MAX_LINES: usize = 200;

/// 一次合并转单的申请单张数上限（合并太多张会让「同物料单价加权」难以复核）。
pub const MAX_MERGE: usize = 50;

/// 备注最大长度。
pub const MAX_REMARK_LEN: usize = 500;

/// 本模块 4 个权限码（`perm_menu_gate` 要求每个都在 `sys_menu` 有承载行）。
pub const REQUISITION_PERMISSIONS: [&str; 4] = [
    "business:erp:purchase-requisition:list",
    "business:erp:purchase-requisition:create",
    "business:erp:purchase-requisition:submit",
    "business:erp:purchase-requisition:to-po",
];

/// 今天（北京时间）的日期。
pub fn today_cst() -> NaiveDate {
    (Utc::now() + Duration::hours(8)).date_naive()
}

// ==================== 入参 ====================

/// 新建采购申请。
///
/// `warehouse_id` 是卡面之外补的必填项：转采购订单时 `biz_purchase_order.warehouse_id`
/// 是 `NOT NULL`，申请单不记住收货仓库就只能让用户转单时再猜一次。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CreateRequisitionReq {
    /// 申请日期（缺省 = 今天，北京时间）。
    pub req_date: Option<NaiveDate>,
    /// 申请部门（必填）。
    pub dept_id: Option<String>,
    /// 申请人（缺省 = 当前登录人；姓名一律服务端快照）。
    pub applicant_id: Option<String>,
    /// 需求日期（整单口径，明细没填时用它）。
    pub demand_date: Option<NaiveDate>,
    /// 收货仓库（必填）。
    pub warehouse_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 明细（不能为空）。
    pub items: Vec<RequisitionItemReq>,
}

/// 申请明细行（数量 > 0、预估单价 > 0；单位由物料带出，客户端不许传）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RequisitionItemReq {
    /// 物料（必填）。
    pub material_id: Option<String>,
    /// 申请数量（> 0）。
    pub qty: Option<Decimal>,
    /// 预估不含税单价（> 0，6 位标度）。
    pub est_price: Option<Decimal>,
    /// 税率（百分数 0–100，缺省 0）。
    pub tax_rate: Option<Decimal>,
    /// 行需求日期（缺省取单头 `demand_date`）。
    pub demand_date: Option<NaiveDate>,
    /// 建议供应商。
    pub suggest_supplier_id: Option<String>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 提交审批（可选说明，写进备注供审批人参考）。
///
/// `skip_approval` / `skip_reason` 平铺进请求体（与采购 / 收货 / 付款同款）：
/// 只有**没配已发布的 `pr_approve` 流程**时才用得上，且需要
/// `business:erp:skip-approval` 权限码 + ≥10 字理由（ARCH-3 DC-01 默认拒绝口径）。
/// 流程已发布时请求跳过 → 400（不许绕过已配置的内控）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SubmitRequisitionReq {
    /// 说明（可选，≤ 200 字）。
    pub reason: Option<String>,
    /// 显式跳过审批的请求片段。
    #[serde(flatten)]
    pub skip: crate::business::erp::approval_gate::SkipApprovalReq,
}

/// 合并转采购订单。
///
/// `requisition_ids` 可以省略：只用路径上那一个 id 转单（单张转单）。
/// 给了就必须**包含路径 id**（否则前端拼错 id 会悄悄少转一张单）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ToPoReq {
    /// 参与合并的申请单 id 集合（含路径 id）。
    pub requisition_ids: Vec<String>,
    /// 供应商（不给则按明细里的建议供应商推断，见 [`resolve_merge_supplier`]）。
    pub supplier_id: Option<String>,
    /// 收货仓库（不给则取申请单上的，多单不一致时必填）。
    pub warehouse_id: Option<String>,
    /// 下单日期（缺省 = 今天，北京时间）。
    pub order_date: Option<NaiveDate>,
    /// 期望到货日（缺省 = 合并后最早的需求日期）。
    pub expect_date: Option<NaiveDate>,
    /// 备注。
    pub remark: Option<String>,
}

/// 分页查询。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RequisitionPageQuery {
    /// 页码（缺省 1）。
    pub page: Option<u64>,
    /// 每页条数（缺省 20，上限 200）。
    pub page_size: Option<u64>,
    /// 状态精确过滤。
    pub status: Option<String>,
    /// 关键字：申请单号 / 备注 / 申请部门 / 申请人 / PO 号。
    pub keyword: Option<String>,
    /// 申请日期起（含）。
    pub date_from: Option<NaiveDate>,
    /// 申请日期止（含）。
    pub date_to: Option<NaiveDate>,
    /// 只取「可转单」的（`status = APPROVED` 且没转过）——前端合并选单用。
    pub convertible_only: Option<bool>,
}

/// 数据层筛选条件（空白串 = 没传）。
#[derive(Debug, Clone, Default)]
pub struct RequisitionFilter {
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
    /// 只看可转单。
    pub convertible_only: bool,
}

impl RequisitionPageQuery {
    /// 入参校验 + 归一化（域值乱码一律 400，不静默忽略）。
    pub fn filter(&self) -> std::result::Result<RequisitionFilter, ApiError> {
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
        let (page, page_size) = crate::business::erp::model::normalize_page(
            self.page.unwrap_or(1),
            self.page_size.unwrap_or(20),
        );
        Ok(RequisitionFilter {
            page,
            page_size,
            status,
            keyword,
            date_from: self.date_from,
            date_to: self.date_to,
            convertible_only: self.convertible_only.unwrap_or(false),
        })
    }
}

// ==================== 出参 ====================

/// 列表行。
#[derive(Debug, Clone, Serialize)]
pub struct RequisitionRowRes {
    /// 主键。
    pub id: String,
    /// 申请单号。
    pub req_no: String,
    /// 申请日期。
    pub req_date: NaiveDate,
    /// 申请部门 id。
    pub dept_id: String,
    /// 申请部门名。
    pub dept_name: String,
    /// 申请人 id。
    pub applicant_id: Option<String>,
    /// 申请人姓名。
    pub applicant_name: Option<String>,
    /// 需求日期。
    pub demand_date: Option<NaiveDate>,
    /// 收货仓库 id。
    pub warehouse_id: String,
    /// 收货仓库名。
    pub warehouse_name: String,
    /// 状态。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 预估金额合计（不含税）。
    pub est_amount: Decimal,
    /// 明细行数。
    pub item_count: i64,
    /// 是否超预算。
    pub over_budget: bool,
    /// 超出金额。
    pub over_amount: Decimal,
    /// 预算判定说明。
    pub budget_reason: Option<String>,
    /// 是否做过预算判定。
    pub budget_checked: bool,
    /// 审批实例 id。
    pub approval_instance_id: Option<String>,
    /// 采购订单 id（已转单）。
    pub po_id: Option<String>,
    /// 采购订单号。
    pub po_no: Option<String>,
    /// 转单时间（RFC3339）。
    pub converted_at: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
}

/// 明细行。
#[derive(Debug, Clone, Serialize)]
pub struct RequisitionItemRes {
    /// 主键。
    pub id: String,
    /// 行号。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 单位 id。
    pub unit_id: String,
    /// 单位名。
    pub unit_name: String,
    /// 数量。
    pub qty: Decimal,
    /// 预估不含税单价。
    pub est_price: Decimal,
    /// 预估含税单价（服务端算：est_price × (1 + tax_rate/100)，6 位舍入）。
    pub est_price_incl: Decimal,
    /// 税率。
    pub tax_rate: Decimal,
    /// 行金额（qty × est_price，4 位舍入）。
    pub amount: Decimal,
    /// 需求日期。
    pub demand_date: Option<NaiveDate>,
    /// 建议供应商 id。
    pub suggest_supplier_id: Option<String>,
    /// 建议供应商名。
    pub suggest_supplier_name: Option<String>,
    /// 已并进的采购订单明细行。
    pub po_item_id: Option<String>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 详情 = 单头 + 明细。
#[derive(Debug, Clone, Serialize)]
pub struct RequisitionDetailRes {
    /// 单头。
    pub requisition: RequisitionRowRes,
    /// 明细。
    pub items: Vec<RequisitionItemRes>,
}

/// 转单生成的一张采购订单。
#[derive(Debug, Clone, Serialize)]
pub struct CreatedPurchaseOrderRes {
    /// 采购订单 id。
    pub po_id: String,
    /// 采购订单号。
    pub po_no: String,
    /// 供应商 id。
    pub supplier_id: String,
    /// 供应商名。
    pub supplier_name: String,
    /// 明细行数。
    pub line_count: usize,
    /// 价税合计。
    pub total_with_tax: Decimal,
}

/// 合并明细（一行 = 一个物料，来源可能是多张申请单的多行）。
#[derive(Debug, Clone, Serialize)]
pub struct MergedLineRes {
    /// 采购订单行号（1 起）。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 合并后数量。
    pub qty: Decimal,
    /// 加权平均单价（不含税）。
    pub est_price: Decimal,
    /// 税率。
    pub tax_rate: Decimal,
    /// 需求日期（取最早）。
    pub demand_date: Option<NaiveDate>,
    /// 并进这一行的申请单号（去重、按出现顺序）。
    pub source_req_nos: Vec<String>,
    /// 并进这一行的申请明细 id。
    pub source_item_ids: Vec<String>,
    /// 对应的采购订单明细 id。
    pub po_item_id: Option<String>,
}

/// 转单结果。
#[derive(Debug, Clone, Serialize)]
pub struct ToPoRes {
    /// 生成的采购订单。
    pub purchase_order: CreatedPurchaseOrderRes,
    /// 参与合并的申请单号（按请求顺序）。
    pub requisition_nos: Vec<String>,
    /// 合并后的明细。
    pub lines: Vec<MergedLineRes>,
    /// 合并了多少张申请单。
    pub merged_count: usize,
}

// ==================== 纯函数 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

fn field_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 状态中文名（前端徽标 + 报文共用）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SUBMITTED => "审批中",
        STATUS_APPROVED => "已审批",
        STATUS_CONVERTED => "已转单",
        _ => "未知状态",
    }
}

/// 状态归一化（大小写不敏感，域外值 400）。
pub fn normalize_status(raw: &str) -> std::result::Result<String, ApiError> {
    let upper = raw.trim().to_ascii_uppercase();
    if STATUSES.contains(&upper.as_str()) {
        Ok(upper)
    } else {
        Err(field_invalid(format!(
            "status 只能是 {} 之一（收到 {raw}）",
            STATUSES.join(" / ")
        )))
    }
}

/// 解析 UUID 字段（空串 / 缺省 → 400，因为调用点都是必填项）。
pub fn parse_uuid_field(raw: Option<&str>, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(field_invalid(format!("{field} 不能为空（需要 UUID）")));
    }
    Uuid::parse_str(text)
        .map_err(|_| field_invalid(format!("{field} 不是合法的 UUID（收到 {text}）")))
}

/// 解析可空 UUID（空串 / 缺省 → None；格式错 → 400）。
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
        .map_err(|_| field_invalid(format!("{field} 不是合法的 UUID（收到 {text}）")))
}

/// 数量校验（> 0，4 位标度内）。
pub fn check_qty(raw: Option<Decimal>, field: &str) -> std::result::Result<Decimal, ApiError> {
    let qty = raw.ok_or_else(|| field_invalid(format!("{field} 不能为空")))?;
    if qty <= Decimal::ZERO {
        return Err(field_invalid(format!("{field} 必须大于 0（收到 {qty}）")));
    }
    if qty.scale() > 4 {
        return Err(field_invalid(format!(
            "{field} 最多 4 位小数（收到 {qty}）"
        )));
    }
    Ok(qty)
}

/// 预估单价校验（> 0，6 位标度内）。
///
/// 为什么必须 > 0：金额是预算判定的输入，0 元单价等于「不占预算」，
/// 会让「超预算加签」这条内控被一行 0 元明细绕过。
pub fn check_price(raw: Option<Decimal>, field: &str) -> std::result::Result<Decimal, ApiError> {
    let price = raw.ok_or_else(|| field_invalid(format!("{field} 不能为空")))?;
    if price <= Decimal::ZERO {
        return Err(field_invalid(format!(
            "{field} 必须大于 0（收到 {price}）：预估单价是预算判定的依据，0 元等于不占预算"
        )));
    }
    if price.scale() > 6 {
        return Err(field_invalid(format!(
            "{field} 最多 6 位小数（收到 {price}）"
        )));
    }
    Ok(price)
}

/// 税率校验（0–100）。
pub fn check_tax(raw: Option<Decimal>, field: &str) -> std::result::Result<Decimal, ApiError> {
    let tax = raw.unwrap_or(Decimal::ZERO);
    if tax < Decimal::ZERO || tax > Decimal::from(100) {
        return Err(field_invalid(format!(
            "{field} 必须在 0–100 之间（收到 {tax}）"
        )));
    }
    if tax.scale() > 4 {
        return Err(field_invalid(format!(
            "{field} 最多 4 位小数（收到 {tax}）"
        )));
    }
    Ok(tax)
}

/// 备注清洗（trim + 长度上限 + 空串归 None）。
pub fn clean_remark(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let count = text.chars().count();
    if count > MAX_REMARK_LEN {
        return Err(field_invalid(format!(
            "备注最多 {MAX_REMARK_LEN} 字（收到 {count} 字）"
        )));
    }
    Ok(Some(text.to_string()))
}

/// 含税单价（6 位舍入）：`price × (1 + tax/100)`。
pub fn price_incl_of(price_excl: Decimal, tax_rate: Decimal) -> Decimal {
    let factor = Decimal::ONE + tax_rate / Decimal::from(100);
    (price_excl * factor).round_dp(6)
}

/// 行金额（4 位舍入）：`qty × est_price`。
pub fn line_amount(qty: Decimal, est_price: Decimal) -> Decimal {
    (qty * est_price).round_dp(4)
}

/// 申请单预估合计（不含税，4 位舍入）。
pub fn est_amount_of(lines: &[(Decimal, Decimal)]) -> Decimal {
    lines
        .iter()
        .fold(Decimal::ZERO, |acc, (qty, price)| {
            acc + line_amount(*qty, *price)
        })
        .round_dp(4)
}

/// 合并输入（一行申请明细）。
#[derive(Debug, Clone, PartialEq)]
pub struct MergeInput {
    /// 申请单 id。
    pub requisition_id: Uuid,
    /// 申请单号（结果里回显）。
    pub requisition_no: String,
    /// 明细 id。
    pub item_id: Uuid,
    /// 物料。
    pub material_id: Uuid,
    /// 数量。
    pub qty: Decimal,
    /// 预估不含税单价。
    pub est_price: Decimal,
    /// 税率。
    pub tax_rate: Decimal,
    /// 需求日期。
    pub demand_date: Option<NaiveDate>,
    /// 建议供应商。
    pub suggest_supplier_id: Option<Uuid>,
}

/// 合并输出（一行 = 一个物料）。
#[derive(Debug, Clone, PartialEq)]
pub struct MergedLine {
    /// 物料。
    pub material_id: Uuid,
    /// 合并后数量。
    pub qty: Decimal,
    /// 加权平均单价（6 位舍入）。
    pub est_price: Decimal,
    /// 税率。
    pub tax_rate: Decimal,
    /// 需求日期（最早）。
    pub demand_date: Option<NaiveDate>,
    /// 来源申请明细 id（按输入顺序）。
    pub source_item_ids: Vec<Uuid>,
    /// 来源申请单 id（按输入顺序、去重）。
    pub source_requisition_ids: Vec<Uuid>,
    /// 来源申请单号（按输入顺序、去重）。
    pub source_req_nos: Vec<String>,
}

/// 合并的失败原因（纯函数返回，handler 翻成 409 + 码）。
#[derive(Debug, Clone, PartialEq)]
pub enum MergeError {
    /// 同一物料在多张申请单上税率不一致。
    TaxRateConflict {
        /// 物料。
        material_id: Uuid,
        /// 出现的税率（去重升序）。
        rates: Vec<Decimal>,
    },
    /// 建议供应商不唯一。
    SupplierAmbiguous {
        /// 候选供应商（去重）。
        suppliers: Vec<Uuid>,
    },
    /// 一个建议供应商都没有，必须显式指定。
    SupplierRequired,
    /// 物料明细为空。
    Empty,
}

/// 合并同物料的多张申请明细。
///
/// 排序：按物料 id 升序（BTreeMap 遍历序）⇒ 同一批输入永远得到同一套行序，
/// 采购单明细行号稳定（重复转单的可复现性靠它）。
pub fn merge_by_material(
    inputs: &[MergeInput],
) -> std::result::Result<Vec<MergedLine>, MergeError> {
    if inputs.is_empty() {
        return Err(MergeError::Empty);
    }
    let mut by_material: BTreeMap<Uuid, Vec<&MergeInput>> = BTreeMap::new();
    for input in inputs {
        by_material
            .entry(input.material_id)
            .or_default()
            .push(input);
    }
    let mut out = Vec::with_capacity(by_material.len());
    for (material_id, rows) in by_material {
        // ① 税率必须一致
        let mut rates: Vec<Decimal> = rows.iter().map(|row| row.tax_rate).collect();
        rates.sort();
        rates.dedup();
        if rates.len() > 1 {
            return Err(MergeError::TaxRateConflict { material_id, rates });
        }
        // ② 数量求和 + 加权平均单价
        let total_qty = rows.iter().fold(Decimal::ZERO, |acc, row| acc + row.qty);
        if total_qty <= Decimal::ZERO {
            // 理论上不可能（单行 qty > 0 已在建单时校验），但不许静默除零
            return Err(MergeError::Empty);
        }
        let total_value = rows
            .iter()
            .fold(Decimal::ZERO, |acc, row| acc + row.qty * row.est_price);
        let est_price = (total_value / total_qty).round_dp(6);
        // ③ 需求日期取最早（合并后要满足最急的部门）
        let demand_date = rows.iter().filter_map(|row| row.demand_date).min();
        let mut source_item_ids = Vec::with_capacity(rows.len());
        let mut source_requisition_ids: Vec<Uuid> = Vec::new();
        let mut source_req_nos: Vec<String> = Vec::new();
        for row in &rows {
            source_item_ids.push(row.item_id);
            if !source_requisition_ids.contains(&row.requisition_id) {
                source_requisition_ids.push(row.requisition_id);
                source_req_nos.push(row.requisition_no.clone());
            }
        }
        out.push(MergedLine {
            material_id,
            qty: total_qty,
            est_price,
            tax_rate: rates[0],
            demand_date,
            source_item_ids,
            source_requisition_ids,
            source_req_nos,
        });
    }
    Ok(out)
}

/// 合并单的供应商：请求优先；否则必须「所有建议供应商一致」。
pub fn resolve_merge_supplier(
    inputs: &[MergeInput],
    requested: Option<Uuid>,
) -> std::result::Result<Uuid, MergeError> {
    if let Some(supplier) = requested {
        return Ok(supplier);
    }
    let mut suppliers: Vec<Uuid> = inputs
        .iter()
        .filter_map(|input| input.suggest_supplier_id)
        .collect();
    suppliers.sort();
    suppliers.dedup();
    match suppliers.len() {
        0 => Err(MergeError::SupplierRequired),
        1 => Ok(suppliers[0]),
        _ => Err(MergeError::SupplierAmbiguous { suppliers }),
    }
}

/// 合并单的收货仓库解析失败原因。
#[derive(Debug, Clone, PartialEq)]
pub enum WarehouseError {
    /// 一张单都没有（理论不可达）。
    Empty,
    /// 多张申请单的收货仓库不一致（返回候选，去重升序）。
    Conflict(Vec<Uuid>),
}

/// 合并单的收货仓库：请求优先；否则所有申请单必须一致。
pub fn resolve_merge_warehouse(
    warehouses: &[Uuid],
    requested: Option<Uuid>,
) -> std::result::Result<Uuid, WarehouseError> {
    if let Some(warehouse) = requested {
        return Ok(warehouse);
    }
    let mut unique: Vec<Uuid> = warehouses.to_vec();
    unique.sort();
    unique.dedup();
    match unique.len() {
        0 => Err(WarehouseError::Empty),
        1 => Ok(unique[0]),
        _ => Err(WarehouseError::Conflict(unique)),
    }
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("十进制字面量")
    }

    fn input(
        material: &str,
        req: &str,
        item: &str,
        qty: &str,
        price: &str,
        tax: &str,
    ) -> MergeInput {
        MergeInput {
            requisition_id: Uuid::parse_str(req).unwrap(),
            requisition_no: format!("PRQ-{req}"),
            item_id: Uuid::parse_str(item).unwrap(),
            material_id: Uuid::parse_str(material).unwrap(),
            qty: dec(qty),
            est_price: dec(price),
            tax_rate: dec(tax),
            demand_date: None,
            suggest_supplier_id: None,
        }
    }

    const M1: &str = "00000000-0000-0000-0000-0000000000a1";
    const M2: &str = "00000000-0000-0000-0000-0000000000a2";
    const R1: &str = "00000000-0000-0000-0000-0000000000b1";
    const R2: &str = "00000000-0000-0000-0000-0000000000b2";
    const I1: &str = "00000000-0000-0000-0000-0000000000c1";
    const I2: &str = "00000000-0000-0000-0000-0000000000c2";
    const I3: &str = "00000000-0000-0000-0000-0000000000c3";
    const S1: &str = "00000000-0000-0000-0000-0000000000d1";
    const S2: &str = "00000000-0000-0000-0000-0000000000d2";

    /// 验收场景的纯函数版：部门 A 100 个 + 部门 B 50 个 → 一行 150 个。
    #[test]
    fn merge_sums_same_material_qty() {
        let rows = vec![
            input(M1, R1, I1, "100", "10", "13"),
            input(M1, R2, I2, "50", "10", "13"),
        ];
        let merged = merge_by_material(&rows).expect("同物料同税率必须能合并");
        assert_eq!(merged.len(), 1, "同物料必须并成一行");
        assert_eq!(merged[0].qty, dec("150"));
        assert_eq!(merged[0].est_price, dec("10"));
        assert_eq!(
            merged[0].source_item_ids,
            vec![Uuid::parse_str(I1).unwrap(), Uuid::parse_str(I2).unwrap()]
        );
        assert_eq!(merged[0].source_requisition_ids.len(), 2);
        assert_eq!(
            merged[0].source_req_nos,
            vec![format!("PRQ-{R1}"), format!("PRQ-{R2}")]
        );
    }

    /// 单价取加权平均：100×10 + 50×12 = 1600 / 150 = 10.666667（6 位舍入）。
    #[test]
    fn merge_weights_price_by_qty() {
        let rows = vec![
            input(M1, R1, I1, "100", "10", "13"),
            input(M1, R2, I2, "50", "12", "13"),
        ];
        let merged = merge_by_material(&rows).expect("价格不同仍可合并（加权）");
        assert_eq!(merged[0].qty, dec("150"));
        assert_eq!(merged[0].est_price, dec("10.666667"));
        // 合并后金额 = 原两张单金额之和（分摊不产生差额）
        let merged_value = line_amount(merged[0].qty, merged[0].est_price);
        let source_value = line_amount(dec("100"), dec("10")) + line_amount(dec("50"), dec("12"));
        let diff = (merged_value - source_value).abs();
        assert!(
            diff < dec("0.0001"),
            "加权平均不能让金额漂移：merged={merged_value} source={source_value}"
        );
    }

    /// 税率不同不许合并（13% 与 9% 混在一行会让采购单金额口径分叉）。
    #[test]
    fn merge_rejects_tax_conflict() {
        let rows = vec![
            input(M1, R1, I1, "100", "10", "13"),
            input(M1, R2, I2, "50", "10", "9"),
        ];
        match merge_by_material(&rows) {
            Err(MergeError::TaxRateConflict { rates, .. }) => {
                assert_eq!(rates, vec![dec("9"), dec("13")]);
            }
            other => panic!("税率冲突必须被拒，实际 {other:?}"),
        }
    }

    /// 不同物料不合并，且按物料 id 升序（行号稳定）。
    #[test]
    fn merge_keeps_distinct_materials_sorted() {
        let rows = vec![
            input(M2, R1, I3, "5", "3", "0"),
            input(M1, R1, I1, "100", "10", "13"),
            input(M1, R2, I2, "50", "10", "13"),
        ];
        let merged = merge_by_material(&rows).expect("可以合并");
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].material_id, Uuid::parse_str(M1).unwrap());
        assert_eq!(merged[0].qty, dec("150"));
        assert_eq!(merged[1].material_id, Uuid::parse_str(M2).unwrap());
        assert_eq!(merged[1].qty, dec("5"));
    }

    /// 需求日期取最早（合并后要满足最急的部门）。
    #[test]
    fn merge_takes_earliest_demand_date() {
        let mut a = input(M1, R1, I1, "100", "10", "13");
        a.demand_date = Some(NaiveDate::from_ymd_opt(2026, 12, 20).unwrap());
        let mut b = input(M1, R2, I2, "50", "10", "13");
        b.demand_date = Some(NaiveDate::from_ymd_opt(2026, 11, 30).unwrap());
        let merged = merge_by_material(&[a, b]).expect("可以合并");
        assert_eq!(
            merged[0].demand_date,
            Some(NaiveDate::from_ymd_opt(2026, 11, 30).unwrap())
        );
    }

    /// 供应商推断：一致就用它；不一致 / 一个都没有则拒绝。
    #[test]
    fn supplier_resolution_rules() {
        let s1 = Uuid::parse_str(S1).unwrap();
        let s2 = Uuid::parse_str(S2).unwrap();
        let mut a = input(M1, R1, I1, "100", "10", "13");
        a.suggest_supplier_id = Some(s1);
        let mut b = input(M1, R2, I2, "50", "10", "13");
        b.suggest_supplier_id = Some(s1);
        assert_eq!(
            resolve_merge_supplier(&[a.clone(), b.clone()], None),
            Ok(s1)
        );
        // 请求指定优先（哪怕建议是另一个）
        assert_eq!(
            resolve_merge_supplier(&[a.clone(), b.clone()], Some(s2)),
            Ok(s2)
        );
        // 一个都没有 → 必须显式指定
        let plain = input(M1, R1, I1, "1", "1", "0");
        assert_eq!(
            resolve_merge_supplier(&[plain.clone()], None),
            Err(MergeError::SupplierRequired)
        );
        // 两家不同 → 候选列出来让人选
        let mut c = b;
        c.suggest_supplier_id = Some(s2);
        match resolve_merge_supplier(&[a, c], None) {
            Err(MergeError::SupplierAmbiguous { suppliers }) => {
                assert_eq!(suppliers.len(), 2);
            }
            other => panic!("供应商不一致必须被拒，实际 {other:?}"),
        }
    }

    /// 仓库推断：一致就用它；不一致返回候选。
    #[test]
    fn warehouse_resolution_rules() {
        let w1 = Uuid::parse_str(M1).unwrap();
        let w2 = Uuid::parse_str(M2).unwrap();
        assert_eq!(resolve_merge_warehouse(&[w1, w1], None), Ok(w1));
        assert_eq!(resolve_merge_warehouse(&[w1, w2], Some(w2)), Ok(w2));
        match resolve_merge_warehouse(&[w1, w2], None) {
            Err(WarehouseError::Conflict(candidates)) => assert_eq!(candidates.len(), 2),
            other => panic!("仓库不一致必须报候选，实际 {other:?}"),
        }
    }

    /// 金额口径：行金额 4 位、含税单价 6 位、合计四舍五入。
    #[test]
    fn amounts_are_rounded_server_side() {
        assert_eq!(line_amount(dec("3"), dec("1.2345678")), dec("3.7037"));
        assert_eq!(price_incl_of(dec("100"), dec("13")), dec("113"));
        assert_eq!(price_incl_of(dec("9.99"), dec("13")), dec("11.288700"));
        let total = est_amount_of(&[(dec("100"), dec("10")), (dec("50"), dec("10"))]);
        assert_eq!(total, dec("1500"));
    }

    /// 状态域与中文名一一对应（前端徽标靠它）。
    #[test]
    fn statuses_are_exhaustive() {
        for status in STATUSES {
            assert_ne!(status_label(status), "未知状态", "{status} 缺中文名");
        }
        assert_eq!(status_label(STATUS_DRAFT), "草稿");
        assert_eq!(status_label(STATUS_SUBMITTED), "审批中");
        assert_eq!(status_label(STATUS_APPROVED), "已审批");
        assert_eq!(status_label(STATUS_CONVERTED), "已转单");
        assert_eq!(STATUSES.len(), 4);
        assert_eq!(
            normalize_status(" converted ").unwrap(),
            STATUS_CONVERTED,
            "归一化必须大小写不敏感"
        );
        assert!(normalize_status("DONE").is_err(), "域外状态必须 400");
    }

    /// 入参校验：0 数量 / 0 单价 / 负税率 / 超长备注 一律 400。
    #[test]
    fn input_guards_reject_bad_values() {
        assert!(check_qty(Some(dec("0")), "qty").is_err(), "0 数量必须拒");
        assert!(check_qty(Some(dec("-1")), "qty").is_err(), "负数量必须拒");
        assert!(
            check_qty(Some(dec("1.00001")), "qty").is_err(),
            "5 位小数必须拒"
        );
        assert!(
            check_price(Some(dec("0")), "price").is_err(),
            "0 单价必须拒（会绕过预算）"
        );
        assert!(
            check_tax(Some(dec("101")), "tax").is_err(),
            "税率 > 100 必须拒"
        );
        assert!(check_tax(Some(dec("-1")), "tax").is_err(), "负税率必须拒");
        assert!(check_tax(None, "tax").is_ok(), "税率缺省 = 0");
        assert!(
            clean_remark(Some("   ")).unwrap().is_none(),
            "空白备注归 None"
        );
        let long = "长".repeat(MAX_REMARK_LEN + 1);
        assert!(clean_remark(Some(&long)).is_err(), "超长备注必须拒");
    }

    /// 权限码清单与迁移逐字一致（4 个，不多不少）。
    #[test]
    fn permission_codes_are_exact() {
        assert_eq!(REQUISITION_PERMISSIONS.len(), 4);
        for code in REQUISITION_PERMISSIONS {
            assert!(
                code.starts_with("business:erp:purchase-requisition:"),
                "权限码前缀必须与迁移一致：{code}"
            );
        }
        assert!(REQUISITION_PERMISSIONS.contains(&"business:erp:purchase-requisition:to-po"));
    }
}
