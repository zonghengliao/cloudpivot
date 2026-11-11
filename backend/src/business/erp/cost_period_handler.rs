//! 成本核算批次（期间结算单）接口层（成本核算 · ARCH-9 §5.2 #25–#33 的骨架部分 · B3-1）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-period/...`（约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 本卡 |
//! |---|---|---|---|---|
//! | 25 | GET | `/business/erp/cost-period` | `business:erp:cost-period:list` | ✅ 列表 |
//! | 26 | GET | `/business/erp/cost-period/{id}` | `business:erp:cost-period:list` | ✅ 详情（单头 + 计算单 + 可去状态） |
//! | — | POST | `/business/erp/cost-period` | `business:erp:cost-period:calc` | ✅ 建 `DRAFT`（见下「多出来的两个接口」） |
//! | — | PUT | `/business/erp/cost-period/{id}` | `business:erp:cost-period:calc` | ✅ 改 `DRAFT`（人工来源 / 备注） |
//! | 31 | POST | `/business/erp/cost-period/{id}/cancel` | `business:erp:cost-period:submit` | ✅ 作废（仅 `DRAFT`） |
//! | 27 | POST | `/business/erp/cost-period/calc` | `business:erp:cost-period:calc` | ⛔ 编排卡（六段串联） |
//! | 28 | POST | `/business/erp/cost-period/{id}/submit` | `business:erp:cost-period:submit` | ⛔ B3-3（走审批引擎） |
//! | 29 | POST | `/business/erp/cost-period/{id}/settle` | `business:erp:cost-period:settle` | ⛔ B3-4（入库 + 凭证） |
//! | 30 | POST | `/business/erp/cost-period/{id}/reverse` | `business:erp:cost-period:reverse` | ⛔ B3-5（红冲） |
//! | 32/33 | GET | `…/cost-sheet` / `…/uncollected` | `business:erp:cost-period:list` | ⛔ 导出 / 缺口清单卡 |
//!
//! # 多出来的两个接口（已具名偏差，见卡内汇报）
//!
//! 设计 §5.2 **没有** `POST /cost-period` 与 `PUT /cost-period/{id}`：
//! 设计里批次由 #27 `calc` 的「不存在则建 `DRAFT`」顺带创建。但卡面要求
//! 「`biz_cost_period` CRUD」，且下游 B3-3/B3-4 与手工联调都需要一个**能先建出批次**的
//! 入口（否则只能等 #27 落地才能测状态机）。所以：
//!
//! * 建 / 改复用 #27 的权限码 `cost-period:calc`（**同一语义、同一动作的归属**，
//!   **不新增权限码** —— 权限码清单是设计 §5.3 定稿，V0378 已种进 `sys_menu.auth_code`）；
//! * 改只放「改数不算账」的两个字段（人工来源 / 备注），且只有 `DRAFT` 可改。
//!
//! # 状态机口径
//!
//! 所有状态变更走 `state_machine::transition`（边表校验 → reason 校验 → 写
//! `biz_status_log`），与状态本身**同一个事务**。本卡只做 `DRAFT → CANCELLED` 一条边；
//! 提交 / 审批落地 / 结算 / 关期 / 红冲分别由 B3-3、B3-4、B3-5 接线，但**边表在本卡一次登记齐**
//! （`state_machine::COST_PERIOD_EDGES`）—— 后三张卡只写副作用，不再动边表。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_labor_dto;
use crate::business::erp::cost_period_dto::{
    self, CostPeriodCancelReq, CostPeriodCreateReq, CostPeriodDetailRes, CostPeriodFilter, CostPeriodListRes,
    CostPeriodPageQuery, CostPeriodUpdateReq,
};
use crate::business::erp::cost_period_model::{self as model, CostPeriodRow};
use crate::business::shared::state_machine;
use crate::utils;

// ==================== 权限码（与设计 §5.3 / V0378 的 auth_code 逐字一致） ====================

/// 列表 / 详情 / 成本计算单 / 缺口清单。
pub const PERM_LIST: &str = "business:erp:cost-period:list";
/// 计算成本（含建 `DRAFT` 批次；#27 与手工建单同码）。
pub const PERM_CALC: &str = "business:erp:cost-period:calc";
/// 提交审批（设计 #31 的作废也用这个码）。
pub const PERM_SUBMIT: &str = "business:erp:cost-period:submit";
/// 结算（入库 + 三张凭证）。
pub const PERM_SETTLE: &str = "business:erp:cost-period:settle";
/// 红冲结算。
pub const PERM_REVERSE: &str = "business:erp:cost-period:reverse";

/// 成本核算批次全部权限码（设计 §5.3 的 5 个；V0378 已种进 `sys_menu`）。
pub const COST_PERIOD_PERMISSIONS: [&str; 5] = [PERM_LIST, PERM_CALC, PERM_SUBMIT, PERM_SETTLE, PERM_REVERSE];

// ==================== 出参组装 ====================

/// 列表行。
fn to_list_res(row: &CostPeriodRow) -> CostPeriodListRes {
    CostPeriodListRes {
        id: row.id.to_string(),
        period_code: row.period_code.clone(),
        batch_no: row.batch_no.clone(),
        status: row.status.clone(),
        labor_source: row.labor_source.clone(),
        object_count: row.object_count,
        total_material: row.total_material.to_string(),
        total_labor: row.total_labor.to_string(),
        total_overhead: row.total_overhead.to_string(),
        total_cost: row.total_cost.to_string(),
        total_finished: row.total_finished.to_string(),
        total_wip: row.total_wip.to_string(),
        settled_at: row.settled_at.map(|at| at.to_rfc3339()),
        created_by_name: row.create_by_name.clone(),
        created_at: row.created_at.to_rfc3339(),
        remark: row.remark.clone(),
    }
}

/// 详情（单头 + 计算单 + 现算合计 + 可去状态）。
///
/// `edges` 必须是**有效边**：管理端禁用某条边之后 `next_statuses` 里就不再出现它，
/// 前端按钮随之消失（不是「按钮还在、点下去 409」）。
fn to_detail_res(
    row: &CostPeriodRow,
    items: Vec<cost_period_dto::CostSheetItemRes>,
    totals: &model::PeriodTotals,
    edges: Vec<(String, String)>,
) -> CostPeriodDetailRes {
    CostPeriodDetailRes {
        id: row.id.to_string(),
        org_id: row.org_id.to_string(),
        period_code: row.period_code.clone(),
        batch_no: row.batch_no.clone(),
        status: row.status.clone(),
        labor_source: row.labor_source.clone(),
        object_count: row.object_count,
        total_material: row.total_material.to_string(),
        total_labor: row.total_labor.to_string(),
        total_overhead: row.total_overhead.to_string(),
        total_cost: row.total_cost.to_string(),
        total_finished: row.total_finished.to_string(),
        total_wip: row.total_wip.to_string(),
        approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
        labor_voucher_id: row.labor_voucher_id.map(|id| id.to_string()),
        overhead_voucher_id: row.overhead_voucher_id.map(|id| id.to_string()),
        finish_voucher_id: row.finish_voucher_id.map(|id| id.to_string()),
        settled_at: row.settled_at.map(|at| at.to_rfc3339()),
        settled_by: row.settled_by.map(|id| id.to_string()),
        settled_by_name: row.settled_by_name.clone(),
        reversed_at: row.reversed_at.map(|at| at.to_rfc3339()),
        reverse_reason: row.reverse_reason.clone(),
        cancel_reason: row.cancel_reason.clone(),
        remark: row.remark.clone(),
        created_by_name: row.create_by_name.clone(),
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
        items,
        items_summary: totals.to_summary(),
        next_statuses: cost_period_dto::next_statuses_of(&edges, &row.status),
        legal_edges: edges,
    }
}

/// 按 id 组装详情（建 / 改 / 作废后的回单都走它）。
async fn detail_of(id: Uuid) -> std::result::Result<CostPeriodDetailRes, ApiError> {
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询成本核算批次"))?
        .ok_or_else(|| cost_period_dto::not_found(id))?;
    let items = model::sheet_items(id)
        .await
        .map_err(|error| utils::db_error(error, "查询成本计算单"))?;
    let totals = model::totals_of(id)
        .await
        .map_err(|error| utils::db_error(error, "汇总成本计算单"))?;
    let edges = model::effective_edges().await;
    Ok(to_detail_res(&row, items, &totals, edges))
}

// ==================== 业务逻辑（单测直接调这些） ====================

/// 新建 `DRAFT` 批次。
///
/// 前置：
/// 1. 期间合法（`yyyy-MM`，复用 `cost_labor_dto::normalize_period` 的口径）；
/// 2. **同期间没有活跃批次**（`uk_biz_cost_period` 的人话版；`REVERSED` / `CANCELLED` 不占位，
///    所以红冲后能直接重建 —— 设计 §4.1「红冲后同期间可重建」）；
/// 3. 人工来源在值域内（`MANUAL` / `PAYROLL`，防双计提）。
pub async fn create_draft(
    operator: Option<Uuid>,
    req: CostPeriodCreateReq,
) -> std::result::Result<CostPeriodDetailRes, ApiError> {
    let period_code = cost_labor_dto::normalize_period(req.period_code.as_deref())?;
    let labor_source = cost_period_dto::normalize_labor_source(req.labor_source.as_deref())?;
    let remark = cost_period_dto::normalize_remark(req.remark.as_deref())?;
    if let Some(existing) = model::active_by_period(db(), &period_code)
        .await
        .map_err(|error| utils::db_error(error, "检查期间的活跃批次"))?
    {
        return Err(cost_period_dto::duplicate_period(&period_code, &existing.batch_no));
    }
    let id = Uuid::now_v7();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启新建批次事务"))?;
    let result = async {
        // 取号在事务内：并发建单两个号必然不同（biz_seq 行锁），撞了也有唯一键兜底
        let batch_no = model::next_batch_no(&tx, &period_code)
            .await
            .map_err(|error| utils::db_error(error, "生成成本核算批次号"))?;
        model::insert_draft(
            &tx,
            id,
            &period_code,
            &batch_no,
            &labor_source,
            remark.as_deref(),
            operator,
        )
        .await
        .map_err(|error| match error {
            // 并发下"前置检查通过但唯一键撞了"：翻译成人话而不是 500
            DbError::UniqueViolation { .. } => {
                cost_period_dto::duplicate_period(&period_code, "（并发创建，已存在活跃批次）")
            }
            other => utils::db_error(other, "新建成本核算批次"),
        })?;
        // 建单留痕（from_status = NULL）：之后每次状态变更都能对上「谁建的」
        state_machine::log_created(
            &tx,
            state_machine::ENTITY_COST_PERIOD,
            id,
            &batch_no,
            cost_period_dto::STATUS_DRAFT,
            operator,
            Some("新建成本核算批次（DRAFT）"),
        )
        .await?;
        Ok::<(), ApiError>(())
    }
    .await;
    if let Err(error) = result {
        let _ = tx.rollback().await;
        return Err(error);
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交新建批次事务"))?;
    detail_of(id).await
}

/// 改 `DRAFT` 批次（人工来源 / 备注）。
pub async fn update_draft(
    operator: Option<Uuid>,
    id_raw: &str,
    req: CostPeriodUpdateReq,
) -> std::result::Result<CostPeriodDetailRes, ApiError> {
    let id = cost_period_dto::parse_uuid(id_raw, "id")?;
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑的批次"))?
        .ok_or_else(|| cost_period_dto::not_found(id))?;
    if !row.writable() {
        return Err(cost_period_dto::status_guard(
            &row.batch_no,
            "改数",
            &row.status,
            &cost_period_dto::WRITABLE_STATUSES,
        ));
    }
    // 未传的字段保持原值（部分更新，与其余 handler 同款）
    let labor_source = match req.labor_source.as_deref() {
        Some(raw) => cost_period_dto::normalize_labor_source(Some(raw))?,
        None => row.labor_source.clone(),
    };
    let remark = match req.remark {
        Some(raw) => cost_period_dto::normalize_remark(Some(&raw))?,
        None => row.remark.clone(),
    };
    let affected = model::update_draft(db(), id, &labor_source, remark.as_deref(), operator)
        .await
        .map_err(|error| utils::db_error(error, "编辑成本核算批次"))?;
    if affected == 0 {
        return Err(cost_period_dto::conflict(format!(
            "成本核算批次 {} 的状态在本次编辑期间被改动（可能已被提交），请刷新后重试",
            row.batch_no
        )));
    }
    detail_of(id).await
}

/// 作废（`DRAFT → CANCELLED`；设计 §5.2 #31，原因 ≥10 字）。
pub async fn cancel_period(
    operator: Option<Uuid>,
    id_raw: &str,
    req: CostPeriodCancelReq,
) -> std::result::Result<CostPeriodDetailRes, ApiError> {
    let id = cost_period_dto::parse_uuid(id_raw, "id")?;
    let reason = cost_period_dto::require_reason(req.reason.as_deref(), cost_period_dto::CANCEL_REASON_MIN, "作废")?;
    let row = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待作废的批次"))?
        .ok_or_else(|| cost_period_dto::not_found(id))?;
    if !row.writable() {
        return Err(cost_period_dto::status_guard(
            &row.batch_no,
            "作废",
            &row.status,
            &cost_period_dto::WRITABLE_STATUSES,
        ));
    }
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启作废事务"))?;
    let result = async {
        // ① 状态机：DRAFT → CANCELLED（边表核对 + 必填原因 + 写 biz_status_log）
        state_machine::transition(
            &tx,
            state_machine::ENTITY_COST_PERIOD,
            id,
            &row.batch_no,
            &model::LEGAL_EDGES,
            cost_period_dto::STATUS_DRAFT,
            cost_period_dto::STATUS_CANCELLED,
            operator,
            Some(reason.as_str()),
            None,
        )
        .await?;
        // ② 状态与作废原因（同事务：CAS `WHERE status='DRAFT'`；库层 ck_..._cancel 要求原因）
        let affected = model::apply_cancel(&tx, id, &reason, operator)
            .await
            .map_err(|error| utils::db_error(error, "写作废状态"))?;
        if affected == 0 {
            return Err(cost_period_dto::conflict(format!(
                "成本核算批次 {} 的状态在本次作废期间被改动（可能已被提交），请刷新后重试",
                row.batch_no
            )));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    if let Err(error) = result {
        let _ = tx.rollback().await;
        return Err(error);
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交作废事务"))?;
    detail_of(id).await
}

/// 列表分页（期间合计单头 **unscoped** + 权限码，设计 D-18）。
pub async fn page_periods(
    query: CostPeriodPageQuery,
) -> std::result::Result<PageData<Vec<CostPeriodListRes>>, ApiError> {
    let filter = filter_of(&query)?;
    let (page, page_size) = (filter.page, filter.page_size);
    let (rows, total) = model::page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "分页查询成本核算批次"))?;
    Ok(PageData {
        list: rows.iter().map(to_list_res).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 归一化列表筛选（纯逻辑；单测可直接验）。
///
/// `status` 只认 7 个落库值；传 `PENDING` 会 400 并**直接告诉用户该用 `SUBMITTED`**
/// （卡面 / 口头都爱说「待审」，与其让前端猜，不如报文里写清楚）。
pub fn filter_of(query: &CostPeriodPageQuery) -> std::result::Result<CostPeriodFilter, ApiError> {
    let status = cost_period_dto::clean_optional(query.status.clone());
    let status = match status {
        None => None,
        Some(text) => {
            let upper = text.to_ascii_uppercase();
            if !cost_period_dto::STATUSES.contains(&upper.as_str()) {
                let hint = if upper == "PENDING" {
                    "：本系统的「待审」落库值是 SUBMITTED（卡面的 PENDING 是同一个状态的叫法）"
                } else {
                    ""
                };
                return Err(cost_period_dto::bad_request(format!(
                    "status 只认 {:?}（收到「{text}」）{hint}",
                    cost_period_dto::STATUSES
                )));
            }
            Some(upper)
        }
    };
    let period_code = cost_period_dto::clean_optional(query.period_code.clone())
        .map(|raw| cost_labor_dto::normalize_period(Some(&raw)))
        .transpose()?;
    let (page, page_size) =
        crate::business::erp::model::normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    Ok(CostPeriodFilter {
        period_code,
        status,
        keyword: cost_period_dto::clean_optional(query.keyword.clone()),
        page,
        page_size,
    })
}

// ==================== HTTP 入口 ====================

/// 成本核算批次分页。
#[nik::permission(
    path = "/business/erp/cost-period",
    method = "GET",
    permission = "business:erp:cost-period:list"
)]
#[nik::get("/business/erp/cost-period")]
pub async fn cost_period_page_api(
    Query(query): Query<CostPeriodPageQuery>,
) -> ResResult<PageData<Vec<CostPeriodListRes>>> {
    Ok(Res::ok(page_periods(query).await?))
}

/// 成本核算批次详情。
#[nik::permission(
    path = "/business/erp/cost-period/{id}",
    method = "GET",
    permission = "business:erp:cost-period:list"
)]
#[nik::get("/business/erp/cost-period/{id}")]
pub async fn cost_period_detail_api(Path(id): Path<String>) -> ResResult<CostPeriodDetailRes> {
    let id = cost_period_dto::parse_uuid(&id, "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 新建批次（`DRAFT`）。
#[nik::permission(
    path = "/business/erp/cost-period",
    method = "POST",
    permission = "business:erp:cost-period:calc"
)]
#[nik::post("/business/erp/cost-period")]
pub async fn cost_period_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CostPeriodCreateReq>,
) -> ResResult<CostPeriodDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_draft(operator, req).await?))
}

/// 编辑批次（只允许 `DRAFT`）。
#[nik::permission(
    path = "/business/erp/cost-period/{id}",
    method = "PUT",
    permission = "business:erp:cost-period:calc"
)]
#[nik::put("/business/erp/cost-period/{id}")]
pub async fn cost_period_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CostPeriodUpdateReq>,
) -> ResResult<CostPeriodDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_draft(operator, &id, req).await?))
}

/// 作废（仅 `DRAFT`）。
#[nik::permission(
    path = "/business/erp/cost-period/{id}/cancel",
    method = "POST",
    permission = "business:erp:cost-period:submit"
)]
#[nik::post("/business/erp/cost-period/{id}/cancel")]
pub async fn cost_period_cancel_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CostPeriodCancelReq>,
) -> ResResult<CostPeriodDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(cancel_period(operator, &id, req).await?))
}

/// 权限码自检出口（与其余模块 `all_permissions` 同形，便于汇总脚本收集）。
pub fn all_permissions() -> Vec<&'static str> {
    COST_PERIOD_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- 权限码：与设计 §5.3 / V0378 菜单 auth_code 三方一致 ----------

    /// V0378 的种子（**已应用迁移**，只读不改）：菜单 `auth_code` 的唯一来源。
    const MENU_SEED: &str = include_str!("../../../migrations/business/V0378__cost_menu_and_perm.up.sql");

    #[test]
    fn permission_codes_match_design_5_3() {
        assert_eq!(PERM_LIST, "business:erp:cost-period:list");
        assert_eq!(PERM_CALC, "business:erp:cost-period:calc");
        assert_eq!(PERM_SUBMIT, "business:erp:cost-period:submit");
        assert_eq!(PERM_SETTLE, "business:erp:cost-period:settle");
        assert_eq!(PERM_REVERSE, "business:erp:cost-period:reverse");
        assert_eq!(all_permissions().len(), 5, "设计 §5.3 给了 5 个码");
        for code in COST_PERIOD_PERMISSIONS {
            let parts: Vec<&str> = code.split(':').collect();
            assert_eq!(parts.len(), 4, "{code}");
            assert_eq!(parts[0], "business", "{code}");
            assert_eq!(parts[1], "erp", "{code}");
            assert_eq!(parts[2], "cost-period", "{code}");
        }
    }

    #[test]
    fn permission_codes_match_menu_auth_code_in_v0378() {
        // 按钮菜单的 `name` 与 `auth_code` **都写同一个码**（V0378 的写法），所以出现两次；
        // 页面菜单只有 `auth_code` 一处（`name` 是 '成本核算'）。
        for code in COST_PERIOD_PERMISSIONS {
            let quoted = format!("'{code}'");
            let hits = MENU_SEED.matches(&quoted).count();
            let expected = if code == PERM_LIST { 1 } else { 2 };
            assert!(
                hits >= expected,
                "{code} 在 V0378 里只出现 {hits} 次（至少 {expected} 次：按钮 name + auth_code \
                 / 页面 auth_code），否则侧边栏点下去 403"
            );
        }
        // 四个按钮菜单的文案也在（码存在但按钮没种 = 前端没入口）
        for title in ["计算成本", "提交审批", "结算（入库+凭证）", "红冲结算"] {
            assert!(MENU_SEED.contains(title), "V0378 缺按钮菜单「{title}」");
        }
        // 页面菜单：标题「成本核算」+ 路径 + 权限码同一行
        assert!(
            MENU_SEED.contains("'成本核算'") && MENU_SEED.contains("'/business/erp/cost-period'"),
            "V0378 的页面菜单行必须同时带标题与路径"
        );
        assert!(
            MENU_SEED.contains("'business/erp/cost-period/index'"),
            "V0378 的页面菜单缺前端组件路径"
        );
    }

    #[test]
    fn route_paths_have_no_api_prefix() {
        for path in [
            "/business/erp/cost-period",
            "/business/erp/cost-period/{id}",
            "/business/erp/cost-period/{id}/cancel",
        ] {
            assert!(path.starts_with("/business/erp/cost-period"));
            assert!(!path.starts_with("/api"), "{path} 不能带 /api 前缀（约定 8）");
        }
    }

    // ---------- 筛选归一化 ----------

    #[test]
    fn filter_normalizes_status_and_period() {
        let query = CostPeriodPageQuery {
            status: Some(" submitted ".to_string()),
            period_code: Some("2026-10".to_string()),
            page: None,
            page_size: Some(0),
            ..Default::default()
        };
        let filter = filter_of(&query).expect("筛选");
        assert_eq!(filter.status.as_deref(), Some(cost_period_dto::STATUS_SUBMITTED));
        assert_eq!(filter.period_code.as_deref(), Some("2026-10"));
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, 20, "page_size=0 回落到默认 20");

        let query = CostPeriodPageQuery {
            page_size: Some(10_000),
            ..Default::default()
        };
        assert_eq!(filter_of(&query).expect("筛选").page_size, 200, "上限 200");
    }

    #[test]
    fn filter_rejects_pending_with_a_hint() {
        let query = CostPeriodPageQuery {
            status: Some("PENDING".to_string()),
            ..Default::default()
        };
        let error = filter_of(&query).expect_err("PENDING 不是落库值");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("SUBMITTED"), "报文要点名正确的值：{}", error.msg);

        let query = CostPeriodPageQuery {
            status: Some("FINISHED".to_string()),
            ..Default::default()
        };
        assert_eq!(filter_of(&query).expect_err("未知状态").status, 400);
    }

    #[test]
    fn filter_rejects_bad_period() {
        let query = CostPeriodPageQuery {
            period_code: Some("2026-13".to_string()),
            ..Default::default()
        };
        assert_eq!(filter_of(&query).expect_err("非法期间").status, 400);
    }

    // ---------- 状态机：合法边 / 非法跃迁 ----------

    /// 卡面验收的完整链路：DRAFT → SUBMITTED → APPROVED → SETTLED → CLOSED / REVERSED。
    #[test]
    fn full_chain_is_legal_and_reaches_final_states() {
        use state_machine::Entity;
        let chain = [
            ("DRAFT", "SUBMITTED"),
            ("SUBMITTED", "APPROVED"),
            ("APPROVED", "SETTLED"),
            ("SETTLED", "CLOSED"),
        ];
        for (from, to) in chain {
            assert!(Entity::CostPeriod.can_transition(from, to), "{from} → {to} 该合法");
        }
        // 结算后另一条出路：红冲
        assert!(Entity::CostPeriod.can_transition("SETTLED", "REVERSED"));
        // 提交后两条出路：审批通过 / 驳回回草稿
        assert!(Entity::CostPeriod.can_transition("SUBMITTED", "DRAFT"));
        // 草稿两条出路：提交 / 作废
        assert!(Entity::CostPeriod.can_transition("DRAFT", "CANCELLED"));
        assert_eq!(Entity::CostPeriod.edges().len(), 7);
    }

    #[test]
    fn illegal_jumps_are_rejected() {
        use state_machine::Entity;
        let illegal = [
            // 免审直过：结算必须过审批流（设计 §4.1）
            ("DRAFT", "APPROVED"),
            ("DRAFT", "SETTLED"),
            ("DRAFT", "CLOSED"),
            ("DRAFT", "REVERSED"),
            // 提交后不能跳过审批直接结算 / 关期
            ("SUBMITTED", "SETTLED"),
            ("SUBMITTED", "CLOSED"),
            // 审批通过后不能作废（数据已冻结，只能结算或撤回）
            ("APPROVED", "CANCELLED"),
            ("APPROVED", "REVERSED"),
            // 结算后不能回草稿 / 作废
            ("SETTLED", "DRAFT"),
            ("SETTLED", "CANCELLED"),
            ("SETTLED", "SUBMITTED"),
            // 结账后只读（红冲要在 CLOSED 之前）
            ("CLOSED", "REVERSED"),
            ("CLOSED", "SETTLED"),
            ("CLOSED", "DRAFT"),
            // 终态回头路
            ("REVERSED", "DRAFT"),
            ("REVERSED", "SETTLED"),
            ("CANCELLED", "DRAFT"),
        ];
        for (from, to) in illegal {
            assert!(
                !Entity::CostPeriod.can_transition(from, to),
                "{from} → {to} 必须被拒（非法跃迁）"
            );
        }
    }

    #[test]
    fn reason_is_required_for_reverse_and_cancel_only() {
        use state_machine::Entity;
        assert!(Entity::CostPeriod.needs_reason("SETTLED", "REVERSED"), "红冲要原因");
        assert!(Entity::CostPeriod.needs_reason("DRAFT", "CANCELLED"), "作废要原因");
        // 撤回 / 驳回同一条边：标成必填会把「撤回」挡住（LNK-P08 同款）
        assert!(!Entity::CostPeriod.needs_reason("SUBMITTED", "DRAFT"));
        assert!(!Entity::CostPeriod.needs_reason("DRAFT", "SUBMITTED"));
        assert!(!Entity::CostPeriod.needs_reason("APPROVED", "SETTLED"));
        assert!(!Entity::CostPeriod.needs_reason("SETTLED", "CLOSED"));
    }

    #[test]
    fn base_edges_match_registry_one_by_one() {
        use state_machine::Entity;
        let edges = Entity::CostPeriod.edges();
        let base = Entity::CostPeriod.base_edges();
        assert_eq!(edges.len(), base.len(), "BASE 与注册表条数必须一致");
        for ((from, to), edge) in edges.iter().zip(base.iter()) {
            assert_eq!((*from, *to), (edge.from, edge.to), "顺序也必须一致");
            assert!(edge.meta.mandatory, "结算链的边全部 mandatory（禁掉就是死单）");
            assert_eq!(
                edge.meta.requires_reason,
                Entity::CostPeriod.needs_reason(from, to),
                "{from} → {to} 的 requires_reason 与 reason_edges 不一致"
            );
        }
    }

    // ---------- 详情出参 ----------

    fn sample_row(status: &str) -> CostPeriodRow {
        use nik::prelude::chrono::Utc;
        use nik::prelude::rust_decimal::Decimal;
        let now = Utc::now();
        CostPeriodRow {
            id: Uuid::now_v7(),
            org_id: Uuid::nil(),
            period_code: "2026-10".to_string(),
            batch_no: "CS-202610-0001".to_string(),
            status: status.to_string(),
            labor_source: cost_period_dto::LABOR_SOURCE_MANUAL.to_string(),
            object_count: 2,
            total_material: Decimal::ZERO,
            total_labor: Decimal::ZERO,
            total_overhead: Decimal::ZERO,
            total_cost: Decimal::ZERO,
            total_finished: Decimal::ZERO,
            total_wip: Decimal::ZERO,
            approval_instance_id: None,
            labor_voucher_id: None,
            overhead_voucher_id: None,
            finish_voucher_id: None,
            settled_at: None,
            settled_by: None,
            reversed_at: None,
            reversed_by: None,
            reverse_reason: None,
            cancel_reason: None,
            remark: None,
            created_at: now,
            updated_at: now,
            create_by: None,
            update_by: None,
            create_by_name: Some("张三".to_string()),
            settled_by_name: None,
            reversed_by_name: None,
        }
    }

    fn edges_of_registry() -> Vec<(String, String)> {
        state_machine::COST_PERIOD_EDGES
            .iter()
            .map(|(from, to)| (from.to_string(), to.to_string()))
            .collect()
    }

    #[test]
    fn draft_offers_submit_and_cancel() {
        let res = to_detail_res(
            &sample_row(cost_period_dto::STATUS_DRAFT),
            Vec::new(),
            &model::PeriodTotals::default(),
            edges_of_registry(),
        );
        assert_eq!(
            res.next_statuses,
            vec!["SUBMITTED".to_string(), "CANCELLED".to_string()]
        );
        assert_eq!(res.legal_edges.len(), 7, "边表本身如实返回");
    }

    #[test]
    fn final_states_offer_nothing() {
        for status in [
            cost_period_dto::STATUS_CLOSED,
            cost_period_dto::STATUS_REVERSED,
            cost_period_dto::STATUS_CANCELLED,
        ] {
            let res = to_detail_res(
                &sample_row(status),
                Vec::new(),
                &model::PeriodTotals::default(),
                edges_of_registry(),
            );
            assert!(res.next_statuses.is_empty(), "{status} 是终态");
        }
    }

    #[test]
    fn settled_offers_close_and_reverse() {
        let res = to_detail_res(
            &sample_row(cost_period_dto::STATUS_SETTLED),
            Vec::new(),
            &model::PeriodTotals::default(),
            edges_of_registry(),
        );
        assert_eq!(res.next_statuses, vec!["CLOSED".to_string(), "REVERSED".to_string()]);
    }

    #[test]
    fn detail_serializes_contract_fields() {
        let res = to_detail_res(
            &sample_row(cost_period_dto::STATUS_DRAFT),
            Vec::new(),
            &model::PeriodTotals::default(),
            edges_of_registry(),
        );
        let json = serde_json::to_value(&res).expect("序列化");
        for field in [
            "id",
            "org_id",
            "period_code",
            "batch_no",
            "status",
            "labor_source",
            "object_count",
            "total_material",
            "total_labor",
            "total_overhead",
            "total_cost",
            "total_finished",
            "total_wip",
            "approval_instance_id",
            "finish_voucher_id",
            "items",
            "items_summary",
            "next_statuses",
            "legal_edges",
            "created_by_name",
        ] {
            assert!(json.get(field).is_some(), "详情缺字段 {field}（前端会 undefined）");
        }
        assert_eq!(json["total_cost"], serde_json::json!("0"), "金额是字符串");
        assert_eq!(json["period_code"], serde_json::json!("2026-10"));
    }

    #[test]
    fn list_row_serializes_frontend_fields() {
        let row = sample_row(cost_period_dto::STATUS_DRAFT);
        let json = serde_json::to_value(to_list_res(&row)).expect("序列化");
        for field in [
            "id",
            "period_code",
            "batch_no",
            "status",
            "labor_source",
            "object_count",
            "total_cost",
            "total_finished",
            "total_wip",
            "created_by_name",
            "created_at",
        ] {
            assert!(json.get(field).is_some(), "列表行缺字段 {field}");
        }
        assert_eq!(json["batch_no"], serde_json::json!("CS-202610-0001"));
    }

    /// `cost_object_dto::bad_request` 与本模块是同一种错误出口（400 + 10001），
    /// 这里只钉住「互不重复实现」这件事不再回头：两个模块都用仓库的 PARAM_INVALID。
    #[test]
    fn error_helpers_share_the_house_codes() {
        let ours = cost_period_dto::bad_request("x");
        let theirs = crate::business::erp::cost_object_dto::bad_request("x");
        assert_eq!(ours.status, theirs.status);
        assert_eq!(ours.code, theirs.code);
        assert_eq!(ours.code, crate::shared::error_code::PARAM_INVALID);
    }
}
