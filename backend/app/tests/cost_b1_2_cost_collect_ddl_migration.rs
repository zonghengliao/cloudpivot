//! 迁移 `V0371`（成本归集四表：`biz_cost_collect` / `biz_cost_labor` /
//! `biz_cost_overhead` / `biz_cost_rate`）的静态不变量。
//!
//! 卡：t_76333a39（COST-B1-2【成本】迁移）—— 设计依据
//! `docs/architecture/ARCH-9-成本核算升级设计.md` §3.1 ③④⑤⑥，拆卡清单 §8.1 的 B1-2。
//!
//! # 这层测试钉死什么
//!
//! 迁移是**一次性、不可回改**的东西（`schema_migrations` 记内容指纹，改一个字节
//! 已升级的机器就起不来），所以它的错误只有下次部署时才炸。这里把「写对了」的
//! 形状在 `cargo test` 阶段固化，覆盖的都是会**静默算错账**的类型：
//!
//! 1. **金额 / 数量列精度被写窄**（`NUMERIC(20,2)` 而不是 `18,4`）⇒
//!    `ck_biz_cost_labor_amount`（`amount = round(hours*rate,4)`）变成恒假，
//!    真实工资率的工时行根本插不进去。卡面验收写「金额 numeric(20,2)」，设计 §3.1
//!    写 `NUMERIC(18,4)` —— 本文件按设计落地并在 `v0371_amount_precision_follows_design`
//!    里写明实测理由（真库反证见 `docs/evidence/2026-11-10-cost-b1-2-v0371/`）。
//! 2. **唯一键漏掉 `WHERE deleted_at IS NULL`** ⇒ 软删后同一来源行再也归集不了、
//!    费率换版只能改历史行（不报错，运维只能改库）。
//! 3. **外键少了 `ON DELETE RESTRICT`** ⇒ 删成本对象时归集明细被静默带走 / 或
//!    直接删不动（CASCADE 时是前者）。
//! 4. **给主数据 / 单据列加物理外键** ⇒ 违反设计 §3.4 的弱引用约定，历史单据
//!    连 UPDATE 都做不了。
//! 5. **本卡越界建表**（建了 `biz_cost_alloc_rule` 之类）⇒ 与 B1-3 的迁移撞号撞表。
//! 6. **自检断言被删** ⇒ 迁移「跑过了但没验过」，等于没有自检。
//! 7. **语句被引擎拆分器切坏**（`DO $$ … $$` 里含分号）⇒ PG 报语法错误，
//!    整个迁移事务回滚，服务起不来。这条用引擎自己的 `split_statements` 验。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 的
//! `migration_versions_are_unique_padded_and_have_down` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up×3 / down×2 / 回滚重放 / 下游护栏 / 精度反证）的结果见卡内汇报与
//! `docs/evidence/2026-11-10-cost-b1-2-v0371/02_up_down_drill.out`。

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

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材（比如「金额不能用
/// numeric(20,2)」），「禁止出现」类断言必须只看真正会执行的 SQL。
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

/// 取一个语句块的「首行真正的 SQL」：跳过空行与整行 `--` 注释。
///
/// 引擎的 `split_statements` 把注释**原样留在语句块里**（见
/// `vendor/nik-db/src/migrate/splitter.rs`「`--` 行注释：整段原样保留」），
/// 所以块首往往是注释而不是 `DO` / `CREATE`。直接 `trim_start().starts_with("DO")`
/// 会数出 0 条 —— 这不是拆分失败，是判断方式错了。
fn sql_head(statement: &str) -> String {
    statement
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("--"))
        .unwrap_or("")
        .to_ascii_uppercase()
}

const V0371_UP: &str = "business/V0371__cost_collect_four_tables.up.sql";
const V0371_DOWN: &str = "business/V0371__cost_collect_four_tables.down.sql";

/// 本卡只允许碰这四张表（范围纪律：另外 6 张成本表各有自己的卡）。
const OWN_TABLES: [&str; 4] = [
    "biz_cost_collect",
    "biz_cost_labor",
    "biz_cost_overhead",
    "biz_cost_rate",
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

/// ① up / down 必须成对存在（缺 down = 回滚无路）。
#[test]
fn v0371_up_and_down_exist() {
    for rel in [V0371_UP, V0371_DOWN] {
        let path = backend_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ② 只建四张表，且建表语句必须幂等（`IF NOT EXISTS`）。
///
/// 多建一张就是越界（分摊/期间/二期各有自己的卡）；少一个 `IF NOT EXISTS`，
/// 迁移重复执行（回滚后重放 / 演习库补跑）就会 `relation already exists` 中断。
#[test]
fn v0371_creates_exactly_the_four_owned_tables_idempotently() {
    let sql = sql_only(&read_migration(V0371_UP));
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
        create_table_count, 4,
        "本迁移只许建归集四表（collect / labor / overhead / rate），实际有 {create_table_count} \
         处 CREATE TABLE —— 多出来的要么越界到 B1-3/B1-4，要么重复建表"
    );

    // 一期 10 张表里不属于本卡的，一张都不许出现
    for forbidden in [
        "biz_lot",
        "biz_cost_object",
        "biz_cost_alloc_rule",
        "biz_cost_alloc_result",
        "biz_cost_period",
        "biz_cost_period_item",
    ] {
        assert!(
            !squashed.contains(&format!("CREATE TABLE IF NOT EXISTS {forbidden}")),
            "本卡（B1-2）不建 {forbidden} —— biz_lot / biz_cost_object 是 B1-1（V0370），\
             分摊与期间四表是 B1-3（V0372/V0373）"
        );
    }
}

/// ③ 不许碰任何存量表结构（本卡是「只加新表」）。
///
/// 一旦出现 `ALTER TABLE biz_stock_out_item …`，就和 B1-5（出库/入库加列）的卡
/// 正面撞车：两张卡改同一张表、同一批上线，谁后跑谁覆盖。
#[test]
fn v0371_never_alters_existing_tables() {
    let code = sql_only(&read_migration(V0371_UP));
    for forbidden in ["ALTER TABLE", "TRUNCATE", "DROP TABLE", "DROP INDEX"] {
        assert!(
            !squash(&code).contains(forbidden),
            "V0371 只允许「建新表 + 建索引」，不允许出现 {forbidden}：\
             存量表改造属 B1-5 等卡，改同一张表会撞车"
        );
    }
}

/// ④ up 里的 DML 只允许出现在「自己四张表 + biz_cost_object 探针父行」上。
///
/// 探针父行是必需的：`ON DELETE RESTRICT` 只有真有一个父对象 + 一条子行才验得出来。
/// 但除了这个已知例外，不许往任何别人的表里写数据（会污染真库）。
#[test]
fn v0371_dml_only_touches_own_tables_and_probe_parent() {
    let code = sql_only(&read_migration(V0371_UP));
    let targets = dml_targets(&code);
    assert!(
        !targets.is_empty(),
        "自检里的探针行应该用 INSERT/DELETE 写自己的表；一个 DML 都没有说明自检被删光了"
    );
    for table in targets {
        assert!(
            OWN_TABLES.contains(&table.as_str()) || table == "biz_cost_object",
            "V0371 写了 {table} —— 除 biz_cost_object（RESTRICT 反证用的探针父行，断言后删除）\
             之外，本卡不许对任何其它表做 DML"
        );
    }
}

/// ⑤ 外键必须精确：3 条、全部指向 `biz_cost_object`、全部 `ON DELETE RESTRICT`。
///
/// 这是卡面验收项「外键指向 biz_cost_object 且 ON DELETE RESTRICT」。
/// 顺带堵住反向错误：给 `material_id` / `dept_id` 这类主数据列加物理外键
/// （设计 §3.4：主数据一律弱引用，否则主数据软删/重建会锁死历史成本）。
#[test]
fn v0371_foreign_keys_point_to_cost_object_with_restrict() {
    let code = sql_only(&read_migration(V0371_UP));
    let squashed = squash(&code);

    // 三个 FK 各自逐字校验
    for snippet in [
        "cost_object_id UUID REFERENCES biz_cost_object(id) ON DELETE RESTRICT",
        "cost_object_id UUID NOT NULL REFERENCES biz_cost_object(id) ON DELETE RESTRICT",
    ] {
        assert_has(
            &code,
            snippet,
            &format!("外键定义必须带 ON DELETE RESTRICT（漏了就是 NO ACTION / CASCADE）：{snippet}"),
        );
    }

    let fk_count = squashed.matches("REFERENCES").count();
    assert_eq!(
        fk_count, 3,
        "本迁移只允许 3 条物理外键（collect / labor / overhead 各一条指向 biz_cost_object），\
         实际 {fk_count} 处 REFERENCES —— 多出来的多半是给主数据列加了 FK（违反设计 §3.4 弱引用约定）"
    );

    // 反向：不许 REFERENCES 到成本对象以外的东西
    let mut rest = squashed.as_str();
    while let Some(pos) = rest.find("REFERENCES") {
        let after = &rest[pos + "REFERENCES".len()..];
        let target: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        assert_eq!(
            target, "biz_cost_object",
            "发现一条指向 {target} 的外键：成本表对主数据 / 单据必须弱引用（设计 §3.4）"
        );
        rest = after;
    }
}

/// ⑥ 两条唯一键都必须带软删条件（本卡验收项，也是 B1-1 定下的全仓口径）。
///
/// `uk_biz_cost_collect_src` 还额外要求 `source_id IS NOT NULL`：
/// 车间费用池行（MANUAL/折旧）`source_id` 为空，不带这个谓词它们会互相撞键。
#[test]
fn v0371_unique_keys_are_soft_delete_aware() {
    let sql = read_migration(V0371_UP);

    assert_has(
        &sql,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_collect_src
            ON biz_cost_collect (source_type, source_id, line_no)
            WHERE deleted_at IS NULL AND source_id IS NOT NULL",
        "uk_biz_cost_collect_src 必须是「UNIQUE + IF NOT EXISTS + 软删 + source_id 非空」的完整形状",
    );
    assert_has(
        &sql,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_rate
            ON biz_cost_rate (rate_type, scope, coalesce(scope_id, '00000000-0000-0000-0000-000000000000'::uuid), effective_from)
            WHERE deleted_at IS NULL",
        "uk_biz_cost_rate 必须是「UNIQUE + IF NOT EXISTS + 软删」的完整形状，\
         且 scope_id 用哨兵 uuid 把 COMPANY 的 NULL 收进同一个唯一键（否则 COMPANY 费率能重复配）",
    );

    // 反向：本迁移里任何 UNIQUE INDEX 都不许缺 WHERE deleted_at IS NULL
    let squashed = squash(&sql_only(&sql));
    let mut rest = squashed.as_str();
    let mut checked = 0;
    while let Some(pos) = rest.find("CREATE UNIQUE INDEX") {
        let stmt_end = rest[pos..].find(';').map(|e| pos + e).unwrap_or(rest.len());
        let stmt = &rest[pos..stmt_end];
        assert!(
            stmt.contains("WHERE deleted_at IS NULL"),
            "找到一条不带软删条件的唯一索引：{stmt}"
        );
        checked += 1;
        rest = &rest[stmt_end..];
    }
    assert_eq!(checked, 2, "本迁移应有 2 条唯一索引，实际 {checked} 条");
}

/// ⑦ 24 条 CHECK 约束一条都不能少（漏一条 = 一条脏数据入口）。
#[test]
fn v0371_keeps_all_check_constraints() {
    let sql = read_migration(V0371_UP);
    let expected: [(&str, &[&str]); 4] = [
        (
            "biz_cost_collect",
            &[
                "ck_biz_cost_collect_element",
                "ck_biz_cost_collect_src",
                "ck_biz_cost_collect_amount",
                "ck_biz_cost_collect_qty",
                "ck_biz_cost_collect_period",
                "ck_biz_cost_collect_mat",
                "ck_biz_cost_collect_obj",
            ],
        ),
        (
            "biz_cost_labor",
            &[
                "ck_biz_cost_labor_hours",
                "ck_biz_cost_labor_rate",
                "ck_biz_cost_labor_src",
                "ck_biz_cost_labor_status",
                "ck_biz_cost_labor_period",
                "ck_biz_cost_labor_amount",
            ],
        ),
        (
            "biz_cost_overhead",
            &[
                "ck_biz_cost_oh_item",
                "ck_biz_cost_oh_src",
                "ck_biz_cost_oh_amount",
                "ck_biz_cost_oh_period",
                "ck_biz_cost_oh_debit",
            ],
        ),
        (
            "biz_cost_rate",
            &[
                "ck_biz_cost_rate_type",
                "ck_biz_cost_rate_scope",
                "ck_biz_cost_rate_rate",
                "ck_biz_cost_rate_status",
                "ck_biz_cost_rate_dates",
                "ck_biz_cost_rate_scope_id",
            ],
        ),
    ];
    let mut total = 0;
    for (table, names) in expected {
        for name in names {
            assert_has(
                &sql,
                &format!("CONSTRAINT {name} CHECK ("),
                &format!("{table}.{name} 缺失"),
            );
            total += 1;
        }
    }
    assert_eq!(total, 24, "设计 §3.1 ③④⑤⑥ 一共 24 条 CHECK");

    // 最容易写错、代价最大的几条：把语义钉死
    assert_has(
        &sql,
        "ck_biz_cost_collect_mat CHECK ( (element = 'MATERIAL' AND material_id IS NOT NULL AND qty IS NOT NULL)
            OR (element <> 'MATERIAL' AND material_id IS NULL AND qty IS NULL))",
        "料行必须有物料+数量、工/费行不许挂物料：否则「一张表两种语义」，料成本与人工成本会串口径",
    );
    assert_has(
        &sql,
        "ck_biz_cost_collect_obj CHECK ( cost_object_id IS NOT NULL
            OR (element = 'OVERHEAD' AND source_type IN ('DEPRECIATION','EXPENSE','OVERHEAD_TICKET')))",
        "只有折旧/报销/手工费用三类池行允许不挂成本对象：领料行不挂对象会让料成本掉进无主池",
    );
    assert_has(
        &sql,
        "ck_biz_cost_labor_amount CHECK (amount = round(hours * rate, 4))",
        "工时金额必须等于 工时×费率（4 位四舍五入）：堵「改工时忘改金额」",
    );
    assert_has(
        &sql,
        "ck_biz_cost_oh_debit CHECK (debit_account_code LIKE '4101.%')",
        "制造费用借方必须 4101.*：配成 5602 会让费用永远进不了成本（设计风险 R-2）",
    );
    assert_has(
        &sql,
        "ck_biz_cost_rate_scope_id CHECK ( (scope = 'COMPANY' AND scope_id IS NULL)
            OR (scope <> 'COMPANY' AND scope_id IS NOT NULL))",
        "费率范围与 scope_id 必须配对：COMPANY 带 id / DEPT 空 id 都会让取数错配到别的车间",
    );
    // period_code 的 yyyy-MM 正则：四张表里三张有期间列（rate 是版本表，用生效期）
    let period_checks = squash(&sql_only(&sql))
        .matches("period_code ~ '^\\d{4}-(0[1-9]|1[0-2])$'")
        .count();
    assert_eq!(
        period_checks, 3,
        "collect / labor / overhead 三张表的 period_code 都必须有 yyyy-MM 正则 CHECK，实际 {period_checks} 条"
    );
}

/// ⑧ 关键列的类型 / 可空性 / 默认值必须与设计 §3.1 一致。
///
/// **精度这一条是卡内偏差的固化点**：卡面验收写「金额 numeric(20,2)、数量 numeric(20,4)」，
/// 设计 §3.1 写 `NUMERIC(18,4)`（金额/数量）+ `NUMERIC(18,6)`（单价/费率）。
/// 这里取设计，理由是实测出来的，不是口味问题：
///
/// * `ck_biz_cost_labor_amount` 是 `amount = round(hours*rate,4)`，而 `rate` 是
///   `NUMERIC(18,6)`（月薪 ÷ 工时必然除不尽）。amount 若只有 2 位小数，
///   `hours=1, rate=33.333333` 这类**真实**工资率算出的 33.3333 存不进去，
///   CHECK 恒假 ⇒ 工时行插不进库（真库反证见证据目录第 7 步）。
/// * 约当产量 / 分摊尾差（设计 §4.4.4、§4.6.2）全部按 4 位小数倒挤。
/// * 全仓口径：`backend/migrations` 里 210 处 `NUMERIC(18,4)`、0 处 `numeric(20,2)`
///   的列定义；前置卡 V0370 的数量列同样是 `NUMERIC(18,4)`。
#[test]
fn v0371_amount_precision_follows_design() {
    let sql = read_migration(V0371_UP);

    let expected = [
        // ③ biz_cost_collect
        "cost_object_id UUID REFERENCES biz_cost_object(id) ON DELETE RESTRICT",
        "period_code VARCHAR(7) NOT NULL",
        "element VARCHAR(12) NOT NULL",
        "source_type VARCHAR(16) NOT NULL",
        "line_no INT NOT NULL DEFAULT 0",
        "occur_date DATE NOT NULL",
        "qty NUMERIC(18,4)",
        "unit_price NUMERIC(18,6)",
        "amount NUMERIC(18,4) NOT NULL",
        "dept_name VARCHAR(120)",
        // ④ biz_cost_labor
        "cost_object_id UUID NOT NULL REFERENCES biz_cost_object(id) ON DELETE RESTRICT",
        "work_date DATE NOT NULL",
        "employee_name VARCHAR(50) NOT NULL",
        "dept_id UUID NOT NULL",
        "hours NUMERIC(10,2) NOT NULL",
        "machine_hours NUMERIC(10,2) NOT NULL DEFAULT 0",
        "rate NUMERIC(18,6) NOT NULL",
        "amount NUMERIC(18,4) NOT NULL",
        "source_type VARCHAR(12) NOT NULL DEFAULT 'MANUAL'",
        "status VARCHAR(12) NOT NULL DEFAULT 'DRAFT'",
        // ⑤ biz_cost_overhead
        "expense_item VARCHAR(16) NOT NULL",
        "debit_account_code VARCHAR(30) NOT NULL DEFAULT '4101.09'",
        "credit_account_code VARCHAR(30) NOT NULL",
        "amount NUMERIC(18,4) NOT NULL",
        // ⑥ biz_cost_rate
        "rate_type VARCHAR(12) NOT NULL",
        "scope VARCHAR(12) NOT NULL",
        "rate NUMERIC(18,6) NOT NULL",
        "effective_from DATE NOT NULL",
        "status SMALLINT NOT NULL DEFAULT 1",
        // 软删列（四张表都有，是全库软删口径的锚点）
        "deleted_at TIMESTAMPTZ",
    ];
    for snippet in expected {
        assert_has(&sql, snippet, &format!("列定义与设计不符（漏了或写窄了）：{snippet}"));
    }

    // 反向断言：本迁移里不许出现 scale=2 的金额列（20,2 也不行 —— 整数字段更宽但小数位更少）
    let code = squash(&sql_only(&sql));
    for bad in ["NUMERIC(20,2)", "NUMERIC(18,2)", "NUMERIC(20,4)"] {
        assert!(
            !code.contains(bad),
            "V0371 出现了 {bad}：金额/数量列必须是设计 §3.1 的 NUMERIC(18,4)，\
             否则 round(…,4) 的 CHECK 恒假、约当产量的 4 位尾差会被抹平"
        );
    }
}

/// ⑨ 自检写进迁移：3 个 `DO $$` 块（前置护栏 + 形态 + 行为），每个失败路径都 `RAISE EXCEPTION`。
///
/// 「迁移跑过了」和「迁移验过了」是两件事。没有 RAISE EXCEPTION 的 DO 块只是打印，
/// 表结构错了照样提交，事故要等到算账时才发现。
#[test]
fn v0371_self_check_raises_exceptions() {
    let sql = read_migration(V0371_UP);
    let blocks: Vec<&str> = sql.split("DO $$").skip(1).collect();
    assert_eq!(
        blocks.len(),
        3,
        "V0371 应有 3 个 DO 块（前置护栏 / 形态断言 / 行为断言），实际 {} 个",
        blocks.len()
    );

    for (i, block) in blocks.iter().enumerate() {
        assert!(
            block.contains("RAISE EXCEPTION"),
            "第 {} 个 DO 块没有任何 RAISE EXCEPTION —— 自检失败也不会拦住迁移",
            i + 1
        );
    }

    // 关键断言逐条点名（删掉任意一条都要红）
    for msg in [
        "biz_cost_object 不存在，请先执行 V0370",
        "uk_biz_cost_collect_src 缺「软删 + source_id 非空」谓词",
        "uk_biz_cost_rate 缺软删条件",
        "biz_cost_collect 的 7 条 CHECK 约束只有",
        "biz_cost_labor 的 6 条 CHECK 约束只有",
        "biz_cost_overhead 的 5 条 CHECK 约束只有",
        "biz_cost_rate 的 6 条 CHECK 约束只有",
        "指向 biz_cost_object 的外键应有 3 条",
        "条外键不是 ON DELETE RESTRICT",
        "四张表上一共有",
        "同一来源行被归集两次竟然成功",
        "料行不带 material_id 写入成功",
        "领料行不挂成本对象竟然写入成功",
        "amount≠hours×rate 写入成功",
        "制造费用借方记 5602.01 竟然成功",
        "同日同范围第二条活费率写入成功",
        "有归集行时成本对象被删掉",
        "探针行未清理干净",
    ] {
        assert!(sql.contains(msg), "自检断言里必须有一条覆盖「{msg}」（当前找不到）");
    }

    // 探针行必须删干净：迁移重复执行不能往真库里攒垃圾行
    let code = squash(&sql_only(&sql));
    for table in OWN_TABLES {
        assert!(
            code.contains(&format!("DELETE FROM {table}")),
            "{table} 的自检探针行必须在迁移里被 DELETE 掉（重复执行零残留）"
        );
    }
    let remark_deletes = code.matches("remark = 'V0371-SELFCHECK'").count();
    assert!(
        remark_deletes >= 4,
        "四张表的探针行都应按 remark='V0371-SELFCHECK' 清理（实际只有 {remark_deletes} 处）"
    );
    assert_has(
        &sql,
        "DELETE FROM biz_cost_object   WHERE id = v_obj;",
        "探针父行（成本对象）必须按 id 删干净",
    );
}

/// ⑩ down 必须幂等 + 有下游依赖护栏 + 结尾断言零残留。
#[test]
fn v0371_down_is_idempotent_and_guarded() {
    let down = read_migration(V0371_DOWN);

    for table in OWN_TABLES {
        assert_has(
            &down,
            &format!("DROP TABLE IF EXISTS {table};"),
            &format!("down 必须用 DROP TABLE IF EXISTS 删 {table}（重复执行不报错）"),
        );
    }

    // 下游表还在时必须拒绝执行（否则 PG 抛的是「内部依赖」错误，看不出该先回滚哪张卡）
    assert_has(
        &down,
        "FROM pg_constraint f",
        "down 必须用 pg_constraint 扫「有没有外部表 FK 到我这四张表」，而不是硬编码表名",
    );
    assert_has(
        &down,
        "V0371 down 拒绝执行：下游成本表仍在",
        "down 的拒绝信息必须说清「先回滚哪张卡」",
    );
    assert_has(
        &down,
        "请先回滚建立它们的迁移（V0372 及之后），再回滚 V0371",
        "down 必须指明该先回滚哪段号段",
    );

    // down 自身幂等：不许出现 `'biz_cost_labor'::regclass` 这种「表已删就抛错」的写法
    let code = sql_only(&down);
    for table in OWN_TABLES {
        assert!(
            !code.contains(&format!("'{table}'::regclass")),
            "down 里出现了 '{table}'::regclass：第二次执行 down 时表已不存在，\
             这行会直接抛 `relation does not exist`，down 就不幂等了（改用 to_regclass / 表名比较）"
        );
    }
    assert!(
        squash(&code).contains("RAISE NOTICE 'V0371 down 完成"),
        "down 结尾必须有自检：删完要断言四张表和索引确实没了"
    );
}

/// ⑪ 引擎拆分器必须把三个 `DO $$ … $$` 各留成一条语句。
///
/// 用框架自己的 `split_statements`（引擎执行迁移走的就是它）验证：
/// `$$` 内部的 `;` 若被当成语句分隔符，PG 会报语法错误，整个迁移事务回滚 ⇒ 服务起不来。
#[test]
fn v0371_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    let sql = read_migration(V0371_UP);
    let statements =
        split_statements(&sql, DialectKind::Postgres).expect("引擎拆分器无法拆分 V0371（方言感知的 $$ 处理失败）");

    let do_blocks: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
    assert_eq!(
        do_blocks.len(),
        3,
        "三个 DO 块必须各被拆成**一条**语句（实际 {} 条）：>3 说明 $$ 内部的 `;` 被当成分隔符了",
        do_blocks.len()
    );
    for block in &do_blocks {
        assert!(block.contains("RAISE EXCEPTION"), "拆分后的 DO 块必须完整包含断言");
        assert!(
            block.trim_end().ends_with("END $$;") || block.trim_end().ends_with("END $$"),
            "拆分后的 DO 块必须以 END $$ 收口（被截断就会语法错误）"
        );
    }
    let last = do_blocks.last().unwrap();
    assert!(
        last.contains("RAISE NOTICE 'V0371 完成"),
        "行为自检块必须以完成提示收口（否则是被截断的最后一块）"
    );

    let create_table_count = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE TABLE"))
        .count();
    assert_eq!(
        create_table_count, 4,
        "四条 CREATE TABLE 应各成一条语句，实际 {create_table_count} 条"
    );

    let create_index_count = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE") && sql_head(s).contains("INDEX"))
        .count();
    assert_eq!(
        create_index_count, 10,
        "本迁移应有 10 条 CREATE INDEX（collect 3 + labor 3 + overhead 2 + rate 2），实际 {create_index_count} 条"
    );
}
