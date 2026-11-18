//! 按生产订单/批次归集表 · 接口层（成本核算 · M5-02【M5·成本】分批法 ①）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-collect-sheet/...`。
//!
//! | # | 方法 | 路径 | 权限码 |
//! |---|---|---|---|
//! | 1 | GET | `/business/erp/cost-collect-sheet?period_code=yyyy-MM` | `business:erp:cost-object:list` |
//! | 2 | GET | `/business/erp/cost-collect-sheet/{object_id}?period_code=yyyy-MM` | `business:erp:cost-object:list` |
//!
//! # 两个接口的分工
//!
//! * **1 归集表**：一页批次 × 料 / 工 / 费 / 合计，表尾一份**整张表**的合计
//!   （不是当前页的合计 —— 那个数会随翻页变，用户会以为账在跳）。
//! * **2 明细**：某批次本期逐笔归集行（料从哪张领料单来、工是哪几张工时单、
//!   费是哪几笔台账），合计由这些行相加 —— 与表 1 的该行**必然相等**（同一份
//!   `biz_cost_collect`，只是粒度不同）。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_object_handler` / `cost_uncollected_handler` 同款：axum handler 要
//! `AuthClaims` 才能跑，单测调不到；把「校验 + 取数 + 装配」收进 [`sheet_of`] /
//! [`detail_of`] 之后，真库用例走**同一条代码路径**（只跳过认证）。
//!
//! # 为什么没有 `POST` / 写接口
//!
//! 这张表是**视图**（归集明细由 B2-6/7/8 的 calc 各段写入 `biz_cost_collect`）。
//! 卡面「归集表」要的是「按生产订单/批次看归集」，不是「手工往批上填钱」——
//! 手工补料请走领料单、手工补工请走工时台账、手工补费请走费用台账，
//! 三条路都有单据、有审计、有幂等键；在这里开一个「直接改归集额」的口子，
//! 等于让成本能绕过单据凭空出现。
//!
//! # 数据权限与 404 口径
//!
//! 列表的行由 `biz_cost_object.workshop_dept_id` 裁剪（见 model 模块头）；
//! 明细的 `{object_id}` 走同一份探针，**看不见就是 404**（不是 403：403 等于
//! 承认「这个 id 真实存在」，把别的车间的批次号变成探测口）。

use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_collect_sheet_dto::{
    self as dto, SheetAmounts, SheetDetailQuery, SheetDetailRes, SheetQuery, SheetRes, SheetTotalsRes,
};
use crate::business::erp::cost::cost_collect_sheet_model as model;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码 ====================

/// 列表 / 明细（**复用** `cost-object:list`，理由见 `cost_collect_sheet_dto::PERM_LIST`）。
pub const PERM_LIST: &str = dto::PERM_LIST;

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形，便于汇总脚本收集）。
pub const SHEET_PERMISSIONS: [&str; 1] = dto::PERMISSIONS;

// ==================== 错误出口 ====================

/// 批次不存在 / 已删 / 不在你的数据范围 —— 三种都是 404（不区分，避免探测）。
fn batch_not_found(id: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!(
            "批次不存在、已删除，或不在你的数据范围（{id}）。\
             归集表请用 GET /business/erp/cost-collect-sheet?period_code=yyyy-MM"
        ),
    )
}

/// 路径参数 → UUID（非法形状也是 404：这个位置本来就该是 id）。
fn parse_id(raw: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| batch_not_found(raw))
}

// ==================== 装配 ====================

/// 归集表（分页 + 整张表合计）。
pub async fn sheet_of(principal: &ScopedPrincipal, query: SheetQuery) -> std::result::Result<SheetRes, ApiError> {
    let filter = dto::filter_of(&query)?;
    let (rows, total) = model::page_rows(principal, &filter)
        .await
        .map_err(|error| utils::db_error(error, "查询按批次归集表"))?;
    let totals = model::totals(principal, &filter)
        .await
        .map_err(|error| utils::db_error(error, "汇总按批次归集表"))?;

    Ok(SheetRes {
        period_code: filter.period_code.clone(),
        totals: SheetTotalsRes {
            amounts: SheetAmounts::of(totals.mat, totals.labor, totals.oh),
            line_count: totals.line_count,
            batch_count: totals.batch_count,
        },
        list: rows.iter().map(model::SheetRow::to_res).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page: filter.page,
        page_size: filter.page_size,
    })
}

/// 某一批次某期间的逐笔归集明细。
pub async fn detail_of(
    principal: &ScopedPrincipal,
    id: &str,
    query: SheetDetailQuery,
) -> std::result::Result<SheetDetailRes, ApiError> {
    let object_id = parse_id(id)?;
    let period_code = dto::period_of(query.period_code.as_deref())?;

    let batch = model::find_visible_batch(principal, object_id)
        .await
        .map_err(|error| utils::db_error(error, "查询批次的归集抬头"))?
        .ok_or_else(|| batch_not_found(id))?;

    let rows = model::lines_of(object_id, &period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询批次的归集明细"))?;

    // 合计由明细行相加（单一口径）：表 1 的该行金额 = 这里的三项和，
    // 两处若各查一次库，中间那笔并发写入会让它们对不上。
    let (mut mat, mut labor, mut oh) = (
        nik::prelude::rust_decimal::Decimal::ZERO,
        nik::prelude::rust_decimal::Decimal::ZERO,
        nik::prelude::rust_decimal::Decimal::ZERO,
    );
    for row in &rows {
        match row.element.as_str() {
            "MATERIAL" => mat += row.amount,
            "LABOR" => labor += row.amount,
            _ => oh += row.amount,
        }
    }

    Ok(SheetDetailRes {
        period_code,
        batch: batch.to_res(),
        totals: SheetAmounts::of(mat, labor, oh),
        lines: rows.iter().map(model::LineRow::to_res).collect(),
    })
}

// ==================== HTTP 入口 ====================

/// 按生产订单/批次归集表（分页）。
#[nik::permission(
    path = "/business/erp/cost-collect-sheet",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-collect-sheet")]
pub async fn cost_collect_sheet_page_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<SheetQuery>,
) -> ResResult<SheetRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(sheet_of(&principal, query).await?))
}

/// 某一批次某期间的逐笔归集明细。
#[nik::permission(
    path = "/business/erp/cost-collect-sheet/{object_id}",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-collect-sheet/{object_id}")]
pub async fn cost_collect_sheet_detail_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(object_id): Path<String>,
    Query(query): Query<SheetDetailQuery>,
) -> ResResult<SheetDetailRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(detail_of(&principal, &object_id, query).await?))
}

/// 权限码自检出口（与 `handler::all_permissions` 同形）。
pub fn all_permissions() -> Vec<&'static str> {
    SHEET_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_is_reused_from_cost_object_list() {
        assert_eq!(PERM_LIST, "business:erp:cost-object:list");
        assert_eq!(all_permissions(), vec!["business:erp:cost-object:list"]);
    }

    #[test]
    fn path_param_must_be_uuid_and_404_on_garbage() {
        let id = Uuid::nil();
        assert_eq!(parse_id(&id.to_string()).expect("合法 uuid"), id);
        // 非法 id ⇒ 404（不是 400）：这个位置本来就该是 id，且不告诉调用方「形状错」
        let error = parse_id("CB-20261101-000001").expect_err("批次号不是 uuid");
        assert_eq!(error.status, 404);
        assert_eq!(error.code, error_code::RESOURCE_NOT_FOUND);
    }

    #[test]
    fn invalid_period_is_400_before_touching_the_database() {
        // 期间校验在取数之前：非法期间不该打库（也不该先做 404 探测）
        let query = SheetQuery {
            period_code: Some("2026-1".to_string()),
            ..Default::default()
        };
        let error = dto::filter_of(&query).expect_err("2026-1 不是 yyyy-MM");
        assert_eq!(error.status, 400);
    }
}
