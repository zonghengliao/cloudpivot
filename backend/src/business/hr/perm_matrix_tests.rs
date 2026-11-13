//! 人力资源权限矩阵的**静态契约**（HR-47 + 解耦 t_0af7ec46）：
//! 30 个业务按钮码 + 5 个页面显形码（`*:page`）+ 5 个全量列表码（`*:list`）× 6 个角色。
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

/// 41 行菜单 = 1 目录 + 5 页面（`*:page`）+ 30 业务按钮 + 5 全量列表按钮（`*:list`）。
///
/// # 与 `V0387` 原表的差别（卡 t_0af7ec46 / 迁移 `V0401`）
///
/// `V0387` 当初把 5 个页面行直接挂在 `*:list` 上（页面码兼作全量接口码）。`V0401` 把
/// 页面行原地改名成 `*:page`（id 不动 ⇒ 授权不丢），并把 `*:list` 挪到 5 个新按钮行
/// （`…e061`~`…e065`，`menu_type=3`）。所以本表描述的是**解耦后**的最终形态；
/// `V0387` 原文仍由下方 `array_block` / `ids_of` 逐条核对（历史迁移不可改）。
const MENUS: [Menu; 41] = [
    ("20000000-0000-4000-8000-00000000e000", 1, ""),
    ("20000000-0000-4000-8000-00000000e001", 2, "business:hr:employee:page"),
    ("20000000-0000-4000-8000-00000000e002", 2, "business:hr:attendance:page"),
    ("20000000-0000-4000-8000-00000000e003", 2, "business:hr:leave:page"),
    ("20000000-0000-4000-8000-00000000e004", 2, "business:hr:salary:page"),
    (
        "20000000-0000-4000-8000-00000000e005",
        2,
        "business:hr:salary-item:page",
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
    // ---- 全量列表码（`V0401` 从页面行挪到按钮行；id 是解耦迁移新种的） ----
    ("20000000-0000-4000-8000-00000000e061", 3, "business:hr:employee:list"),
    ("20000000-0000-4000-8000-00000000e062", 3, "business:hr:attendance:list"),
    ("20000000-0000-4000-8000-00000000e063", 3, "business:hr:leave:list"),
    ("20000000-0000-4000-8000-00000000e064", 3, "business:hr:salary:list"),
    (
        "20000000-0000-4000-8000-00000000e065",
        3,
        "business:hr:salary-item:list",
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
///
/// 解耦后「全量」= `V0387` 的 36 行 ∪ `V0401` 补的 5 个 `:list` 按钮行。
const ALL_41: [&str; 41] = [
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
    // V0401 补的 5 个全量列表按钮
    "20000000-0000-4000-8000-00000000e061",
    "20000000-0000-4000-8000-00000000e062",
    "20000000-0000-4000-8000-00000000e063",
    "20000000-0000-4000-8000-00000000e064",
    "20000000-0000-4000-8000-00000000e065",
];

/// 财务经理：目录 + 薪资管理页 + 看 / 过账 / 导出（**不含红冲**）+ `V0401` 补的全量列表码。
const FINANCE_MANAGER_IDS: [&str; 6] = [
    "20000000-0000-4000-8000-00000000e000",
    "20000000-0000-4000-8000-00000000e004",
    "20000000-0000-4000-8000-00000000e041",
    "20000000-0000-4000-8000-00000000e044",
    "20000000-0000-4000-8000-00000000e046",
    // V0401：它本来就靠页面行的 salary:list 看全公司工资单，解耦后显式补上
    "20000000-0000-4000-8000-00000000e064",
];

/// 部门主管：目录 + 员工档案页 + 员工详情 + 请假单页 + 请假详情 + `V0401` 补的员工全量列表。
///
/// ⚠ **不补** `leave:list`（`…e063`）：它的 `data_scope=1`（全部），补了就等于让部门主管
/// 看全公司请假单 —— 比解耦前（handler 把它收窄成「只看本人」）更宽。要放开得先给
/// `biz_leave_request` 补数据范围裁剪，那是另一张卡的事（见 ADR §决策 2 的逐码理由）。
const DEPT_LEADER_IDS: [&str; 6] = [
    "20000000-0000-4000-8000-00000000e000",
    "20000000-0000-4000-8000-00000000e001",
    "20000000-0000-4000-8000-00000000e014",
    "20000000-0000-4000-8000-00000000e003",
    "20000000-0000-4000-8000-00000000e036",
    // V0401：它本来就有「员工档案页 + employee:detail」且 data_scope=1，补上保持原可见面
    "20000000-0000-4000-8000-00000000e061",
];

/// 员工自助：目录 + 请假单页 + 新建请假单 + 薪资管理页 + 我的薪资。
const EMPLOYEE_IDS: [&str; 5] = [
    "20000000-0000-4000-8000-00000000e000",
    "20000000-0000-4000-8000-00000000e003",
    "20000000-0000-4000-8000-00000000e031",
    "20000000-0000-4000-8000-00000000e004",
    "20000000-0000-4000-8000-00000000e047",
];

/// 「种了码但**没有**后端路由」的权限点（已知缺口，必须显式登记）。
///
/// 它们不是 bug 的隐藏处，而是**必须继续存在**的观察项：真要接路由的那天，这份清单
/// 会先红一次，提醒改矩阵脚本与前端按钮的显隐。逐个说明：
/// * `employee:resign` —— 离职走「离职单 + 流程」，状态接口只认 `employee:update`（HR-12 决策）；
/// * `leave:delete` —— 请假单没有物理删除，草稿作废走 `leave:cancel`；
/// * `salary-item:delete` —— 薪资项停用走 `salary-item:update`（`is_active`），不删定义。
const ROUTELESS_CODES: [&str; 3] = [
    "business:hr:employee:resign",
    "business:hr:leave:delete",
    "business:hr:salary-item:delete",
];

/// 契约里有、但**handler 还没落地**的码（`ROUTE_CONTRACT` 的 `card = "TBD"`）。
///
/// `business:hr:employee:export` 属这一类：设计 §5.2 #12 给了接口行，§8.1 的卡清单里
/// 没排卡（员工导出实际走通用 impexp），所以 `#[nik::permission]` 一行都没有 ——
/// 前端若照契约画出「导出」按钮，点了必然 404。要跟 `ROUTELESS_CODES` 分开记：
/// 前者是「设计了但没卡」，后者是「有卡但决定不接路由」。
const CONTRACT_ONLY_CODES: [&str; 1] = ["business:hr:employee:export"];

/// **页面显形码**（`V0401` 起）：只挂 `sys_menu.menu_type=2`，后端零引用、无路由。
///
/// 它们不是「缺口」，是设计：页面显隐与全量接口码解耦后，页面码只给前端路由
/// （`router/index.ts` 的 `meta.permission`）。登记在这里是为了让
/// 「路由覆盖」用例放过它们 —— 同时 `permission_audit_tests::ROUTE_LEVEL_ONLY`
/// 会反过来盯住「后端源码里不许出现」。
const PAGE_ONLY_CODES: [&str; 5] = [
    "business:hr:employee:page",
    "business:hr:attendance:page",
    "business:hr:leave:page",
    "business:hr:salary:page",
    "business:hr:salary-item:page",
];

/// 解耦迁移原文（`V0401`：页面行改名 `*:page` + `*:list` 挪到按钮行 + 逐角色补授权）。
const V0401: &str = include_str!("../../../migrations/business/V0401__hr_page_code_split.up.sql");

/// 从 `V0401` §3 的授权表里取某个角色被补的 `auth_code`（`(role_code, auth_code) VALUES` 矩阵）。
///
/// 解析失败（表被改成别的写法）一律 panic —— 这份表是「谁该看全量」的决策记录，
/// 悄悄解析不到会让断言变成空集合而假绿。
fn v0401_codes_of(role: &str) -> Vec<&'static str> {
    let start = V0401
        .find("FROM (VALUES")
        .unwrap_or_else(|| panic!("V0401 里找不到 §3 的授权 VALUES 矩阵"));
    let rest = &V0401[start..];
    let end = rest
        .find(") AS g(role_code, auth_code)")
        .unwrap_or_else(|| panic!("V0401 的授权矩阵没有 AS g(role_code, auth_code) 收尾"));
    let body = &rest[..end];
    let mut codes = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        let Some(head) = line.strip_prefix("('") else {
            continue;
        };
        // ('ROLE', 'code') 或 ('ROLE','code'),
        let Some(close_role) = head.find("',") else {
            continue;
        };
        if &head[..close_role] != role {
            continue;
        }
        let tail = head[close_role + 2..].trim_start();
        let Some(tail) = tail.strip_prefix('\'') else {
            continue;
        };
        if let Some(close_code) = tail.find('\'') {
            codes.push(&tail[..close_code]);
        }
    }
    codes
}

/// 解耦后某角色的完整授权 id 集合 = `V0387` 的数组 ∪ `V0401` 给它补的 `:list` 行。
fn granted_ids(role: &str, base_array: &str) -> Vec<&'static str> {
    let mut ids = ids_of(base_array);
    for code in v0401_codes_of(role) {
        let id = MENUS
            .iter()
            .find(|(_, _, c)| *c == code)
            .map(|(id, _, _)| *id)
            .unwrap_or_else(|| panic!("V0401 授权表里的 {code} 不在菜单表里"));
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

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
fn thirty_buttons_five_list_buttons_and_five_pages_are_seeded() {
    let buttons: Vec<&Menu> = MENUS.iter().filter(|(_, t, _)| *t == 3).collect();
    let pages: Vec<&Menu> = MENUS.iter().filter(|(_, t, _)| *t == 2).collect();
    assert_eq!(
        buttons.len(),
        35,
        "解耦后按钮 = 设计 §3.2 ④ 的 30 个业务码 + V0401 的 5 个全量列表码"
    );
    assert_eq!(pages.len(), 5, "5 个页面各挂一个 `*:page` 显形码");
    assert_eq!(MENUS.iter().filter(|(_, t, _)| *t == 1).count(), 1, "只有一个目录");

    // 每个 id / code 都必须真的出现在迁移里（V0387 种菜单、V0401 改名 + 种 5 个按钮）
    for (id, _, code) in MENUS.iter() {
        assert!(V0387.contains(id) || V0401.contains(id), "两份迁移里都没有菜单 id {id}");
        if !code.is_empty() {
            assert!(
                V0387.contains(&format!("'{code}'")) || V0401.contains(&format!("'{code}'")),
                "两份迁移里都没有权限码 {code}（菜单 id {id}）"
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
    assert_eq!(before, 40, "40 = 30 业务按钮 + 5 全量列表 + 5 页面显形码");
}

/// 页面码与全量列表码**一一配对且不重叠**（解耦的核心结构）。
#[test]
fn page_codes_and_list_codes_are_a_bijection() {
    for code in PAGE_ONLY_CODES {
        let resource = code
            .strip_prefix("business:hr:")
            .and_then(|rest| rest.strip_suffix(":page"))
            .unwrap_or_else(|| panic!("页面码 {code} 不是 business:hr:<资源>:page 形态"));
        let list_code = format!("business:hr:{resource}:list");
        let list_rows = MENUS.iter().filter(|(_, t, c)| *t == 3 && *c == list_code).count();
        assert_eq!(
            list_rows, 1,
            "{code} 必须恰好配一个同资源的 `:list` 按钮行，实际 {list_rows}"
        );
        assert!(
            !MENUS.iter().any(|(_, t, c)| *t == 2 && *c == list_code),
            "{list_code} 不许再挂在页面行上（那正是解耦要拆掉的耦合）"
        );
    }
    // 反向：每个 `:list` 都必须是按钮行，页面行只挂 `:page`
    for (id, menu_type, code) in MENUS.iter() {
        if code.ends_with(":list") {
            assert_eq!(*menu_type, 3, "{id} 挂着 {code} 却不是按钮行");
        }
        if *menu_type == 2 {
            assert!(
                code.ends_with(":page"),
                "页面行 {id} 的码是 {code} —— 页面行只许挂 `*:page` 显形码"
            );
        }
    }
}

// ==================== ② 授权矩阵 ====================

#[test]
fn super_admin_and_hr_manager_get_all_41() {
    assert_same_ids("SUPER_ADMIN", &granted_ids("SUPER_ADMIN", "all_hr"), &ALL_41);
    // HR_MANAGER 与 SUPER_ADMIN 共用 all_hr；V0401 §3 对两者都补了 5 个全量列表码
    assert_same_ids("HR_MANAGER", &granted_ids("HR_MANAGER", "all_hr"), &ALL_41);
}

#[test]
fn finance_manager_gets_view_post_export_but_not_reverse() {
    let ids = granted_ids("FINANCE_MANAGER", "fin_hr");
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
    let ids = granted_ids("DEPT_LEADER", "lead_hr");
    assert_same_ids("DEPT_LEADER", &ids, &DEPT_LEADER_IDS);
    // 解耦时**刻意不补** leave:list：它的 data_scope=1，补了就能看全公司请假单
    assert!(
        !ids.contains(&"20000000-0000-4000-8000-00000000e063"),
        "部门主管不得有 business:hr:leave:list（data_scope=1 ⇒ 会看到全公司请假单）"
    );
}

#[test]
fn employee_gets_two_self_service_codes_only() {
    let ids = granted_ids("EMPLOYEE", "emp_hr");
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

/// **本卡的核心验收**：员工自助持有页面码，但**一个全量列表码都不许有**。
#[test]
fn employee_holds_no_full_list_code() {
    let ids = granted_ids("EMPLOYEE", "emp_hr");
    for (id, _, code) in MENUS.iter() {
        if code.ends_with(":list") {
            assert!(
                !ids.contains(id),
                "EMPLOYEE 不该持有全量列表码 {code} —— 页面码已经独立（V0401），\
                 再持有它等于「授页面 ⇒ 授全量」的老耦合复发"
            );
        }
    }
    assert!(
        ids.contains(&"20000000-0000-4000-8000-00000000e003"),
        "EMPLOYEE 必须仍持有请假单页（否则「我的请假」入口整片消失）"
    );
    assert!(
        ids.contains(&"20000000-0000-4000-8000-00000000e004"),
        "EMPLOYEE 必须仍持有薪资管理页（否则「我的薪资」入口整片消失）"
    );
}

#[test]
fn finance_director_is_the_zero_grant_control_column() {
    // 矩阵的第六列必须是「真实存在但一个 HR 码都没有」的角色：它的价值在于证明
    // 「403 不是账号不存在导致的」。两份迁移里任何授权段都不许包含 FINANCE_DIRECTOR。
    assert!(
        !V0387.contains("'FINANCE_DIRECTOR'"),
        "V0387 不该给 FINANCE_DIRECTOR 授权（零授权对照列被破坏，矩阵失去对照组）"
    );
    assert!(
        !V0401.contains("'FINANCE_DIRECTOR'"),
        "V0401 不该给 FINANCE_DIRECTOR 补全量列表码（对照列被破坏）"
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
        // 页面显形码按设计没有后端路由（只给前端路由）
        if PAGE_ONLY_CODES.contains(code) {
            continue;
        }
        let covered = ROUTE_CONTRACT.iter().any(|r| r.permission == Some(*code));
        if !covered && !ROUTELESS_CODES.contains(code) && !CONTRACT_ONLY_CODES.contains(code) {
            missing.push(code);
        }
    }
    assert!(
        missing.is_empty(),
        "这些权限码既没有后端路由、也不在已知缺口清单里 —— 界面上点了必然 404：{missing:?}"
    );

    // 正向：页面显形码**一个都不许**出现在路由契约里（出现 = 又拿它当接口码了）
    for code in PAGE_ONLY_CODES {
        assert!(
            !ROUTE_CONTRACT.iter().any(|r| r.permission == Some(code)),
            "路由契约里出现了页面显形码 {code} —— 接口只许认 `*:list` / `*:detail` 这类业务码"
        );
    }

    // 反向一：`ROUTELESS_CODES` 里的码**真的**连契约行都没有（哪天接了路由要摘掉）
    for code in ROUTELESS_CODES {
        assert!(
            !ROUTE_CONTRACT.iter().any(|r| r.permission == Some(code)),
            "{code} 已经有契约行了 → 请从 ROUTELESS_CODES 删除（清单过期）"
        );
    }
    // 反向二：`CONTRACT_ONLY_CODES` 里的码**在**契约里（否则该挪到 ROUTELESS_CODES）
    for code in CONTRACT_ONLY_CODES {
        assert!(
            ROUTE_CONTRACT.iter().any(|r| r.permission == Some(code)),
            "{code} 不在契约里了 → 请从 CONTRACT_ONLY_CODES 删除（清单过期）"
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
    // 回归脚本（真 HTTP）通过 hr_perm_expect.py 解析 V0387 + V0401 得到期望矩阵；
    // 若哪天脚本改成读别的文件，这里先红 —— 两份期望值必须同源，否则「矩阵绿了」不代表矩阵对。
    let script = include_str!("../../../../scripts/verify/hr/hr_perm_expect.py");
    assert!(
        script.contains("V0387__hr_menu_perms.up.sql"),
        "hr_perm_expect.py 必须解析 V0387（菜单/授权的历史真源）"
    );
    assert!(
        script.contains("V0401__hr_page_code_split.up.sql"),
        "hr_perm_expect.py 必须叠加 V0401（页面码解耦的覆盖层），否则期望矩阵还停在解耦前"
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
