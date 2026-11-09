// ============================================================================
// stocktake_tests.rs —— 盘点单用例（ARCH-3 DC-14）
//
// 两类用例：
//   · **纯逻辑**（不连库）：差异口径 / 状态边 / DTO 校验 / 凭证方向；
//   · **真库**（`ERP_DB_URL` 开关，与其它 ERP 用例同口径）：盘盈 / 盘亏 / 无差异 /
//     作废回滚 + 红冲 / 审批流（分档 + 未批不许过账 + 驳回回草稿）/ 负库存拦截 / 快照口径。
//
// ⚠ `ERP_DB_URL` 未设置时真库用例全部 SKIP（假绿）。交付前必须真跑：
//
// ```bash
// ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//   cargo test --lib business::erp::stocktake -- --test-threads=1
// ```
//
// # 夹具口径
//
// | 对象 | 前缀 / UUID | 说明 |
// |------|-------------|------|
// | 用户 | `stk_` 前缀 + `d211900x` UUID | 制单人 / 仓管主管 / 总经理 |
// | 单位 / 仓库 / 物料 | `DC14-` + `d2110100-…` | 主数据 |
// | 两本账 | 按物料 uuid 删 | 成本账 + 库位账 |
// | 盘点单 | `ST-` 前缀（取号器产出） | 连同状态日志 / 凭证 / 流水一起清 |
//!
//! **串行**（`--test-threads=1`）：清理按前缀删，并行会互删夹具。
// ============================================================================

use std::str::FromStr;

use axum::Json;
use axum::extract::{Extension, Path};
use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use super::stocktake_bridge;
use super::stocktake_dto::{
    CreateStockTakeReq, StockTakeDetailRes, StockTakeScopeItemReq, StockTakeStatusReq, UpdateStockTakeReq,
};
use super::stocktake_handler::{
    create_stock_take, stock_take_change_status, stock_take_create, stock_take_delete, stock_take_detail,
    stock_take_page, stock_take_update, update_stock_take,
};
use super::stocktake_model::{self as model, STATUS_CANCELLED, STATUS_DRAFT, STATUS_PENDING_POST, STATUS_POSTED};
use crate::business::flow::engine;
use crate::business::flow::linkage;
use crate::business::flow::linkage::spec;
use crate::shared::error_code;

// ---------- 夹具常量 ----------

/// 仓管主管角色（`V0182` 种子的固定 UUID）。
const ROLE_WM: &str = "20000000-0000-4000-8000-0000000f0004";
/// 总经理角色（`V0182` 种子的固定 UUID）。
const ROLE_GM: &str = "20000000-0000-4000-8000-0000000f0003";

/// 制单人（盘的人，不能自己批）。
const U_MAKER: &str = "d2119001-0000-4000-8000-000000000001";
/// 仓管主管（单级审批 / 两级的第一级）。
const U_KEEPER: &str = "d2119002-0000-4000-8000-000000000002";
/// 总经理（两级审批的第二级）。
const U_GM: &str = "d2119003-0000-4000-8000-000000000003";

const UNIT: &str = "d2110100-0000-4000-8000-000000000001";
const WAREHOUSE: &str = "d2110100-0000-4000-8000-000000000002";
const MATERIAL_A: &str = "d2110100-0000-4000-8000-000000000011";
const MATERIAL_B: &str = "d2110100-0000-4000-8000-000000000012";

/// 盘点日期（2033-03：测试期间，不与生产 2026-xx 交叠）。
const TAKE_DATE: &str = "2033-03-15";

// 真库用例必须串行（共用全局 `db()`、同一批夹具与同一批 `biz_seq`）。
//
// 锁必须是 `period_test_support` 那把的**同一个对象**：本模块的夹具期是 `2033-03`，
// 而 `period::tests::cleanup()` 会 `DELETE ... WHERE period LIKE '2033-%'` 与
// `biz_accounting_period LIKE '2033-%'` —— 两把不同的锁时，它会把本模块正在用的
// 单据与期间删掉（反之 `period_test_support::cleanup()` 同样会删本模块的凭证）。
use crate::business::erp::period_test_support::DB_LOCK;

fn uid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("金额字面量非法")
}

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期夹具")
}

/// 取响应体（`Res<T>` 的 `data`）。
fn data<T: Serialize>(res: Res<T>) -> T {
    res.data.expect("响应体应带 data")
}

/// 以某个真实用户身份造 claims。
fn claims_of(user: &str) -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: user.to_string(),
        username: Some(format!("stk_{}", &user[..8])),
        roles: vec!["SUPER_ADMIN".to_string()],
        permissions: vec!["*".to_string()],
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    }
}

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

async fn exec(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

/// 清本模块夹具（顺序受外键约束：通知 → 审批实例 → 状态日志 → 单据 → 凭证 → 流水 → 两本账 → 主数据 → 用户）。
async fn purge_fixtures() {
    let cleanups = [
        "DELETE FROM sys_notice_receiver WHERE notice_id IN \
           (SELECT id FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN \
              (SELECT id::text FROM biz_flow_instance WHERE biz_type = 'STOCK_TAKE_POST'))"
            .to_string(),
        "DELETE FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN \
           (SELECT id::text FROM biz_flow_instance WHERE biz_type = 'STOCK_TAKE_POST')"
            .to_string(),
        "UPDATE biz_flow_instance SET prev_instance_id = NULL \
          WHERE prev_instance_id IN \
            (SELECT id FROM biz_flow_instance WHERE biz_type = 'STOCK_TAKE_POST')"
            .to_string(),
        "DELETE FROM biz_flow_instance WHERE biz_type = 'STOCK_TAKE_POST'".to_string(),
        // LNK-04 联动夹具：规则 / 通知（接收人随 notice 级联）/ 探针任务
        "DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK04%'".to_string(),
        "DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:STOCK_TAKE:%'".to_string(),
        "DELETE FROM biz_task WHERE title LIKE 'LNK04 %'".to_string(),
        "DELETE FROM biz_status_log WHERE entity_type = 'STOCK_TAKE'".to_string(),
        "DELETE FROM biz_stock_take WHERE take_no LIKE 'ST-%'".to_string(),
        "DELETE FROM biz_voucher WHERE source_type = 'STOCK_TAKE'".to_string(),
        "DELETE FROM biz_inventory_txn WHERE doc_type = 'STOCK_TAKE' OR material_id IN \
            ('d2110100-0000-4000-8000-000000000011'::uuid, 'd2110100-0000-4000-8000-000000000012'::uuid)"
            .to_string(),
        "DELETE FROM biz_inventory WHERE material_id IN \
            ('d2110100-0000-4000-8000-000000000011'::uuid, 'd2110100-0000-4000-8000-000000000012'::uuid)"
            .to_string(),
        "DELETE FROM biz_inventory_location WHERE material_id IN \
            ('d2110100-0000-4000-8000-000000000011'::uuid, 'd2110100-0000-4000-8000-000000000012'::uuid)"
            .to_string(),
        "DELETE FROM sys_user_role WHERE user_id IN \
           (SELECT id FROM sys_user WHERE username LIKE 'stk_%')"
            .to_string(),
        "DELETE FROM sys_user WHERE username LIKE 'stk_%'".to_string(),
        "DELETE FROM biz_warehouse WHERE id = 'd2110100-0000-4000-8000-000000000002'::uuid".to_string(),
        "DELETE FROM biz_material WHERE id IN \
            ('d2110100-0000-4000-8000-000000000011'::uuid, 'd2110100-0000-4000-8000-000000000012'::uuid)"
            .to_string(),
        "DELETE FROM biz_unit WHERE id = 'd2110100-0000-4000-8000-000000000001'::uuid".to_string(),
    ];
    for sql in cleanups {
        exec(&sql).await;
    }
}

/// 拿锁 + 连库 + 清上一轮 + 建主数据与三个账号。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    purge_fixtures().await;
    for sql in [
        format!(
            "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order) \
             VALUES ('{UNIT}'::uuid, 'DC14-PCS', '个', 1, NULL, 1, 1, 1) ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, allow_negative) \
             VALUES ('{WAREHOUSE}'::uuid, 'DC14-WH', 'DC14 盘点仓', '/{WAREHOUSE}/', 1, 1, 1, 0) ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required) \
             VALUES ('{MATERIAL_A}'::uuid, 'DC14-M1', 'DC14 螺丝', '{UNIT}'::uuid, 1, false), \
                    ('{MATERIAL_B}'::uuid, 'DC14-M2', 'DC14 垫片', '{UNIT}'::uuid, 1, false) \
             ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO sys_user (id, username, password, real_name, status) VALUES \
             ('{U_MAKER}'::uuid, 'stk_maker', 'x', 'DC14 制单人', 1), \
             ('{U_KEEPER}'::uuid, 'stk_keeper', 'x', 'DC14 仓管主管', 1), \
             ('{U_GM}'::uuid, 'stk_gm', 'x', 'DC14 总经理', 1) ON CONFLICT DO NOTHING"
        ),
        format!(
            "INSERT INTO sys_user_role (user_id, role_id) VALUES \
             ('{U_KEEPER}'::uuid, '{ROLE_WM}'::uuid), \
             ('{U_GM}'::uuid, '{ROLE_GM}'::uuid) ON CONFLICT DO NOTHING"
        ),
    ] {
        exec(&sql).await;
    }
    Some(guard)
}

/// 落一笔库存（成本账 + 库位账，未分配库位桶），先清掉该物料旧行。
async fn seed_stock(material_id: &str, qty: &str, unit_cost: &str) {
    let amount = dec(qty) * dec(unit_cost);
    exec(&format!(
        "DELETE FROM biz_inventory WHERE material_id = '{material_id}'::uuid"
    ))
    .await;
    exec(&format!(
        "DELETE FROM biz_inventory_location WHERE material_id = '{material_id}'::uuid"
    ))
    .await;
    exec(&format!(
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount) \
         VALUES (gen_random_uuid(), '{material_id}'::uuid, '{WAREHOUSE}'::uuid, NULL, {qty}, 0, {unit_cost}, {amount})"
    ))
    .await;
    exec(&format!(
        "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty, locked_qty) \
         VALUES (gen_random_uuid(), '{material_id}'::uuid, '{WAREHOUSE}'::uuid, NULL, {qty}, 0)"
    ))
    .await;
}

/// 成本账 `(qty, amount)`。
async fn cost_ledger(material_id: &str) -> (Decimal, Decimal) {
    let row = db()
        .query_first_sql(
            "SELECT coalesce(sum(qty),0) AS qty, coalesce(sum(amount),0) AS amount \
               FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ?::uuid"
                .to_string(),
            vec![Value::from(&uid(material_id)), Value::from(&uid(WAREHOUSE))],
        )
        .await
        .expect("读成本账")
        .expect("聚合必有行");
    (
        row.get_typed("qty").expect("qty"),
        row.get_typed("amount").expect("amount"),
    )
}

/// 库位账合计。
async fn location_ledger(material_id: &str) -> Decimal {
    db().scalar_with_params(
        "SELECT coalesce(sum(qty),0) FROM biz_inventory_location \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&uid(material_id)), Value::from(&uid(WAREHOUSE))],
    )
    .await
    .expect("读库位账")
}

/// 流水累计（IN 加、OUT 减），只算盘点单流水。
async fn take_txn_total(material_id: &str) -> Decimal {
    db().scalar_with_params(
        "SELECT coalesce(sum(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END),0) \
           FROM biz_inventory_txn \
          WHERE doc_type = 'STOCK_TAKE' AND material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&uid(material_id)), Value::from(&uid(WAREHOUSE))],
    )
    .await
    .expect("读流水")
}

/// 建一张全盘单（返回详情）。
async fn make_full_take(remark: &str) -> StockTakeDetailRes {
    let req = CreateStockTakeReq {
        warehouse_id: Some(WAREHOUSE.to_string()),
        take_date: Some(date(TAKE_DATE)),
        take_type: Some("FULL".to_string()),
        remark: Some(remark.to_string()),
        items: Vec::new(),
    };
    data(
        stock_take_create(Extension(AuthClaims(claims_of(U_MAKER))), Json(req))
            .await
            .unwrap_or_else(|err| panic!("建盘点单失败：{}", err.msg)),
    )
}

/// 把某物料的实盘数写进去（按明细物料匹配）。
async fn set_actual(take: &StockTakeDetailRes, material_id: &str, actual: &str) -> StockTakeDetailRes {
    let item = take
        .items
        .iter()
        .find(|item| item.material_id == material_id)
        .unwrap_or_else(|| panic!("盘点单里没有物料 {material_id}"));
    let req = UpdateStockTakeReq {
        take_date: None,
        remark: None,
        lines: vec![super::stocktake_dto::StockTakeActualReq {
            item_id: Some(item.id.clone()),
            actual_qty: Some(dec(actual)),
        }],
    };
    data(
        stock_take_update(
            Extension(AuthClaims(claims_of(U_MAKER))),
            Path(take.header.id.clone()),
            Json(req),
        )
        .await
        .unwrap_or_else(|err| panic!("填实盘数失败：{}", err.msg)),
    )
}

/// 状态流转（成功）。
async fn transition(id: &str, target: &str, reason: Option<&str>) -> StockTakeDetailRes {
    // ATT-06：确认（→ PENDING_POST）前必须已有盘点表 / 盈亏说明，先挂一份 pdf 夹具
    if target == STATUS_PENDING_POST {
        crate::business::attachment::test_support::attach_pdf(
            crate::business::attachment::model::DocKind::StockTake,
            uid(id),
        )
        .await;
    }
    let req = StockTakeStatusReq {
        status: Some(target.to_string()),
        reason: reason.map(|text| text.to_string()),
        ..Default::default()
    };
    data(
        stock_take_change_status(
            Extension(AuthClaims(claims_of(U_MAKER))),
            Path(id.to_string()),
            Json(req),
        )
        .await
        .unwrap_or_else(|err| panic!("状态流转到 {target} 失败：{}", err.msg)),
    )
}

/// 状态流转（期望失败），返回错误。
async fn transition_expect_err(id: &str, target: &str, reason: Option<&str>) -> ApiError {
    let req = StockTakeStatusReq {
        status: Some(target.to_string()),
        reason: reason.map(|text| text.to_string()),
        ..Default::default()
    };
    match stock_take_change_status(
        Extension(AuthClaims(claims_of(U_MAKER))),
        Path(id.to_string()),
        Json(req),
    )
    .await
    {
        Ok(_) => panic!("期望失败，但状态流转到 {target} 成功了"),
        Err(err) => err,
    }
}

/// 某盘点单在跑的审批实例（`PENDING`）。
async fn running_instance(take_id: &str) -> Uuid {
    db().scalar_with_params::<Option<Uuid>>(
        "SELECT id FROM biz_flow_instance \
          WHERE biz_type = 'STOCK_TAKE_POST' AND biz_id = ?::uuid \
            AND status = 'PENDING' AND deleted_at IS NULL",
        vec![Value::from(&uid(take_id))],
    )
    .await
    .expect("查审批实例失败")
    .expect("应当有在跑的审批实例")
}

/// 某实例的 `PENDING` 待办：`(task_id, assignee_id)`。
async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, Uuid)> {
    let rows = db()
        .find_with_params(
            "SELECT id, assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING' ORDER BY id",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("读待办失败");
    rows.iter()
        .map(|row| (row.get_typed("id").unwrap(), row.get_typed("assignee_id").unwrap()))
        .collect()
}

/// 以某人身份同意一条待办（只动引擎；业务回调由测试自己按 `flow::handler` 的顺序调）。
async fn approve_task(task_id: Uuid, operator: Uuid) -> engine::ActionOut {
    let out = db()
        .transaction(|tx| async move { engine::approve(&tx, task_id, operator, Some("同意（DC14 单测）")).await })
        .await
        .unwrap_or_else(|err| panic!("审批同意失败：{err}"));
    stocktake_bridge::on_stock_take_settled(out.instance_id, &out.instance_status).await;
    out
}

/// 以某人身份驳回并终止（`ReturnTo::Final`）。
async fn reject_final(task_id: Uuid, operator: Uuid) -> engine::ActionOut {
    let out = db()
        .transaction(|tx| async move {
            engine::reject(
                &tx,
                task_id,
                operator,
                "驳回：DC14 单测（实盘数与台账差异过大，请复盘后重提）",
                engine::ReturnTo::Final,
            )
            .await
        })
        .await
        .unwrap_or_else(|err| panic!("审批驳回失败：{err}"));
    stocktake_bridge::on_stock_take_settled(out.instance_id, &out.instance_status).await;
    out
}

/// 把一个节点的待办**全部**批掉（同一个审批节点可能派给该角色下的多个用户，
/// 有多少人就有多少条待办 —— 这是引擎的既有行为，不是本卡的差异）。
async fn approve_node(instance: Uuid) -> engine::ActionOut {
    let tasks = pending_tasks(instance).await;
    assert!(!tasks.is_empty(), "当前节点至少应有一条待办");
    let mut last = None;
    for (task_id, assignee) in tasks {
        last = Some(approve_task(task_id, assignee).await);
    }
    last.expect("至少批过一条")
}

/// 提交审批 →（单级）审批通过 → 待过账。
async fn submit_and_approve_single(take: &StockTakeDetailRes) -> StockTakeDetailRes {
    let submitted = transition(&take.header.id, STATUS_PENDING_POST, None).await;
    let instance = running_instance(&take.header.id).await;
    let out = approve_node(instance).await;
    assert!(
        out.instance_status.eq_ignore_ascii_case("APPROVED"),
        "单级审批批完应当 APPROVED（实际 {}）",
        out.instance_status
    );
    submitted
}

/// 凭证分录：`(科目编码, 借方, 贷方)`，按凭证 id 读。
async fn voucher_entries(voucher_id: &str) -> Vec<(String, Decimal, Decimal)> {
    let rows = db()
        .find_with_params(
            "SELECT a.account_code, e.debit_amount, e.credit_amount \
               FROM biz_voucher_entry e JOIN biz_account a ON a.id = e.account_id \
              WHERE e.voucher_id = ?::uuid ORDER BY e.line_no",
            vec![Value::from(&uid(voucher_id))],
        )
        .await
        .expect("读凭证分录");
    rows.iter()
        .map(|row| {
            (
                row.get_typed("account_code").unwrap(),
                row.get_typed("debit_amount").unwrap(),
                row.get_typed("credit_amount").unwrap(),
            )
        })
        .collect()
}

// ============================================================================
// 纯逻辑用例（不连库）
// ============================================================================

/// ① 状态白名单 / 中文名 / 边表与状态机同源。
#[test]
fn status_and_edges_are_consistent() {
    use crate::business::shared::state_machine;
    assert_eq!(model::status_label(STATUS_DRAFT), "草稿");
    assert_eq!(model::status_label(STATUS_PENDING_POST), "待过账");
    assert_eq!(model::status_label(STATUS_POSTED), "已过账");
    assert_eq!(model::status_label(STATUS_CANCELLED), "已作废");
    assert_eq!(model::TAKE_TYPES, ["FULL", "PARTIAL"]);
    assert_eq!(model::take_type_label("FULL"), "全盘");
    assert_eq!(model::take_type_label("PARTIAL"), "抽盘");

    let entity = state_machine::Entity::StockTake;
    assert_eq!(entity.as_str(), "STOCK_TAKE");
    assert_eq!(entity.label(), "盘点单");
    assert_eq!(entity.edges(), &model::LEGAL_EDGES[..]);
    // 六条边逐条钉死
    for (from, to) in [
        ("DRAFT", "PENDING_POST"),
        ("PENDING_POST", "POSTED"),
        ("PENDING_POST", "DRAFT"),
        ("DRAFT", "CANCELLED"),
        ("PENDING_POST", "CANCELLED"),
        ("POSTED", "CANCELLED"),
    ] {
        assert!(entity.can_transition(from, to), "{from} → {to} 必须在边表里");
    }
    // 「没人批就改账」这条路必须不存在
    assert!(
        !entity.can_transition("DRAFT", "POSTED"),
        "DRAFT → POSTED 必须禁止（盘点必须先审批再过账）"
    );
    assert!(entity.needs_reason("POSTED", "CANCELLED"), "已过账作废必须写原因");
    assert!(entity.is_locked("POSTED") && entity.is_locked("PENDING_POST"));
}

/// ② 差异金额合计 / 差异数量合计的口径。
#[test]
fn diff_totals_are_gain_plus_loss() {
    let row = model::StockTakeRow {
        id: Uuid::nil(),
        take_no: "ST-TEST".to_string(),
        warehouse_id: Uuid::nil(),
        take_date: date(TAKE_DATE),
        take_type: "FULL".to_string(),
        status: STATUS_POSTED.to_string(),
        approval_instance_id: None,
        voucher_id: None,
        reverse_voucher_id: None,
        total_book_qty: dec("110"),
        total_actual_qty: dec("105"),
        total_gain_qty: dec("3"),
        total_loss_qty: dec("8"),
        total_gain_amount: dec("15.0000"),
        total_loss_amount: dec("64.0000"),
        remark: None,
        posted_at: None,
        posted_by: None,
        cancelled_at: None,
        cancelled_by: None,
        cancel_reason: None,
        created_at: nik::prelude::chrono::Utc::now(),
        updated_at: nik::prelude::chrono::Utc::now(),
        create_by: None,
        update_by: None,
    };
    assert_eq!(row.total_diff_amount(), dec("79"));
    assert_eq!(row.total_diff_qty(), dec("11"));
}

/// ③ 全盘不接受 items / 抽盘必须给 items / 抽盘重复行在 handler 里被拦。
#[test]
fn scope_validation_rejects_bad_requests() {
    let mut req = CreateStockTakeReq {
        warehouse_id: Some(WAREHOUSE.to_string()),
        take_type: Some("FULL".to_string()),
        items: vec![StockTakeScopeItemReq {
            material_id: Some(MATERIAL_A.to_string()),
            location_id: None,
        }],
        ..Default::default()
    };
    assert!(
        req.scope_items("FULL").is_err(),
        "全盘不接受 items（应按仓库库存桶自动快照）"
    );
    req.take_type = Some("PARTIAL".to_string());
    assert!(req.scope_items("PARTIAL").is_ok());
    req.items.clear();
    assert!(req.scope_items("PARTIAL").is_err(), "抽盘必须给 items");
    assert!(req.take_type_value().is_ok());
    req.take_type = Some("SOMETHING".to_string());
    assert!(req.take_type_value().is_err(), "未知盘点类型必须 400");
}

/// ④ 实盘数不能为负 / 状态目标白名单 / 状态别名矛盾。
#[test]
fn payload_validation() {
    use super::stocktake_dto::{StockTakeActualReq, StockTakeStatusReq, UpdateStockTakeReq};
    let bad = StockTakeActualReq {
        item_id: Some(Uuid::now_v7().to_string()),
        actual_qty: Some(dec("-1")),
    };
    assert!(bad.parse(0).is_err(), "负实盘数必须 400");
    let missing = StockTakeActualReq {
        item_id: None,
        actual_qty: Some(dec("1")),
    };
    assert!(missing.parse(0).is_err(), "缺 item_id 必须 400");
    let update = UpdateStockTakeReq::default();
    assert!(update.parsed_lines().unwrap().is_empty(), "不传 lines = 只改单头");

    let req = StockTakeStatusReq {
        status: Some("posted".to_string()),
        ..Default::default()
    };
    assert_eq!(req.target().unwrap(), "POSTED", "小写应被归一化为大写");
    let req = StockTakeStatusReq {
        status: Some("POSTED".to_string()),
        target_status: Some("DRAFT".to_string()),
        ..Default::default()
    };
    assert!(req.target().is_err(), "两个别名矛盾必须 400");
    let req = StockTakeStatusReq {
        status: Some("APPROVED".to_string()),
        ..Default::default()
    };
    assert!(req.target().is_err(), "非白名单状态必须 400");
}

// ============================================================================
// 真库用例
// ============================================================================

/// ⑤ 全盘建单：账面数按库存桶快照、实盘数预填账面数、差异为 0。
#[tokio::test]
async fn full_take_snapshots_book_qty() {
    let Some(_guard) = setup().await else { return };
    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;

    let take = make_full_take("全盘快照用例").await;
    assert_eq!(take.header.take_type, "FULL");
    assert_eq!(take.header.status, STATUS_DRAFT);
    assert_eq!(take.items.len(), 2, "该仓 2 个库存桶 → 2 行明细");
    for item in &take.items {
        assert_eq!(item.book_qty, item.actual_qty, "实盘数应预填账面数（差异 0）");
        assert_eq!(dec(&item.diff_qty), Decimal::ZERO);
    }
    assert_eq!(dec(&take.header.total_book_qty), dec("110"));
    assert_eq!(dec(&take.header.total_diff_amount), Decimal::ZERO);
}

/// ⑥ 盘亏过账：库存 100 → 92，三本账同步，凭证 借 1901 / 贷 1405。
#[tokio::test]
async fn loss_posts_three_books_and_voucher() {
    let Some(_guard) = setup().await else { return };
    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;

    let take = make_full_take("盘亏用例").await;
    let take = set_actual(&take, MATERIAL_A, "92").await;
    assert_eq!(dec(&take.header.total_book_qty), dec("110"));
    assert_eq!(dec(&take.header.total_actual_qty), dec("102"));

    let _ = submit_and_approve_single(&take).await;
    let posted = transition(&take.header.id, STATUS_POSTED, None).await;
    assert_eq!(posted.header.status, STATUS_POSTED);

    // 成本账 / 库位账 / 流水
    let (qty, amount) = cost_ledger(MATERIAL_A).await;
    assert_eq!(qty, dec("92"), "账面 100 盘亏 8 → 92");
    assert_eq!(amount, dec("736"), "金额按账上真实差额扣减（8×8=64）");
    assert_eq!(location_ledger(MATERIAL_A).await, dec("92"));
    assert_eq!(
        take_txn_total(MATERIAL_A).await,
        dec("-8"),
        "盘点流水净额 = 差异（盘亏 8 记 OUT）"
    );
    // 未盘动的物料不受影响
    let (qty_b, _) = cost_ledger(MATERIAL_B).await;
    assert_eq!(qty_b, dec("10"), "无差异的行不许动账");

    // 凭证：借 1901 / 贷 1405，各 64
    let voucher_id = posted.header.voucher_id.clone().expect("过账必须生成凭证");
    let entries = voucher_entries(&voucher_id).await;
    assert_eq!(entries.len(), 2, "一行差异 → 两条分录");
    let debit_1901 = entries
        .iter()
        .find(|(code, _, _)| code == "1901")
        .expect("必须有 1901 借方");
    assert_eq!(debit_1901.1, dec("64"));
    let credit_1405 = entries
        .iter()
        .find(|(code, _, _)| code == "1405")
        .expect("必须有 1405 贷方");
    assert_eq!(credit_1405.2, dec("64"));

    assert_eq!(dec(&posted.header.total_loss_qty), dec("8"));
    assert_eq!(dec(&posted.header.total_loss_amount), dec("64"));
    assert_eq!(dec(&posted.header.total_gain_qty), Decimal::ZERO);
}

/// ⑦ 盘盈过账：库存 10 → 13，行入库方向，凭证 借 1405 / 贷 1901。
#[tokio::test]
async fn gain_books_inbound_direction() {
    let Some(_guard) = setup().await else { return };
    seed_stock(MATERIAL_A, "10", "5").await;
    seed_stock(MATERIAL_B, "10", "5").await;

    let take = make_full_take("盘盈用例").await;
    let take = set_actual(&take, MATERIAL_A, "13").await;
    let _ = submit_and_approve_single(&take).await;
    let posted = transition(&take.header.id, STATUS_POSTED, None).await;

    let (qty, amount) = cost_ledger(MATERIAL_A).await;
    assert_eq!(qty, dec("13"), "账面 10 + 盘盈 3 = 13");
    assert_eq!(amount, dec("65"), "盘盈按账面成本快照入账（3×5=15 → 50+15）");
    assert_eq!(location_ledger(MATERIAL_A).await, dec("13"));
    assert_eq!(take_txn_total(MATERIAL_A).await, dec("3"), "盘盈记 IN 3");

    let voucher_id = posted.header.voucher_id.clone().expect("盘盈必须生成凭证");
    let entries = voucher_entries(&voucher_id).await;
    let debit_1405 = entries
        .iter()
        .find(|(code, _, _)| code == "1405")
        .expect("必须有 1405 借方");
    assert_eq!(debit_1405.1, dec("15"), "盘盈 借 1405");
    let credit_1901 = entries
        .iter()
        .find(|(code, _, _)| code == "1901")
        .expect("必须有 1901 贷方");
    assert_eq!(credit_1901.2, dec("15"), "盘盈 贷 1901");
    assert_eq!(dec(&posted.header.total_gain_qty), dec("3"));
    assert_eq!(dec(&posted.header.total_gain_amount), dec("15"));
}

/// ⑧ 无差异过账：不动三本账、不出凭证。
#[tokio::test]
async fn no_diff_posts_without_any_entry() {
    let Some(_guard) = setup().await else { return };
    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;

    let take = make_full_take("无差异用例").await;
    let _ = submit_and_approve_single(&take).await;
    let posted = transition(&take.header.id, STATUS_POSTED, None).await;

    let (qty, amount) = cost_ledger(MATERIAL_A).await;
    assert_eq!((qty, amount), (dec("100"), dec("800")), "无差异不许动账");
    assert_eq!(take_txn_total(MATERIAL_A).await, dec("0"), "无差异不写流水");
    assert!(
        posted.header.voucher_id.is_none(),
        "金额 0 不构成凭证（post_voucher 会拒），应跳过而不是报错"
    );
    assert_eq!(dec(&posted.header.total_gain_amount), Decimal::ZERO);
    assert_eq!(dec(&posted.header.total_loss_amount), Decimal::ZERO);
}

/// ⑨ 已过账作废：三本账回基线 + 凭证红冲。
#[tokio::test]
async fn cancel_after_post_rolls_back_and_reds_voucher() {
    let Some(_guard) = setup().await else { return };
    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;

    let take = make_full_take("作废回滚用例").await;
    let take = set_actual(&take, MATERIAL_A, "92").await;
    let _ = submit_and_approve_single(&take).await;
    let posted = transition(&take.header.id, STATUS_POSTED, None).await;
    let voucher_id = posted.header.voucher_id.clone().expect("过账凭证");

    let cancelled = transition(
        &take.header.id,
        STATUS_CANCELLED,
        Some("年底复盘发现实盘数填错，作废重盘"),
    )
    .await;
    assert_eq!(cancelled.header.status, STATUS_CANCELLED);

    // 三本账回基线
    let (qty, amount) = cost_ledger(MATERIAL_A).await;
    assert_eq!(qty, dec("100"), "作废后库存回到 100");
    assert_eq!(amount, dec("800"), "作废后金额回到 800");
    assert_eq!(location_ledger(MATERIAL_A).await, dec("100"));
    assert_eq!(take_txn_total(MATERIAL_A).await, dec("0"), "正向 + 冲销流水净额为 0");

    // 红字凭证：is_red = 1、reverse_of = 原凭证、借贷互换
    let red_id = cancelled.header.reverse_voucher_id.clone().expect("必须红冲");
    let row = db()
        .query_first_sql(
            "SELECT is_red, reverse_of FROM biz_voucher WHERE id = ?::uuid".to_string(),
            vec![Value::from(&uid(&red_id))],
        )
        .await
        .expect("读红字凭证")
        .expect("红字凭证应存在");
    let is_red: i16 = row.get_typed("is_red").unwrap();
    let reverse_of: Option<Uuid> = row.get_typed("reverse_of").unwrap();
    assert_eq!(is_red, 1);
    assert_eq!(reverse_of, Some(uid(&voucher_id)));
    let entries = voucher_entries(&red_id).await;
    let credit_1901 = entries
        .iter()
        .find(|(code, _, _)| code == "1901")
        .expect("红字必须有 1901 贷方（冲原借方）");
    assert_eq!(credit_1901.2, dec("64"));
    let debit_1405 = entries
        .iter()
        .find(|(code, _, _)| code == "1405")
        .expect("红字必须有 1405 借方（冲原贷方）");
    assert_eq!(debit_1405.1, dec("64"));
}

/// ⑩ 负库存拦截：建单后库存被别处消耗 → 过账必须 409 且整笔回滚。
#[tokio::test]
async fn negative_stock_is_blocked() {
    let Some(_guard) = setup().await else { return };
    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;

    let take = make_full_take("负库存用例").await;
    let take = set_actual(&take, MATERIAL_A, "92").await;
    let _ = submit_and_approve_single(&take).await;

    // 建单之后、过账之前，库存被别的单据消耗掉了：账面只剩 4（不够亏 8）
    exec(&format!(
        "UPDATE biz_inventory SET qty = 4 WHERE material_id = '{MATERIAL_A}'::uuid"
    ))
    .await;
    exec(&format!(
        "UPDATE biz_inventory_location SET qty = 4 WHERE material_id = '{MATERIAL_A}'::uuid"
    ))
    .await;

    let err = transition_expect_err(&take.header.id, STATUS_POSTED, None).await;
    assert_eq!(err.code, error_code::ERP_STOCK_OUT_NEGATIVE, "必须 409 + 负库存码");
    assert!(
        err.msg.contains("盘亏") && err.msg.contains("4"),
        "报文要点名发生了什么（盘亏多少 / 账面剩多少）：{}",
        err.msg
    );

    // 整笔回滚：状态仍是待过账，两本账没动
    let row = model::find(db(), uid(&take.header.id))
        .await
        .expect("读盘点单")
        .expect("盘点单应存在");
    assert_eq!(row.status, STATUS_PENDING_POST, "失败必须回滚状态");
    let (qty, _) = cost_ledger(MATERIAL_A).await;
    assert_eq!(qty, dec("4"), "失败必须回滚库存（不许把库存冲成负数）");
}

/// ⑪ 未审批不许过账（提交后直接过账 → 409）。
#[tokio::test]
async fn post_requires_approved_instance() {
    let Some(_guard) = setup().await else { return };
    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;

    let take = make_full_take("未审批过账用例").await;
    let take = set_actual(&take, MATERIAL_A, "92").await;
    let submitted = transition(&take.header.id, STATUS_PENDING_POST, None).await;
    assert!(submitted.header.approval_instance_id.is_some(), "提交审批后必须挂实例");

    let err = transition_expect_err(&take.header.id, STATUS_POSTED, None).await;
    assert_eq!(err.code, error_code::RESOURCE_CONFLICT);
    assert!(err.msg.contains("审批尚未通过"), "报文要点名审批没过：{}", err.msg);
    let (qty, _) = cost_ledger(MATERIAL_A).await;
    assert_eq!(qty, dec("100"), "被拦下的过账不许动账");
}

/// ⑫ 驳回：审批终止 → 单据回草稿，实盘数可再改（账一分没动）。
#[tokio::test]
async fn reject_returns_to_draft() {
    let Some(_guard) = setup().await else { return };
    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;

    let take = make_full_take("驳回用例").await;
    let take = set_actual(&take, MATERIAL_A, "92").await;
    let _ = transition(&take.header.id, STATUS_PENDING_POST, None).await;
    let instance = running_instance(&take.header.id).await;
    let tasks = pending_tasks(instance).await;
    assert!(!tasks.is_empty());
    reject_final(tasks[0].0, tasks[0].1).await;

    let detail = data(stock_take_detail(Path(take.header.id.clone())).await.expect("读详情"));
    assert_eq!(detail.header.status, STATUS_DRAFT, "驳回必须回草稿");
    assert!(
        detail.header.approval_instance_id.is_none(),
        "回位必须清空实例（否则下次提交撞唯一索引）"
    );
    let (qty, _) = cost_ledger(MATERIAL_A).await;
    assert_eq!(qty, dec("100"), "驳回不许动账");

    // 回草稿后实盘数能改（这是回草稿的意义）
    let again = set_actual(&detail, MATERIAL_A, "95").await;
    assert_eq!(again.header.status, STATUS_DRAFT);
    let item = again.items.iter().find(|item| item.material_id == MATERIAL_A).unwrap();
    assert_eq!(dec(&item.diff_qty), dec("-5"), "改完实盘数差异必须立刻反映");
}

/// ⑬ 审批分档：差异金额 ≥ 1 万走两级（串行），两级都通过才能过账。
#[tokio::test]
async fn large_diff_takes_two_approval_levels() {
    let Some(_guard) = setup().await else { return };
    // 100 件 × 200 = 20000：盘亏 10 件 = 2000 元 → 未到 1 万；盘亏 60 件 = 12000 → 两级
    seed_stock(MATERIAL_A, "100", "200").await;
    seed_stock(MATERIAL_B, "10", "5").await;

    let take = make_full_take("大额差异用例").await;
    let take = set_actual(&take, MATERIAL_A, "40").await;
    let _ = transition(&take.header.id, STATUS_PENDING_POST, None).await;
    let instance = running_instance(&take.header.id).await;

    // 第一级：仓管主管（n_fin）—— 批完仍在跑，说明还有第二级
    let first = approve_node(instance).await;
    assert!(
        first.instance_status.eq_ignore_ascii_case("PENDING"),
        "第一级通过后实例仍应在跑（还有总经理那一级），实际 {}",
        first.instance_status
    );
    // 第二级：总经理（n_gm）—— **串行**出现（不是会签：第一级没批完不会有第二级待办）
    let second = approve_node(instance).await;
    assert!(
        second.instance_status.eq_ignore_ascii_case("APPROVED"),
        "两级都批完才 APPROVED（实际 {}）",
        second.instance_status
    );

    // 审批通过后才能过账
    let posted = transition(&take.header.id, STATUS_POSTED, None).await;
    assert_eq!(posted.header.status, STATUS_POSTED);
    let (qty, _) = cost_ledger(MATERIAL_A).await;
    assert_eq!(qty, dec("40"));
    assert_eq!(dec(&posted.header.total_loss_qty), dec("60"));
    assert_eq!(dec(&posted.header.total_loss_amount), dec("12000"));
}

/// ⑭ 抽盘只盘指定物料（其余库存行不进明细）。
#[tokio::test]
async fn partial_scope_only_includes_requested_material() {
    let Some(_guard) = setup().await else { return };
    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;

    let req = CreateStockTakeReq {
        warehouse_id: Some(WAREHOUSE.to_string()),
        take_date: Some(date(TAKE_DATE)),
        take_type: Some("PARTIAL".to_string()),
        remark: Some("抽盘用例".to_string()),
        items: vec![StockTakeScopeItemReq {
            material_id: Some(MATERIAL_A.to_string()),
            location_id: None,
        }],
    };
    let take: StockTakeDetailRes = data(
        stock_take_create(Extension(AuthClaims(claims_of(U_MAKER))), Json(req))
            .await
            .expect("抽盘建单"),
    );
    assert_eq!(take.items.len(), 1, "抽盘只含指定物料");
    assert_eq!(take.items[0].material_id, MATERIAL_A);
    assert_eq!(take.header.take_type, "PARTIAL");
    assert_eq!(dec(&take.header.total_book_qty), dec("100"));

    // 重复行必须被拦（同一桶只能盘一次）
    let req = CreateStockTakeReq {
        warehouse_id: Some(WAREHOUSE.to_string()),
        take_date: Some(date(TAKE_DATE)),
        take_type: Some("PARTIAL".to_string()),
        remark: None,
        items: vec![
            StockTakeScopeItemReq {
                material_id: Some(MATERIAL_A.to_string()),
                location_id: None,
            },
            StockTakeScopeItemReq {
                material_id: Some(MATERIAL_A.to_string()),
                location_id: None,
            },
        ],
    };
    let claims = claims_of(U_MAKER);
    let err = create_stock_take(&req, &claims).await.expect_err("重复行必须被拒");
    assert!(err.msg.contains("重复"), "报文要点名重复行：{}", err.msg);
}

/// ⑮ 状态机铁律：草稿不能直接过账、已过账不能改实盘数、只有草稿能删。
#[tokio::test]
async fn illegal_transitions_are_rejected() {
    let Some(_guard) = setup().await else { return };
    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;

    let take = make_full_take("非法流转用例").await;
    // 草稿 → 已过账
    let err = transition_expect_err(&take.header.id, STATUS_POSTED, None).await;
    assert_eq!(err.code, error_code::RESOURCE_CONFLICT);
    assert!(err.msg.contains("不能转到"), "报文要解释为什么：{}", err.msg);

    // 过账后改实盘数
    let take = set_actual(&take, MATERIAL_A, "92").await;
    let _ = submit_and_approve_single(&take).await;
    let posted = transition(&take.header.id, STATUS_POSTED, None).await;
    let err = update_stock_take(
        uid(&posted.header.id),
        &UpdateStockTakeReq {
            take_date: None,
            remark: Some("想改".to_string()),
            lines: vec![super::stocktake_dto::StockTakeActualReq {
                item_id: Some(posted.items[0].id.clone()),
                actual_qty: Some(dec("90")),
            }],
        },
        &claims_of(U_MAKER),
    )
    .await
    .expect_err("已过账不许改实盘数");
    assert!(err.msg.contains("只有草稿能改"), "{}", err.msg);

    // 已过账不许删
    let err = stock_take_delete(
        Extension(AuthClaims(claims_of(U_MAKER))),
        Path(posted.header.id.clone()),
    )
    .await
    .expect_err("已过账不许删");
    assert!(err.msg.contains("只有草稿能删"), "{}", err.msg);

    // 列表能查到这张单
    let page = data(
        stock_take_page(Query(super::stocktake_dto::StockTakePageQuery {
            status: Some(STATUS_POSTED.to_string()),
            ..Default::default()
        }))
        .await
        .expect("分页"),
    );
    assert!(page.total >= 1);
    assert!(page.list.iter().any(|row| row.take_no == posted.header.take_no));
}

/// ⑯ 草稿删除 + 审批中撤回：都能终止在跑的审批（不留孤儿实例）。
#[tokio::test]
async fn withdraw_and_delete_terminate_running_approval() {
    let Some(_guard) = setup().await else { return };
    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;

    let take = make_full_take("撤回用例").await;
    let take = set_actual(&take, MATERIAL_A, "92").await;
    let _ = transition(&take.header.id, STATUS_PENDING_POST, None).await;
    let _ = running_instance(&take.header.id).await;

    // 撤回：回草稿 + 实例终止
    let back = transition(&take.header.id, STATUS_DRAFT, Some("实盘数还要复核")).await;
    assert_eq!(back.header.status, STATUS_DRAFT);
    let running: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_flow_instance \
              WHERE biz_type = 'STOCK_TAKE_POST' AND biz_id = ?::uuid AND status = 'PENDING'",
            vec![Value::from(&uid(&take.header.id))],
        )
        .await
        .expect("统计在跑实例");
    assert_eq!(running, 0, "撤回必须终止在跑实例（否则下次提交撞唯一索引）");

    // 删：草稿可删，删完详情 404
    stock_take_delete(Extension(AuthClaims(claims_of(U_MAKER))), Path(take.header.id.clone()))
        .await
        .expect("草稿删除应成功");
    let missing = stock_take_detail(Path(take.header.id.clone())).await;
    assert!(missing.is_err(), "删掉的单详情必须 404");
}

// ==================== LNK-04：盘点单联动接线（真库） ====================
//
// 这一组用例回答的是「配一条规则，盘点过账 / 作废它真的执行，而且**不重复落账**」。
// 纯逻辑守卫（`linkage::tests_wiring::stocktake_wires_*`）只能证明源码里有调用，
// 证明不了：
//
// - 规则读得到 → 事务内动作落库 → commit 之后通知真的发出去（`sys_notice` 有行）；
// - 盘盈盘亏只落一次账（成本账 = 实盘、流水净额 = 差异、凭证恰好一张）；
// - 重复过账（第二次点「过账」）不会第二次触发联动；
// - 排除清单里的 `WRITE_INVENTORY` / `GENERATE_VOUCHER` 确实没跑（反证：规则真的读到了）。
//
// 夹具规则名 `LNK04-*`、通知幂等键 `LNK:STOCK_TAKE:*`、探针任务标题 `LNK04 *`，
// 由 `purge_fixtures()` 与每个用例开头/结尾各自清一遍（软删的规则仍占唯一索引坑位，
// 只能硬删）。

/// 本卡联动夹具的规则名前缀。
const LNK04: &str = "LNK04";

/// 插一条 `STOCK_TAKE/<status>` 上的规则（绕过 API 写库：与「管理员在配置页配好」等价）。
async fn add_linkage_rule(status: &str, action: &str, params: &str) {
    exec(&format!(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, seq) \
         VALUES (gen_random_uuid(), '{LNK04}-{status}-{action}', 'STOCK_TAKE', '{status}', \
                 '{action}', '{params}'::jsonb, 10)"
    ))
    .await;
}

/// 数一行（失败即 panic）。
async fn scalar_i64(sql: &str, params: Vec<Value>, label: &str) -> i64 {
    db().scalar_with_params::<i64>(sql, params)
        .await
        .unwrap_or_else(|error| panic!("{label}：{error}"))
}

/// 按幂等键数通知条数。
async fn notices_of_key(key: &str) -> i64 {
    scalar_i64(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key = ?",
        vec![Value::Text(key.to_string())],
        "查联动通知",
    )
    .await
}

/// 某个盘点单名下的差异凭证条数（**应当是 1**：`post_tx` ⑤ 那一张）。
async fn take_vouchers(take_id: &str) -> i64 {
    scalar_i64(
        "SELECT count(*) FROM biz_voucher \
          WHERE source_type = 'STOCK_TAKE' AND source_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&uid(take_id))],
        "查盘点凭证",
    )
    .await
}

/// 本卡探针任务条数（`CREATE_TASK` 没有幂等键，多跑一次就多一行 —— 比通知更能证明
/// 「引擎真的只跑了一次」）。
async fn probe_tasks() -> i64 {
    scalar_i64(
        "SELECT count(*) FROM biz_task WHERE title = 'LNK04 盘点复核'",
        vec![],
        "查联动任务",
    )
    .await
}

/// ⑱ 过账触发联动：`STOCK_TAKE/POSTED` 上的 `SEND_NOTICE` 在 commit 之后真的发出来，
/// 且三本账与实盘一致（盘点后库存 = 实盘数）。
#[tokio::test]
async fn lnk04_post_triggers_linkage_notice() {
    let Some(_guard) = setup().await else { return };
    // SEND_NOTICE 是 AfterCommit 动作：必须由 commit 之后的 run_deferred_after_commit 执行
    add_linkage_rule(
        STATUS_POSTED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;
    let take = make_full_take("LNK04 过账触发").await;
    let take = set_actual(&take, MATERIAL_A, "92").await;
    let _ = submit_and_approve_single(&take).await;
    let posted = transition(&take.header.id, STATUS_POSTED, None).await;
    assert_eq!(posted.header.status, STATUS_POSTED);

    let key = format!("LNK:STOCK_TAKE:{STATUS_POSTED}:{}", take.header.id);
    assert_eq!(
        notices_of_key(&key).await,
        1,
        "STOCK_TAKE/POSTED 上配的 SEND_NOTICE 必须真的发出一条通知（幂等键 {key}）—— \
         只改 spec.rs 的 wired 标记、handler 不调引擎时这里会是 0"
    );
    let rows = db()
        .find_with_params(
            "SELECT title, biz_type, biz_id, link_url, sender_name FROM sys_notice \
              WHERE idempotency_key = ?",
            vec![Value::Text(key.clone())],
        )
        .await
        .expect("查通知行");
    let notice = rows.first().expect("通知行");
    assert_eq!(
        notice.get_typed::<String>("title").expect("title"),
        "盘点单已过账",
        "标题来自触发点基稿（目标态中文名），不是配置凭空生成"
    );
    assert_eq!(notice.get_typed::<String>("biz_type").expect("biz_type"), "STOCK_TAKE");
    assert_eq!(notice.get_typed::<String>("biz_id").expect("biz_id"), take.header.id);
    assert!(
        notice
            .get_typed::<String>("link_url")
            .expect("link_url")
            .contains(&take.header.id),
        "通知必须链回盘点单详情"
    );

    // 三账一致：成本账 / 库位账 / 流水都按实盘数落过账，且只落一次
    let (qty, amount) = cost_ledger(MATERIAL_A).await;
    assert_eq!(qty, dec("92"), "盘点后库存 = 实盘 92（不是 84）");
    assert_eq!(amount, dec("736"));
    assert_eq!(location_ledger(MATERIAL_A).await, dec("92"));
    assert_eq!(take_txn_total(MATERIAL_A).await, dec("-8"));
    assert_eq!(
        take_vouchers(&take.header.id).await,
        1,
        "一张盘点单恰好一张差异凭证（联动不许再生成一张）"
    );
}

/// ⑲ 已过账作废触发联动：`STOCK_TAKE/CANCELLED` 的通知在 commit 之后发出来，
/// 且三本账回滚到基线（库里不残留「作废了但账还在」）。
#[tokio::test]
async fn lnk04_cancel_after_post_triggers_linkage_notice() {
    let Some(_guard) = setup().await else { return };
    add_linkage_rule(
        STATUS_CANCELLED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;
    let take = make_full_take("LNK04 作废触发").await;
    let take = set_actual(&take, MATERIAL_A, "92").await;
    let _ = submit_and_approve_single(&take).await;
    let posted = transition(&take.header.id, STATUS_POSTED, None).await;
    assert_eq!(take_vouchers(&take.header.id).await, 1);

    let cancelled = transition(
        &take.header.id,
        STATUS_CANCELLED,
        Some("LNK04 用例：实盘数填错，作废重盘"),
    )
    .await;
    assert_eq!(cancelled.header.status, STATUS_CANCELLED);

    let key = format!("LNK:STOCK_TAKE:{STATUS_CANCELLED}:{}", take.header.id);
    assert_eq!(
        notices_of_key(&key).await,
        1,
        "作废这条边必须触发联动（本卡接线前它永远不响）"
    );
    let rows = db()
        .find_with_params(
            "SELECT title FROM sys_notice WHERE idempotency_key = ?",
            vec![Value::Text(key.clone())],
        )
        .await
        .expect("查作废通知");
    assert_eq!(
        rows.first()
            .expect("作废通知行")
            .get_typed::<String>("title")
            .expect("title"),
        "盘点单已作废"
    );

    // 作废把三本账原路回滚（正向流水 + 冲销流水净额为 0）
    let (qty, amount) = cost_ledger(MATERIAL_A).await;
    assert_eq!((qty, amount), (dec("100"), dec("800")), "作废后库存回基线");
    assert_eq!(location_ledger(MATERIAL_A).await, dec("100"));
    assert_eq!(take_txn_total(MATERIAL_A).await, dec("0"));
    assert!(posted.header.voucher_id.is_some(), "过账那张蓝字凭证还在");
}

/// ⑳ 未过账作废（草稿 → 已作废）也触发联动：三条来源态里最容易漏的一条
/// （`cancel_tx` 在「账没动过」处提前 return，不接线就只有 POSTED 那条能响）。
#[tokio::test]
async fn lnk04_cancel_draft_triggers_linkage_notice() {
    let Some(_guard) = setup().await else { return };
    add_linkage_rule(
        STATUS_CANCELLED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;
    let take = make_full_take("LNK04 草稿作废").await;

    let cancelled = transition(
        &take.header.id,
        STATUS_CANCELLED,
        Some("LNK04 用例：建错仓库，草稿作废"),
    )
    .await;
    assert_eq!(cancelled.header.status, STATUS_CANCELLED);

    assert_eq!(
        notices_of_key(&format!("LNK:STOCK_TAKE:{STATUS_CANCELLED}:{}", take.header.id)).await,
        1,
        "草稿作废也要触发联动（cancel_tx 的提前 return 分支漏接就没这一条）"
    );
    // 账没动过：库位账与流水都不该有任何盘点痕迹
    assert_eq!(take_txn_total(MATERIAL_A).await, dec("0"));
    assert_eq!(take_vouchers(&take.header.id).await, 0);
}

/// ㉑ 重复过账不重复触发：第二次点「过账」被状态闸挡住（409），联动不会跑第二次。
#[tokio::test]
async fn lnk04_repeat_post_does_not_trigger_twice() {
    let Some(_guard) = setup().await else { return };
    // CREATE_TASK 没有幂等键兜底：重复触发一次就多一个任务行
    add_linkage_rule(
        STATUS_POSTED,
        spec::ACTION_CREATE_TASK,
        r#"{"title":"LNK04 盘点复核","assignee_strategy":"OPERATOR"}"#,
    )
    .await;

    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;
    let take = make_full_take("LNK04 重复过账").await;
    let take = set_actual(&take, MATERIAL_A, "92").await;
    let _ = submit_and_approve_single(&take).await;
    let _ = transition(&take.header.id, STATUS_POSTED, None).await;
    assert_eq!(probe_tasks().await, 1, "正常触发一次：建 1 个任务");

    // 第二次过账：状态已经是 POSTED，过账闸 409（账只动一次）
    let err = transition_expect_err(&take.header.id, STATUS_POSTED, None).await;
    assert_eq!(err.status, 409, "已过账的单再过账必须 409，实际：{}", err.msg);
    assert_eq!(
        probe_tasks().await,
        1,
        "被拦下的第二次过账不许再触发一次联动（任务数从 1 变 2 = 引擎被重复调用）"
    );
    // 库存也只落过一次
    let (qty, _) = cost_ledger(MATERIAL_A).await;
    assert_eq!(qty, dec("92"), "重复过账不许把库存再扣一次");
}

/// ㉒ 排除动作确实不跑：配了 `WRITE_INVENTORY` / `GENERATE_VOUCHER` 也不重复落账。
///
/// 反证分两层：
///
/// 1. **已实现动作**（`GENERATE_VOUCHER`，TAKEOVER 会真落库）→ 盘点单名下凭证仍然只有
///    `post_tx` ⑤ 那一张；
/// 2. **占位动作**（`WRITE_INVENTORY` 尚未接管）→ 排除清单生效时引擎**不产生任何
///    `RuleRun`**；把排除清单去掉后同一份规则会被读到（`Skipped` 或关键动作 `Err`）
///    —— 否则上面的「没重复写」只是「规则根本没读到」的假绿。
#[tokio::test]
async fn lnk04_excluded_inventory_actions_do_not_double_write() {
    let Some(_guard) = setup().await else { return };
    add_linkage_rule(
        STATUS_POSTED,
        "WRITE_INVENTORY",
        r#"{"direction":"INCREASE","qty":"1000"}"#,
    )
    .await;
    add_linkage_rule(
        STATUS_POSTED,
        spec::ACTION_GENERATE_VOUCHER,
        r#"{"biz_type":"STOCK_TAKE","mode":"TAKEOVER"}"#,
    )
    .await;

    seed_stock(MATERIAL_A, "100", "8").await;
    seed_stock(MATERIAL_B, "10", "5").await;
    let take = make_full_take("LNK04 排除动作").await;
    let take = set_actual(&take, MATERIAL_A, "92").await;
    let _ = submit_and_approve_single(&take).await;
    let posted = transition(&take.header.id, STATUS_POSTED, None).await;
    assert_eq!(posted.header.status, STATUS_POSTED, "排除清单生效时过账必须成功");

    // ① 库存只按差异落一次：100 - 8 = 92（排除清单丢了会是 1092 / 84 之类）
    let (qty, amount) = cost_ledger(MATERIAL_A).await;
    assert_eq!(qty, dec("92"), "库存只能按实盘落一次");
    assert_eq!(amount, dec("736"));
    assert_eq!(location_ledger(MATERIAL_A).await, dec("92"));
    assert_eq!(take_txn_total(MATERIAL_A).await, dec("-8"));
    // ② 凭证只有 post_tx ⑤ 那一张（TAKEOVER 要是跑起来会多一张）
    assert_eq!(
        take_vouchers(&take.header.id).await,
        1,
        "差异凭证由 post_tx ⑤ 独占：联动不许再生成一张"
    );

    // ③ 反证：同一份规则，去掉排除清单后引擎真的会去读它
    let row = model::find(db(), uid(&take.header.id))
        .await
        .expect("读盘点单")
        .expect("盘点单存在");
    let ctx = linkage::LinkageContext::new(
        crate::business::shared::state_machine::ENTITY_STOCK_TAKE,
        STATUS_POSTED,
        row.id,
        &row.take_no,
        Some(uid(U_MAKER)),
    )
    .with_people(Some(uid(U_MAKER)), Some(uid(U_MAKER)), None);
    let excluded = linkage::engine::run_excluding(db(), &ctx, spec::STOCKTAKE_EXCLUDED_ACTIONS)
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
    match linkage::engine::run_excluding(db(), &ctx, &[]).await {
        Ok(run) => assert!(
            run.runs.iter().any(|item| item.action_code == "WRITE_INVENTORY"),
            "不传排除清单时规则必须被执行到，实际：{:?}",
            run.runs
                .iter()
                .map(|item| (&item.action_code, item.outcome.label()))
                .collect::<Vec<_>>()
        ),
        Err(error) => assert!(
            error.msg.contains(spec::ACTION_GENERATE_VOUCHER) || error.msg.contains("凭证"),
            "关键动作失败必须点名动作 / 原因，实际：{}",
            error.msg
        ),
    }
}

/// ⑰ 收尾：把本模块夹具清干净（`zz_` 前缀 → 本文件里最后跑）。
///
/// # 为什么必须有
///
/// 本模块给 `WAREHOUSE_MANAGER` / `GENERAL_MANAGER` 挂了夹具账号（`stk_*`）。
/// 这两个角色是**共享资源**：`contract_approval_tests` 断言「某一级只有一条待办」
/// （审批人按角色展开）。本模块跑完不摘钩，同一个库上下一次 `cargo test --lib`
/// 里合同用例就会看到「多一个人」而假红 —— 排序上本文件在 `contract_*` 之后，
/// 所以只有**跨 run** 才会踩，最难查。
#[tokio::test]
async fn zz_teardown_purges_stocktake_fixtures() {
    let _guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return;
    }
    purge_fixtures().await;
    let left: i64 = db()
        .scalar_with_params(
            "SELECT (SELECT count(*) FROM sys_user WHERE username LIKE 'stk_%') \
                  + (SELECT count(*) FROM biz_stock_take WHERE take_no LIKE 'ST-%') \
                  + (SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_TAKE') \
                  + (SELECT count(*) FROM biz_inventory WHERE material_id = ?::uuid)",
            vec![Value::from(&uid(MATERIAL_A))],
        )
        .await
        .expect("统计残留");
    assert_eq!(left, 0, "盘点用例的夹具必须零残留（用户 / 单据 / 凭证 / 库存）");
}
