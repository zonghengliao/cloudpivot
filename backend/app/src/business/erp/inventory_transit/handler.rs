//! 在途库存查询接口层（M2-04）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/inventory-transit/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/inventory-transit/page` | `business:erp:inventory-transit:list` | 在途明细分页（物料 / 仓库 / 客户 / 发货单 / 状态 / 发货日期区间） |
//! | GET | `/business/erp/inventory-transit/summary` | `business:erp:inventory-transit:list` | 按 物料 × 发货仓 汇总未签收在途（含合计） |
//!
//! # 只读，零写路径
//!
//! 在途行由 M2-02（发货过账写 `shipped_qty`）产生、由 M2-03（签收确认推 `received_qty`）
//! 结转。本文件的 handler **一行写 SQL 都没有**，也不开事务 —— 查不到就返回空页，
//! 不「顺手补一行」。
//!
//! # 两个接口的分工
//!
//! ```text
//! /page      行级明细：谁（客户 / 发货单）的哪批货（物料）、从哪个仓、发了多少、
//!            签了多少、还剩多少在路上、挂了几天
//! /summary   合计视图：这个 物料 × 仓库 还有多少在途（跨发货单合并）
//! ```
//!
//! 汇总与明细**同一份 WHERE 口径**（`model::summary_where` / `model::detail_where` 共用
//! `SHIPMENT_JOIN`，客户都是经发货单头关联），所以「明细逐行相加」与「汇总合计」
//! 一定对得上；截断时 `truncated` 明确告知调用方「合计是全量的、行不是」。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 在途账是发货单的派生账（V0420 无 dept_id / 归属人列），数据范围表达不出来；
//! 可见性由 business:erp:inventory-transit:list 控制。明细走 model::transit_page →
//! page_of → scope::paginate_unscoped。
//! ```
//!
//! # 错误口径（与 ERP 其余查询接口一致）
//!
//! 筛选参数非法（非法 UUID / 日期格式 / 状态不在 3 值域内 / 区间反了）→
//! **400 + 66006（`ERP_FILTER_INVALID`）**，绝不静默忽略条件 ——
//! 「按物料筛」被吞掉会变成「返回全部物料」的假数据。

use axum::extract::Query;
use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_transit::dto::{
    self, TransitFilter, TransitRes, TransitSummaryFilter, TransitSummaryRes, TransitSummaryRow,
};
use crate::business::erp::inventory_transit::model::{self, DetailQuery, SummaryQuery};
use crate::business::erp::sales::sales_order_model;
use crate::business::erp::shipment::shipment_model;
use crate::business::erp::shipment::shipment_transit;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码（与 V0421 的 sys_menu.auth_code 逐字一致） ====================

/// 在途明细 / 汇总共用同一个读码（与 `business:erp:inventory:list` 同形：
/// 台账类只读接口一个 `:list` 码管全部读路径）。
pub const PERM_LIST: &str = "business:erp:inventory-transit:list";

/// 本模块全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const INVENTORY_TRANSIT_PERMISSIONS: [&str; 1] = [PERM_LIST];

// ==================== 校验原语 ====================

/// `400 + 66006`：筛选项非法。
fn bad_filter(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_FILTER_INVALID, message.into())
}

/// 可选 UUID（空白 → `None`；非法 → 400，**不静默当作没传**）。
fn parse_opt_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => Uuid::parse_str(text)
            .map(Some)
            .map_err(|_| bad_filter(format!("{field} 不是合法的 UUID：{text}"))),
    }
}

/// 可选日期（`yyyy-MM-dd`；空白 → `None`；格式不对 → 400）。
fn parse_opt_date(raw: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| bad_filter(format!("{field} 必须是 yyyy-MM-dd 格式（当前 `{text}`）"))),
    }
}

// ==================== 明细 ====================

/// 在途明细分页。
///
/// 名字一律批量补齐（一页固定 3 条 `IN (…)` 往返：物料 / 仓库 / 客户），**不做 N+1**；
/// 发货单号与客户 id 已经在主查询里经发货单头带出，不再多打一条。
pub async fn page_of(filter: TransitFilter) -> std::result::Result<PageData<Vec<TransitRes>>, ApiError> {
    let status = dto::normalize_status(filter.status.as_deref()).map_err(bad_filter)?;
    let material_id = parse_opt_uuid(filter.material_id.as_deref(), "material_id")?;
    let warehouse_id = parse_opt_uuid(filter.warehouse_id.as_deref(), "warehouse_id")?;
    let customer_id = parse_opt_uuid(filter.customer_id.as_deref(), "customer_id")?;
    let shipment_id = parse_opt_uuid(filter.shipment_id.as_deref(), "shipment_id")?;
    let ship_date_from = parse_opt_date(filter.ship_date_from.as_deref(), "ship_date_from")?;
    let ship_date_to = parse_opt_date(filter.ship_date_to.as_deref(), "ship_date_to")?;
    dto::ensure_date_range(ship_date_from, ship_date_to).map_err(bad_filter)?;

    let query = DetailQuery {
        status,
        material_id,
        warehouse_id,
        customer_id,
        shipment_id,
        ship_date_from,
        ship_date_to,
    };
    let (rows, total) = model::transit_page(&query, filter.page, filter.page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询在途库存"))?;

    // 主数据名字：三个批量查询，一页固定 3 条 SQL（不是每行各查一次）
    let material_ids: Vec<Uuid> = rows.iter().map(|row| row.material_id).collect();
    let materials: std::collections::HashMap<Uuid, model::MaterialBrief> = model::material_labels(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "批量查询在途物料"))?
        .into_iter()
        .map(|brief| (brief.id, brief))
        .collect();
    let warehouse_ids: Vec<Uuid> = rows.iter().map(|row| row.warehouse_id).collect();
    let warehouses: std::collections::HashMap<Uuid, (String, String)> =
        shipment_model::warehouse_labels(db(), &warehouse_ids)
            .await
            .map_err(|error| utils::db_error(error, "批量查询在途仓库"))?
            .into_iter()
            .map(|(id, code, name)| (id, (code, name)))
            .collect();
    let customer_ids: Vec<Uuid> = rows.iter().filter_map(|row| row.customer_id).collect();
    let customers: std::collections::HashMap<Uuid, (String, String)> =
        sales_order_model::customer_labels(&customer_ids)
            .await
            .map_err(|error| utils::db_error(error, "批量查询在途客户"))?
            .into_iter()
            .map(|(id, code, name)| (id, (code, name)))
            .collect();

    let list = rows
        .iter()
        .map(|row| {
            let mut res = TransitRes::of(row);
            if let Some(brief) = materials.get(&row.material_id) {
                res.material_code = Some(brief.code.clone());
                res.material_name = Some(brief.name.clone());
                res.spec = brief.spec.clone();
                res.unit_name = brief.unit_name.clone();
            }
            if let Some((code, name)) = warehouses.get(&row.warehouse_id) {
                res.warehouse_code = Some(code.clone());
                res.warehouse_name = Some(name.clone());
            }
            if let Some(customer_id) = row.customer_id
                && let Some((code, name)) = customers.get(&customer_id)
            {
                res.customer_code = Some(code.clone());
                res.customer_name = Some(name.clone());
            }
            res
        })
        .collect();
    let (page, page_size) = shipment_model::normalize_page(filter.page, filter.page_size);
    Ok(PageData {
        list,
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

impl TransitRes {
    /// 一行 → 出参：库里能直接给的全给，主数据名字留空由调用方批量补齐。
    fn of(row: &model::TransitRow) -> Self {
        Self {
            id: row.id.to_string(),
            shipment_id: row.shipment_id.to_string(),
            ship_no: row.ship_no.clone(),
            customer_id: row.customer_id.map(|id| id.to_string()),
            customer_code: None,
            customer_name: None,
            warehouse_id: row.warehouse_id.to_string(),
            warehouse_code: None,
            warehouse_name: None,
            material_id: row.material_id.to_string(),
            material_code: None,
            material_name: None,
            spec: None,
            unit_name: None,
            ship_date: row.ship_date.to_string(),
            age_days: row.age_days,
            shipped_qty: row.shipped_qty,
            received_qty: row.received_qty,
            qty_in_transit: row.qty_in_transit,
            status: row.status.clone(),
            status_label: shipment_transit::status_label(&row.status).to_string(),
            received_at: row.received_at,
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== 汇总 ====================

/// 按 物料 × 发货仓 汇总未签收在途。
///
/// 合计（`total_qty_in_transit` / `total_shipments` / `total_lines`）**不受行数截断影响**：
/// 它是另一条聚合查询算的，与 `rows` 共用同一个 WHERE。
pub async fn summary_of(filter: TransitSummaryFilter) -> std::result::Result<TransitSummaryRes, ApiError> {
    let material_id = parse_opt_uuid(filter.material_id.as_deref(), "material_id")?;
    let warehouse_id = parse_opt_uuid(filter.warehouse_id.as_deref(), "warehouse_id")?;
    let customer_id = parse_opt_uuid(filter.customer_id.as_deref(), "customer_id")?;
    let query = SummaryQuery::new(material_id, warehouse_id, customer_id);

    let (rows, truncated) = model::summary_rows(&query)
        .await
        .map_err(|error| utils::db_error(error, "汇总在途库存"))?;
    let (total_qty, total_shipments, total_lines) = model::summary_totals(&query)
        .await
        .map_err(|error| utils::db_error(error, "汇总在途合计"))?;

    let material_ids: Vec<Uuid> = rows.iter().map(|row| row.material_id).collect();
    let materials: std::collections::HashMap<Uuid, model::MaterialBrief> = model::material_labels(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "批量查询汇总物料"))?
        .into_iter()
        .map(|brief| (brief.id, brief))
        .collect();
    let warehouse_ids: Vec<Uuid> = rows.iter().map(|row| row.warehouse_id).collect();
    let warehouses: std::collections::HashMap<Uuid, (String, String)> =
        shipment_model::warehouse_labels(db(), &warehouse_ids)
            .await
            .map_err(|error| utils::db_error(error, "批量查询汇总仓库"))?
            .into_iter()
            .map(|(id, code, name)| (id, (code, name)))
            .collect();

    let list: Vec<TransitSummaryRow> = rows
        .iter()
        .map(|row| {
            let brief = materials.get(&row.material_id);
            let warehouse = warehouses.get(&row.warehouse_id);
            TransitSummaryRow {
                material_id: row.material_id.to_string(),
                material_code: brief.map(|brief| brief.code.clone()),
                material_name: brief.map(|brief| brief.name.clone()),
                spec: brief.and_then(|brief| brief.spec.clone()),
                unit_name: brief.and_then(|brief| brief.unit_name.clone()),
                warehouse_id: row.warehouse_id.to_string(),
                warehouse_code: warehouse.map(|(code, _)| code.clone()),
                warehouse_name: warehouse.map(|(_, name)| name.clone()),
                qty_in_transit: row.qty_in_transit,
                shipped_qty: row.shipped_qty,
                received_qty: row.received_qty,
                shipments: row.shipments,
                first_ship_date: row.first_ship_date.clone(),
                max_age_days: row.max_age_days,
            }
        })
        .collect();

    Ok(TransitSummaryRes {
        row_count: u64::try_from(list.len()).unwrap_or(0),
        rows: list,
        truncated,
        total_qty_in_transit: total_qty,
        total_shipments,
        total_lines,
    })
}

// ==================== HTTP 入口 ====================

/// 在途明细分页（物料 / 仓库 / 客户 / 发货单 / 状态 / 发货日期区间）。
#[nik::permission(
    path = "/business/erp/inventory-transit/page",
    method = "GET",
    permission = "business:erp:inventory-transit:list"
)]
#[nik::get("/business/erp/inventory-transit/page")]
pub async fn inventory_transit_page(Query(filter): Query<TransitFilter>) -> ResResult<PageData<Vec<TransitRes>>> {
    Ok(Res::ok(page_of(filter).await?))
}

/// 按 物料 × 发货仓 汇总未签收在途（含合计）。
#[nik::permission(
    path = "/business/erp/inventory-transit/summary",
    method = "GET",
    permission = "business:erp:inventory-transit:list"
)]
#[nik::get("/business/erp/inventory-transit/summary")]
pub async fn inventory_transit_summary(Query(filter): Query<TransitSummaryFilter>) -> ResResult<TransitSummaryRes> {
    Ok(Res::ok(summary_of(filter).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::Utc;

    fn dec(text: &str) -> nik::prelude::rust_decimal::Decimal {
        text.parse().expect("定点字面量")
    }

    fn row() -> model::TransitRow {
        let now = Utc::now();
        model::TransitRow {
            id: Uuid::from_u128(0x01),
            shipment_id: Uuid::from_u128(0x02),
            ship_no: Some("SH-20261116-000001".to_string()),
            customer_id: Some(Uuid::from_u128(0x03)),
            material_id: Uuid::from_u128(0x04),
            warehouse_id: Uuid::from_u128(0x05),
            ship_date: NaiveDate::from_ymd_opt(2026, 11, 6).expect("日期"),
            age_days: 10,
            shipped_qty: dec("100"),
            received_qty: dec("40"),
            qty_in_transit: dec("60"),
            status: "IN_TRANSIT".to_string(),
            received_at: None,
            remark: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// 出参映射：三个数量**原样透传**（在途量不在这里算）、状态中文名跟着状态走。
    #[test]
    fn res_passes_quantities_through_and_labels_status() {
        let res = TransitRes::of(&row());
        assert_eq!(res.shipped_qty, dec("100"));
        assert_eq!(res.received_qty, dec("40"));
        assert_eq!(res.qty_in_transit, dec("60"));
        assert_eq!(res.status_label, "在途");
        assert_eq!(res.age_days, 10);
        assert_eq!(res.ship_no.as_deref(), Some("SH-20261116-000001"));
        // 名字由批量补齐填，映射阶段留空（不是「没有名字」而是「还没查」）
        assert!(res.material_name.is_none());
        assert!(res.customer_name.is_none());
        println!("[M2-04] TransitRes 映射：数量透传 / 状态中文名 / 名字待补齐 ✓");
    }

    /// 三个状态的中文名必须与库层状态域一一对应。
    #[test]
    fn status_labels_cover_three_states() {
        for (status, label) in [("IN_TRANSIT", "在途"), ("RECEIVED", "已签收"), ("CANCELLED", "已作废")] {
            assert_eq!(shipment_transit::status_label(status), label);
            assert!(shipment_transit::is_valid_status(status));
        }
        assert_eq!(INVENTORY_TRANSIT_PERMISSIONS, [PERM_LIST]);
        assert_eq!(PERM_LIST, "business:erp:inventory-transit:list");
        println!("[M2-04] 三态中文名 + 权限码常量 ✓");
    }

    /// 非法筛选必须 400 + 66006（不能静默当作没传）。
    #[test]
    fn invalid_filters_are_400_with_erp_filter_invalid() {
        let error = parse_opt_uuid(Some("not-a-uuid"), "material_id").expect_err("非法 UUID 必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_FILTER_INVALID);
        assert!(error.msg.contains("material_id"), "文案要点名字段：{}", error.msg);

        let error = parse_opt_date(Some("2026/11/01"), "ship_date_from").expect_err("日期格式错必须报错");
        assert_eq!(error.code, error_code::ERP_FILTER_INVALID);
        assert!(error.msg.contains("yyyy-MM-dd"));

        // 空白 → None（不是错误）
        assert_eq!(parse_opt_uuid(Some("  "), "material_id").expect("空白可接受"), None);
        assert_eq!(parse_opt_date(Some(""), "ship_date_from").expect("空白可接受"), None);
        println!("[M2-04] 非法 UUID / 日期 → 400 + 66006；空白 → None ✓");
    }
}
