//! 分批法「完工批次结转（完工转出）」· 接口层（成本核算 · M5-04）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-collect-sheet/...`（约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 写库 |
//! |---|---|---|---|---|
//! | 1 | GET | `/business/erp/cost-collect-sheet/{object_id}/finish-preview?period_code=yyyy-MM` | `business:erp:cost-object:list` | ❌ 试算 |
//! | 2 | POST | `/business/erp/cost-collect-sheet/{object_id}/finish` | `business:erp:cost-period:calc` | ✅ 结转 |
//!
//! # 为什么继续挂在 `cost-collect-sheet` 这个前缀下
//!
//! 与 M5-02 归集表 / M5-03 归集同一族资源（都是「一批货的成本工作底稿」），路径语义连贯：
//!
//! ```text
//! GET  /cost-collect-sheet                    → 一批一行（本期料工费合计）
//! GET  /cost-collect-sheet/{id}               → 这一批的逐笔归集行
//! GET  /cost-collect-sheet/{id}/sources       → 三路来源预览（M5-03）
//! POST /cost-collect-sheet/{id}/collect       → 归集进来（M5-03）
//! GET  /cost-collect-sheet/{id}/finish-preview→ 完工结转试算（M5-04）
//! POST /cost-collect-sheet/{id}/finish        → 把累计成本结转出去（M5-04）
//! ```
//!
//! 前端一个 api 文件、一套路径变量就够。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_batch_collect_handler` 同款：axum handler 要 `AuthClaims` 才能跑，单测调不到；
//! 把「校验 + 取数 + 装配」收进 [`cost_batch_finish::preview`] / [`cost_batch_finish::finish`]
//! 之后，真库用例走**同一条代码路径**（只跳过认证）。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;

use crate::business::erp::cost::cost_batch_finish;
use crate::business::erp::cost::cost_batch_finish_dto::{
    self as dto, BatchFinishPreviewRes, BatchFinishQuery, BatchFinishReq, BatchFinishRes,
};
use cloudpivot_kernel::utils;

// ==================== 权限码（复用 · 见 dto 模块头） ====================

/// 读结转试算（**复用** `cost-object:list`）。
pub const PERM_READ: &str = dto::PERM_READ;

/// 执行结转（**复用** `cost-period:calc`）。
pub const PERM_FINISH: &str = dto::PERM_FINISH;

/// 本模块全部权限码。
pub const FINISH_PERMISSIONS: [&str; 2] = dto::PERMISSIONS;

// ==================== HTTP 入口 ====================

/// 本批的完工结转试算（累计归集 / 已结转 / 本次可转 / 单位成本 / 拦截原因）。
#[nik::permission(
    path = "/business/erp/cost-collect-sheet/{object_id}/finish-preview",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-collect-sheet/{object_id}/finish-preview")]
pub async fn cost_batch_finish_preview_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(object_id): Path<String>,
    Query(query): Query<BatchFinishQuery>,
) -> ResResult<BatchFinishPreviewRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(
        cost_batch_finish::preview(&principal, &object_id, query).await?,
    ))
}

/// 执行完工批次结转（把本批累计归集的料 / 工 / 费差额写进结转单）。
#[nik::permission(
    path = "/business/erp/cost-collect-sheet/{object_id}/finish",
    method = "POST",
    permission = "business:erp:cost-period:calc"
)]
#[nik::post("/business/erp/cost-collect-sheet/{object_id}/finish")]
pub async fn cost_batch_finish_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(object_id): Path<String>,
    Json(req): Json<BatchFinishReq>,
) -> ResResult<BatchFinishRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(
        cost_batch_finish::finish(&principal, operator, &object_id, req).await?,
    ))
}

/// 权限码自检出口（与 `handler::all_permissions` 同形）。
pub fn all_permissions() -> Vec<&'static str> {
    FINISH_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_are_the_two_reused_codes() {
        assert_eq!(PERM_READ, "business:erp:cost-object:list");
        assert_eq!(PERM_FINISH, "business:erp:cost-period:calc");
        assert_eq!(
            all_permissions(),
            vec!["business:erp:cost-object:list", "business:erp:cost-period:calc"]
        );
    }

    /// 权限码常量必须与路由宏里写的字面量逐字一致（改一处忘改另一处 ⇒ 线上 403 或越权）。
    #[test]
    fn route_macros_declare_the_same_permission_literals() {
        let source = include_str!("cost_batch_finish_handler.rs");
        for perm in FINISH_PERMISSIONS {
            let line = format!("permission = \"{perm}\"");
            assert!(source.contains(&line), "路由宏里找不到 `{line}`");
        }
        assert_eq!(
            source.matches("permission = \"business:erp:cost-object:list\"").count(),
            1
        );
        assert_eq!(
            source.matches("permission = \"business:erp:cost-period:calc\"").count(),
            1
        );
    }

    /// 两条路由的方法必须一读一写（GET 只试算、POST 才写库）。
    #[test]
    fn route_methods_are_get_for_preview_and_post_for_finish() {
        let source = include_str!("cost_batch_finish_handler.rs");
        assert!(source.contains("#[nik::get(\"/business/erp/cost-collect-sheet/{object_id}/finish-preview\")]"));
        assert!(source.contains("#[nik::post(\"/business/erp/cost-collect-sheet/{object_id}/finish\")]"));
        assert!(!source.contains("#[nik::post(\"/business/erp/cost-collect-sheet/{object_id}/finish-preview\")]"));
        assert!(!source.contains("#[nik::get(\"/business/erp/cost-collect-sheet/{object_id}/finish\")]"));
    }
}
