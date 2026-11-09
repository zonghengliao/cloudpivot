//! 巡检登记一致性：`10_checks.sql` 里定义的每个 `di.c_diNN` 视图，必须在
//! `scripts/verify/snapshot_audit.py` 的 `CHECKS` 清单里**登记同名编号**，反之亦然。
//!
//! # 为什么必须有这一层（不是「文档洁癖」）
//!
//! 巡检的**执行入口只有 `CHECKS` 清单**：`snapshot_audit.py` 逐条按 `CHECKS[i]["view"]`
//! 去查 `di.c_diNN`。于是有两种静默退化，而且都不会报错、也不会少行：
//!
//! 1. **视图写了、没登记** —— SQL 里 `CREATE OR REPLACE VIEW di.c_di26` 落地了，
//!    但 `CHECKS` 忘了加一条 ⇒ 这条巡检**永远不会跑**，日志里连名字都看不到。
//!    比「跑出来有命中」危险得多：人以为它绿着。
//! 2. **登记了、视图没写** —— `CHECKS` 加了条目但 SQL 没加视图 ⇒ 每轮巡检该项记
//!    `ERROR`（执行失败），整天吵但没人知道是缺一条 DDL。
//!
//! 这两种都是「改一处忘另一处」的机械错误，靠人眼 review 不可靠（DI 编号还在增长：
//! P2-6 加到 DI-22、P2-7 加到 DI-26）。本测试把两处对齐钉成一条断言。
//!
//! # 判等口径
//!
//! * SQL 侧：`VIEW di.c_diNN` 的 `NN` 集合（`CREATE OR REPLACE VIEW` / `CREATE VIEW` 都认）；
//! * 清单侧：`dict(id="DI-NN"` 的 `NN` 集合；
//! * 两边集合**必须相等**（不是包含）：多一条、少一条都红。
//!
//! 纯文件解析，不连库、不需要任何环境变量 ⇒ 任何一次 `cargo test` 都会跑到。
//! 找不到文件（只 checkout 了 backend 的场景）时打印 SKIP 直接返回。

use std::collections::BTreeSet;
use std::path::PathBuf;

/// 仓库根目录（`backend/tests/xxx.rs` → 上跳两级）
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend/ 应有上级目录（仓库根）")
        .to_path_buf()
}

/// 从 SQL 里抽出所有 `di.c_diNN` 视图的编号，规范成 `DI-NN`（补零到两位）
fn view_ids(sql: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    // 只认「创建视图」的上下文，避免把 `FROM di.c_di06` 之类的引用也算成定义
    for anchor in ["VIEW di.c_di", "view di.c_di"] {
        for chunk in sql.split(anchor).skip(1) {
            let digits: String = chunk.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                continue;
            }
            let n: u32 = match digits.parse() {
                Ok(n) => n,
                Err(_) => continue,
            };
            ids.insert(format!("DI-{n:02}"));
        }
    }
    ids
}

/// 从巡检脚本的 `CHECKS` 清单里抽出所有 `id="DI-NN"`
fn check_ids(py: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for chunk in py.split("dict(id=\"").skip(1) {
        let Some(rest) = chunk.strip_prefix("DI-") else {
            continue;
        };
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        if let Ok(n) = digits.parse::<u32>() {
            ids.insert(format!("DI-{n:02}"));
        }
    }
    ids
}

#[test]
fn n01_check_view_and_registry_are_bijective() {
    let root = repo_root();
    let sql_path = root.join("docs/architecture/snapshot-drift/10_checks.sql");
    let py_path = root.join("scripts/verify/snapshot_audit.py");
    let (Ok(sql), Ok(py)) = (std::fs::read_to_string(&sql_path), std::fs::read_to_string(&py_path)) else {
        println!("SKIP: 非完整仓库（缺 {} 或 {}）", sql_path.display(), py_path.display());
        return;
    };

    let views = view_ids(&sql);
    let checks = check_ids(&py);

    assert!(
        !views.is_empty(),
        "10_checks.sql 里没解析出任何 `VIEW di.c_diNN`，解析口径需更新（可能是 DDL 写法变了）"
    );
    assert!(
        !checks.is_empty(),
        "snapshot_audit.py 里没解析出任何 `dict(id=\"DI-NN\"`，解析口径需更新（可能是清单结构变了）"
    );

    let missing_in_checks: Vec<&String> = views.difference(&checks).collect();
    let missing_in_sql: Vec<&String> = checks.difference(&views).collect();

    assert!(
        missing_in_checks.is_empty(),
        "这些巡检视图已定义但**没登记**进 snapshot_audit.py 的 CHECKS ⇒ 永远不会跑（静默漏报）：{missing_in_checks:?}\n\
         修法：在 CHECKS 里补 dict(id=\"…\", group=…, view=\"di.c_diNN\", level=…, contract=…, name=…, meaning=…)"
    );
    assert!(
        missing_in_sql.is_empty(),
        "这些巡检项已登记但**没有对应视图** ⇒ 每轮都记 ERROR：{missing_in_sql:?}\n\
         修法：在 10_checks.sql 里补 CREATE OR REPLACE VIEW di.c_diNN"
    );
    assert_eq!(
        views.len(),
        checks.len(),
        "视图数 {} 与登记数 {} 不等（集合相等但计数不等说明解析有重复/漏项）",
        views.len(),
        checks.len()
    );

    // 编号必须从 DI-01 起连续到最大值：跳号意味着「曾经删过一条」或「手写编号撞号」，
    // 两种情况都会让「按编号追一条巡检」的人找不到东西（落库表 biz_snapshot_drift.issue_code
    // 与 dashboard 都按 issue_code 聚合，跳号会被当成「这条从没命中过」）。
    let max = views
        .iter()
        .filter_map(|id| id.strip_prefix("DI-").and_then(|n| n.parse::<u32>().ok()))
        .max()
        .expect("视图集合非空");
    let nums: BTreeSet<u32> = views
        .iter()
        .filter_map(|id| id.strip_prefix("DI-").and_then(|n| n.parse::<u32>().ok()))
        .collect();
    let expected: BTreeSet<u32> = (1..=max).collect();
    assert_eq!(
        nums,
        expected,
        "巡检编号不连续（DI-01..DI-{max:02}）：缺 {:?}",
        expected.difference(&nums).collect::<Vec<_>>()
    );
}
