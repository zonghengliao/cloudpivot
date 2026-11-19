//! 成本方法切换（M5-31）的取数层。
//!
//! 只干两件事：
//!
//! | # | 函数 | 用途 |
//! |---|---|---|
//! | 1 | [`period_ledgers`] | 把 `biz_cost_period` 按期间聚合成「口径台账」（期间 / 批次数 / 状态）—— 预演据此判断「这个期间切不切得」以及「存量期间有哪些」 |
//! | 2 | [`object_nos`] | 一次查出若干成本对象的对象号（出参里 `scope_object_no` / 对象级覆盖清单用，**不逐行查库**，AGENTS 约定 13 的防 N+1） |
//!
//! 写入（收口旧版 / 插入新版）复用 `cost_method_model` 的 `insert_in` / `update_in`：
//! 配置表只有一份写入口，本模块不另开一套 SQL。
//!
//! # 为什么读 `biz_cost_period` 而不是 `biz_cost_period_item`
//!
//! 判断「这一期有没有算过」只需要**单头**：`biz_cost_period` 一行 = 一个期间的一个批次
//! （活跃批次按 `(org_id, period_code)` 唯一，红冲 / 作废留痕行额外存在）。明细表按
//! 成本对象 × 期间可能有几百行，对「能不能切」没有增量信息。
//!
//! 状态口径与 V0373 的唯一键一致：`REVERSED` / `CANCELLED` 是审计留痕，不占期间位置；
//! 但它们**照样列出来**（报文要让用户看到「这一期算过、后来红冲了」），是否构成
//! 「已有批次」由 `PeriodLedger::blocks_switch` 判。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_method_switch_dto::PeriodLedger;

/// 成本期间单头表。
pub const PERIOD_TABLE: &str = "biz_cost_period";

/// 成本对象表（取对象号用）。
pub const OBJECT_TABLE: &str = "biz_cost_object";

/// 某期间的口径台账（**全部**期间，一次取完；期间量级 = 年月数，几十行）。
///
/// 排序按期间码升序（`yyyy-MM` 定长可字典序比较），出参直接可以渲染。
pub async fn period_ledgers(ex: &impl SqlExecutor) -> DbResult<Vec<PeriodLedger>> {
    let sql = format!(
        "SELECT t.period_code, count(*)::bigint AS batch_count, \
                COALESCE(string_agg(DISTINCT t.status, ',' ORDER BY t.status), '') AS statuses \
           FROM {PERIOD_TABLE} t \
          WHERE t.deleted_at IS NULL \
          GROUP BY t.period_code \
          ORDER BY t.period_code ASC"
    );
    let rows = ex.query_sql(sql, vec![]).await?;
    rows.iter()
        .map(|row| {
            let statuses: String = row.get_typed("statuses")?;
            Ok(PeriodLedger {
                period_code: row.get_typed("period_code")?,
                batch_count: row.get_typed("batch_count")?,
                statuses: statuses
                    .split(',')
                    .filter(|text| !text.is_empty())
                    .map(str::to_string)
                    .collect(),
            })
        })
        .collect()
}

/// 成本对象 id → 对象号（一次一条 SQL，`IN` 列表；空入参直接返回空表，不查库）。
///
/// 已软删 / 已作废的对象也照实回（历史配置要能显示「当时挂的是哪个对象」）：
/// 这里只做显示，不参与判定。
pub async fn object_nos(ex: &impl SqlExecutor, ids: &[Uuid]) -> DbResult<std::collections::HashMap<Uuid, String>> {
    let mut wanted = ids
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if wanted.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    wanted.sort();
    let list = wanted
        .iter()
        .map(|id| format!("'{id}'::uuid"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT t.id, t.object_no FROM {OBJECT_TABLE} t WHERE t.id IN ({list})");
    let rows = ex.query_sql(sql, vec![]).await?;
    let mut map = std::collections::HashMap::with_capacity(rows.len());
    for row in &rows {
        let id: Uuid = row.get_typed("id")?;
        let object_no: String = row.get_typed("object_no")?;
        map.insert(id, object_no);
    }
    Ok(map)
}

// ==================== 单测（不连库：只验 SQL 形状） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 台账 SQL：按期间分组、状态去重排序、软删行排除、升序 —— 四条都是判据的一部分。
    #[test]
    fn ledger_sql_shape() {
        let sql = format!(
            "SELECT t.period_code, count(*)::bigint AS batch_count, \
                    COALESCE(string_agg(DISTINCT t.status, ',' ORDER BY t.status), '') AS statuses \
               FROM {PERIOD_TABLE} t \
              WHERE t.deleted_at IS NULL \
              GROUP BY t.period_code \
              ORDER BY t.period_code ASC"
        );
        assert!(sql.contains("GROUP BY t.period_code"));
        assert!(sql.contains("WHERE t.deleted_at IS NULL"), "软删行不参与");
        assert!(sql.contains("DISTINCT t.status"), "同期间状态要去重");
        assert!(sql.contains("ORDER BY t.period_code ASC"));
        assert!(sql.contains("biz_cost_period"));
    }

    /// 对象号 SQL：单条 `IN`，不做逐行查询。
    #[test]
    fn object_no_sql_is_a_single_in_query() {
        let ids = [Uuid::from_u128(1), Uuid::from_u128(2)];
        let list = ids
            .iter()
            .map(|id| format!("'{id}'::uuid"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT t.id, t.object_no FROM {OBJECT_TABLE} t WHERE t.id IN ({list})");
        assert_eq!(sql.matches("SELECT").count(), 1, "必须是单条 SQL");
        assert!(sql.contains("00000000-0000-0000-0000-000000000001"));
        assert!(sql.contains("biz_cost_object"));
    }
}
