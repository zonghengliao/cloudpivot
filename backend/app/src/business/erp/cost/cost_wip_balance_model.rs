//! 在制品「期末在制余额 + 结转核对」的**取数层**（成本核算 · M5-23）。
//!
//! # 取什么、从哪来
//!
//! | 来源 | 取什么 | 干什么 |
//! |---|---|---|
//! | `cost_wip_alloc_model::rows_of`（**复用 M5-22**） | 对象抬头 + 期初（上期已结算）+ 本期三要素归集 + 本期计算单快照 | 余额滚动的全部算数输入（同一口径，不另写 SQL） |
//! | `biz_cost_object` | 对象状态（`OPEN` / `CLOSED`） | 关批仍挂余额 ⇒ `closed_wip`（永远结不掉的死余额） |
//! | `biz_cost_period_item` + `biz_cost_period` | 本期之后**最早**一张计算单的期初两列 | 结转核对（下期接走了多少） |
//! | `biz_cost_period` | 本期活跃批次 / 最近一次红冲批次 | 红冲作废（`reversed`）与「账上到底有没有批次」 |
//!
//! # 为什么复用 M5-22 的取数，而不是再写一条大 SQL
//!
//! 余额 = 同一批数的另一次装配。M5-22 的 SQL 已经把「期初按上期单位成本还原、前期完工累计、
//! 本期归集、本期快照」四件事一次取全（无 N+1），父卡交接也明确要求「消费同一口径，不要另起公式」。
//! 重写一遍只会多一个会漂的副本。
//!
//! # 批量取，不逐行问库
//!
//! 对象状态 / 下一期期初都是 `id = ANY(?::text::uuid[])` 一次取回（参数走
//! [`crate::business::erp::model_base::ids_array_param`]，本栈驱动不支持 `Value::Array`），
//! 行数上限与 M5-22 的 [`cost_wip_alloc_model::ROW_GUARD`] 同档。
//!
//! # 数据范围
//!
//! 行本身由 M5-22 的 SQL 按 `biz_cost_object.workshop_dept_id` 裁剪；本层再问
//! `biz_cost_object` 时**同样挂一遍**锚点（`scope::filter_for(..ALLOC_TARGET)`）——
//! 不是重复劳动：万一以后有人拿未裁剪的 id 列表调进来，这一层不会成为越权口子。
//! 锚点常量直接复用 [`cost_wip_alloc_model::ALLOC_TARGET`]（登记表里那条 `BIZ_COST_OBJECT`）。

use std::collections::HashMap;

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_wip_alloc_dto::{AllocFilter, AllocStateFilter};
use crate::business::erp::cost::cost_wip_alloc_model as alloc_model;
pub use crate::business::erp::cost::cost_wip_alloc_model::SourceRow;
use crate::business::erp::cost::cost_wip_balance_dto::BatchRef;
use crate::business::erp::cost::cost_wip_balance_dto::NextFacts;
use crate::business::erp::model_base::ids_array_param;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::business::shared::scope;

/// 数据权限锚点：**复用** M5-22 / 登记表里的同一条（`biz_cost_object`，别名 `t`）。
pub const BALANCE_TARGET: scope::ScopeTarget = alloc_model::ALLOC_TARGET;

/// 单次取数的行数上限（与 M5-22 同档；`rows_of` 不分页，这里只是把 `AllocFilter` 填成合法值）。
pub const PAGE_SIZE_CAP: u64 = 200;

/// 本期的批次视图（活跃的那条 + 最近一次红冲的那条）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeriodBatches {
    /// 活跃批次（`REVERSED` / `CANCELLED` / 软删不占位 ⇒ 可能为 `None`）。
    pub active: Option<(Uuid, BatchRef)>,
    /// 最近一次红冲批次（有 ⇒ 本表按「已红冲」处理）。
    pub reversed: Option<BatchRef>,
}

impl PeriodBatches {
    /// 活跃批次 id（给 M5-22 的取数当 `ci` 的 join 键；没有批次 ⇒ `Uuid::nil()`）。
    pub fn active_id(&self) -> Uuid {
        self.active.as_ref().map(|(id, _)| *id).unwrap_or_else(Uuid::nil)
    }

    /// 交给结转核对用的批次引用（红冲优先：红冲之后账面余额作废，比「有哪个批次」更要紧）。
    pub fn carry_ref(&self) -> Option<&BatchRef> {
        match (&self.active, &self.reversed) {
            (_, Some(reversed)) => Some(reversed),
            (Some((_, active)), None) => Some(active),
            (None, None) => None,
        }
    }
}

// ==================== 本期批次 ====================

const ACTIVE_BATCH_SQL: &str = "SELECT id, batch_no, status FROM biz_cost_period \
                                 WHERE period_code = ? AND deleted_at IS NULL \
                                   AND status NOT IN ('REVERSED', 'CANCELLED') \
                                 ORDER BY created_at DESC LIMIT 1";

const REVERSED_BATCH_SQL: &str = "SELECT id, batch_no, status, reverse_reason FROM biz_cost_period \
                                  WHERE period_code = ? AND deleted_at IS NULL AND status = 'REVERSED' \
                                  ORDER BY reversed_at DESC NULLS LAST, created_at DESC LIMIT 1";

/// 本期的活跃批次 + 最近一次红冲批次（两条 `LIMIT 1`，无 N+1）。
pub async fn batches_of(period_code: &str) -> DbResult<PeriodBatches> {
    let active_rows = db()
        .query_sql(ACTIVE_BATCH_SQL.to_string(), vec![Value::Text(period_code.to_string())])
        .await?;
    let active = match active_rows.first() {
        None => None,
        Some(row) => Some((
            row.get_typed::<Uuid>("id")?,
            BatchRef {
                id: row.get_typed::<Uuid>("id")?.to_string(),
                batch_no: row.get_typed("batch_no")?,
                status: row.get_typed("status")?,
                reverse_reason: None,
            },
        )),
    };

    let reversed_rows = db()
        .query_sql(
            REVERSED_BATCH_SQL.to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    let reversed = match reversed_rows.first() {
        None => None,
        Some(row) => Some(BatchRef {
            id: row.get_typed::<Uuid>("id")?.to_string(),
            batch_no: row.get_typed("batch_no")?,
            status: row.get_typed("status")?,
            reverse_reason: row.get_typed("reverse_reason")?,
        }),
    };

    Ok(PeriodBatches { active, reversed })
}

// ==================== 余额行（复用 M5-22） ====================

/// 取本期参与余额表的全部成本对象（已按数据范围 / 关键字裁剪，未分页）。
///
/// 直接调 M5-22 的 [`cost_wip_alloc_model::rows_of`]：同一批数、同一套期初口径、同一条数据范围锚点。
/// `state` 只影响筛行、不影响算数，所以这里固定传 `All`（筛选在 handler 里做，合计才是整表口径）。
pub async fn rows_of(
    principal: &ScopedPrincipal,
    period_code: &str,
    keyword: Option<&str>,
    batch_id: Uuid,
) -> DbResult<Vec<SourceRow>> {
    let filter = AllocFilter {
        period_code: period_code.to_string(),
        keyword: keyword.map(str::to_string),
        state: AllocStateFilter::All,
        page: 1,
        page_size: PAGE_SIZE_CAP,
    };
    alloc_model::rows_of(principal, &filter, batch_id).await
}

// ==================== 对象状态 ====================

const OBJECT_STATUS_SQL: &str = "SELECT t.id, t.status FROM biz_cost_object t \
                                 WHERE t.deleted_at IS NULL AND t.id = ANY(?::text::uuid[])";

/// 批量取对象状态（`OPEN` / `CLOSED`）。缺失的对象不出现在 map 里（调用方按 `OPEN` 兜底）。
pub async fn object_status_of(principal: &ScopedPrincipal, ids: &[Uuid]) -> DbResult<HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let scope_filter = scope::filter_for(principal, BALANCE_TARGET);
    let sql = format!("{OBJECT_STATUS_SQL}{}", scope_filter.sql());
    let mut params = vec![ids_array_param(ids)];
    params.extend_from_slice(scope_filter.params());
    bind(&sql, &params)?;

    let rows = db().query_sql(sql, params).await?;
    let mut out = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        out.insert(row.get_typed::<Uuid>("id")?, row.get_typed("status")?);
    }
    Ok(out)
}

// ==================== 下一期（结转核对） ====================

const NEXT_ITEM_SQL: &str = "SELECT DISTINCT ON (i.cost_object_id) \
                                    i.cost_object_id, p.period_code, \
                                    i.wip_beg_qty, i.wip_beg_cost, \
                                    i.status AS item_status, p.status AS period_status \
                               FROM biz_cost_period_item i \
                               JOIN biz_cost_period p ON p.id = i.period_id \
                              WHERE p.deleted_at IS NULL AND p.period_code > ? \
                                AND p.status NOT IN ('REVERSED', 'CANCELLED') \
                                AND i.cost_object_id = ANY(?::text::uuid[]) \
                              ORDER BY i.cost_object_id, p.period_code ASC";

/// 批量取「本期之后**最早**一张计算单」的期初两列（结转核对用）。
///
/// `DISTINCT ON (cost_object_id) ... ORDER BY period_code ASC` = 每个对象取最近的下一个期间；
/// 红冲 / 作废批次不参与（它们的期初不构成「结转到了」的证据）。
pub async fn next_items_of(ids: &[Uuid], period_code: &str) -> DbResult<HashMap<Uuid, NextFacts>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let params = vec![Value::Text(period_code.to_string()), ids_array_param(ids)];
    bind(NEXT_ITEM_SQL, &params)?;

    let rows = db().query_sql(NEXT_ITEM_SQL.to_string(), params).await?;
    let mut out = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        out.insert(
            row.get_typed::<Uuid>("cost_object_id")?,
            NextFacts {
                period_code: row.get_typed("period_code")?,
                wip_beg_qty: row.get_typed("wip_beg_qty")?,
                wip_beg_cost: row.get_typed("wip_beg_cost")?,
                item_status: row.get_typed("item_status")?,
                period_status: row.get_typed("period_status")?,
            },
        );
    }
    Ok(out)
}

// ==================== 占位符一致性 ====================

/// 占位符 / 参数一致性校验（**有牙**：错位立刻 500，绝不绑错值）。
fn bind(sql: &str, params: &[Value]) -> DbResult<()> {
    let placeholders = sql.matches('?').count();
    if placeholders != params.len() {
        return Err(DbError::InvalidValue(format!(
            "在制品余额表 SQL 占位符与参数数量不一致（?={placeholders}，参数={}）：参数错位会静默绑错值，\
             拒绝执行。SQL={sql}",
            params.len()
        )));
    }
    Ok(())
}

// ==================== 单测（不连库：SQL 形状与参数一致性） ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_and_reversed_batch_take_one_period_parameter_each() {
        assert_eq!(ACTIVE_BATCH_SQL.matches('?').count(), 1);
        assert_eq!(REVERSED_BATCH_SQL.matches('?').count(), 1);
        assert!(bind(ACTIVE_BATCH_SQL, &[Value::Text("2026-11".to_string())]).is_ok());
        assert!(bind(REVERSED_BATCH_SQL, &[Value::Text("2026-11".to_string())]).is_ok());
    }

    #[test]
    fn active_batch_excludes_reversed_and_cancelled() {
        // 与 `uk_biz_cost_period` 同口径：红冲 / 作废行不占位
        assert!(ACTIVE_BATCH_SQL.contains("status NOT IN ('REVERSED', 'CANCELLED')"));
        assert!(ACTIVE_BATCH_SQL.contains("deleted_at IS NULL"));
        assert!(REVERSED_BATCH_SQL.contains("status = 'REVERSED'"));
    }

    #[test]
    fn next_item_sql_takes_the_earliest_later_period() {
        // 结转只看「下一个期间」：ORDER BY period_code ASC + DISTINCT ON 取第一条
        assert!(NEXT_ITEM_SQL.contains("DISTINCT ON (i.cost_object_id)"));
        assert!(NEXT_ITEM_SQL.contains("ORDER BY i.cost_object_id, p.period_code ASC"));
        assert!(NEXT_ITEM_SQL.contains("p.period_code > ?"));
        assert!(
            NEXT_ITEM_SQL.contains("p.status NOT IN ('REVERSED', 'CANCELLED')"),
            "红冲批次的期初不构成「结转到了」的证据"
        );
        assert_eq!(NEXT_ITEM_SQL.matches('?').count(), 2, "期间 + id 数组");
    }

    #[test]
    fn object_status_sql_uses_the_scope_alias() {
        // 数据范围片段写死 `t.workshop_dept_id`：换别名只有非超管会 500
        assert!(OBJECT_STATUS_SQL.contains("FROM biz_cost_object t"));
        assert!(OBJECT_STATUS_SQL.contains("t.id = ANY(?::text::uuid[])"));
        assert!(!OBJECT_STATUS_SQL.contains("biz_cost_object o"));
    }

    #[test]
    fn object_status_sql_has_exactly_one_placeholder() {
        assert_eq!(OBJECT_STATUS_SQL.matches('?').count(), 1, "只有 id 数组");
    }

    #[test]
    fn bind_rejects_misaligned_parameters() {
        let error =
            bind("SELECT 1 WHERE a = ? AND b = ?", &[Value::Text("x".to_string())]).expect_err("少一个参数必须失败");
        assert!(format!("{error:?}").contains("占位符与参数数量不一致"));
    }

    #[test]
    fn bind_accepts_aligned_parameters() {
        bind("SELECT 1 WHERE a = ?", &[Value::Text("x".to_string())]).expect("对齐即通过");
    }

    #[test]
    fn batches_carry_ref_prefers_the_reversed_row() {
        let active = BatchRef {
            id: "batch-active".to_string(),
            batch_no: "CS-1".to_string(),
            status: "SETTLED".to_string(),
            reverse_reason: None,
        };
        let reversed = BatchRef {
            id: "batch-reversed".to_string(),
            batch_no: "CS-2".to_string(),
            status: "REVERSED".to_string(),
            reverse_reason: Some("红冲".to_string()),
        };
        let none = PeriodBatches::default();
        assert!(none.active_id().is_nil(), "没批次 ⇒ nil");
        assert!(none.carry_ref().is_none());

        let only_active = PeriodBatches {
            active: Some((Uuid::now_v7(), active.clone())),
            reversed: None,
        };
        assert_eq!(
            only_active.carry_ref().map(|item| item.status.as_str()),
            Some("SETTLED")
        );

        let both = PeriodBatches {
            active: Some((Uuid::now_v7(), active)),
            reversed: Some(reversed),
        };
        assert_eq!(
            both.carry_ref().map(|item| item.status.as_str()),
            Some("REVERSED"),
            "红冲优先：账面余额已作废"
        );
    }
}
