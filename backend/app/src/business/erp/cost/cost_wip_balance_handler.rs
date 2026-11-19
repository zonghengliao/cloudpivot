//! 在制品「期末在制余额 + 结转核对」的**接口层**（成本核算 · M5-23）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-wip-balance`（AGENTS 约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 1 | GET | `/business/erp/cost-wip-balance` | `business:erp:cost-object:list` | 期间内逐成本对象的期末在制余额（期初 / 投入 / 转出 / 期末）+ 结转核对 |
//!
//! # 一条路由，为什么没有第二个
//!
//! 「期末在制余额」与「结转」在这套设计里是**同一件事的两面**：
//!
//! * 余额 = 期初 + 投入 − 完工转出（逐成本对象、逐要素）；
//! * 结转 = 这笔余额进没进下一期的期初 —— 库里没有「结转表」，因为期初在制**自动**从
//!   上期**已结算**计算单取数（设计 §4.6.3），再建一张表就是第二本成本账（ARCH-4 D9）。
//!
//! 所以写路径不存在（也不需要）：下期执行 `POST /cost-period/calc` 时自动带上；
//! 本卡把「到底带上了没有」逐行核出来，核不上就报差额与怎么修。
//!
//! # 权限码：复用 `cost-object:list`（零新增码、零迁移、零菜单）
//!
//! 与 `cost-wip-alloc`（M5-22）/ `cost-wip-equiv`（M5-20）/ `cost-collect-sheet`（M5-02）/
//! `wip-balance`（M5-05）同码：它们读的是同一批 `biz_cost_object`。新开 `cost-wip-balance:list`
//! 会带来两个问题：① 没有菜单行承载 ⇒ 死码（`permission_audit_tests::n14` 判红）；
//! ② 先插一条指向不存在组件的菜单行 ⇒ `n08` 判红。页面码 / 接口码解耦（PERM-01）只在 HR 试点。
//!
//! # 数据权限
//!
//! 行由 `biz_cost_object.workshop_dept_id` 裁剪（与分配表 / 约当表 / 归集表同一锚点，见取数层）。
//! 看不见的对象**不出现在表里**，也不报 403。
//!
//! # 只读
//!
//! 一条取数 + 纯内存装配：任一步失败就整请求 500，没有中间态
//! （AGENTS 约定 11 的「提交后失败」在这里没有面）。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_wip_alloc_handler` / `cost_wip_equiv_handler` 同款：axum handler 要 `AuthClaims`
//! 才能跑，单测调不到；把「归一化 + 取数 + 算 + 分页 + 装配」收进 [`balance_table_of`] 之后，
//! 真库用例走**同一条代码路径**（只跳过认证）。

use axum::extract::Query;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_calc_overhead;
use crate::business::erp::cost::cost_labor_dto;
use crate::business::erp::cost::cost_wip_alloc_dto::{self as alloc, alloc_of};
use crate::business::erp::cost::cost_wip_balance_dto::{
    self as dto, BalanceFilter, BalanceQuery, BalanceRes, BalanceStateFilter, BalanceTotals, TotalsExtra,
};
use crate::business::erp::cost::cost_wip_balance_model as model;
use crate::business::erp::model_base::normalize_page;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::utils;

// ==================== 权限码 ====================

/// 表读取（**复用** `cost-object:list`，理由见模块头）。
pub const PERM_LIST: &str = dto::PERM_LIST;

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形）。
pub const BALANCE_PERMISSIONS: [&str; 1] = dto::PERMISSIONS;

// ==================== 入参归一化 ====================

/// 归一化入参（纯逻辑，单测直接喂 [`BalanceQuery`]）。
///
/// 期间必须显式给：余额是「截至某一期」的存量，选错期间等于把别的月份的余额当本期；
/// 而且「默认上个月」会让车间看错月份还不自知。
pub fn filter_of(query: &BalanceQuery) -> std::result::Result<BalanceFilter, ApiError> {
    let period_code = cost_labor_dto::clean_optional(query.period_code.clone())
        .ok_or_else(|| dto::bad_request("period_code（yyyy-MM）不能为空：余额是截至某一期的存量，例如 2026-11"))?;
    cost_calc_overhead::ensure_period_code(&period_code)?;
    let state = BalanceStateFilter::parse(query.state.as_deref())?;
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    Ok(BalanceFilter {
        period_code,
        keyword: cost_labor_dto::clean_optional(query.keyword.clone()),
        state,
        page,
        page_size,
    })
}

// ==================== 编排 ====================

/// 一行的算数结果（避免在分页处再算一遍）。
struct ComputedRow {
    /// 行头 + 全部事实（M5-22 的取数结果）。
    source: model::SourceRow,
    /// 对象状态（`OPEN` / `CLOSED`）。
    object_status: String,
    /// 分配结果（M5-22 同一段代码）。
    calc: alloc::AllocCalc,
    /// 余额滚动。
    roll: dto::BalanceCalc,
    /// 结转核对。
    carry: dto::CarryOutcome,
}

/// 期末在制余额表（一条只读报表：取数 → 逐行滚动 + 结转核对 → 筛 → 分页 → 整表合计）。
pub async fn balance_table_of(
    principal: &ScopedPrincipal,
    filter: &BalanceFilter,
) -> std::result::Result<BalanceRes, ApiError> {
    // ① 本期批次（活跃的 + 最近一次红冲的）：红冲 ⇒ 账面余额作废
    let batches = model::batches_of(&filter.period_code)
        .await
        .map_err(|error| utils::db_error(error, "取本期成本核算批次"))?;

    // ② 取数（数据范围 + 关键字已在 SQL 里收窄；与 M5-22 同一批数）
    let rows = model::rows_of(
        principal,
        &filter.period_code,
        filter.keyword.as_deref(),
        batches.active_id(),
    )
    .await
    .map_err(|error| utils::db_error(error, "查询期末在制余额"))?;

    // ③ 对象 id（库里的 uuid → 批量取状态 / 下一期，各一次查询，无 N+1）
    let mut ids: Vec<Uuid> = Vec::with_capacity(rows.len());
    for row in &rows {
        ids.push(parse_object_id(&row.head.object_id)?);
    }
    let statuses = model::object_status_of(principal, &ids)
        .await
        .map_err(|error| utils::db_error(error, "取成本对象状态"))?;
    let nexts = model::next_items_of(&ids, &filter.period_code)
        .await
        .map_err(|error| utils::db_error(error, "取下一期的期初在制"))?;

    // ④ 逐行算 + 筛 + 合计（整表，不是当前页）
    let mut totals = BalanceTotals::default();
    let mut extra = TotalsExtra::default();
    let mut kept: Vec<ComputedRow> = Vec::with_capacity(rows.len());
    for (row, object_id) in rows.into_iter().zip(ids) {
        let calc = alloc_of(&row.facts);
        let roll = dto::balance_of(&row.facts, &calc);
        let object_status = statuses
            .get(&object_id)
            .map(String::as_str)
            .unwrap_or("OPEN")
            .to_string();
        let carry = dto::carry_of(
            &filter.period_code,
            &calc,
            roll.book,
            &object_status,
            batches.carry_ref(),
            nexts.get(&object_id),
        );
        if !filter.state.matches(&carry) {
            continue;
        }
        totals.add(&calc, &roll, &carry);
        match carry.state {
            dto::CarryState::Unsettled => {
                extra.unbooked_total += roll.wip_total.unwrap_or(rust_decimal_zero());
            }
            dto::CarryState::ClosedWip => {
                extra.closed_wip_total += roll.wip_total.unwrap_or(rust_decimal_zero());
            }
            dto::CarryState::Mismatch => {
                extra.mismatch_abs_total += carry.diff_cost.unwrap_or(rust_decimal_zero()).abs();
            }
            _ => {}
        }
        kept.push(ComputedRow {
            source: row,
            object_status,
            calc,
            roll,
            carry,
        });
    }

    // ⑤ 分页（升序来自 SQL：`ORDER BY t.object_no`；页码越界给空页，不报错）
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
    let list: Vec<dto::BalanceRow> = kept
        .get(start..end)
        .unwrap_or(&[])
        .iter()
        .map(|row| {
            dto::row_view(
                &row.source.head,
                &row.object_status,
                &row.source.facts,
                &row.calc,
                &row.roll,
                &row.carry,
            )
        })
        .collect();

    // ⑥ 页脚说明：把「没批次 / 有未入账 / 有结转不齐 / 有算不出来」四件事说清楚
    let cost_ready = batches.active.is_some();
    let mut note = String::new();
    if let Some(reversed) = batches.reversed.as_ref() {
        note.push_str(&format!(
            "{p} 的核算批次 {no} 已红冲（{reason}）：本表所有行的账面余额都按「已作废」处理\
             —— 重新计算本期（POST /business/erp/cost-period/calc）后再看。",
            p = filter.period_code,
            no = reversed.batch_no,
            reason = reversed
                .reverse_reason
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .unwrap_or("未填原因")
        ));
    }
    if !cost_ready && batches.reversed.is_none() {
        note.push_str(&format!(
            "{p} 还没有成本核算批次：本表是**试算视图**（按成本对象当前的数量 / 完工率 / 归集算），\
             余额还没入账，下一期也取不到；建批次并执行计算（POST /business/erp/cost-period/calc）后回来，\
             `ledger` 一列才有账面快照。",
            p = filter.period_code
        ));
    }
    if totals.unsettled_count > 0 {
        note.push_str(&format!(
            "有 {} 行余额**未入账**（本期没结算，试算合计 {} ）：下一期算的时候期初取 0，\
             这笔钱会凭空变成下期的完工成本 —— 先结算本期。",
            totals.unsettled_count,
            dto::amount_text(extra.unbooked_total)
        ));
    }
    if totals.mismatch_count > 0 {
        note.push_str(&format!(
            "有 {} 行结转不齐（差额绝对值合计 {}）：下期期初与本期账面期末对不上，按行内 hint 核对。",
            totals.mismatch_count,
            dto::amount_text(extra.mismatch_abs_total)
        ));
    }
    if totals.closed_wip_count > 0 {
        note.push_str(&format!(
            "有 {} 行已关批仍挂余额（{}）：不会再有完工，必须改回 OPEN 或按呆滞处理。",
            totals.closed_wip_count,
            dto::amount_text(extra.closed_wip_total)
        ));
    }
    if totals.blocked_count > 0 {
        note.push_str(&format!(
            "有 {} 行余额算不出来（缺完工率 / 本期无产量 / 投料方式不支持 / 期初还原失败 / 出现负数分配）：\
             它们的余额给空值而不是 0，请按每行的「怎么修」处理后再重算。",
            totals.blocked_count
        ));
    }
    note.push_str(dto::FORMULA_ROLL);
    note.push(' ');
    note.push_str(dto::FORMULA_CARRY);
    note.push_str(
        " 已结算的行「期初 / 完工转出 / 期末余额」三列取计算单快照（账上的数）；\
         试算与账面漂移时三要素分列一律给空（按试算拆出来加不出账面余额），红冲重算后恢复。",
    );

    Ok(BalanceRes {
        period_code: filter.period_code.clone(),
        batch: batches.active.as_ref().map(|(_, batch)| dto::BatchView {
            id: batch.id.clone(),
            batch_no: batch.batch_no.clone(),
            status: batch.status.clone(),
        }),
        reversed_batch: batches.reversed.as_ref().map(|batch| dto::BatchView {
            id: batch.id.clone(),
            batch_no: batch.batch_no.clone(),
            status: batch.status.clone(),
        }),
        cost_ready,
        state: filter.state.as_str().to_string(),
        keyword: filter.keyword.clone(),
        formula_roll: dto::FORMULA_ROLL.to_string(),
        formula_carry: dto::FORMULA_CARRY.to_string(),
        totals: dto::totals_view(&totals, &extra),
        list,
        total: u64::try_from(total_rows).unwrap_or(0),
        page: filter.page,
        page_size: filter.page_size,
        state_options: dto::state_options(),
        note,
    })
}

/// 行头里的对象 id（String）→ `Uuid`。解析不出来是**内部契约**问题（库里存的就是 uuid）⇒ 500。
fn parse_object_id(text: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(text).map_err(|error| {
        dto::internal_error(&format!(
            "成本对象 id `{text}` 不是合法 uuid（{error}）：取数层与库的数据契约不一致"
        ))
    })
}

/// `Decimal::ZERO`（少一行 import）。
fn rust_decimal_zero() -> nik::prelude::rust_decimal::Decimal {
    nik::prelude::rust_decimal::Decimal::ZERO
}

// ==================== HTTP 入口 ====================

/// 期末在制余额表（逐成本对象：期初 / 投入 / 完工转出 / 期末余额 + 结转核对）。
#[nik::permission(
    path = "/business/erp/cost-wip-balance",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-wip-balance")]
pub async fn cost_wip_balance_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<BalanceQuery>,
) -> ResResult<BalanceRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    let filter = filter_of(&query)?;
    Ok(Res::ok(balance_table_of(&principal, &filter).await?))
}

/// 权限码自检出口（与其它 ERP 模块同形）。
pub fn all_permissions() -> Vec<&'static str> {
    BALANCE_PERMISSIONS.to_vec()
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn query() -> BalanceQuery {
        BalanceQuery {
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
        q.state = Some("carry".to_string());
        let error = filter_of(&q).expect_err("拼错必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("carry"));
    }

    #[test]
    fn defaults_are_wip_and_page_one() {
        let filter = filter_of(&query()).expect("缺省可解析");
        assert_eq!(filter.period_code, "2026-11");
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, 20);
        assert_eq!(filter.state, BalanceStateFilter::Wip, "默认只看有余额的行");
        assert!(filter.keyword.is_none());
    }

    #[test]
    fn page_size_is_capped_and_keyword_trimmed() {
        let mut q = query();
        q.page = Some(0);
        q.page_size = Some(9999);
        q.keyword = Some("  CB-1  ".to_string());
        q.state = Some("unsettled".to_string());
        let filter = filter_of(&q).expect("合法入参");
        assert_eq!(filter.page, 1, "页码从 1 起");
        assert_eq!(filter.page_size, 200, "上限 200");
        assert_eq!(filter.keyword.as_deref(), Some("CB-1"));
        assert_eq!(filter.state, BalanceStateFilter::Unsettled);
    }

    #[test]
    fn object_id_must_be_a_uuid() {
        assert!(parse_object_id("not-a-uuid").is_err_and(|error| error.status == 500));
        let id = Uuid::now_v7();
        assert_eq!(parse_object_id(&id.to_string()).expect("合法 uuid"), id);
    }
}
