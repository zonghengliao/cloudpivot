//! 迁移 `V0387`（人力资源 · 36 行菜单 + HR_MANAGER 角色 + 授权）的静态不变量。
//!
//! 卡：t_248d6edc（HR-04【HR】迁移 V0381）—— 设计依据
//! `docs/architecture/ARCH-9-人力资源模块设计.md` §3.2 ④（1 目录 + 5 页面 + 30 按钮 = 36 行）、
//! §3.2 ⑤（`HR_MANAGER` 角色 + 授权范围）、§5.2（46 条 API 的权限码）、§6.1（页面清单 / path /
//! icon）、§7.1（迁移清单，本卡实际号 V0387）、§8.1（拆卡清单 HR-04）。
//!
//! # 号段
//!
//! 卡面 `V0381` 已被成本核算实占（`V0381__cost_flow_def`）；设计 §7.1 的"实际号"列经两次顺延后
//! 定稿为 **V0387**（V0383=hr_tables / V0384=hr_accounts / V0385=cost_module_switch /
//! V0386=hr_seq_dict）。本仓迁移版本号跨目录唯一，重号 = 服务启动失败。
//!
//! # 这层测试钉死什么
//!
//! 菜单/角色/授权是**种一次、事后只能手写补丁**的数据（`schema_migrations` 记内容指纹），
//! 写错的代价不是编译错误，而是运行期静默残废：
//!
//! 1. 菜单少一行 ⇒ 角色授权树上缺分支，前端侧栏点不进去（且没人报错）；
//! 2. 页面挂错父级 / 同父 `sort_order` 撞车 ⇒ 侧栏顺序每次刷新都变（AGENTS 约定 12）；
//! 3. `title` 写成长句 ⇒ 侧栏换行（规范 §1.6.1；`menu_invariants` ⑥ 判红）；
//! 4. `icon` 用了 TDesign 不存在的名字 ⇒ 那一项**空白图标**，而且 `icon` 非空时组件兜底不触发
//!    （`menu_invariants` ⑧ 判红）；
//! 5. 授了按钮却没授父页面 / 授了页面却没授祖先目录 ⇒ 非超管侧边栏整棵子树被静默丢掉；
//! 6. `HR_MANAGER` 角色没建 ⇒ 授权 `INSERT … SELECT` 选中 0 行，迁移照样"成功"；
//! 7. 按钮码没登记到 `permission_audit_tests::PENDING_LANDING` ⇒ N14 判死码、L2 连库闸整片红；
//! 8. `down` 按 role_code 通配删授权 ⇒ 误伤别的迁移种的同码菜单授权；
//! 9. 引擎拆分器把 `DO $$ … $$` 内部的 `;` 当分隔符 ⇒ 整笔迁移回滚、服务起不来。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由 `tests/flow_all_role_multiversion_migration.rs`
//! 的 `migration_versions_are_unique_padded_and_have_down` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（空库首启 / up×2 / down×2 / 7 条护栏反证 / menu_invariants 8 条）的结果写在卡内汇报里。

use std::path::PathBuf;

fn backend_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    backend_root().parent().expect("backend/ 应有父目录").to_path_buf()
}

fn read_migration(relative: &str) -> String {
    let path = backend_root().join("migrations").join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到迁移文件 {}: {e}", path.display()))
}

fn squash(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 去掉**所有**空白：菜单行这类多列 VALUES 的比对只关心列的次序与取值，不该被对齐空格绑死。
fn nospace(sql: &str) -> String {
    sql.chars().filter(|c| !c.is_whitespace()).collect()
}

fn assert_has_nows(sql: &str, snippet: &str, why: &str) {
    assert!(
        nospace(sql).contains(&nospace(snippet)),
        "{why}\n期望包含（忽略空白）：{}",
        squash(snippet)
    );
}

/// 去掉 `--` 行注释：注释里会**引用**错误写法做反面教材，「禁止出现」类断言只能看真正执行的 SQL。
fn sql_only(sql: &str) -> String {
    sql.lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_has(sql: &str, snippet: &str, why: &str) {
    let squashed = squash(sql);
    assert!(
        squashed.contains(&squash(snippet)),
        "{why}\n期望包含：{}",
        squash(snippet)
    );
}

/// 取一个语句块的「首行真正的 SQL」。
fn sql_head(statement: &str) -> String {
    statement
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("--"))
        .unwrap_or("")
        .to_ascii_uppercase()
}

/// `INSERT INTO <t>` / `DELETE FROM <t>` / `UPDATE <t>` 的目标表名。
fn dml_targets(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    for keyword in ["INSERT INTO", "DELETE FROM", "UPDATE"] {
        let mut rest = sql;
        while let Some(pos) = rest.find(keyword) {
            let after = &rest[pos + keyword.len()..];
            let table: String = after
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !table.is_empty() {
                out.push(table);
            }
            rest = after;
        }
    }
    out
}

/// 从 `NAME UUID[] := ARRAY[ … ]::uuid[];` 里取出 UUID 字面量。
fn extract_uuid_array(sql: &str, name: &str) -> Vec<String> {
    let start = sql
        .find(&format!("{name} UUID[] := ARRAY["))
        .unwrap_or_else(|| panic!("迁移里找不到数组 `{name}`（授权矩阵的写法变了？）"));
    let body = &sql[start..];
    let end = body
        .find("]::uuid[]")
        .unwrap_or_else(|| panic!("数组 `{name}` 没有 `]::uuid[]` 结尾"));
    body[..end]
        .split('\'')
        .skip(1)
        .step_by(2)
        .filter(|s| s.starts_with("2000"))
        .map(str::to_string)
        .collect()
}

/// 从文本里抽 `business:hr:*` 权限码（遇到 `%` / 结尾冒号的不算，那些是 LIKE 模式）。
fn hr_codes(text: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == ':' || ch == '-' || ch == '_' {
            cur.push(ch);
        } else {
            if cur.starts_with("business:hr:") && !cur.ends_with(':') {
                out.insert(cur.clone());
            }
            cur.clear();
        }
    }
    if cur.starts_with("business:hr:") && !cur.ends_with(':') {
        out.insert(cur);
    }
    out
}

const V0387_UP: &str = "business/V0387__hr_menu_perms.up.sql";
const V0387_DOWN: &str = "business/V0387__hr_menu_perms.down.sql";

/// 目录 UUID（顶级，sort 77）。
const DIR_ID: &str = "20000000-0000-4000-8000-00000000e000";

/// 5 个页面：(id, path, sort, title, icon, component)
const PAGES: [(&str, &str, i32, &str, &str, &str); 5] = [
    (
        "20000000-0000-4000-8000-00000000e001",
        "/business/hr/employee",
        10,
        "员工档案",
        "user",
        "business/hr/employee/index",
    ),
    (
        "20000000-0000-4000-8000-00000000e002",
        "/business/hr/attendance",
        20,
        "考勤管理",
        "calendar",
        "business/hr/attendance/index",
    ),
    (
        "20000000-0000-4000-8000-00000000e003",
        "/business/hr/leave",
        30,
        "请假单",
        "time",
        "business/hr/leave/index",
    ),
    (
        "20000000-0000-4000-8000-00000000e004",
        "/business/hr/salary",
        40,
        "薪资管理",
        "wallet",
        "business/hr/salary/index",
    ),
    (
        "20000000-0000-4000-8000-00000000e005",
        "/business/hr/salary-item",
        50,
        "薪资项",
        "setting",
        "business/hr/salary-item/index",
    ),
];

/// 30 个按钮：(id, auth_code, 父页面 id, sort)
const BUTTONS: [(&str, &str, &str, i32); 30] = [
    // 员工档案 9
    (
        "20000000-0000-4000-8000-00000000e011",
        "business:hr:employee:create",
        "20000000-0000-4000-8000-00000000e001",
        10,
    ),
    (
        "20000000-0000-4000-8000-00000000e012",
        "business:hr:employee:update",
        "20000000-0000-4000-8000-00000000e001",
        20,
    ),
    (
        "20000000-0000-4000-8000-00000000e013",
        "business:hr:employee:delete",
        "20000000-0000-4000-8000-00000000e001",
        30,
    ),
    (
        "20000000-0000-4000-8000-00000000e014",
        "business:hr:employee:detail",
        "20000000-0000-4000-8000-00000000e001",
        40,
    ),
    (
        "20000000-0000-4000-8000-00000000e015",
        "business:hr:employee:sensitive",
        "20000000-0000-4000-8000-00000000e001",
        50,
    ),
    (
        "20000000-0000-4000-8000-00000000e016",
        "business:hr:employee:resign",
        "20000000-0000-4000-8000-00000000e001",
        60,
    ),
    (
        "20000000-0000-4000-8000-00000000e017",
        "business:hr:employee:import",
        "20000000-0000-4000-8000-00000000e001",
        70,
    ),
    (
        "20000000-0000-4000-8000-00000000e018",
        "business:hr:employee:export",
        "20000000-0000-4000-8000-00000000e001",
        80,
    ),
    (
        "20000000-0000-4000-8000-00000000e019",
        "business:hr:contract:manage",
        "20000000-0000-4000-8000-00000000e001",
        90,
    ),
    // 考勤 5
    (
        "20000000-0000-4000-8000-00000000e021",
        "business:hr:attendance:period",
        "20000000-0000-4000-8000-00000000e002",
        10,
    ),
    (
        "20000000-0000-4000-8000-00000000e022",
        "business:hr:attendance:record",
        "20000000-0000-4000-8000-00000000e002",
        20,
    ),
    (
        "20000000-0000-4000-8000-00000000e023",
        "business:hr:attendance:import",
        "20000000-0000-4000-8000-00000000e002",
        30,
    ),
    (
        "20000000-0000-4000-8000-00000000e024",
        "business:hr:attendance:export",
        "20000000-0000-4000-8000-00000000e002",
        40,
    ),
    (
        "20000000-0000-4000-8000-00000000e025",
        "business:hr:attendance:lock",
        "20000000-0000-4000-8000-00000000e002",
        50,
    ),
    // 请假 6
    (
        "20000000-0000-4000-8000-00000000e031",
        "business:hr:leave:create",
        "20000000-0000-4000-8000-00000000e003",
        10,
    ),
    (
        "20000000-0000-4000-8000-00000000e032",
        "business:hr:leave:update",
        "20000000-0000-4000-8000-00000000e003",
        20,
    ),
    (
        "20000000-0000-4000-8000-00000000e033",
        "business:hr:leave:delete",
        "20000000-0000-4000-8000-00000000e003",
        30,
    ),
    (
        "20000000-0000-4000-8000-00000000e034",
        "business:hr:leave:submit",
        "20000000-0000-4000-8000-00000000e003",
        40,
    ),
    (
        "20000000-0000-4000-8000-00000000e035",
        "business:hr:leave:cancel",
        "20000000-0000-4000-8000-00000000e003",
        50,
    ),
    (
        "20000000-0000-4000-8000-00000000e036",
        "business:hr:leave:detail",
        "20000000-0000-4000-8000-00000000e003",
        60,
    ),
    // 薪资 7
    (
        "20000000-0000-4000-8000-00000000e041",
        "business:hr:salary:detail",
        "20000000-0000-4000-8000-00000000e004",
        10,
    ),
    (
        "20000000-0000-4000-8000-00000000e042",
        "business:hr:salary:calc",
        "20000000-0000-4000-8000-00000000e004",
        20,
    ),
    (
        "20000000-0000-4000-8000-00000000e043",
        "business:hr:salary:submit",
        "20000000-0000-4000-8000-00000000e004",
        30,
    ),
    (
        "20000000-0000-4000-8000-00000000e044",
        "business:hr:salary:post",
        "20000000-0000-4000-8000-00000000e004",
        40,
    ),
    (
        "20000000-0000-4000-8000-00000000e045",
        "business:hr:salary:reverse",
        "20000000-0000-4000-8000-00000000e004",
        50,
    ),
    (
        "20000000-0000-4000-8000-00000000e046",
        "business:hr:salary:export",
        "20000000-0000-4000-8000-00000000e004",
        60,
    ),
    (
        "20000000-0000-4000-8000-00000000e047",
        "business:hr:salary:my",
        "20000000-0000-4000-8000-00000000e004",
        70,
    ),
    // 薪资项 3
    (
        "20000000-0000-4000-8000-00000000e051",
        "business:hr:salary-item:create",
        "20000000-0000-4000-8000-00000000e005",
        10,
    ),
    (
        "20000000-0000-4000-8000-00000000e052",
        "business:hr:salary-item:update",
        "20000000-0000-4000-8000-00000000e005",
        20,
    ),
    (
        "20000000-0000-4000-8000-00000000e053",
        "business:hr:salary-item:delete",
        "20000000-0000-4000-8000-00000000e005",
        30,
    ),
];

/// ① up/down 成对存在、非空，且 up 里有目录/页面/按钮三段 + 角色 + 授权 + 自检。
#[test]
fn v0387_up_and_down_exist_and_are_paired() {
    let up = read_migration(V0387_UP);
    let down = read_migration(V0387_DOWN);
    assert!(
        up.len() > 20_000,
        "V0387 up 太短（{} 字节）：36 行菜单 + 角色 + 授权 + 自检写完了吗",
        up.len()
    );
    assert!(
        down.len() > 4_000,
        "V0387 down 太短（{} 字节）：护栏 + 逆向删除 + 零残留断言写完了吗",
        down.len()
    );
    for snippet in [
        "INSERT INTO sys_menu",
        "INSERT INTO sys_role",
        "INSERT INTO sys_role_menu",
    ] {
        assert_has(&up, snippet, "up 缺一段：36 行菜单 / HR_MANAGER 角色 / 授权");
    }
    for snippet in [
        "DELETE FROM sys_role_menu",
        "DELETE FROM sys_menu",
        "DELETE FROM sys_role",
    ] {
        assert_has(&down, snippet, "down 缺一段逆向删除（授权 → 菜单 → 角色）");
    }
}

/// ② up 只碰 sys_menu / sys_role / sys_role_menu 三张表，且不做任何 DDL。
///
/// 菜单卡一旦顺手 `ALTER TABLE` 别的表，就会和 HR-05（附件 CHECK 放宽）/ HR-01（建表）撞车。
#[test]
fn v0387_up_writes_only_menu_and_role_tables_and_no_ddl() {
    let up = sql_only(&read_migration(V0387_UP));
    let targets = dml_targets(&up);
    assert!(
        !targets.is_empty(),
        "V0387 up 里没解析到任何 DML —— 解析器或文件结构变了"
    );
    for t in &targets {
        assert!(
            matches!(t.as_str(), "sys_menu" | "sys_role" | "sys_role_menu"),
            "V0387 up 写了不该写的表 `{t}`（本卡只写 sys_menu / sys_role / sys_role_menu）"
        );
    }
    let upper = up.to_ascii_uppercase();
    for banned in ["CREATE TABLE", "ALTER TABLE", "DROP TABLE", "TRUNCATE", "CREATE INDEX"] {
        assert!(
            !upper.contains(banned),
            "V0387 up 出现 `{banned}`：菜单卡不许做 DDL（表结构属 HR-01，约束放宽属 HR-05）"
        );
    }
    assert!(
        !upper.contains("DELETE FROM") && !upper.contains("UPDATE SYS_MENU"),
        "V0387 up 不许删/改既有行：客户可能已经改过菜单名或授权"
    );
}

/// ③ 36 行齐：1 目录 + 5 页面 + 30 按钮，且每个 id 都是 `…e0xx` 段（不吃别人的号段）。
#[test]
fn v0387_seeds_exactly_36_menu_rows() {
    let up = read_migration(V0387_UP);

    // 目录
    assert_has_nows(
        &up,
        &format!(
            "('{DIR_ID}'::uuid, '人力资源', 1, '', '/business/hr', NULL, 'Layout', 77, 1, 'user-business', '人力资源', TRUE, 'business')"
        ),
        "目录行必须顶级(parent NULL)/sort 77/Layout/user-business（设计 §6.1）",
    );

    // 5 个页面
    for (id, path, sort, title, icon, component) in PAGES {
        assert_has_nows(
            &up,
            &format!("('{id}'::uuid, '{title}', 2,"),
            &format!("页面 {path} 的 id/title 没按设计写"),
        );
        assert_has_nows(
            &up,
            &format!("'{path}', '{DIR_ID}'::uuid, '{component}', {sort}, 1, '{icon}', '{title}', TRUE, 'business'"),
            &format!("页面 {path} 的 path/父级/sort/component/icon 与设计 §6.1 不符"),
        );
    }

    // 30 个按钮：id + auth_code + name=auth_code + 父页面
    for (id, code, parent, sort) in BUTTONS {
        assert_has_nows(
            &up,
            &format!("('{id}'::uuid,'{code}',3,'{code}',NULL,'{parent}'::uuid,NULL,{sort},1,NULL,"),
            &format!("按钮 {code} 的行形态不对（id/name=auth_code/父页面/sort）"),
        );
    }

    // 没有第 37 行：本迁移的 e0xx id 只出现在这 36 个上
    let ids: std::collections::BTreeSet<String> = up
        .split('\'')
        .filter(|s| s.starts_with("20000000-0000-4000-8000-00000000e0") && s.len() == 36)
        .map(str::to_string)
        .collect();
    let mut expect: std::collections::BTreeSet<String> = BUTTONS.iter().map(|b| b.0.to_string()).collect();
    expect.extend(PAGES.iter().map(|p| p.0.to_string()));
    expect.insert(DIR_ID.to_string());
    assert_eq!(expect.len(), 36, "常量表本身就应该是 36 个 id");
    assert_eq!(
        ids, expect,
        "迁移里出现的 e0xx id 集合与 36 行设计不一致（多一行少一行都会在这里红）"
    );
}

/// ④ 权限码集合 = 5 个页面码 + 30 个按钮码，一个不多一个不少（AGENTS 约定 7 + §5.2）。
#[test]
fn v0387_permission_codes_match_design() {
    let up = sql_only(&read_migration(V0387_UP));
    let codes = hr_codes(&up);
    // 5 个页面码（资源段 :list）+ 30 个按钮码
    let mut expect: std::collections::BTreeSet<String> = BUTTONS.iter().map(|b| b.1.to_string()).collect();
    for page_code in [
        "business:hr:employee:list",
        "business:hr:attendance:list",
        "business:hr:leave:list",
        "business:hr:salary:list",
        "business:hr:salary-item:list",
    ] {
        expect.insert(page_code.to_string());
    }
    assert_eq!(expect.len(), 35, "5 页面码 + 30 按钮码 = 35");
    assert_eq!(codes, expect, "迁移里的 business:hr:* 权限码集合与设计 §5.2 不符");
}

/// ⑤ 目录/页面的侧栏口径：title 2-6 字、icon 非空、页面 sort 10 递增且同父唯一。
#[test]
fn v0387_titles_and_icons_follow_menu_convention() {
    let up = read_migration(V0387_UP);
    let mut titles = vec!["人力资源"];
    titles.extend(PAGES.iter().map(|p| p.3));
    for t in titles {
        let len = t.chars().count();
        assert!(
            (2..=6).contains(&len),
            "侧栏显示名「{t}」是 {len} 字：规范 §1.6.1 要求 2-6 字（module_invariants ⑥ 硬顶 12）"
        );
        assert!(
            !t.contains(' ') && !t.contains('/') && !t.contains('（'),
            "「{t}」带了空格/斜杠/括号：title 是菜单名不是说明"
        );
    }
    let sorts: Vec<i32> = PAGES.iter().map(|p| p.2).collect();
    let uniq: std::collections::BTreeSet<i32> = sorts.iter().copied().collect();
    assert_eq!(
        sorts.len(),
        uniq.len(),
        "5 个页面 sort_order 必须互不相同（同父唯一，AGENTS 约定 12）"
    );
    for (id, _path, _sort, _title, icon, _component) in PAGES {
        assert!(!icon.is_empty(), "页面 {id} 没设 icon（侧栏会空图标）");
    }
    assert_has(
        &up,
        "sort_order = 77",
        "目录顶级 sort 77 的护栏必须还在（被占要停住，不能并排）",
    );
}

/// ⑥ `HR_MANAGER` 角色：固定 UUID、中文名、data_scope=3、幂等靠 NOT EXISTS。
///
/// 注意：`sys_role.role_code` **没有**唯一约束，所以幂等只能靠 `NOT EXISTS (role_code=…)`；
/// 而 UUID 必须避开已被占用的 `…f0008`（那是 SALES_CLERK，照设计稿抄会撞主键）。
#[test]
fn v0387_creates_hr_manager_role_idempotently() {
    let raw = read_migration(V0387_UP);
    let up = sql_only(&raw);
    assert_has(
        &up,
        "INSERT INTO sys_role (id, role_code, role_name, description, status, data_scope, sort_order)",
        "角色插入的列必须显式列出（sys_role 有 data_scope / escalation_role_id 等默认值列）",
    );
    assert_has(
        &up,
        "SELECT '20000000-0000-4000-8000-0000000f0016'::uuid, 'HR_MANAGER', '人事经理',",
        "HR_MANAGER 的固定 UUID / 角色码 / 中文名",
    );
    assert_has(
        &up,
        "1, 3, 25",
        "status=1 / data_scope=3（本部门及以下）/ sort_order=25（80 已被 SALES_CLERK 占用）",
    );
    assert_has(
        &up,
        "WHERE NOT EXISTS (SELECT 1 FROM sys_role WHERE role_code = 'HR_MANAGER' AND deleted_at IS NULL)",
        "角色幂等必须靠 NOT EXISTS(role_code)：sys_role 上 role_code 没有唯一约束",
    );
    // 头注释会点名这两段"别照抄"的 id，所以只看真正执行的 SQL
    assert!(
        !up.contains("'20000000-0000-4000-8000-0000000f0008'"),
        "f0008 是 SALES_CLERK 的 id，用它建 HR_MANAGER 会撞 sys_role 主键（真库实测）"
    );
    assert!(
        !up.contains("20000000-0000-0000-0000-000000001600"),
        "…001600 是应付账龄分析菜单的 id，照证据脚本抄会让首行静默不插（真库实测）"
    );
    assert!(
        raw.contains("00000000e0"),
        "头注释应写明改用的 e0xx 号段（UUID 纠偏的依据要留在文件里）"
    );
}

/// ⑦ 授权矩阵：36/5/5/5，每条都含祖先目录，财务经理**不含**红冲。
#[test]
fn v0387_role_grants_matrix_is_closed_and_least_privilege() {
    let up = read_migration(V0387_UP);
    let all_hr = extract_uuid_array(&up, "all_hr");
    let fin_hr = extract_uuid_array(&up, "fin_hr");
    let lead_hr = extract_uuid_array(&up, "lead_hr");
    let emp_hr = extract_uuid_array(&up, "emp_hr");

    assert_eq!(all_hr.len(), 36, "all_hr 必须是 36 条（1 目录 + 5 页面 + 30 按钮）");
    assert_eq!(
        fin_hr.len(),
        5,
        "FINANCE_MANAGER 5 条（目录 + 薪资页 + detail/post/export）"
    );
    assert_eq!(
        lead_hr.len(),
        5,
        "DEPT_LEADER 5 条（目录 + 员工页 + employee:detail + 请假页 + leave:detail）"
    );
    assert_eq!(
        emp_hr.len(),
        5,
        "EMPLOYEE 5 条（目录 + 请假页 + leave:create + 薪资页 + salary:my）"
    );

    for (name, arr) in [("fin_hr", &fin_hr), ("lead_hr", &lead_hr), ("emp_hr", &emp_hr)] {
        assert!(
            arr.iter().any(|id| id == DIR_ID),
            "{name} 缺祖先目录 e000：非超管缺祖先闭包时侧栏整棵子树被静默丢掉"
        );
    }

    let reverse = "20000000-0000-4000-8000-00000000e045";
    assert!(
        !fin_hr.iter().any(|id| id == reverse),
        "红冲（salary:reverse）不许授给 FINANCE_MANAGER（越权：已结转金额倒回去）"
    );
    let post = "20000000-0000-4000-8000-00000000e044";
    assert!(
        fin_hr.iter().any(|id| id == post),
        "FINANCE_MANAGER 必须有过账码（设计 §3.2 ⑤「过账」）"
    );

    // 无孤儿：每个按钮码的父页面必须在同一角色的集合里
    let parent_of = |id: &str| BUTTONS.iter().find(|b| b.0 == id).map(|b| b.2);
    for (role, arr) in [("fin", &fin_hr), ("lead", &lead_hr), ("emp", &emp_hr)] {
        for got in arr.iter() {
            if let Some(parent) = parent_of(got) {
                assert!(
                    arr.iter().any(|x| x == parent),
                    "{role} 授了按钮 {got} 但没授父页面 {parent}（孤儿：授权树上勾了却进不去列表）"
                );
            }
        }
        for got in arr.iter().filter(|id| PAGES.iter().any(|p| p.0 == **id)) {
            assert!(arr.iter().any(|x| x == DIR_ID), "{role} 授了页面 {got} 但没授目录");
        }
    }
}

/// ⑧ 幂等的三处构造：菜单 `ON CONFLICT (id)`、授权 `ON CONFLICT (role_id, menu_id)`。
#[test]
fn v0387_up_is_idempotent_by_construction() {
    let up = sql_only(&read_migration(V0387_UP));
    let n_on_conflict_id = up.matches("ON CONFLICT (id) DO NOTHING").count();
    assert_eq!(
        n_on_conflict_id, 2,
        "两段菜单 INSERT（目录+页面 / 30 按钮）都必须 `ON CONFLICT (id) DO NOTHING`"
    );
    assert!(
        up.matches("ON CONFLICT (role_id, menu_id) DO NOTHING").count() >= 5,
        "5 个角色的授权都要 `ON CONFLICT (role_id, menu_id) DO NOTHING`（sys_role_menu 有 UNIQUE）"
    );
    let upper = up.to_ascii_uppercase();
    for banned in ["ON CONFLICT (ID) DO UPDATE", "TRUNCATE", "DELETE FROM SYS_ROLE_MENU"] {
        assert!(
            !upper.contains(banned),
            "V0387 up 出现 `{banned}`：重放必须 0 行插入，不许改客户已经调过的授权"
        );
    }
}

/// ⑨ 自检覆盖 9 类失败模式（少行 / 形态 / 同码 / 同 sort / title-icon / 角色 / 授权条数 / 孤儿 / 总数）。
#[test]
fn v0387_selfcheck_covers_failure_modes() {
    let up = read_migration(V0387_UP);
    let checks: [(&str, &str); 10] = [
        (
            "36 行（1 目录 + 5 页面 + 30 按钮）",
            "36 行齐（1 目录 + 5 页面 + 30 按钮）",
        ),
        ("人力资源目录形态与设计", "目录形态（顶级 / sort 77 / Layout / icon）"),
        ("个页面菜单的形态与设计", "5 个页面的形态"),
        ("个按钮权限点的形态不对", "30 个按钮的形态（name=auth_code / 父页面）"),
        ("条 auth_code 被本迁移之外的菜单占用", "权限码全库唯一（AGENTS 约定 7）"),
        ("组 sort_order 撞车", "同父 sort 唯一（AGENTS 约定 12）"),
        ("title 超 12 字或 icon/title 为空", "title 长度 + icon 非空"),
        ("HR_MANAGER 角色应有且仅有 1 个活角色", "角色形态"),
        ("条人力资源菜单授权", "每个角色的授权条数"),
        ("个按钮是孤儿", "按钮孤儿检查（父页面必须也授）"),
    ];
    for (needle, what) in checks {
        assert_has(&up, needle, &format!("自检缺「{what}」这一条"));
    }
    assert_has(
        &up,
        "缺祖先目录授权",
        "自检还必须有「页面缺祖先目录」这一条（非超管侧栏会丢子树）",
    );
    // 目录行 auth_code 是空串：同码比对必须排除空串，否则会命中所有目录（真库实测过这个假红）
    assert_has(
        &up,
        "AND m.auth_code <> ''          --",
        "权限码唯一性比对必须排除空串 auth_code（目录行是空串，否则会一口气报 14 条假冲突）",
    );
}

/// ⑩ 菜单总数上限：迁移自检 520 必须与 `scripts/verify/menu_invariants.sh` 的默认值同值。
///
/// 两个数字不同步的后果：迁移过了、`menu_invariants` ⑤ 却红（或反过来），排查要翻两处。
#[test]
fn v0387_menu_total_cap_matches_invariants_script() {
    let up = read_migration(V0387_UP);
    assert_has(&up, "n_total > 520", "迁移自检的总数上限应为 520");
    let script = std::fs::read_to_string(repo_root().join("scripts/verify/menu_invariants.sh"))
        .expect("读不到 menu_invariants.sh");
    assert_has(
        &script,
        "TOTAL_MAX=\"${MENU_TOTAL_MAX:-520}\"",
        "menu_invariants ⑤ 的默认上限必须也是 520（459 + 36 = 495）",
    );
}

/// ⑪ down：三道护栏 + 只按本迁移 menu_id 删授权（不许 role_code 通配）+ 零残留断言。
#[test]
fn v0387_down_guards_ownership_and_has_no_residue() {
    let down = read_migration(V0387_DOWN);
    let down_sql = sql_only(&down);
    for (needle, what) in [
        ("本迁移之外的子菜单", "① 目录/页面下多出别人的子菜单要拒绝"),
        ("按钮下存在", "② 按钮下挂子节点要拒绝"),
        ("保留角色", "③ 客户复用 HR_MANAGER 时只删授权、保留角色"),
        ("仍残留 % 条人力资源菜单行", "④ 零残留断言（菜单）"),
        ("仍残留 % 条人力资源菜单授权", "⑤ 零残留断言（授权）"),
    ] {
        assert_has(&down, needle, &format!("down 缺护栏：{what}"));
    }
    assert_has(
        &down_sql,
        "DELETE FROM sys_role_menu WHERE menu_id = ANY(all_ids)",
        "授权只能按本迁移的 menu_id 删",
    );
    let upper = down_sql.to_ascii_uppercase();
    // 删授权那一条语句必须只认 menu_id：DOWN 里 `role_code = 'HR_MANAGER'` 只允许出现在
    // 删角色的兜底条件里（钉住"删的是本迁移建的那个角色"），不许出现在授权删除上。
    for line in down_sql.lines() {
        let u = line.to_ascii_uppercase();
        if u.contains("SYS_ROLE_MENU") && u.contains("DELETE") {
            assert!(
                !u.contains("ROLE_CODE"),
                "down 的授权删除语句里出现了 role_code：会误伤别的迁移/客户后加给 HR_MANAGER 的授权\n{line}"
            );
        }
    }
    assert!(
        !upper.contains("ROLE_CODE IN"),
        "down 不许按 role_code 集合通配删授权：`IN` 一旦写错就是「授给所有人/删光所有人」"
    );
    assert!(
        upper.contains("WHERE ID = OWN_ROLE_ID"),
        "删角色必须钉到本迁移建的 id（角色可能是客户早就有的同码角色）"
    );

    // up/down 的菜单 id 集合必须一致（down 漏一个 = 回滚后残留，重放 up 会半套）
    let up = read_migration(V0387_UP);
    let up_ids: std::collections::BTreeSet<String> = up
        .split('\'')
        .filter(|s| s.starts_with("20000000-0000-4000-8000-00000000e0") && s.len() == 36)
        .map(str::to_string)
        .collect();
    let down_ids: std::collections::BTreeSet<String> = down
        .split('\'')
        .filter(|s| s.starts_with("20000000-0000-4000-8000-00000000e0") && s.len() == 36)
        .map(str::to_string)
        .collect();
    assert_eq!(up_ids, down_ids, "down 的 id 集合必须与 up 完全一致（36 个）");
}

/// ⑫ 引擎拆分器必须把每个 `DO $$ … $$` 留在一条语句里（否则整笔迁移回滚、服务起不来）。
#[test]
fn v0387_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    for (rel, expected_do_blocks, tail) in [
        (
            V0387_UP,
            3_usize,
            "RAISE NOTICE 'V0387：人力资源 1 目录 + 5 页面 + 30 按钮已就绪",
        ),
        (V0387_DOWN, 1_usize, "V0387.down：已删除"),
    ] {
        let sql = read_migration(rel);
        let statements = split_statements(&sql, DialectKind::Postgres)
            .unwrap_or_else(|e| panic!("引擎拆分器无法拆分 {rel}（方言感知的 $$ 处理失败）: {e}"));
        let do_blocks: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
        assert_eq!(
            do_blocks.len(),
            expected_do_blocks,
            "{rel} 的 DO 块数不对（实际 {} 条）：多出来通常是 $$ 内部的 `;` 被当成分隔符了",
            do_blocks.len()
        );
        assert!(
            statements.iter().any(|s| s.contains(tail)),
            "{rel} 拆分后必须完整包含收尾提示「{tail}」"
        );
    }
}

/// ⑬ 30 个按钮码必须登记在 `permission_audit_tests::PENDING_LANDING` 临时台账里。
///
/// 本卡是批 1（菜单先种），handler 在批 2~4 才写 ⇒ N14 会判「死码」。台账是唯一诚实出路，
/// 而且 N18 会在某张卡把源码引用写进去时强制删条目（自清理）。这里钉住"种码必登记"，
/// 免得下一个人只加菜单、L2 连库闸突然整片红。
#[test]
fn v0387_pending_landing_ledger_covers_all_codes() {
    let ledger = std::fs::read_to_string(backend_root().join("src/system/menu/permission_audit_tests.rs"))
        .expect("读不到 permission_audit_tests.rs");
    assert_has(
        &ledger,
        "const PENDING_LANDING",
        "临时台账被删了？30 个 HR 码会立刻变成 N14 死码",
    );
    let mut missing = Vec::new();
    for (_, code, _, _) in BUTTONS {
        if !ledger.contains(&format!("\"{code}\"")) {
            missing.push(code);
        }
    }
    assert!(
        missing.is_empty(),
        "以下按钮码没登记到 PENDING_LANDING（N14 会判死码）：\n{}",
        missing.join("\n")
    );
    let n_entries = ledger.matches("business:hr:").count();
    assert!(
        n_entries >= 30,
        "PENDING_LANDING 里 business:hr:* 条目只有 {n_entries} 条（应 ≥30）"
    );
}
