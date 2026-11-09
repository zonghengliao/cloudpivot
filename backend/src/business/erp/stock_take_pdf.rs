//! 盘点单 **PDF 导出**（PDF-04：**盘点表格式**，留空白手写栏）。
//!
//! # 接口（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/business/...`）
//!
//! | 方法 | 路径 | 权限码（复用单据查看权限） | 说明 |
//! |------|------|------------------------------|------|
//! | GET | `/business/erp/stock-take/{id}/pdf` | `business:erp:stock-take:list` | 盘点表 PDF |
//!
//! # 这张表为什么特殊
//!
//! 它是**唯一**需要空白栏的单据：打印出来要拿到仓库现场盘点，
//! 「实盘数量」「差异」「备注」三栏由盘点人手写。
//!
//! 因此本模块**不把库里的 `actual_qty` / `diff_qty` 交给模板** ——
//! [`template::StockTakeItem`] 结构体里根本没有这三个字段（见其文档）。
//! 系统里已录的实盘数只在「合计区」以盘盈 / 盘亏合计的形式出现，
//! 且仅在**确实有差异**时才打印（全 0 会让人误以为已经盘过）。
//!
//! # 卡面要求的字段落点
//!
//! | 要求 | 数据来源 |
//! |------|----------|
//! | 仓库 / 库位 | `warehouse_id` → `biz_warehouse`；明细 `location_id`（**库位账行**）→ `biz_location.location_code` |
//! | 账面数量 | 明细 `book_qty`（建单时快照，**不可改**） |
//! | 实盘 / 差异 / 备注 | **留空**（现场手写） |
//! | 盘点类型 | `take_type`（FULL 全盘 / PARTIAL 抽盘） |
//! | 差异合计 | 单头 `total_gain_qty` / `total_loss_qty` / `total_gain_amount + total_loss_amount`（过账时写入的权威值） |
//!
//! # 审计
//!
//! 导出是 GET，中间件不记；渲染成功后显式补 `EXPORT_STOCK_TAKE_PDF` 一行。

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
use crate::business::erp::stocktake_model::{self, StockTakeItemRow};
use crate::business::pdf::handler as pdf_handler;
use crate::business::pdf::template::{
    self, OrderMetaField, StockTakeContext, StockTakeContextArgs, StockTakeItem, format_quantity,
};
use crate::utils;

/// 审计里的操作码。
pub const EXPORT_OPERATION: &str = "EXPORT_STOCK_TAKE_PDF";

/// 路由权限码（复用盘点单查看权限）。
pub const PERMISSION: &str = "business:erp:stock-take:list";

/// 明细行 → 模板行（纯函数，单测直接调）。
///
/// # 参数
///
/// - `sources`：明细原始行（已按 `line_no` 升序）；
/// - `materials`：物料主档；
/// - `units`：单位 id → 单位名；
/// - `locations`：库位账行 id → 库位编码。
///
/// # 只放「账面数量」
///
/// 实盘数 / 差异 / 备注**刻意不传**（验收①）：模板要把这三栏留白给现场手写。
/// 传进来就等于给了模板一个「顺手印上去」的机会。
pub fn build_stock_take_items(
    sources: &[StockTakeItemRow],
    materials: &std::collections::HashMap<Uuid, MaterialInfo>,
    units: &std::collections::HashMap<Uuid, String>,
    locations: &std::collections::HashMap<Uuid, String>,
) -> Vec<StockTakeItem> {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let material = materials.get(&source.material_id);
            StockTakeItem {
                seq: index as u32 + 1,
                material_code: material
                    .map(|info| info.code.clone())
                    .unwrap_or_else(|| "（物料已删除）".to_string()),
                material_name: material
                    .map(|info| info.name.clone())
                    .unwrap_or_else(|| "（物料已删除）".to_string()),
                spec: material.map(|info| info.spec.clone()).unwrap_or_default(),
                unit: units.get(&source.unit_id).cloned().unwrap_or_else(|| "—".to_string()),
                location: match source.location_id {
                    None => "未分配".to_string(),
                    Some(id) => locations
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| "已删除的库位账行".to_string()),
                },
                book_qty: format_quantity(source.book_qty),
            }
        })
        .collect()
}

/// 盘点单的渲染上下文（真库取数 + 批量补名字）。
///
/// # 异常情况
///
/// - 404 [`crate::shared::error_code::RESOURCE_NOT_FOUND`]：单据不存在 / 已软删；
/// - 500：数据库异常。
pub async fn stock_take_context(id: Uuid) -> std::result::Result<StockTakeContext, ApiError> {
    let row = stocktake_model::find(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点单"))?
        .ok_or_else(|| doc_pdf_model::not_found("盘点单"))?;
    let lines = stocktake_model::items_of(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点单明细"))?;

    let material_ids: Vec<Uuid> = lines.iter().map(|line| line.material_id).collect();
    let materials = order_pdf_model::materials_of(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点单物料"))?;
    let unit_ids: Vec<Uuid> = lines.iter().map(|line| line.unit_id).collect();
    let units = order_pdf_model::unit_names_of(&unit_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点单单位"))?;
    let location_ids: Vec<Uuid> = lines.iter().filter_map(|line| line.location_id).collect();
    let locations = doc_pdf_model::location_labels_of(&location_ids)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点单库位"))?;
    let warehouse = doc_pdf_model::warehouse_names_of(&[row.warehouse_id])
        .await
        .map_err(|error| utils::db_error(error, "查询盘点单仓库"))?
        .get(&row.warehouse_id)
        .cloned()
        .unwrap_or_else(|| "（仓库已删除）".to_string());

    let people: Vec<Uuid> = row
        .create_by
        .into_iter()
        .chain(row.posted_by)
        .chain(row.cancelled_by)
        .collect();
    let users = order_pdf_model::user_names_of(&people)
        .await
        .map_err(|error| utils::db_error(error, "查询盘点单经办人"))?;
    let operator = row
        .create_by
        .and_then(|user_id| users.get(&user_id).cloned())
        .unwrap_or_else(|| "—".to_string());
    let posted_by = row
        .posted_by
        .and_then(|user_id| users.get(&user_id).cloned())
        .unwrap_or_else(|| "—".to_string());

    let items = build_stock_take_items(&lines, &materials, &units, &locations);
    let total_book_qty: Decimal = lines.iter().map(|line| line.book_qty).sum();
    let mut remark = row.remark.clone().unwrap_or_default();
    if let Some(reason) = row
        .cancel_reason
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        if !remark.is_empty() {
            remark.push('；');
        }
        remark.push_str(&format!("作废原因：{reason}"));
    }

    Ok(StockTakeContext::new(StockTakeContextArgs {
        doc_no: row.take_no.clone(),
        meta: vec![
            OrderMetaField::new("单号", row.take_no.clone()),
            OrderMetaField::new("盘点日期", row.take_date.format("%Y-%m-%d").to_string()),
            OrderMetaField::new("状态", stocktake_model::status_label(&row.status)),
            OrderMetaField::new("仓库", warehouse),
            OrderMetaField::new("盘点类型", stocktake_model::take_type_label(&row.take_type)),
            OrderMetaField::new("制单", operator),
            OrderMetaField::new("过账人", posted_by),
        ],
        items,
        total_book_qty,
        take_type: row.take_type.clone(),
        gain_qty: row.total_gain_qty,
        loss_qty: row.total_loss_qty,
        diff_amount: row.total_diff_amount(),
        remark,
        printed_at: doc_pdf_model::printed_at_now(),
    }))
}

/// 导出盘点单 PDF（取数 + 渲染，不含 HTTP 头 / 审计）。
///
/// # 异常情况
///
/// - 400 [`crate::shared::error_code::ERP_FILTER_INVALID`]：id 非法；
/// - 404：单据不存在；
/// - 422 / 503 / 504：模板 / 渲染引擎异常。
pub async fn export_stock_take(id_raw: &str) -> std::result::Result<RenderedDoc, ApiError> {
    let id = doc_pdf_model::parse_doc_id(id_raw)?;
    let context = stock_take_context(id).await?;
    let html = template::render_stock_take(&context).map_err(|error| error.into_api())?;
    let filename = doc_pdf_render::pdf_filename(&context.doc_label, &context.doc_no);
    let opts = template::stock_take_pdf_opts(&context.doc_no, Some(filename));
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

/// 审计金额：**差异金额合计**（盘点单的「金额」是差异，不是货值）。
fn context_total(context: &StockTakeContext) -> Decimal {
    let digits: String = context
        .total_diff_amount
        .chars()
        .filter(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == '-')
        .collect();
    digits.parse::<Decimal>().unwrap_or(Decimal::ZERO)
}

/// 盘点单 PDF（`GET /api/business/erp/stock-take/{id}/pdf`）。
///
/// # 异常情况
///
/// - 401：未登录；
/// - 403 [`crate::shared::error_code::PERMISSION_DENIED`]：缺 `business:erp:stock-take:list`；
/// - 400 / 404 / 422 / 503 / 504：见 [`export_stock_take`]。
#[nik::permission(
    path = "/business/erp/stock-take/{id}/pdf",
    method = "GET",
    permission = "business:erp:stock-take:list"
)]
#[nik::get("/business/erp/stock-take/{id}/pdf")]
pub async fn stock_take_pdf(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> Response {
    let started = Instant::now();
    match export_stock_take(&id).await {
        Ok(rendered) => {
            let response = match pdf_handler::pdf_response(&rendered.bytes, Some(&rendered.filename)) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(
                        doc_no = %rendered.doc_no,
                        bytes = rendered.bytes.len(),
                        "盘点单 PDF 响应组装失败"
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
        target_type: "biz_stock_take".to_string(),
        doc_id,
        doc_no: rendered.doc_no.clone(),
        detail: serde_json::json!({
            "doc_type": "STOCK_TAKE",
            "row_count": rendered.row_count,
            "diff_amount": rendered.total_amount.round_dp(2).to_string(),
            "render_ms": rendered.duration_ms,
        }),
        filename: rendered.filename.clone(),
        bytes: rendered.bytes.len(),
        duration_ms: started.elapsed().as_millis() as u64,
        request_path: format!("/api/business/erp/stock-take/{doc_id}/pdf"),
        client_ip: utils::client_ip(headers, Some(connect_info)),
        user_agent: utils::user_agent(headers),
        request_id: Uuid::now_v7(),
    }
    .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn material(code: &str, name: &str, spec: &str, unit_id: Uuid) -> MaterialInfo {
        MaterialInfo {
            code: code.to_string(),
            name: name.to_string(),
            spec: spec.to_string(),
            unit_id,
        }
    }

    fn source(material_id: Uuid, unit_id: Uuid, location: Option<Uuid>, book: Decimal) -> StockTakeItemRow {
        StockTakeItemRow {
            id: Uuid::now_v7(),
            take_id: Uuid::now_v7(),
            line_no: 1,
            material_id,
            unit_id,
            location_id: location,
            book_qty: book,
            actual_qty: book,
            diff_qty: Decimal::ZERO,
            unit_cost: Decimal::ZERO,
            diff_amount: Decimal::ZERO,
            remark: None,
        }
    }

    /// 明细只有账面数量；实盘 / 差异不进模板结构（验收①的结构性保证）。
    #[test]
    fn items_carry_only_book_qty() {
        let material_id = Uuid::now_v7();
        let unit_id = Uuid::now_v7();
        let location = Uuid::now_v7();
        let sources = vec![
            source(material_id, unit_id, Some(location), Decimal::from(100)),
            source(material_id, unit_id, None, Decimal::new(25_500, 2)),
        ];
        let mut materials = HashMap::new();
        materials.insert(material_id, material("MAT-001", "球阀", "DN50", unit_id));
        let mut units = HashMap::new();
        units.insert(unit_id, "台".to_string());
        let mut locations = HashMap::new();
        locations.insert(location, "A-01-03".to_string());

        let items = build_stock_take_items(&sources, &materials, &units, &locations);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].seq, 1);
        assert_eq!(items[0].material_code, "MAT-001");
        assert_eq!(items[0].spec, "DN50");
        assert_eq!(items[0].unit, "台");
        assert_eq!(items[0].location, "A-01-03");
        assert_eq!(items[0].book_qty, "100");
        assert_eq!(items[1].location, "未分配");
        assert_eq!(items[1].book_qty, "255", "尾零去掉但小数不丢");
    }

    /// 物料 / 单位 / 库位账行查不到时走兜底文案（不 500、不编值）。
    #[test]
    fn missing_master_data_degrades_gracefully() {
        let sources = vec![source(
            Uuid::now_v7(),
            Uuid::now_v7(),
            Some(Uuid::now_v7()),
            Decimal::ZERO,
        )];
        let items = build_stock_take_items(&sources, &HashMap::new(), &HashMap::new(), &HashMap::new());
        assert_eq!(items[0].material_code, "（物料已删除）");
        assert_eq!(items[0].unit, "—");
        assert_eq!(items[0].location, "已删除的库位账行");
        assert_eq!(items[0].book_qty, "0");
    }

    /// 审计金额回读 = 差异金额合计（盘点单的「金额」口径）。
    #[test]
    fn context_total_reads_diff_amount() {
        let ctx = template::sample_stock_take(2);
        assert_eq!(context_total(&ctx), Decimal::ZERO, "样本无差异");

        let mut ctx = ctx;
        ctx.total_diff_amount = "1,234.56".to_string();
        assert_eq!(context_total(&ctx), Decimal::new(123_456, 2));
    }

    /// 操作码 / 权限码 / 表名不能漂移。
    #[test]
    fn operation_and_permission_constants() {
        assert_eq!(EXPORT_OPERATION, "EXPORT_STOCK_TAKE_PDF");
        assert_eq!(PERMISSION, "business:erp:stock-take:list");
    }
}

// ==================== 真库用例（ERP_DB_URL；未设置则 SKIP） ====================

/// 真库用例：盘点表的取数口径（账面快照 + 库位 + 仓库 + 差异合计）。
///
/// # 跑法
///
/// ```bash
/// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/<测试库>'
/// cargo test --lib business::erp::stock_take_pdf -- --test-threads=1
/// ```
///
/// # 夹具卫生
///
/// 独占命名空间 `0f4200xx`。
#[cfg(test)]
mod db_tests {
    use super::*;

    // 夹具 id 命名空间 0f42…（只在 SQL 字面量里用，故不逐个声明常量）：
    //   0001 单位 / 0002 物料 / 0003 操作人 / 0004 仓库 / 0005 库位字典 /
    //   0006 库位账行 / 0010 盘点单
    const OPERATOR: Uuid = Uuid::from_u128(0x0f42_0000_0000_4000_8000_0000_0000_0003);
    const TAKE: Uuid = Uuid::from_u128(0x0f42_0000_0000_4000_8000_0000_0000_0010);

    const TAKE_NO: &str = "PDF04-ST-000001";

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
            // 序列号行引用物料 / 库位 / 仓库 → 先清（否则 FK 拦住删库位）
            "DELETE FROM biz_material_serial WHERE material_id IN \
                (SELECT id FROM biz_material WHERE material_code LIKE 'PDF04-%')",
            "DELETE FROM biz_material_serial WHERE location_id = '0f420000-0000-4000-8000-000000000006'::uuid",
            "DELETE FROM biz_material_serial WHERE warehouse_id IN \
                (SELECT id FROM biz_warehouse WHERE warehouse_code LIKE 'WH-PDF04-%')",
            // 单据：先按单号前缀清（兜住老 id 的残行），再按 id 清
            "DELETE FROM biz_stock_take_item WHERE take_id IN \
                (SELECT id FROM biz_stock_take WHERE take_no LIKE 'PDF04-%')",
            "DELETE FROM biz_stock_take WHERE take_no LIKE 'PDF04-%'",
            "DELETE FROM biz_stock_take_item WHERE take_id = '0f420000-0000-4000-8000-000000000010'::uuid",
            "DELETE FROM biz_stock_take WHERE id = '0f420000-0000-4000-8000-000000000010'::uuid",
            "DELETE FROM operation_logs WHERE target_id = '0f420000-0000-4000-8000-000000000010'",
            "DELETE FROM biz_inventory_location WHERE id = '0f420000-0000-4000-8000-000000000006'::uuid",
            "DELETE FROM biz_location WHERE id = '0f420000-0000-4000-8000-000000000005'::uuid",
            // 主数据：按 id + 按业务码双保险（命名空间改过一次，老行的旧码可能不匹配新前缀）
            "DELETE FROM biz_material WHERE id = '0f420000-0000-4000-8000-000000000002'::uuid",
            "DELETE FROM biz_warehouse WHERE id = '0f420000-0000-4000-8000-000000000004'::uuid",
            "DELETE FROM sys_user WHERE id = '0f420000-0000-4000-8000-000000000003'::uuid",
            "DELETE FROM biz_unit WHERE id = '0f420000-0000-4000-8000-000000000001'::uuid",
            "DELETE FROM biz_material WHERE material_code LIKE 'PDF04-%'",
            "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'WH-PDF04-%'",
            "DELETE FROM sys_user WHERE username LIKE 'pdf04.%'",
            "DELETE FROM biz_unit WHERE unit_code LIKE 'PDF04-%'",
        ] {
            exec(sql, vec![], "清理 PDF-04 盘点夹具").await;
        }
    }

    async fn seed_master() {
        for sql in [
            "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, status, sort_order)
             VALUES ('0f420000-0000-4000-8000-000000000001', 'PDF04-ST-TAI', '台', 1, 1, 96)",
            "INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                                       safety_stock, costing_method, tax_rate, status, serial_required)
             VALUES ('0f420000-0000-4000-8000-000000000002', 'PDF04-ST-MAT', '不锈钢法兰',
                     'DN80/PN16', '0f420000-0000-4000-8000-000000000001', 'RAW',
                     0.0000, 'MOVING_AVG', 0.1300, 1, FALSE)",
            "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path,
                                        level, is_leaf, status)
             VALUES ('0f420000-0000-4000-8000-000000000004', 'WH-PDF04-ST', 'PDF04 盘点仓',
                     '/0f420000-0000-4000-8000-000000000004/', 1, 1, 1)",
            "INSERT INTO sys_user (id, username, password, real_name, status)
             VALUES ('0f420000-0000-4000-8000-000000000003', 'pdf04.st.op', 'x', '盘点测试员', 1)",
            "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, status)
             VALUES ('0f420000-0000-4000-8000-000000000005', 'A-09-09', 'A 区 09 排 09 位',
                     '0f420000-0000-4000-8000-000000000004', 1)",
            "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty)
             VALUES ('0f420000-0000-4000-8000-000000000006',
                     '0f420000-0000-4000-8000-000000000002',
                     '0f420000-0000-4000-8000-000000000004',
                     '0f420000-0000-4000-8000-000000000005', 0)",
        ] {
            exec(sql, vec![], "写入 PDF-04 盘点主数据夹具").await;
        }
    }

    /// 造一张 2 行明细的盘点单（PENDING_POST：实盘已录、差异已有、未过账）。
    async fn seed_take() {
        exec(
            "INSERT INTO biz_stock_take
                (id, take_no, warehouse_id, take_date, take_type, status,
                 total_book_qty, total_actual_qty, total_gain_qty, total_loss_qty,
                 total_gain_amount, total_loss_amount, remark, create_by)
             VALUES ('0f420000-0000-4000-8000-000000000010', ?,
                     '0f420000-0000-4000-8000-000000000004'::uuid, '2026-11-08', 'FULL',
                     'PENDING_POST', 100.5, 97.5, 2, 5, 24.68, 61.70, '年末全盘',
                     '0f420000-0000-4000-8000-000000000003'::uuid)",
            vec![Value::Text(TAKE_NO.to_string())],
            "写入盘点单夹具",
        )
        .await;
        exec(
            "INSERT INTO biz_stock_take_item
                (id, take_id, line_no, material_id, unit_id, location_id,
                 book_qty, actual_qty, unit_cost, diff_amount, remark)
             VALUES ('0f420000-0000-4000-8000-000000000021',
                     '0f420000-0000-4000-8000-000000000010'::uuid, 1,
                     '0f420000-0000-4000-8000-000000000002'::uuid,
                     '0f420000-0000-4000-8000-000000000001'::uuid,
                     '0f420000-0000-4000-8000-000000000006'::uuid,
                     100, 102, 12.340000, 24.68, '盘盈 2 台'),
                    ('0f420000-0000-4000-8000-000000000022',
                     '0f420000-0000-4000-8000-000000000010'::uuid, 2,
                     '0f420000-0000-4000-8000-000000000002'::uuid,
                     '0f420000-0000-4000-8000-000000000001'::uuid,
                     NULL, 0.5, 0, 12.340000, 61.70, '盘亏 0.5 台')",
            vec![],
            "写入盘点明细夹具",
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

    /// 验收①：上下文里**没有**实盘数 / 差异（只有账面快照），渲染后三栏空白。
    #[tokio::test]
    async fn stock_take_context_leaves_handwrite_columns_blank() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_take().await;
        let ctx = stock_take_context(TAKE).await.expect("上下文必须组装成功");

        assert_eq!(ctx.doc_no, TAKE_NO);
        assert_eq!(ctx.doc_title, "盘 点 表");
        assert_eq!(ctx.take_type, "全盘");
        assert_eq!(ctx.items.len(), 2);
        // 账面快照在（现场比对用），实盘 / 差异不在结构里（验收①的结构性保证）
        assert_eq!(ctx.items[0].book_qty, "100");
        assert_eq!(ctx.items[1].book_qty, "0.5");
        assert_eq!(ctx.items[0].location, "A-09-09");
        assert_eq!(ctx.items[1].location, "未分配");
        assert_eq!(ctx.items[0].unit, "台");
        assert_eq!(ctx.total_book_qty, "100.5");
        // 已录实盘 → 差异合计要印（盘盈 2 / 盘亏 5 / 差异金额 86.38）
        assert!(ctx.show_diff);
        assert_eq!(ctx.total_gain_qty, "2");
        assert_eq!(ctx.total_loss_qty, "5");
        assert_eq!(ctx.total_diff_amount, "86.38");

        let meta: Vec<(String, String)> = ctx
            .meta_rows
            .iter()
            .flatten()
            .map(|field| (field.label.clone(), field.value.clone()))
            .collect();
        assert!(meta.contains(&("仓库".into(), "PDF04 盘点仓".into())));
        assert!(meta.contains(&("状态".into(), "待过账".into())));
        assert!(meta.contains(&("制单".into(), "盘点测试员".into())));

        let html = template::render_stock_take(&ctx).expect("模板必须渲染成功");
        // 三列手写栏：每行三个空格；库里已有的实盘数（102 / 0.5）不许出现
        assert_eq!(html.matches("<td class=\"w-actual blank\"></td>").count(), 2);
        assert_eq!(html.matches("<td class=\"w-diff blank\"></td>").count(), 2);
        assert_eq!(html.matches("<td class=\"w-note blank\"></td>").count(), 2);
        assert!(!html.contains("w-actual blank\">102"));
        assert!(html.contains("A-09-09"));
    }

    /// 草稿（还没录实盘）→ 不印盘盈盘亏合计（避免被误读成已盘过）。
    #[tokio::test]
    async fn draft_take_has_no_diff_block() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_take().await;
        exec(
            "UPDATE biz_stock_take SET status = 'DRAFT', total_actual_qty = 0, \
                    total_gain_qty = 0, total_loss_qty = 0, total_gain_amount = 0, total_loss_amount = 0 \
              WHERE id = '0f420000-0000-4000-8000-000000000010'::uuid",
            vec![],
            "把盘点单打回草稿",
        )
        .await;
        let ctx = stock_take_context(TAKE).await.expect("上下文必须成功");
        assert!(!ctx.show_diff);
        let html = template::render_stock_take(&ctx).expect("模板必须渲染成功");
        assert!(!html.contains("盘盈数量合计"));
        purge().await;
    }

    /// 审计：导出后 `operation_logs` 有一行（差异金额口径）。
    #[tokio::test]
    async fn export_writes_operation_log_row() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_take().await;
        let ctx = stock_take_context(TAKE).await.expect("上下文必须成功");
        let request_id = Uuid::now_v7();
        doc_pdf_model::write_export_log(&doc_pdf_model::ExportLog {
            user_id: Some(OPERATOR),
            username: Some("pdf04.st"),
            operation: EXPORT_OPERATION,
            target_type: "biz_stock_take",
            doc_id: TAKE,
            doc_no: TAKE_NO,
            detail: serde_json::json!({
                "doc_type": "STOCK_TAKE",
                "row_count": ctx.items.len(),
                "diff_amount": ctx.total_diff_amount,
            }),
            filename: "盘点单-PDF04-ST-000001.pdf",
            bytes: 64_000,
            duration_ms: 150,
            request_path: "/api/business/erp/stock-take/0f420000-0000-4000-8000-000000000010/pdf",
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
                vec![Value::Text(TAKE.to_string()), Value::Text(request_id.to_string())],
            )
            .await
            .expect("回查审计行");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_typed::<String>("operation").unwrap(), EXPORT_OPERATION);
        assert_eq!(rows[0].get_typed::<String>("target_type").unwrap(), "biz_stock_take");
        let detail: serde_json::Value = rows[0].get_typed("detail").expect("detail 必须是 jsonb");
        assert_eq!(detail["doc_type"], "STOCK_TAKE");
        assert_eq!(detail["row_count"], 2);
        assert_eq!(detail["diff_amount"], "86.38");
        purge().await;
    }

    /// 软删单据必须 404。
    #[tokio::test]
    async fn soft_deleted_take_is_not_found() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_take().await;
        exec(
            "UPDATE biz_stock_take SET deleted_at = now() \
              WHERE id = '0f420000-0000-4000-8000-000000000010'::uuid",
            vec![],
            "软删盘点单",
        )
        .await;
        let error = stock_take_context(TAKE).await.expect_err("软删单必须 404");
        assert_eq!(error.status, 404);
        assert!(error.msg.contains("盘点单"));
        purge().await;
    }

    /// id 非法 → 400。
    #[tokio::test]
    async fn export_rejects_bad_uuid_before_query() {
        if !ensure_db().await {
            return;
        }
        let error = export_stock_take("not-a-uuid").await.expect_err("必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, crate::shared::error_code::ERP_FILTER_INVALID);
    }
}
