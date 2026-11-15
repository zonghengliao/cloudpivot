//! 巡检结果看板接口（只读）+ 手动执行一次。
//!
//! 文件顺序遵循项目约定：**路由方法在前，公共辅助函数在后**。
//!
//! # 权限
//!
//! 读（overview / page / detail）用 `support:invariant:list`；手动执行用
//! `support:invariant:run`。两个都是功能权限（不看行），与 `sys_menu.auth_code` 一一对应。

use nik::prelude::*;

use crate::shared::error_code;
use crate::support::invariant::dto::{
    OverviewQuery, OverviewRes, ResultDetailRes, ResultPageQuery, ResultRes, ScheduleRes, TrendPointRes,
};
use crate::support::invariant::model::{self, ResultFilter};
use crate::support::invariant::runner::{self, Trigger};
use crate::utils;

/// 看板窗口默认天数。
const DEFAULT_DAYS: i64 = 30;
/// 看板窗口上限（防止有人传 days=99999 把聚合拉爆）。
const MAX_DAYS: i64 = 365;

/// 看板总览：最近一次结论 + 近 N 天趋势 + 当前失败项。
///
/// # 参数
///
/// - `query.days`：趋势窗口天数（默认 30，clamp 到 1..=365）。
///
/// # 返回值
///
/// 成功返回看板数据；从未跑过巡检时 `latest = null`、`trend` 为空。
///
/// # 异常情况
///
/// 数据库异常返回 500。
#[nik::permission(
    path = "/support/invariant/overview",
    method = "GET",
    permission = "support:invariant:list"
)]
#[nik::get("/support/invariant/overview")]
pub async fn overview(Query(query): Query<OverviewQuery>) -> ResResult<OverviewRes> {
    let days = query.days.unwrap_or(DEFAULT_DAYS).clamp(1, MAX_DAYS);

    let latest = model::latest()
        .await
        .map_err(|e| utils::db_error(e, "查询最近一次巡检结果"))?;
    let trend = model::trend(days)
        .await
        .map_err(|e| utils::db_error(e, "查询巡检趋势"))?;
    let runs_in_window = model::runs_in_window(days)
        .await
        .map_err(|e| utils::db_error(e, "统计巡检运行次数"))?;
    let fail_streak = model::fail_streak()
        .await
        .map_err(|e| utils::db_error(e, "统计连续失败次数"))?;

    // 「当前失败项」= 最近一次运行的 failures。最近一次是 OK 时它本来就是空数组，
    // 不拿「历史上某次失败的项」冒充当前问题（那会让人以为账现在还错着）。
    let failures = latest
        .as_ref()
        .map(|row| crate::support::invariant::dto::decode_items(row.failures.as_ref()))
        .unwrap_or_default();

    Ok(Res::ok(OverviewRes {
        window_days: days,
        generated_at: chrono::Utc::now(),
        latest: latest.as_ref().map(ResultRes::from),
        trend: trend.into_iter().map(TrendPointRes::from).collect(),
        failures,
        fail_streak,
        runs_in_window,
        schedule: ScheduleRes {
            task_name: runner::TASK_NAME.to_string(),
            cron: runner::CRON.to_string(),
            description: "内置定时任务，每天 02:20 自动执行（无需在定时任务页启用）".to_string(),
        },
    }))
}

/// 运行历史分页。
///
/// # 参数
///
/// - `query`：`page` / `page_size` / `status?`（`OK` / `FAIL` / `ERROR`，大小写不敏感）。
///
/// # 返回值
///
/// 按 `run_at` 倒序的分页列表（只带计数，不带全量明细）。
///
/// # 异常情况
///
/// 数据库异常返回 500。
#[nik::permission(
    path = "/support/invariant/page",
    method = "GET",
    permission = "support:invariant:list"
)]
#[nik::get("/support/invariant/page")]
pub async fn page(Query(query): Query<ResultPageQuery>) -> ResResult<PageData<Vec<ResultRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let filter = ResultFilter { status: query.status };
    let paged = model::page_results(&filter, page, page_size)
        .await
        .map_err(|e| utils::db_error(e, "查询巡检运行历史"))?;
    let list = paged.list.iter().map(ResultRes::from).collect::<Vec<_>>();
    Ok(Res::page(list, paged.total, paged.page, paged.size))
}

/// 单次运行的完整明细。
///
/// # 参数
///
/// - `id`：结果 ID。
///
/// # 返回值
///
/// 成功返回摘要 + 全量检查明细 + 失败项子集。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：记录不存在；
/// - 500：数据库异常。
#[nik::permission(
    path = "/support/invariant/{id}",
    method = "GET",
    permission = "support:invariant:list"
)]
#[nik::get("/support/invariant/{id}")]
pub async fn detail(Path(id): Path<String>) -> ResResult<ResultDetailRes> {
    let id = utils::require_uuid(&id, "巡检结果 ID")?;
    let record = model::find_by_id(id)
        .await
        .map_err(|e| utils::db_error(e, "查询巡检结果详情"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "巡检结果不存在"))?;
    Ok(Res::ok(ResultDetailRes::from_model(&record)))
}

/// 立即执行一次巡检并落库（同步等待结果，最长 300s）。
///
/// # 返回值
///
/// 成功返回本次运行结果（含全量明细）。注意：即使巡检发现账务问题（`status = FAIL`），
/// 本接口仍返回 **200** —— 那是账务结论而不是接口失败；接口失败只发生在「写库失败」。
///
/// # 异常情况
///
/// 数据库异常返回 500。脚本没跑成时返回 200 + `status = ERROR` + `error` 原因
/// （这样前端能看到原因，且这次尝试本身也是一条被记录的事实）。
#[nik::permission(
    path = "/support/invariant/run",
    method = "POST",
    permission = "support:invariant:run"
)]
#[nik::post("/support/invariant/run")]
pub async fn run() -> ResResult<ResultDetailRes> {
    let record = runner::run_and_store(Trigger::Manual)
        .await
        .map_err(|e| utils::db_error(e, "执行账务不变量巡检并落库"))?;
    Ok(Res::ok(ResultDetailRes::from_model(&record)))
}
