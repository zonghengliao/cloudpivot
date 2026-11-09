//! 折旧接口层（FIN-2 / ARCH-6 §5.5）：批次分页 / 批次详情 / 手动计提 / 红冲。
//!
//! | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/depreciation/page` | `depreciation:list` | 批次分页 |
//! | GET | `/business/erp/depreciation/{id}` | `depreciation:list` | 批次详情（明细 + 合计） |
//! | POST | `/business/erp/depreciation/run` | `depreciation:run` | 手动计提（默认上一个会计月） |
//! | POST | `/business/erp/depreciation/{id}/reverse` | `depreciation:reverse` | 红冲（红字凭证 + 回滚卡片） |
//!
//! # 手动与定时是**同一个** `run_batch`
//!
//! 定时任务（`erp_depreciation_run`，每月 1 日 01:30）走的也是
//! [`crate::business::erp::depreciation_model::run_scheduled`] → `run_batch`，
//! 只有「谁来点这个按钮」不同。两条路径分叉过一次就会有「定时提的和手动提的不一样」，
//! 这类问题在财务系统里是灾难（对账永远差一点点）。

use axum::Json;
use axum::extract::{Extension, Path, Query};
use nik::prelude::*;

use crate::business::erp::asset_dto::{
    DepBatchDetailRes, DepBatchFilter, DepBatchRes, ReverseDepreciationReq, RunDepreciationReq, RunDepreciationRes,
    normalize_page, parse_uuid_text,
};
use crate::business::erp::depreciation_model;
use crate::shared::error_code;
use crate::utils;

/// 批次分页。
#[nik::permission(
    path = "/business/erp/depreciation/page",
    method = "GET",
    permission = "business:erp:depreciation:list"
)]
#[nik::get("/business/erp/depreciation/page")]
pub async fn depreciation_page(
    Extension(AuthClaims(_claims)): Extension<AuthClaims>,
    Query(filter): Query<DepBatchFilter>,
) -> ResResult<PageData<Vec<DepBatchRes>>> {
    let (page, page_size) = normalize_page(
        filter.page.unwrap_or(1),
        filter
            .page_size
            .unwrap_or(crate::business::erp::asset_dto::DEFAULT_PAGE_SIZE),
    );
    let (rows, total) = crate::business::erp::asset_model::batch_page(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询折旧批次"))?;
    let list: Vec<DepBatchRes> = rows.iter().map(DepBatchRes::from).collect();
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 批次详情（明细 + 合计；合计必须等于批次金额 —— 巡检 K4）。
#[nik::permission(
    path = "/business/erp/depreciation/{id}",
    method = "GET",
    permission = "business:erp:depreciation:list"
)]
#[nik::get("/business/erp/depreciation/{id}")]
pub async fn depreciation_detail(Path(id): Path<String>) -> ResResult<DepBatchDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    Ok(Res::ok(depreciation_model::batch_detail(id).await?))
}

/// 手动计提（`period_code` 不传 = 上一个会计月，与定时任务同口径）。
#[nik::permission(
    path = "/business/erp/depreciation/run",
    method = "POST",
    permission = "business:erp:depreciation:run"
)]
#[nik::post("/business/erp/depreciation/run")]
pub async fn depreciation_run(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<RunDepreciationReq>,
) -> ResResult<RunDepreciationRes> {
    let operator = utils::current_user_uuid(&claims);
    if operator.is_none() {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "手动计提需要登录用户（令牌里没有用户 id）",
        ));
    }
    let outcome = depreciation_model::run_batch(req.period_code.as_deref(), operator, req.remark.as_deref()).await?;
    Ok(Res::ok(RunDepreciationRes::from(outcome)))
}

/// 红冲批次（红字凭证 + 明细 REVERSED + 卡片回滚）。
#[nik::permission(
    path = "/business/erp/depreciation/{id}/reverse",
    method = "POST",
    permission = "business:erp:depreciation:reverse"
)]
#[nik::post("/business/erp/depreciation/{id}/reverse")]
pub async fn depreciation_reverse(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<ReverseDepreciationReq>,
) -> ResResult<DepBatchDetailRes> {
    let id = parse_uuid_text(&id, "id")?;
    let reason = req.reason.as_deref().unwrap_or_default();
    depreciation_model::reverse_batch(id, reason, utils::current_user_uuid(&claims)).await?;
    Ok(Res::ok(depreciation_model::batch_detail(id).await?))
}
