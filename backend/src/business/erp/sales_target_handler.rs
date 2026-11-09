//! 销售目标 / 预测接口层（SO-44 / ARCH-7 §15.8）：目标维护 + 预测维护 + 达成率看板。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/sales-target/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/sales-target/page` | `business:erp:sales-target:list` | 目标分页 |
//! | GET | `/business/erp/sales-target/achievement` | `business:erp:sales-target:list` | **达成率看板**（两个口径一次给全） |
//! | POST | `/business/erp/sales-target` | `business:erp:sales-target:create` | 新建目标 |
//! | PUT | `/business/erp/sales-target/{id}` | `business:erp:sales-target:update` | 编辑 / 停用（部分更新） |
//! | DELETE | `/business/erp/sales-target/{id}` | `business:erp:sales-target:delete` | 软删（录错了才用） |
//! | GET | `/business/erp/sales-forecast/page` | `business:erp:sales-forecast:list` | 预测分页 |
//! | POST | `/business/erp/sales-forecast` | `business:erp:sales-forecast:create` | 新建预测 |
//! | PUT | `/business/erp/sales-forecast/{id}` | `business:erp:sales-forecast:update` | 编辑预测 |
//! | DELETE | `/business/erp/sales-forecast/{id}` | `business:erp:sales-forecast:delete` | 软删预测 |
//!
//! # 达成率看板的口径（一张表说清，改之前先读）
//!
//! ```text
//! 订单口径 ORDER   ：期间内 order_date 落在区间的订单，status ∈ APPROVED/PARTIAL_OUT/OUTBOUND/CLOSED
//!                    金额 = total_with_tax（含税，与销售的"业绩口径"一致）
//! 收款口径 RECEIPT ：期间内 receipt_date 落在区间、status ∈ CONFIRMED/POSTED 的收款核销额，
//!                    减去同期退款冲减额（净额）
//! ```
//!
//! **两个口径每次都一起算、一起返回**：`actual_amount` / `achievement_rate` / `gap` 是按
//! 请求参数 `basis` 选中的那一份，`by_order` / `by_receipt` 两个都在 —— 前端切口径
//! 不必重新请求，也永远不会出现"两个口径的请求各算一次、数字对不上"的情况。
//!
//! 归属规则（详见 `sales_target_model` 模块头注释）：订单口径按 `salesperson_id`
//! （部门口径经 `sys_user.dept_id`，物料口径经明细 `material_id`）；收款口径顺单据链
//! `收款 → 应收 → 出库(source_type=SALES) → 发货 → 订单` 反查销售员，链断的收款
//! （手工挂账 / 未关联发货）只进 ALL 口径。
//!
//! # 业务逻辑为什么是普通 `async fn`
//!
//! 与 `commission_rule_handler` 同一范式：axum handler 要 `AuthClaims` + `Json`，
//! 单测调不动；把「校验 + 落库」收进 [`create_target`] / [`update_target`] /
//! [`delete_target`] / [`achievement`] 之后，单测走的是**同一条代码路径**（只跳过认证）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_sales_target / biz_sales_forecast 是配置表（scope_id 是"被统计对象"，
//!         不是数据归属列），可见性由 business:erp:sales-target:* / sales-forecast:* 决定。
//! ```

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model::normalize_page;
use crate::business::erp::sales_target_dto::{
    AchievementBasis, AchievementQuery, AchievementRow, BASIS_RECEIPT, ForecastCreateReq, ForecastFilter,
    ForecastPageQuery, ForecastRes, ForecastUpdateReq, Period, SCOPE_ALL, TargetCreateReq, TargetFilter,
    TargetPageQuery, TargetRes, TargetUpdateReq, achievement_rate, check_amount, check_qty, gap, money2,
    normalize_basis, normalize_confidence, normalize_period, normalize_scope, normalize_status,
};
use crate::business::erp::sales_target_model::{self as model, ForecastListRow, ForecastRow, TargetListRow, TargetRow};
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0318 的 sys_menu.auth_code 逐字一致） ====================

/// 目标列表 + 达成率看板。
pub const PERM_TARGET_LIST: &str = "business:erp:sales-target:list";
/// 新建目标。
pub const PERM_TARGET_CREATE: &str = "business:erp:sales-target:create";
/// 编辑 / 停用目标。
pub const PERM_TARGET_UPDATE: &str = "business:erp:sales-target:update";
/// 软删目标。
pub const PERM_TARGET_DELETE: &str = "business:erp:sales-target:delete";
/// 预测列表。
pub const PERM_FORECAST_LIST: &str = "business:erp:sales-forecast:list";
/// 新建预测。
pub const PERM_FORECAST_CREATE: &str = "business:erp:sales-forecast:create";
/// 编辑预测。
pub const PERM_FORECAST_UPDATE: &str = "business:erp:sales-forecast:update";
/// 软删预测。
pub const PERM_FORECAST_DELETE: &str = "business:erp:sales-forecast:delete";

/// 本模块全部权限码（`perm_menu_gate` 要求每个都在 `sys_menu` 有承载行）。
pub const SALES_TARGET_PERMISSIONS: [&str; 8] = [
    PERM_TARGET_LIST,
    PERM_TARGET_CREATE,
    PERM_TARGET_UPDATE,
    PERM_TARGET_DELETE,
    PERM_FORECAST_LIST,
    PERM_FORECAST_CREATE,
    PERM_FORECAST_UPDATE,
    PERM_FORECAST_DELETE,
];

// ==================== 错误出口 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

fn not_found_target(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("销售目标不存在或已删除（{id}）"),
    )
}

fn not_found_forecast(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("销售预测不存在或已删除（{id}）"),
    )
}

/// 落库错误 → 统一错误码；唯一索引冲突翻成 409（这个业务错误用户必须看懂）。
fn write_error(error: DbError, action: &str) -> ApiError {
    match error {
        DbError::UniqueViolation { .. } => conflict(format!(
            "{action}失败：同一范围 / 维度在同期已有一条（目标按「范围 + 期间」唯一，\
             预测按「客户 + 物料 + 期间 + 版本」唯一）。改那一条，或者换个期间 / 版本号"
        )),
        other => utils::db_error(other, action),
    }
}

// ==================== 出参组装 ====================

/// 目标行 → 出参（`scope_name` 为空时用 uuid 兜底，列表里不出现空白格）。
pub fn to_target_res(row: &TargetListRow) -> TargetRes {
    let (target, scope_name) = row;
    TargetRes {
        id: target.id.to_string(),
        target_no: target.target_no.clone(),
        scope_type: target.scope_type.clone(),
        scope_id: target.scope_id.map(|id| id.to_string()),
        scope_name: scope_name.clone().or_else(|| target.scope_id.map(|id| id.to_string())),
        period_code: target.period_code.clone(),
        target_amount: target.target_amount,
        target_qty: target.target_qty,
        status: target.status.clone(),
        remark: target.remark.clone(),
        created_at: target.created_at,
        updated_at: target.updated_at,
    }
}

/// 预测行 → 出参。
pub fn to_forecast_res(row: &ForecastListRow) -> ForecastRes {
    let (forecast, customer_name, material_name) = row;
    ForecastRes {
        id: forecast.id.to_string(),
        forecast_no: forecast.forecast_no.clone(),
        customer_id: forecast.customer_id.map(|id| id.to_string()),
        customer_name: customer_name.clone(),
        material_id: forecast.material_id.map(|id| id.to_string()),
        material_name: material_name.clone(),
        period_code: forecast.period_code.clone(),
        forecast_amount: forecast.forecast_amount,
        confidence: forecast.confidence.clone(),
        version_no: forecast.version_no,
        remark: forecast.remark.clone(),
        created_at: forecast.created_at,
        updated_at: forecast.updated_at,
    }
}

// ==================== 目标：业务逻辑 ====================

/// 范围对象存在性校验（写错表是静默失效，必须在入口挡住）。
async fn ensure_scope_target(scope_type: &str, scope_id: Option<Uuid>) -> std::result::Result<(), ApiError> {
    let Some(scope_id) = scope_id else {
        return Ok(());
    };
    let exists = model::scope_target_exists(scope_type, scope_id)
        .await
        .map_err(|error| utils::db_error(error, "校验销售目标的范围对象"))?;
    if exists {
        return Ok(());
    }
    Err(bad_request(format!(
        "scope_type={scope_type} 的 scope_id（{scope_id}）指向的对象不存在或已删除"
    )))
}

/// 备注清洗（空白串 → `None`）。
fn clean_remark(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

/// 版本号校验：`version_no >= 1`（与 `ck_biz_sf_version` 逐字一致）。
///
/// 不校验会让库侧 CHECK 直接把请求打成 **500**（实测：`POST` 带 `version_no=0`
/// → `check constraint violated (constraint 'ck_biz_sf_version')`）。
/// 这是入参错误，必须是 400。
fn check_version_no(value: i32) -> std::result::Result<i32, ApiError> {
    if value < 1 {
        return Err(bad_request(format!("version_no 必须 >= 1（收到 {value}）")));
    }
    Ok(value)
}

/// 新建目标。
pub async fn create_target(operator: Option<Uuid>, req: TargetCreateReq) -> std::result::Result<TargetRes, ApiError> {
    let period = Period::from_code(&req.period_code)?;
    let (scope_type, scope_id) = normalize_scope(&req.scope_type, req.scope_id.as_deref())?;
    let status = normalize_status(req.status.as_deref())?;
    check_amount(req.target_amount, "target_amount")?;
    let target_qty = req.target_qty.unwrap_or_default();
    check_qty(target_qty, "target_qty")?;
    ensure_scope_target(&scope_type, scope_id).await?;

    let id = Uuid::now_v7();
    let target_no = model::next_target_no(now_utc_time())
        .await
        .map_err(|error| utils::db_error(error, "取销售目标编号"))?;
    let row = TargetRow {
        id,
        target_no: target_no.clone(),
        scope_type,
        scope_id,
        period_code: period.code,
        target_amount: money2(req.target_amount),
        target_qty,
        status,
        remark: clean_remark(req.remark.as_deref()),
        created_at: now_utc_time(),
        updated_at: now_utc_time(),
    };
    model::insert_target(id, &target_no, &row, operator)
        .await
        .map_err(|error| write_error(error, "新建销售目标"))?;
    read_back_target(id).await
}

/// 编辑目标（部分更新：只改传了的字段；`null` 清空三态字段）。
pub async fn update_target(
    operator: Option<Uuid>,
    id_raw: &str,
    req: TargetUpdateReq,
) -> std::result::Result<TargetRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let current = model::find_target(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑的销售目标"))?
        .ok_or_else(|| not_found_target(id))?;
    let (current, _) = current;

    // 三态合并：scope_type / scope_id 一起改（只改一个会让「范围 + 对象」自相矛盾）
    let raw_scope_type = req.scope_type.clone().unwrap_or_else(|| current.scope_type.clone());
    let raw_scope_id: Option<String> = match req.scope_id.clone() {
        Some(Some(raw)) => Some(raw),
        Some(None) => None,
        None => current.scope_id.map(|value| value.to_string()),
    };
    let (scope_type, scope_id) = normalize_scope(&raw_scope_type, raw_scope_id.as_deref())?;

    let period = match req.period_code.clone() {
        Some(raw) => Period::from_code(&raw)?,
        None => Period::from_code(&current.period_code)?,
    };
    let status = match req.status.as_deref() {
        Some(raw) => normalize_status(Some(raw))?,
        None => current.status.clone(),
    };
    let target_amount = match req.target_amount {
        Some(value) => {
            check_amount(value, "target_amount")?;
            money2(value)
        }
        None => current.target_amount,
    };
    let target_qty = match req.target_qty {
        Some(value) => {
            check_qty(value, "target_qty")?;
            value
        }
        None => current.target_qty,
    };
    let remark = match req.remark {
        Some(value) => clean_remark(value.as_deref()),
        None => current.remark.clone(),
    };

    // 范围变了才再查一次对象存在性（省一次往返，语义不变）
    if scope_type != current.scope_type || scope_id != current.scope_id {
        ensure_scope_target(&scope_type, scope_id).await?;
    }

    let row = TargetRow {
        id,
        target_no: current.target_no.clone(),
        scope_type,
        scope_id,
        period_code: period.code,
        target_amount,
        target_qty,
        status,
        remark,
        created_at: current.created_at,
        updated_at: now_utc_time(),
    };
    let affected = model::update_target(id, &row, operator)
        .await
        .map_err(|error| write_error(error, "编辑销售目标"))?;
    if affected == 0 {
        return Err(not_found_target(id));
    }
    read_back_target(id).await
}

/// 软删目标。
pub async fn delete_target(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let affected = model::soft_delete_target(id, operator)
        .await
        .map_err(|error| write_error(error, "删除销售目标"))?;
    if affected == 0 {
        return Err(not_found_target(id));
    }
    Ok(())
}

/// 目标分页。
pub async fn page_targets(query: TargetPageQuery) -> std::result::Result<PageData<Vec<TargetRes>>, ApiError> {
    let mut filter: TargetFilter = query.filter()?;
    let (page, page_size) = normalize_page(filter.page, filter.page_size);
    filter.page = page;
    filter.page_size = page_size;
    let (rows, total) = model::page_targets(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询销售目标"))?;
    Ok(PageData {
        list: rows.iter().map(to_target_res).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

async fn read_back_target(id: Uuid) -> std::result::Result<TargetRes, ApiError> {
    model::find_target(id)
        .await
        .map_err(|error| utils::db_error(error, "回读销售目标"))?
        .map(|row| to_target_res(&row))
        .ok_or_else(|| not_found_target(id))
}

// ==================== 预测：业务逻辑 ====================

/// 新建预测。
pub async fn create_forecast(
    operator: Option<Uuid>,
    req: ForecastCreateReq,
) -> std::result::Result<ForecastRes, ApiError> {
    let period = Period::from_code(&req.period_code)?;
    check_amount(req.forecast_amount, "forecast_amount")?;
    let confidence = normalize_confidence(req.confidence.as_deref())?;
    let customer_id = parse_optional_uuid(req.customer_id.as_deref(), "customer_id")?;
    let material_id = parse_optional_uuid(req.material_id.as_deref(), "material_id")?;
    let version_no = check_version_no(req.version_no.unwrap_or(1))?;

    let id = Uuid::now_v7();
    let forecast_no = model::next_forecast_no(now_utc_time())
        .await
        .map_err(|error| utils::db_error(error, "取销售预测编号"))?;
    let row = ForecastRow {
        id,
        forecast_no: forecast_no.clone(),
        customer_id,
        material_id,
        period_code: period.code,
        forecast_amount: money2(req.forecast_amount),
        confidence,
        version_no,
        remark: clean_remark(req.remark.as_deref()),
        created_at: now_utc_time(),
        updated_at: now_utc_time(),
    };
    model::insert_forecast(id, &forecast_no, &row, operator)
        .await
        .map_err(|error| write_error(error, "新建销售预测"))?;
    read_back_forecast(id).await
}

/// 编辑预测（部分更新）。
pub async fn update_forecast(
    operator: Option<Uuid>,
    id_raw: &str,
    req: ForecastUpdateReq,
) -> std::result::Result<ForecastRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let current = model::find_forecast(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑的销售预测"))?
        .ok_or_else(|| not_found_forecast(id))?;
    let (current, _, _) = current;

    let customer_id = match req.customer_id {
        Some(value) => parse_optional_uuid(value.as_deref(), "customer_id")?,
        None => current.customer_id,
    };
    let material_id = match req.material_id {
        Some(value) => parse_optional_uuid(value.as_deref(), "material_id")?,
        None => current.material_id,
    };
    let period = match req.period_code.clone() {
        Some(raw) => Period::from_code(&raw)?,
        None => Period::from_code(&current.period_code)?,
    };
    let forecast_amount = match req.forecast_amount {
        Some(value) => {
            check_amount(value, "forecast_amount")?;
            money2(value)
        }
        None => current.forecast_amount,
    };
    let confidence = match req.confidence.as_deref() {
        Some(raw) => normalize_confidence(Some(raw))?,
        None => current.confidence.clone(),
    };
    let version_no = check_version_no(req.version_no.unwrap_or(current.version_no))?;
    let remark = match req.remark {
        Some(value) => clean_remark(value.as_deref()),
        None => current.remark.clone(),
    };

    let row = ForecastRow {
        id,
        forecast_no: current.forecast_no.clone(),
        customer_id,
        material_id,
        period_code: period.code,
        forecast_amount,
        confidence,
        version_no,
        remark,
        created_at: current.created_at,
        updated_at: now_utc_time(),
    };
    let affected = model::update_forecast(id, &row, operator)
        .await
        .map_err(|error| write_error(error, "编辑销售预测"))?;
    if affected == 0 {
        return Err(not_found_forecast(id));
    }
    read_back_forecast(id).await
}

/// 软删预测。
pub async fn delete_forecast(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let affected = model::soft_delete_forecast(id, operator)
        .await
        .map_err(|error| write_error(error, "删除销售预测"))?;
    if affected == 0 {
        return Err(not_found_forecast(id));
    }
    Ok(())
}

/// 预测分页。
pub async fn page_forecasts(query: ForecastPageQuery) -> std::result::Result<PageData<Vec<ForecastRes>>, ApiError> {
    let mut filter: ForecastFilter = query.filter()?;
    let (page, page_size) = normalize_page(filter.page, filter.page_size);
    filter.page = page;
    filter.page_size = page_size;
    let (rows, total) = model::page_forecasts(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询销售预测"))?;
    Ok(PageData {
        list: rows.iter().map(to_forecast_res).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

async fn read_back_forecast(id: Uuid) -> std::result::Result<ForecastRes, ApiError> {
    model::find_forecast(id)
        .await
        .map_err(|error| utils::db_error(error, "回读销售预测"))?
        .map(|row| to_forecast_res(&row))
        .ok_or_else(|| not_found_forecast(id))
}

// ==================== 达成率看板 ====================

/// 两个口径的实际数（按范围对象 id 索引；ALL 用总额）。
#[derive(Debug, Clone, Default)]
pub struct Actuals {
    /// 订单口径：范围对象 id → 实际金额。
    pub by_order: Vec<(Uuid, Decimal)>,
    /// 收款口径：范围对象 id → 实际金额。
    pub by_receipt: Vec<(Uuid, Decimal)>,
    /// 订单口径全公司总额。
    pub order_total: Decimal,
    /// 收款口径全公司总额。
    pub receipt_total: Decimal,
}

fn lookup(pairs: &[(Uuid, Decimal)], object: Option<Uuid>, total: Decimal) -> Decimal {
    match object {
        None => total,
        Some(id) => pairs
            .iter()
            .find(|(key, _)| *key == id)
            .map(|(_, value)| *value)
            .unwrap_or_default(),
    }
}

fn basis_of(target: &TargetRow, actual: Decimal) -> AchievementBasis {
    AchievementBasis {
        actual_amount: money2(actual),
        achievement_rate: achievement_rate(actual, target.target_amount),
        gap: gap(target.target_amount, actual),
    }
}

/// 组装看板行（纯函数：单测直接喂数据，不碰库）。
pub fn assemble_achievement(targets: &[TargetListRow], actuals: &Actuals, basis: &str) -> Vec<AchievementRow> {
    targets
        .iter()
        .map(|(target, scope_name)| {
            let order_actual = lookup(&actuals.by_order, target.scope_id, actuals.order_total);
            let receipt_actual = lookup(&actuals.by_receipt, target.scope_id, actuals.receipt_total);
            let by_order = basis_of(target, order_actual);
            let by_receipt = basis_of(target, receipt_actual);
            let chosen = if basis == BASIS_RECEIPT {
                by_receipt.clone()
            } else {
                by_order.clone()
            };
            AchievementRow {
                scope: scope_name
                    .clone()
                    .or_else(|| target.scope_id.map(|id| id.to_string()))
                    .unwrap_or_else(|| "全公司".to_string()),
                scope_type: target.scope_type.clone(),
                scope_id: target.scope_id.map(|id| id.to_string()),
                target_no: target.target_no.clone(),
                period_code: target.period_code.clone(),
                target_amount: target.target_amount,
                target_qty: target.target_qty,
                actual_amount: chosen.actual_amount,
                achievement_rate: chosen.achievement_rate,
                gap: chosen.gap,
                by_order,
                by_receipt,
            }
        })
        .collect()
}

/// 达成率看板。
pub async fn achievement(query: AchievementQuery) -> std::result::Result<Vec<AchievementRow>, ApiError> {
    let raw_period = query
        .period_code
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("period_code 必填（yyyy-MM 或 yyyy-QN）".to_string()))?;
    let period = Period::from_code(&normalize_period(raw_period)?)?;
    let basis = normalize_basis(query.basis.as_deref())?;
    let scope_type = match query
        .scope_type
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        Some(raw) => {
            let normalized = raw.to_ascii_uppercase();
            if !crate::business::erp::sales_target_dto::SCOPE_TYPES.contains(&normalized.as_str()) {
                return Err(bad_request(
                    "scope_type 只认 SALESPERSON / DEPT / MATERIAL_CATEGORY / ALL".to_string(),
                ));
            }
            normalized
        }
        None => SCOPE_ALL.to_string(),
    };

    let targets = model::targets_for_period(&period.code, Some(&scope_type))
        .await
        .map_err(|error| utils::db_error(error, "查询期间内的销售目标"))?;

    // 目标为空时**不再取数**：看板空列表就是空列表，别白跑四条聚合 SQL
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let mut actuals = Actuals::default();
    if scope_type == SCOPE_ALL {
        actuals.order_total = model::order_actual_total(period.from, period.to)
            .await
            .map_err(|error| utils::db_error(error, "统计订单口径实际金额"))?;
        actuals.receipt_total = model::receipt_actual_total(period.from, period.to)
            .await
            .map_err(|error| utils::db_error(error, "统计收款口径实际金额"))?;
    } else {
        actuals.by_order = model::order_actual_by_object(&scope_type, period.from, period.to)
            .await
            .map_err(|error| utils::db_error(error, "统计订单口径实际金额"))?;
        actuals.by_receipt = model::receipt_actual_by_object(&scope_type, period.from, period.to)
            .await
            .map_err(|error| utils::db_error(error, "统计收款口径实际金额"))?;
    }

    Ok(assemble_achievement(&targets, &actuals, &basis))
}

// ==================== 小工具 ====================

/// 解析必填 UUID 参数。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| bad_request(format!("{field} 不是合法的 UUID（收到「{raw}」）")))
}

/// 解析可空 UUID 参数（空白串 = `None`）。
fn parse_optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => Uuid::parse_str(text)
            .map(Some)
            .map_err(|_| bad_request(format!("{field} 不是合法的 UUID（收到「{text}」）"))),
        None => Ok(None),
    }
}

/// 当前时间（回填 `created_at` / `updated_at` 的 DTO 占位值，库侧 `DEFAULT` / `now()` 才是真相）。
fn now_utc_time() -> nik::prelude::chrono::DateTime<nik::prelude::chrono::Utc> {
    nik::prelude::chrono::Utc::now()
}

// ==================== HTTP 入口 ====================

/// 目标分页。
#[nik::permission(
    path = "/business/erp/sales-target/page",
    method = "GET",
    permission = "business:erp:sales-target:list"
)]
#[nik::get("/business/erp/sales-target/page")]
pub async fn sales_target_page_api(Query(query): Query<TargetPageQuery>) -> ResResult<PageData<Vec<TargetRes>>> {
    Ok(Res::ok(page_targets(query).await?))
}

/// 达成率看板（两个口径一次给全）。
#[nik::permission(
    path = "/business/erp/sales-target/achievement",
    method = "GET",
    permission = "business:erp:sales-target:list"
)]
#[nik::get("/business/erp/sales-target/achievement")]
pub async fn sales_target_achievement_api(Query(query): Query<AchievementQuery>) -> ResResult<Vec<AchievementRow>> {
    Ok(Res::ok(achievement(query).await?))
}

/// 新建目标。
#[nik::permission(
    path = "/business/erp/sales-target",
    method = "POST",
    permission = "business:erp:sales-target:create"
)]
#[nik::post("/business/erp/sales-target")]
pub async fn sales_target_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<TargetCreateReq>,
) -> ResResult<TargetRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_target(operator, req).await?))
}

/// 编辑 / 停用目标。
#[nik::permission(
    path = "/business/erp/sales-target/{id}",
    method = "PUT",
    permission = "business:erp:sales-target:update"
)]
#[nik::put("/business/erp/sales-target/{id}")]
pub async fn sales_target_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TargetUpdateReq>,
) -> ResResult<TargetRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_target(operator, &id, req).await?))
}

/// 软删目标。
#[nik::permission(
    path = "/business/erp/sales-target/{id}",
    method = "DELETE",
    permission = "business:erp:sales-target:delete"
)]
#[nik::delete("/business/erp/sales-target/{id}")]
pub async fn sales_target_delete_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    delete_target(operator, &id).await?;
    Ok(Res::ok(()))
}

/// 预测分页。
#[nik::permission(
    path = "/business/erp/sales-forecast/page",
    method = "GET",
    permission = "business:erp:sales-forecast:list"
)]
#[nik::get("/business/erp/sales-forecast/page")]
pub async fn sales_forecast_page_api(Query(query): Query<ForecastPageQuery>) -> ResResult<PageData<Vec<ForecastRes>>> {
    Ok(Res::ok(page_forecasts(query).await?))
}

/// 新建预测。
#[nik::permission(
    path = "/business/erp/sales-forecast",
    method = "POST",
    permission = "business:erp:sales-forecast:create"
)]
#[nik::post("/business/erp/sales-forecast")]
pub async fn sales_forecast_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<ForecastCreateReq>,
) -> ResResult<ForecastRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_forecast(operator, req).await?))
}

/// 编辑预测。
#[nik::permission(
    path = "/business/erp/sales-forecast/{id}",
    method = "PUT",
    permission = "business:erp:sales-forecast:update"
)]
#[nik::put("/business/erp/sales-forecast/{id}")]
pub async fn sales_forecast_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<ForecastUpdateReq>,
) -> ResResult<ForecastRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_forecast(operator, &id, req).await?))
}

/// 软删预测。
#[nik::permission(
    path = "/business/erp/sales-forecast/{id}",
    method = "DELETE",
    permission = "business:erp:sales-forecast:delete"
)]
#[nik::delete("/business/erp/sales-forecast/{id}")]
pub async fn sales_forecast_delete_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    delete_forecast(operator, &id).await?;
    Ok(Res::ok(()))
}

/// 权限码自检出口（与 `handler::all_permissions` 同形，便于汇总脚本收集）。
pub fn all_permissions() -> Vec<&'static str> {
    SALES_TARGET_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::sales_target_dto::{
        BASIS_ORDER, CONFIDENCE_HIGH, CONFIDENCE_MEDIUM, SCOPE_DEPT, SCOPE_MATERIAL, SCOPE_SALESPERSON, TARGET_ACTIVE,
        TARGET_STATUSES,
    };
    use nik::prelude::chrono::NaiveDate;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期可解析")
    }

    fn target_row(
        id: Uuid,
        scope_type: &str,
        scope_id: Option<Uuid>,
        target_amount: Decimal,
        period_code: &str,
    ) -> TargetListRow {
        let scope_name = if scope_type == SCOPE_SALESPERSON {
            Some("张三".to_string())
        } else if scope_type == SCOPE_DEPT {
            Some("销售一部".to_string())
        } else if scope_type == SCOPE_MATERIAL {
            Some("螺栓 M8".to_string())
        } else {
            None
        };
        (
            TargetRow {
                id,
                target_no: format!("STG-20261107-{:06}", 1),
                scope_type: scope_type.to_string(),
                scope_id,
                period_code: period_code.to_string(),
                target_amount,
                target_qty: dec("0"),
                status: TARGET_ACTIVE.to_string(),
                remark: None,
                created_at: now_utc_time(),
                updated_at: now_utc_time(),
            },
            scope_name,
        )
    }

    // ---------- 权限 / 路由 ----------

    #[test]
    fn permission_codes_match_v0318() {
        assert_eq!(PERM_TARGET_LIST, "business:erp:sales-target:list");
        assert_eq!(PERM_TARGET_CREATE, "business:erp:sales-target:create");
        assert_eq!(PERM_TARGET_UPDATE, "business:erp:sales-target:update");
        assert_eq!(PERM_TARGET_DELETE, "business:erp:sales-target:delete");
        assert_eq!(PERM_FORECAST_LIST, "business:erp:sales-forecast:list");
        assert_eq!(PERM_FORECAST_CREATE, "business:erp:sales-forecast:create");
        assert_eq!(PERM_FORECAST_UPDATE, "business:erp:sales-forecast:update");
        assert_eq!(PERM_FORECAST_DELETE, "business:erp:sales-forecast:delete");
        assert_eq!(all_permissions().len(), 8);
        for code in SALES_TARGET_PERMISSIONS {
            let parts: Vec<&str> = code.split(':').collect();
            assert_eq!(parts.len(), 4, "{code}");
            assert_eq!(parts[0], "business", "{code}");
            assert_eq!(parts[1], "erp", "{code}");
            assert!(parts[2] == "sales-target" || parts[2] == "sales-forecast", "{code}");
        }
    }

    #[test]
    fn route_paths_have_no_api_prefix() {
        for path in [
            "/business/erp/sales-target/page",
            "/business/erp/sales-target/achievement",
            "/business/erp/sales-target",
            "/business/erp/sales-target/{id}",
            "/business/erp/sales-forecast/page",
            "/business/erp/sales-forecast",
            "/business/erp/sales-forecast/{id}",
        ] {
            assert!(path.starts_with("/business/erp/sales-"), "{path}");
            assert!(!path.starts_with("/api"), "{path} 不能带 /api 前缀");
        }
    }

    // ---------- 期间码 ----------

    #[test]
    fn period_month_is_left_closed_right_open() {
        let period = Period::from_code("2026-11").expect("月度");
        assert_eq!(period.from, date("2026-11-01"));
        assert_eq!(period.to, date("2026-12-01"));
        // 2 月不是 28/29 天的特例：to 一律是下月 1 号
        let feb = Period::from_code("2026-02").expect("二月");
        assert_eq!(feb.from, date("2026-02-01"));
        assert_eq!(feb.to, date("2026-03-01"));
        // 12 月跨年
        let dec = Period::from_code("2026-12").expect("十二月");
        assert_eq!(dec.to, date("2027-01-01"));
    }

    #[test]
    fn period_quarter_covers_three_months() {
        let q1 = Period::from_code("2026-Q1").expect("Q1");
        assert_eq!(q1.from, date("2026-01-01"));
        assert_eq!(q1.to, date("2026-04-01"));
        let q4 = Period::from_code("2026-q4").expect("小写 q 也收");
        assert_eq!(q4.code, "2026-Q4");
        assert_eq!(q4.from, date("2026-10-01"));
        assert_eq!(q4.to, date("2027-01-01"));
    }

    #[test]
    fn period_rejects_garbage() {
        for raw in [
            "",
            "2026",
            "2026-1",
            "2026-13",
            "2026-Q0",
            "2026-Q5",
            "26-11",
            "2026-11-01",
        ] {
            assert!(Period::from_code(raw).is_err(), "「{raw}」应被拒");
        }
    }

    // ---------- 达成率 / 缺口 ----------

    #[test]
    fn achievement_rate_is_percentage_two_decimals() {
        assert_eq!(achievement_rate(dec("85500"), dec("100000")), dec("85.50"));
        assert_eq!(achievement_rate(dec("120000"), dec("100000")), dec("120.00"));
        assert_eq!(achievement_rate(dec("0"), dec("100000")), dec("0.00"));
        // 除不尽要四舍五入到 2 位（不是截断）
        assert_eq!(achievement_rate(dec("1"), dec("3")), dec("33.33"));
        assert_eq!(achievement_rate(dec("2"), dec("3")), dec("66.67"));
    }

    #[test]
    fn achievement_rate_with_zero_target_is_zero_not_infinity() {
        // 目标填 0 = 没定目标；报 100% 会让看板撒谎，必须恒 0
        assert_eq!(achievement_rate(dec("50000"), dec("0")), Decimal::ZERO);
    }

    #[test]
    fn gap_positive_means_shortfall_negative_means_overachieved() {
        // 缺口 = 目标 − 实际
        assert_eq!(gap(dec("100000"), dec("85500")), dec("14500.00"));
        assert_eq!(gap(dec("100000"), dec("120000")), dec("-20000.00"));
        assert_eq!(gap(dec("100000"), dec("100000")), dec("0.00"));
    }

    // ---------- 看板组装 ----------

    #[test]
    fn assemble_picks_basis_and_keeps_both() {
        let alice = Uuid::now_v7();
        let targets = vec![target_row(
            Uuid::now_v7(),
            SCOPE_SALESPERSON,
            Some(alice),
            dec("100000"),
            "2026-11",
        )];
        let actuals = Actuals {
            by_order: vec![(alice, dec("85500"))],
            by_receipt: vec![(alice, dec("60000"))],
            order_total: dec("300000"),
            receipt_total: dec("210000"),
        };

        let order_view = assemble_achievement(&targets, &actuals, BASIS_ORDER);
        assert_eq!(order_view.len(), 1);
        let row = &order_view[0];
        assert_eq!(row.scope, "张三");
        assert_eq!(row.target_amount, dec("100000.00"));
        assert_eq!(row.actual_amount, dec("85500.00"));
        assert_eq!(row.achievement_rate, dec("85.50"));
        assert_eq!(row.gap, dec("14500.00"));
        // 另一个口径照样在（前端切口径不用重新请求）
        assert_eq!(row.by_receipt.actual_amount, dec("60000.00"));
        assert_eq!(row.by_receipt.achievement_rate, dec("60.00"));

        let receipt_view = assemble_achievement(&targets, &actuals, BASIS_RECEIPT);
        assert_eq!(receipt_view[0].actual_amount, dec("60000.00"));
        assert_eq!(receipt_view[0].achievement_rate, dec("60.00"));
        assert_eq!(receipt_view[0].gap, dec("40000.00"));
        assert_eq!(receipt_view[0].by_order.actual_amount, dec("85500.00"));
    }

    #[test]
    fn assemble_all_scope_uses_totals() {
        let targets = vec![target_row(Uuid::now_v7(), SCOPE_ALL, None, dec("500000"), "2026-Q4")];
        let actuals = Actuals {
            by_order: vec![(Uuid::now_v7(), dec("1"))],
            by_receipt: vec![],
            order_total: dec("450000"),
            receipt_total: dec("380000"),
        };
        let rows = assemble_achievement(&targets, &actuals, BASIS_ORDER);
        assert_eq!(rows[0].scope, "全公司");
        assert_eq!(rows[0].actual_amount, dec("450000.00"));
        assert_eq!(rows[0].achievement_rate, dec("90.00"));
        assert_eq!(rows[0].gap, dec("50000.00"));
        assert_eq!(rows[0].by_receipt.actual_amount, dec("380000.00"));
    }

    #[test]
    fn assemble_missing_actual_is_zero_not_error() {
        // 有目标没开工（一条订单都没有）→ 实际 0、达成率 0、缺口 = 目标
        let targets = vec![target_row(
            Uuid::now_v7(),
            SCOPE_DEPT,
            Some(Uuid::now_v7()),
            dec("200000"),
            "2026-11",
        )];
        let rows = assemble_achievement(&targets, &Actuals::default(), BASIS_ORDER);
        assert_eq!(rows[0].actual_amount, dec("0.00"));
        assert_eq!(rows[0].achievement_rate, dec("0.00"));
        assert_eq!(rows[0].gap, dec("200000.00"));
        assert_eq!(rows[0].by_order.achievement_rate, Decimal::ZERO);
    }

    #[test]
    fn assemble_material_scope_keeps_scope_name() {
        let material = Uuid::now_v7();
        let targets = vec![target_row(
            Uuid::now_v7(),
            SCOPE_MATERIAL,
            Some(material),
            dec("80000"),
            "2026-11",
        )];
        let actuals = Actuals {
            by_order: vec![(material, dec("96000"))],
            by_receipt: vec![(material, dec("80000"))],
            order_total: dec("0"),
            receipt_total: dec("0"),
        };
        let rows = assemble_achievement(&targets, &actuals, BASIS_ORDER);
        assert_eq!(rows[0].scope, "螺栓 M8");
        assert_eq!(rows[0].achievement_rate, dec("120.00"));
        assert_eq!(rows[0].gap, dec("-16000.00"));
    }

    // ---------- 入参归一化 ----------

    #[test]
    fn scope_normalization_matches_ddl_check() {
        let id = Uuid::now_v7().to_string();
        // ALL 不带 scope_id
        let (scope_type, scope_id) = normalize_scope("ALL", None).expect("ALL");
        assert_eq!(scope_type, SCOPE_ALL);
        assert!(scope_id.is_none());
        // ALL 带 scope_id 必须 400（否则 ck_biz_st_target_scope_id 会拒，但用户看不懂 500）
        assert!(normalize_scope("ALL", Some(&id)).is_err());
        // 非 ALL 必须带
        assert!(normalize_scope("SALESPERSON", None).is_err());
        let (scope_type, scope_id) = normalize_scope("salesperson", Some(&id)).expect("小写也收");
        assert_eq!(scope_type, SCOPE_SALESPERSON);
        assert_eq!(scope_id.map(|value| value.to_string()), Some(id));
        // 非法范围
        assert!(normalize_scope("REGION", Some(&Uuid::now_v7().to_string())).is_err());
        // 非法 UUID
        assert!(normalize_scope("DEPT", Some("not-a-uuid")).is_err());
    }

    #[test]
    fn status_and_confidence_normalization() {
        assert_eq!(normalize_status(None).expect("缺省"), TARGET_ACTIVE);
        assert_eq!(normalize_status(Some("inactive")).expect("小写也收"), "INACTIVE");
        assert!(normalize_status(Some("DELETED")).is_err());
        assert_eq!(TARGET_STATUSES.len(), 2);

        assert_eq!(normalize_confidence(None).expect("缺省"), CONFIDENCE_MEDIUM);
        assert_eq!(normalize_confidence(Some("high")).expect("小写也收"), CONFIDENCE_HIGH);
        assert!(normalize_confidence(Some("SURE")).is_err());
    }

    #[test]
    fn basis_normalization_defaults_to_order() {
        assert_eq!(normalize_basis(None).expect("缺省"), BASIS_ORDER);
        assert_eq!(normalize_basis(Some("receipt")).expect("小写"), BASIS_RECEIPT);
        assert!(normalize_basis(Some("CASH")).is_err());
    }

    #[test]
    fn amount_validation_rejects_negative_and_extra_decimals() {
        assert!(check_amount(dec("0"), "target_amount").is_ok());
        assert!(check_amount(dec("100.50"), "target_amount").is_ok());
        assert!(check_amount(dec("-1"), "target_amount").is_err());
        // 3 位小数会被库静默四舍五入 —— 入口就 400
        assert!(check_amount(dec("100.123"), "target_amount").is_err());
        assert!(check_qty(dec("0.0001"), "target_qty").is_ok());
        assert!(check_qty(dec("-0.5"), "target_qty").is_err());
        assert!(check_qty(dec("1.00001"), "target_qty").is_err());
    }

    #[test]
    fn sum_of_object_actuals_matches_total_rule() {
        // 口径一致性（纯算术演练）：各销售员实际之和 = 全公司总额，
        // 看板 `ALL` 行与按人汇总不该互相打架。
        let actuals = Actuals {
            by_order: vec![(Uuid::now_v7(), dec("120000")), (Uuid::now_v7(), dec("80000"))],
            by_receipt: vec![],
            order_total: dec("200000"),
            receipt_total: decimal_zero(),
        };
        let sum: Decimal = actuals.by_order.iter().map(|(_, value)| *value).sum();
        assert_eq!(sum, actuals.order_total);
    }

    fn decimal_zero() -> Decimal {
        Decimal::ZERO
    }

    #[test]
    fn version_no_must_be_positive() {
        // 库侧 ck_biz_sf_version 要求 >= 1；入口不拦就会以 500 出现（实测踩过）
        assert_eq!(check_version_no(1).expect("1 合法"), 1);
        assert_eq!(check_version_no(7).expect("7 合法"), 7);
        let error = check_version_no(0).expect_err("0 应被拒");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(check_version_no(-3).is_err());
        assert!(error.msg.contains("version_no"), "{}", error.msg);
    }
}
