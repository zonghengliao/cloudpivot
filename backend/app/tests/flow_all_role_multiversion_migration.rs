//! 迁移 `V0367`（会签改签 · 多版本流程支持）的静态不变量。
//!
//! # 为什么要有这层测试（2026-11-09 生产部署失败复盘）
//!
//! `V0359`（把「角色型审批」的 `ALL` 改成 `ANY`）实现里把新版本号写成
//! `c_v1_no + 1`（当前版本号 + 1）。生产（144）的 `po_approve` 是
//! `v1 ARCHIVED / v2 PUBLISHED / v3 DRAFT / v4 DRAFT`，`current = v2`
//! ⇒ 它想发 `v3`，而 `v3` 已被一个草稿占住 ⇒
//! `duplicate key value violates unique constraint "uk_biz_flow_def_version"`
//! ⇒ 整个迁移事务回滚 ⇒ 服务起不来。
//!
//! V0367 是它的修正版：版本号改成 `MAX(version_no) + 1`（全库该流程最大版本 + 1）。
//! 这几条断言把「修正版必须一直保持的正确形状」钉死，防止将来有人：
//!
//! 1. 把版本号又改回硬编码（回到同一个坑）；
//! 2. 顺手「修好」`V0359`（它已被 144 / 145 记录为已应用，改一个字节就会校验和失败，
//!    让下一台机器升级时直接起不来 —— 修法只能是新增迁移，不能改旧文件）；
//! 3. 让迁移去碰 `COUNTERSIGN` 会签节点（那是正确配置，碰了就是业务事故）；
//! 4. 新迁移的版本号重号 / 少补零 / 缺 down 文件（`alun` loader 对重号直接启动失败）。
//!
//! 纯文件解析，不连库、不需要任何环境变量 ⇒ 任何一次 `cargo test` 都会跑到。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `backend/` 根目录（`backend/tests/xxx.rs` → 上跳一级）。
fn backend_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn migrations_root() -> PathBuf {
    backend_root().join("migrations")
}

/// 读一个迁移文件；读不到直接 fail（迁移文件缺失本身就是回归）。
fn read_migration(relative: &str) -> String {
    let path = migrations_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到迁移文件 {}: {e}", path.display()))
}

/// 把连续空白压成单个空格：SQL 断言只关心语句形状，不该被缩进/换行绑死。
fn squash(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn assert_has(sql: &str, snippet: &str, why: &str) {
    let squashed = squash(sql);
    assert!(
        squashed.contains(&squash(snippet)),
        "{why}\n期望包含：{}",
        squash(snippet)
    );
}

/// 去掉 `--` 行注释（文件头大段注释里会**引用** V0359 的错误写法，
/// 「禁止出现」类断言必须只看真正会执行的 SQL）。
fn sql_only(sql: &str) -> String {
    sql.lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
}

const V0367_UP: &str = "business/V0367__flow_all_role_approval_to_any_multiversion.up.sql";
const V0367_DOWN: &str = "business/V0367__flow_all_role_approval_to_any_multiversion.down.sql";
const V0359_UP: &str = "business/V0359__flow_all_role_approval_to_any.up.sql";

/// `V0367` 的 up / down 必须成对存在。
#[test]
fn v0367_up_and_down_exist() {
    for rel in [V0367_UP, V0367_DOWN] {
        let path = migrations_root().join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ① 版本号必须是动态的 `MAX(version_no) + 1`，不能回到硬编码。
#[test]
fn v0367_version_number_is_dynamic_max_plus_one() {
    let sql = read_migration(V0367_UP);

    assert_has(
        &sql,
        "COALESCE(MAX(version_no), 0) + 1",
        "V0367 必须用「该流程最大版本号 + 1」发新版（V0359 用当前版本号 + 1，撞了已存在的 DRAFT 版本）",
    );
    assert_has(
        &sql,
        "FROM biz_flow_def_version WHERE def_id = c_def",
        "V0367 的 MAX(version_no) 必须按 def_id 聚合（同一流程内取最大）",
    );
    assert_has(
        &sql,
        "(c_new_ver, c_def, v_next_no, 'PUBLISHED', TRUE",
        "V0367 的版本行 INSERT 必须写 v_next_no（动态），不能写死版本号",
    );

    // V0359 的旧形状都不允许在 V0367 里复活（只看会执行的 SQL，跳过注释）
    let code = sql_only(&sql);
    for forbidden in ["c_v1_no + 1", "v_cur_no + 1"] {
        assert!(
            !code.contains(forbidden),
            "V0367 里不允许出现 `{forbidden}`：那是 V0359 的写法（当前版本号 + 1），\
             多版本流程（po_approve v1..v4）会撞 uk_biz_flow_def_version"
        );
    }
}

/// ② 只处理 `APPROVAL + ROLE + ALL`，`COUNTERSIGN` 一律不动。
#[test]
fn v0367_only_touches_approval_role_all_nodes() {
    let sql = read_migration(V0367_UP);

    assert_has(
        &sql,
        "n.node_type = 'APPROVAL'",
        "V0367 的目标判定必须带 node_type = 'APPROVAL'",
    );
    assert_has(
        &sql,
        "n.assignee_type = 'ROLE'",
        "V0367 的目标判定必须带 assignee_type = 'ROLE'",
    );
    assert_has(&sql, "n.approval_mode = 'ALL'", "V0367 只认 ALL 的节点为错配");
    let code = sql_only(&sql);
    assert!(
        code.contains("SET approval_mode = 'ANY'"),
        "V0367 只把 approval_mode 从 ALL 改成 ANY"
    );
    assert!(
        !code.contains("'COUNTERSIGN'"),
        "V0367 的 SQL 里不允许出现 'COUNTERSIGN' 字面量：跨角色会签（COUNTERSIGN + ALL）\
         是正确配置，必须一字不动（注释里提到 COUNTERSIGN 不加引号，不受影响）"
    );
    assert!(
        !code.contains("UPDATE biz_flow_node_assignee"),
        "V0367 只改签名模式，审批人绑定（biz_flow_node_assignee）只允许照抄，不允许 UPDATE"
    );
    assert!(
        !code.contains("UPDATE biz_flow_edge"),
        "V0367 不动边：边只允许复制到新版本"
    );
}

/// ③ 幂等性来自「按形状过滤」：只处理 `is_current + PUBLISHED` 且仍有错配节点的流程。
#[test]
fn v0367_is_idempotent_by_shape_filter() {
    let sql = read_migration(V0367_UP);

    assert_has(
        &sql,
        "v.is_current AND v.version_status = 'PUBLISHED'",
        "V0367 必须只处理「当前且已发布」的版本（草稿不碰）",
    );
    assert_has(
        &sql,
        "EXISTS ( SELECT 1 FROM biz_flow_node n WHERE n.version_id = v.id",
        "V0367 的循环必须用 EXISTS 按节点形状筛选目标流程：处理完一轮后新的当前版本里\
         已无 APPROVAL+ROLE+ALL ⇒ 第二次执行目标清单为空 ⇒ 幂等",
    );
    assert_has(
        &sql,
        "RAISE NOTICE 'V0367 完成：0 个流程需要处理",
        "V0367 必须在 0 变更时给出明确 NOTICE（排障时一眼区分「已经改过」与「被跳过」）",
    );
    assert_has(&sql, "WHERE d.deleted_at IS NULL", "V0367 必须排除软删流程");
}

/// ④ `V0359` 已被 144 / 145 记录为已应用 —— 文件内容必须保持冻结。
#[test]
fn v0359_stays_frozen() {
    let sql = read_migration(V0359_UP);

    // 它仍然保留「当前版本号 + 1」的原始写法（这才是「没被就地修过」的证据）
    assert_has(
        &sql,
        "(c_v2, c_def, c_v1_no + 1, 'PUBLISHED', TRUE",
        "V0359 的版本行 INSERT 形状变了：V0359 已被 144 / 145 记入 schema_migrations，\
         改它会让校验和比对失败（下一台机器升级直接起不来）。修法只能是新增迁移（V0367）",
    );
    assert_eq!(
        sql.len(),
        19141,
        "V0359 的字节数变了（已应用迁移必须冻结；要改口径请新增迁移，不能改它）"
    );
}

/// ⑤ up 与 down 的标记必须对得上（down 靠 `change_note LIKE 'V0367：%'` 找自己发的版本）。
#[test]
fn v0367_up_and_down_markers_match() {
    let up = read_migration(V0367_UP);
    let down = read_migration(V0367_DOWN);

    assert_has(
        &up,
        "'V0367：' || v_patched",
        "V0367 up 发的版本行 change_note 必须以 'V0367：' 开头",
    );
    assert_has(
        &down,
        "v.change_note LIKE 'V0367：%'",
        "V0367 down 必须用同一个标记（'V0367：%'）定位自己发的版本",
    );
    assert_has(
        &down,
        "biz_flow_instance",
        "V0367 down 必须在删除版本前检查是否有审批单挂在上面（否则会留下指向空气的单）",
    );
}

/// ⑥ 迁移号全局不变量：版本号唯一、4 位补零、每个 up 都有 down。
///
/// `alun` 的 loader 对版本号重号是**直接启动失败**（不是「后一条覆盖前一条」），
/// 而 2026-11 期间 0363 / 0365 / 0366 是并行卡各自占号 —— 靠人眼核对不可靠。
/// 少补零会让排序错乱（`V360` 排在 `V0362` 之后、`V0365` 之前），down 缺失则回滚无路。
#[test]
fn migration_versions_are_unique_padded_and_have_down() {
    let mut ups: Vec<PathBuf> = Vec::new();
    collect_sql(&migrations_root(), &mut ups);

    let mut by_version: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in ups {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        if !name.starts_with('V') || !name.ends_with(".up.sql") {
            continue;
        }
        let version = name[1..].split("__").next().unwrap_or_default().to_string();
        assert!(
            version.len() == 4 && version.chars().all(|c| c.is_ascii_digit()),
            "迁移版本号必须是 4 位补零的数字：{}",
            path.display()
        );
        by_version.entry(version).or_default().push(path.display().to_string());

        let down = path.with_file_name(name.replace(".up.sql", ".down.sql"));
        assert!(down.is_file(), "缺少 down 迁移：{}", down.display());
    }

    let dups: Vec<String> = by_version
        .iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|(v, files)| format!("V{v} → {files:?}"))
        .collect();
    assert!(
        dups.is_empty(),
        "迁移版本号重复（alun loader 会直接拒绝启动）：{dups:#?}"
    );
    assert!(
        by_version.contains_key("0367"),
        "V0367 应该被扫到（确认扫描逻辑没漏目录）"
    );
}

/// 递归收集目录下的 `*.sql`。
fn collect_sql(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sql(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("sql") {
            out.push(path);
        }
    }
}
