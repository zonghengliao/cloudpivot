//! 迁移 `V0386`（人力资源号段 + 字典）的静态不变量。
//!
//! 卡：t_7799d567（HR-03【HR】迁移 V0380）—— 设计依据
//! `docs/architecture/ARCH-9-人力资源模块设计.md` §2 D-15（编号口径）、
//! §3.3 ②（`biz_seq` 4 条号段）、§3.3 ③（4 类字典 26 项）、§3.5（`biz_leave_request.leave_type`
//! 与 `hr_leave_type` 一一对应）、§7.1（迁移清单）、§7.3（回滚方案）、§8.1（拆卡清单 HR-03）。
//!
//! # 号段说明
//!
//! 卡面写 `V0380`、设计 §7.1 一度写 `V0385`，两者都**已被成本核算系列实占并已提交**
//! （`V0380__cost_lot_backfill`；`V0385__cost_module_switch`）。本仓迁移加载器对版本号做
//! **跨目录全局唯一**校验（重号 = 服务启动失败），所以 HR 系列第二次整体后移 1 号，
//! 本卡落在 `V0386`（`V0385__hr_seq_dict.*` 一旦出现就是撞号）。
//!
//! # 这层测试钉死什么
//!
//! 号段/字典是**只种一次、事后只能手写补丁**的东西（`schema_migrations` 记内容指纹），
//! 写错的代价不是编译错误，而是运行期静默缺功能：
//!
//! 1. 号段少一条 ⇒ 建单/建员工时 `load_rule` 报「没有可用取号规则」；
//! 2. `EMP` 写成按日滚动 ⇒ 工号每天从 0001 重发，必然撞 `uk_biz_employee_no`；
//! 3. 字典项少一条（如漏 `COMPENSATORY` 调休）⇒ 界面选不到、请假单存不进去；
//! 4. `hr_leave_type` 与 `ck_biz_leave_type` 值集不一致 ⇒ 能选不能存 / 能存显示裸英文；
//! 5. 字典类型用 `gen_random_uuid()` ⇒ 重复执行会造第二套类型、`down` 无法精确定位；
//! 6. 迁移越界写别的表（`ALTER TABLE biz_employee` 之类）⇒ 与别的卡撞车；
//! 7. `down` 无数据护栏 ⇒ 回滚后老员工还在、号段没了，一建员工就 500；
//! 8. 引擎拆分器把 `DO $$ … $$` 内部的 `;` 当分隔符 ⇒ 整笔迁移回滚、服务起不来。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 的
//! `migration_versions_are_unique_padded_and_have_down` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（空库首启应用 / up×2 / down×2 / 4 条护栏与 5 条自检反证 / 二次启动 0 迁移）
//! 的结果写在卡内汇报里。

use std::path::PathBuf;

/// `backend/` 根目录（`backend/tests/xxx.rs` → 上跳一级）。
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

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材，
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
        "{why}\n期望包含：{}",
        squash(snippet)
    );
}

/// 取一个语句块的「首行真正的 SQL」：跳过空行与整行 `--` 注释（引擎拆分器把注释留在块内）。
fn sql_head(statement: &str) -> String {
    statement
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("--"))
        .unwrap_or("")
        .to_ascii_uppercase()
}

/// 从 SQL 里取 `INSERT INTO <t>` / `DELETE FROM <t>` / `UPDATE <t>` 的目标表名。
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

const V0386_UP: &str = "business/V0386__hr_seq_dict.up.sql";
const V0386_DOWN: &str = "business/V0386__hr_seq_dict.down.sql";
const V0383_UP: &str = "business/V0383__hr_tables.up.sql";

/// 设计 §3.3 ② 的 4 条号段：(seq_type, seq_name, prefix, date_pattern, period_key, pad_width)
const FOUR_SEQS: [(&str, &str, &str, &str, &str, i32); 4] = [
    ("EMP", "员工工号", "EMP", "none", "GLOBAL", 4),
    ("LV", "请假单号", "LV", "yyyyMMdd", "", 6),
    ("ATT", "考勤批次号", "ATT", "yyyyMM", "", 6),
    ("SAL", "薪资单号", "SAL", "yyyyMM", "", 6),
];

/// 4 类字典类型的固定 UUID（`down` 靠它精确定位，重复执行靠它保持稳定）。
const DICT_TYPE_UUIDS: [(&str, &str); 4] = [
    ("20000000-0000-4000-8000-00000000d101", "hr_position"),
    ("20000000-0000-4000-8000-00000000d102", "hr_job_level"),
    ("20000000-0000-4000-8000-00000000d103", "hr_education"),
    ("20000000-0000-4000-8000-00000000d104", "hr_leave_type"),
];

/// 设计 §3.3 ③ 的 26 项：(dict_code, item_value, item_label, sort_order)
const DICT_ITEMS: [(&str, &str, &str, i32); 26] = [
    // 岗位 5
    ("hr_position", "WORKER", "车间工人", 10),
    ("hr_position", "CLERK", "文员", 20),
    ("hr_position", "ENGINEER", "工程师", 30),
    ("hr_position", "SALES", "销售", 40),
    ("hr_position", "MANAGER", "经理", 50),
    // 职级 5
    ("hr_job_level", "P1", "P1", 10),
    ("hr_job_level", "P5", "P5", 20),
    ("hr_job_level", "P8", "P8", 30),
    ("hr_job_level", "M1", "M1", 40),
    ("hr_job_level", "M4", "M4", 50),
    // 学历 7
    ("hr_education", "PRIMARY", "小学", 10),
    ("hr_education", "JUNIOR", "初中", 20),
    ("hr_education", "SENIOR", "高中", 30),
    ("hr_education", "COLLEGE", "大专", 40),
    ("hr_education", "BACHELOR", "本科", 50),
    ("hr_education", "MASTER", "硕士", 60),
    ("hr_education", "DOCTOR", "博士", 70),
    // 请假类型 9（值集 = ck_biz_leave_type）
    ("hr_leave_type", "ANNUAL", "年假", 10),
    ("hr_leave_type", "SICK", "病假", 20),
    ("hr_leave_type", "PERSONAL", "事假", 30),
    ("hr_leave_type", "MARRIAGE", "婚假", 40),
    ("hr_leave_type", "MATERNITY", "产假", 50),
    ("hr_leave_type", "PATERNITY", "陪产假", 60),
    ("hr_leave_type", "BEREAVEMENT", "丧假", 70),
    ("hr_leave_type", "COMPENSATORY", "调休", 80),
    ("hr_leave_type", "OTHER", "其他", 90),
];

/// ① up/down 必须成对存在且非空（down 不能是空壳：回滚能力是迁移契约的一部分）。
#[test]
fn v0386_up_and_down_exist_and_are_paired() {
    let up = read_migration(V0386_UP);
    let down = read_migration(V0386_DOWN);
    assert!(
        up.len() > 4000,
        "V0386 up 太短（{} 字节）：号段 + 字典 + 自检写完了吗",
        up.len()
    );
    assert!(
        down.len() > 2000,
        "V0386 down 太短（{} 字节）：护栏 + 逆向删除 + 反检写完了吗",
        down.len()
    );
    assert_has(&up, "INSERT INTO sys_dict_type", "up 必须种字典类型");
    assert_has(&down, "DELETE FROM sys_dict_item", "down 必须删字典项");
    assert_has(&down, "DELETE FROM biz_seq", "down 必须删号段");
}

/// ② up 只写 3 张表，且不做任何 DDL。
///
/// 本卡是**纯数据种子**：任何 `CREATE`/`ALTER`/`DROP`/`TRUNCATE` 都意味着越界
/// （表结构属 HR-01，其它模块的校验约束属别的卡），一旦撞车会连锁破坏。
#[test]
fn v0386_up_writes_only_three_tables_and_no_ddl() {
    let up = sql_only(&read_migration(V0386_UP));
    let targets = dml_targets(&up);
    assert!(
        !targets.is_empty(),
        "V0386 up 里没解析到任何 DML —— 解析器或文件结构变了"
    );
    for t in &targets {
        assert!(
            matches!(t.as_str(), "biz_seq" | "sys_dict_type" | "sys_dict_item"),
            "V0386 up 写了不该写的表 `{t}`（本卡只种 biz_seq / sys_dict_type / sys_dict_item）"
        );
    }
    // 只读查询（WHERE NOT EXISTS 里引用部门/请假表不算 DML），但 DDL 一律禁止
    let upper = up.to_ascii_uppercase();
    for banned in ["CREATE TABLE", "ALTER TABLE", "DROP TABLE", "TRUNCATE"] {
        assert!(
            !upper.contains(banned),
            "V0386 up 出现 DDL `{banned}`：号段/字典卡只该种数据，表结构属 HR-01"
        );
    }
}

/// ③ 4 条号段逐条与设计 §3.3 ② 一致（含 `EMP` 不按日期滚动这一条最关键的语义）。
#[test]
fn v0386_seeds_four_seq_types_per_design() {
    let up = squash(&read_migration(V0386_UP));
    for (seq_type, name, prefix, date_pattern, period_key, pad_width) in FOUR_SEQS {
        // up 里号段走 DO 块动态拼串，字面量写成 `''EMP''` ⇒ 断言串也要带双层引号
        let needle =
            format!("(''{seq_type}'', ''{name}'', ''{prefix}'', ''{date_pattern}'', ''{period_key}'', {pad_width},");
        assert!(
            up.contains(&needle),
            "号段 {seq_type} 的定义与设计不符（期望 {needle}）。\n\
             EMP 必须 date_pattern='none' + period_key='GLOBAL'（工号是主数据码，永不重置）；\
             LV/ATT/SAL 的日期段决定单号形态"
        );
    }
    // 反向：不许出现按日/按月滚动的 EMP
    assert!(
        !up.contains("(''EMP'', ''员工工号'', ''EMP'', ''yyyyMMdd''"),
        "EMP 不能被种成按日滚动（每天从 0001 重发会撞 uk_biz_employee_no）"
    );
}

/// ④ 字典类型必须用固定 UUID（不能用 `gen_random_uuid()`），否则重复执行会造第二套类型、
/// `down` 也定位不到自己种的那一套。
#[test]
fn v0386_dict_types_use_fixed_uuids() {
    let up = squash(&read_migration(V0386_UP));
    for (uuid, code) in DICT_TYPE_UUIDS {
        let needle = format!("('{uuid}'::uuid, '{code}',");
        assert!(
            up.contains(&needle),
            "字典类型 {code} 没用固定 UUID（期望 {needle}）：down 将无法精确定位、幂等性也靠不住"
        );
    }
    assert!(
        !up.contains("gen_random_uuid(), 'hr_"),
        "字典类型不能用 gen_random_uuid()：重复执行会种出第二套 hr_* 类型，界面出现两组同名下拉"
    );
    assert_has(
        &up,
        "ON CONFLICT (id) DO NOTHING",
        "字典类型插入必须带 ON CONFLICT (id) DO NOTHING（固定 UUID 的幂等写法）",
    );
}

/// ⑤ 26 项字典值逐条比对（少一条就是「界面选不到某个值」的静默缺功能）。
#[test]
fn v0386_seeds_exactly_26_dict_items() {
    assert_eq!(DICT_ITEMS.len(), 26, "常量表本身就写错了：26 项 = 5 + 5 + 7 + 9");
    // 分组计数与设计 §3.3 ③ 一致
    for (dict, expect) in [
        ("hr_position", 5),
        ("hr_job_level", 5),
        ("hr_education", 7),
        ("hr_leave_type", 9),
    ] {
        let n = DICT_ITEMS.iter().filter(|(d, ..)| *d == dict).count();
        assert_eq!(n, expect, "{dict} 的项数应为 {expect}，常量表里是 {n}");
    }

    let up = squash(&read_migration(V0386_UP));
    for (dict, value, label, sort) in DICT_ITEMS {
        let needle = format!("('{dict}', '{value}', '{label}', {sort})");
        assert!(
            up.contains(&needle),
            "字典项缺失或写错：期望 `{needle}`（漏项 ⇒ 界面选不到该值，数据存不进去）"
        );
    }
}

/// ⑥ `hr_leave_type` 的 9 个值必须与 `ck_biz_leave_type`（V0383 真实 DDL）**完全一致**。
///
/// 这是 §3.3 ③「与 `biz_leave_request.leave_type` 的 CHECK 一一对应」的静态侧证据；
/// 运行期侧由迁移末尾的自检（双向 `EXCEPT`）兜住。
#[test]
fn v0386_leave_type_values_match_ddl_check() {
    let ddl = read_migration(V0383_UP);
    let line = ddl
        .lines()
        .find(|l| l.contains("ck_biz_leave_type") && l.contains("CHECK"))
        .unwrap_or_else(|| panic!("V0383 里找不到 ck_biz_leave_type 的 CHECK 定义（HR-01 的 DDL 改过？）"));

    let inside = line
        .split("IN (")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .unwrap_or_else(|| panic!("解析 ck_biz_leave_type 的取值列表失败：{line}"));
    let mut ddl_values: Vec<String> = inside
        .split(',')
        .map(|s| s.trim().trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    ddl_values.sort();

    let mut dict_values: Vec<String> = DICT_ITEMS
        .iter()
        .filter(|(d, ..)| *d == "hr_leave_type")
        .map(|(_, v, ..)| (*v).to_string())
        .collect();
    dict_values.sort();

    assert_eq!(
        dict_values, ddl_values,
        "字典 hr_leave_type 与 ck_biz_leave_type 值集不一致：\n  字典={dict_values:?}\n  CHECK={ddl_values:?}\n\
         字典有而 CHECK 没有 ⇒ 界面能选、落库被拒；CHECK 有而字典没有 ⇒ 数据能存、界面显示裸英文"
    );
}

/// ⑦ 幂等构造：号段与字典项必须用 `NOT EXISTS` 守卫，不许出现裸 `INSERT … VALUES` 重复种。
#[test]
fn v0386_up_is_idempotent_by_construction() {
    let up = read_migration(V0386_UP);
    let flat = squash(&sql_only(&up));
    // 号段走 DO 块动态拼串，`WHERE NOT EXISTS` 被拆成两段字符串常量 ⇒ 只断言两截都在。
    assert_has(
        &flat,
        "' WHERE NOT EXISTS (SELECT 1 FROM biz_seq s '",
        "号段插入必须带 NOT EXISTS(seq_type 且未删) 守卫：否则第二遍执行撞 uk_biz_seq_type",
    );
    assert_has(
        &flat,
        "' WHERE s.seq_type = v.t AND s.deleted_at IS NULL)'",
        "号段 NOT EXISTS 守卫的条件必须落在 seq_type + deleted_at 上（按类型判重、软删可复用）",
    );
    assert_has(
        &flat,
        "WHERE NOT EXISTS (SELECT 1 FROM sys_dict_item i WHERE i.dict_type_id = t.id AND i.item_value = v.val AND i.deleted_at IS NULL)",
        "字典项插入必须带 NOT EXISTS(dict_type_id, item_value 且未删) 守卫",
    );
}

/// ⑧ down 必须有数据护栏（有业务数据就拒绝回滚），且认证号段归属本迁移。
///
/// 反例（§7.3 要挡的死路）：回滚删了 `EMP` 号段，老员工还在 —— 系统看着正常，
/// 一建员工就报「没有可用取号规则」。
#[test]
fn v0386_down_guards_business_data_and_ownership() {
    let down = read_migration(V0386_DOWN);
    for table in [
        "biz_employee",
        "biz_leave_request",
        "biz_attendance_period",
        "biz_salary_sheet",
    ] {
        assert_has(
            &down,
            &format!("count(*) AS n FROM {table} WHERE deleted_at IS NULL"),
            &format!("down 缺少 {table} 的数据护栏（有未删行必须拒绝回滚，否则号段删了还在建单）"),
        );
    }
    assert_has(
        &down,
        "RAISE EXCEPTION 'V0386 down 拒绝执行",
        "down 的护栏必须真的 RAISE",
    );
    assert_has(
        &down,
        "coalesce(s.remark, '') LIKE 'ARCH-9-2 HR 号段（V0386）%'",
        "down 删号段必须按 remark 归属判定：客户自建的同 seq_type 号段不许被误删",
    );
    assert!(
        down.contains("NOT LIKE 'ARCH-9-2 HR 号段（V0386）%'"),
        "down 必须把「归属非本迁移」的号段点名保留（NOTICE），而不是静默跳过"
    );
    // 反向自检：删完必须断言本迁移种的东西零残留
    assert_has(&down, "本迁移种的 HR 号段仍有", "down 末尾必须有零残留反向自检");
}

/// ⑨ up 末尾的自检必须覆盖 6 类静默错误（号段在位 / 号段语义 / 位数 / 字典类 / 字典项数 /
/// 值与 CHECK 对齐），且每一类都是 `RAISE EXCEPTION`（不是 NOTICE —— 失败必须整笔回滚）。
#[test]
fn v0386_selfcheck_covers_six_failure_modes() {
    let up = read_migration(V0386_UP);
    let flat = squash(&up);
    let raises = flat.matches("RAISE EXCEPTION 'V0386 自检失败").count();
    assert!(
        raises >= 6,
        "V0386 up 自检只有 {raises} 处 RAISE EXCEPTION，应至少 6 处（号段在位 / 号段语义 / 位数 / \
         字典类 / 字典项数 / 与 CHECK 对齐）"
    );
    for snippet in [
        "HR 号段只有 % 条",
        "HR 号段配置与设计不符",
        "HR 号段补零位数不对",
        "HR 字典类型只有 % 类",
        "字典项数不对",
        "ck_biz_leave_type 允许的值字典里没有",
    ] {
        assert_has(&up, snippet, "自检缺少该类断言");
    }
    assert_has(
        &up,
        "pg_get_constraintdef(c.oid), '''([A-Z_]+)''', 'g'",
        "自检必须从 pg_constraint 读 ck_biz_leave_type 的真实值集（不能硬编码第二份清单）",
    );
}

/// ⑩ 引擎拆分器必须把每个 `DO $$ … $$` 留在一条语句里。
///
/// 用框架自己的 `split_statements`（引擎执行迁移走的就是它）验证：
/// `$$` 内部的 `;` 若被当成语句分隔符，PG 会报语法错误，整个迁移事务回滚 ⇒ 服务起不来。
#[test]
fn v0386_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    for (rel, expected_do_blocks, tail) in [
        (V0386_UP, 2_usize, "RAISE NOTICE 'V0386 完成"),
        (V0386_DOWN, 4_usize, "RAISE NOTICE 'V0386 down 完成"),
    ] {
        let sql = read_migration(rel);
        let statements = split_statements(&sql, DialectKind::Postgres)
            .unwrap_or_else(|e| panic!("引擎拆分器无法拆分 {rel}（方言感知的 $$ 处理失败）: {e}"));

        let do_blocks: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
        assert_eq!(
            do_blocks.len(),
            expected_do_blocks,
            "{rel} 的 DO 块数不对（实际 {} 条）：> 预期说明 $$ 内部的 `;` 被当成分隔符了",
            do_blocks.len()
        );
        assert!(
            statements.iter().any(|s| s.contains(tail)),
            "{rel} 拆分后的最后一个 DO 块必须完整包含完成提示「{tail}」"
        );
        for block in &do_blocks {
            let upper = block.to_ascii_uppercase();
            let begin = upper.matches("BEGIN").count();
            let end = upper.matches("END IF").count() + upper.matches("END;").count();
            assert!(
                begin >= 1 && end >= 1,
                "{rel} 的某个 DO 块缺少 BEGIN/END 配平：\n{}",
                &block[..block.len().min(200)]
            );
        }
    }
}

/// ⑪ 自检**只读**：不许为了自检插探针行（迁移器重复执行时探针会残留或撞唯一键）。
#[test]
fn v0386_selfcheck_is_read_only() {
    let up = read_migration(V0386_UP);
    let sql = sql_only(&up);
    assert!(
        !sql.contains("probe") && !sql.contains("PROBE"),
        "V0386 允许写探针？本卡的自检必须是纯读（不插行），否则第二遍执行会残留"
    );
    // 整份 up 里不该出现 DELETE / UPDATE（本卡只新增，不改既有行）
    let upper = squash(&sql).to_ascii_uppercase();
    for banned in ["DELETE FROM", "UPDATE BIZ_SEQ", "UPDATE SYS_DICT"] {
        assert!(
            !upper.contains(banned),
            "V0386 up 出现 `{banned}`：本卡只新增号段/字典，不改任何既有行（改了客户配置就丢）"
        );
    }
}
