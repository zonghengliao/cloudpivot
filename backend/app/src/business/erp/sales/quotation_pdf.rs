//! 报价单 **PDF 导出**（M3-07：复用 `business/pdf` 的渲染色道）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/business/...`）
//!
//! | 方法 | 路径 | 权限码（复用单据查看权限） | 说明 |
//! |------|------|------------------------------|------|
//! | GET | `/business/erp/quotation/{id}/pdf` | `business:erp:quotation:list` | 报价单 PDF（11 列明细 + 有效期横幅） |
//!
//! # 为什么复用 `:list`，不新种 `:export`
//!
//! 导出是「看」的延伸：**能看见这张报价单的人就能打印它**。另立一个 `:export` 码会
//! 造出「详情看得见、点导出 403」的哑弹，还得为每个角色补授权（迁移 + 角色矩阵 +
//! 前端按钮显隐三处同步）。同一先例：`purchase-order/{id}/pdf` /
//! `sales-order/{id}/pdf`（PDF-02）、`contract/{id}/pdf`（PDF-03）都挂各自的 `:list`。
//!
//! # 复用链（本卡「复用 pdf 模块」的落点）
//!
//! ```text
//! quotation_model（取数，权威金额只读不重算）
//!   → 本文件 build_items / quotation_context（组装 template::QuotationContext）
//!   → pdf::template::render_quotation（tera，11 列 HTML）
//!   → pdf::engine::render_pdf（Chromium CDP）
//!   → pdf::handler::pdf_response（application/pdf + RFC 5987 中文文件名）
//!   → docpdf::doc_pdf_model::write_export_log（GET 不记中间件日志，显式补审计）
//! ```
//!
//! # 金额口径
//!
//! 单头 `total_amount` / `total_tax` / `total_with_tax` 是**服务端算好落库的**权威值
//! （`quotation_model::line_amounts` 逐行先舍入再汇总），本模块**只读不重算**；
//! 明细行的 `amount` / `tax_amount` 同理。唯一的「算」是展示层：「元」→「分」→
//! 千分位 / 人民币大写，全部走 [`crate::business::pdf::template`]（与购销订单、
//! 退货单共用同一套实现，同一笔钱不会在两种单据上打出两种写法）。
//!
//! # 有效期口径
//!
//! 过没过期是**现算**（`today > valid_until`，含当天算有效），不读状态列 ——
//! 与 `quotation_handler::QuotationRes.expired` / 列表筛选同一判据
//! （[`quotation_model::is_expired`]）。过期单照样能导出（客户手里那份纸质件还在，
//! 业务员要核对历史报价），只是单面上会印红框「已过有效期，价格仅供参考」。
//!
//! # 审计（`operation_logs`）
//!
//! 中间件的操作日志**只记写操作**（`AUDITED_METHODS` 只有 POST/PUT/PATCH/DELETE），
//! 导出是 GET —— 不显式写就一点痕迹都没有。渲染成功后补一行
//! `operation='EXPORT_QUOTATION_PDF'`（谁 / 何时 / 哪张单 / 多少行 / 多少钱 / 文件多大）。
//! 写日志失败**不**回滚这次导出（用户要的 PDF 已经生成好了）：只记 error 日志。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::docpdf::doc_pdf_model::{self, ExportLogOwned};
use crate::business::erp::customer::customer_model;
use crate::business::erp::order_pdf_model::{self, MaterialInfo};
use crate::business::erp::sales::quotation_model;
use crate::business::pdf::template::{
    self as pdf_template, OrderMetaField, QuotationContext, QuotationContextArgs, QuotationItem, format_money,
    format_quantity, format_tax_rate, yuan_to_fen,
};
use crate::business::pdf::{engine, handler as pdf_handler};
use cloudpivot_kernel::utils;

/// 审计里的操作码（`operation_logs.operation`）。
pub const EXPORT_OPERATION: &str = "EXPORT_QUOTATION_PDF";

/// 审计里的目标表名（`operation_logs.target_type`）。
pub const TARGET_TABLE: &str = "biz_quotation";

/// 路由权限码（复用报价单查看权限，**不**另开 `:export`）。
pub const PERMISSION: &str = "business:erp:quotation:list";

/// 审计 `detail.doc_type` 的取值（与 `cloudpivot_doc::DocKind::Quotation` 同口径）。
pub const DOC_TYPE: &str = "QUOTATION";

// ==================== 明细行 → 模板行 ====================

/// 明细的原始行（只留打印需要的列，金额都是库里落好的权威值）。
#[derive(Debug, Clone, PartialEq)]
pub struct ItemSource {
    /// 物料
    pub material_id: Uuid,
    /// 报价数量
    pub qty: Decimal,
    /// 不含税单价
    pub price: Decimal,
    /// 不含税金额
    pub amount: Decimal,
    /// 税率百分数（0–100）
    pub tax_rate: Decimal,
    /// 行税额
    pub tax_amount: Decimal,
    /// 行备注
    pub remark: Option<String>,
}

/// 明细行 → 模板行（纯函数，单测直接调）。
///
/// # 参数
///
/// - `sources`：明细的**原始**数据（已在 SQL 里按 `line_no` 升序）；
/// - `materials`：物料主档批量映射；
/// - `units`：单位名称批量映射。
///
/// # 序号口径
///
/// `#` 列是**打印序号**（1..n，跨页连续），不是库里的 `line_no`：改单会留洞
/// （1,2,4），打出来「3 号」缺席会让人以为漏打了一行。与 PDF-02 同口径。
///
/// # 单位口径
///
/// 报价单明细**没有**单位列（`biz_quotation_line` 只有 `material_id`），所以一律取
/// 物料主档的默认单位；主档已删 → `—`（宁可显示横线，也不编一个单位）。
pub fn build_items(
    sources: &[ItemSource],
    materials: &HashMap<Uuid, MaterialInfo>,
    units: &HashMap<Uuid, String>,
) -> Vec<QuotationItem> {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let material = materials.get(&source.material_id);
            let unit = material
                .and_then(|info| units.get(&info.unit_id).cloned())
                .unwrap_or_else(|| "—".to_string());
            QuotationItem {
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
                tax_rate: format_tax_rate(source.tax_rate),
                tax_amount: format_money(yuan_to_fen(source.tax_amount)),
                remark: source.remark.clone().unwrap_or_default(),
            }
        })
        .collect()
}

/// 备注拼装：客户地址塞进备注行（单面上没有独立的地址栏），用户原备注排最后。
///
/// 与 `order_pdf::compose_remark` 同一写法：空值 / `—` 不占位，用「；」串起来。
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

// ==================== 上下文组装（真库取数） ====================

/// 报价单的渲染上下文（真库取数 + 批量补名字）。
///
/// # 参数
///
/// - `id`：报价单主键；
/// - `today`：CST 的今天（判定「是否已过有效期」，单测可注入）。
///
/// # 异常情况
///
/// - 404 [`cloudpivot_kernel::shared::error_code::RESOURCE_NOT_FOUND`]：单不存在 / 已软删；
/// - 500：数据库异常。
pub async fn quotation_context(id: Uuid, today: NaiveDate) -> std::result::Result<QuotationContext, ApiError> {
    let row = quotation_model::find_quotation(id)
        .await
        .map_err(|error| utils::db_error(error, "查询报价单"))?
        .ok_or_else(|| doc_pdf_model::not_found("报价单"))?;
    let lines = quotation_model::lines_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询报价单明细"))?;

    // 批量补名字：物料 / 单位 / 制单人（一次往返各一条，不逐行查）
    let material_ids: Vec<Uuid> = lines.iter().map(|line| line.material_id).collect();
    let materials = order_pdf_model::materials_of(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询报价单物料"))?;
    let unit_ids: Vec<Uuid> = materials.values().map(|info| info.unit_id).collect();
    let units = order_pdf_model::unit_names_of(&unit_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询报价单单位"))?;
    let creators: Vec<Uuid> = row.create_by.into_iter().collect();
    let users = order_pdf_model::user_names_of(&creators)
        .await
        .map_err(|error| utils::db_error(error, "查询报价单制单人"))?;

    // 客户是**主数据**：join 实时取名（改名后单据显示新名才是对的，见 AGENTS 约定 13）。
    // 软删客户 → 如实印「（客户已删除）」，单号与金额照常。
    let customer = customer_model::customer_find_by_id(row.customer_id)
        .await
        .map_err(|error| utils::db_error(error, "查询报价单客户"))?;
    let customer_name = customer
        .as_ref()
        .map(|value| value.name.clone())
        .unwrap_or_else(|| "（客户已删除）".to_string());
    let customer_code = customer
        .as_ref()
        .map(|value| value.code.clone())
        .unwrap_or_else(|| "—".to_string());
    let contact_name = customer
        .as_ref()
        .and_then(|value| value.contact_name.clone())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "—".to_string());
    let contact_phone = customer
        .as_ref()
        .and_then(|value| value.contact_phone.clone())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "—".to_string());
    let customer_address = customer
        .as_ref()
        .and_then(|value| value.address.clone())
        .unwrap_or_default();

    let operator = row
        .create_by
        .and_then(|creator| users.get(&creator).cloned())
        .unwrap_or_else(|| "—".to_string());

    let sources: Vec<ItemSource> = lines
        .iter()
        .map(|line| ItemSource {
            material_id: line.material_id,
            qty: line.qty,
            price: line.price,
            amount: line.amount,
            tax_rate: line.tax_rate,
            tax_amount: line.tax_amount,
            remark: line.remark.clone(),
        })
        .collect();
    let items = build_items(&sources, &materials, &units);
    // 单头没有 total_qty 列 → 数量合计现算（明细之和，Decimal 精确加；与销售订单同口径）
    let total_qty: Decimal = lines.iter().map(|line| line.qty).sum();

    Ok(QuotationContext::new(QuotationContextArgs {
        doc_no: row.quote_no.clone(),
        meta: vec![
            OrderMetaField::new("单号", row.quote_no.clone()),
            OrderMetaField::new("报价日期", row.quote_date.format("%Y-%m-%d").to_string()),
            OrderMetaField::new("状态", quotation_model::status_label(&row.status)),
            OrderMetaField::new("客户", customer_name),
            OrderMetaField::new("客户编码", customer_code),
            OrderMetaField::new("制单人", operator),
            OrderMetaField::new("有效期至", row.valid_until.format("%Y-%m-%d").to_string()),
            OrderMetaField::new("联系人", contact_name),
            OrderMetaField::new("电话", contact_phone),
        ],
        // 报价单表头没有币种列（`biz_quotation` 只有金额），一律人民币：
        // 编一个 `USD` 会让外来币种的报价单打出错误的大写前缀。
        currency_code: "CNY".to_string(),
        items,
        total_qty,
        total_amount: row.total_amount,
        total_tax: row.total_tax,
        total_with_tax: row.total_with_tax,
        valid_until: row.valid_until,
        expired: row.expired_on(today),
        remark: compose_remark(row.remark.as_deref(), &[("客户地址", customer_address)]),
        printed_at: doc_pdf_model::printed_at_now(),
    }))
}

/// 用「现在」组装上下文（HTTP 入口用）。
///
/// # 异常情况
///
/// 同 [`quotation_context`]。
pub async fn quotation_context_now(id: Uuid) -> std::result::Result<QuotationContext, ApiError> {
    let today = quotation_model::today_cst(Utc::now());
    quotation_context(id, today).await
}

// ==================== 渲染 / 导出 ====================

/// 一次导出的结果（HTTP 层据此组响应头与审计行）。
#[derive(Debug, Clone)]
pub struct RenderedQuotation {
    /// PDF 字节
    pub bytes: Vec<u8>,
    /// 下载文件名（`报价单-QT-20261117-000001.pdf`）
    pub filename: String,
    /// 单号
    pub doc_no: String,
    /// 明细行数
    pub row_count: usize,
    /// 价税合计（元，审计口径）
    pub total_with_tax: Decimal,
    /// 渲染耗时（毫秒）
    pub duration_ms: u64,
}

/// 渲染一张报价单的 PDF（**不含**审计与 HTTP 头，便于单测 / 批量导出复用）。
///
/// # 异常情况
///
/// - 422 [`cloudpivot_kernel::shared::error_code::PDF_TEMPLATE_INVALID`]：模板渲染失败；
/// - 503 [`cloudpivot_kernel::shared::error_code::PDF_RENDER_UNAVAILABLE`]：Chromium（CDP）不可用；
/// - 504 [`cloudpivot_kernel::shared::error_code::PDF_RENDER_TIMEOUT`]：渲染超时。
pub async fn render_quotation_pdf(context: &QuotationContext) -> std::result::Result<RenderedQuotation, ApiError> {
    let html = pdf_template::render_quotation(context).map_err(|error| error.into_api())?;
    let filename = format!(
        "{}-{}.pdf",
        context.doc_label,
        doc_pdf_model::sanitize_filename(&context.doc_no)
    );
    let opts = pdf_template::quotation_pdf_opts(&context.doc_no, Some(filename.clone()));
    let started = Instant::now();
    let bytes = engine::render_pdf(&html, &opts)
        .await
        .map_err(|error| error.into_api())?;
    let duration_ms = started.elapsed().as_millis() as u64;
    Ok(RenderedQuotation {
        bytes,
        filename,
        doc_no: context.doc_no.clone(),
        row_count: context.items.len(),
        // 展示串是 `1,234.56` 形态：去掉千分位后是精确的两位小数，复原回元不会有误差
        total_with_tax: context
            .total_with_tax
            .chars()
            .filter(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == '-')
            .collect::<String>()
            .parse::<Decimal>()
            .unwrap_or(Decimal::ZERO),
        duration_ms,
    })
}

/// 导出报价单 PDF（取数 + 渲染，不含 HTTP 头 / 审计）。
///
/// # 异常情况
///
/// - 400 [`cloudpivot_kernel::shared::error_code::ERP_FILTER_INVALID`]：id 非法；
/// - 404：报价单不存在 / 已软删；
/// - 422 / 503 / 504：同 [`render_quotation_pdf`]。
pub async fn export_quotation(id_raw: &str) -> std::result::Result<RenderedQuotation, ApiError> {
    let id = doc_pdf_model::parse_doc_id(id_raw)?;
    let context = quotation_context_now(id).await?;
    render_quotation_pdf(&context).await
}

// ==================== HTTP 入口 ====================

/// 报价单 PDF（`GET /api/business/erp/quotation/{id}/pdf`）。
///
/// # 异常情况
///
/// - 401：未登录；
/// - 403 [`cloudpivot_kernel::shared::error_code::PERMISSION_DENIED`]：缺 `business:erp:quotation:list`；
/// - 400 / 404 / 422 / 503 / 504：见 [`export_quotation`]。
#[nik::permission(
    path = "/business/erp/quotation/{id}/pdf",
    method = "GET",
    permission = "business:erp:quotation:list"
)]
#[nik::get("/business/erp/quotation/{id}/pdf")]
pub async fn quotation_pdf(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    let started = Instant::now();
    match export_quotation(&id).await {
        Ok(rendered) => {
            let response = match pdf_handler::pdf_response(&rendered.bytes, Some(&rendered.filename)) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(
                        doc_no = %rendered.doc_no,
                        bytes = rendered.bytes.len(),
                        "报价单 PDF 响应组装失败"
                    );
                    return error.into_response();
                }
            };
            audit(&headers, &connect_info, &claims, &id, &rendered, started);
            response
        }
        Err(error) => error.into_response(),
    }
}

/// 补一条导出审计（失败只记日志，不改变已生成的响应）。
fn audit(
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    id_raw: &str,
    rendered: &RenderedQuotation,
    started: Instant,
) {
    let Ok(doc_id) = doc_pdf_model::parse_doc_id(id_raw) else {
        return;
    };
    ExportLogOwned {
        user_id: utils::current_user_uuid(claims),
        username: claims.username.clone(),
        operation: EXPORT_OPERATION.to_string(),
        target_type: TARGET_TABLE.to_string(),
        doc_id,
        doc_no: rendered.doc_no.clone(),
        detail: serde_json::json!({
            "doc_type": DOC_TYPE,
            "row_count": rendered.row_count,
            "total_with_tax": rendered.total_with_tax.round_dp(2).to_string(),
            "render_ms": rendered.duration_ms,
            "source": "M3-07",
        }),
        filename: rendered.filename.clone(),
        bytes: rendered.bytes.len(),
        duration_ms: started.elapsed().as_millis() as u64,
        request_path: format!("/api/business/erp/quotation/{doc_id}/pdf"),
        client_ip: utils::client_ip(headers, Some(connect_info)),
        user_agent: utils::user_agent(headers),
        request_id: Uuid::now_v7(),
    }
    .spawn();
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::pdf::template::{
        QUOTATION_EXPIRED_NOTICE, QUOTATION_LABEL, QUOTATION_SIGN_CAPS, QUOTATION_TITLE, QUOTATION_VALID_NOTICE,
        render_quotation, sample_quotation,
    };

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点可解析")
    }

    fn source(material_id: Uuid) -> ItemSource {
        ItemSource {
            material_id,
            qty: Decimal::from(2),
            price: dec("1250.00"),
            amount: dec("2500.00"),
            tax_rate: Decimal::from(13),
            tax_amount: dec("325.00"),
            remark: None,
        }
    }

    /// 序号必须从 1 连续（跨页打印时「3 号缺席」会被当成漏行）。
    #[test]
    fn seq_is_contiguous_from_one() {
        let material_id = Uuid::now_v7();
        let sources: Vec<ItemSource> = (0..5).map(|_| source(material_id)).collect();
        let items = build_items(&sources, &HashMap::new(), &HashMap::new());
        let seqs: Vec<u32> = items.iter().map(|item| item.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
    }

    /// 物料被硬删（查不到主档）时不能 500：显示「（物料已删除）」，单位回落「—」。
    #[test]
    fn missing_material_degrades_gracefully() {
        let items = build_items(&[source(Uuid::now_v7())], &HashMap::new(), &HashMap::new());
        assert_eq!(items[0].material_code, "（物料已删除）");
        assert_eq!(items[0].material_name, "（物料已删除）");
        assert_eq!(items[0].unit, "—");
        assert_eq!(items[0].qty, "2");
        assert_eq!(items[0].price, "1,250");
        assert_eq!(items[0].amount, "2,500.00");
        assert_eq!(items[0].tax_rate, "13%");
        assert_eq!(items[0].tax_amount, "325.00");
    }

    /// 单位一律取物料默认单位（报价单明细没有单位列）；税率去尾零、金额两位小数。
    #[test]
    fn unit_comes_from_material_and_amounts_are_formatted() {
        let material_id = Uuid::now_v7();
        let unit_id = Uuid::now_v7();
        let mut materials = HashMap::new();
        materials.insert(
            material_id,
            MaterialInfo {
                code: "MAT-001".to_string(),
                name: "球阀".to_string(),
                spec: "DN50".to_string(),
                unit_id,
            },
        );
        let mut units = HashMap::new();
        units.insert(unit_id, "件".to_string());
        let mut line = source(material_id);
        line.qty = dec("1.5");
        line.tax_rate = dec("6.5");
        line.remark = Some("急件".to_string());
        let items = build_items(&[line], &materials, &units);
        assert_eq!(items[0].unit, "件");
        assert_eq!(items[0].material_code, "MAT-001");
        assert_eq!(items[0].spec, "DN50");
        assert_eq!(items[0].qty, "1.5", "数量不许截断小数");
        assert_eq!(items[0].tax_rate, "6.5%");
        assert_eq!(items[0].remark, "急件");
    }

    /// 备注拼装：地址在前、原备注在后；空地址 / `—` 不占位。
    #[test]
    fn remark_composition_orders_and_skips_blanks() {
        assert_eq!(
            compose_remark(Some("含税报价"), &[("客户地址", "上海市浦东新区".to_string())]),
            "客户地址：上海市浦东新区；含税报价"
        );
        assert_eq!(
            compose_remark(Some("含税报价"), &[("客户地址", "  ".to_string())]),
            "含税报价"
        );
        assert_eq!(compose_remark(Some("  "), &[("客户地址", "—".to_string())]), "");
        assert_eq!(compose_remark(None, &[]), "");
    }

    /// 路由权限码必须是 `:list`（不是新种的 `:export`），操作码与目标表名一字不差。
    #[test]
    fn permission_and_audit_constants() {
        assert_eq!(PERMISSION, "business:erp:quotation:list");
        assert_eq!(EXPORT_OPERATION, "EXPORT_QUOTATION_PDF");
        assert_eq!(TARGET_TABLE, "biz_quotation");
        assert_eq!(DOC_TYPE, "QUOTATION");
    }

    /// 文件名 = 单据名 + 净化后的单号（中文名，前端按它存盘）。
    #[test]
    fn filename_is_built_from_doc_no() {
        let ctx = sample_quotation(1);
        let filename = format!(
            "{}-{}.pdf",
            ctx.doc_label,
            doc_pdf_model::sanitize_filename(&ctx.doc_no)
        );
        assert_eq!(filename, "报价单-QT-20261117-000001.pdf");
        assert_eq!(
            format!(
                "{}-{}.pdf",
                QUOTATION_LABEL,
                doc_pdf_model::sanitize_filename("QT/1:2*3")
            ),
            "报价单-QT_1_2_3.pdf"
        );
    }

    /// 上下文渲染出的 HTML 必须带 11 列表头、三项合计与有效期横幅（端到端模板契约）。
    #[test]
    fn rendered_html_carries_tax_columns_and_validity_banner() {
        let ctx = sample_quotation(2);
        let html = render_quotation(&ctx).expect("报价单模板必须渲染成功");
        assert!(html.contains(QUOTATION_TITLE));
        for head in ["税率", "税额", "单价", "金额"] {
            assert!(html.contains(&format!(">{head}<")), "表头缺「{head}」");
        }
        assert!(html.contains(&ctx.total_amount));
        assert!(html.contains(&ctx.total_tax));
        assert!(html.contains(&ctx.total_with_tax));
        assert!(html.contains(QUOTATION_VALID_NOTICE));
        assert!(html.contains("2026-12-17（含当天"));
        for cap in QUOTATION_SIGN_CAPS {
            assert!(html.contains(cap), "签字位缺「{cap}」");
        }
    }

    /// 过期单：横幅切到「仅供参考」并带红框类（报价单过期不是错误，但要印清楚）。
    #[test]
    fn expired_context_prints_warning_banner() {
        let mut ctx = sample_quotation(1);
        ctx.expired = true;
        ctx.validity_notice = QUOTATION_EXPIRED_NOTICE.to_string();
        let html = render_quotation(&ctx).expect("过期单也要能渲染");
        assert!(html.contains(QUOTATION_EXPIRED_NOTICE));
        assert!(html.contains("is-expired"));
        assert!(!html.contains(QUOTATION_VALID_NOTICE));
    }

    /// 渲染参数：A4 纵向、页脚带页码占位符、文件名进 `Content-Disposition`。
    #[test]
    fn pdf_opts_are_portrait_a4_with_page_footer() {
        let opts =
            pdf_template::quotation_pdf_opts("QT-20261117-000001", Some("报价单-QT-20261117-000001.pdf".to_string()));
        assert!(!opts.landscape, "11 列靠收窄列宽排下，不改横向");
        assert!(opts.display_header_footer);
        let footer = opts.footer_template.as_deref().unwrap_or_default();
        assert!(footer.contains("pageNumber"));
        assert!(footer.contains("QT-20261117-000001"));
        assert_eq!(opts.filename.as_deref(), Some("报价单-QT-20261117-000001.pdf"));
    }
}
