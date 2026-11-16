//! 迁移 `V0414`（M1-13：现金流底稿表结构 `biz_cashflow_worksheet`）的静态不变量。
//!
//! 卡：t_31c91cd5（M1-13【M1·财务】现金流底稿：表结构 biz_cashflow_worksheet）。
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §4 M1 财务 的 M1-13 行；
//! 下游 M1-14（录入 + 勾稽校验·直接法）按本表的契约写 upsert 与校验。
//!
//! # 这层测试钉死什么
//!
//! 迁移是**一次性、不可回改**的东西（`schema_migrations` 记内容指纹，改一个字节
//! 已升级的机器就起不来），写错了只有下次部署才炸。这里把「写对了的形状」固化到
//! `cargo test`，覆盖的都是会**静默算错账**的类型：
//!
//! 1. **金额精度被写窄**（`NUMERIC(18,2)`）⇒ 底稿金额来自凭证分录（`NUMERIC(20,4)`），
//!    2 位会在按比例拆分时造出「底稿合计 ≠ 凭证合计」的尾差，不报错、只差几分钱。
//! 2. **明细行允许负数** ⇒ 用负号表达流出会让「经营活动流入合计」出现负流入，
//!    流入/流出两栏互相抵消，报表口径当场失真。方向只能由 `direction` 表达。
//! 3. **唯一性只靠普通唯一约束** ⇒ 余额行的 `item_code` 是 NULL，PG 允许多个 NULL，
//!    「一个期间两个期末余额」这种两个真相就挡不住；必须是**部分**唯一索引。
//! 4. **期间格式没约束** ⇒ `'2026-1'` / `'2026-13'` 的行永远聚合不进任何一期（静默丢数）。
//! 5. **给 `period_code` / `item_code` 加物理外键** ⇒ 期间反结账、分类配置换版这类
//!    正常运维动作会被底稿卡住，历史底稿失去可读性（AGENTS.md 弱引用口径）。
//! 6. **`amount` 之外的「口径列」漂移**：加币种列（底稿只出本位币）、加 `deleted_at`
//!    （墓碑行会造出「这行已删、报表还算着」的双真相）。两条都在本文件里反向钉死，
//!    要改就得连测试一起改 —— 那是**决定**，不是顺手补丁。
//! 7. **本卡越界**：建了别的表 / 动了 `biz_cashflow_item` 或存量表（M1-14..M1-18 各有卡）。
//! 8. **自检断言被删** ⇒ 迁移「跑过了但没验过」，等于没有自检。
//! 9. **语句被引擎拆分器切坏**（`DO $$ … $$` 里含分号）⇒ PG 报语法错误、整个迁移事务
//!    回滚、服务起不来。这条用引擎自己的 `split_statements` 验。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 的
//! `migration_versions_are_unique_padded_and_have_down` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up×2 / down×2 / 回滚重放 / 护栏反证 / upsert / 勾稽闭合 30 断言）的实测见
//! `bash scripts/verify/m113_cashflow_worksheet_drill.sh`。

use std::path::PathBuf;

/// `backend/app` 目录（集成测试 crate 的 manifest 目录就是迁移根所在层）。
fn app_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_migration(relative: &str) -> String {
    let path = app_root().join("migrations").join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到迁移文件 {}: {e}", path.display()))
}

/// 把连续空白压成单个空格：SQL 断言只关心语句形状，不该被缩进/换行绑死。
fn squash(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材（比如「金额不能写 NUMERIC(18,2)」），
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
/// 所以块首往往是注释而不是 `DO` / `CREATE`。
fn sql_head(statement: &str) -> String {
    statement
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("--"))
        .unwrap_or("")
        .to_ascii_uppercase()
}

const V0414_UP: &str = "business/V0414__cashflow_worksheet.up.sql";
const V0414_DOWN: &str = "business/V0414__cashflow_worksheet.down.sql";

/// 本卡只允许碰这一张表（M1-14 录入、M1-15 间接法配置、M1-16 出表各有自己的卡）。
const OWN_TABLE: &str = "biz_cashflow_worksheet";

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
fn v0414_up_and_down_exist() {
    for rel in [V0414_UP, V0414_DOWN] {
        let path = app_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ② 只建这一张表，且建表语句必须幂等。
///
/// 多一张表就是越界（M1-14..M1-18 各自有卡）；少一个 `IF NOT EXISTS`，
/// 回滚后重放 / 演练库补跑就会 `relation already exists` 中断。
#[test]
fn v0414_creates_exactly_one_table_idempotently() {
    let sql = sql_only(&read_migration(V0414_UP));
    let squashed = squash(&sql);

    assert_has(
        &sql,
        "CREATE TABLE IF NOT EXISTS biz_cashflow_worksheet (",
        "建表语句必须是 `CREATE TABLE IF NOT EXISTS biz_cashflow_worksheet`（幂等 + 表名固定）",
    );

    let create_table_count = squashed.matches("CREATE TABLE").count();
    assert_eq!(
        create_table_count, 1,
        "本卡只许建 1 张表（{OWN_TABLE}），实际 {create_table_count} 处 CREATE TABLE —— 多出来的越界了"
    );

    // 不许改 / 删存量表：本卡是「纯新建」，不碰任何已存在的表结构
    for forbidden in ["ALTER TABLE", "DROP TABLE", "TRUNCATE"] {
        assert!(
            !squashed.contains(forbidden),
            "V0414 出现 {forbidden}：本卡只新建一张表，改/删存量表属别的卡"
        );
    }

    // 自检探针只许写自己的表：碰到 biz_cashflow_item / biz_voucher 就是越界
    for table in dml_targets(&sql) {
        assert_eq!(
            table, OWN_TABLE,
            "V0414 的 DML 打到了 {table} —— 本卡只许写 {OWN_TABLE}（探针行必须自清场，别去动配置表/凭证）"
        );
    }
}

/// ③ 14 列逐列形状：长度 / 精度 / 可空性 / 默认值。
#[test]
fn v0414_columns_have_designed_shape() {
    let sql = read_migration(V0414_UP);

    for (snippet, why) in [
        (
            "period_code VARCHAR(7) NOT NULL",
            "period_code 必须 VARCHAR(7) NOT NULL（与 biz_accounting_period.period_code 同型同长度，否则跨表比较要隐式转换）",
        ),
        (
            "line_type VARCHAR(8) NOT NULL DEFAULT 'FLOW'",
            "line_type 必须 VARCHAR(8) NOT NULL DEFAULT 'FLOW'（默认明细行，余额行显式声明）",
        ),
        (
            "item_code VARCHAR(32)",
            "item_code 必须 VARCHAR(32) 且可空（余额行没有项目编码）",
        ),
        (
            "item_name VARCHAR(64) NOT NULL",
            "item_name 必须 VARCHAR(64) NOT NULL（项目名快照：配置改名后历史底稿仍按当时名称呈现）",
        ),
        (
            "activity VARCHAR(12)",
            "activity 必须 VARCHAR(12) 且可空（OPERATING/INVESTING/FINANCING，余额行为空）",
        ),
        (
            "direction VARCHAR(8)",
            "direction 必须 VARCHAR(8) 且可空（INFLOW/OUTFLOW，余额行为空）",
        ),
        (
            "amount NUMERIC(18,4) NOT NULL DEFAULT 0",
            "amount 必须 NUMERIC(18,4) NOT NULL DEFAULT 0（见文件头 1.，与全仓 43 张表同口径）",
        ),
        (
            "sort_order INT NOT NULL DEFAULT 0",
            "sort_order 必须 INT NOT NULL DEFAULT 0（期初 1 / 期末 9999 / 明细照抄配置）",
        ),
    ] {
        assert_has(&sql, snippet, why);
    }

    // 反向：金额精度不许写窄到 2 位、也不许放宽成 6 位（会与凭证 20,4 的合计对不上）
    let code = squash(&sql_only(&sql));
    for bad in [
        "amount NUMERIC(18,2)",
        "amount NUMERIC(20,2)",
        "amount NUMERIC(18,6)",
        "amount NUMERIC(12,2)",
    ] {
        assert!(
            !code.contains(bad),
            "V0414 出现了 {bad}：底稿金额必须 NUMERIC(18,4)（与凭证分录/全仓金额列同口径）"
        );
    }

    // 列数：14 列是契约（迁移自检里也数了这个数，两处必须一致）
    let declared_columns = [
        "id UUID",
        "period_code VARCHAR(7)",
        "line_type VARCHAR(8)",
        "item_code VARCHAR(32)",
        "item_name VARCHAR(64)",
        "activity VARCHAR(12)",
        "direction VARCHAR(8)",
        "amount NUMERIC(18,4)",
        "sort_order INT",
        "remark VARCHAR(255)",
        "create_by UUID",
        "created_at TIMESTAMPTZ",
        "update_by UUID",
        "updated_at TIMESTAMPTZ",
    ];
    for col in declared_columns {
        assert_has(&sql, col, &format!("缺少设计列 {col}"));
    }
}

/// ④ 7 条 CHECK 一条都不能少（漏一条 = 一条脏数据入口）。
#[test]
fn v0414_keeps_all_seven_check_constraints() {
    let sql = read_migration(V0414_UP);

    for (snippet, why) in [
        (
            "CONSTRAINT ck_biz_cashflow_ws_line_type CHECK (line_type IN ('FLOW', 'OPENING', 'CLOSING'))",
            "三种行必须用 CHECK 锁死（多一种行 = 报表聚合口径未定义）",
        ),
        (
            "CONSTRAINT ck_biz_cashflow_ws_period CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$')",
            "期间格式必须有正则 CHECK：'2026-1' / '2026-13' 的行永远聚合不进任何一期（静默丢数）",
        ),
        (
            "CONSTRAINT ck_biz_cashflow_ws_activity CHECK (activity IS NULL OR activity IN ('OPERATING', 'INVESTING', 'FINANCING'))",
            "活动必须三选一（余额行为 NULL）",
        ),
        (
            "CONSTRAINT ck_biz_cashflow_ws_direction CHECK (direction IS NULL OR direction IN ('INFLOW', 'OUTFLOW'))",
            "方向必须二选一（余额行为 NULL）",
        ),
        (
            "CONSTRAINT ck_biz_cashflow_ws_flow_amount CHECK (line_type <> 'FLOW' OR amount >= 0)",
            "明细行金额必须 ≥ 0：用负数表达流出会让流入/流出两栏互相抵消",
        ),
        (
            "CONSTRAINT ck_biz_cashflow_ws_name CHECK (length(btrim(item_name)) > 0)",
            "项目名不许为空/全空白（空名字的底稿行在报表上就是一条看不见的金额）",
        ),
    ] {
        assert_has(&sql, snippet, why);
    }

    // 第 7 条是跨字段形状断言：两条腿都要在（明细行齐全 / 余额行全空）
    assert_has(
        &sql,
        "(line_type = 'FLOW' AND item_code IS NOT NULL AND activity IS NOT NULL AND direction IS NOT NULL)",
        "ck_biz_cashflow_ws_flow_shape 的第一支：明细行必须三要素齐全",
    );
    assert_has(
        &sql,
        "(line_type <> 'FLOW' AND item_code IS NULL AND activity IS NULL AND direction IS NULL)",
        "ck_biz_cashflow_ws_flow_shape 的第二支：余额行必须三要素全空",
    );

    let check_count = squash(&sql).matches("CONSTRAINT ck_biz_cashflow_ws_").count();
    assert_eq!(
        check_count, 7,
        "本表应有 7 条 ck_biz_cashflow_ws_* 约束，实际 {check_count} 条"
    );
}

/// ⑤ 两条唯一索引必须是**部分**唯一索引，两条普通索引各管一个取数入口。
#[test]
fn v0414_unique_indexes_are_partial() {
    let sql = read_migration(V0414_UP);

    assert_has(
        &sql,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cashflow_ws_item ON biz_cashflow_worksheet (period_code, item_code) WHERE item_code IS NOT NULL",
        "缺 upsert 键 (period_code, item_code) 的部分唯一索引：同期间同项目两行 = 报表把同一笔现金流算两遍",
    );
    assert_has(
        &sql,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cashflow_ws_balance ON biz_cashflow_worksheet (period_code, line_type) WHERE line_type <> 'FLOW'",
        "缺「每期间一份期初/期末」的部分唯一索引：余额行 item_code 为 NULL，普通唯一约束挡不住重复（PG 允许多个 NULL）",
    );
    assert_has(
        &sql,
        "CREATE INDEX IF NOT EXISTS idx_biz_cashflow_ws_period ON biz_cashflow_worksheet (period_code, sort_order)",
        "缺底稿列表取数索引 (period_code, sort_order)",
    );
    assert_has(
        &sql,
        "CREATE INDEX IF NOT EXISTS idx_biz_cashflow_ws_item_code ON biz_cashflow_worksheet (item_code) WHERE item_code IS NOT NULL",
        "缺按项目跨期取数索引（M1-16 报表「上期金额」、底稿复核）",
    );

    let code = squash(&sql_only(&sql));
    let unique = code.matches("CREATE UNIQUE INDEX IF NOT EXISTS").count();
    let plain = code.matches("CREATE INDEX IF NOT EXISTS").count();
    assert_eq!(unique, 2, "本表应有 2 条部分唯一索引，实际 {unique} 条");
    assert_eq!(plain, 2, "本表应有 2 条普通索引，实际 {plain} 条");

    // 反证：不许出现不带 IF NOT EXISTS 的索引语句（那样重放/演练补跑会 already exists 中断）
    let total_index_statements = code.matches("CREATE UNIQUE INDEX").count() + code.matches("CREATE INDEX").count();
    assert_eq!(
        total_index_statements,
        unique + plain,
        "仓库里有 {} 处 CREATE [UNIQUE] INDEX 没带 IF NOT EXISTS（重放不安全）",
        total_index_statements - (unique + plain)
    );
}

/// ⑥ 弱引用：不许给 period_code / item_code 加物理外键。
#[test]
fn v0414_has_no_physical_foreign_keys() {
    let code = squash(&sql_only(&read_migration(V0414_UP)));
    assert!(
        !code.contains("REFERENCES"),
        "V0414 出现了 REFERENCES：期间/项目编码必须弱引用 —— 外键会让期间反结账、分类配置换版这类\n\
         正常运维动作被底稿卡住（AGENTS.md 弱引用口径：全仓 120 条外键里指向期间表的是 0 条）"
    );
}

/// ⑦ 两条口径「不加」的决定反向钉死：没有币种列、没有 deleted_at。
///
/// 这两条都是**决定**（写进迁移头注释）：
/// · 底稿只出本位币（外币已在记账时折为本位币），加币种列会引出「原币还是本位币」的二义；
/// · 底稿行是可重算的工作底稿、不是单据，留墓碑行会造出「这行已删、报表还算着」的双真相。
#[test]
fn v0414_has_no_currency_column_and_no_soft_delete() {
    let code = squash(&sql_only(&read_migration(V0414_UP)));
    for forbidden in ["currency_code", "deleted_at"] {
        assert!(
            !code.contains(forbidden),
            "V0414 的建表语句里出现了 {forbidden}：这是**决定不做**的列（理由见迁移头注释）。\n\
             要改先改决定与测试，别顺手加"
        );
    }
}

/// ⑧ 自检块：每个 `DO $$` 块都有 `RAISE EXCEPTION`，关键断言逐条点名，探针自清场。
#[test]
fn v0414_self_check_raises_exceptions() {
    let sql = read_migration(V0414_UP);
    let blocks: Vec<&str> = sql.split("DO $$").skip(1).collect();
    assert_eq!(
        blocks.len(),
        2,
        "V0414 应有 2 个 DO 块（前置护栏 / 结构+行为自检），实际 {} 个",
        blocks.len()
    );
    for (i, block) in blocks.iter().enumerate() {
        let has_raise = block.contains("RAISE EXCEPTION") || block.contains("RAISE NOTICE");
        assert!(
            has_raise,
            "第 {} 个 DO 块既没有 RAISE EXCEPTION 也没有 RAISE NOTICE —— 自检失败也拦不住迁移",
            i + 1
        );
    }

    for msg in [
        "biz_cashflow_item 不存在，请先应用 V0185",
        "biz_accounting_period 不存在",
        "biz_cashflow_worksheet 应有 14 列",
        "七个关键列的类型/精度/可空性不对",
        "7 条 CHECK 只建成",
        "4 条索引只建成",
        "两条唯一索引必须是 UNIQUE ... WHERE ... 的部分索引",
        "同期间同项目的第二行竟然写进去了",
        "负数流入行竟然写进去了",
        "缺 activity/direction 的明细行竟然写进去了",
        "同期间第二个期初余额竟然写进去了",
        "非法 line_type 竟然写进去了",
        "非法期间 2026-13 竟然写进去了",
        "空项目名竟然写进去了",
        "探针行残留",
    ] {
        assert!(sql.contains(msg), "自检断言里必须覆盖「{msg}」（当前找不到）");
    }

    // 探针行必须删干净：迁移重复执行不许往真库里攒垃圾行
    assert!(
        squash(&sql_only(&sql)).contains("DELETE FROM biz_cashflow_worksheet WHERE period_code = '1900-01'"),
        "探针行必须按 period_code='1900-01'（永不使用的期间）删掉，重放零残留"
    );
    assert!(
        sql.contains("V0414 自检通过"),
        "自检块必须以完成提示收口（否则是被截断的最后一块）"
    );
}

/// ⑨ down 幂等 + 有底稿行时拒绝回滚 + 结尾断言零残留。
#[test]
fn v0414_down_is_idempotent_and_guarded() {
    let down = read_migration(V0414_DOWN);

    assert_has(
        &down,
        "V0414 down 拒绝执行：biz_cashflow_worksheet 已有",
        "down 必须有「已有底稿行就拒绝回滚」的护栏（底稿是编制结果，删表无法从别的表还原）",
    );
    assert_has(
        &down,
        "DROP TABLE IF EXISTS biz_cashflow_worksheet",
        "down 必须用 DROP TABLE IF EXISTS 收口（重复执行不报错）",
    );

    let code = sql_only(&down);
    // 不许用 'tbl'::regclass 这种「表已删就抛错」的写法
    assert!(
        !code.contains(&format!("'{OWN_TABLE}'::regclass")),
        "down 里出现了 '{OWN_TABLE}'::regclass：第二次执行时表已不存在，这种写法会直接抛错（改用 to_regclass）"
    );
    assert!(
        squash(&code).contains("RAISE NOTICE 'V0414 down 完成"),
        "down 结尾必须有自检提示：删完要断言表与索引确实没了"
    );
    assert!(
        squash(&code).contains("仍然存在"),
        "down 自检必须断言「表仍然存在」这种失败态（只 NOTICE 不 EXCEPTION 等于没验）"
    );
}

/// ⑩ 引擎拆分器必须把每个 `DO $$ … $$` 各留成一条语句，且建表/索引条数正确。
///
/// 用框架自己的 `split_statements`（引擎执行迁移走的就是它）验证：`$$` 内部的 `;`
/// 若被当成语句分隔符，PG 会报语法错误、整个迁移事务回滚 ⇒ 服务起不来。
#[test]
fn v0414_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    let sql = read_migration(V0414_UP);
    let statements =
        split_statements(&sql, DialectKind::Postgres).expect("引擎拆分器无法拆分 V0414（方言感知的 $$ 处理失败）");

    let do_blocks: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
    assert_eq!(
        do_blocks.len(),
        2,
        "两个 DO 块必须各被拆成**一条**语句（实际 {} 条）：>2 说明 $$ 内部的 `;` 被当成分隔符了",
        do_blocks.len()
    );
    for block in &do_blocks {
        assert!(
            block.trim_end().ends_with("END $$;") || block.trim_end().ends_with("END $$"),
            "拆分后的 DO 块必须以 END $$ 收口（被截断就会语法错误）"
        );
    }

    let create_table = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE TABLE"))
        .count();
    assert_eq!(create_table, 1, "建表语句应成一条，实际 {create_table} 条");

    let create_unique_index = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE UNIQUE INDEX"))
        .count();
    assert_eq!(
        create_unique_index, 2,
        "两条部分唯一索引应各成一条，实际 {create_unique_index} 条"
    );

    let create_index = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE INDEX"))
        .count();
    assert_eq!(create_index, 2, "两条普通索引应各成一条，实际 {create_index} 条");

    let comments = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("COMMENT ON"))
        .count();
    assert_eq!(comments, 9, "1 条表注释 + 8 条列注释应各成一条，实际 {comments} 条");
}
