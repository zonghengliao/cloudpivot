//! 迁移 `V0370`（成本核算地基地基：`biz_lot` + `biz_cost_object`）的静态不变量。
//!
//! 卡：t_f2b6d80a（COST-B1-1【成本】迁移）—— 设计依据
//! `docs/architecture/ARCH-9-成本核算升级设计.md` §3.1 ① ②，拆卡清单 §8.1 的 B1-1。
//!
//! # 这层测试钉死什么
//!
//! 迁移是**一次性、不可回改**的东西（`schema_migrations` 记内容指纹，改一个字节
//! 已升级的机器就起不来），所以它的错误只有在下次部署时才炸。这里把「写对了」的
//! 形状在 `cargo test` 阶段固化，覆盖的都是会**静默算错账**的类型：
//!
//! 1. 唯一键漏掉 `WHERE deleted_at IS NULL` ⇒ 软删后同批号/同对象号再也建不出来
//!    （不报错，只是业务上「删了就得改名」，运维只能改库）；
//! 2. 两张主数据表被加上物理外键 ⇒ 存量 `lot_no` 脏数据（空串/大小写/全半角）
//!    会让历史单据连 UPDATE 都做不了（设计 §3.1 ① 明确「弱引用，不建 FK」）；
//! 3. 本卡越界改存量表（`ALTER TABLE biz_stock_in_item` 之类）⇒ 与 B1-5 的卡撞车；
//! 4. 自检断言被删 ⇒ 迁移「跑过了但没验过」，等于没有自检；
//! 5. 语句被引擎拆分器切坏（`DO $$ … $$` 里含分号）⇒ PG 报语法错误，
//!    整个迁移事务回滚，服务起不来。这条用引擎自己的 `split_statements` 验。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 的
//! `migration_versions_are_unique_padded_and_have_down` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up×3 / down×2 / 反证）的结果写在卡内汇报里。

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

const V0370_UP: &str = "business/V0370__cost_lot_and_cost_object.up.sql";
const V0370_DOWN: &str = "business/V0370__cost_lot_and_cost_object.down.sql";

/// 本卡只允许碰这两张表（范围纪律：其它 8 张成本表各有自己的卡）。
const OWN_TABLES: [&str; 2] = ["biz_lot", "biz_cost_object"];

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
fn v0370_up_and_down_exist() {
    for rel in [V0370_UP, V0370_DOWN] {
        let path = backend_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ② 只建两张表，且建表语句必须幂等（`IF NOT EXISTS`）。
///
/// 多建一张就是越界（B1-2 起每张表有自己的卡）；少一个 `IF NOT EXISTS`，
/// 迁移重复执行（回滚后重放 / 演习库补跑）就会 `relation already exists` 中断。
#[test]
fn v0370_creates_exactly_the_two_owned_tables_idempotently() {
    let sql = sql_only(&read_migration(V0370_UP));
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
        create_table_count, 2,
        "本迁移只许建 biz_lot / biz_cost_object 两张表（越界建表会把「单卡 ≤1.5h」变成大杂烩，\
         且与 B1-2~B1-4 的迁移撞车），实际有 {create_table_count} 处 CREATE TABLE"
    );

    // 一期 10 张表里的另外 8 张，一张都不许在本卡出现
    for forbidden in [
        "biz_cost_collect",
        "biz_cost_labor",
        "biz_cost_overhead",
        "biz_cost_rate",
        "biz_cost_alloc_rule",
        "biz_cost_alloc_result",
        "biz_cost_period",
        "biz_cost_period_item",
    ] {
        assert!(
            !squashed.contains(&format!("CREATE TABLE IF NOT EXISTS {forbidden}")),
            "本卡（B1-1）不建 {forbidden} —— 那是 B1-2 / B1-3 / B1-4 的范围"
        );
    }
}

/// ③ 不许碰任何存量表结构（本卡是「只加新表」）。
///
/// 一旦在这里出现 `ALTER TABLE biz_stock_in_item …`，就和 B1-5（出库/入库加列）
/// 的卡正面撞车：两张卡改同一张表、同一批上线，谁后跑谁覆盖。
#[test]
fn v0370_never_alters_existing_tables() {
    let code = sql_only(&read_migration(V0370_UP));
    for forbidden in ["ALTER TABLE", "TRUNCATE", "DROP TABLE", "DROP INDEX"] {
        assert!(
            !squash(&code).contains(forbidden),
            "V0370 只允许「建新表 + 建索引」，不允许出现 {forbidden}：\
             存量表改造属 B1-5（出库/入库加列）等卡，改同一张表会撞车"
        );
    }
}

/// ④ up 里所有 DML 只碰自己这两张表（探针行），不往别人的表里写数据。
#[test]
fn v0370_dml_only_touches_own_tables() {
    let code = sql_only(&read_migration(V0370_UP));
    let targets = dml_targets(&code);
    assert!(
        !targets.is_empty(),
        "自检里的探针行应该用 INSERT/DELETE 写自己两张表；一个 DML 都没有说明自检被删光了"
    );
    for table in targets {
        assert!(
            OWN_TABLES.contains(&table.as_str()),
            "V0370 写了 {table} —— 本卡只允许对 biz_lot / biz_cost_object 做探针行；\
             往存量表里插数据会污染真库"
        );
    }
}

/// ⑤ 两张主数据表**不许有物理外键**（设计 §3.1 ① 定稿：弱引用）。
///
/// 存量 `biz_stock_in_item.lot_no` / `biz_stock_out_item.lot_no` /
/// `biz_material_serial.batch_no` 是三处自由文本，质量不可控（空串、大小写、全半角）。
/// 加 FK 不是「更严谨」，而是让历史单据连 UPDATE 都做不了 —— 校验放 handler + 巡检。
#[test]
fn v0370_has_no_physical_foreign_keys() {
    let code = sql_only(&read_migration(V0370_UP));
    assert!(
        !squash(&code).contains("REFERENCES"),
        "V0370 里出现了 REFERENCES：设计 §3.1 定稿为「弱引用，不建 FK」\
         （material_id / target_warehouse_id / workshop_dept_id 全是弱引用）"
    );
}

/// ⑥ 唯一键必须带软删条件：`UNIQUE … WHERE deleted_at IS NULL`。
///
/// 这是本卡验收项之一。漏掉谓词不会报错，只会让「删除后重建同批号」变成永不可能。
#[test]
fn v0370_unique_keys_are_soft_delete_aware() {
    let sql = read_migration(V0370_UP);
    let squashed = squash(&sql);

    for (index, columns) in [
        (
            "uk_biz_lot_material_no",
            "ON biz_lot (material_id, lot_no) WHERE deleted_at IS NULL",
        ),
        (
            "uk_biz_cost_object_no",
            "ON biz_cost_object (object_no) WHERE deleted_at IS NULL",
        ),
    ] {
        assert_has(
            &sql,
            &format!("CREATE UNIQUE INDEX IF NOT EXISTS {index} {columns}"),
            &format!("{index} 必须是「UNIQUE + IF NOT EXISTS + WHERE deleted_at IS NULL」的完整形状"),
        );
    }

    // 反向：不许出现「不带 WHERE 的 UNIQUE INDEX on 这两张表」（将来有人补索引时挡住）
    for bad in [
        "UNIQUE INDEX IF NOT EXISTS uk_biz_lot_material_no",
        "UNIQUE INDEX IF NOT EXISTS uk_biz_cost_object_no",
    ] {
        let mut rest = squashed.as_str();
        while let Some(pos) = rest.find(bad) {
            let stmt_end = rest[pos..].find(';').map(|e| pos + e).unwrap_or(rest.len());
            let stmt = &rest[pos..stmt_end];
            assert!(
                stmt.contains("WHERE deleted_at IS NULL"),
                "找到一条不带软删条件的唯一索引：{stmt}"
            );
            rest = &rest[stmt_end..];
        }
    }
}

/// ⑦ 11 条 CHECK 约束一条都不能少（漏一条 = 一条脏数据入口）。
#[test]
fn v0370_keeps_all_check_constraints() {
    let sql = read_migration(V0370_UP);
    let lot_constraints = [
        "ck_biz_lot_source",
        "ck_biz_lot_qc",
        "ck_biz_lot_status",
        "ck_biz_lot_dates",
    ];
    let object_constraints = [
        "ck_biz_cost_object_type",
        "ck_biz_cost_object_status",
        "ck_biz_cost_object_input",
        "ck_biz_cost_object_qty",
        "ck_biz_cost_object_pct",
        "ck_biz_cost_object_dates",
        "ck_biz_cost_object_closed",
    ];
    for name in lot_constraints.iter().chain(object_constraints.iter()) {
        assert_has(&sql, &format!("CONSTRAINT {name} CHECK ("), &format!("{name} 缺失"));
    }

    // 值域与设计逐字一致
    assert_has(
        &sql,
        "ck_biz_lot_source CHECK (source_type IN ('PURCHASE','PRODUCTION','OPENING','MANUAL'))",
        "biz_lot.source_type 值域必须与设计一致（PURCHASE/PRODUCTION/OPENING/MANUAL）",
    );
    assert_has(
        &sql,
        "ck_biz_cost_object_status CHECK (status IN ('OPEN','CLOSED','CANCELLED'))",
        "成本对象状态机值域必须与设计 §4.1 一致（OPEN/CLOSED/CANCELLED）",
    );
    assert_has(
        &sql,
        "ck_biz_cost_object_closed CHECK (status <> 'CLOSED' OR closed_at IS NOT NULL)",
        "关批必须有 closed_at：否则「谁关的、什么时候关的」说不清（死单防线）",
    );
    assert_has(
        &sql,
        "ck_biz_cost_object_qty CHECK (plan_qty > 0 AND completed_qty >= 0 AND wip_qty >= 0)",
        "plan_qty 必须 > 0：0 会让约当产量公式除零",
    );
}

/// ⑧ 关键列的类型 / 可空性 / 默认值必须与设计 §3.1 一致。
///
/// 这些列是下游卡（归集、约当产量、结算回写）直接依赖的口径，
/// 类型写窄（NUMERIC(18,2) 而不是 (18,4)）会在第 4 位小数上算错金额。
#[test]
fn v0370_column_types_match_design() {
    let sql = read_migration(V0370_UP);
    let expected = [
        // biz_lot
        "lot_no VARCHAR(64) NOT NULL",
        "material_id UUID NOT NULL",
        "source_type VARCHAR(16) NOT NULL DEFAULT 'MANUAL'",
        "qc_status VARCHAR(12) NOT NULL DEFAULT 'NA'",
        "status SMALLINT NOT NULL DEFAULT 1",
        // biz_cost_object
        "object_no VARCHAR(32) NOT NULL",
        "object_type VARCHAR(12) NOT NULL DEFAULT 'BATCH'",
        "product_id UUID NOT NULL",
        "target_warehouse_id UUID NOT NULL",
        "plan_qty NUMERIC(18,4) NOT NULL",
        "completed_qty NUMERIC(18,4) NOT NULL DEFAULT 0",
        "wip_qty NUMERIC(18,4) NOT NULL DEFAULT 0",
        "completion_pct NUMERIC(5,2) NOT NULL DEFAULT 0",
        "material_input_mode VARCHAR(16) NOT NULL DEFAULT 'ONE_TIME'",
        "workshop_dept_id UUID NOT NULL",
        "workshop_dept_name VARCHAR(120) NOT NULL",
        "start_date DATE NOT NULL",
        "status VARCHAR(12) NOT NULL DEFAULT 'OPEN'",
        // 软删列（两张表都有，是全库软删口径的锚点）
        "deleted_at TIMESTAMPTZ",
    ];
    for snippet in expected {
        assert_has(&sql, snippet, &format!("列定义与设计不符（漏了或写窄了）：{snippet}"));
    }
}

/// ⑨ 自检写进迁移：至少两个 `DO $$` 块，且每个失败路径都 `RAISE EXCEPTION`。
///
/// 「迁移跑过了」和「迁移验过了」是两件事。没有 RAISE EXCEPTION 的 DO 块只是打印，
/// 表结构错了照样提交，事故要等到算账时才发现。
#[test]
fn v0370_self_check_raises_exceptions() {
    let sql = read_migration(V0370_UP);
    let blocks: Vec<&str> = sql.split("DO $$").skip(1).collect();
    assert!(!blocks.is_empty(), "V0370 必须自带自检（DO $$ … RAISE EXCEPTION … $$）");

    for (i, block) in blocks.iter().enumerate() {
        assert!(
            block.contains("RAISE EXCEPTION"),
            "第 {} 个 DO 块没有任何 RAISE EXCEPTION —— 自检失败也不会拦住迁移",
            i + 1
        );
    }

    // 关键断言逐条点名（删掉任意一条都要红）
    for msg in [
        "biz_lot 未建出来",
        "biz_cost_object 未建出来",
        "uk_biz_lot_material_no 缺软删条件",
        "uk_biz_cost_object_no 缺软删条件",
        "biz_lot 的 4 条 CHECK 约束只有",
        "biz_cost_object 的 7 条 CHECK 约束只有",
        "同物料同批号写入第二条活行竟然成功",
        "同 object_no 写入第二条活行竟然成功",
        "ck_biz_cost_object_closed 未生效",
        "探针行未清理干净",
    ] {
        assert!(sql.contains(msg), "自检断言里必须有一条覆盖「{msg}」（当前找不到）");
    }

    // 探针行必须删干净：迁移重复执行不能往真库里攒垃圾行
    assert_has(
        &sql,
        "DELETE FROM biz_cost_object WHERE product_id = v_mat;",
        "自检探针行必须按 probe 键删干净（重复执行零残留）",
    );
    assert_has(
        &sql,
        "DELETE FROM biz_lot         WHERE material_id = v_mat;",
        "自检探针行必须按 probe 键删干净（重复执行零残留）",
    );
}

/// ⑩ down 必须幂等 + 有下游依赖护栏 + 结尾断言零残留。
#[test]
fn v0370_down_is_idempotent_and_guarded() {
    let down = read_migration(V0370_DOWN);

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
        "biz_cost_collect",
        "down 必须检查下游成本表是否还在（V0371 起的表 FK 到 biz_cost_object）",
    );
    assert_has(
        &down,
        "请先回滚建立它们的迁移（V0371 及之后），再回滚 V0370",
        "down 的拒绝信息必须说清「先回滚哪张卡」",
    );
    assert!(
        squash(&down).contains("RAISE EXCEPTION"),
        "down 必须有自检：删完要断言两张表确实没了"
    );
}

/// ⑪ 引擎拆分器必须把两个 `DO $$ … $$` 留在一条语句里。
///
/// 用框架自己的 `split_statements`（引擎执行迁移走的就是它）验证：
/// `$$` 内部的 `;` 若被当成语句分隔符，PG 会报语法错误，整个迁移事务回滚 ⇒ 服务起不来。
#[test]
fn v0370_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    let sql = read_migration(V0370_UP);
    let statements =
        split_statements(&sql, DialectKind::Postgres).expect("引擎拆分器无法拆分 V0370（方言感知的 $$ 处理失败）");

    let do_blocks: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
    assert_eq!(
        do_blocks.len(),
        1,
        "自检 DO 块必须被拆成**一条**语句（实际 {} 条）：>1 说明 $$ 内部的 `;` 被当成分隔符了",
        do_blocks.len()
    );
    let block = do_blocks[0];
    assert!(
        block.contains("RAISE EXCEPTION") && block.contains("RAISE NOTICE 'V0370 完成"),
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
        create_table_count, 2,
        "两条 CREATE TABLE 应各成一条语句，实际 {create_table_count} 条"
    );

    let create_index_count = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE") && sql_head(s).contains("INDEX"))
        .count();
    assert_eq!(
        create_index_count, 7,
        "本迁移应有 7 条 CREATE INDEX（biz_lot 3 条 + biz_cost_object 4 条），实际 {create_index_count} 条"
    );
}
