// ============================================================================
// transfer_handler_db_tests_fixtures.rs —— 测试夹具：夹具 UUID / 建库连接 / 串行锁 / 准备与清理 / 取单小工具
//
// 由 `transfer_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::*;
use crate::business::erp::transfer_dto::TransferItemReq;
use crate::business::erp::transfer_dto::{CreateTransferReq, TransferStatusReq};
use crate::business::erp::transfer_model::{self, STATUS_PENDING, TransferRow};
use crate::business::erp::transfer_model::{STATUS_APPROVED, STATUS_DONE, STATUS_DRAFT, STATUS_IN_TRANSIT};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use std::str::FromStr;

// ---------- 夹具 UUID（固定值，与其他卡的夹具互不冲突） ----------
pub(super) const U_APPLICANT: &str = "d3b0000a-0000-4000-8000-000000000001";
pub(super) const U_APPROVER: &str = "d3b0000a-0000-4000-8000-000000000002";
pub(super) const WH_FROM: &str = "d3b0000b-0000-4000-8000-000000000001";
pub(super) const WH_TO: &str = "d3b0000b-0000-4000-8000-000000000002";
pub(super) const UNIT: &str = "d3b0000d-0000-4000-8000-000000000001";
/// DC-09 夹具：折算单位「箱」（1 箱 = 12 个，基准 = `W3B-PCS`）。
pub(super) const U_BOX: &str = "d3b0000d-0000-4000-8000-000000000002";
/// DC-09 夹具：挂在折算单位上的物料 —— 本版本**不能**在调拨单里使用。
///
/// UUID 特意避开 `…0003`：`w3c_settle_handles_three_lines_in_one_doc` 自己会插
/// 「W3C-M-L3 第三物料」到那个号上（`ON CONFLICT DO NOTHING`），撞号会让它的物料
/// 变成这只折算单位物料、把那条用例带红（第一版实测就撞了）。
pub(super) const M_BOX: &str = "d3b0000c-0000-4000-8000-0000000000bb";
pub(super) const M_PLAIN: &str = "d3b0000c-0000-4000-8000-000000000001";
pub(super) const M_SN: &str = "d3b0000c-0000-4000-8000-000000000002";
pub(super) const SERIAL_1: &str = "d3b0000e-0000-4000-8000-000000000001";
pub(super) const SERIAL_2: &str = "d3b0000e-0000-4000-8000-000000000002";
pub(super) const LOC_FROM: &str = "d3b0000f-0000-4000-8000-000000000001";
pub(super) const LOC_TO: &str = "d3b0000f-0000-4000-8000-000000000002";
/// 一物一码物料（`M_SN`）在调出仓的库位账行（本卡新增：库位账基线要与成本账逐仓相等）。
pub(super) const LOC_SN: &str = "d3b0000f-0000-4000-8000-000000000003";
// P5 起调拨流程是**迁移种的**（V0157），固定 UUID 与迁移里一致：
// 单测断言直接按这两个 id 定位（不按 flow_code 模糊匹配，避免同业务类型的其它夹具串味）。
pub(super) const DEF_ID: &str = "20000000-0000-4000-8000-00000000d157";
pub(super) const VER_ID: &str = "20000000-0000-4000-8000-00000000e157";

/// 真库用例**串行**执行（共用一个库，取号器是全局单行，并行会互相推进号段）。
pub(super) static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(super) fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

/// 初始化全局 db（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
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
            // 池里的空闲连接绑在创建它的 runtime 上 → 必须逼每次新建（W1b 实测）。
            let mut config = DatabaseConfig::from_url(&url);
            config.pool.max_lifetime = Some(std::time::Duration::ZERO);
            config.pool.idle_timeout = Some(std::time::Duration::ZERO);
            match Db::connect_with(&url, config).await {
                Ok(connection) => set_db(connection).is_ok(),
                Err(err) => {
                    println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

pub(super) async fn exec(sql: &str, params: Vec<Value>, what: &str) {
    if let Err(err) = db().execute_with_params(sql, params).await {
        panic!("夹具写入失败（{what}）：{err}\nSQL: {sql}");
    }
}

/// 本卡造的单据 / 实例的**定位口径**（多处复用，必须逐字一致）。
///
/// 三条同时成立才算本卡的：
/// 1. `biz_type = 'STOCK_TRANSFER'` —— 全库只有调拨模块会发这类实例；
/// 2. `biz_id` 指向本卡夹具仓库上的调拨单（**出发仓 / 目的仓任一边命中**：
///    「在途中转」用例的出发仓是在途仓，只按 `from_warehouse_id` 会漏）；
/// 3. 或者「单据行已经不存在」的孤儿实例（上一轮被 panic 打断留下的）——
///    这类既挡 `uk_biz_flow_instance_biz_active`，又没人认得，必须一起收走；
///    加 `def_id` 精确到迁移种的那条调拨流程，避免误伤别的卡的同名 biz_type 夹具。
pub(super) fn fixture_instance_where() -> String {
    "biz_type = 'STOCK_TRANSFER' AND ( \
         biz_id IN (SELECT id FROM biz_stock_transfer \
                     WHERE from_warehouse_id = ANY(?::text::uuid[]) \
                        OR to_warehouse_id = ANY(?::text::uuid[])) \
      OR (def_id = ?::uuid AND biz_id IS NOT NULL \
          AND biz_id NOT IN (SELECT id FROM biz_stock_transfer)))"
        .to_string()
}

/// 本卡单据的定位口径（`biz_stock_transfer` 上的条件，与实例口径同源）。
pub(super) fn fixture_doc_where() -> &'static str {
    "from_warehouse_id = ANY(?::text::uuid[]) OR to_warehouse_id = ANY(?::text::uuid[])"
}

/// 清掉本卡造的单据**以及它们挂的全部审批痕迹**（按夹具仓库精确定位，不扫全库）。
///
/// # 顺序为什么必须是这样（同库连跑两遍才不红）
///
/// 1. `biz_stock_transfer.approval_instance_id` 是**弱引用**（无外键）→ 先置 `NULL`。
///    不置空也能删掉实例，但单据会指着不存在的实例：`flow_task_bridge::instance_status_of`
///    查不到就按「不拦」处理（门闸失效），是排查时最费时间的那类脏数据。
/// 2. `biz_flow_instance.prev_instance_id` 是**自引用且没有 ON DELETE**（V0122）→
///    先把「指向本卡实例」的外部引用解开。漏了这一步，整批 DELETE 会被外键拦下并抛
///    `query failed: db error` —— PG 那句报错里既没有约束名也没有表名，
///    就是卡面记的「夹具写入失败（清理审批实例）」真身。
/// 3. `biz_flow_task` / `biz_flow_cc` 虽然 `ON DELETE CASCADE`，仍**显式按 instance_id 先删**：
///    把「先子表后主表」写死，将来迁移哪天摘掉 CASCADE 也不会静默留残留。
/// 4. `biz_status_log` **没有外键**（日志要比单据活得久，是设计）→ 必须在**删单据行之前**
///    按 `entity_id` 清。反过来的话，单据一删日志就永远对不上，每跑一轮多攒几百行孤儿。
/// 5. 最后才删明细 / 单据头；**不删流程定义**（P5 起由 V0157 种好，生产同款路径）。
pub(super) async fn cleanup() {
    let warehouses = vec![uuid(WH_FROM), uuid(WH_TO)];
    let docs = array_param(&warehouses);
    let doc_where = fixture_doc_where();
    let inst_where = fixture_instance_where();

    // ① 弱引用先断（单据 → 审批实例）
    exec(
        &format!("UPDATE biz_stock_transfer SET approval_instance_id = NULL WHERE {doc_where}"),
        vec![docs.clone(), docs.clone()],
        "解除单据的审批单弱引用",
    )
    .await;
    // ② 自引用外键先解开（实例 prev_instance_id，无级联）
    exec(
        &format!(
            "UPDATE biz_flow_instance SET prev_instance_id = NULL \
              WHERE prev_instance_id IN (SELECT id FROM biz_flow_instance WHERE {inst_where})"
        ),
        vec![docs.clone(), docs.clone(), Value::from(&uuid(DEF_ID))],
        "解开实例的上一轮引用",
    )
    .await;
    // ③ 待办 / 抄送：显式先删（别只依赖 CASCADE）
    for (table, what) in [("biz_flow_task", "清理待办"), ("biz_flow_cc", "清理抄送")] {
        exec(
            &format!(
                "DELETE FROM {table} WHERE instance_id IN \
                 (SELECT id FROM biz_flow_instance WHERE {inst_where})"
            ),
            vec![docs.clone(), docs.clone(), Value::from(&uuid(DEF_ID))],
            what,
        )
        .await;
    }
    // ④ 实例本体
    exec(
        &format!("DELETE FROM biz_flow_instance WHERE {inst_where}"),
        vec![docs.clone(), docs.clone(), Value::from(&uuid(DEF_ID))],
        "清理审批实例",
    )
    .await;
    // ⑤ 状态日志（无外键 → 必须在删单据之前，按 entity_id 精确清）
    exec(
        &format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'STOCK_TRANSFER' \
               AND entity_id IN (SELECT id FROM biz_stock_transfer WHERE {doc_where})"
        ),
        vec![docs.clone(), docs.clone()],
        "清理状态变更日志",
    )
    .await;
    // ⑥ 子表 → 单据头
    exec(
        "DELETE FROM biz_stock_transfer_item_serial WHERE item_id IN ( \
             SELECT i.id FROM biz_stock_transfer_item i \
               JOIN biz_stock_transfer t ON t.id = i.transfer_id \
              WHERE t.from_warehouse_id = ANY(?::text::uuid[]) \
                 OR t.to_warehouse_id = ANY(?::text::uuid[]))",
        vec![docs.clone(), docs.clone()],
        "清理序列号子表",
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_stock_transfer_item \
              WHERE transfer_id IN (SELECT id FROM biz_stock_transfer WHERE {doc_where})"
        ),
        vec![docs.clone(), docs.clone()],
        "清理明细",
    )
    .await;
    exec(
        &format!("DELETE FROM biz_stock_transfer WHERE {doc_where}"),
        vec![docs.clone(), docs.clone()],
        "清理单据头",
    )
    .await;
}

/// UUID 数组参数（技能 §6.5：`Value::Text("{a,b}")` 配 `?::text::uuid[]`）。
///
/// **花括号不能省**：PG 的数组字面量是 `{a,b}`，只写 `a,b` 会报
/// `malformed array literal`（实测：`query failed: : db error`，没有更多细节）。
pub(super) fn array_param(ids: &[Uuid]) -> Value {
    Value::Text(format!(
        "{{{}}}",
        ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",")
    ))
}

pub(super) async fn prepare() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    exec(
        "INSERT INTO sys_user (id, username, password) VALUES (?::uuid, 'w3b_applicant', 'x') \
         ON CONFLICT DO NOTHING",
        vec![Value::from(&uuid(U_APPLICANT))],
        "用户（申请人）",
    )
    .await;
    exec(
        "INSERT INTO sys_user (id, username, password) VALUES (?::uuid, 'w3b_approver', 'x') \
         ON CONFLICT DO NOTHING",
        vec![Value::from(&uuid(U_APPROVER))],
        "用户（审批人）",
    )
    .await;
    exec(
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status, sort_order) \
         VALUES (?::uuid, 'W3B-PCS', '个', 1, 1, 1, 1) ON CONFLICT DO NOTHING",
        vec![Value::from(&uuid(UNIT))],
        "计量单位",
    )
    .await;
    // DC-09 夹具：折算单位「箱」（is_base = 0，1 箱 = 12 个）+ 挂在它上面的物料。
    // 调拨单里用它必须 400 —— 明细不存 unit_id，数量直接进两本账。
    exec(
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order) \
         VALUES (?::uuid, 'W3B-BOX', '箱', 0, ?::uuid, 12, 1, 2) ON CONFLICT DO NOTHING",
        vec![Value::from(&uuid(U_BOX)), Value::from(&uuid(UNIT))],
        "折算单位",
    )
    .await;
    // 注意：V0137 的 `allow_negative` / `is_leaf` 是 SMALLINT（0/1），不是 BOOLEAN
    // （`\\d` 实测；`serial_required` 反而是 BOOLEAN —— 同一批迁移里两种风格并存）
    exec(
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, \
                                    allow_negative, status, sort_order, parent_id, level, \
                                    warehouse_path, is_leaf) \
         VALUES (?::uuid, 'WH-W3B-1', 'W3B 调出仓', 'NORMAL', 0, 1, 1, NULL, 1, '/w3b1/', 1), \
                (?::uuid, 'WH-W3B-2', 'W3B 调入仓', 'NORMAL', 0, 1, 2, NULL, 1, '/w3b2/', 1) \
         ON CONFLICT DO NOTHING",
        vec![Value::from(&uuid(WH_FROM)), Value::from(&uuid(WH_TO))],
        "仓库",
    )
    .await;
    exec(
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, \
                                   safety_stock, costing_method, tax_rate, status, serial_required) \
         VALUES (?::uuid, 'W3B-M-PLAIN', 'W3B 按数量物料', ?::uuid, 'RAW', 0, 'MOVING_AVG', 0, 1, false), \
                (?::uuid, 'W3B-M-SN', 'W3B 一物一码物料', ?::uuid, 'FINISHED', 0, 'MOVING_AVG', 0, 1, true) \
         ON CONFLICT DO NOTHING",
        vec![
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(UNIT)),
            Value::from(&uuid(M_SN)),
            Value::from(&uuid(UNIT)),
        ],
        "物料",
    )
    .await;
    exec(
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, \
                                   safety_stock, costing_method, tax_rate, status, serial_required) \
         VALUES (?::uuid, 'W3B-M-BOX', 'W3B 整箱物料', ?::uuid, 'RAW', 0, 'MOVING_AVG', 0, 1, false) \
         ON CONFLICT DO NOTHING",
        vec![Value::from(&uuid(M_BOX)), Value::from(&uuid(U_BOX))],
        "折算单位物料",
    )
    .await;
    // DC-09 补口（本卡）的用例会临时把 `M_PLAIN` 改成折算单位（模拟「绕过接口改物料 /
    // 升级前建的存量单」），这里每轮无条件复位 —— 用例中途 panic 也不会把后面的用例带红。
    exec(
        "UPDATE biz_material SET unit_id = ?::uuid WHERE id = ANY(?::text::uuid[])",
        vec![Value::from(&uuid(UNIT)), array_param(&[uuid(M_PLAIN), uuid(M_SN)])],
        "复位物料单位",
    )
    .await;
    exec(
        "INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, status) \
         VALUES (?::uuid, 'W3B-SN-0001', ?::uuid, ?::uuid, 'IN_STOCK'), \
                (?::uuid, 'W3B-SN-0002', ?::uuid, ?::uuid, 'IN_STOCK') \
         ON CONFLICT DO NOTHING",
        vec![
            Value::from(&uuid(SERIAL_1)),
            Value::from(&uuid(M_SN)),
            Value::from(&uuid(WH_FROM)),
            Value::from(&uuid(SERIAL_2)),
            Value::from(&uuid(M_SN)),
            Value::from(&uuid(WH_FROM)),
        ],
        "序列号",
    )
    .await;
    // W3c：库存台账夹具 —— 每个用例都重置到同一基线，断言才能算增量
    //   M_PLAIN 分仓 100 @12.5 / 总仓 10 @8；M_SN 分仓 2 @30（SERIAL_1/2 都在分仓 IN_STOCK）
    //   库位账（biz_inventory_location）**同步重置成同数**（见下面「库位账基线」）：
    //   本卡起 settle 会补写库位账，并在末尾断言「SUM(库位账) == SUM(成本账)」（不变式④），
    //   两账基线不相等的话每个用例都会在结算末尾被新断言拦下。
    let warehouses = vec![uuid(WH_FROM), uuid(WH_TO)];
    exec(
        "DELETE FROM biz_inventory_txn WHERE warehouse_id = ANY(?::text::uuid[])",
        vec![array_param(&warehouses)],
        "重置库存流水",
    )
    .await;
    exec(
        "DELETE FROM biz_inventory WHERE warehouse_id = ANY(?::text::uuid[])",
        vec![array_param(&warehouses)],
        "重置库存台账",
    )
    .await;
    exec(
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                                    avg_cost, amount, last_txn_at) \
         VALUES (gen_random_uuid(), ?::uuid, ?::uuid, NULL, 100, 0, 12.5, 1250, now()), \
                (gen_random_uuid(), ?::uuid, ?::uuid, NULL, 10, 0, 8, 80, now()), \
                (gen_random_uuid(), ?::uuid, ?::uuid, NULL, 2, 0, 30, 60, now())",
        vec![
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_FROM)),
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_TO)),
            Value::from(&uuid(M_SN)),
            Value::from(&uuid(WH_FROM)),
        ],
        "库存台账基线",
    )
    .await;
    // 序列号也要回到「分仓 + IN_STOCK」：上一个用例可能把它们调去总仓（ON CONFLICT 不会覆盖）
    exec(
        "UPDATE biz_material_serial \
            SET warehouse_id = ?::uuid, location_id = NULL, status = 'IN_STOCK', \
                updated_at = now() \
          WHERE id = ANY(?::text::uuid[])",
        vec![
            Value::from(&uuid(WH_FROM)),
            array_param(&[uuid(SERIAL_1), uuid(SERIAL_2)]),
        ],
        "重置序列号位置",
    )
    .await;
    // 库位账基线（本卡新增）：先把可能指向本仓库位账行的引用清掉（三张表的 location_id
    // 都是硬外键 → `biz_inventory_location(id)`），再整仓重建三行，数量与上面的成本账基线
    // **逐条相等**。固定 id（LOC_FROM / LOC_TO / LOC_SN）供「带库位的调拨」用例按行 id 指定。
    exec(
        "UPDATE biz_material_serial SET location_id = NULL \
          WHERE warehouse_id = ANY(?::text::uuid[]) AND location_id IS NOT NULL",
        vec![array_param(&warehouses)],
        "解除序列号库位引用",
    )
    .await;
    exec(
        "UPDATE biz_stock_transfer_item SET from_location_id = NULL \
          WHERE from_location_id IN (SELECT id FROM biz_inventory_location \
                                      WHERE warehouse_id = ANY(?::text::uuid[]))",
        vec![array_param(&warehouses)],
        "解除明细调出库位引用",
    )
    .await;
    exec(
        "UPDATE biz_stock_transfer_item SET to_location_id = NULL \
          WHERE to_location_id IN (SELECT id FROM biz_inventory_location \
                                    WHERE warehouse_id = ANY(?::text::uuid[]))",
        vec![array_param(&warehouses)],
        "解除明细调入库位引用",
    )
    .await;
    exec(
        "DELETE FROM biz_inventory_location WHERE warehouse_id = ANY(?::text::uuid[])",
        vec![array_param(&warehouses)],
        "重置库位账",
    )
    .await;
    exec(
        "INSERT INTO biz_inventory_location \
             (id, material_id, warehouse_id, location_id, qty, locked_qty) \
         VALUES (?::uuid, ?::uuid, ?::uuid, NULL, 100, 0), \
                (?::uuid, ?::uuid, ?::uuid, NULL, 10, 0), \
                (?::uuid, ?::uuid, ?::uuid, NULL, 2, 0)",
        vec![
            Value::from(&uuid(LOC_FROM)),
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_FROM)),
            Value::from(&uuid(LOC_TO)),
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_TO)),
            Value::from(&uuid(LOC_SN)),
            Value::from(&uuid(M_SN)),
            Value::from(&uuid(WH_FROM)),
        ],
        "库位账基线",
    )
    .await;
    // ⑥ 的状态（调拨流程）也在这里自愈：上一个用例若 panic 没恢复软删，后续用例会串味
    ensure_seed_flow().await;
    Some(guard)
}

/// 断言「迁移 V0157 已经把调拨流程种好」，并返回 `(def_id, version_id)`。
///
/// P5 起**不再由夹具造流程**：调拨流程是迁移种子（生产同款路径），被测代码读的就是它。
/// 夹具只负责「把可能被别的用例改坏的状态复原」：
/// - 上一次 ⑥ 用例把流程软删后若 panic 没来得及恢复 → 这里 `deleted_at = NULL` 自愈；
/// - 流程被改成非 PUBLISHED（不该发生，但自愈一次代价极低）→ 复原。
///
/// # 为什么不能把 `current_version_id` 写死成 v1（卡 TEST-ISO-2 实测）
///
/// V0251（P0-FIX-2B-B 流程去 ADMIN 化）给 `stock_transfer` 发了 v2 并把 v1 归档，
/// `biz_flow_def.current_version_id` 指向 v2。旧版这里硬写 `current_version_id = VER_ID`（= v1）
/// ⇒ 定义行指着一个 `ARCHIVED` 版本，自愈反而把库改坏，**52 条 transfer 用例全部红在
/// `prepare()` 的「① 当前版本必须是 PUBLISHED」**（left: "ARCHIVED"）。
///
/// 所以自愈只改「定义行状态」，当前版本一律从库里**读**（`is_current`，取号最大的那行），
/// 只有库里连 `is_current` 行都没有时才退回 v1 兜底。
///
/// # Panics
///
/// 迁移没应用（测试库停在 V0156 之前）→ 直接 panic 并给出可操作的提示。
pub(super) async fn ensure_seed_flow() -> (Uuid, Uuid) {
    exec(
        "UPDATE biz_flow_def d SET status = 'PUBLISHED', is_default = TRUE, deleted_at = NULL, \
                 current_version_id = COALESCE((SELECT v.id FROM biz_flow_def_version v \
                     WHERE v.def_id = d.id AND v.is_current ORDER BY v.version_no DESC LIMIT 1), \
                     ?::uuid), \
                 current_version_no = COALESCE((SELECT v.version_no FROM biz_flow_def_version v \
                     WHERE v.def_id = d.id AND v.is_current ORDER BY v.version_no DESC LIMIT 1), 1) \
          WHERE d.id = ?::uuid",
        vec![Value::from(&uuid(VER_ID)), Value::from(&uuid(DEF_ID))],
        "自愈：调拨种子流程状态",
    )
    .await;
    let rows = db()
        .find_with_params(
            "SELECT d.status, d.is_default, d.biz_type, d.current_version_id::text AS cur_ver, \
                    v.version_status \
               FROM biz_flow_def d JOIN biz_flow_def_version v ON v.id = d.current_version_id \
              WHERE d.id = ?::uuid AND d.deleted_at IS NULL",
            vec![Value::from(&uuid(DEF_ID))],
        )
        .await
        .expect("查询调拨种子流程");
    let row = rows.first().unwrap_or_else(|| {
        panic!(
            "测试库缺少 stock_transfer 流程（V0157 未应用？）：\
             先对测试库跑一次 `target/debug/cloudpivot`（迁移自动应用），再重跑本套用例"
        )
    });
    assert_eq!(
        row.get_typed::<String>("status").expect("status"),
        "PUBLISHED",
        "① 种子流程必须是 PUBLISHED"
    );
    assert!(
        row.get_typed::<bool>("is_default").expect("is_default"),
        "① 种子流程必须是 is_default（提交审批时按 biz_type 取默认流程）"
    );
    assert_eq!(row.get_typed::<String>("biz_type").expect("biz_type"), "STOCK_TRANSFER");
    assert_eq!(
        row.get_typed::<String>("version_status").expect("version_status"),
        "PUBLISHED",
        "① 当前版本必须是 PUBLISHED"
    );
    // 返回库里真实的当前版本（V0251 起是 v2），而不是写死的 v1：
    // 下游用例（⑰-b 的图/hash 自洽）断言的是「当前版本」的内容。
    let cur_ver = row.get_typed::<String>("cur_ver").expect("cur_ver");
    (uuid(DEF_ID), Uuid::parse_str(&cur_ver).expect("当前版本 id 非法"))
}

pub(super) fn item(material_id: &str, qty: &str) -> TransferItemReq {
    TransferItemReq {
        material_id: material_id.to_string(),
        from_location_id: None,
        to_location_id: None,
        qty: Decimal::from_str(qty).expect("夹具数量"),
        serial_ids: Vec::new(),
    }
}

pub(super) fn body(items: Vec<TransferItemReq>) -> CreateTransferReq {
    CreateTransferReq {
        from_warehouse_id: WH_FROM.to_string(),
        to_warehouse_id: WH_TO.to_string(),
        transfer_date: None,
        reason: Some("W3B 单测".to_string()),
        remark: None,
        items,
    }
}

pub(super) fn status_req(status: &str, reason: Option<&str>) -> TransferStatusReq {
    TransferStatusReq {
        status: status.to_string(),
        reason: reason.map(str::to_string),
    }
}

pub(super) async fn refresh(id: Uuid) -> TransferRow {
    transfer_model::find_by_id(id)
        .await
        .expect("查询调拨单")
        .expect("调拨单必须存在")
}

/// sha256 小写十六进制（与 `graph::graph_hash` 同口径：对 canonical JSON 取 digest）。
pub(super) fn hex_sha256(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 某物料在某仓的整仓库存数量（`location_id IS NULL` 的汇总桶），返回库里的 `numeric::text`。
pub(super) async fn inventory_qty(material_id: &str, warehouse_id: &str) -> String {
    let rows = db()
        .find_with_params(
            "SELECT qty::text AS qty FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
            vec![Value::from(&uuid(material_id)), Value::from(&uuid(warehouse_id))],
        )
        .await
        .expect("查库存数量");
    rows.first().expect("库存行必须存在").get_typed("qty").expect("取 qty")
}

/// 某物料在某仓的整仓库存金额。
pub(super) async fn inventory_amount(material_id: &str, warehouse_id: &str) -> String {
    let rows = db()
        .find_with_params(
            "SELECT amount::text AS amount FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
            vec![Value::from(&uuid(material_id)), Value::from(&uuid(warehouse_id))],
        )
        .await
        .expect("查库存金额");
    rows.first()
        .expect("库存行必须存在")
        .get_typed("amount")
        .expect("取 amount")
}

/// 数值比较（`NUMERIC(18,4)` 经 text 回来是 `95.0000`，逐字比字符串会假红 —— W3c 的教训）。
pub(super) fn assert_dec_eq(actual: String, expected: &str, what: &str) {
    let left = Decimal::from_str(&actual).unwrap_or_else(|_| panic!("{what}：实际值不是数字 [{actual}]"));
    let right = Decimal::from_str(expected).unwrap_or_else(|_| panic!("{what}：期望值非法 [{expected}]"));
    assert_eq!(left, right, "{what}：期望 {expected} 实际 {actual}");
}

/// 走**生产同款路径**让审批单落终态：引擎在事务里投票 → 提交 → 回调调拨单。
///
/// P5 之前夹具可以「人工直推 APPROVED」把单据推过审批；现在那条路被门闸（65030）拦住了，
/// 夹具必须真的去审（也正好把「审批通过 → 调拨单 APPROVED」这条主路径跑在单测里）。
/// 实现见文件级 `approve_flow_instance`（两套真库用例共用）。
pub(super) async fn approve_instance(instance_id: Uuid) {
    super::approve_flow_instance(instance_id).await;
}

/// 建一张单并推进到指定状态（测试的公共前置）。
///
/// `PENDING → APPROVED` 这一步：**挂了审批单就走审批回调**（门闸 65030 不许人工直推），
/// 没挂审批单（降级路径）才人工推。
pub(super) async fn transfer_in_status(target: &str) -> (Uuid, TransferRow) {
    let applicant = uuid(U_APPLICANT);
    let id = create_transfer(&body(vec![item(M_PLAIN, "5")]), Some(applicant), Some("w3b_applicant"))
        .await
        .expect("建单必须成功");
    if target == STATUS_DRAFT {
        return (id, refresh(id).await);
    }
    change_transfer_status(id, &status_req(STATUS_PENDING, None), applicant)
        .await
        .unwrap_or_else(|err| panic!("推进到 {STATUS_PENDING} 失败：{}", err.msg));
    if target == STATUS_PENDING {
        return (id, refresh(id).await);
    }
    // → APPROVED
    match refresh(id).await.approval_instance_id {
        Some(instance_id) => approve_instance(instance_id).await,
        None => {
            change_transfer_status(id, &status_req(STATUS_APPROVED, None), applicant)
                .await
                .unwrap_or_else(|err| panic!("降级路径推进到 APPROVED 失败：{}", err.msg));
        }
    }
    for step in [STATUS_IN_TRANSIT, STATUS_DONE] {
        if target == STATUS_APPROVED {
            break;
        }
        change_transfer_status(id, &status_req(step, None), applicant)
            .await
            .unwrap_or_else(|err| panic!("推进到 {step} 失败：{}", err.msg));
        if step == target {
            break;
        }
    }
    (id, refresh(id).await)
}

// ==================== W3c：IN_TRANSIT → DONE 的库存双写 + 成本原价转移 ====================
//
// 夹具基线（[`prepare`] 每个用例都重置）：
//
// ```text
// M_PLAIN（按数量）  分仓 100 件 @ 12.5 = 1250        总仓 10 件 @ 8 = 80
// M_SN  （一物一码）分仓 2 件 @ 30 = 60（SERIAL_1/2 都在分仓 IN_STOCK）
// ```
/// 夹具数字（`Decimal` 的 `FromStr` 在测试里读起来比 `Decimal::new(5, 0)` 清楚）。
pub(super) fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("夹具数字")
}

pub(super) async fn query(sql: &str, params: Vec<Value>) -> Vec<Row> {
    db().find_with_params(sql, params)
        .await
        .unwrap_or_else(|err| panic!("查询失败：{err}\nSQL: {sql}"))
}

/// 某物料在某仓**整仓汇总桶**的账面数量（行不存在 → `None`）。
pub(super) async fn bucket_qty(material: &str, warehouse: &str) -> Option<Decimal> {
    let rows = query(
        "SELECT qty::numeric(18,4) AS qty FROM biz_inventory \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
        vec![Value::from(&uuid(material)), Value::from(&uuid(warehouse))],
    )
    .await;
    rows.first().map(|row| row.get_typed::<Decimal>("qty").expect("qty"))
}

pub(super) async fn bucket_amount(material: &str, warehouse: &str) -> Option<Decimal> {
    let rows = query(
        "SELECT amount::numeric(18,4) AS amount FROM biz_inventory \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
        vec![Value::from(&uuid(material)), Value::from(&uuid(warehouse))],
    )
    .await;
    rows.first()
        .map(|row| row.get_typed::<Decimal>("amount").expect("amount"))
}

pub(super) async fn bucket_avg_cost(material: &str, warehouse: &str) -> Option<Decimal> {
    let rows = query(
        "SELECT avg_cost::numeric(18,6) AS avg_cost FROM biz_inventory \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
        vec![Value::from(&uuid(material)), Value::from(&uuid(warehouse))],
    )
    .await;
    rows.first()
        .map(|row| row.get_typed::<Decimal>("avg_cost").expect("avg_cost"))
}

pub(super) async fn needed(material: &str, warehouse: &str) -> Decimal {
    bucket_qty(material, warehouse)
        .await
        .unwrap_or_else(|| panic!("{material} @ {warehouse} 的库存行必须存在"))
}

/// 某张单的库存流水（按方向排序，便于逐条断言）。
pub(super) async fn txns_of(transfer_id: Uuid) -> Vec<Row> {
    query(
        "SELECT txn_type, biz_type, doc_type, doc_no, line_no, \
                qty::numeric(18,4) AS qty, unit_cost::numeric(18,6) AS unit_cost, \
                amount::numeric(18,4) AS amount, \
                before_qty::numeric(18,4) AS before_qty, after_qty::numeric(18,4) AS after_qty, \
                before_amount::numeric(18,4) AS before_amount, \
                after_amount::numeric(18,4) AS after_amount, warehouse_id \
           FROM biz_inventory_txn \
          WHERE doc_type = 'STOCK_TRANSFER' AND doc_id = ?::uuid \
          ORDER BY txn_type, line_no",
        vec![Value::from(&transfer_id)],
    )
    .await
}

/// 文本列（`txn_type` / `biz_type` / `doc_no` …）。
pub(super) fn row_text(row: &Row, column: &str) -> String {
    row.get_typed::<String>(column)
        .unwrap_or_else(|err| panic!("列 {column} 不是文本：{err}"))
}

/// 数值列取字符串（断言里逐字比对 `"5.0000"` 比 `Decimal` 相等更能钉住标度）。
pub(super) fn txn_text(row: &Row, column: &str) -> String {
    row.get_typed::<Decimal>(column)
        .unwrap_or_else(|err| panic!("列 {column} 不是数字：{err}"))
        .to_string()
}

pub(super) async fn serial_warehouse(serial_id: &str) -> Option<Uuid> {
    let rows = query(
        "SELECT warehouse_id FROM biz_material_serial WHERE id = ?::uuid",
        vec![Value::from(&uuid(serial_id))],
    )
    .await;
    rows.first()
        .map(|row| row.get_typed("warehouse_id").expect("warehouse_id"))
}

pub(super) async fn serial_in_stock_count(material: &str, warehouse: &str) -> i64 {
    let rows = query(
        "SELECT COUNT(*) AS cnt FROM biz_material_serial \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
            AND status = 'IN_STOCK' AND deleted_at IS NULL",
        vec![Value::from(&uuid(material)), Value::from(&uuid(warehouse))],
    )
    .await;
    rows.first()
        .map(|row| row.get_typed::<i64>("cnt").expect("cnt"))
        .unwrap_or(0)
}

/// 建单 → PENDING →（真的走审批）→ APPROVED → IN_TRANSIT（结算前的公共前置）。
pub(super) async fn create_and_ship(items: Vec<TransferItemReq>) -> Uuid {
    create_and_ship_with(body(items)).await
}

/// 同上，但请求体由调用方给（U3 需要反向调拨：`WH_TO → WH_FROM`）。
pub(super) async fn create_and_ship_with(payload: CreateTransferReq) -> Uuid {
    let applicant = uuid(U_APPLICANT);
    let id = create_transfer(&payload, Some(applicant), Some("w3b_applicant"))
        .await
        .expect("建单必须成功");
    change_transfer_status(id, &status_req(STATUS_PENDING, None), applicant)
        .await
        .unwrap_or_else(|err| panic!("推进到 {STATUS_PENDING} 失败：{}", err.msg));
    // PENDING → APPROVED 有人工门闸（65030）：挂了审批单就必须走审批主线
    match transfer_model::find_by_id(id)
        .await
        .expect("查调拨单")
        .and_then(|row| row.approval_instance_id)
    {
        Some(instance_id) => super::approve_flow_instance(instance_id).await,
        None => {
            change_transfer_status(id, &status_req(STATUS_APPROVED, None), applicant)
                .await
                .unwrap_or_else(|err| panic!("推进到 {STATUS_APPROVED} 失败：{}", err.msg));
        }
    }
    change_transfer_status(id, &status_req(STATUS_IN_TRANSIT, None), applicant)
        .await
        .unwrap_or_else(|err| panic!("推进到 {STATUS_IN_TRANSIT} 失败：{}", err.msg));
    id
}

/// 收货（= 触发 W3c 的库存双写）。
pub(super) async fn receive(id: Uuid) -> std::result::Result<(), ApiError> {
    change_transfer_status(id, &status_req(STATUS_DONE, None), uuid(U_APPLICANT)).await
}

pub(super) async fn receive_ok(id: Uuid) {
    receive(id)
        .await
        .unwrap_or_else(|err| panic!("收货结算必须成功：{} {}", err.code, err.msg));
}

// ==================== U1–U3：调拨结算补写库位账（t_270b472e） ====================
//
// 缺口：`settle_transfer` 原来只维护成本账（`biz_inventory`），库位账
// （`biz_inventory_location`）一行不写 ⇒ 任何一次调拨结算后，巡检 B1
// （`SUM(库位账) = SUM(成本账)`，BLOCKER）在调出仓/调入仓两侧同时变红。
// 下面三条用例覆盖卡面 U1/U2/U3，全部真库、走生产同款路径
// （`create_transfer → 审批 → IN_TRANSIT → DONE` → `settle_transfer`）。
/// 某物料在某仓的**成本账**合计（巡检 B1 的右端：`SUM(biz_inventory.qty)`）。
pub(super) async fn cost_book(material: &str, warehouse: &str) -> Decimal {
    let rows = query(
        "SELECT COALESCE(SUM(qty), 0)::numeric(18,4) AS qty FROM biz_inventory \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&uuid(material)), Value::from(&uuid(warehouse))],
    )
    .await;
    rows[0].get_typed::<Decimal>("qty").expect("成本账合计")
}

/// 某物料在某仓的**库位账**合计（巡检 B1 的左端：`SUM(biz_inventory_location.qty)`）。
pub(super) async fn location_book(material: &str, warehouse: &str) -> Decimal {
    let rows = query(
        "SELECT COALESCE(SUM(qty), 0)::numeric(18,4) AS qty FROM biz_inventory_location \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&uuid(material)), Value::from(&uuid(warehouse))],
    )
    .await;
    rows[0].get_typed::<Decimal>("qty").expect("库位账合计")
}

/// 某条库位账行（按 `biz_inventory_location.id`）当前数量。
pub(super) async fn location_row(location_row_id: &str) -> Decimal {
    let rows = query(
        "SELECT qty::numeric(18,4) AS qty FROM biz_inventory_location WHERE id = ?::uuid",
        vec![Value::from(&uuid(location_row_id))],
    )
    .await;
    rows.first()
        .unwrap_or_else(|| panic!("库位账行 {location_row_id} 必须存在"))
        .get_typed::<Decimal>("qty")
        .expect("库位账行 qty")
}

/// 「两本账相符」断言（逐仓，与 `assert_settle_invariants` 第 4 条 / 巡检 B1 同判据）。
pub(super) async fn assert_two_books(material: &str, ctx: &str) {
    for warehouse in [WH_FROM, WH_TO] {
        let cost = cost_book(material, warehouse).await;
        let loc = location_book(material, warehouse).await;
        assert!(
            (cost - loc).abs() <= dec("0.0001"),
            "{ctx}：{material} @ {warehouse} 两本账不符（成本账 {cost} / 库位账 {loc}）"
        );
    }
}

// ---------- D9：复制为新单 ----------
/// 复制用的假令牌（`permissions` 决定 `has_permission`）。
pub(super) fn copy_claims(perms: &[&str]) -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: U_APPLICANT.to_string(),
        username: Some("w3b_applicant".to_string()),
        roles: Vec::new(),
        permissions: perms.iter().map(|p| (*p).to_string()).collect(),
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    }
}

// ---------- ㉑ 提交审批撞唯一索引 → 409 + 单号（不是 500） ----------
/// 标量计数（`i64`）—— 收尾清场断言用。
pub(super) async fn count_of(sql: &str, params: Vec<Value>, what: &str) -> i64 {
    db().scalar_sql(sql.to_string(), params)
        .await
        .unwrap_or_else(|err| panic!("计数失败（{what}）：{err}\nSQL: {sql}"))
}
