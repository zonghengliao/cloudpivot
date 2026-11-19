//! 在制品「料工费在完工与在制之间分配」的**取数层**（成本核算 · M5-22）。
//!
//! 一条 SQL 取回分一行钱所需的全部事实（无 N+1）：
//!
//! | 来源 | 取什么 | 干什么 |
//! |---|---|---|
//! | `biz_cost_object t` | 数量 / 在制 / 完工率 / 投料方式 / 车间 | 行头 + 参与判据 + 数据范围锚点 |
//! | `biz_cost_period_item` + `biz_cost_period`（`pi`） | 最近一期**已结算**计算单的数量 / 成本 / 三个单位成本 | 期初在制按要素还原（走 calc 的 `beg_cost_of`） |
//! | 同上（`pc`） | 前期已结算**完工合计** / 最近一期期末在制数量 | 本期完工 = 累计 − 前期（跨月对象不能用累计数） |
//! | `biz_cost_collect`（`cc`） | 本期按 `element` 分类的三要素投入 | 成本池的「本期投入」 |
//! | `biz_cost_period_item`（`ci`） | 当期活跃批次的同一对象计算单 | 与表值逐字段对拍 |
//!
//! 三个「前期」派生表都按 `period_code < 本期` 比较（`yyyy-MM` 定长字典序 = 时间序），
//! 与 `cost_period_item_model::prior_settled_item` / `prior_completed_qty` 同口径。
//!
//! # 表别名必须是 `t`
//!
//! 数据范围片段是 `scope::filter_for(..BIZ_COST_OBJECT)` 产出的
//! ` AND t.workshop_dept_id = ?`（锚点里写死 `alias = "t"`）。改成 `o` 会让**非超管**
//! 的请求 500（`column t.workshop_dept_id does not exist`），而超管走 `AND 1 = 1`
//! 完全看不出来 —— 所以别名与锚点对齐，别图省事换字母。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_wip_alloc_dto::{self as dto, AllocFacts, AllocFilter, PriorFacts, StoredItem};
use crate::business::erp::cost::cost_wip_equiv_dto::RowHead;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::business::shared::scope;

/// 数据权限锚点：**复用**登记表常量（`shared/scope.rs` 的 `SCOPED_TABLES` 那条）。
///
/// 别 `ScopeTarget::new(...)` 抄一份：抄本会让登记表与代码各自演进，而且
/// `permission::scope_model::tests::d8c_wiring_anchors_exist_in_source` 是按
/// 「登记表锚点常量名 + 同文件 scope 调用」校验接线的，抄本扫不到。
pub const ALLOC_TARGET: scope::ScopeTarget = scope::ScopeTarget::BIZ_COST_OBJECT;

/// 单次取数的行数上限（超了报 400 让人收窄条件，而不是把内存吃光）。
pub const ROW_GUARD: usize = 5000;

/// 一行原始取数结果（分配还没算）。
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRow {
    /// 行头（名称 / 编码 / 维度，与算数无关）。
    pub head: RowHead,
    /// 分一行钱的全部事实。
    pub facts: AllocFacts,
}

// ==================== SQL ====================

/// 取数 SQL（参数顺序见模块头：`pi` → `pc` → `cc` → `ci` → 数据范围 → 关键字）。
fn list_sql() -> &'static str {
    "SELECT t.id AS object_id, t.object_no, t.object_type, t.source_doc_no, t.lot_no, \
            t.product_id, m.material_code AS product_code, m.material_name AS product_name, \
            t.workshop_dept_id, COALESCE(d.dept_name, t.workshop_dept_name) AS workshop_dept_name, \
            t.plan_qty, t.completed_qty AS cumulative_completed_qty, \
            COALESCE(pc.prior_completed_qty, 0) AS prior_completed_qty, \
            COALESCE(pc.prior_wip_end_qty, 0) AS prior_wip_end_qty, \
            t.wip_qty, t.completion_pct, t.material_input_mode, \
            COALESCE(cc.mat_cur, 0) AS mat_cur, \
            COALESCE(cc.labor_cur, 0) AS labor_cur, \
            COALESCE(cc.oh_cur, 0) AS oh_cur, \
            pi.period_code AS prior_period_code, pi.wip_end_qty AS prior_end_qty, \
            pi.wip_end_cost AS prior_end_cost, pi.completion_pct AS prior_completion_pct, \
            pi.unit_material AS prior_unit_material, pi.unit_labor AS prior_unit_labor, \
            pi.unit_overhead AS prior_unit_overhead, \
            ci.status AS calc_status, ci.wip_beg_cost AS calc_wip_beg_cost, \
            ci.completed_qty AS calc_completed_qty, ci.wip_end_qty AS calc_wip_qty, \
            ci.completion_pct AS calc_completion_pct, \
            ci.equiv_material AS calc_equiv_material, ci.equiv_conversion AS calc_equiv_conversion, \
            ci.unit_material AS calc_unit_material, ci.unit_labor AS calc_unit_labor, \
            ci.unit_overhead AS calc_unit_overhead, \
            ci.finished_cost AS calc_finished_cost, ci.wip_end_cost AS calc_wip_end_cost \
       FROM biz_cost_object t \
       LEFT JOIN biz_material m ON m.id = t.product_id AND m.deleted_at IS NULL \
       LEFT JOIN sys_dept d ON d.id = t.workshop_dept_id AND d.deleted_at IS NULL \
       LEFT JOIN ( \
             SELECT DISTINCT ON (i.cost_object_id) i.cost_object_id, p.period_code, \
                    i.wip_end_qty, i.wip_end_cost, i.completion_pct, \
                    i.unit_material, i.unit_labor, i.unit_overhead \
               FROM biz_cost_period_item i \
               JOIN biz_cost_period p ON p.id = i.period_id \
              WHERE p.deleted_at IS NULL AND p.status IN ('SETTLED','CLOSED') \
                AND p.period_code < ? \
              ORDER BY i.cost_object_id, p.period_code DESC \
            ) pi ON pi.cost_object_id = t.id \
       LEFT JOIN ( \
             SELECT i.cost_object_id, sum(i.completed_qty) AS prior_completed_qty, \
                    (array_agg(i.wip_end_qty ORDER BY p.period_code DESC))[1] AS prior_wip_end_qty \
               FROM biz_cost_period_item i \
               JOIN biz_cost_period p ON p.id = i.period_id \
              WHERE p.deleted_at IS NULL AND p.status IN ('SETTLED','CLOSED') \
                AND p.period_code < ? \
              GROUP BY i.cost_object_id \
            ) pc ON pc.cost_object_id = t.id \
       LEFT JOIN ( \
             SELECT c.cost_object_id, \
                    sum(c.amount) FILTER (WHERE c.element = 'MATERIAL') AS mat_cur, \
                    sum(c.amount) FILTER (WHERE c.element = 'LABOR') AS labor_cur, \
                    sum(c.amount) FILTER (WHERE c.element = 'OVERHEAD') AS oh_cur \
               FROM biz_cost_collect c \
              WHERE c.period_code = ? AND c.deleted_at IS NULL AND c.cost_object_id IS NOT NULL \
              GROUP BY c.cost_object_id \
            ) cc ON cc.cost_object_id = t.id \
       LEFT JOIN biz_cost_period_item ci \
              ON ci.cost_object_id = t.id AND ci.period_id = ?::uuid \
      WHERE t.deleted_at IS NULL AND t.status IN ('OPEN','CLOSED') \
        AND (t.wip_qty > 0 \
             OR t.completed_qty > COALESCE(pc.prior_completed_qty, 0) \
             OR COALESCE(pc.prior_wip_end_qty, 0) > 0 \
             OR COALESCE(cc.mat_cur, 0) <> 0 \
             OR COALESCE(cc.labor_cur, 0) <> 0 \
             OR COALESCE(cc.oh_cur, 0) <> 0)"
}

/// 关键字 → LIKE 模式（`%` / `_` / `\` 转义，与 `cost_collect_sheet_model` 同口径）。
fn keyword_pattern(keyword: Option<&str>) -> Option<String> {
    keyword.map(str::trim).filter(|text| !text.is_empty()).map(|text| {
        format!(
            "%{}%",
            text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
        )
    })
}

/// 业务过滤（关键字；**不含**数据范围片段）。
fn business_where(filter: &AllocFilter) -> (String, Vec<Value>) {
    let mut sql = String::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = keyword_pattern(filter.keyword.as_deref()) {
        // 对象号 / 生产订单号 / 产出批号 / 产出品编码 / 产出品名称：
        // 车间手上可能只有其中任一个（订单号来自销售、批号来自仓库、编码来自物料卡）。
        sql.push_str(
            " AND (t.object_no LIKE ? OR COALESCE(t.source_doc_no, '') LIKE ? \
                OR COALESCE(t.lot_no, '') LIKE ? OR COALESCE(m.material_code, '') LIKE ? \
                OR COALESCE(m.material_name, '') LIKE ?)",
        );
        for _ in 0..5 {
            params.push(Value::Text(pattern.clone()));
        }
    }
    (sql, params)
}

/// 占位符 / 参数一致性校验（**有牙**：错位立刻 500，绝不绑错值算错账）。
fn bind(sql: &str, params: &[Value]) -> DbResult<()> {
    let placeholders = sql.matches('?').count();
    if placeholders != params.len() {
        return Err(DbError::InvalidValue(format!(
            "在制品分配表 SQL 占位符与参数数量不一致（?={placeholders}，参数={}）：参数错位会静默绑错值，\
             拒绝执行。SQL={sql}",
            params.len()
        )));
    }
    Ok(())
}

// ==================== 读 ====================

/// 取本期参与分配的全部成本对象（已按数据范围 / 关键字裁剪，未分页）。
///
/// 期间必须已由调用方校验过（`yyyy-MM`）。`batch_id` 是当期活跃批次；没有批次时
/// 传 `Uuid::nil()`（`ci` 一个都 join 不上 ⇒ `stored = None`，表照出）。
pub async fn rows_of(principal: &ScopedPrincipal, filter: &AllocFilter, batch_id: Uuid) -> DbResult<Vec<SourceRow>> {
    let scope_filter = scope::filter_for(principal, ALLOC_TARGET);
    let (biz_sql, biz_params) = business_where(filter);
    let sql = format!("{}{}{} ORDER BY t.object_no", list_sql(), scope_filter.sql(), biz_sql);

    let mut params: Vec<Value> = Vec::with_capacity(4 + scope_filter.params().len() + biz_params.len());
    // ① pi：最近一期已结算（`period_code < 本期`）
    params.push(Value::Text(filter.period_code.clone()));
    // ② pc：前期已结算汇总
    params.push(Value::Text(filter.period_code.clone()));
    // ③ cc：本期归集
    params.push(Value::Text(filter.period_code.clone()));
    // ④ ci：当期活跃批次的计算单
    params.push(Value::from(&batch_id));
    // ⑤ 数据范围
    params.extend_from_slice(scope_filter.params());
    // ⑥ 关键字
    params.extend(biz_params);

    bind(&sql, &params)?;
    let rows = db().query_sql(sql, params).await?;
    if rows.len() > ROW_GUARD {
        return Err(DbError::InvalidValue(format!(
            "期间 {} 参与在制品分配的成本对象超过 {} 个（{} 个）：请用 keyword 收窄，\
             或按车间分开看（本表一次最多算 {} 行）",
            filter.period_code,
            ROW_GUARD,
            rows.len(),
            ROW_GUARD
        )));
    }
    rows.iter().map(source_row_of).collect()
}

/// 由查询行构造一条原始行。
fn source_row_of(row: &Row) -> DbResult<SourceRow> {
    let plan_qty: Decimal = row.get_typed("plan_qty")?;
    let head = RowHead {
        object_id: row.get_typed::<Uuid>("object_id")?.to_string(),
        object_no: row.get_typed("object_no")?,
        object_type: row.get_typed("object_type")?,
        source_doc_no: row.get_typed("source_doc_no")?,
        lot_no: row.get_typed("lot_no")?,
        product_id: row.get_typed::<Uuid>("product_id")?.to_string(),
        product_code: row.get_typed("product_code")?,
        product_name: row.get_typed("product_name")?,
        workshop_dept_id: row.get_typed::<Uuid>("workshop_dept_id")?.to_string(),
        workshop_dept_name: row.get_typed("workshop_dept_name")?,
        plan_qty: dto::qty_text(plan_qty),
    };

    // 期初来源：最近一期已结算计算单（没有 ⇒ 首期，期初全 0）
    let prior_period: Option<String> = row.get_typed("prior_period_code")?;
    let prior = match prior_period {
        None => None,
        Some(period_code) => Some(PriorFacts {
            period_code,
            wip_end_qty: row.get_typed("prior_end_qty")?,
            wip_end_cost: row.get_typed("prior_end_cost")?,
            completion_pct: row.get_typed("prior_completion_pct")?,
            unit_material: row.get_typed("prior_unit_material")?,
            unit_labor: row.get_typed("prior_unit_labor")?,
            unit_overhead: row.get_typed("prior_unit_overhead")?,
        }),
    };

    // 本期计算单快照（没批次 / 该对象没算出计算单 ⇒ None）
    let calc_status: Option<String> = row.get_typed("calc_status")?;
    let stored = match calc_status {
        None => None,
        Some(status) => Some(StoredItem {
            status,
            wip_beg_cost: row.get_typed("calc_wip_beg_cost")?,
            completed_qty: row.get_typed("calc_completed_qty")?,
            wip_end_qty: row.get_typed("calc_wip_qty")?,
            completion_pct: row.get_typed("calc_completion_pct")?,
            equiv_material: row.get_typed("calc_equiv_material")?,
            equiv_conversion: row.get_typed("calc_equiv_conversion")?,
            unit_material: row.get_typed("calc_unit_material")?,
            unit_labor: row.get_typed("calc_unit_labor")?,
            unit_overhead: row.get_typed("calc_unit_overhead")?,
            finished_cost: row.get_typed("calc_finished_cost")?,
            wip_end_cost: row.get_typed("calc_wip_end_cost")?,
        }),
    };

    let facts = AllocFacts {
        object_no: head.object_no.clone(),
        material_input_mode: row.get_typed("material_input_mode")?,
        cumulative_completed_qty: row.get_typed("cumulative_completed_qty")?,
        prior_completed_qty: row.get_typed("prior_completed_qty")?,
        wip_qty: row.get_typed("wip_qty")?,
        completion_pct: row.get_typed("completion_pct")?,
        mat_cur: row.get_typed("mat_cur")?,
        labor_cur: row.get_typed("labor_cur")?,
        oh_cur: row.get_typed("oh_cur")?,
        prior,
        stored,
    };

    Ok(SourceRow { head, facts })
}

// ==================== 单测（不连库：SQL 形状与参数一致性） ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_sql_keeps_the_scope_alias() {
        // 数据范围锚点写死 `t.workshop_dept_id`：换个字母只有非超管会 500
        assert!(list_sql().contains("FROM biz_cost_object t"));
        assert!(list_sql().contains("t.workshop_dept_id"));
        assert!(!list_sql().contains("biz_cost_object o"));
    }

    #[test]
    fn list_sql_takes_exactly_four_period_parameters() {
        // pi / pc / cc / ci 各一个；多一个少一个都会被 bind() 拦下
        assert_eq!(list_sql().matches('?').count(), 4);
    }

    #[test]
    fn list_sql_never_overwrites_the_prior_wip_alias() {
        // pi.prior_end_qty 与 pc.prior_wip_end_qty 必须是两个名字，否则后者会盖掉前者
        assert!(list_sql().contains("pi.wip_end_qty AS prior_end_qty"));
        assert!(list_sql().contains("AS prior_wip_end_qty"));
    }

    #[test]
    fn list_sql_filters_all_three_elements() {
        assert!(list_sql().contains("c.element = 'MATERIAL'"));
        assert!(list_sql().contains("c.element = 'LABOR'"));
        assert!(list_sql().contains("c.element = 'OVERHEAD'"));
    }

    #[test]
    fn keyword_pattern_escapes_wildcards() {
        assert_eq!(keyword_pattern(Some("  CB_1%  ")).as_deref(), Some("%CB\\_1\\%%"));
        assert_eq!(keyword_pattern(Some("   ")), None);
        assert_eq!(keyword_pattern(None), None);
    }

    #[test]
    fn keyword_adds_five_parameters() {
        let (sql, params) = business_where(&AllocFilter {
            period_code: "2026-11".to_string(),
            keyword: Some("CB".to_string()),
            state: dto::AllocStateFilter::All,
            page: 1,
            page_size: 20,
        });
        assert_eq!(params.len(), 5);
        assert_eq!(sql.matches('?').count(), 5);
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
}
