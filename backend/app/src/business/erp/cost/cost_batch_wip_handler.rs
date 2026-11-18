//! 分批法「在制批次余额」· 接口层（成本核算 · M5-05）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-collect-sheet/...`（约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 写库 |
//! |---|---|---|---|---|
//! | 1 | GET | `/business/erp/cost-collect-sheet/wip-balance?period_code=yyyy-MM` | `business:erp:cost-object:list` | ❌ |
//! | 2 | GET | `/business/erp/cost-collect-sheet/{object_id}/wip-ledger?period_code=yyyy-MM` | `business:erp:cost-object:list` | ❌ |
//!
//! # 两个接口的分工
//!
//! ```text
//! GET /cost-collect-sheet                        → 一批一行（本期料工费合计，M5-02）
//! GET /cost-collect-sheet/{id}                   → 这一批的逐笔归集行（M5-02）
//! GET /cost-collect-sheet/{id}/sources           → 三路来源预览（M5-03）
//! POST /cost-collect-sheet/{id}/collect          → 归集进来（M5-03）
//! GET /cost-collect-sheet/{id}/finish-preview    → 完工结转试算（M5-04）
//! POST /cost-collect-sheet/{id}/finish           → 把累计成本结转出去（M5-04）
//! GET /cost-collect-sheet/wip-balance            → **在制余额表**（一批一行，截止某期，本卡）
//! GET /cost-collect-sheet/{id}/wip-ledger        → **在制台账**（逐期：期初/投入/转出/期末，本卡）
//! ```
//!
//! 余额表回答「现在哪几批押着钱」，台账回答「这一批的钱是怎么攒起来的」。
//! 两者的截止余额**必须相等** —— 真库用例逐条钉住（`V13`）。
//!
//! # 为什么没有 `POST`（不夹带、也不该有）
//!
//! 「未完工留余额」不需要写任何东西：不结转，成本自然留在在制。写入口在 M5-03
//! （归集）与 M5-04 （结转）两张卡。在这里开一个「改余额」的口子等于让在制成本能
//! 凭空出现 —— 余额必须是**推出来的**，不是填进去的。
//!
//! # 为什么 `/wip-balance` 不会被 `/cost-collect-sheet/{object_id}` 吃掉
//!
//! 两条路由第二段分别是**字面量** `wip-balance` 与**参数** `{object_id}`。axum 底下的
//! `matchit` 让**字面量优先于参数**匹配，所以 `GET …/wip-balance` 走本模块、
//! `GET …/<uuid>` 仍走 M5-02 明细。这一点不靠"我觉得"：交付前用真跑 HTTP 探针
//! 确认过（余额表 200、明细 200、`{object_id}` 传垃圾串仍是 404）。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_collect_sheet_handler` / `cost_batch_finish_handler` 同款：axum handler 要
//! `AuthClaims` 才能跑，单测调不到；把「校验 + 取数 + 装配」收进 [`balance_of`] /
//! [`ledger_of`] 之后，真库用例走**同一条代码路径**（只跳过认证）。

use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_batch_finish_dto::Amounts;
use crate::business::erp::cost::cost_batch_wip_dto::{
    self as dto, LaterActivityRes, SettledRes, WipAmountsRes, WipBalanceRes, WipFacts, WipLedgerQuery, WipLedgerRes,
    WipQuery, WipSummaryRes, WipTotalsRes, WipWarningRes,
};
use crate::business::erp::cost::cost_batch_wip_model as model;
use crate::business::erp::cost::cost_collect_sheet_model;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码 ====================

/// 读在制余额 / 台账（**复用** `cost-object:list`，理由见 dto 模块头）。
pub const PERM_READ: &str = dto::PERM_READ;

/// 本模块全部权限码。
pub const WIP_PERMISSIONS: [&str; 1] = dto::PERMISSIONS;

// ==================== 错误出口 ====================

/// 批次不存在 / 已删 / 不在你的数据范围 —— 三种都是 404（不区分，避免探测）。
fn batch_not_found(id: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!(
            "批次不存在、已删除，或不在你的数据范围（{id}）。\
             在制余额请用 GET /business/erp/cost-collect-sheet/wip-balance?period_code=yyyy-MM"
        ),
    )
}

/// 路径参数 → UUID（非法形状也是 404：这个位置本来就该是 id）。
fn parse_id(raw: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| batch_not_found(raw))
}

// ==================== 装配：余额表 ====================

/// 在制余额表（分页 + 整张表合计）。
pub async fn balance_of(principal: &ScopedPrincipal, query: WipQuery) -> std::result::Result<WipBalanceRes, ApiError> {
    let filter = dto::filter_of(&query)?;
    let (rows, total) = model::page_rows(principal, &filter)
        .await
        .map_err(|error| utils::db_error(error, "查询在制批次余额表"))?;
    let totals = model::totals(principal, &filter)
        .await
        .map_err(|error| utils::db_error(error, "汇总在制批次余额表"))?;

    let counted = u64::try_from(totals.batch_count).unwrap_or(0);
    if counted != u64::try_from(total).unwrap_or(0) {
        // 行查询与合计查询同一份 FROM/WHERE，行数必须一致；不一致说明有一条 SQL
        // 被单独改过（例如只在分页那条加了条件）—— 宁可 500 也不出对不上的账。
        return Err(ApiError::internal(format!(
            "在制余额：行数与合计行数不一致（list={total}，totals={counted}）"
        )));
    }

    Ok(WipBalanceRes {
        period_code: filter.period_code.clone(),
        totals: WipTotalsRes {
            collected: WipAmountsRes::of(&totals.collected),
            transferred: WipAmountsRes::of(&totals.transferred),
            balance: WipAmountsRes::of(&dto::balance_of(&totals.collected, &totals.transferred)),
            period_in: WipAmountsRes::of(&totals.period_in),
            period_out: WipAmountsRes::of(&totals.period_out),
            batch_count: totals.batch_count,
            wip_batch_count: totals.wip_batch_count,
            negative_batch_count: totals.negative_batch_count,
        },
        list: rows.iter().map(model::WipRow::to_res).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page: filter.page,
        page_size: filter.page_size,
    })
}

// ==================== 装配：台账 ====================

/// 单批在制台账（逐期间滚动 + 截止期间汇总 + 警告）。
pub async fn ledger_of(
    principal: &ScopedPrincipal,
    id: &str,
    query: WipLedgerQuery,
) -> std::result::Result<WipLedgerRes, ApiError> {
    let object_id = parse_id(id)?;
    let cutoff = dto::period_of(query.period_code.as_deref())?;

    // 数据范围探针：与归集表 / 结转单同一份（不可见 ⇒ 404，不报 403 ——
    // 403 等于承认「这个 id 真实存在」，把别车间的批次号变成探测口）。
    let batch = cost_collect_sheet_model::find_visible_batch(principal, object_id)
        .await
        .map_err(|error| utils::db_error(error, "查询批次的归集抬头"))?
        .ok_or_else(|| batch_not_found(id))?;

    let collect = model::collect_flows(object_id, &cutoff)
        .await
        .map_err(|error| utils::db_error(error, "查询批次的逐期归集"))?;
    let finish = model::finish_flows(object_id, &cutoff)
        .await
        .map_err(|error| utils::db_error(error, "查询批次的逐期结转"))?;
    let flows = dto::merge_flows(collect, finish);

    // 汇总与台账行**同源**（都从 flows 折出来）：两处各查一次库，中间那笔并发写入
    // 会让「汇总 ≠ 末行期末」，用户会以为账在跳。
    let mut collected = Amounts::default();
    let mut transferred = Amounts::default();
    let (mut collect_lines, mut transfer_times) = (0_i64, 0_i64);
    for flow in &flows {
        collected = dto::plus(&collected, &flow.inflow);
        transferred = dto::plus(&transferred, &flow.outflow);
        collect_lines += flow.collect_lines;
        transfer_times += flow.transfer_times;
    }
    let series = dto::ledger_series(flows, &cutoff);
    debug_assert_eq!(series.closing, dto::balance_of(&collected, &transferred));

    let after = model::later_activity(object_id, &cutoff)
        .await
        .map_err(|error| utils::db_error(error, "查询截止期间之后的发生额"))?;
    let settled = model::settled_item(object_id, &cutoff)
        .await
        .map_err(|error| utils::db_error(error, "查询已结算计算单"))?;

    let balance = series.closing;
    let mut warnings = dto::evaluate_warnings(&WipFacts {
        status: &batch.status,
        completed_qty: batch.completed_qty,
        wip_qty: batch.wip_qty,
        balance,
    });
    if let Some((later_collected, later_transferred)) = after {
        warnings.push(WipWarningRes {
            code: dto::WARN_LATER_PERIOD_ACTIVITY.to_string(),
            level: dto::LEVEL_INFO.to_string(),
            message: format!("截止期间之后还有发生：归集 {later_collected}、结转 {later_transferred}"),
            hint: format!(
                "本表是「截至 {cutoff}」的快照，之后期间的成本没有计入这里的余额；\
                 要看最新余额请把 period_code 换成之后的期间"
            ),
        });
    }
    if let Some(settled) = &settled {
        warnings.push(dto::settled_note(settled, &balance));
    }

    Ok(WipLedgerRes {
        period_code: cutoff.clone(),
        batch: batch.to_res(),
        opening: WipAmountsRes::of(&series.opening),
        summary: WipSummaryRes {
            collected: WipAmountsRes::of(&collected),
            transferred: WipAmountsRes::of(&transferred),
            balance: WipAmountsRes::of(&balance),
            collect_lines,
            transfer_times,
        },
        periods: series.rows,
        truncated: series.truncated,
        after_period: after.map(|(later_collected, later_transferred)| LaterActivityRes {
            collected_total: later_collected.to_string(),
            transferred_total: later_transferred.to_string(),
        }),
        settled: settled.map(|settled| SettledRes {
            period_code: settled.period_code,
            finished_cost: settled.finished_cost.to_string(),
            wip_end_cost: settled.wip_end_cost.to_string(),
        }),
        warnings,
        notes: notes(&cutoff),
    })
}

/// 这张表的边界说明（前端直接展示，别让用户凭空猜口径）。
fn notes(cutoff: &str) -> Vec<String> {
    vec![
        format!("在制余额 = 累计归集 − 累计结转，均只算 period_code ≤ {cutoff} 的活行（软删视作作废）"),
        "分批法下未完工批次的成本全部留在在制（不做约当产量分摊）；整批完工后走「完工批次结转」转出".to_string(),
        "逐期滚动：本期期末 = 期初 + 本期投入 − 本期转出；期初 = 上一期期末，跨期连续".to_string(),
        "余额为负数说明结转之后归集行又被作废 / 软删（或有人改库），不是舍入误差".to_string(),
    ]
}

// ==================== HTTP 入口 ====================

/// 在制批次余额表（截止某期，一批一行）。
#[nik::permission(
    path = "/business/erp/cost-collect-sheet/wip-balance",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-collect-sheet/wip-balance")]
pub async fn cost_batch_wip_balance_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<WipQuery>,
) -> ResResult<WipBalanceRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(balance_of(&principal, query).await?))
}

/// 单批在制台账（逐期：期初 / 投入 / 转出 / 期末）。
#[nik::permission(
    path = "/business/erp/cost-collect-sheet/{object_id}/wip-ledger",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-collect-sheet/{object_id}/wip-ledger")]
pub async fn cost_batch_wip_ledger_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(object_id): Path<String>,
    Query(query): Query<WipLedgerQuery>,
) -> ResResult<WipLedgerRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(ledger_of(&principal, &object_id, query).await?))
}

/// 权限码自检出口（与 `handler::all_permissions` 同形）。
pub fn all_permissions() -> Vec<&'static str> {
    WIP_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::cost::cost_batch_wip_dto::LedgerSeries;
    use nik::prelude::rust_decimal::Decimal;

    #[test]
    fn permission_is_reused_from_cost_object_list() {
        assert_eq!(PERM_READ, "business:erp:cost-object:list");
        assert_eq!(all_permissions(), vec!["business:erp:cost-object:list"]);
    }

    /// 权限码常量必须与路由宏里写的字面量逐字一致（改一处忘改另一处 ⇒ 线上 403 或越权）。
    #[test]
    fn route_macros_declare_the_same_permission_literal() {
        let source = include_str!("cost_batch_wip_handler.rs");
        assert_eq!(
            source.matches("permission = \"business:erp:cost-object:list\"").count(),
            2,
            "两条读接口各声明一次同一个码"
        );
    }

    /// 两条路由都必须是 GET：在制余额是推出来的，**不提供写接口**（写入口在 M5-03/M5-04）。
    #[test]
    fn both_routes_are_get_and_none_is_post() {
        let source = include_str!("cost_batch_wip_handler.rs");
        assert!(source.contains("#[nik::get(\"/business/erp/cost-collect-sheet/wip-balance\")]"));
        assert!(source.contains("#[nik::get(\"/business/erp/cost-collect-sheet/{object_id}/wip-ledger\")]"));
        // 写方法宏的针要拼出来：直接写整串会让 include_str! 把断言本身当成命中（自己咬自己）。
        for method in ["post", "put", "delete", "patch"] {
            let needle = format!("#[nik::{}({}", method, '"');
            assert!(
                !source.contains(&needle),
                "本卡不写库，不该出现 {method} 路由（命中「{needle}」）"
            );
        }
    }

    #[test]
    fn path_param_must_be_uuid_and_404_on_garbage() {
        let id = Uuid::nil();
        assert_eq!(parse_id(&id.to_string()).expect("合法 uuid"), id);
        let error = parse_id("CB-20261101-000001").expect_err("批次号不是 uuid");
        assert_eq!(error.status, 404);
        assert_eq!(error.code, error_code::RESOURCE_NOT_FOUND);
    }

    /// 余额表与台账的路径第二段一个是字面量、一个是参数 —— 字面量不能长得像 id，
    /// 否则 `matchit` 的静态优先就失去意义（用户拿 `wip-balance` 当 id 会撞进明细）。
    #[test]
    fn literal_segment_is_not_uuid_shaped() {
        assert!(Uuid::parse_str("wip-balance").is_err());
        assert!(Uuid::parse_str("wip-ledger").is_err());
    }

    #[test]
    fn invalid_period_is_400_before_touching_the_database() {
        let error = dto::period_of(Some("2026-1")).expect_err("2026-1 不是 yyyy-MM");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("yyyy-MM"));
    }

    /// 边界说明必须点名截止期间（用户看到的是「截至哪天」，不是「现在」）。
    #[test]
    fn notes_name_the_cutoff_period() {
        let notes = notes("2026-11");
        assert!(notes[0].contains("2026-11"), "{notes:?}");
        assert_eq!(notes.len(), 4);
    }

    /// `Amounts` 是三要素值对象：合计恒等于三项之和（出参的 total 不许另算）。
    #[test]
    fn summary_totals_are_elementwise() {
        let balance = dto::balance_of(
            &Amounts {
                material: Decimal::from(100),
                labor: Decimal::from(20),
                overhead: Decimal::from(3),
            },
            &Amounts {
                material: Decimal::from(40),
                labor: Decimal::ZERO,
                overhead: Decimal::ZERO,
            },
        );
        assert_eq!(balance.total(), Decimal::from(83));
    }

    /// 台账装配出来的序列末行期末必须等于汇总余额（同一个数两种看法）。
    #[test]
    fn ledger_series_closing_matches_summary_balance() {
        let flows = dto::merge_flows(
            vec![
                dto::PeriodFlow {
                    period_code: "2026-10".to_string(),
                    inflow: Amounts {
                        material: Decimal::from(700),
                        labor: Decimal::from(200),
                        overhead: Decimal::from(100),
                    },
                    collect_lines: 3,
                    outflow: Amounts::default(),
                    transfer_times: 0,
                },
                dto::PeriodFlow {
                    period_code: "2026-11".to_string(),
                    inflow: Amounts::default(),
                    collect_lines: 0,
                    outflow: Amounts {
                        material: Decimal::from(300),
                        labor: Decimal::ZERO,
                        overhead: Decimal::ZERO,
                    },
                    transfer_times: 1,
                },
            ],
            Vec::new(),
        );
        let series: LedgerSeries = dto::ledger_series(flows, "2026-11");
        assert_eq!(series.closing.material, Decimal::from(400));
        assert_eq!(series.rows.last().expect("末行").closing.total, "700");
    }
}
