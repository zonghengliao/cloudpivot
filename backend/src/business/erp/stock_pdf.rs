//! 收货入库单 / 出库单 **PDF 导出**（PDF-03）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/business/...`）
//!
//! | 方法 | 路径 | 权限码（复用单据查看权限） | 说明 |
//! |------|------|------------------------------|------|
//! | GET | `/business/erp/stock-in/{id}/pdf` | `business:erp:stock-in:list` | 收货入库单 PDF |
//! | GET | `/business/erp/stock-out/{id}/pdf` | `business:erp:stock-out:list` | 出库单 PDF |
//!
//! # 卡面要求的字段落点
//!
//! | 要求 | 数据来源 |
//! |------|----------|
//! | 仓库 | `biz_stock_in/out.warehouse_id` → `biz_warehouse.warehouse_name` |
//! | 库位 | 明细 `location_id`（**库位账行** `biz_inventory_location.id`）→ `biz_location.location_code`；单据头另给一个「库位」汇总（≤3 个全列，超过则「多库位（见明细）」） |
//! | 经办人 | `create_by` → `sys_user`（`real_name` 优先，离职员工也照实打印） |
//! | 供应商 / 客户 | 入库单：`supplier_id` → `biz_supplier`；出库单：销售发货生成的单子取来源发货单的客户，手工建单打「—」（**不编客户**） |
//! | 来源单据号 | 入库单：`source_order_id` → `biz_purchase_order.order_no`（无源入库打「—」）；出库单：`source_type='SALES'` 时 `source_id` → `biz_shipment.ship_no` |
//! | 明细批次号 | `lot_no`（**验收②**：入库单必须打出来） |
//! | 序列号 | 一物一码的单据：`biz_material_serial.source_id = 单据 id`，按物料分组列成「序列号清单」 |
//! | 收货 / 发货签字栏 | 入库：收货人 / 质检 / 仓管 / 供应商送货人 / 制单；出库：发货人 / 领用人 / 仓管 / 审核 / 制单 |
//!
//! # 金额口径（**只读不重算**）
//!
//! 单头与明细的金额都是**服务端过账时写好的权威值**：
//! 入库 = 不含税金额 / 税额 / 价税合计三档；出库 = **成本结转金额**（账上真实差额，
//! 不是 `qty × avg_cost` 重算 —— 见 `stockout_model` 文件头的金额红线）。
//! 本模块唯一的「算」是展示层：元 → 分 → 千分位 / 人民币大写，全部复用
//! [`crate::business::pdf::template`]，与对账单 / 订单 PDF 同一套实现。
//!
//! # 审计
//!
//! 导出是 GET，中间件不记；渲染成功后显式补 `EXPORT_STOCK_IN_PDF` /
//! `EXPORT_STOCK_OUT_PDF` 一行（写日志失败只记 error，不改响应）。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::doc_pdf_model::{self, ExportLogOwned};
use crate::business::erp::order_pdf_model::{self, MaterialInfo};
use crate::business::erp::{stockin_model, stockout_model};
use crate::business::pdf::template::{
    self, OrderMetaField, SerialGroup, StockContext, StockContextArgs, StockDocKind, StockItem, format_money,
    format_quantity, yuan_to_fen,
};
use crate::business::pdf::{engine, handler as pdf_handler};
use crate::utils;

/// 出库单来源类型：销售发货（SO-10，`source_id` = `biz_shipment.id`）。
pub const SOURCE_TYPE_SALES: &str = "SALES";

/// 审计里的操作码。
pub fn export_operation(kind: StockDocKind) -> &'static str {
    match kind {
        StockDocKind::StockIn => "EXPORT_STOCK_IN_PDF",
        StockDocKind::StockOut => "EXPORT_STOCK_OUT_PDF",
    }
}

/// 路由权限码（复用单据查看权限）。
pub fn permission_code(kind: StockDocKind) -> &'static str {
    match kind {
        StockDocKind::StockIn => "business:erp:stock-in:list",
        StockDocKind::StockOut => "business:erp:stock-out:list",
    }
}

/// 入库类型中文名（`ck_biz_si_type` 四值域，SOURCELESS 由 V0330 加宽）。
pub fn in_type_label(in_type: &str) -> &'static str {
    match in_type {
        "PURCHASE" => "采购入库",
        "RETURN" => "退货入库",
        "OTHER" => "其他入库",
        "SOURCELESS" => "无源入库",
        _ => "未知类型",
    }
}

/// 出库类型中文名（`ck_biz_so_type` 四值域）。
pub fn out_type_label(out_type: &str) -> &'static str {
    match out_type {
        "ISSUE" => "领用",
        "SCRAP" => "报废",
        "LOSS" => "盘亏",
        "OTHER" => "其他",
        _ => "未知类型",
    }
}

/// 明细的原始数据（入库 / 出库两套表结构归一到这里，只留打印需要的列）。
#[derive(Debug, Clone, PartialEq)]
pub struct StockItemSource {
    /// 物料
    pub material_id: Uuid,
    /// 单位（两套表都是 NOT NULL）
    pub unit_id: Uuid,
    /// 数量
    pub qty: Decimal,
    /// 单价（入库 = 不含税进价；出库 = 成本单价快照）
    pub price: Decimal,
    /// 金额（入库 = 不含税金额；出库 = 账上真实扣减金额）
    pub amount: Decimal,
    /// 批次号
    pub lot_no: Option<String>,
    /// 库位账行（`biz_inventory_location.id`，`None` = 未分配桶）
    pub location_id: Option<Uuid>,
}

/// 明细行 → 模板行（纯函数，单测直接调）。
///
/// # 参数
///
/// - `sources`：明细的**原始**行（已按行号升序）；
/// - `materials`：物料主档批量映射；
/// - `units`：单位名称批量映射；
/// - `locations`：库位账行 id → 库位编码。
///
/// # 序号口径
///
/// `#` 列是**打印序号**（1..n，跨页连续），不是库里的 `line_no`（改单会留洞）。
///
/// # 兜底口径（宁可显示横线也不编值）
///
/// - 物料主档查不到 → 「（物料已删除）」；
/// - 单位查不到 → `—`；
/// - 批次号为空 → `—`；
/// - 库位为 `None` → 「未分配」（未分配库位桶，是**真实语义**不是缺失）；
/// - 库位账行存在但库位编码查不到 → 「已删除的库位账行」。
pub fn build_stock_items(
    sources: &[StockItemSource],
    materials: &HashMap<Uuid, MaterialInfo>,
    units: &HashMap<Uuid, String>,
    locations: &HashMap<Uuid, String>,
) -> Vec<StockItem> {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let material = materials.get(&source.material_id);
            StockItem {
                seq: index as u32 + 1,
                material_code: material
                    .map(|info| info.code.clone())
                    .unwrap_or_else(|| "（物料已删除）".to_string()),
                material_name: material
                    .map(|info| info.name.clone())
                    .unwrap_or_else(|| "（物料已删除）".to_string()),
                spec: material.map(|info| info.spec.clone()).unwrap_or_default(),
                unit: units.get(&source.unit_id).cloned().unwrap_or_else(|| "—".to_string()),
                qty: format_quantity(source.qty),
                price: format_quantity(source.price),
                amount: format_money(yuan_to_fen(source.amount)),
                lot_no: source
                    .lot_no
                    .as_deref()
                    .map(str::trim)
                    .filter(|lot| !lot.is_empty())
                    .unwrap_or("—")
                    .to_string(),
                location: match source.location_id {
                    None => "未分配".to_string(),
                    Some(id) => locations
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| "已删除的库位账行".to_string()),
                },
            }
        })
        .collect()
}

/// 序列号清单 → 按「物料首次出现的顺序」分组的展示行。
///
/// # 参数
///
/// - `sources`：明细原始行（决定物料顺序）；
/// - `materials`：物料主档（补编码 / 名称）；
/// - `serials`：`HashMap<物料 id, Vec<序列号>>`（来自 `biz_material_serial`）。
///
/// # 为什么按明细顺序而不是按物料 id 排
///
/// 打印件要能对着明细逐行核对：序列号清单的物料顺序必须与明细表一致，
/// 用 `material_id` 排序会出现「清单顺序跳来跳去」的观感缺陷。
/// 明细里没出现的物料（不该有，防线）不打印。
pub fn build_serial_groups(
    sources: &[StockItemSource],
    materials: &HashMap<Uuid, MaterialInfo>,
    serials: &HashMap<Uuid, Vec<String>>,
) -> Vec<SerialGroup> {
    let mut seen: Vec<Uuid> = Vec::new();
    for source in sources {
        if serials.contains_key(&source.material_id) && !seen.contains(&source.material_id) {
            seen.push(source.material_id);
        }
    }
    seen.iter()
        .enumerate()
        .map(|(index, material_id)| {
            let material = materials.get(material_id);
            SerialGroup {
                seq: index as u32 + 1,
                material_code: material
                    .map(|info| info.code.clone())
                    .unwrap_or_else(|| "（物料已删除）".to_string()),
                material_name: material
                    .map(|info| info.name.clone())
                    .unwrap_or_else(|| "（物料已删除）".to_string()),
                numbers: serials.get(material_id).map(|list| list.join("、")).unwrap_or_default(),
            }
        })
        .collect()
}

/// 单据头的「库位」汇总：最多列 3 个，超过就写「多库位（见明细）」。
///
/// 单据头没有库位列（库位是明细级），但卡面要求打出来；
/// 全列 50 行会撑爆表头，所以做这层收敛 —— 信息不失真（明细里逐行都有）。
pub fn location_summary(locations: &[String]) -> String {
    let mut distinct: Vec<&str> = Vec::new();
    for location in locations {
        let text = location.trim();
        if text.is_empty() {
            continue;
        }
        if !distinct.contains(&text) {
            distinct.push(text);
        }
    }
    match distinct.len() {
        0 => "—".to_string(),
        1..=3 => distinct.join(" / "),
        _ => format!("多库位（{} 个，见明细）", distinct.len()),
    }
}

// ==================== 收货入库单 ====================

/// 收货入库单的渲染上下文（真库取数 + 批量补名字）。
///
/// # 异常情况
///
/// - 404 [`crate::shared::error_code::RESOURCE_NOT_FOUND`]：单据不存在 / 已软删；
/// - 500：数据库异常。
pub async fn stock_in_context(id: Uuid) -> std::result::Result<StockContext, ApiError> {
    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| doc_pdf_model::not_found("收货入库单"))?;
    let lines = stockin_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单明细"))?;

    let material_ids: Vec<Uuid> = lines.iter().map(|line| line.material_id).collect();
    let materials = order_pdf_model::materials_of(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单物料"))?;
    let unit_ids: Vec<Uuid> = lines.iter().map(|line| line.unit_id).collect();
    let units = order_pdf_model::unit_names_of(&unit_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单单位"))?;
    let location_ids: Vec<Uuid> = lines.iter().filter_map(|line| line.location_id).collect();
    let locations = doc_pdf_model::location_labels_of(&location_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单库位"))?;
    let serials = doc_pdf_model::serials_by_material(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单序列号"))?;

    let warehouse = doc_pdf_model::warehouse_names_of(&[row.warehouse_id])
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单仓库"))?
        .get(&row.warehouse_id)
        .cloned()
        .unwrap_or_else(|| "（仓库已删除）".to_string());
    let supplier = match row.supplier_id {
        Some(supplier_id) => doc_pdf_model::supplier_names_of(&[supplier_id])
            .await
            .map_err(|error| utils::db_error(error, "查询收货入库单供应商"))?
            .get(&supplier_id)
            .cloned()
            .unwrap_or_else(|| "（供应商已删除）".to_string()),
        // 无源入库允许没有供应商（`ck_biz_si_supplier` 只对 PURCHASE 强制）
        None => "—".to_string(),
    };
    let source_no = match row.source_order_id {
        Some(order_id) => stockin_model::order_no_of(order_id)
            .await
            .map_err(|error| utils::db_error(error, "查询收货入库单来源采购单"))?
            .unwrap_or_else(|| "（来源单据已删除）".to_string()),
        None => "—".to_string(),
    };
    let people: Vec<Uuid> = row.create_by.into_iter().chain(row.posted_by).collect();
    let users = order_pdf_model::user_names_of(&people)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单经办人"))?;
    let operator = row
        .create_by
        .and_then(|user_id| users.get(&user_id).cloned())
        .unwrap_or_else(|| "—".to_string());

    let sources: Vec<StockItemSource> = lines
        .iter()
        .map(|line| StockItemSource {
            material_id: line.material_id,
            unit_id: line.unit_id,
            qty: line.qty,
            price: line.price,
            amount: line.amount,
            lot_no: line.lot_no.clone(),
            location_id: line.location_id,
        })
        .collect();
    let items = build_stock_items(&sources, &materials, &units, &locations);
    let serial_groups = build_serial_groups(&sources, &materials, &serials);
    let location_summary = location_summary(&items.iter().map(|item| item.location.clone()).collect::<Vec<_>>());
    let total_qty: Decimal = lines.iter().map(|line| line.qty).sum();

    Ok(StockContext::new(StockContextArgs {
        kind: StockDocKind::StockIn,
        doc_no: row.in_no.clone(),
        meta: vec![
            OrderMetaField::new("单号", row.in_no.clone()),
            OrderMetaField::new("日期", row.in_date.format("%Y-%m-%d").to_string()),
            OrderMetaField::new("状态", stockin_model::status_label(&row.status)),
            OrderMetaField::new("供应商", supplier),
            OrderMetaField::new("仓库", warehouse),
            OrderMetaField::new("库位", location_summary),
            OrderMetaField::new("来源单据号", source_no),
            OrderMetaField::new("经办人", operator),
            OrderMetaField::new("入库类型", in_type_label(&row.in_type)),
        ],
        currency_code: row.currency_code.clone(),
        items,
        serial_groups,
        total_qty,
        total_amount: row.total_amount,
        total_tax: row.total_tax,
        total_with_tax: row.total_with_tax,
        reason: String::new(),
        remark: row.remark.clone().unwrap_or_default(),
        printed_at: doc_pdf_model::printed_at_now(),
    }))
}

// ==================== 出库单 ====================

/// 出库单的渲染上下文（真库取数 + 批量补名字）。
///
/// # 异常情况
///
/// - 404 [`crate::shared::error_code::RESOURCE_NOT_FOUND`]：单据不存在 / 已软删；
/// - 500：数据库异常。
pub async fn stock_out_context(id: Uuid) -> std::result::Result<StockContext, ApiError> {
    let row = stockout_model::find(id)
        .await
        .map_err(|error| utils::db_error(error, "查询出库单"))?
        .ok_or_else(|| doc_pdf_model::not_found("出库单"))?;
    let lines = stockout_model::items_of(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询出库单明细"))?;

    let material_ids: Vec<Uuid> = lines.iter().map(|line| line.material_id).collect();
    let materials = order_pdf_model::materials_of(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询出库单物料"))?;
    let unit_ids: Vec<Uuid> = lines.iter().map(|line| line.unit_id).collect();
    let units = order_pdf_model::unit_names_of(&unit_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询出库单单位"))?;
    let location_ids: Vec<Uuid> = lines.iter().filter_map(|line| line.location_id).collect();
    let locations = doc_pdf_model::location_labels_of(&location_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询出库单库位"))?;

    let warehouse = doc_pdf_model::warehouse_names_of(&[row.warehouse_id])
        .await
        .map_err(|error| utils::db_error(error, "查询出库单仓库"))?
        .get(&row.warehouse_id)
        .cloned()
        .unwrap_or_else(|| "（仓库已删除）".to_string());

    // 来源单据号 + 客户：只有销售发货生成的出库单才有（手工领用 / 报废 / 盘亏没有来源）
    let (source_no, customer) = match row.source_type.as_deref() {
        Some(SOURCE_TYPE_SALES) => match row.source_id {
            Some(source_id) => match doc_pdf_model::shipment_ref(source_id)
                .await
                .map_err(|error| utils::db_error(error, "查询出库单来源发货单"))?
            {
                Some(shipment) => {
                    let name = doc_pdf_model::customer_names_of(&[shipment.customer_id])
                        .await
                        .map_err(|error| utils::db_error(error, "查询出库单客户"))?
                        .get(&shipment.customer_id)
                        .cloned()
                        .unwrap_or_else(|| "（客户已删除）".to_string());
                    (shipment.ship_no, name)
                }
                None => ("（来源单据已删除）".to_string(), "—".to_string()),
            },
            None => ("—".to_string(), "—".to_string()),
        },
        // 手工建单：如实打「—」，绝不编一个来源单号 / 客户
        _ => ("—".to_string(), "—".to_string()),
    };

    let people: Vec<Uuid> = row
        .create_by
        .into_iter()
        .chain(row.posted_by)
        .chain(row.cancelled_by)
        .collect();
    let users = order_pdf_model::user_names_of(&people)
        .await
        .map_err(|error| utils::db_error(error, "查询出库单经办人"))?;
    let operator = row
        .create_by
        .and_then(|user_id| users.get(&user_id).cloned())
        .unwrap_or_else(|| "—".to_string());

    let sources: Vec<StockItemSource> = lines
        .iter()
        .map(|line| StockItemSource {
            material_id: line.material_id,
            unit_id: line.unit_id,
            qty: line.qty,
            price: line.unit_cost,
            amount: line.amount,
            lot_no: line.lot_no.clone(),
            location_id: line.location_id,
        })
        .collect();
    let items = build_stock_items(&sources, &materials, &units, &locations);
    let location_summary = location_summary(&items.iter().map(|item| item.location.clone()).collect::<Vec<_>>());
    let total_qty: Decimal = lines.iter().map(|line| line.qty).sum();

    Ok(StockContext::new(StockContextArgs {
        kind: StockDocKind::StockOut,
        doc_no: row.out_no.clone(),
        meta: vec![
            OrderMetaField::new("单号", row.out_no.clone()),
            OrderMetaField::new("日期", row.out_date.format("%Y-%m-%d").to_string()),
            OrderMetaField::new("状态", crate::business::erp::stockout_base::status_label(&row.status)),
            OrderMetaField::new("客户", customer),
            OrderMetaField::new("仓库", warehouse),
            OrderMetaField::new("库位", location_summary),
            OrderMetaField::new("来源单据号", source_no),
            OrderMetaField::new("经办人", operator),
            OrderMetaField::new("出库类型", out_type_label(&row.out_type)),
        ],
        currency_code: "CNY".to_string(),
        items,
        serial_groups: Vec::new(),
        total_qty,
        total_amount: row.total_amount,
        total_tax: Decimal::ZERO,
        total_with_tax: Decimal::ZERO,
        reason: row.reason.clone().unwrap_or_default(),
        remark: row.remark.clone().unwrap_or_default(),
        printed_at: doc_pdf_model::printed_at_now(),
    }))
}

// ==================== 导出（渲染 + 文件名） ====================

/// 一次导出结果（HTTP 层据此组响应头与审计行）。
#[derive(Debug, Clone)]
pub struct RenderedStock {
    /// PDF 字节
    pub bytes: Vec<u8>,
    /// 下载文件名
    pub filename: String,
    /// 单号
    pub doc_no: String,
    /// 明细行数
    pub row_count: usize,
    /// 金额合计（元；入库 = 价税合计，出库 = 成本结转合计）
    pub total_amount: Decimal,
    /// 渲染耗时（毫秒）
    pub duration_ms: u64,
}

/// 渲染一张出入库单的 PDF（**不含**审计与 HTTP 头，便于单测复用）。
///
/// # 异常情况
///
/// - 422 [`crate::shared::error_code::PDF_TEMPLATE_INVALID`]：模板渲染失败；
/// - 503 [`crate::shared::error_code::PDF_RENDER_UNAVAILABLE`]：Chromium 不可用；
/// - 504 [`crate::shared::error_code::PDF_RENDER_TIMEOUT`]：渲染超时。
pub async fn render_stock_pdf(
    kind: StockDocKind,
    context: &StockContext,
) -> std::result::Result<RenderedStock, ApiError> {
    let html = template::render_stock(context).map_err(|error| error.into_api())?;
    let filename = format!(
        "{}-{}.pdf",
        context.doc_label,
        doc_pdf_model::sanitize_filename(&context.doc_no)
    );
    let opts = template::stock_pdf_opts(kind, &context.doc_no, Some(filename.clone()));
    let started = Instant::now();
    let bytes = engine::render_pdf(&html, &opts)
        .await
        .map_err(|error| error.into_api())?;
    let duration_ms = started.elapsed().as_millis() as u64;
    Ok(RenderedStock {
        bytes,
        filename,
        doc_no: context.doc_no.clone(),
        row_count: context.items.len(),
        total_amount: context_total(context),
        duration_ms,
    })
}

/// 从上下文回读合计金额（审计用；展示串去千分位后是精确的两位小数）。
fn context_total(context: &StockContext) -> Decimal {
    let text = if context.show_tax {
        &context.total_with_tax
    } else {
        &context.total_amount
    };
    let digits: String = text
        .chars()
        .filter(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == '-')
        .collect();
    digits.parse::<Decimal>().unwrap_or(Decimal::ZERO)
}

/// 导出入库单 PDF（取数 + 渲染，不含 HTTP 头 / 审计）。
///
/// # 异常情况
///
/// - 400 [`crate::shared::error_code::ERP_FILTER_INVALID`]：id 非法；
/// - 404：单据不存在；
/// - 422 / 503 / 504：同 [`render_stock_pdf`]。
pub async fn export_stock_in(id_raw: &str) -> std::result::Result<RenderedStock, ApiError> {
    let id = doc_pdf_model::parse_doc_id(id_raw)?;
    let context = stock_in_context(id).await?;
    render_stock_pdf(StockDocKind::StockIn, &context).await
}

/// 导出出库单 PDF。
///
/// # 异常情况
///
/// 同 [`export_stock_in`]。
pub async fn export_stock_out(id_raw: &str) -> std::result::Result<RenderedStock, ApiError> {
    let id = doc_pdf_model::parse_doc_id(id_raw)?;
    let context = stock_out_context(id).await?;
    render_stock_pdf(StockDocKind::StockOut, &context).await
}

/// 收货入库单 PDF（`GET /api/business/erp/stock-in/{id}/pdf`）。
///
/// # 异常情况
///
/// - 401：未登录；
/// - 403 [`crate::shared::error_code::PERMISSION_DENIED`]：缺 `business:erp:stock-in:list`；
/// - 400 / 404 / 422 / 503 / 504：见 [`export_stock_in`]。
#[nik::permission(
    path = "/business/erp/stock-in/{id}/pdf",
    method = "GET",
    permission = "business:erp:stock-in:list"
)]
#[nik::get("/business/erp/stock-in/{id}/pdf")]
pub async fn stock_in_pdf(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    export_http(StockDocKind::StockIn, &headers, &connect_info, &claims, &id).await
}

/// 出库单 PDF（`GET /api/business/erp/stock-out/{id}/pdf`）。
///
/// # 异常情况
///
/// - 401：未登录；
/// - 403 [`crate::shared::error_code::PERMISSION_DENIED`]：缺 `business:erp:stock-out:list`；
/// - 400 / 404 / 422 / 503 / 504：见 [`export_stock_out`]。
#[nik::permission(
    path = "/business/erp/stock-out/{id}/pdf",
    method = "GET",
    permission = "business:erp:stock-out:list"
)]
#[nik::get("/business/erp/stock-out/{id}/pdf")]
pub async fn stock_out_pdf(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    export_http(StockDocKind::StockOut, &headers, &connect_info, &claims, &id).await
}

/// 两个入口共用的 HTTP 编排：导出 → 组响应 → 补审计。
async fn export_http(
    kind: StockDocKind,
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    id_raw: &str,
) -> Response {
    let started = Instant::now();
    let rendered = match kind {
        StockDocKind::StockIn => export_stock_in(id_raw).await,
        StockDocKind::StockOut => export_stock_out(id_raw).await,
    };
    match rendered {
        Ok(rendered) => {
            let response = match pdf_handler::pdf_response(&rendered.bytes, Some(&rendered.filename)) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(
                        kind = kind.label(),
                        doc_no = %rendered.doc_no,
                        bytes = rendered.bytes.len(),
                        "出入库单 PDF 响应组装失败"
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
    kind: StockDocKind,
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    id_raw: &str,
    rendered: &RenderedStock,
    started: Instant,
) {
    let Ok(doc_id) = doc_pdf_model::parse_doc_id(id_raw) else {
        return;
    };
    let path_segment = match kind {
        StockDocKind::StockIn => "stock-in",
        StockDocKind::StockOut => "stock-out",
    };
    ExportLogOwned {
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
        request_path: format!("/api/business/erp/{path_segment}/{doc_id}/pdf"),
        client_ip: utils::client_ip(headers, Some(connect_info)),
        user_agent: utils::user_agent(headers),
        request_id: Uuid::now_v7(),
    }
    .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::pdf::template::sample_stock;

    fn material(code: &str, name: &str, spec: &str) -> MaterialInfo {
        MaterialInfo {
            code: code.to_string(),
            name: name.to_string(),
            spec: spec.to_string(),
            unit_id: Uuid::now_v7(),
        }
    }

    /// 序号必须从 1 连续；批次号 / 库位兜底口径逐条钉死。
    #[test]
    fn items_keep_seq_and_lot_and_location() {
        let material_id = Uuid::now_v7();
        let unit_id = Uuid::now_v7();
        let location_id = Uuid::now_v7();
        let sources = vec![
            StockItemSource {
                material_id,
                unit_id,
                qty: Decimal::new(15000, 4), // 1.5
                price: Decimal::new(123456, 4),
                amount: Decimal::new(185184, 4),
                lot_no: Some("LOT-20261108-01".to_string()),
                location_id: Some(location_id),
            },
            StockItemSource {
                material_id,
                unit_id,
                qty: Decimal::ONE,
                price: Decimal::ONE,
                amount: Decimal::ONE,
                lot_no: None,
                location_id: None,
            },
        ];
        let mut materials = HashMap::new();
        materials.insert(material_id, material("MAT-001", "球阀", "DN50"));
        let mut units = HashMap::new();
        units.insert(unit_id, "台".to_string());
        let mut locations = HashMap::new();
        locations.insert(location_id, "A-01-03".to_string());

        let items = build_stock_items(&sources, &materials, &units, &locations);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].seq, 1);
        assert_eq!(items[1].seq, 2);
        assert_eq!(items[0].material_code, "MAT-001");
        assert_eq!(items[0].qty, "1.5", "数量不许截断小数");
        assert_eq!(items[0].price, "12.3456");
        assert_eq!(items[0].amount, "18.52");
        assert_eq!(items[0].lot_no, "LOT-20261108-01", "批次号必须打印");
        assert_eq!(items[0].location, "A-01-03");
        // 无批次 → 「—」（不是空串，打印出来不能是一格空白）
        assert_eq!(items[1].lot_no, "—");
        // 未分配库位桶 → 「未分配」（真实语义）
        assert_eq!(items[1].location, "未分配");
    }

    /// 物料被硬删时不 500：显示「（物料已删除）」；库位账行查不到 → 「已删除的库位账行」。
    #[test]
    fn missing_master_data_degrades_gracefully() {
        let sources = vec![StockItemSource {
            material_id: Uuid::now_v7(),
            unit_id: Uuid::now_v7(),
            qty: Decimal::ONE,
            price: Decimal::ONE,
            amount: Decimal::ONE,
            lot_no: Some("  ".to_string()),
            location_id: Some(Uuid::now_v7()),
        }];
        let items = build_stock_items(&sources, &HashMap::new(), &HashMap::new(), &HashMap::new());
        assert_eq!(items[0].material_code, "（物料已删除）");
        assert_eq!(items[0].unit, "—");
        assert_eq!(items[0].lot_no, "—", "空白批次号也走兜底");
        assert_eq!(items[0].location, "已删除的库位账行");
    }

    /// 序列号清单按明细出现顺序分组；明细里没有的物料不打印。
    #[test]
    fn serial_groups_follow_item_order() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let c = Uuid::now_v7();
        let sources: Vec<StockItemSource> = [b, a, b]
            .iter()
            .map(|material_id| StockItemSource {
                material_id: *material_id,
                unit_id: Uuid::now_v7(),
                qty: Decimal::ONE,
                price: Decimal::ONE,
                amount: Decimal::ONE,
                lot_no: None,
                location_id: None,
            })
            .collect();
        let mut materials = HashMap::new();
        materials.insert(b, material("MAT-B", "法兰", "DN80"));
        materials.insert(a, material("MAT-A", "球阀", "DN50"));
        materials.insert(c, material("MAT-C", "弯头", "DN25"));
        let mut serials: HashMap<Uuid, Vec<String>> = HashMap::new();
        serials.insert(a, vec!["SN-2".to_string(), "SN-1".to_string()]);
        serials.insert(b, vec!["SN-3".to_string()]);
        // c 有序列号但明细里没有 → 不打印
        serials.insert(c, vec!["SN-9".to_string()]);

        let groups = build_serial_groups(&sources, &materials, &serials);
        assert_eq!(groups.len(), 2, "明细里出现的物料才打印");
        assert_eq!(groups[0].material_code, "MAT-B", "顺序跟明细首次出现一致");
        assert_eq!(groups[0].numbers, "SN-3");
        assert_eq!(groups[1].material_code, "MAT-A");
        assert_eq!(groups[1].numbers, "SN-2、SN-1", "同物料多个序列号顿号分隔");
        assert_eq!(groups[1].seq, 2);
    }

    /// 库位汇总：0 / 1..3 / >3 三档。
    #[test]
    fn location_summary_buckets() {
        assert_eq!(location_summary(&[]), "—");
        assert_eq!(location_summary(&["未分配".to_string()]), "未分配");
        assert_eq!(
            location_summary(&["A-01".to_string(), "A-01".to_string(), "B-02".to_string()]),
            "A-01 / B-02",
            "重复库位只列一次"
        );
        assert_eq!(
            location_summary(&["A".to_string(), "B".to_string(), "C".to_string(), "D".to_string()]),
            "多库位（4 个，见明细）"
        );
    }

    /// 类型 / 状态标签与迁移里的值域逐字对应。
    #[test]
    fn type_labels_match_domains() {
        for (raw, label) in [
            ("PURCHASE", "采购入库"),
            ("RETURN", "退货入库"),
            ("OTHER", "其他入库"),
            ("SOURCELESS", "无源入库"),
        ] {
            assert_eq!(in_type_label(raw), label);
        }
        for (raw, label) in [
            ("ISSUE", "领用"),
            ("SCRAP", "报废"),
            ("LOSS", "盘亏"),
            ("OTHER", "其他"),
        ] {
            assert_eq!(out_type_label(raw), label);
        }
    }

    /// 操作码 / 权限码必须一一对应（拼错 = 审计检索不到 / 接口 403）。
    #[test]
    fn operation_and_permission_codes() {
        assert_eq!(export_operation(StockDocKind::StockIn), "EXPORT_STOCK_IN_PDF");
        assert_eq!(export_operation(StockDocKind::StockOut), "EXPORT_STOCK_OUT_PDF");
        assert_eq!(permission_code(StockDocKind::StockIn), "business:erp:stock-in:list");
        assert_eq!(permission_code(StockDocKind::StockOut), "business:erp:stock-out:list");
    }

    /// 上下文金额回读（审计用）不许被千分位污染；出库单取成本结转合计。
    #[test]
    fn context_total_round_trips() {
        // 入库：取价税合计
        let inbound = sample_stock(StockDocKind::StockIn, 2);
        let expected: Decimal = inbound.total_with_tax.replace(',', "").parse().unwrap();
        assert_eq!(context_total(&inbound), expected);

        // 出库：价税合计是空串（没有税额口径），必须回落到成本结转合计
        let outbound = sample_stock(StockDocKind::StockOut, 2);
        assert_eq!(outbound.total_with_tax, "");
        let expected_out: Decimal = outbound.total_amount.replace(',', "").parse().unwrap();
        assert_eq!(context_total(&outbound), expected_out);
        assert!(expected_out > Decimal::ZERO);
    }
}

// ==================== 真库用例（ERP_DB_URL；未设置则 SKIP） ====================

/// 真库用例：入库单（**含批次号 + 序列号**）与出库单（**含来源单据号**）的取数口径。
///
/// # 跑法
///
/// ```bash
/// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/<测试库>'
/// cargo test --lib business::erp::stock_pdf -- --test-threads=1
/// ```
///
/// # 夹具卫生
///
/// 独占命名空间 `0f0400xx`，进 / 出都按这一组 id 清；不扫单号前缀、不删别人的行。
#[cfg(test)]
mod db_tests {
    use super::*;

    const MATERIAL_A: Uuid = Uuid::from_u128(0x0f04_0000_0000_4000_8000_0000_0000_0002);
    const MATERIAL_B: Uuid = Uuid::from_u128(0x0f04_0000_0000_4000_8000_0000_0000_0003);
    const OPERATOR: Uuid = Uuid::from_u128(0x0f04_0000_0000_4000_8000_0000_0007);
    const LOCATION_IL: Uuid = Uuid::from_u128(0x0f04_0000_0000_4000_8000_0000_0000_0008);
    const STOCK_IN: Uuid = Uuid::from_u128(0x0f04_0000_0000_4000_8000_0000_0000_0010);
    const STOCK_OUT: Uuid = Uuid::from_u128(0x0f04_0000_0000_4000_8000_0000_0000_0020);

    const IN_NO: &str = "PDF03-IN-000001";
    const OUT_NO: &str = "PDF03-OUT-000001";
    const SHIP_NO: &str = "PDF03-SH-000001";

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

    async fn exec(sql: &str, params: Vec<Value>, label: &str) {
        db().execute_with_params(sql, params)
            .await
            .unwrap_or_else(|error| panic!("{label}：{error}\nSQL: {sql}"));
    }

    async fn purge() {
        for sql in [
            "DELETE FROM biz_material_serial WHERE source_id = '0f040000-0000-4000-8000-000000000010'::uuid",
            "DELETE FROM biz_stock_in_item WHERE stock_in_id = '0f040000-0000-4000-8000-000000000010'::uuid",
            "DELETE FROM biz_stock_in WHERE id = '0f040000-0000-4000-8000-000000000010'::uuid",
            "DELETE FROM biz_stock_out_item WHERE stock_out_id = '0f040000-0000-4000-8000-000000000020'::uuid",
            "DELETE FROM biz_stock_out WHERE id = '0f040000-0000-4000-8000-000000000020'::uuid",
            "DELETE FROM biz_shipment WHERE id = '0f040000-0000-4000-8000-000000000030'::uuid",
            "DELETE FROM biz_sales_order WHERE id = '0f040000-0000-4000-8000-000000000031'::uuid",
            "DELETE FROM biz_purchase_order WHERE id = '0f040000-0000-4000-8000-000000000040'::uuid",
            "DELETE FROM operation_logs WHERE target_id IN \
                ('0f040000-0000-4000-8000-000000000010', '0f040000-0000-4000-8000-000000000020')",
            "DELETE FROM biz_location WHERE id = '0f040000-0000-4000-8000-000000000009'::uuid",
            "DELETE FROM biz_inventory_location WHERE id = '0f040000-0000-4000-8000-000000000008'::uuid",
            "DELETE FROM biz_material WHERE id IN \
                ('0f040000-0000-4000-8000-000000000002'::uuid, '0f040000-0000-4000-8000-000000000003'::uuid)",
            "DELETE FROM biz_supplier WHERE id = '0f040000-0000-4000-8000-000000000004'::uuid",
            "DELETE FROM biz_warehouse WHERE id = '0f040000-0000-4000-8000-000000000005'::uuid",
            "DELETE FROM biz_customer WHERE id = '0f040000-0000-4000-8000-000000000006'::uuid",
            "DELETE FROM sys_user WHERE id = '0f040000-0000-4000-8000-000000000007'::uuid",
            "DELETE FROM biz_unit WHERE id = '0f040000-0000-4000-8000-000000000001'::uuid",
        ] {
            exec(sql, vec![], "清理 PDF-03 出入库夹具").await;
        }
    }

    async fn seed_master() {
        for sql in [
            "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, status, sort_order)
             VALUES ('0f040000-0000-4000-8000-000000000001', 'PDF03-PCS', '台', 1, 1, 98)",
            "INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                                       safety_stock, costing_method, tax_rate, status, serial_required)
             VALUES ('0f040000-0000-4000-8000-000000000002', 'PDF03-MAT-001', '不锈钢球阀',
                     'DN50/PN16', '0f040000-0000-4000-8000-000000000001', 'FINISHED',
                     0.0000, 'MOVING_AVG', 0.1300, 1, TRUE),
                    ('0f040000-0000-4000-8000-000000000003', 'PDF03-MAT-002', '不锈钢法兰',
                     'DN80/PN16', '0f040000-0000-4000-8000-000000000001', 'RAW',
                     0.0000, 'MOVING_AVG', 0.1300, 1, FALSE)",
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
             VALUES ('0f040000-0000-4000-8000-000000000004', 'PDF03-SUP', 'PDF03 测试供应商', 1)",
            "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path,
                                        level, is_leaf, status)
             VALUES ('0f040000-0000-4000-8000-000000000005', 'WH-PDF03-1', 'PDF03 一号仓',
                     '/0f040000-0000-4000-8000-000000000005/', 1, 1, 1)",
            "INSERT INTO biz_customer (id, code, name, credit_days, credit_limit, credit_used, status)
             VALUES ('0f040000-0000-4000-8000-000000000006', 'PDF03-CUS', 'PDF03 测试客户',
                     30, 0, 0, 'ACTIVE')",
            "INSERT INTO sys_user (id, username, password, real_name, status)
             VALUES ('0f040000-0000-4000-8000-000000000007', 'pdf03.op', 'x', '出入库测试员', 1)",
            "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, status)
             VALUES ('0f040000-0000-4000-8000-000000000009', 'A-01-03', 'A 区 01 排 03 位',
                     '0f040000-0000-4000-8000-000000000005', 1)",
            "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty)
             VALUES ('0f040000-0000-4000-8000-000000000008', '0f040000-0000-4000-8000-000000000002',
                     '0f040000-0000-4000-8000-000000000005',
                     '0f040000-0000-4000-8000-000000000009', 0)",
        ] {
            exec(sql, vec![], "写入 PDF-03 出入库主数据夹具").await;
        }
    }

    /// 造一张 2 行明细的收货入库单（来源 = PDF03-PO-000001；第一行有批次号 + 库位 + 3 个序列号）。
    async fn seed_stock_in() {
        exec(
            "INSERT INTO biz_purchase_order
                (id, order_no, supplier_id, order_date, expect_date, warehouse_id, status,
                 currency_code, total_qty, total_amount, total_tax, total_with_tax, credit_days,
                 remark, create_by, supplier_name_snapshot, warehouse_name_snapshot)
             VALUES ('0f040000-0000-4000-8000-000000000040', 'PDF03-PO-000001',
                     '0f040000-0000-4000-8000-000000000004', '2026-11-01', '2026-11-08',
                     '0f040000-0000-4000-8000-000000000005', 'APPROVED', 'CNY',
                     12.5, 373.45, 48.55, 422.00, 30, 'PDF-03 入库来源夹具',
                     '0f040000-0000-4000-8000-000000000007', 'PDF03 测试供应商', 'PDF03 一号仓')",
            vec![],
            "写入采购订单夹具（入库来源）",
        )
        .await;
        exec(
            "INSERT INTO biz_stock_in
                (id, in_no, in_type, source_order_id, is_sourceless, supplier_id, warehouse_id,
                 in_date, status, currency_code, total_qty, total_amount, total_tax,
                 total_with_tax, remark, create_by)
             VALUES ('0f040000-0000-4000-8000-000000000010', ?, 'PURCHASE',
                     '0f040000-0000-4000-8000-000000000040'::uuid, FALSE,
                     '0f040000-0000-4000-8000-000000000004',
                     '0f040000-0000-4000-8000-000000000005', '2026-11-08', 'POSTED', 'CNY',
                     12.5, 373.45, 48.55, 422.00, '外包装完好，随货附材质证明书',
                     '0f040000-0000-4000-8000-000000000007')",
            vec![Value::Text(IN_NO.to_string())],
            "写入收货入库单夹具",
        )
        .await;
        let lines = [
            (
                1i32,
                MATERIAL_A,
                "10",
                "12.340000",
                "123.4",
                "16.042",
                "139.442",
                Some("LOT-20261108-A"),
                Some(LOCATION_IL),
            ),
            (2, MATERIAL_B, "2.5", "100.000000", "250", "32.5", "282.5", None, None),
        ];
        for (line_no, material, qty, price, amount, tax, with_tax, lot_no, location) in lines {
            exec(
                "INSERT INTO biz_stock_in_item
                    (id, stock_in_id, line_no, material_id, unit_id, qty, price, price_incl,
                     tax_rate, amount, tax_amount, with_tax_amount, location_id, lot_no)
                 VALUES (gen_random_uuid(), '0f040000-0000-4000-8000-000000000010'::uuid, ?,
                         ?::uuid, '0f040000-0000-4000-8000-000000000001'::uuid,
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, 13,
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::uuid, ?)",
                vec![
                    Value::from(line_no),
                    Value::from(&material),
                    Value::Text(qty.to_string()),
                    Value::Text(price.to_string()),
                    Value::Text(price.to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(tax.to_string()),
                    Value::Text(with_tax.to_string()),
                    location.as_ref().map(Value::from).unwrap_or(Value::Null),
                    lot_no.map(|lot| Value::Text(lot.to_string())).unwrap_or(Value::Null),
                ],
                "写入收货入库明细夹具",
            )
            .await;
        }
        // 一物一码：3 个序列号挂在这张收货单上（source_id = 收货单 id）
        for serial_no in ["PDF03-SN-000001", "PDF03-SN-000002", "PDF03-SN-000003"] {
            exec(
                "INSERT INTO biz_material_serial
                    (id, serial_no, material_id, warehouse_id, location_id, status, source_type, source_id)
                 VALUES (gen_random_uuid(), ?, '0f040000-0000-4000-8000-000000000002'::uuid,
                         '0f040000-0000-4000-8000-000000000005'::uuid,
                         '0f040000-0000-4000-8000-000000000008'::uuid, 'IN_STOCK', 'PURCHASE',
                         '0f040000-0000-4000-8000-000000000010'::uuid)",
                vec![Value::Text(serial_no.to_string())],
                "写入序列号夹具",
            )
            .await;
        }
    }

    /// 造一张销售发货生成的出库单（`source_type='SALES'`，来源 = 发货单）。
    async fn seed_stock_out() {
        exec(
            "INSERT INTO biz_sales_order
                (id, so_no, customer_id, order_date, delivery_date, status,
                 total_amount, total_tax, total_with_tax, remark, create_by)
             VALUES ('0f040000-0000-4000-8000-000000000031', 'PDF03-SO-000001',
                     '0f040000-0000-4000-8000-000000000006', '2026-11-08', '2026-11-20',
                     'APPROVED', 100.00, 13.00, 113.00, 'PDF-03 出库来源夹具',
                     '0f040000-0000-4000-8000-000000000007')",
            vec![],
            "写入销售订单夹具（发货单外键）",
        )
        .await;
        exec(
            "INSERT INTO biz_shipment
                (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status,
                 total_qty, stock_out_id)
             VALUES ('0f040000-0000-4000-8000-000000000030', ?, 
                     '0f040000-0000-4000-8000-000000000031'::uuid,
                     '0f040000-0000-4000-8000-000000000006'::uuid,
                     '0f040000-0000-4000-8000-000000000005'::uuid, '2026-11-08', 'POSTED', 10,
                     '0f040000-0000-4000-8000-000000000020'::uuid)",
            vec![Value::Text(SHIP_NO.to_string())],
            "写入发货单夹具",
        )
        .await;
        exec(
            "INSERT INTO biz_stock_out
                (id, out_no, out_type, warehouse_id, out_date, status, total_qty, total_amount,
                 reason, remark, source_type, source_id, create_by)
             VALUES ('0f040000-0000-4000-8000-000000000020', ?, 'OTHER',
                     '0f040000-0000-4000-8000-000000000005'::uuid, '2026-11-08', 'POSTED',
                     10, 123.40, '销售发货过账自动生成', 'PDF-03 出库备注', 'SALES',
                     '0f040000-0000-4000-8000-000000000030'::uuid,
                     '0f040000-0000-4000-8000-000000000007'::uuid)",
            vec![Value::Text(OUT_NO.to_string())],
            "写入出库单夹具",
        )
        .await;
        exec(
            "INSERT INTO biz_stock_out_item
                (id, stock_out_id, line_no, material_id, unit_id, qty, unit_cost, amount,
                 location_id, lot_no)
             VALUES (gen_random_uuid(), '0f040000-0000-4000-8000-000000000020'::uuid, 1,
                     '0f040000-0000-4000-8000-000000000002'::uuid,
                     '0f040000-0000-4000-8000-000000000001'::uuid, 10, 12.340000, 123.40,
                     '0f040000-0000-4000-8000-000000000008'::uuid, 'LOT-20261108-A')",
            vec![],
            "写入出库明细夹具",
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

    /// 验收②：入库单上下文含批次号（明细）+ 序列号清单 + 供应商 / 仓库 / 库位 / 经办人。
    #[tokio::test]
    async fn stock_in_context_has_lot_and_serials() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_stock_in().await;
        let ctx = stock_in_context(STOCK_IN).await.expect("上下文必须组装成功");

        assert_eq!(ctx.doc_no, IN_NO);
        assert_eq!(ctx.doc_title, "收 货 入 库 单");
        let meta: Vec<(String, String)> = ctx
            .meta_rows
            .iter()
            .flatten()
            .map(|field| (field.label.clone(), field.value.clone()))
            .collect();
        assert!(meta.contains(&("供应商".into(), "PDF03 测试供应商".into())));
        assert!(meta.contains(&("仓库".into(), "PDF03 一号仓".into())));
        assert!(meta.contains(&("经办人".into(), "出入库测试员".into())));
        assert!(meta.contains(&("入库类型".into(), "采购入库".into())));
        assert!(meta.contains(&("状态".into(), "已过账".into())));
        // 有源入库：来源单据号 = 采购订单号（验收②的同伴断言）
        assert!(meta.contains(&("来源单据号".into(), "PDF03-PO-000001".into())));
        // 库位汇总 = 明细库位的去重（第一行 A-01-03、第二行未分配）
        assert!(
            meta.iter()
                .any(|(label, value)| label == "库位" && value.contains("A-01-03")),
            "库位汇总必须来自明细：{meta:?}"
        );

        assert_eq!(ctx.items.len(), 2);
        assert_eq!(ctx.items[0].material_code, "PDF03-MAT-001");
        assert_eq!(ctx.items[0].lot_no, "LOT-20261108-A", "验收②：批次号必须打印");
        assert_eq!(ctx.items[0].location, "A-01-03");
        assert_eq!(ctx.items[0].qty, "10");
        assert_eq!(ctx.items[0].unit, "台");
        assert_eq!(ctx.items[1].lot_no, "—");
        assert_eq!(ctx.items[1].location, "未分配");

        // 序列号清单：3 个 SN 挂在 MAT-001 上
        assert_eq!(ctx.serial_groups.len(), 1);
        assert_eq!(ctx.serial_groups[0].material_code, "PDF03-MAT-001");
        assert_eq!(ctx.serial_groups[0].numbers.split('、').count(), 3);
        assert!(ctx.has_serials);

        // 合计（金额 / 税额 / 价税合计 与库里的权威值一致）
        assert_eq!(ctx.total_qty, "12.5");
        assert_eq!(ctx.total_amount, "373.45");
        assert_eq!(ctx.total_tax, "48.55");
        assert_eq!(ctx.total_with_tax, "422.00");
        assert_eq!(ctx.amount_cn, "肆佰贰拾贰元整");
        assert!(ctx.show_tax);

        let html = template::render_stock(&ctx).expect("模板必须渲染成功");
        assert!(html.contains("LOT-20261108-A"));
        assert!(html.contains("序列号清单（一物一码）"));
        assert!(html.contains("PDF03-SN-000001"));
        assert!(html.contains("A-01-03"));
        purge().await;
    }

    /// 验收③：出库单上下文含**来源单据号**（销售发货 → 发货单号）+ 客户 + 成本口径合计。
    #[tokio::test]
    async fn stock_out_context_has_source_doc_no() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_stock_out().await;
        let ctx = stock_out_context(STOCK_OUT).await.expect("上下文必须组装成功");

        assert_eq!(ctx.doc_no, OUT_NO);
        assert_eq!(ctx.doc_title, "出 库 单");
        let meta: Vec<(String, String)> = ctx
            .meta_rows
            .iter()
            .flatten()
            .map(|field| (field.label.clone(), field.value.clone()))
            .collect();
        assert!(meta.contains(&("来源单据号".into(), SHIP_NO.into())), "验收③：{meta:?}");
        assert!(meta.contains(&("客户".into(), "PDF03 测试客户".into())));
        assert!(meta.contains(&("出库类型".into(), "其他".into())));
        assert!(meta.contains(&("经办人".into(), "出入库测试员".into())));

        assert_eq!(ctx.items.len(), 1);
        assert_eq!(ctx.items[0].lot_no, "LOT-20261108-A");
        assert_eq!(ctx.items[0].price, "12.34", "出库单价 = 成本单价快照");
        assert_eq!(ctx.items[0].amount, "123.40");
        assert_eq!(ctx.total_amount, "123.40");
        assert_eq!(ctx.amount_cn, "壹佰贰拾叁元肆角整");
        // 出库单没有税额口径
        assert!(!ctx.show_tax);
        assert_eq!(ctx.total_with_tax, "");

        let html = template::render_stock(&ctx).expect("模板必须渲染成功");
        assert!(html.contains(SHIP_NO));
        assert!(html.contains("PDF03 测试客户"));
        assert!(!html.contains("价税合计"));
        purge().await;
    }

    /// 手工建单的出库单（`source_type` 为空）→ 来源单据号与客户都打「—」（不编值）。
    #[tokio::test]
    async fn manual_stock_out_has_no_source() {
        let Some(_guard) = setup().await else {
            return;
        };
        exec(
            "INSERT INTO biz_stock_out
                (id, out_no, out_type, warehouse_id, out_date, status, total_qty, total_amount,
                 reason, create_by)
             VALUES ('0f040000-0000-4000-8000-000000000020', 'PDF03-OUT-MANUAL', 'ISSUE',
                     '0f040000-0000-4000-8000-000000000005'::uuid, '2026-11-08', 'DRAFT',
                     3, 0, '生产领用', '0f040000-0000-4000-8000-000000000007'::uuid)",
            vec![],
            "写入手工出库单夹具",
        )
        .await;
        let ctx = stock_out_context(STOCK_OUT).await.expect("上下文必须成功");
        let meta: Vec<(String, String)> = ctx
            .meta_rows
            .iter()
            .flatten()
            .map(|field| (field.label.clone(), field.value.clone()))
            .collect();
        assert!(meta.contains(&("来源单据号".into(), "—".into())));
        assert!(meta.contains(&("客户".into(), "—".into())));
        assert!(ctx.reason.contains("生产领用"));
        purge().await;
    }

    /// 审计：出库单导出的 `operation_logs` 行（含行数 / 金额 / doc_type）。
    #[tokio::test]
    async fn export_writes_operation_log_row() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_stock_out().await;
        let request_id = Uuid::now_v7();
        doc_pdf_model::write_export_log(&doc_pdf_model::ExportLog {
            user_id: Some(OPERATOR),
            username: Some("pdf03.op"),
            operation: export_operation(StockDocKind::StockOut),
            target_type: StockDocKind::StockOut.table(),
            doc_id: STOCK_OUT,
            doc_no: OUT_NO,
            detail: serde_json::json!({"doc_type": "STOCK_OUT", "row_count": 1, "amount": "123.40"}),
            filename: "出库单-PDF03-OUT-000001.pdf",
            bytes: 70_000,
            duration_ms: 210,
            request_path: "/api/business/erp/stock-out/0f040000-0000-4000-8000-000000000020/pdf",
            client_ip: Some(std::net::IpAddr::from([127, 0, 0, 1])),
            user_agent: Some("curl/8.5.0".to_string()),
            request_id,
        })
        .await
        .expect("审计写入必须成功");

        let rows = db()
            .find_with_params(
                "SELECT operation, target_type, detail FROM operation_logs \
                  WHERE target_id = ? AND request_id = ?",
                vec![Value::Text(STOCK_OUT.to_string()), Value::Text(request_id.to_string())],
            )
            .await
            .expect("回查审计行");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get_typed::<String>("operation").unwrap(),
            "EXPORT_STOCK_OUT_PDF"
        );
        assert_eq!(rows[0].get_typed::<String>("target_type").unwrap(), "biz_stock_out");
        let detail: serde_json::Value = rows[0].get_typed("detail").expect("detail 必须是 jsonb");
        assert_eq!(detail["doc_type"], "STOCK_OUT");
        assert_eq!(detail["row_count"], 1);
        assert_eq!(detail["source"], "PDF-03");
        purge().await;
    }

    /// 软删单据必须 404（不是「打出一张已删除的单」）。
    #[tokio::test]
    async fn soft_deleted_stock_in_is_not_found() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_stock_in().await;
        exec(
            "UPDATE biz_stock_in SET deleted_at = now()
              WHERE id = '0f040000-0000-4000-8000-000000000010'::uuid",
            vec![],
            "软删收货入库单",
        )
        .await;
        let error = stock_in_context(STOCK_IN).await.expect_err("软删单必须 404");
        assert_eq!(error.status, 404);
        assert!(error.msg.contains("收货入库单"));
        purge().await;
    }

    /// id 不是合法 UUID 时导出必须 400。
    #[tokio::test]
    async fn export_rejects_bad_uuid_before_query() {
        if !ensure_db().await {
            return;
        }
        let error = export_stock_out("not-a-uuid").await.expect_err("必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, crate::shared::error_code::ERP_FILTER_INVALID);
    }
}
