//! 退货单 **PDF 导出**（PDF-04：采购退货单 / 销售退货单）。
//!
//! # 接口（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/business/...`）
//!
//! | 方法 | 路径 | 权限码（复用单据查看权限） | 说明 |
//! |------|------|------------------------------|------|
//! | GET | `/business/erp/purchase-return/{id}/pdf` | `business:erp:purchase-return:list` | 采购退货单 PDF |
//! | GET | `/business/erp/sales-return/{id}/pdf` | `business:erp:sales-return:list` | 销售退货单 PDF |
//!
//! # 卡面要求的字段落点（验收③：**原单号 + 退货原因**）
//!
//! | 要求 | 采购退货 | 销售退货 |
//! |------|----------|----------|
//! | 原单号 | `stock_in_no`（原入库单号）+ `purchase_order_no`（原采购订单号） | `biz_sales_order.so_no`（原销售订单号）+ `biz_shipment.ship_no`（原发货单号） |
//! | 退货原因 | `reason`（NOT NULL，`ck_biz_prt_reason` 拦空白） | `return_type`（QUALITY / WRONG_ITEM / OTHER）+ 备注 |
//! | 对方 | 供应商（`biz_supplier.supplier_name` 快照） | 客户（`biz_customer.name`） |
//! | 仓库 | `warehouse_name` 快照 | `biz_warehouse.warehouse_name` |
//! | 库位 | 明细 `location_id` = **库位账行**（`biz_inventory_location.id`） | 明细 `location_id` = **库位字典**（`biz_location.id`），`NULL` = 整仓 |
//! | 序列号 | 无（整批按库位退） | `serial_nos`（一物一码） |
//!
//! ⚠️ 两种退货单的 `location_id` 口径**不同**（账行 vs 字典），混用会把
//! 「A-01-03」印成别的库位 —— 见 [`crate::business::erp::doc_pdf_model`] 里
//! 两个函数的文档。这也正是本模块不合并成一份取数逻辑的原因。
//!
//! # 审计
//!
//! 导出是 GET，中间件不记；渲染成功后显式补 `EXPORT_PURCHASE_RETURN_PDF` /
//! `EXPORT_SALES_RETURN_PDF` 一行。
//!
//! # 版式
//!
//! 两种退货单都是 12 列明细，**A4 横向**（见 [`template::return_pdf_opts`]）。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::doc_pdf_model;
use crate::business::erp::doc_pdf_render::{self, RenderedDoc};
use crate::business::erp::order_pdf_model::{self, MaterialInfo};
use crate::business::erp::purchase_return_dto;
use crate::business::erp::purchase_return_model;
use crate::business::erp::sales_order_model;
use crate::business::erp::sales_return_model;
use crate::business::erp::shipment_model;
use crate::business::pdf::handler as pdf_handler;
use crate::business::pdf::template::{
    self, OrderMetaField, ReturnContext, ReturnContextArgs, ReturnDocKind, ReturnItem, format_money, format_quantity,
    yuan_to_fen,
};
use crate::utils;

/// 审计里的操作码。
pub fn export_operation(kind: ReturnDocKind) -> &'static str {
    match kind {
        ReturnDocKind::Purchase => "EXPORT_PURCHASE_RETURN_PDF",
        ReturnDocKind::Sales => "EXPORT_SALES_RETURN_PDF",
    }
}

/// 路由权限码（复用单据查看权限）。
pub fn permission_code(kind: ReturnDocKind) -> &'static str {
    match kind {
        ReturnDocKind::Purchase => "business:erp:purchase-return:list",
        ReturnDocKind::Sales => "business:erp:sales-return:list",
    }
}

/// 路径段（`/business/erp/<segment>/{id}/pdf`）。
pub fn path_segment(kind: ReturnDocKind) -> &'static str {
    match kind {
        ReturnDocKind::Purchase => "purchase-return",
        ReturnDocKind::Sales => "sales-return",
    }
}

/// 采购退货明细 → 模板行（纯函数，单测直接调）。
///
/// # 兜底口径
///
/// - 物料查不到 → 用连表带出的编码 / 名称；都没有 → 「（物料已删除）」；
/// - 单位 → 明细 `unit_name` 优先（原入库时的单位快照），否则物料默认单位，再否则 `—`；
/// - 库位 `None` → 「未分配」；账行查不到 → 「已删除的库位账行」。
pub fn build_purchase_return_items(
    sources: &[purchase_return_model::ItemRow],
    materials: &HashMap<Uuid, MaterialInfo>,
    units: &HashMap<Uuid, String>,
    locations: &HashMap<Uuid, String>,
) -> Vec<ReturnItem> {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let material = materials.get(&source.material_id);
            ReturnItem {
                seq: index as u32 + 1,
                material_code: source
                    .material_code
                    .clone()
                    .or_else(|| material.map(|info| info.code.clone()))
                    .unwrap_or_else(|| "（物料已删除）".to_string()),
                material_name: source
                    .material_name
                    .clone()
                    .or_else(|| material.map(|info| info.name.clone()))
                    .unwrap_or_else(|| "（物料已删除）".to_string()),
                spec: material.map(|info| info.spec.clone()).unwrap_or_default(),
                unit: source
                    .unit_name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .or_else(|| material.and_then(|info| units.get(&info.unit_id).cloned()))
                    .unwrap_or_else(|| "—".to_string()),
                qty: format_quantity(source.qty),
                price: format_quantity(source.price),
                amount: format_money(yuan_to_fen(source.amount)),
                tax_rate: format_quantity(source.tax_rate),
                tax_amount: format_money(yuan_to_fen(source.tax_amount)),
                with_tax: format_money(yuan_to_fen(source.with_tax_amount)),
                location: match source.location_id {
                    None => "未分配".to_string(),
                    Some(id) => locations
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| "已删除的库位账行".to_string()),
                },
                // 采购退货不逐件登记序列号（按原入库库位整批退）
                serials: String::new(),
            }
        })
        .collect()
}

/// 销售退货明细 → 模板行（纯函数，单测直接调）。
///
/// # 兜底口径
///
/// - 物料 / 单位同采购侧（本表**没有**单位列，单位取物料主档）；
/// - 库位 `None` = **整仓退货**（真实语义，不是缺失）→ 印「整仓」；
///   字典里查不到 → 「（库位已删除）」；
/// - 序列号为空 → `—`（不是空串，打印出来不能是一格空白）。
///
/// # 行级价税合计
///
/// `biz_sales_return_item` 只存 `amount` / `tax_amount`，**没有**行级价税合计列；
/// 这里打 `—`，合计区仍给出准确的三档合计（单头有权威值）。
pub fn build_sales_return_items(
    sources: &[sales_return_model::ReturnItemRow],
    materials: &HashMap<Uuid, MaterialInfo>,
    units: &HashMap<Uuid, String>,
    locations: &HashMap<Uuid, String>,
) -> Vec<ReturnItem> {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let material = materials.get(&source.material_id);
            ReturnItem {
                seq: index as u32 + 1,
                material_code: material
                    .map(|info| info.code.clone())
                    .unwrap_or_else(|| "（物料已删除）".to_string()),
                material_name: material
                    .map(|info| info.name.clone())
                    .unwrap_or_else(|| "（物料已删除）".to_string()),
                spec: material.map(|info| info.spec.clone()).unwrap_or_default(),
                unit: material
                    .and_then(|info| units.get(&info.unit_id).cloned())
                    .unwrap_or_else(|| "—".to_string()),
                qty: format_quantity(source.qty),
                price: format_quantity(source.price),
                amount: format_money(yuan_to_fen(source.amount)),
                tax_rate: format_quantity(source.tax_rate),
                tax_amount: format_money(yuan_to_fen(source.tax_amount)),
                with_tax: "—".to_string(),
                location: match source.location_id {
                    None => "整仓".to_string(),
                    Some(id) => locations
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| "（库位已删除）".to_string()),
                },
                serials: if source.serial_nos.is_empty() {
                    "—".to_string()
                } else {
                    source.serial_nos.join("、")
                },
            }
        })
        .collect()
}

// ==================== 采购退货单 ====================

/// 采购退货单的渲染上下文（真库取数 + 批量补名字）。
///
/// # 异常情况
///
/// - 404 [`crate::shared::error_code::RESOURCE_NOT_FOUND`]：单据不存在 / 已软删；
/// - 500：数据库异常。
pub async fn purchase_return_context(id: Uuid) -> std::result::Result<ReturnContext, ApiError> {
    let row = purchase_return_model::find_return(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购退货单"))?
        .ok_or_else(|| doc_pdf_model::not_found("采购退货单"))?;
    let lines = purchase_return_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购退货单明细"))?;

    let material_ids: Vec<Uuid> = lines.iter().map(|line| line.material_id).collect();
    let materials = order_pdf_model::materials_of(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询采购退货单物料"))?;
    let unit_ids: Vec<Uuid> = materials.values().map(|info| info.unit_id).collect();
    let units = order_pdf_model::unit_names_of(&unit_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询采购退货单单位"))?;
    // 采购退货明细的 location_id 是**库位账行**（biz_inventory_location.id）
    let location_ids: Vec<Uuid> = lines.iter().filter_map(|line| line.location_id).collect();
    let locations = doc_pdf_model::location_labels_of(&location_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询采购退货单库位"))?;

    let warehouse = match row.warehouse_name.clone() {
        Some(name) if !name.trim().is_empty() => name,
        _ => doc_pdf_model::warehouse_names_of(&[row.warehouse_id])
            .await
            .map_err(|error| utils::db_error(error, "查询采购退货单仓库"))?
            .get(&row.warehouse_id)
            .cloned()
            .unwrap_or_else(|| "（仓库已删除）".to_string()),
    };
    let supplier = match row.supplier_name.clone() {
        Some(name) if !name.trim().is_empty() => name,
        _ => doc_pdf_model::supplier_names_of(&[row.supplier_id])
            .await
            .map_err(|error| utils::db_error(error, "查询采购退货单供应商"))?
            .get(&row.supplier_id)
            .cloned()
            .unwrap_or_else(|| "（供应商已删除）".to_string()),
    };
    let operator = match row.create_by {
        Some(user_id) => order_pdf_model::user_names_of(&[user_id])
            .await
            .map_err(|error| utils::db_error(error, "查询采购退货单经办人"))?
            .get(&user_id)
            .cloned()
            .unwrap_or_else(|| "—".to_string()),
        None => "—".to_string(),
    };

    let items = build_purchase_return_items(&lines, &materials, &units, &locations);
    let total_qty: Decimal = lines.iter().map(|line| line.qty).sum();

    Ok(ReturnContext::new(ReturnContextArgs {
        kind: ReturnDocKind::Purchase,
        doc_no: row.return_no.clone(),
        meta: vec![
            OrderMetaField::new("单号", row.return_no.clone()),
            OrderMetaField::new("退货日期", row.return_date.format("%Y-%m-%d").to_string()),
            OrderMetaField::new("状态", purchase_return_dto::status_label(&row.status)),
            OrderMetaField::new("供应商", supplier),
            OrderMetaField::new("退货仓库", warehouse),
            OrderMetaField::new("经办人", operator),
            // 验收③：原单号（入库单 + 采购订单）必须逐条打印，查不到就打「—」
            OrderMetaField::new("原单号（入库）", row.stock_in_no.clone()),
            OrderMetaField::new(
                "原单号（采购订单）",
                row.purchase_order_no.clone().unwrap_or_else(|| "—".to_string()),
            ),
            OrderMetaField::new(
                "已生成出库单",
                row.stock_out_no.clone().unwrap_or_else(|| "—".to_string()),
            ),
        ],
        reason: row.reason.clone(),
        items,
        total_qty,
        total_amount: row.total_amount,
        total_tax: row.total_tax,
        total_with_tax: row.total_with_tax,
        remark: row.remark.clone().unwrap_or_default(),
        printed_at: doc_pdf_model::printed_at_now(),
    }))
}

// ==================== 销售退货单 ====================

/// 销售退货单的渲染上下文（真库取数 + 批量补名字）。
///
/// # 异常情况
///
/// - 404 [`crate::shared::error_code::RESOURCE_NOT_FOUND`]：单据不存在 / 已软删；
/// - 500：数据库异常。
pub async fn sales_return_context(id: Uuid) -> std::result::Result<ReturnContext, ApiError> {
    let row = sales_return_model::find_return(id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售退货单"))?
        .ok_or_else(|| doc_pdf_model::not_found("销售退货单"))?;
    let lines = sales_return_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售退货单明细"))?;

    let material_ids: Vec<Uuid> = lines.iter().map(|line| line.material_id).collect();
    let materials = order_pdf_model::materials_of(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询销售退货单物料"))?;
    let unit_ids: Vec<Uuid> = materials.values().map(|info| info.unit_id).collect();
    let units = order_pdf_model::unit_names_of(&unit_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询销售退货单单位"))?;
    // 销售退货明细的 location_id 是**库位字典**（biz_location.id）——与采购侧口径不同
    let location_ids: Vec<Uuid> = lines.iter().filter_map(|line| line.location_id).collect();
    let locations = doc_pdf_model::location_dict_labels_of(&location_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询销售退货单库位"))?;

    let warehouse = doc_pdf_model::warehouse_names_of(&[row.warehouse_id])
        .await
        .map_err(|error| utils::db_error(error, "查询销售退货单仓库"))?
        .get(&row.warehouse_id)
        .cloned()
        .unwrap_or_else(|| "（仓库已删除）".to_string());
    let customer = sales_order_model::customer_labels(&[row.customer_id])
        .await
        .map_err(|error| utils::db_error(error, "查询销售退货单客户"))?
        .into_iter()
        .next()
        .map(|(_, _, name)| name)
        .unwrap_or_else(|| "（客户已删除）".to_string());

    let so_no = match row.so_id {
        Some(so_id) => shipment_model::so_no_of(db(), so_id)
            .await
            .map_err(|error| utils::db_error(error, "查询销售退货单原销售订单号"))?,
        None => None,
    };
    let shipment_no = match row.shipment_id {
        Some(shipment_id) => sales_return_model::shipment_no_of(db(), shipment_id)
            .await
            .map_err(|error| utils::db_error(error, "查询销售退货单原发货单号"))?,
        None => None,
    };
    let operator = match row.create_by {
        Some(user_id) => order_pdf_model::user_names_of(&[user_id])
            .await
            .map_err(|error| utils::db_error(error, "查询销售退货单经办人"))?
            .get(&user_id)
            .cloned()
            .unwrap_or_else(|| "—".to_string()),
        None => "—".to_string(),
    };

    let items = build_sales_return_items(&lines, &materials, &units, &locations);
    let total_qty: Decimal = lines.iter().map(|line| line.qty).sum();
    // 退货原因 = 退货类型 + 类型码（本表没有自由文本原因列，备注单列打印）
    let reason = format!(
        "{}（{}）",
        sales_return_model::return_type_label(&row.return_type),
        row.return_type
    );

    Ok(ReturnContext::new(ReturnContextArgs {
        kind: ReturnDocKind::Sales,
        doc_no: row.return_no.clone(),
        meta: vec![
            OrderMetaField::new("单号", row.return_no.clone()),
            OrderMetaField::new("退货日期", row.return_date.format("%Y-%m-%d").to_string()),
            OrderMetaField::new("状态", sales_return_model::status_label(&row.status)),
            OrderMetaField::new("客户", customer),
            OrderMetaField::new("退回仓库", warehouse),
            OrderMetaField::new("经办人", operator),
            // 验收③：原单号（销售订单 + 发货单）
            OrderMetaField::new("原单号（销售订单）", so_no.unwrap_or_else(|| "—".to_string())),
            OrderMetaField::new("原单号（发货单）", shipment_no.unwrap_or_else(|| "—".to_string())),
            OrderMetaField::new("退货类型", sales_return_model::return_type_label(&row.return_type)),
        ],
        reason,
        items,
        total_qty,
        total_amount: row.total_amount,
        total_tax: row.total_tax,
        total_with_tax: row.total_with_tax,
        remark: row.remark.clone().unwrap_or_default(),
        printed_at: doc_pdf_model::printed_at_now(),
    }))
}

// ==================== 导出（渲染 + 文件名） ====================

/// 导出退货单 PDF（取数 + 渲染，不含 HTTP 头 / 审计）。
///
/// # 异常情况
///
/// - 400 [`crate::shared::error_code::ERP_FILTER_INVALID`]：id 非法；
/// - 404：单据不存在；
/// - 422 / 503 / 504：模板 / 渲染引擎异常。
pub async fn export_return(kind: ReturnDocKind, id_raw: &str) -> std::result::Result<RenderedDoc, ApiError> {
    let id = doc_pdf_model::parse_doc_id(id_raw)?;
    let context = match kind {
        ReturnDocKind::Purchase => purchase_return_context(id).await?,
        ReturnDocKind::Sales => sales_return_context(id).await?,
    };
    let html = template::render_return_doc(&context).map_err(|error| error.into_api())?;
    let filename = doc_pdf_render::pdf_filename(&context.doc_label, &context.doc_no);
    let opts = template::return_pdf_opts(kind, &context.doc_no, Some(filename));
    doc_pdf_render::render_document(
        &html,
        &context.doc_no,
        &opts,
        context.items.len(),
        context_total(&context),
    )
    .await
    .map_err(|error| error.into_api())
}

/// 从上下文回读价税合计（审计用）。
fn context_total(context: &ReturnContext) -> Decimal {
    let digits: String = context
        .total_with_tax
        .chars()
        .filter(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == '-')
        .collect();
    digits.parse::<Decimal>().unwrap_or(Decimal::ZERO)
}

/// 采购退货单 PDF（`GET /api/business/erp/purchase-return/{id}/pdf`）。
///
/// # 异常情况
///
/// - 401：未登录；
/// - 403 [`crate::shared::error_code::PERMISSION_DENIED`]：缺 `business:erp:purchase-return:list`；
/// - 400 / 404 / 422 / 503 / 504：见 [`export_return`]。
#[nik::permission(
    path = "/business/erp/purchase-return/{id}/pdf",
    method = "GET",
    permission = "business:erp:purchase-return:list"
)]
#[nik::get("/business/erp/purchase-return/{id}/pdf")]
pub async fn purchase_return_pdf(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    export_http(ReturnDocKind::Purchase, &headers, &connect_info, &claims, &id).await
}

/// 销售退货单 PDF（`GET /api/business/erp/sales-return/{id}/pdf`）。
///
/// # 异常情况
///
/// - 401：未登录；
/// - 403 [`crate::shared::error_code::PERMISSION_DENIED`]：缺 `business:erp:sales-return:list`；
/// - 400 / 404 / 422 / 503 / 504：见 [`export_return`]。
#[nik::permission(
    path = "/business/erp/sales-return/{id}/pdf",
    method = "GET",
    permission = "business:erp:sales-return:list"
)]
#[nik::get("/business/erp/sales-return/{id}/pdf")]
pub async fn sales_return_pdf(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    export_http(ReturnDocKind::Sales, &headers, &connect_info, &claims, &id).await
}

/// 两个入口共用的 HTTP 编排：导出 → 组响应 → 补审计。
async fn export_http(
    kind: ReturnDocKind,
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    id_raw: &str,
) -> Response {
    let started = Instant::now();
    match export_return(kind, id_raw).await {
        Ok(rendered) => {
            let response = match pdf_handler::pdf_response(&rendered.bytes, Some(&rendered.filename)) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(
                        kind = kind.label(),
                        doc_no = %rendered.doc_no,
                        bytes = rendered.bytes.len(),
                        "退货单 PDF 响应组装失败"
                    );
                    return error.into_response();
                }
            };
            audit(kind, headers, connect_info, claims, id_raw, &rendered, started);
            response
        }
        Err(error) => error.into_response(),
    }
}

/// 补一条导出审计（失败只记日志，不改变已生成的响应）。
fn audit(
    kind: ReturnDocKind,
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    id_raw: &str,
    rendered: &RenderedDoc,
    started: Instant,
) {
    let Ok(doc_id) = doc_pdf_model::parse_doc_id(id_raw) else {
        return;
    };
    doc_pdf_model::ExportLogOwned {
        user_id: utils::current_user_uuid(claims),
        username: claims.username.clone(),
        operation: export_operation(kind).to_string(),
        target_type: kind.table().to_string(),
        doc_id,
        doc_no: rendered.doc_no.clone(),
        detail: serde_json::json!({
            "doc_type": kind.doc_kind_str(),
            "row_count": rendered.row_count,
            "amount": rendered.total_amount.round_dp(2).to_string(),
            "render_ms": rendered.duration_ms,
        }),
        filename: rendered.filename.clone(),
        bytes: rendered.bytes.len(),
        duration_ms: started.elapsed().as_millis() as u64,
        request_path: format!("/api/business/erp/{}/{doc_id}/pdf", path_segment(kind)),
        client_ip: utils::client_ip(headers, Some(connect_info)),
        user_agent: utils::user_agent(headers),
        request_id: Uuid::now_v7(),
    }
    .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn material(code: &str, name: &str, spec: &str, unit_id: Uuid) -> MaterialInfo {
        MaterialInfo {
            code: code.to_string(),
            name: name.to_string(),
            spec: spec.to_string(),
            unit_id,
        }
    }

    fn purchase_item(
        material_id: Uuid,
        unit_id: Uuid,
        location: Option<Uuid>,
        with_tax: Decimal,
    ) -> purchase_return_model::ItemRow {
        purchase_return_model::ItemRow {
            id: Uuid::now_v7(),
            line_no: 1,
            stock_in_item_id: Uuid::now_v7(),
            purchase_order_item_id: None,
            material_id,
            material_code: Some("ROW-CODE".to_string()),
            material_name: Some("行内名称".to_string()),
            unit_id,
            unit_name: Some("件".to_string()),
            qty: Decimal::new(20_000, 4),
            price: Decimal::new(123_400, 4),
            price_incl: Decimal::new(139_442, 4),
            tax_rate: Decimal::from(13),
            amount: Decimal::new(246_800, 4),
            tax_amount: Decimal::new(32_084, 4),
            with_tax_amount: with_tax,
            location_id: location,
            remark: None,
            received_qty: Decimal::ZERO,
        }
    }

    fn sales_item(
        material_id: Uuid,
        location: Option<Uuid>,
        serials: Vec<String>,
    ) -> sales_return_model::ReturnItemRow {
        sales_return_model::ReturnItemRow {
            id: Uuid::now_v7(),
            return_id: Uuid::now_v7(),
            so_item_id: None,
            material_id,
            qty: Decimal::from(2),
            price: Decimal::new(50_000, 4),
            amount: Decimal::new(100_000, 4),
            tax_rate: Decimal::from(13),
            tax_amount: Decimal::new(13_000, 4),
            location_id: location,
            serial_nos: serials,
            remark: None,
            line_no: 1,
        }
    }

    /// 采购退货明细：三档金额逐行齐全，库位走账行口径。
    #[test]
    fn purchase_items_have_all_amount_columns() {
        let material_id = Uuid::now_v7();
        let unit_id = Uuid::now_v7();
        let location = Uuid::now_v7();
        let sources = vec![
            purchase_item(material_id, unit_id, Some(location), Decimal::new(278_884, 4)),
            purchase_item(material_id, unit_id, None, Decimal::new(278_884, 4)),
        ];
        let mut materials = HashMap::new();
        materials.insert(material_id, material("MAT-1", "球阀", "DN50", unit_id));
        let mut units = HashMap::new();
        units.insert(unit_id, "台".to_string());
        let mut locations = HashMap::new();
        locations.insert(location, "A-01-03".to_string());

        let items = build_purchase_return_items(&sources, &materials, &units, &locations);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].seq, 1);
        assert_eq!(items[0].material_code, "ROW-CODE");
        assert_eq!(items[0].spec, "DN50");
        assert_eq!(items[0].unit, "件", "明细单位快照优先");
        assert_eq!(items[0].qty, "2", "20.0000 → 2");
        assert_eq!(items[0].price, "12.34");
        assert_eq!(items[0].amount, "24.68");
        assert_eq!(items[0].tax_rate, "13");
        assert_eq!(items[0].tax_amount, "3.21", "金额列按分四舍五入（2 位小数）");
        assert_eq!(items[0].with_tax, "27.89");
        assert_eq!(items[0].location, "A-01-03");
        assert_eq!(items[0].serials, "", "采购退货没有序列号列");
        assert_eq!(items[1].location, "未分配");
    }

    /// 销售退货明细：库位 `None` = 整仓；序列号顿号分隔；行级价税合计打横线。
    #[test]
    fn sales_items_mark_whole_warehouse_and_serials() {
        let material_id = Uuid::now_v7();
        let unit_id = Uuid::now_v7();
        let location = Uuid::now_v7();
        let sources = vec![
            sales_item(
                material_id,
                Some(location),
                vec!["SN-1".to_string(), "SN-2".to_string()],
            ),
            sales_item(material_id, None, Vec::new()),
        ];
        let mut materials = HashMap::new();
        materials.insert(material_id, material("MAT-2", "法兰", "DN80", unit_id));
        let mut units = HashMap::new();
        units.insert(unit_id, "台".to_string());
        let mut locations = HashMap::new();
        locations.insert(location, "B-02-04".to_string());

        let items = build_sales_return_items(&sources, &materials, &units, &locations);
        assert_eq!(items[0].unit, "台");
        assert_eq!(items[0].qty, "2");
        assert_eq!(items[0].amount, "10.00");
        assert_eq!(items[0].tax_amount, "1.30");
        assert_eq!(items[0].with_tax, "—", "销售退货没有行级价税合计");
        assert_eq!(items[0].location, "B-02-04");
        assert_eq!(items[0].serials, "SN-1、SN-2");
        assert_eq!(items[1].location, "整仓", "NULL = 整仓退货（真实语义）");
        assert_eq!(items[1].serials, "—", "没有序列号打横线");
    }

    /// 物料 / 单位 / 库位都查不到时走兜底（不 500、不编值）。
    #[test]
    fn missing_master_data_degrades_gracefully() {
        let material_id = Uuid::now_v7();
        let purchase = build_purchase_return_items(
            &[purchase_item(
                material_id,
                Uuid::now_v7(),
                Some(Uuid::now_v7()),
                Decimal::ONE,
            )],
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(purchase[0].material_code, "ROW-CODE");
        assert_eq!(purchase[0].unit, "件", "连表单位兜底");
        assert_eq!(purchase[0].location, "已删除的库位账行");

        let sales = build_sales_return_items(
            &[sales_item(material_id, Some(Uuid::now_v7()), Vec::new())],
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(sales[0].material_code, "（物料已删除）");
        assert_eq!(sales[0].unit, "—");
        assert_eq!(sales[0].location, "（库位已删除）");
    }

    /// 审计金额 = 价税合计（去千分位后精确两位）。
    #[test]
    fn context_total_reads_with_tax_total() {
        let ctx = template::sample_return(ReturnDocKind::Purchase, 2);
        let expected: Decimal = ctx.total_with_tax.replace(',', "").parse().unwrap();
        assert_eq!(context_total(&ctx), expected);
        assert_eq!(ctx.amount_for_upper, ctx.total_with_tax, "大写与小写同源");
    }

    /// 两个入口的操作码 / 权限码 / 路径段一一对应（点错名字就是点错权限）。
    #[test]
    fn kind_metadata_is_complete() {
        assert_eq!(export_operation(ReturnDocKind::Purchase), "EXPORT_PURCHASE_RETURN_PDF");
        assert_eq!(export_operation(ReturnDocKind::Sales), "EXPORT_SALES_RETURN_PDF");
        assert_eq!(
            permission_code(ReturnDocKind::Purchase),
            "business:erp:purchase-return:list"
        );
        assert_eq!(permission_code(ReturnDocKind::Sales), "business:erp:sales-return:list");
        assert_eq!(path_segment(ReturnDocKind::Purchase), "purchase-return");
        assert_eq!(path_segment(ReturnDocKind::Sales), "sales-return");
        // 两个 kind 不许撞车
        let set: HashSet<&str> = [
            export_operation(ReturnDocKind::Purchase),
            export_operation(ReturnDocKind::Sales),
            permission_code(ReturnDocKind::Purchase),
            permission_code(ReturnDocKind::Sales),
            path_segment(ReturnDocKind::Purchase),
            path_segment(ReturnDocKind::Sales),
        ]
        .into_iter()
        .collect();
        assert_eq!(set.len(), 6);
    }
}

// ==================== 真库用例（ERP_DB_URL；未设置则 SKIP） ====================

/// 真库用例：采购退货 / 销售退货的取数口径（**原单号 + 退货原因**）。
///
/// # 跑法
///
/// ```bash
/// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/<测试库>'
/// cargo test --lib business::erp::return_pdf -- --test-threads=1
/// ```
///
/// # 夹具卫生
///
/// 独占命名空间 `0f4300xx`（采购侧）、`0f4400xx`（销售侧）。
#[cfg(test)]
mod db_tests {
    use super::*;

    // 夹具 id 命名空间 0f43…（采购侧）/ 0f44…（销售侧），只在 SQL 字面量里用：
    //   0f43 0001 单位 / 0002 物料 / 0003 操作人 / 0004 供应商 / 0005 仓库 /
    //        0006 库位字典 / 0007 库位账行 / 0008 入库单 / 0009 入库明细 /
    //        000a 采购订单 / 0010 采购退货单
    //   0f44 0001 客户 / 0002 销售订单 / 0003 发货单 / 0010 销售退货单
    const OPERATOR: Uuid = Uuid::from_u128(0x0f43_0000_0000_4000_8000_0000_0000_0003);
    const PURCHASE_RETURN: Uuid = Uuid::from_u128(0x0f43_0000_0000_4000_8000_0000_0000_0010);
    const SALES_RETURN: Uuid = Uuid::from_u128(0x0f44_0000_0000_4000_8000_0000_0000_0010);

    const PRT_NO: &str = "PDF04-PRT-000001";
    const IN_NO: &str = "PDF04-IN-000001";
    const PO_NO: &str = "PDF04-PO-000001";
    const SR_NO: &str = "PDF04-SR-000001";
    const SO_NO: &str = "PDF04-SO-000001";
    const SH_NO: &str = "PDF04-SH-000001";

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 表头文案存在性：**不绑定 `th` 上有没有 class**。
    ///
    /// PDF-04（提交 2ac33d11）把退货单表头改成 `<th class="c-…">文案</th>`——
    /// `table-layout: fixed` 只认首行单元格的 width，列宽 class 必须留在 `th` 上。
    /// 旧断言逐字匹配 `<th>文案</th>`，模板一加 class 就必然假红（产品行为没变）。
    /// 这里只固定三件事：标签名是 `th`、属性随便、正文恰好是该文案。
    fn th_has_label(html: &str, label: &str) -> bool {
        let pattern = format!(r"<th\b[^>]*>\s*{}\s*</th>", regex::escape(label));
        regex::Regex::new(&pattern).expect("表头正则编译失败").is_match(html)
    }

    async fn exec(sql: &str, params: Vec<Value>, label: &str) {
        db().execute_with_params(sql, params)
            .await
            .unwrap_or_else(|error| panic!("{label}：{error}\nSQL: {sql}"));
    }

    async fn purge() {
        for sql in [
            // 序列号行引用物料 / 库位 / 仓库 → 先清（否则 FK 拦住删库位）
            "DELETE FROM biz_material_serial WHERE material_id IN \
                (SELECT id FROM biz_material WHERE material_code LIKE 'PDF04-%')",
            "DELETE FROM biz_material_serial WHERE location_id IN \
                ('0f430000-0000-4000-8000-000000000006'::uuid, '0f430000-0000-4000-8000-000000000007'::uuid)",
            "DELETE FROM biz_material_serial WHERE warehouse_id IN \
                (SELECT id FROM biz_warehouse WHERE warehouse_code LIKE 'WH-PDF04-%')",
            "DELETE FROM biz_material_serial WHERE source_id IN \
                ('0f430000-0000-4000-8000-000000000008'::uuid, '0f440000-0000-4000-8000-000000000010'::uuid)",
            // 单据：先按单号前缀清（兜住老 id 的残行），再按 id 清
            "DELETE FROM biz_purchase_return_item WHERE return_id IN \
                (SELECT id FROM biz_purchase_return WHERE return_no LIKE 'PDF04-%')",
            "DELETE FROM biz_purchase_return WHERE return_no LIKE 'PDF04-%'",
            "DELETE FROM biz_stock_in_item WHERE stock_in_id IN \
                (SELECT id FROM biz_stock_in WHERE in_no LIKE 'PDF04-%')",
            "DELETE FROM biz_stock_in WHERE in_no LIKE 'PDF04-%'",
            "DELETE FROM biz_purchase_order WHERE order_no LIKE 'PDF04-%'",
            "DELETE FROM biz_sales_return_item WHERE return_id IN \
                (SELECT id FROM biz_sales_return WHERE return_no LIKE 'PDF04-%')",
            "DELETE FROM biz_sales_return WHERE return_no LIKE 'PDF04-%'",
            "DELETE FROM biz_shipment WHERE ship_no LIKE 'PDF04-%'",
            "DELETE FROM biz_sales_order WHERE so_no LIKE 'PDF04-%'",
            "DELETE FROM biz_purchase_return_item WHERE return_id = '0f430000-0000-4000-8000-000000000010'::uuid",
            "DELETE FROM biz_purchase_return WHERE id = '0f430000-0000-4000-8000-000000000010'::uuid",
            "DELETE FROM operation_logs WHERE target_id IN \
                ('0f430000-0000-4000-8000-000000000010', '0f440000-0000-4000-8000-000000000010')",
            "DELETE FROM biz_stock_in_item WHERE stock_in_id = '0f430000-0000-4000-8000-000000000008'::uuid",
            "DELETE FROM biz_stock_in WHERE id = '0f430000-0000-4000-8000-000000000008'::uuid",
            "DELETE FROM biz_purchase_order WHERE id = '0f430000-0000-4000-8000-00000000000a'::uuid",
            "DELETE FROM biz_sales_return_item WHERE return_id = '0f440000-0000-4000-8000-000000000010'::uuid",
            "DELETE FROM biz_sales_return WHERE id = '0f440000-0000-4000-8000-000000000010'::uuid",
            "DELETE FROM biz_shipment WHERE id = '0f440000-0000-4000-8000-000000000003'::uuid",
            "DELETE FROM biz_sales_order WHERE id = '0f440000-0000-4000-8000-000000000002'::uuid",
            "DELETE FROM biz_customer WHERE id = '0f440000-0000-4000-8000-000000000001'::uuid",
            "DELETE FROM biz_inventory_location WHERE id = '0f430000-0000-4000-8000-000000000007'::uuid",
            "DELETE FROM biz_location WHERE id = '0f430000-0000-4000-8000-000000000006'::uuid",
            // 主数据：按 id + 按业务码双保险（命名空间改过一次，老行的旧码可能不匹配新前缀）
            "DELETE FROM biz_material WHERE id = '0f430000-0000-4000-8000-000000000002'::uuid",
            "DELETE FROM biz_supplier WHERE id = '0f430000-0000-4000-8000-000000000004'::uuid",
            "DELETE FROM biz_warehouse WHERE id = '0f430000-0000-4000-8000-000000000005'::uuid",
            "DELETE FROM sys_user WHERE id = '0f430000-0000-4000-8000-000000000003'::uuid",
            "DELETE FROM biz_unit WHERE id = '0f430000-0000-4000-8000-000000000001'::uuid",
            "DELETE FROM biz_material WHERE material_code LIKE 'PDF04-%'",
            "DELETE FROM biz_supplier WHERE supplier_code LIKE 'PDF04-%'",
            "DELETE FROM biz_customer WHERE code LIKE 'PDF04-%'",
            "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'WH-PDF04-%'",
            "DELETE FROM sys_user WHERE username LIKE 'pdf04.%'",
            "DELETE FROM biz_unit WHERE unit_code LIKE 'PDF04-%'",
        ] {
            exec(sql, vec![], "清理 PDF-04 退货夹具").await;
        }
    }

    async fn seed_master() {
        for sql in [
            "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, status, sort_order)
             VALUES ('0f430000-0000-4000-8000-000000000001', 'PDF04-RT-PCS', '件', 1, 1, 95)",
            "INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                                       safety_stock, costing_method, tax_rate, status, serial_required)
             VALUES ('0f430000-0000-4000-8000-000000000002', 'PDF04-RT-MAT', '不锈钢弯头',
                     'DN50/PN16', '0f430000-0000-4000-8000-000000000001', 'RAW',
                     0.0000, 'MOVING_AVG', 0.1300, 1, FALSE)",
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
             VALUES ('0f430000-0000-4000-8000-000000000004', 'PDF04-RT-SUP', 'PDF04 退货供应商', 1)",
            "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path,
                                        level, is_leaf, status)
             VALUES ('0f430000-0000-4000-8000-000000000005', 'WH-PDF04-RT', 'PDF04 退货仓',
                     '/0f430000-0000-4000-8000-000000000005/', 1, 1, 1)",
            "INSERT INTO sys_user (id, username, password, real_name, status)
             VALUES ('0f430000-0000-4000-8000-000000000003', 'pdf04.rt', 'x', '退货测试员', 1)",
            "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, status)
             VALUES ('0f430000-0000-4000-8000-000000000006', 'R-01-01', 'R 区 01 排 01 位',
                     '0f430000-0000-4000-8000-000000000005', 1)",
            "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty)
             VALUES ('0f430000-0000-4000-8000-000000000007',
                     '0f430000-0000-4000-8000-000000000002',
                     '0f430000-0000-4000-8000-000000000005',
                     '0f430000-0000-4000-8000-000000000006', 0)",
            "INSERT INTO biz_customer (id, code, name, credit_days, credit_limit, credit_used, status)
             VALUES ('0f440000-0000-4000-8000-000000000001', 'PDF04-RT-CUS', 'PDF04 退货客户',
                     30, 0, 0, 'ACTIVE')",
        ] {
            exec(sql, vec![], "写入 PDF-04 退货主数据夹具").await;
        }
    }

    /// 采购退货夹具：入库单 + 采购订单 + 退货单（已确认 → 出库单号也印）。
    async fn seed_purchase_return() {
        exec(
            "INSERT INTO biz_purchase_order
                (id, order_no, supplier_id, order_date, expect_date, warehouse_id, status,
                 currency_code, total_qty, total_amount, total_tax, total_with_tax, credit_days,
                 supplier_name_snapshot, warehouse_name_snapshot, create_by)
             VALUES ('0f430000-0000-4000-8000-00000000000a', ?, 
                     '0f430000-0000-4000-8000-000000000004'::uuid, '2026-11-01', '2026-11-08',
                     '0f430000-0000-4000-8000-000000000005'::uuid, 'APPROVED', 'CNY',
                     10, 100, 13, 113, 30, 'PDF04 退货供应商', 'PDF04 退货仓',
                     '0f430000-0000-4000-8000-000000000003'::uuid)",
            vec![Value::Text(PO_NO.to_string())],
            "写入采购订单夹具",
        )
        .await;
        exec(
            "INSERT INTO biz_stock_in
                (id, in_no, in_type, source_order_id, is_sourceless, supplier_id, warehouse_id,
                 in_date, status, currency_code, total_qty, total_amount, total_tax,
                 total_with_tax, create_by)
             VALUES ('0f430000-0000-4000-8000-000000000008', ?, 'PURCHASE',
                     '0f430000-0000-4000-8000-00000000000a'::uuid, FALSE,
                     '0f430000-0000-4000-8000-000000000004'::uuid,
                     '0f430000-0000-4000-8000-000000000005'::uuid, '2026-11-05', 'POSTED', 'CNY',
                     10, 100, 13, 113, '0f430000-0000-4000-8000-000000000003'::uuid)",
            vec![Value::Text(IN_NO.to_string())],
            "写入收货入库单夹具",
        )
        .await;
        exec(
            "INSERT INTO biz_stock_in_item
                (id, stock_in_id, line_no, material_id, unit_id, qty, price, price_incl,
                 tax_rate, amount, tax_amount, with_tax_amount, location_id, lot_no)
             VALUES ('0f430000-0000-4000-8000-000000000009',
                     '0f430000-0000-4000-8000-000000000008'::uuid, 1,
                     '0f430000-0000-4000-8000-000000000002'::uuid,
                     '0f430000-0000-4000-8000-000000000001'::uuid, 10, 10, 11.3, 13,
                     100, 13, 113, '0f430000-0000-4000-8000-000000000007'::uuid, 'LOT-PDF04-A')",
            vec![],
            "写入入库明细夹具",
        )
        .await;
        exec(
            "INSERT INTO biz_purchase_return
                (id, return_no, stock_in_id, purchase_order_id, supplier_id, warehouse_id,
                 return_date, status, total_qty, total_amount, total_tax, total_with_tax,
                 reason, remark, create_by)
             VALUES ('0f430000-0000-4000-8000-000000000010', ?,
                     '0f430000-0000-4000-8000-000000000008'::uuid,
                     '0f430000-0000-4000-8000-00000000000a'::uuid,
                     '0f430000-0000-4000-8000-000000000004'::uuid,
                     '0f430000-0000-4000-8000-000000000005'::uuid, '2026-11-08', 'DRAFT',
                     2, 20, 2.6, 22.6, '阀体密封面有砂眼，供应商同意退货', '附质检报告一份',
                     '0f430000-0000-4000-8000-000000000003'::uuid)",
            vec![Value::Text(PRT_NO.to_string())],
            "写入采购退货单夹具",
        )
        .await;
        exec(
            "INSERT INTO biz_purchase_return_item
                (id, return_id, line_no, stock_in_item_id, purchase_order_item_id, material_id,
                 unit_id, qty, price, price_incl, tax_rate, amount, tax_amount, with_tax_amount,
                 location_id, remark)
             VALUES ('0f430000-0000-4000-8000-000000000021',
                     '0f430000-0000-4000-8000-000000000010'::uuid, 1,
                     '0f430000-0000-4000-8000-000000000009'::uuid, NULL,
                     '0f430000-0000-4000-8000-000000000002'::uuid,
                     '0f430000-0000-4000-8000-000000000001'::uuid,
                     2, 10, 11.3, 13, 20, 2.6, 22.6,
                     '0f430000-0000-4000-8000-000000000007'::uuid, '整批退')",
            vec![],
            "写入采购退货明细夹具",
        )
        .await;
    }

    /// 销售退货夹具：订单 + 发货单 + 退货单（质量退货、带序列号、指定库位）。
    async fn seed_sales_return() {
        exec(
            "INSERT INTO biz_sales_order
                (id, so_no, customer_id, order_date, delivery_date, status,
                 total_amount, total_tax, total_with_tax, create_by)
             VALUES ('0f440000-0000-4000-8000-000000000002', ?,
                     '0f440000-0000-4000-8000-000000000001'::uuid, '2026-11-01', '2026-11-08',
                     'APPROVED', 100, 13, 113, '0f430000-0000-4000-8000-000000000003'::uuid)",
            vec![Value::Text(SO_NO.to_string())],
            "写入销售订单夹具",
        )
        .await;
        exec(
            "INSERT INTO biz_shipment
                (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty)
             VALUES ('0f440000-0000-4000-8000-000000000003', ?,
                     '0f440000-0000-4000-8000-000000000002'::uuid,
                     '0f440000-0000-4000-8000-000000000001'::uuid,
                     '0f430000-0000-4000-8000-000000000005'::uuid, '2026-11-06', 'POSTED', 10)",
            vec![Value::Text(SH_NO.to_string())],
            "写入发货单夹具",
        )
        .await;
        exec(
            "INSERT INTO biz_sales_return
                (id, return_no, customer_id, so_id, shipment_id, return_date, return_type,
                 warehouse_id, status, total_amount, total_tax, total_with_tax, remark, create_by)
             VALUES ('0f440000-0000-4000-8000-000000000010', ?,
                     '0f440000-0000-4000-8000-000000000001'::uuid,
                     '0f440000-0000-4000-8000-000000000002'::uuid,
                     '0f440000-0000-4000-8000-000000000003'::uuid, '2026-11-08', 'QUALITY',
                     '0f430000-0000-4000-8000-000000000005'::uuid, 'DRAFT',
                     20, 2.6, 22.6, '客户反馈焊缝渗漏，整批退回',
                     '0f430000-0000-4000-8000-000000000003'::uuid)",
            vec![Value::Text(SR_NO.to_string())],
            "写入销售退货单夹具",
        )
        .await;
        exec(
            "INSERT INTO biz_sales_return_item
                (id, return_id, so_item_id, material_id, qty, price, amount, tax_rate,
                 tax_amount, location_id, serial_nos, remark)
             VALUES ('0f440000-0000-4000-8000-000000000021',
                     '0f440000-0000-4000-8000-000000000010'::uuid, NULL,
                     '0f430000-0000-4000-8000-000000000002'::uuid,
                     2, 10, 20, 13, 2.6,
                     '0f430000-0000-4000-8000-000000000006'::uuid,
                     '{PDF04-SN-1,PDF04-SN-2}', '序列号件退回')",
            vec![],
            "写入销售退货明细夹具",
        )
        .await;
    }

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        purge().await;
        seed_master().await;
        Some(guard)
    }

    /// 验收③（采购）：原单号（入库 + 采购订单）+ 退货原因 + 供应商 + 库位。
    #[tokio::test]
    async fn purchase_return_context_has_source_numbers_and_reason() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_purchase_return().await;
        let ctx = purchase_return_context(PURCHASE_RETURN)
            .await
            .expect("上下文必须组装成功");

        assert_eq!(ctx.doc_no, PRT_NO);
        assert_eq!(ctx.doc_title, "采 购 退 货 单");
        let meta: Vec<(String, String)> = ctx
            .meta_rows
            .iter()
            .flatten()
            .map(|field| (field.label.clone(), field.value.clone()))
            .collect();
        // 验收③：原单号
        assert!(meta.contains(&("原单号（入库）".into(), IN_NO.into())), "{meta:?}");
        assert!(meta.contains(&("原单号（采购订单）".into(), PO_NO.into())), "{meta:?}");
        // 验收③：退货原因
        assert!(ctx.reason.contains("砂眼"));
        assert!(meta.contains(&("供应商".into(), "PDF04 退货供应商".into())));
        assert!(meta.contains(&("退货仓库".into(), "PDF04 退货仓".into())));
        assert!(meta.contains(&("经办人".into(), "退货测试员".into())));

        assert_eq!(ctx.items.len(), 1);
        assert_eq!(ctx.items[0].material_code, "PDF04-RT-MAT");
        assert_eq!(ctx.items[0].unit, "件");
        assert_eq!(ctx.items[0].qty, "2");
        assert_eq!(ctx.items[0].amount, "20.00");
        assert_eq!(ctx.items[0].tax_amount, "2.60");
        assert_eq!(ctx.items[0].with_tax, "22.60");
        assert_eq!(ctx.items[0].tax_rate, "13");
        // 采购退货库位 = 库位账行 → 字典编码（R-01-01）
        assert_eq!(ctx.items[0].location, "R-01-01");
        assert_eq!(ctx.total_qty, "2");
        assert_eq!(ctx.total_amount, "20.00");
        assert_eq!(ctx.total_tax, "2.60");
        assert_eq!(ctx.total_with_tax, "22.60");
        assert_eq!(ctx.amount_cn, "贰拾贰元陆角整");

        let html = template::render_return_doc(&ctx).expect("模板必须渲染成功");
        assert!(html.contains(IN_NO));
        assert!(html.contains(PO_NO));
        assert!(html.contains("退货原因："));
        assert!(html.contains("砂眼"));
        assert!(
            th_has_label(&html, "价税合计"),
            "采购退货明细表头必须有「价税合计」列（th 带列宽 class 也算）"
        );
        purge().await;
    }

    /// 验收③（销售）：原销售订单 / 原发货单 + 退货类型 + 客户 + 序列号。
    #[tokio::test]
    async fn sales_return_context_has_source_numbers_and_reason() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_sales_return().await;
        let ctx = sales_return_context(SALES_RETURN).await.expect("上下文必须组装成功");

        assert_eq!(ctx.doc_no, SR_NO);
        assert_eq!(ctx.doc_title, "销 售 退 货 单");
        let meta: Vec<(String, String)> = ctx
            .meta_rows
            .iter()
            .flatten()
            .map(|field| (field.label.clone(), field.value.clone()))
            .collect();
        assert!(meta.contains(&("原单号（销售订单）".into(), SO_NO.into())), "{meta:?}");
        assert!(meta.contains(&("原单号（发货单）".into(), SH_NO.into())), "{meta:?}");
        // 退货原因 = 退货类型
        assert!(ctx.reason.contains("质量"), "{}", ctx.reason);
        assert!(ctx.reason.contains("QUALITY"));
        assert!(meta.contains(&("客户".into(), "PDF04 退货客户".into())));
        assert!(meta.contains(&("退回仓库".into(), "PDF04 退货仓".into())));
        assert!(meta.contains(&("退货类型".into(), "质量".into())));

        assert_eq!(ctx.items.len(), 1);
        assert_eq!(ctx.items[0].material_code, "PDF04-RT-MAT");
        assert_eq!(ctx.items[0].serials, "PDF04-SN-1、PDF04-SN-2");
        // 销售退货库位 = 库位字典（R-01-01），与采购侧同码不同源
        assert_eq!(ctx.items[0].location, "R-01-01");
        assert_eq!(ctx.items[0].with_tax, "—");
        assert_eq!(ctx.total_with_tax, "22.60");

        let html = template::render_return_doc(&ctx).expect("模板必须渲染成功");
        assert!(html.contains(SO_NO));
        assert!(html.contains(SH_NO));
        assert!(html.contains("PDF04-SN-1"));
        assert!(
            th_has_label(&html, "序列号"),
            "销售退货明细表头必须有「序列号」列（th 带列宽 class 也算）"
        );
        assert!(
            !th_has_label(&html, "价税合计"),
            "销售退货没有行级价税合计列（模板按 kind 不渲染该 th）"
        );
        purge().await;
    }

    /// 审计：两种退货单的 operation / target_type 各自落对。
    #[tokio::test]
    async fn export_writes_operation_log_rows() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_purchase_return().await;
        seed_sales_return().await;
        let purchase_ctx = purchase_return_context(PURCHASE_RETURN).await.expect("采购上下文");
        let sales_ctx = sales_return_context(SALES_RETURN).await.expect("销售上下文");

        for (kind, doc_id, doc_no, rows, amount) in [
            (
                ReturnDocKind::Purchase,
                PURCHASE_RETURN,
                PRT_NO,
                purchase_ctx.items.len(),
                purchase_ctx.total_with_tax.clone(),
            ),
            (
                ReturnDocKind::Sales,
                SALES_RETURN,
                SR_NO,
                sales_ctx.items.len(),
                sales_ctx.total_with_tax.clone(),
            ),
        ] {
            let request_id = Uuid::now_v7();
            doc_pdf_model::write_export_log(&doc_pdf_model::ExportLog {
                user_id: Some(OPERATOR),
                username: Some("pdf04.rt"),
                operation: export_operation(kind),
                target_type: kind.table(),
                doc_id,
                doc_no,
                detail: serde_json::json!({
                    "doc_type": kind.doc_kind_str(),
                    "row_count": rows,
                    "amount": amount,
                }),
                filename: "退货单.pdf",
                bytes: 70_000,
                duration_ms: 200,
                request_path: "/api/business/erp/x/pdf",
                client_ip: Some(std::net::IpAddr::from([127, 0, 0, 1])),
                user_agent: Some("curl/8.5.0".to_string()),
                request_id,
            })
            .await
            .expect("审计写入必须成功");

            let found = db()
                .find_with_params(
                    "SELECT operation, target_type, detail FROM operation_logs \
                      WHERE target_id = ? AND request_id = ?",
                    vec![Value::Text(doc_id.to_string()), Value::Text(request_id.to_string())],
                )
                .await
                .expect("回查审计行");
            assert_eq!(found.len(), 1);
            assert_eq!(
                found[0].get_typed::<String>("operation").unwrap(),
                export_operation(kind)
            );
            assert_eq!(found[0].get_typed::<String>("target_type").unwrap(), kind.table());
            let detail: serde_json::Value = found[0].get_typed("detail").expect("detail");
            assert_eq!(detail["doc_type"], kind.doc_kind_str());
        }
        purge().await;
    }

    /// 软删单据必须 404（两种退货单各验一次）。
    #[tokio::test]
    async fn soft_deleted_returns_are_not_found() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_purchase_return().await;
        exec(
            "UPDATE biz_purchase_return SET deleted_at = now() \
              WHERE id = '0f430000-0000-4000-8000-000000000010'::uuid",
            vec![],
            "软删采购退货单",
        )
        .await;
        let error = purchase_return_context(PURCHASE_RETURN)
            .await
            .expect_err("软删单必须 404");
        assert_eq!(error.status, 404);
        assert!(error.msg.contains("采购退货单"));
        purge().await;
        // purge 连主数据一起清了 → 第二次造数前必须重新种
        seed_master().await;

        seed_sales_return().await;
        exec(
            "UPDATE biz_sales_return SET deleted_at = now() \
              WHERE id = '0f440000-0000-4000-8000-000000000010'::uuid",
            vec![],
            "软删销售退货单",
        )
        .await;
        let error = sales_return_context(SALES_RETURN).await.expect_err("软删单必须 404");
        assert_eq!(error.status, 404);
        assert!(error.msg.contains("销售退货单"));
        purge().await;
    }

    /// id 非法 → 400（两种单据共用一份解析）。
    #[tokio::test]
    async fn export_rejects_bad_uuid_before_query() {
        if !ensure_db().await {
            return;
        }
        for kind in [ReturnDocKind::Purchase, ReturnDocKind::Sales] {
            let error = export_return(kind, "not-a-uuid").await.expect_err("必须 400");
            assert_eq!(error.status, 400);
            assert_eq!(error.code, crate::shared::error_code::ERP_FILTER_INVALID);
        }
    }
}
