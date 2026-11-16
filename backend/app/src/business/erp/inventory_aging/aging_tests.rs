// ============================================================================
// aging_tests.rs —— 库龄计算的「契约面」纯逻辑用例（不连库）
//
// 卡：t_397dc77c（M2-08【M2·仓库】库龄：计算（按批次入库时间））
//
// # 这里钉什么
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `migration_declares_aging_function_and_permission` | V0424 里函数 / 权限码 / 菜单挂载点逐项在位 |
// | `fifo_formula_exists_only_in_the_database_function` | 「累计 + 夹 0」只写一份（库函数里），Rust 取数层不许自己再算 |
// | `down_migration_reverts_exactly_what_up_created` | down 只撤函数 + 权限点 + 授权，不碰业务数据 |
// | `function_constant_matches_ddl` | 代码里的函数名与 DDL 逐字一致（写错就是运行期 404 表） |
// | `permission_constant_matches_seed` | 代码里的权限码与种子逐字一致（写错就是 N14 红） |
//
// 为什么这些要写成用例而不是靠人看：库函数是 M2-09 / M2-10 的公共依赖，
// 「另写一份 FIFO」是这类卡最典型的退化方式，静态断言是唯一能天天跑的护栏。
// ============================================================================

use std::path::{Path, PathBuf};

use crate::business::erp::inventory_aging::handler::PERM_LIST;
use crate::business::erp::inventory_aging::model::AGING_FUNCTION;

/// 迁移目录（`backend/app/migrations`，与 `doc_reader_impl_tests` 同一取法）。
fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations")
}

fn read_migration(name: &str) -> String {
    let path = migrations_dir().join("business").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("读不到迁移 {}：{error}", path.display()))
}

fn up_sql() -> String {
    read_migration("V0424__inventory_aging_calc.up.sql")
}

fn down_sql() -> String {
    read_migration("V0424__inventory_aging_calc.down.sql")
}

/// V0424 必须同时给出：库函数（含 RETURNS TABLE 的关键列）+ 权限码 + 挂载点。
#[test]
fn migration_declares_aging_function_and_permission() {
    let up = up_sql();
    assert!(
        up.contains("CREATE OR REPLACE FUNCTION biz_inventory_aging_layers("),
        "V0424 必须建库函数 biz_inventory_aging_layers"
    );
    for column in [
        "qty_in",
        "qty_consumed",
        "qty_remaining",
        "unit_cost",
        "amount_remaining",
        "inbound_date",
        "lot_no",
    ] {
        assert!(up.contains(column), "函数出参缺列 {column}（调用方会静默拿到 NULL）");
    }
    // 幂等：CREATE OR REPLACE + 菜单 ON CONFLICT DO NOTHING + 授权 NOT EXISTS
    assert!(
        up.contains("LANGUAGE sql") && up.contains("STABLE"),
        "函数必须是 STABLE 的只读 SQL 函数"
    );
    assert!(up.contains("ON CONFLICT (id) DO NOTHING"), "菜单节点必须幂等插入");
    assert!(up.contains("NOT EXISTS"), "授权必须按码判重（重放不长第二行）");
    // 权限码 + 挂载点（按钮挂「即时库存」页面下，sort 40 接在 30 之后）
    assert!(up.contains("'business:erp:inventory-aging:list'"));
    assert!(up.contains("20000000-0000-0000-0000-000000004240"));
    assert!(
        up.contains("business:erp:inventory:list"),
        "必须用 auth_code 反查父页面"
    );
    // sort_order = 40 接在兄弟按钮 10/20/30 之后（同父必须唯一，约定 12）
    assert!(
        up.contains("40,          --"),
        "sort_order 必须是 40（10/20/30 已被兄弟按钮占用）"
    );
    println!("[M2-08] V0424：库函数 + 权限码 + 幂等 + 挂载点逐项在位 ✓");
}

/// 「累计 + 夹 0」只许写一份：库函数里恰好一次，Rust 取数层一次都不许有。
#[test]
fn fifo_formula_exists_only_in_the_database_function() {
    let up = up_sql();
    assert_eq!(
        up.matches("LEAST(l.qty, l.cum_in").count(),
        1,
        "FIFO 公式在 V0424 里必须恰好出现一次"
    );
    assert_eq!(
        up.matches("SUM(w.qty) OVER (PARTITION BY w.material_id, w.warehouse_id")
            .count(),
        1,
        "累计入库的窗口函数必须恰好出现一次"
    );

    // 取数层（model.rs）只能是「FROM 函数 + WHERE」，自己再算一遍 FIFO 就是两处真相
    let model_src = include_str!("model.rs");
    assert!(
        model_src.contains("FROM biz_inventory_aging_layers("),
        "model 必须调库函数"
    );
    assert!(
        !model_src.contains("OVER ("),
        "model 里不许出现窗口函数（FIFO 只住在库函数里）"
    );
    assert!(!model_src.contains("LEAST("), "model 里不许出现 FIFO 的夹 0 表达式");
    assert!(!model_src.contains("GREATEST("), "model 里不许出现 FIFO 的夹 0 表达式");
    println!("[M2-08] FIFO 公式只在库函数里（Rust 取数层零重复实现）✓");
}

/// down 只撤本迁移种的三样东西，且**不碰**业务表。
#[test]
fn down_migration_reverts_exactly_what_up_created() {
    let down = down_sql();
    assert!(down.contains("DROP FUNCTION IF EXISTS biz_inventory_aging_layers(date, uuid, uuid)"));
    assert!(down.contains("DELETE FROM sys_role_menu"), "必须撤授权行");
    assert!(down.contains("DELETE FROM sys_menu"), "必须撤菜单行");
    for table in ["biz_inventory_txn", "biz_stock_in", "biz_stock_out", "biz_stock_take"] {
        assert!(
            !down
                .to_uppercase()
                .contains(&format!("DELETE FROM {}", table.to_uppercase())),
            "down 不许删业务数据（{table}）"
        );
    }
    println!("[M2-08] V0424 down：函数 + 菜单 + 授权，不碰业务数据 ✓");
}

/// 代码里的函数名 / 权限码必须与 DDL 逐字一致。
#[test]
fn function_and_permission_constants_match_ddl() {
    let up = up_sql();
    assert_eq!(AGING_FUNCTION, "biz_inventory_aging_layers");
    assert!(
        up.contains(&format!("FUNCTION {AGING_FUNCTION}(")),
        "代码里的函数名与 DDL 不一致"
    );
    assert_eq!(PERM_LIST, "business:erp:inventory-aging:list");
    assert!(up.contains(&format!("'{PERM_LIST}'")), "代码里的权限码与种子不一致");
    println!("[M2-08] 函数名 / 权限码与 V0424 逐字一致（{AGING_FUNCTION} / {PERM_LIST}）✓");
}

/// 迁移文件名遵循 `V####__name.{up,down}.sql` 成对约定（加载器按此解析）。
#[test]
fn migration_file_pair_is_well_formed() {
    let dir = migrations_dir().join("business");
    for name in [
        "V0424__inventory_aging_calc.up.sql",
        "V0424__inventory_aging_calc.down.sql",
    ] {
        assert!(dir.join(name).is_file(), "缺迁移文件 {name}");
    }
    println!("[M2-08] 迁移 up/down 成对在位 ✓");
}
