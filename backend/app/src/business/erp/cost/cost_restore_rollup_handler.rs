//! 成本还原「逐层还原到料工费」的**接口层**（成本核算 · M5-27）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-restore-rollup`（AGENTS 约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 1 | GET | `/business/erp/cost-restore-rollup` | `business:erp:cost-object:list` | 期间内逐成本对象的**还原**：还原前料工费 → 逐层卷积 → 还原后料工费（+ 逐层台账 + 半成品明细） |
//!
//! # 一条路由，与 M5-26 的关系
//!
//! * **M5-26**（`cost-restore-structure`）：出**结构**（每一层是什么、比例多少）；
//! * **M5-27**（本条）：把结构**卷积成钱**（还原后的料 / 工 / 费）。
//!
//! 实现上不重查一遍库：直接调 [`expanded_rows_of`] 拿全量行（每行带 `tree[]` 与
//! `elements`），在内存里递归卷积（[`dto::restore_of`]）。两张表的数字因此永远一致 ——
//! 「结构表里 S 的结构比例」与「还原表里 S 被拆成多少料工费」出自同一批取数。
//!
//! # 数据权限
//!
//! 行由 `biz_cost_object.workshop_dept_id` 裁剪（在 M5-26 取数层里做）。
//! 看不见的对象**不出现在表里**，也不报 403。人工 / 制造费用归集只按本页已有对象回填，
//! 不再二次裁剪（否则跨车间对象的人工会被抹成 0，还原前的分母就错了）。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与结构表 / 余额表 / 分配表同款：axum handler 要 `AuthClaims` 才能跑，单测调不到；
//! 把「归一化 + 还原 + 筛选 + 分页 + 装配」收进 [`rollup_table_of`] 之后，
//! 真库用例走**同一条代码路径**（只跳过认证）。

use axum::extract::Query;
use nik::prelude::*;

use crate::business::erp::cost::cost_calc_overhead;
use crate::business::erp::cost::cost_labor_dto;
use crate::business::erp::cost::cost_restore_rollup_dto::{
    self as dto, ElementSplit, RollupFilter, RollupQuery, RollupRes, RollupState, Row, StateFilter, Totals,
};
use crate::business::erp::cost::cost_restore_rollup_model as model;
use crate::business::erp::cost::cost_restore_structure_dto::RestoreFilter;
use crate::business::erp::cost::cost_restore_structure_handler::{Expanded, expanded_rows_of};
use crate::business::erp::model_base::normalize_page;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::utils;

// ==================== 权限码 ====================

/// 表读取（**复用** `cost-object:list`，理由见 dto 模块头）。
pub const PERM_LIST: &str = dto::PERM_LIST;

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形）。
pub const ROLLUP_PERMISSIONS: [&str; 1] = dto::PERMISSIONS;

// ==================== 入参归一化 ====================

/// 归一化入参（纯逻辑，单测直接喂 [`RollupQuery`]）。
///
/// 期间必须显式给：还原的是「某一期投了什么钱」，选错期间等于把别的月份的钱当成本期。
pub fn filter_of(query: &RollupQuery) -> std::result::Result<RollupFilter, ApiError> {
    let period_code = cost_labor_dto::clean_optional(query.period_code.clone())
        .ok_or_else(|| dto::bad_request("period_code（yyyy-MM）不能为空：还原是某一期投料的快照，例如 2026-11"))?;
    cost_calc_overhead::ensure_period_code(&period_code)?;
    let state = StateFilter::parse(query.state.as_deref())?;
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    let max_depth = match query.max_depth.unwrap_or(dto::MAX_DEPTH_DEFAULT) {
        0 => dto::MAX_DEPTH_DEFAULT,
        value => value.min(dto::MAX_DEPTH_LIMIT),
    };
    Ok(RollupFilter {
        period_code,
        keyword: cost_labor_dto::clean_optional(query.keyword.clone()),
        state,
        page,
        page_size,
        max_depth,
    })
}

/// 还原表的筛选条件 → 结构表的筛选条件（**状态不传**：结构表的五档状态与本表的三档不是一回事，
/// 在这里筛会让 `no_structure` 之类的行提前消失，本表的 `partial` 就空了）。
fn structure_filter_of(filter: &RollupFilter) -> RestoreFilter {
    RestoreFilter {
        period_code: filter.period_code.clone(),
        keyword: filter.keyword.clone(),
        state: crate::business::erp::cost::cost_restore_structure_dto::StateFilter::All,
        page: 1,
        page_size: filter.page_size,
        max_depth: filter.max_depth,
    }
}

// ==================== 还原表 ====================

/// 一行还原（结构表的一行 → 还原后的一行；纯装配，不连库）。
fn row_of(structure: &crate::business::erp::cost::cost_restore_structure_dto::Row, costs: model::LaborOverhead) -> Row {
    let before = ElementSplit {
        material: dto::parse_money(&structure.material_cost),
        labor: costs.labor,
        overhead: costs.overhead,
    };
    let rollup = dto::restore_of(&structure.tree, before);
    let semi_cost = dto::parse_money(&structure.semi_cost);
    let state = dto::row_state(u64::try_from(rollup.semis.len()).unwrap_or(0), rollup.unresolved);

    let mut issues = structure.issues.clone();
    let mut warnings = structure.warnings.clone();
    if rollup.missing {
        warnings.push(
            "有半成品取不到完工成本结构（未计算 / 已红冲 / 没有下一层投入）：\
             这部分金额停在半成品形态，记在「还原不彻底」里，不当外购料"
                .to_string(),
        );
    }
    if rollup.truncated {
        warnings.push(format!(
            "有半成品到 {} 层被截断（结构闭环 / 层数上限）：截断处的材料部分记在「还原不彻底」里；\
             调大 max_depth（上限 {}）可继续还原",
            rollup.flows.len(),
            dto::MAX_DEPTH_LIMIT
        ));
    }
    if !rollup.balanced() {
        // 理论上不可能（每一步都守恒）：真出现就是代码 bug，别让数字悄悄错下去
        warnings.push(format!(
            "守恒自检失败：还原后合计 {} + 不彻底 {} ≠ 还原前合计 {}（请报缺陷）",
            dto::money_text(rollup.after.total()),
            dto::money_text(rollup.unresolved),
            dto::money_text(rollup.before.total())
        ));
        issues.push(dto::IssueView {
            code: "UNBALANCED".to_string(),
            message: "还原后合计与还原前合计对不上".to_string(),
            fix: "这是缺陷：请带着 object_no 与期间报给后端，不要手工改数".to_string(),
        });
    }

    let after = rollup.after;
    let depth = rollup.flows.last().map(|flow| flow.level).unwrap_or(0);
    let node_count = rollup.flows.iter().map(|flow| flow.node_count).sum::<u64>();

    Row {
        object_id: structure.object_id.clone(),
        object_no: structure.object_no.clone(),
        object_type: structure.object_type.clone(),
        lot_no: structure.lot_no.clone(),
        product_id: structure.product_id.clone(),
        product_code: structure.product_code.clone(),
        product_name: structure.product_name.clone(),
        workshop_dept_id: structure.workshop_dept_id.clone(),
        workshop_dept_name: structure.workshop_dept_name.clone(),
        item: structure.item.as_ref().map(|item| dto::ItemBrief {
            status: item.status.clone(),
            completed_qty: item.completed_qty.clone(),
            finished_cost: item.finished_cost.clone(),
            unit_finished_cost: item.unit_finished_cost.clone(),
        }),
        finished_cost: structure.finished_cost.clone(),
        before: before.view(),
        semi_in_material: dto::money_text(semi_cost),
        purchased_in_material: dto::money_text(before.material - semi_cost),
        semi_share: dto::share_pct(semi_cost, before.material).map(dto::pct_text),
        after: after.view(),
        after_material_share: dto::share_pct(after.material, after.total()).map(dto::pct_text),
        after_labor_share: dto::share_pct(after.labor, after.total()).map(dto::pct_text),
        after_overhead_share: dto::share_pct(after.overhead, after.total()).map(dto::pct_text),
        delta: dto::delta_view(before, after),
        restored_labor: dto::money_text(after.labor - before.labor),
        restored_overhead: dto::money_text(after.overhead - before.overhead),
        unresolved: dto::money_text(rollup.unresolved),
        state: state.as_str().to_string(),
        state_label: state.label().to_string(),
        depth,
        node_count,
        flows: dto::flow_views(&rollup.flows),
        semis: dto::semi_views(&rollup.semis),
        issues,
        warnings,
    }
}

/// 行状态（从出参回读，避免两处各算一遍）。
fn state_of(row: &Row) -> RollupState {
    match row.state.as_str() {
        "restored" => RollupState::Restored,
        "no_semi" => RollupState::NoSemi,
        _ => RollupState::Partial,
    }
}

/// 还原表的**全部行**（已按状态筛过，未分页）+ 整表合计。
///
/// 抽出来的原因：M5-28（`cost-restore-report`）要按车间 / 产品 / 对象**重新分组汇总**，
/// 而分页只在明细表上有意义。报表直接消费这里，不重查一遍库、也不抄一遍 SQL ——
/// 两张表（明细 / 报表）的每个数字都出自同一次卷积。
#[derive(Debug, Clone)]
pub struct RollupRows {
    /// 当期成本核算批次（没建 ⇒ `None`）。
    pub batch: Option<dto::BatchView>,
    /// 是否已建批次。
    pub cost_ready: bool,
    /// 全部行（已按状态筛过，未分页）。
    pub rows: Vec<Row>,
    /// 整表合计。
    pub totals: Totals,
    /// 节点数到了上限（后面的层没展开完）。
    pub overflow: bool,
}

/// 还原的全部行（结构 → 递归卷积 → 筛 → 合计；**不分页**）。
pub async fn rollup_rows_of(
    principal: &ScopedPrincipal,
    filter: &RollupFilter,
) -> std::result::Result<RollupRows, ApiError> {
    // ① 结构（复用 M5-26：逐层展开 + 数据范围 + 关键字，全量不分页）
    let Expanded {
        batch,
        rows: structure_rows,
        overflow,
        ..
    } = expanded_rows_of(principal, &structure_filter_of(filter)).await?;

    // ② 本期人工 / 制造费用归集（还原前的分母里除材料之外的两项）
    let object_ids: Vec<nik::uuid::Uuid> = structure_rows
        .iter()
        .filter_map(|row| nik::uuid::Uuid::parse_str(&row.object_id).ok())
        .collect();
    let costs = model::labor_overhead_of(&filter.period_code, &object_ids)
        .await
        .map_err(|error| utils::db_error(error, "取本期人工 / 制造费用归集"))?;

    // ③ 逐行还原 + 筛 + 合计（整表，不是当前页）
    let mut totals = Totals::default();
    let mut kept: Vec<Row> = Vec::with_capacity(structure_rows.len());
    for structure in &structure_rows {
        let cost = nik::uuid::Uuid::parse_str(&structure.object_id)
            .ok()
            .and_then(|id| costs.get(&id).copied())
            .unwrap_or_default();
        let row = row_of(structure, cost);
        if !filter.state.matches(state_of(&row)) {
            continue;
        }
        totals.add(&row);
        kept.push(row);
    }

    let cost_ready = batch.is_some();
    Ok(RollupRows {
        batch,
        cost_ready,
        rows: kept,
        totals,
        overflow,
    })
}

/// 还原表（一条只读报表：结构 → 递归卷积 → 筛 → 分页 → 整表合计）。
pub async fn rollup_table_of(
    principal: &ScopedPrincipal,
    filter: &RollupFilter,
) -> std::result::Result<RollupRes, ApiError> {
    let RollupRows {
        batch,
        cost_ready,
        rows: kept,
        totals,
        overflow,
    } = rollup_rows_of(principal, filter).await?;

    // ④ 分页（页码越界给空页，不报错）
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
    let list: Vec<Row> = kept.get(start..end).unwrap_or(&[]).to_vec();

    // ⑤ 页脚说明
    let mut note = String::new();
    if batch.is_none() {
        note.push_str(&format!(
            "{} 还没有成本核算批次：行头的「本期完工成本」是空的，还原照做 —— \
             还原只依赖归集行与半成品产出批次的计算单。",
            filter.period_code
        ));
    }
    if totals.partial_row_count > 0 {
        note.push_str(&format!(
            "有 {} 行还原不彻底（部分金额停在半成品形态）：按行内「还原不彻底」与逐层台账看卡在第几层，\
             常见原因是产出批次没算成本 / 已红冲 / 批号对不上导致不下钻。",
            totals.partial_row_count
        ));
    }
    if overflow {
        note.push_str(&format!(
            "本次展开的节点数到了上限（{}）：后面的层没展开完，串到本表的「还原不彻底」里；\
             请用 keyword 收窄对象或调小 max_depth 分段看。",
            crate::business::erp::cost::cost_restore_structure_dto::NODE_GUARD
        ));
    }
    note.push_str(dto::FORMULA_RESTORE);
    note.push(' ');
    note.push_str(dto::FORMULA_UNRESOLVED);

    Ok(RollupRes {
        period_code: filter.period_code.clone(),
        batch,
        cost_ready,
        state: filter.state.as_str().to_string(),
        keyword: filter.keyword.clone(),
        max_depth: filter.max_depth,
        formula_restore: dto::FORMULA_RESTORE.to_string(),
        formula_unresolved: dto::FORMULA_UNRESOLVED.to_string(),
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

/// 成本还原表（逐成本对象：还原前料工费 → 逐层还原到料工费）。
#[nik::permission(
    path = "/business/erp/cost-restore-rollup",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-restore-rollup")]
pub async fn cost_restore_rollup_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<RollupQuery>,
) -> ResResult<RollupRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    let filter = filter_of(&query)?;
    Ok(Res::ok(rollup_table_of(&principal, &filter).await?))
}

/// 权限码自检出口（与其它 ERP 模块同形）。
pub fn all_permissions() -> Vec<&'static str> {
    ROLLUP_PERMISSIONS.to_vec()
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::rust_decimal::Decimal;

    fn query() -> RollupQuery {
        RollupQuery {
            period_code: Some("2026-11".to_string()),
            keyword: None,
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
    fn state_typo_is_rejected() {
        let mut q = query();
        q.state = Some("done".to_string());
        let error = filter_of(&q).expect_err("拼错必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("done"));
    }

    #[test]
    fn defaults_are_all_and_depth_six() {
        let filter = filter_of(&query()).expect("缺省可解析");
        assert_eq!(filter.period_code, "2026-11");
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, 20);
        assert_eq!(filter.state, StateFilter::All);
        assert_eq!(filter.max_depth, dto::MAX_DEPTH_DEFAULT);
    }

    #[test]
    fn page_size_and_depth_are_capped() {
        let mut q = query();
        q.page = Some(0);
        q.page_size = Some(9999);
        q.max_depth = Some(99);
        let filter = filter_of(&q).expect("合法入参");
        assert_eq!(filter.page, 1, "页码从 1 起");
        assert_eq!(filter.page_size, 200, "上限 200");
        assert_eq!(filter.max_depth, dto::MAX_DEPTH_LIMIT, "层数夹到上限");

        q.max_depth = Some(0);
        assert_eq!(filter_of(&q).expect("0 层按默认").max_depth, dto::MAX_DEPTH_DEFAULT);
    }

    #[test]
    fn structure_filter_drops_the_state() {
        // 结构表的五档状态与本表的三档不是一回事：带过去筛会让 partial 行提前消失
        let filter = filter_of(&query()).expect("缺省可解析");
        let structure = structure_filter_of(&filter);
        assert_eq!(
            structure.state,
            crate::business::erp::cost::cost_restore_structure_dto::StateFilter::All
        );
        assert_eq!(structure.period_code, filter.period_code);
        assert_eq!(structure.max_depth, filter.max_depth);
        assert_eq!(structure.keyword, filter.keyword);
    }

    #[test]
    fn state_of_reads_back_the_row_state() {
        assert_eq!(state_of_of("restored"), RollupState::Restored);
        assert_eq!(state_of_of("no_semi"), RollupState::NoSemi);
        assert_eq!(state_of_of("partial"), RollupState::Partial);
    }

    fn state_of_of(state: &str) -> RollupState {
        match state {
            "restored" => RollupState::Restored,
            "no_semi" => RollupState::NoSemi,
            _ => RollupState::Partial,
        }
    }

    #[test]
    fn before_is_material_plus_collected_labor_and_overhead() {
        // 「还原前」= 归集的三要素：材料来自结构表，人工/费用来自本层的回填
        let costs = model::LaborOverhead {
            labor: Decimal::from(120),
            overhead: Decimal::from(80),
        };
        let before = ElementSplit {
            material: Decimal::from(500),
            labor: costs.labor,
            overhead: costs.overhead,
        };
        assert_eq!(before.total(), Decimal::from(700));
    }
}
