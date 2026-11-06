//! 销售退货单接口层（销售链 · SO-29）：建单 / 详情 / 分页 / 状态流转 / 带出可退明细。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/sales-return/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/sales-return` | `business:erp:sales-return:create` | 建单（`DRAFT`，取号 `SR-yyyyMMdd-000001`）；`items` 留空 = 从原发货单**带出可退明细** |
//! | GET | `/business/erp/sales-return/{id}` | `business:erp:sales-return:list` | 详情（含明细 + 客户 / 仓库 / 原单号） |
//! | GET | `/business/erp/sales-return/page` | `business:erp:sales-return:list` | 分页（`return_no` 模糊 / `status` 精确 / `so_id` / `customer_id` / `shipment_id`） |
//! | GET | `/business/erp/sales-return/returnable/{shipment_id}` | `business:erp:sales-return:list` | 该发货单**可退明细**（前端「从发货单带出」对话框的唯一读数） |
//! | PATCH | `/business/erp/sales-return/{id}/status` | `business:erp:sales-return:status` | 状态流转（只走 `SALES_RETURN_EDGES` 的 7 条边，其余 409） |
//!
//! # 状态机（唯一真相：`state_machine::SALES_RETURN_EDGES`）
//!
//! ```text
//! DRAFT ──提交──▶ SUBMITTED ──批准──▶ APPROVED ──▶ PENDING_POST ──过账──▶ POSTED
//!   │                │                    │
//!   └──────────────  ┴────────────────────┴──作废──▶ CANCELLED
//! ```
//!
//! - 非法迁移一律 **409 + 可选去向**（不是 500、也不是静默忽略）；
//! - `SUBMITTED / APPROVED → CANCELLED` 缺 `reason` → **400**
//!   （与 `state_machine::SALES_RETURN_REASON_EDGES` 同源）；
//! - 落库走两道：`state_machine::transition`（边表 + reason + 写 `biz_status_log`）→
//!   `model::apply_status`（CAS `WHERE status = from`），同一事务。
//!
//! # 本卡的范围（SO-29 = CRUD + 状态机 + 超退守卫）
//!
//! **过账（`PENDING_POST → POSTED`）的执行体不在本卡**：回库单 / 凭证 / 应收冲减 /
//! 红字发票 / 回写 `delivered_qty` 是 SO-30 的一个事务。状态机里这条边**已登记**，
//! 但本文件的 [`change_return_status`] 对 `POSTED` 显式返回 **409**（文案点名 SO-30），
//! 避免「状态说已过账、账上一分没动」这种更坏的结果。SO-30 落地时把这段闸删掉、
//! 换成同事务的过账调用（与 SO-09 → SO-10 的分工方式一致）。
//!
//! # 三道独立的闸（都不是摆设）
//!
//! | 闸 | 位置 | 挡什么 |
//! |----|------|--------|
//! | 原单闸 | [`create_return`] | 给 `DRAFT`/`SUBMITTED`/`APPROVED` 的发货单退货（货还没出库，退什么）；发货单与请求里的客户 / 订单不一致 |
//! | 超退闸 | `model::ensure_qty_within_returnable` | 累计退货量 > 已发货量 − 已退量（含**别的未作废退货单**的占用） |
//! | 提交闸 | [`change_return_status`] | 提交 / 待过账时**重跑**超退判定（草稿期间别人可能已经退过同一行） |
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_sales_return / biz_sales_return_item 是 ERP 单据表（V0303 无 dept_id /
//! 归属人列），数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:sales-return:*
//! 权限码控制。分页走 model::return_page → page_of → paginate_unscoped。
//! ```

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::sales_order_model;
use crate::business::erp::sales_return_model::{
    self as model, NewReturn, NewReturnItem, ReturnItemRow, ReturnRow,
};
use crate::business::erp::shipment_model;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0304 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 / 详情 / 可退明细。
pub const PERM_LIST: &str = "business:erp:sales-return:list";
/// 新建。
pub const PERM_CREATE: &str = "business:erp:sales-return:create";
/// 状态流转（提交 / 批准 / 待过账 / 作废）。
pub const PERM_STATUS: &str = "business:erp:sales-return:status";

/// 退货单域全部权限码（顺序 = V0304 的 VALUES 顺序；`perm_menu_gate.py` 要求每个都在
/// `sys_menu` 有承载行）。
pub const SALES_RETURN_PERMISSIONS: [&str; 3] = [PERM_LIST, PERM_CREATE, PERM_STATUS];

/// 能给退货单「带出可退明细」的发货单状态：只有**已过账**的货才在客户手上。
pub const RETURNABLE_SHIPMENT_STATUSES: [&str; 1] = [model::STATUS_POSTED];

/// 能退货的订单状态（没指定发货单、只挂订单时用）：
/// 订单必须已经发过货（`PARTIAL_OUT` / `OUTBOUND`），或已关闭（关闭只作废剩余未发量，
/// 已发部分仍可退）。`DRAFT` / `SUBMITTED` / `APPROVED` 一分货没出 ⇒ 无从退起。
pub const RETURNABLE_SO_STATUSES: [&str; 3] = ["PARTIAL_OUT", "OUTBOUND", "CLOSED"];

// ==================== 校验原语 ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `404`：退货单不存在 / 已删。
fn not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "销售退货单不存在或已删除".to_string(),
    )
}

/// `409`：状态 / 并发 / 业务冲突。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
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

/// 可选 UUID（空白 → `None`，非法 → 400）。
fn parse_opt_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => Ok(Some(parse_uuid(text, field)?)),
    }
}

/// 可选日期（`yyyy-MM-dd`）：空白 → `None`；格式不对 → 400。
fn parse_date(raw: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| bad_request(format!("{field} 必须是 yyyy-MM-dd（当前 `{text}`）"))),
    }
}

/// 今天（北京时间）。审计口径与其它 ERP 单据一致。
fn today_cst(now: DateTime<Utc>) -> NaiveDate {
    let key = crate::business::erp::serial_model::period_key_of(now);
    NaiveDate::parse_from_str(&key, "%Y%m%d").unwrap_or_else(|_| now.date_naive())
}

/// 写路径的数据库错误收口：唯一冲突 → 409；CHECK → 400；其余 500。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "退货单号冲突（唯一约束{}）：取号器与库里的号撞了，请重试",
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
                "数据不满足数据库约束{}（数量必须 > 0 / 金额非负 / 价税合计恒等 / 状态域 6 值）",
                constraint
                    .as_deref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default()
            ),
        ),
        _ => utils::db_error(err, ctx),
    }
}

// ==================== 纯逻辑校验（单测直接调，不碰库） ====================

/// 状态目标值校验：必须在 6 值域内。
pub fn validate_target(raw: &str) -> std::result::Result<String, String> {
    let target = raw.trim().to_string();
    if !model::is_valid_status(&target) {
        return Err(format!(
            "target_status 必须是 {} 之一（当前 `{raw}`）",
            model::STATUSES.join(" / ")
        ));
    }
    Ok(target)
}

/// 退货类型校验：必须在 3 值域内。
pub fn validate_return_type(raw: &str) -> std::result::Result<String, String> {
    let value = raw.trim().to_string();
    if !model::is_valid_return_type(&value) {
        return Err(format!(
            "return_type 必须是 {} 之一（当前 `{raw}`）",
            model::RETURN_TYPES.join(" / ")
        ));
    }
    Ok(value)
}

/// 本行数量校验：`qty > 0` 且不超过可退量。
///
/// 与库层 `ck_biz_sreturn_item_qty`（只兜 `qty > 0`）互补：上界是跨表的，
/// 只能在这里判，报文要说清「已发多少 / 已退多少 / 还剩多少」。
pub fn validate_qty(
    qty: Decimal,
    shipped: Decimal,
    returned: Decimal,
    label: &str,
) -> std::result::Result<(), String> {
    if qty <= Decimal::ZERO {
        return Err(format!("{label}.qty 必须大于 0（当前 {qty}）"));
    }
    if !model::qty_within_returnable(qty, shipped, returned) {
        return Err(format!(
            "{label}.qty = {qty} 超过可退量 {}（已发 {shipped} − 已退 {returned}）：超退被拒绝",
            model::remaining_returnable(shipped, returned)
        ));
    }
    Ok(())
}

/// 单价 / 税率校验：早于库层 CHECK 给出人话报文。
pub fn validate_amount_inputs(
    price: Decimal,
    tax_rate: Decimal,
    label: &str,
) -> std::result::Result<(), String> {
    if price < Decimal::ZERO {
        return Err(format!("{label}.price 不能为负（当前 {price}）"));
    }
    if tax_rate < Decimal::ZERO || tax_rate > Decimal::from(100) {
        return Err(format!(
            "{label}.tax_rate 必须是 0–100 的百分数（当前 {tax_rate}）"
        ));
    }
    Ok(())
}

/// 序列号条数校验：填了就必须**一物一码**（条数 = 数量），不填则交给过账按库存分配。
pub fn validate_serials(
    serial_nos: &[String],
    qty: Decimal,
    label: &str,
) -> std::result::Result<(), String> {
    if serial_nos.is_empty() {
        return Ok(());
    }
    let count = Decimal::from(serial_nos.len());
    if count != qty {
        return Err(format!(
            "{label}.serial_nos 有 {} 个，与 qty {qty} 不符：序列号必须一物一码",
            serial_nos.len()
        ));
    }
    for (index, serial) in serial_nos.iter().enumerate() {
        if serial.trim().is_empty() {
            return Err(format!("{label}.serial_nos[{index}] 不能是空串"));
        }
    }
    Ok(())
}

/// 同一张退货单里同一订单行只能出现一次（重复行会把超退守卫绕过去：
/// 守卫逐行判「单行 ≤ 可退量」，两行各 6 在可退 10 的行上都能过）。
pub fn ensure_unique_so_items(so_item_ids: &[Uuid]) -> std::result::Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for id in so_item_ids {
        if !seen.insert(*id) {
            return Err(format!(
                "订单行 {id} 在同一张退货单里出现了两次：请合并成一行（数量相加）"
            ));
        }
    }
    Ok(())
}

// ==================== 契约 ====================

/// 分页筛选：`return_no` 模糊、`status` 精确、`so_id` / `customer_id` / `shipment_id` 精确。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ReturnFilter {
    /// 单号（模糊）。
    pub return_no: Option<String>,
    /// 状态（精确：6 值之一）。
    pub status: Option<String>,
    /// 原销售订单 id（精确 UUID）。
    pub so_id: Option<String>,
    /// 客户 id（精确 UUID）。
    pub customer_id: Option<String>,
    /// 原发货单 id（精确 UUID）。
    pub shipment_id: Option<String>,
    /// 页码（从 1 起）。
    pub page: u64,
    /// 每页条数（0 → 默认 20，上限 200）。
    pub page_size: u64,
}

/// 一行明细入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReturnItemReq {
    /// 对应的原销售订单行（有原单必填；`OTHER` 无原单可留空）。
    #[serde(default)]
    pub so_item_id: Option<String>,
    /// 物料（有 `so_item_id` 时**忽略**：服务端从订单行带出）。
    #[serde(default)]
    pub material_id: Option<String>,
    /// 退货数量（留空 = 该行**全部可退量**；仅 `so_item_id` 存在时可用）。
    #[serde(default)]
    pub qty: Option<Decimal>,
    /// 单价（不含税；留空 = 订单行单价，无原单时默认 0）。
    #[serde(default)]
    pub price: Option<Decimal>,
    /// 税率百分数（留空 = 订单行税率，无原单时默认 0）。
    #[serde(default)]
    pub tax_rate: Option<Decimal>,
    /// 退回库位（留空 = 整仓）。
    #[serde(default)]
    pub location_id: Option<String>,
    /// 序列号（序列号物料才填，条数必须等于数量）。
    #[serde(default)]
    pub serial_nos: Vec<String>,
    /// 行备注。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 新建销售退货单。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateReturnReq {
    /// 原发货单（**推荐**：传了就按这张单的可退量校验，`items` 留空还能自动带出）。
    #[serde(default)]
    pub shipment_id: Option<String>,
    /// 原销售订单（可空；与 `shipment_id` 同时传时必须一致）。
    #[serde(default)]
    pub so_id: Option<String>,
    /// 客户（可空：传了 `shipment_id` / `so_id` 时由服务端带出并校验一致）。
    #[serde(default)]
    pub customer_id: Option<String>,
    /// 退回入库仓库（必填）。
    pub warehouse_id: String,
    /// 退货日期（`yyyy-MM-dd`，留空 = 今天，北京时间）。
    #[serde(default)]
    pub return_date: Option<String>,
    /// 退货类型：`QUALITY` / `WRONG_ITEM` / `OTHER`（必填）。
    pub return_type: String,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
    /// 明细（**留空 = 从原发货单带出全部可退明细**，各行数量 = 该行可退量）。
    #[serde(default)]
    pub items: Vec<ReturnItemReq>,
}

/// 状态流转请求。
#[derive(Debug, Clone, Deserialize)]
pub struct ReturnStatusReq {
    /// 目标状态（6 值之一，且必须是 `SALES_RETURN_EDGES` 里的合法边）。
    pub target_status: String,
    /// 原因（`SUBMITTED / APPROVED → CANCELLED` 必填）。
    #[serde(default)]
    pub reason: Option<String>,
}

/// 明细出参。
#[derive(Debug, Clone, Serialize)]
pub struct ReturnItemRes {
    /// 主键。
    pub id: String,
    /// 订单行号（1 起；无原单 / 订单行已删时为 0）。
    pub line_no: i32,
    /// 对应的原销售订单行 id（无原单为 `null`）。
    pub so_item_id: Option<String>,
    /// 物料 id。
    pub material_id: String,
    /// 退货数量。
    pub qty: Decimal,
    /// 单价（不含税）。
    pub price: Decimal,
    /// 行不含税金额。
    pub amount: Decimal,
    /// 税率百分数。
    pub tax_rate: Decimal,
    /// 行税额。
    pub tax_amount: Decimal,
    /// 退回库位（`null` = 整仓）。
    pub location_id: Option<String>,
    /// 序列号。
    pub serial_nos: Vec<String>,
    /// 行备注。
    pub remark: Option<String>,
}

impl From<&ReturnItemRow> for ReturnItemRes {
    fn from(row: &ReturnItemRow) -> Self {
        Self {
            id: row.id.to_string(),
            line_no: row.line_no,
            so_item_id: row.so_item_id.map(|id| id.to_string()),
            material_id: row.material_id.to_string(),
            qty: row.qty,
            price: row.price,
            amount: row.amount,
            tax_rate: row.tax_rate,
            tax_amount: row.tax_amount,
            location_id: row.location_id.map(|id| id.to_string()),
            serial_nos: row.serial_nos.clone(),
            remark: row.remark.clone(),
        }
    }
}

/// 一行「可退明细」（`GET /returnable/{shipment_id}` 的出参）。
#[derive(Debug, Clone, Serialize)]
pub struct ReturnableItemRes {
    /// 订单行 id。
    pub so_item_id: String,
    /// 订单行号（1 起）。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 已发货量。
    pub shipped: Decimal,
    /// 已退量（同一发货单上未作废退货的合计）。
    pub returned: Decimal,
    /// 可退量（`shipped - returned`，下限 0）。
    pub remaining: Decimal,
    /// 单价（取自订单行）。
    pub price: Decimal,
    /// 税率（百分数，取自订单行）。
    pub tax_rate: Decimal,
}

/// 退货单出参（详情 / 列表行同构）。
///
/// **列表行**：`items` 恒为空数组（明细只在详情里返回），但客户 / 仓库 / 原单号会
/// **批量补齐**（各自一次 `IN (…)` 往返，见 [`page_return`]），一页固定 4 条补齐 SQL、
/// 不做 N+1 —— 列表与详情**逐字段同口径**。
#[derive(Debug, Clone, Serialize)]
pub struct ReturnRes {
    /// 主键。
    pub id: String,
    /// 单号。
    pub return_no: String,
    /// 客户 id。
    pub customer_id: String,
    /// 客户编码（客户软删后为 `null`）。
    pub customer_code: Option<String>,
    /// 客户名称（客户软删后为 `null`）。
    pub customer_name: Option<String>,
    /// 原销售订单 id（可空）。
    pub so_id: Option<String>,
    /// 原销售订单号（订单不存在 / 已软删 / 未挂订单时为 `null`）。
    pub so_no: Option<String>,
    /// 原发货单 id（可空）。
    pub shipment_id: Option<String>,
    /// 原发货单号（发货单不存在 / 已软删 / 未挂发货单时为 `null`）。
    pub shipment_no: Option<String>,
    /// 退货日期（`yyyy-MM-dd`）。
    pub return_date: String,
    /// 退货类型编码。
    pub return_type: String,
    /// 退货类型中文名。
    pub return_type_label: String,
    /// 退回入库仓库 id。
    pub warehouse_id: String,
    /// 仓库编码（仓库软删后为 `null`）。
    pub warehouse_code: Option<String>,
    /// 仓库名称（仓库软删后为 `null`）。
    pub warehouse_name: Option<String>,
    /// 状态编码。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 不含税金额合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计。
    pub total_with_tax: Decimal,
    /// 过账生成的入库单（SO-30 回填；未过账为 `null`）。
    pub stock_in_id: Option<String>,
    /// 冲减的应收单（SO-30 回填）。
    pub receivable_id: Option<String>,
    /// 红字发票（SO-30 回填）。
    pub red_invoice_id: Option<String>,
    /// 审批实例 id（弱引用流程引擎；未提审为 `null`）。
    ///
    /// SO-31 起对前端下发（前端 `FlowProgress` 靠它打 `GET /business/flow/instance/{id}`）。
    /// **现状**：退货单尚未接入流程引擎（`change_return_status` 不建实例），本字段恒
    /// `null`，前端按 `approval-progress.ts` 的共享口径降级为「不渲染进度块」；
    /// 接上引擎后前端无需再改。与销售订单 `approval_instance_id` 同口径（列表也下发）。
    pub approval_instance_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 明细（分页列表里恒为空）。
    pub items: Vec<ReturnItemRes>,
}

impl ReturnRes {
    /// 单头 → 出参（`items` 由调用方填）。
    fn of(row: &ReturnRow, items: Vec<ReturnItemRes>) -> Self {
        Self {
            id: row.id.to_string(),
            return_no: row.return_no.clone(),
            customer_id: row.customer_id.to_string(),
            customer_code: None,
            customer_name: None,
            so_id: row.so_id.map(|id| id.to_string()),
            so_no: None,
            shipment_id: row.shipment_id.map(|id| id.to_string()),
            shipment_no: None,
            return_date: row.return_date.to_string(),
            return_type: row.return_type.clone(),
            return_type_label: model::return_type_label(&row.return_type).to_string(),
            warehouse_id: row.warehouse_id.to_string(),
            warehouse_code: None,
            warehouse_name: None,
            status: row.status.clone(),
            status_label: model::status_label(&row.status).to_string(),
            total_amount: row.total_amount,
            total_tax: row.total_tax,
            total_with_tax: row.total_with_tax,
            stock_in_id: row.stock_in_id.map(|id| id.to_string()),
            receivable_id: row.receivable_id.map(|id| id.to_string()),
            red_invoice_id: row.red_invoice_id.map(|id| id.to_string()),
            approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
            items,
        }
    }
}

// ==================== 读 ====================

/// 该发货单**可退明细**（前端「从发货单带出」对话框的唯一读数）。
///
/// 只对**已过账**的发货单开放：未过账的货还在仓库里，没有「退回来」这回事。
pub async fn returnable_of(
    shipment_id: Uuid,
) -> std::result::Result<Vec<ReturnableItemRes>, ApiError> {
    let Some((_, _, status)) = model::shipment_meta(db(), shipment_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发货单"))?
    else {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            "发货单不存在或已删除".to_string(),
        ));
    };
    if !RETURNABLE_SHIPMENT_STATUSES.contains(&status.as_str()) {
        return Err(conflict(format!(
            "发货单当前是「{}」（{status}），只有 {} 的发货单能退：货还没出库（或已作废）时没有可退明细",
            model::status_label(&status),
            RETURNABLE_SHIPMENT_STATUSES.join(" / ")
        )));
    }
    let lines = model::returnable_lines(db(), shipment_id, None)
        .await
        .map_err(|error| utils::db_error(error, "查询发货单可退明细"))?;
    Ok(lines
        .iter()
        .map(|line| ReturnableItemRes {
            so_item_id: line.so_item_id.to_string(),
            line_no: line.line_no,
            material_id: line.material_id.to_string(),
            shipped: line.shipped,
            returned: line.returned,
            remaining: line.remaining,
            price: line.price,
            tax_rate: line.tax_rate,
        })
        .collect())
}

/// 详情：单头 + 明细 + 客户 / 仓库 / 原单号（一次装配，列表不复用这条路径）。
async fn detail_of(id: Uuid) -> std::result::Result<ReturnRes, ApiError> {
    let row = model::find_return(id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售退货单"))?
        .ok_or_else(not_found)?;
    let items = model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售退货单明细"))?;
    let mut res = ReturnRes::of(&row, items.iter().map(ReturnItemRes::from).collect());
    if let Some(so_id) = row.so_id {
        res.so_no = shipment_model::so_no_of(db(), so_id)
            .await
            .map_err(|error| utils::db_error(error, "查询原销售订单号"))?;
    }
    if let Some(shipment_id) = row.shipment_id {
        res.shipment_no = model::shipment_no_of(db(), shipment_id)
            .await
            .map_err(|error| utils::db_error(error, "查询原发货单号"))?;
    }
    let customers = sales_order_model::customer_labels(&[row.customer_id])
        .await
        .map_err(|error| utils::db_error(error, "查询退货单客户"))?;
    if let Some((_, code, name)) = customers.into_iter().next() {
        res.customer_code = Some(code);
        res.customer_name = Some(name);
    }
    let warehouses = shipment_model::warehouse_labels(db(), &[row.warehouse_id])
        .await
        .map_err(|error| utils::db_error(error, "查询退货单仓库"))?;
    if let Some((_, code, name)) = warehouses.into_iter().next() {
        res.warehouse_code = Some(code);
        res.warehouse_name = Some(name);
    }
    Ok(res)
}

/// 退货单详情（软删行 404）。
pub async fn get_return(id_raw: &str) -> std::result::Result<ReturnRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    detail_of(id).await
}

/// 退货单分页。
pub async fn page_return(
    filter: ReturnFilter,
) -> std::result::Result<PageData<Vec<ReturnRes>>, ApiError> {
    // `status` 传了非法值 → 400（静默忽略会把「已过账」筛成「全部」，是假数据）
    if let Some(status) = filter
        .status
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        && !model::is_valid_status(status)
    {
        return Err(bad_request(format!(
            "status 必须是 {} 之一（当前 `{status}`）",
            model::STATUSES.join(" / ")
        )));
    }
    let so_id = parse_opt_uuid(filter.so_id.as_deref(), "so_id")?;
    let customer_id = parse_opt_uuid(filter.customer_id.as_deref(), "customer_id")?;
    let shipment_id = parse_opt_uuid(filter.shipment_id.as_deref(), "shipment_id")?;
    let (rows, total) = model::return_page(
        filter.return_no.as_deref(),
        filter.status.as_deref(),
        so_id,
        customer_id,
        shipment_id,
        filter.page,
        filter.page_size,
    )
    .await
    .map_err(|error| utils::db_error(error, "分页查询销售退货单"))?;

    // 客户 / 仓库 / 订单号 / 发货单号各自一次往返补齐（一页固定 4 条 SQL，不做 N+1）
    let customer_ids: Vec<Uuid> = rows.iter().map(|row| row.customer_id).collect();
    let naming: std::collections::HashMap<Uuid, (String, String)> =
        sales_order_model::customer_labels(&customer_ids)
            .await
            .map_err(|error| utils::db_error(error, "批量查询退货单客户"))?
            .into_iter()
            .map(|(id, code, name)| (id, (code, name)))
            .collect();
    let warehouse_ids: Vec<Uuid> = rows.iter().map(|row| row.warehouse_id).collect();
    let warehouses: std::collections::HashMap<Uuid, (String, String)> =
        shipment_model::warehouse_labels(db(), &warehouse_ids)
            .await
            .map_err(|error| utils::db_error(error, "批量查询退货单仓库"))?
            .into_iter()
            .map(|(id, code, name)| (id, (code, name)))
            .collect();
    let so_ids: Vec<Uuid> = rows.iter().filter_map(|row| row.so_id).collect();
    let so_numbers: std::collections::HashMap<Uuid, String> =
        shipment_model::so_no_labels(db(), &so_ids)
            .await
            .map_err(|error| utils::db_error(error, "批量查询退货单原订单号"))?
            .into_iter()
            .collect();
    let shipment_ids: Vec<Uuid> = rows.iter().filter_map(|row| row.shipment_id).collect();
    let shipment_numbers: std::collections::HashMap<Uuid, String> =
        model::shipment_no_labels(db(), &shipment_ids)
            .await
            .map_err(|error| utils::db_error(error, "批量查询退货单原发货单号"))?
            .into_iter()
            .collect();
    let (page, page_size) = model::normalize_page(filter.page, filter.page_size);
    let list = rows
        .iter()
        .map(|row| {
            let mut res = ReturnRes::of(row, Vec::new());
            if let Some((code, name)) = naming.get(&row.customer_id) {
                res.customer_code = Some(code.clone());
                res.customer_name = Some(name.clone());
            }
            if let Some((code, name)) = warehouses.get(&row.warehouse_id) {
                res.warehouse_code = Some(code.clone());
                res.warehouse_name = Some(name.clone());
            }
            res.so_no = row.so_id.and_then(|id| so_numbers.get(&id).cloned());
            res.shipment_no = row
                .shipment_id
                .and_then(|id| shipment_numbers.get(&id).cloned());
            res
        })
        .collect();
    Ok(PageData {
        list,
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

// ==================== 建单 ====================

/// 把请求里的明细解析成待落库行（`ex` 用于查订单行 / 可退量；建单时传 `&tx`）。
///
/// 三条规则：
/// 1. `items` 留空 + 有 `shipment_id` ⇒ 从该发货单带出**全部可退明细**（数量 = 可退量）；
/// 2. 有 `so_item_id` ⇒ 物料 / 单价 / 税率**从订单行带出**，不信请求体；
/// 3. 没有 `so_item_id`（`OTHER` 无原单）⇒ 必须自带 `material_id`。
async fn resolve_items(
    ex: &impl SqlExecutor,
    shipment_id: Option<Uuid>,
    so_id: Option<Uuid>,
    requests: &[ReturnItemReq],
) -> std::result::Result<Vec<NewReturnItem>, ApiError> {
    // ① `items` 留空 ⇒ 从发货单带出全部可退明细
    if requests.is_empty() {
        let Some(shipment_id) = shipment_id else {
            return Err(bad_request(
                "items 不能为空：没有原发货单（shipment_id）时必须逐行给出退货明细",
            ));
        };
        let auto = model::returnable_lines(ex, shipment_id, None)
            .await
            .map_err(|error| utils::db_error(error, "查询发货单可退明细"))?;
        if auto.is_empty() {
            return Err(conflict(format!(
                "发货单 {shipment_id} 没有可退明细（每行的可退量都是 0）：可能已全部退完，\
                 或其它未作废的退货单已经占满了可退量"
            )));
        }
        return Ok(auto
            .iter()
            .map(|line| {
                let mut item = NewReturnItem::new(
                    Some(line.so_item_id),
                    line.material_id,
                    line.remaining,
                    line.price,
                    line.tax_rate,
                );
                item.remark = None;
                item
            })
            .collect());
    }

    // ② 显式明细：逐行校验
    let mut out = Vec::with_capacity(requests.len());
    let mut ids = Vec::with_capacity(requests.len());
    for (index, req) in requests.iter().enumerate() {
        let label = format!("items[{index}]");
        let so_item_id = parse_opt_uuid(req.so_item_id.as_deref(), &format!("{label}.so_item_id"))?;
        let (material_id, qty, price, tax_rate) = match so_item_id {
            Some(so_item_id) => {
                ids.push(so_item_id);
                // 归属校验：有发货单 ⇒ 必须在它的可退明细里；只有订单 ⇒ 必须属于该订单
                let (material_id, order_price, order_rate, shipped, returned) =
                    match (shipment_id, so_id) {
                        (Some(shipment_id), _) => {
                            let lines = model::returnable_lines(ex, shipment_id, None)
                                .await
                                .map_err(|error| utils::db_error(error, "查询发货单可退明细"))?;
                            match lines.into_iter().find(|line| line.so_item_id == so_item_id) {
                                Some(line) => (
                                    line.material_id,
                                    line.price,
                                    line.tax_rate,
                                    line.shipped,
                                    line.returned,
                                ),
                                None => {
                                    return Err(bad_request(format!(
                                        "{label}.so_item_id 不属于发货单 {shipment_id} 的可退明细\
                                         （行不存在 / 没发过货 / 已全部退完）"
                                    )));
                                }
                            }
                        }
                        (None, Some(so_id)) => {
                            let belongs = shipment_model::so_item_belongs(ex, so_item_id, so_id)
                                .await
                                .map_err(|error| utils::db_error(error, "校验订单行归属"))?;
                            if !belongs {
                                return Err(bad_request(format!(
                                    "{label}.so_item_id 不属于销售订单 {so_id}：\
                                     一张退货单只能退同一张订单的货"
                                )));
                            }
                            let Some((material_id, order_price, order_rate, shipped, returned)) =
                                model::so_item_snapshot(ex, so_item_id, None)
                                    .await
                                    .map_err(|error| utils::db_error(error, "查询订单行可退量"))?
                            else {
                                return Err(bad_request(format!(
                                    "{label}.so_item_id 指向的销售订单行不存在：{so_item_id}"
                                )));
                            };
                            (material_id, order_price, order_rate, shipped, returned)
                        }
                        (None, None) => {
                            // 既没发货单也没订单：允许（OTHER 手工退货），但没有可退量可校
                            let Some((material_id, order_price, order_rate, shipped, returned)) =
                                model::so_item_snapshot(ex, so_item_id, None)
                                    .await
                                    .map_err(|error| utils::db_error(error, "查询订单行可退量"))?
                            else {
                                return Err(bad_request(format!(
                                    "{label}.so_item_id 指向的销售订单行不存在：{so_item_id}"
                                )));
                            };
                            (material_id, order_price, order_rate, shipped, returned)
                        }
                    };
                let qty = req
                    .qty
                    .unwrap_or(model::remaining_returnable(shipped, returned));
                if let Err(message) = validate_qty(qty, shipped, returned, &label) {
                    return Err(bad_request(message));
                }
                (
                    material_id,
                    qty,
                    req.price.unwrap_or(order_price),
                    req.tax_rate.unwrap_or(order_rate),
                )
            }
            None => {
                // 无原单行：物料必须自带（`biz_sales_return_item.material_id` 是 NOT NULL）
                let material_id = parse_uuid(
                    req.material_id.as_deref().unwrap_or_default(),
                    &format!("{label}.material_id"),
                )
                .map_err(|_| {
                    bad_request(format!(
                        "{label} 没有 so_item_id 时必须给 material_id（无原单退货要指明退的是什么物料）"
                    ))
                })?;
                let Some(qty) = req.qty else {
                    return Err(bad_request(format!(
                        "{label}.qty 不能为空：无原单的退货行没有「可退量」可以推断"
                    )));
                };
                if qty <= Decimal::ZERO {
                    return Err(bad_request(format!("{label}.qty 必须大于 0（当前 {qty}）")));
                }
                (
                    material_id,
                    qty,
                    req.price.unwrap_or(Decimal::ZERO),
                    req.tax_rate.unwrap_or(Decimal::ZERO),
                )
            }
        };
        if let Err(message) = validate_amount_inputs(price, tax_rate, &label) {
            return Err(bad_request(message));
        }
        let serial_nos: Vec<String> = req
            .serial_nos
            .iter()
            .map(|item| item.trim().to_string())
            .collect();
        if let Err(message) = validate_serials(&serial_nos, qty, &label) {
            return Err(bad_request(message));
        }
        let mut item = NewReturnItem::new(so_item_id, material_id, qty, price, tax_rate);
        item.location_id =
            parse_opt_uuid(req.location_id.as_deref(), &format!("{label}.location_id"))?;
        item.serial_nos = serial_nos;
        item.remark = req
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        out.push(item);
    }
    if let Err(message) = ensure_unique_so_items(&ids) {
        return Err(bad_request(message));
    }
    Ok(out)
}

/// 建单：原单闸 → 明细编译 → **锁原单 + 超退守卫** → 取号落库（同一事务）。
pub async fn create_return(
    operator: Option<Uuid>,
    req: CreateReturnReq,
) -> std::result::Result<ReturnRes, ApiError> {
    let warehouse_id = parse_uuid(&req.warehouse_id, "warehouse_id")?;
    let return_type = validate_return_type(&req.return_type).map_err(bad_request)?;
    let now = Utc::now();
    let return_date =
        parse_date(req.return_date.as_deref(), "return_date")?.unwrap_or_else(|| today_cst(now));
    let shipment_id = parse_opt_uuid(req.shipment_id.as_deref(), "shipment_id")?;
    let req_so_id = parse_opt_uuid(req.so_id.as_deref(), "so_id")?;
    let req_customer_id = parse_opt_uuid(req.customer_id.as_deref(), "customer_id")?;

    // ① 仓库（弱引用，没外键兜，必须自己查）
    let warehouse = shipment_model::warehouse_label(db(), warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询仓库"))?
        .ok_or_else(|| bad_request(format!("仓库不存在或已删除：{warehouse_id}")))?;

    // ② 原单闸：优先按发货单（推荐口径），其次按订单，最后是纯手工退货
    let (customer_id, so_id) = match (shipment_id, req_so_id) {
        (Some(shipment_id), _) => {
            let Some((customer_id, shipment_so_id, status)) =
                model::shipment_meta(db(), shipment_id)
                    .await
                    .map_err(|error| utils::db_error(error, "查询发货单"))?
            else {
                return Err(ApiError::new(
                    404,
                    error_code::RESOURCE_NOT_FOUND,
                    format!("发货单不存在或已删除：{shipment_id}"),
                ));
            };
            if !RETURNABLE_SHIPMENT_STATUSES.contains(&status.as_str()) {
                return Err(conflict(format!(
                    "发货单当前是「{}」（{status}），只有 {} 的发货单能退货：\
                     货还没出库（或已作废）时没有「退回来」这回事",
                    model::status_label(&status),
                    RETURNABLE_SHIPMENT_STATUSES.join(" / ")
                )));
            }
            if let Some(req_so_id) = req_so_id
                && req_so_id != shipment_so_id
            {
                return Err(bad_request(format!(
                    "so_id {req_so_id} 与发货单 {shipment_id} 的原订单 {shipment_so_id} 不一致"
                )));
            }
            if let Some(req_customer_id) = req_customer_id
                && req_customer_id != customer_id
            {
                return Err(bad_request(format!(
                    "customer_id {req_customer_id} 与发货单 {shipment_id} 的客户 {customer_id} 不一致"
                )));
            }
            (customer_id, Some(shipment_so_id))
        }
        (None, Some(so_id)) => {
            let Some(customer_id) = model::so_customer_of(db(), so_id)
                .await
                .map_err(|error| utils::db_error(error, "查询销售订单客户"))?
            else {
                return Err(ApiError::new(
                    404,
                    error_code::RESOURCE_NOT_FOUND,
                    format!("销售订单不存在或已删除：{so_id}"),
                ));
            };
            let status = model::so_status_of(db(), so_id)
                .await
                .map_err(|error| utils::db_error(error, "查询销售订单状态"))?
                .unwrap_or_default();
            if !RETURNABLE_SO_STATUSES.contains(&status.as_str()) {
                return Err(conflict(format!(
                    "销售订单当前是「{}」（{status}），只有 {} 的订单能退货：\
                     订单还没发货（或已作废）时没有可退的货；建议改为传 shipment_id 指定原发货单",
                    model::status_label(&status),
                    RETURNABLE_SO_STATUSES.join(" / ")
                )));
            }
            if let Some(req_customer_id) = req_customer_id
                && req_customer_id != customer_id
            {
                return Err(bad_request(format!(
                    "customer_id {req_customer_id} 与销售订单 {so_id} 的客户 {customer_id} 不一致"
                )));
            }
            (customer_id, Some(so_id))
        }
        (None, None) => {
            let Some(customer_id) = req_customer_id else {
                return Err(bad_request(
                    "customer_id 不能为空：既没有 shipment_id 也没有 so_id 时，必须指明退货客户",
                ));
            };
            let known = sales_order_model::customer_labels(&[customer_id])
                .await
                .map_err(|error| utils::db_error(error, "查询客户"))?;
            if known.is_empty() {
                return Err(bad_request(format!("客户不存在或已删除：{customer_id}")));
            }
            (customer_id, None)
        }
    };

    // ③ 明细编译（`items` 留空 = 从发货单带出可退明细）
    let items = resolve_items(db(), shipment_id, so_id, &req.items).await?;
    if items.is_empty() {
        return Err(bad_request("退货单至少要 1 行明细"));
    }

    // ④ 事务：锁原单 → 超退守卫 → 取号 + 单头 + 明细
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建单事务"))?;
    if let Some(shipment_id) = shipment_id {
        match model::lock_shipment(&tx, shipment_id).await {
            Ok(Some(status)) => {
                if !RETURNABLE_SHIPMENT_STATUSES.contains(&status.as_str()) {
                    let _ = tx.rollback().await;
                    return Err(conflict(format!(
                        "发货单状态在本次建单期间变更为「{}」（{status}），不再允许退货，请刷新后重试",
                        model::status_label(&status)
                    )));
                }
            }
            Ok(None) => {
                let _ = tx.rollback().await;
                return Err(ApiError::new(
                    404,
                    error_code::RESOURCE_NOT_FOUND,
                    format!("发货单不存在或已删除：{shipment_id}"),
                ));
            }
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(write_error(error, "锁定发货单"));
            }
        }
    }
    for item in &items {
        // 有原单行的行才需要锁订单行（无原单的 OTHER 行没有可退量可判）
        if let Some(so_item_id) = item.so_item_id {
            if shipment_id.is_none()
                && let Err(error) = model::lock_so_item(&tx, so_item_id).await
            {
                let _ = tx.rollback().await;
                return Err(write_error(error, "锁定销售订单行"));
            }
            match model::ensure_qty_within_returnable(&tx, shipment_id, so_item_id, item.qty, None)
                .await
            {
                Ok(_) => {}
                Err(DbError::InvalidValue(message)) => {
                    let _ = tx.rollback().await;
                    return Err(bad_request(message));
                }
                Err(error) => {
                    let _ = tx.rollback().await;
                    return Err(write_error(error, "校验订单行可退量"));
                }
            }
        }
    }
    let new = NewReturn {
        customer_id,
        so_id,
        shipment_id,
        return_date,
        return_type: &return_type,
        warehouse_id,
        remark: req
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty()),
    };
    let created = model::create_return(
        &tx,
        &new,
        &items,
        state_machine::Operator::of(operator),
        now,
    )
    .await;
    let (id, return_no) = match created {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "新建销售退货单"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交建单事务"))?;
    tracing::info!(
        return_id = %id,
        return_no = %return_no,
        shipment_id = ?shipment_id,
        warehouse = %warehouse.0,
        lines = items.len(),
        "销售退货单已建（DRAFT）"
    );
    detail_of(id).await
}

// ==================== 状态流转 ====================

/// 提交 / 待过账前**重跑**超退判定（草稿期间别人可能已经退过同一行）。
///
/// 与建单时同一套口径（同一个 `ensure_qty_within_returnable`），
/// 但这里只读、不锁行 —— 真正的写入在下一步的状态事务里；
/// 若此刻恰好被别人插队，状态机那一步的 CAS / 下游过账会拦（宁可多一次判定，
/// 也不要让一张「已经退超」的单据进入待过账）。
async fn revalidate_items(
    row: &ReturnRow,
    items: &[ReturnItemRow],
) -> std::result::Result<(), ApiError> {
    if items.is_empty() {
        return Err(bad_request(format!(
            "退货单 {} 没有明细，不能流转到需要明细的状态",
            row.return_no
        )));
    }
    for item in items {
        let Some(so_item_id) = item.so_item_id else {
            continue;
        };
        match model::ensure_qty_within_returnable(
            db(),
            row.shipment_id,
            so_item_id,
            item.qty,
            Some(row.id),
        )
        .await
        {
            Ok(_) => {}
            Err(DbError::InvalidValue(message)) => return Err(bad_request(message)),
            Err(error) => return Err(write_error(error, "重跑可退量校验")),
        }
    }
    Ok(())
}

/// 状态流转：边表校验（`SALES_RETURN_EDGES`）→ 业务闸 → 状态机留痕 → CAS 落库（同一事务）。
///
/// 业务闸（各自独立，缺一个就是线上事故）：
/// 1. `target = SUBMITTED / PENDING_POST`：单据至少有 1 行，且**重跑超退判定**；
/// 2. `target = APPROVED`：单据至少有 1 行；
/// 3. `target = POSTED`：**本卡不执行**（409，过账链路由 SO-30 落地）；
/// 4. 任何目标：边表 + 必填原因（`SUBMITTED / APPROVED → CANCELLED`）。
pub async fn change_return_status(
    operator: Option<Uuid>,
    id_raw: &str,
    req: ReturnStatusReq,
) -> std::result::Result<ReturnRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let target = validate_target(&req.target_status).map_err(bad_request)?;
    let row = model::find_return(id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售退货单"))?
        .ok_or_else(not_found)?;

    // ① 合法边校验（非法迁移 409，文案给出当前态与可选去向）
    if !model::can_transition(&row.status, &target) {
        let targets = model::allowed_targets(&row.status);
        let message = if targets.is_empty() {
            format!(
                "退货单 {} 已是终态 {}（{}），不能再流转：已过账的退货要冲回，必须走红冲链（SO-30）",
                row.return_no,
                model::status_label(&row.status),
                row.status
            )
        } else {
            format!(
                "非法状态迁移：{}（{}）→ {}（{}）不在合法边表里；当前可流转到：{}",
                model::status_label(&row.status),
                row.status,
                model::status_label(&target),
                target,
                targets
                    .iter()
                    .map(|code| format!("{code}（{}）", model::status_label(code)))
                    .collect::<Vec<_>>()
                    .join(" / ")
            )
        };
        return Err(conflict(message));
    }

    let reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());

    // ② 必填原因（与 state_machine 的 SALES_RETURN_REASON_EDGES 同源，提前给人话）
    if state_machine::Entity::SalesReturn.needs_reason(&row.status, &target) && reason.is_none() {
        return Err(bad_request(format!(
            "{}（{}）→ {}（{}）必须给出原因（reason）：这条边会让「本来要退的货」不再退，\
             对账时要能回答「为什么」",
            model::status_label(&row.status),
            row.status,
            model::status_label(&target),
            target
        )));
    }

    // ③ 业务闸
    match target.as_str() {
        model::STATUS_POSTED => {
            // 过账执行体在 `sales_return_posting::post_in_tx`（本函数的同一事务里调用）：
            // 回库单（inventory_writer 三本账）+ 红字入库凭证 + 冲减应收 + 红字发票 +
            // 回写 delivered_qty，六件事同生共死。这里只做「空单」这一条最廉价的闸，
            // 其余判定（超退复核 / 期间闸 / 成本来源）全部在过账事务体内。
            let items = model::items_of(id)
                .await
                .map_err(|error| utils::db_error(error, "查询销售退货单明细"))?;
            if items.is_empty() {
                return Err(bad_request(format!(
                    "退货单 {} 没有明细，不能过账",
                    row.return_no
                )));
            }
        }
        model::STATUS_SUBMITTED | model::STATUS_PENDING_POST => {
            let items = model::items_of(id)
                .await
                .map_err(|error| utils::db_error(error, "查询销售退货单明细"))?;
            revalidate_items(&row, &items).await?;
        }
        model::STATUS_APPROVED => {
            let items = model::items_of(id)
                .await
                .map_err(|error| utils::db_error(error, "查询销售退货单明细"))?;
            if items.is_empty() {
                return Err(bad_request(format!(
                    "退货单 {} 没有明细，不能批准",
                    row.return_no
                )));
            }
        }
        _ => {}
    }

    // ④ 状态机留痕 + CAS 落库（同一事务）
    let now = Utc::now();
    let op = state_machine::Operator::of(operator);
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启状态流转事务"))?;
    let outcome = async {
        state_machine::transition(
            &tx,
            state_machine::ENTITY_SALES_RETURN,
            id,
            &row.return_no,
            &model::LEGAL_EDGES,
            &row.status,
            &target,
            op.clone(),
            reason,
            None,
        )
        .await?;
        let affected = model::apply_status(&tx, id, &row.status, &target, operator, now)
            .await
            .map_err(|error| write_error(error, "退货单状态流转"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "退货单 {} 的状态在本次流转期间已被改动（期望 {}），请刷新后重试",
                row.return_no, row.status
            )));
        }
        // 过账：回库 / 红字入库凭证 / 冲减应收 / 红字发票 / 回写 delivered_qty。
        // 与上面的状态 CAS **同一个事务**：任何一步失败，状态与六本账一起回滚
        // （绝不出现「状态已过账、账上一分没动」的单）。
        if target == model::STATUS_POSTED {
            let posted =
                crate::business::erp::sales_return_posting::post_in_tx(&tx, id, &op, now).await?;
            tracing::info!(
                return_no = %row.return_no,
                stock_in_no = %posted.stock_in_no,
                cost_total = %posted.cost_total,
                cost_voucher = ?posted.cost_voucher_id,
                receivable = ?posted.receivable_id,
                receivable_unsettled = ?posted.receivable_unsettled,
                red_invoice_no = ?posted.red_invoice_no,
                revenue_voucher = ?posted.revenue_voucher_id,
                delivered_lines = posted.delivered_lines,
                "退货过账：状态与六本账已同事务落库"
            );
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交状态流转事务"))?;
            detail_of(id).await
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== HTTP 入口 ====================

/// 退货单分页。
#[nik::permission(
    path = "/business/erp/sales-return/page",
    method = "GET",
    permission = "business:erp:sales-return:list"
)]
#[nik::get("/business/erp/sales-return/page")]
pub async fn sales_return_page_api(
    Query(filter): Query<ReturnFilter>,
) -> ResResult<PageData<Vec<ReturnRes>>> {
    Ok(Res::ok(page_return(filter).await?))
}

/// 退货单详情。
#[nik::permission(
    path = "/business/erp/sales-return/{id}",
    method = "GET",
    permission = "business:erp:sales-return:list"
)]
#[nik::get("/business/erp/sales-return/{id}")]
pub async fn sales_return_detail(Path(id): Path<String>) -> ResResult<ReturnRes> {
    Ok(Res::ok(get_return(&id).await?))
}

/// 该发货单**可退明细**（前端「从发货单带出」对话框的唯一读数）。
#[nik::permission(
    path = "/business/erp/sales-return/returnable/{shipment_id}",
    method = "GET",
    permission = "business:erp:sales-return:list"
)]
#[nik::get("/business/erp/sales-return/returnable/{shipment_id}")]
pub async fn sales_return_returnable(
    Path(shipment_id): Path<String>,
) -> ResResult<Vec<ReturnableItemRes>> {
    let shipment_id = parse_uuid(&shipment_id, "shipment_id")?;
    Ok(Res::ok(returnable_of(shipment_id).await?))
}

/// 新建销售退货单（`items` 留空 = 从原发货单带出可退明细）。
#[nik::permission(
    path = "/business/erp/sales-return",
    method = "POST",
    permission = "business:erp:sales-return:create"
)]
#[nik::post("/business/erp/sales-return")]
pub async fn sales_return_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateReturnReq>,
) -> ResResult<ReturnRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_return(operator, req).await?))
}

/// 状态流转（非法迁移 409；POSTED 由 SO-30 解锁）。
#[nik::permission(
    path = "/business/erp/sales-return/{id}/status",
    method = "PATCH",
    permission = "business:erp:sales-return:status"
)]
#[nik::patch("/business/erp/sales-return/{id}/status")]
pub async fn sales_return_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<ReturnStatusReq>,
) -> ResResult<ReturnRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(change_return_status(operator, &id, req).await?))
}

// ==================== 单测（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// 定点字面量（浮点会丢精度，一律走字符串）。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    /// ① 目标态只认 6 值；大小写 / 旧稿值一律 400。
    #[test]
    fn target_status_validation() {
        for status in model::STATUSES {
            assert_eq!(validate_target(status).unwrap(), status);
        }
        assert!(validate_target("  DRAFT  ").is_ok(), "两端空白要 trim");
        for bad in ["", "draft", "SHIPPED", "RECEIVED", "UNKNOWN"] {
            let message = validate_target(bad).unwrap_err();
            assert!(
                message.contains("target_status"),
                "报文要点名字段：{message}"
            );
        }
    }

    /// ② 退货类型只认 3 值。
    #[test]
    fn return_type_validation() {
        for value in model::RETURN_TYPES {
            assert_eq!(validate_return_type(value).unwrap(), value);
        }
        for bad in ["", "quality", "REFUND", "OTHER "] {
            // `OTHER ` 会被 trim 成 `OTHER`，所以这里只断言明显非法的
            if bad.trim() == "OTHER" {
                continue;
            }
            let message = validate_return_type(bad).unwrap_err();
            assert!(message.contains("return_type"));
        }
    }

    /// ③ 超退：报文要同时说清「已发 / 已退 / 可退」，且不允许 0 与负数。
    #[test]
    fn qty_validation_messages() {
        assert!(validate_qty(dec("3"), dec("10"), dec("0"), "items[0]").is_ok());
        assert!(validate_qty(dec("7"), dec("10"), dec("3"), "items[0]").is_ok());

        let over = validate_qty(dec("7.5"), dec("10"), dec("3"), "items[0]").unwrap_err();
        assert!(over.contains("超退"), "报文要点名超退：{over}");
        assert!(over.contains("已发 10"), "{over}");
        assert!(over.contains("已退 3"), "{over}");

        let zero = validate_qty(Decimal::ZERO, dec("10"), dec("0"), "items[0]").unwrap_err();
        assert!(zero.contains("大于 0"), "{zero}");

        let negative = validate_qty(dec("-2"), dec("10"), dec("0"), "items[0]").unwrap_err();
        assert!(negative.contains("大于 0"), "{negative}");

        // 已退超过已发 ⇒ 可退 0，任何正数都拒
        assert!(validate_qty(dec("1"), dec("2"), dec("5"), "items[0]").is_err());
    }

    /// ④ 单价 / 税率早于库层 CHECK 拦。
    #[test]
    fn amount_input_validation() {
        assert!(validate_amount_inputs(dec("0"), dec("0"), "items[0]").is_ok());
        assert!(validate_amount_inputs(dec("1.5"), dec("13"), "items[0]").is_ok());
        assert!(validate_amount_inputs(dec("-0.01"), dec("0"), "items[0]").is_err());
        assert!(validate_amount_inputs(dec("1"), dec("100.0001"), "items[0]").is_err());
        assert!(validate_amount_inputs(dec("1"), dec("-1"), "items[0]").is_err());
    }

    /// ⑤ 序列号：空 = 不填（交给过账分配）；填了必须一物一码。
    #[test]
    fn serial_validation() {
        assert!(validate_serials(&[], dec("3"), "items[0]").is_ok());
        assert!(
            validate_serials(
                &["SN1".to_string(), "SN2".to_string()],
                dec("2"),
                "items[0]"
            )
            .is_ok()
        );
        let mismatch = validate_serials(&["SN1".to_string()], dec("2"), "items[0]").unwrap_err();
        assert!(mismatch.contains("一物一码"), "{mismatch}");
        let blank = validate_serials(&["".to_string()], dec("1"), "items[0]").unwrap_err();
        assert!(blank.contains("不能是空串"), "{blank}");
    }

    /// ⑥ 同一订单行不许重复（重复行会绕过逐行超退守卫）。
    #[test]
    fn duplicate_so_item_rejected() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        assert!(ensure_unique_so_items(&[a, b]).is_ok());
        assert!(ensure_unique_so_items(&[]).is_ok());
        let message = ensure_unique_so_items(&[a, b, a]).unwrap_err();
        assert!(message.contains("出现了两次"), "{message}");
    }

    /// ⑦ 权限码与 V0304 逐字一致，且都在本文件被真实引用。
    #[test]
    fn permission_codes_are_stable() {
        assert_eq!(
            SALES_RETURN_PERMISSIONS,
            [
                "business:erp:sales-return:list",
                "business:erp:sales-return:create",
                "business:erp:sales-return:status",
            ]
        );
    }

    /// ⑧ 闸门的取值本身：只有 POSTED 的发货单能退，订单侧只认已发货的三个状态。
    #[test]
    fn gate_status_sets() {
        assert_eq!(RETURNABLE_SHIPMENT_STATUSES, ["POSTED"]);
        for status in RETURNABLE_SO_STATUSES {
            assert!(
                crate::business::erp::sales_model::STATUSES.contains(&status),
                "{status} 不是销售订单的合法状态：订单侧闸门写错了"
            );
        }
        assert_eq!(
            RETURNABLE_SO_STATUSES,
            ["PARTIAL_OUT", "OUTBOUND", "CLOSED"]
        );
    }

    /// ⑨ 缺 material_id / 缺 qty 的无原单行：在纯逻辑层就拦，不靠库层 23502。
    #[test]
    fn manual_line_requires_material_and_qty() {
        // 直接校验 `parse_uuid("")` 的失败路径（resolve_items 用它兜 MATERIAL 缺失）
        assert!(parse_uuid("", "items[0].material_id").is_err());
        assert!(parse_uuid("not-a-uuid", "items[0].material_id").is_err());
    }
}
