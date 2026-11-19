//! 在制品「完工率录入（按工序）」· 数据层（成本核算 · M5-21）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0470__cost_wip_operation_pct.up.sql`
//! （`biz_cost_object_operation`）+ `V0370`（`biz_cost_object`，对象级完工率回写目标）。
//!
//! # 读什么
//!
//! | 需要什么 | 从哪来 | 怎么取 |
//! |---|---|---|
//! | 对象抬头（编号 / 产出品 / 状态 / 数量 / 当前对象级完工率） | `biz_cost_object` | 主表 |
//! | 产出品编码 / 名称 | `biz_material` | join 实时取名（AGENTS 约定 13：主数据关联取最新名） |
//! | 车间名 | `sys_dept`，落空回落建单快照 | join + `COALESCE` |
//! | 工序行 | `biz_cost_object_operation` | 按 `seq` 升序、只取活行 |
//! | 已结算期间（状态闸） | `biz_cost_period` × `biz_cost_period_item` | 复用 `cost_object_model::settled_periods` |
//!
//! # 数据权限
//!
//! 行按 `biz_cost_object.workshop_dept_id` 裁剪（与归集表 / 约当产量表同一份锚点
//! [`cost_object_model::OBJECT_TARGET`] = `scope::SCOPED_TABLES` 的 `BIZ_COST_OBJECT`）。
//! 工序表自己**没有**车间列，也**不登记** `SCOPED_TABLES`：它的可见性完全跟着对象走，
//! 所以单条入口先用 [`find_visible_object`] 做探针（不可见 ⇒ `None` ⇒ handler 404，
//! 不是 403 —— 列表 / 单条入口都不当「探测口」）。探针之后的工序行读取**不再**重复注入
//! 范围：两处口径打架时（对象可见、工序行被判不可见）会变成「查得到抬头、看不到工序」的
//! 半截页面，比漏一行更难受。
//!
//! # 写什么
//!
//! 三个原语，全部吃 `&impl SqlExecutor`（handler 传事务，回查传 `db()`）：
//!
//! * [`insert_row`]：新增一道工序；
//! * [`update_row`]：按主键改一道工序（`WHERE deleted_at IS NULL` 防并发软删写穿）；
//! * [`soft_delete_row`]：软删一道工序（整表保存里「payload 没给的 seq」走这里）；
//! * [`apply_object_pct`]：把加权汇总写回对象级（`WHERE status='OPEN'` 与产量录入同款 CAS）。
//!
//! # 为什么对象级回写**不**在 model 里做汇总
//!
//! 汇总是纯逻辑（[`dto::rollup_of`]），model 只负责「把算好的数写进去」。
//! 这样单测可以只喂行、不连库，真库用例验的是「保存后对象级 == 汇总值」这条缝。

use std::str::FromStr;

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_wip_pct_dto::{self as dto, OperationSpec};
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::business::shared::scope;

/// 表名（与 V0470 逐字一致）。
pub const OPERATION_TABLE: &str = "biz_cost_object_operation";

/// 数据权限锚点：**复用**登记表常量（`scope::SCOPED_TABLES` 里 `BIZ_COST_OBJECT` 那条）。
///
/// 别 `ScopeTarget::new(...)` 抄一份：抄本会让登记表与代码各自演进，而且
/// `permission::scope_model::tests::d8c_wiring_anchors_exist_in_source` 是按
/// 「登记表锚点常量名 + 同文件 scope 调用」校验接线的，抄本扫不到。
pub const OPERATION_TARGET: scope::ScopeTarget = scope::ScopeTarget::BIZ_COST_OBJECT;

/// 对象抬头的 SELECT（含 join 出来的显示名）。
///
/// 别名固定为 `t`：数据范围片段是 `scope::filter_for(..BIZ_COST_OBJECT)` 产出的，
/// 它按登记表的别名 `t` 写列名，换别名会让片段绑到不存在的列上。
const OBJECT_SQL: &str = "SELECT t.id, t.object_no, t.object_type, t.lot_no, t.status, \
        t.plan_qty, t.completed_qty, t.wip_qty, t.completion_pct, t.material_input_mode, \
        m.material_code AS product_code, m.material_name AS product_name, \
        COALESCE(d.dept_name, t.workshop_dept_name) AS workshop_dept_name \
   FROM biz_cost_object t \
   LEFT JOIN biz_material m ON m.id = t.product_id \
   LEFT JOIN sys_dept d ON d.id = t.workshop_dept_id";

/// 工序行的 SELECT（只取活行，按 seq 升序 —— 出参顺序由服务端定死）。
const ROWS_SQL: &str = "SELECT id, seq, operation_name, weight, completion_pct, remark, updated_at \
      FROM biz_cost_object_operation \
     WHERE cost_object_id = ?::uuid AND deleted_at IS NULL \
     ORDER BY seq ASC, id ASC";

// ==================== 行结构 ====================

/// 成本对象抬头（录入页顶部）。
#[derive(Debug, Clone)]
pub struct ObjectHead {
    /// 主键。
    pub id: Uuid,
    /// 对象号。
    pub object_no: String,
    /// 对象类型。
    pub object_type: String,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 状态。
    pub status: String,
    /// 计划数量。
    pub plan_qty: Decimal,
    /// 累计完工数量。
    pub completed_qty: Decimal,
    /// 期末在制数量。
    pub wip_qty: Decimal,
    /// 对象级完工率。
    pub completion_pct: Decimal,
    /// 投料方式。
    pub material_input_mode: String,
    /// 产出品编码（join 主数据）。
    pub product_code: Option<String>,
    /// 产出品名称（join 主数据）。
    pub product_name: Option<String>,
    /// 车间名（join 现名，落空回落建单快照）。
    pub workshop_dept_name: Option<String>,
}

impl ObjectHead {
    /// 转出参（`gate` 决定能不能改）。
    pub fn view(&self, gate: &dto::EditGate) -> dto::OperationObjectView {
        dto::OperationObjectView {
            id: self.id.to_string(),
            object_no: self.object_no.clone(),
            object_type: self.object_type.clone(),
            product_code: self.product_code.clone(),
            product_name: self.product_name.clone(),
            lot_no: self.lot_no.clone(),
            workshop_dept_name: self.workshop_dept_name.clone(),
            status: self.status.clone(),
            plan_qty: dto::text_of(self.plan_qty),
            completed_qty: dto::text_of(self.completed_qty),
            wip_qty: dto::text_of(self.wip_qty),
            completion_pct: dto::text_of(self.completion_pct),
            material_input_mode: self.material_input_mode.clone(),
            editable: gate.editable(),
            blocked_reason: gate.reason(),
        }
    }
}

/// 工序行（库行）。
#[derive(Debug, Clone)]
pub struct StoredRow {
    /// 主键。
    pub id: Uuid,
    /// 归一化内容。
    pub spec: OperationSpec,
    /// 最后修改时间。
    pub updated_at: DateTime<Utc>,
}

impl StoredRow {
    /// 转出参（`share` / `contribution` 由汇总算出）。
    pub fn view(&self, rollup: Option<&dto::Rollup>) -> dto::OperationRowView {
        let (share, contribution) = match rollup {
            Some(rollup) => (
                dto::fixed_of(dto::share_of(&self.spec, rollup), 4),
                dto::fixed_of(dto::contribution_of(&self.spec, rollup), 4),
            ),
            None => ("0.0000".to_string(), "0.0000".to_string()),
        };
        dto::OperationRowView {
            id: self.id.to_string(),
            seq: self.spec.seq,
            operation_name: self.spec.operation_name.clone(),
            weight: dto::text_of(self.spec.weight),
            completion_pct: dto::text_of(self.spec.completion_pct),
            share_pct: share,
            contribution_pct: contribution,
            remark: self.spec.remark.clone(),
            updated_at: self.updated_at.to_rfc3339(),
        }
    }
}

/// 由查询行构造对象抬头。
fn head_of(row: &Row) -> DbResult<ObjectHead> {
    Ok(ObjectHead {
        id: row.get_typed("id")?,
        object_no: row.get_typed("object_no")?,
        object_type: row.get_typed("object_type")?,
        lot_no: row.get_typed("lot_no")?,
        status: row.get_typed("status")?,
        plan_qty: row.get_typed("plan_qty")?,
        completed_qty: row.get_typed("completed_qty")?,
        wip_qty: row.get_typed("wip_qty")?,
        completion_pct: row.get_typed("completion_pct")?,
        material_input_mode: row.get_typed("material_input_mode")?,
        product_code: row.get_typed("product_code")?,
        product_name: row.get_typed("product_name")?,
        workshop_dept_name: row.get_typed("workshop_dept_name")?,
    })
}

/// 由查询行构造工序行。
fn row_of(row: &Row) -> DbResult<StoredRow> {
    Ok(StoredRow {
        id: row.get_typed("id")?,
        spec: OperationSpec {
            seq: row.get_typed("seq")?,
            operation_name: row.get_typed("operation_name")?,
            weight: row.get_typed("weight")?,
            completion_pct: row.get_typed("completion_pct")?,
            remark: row.get_typed("remark")?,
        },
        updated_at: row.get_typed("updated_at")?,
    })
}

// ==================== 读 ====================

/// 按主键取**本用户可见**的对象抬头（数据范围在此注入；不可见与不存在同返回 `None`）。
///
/// 单条入口的 404 语义就在这里：不是「403 告诉你存在但看不见」，而是根本查不到 ——
/// 与成本对象列表 / 约当产量表的口径一致。
pub async fn find_visible_object(principal: &ScopedPrincipal, id: Uuid) -> DbResult<Option<ObjectHead>> {
    let scope_filter = scope::filter_for(principal, OPERATION_TARGET);
    let sql = format!(
        "{OBJECT_SQL} WHERE t.id = ?::uuid AND t.deleted_at IS NULL {}",
        scope_filter.sql()
    );
    let mut params: Vec<Value> = Vec::with_capacity(1 + scope_filter.params().len());
    params.push(Value::from(&id));
    params.extend_from_slice(scope_filter.params());
    let rows = db().query_sql(sql, params).await?;
    rows.first().map(head_of).transpose()
}

/// 事务内锁对象抬头（`FOR UPDATE OF t`：只锁 `biz_cost_object` 这一张，不锁 join 表）。
///
/// 串行化点：同一个批的「整表保存」在 `FOR UPDATE` 上排队，后到的那次读到的是前一次
/// 提交后的工序行（不会拿旧快照算出一份覆盖对方的汇总）。
pub async fn lock_object(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ObjectHead>> {
    let sql = format!("{OBJECT_SQL} WHERE t.id = ?::uuid AND t.deleted_at IS NULL FOR UPDATE OF t");
    let rows = ex.query_sql(sql, vec![Value::from(&id)]).await?;
    rows.first().map(head_of).transpose()
}

/// 某对象的全部工序行（**调用方必须先做过 [`find_visible_object`] / [`lock_object`] 探针**）。
///
/// # SCOPE-EXEMPT
///
/// 按 `cost_object_id` 取工序行：可见性已由对象探针判定，这里不再重复注入范围
/// （重复注入会让「对象可见、工序行被判不可见」变成半截页面）。
// SCOPE-EXEMPT: 工序行按 cost_object_id 取（单对象录入面，可见性由对象探针判定）
pub async fn rows_of(ex: &impl SqlExecutor, object_id: Uuid) -> DbResult<Vec<StoredRow>> {
    let rows = ex
        .query_sql(ROWS_SQL.to_string(), vec![Value::from(&object_id)])
        .await?;
    rows.iter().map(row_of).collect()
}

// ==================== 写 ====================

/// 新增一道工序（返回主键）。
pub async fn insert_row(
    ex: &impl SqlExecutor,
    object_id: Uuid,
    spec: &OperationSpec,
    operator: Option<Uuid>,
) -> DbResult<Uuid> {
    let id = Uuid::now_v7();
    ex.exec_sql(
        "INSERT INTO biz_cost_object_operation \
             (id, cost_object_id, seq, operation_name, weight, completion_pct, remark, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?::uuid, ?, ?, ?::text::numeric, ?::text::numeric, ?, now(), now(), \
                 NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::from(&object_id),
            Value::I32(spec.seq),
            Value::Text(spec.operation_name.clone()),
            Value::Text(spec.weight.to_string()),
            Value::Text(spec.completion_pct.to_string()),
            opt_text(spec.remark.as_deref()),
            operator_text(operator),
            operator_text(operator),
        ],
    )
    .await?;
    Ok(id)
}

/// 按主键改一道工序（返回受影响行数；`0` = 已被并发软删 / 不属于该对象）。
pub async fn update_row(
    ex: &impl SqlExecutor,
    object_id: Uuid,
    id: Uuid,
    spec: &OperationSpec,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_cost_object_operation \
            SET seq = ?, operation_name = ?, weight = ?::text::numeric, \
                completion_pct = ?::text::numeric, remark = ?, updated_at = now(), \
                update_by = NULLIF(?::text,'')::uuid \
          WHERE id = ?::uuid AND cost_object_id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::I32(spec.seq),
            Value::Text(spec.operation_name.clone()),
            Value::Text(spec.weight.to_string()),
            Value::Text(spec.completion_pct.to_string()),
            opt_text(spec.remark.as_deref()),
            operator_text(operator),
            Value::from(&id),
            Value::from(&object_id),
        ],
    )
    .await
}

/// 软删一道工序（整表保存里「payload 没给的 seq」）。
pub async fn soft_delete_row(
    ex: &impl SqlExecutor,
    object_id: Uuid,
    id: Uuid,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_cost_object_operation \
            SET deleted_at = now(), updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
          WHERE id = ?::uuid AND cost_object_id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![operator_text(operator), Value::from(&id), Value::from(&object_id)],
    )
    .await
}

/// 把加权汇总写回对象级完工率（CAS：`WHERE status = 'OPEN'`）。
///
/// 与 `cost_object_model::apply_output` 同款：`affected = 0` 说明对象在本次事务期间
/// 被关批 / 作废（并发抢跑），handler 转 409 而不是把已关批的批改回可写。
pub async fn apply_object_pct(
    ex: &impl SqlExecutor,
    object_id: Uuid,
    pct: Decimal,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_cost_object \
            SET completion_pct = ?::text::numeric, updated_at = now(), \
                update_by = NULLIF(?::text,'')::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL AND status = 'OPEN'"
            .to_string(),
        vec![
            Value::Text(pct.to_string()),
            operator_text(operator),
            Value::from(&object_id),
        ],
    )
    .await
}

// ==================== 小工具 ====================

/// `Option<&str>` → 可空文本参数。
fn opt_text(value: Option<&str>) -> Value {
    match value {
        Some(text) => Value::Text(text.to_string()),
        None => Value::Null,
    }
}

/// 操作人 → 文本参数（空串由 SQL 里的 `NULLIF` 转成 NULL）。
fn operator_text(operator: Option<Uuid>) -> Value {
    Value::Text(operator.map(|id| id.to_string()).unwrap_or_default())
}

/// 十进制解析（真库用例里从字符串读夹具用；生产代码不走这里）。
pub fn parse_decimal(raw: &str) -> DbResult<Decimal> {
    Decimal::from_str(raw).map_err(|_| DbError::InvalidValue(format!("不是合法十进制：{raw}")))
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_table_name_matches_migration() {
        assert_eq!(OPERATION_TABLE, "biz_cost_object_operation");
    }

    #[test]
    fn scope_anchor_is_the_registered_cost_object_target() {
        // 抄一份 ScopeTarget 会让登记表与代码各自演进；这里钉住「复用的就是登记表那条」
        assert_eq!(OPERATION_TARGET.table, "biz_cost_object");
        assert!(
            OPERATION_TARGET.is_scoped(),
            "费用 / 成本对象是车间级数据，必须接数据范围"
        );
    }

    #[test]
    fn rows_sql_orders_by_seq_and_skips_deleted() {
        assert!(ROWS_SQL.contains("ORDER BY seq ASC"), "出参顺序必须由服务端定死");
        assert!(ROWS_SQL.contains("deleted_at IS NULL"), "软删行不是工序");
    }

    #[test]
    fn object_sql_uses_the_registered_alias() {
        // 数据范围片段按别名 `t` 写列名，换别名会绑到不存在的列上
        assert!(OBJECT_SQL.contains("FROM biz_cost_object t"));
        assert!(OBJECT_SQL.contains("t.workshop_dept_id"));
    }

    #[test]
    fn parse_decimal_reports_bad_literals() {
        assert_eq!(parse_decimal("12.5").expect("合法"), Decimal::from_str("12.5").unwrap());
        assert!(parse_decimal("abc").is_err(), "非法字面量必须报错而不是给 0");
    }
}
