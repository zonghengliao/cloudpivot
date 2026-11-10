//! 工时台账接口层（成本核算 · ARCH-9 §5.2 #10–#14）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-labor/...`（约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 10 | GET | `/business/erp/cost-labor` | `business:erp:cost-labor:list` | 分页列表（按工作中心裁数据范围） |
//! | 11 | POST | `/business/erp/cost-labor` | `business:erp:cost-labor:create` | 单条录入（金额服务端算） |
//! | 12 | PUT | `/business/erp/cost-labor/{id}` | `business:erp:cost-labor:update` | 部分更新（仅 `DRAFT`） |
//! | 13 | DELETE | `/business/erp/cost-labor/{id}` | `business:erp:cost-labor:delete` | 软删（仅 `DRAFT`） |
//! | 14 | POST | `/business/erp/cost-labor/{id}/confirm` | `business:erp:cost-labor:update` | `DRAFT → CONFIRMED`（**幂等**） |
//!
//! 设计 §5.2 里工时台账还有 #15 导入 / #16 模板两个接口 —— 它们是 **B2-2** 的活，
//! 不在本卡；权限码 `cost-labor:import` 已在 V0378 种下，B2-2 直接接。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_object_handler` / `lot_handler` 同款：axum handler 要 `AuthClaims` 才能跑，
//! 单测调不到；把「校验 + 落库」收进 `create_labor` / `update_labor` / `delete_labor` /
//! `confirm_labor` / `page_labors` 之后，真库用例走**同一条代码路径**（只跳过认证），
//! 操作人用 `Option<Uuid>` 传。
//!
//! # 确认幂等怎么保证（验收项「重复确认不翻倍」）
//!
//! 两道：
//!
//! 1. **判定层**：`dto::confirm_action(status)` —— 已经是 `CONFIRMED` 直接返回
//!    [`ConfirmAction::AlreadyConfirmed`]，**根本不进写路径**（所以 `updated_at`
//!    也不会动）；
//! 2. **SQL 层**：`model::confirm` 的 `WHERE ... AND status = 'DRAFT'` —— 并发下
//!    两条请求同时判定为 `Flip` 时，第二条命中 0 行，handler 再读一次发现已是
//!    `CONFIRMED`，照样返回成功。
//!
//! 关键在于「确认」只翻状态、**不写金额、不做累加**：人工成本的金额是这一行自己的
//! `amount`，`calc` 第 2 段（B2-7）按 `status='CONFIRMED'` 逐行归集。所以两次确认
//! 之后仍是「一行、一个金额」，不会出现「同一张工时被归集两次」。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_labor_dto::{
    self, ConfirmAction, CostLaborCreateReq, CostLaborFilter, CostLaborListRes, CostLaborPageQuery, CostLaborUpdateReq,
};
use crate::business::erp::cost_labor_model::{self as model, CostLaborRow};
use crate::business::erp::cost_object_dto;
use crate::business::erp::model_base::normalize_page;
use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与设计 §5.3 逐字一致） ====================

/// 列表。
pub const PERM_LIST: &str = "business:erp:cost-labor:list";
/// 新建。
pub const PERM_CREATE: &str = "business:erp:cost-labor:create";
/// 编辑 / **确认**（设计 §5.2 #14：确认复用 `update` 码，不新开码）。
pub const PERM_UPDATE: &str = "business:erp:cost-labor:update";
/// 删除（软删）。
pub const PERM_DELETE: &str = "business:erp:cost-labor:delete";
/// 导入（B2-2 用；本卡只声明，不实现接口）。
pub const PERM_IMPORT: &str = "business:erp:cost-labor:import";

/// 工时台账全部权限码（设计 §5.3 的 5 个）。
pub const LABOR_PERMISSIONS: [&str; 5] = [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_DELETE, PERM_IMPORT];

// ==================== 引用校验（建 / 改的查库部分） ====================

/// 成本对象必须存在且是 `OPEN`。
///
/// 两条不同的错：
/// * 不存在 / 已删 → **400 + `66002`**（入参指错了：改 id 或换对象）；
/// * 存在但非 `OPEN` → **409 + `66057`**（业务规则拒绝：已关批的成本已结转）。
///   前端提示与用户下一步动作完全不同，所以码不能合并。
async fn ensure_cost_object_open(cost_object_id: Uuid) -> std::result::Result<(), ApiError> {
    let object = model::cost_object_ref(cost_object_id)
        .await
        .map_err(|error| utils::db_error(error, "校验工时的成本对象"))?;
    match object {
        None => Err(ApiError::new(
            400,
            error_code::ERP_REFERENCE_MISSING,
            format!("cost_object_id 指向的成本对象 {cost_object_id} 不存在或已删除"),
        )),
        Some((object_no, status)) if status != cost_object_dto::STATUS_OPEN => {
            Err(cost_labor_dto::object_not_open(&object_no, &status))
        }
        Some(_) => Ok(()),
    }
}

/// 工作中心必须是 `sys_dept` 的现有部门；返回名称（快照来源）。
async fn ensure_dept(dept_id: Uuid) -> std::result::Result<String, ApiError> {
    match model::dept_name_of(dept_id)
        .await
        .map_err(|error| utils::db_error(error, "校验工时的工作中心"))?
    {
        Some(name) => Ok(name),
        None => Err(cost_labor_dto::bad_request(format!(
            "dept_id（{dept_id}）指向的工作中心（部门）不存在或已删除"
        ))),
    }
}

// ==================== 出参组装 ====================

/// 列表行。
fn to_list_res(row: &CostLaborRow) -> CostLaborListRes {
    CostLaborListRes {
        id: row.id.to_string(),
        period_code: row.period_code.clone(),
        work_date: row.work_date.to_string(),
        cost_object_id: row.cost_object_id.to_string(),
        cost_object_no: row.cost_object_no.clone(),
        cost_object_status: row.cost_object_status.clone(),
        product_id: row.product_id.map(|id| id.to_string()),
        product_name: row.product_name.clone(),
        employee_id: row.employee_id.map(|id| id.to_string()),
        employee_name: row.employee_name.clone(),
        dept_id: row.dept_id.to_string(),
        dept_name: Some(row.display_dept_name()),
        // 金额 / 工时一律字符串出参（§5.1：防前端 JS number 精度丢失）
        hours: row.hours.to_string(),
        machine_hours: row.machine_hours.to_string(),
        rate: row.rate.to_string(),
        amount: row.amount.to_string(),
        source_type: row.source_type.clone(),
        status: row.status.clone(),
        remark: row.remark.clone(),
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
    }
}

/// 按 id 组装出参（建 / 改 / 确认后的回单都走它）。
async fn row_of(id: Uuid) -> std::result::Result<CostLaborListRes, ApiError> {
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询工时台账行"))?
        .ok_or_else(|| cost_labor_dto::not_found(id))?;
    Ok(to_list_res(&row))
}

/// 取活行（不存在 → 404）。
async fn load(id: Uuid) -> std::result::Result<CostLaborRow, ApiError> {
    model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询工时台账行"))?
        .ok_or_else(|| cost_labor_dto::not_found(id))
}

// ==================== 业务逻辑（单测 / 真库用例直接调这些） ====================

/// 新建工时（§5.2 #11）。
pub async fn create_labor(
    operator: Option<Uuid>,
    req: CostLaborCreateReq,
) -> std::result::Result<CostLaborListRes, ApiError> {
    // 先拿成本对象 / 部门（查库校验），再拿它们的人话名字去拼规格
    let cost_object_id = cost_labor_dto::require_uuid(req.cost_object_id.as_deref(), "cost_object_id")?;
    ensure_cost_object_open(cost_object_id).await?;
    let dept_id = cost_labor_dto::require_uuid(req.dept_id.as_deref(), "dept_id")?;
    let dept_name = ensure_dept(dept_id).await?;
    let spec = cost_labor_dto::spec_from_create(&req, &dept_name)?;
    let id = Uuid::now_v7();
    model::insert(id, &spec, operator)
        .await
        .map_err(|error| cost_labor_dto::map_write_error(error, &spec, "新建工时台账"))?;
    row_of(id).await
}

/// 编辑工时（§5.2 #12）：仅 `DRAFT`，金额按合并后的工时 × 费率重算。
pub async fn update_labor(
    operator: Option<Uuid>,
    id_raw: &str,
    req: CostLaborUpdateReq,
) -> std::result::Result<CostLaborListRes, ApiError> {
    let id = cost_labor_dto::parse_uuid(id_raw, "id")?;
    let row = load(id).await?;
    if row.status != cost_labor_dto::STATUS_DRAFT {
        return Err(cost_labor_dto::wrong_status("修改", &row.status));
    }
    let mut spec = cost_labor_dto::spec_from_update(&row.to_spec(), &req)?;
    // 换了成本对象 ⇒ 新的对象也必须是 OPEN（旧对象的状态不能当成新对象的通行证）
    if spec.cost_object_id != row.cost_object_id {
        ensure_cost_object_open(spec.cost_object_id).await?;
    }
    // 换了工作中心 ⇒ 刷新快照（没换就保留原快照，快照是「当时记的」）
    if spec.dept_id != row.dept_id {
        spec.dept_name = ensure_dept(spec.dept_id).await?;
    }
    let affected = model::update(id, &spec, operator)
        .await
        .map_err(|error| cost_labor_dto::map_write_error(error, &spec, "编辑工时台账"))?;
    if affected == 0 {
        // 查过存在、更新却是 0 行 ⇒ 期间被确认或被软删了，再查一次给精确原因
        let latest = load(id).await?;
        return Err(cost_labor_dto::wrong_status("修改", &latest.status));
    }
    row_of(id).await
}

/// 软删（§5.2 #13）：仅 `DRAFT`。
///
/// 为什么已确认的不许删：`calc` 第 2 段按 `status='CONFIRMED'` 逐行归集，
/// 归集完再删会让「已归集的金额」失去来源行（`biz_cost_collect.source_id` 指向空）。
/// 要改就作废重录 —— 与「已结算期间只读、改数走红冲」（设计 R-8）同一条思路。
pub async fn delete_labor(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = cost_labor_dto::parse_uuid(id_raw, "id")?;
    let row = load(id).await?;
    if row.status != cost_labor_dto::STATUS_DRAFT {
        return Err(cost_labor_dto::wrong_status("删除", &row.status));
    }
    let affected = model::soft_delete(id, operator)
        .await
        .map_err(|error| utils::db_error(error, "删除工时台账行"))?;
    if affected == 0 {
        let latest = load(id).await?;
        return Err(cost_labor_dto::wrong_status("删除", &latest.status));
    }
    Ok(())
}

/// 确认（§5.2 #14）：`DRAFT → CONFIRMED`，**幂等**。
pub async fn confirm_labor(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<CostLaborListRes, ApiError> {
    let id = cost_labor_dto::parse_uuid(id_raw, "id")?;
    let row = load(id).await?;
    match cost_labor_dto::confirm_action(&row.status) {
        // 已经是 CONFIRMED：一律幂等返回当前行（不写库 ⇒ 金额不会第二次累加）
        ConfirmAction::AlreadyConfirmed => return Ok(to_list_res(&row)),
        ConfirmAction::Flip => {}
    }
    let affected = model::confirm(id, operator)
        .await
        .map_err(|error| utils::db_error(error, "确认工时台账行"))?;
    if affected == 0 {
        // 并发：别人刚刚确认完了 ⇒ 照样返成功（幂等的并发形态）
        let latest = load(id).await?;
        if latest.status != cost_labor_dto::STATUS_CONFIRMED {
            return Err(cost_labor_dto::conflict(format!(
                "工时台账行 {id} 的状态刚刚变成了 {}，请刷新后重试",
                latest.status
            )));
        }
        return Ok(to_list_res(&latest));
    }
    row_of(id).await
}

/// 列表分页（按工作中心裁数据范围 ⇒ 要 `principal`）。
pub async fn page_labors(
    principal: &ScopedPrincipal,
    query: CostLaborPageQuery,
) -> std::result::Result<PageData<Vec<CostLaborListRes>>, ApiError> {
    let filter = filter_of(&query)?;
    let (page, page_size) = (filter.page, filter.page_size);
    let (rows, total) = model::page(principal, &filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询工时台账"))?;
    Ok(PageData {
        list: rows.iter().map(to_list_res).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 归一化列表筛选（纯逻辑；单测可直接验）。
pub fn filter_of(query: &CostLaborPageQuery) -> std::result::Result<CostLaborFilter, ApiError> {
    let period_code = match cost_labor_dto::clean_optional(query.period_code.clone()) {
        None => None,
        Some(raw) => Some(cost_labor_dto::normalize_period(Some(&raw))?),
    };
    let status = cost_labor_dto::clean_optional(query.status.clone())
        .map(|text| text.to_ascii_uppercase())
        .map(|text| {
            if cost_labor_dto::STATUSES.contains(&text.as_str()) {
                Ok(text)
            } else {
                Err(cost_labor_dto::bad_request(format!(
                    "status 只认 {:?}（收到「{text}」）",
                    cost_labor_dto::STATUSES
                )))
            }
        })
        .transpose()?;
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    Ok(CostLaborFilter {
        period_code,
        cost_object_id: cost_labor_dto::parse_opt_uuid(query.cost_object_id.as_deref(), "cost_object_id")?,
        dept_id: cost_labor_dto::parse_opt_uuid(query.dept_id.as_deref(), "dept_id")?,
        employee_id: cost_labor_dto::parse_opt_uuid(query.employee_id.as_deref(), "employee_id")?,
        status,
        keyword: cost_labor_dto::clean_optional(query.keyword.clone()),
        page,
        page_size,
    })
}

// ==================== HTTP 入口 ====================

/// 工时台账分页。
#[nik::permission(
    path = "/business/erp/cost-labor",
    method = "GET",
    permission = "business:erp:cost-labor:list"
)]
#[nik::get("/business/erp/cost-labor")]
pub async fn cost_labor_page_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<CostLaborPageQuery>,
) -> ResResult<PageData<Vec<CostLaborListRes>>> {
    let principal = principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(page_labors(&principal, query).await?))
}

/// 新建工时。
#[nik::permission(
    path = "/business/erp/cost-labor",
    method = "POST",
    permission = "business:erp:cost-labor:create"
)]
#[nik::post("/business/erp/cost-labor")]
pub async fn cost_labor_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CostLaborCreateReq>,
) -> ResResult<CostLaborListRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_labor(operator, req).await?))
}

/// 编辑工时。
#[nik::permission(
    path = "/business/erp/cost-labor/{id}",
    method = "PUT",
    permission = "business:erp:cost-labor:update"
)]
#[nik::put("/business/erp/cost-labor/{id}")]
pub async fn cost_labor_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CostLaborUpdateReq>,
) -> ResResult<CostLaborListRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_labor(operator, &id, req).await?))
}

/// 删除工时（软删，仅 `DRAFT`）。
#[nik::permission(
    path = "/business/erp/cost-labor/{id}",
    method = "DELETE",
    permission = "business:erp:cost-labor:delete"
)]
#[nik::delete("/business/erp/cost-labor/{id}")]
pub async fn cost_labor_delete_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    delete_labor(operator, &id).await?;
    Ok(Res::ok(()))
}

/// 确认工时（`DRAFT → CONFIRMED`，幂等）。
#[nik::permission(
    path = "/business/erp/cost-labor/{id}/confirm",
    method = "POST",
    permission = "business:erp:cost-labor:update"
)]
#[nik::post("/business/erp/cost-labor/{id}/confirm")]
pub async fn cost_labor_confirm_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<CostLaborListRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(confirm_labor(operator, &id).await?))
}

/// 权限码自检出口（与 `handler::all_permissions` 同形，便于汇总脚本收集）。
pub fn all_permissions() -> Vec<&'static str> {
    LABOR_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::shared::scope::SCOPED_TABLES;
    use nik::prelude::chrono::Utc;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    // ---------- 权限 / 路由 ----------

    #[test]
    fn permission_codes_match_design_5_3() {
        assert_eq!(PERM_LIST, "business:erp:cost-labor:list");
        assert_eq!(PERM_CREATE, "business:erp:cost-labor:create");
        assert_eq!(PERM_UPDATE, "business:erp:cost-labor:update");
        assert_eq!(PERM_DELETE, "business:erp:cost-labor:delete");
        assert_eq!(PERM_IMPORT, "business:erp:cost-labor:import");
        assert_eq!(all_permissions().len(), 5, "设计 §5.3 给了 5 个码");
        for code in LABOR_PERMISSIONS {
            let parts: Vec<&str> = code.split(':').collect();
            assert_eq!(parts.len(), 4, "{code}");
            assert_eq!(parts[0], "business", "{code}");
            assert_eq!(parts[1], "erp", "{code}");
            assert_eq!(parts[2], "cost-labor", "{code}");
        }
    }

    #[test]
    fn route_paths_have_no_api_prefix() {
        for path in [
            "/business/erp/cost-labor",
            "/business/erp/cost-labor/{id}",
            "/business/erp/cost-labor/{id}/confirm",
        ] {
            assert!(path.starts_with("/business/erp/cost-labor"), "{path}");
            assert!(!path.starts_with("/api"), "{path} 不能带 /api 前缀（约定 8）");
        }
    }

    /// 确认复用 `update` 码（设计 §5.2 #14），不新开码 —— 否则菜单种子（已应用迁移）没承载行。
    #[test]
    fn confirm_reuses_update_permission() {
        assert_eq!(PERM_UPDATE, "business:erp:cost-labor:update");
        assert_eq!(LABOR_PERMISSIONS.len(), 5);
    }

    // ---------- 列表筛选归一化 ----------

    #[test]
    fn filter_normalizes_period_status_and_paging() {
        let query = CostLaborPageQuery {
            period_code: Some(" 2026-11 ".to_string()),
            status: Some("confirmed".to_string()),
            cost_object_id: Some("018f4b2c-1111-7222-8333-444455556666".to_string()),
            page: None,
            page_size: Some(0),
            ..Default::default()
        };
        let filter = filter_of(&query).expect("筛选");
        assert_eq!(filter.period_code.as_deref(), Some("2026-11"));
        assert_eq!(filter.status.as_deref(), Some("CONFIRMED"));
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, 20, "page_size=0 回落到默认 20");
        assert!(filter.cost_object_id.is_some());

        let query = CostLaborPageQuery {
            page_size: Some(10_000),
            ..Default::default()
        };
        assert_eq!(filter_of(&query).expect("筛选").page_size, 200, "上限 200");
    }

    #[test]
    fn filter_rejects_unknown_period_status_and_uuid() {
        for bad in ["2026-13", "26-11", "2026/11"] {
            let query = CostLaborPageQuery {
                period_code: Some(bad.to_string()),
                ..Default::default()
            };
            let error = filter_of(&query).expect_err(bad);
            assert_eq!(error.status, 400, "{bad}");
            assert!(error.msg.contains("period_code"), "{bad}: {}", error.msg);
        }

        let query = CostLaborPageQuery {
            status: Some("POSTED".to_string()),
            ..Default::default()
        };
        assert!(filter_of(&query).expect_err("未知状态").msg.contains("status"));

        let query = CostLaborPageQuery {
            cost_object_id: Some("not-a-uuid".to_string()),
            ..Default::default()
        };
        let error = filter_of(&query).expect_err("非法 UUID");
        assert!(error.msg.contains("cost_object_id"), "{}", error.msg);
    }

    // ---------- 出参映射 ----------

    fn sample_row() -> CostLaborRow {
        let now = Utc::now();
        CostLaborRow {
            id: Uuid::now_v7(),
            period_code: "2026-11".to_string(),
            work_date: "2026-11-06".parse().expect("date"),
            cost_object_id: Uuid::now_v7(),
            employee_id: None,
            employee_name: "张三".to_string(),
            dept_id: Uuid::now_v7(),
            dept_name: "机加工车间（旧名）".to_string(),
            hours: dec("8"),
            machine_hours: dec("2"),
            rate: dec("50"),
            amount: dec("400"),
            source_type: "MANUAL".to_string(),
            status: "DRAFT".to_string(),
            remark: Some("首件".to_string()),
            created_at: now,
            updated_at: now,
            cost_object_no: Some("CB-20261101-000001".to_string()),
            cost_object_status: Some("OPEN".to_string()),
            product_id: Some(Uuid::now_v7()),
            product_name: Some("钢轴".to_string()),
            dept_name_live: Some("机加工车间（新名）".to_string()),
        }
    }

    #[test]
    fn list_res_carries_names_and_string_amounts() {
        let row = sample_row();
        let res = to_list_res(&row);
        assert_eq!(res.employee_name, "张三");
        assert_eq!(res.cost_object_no.as_deref(), Some("CB-20261101-000001"));
        assert_eq!(res.product_name.as_deref(), Some("钢轴"));
        assert_eq!(res.dept_name.as_deref(), Some("机加工车间（新名）"), "出参用实时名");
        // 金额 / 工时是字符串（§5.1：防 JS number 精度丢失）
        assert_eq!(res.amount, "400");
        assert_eq!(res.hours, "8");
        assert_eq!(res.rate, "50");
        assert_eq!(res.work_date, "2026-11-06");
        assert!(res.created_at.contains('T'), "{}", res.created_at);
    }

    /// 台账要按车间裁：锚点必须是登记过的 scoped 目标（不能偷偷 unscoped）。
    #[test]
    fn labor_list_is_scoped_by_workshop() {
        assert!(
            SCOPED_TABLES.contains(&model::LABOR_TARGET),
            "biz_cost_labor 未登记进 SCOPED_TABLES"
        );
        assert_eq!(model::LABOR_TARGET.dept_col, Some("dept_id"));
    }

    /// 对象状态标签与 409 文案（前端提示 / 用户下一步动作的分界线）。
    #[test]
    fn object_not_open_message_points_to_next_action() {
        assert_eq!(cost_labor_dto::object_status_label("OPEN"), "在制");
        assert_eq!(cost_labor_dto::object_status_label("CLOSED"), "已关批");
        assert_eq!(cost_labor_dto::object_status_label("CANCELLED"), "已作废");
        let error = cost_labor_dto::object_not_open("CB-20261110-000001", "CLOSED");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, 66057);
        assert!(error.msg.contains("CB-20261110-000001"), "{}", error.msg);
        assert!(error.msg.contains("已关批"), "{}", error.msg);
        assert!(error.msg.contains("新建一批"), "要给出下一步动作：{}", error.msg);
    }

    // ---------- 确认幂等（验收项的结构侧） ----------

    /// 判定层：`CONFIRMED` 再确认 ⇒ 直接返回，不进写路径。
    #[test]
    fn confirm_on_confirmed_row_never_writes() {
        assert_eq!(
            cost_labor_dto::confirm_action(cost_labor_dto::STATUS_CONFIRMED),
            ConfirmAction::AlreadyConfirmed
        );
        assert_eq!(
            cost_labor_dto::confirm_action(cost_labor_dto::STATUS_DRAFT),
            ConfirmAction::Flip
        );
        // 第三、第四次确认与第二次同路（判定是纯函数，不存在「状态机累积」）
        for _ in 0..3 {
            assert_eq!(
                cost_labor_dto::confirm_action(cost_labor_dto::STATUS_CONFIRMED),
                ConfirmAction::AlreadyConfirmed
            );
        }
    }

    /// SQL 层：确认语句带 DRAFT 闸，且不碰 hours / amount（不翻倍的机械保证）。
    #[test]
    fn confirm_sql_only_flips_status() {
        let sql = model::confirm_sql();
        assert!(sql.contains("SET status = ?"), "{sql}");
        assert!(sql.contains("AND status = ?"), "缺 DRAFT 闸：{sql}");
        assert!(!sql.contains("hours"), "确认不许改工时：{sql}");
        assert!(!sql.contains("amount"), "确认不许改金额：{sql}");
    }

    /// 幂等语义（真库用例的口径说明，写在这里便于对照）：
    /// 两次确认后行数不变、金额不变、状态CONFIRMED。真库验证见 `cost_labor_db`。
    #[test]
    fn idempotency_expectation_is_documented() {
        let row = sample_row();
        let res = to_list_res(&row);
        let after = to_list_res(&CostLaborRow {
            status: "CONFIRMED".to_string(),
            ..row
        });
        assert_eq!(res.amount, after.amount, "确认不许改金额");
        assert_eq!(res.hours, after.hours, "确认不许改工时");
        assert_eq!(after.status, "CONFIRMED");
    }
}
