//! 迁移 `V0378`（成本菜单 + 权限点 + 角色授权）/ `V0379`（号段 + 费用项目字典）/
//! `V0380`（存量批号回填）的静态不变量。
//!
//! 卡：t_191cf205（COST-B1-7【成本】迁移 V0376/0377/0379）—— 设计依据
//! `docs/architecture/ARCH-9-成本核算升级设计.md` §6.1（6 页清单 + 挂载点 + sort）、
//! §5.3（权限码清单）、§7.1（迁移清单）、§7.2（存量批号回填 SQL + 待清理清单）、拆卡清单 §8.1 的 B1-7。
//!
//! # 号段为什么是 V0378/0379/0380 而不是卡面的 V0376/0377/0379
//!
//! V0375/V0376 = B1-5，V0377 = B1-6，V0372/0373/0374 = B1-3/B1-4。nik 迁移加载器对版本号做
//! 跨目录全局唯一校验，重号会让服务启动直接失败（AGENTS 约定 6）。故 B1-7 的三件顺延，
//! 设计稿里排 V0378 的 `cost_flow_def`（B3-2）需从 V0381 起。
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 统一覆盖，这里不重复。
//!
//! # 这层测试钉死什么
//!
//! 1. ★ **菜单挂到错的父节点 / sort 撞车** ⇒ 侧栏顺序漂移、成本页面跑到别的模块下（AGENTS 约定 12）。
//! 2. ★ **title 写成长句** ⇒ 侧栏换行截断（规范 1.6.1，硬顶 12 字）。本卡 6 个 title 全是 4 字。
//! 3. ★ **icon 用了 TDesign 没有的名字** ⇒ 侧栏空图标，且不报错（约定 12 + menu_invariants ⑧）。
//! 4. ★ **权限点与 §5.3 权限码不一致**（漏一个 / 多一个 / 拼错）⇒ 后端 `#[nik::permission]`
//!    声明与菜单行的 auth_code 对不上，按钮点了 403 且授权树里查不到。
//! 5. ★ **授权漏了父页面码（孤儿按钮）** ⇒ 按钮在授权树里不可达、侧栏整棵子树不显示（V0356 口径）。
//! 6. ★ **号段用 `ON CONFLICT (id)`**（设计原稿）⇒ 客户库里已有一行 COST_OBJECT（别的 id，
//!    应用先跑过 `ensure_seq_row` 自动补的）时，插入撞 `uk_biz_seq_type` 直接迁移失败。
//!    必须是 `ON CONFLICT (seq_type) WHERE deleted_at IS NULL`。
//! 7. ★ **批号回填没用 btrim / 没用 (material_id, lot_no) 局部唯一冲突目标** ⇒ 同一批货
//!    因多一个空格变成两个批次；重复执行还会主键重复或重复插行（卡面验收要求幂等）。
//! 8. ★ **不合规批号清单写成 RAISE EXCEPTION** ⇒ 客户环境一条历史脏数据就把整个升级挡住
//!    （卡面验收明确「不阻断迁移」）。
//! 9. ★ **down 删菜单不先删 sys_role_menu** ⇒ 外键报错，回滚做不成。
//! 10. **迁移声明 `-- +no-transaction`** ⇒ 自检 RAISE 之后前面已执行的语句留在库里。
//!
//! 纯文件解析，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（菜单/授权幂等 + down 幂等 / 号段已发号被拦 / 批号回填四段场景）见
//! `docs/evidence/2026-11-10-cost-b1-7-v0378-0379-0380/`。

use std::path::PathBuf;

fn backend_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_migration(relative: &str) -> String {
    let path = backend_root().join("migrations").join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到迁移文件 {}: {e}", path.display()))
}

/// 把连续空白压成单个空格：SQL 断言只关心语句形状，不该被缩进/换行绑死。
fn squash(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材（比如 `ON CONFLICT (id)`），
/// 「禁止出现」类断言必须只看真正会执行的 SQL。
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
        "{why}\n期望包含：{}\n（SQL 全文 {} 字符，请直接看迁移文件；这里只回显期望片段）",
        squash(snippet),
        squashed.len()
    );
}

fn assert_lacks(sql: &str, snippet: &str, why: &str) {
    let squashed = squash(sql);
    assert!(
        !squashed.contains(&squash(snippet)),
        "{why}\n不该包含：{}",
        squash(snippet)
    );
}

// ============================================================================
// V0378 —— 菜单 + 权限点 + 角色授权
// ============================================================================

const V0378_UP: &str = "business/V0378__cost_menu_and_perm.up.sql";
const V0378_DOWN: &str = "business/V0378__cost_menu_and_perm.down.sql";

/// 6 个页面：(id 尾号, path, parent 尾号, sort, title, icon, auth_code)
const PAGES: [(&str, &str, &str, u32, &str, &str, &str); 6] = [
    (
        "c001",
        "/business/erp/cost-object",
        "e02",
        60,
        "成本对象",
        "layers",
        "business:erp:cost-object:list",
    ),
    (
        "c002",
        "/business/erp/cost-labor",
        "e02",
        62,
        "工时台账",
        "time",
        "business:erp:cost-labor:list",
    ),
    (
        "c003",
        "/business/erp/cost-overhead",
        "e02",
        64,
        "费用台账",
        "money",
        "business:erp:cost-overhead:list",
    ),
    (
        "c004",
        "/business/erp/cost-alloc-rule",
        "e02",
        66,
        "分摊规则",
        "share",
        "business:erp:cost-alloc-rule:list",
    ),
    (
        "c005",
        "/business/erp/cost-period",
        "e02",
        68,
        "成本核算",
        "calculator",
        "business:erp:cost-period:list",
    ),
    // 设计 §6.1 写「仓库管理 sort 80」，但 e01 下 80 已被出库单占用、90 是盘点单 ⇒ 本迁移取 95
    (
        "c006",
        "/business/erp/lot",
        "e01",
        95,
        "批次管理",
        "barcode",
        "business:erp:lot:list",
    ),
];

/// 20 个按钮：(id 尾号, 父页尾号, auth_code 动作后缀)，与 §5.3 权限码清单逐条对应。
const BUTTONS: [(&str, &str, &str); 20] = [
    ("c101", "c001", "business:erp:cost-object:create"),
    ("c102", "c001", "business:erp:cost-object:update"),
    ("c103", "c001", "business:erp:cost-object:delete"),
    ("c104", "c001", "business:erp:cost-object:close"),
    ("c111", "c002", "business:erp:cost-labor:create"),
    ("c112", "c002", "business:erp:cost-labor:update"),
    ("c113", "c002", "business:erp:cost-labor:delete"),
    ("c114", "c002", "business:erp:cost-labor:import"),
    ("c121", "c003", "business:erp:cost-overhead:create"),
    ("c122", "c003", "business:erp:cost-overhead:delete"),
    ("c131", "c004", "business:erp:cost-alloc-rule:create"),
    ("c132", "c004", "business:erp:cost-alloc-rule:update"),
    ("c133", "c004", "business:erp:cost-alloc-rule:delete"),
    ("c134", "c004", "business:erp:cost-alloc-rule:preview"),
    ("c141", "c005", "business:erp:cost-period:calc"),
    ("c142", "c005", "business:erp:cost-period:submit"),
    ("c143", "c005", "business:erp:cost-period:settle"),
    ("c144", "c005", "business:erp:cost-period:reverse"),
    ("c151", "c006", "business:erp:lot:create"),
    ("c152", "c006", "business:erp:lot:update"),
];

/// 本卡所有固定 UUID 的公共前缀（后 4 位见下表的 tail）。
const ID_PREFIX: &str = "20000000-0000-4000-8000-00000000";
/// 挂载点父菜单（系统内置目录，走的是 …0000-0000-0000-… 那一段 UID）。
const PARENT_PREFIX: &str = "20000000-0000-0000-0000-000000000";

#[test]
fn v0378_seeds_six_pages_with_design_exact_shape() {
    let idf = ID_PREFIX;
    let sql = read_migration(V0378_UP);
    for (id_tail, path, parent_tail, sort, title, icon, auth_code) in PAGES {
        assert_has(
            &sql,
            &format!("'{idf}{id_tail}'::uuid"),
            &format!("页面 {path} 必须以固定 UUID 尾号 {id_tail} 播种（幂等键）"),
        );
        assert_has(&sql, path, &format!("页面 {path} 的 path 必须与设计 §6.1 逐字一致"));
        assert_has(
            &sql,
            &format!("'{PARENT_PREFIX}{parent_tail}'::uuid"),
            &format!("页面 {path} 必须挂到父节点 …{parent_tail}（e02 财务管理 / e01 仓库管理）"),
        );
        assert_has(
            &sql,
            &format!("'{title}', TRUE, 'business'"),
            &format!("页面 {title} 必须是 menu_type=2 + is_visible=TRUE + module_code='business'"),
        );
        assert_has(
            &sql,
            &format!("'business/erp/{}/index'", path.rsplit('/').next().unwrap()),
            &format!("页面 {path} 的 component 必须与路由逐字对齐"),
        );
        assert_has(
            &sql,
            &format!("{sort},"),
            &format!("页面 {path} 的 sort_order 必须写 {sort}"),
        );
        assert_has(
            &sql,
            &format!("'{icon}'"),
            &format!("页面 {path} 的 icon 必须是 {icon}（TDesign 真实存在的名字）"),
        );
        assert_has(&sql, auth_code, &format!("页面 {path} 的 auth_code 必须写 {auth_code}"));
        assert!(
            title.chars().count() <= 12,
            "title [{title}] 超过 menu_invariants ⑥ 的 12 字硬顶"
        );
    }
}

#[test]
fn v0378_seeds_twenty_buttons_each_under_its_page() {
    let idf = ID_PREFIX;
    let sql = read_migration(V0378_UP);
    assert!(
        BUTTONS.len() == 20,
        "设计 §5.3 的按钮权限是 20 个，本表 {}",
        BUTTONS.len()
    );
    for (id_tail, parent_tail, auth_code) in BUTTONS {
        assert_has(
            &sql,
            &format!("'{idf}{id_tail}'::uuid,'{auth_code}'"),
            &format!("按钮 {auth_code} 必须以固定 UUID 尾号 {id_tail} 播种"),
        );
        assert_has(
            &sql,
            &format!("3,'{auth_code}'"),
            &format!("按钮 {auth_code} 必须是 menu_type=3 且 auth_code = name"),
        );
        assert_has(
            &sql,
            &format!("NULL,'{idf}{parent_tail}'::uuid,NULL"),
            &format!("按钮 {auth_code} 的父节点必须是页面 …{parent_tail}，且 path/component 为空"),
        );
    }
    // 按钮一律 is_visible=FALSE（不进侧栏）
    let squashed = squash(&sql_only(&sql));
    let invisible = squashed.matches("FALSE,'business')").count();
    assert_eq!(
        invisible, 20,
        "20 个按钮必须都是 is_visible=FALSE + module_code='business'，实际 {invisible} 处"
    );
}

#[test]
fn v0378_menu_seed_is_idempotent_by_fixed_uuid() {
    let sql = sql_only(&read_migration(V0378_UP));
    let squashed = squash(&sql);
    let inserts = squashed.matches("ON CONFLICT (id) DO NOTHING").count();
    assert!(
        inserts >= 2,
        "页面与按钮两批 INSERT 都必须带 ON CONFLICT (id) DO NOTHING（重放不报错、不覆盖人工改名），实际 {inserts} 处"
    );
    // 不许用 DELETE + INSERT 的方式「幂等」（会抹掉人工改名、也会破坏授权外键）
    assert_lacks(&sql, "DELETE FROM sys_menu", "种子迁移不该删菜单来达到幂等");
}

#[test]
fn v0378_guards_against_customer_owned_same_path_or_code() {
    let sql = sql_only(&read_migration(V0378_UP));
    // 客户自建同 path / 同 auth_code 的菜单若存在，必须点名并停住（否则 path 唯一性 ④ 会红在别处）
    assert_has(
        &sql,
        "path = ANY(own_paths) AND NOT (id = ANY(own_ids))",
        "必须有「6 个 path 被别的 id 占用」的前置护栏",
    );
    assert_has(
        &sql,
        "auth_code IN (SELECT auth_code FROM sys_menu WHERE id = ANY(own_ids))",
        "前置护栏必须也覆盖权限码占用（否则自建同码页面会让授权树分叉）",
    );
    assert_has(
        &sql,
        "V0378 拒绝执行",
        "护栏命中时必须以「V0378 拒绝执行」开头点名，便于运维定位",
    );
}

#[test]
fn v0378_grants_are_complete_and_orphan_free() {
    let sql = sql_only(&read_migration(V0378_UP));
    // 五个角色都要进矩阵
    for role in [
        "SUPER_ADMIN",
        "ADMIN",
        "FINANCE_MANAGER",
        "FINANCE_DIRECTOR",
        "WAREHOUSE_MANAGER",
    ] {
        assert_has(
            &sql,
            &format!("'{role}'"),
            &format!("设计 §7.1 点名角色 {role}，必须在授权矩阵里"),
        );
    }
    // 授权必须有冲突目标（sys_role_menu 的 UNIQUE(role_id, menu_id)）
    assert_has(
        &sql,
        "INSERT INTO sys_role_menu (role_id, menu_id) SELECT v_role_id, unnest(r.ids) ON CONFLICT (role_id, menu_id) DO NOTHING",
        "授权必须按 (role_id, menu_id) 幂等，且用 unnest 批量写",
    );
    // 红冲结算只给财务总监：c144 必须只在 fin_dir_ids 里，不进 fin_ids
    let fin_start = sql.find("fin_ids UUID[] :=").expect("找不到 fin_ids 声明");
    let fin_dir_start = sql.find("fin_dir_ids UUID[] :=").expect("找不到 fin_dir_ids 声明");
    let fin_ids_block = &sql[fin_start..fin_dir_start];
    assert!(
        !fin_ids_block.contains("c144"),
        "红冲结算 c144 不该出现在 fin_ids（会被授给财务经理）"
    );
    assert_has(
        &sql[fin_dir_start..],
        "'20000000-0000-4000-8000-00000000c144'",
        "红冲结算 c144 必须出现在 fin_dir_ids（只给 FINANCE_DIRECTOR）",
    );
    assert_has(&sql, "('FINANCE_MANAGER', fin_ids)", "财务经理只拿 fin_ids（不含红冲）");
    assert_has(
        &sql,
        "('FINANCE_DIRECTOR', fin_ids || fin_dir_ids)",
        "财务总监 = fin_ids + 红冲",
    );
    assert_has(
        &sql,
        "('WAREHOUSE_MANAGER', wh_ids)",
        "仓管只拿批次管理（1 页 + 2 按钮）",
    );
    // 自检必须断言「无孤儿」与「按角色条数」
    assert_has(
        &sql,
        "个成本按钮是孤儿（父页面未授，进不去列表）",
        "自检必须拦孤儿按钮（按钮授了但父页没授 ⇒ 授权树不可达、侧栏整棵子树不显示）",
    );
    assert_has(&sql, "应持有 % 条成本菜单授权", "自检必须钉住每个角色应持有的授权条数");
    // 角色缺失只跳过 + NOTICE，不阻断（空库首启 SUPER_ADMIN/ADMIN 还没种）
    assert_has(
        &sql,
        "EXCEPTION WHEN NO_DATA_FOUND",
        "角色不存在时必须跳过（不阻断空库首发）",
    );
    assert_has(
        &sql,
        "本迁移跳过它的授权",
        "自检里必须 NOTICE 说明哪个角色没种，便于运维补授",
    );
}

#[test]
fn v0378_selfcheck_covers_invariants_that_the_gate_will_check_later() {
    let sql = sql_only(&read_migration(V0378_UP));
    for (snippet, why) in [
        ("成本页面菜单应有 6 个（menu_type=2）", "6 页断言"),
        ("成本按钮权限应有 20 个（menu_type=3）", "20 按钮断言"),
        ("sort_order 撞车（侧栏顺序会漂）", "同父 sort 唯一（约定 12 + gate ⑦）"),
        ("个成本页面 title 超过 12 字", "title 长度（规范 1.6.1 + gate ⑥）"),
        ("个成本页面没设 icon", "icon 非空（gate ⑧）"),
        ("菜单总数 % 超过 menu_invariants ⑤ 的上限 460", "菜单总数上限"),
    ] {
        assert_has(&sql, snippet, &format!("缺自检：{why}"));
    }
    // 自检不能让迁移「带病通过」：必须是 EXCEPTION，不是 NOTICE
    let squashed = squash(&sql);
    assert!(
        squashed.matches("RAISE EXCEPTION 'V0378 自检失败").count() >= 6,
        "结构类自检必须用 RAISE EXCEPTION（迁移失败），不能用 NOTICE 放行"
    );
}

#[test]
fn v0378_does_not_touch_business_tables_or_disable_transactions() {
    let raw = read_migration(V0378_UP);
    assert_lacks(
        &raw,
        "+no-transaction",
        "迁移必须留在单事务里（自检 RAISE 才能整体回滚）",
    );
    let sql = sql_only(&raw);
    for forbidden in [
        "biz_account",
        "biz_lot",
        "biz_cost_object",
        "biz_seq",
        "sys_role (",
        "DELETE FROM",
    ] {
        assert_lacks(
            &sql,
            forbidden,
            &format!("本迁移只碰 sys_menu / sys_role_menu，不该出现 {forbidden}"),
        );
    }
}

#[test]
fn v0378_down_deletes_grants_before_menus_and_has_guards() {
    let raw = read_migration(V0378_DOWN);
    assert_lacks(&raw, "+no-transaction", "down 也必须单事务");
    let sql = sql_only(&raw);
    let grant_at = sql.find("DELETE FROM sys_role_menu").expect("down 必须先删授权");
    let menu_at = sql.find("DELETE FROM sys_menu").expect("down 要删菜单");
    assert!(
        grant_at < menu_at,
        "down 必须先删 sys_role_menu（有外键指向 sys_menu），否则回滚直接失败"
    );
    assert_has(
        &sql,
        "c.parent_id = ANY(own_pages) AND NOT (c.id = ANY(own_btns))",
        "down 必须有「页面下挂着本迁移之外的子菜单 ⇒ 停住」的护栏",
    );
    assert_has(
        &sql,
        "c.parent_id = ANY(own_btns)",
        "down 必须有「按钮下挂着子节点 ⇒ 停住」的护栏",
    );
    // 授权按 menu_id 删，不按 role_code/auth_code 通配
    assert_has(
        &sql,
        "DELETE FROM sys_role_menu WHERE menu_id = ANY(own_pages || own_btns)",
        "授权只按本迁移的 menu_id 删",
    );
}

// ============================================================================
// V0379 —— 号段 + 费用项目字典
// ============================================================================

const V0379_UP: &str = "business/V0379__cost_seq_and_dict.up.sql";
const V0379_DOWN: &str = "business/V0379__cost_seq_and_dict.down.sql";

#[test]
fn v0379_seq_conflict_target_is_seq_type_not_id() {
    let sql = sql_only(&read_migration(V0379_UP));
    assert_has(
        &sql,
        "ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING",
        "号段必须按 seq_type 冲突（uk_biz_seq_type）：按 id 冲突时，客户库里已有一行 \
         COST_OBJECT（应用先跑过 ensure_seq_row）会直接撞唯一索引导致升级失败",
    );
    assert_lacks(
        &sql,
        "ON CONFLICT (id)",
        "号段不许用 ON CONFLICT (id)（设计原稿的写法，客户环境会炸）",
    );
    // 与 voucher.rs::ensure_seq_row 同一口径
    assert_has(
        &sql,
        "'COST_OBJECT', '成本对象号', 'CB', 'yyyyMMdd'",
        "CB 号段：成本对象号 / 天滚",
    );
    assert_has(
        &sql,
        "'COST_PERIOD', '成本核算批次号', 'CS', 'yyyyMM'",
        "CS 号段：成本核算批次号 / 月滚",
    );
    assert_has(&sql, "current_no", "号段必须显式写 current_no = 0（第一条号是 000001）");
}

#[test]
fn v0379_dict_is_one_type_with_seven_items_and_single_default() {
    let sql = sql_only(&read_migration(V0379_UP));
    assert_has(
        &sql,
        "ON CONFLICT (dict_code) WHERE deleted_at IS NULL DO NOTHING",
        "字典类型必须按 dict_code 局部唯一索引幂等",
    );
    assert_has(
        &sql,
        "ON CONFLICT (dict_type_id, item_value) WHERE deleted_at IS NULL DO NOTHING",
        "字典项必须按 (dict_type_id, item_value) 局部唯一索引幂等",
    );
    assert_has(
        &sql,
        "'cost_expense_item', '制造费用项目'",
        "费用项目字典编码/名称必须与设计一致",
    );
    let items = [
        ("DEPRECIATION", "折旧费", 10, "TRUE"),
        ("UTILITY", "水电费", 20, "FALSE"),
        ("RENT", "租金", 30, "FALSE"),
        ("REPAIR", "修理费", 40, "FALSE"),
        ("WORKSHOP_ADMIN", "车间管理费", 50, "FALSE"),
        ("CONSUMABLE", "机物料消耗", 60, "FALSE"),
        ("OTHER", "其他", 70, "FALSE"),
    ];
    assert_eq!(items.len(), 7, "设计 §7.1：费用项目字典 7 项");
    for (value, label, sort, is_default) in items {
        assert_has(&sql, &format!("('{value}',"), &format!("字典项 {value} 必须种下"));
        assert_has(&sql, label, &format!("字典项 {value} 的标签必须是「{label}」"));
        assert_has(
            &sql,
            &format!("{sort}, {is_default})"),
            &format!("字典项 {value} 的 sort_order/is_default 必须写 {sort}/{is_default}"),
        );
    }
    // 只有 DEPRECIATION 是默认项
    assert_has(&sql, "IF n_default <> 1 THEN", "自检必须拦「默认项不唯一」");
    assert_has(
        &sql,
        "费用项目字典的默认项不是 DEPRECIATION（折旧费）",
        "自检必须钉死默认项是折旧费（前端 DictSelect 的初值）",
    );
    // 自检必须核对每项标签/顺序/状态，不是只数个数
    assert_has(
        &sql,
        "个费用项目字典项与设计不一致（标签/顺序/状态）",
        "自检必须逐项核对标签与顺序",
    );
}

#[test]
fn v0379_down_blocks_when_numbers_were_issued() {
    let raw = read_migration(V0379_DOWN);
    assert_lacks(&raw, "+no-transaction", "down 也必须单事务");
    let sql = sql_only(&raw);
    assert_has(
        &sql,
        "current_no > 0",
        "down 必须拦「号段已发过号」：删掉规则行后重建会从 000001 重来，与库里已存在的 CB/CS 单号重号",
    );
    assert_has(&sql, "V0379.down 拒绝执行", "护栏命中必须点名拒绝");
    assert_has(
        &sql,
        "i.item_value NOT IN ('DEPRECIATION','UTILITY','RENT','REPAIR','WORKSHOP_ADMIN','CONSUMABLE','OTHER')",
        "down 只许删本迁移的 7 项；客户自建项必须先人工确认",
    );
    // 号段按 (id, seq_type) 成对删
    assert_has(
        &sql,
        "AND seq_type = 'COST_OBJECT'",
        "号段应按 id + seq_type 成对删，避免误删同名主键的别的取号键",
    );
}

// ============================================================================
// V0380 —— 存量批号回填
// ============================================================================

const V0380_UP: &str = "business/V0380__cost_lot_backfill.up.sql";
const V0380_DOWN: &str = "business/V0380__cost_lot_backfill.down.sql";

#[test]
fn v0380_reads_all_three_legacy_lot_sources() {
    let sql = sql_only(&read_migration(V0380_UP));
    assert_has(
        &sql,
        "FROM biz_stock_in_item WHERE lot_no IS NOT NULL",
        "来源一：入库明细 lot_no（V0145）",
    );
    assert_has(
        &sql,
        "FROM biz_stock_out_item WHERE lot_no IS NOT NULL",
        "来源二：出库明细 lot_no（V0203）",
    );
    assert_has(
        &sql,
        "FROM biz_material_serial WHERE batch_no IS NOT NULL",
        "来源三：序列号 batch_no（V0138）",
    );
    assert_has(
        &sql,
        "AND deleted_at IS NULL",
        "序列号来源必须排除软删行（软删序列号的批号不该进批次主数据）",
    );
}

#[test]
fn v0380_backfill_is_idempotent_and_trims_lot_no() {
    let sql = sql_only(&read_migration(V0380_UP));
    assert_has(
        &sql,
        "ON CONFLICT (material_id, lot_no) WHERE deleted_at IS NULL DO NOTHING",
        "回填必须命中 uk_biz_lot_material_no 这个**局部**唯一索引（带 deleted_at 谓词），\
         否则第二次执行会插重复行；同时保证不覆盖客户维护过的活批次",
    );
    assert_has(&sql, "DISTINCT ON (material_id, lot_no)", "同一 (物料, 批号) 只建一行");
    assert_has(
        &sql,
        "btrim(lot_no)",
        "批号必须 btrim（' AB-01 ' 与 'AB-01' 是一个批次）",
    );
    assert_has(&sql, "btrim(batch_no)", "序列号来源同样要 btrim");
    assert_has(
        &sql,
        "'MANUAL', 'BACKFILL'",
        "回填行形态固定：source_type=MANUAL + source_doc_type=BACKFILL（down 的抓手）",
    );
    // 幂等自检：第二次跑完仍要每个源批号都有活行
    assert_has(
        &sql,
        "个源批号没有落进 biz_lot",
        "必须有「每个源批号都落地」的后置断言（幂等重放也成立）",
    );
}

#[test]
fn v0380_bad_lot_report_is_notice_not_blocking() {
    let sql = sql_only(&read_migration(V0380_UP));
    assert_has(
        &sql,
        "V0380 待清理批号 #%",
        "不合规批号必须逐条打到迁移日志（卡面验收）",
    );
    assert_has(&sql, "首尾含空格", "清单口径①：首尾含空格");
    assert_has(&sql, "超长", "清单口径②：超长（>64）");
    assert_has(&sql, "物料不存在或已软删", "清单口径③：物料失联");
    assert_has(&sql, "不合规批号合计 % 条", "清单必须打合计行（运维报告用）");
    assert_has(&sql, "不阻断迁移", "清单必须明确不阻断");
    // 清单的每一行都必须走 RAISE NOTICE —— 一条历史脏数据不该挡住整个升级
    for line in sql.lines().filter(|l| l.contains("待清理批号")) {
        assert!(
            line.contains("RAISE NOTICE"),
            "待清理清单只能 RAISE NOTICE，实际这行不是：{line}"
        );
    }
}

#[test]
fn v0380_prechecks_source_tables_exist() {
    let sql = sql_only(&read_migration(V0380_UP));
    assert_has(
        &sql,
        "to_regclass('public.biz_lot')",
        "必须前置检查 biz_lot 存在（缺表说明 V0370 没跑，别在这里瞎猜）",
    );
    assert_has(
        &sql,
        "table_name='biz_stock_in_item' AND column_name='lot_no'",
        "必须前置检查 biz_stock_in_item.lot_no 列",
    );
    assert_has(&sql, "V0380 拒绝执行", "前置检查失败必须点名拒绝");
}

#[test]
fn v0380_down_only_deletes_backfill_rows() {
    let raw = read_migration(V0380_DOWN);
    assert_lacks(&raw, "+no-transaction", "down 也必须单事务");
    let sql = sql_only(&raw);
    assert_has(
        &sql,
        "WHERE source_doc_type = 'BACKFILL' AND source_type = 'MANUAL'",
        "down 只删回填行（source_doc_type=BACKFILL + source_type=MANUAL），业务/客户自建批次一行不碰",
    );
    assert_has(
        &sql,
        "source_type <> 'MANUAL'",
        "已人工改过来源类型的回填行要保留（人工成果不该被回滚抹掉）",
    );
    assert_has(&sql, "RAISE WARNING", "被成本对象引用的回填批次删除前要 WARNING 点名");
    // 不许 DELETE 整表 / 删 biz_lot 之外的表
    assert_lacks(&sql, "DELETE FROM biz_lot;", "down 不许整表删");
    assert_lacks(&sql, "DELETE FROM biz_material", "down 不许碰物料主数据");
}

// ============================================================================
// 跨迁移：三个文件都不许关事务、不许改历史迁移
// ============================================================================

#[test]
fn cost_b1_7_migrations_keep_single_transaction_and_have_down() {
    for up in [V0378_UP, V0379_UP, V0380_UP] {
        let raw = read_migration(up);
        assert_lacks(&raw, "+no-transaction", &format!("{up} 必须单事务"));
        assert_has(&raw, "卡：t_191cf205", &format!("{up} 必须写明卡号（可追溯）"));
        assert_has(&raw, "ARCH-9", &format!("{up} 必须写明设计依据"));
        let down = up.replace(".up.sql", ".down.sql");
        assert!(
            backend_root().join("migrations").join(&down).exists(),
            "{up} 缺配套 down：{down}"
        );
    }
}

#[test]
fn cost_b1_7_does_not_reapply_earlier_cost_migrations() {
    // 已应用迁移不可改（AGENTS 约定 6）：本卡只允许新增 V0378/V0379/V0380
    for ver in ["V0375", "V0376", "V0377"] {
        let dir = backend_root().join("migrations").join("business");
        let files: Vec<String> = std::fs::read_dir(&dir)
            .expect("读不到 business 迁移目录")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(ver))
            // 本卡新增的三件恰好也用 V037x 之外的号，这里只校验 V0375~V0377 的文件名没被改成 cost_menu/seq/lot
            .filter(|n| {
                n.contains("cost_menu_and_perm") || n.contains("cost_seq_and_dict") || n.contains("cost_lot_backfill")
            })
            .collect();
        assert!(files.is_empty(), "已应用的 {ver} 迁移被本卡覆盖了：{files:?}");
    }
}
