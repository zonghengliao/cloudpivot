//! 「我的指标」数据访问。
//!
//! 四张卡的数字全部来自**既有查询函数**，本文件不复制任何一条 SQL 谓词：
//!
//! | 卡片 | 取数 | 与谁同口径 |
//! |------|------|-----------|
//! | 待办审批 | [`flow_model::page_my_tasks`]（`MY_TODO_STATUSES` + `live_only`） | `/business/flow/task/todo` |
//! | 本周已办 | [`flow_model::page_my_tasks`]（`MY_DONE_STATUSES` + `completed_since`） | `/business/flow/task/done?since=` |
//! | 今日任务 / 逾期 | [`task_model::my_tasks`] + [`task_model::bucket_of`] | `/business/task/my` |
//! | 今日已完成 | [`task_model::count_done_between`]（按 `done_at` 落在 CST 今天） | 任务详情页的「已完成」 |
//!
//! 为什么不各写一条 `count(*)`：卡片和列表用的谓词一旦分叉，就会出现
//! 「卡片说 8、点进去只有 7」——这种差异用户无法判断是口径还是数据的问题。
//! 宁可多调一个函数（一次索引扫描），也不复制谓词。
//!
//! # 时间口径
//!
//! 全部按 **CST（UTC+8）** 切自然日：老大的时区，也是 `task_model::today_cst` 的口径。
//! PostgreSQL 的 `date_trunc('week', now())` 按服务器时区算，容器里通常是 UTC，
//! 直接用它会让「本周已办」在北京时间周一 00:00–08:00 把上周的单子算进来，
//! 所以周起点在 Rust 侧算（[`week_start_cst`]）再作为下界绑定。

use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::{dto as flow_dto, model as flow_model};
use crate::business::task::model as task_model;
use crate::support::dashboard::dto::MetricCardRes;

/// CST 时区（中国不实行夏令时，固定 UTC+8）。
const CST_OFFSET_SECS: i32 = 8 * 3600;

/// 四项指标 + 一个附加数（今日已完成）的原始计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MyMetricCounts {
    /// 待我审批的单子数（实例还在跑的）。
    pub todo_count: i64,
    /// 本周我审批过（同意 / 驳回 / 转办）的单子数。
    pub done_week: i64,
    /// 今天到期的任务数（不含逾期）。
    pub task_today: i64,
    /// 已逾期（`due_date < 今天`）且未终态的任务数。
    pub overdue: i64,
    /// 今天完成的任务数（`done_at` 落在 CST 今天）。
    pub task_done_today: i64,
}

/// CST 时区偏移量。
fn cst_offset() -> FixedOffset {
    FixedOffset::east_opt(CST_OFFSET_SECS).expect("UTC+8 是合法固定偏移")
}

/// 某个 CST 自然日的 `[00:00, 次日 00:00)`，换算成 UTC 区间。
///
/// 半开区间（`>= from AND < to`）而不是 `::date` 比较：`done_at` 上有索引时
/// 区间扫描能直接用上，`(done_at AT TIME ZONE ...)::date = ?` 会退化成表达式索引缺失。
pub fn cst_day_bounds_utc(day: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let offset = cst_offset();
    let start = offset
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).expect("00:00:00 恒为合法时刻"))
        .single()
        .expect("CST 无夏令时，本地时刻唯一")
        .with_timezone(&Utc);
    (start, start + Duration::days(1))
}

/// 某个 CST 自然日所在 ISO 周的周一 00:00（CST），返成 UTC 瞬间。
///
/// 与 PostgreSQL `date_trunc('week', ...)` 的「周一为一周之首」一致，
/// 只是把时区基准从服务器时区改成 CST。
pub fn week_start_cst(day: NaiveDate) -> DateTime<Utc> {
    let monday = day - Duration::days(day.weekday().num_days_from_monday() as i64);
    cst_day_bounds_utc(monday).0
}

/// 取「我」的四项指标。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn load_counts(me: Uuid) -> DbResult<MyMetricCounts> {
    // ① 待办审批：与审批中心「待我审批」同一条谓词（只认实例还在跑的，P11）。
    //    `page_size = 1` 是为了拿 `total`，不是要那一条数据。
    let todo_page = flow_model::page_my_tasks(
        me,
        flow_model::MY_TODO_STATUSES,
        &flow_dto::TASK_KINDS,
        flow_model::MyTaskOrder::Arrived,
        1,
        1,
        true,
        None,
    )
    .await?;

    // ② 本周已办：`completed_at >= 本周一 00:00 (CST)`。
    //    已办是历史记录，不看实例是否结束（与 `/task/done` 的 `live_only = false` 一致）。
    let today = task_model::today_cst();
    let done_page = flow_model::page_my_tasks(
        me,
        flow_model::MY_DONE_STATUSES,
        &flow_dto::TASK_KINDS,
        flow_model::MyTaskOrder::Operated,
        1,
        1,
        false,
        Some(week_start_cst(today)),
    )
    .await?;

    // ③ 任务的三个数：一次取回「我负责的未终态任务」，用与 `/business/task/my`
    //    完全相同的 `bucket_of` 分桶，再单独按 `done_at` 数今天完成的。
    let rows = task_model::my_tasks(me).await?;
    let mut task_today = 0_i64;
    let mut overdue = 0_i64;
    for row in &rows {
        match task_model::bucket_of(row.due_date, today) {
            task_model::MyBucket::Today => task_today += 1,
            task_model::MyBucket::Overdue => overdue += 1,
            // 本周 / 以后 / 无期限不在这四张卡里
            _ => {}
        }
    }
    let (day_from, day_to) = cst_day_bounds_utc(today);
    let task_done_today = task_model::count_done_between(me, day_from, day_to).await?;

    Ok(MyMetricCounts {
        todo_count: todo_page.total as i64,
        done_week: done_page.total as i64,
        task_today,
        overdue,
        task_done_today,
    })
}

/// 计数 → 四张指标卡（顺序 = 前端展示顺序）。
///
/// 纯函数：不查库、不看时钟，单测直接喂计数验卡片形状。
/// `icon` 全部取 TDesign 图标库**确实存在**的名字（`task` / `check-circle` /
/// `calendar` / `error-circle`），写错名字前端只会渲染一个空白方块。
pub fn build_cards(counts: MyMetricCounts) -> Vec<MetricCardRes> {
    vec![
        MetricCardRes {
            key: "my_todo_count".to_string(),
            label: "待办审批".to_string(),
            value: counts.todo_count,
            extra: 0,
            unit: "项".to_string(),
            icon: "task".to_string(),
            theme: "blue".to_string(),
            tip: "待我审批且流程仍在进行的单据数".to_string(),
        },
        MetricCardRes {
            key: "my_done_week".to_string(),
            label: "本周已办".to_string(),
            value: counts.done_week,
            extra: 0,
            unit: "项".to_string(),
            icon: "check-circle".to_string(),
            theme: "green".to_string(),
            tip: "本周（周一 00:00 起）我审批过的单据数".to_string(),
        },
        MetricCardRes {
            key: "my_task_today".to_string(),
            label: "今日任务".to_string(),
            value: counts.task_today + counts.overdue,
            extra: counts.task_done_today,
            unit: "项".to_string(),
            icon: "calendar".to_string(),
            theme: "blue".to_string(),
            tip: "今天到期的任务（含已逾期）；附加数为今天已完成".to_string(),
        },
        MetricCardRes {
            key: "my_overdue".to_string(),
            label: "逾期".to_string(),
            value: counts.overdue,
            extra: 0,
            unit: "项".to_string(),
            icon: "error-circle".to_string(),
            theme: "red".to_string(),
            tip: "截止日早于今天、且未完成 / 未取消的任务数".to_string(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-09 是周一（ISO 周起点）。
    fn monday() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 9).unwrap()
    }

    #[test]
    fn week_start_is_monday_midnight_cst() {
        // 周一 → 自己；周三 / 周日 → 都是同一周的周一。
        assert_eq!(week_start_cst(monday()), week_start_cst(monday() + Duration::days(2)));
        assert_eq!(week_start_cst(monday()), week_start_cst(monday() + Duration::days(6)));

        // 周一 00:00 CST == 周日 16:00 UTC（UTC+8）。
        let start = week_start_cst(monday() + Duration::days(2));
        assert_eq!(start.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-11-08 16:00:00");
    }

    #[test]
    fn day_bounds_cover_exactly_one_day() {
        let (from, to) = cst_day_bounds_utc(monday());
        assert_eq!(to - from, Duration::days(1), "半开区间必须正好一天");
        // CST 今天 00:00 = UTC 前一天 16:00
        assert_eq!(from.format("%Y-%m-%d %H:%M").to_string(), "2026-11-08 16:00");
    }

    #[test]
    fn cards_have_fixed_keys_and_order() {
        let cards = build_cards(MyMetricCounts {
            todo_count: 8,
            done_week: 23,
            task_today: 4,
            overdue: 1,
            task_done_today: 2,
        });
        let keys: Vec<&str> = cards.iter().map(|card| card.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["my_todo_count", "my_done_week", "my_task_today", "my_overdue"],
            "前端按 key 决定跳转，顺序 / 键名都不许漂移"
        );
        // 今日任务 = 今天到期 + 逾期；附加数 = 今天已完成
        assert_eq!(cards[2].value, 5);
        assert_eq!(cards[2].extra, 2);
        // 逾期单独一张卡，值不掺进别处
        assert_eq!(cards[3].value, 1);
        // 单位统一「项」，图标名非空（写错名字前端渲染空白块，这里至少钉住非空）
        assert!(cards.iter().all(|card| card.unit == "项" && !card.icon.is_empty()));
    }

    #[test]
    fn zero_counts_stay_zero() {
        let cards = build_cards(MyMetricCounts::default());
        assert!(cards.iter().all(|card| card.value == 0 && card.extra == 0));
        assert_eq!(cards.len(), 4, "空数据也要给出 4 张卡（显示 0，不是不显示）");
    }
}
