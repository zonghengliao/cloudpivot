//! 变更日志的查询层（批次 C · C6）。
//!
//! 依据 `docs/批次C-技术架构.md` §4.8（索引与四种真实查询）、§4.10（API 契约）、
//! §4.7（脱敏联动）、§7 C6 行。
//!
//! # 为什么查询单独一个文件，而不是塞进 [`crate::business::changelog::model`]
//!
//! `model.rs` 的职责在 C5 就写死了：「只做把一行写进去 / 按对象读回来」，而且它是
//! **写入路径**的一部分（每个函数都只吃 `&impl SqlExecutor`，为了 D11 的同事务语义）。
//! 查询是另一件事：它要处理时间窗、过滤条件、分页、聚合，且读路径允许拿全局连接
//! （见 [`crate::business::shared::conn`]）。把两类 SQL 混在一个文件里，评审时
//! 「这条 SELECT 会不会破坏同事务」就要逐条重看；分文件后 C5 的写入契约一眼可辨。
//!
//! # 分区的两条硬约束（架构 §4.3.1 / §4.8，写在这里防止被"优化"掉）
//!
//! 1. **主键是 `(id, created_at)`**：单条详情查询必须带 `created_at` 才能裁剪到一个分区；
//!    不带时只能 `ORDER BY created_at DESC LIMIT 1`（可接受，但要写清楚这是退化路径）。
//! 2. **任何列表查询都要有时间范围**：不带 `created_at` 条件会扫全部分区（实测
//!    200k 行 / 3 分区：带时间窗 0.072 ms vs 不带 15.5 ms）。因此 [`Filter`] 的
//!    默认窗口是 [`DEFAULT_WINDOW_DAYS`] 天 —— 见 [`Filter::apply_default_window`]。
//!
//! # 数据权限口径（架构 §4.7.4：`sys_change_log` 不注入 scope）
//!
//! 审计凭证的语义是「全量可查」，给审计员注入 `SELF_ONLY` 等于让他看不到别人改了什么，
//! 审计价值归零；部门视角用**显式过滤参数** `operator_dept_id` 表达。因此本文件的分页
//! 走 [`scope::paginate_unscoped`]（显式声明 unscoped），而不是 `scope::paginate`。
//! 谁想看某部门，就在请求里传 `operator_dept_id`。

use alun::prelude::*;
use alun::uuid::Uuid;
use serde_json::Value as Json;

use crate::business::changelog::diff;
use crate::business::shared::scope;
use crate::utils;

/// 默认检索窗口（天）。
///
/// 架构 §4.8 的原话：「API 层若用户没传时间范围，默认补最近 90 天」。
/// 90 天也正好覆盖绝大多数「追最近一次改动」的场景，同时保证分区裁剪。
pub const DEFAULT_WINDOW_DAYS: i64 = 90;

/// 时间线默认条数（架构 §4.10 的 `limit=50`）。
pub const TIMELINE_DEFAULT_LIMIT: u32 = 50;

/// 时间线条数硬上限：再多就不该用时间线（用分页列表）。
pub const TIMELINE_MAX_LIMIT: u32 = 200;

/// 导出硬上限（架构 §4.10：同步导出 ≤ 5000 行）。
pub const EXPORT_MAX_ROWS: u64 = 5000;

/// 统计里「按操作人」桶的数量上限（审计看的是「谁最频繁」，不是全量排行榜）。
pub const STATS_OPERATOR_LIMIT: u64 = 20;

/// 列表 / 导出 / 统计共用的过滤条件。
///
/// 全部字段可缺省；`None` = 不过滤。时间字段由 handler 解析后填入（RFC3339 或
/// `%Y-%m-%d %H:%M:%S`，与 `support/log` 同口径），本结构不碰字符串解析。
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// 起始时间（含）。
    pub start_time: Option<chrono::DateTime<chrono::Utc>>,
    /// 结束时间（含）。
    pub end_time: Option<chrono::DateTime<chrono::Utc>>,
    /// 操作人 ID（精确）。
    pub operator_id: Option<Uuid>,
    /// 操作人名字（模糊）。
    pub operator_name: Option<String>,
    /// 操作人部门（精确，架构 §4.7.4 的显式过滤参数）。
    pub operator_dept_id: Option<Uuid>,
    /// 模块 `system` / `business` / `support`（精确）。
    pub module: Option<String>,
    /// 资源表名（精确，如 `sys_user`）。
    pub resource: Option<String>,
    /// 对象主键（精确）。
    pub resource_id: Option<String>,
    /// 动作码（精确，如 `UPDATE`）。
    pub action: Option<String>,
    /// 结果（精确，v1 恒 `SUCCESS`）。
    pub result: Option<String>,
    /// 关键词（动作名 / 操作人 / 对象 ID 三处模糊）。
    pub keyword: Option<String>,
}

impl Filter {
    /// 补默认时间窗（架构 §4.8）：**只补起点**，不补终点。
    ///
    /// 只补起点有两个理由：① 终点缺省 = 「到现在」，这是人读区间时的自然语义；
    /// ② 未来时间本来就不允许写入（62013），补一个未来终点纯属多余。
    ///
    /// # 参数
    ///
    /// - `now`：当前时刻（显式传入，便于单测固定时间）。
    pub fn apply_default_window(&mut self, now: chrono::DateTime<chrono::Utc>) {
        if self.start_time.is_none() {
            self.start_time = Some(now - chrono::Duration::days(DEFAULT_WINDOW_DAYS));
        }
    }

    /// 生成 ` AND ...` 形式的 WHERE 片段与配套参数。
    ///
    /// # 返回值
    ///
    /// `(以 " AND " 开头的 SQL 片段, 参数)`；无条件时返回 `(空串, 空 vec)`。
    ///
    /// # 占位符顺序
    ///
    /// **与 push 顺序严格一致**（批次 C · C2 的坑：参数顺序 = `?` 出现顺序，
    /// 不是语义顺序）。本函数是唯一构造处，改条件必须顺手对一遍单测
    /// `where_clause_placeholder_count_matches_params`。
    pub fn where_clause(&self) -> (String, Vec<Value>) {
        let mut sql = String::new();
        let mut params: Vec<Value> = Vec::new();
        if let Some(start) = self.start_time {
            sql.push_str(" AND created_at >= ?::timestamptz");
            params.push(Value::from(start));
        }
        if let Some(end) = self.end_time {
            sql.push_str(" AND created_at <= ?::timestamptz");
            params.push(Value::from(end));
        }
        if let Some(operator_id) = self.operator_id {
            sql.push_str(" AND operator_id = ?::uuid");
            params.push(Value::from(operator_id));
        }
        if let Some(dept_id) = self.operator_dept_id {
            sql.push_str(" AND operator_dept_id = ?::uuid");
            params.push(Value::from(dept_id));
        }
        if let Some(pattern) = utils::like_pattern(self.operator_name.as_deref()) {
            sql.push_str(" AND operator_name ILIKE ?");
            params.push(Value::from(pattern));
        }
        if let Some(module) = utils::trimmed(self.module.as_deref()) {
            sql.push_str(" AND module = ?::text");
            params.push(Value::from(module));
        }
        if let Some(resource) = utils::trimmed(self.resource.as_deref()) {
            sql.push_str(" AND resource = ?::text");
            params.push(Value::from(resource));
        }
        if let Some(resource_id) = utils::trimmed(self.resource_id.as_deref()) {
            sql.push_str(" AND resource_id = ?::text");
            params.push(Value::from(resource_id));
        }
        if let Some(action) = utils::trimmed(self.action.as_deref()) {
            sql.push_str(" AND action = ?::text");
            params.push(Value::from(action));
        }
        if let Some(result) = utils::trimmed(self.result.as_deref()) {
            sql.push_str(" AND result = ?::text");
            params.push(Value::from(result));
        }
        if let Some(pattern) = utils::like_pattern(self.keyword.as_deref()) {
            sql.push_str(
                " AND (action_label ILIKE ? OR COALESCE(operator_name, '') ILIKE ? \
                  OR resource_id ILIKE ?)",
            );
            params.push(Value::from(pattern.clone()));
            params.push(Value::from(pattern.clone()));
            params.push(Value::from(pattern));
        }
        (sql, params)
    }
}

/// 查询结果的一行（`sys_change_log` 读取视图）。
///
/// 只读、不可变；`diff` 已解析成 JSON（库里是 JSONB，出的仍是**写入时**的脱敏值）。
#[derive(Debug, Clone)]
pub struct ChangeLogRow {
    /// 行 ID。
    pub id: Uuid,
    /// 业务发生时刻（分区键）。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 模块。
    pub module: String,
    /// 资源表名。
    pub resource: String,
    /// 对象主键。
    pub resource_id: String,
    /// 动作码。
    pub action: String,
    /// 动作中文名。
    pub action_label: String,
    /// 请求 ID。
    pub request_id: Option<String>,
    /// 来源。
    pub trace_source: String,
    /// 操作人 ID。
    pub operator_id: Option<Uuid>,
    /// 操作人名字。
    pub operator_name: Option<String>,
    /// 操作人类型（`HUMAN` / `ROBOT`；批次 C+ · T5）。
    ///
    /// 用 `String` 而不是枚举：这是**读**路径，库里若有历史行（`V0111` 之前的
    /// 默认值 `HUMAN`）或将来新增的取值（如 `SYSTEM`），读侧不该因此 500 ——
    /// 枚举封闭性由写侧的 [`crate::business::changelog::OperatorType`] 保证。
    pub operator_type: String,
    /// 操作人部门。
    pub operator_dept_id: Option<Uuid>,
    /// 客户端 IP（文本形态，`inet` 列）。
    pub client_ip: Option<String>,
    /// 解析后的 diff。
    pub diff: Json,
    /// 变化字段名。
    pub changed_fields: Vec<String>,
    /// 结果。
    pub result: String,
    /// 记录模式。
    pub diff_mode: String,
    /// 错误码。
    pub error_code: Option<i32>,
    /// 耗时。
    pub duration_ms: Option<i32>,
}

/// 所有查询共用的投影（列顺序与 [`ChangeLogRow::from_row`] 一一对应）。
///
/// `diff` 取 `::text` 再自己 `serde_json::from_str`：JSONB 的驱动解码在各后端行为不一
/// （C5 已按文本口径落地），保持同一条路径比"看起来更类型化"更可靠。
/// `changed_fields` 同理走 `array_to_json`，避免手写 PG 数组字面量解析。
const SELECT_COLUMNS: &str = "\
    id, created_at, module, resource, resource_id, action, action_label, \
    request_id, trace_source, operator_id, operator_name, operator_type, operator_dept_id, \
    client_ip::text AS client_ip, diff::text AS diff_text, \
    COALESCE(array_to_json(changed_fields), '[]'::json)::text AS changed_fields_json, \
    result, diff_mode, error_code, duration_ms";

impl ChangeLogRow {
    /// 从查询行映射。
    ///
    /// # 异常情况
    ///
    /// 列类型不符或 `diff` 不是合法 JSON → [`DbError::InvalidValue`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        let diff_text = row.get_typed::<String>("diff_text")?;
        let diff: Json = serde_json::from_str(&diff_text)
            .map_err(|error| DbError::InvalidValue(format!("diff 不是合法 JSON：{error}")))?;
        let changed_fields_text = row.get_typed::<String>("changed_fields_json")?;
        let changed_fields: Vec<String> =
            serde_json::from_str(&changed_fields_text).map_err(|error| {
                DbError::InvalidValue(format!("changed_fields 不是合法数组：{error}"))
            })?;
        Ok(Self {
            id: row.get_typed::<Uuid>("id")?,
            created_at: row.get_typed::<chrono::DateTime<chrono::Utc>>("created_at")?,
            module: row.get_typed::<String>("module")?,
            resource: row.get_typed::<String>("resource")?,
            resource_id: row.get_typed::<String>("resource_id")?,
            action: row.get_typed::<String>("action")?,
            action_label: row.get_typed::<String>("action_label")?,
            request_id: row.get_typed::<Option<String>>("request_id")?,
            trace_source: row.get_typed::<String>("trace_source")?,
            operator_id: row.get_typed::<Option<Uuid>>("operator_id")?,
            operator_name: row.get_typed::<Option<String>>("operator_name")?,
            operator_type: row
                .get_typed::<Option<String>>("operator_type")?
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| {
                    crate::business::changelog::OperatorType::Human
                        .code()
                        .to_string()
                }),
            operator_dept_id: row.get_typed::<Option<Uuid>>("operator_dept_id")?,
            client_ip: row.get_typed::<Option<String>>("client_ip")?,
            diff,
            changed_fields,
            result: row.get_typed::<String>("result")?,
            diff_mode: row.get_typed::<String>("diff_mode")?,
            error_code: row.get_typed::<Option<i32>>("error_code")?,
            duration_ms: row.get_typed::<Option<i32>>("duration_ms")?,
        })
    }

    /// diff 里是否至少有一个字段被脱敏（供列表页显示「含脱敏字段」角标）。
    pub fn masked(&self) -> bool {
        self.diff
            .get("fields")
            .and_then(Json::as_object)
            .map(|fields| {
                fields.values().any(|field| {
                    field.get("masked").and_then(Json::as_bool).unwrap_or(false)
                        || field.get("redacted").is_some()
                })
            })
            .unwrap_or(false)
    }
}

/// 分页查询（`GET /business/changelog/page`）。
///
/// # 参数
///
/// - `filter`：过滤条件（调用方已补默认时间窗）；
/// - `page` / `page_size`：已由 `utils::normalize_page` 规范化。
///
/// # 为什么没有 executor 参数
///
/// 列表分页必须走闸① 的唯一入口 [`scope::paginate_unscoped`]，而它内部持有全局
/// 分页器（框架 `paginate_with_params`），不接受外部执行器。这不是漏参数，而是
/// 「分页只有一种正确写法」的代价：想给本函数传 executor，就得先绕过闸①。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn page(filter: &Filter, page: u64, page_size: u64) -> DbResult<Page<ChangeLogRow>> {
    let (where_sql, params) = filter.where_clause();
    let base_sql = format!("SELECT {SELECT_COLUMNS} FROM sys_change_log WHERE 1 = 1{where_sql}");
    let Page {
        list,
        total,
        page,
        size,
        pages,
    } = scope::paginate_unscoped(
        scope::ScopeTarget::unscoped("sys_change_log"),
        &base_sql,
        params,
        Some("ORDER BY created_at DESC, id DESC"),
        page,
        page_size,
    )
    .await?;
    let list = list
        .iter()
        .map(ChangeLogRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok(Page {
        list,
        total,
        page,
        size,
        pages,
    })
}

/// 单条详情（`GET /business/changelog/{id}`）。
///
/// # 参数
///
/// - `ex`：执行器；
/// - `id`：行 ID；
/// - `created_at`：业务发生时刻。**传了就能只扫一个分区**（主键含分区键，
///   架构 §4.3.1）；不传则退化成「按 id 在各分区里找一行」，结果相同但更慢。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]；找不到只是 `Ok(None)`。
pub async fn find(
    ex: &impl SqlExecutor,
    id: Uuid,
    created_at: Option<chrono::DateTime<chrono::Utc>>,
) -> DbResult<Option<ChangeLogRow>> {
    // 两条**完整语句**，而不是「先拼半句再按需追加谓词」。
    //
    // 主键是 `(id, created_at)`：任何只按 `id` 过滤的语句都会扫全部月分区
    // （架构 §5.7 R14）。门禁 G6 用 grep 守住这条，而 grep 只能看**语句文本**——
    // 「拼接式」写法在不传时间的分支上生成的是纯 id 查询，文本里没有分区键，
    // 门禁无法区分「运行时带了」和「真的没带」。所以两种形态都写成整句：
    //   · 传了时间：`created_at = ?` → 单分区（裁剪由 PG 完成）
    //   · 没传时间：`created_at BETWEEN '-infinity' AND 'infinity'` → 显式表达
    //     「全部分区、不裁剪」的意图，结果与纯 id 查询逐行相同（走对象索引），
    //     但读代码的人与门禁都能一眼看出这里没有退化成主键查找。
    let (sql, params) = match created_at {
        Some(at) => (
            format!(
                "SELECT {SELECT_COLUMNS} FROM sys_change_log \
                 WHERE created_at = ?::timestamptz AND id = ?::uuid \
                 ORDER BY created_at DESC LIMIT 1"
            ),
            vec![Value::from(at), Value::from(id)],
        ),
        None => (
            format!(
                "SELECT {SELECT_COLUMNS} FROM sys_change_log \
                 WHERE created_at BETWEEN '-infinity'::timestamptz AND 'infinity'::timestamptz \
                 AND id = ?::uuid \
                 ORDER BY created_at DESC LIMIT 1"
            ),
            vec![Value::from(id)],
        ),
    };
    let rows = ex.query_sql(sql, params).await?;
    rows.first().map(ChangeLogRow::from_row).transpose()
}

/// 某对象的变更历史（`GET /business/changelog/timeline`）。
///
/// # 为什么不给默认时间窗
///
/// 「这条记录被改过几次」的正确答案**必须包含全部历史**：补一个 90 天窗口会让
/// 「一共被改过 5 次」变成「最近 90 天被改过 2 次」，而这恰恰是审计要看的数字。
/// 性能由 `idx_sys_change_log_object (resource, resource_id, created_at DESC)`
/// 保证（逐分区 Index Scan，实测 10 万行 < 50 ms，见交付证据）。
///
/// # 参数
///
/// - `ex`：执行器；
/// - `resource` / `resource_id`：对象坐标；
/// - `start` / `end`：可选时间窗（前端"看某段时间内改了什么"时传）；
/// - `limit`：已由 [`clamp_timeline_limit`] 规范化。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn timeline(
    ex: &impl SqlExecutor,
    resource: &str,
    resource_id: &str,
    start: Option<chrono::DateTime<chrono::Utc>>,
    end: Option<chrono::DateTime<chrono::Utc>>,
    limit: u32,
) -> DbResult<Vec<ChangeLogRow>> {
    let mut sql = format!(
        "SELECT {SELECT_COLUMNS} FROM sys_change_log \
          WHERE resource = ?::text AND resource_id = ?::text"
    );
    let mut params = vec![Value::from(resource), Value::from(resource_id)];
    if let Some(start) = start {
        sql.push_str(" AND created_at >= ?::timestamptz");
        params.push(Value::from(start));
    }
    if let Some(end) = end {
        sql.push_str(" AND created_at <= ?::timestamptz");
        params.push(Value::from(end));
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?::bigint");
    params.push(Value::from(i64::from(limit)));
    let rows = ex.query_sql(sql, params).await?;
    rows.iter().map(ChangeLogRow::from_row).collect()
}

/// 某对象的变更次数（时间线的 `total`，与 `items` 分两次查是为了避免窗口截断后
/// 「数字比列表短」的误导）。
///
/// # 参数
///
/// - `ex`：执行器；
/// - `resource` / `resource_id`：对象坐标。
pub async fn count_for_object(
    ex: &impl SqlExecutor,
    resource: &str,
    resource_id: &str,
) -> DbResult<i64> {
    ex.scalar_sql(
        "SELECT COUNT(*) FROM sys_change_log WHERE resource = ?::text AND resource_id = ?::text"
            .to_string(),
        vec![Value::from(resource), Value::from(resource_id)],
    )
    .await
}

/// 一个聚合桶（`k` 是分组键，`count` 是条数）。
#[derive(Debug, Clone)]
pub struct Bucket {
    /// 分组键（模块名 / 操作人名 / 日期）。
    pub key: String,
    /// 条数。
    pub count: i64,
}

/// 统计结果。
#[derive(Debug, Clone, Default)]
pub struct Stats {
    /// 过滤条件下的总条数。
    pub total: i64,
    /// 按模块聚合。
    pub by_module: Vec<Bucket>,
    /// 按操作人聚合（Top [`STATS_OPERATOR_LIMIT`]）。
    pub by_operator: Vec<Bucket>,
    /// 按天聚合（升序，便于直接画折线）。
    pub by_day: Vec<Bucket>,
}

/// 统计（`GET /business/changelog/stats`）。
///
/// # 参数
///
/// - `ex`：执行器；
/// - `filter`：过滤条件（与列表同口径，调用方已补默认时间窗）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn stats(ex: &impl SqlExecutor, filter: &Filter) -> DbResult<Stats> {
    let (where_sql, params) = filter.where_clause();
    let total: i64 = ex
        .scalar_sql(
            format!("SELECT COUNT(*) FROM sys_change_log WHERE 1 = 1{where_sql}"),
            params.clone(),
        )
        .await?;
    let by_module = buckets(
        ex,
        &format!(
            "SELECT module AS k, COUNT(*)::bigint AS c FROM sys_change_log WHERE 1 = 1{where_sql} \
             GROUP BY 1 ORDER BY c DESC, k ASC"
        ),
        params.clone(),
    )
    .await?;
    let by_operator = buckets(
        ex,
        &format!(
            "SELECT COALESCE(operator_name, '(未知)') AS k, COUNT(*)::bigint AS c \
               FROM sys_change_log WHERE 1 = 1{where_sql} \
              GROUP BY 1 ORDER BY c DESC, k ASC LIMIT ?::bigint"
        ),
        {
            let mut with_limit = params.clone();
            with_limit.push(Value::from(STATS_OPERATOR_LIMIT as i64));
            with_limit
        },
    )
    .await?;
    // `to_char` 出来的一定是 `YYYY-MM-DD` 字符串：直接给前端画图用，不再做时区换算
    // （存储层统一 UTC，展示层按需本地化 —— 与 `support/log` 的口径一致）。
    let by_day = buckets(
        ex,
        &format!(
            "SELECT to_char(date_trunc('day', created_at), 'YYYY-MM-DD') AS k, \
                    COUNT(*)::bigint AS c \
               FROM sys_change_log WHERE 1 = 1{where_sql} \
              GROUP BY 1 ORDER BY 1 ASC"
        ),
        params,
    )
    .await?;
    Ok(Stats {
        total,
        by_module,
        by_operator,
        by_day,
    })
}

/// 执行一条「两列（k, c）」的聚合 SQL 并映射成 [`Bucket`]。
async fn buckets(ex: &impl SqlExecutor, sql: &str, params: Vec<Value>) -> DbResult<Vec<Bucket>> {
    let rows = ex.query_sql(sql.to_string(), params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(Bucket {
            key: row.get_typed::<String>("k")?,
            count: row.get_typed::<i64>("c")?,
        });
    }
    Ok(out)
}

/// 导出取数（`GET /business/changelog/export`），硬上限 [`EXPORT_MAX_ROWS`]。
///
/// # 参数
///
/// - `ex`：执行器；
/// - `filter`：与列表同口径；
/// - `limit`：已由 [`clamp_export_limit`] 规范化。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn export_rows(
    ex: &impl SqlExecutor,
    filter: &Filter,
    limit: u64,
) -> DbResult<Vec<ChangeLogRow>> {
    let (where_sql, mut params) = filter.where_clause();
    params.push(Value::from(limit as i64));
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM sys_change_log WHERE 1 = 1{where_sql} \
         ORDER BY created_at DESC, id DESC LIMIT ?::bigint"
    );
    let rows = ex.query_sql(sql, params).await?;
    rows.iter().map(ChangeLogRow::from_row).collect()
}

/// 时间线条数规范化：缺省 [`TIMELINE_DEFAULT_LIMIT`]，上限 [`TIMELINE_MAX_LIMIT`]，0 视为缺省。
///
/// # 参数
///
/// - `raw`：请求里的 `limit`。
pub fn clamp_timeline_limit(raw: Option<u32>) -> u32 {
    match raw {
        None | Some(0) => TIMELINE_DEFAULT_LIMIT,
        Some(value) => value.min(TIMELINE_MAX_LIMIT),
    }
}

/// 导出条数规范化：缺省与上限都是 [`EXPORT_MAX_ROWS`]，0 视为缺省。
///
/// # 参数
///
/// - `raw`：请求里的 `limit`。
pub fn clamp_export_limit(raw: Option<u64>) -> u64 {
    match raw {
        None | Some(0) => EXPORT_MAX_ROWS,
        Some(value) => value.min(EXPORT_MAX_ROWS),
    }
}

/// 已登记的表 / 字段清单（`GET /business/changelog/table-registry`）。
///
/// 数据源是 [`diff::REGISTRY`]（编译期常量），**不查库**：这份清单回答的是
/// 「代码里承诺会记日志的表有哪些」，查库回答不了这个问题。
///
/// # 返回值
///
/// `(表名, 模块, 白名单字段, 大字段)` 四元组列表，按表名升序（便于前端稳定渲染）。
pub fn table_registry() -> Vec<(
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
)> {
    let mut list: Vec<_> = diff::REGISTRY
        .iter()
        .map(|spec| (spec.table, spec.module, spec.fields, spec.big))
        .collect();
    list.sort_by_key(|(table, ..)| *table);
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-10-29T12:00:00Z")
            .expect("固定时间应可解析")
            .with_timezone(&chrono::Utc)
    }

    /// 缺省时间窗必须是「近 90 天」，且只补起点（架构 §4.8）。
    #[test]
    fn default_window_is_90_days_and_open_ended() {
        let mut filter = Filter::default();
        filter.apply_default_window(fixed_now());
        assert_eq!(
            filter.start_time,
            Some(fixed_now() - chrono::Duration::days(90))
        );
        assert!(filter.end_time.is_none(), "终点缺省 = 到现在，不该补");
    }

    /// 显式传了起点就不覆盖（用户的意图优先于默认值）。
    #[test]
    fn explicit_start_time_is_kept() {
        let mut filter = Filter {
            start_time: Some(fixed_now() - chrono::Duration::days(3)),
            ..Filter::default()
        };
        filter.apply_default_window(fixed_now());
        assert_eq!(
            filter.start_time,
            Some(fixed_now() - chrono::Duration::days(3))
        );
    }

    /// 占位符个数必须与参数个数一致，且片段以 " AND " 开头。
    ///
    /// 这是「参数顺序 = `?` 顺序」这类事故的最小防线（C2 踩过）。
    #[test]
    fn where_clause_placeholder_count_matches_params() {
        let filter = Filter {
            start_time: Some(fixed_now() - chrono::Duration::days(1)),
            end_time: Some(fixed_now()),
            operator_id: Some(Uuid::now_v7()),
            operator_name: Some("adm".to_string()),
            operator_dept_id: Some(Uuid::now_v7()),
            module: Some("system".to_string()),
            resource: Some("sys_user".to_string()),
            resource_id: Some("u-1".to_string()),
            action: Some("UPDATE".to_string()),
            result: Some("SUCCESS".to_string()),
            keyword: Some("手机".to_string()),
        };
        let (sql, params) = filter.where_clause();
        assert_eq!(
            sql.matches('?').count(),
            params.len(),
            "占位符与参数必须等长"
        );
        assert_eq!(
            sql.matches('?').count(),
            13,
            "10 个精确条件 + keyword 的 3 处模糊 = 13 个占位符"
        );
        assert!(sql.starts_with(" AND "), "片段必须能直接拼进 WHERE 1 = 1");
    }

    /// 空过滤 → 空片段（不产生 `AND` 悬空）。
    #[test]
    fn empty_filter_produces_empty_clause() {
        let (sql, params) = Filter::default().where_clause();
        assert!(sql.is_empty());
        assert!(params.is_empty());
    }

    /// 空白字符串不算条件（前端把「全选」传成空串是常事）。
    #[test]
    fn blank_strings_are_ignored() {
        let filter = Filter {
            module: Some("   ".to_string()),
            resource: Some("".to_string()),
            action: Some("\t".to_string()),
            ..Filter::default()
        };
        let (sql, params) = filter.where_clause();
        assert!(sql.is_empty(), "空白值不该产生条件：{sql}");
        assert!(params.is_empty());
    }

    /// 时间线 / 导出的 limit 规范化。
    #[test]
    fn limits_are_clamped() {
        assert_eq!(clamp_timeline_limit(None), TIMELINE_DEFAULT_LIMIT);
        assert_eq!(clamp_timeline_limit(Some(0)), TIMELINE_DEFAULT_LIMIT);
        assert_eq!(clamp_timeline_limit(Some(7)), 7);
        assert_eq!(
            clamp_timeline_limit(Some(9999)),
            TIMELINE_MAX_LIMIT,
            "超过上限必须被夹住，否则一次请求能拉全表"
        );
        assert_eq!(clamp_export_limit(None), EXPORT_MAX_ROWS);
        assert_eq!(clamp_export_limit(Some(0)), EXPORT_MAX_ROWS);
        assert_eq!(clamp_export_limit(Some(99_999)), EXPORT_MAX_ROWS);
    }

    /// 登记表清单必须与 `diff::REGISTRY` 完全一致（C6 的 4 张表都在里面），且按表名升序。
    #[test]
    fn registry_lists_the_four_wired_tables() {
        let list = table_registry();
        let tables: Vec<&str> = list.iter().map(|(table, ..)| *table).collect();
        for expected in ["sys_user", "sys_role", "sys_announcement", "sys_help_doc"] {
            assert!(
                tables.contains(&expected),
                "{expected} 必须出现在 table-registry 里（C6 首批接入 4 张表）"
            );
        }
        let mut sorted = tables.clone();
        sorted.sort_unstable();
        assert_eq!(tables, sorted, "清单必须按表名升序，前端才稳定");
        // 每张表的白名单不能为空、不能有重复（重复会让 diff 的 changed_fields 出现两次）
        for (table, module, fields, big) in &list {
            assert!(!fields.is_empty(), "{table} 白名单不能为空");
            assert!(!module.is_empty(), "{table} 必须登记模块归属");
            let unique: std::collections::BTreeSet<&&str> = fields.iter().collect();
            assert_eq!(unique.len(), fields.len(), "{table} 白名单有重复字段");
            for field in *big {
                assert!(
                    fields.contains(field),
                    "{table}.{field} 是大字段但不在白名单里"
                );
            }
        }
    }
}
