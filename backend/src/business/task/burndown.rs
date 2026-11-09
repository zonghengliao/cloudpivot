//! 燃尽图（批次 E · **E11** 三、燃尽图，只读）。
//!
//! 依据：卡面 `t_834dcc63`「三、燃尽图（只读）」——接口
//! `GET /task/burndown?project_id=|board=` 返回 `{dates, ideal, actual}`；
//! 理想线从起始日期线性递减到 0，实际线是每日剩余未完成任务数（按完成时间反推）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/task/burndown` | `business:task:read` | 燃尽图数据（只读，无写入口） |
//!
//! > 卡面把路径写成 `burntdown`（多一个 `t`），本实现按**正确拼写** `burndown`
//! > 注册（与模块名 `task/burndown.rs` 一致）。前端与验收脚本都用 `/api/business/task/burndown`。
//!
//! # 三条口径（决定了「数字对不对」）
//!
//! 1. **两条线的时间轴是同一个「CST 日期区间」**：`dates` 是闭区间内每天一个点。
//! 2. **理想线**：`ideal[i] = 总量 × (剩余天数 / 总跨度天数)`，最后一个点恒为 `0`
//!    （第 0 个点是起始日，值 = 总量）。
//! 3. **实际线**：`actual[i] = 总量 − 「完成日 < dates[i]」的任务数`。
//!    注意是**严格小于**：起始日（第 0 个点）必然是「一个都没完成」的全量，
//!    所以卡面验收里「10 个任务 / 5 天 / 每天完成 2 个」得到的 6 个点是
//!    `10 / 8 / 6 / 4 / 2 / 0`（不是 `8 / 6 / 4 / 2 / 0`）。
//!
//! # 为什么「已完成」用 `done_at` 而不是 `status = 'DONE'`
//!
//! 任务被重开（`DONE → DOING`）时 `done_at` 会被清空（T2 状态机），但历史燃尽
//! 必须反映「当时完成过」。用 `done_at IS NOT NULL` 与卡面的「按 completed_at 反推」
//! 同义（本库没有 `completed_at`，等价列是 `done_at`）。
//!
//! # 可见性
//!
//! 与任务列表完全同一条谓词（[`model::VISIBLE_OR_TAIL`]）：
//! `board=all` → 「数据范围 ∪ 我参与」；`board=my` → 只算「我参与」的任务
//! （约等于列表页的「只看我的任务」开关）。**不存在第二条可见性实现**。

use axum::extract::Query;
use nik::prelude::chrono::{Duration, NaiveDate};
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::business::shared::scope;
use crate::business::task::handler::{param_invalid, parse_uuid_field};
use crate::business::task::model;
use crate::shared::error_code;
use crate::utils;

// ==================== 常量 ====================

/// 时间窗上限（天）。超过一律 400 —— 一年以上的燃尽图没有阅读价值，
/// 而且每天一个点会让前端 ECharts 的类目轴挤成一团。
pub const MAX_WINDOW_DAYS: i64 = 366;

/// `board` 参数取值：全部可见任务。
pub const BOARD_ALL: &str = "all";
/// `board` 参数取值：只看我参与的任务。
pub const BOARD_MY: &str = "my";

// ==================== 请求 / 响应 ====================

/// 燃尽图查询（`GET /business/task/burndown`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskBurndownQuery {
    /// 项目 id（可空；空 = 不按项目过滤）。
    pub project_id: Option<String>,
    /// 看板维度：`all`（缺省）/ `my`。
    pub board: Option<String>,
    /// 起始日期（缺省 = 任务的最早创建日，CST）。
    pub start: Option<NaiveDate>,
    /// 截止日期（缺省 = 任务的最晚截止日；都没有则取起始日 + 4 天）。
    pub end: Option<NaiveDate>,
}

/// 燃尽图响应（**只读**，无任何写字段）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskBurndownRes {
    /// 类目轴：`YYYY-MM-DD`，闭区间内每天一个。
    pub dates: Vec<String>,
    /// 理想线（浮点，保留 2 位）。
    pub ideal: Vec<f64>,
    /// 实际线（当日开始时剩余的未完成任务数）。
    pub actual: Vec<i64>,
    /// 区间内任务总量。
    pub total: i64,
    /// 区间内已完成任务量（`done_at` 非空）。
    pub completed: i64,
    /// 起始日期。
    pub start: String,
    /// 截止日期。
    pub end: String,
    /// 项目 id（空串 = 不限项目）。
    pub project_id: String,
    /// 看板维度（`all` / `my`）。
    pub board: String,
}

/// 燃尽图的三条线（纯计算结果）。
#[derive(Debug, Clone, PartialEq)]
pub struct BurndownSeries {
    /// 类目轴。
    pub dates: Vec<String>,
    /// 理想线。
    pub ideal: Vec<f64>,
    /// 实际线。
    pub actual: Vec<i64>,
}

// ==================== 纯逻辑（可无库单测） ====================

/// 计算燃尽图的三条线。
///
/// # 参数
///
/// - `total`：区间内任务总量；
/// - `start` / `end`：CST 日期闭区间（`end` 必须 ≥ `start`）；
/// - `daily`：每个「完成日」完成了多少任务（顺序无关）。
///
/// # 返回值
///
/// [`BurndownSeries`]；`dates.len() == ideal.len() == actual.len() == (end-start).days + 1`。
///
/// # 口径
///
/// - `ideal[i] = total × (days - i) / days`（`days = end - start`，最后一点为 0）；
/// - `actual[i] = total − Σ{daily.date < dates[i]}`（严格小于 → 首点为全量）。
pub fn build_series(total: i64, start: NaiveDate, end: NaiveDate, daily: &[(NaiveDate, i64)]) -> BurndownSeries {
    let span = (end - start).num_days().max(0);
    let count = (span + 1) as usize;
    let mut dates = Vec::with_capacity(count);
    let mut ideal = Vec::with_capacity(count);
    let mut actual = Vec::with_capacity(count);
    for index in 0..count {
        let date = start + Duration::days(index as i64);
        dates.push(date.format("%Y-%m-%d").to_string());
        let remaining_ratio = if span == 0 {
            0.0
        } else {
            (span - index as i64) as f64 / span as f64
        };
        let ideal_value = (total as f64 * remaining_ratio * 100.0).round() / 100.0;
        ideal.push(ideal_value);
        let done_before: i64 = daily
            .iter()
            .filter(|(done_date, _)| *done_date < date)
            .map(|(_, count)| *count)
            .sum();
        actual.push((total - done_before).max(0));
    }
    BurndownSeries { dates, ideal, actual }
}

// ==================== 数据访问 ====================

/// 区间内的事实数据（总量 / 已完成 / 首末日期 / 每日完成数）。
#[derive(Debug, Clone, Default)]
struct BurndownFacts {
    /// 任务总量。
    total: i64,
    /// 已完成量（`done_at` 非空）。
    completed: i64,
    /// 最早创建日（CST）。
    first_date: Option<NaiveDate>,
    /// 最晚截止日。
    last_due: Option<NaiveDate>,
    /// 每日完成数。
    daily: Vec<(NaiveDate, i64)>,
}

/// 可见性 + 项目过滤的 WHERE 片段与参数（**两条查询共用同一份**）。
///
/// 占位符文本顺序：`{FILTER}` 内 scope 参数 → 「我参与」id 数组 → 项目 id。
fn where_clause(
    principal: &ScopedPrincipal,
    scope_ids: &model::MyScopeIds,
    project_id: Option<Uuid>,
    mine_only: bool,
) -> (String, Vec<Value>) {
    let mut sql = String::from(" AND t.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if mine_only {
        sql.push_str(" AND t.id = ANY(?::text::uuid[])");
        params.push(scope_ids.param());
    } else {
        let filter = scope::filter_for(principal, model::TASK_TARGET);
        sql.push_str(&model::VISIBLE_OR_TAIL.replace("{FILTER}", filter.sql()));
        params.extend_from_slice(filter.params());
        params.push(scope_ids.param());
    }
    if let Some(id) = project_id {
        sql.push_str(" AND t.project_id = ?::uuid");
        params.push(Value::from(&id));
    }
    (sql, params)
}

/// 拉取燃尽图事实数据（两条聚合查询，同一份 WHERE）。
async fn facts(principal: &ScopedPrincipal, project_id: Option<Uuid>, mine_only: bool) -> DbResult<BurndownFacts> {
    let scope_ids = model::my_member_task_ids(principal.user_id()).await?;
    let (where_sql, params) = where_clause(principal, &scope_ids, project_id, mine_only);

    let totals_sql = format!(
        "SELECT count(*)::bigint AS total, \
                count(*) FILTER (WHERE t.done_at IS NOT NULL)::bigint AS completed, \
                min((t.created_at AT TIME ZONE 'Asia/Shanghai')::date) AS first_date, \
                max(t.due_date) AS last_due \
           FROM biz_task t WHERE 1 = 1 {where_sql}"
    );
    let totals = db().find_with_params(&totals_sql, params.clone()).await?;
    let mut facts = BurndownFacts::default();
    if let Some(row) = totals.first() {
        facts.total = row.get_typed("total")?;
        facts.completed = row.get_typed("completed")?;
        facts.first_date = row.get_typed("first_date")?;
        facts.last_due = row.get_typed("last_due")?;
    }

    let daily_sql = format!(
        "SELECT (t.done_at AT TIME ZONE 'Asia/Shanghai')::date AS done_date, \
                count(*)::bigint AS done_count \
           FROM biz_task t \
          WHERE 1 = 1 {where_sql} AND t.done_at IS NOT NULL \
          GROUP BY 1 ORDER BY 1"
    );
    let daily_rows = db().find_with_params(&daily_sql, params).await?;
    for row in &daily_rows {
        let date: NaiveDate = row.get_typed("done_date")?;
        let count: i64 = row.get_typed("done_count")?;
        facts.daily.push((date, count));
    }
    Ok(facts)
}

/// 解析区间：显式入参 → 数据事实 → 兜底（起始日 + 4 天）。
///
/// # 异常情况
///
/// 区间倒置或超过 [`MAX_WINDOW_DAYS`] → 400 + `63504`。
fn resolve_window(
    query: &TaskBurndownQuery,
    facts: &BurndownFacts,
) -> std::result::Result<(NaiveDate, NaiveDate), ApiError> {
    let today = model::today_cst();
    let start = query.start.or(facts.first_date).unwrap_or(today);
    let end = query
        .end
        .or(facts.last_due)
        .unwrap_or_else(|| start + Duration::days(4));
    if end < start {
        return Err(ApiError::new(
            400,
            error_code::TASK_BURNDOWN_RANGE_INVALID,
            format!("日期区间非法：截止日 {end} 早于起始日 {start}"),
        ));
    }
    // 单点区间没有「递减」可言 → 补一天，保证至少两个点（前端的线才画得出来）
    let end = if end == start { start + Duration::days(1) } else { end };
    let span = (end - start).num_days();
    if span > MAX_WINDOW_DAYS {
        return Err(ApiError::new(
            400,
            error_code::TASK_BURNDOWN_RANGE_INVALID,
            format!("日期区间过长：{span} 天（上限 {MAX_WINDOW_DAYS} 天）"),
        ));
    }
    Ok((start, end))
}

// ==================== handler ====================

/// 燃尽图数据（只读）。
///
/// # 异常情况
///
/// - 400 + `10001`：uuid / 日期 / `board` 取值非法；
/// - 400 + `63504`：区间倒置或超过 366 天；
/// - 500：数据库异常（含数据权限片段形态异常）。
#[nik::permission(path = "/business/task/burndown", method = "GET", permission = "business:task:read")]
#[nik::get("/business/task/burndown")]
pub async fn burndown(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<TaskBurndownQuery>,
) -> ResResult<TaskBurndownRes> {
    let principal = principal::resolve_for_claims(&claims).await?;
    let project_id = match query.project_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(parse_uuid_field(raw, "项目")?),
    };
    let board = match query.board.as_deref().map(str::trim) {
        None | Some("") => BOARD_ALL.to_string(),
        Some(raw) if raw.eq_ignore_ascii_case(BOARD_MY) => BOARD_MY.to_string(),
        Some(raw) if raw.eq_ignore_ascii_case(BOARD_ALL) => BOARD_ALL.to_string(),
        Some(raw) => {
            return Err(param_invalid(format!("board 取值非法：{raw}（应为 all / my）")));
        }
    };
    let mine_only = board == BOARD_MY;
    let facts = facts(&principal, project_id, mine_only)
        .await
        .map_err(|error| utils::db_error(error, "统计燃尽图数据"))?;
    let (start, end) = resolve_window(&query, &facts)?;
    let series = build_series(facts.total, start, end, &facts.daily);

    tracing::info!(
        project_id = ?project_id,
        board = %board,
        total = facts.total,
        completed = facts.completed,
        points = series.dates.len(),
        "燃尽图数据已生成"
    );
    Ok(Res::ok(TaskBurndownRes {
        dates: series.dates,
        ideal: series.ideal,
        actual: series.actual,
        total: facts.total,
        completed: facts.completed,
        start: start.format("%Y-%m-%d").to_string(),
        end: end.format("%Y-%m-%d").to_string(),
        project_id: project_id.map(|id| id.to_string()).unwrap_or_default(),
        board,
    }))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("测试日期必须合法")
    }

    /// 卡面验收口径：10 个任务 / 5 天 / 每天完成 2 个 → 实际线 10/8/6/4/2/0。
    #[test]
    fn series_matches_acceptance_numbers() {
        let start = date(2026, 11, 1);
        let end = date(2026, 11, 6);
        let daily = vec![
            (date(2026, 11, 1), 2),
            (date(2026, 11, 2), 2),
            (date(2026, 11, 3), 2),
            (date(2026, 11, 4), 2),
            (date(2026, 11, 5), 2),
        ];
        let series = build_series(10, start, end, &daily);
        assert_eq!(series.dates.len(), 6, "5 天跨度 → 6 个点");
        assert_eq!(series.actual, vec![10, 8, 6, 4, 2, 0]);
        assert_eq!(series.ideal, vec![10.0, 8.0, 6.0, 4.0, 2.0, 0.0]);
        assert_eq!(series.dates[0], "2026-11-01");
        assert_eq!(series.dates[5], "2026-11-06");
    }

    /// 理想线：不与实际线耦合，总量 7 天 3 个点 → 7 / 3.5 / 0。
    #[test]
    fn ideal_line_is_linear_to_zero() {
        let series = build_series(7, date(2026, 11, 1), date(2026, 11, 3), &[]);
        assert_eq!(series.ideal, vec![7.0, 3.5, 0.0]);
        assert_eq!(series.actual, vec![7, 7, 7], "没人完成 → 实际线不动");
    }

    /// 边界：区间外 / 区间之后的完成数不计入区间内的剩余量。
    #[test]
    fn series_ignores_completions_outside_window() {
        let start = date(2026, 11, 3);
        let end = date(2026, 11, 5);
        let daily = vec![
            (date(2026, 11, 1), 4), // 窗口之前完成 → 首点就已扣掉
            (date(2026, 11, 9), 1), // 窗口之后完成 → 不影响
        ];
        let series = build_series(6, start, end, &daily);
        assert_eq!(series.actual, vec![2, 2, 2]);
    }

    /// 区间退化（首末同日）只有一个点，不 panic、不出现 NaN。
    #[test]
    fn series_single_point_is_safe() {
        let day = date(2026, 11, 1);
        let series = build_series(0, day, day, &[]);
        assert_eq!(series.dates, vec!["2026-11-01".to_string()]);
        assert_eq!(series.ideal, vec![0.0]);
        assert_eq!(series.actual, vec![0]);
    }
}
