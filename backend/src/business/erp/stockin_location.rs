// ============================================================================
// stockin_location.rs —— 收货入库 · 库位解析（P2-4：库位做完整）
//
// 依据：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §5 DC-22（P2）。
//
// 背景（实测）：库位账 `biz_inventory_location` 11 行里 7 行是「未分配」桶，
// 收货明细 0/1 填了库位，前端收货页 0 处 location 字段，默认库位
// （`biz_location.is_default = 1`）在收货路径 0 处被读取 —— 库位退化成
// 「一个巨大的未分配桶」。根因是**两个 id 被混为一谈**：
//
// ```text
// biz_stock_in_item.location_id        = 库位账行 id（biz_inventory_location.id）
//                                        —— 只在过账之后才存在（按物料建账）
// biz_inventory_location.location_id   = 库位字典 id（biz_location.id）
// biz_stock_in_item.location_dict_id   = 库位字典 id（P2-4 新增，**用户选的**）
// ```
//
// 用户在收货表单上能选的是**库位字典**（A-01-03），不是「某物料的库存账行」。
// 所以 P2-4 的口径是：
//
// ```text
// 建单：location_dict_id 存用户选择（校验属于本仓 / 未软删 / 已启用）；
//       不传 → 用该仓默认库位；仍没有 → 两列都空（过账进未分配桶 + 详情给提示）
// 过账：按 (物料, 仓库, location_dict_id) find-or-create 出库位账行，
//       把行 id 回写 location_id → 下游（成本账 / 出库 / 调拨 / 序列号）
//       三处 location_id 的既有语义一个字节都不动
// ```
//
// 本文件只提供**库位主数据与库存账行的解析原语**，不含任何业务编排。
// ============================================================================

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::inventory_writer;

/// `biz_location` 的一行（库位字典 / 主数据）。
#[derive(Debug, Clone)]
pub struct LocationDict {
    /// 主键（`biz_location.id`）。
    pub id: Uuid,
    /// 库位编码（仓库内唯一，如 `A-01-03`）。
    pub location_code: String,
    /// 库位名称（可空）。
    pub location_name: Option<String>,
    /// 所属仓库。
    pub warehouse_id: Uuid,
    /// 是否该仓默认收货库位。
    pub is_default: bool,
    /// 容量上限（同基础单位数量；`None` = 不限）。
    pub capacity: Option<Decimal>,
    /// 启用状态：1 启用 / 0 停用。
    pub status: i16,
    /// 是否已软删。
    pub deleted: bool,
}

impl LocationDict {
    /// 展示用标签：`A-01-03 A区一排三列`。
    pub fn label(&self) -> String {
        match self.location_name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => format!("{} {name}", self.location_code),
            _ => self.location_code.clone(),
        }
    }
}

/// 按主键取库位字典（**不过滤**停用 / 软删，调用方自己判并给出可读文案）。
pub async fn location_dict_of(id: Uuid) -> DbResult<Option<LocationDict>> {
    let rows = db()
        .find_with_params(
            "SELECT id, location_code, location_name, warehouse_id, is_default, capacity, \
                    status, deleted_at \
               FROM biz_location WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(location_dict_from_row).transpose()
}

/// 取某仓库的默认库位（`is_default = 1` 且未软删；每仓最多一个，库侧部分唯一索引兜底）。
pub async fn default_location_of(warehouse_id: Uuid) -> DbResult<Option<LocationDict>> {
    let rows = db()
        .find_with_params(
            "SELECT id, location_code, location_name, warehouse_id, is_default, capacity, \
                    status, deleted_at \
               FROM biz_location \
              WHERE warehouse_id = ?::uuid AND is_default = 1 AND deleted_at IS NULL \
              LIMIT 1",
            vec![Value::from(&warehouse_id)],
        )
        .await?;
    rows.first().map(location_dict_from_row).transpose()
}

/// 同 [`default_location_of`]，但在**给定事务 / 连接**上读（过账路径用）。
pub async fn default_location_in(ex: &impl SqlExecutor, warehouse_id: Uuid) -> DbResult<Option<LocationDict>> {
    let row = ex
        .query_first_sql(
            "SELECT id, location_code, location_name, warehouse_id, is_default, capacity, \
                    status, deleted_at \
               FROM biz_location \
              WHERE warehouse_id = ?::uuid AND is_default = 1 AND deleted_at IS NULL \
              LIMIT 1"
                .to_string(),
            vec![Value::from(&warehouse_id)],
        )
        .await?;
    row.as_ref().map(location_dict_from_row).transpose()
}

/// 库位主数据的展示快照（详情页 / 告警用；一次查一批，避免逐行 N+1）。
#[derive(Debug, Clone)]
pub struct LocationBrief {
    /// 库位编码。
    pub location_code: String,
    /// 库位名称。
    pub location_name: Option<String>,
    /// 容量上限（`None` = 不限）。
    pub capacity: Option<Decimal>,
    /// 是否该仓默认库位。
    pub is_default: bool,
}

impl LocationBrief {
    /// 展示用标签：`A-01-03 A区一排三列`。
    pub fn label(&self) -> String {
        match self.location_name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => format!("{} {name}", self.location_code),
            _ => self.location_code.clone(),
        }
    }
}

/// 批量取库位主数据：`库位字典 id → 展示快照`（详情页一次查完，避免逐行 N+1）。
pub async fn location_briefs_of(ids: &[Uuid]) -> DbResult<HashMap<Uuid, LocationBrief>> {
    let mut map = HashMap::new();
    if ids.is_empty() {
        return Ok(map);
    }
    let mut unique: Vec<Uuid> = ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let placeholders = vec!["?::uuid"; unique.len()].join(",");
    let params: Vec<Value> = unique.iter().map(Value::from).collect();
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT id, location_code, location_name, capacity, is_default \
                   FROM biz_location WHERE id IN ({placeholders})"
            ),
            params,
        )
        .await?;
    for row in rows {
        let id: Uuid = row.get_typed("id")?;
        map.insert(
            id,
            LocationBrief {
                location_code: row.get_typed("location_code")?,
                location_name: row.get_typed("location_name")?,
                capacity: row.get_typed("capacity")?,
                is_default: row.get_typed::<i16>("is_default")? == 1,
            },
        );
    }
    Ok(map)
}

/// 批量：库位账行 id → 它的库位字典 id（旧客户端只给 `location_id` 时译名用）。
pub async fn location_dict_of_rows(ids: &[Uuid]) -> DbResult<HashMap<Uuid, Option<Uuid>>> {
    let mut map = HashMap::new();
    if ids.is_empty() {
        return Ok(map);
    }
    let mut unique: Vec<Uuid> = ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let placeholders = vec!["?::uuid"; unique.len()].join(",");
    let params: Vec<Value> = unique.iter().map(Value::from).collect();
    let rows = db()
        .find_with_params(
            &format!("SELECT id, location_id FROM biz_inventory_location WHERE id IN ({placeholders})"),
            params,
        )
        .await?;
    for row in rows {
        map.insert(row.get_typed("id")?, row.get_typed("location_id")?);
    }
    Ok(map)
}

/// 仓库的展示标签 `(编码, 名称)`（告警文案里点名用）。
pub async fn warehouse_label_of(warehouse_id: Uuid) -> DbResult<Option<(String, String)>> {
    let rows = db()
        .find_with_params(
            "SELECT warehouse_code, warehouse_name FROM biz_warehouse WHERE id = ?::uuid",
            vec![Value::from(&warehouse_id)],
        )
        .await?;
    rows.first()
        .map(|row| {
            Ok((
                row.get_typed::<String>("warehouse_code")?,
                row.get_typed::<String>("warehouse_name")?,
            ))
        })
        .transpose()
}

/// 某库位上**当前的总数量**（该仓库该库位所有物料合计；库位不存在 → 0）。
///
/// 容量预警的取数口径：`biz_location.capacity` 的注释写的是「同基础单位数量」，
/// 即库位容量是按基础单位计的粗粒度上限（不做单位换算）。这里按库位账
/// `SUM(qty)` 取，只用于**提示**，不做拦截（卡面 §三 允许二选一，本卡选预警）。
pub async fn location_usage_in(ex: &impl SqlExecutor, warehouse_id: Uuid, location_dict_id: Uuid) -> DbResult<Decimal> {
    let value: Option<Decimal> = ex
        .query_first_sql(
            "SELECT COALESCE(SUM(qty), 0) AS used FROM biz_inventory_location \
              WHERE warehouse_id = ?::uuid AND location_id = ?::uuid"
                .to_string(),
            vec![Value::from(&warehouse_id), Value::from(&location_dict_id)],
        )
        .await?
        .map(|row| row.get_typed("used"))
        .transpose()?;
    Ok(value.unwrap_or(Decimal::ZERO))
}

/// 库位账行 id ← 库位字典 id（历史明细回填 / 旧客户端展示用）。
pub async fn ledger_row_of_location(
    material_id: Uuid,
    warehouse_id: Uuid,
    location_dict_id: Uuid,
) -> DbResult<Option<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT id FROM biz_inventory_location \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id = ?::uuid",
            vec![
                Value::from(&material_id),
                Value::from(&warehouse_id),
                Value::from(&location_dict_id),
            ],
        )
        .await?;
    rows.first().map(|row| row.get_typed("id")).transpose()
}

/// 库位字典 id ← 库位账行 id（旧客户端只给 `location_id` 时补出「用户意图」那一列）。
pub async fn location_dict_of_ledger_row(row_id: Uuid) -> DbResult<Option<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT location_id FROM biz_inventory_location WHERE id = ?::uuid",
            vec![Value::from(&row_id)],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("location_id"))
        .transpose()
        .map(Option::flatten)
}

/// **find-or-create** 库位账行：`(物料, 仓库, 库位字典)` → `biz_inventory_location.id`。
///
/// 这是 P2-4 的核心原语：过账时按用户选的库位建账（qty 从 0 起，随后的
/// `apply_cost` / `apply_location_qty` 把它加上去），因此**不需要**用户在
/// 收货表单里「先有一条该物料的库存账行」——库位选择才真正可用。
///
/// 冲突目标必须写成三列（`uk_biz_inv_loc … NULLS NOT DISTINCT`）；这里
/// `location_id` 恒非空（走默认库位 / 未分配桶的分支不经过本函数），不会触及 NULL 折叠。
pub async fn ensure_location_row(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_dict_id: Uuid,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    // P2-5A：库位账行 upsert 收口到 [`inventory_writer::ensure_location_row`]。
    inventory_writer::ensure_location_row(ex, material_id, warehouse_id, location_dict_id, now).await
}

/// 过账时把「解析出来的库位账行 id」回写到明细（作废 / 红冲按它原路退回）。
pub async fn set_item_location_id(
    ex: &impl SqlExecutor,
    item_id: Uuid,
    row_id: Uuid,
    now: DateTime<Utc>,
) -> DbResult<()> {
    ex.exec_sql(
        "UPDATE biz_stock_in_item SET location_id = ?::uuid, updated_at = ? WHERE id = ?::uuid".to_string(),
        vec![Value::from(&row_id), Value::from(now), Value::from(&item_id)],
    )
    .await?;
    Ok(())
}

/// 一行明细的**库位解析结果**（供过账路径内部使用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedLocation {
    /// 落到具体库位账行。
    Row(Uuid),
    /// 未分配库位桶（`biz_inventory_location.location_id IS NULL`）。
    Unassigned,
}

fn location_dict_from_row(row: &Row) -> DbResult<LocationDict> {
    let deleted_at: Option<DateTime<Utc>> = row.get_typed("deleted_at")?;
    Ok(LocationDict {
        id: row.get_typed("id")?,
        location_code: row.get_typed("location_code")?,
        location_name: row.get_typed("location_name")?,
        warehouse_id: row.get_typed("warehouse_id")?,
        is_default: row.get_typed::<i16>("is_default")? == 1,
        capacity: row.get_typed("capacity")?,
        status: row.get_typed("status")?,
        deleted: deleted_at.is_some(),
    })
}
