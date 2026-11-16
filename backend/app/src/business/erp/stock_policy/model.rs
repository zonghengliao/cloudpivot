//! 水位设置数据层（M2-13）：`biz_material_stock_policy` 的读 / 写，以及**生效水位**的唯一读数入口。
//!
//! 卡：t_d997ada8（M2-13【M2·仓库】安全库存：字段（物料 × 仓库）最低/最高水位）；
//! 表结构唯一依据：`backend/migrations/business/V0428__erp_stock_policy_water_level.up.sql`。
//!
//! # 三条铁律（与 V0428 的建表注释一一对应）
//!
//! 1. **软删不物理删**：`deleted_at IS NULL` 贯穿所有查询；删掉一格 = 该格回退物料级
//!    `safety_stock`（不是「不判」）。
//! 2. **唯一键是「存活行」的部分唯一索引**：`uk_biz_msp_mat_wh (material_id, warehouse_id)
//!    WHERE deleted_at IS NULL` ⇒ 软删后可以重新设置同一格；handler 先查一次给人话 409，
//!    索引是并发兜底。
//! 3. **数量一律 `NUMERIC(18,4)`，绑参走 `?::text::numeric`**（驱动不吃 `Decimal` 直接绑）；
//!    `min_qty >= 0` / `max_qty > min_qty` / `status IN (0,1)` 的 CHECK 在 DDL 里，本层不重复判。
//!
//! # 设置表里**不存结存**
//!
//! 本文件一个 `biz_inventory` 都不查：水位设置只回答「阈值是多少」，现存量永远现算
//! （缓存结存 = 一份必漂移的副本，而「低于水位」恰恰要拿最新结存判）。M2-14 的检查、
//! M2-15 的补货建议都自己现算结存，再调本文件的 [`effective_of`] 拿阈值。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_material_stock_policy 是全局配置表（V0428 无 dept_id / 归属人列），
//!               数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:stock-policy:*
//!               权限码控制。分页走 model_base::page_of → scope::paginate_unscoped（闸①口径）。
//! ```

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model_base::{find_by_id, page_of};
use crate::business::erp::stock_policy::dto::{
    EffectiveMin, PolicyLevels, PolicyWrite, resolve_max_qty, resolve_min_qty,
};
use cloudpivot_kernel::business::shared::scope;
use cloudpivot_kernel::utils;

/// 分页口径（默认 20 / 上限 200 / `page_size = 0` 回落）复用底座。
pub use crate::business::erp::model_base::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, normalize_page};

// ==================== 列清单 / 排序 / 分页锚点 ====================

/// 设置行查询列（`p` = 设置行，`m` = 物料，`u` = 单位，`w` = 仓库）。
const POLICY_SELECT: &str = "SELECT p.id, p.material_id, m.material_code, m.material_name, m.spec, \
     u.unit_name, p.warehouse_id, w.warehouse_code, w.warehouse_name, \
     p.min_qty, p.max_qty, p.status, p.remark, p.created_at, p.updated_at \
     FROM biz_material_stock_policy p \
     LEFT JOIN biz_material m ON m.id = p.material_id \
     LEFT JOIN biz_unit u ON u.id = m.unit_id \
     LEFT JOIN biz_warehouse w ON w.id = p.warehouse_id";

/// 分页锚点（全局配置，不参与数据范围）。
pub const POLICY_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_material_stock_policy");

/// 列表排序：物料编码 → 仓库编码 → id（全序，翻页不换行）。
///
/// 同物料的多仓设置挨着看（「这个物料在哪些仓设了水位」是最常见的读法）；
/// 物料软删 → 编码为 `NULL`，`NULLS LAST` 把它们收到末尾而不是排到最前。
const POLICY_ORDER: &str = "ORDER BY m.material_code ASC NULLS LAST, p.material_id ASC, \
     w.warehouse_code ASC NULLS LAST, p.warehouse_id ASC, p.id ASC";

// ==================== 行结构 ====================

/// `biz_material_stock_policy` 一行（**带物料 / 仓库名称**：列表页直接显示，不用前端二次映射）。
#[derive(Debug, Clone)]
pub struct PolicyRow {
    /// 主键。
    pub id: Uuid,
    /// 物料 id。
    pub material_id: Uuid,
    /// 物料编码（物料软删后为 `NULL`：`LEFT JOIN` 不吞行）。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 规格型号。
    pub spec: Option<String>,
    /// 计量单位名。
    pub unit_name: Option<String>,
    /// 仓库 id。
    pub warehouse_id: Uuid,
    /// 仓库编码。
    pub warehouse_code: Option<String>,
    /// 仓库名称。
    pub warehouse_name: Option<String>,
    /// 最低水位。
    pub min_qty: Decimal,
    /// 最高水位（`None` = 未设）。
    pub max_qty: Option<Decimal>,
    /// 状态（`1` 启用 / `0` 停用）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl PolicyRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            material_id: row.get_typed("material_id")?,
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            spec: row.get_typed("spec")?,
            unit_name: row.get_typed("unit_name")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            warehouse_code: row.get_typed("warehouse_code")?,
            warehouse_name: row.get_typed("warehouse_name")?,
            min_qty: row.get_typed("min_qty")?,
            max_qty: row.get_typed("max_qty")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 折算成下游用的三列（不含结存）。
    pub fn levels(&self) -> PolicyLevels {
        PolicyLevels::new(self.min_qty, self.max_qty, self.status)
    }
}

/// 一格**生效水位**（策略 + 物料级回退的结果，下游 M2-14 / M2-15 直接用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveLevels {
    /// 生效最低水位 + 来源。
    pub min: EffectiveMin,
    /// 生效最高水位（`None` = 不判超储）。
    pub max: Option<Decimal>,
    /// 命中的策略行（`None` = 该格没有设置，或设置被停用/软删）。
    ///
    /// 回显用：`min.source = MATERIAL` 且 `policy = Some(..)` 就是「这格设过但停用了」。
    pub policy: Option<PolicyLevels>,
}

// ==================== 查询 ====================

/// 分页（`material_id` / `warehouse_id` / `status` 精确，`keyword` 命中物料或仓库编码 / 名称）。
pub async fn policy_page(
    material_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    status: Option<i16>,
    keyword: Option<&str>,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<PolicyRow>, i64)> {
    let mut sql = format!("{POLICY_SELECT} WHERE p.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(id) = material_id {
        sql.push_str(" AND p.material_id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(id) = warehouse_id {
        sql.push_str(" AND p.warehouse_id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(status) = status {
        sql.push_str(" AND p.status = ?::text::smallint");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(pattern) = utils::like_pattern(keyword) {
        sql.push_str(
            " AND (m.material_code LIKE ? OR m.material_name LIKE ? OR w.warehouse_code LIKE ? OR w.warehouse_name LIKE ?)",
        );
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    page_of(
        POLICY_TARGET,
        sql,
        params,
        POLICY_ORDER,
        page,
        page_size,
        PolicyRow::from_row,
    )
    .await
}

/// 按主键取设置行（软删行不返回）。
pub async fn policy_find_by_id(id: Uuid) -> DbResult<Option<PolicyRow>> {
    find_by_id(
        "(SELECT p.id, p.material_id, m.material_code, m.material_name, m.spec, u.unit_name, \
          p.warehouse_id, w.warehouse_code, w.warehouse_name, p.min_qty, p.max_qty, p.status, \
          p.remark, p.created_at, p.updated_at \
          FROM biz_material_stock_policy p \
          LEFT JOIN biz_material m ON m.id = p.material_id \
          LEFT JOIN biz_unit u ON u.id = m.unit_id \
          LEFT JOIN biz_warehouse w ON w.id = p.warehouse_id) p",
        "id, material_id, material_code, material_name, spec, unit_name, warehouse_id, \
         warehouse_code, warehouse_name, min_qty, max_qty, status, remark, created_at, updated_at",
        id,
        false,
        PolicyRow::from_row,
    )
    .await
}

/// 按 物料 × 仓库 取设置行（软删行不返回；upsert 的「建 or 改」判据）。
pub async fn policy_find_by_key(material_id: Uuid, warehouse_id: Uuid) -> DbResult<Option<PolicyRow>> {
    let rows = db()
        .find_with_params(
            &format!(
                "{POLICY_SELECT} WHERE p.material_id = ?::uuid AND p.warehouse_id = ?::uuid \
                 AND p.deleted_at IS NULL"
            ),
            vec![Value::from(&material_id), Value::from(&warehouse_id)],
        )
        .await?;
    rows.first().map(PolicyRow::from_row).transpose()
}

/// 该格是否已被别的存活行占用（`exclude` = upsert 时排除自己）。
///
/// 与 `uk_biz_msp_mat_wh` 同口径（只看未删行）；handler 先查一次给人话 409，
/// 并发下由索引兜底（[`insert`] 的 `UniqueViolation` 会被 handler 翻成同一个 409）。
pub async fn policy_taken(material_id: Uuid, warehouse_id: Uuid, exclude: Option<Uuid>) -> DbResult<bool> {
    let mut sql = String::from(
        "SELECT COUNT(*) FROM biz_material_stock_policy \
         WHERE deleted_at IS NULL AND material_id = ?::uuid AND warehouse_id = ?::uuid",
    );
    let mut params = vec![Value::from(&material_id), Value::from(&warehouse_id)];
    if let Some(id) = exclude {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&id));
    }
    let count: i64 = db().scalar_with_params(&sql, params).await?;
    Ok(count > 0)
}

/// 一格**生效水位**：策略行（启用才生效）→ 物料级 `safety_stock` 回退。
///
/// **这是下游（M2-14 水位检查 / M2-15 补货建议）唯一的读数入口**：三级回退只在
/// [`dto::resolve_min_qty`] 里写一次，本函数负责把它和数据库里的两处事实对上
/// （策略表一行 + 物料表一个数）。物料不存在 / 已软删 → 按「无策略、物料级 0」返回，
/// 不报错（调用方按「不判水位」处理，这与历史数据里删掉物料后的存留设置一致）。
pub async fn effective_of(material_id: Uuid, warehouse_id: Uuid) -> DbResult<EffectiveLevels> {
    let rows = db()
        .find_with_params(
            "SELECT p.min_qty, p.max_qty, p.status, m.safety_stock \
               FROM biz_material m \
               LEFT JOIN biz_material_stock_policy p \
                 ON p.material_id = m.id AND p.warehouse_id = ?::uuid AND p.deleted_at IS NULL \
              WHERE m.id = ?::uuid AND m.deleted_at IS NULL",
            vec![Value::from(&warehouse_id), Value::from(&material_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(EffectiveLevels {
            min: resolve_min_qty(None, Decimal::ZERO),
            max: None,
            policy: None,
        });
    };
    let material_safety: Decimal = row.get_typed("safety_stock")?;
    // 策略行可能不存在（LEFT JOIN）→ 三列都是 NULL，此时 `policy = None`
    let policy = match (
        row.get_typed::<Option<Decimal>>("min_qty")?,
        row.get_typed::<Option<Decimal>>("max_qty")?,
        row.get_typed::<Option<i16>>("status")?,
    ) {
        (Some(min_qty), max_qty, Some(status)) => Some(PolicyLevels::new(min_qty, max_qty, status)),
        _ => None,
    };
    Ok(EffectiveLevels {
        min: resolve_min_qty(policy.as_ref(), material_safety),
        max: resolve_max_qty(policy.as_ref()),
        policy,
    })
}

/// 物料是否存在（软删不算）。
pub async fn material_exists(material_id: Uuid) -> DbResult<bool> {
    let count: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM biz_material WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&material_id)],
        )
        .await?;
    Ok(count > 0)
}

/// 仓库是否存在（软删不算）。
pub async fn warehouse_exists(warehouse_id: Uuid) -> DbResult<bool> {
    let count: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM biz_warehouse WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&warehouse_id)],
        )
        .await?;
    Ok(count > 0)
}

// ==================== 写入 ====================

/// 新建一格设置（`id` 由 handler 生成，便于回读与审计）。
pub async fn policy_insert(
    id: Uuid,
    material_id: Uuid,
    warehouse_id: Uuid,
    write: &PolicyWrite,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    db().execute_with_params(
        "INSERT INTO biz_material_stock_policy \
             (id, material_id, warehouse_id, min_qty, max_qty, status, remark, \
              created_at, updated_at, create_by, update_by) \
             VALUES (?::uuid, ?::uuid, ?::uuid, ?::text::numeric, NULLIF(?::text,'')::numeric, \
                     ?::text::smallint, ?, now(), now(), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)",
        vec![
            Value::from(&id),
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::Text(write.min_qty.to_string()),
            Value::Text(write.max_qty.map(|qty| qty.to_string()).unwrap_or_default()),
            Value::Text(write.status.to_string()),
            Value::from(write.remark.clone()),
            uuid_text(operator),
            uuid_text(operator),
        ],
    )
    .await
}

/// 覆盖一格设置（**键不可改**：物料 / 仓库是 `WHERE` 条件，不在 `SET` 里）。
pub async fn policy_update(id: Uuid, write: &PolicyWrite, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_material_stock_policy SET min_qty = ?::text::numeric, \
             max_qty = NULLIF(?::text,'')::numeric, status = ?::text::smallint, remark = ?, \
             updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::Text(write.min_qty.to_string()),
            Value::Text(write.max_qty.map(|qty| qty.to_string()).unwrap_or_default()),
            Value::Text(write.status.to_string()),
            Value::from(write.remark.clone()),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删一格设置（删了 = 该格回退物料级 `safety_stock`）。
pub async fn policy_soft_delete(id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_material_stock_policy SET deleted_at = now(), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![uuid_text(operator), Value::from(&id)],
    )
    .await
}

/// 可选 uuid 的绑参文本：`None` → 空串（SQL 侧 `NULLIF(?::text,'')::uuid` 转 NULL）。
fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

// ==================== 单测（纯逻辑，不碰库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 分页锚点必须是「不参与数据权限」的（全局配置表）——改成 `new(...)` 就说明有人
    /// 想给水位设置加数据范围，那时 `paginate_unscoped` 会直接报错，本断言先挡住。
    #[test]
    fn policy_target_is_unscoped() {
        assert_eq!(POLICY_TARGET.table, "biz_material_stock_policy");
        assert!(!POLICY_TARGET.is_scoped(), "水位设置是全局配置：不参与数据范围注入");
        println!("[M2-13] 分页锚点 biz_material_stock_policy 为 unscoped ✓");
    }

    /// 排序与筛选拼装：`status` 绑参是 `?::text::smallint`（驱动不吃整数直接绑）。
    #[test]
    fn order_and_cast_shape_are_pinned() {
        assert!(POLICY_ORDER.starts_with("ORDER BY m.material_code ASC NULLS LAST"));
        assert!(POLICY_ORDER.contains("p.id ASC"), "必须以 id 收全序（翻页不换行）");
        println!("[M2-13] 列表全序：物料编码 → 物料 → 仓库编码 → 仓库 → id ✓");
    }
}
