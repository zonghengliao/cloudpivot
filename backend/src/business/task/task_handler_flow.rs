// ============================================================================
// task_handler_flow.rs —— 流程与统计：任务流程来源查询、按执行模式的统计聚合
//
// 由 `handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use axum::extract::{Path, Query};
use nik::prelude::*;

use super::task_handler_common::*;
use crate::business::shared::principal::{self};
use crate::business::task::dto::{self};
use crate::business::task::model::{self};
use crate::utils;

/// 任务所属流程（`GET /business/task/{id}/flow`）。
///
/// 任务详情页的「所属审批单」卡片用它：单号 / 流程名 / 发起人 / 当前节点 / 实例状态。
///
/// # 手工任务也 200
///
/// `source != 'FLOW'` 的任务 `flow` 字段为 `null`，**不是 404**：
/// 「这张任务没有流程」是正常事实（两秒建任务的人不会因此看到一个错误弹窗）。
///
/// # 可见性
///
/// 与详情接口**同一套**（`数据范围 ∪ 参与关系`）：看不见的任务一律 404，不暴露「存在但无权」。
///
/// # 异常情况
///
/// - 400 + `10001`：路径里的 id 不是 UUID；
/// - 404 + `63001`：任务不存在 / 已软删 / 不在可见范围内；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/flow",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/flow")]
pub async fn flow_of_task(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<dto::TaskFlowRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let me = principal::resolve_for_claims(&claims).await?;
    // 可见性口径与详情接口一致：看不见的任务一律 404（不暴露「存在但无权」）。
    let row = model::find_detail(&me, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;
    let flow = model::flow_ref_of_task(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务所属流程"))?;
    Ok(Res::ok(dto::TaskFlowRes {
        task_id: task_id.to_string(),
        task_no: row.card.task_no.clone(),
        flow: flow.map(|flow| dto::TaskFlowDetailRes {
            instance_id: flow.instance_id.to_string(),
            instance_no: flow.instance_no,
            flow_code: flow.flow_code,
            flow_name: flow.flow_name,
            biz_type: flow.biz_type,
            biz_no: flow.biz_no.unwrap_or_default(),
            instance_status: flow.instance_status,
            initiator_id: flow.initiator_id.to_string(),
            initiator_name: flow.initiator_name,
            started_at: flow.started_at,
            ended_at: flow.ended_at,
            node_key: flow.node_key,
            node_name: flow.node_name,
            round: flow.round,
            node_status: flow.node_status,
            on_complete: flow.on_complete,
            on_reject: flow.on_reject,
            on_cancel: flow.on_cancel,
            approval: flow.approval.map(|approval| dto::TaskFlowApprovalRes {
                instance_id: approval.instance_id.to_string(),
                instance_no: approval.instance_no,
                status: approval.status,
                pending_approvers: approval.pending_approvers,
            }),
        }),
    }))
}

// ==================== 统计（T5b · b4：exec_mode 维度） ====================

/// 按执行模式统计（`GET /business/task/stats`，**人类会话调用**）。
///
/// # 为什么要单开一个接口，而不是在 `page` 上加 `group_by`
///
/// 「人类 / AI 分别统计完成率与平均耗时」是**聚合**语义：它要跨全部可见任务做
/// `GROUP BY exec_mode`，而分页接口每页最多 200 行，前端自己算出来的完成率只代表
/// 当前页。聚合必须在服务端做，且必须走同一条可见性谓词（否则「我的完成率」会被
/// 别人的任务稀释）。
///
/// # 出参口径
///
/// - `by_exec_mode` **恒含 `HUMAN` 与 `AI` 两行**，窗口内没有任务就给全 0；
/// - `total` 是合计行：计数直接相加，**平均耗时按已完成数加权、平均进度按有效任务数加权**
///   （见 [`stats_total`]）；
/// - 窗口按**创建时间**的 CST 日区间，默认最近 30 天（含今天），上限 366 天。
///
/// # 权限
///
/// 与列表 / 甘特同一套：`business:task:read` 决定能否进页面，
/// `scope ∪ 参与关系` 决定能统计到哪些任务。
///
/// # 异常情况
///
/// - 400 + `10001`：日期 / uuid / scope 取值非法，或窗口倒置 / 超过 366 天；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/stats",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/stats")]
pub async fn stats(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<dto::TaskStatsQuery>,
) -> ResResult<dto::TaskStatsRes> {
    let principal = principal::resolve_for_claims(&claims).await?;
    let end = query.end.unwrap_or_else(model::today_cst);
    let start = query
        .start
        .unwrap_or_else(|| model::stats_default_window(end).0);
    if let Some(message) = model::stats_window_error(start, end) {
        return Err(param_invalid(message));
    }
    let project_id = match query.project_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(parse_uuid_field(raw, "项目")?),
    };
    let mine = match query.scope.as_deref().map(str::trim) {
        None | Some("") | Some("all") => None,
        Some("my") => Some(principal.user_id()),
        Some(raw) => {
            return Err(param_invalid(format!(
                "scope 取值非法：{raw}（应为 my / all）"
            )));
        }
    };

    let rows = model::task_stats(
        &principal,
        &model::StatsFilter {
            start,
            end,
            project_id,
            mine,
        },
    )
    .await
    .map_err(|error| utils::db_error(error, "统计任务"))?;

    // 固定两行（HUMAN / AI）——前端的对比卡片是固定两列
    let mut by_exec_mode: Vec<dto::ExecModeStatRes> = Vec::with_capacity(2);
    for mode in ["HUMAN", "AI"] {
        let row = rows.iter().find(|item| item.exec_mode == mode);
        by_exec_mode.push(stats_row(mode, row));
    }
    // 兜底：库里出现第三种 exec_mode（历史脏数据）时如实列出，不静默吞掉
    for row in &rows {
        if !matches!(row.exec_mode.as_str(), "HUMAN" | "AI") {
            by_exec_mode.push(stats_row(&row.exec_mode, Some(row)));
        }
    }
    let total = stats_total(&by_exec_mode);

    tracing::info!(
        start = %start,
        end = %end,
        modes = by_exec_mode.len(),
        total_tasks = total.total,
        "任务统计已生成（按执行模式）"
    );
    Ok(Res::ok(dto::TaskStatsRes {
        start,
        end,
        total,
        by_exec_mode,
    }))
}

/// 把一行统计转成响应（`None` = 窗口内没有该模式的任务 → 全 0）。
pub(super) fn stats_row(mode: &str, row: Option<&model::ExecModeStat>) -> dto::ExecModeStatRes {
    let empty = model::ExecModeStat {
        exec_mode: mode.to_string(),
        ..Default::default()
    };
    let stat = row.unwrap_or(&empty);
    dto::ExecModeStatRes {
        exec_mode: mode.to_string(),
        total: stat.total,
        todo: stat.todo,
        doing: stat.doing,
        review: stat.review,
        done: stat.done,
        cancelled: stat.cancelled,
        completion_rate: model::completion_rate(stat.done, stat.total, stat.cancelled),
        avg_done_hours: stat.avg_done_hours.map(round_one_decimal),
        avg_progress: stat.avg_progress.map(round_one_decimal),
    }
}

/// 合计行：计数直接相加；**平均值按权重加权**，不是把两个平均数再平均。
///
/// 反例（为什么必须加权）：人类 100 条已完成（平均 10 h）+ AI 1 条已完成（平均 40 h）。
/// 直接平均 = 25 h（把一条 AI 任务的权重放大到 50%）；加权 = `(10×100 + 40×1)/101 ≈ 10.3 h`，
/// 这才是「全体任务的平均耗时」。
pub(super) fn stats_total(rows: &[dto::ExecModeStatRes]) -> dto::ExecModeStatRes {
    let sum = |pick: fn(&dto::ExecModeStatRes) -> i64| rows.iter().map(pick).sum::<i64>();
    let total = sum(|row| row.total);
    let done = sum(|row| row.done);
    let cancelled = sum(|row| row.cancelled);
    let hours = weighted_average(
        rows.iter()
            .filter_map(|row| row.avg_done_hours.map(|avg| (avg, row.done))),
    );
    let progress = weighted_average(rows.iter().filter_map(|row| {
        let effective = (row.total - row.cancelled).max(0);
        row.avg_progress.map(|avg| (avg, effective))
    }));
    dto::ExecModeStatRes {
        exec_mode: "TOTAL".to_string(),
        total,
        todo: sum(|row| row.todo),
        doing: sum(|row| row.doing),
        review: sum(|row| row.review),
        done,
        cancelled,
        completion_rate: model::completion_rate(done, total, cancelled),
        avg_done_hours: hours.map(round_one_decimal),
        avg_progress: progress.map(round_one_decimal),
    }
}

/// 加权平均 `Σ(value × weight) / Σweight`；权重和为 0 时返回 `None`。
pub(super) fn weighted_average<I>(items: I) -> Option<f64>
where
    I: Iterator<Item = (f64, i64)>,
{
    let (weighted, weight) = items.fold((0.0_f64, 0_i64), |(sum, count), (value, item_weight)| {
        let item_weight = item_weight.max(0);
        (sum + value * item_weight as f64, count + item_weight)
    });
    if weight == 0 {
        None
    } else {
        Some(weighted / weight as f64)
    }
}

/// 保留 1 位小数（`10.26 → 10.3`）。前端只显示 1 位，服务端先归一避免「9.9 vs 10.0」两处不一致。
pub(super) fn round_one_decimal(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}
