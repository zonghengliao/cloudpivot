// ============================================================================
// task_model_stats_tests.rs —— 统计窗口与完成率
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::NaiveDate;

use super::*;

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("合法日期")
}

#[test]
fn completion_rate_excludes_cancelled_and_clamps() {
    assert_eq!(completion_rate(0, 0, 0), 0.0, "空窗口给 0.0 而不是 NaN");
    assert_eq!(completion_rate(0, 5, 5), 0.0, "全取消 → 分母 0 → 0.0");
    assert_eq!(completion_rate(3, 4, 0), 0.75);
    assert_eq!(completion_rate(4, 5, 1), 1.0, "取消剔除后 4/4 = 100%");
    assert_eq!(completion_rate(1, 3, 0), 0.3333, "保留 4 位小数");
    assert_eq!(completion_rate(9, 3, 0), 1.0, "脏数据也不返回 >1");
}

#[test]
fn default_stats_window_is_thirty_days_inclusive() {
    let end = date(2026, 10, 29);
    let (start, same_end) = stats_default_window(end);
    assert_eq!(same_end, end);
    assert_eq!(start, date(2026, 9, 30));
    assert_eq!((end - start).num_days() + 1, 30);
    // 跨月 / 跨年边界
    let (start, _) = stats_default_window(date(2026, 1, 1));
    assert_eq!(start, date(2025, 12, 3));
}

#[test]
fn stats_window_error_covers_reversed_and_too_long() {
    let start = date(2026, 10, 1);
    assert!(stats_window_error(start, date(2026, 10, 29)).is_none());
    assert!(stats_window_error(start, start).is_none(), "单日窗口合法");
    assert!(
        stats_window_error(date(2026, 10, 2), start)
            .expect("倒置必须报错")
            .contains("开始日")
    );
    let too_long = stats_window_error(date(2025, 1, 1), date(2026, 10, 29));
    assert!(too_long.expect("超 366 天必须报错").contains("366"), "文案必须点名上限");
    assert_eq!(MAX_STATS_WINDOW_DAYS, 366);
}
