// ============================================================================
// transfer_scan.rs —— 在途超期扫描：阈值读取、超期判定、候选扫描与告警打标、汇总
//
// 由 `transfer_model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{DateTime, Duration, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::support::config::model::SysConfigModel;

use super::transfer_base::*;

/// 超期阈值参数键（`sys_config`，单位：**天**，按 `shipped_at` 计算）。
///
/// 与迁移 `V0171__transfer_in_transit_overdue.up.sql` 种下的行逐字一致。
pub const CONFIG_OVERDUE_DAYS_KEY: &str = "erp.transfer.in_transit_overdue_days";

/// 超期阈值缺省值（天）。参数缺失 / 停用 / 非数字 / 非正数时一律回退该值。
pub const DEFAULT_OVERDUE_DAYS: i64 = 7;

/// 重提醒间隔参数键（`sys_config`，单位：**小时**）。
pub const CONFIG_REALERT_HOURS_KEY: &str = "erp.transfer.in_transit_realert_hours";

/// 重提醒间隔缺省值（小时）：同一张超期单在间隔内不重复告警。
pub const DEFAULT_REALERT_HOURS: i64 = 24;

/// 一轮扫描最多处理多少张超期单（与 `flow::timeout::SCAN_BATCH` 同量级）。
pub const OVERDUE_SCAN_BATCH: i64 = 500;

/// 判定「超期在途」：`IN_TRANSIT` 且 `shipped_at` 早于 `now - threshold_days`（纯函数）。
///
/// # 为什么时间基准是 `shipped_at`
///
/// 压货的起点是「货离开调出仓」（发货），审批快慢与告警无关。用 `created_at`
/// 会把「审批走了三天」的单子当成压货，用 `approved_at` 同理。
///
/// # 边界
///
/// 恰好等于阈值（`shipped_at == now - threshold_days`）**不算**超期（严格小于），
/// 与 SQL 侧 `shipped_at < ?` 逐字一致 —— 纯函数与库查询不能有两套口径。
pub fn is_in_transit_overdue(
    status: &str,
    shipped_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    threshold_days: i64,
) -> bool {
    if status != STATUS_IN_TRANSIT {
        return false;
    }
    let Some(shipped_at) = shipped_at else {
        // 库约束 `ck_biz_transfer_state` 保证 IN_TRANSIT 必有 shipped_at；
        // 这里仍按「无法判定 → 不告警」，绝不因为脏数据把单据误报成压货。
        return false;
    };
    shipped_at < cutoff_of(now, threshold_days)
}

/// 超期判定的时间分界点（`now - threshold_days` 天）。
///
/// 时间点**在 Rust 侧算好**再绑定给 SQL（`?::timestamptz`）：写进 SQL 的
/// `now() - make_interval(...)` 会让纯函数与库查询的时间基准差几微秒，
/// 边界用例会出现「函数说超期、SQL 说不超期」。
pub fn cutoff_of(now: DateTime<Utc>, threshold_days: i64) -> DateTime<Utc> {
    now - Duration::try_days(threshold_days).unwrap_or_else(Duration::zero)
}

/// 已压天数（向下取整，最少 0）：`now - shipped_at` 的整天数。
///
/// 只用于展示（列表「超期 N 天」）与告警文案，**判定**仍走 [`is_in_transit_overdue`]。
pub fn overdue_elapsed_days(shipped_at: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    let secs = (now - shipped_at).num_seconds().max(0);
    secs / 86_400
}

/// 读超期阈值（天）：`sys_config` → 缺省 [`DEFAULT_OVERDUE_DAYS`]。
///
/// # 为什么读取失败也回退默认值
///
/// 这条参数是「多久算压货」的业务口径，读不到就**不告警**等于功能整体失效；
/// 回退 7 天最多是「按默认口径告警」，属于可接受的降级（与
/// `announcement::fanout_enabled` 读失败按「开启」处理同一取舍）。
pub async fn overdue_threshold_days() -> i64 {
    SysConfigModel::read_i64(CONFIG_OVERDUE_DAYS_KEY)
        .await
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_OVERDUE_DAYS)
}

/// 读重提醒间隔（小时）：`sys_config` → 缺省 [`DEFAULT_REALERT_HOURS`]。
pub async fn realert_hours() -> i64 {
    SysConfigModel::read_i64(CONFIG_REALERT_HOURS_KEY)
        .await
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_REALERT_HOURS)
}

// ==================== 6 态常量（与 DDL 的 CHECK 逐字一致） ====================

/// 一张待告警的超期在途单（扫描任务的输入，只带通知需要的字段）。
#[derive(Debug, Clone)]
pub struct OverdueCandidate {
    /// 调拨单主键。
    pub id: Uuid,
    /// 单号（告警标题与跳转参数）。
    pub transfer_no: String,
    /// 申请人（发货方）：告警的第一收件人。
    pub applicant_id: Option<Uuid>,
    /// 审批人（可选收件人）。审批人就是「批了这张单的人」，货压着他也该知道。
    pub approver_id: Option<Uuid>,
    /// 发货时间（已压天数的起点）。
    pub shipped_at: DateTime<Utc>,
    /// 累计告警次数（CAS 的期望值 + 通知幂等键的序号段）。
    pub overdue_alert_count: i16,
    /// 调出仓（告警正文里写清「从哪到哪」）。
    pub from_warehouse_id: Uuid,
    /// 调入仓。
    pub to_warehouse_id: Uuid,
}

/// 扫描「该告警的超期在途单」。
///
/// # 判据（与 [`is_in_transit_overdue`] 同一口径）
///
/// ```text
/// status = 'IN_TRANSIT'
///   AND shipped_at IS NOT NULL
///   AND shipped_at < now - threshold_days       -- 超期
///   AND (overdue_alerted_at IS NULL             -- 从没提醒过
///        OR overdue_alerted_at < now - realert_hours)  -- 或过了重提醒间隔
/// ORDER BY shipped_at ASC LIMIT ?
/// ```
///
/// 时间点全部在 Rust 侧算好再绑 `?::timestamptz`（W3c 教训：`now() - interval`
/// 会让 Rust 侧算的「已压天数」与 SQL 侧的分界差几微秒）。
///
/// 走 V0171 的部分索引 `idx_biz_transfer_in_transit_shipped`。
pub async fn scan_overdue_candidates(
    ex: &impl SqlExecutor,
    now: DateTime<Utc>,
    threshold_days: i64,
    realert_hours: i64,
    limit: i64,
) -> DbResult<Vec<OverdueCandidate>> {
    let cutoff = cutoff_of(now, threshold_days);
    let realert_before = now - Duration::try_hours(realert_hours).unwrap_or_else(Duration::zero);
    // SCOPE-EXEMPT: 调拨单是仓储作业单据，三张表都没有 dept_id / 归属人列（口径同文件头）。
    let rows = ex
        .query_sql(
            "SELECT id, transfer_no, applicant_id, approver_id, shipped_at, \
                    overdue_alert_count, from_warehouse_id, to_warehouse_id \
               FROM biz_stock_transfer \
              WHERE deleted_at IS NULL AND status = 'IN_TRANSIT' AND shipped_at IS NOT NULL \
                AND shipped_at < ?::timestamptz \
                AND (overdue_alerted_at IS NULL OR overdue_alerted_at < ?::timestamptz) \
              ORDER BY shipped_at ASC, id ASC LIMIT ?"
                .to_string(),
            vec![
                Value::from(cutoff),
                Value::from(realert_before),
                Value::I64(limit),
            ],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(OverdueCandidate {
                id: row.get_typed("id")?,
                transfer_no: row.get_typed("transfer_no")?,
                applicant_id: row.get_typed("applicant_id")?,
                approver_id: row.get_typed("approver_id")?,
                shipped_at: row.get_typed("shipped_at")?,
                overdue_alert_count: row.get_typed("overdue_alert_count")?,
                from_warehouse_id: row.get_typed("from_warehouse_id")?,
                to_warehouse_id: row.get_typed("to_warehouse_id")?,
            })
        })
        .collect()
}

/// 记一次告警（CAS）：只有 `overdue_alert_count` 仍是读到的旧值时才写。
///
/// # 为什么是 CAS 而不是无条件 UPDATE
///
/// 多实例 / 多轮并发扫描时，同一张单可能被两个扫描同时捞到。CAS 保证只有一方能把
/// 台账 +1：另一方看到 `affected = 0` 就放弃（通知侧还有 `uk_sys_notice_idem`
/// 按「第几次」去重，两层加起来不会出现「一张单两条一模一样的告警」）。
///
/// # 返回值
///
/// 受影响行数（`1` = 本次真的记上了；`0` = 被别的扫描抢先，跳过）。
pub async fn mark_overdue_alerted(
    ex: &impl SqlExecutor,
    id: Uuid,
    expected_count: i16,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_stock_transfer \
            SET overdue_alerted_at = ?, overdue_alert_count = overdue_alert_count + 1, \
                updated_at = ? \
          WHERE id = ?::uuid AND overdue_alert_count = ? AND status = 'IN_TRANSIT'"
            .to_string(),
        vec![
            Value::from(now),
            Value::from(now),
            Value::from(&id),
            Value::I16(expected_count),
        ],
    )
    .await
}

/// 超期在途汇总（告警中心 / 看板用）。
#[derive(Debug, Clone)]
pub struct OverdueSummary {
    /// 判定用的阈值（天）—— 前端要写「超过 N 天未收货」。
    pub threshold_days: i64,
    /// 超期单数。
    pub total: i64,
    /// 最早一张的发货时间（`None` = 没有超期单）。
    pub oldest_shipped_at: Option<DateTime<Utc>>,
}

/// 统计超期在途（全库口径，不受分页影响）。
///
/// SCOPE-EXEMPT: 同上，调拨单无数据范围列。
pub async fn overdue_summary(threshold_days: i64, now: DateTime<Utc>) -> DbResult<OverdueSummary> {
    let cutoff = cutoff_of(now, threshold_days);
    let row = db()
        .find_with_params(
            "SELECT count(*) AS total, min(shipped_at) AS oldest_shipped_at \
               FROM biz_stock_transfer \
              WHERE deleted_at IS NULL AND status = 'IN_TRANSIT' AND shipped_at IS NOT NULL \
                AND shipped_at < ?::timestamptz",
            vec![Value::from(cutoff)],
        )
        .await?;
    let Some(row) = row.first() else {
        return Ok(OverdueSummary {
            threshold_days,
            total: 0,
            oldest_shipped_at: None,
        });
    };
    Ok(OverdueSummary {
        threshold_days,
        total: row.get_typed("total")?,
        oldest_shipped_at: row.get_typed("oldest_shipped_at")?,
    })
}
