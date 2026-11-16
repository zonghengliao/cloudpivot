//! 迁移 `V0432`（M3-01：销售报价单表结构 `biz_quotation` + `biz_quotation_line`）的静态不变量。
//!
//! 卡：t_6a61726c（M3-01【M3·销售】报价单：表结构 biz_quotation + biz_quotation_line）。
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §「M3 销售（26 卡）」M3-01 行；
//! 下游 M3-02（CRUD + 状态机）、M3-03（转销售订单，带行）、M3-04（审批流接线）、
//! M3-05（单测）、M3-06（列表/详情页）、M3-07（PDF）、M3-24（全链 E2E）都按本表的契约写 SQL。
//!
//! # 这层测试钉死什么
//!
//! 迁移是**一次性、不可回改**的东西（`schema_migrations` 记内容指纹，改一个字节
//! 已升级的机器就起不来），写错了只有下次部署才炸。这里把「写对了的形状」固化到
//! `cargo test`，覆盖的都是会**静默算错价/算错期/转单转两张**的类型：
//!
//! 1. **有效期两列被放空/可空** ⇒ 客户永远拿旧价压价，「过期」没有依据；
//!    且 `valid_until >= quote_date` 若不在库层，建单当天就过期的单只会在客户砍价时暴露。
//! 2. **有效期被做成状态（如 EXPIRED）** ⇒ 需要定时翻转 + 时钟回拨对账，且「过期后又被接受」
//!    会造出逆向迁移边。过期必须是时间事实（现算 `CURRENT_DATE > valid_until`）。
//! 3. **价税合计恒等退化成注释** ⇒ 程序算错时库里留着 100 + 13 = 112 这种单，报表对不平。
//! 4. **转单回链只留一列** ⇒ 「转过了但不知何时」，且 M3-03 没有幂等判据，点两次出两张订单。
//! 5. **单号唯一索引退化成普通索引/不带 `WHERE deleted_at IS NULL`** ⇒ 要么重号能进，
//!    要么软删后单号不能复用（草案单删了重开被「单号已存在」挡住）。
//! 6. **行号不唯一** ⇒ M3-03 的「订单行 ↔ 报价行」对应键多义。
//! 7. **给 `customer_id` / `material_id` / `converted_so_id` 加物理外键** ⇒ 客户/物料软删后
//!    历史报价失去可读性，转单事务成环（AGENTS.md 弱引用口径）；只有明细→单头是 CASCADE 硬外键。
//! 8. **本卡越界**：建了别的表 / 动了 `biz_customer`、`biz_sales_order`、`biz_seq` 的存量结构。
//! 9. **自检断言被删** ⇒ 迁移「跑过了但没验过」，等于没有自检。
//! 10. **语句被引擎拆分器切坏**（`DO $$ … $$` 里含分号）⇒ PG 报语法错误、整个迁移事务回滚、服务起不来。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 的
//! `migration_versions_are_unique_padded_and_have_down` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up×2 / down×2 / 护栏反证 / 行为探针 / 转单唯一）的实测见
//! `bash scripts/verify/m301_sales_quotation_drill.sh`（56 条断言）。

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

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材（比如「有效期不能做成状态」），
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
/// 引擎的 `split_statements` 把注释**原样留在语句块里**，所以块首往往是注释而不是 `DO` / `CREATE`。
fn sql_head(statement: &str) -> String {
    statement
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("--"))
        .unwrap_or("")
        .to_ascii_uppercase()
}

const V0432_UP: &str = "business/V0432__sales_quotation.up.sql";
const V0432_DOWN: &str = "business/V0432__sales_quotation.down.sql";

const OWN_TABLES: [&str; 2] = ["biz_quotation", "biz_quotation_line"];

/// 本卡唯一允许 DML 的**存量**表：取号规则行（`INSERT INTO biz_seq … WHERE NOT EXISTS`）。
/// 探针**不碰** `biz_customer` / `biz_material`（两列都是弱引用，探针用随机 UUID 就够）。
const PROBE_FIXTURE_TABLES: [&str; 1] = ["biz_seq"];

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
fn v0432_up_and_down_exist() {
    for rel in [V0432_UP, V0432_DOWN] {
        let path = app_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ② 只建这两张表，且建表语句必须幂等；DML 只许打在自己的表与 biz_seq 上。
#[test]
fn v0432_creates_exactly_two_tables_idempotently() {
    let sql = sql_only(&read_migration(V0432_UP));
    let squashed = squash(&sql);

    for table in OWN_TABLES {
        assert_has(
            &sql,
            &format!("CREATE TABLE IF NOT EXISTS {table} ("),
            &format!("建表语句必须是 `CREATE TABLE IF NOT EXISTS {table}`（幂等 + 表名固定）"),
        );
    }

    let create_table_count = squashed.matches("CREATE TABLE").count();
    assert_eq!(
        create_table_count, 2,
        "本卡只许建 2 张表（{OWN_TABLES:?}），实际 {create_table_count} 处 CREATE TABLE —— 多出来的越界了"
    );

    // 不许改 / 删存量表：本卡是「纯新建」，客户/订单/取号规则的结构都归别的卡
    for forbidden in ["ALTER TABLE", "DROP TABLE", "TRUNCATE"] {
        assert!(
            !squashed.contains(forbidden),
            "V0432 出现 {forbidden}：本卡只新建两张表，改/删存量表属别的卡"
        );
    }

    // DML 目标：自己的两张表 + biz_seq 取号行，别的表一律越界
    for table in dml_targets(&sql) {
        let allowed = OWN_TABLES.contains(&table.as_str()) || PROBE_FIXTURE_TABLES.contains(&table.as_str());
        assert!(
            allowed,
            "V0432 的 DML 打到了 {table} —— 本卡只许写 {OWN_TABLES:?} 与取号表 {PROBE_FIXTURE_TABLES:?}（别去动客户/订单存量数据）"
        );
    }
}

/// ③ 头表 17 列逐列形状：长度 / 精度 / 可空性 / 默认值。
#[test]
fn v0432_header_columns_have_designed_shape() {
    let sql = read_migration(V0432_UP);

    for (snippet, why) in [
        (
            "quote_no VARCHAR(32) NOT NULL",
            "quote_no 必须 VARCHAR(32) NOT NULL（QT-yyyyMMdd-000001 单号列，与 sales_order.so_no 同宽度）",
        ),
        (
            "customer_id UUID NOT NULL",
            "customer_id 必须 UUID NOT NULL（卡面口径：报价单必须挂在客户上）",
        ),
        (
            "quote_date DATE NOT NULL",
            "quote_date 必须 DATE NOT NULL（有效期区间的起点）",
        ),
        (
            "valid_until DATE NOT NULL",
            "valid_until 必须 DATE NOT NULL（卡面点名「含有效期」；可空 = 客户永远拿旧价压价）",
        ),
        (
            "status VARCHAR(24) NOT NULL DEFAULT 'DRAFT'",
            "status 必须 VARCHAR(24) NOT NULL DEFAULT 'DRAFT'（业务状态列用 VARCHAR 枚举，与销售订单同口径）",
        ),
        (
            "total_amount NUMERIC(18,4) NOT NULL DEFAULT 0",
            "total_amount 必须 NUMERIC(18,4) NOT NULL DEFAULT 0（与 biz_sales_order 同口径，转单整列搬不出尾差）",
        ),
        (
            "total_tax NUMERIC(18,4) NOT NULL DEFAULT 0",
            "total_tax 必须 NUMERIC(18,4) NOT NULL DEFAULT 0",
        ),
        (
            "total_with_tax NUMERIC(18,4) NOT NULL DEFAULT 0",
            "total_with_tax 必须 NUMERIC(18,4) NOT NULL DEFAULT 0",
        ),
        (
            "converted_so_id UUID",
            "converted_so_id 必须 UUID 且可空（M3-03 转单回填；未转单时是 NULL）",
        ),
        (
            "converted_at TIMESTAMPTZ",
            "converted_at 必须 TIMESTAMPTZ 且可空（与 converted_so_id 成对）",
        ),
        (
            "deleted_at TIMESTAMPTZ",
            "deleted_at 必须可空（软删；单号唯一索引靠它做部分索引）",
        ),
    ] {
        assert_has(&sql, snippet, why);
    }

    // 反向：金额精度不许写窄到 2 位、也不许放宽成 6 位（会与订单/应收口径对不上）
    let code = squash(&sql_only(&sql));
    for bad in [
        "total_amount NUMERIC(18,2)",
        "total_tax NUMERIC(18,2)",
        "total_with_tax NUMERIC(18,2)",
        "total_with_tax NUMERIC(18,6)",
        "total_amount NUMERIC(12,2)",
    ] {
        assert!(
            !code.contains(bad),
            "V0432 出现了 {bad}：三个合计金额列必须 NUMERIC(18,4)（与 biz_sales_order 同口径）"
        );
    }

    // 列数：17 列是契约（迁移自检里也数了这个数，两处必须一致）
    let declared_columns = [
        "id UUID",
        "quote_no VARCHAR(32)",
        "customer_id UUID",
        "quote_date DATE",
        "valid_until DATE",
        "status VARCHAR(24)",
        "total_amount NUMERIC(18,4)",
        "total_tax NUMERIC(18,4)",
        "total_with_tax NUMERIC(18,4)",
        "converted_so_id UUID",
        "converted_at TIMESTAMPTZ",
        "remark TEXT",
        "created_at TIMESTAMPTZ",
        "updated_at TIMESTAMPTZ",
        "create_by UUID",
        "update_by UUID",
        "deleted_at TIMESTAMPTZ",
    ];
    assert_eq!(declared_columns.len(), 17, "头表设计列数必须是 17");
    for col in declared_columns {
        assert_has(&sql, col, &format!("缺少设计列 {col}"));
    }
}

/// ④ 行表 12 列逐列形状（单价 6 位小数 / 金额 4 位 / 税率 7,4）。
#[test]
fn v0432_line_columns_have_designed_shape() {
    let sql = read_migration(V0432_UP);

    for (snippet, why) in [
        (
            "quotation_id UUID NOT NULL",
            "quotation_id 必须 UUID NOT NULL（明细必须挂在单头上）",
        ),
        (
            "line_no INTEGER NOT NULL",
            "line_no 必须 INTEGER NOT NULL（行号 1 起，单内唯一）",
        ),
        (
            "material_id UUID NOT NULL",
            "material_id 必须 UUID NOT NULL（卡面口径：行明细必须有物料）",
        ),
        (
            "qty NUMERIC(18,4) NOT NULL",
            "qty 必须 NUMERIC(18,4) NOT NULL（与 biz_sales_order_item.qty 同口径）",
        ),
        (
            "price NUMERIC(18,6) NOT NULL DEFAULT 0",
            "price 必须 NUMERIC(18,6) NOT NULL DEFAULT 0（与 biz_sales_order_item.price 同精度）",
        ),
        (
            "amount NUMERIC(18,4) NOT NULL DEFAULT 0",
            "amount 必须 NUMERIC(18,4) NOT NULL DEFAULT 0",
        ),
        (
            "tax_rate NUMERIC(7,4) NOT NULL DEFAULT 0",
            "tax_rate 必须 NUMERIC(7,4) NOT NULL DEFAULT 0（百分数 0–100）",
        ),
        (
            "tax_amount NUMERIC(18,4) NOT NULL DEFAULT 0",
            "tax_amount 必须 NUMERIC(18,4) NOT NULL DEFAULT 0",
        ),
    ] {
        assert_has(&sql, snippet, why);
    }

    let code = squash(&sql_only(&sql));
    for bad in [
        "price NUMERIC(18,2)",
        "price NUMERIC(18,4)",
        "qty NUMERIC(18,2)",
        "tax_rate NUMERIC(7,2)",
        "tax_rate NUMERIC(5,2)",
    ] {
        assert!(
            !code.contains(bad),
            "V0432 出现了 {bad}：单价必须 (18,6)、数量/金额 (18,4)、税率 (7,4)（与销售订单行同口径）"
        );
    }
}

/// ⑤ 10 条 CHECK 一条都不能少 —— 尤其「有效期」「价税恒等」「转单成对」三条。
#[test]
fn v0432_keeps_ten_check_constraints() {
    let sql = read_migration(V0432_UP);

    for (snippet, why) in [
        (
            "CONSTRAINT ck_biz_quotation_status CHECK (status IN ('DRAFT','SENT','ACCEPTED','REJECTED','CANCELLED'))",
            "5 个状态必须用 CHECK 锁死（DRAFT/SENT/ACCEPTED/REJECTED + CANCELLED 反向动作）",
        ),
        (
            "CONSTRAINT ck_biz_quotation_valid_until CHECK (valid_until >= quote_date)",
            "有效期必须 >= 报价日（允许当天有效，禁止建单即过期）",
        ),
        (
            "CONSTRAINT ck_biz_quotation_amount CHECK (total_with_tax = total_amount + total_tax)",
            "价税合计必须精确恒等（算错是 400 不是 500，与 ck_sales_order_amount 同口径）",
        ),
        (
            "(converted_so_id IS NULL AND converted_at IS NULL)",
            "ck_biz_quotation_converted 的第一支：未转单时两列都空",
        ),
        (
            "(converted_so_id IS NOT NULL AND converted_at IS NOT NULL)",
            "ck_biz_quotation_converted 的第二支：转过单时两列都有值（成对）",
        ),
        ("CONSTRAINT ck_biz_quotation_line_qty CHECK (qty > 0)", "行数量必须 > 0"),
        (
            "CONSTRAINT ck_biz_quotation_line_rate CHECK (tax_rate >= 0 AND tax_rate <= 100)",
            "税率必须 0–100（百分数口径）",
        ),
        (
            "CONSTRAINT ck_biz_quotation_line_no CHECK (line_no >= 1)",
            "行号必须 >= 1",
        ),
    ] {
        assert_has(&sql, snippet, why);
    }

    let header_checks = squash(&sql).matches("CONSTRAINT ck_biz_quotation_").count();
    let line_checks = squash(&sql).matches("CONSTRAINT ck_biz_quotation_line_").count();
    assert_eq!(
        header_checks - line_checks,
        5,
        "头表应有 5 条 ck_biz_quotation_* 约束（不含 line 前缀），实际 {} 条",
        header_checks - line_checks
    );
    assert_eq!(
        line_checks, 5,
        "行表应有 5 条 ck_biz_quotation_line_* 约束，实际 {line_checks} 条"
    );
}

/// ⑥ 外键口径：只有 `明细 → 单头` 一条 CASCADE；客户/物料/转单回链都是弱引用。
#[test]
fn v0432_fk_is_only_line_to_header_cascade() {
    let sql = read_migration(V0432_UP);

    assert_has(
        &sql,
        "CONSTRAINT fk_biz_quotation_line_quotation FOREIGN KEY (quotation_id) REFERENCES biz_quotation (id) ON DELETE CASCADE",
        "quotation_id 必须是 ON DELETE CASCADE 的硬外键（明细离开单头没有独立意义）",
    );

    let code = squash(&sql_only(&sql));
    // 数 `FOREIGN KEY (` 而不是 `REFERENCES`：自检块里有一句
    // `d NOT LIKE '%REFERENCES biz_quotation(id)%'`（验约束形状的字符串字面量，不是外键）。
    let fk_count = code.matches("FOREIGN KEY (").count();
    assert_eq!(
        fk_count, 1,
        "V0432 只许有 1 条外键（明细→单头）：客户/物料/转单回链是弱引用 —— 外键会让主数据软删后历史报价失去可读性、转单事务成环"
    );
    for (col, why) in [
        ("customer_id", "客户"),
        ("material_id", "物料"),
        ("converted_so_id", "转成的销售订单"),
    ] {
        assert!(
            !code.contains(&format!("FOREIGN KEY ({col})")),
            "{col}（{why}）不许加外键：主数据/后建单据软删后历史报价必须仍可读（AGENTS.md 弱引用口径）",
        );
    }
}

/// ⑦ 索引：3 条唯一（单号部分索引 / 转单反向唯一 / 行号单内唯一）+ 4 条普通，全部幂等。
#[test]
fn v0432_indexes_are_idempotent_and_partial() {
    let sql = read_migration(V0432_UP);

    assert_has(
        &sql,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_quotation_no ON biz_quotation (quote_no) WHERE deleted_at IS NULL",
        "单号唯一必须是「存活行唯一」的部分索引（软删后单号可复用，与 uk_biz_sales_order_no 同口径）",
    );
    assert_has(
        &sql,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_quotation_converted_so ON biz_quotation (converted_so_id) WHERE converted_so_id IS NOT NULL",
        "转单反向唯一：一张销售订单只能由一个报价单认领（M3-03 转单幂等的库层兜底）",
    );
    assert_has(
        &sql,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_quotation_line_no ON biz_quotation_line (quotation_id, line_no)",
        "行号必须单内唯一（M3-03 的「订单行 ↔ 报价行」对应键不许有第二种解释）",
    );
    for snippet in [
        "CREATE INDEX IF NOT EXISTS idx_biz_quotation_customer ON biz_quotation (customer_id, quote_date DESC)",
        "CREATE INDEX IF NOT EXISTS idx_biz_quotation_status ON biz_quotation (status)",
        "CREATE INDEX IF NOT EXISTS idx_biz_quotation_valid_until ON biz_quotation (valid_until)",
        "CREATE INDEX IF NOT EXISTS idx_biz_quotation_line_material ON biz_quotation_line (material_id)",
    ] {
        assert_has(&sql, snippet, "缺少设计索引");
    }

    let code = squash(&sql_only(&sql));
    let unique = code.matches("CREATE UNIQUE INDEX IF NOT EXISTS").count();
    let plain = code.matches("CREATE INDEX IF NOT EXISTS").count();
    assert_eq!(unique, 3, "应有 3 条唯一索引，实际 {unique} 条");
    assert_eq!(plain, 4, "应有 4 条普通索引，实际 {plain} 条");

    // 反证：不许出现不带 IF NOT EXISTS 的索引语句（重放/演练补跑会 already exists 中断）
    let total = code.matches("CREATE UNIQUE INDEX").count() + code.matches("CREATE INDEX").count();
    assert_eq!(
        total,
        unique + plain,
        "仓库里有 {} 处 CREATE [UNIQUE] INDEX 没带 IF NOT EXISTS（重放不安全）",
        total - (unique + plain)
    );
}

/// ⑧ 有效期是**时间事实**不是状态：不许出现 EXPIRED / 过期时间戳列。
///
/// 决定（写进迁移头注释）：过期 = `CURRENT_DATE > valid_until`，由读路径现算。
/// 做成状态就要定时任务翻转 + 时钟回拨对账，且「过期后又被迫接受」会造出逆向迁移边。
#[test]
fn v0432_expiry_is_a_date_fact_not_a_status() {
    let code = squash(&sql_only(&read_migration(V0432_UP)));
    for forbidden in ["EXPIRED", "expired_at", "is_expired"] {
        assert!(
            !code.contains(forbidden),
            "V0432 的执行 SQL 里出现了 {forbidden}：过期是时间事实（现算 CURRENT_DATE > valid_until），不做状态列/标记列"
        );
    }
    // 状态域必须是卡面的 4 态 + 反向动作 CANCELLED
    assert!(
        code.contains("'DRAFT','SENT','ACCEPTED','REJECTED','CANCELLED'"),
        "状态域必须逐字是 DRAFT/SENT/ACCEPTED/REJECTED/CANCELLED（卡面 4 态 + ARCH-7 §12.3 的反向动作）"
    );
}

/// ⑨ 取号规则：QT 种子必须幂等（`WHERE NOT EXISTS`）且前缀/滚动方式明确。
#[test]
fn v0432_seeds_qt_sequence_idempotently() {
    let sql = read_migration(V0432_UP);
    assert_has(
        &sql,
        "SELECT v.id::uuid, 'QT', '报价单号', 'QT', 'yyyyMMdd', '', 0, 1, '-', 1,",
        "必须种 QT 取号规则：QT-yyyyMMdd-000001，按天滚动（与 SO/SH/AR 同口径）",
    );
    assert_has(
        &sql,
        "WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'QT' AND q.deleted_at IS NULL)",
        "QT 取号行必须 WHERE NOT EXISTS（重放不许种出第二行 —— 两行规则会让号段各走一半）",
    );
}

/// ⑩ 自检块：两个 `DO $$` 都有 RAISE，关键断言逐条点名，探针自清场。
#[test]
fn v0432_self_check_raises_exceptions() {
    let sql = read_migration(V0432_UP);
    let blocks: Vec<&str> = sql.split("DO $$").skip(1).collect();
    assert_eq!(
        blocks.len(),
        2,
        "V0432 应有 2 个 DO 块（前置护栏 / 结构+行为自检），实际 {} 个",
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
        "biz_customer 不存在，请先应用 V0260",
        "biz_seq 不存在",
        "biz_quotation / biz_quotation_line 未建全",
        "biz_quotation 应有 17 列",
        "biz_quotation_line 应有 12 列",
        "头表 8 个关键列的类型/精度/可空性不对",
        "行表 8 个关键列的类型/精度/可空性不对",
        "头表 5 条 CHECK 只建成",
        "行表 5 条 CHECK 只建成",
        "明细外键缺失或不是 CASCADE",
        "唯一索引 uk_biz_quotation_no 缺失",
        "uk_biz_quotation_converted_so 必须是部分唯一索引",
        "行号唯一索引 uk_biz_quotation_line_no 缺失",
        "biz_seq(seq_type=QT) 取号规则应恰好 1 行",
        "报价单落库默认值",
        "valid_until 早于 quote_date 未被",
        "valid_until = quote_date（当天有效）被拒了",
        "status = WIN 未被",
        "total_with_tax <> total_amount + total_tax 未被",
        "只填 converted_so_id 不填 converted_at 未被",
        "存活行单号重号未被",
        "软删后同单号仍被拒",
        "line_no = 0 未被",
        "qty = 0 未被",
        "tax_rate = 101 未被",
        "同一报价单内行号重号未被",
        "删报价单后仍剩",
        "头表探针残留",
        "明细探针残留",
    ] {
        assert!(sql.contains(msg), "自检断言里必须覆盖「{msg}」（当前找不到）");
    }

    assert!(
        sql.contains("V0432 自检通过"),
        "自检块必须以完成提示收口（否则是被截断的最后一块）"
    );
}

/// ⑪ 探针必须自清场：`__M301_PROBE_*` 行在块尾全部删除（重放零残留）。
#[test]
fn v0432_probe_fixtures_are_cleaned_up() {
    let sql = sql_only(&read_migration(V0432_UP));
    for cleanup in [
        "DELETE FROM biz_quotation_line WHERE quotation_id IN (probe_q, probe_q2, probe_q3)",
        "DELETE FROM biz_quotation WHERE quote_no LIKE '__M301_PROBE%'",
        "WHERE quote_no LIKE '__M301_PROBE%'",
    ] {
        assert_has(&sql, cleanup, &format!("探针缺少清场语句：{cleanup}"));
    }
    assert!(
        squash(&sql).contains("'__M301_PROBE_Q1__'"),
        "探针单号必须用 __M301_PROBE_* 前缀（撞真数据就麻烦了）"
    );
}

/// ⑫ down 幂等 + 有非作废报价单时拒绝回滚 + 取号行一并回收 + 结尾断言零残留。
#[test]
fn v0432_down_is_idempotent_and_guarded() {
    let down = read_migration(V0432_DOWN);

    assert_has(
        &down,
        "V0432 down 拒绝执行：biz_quotation 还有",
        "down 必须有「还有非作废报价单就拒绝回滚」的护栏（客户认过的价无法从别的表重建）",
    );
    assert_has(
        &down,
        "WHERE status <> 'CANCELLED'",
        "护栏只应拦非作废单：作废单是纯审计痕迹，可以随表删除",
    );
    assert_has(
        &down,
        "DROP TABLE IF EXISTS biz_quotation_line",
        "down 必须幂等删明细表",
    );
    assert_has(&down, "DROP TABLE IF EXISTS biz_quotation", "down 必须幂等删头表");
    assert_has(
        &down,
        "DELETE FROM biz_seq WHERE seq_type = 'QT'",
        "down 必须一并回收 QT 取号行（表都没了，留着规则会让下次重建号段错位）",
    );
    assert_has(
        &down,
        "biz_quotation 不存在，无需回滚（幂等）",
        "down 对「表已不存在」必须先 NOTICE 再跳过（幂等）",
    );

    let code = sql_only(&down);
    for table in OWN_TABLES {
        assert!(
            !code.contains(&format!("'{table}'::regclass")),
            "down 里出现了 '{table}'::regclass：第二次执行时表已不存在，这种写法会直接抛错（改用 to_regclass）"
        );
    }
    let squashed = squash(&code);
    assert!(
        squashed.contains("仍然存在"),
        "down 自检必须断言「表仍然存在」这种失败态（只 NOTICE 不 EXCEPTION 等于没验）"
    );
    assert!(
        squashed.contains("V0432 down 完成"),
        "down 结尾必须有完成提示：删完要断言表/索引/取号行确实没了"
    );
}

/// ⑬ 引擎拆分器必须把每个 `DO $$ … $$` 各留成一条语句，且建表/索引/注释条数正确。
///
/// 用框架自己的 `split_statements`（引擎执行迁移走的就是它）验证：`$$` 内部的 `;`
/// 若被当成语句分隔符，PG 会报语法错误、整个迁移事务回滚 ⇒ 服务起不来。
#[test]
fn v0432_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    let sql = read_migration(V0432_UP);
    let statements =
        split_statements(&sql, DialectKind::Postgres).expect("引擎拆分器无法拆分 V0432（方言感知的 $$ 处理失败）");

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
    assert_eq!(create_table, 2, "两条建表语句应各成一条，实际 {create_table} 条");

    let create_unique_index = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE UNIQUE INDEX"))
        .count();
    assert_eq!(
        create_unique_index, 3,
        "三条唯一索引应各成一条，实际 {create_unique_index} 条"
    );

    let create_index = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE INDEX"))
        .count();
    assert_eq!(create_index, 4, "四条普通索引应各成一条，实际 {create_index} 条");

    let comments = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("COMMENT ON"))
        .count();
    assert_eq!(comments, 17, "2 条表注释 + 15 条列注释应各成一条，实际 {comments} 条");

    let inserts = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("INSERT INTO"))
        .count();
    assert_eq!(inserts, 1, "QT 取号种子应只有 1 条 INSERT，实际 {inserts} 条");
}
