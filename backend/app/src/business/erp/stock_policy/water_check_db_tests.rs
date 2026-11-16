// ============================================================================
// water_check_db_tests.rs —— 水位检查（M2-14）的真库用例
//
// 卡：t_df0d7d48（M2-14【M2·仓库】安全库存：水位检查（低于 → 生成待办））
//
// # 覆盖什么（每条都对应卡面 / 上游契约的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `check_raises_one_todo_for_a_low_cell` | 「低于 → 生成待办」：一格一条，挂 `doc_type=INVENTORY` / `doc_id=库存行`，负责人是仓管主管，标题/描述带齐事实 |
// | `second_run_is_idempotent` | 「每小时扫一次」不会重复建：第二轮 `skipped_open=1`、待办仍 1 条 |
// | `open_todo_unique_index_blocks_a_second_row` | 幂等的**库层**那一层：绕过代码直插同格第二条未完成待办 → UniqueViolation |
// | `policy_level_overrides_material_level` | 阈值只走 M2-13 的三级回退：策略启用覆盖物料级 / 策略 `min=0` 明示不预警 / 策略停用回退物料级 |
// | `recovered_stock_closes_the_todo` | 「回升 → 自动收口」：`CANCELLED` + `AUTO_RECOVERED` 前缀；再跌破**立刻**重新生成（不被间隔抑制） |
// | `manual_close_is_suppressed_within_realert_days` | 人工取消的待办享受重提醒间隔；窗口外可以再提醒 |
// | `dry_run_writes_nothing` | `dry_run` 只算不写：明细有、待办 0 条 |
// | `disabled_material_closes_the_todo` | 物料停用 → 未完成待办自动收口 |
// | `owner_source_falls_back_to_operator_then_none` | 角色下没人：手动触发兜底成触发人（`OPERATOR`）；后台任务无人可派（`NONE` + 跳过） |
// | `scope_filter_narrows_the_scan` | 范围参数（物料 / 仓库）只动指定的格子 |
// | `zz_teardown_purges_fixtures` | 清场：`M214-` 物料的库存行 / 待办 / 设置行 0 条 |
//
// # 为什么这里直接插 `biz_inventory` 而不走过账
//
// 本卡是**读结存**的判定：要造的是「各种结存量 × 各种水位来源」的组合。走真实收货/出库
// 造不出「结存正好等于水位」「负库存」这类边界（过账路径有自己的校验与单据前置），
// 而这正是判定的关键。直插的风险由 V0137 的 CHECK（`locked_qty >= 0`）与唯一索引兜着；
// 主数据一律走 `crate::test_seed::ensure_*`（门禁：测试代码不许手写主数据 INSERT）。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `aging_db_tests::DB_LOCK`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m214_drill" cargo test --lib \
//   business::erp::stock_policy -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::stock_policy::dto::{STATUS_DISABLED, STATUS_ENABLED};
use crate::business::erp::stock_policy::water_check::{self, AUTO_CLOSE_PREFIX, DOC_TYPE};
use crate::business::erp::stock_policy::{handler, model};
use crate::test_seed;

const WAREHOUSE_A_CODE: &str = "M214-WHA";
const MAT_A_CODE: &str = "M214-M1";
const MAT_B_CODE: &str = "M214-M2";

/// 仓管主管角色编码（与 `reminder_stock::WAREHOUSE_ROLES` 同值；所有者由它解析）。
const ROLE_CODE: &str = "WAREHOUSE_MANAGER";

/// 夹具账号用**极小**的固定 UUID：`resolve_owner` 取角色成员里 uuid 升序第一位，
/// 用极小值保证用例里选中它（种子库若已有仓管主管成员，也不会抢走）。
fn manager_uuid() -> Uuid {
    Uuid::from_u128(0x2140)
}

/// 一个「谁都不许被当成操作人」的对照账号（手动触发的兜底人）。
fn operator_uuid() -> Uuid {
    Uuid::from_u128(0x2141)
}

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

async fn warehouse_a() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_A_CODE, "水位检查仓").await
}

async fn mat_a() -> Uuid {
    test_seed::ensure_material(MAT_A_CODE, "水位检查物料甲").await
}

async fn mat_b() -> Uuid {
    test_seed::ensure_material(MAT_B_CODE, "水位检查物料乙").await
}

/// 仓管主管角色（已存在则复用：种子库里有；空库首启时也可能还没有）。
async fn role_manager() -> Uuid {
    exec(
        "INSERT INTO sys_role (id, role_code, role_name, status, sort_order) \
         VALUES (?::uuid, ?, '仓管主管（M2-14 夹具）', 1, 50) \
         ON CONFLICT (role_code) WHERE deleted_at IS NULL DO NOTHING",
        vec![
            Value::from(&Uuid::from_u128(0x2142)),
            Value::Text(ROLE_CODE.to_string()),
        ],
    )
    .await;
    let rows = db()
        .find_with_params(
            "SELECT id FROM sys_role WHERE role_code = ? AND deleted_at IS NULL",
            vec![Value::Text(ROLE_CODE.to_string())],
        )
        .await
        .expect("查角色");
    rows.first()
        .expect("仓管主管角色必须存在")
        .get_typed("id")
        .expect("角色 id")
}

/// 夹具账号（仓管主管成员）+ 对照组账号（不属于任何角色）。
async fn ensure_users() {
    for (id, name) in [
        (manager_uuid(), "m214-warehouse-manager"),
        (operator_uuid(), "m214-operator"),
    ] {
        exec(
            "INSERT INTO sys_user (id, username, password, real_name, status, must_change_password) \
             VALUES (?::uuid, ?, 'x', ?, 1, false) \
             ON CONFLICT (id) DO UPDATE SET real_name = EXCLUDED.real_name, status = 1, deleted_at = NULL",
            vec![
                Value::from(&id),
                Value::Text(name.to_string()),
                Value::Text(name.to_string()),
            ],
        )
        .await;
    }
    exec(
        "INSERT INTO sys_user_role (id, user_id, role_id) VALUES (gen_random_uuid(), ?::uuid, ?::uuid) \
         ON CONFLICT (user_id, role_id) DO NOTHING",
        vec![Value::from(&manager_uuid()), Value::from(&role_manager().await)],
    )
    .await;
}

/// 结存（`biz_inventory`，物料 × 仓库 唯一行）。
///
/// **先 UPDATE，没命中再 INSERT**（不删不重建：库存行 id 是水位待办的 `doc_id`，
/// 删了重建会让「回升 → 收口」看不到原待办）。为什么不用 `ON CONFLICT`：
/// 该表的唯一键后来演进成 `(material_id, warehouse_id, COALESCE(location_id, 0-uuid))`
/// （引入库位后），列清单式的 `ON CONFLICT (material_id, warehouse_id)` 匹配不到那个表达式索引。
async fn set_qty(material_id: Uuid, warehouse_id: Uuid, qty: &str) -> Uuid {
    let affected = db()
        .execute_with_params(
            "UPDATE biz_inventory SET qty = ?::text::numeric, updated_at = now() \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![
                Value::Text(qty.to_string()),
                Value::from(&material_id),
                Value::from(&warehouse_id),
            ],
        )
        .await
        .unwrap_or_else(|error| panic!("更新结存失败：{error}"));
    if affected == 0 {
        exec(
            "INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, created_at, updated_at) \
             VALUES (gen_random_uuid(), ?::uuid, ?::uuid, ?::text::numeric, now(), now())",
            vec![
                Value::from(&material_id),
                Value::from(&warehouse_id),
                Value::Text(qty.to_string()),
            ],
        )
        .await;
    }
    inventory_id(material_id, warehouse_id).await
}

async fn inventory_id(material_id: Uuid, warehouse_id: Uuid) -> Uuid {
    let rows = db()
        .find_with_params(
            "SELECT id FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![Value::from(&material_id), Value::from(&warehouse_id)],
        )
        .await
        .expect("查库存行");
    rows.first()
        .expect("库存行必须存在")
        .get_typed("id")
        .expect("库存行 id")
}

/// 物料级安全库存（M2-13 的回退值）。
async fn set_material_safety(material_id: Uuid, value: &str) {
    exec(
        "UPDATE biz_material SET safety_stock = ?::text::numeric WHERE id = ?::uuid",
        vec![Value::Text(value.to_string()), Value::from(&material_id)],
    )
    .await;
}

/// 一格的策略行（直插：`uk_biz_msp_mat_wh` 只看未删行，先删后插即「重置」）。
async fn set_policy(material_id: Uuid, warehouse_id: Uuid, min: &str, status: i16) {
    exec(
        "DELETE FROM biz_material_stock_policy WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&material_id), Value::from(&warehouse_id)],
    )
    .await;
    exec(
        "INSERT INTO biz_material_stock_policy (material_id, warehouse_id, min_qty, status) \
         VALUES (?::uuid, ?::uuid, ?::text::numeric, ?::text::smallint)",
        vec![
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::Text(min.to_string()),
            Value::Text(status.to_string()),
        ],
    )
    .await;
}

async fn clear_policy(material_id: Uuid, warehouse_id: Uuid) {
    exec(
        "DELETE FROM biz_material_stock_policy WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&material_id), Value::from(&warehouse_id)],
    )
    .await;
}

/// 该格的水位待办（`doc_type = 'INVENTORY'`，含已关闭的行）。
async fn todos_of(inventory_id: Uuid) -> Vec<(Uuid, String, Option<String>, String, String)> {
    let rows = db()
        .find_with_params(
            "SELECT id, status, cancel_reason, priority, title FROM biz_task \
              WHERE doc_type = ? AND doc_id = ?::uuid AND deleted_at IS NULL ORDER BY created_at ASC",
            vec![Value::Text(DOC_TYPE.to_string()), Value::from(&inventory_id)],
        )
        .await
        .expect("查待办");
    rows.iter()
        .map(|row| {
            (
                row.get_typed::<Uuid>("id").expect("task id"),
                row.get_typed::<String>("status").expect("status"),
                row.get_typed::<Option<String>>("cancel_reason").ok().flatten(),
                row.get_typed::<String>("priority").expect("priority"),
                row.get_typed::<String>("title").expect("title"),
            )
        })
        .collect()
}

async fn open_todos_of(inventory_id: Uuid) -> usize {
    todos_of(inventory_id)
        .await
        .into_iter()
        .filter(|(_, status, ..)| status == "TODO" || status == "DOING")
        .count()
}

/// 清场：只删本模块造的行（物料 / 仓库 / 账号由 ensure_* 复用，不删）。
async fn cleanup() {
    let mats = format!("'{MAT_A_CODE}', '{MAT_B_CODE}'");
    for sql in [
        format!(
            "DELETE FROM biz_task WHERE doc_type = '{DOC_TYPE}' AND doc_id IN \
             (SELECT id FROM biz_inventory WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ({mats})))"
        ),
        format!(
            "DELETE FROM biz_inventory WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ({mats}))"
        ),
        format!(
            "DELETE FROM biz_material_stock_policy WHERE material_id IN \
             (SELECT id FROM biz_material WHERE material_code IN ({mats}))"
        ),
        format!("UPDATE biz_material SET safety_stock = 0 WHERE material_code IN ({mats})"),
    ] {
        exec(&sql, vec![]).await;
    }
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    ensure_users().await;
    warehouse_a().await;
    mat_a().await;
    mat_b().await;
    cleanup().await;
    Some(guard)
}

// ============================================================================
// 用例
// ============================================================================

/// ① 低于 → 生成一条待办；载体 / 负责人 / 文案 / 优先级全部对上。
#[tokio::test]
async fn check_raises_one_todo_for_a_low_cell() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let material = mat_a().await;
    set_material_safety(material, "0").await;
    set_policy(material, wh, "50", STATUS_ENABLED).await;
    let inv = set_qty(material, wh, "12").await;

    let report = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("跑水位检查");

    assert_eq!(report.scanned, 1, "只扫到这一格");
    assert_eq!(report.low, 1, "12 < 50 → 低于水位");
    assert_eq!(report.raised, 1, "应新建 1 条待办");
    assert_eq!(report.failed, 0);
    assert_eq!(report.items.len(), 1);
    let item = &report.items[0];
    assert_eq!(item.inventory_id, inv.to_string());
    assert_eq!(item.min_source, "POLICY", "阈值来自策略行");
    assert_eq!(item.min_qty, "50");
    assert_eq!(item.gap_qty, "38");
    assert_eq!(item.action, "RAISED");
    assert!(item.task_no.is_some(), "新建的待办要回显编号");
    assert_eq!(report.owner.as_deref(), Some(manager_uuid().to_string().as_str()));
    assert_eq!(report.owner_source, "ROLE", "负责人来自仓管主管角色");

    let todos = todos_of(inv).await;
    assert_eq!(todos.len(), 1, "一格一条");
    let (task_id, status, _reason, priority, title) = &todos[0];
    assert_eq!(status, "TODO");
    assert_eq!(priority, "P2", "有货但不足 → P2");
    assert!(title.contains(MAT_A_CODE), "标题带物料编码：{title}");
    assert!(title.contains("水位检查仓"), "标题带仓库名：{title}");

    // 载体三件：doc_type / doc_id / owner / 截止日 / 描述里的来源
    let rows = db()
        .find_with_params(
            "SELECT doc_type, doc_id, owner_id, creator_id, due_date, description, source \
               FROM biz_task WHERE id = ?::uuid",
            vec![Value::from(task_id)],
        )
        .await
        .expect("回读待办");
    let row = rows.first().expect("待办必须存在");
    assert_eq!(row.get_typed::<String>("doc_type").expect("doc_type"), "INVENTORY");
    assert_eq!(row.get_typed::<Uuid>("doc_id").expect("doc_id"), inv);
    assert_eq!(row.get_typed::<Uuid>("owner_id").expect("owner"), manager_uuid());
    assert_eq!(
        row.get_typed::<Uuid>("creator_id").expect("creator"),
        manager_uuid(),
        "系统生成的待办：creator = owner（不是分派）"
    );
    assert_eq!(row.get_typed::<String>("source").expect("source"), "SELF");
    assert!(
        row.get_typed::<Option<nik::prelude::chrono::NaiveDate>>("due_date")
            .expect("due")
            .is_some(),
        "系统待办按「本周五」给截止日（与手工建任务同口径）"
    );
    let description = row
        .get_typed::<Option<String>>("description")
        .expect("desc")
        .expect("描述非空");
    for fact in ["12", "50", "38", "策略行", "补货"] {
        assert!(description.contains(fact), "描述缺 `{fact}`：{description}");
    }
    println!("[M2-14] 低于水位 → 一格一条待办（doc_type=INVENTORY / owner=仓管主管 / P2）✓");
}

/// ② 连跑两次不重复建单：第二轮的计数落在 `skipped_open`。
#[tokio::test]
async fn second_run_is_idempotent() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let material = mat_a().await;
    set_policy(material, wh, "50", STATUS_ENABLED).await;
    let inv = set_qty(material, wh, "10").await;

    let first = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("第一轮");
    assert_eq!(first.raised, 1);
    let second = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("第二轮");
    assert_eq!(second.raised, 0, "第二轮不许再建");
    assert_eq!(second.skipped_open, 1, "已有未完成待办 → skipped_open");
    assert_eq!(second.items[0].action, "EXISTS");
    assert_eq!(open_todos_of(inv).await, 1, "库里仍只有一条未完成待办");
    println!("[M2-14] 重复跑幂等：第二轮 raised=0 / skipped_open=1 ✓");
}

/// ③ 库层那一层幂等：绕过代码直插同格第二条未完成待办必须被唯一索引拒。
#[tokio::test]
async fn open_todo_unique_index_blocks_a_second_row() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let material = mat_a().await;
    set_policy(material, wh, "50", STATUS_ENABLED).await;
    let inv = set_qty(material, wh, "10").await;

    water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("先建一条");
    let error = db()
        .execute_with_params(
            "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, doc_type, doc_id) \
             VALUES (gen_random_uuid(), 'TSK-214001-999999', '重复水位待办', ?::uuid, ?::uuid, 'INVENTORY', ?::uuid)",
            vec![
                Value::from(&manager_uuid()),
                Value::from(&manager_uuid()),
                Value::from(&inv),
            ],
        )
        .await
        .expect_err("同格第二条未完成待办必须被 uk_biz_task_open_inventory 拒");
    match error {
        DbError::UniqueViolation { constraint, .. } => {
            let name = constraint.unwrap_or_default();
            assert!(
                name.contains("uk_biz_task_open_inventory"),
                "冲突应来自水位待办唯一索引，实际 {name}"
            );
        }
        other => panic!("期望唯一冲突，实际 {other}"),
    }

    // 关掉那条之后同格可以再建（部分谓词的语义：唯一域只在「未完成」上）
    let todos = todos_of(inv).await;
    exec(
        "UPDATE biz_task SET status = 'CANCELLED', cancel_reason = '人工取消（用例）' WHERE id = ?::uuid",
        vec![Value::from(&todos[0].0)],
    )
    .await;
    db().execute_with_params(
        "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, doc_type, doc_id) \
             VALUES (gen_random_uuid(), 'TSK-214001-999998', '关闭后可再建', ?::uuid, ?::uuid, 'INVENTORY', ?::uuid)",
        vec![
            Value::from(&manager_uuid()),
            Value::from(&manager_uuid()),
            Value::from(&inv),
        ],
    )
    .await
    .expect("关闭后同格可以再建（历史行保留）");
    println!("[M2-14] uk_biz_task_open_inventory：未完成时拒重复 / 关闭后可再建 ✓");
}

/// ④ 阈值只走 M2-13 的三级回退（策略启用覆盖 / min=0 阻断 / 停用回退）。
#[tokio::test]
async fn policy_level_overrides_material_level() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let material = mat_a().await;

    // 物料级 100，策略启用 min=50，结存 60：按策略 → 不报（按物料级会报）
    set_material_safety(material, "100").await;
    set_policy(material, wh, "50", STATUS_ENABLED).await;
    set_qty(material, wh, "60").await;
    let report = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("策略优先");
    assert_eq!(report.low, 0, "60 >= 策略 50 → 不报（物料级 100 被覆盖）");
    assert_eq!(report.raised, 0);

    // 策略 min=0（明示不预警）：结存 1 也不报，且**不**回退物料级
    clear_policy(material, wh).await;
    set_policy(material, wh, "0", STATUS_ENABLED).await;
    set_qty(material, wh, "1").await;
    let report = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("min=0 阻断回退");
    assert_eq!(report.low, 0, "策略行 min=0 = 本格明示不预警");

    // 策略停用 → 回退物料级 100：结存 1 < 100 → 报
    set_policy(material, wh, "50", STATUS_DISABLED).await;
    let report = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("停用回退物料级");
    assert_eq!(report.low, 1, "停用策略 → 回退物料级 100");
    assert_eq!(report.items[0].min_source, "MATERIAL");
    assert_eq!(report.items[0].min_qty, "100");
    println!("[M2-14] 阈值三级回退（策略覆盖 / min=0 阻断 / 停用回退）在检查里逐条生效 ✓");
}

/// ⑤ 回升 → 自动收口；再跌破 → 立刻重新生成（不被重提醒间隔抑制）。
#[tokio::test]
async fn recovered_stock_closes_the_todo() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let material = mat_a().await;
    set_policy(material, wh, "50", STATUS_ENABLED).await;
    let inv = set_qty(material, wh, "10").await;

    let first = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("低于水位");
    assert_eq!(first.raised, 1);

    // 补货到 80（>= 50）
    set_qty(material, wh, "80").await;
    let second = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("回升");
    assert_eq!(second.low, 0);
    assert_eq!(second.closed, 1, "库存回升 → 自动关闭那条待办");
    let todos = todos_of(inv).await;
    assert_eq!(todos.len(), 1, "历史行保留（不是物理删）");
    assert_eq!(todos[0].1, "CANCELLED");
    let reason = todos[0].2.clone().expect("关闭原因必填");
    assert!(
        reason.starts_with(AUTO_CLOSE_PREFIX),
        "关闭原因必须带 AUTO_RECOVERED 前缀（重提醒抑制靠它区分）：{reason}"
    );

    // 又跌破：新事件 → 立刻重新生成（不被间隔抑制）
    set_qty(material, wh, "5").await;
    let third = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("再跌破");
    assert_eq!(third.raised, 1, "回升后又跌破 → 立刻重新提醒");
    assert_eq!(third.skipped_recent, 0);
    assert_eq!(todos_of(inv).await.len(), 2, "两条历史 + 一条新的未完成");
    assert_eq!(open_todos_of(inv).await, 1);
    println!("[M2-14] 回升收口（CANCELLED + AUTO_RECOVERED）→ 再跌破立刻重新生成 ✓");
}

/// ⑥ 人工取消的待办享受重提醒间隔；窗口外可以再提醒。
#[tokio::test]
async fn manual_close_is_suppressed_within_realert_days() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let material = mat_a().await;
    set_policy(material, wh, "50", STATUS_ENABLED).await;
    let inv = set_qty(material, wh, "10").await;

    water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("建单");
    let todos = todos_of(inv).await;
    // 人手动取消（不是回升）：条件还在，但人判断过
    exec(
        "UPDATE biz_task SET status = 'CANCELLED', cancel_reason = '手动取消：这批料已下紧急采购' \
          WHERE id = ?::uuid",
        vec![Value::from(&todos[0].0)],
    )
    .await;

    let suppressed = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("间隔内");
    assert_eq!(suppressed.raised, 0, "间隔内不再打扰");
    assert_eq!(suppressed.skipped_recent, 1);
    assert_eq!(suppressed.items[0].action, "RECENT");

    // 把上次提醒时间推到窗口外（10 天前）→ 可以再提醒
    exec(
        "UPDATE biz_task SET created_at = now() - interval '10 days' WHERE id = ?::uuid",
        vec![Value::from(&todos[0].0)],
    )
    .await;
    let after = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("窗口外");
    assert_eq!(after.raised, 1, "过了重提醒间隔再提醒一次");
    println!("[M2-14] 人工取消 → 间隔内抑制（skipped_recent）/ 窗口外再提醒 ✓");
}

/// ⑦ `dry_run` 只算不写。
#[tokio::test]
async fn dry_run_writes_nothing() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let material = mat_a().await;
    set_policy(material, wh, "50", STATUS_ENABLED).await;
    let inv = set_qty(material, wh, "10").await;

    let report = water_check::run_check(Some(material), Some(wh), true, None)
        .await
        .expect("预演");
    assert!(report.dry_run);
    assert_eq!(report.low, 1);
    assert_eq!(report.raised, 0, "dry_run 不建单");
    assert_eq!(report.items[0].action, "DRY_RUN");
    assert_eq!(report.items[0].gap_qty, "40");
    assert_eq!(todos_of(inv).await.len(), 0, "库里一条待办都没有");
    println!("[M2-14] dry_run：明细给结论 / 待办 0 条 ✓");
}

/// ⑧ 物料停用 → 未完成待办自动收口（不提醒没人处理的事）。
#[tokio::test]
async fn disabled_material_closes_the_todo() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let material = mat_a().await;
    set_policy(material, wh, "50", STATUS_ENABLED).await;
    let inv = set_qty(material, wh, "10").await;

    water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("建单");
    exec(
        "UPDATE biz_material SET status = 0 WHERE id = ?::uuid",
        vec![Value::from(&material)],
    )
    .await;

    let report = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("物料停用后");
    assert_eq!(report.closed, 1, "物料停用 → 收口");
    assert_eq!(report.low, 0, "停用物料不再参与判定");
    let todos = todos_of(inv).await;
    assert_eq!(todos[0].1, "CANCELLED");
    assert!(
        todos[0].2.clone().unwrap_or_default().contains("物料已停用"),
        "原因要点明是哪一条：{:?}",
        todos[0].2
    );

    exec(
        "UPDATE biz_material SET status = 1 WHERE id = ?::uuid",
        vec![Value::from(&material)],
    )
    .await;
    println!("[M2-14] 物料停用 → 未完成待办自动收口（原因点名）✓");
}

/// ⑨ 角色下没人：手动触发兜底成触发人；后台任务无人可派 → 跳过并记 WARN。
#[tokio::test]
async fn owner_source_falls_back_to_operator_then_none() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let material = mat_a().await;
    set_policy(material, wh, "50", STATUS_ENABLED).await;
    let inv = set_qty(material, wh, "10").await;

    // 临时把仓管主管成员全部摘掉（模拟「角色还没配人」），摘之前先记下原有成员，用完还原
    let role = role_manager().await;
    let prior: Vec<(Uuid, Uuid)> = db()
        .find_with_params(
            "SELECT user_id, role_id FROM sys_user_role WHERE role_id = ?::uuid",
            vec![Value::from(&role)],
        )
        .await
        .expect("记下原有成员")
        .iter()
        .map(|row| {
            (
                row.get_typed::<Uuid>("user_id").expect("user_id"),
                row.get_typed::<Uuid>("role_id").expect("role_id"),
            )
        })
        .collect();
    assert!(!prior.is_empty(), "夹具成员应在角色里");
    exec(
        "DELETE FROM sys_user_role WHERE role_id = ?::uuid",
        vec![Value::from(&role)],
    )
    .await;

    let background = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("后台任务");
    assert_eq!(background.owner_source, "NONE");
    assert_eq!(background.raised, 0);
    assert_eq!(background.skipped_no_owner, 1, "无人可派 → 跳过并计数");
    assert_eq!(background.items[0].action, "NO_OWNER");
    assert_eq!(todos_of(inv).await.len(), 0);

    let manual = water_check::run_check(Some(material), Some(wh), false, Some(operator_uuid()))
        .await
        .expect("手动触发");
    assert_eq!(manual.owner_source, "OPERATOR", "手动触发兜底成触发人");
    assert_eq!(manual.raised, 1);
    let todos = todos_of(inv).await;
    let rows = db()
        .find_with_params(
            "SELECT owner_id, create_by FROM biz_task WHERE id = ?::uuid",
            vec![Value::from(&todos[0].0)],
        )
        .await
        .expect("回读");
    let row = rows.first().expect("待办");
    assert_eq!(row.get_typed::<Uuid>("owner_id").expect("owner"), operator_uuid());
    assert_eq!(
        row.get_typed::<Option<Uuid>>("create_by")
            .expect("create_by")
            .expect("非空"),
        operator_uuid(),
        "create_by 记真实触发人"
    );

    // 还原角色成员（原样恢复，避免影响后续用例 / 其它模块的真库用例）
    for (user_id, role_id) in prior {
        exec(
            "INSERT INTO sys_user_role (id, user_id, role_id) VALUES (gen_random_uuid(), ?::uuid, ?::uuid) \
             ON CONFLICT (user_id, role_id) DO NOTHING",
            vec![Value::from(&user_id), Value::from(&role_id)],
        )
        .await;
    }
    ensure_users().await;
    println!("[M2-14] 无人可派：后台 NONE 跳过 / 手动 OPERATOR 兜底（create_by 记触发人）✓");
}

/// ⑩ 范围参数只动指定格子（另一格同样缺水却不受影响）。
#[tokio::test]
async fn scope_filter_narrows_the_scan() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let a = mat_a().await;
    let b = mat_b().await;
    set_policy(a, wh, "50", STATUS_ENABLED).await;
    set_policy(b, wh, "50", STATUS_ENABLED).await;
    let inv_a = set_qty(a, wh, "1").await;
    let inv_b = set_qty(b, wh, "1").await;

    let report = water_check::run_check(Some(a), Some(wh), false, None)
        .await
        .expect("只查物料甲");
    assert_eq!(report.scanned, 1, "范围参数生效（只扫一格）");
    assert_eq!(report.raised, 1);
    assert_eq!(open_todos_of(inv_a).await, 1);
    assert_eq!(open_todos_of(inv_b).await, 0, "另一格不动");

    let all = water_check::run_check(None, Some(wh), false, None)
        .await
        .expect("全仓扫");
    assert!(all.scanned >= 2, "不限定物料时两格都在范围里");
    assert_eq!(all.raised, 1, "乙格补建一条，甲格已有 → skipped_open");
    assert_eq!(open_todos_of(inv_b).await, 1);
    println!("[M2-14] 范围参数：物料甲 1 格 / 全仓扫补建乙格 ✓");
}

/// ⑪ 迁移自检：部分唯一索引在，且谓词只覆盖 INVENTORY + 未删 + 未完成。
#[tokio::test]
async fn migration_index_is_partial_and_unique() {
    let Some(_guard) = setup().await else { return };
    let rows = db()
        .query_sql(
            "SELECT indexdef FROM pg_indexes \
              WHERE schemaname = current_schema() AND indexname = 'uk_biz_task_open_inventory'"
                .to_string(),
            vec![],
        )
        .await
        .expect("查索引");
    let def = rows
        .first()
        .expect("V0429 必须建出 uk_biz_task_open_inventory")
        .get_typed::<String>("indexdef")
        .expect("indexdef");
    for needle in ["UNIQUE", "doc_type", "INVENTORY", "deleted_at IS NULL", "TODO", "DOING"] {
        assert!(def.contains(needle), "索引定义缺 `{needle}`：{def}");
    }
    println!("[M2-14] 迁移索引：部分唯一（INVENTORY + 未删 + TODO/DOING）✓");
}

/// ⑫ 清场自证：本模块的行都删干净（用例间不互相污染）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let material = mat_a().await;
    set_policy(material, wh, "50", STATUS_ENABLED).await;
    let inv = set_qty(material, wh, "10").await;
    water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("建单");
    assert_eq!(todos_of(inv).await.len(), 1);

    cleanup().await;
    let rows = db()
        .query_sql(
            "SELECT count(*)::bigint AS n FROM biz_inventory WHERE material_id IN \
             (SELECT id FROM biz_material WHERE material_code LIKE 'M214-%')"
                .to_string(),
            vec![],
        )
        .await
        .expect("统计");
    assert_eq!(rows[0].get_typed::<i64>("n").expect("n"), 0, "库存行已清");
    let rows = db()
        .query_sql(
            "SELECT count(*)::bigint AS n FROM biz_task WHERE doc_type = 'INVENTORY' AND doc_id IN \
             (SELECT id FROM biz_inventory WHERE material_id IN \
              (SELECT id FROM biz_material WHERE material_code LIKE 'M214-%'))"
                .to_string(),
            vec![],
        )
        .await
        .expect("统计待办");
    assert_eq!(rows[0].get_typed::<i64>("n").expect("n"), 0, "待办已清");
    println!("[M2-14] 清场：M214- 的库存行 / 水位待办 0 条 ✓");
}

/// ⑬ handler 入口与定时任务体同一个函数（接口层参数校验的挡板也在）。
#[tokio::test]
async fn handler_entry_shares_the_same_check_body() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let material = mat_a().await;
    set_policy(material, wh, "50", STATUS_ENABLED).await;
    let inv = set_qty(material, wh, "10").await;

    // 非法 UUID → 400 + 10001，不静默当没传
    let error = handler::check_water_of(
        Some(operator_uuid()),
        crate::business::erp::stock_policy::dto::WaterCheckReq {
            material_id: Some("not-a-uuid".to_string()),
            warehouse_id: None,
            dry_run: true,
        },
    )
    .await
    .expect_err("非法 material_id 必须 400");
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("material_id"), "{}", error.msg);

    // 正常路径（手动触发：负责人口径 = 角色优先）
    let res = handler::check_water_of(
        Some(operator_uuid()),
        crate::business::erp::stock_policy::dto::WaterCheckReq {
            material_id: Some(material.to_string()),
            warehouse_id: Some(wh.to_string()),
            dry_run: false,
        },
    )
    .await
    .expect("接口路径可跑");
    assert_eq!(res.raised, 1);
    assert!(res.message.contains("erp_stock_water_check"), "{}", res.message);
    assert_eq!(open_todos_of(inv).await, 1);

    // 定时任务体（后台入口）也跑得通，且不重复建单
    let summary = water_check::run_background().await.expect("后台任务体");
    assert!(summary.contains("scanned="), "{summary}");
    println!("[M2-14] handler 与后台任务共用同一检查体（400 挡板 + 幂等）✓");
}

/// ⑭ 设置行删除（回退物料级）：老待办随「不再判水位」收口。
#[tokio::test]
async fn deleting_policy_that_stops_judging_closes_the_todo() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let material = mat_a().await;
    set_material_safety(material, "0").await;
    set_policy(material, wh, "50", STATUS_ENABLED).await;
    let inv = set_qty(material, wh, "10").await;

    water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("建单");
    // 删掉策略行且物料级为 0 → 该格不再判水位 → 待办没有意义了
    clear_policy(material, wh).await;
    let report = water_check::run_check(Some(material), Some(wh), false, None)
        .await
        .expect("删策略后");
    assert_eq!(report.low, 0, "不再判水位");
    assert_eq!(report.closed, 1, "不再判水位 → 收口");
    let todos = todos_of(inv).await;
    assert_eq!(todos[0].1, "CANCELLED");
    println!("[M2-14] 删除水位设置（且物料级为 0）→ 待办收口 ✓");
}

/// ⑮ `effective_of`（M2-13 的一格读数）与检查用的判定结果一致 —— 两条读法不分叉。
#[tokio::test]
async fn effective_of_agrees_with_the_check() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let material = mat_a().await;
    set_material_safety(material, "70").await;
    set_policy(material, wh, "50", STATUS_ENABLED).await;
    set_qty(material, wh, "60").await;

    let levels = model::effective_of(material, wh).await.expect("读一格水位");
    assert_eq!(levels.min.qty, Some(dec("50")), "策略行优先");

    let report = water_check::run_check(Some(material), Some(wh), true, None)
        .await
        .expect("预演");
    assert_eq!(report.low, 0, "60 >= 50 与 effective_of 一致");
    println!("[M2-14] effective_of 与检查判定一致（同一份三级回退）✓");
}
