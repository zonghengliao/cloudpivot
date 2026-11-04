//! 巡检结果看板与分页的请求 / 响应 DTO。
//!
//! 字段名与前端契约（`frontend/src/api/support/invariant-api.ts`）严格对齐。
//!
//! # 为什么把「明细」与「摘要」拆成两个结构
//!
//! 一次运行有几十条检查、每条带中文 `detail`（可能好几行）。分页列表里带全量明细会让
//! 一页 20 行变成几百 KB，而列表页只显示结论与计数。因此：
//! [`ResultRes`] 只带计数（列表 / 最新一行用），[`ResultDetailRes`] 才带 `detail` 与
//! `failures`（点开某次运行才拉）。

use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::support::invariant::model::{SysInvariantResultModel, TrendRow};

/// 一条检查项（与 `scripts/verify/erp_invariants.py` 的 `dict(name, group, level, ok, detail)` 同名同义）。
///
/// 脚本是唯一产出方，本结构**只做透传**：字段名改动会直接打断解析，因此两边必须同步改。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckItem {
    /// 检查项名称（脚本里的 `name`）。
    pub name: String,
    /// 检查分组（会计 / 账账 / 账实 / 单据链 / 状态 / 期间 / 闭环 / 审批 / 盘点 / 固资）。
    pub group: String,
    /// 级别：`BLOCKER` / `WARN` / `INFO`。
    pub level: String,
    /// 是否通过。
    pub ok: bool,
    /// 说明（失败时「差多少、差在哪几条」）。
    #[serde(default)]
    pub detail: String,
}

/// 一次巡检运行的摘要（列表 / 最新一行）。
#[derive(Debug, Clone, Serialize)]
pub struct ResultRes {
    /// 结果 ID。
    pub id: String,
    /// 本次巡检开始时间。
    pub run_at: chrono::DateTime<chrono::Utc>,
    /// 触发方式：`SCHEDULED` / `MANUAL`。
    pub trigger_type: String,
    /// 运行结论：`OK` / `FAIL` / `ERROR`。
    pub status: String,
    /// 检查总数。
    pub total: i32,
    /// 通过数。
    pub passed: i32,
    /// 失败数。
    pub failed: i32,
    /// BLOCKER 数。
    pub blockers: i32,
    /// WARN 数。
    pub warns: i32,
    /// 耗时（毫秒）。
    pub duration_ms: i32,
    /// 巡检目标库名。
    pub database_name: String,
    /// 实际执行的脚本路径。
    pub script_path: String,
    /// `status = ERROR` 时的原因（其它情况为空串）。
    pub error: String,
}

impl From<&SysInvariantResultModel> for ResultRes {
    fn from(m: &SysInvariantResultModel) -> Self {
        Self {
            id: m.id.to_string(),
            run_at: m.run_at,
            trigger_type: m.trigger_type.clone(),
            status: m.status.clone(),
            total: m.total,
            passed: m.passed,
            failed: m.failed,
            blockers: m.blockers,
            warns: m.warns,
            duration_ms: m.duration_ms,
            database_name: m.database_name.clone(),
            script_path: m.script_path.clone(),
            error: m.error.clone().unwrap_or_default(),
        }
    }
}

/// 一次巡检运行的完整记录（详情页 / 手动执行返回值）。
#[derive(Debug, Clone, Serialize)]
pub struct ResultDetailRes {
    /// 摘要字段。
    #[serde(flatten)]
    pub summary: ResultRes,
    /// 全量检查明细。
    pub detail: Vec<CheckItem>,
    /// 失败项子集（`ok = false`）。
    pub failures: Vec<CheckItem>,
}

impl ResultDetailRes {
    /// 由模型构造详情响应。
    ///
    /// # 参数
    ///
    /// - `m`：结果模型引用。
    ///
    /// # 返回值
    ///
    /// 明细 JSONB 解析失败时退化为空列表（不在详情页造 500：摘要仍然可信）。
    pub fn from_model(m: &SysInvariantResultModel) -> Self {
        let detail = decode_items(m.detail.as_ref());
        let failures = decode_items(m.failures.as_ref());
        Self {
            summary: ResultRes::from(m),
            detail,
            failures,
        }
    }
}

/// 解码 JSONB 里的检查项数组。
///
/// # 参数
///
/// - `value`：`detail` / `failures` 列原始 JSON。
///
/// # 返回值
///
/// 解析成功返回数组；`None` 或结构不符返回空数组。
pub fn decode_items(value: Option<&JsonValue>) -> Vec<CheckItem> {
    value
        .and_then(|raw| serde_json::from_value::<Vec<CheckItem>>(raw.clone()).ok())
        .unwrap_or_default()
}

/// 趋势点（某一天的最后一次运行 + 当天运行次数）。
#[derive(Debug, Clone, Serialize)]
pub struct TrendPointRes {
    /// 业务日（`YYYY-MM-DD`，北京时间）。
    pub date: String,
    /// 当天运行次数。
    pub runs: i64,
    /// 当天最后一次运行的结论。
    pub status: String,
    /// 检查总数。
    pub total: i32,
    /// 通过数。
    pub passed: i32,
    /// 失败数。
    pub failed: i32,
    /// BLOCKER 数。
    pub blockers: i32,
    /// WARN 数。
    pub warns: i32,
}

impl From<TrendRow> for TrendPointRes {
    fn from(r: TrendRow) -> Self {
        Self {
            date: r.date,
            runs: r.runs,
            status: r.status,
            total: r.total,
            passed: r.passed,
            failed: r.failed,
            blockers: r.blockers,
            warns: r.warns,
        }
    }
}

/// 看板总览响应。
#[derive(Debug, Clone, Serialize)]
pub struct OverviewRes {
    /// 趋势窗口天数。
    pub window_days: i64,
    /// 装配时间（前端显示「数据截至」）。
    pub generated_at: chrono::DateTime<chrono::Utc>,
    /// 最近一次运行（从未跑过为 `null`）。
    pub latest: Option<ResultRes>,
    /// 窗口内每日趋势（按日期升序；没跑的天不补零）。
    pub trend: Vec<TrendPointRes>,
    /// 当前失败项（取最近一次运行的 `failures`）。
    pub failures: Vec<CheckItem>,
    /// 连续非 OK 的运行次数（最新一次为 OK 时为 0）。
    pub fail_streak: i64,
    /// 窗口内运行次数。
    pub runs_in_window: i64,
    /// 内置调度口径说明（前端直接展示，避免「多久跑一次」靠猜）。
    pub schedule: ScheduleRes,
}

/// 内置调度口径（前端展示用）。
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleRes {
    /// 任务名（同时是 `sys_job.invoke_target`）。
    pub task_name: String,
    /// cron 表达式。
    pub cron: String,
    /// 人话描述。
    pub description: String,
}

/// 看板查询参数。
#[derive(Debug, Clone, Deserialize)]
pub struct OverviewQuery {
    /// 趋势窗口天数（默认 30，clamp 到 1..=365）。
    pub days: Option<i64>,
}

/// 运行历史分页查询参数。
#[derive(Debug, Clone, Deserialize)]
pub struct ResultPageQuery {
    /// 页码，从 1 开始。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 运行结论过滤（`OK` / `FAIL` / `ERROR`，大小写不敏感）。
    pub status: Option<String>,
}
