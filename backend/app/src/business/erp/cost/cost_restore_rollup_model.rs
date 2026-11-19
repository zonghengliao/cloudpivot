//! 成本还原「逐层还原到料工费」的**取数层**（成本核算 · M5-27）。
//!
//! # 只补一处取数，其余全复用 M5-26
//!
//! 逐层结构（材料投入 / 谁是自制半成品 / 产出批次 / 完工成本结构）是 M5-26 的
//! `cost_restore_structure_model` 已经解决的问题，本卡**一行 SQL 都不抄**：
//! 行、树、结构比例全部由 `cost_restore_structure_handler::expanded_rows_of` 给出。
//!
//! 本层只补一件结构表没有的东西 —— **本期人工 / 制造费用归集**（还原前的分母）：
//!
//! | 来源 | 取什么 | 干什么 |
//! |---|---|---|
//! | `biz_cost_collect`（`element = 'LABOR'` / `'OVERHEAD'`） | 本期按对象汇总的人工与制造费用 | 还原前「料工费」里除材料之外的两项 |
//!
//! 口径与 `cost_period_item_model::objects_of_period` 逐字一致（那里也是
//! `sum(amount) FILTER (WHERE element = 'LABOR'/'OVERHEAD')`），所以
//! **还原前三要素 = 计算单的 `mat_cur` / `labor_cur` / `oh_cur`**，不会出现
//! 「表上的投入合计对不上计算单」这种第二本账。
//!
//! # 数据范围
//!
//! 本查询**不带 scope 片段**，只按本页已有的对象 id 回填 —— 行是
//! `expanded_rows_of` 用 `biz_cost_object.workshop_dept_id` 裁过的，
//! 本层拿不到、也不需要「没裁过的行」。别图省事改成全表汇总：那会把
//! 看不见的车间的人工算进别人的分母。

use std::collections::HashMap;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model_base::ids_array_param;

/// 一次回填的取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LaborOverhead {
    /// 本期人工归集。
    pub labor: Decimal,
    /// 本期制造费用归集。
    pub overhead: Decimal,
}

const COLLECT_SQL: &str = "\
SELECT c.cost_object_id, \
       COALESCE(sum(c.amount) FILTER (WHERE c.element = 'LABOR'), 0) AS labor, \
       COALESCE(sum(c.amount) FILTER (WHERE c.element = 'OVERHEAD'), 0) AS oh \
  FROM biz_cost_collect c \
 WHERE c.period_code = ? AND c.deleted_at IS NULL \
   AND c.cost_object_id = ANY(?::text::uuid[]) \
 GROUP BY c.cost_object_id";

/// 取一批成本对象本期的人工 / 制造费用归集。
///
/// 与 `cost_period_item_model::objects_of_period` 的 element 口径一致（只认
/// `LABOR` / `OVERHEAD`，`MATERIAL` 由结构表的归集合计负责）。
pub async fn labor_overhead_of(period_code: &str, object_ids: &[Uuid]) -> DbResult<HashMap<Uuid, LaborOverhead>> {
    if object_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut unique: Vec<Uuid> = object_ids.to_vec();
    unique.sort();
    unique.dedup();

    let params = vec![Value::Text(period_code.to_string()), ids_array_param(&unique)];
    bind(COLLECT_SQL, &params)?;
    let rows = db().query_sql(COLLECT_SQL.to_string(), params).await?;
    let mut out: HashMap<Uuid, LaborOverhead> = HashMap::with_capacity(unique.len());
    for row in rows.iter() {
        let object_id: Uuid = row.get_typed("cost_object_id")?;
        out.insert(
            object_id,
            LaborOverhead {
                labor: row.get_typed("labor")?,
                overhead: row.get_typed("oh")?,
            },
        );
    }
    Ok(out)
}

/// 占位符 / 参数一致性校验（**有牙**：错位立刻 500，绝不绑错值）。
fn bind(sql: &str, params: &[Value]) -> DbResult<()> {
    let placeholders = sql.matches('?').count();
    if placeholders != params.len() {
        return Err(DbError::InvalidValue(format!(
            "成本还原表 SQL 占位符与参数数量不一致（?={placeholders}，参数={}）：参数错位会静默绑错值，\
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
    fn collect_sql_takes_period_and_object_array() {
        assert_eq!(COLLECT_SQL.matches('?').count(), 2, "期间 + 对象数组");
        assert!(COLLECT_SQL.contains("c.period_code = ?"));
        assert!(COLLECT_SQL.contains("c.cost_object_id = ANY(?::text::uuid[])"));
    }

    #[test]
    fn collect_sql_uses_the_same_element_codes_as_calc() {
        // 与 cost_period_item_model::objects_of_period 同口径（不同就成第二本账）
        assert!(COLLECT_SQL.contains("c.element = 'LABOR'"));
        assert!(COLLECT_SQL.contains("c.element = 'OVERHEAD'"));
        assert!(!COLLECT_SQL.contains("'ALLOC'"), "ALLOC 是来源类型，不是 element");
        assert!(COLLECT_SQL.contains("c.deleted_at IS NULL"));
    }

    #[test]
    fn collect_sql_has_no_scope_fragment() {
        // 行已经由结构表按 workshop_dept_id 裁过；这里再裁一次会把跨车间人工抹成 0
        assert!(!COLLECT_SQL.contains("workshop_dept_id"));
    }

    #[test]
    fn bind_rejects_misaligned_parameters() {
        let error = bind("SELECT 1 WHERE a = ?", &[]).expect_err("少参数必须失败");
        assert!(format!("{error:?}").contains("占位符与参数数量不一致"));
        bind("SELECT 1 WHERE a = ?", &[Value::Text("x".to_string())]).expect("对齐即通过");
    }

    #[tokio::test]
    async fn empty_objects_short_circuit() {
        // 没有对象时不发 SQL（分页空页也走这条路）
        let map = labor_overhead_of("2099-01", &[]).await.expect("空集合不发查询");
        assert!(map.is_empty());
    }
}
