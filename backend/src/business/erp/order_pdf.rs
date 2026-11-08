//! 采购订单 / 销售订单 **PDF 导出**（PDF-02）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/business/...`）
//!
//! | 方法 | 路径 | 权限码（复用单据查看权限） | 说明 |
//! |------|------|------------------------------|------|
//! | GET | `/business/erp/purchase-order/{id}/pdf` | `business:erp:purchase-order:list` | 采购订单 PDF |
//! | GET | `/business/erp/sales-order/{id}/pdf` | `business:erp:sale-order:list` | 销售订单 PDF |
//!
//! # 为什么复用 list 码而不是新种权限点
//!
//! 导出是「看」的延伸：**能看见这张单的人就能打印它**。另立一个
//! `:export` 码会造出「列表看得见、点导出 403」的哑弹，还得为每个角色补授权
//! （迁移 + 角色矩阵 + 前端按钮显隐三处同步）。同一个先例是
//! `GET /purchase-order/{id}/pending-items`（也挂 `:list`）。
//!
//! # 响应形态
//!
//! `200 application/pdf` + `Content-Disposition: attachment; filename="采购订单-PO-…pdf"`
//! （中文走 RFC 5987，见 [`crate::business::pdf::handler::content_disposition`]）；
//! 出错时走统一错误出口 `{code,msg,data}`。前端必须用 `responseType: 'blob'` 取。
//!
//! # 审计（`operation_logs`）
//!
//! 中间件的操作日志**只记写操作**（POST/PUT/PATCH/DELETE），导出是 GET ——
//! 不显式写就一点痕迹都没有。所以渲染成功后补一行
//! [`order_pdf_model::write_export_log`]（谁 / 何时 / 哪张单 / 多少行 / 多少钱 / 文件多大）。
//! 写日志失败**不**回滚这次导出（用户要的 PDF 已经生成好了）：只记 error 日志，
//! 与 `middleware::operation_log` 的「落库失败不改响应」同一口径。
//!
//! # 金额口径
//!
//! 单头 `total_*` 是**服务端算好落库的**权威值（`purchase_model::line_amounts` /
//! `sales_order_model::line_amounts` 逐行先舍入再汇总），本模块**只读不重算**；
//! 明细行的金额同理。唯一的「算」是展示层：「元」→「分」→ 千分位 / 人民币大写，
//! 全部在 [`crate::business::pdf::template`] 里，与 PDF-01 的对账单共用同一套实现
//! （同一个金额不会在两种单据上打出两种写法）。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::order_pdf_model::{self, ExportLog, MaterialInfo};
use crate::business::erp::{customer_model, purchase_model, sales_order_model};
use crate::business::pdf::template::{
    self, OrderContext, OrderContextArgs, OrderDocKind, OrderItem, OrderMetaField, format_money,
    format_quantity, yuan_to_fen,
};
use crate::business::pdf::{engine, handler as pdf_handler};
use crate::shared::error_code;
use crate::utils;

// ==================== 公共小工具 ====================

/// 路径里的 UUID：非法 → `400 + 66006`（绝不静默当作「没传」）。
///
/// 与 `purchase_handler::parse_uuid` / `sales_order_handler::parse_uuid` 同一判据，
/// 这里单独留一份是为了不让导出功能反向依赖那两个大文件（它们反过来也不引用本文件）。
fn parse_id(raw: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            "id 不能为空",
        ));
    }
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("id 不是合法的 UUID：{text}"),
        )
    })
}

/// 单据不存在（含已软删）统一 404。
fn not_found(kind: OrderDocKind) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("{}不存在或已删除", kind.label()),
    )
}

/// 打印时间（北京时间，`yyyy-MM-dd HH:mm`）。
///
/// 服务器时区不可控（容器里常常是 UTC），所以**显式**用 +08:00 ——
/// 单据上印的「打印时间」必须是给中国人看的当地时间。
fn printed_at_now() -> String {
    let offset =
        nik::prelude::chrono::FixedOffset::east_opt(8 * 3600).expect("+08:00 一定是合法偏移");
    nik::prelude::chrono::Utc::now()
        .with_timezone(&offset)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// 下载名里的单号必须与 URL / Content-Disposition 兼容（去掉路径分隔符与控制字符）。
fn sanitize_filename(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_control()
                || matches!(
                    ch,
                    '/' | '\\' | '"' | '\'' | ':' | '*' | '?' | '<' | '>' | '|'
                )
            {
                '_'
            } else {
                ch
            }
        })
        .collect()
}

/// 备注拼装：单据本身没有专门的「交期 / 合同号」栏，塞进备注行（用户原备注在最前）。
fn compose_remark(base: Option<&str>, extras: &[(&str, String)]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (label, value) in extras {
        if !value.trim().is_empty() && value.trim() != "—" {
            parts.push(format!("{label}：{}", value.trim()));
        }
    }
    if let Some(text) = base.map(str::trim).filter(|text| !text.is_empty()) {
        parts.push(text.to_string());
    }
    parts.join("；")
}

/// 明细行 → 模板行（纯函数，单测直接调）。
///
/// # 参数
///
/// - `sources`：明细的**原始**数据（已在 SQL 里按行号升序）；
/// - `materials`：物料主档批量映射；
/// - `units`：单位名称批量映射。
///
/// 单位取值顺序：明细行自带的 `unit_id`（采购单有）→ 物料默认单位（销售单没有
/// 明细单位）→ `—`（主档已删时的兜底，宁可显示横线也不编一个单位）。
///
/// # 序号口径
///
/// `#` 列是**打印序号**（1..n，跨页连续），不是库里的 `line_no`：明细行号可能因为
/// 改单留洞（1,2,4），打出来「3 号」缺席会让人以为漏打了一行。
pub fn build_items(
    sources: &[ItemSource],
    materials: &HashMap<Uuid, MaterialInfo>,
    units: &HashMap<Uuid, String>,
) -> Vec<OrderItem> {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let material = materials.get(&source.material_id);
            let unit_id = source.unit_id.or(material.map(|info| info.unit_id));
            let unit = unit_id
                .and_then(|id| units.get(&id).cloned())
                .unwrap_or_else(|| "—".to_string());
            OrderItem {
                seq: index as u32 + 1,
                material_code: material
                    .map(|info| info.code.clone())
                    .unwrap_or_else(|| "（物料已删除）".to_string()),
                material_name: material
                    .map(|info| info.name.clone())
                    .unwrap_or_else(|| "（物料已删除）".to_string()),
                spec: material.map(|info| info.spec.clone()).unwrap_or_default(),
                unit,
                qty: format_quantity(source.qty),
                price: format_quantity(source.price),
                amount: format_money(yuan_to_fen(source.amount)),
                remark: source.remark.clone().unwrap_or_default(),
            }
        })
        .collect()
}

/// 明细的原始行（采购 / 销售两套表结构归一到这里，只留打印需要的列）。
#[derive(Debug, Clone, PartialEq)]
pub struct ItemSource {
    /// 物料
    pub material_id: Uuid,
    /// 明细行上的单位（采购有；销售为空 → 用物料默认单位）
    pub unit_id: Option<Uuid>,
    /// 数量
    pub qty: Decimal,
    /// 不含税单价
    pub price: Decimal,
    /// 不含税金额
    pub amount: Decimal,
    /// 备注
    pub remark: Option<String>,
}

// ==================== 采购订单 ====================

/// 采购订单的渲染上下文（真库取数 + 批量补名字）。
///
/// # 异常情况
///
/// - 404 [`error_code::RESOURCE_NOT_FOUND`]：单据不存在 / 已软删；
/// - 500：数据库异常。
pub async fn purchase_order_context(id: Uuid) -> std::result::Result<OrderContext, ApiError> {
    let row = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found(OrderDocKind::Purchase))?;
    let lines = purchase_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单明细"))?;

    // 批量补名字：物料 / 单位 / 经办人（一次往返各一条）
    let material_ids: Vec<Uuid> = lines.iter().map(|line| line.material_id).collect();
    let materials = order_pdf_model::materials_of(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单物料"))?;
    let unit_ids: Vec<Uuid> = lines
        .iter()
        .map(|line| line.unit_id)
        .chain(materials.values().map(|info| info.unit_id))
        .collect();
    let units = order_pdf_model::unit_names_of(&unit_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单单位"))?;
    let operators: Vec<Uuid> = row.create_by.into_iter().collect();
    let users = order_pdf_model::user_names_of(&operators)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单经办人"))?;

    let supplier = match row.supplier_name_snapshot.as_deref() {
        Some(name) if !name.trim().is_empty() => name.to_string(),
        // 存量单没有快照 → 关联查现值（P2-9 口径：快照优先，缺失才现查）
        _ => purchase_model::supplier_ref(row.supplier_id)
            .await
            .map_err(|error| utils::db_error(error, "查询采购订单供应商"))?
            .unwrap_or_else(|| "（供应商已删除）".to_string()),
    };
    let warehouse = match row.warehouse_name_snapshot.as_deref() {
        Some(name) if !name.trim().is_empty() => name.to_string(),
        _ => purchase_model::warehouse_ref(row.warehouse_id)
            .await
            .map_err(|error| utils::db_error(error, "查询采购订单仓库"))?
            .unwrap_or_else(|| "（仓库已删除）".to_string()),
    };
    let operator = row
        .create_by
        .and_then(|id| users.get(&id).cloned())
        .unwrap_or_else(|| "—".to_string());

    let sources: Vec<ItemSource> = lines
        .iter()
        .map(|line| ItemSource {
            material_id: line.material_id,
            unit_id: Some(line.unit_id),
            qty: line.qty,
            price: line.price,
            amount: line.amount,
            remark: line.remark.clone(),
        })
        .collect();
    let items = build_items(&sources, &materials, &units);

    let extras = vec![
        (
            "交期",
            row.expect_date
                .map(|date| date.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
        ),
        (
            "合同号",
            row.contract_no_snapshot.clone().unwrap_or_default(),
        ),
        (
            "项目",
            match row.project_id {
                Some(project_id) => purchase_model::project_ref(project_id)
                    .await
                    .map_err(|error| utils::db_error(error, "查询采购订单项目"))?
                    .unwrap_or_default(),
                None => String::new(),
            },
        ),
    ];

    Ok(OrderContext::new(OrderContextArgs {
        kind: OrderDocKind::Purchase,
        doc_no: row.order_no.clone(),
        meta: vec![
            OrderMetaField::new("单号", row.order_no.clone()),
            OrderMetaField::new("日期", row.order_date.format("%Y-%m-%d").to_string()),
            OrderMetaField::new("状态", purchase_model::status_label(&row.status)),
            OrderMetaField::new("供应商", supplier),
            OrderMetaField::new("仓库", warehouse),
            OrderMetaField::new("经办人", operator),
        ],
        currency_code: row.currency_code.clone(),
        items,
        total_qty: row.total_qty,
        total_amount: row.total_amount,
        total_tax: row.total_tax,
        total_with_tax: row.total_with_tax,
        remark: compose_remark(row.remark.as_deref(), &extras),
        printed_at: printed_at_now(),
    }))
}

// ==================== 销售订单 ====================

/// 销售订单的渲染上下文（真库取数 + 批量补名字）。
///
/// # 异常情况
///
/// - 404 [`error_code::RESOURCE_NOT_FOUND`]：单据不存在 / 已软删；
/// - 500：数据库异常。
pub async fn sales_order_context(id: Uuid) -> std::result::Result<OrderContext, ApiError> {
    let row = sales_order_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单"))?
        .ok_or_else(|| not_found(OrderDocKind::Sales))?;
    let lines = sales_order_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单明细"))?;

    let material_ids: Vec<Uuid> = lines.iter().map(|line| line.material_id).collect();
    let materials = order_pdf_model::materials_of(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单物料"))?;
    let unit_ids: Vec<Uuid> = materials.values().map(|info| info.unit_id).collect();
    let units = order_pdf_model::unit_names_of(&unit_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单单位"))?;
    let people: Vec<Uuid> = row
        .salesperson_id
        .into_iter()
        .chain(row.create_by)
        .collect();
    let users = order_pdf_model::user_names_of(&people)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单经办人"))?;

    let customer = customer_model::customer_find_by_id(row.customer_id)
        .await
        .map_err(|error| utils::db_error(error, "查询销售订单客户"))?
        .map(|customer| customer.name)
        .unwrap_or_else(|| "（客户已删除）".to_string());
    // 销售订单不指定发货仓库（仓库在发货单上）：如实打「—」，别编一个仓
    let warehouse = "—".to_string();
    let operator = row
        .salesperson_id
        .and_then(|id| users.get(&id).cloned())
        .or_else(|| row.create_by.and_then(|id| users.get(&id).cloned()))
        .unwrap_or_else(|| "—".to_string());

    let sources: Vec<ItemSource> = lines
        .iter()
        .map(|line| ItemSource {
            material_id: line.material_id,
            unit_id: None,
            qty: line.qty,
            price: line.price,
            amount: line.amount,
            remark: None,
        })
        .collect();
    let items = build_items(&sources, &materials, &units);
    // 销售单头没有 total_qty 列 → 数量合计现算（明细之和，Decimal 精确加）
    let total_qty: Decimal = lines.iter().map(|line| line.qty).sum();

    let extras = vec![
        (
            "交期",
            row.delivery_date
                .map(|date| date.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
        ),
        (
            "合同号",
            match row.contract_id {
                Some(contract_id) => {
                    crate::business::erp::contract_model::contract_find(contract_id)
                        .await
                        .map_err(|error| utils::db_error(error, "查询销售订单合同"))?
                        .map(|contract| contract.contract_no)
                        .unwrap_or_default()
                }
                None => String::new(),
            },
        ),
    ];

    Ok(OrderContext::new(OrderContextArgs {
        kind: OrderDocKind::Sales,
        doc_no: row.so_no.clone(),
        meta: vec![
            OrderMetaField::new("单号", row.so_no.clone()),
            OrderMetaField::new("日期", row.order_date.format("%Y-%m-%d").to_string()),
            OrderMetaField::new("状态", sales_order_model::status_label(&row.status)),
            OrderMetaField::new("客户", customer),
            OrderMetaField::new("仓库", warehouse),
            OrderMetaField::new("经办人", operator),
        ],
        currency_code: row.currency_code.clone(),
        items,
        total_qty,
        total_amount: row.total_amount,
        total_tax: row.total_tax,
        total_with_tax: row.total_with_tax,
        remark: compose_remark(row.remark.as_deref(), &extras),
        printed_at: printed_at_now(),
    }))
}

// ==================== 导出（渲染 + 文件名） ====================

/// 一次导出的结果（HTTP 层据此组响应头与审计行）。
#[derive(Debug, Clone)]
pub struct RenderedOrder {
    /// PDF 字节
    pub bytes: Vec<u8>,
    /// 下载文件名（`采购订单-PO-20261108-000001.pdf`）
    pub filename: String,
    /// 单号
    pub doc_no: String,
    /// 明细行数
    pub row_count: usize,
    /// 价税合计（元）
    pub total_with_tax: Decimal,
    /// 渲染耗时（毫秒）
    pub duration_ms: u64,
}

/// 渲染一张单据的 PDF（**不含**审计与 HTTP 头，便于单测 / 批量导出复用）。
///
/// # 参数
///
/// - `kind`：单据种类；
/// - `context`：已组装好的上下文。
///
/// # 异常情况
///
/// - 422 [`error_code::PDF_TEMPLATE_INVALID`]：模板渲染失败；
/// - 503 [`error_code::PDF_RENDER_UNAVAILABLE`]：Chromium（CDP）不可用；
/// - 504 [`error_code::PDF_RENDER_TIMEOUT`]：渲染超时。
pub async fn render_order_pdf(
    kind: OrderDocKind,
    context: &OrderContext,
) -> std::result::Result<RenderedOrder, ApiError> {
    let html = template::render_order(context).map_err(|error| error.into_api())?;
    let filename = format!(
        "{}-{}.pdf",
        context.doc_kind_label,
        sanitize_filename(&context.doc_no)
    );
    let opts = template::order_pdf_opts(kind, &context.doc_no, Some(filename.clone()));
    let started = Instant::now();
    let bytes = engine::render_pdf(&html, &opts)
        .await
        .map_err(|error| error.into_api())?;
    let duration_ms = started.elapsed().as_millis() as u64;
    Ok(RenderedOrder {
        bytes,
        filename,
        doc_no: context.doc_no.clone(),
        row_count: context.items.len(),
        total_with_tax: context_total(context),
        duration_ms,
    })
}

/// 导出采购订单 PDF 字节（不含 HTTP 头 / 审计）。
///
/// # 异常情况
///
/// - 400 [`error_code::ERP_FILTER_INVALID`]：id 非法；
/// - 404：单据不存在；
/// - 422 / 503 / 504：同 [`render_order_pdf`]。
pub async fn export_purchase_order(id_raw: &str) -> std::result::Result<RenderedOrder, ApiError> {
    let id = parse_id(id_raw)?;
    let context = purchase_order_context(id).await?;
    render_order_pdf(OrderDocKind::Purchase, &context).await
}

/// 导出销售订单 PDF 字节。
///
/// # 异常情况
///
/// 同 [`export_purchase_order`]。
pub async fn export_sales_order(id_raw: &str) -> std::result::Result<RenderedOrder, ApiError> {
    let id = parse_id(id_raw)?;
    let context = sales_order_context(id).await?;
    render_order_pdf(OrderDocKind::Sales, &context).await
}

/// 从上下文取价税合计（**回读展示串**会丢精度，所以由调用方把原值留在 meta 之外）。
///
/// 说明：`OrderContext` 只保留格式化后的字符串（模板不该拿 Decimal 再算一遍），
/// 审计需要的原始金额因此在这里按「分」重建 —— `format_money` 的输出是
/// `1,234.56` 形态，去掉千分位后是精确的两位小数，复原回元不会有误差。
fn context_total(context: &OrderContext) -> Decimal {
    let digits: String = context
        .total_with_tax
        .chars()
        .filter(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == '-')
        .collect();
    digits.parse::<Decimal>().unwrap_or(Decimal::ZERO)
}

// ==================== HTTP 入口 ====================

/// 采购订单 PDF（`GET /api/business/erp/purchase-order/{id}/pdf`）。
///
/// # 异常情况
///
/// - 401：未登录；
/// - 403 [`error_code::PERMISSION_DENIED`]：缺 `business:erp:purchase-order:list`；
/// - 400 / 404 / 422 / 503 / 504：见 [`export_purchase_order`]。
#[nik::permission(
    path = "/business/erp/purchase-order/{id}/pdf",
    method = "GET",
    permission = "business:erp:purchase-order:list"
)]
#[nik::get("/business/erp/purchase-order/{id}/pdf")]
pub async fn purchase_order_pdf(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    export_http(
        OrderDocKind::Purchase,
        &headers,
        &connect_info,
        &claims,
        &id,
    )
    .await
}

/// 销售订单 PDF（`GET /api/business/erp/sales-order/{id}/pdf`）。
///
/// # 异常情况
///
/// - 401：未登录；
/// - 403 [`error_code::PERMISSION_DENIED`]：缺 `business:erp:sale-order:list`；
/// - 400 / 404 / 422 / 503 / 504：见 [`export_sales_order`]。
#[nik::permission(
    path = "/business/erp/sales-order/{id}/pdf",
    method = "GET",
    permission = "business:erp:sale-order:list"
)]
#[nik::get("/business/erp/sales-order/{id}/pdf")]
pub async fn sales_order_pdf(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    export_http(OrderDocKind::Sales, &headers, &connect_info, &claims, &id).await
}

/// 两个入口共用的 HTTP 编排：导出 → 组响应 → 补审计。
async fn export_http(
    kind: OrderDocKind,
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    id_raw: &str,
) -> Response {
    let started = Instant::now();
    let rendered = match kind {
        OrderDocKind::Purchase => export_purchase_order(id_raw).await,
        OrderDocKind::Sales => export_sales_order(id_raw).await,
    };
    match rendered {
        Ok(rendered) => {
            let response = respond(kind, &rendered);
            audit(
                kind,
                headers,
                connect_info,
                claims,
                id_raw,
                &rendered,
                started,
            );
            response
        }
        Err(error) => error.into_response(),
    }
}

/// 组 200 响应（PDF 字节 + 下载头）。
fn respond(kind: OrderDocKind, rendered: &RenderedOrder) -> Response {
    match pdf_handler::pdf_response(&rendered.bytes, Some(&rendered.filename)) {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(
                kind = kind.label(),
                doc_no = %rendered.doc_no,
                bytes = rendered.bytes.len(),
                "单据 PDF 响应组装失败"
            );
            error.into_response()
        }
    }
}

/// 补一条导出审计（失败只记日志，不改变已生成的响应）。
fn audit(
    kind: OrderDocKind,
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    id_raw: &str,
    rendered: &RenderedOrder,
    started: Instant,
) {
    let Ok(doc_id) = parse_id(id_raw) else {
        return;
    };
    let path_segment = match kind {
        OrderDocKind::Purchase => "purchase-order",
        OrderDocKind::Sales => "sales-order",
    };
    let entry = ExportLogOwned {
        user_id: utils::current_user_uuid(claims),
        username: claims.username.clone(),
        kind,
        doc_id,
        doc_no: rendered.doc_no.clone(),
        row_count: rendered.row_count,
        total_with_tax: rendered.total_with_tax,
        filename: rendered.filename.clone(),
        bytes: rendered.bytes.len(),
        duration_ms: started.elapsed().as_millis() as u64,
        request_path: format!("/api/business/erp/{path_segment}/{doc_id}/pdf"),
        client_ip: utils::client_ip(headers, Some(connect_info)),
        user_agent: utils::user_agent(headers),
        request_id: Uuid::now_v7(),
    };
    // 审计写入不能拖慢响应：`tokio::spawn` 里只拿**拥有所有权**的数据
    // （`ExportLog` 借用的那版用于单测 / 同步调用，这里用 owned 版本）。
    tokio::spawn(async move {
        match entry.write().await {
            Ok(()) => tracing::info!(
                audit = order_pdf_model::operation_code(entry.kind),
                doc_no = %entry.doc_no,
                bytes = entry.bytes,
                "单据 PDF 导出已留痕"
            ),
            Err(error) => tracing::error!(
                audit = order_pdf_model::operation_code(entry.kind),
                doc_no = %entry.doc_no,
                error = %error,
                "单据 PDF 导出审计写入失败（导出本身已成功）"
            ),
        }
    });
}

/// 导出审计的**拥有所有权**版本（`tokio::spawn` 要求 `'static`）。
#[derive(Debug, Clone)]
struct ExportLogOwned {
    user_id: Option<Uuid>,
    username: Option<String>,
    kind: OrderDocKind,
    doc_id: Uuid,
    doc_no: String,
    row_count: usize,
    total_with_tax: Decimal,
    filename: String,
    bytes: usize,
    duration_ms: u64,
    request_path: String,
    client_ip: Option<std::net::IpAddr>,
    user_agent: Option<String>,
    request_id: Uuid,
}

impl ExportLogOwned {
    /// 落库（转成借用的 [`ExportLog`]）。
    async fn write(&self) -> DbResult<()> {
        order_pdf_model::write_export_log(&ExportLog {
            user_id: self.user_id,
            username: self.username.as_deref(),
            kind: self.kind,
            doc_id: self.doc_id,
            doc_no: &self.doc_no,
            row_count: self.row_count,
            total_with_tax: self.total_with_tax,
            filename: &self.filename,
            bytes: self.bytes,
            duration_ms: self.duration_ms,
            request_path: &self.request_path,
            client_ip: self.client_ip,
            user_agent: self.user_agent.clone(),
            request_id: self.request_id,
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::rust_decimal::Decimal;

    /// 序号必须从 1 连续（跨页打印时「3 号缺席」会被当成漏行）。
    #[test]
    fn seq_is_contiguous_from_one() {
        let material_id = Uuid::now_v7();
        let sources: Vec<ItemSource> = (0..5)
            .map(|_| ItemSource {
                material_id,
                unit_id: None,
                qty: Decimal::from(2),
                price: Decimal::new(1250, 2),
                amount: Decimal::new(2500, 2),
                remark: None,
            })
            .collect();
        let items = build_items(&sources, &HashMap::new(), &HashMap::new());
        let seqs: Vec<u32> = items.iter().map(|item| item.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
    }

    /// 物料被硬删（查不到主档）时不能 500：显示「（物料已删除）」，单位回落「—」。
    #[test]
    fn missing_material_degrades_gracefully() {
        let sources = vec![ItemSource {
            material_id: Uuid::now_v7(),
            unit_id: None,
            qty: Decimal::new(15000, 4),     // 1.5
            price: Decimal::new(123456, 4),  // 12.3456
            amount: Decimal::new(185184, 4), // 18.5184
            remark: Some("急件".to_string()),
        }];
        let items = build_items(&sources, &HashMap::new(), &HashMap::new());
        assert_eq!(items[0].material_code, "（物料已删除）");
        assert_eq!(items[0].material_name, "（物料已删除）");
        assert_eq!(items[0].unit, "—");
        assert_eq!(items[0].qty, "1.5", "数量不许截断小数");
        assert_eq!(items[0].price, "12.3456");
        assert_eq!(items[0].amount, "18.52", "金额两位小数（四舍五入）");
        assert_eq!(items[0].remark, "急件");
    }

    /// 单位优先取明细行上的（采购口径），没有才回落物料的默认单位（销售口径）。
    #[test]
    fn unit_prefers_line_then_material() {
        let material_id = Uuid::now_v7();
        let line_unit = Uuid::now_v7();
        let material_unit = Uuid::now_v7();
        let mut materials = HashMap::new();
        materials.insert(
            material_id,
            MaterialInfo {
                code: "MAT-001".to_string(),
                name: "球阀".to_string(),
                spec: "DN50".to_string(),
                unit_id: material_unit,
            },
        );
        let mut units = HashMap::new();
        units.insert(line_unit, "件".to_string());
        units.insert(material_unit, "米".to_string());

        let with_line = build_items(
            &[ItemSource {
                material_id,
                unit_id: Some(line_unit),
                qty: Decimal::ONE,
                price: Decimal::ONE,
                amount: Decimal::ONE,
                remark: None,
            }],
            &materials,
            &units,
        );
        assert_eq!(with_line[0].unit, "件", "有明细单位时用明细的");
        assert_eq!(with_line[0].material_code, "MAT-001");

        let without_line = build_items(
            &[ItemSource {
                material_id,
                unit_id: None,
                qty: Decimal::ONE,
                price: Decimal::ONE,
                amount: Decimal::ONE,
                remark: None,
            }],
            &materials,
            &units,
        );
        assert_eq!(
            without_line[0].unit, "米",
            "销售单没有明细单位 → 物料默认单位"
        );
    }

    /// 文件名里的危险字符必须被替换（单号来自库，但库里的值不能直接进响应头）。
    #[test]
    fn filename_is_sanitized() {
        assert_eq!(
            sanitize_filename("PO-20261108-000001"),
            "PO-20261108-000001"
        );
        assert_eq!(sanitize_filename("a/b\\c:d*e?f"), "a_b_c_d_e_f");
        assert_eq!(sanitize_filename("PO\n1"), "PO_1");
    }

    /// 备注拼装：交期 / 合同号 / 项目在前、用户备注在后；空值不占位。
    #[test]
    fn remark_composition_skips_empty() {
        let text = compose_remark(
            Some("按合同分批发货"),
            &[
                ("交期", "2026-11-20".to_string()),
                ("合同号", "".to_string()),
                ("项目", "—".to_string()),
            ],
        );
        assert_eq!(text, "交期：2026-11-20；按合同分批发货");
        assert_eq!(compose_remark(None, &[]), "");
    }

    /// id 非法必须 400（不是 404，也不是静默「没传」）。
    #[test]
    fn invalid_id_is_rejected() {
        let error = parse_id("not-a-uuid").expect_err("非法 id 必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_FILTER_INVALID);
        let empty = parse_id("   ").expect_err("空 id 必须报错");
        assert_eq!(empty.status, 400);
        assert!(parse_id(&Uuid::nil().to_string()).is_ok());
    }

    /// 打印时间必须是北京时间（容器里常是 UTC，差 8 小时）。
    #[test]
    fn printed_at_is_beijing_time() {
        let text = printed_at_now();
        assert_eq!(text.len(), 16, "yyyy-MM-dd HH:mm：{text}");
        assert_eq!(&text[4..5], "-");
        assert_eq!(&text[10..11], " ");
    }

    /// 从上下文回读的合计金额要精确（审计用，不许有千分位 / 精度污染）。
    #[test]
    fn context_total_round_trips() {
        let ctx = OrderContext::new(OrderContextArgs {
            kind: OrderDocKind::Purchase,
            doc_no: "PO-1".to_string(),
            meta: Vec::new(),
            currency_code: "CNY".to_string(),
            items: Vec::new(),
            total_qty: Decimal::ZERO,
            total_amount: Decimal::ZERO,
            total_tax: Decimal::ZERO,
            total_with_tax: Decimal::new(1234567, 2), // 12,345.67
            remark: String::new(),
            printed_at: "2026-11-08 18:30".to_string(),
        });
        assert_eq!(ctx.total_with_tax, "12,345.67");
        assert_eq!(context_total(&ctx), Decimal::new(1234567, 2));
    }

    /// 单据不存在时 404（且文案点名单据类型）。
    #[test]
    fn not_found_names_the_document() {
        let error = not_found(OrderDocKind::Sales);
        assert_eq!(error.status, 404);
        assert!(error.msg.contains("销售订单"));
    }
}

// ==================== 真库用例（ERP_DB_URL；未设置则 SKIP） ====================

/// 真库用例：**取数 → 上下文 → 审计行**三段都在生产同一条路径上。
///
/// # 为什么必须真库
///
/// 单元测试只能证明「给一段行数据能拼出模板」，证明不了：
/// ① 那几条批量 SQL（`ANY(?::text::uuid[])`）真的能查到物料 / 单位 / 经办人；
/// ② 采购单的**快照优先**与销售单的**物料默认单位**口径在真实表结构下成立；
/// ③ `operation_logs`（`detail ?::jsonb` + `ip ?::inet`）真的写进去了。
///
/// # 跑法
///
/// ```bash
/// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/<测试库>'
/// cargo test --lib business::erp::order_pdf::db_tests -- --nocapture --test-threads=1
/// ```
///
/// 缺 `ERP_DB_URL` 时打印 `SKIP` 直接返回（CI 无库时 `cargo test --lib` 仍全绿）。
///
/// # 夹具卫生
///
/// 固定 UUID 命名空间 `0f0200xx-…`，进 / 出都按这一组 id 清；不扫单号前缀、
/// 不删别人的行（多 worker 共用一个测试库）。
#[cfg(test)]
mod db_tests {
    use super::*;
    use nik::prelude::rust_decimal::Decimal;

    // ---------- 夹具 id（本卡独占命名空间 0f0200xx） ----------
    const MATERIAL_A: Uuid = Uuid::from_u128(0x0f02_0000_0000_4000_8000_0000_0000_0002);
    const MATERIAL_B: Uuid = Uuid::from_u128(0x0f02_0000_0000_4000_8000_0000_0000_0003);
    const OPERATOR: Uuid = Uuid::from_u128(0x0f02_0000_0000_4000_8000_0000_0000_0007);
    const PO: Uuid = Uuid::from_u128(0x0f02_0000_0000_4000_8000_0000_0000_0010);
    const SO: Uuid = Uuid::from_u128(0x0f02_0000_0000_4000_8000_0000_0000_0020);

    const PO_NO: &str = "PDF02-PO-000001";
    const SO_NO: &str = "PDF02-SO-000001";

    /// 真库用例串行（共用一个库 + 同一批固定 id）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 初始化全局 db（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
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
                // 每个 #[tokio::test] 是独立 runtime，db() 是进程级全局池：
                // 关掉连接的复用寿命，避免借到上一个 runtime 的死连接。
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

    /// 执行一条 SQL（失败即 panic：测试里不做容错）。
    async fn exec(sql: &str, params: Vec<Value>, label: &str) {
        db().execute_with_params(sql, params)
            .await
            .unwrap_or_else(|error| panic!("{label}：{error}\nSQL: {sql}"));
    }

    /// 本卡夹具的清场（顺序按外键：明细 → 单头 → 主数据 → 审计行）。
    async fn purge() {
        for sql in [
            "DELETE FROM biz_purchase_order_item WHERE order_id = '0f020000-0000-4000-8000-000000000010'",
            "DELETE FROM biz_purchase_order WHERE id = '0f020000-0000-4000-8000-000000000010'",
            "DELETE FROM biz_sales_order_item WHERE so_id = '0f020000-0000-4000-8000-000000000020'",
            "DELETE FROM biz_sales_order WHERE id = '0f020000-0000-4000-8000-000000000020'",
            "DELETE FROM operation_logs WHERE target_id IN \
             ('0f020000-0000-4000-8000-000000000010', '0f020000-0000-4000-8000-000000000020')",
            "DELETE FROM biz_material WHERE id IN \
             ('0f020000-0000-4000-8000-000000000002', '0f020000-0000-4000-8000-000000000003')",
            "DELETE FROM biz_supplier WHERE id = '0f020000-0000-4000-8000-000000000004'",
            "DELETE FROM biz_warehouse WHERE id = '0f020000-0000-4000-8000-000000000005'",
            "DELETE FROM biz_customer WHERE id = '0f020000-0000-4000-8000-000000000006'",
            "DELETE FROM sys_user WHERE id = '0f020000-0000-4000-8000-000000000007'",
            "DELETE FROM biz_unit WHERE id = '0f020000-0000-4000-8000-000000000001'",
        ] {
            exec(sql, vec![], "清理 PDF-02 夹具").await;
        }
    }

    /// 造主数据（单位 / 物料 / 供应商 / 仓库 / 客户 / 经办人）。
    async fn seed_master() {
        for sql in [
            "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, status, sort_order)
             VALUES ('0f020000-0000-4000-8000-000000000001', 'PDF02-PCS', '台', 1, 1, 99)",
            "INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                                       safety_stock, costing_method, tax_rate, status, serial_required)
             VALUES ('0f020000-0000-4000-8000-000000000002', 'PDF02-MAT-001', '不锈钢球阀',
                     'DN50/PN16', '0f020000-0000-4000-8000-000000000001', 'FINISHED',
                     0.0000, 'MOVING_AVG', 0.1300, 1, FALSE),
                    ('0f020000-0000-4000-8000-000000000003', 'PDF02-MAT-002', '不锈钢法兰',
                     'DN80/PN16', '0f020000-0000-4000-8000-000000000001', 'RAW',
                     0.0000, 'MOVING_AVG', 0.1300, 1, FALSE)",
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
             VALUES ('0f020000-0000-4000-8000-000000000004', 'PDF02-SUP', 'PDF02 测试供应商', 1)",
            "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path,
                                        level, is_leaf, status)
             VALUES ('0f020000-0000-4000-8000-000000000005', 'WH-PDF02-1', 'PDF02 一号仓',
                     '/0f020000-0000-4000-8000-000000000005/', 1, 1, 1)",
            "INSERT INTO biz_customer (id, code, name, credit_days, credit_limit, credit_used, status)
             VALUES ('0f020000-0000-4000-8000-000000000006', 'PDF02-CUS', 'PDF02 测试客户',
                     30, 0, 0, 'ACTIVE')",
            "INSERT INTO sys_user (id, username, password, real_name, status)
             VALUES ('0f020000-0000-4000-8000-000000000007', 'pdf02.op', 'x', '打印测试员', 1)",
        ] {
            exec(sql, vec![], "写入 PDF-02 主数据夹具").await;
        }
    }

    /// 造一张 3 行明细的采购订单。
    ///
    /// 金额刻意选了会让**末位**见真章的三个值（含 4 位小数税额），
    /// 用来证明「元 → 分」是按分四舍五入而不是截断：
    /// 税额合计 48.5485 → 打出来必须是 48.55，价税合计 421.9985 → 422.00。
    async fn seed_purchase_order() {
        exec(
            "INSERT INTO biz_purchase_order
                (id, order_no, supplier_id, order_date, expect_date, warehouse_id, status,
                 currency_code, total_qty, total_amount, total_tax, total_with_tax, credit_days,
                 remark, create_by, supplier_name_snapshot, warehouse_name_snapshot)
             VALUES ('0f020000-0000-4000-8000-000000000010', ?,
                     '0f020000-0000-4000-8000-000000000004', '2026-11-08', '2026-11-20',
                     '0f020000-0000-4000-8000-000000000005', 'APPROVED', 'CNY',
                     13.5, 373.45, 48.5485, 421.9985, 30,
                     '按合同分批发货', '0f020000-0000-4000-8000-000000000007',
                     'PDF02 测试供应商', 'PDF02 一号仓')",
            vec![Value::Text(PO_NO.to_string())],
            "写入采购订单夹具",
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
                "急件",
            ),
            (
                2,
                MATERIAL_B,
                "2.5",
                "100.000000",
                "250",
                "32.5",
                "282.5",
                "",
            ),
            (
                3, MATERIAL_A, "1", "0.050000", "0.05", "0.0065", "0.0565", "",
            ),
        ];
        for (line_no, material, qty, price, amount, tax, with_tax, remark) in lines {
            exec(
                "INSERT INTO biz_purchase_order_item
                    (id, order_id, line_no, material_id, unit_id, qty, price, price_incl,
                     tax_rate, amount, tax_amount, with_tax_amount, remark)
                 VALUES (gen_random_uuid(), '0f020000-0000-4000-8000-000000000010', ?,
                         ?::uuid, '0f020000-0000-4000-8000-000000000001',
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, 13,
                         ?::text::numeric, ?::text::numeric, ?::text::numeric, ?)",
                vec![
                    Value::from(line_no),
                    Value::from(&material),
                    Value::Text(qty.to_string()),
                    Value::Text(price.to_string()),
                    Value::Text(price.to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(tax.to_string()),
                    Value::Text(with_tax.to_string()),
                    Value::Text(remark.to_string()),
                ],
                "写入采购明细夹具",
            )
            .await;
        }
    }

    /// 造一张 50 行明细的销售订单（跨页验收②的数据面）。
    async fn seed_sales_order() {
        exec(
            "INSERT INTO biz_sales_order
                (id, so_no, customer_id, order_date, delivery_date, status,
                 total_amount, total_tax, total_with_tax, remark, create_by)
             VALUES ('0f020000-0000-4000-8000-000000000020', ?,
                     '0f020000-0000-4000-8000-000000000006', '2026-11-08', '2026-11-30',
                     'APPROVED', 500.00, 65.00, 565.00, '客户指定物流', 
                     '0f020000-0000-4000-8000-000000000007')",
            vec![Value::Text(SO_NO.to_string())],
            "写入销售订单夹具",
        )
        .await;
        for line_no in 1..=50i32 {
            let material = if line_no % 2 == 0 {
                MATERIAL_A
            } else {
                MATERIAL_B
            };
            exec(
                "INSERT INTO biz_sales_order_item
                    (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount,
                     delivered_qty)
                 VALUES (gen_random_uuid(), '0f020000-0000-4000-8000-000000000020', ?,
                         ?::uuid, 1, 10.000000, 10.00, 13, 1.30, 0)",
                vec![Value::from(line_no), Value::from(&material)],
                "写入销售明细夹具",
            )
            .await;
        }
    }

    /// 拿锁 + 连库 + 清场 + 建夹具。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        purge().await;
        seed_master().await;
        Some(guard)
    }

    /// 采购单：模板头 6 个字段、3 行明细的编码/单位/金额、合计与大写全部要对上。
    #[tokio::test]
    async fn purchase_order_context_reads_real_rows() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_purchase_order().await;
        let ctx = purchase_order_context(PO)
            .await
            .expect("上下文必须组装成功");

        assert_eq!(ctx.doc_no, PO_NO);
        assert_eq!(ctx.doc_title, "采 购 订 单");
        assert_eq!(ctx.row_count, "3");
        let meta: Vec<(String, String)> = ctx
            .meta_rows
            .iter()
            .flatten()
            .map(|field| (field.label.clone(), field.value.clone()))
            .collect();
        assert!(meta.contains(&("单号".into(), PO_NO.into())));
        assert!(meta.contains(&("日期".into(), "2026-11-08".into())));
        assert!(
            meta.contains(&("状态".into(), "已审批".into())),
            "状态必须是中文标签：{meta:?}"
        );
        assert!(meta.contains(&("供应商".into(), "PDF02 测试供应商".into())));
        assert!(meta.contains(&("仓库".into(), "PDF02 一号仓".into())));
        assert!(meta.contains(&("经办人".into(), "打印测试员".into())));

        assert_eq!(ctx.items.len(), 3);
        assert_eq!(ctx.items[0].seq, 1);
        assert_eq!(ctx.items[0].material_code, "PDF02-MAT-001");
        assert_eq!(ctx.items[0].material_name, "不锈钢球阀");
        assert_eq!(ctx.items[0].spec, "DN50/PN16");
        assert_eq!(ctx.items[0].unit, "台");
        assert_eq!(ctx.items[0].qty, "10");
        assert_eq!(ctx.items[0].price, "12.34");
        assert_eq!(ctx.items[0].amount, "123.40");
        assert_eq!(ctx.items[0].remark, "急件");
        // 小数数量不许被截断（0.5 米的单子不能打成 0 米）
        assert_eq!(ctx.items[1].qty, "2.5");
        assert_eq!(ctx.items[2].amount, "0.05");

        assert_eq!(ctx.total_qty, "13.5");
        assert_eq!(ctx.total_amount, "373.45");
        assert_eq!(ctx.total_tax, "48.55", "48.5485 必须四舍五入到 48.55");
        assert_eq!(ctx.total_with_tax, "422.00");
        assert_eq!(ctx.total_with_tax_cn, "肆佰贰拾贰元整");
        assert!(ctx.currency_cn == "人民币");
        // 备注：交期（单据头没这栏，塞进备注）+ 用户原备注
        assert!(
            ctx.remark.contains("交期：2026-11-20"),
            "实际：{}",
            ctx.remark
        );
        assert!(ctx.remark.contains("按合同分批发货"));

        // 模板渲染（不碰 chromium）：结构 + 用户数据都在
        let html = template::render_order(&ctx).expect("模板必须渲染成功");
        assert!(html.contains("PDF02-PO-000001"));
        assert!(html.contains("不锈钢球阀"));
        assert!(html.contains("肆佰贰拾贰元整"));
        purge().await;
    }

    /// 销售单 50 行：行数 / 数量合计 / 物料单位（走物料默认单位）/ 交期 都要对。
    #[tokio::test]
    async fn sales_order_context_handles_fifty_rows() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_sales_order().await;
        let ctx = sales_order_context(SO).await.expect("上下文必须组装成功");

        assert_eq!(ctx.doc_no, SO_NO);
        assert_eq!(ctx.doc_title, "销 售 订 单");
        assert_eq!(ctx.items.len(), 50, "50 行明细必须全部进上下文");
        assert_eq!(ctx.row_count, "50");
        assert_eq!(ctx.total_qty, "50");
        assert_eq!(ctx.total_amount, "500.00");
        assert_eq!(ctx.total_tax, "65.00");
        assert_eq!(ctx.total_with_tax, "565.00");
        assert_eq!(ctx.total_with_tax_cn, "伍佰陆拾伍元整");
        // 销售明细没有 unit_id → 必须回落物料默认单位（不是「—」）
        assert!(
            ctx.items.iter().all(|item| item.unit == "台"),
            "单位必须解析出来"
        );
        assert_eq!(ctx.items[49].seq, 50, "序号必须连续到 50");
        // 销售单不指定仓库：如实打「—」
        let warehouse = ctx
            .meta_rows
            .iter()
            .flatten()
            .find(|field| field.label == "仓库")
            .map(|field| field.value.clone());
        assert_eq!(warehouse.as_deref(), Some("—"));
        assert!(
            ctx.remark.contains("交期：2026-11-30"),
            "实际：{}",
            ctx.remark
        );

        // 50 行的 HTML 必须有 50 个序号单元格（跨页由浏览器切，不是截断）
        let html = template::render_order(&ctx).expect("模板必须渲染成功");
        assert_eq!(html.matches("<td class=\"c-seq\">").count(), 50);
        purge().await;
    }

    /// 审计：`operation_logs` 真的落下「谁导出了哪张单」（GET 不会被中间件记录，
    /// 这行是本模块自己写的）。
    #[tokio::test]
    async fn export_writes_operation_log_row() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_purchase_order().await;
        let request_id = Uuid::now_v7();
        order_pdf_model::write_export_log(&ExportLog {
            user_id: Some(OPERATOR),
            username: Some("pdf02.op"),
            kind: OrderDocKind::Purchase,
            doc_id: PO,
            doc_no: PO_NO,
            row_count: 3,
            total_with_tax: Decimal::new(4219985, 4),
            filename: "采购订单-PDF02-PO-000001.pdf",
            bytes: 51_200,
            duration_ms: 168,
            request_path: "/api/business/erp/purchase-order/0f020000-0000-4000-8000-000000000010/pdf",
            client_ip: Some(std::net::IpAddr::from([127, 0, 0, 1])),
            user_agent: Some("curl/8.5.0".to_string()),
            request_id,
        })
        .await
        .expect("审计写入必须成功");

        let rows = db()
            .find_with_params(
                "SELECT request_id, user_id, username, operation, target_type, target_id,
                        detail, ip::text AS ip, request_method, status_code, duration_ms
                   FROM operation_logs WHERE target_id = ? AND request_id = ?",
                vec![
                    Value::Text(PO.to_string()),
                    Value::Text(request_id.to_string()),
                ],
            )
            .await
            .expect("回查审计行");
        assert_eq!(rows.len(), 1, "必须正好一行");
        let row = &rows[0];
        assert_eq!(
            row.get_typed::<String>("operation").unwrap(),
            "EXPORT_PURCHASE_ORDER_PDF"
        );
        assert_eq!(
            row.get_typed::<String>("target_type").unwrap(),
            "biz_purchase_order"
        );
        assert_eq!(
            row.get_typed::<String>("target_id").unwrap(),
            PO.to_string()
        );
        assert_eq!(row.get_typed::<String>("username").unwrap(), "pdf02.op");
        assert_eq!(row.get_typed::<Uuid>("user_id").unwrap(), OPERATOR);
        assert_eq!(row.get_typed::<String>("request_method").unwrap(), "GET");
        assert_eq!(row.get_typed::<i32>("status_code").unwrap(), 200);
        assert_eq!(row.get_typed::<i32>("duration_ms").unwrap(), 168);
        assert!(
            row.get_typed::<String>("ip")
                .unwrap()
                .starts_with("127.0.0.1")
        );
        // detail（jsonb）里要能读到「哪张单 / 多少行 / 多少钱 / 多大」
        let detail: serde_json::Value = row.get_typed("detail").expect("detail 必须是 jsonb");
        assert_eq!(detail["doc_no"], PO_NO);
        assert_eq!(detail["doc_type"], "PURCHASE_ORDER");
        assert_eq!(detail["row_count"], 3);
        assert_eq!(detail["total_with_tax"], "422.00", "审计里存两位小数的金额");
        assert_eq!(detail["source"], "PDF-02");
        purge().await;
    }

    /// 已软删的单据必须 404（不是「打出一张已删除的单」）。
    #[tokio::test]
    async fn soft_deleted_order_is_not_found() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_purchase_order().await;
        exec(
            "UPDATE biz_purchase_order SET deleted_at = now()
              WHERE id = '0f020000-0000-4000-8000-000000000010'",
            vec![],
            "软删采购订单",
        )
        .await;
        let error = purchase_order_context(PO)
            .await
            .expect_err("软删单必须 404");
        assert_eq!(error.status, 404);
        assert!(error.msg.contains("采购订单"));
        purge().await;
    }

    /// id 不是合法 UUID 时导出必须 400（在碰数据库之前就挡掉）。
    #[tokio::test]
    async fn export_rejects_bad_uuid_before_query() {
        // 这条用例不需要库：先跑 ensure_db 只是为了让「无库环境」整体 SKIP 一致
        if !ensure_db().await {
            return;
        }
        let error = export_purchase_order("not-a-uuid")
            .await
            .expect_err("必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_FILTER_INVALID);
    }
}
