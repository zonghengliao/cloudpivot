// ============================================================================
// stockin_query.rs —— 只读：单据/明细/来源单查询、采购单与审批流定义的只读引用解析
//
// 由 `stockin_model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model::normalize_page;
use crate::business::shared::scope;
use crate::utils;

use super::stockin_base::*;

/// 按主键取单头（未删除）。
pub async fn find_stock_in(id: Uuid) -> DbResult<Option<StockInRow>> {
    let sql = format!("SELECT {HEADER_COLUMNS} FROM {TABLE} WHERE id = ? AND deleted_at IS NULL");
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(StockInRow::from_row).transpose()
}

/// 在事务/连接上按主键取单头并**加行锁**（过账用：CAS 之前先锁）。
pub async fn lock_stock_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<StockInRow>> {
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {HEADER_COLUMNS} FROM {TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(StockInRow::from_row).transpose()
}

/// 取整单明细（行号升序）。
pub async fn items_of(id: Uuid) -> DbResult<Vec<StockInItemRow>> {
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT {ITEM_COLUMNS} FROM {ITEM_TABLE} WHERE stock_in_id = ?::uuid \
                 ORDER BY line_no ASC"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    rows.iter().map(StockInItemRow::from_row).collect()
}

/// 在**调用方执行器**上按主键取单头（不加锁；联动动作取数用）。
///
/// 与 [`find_stock_in`] 的区别只是「走传入的连接/事务」——过账事务里的联动动作
/// 必须看见同一事务内刚写完的单头状态，不能用连接池里的另一条连接。
pub async fn header_on(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<StockInRow>> {
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {HEADER_COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(StockInRow::from_row).transpose()
}

/// 收货单的**按物料聚合明细行**（P2-1b §4.2：粒度 `MATERIAL`，不是明细行）。
///
/// **一条聚合 SQL**：同一 `material_id` 的多行在这里就合并（金额 / 数量相加、行号取区间端点），
/// 与过账路径（`stockin_handler_post` 用 `VoucherMaterialLine::normalize` 在内存明细上聚合）
/// 逐字同口径 —— 两条路生成的凭证因此逐行一致，这是 D3 影子对比不报差异的前提。
///
/// `ORDER BY material_id ASC` 是**契约**而不是优化：与批次 1 技术架构的加锁顺序同口径，
/// 保证同一张单两次生成的分录行序与金额完全一致。
pub async fn material_lines_of(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> DbResult<Vec<crate::business::erp::voucher::VoucherMaterialLine>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT material_id, sum(amount) AS material_amount, sum(qty) AS material_qty, \
                        min(line_no) AS line_from, max(line_no) AS line_to \
                   FROM {ITEM_TABLE} \
                  WHERE stock_in_id = ?::uuid \
                  GROUP BY material_id \
                  ORDER BY material_id ASC"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(crate::business::erp::voucher::VoucherMaterialLine {
                material_id: row.get_typed("material_id")?,
                qty: row.get_typed("material_qty")?,
                amount: row.get_typed("material_amount")?,
                line_from: row.get_typed("line_from")?,
                line_to: row.get_typed("line_to")?,
            })
        })
        .collect()
}

/// 收货单的**凭证上下文**（联动动作 `GENERATE_VOUCHER` 的唯一取数入口）。
///
/// 取数公式只有一份（[`crate::business::erp::voucher::VoucherBizCtx::of_stock_in`]）：
/// 本函数只负责「按 id 取单头 + 取按物料聚合的明细行」，然后交给它。
/// 单据不存在 → `Ok(None)`（调用方跳过动作，不当失败）。
pub async fn voucher_biz_ctx(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> DbResult<Option<crate::business::erp::voucher::VoucherBizCtx>> {
    let Some(header) = header_on(ex, id).await? else {
        return Ok(None);
    };
    let material_lines = material_lines_of(ex, id).await?;
    Ok(Some(
        crate::business::erp::voucher::VoucherBizCtx::of_stock_in(&header, material_lines),
    ))
}

/// 复制源（D9）：原单头 + 明细，**只读**（`FOR SHARE`：本事务不会改这两张表的行）。
///
/// 与 [`find_stock_in`] / [`items_of`] 的区别是「走调用方的事务 + 显式共享锁」：
/// 复制要在同一事务里「读原单 → 建新单」，读的时候就用 `FOR SHARE` 告诉 PG
/// 「我只看」，既不会挡住别人过账，也不会有人在我读一半时把明细删掉（改单是删旧插新）。
///
/// # 返回值
///
/// - `Ok(Some((单头, 明细)))`：明细按 `line_no` 升序；
/// - `Ok(None)`：单头不存在或已软删；
/// - `Err(DbError)`：数据库异常。
pub async fn copy_source(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> DbResult<Option<(StockInRow, Vec<StockInItemRow>)>> {
    let header = ex
        .query_first_sql(
            format!(
                "SELECT {HEADER_COLUMNS} FROM {TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR SHARE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    let Some(row) = header.as_ref() else {
        return Ok(None);
    };
    let header = StockInRow::from_row(row)?;
    let items = ex
        .query_sql(
            format!(
                "SELECT {ITEM_COLUMNS} FROM {ITEM_TABLE} \
                  WHERE stock_in_id = ?::uuid ORDER BY line_no ASC FOR SHARE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    let items = items
        .iter()
        .map(StockInItemRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok(Some((header, items)))
}

/// 分页（关键词 / 状态 / 供应商 / 仓库 / 入库日期区间）。
pub async fn stock_in_page(q: &StockInFilter) -> DbResult<(Vec<StockInRow>, i64)> {
    let mut sql = format!("SELECT {HEADER_COLUMNS} FROM {TABLE} WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (in_no LIKE ? OR remark LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    if let Some(supplier_id) = q.supplier_id.as_deref() {
        sql.push_str(" AND supplier_id = ?::uuid");
        params.push(Value::from(parse_id(supplier_id)?));
    }
    if let Some(warehouse_id) = q.warehouse_id.as_deref() {
        sql.push_str(" AND warehouse_id = ?::uuid");
        params.push(Value::from(parse_id(warehouse_id)?));
    }
    if let Some(pattern) = utils::like_pattern(q.purchase_order_no.as_deref()) {
        // 子查询而不是 JOIN：`paginate_unscoped` 会对 `sql` 原样包一层 count(*)，
        // JOIN 在同一采购单有多张收货单时会让 count 虚高（分页页码错乱）。
        sql.push_str(
            " AND source_order_id IN (SELECT id FROM biz_purchase_order \
               WHERE order_no LIKE ? AND deleted_at IS NULL)",
        );
        params.push(Value::from(pattern));
    }
    if let Some(date_from) = q.date_from {
        sql.push_str(" AND in_date >= ?");
        params.push(Value::from(date_from));
    }
    if let Some(date_to) = q.date_to {
        sql.push_str(" AND in_date <= ?");
        params.push(Value::from(date_to));
    }
    let (page, page_size) = normalize_page(q.page, q.page_size);
    let result =
        scope::paginate_unscoped(LIST_TARGET, &sql, params, Some(LIST_ORDER), page, page_size)
            .await?;
    let rows = result
        .list
        .iter()
        .map(StockInRow::from_row)
        .collect::<DbResult<Vec<StockInRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// DTO 里的 id 是 `String`：非法 UUID 一律报错（不静默跳过条件）。
pub(super) fn parse_id(value: &str) -> DbResult<Uuid> {
    Uuid::parse_str(value.trim())
        .map_err(|err| DbError::InvalidValue(format!("非法 UUID `{value}`：{err}")))
}

/// 单查来源采购单号（详情页用；采购单被软删 / 不存在 → `None`）。
pub async fn order_no_of(order_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT order_no FROM biz_purchase_order WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&order_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(row.get_typed("order_no")?)),
    }
}

/// 单查来源采购单的合同号快照（详情页「PO-xxx → HT-xxx」**一跳**带出用）。
///
/// 只读 `biz_purchase_order.contract_no_snapshot`（A3 落库的快照列），
/// **不 JOIN `biz_contract`**：合同在两环之上（设计 §6.4 / AP-6），入库单里也没有
/// `contract_id` 列 —— 显示可以，落库/判定不行。
pub async fn contract_no_of(order_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT contract_no_snapshot FROM biz_purchase_order \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&order_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("contract_no_snapshot")?),
    }
}

/// 批量取采购单号：`source_order_id → order_no`（列表补 `source_order_no` 用，**一次**查询）。
///
/// 列表一页 20 条，逐条查会是 20 次往返；这里一次 `IN (...)` 拿完。
/// 采购单被软删的不返回 → 前端拿到 `null`，列表显示「—」（历史单据的单号不该因为
/// 采购单被删就一起消失，所以这里**不**把 deleted_at 条件套到调用方的行上）。
pub async fn order_nos_of(ids: &[Uuid]) -> DbResult<std::collections::HashMap<Uuid, String>> {
    let mut map = std::collections::HashMap::new();
    if ids.is_empty() {
        return Ok(map);
    }
    let placeholders = vec!["?::uuid"; ids.len()].join(", ");
    let sql = format!(
        "SELECT id, order_no FROM biz_purchase_order WHERE id IN ({placeholders}) \
           AND deleted_at IS NULL"
    );
    let params: Vec<Value> = ids.iter().map(Value::from).collect();
    let rows = db().find_with_params(&sql, params).await?;
    for row in rows.iter() {
        let id: Uuid = row.get_typed("id")?;
        let no: String = row.get_typed("order_no")?;
        map.insert(id, no);
    }
    Ok(map)
}

/// 取一张收货单里带来源采购明细行的 `(source_item_id, qty)`（作废回退已收数量用）。
///
/// 只返回 `source_item_id IS NOT NULL` 的行：手工收货（无来源采购单）/ 采购单之外的入库
/// 不该动 `biz_purchase_order_item`。
pub async fn source_lines_of(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
) -> DbResult<Vec<(Uuid, Decimal)>> {
    let rows = ex
        .query_sql(
            "SELECT source_item_id, qty FROM biz_stock_in_item \
              WHERE stock_in_id = ?::uuid AND source_item_id IS NOT NULL \
              ORDER BY line_no ASC"
                .to_string(),
            vec![Value::from(&stock_in_id)],
        )
        .await?;
    let mut lines = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        lines.push((row.get_typed("source_item_id")?, row.get_typed("qty")?));
    }
    Ok(lines)
}

/// 收货单已生成的序列号数量（详情展示 / 单测断言）。
pub async fn serial_count_of(stock_in_id: Uuid) -> DbResult<i64> {
    // SCOPE-EXEMPT: 序列号台账是仓储数据，无归属列。
    let count: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_material_serial \
              WHERE source_id = ?::uuid AND source_type = 'PURCHASE' AND deleted_at IS NULL",
            vec![Value::from(&stock_in_id)],
        )
        .await?;
    Ok(count)
}

// ==================== 作废：退序列号（P10） ====================

/// 已发布的「收货单作废审批」流程（`flow_code='stock_in_cancel'` 或 `biz_type='STOCK_IN_CANCEL'`）。
/// `None` = **没配流程**（不是错误）：调用方按卡面 §二 ①「降级」处理 —— 直接作废
/// （保持 P16 之前的行为）并打一条 `warn`，绝不因为没配流程就把作废功能锁死。
///
/// 排序口径与 [`transfer_model::published_transfer_flow`] 完全一致（`biz_type` 精确命中优先 →
/// `is_default` → 版本号 → 更新时间），这样「客户自己建了同名流程」也能被稳定选中。
#[derive(Debug, Clone)]
pub struct PublishedFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`STOCK_IN_CANCEL`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 作废审批的业务类型（`biz_flow_def.biz_type`，与种子迁移 V0174 同口径）。
pub const CANCEL_BIZ_TYPE: &str = "STOCK_IN_CANCEL";
/// 作废审批的流程编码（`flow_code`）。
pub const CANCEL_FLOW_CODE: &str = "stock_in_cancel";

/// 查「已发布的收货单作废流程」。
pub async fn published_cancel_flow() -> DbResult<Option<PublishedFlow>> {
    published_flow_of(CANCEL_BIZ_TYPE, CANCEL_FLOW_CODE).await
}

/// 过账审批的业务类型（`biz_flow_def.biz_type`，与种子迁移 V0182 同口径）。
pub const POST_BIZ_TYPE: &str = "STOCK_IN_POST";
/// 过账审批的流程编码（`flow_code`）。
pub const POST_FLOW_CODE: &str = "stock_in_post";

/// 查「已发布的收货单过账流程」（P-FLOW-3A）。
///
/// `None` = **没配流程**（不是错误）：调用方按「降级」处理 —— 直接过账
/// （保持 P-FLOW-3A 之前的行为）并打一条 `warn`，绝不因为没配流程就把过账锁死。
pub async fn published_post_flow() -> DbResult<Option<PublishedFlow>> {
    published_flow_of(POST_BIZ_TYPE, POST_FLOW_CODE).await
}

/// 按 `(biz_type, flow_code)` 取唯一的已发布流程。
///
/// 排序口径（作废 / 过账 / 调拨三处一致）：`biz_type` 精确命中优先 → `is_default` →
/// 版本号 → 更新时间，这样「客户自己建了同名流程」也能被稳定选中。
pub(super) async fn published_flow_of(
    biz_type: &str,
    flow_code: &str,
) -> DbResult<Option<PublishedFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的单据该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = ? OR lower(d.flow_code) = ?) \
              ORDER BY (d.biz_type = ?) DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![
                Value::Text(biz_type.to_string()),
                Value::Text(flow_code.to_string()),
                Value::Text(biz_type.to_string()),
            ],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(PublishedFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}
