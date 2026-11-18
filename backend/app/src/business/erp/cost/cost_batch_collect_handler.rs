//! 分批法「料工费归集到批次」· 接口层（成本核算 · M5-03【M5·成本】分批法 ③）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-collect-sheet/...`（约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 写库 |
//! |---|---|---|---|---|
//! | 1 | GET | `/business/erp/cost-collect-sheet/{object_id}/sources?period_code=yyyy-MM` | `business:erp:cost-object:list` | ❌ 预览 |
//! | 2 | POST | `/business/erp/cost-collect-sheet/{object_id}/collect` | `business:erp:cost-period:calc` | ✅ 归集 |
//!
//! # 为什么挂在 `cost-collect-sheet` 这个前缀下
//!
//! 与 M5-02 的归集表/明细同一族资源（`biz_cost_collect`），路径语义连贯：
//!
//! ```text
//! GET  /cost-collect-sheet                     → 一批一行（本期料工费合计）
//! GET  /cost-collect-sheet/{id}                → 这一批的逐笔归集行（已经归集进来的）
//! GET  /cost-collect-sheet/{id}/sources        → 这一批的三路来源（含「还没归集进来的」）
//! POST /cost-collect-sheet/{id}/collect        → 把该进来的归集进来
//! ```
//!
//! 前两条只读（M5-02），后两条是 M5-03。这样前端一个 api 文件、一套路径变量就够。
//!
//! # 权限码为什么复用的两个不同码
//!
//! * 预览只读 → `cost-object:list`（与 M5-02 同码：同一批数据，不多露一分）；
//! * 归集写库 → `cost-period:calc`（**语义就是「算成本」**：calc 第 1/2/3 段做的正是
//!   这件事，本接口只是收窄到单批。不给新码 —— 新码要种 `sys_menu`，而页面属 M5-07，
//!   插一条指向不存在组件的菜单会让侧栏点开 404）。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_collect_sheet_handler` / `cost_object_handler` 同款：axum handler 要
//! `AuthClaims` 才能跑，单测调不到；把「校验 + 取数 + 装配」收进
//! [`cost_batch_collect::preview`] / [`cost_batch_collect::collect`] 之后，
//! 真库用例走**同一条代码路径**（只跳过认证）。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;

use crate::business::erp::cost::cost_batch_collect;
use crate::business::erp::cost::cost_batch_collect_dto::{
    self as dto, BatchCollectReq, BatchCollectRes, BatchSourceQuery, BatchSourcesRes,
};
use cloudpivot_kernel::utils;

// ==================== 权限码（复用 · 见模块头） ====================

/// 读三路来源（**复用** `cost-object:list`）。
pub const PERM_READ: &str = dto::PERM_READ;

/// 执行归集（**复用** `cost-period:calc`）。
pub const PERM_COLLECT: &str = dto::PERM_COLLECT;

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形，便于汇总脚本收集）。
pub const BATCH_PERMISSIONS: [&str; 2] = dto::PERMISSIONS;

// ==================== HTTP 入口 ====================

/// 本批本期的三路来源预览（料 / 工 / 费：哪几笔、哪些已归集、哪些归不了）。
#[nik::permission(
    path = "/business/erp/cost-collect-sheet/{object_id}/sources",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-collect-sheet/{object_id}/sources")]
pub async fn cost_batch_sources_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(object_id): Path<String>,
    Query(query): Query<BatchSourceQuery>,
) -> ResResult<BatchSourcesRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(
        cost_batch_collect::preview(&principal, &object_id, query).await?,
    ))
}

/// 把本批本期的料 / 工 / 费归集进 `biz_cost_collect`（幂等）。
#[nik::permission(
    path = "/business/erp/cost-collect-sheet/{object_id}/collect",
    method = "POST",
    permission = "business:erp:cost-period:calc"
)]
#[nik::post("/business/erp/cost-collect-sheet/{object_id}/collect")]
pub async fn cost_batch_collect_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(object_id): Path<String>,
    Json(req): Json<BatchCollectReq>,
) -> ResResult<BatchCollectRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(
        cost_batch_collect::collect(&principal, operator, &object_id, req).await?,
    ))
}

/// 权限码自检出口（与 `handler::all_permissions` 同形）。
pub fn all_permissions() -> Vec<&'static str> {
    BATCH_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_are_the_two_reused_codes() {
        assert_eq!(PERM_READ, "business:erp:cost-object:list");
        assert_eq!(PERM_COLLECT, "business:erp:cost-period:calc");
        assert_eq!(
            all_permissions(),
            vec!["business:erp:cost-object:list", "business:erp:cost-period:calc"]
        );
    }

    /// 权限码常量必须与路由宏里写的字面量逐字一致（改一处忘改另一处 ⇒ 线上 403 或越权）。
    ///
    /// 这条用例扫自己的源码文本：宏参是编译期属性，运行时拿不到，只能比对文本。
    #[test]
    fn route_macros_declare_the_same_permission_literals() {
        let source = include_str!("cost_batch_collect_handler.rs");
        for perm in BATCH_PERMISSIONS {
            let line = format!("permission = \"{perm}\"");
            assert!(source.contains(&line), "路由宏里找不到 `{line}`");
        }
        // 两条路由各一次（`sources` 读 + `collect` 写），多一条少一条都要在这条上炸
        assert_eq!(
            source.matches("permission = \"business:erp:cost-object:list\"").count(),
            1
        );
        assert_eq!(
            source.matches("permission = \"business:erp:cost-period:calc\"").count(),
            1
        );
    }

    /// 两条路由的方法必须一读一写（GET 只读、POST 才写库）：路径相同、方法写错是最典型的
    /// 「看起来能点、其实 405」来源。
    #[test]
    fn route_methods_are_get_for_preview_and_post_for_collect() {
        let source = include_str!("cost_batch_collect_handler.rs");
        assert!(source.contains("#[nik::get(\"/business/erp/cost-collect-sheet/{object_id}/sources\")]"));
        assert!(source.contains("#[nik::post(\"/business/erp/cost-collect-sheet/{object_id}/collect\")]"));
        assert!(!source.contains("#[nik::post(\"/business/erp/cost-collect-sheet/{object_id}/sources\")]"));
        assert!(!source.contains("#[nik::get(\"/business/erp/cost-collect-sheet/{object_id}/collect\")]"));
    }
}
