//! 迁移 `V0406`（M1-01：多币种存储层第一步 —— `biz_account.currency_code` +
//! `biz_voucher.currency_code` / `exchange_rate`）的静态不变量。
//!
//! 卡：t_76e55ffd（M1-01【M1·财务】多币种：迁移 biz_account 加 currency_code +
//! biz_voucher 加 currency/exchange_rate）。拆卡依据
//! `docs/planning/20261113-新卡清单.md` §4 M1 财务 的 M1-01 行。
//!
//! # 这层测试钉死什么
//!
//! 迁移是**一次性、不可回改**的东西（`schema_migrations` 记内容指纹，改一个字节已升级的
//! 机器就起不来），写错了只有下次部署才炸。这里把「写对了的形状」固化到 `cargo test`，
//! 覆盖的都是会**静默算错账**的类型：
//!
//! 1. **精度被写窄**（`exchange_rate NUMERIC(18,6)` / 金额意义的列用 `NUMERIC(18,2)`）⇒
//!    JPY→CNY 这类 0.04x 汇率丢有效位，月末调汇的折算差额变成无来源尾差。
//! 2. **币种列用 `CHAR(3)`** ⇒ 跨表 UNION / JOIN 带定长空格语义，与
//!    `biz_currency.code VARCHAR(8)` 比较要隐式转换（V0314 已把 `biz_receivable`
//!    从 `CHAR(3)` 放宽过，不许倒退）。
//! 3. **列名写成卡面的 `currency`** ⇒ 全仓 12 处同类列都是 `currency_code`，两套列名
//!    会让下游 M1-05 调汇 / M1-09 报表折算每次写错一次（列名不参与类型检查，只在
//!    运行期报 `column does not exist`）。偏差与理由写在迁移头注释里，这里反向钉死。
//! 4. **列可空或缺默认值** ⇒ 存量 83 行科目 / 历史凭证回填不到 CNY，折算链上取到 NULL，
//!    汇总静默丢行。
//! 5. **缺 `exchange_rate > 0`** ⇒ rate = 0 的凭证折算后本位币金额全 0（账面金额凭空消失）。
//! 6. **回填写成无条件全表 UPDATE** ⇒ 重放时会把业务已经写进去的外币行改成 CNY。
//! 7. **自检断言被删** ⇒ 迁移「跑过了但没验过」，等于没有自检。
//! 8. **语句被引擎拆分器切坏**（`DO $$ … $$` 里含分号）⇒ PG 报语法错误、整个迁移事务
//!    回滚、服务起不来。这条用引擎自己的 `split_statements` 验。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 的
//! `migration_versions_are_unique_padded_and_have_down` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up×2 / down×2 / 回滚重放 / 护栏反证）的实测结果见卡内汇报。

use std::path::PathBuf;

/// `backend/app` 目录（测试 crate 的 manifest 目录就是迁移根所在层）。
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

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材（比如「卡面写的列名是
/// `currency`」），「禁止出现」类断言必须只看真正会执行的 SQL。
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
/// 所以块首往往是注释而不是 `DO` / `ALTER`。
fn sql_head(statement: &str) -> String {
    statement
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("--"))
        .unwrap_or("")
        .to_ascii_uppercase()
}

const V0406_UP: &str = "business/V0406__account_voucher_currency.up.sql";
const V0406_DOWN: &str = "business/V0406__account_voucher_currency.down.sql";

/// 本卡只允许碰这两张表（范围纪律：M1-02 的 `biz_exchange_rate`、M1-03 的科目标记
/// 各有自己的卡，动同一张表会撞车）。
const OWN_TABLES: [&str; 2] = ["biz_account", "biz_voucher"];

/// 从 SQL 里取 `ALTER TABLE <t>` 的目标表名。
fn alter_targets(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = sql;
    while let Some(pos) = rest.find("ALTER TABLE") {
        let after = &rest[pos + "ALTER TABLE".len()..];
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
    out
}

/// ① up / down 必须成对存在（缺 down = 回滚无路）。
#[test]
fn v0406_up_and_down_exist() {
    for rel in [V0406_UP, V0406_DOWN] {
        let path = app_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ② 只加三列，且必须 `ADD COLUMN IF NOT EXISTS`（幂等）。
///
/// 多一列就是越界（M1-03/M1-04 各有自己的卡）；少一个 `IF NOT EXISTS`，
/// 回滚后重放 / 演习库补跑就会 `column already exists` 中断。
#[test]
fn v0406_adds_exactly_three_columns_idempotently() {
    let sql = sql_only(&read_migration(V0406_UP));
    let squashed = squash(&sql);

    for (table, column, decl) in [
        (
            "biz_account",
            "currency_code",
            "ALTER TABLE biz_account ADD COLUMN IF NOT EXISTS currency_code VARCHAR(8) NOT NULL DEFAULT 'CNY'",
        ),
        (
            "biz_voucher",
            "currency_code",
            "ALTER TABLE biz_voucher ADD COLUMN IF NOT EXISTS currency_code VARCHAR(8) NOT NULL DEFAULT 'CNY'",
        ),
        (
            "biz_voucher",
            "exchange_rate",
            "ALTER TABLE biz_voucher ADD COLUMN IF NOT EXISTS exchange_rate NUMERIC(18,8) NOT NULL DEFAULT 1",
        ),
    ] {
        assert_has(
            &sql,
            decl,
            &format!("{table}.{column} 的加列语句形状不符（必须是 IF NOT EXISTS + NOT NULL + DEFAULT）"),
        );
    }

    // 精确数「加列动作」：只数 ADD COLUMN IF NOT EXISTS（自检 NOTICE 里的「ADD COLUMN」是文案，不是动作）
    let add_column_count = squashed.matches("ADD COLUMN IF NOT EXISTS").count();
    assert_eq!(
        add_column_count, 3,
        "本迁移只许加 3 列（biz_account.currency_code / biz_voucher.currency_code / \
         biz_voucher.exchange_rate），实际 {add_column_count} 处 ADD COLUMN IF NOT EXISTS —— 多出来的要么越界到 \
         M1-03（科目外币标记）/ M1-04（凭证本位币金额），要么重复加列"
    );

    // 一处 CREATE TABLE 都不许有：本卡是「给存量表加列」，不是建表
    assert!(
        !squashed.contains("CREATE TABLE"),
        "V0406 不许建表 —— 本卡只有加列，建表属别的卡"
    );
    // 也不许删表 / 删数据
    for forbidden in ["DROP TABLE", "TRUNCATE"] {
        assert!(
            !squashed.contains(forbidden),
            "V0406 出现 {forbidden}：迁移只加列，不许删结构或清数据"
        );
    }
}

/// ③ 类型 / 精度 / 默认值 / 可空性 —— 与 V0314（销售链双币）逐字同口径。
#[test]
fn v0406_types_follow_repo_wide_currency_convention() {
    let sql = read_migration(V0406_UP);

    // 币种列：varchar(8) 而不是 CHAR(3)（见文件头 2.）
    assert_has(
        &sql,
        "currency_code VARCHAR(8) NOT NULL DEFAULT 'CNY'",
        "币种列必须是 VARCHAR(8) NOT NULL DEFAULT 'CNY'（与 biz_currency.code 同型；CHAR(3) 会带定长空格语义）",
    );
    // 汇率精度：18,8（与 biz_exchange_rate.rate / V0314 同精度）
    assert_has(
        &sql,
        "exchange_rate NUMERIC(18,8) NOT NULL DEFAULT 1",
        "汇率必须是 NUMERIC(18,8) NOT NULL DEFAULT 1（写窄会让 0.04x 量级汇率丢有效位）",
    );

    // 反向：空上不许出现窄精度 / 定长字符的币种或汇率列
    let code = squash(&sql_only(&sql));
    for bad in [
        "currency_code CHAR(3)",
        "currency_code CHAR(8)",
        "exchange_rate NUMERIC(18,6)",
        "exchange_rate NUMERIC(18,4)",
        "exchange_rate NUMERIC(20,8)",
    ] {
        assert!(
            !code.contains(bad),
            "V0406 出现了 {bad}：与 V0314 / biz_exchange_rate 的口径不一致（见文件头「类型与精度」）"
        );
    }

    // 列名口径：卡面写 `currency`，本卡取 `currency_code`（全仓 12 处同类列同写法）。
    // 反向断言：代码里不许出现裸 `currency` 列（`currency_code` 不算，按带分隔符匹配）。
    for bad in [
        "ADD COLUMN IF NOT EXISTS currency ",
        "ADD COLUMN IF NOT EXISTS currency(",
    ] {
        assert!(
            !code.contains(bad),
            "V0406 出现了裸列名 `currency`（{bad}）：必须与全仓口径一致写 `currency_code`"
        );
    }
}

/// ④ 三条 CHECK 一条都不能少（漏一条 = 一条脏数据入口）。
#[test]
fn v0406_keeps_all_three_check_constraints() {
    let sql = read_migration(V0406_UP);

    assert_has(
        &sql,
        "ADD CONSTRAINT ck_biz_account_currency_code CHECK (currency_code ~ '^[A-Z]{3}$')",
        "biz_account 的币种码正则 CHECK 必须与 ck_biz_currency_code 同口径（^[A-Z]{3}$）",
    );
    assert_has(
        &sql,
        "ADD CONSTRAINT ck_biz_voucher_currency_code CHECK (currency_code ~ '^[A-Z]{3}$')",
        "biz_voucher 的币种码正则 CHECK 必须与 ck_biz_currency_code 同口径（^[A-Z]{3}$）",
    );
    assert_has(
        &sql,
        "ADD CONSTRAINT ck_biz_voucher_exchange_rate CHECK (exchange_rate > 0)",
        "凭证汇率必须 > 0：rate = 0 会让折算后金额全 0，负汇率会造出反向金额",
    );

    // 每一条都必须「先查 pg_constraint 再 ADD」（重放安全）
    let code = squash(&sql_only(&sql));
    let guard_count = code.matches("SELECT 1 FROM pg_constraint").count();
    assert!(
        guard_count >= 3,
        "三条 CHECK 都必须有「先查 pg_constraint 再 ADD」的守卫（重放安全），实际只有 {guard_count} 处"
    );

    // 反向：不许给币种列加跨表断言（CHECK 不能跨表；币种存在性由写入口保证）
    assert!(
        !code.contains("REFERENCES biz_currency"),
        "V0406 不许给币种列加外键：与 V0314 销售链四表同口径（弱引用），\
         外键会把币种主数据变成单据表的删除依赖"
    );
}

/// ⑤ 两条索引：科目币种索引 + 外币凭证部分索引（M1-05 调汇 / M1-08 外币明细账的取数入口）。
#[test]
fn v0406_creates_two_indexes_with_partial_predicate() {
    let sql = sql_only(&read_migration(V0406_UP));

    assert_has(
        &sql,
        "CREATE INDEX IF NOT EXISTS idx_biz_account_currency ON biz_account (currency_code)",
        "缺 idx_biz_account_currency（M1-05 按币种取外币科目）",
    );
    assert_has(
        &sql,
        "CREATE INDEX IF NOT EXISTS idx_biz_voucher_currency ON biz_voucher (currency_code, voucher_date DESC) WHERE currency_code <> 'CNY'",
        "缺 idx_biz_voucher_currency 或部分索引谓词不对：\
         外币明细账按「币种 + 日期倒序」取数，且只该收外币行（CNY 占绝大多数，收进索引纯属负担）",
    );

    let count = squash(&sql).matches("CREATE INDEX").count();
    assert_eq!(
        count, 2,
        "本迁移应有 2 条 CREATE INDEX（科目 / 凭证各一条），实际 {count} 条"
    );
}

/// ⑥ 回填必须带 `IS NULL` 谓词 —— 不许无条件全表 UPDATE。
///
/// 无条件 `UPDATE biz_voucher SET currency_code = 'CNY'` 在**重放**时会把业务已经写进去的
/// 外币行改成 CNY：汇率锁定信息被静默抹平，而且没有任何报错（最危险的一类迁移 bug）。
#[test]
fn v0406_backfill_is_null_guarded_and_never_clobbers_foreign_rows() {
    let sql = read_migration(V0406_UP);
    let code = squash(&sql_only(&sql));

    // 三处回填：两张表币种 + 凭证汇率
    let backfills = [
        "UPDATE biz_account SET currency_code = 'CNY', updated_at = now() WHERE currency_code IS NULL OR btrim(currency_code) = ''",
        "UPDATE biz_voucher SET currency_code = 'CNY', updated_at = now() WHERE currency_code IS NULL OR btrim(currency_code) = ''",
        "UPDATE biz_voucher SET exchange_rate = 1, updated_at = now() WHERE exchange_rate IS NULL",
    ];
    for snippet in backfills {
        assert_has(
            &sql,
            snippet,
            "回填语句形状不符（必须带 IS NULL / 空串谓词，重放才不覆盖外币行）",
        );
    }

    // 反向：每条 UPDATE 必须在同一语句里带空值守卫；回填（不带探针号）恰好 3 条、且都是回填形状
    let mut rest = code.as_str();
    let mut backfill = 0;
    let mut probe = 0;
    while let Some(pos) = rest.find("UPDATE ") {
        let after = &rest[pos..];
        let stmt_end = after.find(';').unwrap_or(after.len());
        let stmt = &after[..stmt_end];
        assert!(
            stmt.contains("IS NULL") || stmt.contains("IS DISTINCT FROM"),
            "找到一条没有空值守卫的 UPDATE（重放会覆盖业务数据）：{stmt}"
        );
        if stmt.contains("V0406PROBE") {
            probe += 1;
        } else {
            backfill += 1;
        }
        rest = &after[stmt_end..];
    }
    assert_eq!(
        backfill, 3,
        "回填 UPDATE 应为 3 条（两张表币种 + 凭证汇率），实际 {backfill} 条"
    );
    assert_eq!(
        probe, 4,
        "自检探针里的 UPDATE 应为 4 条（默认可写、汇率 0 / 负数 / 非法币种被拒、外币行可写），实际 {probe} 条"
    );
}

/// ⑦ 只许碰 biz_account / biz_voucher 两张表。
#[test]
fn v0406_alters_only_owned_tables() {
    let code = sql_only(&read_migration(V0406_UP));
    let targets = alter_targets(&code);
    assert!(!targets.is_empty(), "迁移里应该有 ALTER TABLE（本卡是给存量表加列）");
    for table in targets {
        assert!(
            OWN_TABLES.contains(&table.as_str()),
            "V0406 改了 {table} —— 本卡只允许碰 biz_account / biz_voucher（biz_exchange_rate 属 M1-02，\
             科目外币标记属 M1-03）"
        );
    }
}

/// ⑧ 自检写进迁移：每个 `DO $$` 块都有 `RAISE EXCEPTION`，且关键断言逐条点名。
#[test]
fn v0406_self_check_raises_exceptions() {
    let sql = read_migration(V0406_UP);
    let blocks: Vec<&str> = sql.split("DO $$").skip(1).collect();
    assert_eq!(
        blocks.len(),
        4,
        "V0406 应有 4 个 DO 块（前置护栏 / 约束守卫 / 存量回填 / 自检），实际 {} 个",
        blocks.len()
    );
    for (i, block) in blocks.iter().enumerate() {
        assert!(
            block.contains("RAISE EXCEPTION"),
            "第 {} 个 DO 块没有任何 RAISE EXCEPTION —— 自检失败也拦不住迁移",
            i + 1
        );
    }

    for msg in [
        "biz_account 不存在，请先应用 V0137",
        "biz_voucher 不存在，请先应用 V0149",
        "biz_account.currency_code 不存在",
        "biz_account.currency_code 类型 =",
        "exchange_rate 精度 =",
        "币种 / 汇率 CHECK 只有",
        "三条币种 / 汇率 CHECK 只建成",
        "索引只有",
        "仍有 % 行 currency_code 为空",
        "ck_biz_account_currency_code 没生效",
        "ck_biz_voucher_currency_code 没生效",
        "ck_biz_voucher_exchange_rate 没生效",
        "新建科目的默认币种 =",
        "新建凭证的默认汇率 =",
        "外币凭证汇率落库 =",
        "凭证探针行残留",
        "科目探针行残留",
    ] {
        assert!(sql.contains(msg), "自检断言里必须覆盖「{msg}」（当前找不到）");
    }

    // 探针行必须删干净：迁移重复执行不许往真库里攒垃圾行
    let code = squash(&sql_only(&sql));
    assert!(
        code.contains("DELETE FROM biz_voucher WHERE voucher_no = 'V0406PROBE'"),
        "凭证探针行必须按 voucher_no='V0406PROBE' 删掉（重放零残留）"
    );
    assert!(
        code.contains("DELETE FROM biz_account WHERE account_code = 'V0406PROBE'"),
        "科目探针行必须按 account_code='V0406PROBE' 删掉（重放零残留）"
    );
}

/// ⑨ down 幂等 + 有外币数据时拒绝回滚 + 结尾断言零残留。
#[test]
fn v0406_down_is_idempotent_and_guarded() {
    let down = read_migration(V0406_DOWN);

    // 护栏：已有外币数据时必须拒绝（回滚会永久丢掉币种/汇率信息）
    assert_has(
        &down,
        "V0406 down 拒绝执行：biz_account 已有",
        "down 必须有「已有外币科目就拒绝回滚」的护栏（这三列是业务信息，不是纯结构）",
    );
    assert_has(
        &down,
        "V0406 down 拒绝执行：biz_voucher 已有",
        "down 必须有「已有外币凭证就拒绝回滚」的护栏",
    );

    // 幂等：三列 / 三条约束 / 两条索引全部 IF EXISTS
    for snippet in [
        "ALTER TABLE biz_account DROP COLUMN IF EXISTS currency_code",
        "ALTER TABLE biz_voucher DROP COLUMN IF EXISTS currency_code",
        "ALTER TABLE biz_voucher DROP COLUMN IF EXISTS exchange_rate",
        "ALTER TABLE biz_account DROP CONSTRAINT IF EXISTS ck_biz_account_currency_code",
        "ALTER TABLE biz_voucher DROP CONSTRAINT IF EXISTS ck_biz_voucher_currency_code",
        "ALTER TABLE biz_voucher DROP CONSTRAINT IF EXISTS ck_biz_voucher_exchange_rate",
        "DROP INDEX IF EXISTS idx_biz_account_currency",
        "DROP INDEX IF EXISTS idx_biz_voucher_currency",
    ] {
        assert_has(&down, snippet, "down 必须用 IF EXISTS 收口（重复执行不报错）");
    }

    // 不许用 'tbl'::regclass 这种「列已删就抛错」的写法
    let code = sql_only(&down);
    for table in OWN_TABLES {
        assert!(
            !code.contains(&format!("'{table}'::regclass")),
            "down 里出现了 '{table}'::regclass：第二次执行时列已不存在，这种写法会直接抛错（改用 information_schema）"
        );
    }

    // 结尾自检
    assert!(
        squash(&code).contains("RAISE NOTICE 'V0406 down 完成"),
        "down 结尾必须有自检提示：删完要断言三列 / 三条约束 / 两条索引确实没了"
    );
}

/// ⑩ 引擎拆分器必须把每个 `DO $$ … $$` 各留成一条语句。
///
/// 用框架自己的 `split_statements`（引擎执行迁移走的就是它）验证：`$$` 内部的 `;`
/// 若被当成语句分隔符，PG 会报语法错误、整个迁移事务回滚 ⇒ 服务起不来。
#[test]
fn v0406_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    let sql = read_migration(V0406_UP);
    let statements =
        split_statements(&sql, DialectKind::Postgres).expect("引擎拆分器无法拆分 V0406（方言感知的 $$ 处理失败）");

    let do_blocks: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
    assert_eq!(
        do_blocks.len(),
        4,
        "四个 DO 块必须各被拆成**一条**语句（实际 {} 条）：>4 说明 $$ 内部的 `;` 被当成分隔符了",
        do_blocks.len()
    );
    for block in &do_blocks {
        assert!(
            block.contains("RAISE EXCEPTION") || block.contains("RAISE NOTICE"),
            "拆分后的 DO 块必须完整"
        );
        assert!(
            block.trim_end().ends_with("END $$;") || block.trim_end().ends_with("END $$"),
            "拆分后的 DO 块必须以 END $$ 收口（被截断就会语法错误）"
        );
    }
    let last = do_blocks.last().unwrap();
    assert!(
        last.contains("V0406 自检通过"),
        "行为自检块必须以完成提示收口（否则是被截断的最后一块）"
    );

    let alter_count = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("ALTER TABLE"))
        .count();
    assert_eq!(alter_count, 3, "三条 ADD COLUMN 应各成一条语句，实际 {alter_count} 条");

    let create_index_count = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE") && sql_head(s).contains("INDEX"))
        .count();
    assert_eq!(
        create_index_count, 2,
        "两条 CREATE INDEX 应各成一条语句，实际 {create_index_count} 条"
    );
}
