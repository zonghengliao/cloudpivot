// ============================================================================
// cancel_cascade_tests.rs —— R-P1-5 联动作废的真库单测
//
// 覆盖卡面要求的四类：
//   ① 影响分析准确性（列出收货单 / 凭证 / 应付单 + 金额 + 处理方式）
//   ② 级联执行顺序（先收货单、后采购单；返回的 `seq` 就是执行顺序）
//   ③ 中途失败整体回滚（第二步失败 → 第一步的作废也退回）
//   ④ 无下游时直接作废（只改采购单状态）
//
// 真库用例需要 `ERP_DB_URL`（未设置 → 全部 SKIP，与 `stockin_handler` 的用例同一开关）：
//
// ```bash
// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_p15'
// cargo test --lib p15_
// ```
//
// # 夹具隔离
//
// 本模块用**自己的 UUID 命名空间**（`c1c10001-…`）、自己的供应商 / 仓库 / 物料 /
// 单位，与 `stockin_handler`（`b2b10001-…`）、`transfer`（`b3b1…`）等模块完全不重叠：
// 别人的 `cleanup_fixtures()` 删不到我的行，我的 `cleanup()` 也删不到别人的行。
// 每轮 `setup()` 先按这个命名空间清场，重复跑不积累。
// ============================================================================

use std::str::FromStr;

use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cancel_cascade;
use crate::business::erp::purchase_model;
use crate::business::erp::stockin_dto::{CreateStockInReq, StockInItemReq, StockInStatusReq};
use crate::business::erp::stockin_handler;
use crate::business::erp::stockin_model;
use crate::business::shared::state_machine;

// ==================== 夹具常量（本模块独占的命名空间） ====================

const UNIT: &str = "c1c10001-0000-4000-8000-000000000001";
const SUPPLIER: &str = "c1c10001-0000-4000-8000-000000000002";
const WAREHOUSE: &str = "c1c10001-0000-4000-8000-000000000003";
const MATERIAL_A: &str = "c1c10001-0000-4000-8000-000000000011";
const MATERIAL_B: &str = "c1c10001-0000-4000-8000-000000000012";

/// 真库用例必须串行（共用全局 `db()` 与同一批夹具）。
static P15_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("测试金额字面量非法")
}

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

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

/// 清掉本模块命名空间下的全部痕迹（顺序写死：子表 → 主表）。
async fn cleanup() {
    let si_ids = format!("SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid");
    let po_ids =
        format!("SELECT id FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid");
    let payable_ids = format!("SELECT id FROM biz_payable WHERE source_id IN ({si_ids})");
    let voucher_ids = format!("SELECT id FROM biz_voucher WHERE source_id IN ({si_ids})");
    for sql in [
        // 状态日志无外键，按实体 id 清
        format!(
            "DELETE FROM biz_status_log WHERE entity_id IN ({si_ids}) \
                OR entity_id IN ({po_ids}) OR entity_id IN ({payable_ids})"
        ),
        // 审批痕迹（本模块的用例不建审批单，但复用收货单作废时可能留下）
        format!(
            "DELETE FROM biz_flow_task WHERE instance_id IN \
                (SELECT id FROM biz_flow_instance WHERE biz_id IN ({si_ids}) OR biz_id IN ({po_ids}))"
        ),
        format!("DELETE FROM biz_flow_instance WHERE biz_id IN ({si_ids}) OR biz_id IN ({po_ids})"),
        format!("DELETE FROM biz_voucher_entry WHERE voucher_id IN ({voucher_ids})"),
        format!("DELETE FROM biz_payment WHERE payable_id IN ({payable_ids})"),
        format!("DELETE FROM biz_voucher WHERE id IN ({voucher_ids})"),
        format!("DELETE FROM biz_payable WHERE id IN ({payable_ids})"),
        format!("DELETE FROM biz_inventory_txn WHERE doc_id IN ({si_ids})"),
        format!("DELETE FROM biz_stock_in_item WHERE stock_in_id IN ({si_ids})"),
        format!("DELETE FROM biz_stock_in WHERE id IN ({si_ids})"),
        format!("DELETE FROM biz_purchase_order_item WHERE order_id IN ({po_ids})"),
        format!("DELETE FROM biz_purchase_order WHERE id IN ({po_ids})"),
        // 库存行按物料清（本模块独占 MATERIAL_A / MATERIAL_B）
        format!(
            "DELETE FROM biz_inventory_location WHERE material_id IN ('{MATERIAL_A}'::uuid, '{MATERIAL_B}'::uuid)"
        ),
        format!(
            "DELETE FROM biz_inventory WHERE material_id IN ('{MATERIAL_A}'::uuid, '{MATERIAL_B}'::uuid)"
        ),
    ] {
        if let Err(err) = db().execute_with_params(&sql, vec![]).await {
            println!("cleanup 语句失败（继续）：{err} —— {sql}");
        }
    }
}

/// 建夹具主数据 + 清上一轮。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = P15_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    for sql in [
        format!(
            "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order) \
             VALUES ('{UNIT}'::uuid, 'P15-PCS', '个', 1, NULL, 1.000000, 1, 1) \
             ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) \
             VALUES ('{SUPPLIER}'::uuid, 'P15-SUP', 'P15 联动作废供应商', 1) \
             ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status) \
             VALUES ('{WAREHOUSE}'::uuid, 'P15-WH', 'P15 联动作废仓', \
                     '/c1c10001-0000-4000-8000-000000000003/', 1, 1, 1) \
             ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required) \
             VALUES ('{MATERIAL_A}'::uuid, 'P15-M1', 'P15 螺丝', '{UNIT}'::uuid, 1, false), \
                    ('{MATERIAL_B}'::uuid, 'P15-M2', 'P15 垫片', '{UNIT}'::uuid, 1, false) \
             ON CONFLICT DO NOTHING"
        ),
    ] {
        if let Err(err) = db().execute_with_params(&sql, vec![]).await {
            panic!("夹具写入失败：{err}\nSQL: {sql}");
        }
    }
    Some(guard)
}

// ==================== 造单 ====================

/// 造一张采购单：`lines` = `[(物料, 数量)]`，单价固定 2、税率 0，状态 `SUBMITTED`。
///
/// 返回 `(order_id, item_ids)`（`item_ids` 与 `lines` 同序）。
async fn create_po(lines: &[(&str, &str)]) -> (Uuid, Vec<Uuid>) {
    let order_id = Uuid::now_v7();
    // 单号取 uuid 的随机尾段（前 8 位是毫秒时间戳，同窗口多张单会撞 uk_biz_po_no）
    let order_no = format!("P15-{}", &order_id.simple().to_string()[24..32]);
    let total_qty: Decimal = lines.iter().map(|(_, qty)| dec(qty)).sum();
    let total_amount = total_qty * dec("2");
    db().execute_with_params(
        "INSERT INTO biz_purchase_order \
             (id, order_no, supplier_id, order_date, warehouse_id, status, total_qty, \
              total_amount, total_tax, total_with_tax) \
         VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, ?::uuid, 'SUBMITTED', \
             NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,'')::numeric)",
        vec![
            Value::from(&order_id),
            Value::Text(order_no),
            Value::from(&uuid(SUPPLIER)),
            Value::from(&uuid(WAREHOUSE)),
            Value::Text(total_qty.round_dp(4).to_string()),
            Value::Text(total_amount.round_dp(4).to_string()),
            Value::Text(total_amount.round_dp(4).to_string()),
        ],
    )
    .await
    .unwrap_or_else(|err| panic!("建采购单失败：{err}"));

    let mut item_ids = Vec::new();
    for (index, (material, qty)) in lines.iter().enumerate() {
        let item_id = Uuid::now_v7();
        let amount = dec(qty) * dec("2");
        // `line_no` 是整数列：`Value::Text` 绑过去会报 `error serializing parameter`，
        // 测试里直接把它拼进 SQL（只有内部生成的序号，没有注入面）
        db().execute_with_params(
            &format!(
                "INSERT INTO biz_purchase_order_item \
                     (id, order_id, line_no, material_id, unit_id, qty, price, price_incl, tax_rate, \
                      amount, tax_amount, with_tax_amount) \
                 VALUES (?::uuid, ?::uuid, {}, ?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, 0, \
                     NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,'')::numeric)",
                index + 1
            ),
            vec![
                Value::from(&item_id),
                Value::from(&order_id),
                Value::from(&uuid(material)),
                Value::from(&uuid(UNIT)),
                Value::Text(qty.to_string()),
                Value::Text("2".to_string()),
                Value::Text("2".to_string()),
                Value::Text(amount.round_dp(4).to_string()),
                Value::Text(amount.round_dp(4).to_string()),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("建采购明细失败：{err}"));
        item_ids.push(item_id);
    }
    (order_id, item_ids)
}

/// 造一张「来源采购单」的收货单草稿，返回收货单 id。
async fn create_sourced_stock_in(order_id: Uuid, item_id: Uuid, material: &str, qty: &str) -> Uuid {
    let request = CreateStockInReq {
        source_order_id: Some(order_id.to_string()),
        purchase_order_id: None,
        supplier_id: None,
        is_sourceless: None,
        warehouse_id: Some(WAREHOUSE.to_string()),
        in_date: None,
        invoice_received: None,
        remark: Some("P15 联动作废夹具".to_string()),
        items: vec![StockInItemReq {
            purchase_order_item_id: Some(item_id.to_string()),
            material_id: Some(material.to_string()),
            qty: Some(dec(qty)),
            price_excl: Some(dec("2")),
            tax_rate: Some(dec("0")),
            location_id: None,
            serial_ids: Vec::new(),
            remark: None,
        }],
        total_qty: None,
        total_amount: None,
        total_tax: None,
    };
    stockin_handler::create_stock_in(&request, None)
        .await
        .unwrap_or_else(|err| panic!("建收货单失败：{} {}", err.code, err.msg))
}

fn post_req() -> StockInStatusReq {
    StockInStatusReq {
        status: Some("POSTED".to_string()),
        ..Default::default()
    }
}

/// 建 + 过账，返回收货单 id（过账会顺带生成进项凭证 + 应付单）。
async fn create_and_post(order_id: Uuid, item_id: Uuid, material: &str, qty: &str) -> Uuid {
    let id = create_sourced_stock_in(order_id, item_id, material, qty).await;
    stockin_handler::change_stock_in_status(id, &post_req(), None)
        .await
        .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));
    id
}

// ==================== 断言小工具 ====================

async fn text_of(sql: &str) -> String {
    db().scalar_with_params(sql, vec![])
        .await
        .unwrap_or_else(|err| panic!("查询失败：{err}\nSQL: {sql}"))
}

async fn i64_of(sql: &str) -> i64 {
    db().scalar_with_params(sql, vec![])
        .await
        .unwrap_or_else(|err| panic!("查询失败：{err}\nSQL: {sql}"))
}

async fn decimal_of(sql: &str) -> Decimal {
    db().scalar_with_params(sql, vec![])
        .await
        .unwrap_or_else(|err| panic!("查询失败：{err}\nSQL: {sql}"))
}

async fn po_status(order_id: Uuid) -> String {
    text_of(&format!(
        "SELECT status FROM biz_purchase_order WHERE id = '{order_id}'::uuid"
    ))
    .await
}

async fn si_status(stock_in_id: Uuid) -> String {
    text_of(&format!(
        "SELECT status FROM biz_stock_in WHERE id = '{stock_in_id}'::uuid"
    ))
    .await
}

async fn inventory_qty(material: &str) -> Decimal {
    decimal_of(&format!(
        "SELECT COALESCE(sum(qty), 0) FROM biz_inventory \
          WHERE material_id = '{material}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
    ))
    .await
}

async fn red_voucher_count(stock_in_id: Uuid) -> i64 {
    i64_of(&format!(
        "SELECT count(*) FROM biz_voucher WHERE source_id = '{stock_in_id}'::uuid AND is_red = 1"
    ))
    .await
}

async fn payable_status(stock_in_id: Uuid) -> String {
    text_of(&format!(
        "SELECT status FROM biz_payable \
          WHERE source_type = 'STOCK_IN' AND source_id = '{stock_in_id}'::uuid AND deleted_at IS NULL"
    ))
    .await
}

// ==================== ① 纯逻辑用例（不需要库） ====================

/// ① `doc_type` 归一化：三种写法都认，未知类型 400 且点名可选项。
#[test]
fn p15_doc_type_is_normalized_and_unknown_is_rejected() {
    for raw in ["purchase-order", "purchase_order", "PO"] {
        let text = raw.trim().to_ascii_lowercase().replace('_', "-");
        assert!(
            matches!(text.as_str(), "purchase-order" | "po"),
            "前端可能发这几种写法：{raw}"
        );
    }
    // 真正的校验函数是私有的，这里通过公开入口验证 400 行为（不碰库：解析在连接之前）
    let rt = tokio::runtime::Runtime::new().unwrap();
    let error = rt
        .block_on(cancel_cascade::analyze("unknown-doc", Uuid::now_v7()))
        .unwrap_err();
    assert_eq!(error.status, 400, "{}", error.msg);
    assert!(
        error.msg.contains("purchase-order"),
        "报文要点名可选项：{}",
        error.msg
    );
}

/// ② 调拨单不支持联动作废（文案要给出替代入口）。
#[test]
fn p15_transfer_is_rejected_with_alternative() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let error = rt
        .block_on(cancel_cascade::cascade(
            "transfer",
            Uuid::now_v7(),
            Some("测试"),
            None,
        ))
        .unwrap_err();
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("PATCH"), "{}", error.msg);
}

/// ③ reason 必填且至少 4 字（会计作废必须能追溯）—— 校验发生在连库之前。
#[test]
fn p15_reason_is_required_and_long_enough() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let missing = rt
        .block_on(cancel_cascade::cascade(
            "purchase-order",
            Uuid::now_v7(),
            None,
            None,
        ))
        .unwrap_err();
    assert_eq!(missing.status, 400);
    assert!(missing.msg.contains("reason"), "{}", missing.msg);

    let too_short = rt
        .block_on(cancel_cascade::cascade(
            "purchase-order",
            Uuid::now_v7(),
            Some("废"),
            None,
        ))
        .unwrap_err();
    assert_eq!(too_short.status, 400);
    assert!(too_short.msg.contains("太短"), "{}", too_short.msg);
}

/// ④ 状态机：联动作废依赖的两条新边必须放行，且都要求原因。
#[test]
fn p15_purchase_order_can_be_cancelled_after_receipt() {
    for (from, to) in [
        (
            purchase_model::STATUS_PARTIAL_RECEIVED,
            purchase_model::STATUS_CANCELLED,
        ),
        (
            purchase_model::STATUS_RECEIVED,
            purchase_model::STATUS_CANCELLED,
        ),
    ] {
        assert!(
            purchase_model::can_transition(from, to),
            "{from} → {to} 必须放行（联动作废要走到）"
        );
        assert!(
            state_machine::PURCHASE_ORDER_REASON_EDGES.contains(&(from, to)),
            "{from} → {to} 必须要求原因"
        );
    }
    // 终态仍然不可再流转
    assert!(!purchase_model::can_transition(
        purchase_model::STATUS_CANCELLED,
        purchase_model::STATUS_SUBMITTED
    ));
}

// ==================== ② 真库用例 ====================

/// ⑤ 影响分析：已收货的采购单要列出「收货单 + 凭证 + 应付单 + 采购单」四类，
/// 金额取各自的实际值，`can_cancel = true`。
#[tokio::test]
async fn p15_impact_lists_stock_in_voucher_payable_and_order() {
    let Some(_guard) = setup().await else {
        return;
    };
    let (order_id, item_ids) = create_po(&[(MATERIAL_A, "5")]).await;
    let stock_in_id = create_and_post(order_id, item_ids[0], MATERIAL_A, "5").await;

    // 过账把采购单推到 RECEIVED：这是卡的正面场景
    assert_eq!(po_status(order_id).await, purchase_model::STATUS_RECEIVED);

    let impact = cancel_cascade::analyze("purchase-order", order_id)
        .await
        .expect("影响分析必须成功");
    assert!(impact.can_cancel, "没有拦路虎：{:?}", impact.blockers);
    let kinds: Vec<&str> = impact.steps.iter().map(|s| s.doc_type.as_str()).collect();
    assert_eq!(
        kinds,
        vec!["stock-in", "voucher", "payable", "purchase-order"],
        "影响面必须是这四类，实际 {kinds:?}"
    );
    // 收货单金额 = 价税合计（5 × 2 = 10）
    let si_step = &impact.steps[0];
    let in_no = text_of(&format!(
        "SELECT in_no FROM biz_stock_in WHERE id = '{stock_in_id}'::uuid"
    ))
    .await;
    assert_eq!(si_step.doc_no, in_no, "影响面里的单号必须与库一致");
    assert!(
        si_step.amount == dec("10.0000") || si_step.amount == dec("10"),
        "收货单金额应为 10，实际 {}",
        si_step.amount
    );
    // 凭证 / 应付单都标成「自动处理」
    assert_eq!(impact.steps[1].action, cancel_cascade::ACTION_EXECUTE);
    assert!(impact.steps[1].action_label.contains("红冲"));
    assert_eq!(impact.steps[2].action, cancel_cascade::ACTION_EXECUTE);
    assert!(impact.steps[2].action_label.contains("自动作废"));
    // 采购单自己也标成执行
    assert_eq!(impact.steps[3].action, cancel_cascade::ACTION_EXECUTE);
    assert_eq!(impact.steps[3].doc_status, purchase_model::STATUS_RECEIVED);
}

/// ⑥ 影响分析只读：连跑两次，单据状态与库存一动不动。
#[tokio::test]
async fn p15_impact_is_read_only() {
    let Some(_guard) = setup().await else {
        return;
    };
    let (order_id, item_ids) = create_po(&[(MATERIAL_A, "3")]).await;
    let stock_in_id = create_and_post(order_id, item_ids[0], MATERIAL_A, "3").await;

    let before_qty = inventory_qty(MATERIAL_A).await;
    let first = cancel_cascade::analyze("purchase-order", order_id)
        .await
        .unwrap();
    let second = cancel_cascade::analyze("purchase-order", order_id)
        .await
        .unwrap();
    assert_eq!(first.steps.len(), second.steps.len());
    assert_eq!(po_status(order_id).await, purchase_model::STATUS_RECEIVED);
    assert_eq!(si_status(stock_in_id).await, stockin_model::STATUS_POSTED);
    assert_eq!(inventory_qty(MATERIAL_A).await, before_qty);
    assert_eq!(red_voucher_count(stock_in_id).await, 0);
}

/// ⑦ 联动作废主链：三本账回基线 + 凭证红冲 + 应付作废 + 采购单作废，
/// 且返回的 `seq` 就是执行顺序（先收货单、后采购单）。
#[tokio::test]
async fn p15_cascade_voids_downstream_then_order() {
    let Some(_guard) = setup().await else {
        return;
    };
    let (order_id, item_ids) = create_po(&[(MATERIAL_A, "5")]).await;
    let stock_in_id = create_and_post(order_id, item_ids[0], MATERIAL_A, "5").await;
    assert_eq!(inventory_qty(MATERIAL_A).await, dec("5.0000"));

    let result = cancel_cascade::cascade(
        "purchase-order",
        order_id,
        Some("客户取消订单，联动作废"),
        None,
    )
    .await
    .expect("联动作废必须成功");

    // 执行顺序：收货单 → 采购单
    let order_seq: Vec<(u32, &str)> = result
        .steps
        .iter()
        .map(|step| (step.seq, step.doc_type.as_str()))
        .collect();
    assert_eq!(order_seq, vec![(1, "stock-in"), (2, "purchase-order")]);
    assert!(
        result
            .steps
            .iter()
            .all(|s| s.outcome == cancel_cascade::OUTCOME_DONE)
    );

    // 单据状态
    assert_eq!(
        si_status(stock_in_id).await,
        stockin_model::STATUS_CANCELLED
    );
    assert_eq!(po_status(order_id).await, purchase_model::STATUS_CANCELLED);
    // 三本账回基线
    assert_eq!(inventory_qty(MATERIAL_A).await, Decimal::ZERO);
    assert_eq!(
        decimal_of(&format!(
            "SELECT COALESCE(sum(amount), 0) FROM biz_inventory \
              WHERE material_id = '{MATERIAL_A}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
        ))
        .await,
        Decimal::ZERO
    );
    assert_eq!(
        i64_of(&format!(
            "SELECT count(*) FROM biz_inventory_location \
              WHERE material_id = '{MATERIAL_A}'::uuid AND qty <> 0"
        ))
        .await,
        0
    );
    // 凭证红冲（原凭证保持 POSTED，另生成一张红字）
    assert_eq!(red_voucher_count(stock_in_id).await, 1);
    assert_eq!(
        i64_of(&format!(
            "SELECT count(*) FROM biz_voucher \
              WHERE source_id = '{stock_in_id}'::uuid AND is_red = 0 AND reversed_by IS NOT NULL"
        ))
        .await,
        1,
        "原凭证要打上 reversed_by"
    );
    // 应付单作废
    assert_eq!(payable_status(stock_in_id).await, "CANCELLED");
    // 采购明细的已收数量退回（P2-3：派生值，本单作废后不再计入 POSTED 集合）
    let received_after = crate::business::erp::model::purchase_order_item_find_by_id(item_ids[0])
        .await
        .unwrap()
        .expect("采购明细行必须存在")
        .received_qty;
    assert_eq!(received_after, Decimal::ZERO);
}

/// ⑧ 无下游时直接作废：只有采购单自己一个动作。
#[tokio::test]
async fn p15_cascade_without_downstream_only_voids_order() {
    let Some(_guard) = setup().await else {
        return;
    };
    let (order_id, _items) = create_po(&[(MATERIAL_A, "5")]).await;

    let impact = cancel_cascade::analyze("purchase-order", order_id)
        .await
        .unwrap();
    assert_eq!(impact.steps.len(), 1, "没有下游就只列采购单自己");
    assert_eq!(impact.steps[0].doc_type, "purchase-order");
    assert!(impact.can_cancel);

    let result = cancel_cascade::cascade("purchase-order", order_id, Some("供应商取消供货"), None)
        .await
        .unwrap();
    assert_eq!(result.steps.len(), 1);
    assert_eq!(result.steps[0].outcome, cancel_cascade::OUTCOME_DONE);
    assert_eq!(po_status(order_id).await, purchase_model::STATUS_CANCELLED);
}

/// ⑨ 中途失败整体回滚：第二步（第二张收货单）退不了库存时，
/// 第一步已经作废的收货单必须**退回已过账**，凭证也不算红冲过。
///
/// 这里直接打事务体 `cascade_purchase_order_tx`：影响分析是只读预检，
/// 会在开事务前就把「库存不够退」拦成拦路虎；要验证的正是**事务体内部**
/// 在某一步失败时是否整笔回滚，所以绕开预检、在同一个事务里制造失败条件。
#[tokio::test]
async fn p15_cascade_rolls_back_when_later_step_fails() {
    let Some(_guard) = setup().await else {
        return;
    };
    let (order_id, item_ids) = create_po(&[(MATERIAL_A, "5"), (MATERIAL_B, "3")]).await;
    let si_a = create_and_post(order_id, item_ids[0], MATERIAL_A, "5").await;
    let si_b = create_and_post(order_id, item_ids[1], MATERIAL_B, "3").await;
    assert_eq!(po_status(order_id).await, purchase_model::STATUS_RECEIVED);

    let before_a = inventory_qty(MATERIAL_A).await;
    let before_b = inventory_qty(MATERIAL_B).await;

    let impact = cancel_cascade::analyze("purchase-order", order_id)
        .await
        .unwrap();
    assert!(impact.can_cancel, "预检阶段一切正常：{:?}", impact.blockers);

    let now = Utc::now();
    let op = state_machine::Operator::of(None);
    let tx = db().begin().await.unwrap();
    // 在同一事务里把 B 的过账流水删掉 —— 第一张收货单（A）会先被作废成功，
    // 轮到 B 时 `cancel_plan_of` 找不到流水 → `INV_NO_TXN` 失败。
    //
    // ⚠ 必须走 `tx.execute_with_params` 而不是 `db()`：走 `db()` 是连接池上的**另一个连接**，
    // 那条 DELETE 自动提交，`tx.rollback()` 收不回来（第一版就是这么写的，
    // 于是最后那条「流水被一起恢复」的断言永远拿 0 —— 测的其实是「删除生效了」，
    // 根本证明不了「同一个事务」）。删除跟着事务一起回滚，才验证得到整笔回滚。
    tx.execute_with_params(
        &format!("DELETE FROM biz_inventory_txn WHERE doc_id = '{si_b}'::uuid"),
        vec![],
    )
    .await
    .unwrap();
    let outcome = cancel_cascade::cascade_purchase_order_tx(
        &tx,
        order_id,
        &impact,
        "联动作废：回滚用例",
        &op,
        now,
    )
    .await;
    assert!(outcome.is_err(), "第二步必须失败");
    let error = outcome.unwrap_err();
    assert!(
        error.msg.contains("第 2 步"),
        "报文要点名第几步：{}",
        error.msg
    );
    assert!(
        error.msg.contains("回滚"),
        "报文要说清已回滚：{}",
        error.msg
    );
    let _ = tx.rollback().await;

    // 整笔回滚：第一张收货单仍是已过账，A 的库存一分没退，也没有红字凭证
    assert_eq!(si_status(si_a).await, stockin_model::STATUS_POSTED);
    assert_eq!(si_status(si_b).await, stockin_model::STATUS_POSTED);
    assert_eq!(po_status(order_id).await, purchase_model::STATUS_RECEIVED);
    assert_eq!(inventory_qty(MATERIAL_A).await, before_a);
    assert_eq!(inventory_qty(MATERIAL_B).await, before_b);
    assert_eq!(red_voucher_count(si_a).await, 0);
    // B 的流水被回滚语句一起恢复了（证明这是同一个事务）
    assert_eq!(
        i64_of(&format!(
            "SELECT count(*) FROM biz_inventory_txn WHERE doc_id = '{si_b}'::uuid"
        ))
        .await,
        1
    );
}

/// ⑩ 有拦路虎时不执行：应付单已付过款 → `can_cancel = false` 且 409 点名原因。
#[tokio::test]
async fn p15_cascade_blocked_when_payable_already_paid() {
    let Some(_guard) = setup().await else {
        return;
    };
    let (order_id, item_ids) = create_po(&[(MATERIAL_A, "4")]).await;
    let stock_in_id = create_and_post(order_id, item_ids[0], MATERIAL_A, "4").await;
    // 造「付过一部分钱」：直接改库（付款链路不在本卡范围）
    db().execute_with_params(
        &format!(
            "UPDATE biz_payable SET paid_amount = 1, unpaid_amount = amount - 1, status = 'PARTIAL_PAID' \
              WHERE source_type = 'STOCK_IN' AND source_id = '{stock_in_id}'::uuid"
        ),
        vec![],
    )
    .await
    .unwrap();

    let impact = cancel_cascade::analyze("purchase-order", order_id)
        .await
        .unwrap();
    assert!(!impact.can_cancel, "已付款的应付单是拦路虎");
    assert!(
        impact.blockers.iter().any(|text| text.contains("已付款")),
        "拦路虎要说明是付款：{:?}",
        impact.blockers
    );
    assert!(
        impact
            .steps
            .iter()
            .any(|step| step.action == cancel_cascade::ACTION_BLOCKED),
        "被拦的那一步要标 BLOCKED"
    );

    let error = cancel_cascade::cascade("purchase-order", order_id, Some("试试能不能硬做"), None)
        .await
        .unwrap_err();
    assert_eq!(error.status, 409);
    assert!(error.msg.contains("不能一键处理"), "{}", error.msg);
    // 一处都没动
    assert_eq!(po_status(order_id).await, purchase_model::STATUS_RECEIVED);
    assert_eq!(si_status(stock_in_id).await, stockin_model::STATUS_POSTED);
}

/// ⑪ 已作废的收货单在影响面里标成 SKIP，不再重复退账。
#[tokio::test]
async fn p15_impact_marks_cancelled_stock_in_as_skip() {
    let Some(_guard) = setup().await else {
        return;
    };
    let (order_id, item_ids) = create_po(&[(MATERIAL_A, "5")]).await;
    let stock_in_id = create_and_post(order_id, item_ids[0], MATERIAL_A, "5").await;
    // 先单独把收货单废掉（走联动作废的收货单入口）
    cancel_cascade::cascade("stock-in", stock_in_id, Some("收货数量填错"), None)
        .await
        .unwrap();
    assert_eq!(
        si_status(stock_in_id).await,
        stockin_model::STATUS_CANCELLED
    );

    let impact = cancel_cascade::analyze("purchase-order", order_id)
        .await
        .unwrap();
    let si_step = impact
        .steps
        .iter()
        .find(|step| step.doc_type == "stock-in")
        .expect("已作废的收货单也要出现在影响面里");
    assert_eq!(si_step.action, cancel_cascade::ACTION_SKIP);
    assert!(impact.can_cancel, "下游都处理完了，采购单本身还能废");
}

/// ⑫ 幂等：整张单已经废完再调一次 → 不报错、全部 SKIPPED、账不动。
#[tokio::test]
async fn p15_cascade_is_idempotent_on_cancelled_document() {
    let Some(_guard) = setup().await else {
        return;
    };
    let (order_id, item_ids) = create_po(&[(MATERIAL_A, "2")]).await;
    let stock_in_id = create_and_post(order_id, item_ids[0], MATERIAL_A, "2").await;
    cancel_cascade::cascade("purchase-order", order_id, Some("第一次联动作废"), None)
        .await
        .unwrap();

    let again = cancel_cascade::cascade("purchase-order", order_id, Some("再点一次"), None)
        .await
        .expect("重复调用必须成功（幂等），而不是 409");
    assert!(
        again
            .steps
            .iter()
            .all(|step| step.outcome == cancel_cascade::OUTCOME_SKIPPED),
        "{:?}",
        again.steps
    );
    assert!(again.message.contains("没有需要执行"), "{}", again.message);
    assert_eq!(
        si_status(stock_in_id).await,
        stockin_model::STATUS_CANCELLED
    );
    assert_eq!(red_voucher_count(stock_in_id).await, 1, "红冲只做一次");
    assert_eq!(inventory_qty(MATERIAL_A).await, Decimal::ZERO);
}

/// ⑬ 收货单入口：影响面要带上凭证与应付单（它们随收货单作废自动处理）。
#[tokio::test]
async fn p15_stock_in_impact_lists_voucher_and_payable() {
    let Some(_guard) = setup().await else {
        return;
    };
    let (order_id, item_ids) = create_po(&[(MATERIAL_A, "6")]).await;
    let stock_in_id = create_and_post(order_id, item_ids[0], MATERIAL_A, "6").await;

    let impact = cancel_cascade::analyze("stock-in", stock_in_id)
        .await
        .unwrap();
    let kinds: Vec<&str> = impact.steps.iter().map(|s| s.doc_type.as_str()).collect();
    assert_eq!(kinds, vec!["stock-in", "voucher", "payable"]);
    assert!(impact.can_cancel);

    let result = cancel_cascade::cascade("stock-in", stock_in_id, Some("收货单作废"), None)
        .await
        .unwrap();
    assert_eq!(result.steps.len(), 1);
    assert_eq!(
        si_status(stock_in_id).await,
        stockin_model::STATUS_CANCELLED
    );
    assert_eq!(red_voucher_count(stock_in_id).await, 1);
    assert_eq!(payable_status(stock_in_id).await, "CANCELLED");
    assert_eq!(inventory_qty(MATERIAL_A).await, Decimal::ZERO);
    // 采购单回到 PARTIAL_RECEIVED（收货作废的回退边），仍可再收货
    assert_eq!(
        po_status(order_id).await,
        purchase_model::STATUS_PARTIAL_RECEIVED
    );
}

/// ⑭ 应付单入口：未付款的应付单能一键作废并红冲进项凭证；已作废的再调是幂等。
#[tokio::test]
async fn p15_payable_cascade_reverses_voucher() {
    let Some(_guard) = setup().await else {
        return;
    };
    let (order_id, item_ids) = create_po(&[(MATERIAL_A, "8")]).await;
    let stock_in_id = create_and_post(order_id, item_ids[0], MATERIAL_A, "8").await;
    let payable_id: Uuid = db()
        .scalar_with_params(
            &format!(
                "SELECT id FROM biz_payable WHERE source_type = 'STOCK_IN' \
                  AND source_id = '{stock_in_id}'::uuid"
            ),
            vec![],
        )
        .await
        .unwrap();

    let impact = cancel_cascade::analyze("payable", payable_id)
        .await
        .unwrap();
    assert!(impact.can_cancel);
    assert!(
        impact.steps.iter().any(|step| step.doc_type == "voucher"),
        "应付作废要顺带红冲进项凭证"
    );

    cancel_cascade::cascade("payable", payable_id, Some("发票开错，作废重开"), None)
        .await
        .unwrap();
    assert_eq!(payable_status(stock_in_id).await, "CANCELLED");
    assert_eq!(
        i64_of(&format!(
            "SELECT count(*) FROM biz_voucher WHERE source_id = '{stock_in_id}'::uuid AND is_red = 1"
        ))
        .await,
        1
    );

    // 再调一次：幂等（全 SKIPPED）
    let again = cancel_cascade::cascade("payable", payable_id, Some("再点一次"), None)
        .await
        .unwrap();
    assert!(
        again
            .steps
            .iter()
            .all(|step| step.outcome == cancel_cascade::OUTCOME_SKIPPED),
        "{:?}",
        again.steps
    );
    let _ = order_id;
}
