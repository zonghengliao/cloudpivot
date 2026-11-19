//! 在制品「料工费在完工与在制之间分配」的**接口层**（成本核算 · M5-22）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-wip-alloc`（AGENTS 约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 1 | GET | `/business/erp/cost-wip-alloc` | `business:erp:cost-object:list` | 期间内逐成本对象的料 / 工 / 费在完工与在制之间的分配（约当产量法） |
//!
//! # 一条路由，为什么没有第二个
//!
//! 这张表一屏就够看：一行 = 一个成本对象，三个要素各给「期初 / 投入 / 池 / 分配率 /
//! 完工 / 在制」。它把 M5-20 的分母（约当产量）与本期的钱**对上**，是 M5-23（期末在制余额）
//! 与 M5-25（前端在制品页）的数据源。写路径不存在：分配额是 calc 的产物，本表只重算 + 对拍。
//!
//! # 权限码：复用 `cost-object:list`（零新增码、零迁移、零菜单）
//!
//! 与 `cost-wip-equiv`（M5-20）/ `cost-collect-sheet`（M5-02）/ `wip-balance`（M5-05）同码：
//! 它们读的是同一批 `biz_cost_object`。新开 `cost-wip:list` 会带来两个问题：① 没有菜单行
//! 承载 ⇒ 死码（`permission_audit_tests::n14` 判红）；② 先插一条指向不存在组件的菜单行 ⇒
//! `n08` 判红。页面码 / 接口码解耦（PERM-01）只在 HR 试点，成本域另开卡。
//!
//! # 数据权限
//!
//! 行由 `biz_cost_object.workshop_dept_id` 裁剪（与约当产量表 / 归集表同一锚点）：
//! 车间主任只看本车间，超管 / 财务看全公司。看不见的对象**不出现在表里**，也不报 403。
//!
//! # 只读
//!
//! 一条只读查询 + 纯内存算数：任一步失败就整请求 500，没有中间态
//! （AGENTS 约定 11 的「提交后失败」在这里没有面）。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_wip_equiv_handler` / `cost_collect_sheet_handler` 同款：axum handler 要
//! `AuthClaims` 才能跑，单测调不到；把「归一化 + 取数 + 算 + 分页 + 装配」收进
//! [`alloc_table_of`] 之后，真库用例走**同一条代码路径**（只跳过认证）。

use axum::extract::Query;
use nik::prelude::*;

use crate::business::erp::cost::cost_calc_overhead;
use crate::business::erp::cost::cost_labor_dto;
use crate::business::erp::cost::cost_period_item_model;
use crate::business::erp::cost::cost_wip_alloc_dto::{
    self as dto, AllocFilter, AllocQuery, AllocRes, AllocStateFilter, AllocTotals,
};
use crate::business::erp::cost::cost_wip_alloc_model as model;
use crate::business::erp::model_base::normalize_page;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::utils;

// ==================== 权限码 ====================

/// 表读取（**复用** `cost-object:list`，理由见模块头）。
pub const PERM_LIST: &str = dto::PERM_LIST;

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形）。
pub const ALLOC_PERMISSIONS: [&str; 1] = dto::PERMISSIONS;

// ==================== 入参归一化 ====================

/// 归一化入参（纯逻辑，单测直接喂 [`AllocQuery`]）。
///
/// 期间必须显式给：期初在制按期间结转，选错期间等于把上期的在制当本期分钱；
/// 而且「默认上个月」会让车间看错月份还不自知。
pub fn filter_of(query: &AllocQuery) -> std::result::Result<AllocFilter, ApiError> {
    let period_code = cost_labor_dto::clean_optional(query.period_code.clone())
        .ok_or_else(|| dto::bad_request("period_code（yyyy-MM）不能为空：料工费按期间分配，例如 2026-11"))?;
    cost_calc_overhead::ensure_period_code(&period_code)?;
    let state = AllocStateFilter::parse(query.state.as_deref())?;
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    Ok(AllocFilter {
        period_code,
        keyword: cost_labor_dto::clean_optional(query.keyword.clone()),
        state,
        page,
        page_size,
    })
}

// ==================== 编排 ====================

/// 在制品分配表（一条只读报表：当期批次 + 取数 + 算 + 筛 + 分页 + 整表合计）。
pub async fn alloc_table_of(
    principal: &ScopedPrincipal,
    filter: &AllocFilter,
) -> std::result::Result<AllocRes, ApiError> {
    // ① 当期活跃批次（`REVERSED` / `CANCELLED` / 软删不占位，与 uk_biz_cost_period 同口径）
    let batch = cost_period_item_model::active_batch(db(), &filter.period_code)
        .await
        .map_err(|error| utils::db_error(error, "取当期成本核算批次"))?;
    let batch_id = batch.as_ref().map(|row| row.id).unwrap_or_else(nik::uuid::Uuid::nil);

    // ② 取数（数据范围 + 关键字已在 SQL 里收窄；期间出现在三个派生表）
    let rows = model::rows_of(principal, filter, batch_id)
        .await
        .map_err(|error| utils::db_error(error, "查询在制品分配表"))?;

    // ③ 算 + 筛 + 合计（整表，不是当前页）
    let mut totals = AllocTotals::default();
    let mut kept: Vec<(model::SourceRow, dto::AllocCalc)> = Vec::with_capacity(rows.len());
    for row in rows {
        let calc = dto::alloc_of(&row.facts);
        if !filter.state.matches(&calc) {
            continue;
        }
        totals.add(&calc);
        kept.push((row, calc));
    }

    // ④ 分页（升序来自 SQL：`ORDER BY t.object_no`；页码越界给空页，不报错）
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
    let list: Vec<dto::AllocRow> = kept
        .get(start..end)
        .unwrap_or(&[])
        .iter()
        .map(|(row, calc)| dto::row_view(&row.head, &row.facts, calc))
        .collect();

    // ⑤ 页脚说明：把「没批次 / 算不出来 / 要不要重算」三件事说清楚
    let cost_ready = batch.is_some();
    let mut note = String::new();
    if !cost_ready {
        note.push_str(&format!(
            "{p} 还没有成本核算批次：本表是**试算视图**（按成本对象当前的完工数量 / 期末在制 / \
             完工率 + 本期归集算），与计算单对拍一列为空。建批次并执行计算\
             （POST /business/erp/cost-period/calc）后回来，就能看到「表值 vs 计算单」逐字段对拍。",
            p = filter.period_code
        ));
    }
    if totals.blocked_row_count > 0 {
        note.push_str(&format!(
            "有 {} 行分不出来（缺完工率 / 本期无产量 / 投料方式不支持 / 期初还原失败 / 出现负数分配）：\
             它们的金额给空值而不是 0，请按每行的「怎么修」处理后再重算。",
            totals.blocked_row_count
        ));
    }
    if totals.drift_row_count > 0 {
        note.push_str(&format!(
            "有 {} 行与本期计算单不一致（算过之后又改了数量 / 完工率 / 归集）：\
             state=drift 可一屏筛出；重新执行期间计算后两侧一致（已结算的需红冲后重算）。",
            totals.drift_row_count
        ));
    }
    note.push_str(dto::FORMULA_SPLIT);
    note.push_str(
        " 分配率按未舍入的约当算（与期间计算同一分母），表里约当显示 4 位，\
                   所以用显示值回算分配率可能在第 6 位差 1 —— 这是计算单本身的口径。",
    );

    Ok(AllocRes {
        period_code: filter.period_code.clone(),
        batch: batch.map(|row| dto::BatchView {
            id: row.id.to_string(),
            batch_no: row.batch_no,
            status: row.status,
        }),
        cost_ready,
        state: filter.state.as_str().to_string(),
        keyword: filter.keyword.clone(),
        formula_pool: dto::FORMULA_POOL.to_string(),
        formula_rate: dto::FORMULA_RATE.to_string(),
        formula_split: dto::FORMULA_SPLIT.to_string(),
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

/// 在制品分配表（期间内逐成本对象：料 / 工 / 费在完工与在制之间的分配）。
#[nik::permission(
    path = "/business/erp/cost-wip-alloc",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-wip-alloc")]
pub async fn cost_wip_alloc_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<AllocQuery>,
) -> ResResult<AllocRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    let filter = filter_of(&query)?;
    Ok(Res::ok(alloc_table_of(&principal, &filter).await?))
}

/// 权限码自检出口（与其它 ERP 模块同形）。
pub fn all_permissions() -> Vec<&'static str> {
    ALLOC_PERMISSIONS.to_vec()
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn query() -> AllocQuery {
        AllocQuery {
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
    fn state_typo_is_rejected() {
        let mut q = query();
        q.state = Some("wipp".to_string());
        let error = filter_of(&q).expect_err("拼错必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("wipp"));
    }

    #[test]
    fn defaults_are_page_one_and_twenty() {
        let filter = filter_of(&query()).expect("缺省可解析");
        assert_eq!(filter.period_code, "2026-11");
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, 20);
        assert_eq!(filter.state, AllocStateFilter::All);
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
        assert_eq!(filter.state, AllocStateFilter::Wip);
    }
}
