//! 在制品「约当产量表」的**接口层**（成本核算 · M5-20）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-wip-equiv`（AGENTS 约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 1 | GET | `/business/erp/cost-wip-equiv` | `business:erp:cost-object:list` | 期间内逐成本对象的约当产量（完工率 × 数量 + 整表合计） |
//!
//! # 一条路由，为什么没有第二个
//!
//! 这张表一屏就够看：一行 = 一个成本对象（一个生产批），列 = 完工数量 / 期末在制 / 完工率 /
//! 两个约当产量 / 与计算单是否一致。钻取（某对象的工序级完工率）属 M5-21；
//! 「料工费怎么在完工与在制之间分」属 M5-22（金额侧）；「期末在制余额」属 M5-23。
//! 本卡**只出分母**（约当产量），一分钱都不分。
//!
//! # 权限码：复用 `cost-object:list`（零新增码、零新增迁移）
//!
//! 与 `cost-collect-sheet`（M5-02）/ `cost-collect-sheet/wip-balance`（M5-05）同码：
//! 它们读的是同一批 `biz_cost_object`。新开 `cost-wip:list` 会带来两个问题：① 没有菜单行
//! 承载 ⇒ 死码（`permission_audit_tests::n14` 判红）；② 先插一条指向不存在组件的菜单行 ⇒
//! `n08` 判红。页面码 / 接口码解耦（PERM-01）只在 HR 试点，成本域另开卡。
//!
//! # 数据权限
//!
//! 行由 `biz_cost_object.workshop_dept_id` 裁剪（与归集表同一锚点）：车间主任只看本车间，
//! 超管 / 财务看全公司。看不见的对象**不出现在表里**，也不报 403（列表没有「探测口」问题）。
//!
//! # 只读
//!
//! 一条只读查询 + 纯内存算数：任一步失败就整请求 500，没有中间态
//! （AGENTS 约定 11 的「提交后失败」在这里没有面）。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_collect_sheet_handler` / `cost_standard_compare_handler` 同款：axum handler 要
//! `AuthClaims` 才能跑，单测调不到；把「归一化 + 取数 + 算 + 分页 + 装配」收进
//! [`equiv_table_of`] 之后，真库用例走**同一条代码路径**（只跳过认证）。

use axum::extract::Query;
use nik::prelude::*;

use crate::business::erp::cost::cost_calc_overhead;
use crate::business::erp::cost::cost_labor_dto;
use crate::business::erp::cost::cost_period_item_model;
use crate::business::erp::cost::cost_wip_equiv_dto::{
    self as dto, EquivFilter, EquivQuery, EquivRes, EquivStateFilter, EquivTotals,
};
use crate::business::erp::cost::cost_wip_equiv_model as model;
use crate::business::erp::model_base::normalize_page;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::utils;

// ==================== 权限码 ====================

/// 表读取（**复用** `cost-object:list`，理由见模块头）。
pub const PERM_LIST: &str = dto::PERM_LIST;

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形）。
pub const EQUIV_PERMISSIONS: [&str; 1] = dto::PERMISSIONS;

// ==================== 入参归一化 ====================

/// 归一化入参（纯逻辑，单测直接喂 [`EquivQuery`]）。
///
/// 期间必须显式给：跨期间的约当产量没有默认值（默认「上个月」会让车间看错月份还不自知），
/// 而且期初在制是按期间结转的，选错期间等于把上期的在制当本期算。
pub fn filter_of(query: &EquivQuery) -> std::result::Result<EquivFilter, ApiError> {
    let period_code = cost_labor_dto::clean_optional(query.period_code.clone())
        .ok_or_else(|| dto::bad_request("period_code（yyyy-MM）不能为空：约当产量按期间算，例如 2026-11"))?;
    cost_calc_overhead::ensure_period_code(&period_code)?;
    let state = EquivStateFilter::parse(query.state.as_deref())?;
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    Ok(EquivFilter {
        period_code,
        keyword: cost_labor_dto::clean_optional(query.keyword.clone()),
        state,
        page,
        page_size,
    })
}

// ==================== 编排 ====================

/// 约当产量表（一条只读报表：当期批次 + 取数 + 算 + 筛 + 分页 + 整表合计）。
pub async fn equiv_table_of(
    principal: &ScopedPrincipal,
    filter: &EquivFilter,
) -> std::result::Result<EquivRes, ApiError> {
    // ① 当期活跃批次（`REVERSED` / `CANCELLED` / 软删不占位，与 uk_biz_cost_period 同口径）
    let batch = cost_period_item_model::active_batch(db(), &filter.period_code)
        .await
        .map_err(|error| utils::db_error(error, "取当期成本核算批次"))?;
    let batch_id = batch.as_ref().map(|row| row.id).unwrap_or_else(nik::uuid::Uuid::nil);

    // ② 取数（数据范围 + 关键字已在 SQL 里收窄；期间只出现在两个派生表）
    let rows = model::rows_of(principal, filter, batch_id)
        .await
        .map_err(|error| utils::db_error(error, "查询约当产量表"))?;

    // ③ 算 + 筛 + 合计（整表，不是当前页）
    let mut totals = EquivTotals::default();
    let mut kept: Vec<(model::SourceRow, dto::EquivCalc)> = Vec::with_capacity(rows.len());
    for row in rows {
        let facts = row.facts();
        let calc = dto::equiv_of(&facts);
        if !filter.state.matches(&calc) {
            continue;
        }
        totals.add(&facts, &calc);
        kept.push((row, calc));
    }

    // ④ 分页（升序来自 SQL：`ORDER BY o.object_no`；页码越界给空页，不报错）
    let total_rows = kept.len();
    let offset = filter
        .page
        .saturating_sub(1)
        .saturating_mul(filter.page_size)
        .min(u64::try_from(total_rows).unwrap_or(u64::MAX));
    let start = usize::try_from(offset).unwrap_or(total_rows);
    let end = start
        .saturating_add(usize::try_from(filter.page_size).unwrap_or(0))
        .min(total_rows);
    let list: Vec<dto::EquivRow> = kept
        .get(start..end)
        .unwrap_or(&[])
        .iter()
        .map(|(row, calc)| dto::row_view(&row.head(), &row.facts(), calc))
        .collect();

    // ⑤ 页脚说明：把「没批次 / 算不出来 / 要不要重算」三件事说清楚
    let cost_ready = batch.is_some();
    let mut note = String::new();
    if !cost_ready {
        note.push_str(&format!(
            "{p} 还没有成本核算批次：本表是**试算视图**（按成本对象当前的完工数量 / 期末在制 / \
             完工率算），与计算单对拍一列显示为空。建批次并执行计算（POST /business/erp/cost-period/calc）\
             后回来，就能看到「表值 vs 计算单」逐字段对拍。",
            p = filter.period_code
        ));
    }
    if totals.blocked_row_count > 0 {
        note.push_str(&format!(
            "有 {} 行算不出来（缺完工率 / 本期无产量 / 累计完工小于前期已结算）：它们的约当给空值而不是 0，\
             请按每行的「怎么修」处理后再重算。",
            totals.blocked_row_count
        ));
    }
    if totals.drift_row_count > 0 {
        note.push_str(&format!(
            "有 {} 行与本期计算单不一致（算过之后又改了完工数量 / 在制数量 / 完工率）：\
             state=drift 可一屏筛出；重新执行期间计算后两侧一致（已结算的需红冲后重算）。",
            totals.drift_row_count
        ));
    }
    if note.is_empty() {
        note.push_str("全部行与本期的成本对象数据一致，且都能算出约当产量。");
    }

    Ok(EquivRes {
        period_code: filter.period_code.clone(),
        batch: batch.map(|row| dto::BatchView {
            id: row.id.to_string(),
            batch_no: row.batch_no,
            status: row.status,
        }),
        cost_ready,
        state: filter.state.as_str().to_string(),
        keyword: filter.keyword.clone(),
        formula_material: dto::FORMULA_MATERIAL.to_string(),
        formula_conversion: dto::FORMULA_CONVERSION.to_string(),
        formula_completed: dto::FORMULA_COMPLETED.to_string(),
        totals: dto::totals_view(&totals),
        list,
        total: u64::try_from(total_rows).unwrap_or(0),
        page: filter.page,
        page_size: filter.page_size,
        state_options: dto::state_options(),
        note,
    })
}

// ==================== HTTP 入口 ====================

/// 在制品约当产量表（期间内逐成本对象：完工数量 + 期末在制 × 完工率）。
#[nik::permission(
    path = "/business/erp/cost-wip-equiv",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-wip-equiv")]
pub async fn cost_wip_equiv_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<EquivQuery>,
) -> ResResult<EquivRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    let filter = filter_of(&query)?;
    Ok(Res::ok(equiv_table_of(&principal, &filter).await?))
}

/// 权限码自检出口（与其它 ERP 模块同形）。
pub fn all_permissions() -> Vec<&'static str> {
    EQUIV_PERMISSIONS.to_vec()
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn query() -> EquivQuery {
        EquivQuery {
            period_code: Some("2026-11".to_string()),
            keyword: None,
            state: None,
            page: None,
            page_size: None,
        }
    }

    #[test]
    fn permission_reuses_cost_object_list() {
        assert_eq!(PERM_LIST, "business:erp:cost-object:list");
        assert_eq!(all_permissions(), vec!["business:erp:cost-object:list"]);
    }

    #[test]
    fn period_is_required() {
        let mut q = query();
        q.period_code = None;
        let error = filter_of(&q).expect_err("没期间必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("period_code"));

        q.period_code = Some("   ".to_string());
        assert!(filter_of(&q).is_err(), "空白串也算没给");
    }

    #[test]
    fn period_shape_is_validated() {
        let mut q = query();
        q.period_code = Some("2026-13".to_string());
        let error = filter_of(&q).expect_err("13 月必须 400");
        assert_eq!(error.status, 400);
        q.period_code = Some("202611".to_string());
        assert!(filter_of(&q).is_err(), "缺连字符必须 400");
    }

    #[test]
    fn defaults_are_page_one_and_twenty() {
        let filter = filter_of(&query()).expect("缺省可解析");
        assert_eq!(filter.period_code, "2026-11");
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, 20);
        assert_eq!(filter.state, EquivStateFilter::All);
        assert!(filter.keyword.is_none());
    }

    #[test]
    fn page_size_is_capped_and_keyword_trimmed() {
        let mut q = query();
        q.page = Some(0);
        q.page_size = Some(9999);
        q.keyword = Some("  CB-1  ".to_string());
        q.state = Some("wip".to_string());
        let filter = filter_of(&q).expect("合法入参");
        assert_eq!(filter.page, 1, "页码从 1 起");
        assert_eq!(filter.page_size, 200, "上限 200");
        assert_eq!(filter.keyword.as_deref(), Some("CB-1"));
        assert_eq!(filter.state, EquivStateFilter::Wip);
    }
}
