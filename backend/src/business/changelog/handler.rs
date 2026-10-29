//! 变更日志查询接口（批次 C · C6）。
//!
//! 依据 `docs/批次C-技术架构.md` §4.10（API 契约）、§4.7.2/L3（出参前再脱敏）、
//! §4.8（索引与默认时间窗）、§7 C6 行。路由前缀是 `/business/changelog`（C6 任务书定的，
//! 与架构 §4.10 的 `/system/change-log` 不同名但同语义 —— 模块名 `changelog` 由 C5 定，
//! 见 [`crate::business::changelog`] 的头注释）。
//!
//! # 六个接口
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|
//! | GET | `/business/changelog/page` | `system:change-log:list` | 列表（时间/操作人/模块/动作/对象/结果 筛选） |
//! | GET | `/business/changelog/{id}` | `system:change-log:list` | 详情（diff 前后值对比） |
//! | GET | `/business/changelog/timeline` | `system:change-log:list` | 按对象查历史（被改过几次） |
//! | GET | `/business/changelog/export` | `system:change-log:export` | 导出 Excel（**必须脱敏**） |
//! | GET | `/business/changelog/stats` | `system:change-log:list` | 统计（模块/操作人/时间段聚合） |
//! | GET | `/business/changelog/table-registry` | `system:change-log:list` | 已登记表/字段清单 |
//!
//! # 为什么写接口在别的模块、查询接口在这里（职责划分）
//!
//! `sys_change_log` 的**写入**只能由业务事务内的 [`crate::business::changelog::record`]
//! 完成（D11）；本文件只读。读路径允许使用全局连接，执行器由
//! [`crate::business::shared::conn`] 提供 —— 门禁 G2（`business/changelog/` 内不得出现
//! 自开事务的 `db()`）因此仍然成立：本文件的每一个 SQL 都只吃 `&impl SqlExecutor`，
//! 连接从「外部」传进来，写路径的签名一个字都没变。
//!
//! # 脱敏的三层在本文件里的落点
//!
//! - L1 写入侧：`record()` 已经做了（库里的 `diff` 永远是脱敏值）；
//! - L2/L3 读取侧：**导出**是本文件唯一会「把值搬出系统」的出口，所以导出前对每个字段
//!   再跑一次 [`mask::apply`]（幂等：已脱敏的值不会变形），并把
//!   `X-Mask-Applied: true` 写进响应头。列表 / 详情 / 时间线只回吐库里已有的脱敏值，
//!   不额外做 L3（它们不出系统边界，而 L2 的「按读取者 MaskSet 再打一遍」需要
//!   读取者本人的字段豁免规则，属于 B6 的 `MaskSet` 语义，C6 不越界实现）。

use std::collections::HashMap;

use alun::prelude::*;
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value as Json;

use crate::business::changelog::dto::{
    ChangeLogDetailQuery, ChangeLogDetailRes, ChangeLogListItem, ChangeLogPageQuery,
    ChangeLogStatsQuery, ChangeLogStatsRes, ChangeLogTimelineItem, ChangeLogTimelineQuery,
    ChangeLogTimelineRes, CountBucketRes, TableRegistryEntryRes, TableRegistryRes,
};
use crate::business::changelog::mask;
use crate::business::changelog::query::{self, ChangeLogRow, Filter};
use crate::business::shared::conn;
use crate::shared::error_code;
use crate::utils;

/// xlsx 的 MIME（与 `business/impexp` 同口径）。
const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

/// 导出表头（顺序即列顺序；脱敏证据按这份表头逐列核对）。
const EXPORT_HEADERS: [&str; 15] = [
    "序号",
    "变更时间",
    "模块",
    "资源表",
    "对象ID",
    "动作",
    "动作名称",
    "操作人",
    "来源",
    "变化字段",
    "变更内容（前后值）",
    "请求ID",
    "客户端IP",
    "结果",
    "含脱敏字段",
];

// ==================== 接口 ====================

/// 分页查询变更日志（全局检索）。
///
/// # 参数
///
/// - `query`：筛选条件。**没传时间范围时默认补最近 90 天**（架构 §4.8：
///   不带 `created_at` 条件会扫全部分区，实测 15.5 ms vs 带时间窗 0.072 ms）。
///
/// # 返回值
///
/// 分页列表（按 `created_at DESC, id DESC`）。
///
/// # 异常情况
///
/// - 400：时间 / UUID 参数格式非法；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/changelog/page",
    method = "GET",
    permission = "system:change-log:list"
)]
#[alun::get("/business/changelog/page")]
pub async fn page(
    Query(query): Query<ChangeLogPageQuery>,
) -> ResResult<PageData<Vec<ChangeLogListItem>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let filter = build_filter(&query)?;
    let result = query::page(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询变更日志"))?;
    let list = result
        .list
        .iter()
        .map(ChangeLogListItem::from_row)
        .collect();
    Ok(Res::page(list, result.total, result.page, result.size))
}

/// 单条详情（diff 前后值对比）。
///
/// # 参数
///
/// - `id`：行 ID；
/// - `query.created_at`：业务发生时刻。**传了就能只扫一个分区**（主键含分区键）；
///   不传则按 id 在各分区里找一行（结果相同、代价更高）。
///
/// # 返回值
///
/// 详情：`fields`（逐字段前/后值）、`big`（大字段摘要）、`meta`（表/主键/动作）、
/// `summary`（一行可读的「A → B」摘要，前端直接显示）。
///
/// # 异常情况
///
/// - 400：ID / 时间格式非法；
/// - 404 + 10002：日志不存在；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/changelog/{id}",
    method = "GET",
    permission = "system:change-log:list"
)]
#[alun::get("/business/changelog/{id}")]
pub async fn detail(
    Path(id): Path<String>,
    Query(query): Query<ChangeLogDetailQuery>,
) -> ResResult<ChangeLogDetailRes> {
    let id = utils::require_uuid(&id, "id")?;
    let created_at = parse_optional_time(query.created_at.as_deref(), "created_at")?;
    let row = query::find(conn::global(), id, created_at)
        .await
        .map_err(|error| utils::db_error(error, "查询变更日志详情"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                "变更日志不存在（若已知发生时间，请带上 created_at 以便分区裁剪）",
            )
        })?;
    Ok(Res::ok(ChangeLogDetailRes::from_row(&row)))
}

/// 按对象查变更历史（「这条记录被改过几次」）。
///
/// # 参数
///
/// - `query.table`：资源表名（如 `sys_user`）；
/// - `query.target_id`：对象主键；
/// - `query.start_time` / `query.end_time`：可选时间窗（**不补默认 90 天**：
///   「一共被改过几次」必须覆盖全部历史，性能由对象索引保证，见交付证据）；
/// - `query.limit`：条数（缺省 50，上限 200）。
///
/// # 返回值
///
/// `{ total, items }`：`total` 是该对象的历史变更**总次数**（不受 limit 影响，
/// 也不受时间窗影响之外的过滤），`items` 是按时间倒序的变更列表。
///
/// # 异常情况
///
/// - 400：`table` / `target_id` 缺失或时间格式非法；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/changelog/timeline",
    method = "GET",
    permission = "system:change-log:list"
)]
#[alun::get("/business/changelog/timeline")]
pub async fn timeline(
    Query(query): Query<ChangeLogTimelineQuery>,
) -> ResResult<ChangeLogTimelineRes> {
    let table = utils::trimmed(query.table.as_deref()).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "table 不能为空（如 sys_user）",
        )
    })?;
    let target_id = utils::trimmed(query.target_id.as_deref())
        .ok_or_else(|| ApiError::new(400, error_code::PARAM_INVALID, "target_id 不能为空"))?;
    let start = parse_optional_time(query.start_time.as_deref(), "start_time")?;
    let end = parse_optional_time(query.end_time.as_deref(), "end_time")?;
    let limit = query::clamp_timeline_limit(query.limit);
    let total = query::count_for_object(conn::global(), table, target_id)
        .await
        .map_err(|error| utils::db_error(error, "统计对象变更次数"))?;
    let rows = query::timeline(conn::global(), table, target_id, start, end, limit)
        .await
        .map_err(|error| utils::db_error(error, "查询对象变更历史"))?;
    Ok(Res::ok(ChangeLogTimelineRes {
        total,
        limit,
        items: rows.iter().map(ChangeLogTimelineItem::from_row).collect(),
    }))
}

/// 导出 Excel（**必须脱敏**）。
///
/// # 参数
///
/// - `query`：与 [`page`] 完全同口径的筛选条件（另加 `limit`，缺省与上限都是 5000）。
///
/// # 返回值
///
/// xlsx 字节流。响应头带 `X-Mask-Applied: true`（导出前跑过 L3 再脱敏）与
/// `X-Mask-Policy: L1+L3`。
///
/// # 异常情况
///
/// - 400：时间 / UUID 参数格式非法；
/// - 500：数据库异常或 xlsx 生成失败。
#[alun::permission(
    path = "/business/changelog/export",
    method = "GET",
    permission = "system:change-log:export"
)]
#[alun::get("/business/changelog/export")]
pub async fn export(Query(query): Query<ChangeLogPageQuery>) -> Response {
    match export_response(query).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 统计（按模块 / 操作人 / 时间段聚合）。
///
/// # 参数
///
/// - `query`：与 [`page`] 同口径的筛选条件（无分页字段）。
///
/// # 返回值
///
/// `{ total, by_module, by_operator, by_day }`。
///
/// # 异常情况
///
/// - 400：时间 / UUID 参数格式非法；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/changelog/stats",
    method = "GET",
    permission = "system:change-log:list"
)]
#[alun::get("/business/changelog/stats")]
pub async fn stats(Query(query): Query<ChangeLogStatsQuery>) -> ResResult<ChangeLogStatsRes> {
    let filter = build_stats_filter(&query)?;
    let stats = query::stats(conn::global(), &filter)
        .await
        .map_err(|error| utils::db_error(error, "统计变更日志"))?;
    Ok(Res::ok(ChangeLogStatsRes {
        total: stats.total,
        by_module: stats
            .by_module
            .iter()
            .map(|bucket| CountBucketRes {
                key: bucket.key.clone(),
                count: bucket.count,
            })
            .collect(),
        by_operator: stats
            .by_operator
            .iter()
            .map(|bucket| CountBucketRes {
                key: bucket.key.clone(),
                count: bucket.count,
            })
            .collect(),
        by_day: stats
            .by_day
            .iter()
            .map(|bucket| CountBucketRes {
                key: bucket.key.clone(),
                count: bucket.count,
            })
            .collect(),
    }))
}

/// 已登记的表 / 字段清单（回答 UI 的「哪些表会记日志」）。
///
/// # 返回值
///
/// 每张登记表的模块归属、白名单字段、大字段列。**不查库**：数据源是编译期常量
/// [`crate::business::changelog::diff::REGISTRY`]。
#[alun::permission(
    path = "/business/changelog/table-registry",
    method = "GET",
    permission = "system:change-log:list"
)]
#[alun::get("/business/changelog/table-registry")]
pub async fn table_registry() -> ResResult<TableRegistryRes> {
    let tables = query::table_registry()
        .into_iter()
        .map(|(table, module, fields, big)| TableRegistryEntryRes {
            table: table.to_string(),
            module: module.to_string(),
            fields: fields.iter().map(|field| (*field).to_string()).collect(),
            big_fields: big.iter().map(|field| (*field).to_string()).collect(),
        })
        .collect();
    Ok(Res::ok(TableRegistryRes { tables }))
}

// ==================== 内部实现 ====================

/// 导出响应构造（把 IO 与错误转换从 handler 里拆出来，便于单测覆盖纯逻辑部分）。
async fn export_response(query: ChangeLogPageQuery) -> std::result::Result<Response, ApiError> {
    let limit = query::clamp_export_limit(query.limit);
    let filter = build_filter(&query)?;
    let rows = query::export_rows(conn::global(), &filter, limit)
        .await
        .map_err(|error| utils::db_error(error, "导出变更日志"))?;
    // L3：出系统边界前，按每张表自己的脱敏策略再跑一遍（幂等，见模块头注释）
    let policies = load_policies().await?;
    let mut sheet_rows = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let (rendered, masked_any) = render_diff(&policies, row);
        let mut line: HashMap<String, String> = HashMap::with_capacity(EXPORT_HEADERS.len());
        line.insert("序号".to_string(), (index + 1).to_string());
        line.insert(
            "变更时间".to_string(),
            row.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
        );
        line.insert("模块".to_string(), row.module.clone());
        line.insert("资源表".to_string(), row.resource.clone());
        line.insert("对象ID".to_string(), row.resource_id.clone());
        line.insert("动作".to_string(), row.action.clone());
        line.insert("动作名称".to_string(), row.action_label.clone());
        line.insert(
            "操作人".to_string(),
            row.operator_name.clone().unwrap_or_default(),
        );
        line.insert("来源".to_string(), row.trace_source.clone());
        line.insert("变化字段".to_string(), row.changed_fields.join(","));
        line.insert("变更内容（前后值）".to_string(), rendered);
        line.insert(
            "请求ID".to_string(),
            row.request_id.clone().unwrap_or_default(),
        );
        line.insert(
            "客户端IP".to_string(),
            row.client_ip.clone().unwrap_or_default(),
        );
        line.insert("结果".to_string(), row.result.clone());
        line.insert(
            "含脱敏字段".to_string(),
            if masked_any { "是" } else { "否" }.to_string(),
        );
        sheet_rows.push(line);
    }
    let bytes = crate::business::impexp::engine::write_sheet(&EXPORT_HEADERS, &sheet_rows)?;
    let file_name = format!(
        "变更日志_{}.xlsx",
        chrono::Utc::now().format("%Y%m%d_%H%M%S")
    );
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(XLSX_MIME));
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&content_disposition(&file_name))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    // 脱敏证据直接写进响应头：运维/审计用 `curl -I` 就能看见，不必解 xlsx
    headers.insert("x-mask-applied", HeaderValue::from_static("true"));
    headers.insert("x-mask-policy", HeaderValue::from_static("L1+L3"));
    Ok((headers, bytes).into_response())
}

/// 载入所有登记表的写入侧脱敏策略（一次导出只查一遍，避免逐行 IO）。
///
/// # 异常情况
///
/// 数据库异常返回 500。
async fn load_policies() -> std::result::Result<HashMap<&'static str, mask::Policy>, ApiError> {
    let mut map: HashMap<&'static str, mask::Policy> = HashMap::new();
    for spec in crate::business::changelog::diff::REGISTRY {
        let policy = mask::Policy::load(conn::global(), spec.table)
            .await
            .map_err(|error| utils::db_error(error, "载入脱敏规则"))?;
        map.insert(spec.table, policy);
    }
    Ok(map)
}

/// 把 diff 渲染成「字段: 旧 → 新；…」的文本，**并做 L3 再脱敏**。
///
/// # 参数
///
/// - `policies`：表名 → 脱敏策略；
/// - `row`：一行日志。
///
/// # 返回值
///
/// `(文本, 是否有字段被脱敏)`。第二个值进导出的「含脱敏字段」列。
///
/// # 为什么对「库里已经脱敏的值」再跑一遍
///
/// ① 幂等（`masker` 对已脱敏值不变形），代价可忽略；② 防的是「L1 之后有人改了
/// `sys_field_mask`，旧行里的值现在算明文」这类跨时间漂移 —— 出系统边界时按**当前**
/// 规则再判一次，是唯一能做的地方。
fn render_diff(
    policies: &HashMap<&'static str, mask::Policy>,
    row: &ChangeLogRow,
) -> (String, bool) {
    let policy = policies.get(row.resource.as_str());
    let mut parts: Vec<String> = Vec::new();
    let mut masked_any = row.masked();
    if let Some(fields) = row.diff.get("fields").and_then(Json::as_object) {
        let mut names: Vec<&String> = fields.keys().collect();
        names.sort();
        for name in names {
            let pair = &fields[name];
            let decisions = policy.map(|item| item.decisions(name)).unwrap_or_default();
            let (before, masked_before) =
                mask::apply(&decisions, pair.get("b").unwrap_or(&Json::Null));
            let (after, masked_after) =
                mask::apply(&decisions, pair.get("a").unwrap_or(&Json::Null));
            masked_any = masked_any || masked_before || masked_after;
            parts.push(format!("{name}: {} → {}", plain(&before), plain(&after)));
        }
    }
    if let Some(big) = row.diff.get("big").and_then(Json::as_object) {
        let mut names: Vec<&String> = big.keys().collect();
        names.sort();
        for name in names {
            let pair = &big[name];
            parts.push(format!(
                "{name}: {} → {}（摘要）",
                big_plain(pair.get("b")),
                big_plain(pair.get("a"))
            ));
        }
    }
    (parts.join("；"), masked_any)
}

/// 标量值转文本（`null` → 空，字符串不带引号，对象/数组走紧凑 JSON）。
fn plain(value: &Json) -> String {
    match value {
        Json::Null => String::new(),
        Json::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// 大字段摘要转文本（`{len,sha256,preview}` → `[len=1234 sha=abcd1234 …]`）。
fn big_plain(value: Option<&Json>) -> String {
    let Some(Json::Object(map)) = value else {
        return String::new();
    };
    let len = map.get("len").and_then(Json::as_u64).unwrap_or(0);
    let sha = map
        .get("sha256")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .chars()
        .take(8)
        .collect::<String>();
    if map.get("redacted").and_then(Json::as_bool).unwrap_or(false) {
        return format!("[redacted len={len}]");
    }
    format!("[len={len} sha={sha}]")
}

/// 列表 / 导出的筛选条件构造（含默认 90 天窗口）。
fn build_filter(query: &ChangeLogPageQuery) -> std::result::Result<Filter, ApiError> {
    let mut filter = shallow_filter(
        query.start_time.as_deref(),
        query.end_time.as_deref(),
        query.operator_id.as_deref(),
        query.operator_dept_id.as_deref(),
        query.operator_name.as_deref(),
        query.module.as_deref(),
        query.resource.as_deref(),
        query.resource_id.as_deref(),
        query.action.as_deref(),
        query.result.as_deref(),
        query.keyword.as_deref(),
    )?;
    filter.apply_default_window(chrono::Utc::now());
    Ok(filter)
}

/// 统计的筛选条件构造（同口径，同样补默认窗口）。
fn build_stats_filter(query: &ChangeLogStatsQuery) -> std::result::Result<Filter, ApiError> {
    let mut filter = shallow_filter(
        query.start_time.as_deref(),
        query.end_time.as_deref(),
        query.operator_id.as_deref(),
        query.operator_dept_id.as_deref(),
        query.operator_name.as_deref(),
        query.module.as_deref(),
        query.resource.as_deref(),
        query.resource_id.as_deref(),
        query.action.as_deref(),
        query.result.as_deref(),
        query.keyword.as_deref(),
    )?;
    filter.apply_default_window(chrono::Utc::now());
    Ok(filter)
}

/// 把查询字符串逐个解析成 [`Filter`]（避免 page / stats 两份重复的解析代码）。
#[allow(clippy::too_many_arguments)]
fn shallow_filter(
    start_time: Option<&str>,
    end_time: Option<&str>,
    operator_id: Option<&str>,
    operator_dept_id: Option<&str>,
    operator_name: Option<&str>,
    module: Option<&str>,
    resource: Option<&str>,
    resource_id: Option<&str>,
    action: Option<&str>,
    result: Option<&str>,
    keyword: Option<&str>,
) -> std::result::Result<Filter, ApiError> {
    Ok(Filter {
        start_time: parse_optional_time(start_time, "start_time")?,
        end_time: parse_optional_time(end_time, "end_time")?,
        operator_id: parse_optional_uuid(operator_id, "operator_id")?,
        operator_dept_id: parse_optional_uuid(operator_dept_id, "operator_dept_id")?,
        operator_name: utils::trimmed(operator_name).map(str::to_string),
        module: utils::trimmed(module).map(str::to_string),
        resource: utils::trimmed(resource).map(str::to_string),
        resource_id: utils::trimmed(resource_id).map(str::to_string),
        action: utils::trimmed(action).map(str::to_string),
        result: utils::trimmed(result).map(str::to_string),
        keyword: utils::trimmed(keyword).map(str::to_string),
    })
}

/// 可空 UUID 参数字符串解析。
fn parse_optional_uuid(
    raw: Option<&str>,
    field: &str,
) -> std::result::Result<Option<uuid::Uuid>, ApiError> {
    match utils::trimmed(raw) {
        Some(value) => utils::require_uuid(value, field).map(Some),
        None => Ok(None),
    }
}

/// 可空时间参数字符串解析（RFC3339 优先，其次 `%Y-%m-%d %H:%M:%S`，最后 `%Y-%m-%d`）。
fn parse_optional_time(
    raw: Option<&str>,
    field: &str,
) -> std::result::Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    match utils::trimmed(raw) {
        Some(value) => parse_time(value, field).map(Some),
        None => Ok(None),
    }
}

/// 单个时间参数解析。
///
/// # 异常情况
///
/// 三种格式都不匹配 → 400 参数错误。
fn parse_time(
    raw: &str,
    field: &str,
) -> std::result::Result<chrono::DateTime<chrono::Utc>, ApiError> {
    if let Ok(value) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(value.with_timezone(&chrono::Utc));
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S") {
        return Ok(naive.and_utc());
    }
    // 只给日期时按当天 00:00:00 UTC 解释：前端日期选择器常只传 `2026-10-01`
    if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(date
            .and_hms_opt(0, 0, 0)
            .unwrap_or_else(|| date.and_hms_opt(0, 0, 0).expect("00:00:00 一定合法"))
            .and_utc());
    }
    Err(ApiError::new(
        400,
        error_code::PARAM_INVALID,
        format!("{field} 时间格式不正确（支持 RFC3339 / YYYY-MM-DD HH:MM:SS / YYYY-MM-DD）"),
    ))
}

/// 构造 `Content-Disposition`（ASCII 回退名 + RFC 5987 编码名，与 impexp 同口径）。
fn content_disposition(file_name: &str) -> String {
    let encoded = percent_encode(file_name);
    let ascii = file_name
        .chars()
        .map(|value| {
            if value.is_ascii() && value != '"' {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// URL 百分号编码（只保留 RFC 3986 非保留字符）。
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row_with_diff(resource: &str, diff: Json) -> ChangeLogRow {
        ChangeLogRow {
            id: uuid::Uuid::now_v7(),
            created_at: chrono::Utc::now(),
            module: "system".to_string(),
            resource: resource.to_string(),
            resource_id: "u-1".to_string(),
            action: "UPDATE".to_string(),
            action_label: "修改用户".to_string(),
            request_id: None,
            trace_source: "API".to_string(),
            operator_id: None,
            operator_name: Some("admin".to_string()),
            operator_dept_id: None,
            client_ip: None,
            diff,
            changed_fields: vec!["mobile".to_string()],
            result: "SUCCESS".to_string(),
            diff_mode: "WHITELIST".to_string(),
            error_code: None,
            duration_ms: None,
        }
    }

    /// 三种时间格式都要能吃（前端日期选择器 / RFC3339 / 带秒）。
    #[test]
    fn parse_time_accepts_three_formats() {
        assert!(parse_time("2026-10-29T10:00:00Z", "t").is_ok());
        assert!(parse_time("2026-10-29 10:00:00", "t").is_ok());
        assert!(parse_time("2026-10-29", "t").is_ok());
        let error = parse_time("昨天", "t").expect_err("非法格式必须 400");
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert_eq!(error.status, 400);
    }

    /// 空白 = 没传（不是「解析失败」）。
    #[test]
    fn blank_time_is_none() {
        assert_eq!(
            parse_optional_time(Some("  "), "t").expect("空白应放行"),
            None
        );
        assert_eq!(parse_optional_time(None, "t").expect("None 应放行"), None);
    }

    /// 列表页默认补 90 天窗口（架构 §4.8）。
    #[test]
    fn list_filter_gets_default_window() {
        let filter = build_filter(&ChangeLogPageQuery::default()).expect("默认条件应可构造");
        assert!(
            filter.start_time.is_some(),
            "列表必须带时间窗，否则扫全部分区"
        );
        assert!(filter.end_time.is_none());
    }

    /// 片段与导出共用的渲染：`{b,a}` 成对、大字段走摘要、顺序按字段名升序。
    #[test]
    fn render_diff_pairs_and_sorts() {
        let diff = json!({
            "v": 1,
            "mode": "WHITELIST",
            "fields": {
                "status": {"b": 1, "a": 0},
                "mobile": {"b": "138****1234", "a": "139****5678", "masked": true}
            },
            "big": {
                "remark": {
                    "b": {"len": 12, "sha256": "abcdef0123456789", "preview": "旧"},
                    "a": {"len": 13, "sha256": "fedcba9876543210", "preview": "新"},
                    "truncated": true
                }
            },
            "meta": {"table": "sys_user", "pk": "u-1", "action": "UPDATE"}
        });
        let row = row_with_diff("sys_user", diff);
        let (text, masked) = render_diff(&HashMap::new(), &row);
        assert_eq!(
            text,
            "mobile: 138****1234 → 139****5678；status: 1 → 0；remark: [len=12 sha=abcdef01] → [len=13 sha=fedcba98]（摘要）",
            "实际：{text}"
        );
        assert!(masked, "含 masked:true 的字段必须被标记");
        assert!(!text.contains("13812341234"), "渲染不得还原明文");
    }

    /// `{redacted:true}` 必须原样呈现成 redacted 标记，而不是空字符串（让人知道「这个值被抹了」）。
    #[test]
    fn render_diff_shows_redacted_marker() {
        let diff = json!({
            "v": 1,
            "mode": "WHITELIST",
            "fields": {"password": {"b": {"redacted": true}, "a": {"redacted": true}, "masked": true}},
            "meta": {"table": "sys_user", "pk": "u-1", "action": "UPDATE"}
        });
        let row = row_with_diff("sys_user", diff);
        let (text, masked) = render_diff(&HashMap::new(), &row);
        assert!(
            text.contains("redacted"),
            "密码类字段要显示为 redacted：{text}"
        );
        assert!(masked);
    }

    /// 空 diff 不 panic、不产生多余分隔符。
    #[test]
    fn render_diff_empty_is_empty() {
        let row = row_with_diff(
            "sys_user",
            json!({"v": 1, "mode": "WHITELIST", "fields": {}}),
        );
        let (text, masked) = render_diff(&HashMap::new(), &row);
        assert!(text.is_empty());
        assert!(!masked);
    }

    /// 中文文件名要能进 Content-Disposition（ASCII 回退 + RFC5987 编码）。
    #[test]
    fn content_disposition_is_rfc5987_safe() {
        let value = content_disposition("变更日志_20261029.xlsx");
        assert!(value.starts_with("attachment; filename=\""));
        assert!(value.contains("filename*=UTF-8''"));
        assert!(!value.contains('\n'), "绝不能把换行带进响应头");
        // ASCII 回退名里不允许出现非 ASCII 字符
        let ascii_part = value.split("filename*=UTF-8''").next().unwrap_or_default();
        assert!(ascii_part.is_ascii());
    }
}
