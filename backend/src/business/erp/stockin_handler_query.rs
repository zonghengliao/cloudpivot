// ============================================================================
// stockin_handler_query.rs —— 只读入口：分页 / 详情
//
// 由 `stockin_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use super::stockin_handler_common::*;
use crate::business::erp::model;
use crate::business::erp::stockin_dto::{StockInDetailRes, StockInPageQuery, StockInRes};
use crate::business::erp::stockin_model::{self};
use crate::utils;

/// 收货入库单分页。
#[nik::permission(
    path = "/business/erp/stock-in/page",
    method = "GET",
    permission = "business:erp:stock-in:list"
)]
#[nik::get("/business/erp/stock-in/page")]
pub async fn stock_in_page(Query(query): Query<StockInPageQuery>) -> ResResult<PageData<Vec<StockInRes>>> {
    let page = query.page;
    let page_size = query.page_size;
    let (rows, total) = stockin_model::stock_in_page(&query.to_filter())
        .await
        .map_err(|error| utils::db_error(error, "分页查询收货入库单"))?;
    let mut list: Vec<StockInRes> = rows.iter().map(StockInRes::from).collect();
    // 来源采购单号：一次 `IN (...)` 批量补齐（列表要显示「来源采购单 PO-xxx」，
    // 逐条查 = 一页 20 次往返）。
    let order_ids: Vec<Uuid> = rows.iter().filter_map(|row| row.source_order_id).collect();
    if !order_ids.is_empty() {
        let nos = stockin_model::order_nos_of(&order_ids)
            .await
            .map_err(|error| utils::db_error(error, "查询来源采购单号"))?;
        for (header, row) in list.iter_mut().zip(rows.iter()) {
            if let Some(order_id) = row.source_order_id {
                header.source_order_no = nos.get(&order_id).cloned();
            }
        }
    }
    // 供应商名称：同样一次 `IN (...)` 批量补齐，读的是**本单的 `supplier_id` 快照**去关联主数据
    // （不是实时 JOIN 采购单拿供应商 —— 列表口径必须冻结在「收货那一刻」，R3-a）。
    let supplier_ids: Vec<Uuid> = rows.iter().filter_map(|row| row.supplier_id).collect();
    if !supplier_ids.is_empty() {
        let names = crate::business::erp::purchase_model::supplier_names_of(&supplier_ids)
            .await
            .map_err(|error| utils::db_error(error, "查询供应商名称"))?;
        for (header, row) in list.iter_mut().zip(rows.iter()) {
            if let Some(supplier_id) = row.supplier_id {
                header.supplier_name = names.get(&supplier_id).cloned();
            }
        }
    }
    // AUD-02：制单人姓名。整页一次 `WHERE id = ANY(...)`（与供应商名同一套批量口径）
    crate::business::shared::user_name::fill_person_names(&mut list)
        .await
        .unwrap_or_default();
    Ok(page_body(list, total, page, page_size))
}

/// 收货入库单详情。
#[nik::permission(
    path = "/business/erp/stock-in/{id}",
    method = "GET",
    permission = "business:erp:stock-in:detail"
)]
#[nik::get("/business/erp/stock-in/{id}")]
pub async fn stock_in_detail(Path(id): Path<String>) -> ResResult<StockInDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 分页响应包装（与 `handler::page_body` 同口径；那边的函数是私有的）。
pub(super) fn page_body<T: Serialize>(list: Vec<T>, total: i64, page: u64, page_size: u64) -> Res<PageData<Vec<T>>> {
    let (page, page_size) = model::normalize_page(page, page_size);
    Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size)
}

// ==================== 单测 ====================
//
// 纯逻辑（金额 / 状态机 / 序列号条数校验）在 `stockin_dto` 与 `stockin_model` 的单测里；
// 这里放**真库用例**（全链路：建单 → 过账 → 查库存 / 流水 / 序列号 / 采购回写）。
//
// 需要真库：`ERP_DB_URL` 指向「已应用 V0145 的隔离库」，缺省则全部 `SKIP`：
//
// ```bash
// su - postgres -c 'createdb -O cloudpivot cloudpivot_p2b1'
// export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_p2b1'
// cargo test --lib business::erp::stockin
// ```
//
// 夹具固定 UUID（`b2b1…` 段）+ `P2B1-` 前缀；每次 `setup()` 先按供应商清掉上一轮的单、
// 流水与库存行，所以重复跑不积累（序列号按 `source_id` 删）。
