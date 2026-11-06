//! 发货单接口层（销售链 · SO-09）：建单 / 详情 / 分页 / 状态流转 / 带出可发明细。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/shipment/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/shipment` | `business:erp:shipment:create` | 建单（`DRAFT`，取号 `SH-yyyyMMdd-000001`）；`items` 留空 = 从订单**带出未发完的行** |
//! | GET | `/business/erp/shipment/{id}` | `business:erp:shipment:list` | 详情（含明细 + 订单号 / 客户 / 仓库） |
//! | GET | `/business/erp/shipment/page` | `business:erp:shipment:list` | 分页（`ship_no` 模糊 / `status` 精确 / `so_id` / `customer_id`） |
//! | GET | `/business/erp/shipment/shippable/{so_id}` | `business:erp:shipment:list` | 该订单**未发完的行**（前端「从订单带出」对话框的唯一读数） |
//! | PATCH | `/business/erp/shipment/{id}/status` | `business:erp:shipment:status` | 状态流转（只走 `SHIPMENT_EDGES` 的 4 条边，其余 409） |
//!
//! # 状态机（唯一真相：`state_machine::SHIPMENT_EDGES`）
//!
//! ```text
//! DRAFT ──提交──▶ PENDING_POST ──过账──▶ POSTED
//!   │                 │
//!   └──草稿作废───────┴──待过账作废（要原因）──▶ CANCELLED
//! ```
//!
//! - 非法迁移一律 **409 + 可选去向**（不是 500、也不是静默忽略）；
//! - `PENDING_POST → CANCELLED` 缺 `reason` → **400**（这条边会释放已占用的未发量）；
//! - 落库走两道：`state_machine::transition`（边表 + reason + 写 `biz_status_log`）→
//!   `shipment_model::apply_status`（CAS `WHERE status = from`），同一事务。
//!
//! # 两道独立的闸（都不是摆设）
//!
//! | 闸 | 位置 | 挡什么 |
//! |----|------|--------|
//! | 订单状态闸 | [`create_shipment`] / 提交 | 给 `DRAFT`/`SUBMITTED` 的订单发货（还没批准就发货）；给 `OUTBOUND`/`CLOSED`/`CANCELLED` 的订单发货 |
//! | 超发闸 | `shipment_model::ensure_qty_within_remaining` | 累计发货量 > 订单行数量（含**其他未过账单的占用**） |
//! | 库存闸 | [`transition_to_posted`] | 过账时仓库不够量（逐行 `stockout_model::fifo_plan`，与销售出库过账同一口径） |
//!
//! # 过账的边界（SO-09 交 SO-10 时的那份分工，现由 [`super::shipment_posting`] 落地）
//!
//! SO-09 只把状态推到 `POSTED` + 过账前的**库存充足校验**（[`transition_to_posted`]，只读）；
//! 真正的库存效果在 SO-10：`POSTED` 之后、`tx.commit()` 之前调用
//! [`super::shipment_posting::post_effects`]，在同一事务里完成
//!
//! ```text
//! 生成销售出库单（biz_stock_out, source_type=SALES, out_type=OTHER）
//!   → stockout_handler::post_out_in_tx（负库存闸 / 三本账负向 / 成本结转凭证 / 联动）
//!   → 回填 biz_shipment.stock_out_id
//!   → biz_sales_order_item.delivered_qty += qty（超发闸在 SQL 里）
//!   → 销售订单 PARTIAL_OUT / OUTBOUND
//! ```
//!
//! 本文件仍然**一行库存写 SQL 都没有**：三本账的扣减全部由 `inventory_writer`（经出库过账内核）完成。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_shipment / biz_shipment_item 是 ERP 单据表（V0283 无 dept_id /
//! 归属人列），数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:shipment:*
//! 权限码控制。分页走 shipment_model::shipment_page → page_of → paginate_unscoped。
//! ```

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::sales_order_model;
use crate::business::erp::shipment_model::{
    self as model, NewShipment, NewShipmentItem, ShipmentItemRow, ShipmentRow,
};
use crate::business::erp::shipment_posting;
use crate::business::erp::stockout_handler;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0284 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 / 详情 / 可发明细。
pub const PERM_LIST: &str = "business:erp:shipment:list";
/// 新建。
pub const PERM_CREATE: &str = "business:erp:shipment:create";
/// 状态流转（提交 / 过账 / 作废）。
pub const PERM_STATUS: &str = "business:erp:shipment:status";

/// 发货单域全部权限码（顺序 = V0284 的 VALUES 顺序；`perm_menu_gate.py` 要求每个都在
/// `sys_menu` 有承载行）。
pub const SHIPMENT_PERMISSIONS: [&str; 3] = [PERM_LIST, PERM_CREATE, PERM_STATUS];

// ==================== 校验原语 ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `404`：发货单不存在 / 已删。
fn not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "发货单不存在或已删除".to_string(),
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
            .map_err(|_| bad_request(format!("{field} 必须是 yyyy-MM-dd 格式（当前 `{text}`）"))),
    }
}

/// 今天的**北京时间**日期（`ship_date` 默认值；UTC 直接取日期在东八区早上 8 点前会差一天）。
pub fn today_cst(now: DateTime<Utc>) -> NaiveDate {
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
                "发货单号冲突（唯一约束{}）：取号器与库里的号撞了，请重试",
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
                "数据不满足数据库约束{}（数量必须 > 0 / 合计 ≥ 0 / 状态域 4 值）",
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

/// 状态目标值校验：必须在 4 值域内。
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

/// 本行数量校验：`qty > 0` 且不超过订单行未发量。
///
/// 与库层 `ck_biz_shipment_item_qty`（只兜 `qty > 0`）互补：上界是跨表的，
/// 只能在这里判，报文要说清「还剩多少」。
pub fn validate_qty(
    qty: Decimal,
    remaining: Decimal,
    label: &str,
) -> std::result::Result<(), String> {
    if qty <= Decimal::ZERO {
        return Err(format!("{label}.qty 必须大于 0（当前 {qty}）"));
    }
    if qty > remaining {
        return Err(format!(
            "{label}.qty = {qty} 超过订单行未发货量 {remaining}（超发）"
        ));
    }
    Ok(())
}

/// 序列号条数校验：填了就必须**一物一码**（条数 = 数量），不填则交给过账按库存分配。
///
/// 不在这里强制「序列号物料必须填」：序列号主表是 SO-10 过账时的读物，
/// 本卡只拦「填了但与数量不符」这种当场就能发现的错。
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

/// 同一张发货单里同一订单行只能出现一次（重复行会把超发守卫绕过去：
/// 守卫逐行判「单行 ≤ 未发量」，两行各 6 在剩余 10 的行上都能过）。
pub fn ensure_unique_so_items(so_item_ids: &[Uuid]) -> std::result::Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for id in so_item_ids {
        if !seen.insert(*id) {
            return Err(format!(
                "订单行 {id} 在同一张发货单里出现了两次：请合并成一行（数量相加）"
            ));
        }
    }
    Ok(())
}

// ==================== 契约 ====================

/// 分页筛选：`ship_no` 模糊、`status` 精确、`so_id` / `customer_id` 精确。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ShipmentFilter {
    /// 单号（模糊）。
    pub ship_no: Option<String>,
    /// 状态（精确：4 值之一）。
    pub status: Option<String>,
    /// 来源销售订单 id（精确 UUID）。
    pub so_id: Option<String>,
    /// 客户 id（精确 UUID）。
    pub customer_id: Option<String>,
    /// 页码（从 1 起）。
    pub page: u64,
    /// 每页条数（0 → 默认 20，上限 200）。
    pub page_size: u64,
}

/// 一行明细入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShipmentItemReq {
    /// 对应的销售订单行（**必填**：物料由服务端从订单行带出，不信请求体）。
    pub so_item_id: String,
    /// 发货数量（留空 = 该行**全部未发量**）。
    #[serde(default)]
    pub qty: Option<Decimal>,
    /// 发货库位（留空 = 整仓，过账按 FIFO 选）。
    #[serde(default)]
    pub location_id: Option<String>,
    /// 序列号（序列号物料才填，条数必须等于数量）。
    #[serde(default)]
    pub serial_nos: Vec<String>,
    /// 行备注。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 新建发货单。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateShipmentReq {
    /// 来源销售订单（必须是 `APPROVED` / `PARTIAL_OUT`）。
    pub so_id: String,
    /// 发货仓库。
    pub warehouse_id: String,
    /// 发货日期（`yyyy-MM-dd`，留空 = 今天，北京时间）。
    #[serde(default)]
    pub ship_date: Option<String>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
    /// 明细（**留空 = 从订单带出全部未发完的行**，各行数量 = 该行未发量）。
    #[serde(default)]
    pub items: Vec<ShipmentItemReq>,
}

/// 状态流转请求。
#[derive(Debug, Clone, Deserialize)]
pub struct ShipmentStatusReq {
    /// 目标状态（4 值之一，且必须是 `SHIPMENT_EDGES` 里的合法边）。
    pub target_status: String,
    /// 原因（`PENDING_POST → CANCELLED` 必填）。
    #[serde(default)]
    pub reason: Option<String>,
}

/// 明细出参。
#[derive(Debug, Clone, Serialize)]
pub struct ShipmentItemRes {
    /// 主键。
    pub id: String,
    /// 订单行号（1 起；订单行已被物理删时为 0）。
    pub line_no: i32,
    /// 对应的销售订单行 id。
    pub so_item_id: String,
    /// 物料 id。
    pub material_id: String,
    /// 本次发货数量。
    pub qty: Decimal,
    /// 发货库位（`null` = 整仓 / FIFO）。
    pub location_id: Option<String>,
    /// 序列号。
    pub serial_nos: Vec<String>,
    /// 行备注。
    pub remark: Option<String>,
}

impl From<&ShipmentItemRow> for ShipmentItemRes {
    fn from(row: &ShipmentItemRow) -> Self {
        Self {
            id: row.id.to_string(),
            line_no: row.line_no,
            so_item_id: row.so_item_id.to_string(),
            material_id: row.material_id.to_string(),
            qty: row.qty,
            location_id: row.location_id.map(|id| id.to_string()),
            serial_nos: row.serial_nos.clone(),
            remark: row.remark.clone(),
        }
    }
}

/// 发货单出参（详情 / 列表行同构）。
///
/// **列表行**：`items` 恒为空数组（明细只在详情里返回），但 `so_no` /
/// `customer_code` / `warehouse_code` / `stock_out_no` 会**批量补齐**（各自一次
/// `IN (…)` 往返，见 [`page_shipment`]），一页固定 4 条补齐 SQL、不做 N+1 ——
/// 列表与详情**逐字段同口径**，前端列表可直接展示，不必为每一行再发一次详情请求。
/// 订单 / 出库单不存在或已软删时，对应单号为 `null`。
#[derive(Debug, Clone, Serialize)]
pub struct ShipmentRes {
    /// 主键。
    pub id: String,
    /// 单号。
    pub ship_no: String,
    /// 来源销售订单 id。
    pub so_id: String,
    /// 来源销售订单号（订单不存在或已软删时为 `null`；**列表与详情都下发**）。
    pub so_no: Option<String>,
    /// 客户 id。
    pub customer_id: String,
    /// 客户编码（客户软删后为 `null`）。
    pub customer_code: Option<String>,
    /// 客户名称（客户软删后为 `null`）。
    pub customer_name: Option<String>,
    /// 发货仓库 id。
    pub warehouse_id: String,
    /// 仓库编码（仓库软删后为 `null`）。
    pub warehouse_code: Option<String>,
    /// 仓库名称（仓库软删后为 `null`）。
    pub warehouse_name: Option<String>,
    /// 发货日期（`yyyy-MM-dd`）。
    pub ship_date: String,
    /// 状态编码。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 明细数量合计。
    pub total_qty: Decimal,
    /// 过账生成的销售出库单 id（未过账为 `null`，SO-10 回填）。
    pub stock_out_id: Option<String>,
    /// 过账生成的销售出库单号（`OUT-yyyyMMdd-000001`；未过账 / 出库单不存在或已软删时为 `null`）。
    ///
    /// SO-11 页面要显示「关联出库单号」：**列表与详情都下发**（列表按 `stock_out_id` 批量补齐），
    /// 前端不必再为一行列表补一次详情请求。
    pub stock_out_no: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 明细（分页列表里恒为空）。
    pub items: Vec<ShipmentItemRes>,
}

impl ShipmentRes {
    /// 单头 → 出参（`items` 由调用方填）。
    fn of(row: &ShipmentRow, items: Vec<ShipmentItemRes>) -> Self {
        Self {
            id: row.id.to_string(),
            ship_no: row.ship_no.clone(),
            so_id: row.so_id.to_string(),
            so_no: None,
            customer_id: row.customer_id.to_string(),
            customer_code: None,
            customer_name: None,
            warehouse_id: row.warehouse_id.to_string(),
            warehouse_code: None,
            warehouse_name: None,
            ship_date: row.ship_date.to_string(),
            status: row.status.clone(),
            status_label: model::status_label(&row.status).to_string(),
            total_qty: row.total_qty,
            stock_out_id: row.stock_out_id.map(|id| id.to_string()),
            stock_out_no: None,
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
            items,
        }
    }
}

/// 一行「可发明细」（`GET /shippable/{so_id}` 的出参，前端「从订单带出」对话框读数）。
#[derive(Debug, Clone, Serialize)]
pub struct ShippableItemRes {
    /// 订单行 id。
    pub so_item_id: String,
    /// 订单行号（1 起）。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 订单数量。
    pub ordered: Decimal,
    /// 已过账发货量。
    pub delivered: Decimal,
    /// 其他未过账发货单的占用。
    pub reserved: Decimal,
    /// 未发量（`ordered - delivered - reserved`，下限 0）。
    pub remaining: Decimal,
}

// ==================== 落库前的编译（纯逻辑 + 少量查询） ====================

/// 从订单带出**未发完的行**（卡面「只带未发完的行」的唯一取数口径）。
///
/// 直接调 `shipment_model::shippable_items`：剩余量 = 订单 − 已发 − 其他未过账占用，
/// 与建单时的超发守卫是**同一个公式**（两处口径不会漂）。
pub async fn shippable_of(so_id: Uuid) -> std::result::Result<Vec<ShippableItemRes>, ApiError> {
    if model::so_status_of(db(), so_id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单状态"))?
        .is_none()
    {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            "销售订单不存在或已删除".to_string(),
        ));
    }
    let rows = model::shippable_items(db(), so_id)
        .await
        .map_err(|error| utils::db_error(error, "查询订单可发明细"))?;
    Ok(rows
        .iter()
        .map(|row| ShippableItemRes {
            so_item_id: row.so_item_id.to_string(),
            line_no: row.line_no,
            material_id: row.material_id.to_string(),
            ordered: row.availability.ordered,
            delivered: row.availability.delivered,
            reserved: row.availability.reserved,
            remaining: row.availability.remaining,
        })
        .collect())
}

/// 把请求里的明细解析成待落库行（`ex` 用于查订单行 / 剩余量；建单时传 `&tx`）。
async fn resolve_items(
    ex: &impl SqlExecutor,
    so_id: Uuid,
    requests: &[ShipmentItemReq],
) -> std::result::Result<Vec<NewShipmentItem>, ApiError> {
    // ① `items` 留空 ⇒ 从订单带出全部未发完的行（数量 = 该行未发量）
    if requests.is_empty() {
        let auto = model::shippable_items(ex, so_id)
            .await
            .map_err(|error| utils::db_error(error, "查询订单可发明细"))?;
        if auto.is_empty() {
            return Err(conflict(format!(
                "销售订单 {so_id} 没有可发货的明细（每行的未发量都是 0）：\
                 可能已全部发货，或未发量被其他未过账的发货单占满了"
            )));
        }
        return Ok(auto
            .iter()
            .map(|row| NewShipmentItem {
                so_item_id: row.so_item_id,
                material_id: row.material_id,
                qty: row.availability.remaining,
                location_id: None,
                serial_nos: Vec::new(),
                remark: None,
            })
            .collect());
    }

    // ② 显式明细：逐行校验（物料从订单行带出，不信请求体）
    let mut out = Vec::with_capacity(requests.len());
    let mut ids = Vec::with_capacity(requests.len());
    for (index, req) in requests.iter().enumerate() {
        let label = format!("items[{index}]");
        let so_item_id = parse_uuid(&req.so_item_id, &format!("{label}.so_item_id"))?;
        ids.push(so_item_id);
        let Some((material_id, _line_no)) = model::so_item_brief(ex, so_item_id)
            .await
            .map_err(|error| utils::db_error(error, "查询销售订单行"))?
        else {
            return Err(bad_request(format!(
                "{label}.so_item_id 指向的销售订单行不存在：{so_item_id}"
            )));
        };
        let belongs = model::so_item_belongs(ex, so_item_id, so_id)
            .await
            .map_err(|error| utils::db_error(error, "校验订单行归属"))?;
        if !belongs {
            return Err(bad_request(format!(
                "{label}.so_item_id 不属于销售订单 {so_id}：一张发货单只能发同一张订单的货"
            )));
        }
        // 数量：留空 = 该行全部未发量（`availability_of` 与守卫同源）
        let availability = model::availability_of(ex, so_item_id, None)
            .await
            .map_err(|error| utils::db_error(error, "查询订单行未发量"))?;
        let qty = req.qty.unwrap_or(availability.remaining);
        if let Err(message) = validate_qty(qty, availability.remaining, &label) {
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
        out.push(NewShipmentItem {
            so_item_id,
            material_id,
            qty,
            location_id: parse_opt_uuid(
                req.location_id.as_deref(),
                &format!("{label}.location_id"),
            )?,
            serial_nos,
            remark: req
                .remark
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string),
        });
    }
    if let Err(message) = ensure_unique_so_items(&ids) {
        return Err(bad_request(message));
    }
    Ok(out)
}

/// 详情：单头 + 明细 + 订单号 / 客户 / 仓库（一次装配，列表不复用这条路径）。
async fn detail_of(id: Uuid) -> std::result::Result<ShipmentRes, ApiError> {
    let row = model::find_shipment(id)
        .await
        .map_err(|error| utils::db_error(error, "查询发货单"))?
        .ok_or_else(not_found)?;
    let items = model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询发货单明细"))?;
    let mut res = ShipmentRes::of(&row, items.iter().map(ShipmentItemRes::from).collect());
    res.so_no = model::so_no_of(db(), row.so_id)
        .await
        .map_err(|error| utils::db_error(error, "查询来源销售订单号"))?;
    // 关联出库单号（SO-10 过账后回填；SO-11 页面直接显示这个号）
    if let Some(stock_out_id) = row.stock_out_id {
        res.stock_out_no = model::stock_out_no_of(db(), stock_out_id)
            .await
            .map_err(|error| utils::db_error(error, "查询关联销售出库单号"))?;
    }
    let customers = sales_order_model::customer_labels(&[row.customer_id])
        .await
        .map_err(|error| utils::db_error(error, "查询发货单客户"))?;
    if let Some((_, code, name)) = customers.into_iter().next() {
        res.customer_code = Some(code);
        res.customer_name = Some(name);
    }
    let warehouses = model::warehouse_labels(db(), &[row.warehouse_id])
        .await
        .map_err(|error| utils::db_error(error, "查询发货单仓库"))?;
    if let Some((_, code, name)) = warehouses.into_iter().next() {
        res.warehouse_code = Some(code);
        res.warehouse_name = Some(name);
    }
    Ok(res)
}

// ==================== 建单 ====================

/// 建单：订单状态闸 → 明细编译 → **逐行锁订单行 + 超发守卫** → 取号落库（同一事务）。
pub async fn create_shipment(
    operator: Option<Uuid>,
    req: CreateShipmentReq,
) -> std::result::Result<ShipmentRes, ApiError> {
    let so_id = parse_uuid(&req.so_id, "so_id")?;
    let warehouse_id = parse_uuid(&req.warehouse_id, "warehouse_id")?;
    let now = Utc::now();
    let ship_date =
        parse_date(req.ship_date.as_deref(), "ship_date")?.unwrap_or_else(|| today_cst(now));

    // ① 订单状态闸：只有审批通过 / 部分出库的订单能发货
    let so_status = model::so_status_of(db(), so_id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单状态"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                "销售订单不存在或已删除".to_string(),
            )
        })?;
    if !model::SHIPPABLE_SO_STATUSES.contains(&so_status.as_str()) {
        return Err(conflict(format!(
            "销售订单当前是「{}」（{so_status}），只有 {} 的订单能发货：\
             订单还没审批通过（或已关闭 / 已作废 / 已发完）时不允许开发货单",
            so_status,
            model::SHIPPABLE_SO_STATUSES.join(" / ")
        )));
    }

    // ② 客户 / 仓库（弱引用，没外键兜，必须自己查）
    let customer_id: Uuid = {
        // SCOPE-EXEMPT: 销售订单，ERP 单据，无归属列。
        let rows = db()
            .query_sql(
                "SELECT customer_id FROM biz_sales_order WHERE id = ?::uuid".to_string(),
                vec![Value::from(so_id)],
            )
            .await
            .map_err(|error| utils::db_error(error, "查询销售订单客户"))?;
        rows.first()
            .and_then(|row| row.get_typed::<Uuid>("customer_id").ok())
            .ok_or_else(|| {
                ApiError::new(
                    404,
                    error_code::RESOURCE_NOT_FOUND,
                    "销售订单不存在或已删除".to_string(),
                )
            })?
    };
    let warehouse = model::warehouse_label(db(), warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询仓库"))?
        .ok_or_else(|| bad_request(format!("仓库不存在或已删除：{warehouse_id}")))?;

    // ③ 明细编译（`items` 留空 = 从订单带出未发完的行）
    let items = resolve_items(db(), so_id, &req.items).await?;
    if items.is_empty() {
        return Err(bad_request("发货单至少要 1 行明细"));
    }

    // ④ 事务：锁订单行 → 超发守卫 → 取号 + 单头 + 明细
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建单事务"))?;
    let mut guarded = Vec::with_capacity(items.len());
    for item in &items {
        // 锁行必须在判定之前：两个并发建单只做「读 → 判 → 写」会双双通过
        if let Err(error) = model::lock_so_item(&tx, item.so_item_id).await {
            let _ = tx.rollback().await;
            return Err(write_error(error, "锁定销售订单行"));
        }
        match model::ensure_qty_within_remaining(&tx, item.so_item_id, item.qty, None).await {
            Ok(snapshot) => guarded.push(snapshot),
            Err(DbError::InvalidValue(message)) => {
                let _ = tx.rollback().await;
                return Err(bad_request(message));
            }
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(write_error(error, "校验订单行未发量"));
            }
        }
    }
    let new = NewShipment {
        so_id,
        customer_id,
        warehouse_id,
        ship_date,
        remark: req
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty()),
    };
    let created = model::create_shipment(
        &tx,
        &new,
        &items,
        state_machine::Operator::of(operator),
        now,
    )
    .await;
    let (id, ship_no) = match created {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "新建发货单"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交建单事务"))?;
    tracing::info!(
        shipment_id = %id,
        ship_no = %ship_no,
        so_id = %so_id,
        warehouse = %warehouse.0,
        lines = items.len(),
        "发货单已建（DRAFT）"
    );
    detail_of(id).await
}

// ==================== 读 ====================

/// 发货单详情（软删行 404）。
pub async fn get_shipment(id_raw: &str) -> std::result::Result<ShipmentRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    detail_of(id).await
}

/// 发货单分页。
pub async fn page_shipment(
    filter: ShipmentFilter,
) -> std::result::Result<PageData<Vec<ShipmentRes>>, ApiError> {
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
    let (rows, total) = model::shipment_page(
        filter.ship_no.as_deref(),
        filter.status.as_deref(),
        so_id,
        customer_id,
        filter.page,
        filter.page_size,
    )
    .await
    .map_err(|error| utils::db_error(error, "分页查询发货单"))?;

    // 客户 / 仓库 / 订单号 / 出库单号各自一次往返补齐（一页里多张单常常同客户同仓库，
    // 订单号与出库单号则按行去重后再批量查）—— 一页固定 4 条 SQL，不做 N+1。
    let customer_ids: Vec<Uuid> = rows.iter().map(|row| row.customer_id).collect();
    let naming: std::collections::HashMap<Uuid, (String, String)> =
        sales_order_model::customer_labels(&customer_ids)
            .await
            .map_err(|error| utils::db_error(error, "批量查询发货单客户"))?
            .into_iter()
            .map(|(id, code, name)| (id, (code, name)))
            .collect();
    let warehouse_ids: Vec<Uuid> = rows.iter().map(|row| row.warehouse_id).collect();
    let warehouses: std::collections::HashMap<Uuid, (String, String)> =
        model::warehouse_labels(db(), &warehouse_ids)
            .await
            .map_err(|error| utils::db_error(error, "批量查询发货单仓库"))?
            .into_iter()
            .map(|(id, code, name)| (id, (code, name)))
            .collect();
    // 来源销售订单号（t_45900ea7：列表缺这一列，与 `ShipmentRes` 注释矛盾）
    let so_ids: Vec<Uuid> = rows.iter().map(|row| row.so_id).collect();
    let so_numbers: std::collections::HashMap<Uuid, String> = model::so_no_labels(db(), &so_ids)
        .await
        .map_err(|error| utils::db_error(error, "批量查询发货单来源订单号"))?
        .into_iter()
        .collect();
    // 关联销售出库单号（只对已过账的行查；未过账 = 没有 stock_out_id = 保持 None）
    let stock_out_ids: Vec<Uuid> = rows.iter().filter_map(|row| row.stock_out_id).collect();
    let stock_out_numbers: std::collections::HashMap<Uuid, String> =
        model::stock_out_no_labels(db(), &stock_out_ids)
            .await
            .map_err(|error| utils::db_error(error, "批量查询发货单关联出库单号"))?
            .into_iter()
            .collect();
    let (page, page_size) = model::normalize_page(filter.page, filter.page_size);
    let list = rows
        .iter()
        .map(|row| {
            let mut res = ShipmentRes::of(row, Vec::new());
            if let Some((code, name)) = naming.get(&row.customer_id) {
                res.customer_code = Some(code.clone());
                res.customer_name = Some(name.clone());
            }
            if let Some((code, name)) = warehouses.get(&row.warehouse_id) {
                res.warehouse_code = Some(code.clone());
                res.warehouse_name = Some(name.clone());
            }
            res.so_no = so_numbers.get(&row.so_id).cloned();
            res.stock_out_no = row
                .stock_out_id
                .and_then(|id| stock_out_numbers.get(&id).cloned());
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

// ==================== 状态流转 ====================

/// 过账前置：**库存必须够量**（只读，逐行 `stockout_model::fifo_plan`）。
///
/// 不够量 → 409 + 逐行缺口文案（物料 / 要发 / 可用 / 缺口）。
/// 这里不扣库存：三本账的唯一写入口是 `inventory_writer`，由 SO-10 过账时调用。
async fn transition_to_posted(
    row: &ShipmentRow,
    items: &[ShipmentItemRow],
) -> std::result::Result<(), ApiError> {
    if items.is_empty() {
        return Err(conflict(format!(
            "发货单 {} 没有明细，不能过账",
            row.ship_no
        )));
    }
    let shortfalls = model::stock_shortfalls(db(), row.warehouse_id, items)
        .await
        .map_err(|error| utils::db_error(error, "校验库存是否充足"))?;
    if shortfalls.is_empty() {
        return Ok(());
    }
    let detail = shortfalls
        .iter()
        .map(|item| {
            format!(
                "订单行 {}（物料 {}）要发 {} / 该仓可用 {}，缺 {}",
                item.so_item_id,
                item.material_id,
                item.needed.round_dp(4),
                item.available.round_dp(4),
                item.shortfall().round_dp(4)
            )
        })
        .collect::<Vec<_>>()
        .join("；");
    Err(conflict(format!(
        "库存不足，过账已拒绝：发货单 {} 的仓库（{}）不够量 —— {detail}。\
         处理办法：① 先补货 / 收货后再过账；② 改小本单数量；③ 换成有货的仓库重新建单",
        row.ship_no, row.warehouse_id
    )))
}

/// 状态流转：边表校验（`SHIPMENT_EDGES`）→ 业务闸 → 状态机留痕 → CAS 落库（同一事务）。
///
/// 三档业务闸（各自独立，缺一个就是线上事故）：
/// 1. `target = PENDING_POST`：单据至少有 1 行；**来源订单仍在可发状态**（草稿期间订单被作废，不能继续发）；
/// 2. `target = POSTED`：**库存充足**（[`transition_to_posted`]）；
/// 3. 任何目标：边表 + 必填原因（`PENDING_POST → CANCELLED`）。
pub async fn change_shipment_status(
    operator: Option<Uuid>,
    id_raw: &str,
    req: ShipmentStatusReq,
) -> std::result::Result<ShipmentRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let target = validate_target(&req.target_status).map_err(bad_request)?;
    let row = model::find_shipment(id)
        .await
        .map_err(|error| utils::db_error(error, "查询发货单"))?
        .ok_or_else(not_found)?;

    // ① 合法边校验（非法迁移 409，文案给出当前态与可选去向）
    if !model::can_transition(&row.status, &target) {
        let targets = model::allowed_targets(&row.status);
        let message = if targets.is_empty() {
            format!(
                "发货单 {} 已是终态 {}（{}），不能再流转：已过账的作废要走销售出库红冲（SO-10）",
                row.ship_no,
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

    // ② 必填原因（与 state_machine 的 SHIPMENT_REASON_EDGES 同源，提前给人话）
    if state_machine::Entity::Shipment.needs_reason(&row.status, &target) && reason.is_none() {
        return Err(bad_request(format!(
            "{}（{}）→ {}（{}）必须给出原因（reason）：这条边会释放本单占用的未发量，\
             对账时要能回答「为什么」",
            model::status_label(&row.status),
            row.status,
            model::status_label(&target),
            target
        )));
    }

    // ③ 业务闸
    let items = model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询发货单明细"))?;
    if target == model::STATUS_PENDING_POST {
        if items.is_empty() {
            return Err(bad_request(format!(
                "发货单 {} 没有明细，不能提交待过账",
                row.ship_no
            )));
        }
        let so_status = model::so_status_of(db(), row.so_id)
            .await
            .map_err(|error| utils::db_error(error, "查询来源销售订单状态"))?
            .unwrap_or_default();
        if !model::SHIPPABLE_SO_STATUSES.contains(&so_status.as_str()) {
            return Err(conflict(format!(
                "来源销售订单当前是「{so_status}」，不能继续发货：\
                 订单已作废 / 已关闭 / 已发完时，请把本单作废（CANCELLED）而不是提交"
            )));
        }
    }
    if target == model::STATUS_POSTED {
        transition_to_posted(&row, &items).await?;
    }

    // ④ 状态机留痕 + CAS 落库（同一事务）
    let now = Utc::now();
    let op = state_machine::Operator::of(operator);
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启状态流转事务"))?;
    let mut posted: Option<shipment_posting::PostOutcome> = None;
    let outcome = async {
        // ① 状态机留痕（边表校验 + reason 校验 + 写 biz_status_log）
        state_machine::transition(
            &tx,
            state_machine::ENTITY_SHIPMENT,
            id,
            &row.ship_no,
            &model::LEGAL_EDGES,
            &row.status,
            &target,
            op.clone(),
            reason,
            None,
        )
        .await?;
        // ② CAS 落状态**先于**库存效果：并发重复过账在这里就分出生死
        //    （第二个请求 UPDATE 到 0 行 → 整笔回滚，库存一遍都不会扣）
        let affected = model::apply_status(&tx, id, &row.status, &target, operator, now)
            .await
            .map_err(|error| write_error(error, "发货单状态流转"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "发货单 {} 的状态在本次流转期间已被改动（期望 {}），请刷新后重试",
                row.ship_no, row.status
            )));
        }
        // ③ SO-10 库存效果：生成销售出库单 → 过账（三本账负向 + 成本结转凭证）
        //    → 回填 stock_out_id → 订单行 delivered_qty → 订单 PARTIAL_OUT/OUTBOUND。
        //    全部在**本事务**里：任一步失败，上面的 CAS 也一起回滚。
        if target == model::STATUS_POSTED {
            posted = Some(shipment_posting::post_effects(&tx, &row, &items, &op, now).await?);
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交状态流转事务"))?;
            if let Some(note) = posted {
                tracing::info!(
                    shipment_id = %id,
                    ship_no = %row.ship_no,
                    stock_out_id = %note.stock_out_id,
                    stock_out_no = %note.stock_out_no,
                    lines = note.lines,
                    order_status = note.so_status.as_deref().unwrap_or_default(),
                    "发货单已过账：销售出库单 / 三本账 / delivered_qty / 订单状态已同事务落库"
                );
                // 提交**之后**才跑延迟联动（发通知 / 建任务，独立事务，失败只记日志）
                stockout_handler::run_deferred_linkage_after_commit(
                    note.stock_out_id,
                    &op,
                    note.linkage,
                )
                .await;
            }
            detail_of(id).await
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== HTTP 入口 ====================

/// 发货单分页。
#[nik::permission(
    path = "/business/erp/shipment/page",
    method = "GET",
    permission = "business:erp:shipment:list"
)]
#[nik::get("/business/erp/shipment/page")]
pub async fn shipment_page_api(
    Query(filter): Query<ShipmentFilter>,
) -> ResResult<PageData<Vec<ShipmentRes>>> {
    Ok(Res::ok(page_shipment(filter).await?))
}

/// 发货单详情。
#[nik::permission(
    path = "/business/erp/shipment/{id}",
    method = "GET",
    permission = "business:erp:shipment:list"
)]
#[nik::get("/business/erp/shipment/{id}")]
pub async fn shipment_detail(Path(id): Path<String>) -> ResResult<ShipmentRes> {
    Ok(Res::ok(get_shipment(&id).await?))
}

/// 该销售订单**未发完的行**（前端「从订单带出」对话框的唯一读数）。
#[nik::permission(
    path = "/business/erp/shipment/shippable/{so_id}",
    method = "GET",
    permission = "business:erp:shipment:list"
)]
#[nik::get("/business/erp/shipment/shippable/{so_id}")]
pub async fn shipment_shippable(Path(so_id): Path<String>) -> ResResult<Vec<ShippableItemRes>> {
    let so_id = parse_uuid(&so_id, "so_id")?;
    Ok(Res::ok(shippable_of(so_id).await?))
}

/// 新建发货单（`items` 留空 = 从订单带出未发完的行）。
#[nik::permission(
    path = "/business/erp/shipment",
    method = "POST",
    permission = "business:erp:shipment:create"
)]
#[nik::post("/business/erp/shipment")]
pub async fn shipment_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateShipmentReq>,
) -> ResResult<ShipmentRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_shipment(operator, req).await?))
}

/// 状态流转（非法迁移 409）。
#[nik::permission(
    path = "/business/erp/shipment/{id}/status",
    method = "PATCH",
    permission = "business:erp:shipment:status"
)]
#[nik::patch("/business/erp/shipment/{id}/status")]
pub async fn shipment_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<ShipmentStatusReq>,
) -> ResResult<ShipmentRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(change_shipment_status(operator, &id, req).await?))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// 定点字面量（浮点会丢精度，一律走字符串）。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    /// ① 目标态只认 4 值；大小写 / 销售订单的状态一律 400。
    #[test]
    fn status_target_must_be_in_domain() {
        for good in model::STATUSES {
            assert_eq!(validate_target(good).as_deref(), Ok(good));
            assert_eq!(validate_target(&format!(" {good} ")).as_deref(), Ok(good));
        }
        for bad in [
            "SHIPPED",  // 销售订单的终态，发货单没有
            "APPROVED", // 审批态不属于发货单
            "PARTIAL_OUT",
            "posted",     // 小写不认
            "CANCELLED ", // 尾部空格 trim 后其实合法？—— 见下
            "",
        ] {
            let outcome = validate_target(bad);
            if bad.trim() == "CANCELLED" {
                // trim 之后是合法值：请求体带尾空格不该 500，也不该被判非法
                assert_eq!(outcome.as_deref(), Ok("CANCELLED"));
            } else {
                assert!(outcome.is_err(), "`{bad}` 不该通过状态域校验");
            }
        }
        assert!(
            validate_target("SHIPPED").unwrap_err().contains("DRAFT"),
            "错误文案要列出 4 个合法值"
        );
    }

    /// ② 数量校验：`qty > 0` 且 ≤ 未发量；边界（等号放行 / 多 0.0001 拒）。
    #[test]
    fn qty_validation_boundary() {
        assert!(
            validate_qty(dec("10"), dec("10"), "items[0]").is_ok(),
            "等号放行"
        );
        assert!(validate_qty(dec("0.0001"), dec("10"), "items[0]").is_ok());
        let over = validate_qty(dec("10.0001"), dec("10"), "items[0]").unwrap_err();
        assert!(over.contains("超发"), "超发文案要点明：{over}");
        assert!(over.contains("items[0]"), "文案要能定位到哪一行：{over}");
        let zero = validate_qty(Decimal::ZERO, dec("10"), "items[1]").unwrap_err();
        assert!(zero.contains("大于 0"), "{zero}");
        assert!(validate_qty(dec("-1"), dec("10"), "items[2]").is_err());
        // 剩余 0 ⇒ 任何正数都拒（不能「库层 qty>0 放行、业务超发」）
        assert!(validate_qty(dec("0.0001"), Decimal::ZERO, "items[3]").is_err());
    }

    /// ③ 序列号：不填放行；填了必须一物一码（条数 = 数量），空串拒。
    #[test]
    fn serial_numbers_must_match_qty() {
        assert!(
            validate_serials(&[], dec("3"), "items[0]").is_ok(),
            "不填 = 由过账分配"
        );
        assert!(validate_serials(&["SN-1".into(), "SN-2".into()], dec("2"), "items[0]").is_ok());
        let mismatch = validate_serials(&["SN-1".into()], dec("2"), "items[0]").unwrap_err();
        assert!(mismatch.contains("一物一码"), "{mismatch}");
        let blank = validate_serials(&["".into()], dec("1"), "items[0]").unwrap_err();
        assert!(blank.contains("不能是空串"), "{blank}");
    }

    /// ④ 同一订单行在一张单里只能出现一次（否则超发守卫被「拆行」绕过）。
    #[test]
    fn duplicate_so_items_are_rejected() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        assert!(ensure_unique_so_items(&[a, b]).is_ok());
        assert!(ensure_unique_so_items(&[]).is_ok());
        let err = ensure_unique_so_items(&[a, b, a]).unwrap_err();
        assert!(err.contains("两次"), "文案要说清重复：{err}");
    }

    /// ⑤ 默认发货日期是**北京时间**的今天（UTC 直接取日期在东八区早上 8 点前会差一天）。
    #[test]
    fn default_ship_date_is_cst() {
        // 2026-11-06T17:30:00Z = 北京时间 2026-11-07 01:30
        let now = DateTime::parse_from_rfc3339("2026-11-06T17:30:00Z")
            .expect("RFC3339")
            .with_timezone(&Utc);
        assert_eq!(today_cst(now).to_string(), "2026-11-07");
        // 2026-11-06T00:30:00Z = 北京时间 08:30，同一天
        let now = DateTime::parse_from_rfc3339("2026-11-06T00:30:00Z")
            .expect("RFC3339")
            .with_timezone(&Utc);
        assert_eq!(today_cst(now).to_string(), "2026-11-06");
    }

    /// ⑥ 状态机边表：4 条边，且「过账」只有一条入口、终态没有出口。
    #[test]
    fn edges_gate_only_the_four_legal_moves() {
        assert_eq!(
            model::LEGAL_EDGES,
            state_machine::SHIPMENT_EDGES,
            "model 的边表必须就是注册表（不许在调用点私自放宽）"
        );
        assert_eq!(
            model::allowed_targets(model::STATUS_DRAFT),
            vec!["PENDING_POST", "CANCELLED"]
        );
        assert_eq!(
            model::allowed_targets(model::STATUS_PENDING_POST),
            vec!["POSTED", "CANCELLED"]
        );
        assert!(
            model::allowed_targets(model::STATUS_POSTED).is_empty(),
            "已过账是终态"
        );
        assert!(
            model::allowed_targets(model::STATUS_CANCELLED).is_empty(),
            "已作废是终态"
        );
        assert!(
            !model::can_transition(model::STATUS_POSTED, model::STATUS_CANCELLED),
            "已过账作废要走红冲（SO-10），不能靠状态边假装账回来了"
        );
        assert!(
            !model::can_transition(model::STATUS_PENDING_POST, model::STATUS_DRAFT),
            "发货单没有审批，不该有「打回草稿」的边"
        );
        // 只有「待过账作废」要原因
        let entity = state_machine::Entity::Shipment;
        assert!(entity.needs_reason("PENDING_POST", "CANCELLED"));
        assert!(!entity.needs_reason("DRAFT", "CANCELLED"));
        // 已过账 / 已作废是锁定态（不能再按普通改单写）
        assert!(entity.is_locked("POSTED"));
        assert!(entity.is_locked("CANCELLED"));
        assert!(!entity.is_locked("PENDING_POST"));
    }

    /// ⑦ 分页筛选：域外状态 400、非法 UUID 400（不静默当作「没传」）。
    #[test]
    fn filter_rejects_bad_values() {
        assert!(parse_uuid("not-a-uuid", "so_id").is_err());
        assert!(parse_uuid("  ", "so_id").is_err());
        assert_eq!(
            parse_opt_uuid(Some("   "), "so_id").expect("空白视为不筛"),
            None
        );
        let good = Uuid::now_v7();
        assert_eq!(
            parse_opt_uuid(Some(&good.to_string()), "so_id").expect("合法 UUID"),
            Some(good)
        );
        assert!(
            parse_date(Some("2026-13-01"), "ship_date").is_err(),
            "13 月必须拒"
        );
        assert_eq!(
            parse_date(Some(""), "ship_date").expect("空白 = 今天"),
            None
        );
    }
}
