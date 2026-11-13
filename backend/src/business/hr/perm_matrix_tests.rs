//! 人力资源权限矩阵的**静态契约**（HR-47）：30 个按钮码 + 5 个页面 list 码 × 6 个角色。
//!
//! # 这份测试守的是什么
//!
//! 矩阵的实际回归靠 `scripts/verify/hr/hr_permission_matrix.sh`（真 HTTP + 真库 + 6 个
//! 真实身份）。本文件补的是**离线那一半** —— 不需要起服务、不需要连库，就能在 `cargo test`
//! 里挡住四类「改错了却编译通过」的漂移：
//!
//! 1. **码表变了**：新增 / 改名 / 删掉一个 `business:hr:*` 码，而种子（`V0387`）或
//!    `ROUTE_CONTRACT` 没跟上 → 要么死码（永远 403），要么孤儿按钮；
//! 2. **授权矩阵变了**：谁该有哪个码是**设计决策**（ARCH-9 §3.2 ④⑤），不是实现细节。
//!    改这里必须同时改设计文档 —— 本文件的表就是「决策记录」，改一格都要过评审；
//! 3. **全新库的 EMPLOYEE 缺口复发**：`S__hr_employee_grants.sql` 存在的原因见该文件头
//!    （种子晚于版本化迁移执行 ⇒ V0387 的 EMPLOYEE 分支在空库上静默跳过）。本文件钉死
//!    「这个种子授的 id 集合 == V0387 想授给 EMPLOYEE 的集合」，防止两边再漂；
//! 4. **路由覆盖缺口扩大**：4 个码没有后端路由（`employee:resign` / `employee:export` /
//!    `leave:delete` / `salary-item:delete`）。缺口本身是已知的、要报出来；一旦**再多**
//!    一个码掉出路由，本文件立刻红 —— 那意味着某个界面按钮点了必然 404。
//!
//! # 为什么表是手写的而不是从 SQL 里解析出来
//!
//! 「从迁移解析」只能证明「迁移与解析器一致」，证明不了「迁移与设计一致」。
//! 这里手写的表**就是设计**（ARCH-9 §3.2 ④ 的 30 行 + ⑤ 的四档授权），
//! 迁移文件被它反向核对（每个 id / code 必须在 SQL 里出现、每个角色数组必须恰好是这些 id）。

use crate::business::hr::handler::ROUTE_CONTRACT;

/// `V0387__hr_menu_perms.up.sql` 原文（授权矩阵的唯一种子）。
const V0387: &str = include_str!("../../../migrations/business/V0387__hr_menu_perms.up.sql");

/// 全新库补 EMPLOYEE 自助授权的可重复种子（见文件头 §3）。
const SEED_EMP: &str = include_str!("../../../migrations/seed/S__hr_employee_grants.sql");

/// 菜单行：`(id, menu_type, auth_code)`；目录的 `auth_code` 为 `""`。
type Menu = (&'static str, u8, &'static str);

/// 36 行菜单（1 目录 + 5 页面 + 30 按钮），顺序与设计 §3.2 ④ 一致。
const MENUS: [Menu; 36] = [
    ("20000000-0000-4000-8000-00000000e000", 1, ""),
    ("20000000-0000-4000-8000-00000000e001", 2, "business:hr:employee:list"),
    ("20000000-0000-4000-8000-00000000e002", 2, "business:hr:attendance:list"),
    ("20000000-0000-4000-8000-00000000e003", 2, "business:hr:leave:list"),
    ("20000000-0000-4000-8000-00000000e004", 2, "business:hr:salary:list"),
    (
        "20000000-0000-4000-8000-00000000e005",
        2,
        "business:hr:salary-item:list",
    ),
    ("20000000-0000-4000-8000-00000000e011", 3, "business:hr:employee:create"),
    ("20000000-0000-4000-8000-00000000e012", 3, "business:hr:employee:update"),
    ("20000000-0000-4000-8000-00000000e013", 3, "business:hr:employee:delete"),
    ("20000000-0000-4000-8000-00000000e014", 3, "business:hr:employee:detail"),
    (
        "20000000-0000-4000-8000-00000000e015",
        3,
        "business:hr:employee:sensitive",
    ),
    ("20000000-0000-4000-8000-00000000e016", 3, "business:hr:employee:resign"),
    ("20000000-0000-4000-8000-00000000e017", 3, "business:hr:employee:import"),
    ("20000000-0000-4000-8000-00000000e018", 3, "business:hr:employee:export"),
    ("20000000-0000-4000-8000-00000000e019", 3, "business:hr:contract:manage"),
    (
        "20000000-0000-4000-8000-00000000e021",
        3,
        "business:hr:attendance:period",
    ),
    (
        "20000000-0000-4000-8000-00000000e022",
        3,
        "business:hr:attendance:record",
    ),
    (
        "20000000-0000-4000-8000-00000000e023",
        3,
        "business:hr:attendance:import",
    ),
    (
        "20000000-0000-4000-8000-00000000e024",
        3,
        "business:hr:attendance:export",
    ),
    ("20000000-0000-4000-8000-00000000e025", 3, "business:hr:attendance:lock"),
    ("20000000-0000-4000-8000-00000000e031", 3, "business:hr:leave:create"),
    ("20000000-0000-4000-8000-00000000e032", 3, "business:hr:leave:update"),
    ("20000000-0000-4000-8000-00000000e033", 3, "business:hr:leave:delete"),
    ("20000000-0000-4000-8000-00000000e034", 3, "business:hr:leave:submit"),
    ("20000000-0000-4000-8000-00000000e035", 3, "business:hr:leave:cancel"),
    ("20000000-0000-4000-8000-00000000e036", 3, "business:hr:leave:detail"),
    ("20000000-0000-4000-8000-00000000e041", 3, "business:hr:salary:detail"),
    ("20000000-0000-4000-8000-00000000e042", 3, "business:hr:salary:calc"),
    ("20000000-0000-4000-8000-00000000e043", 3, "business:hr:salary:submit"),
    ("20000000-0000-4000-8000-00000000e044", 3, "business:hr:salary:post"),
    ("20000000-0000-4000-8000-00000000e045", 3, "business:hr:salary:reverse"),
    ("20000000-0000-4000-8000-00000000e046", 3, "business:hr:salary:export"),
    ("20000000-0000-4000-8000-00000000e047", 3, "business:hr:salary:my"),
    (
        "20000000-0000-4000-8000-00000000e051",
        3,
        "business:hr:salary-item:create",
    ),
    (
        "20000000-0000-4000-8000-00000000e052",
        3,
        "business:hr:salary-item:update",
    ),
    (
        "20000000-0000-4000-8000-00000000e053",
        3,
        "business:hr:salary-item:delete",
    ),
];

/// 授权矩阵（设计 §3.2 ⑤ 的落地口径，V0387 逐码实现）：
/// `(角色码, 授权的菜单 id)` —— 目录 `e000` 是每个角色的祖先闭包锚点。
///
/// * `SUPER_ADMIN` / `HR_MANAGER`：36 行全给（超管本来短路，显式授予是为了角色树上能看清范围）
/// * `FINANCE_MANAGER`：看 + 过账 + 导出，**不含红冲**（红冲只给人事；§4.6「过账后不可改」）
/// * `DEPT_LEADER`：员工查看 + 请假详情（审批在流程中心，用 flow 码）
/// * `EMPLOYEE`：页面外壳 + 两个自助码（`salary:my` / `leave:create`），**没有** `employee:detail`
///   —— 给了就等于把「看全员档案」的码发给全员（数据范围只能兜一层，不能当权限用）
/// * `FINANCE_DIRECTOR`：**0 个** HR 码（矩阵的零授权对照列：真实角色、真没授权）
const ALL_36: [&str; 36] = [
    "20000000-0000-4000-8000-00000000e000",
    "20000000-0000-4000-8000-00000000e001",
    "20000000-0000-4000-8000-00000000e002",
    "20000000-0000-4000-8000-00000000e003",
    "20000000-0000-4000-8000-00000000e004",
    "20000000-0000-4000-8000-00000000e005",
    "20000000-0000-4000-8000-00000000e011",
    "20000000-0000-4000-8000-00000000e012",
    "20000000-0000-4000-8000-00000000e013",
    "20000000-0000-4000-8000-00000000e014",
    "20000000-0000-4000-8000-00000000e015",
    "20000000-0000-4000-8000-00000000e016",
    "20000000-0000-4000-8000-00000000e017",
    "20000000-0000-4000-8000-00000000e018",
    "20000000-0000-4000-8000-00000000e019",
    "20000000-0000-4000-8000-00000000e021",
    "20000000-0000-4000-8000-00000000e022",
    "20000000-0000-4000-8000-00000000e023",
    "20000000-0000-4000-8000-00000000e024",
    "20000000-0000-4000-8000-00000000e025",
    "20000000-0000-4000-8000-00000000e031",
    "20000000-0000-4000-8000-00000000e032",
    "20000000-0000-4000-8000-00000000e033",
    "20000000-0000-4000-8000-00000000e034",
    "20000000-0000-4000-8000-00000000e035",
    "20000000-0000-4000-8000-00000000e036",
    "20000000-0000-4000-8000-00000000e041",
    "20000000-0000-4000-8000-00000000e042",
    "20000000-0000-4000-8000-00000000e043",
    "20000000-0000-4000-8000-00000000e044",
    "20000000-0000-4000-8000-00000000e045",
    "20000000-0000-4000-8000-00000000e046",
    "20000000-0000-4000-8000-00000000e047",
    "20000000-0000-4000-8000-00000000e051",
    "20000000-0000-4000-8000-00000000e052",
    "20000000-0000-4000-8000-00000000e053",
];

/// 财务经理：目录 + 薪资管理页 + 看 / 过账 / 导出（**不含红冲**）。
const FINANCE_MANAGER_IDS: [&str; 5] = [
    "20000000-0000-4000-8000-00000000e000",
    "20000000-0000-4000-8000-00000000e004",
    "20000000-0000-4000-8000-00000000e041",
    "20000000-0000-4000-8000-00000000e044",
    "20000000-0000-4000-8000-00000000e046",
];

/// 部门主管：目录 + 员工档案页 + 员工详情 + 请假单页 + 请假详情。
const DEPT_LEADER_IDS: [&str; 5] = [
    "20000000-0000-4000-8000-00000000e000",
    "20000000-0000-4000-8000-00000000e001",
    "20000000-0000-4000-8000-00000000e014",
    "20000000-0000-4000-8000-00000000e003",
    "20000000-0000-4000-8000-00000000e036",
];

/// 员工自助：目录 + 请假单页 + 新建请假单 + 薪资管理页 + 我的薪资。
const EMPLOYEE_IDS: [&str; 5] = [
    "20000000-0000-4000-8000-00000000e000",
    "20000000-0000-4000-8000-00000000e003",
    "20000000-0000-4000-8000-00000000e031",
    "20000000-0000-4000-8000-00000000e004",
    "20000000-0000-4000-8000-00000000e047",
];

/// 4 个「种了码但没有后端路由」的权限点（已知缺口，必须显式登记）。
///
/// 它们不是 bug 的隐藏处，而是**必须继续存在**的观察项：真要接路由的那天，这份清单
/// 会先红一次，提醒改矩阵脚本与前端按钮的显隐。逐个说明：
/// * `employee:resign` —— 离职走「离职单 + 流程」，状态接口只认 `employee:update`（HR-12 决策）；
/// * `employee:export` —— 员工导出走通用 impexp（`business:impexp:*`），HR 专属路由未落地；
/// * `leave:delete` —— 请假单没有物理删除，草稿作废走 `leave:cancel`；
/// * `salary-item:delete` —— 薪资项停用走 `salary-item:update`（`is_active`），不删定义。
const ROUTELESS_CODES: [&str; 4] = [
    "business:hr:employee:resign",
    "business:hr:employee:export",
    "business:hr:leave:delete",
    "business:hr:salary-item:delete",
];

/// 取 `V0387` 里某个角色授权数组的原文块（`NAME UUID[] := ARRAY[ … ]::uuid[]`）。
fn array_block(name: &str) -> &'static str {
    let head = format!("{name} UUID[] := ARRAY[");
    let start = V0387
        .find(&head)
        .unwrap_or_else(|| panic!("V0387 里找不到授权数组 {name}（矩阵被改动了？）"));
    let rest = &V0387[start..];
    let end = rest
        .find("]::uuid[]")
        .unwrap_or_else(|| panic!("{name} 数组没有闭合 ]::uuid[]"));
    &rest[..end]
}

/// 一个角色授权数组里的 36 个菜单 id（顺序不敏感，返回集合）。
fn ids_of(name: &str) -> Vec<&'static str> {
    let block = array_block(name);
    MENUS
        .iter()
        .map(|(id, _, _)| *id)
        .filter(|id| block.contains(&format!("'{id}'")))
        .collect()
}

/// 断言两个 id 集合相等（不依赖顺序）。
fn assert_same_ids(role: &str, actual: &[&str], expected: &[&str]) {
    let mut a: Vec<&str> = actual.to_vec();
    let mut e: Vec<&str> = expected.to_vec();
    a.sort_unstable();
    e.sort_unstable();
    assert_eq!(
        a, e,
        "{role} 的授权矩阵与设计定稿不一致（左=迁移实际，右=设计/决策记录）"
    );
}

// ==================== ① 码表本身 ====================

#[test]
fn thirty_buttons_and_five_pages_are_seeded() {
    let buttons: Vec<&Menu> = MENUS.iter().filter(|(_, t, _)| *t == 3).collect();
    let pages: Vec<&Menu> = MENUS.iter().filter(|(_, t, _)| *t == 2).collect();
    assert_eq!(buttons.len(), 30, "设计 §3.2 ④ 是 30 个按钮码");
    assert_eq!(pages.len(), 5, "5 个页面各挂一个 *:list 码");
    assert_eq!(MENUS.iter().filter(|(_, t, _)| *t == 1).count(), 1, "只有一个目录");

    // 每个 id / code 都必须真的出现在迁移里（改错一格就红）
    for (id, _, code) in MENUS.iter() {
        assert!(V0387.contains(id), "V0387 里没有菜单 id {id}");
        if !code.is_empty() {
            assert!(
                V0387.contains(&format!("'{code}'")),
                "V0387 里没有权限码 {code}（菜单 id {id}）"
            );
            assert!(
                code.starts_with("business:hr:"),
                "权限码 {code} 不在 business:hr 域（AGENTS 约定 7）"
            );
        }
    }

    // 码不重复（一一对应是菜单树勾选授权的前提）
    let mut codes: Vec<&str> = MENUS.iter().map(|(_, _, c)| *c).filter(|c| !c.is_empty()).collect();
    let before = codes.len();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(before, codes.len(), "有重复的权限码");
    assert_eq!(before, 35, "35 = 30 按钮 + 5 页面 list 码");
}

// ==================== ② 授权矩阵 ====================

#[test]
fn super_admin_and_hr_manager_get_all_36() {
    assert_same_ids("SUPER_ADMIN", &ids_of("all_hr"), &ALL_36);
    // HR_MANAGER 与 SUPER_ADMIN 共用 all_hr（V0387 §4 的实现口径）
    assert_same_ids("HR_MANAGER", &ids_of("all_hr"), &ALL_36);
}

#[test]
fn finance_manager_gets_view_post_export_but_not_reverse() {
    let ids = ids_of("fin_hr");
    assert_same_ids("FINANCE_MANAGER", &ids, &FINANCE_MANAGER_IDS);
    assert!(
        !ids.contains(&"20000000-0000-4000-8000-00000000e045"),
        "财务经理不得有 salary:reverse（红冲只给人事，§4.6 过账后不可改）"
    );
    assert!(
        !ids.contains(&"20000000-0000-4000-8000-00000000e042"),
        "财务经理不得有 salary:calc（算薪是人事的活）"
    );
}

#[test]
fn dept_leader_gets_employee_and_leave_detail_only() {
    assert_same_ids("DEPT_LEADER", &ids_of("lead_hr"), &DEPT_LEADER_IDS);
}

#[test]
fn employee_gets_two_self_service_codes_only() {
    let ids = ids_of("emp_hr");
    assert_same_ids("EMPLOYEE", &ids, &EMPLOYEE_IDS);
    // 关键负向：员工**不能**有 employee:detail（否则等于把「看全员档案」的码发给全员）
    assert!(
        !ids.contains(&"20000000-0000-4000-8000-00000000e014"),
        "EMPLOYEE 不得有 business:hr:employee:detail"
    );
    // 但有 salary:my（他看自己的工资条的唯一入口）
    assert!(
        ids.contains(&"20000000-0000-4000-8000-00000000e047"),
        "EMPLOYEE 必须有 business:hr:salary:my"
    );
}

#[test]
fn finance_director_is_the_zero_grant_control_column() {
    // 矩阵的第六列必须是「真实存在但一个 HR 码都没有」的角色：它的价值在于证明
    // 「403 不是账号不存在导致的」。V0387 里任何数组都不许包含 FINANCE_DIRECTOR。
    assert!(
        !V0387.contains("'FINANCE_DIRECTOR'"),
        "V0387 不该给 FINANCE_DIRECTOR 授权（零授权对照列被破坏，矩阵失去对照组）"
    );
}

#[test]
fn every_granted_id_is_a_real_menu_row() {
    for array in ["all_hr", "fin_hr", "lead_hr", "emp_hr"] {
        for id in ids_of(array) {
            assert!(
                MENUS.iter().any(|(mid, _, _)| *mid == id),
                "{array} 里出现了不属于本模块的菜单 id {id}（会授出跨模块权限）"
            );
        }
    }
}

// ==================== ③ 全新库 EMPLOYEE 缺口（种子）====================

#[test]
fn employee_seed_grants_exactly_the_designed_ids() {
    for id in EMPLOYEE_IDS {
        assert!(
            SEED_EMP.contains(id),
            "S__hr_employee_grants.sql 没授 {id}（全新库上员工自助会 403）"
        );
    }
    // 不多授：种子里的 5 个 id 必须正好是 EMPLOYEE_IDS
    let granted: Vec<&str> = MENUS
        .iter()
        .map(|(id, _, _)| *id)
        .filter(|id| SEED_EMP.contains(&format!("'{id}'")))
        .collect();
    assert_same_ids("S__hr_employee_grants", &granted, &EMPLOYEE_IDS);
    // 幂等写法：不许裸 INSERT（重放会撞 uk_sys_role_menu）
    assert!(SEED_EMP.contains("NOT EXISTS"), "种子必须用 NOT EXISTS 做幂等");
    assert!(
        SEED_EMP.contains("RAISE EXCEPTION"),
        "种子必须带自检（缺授权就启动失败）"
    );
}

// ==================== ④ 路由覆盖 ====================

#[test]
fn route_contract_covers_every_code_except_the_known_gap() {
    let mut missing: Vec<&str> = Vec::new();
    for (_, _, code) in MENUS.iter().filter(|(_, t, _)| *t >= 2) {
        let covered = ROUTE_CONTRACT.iter().any(|r| r.permission == Some(*code));
        if !covered && !ROUTELESS_CODES.contains(code) {
            missing.push(code);
        }
    }
    assert!(
        missing.is_empty(),
        "这些权限码既没有后端路由、也不在已知缺口清单里 —— 界面上点了必然 404：{missing:?}"
    );

    // 反向：已知缺口里的码**真的**还是没路由（接上了就把它从清单里删掉）
    for code in ROUTELESS_CODES {
        assert!(
            !ROUTE_CONTRACT.iter().any(|r| r.permission == Some(code)),
            "{code} 已经有后端路由了 → 请从 ROUTELESS_CODES 删除（清单过期）"
        );
    }
}

#[test]
fn every_route_permission_is_a_seeded_hr_code() {
    let seeded: Vec<&str> = MENUS.iter().map(|(_, _, c)| *c).filter(|c| !c.is_empty()).collect();
    for route in ROUTE_CONTRACT.iter() {
        if let Some(perm) = route.permission {
            assert!(
                seeded.contains(&perm),
                "路由 {} {} 用的权限码 {perm} 不在 V0387 菜单表里 —— 授不出去，除超管外全员 403",
                route.method,
                route.path
            );
        }
    }
}

#[test]
fn self_service_routes_are_login_only_by_design() {
    // 设计 §5.2 #25 与 HR-41：考勤 / 请假的自助入口是「登录即可」，薪资的自助入口要码。
    // 这条钉死「谁挂了码、谁没挂」，防止有人顺手给 /attendance/my 加码
    // ⇒ 客户没勾那个码时全员看不到自己的考勤。
    let find = |path: &str| {
        ROUTE_CONTRACT
            .iter()
            .find(|r| r.path == path)
            .unwrap_or_else(|| panic!("契约里没有 {path}"))
    };
    assert_eq!(find("/business/hr/attendance/my").permission, None);
    assert_eq!(find("/business/hr/leave/my").permission, None);
    assert_eq!(find("/business/hr/salary/my").permission, Some("business:hr:salary:my"));
}

// ==================== ⑤ 与回归脚本同源 ====================

#[test]
fn matrix_script_reads_the_same_migration() {
    // 回归脚本（真 HTTP）通过 hr_perm_expect.py 解析 V0387 得到期望矩阵；
    // 若哪天脚本改成读别的文件，这里先红 —— 两份期望值必须同源，否则「矩阵绿了」不代表矩阵对。
    let script = include_str!("../../../../scripts/verify/hr/hr_perm_expect.py");
    assert!(
        script.contains("V0387__hr_menu_perms.up.sql"),
        "hr_perm_expect.py 必须解析 V0387（矩阵期望值的唯一真源）"
    );
    let sh = include_str!("../../../../scripts/verify/hr/hr_permission_matrix.sh");
    for role in [
        "SUPER_ADMIN",
        "HR_MANAGER",
        "FINANCE_MANAGER",
        "DEPT_LEADER",
        "EMPLOYEE",
        "FINANCE_DIRECTOR",
    ] {
        assert!(sh.contains(role), "回归脚本的 6 个角色里缺 {role}");
    }
}
