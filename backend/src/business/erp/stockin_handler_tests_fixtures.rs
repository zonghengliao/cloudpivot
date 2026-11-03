// ============================================================================
// stockin_handler_tests_fixtures.rs —— 测试夹具与库连接（常量 / 建库 / 清理 / 小工具）
//
// 由 `stockin_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use std::str::FromStr;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::stockin_handler_tests_helpers::*;
use super::*;
use crate::business::erp::approval_gate;
use crate::business::erp::stockin_dto::{CreateStockInReq, StockInItemReq, StockInStatusReq};

/// ARCH-3 DC-01：系统路径的审批前置上下文 —— 不请求跳过、也没有权限声明。
///
/// 用它调 [`submit_post`] 等于「一个普通内部调用」：没配 `stock_in_post` /
/// `stock_in_cancel` 流程时**一律 409**（新的默认行为，不再是旧的静默降级）。
pub(super) fn none_gate() -> approval_gate::ApprovalGate<'static> {
    approval_gate::ApprovalGate::none()
}

pub(super) const UNIT: &str = "b2b10001-0000-4000-8000-000000000001";
pub(super) const SUPPLIER: &str = "b2b10001-0000-4000-8000-000000000002";
pub(super) const WAREHOUSE: &str = "b2b10001-0000-4000-8000-000000000003";
/// 非一物一码物料（螺丝）。
pub(super) const MATERIAL_A: &str = "b2b10001-0000-4000-8000-000000000011";
/// 非一物一码物料（垫片）。
pub(super) const MATERIAL_B: &str = "b2b10001-0000-4000-8000-000000000012";
/// 一物一码物料（电机）。
pub(super) const MATERIAL_SN: &str = "b2b10001-0000-4000-8000-000000000013";
/// DC-09 夹具：挂在折算单位上的物料 —— 本版本**不能**在收货单里使用。
pub(super) const MATERIAL_BOX: &str = "b2b10001-0000-4000-8000-000000000015";
/// DC-09 夹具：折算单位「箱」（`is_base = 0`，1 箱 = 12 个）。
pub(super) const UNIT_BOX: &str = "b2b10001-0000-4000-8000-000000000004";
/// 库位字典（`biz_location.id`，P6：库位账行的 `location_id` 列指这个）。
pub(super) const LOC_DICT_A: &str = "b2b10003-0000-4000-8000-000000000001";
/// 库位字典（第二个库位）。
pub(super) const LOC_DICT_B: &str = "b2b10003-0000-4000-8000-000000000002";
/// A4 夹具：**第二个**供应商。
///
/// 与 [`SUPPLIER`] 分开，才能证明「入库单的供应商确实从采购单派生」——
/// 用同一个供应商的话，派生与手填的结果一样，断言再严也测不出东西。
pub(super) const A4_SUPPLIER_B: &str = "b2b10001-0000-4000-8000-000000000014";

pub(super) const FIXTURES: &[&str] = &[
    "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
     VALUES ('b2b10001-0000-4000-8000-000000000001', 'P2B1-PCS', '个', 1, NULL, 1.000000, 1, 1)
     ON CONFLICT DO NOTHING",
    // DC-09 夹具：折算单位「箱」（is_base = 0，1 箱 = 12 个）。收货单里用它会 400。
    "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
     VALUES ('b2b10001-0000-4000-8000-000000000004', 'P2B1-BOX', '箱', 0,
             'b2b10001-0000-4000-8000-000000000001', 12.000000, 1, 2)
     ON CONFLICT DO NOTHING",
    "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
     VALUES ('b2b10001-0000-4000-8000-000000000002', 'P2B1-SUP', 'P2B1 测试供应商', 1)
     ON CONFLICT DO NOTHING",
    // A4 夹具：第二个供应商（证明入库单供应商是从采购单派生的）
    "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
     VALUES ('b2b10001-0000-4000-8000-000000000014', 'A4-SUP-B', 'A4 二号供应商', 1)
     ON CONFLICT DO NOTHING",
    "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
     VALUES ('b2b10001-0000-4000-8000-000000000003', 'P2B1-WH', 'P2B1 收货仓',
             '/b2b10001-0000-4000-8000-000000000003/', 1, 1, 1)
     ON CONFLICT DO NOTHING",
    "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
     VALUES ('b2b10001-0000-4000-8000-000000000011', 'P2B1-M1', '螺丝 M6x20',
             'b2b10001-0000-4000-8000-000000000001', 1, false),
            ('b2b10001-0000-4000-8000-000000000012', 'P2B1-M2', '垫片 8mm',
             'b2b10001-0000-4000-8000-000000000001', 1, false),
            ('b2b10001-0000-4000-8000-000000000013', 'P2B1-M3', '伺服电机',
             'b2b10001-0000-4000-8000-000000000001', 1, true)
     ON CONFLICT DO NOTHING",
    // DC-09 夹具：物料挂在折算单位「箱」上 —— 收货单必须被拒（手工插入，模拟主数据里
    // 已经配成折算单位的存量物料；主数据接口本卡不动）。
    "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
     VALUES ('b2b10001-0000-4000-8000-000000000015', 'P2B1-M4-BOX', '整箱垫片（折算单位）',
             'b2b10001-0000-4000-8000-000000000004', 1, false)
     ON CONFLICT DO NOTHING",
    // P6：两个库位字典（库位账行的 location_id 列指它；无外键，弱引用）
    "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                               is_default, capacity, status, sort_order)
     VALUES ('b2b10003-0000-4000-8000-000000000001', 'P2B1-L01', 'P2B1 A 区一号',
             'b2b10001-0000-4000-8000-000000000003', 'STORAGE', 0, NULL, 1, 1),
            ('b2b10003-0000-4000-8000-000000000002', 'P2B1-L02', 'P2B1 A 区二号',
             'b2b10001-0000-4000-8000-000000000003', 'STORAGE', 0, NULL, 1, 2)
     ON CONFLICT DO NOTHING",
    // P16：作废审批的**发起人**（仓管员，故意不是超管）。
    // 流程节点 `self_approval=FORBID`（内控：审批人 ≠ 发起人），若发起人本身就是超管，
    // 引擎会因为他出现在 ADMIN 候选里直接 422 —— 那不是 bug，是「不许自审自批」的硬拦。
    "INSERT INTO sys_user (id, username, password, real_name)
     VALUES ('b2b10009-0000-4000-8000-000000000001', 'p16_keeper', 'x', 'P16 仓管员')
     ON CONFLICT DO NOTHING",
];

/// 真库用例必须串行（共用全局 `db()`、同一批夹具与同一 `biz_seq(IN)`）。
pub(super) static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 初始化全局数据库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
pub(super) async fn ensure_db() -> bool {
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
            // 每个 `#[tokio::test]` 是独立 runtime，而 `db()` 是进程级全局池：
            // `max_lifetime = 0` 逼池子每次新建连接，避免借到上一个 runtime 的死连接（W1b 实测）。
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

/// 拿锁 + 连库 + 清上一轮 + 建夹具。
pub(super) async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup_fixtures().await;
    for sql in FIXTURES {
        if let Err(err) = db().execute_with_params(sql, vec![]).await {
            panic!("夹具写入失败：{err}\nSQL: {sql}");
        }
    }
    Some(guard)
}

/// 本夹具的「过账 / 作废」审批实例定位口径（多处复用，必须逐字一致）。
///
/// 三条任一成立即算本夹具的：
/// 1. `biz_id` 指向本夹具供应商的收货单；
/// 2. `biz_id` 为空，或指向**已经不存在的收货单** —— 上一轮 `cleanup_fixtures()` 删单时留下的孤儿，
///    这正是「每跑一轮全量就多 10 条 `STOCK_IN_POST | PENDING`」的真身
///    （实例 `biz_id` 是弱引用，删单据带不走它）；
/// 3. 按本模块的两条审批流类型（`STOCK_IN_POST` / `STOCK_IN_CANCEL`）。
pub(super) fn post_instance_where() -> String {
    format!(
        "biz_type IN ('STOCK_IN_POST', 'STOCK_IN_CANCEL') AND ( \
             biz_id IS NULL \
          OR biz_id NOT IN (SELECT id FROM biz_stock_in) \
          OR biz_id IN (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)))"
    )
}

/// 清掉本夹具的审批痕迹（**删收货单之前**跑）：实例 / 待办 / 抄送。
///
/// 顺序不可交换（与 transfer / purchase 同一范式）：
/// 1. `biz_stock_in.approval_instance_id` 是弱引用 → 先置 NULL；
/// 2. `biz_flow_instance.prev_instance_id` 是**自引用且没有 ON DELETE**（V0122）→ 先解开，
///    否则整批 DELETE 被外键拦下、只抛 `query failed: db error`（PG 原文里没有约束名）；
/// 3. `biz_flow_task` / `biz_flow_cc` 虽 ON DELETE CASCADE，仍显式先删（顺序写死）；
/// 4. 实例本体（**含孤儿**，见 [`post_instance_where`]）。
pub(super) async fn purge_post_approval_traces() {
    let inst_where = post_instance_where();
    let statements = [
        // `ck_biz_si_pending_cancel` 把「PENDING_CANCEL / PENDING_POST ⇒ approval_instance_id 非空」
        // 钉成库级约束 ⇒ 只能给「非待审状态」的单解开弱引用（待审的两类单据在
        // `cleanup_fixtures` 里会被整行删掉，不需要先置 NULL）。
        format!(
            "UPDATE biz_stock_in SET approval_instance_id = NULL \
              WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid) AND deleted_at IS NULL \
                AND status NOT IN ('PENDING_CANCEL', 'PENDING_POST')"
        ),
        format!(
            "UPDATE biz_flow_instance SET prev_instance_id = NULL \
              WHERE prev_instance_id IN (SELECT id FROM biz_flow_instance WHERE {inst_where})"
        ),
        format!(
            "DELETE FROM biz_flow_task \
              WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE {inst_where})"
        ),
        format!(
            "DELETE FROM biz_flow_cc \
              WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE {inst_where})"
        ),
        format!("DELETE FROM biz_flow_instance WHERE {inst_where}"),
    ];
    for sql in statements {
        if let Err(err) = db().execute_with_params(&sql, vec![]).await {
            panic!("清理收货审批痕迹失败：{err}\nSQL: {sql}");
        }
    }
}

/// 清掉本模块造的所有夹具（含 P9 新增的凭证/应付清理）。
///
/// 顺序有讲究：序列号 → 流水 → 库存 → 库位账 → 凭证/应付 → 收货单 → 采购单。
/// 凭证/应付是**弱引用**（无外键），必须在删收货单之前按 `source_id` 清掉。
pub(super) async fn cleanup_fixtures() {
    // 审批实例先清（`biz_id` 是弱引用：删收货单带不走实例，会留下孤儿 PENDING →
    // 每轮多 10~15 条、顶住 `uk_biz_flow_instance_biz_active`）。
    purge_post_approval_traces().await;
    let cleanups = [
        // P16：某条用例会把种子流程临时下线（降级路径），这里无条件恢复，
        // 免得一条用例 panic 之后拖垮后面所有用例
        "UPDATE biz_flow_def SET deleted_at = NULL WHERE flow_code = 'stock_in_cancel'".to_string(),
        // 作废审批实例已由上面的 `purge_post_approval_traces` 一并清掉（含孤儿）。
        // P10：状态日志没有外键，必须在删单据之前按「本夹具的单据 id / 单号」清掉；
        // 同时收掉上一轮删单留下的孤儿（`entity_id` 已指向不存在的收货单 —— 每轮几十行）。
        format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'STOCK_IN' AND ( \
                 entity_id IN (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)) \
              OR entity_id NOT IN (SELECT id FROM biz_stock_in))"
        ),
        "DELETE FROM biz_status_log WHERE entity_no LIKE 'B1-%'".to_string(),
        format!(
            "DELETE FROM biz_material_serial WHERE source_type = 'PURCHASE' AND source_id IN \
         (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))"
        ),
        format!(
            "DELETE FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id IN \
         (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))"
        ),
        format!(
            "DELETE FROM biz_inventory WHERE material_id IN \
         ('{MATERIAL_A}'::uuid, '{MATERIAL_B}'::uuid, '{MATERIAL_SN}'::uuid) \
         AND warehouse_id = '{WAREHOUSE}'::uuid"
        ),
        // P6：库位账必须排在 biz_inventory 之后删（`fk_biz_inv_location_id` 指本表）
        format!(
            "DELETE FROM biz_inventory_location WHERE material_id IN \
         ('{MATERIAL_A}'::uuid, '{MATERIAL_B}'::uuid, '{MATERIAL_SN}'::uuid) \
         AND warehouse_id = '{WAREHOUSE}'::uuid"
        ),
        // P9：过账即生成凭证/应付，不清理会让「按 period 全局计数」的其它模块用例漂数
        format!(
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
         (SELECT id FROM biz_voucher WHERE source_id IN \
          (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)))"
        ),
        format!(
            "DELETE FROM biz_voucher WHERE source_id IN \
         (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))"
        ),
        format!(
            "DELETE FROM biz_payable WHERE source_id IN \
         (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))"
        ),
        // 作废路径会为凭证 / 应付写状态日志（P10 起所有实体都写），而日志**没有外键**：
        // 单据删了日志还在，每轮多攒几百行永远对不上的孤儿。这里收掉「entity_id 已指向
        // 不存在的凭证 / 应付」的行（活行的日志一条不动）。
        "DELETE FROM biz_status_log WHERE entity_type IN ('PAYABLE', 'VOUCHER') \
            AND entity_id NOT IN (SELECT id FROM biz_payable) \
            AND entity_id NOT IN (SELECT id FROM biz_voucher)"
            .to_string(),
        format!(
            "DELETE FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)"
        ),
        format!(
            "DELETE FROM biz_purchase_order WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)"
        ),
    ];
    for sql in cleanups {
        if let Err(err) = db().execute_with_params(&sql, vec![]).await {
            panic!("清理数据失败：{err}\nSQL: {sql}");
        }
    }
}

pub(super) fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("测试金额字面量非法")
}

/// 一行收货明细的请求态。
pub(super) fn line(material: &str, qty: &str, price: &str, rate: &str) -> StockInItemReq {
    StockInItemReq {
        purchase_order_item_id: None,
        material_id: Some(material.to_string()),
        qty: Some(dec(qty)),
        price_excl: Some(dec(price)),
        tax_rate: Some(dec(rate)),
        location_id: None,
        serial_ids: Vec::new(),
        remark: None,
    }
}

/// 同 [`line`]，但带上库位（`biz_inventory_location.id`，P6）。
pub(super) fn line_at(
    material: &str,
    qty: &str,
    price: &str,
    rate: &str,
    location: &str,
) -> StockInItemReq {
    StockInItemReq {
        location_id: Some(location.to_string()),
        ..line(material, qty, price, rate)
    }
}

pub(super) fn req(lines: Vec<StockInItemReq>) -> CreateStockInReq {
    CreateStockInReq {
        source_order_id: None,
        purchase_order_id: None,
        supplier_id: Some(SUPPLIER.to_string()),
        // A4：没有来源采购单 ⇒ 必须显式声明「无源单据」+ 写明来源（AP-5）
        is_sourceless: Some(true),
        warehouse_id: Some(WAREHOUSE.to_string()),
        in_date: None,
        invoice_received: None,
        remark: Some("A4 夹具：无源入库（手工单）".to_string()),
        items: lines,
        total_qty: None,
        total_amount: None,
        total_tax: None,
    }
}

pub(super) fn post_req() -> StockInStatusReq {
    StockInStatusReq {
        status: Some("POSTED".to_string()),
        reason: None,
        serials: Vec::new(),
        ..Default::default()
    }
}

/// 作废审批的发起人（仓管员；非超管 —— 见 `FIXTURES` 的说明）。
pub(super) fn uuid_keeper() -> Uuid {
    Uuid::parse_str("b2b10009-0000-4000-8000-000000000001").expect("夹具用户 UUID")
}

// ==================== P-FLOW-3A：收货过账审批（8 条） ====================
//
// 跑的是**真种子**（V0182 的 `stock_in_post`：5 节点 5 边，含 NOTIFY_NODE 小额免审分支），
// 不是另造测试图 —— 所以这些用例同时验证「种子能发起 / 免审能直接过 / 阈值落在条件边上」。
/// 仓管主管角色（V0182 种子里的固定 UUID）。
pub(super) const ROLE_WH_MGR: &str = "20000000-0000-4000-8000-0000000f0004";
/// 仓管主管账号。
pub(super) const U_WH_MGR: &str = "b2b1000a-0000-4000-8000-000000000002";
/// 制单人（P16 夹具里的仓管员，本卡用来验证「自己提自己批」被拦）。
pub(super) const U_KEEPER: &str = "b2b10009-0000-4000-8000-000000000001";

pub(super) fn uid_of(raw: &str) -> Uuid {
    Uuid::parse_str(raw).expect("夹具 UUID 非法")
}

pub(super) async fn sql(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

// ---------- ⑬ 收尾清场：模块级 teardown（`zz_` 排在本模块最后） ----------
/// 本模块跑完**不许留**「STOCK_IN_POST 实例 / 待办 / 状态日志 / 夹具收货单」四类残留。
///
/// 为什么必须有这一条：`setup()` 只在**每个用例开头**跑，模块最后一个用例的产物没人收；
/// 同一库连跑第二遍时，遗留的 `STOCK_IN_POST | PENDING` 会顶住
/// `uk_biz_flow_instance_biz_active`（下一次提交永久 409），`biz_status_log` 则每轮多攒几十行。
/// libtest 按名字排序，`zz_` 必然最后。
///
/// 顺带钉住「孤儿也算本夹具的」这条口径：本用例造的单被清场删掉后，实例
/// （`biz_id` 是弱引用）必须**跟着一起**被 `purge_post_approval_traces` 收走 ——
/// 这正是「每跑一轮多 10 条 PENDING」的结构性缺口。
#[tokio::test]
pub(super) async fn zz_teardown_purges_post_approval_residue() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    let id = stock_in_with_amount("5000", None).await;
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .expect("提交过账审批应成功");
    let instance_id = running_post_instance(id).await;
    assert!(
        !post_pending_tasks(instance_id).await.is_empty(),
        "清场前必须有待办，否则这条用例证明不了什么"
    );

    // 走模块**真实**的收尾路径（`cleanup_fixtures`：先审批痕迹 → 状态日志 → 单据本体），
    // 而不是在用例里另写一套 —— 这样这条用例证明的就是「模块收尾真的不留残留」。
    cleanup_fixtures().await;
    // ① 实例没了（**孤儿也认**：单据已删，实例仍必须被收走）
    for (label, sql) in [
        (
            "残留实例",
            "SELECT count(*) FROM biz_flow_instance WHERE id = ?::uuid".to_string(),
        ),
        (
            "残留待办",
            "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid".to_string(),
        ),
    ] {
        let n: i64 = db()
            .scalar_with_params::<Option<i64>>(&sql, vec![Value::from(&instance_id)])
            .await
            .expect("查残留")
            .unwrap_or(0);
        assert_eq!(n, 0, "清场后{label}必须为 0");
    }
    // ② 状态日志没了（无外键 → 必须在删单据之前清）
    let logs: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_status_log \
              WHERE entity_type = 'STOCK_IN' AND entity_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("查状态日志")
        .unwrap_or(0);
    assert_eq!(logs, 0, "清场后状态日志必须为 0");
    // ③ 全库不剩「在跑的过账实例」—— 顶住唯一索引的就是它
    let running: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_flow_instance \
              WHERE biz_type = 'STOCK_IN_POST' AND status = 'PENDING' AND deleted_at IS NULL",
            vec![],
        )
        .await
        .expect("查在跑实例")
        .unwrap_or(0);
    assert_eq!(
        running, 0,
        "模块收尾后不许剩 PENDING 的过账审批实例（同库连跑第二遍的前提）"
    );
}
