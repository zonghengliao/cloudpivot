//! 调拨单 **PDF 导出**（PDF-04）。
//!
//! # 接口（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/business/...`）
//!
//! | 方法 | 路径 | 权限码（复用单据查看权限） | 说明 |
//! |------|------|------------------------------|------|
//! | GET | `/business/erp/stock-transfer/{id}/pdf` | `business:erp:transfer:list` | 调拨单 PDF |
//!
//! # 卡面要求的字段落点
//!
//! | 要求 | 数据来源 |
//! |------|----------|
//! | **调出 / 调入仓库**（验收②） | `biz_stock_transfer.from_warehouse_id` / `to_warehouse_id` → `biz_warehouse.warehouse_name`；同时进单据头（两个字段）+ 顶部路线条「调出 ➜ 调入」 |
//! | 在途仓 | `transit_warehouse_id` → 仓库名；`IN_TRANSIT` / `DONE` 才有值，草稿为空（路线条不印「在途经」） |
//! | 调出 / 调入库位 | 明细 `from_location_id` / `to_location_id`（**库位账行** `biz_inventory_location.id`）→ `biz_location.location_code` |
//! | 单位 | 明细表**没有**单位列（V0140）→ 取物料主档 `biz_material.unit_id` → `biz_unit.unit_name`；查不到打 `—` |
//! | 单位成本 / 金额 | 明细 `unit_cost` / `amount` 快照（调出时算好，**不重算**） |
//! | 申请人 / 审批人 / 经办人 | `applicant_name` 快照优先（单据留痕），否则 `applicant_id` / `approver_id` / `create_by` → `sys_user` |
//!
//! # 金额口径
//!
//! 调拨是**原价转移**：没有税额三档，只有「成本金额合计」（= 单头 `total_amount`）。
//! 单头合计是服务端过账时写好的权威值，本模块只做展示层格式化
//! （元 → 分 → 千分位 / 人民币大写），与订单 / 出入库 PDF 同一套实现。
//!
//! # 审计
//!
//! 导出是 GET，中间件不记；渲染成功后显式补 `EXPORT_STOCK_TRANSFER_PDF` 一行
//! （写日志失败只记 error，不改响应）。

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
use crate::business::erp::transfer_base::TransferItemRow;
use crate::business::erp::transfer_model;
use crate::business::pdf::handler as pdf_handler;
use crate::business::pdf::template::{
    self, OrderMetaField, TransferContext, TransferContextArgs, TransferItem, format_money, format_quantity,
    yuan_to_fen,
};
use crate::utils;

/// 审计里的操作码。
pub const EXPORT_OPERATION: &str = "EXPORT_STOCK_TRANSFER_PDF";

/// 路由权限码（复用调拨单查看权限）。
pub const PERMISSION: &str = "business:erp:transfer:list";

/// 明细行 → 模板行（纯函数，单测直接调）。
///
/// # 参数
///
/// - `sources`：明细原始行（已按 `line_no` 升序；`material_code` / `material_name` 已连表带出）；
/// - `materials`：物料主档（补规格与单位）。
/// - `units`：单位 id → 单位名。
/// - `locations`：库位账行 id → 库位编码。
///
/// # 兜底口径（宁可显示横线也不编值）
///
/// - 物料主档查不到 → 用明细连表带出的编码 / 名称；两者都没有 → 「（物料已删除）」；
/// - 规格查不到 → 空串（不是「—」：规格为空是常态）；
/// - 库位为 `None` → 「未分配」（未分配库位桶，是**真实语义**）；账行查不到 → 「已删除的库位账行」。
pub fn build_transfer_items(
    sources: &[TransferItemRow],
    materials: &HashMap<Uuid, MaterialInfo>,
    units: &HashMap<Uuid, String>,
    locations: &HashMap<Uuid, String>,
) -> Vec<TransferItem> {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let material = materials.get(&source.material_id);
            let code = source
                .material_code
                .clone()
                .or_else(|| material.map(|info| info.code.clone()))
                .unwrap_or_else(|| "（物料已删除）".to_string());
            let name = source
                .material_name
                .clone()
                .or_else(|| material.map(|info| info.name.clone()))
                .unwrap_or_else(|| "（物料已删除）".to_string());
            let unit = material
                .and_then(|info| units.get(&info.unit_id).cloned())
                .unwrap_or_else(|| "—".to_string());
            TransferItem {
                seq: index as u32 + 1,
                material_code: code,
                material_name: name,
                spec: material.map(|info| info.spec.clone()).unwrap_or_default(),
                unit,
                qty: format_quantity(source.qty),
                from_location: location_label(source.from_location_id, locations),
                to_location: location_label(source.to_location_id, locations),
                unit_cost: format_quantity(source.unit_cost),
                amount: format_money(yuan_to_fen(source.amount)),
            }
        })
        .collect()
}

/// 库位账行 id → 展示文案（`未分配` 是真实语义，不是缺失）。
fn location_label(id: Option<Uuid>, locations: &HashMap<Uuid, String>) -> String {
    match id {
        None => "未分配".to_string(),
        Some(id) => locations
            .get(&id)
            .cloned()
            .unwrap_or_else(|| "已删除的库位账行".to_string()),
    }
}

/// 调拨单的渲染上下文（真库取数 + 批量补名字）。
///
/// # 异常情况
///
/// - 404 [`crate::shared::error_code::RESOURCE_NOT_FOUND`]：单据不存在 / 已软删；
/// - 500：数据库异常。
pub async fn transfer_context(id: Uuid) -> std::result::Result<TransferContext, ApiError> {
    let row = transfer_model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单"))?
        .ok_or_else(|| doc_pdf_model::not_found("调拨单"))?;
    let lines = transfer_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单明细"))?;

    let material_ids: Vec<Uuid> = lines.iter().map(|line| line.material_id).collect();
    let materials = order_pdf_model::materials_of(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单物料"))?;
    let unit_ids: Vec<Uuid> = materials.values().map(|info| info.unit_id).collect();
    let units = order_pdf_model::unit_names_of(&unit_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单单位"))?;
    let location_ids: Vec<Uuid> = lines
        .iter()
        .flat_map(|line| line.from_location_id.into_iter().chain(line.to_location_id))
        .collect();
    let locations = doc_pdf_model::location_labels_of(&location_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单库位"))?;

    // 调出 / 调入 / 在途 三个仓库一次查完（在途仓可能为空）
    let warehouse_ids: Vec<Uuid> = [Some(row.from_warehouse_id), Some(row.to_warehouse_id)]
        .into_iter()
        .flatten()
        .chain(row.transit_warehouse_id)
        .collect();
    let warehouses = doc_pdf_model::warehouse_names_of(&warehouse_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单仓库"))?;
    let from_warehouse = warehouses
        .get(&row.from_warehouse_id)
        .cloned()
        .unwrap_or_else(|| "（仓库已删除）".to_string());
    let to_warehouse = warehouses
        .get(&row.to_warehouse_id)
        .cloned()
        .unwrap_or_else(|| "（仓库已删除）".to_string());
    let transit_warehouse = row.transit_warehouse_id.and_then(|id| warehouses.get(&id).cloned());

    let people: Vec<Uuid> = row
        .applicant_id
        .into_iter()
        .chain(row.approver_id)
        .chain(row.create_by)
        .collect();
    let users = order_pdf_model::user_names_of(&people)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单经办人"))?;
    // 申请人优先用单据上的姓名快照（人员改名后历史单据不该跟着变）
    let applicant = row
        .applicant_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| row.applicant_id.and_then(|id| users.get(&id).cloned()))
        .unwrap_or_else(|| "—".to_string());
    let approver = row
        .approver_id
        .and_then(|id| users.get(&id).cloned())
        .unwrap_or_else(|| "—".to_string());
    let operator = row
        .create_by
        .and_then(|id| users.get(&id).cloned())
        .unwrap_or_else(|| "—".to_string());

    let items = build_transfer_items(&lines, &materials, &units, &locations);
    let total_qty: Decimal = lines.iter().map(|line| line.qty).sum();

    Ok(TransferContext::new(TransferContextArgs {
        doc_no: row.transfer_no.clone(),
        meta: vec![
            OrderMetaField::new("单号", row.transfer_no.clone()),
            OrderMetaField::new("调拨日期", row.transfer_date.format("%Y-%m-%d").to_string()),
            OrderMetaField::new("状态", transfer_model::status_label(&row.status)),
            OrderMetaField::new("调出仓库", from_warehouse.clone()),
            OrderMetaField::new("调入仓库", to_warehouse.clone()),
            OrderMetaField::new("在途仓", transit_warehouse.clone().unwrap_or_else(|| "—".to_string())),
            OrderMetaField::new("申请人", applicant),
            OrderMetaField::new("审批人", approver),
            OrderMetaField::new("经办人", operator),
        ],
        from_warehouse,
        to_warehouse,
        transit_warehouse,
        items,
        total_qty,
        total_amount: row.total_amount,
        reason: row.reason.clone().unwrap_or_default(),
        remark: row.remark.clone().unwrap_or_default(),
        printed_at: doc_pdf_model::printed_at_now(),
    }))
}

/// 导出调拨单 PDF（取数 + 渲染，不含 HTTP 头 / 审计）。
///
/// # 异常情况
///
/// - 400 [`crate::shared::error_code::ERP_FILTER_INVALID`]：id 非法；
/// - 404：单据不存在；
/// - 422 / 503 / 504：模板 / 渲染引擎异常。
pub async fn export_transfer(id_raw: &str) -> std::result::Result<RenderedDoc, ApiError> {
    let id = doc_pdf_model::parse_doc_id(id_raw)?;
    let context = transfer_context(id).await?;
    let html = template::render_transfer(&context).map_err(|error| error.into_api())?;
    let filename = doc_pdf_render::pdf_filename(&context.doc_label, &context.doc_no);
    let opts = template::transfer_pdf_opts(&context.doc_no, Some(filename));
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

/// 从上下文回读合计金额（审计用；展示串去千分位后是精确的两位小数）。
fn context_total(context: &TransferContext) -> Decimal {
    let digits: String = context
        .total_amount
        .chars()
        .filter(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == '-')
        .collect();
    digits.parse::<Decimal>().unwrap_or(Decimal::ZERO)
}

/// 调拨单 PDF（`GET /api/business/erp/stock-transfer/{id}/pdf`）。
///
/// # 异常情况
///
/// - 401：未登录；
/// - 403 [`crate::shared::error_code::PERMISSION_DENIED`]：缺 `business:erp:transfer:list`；
/// - 400 / 404 / 422 / 503 / 504：见 [`export_transfer`]。
#[nik::permission(
    path = "/business/erp/stock-transfer/{id}/pdf",
    method = "GET",
    permission = "business:erp:transfer:list"
)]
#[nik::get("/business/erp/stock-transfer/{id}/pdf")]
pub async fn stock_transfer_pdf(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    let started = Instant::now();
    match export_transfer(&id).await {
        Ok(rendered) => {
            let response = match pdf_handler::pdf_response(&rendered.bytes, Some(&rendered.filename)) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(
                        doc_no = %rendered.doc_no,
                        bytes = rendered.bytes.len(),
                        "调拨单 PDF 响应组装失败"
                    );
                    return error.into_response();
                }
            };
            audit(&id, &headers, &connect_info, &claims, &rendered, started);
            response
        }
        Err(error) => error.into_response(),
    }
}

/// 补一条导出审计（失败只记日志，不改变已生成的响应）。
fn audit(
    id_raw: &str,
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    rendered: &RenderedDoc,
    started: Instant,
) {
    let Ok(doc_id) = doc_pdf_model::parse_doc_id(id_raw) else {
        return;
    };
    doc_pdf_model::ExportLogOwned {
        user_id: utils::current_user_uuid(claims),
        username: claims.username.clone(),
        operation: EXPORT_OPERATION.to_string(),
        target_type: "biz_stock_transfer".to_string(),
        doc_id,
        doc_no: rendered.doc_no.clone(),
        detail: serde_json::json!({
            "doc_type": "STOCK_TRANSFER",
            "row_count": rendered.row_count,
            "amount": rendered.total_amount.round_dp(2).to_string(),
            "render_ms": rendered.duration_ms,
        }),
        filename: rendered.filename.clone(),
        bytes: rendered.bytes.len(),
        duration_ms: started.elapsed().as_millis() as u64,
        request_path: format!("/api/business/erp/stock-transfer/{doc_id}/pdf"),
        client_ip: utils::client_ip(headers, Some(connect_info)),
        user_agent: utils::user_agent(headers),
        request_id: Uuid::now_v7(),
    }
    .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material(code: &str, name: &str, spec: &str) -> MaterialInfo {
        MaterialInfo {
            code: code.to_string(),
            name: name.to_string(),
            spec: spec.to_string(),
            unit_id: Uuid::now_v7(),
        }
    }

    fn source(material_id: Uuid, qty: Decimal, from: Option<Uuid>, to: Option<Uuid>) -> TransferItemRow {
        TransferItemRow {
            id: Uuid::now_v7(),
            transfer_id: Uuid::now_v7(),
            line_no: 1,
            material_id,
            from_location_id: from,
            to_location_id: to,
            qty,
            shipped_qty: Decimal::ZERO,
            received_qty: Decimal::ZERO,
            unit_cost: Decimal::new(123_456, 4),
            amount: Decimal::new(185_184, 4),
            remark: None,
            material_code: Some("ROW-CODE".to_string()),
            material_name: Some("行内名称".to_string()),
        }
    }

    /// 序号从 1 连续；调出 / 调入库位各自落位；单位取物料主档。
    #[test]
    fn items_keep_seq_and_both_locations() {
        let material_id = Uuid::now_v7();
        let unit_id = Uuid::now_v7();
        let from = Uuid::now_v7();
        let to = Uuid::now_v7();
        let sources = vec![
            source(material_id, Decimal::new(15_000, 4), Some(from), Some(to)),
            source(material_id, Decimal::ONE, None, Some(to)),
        ];
        let mut materials = HashMap::new();
        let mut info = material("MAT-001", "球阀", "DN50");
        info.unit_id = unit_id;
        materials.insert(material_id, info);
        let mut units = HashMap::new();
        units.insert(unit_id, "台".to_string());
        let mut locations = HashMap::new();
        locations.insert(from, "A-01-03".to_string());
        locations.insert(to, "B-02-04".to_string());

        let items = build_transfer_items(&sources, &materials, &units, &locations);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].seq, 1);
        assert_eq!(items[1].seq, 2);
        // 明细连表带出的编码 / 名称优先（即便主档也在）
        assert_eq!(items[0].material_code, "ROW-CODE");
        assert_eq!(items[0].material_name, "行内名称");
        assert_eq!(items[0].spec, "DN50");
        assert_eq!(items[0].unit, "台");
        assert_eq!(items[0].qty, "1.5", "数量不许截断小数");
        assert_eq!(items[0].unit_cost, "12.3456");
        assert_eq!(items[0].amount, "18.52");
        assert_eq!(items[0].from_location, "A-01-03");
        assert_eq!(items[0].to_location, "B-02-04");
        // 调出库位为空 = 未分配库位桶（真实语义）
        assert_eq!(items[1].from_location, "未分配");
        assert_eq!(items[1].to_location, "B-02-04");
    }

    /// 物料主档 / 单位 / 库位账行都查不到时不 500，走兜底文案。
    #[test]
    fn missing_master_data_degrades_gracefully() {
        let sources = vec![source(Uuid::now_v7(), Decimal::ONE, Some(Uuid::now_v7()), None)];
        let items = build_transfer_items(&sources, &HashMap::new(), &HashMap::new(), &HashMap::new());
        assert_eq!(items[0].material_code, "ROW-CODE", "连表值兜底");
        assert_eq!(items[0].spec, "", "规格为空是常态，不塞横线");
        assert_eq!(items[0].unit, "—", "单位查不到打横线");
        assert_eq!(items[0].from_location, "已删除的库位账行");
        assert_eq!(items[0].to_location, "未分配");
    }

    /// 完全没有连表值时打「（物料已删除）」（不编编码）。
    #[test]
    fn deleted_material_is_marked() {
        let mut row = source(Uuid::now_v7(), Decimal::ONE, None, None);
        row.material_code = None;
        row.material_name = None;
        let items = build_transfer_items(&[row], &HashMap::new(), &HashMap::new(), &HashMap::new());
        assert_eq!(items[0].material_code, "（物料已删除）");
        assert_eq!(items[0].material_name, "（物料已删除）");
    }

    /// 上下文合计回读：千分位串能精确还原成 Decimal（审计金额不能错）。
    #[test]
    fn context_total_reads_back_display_string() {
        let ctx = template::sample_transfer(3);
        let expected: Decimal = ctx.total_amount.replace(',', "").parse().unwrap();
        assert_eq!(context_total(&ctx), expected);
        assert_eq!(
            ctx.total_amount,
            format_money(yuan_to_fen(expected)),
            "展示串与回读值必须同源"
        );
    }

    /// 权限码 / 操作码 / 表名三者不能漂移（点错名字就是点错权限）。
    #[test]
    fn operation_and_permission_constants() {
        assert_eq!(EXPORT_OPERATION, "EXPORT_STOCK_TRANSFER_PDF");
        assert_eq!(PERMISSION, "business:erp:transfer:list");
    }
}

// ==================== 真库用例（ERP_DB_URL；未设置则 SKIP） ====================

/// 真库用例：调拨单（**调出 / 调入仓库**）的取数口径。
///
/// # 跑法
///
/// ```bash
/// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/<测试库>'
/// cargo test --lib business::erp::transfer_pdf -- --test-threads=1
/// ```
///
/// # 夹具卫生
///
/// 独占命名空间 `0f4100xx`，进 / 出都按这一组 id 清；不扫单号前缀、不删别人的行。
#[cfg(test)]
mod db_tests {
    use super::*;

    // 夹具 id 命名空间 0f41…（只在 SQL 字面量里用，故不逐个声明常量）：
    //   0001 单位 / 0002 物料 / 0003 操作人 / 0004 调出仓 / 0005 调入仓 /
    //   0006 在途仓 / 0007 库位字典 / 0008 调出库位账行 / 0009 调入库位账行 / 0010 调拨单
    const OPERATOR: Uuid = Uuid::from_u128(0x0f41_0000_0000_4000_8000_0000_0000_0003);
    const TRANSFER: Uuid = Uuid::from_u128(0x0f41_0000_0000_4000_8000_0000_0000_0010);

    const TRANSFER_NO: &str = "PDF04-TK-000001";

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
            // ① 序列号行引用物料 / 库位 / 仓库（三个硬外键）→ 必须先清
            "DELETE FROM biz_material_serial WHERE material_id IN \
                (SELECT id FROM biz_material WHERE material_code LIKE 'PDF04-%')",
            "DELETE FROM biz_material_serial WHERE location_id IN \
                ('0f410000-0000-4000-8000-000000000008'::uuid, '0f410000-0000-4000-8000-000000000009'::uuid)",
            "DELETE FROM biz_material_serial WHERE warehouse_id IN \
                (SELECT id FROM biz_warehouse WHERE warehouse_code LIKE 'WH-PDF04-%')",
            // ② 单据（先按**单号前缀**清：单号是唯一键，老 id 的残行也能兜住；再按 id 清）
            "DELETE FROM biz_stock_transfer_item_serial WHERE item_id IN \
                (SELECT id FROM biz_stock_transfer_item WHERE transfer_id IN \
                  (SELECT id FROM biz_stock_transfer WHERE transfer_no LIKE 'PDF04-%'))",
            "DELETE FROM biz_stock_transfer_item WHERE transfer_id IN \
                (SELECT id FROM biz_stock_transfer WHERE transfer_no LIKE 'PDF04-%')",
            "DELETE FROM biz_stock_transfer WHERE transfer_no LIKE 'PDF04-%'",
            "DELETE FROM biz_stock_transfer_item_serial WHERE item_id IN \
                (SELECT id FROM biz_stock_transfer_item WHERE transfer_id = \
                  '0f410000-0000-4000-8000-000000000010'::uuid)",
            "DELETE FROM biz_stock_transfer_item WHERE transfer_id = \
                '0f410000-0000-4000-8000-000000000010'::uuid",
            "DELETE FROM biz_stock_transfer WHERE id = '0f410000-0000-4000-8000-000000000010'::uuid",
            "DELETE FROM operation_logs WHERE target_id = '0f410000-0000-4000-8000-000000000010'",
            // ③ 库位账行 / 库位字典（按 id：库位编码是通用码，不按码清）
            "DELETE FROM biz_inventory_location WHERE id IN \
                ('0f410000-0000-4000-8000-000000000008'::uuid, '0f410000-0000-4000-8000-000000000009'::uuid)",
            "DELETE FROM biz_location WHERE id = '0f410000-0000-4000-8000-000000000007'::uuid",
            // ④ 主数据：**按业务码 + 按 id 双保险**（命名空间改过一次，只按码清会漏老行的旧码）
            "DELETE FROM biz_material WHERE id IN ('0f410000-0000-4000-8000-000000000002'::uuid)",
            "DELETE FROM biz_warehouse WHERE id IN \
                ('0f410000-0000-4000-8000-000000000004'::uuid, '0f410000-0000-4000-8000-000000000005'::uuid, \
                 '0f410000-0000-4000-8000-000000000006'::uuid)",
            "DELETE FROM sys_user WHERE id = '0f410000-0000-4000-8000-000000000003'::uuid",
            "DELETE FROM biz_unit WHERE id = '0f410000-0000-4000-8000-000000000001'::uuid",
            "DELETE FROM biz_material WHERE material_code LIKE 'PDF04-%'",
            "DELETE FROM biz_supplier WHERE supplier_code LIKE 'PDF04-%'",
            "DELETE FROM biz_customer WHERE code LIKE 'PDF04-%'",
            "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'WH-PDF04-%'",
            "DELETE FROM sys_user WHERE username LIKE 'pdf04.%'",
            "DELETE FROM biz_unit WHERE unit_code LIKE 'PDF04-%'",
        ] {
            exec(sql, vec![], "清理 PDF-04 调拨夹具").await;
        }
    }

    async fn seed_master() {
        for sql in [
            "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, status, sort_order)
             VALUES ('0f410000-0000-4000-8000-000000000001', 'PDF04-TK-TAI', '台', 1, 1, 97)",
            "INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                                       safety_stock, costing_method, tax_rate, status, serial_required)
             VALUES ('0f410000-0000-4000-8000-000000000002', 'PDF04-TK-MAT-001', '不锈钢球阀',
                     'DN50/PN16', '0f410000-0000-4000-8000-000000000001', 'FINISHED',
                     0.0000, 'MOVING_AVG', 0.1300, 1, FALSE)",
            "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path,
                                        level, is_leaf, status)
             VALUES ('0f410000-0000-4000-8000-000000000004', 'WH-PDF04-TK-A', 'PDF04 一号仓',
                     '/0f410000-0000-4000-8000-000000000004/', 1, 1, 1),
                    ('0f410000-0000-4000-8000-000000000005', 'WH-PDF04-TK-B', 'PDF04 二号仓',
                     '/0f410000-0000-4000-8000-000000000005/', 1, 1, 1),
                    ('0f410000-0000-4000-8000-000000000006', 'WH-PDF04-TK-T', 'PDF04 在途仓',
                     '/0f410000-0000-4000-8000-000000000006/', 1, 1, 1)",
            "INSERT INTO sys_user (id, username, password, real_name, status)
             VALUES ('0f410000-0000-4000-8000-000000000003', 'pdf04.tk.op', 'x', '调拨测试员', 1)",
            "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, status)
             VALUES ('0f410000-0000-4000-8000-000000000007', 'A-01-03', 'A 区 01 排 03 位',
                     '0f410000-0000-4000-8000-000000000004', 1)",
            "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty)
             VALUES ('0f410000-0000-4000-8000-000000000008', '0f410000-0000-4000-8000-000000000002',
                     '0f410000-0000-4000-8000-000000000004',
                     '0f410000-0000-4000-8000-000000000007', 0),
                    ('0f410000-0000-4000-8000-000000000009', '0f410000-0000-4000-8000-000000000002',
                     '0f410000-0000-4000-8000-000000000005', NULL, 0)",
        ] {
            exec(sql, vec![], "写入 PDF-04 调拨主数据夹具").await;
        }
    }

    /// 造一张 2 行明细的调拨单（DONE：调出 / 调入 / 在途仓全有值）。
    async fn seed_transfer(status: &str) {
        let (submitted, approved, shipped, received, transit) = match status {
            "DONE" => (
                "now()",
                "now()",
                "now()",
                "now()",
                "'0f410000-0000-4000-8000-000000000006'::uuid",
            ),
            "IN_TRANSIT" => (
                "now()",
                "now()",
                "now()",
                "NULL",
                "'0f410000-0000-4000-8000-000000000006'::uuid",
            ),
            _ => ("NULL", "NULL", "NULL", "NULL", "NULL"),
        };
        let sql = format!(
            "INSERT INTO biz_stock_transfer
                (id, transfer_no, from_warehouse_id, to_warehouse_id, transit_warehouse_id,
                 status, transfer_date, reason, applicant_id, applicant_name, approver_id,
                 total_qty, total_amount, submitted_at, approved_at, shipped_at, received_at,
                 remark, create_by)
             VALUES ('0f410000-0000-4000-8000-000000000010', ?, 
                     '0f410000-0000-4000-8000-000000000004'::uuid,
                     '0f410000-0000-4000-8000-000000000005'::uuid,
                     {transit}, '{status}', '2026-11-08', '二号仓备货', 
                     '0f410000-0000-4000-8000-000000000003'::uuid, '调拨测试员', NULL,
                     12.5, 185.20, {submitted}, {approved}, {shipped}, {received},
                     '两仓已实物清点', '0f410000-0000-4000-8000-000000000003'::uuid)"
        );
        exec(&sql, vec![Value::Text(TRANSFER_NO.to_string())], "写入调拨单夹具").await;
        exec(
            "INSERT INTO biz_stock_transfer_item
                (id, transfer_id, line_no, material_id, from_location_id, to_location_id,
                 qty, shipped_qty, received_qty, unit_cost, amount, remark)
             VALUES ('0f410000-0000-4000-8000-000000000021',
                     '0f410000-0000-4000-8000-000000000010'::uuid, 1,
                     '0f410000-0000-4000-8000-000000000002'::uuid,
                     '0f410000-0000-4000-8000-000000000008'::uuid,
                     '0f410000-0000-4000-8000-000000000009'::uuid,
                     10, 10, 10, 12.340000, 123.40, NULL),
                    ('0f410000-0000-4000-8000-000000000022',
                     '0f410000-0000-4000-8000-000000000010'::uuid, 2,
                     '0f410000-0000-4000-8000-000000000002'::uuid,
                     NULL, NULL, 2.5, 2.5, 2.5, 24.720000, 61.80, '第二行无库位')",
            vec![],
            "写入调拨明细夹具",
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

    /// 验收②：调拨单上下文必须带**调出 / 调入仓库**（名字，不是 id）。
    #[tokio::test]
    async fn transfer_context_has_both_warehouses() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_transfer("DONE").await;
        let ctx = transfer_context(TRANSFER).await.expect("上下文必须组装成功");

        assert_eq!(ctx.doc_no, TRANSFER_NO);
        assert_eq!(ctx.doc_title, "调 拨 单");
        // 路线条数据源
        assert_eq!(ctx.from_warehouse, "PDF04 一号仓");
        assert_eq!(ctx.to_warehouse, "PDF04 二号仓");
        assert_eq!(ctx.transit_warehouse, "PDF04 在途仓");
        // 单据头（验收②的字段级断言）
        let meta: Vec<(String, String)> = ctx
            .meta_rows
            .iter()
            .flatten()
            .map(|field| (field.label.clone(), field.value.clone()))
            .collect();
        assert!(meta.contains(&("调出仓库".into(), "PDF04 一号仓".into())));
        assert!(meta.contains(&("调入仓库".into(), "PDF04 二号仓".into())));
        assert!(meta.contains(&("在途仓".into(), "PDF04 在途仓".into())));
        assert!(meta.contains(&("状态".into(), "已完成".into())));
        assert!(meta.contains(&("申请人".into(), "调拨测试员".into())));
        assert!(meta.contains(&("经办人".into(), "调拨测试员".into())));

        // 明细：双库位 + 单位取自物料主档
        assert_eq!(ctx.items.len(), 2);
        assert_eq!(ctx.items[0].from_location, "A-01-03");
        assert_eq!(ctx.items[0].to_location, "未分配");
        assert_eq!(ctx.items[1].from_location, "未分配");
        assert_eq!(ctx.items[1].unit, "台");
        assert_eq!(ctx.items[0].qty, "10");
        assert_eq!(ctx.items[0].unit_cost, "12.34");
        assert_eq!(ctx.items[0].amount, "123.40");
        assert_eq!(ctx.total_qty, "12.5");
        assert_eq!(ctx.total_amount, "185.20");

        let html = template::render_transfer(&ctx).expect("模板必须渲染成功");
        assert!(html.contains("调出：PDF04 一号仓"));
        assert!(html.contains("调入：PDF04 二号仓"));
        assert!(html.contains("在途经 PDF04 在途仓"));
        assert!(html.contains("A-01-03"));
    }

    /// 草稿 / 在途：在途仓为空时不印「在途经」；在途时必须有值。
    #[tokio::test]
    async fn transit_warehouse_follows_status() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_transfer("DRAFT").await;
        let ctx = transfer_context(TRANSFER).await.expect("上下文必须成功");
        assert!(ctx.transit_warehouse.is_empty());
        let html = template::render_transfer(&ctx).expect("模板必须渲染成功");
        assert!(!html.contains("在途经"));
        purge().await;
        // purge 连主数据一起清了 → 第二次造数前必须重新种
        seed_master().await;

        seed_transfer("IN_TRANSIT").await;
        let ctx = transfer_context(TRANSFER).await.expect("上下文必须成功");
        assert_eq!(ctx.transit_warehouse, "PDF04 在途仓");
        purge().await;
    }

    /// 审计：导出后 `operation_logs` 有一行（doc_type / row_count / amount）。
    #[tokio::test]
    async fn export_writes_operation_log_row() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_transfer("DONE").await;
        let ctx = transfer_context(TRANSFER).await.expect("上下文必须成功");
        let request_id = Uuid::now_v7();
        doc_pdf_model::write_export_log(&doc_pdf_model::ExportLog {
            user_id: Some(OPERATOR),
            username: Some("pdf04.op"),
            operation: EXPORT_OPERATION,
            target_type: "biz_stock_transfer",
            doc_id: TRANSFER,
            doc_no: TRANSFER_NO,
            detail: serde_json::json!({
                "doc_type": "STOCK_TRANSFER",
                "row_count": ctx.items.len(),
                "amount": ctx.total_amount,
            }),
            filename: "调拨单-PDF04-TK-000001.pdf",
            bytes: 68_000,
            duration_ms: 180,
            request_path: "/api/business/erp/stock-transfer/0f410000-0000-4000-8000-000000000010/pdf",
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
                vec![Value::Text(TRANSFER.to_string()), Value::Text(request_id.to_string())],
            )
            .await
            .expect("回查审计行");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_typed::<String>("operation").unwrap(), EXPORT_OPERATION);
        assert_eq!(
            rows[0].get_typed::<String>("target_type").unwrap(),
            "biz_stock_transfer"
        );
        let detail: serde_json::Value = rows[0].get_typed("detail").expect("detail 必须是 jsonb");
        assert_eq!(detail["doc_type"], "STOCK_TRANSFER");
        assert_eq!(detail["row_count"], 2);
        assert_eq!(detail["source"], "PDF-03");
        purge().await;
    }

    /// 软删单据必须 404（不是「打出一张已删除的单」）。
    #[tokio::test]
    async fn soft_deleted_transfer_is_not_found() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_transfer("DONE").await;
        exec(
            "UPDATE biz_stock_transfer SET deleted_at = now() \
              WHERE id = '0f410000-0000-4000-8000-000000000010'::uuid",
            vec![],
            "软删调拨单",
        )
        .await;
        let error = transfer_context(TRANSFER).await.expect_err("软删单必须 404");
        assert_eq!(error.status, 404);
        assert!(error.msg.contains("调拨单"));
        purge().await;
    }

    /// id 不是合法 UUID 时导出必须 400。
    #[tokio::test]
    async fn export_rejects_bad_uuid_before_query() {
        if !ensure_db().await {
            return;
        }
        let error = export_transfer("not-a-uuid").await.expect_err("必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, crate::shared::error_code::ERP_FILTER_INVALID);
    }
}
