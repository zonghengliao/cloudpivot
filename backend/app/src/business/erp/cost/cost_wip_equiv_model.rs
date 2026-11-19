//! 约当产量表 · 数据层（成本核算 · M5-20）。
//!
//! 表结构唯一依据：`biz_cost_object`（V0370 §3.1 ②）+ `biz_cost_period_item`（V0373 ⑩）
//! + `biz_cost_collect`（V0371 §3.1 ③）。**本卡不新增迁移**。
//!
//! # 一次查询把一行要的东西全取回来
//!
//! | 需要什么 | 从哪来 | 怎么取 |
//! |---|---|---|
//! | 累计完工 / 期末在制 / 完工率 / 投料方式 | `biz_cost_object` | 主表 |
//! | 前期已结算完工合计 + 上期期末在制数量 | `biz_cost_period_item` × `biz_cost_period` | 派生表 `pc`（`period_code < ?`） |
//! | 本期已归集成本（判 `NO_OUTPUT` 用） | `biz_cost_collect` | 派生表 `cc`（`period_code = ?`） |
//! | 本期计算单快照（对拍漂移用） | `biz_cost_period_item` | `ci`（`period_id = 当期活跃批次`） |
//! | 产出品编码 / 名称 | `biz_material` | 实时 join（AGENTS 约定 13：主数据关联取最新名） |
//! | 车间名 | `sys_dept`，落空回落建单快照 | 实时 join + `COALESCE` |
//!
//! # 行集口径（表里该有哪些对象）
//!
//! ```text
//! 期末在制 > 0                        -- 有在制的对象，当然要算约当
//! ∪ 本期完工 > 0（累计 − 前期已结算）  -- 本期出货的对象，约当 = 完工数
//! ∪ 上期已结算还挂着期末在制           -- 跨月对象哪怕这期一笔没投也在表里
//! ∪ 本期有归集行                       -- 有投入没产量的对象要出现，标「算不出来」
//! ```
//!
//! 后两类不能漏：漏了就等于「这行的成本没人管」，而它们正是最需要人看一眼的那几行。
//!
//! # 数据权限与参数顺序
//!
//! 行由 `biz_cost_object.workshop_dept_id` 裁剪（与 `cost-collect-sheet` 同一份锚点
//! [`EQUIV_TARGET`]，不手写范围谓词）。本模块**不用** `scope::paginate`：分页在 Rust 侧
//! 做（见下），所以按 `scope::filter_for` 的「单条 / 导出取数」用法显式拼进 SQL。
//!
//! `?` 在 SQL 文本里的出现顺序 = 参数列表顺序，**改 SQL 必须同步改 [`bind`] 的注释与参数构造**：
//!
//! ```text
//! ① pc 派生表的 period_code < ?         ← FROM 里第一个派生表
//! ② cc 派生表的 period_code = ?         ← FROM 里第二个派生表
//! ③ ci 的 period_id = ?::uuid           ← FROM 里最后一个 join
//! ④ 数据范围片段 scope_filter.sql()      ← WHERE 首位（行集条件之后）
//! ⑤ 关键字 5 个 LIKE ?                  ← WHERE 末位
//! ```
//!
//! [`bind`] 逐次校验「`?` 个数 == 参数个数」：错位会静默绑错值（期间绑到状态上、
//! 返回一张空表），比报错危险得多。
//!
//! # 为什么分页在 Rust 侧
//!
//! 三类筛选里有两类（`blocked` / `drift`）**只能算出来才知道**：`blocked` 取决于
//! 「在制 > 0 且完工率 0」这种组合，`drift` 要拿表值和计算单逐字段比。SQL 里分页会把
//! 「筛选」变成「先分页后过滤」，于是 `total` 与合计都不对。行集量级是「一个期间的
//! 成本对象数」（当月的批次数），整表取回来再算再分页是划算的；另设 [`ROW_GUARD`]
//! 兜住离谱量级（超了报 400 让人收窄期间 / 关键字，而不是把内存吃光）。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_wip_equiv_dto::{self as dto, EquivFilter};
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::business::shared::scope;

/// 数据权限锚点：**复用**登记表常量（`shared/scope.rs` 的 `SCOPED_TABLES` 那条）。
///
/// 别 `ScopeTarget::new(...)` 抄一份：抄本会让登记表与代码各自演进，而且
/// `permission::scope_model::tests::d8c_wiring_anchors_exist_in_source` 是按
/// 「登记表锚点常量名 + 同文件 scope 调用」校验接线的，抄本扫不到。
pub const EQUIV_TARGET: scope::ScopeTarget = scope::ScopeTarget::BIZ_COST_OBJECT;

/// 单次取数的行数上限（超了报 400 让人收窄条件，而不是把内存吃光）。
pub const ROW_GUARD: usize = 5000;

/// 一行原始取数结果（约当还没算）。
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRow {
    /// 成本对象 id。
    pub object_id: Uuid,
    /// 对象号。
    pub object_no: String,
    /// 对象类型。
    pub object_type: String,
    /// 生产订单号。
    pub source_doc_no: Option<String>,
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
    /// 车间名（join 到的现名，落空回落快照）。
    pub workshop_dept_name: Option<String>,
    /// 计划数量。
    pub plan_qty: Decimal,
    /// 累计完工数量。
    pub cumulative_completed_qty: Decimal,
    /// 前期已结算完工合计。
    pub prior_completed_qty: Decimal,
    /// 上期已结算的期末在制数量。
    pub prior_wip_end_qty: Decimal,
    /// 期末在制数量。
    pub wip_qty: Decimal,
    /// 完工率 %。
    pub completion_pct: Decimal,
    /// 投料方式。
    pub material_input_mode: String,
    /// 本期已归集成本。
    pub collected_total: Decimal,
    /// 本期计算单快照。
    pub stored: Option<dto::StoredCalc>,
}

impl SourceRow {
    /// 算数事实（纯数据，喂给 [`dto::equiv_of`]）。
    pub fn facts(&self) -> dto::EquivFacts {
        dto::EquivFacts {
            object_no: self.object_no.clone(),
            material_input_mode: self.material_input_mode.clone(),
            cumulative_completed_qty: self.cumulative_completed_qty,
            prior_completed_qty: self.prior_completed_qty,
            wip_qty: self.wip_qty,
            completion_pct: self.completion_pct,
            collected_total: self.collected_total,
            prior_wip_end_qty: self.prior_wip_end_qty,
            stored: self.stored.clone(),
        }
    }

    /// 行头（名称 / 维度，与算数无关）。
    pub fn head(&self) -> dto::RowHead {
        dto::RowHead {
            object_id: self.object_id.to_string(),
            object_no: self.object_no.clone(),
            object_type: self.object_type.clone(),
            source_doc_no: self.source_doc_no.clone(),
            lot_no: self.lot_no.clone(),
            product_id: self.product_id.to_string(),
            product_code: self.product_code.clone(),
            product_name: self.product_name.clone(),
            workshop_dept_id: self.workshop_dept_id.to_string(),
            workshop_dept_name: self.workshop_dept_name.clone(),
            plan_qty: dto::qty_text(self.plan_qty),
        }
    }
}

// ==================== SQL ====================

/// 取数 SQL（参数顺序见模块头）。
fn list_sql() -> &'static str {
    // ★ 表别名必须是 `t`：数据范围片段是 `scope::filter_for(..BIZ_COST_OBJECT)` 产出的
    //   ` AND t.workshop_dept_id = ?`（锚点里写死 alias = "t"）。改成 `o` 会让非超管
    //   的请求 500（`column t.workshop_dept_id does not exist`），而超管走 `AND 1 = 1`
    //   完全看不出来 —— 所以别名与锚点对齐，别图省事换字母。
    "SELECT t.id AS object_id, t.object_no, t.object_type, t.source_doc_no, t.lot_no, \
            t.product_id, m.material_code AS product_code, m.material_name AS product_name, \
            t.workshop_dept_id, COALESCE(d.dept_name, t.workshop_dept_name) AS workshop_dept_name, \
            t.plan_qty, t.completed_qty AS cumulative_completed_qty, \
            COALESCE(pc.prior_completed_qty, 0) AS prior_completed_qty, \
            COALESCE(pc.prior_wip_end_qty, 0) AS prior_wip_end_qty, \
            t.wip_qty, t.completion_pct, t.material_input_mode, \
            COALESCE(cc.collected_total, 0) AS collected_total, \
            ci.status AS calc_status, ci.completed_qty AS calc_completed_qty, \
            ci.wip_end_qty AS calc_wip_qty, ci.completion_pct AS calc_completion_pct, \
            ci.equiv_material AS calc_equiv_material, ci.equiv_conversion AS calc_equiv_conversion \
       FROM biz_cost_object t \
       LEFT JOIN biz_material m ON m.id = t.product_id AND m.deleted_at IS NULL \
       LEFT JOIN sys_dept d ON d.id = t.workshop_dept_id AND d.deleted_at IS NULL \
       LEFT JOIN ( \
             SELECT i.cost_object_id, \
                    sum(i.completed_qty) AS prior_completed_qty, \
                    (array_agg(i.wip_end_qty ORDER BY p.period_code DESC))[1] AS prior_wip_end_qty \
               FROM biz_cost_period_item i \
               JOIN biz_cost_period p ON p.id = i.period_id \
              WHERE p.deleted_at IS NULL AND p.status IN ('SETTLED','CLOSED') \
                AND p.period_code < ? \
              GROUP BY i.cost_object_id \
            ) pc ON pc.cost_object_id = t.id \
       LEFT JOIN ( \
             SELECT c.cost_object_id, sum(c.amount) AS collected_total \
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
             OR COALESCE(cc.collected_total, 0) > 0)"
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
fn business_where(filter: &EquivFilter) -> (String, Vec<Value>) {
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
            "约当产量表 SQL 占位符与参数数量不一致（?={placeholders}，参数={}）：参数错位会静默绑错值，\
             拒绝执行。SQL={sql}",
            params.len()
        )));
    }
    Ok(())
}

// ==================== 读 ====================

/// 取本期参与约当产量的全部成本对象（已按数据范围 / 关键字裁剪，未分页）。
///
/// 期间必须已由调用方校验过（`yyyy-MM`）。`batch_id` 是当期活跃批次；
/// 没有批次时传 `Uuid::nil()`（`ci` 一个都 join 不上 ⇒ `stored = None`，表照出）。
pub async fn rows_of(principal: &ScopedPrincipal, filter: &EquivFilter, batch_id: Uuid) -> DbResult<Vec<SourceRow>> {
    let scope_filter = scope::filter_for(principal, EQUIV_TARGET);
    let (biz_sql, biz_params) = business_where(filter);
    let sql = format!("{}{}{} ORDER BY t.object_no", list_sql(), scope_filter.sql(), biz_sql);

    let mut params: Vec<Value> = Vec::with_capacity(3 + scope_filter.params().len() + biz_params.len());
    // ① pc 派生表：前期已结算（`period_code < 本期`）
    params.push(Value::Text(filter.period_code.clone()));
    // ② cc 派生表：本期归集
    params.push(Value::Text(filter.period_code.clone()));
    // ③ ci：本期活跃批次的计算单
    params.push(Value::from(&batch_id));
    // ④ 数据范围
    params.extend_from_slice(scope_filter.params());
    // ⑤ 关键字
    params.extend(biz_params);

    bind(&sql, &params)?;
    let rows = db().query_sql(sql, params).await?;
    if rows.len() > ROW_GUARD {
        return Err(DbError::InvalidValue(format!(
            "期间 {} 参与约当产量的成本对象超过 {} 个（{} 个）：请用 keyword 收窄，\
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
    let snapshot: Option<String> = row.get_typed("workshop_dept_name")?;
    let calc_status: Option<String> = row.get_typed("calc_status")?;
    let stored = match calc_status {
        None => None,
        Some(status) => Some(dto::StoredCalc {
            status,
            completed_qty: row.get_typed("calc_completed_qty")?,
            wip_qty: row.get_typed("calc_wip_qty")?,
            completion_pct: row.get_typed("calc_completion_pct")?,
            equiv_material: row.get_typed("calc_equiv_material")?,
            equiv_conversion: row.get_typed("calc_equiv_conversion")?,
        }),
    };
    Ok(SourceRow {
        object_id: row.get_typed("object_id")?,
        object_no: row.get_typed("object_no")?,
        object_type: row.get_typed("object_type")?,
        source_doc_no: row.get_typed("source_doc_no")?,
        lot_no: row.get_typed("lot_no")?,
        product_id: row.get_typed("product_id")?,
        product_code: row.get_typed("product_code")?,
        product_name: row.get_typed("product_name")?,
        workshop_dept_id: row.get_typed("workshop_dept_id")?,
        workshop_dept_name: snapshot.filter(|text| !text.is_empty()),
        plan_qty: row.get_typed("plan_qty")?,
        cumulative_completed_qty: row.get_typed("cumulative_completed_qty")?,
        prior_completed_qty: row.get_typed("prior_completed_qty")?,
        prior_wip_end_qty: row.get_typed("prior_wip_end_qty")?,
        wip_qty: row.get_typed("wip_qty")?,
        completion_pct: row.get_typed("completion_pct")?,
        material_input_mode: row.get_typed("material_input_mode")?,
        collected_total: row.get_typed("collected_total")?,
        stored,
    })
}

// ==================== 单测（不连库的部分） ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_pattern_escapes_like_wildcards() {
        assert_eq!(keyword_pattern(None), None);
        assert_eq!(keyword_pattern(Some("   ")), None);
        assert_eq!(keyword_pattern(Some(" CB-1 ")), Some("%CB-1%".to_string()));
        assert_eq!(keyword_pattern(Some("100%_a")), Some("%100\\%\\_a%".to_string()));
    }

    #[test]
    fn sql_placeholder_count_matches_the_documented_order() {
        // 3 个固定参数（前期期间 / 本期期间 / 批次）+ 关键字 5 个；数据范围片段另算
        let sql = list_sql();
        let fixed = sql.matches('?').count();
        assert_eq!(fixed, 3, "派生表 pc / cc 与 ci 各一个 `?`，顺序见模块头");
        let filter = EquivFilter {
            period_code: "2026-11".to_string(),
            keyword: Some("CB".to_string()),
            state: dto::EquivStateFilter::All,
            page: 1,
            page_size: 20,
        };
        let (biz_sql, biz_params) = business_where(&filter);
        assert_eq!(biz_params.len(), 5);
        assert_eq!(biz_sql.matches('?').count(), 5);
        // 行集条件里不许再藏参数（否则顺序说明会骗人）
        assert!(sql.contains("t.wip_qty > 0"));
        assert!(sql.contains("COALESCE(cc.collected_total, 0) > 0"));
    }

    #[test]
    fn table_alias_matches_the_scope_anchor() {
        // 数据范围锚点 BIZ_COST_OBJECT 的 alias 是 "t"：SQL 里换了字母，
        // 非超管的请求就会 500（超管走 AND 1 = 1，测不出来）。这里钉死。
        assert_eq!(EQUIV_TARGET.alias, "t");
        for token in ["FROM biz_cost_object t", "t.workshop_dept_id", "t.deleted_at"] {
            assert!(list_sql().contains(token), "取数 SQL 必须用锚点别名 t：缺 {token}");
        }
        assert!(!list_sql().contains("FROM biz_cost_object o "), "别名不许是 o");
    }

    #[test]
    fn bind_rejects_misaligned_params() {
        let error = bind("SELECT ? , ?", &[Value::Text("x".to_string())]).expect_err("少一个参数必须拒");
        match error {
            DbError::InvalidValue(message) => assert!(message.contains("占位符与参数数量不一致")),
            other => panic!("期望 InvalidValue，实得 {other:?}"),
        }
        assert!(bind("SELECT ?", &[Value::Text("x".to_string())]).is_ok());
    }

    #[test]
    fn period_is_only_used_in_the_two_derived_tables() {
        // 「期间只出现在两个派生表」是刻意的：pc 用 `<`、cc 用 `=`，外层不再筛一遍。
        // 第三处是 pc 里「取最近一期的期末在制」的 ORDER BY（不是筛选条件）。
        let sql = list_sql();
        assert_eq!(sql.matches("period_code").count(), 3);
        assert!(sql.contains("p.period_code < ?"));
        assert!(sql.contains("c.period_code = ?"));
        assert!(sql.contains("ORDER BY p.period_code DESC"), "上期期末在制取最近一期");
    }
}
