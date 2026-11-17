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
//! | GET | `/business/erp/sales-forecast/variance` | `business:erp:sales-forecast:list` | **预测 vs 实际**（偏差分析，订单口径） |
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
use crate::business::erp::sales::sales_target_dto::{
    AchievementBasis, AchievementQuery, AchievementRow, BASIS_RECEIPT, DEVIATION_OVER, DEVIATION_UNDER,
    FORECAST_DIM_ALL, FORECAST_DIM_CUSTOMER, FORECAST_DIM_CUSTOMER_MATERIAL, FORECAST_DIM_MATERIAL,
    FORECAST_DIMENSIONS, ForecastCreateReq, ForecastDimensionCount, ForecastFilter, ForecastPageQuery, ForecastRes,
    ForecastUpdateReq, ForecastVarianceQuery, ForecastVarianceRes, ForecastVarianceRow, ForecastVarianceSummary,
    ForecastVarianceWorst, Period, SCOPE_ALL, TargetCreateReq, TargetFilter, TargetPageQuery, TargetRes,
    TargetUpdateReq, VARIANCE_BASIS, achievement_rate, check_amount, check_qty, deviation_level, dimension_label, gap,
    mean_absolute_pct, money2, normalize_basis, normalize_confidence, normalize_dimension, normalize_period,
    normalize_scope, normalize_status, normalize_tolerance_pct, variance_amount, variance_rate,
};
use crate::business::erp::sales::sales_target_model::{
    self as model, ForecastListRow, ForecastRow, TargetListRow, TargetRow,
};
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

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
        forecast_qty: forecast.forecast_qty,
        confidence: forecast.confidence.clone(),
        version_no: forecast.version_no,
        revised_from_id: forecast.revised_from_id.map(|id| id.to_string()),
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

/// 版本链校验（V0444 的 `revised_from_id` 是**多行**约束，库层 CHECK 表达不了，收在写入口）。
///
/// 给了 `revised_from_id` 时逐条校验：
/// 1. 不许指向自己（库侧 `ck_biz_sf_revised_from` 兜同一件事，这里给 400 而不是 500）；
/// 2. 指向的预测必须存在、且未被软删；
/// 3. 必须**同客户 / 同物料 / 同期间** —— 跨维度的"修订"是把别处的预测搬过来，不是滚动预测，
///    允许它会让「这一版取代哪一版」失去意义；
/// 4. `version_no` 必须等于上一版 + 1 —— 跳号会让"当前版本 = 最大版号"（视图
///    `v_biz_sales_forecast_current`）漏掉中间痕迹，也会和唯一索引（维度+期间+版本）
///    撞出用户看不懂的 409。
async fn validate_version_chain(
    current_id: Option<Uuid>,
    revised_from_id: Option<Uuid>,
    customer_id: Option<Uuid>,
    material_id: Option<Uuid>,
    period_code: &str,
    version_no: i32,
) -> std::result::Result<(), ApiError> {
    let Some(prev_id) = revised_from_id else {
        return Ok(());
    };
    if Some(prev_id) == current_id {
        return Err(bad_request("revised_from_id 不能指向自己（版本链会成环）"));
    }
    let prev = model::find_forecast(prev_id)
        .await
        .map_err(|error| utils::db_error(error, "校验销售预测的版本链"))?
        .ok_or_else(|| bad_request(format!("revised_from_id 指向的销售预测不存在或已删除（{prev_id}）")))?;
    let (prev, _, _) = prev;
    if prev.customer_id != customer_id || prev.material_id != material_id || prev.period_code != period_code {
        return Err(bad_request(format!(
            "revised_from_id 指向的预测不在同一维度：上一版是 客户 {:?} / 物料 {:?} / 期间 {}，\
             本次是 客户 {:?} / 物料 {:?} / 期间 {period_code}（客户、物料、期间三者必须一致）",
            prev.customer_id, prev.material_id, prev.period_code, customer_id, material_id
        )));
    }
    let expected = prev.version_no + 1;
    if version_no != expected {
        return Err(bad_request(format!(
            "新版本号必须等于上一版 + 1：上一版 version_no = {}，本次应为 {expected}（收到 {version_no}）",
            prev.version_no
        )));
    }
    Ok(())
}

/// 新建预测。
pub async fn create_forecast(
    operator: Option<Uuid>,
    req: ForecastCreateReq,
) -> std::result::Result<ForecastRes, ApiError> {
    let period = Period::from_code(&req.period_code)?;
    check_amount(req.forecast_amount, "forecast_amount")?;
    let forecast_qty = req.forecast_qty.unwrap_or_default();
    check_qty(forecast_qty, "forecast_qty")?;
    let confidence = normalize_confidence(req.confidence.as_deref())?;
    let customer_id = parse_optional_uuid(req.customer_id.as_deref(), "customer_id")?;
    let material_id = parse_optional_uuid(req.material_id.as_deref(), "material_id")?;
    let version_no = check_version_no(req.version_no.unwrap_or(1))?;
    let revised_from_id = parse_optional_uuid(req.revised_from_id.as_deref(), "revised_from_id")?;
    validate_version_chain(
        None,
        revised_from_id,
        customer_id,
        material_id,
        &period.code,
        version_no,
    )
    .await?;

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
        forecast_qty,
        confidence,
        version_no,
        revised_from_id,
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
    let forecast_qty = match req.forecast_qty {
        Some(value) => {
            check_qty(value, "forecast_qty")?;
            value
        }
        None => current.forecast_qty,
    };
    let confidence = match req.confidence.as_deref() {
        Some(raw) => normalize_confidence(Some(raw))?,
        None => current.confidence.clone(),
    };
    let version_no = check_version_no(req.version_no.unwrap_or(current.version_no))?;
    let revised_from_id = match req.revised_from_id {
        Some(value) => parse_optional_uuid(value.as_deref(), "revised_from_id")?,
        None => current.revised_from_id,
    };
    validate_version_chain(
        Some(id),
        revised_from_id,
        customer_id,
        material_id,
        &period.code,
        version_no,
    )
    .await?;
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
        forecast_qty,
        confidence,
        version_no,
        revised_from_id,
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
            if !crate::business::erp::sales::sales_target_dto::SCOPE_TYPES.contains(&normalized.as_str()) {
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

// ==================== 预测 vs 实际（偏差分析，M3-21） ====================
//
// 口径与字段语义见 `sales_target_dto` 的「预测 vs 实际（偏差分析）」一节（卡面替身）。
// 这里只做三件事：取一组实际数 → 组装行（含漏预测行）→ 汇总。判档规则在 dto 的
// [`deviation_level`]（纯函数，单测直接喂数）。

/// 实际数取数结果（一次查一组，组装阶段不再碰库）。
#[derive(Debug, Clone, Default)]
pub struct VarianceActuals {
    /// 按客户（`CUSTOMER` 粒度）。
    pub by_customer: Vec<model::CustomerActual>,
    /// 按物料（`MATERIAL` 粒度）。
    pub by_material: Vec<model::MaterialActual>,
    /// 按客户 × 物料（`CUSTOMER_MATERIAL` 粒度）。
    pub by_pair: Vec<model::PairActual>,
    /// 全公司实际金额（`ALL` 粒度）。
    pub total_amount: Decimal,
    /// 全公司实际数量（`ALL` 粒度）。
    pub total_qty: Decimal,
}

/// 显示名：客户名缺失（被物理清过 / 视图外）时回落 id，列表里不留空白格，也不显示 `null`。
fn display_customer(name: Option<&str>, id: Uuid) -> String {
    name.map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string())
}

/// 显示名：物料。
fn display_material(name: Option<&str>, id: Uuid) -> String {
    display_customer(name, id)
}

/// 显示名：客户 × 物料。
fn display_pair(
    customer_name: Option<&str>,
    customer_id: Uuid,
    material_name: Option<&str>,
    material_id: Uuid,
) -> String {
    format!(
        "{} × {}",
        display_customer(customer_name, customer_id),
        display_material(material_name, material_id)
    )
}

/// 一行的维度视图：客户 / 物料的 id + 显示名（全公司粒度两项都空，`display_name` = 「全公司」）。
///
/// 抽成结构体而不是继续堆参数：行构造函数的入参一旦超过 7 个，clippy 的
/// `too_many_arguments` 会直接判红，而且调用点会退化成「一排 None, None, None」。
#[derive(Debug, Clone, Default)]
struct DimensionView {
    /// 客户 id。
    customer_id: Option<Uuid>,
    /// 客户名（JOIN 回来的）。
    customer_name: Option<String>,
    /// 物料 id。
    material_id: Option<Uuid>,
    /// 物料名（JOIN 回来的）。
    material_name: Option<String>,
    /// 显示名（名字缺失时已回落 id，绝不留空白）。
    display_name: String,
}

/// 有预测的行（预测侧字段全给，实际数为同粒度同维度的订单数）。
fn forecast_variance_row(
    dimension: &str,
    period_code: &str,
    tolerance_pct: Decimal,
    forecast: &ForecastRow,
    view: DimensionView,
    actual_amount: Decimal,
    actual_qty: Decimal,
) -> ForecastVarianceRow {
    let actual_amount = money2(actual_amount);
    let variance_qty = (actual_qty - forecast.forecast_qty).round_dp(4);
    ForecastVarianceRow {
        dimension: dimension.to_string(),
        period_code: period_code.to_string(),
        display_name: view.display_name,
        forecast_id: Some(forecast.id.to_string()),
        forecast_no: Some(forecast.forecast_no.clone()),
        version_no: Some(forecast.version_no),
        confidence: Some(forecast.confidence.clone()),
        customer_id: forecast.customer_id.map(|id| id.to_string()),
        customer_name: view.customer_name,
        material_id: forecast.material_id.map(|id| id.to_string()),
        material_name: view.material_name,
        has_forecast: true,
        forecast_amount: forecast.forecast_amount,
        forecast_qty: forecast.forecast_qty,
        actual_amount,
        actual_qty,
        variance_amount: variance_amount(actual_amount, forecast.forecast_amount),
        variance_rate: variance_rate(actual_amount, forecast.forecast_amount),
        variance_qty,
        variance_qty_rate: variance_rate(actual_qty, forecast.forecast_qty),
        deviation_level: deviation_level(actual_amount, forecast.forecast_amount, tolerance_pct).to_string(),
    }
}

/// 漏预测行（有实际、没有当前版本预测）：预测侧全空 / 全 0，偏差 = 实际值。
fn unforecast_variance_row(
    dimension: &str,
    period_code: &str,
    view: DimensionView,
    actual_amount: Decimal,
    actual_qty: Decimal,
) -> ForecastVarianceRow {
    let actual_amount = money2(actual_amount);
    ForecastVarianceRow {
        dimension: dimension.to_string(),
        period_code: period_code.to_string(),
        display_name: view.display_name,
        forecast_id: None,
        forecast_no: None,
        version_no: None,
        confidence: None,
        customer_id: view.customer_id.map(|id| id.to_string()),
        customer_name: view.customer_name,
        material_id: view.material_id.map(|id| id.to_string()),
        material_name: view.material_name,
        has_forecast: false,
        forecast_amount: Decimal::ZERO,
        forecast_qty: Decimal::ZERO,
        actual_amount,
        actual_qty,
        variance_amount: actual_amount,
        variance_rate: None,
        variance_qty: actual_qty,
        variance_qty_rate: None,
        deviation_level: crate::business::erp::sales::sales_target_dto::DEVIATION_NO_FORECAST.to_string(),
    }
}

/// 组装明细行（纯函数：单测直接喂预测行 + 实际数，不碰库）。
///
/// 按 `dimension` 选一组实际数；**漏预测行只在本粒度内补**（跨粒度补就是另一种双计）。
/// 返回行按 `|偏差金额|` 倒序：偏差最大的在最上面，漏预测行会凭全差额自然浮起。
pub fn assemble_variance(
    forecasts: &[ForecastListRow],
    actuals: &VarianceActuals,
    dimension: &str,
    period_code: &str,
    tolerance_pct: Decimal,
) -> Vec<ForecastVarianceRow> {
    use std::collections::{HashMap, HashSet};
    let mut rows: Vec<ForecastVarianceRow> = Vec::with_capacity(forecasts.len() + 8);

    match dimension {
        FORECAST_DIM_CUSTOMER => {
            let by_id: HashMap<Uuid, &model::CustomerActual> = actuals
                .by_customer
                .iter()
                .map(|item| (item.customer_id, item))
                .collect();
            let mut seen: HashSet<Uuid> = HashSet::new();
            for (forecast, customer_name, _) in forecasts {
                let Some(customer_id) = forecast.customer_id else {
                    continue;
                };
                seen.insert(customer_id);
                let (amount, qty) = by_id
                    .get(&customer_id)
                    .map(|item| (item.amount, item.qty))
                    .unwrap_or((Decimal::ZERO, Decimal::ZERO));
                rows.push(forecast_variance_row(
                    dimension,
                    period_code,
                    tolerance_pct,
                    forecast,
                    DimensionView {
                        customer_name: customer_name.clone(),
                        display_name: display_customer(customer_name.as_deref(), customer_id),
                        ..Default::default()
                    },
                    amount,
                    qty,
                ));
            }
            for actual in &actuals.by_customer {
                if seen.contains(&actual.customer_id) {
                    continue;
                }
                rows.push(unforecast_variance_row(
                    dimension,
                    period_code,
                    DimensionView {
                        customer_id: Some(actual.customer_id),
                        customer_name: actual.customer_name.clone(),
                        display_name: display_customer(actual.customer_name.as_deref(), actual.customer_id),
                        ..Default::default()
                    },
                    actual.amount,
                    actual.qty,
                ));
            }
        }
        FORECAST_DIM_MATERIAL => {
            let by_id: HashMap<Uuid, &model::MaterialActual> = actuals
                .by_material
                .iter()
                .map(|item| (item.material_id, item))
                .collect();
            let mut seen: HashSet<Uuid> = HashSet::new();
            for (forecast, _, material_name) in forecasts {
                let Some(material_id) = forecast.material_id else {
                    continue;
                };
                seen.insert(material_id);
                let (amount, qty) = by_id
                    .get(&material_id)
                    .map(|item| (item.amount, item.qty))
                    .unwrap_or((Decimal::ZERO, Decimal::ZERO));
                rows.push(forecast_variance_row(
                    dimension,
                    period_code,
                    tolerance_pct,
                    forecast,
                    DimensionView {
                        material_name: material_name.clone(),
                        display_name: display_material(material_name.as_deref(), material_id),
                        ..Default::default()
                    },
                    amount,
                    qty,
                ));
            }
            for actual in &actuals.by_material {
                if seen.contains(&actual.material_id) {
                    continue;
                }
                rows.push(unforecast_variance_row(
                    dimension,
                    period_code,
                    DimensionView {
                        material_id: Some(actual.material_id),
                        material_name: actual.material_name.clone(),
                        display_name: display_material(actual.material_name.as_deref(), actual.material_id),
                        ..Default::default()
                    },
                    actual.amount,
                    actual.qty,
                ));
            }
        }
        FORECAST_DIM_CUSTOMER_MATERIAL => {
            let by_key: HashMap<(Uuid, Uuid), &model::PairActual> = actuals
                .by_pair
                .iter()
                .map(|item| ((item.customer_id, item.material_id), item))
                .collect();
            let mut seen: HashSet<(Uuid, Uuid)> = HashSet::new();
            for (forecast, customer_name, material_name) in forecasts {
                let (Some(customer_id), Some(material_id)) = (forecast.customer_id, forecast.material_id) else {
                    continue;
                };
                seen.insert((customer_id, material_id));
                let (amount, qty) = by_key
                    .get(&(customer_id, material_id))
                    .map(|item| (item.amount, item.qty))
                    .unwrap_or((Decimal::ZERO, Decimal::ZERO));
                rows.push(forecast_variance_row(
                    dimension,
                    period_code,
                    tolerance_pct,
                    forecast,
                    DimensionView {
                        customer_name: customer_name.clone(),
                        material_name: material_name.clone(),
                        display_name: display_pair(
                            customer_name.as_deref(),
                            customer_id,
                            material_name.as_deref(),
                            material_id,
                        ),
                        ..Default::default()
                    },
                    amount,
                    qty,
                ));
            }
            for actual in &actuals.by_pair {
                if seen.contains(&(actual.customer_id, actual.material_id)) {
                    continue;
                }
                rows.push(unforecast_variance_row(
                    dimension,
                    period_code,
                    DimensionView {
                        customer_id: Some(actual.customer_id),
                        customer_name: actual.customer_name.clone(),
                        material_id: Some(actual.material_id),
                        material_name: actual.material_name.clone(),
                        display_name: display_pair(
                            actual.customer_name.as_deref(),
                            actual.customer_id,
                            actual.material_name.as_deref(),
                            actual.material_id,
                        ),
                    },
                    actual.amount,
                    actual.qty,
                ));
            }
        }
        _ => {
            // ALL：全公司只有一条口径，没有"漏预测"之外的分行
            if let Some((forecast, _, _)) = forecasts.first() {
                rows.push(forecast_variance_row(
                    FORECAST_DIM_ALL,
                    period_code,
                    tolerance_pct,
                    forecast,
                    DimensionView {
                        display_name: "全公司".to_string(),
                        ..Default::default()
                    },
                    actuals.total_amount,
                    actuals.total_qty,
                ));
            } else if !actuals.total_amount.is_zero() || !actuals.total_qty.is_zero() {
                rows.push(unforecast_variance_row(
                    FORECAST_DIM_ALL,
                    period_code,
                    DimensionView {
                        display_name: "全公司".to_string(),
                        ..Default::default()
                    },
                    actuals.total_amount,
                    actuals.total_qty,
                ));
            }
        }
    }

    rows.sort_by(|left, right| {
        right
            .variance_amount
            .abs()
            .cmp(&left.variance_amount.abs())
            .then_with(|| right.actual_amount.cmp(&left.actual_amount))
            .then_with(|| left.display_name.cmp(&right.display_name))
    });
    rows
}

/// 汇总（纯函数，喂的是 [`assemble_variance`] 的同一批行 —— 汇总与明细不可能对不上）。
///
/// `over/under/on_target` 只数**有预测**的行（三者之和 = `forecast_count`）；
/// `row_count = forecast_count + unforecast_count`。
pub fn summarize_variance(rows: &[ForecastVarianceRow]) -> ForecastVarianceSummary {
    let mut summary = ForecastVarianceSummary {
        row_count: 0,
        forecast_count: 0,
        unforecast_count: 0,
        over_count: 0,
        under_count: 0,
        on_target_count: 0,
        forecast_amount: Decimal::ZERO,
        actual_amount: Decimal::ZERO,
        variance_amount: Decimal::ZERO,
        variance_rate: None,
        forecast_qty: Decimal::ZERO,
        actual_qty: Decimal::ZERO,
        variance_qty: Decimal::ZERO,
        variance_qty_rate: None,
        mape_pct: None,
        worst: None,
    };
    let mut rates: Vec<Decimal> = Vec::with_capacity(rows.len());
    for row in rows {
        summary.row_count += 1;
        summary.forecast_amount += row.forecast_amount;
        summary.actual_amount += row.actual_amount;
        summary.forecast_qty += row.forecast_qty;
        summary.actual_qty += row.actual_qty;
        if !row.has_forecast {
            summary.unforecast_count += 1;
            continue;
        }
        summary.forecast_count += 1;
        match row.deviation_level.as_str() {
            DEVIATION_OVER => summary.over_count += 1,
            DEVIATION_UNDER => summary.under_count += 1,
            _ => summary.on_target_count += 1,
        }
        if let Some(rate) = row.variance_rate {
            rates.push(rate);
        }
    }
    summary.variance_amount = variance_amount(summary.actual_amount, summary.forecast_amount);
    summary.variance_rate = variance_rate(summary.actual_amount, summary.forecast_amount);
    summary.variance_qty = (summary.actual_qty - summary.forecast_qty).round_dp(4);
    summary.variance_qty_rate = variance_rate(summary.actual_qty, summary.forecast_qty);
    summary.mape_pct = mean_absolute_pct(&rates);
    summary.worst = rows
        .iter()
        .max_by(|left, right| {
            left.variance_amount
                .abs()
                .cmp(&right.variance_amount.abs())
                .then_with(|| left.has_forecast.cmp(&right.has_forecast))
                .then_with(|| right.display_name.cmp(&left.display_name))
        })
        .map(|row| ForecastVarianceWorst {
            display_name: row.display_name.clone(),
            forecast_no: row.forecast_no.clone(),
            variance_amount: row.variance_amount,
            variance_rate: row.variance_rate,
            deviation_level: row.deviation_level.clone(),
        });
    summary
}

/// 预测 vs 实际（偏差分析）。`period_code` 必填；`dimension` 缺省按客户。
pub async fn forecast_variance(query: ForecastVarianceQuery) -> std::result::Result<ForecastVarianceRes, ApiError> {
    let raw_period = query
        .period_code
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("period_code 必填（yyyy-MM 或 yyyy-QN）".to_string()))?;
    let period = Period::from_code(&normalize_period(raw_period)?)?;
    let dimension = normalize_dimension(query.dimension.as_deref())?;
    let tolerance_pct = normalize_tolerance_pct(query.tolerance_pct.as_deref())?;

    let forecasts = model::current_forecasts_for_period(&period.code, &dimension)
        .await
        .map_err(|error| utils::db_error(error, "查询当前版本的销售预测"))?;
    let counts = model::forecast_dimension_counts(&period.code)
        .await
        .map_err(|error| utils::db_error(error, "统计各粒度的销售预测条数"))?;

    let mut actuals = VarianceActuals::default();
    match dimension.as_str() {
        FORECAST_DIM_CUSTOMER => {
            actuals.by_customer = model::order_actual_by_customer(period.from, period.to)
                .await
                .map_err(|error| utils::db_error(error, "统计按客户的订单实际数"))?;
        }
        FORECAST_DIM_MATERIAL => {
            actuals.by_material = model::order_actual_by_material(period.from, period.to)
                .await
                .map_err(|error| utils::db_error(error, "统计按物料的订单实际数"))?;
        }
        FORECAST_DIM_CUSTOMER_MATERIAL => {
            actuals.by_pair = model::order_actual_by_pair(period.from, period.to)
                .await
                .map_err(|error| utils::db_error(error, "统计按客户 × 物料的订单实际数"))?;
        }
        _ => {
            let (amount, qty) = model::order_actual_total_with_qty(period.from, period.to)
                .await
                .map_err(|error| utils::db_error(error, "统计全公司订单实际数"))?;
            actuals.total_amount = amount;
            actuals.total_qty = qty;
        }
    }

    let rows = assemble_variance(&forecasts, &actuals, &dimension, &period.code, tolerance_pct);
    let summary = summarize_variance(&rows);
    // 各粒度条数：四种粒度都回（0 也回），前端下拉不必猜哪个粒度有数据
    let dimension_counts = FORECAST_DIMENSIONS
        .iter()
        .map(|name| ForecastDimensionCount {
            dimension: (*name).to_string(),
            dimension_label: dimension_label(name).to_string(),
            row_count: u32::try_from(
                counts
                    .iter()
                    .find(|(found, _)| found == name)
                    .map(|(_, n)| *n)
                    .unwrap_or(0),
            )
            .unwrap_or(0),
        })
        .collect::<Vec<_>>();

    Ok(ForecastVarianceRes {
        period_code: period.code,
        dimension: dimension.clone(),
        dimension_label: dimension_label(&dimension).to_string(),
        basis: VARIANCE_BASIS.to_string(),
        tolerance_pct,
        summary,
        dimension_counts,
        rows,
    })
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

/// 预测 vs 实际（偏差分析，订单口径；`dimension` 缺省按客户）。
///
/// 路径 `/variance` 是静态段，与 `PUT/DELETE /sales-forecast/{id}` 不冲突
/// （方法不同，且 `variance` 不会被当成 UUID —— 这里也不接受任何 `{id}`）。
#[nik::permission(
    path = "/business/erp/sales-forecast/variance",
    method = "GET",
    permission = "business:erp:sales-forecast:list"
)]
#[nik::get("/business/erp/sales-forecast/variance")]
pub async fn sales_forecast_variance_api(Query(query): Query<ForecastVarianceQuery>) -> ResResult<ForecastVarianceRes> {
    Ok(Res::ok(forecast_variance(query).await?))
}

/// 权限码自检出口（与 `handler::all_permissions` 同形，便于汇总脚本收集）。
pub fn all_permissions() -> Vec<&'static str> {
    SALES_TARGET_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::sales::sales_target_dto::{
        BASIS_ORDER, CONFIDENCE_HIGH, CONFIDENCE_MEDIUM, DEVIATION_NO_FORECAST, DEVIATION_ON_TARGET, SCOPE_DEPT,
        SCOPE_MATERIAL, SCOPE_SALESPERSON, TARGET_ACTIVE, TARGET_STATUSES,
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
            "/business/erp/sales-forecast/variance",
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

    // ---------- 预测 vs 实际（偏差分析，M3-21） ----------

    /// 预测行夹具（客户 / 物料都给名字，显示名走 JOIN 回来的那份）。
    fn forecast_pair(
        customer: Option<(Uuid, &str)>,
        material: Option<(Uuid, &str)>,
        amount: &str,
        qty: &str,
    ) -> ForecastListRow {
        (
            ForecastRow {
                id: Uuid::now_v7(),
                forecast_no: "SFC-20310501-000001".to_string(),
                customer_id: customer.map(|(id, _)| id),
                material_id: material.map(|(id, _)| id),
                period_code: "2031-05".to_string(),
                forecast_amount: dec(amount),
                forecast_qty: dec(qty),
                confidence: "MEDIUM".to_string(),
                version_no: 1,
                revised_from_id: None,
                remark: None,
                created_at: now_utc_time(),
                updated_at: now_utc_time(),
            },
            customer.map(|(_, name)| name.to_string()),
            material.map(|(_, name)| name.to_string()),
        )
    }

    #[test]
    fn variance_math_zero_forecast_has_no_rate() {
        // 预测 0 而实际 50000：比率没有意义（None），差额就是全差额，档位 OVER
        assert_eq!(variance_amount(dec("50000"), dec("0")), dec("50000.00"));
        assert_eq!(variance_rate(dec("50000"), dec("0")), None);
        assert_eq!(deviation_level(dec("50000"), dec("0"), dec("5")), DEVIATION_OVER);
        // 预测与实际都为 0 = 命中（不是 OVER）
        assert_eq!(deviation_level(dec("0"), dec("0"), dec("5")), DEVIATION_ON_TARGET);
        // 常规：实际 85000 / 预测 100000 ⇒ 差 -15000、率 -15.00%
        assert_eq!(variance_amount(dec("85000"), dec("100000")), dec("-15000.00"));
        assert_eq!(variance_rate(dec("85000"), dec("100000")), Some(dec("-15.00")));
        assert_eq!(deviation_level(dec("85000"), dec("100000"), dec("5")), DEVIATION_UNDER);
        // 除不尽四舍五入（不是截断）
        assert_eq!(variance_rate(dec("2"), dec("3")), Some(dec("-33.33")));
    }

    #[test]
    fn deviation_level_compares_unrounded_rate() {
        // 5.004% 先四舍五入成 5.00 再比会被误判成「命中」—— 必须按未取整的比率判
        assert_eq!(
            deviation_level(dec("105004"), dec("100000"), dec("5")),
            DEVIATION_OVER,
            "5.004% 超出 5% 容差"
        );
        // 正好 5% 算命中（含边界）
        assert_eq!(
            deviation_level(dec("105000"), dec("100000"), dec("5")),
            DEVIATION_ON_TARGET
        );
        assert_eq!(
            deviation_level(dec("95000"), dec("100000"), dec("5")),
            DEVIATION_ON_TARGET
        );
        assert_eq!(deviation_level(dec("94999"), dec("100000"), dec("5")), DEVIATION_UNDER);
        // 容差 0 = 任何非零差额都算偏差
        assert_eq!(deviation_level(dec("100001"), dec("100000"), dec("0")), DEVIATION_OVER);
    }

    #[test]
    fn assemble_customer_grain_joins_actuals_and_adds_unforecast_rows() {
        let (customer_a, customer_b, customer_c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let forecasts = vec![
            forecast_pair(Some((customer_a, "客户甲")), None, "100000", "1000"),
            forecast_pair(Some((customer_b, "客户乙")), None, "50000", "500"),
        ];
        let actuals = VarianceActuals {
            by_customer: vec![
                model::CustomerActual {
                    customer_id: customer_a,
                    customer_name: Some("客户甲".to_string()),
                    amount: dec("85000"),
                    qty: dec("900"),
                },
                // 客户丙完全没预测，但期间内卖了 30000 —— 必须补成漏预测行
                model::CustomerActual {
                    customer_id: customer_c,
                    customer_name: Some("客户丙".to_string()),
                    amount: dec("30000"),
                    qty: dec("300"),
                },
            ],
            ..Default::default()
        };

        let rows = assemble_variance(&forecasts, &actuals, FORECAST_DIM_CUSTOMER, "2031-05", dec("5"));
        assert_eq!(rows.len(), 3, "两行预测 + 一行漏预测");

        // 按 |偏差金额| 倒序：客户乙 -50000 > 客户丙 +30000（漏预测）> 客户甲 -15000
        assert_eq!(rows[0].display_name, "客户乙");
        assert!(rows[0].has_forecast);
        assert_eq!(rows[0].actual_amount, dec("0.00"));
        assert_eq!(rows[0].variance_amount, dec("-50000.00"));
        assert_eq!(rows[0].variance_rate, Some(dec("-100.00")));
        assert_eq!(rows[0].deviation_level, DEVIATION_UNDER);
        assert_eq!(rows[0].forecast_no.as_deref(), Some("SFC-20310501-000001"));

        assert_eq!(rows[1].display_name, "客户丙");
        assert!(!rows[1].has_forecast, "没预测的行 has_forecast=false");
        assert!(rows[1].forecast_id.is_none() && rows[1].forecast_no.is_none());
        assert!(rows[1].version_no.is_none() && rows[1].confidence.is_none());
        assert_eq!(rows[1].forecast_amount, Decimal::ZERO);
        assert_eq!(rows[1].variance_amount, dec("30000.00"));
        assert_eq!(rows[1].variance_rate, None);
        assert_eq!(rows[1].variance_qty, dec("300"));
        assert_eq!(rows[1].deviation_level, "NO_FORECAST");

        assert_eq!(rows[2].display_name, "客户甲");
        assert_eq!(rows[2].variance_amount, dec("-15000.00"));
        assert_eq!(rows[2].variance_rate, Some(dec("-15.00")));
        assert_eq!(rows[2].variance_qty, dec("-100"));

        // 汇总与明细同源：合计偏差 = 各行之和
        let summary = summarize_variance(&rows);
        assert_eq!(summary.row_count, 3);
        assert_eq!(summary.forecast_count, 2);
        assert_eq!(summary.unforecast_count, 1);
        assert_eq!(summary.forecast_amount, dec("150000"));
        assert_eq!(summary.actual_amount, dec("115000"));
        assert_eq!(summary.variance_amount, dec("-35000.00"));
        assert_eq!(summary.variance_rate, Some(dec("-23.33")));
        assert_eq!(
            (summary.over_count, summary.under_count, summary.on_target_count),
            (0, 2, 0)
        );
        // MAPE 只算有预测金额的行：(100 + 15) / 2 = 57.50%
        assert_eq!(summary.mape_pct, Some(dec("57.50")));
        let worst = summary.worst.expect("有行就有最贵偏差");
        assert_eq!(worst.display_name, "客户乙");
        assert_eq!(worst.variance_amount, dec("-50000.00"));
    }

    #[test]
    fn assemble_material_grain_never_borrows_customer_actuals() {
        let material_id = Uuid::now_v7();
        let forecasts = vec![forecast_pair(None, Some((material_id, "螺栓 M8")), "80000", "1000")];

        // 客户口径的实际数不能串到物料口径：这里只给 by_customer，物料行必须读到 0
        let customer_only = VarianceActuals {
            by_customer: vec![model::CustomerActual {
                customer_id: Uuid::now_v7(),
                customer_name: Some("客户甲".to_string()),
                amount: dec("99999"),
                qty: dec("9"),
            }],
            ..Default::default()
        };
        let rows = assemble_variance(&forecasts, &customer_only, FORECAST_DIM_MATERIAL, "2031-05", dec("5"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].actual_amount, dec("0.00"), "物料粒度只认物料实际数");
        assert_eq!(rows[0].variance_rate, Some(dec("-100.00")));

        // 真正的物料实际数到了才对得上
        let with_material = VarianceActuals {
            by_material: vec![model::MaterialActual {
                material_id,
                material_name: Some("螺栓 M8".to_string()),
                amount: dec("96000"),
                qty: dec("1200"),
            }],
            ..Default::default()
        };
        let rows = assemble_variance(&forecasts, &with_material, FORECAST_DIM_MATERIAL, "2031-05", dec("5"));
        assert_eq!(rows[0].display_name, "螺栓 M8");
        assert_eq!(rows[0].actual_amount, dec("96000.00"));
        assert_eq!(rows[0].variance_rate, Some(dec("20.00")));
        assert_eq!(rows[0].variance_qty, dec("200"));
        assert_eq!(rows[0].variance_qty_rate, Some(dec("20.00")));
        assert_eq!(rows[0].deviation_level, DEVIATION_OVER);
    }

    #[test]
    fn assemble_pair_grain_keeps_customer_and_material_names() {
        let (customer_id, material_id) = (Uuid::now_v7(), Uuid::now_v7());
        let forecasts = vec![forecast_pair(
            Some((customer_id, "客户甲")),
            Some((material_id, "螺栓 M8")),
            "10000",
            "100",
        )];
        let actuals = VarianceActuals {
            by_pair: vec![model::PairActual {
                customer_id,
                customer_name: Some("客户甲".to_string()),
                material_id,
                material_name: Some("螺栓 M8".to_string()),
                amount: dec("10000"),
                qty: dec("100"),
            }],
            ..Default::default()
        };
        let rows = assemble_variance(
            &forecasts,
            &actuals,
            FORECAST_DIM_CUSTOMER_MATERIAL,
            "2031-05",
            dec("5"),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].display_name, "客户甲 × 螺栓 M8");
        assert_eq!(rows[0].variance_amount, Decimal::ZERO);
        assert_eq!(rows[0].deviation_level, DEVIATION_ON_TARGET);
        let summary = summarize_variance(&rows);
        assert_eq!(summary.variance_rate, Some(Decimal::ZERO));
        assert_eq!(summary.mape_pct, Some(Decimal::ZERO));
    }

    #[test]
    fn assemble_all_grain_uses_company_total_and_reports_missing_forecast() {
        let period = Period::from_code("2031-05").expect("期间");
        let global = vec![forecast_pair(None, None, "1000000", "10000")];
        let with_forecast = VarianceActuals {
            total_amount: dec("1200000"),
            total_qty: dec("11000"),
            ..Default::default()
        };
        let rows = assemble_variance(&global, &with_forecast, FORECAST_DIM_ALL, &period.code, dec("5"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].display_name, "全公司");
        assert_eq!(rows[0].actual_amount, dec("1200000.00"));
        assert_eq!(rows[0].variance_rate, Some(dec("20.00")));
        assert_eq!(rows[0].deviation_level, DEVIATION_OVER);

        // 没有全局预测但有实际：给一行「全公司 · 漏预测」，不是返回空列表说没偏差
        let rows = assemble_variance(&[], &with_forecast, FORECAST_DIM_ALL, &period.code, dec("5"));
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].has_forecast);
        assert_eq!(rows[0].deviation_level, "NO_FORECAST");
        assert_eq!(rows[0].variance_amount, dec("1200000.00"));

        // 既没预测也没实际：空列表（不是一行 0）
        let rows = assemble_variance(
            &[],
            &VarianceActuals::default(),
            FORECAST_DIM_ALL,
            &period.code,
            dec("5"),
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn summarize_zero_rows_is_all_zero_without_rates() {
        let summary = summarize_variance(&[]);
        assert_eq!(summary.row_count, 0);
        assert_eq!(summary.forecast_amount, Decimal::ZERO);
        assert_eq!(summary.variance_rate, None, "预测合计 0 ⇒ 比率无意义");
        assert_eq!(summary.variance_qty_rate, None);
        assert_eq!(summary.mape_pct, None);
        assert!(summary.worst.is_none());
    }

    #[test]
    fn dimension_and_tolerance_normalization() {
        assert_eq!(normalize_dimension(None).expect("缺省"), FORECAST_DIM_CUSTOMER);
        assert_eq!(normalize_dimension(Some("  ")).expect("空白"), FORECAST_DIM_CUSTOMER);
        assert_eq!(
            normalize_dimension(Some(" material ")).expect("小写"),
            FORECAST_DIM_MATERIAL
        );
        assert_eq!(
            normalize_dimension(Some("customer_material")).expect("下划线粒度"),
            FORECAST_DIM_CUSTOMER_MATERIAL
        );
        assert!(normalize_dimension(Some("REGION")).is_err());
        assert_eq!(FORECAST_DIMENSIONS.len(), 4);

        assert_eq!(normalize_tolerance_pct(None).expect("缺省"), dec("5"));
        assert_eq!(normalize_tolerance_pct(Some(" 0 ")).expect("0 合法"), Decimal::ZERO);
        assert_eq!(normalize_tolerance_pct(Some("12.5")).expect("小数"), dec("12.5"));
        assert!(normalize_tolerance_pct(Some("-1")).is_err(), "负容差没有含义");
        assert!(normalize_tolerance_pct(Some("101")).is_err(), "超过 100% 没有含义");
        assert!(normalize_tolerance_pct(Some("5.123")).is_err(), "最多 2 位小数");
        assert!(normalize_tolerance_pct(Some("偏差")).is_err());
    }

    // ---------- 汇总口径（M3-22） ----------
    //
    // 这一节只钉 `summarize_variance` / `assemble_variance` 的**汇总语义**，
    // 每条都对应 `sales_target_dto` 里「出参金额 / 比例」与行构造的一段口径：
    //
    // | 用例 | 钉住的口径 |
    // |---|---|
    // | `summary_multi_rows_add_up_like_the_rows` | 汇总 = 同一批行相加（不是另跑一遍全库 SQL）；over/under/on_target 只数有预测的行 |
    // | `summary_rate_comes_from_totals_not_mean_of_rates` | **合计偏差率**用 Σ实际 / Σ预测 算；行偏差率的算术平均是 MAPE，两个数不同 |
    // | `summary_keeps_unforecast_actuals_in_totals` | 漏预测行的实际额**进合计**（分子分母都进），偏差率才有意义 |
    // | `summary_counts_zero_amount_forecast_row_but_excludes_it_from_mape` | 只预测数量（金额 0）的行算 OVER、进 counts，但比率无意义 ⇒ 不进 MAPE |
    // | `summary_quantity_rate_is_null_when_quantity_never_forecast` | 数量侧同一规则：预测数量合计 0 ⇒ 数量偏差率 null |
    // | `pair_grain_unforecast_rows_keep_both_dimensions` | 客户 × 物料的漏预测行两维 id/名字都带（与物料粒度的空客户列不同） |
    // | `tolerance_boundaries_pick_the_right_level` | 容差边界：正好 ±容差算命中；容差 0 / 100 的两端 |
    // | `rows_sort_by_absolute_variance_then_actual_then_name` | 行序：\|偏差\| 倒序 → 实际额倒序 → 显示名 —— 同额偏差不会每次刷新换位置 |
    // | `summary_worst_prefers_the_row_with_a_forecast` | 同额偏差时 `worst` 取**有预测**的那行（能说清是哪条预测偏了） |
    // | `all_grain_quantity_only_actual_is_still_a_missing_forecast` | 全公司没预测但只有数量（金额 0）也要给漏预测行（判据是「金额或数量非 0」） |

    #[test]
    fn summary_multi_rows_add_up_like_the_rows() {
        let (customer_a, customer_b, customer_c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let forecasts = vec![
            forecast_pair(Some((customer_a, "客户甲")), None, "100000", "1000"),
            forecast_pair(Some((customer_b, "客户乙")), None, "50000", "500"),
            forecast_pair(Some((customer_c, "客户丙")), None, "40000", "400"),
        ];
        let actuals = VarianceActuals {
            by_customer: vec![
                model::CustomerActual {
                    customer_id: customer_a,
                    customer_name: Some("客户甲".to_string()),
                    amount: dec("90000"),
                    qty: dec("900"),
                },
                model::CustomerActual {
                    customer_id: customer_b,
                    customer_name: Some("客户乙".to_string()),
                    amount: dec("65000"),
                    qty: dec("650"),
                },
                model::CustomerActual {
                    customer_id: customer_c,
                    customer_name: Some("客户丙".to_string()),
                    amount: dec("42000"),
                    qty: dec("420"),
                },
            ],
            ..Default::default()
        };
        let rows = assemble_variance(&forecasts, &actuals, FORECAST_DIM_CUSTOMER, "2031-05", dec("5"));
        let summary = summarize_variance(&rows);

        // 逐行：-10% UNDER / +30% OVER / 正好 +5.00% ON_TARGET（边界命中）
        assert_eq!(
            (summary.over_count, summary.under_count, summary.on_target_count),
            (1, 1, 1)
        );
        assert_eq!(summary.row_count, 3);
        assert_eq!(summary.forecast_count, 3, "三个档位相加必须等于有预测的行数");
        assert_eq!(summary.unforecast_count, 0);

        // 合计 = 三行相加（金额 2 位、数量 4 位都不丢精度）
        assert_eq!(summary.forecast_amount, dec("190000"));
        assert_eq!(summary.actual_amount, dec("197000"));
        assert_eq!(summary.variance_amount, dec("7000.00"));
        // 合计偏差率 = 7000 / 190000 = 3.68%（不是 (10+30+5)/3 = 15%）
        assert_eq!(summary.variance_rate, Some(dec("3.68")));
        assert_eq!(summary.forecast_qty, dec("1900"));
        assert_eq!(summary.actual_qty, dec("1970"));
        assert_eq!(summary.variance_qty, dec("70"));
        assert_eq!(summary.variance_qty_rate, Some(dec("3.68")));
        assert_eq!(summary.mape_pct, Some(dec("15.00")), "(10 + 30 + 5) / 3");

        // 汇总与明细同源：把行自己再加一遍，必须与 summary 完全一致（含数量）
        let row_forecast: Decimal = rows.iter().map(|row| row.forecast_amount).sum();
        let row_actual: Decimal = rows.iter().map(|row| row.actual_amount).sum();
        let row_forecast_qty: Decimal = rows.iter().map(|row| row.forecast_qty).sum();
        let row_actual_qty: Decimal = rows.iter().map(|row| row.actual_qty).sum();
        assert_eq!(row_forecast, summary.forecast_amount);
        assert_eq!(row_actual, summary.actual_amount);
        assert_eq!(row_forecast_qty, summary.forecast_qty);
        assert_eq!(row_actual_qty, summary.actual_qty);

        let worst = summary.worst.expect("有行就有最贵偏差");
        assert_eq!(worst.display_name, "客户乙", "|+15000| 最大");
        assert_eq!(worst.variance_amount, dec("15000.00"));
        assert_eq!(worst.deviation_level, DEVIATION_OVER);
    }

    #[test]
    fn summary_rate_comes_from_totals_not_mean_of_rates() {
        let (big, small) = (Uuid::now_v7(), Uuid::now_v7());
        let forecasts = vec![
            forecast_pair(Some((big, "大客户")), None, "1000000", "10000"),
            forecast_pair(Some((small, "小客户")), None, "1000", "10"),
        ];
        let actuals = VarianceActuals {
            by_customer: vec![
                model::CustomerActual {
                    customer_id: big,
                    customer_name: Some("大客户".to_string()),
                    amount: dec("1010000"),
                    qty: dec("10100"),
                },
                model::CustomerActual {
                    customer_id: small,
                    customer_name: Some("小客户".to_string()),
                    amount: dec("2000"),
                    qty: dec("20"),
                },
            ],
            ..Default::default()
        };
        let rows = assemble_variance(&forecasts, &actuals, FORECAST_DIM_CUSTOMER, "2031-05", dec("5"));
        let summary = summarize_variance(&rows);

        // 行偏差率：大单 +1.00%、小单 +100.00%
        assert_eq!(rows.len(), 2);
        let rate_of = |name: &str| {
            rows.iter()
                .find(|row| row.display_name == name)
                .and_then(|row| row.variance_rate)
                .expect("两行都有预测金额")
        };
        assert_eq!(rate_of("大客户"), dec("1.00"));
        assert_eq!(rate_of("小客户"), dec("100.00"));

        // 合计偏差率按钱算：(1012000 - 1001000) / 1001000 = 1.10%（小单被大单摊薄）
        assert_eq!(summary.forecast_amount, dec("1001000"));
        assert_eq!(summary.actual_amount, dec("1012000"));
        assert_eq!(summary.variance_amount, dec("11000.00"));
        assert_eq!(summary.variance_rate, Some(dec("1.10")));
        // 行等权的 MAPE 才是 (1 + 100) / 2 = 50.50%
        assert_eq!(summary.mape_pct, Some(dec("50.50")));
        assert_ne!(
            summary.variance_rate, summary.mape_pct,
            "合计率看钱、MAPE 看稳 —— 两个口径不能混成一个数"
        );
    }

    #[test]
    fn summary_keeps_unforecast_actuals_in_totals() {
        let customer_a = Uuid::now_v7();
        let customer_d = Uuid::now_v7();
        let forecasts = vec![forecast_pair(Some((customer_a, "客户甲")), None, "100000", "1000")];
        // 客户甲一单没开（实际 0）；客户丁完全没预测但卖了 50000
        let actuals = VarianceActuals {
            by_customer: vec![model::CustomerActual {
                customer_id: customer_d,
                customer_name: Some("客户丁".to_string()),
                amount: dec("50000"),
                qty: dec("500"),
            }],
            ..Default::default()
        };
        let rows = assemble_variance(&forecasts, &actuals, FORECAST_DIM_CUSTOMER, "2031-05", dec("5"));
        let summary = summarize_variance(&rows);

        assert_eq!(summary.forecast_count, 1);
        assert_eq!(summary.unforecast_count, 1);
        assert_eq!(summary.row_count, 2);
        // 漏预测行的实际额必须进合计：否则「预测 10 万、实际 5 万」会被算成实际 0
        assert_eq!(summary.forecast_amount, dec("100000"));
        assert_eq!(summary.actual_amount, dec("50000"));
        assert_eq!(summary.variance_amount, dec("-50000.00"));
        assert_eq!(summary.variance_rate, Some(dec("-50.00")));
        // MAPE 只统计有预测金额的行：客户甲 -100% 一行
        assert_eq!(summary.mape_pct, Some(dec("100.00")));
        assert_eq!(summary.under_count, 1, "漏预测行不算 UNDER（它没有档位可判）");
    }

    #[test]
    fn summary_counts_zero_amount_forecast_row_but_excludes_it_from_mape() {
        let customer_a = Uuid::now_v7();
        // 只预测数量（金额 0）—— check_amount 明确允许 0，「0 件金额」是真业务写法
        let forecasts = vec![forecast_pair(Some((customer_a, "客户甲")), None, "0", "100")];
        let actuals = VarianceActuals {
            by_customer: vec![model::CustomerActual {
                customer_id: customer_a,
                customer_name: Some("客户甲".to_string()),
                amount: dec("5000"),
                qty: dec("120"),
            }],
            ..Default::default()
        };
        let rows = assemble_variance(&forecasts, &actuals, FORECAST_DIM_CUSTOMER, "2031-05", dec("5"));
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert!(row.has_forecast);
        assert_eq!(row.variance_amount, dec("5000.00"));
        assert_eq!(row.variance_rate, None, "预测金额 0 ⇒ 比率无意义（不是 0%/100%）");
        assert_eq!(
            row.deviation_level, DEVIATION_OVER,
            "预测 0 而实际 > 0 = 漏预测性质的 OVER"
        );
        // 数量侧正常：预测 100 / 实际 120
        assert_eq!(row.variance_qty, dec("20"));
        assert_eq!(row.variance_qty_rate, Some(dec("20.00")));

        let summary = summarize_variance(&rows);
        assert_eq!(summary.forecast_count, 1, "它有预测行，算 forecast_count");
        assert_eq!(summary.over_count, 1);
        assert_eq!(summary.unforecast_count, 0, "不是漏预测行（表里有预测）");
        assert_eq!(summary.forecast_amount, Decimal::ZERO);
        assert_eq!(summary.variance_rate, None);
        assert_eq!(summary.mape_pct, None, "唯一的行没有比率 ⇒ 一行都不进 MAPE，不是 0");
        assert_eq!(summary.variance_qty_rate, Some(dec("20.00")));
    }

    #[test]
    fn summary_quantity_rate_is_null_when_quantity_never_forecast() {
        let customer_a = Uuid::now_v7();
        // 只预测金额（数量 0）：数量侧没有分母，比率必须空着
        let forecasts = vec![forecast_pair(Some((customer_a, "客户甲")), None, "100000", "0")];
        let actuals = VarianceActuals {
            by_customer: vec![model::CustomerActual {
                customer_id: customer_a,
                customer_name: Some("客户甲".to_string()),
                amount: dec("90000"),
                qty: dec("300"),
            }],
            ..Default::default()
        };
        let rows = assemble_variance(&forecasts, &actuals, FORECAST_DIM_CUSTOMER, "2031-05", dec("5"));
        assert_eq!(rows[0].variance_qty_rate, None, "预测数量 0 ⇒ 数量偏差率 null");
        assert_eq!(rows[0].variance_qty, dec("300"));

        let summary = summarize_variance(&rows);
        assert_eq!(summary.forecast_qty, Decimal::ZERO);
        assert_eq!(summary.variance_qty, dec("300"));
        assert_eq!(summary.variance_qty_rate, None);
        // 金额侧照常
        assert_eq!(summary.variance_rate, Some(dec("-10.00")));
        assert_eq!(summary.mape_pct, Some(dec("10.00")));
    }

    #[test]
    fn pair_grain_unforecast_rows_keep_both_dimensions() {
        let (customer_a, customer_b, material_1, material_2) =
            (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let forecasts = vec![forecast_pair(
            Some((customer_a, "客户甲")),
            Some((material_1, "物料甲")),
            "10000",
            "100",
        )];
        let actuals = VarianceActuals {
            by_pair: vec![
                model::PairActual {
                    customer_id: customer_a,
                    customer_name: Some("客户甲".to_string()),
                    material_id: material_1,
                    material_name: Some("物料甲".to_string()),
                    amount: dec("8000"),
                    qty: dec("80"),
                },
                // 客户甲 × 物料乙：没预测但卖了
                model::PairActual {
                    customer_id: customer_a,
                    customer_name: Some("客户甲".to_string()),
                    material_id: material_2,
                    material_name: Some("物料乙".to_string()),
                    amount: dec("3000"),
                    qty: dec("30"),
                },
                // 客户乙 × 物料甲：同上
                model::PairActual {
                    customer_id: customer_b,
                    customer_name: Some("客户乙".to_string()),
                    material_id: material_1,
                    material_name: Some("物料甲".to_string()),
                    amount: dec("5000"),
                    qty: dec("50"),
                },
            ],
            ..Default::default()
        };
        let rows = assemble_variance(
            &forecasts,
            &actuals,
            FORECAST_DIM_CUSTOMER_MATERIAL,
            "2031-05",
            dec("5"),
        );
        assert_eq!(rows.len(), 3, "一行预测 + 两行漏预测");

        // 按 |偏差| 倒序：客户乙 × 物料甲 5000 > 客户甲 × 物料乙 3000 > 客户甲 × 物料甲 -2000
        assert_eq!(rows[0].display_name, "客户乙 × 物料甲");
        assert!(!rows[0].has_forecast);
        assert_eq!(rows[0].deviation_level, DEVIATION_NO_FORECAST);
        // 漏预测行必须带**两维** id + 名字（物料粒度的漏预测行客户列是空的，别混）
        assert!(rows[0].customer_id.is_some(), "客户 × 物料粒度两维都要给");
        assert!(rows[0].material_id.is_some());
        assert_eq!(rows[0].customer_name.as_deref(), Some("客户乙"));
        assert_eq!(rows[0].material_name.as_deref(), Some("物料甲"));
        assert_eq!(rows[0].variance_amount, dec("5000.00"));
        assert_eq!(rows[0].variance_rate, None);

        assert_eq!(rows[1].display_name, "客户甲 × 物料乙");
        assert!(!rows[1].has_forecast);
        assert_eq!(rows[2].display_name, "客户甲 × 物料甲");
        assert!(rows[2].has_forecast);
        assert_eq!(rows[2].variance_amount, dec("-2000.00"));
        assert_eq!(rows[2].variance_rate, Some(dec("-20.00")));

        let summary = summarize_variance(&rows);
        assert_eq!(summary.row_count, 3);
        assert_eq!(summary.forecast_count, 1);
        assert_eq!(summary.unforecast_count, 2);
        assert_eq!(summary.forecast_amount, dec("10000"));
        assert_eq!(summary.actual_amount, dec("16000"));
        assert_eq!(summary.variance_amount, dec("6000.00"));
        assert_eq!(summary.variance_rate, Some(dec("60.00")));
        assert_eq!(summary.mape_pct, Some(dec("20.00")), "两行漏预测不参与 MAPE");
        assert_eq!(
            summary.over_count + summary.under_count + summary.on_target_count,
            summary.forecast_count,
            "三个档位相加 = 有预测的行数（漏预测行不占档位）"
        );
    }

    #[test]
    fn tolerance_boundaries_pick_the_right_level() {
        // 容差 0：只有完全相等算命中，差一分钱都算偏
        assert_eq!(
            deviation_level(dec("100000"), dec("100000"), dec("0")),
            DEVIATION_ON_TARGET
        );
        assert_eq!(
            deviation_level(dec("100000.01"), dec("100000"), dec("0")),
            DEVIATION_OVER
        );
        assert_eq!(
            deviation_level(dec("99999.99"), dec("100000"), dec("0")),
            DEVIATION_UNDER
        );

        // 容差 100%：翻倍正好命中（|+100%| <= 100），再多一分钱就是 OVER
        assert_eq!(
            deviation_level(dec("200000"), dec("100000"), dec("100")),
            DEVIATION_ON_TARGET
        );
        assert_eq!(
            deviation_level(dec("200100"), dec("100000"), dec("100")),
            DEVIATION_OVER
        );
        // -100%（实际 0）也在容差内 ⇒ 命中（实际为负的订单不存在，不需另判）
        assert_eq!(
            deviation_level(dec("0"), dec("100000"), dec("100")),
            DEVIATION_ON_TARGET
        );

        // 5% 边界：±5.00% 命中，5.01% / -5.01% 出界
        assert_eq!(
            deviation_level(dec("105.00"), dec("100"), dec("5")),
            DEVIATION_ON_TARGET
        );
        assert_eq!(deviation_level(dec("95.00"), dec("100"), dec("5")), DEVIATION_ON_TARGET);
        assert_eq!(deviation_level(dec("105.01"), dec("100"), dec("5")), DEVIATION_OVER);
        assert_eq!(deviation_level(dec("94.99"), dec("100"), dec("5")), DEVIATION_UNDER);
    }

    #[test]
    fn rows_sort_by_absolute_variance_then_actual_then_name() {
        let (customer_a, customer_b, customer_c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let forecasts = vec![
            forecast_pair(Some((customer_a, "客户甲")), None, "2000", "20"),
            forecast_pair(Some((customer_b, "客户乙")), None, "1000", "10"),
        ];
        // 三行 |偏差| 全是 1000：甲 -1000（实际 1000）、乙 -1000（实际 0）、丙 漏预测 +1000（实际 1000）
        let actuals = VarianceActuals {
            by_customer: vec![
                model::CustomerActual {
                    customer_id: customer_a,
                    customer_name: Some("客户甲".to_string()),
                    amount: dec("1000"),
                    qty: dec("10"),
                },
                model::CustomerActual {
                    customer_id: customer_c,
                    customer_name: Some("客户丙".to_string()),
                    amount: dec("1000"),
                    qty: dec("10"),
                },
            ],
            ..Default::default()
        };
        let rows = assemble_variance(&forecasts, &actuals, FORECAST_DIM_CUSTOMER, "2031-05", dec("5"));
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.variance_amount.abs() == dec("1000.00")));
        // 实际额倒序 ⇒ 实际 1000 的两行在前，实际 0 的「客户乙」垫底
        assert_eq!(rows[2].display_name, "客户乙");
        assert_eq!(rows[2].actual_amount, Decimal::ZERO);
        // 同额同实际时按显示名升序（否则同一份数据每次刷新顺序都可能变）
        let mut top_two = vec![rows[0].display_name.clone(), rows[1].display_name.clone()];
        let mut expected = top_two.clone();
        expected.sort();
        assert_eq!(top_two, expected, "同额同实际按显示名升序");
        top_two.sort();
        assert_eq!(top_two, vec!["客户丙".to_string(), "客户甲".to_string()]);
    }

    #[test]
    fn summary_worst_prefers_the_row_with_a_forecast() {
        let (customer_a, customer_b) = (Uuid::now_v7(), Uuid::now_v7());
        let forecasts = vec![forecast_pair(Some((customer_a, "客户甲")), None, "4000", "40")];
        let actuals = VarianceActuals {
            by_customer: vec![
                model::CustomerActual {
                    customer_id: customer_a,
                    customer_name: Some("客户甲".to_string()),
                    amount: dec("5000"),
                    qty: dec("50"),
                },
                // 漏预测行 +1000，与客户甲同额
                model::CustomerActual {
                    customer_id: customer_b,
                    customer_name: Some("客户乙".to_string()),
                    amount: dec("1000"),
                    qty: dec("10"),
                },
            ],
            ..Default::default()
        };
        let rows = assemble_variance(&forecasts, &actuals, FORECAST_DIM_CUSTOMER, "2031-05", dec("5"));
        let summary = summarize_variance(&rows);
        let worst = summary.worst.expect("有行就有最贵偏差");
        assert_eq!(worst.variance_amount, dec("1000.00"));
        assert_eq!(
            worst.display_name, "客户甲",
            "同额偏差优先点名有预测的那行（能说清是哪条预测偏了）"
        );
        assert!(worst.forecast_no.is_some(), "有预测的行才带预测编号");
    }

    #[test]
    fn all_grain_quantity_only_actual_is_still_a_missing_forecast() {
        // 全公司没预测、金额 0 但卖了 500 件：判据是「金额或数量非 0」，必须给漏预测行
        let actuals = VarianceActuals {
            total_amount: Decimal::ZERO,
            total_qty: dec("500"),
            ..Default::default()
        };
        let rows = assemble_variance(&[], &actuals, FORECAST_DIM_ALL, "2031-05", dec("5"));
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].has_forecast);
        assert_eq!(rows[0].display_name, "全公司");
        assert_eq!(rows[0].actual_amount, Decimal::ZERO);
        assert_eq!(rows[0].actual_qty, dec("500"));
        assert_eq!(rows[0].variance_amount, Decimal::ZERO);
        assert_eq!(rows[0].variance_qty, dec("500"));
        assert_eq!(rows[0].deviation_level, DEVIATION_NO_FORECAST);

        let summary = summarize_variance(&rows);
        assert_eq!(summary.unforecast_count, 1);
        assert_eq!(summary.forecast_count, 0);
        assert_eq!(summary.actual_qty, dec("500"));
        assert_eq!(summary.variance_rate, None, "预测金额 0 ⇒ 金额偏差率无意义");
        assert_eq!(summary.variance_qty_rate, None, "预测数量 0 ⇒ 数量偏差率无意义");
        assert_eq!(summary.mape_pct, None);
    }
}
