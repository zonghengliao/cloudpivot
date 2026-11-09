#![cfg(test)]
//! 发货单列表批量补齐（缺陷卡 **t_45900ea7**）：`page_shipment` 必须与 `get_shipment` 逐字段同口径。
//!
//! # 这次要钉住的事
//!
//! SO-11 前端卡实测发现：`GET /shipment/page` 的列表行 `so_no` / `stock_out_no` 恒为 `null`，
//! 而 `GET /shipment/{id}` 有值 —— 同时 `ShipmentRes` 的文档注释承诺「列表会批量补齐 so_no」。
//! 注释与实现互相矛盾，用户体感是「列表缺数据」。修复后本模块用**真库**把口径钉死：
//!
//! 1. 列表行的 `so_no` / `stock_out_no` 与详情**逐字相等**（同一个值，不是"都有值"）；
//! 2. 未过账行 `stock_out_no` 为 `null`（没有 `stock_out_id` 时不查库、不编造）；
//! 3. 订单 / 出库单**软删**后，列表与详情**同时**变 `null` —— 这正是 `so_no_of` /
//!    `stock_out_no_of` 补上 `deleted_at IS NULL` 的原因（否则会出现"列表说有、详情说没有"）；
//! 4. 批量函数去重（一页多张单常常同一订单）且空入参短路。
//!
//! # 跑法（未设 `ERP_DB_URL` → 静默 SKIP = 假绿，验收必须显式带上）
//!
//! ```bash
//! ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_xxx' \
//!   cargo test --lib shipment_page_fill -- --test-threads=1
//! ```
//!
//! # 夹具隔离
//!
//! 本模块独占 UUID 命名空间 `c9c90001-…` 与备注前缀 `SO09FIX`：
//! 单号取 uuid 随机尾段（前 8 位是 v7 毫秒时间戳，同窗口多张单会撞唯一索引），
//! `setup()` 先按前缀清场，重复跑不积累，也删不到别人的行。

use std::str::FromStr;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::shipment_handler::{self, ShipmentFilter};
use crate::business::erp::shipment_model;

// ==================== 夹具常量（本模块独占的命名空间） ====================

const UNIT: &str = "c9c90001-0000-4000-8000-000000000001";
const CUSTOMER: &str = "c9c90001-0000-4000-8000-000000000002";
const WAREHOUSE: &str = "c9c90001-0000-4000-8000-000000000003";
const MATERIAL: &str = "c9c90001-0000-4000-8000-000000000004";

/// 备注前缀：清场按它删，绝不按单号前缀删（`SH-` / `OUT-` / `SO-` 是生产号段）。
const TAG: &str = "SO09FIX";

/// 真库用例串行执行（`cargo test` 默认多线程，同一张表互相踩）。
static FIXTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("定点可解析")
}

// ==================== 真库连接 ====================

/// 初始化全局数据库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
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

// ==================== 夹具 ====================

/// 清掉本模块的全部痕迹（顺序写死：明细 → 发货单 → 订单明细 → 订单；出库单最后）。
async fn cleanup() {
    let shipments = format!(
        "SELECT id FROM biz_shipment WHERE so_id IN \
         (SELECT id FROM biz_sales_order WHERE remark LIKE '{TAG}%')"
    );
    let orders = format!("SELECT id FROM biz_sales_order WHERE remark LIKE '{TAG}%'");
    for sql in [
        format!("DELETE FROM biz_shipment_item WHERE shipment_id IN ({shipments})"),
        format!("DELETE FROM biz_shipment WHERE so_id IN ({orders})"),
        format!("DELETE FROM biz_sales_order_item WHERE so_id IN ({orders})"),
        format!("DELETE FROM biz_sales_order WHERE remark LIKE '{TAG}%'"),
        format!("DELETE FROM biz_stock_out WHERE remark LIKE '{TAG}%'"),
    ] {
        if let Err(err) = db().execute_with_params(&sql, vec![]).await {
            println!("cleanup 语句失败（继续）：{err} —— {sql}");
        }
    }
}

/// 建夹具主数据 + 清上一轮。返回 `None` = 没配 `ERP_DB_URL`（用例 SKIP）。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = FIXTURE_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    for sql in [
        format!(
            "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order) \
             VALUES ('{UNIT}'::uuid, 'SO09-PCS', '个', 1, NULL, 1.000000, 1, 1) \
             ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_customer (id, code, name, status) \
             VALUES ('{CUSTOMER}'::uuid, 'SO09-CUS', 'SO09 列表补齐客户', 'ACTIVE') \
             ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, \
              allow_negative, status, sort_order, level, warehouse_path, is_leaf) \
             VALUES ('{WAREHOUSE}'::uuid, 'SO09-WH', 'SO09 列表补齐仓', 'NORMAL', 0, 1, 0, 1, \
                     '/c9c90001-0000-4000-8000-000000000003/', 1) \
             ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required) \
             VALUES ('{MATERIAL}'::uuid, 'SO09-M1', 'SO09 螺丝', '{UNIT}'::uuid, 1, false) \
             ON CONFLICT DO NOTHING"
        ),
    ] {
        if let Err(err) = db().execute_with_params(&sql, vec![]).await {
            panic!("夹具写入失败：{err}\nSQL: {sql}");
        }
    }
    Some(guard)
}

/// 单号取 uuid 随机尾段（`now_v7` 前 8 位是毫秒时间戳，同窗口多单会撞唯一索引）。
fn suffix(id: Uuid) -> String {
    id.simple().to_string()[24..32].to_string()
}

/// 造一张**已审批**的销售订单 + 一行明细，返回 `(so_id, so_no, so_item_id)`。
async fn create_approved_order() -> (Uuid, String, Uuid) {
    let so_id = Uuid::now_v7();
    let so_no = format!("SO-FIX-{}", suffix(so_id));
    let item_id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_sales_order \
             (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax, remark) \
         VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, 'APPROVED', 0, 0, 0, ?)",
        vec![
            Value::from(&so_id),
            Value::Text(so_no.clone()),
            Value::from(&uuid(CUSTOMER)),
            Value::Text(format!("{TAG} 列表补齐订单")),
        ],
    )
    .await
    .expect("夹具：销售订单写入失败");
    db().execute_with_params(
        "INSERT INTO biz_sales_order_item \
             (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty) \
         VALUES (?::uuid, ?::uuid, 1, ?::uuid, NULLIF(?::text,'')::numeric, 100, 1000, 13, 130, 0)",
        vec![
            Value::from(&item_id),
            Value::from(&so_id),
            Value::from(&uuid(MATERIAL)),
            Value::Text(dec("10").to_string()),
        ],
    )
    .await
    .expect("夹具：销售订单行写入失败");
    (so_id, so_no, item_id)
}

/// 造一张发货单，返回 `id`。`status` = `DRAFT` / `POSTED`；`stock_out_id` 只对已过账行给。
async fn create_shipment(so_id: Uuid, status: &str, stock_out_id: Option<Uuid>) -> Uuid {
    let id = Uuid::now_v7();
    let ship_no = format!("SH-FIX-{}", suffix(id));
    db().execute_with_params(
        "INSERT INTO biz_shipment \
             (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty, \
              stock_out_id, remark) \
         VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?::uuid, CURRENT_DATE, ?, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::uuid, ?)",
        vec![
            Value::from(&id),
            Value::Text(ship_no),
            Value::from(&so_id),
            Value::from(&uuid(CUSTOMER)),
            Value::from(&uuid(WAREHOUSE)),
            Value::Text(status.to_string()),
            Value::Text(dec("10").to_string()),
            Value::Text(stock_out_id.map(|value| value.to_string()).unwrap_or_default()),
            Value::Text(format!("{TAG} 发货单 {status}")),
        ],
    )
    .await
    .expect("夹具：发货单写入失败");
    id
}

/// 造一张已过账的销售出库单（只做"单号存在"，三账与凭证由过账链路负责，别卡验）。
async fn create_posted_stock_out() -> (Uuid, String) {
    let id = Uuid::now_v7();
    let out_no = format!("OUT-FIX-{}", suffix(id));
    db().execute_with_params(
        "INSERT INTO biz_stock_out \
             (id, out_no, out_type, warehouse_id, out_date, status, total_qty, total_amount, remark) \
         VALUES (?::uuid, ?, 'ISSUE', ?::uuid, CURRENT_DATE, 'POSTED', NULLIF(?::text,'')::numeric, 0, ?)",
        vec![
            Value::from(&id),
            Value::Text(out_no.clone()),
            Value::from(&uuid(WAREHOUSE)),
            Value::Text(dec("10").to_string()),
            Value::Text(format!("{TAG} 出库单")),
        ],
    )
    .await
    .expect("夹具：销售出库单写入失败");
    (id, out_no)
}

/// 常量 id → `Uuid`（夹具常量都是字符串，避免 40 处 `parse().unwrap()`）。
fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具常量必须是合法 UUID")
}

/// 分页取本卡夹具的列表行（按 `so_id` 精确筛）。
async fn page_rows(so_id: Uuid) -> Vec<shipment_handler::ShipmentRes> {
    let page = shipment_handler::page_shipment(ShipmentFilter {
        ship_no: None,
        status: None,
        so_id: Some(so_id.to_string()),
        customer_id: None,
        page: 1,
        page_size: 50,
    })
    .await
    .expect("page_shipment 应当成功");
    page.list
}

/// 详情（按 id 查，走 `get_shipment` = HTTP handler 的同一条路径）。
async fn detail(id: Uuid) -> shipment_handler::ShipmentRes {
    shipment_handler::get_shipment(&id.to_string())
        .await
        .expect("get_shipment 应当成功")
}

// ==================== 用例 ====================

/// 列表与详情**逐字相等**：`so_no` 恒有值、未过账行 `stock_out_no` 为 `null`、
/// 已过账行 `stock_out_no` 与详情一致 —— 这就是本卡修的缺陷本身。
#[tokio::test]
async fn page_row_carries_so_no_and_stock_out_no_like_detail() {
    let Some(_guard) = setup().await else {
        return;
    };
    let (so_id, so_no, _) = create_approved_order().await;
    let (out_id, out_no) = create_posted_stock_out().await;
    let draft_id = create_shipment(so_id, "DRAFT", None).await;
    let posted_id = create_shipment(so_id, "POSTED", Some(out_id)).await;

    let rows = page_rows(so_id).await;
    assert_eq!(rows.len(), 2, "本卡的两张发货单都应出现在列表里");
    let draft_row = rows
        .iter()
        .find(|row| row.id == draft_id.to_string())
        .expect("草稿行应在列表里");
    let posted_row = rows
        .iter()
        .find(|row| row.id == posted_id.to_string())
        .expect("已过账行应在列表里");

    // ① so_no：两行都有值，且与订单号逐字相等
    assert_eq!(
        draft_row.so_no.as_deref(),
        Some(so_no.as_str()),
        "列表行 so_no 必须下发（缺陷原状：None）"
    );
    assert_eq!(
        posted_row.so_no.as_deref(),
        Some(so_no.as_str()),
        "已过账行的列表 so_no 也必须下发"
    );

    // ② stock_out_no：未过账行没有出库单 ⇒ null（不编造）；已过账行 = 出库单号
    assert_eq!(
        draft_row.stock_out_no, None,
        "未过账行没有 stock_out_id ⇒ 列表 stock_out_no 必须是 null"
    );
    assert_eq!(
        posted_row.stock_out_no.as_deref(),
        Some(out_no.as_str()),
        "已过账行的列表 stock_out_no 必须下发"
    );

    // ③ 与详情逐字相等（列表 / 详情同口径，前端不必为每一行再发一次详情请求）
    let draft_detail = detail(draft_id).await;
    let posted_detail = detail(posted_id).await;
    assert_eq!(draft_row.so_no, draft_detail.so_no, "列表与详情的 so_no 必须逐字相等");
    assert_eq!(posted_row.so_no, posted_detail.so_no, "列表与详情的 so_no 必须逐字相等");
    assert_eq!(
        draft_row.stock_out_no, draft_detail.stock_out_no,
        "列表与详情的 stock_out_no 必须逐字相等"
    );
    assert_eq!(
        posted_row.stock_out_no, posted_detail.stock_out_no,
        "列表与详情的 stock_out_no 必须逐字相等"
    );
    assert_eq!(
        posted_detail.stock_out_no.as_deref(),
        Some(out_no.as_str()),
        "详情本身就是这次对照的基准"
    );

    // ④ 客户 / 仓库编码的既有补齐不能被这次改动碰掉
    assert_eq!(
        draft_row.customer_code.as_deref(),
        Some("SO09-CUS"),
        "列表行客户编码（既有行为）"
    );
    assert_eq!(
        draft_row.warehouse_code.as_deref(),
        Some("SO09-WH"),
        "列表行仓库编码（既有行为）"
    );

    // ⑤ 列表行明细恒为空（明细只在详情里），顺便确认这次没把 items 塞进来
    assert!(
        draft_row.items.is_empty(),
        "列表行 items 必须恒为空数组（明细只在详情）"
    );
    assert_eq!(draft_detail.items.len(), 0, "夹具没写明细行，详情也应为空");

    cleanup().await;
}

/// 订单 / 出库单**软删**后，列表与详情**同时**变 `null`。
///
/// 这是 `so_no_of` / `stock_out_no_of` 补 `deleted_at IS NULL` 的护栏：
/// 单条与批量两个入口若判据不一致，就会退回本卡的病灶形态 ——
/// 「同一张发货单，列表说没单号、详情说有」。
#[tokio::test]
async fn soft_deleted_order_or_stock_out_is_null_on_both_list_and_detail() {
    let Some(_guard) = setup().await else {
        return;
    };
    let (so_id, so_no, _) = create_approved_order().await;
    let (out_id, out_no) = create_posted_stock_out().await;
    let posted_id = create_shipment(so_id, "POSTED", Some(out_id)).await;

    // 基线：都在
    assert_eq!(page_rows(so_id).await[0].so_no.as_deref(), Some(so_no.as_str()));
    assert_eq!(detail(posted_id).await.stock_out_no.as_deref(), Some(out_no.as_str()));

    // 订单软删 ⇒ 列表与详情同时 null
    db().execute_with_params(
        "UPDATE biz_sales_order SET deleted_at = now() WHERE id = ?::uuid",
        vec![Value::from(&so_id)],
    )
    .await
    .expect("软删订单");
    let row = page_rows(so_id).await;
    assert_eq!(
        row[0].so_no, None,
        "订单软删后列表 so_no 必须是 null（按 deleted_at 判，不按物理删判）"
    );
    assert_eq!(
        detail(posted_id).await.so_no,
        None,
        "订单软删后详情 so_no 必须也是 null（列表 / 详情同判据）"
    );

    // 恢复订单，改软删出库单 ⇒ stock_out_no 列表与详情同时 null
    db().execute_with_params(
        "UPDATE biz_sales_order SET deleted_at = NULL WHERE id = ?::uuid",
        vec![Value::from(&so_id)],
    )
    .await
    .expect("恢复订单");
    db().execute_with_params(
        "UPDATE biz_stock_out SET deleted_at = now() WHERE id = ?::uuid",
        vec![Value::from(&out_id)],
    )
    .await
    .expect("软删出库单");
    assert_eq!(
        page_rows(so_id).await[0].stock_out_no,
        None,
        "出库单软删后列表 stock_out_no 必须是 null"
    );
    assert_eq!(
        detail(posted_id).await.stock_out_no,
        None,
        "出库单软删后详情 stock_out_no 必须也是 null"
    );

    cleanup().await;
}

/// 批量函数：去重（一页多张单常常同一订单 / 同一出库单）+ 空入参短路 + 软删不出现。
#[tokio::test]
async fn batch_labels_dedupe_and_skip_soft_deleted() {
    let Some(_guard) = setup().await else {
        return;
    };
    // 空入参：不发 SQL、返回空
    assert!(
        shipment_model::so_no_labels(db(), &[]).await.unwrap().is_empty(),
        "空入参必须短路返回空 vec"
    );
    assert!(
        shipment_model::stock_out_no_labels(db(), &[]).await.unwrap().is_empty(),
        "空入参必须短路返回空 vec"
    );

    let (so_id, so_no, _) = create_approved_order().await;
    let (out_id, out_no) = create_posted_stock_out().await;

    let repeated = vec![so_id, so_id, so_id];
    let labels = shipment_model::so_no_labels(db(), &repeated)
        .await
        .expect("批量取订单号");
    assert_eq!(labels.len(), 1, "重复 id 必须去重成 1 行");
    assert_eq!(labels[0], (so_id, so_no.clone()));

    let outs = shipment_model::stock_out_no_labels(db(), &[out_id, out_id])
        .await
        .expect("批量取出库单号");
    assert_eq!(outs.len(), 1, "重复 id 必须去重成 1 行");
    assert_eq!(outs[0], (out_id, out_no.clone()));

    // 软删后批量查不到（调用方按缺失处理 = None）
    db().execute_with_params(
        "UPDATE biz_sales_order SET deleted_at = now() WHERE id = ?::uuid",
        vec![Value::from(&so_id)],
    )
    .await
    .expect("软删订单");
    assert!(
        shipment_model::so_no_labels(db(), &[so_id])
            .await
            .expect("批量取订单号")
            .is_empty(),
        "软删订单不应出现在批量结果里"
    );

    // 单条与批量判据一致（同一张单，两个入口都不能给值）
    assert_eq!(
        shipment_model::so_no_of(db(), so_id).await.expect("单条取订单号"),
        None,
        "单条入口与批量入口的判据必须一致（都带 deleted_at IS NULL）"
    );

    cleanup().await;
}
