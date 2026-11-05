//! 费用报销接口层（FIN-3 / ARCH-6 第四章）：费用类型 / 报销单 / 审批 / 付款 / 作废。
//!
//! | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/expense-type/page` | `expense-type:list` | 费用类型分页 |
//! | GET | `/business/erp/expense-type/list` | `expense-type:list` | 启用类型（建单下拉） |
//! | POST | `/business/erp/expense-type` | `expense-type:create` | 新建类型 |
//! | PUT | `/business/erp/expense-type/{id}` | `expense-type:update` | 改类型 |
//! | DELETE | `/business/erp/expense-type/{id}` | `expense-type:delete` | 删类型（被引用则 409） |
//! | GET | `/business/erp/expense/page` | `expense:list` | 报销单分页（按 dept 数据权限） |
//! | GET | `/business/erp/expense/pay/page` | `expense:pay` | 待付款列表（已审批未付清） |
//! | GET | `/business/erp/expense/{id}` | `expense:detail` | 详情（明细 / 付款 / 预算占用 / 凭证） |
//! | POST | `/business/erp/expense` | `expense:create` | 建单（明细 ≥1，含税录入自动拆价税） |
//! | PUT | `/business/erp/expense/{id}` | `expense:update` | 改草稿（明细整体重建） |
//! | DELETE | `/business/erp/expense/{id}` | `expense:delete` | 删草稿（软删） |
//! | POST | `/business/erp/expense/{id}/submit` | `expense:submit` | 提交审批（预算守卫 + 发起流程） |
//! | POST | `/business/erp/expense/{id}/withdraw` | `expense:submit` | 撤回（回草稿 + 释放预算） |
//! | POST | `/business/erp/expense/{id}/cancel` | `expense:cancel` | 作废（已入账的红冲凭证） |
//! | POST | `/business/erp/expense/{id}/pay` | `expense:pay` | 付款登记（生成付款凭证） |
//!
//! # 三条硬约束（写在这里免得后人「顺手优化」）
//!
//! ① **状态只能走状态机**：`state_machine::transition` 是唯一的状态写入口
//!    （它自己写 `biz_status_log`）；本文件没有 `match (from, to)` 的硬判。
//! ② **金额恒等式每次写库前复算**：`expense_compute::Summary` 算出三件套 + 条数，
//!    单头与明细同事务写入；提交 / 入账前再用 `expense_bridge::check_totals` 复核一次
//!    （对不上 → 66042，绝不让「明细与单头对不上」的单子进审批）。
//! ③ **付款必须行锁报销单**（`report_lock_in`），且付款与凭证同事务。

use axum::Json;
use axum::extract::{Extension, Path, Query};
use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde_json::json;
use std::result::Result;

use crate::business::erp::expense_bridge::{self, BIZ_TYPE};
use crate::business::erp::expense_compute::{self, Summary};
use crate::business::erp::expense_dto::{
    self, CancelReq, ItemRes, PayReq, PaymentRes, ReportDetailRes, ReportFilter, ReportReq,
    ReportRes, TypeFilter, TypeReq, TypeRes, UsageRes, conflict, invalid, normalize_page,
    parse_uuid_text, status_label,
};
use crate::business::erp::expense_model::{
    self, ItemInsert, PaymentInsert, ReportInsert, ReportRow, TypeRow,
};
use crate::business::erp::{budget_guard, voucher, voucher_multi};
use crate::business::flow::{engine, model as flow_model};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

fn operator_of(claims: &TokenClaims) -> Option<Uuid> {
    utils::current_user_uuid(claims)
}

fn not_found() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "报销单不存在或已删除")
}

fn type_not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "费用类型不存在或已删除",
    )
}

/// 岗位用 db_error（500 + 原始错误进 internal_detail）。
fn db_err(error: DbError, ctx: &str) -> ApiError {
    utils::db_error(error, ctx)
}

/// 当前业务日（CST）—— 付款日期缺省值。
fn today_cst() -> nik::prelude::chrono::NaiveDate {
    voucher::today_cst(Utc::now())
}

/// 部门名（报销单快照用；拿不到就报 400，不写空串冒充）。
async fn dept_name_of(dept_id: Uuid) -> Result<String, ApiError> {
    // SCOPE-EXEMPT: 主数据按主键取 1 行。
    let rows = db()
        .query_sql(
            "SELECT dept_name FROM sys_dept WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&dept_id)],
        )
        .await
        .map_err(|error| db_err(error, "查询部门"))?;
    match rows.first() {
        None => Err(invalid(format!("部门 {dept_id} 不存在或已删除"))),
        Some(row) => row
            .get_typed("dept_name")
            .map_err(|error| db_err(error, "读取部门名")),
    }
}

/// 报销人姓名 + 部门（可以是别人：财务代填）。
async fn reporter_profile(user_id: Uuid) -> Result<(String, Uuid), ApiError> {
    // SCOPE-EXEMPT: 用户按主键取 1 行。
    let rows = db()
        .query_sql(
            "SELECT coalesce(nullif(real_name,''), username) AS name, dept_id \
               FROM sys_user WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&user_id)],
        )
        .await
        .map_err(|error| db_err(error, "查询报销人"))?;
    let Some(row) = rows.first() else {
        return Err(invalid(format!("报销人 {user_id} 不存在或已停用")));
    };
    let name: String = row
        .get_typed("name")
        .map_err(|error| db_err(error, "读取报销人姓名"))?;
    let dept: Option<Uuid> = row
        .get_typed("dept_id")
        .map_err(|error| db_err(error, "读取报销人部门"))?;
    let dept = dept.ok_or_else(|| {
        invalid(format!(
            "报销人 {name} 没有归属部门：报销单的部门是预算维度，请先在用户资料里补部门"
        ))
    })?;
    Ok((name, dept))
}

/// 凭证号（详情页展示用）。
async fn voucher_no_of(id: Option<Uuid>) -> Option<String> {
    let id = id?;
    match voucher::find_voucher(db(), id).await {
        Ok(Some(row)) => Some(row.voucher_no),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%id, %error, "查询凭证号失败");
            None
        }
    }
}

// ==================== ① 费用类型 ====================

#[nik::permission(
    path = "/business/erp/expense-type/page",
    method = "GET",
    permission = "business:erp:expense-type:list"
)]
#[nik::get("/business/erp/expense-type/page")]
pub async fn type_page(
    Extension(AuthClaims(_claims)): Extension<AuthClaims>,
    Query(filter): Query<TypeFilter>,
) -> ResResult<PageData<Vec<TypeRes>>> {
    let (page, page_size) = normalize_page(
        filter.page.unwrap_or(1),
        filter.page_size.unwrap_or(expense_dto::DEFAULT_PAGE_SIZE),
    );
    let (rows, total) = expense_model::type_page(&filter, page, page_size)
        .await
        .map_err(|error| db_err(error, "分页查询费用类型"))?;
    let list: Vec<TypeRes> = rows.iter().map(type_res).collect();
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    ))
}

#[nik::permission(
    path = "/business/erp/expense-type/list",
    method = "GET",
    permission = "business:erp:expense-type:list"
)]
#[nik::get("/business/erp/expense-type/list")]
pub async fn type_list(
    Extension(AuthClaims(_claims)): Extension<AuthClaims>,
) -> ResResult<Vec<TypeRes>> {
    let rows = expense_model::type_active()
        .await
        .map_err(|error| db_err(error, "查询费用类型"))?;
    Ok(Res::ok(rows.iter().map(type_res).collect()))
}

fn type_res(row: &TypeRow) -> TypeRes {
    TypeRes {
        id: row.id.to_string(),
        type_code: row.type_code.clone(),
        type_name: row.type_name.clone(),
        account_id: row.account_id.to_string(),
        account_code: row.account_code.clone(),
        account_name: row.account_name.clone(),
        tax_deductible: row.tax_deductible,
        budget_controlled: row.budget_controlled,
        need_invoice_no: row.need_invoice_no,
        amount_limit: row.amount_limit,
        sort_order: row.sort_order,
        status: row.status,
        remark: row.remark.clone(),
    }
}

#[nik::permission(
    path = "/business/erp/expense-type",
    method = "POST",
    permission = "business:erp:expense-type:create"
)]
#[nik::post("/business/erp/expense-type")]
pub async fn type_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<TypeReq>,
) -> ResResult<TypeRes> {
    let spec = expense_dto::validate_type(&req, true)?;
    let operator = operator_of(&claims);
    if expense_model::type_find_by_code(db(), &spec.type_code)
        .await
        .map_err(|error| db_err(error, "查询费用类型编码"))?
        .is_some()
    {
        return Err(conflict(format!(
            "费用类型编码 {} 已存在（同编码只能有一个未删除的类型）",
            spec.type_code
        )));
    }
    let account = resolve_account(spec.account_id).await?;
    let id = Uuid::now_v7();
    expense_model::type_insert(db(), id, &spec, &account.code, operator)
        .await
        .map_err(|error| db_err(error, "新建费用类型"))?;
    let row = expense_model::type_find(id)
        .await
        .map_err(|error| db_err(error, "读回费用类型"))?
        .ok_or_else(type_not_found)?;
    Ok(Res::ok(type_res(&row)))
}

#[nik::permission(
    path = "/business/erp/expense-type/{id}",
    method = "PUT",
    permission = "business:erp:expense-type:update"
)]
#[nik::put("/business/erp/expense-type/{id}")]
pub async fn type_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TypeReq>,
) -> ResResult<TypeRes> {
    let id = parse_uuid_text(&id, "id")?;
    let spec = expense_dto::validate_type(&req, false)?;
    let operator = operator_of(&claims);
    let existing = expense_model::type_find(id)
        .await
        .map_err(|error| db_err(error, "查询费用类型"))?
        .ok_or_else(type_not_found)?;
    let account = resolve_account(spec.account_id).await?;
    let affected = expense_model::type_update(db(), id, &spec, &account.code, operator)
        .await
        .map_err(|error| db_err(error, "修改费用类型"))?;
    if affected == 0 {
        return Err(type_not_found());
    }
    tracing::info!(%id, from = %existing.type_code, to = %account.code, "费用类型已修改");
    let row = expense_model::type_find(id)
        .await
        .map_err(|error| db_err(error, "读回费用类型"))?
        .ok_or_else(type_not_found)?;
    Ok(Res::ok(type_res(&row)))
}

#[nik::permission(
    path = "/business/erp/expense-type/{id}",
    method = "DELETE",
    permission = "business:erp:expense-type:delete"
)]
#[nik::delete("/business/erp/expense-type/{id}")]
pub async fn type_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<expense_dto::IdRes> {
    let id = parse_uuid_text(&id, "id")?;
    let operator = operator_of(&claims);
    let row = expense_model::type_find(id)
        .await
        .map_err(|error| db_err(error, "查询费用类型"))?
        .ok_or_else(type_not_found)?;
    let used = expense_model::type_used_count(db(), id)
        .await
        .map_err(|error| db_err(error, "统计费用类型引用"))?;
    if used > 0 {
        return Err(conflict(format!(
            "费用类型 {} 已被 {used} 张报销单引用，不能删除（可以停用：status = 0）",
            row.type_code
        )));
    }
    expense_model::type_soft_delete(db(), id, operator)
        .await
        .map_err(|error| db_err(error, "删除费用类型"))?;
    Ok(Res::ok(expense_dto::IdRes { id: id.to_string() }))
}

/// 解析费用类型要用的借方科目（必须末级 + 启用 + 方向为借）。
async fn resolve_account(account_id: Uuid) -> Result<voucher::AccountRef, ApiError> {
    if account_id.is_nil() {
        return Err(invalid("借方费用科目 account_id 必填（末级科目）"));
    }
    let account = voucher_multi::ensure_leaf(db(), account_id)
        .await
        .map_err(|error| db_err(error, "校验借方科目"))?;
    // SCOPE-EXEMPT: 科目按主键取 1 行。
    let rows = db()
        .query_sql(
            "SELECT balance_direction, account_category FROM biz_account \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&account_id)],
        )
        .await
        .map_err(|error| db_err(error, "查询科目方向"))?;
    let Some(row) = rows.first() else {
        return Err(invalid(format!("科目 {} 不存在或已删除", account.code)));
    };
    let direction: String = row
        .get_typed("balance_direction")
        .map_err(|error| db_err(error, "读取科目方向"))?;
    if direction != "DEBIT" {
        return Err(invalid(format!(
            "科目 {}（{}）的余额方向是 {direction}：费用类型的借方必须是借方科目",
            account.code, account.name
        )));
    }
    Ok(account)
}

// ==================== ② 报销单 ====================

#[nik::permission(
    path = "/business/erp/expense/page",
    method = "GET",
    permission = "business:erp:expense:list"
)]
#[nik::get("/business/erp/expense/page")]
pub async fn report_page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(filter): Query<ReportFilter>,
) -> ResResult<PageData<Vec<ReportRes>>> {
    let (page, page_size) = normalize_page(
        filter.page.unwrap_or(1),
        filter.page_size.unwrap_or(expense_dto::DEFAULT_PAGE_SIZE),
    );
    let principal = crate::business::shared::principal::resolve_for_claims(&claims).await?;
    let (rows, total) = expense_model::report_page(&filter, page, page_size, &principal)
        .await
        .map_err(|error| db_err(error, "分页查询报销单"))?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(report_res(row).await);
    }
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    ))
}

#[nik::permission(
    path = "/business/erp/expense/pay/page",
    method = "GET",
    permission = "business:erp:expense:pay"
)]
#[nik::get("/business/erp/expense/pay/page")]
pub async fn report_pay_page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(filter): Query<ReportFilter>,
) -> ResResult<PageData<Vec<ReportRes>>> {
    let (page, page_size) = normalize_page(
        filter.page.unwrap_or(1),
        filter.page_size.unwrap_or(expense_dto::DEFAULT_PAGE_SIZE),
    );
    let principal = crate::business::shared::principal::resolve_for_claims(&claims).await?;
    let (rows, total) = expense_model::report_payable_page(page, page_size, &principal)
        .await
        .map_err(|error| db_err(error, "分页查询待付款报销单"))?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(report_res(row).await);
    }
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    ))
}

#[nik::permission(
    path = "/business/erp/expense/{id}",
    method = "GET",
    permission = "business:erp:expense:detail"
)]
#[nik::get("/business/erp/expense/{id}")]
pub async fn report_detail(Path(id): Path<String>) -> ResResult<ReportDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 详情装配（建单 / 改单 / 提交后也走它，前端只拉一次）。
pub async fn detail_of(id: Uuid) -> Result<ReportDetailRes, ApiError> {
    let row = expense_model::report_find(id)
        .await
        .map_err(|error| db_err(error, "查询报销单"))?
        .ok_or_else(not_found)?;
    let items = expense_model::items_of(db(), id)
        .await
        .map_err(|error| db_err(error, "查询报销明细"))?;
    let payments = expense_model::payments_of(db(), id)
        .await
        .map_err(|error| db_err(error, "查询付款记录"))?;
    let mut payment_list = Vec::with_capacity(payments.len());
    for payment in &payments {
        payment_list.push(PaymentRes {
            id: payment.id.to_string(),
            payment_no: payment.payment_no.clone(),
            amount: payment.amount,
            pay_date: payment.pay_date,
            pay_method: payment.pay_method.clone(),
            pay_account_code: payment.pay_account_code.clone(),
            voucher_id: payment.voucher_id.map(|id| id.to_string()),
            voucher_no: voucher_no_of(payment.voucher_id).await,
            period_code: payment.period_code.clone(),
            status: payment.status.clone(),
            remark: payment.remark.clone(),
        });
    }
    let usages = budget_guard::usages_of(db(), budget_guard::SOURCE_EXPENSE, id)
        .await
        .map_err(|error| db_err(error, "查询预算占用"))?;
    let mut usage_list = Vec::with_capacity(usages.len());
    for usage in &usages {
        let account_code = account_code_of(usage.account_id).await.unwrap_or_default();
        usage_list.push(UsageRes {
            id: usage.id.to_string(),
            dept_id: usage.dept_id.to_string(),
            account_id: usage.account_id.to_string(),
            account_code,
            fiscal_year: usage.fiscal_year,
            period_code: usage.period_code.clone(),
            amount: usage.amount,
            status: usage.status.clone(),
            over_budget: usage.over_budget,
            over_amount: usage.over_amount,
            release_reason: usage.release_reason.clone(),
        });
    }
    Ok(ReportDetailRes {
        tier_label: expense_dto::tier_label(row.total_with_tax).to_string(),
        report: report_res(&row).await,
        items: items.iter().map(item_res).collect(),
        payments: payment_list,
        budget_usages: usage_list,
    })
}

async fn account_code_of(account_id: Uuid) -> Option<String> {
    // SCOPE-EXEMPT: 科目按主键取 1 行。
    let rows = db()
        .query_sql(
            "SELECT account_code FROM biz_account WHERE id = ?::uuid".to_string(),
            vec![Value::from(&account_id)],
        )
        .await
        .ok()?;
    rows.first()?.get_typed("account_code").ok()
}

async fn report_res(row: &ReportRow) -> ReportRes {
    ReportRes {
        id: row.id.to_string(),
        report_no: row.report_no.clone(),
        reporter_id: row.reporter_id.to_string(),
        reporter_name: row.reporter_name.clone(),
        dept_id: row.dept_id.to_string(),
        dept_name: row.dept_name.clone(),
        title: row.title.clone(),
        expense_date: row.expense_date,
        period_code: row.period_code.clone(),
        total_amount: row.total_amount,
        total_tax: row.total_tax,
        total_with_tax: row.total_with_tax,
        item_count: row.item_count,
        status: row.status.clone(),
        status_label: status_label(&row.status).to_string(),
        pay_status: row.pay_status.clone(),
        pay_status_label: expense_dto::pay_status_label(&row.pay_status).to_string(),
        paid_amount: row.paid_amount,
        unpaid_amount: row.unpaid_amount,
        last_pay_date: row.last_pay_date,
        over_budget: row.over_budget,
        over_amount: row.over_amount,
        approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
        voucher_id: row.voucher_id.map(|id| id.to_string()),
        voucher_no: voucher_no_of(row.voucher_id).await,
        submitted_at: row.submitted_at,
        approved_at: row.approved_at,
        cancel_reason: row.cancel_reason.clone(),
        remark: row.remark.clone(),
        created_at: row.created_at,
    }
}

fn item_res(row: &expense_model::ItemRow) -> ItemRes {
    ItemRes {
        id: row.id.to_string(),
        line_no: row.line_no,
        expense_date: row.expense_date,
        expense_type_id: row.expense_type_id.to_string(),
        type_code: row.type_code.clone(),
        type_name: row.type_name.clone(),
        account_id: row.account_id.to_string(),
        account_code: row.account_code.clone(),
        account_name: row.account_name.clone(),
        amount: row.amount,
        tax_rate: row.tax_rate,
        tax_amount: row.tax_amount,
        with_tax_amount: row.with_tax_amount,
        invoice_no: row.invoice_no.clone(),
        invoice_date: row.invoice_date,
        invoice_type: row.invoice_type.clone(),
        summary: row.summary.clone(),
        remark: row.remark.clone(),
    }
}

#[nik::permission(
    path = "/business/erp/expense",
    method = "POST",
    permission = "business:erp:expense:create"
)]
#[nik::post("/business/erp/expense")]
pub async fn report_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<ReportReq>,
) -> ResResult<ReportDetailRes> {
    let Some(operator) = operator_of(&claims) else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "建报销单需要登录用户（令牌里没有用户 id）",
        ));
    };
    let spec = expense_dto::validate_report(&req)?;
    let insert = build_insert(&spec, operator).await?;
    let id = insert.id;
    let tx = db()
        .begin()
        .await
        .map_err(|error| db_err(error, "开启建单事务"))?;
    let outcome = async {
        expense_model::report_insert(&tx, &insert, Some(operator))
            .await
            .map_err(|error| db_err(error, "写入报销单"))?;
        expense_model::items_insert_many(&tx, id, &insert.items)
            .await
            .map_err(|error| db_err(error, "写入报销明细"))?;
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => tx
            .commit()
            .await
            .map_err(|error| db_err(error, "提交建单事务"))?,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    }
    tracing::info!(%id, report_no = %insert.report_no, "报销单已创建（草稿）");
    Ok(Res::ok(detail_of(id).await?))
}

/// 把校验后的契约变成写库载荷（类型快照 / 金额拆分 / 单号）。
async fn build_insert(
    spec: &expense_dto::ReportSpec,
    operator: Uuid,
) -> Result<ReportInsert, ApiError> {
    let reporter_id = spec.reporter_id.unwrap_or(operator);
    let (reporter_name, dept_from_user) = reporter_profile(reporter_id).await?;
    let dept_id = spec.dept_id.unwrap_or(dept_from_user);
    let dept_name = dept_name_of(dept_id).await?;
    let id = Uuid::now_v7();
    let report_no = voucher::next_doc_no(db(), &expense_model::SEQ, Utc::now())
        .await
        .map_err(|error| db_err(error, "生成报销单号"))?;
    let mut items = Vec::with_capacity(spec.items.len());
    let mut splits = Vec::with_capacity(spec.items.len());
    for (index, item) in spec.items.iter().enumerate() {
        let no = index + 1;
        let kind = expense_model::type_find(item.expense_type_id)
            .await
            .map_err(|error| db_err(error, "查询费用类型"))?
            .ok_or_else(|| {
                invalid(format!(
                    "第 {no} 行明细的费用类型 {} 不存在或已删除",
                    item.expense_type_id
                ))
            })?;
        if kind.status != 1 {
            return Err(invalid(format!(
                "第 {no} 行明细的费用类型「{}」已停用：请换一个类型或让管理员启用",
                kind.type_name
            )));
        }
        if kind.need_invoice_no && item.invoice_no.is_none() {
            return Err(invalid(format!(
                "第 {no} 行明细的费用类型「{}」要求填写发票号（invoice_no）",
                kind.type_name
            )));
        }
        if kind.amount_limit > Decimal::ZERO && item.with_tax > kind.amount_limit {
            return Err(invalid(format!(
                "第 {no} 行明细金额 {} 超过费用类型「{}」的单笔上限 {}",
                item.with_tax, kind.type_name, kind.amount_limit
            )));
        }
        let split = item.split(kind.tax_deductible);
        splits.push(split);
        items.push(ItemInsert {
            line_no: i32::try_from(no).unwrap_or(1),
            expense_date: item.expense_date,
            expense_type_id: kind.id,
            type_code: kind.type_code.clone(),
            type_name: kind.type_name.clone(),
            account_id: kind.account_id,
            account_code: kind.account_code.clone(),
            account_name: kind.account_name.clone(),
            amount: split.amount,
            tax_rate: item.tax_rate,
            tax_amount: split.tax,
            with_tax_amount: split.with_tax,
            invoice_no: item.invoice_no.clone(),
            invoice_date: item.invoice_date,
            invoice_type: item.invoice_type.clone(),
            summary: item.summary.clone(),
            remark: item.remark.clone(),
        });
    }
    let summary = Summary::of(&splits);
    summary.check().map_err(|reason| {
        ApiError::new(
            422,
            error_code::ERP_EXPENSE_TOTAL_MISMATCH,
            format!("报销单金额校验失败：{reason}"),
        )
    })?;
    Ok(ReportInsert {
        id,
        report_no,
        reporter_id,
        reporter_name,
        dept_id,
        dept_name,
        title: spec.title.clone(),
        expense_date: spec.expense_date,
        period_code: expense_compute::period_of(spec.expense_date),
        total_amount: summary.total_amount,
        total_tax: summary.total_tax,
        total_with_tax: summary.total_with_tax,
        item_count: summary.item_count,
        remark: spec.remark.clone(),
        items,
    })
}

#[nik::permission(
    path = "/business/erp/expense/{id}",
    method = "PUT",
    permission = "business:erp:expense:update"
)]
#[nik::put("/business/erp/expense/{id}")]
pub async fn report_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<ReportReq>,
) -> ResResult<ReportDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let Some(operator) = operator_of(&claims) else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "改报销单需要登录用户",
        ));
    };
    let spec = expense_dto::validate_report(&req)?;
    let existing = expense_model::report_find(id)
        .await
        .map_err(|error| db_err(error, "查询报销单"))?
        .ok_or_else(not_found)?;
    if existing.status != expense_dto::STATUS_DRAFT {
        return Err(ApiError::new(
            409,
            error_code::ERP_EXPENSE_STATE,
            format!(
                "报销单 {} 当前是「{}」：只有草稿能改（提交后要改请先撤回或让审批人驳回）",
                existing.report_no,
                status_label(&existing.status)
            ),
        ));
    }
    let mut insert = build_insert(&spec, operator).await?;
    // 改单沿用原单号与主键（单号是审计锚点，不许换）
    insert.id = existing.id;
    insert.report_no = existing.report_no.clone();
    let tx = db()
        .begin()
        .await
        .map_err(|error| db_err(error, "开启改单事务"))?;
    let outcome = async {
        let affected = expense_model::report_update_draft(&tx, id, &insert, Some(operator))
            .await
            .map_err(|error| db_err(error, "更新报销单"))?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::ERP_EXPENSE_STATE,
                format!("报销单 {} 已不是草稿，请刷新后重试", insert.report_no),
            ));
        }
        expense_model::items_delete_of(&tx, id)
            .await
            .map_err(|error| db_err(error, "清理旧明细"))?;
        expense_model::items_insert_many(&tx, id, &insert.items)
            .await
            .map_err(|error| db_err(error, "写入新明细"))?;
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => tx
            .commit()
            .await
            .map_err(|error| db_err(error, "提交改单事务"))?,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    }
    tracing::info!(%id, report_no = %insert.report_no, "报销单草稿已更新（明细整体重建）");
    Ok(Res::ok(detail_of(id).await?))
}

#[nik::permission(
    path = "/business/erp/expense/{id}",
    method = "DELETE",
    permission = "business:erp:expense:delete"
)]
#[nik::delete("/business/erp/expense/{id}")]
pub async fn report_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<expense_dto::IdRes> {
    let id = parse_uuid_text(&id, "id")?;
    let operator = operator_of(&claims);
    let row = expense_model::report_find(id)
        .await
        .map_err(|error| db_err(error, "查询报销单"))?
        .ok_or_else(not_found)?;
    if row.status != expense_dto::STATUS_DRAFT {
        return Err(ApiError::new(
            409,
            error_code::ERP_EXPENSE_STATE,
            format!(
                "报销单 {} 当前是「{}」：只有草稿能删除（已提交的请先撤回，已入账的请走作废）",
                row.report_no,
                status_label(&row.status)
            ),
        ));
    }
    expense_model::report_soft_delete(db(), id, operator)
        .await
        .map_err(|error| db_err(error, "删除报销单"))?;
    Ok(Res::ok(expense_dto::IdRes { id: id.to_string() }))
}

// ==================== ③ 提交 / 撤回 / 作废 ====================

#[nik::permission(
    path = "/business/erp/expense/{id}/submit",
    method = "POST",
    permission = "business:erp:expense:submit"
)]
#[nik::post("/business/erp/expense/{id}/submit")]
pub async fn report_submit(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<ReportDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let Some(operator) = operator_of(&claims) else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交报销需要登录用户",
        ));
    };
    let row = expense_model::report_find(id)
        .await
        .map_err(|error| db_err(error, "查询报销单"))?
        .ok_or_else(not_found)?;
    if row.status != expense_dto::STATUS_DRAFT {
        return Err(ApiError::new(
            409,
            error_code::ERP_EXPENSE_STATE,
            format!(
                "报销单 {} 当前是「{}」：只有草稿能提交",
                row.report_no,
                status_label(&row.status)
            ),
        ));
    }
    let flow = expense_bridge::published_expense_flow()
        .await
        .map_err(|error| db_err(error, "查询报销审批流程"))?
        .ok_or_else(|| {
            ApiError::new(
                409,
                error_code::APPROVAL_FLOW_NOT_CONFIGURED,
                format!(
                    "报销单 {} 的提交必须走审批，但系统里没有已发布的 expense 流程，所以不能提交。\
                     请管理员在「审批流程」里发布费用报销审批流程（expense）：\
                     直属上级 → 按金额分级（< 1000 结束 / 1000~10000 财务经理 / ≥ 10000 会签）。",
                    row.report_no
                ),
            )
        })?;
    if flow_model::biz_instance_running(BIZ_TYPE, id)
        .await
        .map_err(|error| db_err(error, "查询在跑报销审批"))?
    {
        return Err(ApiError::new(
            409,
            error_code::ERP_EXPENSE_STATE,
            format!(
                "报销单 {} 已有在跑的审批：请等审批结束或先撤回",
                row.report_no
            ),
        ));
    }
    let items = expense_model::items_of(db(), id)
        .await
        .map_err(|error| db_err(error, "查询报销明细"))?;
    expense_bridge::check_totals(&row, &items)?;
    // 预算守卫（ARCH-6 §3.5.1 控制点①）：按 (部门, 科目) 聚合可受控明细
    let slot_items = expense_slot_amounts(&items).await?;
    let budget = budget_guard::check_for_expense(db(), row.dept_id, &row.period_code, &slot_items)
        .await
        .map_err(|error| db_err(error, "预算校验"))?;
    if budget.blocks() {
        return Err(ApiError::new(
            409,
            error_code::ERP_BUDGET_EXCEEDED,
            format!(
                "报销单 {} 超出预算额度（{}）：当前策略是 BLOCK，提交被拒绝。\
                 请联系财务调整预算，或让管理员把控制策略改为 WARN（标记 + 强制审批）。",
                row.report_no, budget.reason
            ),
        ));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator)
        .await
        .map_err(|error| db_err(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let form_data = submit_form_data(&row, &items, &budget)?;
    let start = engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(row.id),
        biz_no: Some(row.report_no.clone()),
        title: format!("{} {}", row.report_no, row.title)
            .chars()
            .take(200)
            .collect::<String>(),
        initiator_id: operator,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data,
    };
    let mut out = {
        let tx = db()
            .begin()
            .await
            .map_err(|error| db_err(error, "开启提交事务"))?;
        let outcome = async {
            let affected = expense_model::report_mark_submitted(
                &tx,
                id,
                instance_id,
                budget.over(),
                budget.over_amount(),
                Some(budget.mode.as_str()),
                operator,
                Utc::now(),
            )
            .await
            .map_err(|error| db_err(error, "标记报销单为审批中"))?;
            if affected == 0 {
                return Err(ApiError::new(
                    409,
                    error_code::ERP_EXPENSE_STATE,
                    format!("报销单 {} 已不是草稿，请刷新后重试", row.report_no),
                ));
            }
            budget_guard::occupy_for_expense(&tx, &budget, id, &row.report_no, Some(operator))
                .await
                .map_err(|error| db_err(error, "写预算占用"))?;
            state_machine::transition(
                &tx,
                state_machine::ENTITY_EXPENSE,
                id,
                &row.report_no,
                &state_machine::EXPENSE_EDGES,
                expense_dto::STATUS_DRAFT,
                expense_dto::STATUS_SUBMITTED,
                operator,
                None,
                None,
            )
            .await?;
            let out = engine::start(&tx, &start)
                .await
                .map_err(|error| db_err(error, "发起报销审批单"))?;
            Ok::<engine::ActionOut, ApiError>(out)
        }
        .await;
        match outcome {
            Ok(out) => {
                tx.commit()
                    .await
                    .map_err(|error| db_err(error, "提交报销审批事务"))?;
                out
            }
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        }
    };
    if let Some(rejected) = out.rejected.take() {
        return Err(ApiError::new(rejected.status, rejected.code, rejected.msg));
    }
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    // 单站流程可能当场走完（金额 < 1000 且直属上级为空 → 兜底超管，仍是人工审批；
    // 这里只是防御：真的自动通过了也要立刻入账，不能停在 APPROVED 却没凭证）
    if expense_bridge::should_execute(&out.instance_status) {
        expense_bridge::on_expense_settled(out.instance_id, &out.instance_status).await;
    }
    tracing::info!(
        %id,
        report_no = %row.report_no,
        instance = %instance_id,
        over_budget = budget.over(),
        "报销单已提交审批（预算占用 {} 个槽）",
        budget.checks.len()
    );
    Ok(Res::ok(detail_of(id).await?))
}

/// 把受预算控制的明细按科目聚合成槽金额（`budget_controlled = 0` 的类型被排除）。
async fn expense_slot_amounts(
    items: &[expense_model::ItemRow],
) -> Result<Vec<budget_guard::ExpenseSlotAmount>, ApiError> {
    let mut out: Vec<budget_guard::ExpenseSlotAmount> = Vec::new();
    for item in items {
        let kind = expense_model::type_find(item.expense_type_id)
            .await
            .map_err(|error| db_err(error, "查询费用类型"))?;
        let controlled = kind
            .as_ref()
            .map(|row| row.budget_controlled)
            .unwrap_or(true);
        if !controlled {
            continue;
        }
        match out
            .iter_mut()
            .find(|slot| slot.account_id == item.account_id)
        {
            Some(slot) => {
                slot.amount = expense_compute::money(slot.amount + item.with_tax_amount);
                slot.amount_net = expense_compute::money(slot.amount_net + item.amount);
            }
            None => out.push(budget_guard::ExpenseSlotAmount {
                account_id: item.account_id,
                amount: item.with_tax_amount,
                amount_net: item.amount,
            }),
        }
    }
    Ok(out)
}

/// `form_data` 契约（ARCH-6 §4.4.3 的字段清单）。
///
/// ⚠️ **键名不带 `form.` 前缀**：流程引擎的条件求值把 `form_data` 整个对象当
/// `form` 作用域（`engine::eval_context`：`form = instance.form_data`），条件里写
/// `form.amount` 实际取的是 `form_data["amount"]`。设计 §4.4.3 的样例把键写成
/// `"form.amount"`，照抄会让 `< 1000` / `< 10000` 两条条件边**静默 Missing**、
/// 一律走兜底边（6,360 的单直接冲到总经理会签那档）—— 真机首跑踩到，
/// 与 `po_approve`（`form_data["amount"]`）的口径对齐，差异写进交付说明。
///
/// `amount` 必须是 **JSON 数字**（条件节点的 `cmp` 只认 `Number`，见
/// `payable_payment::amount_number` 的注释）。
fn submit_form_data(
    row: &ReportRow,
    items: &[expense_model::ItemRow],
    budget: &budget_guard::ExpenseBudget,
) -> Result<serde_json::Value, ApiError> {
    let mut type_names: Vec<&str> = items.iter().map(|item| item.type_name.as_str()).collect();
    type_names.sort_unstable();
    type_names.dedup();
    let amount_number = crate::business::erp::payable_payment::amount_number;
    Ok(json!({
        "amount": amount_number(row.total_with_tax),
        "report_no": row.report_no,
        "reporter_name": row.reporter_name,
        "dept_name": row.dept_name,
        "title": row.title,
        "item_count": row.item_count,
        "expense_types": type_names.join(","),
        "over_budget": budget.over(),
        "over_amount": amount_number(budget.over_amount()),
        "budget_mode": budget.mode.as_str(),
        "voucher_summary": format!("报销-{}", row.title),
    }))
}

#[nik::permission(
    path = "/business/erp/expense/{id}/withdraw",
    method = "POST",
    permission = "business:erp:expense:submit"
)]
#[nik::post("/business/erp/expense/{id}/withdraw")]
pub async fn report_withdraw(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<ReportDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let Some(operator) = operator_of(&claims) else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "撤回报销需要登录用户",
        ));
    };
    let row = expense_model::report_find(id)
        .await
        .map_err(|error| db_err(error, "查询报销单"))?
        .ok_or_else(not_found)?;
    if row.status != expense_dto::STATUS_SUBMITTED {
        return Err(ApiError::new(
            409,
            error_code::ERP_EXPENSE_STATE,
            format!(
                "报销单 {} 当前是「{}」：只有审批中的单能撤回",
                row.report_no,
                status_label(&row.status)
            ),
        ));
    }
    let Some(instance_id) = row.approval_instance_id else {
        return Err(ApiError::new(
            409,
            error_code::ERP_EXPENSE_STATE,
            format!(
                "报销单 {} 处于审批中但没有审批实例，请让管理员修数据",
                row.report_no
            ),
        ));
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| db_err(error, "开启撤回事务"))?;
    let outcome = async {
        let out = engine::withdraw(&tx, instance_id, operator)
            .await
            .map_err(|error| db_err(error, "撤回报销审批"))?;
        if let Some(rejected) = out.rejected {
            return Err(ApiError::new(rejected.status, rejected.code, rejected.msg));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => tx
            .commit()
            .await
            .map_err(|error| db_err(error, "提交撤回事务"))?,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    }
    // 业务侧回调（回草稿 + 释放预算占用）
    expense_bridge::on_expense_settled(Some(instance_id), "WITHDRAWN").await;
    tracing::info!(%id, report_no = %row.report_no, "报销单已撤回");
    Ok(Res::ok(detail_of(id).await?))
}

#[nik::permission(
    path = "/business/erp/expense/{id}/cancel",
    method = "POST",
    permission = "business:erp:expense:cancel"
)]
#[nik::post("/business/erp/expense/{id}/cancel")]
pub async fn report_cancel(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CancelReq>,
) -> ResResult<ReportDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let reason = expense_dto::validate_cancel(&req)?;
    let operator = operator_of(&claims);
    let row = expense_model::report_find(id)
        .await
        .map_err(|error| db_err(error, "查询报销单"))?
        .ok_or_else(not_found)?;
    // 三条硬规则（ARCH-6 §4.3.3）
    if row.status == expense_dto::STATUS_PAID {
        return Err(ApiError::new(
            409,
            error_code::ERP_EXPENSE_STATE,
            format!(
                "报销单 {} 已经付清（{}）：PAID 之后不允许作废 —— 钱已经付给员工了，\
                 要退钱是「员工退款」业务（v2），不是作废",
                row.report_no,
                status_label(&row.status)
            ),
        ));
    }
    if row.status == expense_dto::STATUS_CANCELLED {
        return Err(ApiError::new(
            409,
            error_code::ERP_EXPENSE_STATE,
            format!("报销单 {} 已经作废过了", row.report_no),
        ));
    }
    // 已经付过钱（哪怕只付了一部分）的单不能作废：钱出去了，账上没有「作废」这个动作
    if row.paid_amount > Decimal::ZERO {
        return Err(ApiError::new(
            409,
            error_code::ERP_EXPENSE_STATE,
            format!(
                "报销单 {} 已经付过款（已付 {}，未付 {}）：付过钱的单不能作废 —— \
                 请让员工走「退款」后按新单处理（ARCH-6 §4.3.3-3 的同一口径）",
                row.report_no, row.paid_amount, row.unpaid_amount
            ),
        ));
    }
    // SOD：制单人不能自己作废已提交 / 已入账的单（DRAFT 走删除，不受此限）
    if row.status != expense_dto::STATUS_DRAFT
        && row
            .create_by
            .is_some_and(|creator| Some(creator) == operator)
        && !claims.is_super_admin()
    {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            "内控要求：报销单的制单人不能自己作废已提交 / 已入账的单据（请让财务或另一位管理员操作）",
        ));
    }
    let from_status = row.status.clone();
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| db_err(error, "开启作废事务"))?;
    let outcome = async {
        let Some(current) = expense_model::report_lock_in(&tx, id)
            .await
            .map_err(|error| db_err(error, "锁定报销单"))?
        else {
            return Err(not_found());
        };
        if current.status != from_status {
            return Err(ApiError::new(
                409,
                error_code::ERP_EXPENSE_STATE,
                format!("报销单 {} 的状态已被改动，请刷新后重试", current.report_no),
            ));
        }
        // ① 审批中的单：先终止审批实例（否则孤儿实例会顶住唯一索引）
        if from_status == expense_dto::STATUS_SUBMITTED {
            expense_bridge::cancel_approval_of(&tx, &current).await?;
        }
        // ② 已入账的单：红冲计提凭证（同事务）
        let mut reds = Vec::new();
        if from_status == expense_dto::STATUS_APPROVED {
            reds = expense_bridge::reverse_report_vouchers(&tx, id, &reason, operator).await?;
        }
        // ③ 状态机流转（SUBMITTED / APPROVED → CANCELLED；DRAFT → CANCELLED）
        state_machine::transition(
            &tx,
            state_machine::ENTITY_EXPENSE,
            id,
            &current.report_no,
            &state_machine::EXPENSE_EDGES,
            &from_status,
            expense_dto::STATUS_CANCELLED,
            operator,
            Some(&reason),
            None,
        )
        .await?;
        // ④ 回写作废列（CAS 在 WHERE：并发点两次只有一次生效）
        let affected =
            expense_model::report_cancel_from(&tx, id, &from_status, &reason, operator, now)
                .await
                .map_err(|error| db_err(error, "回写报销单作废"))?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::ERP_EXPENSE_STATE,
                format!(
                    "报销单 {} 的状态在作废期间被改动，请刷新后重试",
                    current.report_no
                ),
            ));
        }
        // ⑤ 释放预算占用（已执行 / 占用中都释放；付款从不改占用）
        budget_guard::release(&tx, budget_guard::SOURCE_EXPENSE, id, "CANCELLED")
            .await
            .map_err(|error| db_err(error, "释放预算占用"))?;
        Ok::<usize, ApiError>(reds.len())
    }
    .await;
    let red_count = match outcome {
        Ok(count) => {
            tx.commit()
                .await
                .map_err(|error| db_err(error, "提交作废事务"))?;
            count
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    tracing::info!(
        %id,
        report_no = %row.report_no,
        from = %from_status,
        red_vouchers = red_count,
        "报销单已作废（红冲 {red_count} 张凭证，预算占用已释放）"
    );
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ④ 付款 ====================

#[nik::permission(
    path = "/business/erp/expense/{id}/pay",
    method = "POST",
    permission = "business:erp:expense:pay"
)]
#[nik::post("/business/erp/expense/{id}/pay")]
pub async fn report_pay(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<PayReq>,
) -> ResResult<ReportDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let operator = operator_of(&claims);
    let today = today_cst();
    let spec = expense_dto::validate_pay(&req, today)?;
    let row = expense_model::report_find(id)
        .await
        .map_err(|error| db_err(error, "查询报销单"))?
        .ok_or_else(not_found)?;
    if row.status != expense_dto::STATUS_APPROVED && row.status != expense_dto::STATUS_PAID {
        return Err(ApiError::new(
            409,
            error_code::ERP_EXPENSE_STATE,
            format!(
                "报销单 {} 当前是「{}」：只有已审批的单能付款（付款是登记式的，不改变费用归属）",
                row.report_no,
                status_label(&row.status)
            ),
        ));
    }
    let now = Utc::now();
    let pay_account = voucher_multi::leaf_required(db(), &spec.pay_account_code)
        .await
        .map_err(|error| {
            ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!("付款账户科目 {} 不可用：{error}", spec.pay_account_code),
            )
        })?;
    let tx = db()
        .begin()
        .await
        .map_err(|error| db_err(error, "开启付款事务"))?;
    let outcome = async {
        // ① 行锁报销单（付款是拒绝型操作，不锁会超额付款 —— ARCH-6 §4.8.3）
        let Some(current) = expense_model::report_lock_in(&tx, id)
            .await
            .map_err(|error| db_err(error, "锁定报销单"))?
        else {
            return Err(not_found());
        };
        if current.status != row.status || current.pay_status == expense_dto::PAY_PAID {
            return Err(ApiError::new(
                409,
                error_code::ERP_EXPENSE_STATE,
                format!(
                    "报销单 {} 的状态已被改动或已付清，请刷新后重试",
                    row.report_no
                ),
            ));
        }
        if spec.amount > current.unpaid_amount {
            return Err(invalid(format!(
                "本次付款 {} 超过未付金额 {}（报销单 {} 价税合计 {}，已付 {}）",
                spec.amount,
                current.unpaid_amount,
                current.report_no,
                current.total_with_tax,
                current.paid_amount
            )));
        }
        let payment_id = Uuid::now_v7();
        let payment_no = voucher::next_doc_no(&tx, &expense_model::PAY_SEQ, now)
            .await
            .map_err(|error| db_err(error, "生成付款单号"))?;
        let period_code = voucher::period_of(&tx, spec.pay_date)
            .await
            .map_err(|error| db_err(error, "解析付款会计期间"))?;
        // ② 付款凭证（期间闸在 post_voucher 里：已结账 → 66043）
        let voucher_id = expense_bridge::post_payment_voucher(
            &tx,
            &current,
            &payment_no,
            spec.amount,
            spec.pay_date,
            &spec.pay_account_code,
            now,
        )
        .await
        .map_err(|error| expense_bridge::voucher_error(error, "生成报销付款凭证", &payment_no))?;
        // ③ 付款记录
        expense_model::payment_insert(
            &tx,
            id,
            &PaymentInsert {
                id: payment_id,
                payment_no: payment_no.clone(),
                amount: spec.amount,
                pay_date: spec.pay_date,
                pay_method: spec.pay_method.clone(),
                pay_account_id: Some(pay_account.id),
                voucher_id: Some(voucher_id),
                period_code: period_code.clone(),
                remark: spec.remark.clone(),
            },
            operator,
        )
        .await
        .map_err(|error| db_err(error, "写入付款记录"))?;
        // ④ 回写报销单金额与状态
        let paid = expense_compute::money(current.paid_amount + spec.amount);
        let pay_status = expense_compute::pay_status_of(current.total_with_tax, paid);
        let new_status = expense_bridge::paid_status(pay_status, &current.status);
        let affected = expense_model::report_apply_payment(
            &tx,
            id,
            spec.amount,
            spec.pay_date,
            pay_status,
            new_status,
            operator,
        )
        .await
        .map_err(|error| db_err(error, "回写报销单付款金额"))?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::ERP_EXPENSE_STATE,
                format!(
                    "报销单 {} 的付款金额被并发修改，请刷新后重试",
                    current.report_no
                ),
            ));
        }
        // ⑤ 付清 → 状态机 APPROVED → PAID（部分付款不改状态，只改 pay_status）
        if new_status == expense_dto::STATUS_PAID {
            state_machine::transition_if_changed(
                &tx,
                state_machine::ENTITY_EXPENSE,
                id,
                &current.report_no,
                &state_machine::EXPENSE_EDGES,
                &current.status,
                expense_dto::STATUS_PAID,
                operator,
                Some("付款付清"),
                Some((expense_bridge::SOURCE_PAYMENT, payment_no.as_str())),
            )
            .await?;
        }
        Ok::<String, ApiError>(payment_no)
    }
    .await;
    let payment_no = match outcome {
        Ok(no) => {
            tx.commit()
                .await
                .map_err(|error| db_err(error, "提交付款事务"))?;
            no
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    tracing::info!(%id, report_no = %row.report_no, %payment_no, amount = %spec.amount, "报销付款已登记");
    Ok(Res::ok(detail_of(id).await?))
}
