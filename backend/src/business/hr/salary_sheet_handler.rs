//! 薪资单接口层（`biz_salary_sheet` · HR-36）：设计 §5.2 的 #36~#40 五条接口。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 落地 |
//! |:--:|---|---|---|---|
//! | 36 | GET | `/business/hr/salary/page` | `business:hr:salary:list` | 列表（期间 / 状态 / 金额合计） |
//! | 37 | GET | `/business/hr/salary/{id}` | `business:hr:salary:detail` | **单头汇总**（单头 + 部门汇总 + 三口径对账） |
//! | 38 | GET | `/business/hr/salary/{id}/detail/page` | `business:hr:salary:detail` | 明细分页（含明细项展开） |
//! | 39 | POST | `/business/hr/salary/calc` | `business:hr:salary:calc` | 试算（不落库） |
//! | 40 | POST | `/business/hr/salary` | `business:hr:salary:calc` | 建单并落库 |
//!
//! 数据层在 [`crate::business::hr::salary_sheet_model`]（唯一 SQL 出口），入参 / 回执契约与
//! 对账纯函数在 [`crate::business::hr::salary_sheet_dto`]，计算本身在
//! [`crate::business::hr::salary_compute`]（HR-34）。
//!
//! # 本卡为什么不写业务编排
//!
//! 「建单」= 引擎的一次 `dry_run = false` 计算，事务边界、恒等式校验、单号取号、状态流转
//! 全在 HR-34 的 [`salary_compute::calc`] 里。本文件**只做三件事**：
//!
//! 1. 把 HTTP 入参解析成 `CalcRequest`（形状 / 域值 / `dry_run` 与接口身份一致）；
//! 2. 调引擎并把 `CalcOutcome` 摊平成回执；
//! 3. 把落库后的东西读出来（列表 / 单头汇总 / 明细分页）。
//!
//! 在 handler 里再算一遍工资，就是「两套公式」——改动一侧漏另一侧，是 HR 模块最贵的 bug。
//!
//! # #37 「单头汇总」为什么不内嵌明细分页
//!
//! 设计 §5.2 #37 的描述是「单头 + 部门汇总 + 明细分页」，但 #38 已经是一条独立的分页接口。
//! 一页 200 人的明细塞进详情响应里，前端要么不用（白传），要么两套分页状态互相打架。
//! 所以本卡把 #37 收成**纯汇总**（单头全列 + 部门汇总 + 三口径对账），明细分页留给 #38，
//! 与设计 §5.3「列表走唯一分页入口」同一取向。
//!
//! # 汇总与明细一致（本卡验收项）
//!
//! [`summary_of`] 每次读都做三方对拍：单头 7 个金额列 vs `SUM(明细)` vs `SUM(明细项按类型)`。
//! 不一致**不报 5xx**：这是**诊断信息**（界面要能打开单据看「哪几个字段漂了」，
//! 而不是整个详情页 500），所以回执里带 `consistent=false` + 逐字段 `mismatches[]`。
//! 但**测试**里必须为 `true` —— 建单后立刻读汇总，三份口径必须一分不差。
//!
//! 为什么不能只靠库层 `CHECK`：`ck_biz_sal_sheet_*` 保证单头内部自洽、`ck_biz_sal_det_*`
//! 保证明细行内部自洽，**跨表**（明细加起来是否等于单头）没有任何约束能管 —— 那是设计
//! §4.9 的巡检 A6/A7，只能在读时（本接口）与离线（HR-45 脚本）各守一道。
//!
//! # 错误出口
//!
//! 只有 `ResResult<T>`：handler 内 `DbError` 直接 `?`（`utils::db_error` 只在要换文案时用），
//! 业务态冲突复用 `HR_SALARY_RULE_BLOCKED(68008)` —— 本卡不新增错误码（加码要动注册表与
//! `docs/`，不在本卡范围）。

use axum::Json;
use axum::extract::{Extension, Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::salary_compute::{self as compute, CalcRequest};
use crate::business::hr::salary_sheet_dto::{
    self as dto, CalcMode, SalaryCalcRes, SalaryDetailPageQuery, SalaryDetailRes, SalarySheetCalcReq,
    SalarySheetListQuery, SalarySheetSummaryRes, SalaryTotalsRes,
};
use crate::business::hr::salary_sheet_model::{self as model};
use crate::business::shared::state_machine::Operator;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0387 的 sys_menu.auth_code 逐字一致） ====================

/// 薪资单列表。
pub const PERM_SALARY_LIST: &str = "business:hr:salary:list";

/// 薪资单详情 / 明细分页（设计 §5.2 #37 / #38 共用这一个码）。
pub const PERM_SALARY_DETAIL: &str = "business:hr:salary:detail";

/// 试算 / 建单 / 明细覆盖（设计 §5.2 #39 / #40 共用）。
pub const PERM_SALARY_CALC: &str = "business:hr:salary:calc";

// ==================== 错误原语 ====================

/// `404 + 10002`：单不存在（或已软删）。
///
/// 报文里带 id：前端「刷新后仍打不开」时，用户报的 id 能直接被运维对上。
fn not_found(sheet_id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("薪资单 {sheet_id} 不存在（可能已删除，或 id 抄错）"),
    )
}

/// `500 + 10004`：引擎退化（落库路径没带回单头）。
///
/// 这条理论上不可达（`dry_run=false` 时引擎必回 `Some(sheet)`）。之所以写成显式 500 而不是
/// `unwrap`：真出现时用户看到的是「建单没拿到单号，请重试」，而不是进程 panic 的 502。
fn lost_sheet() -> ApiError {
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        "建单已执行但没拿到单头信息：请刷新列表确认是否已生成，未生成则重试",
    )
}

// ==================== ㊱ 列表 ====================

/// 薪资单列表（设计 §5.2 #36）。
#[nik::permission(
    path = "/business/hr/salary/page",
    method = "GET",
    permission = "business:hr:salary:list"
)]
#[nik::get("/business/hr/salary/page")]
pub async fn salary_page(
    Query(query): Query<SalarySheetListQuery>,
) -> ResResult<PageData<Vec<dto::SalarySheetCardRes>>> {
    let (list, total, page, page_size) = list_sheets(&query).await?;
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 列表的业务实现（接口与真库用例共用同一条路径）。
pub async fn list_sheets(
    query: &SalarySheetListQuery,
) -> std::result::Result<(Vec<dto::SalarySheetCardRes>, i64, u64, u64), ApiError> {
    let (page, page_size) = dto::normalize_page(query.page, query.page_size);
    let filter = dto::parse_list_query(query)?;
    let (rows, total) = model::page_sheets(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询薪资单"))?;
    Ok((
        rows.iter().map(model::SheetRow::to_card).collect(),
        total,
        page,
        page_size,
    ))
}

// ==================== ㊲ 单头汇总 ====================

/// 单头汇总（设计 §5.2 #37：单头 + 部门汇总 + 三口径对账）。
#[nik::permission(
    path = "/business/hr/salary/{id}",
    method = "GET",
    permission = "business:hr:salary:detail"
)]
#[nik::get("/business/hr/salary/{id}")]
pub async fn salary_summary(Path(id): Path<String>) -> ResResult<SalarySheetSummaryRes> {
    let sheet_id = dto::parse_uuid(&id, "id")?;
    Ok(Res::ok(summary_of(sheet_id).await?))
}

/// 汇总的业务实现。
///
/// 三次读（明细合计 / 明细项合计 / 部门汇总）+ 一次单头读，全部按 `sheet_id` 走索引
/// （`uk_biz_sal_det` 的前缀、`idx_biz_sal_ditem_sheet`、`idx_biz_sal_det_dept`）。
pub async fn summary_of(sheet_id: Uuid) -> std::result::Result<SalarySheetSummaryRes, ApiError> {
    let sheet = model::find_sheet(sheet_id)
        .await
        .map_err(|error| utils::db_error(error, "查询薪资单"))?
        .ok_or_else(|| not_found(sheet_id))?;

    let detail_calc = model::totals_of_details(sheet_id)
        .await
        .map_err(|error| utils::db_error(error, "汇总薪资明细"))?;
    let item_totals = model::item_totals(sheet_id)
        .await
        .map_err(|error| utils::db_error(error, "汇总薪资明细项"))?;
    let dept_summary = model::dept_summary(sheet_id)
        .await
        .map_err(|error| utils::db_error(error, "汇总薪资单的部门合计"))?;

    let header_totals = sheet.totals();
    let detail_totals = SalaryTotalsRes::of_details(&detail_calc);
    let mut mismatches = dto::diff_totals(&header_totals, &detail_totals);
    mismatches.extend(dto::diff_item_totals(&detail_totals, &item_totals));

    Ok(SalarySheetSummaryRes {
        sheet: sheet.to_header(),
        detail_totals,
        item_totals,
        consistent: mismatches.is_empty(),
        mismatches,
        dept_summary,
    })
}

// ==================== ㊳ 明细分页 ====================

/// 明细分页（设计 §5.2 #38）。
#[nik::permission(
    path = "/business/hr/salary/{id}/detail/page",
    method = "GET",
    permission = "business:hr:salary:detail"
)]
#[nik::get("/business/hr/salary/{id}/detail/page")]
pub async fn salary_detail_page(
    Path(id): Path<String>,
    Query(query): Query<SalaryDetailPageQuery>,
) -> ResResult<PageData<Vec<SalaryDetailRes>>> {
    let sheet_id = dto::parse_uuid(&id, "id")?;
    let (list, total, page, page_size) = detail_page_of(sheet_id, &query).await?;
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 明细分页的业务实现（接口与真库用例共用同一条路径）。
///
/// 单头不存在 → 404，**不给空页**：空页会让用户以为「这单一个人都没有」，
/// 而真实原因是 id 错了（或者单被软删了）。
pub async fn detail_page_of(
    sheet_id: Uuid,
    query: &SalaryDetailPageQuery,
) -> std::result::Result<(Vec<SalaryDetailRes>, i64, u64, u64), ApiError> {
    model::find_sheet(sheet_id)
        .await
        .map_err(|error| utils::db_error(error, "查询薪资单"))?
        .ok_or_else(|| not_found(sheet_id))?;

    let filter = dto::parse_detail_query(sheet_id, query)?;
    let (page, page_size) = dto::normalize_page(query.page, query.page_size);
    let (rows, total) = model::page_details(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询薪资明细"))?;

    // 明细项按本页的 detail_id 一次查完（不发 N 次）
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let grouped = if filter.with_items {
        model::items_of_details(sheet_id, &ids)
            .await
            .map_err(|error| utils::db_error(error, "查询薪资明细项"))?
    } else {
        std::collections::HashMap::new()
    };

    let list = rows
        .iter()
        .map(|row| {
            let items = grouped
                .get(&row.id)
                .map(|items| items.iter().map(model::DetailItemRow::to_res).collect())
                .unwrap_or_default();
            row.to_res(items)
        })
        .collect();
    Ok((list, total, page, page_size))
}

// ==================== ㊴ 试算 ====================

/// 试算（设计 §5.2 #39：算完回滚，库里零行、号段不推进）。
#[nik::permission(
    path = "/business/hr/salary/calc",
    method = "POST",
    permission = "business:hr:salary:calc"
)]
#[nik::post("/business/hr/salary/calc")]
pub async fn salary_calc(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<SalarySheetCalcReq>,
) -> ResResult<SalaryCalcRes> {
    let operator: Operator = (&claims).into();
    Ok(Res::ok(preview(operator, &req).await?))
}

/// 试算的业务实现（接口与真库用例共用同一条路径）。
pub async fn preview(operator: Operator, req: &SalarySheetCalcReq) -> std::result::Result<SalaryCalcRes, ApiError> {
    let request = dto::parse_calc_req(req, CalcMode::Preview)?;
    let outcome = compute::calc(operator, &request).await?;
    // 试算回执带出单头就是 bug（说明引擎没回滚或调用方传错了 dry_run）
    if outcome.sheet.is_some() {
        return Err(lost_sheet());
    }
    Ok(SalaryCalcRes::of(&outcome))
}

// ==================== ㊵ 建单 ====================

/// 建单并落库（设计 §5.2 #40：同一次计算的 `dry_run = false`）。
#[nik::permission(
    path = "/business/hr/salary",
    method = "POST",
    permission = "business:hr:salary:calc"
)]
#[nik::post("/business/hr/salary")]
pub async fn salary_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<SalarySheetCalcReq>,
) -> ResResult<SalaryCalcRes> {
    let operator: Operator = (&claims).into();
    Ok(Res::ok(persist(operator, &req).await?))
}

/// 建单的业务实现（接口与真库用例共用同一条路径）。
///
/// 事务边界、单号、状态流转、恒等式校验全部在 [`compute::calc`] 里（HR-34）；
/// 这里只保证「落库路径必须回单头」。
pub async fn persist(operator: Operator, req: &SalarySheetCalcReq) -> std::result::Result<SalaryCalcRes, ApiError> {
    let request: CalcRequest = dto::parse_calc_req(req, CalcMode::Persist)?;
    let outcome = compute::calc(operator, &request).await?;
    if outcome.sheet.is_none() {
        return Err(lost_sheet());
    }
    Ok(SalaryCalcRes::of(&outcome))
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::hr::handler::ROUTE_CONTRACT;

    /// 本卡落地的五条路由必须在 `ROUTE_CONTRACT`（设计 §5.2 的冻结表）里逐字存在，
    /// 且权限码与声明一致 —— 路径 / 权限码写错的直接后果是「菜单给了、点进去 403/404」。
    #[test]
    fn routes_and_permissions_match_the_frozen_contract() {
        let expected = [
            ("GET", "/business/hr/salary/page", PERM_SALARY_LIST),
            ("GET", "/business/hr/salary/{id}", PERM_SALARY_DETAIL),
            ("GET", "/business/hr/salary/{id}/detail/page", PERM_SALARY_DETAIL),
            ("POST", "/business/hr/salary/calc", PERM_SALARY_CALC),
            ("POST", "/business/hr/salary", PERM_SALARY_CALC),
        ];
        for (method, path, permission) in expected {
            let spec = ROUTE_CONTRACT
                .iter()
                .find(|spec| spec.method == method && spec.path == path)
                .unwrap_or_else(|| panic!("契约表里没有 {method} {path}"));
            assert_eq!(
                spec.permission,
                Some(permission),
                "{method} {path} 的权限码与契约表不一致"
            );
        }
    }

    /// 三条权限码必须与 `V0387` 种进 `sys_menu.auth_code` 的字符串逐字一致。
    #[test]
    fn permission_codes_exist_in_the_menu_seed() {
        const V0387: &str = include_str!("../../../migrations/business/V0387__hr_menu_perms.up.sql");
        for code in [PERM_SALARY_LIST, PERM_SALARY_DETAIL, PERM_SALARY_CALC] {
            assert!(V0387.contains(code), "V0387 没有种权限码 {code}");
        }
    }

    /// 404 报文点名 id（用户报障时能直接对上单）。
    #[test]
    fn not_found_names_the_sheet() {
        let id = Uuid::now_v7();
        let error = not_found(id);
        assert_eq!(error.status, 404);
        assert_eq!(error.code, error_code::RESOURCE_NOT_FOUND);
        assert!(error.msg.contains(&id.to_string()), "报文：{}", error.msg);
    }
}
