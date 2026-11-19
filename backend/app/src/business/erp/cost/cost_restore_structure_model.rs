//! 成本还原「半成品成本结构表」的**取数层**（成本核算 · M5-26）。
//!
//! # 取什么、从哪来
//!
//! | 来源 | 取什么 | 干什么 |
//! |---|---|---|
//! | `biz_cost_object` + `biz_material` + `sys_dept` | 对象抬头、产出品、车间 | 根行（一行 = 一个本期有材料投入的成本对象） |
//! | `biz_cost_collect`（`element = 'MATERIAL'`） | 本期材料投入合计 | 行筛选（没有材料投入就没有结构可看） |
//! | `biz_cost_period_item` + `biz_cost_period` | 本期计算单快照（完工 / 完工成本 / 三个单位成本） | 行头的成本总额与 `structure_of` 的输入 |
//! | `biz_cost_collect` + `biz_stock_out_item` + `biz_material` | 逐（对象, 期间, 物料, 批号）的材料投入 | 逐层结构的每一行（金额 / 数量 / 批号） |
//! | `biz_cost_object` + `biz_cost_period_item` + `biz_cost_period` | 「谁产出了这个物料」的候选批次（本期及以前、非红冲） | 半成品的完工成本结构（`batch` / `period_avg`） |
//!
//! # 数据范围只裁「行」，不裁「半成品的产出批次」
//!
//! 行由 `biz_cost_object.workshop_dept_id` 裁剪（与归集表 / 约当表 / 分配表 / 余额表同一锚点）。
//!
//! 半成品的产出批次**不再二次裁剪** —— 这是有意的：
//! ① 半成品的成本结构就是**已经算进本行材料成本**的那笔钱，用户看得到总额却看不到构成，
//!    只会拿着一个算不清的金额去对总账；
//! ② 跨车间的半成品流转（冲压 → 焊装 → 总装）是离散制造的常态，二次裁剪会让
//!    「本期材料投入 100 万、逐层结构全是 `null`」变成常见状态。
//!
//! 因此生产者取数不带 scope 片段；本层只暴露**单位成本与结构比例**（`ProducerView`
//! 里的完工数量 / 完工成本是该批次自身的口径，不是别的车间的台账明细）。
//!
//! # 批量取，不逐行问库
//!
//! 逐层展开是 BFS：每一层两条查询（材料投入 + 生产者），层数 ≤ `MAX_DEPTH_LIMIT`。
//! 材料投入按 `(对象 id 数组, 期间数组)` 一次取回，再在内存里按允许的 (对象, 期间) 配对过滤
//! （驱动不支持 `Value::Array`，数组一律走 PG 文本数组字面量）。
//!
//! # 别名必须是 `t`
//!
//! 数据范围片段是 `scope::filter_for(..BIZ_COST_OBJECT)` 产出的 ` AND t.workshop_dept_id = ?`
//! （锚点里写死 `alias = "t"`）。改成别的字母会让**非超管**的请求 500，而超管
//! 走 `AND 1 = 1` 完全看不出来 —— 别图省事换字母。

use std::collections::HashMap;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_restore_structure_dto::{ItemFacts, ProducerFacts};
use crate::business::erp::model_base::ids_array_param;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::business::shared::scope;

/// 数据权限锚点：**复用**登记表常量（`biz_cost_object`，别名 `t`）。
pub const RESTORE_TARGET: scope::ScopeTarget = scope::ScopeTarget::BIZ_COST_OBJECT;

/// 单次取数的行数上限（与归集表 / 约当表同档）。
pub const ROW_GUARD: usize = 5000;

/// 一次性查生产者的物料 id 分片大小（数组太长的 SQL 会拖慢规划）。
pub const MATERIAL_CHUNK: usize = 500;

// ==================== 行 ====================

/// 一行原始取数结果（结构还没展开）。
#[derive(Debug, Clone, PartialEq)]
pub struct RootRow {
    /// 成本对象 id。
    pub object_id: Uuid,
    /// 对象号。
    pub object_no: String,
    /// 对象类型。
    pub object_type: String,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 产出品 id。
    pub product_id: Uuid,
    /// 产出品编码。
    pub product_code: Option<String>,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 车间 id。
    pub workshop_dept_id: Uuid,
    /// 车间名称。
    pub workshop_dept_name: Option<String>,
    /// 本期材料投入（归集合计）。
    pub material_cost: Decimal,
    /// 本期计算单的材料投入（没计算单 ⇒ 0）。
    pub mat_cur: Decimal,
    /// 本期计算单的人工投入（没计算单 ⇒ 0）。
    pub labor_cur: Decimal,
    /// 本期计算单的制造费用投入（没计算单 ⇒ 0）。
    pub oh_cur: Decimal,
    /// 本期计算单（没批次 / 该对象没算出来 ⇒ `None`）。
    pub item: Option<ItemFacts>,
}

const ROOT_SQL: &str = "\
SELECT t.id AS object_id, t.object_no, t.object_type, t.lot_no, \
       t.product_id, m.material_code AS product_code, m.material_name AS product_name, \
       t.workshop_dept_id, COALESCE(d.dept_name, t.workshop_dept_name) AS workshop_dept_name, \
       COALESCE(cc.mat_cost, 0) AS material_cost, \
       ci.status AS item_status, ci.completed_qty, ci.finished_cost, ci.unit_finished_cost, \
       ci.mat_cur, ci.labor_cur, ci.oh_cur, \
       ci.unit_material, ci.unit_labor, ci.unit_overhead \
  FROM biz_cost_object t \
  LEFT JOIN biz_material m ON m.id = t.product_id AND m.deleted_at IS NULL \
  LEFT JOIN sys_dept d ON d.id = t.workshop_dept_id AND d.deleted_at IS NULL \
  LEFT JOIN ( \
        SELECT c.cost_object_id, sum(c.amount) AS mat_cost \
          FROM biz_cost_collect c \
         WHERE c.period_code = ? AND c.element = 'MATERIAL' AND c.deleted_at IS NULL \
           AND c.cost_object_id IS NOT NULL \
         GROUP BY c.cost_object_id \
        ) cc ON cc.cost_object_id = t.id \
  LEFT JOIN biz_cost_period_item ci ON ci.cost_object_id = t.id AND ci.period_id = ?::uuid \
 WHERE t.deleted_at IS NULL AND t.status IN ('OPEN','CLOSED') \
   AND COALESCE(cc.mat_cost, 0) > 0";

/// 关键字 → LIKE 模式（`%` / `_` / `\` 转义，与其它成本表同口径）。
fn keyword_pattern(keyword: Option<&str>) -> Option<String> {
    keyword.map(str::trim).filter(|text| !text.is_empty()).map(|text| {
        format!(
            "%{}%",
            text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
        )
    })
}

/// 关键字过滤（5 个占位符：对象号 / 生产订单号 / 产出批号 / 产出品编码 / 产出品名称）。
fn keyword_where(keyword: Option<&str>) -> (String, Vec<Value>) {
    match keyword_pattern(keyword) {
        None => (String::new(), Vec::new()),
        Some(pattern) => {
            let sql = " AND (t.object_no LIKE ? OR COALESCE(t.source_doc_no, '') LIKE ? \
                       OR COALESCE(t.lot_no, '') LIKE ? OR COALESCE(m.material_code, '') LIKE ? \
                       OR COALESCE(m.material_name, '') LIKE ?)"
                .to_string();
            let params = (0..5).map(|_| Value::Text(pattern.clone())).collect();
            (sql, params)
        }
    }
}

/// 取本期参与结构表的成本对象（已按数据范围 / 关键字裁剪，未分页）。
///
/// `batch_id` 是当期活跃批次；没有批次时传 `Uuid::nil()`（`ci` 一个都 join 不上 ⇒ 行头的
/// 计算单为空，结构照出 —— 结构只依赖归集行与产出批次，不依赖本期算没算过）。
pub async fn roots_of(
    principal: &ScopedPrincipal,
    period_code: &str,
    keyword: Option<&str>,
    batch_id: Uuid,
) -> DbResult<Vec<RootRow>> {
    let scope_filter = scope::filter_for(principal, RESTORE_TARGET);
    let (keyword_sql, keyword_params) = keyword_where(keyword);
    let sql = format!("{ROOT_SQL}{}{keyword_sql} ORDER BY t.object_no", scope_filter.sql());

    let mut params: Vec<Value> = Vec::with_capacity(2 + scope_filter.params().len() + keyword_params.len());
    params.push(Value::Text(period_code.to_string()));
    params.push(Value::from(&batch_id));
    params.extend_from_slice(scope_filter.params());
    params.extend(keyword_params);

    bind(&sql, &params)?;
    let rows = db().query_sql(sql, params).await?;
    if rows.len() > ROW_GUARD {
        return Err(DbError::InvalidValue(format!(
            "期间 {period_code} 有材料投入的成本对象超过 {ROW_GUARD} 个（{} 个）：请用 keyword 收窄，\
             或按车间分开看（本表一次最多算 {ROW_GUARD} 行）",
            rows.len()
        )));
    }
    rows.iter().map(root_row_of).collect()
}

fn root_row_of(row: &Row) -> DbResult<RootRow> {
    let item_status: Option<String> = row.get_typed("item_status")?;
    let item = match item_status {
        None => None,
        Some(status) => Some(ItemFacts {
            status,
            completed_qty: row.get_typed("completed_qty")?,
            finished_cost: row.get_typed("finished_cost")?,
            unit_finished_cost: row.get_typed("unit_finished_cost")?,
            unit_material: row.get_typed("unit_material")?,
            unit_labor: row.get_typed("unit_labor")?,
            unit_overhead: row.get_typed("unit_overhead")?,
        }),
    };
    let mat_cur: Option<Decimal> = row.get_typed("mat_cur")?;
    let labor_cur: Option<Decimal> = row.get_typed("labor_cur")?;
    let oh_cur: Option<Decimal> = row.get_typed("oh_cur")?;
    Ok(RootRow {
        object_id: row.get_typed("object_id")?,
        object_no: row.get_typed("object_no")?,
        object_type: row.get_typed("object_type")?,
        lot_no: row.get_typed("lot_no")?,
        product_id: row.get_typed("product_id")?,
        product_code: row.get_typed("product_code")?,
        product_name: row.get_typed("product_name")?,
        workshop_dept_id: row.get_typed("workshop_dept_id")?,
        workshop_dept_name: row.get_typed("workshop_dept_name")?,
        material_cost: row.get_typed("material_cost")?,
        mat_cur: mat_cur.unwrap_or(Decimal::ZERO),
        labor_cur: labor_cur.unwrap_or(Decimal::ZERO),
        oh_cur: oh_cur.unwrap_or(Decimal::ZERO),
        item,
    })
}

// ==================== 材料投入（逐层） ====================

/// 一条材料投入（一个父对象在其期间内、对某物料某个批号的领料汇总）。
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialInput {
    /// 父成本对象 id。
    pub cost_object_id: Uuid,
    /// 父对象期间（`yyyy-MM`）。
    pub period_code: String,
    /// 物料 id。
    pub material_id: Option<Uuid>,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 批号（没有 ⇒ `None`）。
    pub lot_no: Option<String>,
    /// 数量合计。
    pub qty: Decimal,
    /// 金额合计。
    pub amount: Decimal,
}

const INPUT_SQL: &str = "\
SELECT c.cost_object_id, c.period_code, c.material_id, \
       m.material_code AS material_code, m.material_name AS material_name, \
       NULLIF(COALESCE(i.lot_no, ''), '') AS lot_no, \
       COALESCE(sum(c.qty), 0) AS qty, COALESCE(sum(c.amount), 0) AS amount \
  FROM biz_cost_collect c \
  LEFT JOIN biz_stock_out_item i ON i.id = c.source_id AND c.source_type = 'STOCK_OUT' \
  LEFT JOIN biz_material m ON m.id = c.material_id AND m.deleted_at IS NULL \
 WHERE c.element = 'MATERIAL' AND c.deleted_at IS NULL \
   AND c.cost_object_id = ANY(?::text::uuid[]) \
   AND c.period_code = ANY(string_to_array(?, ',')) \
 GROUP BY c.cost_object_id, c.period_code, c.material_id, m.material_code, m.material_name, \
          NULLIF(COALESCE(i.lot_no, ''), '') \
 ORDER BY amount DESC, m.material_code ASC NULLS LAST, lot_no ASC NULLS LAST";

/// 取一批 `(对象, 期间)` 的材料投入（一次查询，结果按两者配对在内存里过滤）。
///
/// # 为什么带期间数组再过滤
///
/// 各层的父对象期间可能不同（半成品是上个月产的、成品是本月产的），一次带
/// `period_code = ANY(...)` 取回后在内存里按允许的配对过滤，比按期间分多次查询少绕一圈；
/// 多取的行只是「某个对象在另一个期间」的材料投入，金额不进任何节点。
pub async fn material_inputs_of(pairs: &[(Uuid, String)]) -> DbResult<Vec<MaterialInput>> {
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let mut object_ids: Vec<Uuid> = Vec::with_capacity(pairs.len());
    let mut periods: Vec<String> = Vec::with_capacity(pairs.len());
    for (object_id, period) in pairs {
        object_ids.push(*object_id);
        periods.push(period.clone());
    }
    object_ids.sort();
    object_ids.dedup();
    periods.sort();
    periods.dedup();

    let params = vec![ids_array_param(&object_ids), periods_param(&periods)];
    bind(INPUT_SQL, &params)?;
    let rows = db().query_sql(INPUT_SQL.to_string(), params).await?;
    rows.iter()
        .map(|row| {
            Ok(MaterialInput {
                cost_object_id: row.get_typed("cost_object_id")?,
                period_code: row.get_typed("period_code")?,
                material_id: row.get_typed("material_id")?,
                material_code: row.get_typed("material_code")?,
                material_name: row.get_typed("material_name")?,
                lot_no: row.get_typed("lot_no")?,
                qty: row.get_typed("qty")?,
                amount: row.get_typed("amount")?,
            })
        })
        .collect()
}

// ==================== 生产者（谁产出了这个物料） ====================

const PRODUCED_MATERIAL_SQL: &str = "\
SELECT DISTINCT o.product_id \
  FROM biz_cost_object o \
 WHERE o.deleted_at IS NULL AND o.status IN ('OPEN','CLOSED') \
   AND o.product_id = ANY(?::text::uuid[])";

/// 「哪些物料是自己产过的」（= 自制半成品），**不看有没有计算单**。
///
/// 这是 `is_semi` 的判据，必须与「取不取得到结构」分开：
/// 一个物料只要有成本对象产出过，它就是自制半成品；产出它的批次还没计算 ⇒
/// 结构取不到（`NO_STRUCTURE`），**不能**因此把它当成外购料（那会把金额全额记进材料，
/// 还原出来的料工费就少算了工和费）。
pub async fn produced_materials(material_ids: &[Uuid]) -> DbResult<std::collections::HashSet<Uuid>> {
    let mut unique: Vec<Uuid> = material_ids.to_vec();
    unique.sort();
    unique.dedup();

    let mut out = std::collections::HashSet::new();
    for chunk in unique.chunks(MATERIAL_CHUNK) {
        let params = vec![ids_array_param(chunk)];
        bind(PRODUCED_MATERIAL_SQL, &params)?;
        let rows = db().query_sql(PRODUCED_MATERIAL_SQL.to_string(), params).await?;
        for row in rows.iter() {
            out.insert(row.get_typed::<Uuid>("product_id")?);
        }
    }
    Ok(out)
}

const PRODUCER_SQL: &str = "\
SELECT o.product_id, NULLIF(COALESCE(o.lot_no, ''), '') AS lot_no, \
       o.id AS object_id, o.object_no, p.period_code, \
       i.status AS item_status, i.completed_qty, i.finished_cost, i.unit_finished_cost, \
       i.unit_material, i.unit_labor, i.unit_overhead \
  FROM biz_cost_object o \
  JOIN biz_cost_period_item i ON i.cost_object_id = o.id \
  JOIN biz_cost_period p ON p.id = i.period_id \
 WHERE o.deleted_at IS NULL AND o.status IN ('OPEN','CLOSED') \
   AND p.deleted_at IS NULL AND p.status NOT IN ('REVERSED', 'CANCELLED') \
   AND p.period_code <= ? \
   AND o.product_id = ANY(?::text::uuid[]) \
 ORDER BY o.product_id, p.period_code DESC, o.object_no";

/// 取「产出这些物料的候选批次」（本期及以前、非红冲），按物料 id 分组。
///
/// 不挂数据范围片段：理由见模块头（半成品结构是**已算进**本行材料成本的那笔钱的结构）。
pub async fn producers_of(material_ids: &[Uuid], period_code: &str) -> DbResult<HashMap<Uuid, Vec<ProducerFacts>>> {
    let mut unique: Vec<Uuid> = material_ids.to_vec();
    unique.sort();
    unique.dedup();

    let mut out: HashMap<Uuid, Vec<ProducerFacts>> = HashMap::new();
    for chunk in unique.chunks(MATERIAL_CHUNK) {
        let params = vec![Value::Text(period_code.to_string()), ids_array_param(chunk)];
        bind(PRODUCER_SQL, &params)?;
        let rows = db().query_sql(PRODUCER_SQL.to_string(), params).await?;
        for row in rows.iter() {
            let product_id: Uuid = row.get_typed("product_id")?;
            out.entry(product_id).or_default().push(ProducerFacts {
                product_id: product_id.to_string(),
                lot_no: row.get_typed("lot_no")?,
                object_id: row.get_typed::<Uuid>("object_id")?.to_string(),
                object_no: row.get_typed("object_no")?,
                period_code: row.get_typed("period_code")?,
                item: ItemFacts {
                    status: row.get_typed("item_status")?,
                    completed_qty: row.get_typed("completed_qty")?,
                    finished_cost: row.get_typed("finished_cost")?,
                    unit_finished_cost: row.get_typed("unit_finished_cost")?,
                    unit_material: row.get_typed("unit_material")?,
                    unit_labor: row.get_typed("unit_labor")?,
                    unit_overhead: row.get_typed("unit_overhead")?,
                },
            });
        }
    }
    Ok(out)
}

// ==================== 辅助 ====================

/// 期间数组参数（逗号分隔的 text 载荷，配 `string_to_array(?, ',')`）。
///
/// 为什么不用 `?::text[]`：本栈驱动把占位符推断成数组类型后只有 `text` 编码器，
/// 实测 `error serializing parameter N`（`erp/serial_model.rs` 的踩坑）；
/// `?::text::uuid[]` 可行（uuid 数组例外），text 数组一律走 `string_to_array`。
fn periods_param(periods: &[String]) -> Value {
    Value::Text(periods.join(","))
}

/// 占位符 / 参数一致性校验（**有牙**：错位立刻 500，绝不绑错值）。
fn bind(sql: &str, params: &[Value]) -> DbResult<()> {
    let placeholders = sql.matches('?').count();
    if placeholders != params.len() {
        return Err(DbError::InvalidValue(format!(
            "成本还原结构表 SQL 占位符与参数数量不一致（?={placeholders}，参数={}）：参数错位会静默绑错值，\
             拒绝执行。SQL={sql}",
            params.len()
        )));
    }
    Ok(())
}

/// 期间字符串 → `(年, 月)`（闭包式校验的兜底，取数层不做业务校验）。
pub fn period_of(text: &str) -> Option<(i32, u32)> {
    let (year, month) = text.split_once('-')?;
    Some((year.parse().ok()?, month.parse().ok()?))
}

// ==================== 单测（不连库：SQL 形状与参数一致性） ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_sql_keeps_the_scope_alias() {
        // 数据范围锚点写死 `t.workshop_dept_id`：换个字母只有非超管会 500
        assert!(ROOT_SQL.contains("FROM biz_cost_object t"));
        assert!(ROOT_SQL.contains("t.workshop_dept_id"));
        assert!(!ROOT_SQL.contains("biz_cost_object o"));
    }

    #[test]
    fn root_sql_takes_two_parameters_without_keyword() {
        assert_eq!(ROOT_SQL.matches('?').count(), 2, "期间 + 批次 id");
    }

    #[test]
    fn root_sql_only_keeps_objects_with_material_input() {
        // 「本期没有材料投入」的行没有结构可看：在 SQL 里就剔掉，而不是出成一行 0
        assert!(ROOT_SQL.contains("COALESCE(cc.mat_cost, 0) > 0"));
        assert!(ROOT_SQL.contains("c.element = 'MATERIAL'"));
    }

    #[test]
    fn keyword_adds_five_parameters() {
        let (sql, params) = keyword_where(Some("CB"));
        assert_eq!(params.len(), 5);
        assert_eq!(sql.matches('?').count(), 5);
        let (sql, params) = keyword_where(None);
        assert!(sql.is_empty());
        assert!(params.is_empty());
    }

    #[test]
    fn keyword_pattern_escapes_wildcards() {
        assert_eq!(keyword_pattern(Some("  CB_1%  ")).as_deref(), Some("%CB\\_1\\%%"));
        assert_eq!(keyword_pattern(Some("   ")), None);
        assert_eq!(keyword_pattern(None), None);
    }

    #[test]
    fn input_sql_takes_object_and_period_arrays() {
        assert_eq!(INPUT_SQL.matches('?').count(), 2, "对象数组 + 期间载荷");
        assert!(INPUT_SQL.contains("c.cost_object_id = ANY(?::text::uuid[])"));
        assert!(
            INPUT_SQL.contains("c.period_code = ANY(string_to_array(?, ','))"),
            "text 数组不能直绑 ?::text[]（驱动只有 text 编码器）"
        );
        assert!(INPUT_SQL.contains("c.element = 'MATERIAL'"));
        // 批号来自领料明细，且只认 STOCK_OUT（手工归集行没有批号 ⇒ 走同月平均）
        assert!(INPUT_SQL.contains("c.source_type = 'STOCK_OUT'"));
        assert!(INPUT_SQL.contains("i.lot_no"));
    }

    #[test]
    fn producer_sql_excludes_reversed_and_future_periods() {
        assert!(PRODUCER_SQL.contains("p.status NOT IN ('REVERSED', 'CANCELLED')"));
        assert!(
            PRODUCER_SQL.contains("p.period_code <= ?"),
            "未来期间的计算单不该解释本期成本"
        );
        assert!(PRODUCER_SQL.contains("o.product_id = ANY(?::text::uuid[])"));
        assert_eq!(PRODUCER_SQL.matches('?').count(), 2);
        // 刻意不挂数据范围：半成品结构是已算进本行材料成本的结构（见模块头）
        assert!(!PRODUCER_SQL.contains("workshop_dept_id"));
    }

    #[test]
    fn periods_param_is_comma_payload() {
        // text 数组不能直绑 `?::text[]`（驱动只有 text 编码器）⇒ 逗号载荷 + string_to_array
        assert!(matches!(
            periods_param(&["2026-09".to_string(), "2026-10".to_string()]),
            Value::Text(ref text) if text == "2026-09,2026-10"
        ));
        assert!(matches!(periods_param(&[]), Value::Text(ref text) if text.is_empty()));
    }

    #[test]
    fn bind_rejects_misaligned_parameters() {
        let error =
            bind("SELECT 1 WHERE a = ? AND b = ?", &[Value::Text("x".to_string())]).expect_err("少一个参数必须失败");
        assert!(format!("{error:?}").contains("占位符与参数数量不一致"));
        bind("SELECT 1 WHERE a = ?", &[Value::Text("x".to_string())]).expect("对齐即通过");
    }

    #[test]
    fn period_of_splits_year_and_month() {
        assert_eq!(period_of("2026-11"), Some((2026, 11)));
        assert_eq!(period_of("2026-11-01"), None);
        assert_eq!(period_of("x"), None);
    }
}
