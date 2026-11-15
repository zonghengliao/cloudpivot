//! 迁移 `V0383`（人力资源底座：10 张 HR 表）的静态不变量。
//!
//! 卡：t_65a3dc14（HR-01【HR】迁移 V0378）—— 设计依据
//! `docs/architecture/ARCH-9-人力资源模块设计.md` §3.1（10 表 DDL，真库跑通）、
//! §7.1（迁移清单）、§7.3（回滚方案）、§8.1（拆卡清单 HR-01）。
//!
//! # 号段说明
//!
//! 卡面与设计 §7.1 写的是 `V0378`，但 `V0378~V0382` 已被成本核算卡片实占并已提交
//! （`V0378__cost_menu_and_perm` … `V0382__stock_in_production_sourceless`）。
//! 本仓迁移加载器对版本号做**跨目录全局唯一**校验（重号 = 服务启动失败），
//! 所以 HR 全系列整体后移 5 号，本卡落在 `V0383`，并在迁移头注释里写明映射表。
//!
//! # 这层测试钉死什么
//!
//! 迁移是一次性、不可回改的东西（`schema_migrations` 记内容指纹），错误只会在下次
//! 部署时炸。这里把「写对了」的形状在 `cargo test` 阶段固化，覆盖会**静默算错账**的类型：
//!
//! 1. 唯一键漏掉 `WHERE deleted_at IS NULL` ⇒ 软删后同工号 / 同期间 / 同薪资单再也建不出来；
//! 2. 金额列写窄（NUMERIC(18,2)）⇒ 第 3/4 位小数被四舍五入，工资表与凭证对不平；
//! 3. CHECK 少一条 ⇒ 一条脏数据入口（离职无日期、薪资单三条恒等式不成立…）；
//! 4. 本卡越界改存量表（`ALTER TABLE biz_voucher` 之类）⇒ 与别的卡撞车；
//! 5. 自检断言被删 ⇒ 迁移「跑过了但没验过」，等于没有自检；
//! 6. 引擎拆分器把 `DO $$ … $$` 内部的 `;` 当语句分隔符 ⇒ 整个迁移事务回滚，服务起不来。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 的
//! `migration_versions_are_unique_padded_and_have_down` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up×2 / down×2 / 两条护栏反证）的结果写在卡内汇报里。

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

const V0383_UP: &str = "business/V0383__hr_tables.up.sql";
const V0383_DOWN: &str = "business/V0383__hr_tables.down.sql";

/// 本卡只建这 10 张表，其余 HR 对象（科目/号段/字典/菜单/脱敏/配置/附件/流程/薪资项种子）
/// 各有自己的卡。
const OWN_TABLES: [&str; 10] = [
    "biz_employee",
    "biz_employee_contract",
    "biz_employee_salary",
    "biz_attendance_period",
    "biz_attendance_record",
    "biz_leave_request",
    "biz_salary_item",
    "biz_salary_sheet",
    "biz_salary_detail",
    "biz_salary_detail_item",
];

/// 12 条唯一索引必须带软删谓词（表有 `deleted_at`）。
const SOFT_DELETE_UNIQUE_INDEXES: [&str; 12] = [
    "uk_biz_employee_no",
    "uk_biz_employee_user",
    "uk_biz_employee_idcard",
    "uk_biz_emp_contract_no",
    "uk_biz_emp_salary",
    "uk_biz_att_period",
    "uk_biz_att_rec",
    "uk_biz_leave_no",
    "uk_biz_sal_item_code",
    "uk_biz_sal_item_sort",
    "uk_biz_sal_sheet_no",
    "uk_biz_sal_sheet_period",
];

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

/// ① up / down 必须成对存在，且号段落在 V0383（不是卡面写的 V0378 —— 那个号已被成本占）。
#[test]
fn v0383_up_and_down_exist_and_use_free_version() {
    for rel in [V0383_UP, V0383_DOWN] {
        let path = backend_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
    // 反面：不许有人把文件改回撞号的 V0378~V0382
    for taken in [
        "business/V0378__hr_tables.up.sql",
        "business/V0379__hr_tables.up.sql",
        "business/V0380__hr_tables.up.sql",
        "business/V0381__hr_tables.up.sql",
        "business/V0382__hr_tables.up.sql",
    ] {
        let path = backend_root().join("migrations").join(taken);
        assert!(
            !path.exists(),
            "{} 存在：V0378~V0382 已被成本核算迁移实占，撞号会让服务启动失败",
            path.display()
        );
    }
}

/// ② 只建 10 张表，且建表语句必须幂等（`IF NOT EXISTS`）。
#[test]
fn v0383_creates_exactly_the_ten_owned_tables_idempotently() {
    let sql = sql_only(&read_migration(V0383_UP));
    let squashed = squash(&sql);

    for table in OWN_TABLES {
        assert_has(
            &sql,
            &format!("CREATE TABLE IF NOT EXISTS {table} ("),
            &format!("{table} 必须以 CREATE TABLE IF NOT EXISTS 建（幂等）"),
        );
    }

    let create_table_count = squashed.matches("CREATE TABLE").count();
    assert_eq!(
        create_table_count, 10,
        "本迁移必须恰好建 §3.1 的 10 张表（实际 {create_table_count} 处 CREATE TABLE）"
    );
}

/// ③ 不许碰任何存量表结构（本卡是「只加新表」，与成本/采购等卡零交集）。
#[test]
fn v0383_never_alters_existing_tables() {
    let code = sql_only(&read_migration(V0383_UP));
    for forbidden in ["ALTER TABLE", "TRUNCATE", "DROP TABLE", "DROP INDEX"] {
        assert!(
            !squash(&code).contains(forbidden),
            "V0383 只允许「建新表 + 建索引 + 注释」，不允许出现 {forbidden}：\
             存量表改造属别的卡，改同一张表会撞车"
        );
    }
}

/// ④ up 里所有 DML 只碰自己这 10 张表（自检探针行），不往别人的表里写数据。
#[test]
fn v0383_dml_only_touches_own_tables() {
    let code = sql_only(&read_migration(V0383_UP));
    let targets = dml_targets(&code);
    assert!(
        !targets.is_empty(),
        "自检里的探针行应该用 INSERT/DELETE 写自己的表；一个 DML 都没有说明自检被删光了"
    );
    for table in targets {
        assert!(
            OWN_TABLES.contains(&table.as_str()),
            "V0383 写了 {table} —— 本卡只允许对自己的 10 张表做探针行；往存量表里插数据会污染真库"
        );
    }
}

/// ⑤ 唯一键必须带软删条件（本卡验收项之一）。
///
/// 漏谓词不会报错，只会让「删除后重建同工号 / 同期间」变成永不可能（只能改库）。
#[test]
fn v0383_unique_keys_are_soft_delete_aware() {
    let sql = read_migration(V0383_UP);
    let squashed = squash(&sql);

    for index in SOFT_DELETE_UNIQUE_INDEXES {
        let mut rest = squashed.as_str();
        let mut found = false;
        while let Some(pos) = rest.find(&format!("CREATE UNIQUE INDEX IF NOT EXISTS {index}")) {
            let stmt_end = rest[pos..].find(';').map(|e| pos + e).unwrap_or(rest.len());
            let stmt = &rest[pos..stmt_end];
            assert!(
                stmt.contains("WHERE") && stmt.contains("deleted_at IS NULL"),
                "{index} 缺软删条件：{stmt}"
            );
            assert!(
                stmt.contains("IF NOT EXISTS"),
                "{index} 缺 IF NOT EXISTS（重复执行会 relation already exists）"
            );
            found = true;
            rest = &rest[stmt_end..];
        }
        assert!(found, "{index} 不存在 —— 10 张表的唯一键必须与设计 §3.1 一致");
    }
}

/// ⑥ 明细两表是级联口径，**不许有** `deleted_at`。
///
/// 设计 §3.1 ⑨ 定稿：明细随单头级联，留痕靠 `detail_item.expr_snapshot` + `biz_status_log`。
/// 一旦给它们加了软删列，唯一键就得改成部分索引，两套口径会打架（同单同员工能留两条活行）。
#[test]
fn v0383_detail_tables_have_no_soft_delete_column() {
    let sql = read_migration(V0383_UP);
    let squashed = squash(&sql);

    for table in ["biz_salary_detail", "biz_salary_detail_item"] {
        let start = squashed
            .find(&format!("CREATE TABLE IF NOT EXISTS {table} ("))
            .unwrap_or_else(|| panic!("{table} 建表语句找不到"));
        let body = &squashed[start..];
        let end = body.find(");").expect("建表语句未收口");
        assert!(
            !body[..end].contains("deleted_at"),
            "{table} 不该有 deleted_at：明细随单头 ON DELETE CASCADE，靠 expr_snapshot 留痕"
        );
    }

    // 两条不带软删谓词的全量唯一键（列里没有 deleted_at，谓词写不进去）
    assert_has(
        &sql,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sal_det ON biz_salary_detail (sheet_id, employee_id);",
        "明细一人一行：uniq(sheet_id, employee_id) 是薪资三方对账的前提",
    );
    assert_has(
        &sql,
        "CONSTRAINT uk_biz_sal_ditem UNIQUE (detail_id, item_code)",
        "一项一码：同明细同薪资项两行会让「公式解释层」自己算不平",
    );
}

/// ⑦ 49 条 CHECK 约束一条都不能少（漏一条 = 一条脏数据入口）。
#[test]
fn v0383_keeps_all_check_constraints() {
    let sql = read_migration(V0383_UP);

    // 表 → 该表的 CHECK 约束名（与设计 §3.1 逐条对应）
    let groups: [(&str, usize, &[&str]); 10] = [
        (
            "biz_employee",
            10,
            &[
                "ck_biz_employee_gender",
                "ck_biz_employee_idtype",
                "ck_biz_employee_emptype",
                "ck_biz_employee_status",
                "ck_biz_employee_leavetype",
                "ck_biz_employee_leave",
                "ck_biz_employee_leavetype2",
                "ck_biz_employee_dates",
                "ck_biz_employee_probation",
                "ck_biz_employee_idcard_len",
            ],
        ),
        (
            "biz_employee_contract",
            7,
            &[
                "ck_biz_emp_contract_type",
                "ck_biz_emp_contract_status",
                "ck_biz_emp_contract_range",
                "ck_biz_emp_contract_sign",
                "ck_biz_emp_contract_prob",
                "ck_biz_emp_contract_term",
                "ck_biz_emp_contract_unfixed",
            ],
        ),
        (
            "biz_employee_salary",
            2,
            &["ck_biz_emp_salary_amount", "ck_biz_emp_salary_range"],
        ),
        (
            "biz_attendance_period",
            4,
            &[
                "ck_biz_att_period_code",
                "ck_biz_att_period_range",
                "ck_biz_att_period_status",
                "ck_biz_att_period_counts",
            ],
        ),
        (
            "biz_attendance_record",
            2,
            &["ck_biz_att_rec_nonneg", "ck_biz_att_rec_source"],
        ),
        (
            "biz_leave_request",
            6,
            &[
                "ck_biz_leave_type",
                "ck_biz_leave_status",
                "ck_biz_leave_range",
                "ck_biz_leave_days",
                "ck_biz_leave_paid",
                "ck_biz_leave_cancel",
            ],
        ),
        (
            "biz_salary_item",
            6,
            &[
                "ck_biz_sal_item_type",
                "ck_biz_sal_item_calc",
                "ck_biz_sal_item_source",
                "ck_biz_sal_item_formula",
                "ck_biz_sal_item_negflag",
                "ck_biz_sal_item_account",
            ],
        ),
        (
            "biz_salary_sheet",
            6,
            &[
                "ck_biz_sal_sheet_code",
                "ck_biz_sal_sheet_range",
                "ck_biz_sal_sheet_status",
                "ck_biz_sal_sheet_idem",
                "ck_biz_sal_sheet_gross",
                "ck_biz_sal_sheet_nonneg",
            ],
        ),
        (
            "biz_salary_detail",
            4,
            &[
                "ck_biz_sal_det_gross",
                "ck_biz_sal_det_withold",
                "ck_biz_sal_det_net",
                "ck_biz_sal_det_nonneg",
            ],
        ),
        (
            "biz_salary_detail_item",
            2,
            &["ck_biz_sal_ditem_type", "ck_biz_sal_ditem_calc"],
        ),
    ];

    let mut total = 0;
    for (table, expected, names) in groups {
        assert_eq!(
            names.len(),
            expected,
            "{table} 的 CHECK 名单条数与声明不一致（测试自身写错）"
        );
        total += expected;
        for name in names {
            assert_has(
                &sql,
                &format!("CONSTRAINT {name} CHECK ("),
                &format!("{table}.{name} 缺失（漏一条就是一条脏数据入口）"),
            );
        }
    }
    assert_eq!(total, 49, "设计 §3.1 一共 49 条 CHECK");
}

/// ⑧ 值域 / 语义类约束必须与设计逐字一致（这几条错了不会报错，只会放脏数据进来）。
#[test]
fn v0383_critical_constraint_semantics_match_design() {
    let sql = read_migration(V0383_UP);

    // 离职语义双向：有 leave_date ⇔ status = RESIGNED
    assert_has(
        &sql,
        "ck_biz_employee_leave CHECK ((status = 'RESIGNED') = (leave_date IS NOT NULL))",
        "离职语义必须双向：否则「标了离职但没日期」「有离职日但还在职」两种脏数据都能写进来",
    );
    // 工资卡号/身份证长度、性别值域
    assert_has(
        &sql,
        "ck_biz_employee_idcard_len CHECK (id_card IS NULL OR length(id_card) BETWEEN 6 AND 18)",
        "身份证长度约束缺失会让 3 位脏数据混进查重键",
    );
    // 月度正则必须收窄到 01~12（宽正则的经典坑：2026-13 能进库）
    assert_has(
        &sql,
        "ck_biz_att_period_code CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$')",
        "考勤期间正则必须 (0[1-9]|1[0-2])，否则 2026-13 能写入",
    );
    assert_has(
        &sql,
        "ck_biz_sal_sheet_code CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$')",
        "薪资期间正则同上",
    );
    // 薪资单三条恒等式（任何绕过引擎的写路径都写不进「算错」的单）
    assert_has(
        &sql,
        "ck_biz_sal_sheet_idem CHECK (gross_total = net_total + withhold_total)",
        "应发 = 实发 + 代扣：这是工资凭证勾稽的库级兜底",
    );
    assert_has(
        &sql,
        "ck_biz_sal_sheet_gross CHECK (gross_total = earning_total - pre_deduct_total)",
        "应发 = 收入合计 - 税前扣款合计",
    );
    assert_has(
        &sql,
        "ck_biz_sal_det_net CHECK (gross_amount = net_amount + withhold_amount)",
        "明细行也必须三方闭合（sheet 与 detail 对账靠它）",
    );
    // 请假：CANCELLED 必须有原因；时长必须 > 0
    assert_has(
        &sql,
        "ck_biz_leave_cancel CHECK (status <> 'CANCELLED' OR cancel_reason IS NOT NULL)",
        "作废必须写原因（审计口径）",
    );
    assert_has(
        &sql,
        "ck_biz_leave_range CHECK (end_time > start_time)",
        "请假区间必须正向",
    );
    // 薪资项：FORMULA 必须有公式；科目按 item_type 分别必填
    assert_has(
        &sql,
        "ck_biz_sal_item_formula CHECK (calc_type <> 'FORMULA' OR formula IS NOT NULL)",
        "公式项没有公式 = 试算到这一项就崩",
    );
    assert_has(
        &sql,
        "ck_biz_sal_item_account CHECK (",
        "科目按 item_type 分别必填（WITHHOLD 只需贷方 / SUMMARY 不要科目）",
    );
    // 无固定期限合同不许有 end_date（劳动合同法口径）
    assert_has(
        &sql,
        "ck_biz_emp_contract_unfixed CHECK (contract_type <> 'UNFIXED_TERM' OR end_date IS NULL)",
        "无固定期限合同带终止日 = 台账自相矛盾",
    );
}

/// ⑨ 关键列的类型 / 精度必须与设计 §3.1 一致。
///
/// 金额列写窄（NUMERIC(18,2) 而不是 (18,4)）不会报错，只会在第 3/4 位小数上算错，
/// 而工资凭证按「Σ 薪资项」入账 ⇒ 试算平衡表差几分钱，查一天。
#[test]
fn v0383_column_types_match_design() {
    let sql = read_migration(V0383_UP);
    let expected = [
        // biz_employee
        "emp_no VARCHAR(32) NOT NULL",
        "name VARCHAR(50) NOT NULL",
        "dept_id UUID NOT NULL",
        "hire_date DATE NOT NULL",
        "status VARCHAR(16) NOT NULL DEFAULT 'PROBATION'",
        "user_id UUID,",
        // biz_employee_contract
        "contract_no VARCHAR(32) NOT NULL",
        "probation_months SMALLINT NOT NULL DEFAULT 0",
        "renewed_from_id UUID REFERENCES biz_employee_contract(id) ON DELETE SET NULL",
        // biz_employee_salary
        "amount NUMERIC(18,4) NOT NULL",
        "effective_from DATE NOT NULL",
        // biz_attendance_period
        "period_code VARCHAR(7) NOT NULL",
        "period_start DATE NOT NULL",
        // biz_attendance_record
        "employee_no VARCHAR(32) NOT NULL",
        "employee_name VARCHAR(50) NOT NULL",
        // biz_leave_request
        "leave_no VARCHAR(32) NOT NULL",
        "duration_days NUMERIC(6,2) NOT NULL",
        "paid SMALLINT NOT NULL DEFAULT 1",
        // biz_salary_item
        "item_code VARCHAR(32) NOT NULL",
        "sort_order INT NOT NULL DEFAULT 10",
        "debit_account_code VARCHAR(30)",
        // biz_salary_sheet
        "sheet_no VARCHAR(32) NOT NULL",
        "earning_total NUMERIC(20,4) NOT NULL DEFAULT 0",
        "gross_total NUMERIC(20,4) NOT NULL DEFAULT 0",
        "net_total NUMERIC(20,4) NOT NULL DEFAULT 0",
        "accrual_voucher_id UUID,",
        "payment_voucher_id UUID,",
        // biz_salary_detail（无 deleted_at，见测试 ⑥）
        "employee_no VARCHAR(32) NOT NULL",
        "dept_name VARCHAR(100)",
        "user_id UUID,",
        "net_amount NUMERIC(18,4) NOT NULL DEFAULT 0",
        // biz_salary_detail_item
        "expr_snapshot VARCHAR(500)",
        "amount NUMERIC(18,4) NOT NULL DEFAULT 0",
    ];
    for snippet in expected {
        assert_has(&sql, snippet, &format!("列定义与设计不符（漏了或写窄了）：{snippet}"));
    }
}

/// ⑩ 自检写进迁移：DO 块存在、每条失败路径都 `RAISE EXCEPTION`、探针按 probe 键清理。
#[test]
fn v0383_self_check_raises_exceptions_and_cleans_probes() {
    let sql = read_migration(V0383_UP);
    let blocks: Vec<&str> = sql.split("DO $$").skip(1).collect();
    assert_eq!(
        blocks.len(),
        1,
        "V0383 必须自带**一个**收口自检 DO 块（实际 {} 个）",
        blocks.len()
    );
    assert!(
        blocks[0].contains("RAISE EXCEPTION"),
        "自检块没有任何 RAISE EXCEPTION —— 自检失败也不会拦住迁移"
    );

    // 关键断言逐条点名（删掉任意一条都要红）
    for msg in [
        "未建出来",
        "缺软删条件",
        "不该有 deleted_at",
        "的 CHECK 约束应为",
        "10 张表的外键应为 12 条",
        "不是 ON DELETE CASCADE",
        "的金额精度应为 NUMERIC",
        "ck_biz_employee_leave 未生效",
        "uk_biz_employee_no 未生效",
        "uk_biz_employee_user 未生效",
        "ck_biz_emp_contract_unfixed 未生效",
        "ck_biz_att_period_code 未生效",
        "uk_biz_att_rec 未生效",
        "ck_biz_leave_range 未生效",
        "ck_biz_sal_item_formula / ck_biz_sal_item_account 未生效",
        "ck_biz_sal_sheet_idem 未生效",
        "uk_biz_sal_sheet_period 未生效",
        "uk_biz_sal_ditem 未生效",
        "级联未生效",
        "探针行未清理干净",
    ] {
        assert!(sql.contains(msg), "自检断言里必须有一条覆盖「{msg}」（当前找不到）");
    }

    // 探针行必须按 probe 键删干净（迁移重复执行不能往真库里攒垃圾行）
    assert_has(
        &sql,
        "DELETE FROM biz_employee WHERE org_id = v_org;",
        "自检探针行必须按 probe org 删干净（重复执行零残留）",
    );
}

/// ⑪ down 必须幂等 + 有数据护栏（有业务数据就拒绝回滚）+ 结尾断言零残留。
#[test]
fn v0383_down_is_idempotent_and_guarded() {
    let down = read_migration(V0383_DOWN);

    for table in OWN_TABLES {
        assert_has(
            &down,
            &format!("DROP TABLE IF EXISTS {table};"),
            &format!("down 必须用 DROP TABLE IF EXISTS 删 {table}（重复执行不报错）"),
        );
    }

    // 护栏：未删员工 / 已过账薪资单 / 锁定考勤批次（设计 §7.3 定稿口径）
    for (snippet, why) in [
        (
            "biz_employee 还有 % 名未删员工",
            "有人事主数据时删表 = 工资/考勤/合同的历史再也解释不清「这行是谁」",
        ),
        (
            "存在已过账/已红冲的薪资单",
            "凭证已进账，拆表会让 5602.*/2211.01 分录变成孤儿",
        ),
        (
            "已 LOCKED（薪资试算输入）",
            "锁定批次是薪资试算的输入，删了当月无法复算",
        ),
    ] {
        assert_has(&down, snippet, why);
    }

    assert!(
        squash(&down).contains("RAISE EXCEPTION"),
        "down 必须有自检：删完要断言 10 张表确实没了"
    );
    assert_has(
        &down,
        "仍有表未删掉",
        "down 结尾必须断言零残留（否则迁移器记一个「成功」但库里还留着半张表）",
    );
}

/// ⑫ 引擎拆分器必须把 `DO $$ … $$` 留在一条语句里，且语句条数与预期一致。
///
/// 用框架自己的 `split_statements`（引擎执行迁移走的就是它）验证：
/// `$$` 内部的 `;` 若被当成语句分隔符，PG 会报语法错误，整个迁移事务回滚 ⇒ 服务起不来。
#[test]
fn v0383_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    let sql = read_migration(V0383_UP);
    let statements =
        split_statements(&sql, DialectKind::Postgres).expect("引擎拆分器无法拆分 V0383（方言感知的 $$ 处理失败）");

    let do_blocks: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
    assert_eq!(
        do_blocks.len(),
        1,
        "自检 DO 块必须被拆成**一条**语句（实际 {} 条）：>1 说明 $$ 内部的 `;` 被当成分隔符了",
        do_blocks.len()
    );
    let block = do_blocks[0];
    assert!(
        block.contains("RAISE EXCEPTION") && block.contains("RAISE NOTICE 'V0383 完成"),
        "拆分后的 DO 块必须完整包含断言与完成提示"
    );
    assert!(
        block.trim_end().ends_with("END $$"),
        "拆分后的 DO 块必须以 END $$ 收口（被截断就会语法错误）"
    );

    let create_table_count = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE TABLE"))
        .count();
    assert_eq!(
        create_table_count, 10,
        "10 条 CREATE TABLE 应各成一条语句，实际 {create_table_count} 条"
    );

    let create_index_count = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE") && sql_head(s).contains("INDEX"))
        .count();
    assert_eq!(
        create_index_count, 27,
        "本迁移应有 27 条 CREATE INDEX（员工 6 + 合同 3 + 薪资标准 2 + 考勤期间 1 + 考勤记录 2 \
         + 请假 3 + 薪资项 2 + 薪资单 2 + 明细 4 + 明细项目 2），实际 {create_index_count} 条"
    );

    let comment_count = statements.iter().filter(|s| sql_head(s).starts_with("COMMENT")).count();
    assert!(comment_count >= 20, "注释语句被切坏或删光（实际 {comment_count} 条）");
}
