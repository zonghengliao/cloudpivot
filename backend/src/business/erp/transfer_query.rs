// ============================================================================
// transfer_query.rs —— 只读：分页/详情/明细/序列号/仓库与物料引用解析
//
// 由 `transfer_model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model::normalize_page;
use crate::business::shared::scope;

use super::transfer_base::*;
use super::transfer_scan::{DEFAULT_OVERDUE_DAYS, cutoff_of};

/// 分页查询条件（handler 把 DTO 归一化后传进来）。
#[derive(Debug, Clone)]
pub struct TransferQuery {
    /// 状态。
    pub status: Option<String>,
    /// 调出仓库。
    pub from_warehouse_id: Option<Uuid>,
    /// 调入仓库。
    pub to_warehouse_id: Option<Uuid>,
    /// 单号前缀。
    pub transfer_no: Option<String>,
    /// 只看「超期在途」（`Some(true)`）/ 排除超期（`Some(false)`）/ 不筛选（`None`）。
    ///
    /// 超期口径见 [`is_in_transit_overdue`]：`IN_TRANSIT` 且 `shipped_at < now - overdue_days`。
    pub in_transit_overdue: Option<bool>,
    /// 超期阈值（天）。handler 用 [`overdue_threshold_days`] 填充；
    /// 直接构造查询的调用方（单测 / 报表）可以显式给值，避免「阈值只能从库里读」。
    pub overdue_days: i64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for TransferQuery {
    fn default() -> Self {
        Self {
            status: None,
            from_warehouse_id: None,
            to_warehouse_id: None,
            transfer_no: None,
            in_transit_overdue: None,
            // 显式给缺省阈值：`derive(Default)` 会让 i64 落成 0，
            // 于是「忘了填」变成「阈值 0 天 = 刚发货就算超期」，是静默错数据。
            overdue_days: DEFAULT_OVERDUE_DAYS,
            page: 1,
            page_size: crate::business::erp::model::DEFAULT_PAGE_SIZE,
        }
    }
}

/// 分页查询（走 [`scope::paginate_unscoped`]，闸①口径）。
///
/// `transfer_no` 用前缀 `LIKE`（`TK-20261030-%`）：调拨单是低频单据、表很小，
/// 参数化 `LIKE` 走不了 btree 也不算问题；等量级上来再换 `~>=~` / `~<~`（W1b 的仓库树写法）。
pub async fn page(query: &TransferQuery) -> DbResult<(Vec<TransferRow>, i64)> {
    let mut sql =
        format!("SELECT {TRANSFER_COLUMNS} FROM {TRANSFER_TABLE} WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = query.status.as_deref().map(str::trim)
        && !status.is_empty()
    {
        sql.push_str(" AND status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(id) = query.from_warehouse_id {
        sql.push_str(" AND from_warehouse_id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(id) = query.to_warehouse_id {
        sql.push_str(" AND to_warehouse_id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(keyword) = query.transfer_no.as_deref().map(str::trim)
        && !keyword.is_empty()
    {
        sql.push_str(" AND transfer_no LIKE ?");
        params.push(Value::Text(format!("{keyword}%")));
    }
    if let Some(overdue) = query.in_transit_overdue {
        // 时间点在 Rust 侧算好再绑定（与纯函数 `is_in_transit_overdue` 同一分界点）
        let cutoff = cutoff_of(Utc::now(), query.overdue_days);
        if overdue {
            sql.push_str(
                " AND status = 'IN_TRANSIT' AND shipped_at IS NOT NULL AND shipped_at < ?::timestamptz",
            );
        } else {
            sql.push_str(
                " AND NOT (status = 'IN_TRANSIT' AND shipped_at IS NOT NULL \
                  AND shipped_at < ?::timestamptz)",
            );
        }
        params.push(Value::from(cutoff));
    }

    let (page, page_size) = normalize_page(query.page, query.page_size);
    let result = scope::paginate_unscoped(
        TRANSFER_TARGET,
        &sql,
        params,
        Some(TRANSFER_ORDER),
        page,
        page_size,
    )
    .await?;
    let rows = result
        .list
        .iter()
        .map(TransferRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 按主键取一行（软删过滤恒带）。
pub async fn find_by_id(id: Uuid) -> DbResult<Option<TransferRow>> {
    let sql = format!(
        "SELECT {TRANSFER_COLUMNS} FROM {TRANSFER_TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(TransferRow::from_row).transpose()
}

/// 按单号取一行（审批回调 / 排障用）。
pub async fn find_by_no(transfer_no: &str) -> DbResult<Option<TransferRow>> {
    let sql = format!(
        "SELECT {TRANSFER_COLUMNS} FROM {TRANSFER_TABLE} \
          WHERE transfer_no = ? AND deleted_at IS NULL"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::Text(transfer_no.to_string())])
        .await?;
    rows.first().map(TransferRow::from_row).transpose()
}

/// 批量取两侧仓库名（列表页一次查完，避免 N+1）。
pub async fn warehouse_names(ids: &[Uuid]) -> DbResult<std::collections::HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let mut unique: Vec<Uuid> = ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let payload = unique
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    // SCOPE-EXEMPT: 仓库是全局主数据（V0137 无 dept_id），只按主键批量取名称。
    let rows = db()
        .query_sql(
            "SELECT w.id, w.warehouse_name \
               FROM biz_warehouse w \
               JOIN unnest(string_to_array(?, E'\\n')) AS t(line) ON w.id = t.line::uuid"
                .to_string(),
            vec![Value::Text(payload)],
        )
        .await?;
    let mut map = std::collections::HashMap::with_capacity(rows.len());
    for row in &rows {
        let id: Uuid = row.get_typed("id")?;
        let name: String = row.get_typed("warehouse_name")?;
        map.insert(id, name);
    }
    Ok(map)
}

/// 明细（连表带出物料编码 / 名称，按 `line_no` 升序）。
pub async fn items_of(transfer_id: Uuid) -> DbResult<Vec<TransferItemRow>> {
    let sql = format!(
        "SELECT i.id, i.transfer_id, i.line_no, i.material_id, i.from_location_id, \
                i.to_location_id, i.qty, i.shipped_qty, i.received_qty, i.unit_cost, i.amount, \
                i.remark, i.created_at, i.updated_at, i.create_by, i.update_by, \
                m.material_code, m.material_name \
           FROM {ITEM_TABLE} i \
           LEFT JOIN biz_material m ON m.id = i.material_id \
          WHERE i.transfer_id = ?::uuid \
          ORDER BY i.line_no ASC, i.id ASC"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&transfer_id)])
        .await?;
    rows.iter().map(TransferItemRow::from_row).collect()
}

/// 复制源（D9）：原单头 + 明细，**只读**（`FOR SHARE`）。
///
/// 复制 = 「读原单 → 建新单」在**同一事务**里完成，所以不能走 [`find_by_id`] /
/// [`items_of`]（它们用全局 `db()`，在另一个连接上，看不到本事务的锁）。
/// `FOR SHARE` 的语义正是这里要的：我看，但不改；别人可以继续过账 / 结算。
///
/// # 返回值
///
/// - `Ok(Some((单头, 明细)))`：明细按 `line_no` 升序；
/// - `Ok(None)`：单头不存在或已软删；
/// - `Err(DbError)`：数据库异常。
pub async fn copy_source(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> DbResult<Option<(TransferRow, Vec<TransferItemRow>)>> {
    let header = ex
        .query_first_sql(
            format!(
                "SELECT {TRANSFER_COLUMNS} FROM {TRANSFER_TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR SHARE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    let Some(row) = header.as_ref() else {
        return Ok(None);
    };
    let header = TransferRow::from_row(row)?;
    let items = ex
        .query_sql(
            format!(
                "SELECT i.id, i.transfer_id, i.line_no, i.material_id, i.from_location_id, \
                        i.to_location_id, i.qty, i.shipped_qty, i.received_qty, i.unit_cost, \
                        i.amount, i.remark, i.created_at, i.updated_at, i.create_by, i.update_by, \
                        m.material_code, m.material_name \
                   FROM {ITEM_TABLE} i \
                   LEFT JOIN biz_material m ON m.id = i.material_id \
                  WHERE i.transfer_id = ?::uuid \
                  ORDER BY i.line_no ASC, i.id ASC \
                  FOR SHARE OF i"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    let items = items
        .iter()
        .map(TransferItemRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok(Some((header, items)))
}

/// 序列号子表（连表带出号文本；按行号 + 号升序）。
pub async fn serials_of(transfer_id: Uuid) -> DbResult<Vec<TransferSerialRow>> {
    let sql = format!(
        "SELECT s.id, s.item_id, s.serial_id, i.line_no, ms.serial_no \
           FROM {ITEM_SERIAL_TABLE} s \
           JOIN {ITEM_TABLE} i ON i.id = s.item_id \
           LEFT JOIN biz_material_serial ms ON ms.id = s.serial_id \
          WHERE i.transfer_id = ?::uuid \
          ORDER BY i.line_no ASC, ms.serial_no ASC NULLS LAST, s.id ASC"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&transfer_id)])
        .await?;
    rows.iter().map(TransferSerialRow::from_row).collect()
}

// ==================== 引用校验（写前面一次查完） ====================

/// 校验一个仓库存在（返回名称）；不存在 → `None`。
pub async fn warehouse_name(id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT warehouse_name FROM biz_warehouse WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("warehouse_name"))
        .transpose()
}

/// 在途仓 id（`WH-TRANSIT`）；没种/被删 → `None`。
pub async fn transit_warehouse_id() -> DbResult<Option<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT id FROM biz_warehouse \
              WHERE warehouse_code = ? AND deleted_at IS NULL ORDER BY level ASC LIMIT 1",
            vec![Value::Text(TRANSIT_WAREHOUSE_CODE.to_string())],
        )
        .await?;
    rows.first().map(|row| row.get_typed("id")).transpose()
}

/// 库位库存行 `(id, warehouse_id)`：`from_location_id` / `to_location_id` 指向
/// `biz_inventory_location.id`（**不是** `biz_location.id` —— 实测外键如此）。
pub async fn inventory_location_ref(id: Uuid) -> DbResult<Option<(Uuid, Uuid)>> {
    let rows = db()
        .find_with_params(
            "SELECT id, warehouse_id FROM biz_inventory_location WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some((row.get_typed("id")?, row.get_typed("warehouse_id")?))),
    }
}

/// 物料是否存在（返回编码 / 名称）。
pub async fn material_ref(id: Uuid) -> DbResult<Option<(String, String)>> {
    let rows = db()
        .find_with_params(
            "SELECT material_code, material_name FROM biz_material \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some((
            row.get_typed("material_code")?,
            row.get_typed("material_name")?,
        ))),
    }
}

/// 序列号「可用且属于该物料」的计数（**一次查完所有行**，参数是 `itemId|serialId|materialId` 的文本载荷）。
///
/// 判据三条同时成立才算有效：`material_id` 匹配、`status = 'IN_STOCK'`、未软删。
/// 调用方拿返回值与自己提交的序列号总数比对 —— 不等就是「有不存在的 / 不属于该物料的 /
/// 已经出库的」序列号，必须 `400` 而不是让它落库成为脏账。
///
/// # 为什么用文本载荷而不是数组参数
///
/// `Value::Text("{a,b}")` 绑 `?::text[]` 会 `error serializing parameter`（W2b 实测），
/// 文本载荷 + `string_to_array` / `split_part` 是唯一稳定的形态。
pub async fn count_valid_serials(payload: &str) -> DbResult<i64> {
    let count: i64 = db()
        .scalar_with_params(
            "SELECT count(*) \
               FROM unnest(string_to_array(?, E'\\n')) AS t(line) \
               JOIN biz_material_serial s \
                 ON s.id = split_part(t.line, '|', 2)::uuid \
                AND s.material_id = split_part(t.line, '|', 3)::uuid \
              WHERE s.status = 'IN_STOCK' AND s.deleted_at IS NULL",
            vec![Value::Text(payload.to_string())],
        )
        .await?;
    Ok(count)
}

// ==================== 写：建单 / 改单 / 删单 ====================
