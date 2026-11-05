// ============================================================================
// task_handler_stats_tests.rs —— 统计口径纯函数用例（加权平均 / 合计行 / 舍入）
//
// 由 `handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use crate::business::task::dto;
use crate::business::task::model;

use super::*;

fn row(mode: &str, done: i64, cancelled: i64, hours: Option<f64>) -> dto::ExecModeStatRes {
    let total = done + cancelled + 1;
    dto::ExecModeStatRes {
        exec_mode: mode.to_string(),
        total,
        todo: 1,
        doing: 0,
        review: 0,
        done,
        cancelled,
        completion_rate: model::completion_rate(done, total, cancelled),
        avg_done_hours: hours,
        avg_progress: Some(50.0),
    }
}

#[test]
fn total_is_weighted_by_done_count_not_a_plain_mean() {
    let rows = vec![
        row("HUMAN", 100, 0, Some(10.0)),
        row("AI", 1, 0, Some(40.0)),
    ];
    let total = stats_total(&rows);
    assert_eq!(total.total, 103, "101 + 2（每行多 1 条待开始）");
    assert_eq!(total.done, 101);
    assert_eq!(
        total.avg_done_hours,
        Some(10.3),
        "加权：(10×100 + 40×1)/101 ≈ 10.297 → 10.3；直接平均会得 25.0"
    );
}

#[test]
fn total_handles_empty_and_cancelled_only() {
    let rows = vec![row("HUMAN", 0, 0, None), row("AI", 0, 3, None)];
    let total = stats_total(&rows);
    assert_eq!(total.avg_done_hours, None, "没有已完成任务 → null 而不是 0");
    assert_eq!(total.completion_rate, 0.0);
    assert_eq!(total.exec_mode, "TOTAL");
}

#[test]
fn weighted_average_ignores_zero_and_negative_weights() {
    assert_eq!(weighted_average(std::iter::empty()), None);
    assert_eq!(weighted_average(vec![(5.0, 0)].into_iter()), None);
    assert_eq!(
        weighted_average(vec![(5.0, 0), (3.0, 2)].into_iter()),
        Some(3.0)
    );
    assert_eq!(
        weighted_average(vec![(99.0, -4), (2.0, 1)].into_iter()),
        Some(2.0),
        "负权重按 0 处理，不能把平均值带偏"
    );
}
