// ============================================================================
// transfer_handler_query.rs —— 只读入口：分页 / 详情 / 涉及序列号分组 / 在途超期汇总
//
// 由 `transfer_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::transfer_handler_common::*;
use crate::business::erp::transfer_dto::{
    TransferDetailRes, TransferFilter, TransferItemRes, TransferOverdueSummaryRes, TransferRes, TransferSerialGroupRes,
    TransferSerialRes, TransferSerialsRes,
};
use crate::business::erp::transfer_model::{self, TransferQuery};
use crate::utils;
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 调拨单分页（状态 / 调出仓 / 调入仓 / 单号前缀）。
#[nik::permission(
    path = "/business/erp/transfer/page",
    method = "GET",
    permission = "business:erp:transfer:list"
)]
#[nik::get("/business/erp/transfer/page")]
pub async fn transfer_page(Query(filter): Query<TransferFilter>) -> ResResult<PageData<Vec<TransferRes>>> {
    let filter = filter.validated()?;
    // 阈值读一次：分页 SQL 的 WHERE 与返回值的 `in_transit_overdue` 必须是同一口径
    let overdue_days = transfer_model::overdue_threshold_days().await;
    let query = TransferQuery {
        status: filter.status.clone(),
        from_warehouse_id: optional_uuid(filter.from_warehouse_id.as_deref(), "from_warehouse_id")?,
        to_warehouse_id: optional_uuid(filter.to_warehouse_id.as_deref(), "to_warehouse_id")?,
        transfer_no: filter.transfer_no.clone(),
        in_transit_overdue: filter.in_transit_overdue,
        overdue_days,
        page: filter.page,
        page_size: filter.page_size,
    };
    let (rows, total) = transfer_model::page(&query)
        .await
        .map_err(|error| utils::db_error(error, "分页查询调拨单"))?;
    let mut ids = Vec::with_capacity(rows.len() * 2);
    for row in &rows {
        ids.push(row.from_warehouse_id);
        ids.push(row.to_warehouse_id);
    }
    let names = transfer_model::warehouse_names(&ids)
        .await
        .map_err(|error| utils::db_error(error, "查询仓库名称"))?;
    let now = Utc::now();
    let list = rows
        .iter()
        .map(|row| {
            TransferRes::of(
                row,
                names.get(&row.from_warehouse_id).cloned(),
                names.get(&row.to_warehouse_id).cloned(),
            )
            .with_overdue(row, now, overdue_days)
        })
        .collect();
    let (page, page_size) = crate::business::erp::model::normalize_page(filter.page, filter.page_size);
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

// ==================== ② 详情 ====================

/// 详情组装（handler 与建单 / 改单 / 流转的返回共用）。
pub async fn detail_of(id: Uuid) -> std::result::Result<TransferDetailRes, ApiError> {
    let row = transfer_model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单"))?
        .ok_or_else(|| not_found("调拨单"))?;
    let items = transfer_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨明细"))?;
    let serials = transfer_model::serials_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨序列号"))?;
    let mut by_item: std::collections::BTreeMap<Uuid, Vec<String>> = std::collections::BTreeMap::new();
    for serial in &serials {
        by_item
            .entry(serial.item_id)
            .or_default()
            .push(serial.serial_id.to_string());
    }
    let names = transfer_model::warehouse_names(&[row.from_warehouse_id, row.to_warehouse_id])
        .await
        .map_err(|error| utils::db_error(error, "查询仓库名称"))?;
    let overdue_days = transfer_model::overdue_threshold_days().await;
    let item_res = items
        .iter()
        .map(|item| {
            TransferItemRes::of(
                item,
                item.material_code.clone(),
                item.material_name.clone(),
                by_item.remove(&item.id).unwrap_or_default(),
            )
        })
        .collect();
    Ok(TransferDetailRes {
        transfer: TransferRes::of(
            &row,
            names.get(&row.from_warehouse_id).cloned(),
            names.get(&row.to_warehouse_id).cloned(),
        )
        .with_overdue(&row, Utc::now(), overdue_days),
        items: item_res,
    })
}

/// 调拨单详情（含明细 + 每行序列号 id）。
#[nik::permission(
    path = "/business/erp/transfer/{id}",
    method = "GET",
    permission = "business:erp:transfer:detail"
)]
#[nik::get("/business/erp/transfer/{id}")]
pub async fn transfer_detail(Path(id): Path<String>) -> ResResult<TransferDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ②' 超期在途汇总（LNK-F12） ====================

/// 超期在途汇总：阈值 + 超期单数 + 最早一张的发货时间。
///
/// # 为什么单独给一个接口
///
/// 列表筛选（`in_transit_overdue=true`）解决「能把超期单查出来」，但「现在有几张、
/// 最老那张压了多久」这类首页 / 告警角标数据用分页接口算不出准确总数口径
/// （分页只回当前页）。本接口是**全库计数**（不走分页），一次查询出两个数。
///
/// 权限码复用 `business:erp:transfer:list`：能看列表的人就能看汇总，
/// 不新增权限码（也就少一条菜单迁移）。
#[nik::permission(
    path = "/business/erp/transfer/overdue/summary",
    method = "GET",
    permission = "business:erp:transfer:list"
)]
#[nik::get("/business/erp/transfer/overdue/summary")]
pub async fn transfer_overdue_summary() -> ResResult<TransferOverdueSummaryRes> {
    let threshold_days = transfer_model::overdue_threshold_days().await;
    let summary = transfer_model::overdue_summary(threshold_days, Utc::now())
        .await
        .map_err(|error| utils::db_error(error, "统计超期在途调拨单"))?;
    Ok(Res::ok(TransferOverdueSummaryRes {
        threshold_days: summary.threshold_days,
        total: summary.total,
        oldest_shipped_at: summary.oldest_shipped_at,
    }))
}

// ==================== ③ 建单 ====================

/// 该调拨单涉及的所有序列号（按明细分组）。
pub async fn serials_of_transfer(id: Uuid) -> std::result::Result<TransferSerialsRes, ApiError> {
    let row = transfer_model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单"))?
        .ok_or_else(|| not_found("调拨单"))?;
    let items = transfer_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨明细"))?;
    let serials = transfer_model::serials_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨序列号"))?;
    let mut by_item: std::collections::BTreeMap<Uuid, Vec<TransferSerialRes>> = std::collections::BTreeMap::new();
    for serial in &serials {
        by_item
            .entry(serial.item_id)
            .or_default()
            .push(TransferSerialRes::of(serial));
    }
    let total = serials.len();
    let groups = items
        .iter()
        .map(|item| TransferSerialGroupRes {
            item_id: item.id.to_string(),
            line_no: item.line_no,
            material_id: item.material_id.to_string(),
            serials: by_item.remove(&item.id).unwrap_or_default(),
        })
        .collect();
    Ok(TransferSerialsRes {
        transfer_id: row.id.to_string(),
        transfer_no: row.transfer_no,
        total,
        groups,
    })
}

/// 调拨单涉及的序列号（按明细分组）。
#[nik::permission(
    path = "/business/erp/transfer/{id}/serials",
    method = "GET",
    permission = "business:erp:transfer:detail"
)]
#[nik::get("/business/erp/transfer/{id}/serials")]
pub async fn transfer_serials(Path(id): Path<String>) -> ResResult<TransferSerialsRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(serials_of_transfer(id).await?))
}

// 编译期契约：状态常量必须覆盖 DDL 的 CHECK（漏一个 state_check_holds 就会恒 false）
