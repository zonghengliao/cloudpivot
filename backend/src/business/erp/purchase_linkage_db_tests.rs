// ============================================================================
// purchase_linkage_db_tests.rs —— 真库：采购单状态变更的跨单据联动（LNK-02）
//
// 为什么必须真库：本卡交付的是「配一条规则，采购单状态一变它真的执行」——
// 纯逻辑单测只能证明「源码里出现了调用」，证明不了
// 「规则读得到 → 事务内 / 提交后真的跑了 → 通知落到 sys_notice
//  → 重复推进时不跑第二次 → 排除清单里的动作确实没跑」。所以全部用例走**生产同款路径**：
//
// ```text
// 夹具：供应商 / 仓库 / 物料 / 单位（stockin 夹具）+ 一行 biz_flow_linkage_rule
//   ① 提交        → purchase_handler::change_purchase_order_status（DRAFT → SUBMITTED）
//   ②③④⑤ 部分/全部收货 → stockin_handler::create_stock_in + change_stock_in_status(POSTED)
//                        → stockin_model::sync_purchase_status（transition_if_changed）
// ```
//
// 用法：
// ```bash
// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/<测试库>'
// cargo test --lib business::erp::stockin_handler::purchase_linkage_db -- --nocapture --test-threads=1
// ```
// 缺 `ERP_DB_URL` 时打印 `SKIP` 并直接返回（CI 无库时 `cargo test --lib` 仍然全绿）。
//
// # 夹具卫生
//
// 规则按 `name LIKE 'LNK02-%'` 定位，每条用例**进 / 出都清**：规则是按触发点全局生效的
// （没有「按单据」粒度），残留一行就会让后续所有跑到 `PURCHASE_ORDER/<状态>` 的用例
// 多发一条通知 / 多建一个任务。通知按幂等键前缀 `LNK:PURCHASE_ORDER:%` 清。
// 采购单 / 收货单 / 库存 / 凭证 / 应付由 `stockin` 夹具的 `setup()` 按供应商根清掉。
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::stockin_handler_tests_fixtures::*;
use super::stockin_handler_tests_helpers::*;
use super::*;
use crate::business::erp::purchase_dto::PurchaseOrderStatusReq;
use crate::business::erp::{purchase_handler, purchase_model};
use crate::business::flow::linkage;
use crate::business::flow::linkage::spec;
use crate::business::shared::state_machine;

/// 夹具规则的命名前缀（本卡专用，与其它卡 / 生产的规则互不影响）。
///
/// ⚠️ 清理用的是 `LIKE 'LNK02%'`（**不带连字符**）：真机取证脚本的规则名是
/// 「LNK02 采购部分收货 → 通知制单人」（`LNK02` + 空格），而唯一索引
/// `uk_flow_linkage_rule(trigger_entity, trigger_status, action_code)` **不带**
/// `WHERE deleted_at IS NULL` —— 取证规则软删之后仍然占着
/// `PURCHASE_ORDER / PARTIAL_RECEIVED / SEND_NOTICE` 这个坑位，
/// 用 `LNK02-%` 清不到它，再插同名规则就撞唯一约束（假红）。
const RULE_PREFIX: &str = "LNK02";

/// 制单人 / 默认收件人：`FIXTURES` 里种入的仓管员（非超管，避免任何提权短路）。
const PO_CREATOR: &str = "b2b10009-0000-4000-8000-000000000001";

/// 执行一条 SQL（失败即 panic，测试里不做容错）。
async fn exec(sql: &str, params: Vec<Value>, label: &str) {
    db().execute_with_params(sql, params)
        .await
        .unwrap_or_else(|error| panic!("{label}：{error}"));
}

/// 数一行（失败即 panic）。
async fn count_of(sql: &str, params: Vec<Value>, label: &str) -> i64 {
    db().scalar_with_params::<i64>(sql, params)
        .await
        .unwrap_or_else(|error| panic!("{label}：{error}"))
}

/// 清本卡的联动规则。
async fn purge_rules() {
    exec(
        &format!("DELETE FROM biz_flow_linkage_rule WHERE name LIKE '{RULE_PREFIX}%'"),
        vec![],
        "清理 LNK-02 联动规则夹具",
    )
    .await;
}

/// 清本卡的联动通知（按幂等键前缀，接收人行级联删）。
async fn purge_notices() {
    exec(
        "DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:PURCHASE_ORDER:%'",
        vec![],
        "清理 LNK-02 联动通知",
    )
    .await;
}

/// 清本卡的联动任务（`CREATE_TASK` 动作的落点）。
async fn purge_tasks() {
    exec(
        "DELETE FROM biz_task WHERE title LIKE 'LNK02 %'",
        vec![],
        "清理 LNK-02 联动任务",
    )
    .await;
}

/// 夹具收尾（规则 / 通知 / 任务一把清）。
async fn teardown() {
    purge_rules().await;
    purge_notices().await;
    purge_tasks().await;
}

/// 插一条 `PURCHASE_ORDER/<status>` 上的规则。
async fn add_rule(status: &str, action: &str, params: &str) {
    exec(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, seq) \
         VALUES (gen_random_uuid(), ?, 'PURCHASE_ORDER', ?, ?, ?::jsonb, 10)",
        vec![
            Value::Text(format!("{RULE_PREFIX}-{status}-{action}")),
            Value::Text(status.to_string()),
            Value::Text(action.to_string()),
            // jsonb 参数必须用 `Value::Json`（与 `linkage::model::upsert_rule` 同口径）
            Value::Json(params.to_string()),
        ],
        "插入联动规则夹具",
    )
    .await;
}

/// 按幂等键数通知条数。
async fn notices_of_key(key: &str) -> i64 {
    count_of(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key = ?",
        vec![Value::Text(key.to_string())],
        "查联动通知",
    )
    .await
}

/// 造一张采购单（1 行，数量 `qty`、单价 2、税率 0），`status` 由调用方指定。
///
/// 金额三列必须自洽（`ck_biz_po_tax_sum`：`total_with_tax = total_amount + total_tax`），
/// 否则提交那一步直接被 DDL CHECK 拒掉（400，不是 500）。
async fn seed_po(qty: &str, status: &str) -> (Uuid, Uuid) {
    let order_id = Uuid::now_v7();
    let item_id = Uuid::now_v7();
    let amount = dec(qty) * Decimal::from(2);
    // 单号尾段取 uuid 的随机段（now_v7 前 8 位是时间戳，同窗口内会撞唯一索引）
    let order_no = format!("LNK02-{}", &order_id.simple().to_string()[24..32]);
    exec(
        "INSERT INTO biz_purchase_order \
             (id, order_no, supplier_id, order_date, warehouse_id, status, total_qty, \
              total_amount, total_tax, total_with_tax, create_by) \
         VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, ?::uuid, ?, NULLIF(?::text,'')::numeric, \
             NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,'')::numeric, ?::uuid)",
        vec![
            Value::from(&order_id),
            Value::Text(order_no),
            Value::from(&Uuid::parse_str(SUPPLIER).unwrap()),
            Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
            Value::Text(status.to_string()),
            Value::Text(qty.to_string()),
            Value::Text(amount.to_string()),
            Value::Text(amount.to_string()),
            Value::from(&Uuid::parse_str(PO_CREATOR).unwrap()),
        ],
        "造采购单",
    )
    .await;
    exec(
        "INSERT INTO biz_purchase_order_item \
             (id, order_id, line_no, material_id, unit_id, qty, price, price_incl, tax_rate, \
              amount, tax_amount, with_tax_amount) \
         VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, 2, 2, 0, \
             NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,'')::numeric)",
        vec![
            Value::from(&item_id),
            Value::from(&order_id),
            Value::from(&Uuid::parse_str(MATERIAL_A).unwrap()),
            Value::from(&Uuid::parse_str(UNIT).unwrap()),
            Value::Text(qty.to_string()),
            Value::Text(amount.to_string()),
            Value::Text(amount.to_string()),
        ],
        "造采购明细",
    )
    .await;
    (order_id, item_id)
}

/// 采购单当前状态。
async fn po_status(order_id: Uuid) -> String {
    db().scalar_with_params(
        "SELECT status::text FROM biz_purchase_order WHERE id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await
    .expect("查采购单状态")
}

/// 采购明细的**派生**已收数量（`POSTED_RECEIVED_SQL`，与过账闸 / 页面同一口径）。
async fn received_of(item_id: Uuid) -> Decimal {
    db().scalar_with_params(
        &format!(
            "SELECT ({}::numeric(18,4)) AS received_qty FROM biz_purchase_order_item i \
              WHERE i.id = ?::uuid",
            purchase_model::POSTED_RECEIVED_SQL
        ),
        vec![Value::from(&item_id)],
    )
    .await
    .expect("查派生已收数量")
}

/// 建一张带来源采购单的收货单并过账（走生产同款 `change_stock_in_status`）。
async fn post_sourced(order_id: Uuid, item_id: Uuid, qty: &str) {
    let id = sourced_doc(order_id, item_id, qty).await;
    // 操作人必须给（`CREATE_TASK` 的 `assignee_strategy=OPERATOR` 要用它；
    // 传 None 时那个动作只会 Failed 记日志，任务建不出来 → 探针失效）
    change_stock_in_status(id, &post_req(), Some(Uuid::parse_str(PO_CREATOR).unwrap()))
        .await
        .unwrap_or_else(|error| panic!("收 {qty} 过账失败：{} {}", error.code, error.msg));
}

// ---------- ① 提交触发：DRAFT → SUBMITTED → 通知真的发出 ----------
#[tokio::test]
async fn submit_status_triggers_linkage_notice_once() {
    let Some(_guard) = setup().await else {
        return;
    };
    teardown().await;
    // SEND_NOTICE 是 AfterCommit 动作：必须由 `LinkageOutbox::flush` 在提交后执行。
    add_rule(
        "SUBMITTED",
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    let (order_id, _item_id) = seed_po("10", "DRAFT").await;
    let req = PurchaseOrderStatusReq {
        status: Some("SUBMITTED".to_string()),
        reason: None,
        approval: Default::default(),
    };
    // 走真实的状态流转入口（PATCH /status 在 target=SUBMITTED 时会把 gate 交给
    // `submit_approval`；这里直接调它内部那一段，绕开审批流编排 —— 状态写入点相同）
    purchase_handler::change_purchase_order_status(
        order_id,
        &req,
        state_machine::Operator::of(Some(Uuid::parse_str(PO_CREATOR).unwrap())),
    )
    .await
    .expect("DRAFT → SUBMITTED 必须成功");
    assert_eq!(po_status(order_id).await, "SUBMITTED", "前置：状态真的变了");

    let key = format!("LNK:PURCHASE_ORDER:SUBMITTED:{order_id}");
    assert_eq!(
        notices_of_key(&key).await,
        1,
        "PURCHASE_ORDER/SUBMITTED 上配的 SEND_NOTICE 必须真的发出一条通知（幂等键 {key}）—— \
         只改 spec.rs 的 wired 标记、handler 里不调引擎时这里会是 0"
    );
    // 通知内容来自触发点基稿（不是配置凭空生成）：标题带目标态中文名
    let rows = db()
        .find_with_params(
            "SELECT title, target_type, biz_type, link_url FROM sys_notice WHERE idempotency_key = ?",
            vec![Value::Text(key.clone())],
        )
        .await
        .expect("查通知行");
    let row = rows.first().expect("通知行");
    assert_eq!(
        row.get_typed::<String>("title").expect("title"),
        "采购订单已提交"
    );
    assert_eq!(
        row.get_typed::<String>("biz_type").expect("biz_type"),
        "PURCHASE_ORDER"
    );
    assert_eq!(
        row.get_typed::<String>("target_type").expect("target_type"),
        "USER"
    );
    assert!(
        row.get_typed::<String>("link_url")
            .expect("link_url")
            .contains(&order_id.to_string()),
        "通知必须链回单据详情"
    );

    teardown().await;
}

// ---------- ② 部分收货触发：SUBMITTED → PARTIAL_RECEIVED ----------
#[tokio::test]
async fn partial_receipt_triggers_linkage() {
    let Some(_guard) = setup().await else {
        return;
    };
    teardown().await;
    add_rule(
        "PARTIAL_RECEIVED",
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    let (order_id, item_id) = seed_po("10", "SUBMITTED").await;
    post_sourced(order_id, item_id, "4").await;

    assert_eq!(
        po_status(order_id).await,
        "PARTIAL_RECEIVED",
        "前提：收 4/10 必须推进到部分收货"
    );
    let key = format!("LNK:PURCHASE_ORDER:PARTIAL_RECEIVED:{order_id}");
    assert_eq!(
        notices_of_key(&key).await,
        1,
        "部分收货这条边由 `sync_purchase_status`（transition_if_changed）推进，\
         不经过 `purchase_model::apply_status` —— 不接那一处这里会是 0"
    );

    teardown().await;
}

// ---------- ③ 全部收货触发：PARTIAL_RECEIVED → RECEIVED ----------
#[tokio::test]
async fn full_receipt_triggers_linkage() {
    let Some(_guard) = setup().await else {
        return;
    };
    teardown().await;
    // 只配 RECEIVED：先部分收货时不响，收满那一刻响一次
    add_rule(
        "RECEIVED",
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    let (order_id, item_id) = seed_po("10", "SUBMITTED").await;
    post_sourced(order_id, item_id, "4").await;
    assert_eq!(po_status(order_id).await, "PARTIAL_RECEIVED");
    let key = format!("LNK:PURCHASE_ORDER:RECEIVED:{order_id}");
    assert_eq!(
        notices_of_key(&key).await,
        0,
        "只配了 RECEIVED：部分收货那一刻不许响"
    );

    post_sourced(order_id, item_id, "6").await;
    assert_eq!(
        po_status(order_id).await,
        "RECEIVED",
        "前提：补收 6 必须推进到已收货"
    );
    assert_eq!(
        notices_of_key(&key).await,
        1,
        "「部分收货 → 全部收货」这条边必须触发联动（本卡接线前它永远不响）"
    );
    assert_eq!(
        received_of(item_id).await,
        dec("10"),
        "派生已收数量 = 10（不是 20：联动不许再改一次已收量）"
    );

    teardown().await;
}

// ---------- ④ 重复部分收货不重复触发 ----------
#[tokio::test]
async fn repeated_partial_receipt_does_not_trigger_twice() {
    let Some(_guard) = setup().await else {
        return;
    };
    teardown().await;
    // 用 CREATE_TASK 做探针（没有幂等键兜底）：重复触发一次就会多一个任务行，
    // 比「通知有幂等键」更能证明「引擎真的只跑了一次」。
    add_rule(
        "PARTIAL_RECEIVED",
        spec::ACTION_CREATE_TASK,
        r#"{"title":"LNK02 部分收货复核","assignee_strategy":"OPERATOR"}"#,
    )
    .await;

    let (order_id, item_id) = seed_po("10", "SUBMITTED").await;
    post_sourced(order_id, item_id, "3").await;
    let tasks = |label: &'static str| async move {
        count_of(
            "SELECT count(*) FROM biz_task WHERE title = 'LNK02 部分收货复核'",
            vec![],
            label,
        )
        .await
    };
    assert_eq!(
        tasks("首次部分收货后建任务").await,
        1,
        "正常触发一次：建 1 个任务"
    );

    // 第二次部分收货：状态仍是 PARTIAL_RECEIVED（3 + 3 = 6 < 10），
    // `transition_if_changed` 返回 None → 状态没变 → 不许再触发一次联动
    post_sourced(order_id, item_id, "3").await;
    assert_eq!(
        po_status(order_id).await,
        "PARTIAL_RECEIVED",
        "前提：6/10 仍是部分收货（状态没变）"
    );
    assert_eq!(
        tasks("第二次部分收货后任务数").await,
        1,
        "状态没变不许再触发一次联动（任务数从 1 变 2 = 引擎在没变的状态上被重复调用）"
    );

    teardown().await;
}

// ---------- ⑤ 排除动作确实不跑：UPDATE_RECEIVED_QTY ----------
#[tokio::test]
async fn excluded_received_qty_action_is_not_executed_by_the_generic_engine() {
    let Some(_guard) = setup().await else {
        return;
    };
    teardown().await;
    let (order_id, item_id) = seed_po("10", "SUBMITTED").await;
    // 故意配一条**必然超收**的校验规则：qty=999 > 订购量 10。
    // UPDATE_RECEIVED_QTY 是「已实现 + Critical」的动作：排除清单要是丢了，
    // 引擎会真的跑它 → 关键动作失败 → **整笔过账回滚**（用户看到 500）。
    add_rule(
        "RECEIVED",
        spec::ACTION_UPDATE_RECEIVED_QTY,
        &format!(r#"{{"source_item_id":"{item_id}","qty":"999"}}"#),
    )
    .await;

    post_sourced(order_id, item_id, "10").await;
    assert_eq!(
        po_status(order_id).await,
        "RECEIVED",
        "排除清单生效时，这条规则不许把过账搞挂"
    );
    assert_eq!(
        received_of(item_id).await,
        dec("10"),
        "已收数量是派生值：联动不许再改一次（重复落账会变成 20 或直接超收）"
    );
    assert_eq!(
        count_of(
            "SELECT count(*) FROM biz_inventory_txn WHERE doc_id IN \
             (SELECT id FROM biz_stock_in WHERE source_order_id = ?::uuid)",
            vec![Value::from(&order_id)],
            "查收货流水",
        )
        .await,
        1,
        "一次过账一条入库流水（引擎没重复落账）"
    );

    // 反证：同一份规则、不传排除清单 → 引擎真的会去看它（证明上面的「没挂」不是假绿）
    let row = purchase_model::find_order(order_id)
        .await
        .expect("查采购单")
        .expect("采购单存在");
    let ctx = purchase_model::purchase_linkage::status_ctx(
        row.id,
        &row.order_no,
        "RECEIVED",
        Some(Uuid::parse_str(PO_CREATOR).unwrap()),
        row.create_by,
    );
    let excluded = linkage::engine::run_excluding(db(), &ctx, spec::PURCHASE_EXCLUDED_ACTIONS)
        .await
        .expect("排除清单下引擎必须正常返回");
    assert!(
        excluded.runs.is_empty() && excluded.deferred.is_empty(),
        "排除清单里的动作不许产生任何执行记录，实际：{:?}",
        excluded
            .runs
            .iter()
            .map(|item| (&item.action_code, item.outcome.label()))
            .collect::<Vec<_>>()
    );
    // 反证：同一份规则、**不传**排除清单 → 引擎会真的去跑它，而且因为超收
    // 作为关键动作失败返回 Err（`run_excluding` 对 Critical 失败直接 Err →
    // 调用方整笔回滚）。这证明前面「过账没挂」不是「规则没读到」的假绿。
    let unfiltered = linkage::engine::run_excluding(db(), &ctx, &[])
        .await
        .expect_err("不带排除清单时，qty=999 必然超收 → 关键动作失败 → 引擎必须返回 Err");
    assert_eq!(unfiltered.status, 500, "关键动作失败按 500 出口");
    assert!(
        unfiltered.msg.contains(spec::ACTION_UPDATE_RECEIVED_QTY)
            && unfiltered.msg.contains("超收"),
        "报文必须点名动作与原因，实际：{}",
        unfiltered.msg
    );

    teardown().await;
}
