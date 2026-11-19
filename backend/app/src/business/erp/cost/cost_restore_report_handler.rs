//! 成本还原「还原后报表：料工费占比」的**接口层**（成本核算 · M5-28）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-restore-report`（AGENTS 约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 1 | GET | `/business/erp/cost-restore-report` | `business:erp:cost-object:list` | 期间内**还原后报表**：按车间 / 产品 / 成本对象 / 整表汇总料工费占比与结构变化 |
//!
//! # 一条路由，与 M5-26 / M5-27 的关系
//!
//! * **M5-26**（`cost-restore-structure`）：出**结构**（每一层是什么、比例多少）；
//! * **M5-27**（`cost-restore-rollup`）：把结构**卷积成钱**（逐对象的还原后料 / 工 / 费）；
//! * **M5-28**（本条）：把钱**按口径汇总成占比**（车间 / 产品 / 对象 / 整表）。
//!
//! 实现上不重查一遍库、也不抄一遍卷积：直接调 M5-27 的 [`rollup_rows_of`] 拿全量行
//! （结构与卷积都在那一次里做完），这里只做「分组 + 加法 + 占比」。
//! 于是**明细表某几行的金额之和 = 报表同一分组的金额**，两张表不可能吵架。
//!
//! # 数据权限
//!
//! 行仍由 `biz_cost_object.workshop_dept_id` 裁剪（M5-26 取数层做）。
//! 看不见的对象**不出现在报表里**，也不报 403；因此**分组也不会带出看不见的车间**。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与结构表 / 还原表 / 余额表 / 分配表同款：axum handler 要 `AuthClaims` 才能跑，单测调不到；
//! 把「归一化 + 取行 + 分组 + 分页 + 装配」收进 [`report_of`] 之后，真库用例走**同一条代码路径**。

use axum::extract::Query;
use nik::prelude::*;

use crate::business::erp::cost::cost_calc_overhead;
use crate::business::erp::cost::cost_labor_dto;
use crate::business::erp::cost::cost_restore_report_dto::{
    self as dto, GroupBy, GroupRow, ReportFilter, ReportQuery, ReportRes, StateFilter,
};
use crate::business::erp::cost::cost_restore_rollup_dto::state_options;
use crate::business::erp::cost::cost_restore_rollup_handler::rollup_rows_of;
use crate::business::erp::cost::cost_restore_structure_dto::NODE_GUARD;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;

// ==================== 权限码 ====================

/// 表读取（**复用** `cost-object:list`，理由见 dto 模块头）。
pub const PERM_LIST: &str = dto::PERM_LIST;

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形）。
pub const REPORT_PERMISSIONS: [&str; 1] = dto::PERMISSIONS;

// ==================== 入参归一化 ====================

/// 归一化入参（纯逻辑，单测直接喂 [`ReportQuery`]）。
///
/// 期间必须显式给：报表汇总的是「某一期投了什么钱」，默认取当前月等于把别的月份的钱当成本期。
pub fn filter_of(query: &ReportQuery) -> std::result::Result<ReportFilter, ApiError> {
    let period_code = cost_labor_dto::clean_optional(query.period_code.clone())
        .ok_or_else(|| dto::bad_request("period_code（yyyy-MM）不能为空：报表是某一期投料的快照，例如 2026-11"))?;
    cost_calc_overhead::ensure_period_code(&period_code)?;
    let group_by = GroupBy::parse(query.group_by.as_deref())?;
    let state = StateFilter::parse(query.state.as_deref())?;
    // 报表一行一个**分组**（不是一行一个对象）：缺省 50、上限 500 —— 与明细表的 20 / 200 不同，
    // 所以不复用 `normalize_page`（它的默认值与上限是明细表口径）。
    let page = query.page.unwrap_or(1).max(1);
    let page_size = match query.page_size.unwrap_or(0) {
        0 => dto::PAGE_SIZE_DEFAULT,
        size => size.min(dto::PAGE_SIZE_LIMIT),
    };
    let max_depth = match query.max_depth.unwrap_or(dto::MAX_DEPTH_DEFAULT) {
        0 => dto::MAX_DEPTH_DEFAULT,
        value => value.min(dto::MAX_DEPTH_LIMIT),
    };
    Ok(ReportFilter {
        period_code,
        keyword: cost_labor_dto::clean_optional(query.keyword.clone()),
        group_by,
        state,
        page,
        page_size,
        max_depth,
    })
}

// ==================== 报表 ====================

/// 还原后报表（取行 → 分组 → 分页 → 整表合计；一条只读报表）。
pub async fn report_of(principal: &ScopedPrincipal, filter: &ReportFilter) -> std::result::Result<ReportRes, ApiError> {
    // ① 还原表的全量行（结构与卷积全在 M5-27，那边已按数据范围 / 关键字 / 状态筛过）
    let rollup = rollup_rows_of(principal, &filter.rollup_filter()).await?;

    // ② 分组累加 + 整表合计（合计与分组同一份加法，不可能对不上）
    let grouped = dto::aggregate(&rollup.rows, filter.group_by);
    let totals_acc = dto::totals_acc(&rollup.rows);
    let totals = dto::group_view(&totals_acc);
    let group_count = grouped.len();

    // ③ 分页（页码越界给空页，不报错）
    let offset = filter
        .page
        .saturating_sub(1)
        .saturating_mul(filter.page_size)
        .min(u64::try_from(group_count).unwrap_or(u64::MAX));
    let start = usize::try_from(offset).unwrap_or(group_count);
    let end = start
        .saturating_add(usize::try_from(filter.page_size).unwrap_or(0))
        .min(group_count);
    let groups: Vec<GroupRow> = grouped
        .get(start..end)
        .unwrap_or(&[])
        .iter()
        .map(dto::group_view)
        .collect();

    // ④ 页脚说明
    let mut note = String::new();
    if group_count == 0 {
        note.push_str(&format!(
            "{} 没有可汇总的成本对象（该期间没有材料投入，或 keyword / state 把全部行筛掉了）：\
             报表为空、合计为 0，不是接口故障。",
            filter.period_code
        ));
    }
    if !rollup.cost_ready {
        note.push_str(&format!(
            "{} 还没有成本核算批次：行头的「本期完工成本」是空的，还原照做 —— \
             还原只依赖归集行与半成品产出批次的计算单。",
            filter.period_code
        ));
    }
    if totals.partial_rows > 0 {
        note.push_str(&format!(
            "有 {} 个成本对象还原不彻底：这些金额单列成「还原不彻底」一格（不算外购材料、\
             也不摊到人工与制造费用），所以「材料 + 人工 + 费 + 不彻底」仍等于 100.00%；\
             卡在第几层看明细表（cost-restore-rollup）的逐层台账。",
            totals.partial_rows
        ));
    }
    if rollup.overflow {
        note.push_str(&format!(
            "本次展开的节点数到了上限（{NODE_GUARD}）：后面的层没展开完，串到「还原不彻底」里；\
             请用 keyword 收窄对象或调小 max_depth 分段看。"
        ));
    }
    note.push_str(dto::BASIS);
    note.push(' ');
    note.push_str(dto::FORMULA_SHARE);
    note.push(' ');
    note.push_str(dto::FORMULA_SHIFT);

    Ok(ReportRes {
        period_code: filter.period_code.clone(),
        batch: rollup.batch,
        cost_ready: rollup.cost_ready,
        group_by: filter.group_by.as_str().to_string(),
        group_by_label: filter.group_by.label().to_string(),
        group_options: dto::group_options(),
        state: filter.state.as_str().to_string(),
        state_label: filter.state.label().to_string(),
        keyword: filter.keyword.clone(),
        max_depth: filter.max_depth,
        basis: dto::BASIS.to_string(),
        formula_restore: dto::FORMULA_RESTORE.to_string(),
        formula_unresolved: dto::FORMULA_UNRESOLVED.to_string(),
        formula_share: dto::FORMULA_SHARE.to_string(),
        formula_shift: dto::FORMULA_SHIFT.to_string(),
        totals,
        groups,
        total: u64::try_from(group_count).unwrap_or(0),
        page: filter.page,
        page_size: filter.page_size,
        state_options: state_options(),
        note,
    })
}

// ==================== HTTP 入口 ====================

/// 成本还原后报表（按车间 / 产品 / 成本对象 / 整表汇总料工费占比与结构变化）。
#[nik::permission(
    path = "/business/erp/cost-restore-report",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-restore-report")]
pub async fn cost_restore_report_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<ReportQuery>,
) -> ResResult<ReportRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    let filter = filter_of(&query)?;
    Ok(Res::ok(report_of(&principal, &filter).await?))
}

/// 权限码自检出口（与其它 ERP 模块同形）。
pub fn all_permissions() -> Vec<&'static str> {
    REPORT_PERMISSIONS.to_vec()
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn query() -> ReportQuery {
        ReportQuery {
            period_code: Some("2026-11".to_string()),
            keyword: None,
            group_by: None,
            state: None,
            page: None,
            page_size: None,
            max_depth: None,
        }
    }

    #[test]
    fn permission_reuses_cost_object_list() {
        assert_eq!(PERM_LIST, "business:erp:cost-object:list");
        assert_eq!(all_permissions(), vec!["business:erp:cost-object:list"]);
        assert_eq!(dto::PERMISSIONS, ["business:erp:cost-object:list"]);
    }

    #[test]
    fn period_is_required_and_validated() {
        let mut q = query();
        q.period_code = None;
        let error = filter_of(&q).expect_err("没期间必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("period_code"));

        q.period_code = Some("   ".to_string());
        assert!(filter_of(&q).is_err(), "空白串也算没给");

        q.period_code = Some("2026-13".to_string());
        assert_eq!(filter_of(&q).expect_err("13 月必须 400").status, 400);
        q.period_code = Some("202611".to_string());
        assert!(filter_of(&q).is_err(), "缺连字符必须 400");
    }

    #[test]
    fn group_by_defaults_to_dept_and_typo_is_rejected() {
        let filter = filter_of(&query()).expect("缺省可解析");
        assert_eq!(filter.group_by, GroupBy::Dept);
        assert_eq!(filter.state, StateFilter::All);
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, dto::PAGE_SIZE_DEFAULT, "报表缺省一页 50 个分组");
        assert_eq!(filter.max_depth, dto::MAX_DEPTH_DEFAULT);

        let mut q = query();
        q.group_by = Some("  ".to_string());
        assert_eq!(filter_of(&q).expect("空白按缺省").group_by, GroupBy::Dept);

        q.group_by = Some("workshop".to_string());
        let error = filter_of(&q).expect_err("拼错必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("workshop"));
    }

    #[test]
    fn state_and_paging_are_validated() {
        let mut q = query();
        q.state = Some("done".to_string());
        assert_eq!(filter_of(&q).expect_err("拼错必须 400").status, 400);

        q.state = None;
        q.page = Some(0);
        q.page_size = Some(9999);
        q.max_depth = Some(99);
        let filter = filter_of(&q).expect("合法入参");
        assert_eq!(filter.page, 1, "页码从 1 起");
        assert_eq!(filter.page_size, dto::PAGE_SIZE_LIMIT, "上限 500");
        assert_eq!(filter.max_depth, dto::MAX_DEPTH_LIMIT, "层数夹到上限");

        q.max_depth = Some(0);
        assert_eq!(filter_of(&q).expect("0 层按默认").max_depth, dto::MAX_DEPTH_DEFAULT);
    }

    #[test]
    fn rollup_filter_is_unpaged() {
        // 汇总必须基于整表：传给还原层的分页一律 1 / 上限，不能把报表的总计算成「当前页」
        let filter = filter_of(&query()).expect("缺省可解析");
        let rollup = filter.rollup_filter();
        assert_eq!(rollup.page, 1);
        assert_eq!(rollup.page_size, dto::PAGE_SIZE_LIMIT);
        assert_eq!(rollup.period_code, filter.period_code);
        assert_eq!(rollup.state, filter.state);
        assert_eq!(rollup.max_depth, filter.max_depth);
    }

    #[test]
    fn group_options_are_four() {
        let options = dto::group_options();
        assert_eq!(options.len(), 4);
        let values: Vec<&str> = options.iter().map(|item| item.value.as_str()).collect();
        assert_eq!(values, vec!["dept", "product", "object", "all"]);
        assert!(options.iter().all(|item| !item.label.is_empty()));
    }
}
