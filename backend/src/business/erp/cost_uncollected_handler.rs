//! 「未归集清单」接口层（成本核算 · ARCH-9 §5.2 #33 / §6.2.2 ② · B2-12）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-period/...`。
//!
//! | # | 方法 | 路径 | 权限码 | 入参 |
//! |---|---|---|---|---|
//! | 33 | GET | `/business/erp/cost-period/{id}/uncollected` | `business:erp:cost-period:list` | 批次 id（设计原文） |
//! | 33b | GET | `/business/erp/cost-period/uncollected?period_code=yyyy-MM` | `business:erp:cost-period:list` | 期间（**本卡新增**，见下） |
//!
//! # 为什么多给一个「按期间」的入口（33b）
//!
//! 设计的向导是 `① 选期间 → ② 自查缺口 → ③ 计算`，而 #33 的 `{id}` 是**批次 id** ——
//! 批次是第 ③ 步 `POST /cost-period/calc` 才创建的（§5.2 #27「不存在则建 DRAFT」）。
//! 也就是说按设计原文，第 ② 步在**第一次核算的期间**根本没有 id 可传（死锁）。
//! 所以补一个按 `period_code` 查的入口：批次存在与否都能自查，出参结构完全一致
//! （`batch` 字段为 `None` 时前端只显示「还没建批次」）。设计原文的 33 号接口**照实现**，
//! 两条路由共用同一个装配函数，不存在两套口径。
//!
//! # 不阻断
//!
//! 设计 §6.2.2「不阻断（可以带着缺口算，但结算前必须有解释）」⇒ 本接口**永远 200**：
//! 有缺口就列出来，没有就给 `count = 0` 的空块。只有三种情况非 200：
//! 期间编码非法（400）、批次 id 不存在（404）、查库失败（500）。
//!
//! # 数据权限
//!
//! unscoped + 权限码（与 `cost-period` 单头同口径，设计 §5.4）：缺口是**全公司期间口径**的
//! 自查，按部门 scope 过滤会让财务看不到别的车间的未归集项，正好把缺口藏起来。
//!
//! # 四块怎么装配
//!
//! ```text
//! ① 未挂成本对象的领料  → cost_uncollected_model::stock_out_gap_{total,rows}
//! ② 缺工时的在产对象    → cost_period_item_model::objects_of_period（与第 6 段同一份对象集）
//!                        × cost_uncollected_model::labor_counts_of_period
//! ③ 缺费率的工时行      → cost_collect_model::labor_source_lines + LaborLine::uncollected_reason
//!                        （B2-7 的口径不重写，只重查一遍库）
//! ④ 未摊完的费用池      → cost_uncollected_model::pool_buckets − rules_covering / active_object_depts
//! ```

use axum::extract::{Path, Query};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Deserialize;

use crate::business::erp::cost_collect_dto;
use crate::business::erp::cost_collect_model;
use crate::business::erp::cost_labor_collect_dto::LaborLine;
use crate::business::erp::cost_object_dto;
use crate::business::erp::cost_period_item_model;
use crate::business::erp::cost_rate_dto::{self as rate_dto, RATE_TYPE_LABOR, RateRequest, RateSubject};
use crate::business::erp::cost_rate_handler;
use crate::business::erp::cost_uncollected_dto::{
    self as dto, BatchBrief, LaborMissingGap, LaborRateGap, PoolGap, StockOutGap, UncollectedRes,
};
use crate::business::erp::cost_uncollected_model::{self as model, BatchBriefRow};
use crate::business::erp::ledger;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（设计 §5.3：本接口属 cost-period:list） ====================

/// 列表 / 自查（与 `cost-period` 单头同码）。**本卡不新增权限码**，也不新增迁移。
pub const PERM_LIST: &str = "business:erp:cost-period:list";

/// 本模块全部权限码（与 `handler::all_permissions` 同形，便于汇总脚本收集）。
pub const UNCOLLECTED_PERMISSIONS: [&str; 1] = [PERM_LIST];

/// 已结算（缺口只作历史解释）的批次状态。
pub const SETTLED_STATUSES: [&str; 2] = ["SETTLED", "CLOSED"];

// ==================== 入参 ====================

/// `GET /cost-period/uncollected?period_code=yyyy-MM`。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UncollectedQuery {
    /// 期间（必填；`yyyy-MM`）。
    pub period_code: Option<String>,
}

/// 归一化期间入参（纯逻辑，单测可直接喂）。
pub fn period_of(query: &UncollectedQuery) -> std::result::Result<String, ApiError> {
    let raw = query
        .period_code
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            ApiError::new(
                400,
                error_code::PARAM_INVALID,
                "period_code 必填（yyyy-MM，例如 2026-11）".to_string(),
            )
        })?;
    if ledger::month_range(raw).is_none() {
        return Err(cost_collect_dto::period_invalid(raw));
    }
    Ok(raw.to_string())
}

// ==================== 错误出口 ====================

fn batch_not_found(id: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!(
            "成本核算批次不存在或已删除（{id}）。若要按期间自查，请用 \
             GET /business/erp/cost-period/uncollected?period_code=yyyy-MM"
        ),
    )
}

// ==================== 装配 ====================

/// ① 未挂成本对象的领料块。
async fn stock_out_block(
    start: nik::prelude::chrono::NaiveDate,
    end: nik::prelude::chrono::NaiveDate,
) -> std::result::Result<dto::GapBlock<StockOutGap>, ApiError> {
    let (count, amount) = model::stock_out_gap_total(db(), start, end)
        .await
        .map_err(|error| utils::db_error(error, "统计未挂成本对象的领料单"))?;
    let rows = model::stock_out_gap_rows(db(), start, end, dto::ROW_LIMIT)
        .await
        .map_err(|error| utils::db_error(error, "查询未挂成本对象的领料单"))?;
    let gaps: Vec<StockOutGap> = rows
        .iter()
        .map(|row| StockOutGap {
            stock_out_id: row.id.to_string(),
            doc_no: row.out_no.clone(),
            doc_date: row.out_date.to_string(),
            out_type: row.out_type.clone(),
            warehouse_name: row.warehouse_name.clone(),
            item_count: row.item_count,
            qty_total: row.qty_total.to_string(),
            amount_total: row.amount_total.to_string(),
            hint: dto::stock_out_hint(&row.out_no),
            jump: dto::jump_stock_out(&row.out_no),
        })
        .collect();
    let hint = if count == 0 {
        "本期没有「已过账但没挂成本对象」的领料单".to_string()
    } else {
        format!(
            "{count} 张已过账领料单没挂成本对象：它们的领料成本不进成本核算（按存量口径直接进 6401）。\
             要摊到批上，请作废后带上成本对象重开"
        )
    };
    Ok(dto::block_of(
        dto::KIND_STOCK_OUT,
        "未挂成本对象的领料",
        hint,
        Some(dto::jump_stock_out_list(&start.to_string(), &end.to_string())),
        count,
        amount,
        gaps,
    ))
}

/// ② 缺工时的在产对象块。
///
/// 对象集与第 6 段**同一份**（`objects_of_period`：本期有归集 ∪ 上期有期末在制），
/// 只保留 `OPEN`（「在产」，设计 §6.2.2 原文）且本期**没有** `CONFIRMED` 工时的。
async fn labor_missing_block(period_code: &str) -> std::result::Result<dto::GapBlock<LaborMissingGap>, ApiError> {
    let objects = cost_period_item_model::objects_of_period(db(), period_code)
        .await
        .map_err(|error| utils::db_error(error, "取期内要点计算单的成本对象"))?;
    let counts = model::labor_counts_of_period(db(), period_code)
        .await
        .map_err(|error| utils::db_error(error, "统计当期工时行数"))?;

    let mut gaps: Vec<LaborMissingGap> = Vec::new();
    let mut material_total = Decimal::ZERO;
    for object in objects
        .iter()
        .filter(|object| object.status == cost_object_dto::STATUS_OPEN)
    {
        let (confirmed, draft) = counts
            .iter()
            .find(|row| row.cost_object_id == object.cost_object_id)
            .map(|row| (row.confirmed_rows, row.draft_rows))
            .unwrap_or((0, 0));
        if confirmed > 0 {
            continue;
        }
        material_total += object.mat_cur;
        gaps.push(LaborMissingGap {
            cost_object_id: object.cost_object_id.to_string(),
            object_no: object.object_no.clone(),
            draft_labor_rows: draft,
            material_amount: object.mat_cur.to_string(),
            hint: dto::labor_missing_hint(&object.object_no, draft),
            jump: dto::jump_labor(period_code, object.cost_object_id),
        });
    }

    let count = i64::try_from(gaps.len()).unwrap_or(0);
    gaps.truncate(dto::ROW_LIMIT);
    let hint = if count == 0 {
        "本期每个在产对象都有确认工时".to_string()
    } else {
        format!("{count} 个在产对象本期没有确认工时：它们的人工成本会算成 0。请在工时台账补录并确认后重算")
    };
    Ok(dto::block_of(
        dto::KIND_LABOR_MISSING,
        "缺工时的在产对象",
        hint,
        Some(dto::jump_labor_root()),
        count,
        material_total,
        gaps,
    ))
}

/// ③ 缺费率的工时行块（口径 = B2-7 的 `uncollected_reason`，这里只是**再查一遍库**）。
async fn labor_rate_block(period_code: &str) -> std::result::Result<dto::GapBlock<LaborRateGap>, ApiError> {
    let lines = cost_collect_model::labor_source_lines(db(), period_code)
        .await
        .map_err(|error| utils::db_error(error, "扫描当期已确认工时"))?;
    let uncollected: Vec<&LaborLine> = lines
        .iter()
        .filter(|line| line.uncollected_reason().is_some())
        .collect();

    // 只有真有缺费率行时才去查费率表（空清单不该多打一次库）
    let candidates = if uncollected.is_empty() {
        Vec::new()
    } else {
        cost_rate_handler::candidates(RATE_TYPE_LABOR).await.unwrap_or_default()
    };

    let mut hours_total = Decimal::ZERO;
    let mut gaps: Vec<LaborRateGap> = Vec::with_capacity(uncollected.len());
    for line in &uncollected {
        hours_total += line.hours;
        gaps.push(LaborRateGap {
            labor_id: line.labor_id.to_string(),
            cost_object_id: line.cost_object_id.to_string(),
            cost_object_no: line.cost_object_no.clone(),
            employee_name: line.employee_name.clone(),
            dept_id: line.dept_id.to_string(),
            dept_name: line.dept_name.clone(),
            work_date: line.work_date.to_string(),
            hours: line.hours.to_string(),
            rate: line.rate.to_string(),
            reason: line.uncollected_reason().unwrap_or_default(),
            hint: rate_hint(line, &candidates),
            jump: dto::jump_labor(period_code, line.cost_object_id),
        });
    }

    let count = i64::try_from(gaps.len()).unwrap_or(0);
    gaps.truncate(dto::ROW_LIMIT);
    let hint = if count == 0 {
        "本期没有「工资率为 0」的已确认工时".to_string()
    } else {
        format!(
            "{count} 行已确认工时的工资率为 0：这些行不写归集、也不按 0 计（人工成本会少算）。\
             补费率或重录后重新确认，再重算"
        )
    };
    Ok(dto::labor_rate_block_of("缺费率的工时", hint, count, hours_total, gaps))
}

/// 单行缺费率的「怎么修」（与 B2-7 `cost_calc_labor::fill_repair_hints` 同一套口径）：
/// 现在能不能取到费率 / 该去配哪一级。
fn rate_hint(line: &LaborLine, candidates: &[rate_dto::RateRow]) -> String {
    if candidates.is_empty() {
        return format!(
            "费率取数三级（对象 > 车间 > 公司）都没命中：请先维护成本费率（生效期要覆盖 {}），\
             再回工时台账补这一行的工资率并重新确认",
            line.work_date
        );
    }
    let subject = RateSubject {
        cost_object_no: line.cost_object_no.as_deref().unwrap_or("-"),
        dept_name: line.dept_name.as_deref().unwrap_or("-"),
    };
    let request = RateRequest::labor(line.cost_object_id, line.dept_id, line.work_date);
    match rate_dto::resolve(candidates, &request, &subject) {
        Ok(resolved) => format!(
            "现在已能取到 {} 元/小时（来源：{}）：请在工时台账把这一行的工资率补上（或作废重录）\
             后重新确认，再重算",
            resolved.rate.normalize(),
            resolved.scope_detail(&subject)
        ),
        Err(error) => format!(
            "费率取数三级都没命中：{} 请先维护成本费率（生效期要覆盖 {}），再重算",
            error.msg, line.work_date
        ),
    }
}

/// ④ 未摊完的费用池块。
async fn pool_block(
    period_code: &str,
    start: nik::prelude::chrono::NaiveDate,
    end: nik::prelude::chrono::NaiveDate,
) -> std::result::Result<dto::GapBlock<PoolGap>, ApiError> {
    let buckets = model::pool_buckets(db(), period_code)
        .await
        .map_err(|error| utils::db_error(error, "汇总当期费用池"))?;
    let rules: std::collections::HashSet<(Uuid, String)> = model::rules_covering(db(), start, end)
        .await
        .map_err(|error| utils::db_error(error, "查询生效分摊规则"))?
        .into_iter()
        .collect();
    let active_depts: std::collections::HashSet<Uuid> = model::active_object_depts(db(), period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询本期有活动的在产对象车间"))?
        .into_iter()
        .collect();

    let mut remaining_total = Decimal::ZERO;
    let mut gaps: Vec<PoolGap> = Vec::new();
    for bucket in &buckets {
        let has_rule = rules.contains(&(bucket.dept_id, bucket.account_code.clone()));
        let has_object = active_depts.contains(&bucket.dept_id);
        let Some(reason) = dto::pool_reason(has_rule, has_object, bucket.remaining_amount) else {
            continue;
        };
        remaining_total += bucket.remaining_amount;
        let label = dto::account_label(&bucket.account_code, bucket.account_name.as_deref());
        gaps.push(PoolGap {
            dept_id: bucket.dept_id.to_string(),
            dept_name: bucket.dept_name.clone(),
            account_code: bucket.account_code.clone(),
            account_name: bucket.account_name.clone(),
            account_label: label.clone(),
            pool_amount: bucket.pool_amount.to_string(),
            allocated_amount: bucket.allocated_amount.to_string(),
            remaining_amount: bucket.remaining_amount.to_string(),
            source_rows: bucket.source_rows,
            reason: reason.to_string(),
            reason_label: dto::reason_label(reason).to_string(),
            hint: dto::pool_hint(reason, &bucket.dept_name, &label, bucket.remaining_amount),
            // 「未摊完」的动作就在本页（点「计算」），不该把人送去别的页面
            jump: match reason {
                dto::REASON_NOT_ALLOCATED => None,
                _ => Some(dto::jump_alloc_rule()),
            },
        });
    }

    let count = i64::try_from(gaps.len()).unwrap_or(0);
    gaps.truncate(dto::ROW_LIMIT);
    let hint = if count == 0 {
        "本期费用池已全部摊完（或本期没有费用池）".to_string()
    } else {
        format!(
            "{count} 个（车间, 科目）费用池有未摊完余额：结算前必须摊完（补规则 / 建对象 / 重算）\
             或确认该科目不进成本"
        )
    };
    Ok(dto::block_of(
        dto::KIND_POOL,
        "未摊完的费用池",
        hint,
        Some(dto::jump_cost_period(period_code)),
        count,
        remaining_total,
        gaps,
    ))
}

// ==================== 业务入口（单测 / 真库用例直接调） ====================

/// 按期间自查缺口（两条路由的共用装配）。
///
/// `batch`：当期活跃批次（没有就 `None` —— 向导第 ② 步可能还没建批次）。
pub async fn uncollected_of_period(
    period_code: &str,
    batch: Option<BatchBriefRow>,
) -> std::result::Result<UncollectedRes, ApiError> {
    let (start, end) = ledger::month_range(period_code).ok_or_else(|| cost_collect_dto::period_invalid(period_code))?;

    let stock_out_without_object = stock_out_block(start, end).await?;
    let labor_missing = labor_missing_block(period_code).await?;
    let labor_rate_missing = labor_rate_block(period_code).await?;
    let pool_unallocated = pool_block(period_code, start, end).await?;

    let settled = batch
        .as_ref()
        .map(|row| SETTLED_STATUSES.contains(&row.status.as_str()))
        .unwrap_or(false);

    let res = UncollectedRes {
        period_code: period_code.to_string(),
        period_start: start.to_string(),
        period_end: end.to_string(),
        batch: batch.map(|row| BatchBrief {
            id: row.id.to_string(),
            batch_no: row.batch_no,
            status: row.status,
        }),
        settled,
        gap_count: 0,
        stock_out_without_object,
        labor_missing,
        labor_rate_missing,
        pool_unallocated,
    };
    let gap_count = res.total_gaps();
    Ok(UncollectedRes { gap_count, ..res })
}

/// 按期间编码自查（`GET /cost-period/uncollected?period_code=`）。
///
/// 批次信息取「活跃批次」（排除 `REVERSED` / `CANCELLED` / 软删，与 `uk_biz_cost_period`
/// 同口径）：红冲/作废批次不占位，向导该看到的是「这期现在挂着哪张单」。
pub async fn uncollected_by_period(period_code: &str) -> std::result::Result<UncollectedRes, ApiError> {
    let batch = cost_period_item_model::active_batch(db(), period_code)
        .await
        .map_err(|error| utils::db_error(error, "取当期成本批次"))?
        .map(|row| BatchBriefRow {
            id: row.id,
            period_code: period_code.to_string(),
            batch_no: row.batch_no,
            status: row.status,
        });
    uncollected_of_period(period_code, batch).await
}

/// 按批次 id 自查（`GET /cost-period/{id}/uncollected`，设计 #33 原文）。
pub async fn uncollected_by_batch(id: &str) -> std::result::Result<UncollectedRes, ApiError> {
    let id = cost_object_dto::parse_uuid(id, "id")?;
    let batch = model::batch_of_id(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询成本批次"))?
        .ok_or_else(|| batch_not_found(&id.to_string()))?;
    let period_code = batch.period_code.clone();
    uncollected_of_period(&period_code, Some(batch)).await
}

// ==================== HTTP 入口 ====================

/// 未归集清单（设计 §5.2 #33：按批次 id）。
#[nik::permission(
    path = "/business/erp/cost-period/{id}/uncollected",
    method = "GET",
    permission = "business:erp:cost-period:list"
)]
#[nik::get("/business/erp/cost-period/{id}/uncollected")]
pub async fn cost_period_uncollected_api(Path(id): Path<String>) -> ResResult<UncollectedRes> {
    Ok(Res::ok(uncollected_by_batch(&id).await?))
}

/// 未归集清单（按期间；向导第 ② 步还没建批次时用）。
#[nik::permission(
    path = "/business/erp/cost-period/uncollected",
    method = "GET",
    permission = "business:erp:cost-period:list"
)]
#[nik::get("/business/erp/cost-period/uncollected")]
pub async fn cost_period_uncollected_by_period_api(Query(query): Query<UncollectedQuery>) -> ResResult<UncollectedRes> {
    let period_code = period_of(&query)?;
    Ok(Res::ok(uncollected_by_period(&period_code).await?))
}

/// 权限码自检出口（与其它 ERP 模块同形）。
pub fn all_permissions() -> Vec<&'static str> {
    UNCOLLECTED_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- 权限 / 入参 ----------

    #[test]
    fn permission_code_reuses_cost_period_list() {
        // 设计 §5.3 只有 cost-period:list 这一个码管「看成本核算」；本接口不新增码
        assert_eq!(PERM_LIST, "business:erp:cost-period:list");
        assert_eq!(UNCOLLECTED_PERMISSIONS, [PERM_LIST]);
    }

    #[test]
    fn period_is_required_and_validated() {
        let empty = UncollectedQuery { period_code: None };
        let err = period_of(&empty).expect_err("缺 period_code 必须 400");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("period_code"), "{}", err.msg);

        let blank = UncollectedQuery {
            period_code: Some("   ".to_string()),
        };
        assert_eq!(period_of(&blank).expect_err("空白也算缺").status, 400);

        for bad in ["2026-13", "2026-00", "202611", "2026-1"] {
            let query = UncollectedQuery {
                period_code: Some(bad.to_string()),
            };
            let err = period_of(&query).expect_err("非法期间必须 400");
            assert_eq!(err.status, 400, "期间「{bad}」应 400");
        }

        let good = UncollectedQuery {
            period_code: Some(" 2026-11 ".to_string()),
        };
        assert_eq!(period_of(&good).expect("合法期间"), "2026-11", "要去空白");
    }

    #[test]
    fn settled_states_are_settled_and_closed() {
        // 与 `cost_period_item_model::SETTLED_STATUSES` 同口径（期初在制只认这两个）
        assert_eq!(SETTLED_STATUSES, ["SETTLED", "CLOSED"]);
        assert_eq!(
            cost_period_item_model::SETTLED_STATUSES,
            SETTLED_STATUSES,
            "两处状态集必须一致，否则「已结算」标记与期初结转口径会分叉"
        );
    }

    // ---------- 路由形态 ----------

    #[test]
    fn by_id_route_rejects_non_uuid_before_querying() {
        // parse_uuid 是同步纯函数：这里只验错误形态（真库路径见 db 用例）
        let err = cost_object_dto::parse_uuid("not-a-uuid", "id").expect_err("非 UUID 必须 400");
        assert_eq!(err.status, 400);
    }

    #[test]
    fn batch_not_found_points_at_the_period_endpoint() {
        let err = batch_not_found("0190f00d-0000-7000-8000-000000000000");
        assert_eq!(err.status, 404);
        assert_eq!(err.code, error_code::RESOURCE_NOT_FOUND);
        assert!(
            err.msg.contains("uncollected?period_code="),
            "404 文案要告诉用户怎么按期间查：{}",
            err.msg
        );
    }
}
