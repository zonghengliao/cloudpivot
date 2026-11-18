//! 分批法「料工费归集到批次」· 取数层（成本核算 · M5-03）。
//!
//! 本文件只做**读原语**与两个小写入原语的复用，不含业务判据（判据在
//! [`crate::business::erp::cost::cost_batch_collect_dto`]，编排在
//! [`crate::business::erp::cost::cost_batch_collect`]）。
//!
//! # 为什么不重写三路扫描 SQL
//!
//! 料 / 工 / 费三路的候补行与已归集行，全部直接调用 calc 各段已经在用的函数
//! （`cost_collect_model::material_source_lines` / `labor_source_lines` /
//! `overhead_source_lines` / `*_collect_rows_of_period`），**本文件不抄一行 SQL**：
//!
//! * 抄一份 = 两份口径各自演进。今天「只扫已过账领料」是对的，明天有人修 calc
//!   那边（比如加「退货冲回不算」），分批归集就会静默按旧口径干；
//! * 三路扫描都是**期间级**的（那是 calc 的口径），本卡要的是「期间 ∩ 本批」——
//!   这段收窄是**纯内存过滤**（`retain`），因为期间级扫描本来就要把行读进内存做
//!   幂等取差，多一次 `WHERE cost_object_id = ?` 只省几百行、却多一份会漂的 SQL。
//! * 参数顺序 / 占位符个数这些坑全部留在原来那几处（已经被真库用例钉住）。
//!
//! # 本文件真正新增的两条读
//!
//! 1. **本期活跃成本批次**（闸门用）：`biz_cost_period` 里排除 `CANCELLED/REVERSED`
//!    的那一行。期间唯一键（V0373 的 `uk_biz_cost_period`）保证至多一行，`LIMIT 2`
//!    只为在索引/约束被人改坏时**立刻暴露**（不是静默取一行）。
//! 2. **本批本期三要素已有归集额**（出参的「已归集」与写后回查）：
//!    一条 `FILTER` 聚合拿三项，避免三次往返。
//!
//! 其余（批次抬头 + 数据范围探针）直接复用 M5-02 的 `find_visible_batch` ——
//! 同一个 404 口径（不可见即 404，不是 403），不给「用 id 探测别的车间」留缝。

use std::collections::HashMap;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_batch_collect_dto::{
    ELEMENT_LABOR, ELEMENT_MATERIAL, ELEMENT_OVERHEAD, PeriodBatchRef,
};
use crate::business::erp::cost::cost_collect_sheet_dto::SheetBatchRes;
use crate::business::erp::cost::cost_collect_sheet_model;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::business::shared::scope;

/// 数据权限锚点：与 M5-02 归集表**同一个**登记项（`shared/scope.rs` 的 `BIZ_COST_OBJECT`）。
///
/// 别 `ScopeTarget::new(...)` 抄一份：抄本让登记表与代码各自演进，且
/// `permission::scope_model::tests::d8c_wiring_anchors_exist_in_source` 是按
/// 「登记表锚点常量名 + 同文件 scope 调用」校验接线的，抄本扫不到。
pub const OBJECT_TARGET: scope::ScopeTarget = scope::ScopeTarget::BIZ_COST_OBJECT;

/// 三要素的已有归集（行数 + 金额）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ElementTotals {
    /// 料行数。
    pub material_rows: i64,
    /// 料金额。
    pub material_amount: Decimal,
    /// 工行数。
    pub labor_rows: i64,
    /// 工金额。
    pub labor_amount: Decimal,
    /// 费行数。
    pub overhead_rows: i64,
    /// 费金额。
    pub overhead_amount: Decimal,
}

impl ElementTotals {
    /// 三项金额合计。
    pub fn total(&self) -> Decimal {
        self.material_amount + self.labor_amount + self.overhead_amount
    }
}

// ==================== 本期活跃成本批次（闸门） ====================

/// 本期**活跃**成本批次（排除 `CANCELLED` / `REVERSED` / 已软删）。
///
/// 返回 `Err(DbError::InvalidValue)`（→ 500）而不是随便取一行：期间唯一键被改坏时
/// 必须炸出来 —— 「这一期到底哪个批次说了算」没有第二个答案可选。
pub async fn active_period_batch(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Option<PeriodBatchRef>> {
    let sql = "SELECT batch_no, status \
                 FROM biz_cost_period \
                WHERE period_code = ? \
                  AND deleted_at IS NULL \
                  AND status NOT IN ('CANCELLED','REVERSED') \
                ORDER BY created_at DESC, id DESC \
                LIMIT 2";
    let rows = ex
        .query_sql(sql.to_string(), vec![Value::Text(period_code.to_string())])
        .await?;
    if rows.len() > 1 {
        return Err(DbError::InvalidValue(format!(
            "期间 {period_code} 存在 {} 个活跃成本批次：期间唯一键 uk_biz_cost_period 被破坏，拒绝猜测「哪个批次说了算」",
            rows.len()
        )));
    }
    match rows.first() {
        Some(row) => Ok(Some(PeriodBatchRef {
            batch_no: row.get_typed("batch_no")?,
            status: row.get_typed("status")?,
        })),
        None => Ok(None),
    }
}

/// 事务内再确认一次批次状态（防「校验后、写库前」被人关批/作废）。
pub async fn object_status_in_tx(ex: &impl SqlExecutor, object_id: Uuid) -> DbResult<Option<String>> {
    let rows = ex
        .query_sql(
            "SELECT status FROM biz_cost_object WHERE id = ?::uuid AND deleted_at IS NULL FOR SHARE".to_string(),
            vec![Value::from(&object_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(row.get_typed("status")?)),
        None => Ok(None),
    }
}

// ==================== 本批本期已有归集额 ====================

/// 某批次在某期间已归集的料 / 工 / 费（行数 + 金额）。
///
/// 一条 `FILTER` 聚合取三项：**不按要素分别查三次**，也不在内存里累加 ——
/// 出参里「已归集」这一列必须与 M5-02 归集表同一行为的数一致。
pub async fn element_totals(ex: &impl SqlExecutor, object_id: Uuid, period_code: &str) -> DbResult<ElementTotals> {
    let sql = "SELECT count(*) FILTER (WHERE element = ?) AS material_rows, \
                      COALESCE(sum(amount) FILTER (WHERE element = ?), 0) AS material_amount, \
                      count(*) FILTER (WHERE element = ?) AS labor_rows, \
                      COALESCE(sum(amount) FILTER (WHERE element = ?), 0) AS labor_amount, \
                      count(*) FILTER (WHERE element = ?) AS overhead_rows, \
                      COALESCE(sum(amount) FILTER (WHERE element = ?), 0) AS overhead_amount \
                 FROM biz_cost_collect \
                WHERE cost_object_id = ?::uuid AND period_code = ? AND deleted_at IS NULL";
    let params = vec![
        Value::Text(ELEMENT_MATERIAL.to_string()),
        Value::Text(ELEMENT_MATERIAL.to_string()),
        Value::Text(ELEMENT_LABOR.to_string()),
        Value::Text(ELEMENT_LABOR.to_string()),
        Value::Text(ELEMENT_OVERHEAD.to_string()),
        Value::Text(ELEMENT_OVERHEAD.to_string()),
        Value::from(&object_id),
        Value::Text(period_code.to_string()),
    ];
    let row = ex
        .query_first_sql(sql.to_string(), params)
        .await?
        .ok_or_else(|| DbError::InvalidValue("三要素归集额聚合查询没有返回行（聚合必有一行）".to_string()))?;
    Ok(ElementTotals {
        material_rows: row.get_typed("material_rows")?,
        material_amount: row.get_typed("material_amount")?,
        labor_rows: row.get_typed("labor_rows")?,
        labor_amount: row.get_typed("labor_amount")?,
        overhead_rows: row.get_typed("overhead_rows")?,
        overhead_amount: row.get_typed("overhead_amount")?,
    })
}

// ==================== 物料名（批量补，防 N+1） ====================

/// 一批物料 id → `(编码, 名称)`。空入参直接返回空表（不打库）。
///
/// 为什么不用 `= ANY(?::uuid[])`：仓库现有的绑参原语没有数组形态（`Value` 只到
/// Text/I64/…），而这里的 id 全部来自 `biz_stock_out_item.material_id`
/// （库里的 uuid 值，不是外部输入），拼 `IN ('…')` 没有注入面。
pub async fn material_labels(
    ex: &impl SqlExecutor,
    ids: &[Uuid],
) -> DbResult<HashMap<Uuid, (Option<String>, Option<String>)>> {
    let mut unique: Vec<Uuid> = ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    if unique.is_empty() {
        return Ok(HashMap::new());
    }
    let list = unique
        .iter()
        .map(|id| format!("'{id}'::uuid"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id, material_code, material_name FROM biz_material WHERE id IN ({list}) AND deleted_at IS NULL"
    );
    let rows = ex.query_sql(sql, Vec::new()).await?;
    let mut labels = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        let id: Uuid = row.get_typed("id")?;
        let code: Option<String> = row.get_typed("material_code")?;
        let name: Option<String> = row.get_typed("material_name")?;
        labels.insert(id, (code, name));
    }
    Ok(labels)
}

// ==================== 批次抬头（复用 M5-02 的探针） ====================

/// 在**当前用户数据范围**内按 id 取批次抬头；不存在 / 已删 / 不可见 ⇒ `None`（调用方转 404）。
///
/// 直接复用 M5-02 的实现：同一份范围过滤、同一个 `left join` 补名、同一条 404 口径。
pub async fn find_visible_batch(principal: &ScopedPrincipal, id: Uuid) -> DbResult<Option<SheetBatchRes>> {
    Ok(cost_collect_sheet_model::find_visible_batch(principal, id)
        .await?
        .map(|batch| batch.to_res()))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_target_is_the_registered_cost_object_anchor() {
        // 与 shared/scope.rs 的 `SCOPED_TABLES` 登记项同一个对象（不是抄本）
        assert_eq!(OBJECT_TARGET.table, cost_collect_sheet_model::SHEET_TARGET.table);
        assert_eq!(OBJECT_TARGET.alias, cost_collect_sheet_model::SHEET_TARGET.alias);
        assert_eq!(OBJECT_TARGET.dept_col, cost_collect_sheet_model::SHEET_TARGET.dept_col);
    }

    #[test]
    fn element_totals_sums_three_elements() {
        let totals = ElementTotals {
            material_rows: 2,
            material_amount: Decimal::new(1500000, 4),
            labor_rows: 1,
            labor_amount: Decimal::new(300000, 4),
            overhead_rows: 3,
            overhead_amount: Decimal::new(200000, 4),
        };
        assert_eq!(totals.total(), Decimal::new(2000000, 4));
    }
}
