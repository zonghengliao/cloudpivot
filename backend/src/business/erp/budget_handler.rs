//! 预算接口层（FIN-1 / ARCH-6 第三章）：编制 / 提交审批 / 调整 / 执行报表 / 科目映射。
//!
//! | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/budget/page` | `budget:list` | 分页（软删不返回，按部门数据权限裁剪） |
//! | GET | `/business/erp/budget/{id}` | `budget:list` | 详情（12 月分解 + 占用记录 + 版本链） |
//! | POST | `/business/erp/budget` | `budget:create` | 新建（`DRAFT` + v1 + 12 月分解） |
//! | PUT | `/business/erp/budget/{id}` | `budget:update` | 改草稿（整单覆盖） |
//! | DELETE | `/business/erp/budget/{id}` | `budget:delete` | 删草稿（软删） |
//! | POST | `/business/erp/budget/{id}/submit` | `budget:submit` | 提交审批（`budget_approve`） |
//! | POST | `/business/erp/budget/{id}/adjust` | `budget:adjust` | 调整 = 新版本 → 直接送审 |
//! | GET | `/business/erp/budget/report` | `budget:report` | 预算执行表（已用 / 可用 / 执行率） |
//! | GET | `/business/erp/budget/mapping/page` | `budget:mapping` | 映射分页 |
//! | POST | `/business/erp/budget/mapping` | `budget:mapping` | 新建映射 |
//! | PUT | `/business/erp/budget/mapping/{id}` | `budget:mapping` | 修改映射 |
//! | DELETE | `/business/erp/budget/mapping/{id}` | `budget:mapping` | 删映射（软删） |
//!
//! # 三条硬约束（写在这里免得后人「顺手优化」）
//!
//! ① **状态只能走状态机**：`state_machine::transition(Entity::Budget, …)` 是唯一写入口
//!    （它自己写 `biz_status_log`）。本文件没有 `match (from, to)` 的硬判。
//! ② **额度不许 UPDATE 成已生效版本**：调整只能新增版本行（[`adjust`]），
//!    `ACTIVE` 之后 `total_amount` 一个字节都不许改（ARCH-6 红线 6）。
//! ③ **预算科目必须末级**：非末级 `is_leaf = 0` 一律 400 —— 否则父科目与本级各占一次额度，
//!    可用余额会算两遍（ARCH-6 红线 3）。
//!
//! # 不产生凭证
//!
//! 预算审批**不落账**（预算是额度控制，不是经济业务发生）。执行侧的凭证由
//! 报销入账 / 采购入库各自的既有链路产生，本模块零凭证代码。

use axum::Json;
use axum::extract::{Extension, Path, Query};
use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde_json::json;

use crate::business::erp::budget_bridge;
use crate::business::erp::budget_dto::{
    AdjustBudgetReq, BudgetDetailRes, BudgetFilter, BudgetRes, CreateBudgetReq, MappingReq,
    MappingRes, PeriodRes, ReportRes, UpdateBudgetReq, UsageRes, conflict, invalid, normalize_page,
    parse_uuid_text, status_label, validate_adjust, validate_control_mode, validate_mapping,
    validate_periods,
};
use crate::business::erp::budget_guard;
use crate::business::erp::budget_model::{
    self, CHANGE_ADJUST, CHANGE_ORIGINAL, STATUS_ACTIVE, STATUS_DRAFT, STATUS_SUBMITTED,
};
use crate::business::flow::{engine, model as flow_model};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

fn operator_of(claims: &TokenClaims) -> Option<Uuid> {
    utils::current_user_uuid(claims)
}

fn not_found() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "预算单不存在或已删除")
}

fn map_not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "预算科目映射不存在或已删除",
    )
}

/// 部门名快照（部门不存在 → 400，不允许编出「无主预算」）。
async fn dept_name_of(dept_id: Uuid) -> std::result::Result<String, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT dept_name FROM sys_dept WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&dept_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询部门"))?;
    match rows.first() {
        None => Err(invalid(format!(
            "部门 {} 不存在或已删除：预算必须挂在真实部门上",
            dept_id
        ))),
        Some(row) => Ok(row.get_typed("dept_name")?),
    }
}

/// 科目快照 + 末级校验（ARCH-6 红线 3）。
async fn account_snapshot(account_id: Uuid) -> std::result::Result<(String, String), ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT account_code, account_name, is_leaf, status FROM biz_account \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&account_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询会计科目"))?;
    let Some(row) = rows.first() else {
        return Err(invalid(format!(
            "预算科目 {} 不存在或已删除：请先在「会计科目」里建好末级科目",
            account_id
        )));
    };
    let is_leaf: i16 = row.get_typed("is_leaf")?;
    let status: i16 = row.get_typed("status")?;
    if is_leaf != 1 {
        return Err(invalid(format!(
            "科目 {} 不是末级科目：预算只能编到末级，否则父科目与子科目会各占一次额度（重复计算）",
            row.get_typed::<String>("account_code")?
        )));
    }
    if status != 1 {
        return Err(invalid(format!(
            "科目 {} 已停用，不能编预算",
            row.get_typed::<String>("account_code")?
        )));
    }
    Ok((
        row.get_typed("account_code")?,
        row.get_typed("account_name")?,
    ))
}

// ==================== ① 分页 / 详情 ====================

/// 预算分页。
#[nik::permission(
    path = "/business/erp/budget/page",
    method = "GET",
    permission = "business:erp:budget:list"
)]
#[nik::get("/business/erp/budget/page")]
pub async fn budget_page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(filter): Query<BudgetFilter>,
) -> ResResult<PageData<Vec<BudgetRes>>> {
    let (page, page_size) = normalize_page(
        filter.page.unwrap_or(1),
        filter.page_size.unwrap_or(budget_dto_default()),
    );
    let mut query = filter.clone();
    query.page = Some(page);
    query.page_size = Some(page_size);
    // 预算按部门做数据权限：本表的 `ScopeTarget` 以 `scoped = true` 构造（部门列 `dept_id`），
    // 于是闸① 强制走 `scope::paginate(…, &principal)`（`paginate_unscoped` 会直接报错）。
    // 基础版没有配置部门数据范围时 principal 展开为「全部」，行为与不裁剪一致；
    // 专业版一旦给角色配了部门范围，预算列表自动跟着裁剪 —— 这正是会计科目的部门口径要的。
    let principal = crate::business::shared::principal::resolve_for_claims(&claims).await?;
    let (rows, total) = budget_model::budget_page(&query, &principal)
        .await
        .map_err(|error| utils::db_error(error, "分页查询预算"))?;
    let list: Vec<BudgetRes> = rows.iter().map(BudgetRes::from).collect();
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    ))
}

fn budget_dto_default() -> u64 {
    crate::business::erp::budget_dto::DEFAULT_PAGE_SIZE
}

/// 预算详情（12 月分解 + 当月已用 + 占用记录 + 版本链）。
#[nik::permission(
    path = "/business/erp/budget/{id}",
    method = "GET",
    permission = "business:erp:budget:list"
)]
#[nik::get("/business/erp/budget/{id}")]
pub async fn budget_detail(Path(id): Path<String>) -> ResResult<BudgetDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 详情组装（handler 与真机用例共用）。
pub async fn detail_of(id: Uuid) -> std::result::Result<BudgetDetailRes, ApiError> {
    let row = budget_model::find(id)
        .await
        .map_err(|error| utils::db_error(error, "查询预算"))?
        .ok_or_else(not_found)?;
    // 当月已用（现算；口径与 budget_guard 完全一致）
    let slot_probe = budget_guard::Slot::new(row.dept_id, row.account_id, "2000-01");
    let _ = slot_probe;
    let usage_rows = budget_guard::usages_of(db(), budget_guard::SOURCE_PURCHASE, row.id)
        .await
        .unwrap_or_default();
    let usages: Vec<UsageRes> = usage_rows
        .iter()
        .map(|usage| UsageRes::of(usage, budget_guard::SOURCE_PURCHASE))
        .collect();
    let periods = budget_model::periods_of(db(), row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询预算月度分解"))?;
    // 分月已用：按期间聚合
    let mut period_res = Vec::with_capacity(periods.len());
    for period in &periods {
        let used = month_used_of(&row, &period.period_code).await?;
        period_res.push(PeriodRes::of(period, used));
    }
    let year_used = year_used_of(&row).await?;
    let versions = budget_model::version_chain(row.dept_id, row.account_id, row.fiscal_year)
        .await
        .map_err(|error| utils::db_error(error, "查询预算版本链"))?;
    Ok(BudgetDetailRes {
        budget: BudgetRes::from(&row),
        periods: period_res,
        usages,
        year_used,
        year_available: row.total_amount - year_used,
        versions: versions.iter().map(BudgetRes::from).collect(),
        next_statuses: state_machine::Entity::Budget
            .next_statuses(&row.status)
            .into_iter()
            .map(str::to_string)
            .collect(),
    })
}

async fn year_used_of(row: &budget_model::BudgetRow) -> std::result::Result<Decimal, ApiError> {
    let rows = db()
        .query_sql(
            "SELECT COALESCE(SUM(amount), 0)::text AS used FROM biz_budget_usage \
              WHERE dept_id = ?::uuid AND account_id = ?::uuid AND fiscal_year = ?::int \
                AND status IN ('OCCUPIED','CONSUMED') AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::from(&row.dept_id),
                Value::from(&row.account_id),
                Value::from(i32::from(row.fiscal_year)),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "统计年度已用"))?;
    Ok(rows
        .first()
        .and_then(|row| row.get_typed::<String>("used").ok())
        .and_then(|text| text.parse::<Decimal>().ok())
        .unwrap_or(Decimal::ZERO))
}

async fn month_used_of(
    row: &budget_model::BudgetRow,
    period_code: &str,
) -> std::result::Result<Decimal, ApiError> {
    let rows = db()
        .query_sql(
            "SELECT COALESCE(SUM(amount), 0)::text AS used FROM biz_budget_usage \
              WHERE dept_id = ?::uuid AND account_id = ?::uuid AND period_code = ? \
                AND status IN ('OCCUPIED','CONSUMED') AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::from(&row.dept_id),
                Value::from(&row.account_id),
                Value::Text(period_code.to_string()),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "统计月度已用"))?;
    Ok(rows
        .first()
        .and_then(|row| row.get_typed::<String>("used").ok())
        .and_then(|text| text.parse::<Decimal>().ok())
        .unwrap_or(Decimal::ZERO))
}

// ==================== ② 编制 ====================

/// 新建预算（v1，`DRAFT`）。
#[nik::permission(
    path = "/business/erp/budget",
    method = "POST",
    permission = "business:erp:budget:create"
)]
#[nik::post("/business/erp/budget")]
pub async fn budget_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateBudgetReq>,
) -> ResResult<BudgetDetailRes> {
    let id = create_budget(&req, operator_of(&claims)).await?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 建单核心（handler 与用例共用）。
pub async fn create_budget(
    req: &CreateBudgetReq,
    operator: Option<Uuid>,
) -> std::result::Result<Uuid, ApiError> {
    let control_mode = validate_control_mode(req.control_mode.as_deref())?;
    let periods = validate_periods(req.fiscal_year, req.total_amount, req.periods.as_deref())?;
    let dept_id = parse_uuid_text(&req.dept_id, "dept_id")?;
    let account_id = parse_uuid_text(&req.account_id, "account_id")?;
    let dept_name = dept_name_of(dept_id).await?;
    let (account_code, account_name) = account_snapshot(account_id).await?;
    // 槽唯一：已有 ACTIVE 或 已有任何版本 → 409（编制不是调整，改额度请走 adjust）
    let mut existing = budget_model::active_of_slot(db(), dept_id, account_id, req.fiscal_year)
        .await
        .map_err(|error| utils::db_error(error, "查询该槽已生效预算"))?;
    if existing.is_none() {
        let pending =
            budget_model::pending_adjust_of_slot(db(), dept_id, account_id, req.fiscal_year)
                .await
                .map_err(|error| utils::db_error(error, "查询该槽在审预算"))?;
        existing = pending;
    }
    if let Some(row) = existing {
        return Err(conflict(format!(
            "{} / {} / {} 年已存在预算 {}（{}，v{}）：调整已有预算请用「调整」（走审批），不要新建",
            dept_name,
            account_code,
            req.fiscal_year,
            row.budget_no,
            status_label(&row.status),
            row.version_no
        )));
    }
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建预算事务"))?;
    let outcome = async {
        let no = budget_model::next_budget_no(&tx, now)
            .await
            .map_err(|error| utils::db_error(error, "取预算单号"))?;
        let id = Uuid::now_v7();
        let new = budget_model::BudgetMut {
            fiscal_year: req.fiscal_year,
            dept_id,
            dept_name,
            account_id,
            account_code,
            account_name,
            total_amount: req.total_amount,
            control_mode,
            version_no: 1,
            supersedes_id: None,
            change_type: CHANGE_ORIGINAL.to_string(),
            change_reason: None,
            remark: req.remark.clone(),
            operator,
        };
        budget_model::insert_budget(&tx, id, &no, &new)
            .await
            .map_err(build_conflict)?;
        budget_model::replace_periods(&tx, id, &periods)
            .await
            .map_err(|error| utils::db_error(error, "写入月度分解"))?;
        Ok::<Uuid, ApiError>(id)
    }
    .await;
    match outcome {
        Ok(id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交建预算事务"))?;
            tracing::info!(%id, budget_no = %req.fiscal_year, "预算已编制（DRAFT v1）");
            Ok(id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 23505 → 409（同一槽已有版本 / 单号重复）。
fn build_conflict(error: DbError) -> ApiError {
    match error {
        DbError::UniqueViolation { .. } => conflict(
            "同一「部门 × 科目 × 年度」已有预算版本：改额度请用「调整」（新增版本走审批），\
             不要重复编制",
        ),
        other => ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("写入预算失败：{other}"),
        ),
    }
}

/// 改草稿（整单覆盖，只允许 `DRAFT`）。
#[nik::permission(
    path = "/business/erp/budget/{id}",
    method = "PUT",
    permission = "business:erp:budget:update"
)]
#[nik::put("/business/erp/budget/{id}")]
pub async fn budget_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateBudgetReq>,
) -> ResResult<BudgetDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let row = budget_model::find(id)
        .await
        .map_err(|error| utils::db_error(error, "查询预算"))?
        .ok_or_else(not_found)?;
    if row.status != STATUS_DRAFT {
        return Err(conflict(format!(
            "预算 {} 当前是「{}」：只有草稿能改。已生效的预算要改额度请用「调整」（走审批，新增版本）",
            row.budget_no,
            status_label(&row.status)
        )));
    }
    let control_mode = validate_control_mode(req.control_mode.as_deref())?;
    let periods = validate_periods(row.fiscal_year, req.total_amount, req.periods.as_deref())?;
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改预算事务"))?;
    let outcome = async {
        let new = budget_model::BudgetMut {
            fiscal_year: row.fiscal_year,
            dept_id: row.dept_id,
            dept_name: row.dept_name.clone(),
            account_id: row.account_id,
            account_code: row.account_code.clone(),
            account_name: row.account_name.clone(),
            total_amount: req.total_amount,
            control_mode,
            version_no: row.version_no,
            supersedes_id: row.supersedes_id,
            change_type: row.change_type.clone(),
            change_reason: row.change_reason.clone(),
            remark: req.remark.clone(),
            operator: operator_of(&claims),
        };
        let affected = budget_model::update_draft(&tx, id, &new)
            .await
            .map_err(|error| utils::db_error(error, "更新预算草稿"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "预算 {} 的状态在本次修改期间已被改动，请刷新后重试",
                row.budget_no
            )));
        }
        budget_model::replace_periods(&tx, id, &periods)
            .await
            .map_err(|error| utils::db_error(error, "重写月度分解"))?;
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交改预算事务"))?;
            Ok(Res::ok(detail_of(id).await?))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 删草稿（软删；已生效 / 审批中一律 409）。
#[nik::permission(
    path = "/business/erp/budget/{id}",
    method = "DELETE",
    permission = "business:erp:budget:delete"
)]
#[nik::delete("/business/erp/budget/{id}")]
pub async fn budget_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<serde_json::Value> {
    let id = parse_uuid_text(&id, "id")?;
    let row = budget_model::find(id)
        .await
        .map_err(|error| utils::db_error(error, "查询预算"))?
        .ok_or_else(not_found)?;
    if row.status != STATUS_DRAFT {
        return Err(conflict(format!(
            "预算 {} 当前是「{}」：只有草稿能删。已生效的预算不能删（历史归档物）",
            row.budget_no,
            status_label(&row.status)
        )));
    }
    budget_model::soft_delete(db(), id, operator_of(&claims))
        .await
        .map_err(|error| utils::db_error(error, "删除预算草稿"))?;
    Ok(Res::ok(json!({"id": id.to_string(), "deleted": true})))
}

// ==================== ③ 提交审批 ====================

/// 提交预算审批（`budget_approve`）。
#[nik::permission(
    path = "/business/erp/budget/{id}/submit",
    method = "POST",
    permission = "business:erp:budget:submit"
)]
#[nik::post("/business/erp/budget/{id}/submit")]
pub async fn budget_submit(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<BudgetDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    submit_budget(id, operator_of(&claims)).await?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 提交核心（handler / 调整 / 用例共用）。
pub async fn submit_budget(id: Uuid, operator: Option<Uuid>) -> std::result::Result<(), ApiError> {
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let row = budget_model::find(id)
        .await
        .map_err(|error| utils::db_error(error, "查询预算"))?
        .ok_or_else(not_found)?;
    if row.status == STATUS_SUBMITTED {
        return Err(conflict(format!(
            "预算 {} 已在审批中：请等审批结束，或从审批中心撤回后再提交",
            row.budget_no
        )));
    }
    if row.status != STATUS_DRAFT {
        return Err(conflict(format!(
            "预算 {} 当前是「{}」（{}），只有草稿可以提交审批",
            row.budget_no,
            status_label(&row.status),
            row.status
        )));
    }
    // 分解完整性再校验一遍（草稿可能被直接写库改坏）
    let periods = budget_model::periods_of(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询月度分解"))?;
    if periods.len() != 12 {
        return Err(invalid(format!(
            "预算 {} 的月度分解只有 {} 行（必须 12 行），不能提交",
            row.budget_no,
            periods.len()
        )));
    }
    let sum: Decimal = periods.iter().map(|period| period.amount).sum();
    if sum != row.total_amount {
        return Err(invalid(format!(
            "预算 {} 的 12 个月合计 {sum} ≠ 年度额度 {}，不能提交",
            row.budget_no, row.total_amount
        )));
    }
    let Some(flow) = budget_bridge::published_budget_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询预算审批流程"))?
    else {
        return Err(ApiError::new(
            409,
            error_code::APPROVAL_FLOW_NOT_CONFIGURED,
            format!(
                "预算 {} 需要走审批，但系统里没有已发布的 budget_approve 流程，所以不能提交。\
                 请管理员在「审批流程」里发布预算审批流程（budget_approve）：\
                 编制走「部门主管 → 财务（≥50 万加总经理会签）」，\
                 调整走「财务前置 → 总经理（≥50 万加财务总监会签）」。",
                row.budget_no
            ),
        ));
    };
    if flow_model::biz_instance_running(budget_bridge::BIZ_TYPE, id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        return Err(conflict(format!(
            "预算 {} 已有在跑的审批单：请等审批结束或先撤回",
            row.budget_no
        )));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let start = engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(row.id),
        biz_no: Some(row.budget_no.clone()),
        title: format!("{} 预算审批", row.budget_no)
            .chars()
            .take(200)
            .collect::<String>(),
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: budget_form_data(&row, sum),
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启预算审批事务"))?;
    let outcome = async {
        state_machine::transition(
            &tx,
            state_machine::ENTITY_BUDGET,
            row.id,
            &row.budget_no,
            &budget_bridge::BUDGET_LEGAL_EDGES,
            STATUS_DRAFT,
            STATUS_SUBMITTED,
            operator_id,
            Some("提交预算审批"),
            None,
        )
        .await?;
        let mut out = engine::start(&tx, &start)
            .await
            .map_err(|error| utils::db_error(error, "发起预算审批单"))?;
        if let Some(rejected) = out.rejected.take() {
            return Err(ApiError::new(rejected.status, rejected.code, rejected.msg));
        }
        let instance_id = out.instance_id.ok_or_else(|| {
            ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id")
        })?;
        let affected = budget_model::apply_status(
            &tx,
            row.id,
            STATUS_DRAFT,
            STATUS_SUBMITTED,
            Some(instance_id),
            Some(operator_id),
            None,
        )
        .await
        .map_err(|error| utils::db_error(error, "预算单进入审批中"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "预算 {} 的状态在本次提交期间已被改动，请刷新后重试",
                row.budget_no
            )));
        }
        Ok::<(engine::ActionOut, Uuid), ApiError>((out, instance_id))
    }
    .await;
    let (mut out, instance) = match outcome {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交预算审批事务"))?;
    let auto_finished = out.instance_status.eq_ignore_ascii_case("APPROVED");
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    tracing::info!(
        budget_id = %row.id,
        budget_no = %row.budget_no,
        %instance,
        "预算已提交审批（SUBMITTED）"
    );
    if auto_finished {
        budget_bridge::on_budget_settled(Some(instance), "APPROVED").await;
    }
    Ok(())
}

/// 审批表单变量（条件边的取值来源；金额必须是 JSON **数字**）。
///
/// 字段名与 ARCH-6 §3.4.4 的契约逐字一致（`form.change_type` / `form.total_amount` 等），
/// 改字段名等于改流程配置，**不许漂移**。
fn budget_form_data(row: &budget_model::BudgetRow, period_sum: Decimal) -> serde_json::Value {
    json!({
        "budget_no": row.budget_no,
        "fiscal_year": row.fiscal_year,
        "dept_id": row.dept_id.to_string(),
        "dept_name": row.dept_name,
        "account_code": row.account_code,
        "account_name": row.account_name,
        "total_amount": json_number(&row.total_amount.round_dp(4).to_string()),
        "change_type": row.change_type,
        "version_no": row.version_no,
        "has_prior_version": row.supersedes_id.is_some(),
        "period_sum_equal": period_sum == row.total_amount,
        // 内控留痕：审批人 ≠ 制单人（`budget_bridge::guard_self_approval` 兜底）
        "create_by": row.create_by.map(|id| id.to_string()),
    })
}

fn json_number(text: &str) -> serde_json::Value {
    text.parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
        .unwrap_or_else(|| serde_json::Value::Number(serde_json::Number::from(0)))
}

// ==================== ④ 调整 ====================

/// 预算调整：**新增版本**（`version_no + 1`）→ 直接送审。
#[nik::permission(
    path = "/business/erp/budget/{id}/adjust",
    method = "POST",
    permission = "business:erp:budget:adjust"
)]
#[nik::post("/business/erp/budget/{id}/adjust")]
pub async fn budget_adjust(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<AdjustBudgetReq>,
) -> ResResult<BudgetDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let new_id = adjust(id, &req, operator_of(&claims)).await?;
    submit_budget(new_id, operator_of(&claims)).await?;
    Ok(Res::ok(detail_of(new_id).await?))
}

/// 调整核心（建新版本草稿）。返回新版本 id。
pub async fn adjust(
    id: Uuid,
    req: &AdjustBudgetReq,
    operator: Option<Uuid>,
) -> std::result::Result<Uuid, ApiError> {
    let current = budget_model::find(id)
        .await
        .map_err(|error| utils::db_error(error, "查询预算"))?
        .ok_or_else(not_found)?;
    if current.status != STATUS_ACTIVE {
        return Err(conflict(format!(
            "预算 {} 当前是「{}」：只能调整已生效（ACTIVE）的预算。草稿直接改，审批中的先撤回",
            current.budget_no,
            status_label(&current.status)
        )));
    }
    // ① 只能调「当年或未来年」（调历史年度 = 篡改已归档报表）
    let this_year = Utc::now().date_naive().format("%Y").to_string();
    let this_year: i16 = this_year.parse().unwrap_or(current.fiscal_year);
    if current.fiscal_year < this_year {
        return Err(conflict(format!(
            "{} 年的预算已归档，不能调整（只能调当年或未来年）",
            current.fiscal_year
        )));
    }
    // ② 同一槽只允许一条在审的调整单（ARCH-6 §3.7.2 第 3 条坑）
    if let Some(pending) = budget_model::pending_adjust_of_slot(
        db(),
        current.dept_id,
        current.account_id,
        current.fiscal_year,
    )
    .await
    .map_err(|error| utils::db_error(error, "查询在审调整单"))?
    {
        return Err(conflict(format!(
            "该槽已有在审的调整单 {}（{}）：等它审批结束再调整",
            pending.budget_no,
            status_label(&pending.status)
        )));
    }
    let control_mode = validate_control_mode(req.control_mode.as_deref())?;
    let (periods, reason) = validate_adjust(
        current.fiscal_year,
        req.total_amount,
        req.periods.as_deref(),
        req.change_reason.as_deref(),
    )?;
    // ③ 调低到「已用 > 新额度」允许，但要在日志里说清（审批人看得到 form_data 的额度）
    let used = year_used_of(&current).await?;
    if used > req.total_amount {
        tracing::warn!(
            budget_id = %current.id,
            budget_no = %current.budget_no,
            %used,
            new_total = %req.total_amount,
            "预算调整：新额度低于已用金额（允许，但审批人必须看见差额）"
        );
    }
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启调整事务"))?;
    let outcome = async {
        let version_no = budget_model::max_version_of_slot(
            &tx,
            current.dept_id,
            current.account_id,
            current.fiscal_year,
        )
        .await
        .map_err(|error| utils::db_error(error, "查询版本号"))?
            + 1;
        let no = budget_model::next_budget_no(&tx, now)
            .await
            .map_err(|error| utils::db_error(error, "取预算单号"))?;
        let new_id = Uuid::now_v7();
        let new = budget_model::BudgetMut {
            fiscal_year: current.fiscal_year,
            dept_id: current.dept_id,
            dept_name: current.dept_name.clone(),
            account_id: current.account_id,
            account_code: current.account_code.clone(),
            account_name: current.account_name.clone(),
            total_amount: req.total_amount,
            control_mode,
            version_no,
            supersedes_id: Some(current.id),
            change_type: CHANGE_ADJUST.to_string(),
            change_reason: Some(reason.clone()),
            remark: req.remark.clone(),
            operator,
        };
        budget_model::insert_budget(&tx, new_id, &no, &new)
            .await
            .map_err(build_conflict)?;
        budget_model::replace_periods(&tx, new_id, &periods)
            .await
            .map_err(|error| utils::db_error(error, "写入月度分解"))?;
        Ok::<Uuid, ApiError>(new_id)
    }
    .await;
    match outcome {
        Ok(new_id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交调整事务"))?;
            tracing::info!(
                old_id = %current.id,
                %new_id,
                "预算调整已建新版本（DRAFT，待送审）"
            );
            Ok(new_id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== ⑤ 执行报表 ====================

/// 预算执行表。
#[nik::permission(
    path = "/business/erp/budget/report",
    method = "GET",
    permission = "business:erp:budget:report"
)]
#[nik::get("/business/erp/budget/report")]
pub async fn budget_report(Query(filter): Query<BudgetFilter>) -> ResResult<Vec<ReportRes>> {
    let rows = budget_model::report(&filter)
        .await
        .map_err(|error| utils::db_error(error, "查询预算执行报表"))?;
    Ok(Res::ok(rows.iter().map(ReportRes::from).collect()))
}

// ==================== ⑥ 科目映射 ====================

/// 映射分页。
#[nik::permission(
    path = "/business/erp/budget/mapping/page",
    method = "GET",
    permission = "business:erp:budget:mapping"
)]
#[nik::get("/business/erp/budget/mapping/page")]
pub async fn mapping_page(
    Query(filter): Query<BudgetFilter>,
) -> ResResult<PageData<Vec<MappingRes>>> {
    let (page, page_size) = normalize_page(
        filter.page.unwrap_or(1),
        filter.page_size.unwrap_or(budget_dto_default()),
    );
    let mut query = filter.clone();
    query.page = Some(page);
    query.page_size = Some(page_size);
    let (rows, total) = budget_model::map_page(&query)
        .await
        .map_err(|error| utils::db_error(error, "分页查询预算科目映射"))?;
    Ok(Res::page(
        rows.iter().map(MappingRes::from).collect(),
        u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    ))
}

/// 新建映射。
#[nik::permission(
    path = "/business/erp/budget/mapping",
    method = "POST",
    permission = "business:erp:budget:mapping"
)]
#[nik::post("/business/erp/budget/mapping")]
pub async fn mapping_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<MappingReq>,
) -> ResResult<serde_json::Value> {
    validate_mapping(&req)?;
    let account_id = parse_uuid_text(&req.account_id, "account_id")?;
    account_snapshot(account_id).await?;
    let ref_id =
        crate::business::erp::budget_dto::parse_optional_uuid(req.ref_id.as_deref(), "ref_id")?;
    let dept_id =
        crate::business::erp::budget_dto::parse_optional_uuid(req.dept_id.as_deref(), "dept_id")?;
    let id = Uuid::now_v7();
    budget_model::map_insert(
        db(),
        id,
        req.map_type.trim(),
        ref_id,
        req.ref_code.as_deref(),
        account_id,
        dept_id,
        req.priority.unwrap_or(0),
        req.remark.as_deref(),
        operator_of(&claims),
    )
    .await
    .map_err(|error| match error {
        DbError::UniqueViolation { .. } => {
            conflict("该目标（map_type + ref_id + 部门）已有映射：请直接修改那一条，不要重复新建")
        }
        other => utils::db_error(other, "新建预算科目映射"),
    })?;
    Ok(Res::ok(json!({"id": id.to_string()})))
}

/// 修改映射。
#[nik::permission(
    path = "/business/erp/budget/mapping/{id}",
    method = "PUT",
    permission = "business:erp:budget:mapping"
)]
#[nik::put("/business/erp/budget/mapping/{id}")]
pub async fn mapping_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<MappingReq>,
) -> ResResult<serde_json::Value> {
    let id = parse_uuid_text(&id, "id")?;
    validate_mapping(&req)?;
    let account_id = parse_uuid_text(&req.account_id, "account_id")?;
    account_snapshot(account_id).await?;
    let dept_id =
        crate::business::erp::budget_dto::parse_optional_uuid(req.dept_id.as_deref(), "dept_id")?;
    let affected = budget_model::map_update(
        db(),
        id,
        req.ref_code.as_deref(),
        account_id,
        dept_id,
        req.priority.unwrap_or(0),
        req.status.unwrap_or(1),
        req.remark.as_deref(),
        operator_of(&claims),
    )
    .await
    .map_err(|error| utils::db_error(error, "修改预算科目映射"))?;
    if affected == 0 {
        return Err(map_not_found());
    }
    Ok(Res::ok(json!({"id": id.to_string(), "updated": true})))
}

/// 删映射（软删）。
#[nik::permission(
    path = "/business/erp/budget/mapping/{id}",
    method = "DELETE",
    permission = "business:erp:budget:mapping"
)]
#[nik::delete("/business/erp/budget/mapping/{id}")]
pub async fn mapping_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<serde_json::Value> {
    let id = parse_uuid_text(&id, "id")?;
    let affected = budget_model::map_delete(db(), id, operator_of(&claims))
        .await
        .map_err(|error| utils::db_error(error, "删除预算科目映射"))?;
    if affected == 0 {
        return Err(map_not_found());
    }
    Ok(Res::ok(json!({"id": id.to_string(), "deleted": true})))
}
