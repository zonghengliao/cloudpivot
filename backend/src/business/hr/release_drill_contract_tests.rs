//! HR-49 的契约测试：**上线演练脚本**的静态形态。
//!
//! 为什么这些用例不连库：本卡的交付物是一条脚本
//! （`scripts/verify/hr/hr_49_release_drill.sh`），它真正跑一次要
//! 「在 145 建 4 个演练库 → 回放 300+ 条迁移 → 从生产 144 拉快照 → 起 4 次后端」，
//! 十几分钟量级且要库口令 —— 那是人工演练的活，不适合塞进 `cargo test`。
//!
//! 这里守的是**跑不了库也能发现**的退化（每一条都对应一次真实会踩的坑）：
//!
//! 1. 四个场景（空库首启 / 生产克隆 / 老库含 2211 翻末级 / 护栏反证）被删一个
//!    → 卡面验收「两次都不丢数据」只剩一半证据；
//! 2. 演练库名护栏被删 → 脚本会往生产库/Agent 主机上建库写夹具（用户已批评过一次）；
//! 3. 口令被写死进脚本 → 口令进仓库（本仓 pre-commit 会拦，但脚本里也不许出现）；
//! 4. 跑完不删库 → 145 上堆垃圾库；
//! 5. 「不丢数据」的判据（逐表行数快照 + 只许白名单增长）被删 → 变成一句空话；
//! 6. 2211 翻末级的**前后**两条断言少一条 → 只证明「有 9 个科目」证明不了「翻过」；
//! 7. 迁移条数/最高版本被写成字面量（323 / 403）→ 下次加迁移就假红或假绿。
//!
//! 与 HR-46 的 `hr_e2e_contract_tests.rs`、HR-45 的 `hr_invariants_tests.rs` 同一做法：
//! 用 [`env!("CARGO_MANIFEST_DIR")`] 定位仓库根读文件，不引新依赖；
//! 读不到直接 panic（**不**静默跳过）。

use std::path::{Path, PathBuf};

/// 仓库根（`backend/` 的上一级）。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("backend/ 一定有上级目录")
        .to_path_buf()
}

/// 读仓库内文件；读不到直接失败（脚本被删时用例必须红，不许静默 SKIP）。
fn read_repo(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("读不到 {}：{err}", path.display()))
}

const DRILL: &str = "scripts/verify/hr/hr_49_release_drill.sh";

/// 四个场景必须都在，且都能单独跑（`all|fresh|prod|legacy|guard`）。
#[test]
fn drill_declares_the_four_scenarios_of_the_card() {
    let s = read_repo(DRILL);
    // 场景声明形态是 `scenario S1 "空库首启（迁移全跑）"`（渲染成 `[场景 S1] …` 打在输出里）
    for marker in [
        "scenario S1 \"空库首启",
        "scenario S2 \"克隆库升级（真实生产快照",
        "scenario S3 \"克隆库升级（老库",
        "scenario S4 \"升级护栏反证",
    ] {
        assert!(
            s.contains(marker),
            "演练脚本缺场景声明「{marker}」—— 卡面验收（空库首启 + 克隆库升级 + 两次都不丢数据）就少了一条腿"
        );
    }
    // 模式解析必须接受四个单场景名，否则只能整跑
    assert!(
        s.contains("all|fresh|prod|legacy|guard"),
        "脚本必须支持 all/fresh/prod/legacy/guard 五个模式（现在只能整跑）"
    );
}

/// 演练库护栏：库名必须含 drill/e2e/test，否则拒绝执行（绝不在生产库/Agent 主机上写）。
#[test]
fn drill_refuses_non_drill_database_names() {
    let s = read_repo(DRILL);
    assert!(
        s.contains("不像演练库") && s.contains("*drill*"),
        "脚本必须校验库名含 drill/e2e/test —— 这是「绝不在 99 上建库、绝不碰生产」的唯一硬闸"
    );
    assert!(
        s.contains("DB_FRESH=hr49_fresh_drill")
            && s.contains("DB_PROD=hr49_prod_drill")
            && s.contains("DB_LEGACY=hr49_legacy_drill")
            && s.contains("DB_GUARD=hr49_guard_drill"),
        "四个演练库名都必须带 drill 后缀（与护栏判据一致）"
    );
    assert!(
        s.contains("172.18.10.145"),
        "演练库主机必须是 145（卡面 2026-11-12 明确：绝对不要建在 99）"
    );
}

/// 口令只能从 145 的私有 env 读（ssh），不许写进脚本。
#[test]
fn drill_never_hardcodes_the_database_password() {
    let s = read_repo(DRILL);
    assert!(
        s.contains("/etc/cloudpivot/cloudpivot-dev.env"),
        "口令应经 ssh 从 145 的 /etc/cloudpivot/cloudpivot-dev.env 读"
    );
    assert!(
        s.contains("HR49_PGPASSWORD"),
        "必须支持 HR49_PGPASSWORD 环境变量覆盖（CI/人工演练）"
    );
    for bad in ["PGPASSWORD=cloud", "NIK_DATABASE_PASSWORD=cloud", "password123"] {
        assert!(!s.contains(bad), "脚本里出现了疑似写死的口令片段「{bad}」");
    }
}

/// 跑完必须自己删库（四个场景各一条），否则 145 上堆垃圾库。
#[test]
fn drill_drops_every_database_it_creates() {
    let s = read_repo(DRILL);
    assert!(
        s.contains("DROP DATABASE IF EXISTS $1 WITH (FORCE)") || s.contains("DROP DATABASE IF EXISTS $db WITH (FORCE)"),
        "建库函数必须能强制删库（WITH (FORCE)，否则有连接时删不掉）"
    );
    let n = s.matches("drop_if_green \"").count();
    assert_eq!(n, 4, "四个场景都要在收尾调 drop_if_green（零失败才删），实际 {n} 处");
    assert!(
        s.contains("pg_database 残留计数"),
        "删库后必须复查 pg_database 残留计数（证明真的删了）"
    );
}

/// 「不丢数据」必须有硬判据：逐表行数快照 + 只许白名单表增长。
#[test]
fn drill_proves_no_data_is_lost() {
    let s = read_repo(DRILL);
    assert!(
        s.contains("c.relkind = 'r'") && s.contains("\\gexec"),
        "行数快照要动态遍历 public 下所有表（不写死表名），以后加表自动纳入"
    );
    assert!(
        s.contains("行数减少") && s.contains("丢数据"),
        "对拍必须把「行数减少」直接判成丢数据（硬失败）"
    );
    assert!(
        s.contains("GROW_OK"),
        "增长必须有白名单（没写的表一长就 FAIL，逼人确认是不是重灌）"
    );
    // 白名单里每条都要有理由注释，否则下次有人顺手加表名
    assert!(s.contains("白名单逐条给出「为什么它应该长」"), "白名单必须逐条写理由");
}

/// 2211 翻末级必须有「升级前是末级 / 升级后不是末级」两条断言。
#[test]
fn drill_asserts_2211_leaf_flip_both_sides() {
    let s = read_repo(DRILL);
    assert!(
        s.contains("S3 老库 2211 仍是末级（升级前基线）"),
        "缺「升级前 2211 是末级」的基线断言 —— 否则证明不了是这次升级翻的"
    );
    assert!(
        s.contains("S3 升级后 2211 翻成非末级"),
        "缺「升级后 2211 是非末级」的断言"
    );
    assert!(
        s.contains("2211.01','2211.02','2211.03','2241.03','2241.04','2221.03','5602.10','5602.11','5602.12"),
        "9 个工资末级科目必须逐码点名（V0384 的交付物）"
    );
}

/// 克隆库要贴真实：生产 144 只读 pg_dump + 老库按发布过的旧版本集合回放。
#[test]
fn drill_clones_prod_and_replays_legacy_migration_set() {
    let s = read_repo(DRILL);
    assert!(
        s.contains("pg_dump") && s.contains("--no-owner --no-privileges"),
        "生产克隆必须走 pg_dump（只读、不带 owner/privilege）"
    );
    assert!(
        s.contains("/etc/cloudpivot/cloudpivot.env"),
        "生产源口令应读 144 的 /etc/cloudpivot/cloudpivot.env"
    );
    assert!(
        s.contains("build_legacy_tree"),
        "老库必须由「旧版本发布过的迁移集合」回放出（不是手工改数据造出来的）"
    );
    assert!(
        s.contains("LEGACY_MAX=${HR49_LEGACY_MAX:-376}"),
        "老库基线默认应为 376（V0377 成本科目之前：2211 还是末级）"
    );
}

/// 端口必须落在隔离段 18000-18999（prod 是 8023）。
#[test]
fn drill_uses_isolated_ports_only() {
    let s = read_repo(DRILL);
    for p in ["18149", "18150", "18151", "18152"] {
        assert!(s.contains(p), "缺隔离端口 {p}");
    }
    assert!(s.contains("127.0.0.1:"), "实例只许监听回环（不许 0.0.0.0）");
    assert!(!s.contains(":8023"), "脚本里不许出现生产端口 8023");
}

/// 迁移条数/最高版本必须从磁盘算，不许写死（否则加一条迁移就假绿/假红）。
#[test]
fn drill_derives_migration_counts_from_disk() {
    let s = read_repo(DRILL);
    assert!(
        s.contains("TOTAL_VER=$(find") && s.contains("TOTAL_SEED=$(find"),
        "迁移条数必须用 find 现算"
    );
    assert!(s.contains("MAX_VER=$(find"), "最高版本必须用 find 现算");
    for bad in ["TOTAL_VER=323", "MAX_VER=403", "TOTAL_SEED=12"] {
        assert!(!s.contains(bad), "脚本写死了「{bad}」—— 下次加迁移就错");
    }
}

/// 升级完必须跑 HR-45 的 10 条巡检断言（收口自检不能只靠迁移里的 DO 块）。
#[test]
fn drill_runs_hr_invariants_after_upgrade() {
    let s = read_repo(DRILL);
    assert!(
        s.contains("hr_invariants.sh"),
        "演练收尾必须调 scripts/verify/hr_invariants.sh"
    );
    // S1（空库首启后）/ S2（生产克隆升级后）/ S3（老库升级后）三处 —— S4 是「升级被拦下」的场景，
    // 没有升级后的库可巡检，故不计。
    let n = s.matches("drill_invariants \"").count();
    assert!(n >= 3, "S1 / S2 升级后 / S3 升级后 都该跑巡检，实际 {n} 处");
}
