//! 成本计算单数据层（`biz_cost_period_item` 及其取数）—— calc 第 6 段
//! （ARCH-9 §3.1 ⑩ / §4.6 · B2-11）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0373__cost_period_tables.up.sql`
//! （与设计 §3.1 ⑩ 逐字一致，已应用）。**本卡不新增迁移**：「迁移幂等 + `.down.sql`」N/A，
//! 也不碰任何已应用迁移（`git status backend/migrations` 必须 0 行）。
//!
//! # 重算口径（设计 §4.6.3 末句 + §3.1 ⑩ 注释）
//!
//! ```text
//! DELETE FROM biz_cost_period_item WHERE period_id = ? AND status = 'CALC'
//! ```
//!
//! 只删 `CALC`：`SETTLED` 行是已结算入库与结转凭证的数字来源，删了就是改写历史
//! （闸在 [`crate::business::erp::cost_calc_item`]，这里只做「按状态删」的机械动作）。
//!
//! # 取数的两条硬口径
//!
//! 1. **期初在制只认「已结算」批次**（`SETTLED` / `CLOSED`）：`SUBMITTED` / `APPROVED` 的
//!    数字还可能被驳回重算，拿它做期初等于把没定稿的数当事实。
//! 2. **前期一律用 `period_code <` 比较**（`yyyy-MM` 定长字典序 = 时间序），
//!    不用时间戳：期间归属由 `period_code` 决定，`created_at` 只说明「什么时候算的」。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 计算单表名。
pub const TABLE: &str = "biz_cost_period_item";

/// 本模块读的列（与 V0373 ⑩ 逐列对应；`period_id` 单独取，回查时按它过滤）。
pub const COLUMNS: &str = "cost_object_id, product_id, wip_beg_qty, wip_beg_cost, completed_qty, \
     wip_end_qty, completion_pct, equiv_material, equiv_conversion, mat_cur, labor_cur, oh_cur, \
     unit_material, unit_labor, unit_overhead, finished_cost, wip_end_cost, unit_finished_cost, status";

/// 算过账、可以当期初的批次状态（设计 §4.6.3「上一个已结算批次」）。
pub const SETTLED_STATUSES: [&str; 2] = ["SETTLED", "CLOSED"];

/// 允许重算计算单的批次状态（与第 5 段同口径：`CANCELLED` / `REVERSED` 不占位，能重建）。
pub const REDOABLE_STATUSES: [&str; 3] = ["DRAFT", "CANCELLED", "REVERSED"];

// ==================== 行结构 ====================

/// 当前活跃批次（`uk_biz_cost_period` 排除了 `REVERSED` / `CANCELLED` 与软删）。
#[derive(Debug, Clone, PartialEq)]
pub struct BatchRow {
    /// 批次主键（计算单的 `period_id`）。
    pub id: Uuid,
    /// 批次号（报文点名用）。
    pub batch_no: String,
    /// 状态。
    pub status: String,
}

impl BatchRow {
    /// 是否允许重算计算单（见 [`REDOABLE_STATUSES`]）。
    pub fn redoable(&self) -> bool {
        REDOABLE_STATUSES.contains(&self.status.as_str())
    }
}

/// 一个成本对象本期的取数结果（对象口径 + 当期三要素投入）。
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectCostRow {
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 对象号。
    pub object_no: String,
    /// 产出品 id（计算单冗余快照）。
    pub product_id: Uuid,
    /// 对象状态（`OPEN` / `CLOSED`）。
    pub status: String,
    /// 投料方式（一期只支持 `ONE_TIME`）。
    pub material_input_mode: String,
    /// **累计**完工数量（`biz_cost_object.completed_qty`）。
    pub cumulative_completed_qty: Decimal,
    /// 期末在制数量（`biz_cost_object.wip_qty`）。
    pub wip_qty: Decimal,
    /// 期末在制完工进度 %。
    pub completion_pct: Decimal,
    /// 本期材料投入（Σ collect `element='MATERIAL'`）。
    pub mat_cur: Decimal,
    /// 本期人工投入（Σ collect `element='LABOR'`）。
    pub labor_cur: Decimal,
    /// 本期制造费用投入（Σ collect `element='OVERHEAD'`，含分摊转入的 `ALLOC` 行）。
    pub oh_cur: Decimal,
}

/// 上期已结算计算单里可以当作期初的那几列（设计 §4.6.3）。
#[derive(Debug, Clone, PartialEq)]
pub struct PriorItemRow {
    /// 上期期间。
    pub period_code: String,
    /// 上期期末在制数量。
    pub wip_end_qty: Decimal,
    /// 上期期末在制成本。
    pub wip_end_cost: Decimal,
    /// 上期期末在制完工进度 %。
    pub completion_pct: Decimal,
    /// 上期单位材料成本。
    pub unit_material: Decimal,
    /// 上期单位人工成本。
    pub unit_labor: Decimal,
    /// 上期单位制造费用。
    pub unit_overhead: Decimal,
}

/// `biz_cost_period_item` 一行（写入与回查共用）。
#[derive(Debug, Clone, PartialEq)]
pub struct ItemRow {
    /// 主键。
    pub id: Uuid,
    /// 所属批次。
    pub period_id: Uuid,
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 产出品（冗余快照）。
    pub product_id: Uuid,
    /// 期初在制数量。
    pub wip_beg_qty: Decimal,
    /// 期初在制成本。
    pub wip_beg_cost: Decimal,
    /// 本期完工数量。
    pub completed_qty: Decimal,
    /// 期末在制数量。
    pub wip_end_qty: Decimal,
    /// 期末在制完工进度 %。
    pub completion_pct: Decimal,
    /// 约当产量（材料）。
    pub equiv_material: Decimal,
    /// 约当产量（转换成本）。
    pub equiv_conversion: Decimal,
    /// 本期材料投入。
    pub mat_cur: Decimal,
    /// 本期人工投入。
    pub labor_cur: Decimal,
    /// 本期制造费用投入。
    pub oh_cur: Decimal,
    /// 单位材料成本。
    pub unit_material: Decimal,
    /// 单位人工成本。
    pub unit_labor: Decimal,
    /// 单位制造费用。
    pub unit_overhead: Decimal,
    /// 完工转出成本。
    pub finished_cost: Decimal,
    /// 期末在制成本。
    pub wip_end_cost: Decimal,
    /// 完工单位成本。
    pub unit_finished_cost: Decimal,
    /// 行状态（`CALC` / `SETTLED`）。
    pub status: String,
}

impl ItemRow {
    /// 行 → 结构（列名与 [`COLUMNS`] 对齐）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            period_id: row.get_typed("period_id")?,
            cost_object_id: row.get_typed("cost_object_id")?,
            product_id: row.get_typed("product_id")?,
            wip_beg_qty: row.get_typed("wip_beg_qty")?,
            wip_beg_cost: row.get_typed("wip_beg_cost")?,
            completed_qty: row.get_typed("completed_qty")?,
            wip_end_qty: row.get_typed("wip_end_qty")?,
            completion_pct: row.get_typed("completion_pct")?,
            equiv_material: row.get_typed("equiv_material")?,
            equiv_conversion: row.get_typed("equiv_conversion")?,
            mat_cur: row.get_typed("mat_cur")?,
            labor_cur: row.get_typed("labor_cur")?,
            oh_cur: row.get_typed("oh_cur")?,
            unit_material: row.get_typed("unit_material")?,
            unit_labor: row.get_typed("unit_labor")?,
            unit_overhead: row.get_typed("unit_overhead")?,
            finished_cost: row.get_typed("finished_cost")?,
            wip_end_cost: row.get_typed("wip_end_cost")?,
            unit_finished_cost: row.get_typed("unit_finished_cost")?,
            status: row.get_typed("status")?,
        })
    }

    /// 从库里读回来的守恒式左边（完工 + 在制）。
    pub fn outflow_total(&self) -> Decimal {
        self.finished_cost + self.wip_end_cost
    }

    /// 从库里读回来的守恒式右边（期初 + 投入）。
    pub fn inflow_total(&self) -> Decimal {
        self.wip_beg_cost + self.mat_cur + self.labor_cur + self.oh_cur
    }
}

// ==================== 读 ====================

/// 该期间**活跃**批次（不含 `REVERSED` / `CANCELLED` / 软删）；没有则 `None`。
pub async fn active_batch(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Option<BatchRow>> {
    let rows = ex
        .query_sql(
            "SELECT id, batch_no, status FROM biz_cost_period \
              WHERE period_code = ? AND deleted_at IS NULL \
                AND status NOT IN ('REVERSED','CANCELLED') \
              ORDER BY created_at DESC, id DESC LIMIT 1"
                .to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(BatchRow {
            id: row.get_typed("id")?,
            batch_no: row.get_typed("batch_no")?,
            status: row.get_typed("status")?,
        })),
    }
}

/// 本期要点计算单的成本对象 = 「当期有归集行」∪「上期已结算还有期末在制」。
///
/// 后一类不能漏：跨月对象的期初在制（上期 `wip_end_qty > 0`）必须结转到本期，
/// 哪怕本期一笔新投入都没有（全完工的那一期就是这种形态）。
///
/// 两类都只要 `OPEN` / `CLOSED` 的对象（设计 §4.4.3 的参与判据；`CANCELLED` 是作废批）。
pub async fn objects_of_period(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<ObjectCostRow>> {
    // ① 当期有归集行的对象（料 / 工 / 费任一 element，含分摊转入的 ALLOC 行）
    let with_cost = ex
        .query_sql(
            "SELECT o.id AS cost_object_id, o.object_no, o.product_id, o.status, \
                    o.material_input_mode, o.completed_qty AS cumulative_completed_qty, \
                    o.wip_qty, o.completion_pct, \
                    COALESCE(sum(cc.amount) FILTER (WHERE cc.element = 'MATERIAL'), 0) AS mat_cur, \
                    COALESCE(sum(cc.amount) FILTER (WHERE cc.element = 'LABOR'), 0) AS labor_cur, \
                    COALESCE(sum(cc.amount) FILTER (WHERE cc.element = 'OVERHEAD'), 0) AS oh_cur \
               FROM biz_cost_object o \
               JOIN biz_cost_collect cc \
                 ON cc.cost_object_id = o.id AND cc.period_code = ? AND cc.deleted_at IS NULL \
              WHERE o.deleted_at IS NULL AND o.status IN ('OPEN','CLOSED') \
              GROUP BY o.id, o.object_no, o.product_id, o.status, o.material_input_mode, \
                       o.completed_qty, o.wip_qty, o.completion_pct \
              ORDER BY o.object_no"
                .to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;

    // ② 当期没归集、但上期已结算还挂着期末在制的对象（期初在制要结转，投入按 0 算）
    let with_wip = ex
        .query_sql(
            "SELECT o.id AS cost_object_id, o.object_no, o.product_id, o.status, \
                    o.material_input_mode, o.completed_qty AS cumulative_completed_qty, \
                    o.wip_qty, o.completion_pct \
               FROM biz_cost_object o \
              WHERE o.deleted_at IS NULL AND o.status IN ('OPEN','CLOSED') \
                AND NOT EXISTS (SELECT 1 FROM biz_cost_collect cc \
                                 WHERE cc.cost_object_id = o.id AND cc.period_code = ? \
                                   AND cc.deleted_at IS NULL) \
                AND EXISTS (SELECT 1 FROM biz_cost_period_item i \
                              JOIN biz_cost_period p ON p.id = i.period_id \
                             WHERE i.cost_object_id = o.id AND p.period_code < ? \
                               AND p.deleted_at IS NULL AND p.status IN ('SETTLED','CLOSED') \
                               AND i.wip_end_qty > 0) \
              ORDER BY o.object_no"
                .to_string(),
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(period_code.to_string()),
            ],
        )
        .await?;

    let mut objects: Vec<ObjectCostRow> = with_cost
        .iter()
        .map(|row| {
            Ok(ObjectCostRow {
                cost_object_id: row.get_typed("cost_object_id")?,
                object_no: row.get_typed("object_no")?,
                product_id: row.get_typed("product_id")?,
                status: row.get_typed("status")?,
                material_input_mode: row.get_typed("material_input_mode")?,
                cumulative_completed_qty: row.get_typed("cumulative_completed_qty")?,
                wip_qty: row.get_typed("wip_qty")?,
                completion_pct: row.get_typed("completion_pct")?,
                mat_cur: row.get_typed("mat_cur")?,
                labor_cur: row.get_typed("labor_cur")?,
                oh_cur: row.get_typed("oh_cur")?,
            })
        })
        .collect::<DbResult<Vec<_>>>()?;

    for row in &with_wip {
        let cost_object_id: Uuid = row.get_typed("cost_object_id")?;
        if objects.iter().any(|item| item.cost_object_id == cost_object_id) {
            // 理论上不会重叠（② 的条件是「当期没有归集行」）；防御性跳过更安全
            continue;
        }
        objects.push(ObjectCostRow {
            cost_object_id,
            object_no: row.get_typed("object_no")?,
            product_id: row.get_typed("product_id")?,
            status: row.get_typed("status")?,
            material_input_mode: row.get_typed("material_input_mode")?,
            cumulative_completed_qty: row.get_typed("cumulative_completed_qty")?,
            wip_qty: row.get_typed("wip_qty")?,
            completion_pct: row.get_typed("completion_pct")?,
            mat_cur: Decimal::ZERO,
            labor_cur: Decimal::ZERO,
            oh_cur: Decimal::ZERO,
        });
    }
    objects.sort_by(|left, right| left.object_no.cmp(&right.object_no));
    Ok(objects)
}

/// 上期**已结算**计算单（同一对象、`period_code < 当期`，取最近一期）。
pub async fn prior_settled_item(
    ex: &impl SqlExecutor,
    cost_object_id: Uuid,
    period_code: &str,
) -> DbResult<Option<PriorItemRow>> {
    let rows = ex
        .query_sql(
            "SELECT p.period_code, i.wip_end_qty, i.wip_end_cost, i.completion_pct, \
                    i.unit_material, i.unit_labor, i.unit_overhead \
               FROM biz_cost_period_item i \
               JOIN biz_cost_period p ON p.id = i.period_id \
              WHERE i.cost_object_id = ?::uuid AND p.deleted_at IS NULL \
                AND p.status IN ('SETTLED','CLOSED') AND p.period_code < ? \
              ORDER BY p.period_code DESC LIMIT 1"
                .to_string(),
            vec![Value::from(&cost_object_id), Value::Text(period_code.to_string())],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(PriorItemRow {
            period_code: row.get_typed("period_code")?,
            wip_end_qty: row.get_typed("wip_end_qty")?,
            wip_end_cost: row.get_typed("wip_end_cost")?,
            completion_pct: row.get_typed("completion_pct")?,
            unit_material: row.get_typed("unit_material")?,
            unit_labor: row.get_typed("unit_labor")?,
            unit_overhead: row.get_typed("unit_overhead")?,
        })),
    }
}

/// 前期**已结算**计算单的完工数量合计（本期完工 = 累计完工 − 它）。
pub async fn prior_completed_qty(ex: &impl SqlExecutor, cost_object_id: Uuid, period_code: &str) -> DbResult<Decimal> {
    ex.scalar_sql(
        "SELECT COALESCE(sum(i.completed_qty), 0)::numeric AS qty \
               FROM biz_cost_period_item i \
               JOIN biz_cost_period p ON p.id = i.period_id \
              WHERE i.cost_object_id = ?::uuid AND p.deleted_at IS NULL \
                AND p.status IN ('SETTLED','CLOSED') AND p.period_code < ?"
            .to_string(),
        vec![Value::from(&cost_object_id), Value::Text(period_code.to_string())],
    )
    .await
}

/// 该对象**被红冲过的前期批次**（设计 §4.6.3：上期被红冲则本期不得自动取数）。
pub async fn reversed_prior_periods(
    ex: &impl SqlExecutor,
    cost_object_id: Uuid,
    period_code: &str,
) -> DbResult<Vec<String>> {
    let rows = ex
        .query_sql(
            "SELECT DISTINCT p.period_code \
               FROM biz_cost_period_item i \
               JOIN biz_cost_period p ON p.id = i.period_id \
              WHERE i.cost_object_id = ?::uuid AND p.deleted_at IS NULL \
                AND p.status = 'REVERSED' AND p.period_code < ? \
              ORDER BY p.period_code"
                .to_string(),
            vec![Value::from(&cost_object_id), Value::Text(period_code.to_string())],
        )
        .await?;
    rows.iter().map(|row| row.get_typed("period_code")).collect()
}

/// 该批次里已结算（`SETTLED`）的计算单行数 —— 大于 0 就不许重算。
pub async fn settled_count(ex: &impl SqlExecutor, period_id: Uuid) -> DbResult<i64> {
    ex.scalar_sql(
        "SELECT count(*)::bigint FROM biz_cost_period_item \
          WHERE period_id = ?::uuid AND status = 'SETTLED'"
            .to_string(),
        vec![Value::from(&period_id)],
    )
    .await
}

/// 该批次的全部计算单（回查用；按期初在制数量稳定排序不必要，这里按对象号无关的 id 排）。
pub async fn items_of_period(ex: &impl SqlExecutor, period_id: Uuid) -> DbResult<Vec<ItemRow>> {
    let sql = format!(
        "SELECT id, period_id, {COLUMNS} FROM {TABLE} WHERE period_id = ?::uuid \
         ORDER BY cost_object_id"
    );
    let rows = ex.query_sql(sql, vec![Value::from(&period_id)]).await?;
    rows.iter().map(ItemRow::from_row).collect()
}

// ==================== 写 ====================

/// 清掉本批次的 `CALC` 计算单（整删重灌，设计 §3.1 ⑩ 注释）。
pub async fn purge_calc_rows(ex: &impl SqlExecutor, period_id: Uuid) -> DbResult<u64> {
    ex.exec_sql(
        format!("DELETE FROM {TABLE} WHERE period_id = ?::uuid AND status = 'CALC'"),
        vec![Value::from(&period_id)],
    )
    .await
}

/// 写一行计算单。
pub async fn insert_item(ex: &impl SqlExecutor, row: &ItemRow) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} (id, period_id, cost_object_id, product_id, wip_beg_qty, \
                                  wip_beg_cost, completed_qty, wip_end_qty, completion_pct, \
                                  equiv_material, equiv_conversion, mat_cur, labor_cur, oh_cur, \
                                  unit_material, unit_labor, unit_overhead, finished_cost, \
                                  wip_end_cost, unit_finished_cost, status, created_at, updated_at) \
             VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::text::numeric, ?::text::numeric, \
                     ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                     ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                     ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                     ?::text::numeric, ?::text::numeric, ?, now(), now())"
        ),
        vec![
            Value::from(&row.id),
            Value::from(&row.period_id),
            Value::from(&row.cost_object_id),
            Value::from(&row.product_id),
            Value::Text(row.wip_beg_qty.to_string()),
            Value::Text(row.wip_beg_cost.to_string()),
            Value::Text(row.completed_qty.to_string()),
            Value::Text(row.wip_end_qty.to_string()),
            Value::Text(row.completion_pct.to_string()),
            Value::Text(row.equiv_material.to_string()),
            Value::Text(row.equiv_conversion.to_string()),
            Value::Text(row.mat_cur.to_string()),
            Value::Text(row.labor_cur.to_string()),
            Value::Text(row.oh_cur.to_string()),
            Value::Text(row.unit_material.to_string()),
            Value::Text(row.unit_labor.to_string()),
            Value::Text(row.unit_overhead.to_string()),
            Value::Text(row.finished_cost.to_string()),
            Value::Text(row.wip_end_cost.to_string()),
            Value::Text(row.unit_finished_cost.to_string()),
            Value::Text(row.status.clone()),
        ],
    )
    .await
}

// ==================== 单测（不需要数据库：钉口径常量与列形状） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 期初只认已结算：`SUBMITTED` / `APPROVED` 不算（数字还可能被驳回重算）。
    #[test]
    fn only_settled_periods_can_be_beginning_wip() {
        assert_eq!(SETTLED_STATUSES, ["SETTLED", "CLOSED"]);
        for draft in ["DRAFT", "SUBMITTED", "APPROVED", "CANCELLED", "REVERSED"] {
            assert!(!SETTLED_STATUSES.contains(&draft), "{draft} 不该被当成期初在制的来源");
        }
    }

    /// 重算闸：与第 5 段同口径（`CANCELLED` / `REVERSED` 不占位，能重算）。
    #[test]
    fn redoable_statuses_match_the_alloc_segment() {
        let state = |status: &str| BatchRow {
            id: Uuid::nil(),
            batch_no: "CS-202611-0001".to_string(),
            status: status.to_string(),
        };
        for allowed in ["DRAFT", "CANCELLED", "REVERSED"] {
            assert!(state(allowed).redoable(), "{allowed} 必须允许重算");
        }
        for blocked in ["SUBMITTED", "APPROVED", "SETTLED", "CLOSED", ""] {
            assert!(!state(blocked).redoable(), "{blocked} 必须拦住重算");
        }
    }

    /// 读列必须覆盖每一个金额 / 数量字段（漏一列 = 回查永远对不上）。
    #[test]
    fn columns_cover_every_amount_field() {
        for column in [
            "wip_beg_qty",
            "wip_beg_cost",
            "completed_qty",
            "wip_end_qty",
            "completion_pct",
            "equiv_material",
            "equiv_conversion",
            "mat_cur",
            "labor_cur",
            "oh_cur",
            "unit_material",
            "unit_labor",
            "unit_overhead",
            "finished_cost",
            "wip_end_cost",
            "unit_finished_cost",
            "status",
        ] {
            assert!(COLUMNS.contains(column), "COLUMNS 缺 {column}");
        }
    }

    /// 表名与库层 DDL 一致（V0373 ⑩），别写成 `biz_cost_period_items` 之类的复数。
    #[test]
    fn table_name_matches_the_migration() {
        assert_eq!(TABLE, "biz_cost_period_item");
    }
}
