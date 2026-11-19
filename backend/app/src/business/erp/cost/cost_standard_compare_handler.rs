//! 标准成本 × 实际成本 **逐物料对比** 的接口层（成本核算 · M5-11）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/report/cost-variance`（AGENTS 约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 36 | GET | `/business/erp/report/cost-variance` | `business:erp:cost-method:list` | 逐物料对比（标准 vs 实际，含三要素差异与整表合计） |
//!
//! # 一条路由，为什么没有第二个参数版本
//!
//! 设计 §5.2 #36 只给了这一条（原设计口径是「期间 × 成本对象 × 差异类型」的差异报表）。本卡是
//! 该报表的**汇总层**：一个物料一行，出「标准 / 实际 / 差异额 / 差异率 / 三要素拆分」。
//! 钻取（某物料某批的六类差异明细）由 M5-12~M5-16 填 `biz_cost_variance` 之后另开子路径
//! `/business/erp/report/cost-variance/{...}`，**不改本路径的出参结构**。
//!
//! # 权限码：复用 `cost-method:list`（零新增码、零新增迁移）
//!
//! 标准成本卡那一族接口（M5-08/09/10 共 12 条）全部挂 `cost-method` 的四个码，理由见
//! `cost_standard_handler.rs` 模块头：标准成本与「按什么方法核算」是同一类**全公司财务口径配置**，
//! 维护角色完全一致。本报表读的正是这批数据（标准侧）+ 成本计算单（实际侧），看得到两者的人
//! 本来就是同一批（财务经理 / 财务总监）。
//!
//! 新开 `cost-variance:list` 会带来两个问题：① 没有菜单行承载 ⇒ 死码（`permission_audit_tests::n14`
//! 判红）；② 先插一条指向不存在组件的菜单行 ⇒ `n08` 判红（侧栏点开 404）。差异分析页（M5-19）
//! 落地时连同菜单一起解耦，那时才是新开码的时机。
//!
//! # 数据权限：unscoped（全公司财务口径）
//!
//! 与 `cost-standard` / `cost-period` 单头同口径：按部门裁剪差异报表，等于让财务看不见别的车间的
//! 超支 —— 正好把差异藏起来。理由见 `cost_standard_compare_model.rs` 模块头。
//!
//! # 只读
//!
//! 本卡不写库（不落 `biz_cost_variance`、不出凭证、不动存货）：ARCH-9 D-15 定「差异只出报表不入账」。
//! 因此也不存在「提交后失败留死单」的面（AGENTS 约定 11）：只有两条只读查询，任一条失败就整请求 500，
//! 没有中间态。

use axum::extract::Query;
use nik::prelude::*;
use serde::Deserialize;

use crate::business::erp::cost::cost_labor_dto;
use crate::business::erp::cost::cost_period_item_model;
use crate::business::erp::cost::cost_standard_compare_dto::{self as dto, CompareRes, StateFilter};
use crate::business::erp::cost::cost_standard_compare_model::{self as model, CompareFilter};
use crate::business::erp::cost::cost_standard_dto;
use crate::business::erp::model_base::normalize_page;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码（复用 cost-method:list，理由见模块头） ====================

/// 报表读取（与 `cost-standard` 一族同码）。
pub const PERM_LIST: &str = "business:erp:cost-method:list";

/// 本模块全部权限码（与其余成本模块 `all_permissions` 同形）。
pub const COMPARE_PERMISSIONS: [&str; 1] = [PERM_LIST];

// ==================== 入参 ====================

/// `GET /business/erp/report/cost-variance`。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CompareQuery {
    /// 期间（必填；`yyyy-MM`）。
    pub period_code: Option<String>,
    /// 物料编码 / 名称关键字（可选）。
    pub keyword: Option<String>,
    /// 状态筛选（`all` / `comparable` / `no_standard` / `no_actual`；缺省 `all`）。
    pub state: Option<String>,
    /// 页码（缺省 1）。
    pub page: Option<u64>,
    /// 每页条数（缺省 20，上限 200）。
    pub page_size: Option<u64>,
}

/// 归一化入参（纯逻辑，单测直接喂 [`CompareQuery`]）。
///
/// 期间必须显式给：报表跨期间没有默认值（默认「上个月」会让财务看错月份还不自知）。
pub fn filter_of(query: &CompareQuery) -> std::result::Result<(String, CompareFilter), ApiError> {
    let raw = cost_labor_dto::clean_optional(query.period_code.clone())
        .ok_or_else(|| dto_bad_request("period_code（yyyy-MM）不能为空：对比按期间取数，例如 2026-11"))?;
    let period = cost_standard_dto::period_window(&raw)?;
    let state = StateFilter::parse(query.state.as_deref())?;
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    let filter = CompareFilter {
        period,
        keyword: cost_labor_dto::clean_optional(query.keyword.clone()),
        state,
        page,
        page_size,
    };
    Ok((raw, filter))
}

/// 400 + `PARAM_INVALID`（与成本域其它模块同形）。
fn dto_bad_request(message: &str) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.to_string())
}

// ==================== 编排 ====================

/// 逐物料对比（一条只读报表：活跃批次 + 数据页 + 整表合计）。
pub async fn compare(period_code: &str, filter: &CompareFilter) -> std::result::Result<CompareRes, ApiError> {
    // ① 实际侧出处：当期活跃批次（`REVERSED` / `CANCELLED` / 软删不占位，与 uk_biz_cost_period 同口径）
    let batch = cost_period_item_model::active_batch(db(), period_code)
        .await
        .map_err(|error| utils::db_error(error, "取当期成本核算批次"))?;
    let batch_id = batch.as_ref().map(|row| row.id);

    // ② 数据页 + ③ 整表合计：两条查询同一份筛选 SQL，口径不会分叉
    let (rows, total) = model::page(filter, batch_id)
        .await
        .map_err(|error| utils::db_error(error, "查询标准成本与实际成本对比"))?;
    let totals = model::totals(filter, batch_id)
        .await
        .map_err(|error| utils::db_error(error, "统计标准成本与实际成本对比合计"))?;

    let (start, end) = filter.period;
    let list: Vec<dto::CompareRow> = rows.iter().map(|core| dto::row_view(core, period_code)).collect();
    let mut note = dto::note_of(&end.to_string());
    let cost_ready = batch.is_some();
    if !cost_ready {
        note.push_str(&format!(
            "另：{period_code} 还没有成本核算批次，实际一侧为空（全部为「缺实际」）——\
             先在该期间建成本核算批次并执行计算（POST /business/erp/cost-period/calc），再回来看对比。"
        ));
    }

    Ok(CompareRes {
        period_code: period_code.to_string(),
        period_start: start.to_string(),
        period_end: end.to_string(),
        standard_basis: dto::STANDARD_BASIS.to_string(),
        batch: batch.map(|row| dto::BatchView {
            id: row.id.to_string(),
            batch_no: row.batch_no,
            status: row.status,
        }),
        cost_ready,
        state_filter: filter.state.as_str().to_string(),
        keyword: filter.keyword.clone(),
        list,
        total: u64::try_from(total).unwrap_or(0),
        page: filter.page,
        page_size: filter.page_size,
        totals: dto::totals_view(&totals),
        note,
    })
}

// ==================== HTTP 入口 ====================

/// 逐物料对比报表（设计 §5.2 #36）。
#[nik::permission(
    path = "/business/erp/report/cost-variance",
    method = "GET",
    permission = "business:erp:cost-method:list"
)]
#[nik::get("/business/erp/report/cost-variance")]
pub async fn cost_variance_by_material_api(Query(query): Query<CompareQuery>) -> ResResult<CompareRes> {
    let (period_code, filter) = filter_of(&query)?;
    Ok(Res::ok(compare(&period_code, &filter).await?))
}

/// 权限码自检出口（与其它 ERP 模块同形）。
pub fn all_permissions() -> Vec<&'static str> {
    COMPARE_PERMISSIONS.to_vec()
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::NaiveDate;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("日期字面量")
    }

    // ---------- 权限 ----------

    #[test]
    fn permission_reuses_cost_method_list() {
        // 零新增权限码：与标准成本一族同码（新开码在 M5-19 页落地前是死码 n14）
        assert_eq!(PERM_LIST, "business:erp:cost-method:list");
        assert_eq!(COMPARE_PERMISSIONS, [PERM_LIST]);
        assert_eq!(all_permissions(), vec![PERM_LIST]);
    }

    // ---------- 入参 ----------

    #[test]
    fn period_is_required_and_validated() {
        let missing = CompareQuery::default();
        let err = filter_of(&missing).expect_err("缺期间必须 400");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("period_code"), "{}", err.msg);

        let blank = CompareQuery {
            period_code: Some("   ".to_string()),
            ..Default::default()
        };
        assert_eq!(filter_of(&blank).expect_err("空白也是缺").status, 400);

        for bad in ["2026-13", "2026-00", "202611", "2026-1", "2026-11-01"] {
            let query = CompareQuery {
                period_code: Some(bad.to_string()),
                ..Default::default()
            };
            let err = filter_of(&query).expect_err("非法期间必须 400");
            assert_eq!(err.status, 400, "期间「{bad}」应 400：{}", err.msg);
        }
    }

    #[test]
    fn period_window_is_first_to_last_day_and_paged_defaults_apply() {
        let query = CompareQuery {
            period_code: Some(" 2026-02 ".to_string()),
            ..Default::default()
        };
        let (code, filter) = filter_of(&query).expect("合法期间");
        assert_eq!(code, "2026-02", "期间要去空白");
        assert_eq!(
            filter.period,
            (date(2026, 2, 1), date(2026, 2, 28)),
            "闰年判断交给 period_window"
        );
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, 20, "缺省每页 20（model_base::DEFAULT_PAGE_SIZE）");
        assert_eq!(filter.state, StateFilter::All, "缺省看全部");
        assert!(filter.keyword.is_none());
    }

    #[test]
    fn page_size_is_capped_and_blank_keyword_dropped() {
        let query = CompareQuery {
            period_code: Some("2026-02".to_string()),
            keyword: Some("  ".to_string()),
            page: Some(0),
            page_size: Some(500),
            ..Default::default()
        };
        let (_, filter) = filter_of(&query).expect("合法入参");
        assert_eq!(filter.page, 1, "page 0 归一到 1");
        assert_eq!(filter.page_size, 200, "每页上限 200");
        assert!(filter.keyword.is_none(), "空白关键字等于没有");

        let keyword = CompareQuery {
            period_code: Some("2026-02".to_string()),
            keyword: Some(" FG-100 ".to_string()),
            ..Default::default()
        };
        let (_, filter) = filter_of(&keyword).expect("带关键字");
        assert_eq!(filter.keyword.as_deref(), Some("FG-100"), "关键字要去空白");
    }

    #[test]
    fn state_filter_accepts_four_values_and_rejects_others() {
        for (raw, expected) in [
            (None, StateFilter::All),
            (Some("all"), StateFilter::All),
            (Some(" COMPARABLE "), StateFilter::Comparable),
            (Some("no_standard"), StateFilter::NoStandard),
            (Some("no_actual"), StateFilter::NoActual),
        ] {
            let query = CompareQuery {
                period_code: Some("2026-11".to_string()),
                state: raw.map(str::to_string),
                ..Default::default()
            };
            let (_, filter) = filter_of(&query).unwrap_or_else(|error| panic!("{raw:?} 应通过：{}", error.msg));
            assert_eq!(filter.state, expected, "state={raw:?}");
        }

        let bad = CompareQuery {
            period_code: Some("2026-11".to_string()),
            state: Some("variance".to_string()),
            ..Default::default()
        };
        let err = filter_of(&bad).expect_err("非法 state 必须 400（不静默回落 all）");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("state 只能是"), "{}", err.msg);
        assert!(err.msg.contains("no_standard"), "400 文案要把值域列全：{}", err.msg);
    }

    #[test]
    fn state_filter_round_trips_its_literal() {
        for state in [
            StateFilter::All,
            StateFilter::Comparable,
            StateFilter::NoStandard,
            StateFilter::NoActual,
        ] {
            let literal = state.as_str();
            assert_eq!(
                StateFilter::parse(Some(literal)).expect("回读自身字面值"),
                state,
                "as_str / parse 必须互逆（{literal}）"
            );
        }
    }

    // ---------- 出参口径 ----------

    #[test]
    fn note_states_sign_convention_and_no_posting() {
        let note = dto::note_of("2026-11-30");
        assert!(note.contains("2026-11-30"), "口径要写清基准日：{note}");
        assert!(note.contains("正数 = 不利"), "符号约定必须写在报文里：{note}");
        assert!(note.contains("不入账"), "D-15：差异不入账要明说：{note}");
        assert!(note.contains("ARCH-9 D-15"), "给出来源：{note}");
    }

    #[test]
    fn standard_basis_mentions_period_end_and_publish() {
        assert!(dto::STANDARD_BASIS.contains("期间末"));
        assert!(dto::STANDARD_BASIS.contains("已发布"));
    }
}
