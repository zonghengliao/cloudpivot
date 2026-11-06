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
//! | PATCH | `/business/erp/sales-order/{id}/status` | `business:erp:sale-order:status` | 状态流转（**只走 `LEGAL_EDGES` 的 11 条边**，其余 409）；`target=SUBMITTED` 走审批入口 |
//! | POST | `/business/erp/sales-order/{id}/submit-approval` | `business:erp:sale-order:status` | 提交审批（`DRAFT → SUBMITTED` + 发起 `so_approve` 实例；SO-06） |
//!
//! # 状态机（唯一真相：`state_machine::SALES_ORDER_EDGES`）
//!
//! ```text
//! DRAFT → SUBMITTED → APPROVED → PARTIAL_OUT → OUTBOUND
//!                        └→ OUTBOUND（一次发完）   └→ CLOSED（剩余量作废）
//!                        └→ CLOSED（未发货直接关闭）
//! DRAFT / SUBMITTED / APPROVED → CANCELLED
//! SUBMITTED → DRAFT（审批驳回 / 撤回打回草稿，**只有审批回调能写**）
//! ```
//!
//! - 非法迁移一律 **409 + 可选去向**（不是 500、也不是静默忽略）；
//! - 需要原因的边（提交后 / 审批后作废、两种关闭）缺 `reason` → **400**；
//! - 流转落库走两道：`state_machine::transition`（边表校验 + 写 `biz_status_log`）→
//!   `sales_order_model::apply_status`（CAS `WHERE status = from`），同一事务。
//! - `PARTIAL_OUT` / `OUTBOUND` 正常由**出库过账**回写（S3-05 卡），本卡也允许持有
//!   `sale-order:status` 权限的人手工推（联调 / 导入历史单场景），边表是同一张。
//!
//! # 审批（SO-06）
//!
//! | 目标态 | 谁写 | 说明 |
//! |--------|------|------|
//! | `SUBMITTED` | [`submit_approval`] | `DRAFT → SUBMITTED` + 发起 `so_approve` 实例（同一事务）；没配流程 → 409 默认拒绝，除非持 `business:erp:skip-approval` 并写明理由（≥10 字） |
//! | `APPROVED` | **审批回调** `sales_approval_bridge::on_so_settled` | 人工点 `PATCH /status` 直接 400（指向审批中心）——否则「审批」只是画在纸上 |
//! | `DRAFT` | **审批回调** | 驳回 / 撤回打回草稿（意见写 `remark`、清实例），改完可重新提交 |
//!
//! 金额三档（`< 1 万` 销售主管 / `1–10 万` 销售主管→财务经理 / `≥ 10 万`
//! 销售主管→财务经理→总经理）**不在代码里**：写在 `V0281__sales_order_flow` 的
//! 条件边（`form.amount`）+ `biz_flow_node_assignee` 角色指派里，客户改阈值不用发版。
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

use crate::business::erp::approval_gate;
use crate::business::erp::contract_model::{self, ContractRow};
use crate::business::erp::customer_model::{self, STATUS_ACTIVE};
use crate::business::erp::sales_approval_bridge;
use crate::business::erp::sales_order_model::{
    self as model, NewItem, NewSalesOrder, SalesOrderItemRow, SalesOrderRow, Totals,
};
use crate::business::flow::engine;
use crate::business::flow::model as flow_model;
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
    /// 销售合同 id（UUID，可空 = 不挂合同）。
    ///
    /// 填了就必须过四关（`resolve_sales_contract`）：存在未软删 → `contract_type=SALES`
    /// → 状态可用（`SIGNED` / `ACTIVE`）→ 合同乙方 = 本单客户；
    /// 并在事务里过额度守卫（累计订单额不得超合同金额，超出 409 `66019`）。
    #[serde(default)]
    pub contract_id: Option<String>,
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
    /// 销售合同 id（**不传 = 不改**；传空串 = 解绑合同；传 id = 换成这张）。
    ///
    /// 与其它可空字段同一口径：`Option<String>` + 空串代表「清空」，
    /// 所以「解绑合同」不需要第二个布尔标志。
    #[serde(default)]
    pub contract_id: Option<String>,
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

/// 提交审批请求（`POST /business/erp/sales-order/{id}/submit-approval`）。
///
/// 除 `reason` 外还接受内控例外的两个字段（`skip_approval` / `skip_reason`，
/// 见 `approval_gate::SkipApprovalReq`）：**没配 `so_approve` 流程时**，
/// 只有持 `business:erp:skip-approval` 且理由 ≥10 字的账号能显式跳过；
/// 配了流程时带 `skip_approval` 一律 400（不许绕过已配置的内控）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SalesOrderSubmitReq {
    /// 提交备注（写进 `biz_status_log.reason`；跳过审批时与跳过理由合并）。
    #[serde(default)]
    pub reason: Option<String>,
    /// 内控例外片段（`skip_approval` / `skip_reason`）。
    #[serde(flatten)]
    pub approval: approval_gate::SkipApprovalReq,
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
    /// 销售合同 id（未挂合同为 `null`）。
    pub contract_id: Option<String>,
    /// 销售合同号（`HT-yyyyMMdd-000001`）。
    ///
    /// **只有详情填**（列表恒 `null`）：与 `approval_instance_id` 同口径 —— 列表要带上它
    /// 就得每行 JOIN 一次 `biz_contract`，或在内存里批量补一次；列表页当前不需要展示，
    /// 先用 `contract_id` 判断「有没有挂合同」。
    pub contract_no: Option<String>,
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
    /// 当前挂着的审批实例 id（`biz_sales_order.approval_instance_id`）。
    ///
    /// **只有详情填**（列表恒 `null`）：审批进度区块（前端 `FlowProgress.vue`）
    /// 靠它再打 `GET /business/flow/instance/{id}` 拿节点行，草稿 / 已驳回打回
    /// 的单据这里为 `null`（SO-06 的 `apply_status_with_instance` 会清空）。
    pub approval_instance_id: Option<String>,
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
            contract_id: row.contract_id.map(|id| id.to_string()),
            // 只由 `detail_of` 补（列表不发这条查询）；`of()` 保持纯函数
            contract_no: None,
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
            // 只由 `detail_of` 补（列表不发这条查询）；`of()` 保持纯函数
            approval_instance_id: None,
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

/// 可选合同 id：空白 / `None` → `None`（不挂合同）；非空必须是合法 UUID。
///
/// 为什么不用 `parse_uuid` 一把梭：`contract_id` 是可空字段，「没传」和「传空串」
/// 都是「不挂合同」，不能报 400；但传了乱码必须 400（静默当没传会让调用方以为挂上了）。
fn parse_contract_id(raw: Option<&str>) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => parse_uuid(text, "contract_id").map(Some),
    }
}

/// 校验 + 归一化整单入参：客户 id、合同 id、日期、明细（**至少 1 行**）。
fn build_order<'a>(
    customer_id: &str,
    contract_id: Option<&str>,
    order_date: Option<&str>,
    delivery_date: Option<&str>,
    remark: Option<&'a str>,
    items: &[SalesOrderItemReq],
) -> std::result::Result<(NewSalesOrder<'a>, Vec<NewItem>), ApiError> {
    let customer_id = parse_uuid(customer_id, "customer_id")?;
    let contract_id = parse_contract_id(contract_id)?;
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
            contract_id,
            order_date,
            delivery_date,
            remark: remark.map(str::trim).filter(|text| !text.is_empty()),
        },
        built,
    ))
}

// ==================== 合同关联（SO-27） ====================

/// 合同能不能挂到这张销售订单上（**纯逻辑**，单测直接调）。
///
/// 三条判据（对应 ARCH-7 §15.1.2 ③ 与采购单侧 V-22 的同构规则）：
/// 1. 类型必须是 `SALES` —— 采购 / 服务 / 租赁合同挂到销售订单上，
///    额度守卫会拿错合同的额度去卡销售单，账就永远对不上；
/// 2. 状态必须在 `contract_model::USABLE_FOR_ORDER`（`SIGNED` / `ACTIVE`）——
///    与采购单**同一个常量**：合同详情的 `usable_for_order` 也是按它算的，
///    两处口径不一致会让用户看到「可用 = true」却选不到；
/// 3. 合同乙方必须是本单客户（`party_b_id == customer_id`）——
///    挂到别家客户的合同上，累计额会算进别人的合同，等于替别人用额度。
fn ensure_sales_contract_usable(
    contract: &ContractRow,
    customer_id: Uuid,
) -> std::result::Result<(), ApiError> {
    if contract.contract_type != "SALES" {
        return Err(ApiError::new(
            400,
            error_code::ERP_CONTRACT_NOT_USABLE,
            format!(
                "合同 {} 的类型是 {}，不能挂到销售订单上（只接受 SALES）",
                contract.contract_no, contract.contract_type
            ),
        ));
    }
    if !contract_model::USABLE_FOR_ORDER.contains(&contract.status.as_str()) {
        return Err(ApiError::new(
            400,
            error_code::ERP_CONTRACT_NOT_USABLE,
            format!(
                "合同 {} 当前是「{}」（{}），只有已签署（SIGNED）/ 生效（ACTIVE）的合同能挂订单",
                contract.contract_no,
                contract_model::status_label(&contract.status),
                contract.status
            ),
        ));
    }
    if contract.party_b_id != customer_id {
        return Err(ApiError::new(
            400,
            error_code::ERP_SO_CONTRACT_CUSTOMER_MISMATCH,
            format!(
                "合同 {} 的乙方不是本单客户（合同乙方 {}，本单客户 {}）：\
                 挂别家客户的合同会把金额记到别人的合同额度上",
                contract.contract_no, contract.party_b_id, customer_id
            ),
        ));
    }
    Ok(())
}

/// 合同额度判定（**纯逻辑**）：`已下单 + 本单 ≤ 合同金额`，超出即 409。
///
/// 为什么是 409 不是 400：入参本身合法（合同存在、状态可用、客户匹配），
/// 冲突的是「这张合同的额度已经用掉了」这个现状 —— 同一张单在额度够的时候能建。
fn ensure_within_contract_amount(
    contract_no: &str,
    contract_amount: Decimal,
    used: Decimal,
    this_total: Decimal,
) -> std::result::Result<(), ApiError> {
    let after = used + this_total;
    if after > contract_amount {
        return Err(ApiError::new(
            409,
            error_code::ERP_CONTRACT_AMOUNT_EXCEEDED,
            format!(
                "合同 {contract_no} 额度不足：合同金额 {contract_amount}，已下单 {used}（不含作废单），\
                 本单 {this_total}，下单后累计 {after} 将超出合同额（剩余额度 {}）",
                contract_amount - used
            ),
        ));
    }
    Ok(())
}

/// 取合同并校验可挂（不存在 / 已软删 → 404）。
///
/// 读在事务外（与采购单 `resolve_contract` 同序）：额度守卫会在**事务内**再锁一次
/// 合同行并复检状态，所以这里读到的是「建单那一刻」的快照。
async fn resolve_sales_contract(
    contract_id: Uuid,
    customer_id: Uuid,
) -> std::result::Result<ContractRow, ApiError> {
    let contract = contract_model::contract_find(contract_id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售合同"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                format!("合同 {contract_id} 不存在或已删除（contract_id 必须在建单前校验）"),
            )
        })?;
    ensure_sales_contract_usable(&contract, customer_id)?;
    Ok(contract)
}

/// 合同额度守卫（**必须在事务里调**）：先 `FOR UPDATE` 锁合同，再现算累计额。
///
/// 为什么不能「先查后写」：两个并发建单各自都看到「还剩额度」，加起来就超了
/// （ARCH-2 V-23 的并发坑，采购单侧 `guard_contract_amount` 同一套写法）。
/// 锁顺序 = 合同行 → 订单写入，与采购链同序，不会互相死锁。
///
/// 锁内复检状态：从读到写之间合同可能被终止 / 软删。
async fn guard_contract_amount(
    ex: &impl SqlExecutor,
    contract: &ContractRow,
    this_total: Decimal,
    exclude: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let locked = contract_model::contract_lock_full(ex, contract.id)
        .await
        .map_err(|error| utils::db_error(error, "锁定销售合同"))?;
    let Some(locked) = locked else {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            format!("合同 {} 在本次建单期间已被删除", contract.contract_no),
        ));
    };
    if !contract_model::USABLE_FOR_ORDER.contains(&locked.status.as_str()) {
        return Err(ApiError::new(
            400,
            error_code::ERP_CONTRACT_NOT_USABLE,
            format!(
                "合同 {} 在本次建单期间变成「{}」（{}），已不可挂订单",
                locked.contract_no,
                contract_model::status_label(&locked.status),
                locked.status
            ),
        ));
    }
    let used = model::committed_amount(ex, locked.id, exclude)
        .await
        .map_err(|error| utils::db_error(error, "统计合同已下单额"))?;
    ensure_within_contract_amount(&locked.contract_no, locked.amount, used, this_total)
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
    let instance = model::approval_instance_of(row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单审批实例"))?;
    let mut res = SalesOrderRes::of(&row, items).with_customer(customer.as_ref());
    res.approval_instance_id = instance.map(|id| id.to_string());
    // SO-27：合同号按 `contract_id` 现查（不落快照列 —— 合同号不可改，没有「当时是谁」的问题；
    // 合同被硬删时外键 SET NULL，这里也不会查）
    if let Some(contract_id) = row.contract_id {
        res.contract_no = contract_model::contract_find(contract_id)
            .await
            .map_err(|error| utils::db_error(error, "查询销售订单关联合同"))?
            .map(|contract| contract.contract_no);
    }
    Ok(res)
}

/// 新建销售订单（`DRAFT`）。
pub async fn create_sales_order(
    operator: Option<Uuid>,
    req: CreateSalesOrderReq,
) -> std::result::Result<SalesOrderRes, ApiError> {
    let (new, items) = build_order(
        &req.customer_id,
        req.contract_id.as_deref(),
        req.order_date.as_deref(),
        req.delivery_date.as_deref(),
        req.remark.as_deref(),
        &req.items,
    )?;
    let customer = require_active_customer(new.customer_id).await?;
    // SO-27：合同的「类型 / 状态 / 乙方=客户」在事务外先拦（读一次即可），
    // 额度守卫必须在事务内（锁合同行 → 现算累计额），见 guard_contract_amount
    let contract = match new.contract_id {
        None => None,
        Some(contract_id) => Some(resolve_sales_contract(contract_id, new.customer_id).await?),
    };
    let totals = Totals::of(&items);
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建单事务"))?;
    if let Some(contract) = contract.as_ref()
        && let Err(error) = guard_contract_amount(&tx, contract, totals.with_tax, None).await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
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
    // 合同：不传 = 沿用；空串 = 解绑；传 id = 换合同（换也要重过四关 + 额度守卫）
    let contract_id = match req.contract_id.as_deref() {
        None => current.contract_id,
        Some(text) => parse_contract_id(Some(text))?,
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
    // SO-27：合同四关（含「乙方 = 本单客户」，所以必须排在客户确定之后）；
    // 额度守卫在事务里，改单时把自己排除（exclude = 本单 id）
    let contract = match contract_id {
        None => None,
        Some(contract_id) => Some(resolve_sales_contract(contract_id, customer_id).await?),
    };
    let new = NewSalesOrder {
        customer_id,
        contract_id,
        order_date,
        delivery_date,
        remark: remark.as_deref(),
    };
    let totals = Totals::of(&items);
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改单事务"))?;
    if let Some(contract) = contract.as_ref()
        && let Err(error) = guard_contract_amount(&tx, contract, totals.with_tax, Some(id)).await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
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
    // SO-06：审批中作废（`SUBMITTED → CANCELLED`）→ **同一事务**终止在跑的审批单。
    // 不做这一步就是采购桥 / 收货桥踩过的老坑：审批中心留孤儿待办，而且
    // `uk_biz_flow_instance_biz_active` 会把这张单**下一次提交**永久堵死。
    if row.status == model::STATUS_SUBMITTED && target == model::STATUS_CANCELLED {
        let cancel_reason = reason.unwrap_or("审批中作废：终止在跑的审批单");
        if let Err(error) =
            sales_approval_bridge::cancel_approval_of(&tx, id, &row.so_no, cancel_reason).await
        {
            let _ = tx.rollback().await;
            return Err(write_error(error, "终止在跑的销售订单审批"));
        }
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

// ==================== 提交审批（SO-06） ====================

/// 提交销售订单审批的**唯一入口**（SO-06 + ARCH-3 DC-01 口径）。
///
/// 配了已发布的 `so_approve` 流程 → `DRAFT → SUBMITTED` + 发起审批实例（**同一事务**）；
/// **没配流程 → 409 + `APPROVAL_FLOW_NOT_CONFIGURED`（默认拒绝）**，只有持有
/// `business:erp:skip-approval` 并写明理由（≥10 字）的账号才能显式跳过，
/// 理由会作为本次状态流转的 `reason` 落进 `biz_status_log`。
///
/// 为什么入口收在这里：`PATCH /status`（老前端）与 `POST /submit-approval`（新按钮）
/// **必须同一条路** —— 否则审批可以被换一个 URL 绕过。
///
/// 提交前的三道守卫与直接提交是**同一套**（0 行明细 / 客户冻结 / 信用额度超），
/// 区别只在于「过不过审批单」。
pub async fn submit_approval(
    operator: Option<Uuid>,
    id_raw: &str,
    gate: &approval_gate::ApprovalGate<'_>,
) -> std::result::Result<SalesOrderRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let row = model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单"))?
        .ok_or_else(not_found)?;
    // ① 已在审批中 → 409（带上审批单号，报文能直接照着去找）
    if row.status == model::STATUS_SUBMITTED {
        let instance_no = pending_instance_no(&row).await;
        return Err(conflict(format!(
            "销售订单 {} 已在审批中（{instance_no}）：请等审批结束，或从审批中心撤回后再提交",
            row.so_no
        )));
    }
    // ② 只有草稿能提交审批
    if row.status != model::STATUS_DRAFT {
        return Err(conflict(format!(
            "销售订单 {} 当前是「{}」（{}），只有草稿（DRAFT）可以提交审批",
            row.so_no,
            model::status_label(&row.status),
            row.status
        )));
    }
    // ③ 三道守卫（明细 ≥ 1 / 客户未冻结 / 信用额度未超）
    let lines = model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单明细"))?;
    ensure_submittable(&row, &lines).await?;
    // ④ 幂等：同一单已有在跑的审批 → 409
    if flow_model::biz_instance_running(sales_approval_bridge::BIZ_TYPE, id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = pending_instance_no(&row).await;
        return Err(conflict(format!(
            "销售订单 {} 已在审批中（{instance_no}）：请等审批结束，或从审批中心撤回后再提交",
            row.so_no
        )));
    }
    match sales_approval_bridge::published_so_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单审批流程"))?
    {
        Some(flow) => {
            // 有流程就必须走流程：请求里带了 skip_approval 也要 400（不许绕过已配置的内控）
            approval_gate::ensure_not_skipped(sales_approval_bridge::FLOW_CODE, gate)?;
            start_approval(&row, &flow, operator).await
        }
        None => {
            // 默认拒绝：没配流程 → 409（点名 so_approve）；只有「持跳过权限 + 理由 ≥10 字」放行
            let skip_reason = approval_gate::authorize_missing_flow(
                sales_approval_bridge::BIZ_TYPE,
                sales_approval_bridge::FLOW_CODE,
                gate,
            )?;
            tracing::warn!(
                order_id = %row.id,
                order_no = %row.so_no,
                %skip_reason,
                "销售订单提交：没有已发布的 so_approve 流程，按「显式跳过审批」放行（理由已写 biz_status_log）"
            );
            let skip_req = SalesOrderStatusReq {
                target_status: model::STATUS_SUBMITTED.to_string(),
                reason: Some(skip_reason),
            };
            change_sales_order_status(operator, id_raw, skip_req).await
        }
    }
}

/// [`submit_approval`] 的「有流程」分支：`DRAFT → SUBMITTED` + 发起实例（同一事务）。
async fn start_approval(
    row: &SalesOrderRow,
    flow: &sales_approval_bridge::SoFlow,
    operator: Option<Uuid>,
) -> std::result::Result<SalesOrderRes, ApiError> {
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 销售订单审批", row.so_no)
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
        biz_no: Some(row.so_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: sales_approval_bridge::form_data(row),
    };
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启销售订单审批事务"))?;
    let outcome = start_approval_tx(&tx, row, &start, operator, now).await;
    let mut out = match outcome {
        Ok(out) => out,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    let instance = out.instance_id.unwrap_or_default();
    let auto_finished = out.instance_status.eq_ignore_ascii_case("APPROVED");
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交销售订单审批事务"))?;
    // 锁外副作用（审批通知 / 客户自配的通知节点）必须在提交之后发，失败只 WARN
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    tracing::info!(
        order_id = %row.id,
        order_no = %row.so_no,
        instance_id = %instance,
        flow_code = %start.flow_code,
        "销售订单已提交审批（状态 SUBMITTED）"
    );
    // 流程里没有任何审批节点（全被跳过）→ 实例在同一个事务里就走完了，
    // 提交后立刻执行「审批通过」的回调，否则单据停在 SUBMITTED 且没有待办能推进它。
    if auto_finished {
        sales_approval_bridge::on_so_settled(Some(instance), "APPROVED").await;
    }
    detail_of(row.id).await
}

/// [`start_approval`] 的事务体：返回引擎动作结果（调用方提交后负责发锁外副作用）。
async fn start_approval_tx(
    tx: &Transaction,
    row: &SalesOrderRow,
    start: &engine::StartReq,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<engine::ActionOut, ApiError> {
    // ① 状态机唯一入口（边表 + 写 biz_status_log）
    state_machine::transition(
        tx,
        state_machine::ENTITY_SALES_ORDER,
        row.id,
        &row.so_no,
        &model::LEGAL_EDGES,
        model::STATUS_DRAFT,
        model::STATUS_SUBMITTED,
        state_machine::Operator::of(operator),
        Some("提交审批"),
        None,
    )
    .await?;
    // ② 发起审批单（引擎在同一事务里建实例 + 待办 + 节点）
    let mut out = engine::start(tx, start)
        .await
        .map_err(|error| submit_conflict(error, &row.so_no))?;
    if let Some(rejected) = out.rejected.take() {
        let hint = if rejected.code == error_code::FLOW_SELF_APPROVAL_FORBIDDEN {
            "（内控要求：审批人不能是发起人本人。请由他人提交，或让另一个账号具备审批权限后再审）"
        } else {
            ""
        };
        return Err(ApiError::new(
            rejected.status,
            rejected.code,
            format!("{}{hint}", rejected.msg),
        ));
    }
    let instance_id = out
        .instance_id
        .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
    // ③ CAS 回写单据：状态 + 实例 id（`WHERE status = 'DRAFT'` 双保险）
    let affected = model::apply_status_with_instance(
        tx,
        row.id,
        model::STATUS_DRAFT,
        model::STATUS_SUBMITTED,
        None,
        operator,
        Some(instance_id),
        now,
    )
    .await
    .map_err(|error| write_error(error, "销售订单提交审批落库"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "销售订单 {} 的状态在本次提交期间已被改动（期望 DRAFT），请刷新后重试",
            row.so_no
        )));
    }
    Ok(out)
}

/// 审批实例的唯一索引冲突 / 其它写错误 → 人话错误。
fn submit_conflict(err: DbError, order_no: &str) -> ApiError {
    if flow_model::is_biz_active_duplicate(&err) {
        tracing::warn!(
            order_no,
            error = %err,
            "提交审批撞审批实例唯一索引：已有在跑实例，翻 409 + 单号"
        );
        return flow_model::approval_in_progress("销售订单", Some(order_no));
    }
    write_error(err, "发起销售订单审批单")
}

/// 审批中实例单号（详情 / 409 报文用；查不到就说「审批单」）。
async fn pending_instance_no(row: &SalesOrderRow) -> String {
    if let Ok(Some(instance_id)) = model::approval_instance_of(row.id).await
        && let Ok(Some(instance)) = flow_model::find_instance(instance_id).await
    {
        return instance.instance_no;
    }
    "审批单".to_string()
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
///
/// `target=SUBMITTED` 走**审批入口**（[`submit_approval`]，配了流程就先建实例）；
/// `target=APPROVED` / `DRAFT` 是**审批回调专属**，人工点一律 400（指向审批中心）。
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
    let target = req.target_status.trim().to_string();
    // 审批结论只能由审批中心写：人工推这两个目标态等于「绕过审批」或「自己打回自己」
    if target == model::STATUS_APPROVED || target == model::STATUS_DRAFT {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "`{target}` 是审批结论，不能人工流转：请在「审批中心」审批（通过 → APPROVED，\
                 驳回 → 打回草稿 DRAFT）。人工 PATCH 只用于作废 / 关闭 / 出库这类状态"
            ),
        ));
    }
    if target == model::STATUS_SUBMITTED {
        let no_skip = approval_gate::SkipApprovalReq::default();
        let gate = approval_gate::ApprovalGate::new(&no_skip, &claims);
        return Ok(Res::ok(submit_approval(operator, &id, &gate).await?));
    }
    Ok(Res::ok(
        change_sales_order_status(operator, &id, req).await?,
    ))
}

/// 提交审批（`DRAFT → SUBMITTED` + 发起 `so_approve` 实例）。
///
/// 与 `PATCH /status` 的 `target=SUBMITTED` 是**同一条路**（[`submit_approval`]）；
/// 本端点额外支持 `skip_approval` / `skip_reason`（没配流程时的内控例外，需要
/// `business:erp:skip-approval` 权限码）。
#[nik::permission(
    path = "/business/erp/sales-order/{id}/submit-approval",
    method = "POST",
    permission = "business:erp:sale-order:status"
)]
#[nik::post("/business/erp/sales-order/{id}/submit-approval")]
pub async fn sales_order_submit_approval(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<SalesOrderSubmitReq>>,
) -> ResResult<SalesOrderRes> {
    let operator = utils::current_user_uuid(&claims);
    let req = body.map(|Json(req)| req).unwrap_or_default();
    let gate = approval_gate::ApprovalGate::new(&req.approval, &claims);
    Ok(Res::ok(submit_approval(operator, &id, &gate).await?))
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
            contract_id: None,
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
        let err = build_order(CUST, None, Some("2026-11-06"), None, None, &[])
            .expect_err("0 行明细必须被拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("至少"), "{}", err.msg);

        // ② 数量 <= 0
        let bad_qty = item_req("0", "10", "13");
        let err =
            build_order(CUST, None, None, None, None, &[bad_qty]).expect_err("qty=0 必须被拒");
        assert!(err.msg.contains("大于 0"), "{}", err.msg);
        // 负数数量
        let err = build_order(CUST, None, None, None, None, &[item_req("-1", "10", "13")])
            .expect_err("负数量必须被拒");
        assert!(err.msg.contains("大于 0"), "{}", err.msg);

        // ③ 单价为负
        let err = build_order(
            CUST,
            None,
            None,
            None,
            None,
            &[item_req("1", "-0.0001", "0")],
        )
        .expect_err("负单价必须被拒");
        assert!(err.msg.contains("不能为负"), "{}", err.msg);

        // ④ 税率越界（> 100 / < 0）
        for rate in ["100.0001", "-1"] {
            let err = build_order(CUST, None, None, None, None, &[item_req("1", "10", rate)])
                .unwrap_err();
            assert!(err.msg.contains("0–100"), "税率 {rate}：{}", err.msg);
        }
        // 边界值 0 / 100 合法
        build_order(CUST, None, None, None, None, &[item_req("1", "10", "0")]).expect("0% 合法");
        build_order(CUST, None, None, None, None, &[item_req("1", "10", "100")])
            .expect("100% 合法");

        // ⑤ 客户 id 非法 UUID / 缺失
        let err = build_order("", None, None, None, None, &[item_req("1", "10", "0")])
            .expect_err("空客户 id 必须被拒");
        assert_eq!(err.code, error_code::ERP_FILTER_INVALID);
        let err = build_order(
            "not-a-uuid",
            None,
            None,
            None,
            None,
            &[item_req("1", "10", "0")],
        )
        .expect_err("非法客户 id 必须被拒");
        assert_eq!(err.code, error_code::ERP_FILTER_INVALID);

        // ⑥ 日期格式 / 交期早于下单日
        let err = build_order(
            CUST,
            None,
            Some("2026-13-01"),
            None,
            None,
            &[item_req("1", "10", "0")],
        )
        .expect_err("13 月必须被拒");
        assert!(err.msg.contains("yyyy-MM-dd"), "{}", err.msg);
        let err = build_order(
            CUST,
            None,
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
            None,
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

    /// 审批实例 id 的口径：`of()`（列表 / 新建 / 状态流转都用它）恒为 `None`，
    /// **只有详情** `detail_of` 会补 —— 前端据此只对详情画审批进度（列表不画）。
    #[test]
    fn approval_instance_id_is_detail_only() {
        let req = order_req(CUST, vec![item_req("2", "100", "13")]);
        let (new, items) = build_order(
            &req.customer_id,
            req.contract_id.as_deref(),
            req.order_date.as_deref(),
            req.delivery_date.as_deref(),
            req.remark.as_deref(),
            &req.items,
        )
        .expect("合法入参");
        let totals = Totals::of(&items);
        let row = model::SalesOrderRow {
            id: uuid::Uuid::nil(),
            so_no: "SO-20261106-000001".to_string(),
            customer_id: new.customer_id,
            contract_id: new.contract_id,
            order_date: new.order_date,
            delivery_date: None,
            status: model::STATUS_DRAFT.to_string(),
            total_amount: totals.amount,
            total_tax: totals.tax,
            total_with_tax: totals.with_tax,
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            create_by: None,
            update_by: None,
            deleted_at: None,
        };
        let res = SalesOrderRes::of(&row, Vec::new());
        assert!(
            res.approval_instance_id.is_none(),
            "列表口径必须是 null（实例 id 只由详情补）"
        );
        assert_eq!(res.status, model::STATUS_DRAFT);
    }

    // ---------- SO-27：销售订单 ↔ 销售合同（纯逻辑） ----------

    /// 合同夹具：只填与「能不能挂单」相关的四列，其余给安全默认值。
    fn contract_fixture(
        kind: &str,
        status: &str,
        party_b_id: Uuid,
        amount: Decimal,
    ) -> ContractRow {
        ContractRow {
            id: Uuid::now_v7(),
            contract_no: "HT-20261107-000001".to_string(),
            contract_name: "SO-27 夹具合同".to_string(),
            contract_type: kind.to_string(),
            party_a_id: None,
            party_a_name: None,
            party_b_type: "CUSTOMER".to_string(),
            party_b_id,
            party_b_name: "夹具客户".to_string(),
            party_b_current_name: Some("夹具客户".to_string()),
            amount,
            currency_code: "CNY".to_string(),
            sign_date: NaiveDate::from_ymd_opt(2026, 1, 1),
            start_date: NaiveDate::from_ymd_opt(2026, 1, 2),
            end_date: NaiveDate::from_ymd_opt(2026, 12, 31),
            status: status.to_string(),
            owner_id: None,
            dept_id: None,
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            create_by: None,
            update_by: None,
            deleted_at: None,
            approval_instance_id: None,
        }
    }

    /// `contract_id` 是可空字段：不传 / 空串 = 不挂合同，乱码必须 400（不许静默当没传）。
    #[test]
    fn contract_id_is_optional_but_never_silently_ignored() {
        assert_eq!(parse_contract_id(None).unwrap(), None);
        assert_eq!(parse_contract_id(Some("")).unwrap(), None);
        assert_eq!(parse_contract_id(Some("   ")).unwrap(), None);
        let id = Uuid::now_v7();
        assert_eq!(
            parse_contract_id(Some(&format!("  {id}  "))).unwrap(),
            Some(id),
            "两侧空白应被 trim 后解析"
        );
        let err = parse_contract_id(Some("HT-20261107-000001"))
            .expect_err("合同号不是 UUID，必须 400（静默丢字段会让调用方以为挂上了）");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_FILTER_INVALID);

        // 建单入参经过 build_order 后，contract_id 落到 NewSalesOrder
        let (new, _) = build_order(
            CUST,
            Some(&id.to_string()),
            None,
            None,
            None,
            &[item_req("1", "10", "0")],
        )
        .expect("合法入参");
        assert_eq!(new.contract_id, Some(id));
    }

    /// 合同四关：类型必须是 SALES、状态必须可用（SIGNED/ACTIVE）、乙方必须是本单客户。
    #[test]
    fn sales_contract_must_be_sales_usable_and_same_customer() {
        let customer = Uuid::now_v7();

        // ① 类型不对（采购合同挂销售单 → 拿别人的额度卡销售单）
        let purchase = contract_fixture("PURCHASE", "ACTIVE", customer, dec("1000"));
        let err = ensure_sales_contract_usable(&purchase, customer).expect_err("PURCHASE 必须被拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_CONTRACT_NOT_USABLE);
        assert!(err.msg.contains("SALES"), "{}", err.msg);

        // ② 状态不可用（DRAFT / COMPLETED / TERMINATED 都不行）
        for status in ["DRAFT", "PENDING_APPROVAL", "COMPLETED", "TERMINATED"] {
            let contract = contract_fixture("SALES", status, customer, dec("1000"));
            let err = match ensure_sales_contract_usable(&contract, customer) {
                Err(error) => error,
                Ok(()) => panic!("{status} 必须被拒"),
            };
            assert_eq!(err.status, 400);
            assert_eq!(err.code, error_code::ERP_CONTRACT_NOT_USABLE);
        }
        // ③ SIGNED / ACTIVE 都能挂（与采购单同一常量 `USABLE_FOR_ORDER`）
        for status in contract_model::USABLE_FOR_ORDER {
            let contract = contract_fixture("SALES", status, customer, dec("1000"));
            ensure_sales_contract_usable(&contract, customer)
                .unwrap_or_else(|error| panic!("{status} 应放行：{}", error.msg));
        }

        // ④ 乙方不是本单客户（把 A 的单记到 B 的合同额度上，两边都错）
        let other = Uuid::now_v7();
        let contract = contract_fixture("SALES", "ACTIVE", other, dec("1000"));
        let err = ensure_sales_contract_usable(&contract, customer).unwrap_err();
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_SO_CONTRACT_CUSTOMER_MISMATCH);
        assert!(err.msg.contains("乙方"), "{}", err.msg);
    }

    /// 额度守卫：`已下单 + 本单 ≤ 合同金额`，超出 409 `66019`（边界值放行）。
    #[test]
    fn contract_amount_guard_rejects_only_when_exceeded() {
        let no = "HT-20261107-000001";
        // 边界：正好用满 → 放行
        ensure_within_contract_amount(no, dec("100"), dec("80"), dec("20"))
            .expect("用满不等于超出");
        // 未用满 → 放行
        ensure_within_contract_amount(no, dec("100"), dec("0"), dec("99.9999"))
            .expect("未超应放行");
        // 超出 0.0001 → 409 + 66019，报文点名四个数（合同额 / 已下 / 本单 / 剩余）
        let err = ensure_within_contract_amount(no, dec("100"), dec("80"), dec("20.0001"))
            .expect_err("超出必须被拒");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, error_code::ERP_CONTRACT_AMOUNT_EXCEEDED);
        for needle in ["100", "80", "20.0001", "剩余额度 20"] {
            assert!(err.msg.contains(needle), "报文缺 {needle}：{}", err.msg);
        }
        // 已下单额本身就超了合同（历史脏数据）→ 依然拒绝，不能因为「本单很小」放过
        let err = ensure_within_contract_amount(no, dec("100"), dec("120"), dec("1"))
            .expect_err("累计已超也必须拒");
        assert_eq!(err.code, error_code::ERP_CONTRACT_AMOUNT_EXCEEDED);
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
