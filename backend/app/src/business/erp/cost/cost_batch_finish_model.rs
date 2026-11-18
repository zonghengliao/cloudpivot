//! 分批法「完工批次结转（完工转出）」· 取数层（成本核算 · M5-04）。
//!
//! 本文件只做读原语与一个写入原语，不含业务判据（判据在
//! [`crate::business::erp::cost::cost_batch_finish_dto`]，编排在
//! [`crate::business::erp::cost::cost_batch_finish`]）。
//!
//! # 本文件真正新增的读
//!
//! | 读 | 干什么 |
//! |---|---|
//! | [`accumulated_totals`] | 本批**全期间**累计归集（料 / 工 / 费 + 行数） |
//! | [`transferred_totals`] | 本批已结转（历史结转单之和 + 次数） |
//! | [`history`] | 历史结转单（新 → 旧，带操作人姓名） |
//! | [`settled_items`] | 本批有几行计算单已 `SETTLED`（账已走则不许再补工作底稿） |
//! | [`lock_object`] | 事务内锁住成本对象行（`FOR UPDATE`）并取「结转时点」的产量口径 |
//!
//! 其余一律复用：批次抬头与数据范围探针用 M5-02 的
//! [`cost_collect_sheet_model::find_visible_batch`]（同一个 404 口径）；
//! 期间批次闸门用 M5-03 的 [`cost_batch_collect_model::active_period_batch`]；
//! 草稿工时 / 未归集领料 / 未过账领料 / 期间锁定这四条事实用 `cost_object_model::close_facts`
//! —— **归集侧与关批侧对「成本还能不能变」必须只有一套判据**，抄一份就是两份口径各自演进。
//!
//! # 为什么锁成本对象行（`FOR UPDATE`）
//!
//! 「本次结转 = 累计 − 已结转」是**读后写**：两个并发请求都读到「已结转 0」，就会各写一张
//! 全额结转单（成本被转两次）。成本对象行是天然的串行点（同一批的结转本来就不该并行），
//! 锁它比锁结转单更早生效（第一次结转时结转单还不存在，锁不住）。
//! 第二次进来的请求在锁上等，拿到锁后重算差额 = 0 ⇒ 幂等空转，不写空单。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_batch_finish_dto::{self as dto, Amounts, FinishHistoryRes};
use crate::business::erp::cost::cost_collect_sheet_dto::SheetBatchRes;
use crate::business::erp::cost::cost_collect_sheet_model;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;

// ==================== 成本对象（事务内锁 + 结转时点快照） ====================

/// 结转时点从 **`biz_cost_object` 里读到的**生产口径（不回显请求里的数字）。
#[derive(Debug, Clone)]
pub struct ObjectSnapshot {
    /// 对象 id。
    pub id: Uuid,
    /// 对象号。
    pub object_no: String,
    /// 状态 `OPEN` / `CLOSED` / `CANCELLED`。
    pub status: String,
    /// 产出品。
    pub product_id: Uuid,
    /// 产出批号（写进结转单：这批货是哪一批）。
    pub lot_no: Option<String>,
    /// 计划数量。
    pub plan_qty: Decimal,
    /// 累计完工数量（**结转口径的完工数**）。
    pub completed_qty: Decimal,
    /// 期末在制数量（> 0 一律不许结转，分批法不按约当分摊）。
    pub wip_qty: Decimal,
}

/// 事务外读成本对象快照（试算用；不加锁 —— 试算不写库，锁它只会挡别人的正常操作）。
pub async fn read_object(ex: &impl SqlExecutor, object_id: Uuid) -> DbResult<Option<ObjectSnapshot>> {
    let sql = "SELECT id, object_no, status, product_id, lot_no, plan_qty, completed_qty, wip_qty \
                 FROM biz_cost_object \
                WHERE id = ?::uuid AND deleted_at IS NULL";
    snapshot_of(ex, sql, object_id).await
}

/// 事务内锁住成本对象行并取快照（`SELECT … FOR UPDATE`）。
///
/// 不存在 / 已软删 ⇒ `None`（调用方转 404 或 409）。
pub async fn lock_object(ex: &impl SqlExecutor, object_id: Uuid) -> DbResult<Option<ObjectSnapshot>> {
    let sql = "SELECT id, object_no, status, product_id, lot_no, plan_qty, completed_qty, wip_qty \
                 FROM biz_cost_object \
                WHERE id = ?::uuid AND deleted_at IS NULL \
                  FOR UPDATE";
    snapshot_of(ex, sql, object_id).await
}

/// 同一份快照装配（锁 / 不锁只差 SQL 尾部的 `FOR UPDATE`）。
async fn snapshot_of(ex: &impl SqlExecutor, sql: &str, object_id: Uuid) -> DbResult<Option<ObjectSnapshot>> {
    let rows = ex.query_sql(sql.to_string(), vec![Value::from(&object_id)]).await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(ObjectSnapshot {
        id: row.get_typed("id")?,
        object_no: row.get_typed("object_no")?,
        status: row.get_typed("status")?,
        product_id: row.get_typed("product_id")?,
        lot_no: row.get_typed("lot_no")?,
        plan_qty: row.get_typed("plan_qty")?,
        completed_qty: row.get_typed("completed_qty")?,
        wip_qty: row.get_typed("wip_qty")?,
    }))
}

// ==================== 累计归集（全期间） ====================
/// 本批**全期间**的累计归集（料 / 工 / 费金额 + 行数）。
///
/// 一条 `FILTER` 聚合拿三项：出参的「累计归集」与归集表 / 明细必须是同一个数。
/// 只算活行（`deleted_at IS NULL`）—— 软删行是「来源不再合规」的留痕，不算成本。
pub async fn accumulated_totals(ex: &impl SqlExecutor, object_id: Uuid) -> DbResult<(Amounts, i64)> {
    let sql = "SELECT COALESCE(sum(amount) FILTER (WHERE element = ?), 0) AS material_amount, \
                      COALESCE(sum(amount) FILTER (WHERE element = ?), 0) AS labor_amount, \
                      COALESCE(sum(amount) FILTER (WHERE element = ?), 0) AS overhead_amount, \
                      count(*) AS rows \
                 FROM biz_cost_collect \
                WHERE cost_object_id = ?::uuid AND deleted_at IS NULL";
    let row = ex
        .query_first_sql(
            sql.to_string(),
            vec![
                Value::Text(dto::ELEMENT_MATERIAL.to_string()),
                Value::Text(dto::ELEMENT_LABOR.to_string()),
                Value::Text(dto::ELEMENT_OVERHEAD.to_string()),
                Value::from(&object_id),
            ],
        )
        .await?
        .ok_or_else(|| DbError::InvalidValue("累计归集聚合没有返回行（聚合必有一行）".to_string()))?;
    Ok((
        Amounts {
            material: row.get_typed("material_amount")?,
            labor: row.get_typed("labor_amount")?,
            overhead: row.get_typed("overhead_amount")?,
        },
        row.get_typed("rows")?,
    ))
}

// ==================== 已结转（历史结转单） ====================

/// 已结转口径：三要素合计 + 结转次数 + 最近一次结转时间。
#[derive(Debug, Clone, Default)]
pub struct Transferred {
    /// 三要素合计。
    pub amounts: Amounts,
    /// 已有几张结转单（= 已结转次数）。
    pub times: i32,
    /// 最近一次结转时间。
    pub last_at: Option<DateTime<Utc>>,
}

/// 本批已结转（只算活行：软删的结转单视为「这张作废了」）。
pub async fn transferred_totals(ex: &impl SqlExecutor, object_id: Uuid) -> DbResult<Transferred> {
    let sql = "SELECT COALESCE(sum(material_amount), 0) AS material_amount, \
                      COALESCE(sum(labor_amount), 0) AS labor_amount, \
                      COALESCE(sum(overhead_amount), 0) AS overhead_amount, \
                      count(*) AS times, \
                      max(created_at) AS last_at \
                 FROM biz_cost_batch_finish \
                WHERE object_id = ?::uuid AND deleted_at IS NULL";
    let rows = ex.query_sql(sql.to_string(), vec![Value::from(&object_id)]).await?;
    let Some(row) = rows.first() else {
        return Ok(Transferred::default());
    };
    let times: i64 = row.get_typed("times")?;
    Ok(Transferred {
        amounts: Amounts {
            material: row.get_typed("material_amount")?,
            labor: row.get_typed("labor_amount")?,
            overhead: row.get_typed("overhead_amount")?,
        },
        times: i32::try_from(times).unwrap_or(i32::MAX),
        last_at: row.get_typed("last_at")?,
    })
}

/// 历史结转单（新 → 旧）。
pub async fn history(ex: &impl SqlExecutor, object_id: Uuid) -> DbResult<Vec<FinishHistoryRes>> {
    let sql = "SELECT f.id, f.transfer_seq, f.period_code, f.finished_qty, f.unit_cost, \
                      f.material_amount, f.labor_amount, f.overhead_amount, \
                      f.created_at, f.remark, \
                      COALESCE(uc.real_name, uc.username) AS created_by_name \
                 FROM biz_cost_batch_finish f \
                 LEFT JOIN sys_user uc ON uc.id = f.create_by \
                WHERE f.object_id = ?::uuid AND f.deleted_at IS NULL \
                ORDER BY f.transfer_seq DESC, f.id DESC";
    let rows = ex.query_sql(sql.to_string(), vec![Value::from(&object_id)]).await?;
    rows.iter()
        .map(|row| {
            let id: Uuid = row.get_typed("id")?;
            let finished_qty: Decimal = row.get_typed("finished_qty")?;
            let unit_cost: Decimal = row.get_typed("unit_cost")?;
            let created_at: DateTime<Utc> = row.get_typed("created_at")?;
            let amounts = Amounts {
                material: row.get_typed("material_amount")?,
                labor: row.get_typed("labor_amount")?,
                overhead: row.get_typed("overhead_amount")?,
            };
            Ok(FinishHistoryRes {
                id: id.to_string(),
                transfer_seq: row.get_typed("transfer_seq")?,
                period_code: row.get_typed("period_code")?,
                finished_qty: dto::quantity_text(finished_qty),
                unit_cost: dto::unit_cost_text(unit_cost),
                amounts: dto::AmountsRes::of(&amounts),
                created_at: created_at.to_rfc3339(),
                created_by_name: row.get_typed("created_by_name")?,
                remark: row.get_typed("remark")?,
            })
        })
        .collect()
}

// ==================== 已随期间结算结转（账已走） ====================

/// 本批已 `SETTLED` 的计算单行数（> 0 ⇒ 账务已结转，不能再补工作底稿）。
pub async fn settled_items(ex: &impl SqlExecutor, object_id: Uuid) -> DbResult<i64> {
    let row = ex
        .query_first_sql(
            "SELECT count(*) AS settled FROM biz_cost_period_item \
              WHERE cost_object_id = ?::uuid AND status = 'SETTLED'"
                .to_string(),
            vec![Value::from(&object_id)],
        )
        .await?
        .ok_or_else(|| DbError::InvalidValue("已结算计算单计数没有返回行（聚合必有一行）".to_string()))?;
    row.get_typed("settled")
}

// ==================== 写入 ====================

/// 一条待写的结转单。
#[derive(Debug, Clone)]
pub struct FinishInsert<'a> {
    /// 成本对象。
    pub object_id: Uuid,
    /// 对象号快照（审计口径：对象号将来被改，结转单仍能说清是哪批）。
    pub object_no: &'a str,
    /// 结转期间。
    pub period_code: &'a str,
    /// 第几次结转。
    pub transfer_seq: i32,
    /// 产出品。
    pub product_id: Uuid,
    /// 产出批号。
    pub lot_no: Option<&'a str>,
    /// 结转口径的完工数量。
    pub finished_qty: Decimal,
    /// 本次单位成本。
    pub unit_cost: Decimal,
    /// 结转时点的累计归集快照。
    pub collected: Amounts,
    /// 本次结转金额。
    pub transfer: Amounts,
    /// 备注。
    pub remark: Option<&'a str>,
    /// 操作人。
    pub operator: Option<Uuid>,
}

/// 写一条结转单，返回新行 id。
///
/// 库层 `uk_biz_cost_batch_finish_seq (object_id, transfer_seq) WHERE deleted_at IS NULL` 是
/// 「并发下同一批写成两个第 1 次」的最后一道闸：绕开行锁直插会撞唯一键（真库用例有牙）。
pub async fn insert_finish(ex: &impl SqlExecutor, insert: &FinishInsert<'_>) -> DbResult<Uuid> {
    let id = Uuid::now_v7();
    let operator_text = insert.operator.map(|value| value.to_string()).unwrap_or_default();
    let sql = "INSERT INTO biz_cost_batch_finish \
                 (id, object_id, object_no, period_code, transfer_seq, product_id, lot_no, \
                  finished_qty, unit_cost, \
                  collected_material, collected_labor, collected_overhead, collected_total, \
                  material_amount, labor_amount, overhead_amount, total_amount, \
                  remark, create_by, update_by, created_at, updated_at) \
               VALUES (?::uuid, ?::uuid, ?, ?, ?, ?::uuid, ?, \
                       ?::text::numeric, ?::text::numeric, \
                       ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                       ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                       ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, now(), now())";
    ex.exec_sql(
        sql.to_string(),
        vec![
            Value::from(&id),
            Value::from(&insert.object_id),
            Value::Text(insert.object_no.to_string()),
            Value::Text(insert.period_code.to_string()),
            Value::I64(i64::from(insert.transfer_seq)),
            Value::from(&insert.product_id),
            insert
                .lot_no
                .map(|value| Value::Text(value.to_string()))
                .unwrap_or(Value::Null),
            Value::Text(insert.finished_qty.to_string()),
            Value::Text(insert.unit_cost.to_string()),
            Value::Text(insert.collected.material.to_string()),
            Value::Text(insert.collected.labor.to_string()),
            Value::Text(insert.collected.overhead.to_string()),
            Value::Text(insert.collected.total().to_string()),
            Value::Text(insert.transfer.material.to_string()),
            Value::Text(insert.transfer.labor.to_string()),
            Value::Text(insert.transfer.overhead.to_string()),
            Value::Text(insert.transfer.total().to_string()),
            insert
                .remark
                .map(|value| Value::Text(value.to_string()))
                .unwrap_or(Value::Null),
            Value::Text(operator_text.clone()),
            Value::Text(operator_text),
        ],
    )
    .await?;
    Ok(id)
}

// ==================== 批次抬头（复用 M5-02） ====================

/// 在**当前用户数据范围**内按 id 取批次抬头；不可见 ⇒ `None`（调用方转 404）。
pub async fn find_visible_batch(principal: &ScopedPrincipal, id: Uuid) -> DbResult<Option<SheetBatchRes>> {
    Ok(cost_collect_sheet_model::find_visible_batch(principal, id)
        .await?
        .map(|batch| batch.to_res()))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 结转单的「第 n 次」不得为 0（库层 `ck_biz_cost_batch_finish_seq` 同口径）。
    #[test]
    fn transfer_seq_starts_at_one() {
        assert_eq!(
            dto::plan_transfer(&Amounts::default(), &Amounts::default(), Decimal::ONE, 0).transfer_no,
            1
        );
        assert_eq!(
            dto::plan_transfer(&Amounts::default(), &Amounts::default(), Decimal::ONE, 3).transfer_no,
            4
        );
    }

    /// 「已结转次数」出参不得因为历史行数超过 i32 上限而 panic（防御 `try_from`）。
    #[test]
    fn transferred_times_saturates_instead_of_panicking() {
        let times = i32::try_from(i64::from(i32::MAX) + 1).unwrap_or(i32::MAX);
        assert_eq!(times, i32::MAX);
    }
}
