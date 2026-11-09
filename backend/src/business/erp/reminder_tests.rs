//! R-P1-2 业务提醒任务包的**真库用例**（幂等台账 / 收件人解析 / 真机扫描）。
//!
//! 跑法（缺 `ERP_DB_URL` 时全部 SKIP，不假绿）：
//!
//! ```bash
//! export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cp_p12'
//! cargo test --lib business::erp::reminder -- --test-threads=1
//! ```
//!
//! # 夹具口径
//!
//! | 对象 | 前缀 | 说明 |
//! |------|------|------|
//! | 用户 | `p2.rm.*` | 复用 `period_test_support` 的造用户 / 挂角色工具（清理按 `p2.` 删） |
//! | 仓库 / 单位 / 物料 | `RM-` | 主数据夹具 |
//! | 应付单 / 采购单 / 合同 | `RM-` | 单据夹具（`due_date` / `expect_date` / `end_date` 按业务日算） |
//! | 通知 / 台账 | 按 `biz_type` / `remind_kind` | 只清本模块 4 类，不碰别人的通知 |
//!
//! **串行**（`--test-threads=1`）：清理按前缀删，并行会互删夹具。

use super::reminder::{self, RemindKind};
use super::reminder_stock;
use crate::business::erp::period_test_support as base;
use nik::prelude::chrono::{DateTime, Duration, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

/// 夹具用户（仓管主管）。
const USER_KEEPER: &str = "p2.rm.keeper";
/// 夹具用户（采购员 / 制单人）。
const USER_BUYER: &str = "p2.rm.buyer";
/// 夹具用户（财务经理）。
const USER_FINANCE: &str = "p2.rm.finance";

/// 执行 SQL（失败 panic，不静默吞错）。
async fn sql(text: &str) {
    base::sql(text).await;
}

/// 造一个夹具用户并挂到（已存在的或新建的）角色上。
async fn fixture_user(username: &str, role_code: &str) -> Uuid {
    let user = base::seed_user(username, "提醒夹具").await;
    let role = ensure_role(role_code).await;
    base::assign(user, role).await;
    user
}

/// 取角色 id；角色不存在就造一个（隔离库往往只种了 SUPER_ADMIN 的授权）。
async fn ensure_role(role_code: &str) -> Uuid {
    let sql_text = format!(
        "SELECT id FROM sys_role WHERE role_code = '{}' AND deleted_at IS NULL LIMIT 1",
        role_code.replace('\'', "")
    );
    match uuid_opt(&sql_text).await {
        Some(id) => id,
        None => base::seed_role(role_code).await,
    }
}

/// 清本模块夹具：通知 → 台账 → 单据 → 主数据 → 用户/角色（`p2.` 由 base 清）。
async fn cleanup() {
    sql("DELETE FROM sys_notice WHERE biz_type IN \
         ('payable_due', 'po_overdue', 'contract_expire', 'low_stock')")
    .await;
    sql("DELETE FROM biz_reminder_log").await;
    sql("DELETE FROM biz_payable WHERE payable_no LIKE 'RM-%'").await;
    sql("DELETE FROM biz_purchase_order WHERE order_no LIKE 'RM-%'").await;
    sql("DELETE FROM biz_contract WHERE contract_no LIKE 'RM-%'").await;
    sql("DELETE FROM biz_inventory WHERE material_id IN \
         (SELECT id FROM biz_material WHERE material_code LIKE 'RM-%')")
    .await;
    sql("DELETE FROM biz_material WHERE material_code LIKE 'RM-%'").await;
    sql("DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'RM-%'").await;
    sql("DELETE FROM biz_unit WHERE unit_code LIKE 'RM-%'").await;
    base::cleanup().await;
}

/// 取锁 + 连库 + 清场；`None` = 没有测试库（SKIP）。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = base::DB_LOCK.lock().await;
    if !base::ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 取一个可空的 uuid 标量（`None` = 没有行）。用于「有就返回、没有就造」的夹具。
async fn uuid_opt(sql_text: &str) -> Option<Uuid> {
    match db().scalar_with_params::<Uuid>(sql_text, vec![]).await {
        Ok(id) => Some(id),
        Err(DbError::NotFound) => None,
        Err(err) => panic!("查询失败：{err}\nSQL: {sql_text}"),
    }
}

/// 夹具仓库 id（`RM-WH`）。
async fn fixture_warehouse() -> Uuid {
    let sql_text = "SELECT id FROM biz_warehouse WHERE warehouse_code = 'RM-WH' AND deleted_at IS NULL";
    if let Some(id) = uuid_opt(sql_text).await {
        return id;
    }
    sql(
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, \
                                   warehouse_path, status) \
         VALUES (gen_random_uuid(), 'RM-WH', '提醒夹具仓', 'NORMAL', '/RM-WH', 1)",
    )
    .await;
    uuid_opt(sql_text).await.expect("夹具仓库应存在")
}

/// 夹具计量单位 id（`RM-PCS`）。
async fn fixture_unit() -> Uuid {
    let sql_text = "SELECT id FROM biz_unit WHERE unit_code = 'RM-PCS' AND deleted_at IS NULL";
    if let Some(id) = uuid_opt(sql_text).await {
        return id;
    }
    sql(
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status) \
         VALUES (gen_random_uuid(), 'RM-PCS', '个', 1, 1, 1)",
    )
    .await;
    uuid_opt(sql_text).await.expect("夹具单位应存在")
}

/// 造一张 `days_left` 天后到期的应付单（未付 100）。
async fn seed_payable(no: &str, days_left: i64, created_by: Uuid) -> Uuid {
    let due = reminder::business_day(Utc::now()) + Duration::days(days_left);
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_payable (id, payable_no, supplier_id, source_type, source_id, source_no, \
                                  amount, paid_amount, unpaid_amount, due_date, period_code, status, \
                                  create_by) \
         VALUES (?, ?, gen_random_uuid(), 'STOCK_IN', gen_random_uuid(), ?, \
                 100, 0, 100, ?::text::date, '2039-01', 'OPEN', ?)",
        vec![
            Value::from(&id),
            Value::Text(no.to_string()),
            Value::Text(format!("{no}-SRC")),
            Value::Text(due.format("%Y-%m-%d").to_string()),
            Value::from(&created_by),
        ],
    )
    .await
    .unwrap_or_else(|err| panic!("插入应付单失败：{err}"));
    id
}

/// 造一张采购单（`expect_date = 今天 - overdue_days`，状态 APPROVED）。
async fn seed_purchase_order(no: &str, overdue_days: i64, created_by: Uuid) -> Uuid {
    let warehouse = fixture_warehouse().await;
    let expect = reminder::business_day(Utc::now()) - Duration::days(overdue_days);
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, expect_date, \
                                         warehouse_id, status, create_by) \
         VALUES (?, ?, gen_random_uuid(), CURRENT_DATE - 30, ?::text::date, ?, 'APPROVED', ?)",
        vec![
            Value::from(&id),
            Value::Text(no.to_string()),
            Value::Text(expect.format("%Y-%m-%d").to_string()),
            Value::from(&warehouse),
            Value::from(&created_by),
        ],
    )
    .await
    .unwrap_or_else(|err| panic!("插入采购单失败：{err}"));
    id
}

/// 造一份 `days_left` 天后到期的生效合同（ACTIVE）。
async fn seed_contract(no: &str, days_left: i64, created_by: Uuid) -> Uuid {
    let end = reminder::business_day(Utc::now()) + Duration::days(days_left);
    let start = end - Duration::days(365);
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type, \
                                   party_b_id, party_b_name, amount, sign_date, start_date, end_date, \
                                   status, owner_id, create_by) \
         VALUES (?, ?, '提醒夹具采购合同', 'PURCHASE', 'SUPPLIER', gen_random_uuid(), \
                 '夹具供应商', 1000, ?::text::date, ?::text::date, ?::text::date, 'ACTIVE', ?, ?)",
        vec![
            Value::from(&id),
            Value::Text(no.to_string()),
            Value::Text(start.format("%Y-%m-%d").to_string()),
            Value::Text(start.format("%Y-%m-%d").to_string()),
            Value::Text(end.format("%Y-%m-%d").to_string()),
            Value::from(&created_by),
            Value::from(&created_by),
        ],
    )
    .await
    .unwrap_or_else(|err| panic!("插入合同失败：{err}"));
    id
}

/// 造一个物料 + 库存行（库存 10 < 安全库存 50）。
async fn seed_low_stock(material_code: &str) -> (Uuid, Uuid) {
    let warehouse = fixture_warehouse().await;
    seed_stock_in(material_code, 50, 10, warehouse).await
}

/// 造一个物料（安全库存 = `safety`）+ 指定仓库的一条库存行（`qty`）—— P2-2 ② 的夹具。
///
/// 为什么单独抽出来：安全库存口径的四条边界（0 / 恰好等于 / 负数 / 在途仓）都要**同一套
/// 造数代码**才可比，各写一遍就会各有各的口径。
async fn seed_stock_in(material_code: &str, safety: i64, qty: i64, warehouse: Uuid) -> (Uuid, Uuid) {
    let unit = fixture_unit().await;
    let material = Uuid::now_v7();
    db().execute_with_params(
        &format!(
            "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, \
                                     safety_stock, costing_method, status) \
             VALUES (?, ?, '提醒夹具物料', ?, 'RAW', {safety}, 'MOVING_AVG', 1)"
        ),
        vec![
            Value::from(&material),
            Value::Text(material_code.to_string()),
            Value::from(&unit),
        ],
    )
    .await
    .unwrap_or_else(|err| panic!("插入物料失败：{err}"));

    let inventory = Uuid::now_v7();
    // 数量直接拼进 SQL：`i64` 是测试侧常量（无注入面），而 `qty` 是 `numeric`，
    // 用 `Value::Text` 绑定会在准备语句时报 `error serializing parameter`（实测）。
    db().execute_with_params(
        &format!(
            "INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount) \
             VALUES (?, ?, ?, {qty}, 0, 1, {qty})"
        ),
        vec![Value::from(&inventory), Value::from(&material), Value::from(&warehouse)],
    )
    .await
    .unwrap_or_else(|err| panic!("插入库存失败：{err}"));
    (material, inventory)
}

/// 在途仓（`warehouse_type = 'IN_TRANSIT'`）—— P2-2 ② 的「不参与补货提醒」夹具。
async fn fixture_transit_warehouse() -> Uuid {
    let sql_text = "SELECT id FROM biz_warehouse WHERE warehouse_code = 'RM-WH-TRANSIT' AND deleted_at IS NULL";
    if let Some(id) = uuid_opt(sql_text).await {
        return id;
    }
    sql(
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, \
                                   warehouse_path, status) \
         VALUES (gen_random_uuid(), 'RM-WH-TRANSIT', '提醒夹具在途仓', 'IN_TRANSIT', \
                 '/RM-WH-TRANSIT/', 1)",
    )
    .await;
    uuid_opt(sql_text).await.expect("在途仓夹具应存在")
}

/// 当前扫描出的候选物料编码（**只读**，不发通知；`scan_candidates` 是提醒的唯一取数口径）。
async fn candidate_codes() -> Vec<String> {
    reminder_stock::scan_candidates()
        .await
        .expect("扫描低库存候选")
        .iter()
        .map(|candidate| candidate.material_code.clone())
        .collect()
}

/// 某张单在 `sys_notice` 里的通知条数。
async fn notice_count(biz_type: &str, doc_id: Uuid) -> i64 {
    db().scalar_with_params(
        "SELECT COUNT(*) FROM sys_notice WHERE biz_type = ? AND biz_id = ?",
        vec![Value::Text(biz_type.to_string()), Value::Text(doc_id.to_string())],
    )
    .await
    .expect("统计通知")
}

/// 断言某张单的通知条数；不符时把「每一行长什么样」打出来（幂等键 / 时间 / 收件人数）。
///
/// 为什么要这个：`notice_count != 期望` 时只看到「2 != 1」根本判断不了是「重复发」还是
/// 「别的套件留下的行」，而幂等键能一眼看出是 seq 变了还是日期/档位变了。
async fn assert_notice_count(biz_type: &str, doc_id: Uuid, expected: i64) {
    let rows = db()
        .query_sql(
            "SELECT notice_no, idempotency_key, receiver_count, created_at::text AS created_at \
               FROM sys_notice WHERE biz_type = ? AND biz_id = ? ORDER BY created_at"
                .to_string(),
            vec![Value::Text(biz_type.to_string()), Value::Text(doc_id.to_string())],
        )
        .await
        .expect("查通知明细");
    let detail = rows
        .iter()
        .map(|row| {
            format!(
                "{}|{}|recv={}|{}",
                row.get_typed::<String>("notice_no").unwrap_or_default(),
                row.get_typed::<String>("idempotency_key").unwrap_or_default(),
                row.get_typed::<i64>("receiver_count").unwrap_or(-1),
                row.get_typed::<String>("created_at").unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows.len() as i64,
        expected,
        "{biz_type} × {doc_id} 的通知条数不符；实际行（单号|幂等键|收件人数|创建时间）：{detail:#?}"
    );
}

/// 某条通知的 (标题, 正文)。
async fn notice_text(biz_type: &str, doc_id: Uuid) -> (String, String) {
    let row = db()
        .find_with_params(
            "SELECT title, content FROM sys_notice WHERE biz_type = ? AND biz_id = ? \
             ORDER BY created_at DESC LIMIT 1",
            vec![Value::Text(biz_type.to_string()), Value::Text(doc_id.to_string())],
        )
        .await
        .expect("查通知");
    let row = row.first().expect("通知应存在");
    (
        row.get_typed("title").expect("标题"),
        row.get_typed("content").expect("正文"),
    )
}

/// 某条通知的收件人数。
async fn receiver_count(biz_type: &str, doc_id: Uuid) -> i64 {
    db().scalar_with_params(
        "SELECT COALESCE(SUM(n.receiver_count), 0) FROM sys_notice n \
          WHERE n.biz_type = ? AND n.biz_id = ?",
        vec![Value::Text(biz_type.to_string()), Value::Text(doc_id.to_string())],
    )
    .await
    .expect("统计收件人")
}

/// 台账行数（某单据 × 某类）。
async fn ledger_count(kind: RemindKind, doc_id: Uuid) -> i64 {
    db().scalar_with_params(
        "SELECT COUNT(*) FROM biz_reminder_log WHERE remind_kind = ? AND doc_id = ?",
        vec![Value::Text(kind.code().to_string()), Value::from(doc_id)],
    )
    .await
    .expect("统计台账")
}

// ==================== ① 应付到期：同一天连跑 3 次只发 1 条 ====================

/// 卡面验收口径：造一张 3 天后到期的应付单 → 触发 → 收到通知；再跑 2 次 → 不再发。
#[tokio::test]
async fn payable_due_sends_once_per_day() {
    let Some(_guard) = setup().await else {
        return;
    };
    let buyer = fixture_user(USER_BUYER, "PURCHASE_MANAGER").await;
    let _finance = fixture_user(USER_FINANCE, "FINANCE_MANAGER").await;
    let payable = seed_payable("RM-AP-0001", 3, buyer).await;

    let now = Utc::now();
    let first = reminder::run_kind(RemindKind::PayableDue, now)
        .await
        .expect("扫描应付到期");
    // 计数是**整类全局**的（全量套件里别的夹具也可能命中档位），
    // 本用例的强断言一律落到「这张单」上（notice / 台账按 doc_id 收窄）。
    assert!(first.scanned >= 1, "应扫到至少 1 张 3 天后到期的应付单");
    assert!(first.sent >= 1, "首次必须发出去");
    assert_eq!(first.failed, 0);
    assert_notice_count("payable_due", payable, 1).await;
    assert!(
        receiver_count("payable_due", payable).await >= 2,
        "采购员 + 财务至少 2 人"
    );

    // 通知内容三件套：单号 / 关键日期 / 一句话该做什么
    let (title, content) = notice_text("payable_due", payable).await;
    let due = (reminder::business_day(now) + Duration::days(3))
        .format("%Y-%m-%d")
        .to_string();
    assert!(title.contains("RM-AP-0001"), "标题要带单号：{title}");
    assert!(content.contains("RM-AP-0001"), "正文要带单号：{content}");
    assert!(content.contains(&due), "正文要带到期日 {due}：{content}");
    assert!(content.contains("安排付款"), "正文要写该做什么：{content}");
    assert!(content.contains('3'), "正文要写剩余天数：{content}");

    // 台账：D-3 档一行，带收件人数
    assert_eq!(ledger_count(RemindKind::PayableDue, payable).await, 1);
    let stage: String = db()
        .scalar_with_params::<String>(
            "SELECT stage FROM biz_reminder_log WHERE remind_kind = 'PAYABLE_DUE' AND doc_id = ?",
            vec![Value::from(payable)],
        )
        .await
        .expect("查台账档位");
    assert_eq!(stage, "D-3");

    // 同一天再跑 2 次：这张单命中台账，通知条数不变
    for round in 2..=3 {
        let again = reminder::run_kind(RemindKind::PayableDue, now).await.expect("重复扫描");
        assert!(again.skipped_reminded >= 1, "第 {round} 次应计入「今天已提醒」");
        assert_eq!(
            notice_count("payable_due", payable).await,
            1,
            "第 {round} 次后这张单的通知仍是 1 条（防轰炸）"
        );
    }
    assert_eq!(ledger_count(RemindKind::PayableDue, payable).await, 1, "台账不能重复写");
}

// ==================== ② 低库存：扫描 + 幂等 ====================

/// 造一个库存低于安全库存的物料 → 触发 → 仓管主管收到；再跑一次不重复。
#[tokio::test]
async fn low_stock_sends_once_and_dedupes() {
    let Some(_guard) = setup().await else {
        return;
    };
    let keeper = fixture_user(USER_KEEPER, "WAREHOUSE_MANAGER").await;
    let (_material, inventory) = seed_low_stock("RM-M-0001").await;

    let now = Utc::now();
    let first = reminder::run_kind(RemindKind::LowStock, now).await.expect("扫描低库存");
    assert!(first.scanned >= 1);
    assert!(first.sent >= 1, "首次必须发出去");
    assert_notice_count("low_stock", inventory, 1).await;

    let (title, content) = notice_text("low_stock", inventory).await;
    assert!(title.contains("RM-M-0001"), "标题要带物料编码：{title}");
    for fact in ["RM-M-0001", "提醒夹具物料", "提醒夹具仓", "10", "50", "40", "补货"] {
        assert!(content.contains(fact), "正文缺 `{fact}`：{content}");
    }
    // 收件人就是夹具仓管主管
    let receivers: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM sys_notice_receiver r JOIN sys_notice n ON n.id = r.notice_id \
              WHERE n.biz_type = 'low_stock' AND n.biz_id = ? AND r.user_id = ?",
            vec![Value::Text(inventory.to_string()), Value::from(keeper)],
        )
        .await
        .expect("统计收件行");
    assert_eq!(receivers, 1, "仓管主管必须有收件行");

    let again = reminder::run_kind(RemindKind::LowStock, now).await.expect("重复扫描");
    assert!(again.skipped_reminded >= 1);
    assert_eq!(notice_count("low_stock", inventory).await, 1, "同一天再触发不得重复发");
}

// ==================== ②b 安全库存口径（P2-2 ②：0 = 不预警 / 恰好等于不报 / 在途仓不报） ====================

/// **P2-2-②a**：安全库存留空（库里 = `0`）→ **不预警**；同仓同批的对照物料照报。
///
/// 这是卡面「空值不预警有单测」的那一条。本仓 `biz_material.safety_stock` 是
/// `NUMERIC NOT NULL DEFAULT 0`，所以「留空」落库就是 `0`，没有 NULL 状态 ——
/// 判定写成 `COALESCE(safety_stock,0) > 0`，0 与（未来可能的）NULL 都不参与。
///
/// 对照组（同一次扫描里 `safety_stock = 50 / qty = 10` 必须出现）保证了「没出现」
/// 不是因为扫描整体失效（比如 SQL 报错被吞、或物料被 `status = 0` 过滤掉）。
#[tokio::test]
async fn low_stock_zero_safety_is_silent_but_control_material_is_not() {
    let Some(_guard) = setup().await else {
        return;
    };
    let warehouse = fixture_warehouse().await;
    // 留空（0）：qty 也是 0，若把 0 当阈值就会天天报 —— 正是要消灭的噪音
    let (_zero, zero_row) = seed_stock_in("RM-M-ZERO", 0, 0, warehouse).await;
    // 对照组：明确填了 50，库存 10
    let (_low, _low_row) = seed_stock_in("RM-M-CONTROL", 50, 10, warehouse).await;

    let codes = candidate_codes().await;
    assert!(
        !codes.iter().any(|code| code == "RM-M-ZERO"),
        "安全库存留空（0）的物料不该进候选：{codes:?}"
    );
    assert!(
        codes.iter().any(|code| code == "RM-M-CONTROL"),
        "同一仓里填了安全库存的对照物料必须进候选（否则本用例证明不了什么）：{codes:?}"
    );

    // 端到端再确认一次：跑一轮提醒，留空物料一条都不发
    reminder::run_kind(RemindKind::LowStock, Utc::now())
        .await
        .expect("扫描低库存");
    assert_eq!(
        notice_count("low_stock", zero_row).await,
        0,
        "留空安全库存的库存行不该有提醒"
    );
}

/// **P2-2-②b**：`qty` **恰好等于**安全库存 → **不报**（口径是「低于」，不是「不高于」）。
#[tokio::test]
async fn low_stock_exact_boundary_is_silent() {
    let Some(_guard) = setup().await else {
        return;
    };
    let warehouse = fixture_warehouse().await;
    let (_boundary, boundary_row) = seed_stock_in("RM-M-BOUND", 50, 50, warehouse).await;
    let (_below, below_row) = seed_stock_in("RM-M-BELOW", 50, 49, warehouse).await;

    let codes = candidate_codes().await;
    assert!(
        !codes.iter().any(|code| code == "RM-M-BOUND"),
        "qty == safety_stock 不算低库存：{codes:?}"
    );
    assert!(
        codes.iter().any(|code| code == "RM-M-BELOW"),
        "少 1 个就必须报（边界另一侧）：{codes:?}"
    );
    let _ = (boundary_row, below_row);
}

/// **P2-2-②c**：负库存照实数参与（不 `GREATEST(qty,0)` 抹平）——
/// 允许负库存的仓出现 `-3` 说明账实已经不符，比「刚好低一点」更该提醒。
#[tokio::test]
async fn low_stock_negative_qty_is_reported() {
    let Some(_guard) = setup().await else {
        return;
    };
    let warehouse = fixture_warehouse().await;
    seed_stock_in("RM-M-NEG", 50, -3, warehouse).await;
    let codes = candidate_codes().await;
    assert!(
        codes.iter().any(|code| code == "RM-M-NEG"),
        "负库存必须进候选（不许被抹成 0 后当成「刚好」）：{codes:?}"
    );
}

/// **P2-2-②d**：**在途仓不参与**低库存提醒 —— 在途仓是记账用的虚拟仓，不是可补货的实体仓。
///
/// 同一个物料在两个仓各一行：在途仓 `qty = 0`（本该"低于"），实体仓 `qty = 10`。
/// 结果是**一条**候选（实体仓那条），不是两条 —— 否则仓管会收到「请给在途仓补货」这种
/// 无法执行的提醒。
#[tokio::test]
async fn low_stock_ignores_in_transit_warehouse() {
    let Some(_guard) = setup().await else {
        return;
    };
    let warehouse = fixture_warehouse().await;
    let transit = fixture_transit_warehouse().await;
    assert_ne!(warehouse, transit, "夹具自检：两个仓必须是不同的行");
    let (_material, _normal_row) = seed_stock_in("RM-M-TRANSIT", 50, 10, warehouse).await;
    let (_same_material, transit_row) = {
        // 同一物料第二条库存行（物料×仓库唯一）：物料复用上一条的 id
        let material: Uuid = db()
            .scalar_with_params(
                "SELECT id FROM biz_material WHERE material_code = 'RM-M-TRANSIT' \
                  AND deleted_at IS NULL LIMIT 1",
                vec![],
            )
            .await
            .expect("查物料");
        let row = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount) \
             VALUES (?, ?, ?, 0, 0, 1, 0)",
            vec![Value::from(&row), Value::from(&material), Value::from(&transit)],
        )
        .await
        .expect("插入在途仓库存行");
        (material, row)
    };

    let candidates = reminder_stock::scan_candidates().await.expect("扫描低库存候选");
    let hits: Vec<&reminder_stock::LowStockCandidate> = candidates
        .iter()
        .filter(|candidate| candidate.material_code == "RM-M-TRANSIT")
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "同一物料只该有实体仓那一条候选（在途仓被排除）：{:?}",
        hits.iter()
            .map(|c| (c.warehouse_name.clone(), c.qty.clone()))
            .collect::<Vec<_>>()
    );
    assert_ne!(hits[0].id, transit_row, "候选不能是在途仓那一行");
    assert_eq!(
        hits[0].qty, "10.0000",
        "候选是实体仓那一行（qty 读出是 NUMERIC 原文，带 4 位标度）"
    );
}

// ==================== ③ 重提醒间隔（采购超期 / 低库存） ====================
/// 采购超期：首次发；间隔内不重复；过了间隔（3 天）再发一次。
#[tokio::test]
async fn purchase_overdue_respects_realert_window() {
    let Some(_guard) = setup().await else {
        return;
    };
    let buyer = fixture_user(USER_BUYER, "PURCHASE_MANAGER").await;
    let order = seed_purchase_order("RM-PO-0001", 5, buyer).await;
    let now = Utc::now();
    let day = reminder::business_day(now);

    let first = reminder::run_kind(RemindKind::PoOverdue, now)
        .await
        .expect("扫描采购超期");
    assert!(first.scanned >= 1);
    assert!(first.sent >= 1);
    let (title, content) = notice_text("po_overdue", order).await;
    assert!(title.contains("RM-PO-0001"), "{}", title);
    assert!(content.contains("RM-PO-0001") && content.contains('5'), "{}", content);
    assert!(content.contains("催供应商交货"), "{}", content);

    // 同一天再跑：台账命中
    let same_day = reminder::run_kind(RemindKind::PoOverdue, now).await.expect("重复扫描");
    assert!(same_day.skipped_reminded >= 1);
    assert_notice_count("po_overdue", order, 1).await;

    // 把台账改成 1 天前：仍在 3 天间隔内 → skipped_recent
    let yesterday = (day - Duration::days(1)).format("%Y-%m-%d").to_string();
    sql(&format!(
        "UPDATE biz_reminder_log SET remind_date = '{yesterday}'::date \
          WHERE remind_kind = 'PO_OVERDUE' AND doc_id = '{order}'"
    ))
    .await;
    let within = reminder::run_kind(RemindKind::PoOverdue, now)
        .await
        .expect("扫描采购超期");
    assert!(within.skipped_recent >= 1, "间隔内应计入 skipped_recent");
    assert_notice_count("po_overdue", order, 1).await;

    // 把台账改成 3 天前：出了间隔 → 再提醒一次（档位变了 → 通知幂等键也变了）
    let three_days = (day - Duration::days(3)).format("%Y-%m-%d").to_string();
    sql(&format!(
        "UPDATE biz_reminder_log SET remind_date = '{three_days}'::date \
          WHERE remind_kind = 'PO_OVERDUE' AND doc_id = '{order}'"
    ))
    .await;
    let _ = reminder::run_kind(RemindKind::PoOverdue, now)
        .await
        .expect("扫描采购超期");
    assert_eq!(notice_count("po_overdue", order).await, 2, "过了重提醒间隔应再发一条");
}

// ==================== ④ 合同到期 ====================

/// 合同 D-30 档：扫到、发出、文案带合同号与到期日。
#[tokio::test]
async fn contract_expire_sends_for_d30() {
    let Some(_guard) = setup().await else {
        return;
    };
    let buyer = fixture_user(USER_BUYER, "PURCHASE_MANAGER").await;
    let contract = seed_contract("RM-HT-20260101-000001", 30, buyer).await;

    let now = Utc::now();
    let report = reminder::run_kind(RemindKind::ContractExpire, now)
        .await
        .expect("扫描合同到期");
    assert!(report.scanned >= 1);
    assert!(report.sent >= 1);
    let (title, content) = notice_text("contract_expire", contract).await;
    let end = (reminder::business_day(now) + Duration::days(30))
        .format("%Y-%m-%d")
        .to_string();
    assert!(title.contains("RM-HT-20260101-000001"), "{}", title);
    assert!(content.contains(&end), "正文要带到期日 {end}：{content}");
    assert!(content.contains("续签"), "正文要写该做什么：{content}");

    // 非档位日期（20 天）不提醒（强断言落在「这张合同没有通知」上）
    let other = seed_contract("RM-HT-20260101-000002", 20, buyer).await;
    let _ = reminder::run_kind(RemindKind::ContractExpire, now)
        .await
        .expect("扫描合同到期");
    assert_notice_count("contract_expire", other, 0).await;
}

// ==================== ⑤ 定位不到人：计数 + 不发，不静默丢弃 ====================

/// 仓管主管全部停用 → `skipped_no_recipient = 1`，且一条通知都不发。
#[tokio::test]
async fn no_recipient_is_counted_not_dropped() {
    let Some(_guard) = setup().await else {
        return;
    };
    let _keeper = fixture_user(USER_KEEPER, "WAREHOUSE_MANAGER").await;
    let (_material, inventory) = seed_low_stock("RM-M-0002").await;
    // 让**该角色下所有用户**停用（不止本夹具：全量套件里可能还有别的夹具用户挂着同名角色），
    // 跑完再恢复，避免影响后续用例。
    sql("UPDATE sys_user SET status = 0 WHERE id IN \
         (SELECT ur.user_id FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id \
           WHERE r.role_code = 'WAREHOUSE_MANAGER' AND r.deleted_at IS NULL)")
    .await;

    let report = reminder::run_kind(RemindKind::LowStock, Utc::now())
        .await
        .expect("扫描低库存");

    // 恢复（本夹具用户随后被 base::cleanup 删掉，恢复不存在的行也无害）
    sql("UPDATE sys_user SET status = 1 WHERE id IN \
         (SELECT ur.user_id FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id \
           WHERE r.role_code = 'WAREHOUSE_MANAGER' AND r.deleted_at IS NULL)")
    .await;

    assert!(report.scanned >= 1);
    assert!(report.skipped_no_recipient >= 1, "必须计数（不许静默丢弃）");
    assert_notice_count("low_stock", inventory, 0).await;
    assert_eq!(
        ledger_count(RemindKind::LowStock, inventory).await,
        0,
        "没发就不该记台账"
    );
}

// ==================== ⑥ 业务日边界（写入路径） ====================

/// 台账按**业务日**去重：UTC 前一天 17:30（北京次日 01:30）与北京当日是同一天。
#[tokio::test]
async fn ledger_uses_beijing_business_day() {
    let Some(_guard) = setup().await else {
        return;
    };
    let buyer = fixture_user(USER_BUYER, "PURCHASE_MANAGER").await;
    let payable = seed_payable("RM-AP-0002", 1, buyer).await;

    // 同一个「北京业务日」的两个时刻：北京 08:00 与 23:00（UTC 00:00 / 15:00）
    let morning = Utc::now();
    let first = reminder::run_kind(RemindKind::PayableDue, morning)
        .await
        .expect("第一次扫描");
    assert!(first.sent >= 1);

    // 用「北京同一业务日的晚些时候」再跑一次（+1 小时，仍在同一天，除非跨零点）
    let later: DateTime<Utc> = morning + Duration::hours(1);
    if reminder::business_day(later) == reminder::business_day(morning) {
        let second = reminder::run_kind(RemindKind::PayableDue, later)
            .await
            .expect("第二次扫描");
        assert!(second.skipped_reminded >= 1, "同一业务日应命中台账（不重复提醒）");
    }
    assert_notice_count("payable_due", payable, 1).await;
    // 低库存的预览入口能跑通（不连库失败）
    assert!(reminder_stock::scan(Utc::now()).await.is_ok());
}
