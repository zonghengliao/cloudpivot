//! 迁移 `V0388`（HR 脱敏登记 + 3 个 hr 开关）与 `V0389`（附件单据类型 17 → 21 + 2 条规则）的静态不变量。
//!
//! 卡：t_b85f402e（HR-05【HR】迁移 V0382 + V0383）—— 设计依据
//! `docs/architecture/ARCH-9-人力资源模块设计.md` §3.2 ⑥（2 条全局脱敏）、§3.2 ⑦（附件类型放宽 + 必传规则）、
//! §3.2 ⑩（3 个 sys_config）、§7.1（迁移清单）、§8.1（拆卡清单 HR-05）。
//!
//! # 号段
//!
//! 卡面写 V0382 + V0383，均已被成本核算实占（`V0382__stock_in_production_sourceless` /
//! `V0383` 现为 HR-01 的 `hr_tables`）；本仓迁移版本号**跨目录全局唯一**（重号 = 启动失败），
//! 两次顺延后本卡落 **V0388 + V0389**（设计 §7.1 的「实际号」列已回填）。
//!
//! # 这层测试钉死什么（都是「迁移跑过了但语义错了」的静默事故）
//!
//! 1. `sys_field_mask` 的主键写成 `gen_random_uuid()` ⇒ 重放插第二条，撞
//!    `uk_sys_field_mask_rule` 的 `NULLS NOT DISTINCT`（升级时直接失败）；
//! 2. `mask_param` 写错（如身份证写成 `'4,4'`）⇒ 脱敏口径悄悄变宽，身份证露出前 4 后 4；
//! 3. 脱敏登记到不存在的表/列 ⇒ **配了不生效**，界面上照样是明文；
//! 4. `sys_config` 用 `gen_random_uuid() + ON CONFLICT (id)` 做幂等 ⇒ 那个 ON CONFLICT 永远不命中，
//!    重放插 3 条重复键（`uk_sys_config_key` 只约束未软删行，会硬报错或按分支静默留脏）；
//! 5. 附件 CHECK 列表照抄设计稿的 20 值 ⇒ **丢掉 `RECEIPT_REFUND`**（V0368 加的），
//!    存量退款凭据附件在 `ADD CONSTRAINT` 校验时整笔迁移回滚；
//! 6. 规则 `file_role` 写设计稿的 `'OTHER'` ⇒ 它是入参老别名（落库归一成 `DEFAULT`），
//!    规则侧按字面值比较，永远匹配不到附件；
//! 7. `down` 先收约束后删规则 ⇒ 规则本身用新 doc_type，收约束必然失败；
//! 8. `down` 通配删（按 doc_type / 按表列）⇒ 连坐客户自建的规则与同名角色级脱敏；
//! 9. 引擎拆分器把 `DO $$ … $$` 里的 `;` 当分隔符 ⇒ 整笔迁移回滚、服务起不来。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 的 `migration_versions_are_unique_padded_and_have_down`
//! 统一覆盖，这里不重复，但本文件额外断言「这两个号确实没被别人占」。
//!
//! 纯文件解析 + 框架拆分器 + 一个不碰库的脱敏纯函数调用 ⇒ 每次 `cargo test` 都会跑到，不需环境变量。
//! 真库往返（存量 17 类附件 / up×2 / down×2 / 3 条护栏反证 / 空库首启 320 迁移）的结果见
//! `docs/evidence/2026-11-11-hr-05-mask-attach/` 与卡内汇报。

use std::path::PathBuf;

use cloudpivot::business::masking::masker::{MaskType, apply};

fn backend_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_migration(relative: &str) -> String {
    let path = backend_root().join("migrations").join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到迁移文件 {}: {e}", path.display()))
}

fn squash(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 去掉 `--` 行注释：注释里会**引用**错误写法当反面教材，「禁止出现」类断言必须只看真正会执行的 SQL。
fn sql_only(sql: &str) -> String {
    sql.lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_has(sql: &str, snippet: &str, why: &str) {
    assert!(
        squash(sql).contains(&squash(snippet)),
        "{why}\n期望包含：{}",
        squash(snippet)
    );
}

fn assert_absent(sql: &str, snippet: &str, why: &str) {
    assert!(
        !squash(sql).contains(&squash(snippet)),
        "{why}\n不该出现：{}",
        squash(snippet)
    );
}

fn sql_head(statement: &str) -> String {
    statement
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("--"))
        .unwrap_or("")
        .to_ascii_uppercase()
}

const V0388_UP: &str = "business/V0388__hr_mask_config.up.sql";
const V0388_DOWN: &str = "business/V0388__hr_mask_config.down.sql";
const V0389_UP: &str = "business/V0389__hr_attachment_doc_types.up.sql";
const V0389_DOWN: &str = "business/V0389__hr_attachment_doc_types.down.sql";

/// 现网（V0368 之后）的 17 个单据类型 —— 放宽必须**逐个保留**。
const LEGACY_DOC_TYPES: [&str; 17] = [
    "CONTRACT",
    "PURCHASE_ORDER",
    "STOCK_IN",
    "PAYABLE",
    "PAYMENT",
    "FIXED_ASSET",
    "EXPENSE_REPORT",
    "INVOICE",
    "RECEIPT",
    "STOCK_TAKE",
    "PURCHASE_RETURN",
    "SALES_RETURN",
    "STOCK_TRANSFER",
    "SUPPLIER",
    "CUSTOMER",
    "OPENING_BALANCE",
    "RECEIPT_REFUND",
];

/// 本卡新增的 4 个值。
const HR_DOC_TYPES: [&str; 4] = ["EMPLOYEE", "EMPLOYEE_CONTRACT", "LEAVE_REQUEST", "SALARY_SHEET"];

// ============================================================
// ① 号段与文件形状
// ============================================================

/// 两个号确实空着（V0387 之前是 HR-04，V0390 起是批 3 的三套流程）。
#[test]
fn hr05_uses_versions_0388_and_0389_and_has_down() {
    for (up, down) in [(V0388_UP, V0388_DOWN), (V0389_UP, V0389_DOWN)] {
        let up_sql = read_migration(up);
        let down_sql = read_migration(down);
        assert!(!up_sql.trim().is_empty(), "{up} 是空文件");
        assert!(!down_sql.trim().is_empty(), "{down} 是空文件");
        // up/down 内不许自带事务（迁移器自己开事务；自开 BEGIN 会让失败后无法整体回滚）
        for (name, sql) in [(up, &up_sql), (down, &down_sql)] {
            let body = sql_only(sql).to_ascii_uppercase();
            assert!(!body.contains("\nBEGIN;"), "{name} 不许写 BEGIN（迁移器自己开事务）");
            assert!(!body.contains("\nCOMMIT;"), "{name} 不许写 COMMIT（迁移器自己提交）");
        }
    }

    // 4 位补零的版本号必须与文件名一致（`V388` / `V00388` 都会被加载器当另一个号）
    for (file, ver) in [(V0388_UP, "0388"), (V0389_UP, "0389")] {
        let stem = file.rsplit('/').next().unwrap();
        assert!(
            stem.starts_with(&format!("V{ver}__")),
            "{file} 文件名与声明的版本号 V{ver} 不一致（加载器按文件名取号）"
        );
    }
}

// ============================================================
// ② V0388：脱敏登记
// ============================================================

/// 两条全局脱敏规则：取值精确、主键固定、幂等靠 `NOT EXISTS`、不碰 `sys_field_mask` 的存量行。
#[test]
fn v0388_seeds_two_field_masks_precisely_and_idempotently() {
    let up = read_migration(V0388_UP);
    let body = sql_only(&up);

    // 主键必须是固定 UUID（gen_random_uuid 会让 down 无处下手、重放插重复）
    for id in [
        "b8800000-0000-4000-8000-000000000001",
        "b8800000-0000-4000-8000-000000000002",
    ] {
        assert_has(&body, id, "脱敏规则必须用固定主键（down 按 id 精确删，重放不插重复）");
    }
    assert_absent(
        &body,
        "INSERT INTO sys_field_mask (id, table_name, column_name, mask_type, mask_param, role_id, status, reason) SELECT gen_random_uuid()",
        "脱敏规则不许用 gen_random_uuid()：重放会插第二条同 (表,列,NULL)，撞 uk_sys_field_mask_rule",
    );

    // 两条规则的取值必须与 masker 的解析口径逐字对齐
    assert_has(
        &body,
        "'biz_employee', 'id_card', 2, '6,4', NULL, 1",
        "身份证规则必须是 mask_type=2（身份证）+ mask_param='6,4'（保留前 6 后 4）+ 全局（role_id NULL）",
    );
    assert_has(
        &body,
        "'biz_employee', 'bank_account', 5, '4,4', NULL, 1",
        "工资卡规则必须是 mask_type=5（银行卡）+ mask_param='4,4' + 全局（role_id NULL）",
    );

    // 幂等：业务唯一键（表+列+全局）的 NOT EXISTS 守卫
    for col in ["id_card", "bank_account"] {
        assert_has(
            &body,
            &format!(
                "WHERE table_name = 'biz_employee' AND column_name = '{col}' AND role_id IS NULL AND deleted_at IS NULL"
            ),
            "脱敏种子必须按 (table_name, column_name, role_id IS NULL, deleted_at IS NULL) 做 NOT EXISTS —— \
             否则重放插重复、且会覆盖运维后来调过的 mask_param",
        );
    }
    assert_has(
        &body,
        "ON CONFLICT DO NOTHING",
        "固定主键 + ON CONFLICT DO NOTHING 兜底「主键被别人手搓占用」（不报错、不覆盖）",
    );

    // 不碰存量：本迁移对 sys_field_mask 只有 INSERT
    for forbidden in [
        "UPDATE sys_field_mask",
        "DELETE FROM sys_field_mask",
        "DROP TABLE",
        "ALTER TABLE",
    ] {
        assert_absent(&body, forbidden, "V0388 是纯新增迁移：不许改/删任何存量表");
    }
}

/// 三条 `sys_config` 开关：层级/类型/分类/排序精确，幂等靠 config_key 的 NOT EXISTS。
#[test]
fn v0388_seeds_three_hr_configs_with_correct_types() {
    let up = read_migration(V0388_UP);
    let body = sql_only(&up);

    for (key, value, ty, sort) in [
        ("hr.salary.iit_mode", "MANUAL", "STRING", 10),
        ("hr.salary.si_mode", "MANUAL", "STRING", 20),
        ("hr.attendance.require_leave_match", "TRUE", "BOOLEAN", 30),
    ] {
        assert_has(
            &body,
            &format!("'{key}', '{value}', '{ty}'"),
            &format!("配置 {key} 的键/值/类型必须是 {key}={value} ({ty})"),
        );
        assert_has(
            &body,
            &format!("'hr', {sort}, 1, TRUE, FALSE, FALSE, 1"),
            &format!("配置 {key} 必须是 category='hr'、sort_order={sort}、config_level=1（SMALLINT 系统级）"),
        );
        assert_has(
            &body,
            &format!("WHERE NOT EXISTS (SELECT 1 FROM sys_config WHERE config_key = '{key}' AND deleted_at IS NULL)"),
            &format!("配置 {key} 的幂等必须按 config_key 判（uk_sys_config_key 是「未软删」部分唯一索引）"),
        );
    }

    // 反面教材：`gen_random_uuid() + ON CONFLICT (id)` 等于没有幂等
    assert_absent(
        &body,
        "INSERT INTO sys_config (id, config_key, config_value, config_type, description, category, sort_order, config_level, is_modifiable, is_encrypted, is_restart, status) VALUES (gen_random_uuid()",
        "配置种子不许 VALUES (gen_random_uuid(), …) + ON CONFLICT (id)：那个冲突永远不会命中，重放插 3 条重复键",
    );
}

/// 前置护栏：表 / 列 / 唯一索引不在时必须炸，而不是「种了个永不生效的规则」。
#[test]
fn v0388_up_guards_point_at_real_tables_columns_and_indexes() {
    let up = read_migration(V0388_UP);

    for (snippet, why) in [
        (
            "to_regclass('public.sys_field_mask') IS NULL",
            "缺 sys_field_mask 表时必须 RAISE（否则种了没人读）",
        ),
        (
            "to_regclass('public.sys_config') IS NULL",
            "缺 sys_config 表时必须 RAISE",
        ),
        (
            "to_regclass('public.biz_employee') IS NULL",
            "缺 biz_employee 时必须 RAISE（HR-01 没跑就别种脱敏）",
        ),
        (
            "column_name IN ('id_card', 'bank_account')",
            "必须确认 biz_employee 上真有这两列（配置到不存在的列 = 配了不生效）",
        ),
        (
            "uk_sys_field_mask_rule",
            "必须确认脱敏唯一索引在位（它同时是幂等与并发安全的地基）",
        ),
        ("uk_sys_config_key", "必须确认配置键唯一索引在位"),
        ("RAISE EXCEPTION", "前置条件失败必须报错回滚，不许 NOTICE 后继续"),
    ] {
        assert_has(&up, snippet, why);
    }

    // 自检必须校验「登记的列真实存在」与「参数形状」
    assert_has(
        &up,
        "JOIN information_schema.columns c",
        "自检必须 join information_schema 证明登记的列真实存在",
    );
    assert_has(
        &up,
        "mask_param !~ '^[0-9]+,[0-9]+$'",
        "自检必须校验 mask_param 是「前N,后M」形状（形状错 ⇒ 整值被掩码，等于数据丢了）",
    );
    for tag in ["断言 A", "断言 B", "断言 C", "断言 D", "断言 E", "断言 F"] {
        assert_has(&up, tag, "自检必须有 6 条断言（缺一条就少一个静默失败的入口）");
    }
}

/// `down`：先护栏（有员工身份数据就拒绝）→ 按固定主键删 → 按 config_key 删 → 断言零残留。
#[test]
fn v0388_down_is_guarded_and_targeted() {
    let down = read_migration(V0388_DOWN);
    let body = sql_only(&down);

    assert_has(
        &body,
        "WHERE deleted_at IS NULL AND (id_card IS NOT NULL OR bank_account IS NOT NULL)",
        "down 必须先数「未删员工里还有身份证/工资卡」的行 —— 那是明文泄露的判据",
    );
    assert_has(
        &down,
        "V0388 回滚被拒绝",
        "有员工身份数据时必须 RAISE 拒绝回滚（不许把脱敏规则删了让明文出系统边界）",
    );
    for id in [
        "b8800000-0000-4000-8000-000000000001",
        "b8800000-0000-4000-8000-000000000002",
    ] {
        assert_has(
            &body,
            id,
            "down 必须按 up 里写死的固定主键删脱敏规则（不按表列通配，避免连坐角色级规则）",
        );
    }
    assert_absent(
        &body,
        "DELETE FROM sys_field_mask WHERE table_name = 'biz_employee'",
        "down 不许按 (table_name) 通配删：客户给同一列加的角色级规则会被连坐",
    );
    for key in [
        "hr.salary.iit_mode",
        "hr.salary.si_mode",
        "hr.attendance.require_leave_match",
    ] {
        assert_has(&body, key, "down 必须删掉三条 hr 配置（按 config_key）");
    }
    assert_has(&down, "V0388 回滚失败", "down 结尾必须断言零残留");
}

// ============================================================
// ③ V0389：附件单据类型放宽
// ============================================================

/// 21 值集合 = 现网 17 值 ∪ 新 4 值；两个 CHECK 都要放宽；幂等靠 position 守卫；不动 file_role。
#[test]
fn v0389_relaxes_both_checks_to_exactly_21_values() {
    let up = read_migration(V0389_UP);

    for kind in LEGACY_DOC_TYPES {
        assert_has(
            &up,
            &format!("'{kind}'"),
            &format!("老 doc_type {kind} 必须仍在新的 CHECK 列表里（照抄设计稿的 20 值会把它摘掉）"),
        );
    }
    for kind in HR_DOC_TYPES {
        assert_has(
            &up,
            &format!("'{kind}'"),
            &format!("新 doc_type {kind} 必须在 CHECK 列表里"),
        );
    }

    // 两个约束都必须重建（只改一处 = 上传能过但规则保存 400，或反过来）
    assert_has(
        &up,
        "ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type",
        "biz_doc_attachment 的 doc_type 约束必须 DROP IF EXISTS 后重建",
    );
    assert_has(
        &up,
        "ALTER TABLE biz_attachment_rule DROP CONSTRAINT IF EXISTS ck_biz_attachment_rule_doc_type",
        "biz_attachment_rule 的 doc_type 约束必须同步放宽（规则管理界面能选就得能存）",
    );

    // 幂等：4 个新值的 position 守卫（已含就跳过，不每次重放全表校验）
    for kind in HR_DOC_TYPES {
        assert_has(
            &up,
            &format!("position('''{kind}''' IN v_def) > 0"),
            &format!("幂等守卫必须逐个检查 {kind} 是否已在约束定义里"),
        );
    }
    assert_has(&up, "已含 4 个新值，跳过", "已含全部新值时必须跳过（NOTICE 可观察）");

    // 不许顺手动 file_role（15 值集合与 doc_type 是两件事）
    assert_absent(
        &up,
        "DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_role",
        "本迁移只放宽 doc_type，不许改 file_role 的 15 值约束",
    );

    // 自检：老值一个不少 + 存量零违规 + file_role 未被动
    assert_has(&up, "V0389 断言 A 失败", "必须断言 doc_type 约束存在");
    assert_has(&up, "V0389 断言 B 失败", "必须断言 21 个取值齐全（缺任何一个都炸）");
    assert_has(
        &up,
        "V0389 断言 C 失败",
        "必须断言解析出的取值个数 ≥21（防止约束被写成别的形态）",
    );
    assert_has(
        &up,
        "V0389 断言 E 失败",
        "必须断言 file_role 的 15 个老角色仍在（本迁移不该动它）",
    );
    assert_has(
        &up,
        "WHERE doc_type NOT IN (",
        "必须断言「存量附件的每一行 doc_type 都落在新集合内」—— 这是「放宽不影响存量」的库内判据",
    );
}

/// 两条附件规则：合同硬闸（enabled=true, CONTRACT_SCAN）、工资表建议（enabled=false, file_role NULL）。
#[test]
fn v0389_seeds_two_rules_with_design_accepted_semantics() {
    let up = read_migration(V0389_UP);
    let body = sql_only(&up);

    assert_has(
        &body,
        "'EMPLOYEE_CONTRACT', 'CONTRACT_SCAN', 'SUBMIT', 1, 20, NULL, NULL, TRUE",
        "劳动合同必须「提交时必传扫描件」（D-5）：file_role=CONTRACT_SCAN / required_at=SUBMIT / enabled=true",
    );
    assert_has(
        &body,
        "'SALARY_SHEET', NULL, 'ACTIVATE', 1, 20, NULL, NULL, FALSE",
        "工资表是「建议传、不硬拦」（设计 §2.1 / §3.2 ⑦）：file_role 必须 NULL（任意角色）、enabled 必须 false",
    );

    // 反面教材：'OTHER' 是上传接口的入参老别名（落库归一成 DEFAULT），规则侧按字面值比永远不命中
    assert_absent(
        &body,
        "'SALARY_SHEET', 'OTHER'",
        "规则 file_role 不许写 'OTHER'：它是入参老别名，落库被归一成 DEFAULT，规则侧永远匹配不到附件",
    );
    assert_has(
        &up,
        "WHERE file_role = 'OTHER'",
        "自检必须断言规则表里没有 file_role=OTHER 的脏行",
    );

    // 幂等：规则表没有唯一约束，只能 NOT EXISTS
    assert_has(
        &body,
        "WHERE doc_type = 'EMPLOYEE_CONTRACT' AND file_role = 'CONTRACT_SCAN' AND required_at = 'SUBMIT' AND deleted_at IS NULL",
        "规则种子必须按 (doc_type, file_role, required_at, 未软删) 做 NOT EXISTS（该表没有唯一约束）",
    );
    assert_has(
        &body,
        "WHERE doc_type = 'SALARY_SHEET' AND file_role IS NULL AND required_at = 'ACTIVATE' AND deleted_at IS NULL",
        "工资表规则的 NOT EXISTS 必须用 file_role IS NULL（NULL 不是 = 比较）",
    );
    // 表可能不存在（老库没跑 V0347）→ 整段必须在 DO 块里用 to_regclass 守卫（静态 SQL 引用了它）
    assert_has(
        &body,
        "IF to_regclass('public.biz_attachment_rule') IS NULL THEN",
        "老库缺 biz_attachment_rule 时必须跳过：守卫要写成 DO 块前置判断（写成 WHERE to_regclass(...) IS NOT NULL 不管用，PG 会先规划整条语句）",
    );
    assert_has(
        &up,
        "GET DIAGNOSTICS n = ROW_COUNT",
        "插入条数必须可观察（0 = 幂等跳过）",
    );
}

/// `down`：护栏（新类型附件/规则）→ 先删规则 → 再收约束 → 断言 17 值（含 RECEIPT_REFUND）。
#[test]
fn v0389_down_orders_delete_before_tighten_and_denies_rollback_with_data() {
    let down = read_migration(V0389_DOWN);
    let body = sql_only(&down);
    let flat = squash(&body);

    assert_has(
        &body,
        "FROM biz_doc_attachment WHERE doc_type IN ('EMPLOYEE', 'EMPLOYEE_CONTRACT', 'LEAVE_REQUEST', 'SALARY_SHEET')",
        "down 必须先数「还有多少条 HR 新类型附件」",
    );
    assert_has(&down, "V0389 回滚被拒绝", "有 HR 新类型附件/规则时必须 RAISE 拒绝回滚");
    assert_has(
        &down,
        "不是本迁移种的两条",
        "护栏 B 必须把「客户自建的新类型规则」与「本迁移的两条」区分开",
    );

    // 顺序：DELETE 规则 必须早于 ADD CONSTRAINT（规则本身引用了新 doc_type）
    let delete_pos = flat
        .find("DELETE FROM biz_attachment_rule")
        .expect("down 必须删本迁移的 2 条规则");
    let tighten_pos = flat
        .find("ADD CONSTRAINT ck_biz_doc_attachment_type")
        .expect("down 必须把 doc_type 约束收回 17 值");
    assert!(
        delete_pos < tighten_pos,
        "down 必须先删规则再收约束：规则行本身用新 doc_type，先收约束会校验失败"
    );

    // 收回的列表必须仍含 RECEIPT_REFUND（它是 V0368 的现网值，摘掉会让存量退款凭据附件违规）
    // 只看「ADD CONSTRAINT … CHECK (doc_type IN ( … ))」这一段，别把后面的自检断言也算进来
    let after_add = &flat[tighten_pos..];
    let list_start = after_add
        .find("CHECK (doc_type IN (")
        .expect("down 收回时必须重写 CHECK (doc_type IN (…)) 列表");
    let list_end = after_add[list_start..].find("));").expect("CHECK 列表没有收口 `));`");
    let list = &after_add[list_start..list_start + list_end];

    assert!(
        list.contains("'RECEIPT_REFUND'"),
        "down 收回的 17 值列表必须保留 RECEIPT_REFUND（V0368 加的；摘掉会让存量退款凭据附件违规）"
    );
    for kind in LEGACY_DOC_TYPES {
        assert!(list.contains(&format!("'{kind}'")), "down 收回的 17 值列表里缺 {kind}");
    }
    for kind in HR_DOC_TYPES {
        assert!(
            !list.contains(&format!("'{kind}'")),
            "down 收回的列表不许再含新值 {kind}：{list}"
        );
    }

    assert_has(
        &down,
        "V0389 回滚失败",
        "down 结尾必须断言零残留（约束 17 值、规则已删、存量 0 行违规）",
    );
}

// ============================================================
// ④ 迁移能被引擎拆分器正确处理（`$$` 内部的 `;` 不许被当分隔符）
// ============================================================

#[test]
fn hr05_migrations_split_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    let cases = [
        (
            V0388_UP,
            7usize,
            2usize,
            "V0388：1 段前置 DO + 2 条脱敏 INSERT + 3 条配置 INSERT + 1 段自检 DO",
        ),
        (
            V0389_UP,
            6usize,
            5usize,
            "V0389：1 段前置 DO + 2 段约束 DO + 1 条 COMMENT + 1 段规则 DO + 1 段自检 DO",
        ),
    ];

    for (file, want_total, want_do, why) in cases {
        let sql = read_migration(file);
        let statements =
            split_statements(&sql, DialectKind::Postgres).unwrap_or_else(|e| panic!("引擎拆分器无法拆分 {file}：{e}"));

        assert_eq!(statements.len(), want_total, "{file} 语句条数不符（{why}）");

        let do_blocks: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
        assert_eq!(
            do_blocks.len(),
            want_do,
            "{file} 的 DO 块应被拆成 {want_do} 条语句（实际 {}）：>此数说明 $$ 内部的 `;` 被当成分隔符了",
            do_blocks.len()
        );
        for block in &do_blocks {
            assert!(
                block.trim_end().ends_with("END $$"),
                "{file} 的 DO 块被截断（不以 END $$ 收口 ⇒ PG 语法错误 ⇒ 整笔迁移回滚）"
            );
        }
    }
}

// ============================================================
// ⑤ 种子参数与代码里的脱敏实现对齐（不是「看着像」）
// ============================================================

/// 库里种的 `mask_param` 必须真能被 `masking::masker` 解析出**预期形状**的掩码。
///
/// 参数写错不是「脱敏变弱」而是 fail-closed 整值抹掉（`masker::apply` 报 `MaskError`），
/// 这里把两侧钉在一起：改库里的参数就会让这条测试红。
#[test]
fn seeded_mask_params_match_masker_output() {
    let id_card = apply(MaskType::IdCard, Some("6,4"), "110101199001011234")
        .expect("身份证 + '6,4' 必须是合法参数组合（否则出参整值被抹掉）");
    // 注意：中间星号是**固定 4 个**（`nik-utils` 的 `Mask::id_card` 实现口径），不是「长度 - 前后保留」。
    assert_eq!(
        id_card, "110101****1234",
        "身份证脱敏口径：保留前 6 后 4，中间固定 4 星"
    );

    let bank = apply(MaskType::BankCard, Some("4,4"), "6222021234567890").expect("银行卡 + '4,4' 必须是合法参数组合");
    assert_eq!(bank, "6222 **** 7890", "工资卡脱敏口径：保留前 4 后 4");

    // 反面：参数写成「前/后」颠倒或非数字，必须报错（fail-closed 的前提）
    assert!(
        apply(MaskType::IdCard, Some("6;4"), "110101199001011234").is_err(),
        "非法参数必须返回 Err（不许悄悄降级成明文）"
    );
}
