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
/// P2-4：默认库位候选（用例把 `is_default` 临时置 1，cleanup 每轮复位）。
pub(super) const LOC_DEFAULT: &str = "b2b10004-0000-4000-8000-000000000001";
/// P2-4：容量 50 的小库位（超容量预警用例）。
pub(super) const LOC_CAP: &str = "b2b10004-0000-4000-8000-000000000002";
/// P2-4：停用库位（status = 0）。
pub(super) const LOC_DISABLED: &str = "b2b10004-0000-4000-8000-000000000003";
/// P2-4：已软删库位。
pub(super) const LOC_DELETED: &str = "b2b10004-0000-4000-8000-000000000004";
/// P2-4：第二个仓库（跨仓库位被拒用例）。
pub(super) const WAREHOUSE_B: &str = "b2b10004-0000-4000-8000-000000000010";
/// P2-4：第二个仓库下的库位。
pub(super) const LOC_IN_WAREHOUSE_B: &str = "b2b10004-0000-4000-8000-000000000011";

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
    // ==================== P2-4：库位做完整 ====================
    // ① 默认库位候选：`is_default` 由用例临时置 1（cleanup 复位），避免影响既有用例的 NULL 桶口径
    // ② 容量 50 的小库位（超容量预警用例）③ 停用位 ④ 已软删位
    "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                               is_default, capacity, status, sort_order)
     VALUES ('b2b10004-0000-4000-8000-000000000001', 'P2B1-L03', 'P2B1 默认位（用例置默认）',
             'b2b10001-0000-4000-8000-000000000003', 'STORAGE', 0, NULL, 1, 3),
            ('b2b10004-0000-4000-8000-000000000002', 'P2B1-L04', 'P2B1 小库位（容量 50）',
             'b2b10001-0000-4000-8000-000000000003', 'STORAGE', 0, 50, 1, 4),
            ('b2b10004-0000-4000-8000-000000000003', 'P2B1-L05', 'P2B1 停用位',
             'b2b10001-0000-4000-8000-000000000003', 'STORAGE', 0, NULL, 0, 5)
     ON CONFLICT DO NOTHING",
    // 已软删位（`deleted_at` 非空；库位编码可复用，所以不受部分唯一索引约束）
    "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                               is_default, capacity, status, sort_order, deleted_at)
     VALUES ('b2b10004-0000-4000-8000-000000000004', 'P2B1-L06', 'P2B1 已删位',
             'b2b10001-0000-4000-8000-000000000003', 'STORAGE', 0, NULL, 1, 6,
             CURRENT_TIMESTAMP)
     ON CONFLICT DO NOTHING",
    // 第二个仓库 + 它的库位（跨仓库位被拒的用例；本夹具的库位不属于它）
    "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
     VALUES ('b2b10004-0000-4000-8000-000000000010', 'P2B1-WH2', 'P2B1 二号仓',
             '/b2b10004-0000-4000-8000-000000000010/', 1, 1, 1)
     ON CONFLICT DO NOTHING",
    "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                               is_default, capacity, status, sort_order)
     VALUES ('b2b10004-0000-4000-8000-000000000011', 'P2B1-W2-L01', '二号仓一号位',
             'b2b10004-0000-4000-8000-000000000010', 'STORAGE', 0, NULL, 1, 1)
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

/// 本模块夹具供应商的 id 列表 —— **夹具删除集的根**（本模块独占的固定 uuid 主数据行）。
///
/// 夹具造的所有单据（收货单 / 采购单 / 凭证 / 应付 / 状态日志）都能沿外键链
/// 从这两个供应商 id 派生出来，所以清场不需要（也不允许）按单号前缀 `B1-%` /
/// 期次 / `biz_type` 扫全表（卡 `t_bfa81333`：这种写法在并行线程下会删掉兄弟用例的行）。
pub(super) fn fixture_suppliers() -> String {
    format!("'{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid")
}

/// 本夹具的「过账 / 作废」审批实例定位口径（**按本模块 id 集合**，不扫全库）。
///
/// 成立条件只有一个：`biz_type` 是本模块的两条审批流类型，且 `biz_id` 指向
/// **本夹具供应商**的收货单（→ 夹具删除集的根，见 [`fixture_suppliers`]）。
///
/// ⚠️ 这里**不再**带 `biz_id IS NULL OR biz_id NOT IN (SELECT id FROM biz_stock_in)`
/// 这半句「全库孤儿扫」：那是上一轮残留的口径，放在每例 `setup()` 里等于
/// 「谁跑 setup 谁扫全库」，并行时会删掉别的模块/别的进程正在用的实例
/// （审计 S3/S5 同族问题）。孤儿收口只放在模块级 `zz_` 收尾做，且是收敛式判定
/// （见 [`zz_module_namespace_has_no_residue`]）。
pub(super) fn post_instance_where() -> String {
    format!(
        "biz_type IN ('STOCK_IN_POST', 'STOCK_IN_CANCEL') AND biz_id IN \
         (SELECT id FROM biz_stock_in WHERE supplier_id IN ({}))",
        fixture_suppliers()
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
        // P2-4：默认库位是**用例临时置的**（每轮复位，免得一条用例 panic 之后拖垮后面所有用例）
        format!("UPDATE biz_location SET is_default = 0 WHERE id = '{LOC_DEFAULT}'::uuid"),
        // P2-4：第二个仓库的台账（跨仓库用例万一写进去了也要收干净）
        format!("DELETE FROM biz_inventory_location WHERE warehouse_id = '{WAREHOUSE_B}'::uuid"),
        format!("DELETE FROM biz_inventory WHERE warehouse_id = '{WAREHOUSE_B}'::uuid"),
        // P16：某条用例会把种子流程临时下线（降级路径），这里无条件恢复，
        // 免得一条用例 panic 之后拖垮后面所有用例
        "UPDATE biz_flow_def SET deleted_at = NULL WHERE flow_code = 'stock_in_cancel'".to_string(),
        // 作废审批实例已由上面的 `purge_post_approval_traces` 一并清掉（含孤儿）。
        // P10：状态日志没有外键，必须在删单据之前按「本夹具收货单 id 集合」清掉。
        // ⚠️ 这里**不再**收「entity_id 已指向不存在的收货单」的孤儿行（审计 S3）：
        //    那是「上一轮残留」的口径，放在每例 setup 里 = 谁跑 setup 谁扫全表，
        //    并行时会删掉别处正在写的行。孤儿收口只在 `zz_` 收尾做（收敛式，见文件末尾），
        //    而**本轮**产生的日志一条都不会变孤儿 —— 它们先于单据被按 id 删掉。
        format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'STOCK_IN' AND entity_id IN \
             (SELECT id FROM biz_stock_in WHERE supplier_id IN ({}))",
            fixture_suppliers()
        ),
        // ⚠️ 这里**不再**有 `entity_no LIKE 'B1-%'`（审计 S4）：单号是弱引用，
        //    按前缀删等于「按号段扫全表」。本轮日志已由上面按 id 清掉，历史孤儿由 `zz_` 收口。
        // 作废路径会为凭证 / 应付写状态日志（P10 起所有实体都写），而日志**没有外键**：
        // 单据删了日志还在。所以要在删凭证/应付**之前**、按「本夹具收货单 → 凭证/应付」的
        // id 链清掉（审计 S5：过去这里扫的是「全库 PAYABLE/VOUCHER 孤儿」，
        // 会删掉 voucher / payable_payment 模块正在写的行 —— 夹具只许动自己造的行）。
        format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
             (SELECT id FROM biz_voucher WHERE source_id IN \
              (SELECT id FROM biz_stock_in WHERE supplier_id IN ({})))",
            fixture_suppliers()
        ),
        format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'PAYABLE' AND entity_id IN \
             (SELECT id FROM biz_payable WHERE source_id IN \
              (SELECT id FROM biz_stock_in WHERE supplier_id IN ({})))",
            fixture_suppliers()
        ),
        format!(
            "DELETE FROM biz_material_serial WHERE source_type = 'PURCHASE' AND source_id IN \
         (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))"
        ),
        format!(
            "DELETE FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id IN \
         (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))"
        ),
        // ★ P2-4：库位用例会**出库**（验证 FIFO / 指定库位扣减），出库流水挂在
        // `doc_type='STOCK_OUT'` 上，上面那条删不到 —— 留着会让后续用例的
        // 「流水累计 = 当前库存」（B4）差出这批出库量。按「夹具物料 × 夹具仓库」清，
        // 不按单据类型清：这台库上的这些物料都只属于本模块。
        format!(
            "DELETE FROM biz_inventory_txn WHERE material_id IN \
         ('{MATERIAL_A}'::uuid, '{MATERIAL_B}'::uuid, '{MATERIAL_SN}'::uuid) \
         AND warehouse_id = '{WAREHOUSE}'::uuid"
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
        // 单据删了日志还在。所以要在删凭证/应付**之前**、按「本夹具收货单 → 凭证/应付」的
        // id 链清掉 —— 那两条语句排在前面（紧跟 STOCK_IN 日志之后），这里不再重复。
        format!(
            "DELETE FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)"
        ),
        // 卡 t_8ec0a1af（**必须排在删采购单之前**）：收货过账会推进采购单状态
        // （`SUBMITTED → PARTIAL_RECEIVED / RECEIVED`，见 `stockin_purchase.rs`），
        // 而 `biz_status_log` **没有外键** —— 只在下面那条里删 `biz_purchase_order`，
        // 采购单的状态日志就永远留在库里。实测后果：每轮 stockin +35 条
        // `entity_type='PURCHASE_ORDER'` 孤儿，7 轮 245 条、线性无上界
        // （卡 t_25926f63 并发自检用 psql 独立复查抓到；此前没有任何断言覆盖它，所以一直不红）。
        //
        // 删除集口径与上面几条一致：根 = **夹具供应商 id**（[`fixture_suppliers`]），
        // 沿 `biz_purchase_order.id` 取本模块历次运行造过的采购单 id 集合；
        // 既不按 `entity_no LIKE 'B1-%'` 前缀扫（单号是弱引用，见 S4），
        // 也不按「全库孤儿」（`entity_id NOT IN (SELECT id FROM biz_purchase_order)`）扫 ——
        // 后者会在并行时删掉别的模块「已删单据、还没删日志」的毫秒级中间态。
        format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'PURCHASE_ORDER' AND entity_id IN \
             (SELECT id FROM biz_purchase_order WHERE supplier_id IN ({}))",
            fixture_suppliers()
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
        location_dict_id: None,
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

/// 同 [`line`]，但带上「用户选的库位」（`biz_location.id`，P2-4 的推荐口径）。
pub(super) fn line_dict(
    material: &str,
    qty: &str,
    price: &str,
    rate: &str,
    location_dict: &str,
) -> StockInItemReq {
    StockInItemReq {
        location_dict_id: Some(location_dict.to_string()),
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

// ---------- ⑬ 收尾自证：清场按 id 生效 + 模块命名空间口径复查 ----------
//
// 下面两条**都不承担清场职责**（R5）：清场在每例 `setup()` 前跑，且只删本模块 id 集合。
// 它们只证明「清场真的有效」与「模块命名空间最终干净」，因此单跑、并行跑、排在模块
// 中间跑，行为都一样 —— 不再依赖「libtest 按名字排序 ⇒ `zz_` 最后跑」
// （`--test-threads=4` 下这个前提不成立，卡 `t_9476d9f1` 实测）。

/// 过账审批的痕迹（实例 / 待办 / 状态日志）会被模块清场**按本用例 id** 收干净。
///
/// 原 `zz_teardown_purges_post_approval_residue` 的断言全部保留，只改两处机制：
/// ① 「清场靠 `zz_` 排序」→ 改成每例 `setup()` 自清（本用例只验证清场函数有效）；
/// ② 「全库不许剩 PENDING 过账实例」→ 收到**本模块命名空间 + 收敛判定**，
///    见 [`zz_module_namespace_has_no_residue`]。
#[tokio::test]
pub(super) async fn post_approval_residue_is_purged_by_cleanup() {
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
    // ① 实例 / 待办没了（按本用例的 instance_id 精确查）
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
    // ② 状态日志没了（无外键 → 必须在删单据之前按 id 清）
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
}

/// 采购单状态日志（无外键）会被模块清场**按本夹具采购单 id 集合**收干净（卡 `t_8ec0a1af`）。
///
/// # 这条用例守的是什么
///
/// 过去的缺口：清场只删 `biz_purchase_order`，不删 `entity_type = 'PURCHASE_ORDER'` 的
/// `biz_status_log` ⇒ 每轮 stockin 在库里攒 35 条孤儿（7 轮 245 条、线性无上界，
/// 卡 `t_25926f63` 用 psql 独立复查抓到），而 `zz_` 断言只复查 `STOCK_IN`，
/// 所以**没有任何用例会因此变红**。
///
/// 走的是**真实路径**，不是另写一套 SQL：造采购单 → 建收货单 → 过账（状态机把采购单推到
/// `RECEIVED` 并写一行日志）→ 调模块真实的 `cleanup_fixtures()` → 按 `order_id` 精确复查。
#[tokio::test]
pub(super) async fn purchase_order_status_logs_are_purged_by_cleanup() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = seed_purchase_order("30", "0").await;
    let mut one = line(MATERIAL_A, "30", "2", "0");
    one.purchase_order_item_id = Some(item_id.to_string());
    let mut request = req(vec![one]);
    request.source_order_id = Some(order_id.to_string());
    let id = create_stock_in(&request, None)
        .await
        .unwrap_or_else(|err| panic!("有源单应当建得出来：{} {}", err.code, err.msg));
    change_stock_in_status(id, &post_req(), None)
        .await
        .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));

    assert!(
        po_status_logs(order_id).await > 0,
        "过账必须给采购单写状态日志，否则这条用例证明不了什么"
    );

    cleanup_fixtures().await;

    assert_eq!(
        po_status_logs(order_id).await,
        0,
        "清场后采购单状态日志必须为 0（`biz_status_log` 没有外键 ⇒ 必须在删采购单之前按 id 删）"
    );
    let left: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
        )
        .await
        .expect("查采购单本体")
        .unwrap_or(0);
    assert_eq!(left, 0, "采购单本体也必须被清场收掉");
}

/// 指定采购单的 `biz_status_log` 行数（`entity_type = 'PURCHASE_ORDER'`）。
async fn po_status_logs(order_id: Uuid) -> i64 {
    db().scalar_with_params::<Option<i64>>(
        "SELECT count(*) FROM biz_status_log \
          WHERE entity_type = 'PURCHASE_ORDER' AND entity_id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await
    .expect("查采购单状态日志")
    .unwrap_or(0)
}

/// 模块命名空间最终无残留（**只做复查，不做清场**）。
///
/// 口径全部落在「本模块私有 / 夹具 id」上，不是整库：
/// 1. **夹具供应商名下的收货单 = 0**（`supplier_id IN (夹具供应商 id)`，夹具删除集的根）；
/// 2. **`STOCK_IN_POST` / `STOCK_IN_CANCEL` 命名空间内无 PENDING 实例** —— 顶住
///    `uk_biz_flow_instance_biz_active` 的就是它，留着会让下一轮提交永久 409。
///    这两种 `biz_type` 只有本模块产生（`stockin_query::POST_BIZ_TYPE` / `CANCEL_BIZ_TYPE`），
///    且本模块测试全在 `DB_LOCK` 内串行，所以「命名空间内还有 PENDING」必然是自己的残留；
/// 3. **`STOCK_IN` / `PURCHASE_ORDER` 状态日志没有孤儿行** —— 用**收敛式判定**
///    （`fixture_hygiene::settle_orphans`）：`biz_status_log` 没有外键，历史轮次可能留下
///    「单据已删、日志还在」的行。真孤儿等多久都还在 ⇒ 仍会红；收敛只吸收「别人删单据与
///    删日志之间」的毫秒级窗口，口径没有放宽。
///    ‼️ `PURCHASE_ORDER` 这条是卡 `t_8ec0a1af` 补的**断言覆盖缺口**：采购单状态日志过去
///    只删单据不删日志，每轮 stockin 攒 35 条孤儿（7 轮 245 条、线性无上界）而**没有任何
///    用例会因此变红** —— 断言只复查了 `STOCK_IN`。判据只能落在「整库」上：采购单行一旦
///    被删，孤儿日志的 `entity_id` 就再也追不到是哪个模块造的（没有供应商可反查），
///    所以这里用 `settle_orphans` 先收口再断言 0，而不是按模块命名空间过滤。
///
/// 本用例**不造任何数据**，`setup()` 刚清过一轮 ⇒ 单独跑也成立、并行跑也成立。
#[tokio::test]
pub(super) async fn zz_module_namespace_has_no_residue() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let stock_in_where = format!("supplier_id IN ({})", fixture_suppliers());
    let sql = format!("SELECT count(*) FROM biz_stock_in WHERE {stock_in_where}");
    let left: i64 = db()
        .scalar_with_params::<Option<i64>>(sql.as_str(), vec![])
        .await
        .expect("查夹具收货单")
        .unwrap_or(0);
    assert_eq!(
        left, 0,
        "模块命名空间内不许留夹具收货单（setup 已清过一轮）"
    );

    // 本模块命名空间内的孤儿实例（`biz_id` 指向已不存在的收货单）先收掉，再断言为 0
    purge_orphan_post_instances().await;
    let running: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_flow_instance \
              WHERE biz_type IN ('STOCK_IN_POST', 'STOCK_IN_CANCEL') \
                AND status = 'PENDING' AND deleted_at IS NULL",
            vec![],
        )
        .await
        .expect("查在跑实例")
        .unwrap_or(0);
    assert_eq!(
        running, 0,
        "本模块命名空间内不许剩 PENDING 的过账/作废审批实例（同库连跑第二遍的前提）"
    );

    let orphans = crate::business::erp::fixture_hygiene::settle_orphans(
        &["STOCK_IN", "PURCHASE_ORDER"],
        std::time::Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        orphans, 0,
        "STOCK_IN / PURCHASE_ORDER 状态日志不许留孤儿行（收敛等待 5s 后仍非 0）"
    );
}

/// 收掉本模块命名空间内的**孤儿审批实例**（`biz_id` 指向已不存在的收货单）。
///
/// 只在 [`zz_module_namespace_has_no_residue`] 里调用，不进每例 `setup()`：
/// * 判据先按**本模块私有的两种 `biz_type`** 收口（不是全库扫），再看 `biz_id` 是否已死；
/// * 这类行只可能来自本模块历史轮次（某轮在「删实例」与「删单据」之间 panic）；
/// * 顺序与 [`purge_post_approval_traces`] 一致：先解自引用 → 删子表 → 删本体。
async fn purge_orphan_post_instances() {
    let inst_where = "biz_type IN ('STOCK_IN_POST', 'STOCK_IN_CANCEL') \
                      AND biz_id IS NOT NULL AND biz_id NOT IN (SELECT id FROM biz_stock_in)";
    for text in [
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
    ] {
        sql(&text).await;
    }
}
