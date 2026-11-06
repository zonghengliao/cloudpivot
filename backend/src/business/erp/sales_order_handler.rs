//! 销售订单接口层（销售链 · SO-05）：建单 / 改单 / 详情 / 分页 / 状态流转。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/sales-order/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/sales-order` | `business:erp:sale-order:create` | 建单（`DRAFT`，取号 `SO-yyyyMMdd-000001`） |
//! | PUT | `/business/erp/sales-order/{id}` | `business:erp:sale-order:update` | 改单（**只有 DRAFT**，明细整单替换） |
//! | GET | `/business/erp/sales-order/{id}` | `business:erp:sale-order:list` | 详情（含明细 + 客户编码/名称） |
//! | GET | `/business/erp/sales-order/page` | `business:erp:sale-order:list` | 分页（`so_no` 模糊 / `status` 精确 / `customer_id` 精确） |
//! | PATCH | `/business/erp/sales-order/{id}/status` | `business:erp:sale-order:status` | 状态流转（**只走 `LEGAL_EDGES` 的 10 条边**，其余 409） |
//!
//! # 状态机（唯一真相：`state_machine::SALES_ORDER_EDGES`）
//!
//! ```text
//! DRAFT → SUBMITTED → APPROVED → PARTIAL_OUT → OUTBOUND
//!                        └→ OUTBOUND（一次发完）   └→ CLOSED（剩余量作废）
//!                        └→ CLOSED（未发货直接关闭）
//! DRAFT / SUBMITTED / APPROVED → CANCELLED
//! ```
//!
//! - 非法迁移一律 **409 + 可选去向**（不是 500、也不是静默忽略）；
//! - 需要原因的边（提交后 / 审批后作废、两种关闭）缺 `reason` → **400**；
//! - 流转落库走两道：`state_machine::transition`（边表校验 + 写 `biz_status_log`）→
//!   `sales_order_model::apply_status`（CAS `WHERE status = from`），同一事务。
//! - `PARTIAL_OUT` / `OUTBOUND` 正常由**出库过账**回写（S3-05 卡），本卡也允许持有
//!   `sale-order:status` 权限的人手工推（联调 / 导入历史单场景），边表是同一张。
//!
//! # 提交时（`DRAFT → SUBMITTED`）的三道守卫（卡面要求）
//!
//! 1. **至少 1 行明细**（0 行明细的订单提交上去，审批人批的是一张空单）；
//! 2. **客户未冻结**（`biz_customer.status = 'ACTIVE'`；`customer_id` 是弱引用，
//!    没有外键能兜这件事，必须自己查 —— SO-04 交接的第 ③ 条）；
//! 3. **信用额度未超**（`= 客户 limit 0` 表示不控额度；否则
//!    「在手订单占用量 + 本单价税合计 ≤ limit」，占用口径见
//!    `sales_order_model::CREDIT_OCCUPYING_STATUSES`）。
//!
//! 建单 / 改单时也会校验「客户存在且 ACTIVE」（冻结 = 禁开新单，SO-02 口径），
//! 提交时**再查一次**：草稿期间客户可能被冻结。
//!
//! # 不碰的东西
//!
//! - **不写库存三本账**：销售订单不产生库存变动，出库过账走
//!   `inventory_writer`（唯一写入口），本卡一行库存 SQL 都没有；
//! - **不生成凭证**：销售凭证由出库 / 开票卡负责（`biz_voucher` 复用，不在这里造）；
//! - **不调联动引擎**：本卡把 `SALE_ORDER` 登记进联动白名单的**豁免清单**
//!   （`flow::linkage::spec::TRIGGER_EXEMPT`），出库过账卡接上引擎后再移出。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_sales_order / biz_sales_order_item 是 ERP 单据表（V0264 无 dept_id /
//! 归属人列），数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:sale-order:*
//! 权限码控制。分页走 sales_order_model::order_page → page_of → paginate_unscoped。
//! ```

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::customer_model::{self, STATUS_ACTIVE};
use crate::business::erp::sales_order_model::{
    self as model, NewItem, NewSalesOrder, SalesOrderItemRow, SalesOrderRow, Totals,
};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0265 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 / 详情。
pub const PERM_LIST: &str = "business:erp:sale-order:list";
/// 新建。
pub const PERM_CREATE: &str = "business:erp:sale-order:create";
/// 编辑。
pub const PERM_UPDATE: &str = "business:erp:sale-order:update";
/// 状态流转。
pub const PERM_STATUS: &str = "business:erp:sale-order:status";

/// 销售订单域全部权限码（顺序 = V0265 的 VALUES 顺序；`perm_menu_gate.py` 要求每个都在
/// `sys_menu` 有承载行）。
pub const SALES_ORDER_PERMISSIONS: [&str; 4] = [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_STATUS];

// ==================== 校验原语 ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `404`：销售订单不存在 / 已删。
fn not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "销售订单不存在或已删除".to_string(),
    )
}

/// `409`：状态 / 并发冲突（当前态与请求态不匹配）。
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

/// 可选日期（`yyyy-MM-dd`）：空白 → `None`；格式不对 → 400。
fn parse_date(raw: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| bad_request(format!("{field} 必须是 yyyy-MM-dd 格式（当前 `{text}`）"))),
    }
}

/// 写路径的数据库错误收口：唯一冲突 → 409；CHECK → 400；其余 500。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "销售订单号冲突（唯一约束{}）：取号器与库里的号撞了，请重试",
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
                "数据不满足数据库约束{}（数量 > 0 / 价税合计恒等 / 状态域 7 值）",
                constraint
                    .as_deref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default()
            ),
        ),
        _ => utils::db_error(err, ctx),
    }
}

// ==================== 契约 ====================

/// 分页筛选：`so_no` 模糊、`status` 精确、`customer_id` 精确。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SalesOrderFilter {
    /// 单号（模糊）。
    pub so_no: Option<String>,
    /// 状态（精确：7 值之一）。
    pub status: Option<String>,
    /// 客户 id（精确 UUID）。
    pub customer_id: Option<String>,
    /// 页码（从 1 起）。
    pub page: u64,
    /// 每页条数（0 → 默认 20，上限 200）。
    pub page_size: u64,
}

/// 一行明细入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SalesOrderItemReq {
    /// 物料 id（UUID；弱引用 `biz_material.id`）。
    pub material_id: String,
    /// 数量（> 0）。
    #[serde(default)]
    pub qty: Option<Decimal>,
    /// 不含税单价（≥ 0，留空 = 0）。
    #[serde(default)]
    pub price: Option<Decimal>,
    /// 税率百分数（0–100，留空 = 0）。
    #[serde(default)]
    pub tax_rate: Option<Decimal>,
}

/// 新建销售订单。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateSalesOrderReq {
    /// 客户 id（UUID；必须是未软删且 `ACTIVE` 的客户）。
    pub customer_id: String,
    /// 下单日期（`yyyy-MM-dd`，留空 = 今天）。
    #[serde(default)]
    pub order_date: Option<String>,
    /// 要求交期（`yyyy-MM-dd`，留空 = 未约定）。
    #[serde(default)]
    pub delivery_date: Option<String>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
    /// 明细（**至少 1 行**）。
    #[serde(default)]
    pub items: Vec<SalesOrderItemReq>,
}

/// 编辑销售订单（**整单覆盖式**：只允许 DRAFT；明细全量替换）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateSalesOrderReq {
    /// 客户 id（留空 = 不改）。
    #[serde(default)]
    pub customer_id: Option<String>,
    /// 下单日期（留空 = 不改）。
    #[serde(default)]
    pub order_date: Option<String>,
    /// 要求交期（传空串 = 清空）。
    #[serde(default)]
    pub delivery_date: Option<String>,
    /// 备注（传空串 = 清空）。
    #[serde(default)]
    pub remark: Option<String>,
    /// 明细（传了就整单替换；不传 = 明细不动）。
    #[serde(default)]
    pub items: Option<Vec<SalesOrderItemReq>>,
}

/// 状态流转请求。
#[derive(Debug, Clone, Deserialize)]
pub struct SalesOrderStatusReq {
    /// 目标状态（7 值之一，且必须是 `LEGAL_EDGES` 里的合法边）。
    pub target_status: String,
    /// 原因（需要原因的边必填：提交后 / 审批后作废、两种关闭）。
    #[serde(default)]
    pub reason: Option<String>,
}

/// 明细出参。
#[derive(Debug, Clone, Serialize)]
pub struct SalesOrderItemRes {
    /// 主键。
    pub id: String,
    /// 行号（1 起）。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 数量。
    pub qty: Decimal,
    /// 不含税单价。
    pub price: Decimal,
    /// 行不含税金额。
    pub amount: Decimal,
    /// 税率百分数。
    pub tax_rate: Decimal,
    /// 行税额。
    pub tax_amount: Decimal,
    /// 已发货数量。
    pub delivered_qty: Decimal,
    /// 未发货数量（`qty - delivered_qty`）。
    pub open_qty: Decimal,
}

impl From<&SalesOrderItemRow> for SalesOrderItemRes {
    fn from(row: &SalesOrderItemRow) -> Self {
        Self {
            id: row.id.to_string(),
            line_no: row.line_no,
            material_id: row.material_id.to_string(),
            qty: row.qty,
            price: row.price,
            amount: row.amount,
            tax_rate: row.tax_rate,
            tax_amount: row.tax_amount,
            delivered_qty: row.delivered_qty,
            open_qty: row.open_qty(),
        }
    }
}

/// 销售订单出参（详情 / 列表行同构）。
///
/// **列表行**：`items` 恒为空数组（别据此判断「没有明细」，明细只在详情里返回），
/// 但 `customer_code` / `customer_name` 会**批量补齐**（客户软删后仍为 `null`）。
#[derive(Debug, Clone, Serialize)]
pub struct SalesOrderRes {
    /// 主键。
    pub id: String,
    /// 单号。
    pub so_no: String,
    /// 客户 id。
    pub customer_id: String,
    /// 客户编码（客户软删后为 `null`）。
    pub customer_code: Option<String>,
    /// 客户名称（客户软删后为 `null`）。
    pub customer_name: Option<String>,
    /// 下单日期（`yyyy-MM-dd`）。
    pub order_date: String,
    /// 要求交期（`yyyy-MM-dd`，未约定为 `null`）。
    pub delivery_date: Option<String>,
    /// 状态编码。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 不含税合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计（= 上两项之和）。
    pub total_with_tax: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 明细（分页列表里恒为空）。
    pub items: Vec<SalesOrderItemRes>,
}

impl SalesOrderRes {
    /// 单头 → 出参（`items` 由调用方填）。
    fn of(row: &SalesOrderRow, items: Vec<SalesOrderItemRes>) -> Self {
        Self {
            id: row.id.to_string(),
            so_no: row.so_no.clone(),
            customer_id: row.customer_id.to_string(),
            customer_code: None,
            customer_name: None,
            order_date: row.order_date.to_string(),
            delivery_date: row.delivery_date.map(|date| date.to_string()),
            status: row.status.clone(),
            status_label: model::status_label(&row.status).to_string(),
            total_amount: row.total_amount,
            total_tax: row.total_tax,
            total_with_tax: row.total_with_tax,
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
            items,
        }
    }

    /// 补客户编码 / 名称（查不到 = 客户已软删，两列留 `None`）。
    fn with_customer(mut self, customer: Option<&customer_model::CustomerRow>) -> Self {
        if let Some(customer) = customer {
            self.customer_code = Some(customer.code.clone());
            self.customer_name = Some(customer.name.clone());
        }
        self
    }
}

// ==================== 入参 → 落库值（纯逻辑，单测直接调） ====================

/// 校验 + 归一化一行明细，算出金额。
fn build_item(req: &SalesOrderItemReq, index: usize) -> std::result::Result<NewItem, ApiError> {
    let label = format!("items[{index}]");
    let material_id = parse_uuid(&req.material_id, &format!("{label}.material_id"))?;
    let qty = req
        .qty
        .ok_or_else(|| bad_request(format!("{label}.qty 不能为空（数量必须 > 0）")))?;
    if qty <= Decimal::ZERO {
        return Err(bad_request(format!("{label}.qty 必须大于 0（当前 {qty}）")));
    }
    let price = req.price.unwrap_or(Decimal::ZERO);
    if price < Decimal::ZERO {
        return Err(bad_request(format!(
            "{label}.price 不能为负（当前 {price}）"
        )));
    }
    let tax_rate = req.tax_rate.unwrap_or(Decimal::ZERO);
    if tax_rate < Decimal::ZERO || tax_rate > Decimal::from(100) {
        return Err(bad_request(format!(
            "{label}.tax_rate 必须在 0–100 之间（当前 {tax_rate}，百分数）"
        )));
    }
    let (amount, tax_amount) = model::line_amounts(qty, price, tax_rate);
    Ok(NewItem {
        material_id,
        qty,
        price,
        tax_rate,
        amount,
        tax_amount,
    })
}

/// 校验 + 归一化整单入参：客户 id、日期、明细（**至少 1 行**）。
fn build_order<'a>(
    customer_id: &str,
    order_date: Option<&str>,
    delivery_date: Option<&str>,
    remark: Option<&'a str>,
    items: &[SalesOrderItemReq],
) -> std::result::Result<(NewSalesOrder<'a>, Vec<NewItem>), ApiError> {
    let customer_id = parse_uuid(customer_id, "customer_id")?;
    let order_date =
        parse_date(order_date, "order_date")?.unwrap_or_else(|| Utc::now().date_naive());
    let delivery_date = parse_date(delivery_date, "delivery_date")?;
    if let Some(delivery) = delivery_date
        && delivery < order_date
    {
        return Err(bad_request(format!(
            "delivery_date（{delivery}）不能早于 order_date（{order_date}）"
        )));
    }
    if items.is_empty() {
        return Err(bad_request(
            "销售订单至少要 1 行明细（明细为空的订单提交上去，审批人批的是一张空单）",
        ));
    }
    let built: Vec<NewItem> = items
        .iter()
        .enumerate()
        .map(|(index, item)| build_item(item, index))
        .collect::<std::result::Result<_, _>>()?;
    // 同行重复物料不拦（同一物料两行 = 两个交期 / 两个价格，是合理业务），
    // 但总金额必须自洽：这里再跑一次恒等守卫（漏了就是库层 23514 = 500 手感）
    let totals = Totals::of(&built);
    if !crate::business::erp::sales_model::totals_consistent(
        totals.amount,
        totals.tax,
        totals.with_tax,
    ) || !crate::business::erp::sales_model::amounts_non_negative(
        totals.amount,
        totals.tax,
        totals.with_tax,
    ) {
        return Err(bad_request(
            "价税合计不自洽（total_with_tax 必须等于 amount + tax，且三项都非负）",
        ));
    }
    Ok((
        NewSalesOrder {
            customer_id,
            order_date,
            delivery_date,
            remark: remark.map(str::trim).filter(|text| !text.is_empty()),
        },
        built,
    ))
}

// ==================== 业务逻辑（单测直接调这些） ====================

/// 取客户主档：不存在 / 已软删 → 404；冻结 → 409（冻结 = 禁开新单，SO-02 口径）。
async fn require_active_customer(
    customer_id: Uuid,
) -> std::result::Result<customer_model::CustomerRow, ApiError> {
    let row = customer_model::customer_find_by_id(customer_id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单客户"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                format!(
                    "客户 {customer_id} 不存在或已删除（customer_id 是弱引用，必须在建单前校验）"
                ),
            )
        })?;
    if row.status != STATUS_ACTIVE {
        return Err(conflict(format!(
            "客户 `{}`（{}）已冻结，不能开新销售订单；冻结客户的历史单据不受影响",
            row.code, row.name
        )));
    }
    Ok(row)
}

/// 详情（含明细 + 客户编码 / 名称）。
async fn detail_of(id: Uuid) -> std::result::Result<SalesOrderRes, ApiError> {
    let row = model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单"))?
        .ok_or_else(not_found)?;
    let lines = model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单明细"))?;
    let customer = customer_model::customer_find_by_id(row.customer_id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单客户"))?;
    let items = lines.iter().map(SalesOrderItemRes::from).collect();
    Ok(SalesOrderRes::of(&row, items).with_customer(customer.as_ref()))
}

/// 新建销售订单（`DRAFT`）。
pub async fn create_sales_order(
    operator: Option<Uuid>,
    req: CreateSalesOrderReq,
) -> std::result::Result<SalesOrderRes, ApiError> {
    let (new, items) = build_order(
        &req.customer_id,
        req.order_date.as_deref(),
        req.delivery_date.as_deref(),
        req.remark.as_deref(),
        &req.items,
    )?;
    let customer = require_active_customer(new.customer_id).await?;
    let totals = Totals::of(&items);
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建单事务"))?;
    let created = model::create_order(
        &tx,
        &new,
        &items,
        &totals,
        state_machine::Operator::of(operator),
        Utc::now(),
    )
    .await;
    let (id, so_no) = match created {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "新建销售订单"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交建单事务"))?;
    let mut res = detail_of(id).await?;
    // 建单时客户刚校验过，直接用查到的行，省一次往返（detail_of 已再查一遍，这里只覆盖空值）
    if res.customer_code.is_none() {
        res.customer_code = Some(customer.code.clone());
        res.customer_name = Some(customer.name.clone());
    }
    debug_assert_eq!(res.so_no, so_no, "回读单号必须与取号结果一致");
    Ok(res)
}

/// 编辑销售订单（**只有 DRAFT**；明细传了就整单替换）。
pub async fn update_sales_order(
    operator: Option<Uuid>,
    id_raw: &str,
    req: UpdateSalesOrderReq,
) -> std::result::Result<SalesOrderRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let current = model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑销售订单"))?
        .ok_or_else(not_found)?;
    if current.status != model::STATUS_DRAFT {
        return Err(conflict(format!(
            "只有草稿（DRAFT）能改：当前状态是 {}（{}）。已提交 / 已审批的单据要改，\
             请先作废再重建（销售订单没有「打回草稿」的边）",
            model::status_label(&current.status),
            current.status
        )));
    }
    // --- 合并「部分更新」到整单值（未传的字段沿用当前值） ---
    let customer_id = match req.customer_id.as_deref() {
        Some(raw) => parse_uuid(raw, "customer_id")?,
        None => current.customer_id,
    };
    let order_date =
        parse_date(req.order_date.as_deref(), "order_date")?.unwrap_or(current.order_date);
    let delivery_date = match req.delivery_date.as_deref() {
        // 传空串 = 清空交期
        Some(text) if text.trim().is_empty() => None,
        Some(_) => parse_date(req.delivery_date.as_deref(), "delivery_date")?,
        None => current.delivery_date,
    };
    if let Some(delivery) = delivery_date
        && delivery < order_date
    {
        return Err(bad_request(format!(
            "delivery_date（{delivery}）不能早于 order_date（{order_date}）"
        )));
    }
    // 传空串 = 清空备注；不传 = 沿用
    let remark: Option<String> = match req.remark.as_deref() {
        Some(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
        None => current.remark.clone(),
    };
    // 明细：不传 = 沿用库里现有明细（金额沿用库里已算好的值）；传了就整单替换
    let items: Vec<NewItem> = match req.items.as_ref() {
        None => {
            let lines = model::items_of(id)
                .await
                .map_err(|error| utils::db_error(error, "查询销售订单明细"))?;
            lines
                .iter()
                .map(|line| NewItem {
                    material_id: line.material_id,
                    qty: line.qty,
                    price: line.price,
                    tax_rate: line.tax_rate,
                    amount: line.amount,
                    tax_amount: line.tax_amount,
                })
                .collect()
        }
        Some(request_items) => {
            if request_items.is_empty() {
                return Err(bad_request("改单不能把明细清空：销售订单至少要 1 行明细"));
            }
            request_items
                .iter()
                .enumerate()
                .map(|(index, item)| build_item(item, index))
                .collect::<std::result::Result<_, _>>()?
        }
    };
    if customer_id != current.customer_id {
        // 换客户也要过「存在且未冻结」这一关（冻结客户禁开新单）
        require_active_customer(customer_id).await?;
    }
    let new = NewSalesOrder {
        customer_id,
        order_date,
        delivery_date,
        remark: remark.as_deref(),
    };
    let totals = Totals::of(&items);
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改单事务"))?;
    let affected = model::update_order(&tx, id, &new, &items, &totals, operator, Utc::now()).await;
    let affected = match affected {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "编辑销售订单"));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(conflict(
            "销售订单状态在本次改单期间已被改动（不再处于草稿），请刷新后重试",
        ));
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交改单事务"))?;
    detail_of(id).await
}

/// 销售订单详情（软删行 404）。
pub async fn get_sales_order(id_raw: &str) -> std::result::Result<SalesOrderRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    detail_of(id).await
}

/// 销售订单分页。
pub async fn page_sales_order(
    filter: SalesOrderFilter,
) -> std::result::Result<PageData<Vec<SalesOrderRes>>, ApiError> {
    // `status` 传了非法值 → 400（静默忽略会把「已审批」筛成「全部」，是假数据）
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
    let customer_id = match filter
        .customer_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => None,
        Some(raw) => Some(parse_uuid(raw, "customer_id")?),
    };
    let (rows, total) = model::order_page(
        filter.so_no.as_deref(),
        filter.status.as_deref(),
        customer_id,
        filter.page,
        filter.page_size,
    )
    .await
    .map_err(|error| utils::db_error(error, "分页查询销售订单"))?;
    // 客户编码 / 名称一次往返补齐（一页里多张单常常同客户；软删客户 → 两列 None）
    let customer_ids: Vec<Uuid> = rows.iter().map(|row| row.customer_id).collect();
    let labels = model::customer_labels(&customer_ids)
        .await
        .map_err(|error| utils::db_error(error, "批量查询销售订单客户"))?;
    let naming: std::collections::HashMap<Uuid, (String, String)> = labels
        .into_iter()
        .map(|(id, code, name)| (id, (code, name)))
        .collect();
    let (page, page_size) = model::normalize_page(filter.page, filter.page_size);
    let list = rows
        .iter()
        .map(|row| {
            let mut res = SalesOrderRes::of(row, Vec::new());
            if let Some((code, name)) = naming.get(&row.customer_id) {
                res.customer_code = Some(code.clone());
                res.customer_name = Some(name.clone());
            }
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

/// 提交前的三道守卫（卡面要求）：明细 / 客户未冻结 / 信用额度未超。
async fn ensure_submittable(
    row: &SalesOrderRow,
    lines: &[SalesOrderItemRow],
) -> std::result::Result<(), ApiError> {
    if lines.is_empty() {
        return Err(bad_request(format!(
            "销售订单 {} 没有明细，不能提交：至少要有 1 行明细",
            row.so_no
        )));
    }
    let customer = require_active_customer(row.customer_id).await?;
    let limit = customer.credit_limit;
    if limit <= Decimal::ZERO {
        // 0 = 不控额度（与供应商 / 客户档案的口径一致）
        return Ok(());
    }
    let used = model::customer_open_credit(db(), row.customer_id, Some(row.id))
        .await
        .map_err(|error| utils::db_error(error, "统计客户在手订单信用占用"))?;
    let after = used + row.total_with_tax;
    if after > limit {
        return Err(conflict(format!(
            "客户 `{}`（{}）信用额度不足：额度 {limit}，在手订单已占 {used}，\
             本单 {so_no} 价税合计 {amount}，合计 {after} 超出 {over}；\
             请先收款 / 关闭旧单，或由管理员调高额度",
            customer.code,
            customer.name,
            so_no = row.so_no,
            amount = row.total_with_tax,
            over = after - limit
        )));
    }
    Ok(())
}

/// 状态流转：边表校验（`LEGAL_EDGES`）→ 提交守卫 → 状态机留痕 → CAS 落库（同一事务）。
pub async fn change_sales_order_status(
    operator: Option<Uuid>,
    id_raw: &str,
    req: SalesOrderStatusReq,
) -> std::result::Result<SalesOrderRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let target = req.target_status.trim().to_string();
    if !model::is_valid_status(&target) {
        return Err(bad_request(format!(
            "target_status 必须是 {} 之一（当前 `{}`）",
            model::STATUSES.join(" / "),
            req.target_status
        )));
    }
    let row = model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单"))?
        .ok_or_else(not_found)?;

    // ① 合法边校验（非法迁移 409，文案给出当前态与可选去向）
    if !model::can_transition(&row.status, &target) {
        let targets = model::allowed_targets(&row.status);
        let message = if targets.is_empty() {
            format!(
                "销售订单 {} 已是终态 {}（{}），不能再流转",
                row.so_no,
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

    // ② 需要原因的边（与 state_machine 的 SALES_ORDER_REASON_EDGES 同源，提前给人话）
    let needs_reason = state_machine::Entity::SaleOrder
        .reason_edges()
        .iter()
        .any(|(from, to)| *from == row.status.as_str() && *to == target.as_str());
    if needs_reason && reason.is_none() {
        return Err(bad_request(format!(
            "{}（{}）→ {}（{}）必须给出原因（reason）：这条边会影响客户额度占用 / 未发数量，\
             对账时要能回答「为什么」",
            model::status_label(&row.status),
            row.status,
            model::status_label(&target),
            target
        )));
    }

    // ③ 提交守卫（0 行明细 / 客户冻结 / 信用额度超）
    if target == model::STATUS_SUBMITTED {
        let lines = model::items_of(id)
            .await
            .map_err(|error| utils::db_error(error, "查询销售订单明细"))?;
        ensure_submittable(&row, &lines).await?;
    }

    // ④ 状态机留痕 + CAS 落库（同一事务）
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启状态流转事务"))?;
    if let Err(error) = state_machine::transition(
        &tx,
        state_machine::ENTITY_SALES_ORDER,
        id,
        &row.so_no,
        &model::LEGAL_EDGES,
        &row.status,
        &target,
        state_machine::Operator::of(operator),
        reason,
        None,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    let applied = model::apply_status(&tx, id, &row.status, &target, operator, now).await;
    match applied {
        Ok(0) => {
            let _ = tx.rollback().await;
            Err(conflict(format!(
                "销售订单 {} 的状态在本次流转期间已被改动（期望 {}），请刷新后重试",
                row.so_no, row.status
            )))
        }
        Ok(_) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交状态流转事务"))?;
            detail_of(id).await
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "销售订单状态流转"))
        }
    }
}

// ==================== HTTP 入口 ====================

/// 销售订单分页。
#[nik::permission(
    path = "/business/erp/sales-order/page",
    method = "GET",
    permission = "business:erp:sale-order:list"
)]
#[nik::get("/business/erp/sales-order/page")]
pub async fn sales_order_page(
    Query(filter): Query<SalesOrderFilter>,
) -> ResResult<PageData<Vec<SalesOrderRes>>> {
    Ok(Res::ok(page_sales_order(filter).await?))
}

/// 销售订单详情。
#[nik::permission(
    path = "/business/erp/sales-order/{id}",
    method = "GET",
    permission = "business:erp:sale-order:list"
)]
#[nik::get("/business/erp/sales-order/{id}")]
pub async fn sales_order_detail(Path(id): Path<String>) -> ResResult<SalesOrderRes> {
    Ok(Res::ok(get_sales_order(&id).await?))
}

/// 新建销售订单。
#[nik::permission(
    path = "/business/erp/sales-order",
    method = "POST",
    permission = "business:erp:sale-order:create"
)]
#[nik::post("/business/erp/sales-order")]
pub async fn sales_order_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateSalesOrderReq>,
) -> ResResult<SalesOrderRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_sales_order(operator, req).await?))
}

/// 编辑销售订单（只有 DRAFT）。
#[nik::permission(
    path = "/business/erp/sales-order/{id}",
    method = "PUT",
    permission = "business:erp:sale-order:update"
)]
#[nik::put("/business/erp/sales-order/{id}")]
pub async fn sales_order_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateSalesOrderReq>,
) -> ResResult<SalesOrderRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_sales_order(operator, &id, req).await?))
}

/// 状态流转（非法迁移 409）。
#[nik::permission(
    path = "/business/erp/sales-order/{id}/status",
    method = "PATCH",
    permission = "business:erp:sale-order:status"
)]
#[nik::patch("/business/erp/sales-order/{id}/status")]
pub async fn sales_order_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<SalesOrderStatusReq>,
) -> ResResult<SalesOrderRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(
        change_sales_order_status(operator, &id, req).await?,
    ))
}

/// 权限码自检：4 个码、无重复、前缀 `business:erp:sale-order:`（V0265 的菜单行逐字对应）。
pub fn all_permissions() -> Vec<&'static str> {
    SALES_ORDER_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    fn item_req(qty: &str, price: &str, rate: &str) -> SalesOrderItemReq {
        SalesOrderItemReq {
            material_id: "d5050000-0000-4000-8000-000000000001".to_string(),
            qty: Some(dec(qty)),
            price: Some(dec(price)),
            tax_rate: Some(dec(rate)),
        }
    }

    fn order_req(customer: &str, items: Vec<SalesOrderItemReq>) -> CreateSalesOrderReq {
        CreateSalesOrderReq {
            customer_id: customer.to_string(),
            order_date: Some("2026-11-06".to_string()),
            delivery_date: None,
            remark: None,
            items,
        }
    }

    const CUST: &str = "d5050000-0000-4000-8000-00000000c001";

    /// 入参校验（纯逻辑，不需要库）：明细必填 / 数量 > 0 / 单价非负 / 税率域 / 日期格式。
    #[test]
    fn create_request_validation_rejects_bad_input() {
        // ① 0 行明细
        let err =
            build_order(CUST, Some("2026-11-06"), None, None, &[]).expect_err("0 行明细必须被拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("至少"), "{}", err.msg);

        // ② 数量 <= 0
        let bad_qty = item_req("0", "10", "13");
        let err = build_order(CUST, None, None, None, &[bad_qty]).expect_err("qty=0 必须被拒");
        assert!(err.msg.contains("大于 0"), "{}", err.msg);
        // 负数数量
        let err = build_order(CUST, None, None, None, &[item_req("-1", "10", "13")])
            .expect_err("负数量必须被拒");
        assert!(err.msg.contains("大于 0"), "{}", err.msg);

        // ③ 单价为负
        let err = build_order(CUST, None, None, None, &[item_req("1", "-0.0001", "0")])
            .expect_err("负单价必须被拒");
        assert!(err.msg.contains("不能为负"), "{}", err.msg);

        // ④ 税率越界（> 100 / < 0）
        for rate in ["100.0001", "-1"] {
            let err =
                build_order(CUST, None, None, None, &[item_req("1", "10", rate)]).unwrap_err();
            assert!(err.msg.contains("0–100"), "税率 {rate}：{}", err.msg);
        }
        // 边界值 0 / 100 合法
        build_order(CUST, None, None, None, &[item_req("1", "10", "0")]).expect("0% 合法");
        build_order(CUST, None, None, None, &[item_req("1", "10", "100")]).expect("100% 合法");

        // ⑤ 客户 id 非法 UUID / 缺失
        let err = build_order("", None, None, None, &[item_req("1", "10", "0")])
            .expect_err("空客户 id 必须被拒");
        assert_eq!(err.code, error_code::ERP_FILTER_INVALID);
        let err = build_order("not-a-uuid", None, None, None, &[item_req("1", "10", "0")])
            .expect_err("非法客户 id 必须被拒");
        assert_eq!(err.code, error_code::ERP_FILTER_INVALID);

        // ⑥ 日期格式 / 交期早于下单日
        let err = build_order(
            CUST,
            Some("2026-13-01"),
            None,
            None,
            &[item_req("1", "10", "0")],
        )
        .expect_err("13 月必须被拒");
        assert!(err.msg.contains("yyyy-MM-dd"), "{}", err.msg);
        let err = build_order(
            CUST,
            Some("2026-11-06"),
            Some("2026-11-05"),
            None,
            &[item_req("1", "10", "0")],
        )
        .expect_err("交期早于下单日必须被拒");
        assert!(err.msg.contains("不能早于"), "{}", err.msg);
    }

    /// 金额由服务端算：客户端只给数量 / 单价 / 税率，金额三项恒等。
    #[test]
    fn amounts_are_computed_server_side() {
        let (new, items) = build_order(
            CUST,
            Some("2026-11-06"),
            Some("2026-11-20"),
            Some("  hello  "),
            &[item_req("3", "17.655", "13"), item_req("2", "100", "0")],
        )
        .expect("合法入参应通过");
        assert_eq!(new.order_date.to_string(), "2026-11-06");
        assert_eq!(
            new.delivery_date.map(|date| date.to_string()).as_deref(),
            Some("2026-11-20")
        );
        assert_eq!(new.remark, Some("hello"), "备注应 trim");
        let totals = Totals::of(&items);
        assert_eq!(totals.amount, dec("252.9650"));
        assert_eq!(totals.tax, dec("6.8855"));
        assert_eq!(totals.with_tax, dec("259.8505"));
        assert_eq!(
            totals.with_tax,
            totals.amount + totals.tax,
            "价税合计必须精确恒等（库里是精确 CHECK）"
        );
    }

    /// 分页 status 域外值 → 400（不静默忽略）。
    #[test]
    fn page_status_domain_is_checked_by_pure_rule() {
        for bad in ["SHIPPED", "PARTIAL_SHIPPED", "draft", ""] {
            assert!(
                !model::is_valid_status(bad.trim()),
                "`{bad}` 不应通过状态域校验"
            );
        }
        for good in model::STATUSES {
            assert!(model::is_valid_status(good));
        }
    }

    /// 权限码自检：4 个码、无重复、前缀 `business:erp:sale-order:`（V0265 的菜单行逐字对应）。
    #[test]
    fn permission_codes_are_unique_and_prefixed() {
        let codes = all_permissions();
        assert_eq!(codes.len(), 4);
        for (index, code) in codes.iter().enumerate() {
            assert!(
                code.starts_with("business:erp:sale-order:"),
                "权限码前缀不对：{code}"
            );
            assert!(!codes[index + 1..].contains(code), "权限码重复：{code}");
        }
    }

    // ---------- 真库用例（`ERP_DB_URL` 开关） ----------

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 夹具客户编码前缀（清场按它删；单号是取号器生成的，只能按客户 id 定位单据）。
    const CUSTOMER_CODE: &str = "SO5HT-CUST";
    const CUSTOMER_CODE_TIGHT: &str = "SO5HT-TIGHT";
    const CUSTOMER_CODE_FROZEN: &str = "SO5HT-FROZEN";

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

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    /// 清场：状态日志 → 单头（明细 CASCADE）→ 三个夹具客户。
    async fn cleanup() {
        let statements: [(&str, Vec<Value>); 3] = [
            (
                "DELETE FROM biz_status_log \
                  WHERE entity_type = 'SALE_ORDER' \
                    AND entity_id IN ( \
                          SELECT o.id FROM biz_sales_order o \
                            JOIN biz_customer c ON c.id = o.customer_id \
                           WHERE c.code LIKE 'SO5HT-%')",
                vec![],
            ),
            (
                "DELETE FROM biz_sales_order \
                  WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'SO5HT-%')",
                vec![],
            ),
            ("DELETE FROM biz_customer WHERE code LIKE 'SO5HT-%'", vec![]),
        ];
        for (sql, params) in statements {
            if let Err(err) = db().execute_with_params(sql, params).await {
                panic!("清理夹具失败：{err}\nSQL: {sql}");
            }
        }
    }

    /// 造一个夹具客户（`credit_limit` 由调用方给；`0` = 不控额度）。
    async fn fixture_customer(code: &str, credit_limit: &str, status: &str) -> Uuid {
        db().execute_with_params(
            "INSERT INTO biz_customer (id, code, name, credit_limit, status) \
             VALUES (gen_random_uuid(), ?, ?, ?::text::numeric, ?) ON CONFLICT DO NOTHING",
            vec![
                Value::from(code),
                Value::from(format!("SO-05 接口夹具 {code}")),
                Value::from(credit_limit),
                Value::from(status),
            ],
        )
        .await
        .expect("插夹具客户应成功");
        // 上一轮留在库里的同名行可能是旧额度：这里强制改成期望值（幂等）
        db().execute_with_params(
            "UPDATE biz_customer SET credit_limit = ?::text::numeric, status = ?, deleted_at = NULL \
              WHERE code = ?",
            vec![
                Value::from(credit_limit),
                Value::from(status),
                Value::from(code),
            ],
        )
        .await
        .expect("重置夹具客户应成功");
        db().scalar_with_params(
            "SELECT id FROM biz_customer WHERE code = ? AND deleted_at IS NULL",
            vec![Value::from(code)],
        )
        .await
        .expect("查夹具客户应成功")
    }

    /// 主链 + 非法迁移（**卡面验收点**）：真库里走完整 HTTP 业务路径。
    #[tokio::test]
    async fn status_machine_main_chain_and_illegal_transitions() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer(CUSTOMER_CODE, "10000", "ACTIVE").await;
        let created = create_sales_order(
            None,
            order_req(&customer.to_string(), vec![item_req("2", "100", "13")]),
        )
        .await
        .expect("建单应成功");
        assert_eq!(created.status, model::STATUS_DRAFT);
        assert!(created.so_no.starts_with("SO-"), "{}", created.so_no);
        assert_eq!(created.items.len(), 1);
        assert_eq!(created.total_with_tax, dec("226"), "2×100 + 13% 税 = 226");
        assert_eq!(
            created.customer_code.as_deref(),
            Some(CUSTOMER_CODE),
            "详情应带客户编码"
        );

        // ① 非法迁移：DRAFT → APPROVED（跳步）→ 409 + 可选去向
        let err = change_sales_order_status(
            None,
            &created.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_APPROVED.to_string(),
                reason: None,
            },
        )
        .await
        .expect_err("跳步必须被拒");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert_eq!(err.code, error_code::RESOURCE_CONFLICT);
        assert!(
            err.msg.contains("SUBMITTED") && err.msg.contains("CANCELLED"),
            "409 文案必须列出当前可流转的去向：{}",
            err.msg
        );

        // ② 域外目标态（技术架构旧稿的 SHIPPED）→ 400
        let err = change_sales_order_status(
            None,
            &created.id,
            SalesOrderStatusReq {
                target_status: "SHIPPED".to_string(),
                reason: None,
            },
        )
        .await
        .expect_err("SHIPPED 不在状态域内");
        assert_eq!(err.status, 400, "{}", err.msg);

        // ③ 主链：DRAFT → SUBMITTED → APPROVED → OUTBOUND
        let submitted = change_sales_order_status(
            None,
            &created.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_SUBMITTED.to_string(),
                reason: None,
            },
        )
        .await
        .expect("提交应成功");
        assert_eq!(submitted.status, model::STATUS_SUBMITTED);

        // ④ 提交后改单 → 409（只有草稿能改）
        let err = update_sales_order(
            None,
            &created.id,
            UpdateSalesOrderReq {
                remark: Some("改不动".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("非草稿改单必须被拒");
        assert_eq!(err.status, 409, "{}", err.msg);

        // ⑤ SUBMITTED → OUTBOUND（跳步）→ 409
        let err = change_sales_order_status(
            None,
            &created.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_OUTBOUND.to_string(),
                reason: None,
            },
        )
        .await
        .expect_err("未审批直接出库必须被拒");
        assert_eq!(err.status, 409, "{}", err.msg);

        let approved = change_sales_order_status(
            None,
            &created.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_APPROVED.to_string(),
                reason: None,
            },
        )
        .await
        .expect("审批通过应成功");
        assert_eq!(approved.status, model::STATUS_APPROVED);

        let shipped = change_sales_order_status(
            None,
            &created.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_OUTBOUND.to_string(),
                reason: None,
            },
        )
        .await
        .expect("出库应成功");
        assert_eq!(shipped.status, model::STATUS_OUTBOUND);

        // ⑥ 终态出边：OUTBOUND → CANCELLED → 409（终态文案）
        let err = change_sales_order_status(
            None,
            &created.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_CANCELLED.to_string(),
                reason: Some("试试".to_string()),
            },
        )
        .await
        .expect_err("终态不能出边");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("终态"), "{}", err.msg);

        // ⑦ 状态日志：DRAFT(null) + 3 条流转，共 4 行
        let logs: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_status_log WHERE entity_type = 'SALE_ORDER' AND entity_id = ?",
                vec![Value::from(Uuid::parse_str(&created.id).expect("id 合法"))],
            )
            .await
            .expect("数状态日志");
        assert_eq!(logs, 4, "建单 + 3 次流转 = 4 条状态日志");
        cleanup().await;
    }

    /// 取消路径 + 必须给原因：DRAFT→CANCELLED 不要原因；SUBMITTED→CANCELLED 缺原因 400。
    #[tokio::test]
    async fn cancel_requires_reason_after_submit() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer(CUSTOMER_CODE, "10000", "ACTIVE").await;
        let first = create_sales_order(
            None,
            order_req(&customer.to_string(), vec![item_req("1", "10", "0")]),
        )
        .await
        .expect("建单应成功");
        // 草稿作废：原因可选
        let cancelled = change_sales_order_status(
            None,
            &first.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_CANCELLED.to_string(),
                reason: None,
            },
        )
        .await
        .expect("草稿作废应成功");
        assert_eq!(cancelled.status, model::STATUS_CANCELLED);

        // 第二张：提交后作废，缺原因 → 400
        let second = create_sales_order(
            None,
            order_req(&customer.to_string(), vec![item_req("1", "10", "0")]),
        )
        .await
        .expect("建单应成功");
        change_sales_order_status(
            None,
            &second.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_SUBMITTED.to_string(),
                reason: None,
            },
        )
        .await
        .expect("提交应成功");
        let err = change_sales_order_status(
            None,
            &second.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_CANCELLED.to_string(),
                reason: None,
            },
        )
        .await
        .expect_err("提交后作废缺原因必须被拒");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("原因"), "{}", err.msg);

        // 给了原因就能作废
        let cancelled = change_sales_order_status(
            None,
            &second.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_CANCELLED.to_string(),
                reason: Some("客户改主意，转下月订单".to_string()),
            },
        )
        .await
        .expect("带原因的作废应成功");
        assert_eq!(cancelled.status, model::STATUS_CANCELLED);
        cleanup().await;
    }

    /// 提交守卫：0 行明细 / 客户冻结 / 信用额度超 —— 三条各自被拒。
    #[tokio::test]
    async fn submit_guards_reject_empty_items_frozen_customer_and_credit_overflow() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer(CUSTOMER_CODE, "1000", "ACTIVE").await;

        // ① 0 行明细：建单后手工清掉明细，再提交 → 400
        let no_lines = create_sales_order(
            None,
            order_req(&customer.to_string(), vec![item_req("1", "10", "0")]),
        )
        .await
        .expect("建单应成功");
        db().execute_with_params(
            "DELETE FROM biz_sales_order_item WHERE so_id = ?",
            vec![Value::from(Uuid::parse_str(&no_lines.id).expect("id"))],
        )
        .await
        .expect("删明细应成功");
        let err = change_sales_order_status(
            None,
            &no_lines.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_SUBMITTED.to_string(),
                reason: None,
            },
        )
        .await
        .expect_err("0 行明细不能提交");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("明细"), "{}", err.msg);

        // ② 客户冻结：建单时 ACTIVE，草稿期间被冻结 → 提交 409
        let frozen_later = create_sales_order(
            None,
            order_req(&customer.to_string(), vec![item_req("1", "10", "0")]),
        )
        .await
        .expect("建单应成功");
        db().execute_with_params(
            "UPDATE biz_customer SET status = 'FROZEN' WHERE id = ?",
            vec![Value::from(customer)],
        )
        .await
        .expect("冻结客户应成功");
        let err = change_sales_order_status(
            None,
            &frozen_later.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_SUBMITTED.to_string(),
                reason: None,
            },
        )
        .await
        .expect_err("冻结客户不能提交");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("冻结"), "{}", err.msg);
        // 冻结客户也不能建新单
        let err = create_sales_order(
            None,
            order_req(&customer.to_string(), vec![item_req("1", "10", "0")]),
        )
        .await
        .expect_err("冻结客户不能开新单");
        assert_eq!(err.status, 409, "{}", err.msg);
        // 复原
        db().execute_with_params(
            "UPDATE biz_customer SET status = 'ACTIVE' WHERE id = ?",
            vec![Value::from(customer)],
        )
        .await
        .expect("恢复客户应成功");

        // ③ 信用额度：额度 50，本单 113 → 超；额度 200 的另一客户 → 通过
        let tight = fixture_customer(CUSTOMER_CODE_TIGHT, "50", "ACTIVE").await;
        let over = create_sales_order(
            None,
            order_req(&tight.to_string(), vec![item_req("1", "100", "13")]),
        )
        .await
        .expect("建单应成功（建单不占额度）");
        let err = change_sales_order_status(
            None,
            &over.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_SUBMITTED.to_string(),
                reason: None,
            },
        )
        .await
        .expect_err("超额必须被拒");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("信用额度") && err.msg.contains("113"),
            "文案要给出数字：{}",
            err.msg
        );

        // 额度 0 = 不控额度：同一张单换个客户就能提交
        let unlimited = fixture_customer(CUSTOMER_CODE_FROZEN, "0", "ACTIVE").await;
        let ok = create_sales_order(
            None,
            order_req(&unlimited.to_string(), vec![item_req("1", "100", "13")]),
        )
        .await
        .expect("建单应成功");
        let submitted = change_sales_order_status(
            None,
            &ok.id,
            SalesOrderStatusReq {
                target_status: model::STATUS_SUBMITTED.to_string(),
                reason: None,
            },
        )
        .await
        .expect("额度 0（不控额度）应可通过");
        assert_eq!(submitted.status, model::STATUS_SUBMITTED);
        cleanup().await;
    }

    /// 改单 + 分页 + 详情：明细整单替换、金额重算、分页按单号 / 状态 / 客户过滤。
    #[tokio::test]
    async fn update_page_and_detail_work_end_to_end() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer(CUSTOMER_CODE, "10000", "ACTIVE").await;
        let created = create_sales_order(
            None,
            order_req(&customer.to_string(), vec![item_req("1", "10", "0")]),
        )
        .await
        .expect("建单应成功");
        assert_eq!(created.total_with_tax, dec("10"));

        let updated = update_sales_order(
            None,
            &created.id,
            UpdateSalesOrderReq {
                remark: Some("改单：两行".to_string()),
                items: Some(vec![item_req("2", "20", "13"), item_req("5", "1", "0")]),
                ..Default::default()
            },
        )
        .await
        .expect("改单应成功");
        assert_eq!(updated.items.len(), 2, "明细整单替换");
        assert_eq!(updated.remark.as_deref(), Some("改单：两行"));
        // 2×20 = 40 + 5.2 税 + 5 = 50.2
        assert_eq!(updated.total_amount, dec("45"));
        assert_eq!(updated.total_tax, dec("5.2"));
        assert_eq!(updated.total_with_tax, dec("50.2"));

        // 改单不能把明细清空
        let err = update_sales_order(
            None,
            &created.id,
            UpdateSalesOrderReq {
                items: Some(Vec::new()),
                ..Default::default()
            },
        )
        .await
        .expect_err("清空明细必须被拒");
        assert_eq!(err.status, 400, "{}", err.msg);

        // 分页：按单号精确、按状态、按客户
        let by_no = page_sales_order(SalesOrderFilter {
            so_no: Some(created.so_no.clone()),
            ..Default::default()
        })
        .await
        .expect("分页应成功");
        assert_eq!(by_no.total, 1);
        assert_eq!(by_no.list[0].so_no, created.so_no);
        assert!(by_no.list[0].items.is_empty(), "列表行不带明细（详情才带）");

        let by_customer = page_sales_order(SalesOrderFilter {
            customer_id: Some(customer.to_string()),
            ..Default::default()
        })
        .await
        .expect("按客户筛应成功");
        assert_eq!(by_customer.total, 1);
        assert_eq!(
            by_customer.list[0].customer_code.as_deref(),
            Some(CUSTOMER_CODE),
            "分页行必须批量补齐客户编码（前端列表要显示）"
        );
        assert_eq!(
            by_customer.list[0].customer_name.as_deref(),
            Some("SO-05 接口夹具 SO5HT-CUST"),
            "分页行必须补齐客户名称"
        );

        let by_status = page_sales_order(SalesOrderFilter {
            customer_id: Some(customer.to_string()),
            status: Some(model::STATUS_DRAFT.to_string()),
            ..Default::default()
        })
        .await
        .expect("按状态筛应成功");
        assert_eq!(by_status.total, 1);

        // 非法状态筛选 → 400（不静默忽略）
        let err = page_sales_order(SalesOrderFilter {
            status: Some("SHIPPED".to_string()),
            ..Default::default()
        })
        .await
        .expect_err("非法状态必须 400");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);

        // 详情 404：随机 id
        let err = get_sales_order(&Uuid::now_v7().to_string())
            .await
            .expect_err("不存在的单必须 404");
        assert_eq!(err.status, 404);
        cleanup().await;
    }
}
