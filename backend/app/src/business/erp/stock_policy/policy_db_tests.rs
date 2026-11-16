// ============================================================================
// policy_db_tests.rs —— 水位设置（M2-13）的真库用例
//
// 卡：t_d997ada8（M2-13【M2·仓库】安全库存：字段（物料 × 仓库）最低/最高水位）
//
// # 覆盖什么（每条都对应卡面 / 迁移里的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `save_creates_then_updates_the_same_cell` | 「一格一行」：同一物料同一仓库再保存 = 改，不是建第二行；id 不变 |
// | `page_joins_names_and_filters` | 列表带物料 / 仓库 / 单位名（LEFT JOIN，一页不 N+1）+ 物料/仓库/状态/关键字筛选 |
// | `unique_index_blocks_a_second_row_for_the_same_cell` | 并发兜底：绕过 handler 直插同键 → UniqueViolation（handler 翻成 409） |
// | `check_constraints_reject_bad_levels` | 库层兜底：min<0 / max<=min / status 域外 一律被 CHECK 拒 |
// | `soft_delete_frees_the_cell_and_falls_back_to_material` | 软删 = 该格回退物料级；同键可重建（部分唯一索引只看未删行） |
// | `effective_of_applies_policy_then_material_then_unset` | 三级回退（策略启用 > 物料级 > 不判）+ 停用回退 + min=0 阻断回退 |
// | `effective_of_is_empty_for_unknown_material` | 物料不存在 → 不判水位，不报错 |
// | `handler_rejects_bad_input_and_missing_rows` | 非法 UUID / 物料不存在 / 删不存在的行 → 400 / 404 人话 |
// | `batch_creates_all_cells_then_overwrites_and_counts` | 批量（M2-17）：2×2=4 格一次写；再批全覆盖；created/updated 分开计数；留空覆盖成 NULL |
// | `batch_rolls_back_everything_when_one_material_is_missing` | 批量原子性：任一物料 / 仓库不存在 → 404 + 整批回滚 |
// | `batch_dedupes_input_and_recreates_soft_deleted_cells` | 入参去重（同格两次=一格）；软删格重设算新建 |
// | `batch_rejects_over_limit_before_touching_the_db` | 铺开 > 2000 格 → 400，碰库之前拦住 |
// | `import_definition_upserts_by_cell_and_keeps_blank_max_as_null` | 导入定义（`business.erp.stock-policy`）：编码→id JOIN、max 空落 NULL、重导按格覆盖 |
// | `zz_teardown_purges_fixtures` | 清场：`M213-` 物料的水位行 0 条，`safety_stock` 复位 0 |
//
// # 为什么夹具直接用 `test_seed::ensure_*` + 直插设置行
//
// 设置表没有「过账」这类业务动作可走 —— 它的写入路径就是本卡的那三条接口，用例直接调
// `handler::save_policy_of`（接口那条路，不是测试自己拼的替身），只有想证明「库层兜底」
// （唯一索引 / CHECK）时才绕过 handler 直调 `model::policy_insert`。
// 主数据一律走 `crate::test_seed::ensure_*`（门禁：测试代码不许手写主数据 INSERT）。
//
// # 与其它 ERP 用例共用串行锁
//
// `db()` 是进程级全局池，M2-08/M2-09/M2-10 的真库用例用 `aging_db_tests::DB_LOCK` 串行；
// 本文件复用**同一个**锁（各锁各的等于没锁）。
//
// ⚠ 真库用例必须串行，跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_td997ada8_policy" cargo test --lib \
//   business::erp::stock_policy -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::stock_policy::dto::{
    self, BatchSavePolicyReq, MAX_BATCH_CELLS, MinSource, PolicyFilter, PolicyWrite, STATUS_DISABLED, STATUS_ENABLED,
    SavePolicyReq,
};
use crate::business::erp::stock_policy::{handler, model};
use crate::test_seed;

const WAREHOUSE_A_CODE: &str = "M213-WHA";
const WAREHOUSE_B_CODE: &str = "M213-WHB";
const MAT_A_CODE: &str = "M213-M1";
const MAT_B_CODE: &str = "M213-M2";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

async fn warehouse_a() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_A_CODE, "水位仓甲").await
}

async fn warehouse_b() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_B_CODE, "水位仓乙").await
}

async fn mat_a() -> Uuid {
    test_seed::ensure_material(MAT_A_CODE, "水位物料甲").await
}

async fn mat_b() -> Uuid {
    test_seed::ensure_material(MAT_B_CODE, "水位物料乙").await
}

/// 清场：只删本模块造的水位行（主数据由 `test_seed` 复用，不删），并把物料级安全库存复位。
async fn cleanup() {
    exec(
        &format!(
            "DELETE FROM biz_material_stock_policy WHERE material_id IN \
              (SELECT id FROM biz_material WHERE material_code IN ('{MAT_A_CODE}', '{MAT_B_CODE}'))"
        ),
        vec![],
    )
    .await;
    exec(
        &format!("UPDATE biz_material SET safety_stock = 0 WHERE material_code IN ('{MAT_A_CODE}', '{MAT_B_CODE}')"),
        vec![],
    )
    .await;
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    warehouse_a().await;
    warehouse_b().await;
    mat_a().await;
    mat_b().await;
    cleanup().await;
    Some(guard)
}

/// 物料级安全库存（`biz_material.safety_stock`，本卡不改它的行为，只让它当回退值）。
async fn set_material_safety(material_id: Uuid, value: &str) {
    exec(
        "UPDATE biz_material SET safety_stock = ?::text::numeric WHERE id = ?::uuid",
        vec![Value::Text(value.to_string()), Value::from(&material_id)],
    )
    .await;
}

/// 造一份设置入参（默认启用、无备注）。
fn req(material_id: Uuid, warehouse_id: Uuid, min: &str, max: Option<&str>) -> SavePolicyReq {
    SavePolicyReq {
        material_id: material_id.to_string(),
        warehouse_id: warehouse_id.to_string(),
        min_qty: dec(min),
        max_qty: max.map(dec),
        status: None,
        remark: None,
    }
}

fn filter_page() -> PolicyFilter {
    PolicyFilter {
        page: 1,
        page_size: 50,
        ..PolicyFilter::default()
    }
}

/// 一格一行：第二次保存是**改**（id 不变、行数不变），不是建第二行。
#[tokio::test]
async fn save_creates_then_updates_the_same_cell() {
    let Some(_guard) = setup().await else { return };
    let (mat, wh) = (mat_a().await, warehouse_a().await);

    // ① 建
    let created = handler::save_policy_of(None, req(mat, wh, "100", Some("800")))
        .await
        .expect("首次保存 = 建");
    assert_eq!(created.min_qty, dec("100"));
    assert_eq!(created.max_qty, Some(dec("800")));
    assert_eq!(created.status, STATUS_ENABLED, "不传 status = 启用");
    assert_eq!(
        created.material_code.as_deref(),
        Some(MAT_A_CODE),
        "出参带物料编码（LEFT JOIN）"
    );
    assert_eq!(created.warehouse_code.as_deref(), Some(WAREHOUSE_A_CODE));

    // ② 同一格再保存 = 改（含清除最高水位、停用、写备注）
    let mut again = req(mat, wh, "120", None);
    again.status = Some(STATUS_DISABLED);
    again.remark = Some("  备件仓暂不检查  ".to_string());
    let updated = handler::save_policy_of(None, again).await.expect("第二次 = 改");
    assert_eq!(updated.id, created.id, "upsert 命中同一行：id 不变");
    assert_eq!(updated.min_qty, dec("120"));
    assert_eq!(updated.max_qty, None, "最高水位传 null = 清除（不判超储）");
    assert_eq!(updated.status, STATUS_DISABLED);
    assert_eq!(updated.remark.as_deref(), Some("备件仓暂不检查"), "备注两端空白被去掉");

    // ③ 行数仍是 1（同一格不会长出第二行）
    let (rows, total) = model::policy_page(Some(mat), Some(wh), None, None, 1, 50)
        .await
        .expect("分页");
    assert_eq!(total, 1, "一格一行");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].id.to_string(),
        created.id,
        "同一格只有这一行（upsert 没有建第二行）"
    );

    // ④ 同一物料换一个仓库 = 另一格
    let other = handler::save_policy_of(None, req(mat, warehouse_b().await, "50", None))
        .await
        .expect("另一格");
    assert_ne!(other.id, created.id);
    let (_, total) = model::policy_page(Some(mat), None, None, None, 1, 50)
        .await
        .expect("分页");
    assert_eq!(total, 2, "同物料的两个仓库 = 两格");
    println!("[M2-13] upsert：首次建 / 再次改（id 不变、行数不变）/ 换仓 = 另一格 ✓");
}

/// 列表：带名字（LEFT JOIN）+ 物料 / 仓库 / 状态 / 关键字四种筛选都真的生效。
#[tokio::test]
async fn page_joins_names_and_filters() {
    let Some(_guard) = setup().await else { return };
    let (mat_a, mat_b, wh_a, wh_b) = (mat_a().await, mat_b().await, warehouse_a().await, warehouse_b().await);

    let row_aa = handler::save_policy_of(None, req(mat_a, wh_a, "100", Some("900")))
        .await
        .expect("甲甲");
    let mut disabled = req(mat_a, wh_b, "20", None);
    disabled.status = Some(STATUS_DISABLED);
    handler::save_policy_of(None, disabled).await.expect("甲乙");
    handler::save_policy_of(None, req(mat_b, wh_a, "5", None))
        .await
        .expect("乙甲");

    // 名字齐全（物料编码 / 名称 / 单位名 / 仓库编码 / 名称）
    let (rows, total) = model::policy_page(Some(mat_a), Some(wh_a), None, None, 1, 50)
        .await
        .expect("分页");
    assert_eq!(total, 1);
    let row = &rows[0];
    assert_eq!(row.id.to_string(), row_aa.id, "筛选命中的就是那一行");
    assert_eq!(row.material_code.as_deref(), Some(MAT_A_CODE));
    assert_eq!(row.material_name.as_deref(), Some("水位物料甲"));
    assert!(row.unit_name.is_some(), "单位名来自 biz_unit LEFT JOIN");
    assert_eq!(row.warehouse_name.as_deref(), Some("水位仓甲"));

    // 物料筛选
    let (_, total) = model::policy_page(Some(mat_a), None, None, None, 1, 50)
        .await
        .expect("按物料");
    assert_eq!(total, 2);
    // 仓库筛选
    let (_, total) = model::policy_page(None, Some(wh_b), None, None, 1, 50)
        .await
        .expect("按仓库");
    assert_eq!(total, 1);
    // 状态筛选
    let (rows, total) = model::policy_page(None, None, Some(STATUS_DISABLED), None, 1, 50)
        .await
        .expect("按状态");
    assert_eq!(total, 1, "只有甲乙那格是停用");
    assert_eq!(rows[0].min_qty, dec("20"));
    // 关键字（命中物料编码）
    let (_, total) = model::policy_page(None, None, None, Some("M213-M1"), 1, 50)
        .await
        .expect("关键字");
    assert_eq!(total, 2, "M213-M1 在两个仓各一格");
    // 关键字（命中仓库名）
    let (_, total) = model::policy_page(None, None, None, Some("水位仓乙"), 1, 50)
        .await
        .expect("关键字仓库");
    assert_eq!(total, 1);

    // handler 层：非法状态 / 非法 UUID 是 400（不静默当没传）
    let mut bad = filter_page();
    bad.status = Some("9".to_string());
    let error = handler::page_policy_of(bad).await.expect_err("非法状态必须 400");
    assert_eq!(
        (error.status, error.code),
        (400, cloudpivot_kernel::shared::error_code::PARAM_INVALID)
    );
    let mut bad = filter_page();
    bad.material_id = Some("not-a-uuid".to_string());
    let error = handler::page_policy_of(bad).await.expect_err("非法 UUID 必须 400");
    assert!(error.msg.contains("material_id"), "{}", error.msg);

    // 空白筛选 = 不筛（不是 `= ''` 把结果清空）
    let mut blank = filter_page();
    blank.material_id = Some("   ".to_string());
    blank.keyword = Some("  ".to_string());
    let page = handler::page_policy_of(blank).await.expect("空白不报错");
    assert_eq!(page.total, 3, "空白参数等于全量（本用例造了 3 格）");
    println!("[M2-13] 列表：名字 LEFT JOIN 齐全 + 物料/仓库/状态/关键字四种筛选 + 空白不筛 ✓");
}

/// 并发兜底：绕过 handler 直插同一个 物料 × 仓库 → 库层的部分唯一索引必须拒。
#[tokio::test]
async fn unique_index_blocks_a_second_row_for_the_same_cell() {
    let Some(_guard) = setup().await else { return };
    let (mat, wh) = (mat_a().await, warehouse_a().await);

    let write = PolicyWrite {
        min_qty: dec("10"),
        max_qty: None,
        status: STATUS_ENABLED,
        remark: None,
    };
    model::policy_insert(Uuid::now_v7(), mat, wh, &write, None)
        .await
        .expect("第一行应当插入成功");

    // handler 视角：taken 已经看得见（所以正常路径不会走到撞索引）
    assert!(
        model::policy_taken(mat, wh, None).await.expect("查占用"),
        "同一格已存在 → taken = true（handler 据此回 409 人话）"
    );

    // 绕过 handler 再插一次（模拟并发两路都判定「不存在」）
    let error = model::policy_insert(Uuid::now_v7(), mat, wh, &write, None)
        .await
        .expect_err("部分唯一索引必须拒第二行");
    assert!(
        matches!(error, DbError::UniqueViolation { .. }),
        "应为 UniqueViolation（handler 翻成 409），实得 {error:?}"
    );

    // 软删行不占键（索引条件是 deleted_at IS NULL）——由下一条用例钉住
    println!("[M2-13] 同键第二行被 uk_biz_msp_mat_wh 拒（唯一索引 = 并发 upsert 的兜底）✓");
}

/// 库层兜底：三条 CHECK 真的会拒（handler 校验被绕过时最后的闸）。
#[tokio::test]
async fn check_constraints_reject_bad_levels() {
    let Some(_guard) = setup().await else { return };
    let (mat, wh) = (mat_a().await, warehouse_a().await);

    // min < 0
    let negative = PolicyWrite {
        min_qty: dec("-1"),
        max_qty: None,
        status: STATUS_ENABLED,
        remark: None,
    };
    let error = model::policy_insert(Uuid::now_v7(), mat, wh, &negative, None)
        .await
        .expect_err("min_qty < 0 必须被 CHECK 拒");
    assert!(matches!(error, DbError::CheckViolation { .. }), "实得 {error:?}");

    // max <= min（相等 = 空区间）
    let flat = PolicyWrite {
        min_qty: dec("100"),
        max_qty: Some(dec("100")),
        status: STATUS_ENABLED,
        remark: None,
    };
    let error = model::policy_insert(Uuid::now_v7(), mat, wh, &flat, None)
        .await
        .expect_err("max_qty = min_qty 必须被 CHECK 拒");
    assert!(matches!(error, DbError::CheckViolation { .. }), "实得 {error:?}");

    // status 域外
    let bad_status = PolicyWrite {
        min_qty: dec("1"),
        max_qty: None,
        status: 7,
        remark: None,
    };
    let error = model::policy_insert(Uuid::now_v7(), mat, wh, &bad_status, None)
        .await
        .expect_err("status = 7 必须被 CHECK 拒");
    assert!(matches!(error, DbError::CheckViolation { .. }), "实得 {error:?}");

    // 一列都没写进去
    let (_, total) = model::policy_page(Some(mat), Some(wh), None, None, 1, 50)
        .await
        .expect("分页");
    assert_eq!(total, 0, "被拒的插入不得留残行");
    println!("[M2-13] CHECK 兜底：min<0 / max<=min / status=7 全部被库层拒（零残留）✓");
}

/// 软删 = 回退物料级；同键可重建（部分唯一索引只看未删行）。
#[tokio::test]
async fn soft_delete_frees_the_cell_and_falls_back_to_material() {
    let Some(_guard) = setup().await else { return };
    let (mat, wh) = (mat_a().await, warehouse_a().await);
    set_material_safety(mat, "50").await;

    let created = handler::save_policy_of(None, req(mat, wh, "120", Some("1000")))
        .await
        .expect("建");
    let before = model::effective_of(mat, wh).await.expect("生效水位");
    assert_eq!(before.min.qty, Some(dec("120")), "有策略 → 用策略");
    assert_eq!(before.min.source, MinSource::Policy);

    handler::delete_policy_of(None, &created.id.to_string())
        .await
        .expect("软删");
    let (_, total) = model::policy_page(Some(mat), Some(wh), None, None, 1, 50)
        .await
        .expect("分页");
    assert_eq!(total, 0, "软删行不出现在列表");

    let after = model::effective_of(mat, wh).await.expect("生效水位");
    assert_eq!(after.min.qty, Some(dec("50")), "删掉设置 → 回退物料级");
    assert_eq!(after.min.source, MinSource::Material);
    assert_eq!(after.max, None, "最高水位没有物料级回退 → 不判超储");

    // 二次删除 → 404（不静默成功）
    let error = handler::delete_policy_of(None, &created.id.to_string())
        .await
        .expect_err("重复删必须 404");
    assert_eq!(error.status, 404);

    // 同键可重建（新 id，行数仍 1）
    let rebuilt = handler::save_policy_of(None, req(mat, wh, "80", None))
        .await
        .expect("软删后同键可重建");
    assert_ne!(rebuilt.id, created.id, "重建是新行（历史行保留供审计）");
    let (_, total) = model::policy_page(Some(mat), Some(wh), None, None, 1, 50)
        .await
        .expect("分页");
    assert_eq!(total, 1);
    println!("[M2-13] 软删：列表消失 / 回退物料级 / 重复删 404 / 同键可重建 ✓");
}

/// 三级回退：策略启用 > 物料级 > 不判；停用回退物料级；min=0 阻断回退。
#[tokio::test]
async fn effective_of_applies_policy_then_material_then_unset() {
    let Some(_guard) = setup().await else { return };
    let (mat, wh) = (mat_a().await, warehouse_a().await);

    // ① 没有策略行、物料级也没设 → 不判
    let unset = model::effective_of(mat, wh).await.expect("生效水位");
    assert_eq!(unset.min.qty, None);
    assert_eq!(unset.min.source, MinSource::Unset);
    assert_eq!(unset.max, None);
    assert!(unset.policy.is_none());

    // ② 物料级 50，无策略行 → MATERIAL 50
    set_material_safety(mat, "50").await;
    let material = model::effective_of(mat, wh).await.expect("生效水位");
    assert_eq!(material.min.qty, Some(dec("50")));
    assert_eq!(material.min.source, MinSource::Material);
    assert_eq!(material.min.source.as_str(), "MATERIAL");

    // ③ 策略启用 120/1000 → POLICY（覆盖物料级）
    handler::save_policy_of(None, req(mat, wh, "120", Some("1000")))
        .await
        .expect("建");
    let policy = model::effective_of(mat, wh).await.expect("生效水位");
    assert_eq!(policy.min.qty, Some(dec("120")));
    assert_eq!(policy.min.source, MinSource::Policy);
    assert_eq!(policy.max, Some(dec("1000")));
    assert_eq!(policy.policy.map(|levels| levels.enabled), Some(true));

    // ④ 停用 → 回退物料级（不是「不判」），最高水位同时失效
    let mut disabled = req(mat, wh, "120", Some("1000"));
    disabled.status = Some(STATUS_DISABLED);
    handler::save_policy_of(None, disabled).await.expect("停用");
    let stopped = model::effective_of(mat, wh).await.expect("生效水位");
    assert_eq!(stopped.min.qty, Some(dec("50")), "停用 = 等同没设置，退回物料级");
    assert_eq!(stopped.min.source, MinSource::Material);
    assert_eq!(stopped.max, None, "停用的最高水位不生效");
    assert_eq!(
        stopped.policy.map(|levels| levels.enabled),
        Some(false),
        "回显里看得出来「设过但停用」"
    );

    // ⑤ 启用但 min=0 → 明示不预警（阻断回退），source 仍是 POLICY
    handler::save_policy_of(None, req(mat, wh, "0", Some("800")))
        .await
        .expect("保存 0");
    let zero = model::effective_of(mat, wh).await.expect("生效水位");
    assert_eq!(zero.min.qty, None, "策略行的 0 阻断回退物料级 50");
    assert_eq!(zero.min.source, MinSource::Policy);
    assert_eq!(zero.max, Some(dec("800")), "最高水位与最低水位独立");
    println!("[M2-13] 生效水位：策略启用 > 物料级 > 不判；停用回退；min=0 阻断回退 ✓");
}

/// 物料不存在 → 不判水位、不报错（历史设置行 / 已删物料的下游调用不能炸）。
#[tokio::test]
async fn effective_of_is_empty_for_unknown_material() {
    let Some(_guard) = setup().await else { return };
    let ghost = Uuid::now_v7();
    let levels = model::effective_of(ghost, warehouse_a().await).await.expect("不报错");
    assert_eq!(levels.min.qty, None);
    assert_eq!(levels.min.source, MinSource::Unset);
    assert!(levels.policy.is_none());
    println!("[M2-13] 物料不存在 → 不判水位且不报错（下游按「不检查」处理）✓");
}

/// 写接口的入口校验：非法 UUID / 物料不存在 / 仓库不存在 / 删不存在的行。
#[tokio::test]
async fn handler_rejects_bad_input_and_missing_rows() {
    let Some(_guard) = setup().await else { return };
    let (mat, wh) = (mat_a().await, warehouse_a().await);

    // 非法 UUID
    let mut bad = req(mat, wh, "10", None);
    bad.warehouse_id = "not-a-uuid".to_string();
    let error = handler::save_policy_of(None, bad)
        .await
        .expect_err("非法 UUID 必须 400");
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("warehouse_id"), "{}", error.msg);

    // 契约校验（负数 / 最高 ≤ 最低）→ 400 且点名字段
    let error = handler::save_policy_of(None, req(mat, wh, "-1", None))
        .await
        .expect_err("负数必须 400");
    assert!(error.msg.contains("min_qty"), "{}", error.msg);
    let error = handler::save_policy_of(None, req(mat, wh, "100", Some("100")))
        .await
        .expect_err("最高 = 最低必须 400");
    assert!(error.msg.contains("max_qty"), "{}", error.msg);

    // 物料 / 仓库不存在 → 404（人话点名是哪一边）
    let ghost = Uuid::now_v7();
    let error = handler::save_policy_of(None, req(ghost, wh, "10", None))
        .await
        .expect_err("物料不存在必须 404");
    assert_eq!(error.status, 404);
    assert!(error.msg.contains("物料"), "{}", error.msg);
    let error = handler::save_policy_of(None, req(mat, ghost, "10", None))
        .await
        .expect_err("仓库不存在必须 404");
    assert!(error.msg.contains("仓库"), "{}", error.msg);

    // 删不存在的行 → 404
    let error = handler::delete_policy_of(None, &Uuid::now_v7().to_string())
        .await
        .expect_err("不存在的行必须 404");
    assert_eq!(error.status, 404);

    // 全被拒 → 一行都没落库
    let (_, total) = model::policy_page(Some(mat), None, None, None, 1, 50)
        .await
        .expect("分页");
    assert_eq!(total, 0);
    println!("[M2-13] 入口校验：非法 UUID / 负数 / 最高≤最低 / 物料或仓库不存在 / 删空 → 400·404 ✓");
}

// ==================== 批量设置（M2-17） ====================

/// 批量设置：2 物料 × 2 仓库 = 4 格一次写；第二次同批 = 全部覆盖（created/updated 分开计数）。
#[tokio::test]
async fn batch_creates_all_cells_then_overwrites_and_counts() {
    let Some(_guard) = setup().await else { return };
    let (m1, m2, w1, w2) = (mat_a().await, mat_b().await, warehouse_a().await, warehouse_b().await);
    cleanup().await;

    let req = BatchSavePolicyReq {
        material_ids: vec![m1.to_string(), m2.to_string()],
        warehouse_ids: vec![w1.to_string(), w2.to_string()],
        min_qty: dec("100"),
        max_qty: Some(dec("500")),
        status: None,
        remark: Some("  M2-17 批量  ".to_string()),
    };
    let res = handler::save_policy_batch_of(None, req.clone())
        .await
        .expect("批量设置");
    assert_eq!(res.cells, 4, "2 物料 × 2 仓库 = 4 格");
    assert_eq!(res.created, 4, "第一次全部是新建");
    assert_eq!(res.updated, 0);
    assert!(res.message.contains("4 格"), "{}", res.message);

    // 四格都在，且四列都按入参落库（备注两端空白被去掉）
    for (material_id, warehouse_id) in [(m1, w1), (m1, w2), (m2, w1), (m2, w2)] {
        let row = model::policy_find_by_key(material_id, warehouse_id)
            .await
            .expect("查格")
            .unwrap_or_else(|| panic!("{material_id}/{warehouse_id} 应已建行"));
        assert_eq!(row.min_qty, dec("100"));
        assert_eq!(row.max_qty, Some(dec("500")));
        assert_eq!(row.status, STATUS_ENABLED);
        assert_eq!(row.remark.as_deref(), Some("M2-17 批量"));
    }

    // 再发一次（改最低水位、去掉最高水位）→ 全部覆盖，不新建第二行
    let again = BatchSavePolicyReq {
        min_qty: dec("250"),
        max_qty: None,
        remark: None,
        ..req
    };
    let res = handler::save_policy_batch_of(None, again).await.expect("再批量设置");
    assert_eq!(res.cells, 4);
    assert_eq!(res.created, 0, "第二次一格都不新建");
    assert_eq!(res.updated, 4, "第二次四格全是覆盖");

    let (_, total) = model::policy_page(Some(m1), None, None, None, 1, 50)
        .await
        .expect("分页");
    assert_eq!(total, 2, "同一物料只在这两个仓库各一行（没有重复行）");
    let row = model::policy_find_by_key(m1, w1).await.expect("查格").expect("应存在");
    assert_eq!(row.min_qty, dec("250"));
    assert_eq!(row.max_qty, None, "最高水位留空 = 覆盖成 NULL（不是保留旧值）");
    assert_eq!(row.remark, None, "备注留空 = 覆盖成 NULL");

    cleanup().await;
    println!("[M2-17] 批量设置：4 格新建 → 再批 4 格覆盖；created/updated 分开计数；留空覆盖成 NULL ✓");
}

/// 批量设置是**一个事务**：物料不存在时整批回滚，其余格一行都不落。
#[tokio::test]
async fn batch_rolls_back_everything_when_one_material_is_missing() {
    let Some(_guard) = setup().await else { return };
    let (m1, w1, w2) = (mat_a().await, warehouse_a().await, warehouse_b().await);
    cleanup().await;

    let ghost = Uuid::now_v7();
    let req = BatchSavePolicyReq {
        material_ids: vec![m1.to_string(), ghost.to_string()],
        warehouse_ids: vec![w1.to_string(), w2.to_string()],
        min_qty: dec("10"),
        max_qty: None,
        status: None,
        remark: None,
    };
    let error = handler::save_policy_batch_of(None, req)
        .await
        .expect_err("物料不存在必须 404");
    assert_eq!(error.status, 404);
    assert!(error.msg.contains("物料"), "{}", error.msg);
    assert!(error.msg.contains("整批未写入"), "{}", error.msg);

    let (_, total) = model::policy_page(Some(m1), None, None, None, 1, 50)
        .await
        .expect("分页");
    assert_eq!(total, 0, "整批回滚：合法物料那一格也不能留下");

    // 仓库不存在同理
    let req = BatchSavePolicyReq {
        material_ids: vec![m1.to_string()],
        warehouse_ids: vec![w1.to_string(), Uuid::now_v7().to_string()],
        min_qty: dec("10"),
        max_qty: None,
        status: None,
        remark: None,
    };
    let error = handler::save_policy_batch_of(None, req)
        .await
        .expect_err("仓库不存在必须 404");
    assert_eq!(error.status, 404);
    assert!(error.msg.contains("仓库"), "{}", error.msg);
    let (_, total) = model::policy_page(Some(m1), None, None, None, 1, 50)
        .await
        .expect("分页");
    assert_eq!(total, 0, "仓库不存在时同样整批回滚");

    cleanup().await;
    println!("[M2-17] 批量原子性：任一物料 / 仓库不存在 → 404 + 整批回滚（一格都不留）✓");
}

/// 入参去重与「软删格可重建」两个易混点：同一 id 传两次只落一格；软删过的格重新建行。
#[tokio::test]
async fn batch_dedupes_input_and_recreates_soft_deleted_cells() {
    let Some(_guard) = setup().await else { return };
    let (m1, w1) = (mat_a().await, warehouse_a().await);
    cleanup().await;

    let req = BatchSavePolicyReq {
        material_ids: vec![m1.to_string(), m1.to_string()],
        warehouse_ids: vec![w1.to_string(), format!("  {}  ", w1)],
        min_qty: dec("30"),
        max_qty: None,
        status: Some(STATUS_DISABLED),
        remark: None,
    };
    let res = handler::save_policy_batch_of(None, req).await.expect("批量设置");
    assert_eq!(res.cells, 1, "同一格传两次仍是一格");
    assert_eq!(res.duplicate_materials, 1);
    assert_eq!(res.duplicate_warehouses, 1);
    assert_eq!(res.created, 1);
    let row = model::policy_find_by_key(m1, w1).await.expect("查格").expect("应存在");
    assert_eq!(row.status, STATUS_DISABLED, "status 传 0 = 停用");

    // 软删该格 → 再批量设置一次 = 新建（部分唯一索引只看存活行），历史行保留
    handler::delete_policy_of(None, &row.id.to_string())
        .await
        .expect("软删");
    let req = BatchSavePolicyReq {
        material_ids: vec![m1.to_string()],
        warehouse_ids: vec![w1.to_string()],
        min_qty: dec("40"),
        max_qty: None,
        status: None,
        remark: None,
    };
    let res = handler::save_policy_batch_of(None, req).await.expect("软删后重设");
    assert_eq!(res.created, 1, "软删过的格算新建（不是覆盖那条已删行）");
    assert_eq!(res.updated, 0);

    let alive: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM biz_material_stock_policy \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&m1), Value::from(&w1)],
        )
        .await
        .expect("计数");
    assert_eq!(alive, 1, "存活行恰好 1 条");

    cleanup().await;
    println!("[M2-17] 批量：入参去重（同格两次=一格）；软删后重设算新建、存活行仍 1 条 ✓");
}

/// 铺开格数上限在**碰库之前**就拦住（2 001 格 → 400），库里的格数不变。
#[tokio::test]
async fn batch_rejects_over_limit_before_touching_the_db() {
    let Some(_guard) = setup().await else { return };
    let (m1, w1) = (mat_a().await, warehouse_a().await);
    cleanup().await;

    let materials: Vec<String> = std::iter::once(m1.to_string())
        .chain((1..=MAX_BATCH_CELLS).map(|_| Uuid::now_v7().to_string()))
        .collect();
    let req = BatchSavePolicyReq {
        material_ids: materials,
        warehouse_ids: vec![w1.to_string()],
        min_qty: dec("10"),
        max_qty: None,
        status: None,
        remark: None,
    };
    let error = handler::save_policy_batch_of(None, req)
        .await
        .expect_err("超上限必须 400");
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("2000"), "{}", error.msg);

    let (_, total) = model::policy_page(Some(m1), None, None, None, 1, 50)
        .await
        .expect("分页");
    assert_eq!(total, 0, "超上限的请求不写任何格");

    cleanup().await;
    println!("[M2-17] 批量上限：2001 格 → 400（碰库之前拦住，一格不写）✓");
}

/// Excel 导入定义（M2-17）：按「物料编码 + 仓库编码」在库端解析成 id 并 upsert。
///
/// 这里直接调定义的 `persist`（框架在事务里调的就是它），覆盖三件框架不管的事：
/// 编码 → id 的 JOIN、`max_qty` 留空落 NULL、`ON CONFLICT (material_id, warehouse_id)` 覆盖。
#[tokio::test]
async fn import_definition_upserts_by_cell_and_keeps_blank_max_as_null() {
    use crate::business::impexp::builtin::erp_setting::StockPolicyImport;
    use crate::business::impexp::definition::{ImportCtx, ImportDefinition, RowValues};

    let Some(_guard) = setup().await else { return };
    let (m1, m2, w1, w2) = (mat_a().await, mat_b().await, warehouse_a().await, warehouse_b().await);
    cleanup().await;

    let definition = StockPolicyImport;
    let ctx = ImportCtx {
        task_id: Uuid::now_v7(),
        operator: None,
        mode: crate::business::impexp::definition::ImportMode::Strict,
    };
    let row = |row_no: u32, material_code: &str, warehouse_code: &str, min: &str, max: &str| {
        let mut values = RowValues {
            row_no,
            ..Default::default()
        };
        values.fields.insert("material_code", material_code.to_string());
        values.fields.insert("warehouse_code", warehouse_code.to_string());
        values.fields.insert("min_qty", min.to_string());
        values.fields.insert("max_qty", max.to_string());
        values.fields.insert("status", "1".to_string());
        values.fields.insert("remark", "导入".to_string());
        values
    };
    let rows = vec![
        row(2, MAT_A_CODE, WAREHOUSE_A_CODE, "50", ""),
        row(3, MAT_B_CODE, WAREHOUSE_B_CODE, "60", "600"),
    ];

    let tx = db().begin().await.expect("begin");
    let affected = definition.persist(&tx, &rows, &ctx).await.expect("导入入库");
    tx.commit().await.expect("commit");
    assert_eq!(affected, 2, "两行都落库");

    let first = model::policy_find_by_key(m1, w1)
        .await
        .expect("查格")
        .expect("导入应建行");
    assert_eq!(first.min_qty, dec("50"));
    assert_eq!(first.max_qty, None, "最高水位留空 = NULL（不是 0）");
    let second = model::policy_find_by_key(m2, w2).await.expect("查格").expect("应建行");
    assert_eq!(second.max_qty, Some(dec("600")));

    // 重导同一份文件（改数值）→ 覆盖同一格，不产生第二行
    let rows = vec![
        row(2, MAT_A_CODE, WAREHOUSE_A_CODE, "70", ""),
        row(3, MAT_B_CODE, WAREHOUSE_B_CODE, "80", "800"),
    ];
    let tx = db().begin().await.expect("begin");
    let affected = definition.persist(&tx, &rows, &ctx).await.expect("重导入库");
    tx.commit().await.expect("commit");
    assert_eq!(affected, 2, "UPSERT 命中两格");

    let again = model::policy_find_by_key(m1, w1).await.expect("查格").expect("应存在");
    assert_eq!(again.id, first.id, "同一格：id 不变（覆盖而不是新建）");
    assert_eq!(again.min_qty, dec("70"));
    let (_, total) = model::policy_page(Some(m1), None, None, None, 1, 50)
        .await
        .expect("分页");
    assert_eq!(total, 1, "重导后仍只有一行");

    cleanup().await;
    println!("[M2-17] 导入定义：编码→id JOIN 落库、max 留空落 NULL、重导按格覆盖（id 不变、行数不增）✓");
}

/// 常量口径：三状态域与 DDL 的 CHECK、契约里的缺省值对齐（改一处必改另一处）。
#[tokio::test]
async fn constants_align_with_ddl() {
    let Some(_guard) = setup().await else { return };
    // 库层 CHECK 允许的就是 1 / 0，契约白名单必须逐个相同
    for status in dto::STATUSES {
        assert!([0_i16, 1_i16].contains(&status), "契约状态域只能是 0/1");
    }
    // 默认启用：不传 status 落 1（与列 DEFAULT 1 一致）
    let write = req(Uuid::now_v7(), Uuid::now_v7(), "1", None).to_write().expect("合法");
    assert_eq!(write.status, STATUS_ENABLED, "status 缺省 = 1（与列 DEFAULT 1 一致）");
    println!("[M2-13] 状态域 0/1 与 ck_biz_msp_status 对齐；缺省启用 = 列 DEFAULT 1 ✓");
}

/// 清场：跑完后本模块夹具 0 残留（下次跑不受影响）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    let (mat_a, mat_b) = (mat_a().await, mat_b().await);
    let leftover: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM biz_material_stock_policy WHERE material_id IN (?::uuid, ?::uuid)",
            vec![Value::from(&mat_a), Value::from(&mat_b)],
        )
        .await
        .expect("计数");
    assert_eq!(leftover, 0, "M213- 物料不应残留水位设置");

    let safety: String = db()
        .scalar_with_params(
            "SELECT COALESCE(MAX(safety_stock), 0)::text FROM biz_material WHERE material_code IN (?::text, ?::text)",
            vec![Value::Text(MAT_A_CODE.to_string()), Value::Text(MAT_B_CODE.to_string())],
        )
        .await
        .expect("查物料级安全库存");
    assert_eq!(safety, "0.0000", "物料级 safety_stock 复位 0（本卡不改它的值）");
    println!("[M2-13] 清场：水位行 0 条、物料级 safety_stock 复位 0 ✓");
}
