//! 巡检结果的强类型模型与数据访问。
//!
//! 对应迁移 `migrations/system/V0235__create_sys_invariant_result`。
//!
//! # 粒度：一次运行一行
//!
//! `sys_invariant_result` 不是「一条检查一行」，而是「一次运行一行」：检查条数会随脚本演进
//! 变化（今天 40 条、下周 42 条），拆成行会让跨天对比永远对不齐；一运行一行的口径下，
//! `total / passed / failed` 与 `detail` 永远是同一次运行的同一份快照。
//!
//! # 分层
//!
//! SQL 集中在**本文件**：分页、最新一行、趋势聚合、连续失败次数。handler 只做参数校验与
//! 响应组装，runner 只负责「跑脚本 + 写一行」。

use nik::prelude::*;

use crate::business::shared::scope;

/// 趋势与看板的业务时区。
///
/// 「今天跑了几次 / 哪天开始红」是给人看的语义，必须按北京时间切自然日；用 UTC 切会让
/// 凌晨 02:20 的定时运行落到前一天。中国无夏令时，用固定偏移即可，不引时区库。
pub const REPORT_TIME_ZONE: &str = "Asia/Shanghai";

/// `sys_invariant_result` 表对应的强类型模型（一次巡检运行）。
///
/// # 追加写表
///
/// 只有 `created_at`，没有 `updated_at` / `deleted_at`：结果行是不可变事实，
/// 因此 `#[model(no_timestamps, hard_delete)]`（不挂 `TimestampHook`，`created_at` 由写入方赋值）。
///
/// # 快照字段
///
/// `database_name` / `script_path` 是执行当时的冗余快照：事后改配置也不会让历史行
/// 「说不清到底扫的是哪个库、哪个脚本」。
#[derive(Debug, Clone, Model)]
#[table("sys_invariant_result")]
#[model(no_timestamps, hard_delete)]
pub struct SysInvariantResultModel {
    /// 主键（UUID v7，按时间有序）。
    pub id: uuid::Uuid,
    /// 本次巡检开始时间（趋势按此列切自然日）。
    pub run_at: chrono::DateTime<chrono::Utc>,
    /// 触发方式：`SCHEDULED`（内置 cron）/ `MANUAL`（看板或定时任务页手动）。
    pub trigger_type: String,
    /// 运行结论：`OK` / `FAIL` / `ERROR`。
    pub status: String,
    /// 检查总数。
    pub total: i32,
    /// 通过数。
    pub passed: i32,
    /// 失败数（`ok = false` 的检查项合计）。
    pub failed: i32,
    /// 失败项里 BLOCKER 级别数（> 0 表示账已错）。
    pub blockers: i32,
    /// 失败项里 WARN 级别数。
    pub warns: i32,
    /// 本次运行耗时（毫秒，含脚本启动与全部 SQL）。
    pub duration_ms: i32,
    /// 巡检目标库名（快照）。
    pub database_name: String,
    /// 实际执行的脚本路径（快照）。
    pub script_path: String,
    /// 全量检查明细：`[{name, group, level, ok, detail}]`。
    #[json(nullable)]
    pub detail: Option<JsonValue>,
    /// 失败项子集（看板「当前失败项」直出，不必在 JSON 里过滤）。
    #[json(nullable)]
    pub failures: Option<JsonValue>,
    /// `status = ERROR` 时的原因。
    pub error: Option<String>,
    /// 落库时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 运行历史的过滤条件。
#[derive(Debug, Clone, Default)]
pub struct ResultFilter {
    /// 运行结论（`OK` / `FAIL` / `ERROR`，大小写不敏感）。
    pub status: Option<String>,
}

/// 趋势聚合的一行：**某一天的最后一次运行** + 当天运行次数。
///
/// 为什么取「当天最后一次」而不是当天全部运行的平均/最大值：看板回答的是
/// 「这一天结束时账怎么样」。取平均会让「先红后绿」与「一直红」看起来一样。
#[derive(Debug, Clone)]
pub struct TrendRow {
    /// 业务日（`YYYY-MM-DD`，北京时间）。
    pub date: String,
    /// 当天运行次数（含手动触发）。
    pub runs: i64,
    /// 当天最后一次运行的结论。
    pub status: String,
    /// 当天最后一次运行的检查总数。
    pub total: i32,
    /// 当天最后一次运行的通过数。
    pub passed: i32,
    /// 当天最后一次运行的失败数。
    pub failed: i32,
    /// 当天最后一次运行的 BLOCKER 数。
    pub blockers: i32,
    /// 当天最后一次运行的 WARN 数。
    pub warns: i32,
}

/// 分页查询运行历史。
///
/// # 参数
///
/// - `filter`：过滤条件（结论，可选）。
/// - `page`：页码（从 1 开始）。
/// - `page_size`：每页条数。
///
/// # 返回值
///
/// 按 `run_at` 倒序的分页结果。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn page_results(
    filter: &ResultFilter,
    page: u64,
    page_size: u64,
) -> std::result::Result<Page<SysInvariantResultModel>, DbError> {
    let mut sql = String::from("SELECT * FROM sys_invariant_result WHERE TRUE");
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = filter
        .status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sql.push_str(" AND upper(status) = upper(?)");
        params.push(Value::from(status));
    }
    // SCOPE-EXEMPT：`sys_invariant_result` 是运维类全局事实表（一张表装全库账务体检结论），
    // 没有部门 / 归属人列，不属于架构 §2.5 的数据权限范围；看板权限靠
    // `support:invariant:list` 这个功能权限控制，不注入行级范围。
    let Page {
        list,
        total,
        page,
        size,
        pages,
    } = scope::paginate_unscoped(
        scope::ScopeTarget::unscoped("sys_invariant_result"),
        &sql,
        params,
        Some("ORDER BY run_at DESC"),
        page,
        page_size,
    )
    .await?;
    let list = list
        .into_iter()
        .map(SysInvariantResultModel::from_row)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Page {
        list,
        total,
        page,
        size,
        pages,
    })
}

/// 读取最近一次运行（不分页、只取一行）。
///
/// # 返回值
///
/// 无任何运行记录时返回 `None`（看板据此显示「从未跑过」，而不是假装有一条 OK）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// # 数据权限
///
/// SCOPE-EXEMPT：理由同 [`page_results`]（运维类全局事实表）。
pub async fn latest() -> std::result::Result<Option<SysInvariantResultModel>, DbError> {
    let row = db()
        .find_first("SELECT * FROM sys_invariant_result ORDER BY run_at DESC LIMIT 1")
        .await?;
    row.map(SysInvariantResultModel::from_row).transpose()
}

/// 按 ID 读取单次运行的完整记录。
///
/// # 参数
///
/// - `id`：结果 ID。
///
/// # 返回值
///
/// 不存在时返回 `None`。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// # 数据权限
///
/// SCOPE-EXEMPT：理由同 [`page_results`]。
pub async fn find_by_id(
    id: uuid::Uuid,
) -> std::result::Result<Option<SysInvariantResultModel>, DbError> {
    SysInvariantResultModel::find_or_none(db(), id).await
}

/// 近 `days` 天的每日趋势（按北京时间切自然日）。
///
/// # 参数
///
/// - `days`：窗口天数（调用方已 clamp 到 1..=365）。
///
/// # 返回值
///
/// 按日期升序的趋势行；窗口内没有任何运行时返回空列表（**不补零行**：补齐的零会让
/// 前端画出「当天通过 0 条」这种假数据，正确表达是「这天没跑」）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// # 数据权限
///
/// SCOPE-EXEMPT：理由同 [`page_results`]。
pub async fn trend(days: i64) -> std::result::Result<Vec<TrendRow>, DbError> {
    let sql = format!(
        r#"
WITH window_rows AS (
    SELECT (run_at AT TIME ZONE '{tz}')::date AS day,
           run_at, status, total, passed, failed, blockers, warns
      FROM sys_invariant_result
     WHERE run_at >= now() - make_interval(days => ?::int)
),
daily AS (
    SELECT DISTINCT ON (day) day, status, total, passed, failed, blockers, warns
      FROM window_rows
     ORDER BY day, run_at DESC
),
run_counts AS (
    SELECT day, COUNT(*) AS runs FROM window_rows GROUP BY day
)
SELECT to_char(d.day, 'YYYY-MM-DD') AS date,
       c.runs                        AS runs,
       d.status                      AS status,
       d.total                       AS total,
       d.passed                      AS passed,
       d.failed                      AS failed,
       d.blockers                    AS blockers,
       d.warns                       AS warns
  FROM daily d
  JOIN run_counts c ON c.day = d.day
 ORDER BY d.day
"#,
        tz = REPORT_TIME_ZONE
    );
    let rows = db().find_with_params(&sql, vec![Value::from(days)]).await?;
    Ok(rows.iter().map(trend_row).collect())
}

/// 把聚合结果行解码成 [`TrendRow`]（缺列按 0 / 空串兜底，不让一行脏数据把整块看板打挂）。
fn trend_row(row: &Row) -> TrendRow {
    TrendRow {
        date: row.get_typed::<String>("date").unwrap_or_default(),
        runs: row.get_typed::<i64>("runs").unwrap_or_default(),
        status: row.get_typed::<String>("status").unwrap_or_default(),
        total: row.get_typed::<i32>("total").unwrap_or_default(),
        passed: row.get_typed::<i32>("passed").unwrap_or_default(),
        failed: row.get_typed::<i32>("failed").unwrap_or_default(),
        blockers: row.get_typed::<i32>("blockers").unwrap_or_default(),
        warns: row.get_typed::<i32>("warns").unwrap_or_default(),
    }
}

/// 连续非 OK 的运行次数（从最新一次往前数，遇到 OK 或 ERROR 之外的边界即停）。
///
/// 语义：**最新一次**若不是 OK，就往前数连续非 OK 的行数，回答「这条红已经红了几天」。
/// 最新一次是 OK 时返回 0。
///
/// # 返回值
///
/// 连续 `FAIL` 的运行次数；最新一次为 OK / 无记录时为 0。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// # 数据权限
///
/// SCOPE-EXEMPT：理由同 [`page_results`]。
pub async fn fail_streak() -> std::result::Result<i64, DbError> {
    let row = db()
        .find_first(
            r#"
WITH ordered AS (
    SELECT status, row_number() OVER (ORDER BY run_at DESC) AS seq
      FROM sys_invariant_result
     WHERE run_at >= now() - interval '90 days'
)
SELECT COUNT(*) AS streak
  FROM ordered
 WHERE seq < COALESCE((SELECT MIN(seq) FROM ordered WHERE status = 'OK'), 1000000000)
"#,
        )
        .await?;
    Ok(row
        .as_ref()
        .and_then(|r| r.get_typed::<i64>("streak").ok())
        .unwrap_or(0))
}

/// 窗口内运行次数（看板副标题「近 30 天跑了 N 次」）。
///
/// # 参数
///
/// - `days`：窗口天数。
///
/// # 返回值
///
/// 窗口内的运行行数。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// # 数据权限
///
/// SCOPE-EXEMPT：理由同 [`page_results`]。
pub async fn runs_in_window(days: i64) -> std::result::Result<i64, DbError> {
    let row = db()
        .find_first_with_params(
            "SELECT COUNT(*) AS n FROM sys_invariant_result \
             WHERE run_at >= now() - make_interval(days => ?::int)",
            vec![Value::from(days)],
        )
        .await?;
    Ok(row
        .as_ref()
        .and_then(|r| r.get_typed::<i64>("n").ok())
        .unwrap_or(0))
}
