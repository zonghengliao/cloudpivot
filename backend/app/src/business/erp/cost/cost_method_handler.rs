//! 成本方法配置接口层（成本核算 · M5-01 · 品种法 / 分批法 / 标准成本）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-method/...`（AGENTS 约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 1 | GET | `/business/erp/cost-method` | `business:erp:cost-method:list` | 分页列表（范围 / 方法 / 状态 / 对象 / 关键字） |
//! | 2 | GET | `/business/erp/cost-method/effective` | `business:erp:cost-method:list` | 查「某对象在某天按哪种方法算」（只读探针，含来源与能否计算） |
//! | 3 | POST | `/business/erp/cost-method` | `business:erp:cost-method:create` | 新增一条方法配置版本 |
//! | 4 | PUT | `/business/erp/cost-method/{id}` | `business:erp:cost-method:update` | 改配置（已生效过的版本只允许改停止日 / 状态 / 备注） |
//! | 5 | DELETE | `/business/erp/cost-method/{id}` | `business:erp:cost-method:delete` | 软删（已生效过的版本 409，改停用） |
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_object_handler` / `cost_alloc_rule_handler` 同款：axum handler 要 `AuthClaims`
//! 才跑得起来，单测调不到。把「校验 + 落库 + 判定」收进 `create_config` / `update_config` /
//! `delete_config` / `page_configs` / `effective_of` 之后，真库用例走**同一条代码路径**
//! （只跳过认证），操作人用 `Option<Uuid>` 传。
//!
//! # 三条纪律（与 dto 模块头呼应）
//!
//! 1. **同范围生效期不许重叠**：一重叠，「这期按什么算」就没有确定答案（409 + `66102`）。
//! 2. **已生效过（`effective_from < 今天`）的版本只允许改停止日 / 状态 / 备注**：
//!    改方法 / 改生效日 = 事后改写「那段期间按什么方法算的」（409 + `66105`）。
//!    切换方法的正确姿势：旧版 `effective_to` 收到切换日前一天 + 新建一条切换日生效的版本。
//! 3. **已生效过的版本不许删**（409 + `66104`）：配置行本身就是历史口径的凭据。
//!
//! # 一期只实现分批法
//!
//! 配置可以选三种方法里的任意一种，但 [`ensure_methods_runnable_in_tx`] 会在成本计算
//! （`cost_calc_period`）开算前拦下「本期生效范围里有 `method <> 'JOB'` 的配置」
//! （409 + `66101`）。不静默按分批法算：算出来的报表是错的，而且看不出来。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{FixedOffset, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_method_dto::{
    self as dto, CostMethodCreateReq, CostMethodEffectiveQuery, CostMethodEffectiveRes, CostMethodPageQuery,
    CostMethodRes, CostMethodSpec, CostMethodUpdateReq,
};
use crate::business::erp::cost::cost_method_model::{
    self as model, BlockingMethodRow, CostMethodFilter, CostMethodRow,
};
use crate::business::erp::model_base::normalize_page;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码（与 V0460 菜单种子逐字一致） ====================

/// 列表 / 生效方法查询。
pub const PERM_LIST: &str = "business:erp:cost-method:list";
/// 新增。
pub const PERM_CREATE: &str = "business:erp:cost-method:create";
/// 修改。
pub const PERM_UPDATE: &str = "business:erp:cost-method:update";
/// 删除（软删）。
pub const PERM_DELETE: &str = "business:erp:cost-method:delete";

/// 成本方法配置全部权限码（4 个，与 `V0460` 的 4 行菜单一一对应）。
pub const COST_METHOD_PERMISSIONS: [&str; 4] = [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_DELETE];

// ==================== 小件 ====================

/// 今日（CST）—— 与 `invoice::today_cst` 同口径（业务日期一律按北京时间）。
pub(crate) fn today_cst() -> NaiveDate {
    Utc::now()
        .with_timezone(&FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定"))
        .date_naive()
}

/// 行 → 列表出参。
fn to_res(row: &CostMethodRow) -> CostMethodRes {
    CostMethodRes {
        id: row.id.to_string(),
        method: row.method.clone(),
        method_label: dto::method_label(&row.method).to_string(),
        method_implemented: dto::method_implemented(&row.method),
        method_note: dto::method_note(&row.method).to_string(),
        scope: row.scope_type.clone(),
        scope_label: dto::scope_label(&row.scope_type).to_string(),
        scope_id: row.scope_id.map(|id| id.to_string()),
        scope_object_no: row.scope_object_no.clone(),
        scope_object_status: row.scope_object_status.clone(),
        effective_from: row.effective_from.to_string(),
        effective_to: row.effective_to.map(|date| date.to_string()),
        status: row.status,
        status_label: dto::status_label(row.status).to_string(),
        remark: row.remark.clone(),
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
    }
}

/// 取活行（不存在 / 已软删 → 404）。
async fn load(id: Uuid) -> std::result::Result<CostMethodRow, ApiError> {
    model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询成本方法配置"))?
        .ok_or_else(|| dto::not_found(id))
}

/// 按 id 组装出参（建 / 改后的回单走它）。
async fn row_of(id: Uuid) -> std::result::Result<CostMethodRes, ApiError> {
    Ok(to_res(&load(id).await?))
}

/// `scope_id` 必须指向一个**未删除、未作废**的成本对象；返回对象号（报错文案用）。
pub(crate) async fn ensure_object(scope_id: Uuid) -> std::result::Result<String, ApiError> {
    let object = model::object_of(scope_id)
        .await
        .map_err(|error| utils::db_error(error, "校验成本方法配置的成本对象"))?
        .ok_or_else(|| {
            dto::scope_invalid(format!(
                "scope_id（{scope_id}）指向的成本对象不存在或已删除；请先建成本对象，或改用 scope = GLOBAL"
            ))
        })?;
    if object.status == "CANCELLED" {
        return Err(dto::scope_invalid(format!(
            "成本对象 {} 已作废（CANCELLED）：作废对象不参与成本计算，给它配核算方法没有意义。\
             请换一个 OPEN / CLOSED 的对象，或改用 scope = GLOBAL",
            object.object_no
        )));
    }
    Ok(object.object_no)
}

/// 全量配置（判定用）。
async fn all_configs() -> std::result::Result<Vec<CostMethodRow>, ApiError> {
    model::configs()
        .await
        .map_err(|error| utils::db_error(error, "查询成本方法配置"))
}

// ==================== 列表 ====================

/// 列表分页（本表 SCOPE-EXEMPT，不需要 `principal`）。
pub async fn page_configs(query: CostMethodPageQuery) -> std::result::Result<PageData<Vec<CostMethodRes>>, ApiError> {
    let filter = filter_of(&query)?;
    let (page, page_size) = (filter.page, filter.page_size);
    let (rows, total) = model::page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询成本方法配置"))?;
    Ok(PageData {
        list: rows.iter().map(to_res).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 归一化列表筛选（纯逻辑；单测可直接验）。
pub fn filter_of(query: &CostMethodPageQuery) -> std::result::Result<CostMethodFilter, ApiError> {
    let scope = match query.scope.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        None => None,
        Some(raw) => Some(dto::normalize_scope(Some(raw))?),
    };
    let method = match query.method.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        None => None,
        Some(raw) => Some(dto::normalize_method(Some(raw))?),
    };
    let status = dto::normalize_status(query.status)?;
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    Ok(CostMethodFilter {
        scope,
        method,
        status,
        scope_id: crate::business::erp::cost::cost_labor_dto::parse_opt_uuid(query.scope_id.as_deref(), "scope_id")?,
        keyword: crate::business::erp::cost::cost_labor_dto::clean_optional(query.keyword.clone()),
        page,
        page_size,
    })
}

// ==================== 生效方法查询 ====================

/// 查「某成本对象在某天按哪种方法算」。
///
/// 出参带**来源**（对象级 / 全局 / 代码兜底）与**能否计算**（一期只有分批法算得出来），
/// 界面据此把「选了标准成本 ⇒ 这期算不出来」提前说清楚，而不是等用户点了计算才 409。
pub async fn effective_of(query: CostMethodEffectiveQuery) -> std::result::Result<CostMethodEffectiveRes, ApiError> {
    let cost_object_id =
        crate::business::erp::cost::cost_labor_dto::parse_opt_uuid(query.cost_object_id.as_deref(), "cost_object_id")?;
    let on = match crate::business::erp::cost::cost_labor_dto::clean_optional(query.on.clone()) {
        None => today_cst(),
        Some(raw) => crate::business::erp::cost::cost_labor_dto::parse_date(&raw, "on")?,
    };
    let object_no = match cost_object_id {
        None => None,
        Some(id) => Some(ensure_object(id).await?),
    };
    let rows = all_configs().await?;
    let views = rows.iter().map(CostMethodRow::to_view).collect::<Vec<_>>();
    let resolution = dto::resolve(&views, cost_object_id, on);
    let implemented = dto::method_implemented(&resolution.method);
    let engine_hint = if implemented {
        format!(
            "{on} 按「{}」核算：一期已实现，成本计算按它执行",
            dto::method_label(&resolution.method)
        )
    } else {
        format!(
            "{on} 按「{}」核算，但该方法在一期还没实现 —— 这一期点「计算成本」会被拒绝（66101），\
             请把这批配置的生效日推到下期、停用，或改回分批法",
            dto::method_label(&resolution.method)
        )
    };
    Ok(CostMethodEffectiveRes {
        cost_object_id: cost_object_id.map(|id| id.to_string()),
        cost_object_no: object_no,
        on_date: on.to_string(),
        method: resolution.method.clone(),
        method_label: dto::method_label(&resolution.method).to_string(),
        method_implemented: implemented,
        method_note: dto::method_note(&resolution.method).to_string(),
        source: resolution.source.clone(),
        source_label: dto::source_label(&resolution.source).to_string(),
        config_id: resolution.config.as_ref().map(|hit| hit.id.to_string()),
        effective_from: resolution.config.as_ref().map(|hit| hit.effective_from.to_string()),
        effective_to: resolution
            .config
            .as_ref()
            .and_then(|hit| hit.effective_to)
            .map(|date| date.to_string()),
        engine_hint,
    })
}

// ==================== 增 / 改 / 删 ====================

/// 新增配置。
///
/// # 异常情况
///
/// - 值域 / 范围成对不合法 ⇒ 400（`10001` / `66103`）
/// - 同范围生效期与既有版本重叠 ⇒ 409 + `66102`
/// - 同范围同生效日（并发下唯一键兜底）⇒ 409
/// - 查库 / 写库失败 ⇒ 500
pub async fn create_config(
    operator: Option<Uuid>,
    req: CostMethodCreateReq,
) -> std::result::Result<CostMethodRes, ApiError> {
    let spec = dto::spec_from_create(&req)?;
    let scope_name = match spec.scope_id {
        None => "全公司默认".to_string(),
        Some(id) => ensure_object(id).await?,
    };
    let rows = all_configs().await?;
    let views = rows.iter().map(CostMethodRow::to_view).collect::<Vec<_>>();
    if let Some(hit) = dto::find_overlap(&views, &spec, None) {
        return Err(dto::overlap_error(
            &spec.scope_type,
            &scope_name,
            &hit.method,
            &dto::range_text(hit.effective_from, hit.effective_to),
            &dto::range_text(spec.effective_from, spec.effective_to),
        ));
    }
    let id = Uuid::now_v7();
    model::insert(id, &spec, operator)
        .await
        .map_err(|error| map_write_error(error, &spec, &scope_name, "新增成本方法配置"))?;
    row_of(id).await
}

/// 改配置。
///
/// # 异常情况
///
/// - 不存在 / 已删 ⇒ 404
/// - 已生效过（`effective_from < 今天`）却改了方法 / 范围 / 生效日 ⇒ 409 + `66105`
/// - 改完与别的版本生效期重叠 ⇒ 409 + `66102`
/// - 查库 / 写库失败 ⇒ 500
pub async fn update_config(
    operator: Option<Uuid>,
    id_raw: &str,
    req: CostMethodUpdateReq,
) -> std::result::Result<CostMethodRes, ApiError> {
    let id = crate::business::erp::cost::cost_labor_dto::parse_uuid(id_raw, "id")?;
    let current = load(id).await?;
    let current_spec = current.to_spec();
    let spec = dto::spec_from_update(&req, &current_spec)?;
    let scope_name = match spec.scope_id {
        None => "全公司默认".to_string(),
        Some(scope_id) => ensure_object(scope_id).await?,
    };

    // 已生效过的版本：只允许改停止日 / 状态 / 备注（纪律 2）
    let today = today_cst();
    if current_spec.effective_from < today {
        let rewritten = spec.method != current_spec.method
            || spec.scope_type != current_spec.scope_type
            || spec.scope_id != current_spec.scope_id
            || spec.effective_from != current_spec.effective_from;
        if rewritten {
            return Err(dto::locked_effective(
                &scope_name,
                &current_spec.method,
                current_spec.effective_from,
                today,
            ));
        }
    }

    let rows = all_configs().await?;
    let views = rows.iter().map(CostMethodRow::to_view).collect::<Vec<_>>();
    if let Some(hit) = dto::find_overlap(&views, &spec, Some(id)) {
        return Err(dto::overlap_error(
            &spec.scope_type,
            &scope_name,
            &hit.method,
            &dto::range_text(hit.effective_from, hit.effective_to),
            &dto::range_text(spec.effective_from, spec.effective_to),
        ));
    }
    let affected = model::update(id, &spec, operator)
        .await
        .map_err(|error| map_write_error(error, &spec, &scope_name, "修改成本方法配置"))?;
    if affected == 0 {
        // 查过存在、改却是 0 行 ⇒ 被并发删掉了
        return Err(dto::not_found(id));
    }
    row_of(id).await
}

/// 软删配置（纪律 3：已生效过的版本不许删）。
pub async fn delete_config(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = crate::business::erp::cost::cost_labor_dto::parse_uuid(id_raw, "id")?;
    let current = load(id).await?;
    let today = today_cst();
    if current.effective_from < today {
        return Err(dto::delete_in_use(current.effective_from, today));
    }
    let affected = model::soft_delete(id, operator)
        .await
        .map_err(|error| utils::db_error(error, "删除成本方法配置"))?;
    if affected == 0 {
        return Err(dto::not_found(id));
    }
    Ok(())
}

/// 写库错误 → 友好 `ApiError`（唯一出口，判据走 `DbError` 变体不看文案）。
///
/// - `UniqueViolation`：撞的是 `uk_biz_cmc_{global,object}_effective`（同范围同生效日）
///   ⇒ `409` + `66102`，报文点名那一天（handler 的重叠预检在并发下可能双双通过）。
/// - 其它 `CheckViolation`：值域 / 成对 / 区间 / 状态（dto 已先拦，这里是兜底）⇒ `400` + `10001`。
/// - 其它：500（`utils::db_error`，真实原因只进日志）。
fn map_write_error(err: DbError, spec: &CostMethodSpec, scope_name: &str, ctx: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { .. } => ApiError::new(
            409,
            dto::CODE_OVERLAP,
            format!(
                "{scope_name} 在 {} 已经有一条方法配置（同一范围同一生效日只能有一条版本，\
                 并发写入撞了唯一键）。请刷新后直接修改已有那条，或换一个生效日",
                spec.effective_from
            ),
        ),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "数据不满足数据库约束{}（方法值域 / 范围成对 / 生效期区间 / 状态值域）",
                constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
            ),
        ),
        _ => utils::db_error(err, ctx),
    }
}

// ==================== 成本计算入口的拦截（供 cost_calc_period 调用） ====================

/// 成本计算开算前的方法闸：本期（`[start, end]`）生效范围内不许有未实现的核算方法。
///
/// 这是本卡与「计算引擎」之间**唯一**的接线点：不接的话，用户配了标准成本而计算
/// 照旧按分批法算完，报表标签写「标准成本」但数字是分批法的 —— 错账且不可发现。
///
/// # 异常情况
///
/// 有未实现方法 ⇒ `409` + `66101`（报文逐个点名对象号与三条出路）；查库失败 ⇒ 500。
pub async fn ensure_methods_runnable_in_tx(
    ex: &impl SqlExecutor,
    period_code: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> std::result::Result<(), ApiError> {
    let blocking: Vec<BlockingMethodRow> = model::blocking_in(ex, start, end)
        .await
        .map_err(|error| utils::db_error(error, "检查本期成本核算方法配置"))?;
    if blocking.is_empty() {
        return Ok(());
    }
    let entries = blocking
        .iter()
        .map(|row| (row.subject.clone(), row.method.clone()))
        .collect::<Vec<_>>();
    Err(dto::unimplemented_error(period_code, &entries))
}

/// 全部权限码（与其余成本模块 `all_permissions` 同形）。
pub fn all_permissions() -> Vec<&'static str> {
    COST_METHOD_PERMISSIONS.to_vec()
}

// ==================== HTTP 入口 ====================

/// 成本方法配置分页。
#[nik::permission(
    path = "/business/erp/cost-method",
    method = "GET",
    permission = "business:erp:cost-method:list"
)]
#[nik::get("/business/erp/cost-method")]
pub async fn cost_method_page_api(Query(query): Query<CostMethodPageQuery>) -> ResResult<PageData<Vec<CostMethodRes>>> {
    Ok(Res::ok(page_configs(query).await?))
}

/// 查生效方法（某对象在某天按哪种方法算）。
#[nik::permission(
    path = "/business/erp/cost-method/effective",
    method = "GET",
    permission = "business:erp:cost-method:list"
)]
#[nik::get("/business/erp/cost-method/effective")]
pub async fn cost_method_effective_api(
    Query(query): Query<CostMethodEffectiveQuery>,
) -> ResResult<CostMethodEffectiveRes> {
    Ok(Res::ok(effective_of(query).await?))
}

/// 新增配置。
#[nik::permission(
    path = "/business/erp/cost-method",
    method = "POST",
    permission = "business:erp:cost-method:create"
)]
#[nik::post("/business/erp/cost-method")]
pub async fn cost_method_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CostMethodCreateReq>,
) -> ResResult<CostMethodRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_config(operator, req).await?))
}

/// 修改配置。
#[nik::permission(
    path = "/business/erp/cost-method/{id}",
    method = "PUT",
    permission = "business:erp:cost-method:update"
)]
#[nik::put("/business/erp/cost-method/{id}")]
pub async fn cost_method_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CostMethodUpdateReq>,
) -> ResResult<CostMethodRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_config(operator, &id, req).await?))
}

/// 删除配置（软删）。
#[nik::permission(
    path = "/business/erp/cost-method/{id}",
    method = "DELETE",
    permission = "business:erp:cost-method:delete"
)]
#[nik::delete("/business/erp/cost-method/{id}")]
pub async fn cost_method_delete_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    delete_config(operator, &id).await?;
    Ok(Res::ok(()))
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::Duration;

    // ---------- 权限 / 路由 ----------

    /// 4 个权限码与 V0460 的 4 行菜单逐字一致（多一个 = 死码，少一个 = 裸路由）。
    #[test]
    fn permission_codes_match_the_migration_seed() {
        assert_eq!(
            COST_METHOD_PERMISSIONS,
            [
                "business:erp:cost-method:list",
                "business:erp:cost-method:create",
                "business:erp:cost-method:update",
                "business:erp:cost-method:delete",
            ]
        );
        assert_eq!(all_permissions().len(), 4);
        for code in COST_METHOD_PERMISSIONS {
            assert!(code.starts_with("business:erp:cost-method:"), "{code}");
        }
    }

    /// 列表筛选归一化：大小写 / 空白 / 非法值。
    #[test]
    fn filter_normalizes_and_rejects_unknown() {
        let query = CostMethodPageQuery {
            scope: Some(" global ".to_string()),
            method: Some("job".to_string()),
            status: Some(1),
            scope_id: None,
            keyword: Some("  CB-2026  ".to_string()),
            page: Some(0),
            page_size: Some(0),
        };
        let filter = filter_of(&query).expect("合法筛选");
        assert_eq!(filter.scope.as_deref(), Some("GLOBAL"));
        assert_eq!(filter.method.as_deref(), Some("JOB"));
        assert_eq!(filter.status, Some(1));
        assert_eq!(filter.keyword.as_deref(), Some("CB-2026"), "关键字要 trim");
        assert_eq!(filter.page, 1, "页号 0 → 1");
        assert!(filter.page_size > 0);

        let bad = CostMethodPageQuery {
            method: Some("FIFO".to_string()),
            ..Default::default()
        };
        assert!(filter_of(&bad).is_err(), "未知方法必须 400");

        let empty = CostMethodPageQuery {
            scope: Some("   ".to_string()),
            ..Default::default()
        };
        assert_eq!(
            filter_of(&empty).expect("空白 = 不筛").scope,
            None,
            "空白筛选条件等同不传"
        );
    }

    /// 路由数量与权限注解数量对齐（少一条 = 裸接口，N16 也会红）。
    #[test]
    fn routes_and_permissions_are_paired() {
        // 5 条路由：list / effective / create / update / delete
        assert_eq!(
            COST_METHOD_PERMISSIONS.len(),
            4,
            "4 个码承载 5 条路由（list 承载两条读接口）"
        );
        assert_eq!(PERM_LIST, "business:erp:cost-method:list");
        assert_eq!(PERM_CREATE, "business:erp:cost-method:create");
        assert_eq!(PERM_UPDATE, "business:erp:cost-method:update");
        assert_eq!(PERM_DELETE, "business:erp:cost-method:delete");
    }

    /// 今天按 CST 取：UTC 16:00 之后已经是北京的次日（界面上「已生效」判定靠它）。
    #[test]
    fn today_is_cst_not_utc() {
        let today = today_cst();
        let utc_today = Utc::now().date_naive();
        assert!(
            today == utc_today || today == utc_today + Duration::days(1),
            "CST 日期只能是 UTC 今天或 UTC 明天（实测 {today} vs {utc_today}）"
        );
    }
}
