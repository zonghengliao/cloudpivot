//! 销售报价单接口层（销售链 · M3-02）：CRUD + 状态机（草稿 → 已发出 → 已接受 / 已拒绝）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/quotation/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/quotation` | `business:erp:quotation:create` | 建单（`DRAFT`，取号 `QT-yyyyMMdd-000001`） |
//! | PUT | `/business/erp/quotation/{id}` | `business:erp:quotation:update` | 改单（**只有 DRAFT**，明细整单替换） |
//! | GET | `/business/erp/quotation/{id}` | `business:erp:quotation:list` | 详情（含明细 + 客户编码/名称 + 是否过期） |
//! | GET | `/business/erp/quotation/page` | `business:erp:quotation:list` | 分页（`quote_no` 模糊 / `status` / `customer_id` / `expired`） |
//! | PATCH | `/business/erp/quotation/{id}/status` | `business:erp:quotation:status` | 状态流转（**只走 `LEGAL_EDGES` 的 6 条边**，其余 409） |
//! | DELETE | `/business/erp/quotation/{id}` | `business:erp:quotation:delete` | 软删（**只有 DRAFT / CANCELLED**） |
//!
//! # 状态机（唯一真相：`state_machine::QUOTATION_EDGES`）
//!
//! ```text
//! DRAFT ──发出──▶ SENT ──客户接受──▶ ACCEPTED
//!   │              │  └──客户拒绝──▶ REJECTED（终态）
//!   └──作废──▶ CANCELLED ◀──撤回 / 客户反悔──┘
//! ```
//!
//! - 非法迁移一律 **409 + 可选去向**（不是 500、也不是静默忽略）；
//! - 需要原因的边（客户拒绝、已发出后的两条作废）缺 `reason` → **400**；
//! - 流转落库走两道：`state_machine::transition`（边表校验 + 写 `biz_status_log`）→
//!   `quotation_model::apply_status`（CAS `WHERE status = from`），同一事务。
//!
//! # 三个业务守卫（卡面「状态流转有校验」的落点）
//!
//! | 边 | 守卫 | 为什么 |
//! |----|------|--------|
//! | `DRAFT → SENT` | ① ≥1 行明细 ② 客户未冻结 ③ 有效期未过 | 空单 / 冻结客户 / 过期价发给客户，都是对外承诺 |
//! | `SENT → ACCEPTED` | 有效期未过 | **过期价不能被接受**（`valid_until < 今天` → 409，要重报新单） |
//! | `ACCEPTED → CANCELLED` | 未转销售订单（`converted_so_id IS NULL`） | 已转单的报价作废，销售订单就失去了来源（M3-03 回链） |
//!
//! 「已过期」一律**现算**（`today > valid_until`，按 CST），不进状态机、不落状态列 ——
//! M3-01 建表注释的既定口径（做过期状态就要定时翻转 + 时钟回拨对账）。
//!
//! # 不碰的东西
//!
//! - **不转销售订单**：转单是 M3-03（本卡只回链 `converted_so_id` 的读取与守卫）；
//! - **不接审批流**：审批是 M3-04（本卡 `DocKind::Quotation::is_document() = false`）；
//! - **不写库存 / 不生成凭证**：报价阶段客户还没下单，一分钱一分货都没动。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_quotation / biz_quotation_line 是 ERP 单据表（V0432 无 dept_id /
//! 归属人列），数据范围表达不出来；可见性由 business:erp:quotation:* 权限码控制。
//! ```

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::customer::customer_model::{self, CustomerRow, STATUS_ACTIVE};
use crate::business::erp::inventory_transit::model as transit_model;
use crate::business::erp::sales::quotation_model::{
    self as model, NewLine, NewQuotation, QuotationLineRow, QuotationRow, Totals,
};
use cloudpivot_kernel::business::shared::state_machine;
use cloudpivot_kernel::shared::error_code;

// ==================== 权限码（与 V0433 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 / 详情。
pub const PERM_LIST: &str = "business:erp:quotation:list";
/// 新建。
pub const PERM_CREATE: &str = "business:erp:quotation:create";
/// 编辑（只有 DRAFT）。
pub const PERM_UPDATE: &str = "business:erp:quotation:update";
/// 删除（软删，只有 DRAFT / CANCELLED）。
pub const PERM_DELETE: &str = "business:erp:quotation:delete";
/// 状态流转。
pub const PERM_STATUS: &str = "business:erp:quotation:status";

/// 报价单域全部权限码（顺序 = V0433 的 VALUES 顺序；`perm_menu_gate.py` 要求每个都在
/// `sys_menu` 有承载行）。
pub const QUOTATION_PERMISSIONS: [&str; 5] = [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_DELETE, PERM_STATUS];

/// 一张报价单最多多少行明细（与销售订单 / 采购单同口径：超过就是导入脚本出问题了）。
pub const MAX_LINES: usize = 200;

// ==================== 校验原语 ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `404`：报价单不存在 / 已删。
fn not_found() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "报价单不存在或已删除".to_string())
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
            format!("{field} 不是合法 UUID：`{text}`"),
        )
    })
}

/// 请求体里的日期：`yyyy-MM-dd`；非法 → 400（不做「猜格式」）。
fn parse_date(raw: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d").map(Some).map_err(|_| {
            ApiError::new(
                400,
                error_code::ERP_FILTER_INVALID,
                format!("{field} 必须是 yyyy-MM-dd（当前 `{text}`）"),
            )
        }),
    }
}

/// 写库错误：唯一键 / CHECK 违反翻译成 409 / 400，其余交给底座（5xx 带上下文）。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match err {
        // 存活行单号唯一 / 行号单内唯一
        DbError::UniqueViolation { .. } => conflict(format!("{ctx}失败：单号或行号冲突，请重试")),
        // ck_biz_quotation_* / ck_biz_quotation_line_*
        DbError::InvalidValue(text) => bad_request(format!("{ctx}失败：{text}")),
        other => cloudpivot_kernel::utils::db_error(other, ctx),
    }
}

// ==================== 入参 DTO ====================

/// 一行明细入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct QuotationLineReq {
    /// 物料 id（UUID；弱引用 `biz_material.id`）。
    pub material_id: String,
    /// 报价数量（> 0）。
    #[serde(default)]
    pub qty: Option<Decimal>,
    /// 不含税单价（≥ 0，留空 = 0）。
    #[serde(default)]
    pub price: Option<Decimal>,
    /// 税率百分数（0–100，留空 = 0）。
    #[serde(default)]
    pub tax_rate: Option<Decimal>,
    /// 行备注（交期 / 替代料说明等）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 新建报价单。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateQuotationReq {
    /// 客户 id（UUID；必须存在、未软删且 `ACTIVE`）。
    pub customer_id: String,
    /// 报价日期（`yyyy-MM-dd`，留空 = 今天 CST）。
    #[serde(default)]
    pub quote_date: Option<String>,
    /// 有效期至（**必填**，`yyyy-MM-dd`，`>= quote_date`）。
    ///
    /// 不默认：有效期是报价的关键条款（给多少天是商业决定），
    /// 默认一个值等于替销售做了承诺 —— 客户拿着「30 天有效」来压价时，
    /// 系统里到底约定过几天必须是本人录进去的。
    pub valid_until: String,
    /// 备注（付款条件 / 报价说明等）。
    #[serde(default)]
    pub remark: Option<String>,
    /// 明细（**至少 1 行**，最多 [`MAX_LINES`] 行）。
    #[serde(default)]
    pub items: Vec<QuotationLineReq>,
}

/// 编辑报价单（**只有 DRAFT**；明细传了就整单替换）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateQuotationReq {
    /// 客户 id（**不传 = 不改**）。
    #[serde(default)]
    pub customer_id: Option<String>,
    /// 报价日期（不传 = 不改）。
    #[serde(default)]
    pub quote_date: Option<String>,
    /// 有效期至（不传 = 不改）。
    #[serde(default)]
    pub valid_until: Option<String>,
    /// 备注（传空串 = 清空）。
    #[serde(default)]
    pub remark: Option<String>,
    /// 明细（**不传 = 保留现有明细**；传了 = 整单替换，不能为空）。
    #[serde(default)]
    pub items: Option<Vec<QuotationLineReq>>,
}

/// 状态流转请求。
#[derive(Debug, Clone, Deserialize)]
pub struct QuotationStatusReq {
    /// 目标状态（5 值之一，且必须是 `LEGAL_EDGES` 里的合法边）。
    pub target_status: String,
    /// 原因（客户拒绝 / 已发出后作废必填）。
    #[serde(default)]
    pub reason: Option<String>,
}

/// 分页筛选。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct QuotationFilter {
    /// 单号模糊。
    #[serde(default)]
    pub quote_no: Option<String>,
    /// 状态精确（5 值之一）。
    #[serde(default)]
    pub status: Option<String>,
    /// 客户精确。
    #[serde(default)]
    pub customer_id: Option<String>,
    /// 有效期筛选：`true` = 只要已过期、`false` = 只要未过期（含今天）、不传 = 全部。
    ///
    /// 「已过期」现算（`valid_until < 今天`），不是库里的状态值。
    #[serde(default)]
    pub expired: Option<bool>,
    /// 页码（1 起）。
    #[serde(default)]
    pub page: u64,
    /// 每页条数（0 = 默认 20，上限 200）。
    #[serde(default)]
    pub page_size: u64,
}

// ==================== 出参 DTO ====================

/// 明细出参。
#[derive(Debug, Clone, Serialize)]
pub struct QuotationLineRes {
    /// 主键。
    pub id: String,
    /// 行号（1 起）。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码（软删物料 → `None`）。
    pub material_code: Option<String>,
    /// 物料名称（软删物料 → `None`）。
    pub material_name: Option<String>,
    /// 规格。
    pub spec: Option<String>,
    /// 计量单位名。
    pub unit_name: Option<String>,
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
    /// 行含税金额（= `amount + tax_amount`）。
    pub with_tax: Decimal,
    /// 行备注。
    pub remark: Option<String>,
}

impl QuotationLineRes {
    /// 由行结构构造（物料名由调用方批量补齐，避免 N+1）。
    fn of(row: &QuotationLineRow) -> Self {
        Self {
            id: row.id.to_string(),
            line_no: row.line_no,
            material_id: row.material_id.to_string(),
            material_code: None,
            material_name: None,
            spec: None,
            unit_name: None,
            qty: row.qty,
            price: row.price,
            amount: row.amount,
            tax_rate: row.tax_rate,
            tax_amount: row.tax_amount,
            with_tax: row.with_tax(),
            remark: row.remark.clone(),
        }
    }
}

/// 单头出参。
#[derive(Debug, Clone, Serialize)]
pub struct QuotationRes {
    /// 主键。
    pub id: String,
    /// 单号。
    pub quote_no: String,
    /// 客户 id。
    pub customer_id: String,
    /// 客户编码（软删客户 → `None`）。
    pub customer_code: Option<String>,
    /// 客户名称（软删客户 → `None`）。
    pub customer_name: Option<String>,
    /// 报价日期。
    pub quote_date: NaiveDate,
    /// 有效期至（含当天）。
    pub valid_until: NaiveDate,
    /// **是否已过期**（现算 `today > valid_until`，不是状态）。
    pub expired: bool,
    /// 状态（5 值之一）。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 不含税合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计。
    pub total_with_tax: Decimal,
    /// 转成的销售订单 id（M3-03 回填；`None` = 未转单）。
    pub converted_so_id: Option<String>,
    /// 转单时间。
    pub converted_at: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 现在能不能改（= 状态是 `DRAFT`）。
    pub editable: bool,
    /// 现在能不能删（= 状态是 `DRAFT` / `CANCELLED`）。
    pub deletable: bool,
    /// 现在能流转到哪些状态（前端按它显示按钮；空 = 终态）。
    pub allowed_targets: Vec<String>,
    /// 明细（列表页为空数组，详情页带全量）。
    pub items: Vec<QuotationLineRes>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<String>,
    /// 更新人。
    pub update_by: Option<String>,
}

impl QuotationRes {
    /// 由行结构构造（客户 / 物料名由调用方补齐）。
    fn of(row: &QuotationRow, items: Vec<QuotationLineRes>, today: NaiveDate) -> Self {
        Self {
            id: row.id.to_string(),
            quote_no: row.quote_no.clone(),
            customer_id: row.customer_id.to_string(),
            customer_code: None,
            customer_name: None,
            quote_date: row.quote_date,
            valid_until: row.valid_until,
            expired: row.expired_on(today),
            status: row.status.clone(),
            status_label: model::status_label(&row.status).to_string(),
            total_amount: row.total_amount,
            total_tax: row.total_tax,
            total_with_tax: row.total_with_tax,
            converted_so_id: row.converted_so_id.map(|id| id.to_string()),
            converted_at: row.converted_at.map(|at| at.to_rfc3339()),
            remark: row.remark.clone(),
            editable: row.status == model::STATUS_DRAFT,
            deletable: model::DELETABLE_STATUSES.contains(&row.status.as_str()),
            allowed_targets: model::allowed_targets(&row.status)
                .into_iter()
                .map(str::to_string)
                .collect(),
            items,
            created_at: row.created_at,
            updated_at: row.updated_at,
            create_by: row.create_by.map(|id| id.to_string()),
            update_by: row.update_by.map(|id| id.to_string()),
        }
    }
}

// ==================== 纯逻辑：入参 → 行结构 ====================

/// 一行入参 → [`NewLine`]（金额在这里算，**不接受客户端传金额**）。
///
/// 校验与库层 CHECK 逐条对应，但给人话：
/// `qty > 0`（`ck_biz_quotation_line_qty`）、`price >= 0`、
/// `tax_rate ∈ [0, 100]`（`ck_biz_quotation_line_rate`）。
fn build_line(req: &QuotationLineReq, index: usize) -> std::result::Result<NewLine, ApiError> {
    let line_no = index + 1;
    let material_id = parse_uuid(&req.material_id, &format!("items[{index}].material_id"))?;
    let qty = req.qty.unwrap_or(Decimal::ZERO);
    if qty <= Decimal::ZERO {
        return Err(bad_request(format!("第 {line_no} 行数量必须大于 0（当前 {qty}）")));
    }
    let price = req.price.unwrap_or(Decimal::ZERO);
    if price < Decimal::ZERO {
        return Err(bad_request(format!("第 {line_no} 行单价不能为负（当前 {price}）")));
    }
    let tax_rate = req.tax_rate.unwrap_or(Decimal::ZERO);
    if tax_rate < Decimal::ZERO || tax_rate > Decimal::from(100) {
        return Err(bad_request(format!(
            "第 {line_no} 行税率必须在 0–100 之间（当前 {tax_rate}）"
        )));
    }
    let (amount, tax_amount) = model::line_amounts(qty, price, tax_rate);
    Ok(NewLine {
        material_id,
        qty,
        price,
        tax_rate,
        amount,
        tax_amount,
        remark: req
            .remark
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string),
    })
}

/// 明细入参整体校验 + 转换（至少 1 行、最多 [`MAX_LINES`] 行）。
fn build_lines(items: &[QuotationLineReq], allow_empty: bool) -> std::result::Result<Vec<NewLine>, ApiError> {
    if items.is_empty() {
        if allow_empty {
            return Ok(Vec::new());
        }
        return Err(bad_request("报价单至少要 1 行明细"));
    }
    if items.len() > MAX_LINES {
        return Err(bad_request(format!(
            "一张报价单最多 {MAX_LINES} 行明细（当前 {} 行）",
            items.len()
        )));
    }
    items
        .iter()
        .enumerate()
        .map(|(index, req)| build_line(req, index))
        .collect()
}

// ==================== 业务守卫 ====================

/// 客户必须存在、未软删且 `ACTIVE`（冻结客户禁开新报价：与销售订单同口径）。
async fn require_active_customer(customer_id: Uuid) -> std::result::Result<CustomerRow, ApiError> {
    let customer = customer_model::customer_find_by_id(customer_id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单客户"))?
        .ok_or_else(|| bad_request("客户不存在或已删除"))?;
    if customer.status != STATUS_ACTIVE {
        return Err(bad_request(format!(
            "客户 `{}`（{}）状态是 {}，不是 ACTIVE：冻结客户不允许新开报价单",
            customer.code, customer.name, customer.status
        )));
    }
    Ok(customer)
}

/// `DRAFT → SENT` 的三道守卫：≥1 行明细 / 客户未冻结 / 有效期未过。
async fn ensure_sendable(
    row: &QuotationRow,
    lines: &[QuotationLineRow],
    today: NaiveDate,
) -> std::result::Result<(), ApiError> {
    if lines.is_empty() {
        return Err(bad_request(format!(
            "报价单 {} 没有明细，不能发出：至少要 1 行明细（空单发给客户等于报了个空价）",
            row.quote_no
        )));
    }
    require_active_customer(row.customer_id).await?;
    if row.expired_on(today) {
        return Err(conflict(format!(
            "报价单 {} 的有效期已过（valid_until = {}，今天是 {today}）：\
             请先改有效期再发出（发给客户一张天生过期的报价，客户接受时也走不通）",
            row.quote_no, row.valid_until
        )));
    }
    Ok(())
}

/// `SENT → ACCEPTED` 的守卫：有效期未过（**过期价不能被接受**）。
fn ensure_acceptable(row: &QuotationRow, today: NaiveDate) -> std::result::Result<(), ApiError> {
    if row.expired_on(today) {
        return Err(conflict(format!(
            "报价单 {} 已于 {} 过期（今天是 {today}），不能标记为「已接受」：\
             请按当前价格重新报价后再走接受（否则等于用旧价成交，财务/成本口径都会歪）",
            row.quote_no, row.valid_until
        )));
    }
    Ok(())
}

/// `ACCEPTED → CANCELLED` 的守卫：已转销售订单的不许作废。
fn ensure_cancellable(row: &QuotationRow) -> std::result::Result<(), ApiError> {
    if let Some(so_id) = row.converted_so_id {
        return Err(conflict(format!(
            "报价单 {} 已转成销售订单（{so_id}），不能作废：作废它会让那张订单失去来源。\
             要撤销成交请走销售订单的作废流程",
            row.quote_no
        )));
    }
    Ok(())
}

// ==================== 业务实现 ====================

/// 详情（单头 + 明细 + 客户名 + 物料名，软删行 404）。
async fn detail_of(id: Uuid) -> std::result::Result<QuotationRes, ApiError> {
    let today = model::today_cst(Utc::now());
    let row = model::find_quotation(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单"))?
        .ok_or_else(not_found)?;
    let lines = model::lines_of(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单明细"))?;
    let material_ids: Vec<Uuid> = lines.iter().map(|line| line.material_id).collect();
    let materials: std::collections::HashMap<Uuid, transit_model::MaterialBrief> =
        transit_model::material_labels(&material_ids)
            .await
            .map_err(|error| cloudpivot_kernel::utils::db_error(error, "批量查询报价单物料"))?
            .into_iter()
            .map(|brief| (brief.id, brief))
            .collect();
    let items: Vec<QuotationLineRes> = lines
        .iter()
        .map(|line| {
            let mut res = QuotationLineRes::of(line);
            if let Some(brief) = materials.get(&line.material_id) {
                res.material_code = Some(brief.code.clone());
                res.material_name = Some(brief.name.clone());
                res.spec = brief.spec.clone();
                res.unit_name = brief.unit_name.clone();
            }
            res
        })
        .collect();
    let mut res = QuotationRes::of(&row, items, today);
    if let Some((_, code, name)) = model::customer_labels(&[row.customer_id])
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单客户"))?
        .into_iter()
        .next()
    {
        res.customer_code = Some(code);
        res.customer_name = Some(name);
    }
    Ok(res)
}

/// 新建报价单（`DRAFT`）。
pub async fn create_quotation(
    operator: Option<Uuid>,
    req: CreateQuotationReq,
) -> std::result::Result<QuotationRes, ApiError> {
    let customer_id = parse_uuid(&req.customer_id, "customer_id")?;
    let today = model::today_cst(Utc::now());
    let quote_date = parse_date(req.quote_date.as_deref(), "quote_date")?.unwrap_or(today);
    let valid_until = parse_date(Some(&req.valid_until), "valid_until")?
        .ok_or_else(|| bad_request("valid_until（有效期至）必填：有效期是报价的关键条款"))?;
    if valid_until < quote_date {
        return Err(bad_request(format!(
            "valid_until（{valid_until}）不能早于 quote_date（{quote_date}）：\
             建单当天就过期的报价只会在客户砍价时才被发现"
        )));
    }
    let lines = build_lines(&req.items, false)?;
    let customer = require_active_customer(customer_id).await?;
    let totals = Totals::of(&lines);
    // 金额恒等在这里也过一遍：算错就是 400，而不是等库层 CHECK 报 23514（对用户是 500 手感）
    if !model::totals_consistent(totals.amount, totals.tax, totals.with_tax)
        || !model::amounts_non_negative(totals.amount, totals.tax, totals.with_tax)
    {
        return Err(bad_request(format!(
            "金额自检失败：价税合计 {} ≠ 不含税 {} + 税额 {}",
            totals.with_tax, totals.amount, totals.tax
        )));
    }
    let new = NewQuotation {
        customer_id,
        quote_date,
        valid_until,
        remark: req.remark.as_deref().map(str::trim).filter(|text| !text.is_empty()),
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "开启建单事务"))?;
    let created = model::create_quotation(
        &tx,
        &new,
        &lines,
        &totals,
        state_machine::Operator::of(operator),
        Utc::now(),
    )
    .await;
    let (id, quote_no) = match created {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "新建报价单"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "提交建单事务"))?;
    let mut res = detail_of(id).await?;
    if res.customer_code.is_none() {
        res.customer_code = Some(customer.code.clone());
        res.customer_name = Some(customer.name.clone());
    }
    debug_assert_eq!(res.quote_no, quote_no, "回读单号必须与取号结果一致");
    Ok(res)
}

/// 编辑报价单（**只有 DRAFT**；明细传了就整单替换）。
pub async fn update_quotation(
    operator: Option<Uuid>,
    id_raw: &str,
    req: UpdateQuotationReq,
) -> std::result::Result<QuotationRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let current = model::find_quotation(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询待编辑报价单"))?
        .ok_or_else(not_found)?;
    // 入账态只读闸：发出即冻结（改价要走新报价单，M3-01 建表口径）
    if current.status != model::STATUS_DRAFT {
        return Err(conflict(format!(
            "只有草稿（DRAFT）能改：当前状态是 {}（{}）。已发出的报价要改价 / 改条款，\
             请作废本单后新开一张（客户手里那份报价不能被原地改掉）",
            model::status_label(&current.status),
            current.status
        )));
    }
    let customer_id = match req
        .customer_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => current.customer_id,
        Some(raw) => parse_uuid(raw, "customer_id")?,
    };
    let quote_date = parse_date(req.quote_date.as_deref(), "quote_date")?.unwrap_or(current.quote_date);
    let valid_until = parse_date(req.valid_until.as_deref(), "valid_until")?.unwrap_or(current.valid_until);
    if valid_until < quote_date {
        return Err(bad_request(format!(
            "valid_until（{valid_until}）不能早于 quote_date（{quote_date}）"
        )));
    }
    // 备注：不传 = 沿用；传空串 = 清空
    let remark: Option<String> = match req.remark.as_deref() {
        Some(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
        None => current.remark.clone(),
    };
    // 明细：不传 = 沿用库里现有明细（金额沿用已算好的值）；传了 = 整单替换
    let lines: Vec<NewLine> = match req.items.as_ref() {
        None => model::lines_of(id)
            .await
            .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单明细"))?
            .iter()
            .map(|line| NewLine {
                material_id: line.material_id,
                qty: line.qty,
                price: line.price,
                tax_rate: line.tax_rate,
                amount: line.amount,
                tax_amount: line.tax_amount,
                remark: line.remark.clone(),
            })
            .collect(),
        Some(items) => {
            if items.is_empty() {
                return Err(bad_request("改单不能把明细清空：报价单至少要 1 行明细"));
            }
            build_lines(items, false)?
        }
    };
    if customer_id != current.customer_id {
        // 换客户也要过「存在且未冻结」这一关（冻结客户禁开新报价）
        require_active_customer(customer_id).await?;
    }
    let new = NewQuotation {
        customer_id,
        quote_date,
        valid_until,
        remark: remark.as_deref(),
    };
    let totals = Totals::of(&lines);
    let tx = db()
        .begin()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "开启改单事务"))?;
    let affected = model::update_quotation(
        &tx,
        model::UpdateQuotationInput {
            id,
            new: &new,
            lines: &lines,
            totals: &totals,
            operator,
            now: Utc::now(),
        },
    )
    .await;
    let affected = match affected {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(write_error(error, "编辑报价单"));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(conflict(
            "报价单状态在本次改单期间已被改动（不再处于草稿），请刷新后重试",
        ));
    }
    tx.commit()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "提交改单事务"))?;
    detail_of(id).await
}

/// 报价单详情（软删行 404）。
pub async fn get_quotation(id_raw: &str) -> std::result::Result<QuotationRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    detail_of(id).await
}

/// 报价单分页。
pub async fn page_quotations(filter: QuotationFilter) -> std::result::Result<PageData<Vec<QuotationRes>>, ApiError> {
    // `status` 传了非法值 → 400（静默忽略会把「已接受」筛成「全部」，是假数据）
    if let Some(status) = filter.status.as_deref().map(str::trim).filter(|text| !text.is_empty())
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
    let today = model::today_cst(Utc::now());
    let (rows, total) = model::quotation_page(&model::QuotationPageQuery {
        quote_no: filter.quote_no.as_deref(),
        status: filter.status.as_deref(),
        customer_id,
        expired: filter.expired,
        today,
        page: filter.page,
        page_size: filter.page_size,
    })
    .await
    .map_err(|error| cloudpivot_kernel::utils::db_error(error, "分页查询报价单"))?;
    // 客户编码 / 名称一次往返补齐（一页里多张单常常同客户；软删客户 → 两列 None）
    let customer_ids: Vec<Uuid> = rows.iter().map(|row| row.customer_id).collect();
    let naming: std::collections::HashMap<Uuid, (String, String)> = model::customer_labels(&customer_ids)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "批量查询报价单客户"))?
        .into_iter()
        .map(|(id, code, name)| (id, (code, name)))
        .collect();
    let (page, page_size) = model::normalize_page(filter.page, filter.page_size);
    let list = rows
        .iter()
        .map(|row| {
            let mut res = QuotationRes::of(row, Vec::new(), today);
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

/// 状态流转：边表校验 → 业务守卫 → 状态机留痕 → CAS 落库（同一事务）。
pub async fn change_quotation_status(
    operator: Option<Uuid>,
    id_raw: &str,
    req: QuotationStatusReq,
) -> std::result::Result<QuotationRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let target = req.target_status.trim().to_string();
    if !model::is_valid_status(&target) {
        return Err(bad_request(format!(
            "target_status 必须是 {} 之一（当前 `{}`）",
            model::STATUSES.join(" / "),
            req.target_status
        )));
    }
    let row = model::find_quotation(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单"))?
        .ok_or_else(not_found)?;
    let today = model::today_cst(Utc::now());

    // ① 合法边校验（非法迁移 409，文案给出当前态与可选去向）
    if !model::can_transition(&row.status, &target) {
        let targets = model::allowed_targets(&row.status);
        let message = if targets.is_empty() {
            format!(
                "报价单 {} 已是终态 {}（{}），不能再流转",
                row.quote_no,
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

    let reason = req.reason.as_deref().map(str::trim).filter(|text| !text.is_empty());

    // ② 需要原因的边（与 state_machine 的 QUOTATION_REASON_EDGES 同源，提前给人话）
    if state_machine::Entity::Quotation.needs_reason(&row.status, &target) && reason.is_none() {
        return Err(bad_request(format!(
            "{}（{}）→ {}（{}）必须给出原因（reason）：这条边对外推翻过承诺，\
             事后要能回答「为什么」",
            model::status_label(&row.status),
            row.status,
            model::status_label(&target),
            target
        )));
    }

    // ③ 业务守卫（按边逐个过）
    if row.status == model::STATUS_DRAFT && target == model::STATUS_SENT {
        let lines = model::lines_of(id)
            .await
            .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询报价单明细"))?;
        ensure_sendable(&row, &lines, today).await?;
    }
    if row.status == model::STATUS_SENT && target == model::STATUS_ACCEPTED {
        ensure_acceptable(&row, today)?;
    }
    if target == model::STATUS_CANCELLED {
        ensure_cancellable(&row)?;
    }

    // ④ 状态机留痕 + CAS 落库（同一事务）
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "开启状态流转事务"))?;
    if let Err(error) = state_machine::transition(
        &tx,
        state_machine::ENTITY_QUOTATION,
        id,
        &row.quote_no,
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
                "报价单 {} 的状态在本次流转期间已被改动（期望 {}），请刷新后重试",
                row.quote_no, row.status
            )))
        }
        Ok(_) => {
            tx.commit()
                .await
                .map_err(|error| cloudpivot_kernel::utils::db_error(error, "提交状态流转事务"))?;
            detail_of(id).await
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "报价单状态流转"))
        }
    }
}

/// 软删报价单（**只有 DRAFT / CANCELLED**）。
pub async fn delete_quotation(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let row = model::find_quotation(id)
        .await
        .map_err(|error| cloudpivot_kernel::utils::db_error(error, "查询待删除报价单"))?
        .ok_or_else(not_found)?;
    if !model::DELETABLE_STATUSES.contains(&row.status.as_str()) {
        return Err(conflict(format!(
            "报价单 {} 当前状态是 {}（{}），不能删除：只有 {} 能删。\
             已发出的报价请先作废（CANCELLED，需给原因），历史才留得住",
            row.quote_no,
            model::status_label(&row.status),
            row.status,
            model::DELETABLE_STATUSES
                .iter()
                .map(|code| format!("{code}（{}）", model::status_label(code)))
                .collect::<Vec<_>>()
                .join(" / ")
        )));
    }
    let affected = model::soft_delete(db(), id, &row.status, operator, Utc::now())
        .await
        .map_err(|error| write_error(error, "删除报价单"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "报价单 {} 的状态在本次删除期间已被改动（期望 {}），请刷新后重试",
            row.quote_no, row.status
        )));
    }
    Ok(())
}

// ==================== HTTP 入口 ====================

/// 报价单分页。
#[nik::permission(
    path = "/business/erp/quotation/page",
    method = "GET",
    permission = "business:erp:quotation:list"
)]
#[nik::get("/business/erp/quotation/page")]
pub async fn quotation_page(Query(filter): Query<QuotationFilter>) -> ResResult<PageData<Vec<QuotationRes>>> {
    Ok(Res::ok(page_quotations(filter).await?))
}

/// 报价单详情。
#[nik::permission(
    path = "/business/erp/quotation/{id}",
    method = "GET",
    permission = "business:erp:quotation:list"
)]
#[nik::get("/business/erp/quotation/{id}")]
pub async fn quotation_detail(Path(id): Path<String>) -> ResResult<QuotationRes> {
    Ok(Res::ok(get_quotation(&id).await?))
}

/// 新建报价单。
#[nik::permission(
    path = "/business/erp/quotation",
    method = "POST",
    permission = "business:erp:quotation:create"
)]
#[nik::post("/business/erp/quotation")]
pub async fn quotation_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateQuotationReq>,
) -> ResResult<QuotationRes> {
    let operator = cloudpivot_kernel::utils::current_user_uuid(&claims);
    Ok(Res::ok(create_quotation(operator, req).await?))
}

/// 编辑报价单（只有 DRAFT）。
#[nik::permission(
    path = "/business/erp/quotation/{id}",
    method = "PUT",
    permission = "business:erp:quotation:update"
)]
#[nik::put("/business/erp/quotation/{id}")]
pub async fn quotation_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateQuotationReq>,
) -> ResResult<QuotationRes> {
    let operator = cloudpivot_kernel::utils::current_user_uuid(&claims);
    Ok(Res::ok(update_quotation(operator, &id, req).await?))
}

/// 状态流转（非法迁移 409）。
#[nik::permission(
    path = "/business/erp/quotation/{id}/status",
    method = "PATCH",
    permission = "business:erp:quotation:status"
)]
#[nik::patch("/business/erp/quotation/{id}/status")]
pub async fn quotation_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<QuotationStatusReq>,
) -> ResResult<QuotationRes> {
    let operator = cloudpivot_kernel::utils::current_user_uuid(&claims);
    Ok(Res::ok(change_quotation_status(operator, &id, req).await?))
}

/// 删除报价单（软删，只有 DRAFT / CANCELLED）。
#[nik::permission(
    path = "/business/erp/quotation/{id}",
    method = "DELETE",
    permission = "business:erp:quotation:delete"
)]
#[nik::delete("/business/erp/quotation/{id}")]
pub async fn quotation_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = cloudpivot_kernel::utils::current_user_uuid(&claims);
    Ok(Res::ok(delete_quotation(operator, &id).await?))
}

/// 权限码自检：5 个码、无重复、前缀 `business:erp:quotation:`（V0433 的菜单行逐字对应）。
pub fn all_permissions() -> Vec<&'static str> {
    QUOTATION_PERMISSIONS.to_vec()
}

// ==================== 单测（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    fn line_req(qty: &str, price: &str, rate: &str) -> QuotationLineReq {
        QuotationLineReq {
            material_id: Uuid::now_v7().to_string(),
            qty: Some(dec(qty)),
            price: Some(dec(price)),
            tax_rate: Some(dec(rate)),
            remark: None,
        }
    }

    /// 权限码：5 个、无重复、统一前缀（与 V0433 的 sys_menu 行逐字一致）。
    #[test]
    fn permission_codes_are_unique_and_prefixed() {
        assert_eq!(QUOTATION_PERMISSIONS.len(), 5);
        assert_eq!(all_permissions().len(), 5);
        let prefix = "business:erp:quotation:";
        let mut seen = std::collections::BTreeSet::new();
        for code in QUOTATION_PERMISSIONS {
            assert!(code.starts_with(prefix), "{code} 前缀必须统一");
            assert!(seen.insert(code), "{code} 重复");
        }
        for suffix in ["list", "create", "update", "delete", "status"] {
            assert!(
                QUOTATION_PERMISSIONS.contains(&format!("{prefix}{suffix}").as_str()),
                "缺权限码 {prefix}{suffix}"
            );
        }
    }

    /// 行入参校验：数量 > 0 / 单价非负 / 税率 0-100，金额服务端算。
    #[test]
    fn build_line_rejects_bad_input_and_computes_amounts() {
        // 数量 0 / 负数
        let err = build_line(&line_req("0", "10", "13"), 0).expect_err("数量 0 必须被拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("数量必须大于 0"), "文案要指名字段：{}", err.msg);
        let err = build_line(&line_req("-1", "10", "13"), 0).expect_err("负数数量必须被拒");
        assert_eq!(err.status, 400);
        // 单价负数
        let err = build_line(&line_req("1", "-0.01", "13"), 1).expect_err("负单价必须被拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("第 2 行"), "文案要带行号：{}", err.msg);
        // 税率越界
        let err = build_line(&line_req("1", "10", "100.0001"), 0).expect_err("税率 > 100 必须被拒");
        assert_eq!(err.status, 400);
        let err = build_line(&line_req("1", "10", "-0.0001"), 0).expect_err("负税率必须被拒");
        assert_eq!(err.status, 400);
        // 物料 id 非法
        let mut bad = line_req("1", "10", "13");
        bad.material_id = "not-a-uuid".to_string();
        let err = build_line(&bad, 0).expect_err("非法物料 id 必须被拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::ERP_FILTER_INVALID);

        // 正常：金额由服务端算（3 × 17.655、税 13%）
        let line = build_line(&line_req("3", "17.655", "13"), 0).expect("正常行应通过");
        assert_eq!(line.amount, dec("52.9650"));
        assert_eq!(line.tax_amount, dec("6.8855"));
        // 留空的 price / tax_rate 按 0 处理（不是「不传就报错」）
        let mut zero = line_req("2", "0", "0");
        zero.price = None;
        zero.tax_rate = None;
        let line = build_line(&zero, 0).expect("缺省价税按 0");
        assert_eq!((line.amount, line.tax_amount), (Decimal::ZERO, Decimal::ZERO));
    }

    /// 明细整体：空明细拒（除非明确允许）、超 200 行拒。
    #[test]
    fn build_lines_enforces_count_bounds() {
        let err = build_lines(&[], false).expect_err("空明细必须被拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("至少要 1 行"));
        assert!(build_lines(&[], true).expect("允许空时返回空表").is_empty());
        let many: Vec<QuotationLineReq> = (0..=MAX_LINES).map(|_| line_req("1", "1", "0")).collect();
        let err = build_lines(&many, false).expect_err("超过上限必须被拒");
        assert!(err.msg.contains("最多 200 行"));
        let ok: Vec<QuotationLineReq> = (0..MAX_LINES).map(|_| line_req("1", "1", "0")).collect();
        assert_eq!(build_lines(&ok, false).expect("200 行应通过").len(), MAX_LINES);
    }

    /// 日期解析：严格 `yyyy-MM-dd`，空串当没传。
    #[test]
    fn parse_date_is_strict() {
        assert_eq!(
            parse_date(Some("2026-11-17"), "quote_date").expect("合法日期"),
            NaiveDate::from_ymd_opt(2026, 11, 17)
        );
        assert!(parse_date(Some("   "), "quote_date").expect("空串 = 没传").is_none());
        assert!(parse_date(None, "quote_date").expect("None 合法").is_none());
        for bad in ["2026/11/17", "20261117", "2026-13-01", "yesterday"] {
            let err = parse_date(Some(bad), "quote_date").expect_err("非法日期必须被拒");
            assert_eq!(err.status, 400);
            assert_eq!(err.code, error_code::ERP_FILTER_INVALID);
        }
    }

    /// UUID 解析：空串与非法值都报 400（绝不静默当作「没传」）。
    #[test]
    fn parse_uuid_never_silently_ignores() {
        let id = Uuid::now_v7();
        assert_eq!(parse_uuid(&id.to_string(), "id").expect("合法"), id);
        let err = parse_uuid("", "id").expect_err("空串必须拒");
        assert_eq!(err.status, 400);
        let err = parse_uuid("  abc  ", "customer_id").expect_err("非法值必须拒");
        assert_eq!(err.code, error_code::ERP_FILTER_INVALID);
        assert!(err.msg.contains("customer_id"), "文案要指名字段");
    }
}
