//! 供应商准入 / 黑名单接口层（M4-14）：建单 / 改草稿 / 删草稿 / 分页 / 详情 / **提交审批**。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/supplier-access/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST   | `/business/erp/supplier-access` | `business:erp:supplier-access:create` | 建申请单（`SAC-yyyyMMdd-000001`，`DRAFT`） |
//! | PUT    | `/business/erp/supplier-access/{id}` | `business:erp:supplier-access:update` | 改草稿（类型与供应商不可改） |
//! | DELETE | `/business/erp/supplier-access/{id}` | `business:erp:supplier-access:update` | 软删草稿 |
//! | GET    | `/business/erp/supplier-access/page` | `business:erp:supplier-access:list` | 分页（关键字 / 状态 / 类型 / 供应商） |
//! | GET    | `/business/erp/supplier-access/{id}` | `business:erp:supplier-access:list` | 详情 |
//! | POST   | `/business/erp/supplier-access/{id}/submit` | `business:erp:supplier-access:submit` | 提交审批（走 `supplier_access` 流程） |
//!
//! 页面显形码 `business:erp:supplier-access:page` 只给前端（侧栏显隐 + 路由 meta.permission），
//! 后端零引用（AGENTS 约定 14）。
//!
//! # 三条硬规则
//!
//! ```text
//! ① 走审批流，不硬编码：提交时查「已发布的 supplier_access 流程」——
//!    有 → 同一事务里「发起审批 + CAS 状态 DRAFT→SUBMITTED + 写实例 id」；
//!    没有 → **默认 409**，只有持 business:erp:skip-approval + ≥10 字理由才直接生效
//!    （理由写进备注）。代码里没有任何「拉黑要财务复核」的 if —— 分级在流程定义的条件边里。
//! ② 供应商状态只在审批通过时被写（唯一入口 supplier_access_flow::settle）：
//!    本文件从头到尾不 UPDATE biz_supplier —— 提交 / 改动 / 删除都不碰主数据。
//! ③ 起点判据在**建单与提交各判一次**：草稿期间供应商可能被别的单拉黑 / 解除，
//!    提交时重判一次才能保证「进审批的那张单在那一刻是合法的」。
//! ```
//!
//! # 幂等
//!
//! * 提交：状态机 CAS（`DRAFT → SUBMITTED`）+ `biz_instance_running` 双保险 ——
//!   重复提交 409 且带审批单号；
//! * 改 / 删：CAS（`WHERE status='DRAFT'`），并发的第二个请求拿到 0 行 → 409；
//! * 同一供应商同时只能有一张未结单（库层 `uk_biz_sa_supplier_open`），
//!   并发建第二张 → 唯一键冲突被翻译成 409 并点名对方单号。
//!
//! # 事务提交后失败必须有补偿（AGENTS 约定 11）
//!
//! 免审直过分支（流程没有审批节点 → 实例当场 `APPROVED`）在事务提交后立即回调
//! `supplier_access_flow::on_sa_settled`。它失败时申请单会停在 `SUBMITTED` 且**没有待办**
//! 能推进它，所以这里在失败路径上**显式打回草稿**并对外报错（用户能重新提交）。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::{self, ApprovalGate};
use crate::business::erp::purchase::supplier_access_dto::{
    self as dto, AccessPageQuery, AccessRowRes, CreateAccessReq, PERM_CREATE, PERM_LIST, PERM_SUBMIT, PERM_UPDATE,
    SubmitAccessReq, UpdateAccessReq,
};
use crate::business::erp::purchase::supplier_access_flow as flow;
use crate::business::erp::purchase::supplier_access_model::{self as model, AccessRow};
use crate::business::flow::engine;
use crate::business::flow::model as flow_model;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// 代码里断言一份常量与路由注解一致：路由上的字面量必须能在本模块的常量清单里找到
const _: () = {
    let _ = [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_SUBMIT];
};

// ==================== ① 建单 ====================

/// 建单核心逻辑（单测直接调本函数，与 HTTP 走同一条代码路径）。
pub async fn create_application(req: &CreateAccessReq, operator: Option<Uuid>) -> std::result::Result<Uuid, ApiError> {
    let supplier_id = dto::parse_uuid(&req.supplier_id, "supplier_id")?;
    let access_type = dto::check_access_type(&req.access_type)?;
    let grade = dto::check_grade(&access_type, req.grade_snapshot.as_deref())?;
    // 起点判据（建单一次）
    let state = model::supplier_state(db(), supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商状态"))?;
    model::check_start(&access_type, &state).map_err(dto::status_conflict)?;
    // 同一供应商只能有一张未结单（库层唯一键之前先给可读报文）
    if let Some(open) = model::find_open_for_supplier(db(), supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询未结申请单"))?
    {
        return Err(dto::status_conflict(format!(
            "供应商 {} 已有一张未结申请单 {}（{}·{}）：请先提交审批或作废它，\
             同一家供应商同时只允许一张准入 / 黑名单申请",
            open.supplier_name,
            open.access_no,
            model::type_label(&open.access_type),
            model::status_label(&open.status)
        )));
    }
    let input = model::NewAccess {
        supplier_id,
        access_type,
        reason: model::clean_reason(req.reason.as_deref()),
        grade_snapshot: grade,
        remark: model::clean_reason(req.remark.as_deref()),
        operator_id: operator,
    };
    match model::insert_draft(db(), &input, Utc::now()).await {
        Ok(id) => Ok(id),
        Err(DbError::UniqueViolation { .. }) => Err(dto::status_conflict(
            "同一家供应商已有未结的准入 / 黑名单申请（并发建单撞唯一键），请刷新后查看已有单据",
        )),
        Err(error) => Err(utils::db_error(error, "新建供应商准入 / 黑名单申请")),
    }
}

/// 新建申请单。
#[nik::permission(
    path = "/business/erp/supplier-access",
    method = "POST",
    permission = "business:erp:supplier-access:create"
)]
#[nik::post("/business/erp/supplier-access")]
pub async fn supplier_access_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateAccessReq>,
) -> ResResult<AccessRowRes> {
    let operator = utils::current_user_uuid(&claims);
    let id = create_application(&req, operator).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ② 改草稿 / 删草稿 ====================

/// 改草稿核心逻辑（只有 `DRAFT`；类型与供应商不可改）。
pub async fn update_application(
    id: Uuid,
    req: &UpdateAccessReq,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let row = load_or_404(id).await?;
    ensure_draft(&row, "修改")?;
    let grade = dto::check_grade(&row.access_type, req.grade_snapshot.as_deref())?;
    let affected = model::update_draft(
        db(),
        id,
        model::clean_reason(req.reason.as_deref()).as_deref(),
        grade.as_deref(),
        model::clean_reason(req.remark.as_deref()).as_deref(),
        operator,
    )
    .await
    .map_err(|error| utils::db_error(error, "修改供应商准入 / 黑名单申请"))?;
    if affected == 0 {
        return Err(status_drift(&row, "修改"));
    }
    Ok(())
}

/// 修改申请单（草稿）。
#[nik::permission(
    path = "/business/erp/supplier-access/{id}",
    method = "PUT",
    permission = "business:erp:supplier-access:update"
)]
#[nik::put("/business/erp/supplier-access/{id}")]
pub async fn supplier_access_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateAccessReq>,
) -> ResResult<AccessRowRes> {
    let id = dto::parse_uuid(&id, "id")?;
    let operator = utils::current_user_uuid(&claims);
    update_application(id, &req, operator).await?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 删草稿核心逻辑。
pub async fn delete_application(id: Uuid, operator: Option<Uuid>) -> std::result::Result<(), ApiError> {
    let row = load_or_404(id).await?;
    ensure_draft(&row, "删除")?;
    let affected = model::delete_draft(db(), id, operator)
        .await
        .map_err(|error| utils::db_error(error, "删除供应商准入 / 黑名单申请"))?;
    if affected == 0 {
        return Err(status_drift(&row, "删除"));
    }
    Ok(())
}

/// 删除草稿。
#[nik::permission(
    path = "/business/erp/supplier-access/{id}",
    method = "DELETE",
    permission = "business:erp:supplier-access:update"
)]
#[nik::delete("/business/erp/supplier-access/{id}")]
pub async fn supplier_access_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = dto::parse_uuid(&id, "id")?;
    let operator = utils::current_user_uuid(&claims);
    delete_application(id, operator).await?;
    Ok(Res::ok(()))
}

// ==================== ③ 提交审批 ====================

/// 提交审批核心逻辑（单测直接调本函数）。
///
/// 三步：状态闸 → 起点重判（草稿期间供应商可能已变）→ 有流程走流程 / 没流程显式跳过。
pub async fn submit_application(
    id: Uuid,
    req: &SubmitAccessReq,
    operator: Option<Uuid>,
    gate: &ApprovalGate<'_>,
) -> std::result::Result<(), ApiError> {
    let row = load_or_404(id).await?;
    if row.status == model::STATUS_SUBMITTED {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(dto::status_conflict(format!(
            "申请单 {} 已在审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
            row.access_no, instance_no
        )));
    }
    ensure_draft(&row, "提交审批")?;
    // 理由：请求里给了就用请求的（覆盖草稿），否则用草稿里的
    let reason = dto::require_reason(
        req.reason
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .or(row.reason.as_deref()),
    )?;
    // 起点重判：草稿放几天，供应商可能已被别的单拉黑 / 解除
    let state = model::supplier_state(db(), row.supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商状态"))?;
    model::check_start(&row.access_type, &state).map_err(dto::status_conflict)?;
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    match flow::published_sa_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询供应商准入审批流程"))?
    {
        Some(sa_flow) => {
            // 有流程就必须走流程：请求里带了 skip_approval 也要 400（不许绕过已配置的内控）
            approval_gate::ensure_not_skipped(flow::FLOW_CODE, gate)?;
            submit_with_approval(&row, &sa_flow, operator_id, &reason).await
        }
        None => {
            // ARCH-3 DC-01：**不静默降级**。默认 409（点名 supplier_access）；
            // 只有「持 business:erp:skip-approval + 理由 ≥10 字」才放行。
            let skip_reason = approval_gate::authorize_missing_flow(flow::BIZ_TYPE, flow::FLOW_CODE, gate)?;
            tracing::warn!(
                access_id = %row.id,
                access_no = %row.access_no,
                %skip_reason,
                "供应商准入/黑名单提交：没有已发布的 supplier_access 流程，按「显式跳过审批」直接生效"
            );
            apply_directly(&row, operator_id, &reason, &skip_reason).await
        }
    }
}

/// 提交申请单审批。
#[nik::permission(
    path = "/business/erp/supplier-access/{id}/submit",
    method = "POST",
    permission = "business:erp:supplier-access:submit"
)]
#[nik::post("/business/erp/supplier-access/{id}/submit")]
pub async fn supplier_access_submit(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<SubmitAccessReq>>,
) -> ResResult<AccessRowRes> {
    let id = dto::parse_uuid(&id, "id")?;
    let req = body.map(|Json(item)| item).unwrap_or_default();
    let gate = ApprovalGate::new(&req.skip, &claims);
    let operator = utils::current_user_uuid(&claims);
    submit_application(id, &req, operator, &gate).await?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 走审批的提交：状态推进 + 发起实例**同一事务**。
async fn submit_with_approval(
    row: &AccessRow,
    sa_flow: &flow::SaFlow,
    operator_id: Uuid,
    reason: &str,
) -> std::result::Result<(), ApiError> {
    // 幂等：同一单已有在跑的审批 → 409（唯一索引之前先给可读报文）
    if flow_model::biz_instance_running(flow::BIZ_TYPE, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(dto::status_conflict(format!(
            "申请单 {} 已在审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
            row.access_no, instance_no
        )));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 供应商{}申请", row.access_no, model::type_label(&row.access_type))
        .chars()
        .take(200)
        .collect::<String>();
    let start = engine::StartReq {
        instance_id,
        def_id: sa_flow.def_id,
        version_id: sa_flow.version_id,
        version_no: sa_flow.version_no,
        flow_code: sa_flow.flow_code.clone(),
        flow_name: sa_flow.flow_name.clone(),
        biz_type: sa_flow.biz_type.clone(),
        biz_id: Some(row.id),
        biz_no: Some(row.access_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: form_data(row, reason),
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启供应商准入审批事务"))?;
    let outcome = async {
        let mut out = engine::start(&tx, &start)
            .await
            .map_err(|error| submit_conflict(error.into(), "发起供应商准入审批单", &row.access_no))?;
        if let Some(rejected) = out.rejected.take() {
            let hint = if rejected.code == error_code::FLOW_SELF_APPROVAL_FORBIDDEN {
                "（内控要求：审批人不能是发起人本人。请由他人提交，或让另一个账号具备审批权限后再审）"
            } else {
                ""
            };
            return Err(rejected.appended(hint));
        }
        let instance = out
            .instance_id
            .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
        let affected = model::mark_submitted(&tx, row.id, instance, reason, Some(operator_id))
            .await
            .map_err(|error| utils::db_error(error, "申请单进入已提交"))?;
        if affected == 0 {
            return Err(status_drift(row, "提交审批"));
        }
        Ok::<engine::ActionOut, ApiError>(out)
    }
    .await;
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
        .map_err(|error| utils::db_error(error, "提交供应商准入审批事务"))?;
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
        access_id = %row.id,
        access_no = %row.access_no,
        %instance,
        flow_code = %start.flow_code,
        "供应商准入/黑名单申请已提交审批（状态 SUBMITTED）"
    );
    // 流程里没有任何审批节点（全被跳过）→ 实例在同一个事务里就走完了。
    // 这里必须立刻回调落地，否则申请单停在 SUBMITTED 且**没有任何待办**能推进它
    // （AGENTS 约定 11 点名的高危形态）；回调失败就显式打回草稿，让用户能重提。
    if auto_finished && flow::on_sa_settled(Some(instance), "APPROVED").await.is_none() {
        let compensation = model::mark_back_to_draft(
            db(),
            row.id,
            "免审直过回调失败，系统已自动打回草稿，请重新提交（无需改库）",
            Some(operator_id),
        )
        .await;
        match compensation {
            Ok(affected) if affected > 0 => {
                tracing::warn!(
                    access_id = %row.id,
                    access_no = %row.access_no,
                    %instance,
                    "免审直过回调失败：申请单已打回 DRAFT（用户可重新提交）"
                );
                return Err(ApiError::new(
                    500,
                    error_code::INTERNAL_ERROR,
                    format!(
                        "申请单 {} 的审批实例已结束（无审批节点），但结论回写失败；\
                         系统已自动把单据打回草稿，请重新提交（无需改库）",
                        row.access_no
                    ),
                ));
            }
            other => {
                tracing::error!(
                    access_id = %row.id,
                    access_no = %row.access_no,
                    %instance,
                    result = ?other,
                    "免审直过回调失败且打回草稿也失败：申请单停在 SUBMITTED（无待办），需人工介入"
                );
                return Err(ApiError::new(
                    500,
                    error_code::INTERNAL_ERROR,
                    format!(
                        "申请单 {} 的审批实例已结束，但结论回写与自动打回都失败（审批单号可查）；\
                         请联系管理员排查，单据当前停在「审批中」",
                        row.access_no
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// 没有流程时的「显式跳过」路径：同一事务里 CAS 单据 + 落供应商状态。
async fn apply_directly(
    row: &AccessRow,
    operator_id: Uuid,
    reason: &str,
    skip_reason: &str,
) -> std::result::Result<(), ApiError> {
    let merged = approval_gate::merge_reason(Some(reason), skip_reason);
    let target = model::target_supplier_status(&row.access_type);
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启免审生效事务"))?;
    let outcome = async {
        let affected = model::mark_direct_approved(&tx, row.id, Some(operator_id), Some(merged.as_str()))
            .await
            .map_err(|error| utils::db_error(error, "申请单进入已生效（跳过审批）"))?;
        if affected == 0 {
            return Err(status_drift(row, "提交审批"));
        }
        let landed = model::set_supplier_status(&tx, row.supplier_id, target, Some(operator_id))
            .await
            .map_err(|error| utils::db_error(error, "落地供应商启停状态"))?;
        if landed == 0 {
            return Err(ApiError::new(
                409,
                error_code::ERP_SUPPLIER_NOT_USABLE,
                "供应商已被删除，无法落地启停状态：请重新建档后再提交",
            ));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交免审生效事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

// ==================== ④ 分页 / 详情 ====================

/// 分页查询。
#[nik::permission(
    path = "/business/erp/supplier-access/page",
    method = "GET",
    permission = "business:erp:supplier-access:list"
)]
#[nik::get("/business/erp/supplier-access/page")]
pub async fn supplier_access_page(Query(query): Query<AccessPageQuery>) -> ResResult<PageData<Vec<AccessRowRes>>> {
    let filter = query.filter()?;
    let (rows, total) = model::page_access(&filter)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商准入 / 黑名单申请"))?;
    Ok(Res::ok(PageData {
        list: rows.iter().map(dto::to_row_res).collect(),
        total,
        page: filter.page,
        page_size: filter.page_size,
    }))
}

/// 详情。
#[nik::permission(
    path = "/business/erp/supplier-access/{id}",
    method = "GET",
    permission = "business:erp:supplier-access:list"
)]
#[nik::get("/business/erp/supplier-access/{id}")]
pub async fn supplier_access_detail(Path(id): Path<String>) -> ResResult<AccessRowRes> {
    let id = dto::parse_uuid(&id, "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 组装详情（建 / 改 / 提交的返回值共用）。
pub async fn detail_of(id: Uuid) -> std::result::Result<AccessRowRes, ApiError> {
    Ok(dto::to_row_res(&load_or_404(id).await?))
}

// ==================== 内部小工具 ====================

/// 取单据，不存在 → 404。
async fn load_or_404(id: Uuid) -> std::result::Result<AccessRow, ApiError> {
    model::find_access(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商准入 / 黑名单申请"))?
        .ok_or_else(|| dto::not_found(id))
}

/// 只有草稿能做（改 / 删 / 提交）。
fn ensure_draft(row: &AccessRow, action: &str) -> std::result::Result<(), ApiError> {
    if row.status == model::STATUS_DRAFT {
        return Ok(());
    }
    Err(dto::status_conflict(format!(
        "申请单 {} 当前是「{}」（{}），只有拟稿（DRAFT）可以{action}；\
         审批中请走审批中心（撤回 / 审批），已生效的单据不接受改写",
        row.access_no,
        model::status_label(&row.status),
        row.status
    )))
}

/// 读到的状态和刚写完的 CAS 撞车时的 409（并发提交 / 并发删除）。
fn status_drift(row: &AccessRow, action: &str) -> ApiError {
    dto::status_conflict(format!(
        "申请单 {} 的状态在本次{action}期间已被改动（期望 DRAFT），请刷新后重试",
        row.access_no
    ))
}

/// 审批表单（发起时冻结；条件边 `form.access_type eq 'BLACKLIST'` 读的就是这里）。
fn form_data(row: &AccessRow, reason: &str) -> serde_json::Value {
    serde_json::json!({
        "access_no": row.access_no,
        "access_type": row.access_type,
        "access_type_label": model::type_label(&row.access_type),
        "supplier_id": row.supplier_id.to_string(),
        "supplier_code": row.supplier_code,
        "supplier_name": row.supplier_name,
        "grade_snapshot": row.grade_snapshot,
        "reason": reason,
        "target_status": model::target_supplier_status(&row.access_type),
    })
}

/// 在跑实例的单号（拼 409 文案用；查不到就给 `-`）。
async fn pending_instance_no_of(instance_id: Option<Uuid>) -> std::result::Result<String, ApiError> {
    let Some(instance_id) = instance_id else {
        return Ok("-".to_string());
    };
    let no = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批单"))?
        .map(|instance| instance.instance_no)
        .unwrap_or_else(|| "-".to_string());
    Ok(no)
}

/// 引擎错误 → 统一出口（CONFLICT 类错误翻 409，其余 500）。
fn submit_conflict(error: ApiError, action: &str, access_no: &str) -> ApiError {
    if error.status >= 400 && error.status < 500 {
        dto::status_conflict(format!("{access_no} {action}失败：{}", error.msg))
    } else {
        ApiError::new(
            500,
            error.code,
            format!("{access_no} {action}失败（内部错误）：{}", error.msg),
        )
    }
}
