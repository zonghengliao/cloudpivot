// ============================================================================
// model_tests.rs —— 测试部件：`erp/model` 的全部用例（纯函数口径 + 真库分页/取数口径）
//
// 由 `model.rs` 的 `#[cfg(test)] mod tests` 原样搬出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有用例体、夹具、SQL、断言与拆分前一致，只是换了落点 —— 用例数 26 → 26、用例名逐一相同。
// 挂载点写在 facade `model.rs` 末尾（`#[cfg(test)] #[path] mod tests;`），
// 因为只有在 facade 之下 `use super::*` 才指向「重导出后的公开 API」。
// 本文件自带完整 import 头：facade 的 `pub use` 只搬运**项**，不搬运私有 `use`（见方法论 §3 坑 1）。
// ============================================================================

// 拆分前本测试模块是 `model.rs` 里的 `mod tests { use super::*; }`：那时
// `use super::*` 同时带进来 (a) 父模块的公开项 与 (b) 父模块自己的私有 `use`。
// 现在测试独立成文件挂到 facade 下，(b) 不再传递，因此这里把父模块原有的
// import 头**原样补回**（一行不少、一行不多，口子与拆分前等价）。
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::dto::{
    AccountFilter, InventoryFilter, InventoryLocationFilter, InventoryTxnFilter, LocationFilter, MaterialFilter,
    PeriodFilter, PurchaseOrderFilter, PurchaseOrderItemFilter, SeqFilter, SupplierFilter, UnitFilter, WarehouseFilter,
};

use super::*;

// `use nik::prelude::*` 也导出同名 `DEFAULT_PAGE_SIZE`（框架 10）与 `MAX_PAGE_SIZE`（框架 1000）。
// 拆分前测试模块嵌在 `model.rs` 内部，`use super::*` 拿到的是**本模块自己的**常量（20 / 200），
// 局部项优先于 glob；拆成独立文件后两条 glob 同名，解析会落到框架那份，断言就变成
// 「20 == 10」。这里显式钉死回 ERP model 的口径（与拆分前逐字等价，不改断言）。
use crate::business::erp::model::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};

/// 夹具 UUID：`b1b0_00NN-0000-4000-8000-0000000000NN` 形态，段号见常量后缀。
const U_ID: &str = "b1b00001-0000-4000-8000-000000000001";
const U_BOX_ID: &str = "b1b00001-0000-4000-8000-000000000002";
const W_ROOT_ID: &str = "b1b00002-0000-4000-8000-000000000001";
const W_SUB_ID: &str = "b1b00002-0000-4000-8000-000000000002";
const W_LEAF_ID: &str = "b1b00002-0000-4000-8000-000000000003";
const W_OTHER_ID: &str = "b1b00002-0000-4000-8000-000000000004";
const L_A1_ID: &str = "b1b00003-0000-4000-8000-000000000001";
const L_A2_ID: &str = "b1b00003-0000-4000-8000-000000000002";
const L_B1_ID: &str = "b1b00003-0000-4000-8000-000000000003";
const M_RAW_ID: &str = "b1b00004-0000-4000-8000-000000000001";
const M_SEMI_ID: &str = "b1b00004-0000-4000-8000-000000000002";
const M_FIN_ID: &str = "b1b00004-0000-4000-8000-000000000003";
const SUPPLIER_ID: &str = "b1b00005-0000-4000-8000-000000000001";
const ACC_ROOT_ID: &str = "b1b00006-0000-4000-8000-000000000001";
const ACC_CHILD_ID: &str = "b1b00006-0000-4000-8000-000000000002";
const PERIOD_ID: &str = "b1b00007-0000-4000-8000-000000000001";
const SEQ_ID: &str = "b1b00008-0000-4000-8000-000000000001";
const PO_ID: &str = "b1b00009-0000-4000-8000-000000000001";
const PO_ITEM_ID: &str = "b1b0000a-0000-4000-8000-000000000001";
const INV_ID: &str = "b1b0000b-0000-4000-8000-000000000001";
const INV_LOC_A1_ID: &str = "b1b0000c-0000-4000-8000-000000000001";
const INV_LOC_A2_ID: &str = "b1b0000c-0000-4000-8000-000000000002";
const TXN_ID: &str = "b1b0000d-0000-4000-8000-000000000001";

/// 夹具 SQL：13 张表各若干行（金额一律用字符串，钉住 `NUMERIC(18,4)` 的标度）。
const FIXTURES: &[&str] = &[
    // 单位：1 基础 + 1 折算（12 个/箱）
    "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
     VALUES ('b1b00001-0000-4000-8000-000000000001', 'W1B-PCS', '个', 1, NULL, 1.000000, 1, 1),
            ('b1b00001-0000-4000-8000-000000000002', 'W1B-BOX', '箱', 0,
             'b1b00001-0000-4000-8000-000000000001', 12.000000, 1, 2)
     ON CONFLICT DO NOTHING",
    // 仓库：三层 + 另一棵树的顶级仓
    "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, allow_negative,
                                status, sort_order, parent_id, level, warehouse_path, is_leaf)
     VALUES ('b1b00002-0000-4000-8000-000000000001', 'WH-W1B-ROOT', 'W1B 总仓', 'NORMAL', 0, 1, 1,
             NULL, 1, '/b1b00002-0000-4000-8000-000000000001/', 0),
            ('b1b00002-0000-4000-8000-000000000002', 'WH-W1B-SUB', 'W1B 分仓', 'RAW', 0, 1, 1,
             'b1b00002-0000-4000-8000-000000000001', 2,
             '/b1b00002-0000-4000-8000-000000000001/b1b00002-0000-4000-8000-000000000002/', 0),
            ('b1b00002-0000-4000-8000-000000000003', 'WH-W1B-LEAF', 'W1B 库房', 'FINISHED', 0, 1, 1,
             'b1b00002-0000-4000-8000-000000000002', 3,
             '/b1b00002-0000-4000-8000-000000000001/b1b00002-0000-4000-8000-000000000002/b1b00002-0000-4000-8000-000000000003/', 1),
            ('b1b00002-0000-4000-8000-000000000004', 'WH-W1B-OTHER', 'W1B 另一个总仓', 'NORMAL', 0, 1, 2,
             NULL, 1, '/b1b00002-0000-4000-8000-000000000004/', 1)
     ON CONFLICT DO NOTHING",
    // 库位：库房 2 个（A-01 默认）+ 分仓 1 个
    "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                               is_default, capacity, status, sort_order)
     VALUES ('b1b00003-0000-4000-8000-000000000001', 'A-01', 'A 区一号', 'b1b00002-0000-4000-8000-000000000003',
             'STORAGE', 1, 100.0000, 1, 1),
            ('b1b00003-0000-4000-8000-000000000002', 'A-02', 'A 区二号', 'b1b00002-0000-4000-8000-000000000003',
             'PICKING', 0, NULL, 1, 2),
            ('b1b00003-0000-4000-8000-000000000003', 'B-01', 'B 区一号', 'b1b00002-0000-4000-8000-000000000002',
             'STORAGE', 1, NULL, 1, 1)
     ON CONFLICT DO NOTHING",
    // 物料：3 个（RAW / SEMI / FINISHED），编码前缀 W1B-
    "INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                               default_warehouse_id, safety_stock, costing_method, tax_rate, status)
     VALUES ('b1b00004-0000-4000-8000-000000000001', 'W1B-M-001', 'W1B 螺栓', 'M8x20',
             'b1b00001-0000-4000-8000-000000000001', 'RAW', 'b1b00002-0000-4000-8000-000000000003',
             10.5000, 'MOVING_AVG', 0.1300, 1),
            ('b1b00004-0000-4000-8000-000000000002', 'W1B-M-002', 'W1B 半成品', NULL,
             'b1b00001-0000-4000-8000-000000000001', 'SEMI', NULL, 0.0000, 'MOVING_AVG', 0.1300, 1),
            ('b1b00004-0000-4000-8000-000000000003', 'W1B-M-003', 'W1B 成品', NULL,
             'b1b00001-0000-4000-8000-000000000002', 'FINISHED', NULL, 0.0000, 'MOVING_AVG', 0.1300, 1)
     ON CONFLICT DO NOTHING",
    // 供应商（信用额度 = 12345.6789，钉 NUMERIC(18,4)）
    "INSERT INTO biz_supplier (id, supplier_code, supplier_name, short_name, contact_person,
                               credit_days, credit_limit, status)
     VALUES ('b1b00005-0000-4000-8000-000000000001', 'W1B-SUP-001', 'W1B 供应商', 'W1B 供应',
             '张三', 30, 12345.6789, 1)
     ON CONFLICT DO NOTHING",
    // 科目：一级 + 二级
    "INSERT INTO biz_account (id, account_code, account_name, parent_id, level, account_category,
                              balance_direction, is_leaf, status)
     VALUES ('b1b00006-0000-4000-8000-000000000001', 'W1B1001', 'W1B 库存现金', NULL, 1, 'ASSET', 'DEBIT', 0, 1),
            ('b1b00006-0000-4000-8000-000000000002', 'W1B100101', 'W1B 人民币现金',
             'b1b00006-0000-4000-8000-000000000001', 2, 'ASSET', 'DEBIT', 1, 1)
     ON CONFLICT DO NOTHING",
    // 自愈：早先的夹具版本用 1001 / 100101 当科目编码（与种子撞车后被跳过），
    // 重跑时把这两行纠正成 W1B 前缀，保证断言码与库里一致（幂等）。
    "UPDATE biz_account SET account_code = 'W1B1001'
      WHERE id = 'b1b00006-0000-4000-8000-000000000001' AND account_code <> 'W1B1001'",
    "UPDATE biz_account SET account_code = 'W1B100101'
      WHERE id = 'b1b00006-0000-4000-8000-000000000002' AND account_code <> 'W1B100101'",
    // 会计期间
    "INSERT INTO biz_accounting_period (id, period_year, period_month, period_code, start_date,
                                        end_date, status)
     VALUES ('b1b00007-0000-4000-8000-000000000001', 2025, 12, '2025-12', '2025-12-01',
             '2025-12-31', 'OPEN')
     ON CONFLICT DO NOTHING",
    // 单号序列（当前序号 7，步长 1）
    "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no,
                          step, separator, status)
     VALUES ('b1b00008-0000-4000-8000-000000000001', 'W1B-PO', 'W1B 采购订单号', 'W1B', 'yyyyMMdd',
             '20261030', 7, 1, '-', 1)
     ON CONFLICT DO NOTHING",
    // 采购订单（含税 = 不含税 + 税，DB 约束会校验）
    "INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, expect_date,
                                     warehouse_id, status, currency_code, total_qty, total_amount,
                                     total_tax, total_with_tax, credit_days)
     VALUES ('b1b00009-0000-4000-8000-000000000001', 'W1B-PO-0001',
             'b1b00005-0000-4000-8000-000000000001', '2026-10-29', '2026-11-05',
             'b1b00002-0000-4000-8000-000000000003', 'DRAFT', 'CNY', 5.0000, 100.0000, 13.0000,
             113.0000, 30)
     ON CONFLICT DO NOTHING",
    // 采购明细（金额 100 + 税 13 = 113）
    "INSERT INTO biz_purchase_order_item (id, order_id, line_no, material_id, unit_id, qty, price,
                                          tax_rate, amount, tax_amount, with_tax_amount)
     VALUES ('b1b0000a-0000-4000-8000-000000000001', 'b1b00009-0000-4000-8000-000000000001', 1,
             'b1b00004-0000-4000-8000-000000000001', 'b1b00001-0000-4000-8000-000000000001',
             5.0000, 20.000000, 0.1300, 100.0000, 13.0000, 113.0000)
     ON CONFLICT DO NOTHING",
    // 成本账：5 个、均价 20、金额 100
    "INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount)
     VALUES ('b1b0000b-0000-4000-8000-000000000001', 'b1b00004-0000-4000-8000-000000000001',
             'b1b00002-0000-4000-8000-000000000003', 5.0000, 0.0000, 20.000000, 100.0000)
     ON CONFLICT DO NOTHING",
    // 数量账：A-01 放 3、A-02 放 2（合计 = 成本账 5）
    "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty, locked_qty)
     VALUES ('b1b0000c-0000-4000-8000-000000000001', 'b1b00004-0000-4000-8000-000000000001',
             'b1b00002-0000-4000-8000-000000000003', 'b1b00003-0000-4000-8000-000000000001',
             3.0000, 0.0000),
            ('b1b0000c-0000-4000-8000-000000000002', 'b1b00004-0000-4000-8000-000000000001',
             'b1b00002-0000-4000-8000-000000000003', 'b1b00003-0000-4000-8000-000000000002',
             2.0000, 0.0000)
     ON CONFLICT DO NOTHING",
    // 流水：入库 5 个
    "INSERT INTO biz_inventory_txn (id, material_id, warehouse_id, txn_type, biz_type, doc_type,
                                    doc_id, doc_no, line_no, qty, unit_cost, amount, before_qty,
                                    before_amount, before_cost, after_qty, after_amount, after_cost,
                                    period_code)
     VALUES ('b1b0000d-0000-4000-8000-000000000001', 'b1b00004-0000-4000-8000-000000000001',
             'b1b00002-0000-4000-8000-000000000003', 'IN', 'PURCHASE', 'PURCHASE_ORDER',
             'b1b00009-0000-4000-8000-000000000001', 'W1B-PO-0001', 1, 5.0000, 20.000000, 100.0000,
             0.0000, 0.0000, 0.000000, 5.0000, 100.0000, 20.000000, '2026-10')
     ON CONFLICT DO NOTHING",
];

// 真库用例**串行**执行：36 个用例共用一个库，而
// `material_page_serial_required_filter_tracks_column_existence` 会 `ALTER TABLE`
// （ACCESS EXCLUSIVE）——并行跑时其它用例的查询会被挂住甚至被断开
// （实测 `query failed … : connection closed`）。锁的粒度 = 一个用例，时间在毫秒级。
//
// 锁与 `cash_flow` 的全表断言共用同一把（`voucher_query::tests::DB_LOCK`）：本模块夹具
// 在 `biz_account` 里造了 `W1B1001` / `W1B100101`（无现金流量分类行），而
// `cash_flow::tests::r3_real_config_covers_all_accounts` 遍历 `biz_account` 全表断言
// 「每个非现金科目都有分类行」—— 两把不同的锁时并行必撞（本模块只在 `zz_` 收尾用例里删，
// 多线程下不保证 `zz_` 最后跑）。
use crate::business::erp::voucher_query::tests::DB_LOCK;

/// 初始化全局数据库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`，用例直接跳过。
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
            // 每个 `#[tokio::test]` 都是**独立的 tokio runtime**，而 `db()` 是进程级全局池：
            // 池里空闲的连接是绑在「上一个 runtime」上的（`tokio::spawn(connection)`），
            // runtime 一结束那条连接就死了，但 `client.is_closed()` 不会立刻变 true
            // ⇒ 下一次借出会拿到死连接，报 `query failed … : connection closed`（实测 36 条里挂 5–7 条）。
            // 对策：把 `max_lifetime` 设成 0，让池每次都丢掉空闲连接、按需新建
            // （`PostgresPool::get` 的 `pop_available` 只认 is_available → 只剩新建一条路）。
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

/// 拿锁 + 连库 + 建夹具（幂等）。返回 `None` = 没配 `ERP_DB_URL`，用例应直接返回。
///
/// 返回的 guard 必须活到用例结束（`let _db = ...`），否则锁会被提前释放。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    for sql in FIXTURES {
        if let Err(err) = db().execute_with_params(sql, vec![]).await {
            panic!("夹具写入失败：{err}\nSQL: {sql}");
        }
    }
    Some(guard)
}

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

fn dec(text: &str) -> Decimal {
    use std::str::FromStr;
    Decimal::from_str(text).expect("测试金额字面量非法")
}

// ---------- ① 13 张表的 Row 字段映射 ----------

#[tokio::test]
async fn unit_row_maps_key_field() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row = unit_find_by_id(uuid(U_ID)).await.unwrap().expect("单位应存在");
    assert_eq!(row.unit_code, "W1B-PCS");
    assert_eq!(row.is_base, 1);
    assert!(row.base_unit_id.is_none());
    let boxed = unit_find_by_id(uuid(U_BOX_ID)).await.unwrap().unwrap();
    assert_eq!(boxed.base_unit_id, Some(uuid(U_ID)));
    assert_eq!(boxed.rate, dec("12"));
}

#[tokio::test]
async fn warehouse_row_maps_level_and_path() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let leaf = warehouse_find_by_id(uuid(W_LEAF_ID)).await.unwrap().unwrap();
    assert_eq!(leaf.level, 3);
    assert_eq!(leaf.is_leaf, 1);
    assert_eq!(leaf.warehouse_type, "FINISHED");
    assert_eq!(leaf.warehouse_path, format!("/{W_ROOT_ID}/{W_SUB_ID}/{W_LEAF_ID}/"));
    assert_eq!(leaf.parent_id, Some(uuid(W_SUB_ID)));
}

#[tokio::test]
async fn location_row_maps_warehouse_and_default() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row = location_find_by_id(uuid(L_A1_ID)).await.unwrap().unwrap();
    assert_eq!(row.location_code, "A-01");
    assert_eq!(row.warehouse_id, uuid(W_LEAF_ID));
    assert_eq!(row.is_default, 1);
    assert_eq!(row.location_type, "STORAGE");
    assert_eq!(row.capacity, Some(dec("100")));

    // 按仓库查库位：库房 2 个（sort_order 升序），分仓 1 个 → L_A2 / L_B1 都用到
    let leaf_locations = location_by_warehouse(uuid(W_LEAF_ID), false).await.unwrap();
    assert_eq!(leaf_locations.len(), 2);
    assert_eq!(leaf_locations[1].id, uuid(L_A2_ID));
    let sub_locations = location_by_warehouse(uuid(W_SUB_ID), false).await.unwrap();
    assert_eq!(sub_locations.len(), 1);
    assert_eq!(sub_locations[0].id, uuid(L_B1_ID));
}

#[tokio::test]
async fn material_row_maps_type_and_tax() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row = material_find_by_code("W1B-M-001").await.unwrap().expect("物料应存在");
    assert_eq!(row.material_type, "RAW");
    assert_eq!(row.costing_method, "MOVING_AVG");
    assert_eq!(row.tax_rate, dec("0.13"));
    assert_eq!(row.safety_stock, dec("10.5"));
    assert_eq!(row.default_warehouse_id, Some(uuid(W_LEAF_ID)));
}

#[tokio::test]
async fn supplier_row_maps_credit_limit() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row = supplier_find_by_id(uuid(SUPPLIER_ID)).await.unwrap().unwrap();
    assert_eq!(row.supplier_code, "W1B-SUP-001");
    assert_eq!(row.credit_days, 30);
    assert_eq!(row.credit_limit, dec("12345.6789"));
}

#[tokio::test]
async fn account_row_maps_direction_and_leaf() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let child = account_find_by_code("W1B100101").await.unwrap().unwrap();
    assert_eq!(child.account_category, "ASSET");
    assert_eq!(child.balance_direction, "DEBIT");
    assert_eq!(child.is_leaf, 1);
    assert_eq!(child.level, 2);
    assert_eq!(child.parent_id, Some(uuid(ACC_ROOT_ID)));
    let children = account_children(uuid(ACC_ROOT_ID)).await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, uuid(ACC_CHILD_ID));
}

#[tokio::test]
async fn period_row_maps_code_and_status() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row = period_find_by_code("2025-12").await.unwrap().unwrap();
    assert_eq!(row.id, uuid(PERIOD_ID));
    assert_eq!(row.period_year, 2025);
    assert_eq!(row.period_month, 12);
    assert_eq!(row.status, "OPEN");
    assert_eq!(row.start_date.to_string(), "2025-12-01");
}

#[tokio::test]
async fn seq_row_maps_current_no_and_step() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row = seq_find_by_type("W1B-PO").await.unwrap().unwrap();
    assert_eq!(row.current_no, 7);
    assert_eq!(row.step, 1);
    assert_eq!(row.date_pattern, "yyyyMMdd");
    assert_eq!(row.prefix, "W1B");
}

#[tokio::test]
async fn purchase_order_row_maps_amounts_and_currency() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row = purchase_order_find_by_no("W1B-PO-0001").await.unwrap().unwrap();
    assert_eq!(row.status, "DRAFT");
    assert_eq!(row.currency_code, "CNY", "CHAR(3) 已 ::text，不带补位空格");
    assert_eq!(row.total_qty, dec("5"));
    assert_eq!(row.total_amount, dec("100"));
    assert_eq!(row.total_tax, dec("13"));
    assert_eq!(row.total_with_tax, dec("113"));
}

#[tokio::test]
async fn purchase_order_item_row_maps_qty_and_received() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let items = purchase_order_items(uuid(PO_ID)).await.unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item.id, uuid(PO_ITEM_ID));
    assert_eq!(item.line_no, 1);
    assert_eq!(item.qty, dec("5"));
    assert_eq!(item.price, dec("20"), "单价 NUMERIC(18,6) 读回 20");
    assert_eq!(
        item.received_qty,
        Decimal::ZERO,
        "P2-3：已收数量是派生值，没有已过账收货单就是 0"
    );
    assert_eq!(item.with_tax_amount, item.amount + item.tax_amount);
}

#[tokio::test]
async fn inventory_row_maps_avg_cost() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row = inventory_find(uuid(M_RAW_ID), uuid(W_LEAF_ID))
        .await
        .unwrap()
        .expect("库存应存在");
    assert_eq!(row.id, uuid(INV_ID));
    assert_eq!(row.qty, dec("5"));
    assert_eq!(row.avg_cost, dec("20"), "均价 NUMERIC(18,6)");
    assert_eq!(row.amount, dec("100"));
    let missing = inventory_find(uuid(M_SEMI_ID), uuid(W_LEAF_ID)).await.unwrap();
    assert!(missing.is_none(), "没库存的物料返回 None，而不是空行");
}

#[tokio::test]
async fn inventory_location_row_maps_qty_per_location() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let rows = inventory_locations(uuid(M_RAW_ID), uuid(W_LEAF_ID)).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, uuid(INV_LOC_A1_ID));
    assert_eq!(rows[0].location_id, Some(uuid(L_A1_ID)));
    assert_eq!(rows[0].qty, dec("3"));
    assert_eq!(rows[1].id, uuid(INV_LOC_A2_ID));
    assert_eq!(rows[1].qty, dec("2"));
}

#[tokio::test]
async fn inventory_txn_row_maps_direction_and_qty() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let filter = InventoryTxnFilter {
        doc_no: Some("W1B-PO-0001".to_string()),
        ..Default::default()
    };
    let (rows, total) = inventory_txn_page(&filter).await.unwrap();
    assert_eq!(total, 1);
    let txn = &rows[0];
    assert_eq!(txn.id, uuid(TXN_ID));
    assert_eq!(txn.txn_type, "IN");
    assert_eq!(txn.qty, dec("5"));
    assert_eq!(txn.before_qty, Decimal::ZERO);
    assert_eq!(txn.after_qty, dec("5"));
    assert_eq!(txn.period_code, "2026-10");
}

// ---------- ② 仓库树（3 层，物化路径前缀） ----------

#[tokio::test]
async fn warehouse_tree_returns_three_levels_in_path_order() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let tree = warehouse_tree(Some(uuid(W_ROOT_ID))).await.unwrap();
    let codes: Vec<&str> = tree.iter().map(|w| w.warehouse_code.as_str()).collect();
    assert_eq!(
        codes,
        vec!["WH-W1B-ROOT", "WH-W1B-SUB", "WH-W1B-LEAF"],
        "子树 = 自身 + 后代，按物化路径先序"
    );
    assert_eq!(tree[0].level, 1);
    assert_eq!(tree[1].level, 2);
    assert_eq!(tree[2].level, 3);
}

#[tokio::test]
async fn warehouse_subtree_excludes_sibling_root() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let tree = warehouse_tree(Some(uuid(W_SUB_ID))).await.unwrap();
    assert_eq!(tree.len(), 2, "分仓子树 = 分仓 + 库房");
    assert!(
        tree.iter().all(|w| w.id != uuid(W_OTHER_ID)),
        "另一个总仓不能出现在分仓的子树里"
    );
    let leaf_only = warehouse_tree(Some(uuid(W_LEAF_ID))).await.unwrap();
    assert_eq!(leaf_only.len(), 1, "叶子仓库子树只有自己");
    assert_eq!(leaf_only[0].id, uuid(W_LEAF_ID));
    let missing = warehouse_tree(Some(Uuid::now_v7())).await.unwrap();
    assert!(missing.is_empty(), "根不存在 → 空子树（不报错）");
}

#[tokio::test]
async fn warehouse_child_counts_and_children() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let counts = warehouse_child_counts(&[uuid(W_ROOT_ID), uuid(W_SUB_ID), uuid(W_OTHER_ID)])
        .await
        .unwrap();
    assert_eq!(counts.get(&uuid(W_ROOT_ID)), Some(&1));
    assert_eq!(counts.get(&uuid(W_SUB_ID)), Some(&1));
    assert_eq!(counts.get(&uuid(W_OTHER_ID)), None, "无子节点不出现在 map 里");
    let children = warehouse_children(uuid(W_ROOT_ID)).await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, uuid(W_SUB_ID));
}

#[test]
fn path_prefix_upper_bounds_the_subtree() {
    assert_eq!(path_prefix_upper("/a/b/"), "/a/b0");
    assert_eq!(path_prefix_upper("/a/b"), "/a/b0");
    // 前缀区间语义自证：所有后代落在 [lo, hi) 内，另一个顶级仓在外面
    let lo = "/root/";
    let hi = path_prefix_upper(lo);
    let descendant = format!("{lo}sub/");
    assert!(descendant.as_str() >= lo && descendant.as_str() < hi.as_str());
    let sibling = "/other/";
    assert!(!(sibling >= lo && sibling < hi.as_str()));
}

// ---------- ③ 物料分页 + 一物一码筛选 ----------

#[tokio::test]
async fn material_page_filters_and_pages() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let filter = MaterialFilter {
        keyword: Some("W1B-M-".to_string()),
        page: 2,
        page_size: 2,
        ..Default::default()
    };
    let (rows, total) = material_page(&filter).await.unwrap();
    assert_eq!(total, 3, "W1B- 前缀共 3 个物料");
    assert_eq!(rows.len(), 1, "第 2 页只剩 1 条");
    assert_eq!(rows[0].material_code, "W1B-M-003");

    let only_semi = MaterialFilter {
        material_type: Some("SEMI".to_string()),
        keyword: Some("W1B-".to_string()),
        ..Default::default()
    };
    let (rows, total) = material_page(&only_semi).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows[0].id, uuid(M_SEMI_ID));
}

#[tokio::test]
async fn material_page_serial_required_filter_tracks_column_existence() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let had_column = material_has_serial_column().await.unwrap();
    if !had_column {
        // V0137 现状：列属 V0138 → 显式报错（不静默返回全量）
        let err = material_page(&MaterialFilter {
            serial_required: Some(true),
            ..Default::default()
        })
        .await
        .expect_err("列不存在时必须报错");
        assert!(
            matches!(&err, DbError::InvalidValue(message) if message.contains("serial_required")),
            "错误必须点名 serial_required，实际：{err:?}"
        );
        db().execute_with_params(
            "ALTER TABLE biz_material ADD COLUMN serial_required SMALLINT NOT NULL DEFAULT 0",
            vec![],
        )
        .await
        .expect("造列（模拟 V0138）");
        db().execute_with_params(
            "UPDATE biz_material SET serial_required = 1 \
             WHERE id = 'b1b00004-0000-4000-8000-000000000003'",
            vec![],
        )
        .await
        .expect("标记一物一码物料");
    } else {
        // V0138 已落地（列是 **BOOLEAN**）：幂等标记同一个物料，
        // 否则本用例在真实库上会筛出 0 行（W2b 实测：total=0 ≠ 1）
        db().execute_with_params(
            "UPDATE biz_material SET serial_required = TRUE \
             WHERE id = 'b1b00004-0000-4000-8000-000000000003'",
            vec![],
        )
        .await
        .expect("标记一物一码物料（BOOLEAN 列）");
    }

    let (rows, total) = material_page(&MaterialFilter {
        serial_required: Some(true),
        keyword: Some("W1B-".to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(total, 1, "只有 W1B-M-003 是一物一码");
    assert_eq!(rows[0].material_code, "W1B-M-003");

    let (_, not_serial) = material_page(&MaterialFilter {
        serial_required: Some(false),
        keyword: Some("W1B-".to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(not_serial, 2);

    if !had_column {
        // 恢复原样：不把「模拟 V0138」的列留在库里
        db().execute_with_params("ALTER TABLE biz_material DROP COLUMN serial_required", vec![])
            .await
            .expect("回滚造列");
    }
}

// ---------- ④ 库存两级账 ----------

#[tokio::test]
async fn inventory_two_level_ledger_is_consistent() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let balance = inventory_balance(uuid(M_RAW_ID), uuid(W_LEAF_ID))
        .await
        .unwrap()
        .expect("两级账应存在");
    assert_eq!(balance.inventory.qty, dec("5"), "成本账数量");
    assert_eq!(balance.locations.len(), 2, "数量账按库位拆成 2 行");
    assert_eq!(balance.location_qty_sum(), dec("5"), "库位数量之和 == 成本账数量");
    assert!(balance.is_consistent());

    let missing = inventory_balance(uuid(M_FIN_ID), uuid(W_LEAF_ID)).await.unwrap();
    assert!(missing.is_none(), "无库存记录 → None");
}

#[test]
fn inventory_balance_consistency_rules() {
    // 纯逻辑：没有库位行时，账面 0 视为一致、账面非 0 视为不一致
    let empty = InventoryBalance {
        inventory: fake_inventory_row(Decimal::ZERO),
        locations: Vec::new(),
    };
    assert!(empty.is_consistent());
    let broken = InventoryBalance {
        inventory: fake_inventory_row(dec("5")),
        locations: Vec::new(),
    };
    assert!(!broken.is_consistent(), "账面有数量却没有库位行 = 账实不符");
}

fn fake_inventory_row(qty: Decimal) -> InventoryRow {
    let now = DateTime::<Utc>::UNIX_EPOCH;
    InventoryRow {
        id: Uuid::now_v7(),
        material_id: Uuid::now_v7(),
        warehouse_id: Uuid::now_v7(),
        qty,
        locked_qty: Decimal::ZERO,
        avg_cost: Decimal::ZERO,
        amount: Decimal::ZERO,
        last_txn_at: None,
        created_at: now,
        updated_at: now,
    }
}

// ---------- ⑥ Decimal 精度（NUMERIC(18,4)） ----------

#[tokio::test]
async fn decimal_12345_6789_roundtrips_without_loss() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // 供应商信用额度列是 NUMERIC(18,4)：写 12345.6789 → 读回必须逐位一致
    let row = supplier_find_by_id(uuid(SUPPLIER_ID)).await.unwrap().unwrap();
    assert_eq!(row.credit_limit, dec("12345.6789"));
    assert_eq!(
        row.credit_limit.to_string(),
        "12345.6789",
        "标度不丢（不是 12345.6790）"
    );

    // 写入侧同样定点：更新成 0.0001（4 位小数的最小单位）再读回
    db().execute_with_params(
        "UPDATE biz_supplier SET credit_limit = ? WHERE id = ?",
        vec![TypeMapper::to_value(&dec("0.0001")), Value::from(uuid(SUPPLIER_ID))],
    )
    .await
    .expect("写入 0.0001");
    let after = supplier_find_by_id(uuid(SUPPLIER_ID)).await.unwrap().unwrap();
    assert_eq!(after.credit_limit, dec("0.0001"));
    // 复原，避免影响同库其它用例的断言
    db().execute_with_params(
        "UPDATE biz_supplier SET credit_limit = ? WHERE id = ?",
        vec![TypeMapper::to_value(&dec("12345.6789")), Value::from(uuid(SUPPLIER_ID))],
    )
    .await
    .expect("复原信用额度");
}

// ---------- ⑦ 分页口径 ----------

#[test]
fn normalize_page_clamps_bounds() {
    assert_eq!(normalize_page(0, 0), (1, DEFAULT_PAGE_SIZE));
    assert_eq!(normalize_page(3, 50), (3, 50));
    assert_eq!(normalize_page(1, 10_000), (1, MAX_PAGE_SIZE));
}

#[tokio::test]
async fn list_pages_respect_filters() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (units, unit_total) = unit_page(&UnitFilter {
        keyword: Some("W1B-".to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(unit_total, 2);
    assert!(units.iter().all(|u| u.unit_code.starts_with("W1B-")));

    let (locs, loc_total) = location_page(&LocationFilter {
        warehouse_id: Some(uuid(W_LEAF_ID).to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(loc_total, 2, "库房只有 2 个库位");
    assert_eq!(locs[0].location_code, "A-01", "按 sort_order 升序");

    let (roots, root_total) = warehouse_page(&WarehouseFilter {
        keyword: Some("W1B".to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(root_total, 4, "W1B 前缀共 4 个仓库");
    assert_eq!(roots.len(), 4, "一页装得下 4 个");

    let (pos, po_total) = purchase_order_page(&PurchaseOrderFilter {
        status: Some("DRAFT".to_string()),
        date_from: Some(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()),
        date_to: Some(NaiveDate::from_ymd_opt(2026, 10, 31).unwrap()),
        keyword: Some("W1B-PO".to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(po_total, 1);
    assert_eq!(pos[0].id, uuid(PO_ID));

    let (periods, period_total) = period_page(&PeriodFilter {
        period_year: Some(2025),
        ..Default::default()
    })
    .await
    .unwrap();
    assert!(period_total >= 1);
    assert!(periods.iter().any(|p| p.id == uuid(PERIOD_ID)));

    let (seqs, seq_total) = seq_page(&SeqFilter {
        keyword: Some("W1B".to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(seq_total, 1);
    assert_eq!(seqs[0].id, uuid(SEQ_ID));

    let (accounts, acc_total) = account_page(&AccountFilter {
        keyword: Some("W1B".to_string()),
        is_leaf: Some(true),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(acc_total, 1);
    assert_eq!(accounts[0].id, uuid(ACC_CHILD_ID));

    let (suppliers, sup_total) = supplier_page(&SupplierFilter {
        keyword: Some("W1B-SUP".to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(sup_total, 1);
    assert_eq!(suppliers[0].id, uuid(SUPPLIER_ID));

    let (inv_rows, inv_total) = inventory_page(&InventoryFilter {
        warehouse_id: Some(uuid(W_LEAF_ID).to_string()),
        only_nonzero: true,
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(inv_total, 1);
    assert_eq!(inv_rows[0].id, uuid(INV_ID));

    let (inv_locs, inv_loc_total) = inventory_location_page(&InventoryLocationFilter {
        warehouse_id: Some(uuid(W_LEAF_ID).to_string()),
        material_id: Some(uuid(M_RAW_ID).to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(inv_loc_total, 2);
    assert!(inv_locs.iter().all(|l| l.qty > Decimal::ZERO));

    let (items, item_total) = purchase_order_item_page(&PurchaseOrderItemFilter {
        order_id: Some(uuid(PO_ID).to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(item_total, 1);
    assert_eq!(items[0].id, uuid(PO_ITEM_ID));
}

/// ㉔ 收尾清场：本模块的**科目夹具**必须删掉（t_06c528fc）。
///
/// 为什么：`cash_flow::tests::r3_real_config_covers_all_accounts` 按「`biz_account` 全表」
/// 断言「每个非现金科目都要有现金流量分类行」。夹具科目（没有分类行）留在共享库里，
/// 全量套件**第二次**跑时就会变成「分类配置漏科目：W1B1001 / W1B100101 …」直接红
/// （与 P-FLOW-3B 那次 P3B 科目污染同源）。
///
/// `biz_account` 没有任何外键引用它（分录按 `account_code` 冗余存一份），删起来是安全的。
/// 名字 `zz_` → 排在本模块最后，等于模块级 teardown。
#[tokio::test]
async fn zz_teardown_purges_fixture_accounts() {
    let Some(_guard) = setup().await else {
        return;
    };
    db().execute_with_params("DELETE FROM biz_account WHERE account_code LIKE 'W1B%'", vec![])
        .await
        .expect("清场删夹具科目失败");
    let left: i64 = db()
        .scalar_sql(
            "SELECT count(*) FROM biz_account WHERE account_code LIKE 'W1B%'".to_string(),
            vec![],
        )
        .await
        .expect("计数失败");
    assert_eq!(left, 0, "夹具科目必须删掉（留着会让第二遍全量变红）");
}
