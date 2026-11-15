//! 迁移 `V0408`（M1-03：科目表加「是否外币核算」标记 + 一个维护开关的权限码）的静态不变量。
//!
//! 卡：t_52b02f6a（M1-03【M1·财务】多币种：科目表加「是否外币核算」标记 —— DTO + handler + 页面开关）。
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §4 M1 财务 的 M1-03 行。
//! 上游：M1-01（V0406 `biz_account.currency_code`）/ M1-02（V0407 汇率区间）。
//!
//! # 这层测试钉死什么
//!
//! 迁移是**一次性、不可回改**的东西（`schema_migrations` 记内容指纹，改一个字节已升级的
//! 机器就起不来），写错了只有下次部署才炸。这里把「写对了的形状」固化到 `cargo test`：
//!
//! 1. **列型写成 BOOLEAN / INT / VARCHAR** ⇒ 同表的 `is_leaf` / `status` 是 `SMALLINT 0/1`，
//!    半张表两套布尔写法，M1-05/M1-07 每次都要记哪个是哪个（DTO 的 `== 1` 转换也会漏）。
//! 2. **列可空或缺默认值** ⇒ 存量科目 100% 落在 NULL，「是不是外币核算」既不是「是」也不是「否」，
//!    三处取数（调汇 / 外币余额表 / 外币明细账）全部漏判。
//! 3. **只加「取值 0/1」一条 CHECK，漏掉「仅末级科目」那条** ⇒ 给汇总科目打标记后，
//!    M1-05 会扫出金额恒 0 的调汇凭证；上级与下级都标了就是同一笔外币余额调两次。
//! 4. **少一条 `IF NOT EXISTS` / 约束没先查 `pg_constraint`** ⇒ 回滚后重放 / 演习库补跑中断。
//! 5. **部分索引谓词写错（`= 1` 写成 `<> 0` 或漏 WHERE）** ⇒ 索引收进全部科目，等于白建；
//!    更糟的是 `<> 0` 在 NULL 行上不成立，将来放宽可空时索引会静默漏行。
//! 6. **权限码没进 `sys_menu`** ⇒ 该码授不出去（除超管 `*:*:*` 通配），财务经理点了开关必然 403
//!    —— `scripts/verify/perm_menu_gate.py` 判红的正是这一类「死码」。
//! 7. **down 没有数据护栏** ⇒ 回滚把「哪些科目是外币核算」永久丢掉（重放 up 只填回 0）。
//! 8. **语句被引擎拆分器切坏**（`DO $$ … $$` 里含分号）⇒ PG 报语法错误、整个迁移事务回滚、
//!    服务起不来。这条用引擎自己的 `split_statements` 验。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 的
//! `migration_versions_are_unique_padded_and_have_down` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（克隆库真引擎升级 / 空库首启 / 真 HTTP 开关）的实测结果见卡内汇报与
//! `docs/evidence/2026-11-16-m103-account-foreign-currency/`。

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

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材，「禁止出现」类断言必须只看真正会执行的 SQL。
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
fn sql_head(statement: &str) -> String {
    statement
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("--"))
        .unwrap_or("")
        .to_ascii_uppercase()
}

const V0408_UP: &str = "business/V0408__account_foreign_currency.up.sql";
const V0408_DOWN: &str = "business/V0408__account_foreign_currency.down.sql";

/// 本卡只允许改这张业务表的**结构**（菜单/授权表是权限承载，不算业务表结构）。
const OWN_TABLE: &str = "biz_account";

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
fn v0408_up_and_down_exist() {
    for rel in [V0408_UP, V0408_DOWN] {
        let path = app_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ② 只加一列，且 `ADD COLUMN IF NOT EXISTS` + `SMALLINT NOT NULL DEFAULT 0`。
#[test]
fn v0408_adds_exactly_one_column_idempotently() {
    let sql = sql_only(&read_migration(V0408_UP));
    let squashed = squash(&sql);

    assert_has(
        &sql,
        "ALTER TABLE biz_account ADD COLUMN IF NOT EXISTS is_foreign_currency SMALLINT NOT NULL DEFAULT 0",
        "is_foreign_currency 的加列语句形状不符（必须 IF NOT EXISTS + SMALLINT + NOT NULL + DEFAULT 0）",
    );

    let add_column_count = squashed.matches("ADD COLUMN IF NOT EXISTS").count();
    assert_eq!(
        add_column_count, 1,
        "本迁移只许加 1 列（biz_account.is_foreign_currency），实际 {add_column_count} 处 —— \
         多出来的要么越界到 M1-04（凭证本位币金额）/ M1-06（汇兑损益科目），要么重复加列"
    );

    for forbidden in ["CREATE TABLE", "DROP TABLE", "TRUNCATE", "DROP COLUMN"] {
        assert!(
            !squashed.contains(forbidden),
            "V0408（up）出现 {forbidden}：本卡只加一列 + 一条权限码，不建表不删结构不清数据"
        );
    }
}

/// ③ 列型必须是 SMALLINT（与同表 `is_leaf` / `status` 同口径），默认值必须是 0。
#[test]
fn v0408_column_type_follows_table_convention() {
    let raw = read_migration(V0408_UP);
    let code = squash(&sql_only(&raw));

    assert_has(
        &raw,
        "is_foreign_currency SMALLINT NOT NULL DEFAULT 0",
        "列必须是 SMALLINT NOT NULL DEFAULT 0",
    );

    // 反向：不许出现别的类型写法（BOOLEAN / INTEGER / SMALLINT(1) / VARCHAR / CHAR）
    //
    // 注意 `is_foreign_currency INTO v_flag`（自检里的 SELECT … INTO）**不是**类型声明，
    // 所以 `INT` 这条必须带尾空格，否则会把 `INTO` 误判成 `INT`（第一版就栽在这儿）。
    for bad in [
        "is_foreign_currency BOOLEAN",
        "is_foreign_currency INTEGER",
        "is_foreign_currency INT ",
        "is_foreign_currency SMALLINT(1)",
        "is_foreign_currency VARCHAR",
        "is_foreign_currency CHAR",
        "is_foreign_currency NUMERIC",
        // 可空写法也是错的：NULL 既不是「是」也不是「否」
        "is_foreign_currency SMALLINT DEFAULT 0",
        "is_foreign_currency SMALLINT NULL",
    ] {
        assert!(
            !code.contains(bad),
            "V0408 出现了 {bad}：与同表 is_leaf / status 的「SMALLINT NOT NULL DEFAULT」口径不一致"
        );
    }

    // 默认值只能是 0（默认 1 = 所有科目默认按外币记账，是最坏的默认）
    assert!(
        !code.contains("DEFAULT 1"),
        "V0408 出现了 DEFAULT 1：外币核算标记的默认值必须是 0（存量账套都是人民币账）"
    );
}

/// ④ 两条 CHECK 一条都不能少，且都必须「先查 pg_constraint 再 ADD」。
#[test]
fn v0408_keeps_both_check_constraints() {
    let raw = read_migration(V0408_UP);

    assert_has(
        &raw,
        "ADD CONSTRAINT ck_biz_account_foreign_currency CHECK (is_foreign_currency IN (0, 1))",
        "缺「取值只能是 0/1」的 CHECK",
    );
    assert_has(
        &raw,
        "ADD CONSTRAINT ck_biz_account_fc_leaf CHECK (is_foreign_currency = 0 OR is_leaf = 1)",
        "缺「只有末级科目可标记外币核算」的 CHECK —— 这是本卡唯一会静默算错账的入口\
         （汇总科目被标记 ⇒ M1-05 调汇金额恒 0；父子都标 ⇒ 同一笔余额调两次）",
    );

    let code = squash(&sql_only(&raw));
    let guard_count = code.matches("SELECT 1 FROM pg_constraint").count();
    assert!(
        guard_count >= 2,
        "两条 CHECK 都必须有「先查 pg_constraint 再 ADD」的守卫（重放安全），实际只有 {guard_count} 处"
    );

    // 反向：不许给这一位加跨表断言（CHECK 不能跨表）
    assert!(
        !code.contains("REFERENCES biz_account"),
        "V0408 不许加外键：parent_id 一直是弱引用（V0137 口径），加 FK 会让科目树维护变脆"
    );
}

/// ⑤ 一条部分索引，谓词必须是 `is_foreign_currency = 1`（不是 `<> 0`、不是无条件）。
#[test]
fn v0408_creates_partial_index_for_foreign_accounts() {
    let raw = read_migration(V0408_UP);
    let code = sql_only(&raw);

    assert_has(
        &code,
        "CREATE INDEX IF NOT EXISTS idx_biz_account_foreign_currency ON biz_account (account_code) WHERE is_foreign_currency = 1",
        "缺 idx_biz_account_foreign_currency 或谓词不对：M1-05 期末调汇 / M1-07 外币余额表按\
         「标了外币的科目」取数，部分索引只该收那几行",
    );

    let count = squash(&code).matches("CREATE INDEX").count();
    assert_eq!(
        count, 1,
        "本迁移应有 1 条 CREATE INDEX（科目外币部分索引），实际 {count} 条"
    );

    // 反向：谓词不许写成 <> 0 / != 0（NULL 行上不成立，将来放宽可空时会静默漏行）
    assert!(
        !squash(&code).contains("is_foreign_currency <> 0"),
        "索引谓词不许写 <> 0：与查询条件 `is_foreign_currency = 1` 不同形，规划器用不上"
    );
}

/// ⑥ 只许改 `biz_account` 的结构（菜单 / 授权表只 INSERT/DELETE，不进 ALTER TABLE）。
#[test]
fn v0408_alters_only_biz_account() {
    let code = sql_only(&read_migration(V0408_UP));
    let targets = alter_targets(&code);
    assert!(!targets.is_empty(), "迁移里应该有 ALTER TABLE（本卡是给存量表加列）");
    for table in targets {
        assert_eq!(
            table, OWN_TABLE,
            "V0408 改了 {table} —— 本卡只允许碰 biz_account（biz_voucher 属 M1-01/M1-04，\
             biz_exchange_rate 属 M1-02）"
        );
    }
}

/// ⑦ 权限码必须真的种进 `sys_menu`（否则授不出去 = 除超管没人能开开关），
///    并带幂等（`ON CONFLICT DO NOTHING` / 授权 `NOT EXISTS`）。
#[test]
fn v0408_seeds_permission_menu_row_idempotently() {
    let raw = read_migration(V0408_UP);
    let code = sql_only(&raw);

    assert_has(
        &code,
        "'business:erp:account:update', NULL, '20000000-0000-0000-0000-000000000950'::uuid",
        "权限码行必须挂在「会计科目」页面（…0e02/…0950 是 V0141 种的那一行）下",
    );
    assert_has(
        &code,
        "ON CONFLICT (id) DO NOTHING",
        "菜单行必须用固定 id + ON CONFLICT DO NOTHING（重放不覆盖运维改过的排序 / 标题）",
    );
    assert_has(
        &code,
        "INSERT INTO sys_role_menu (id, role_id, menu_id) SELECT gen_random_uuid(), r.id",
        "授权必须走 SELECT + NOT EXISTS 去重（空库首启时 sys_role 为空 ⇒ 空操作，可重放补齐）",
    );
    assert_has(
        &code,
        "AND NOT EXISTS ( SELECT 1 FROM sys_role_menu rm",
        "授权去重条件缺失：重放会给同一角色授两次（角色树上出现重复节点）",
    );

    // 菜单行的三段恒定值：type=3（按钮）/ 不显形 / 状态 1
    assert_has(
        &code,
        "'business:erp:account:update', 3,",
        "权限码行必须是 menu_type = 3（按钮），写成 2 会多出一个侧栏页面",
    );
    let squashed = squash(&code);
    assert!(
        squashed.matches("FALSE, 'business')").count() >= 1,
        "按钮行 `is_visible` 必须是 FALSE（按钮不进侧栏）"
    );

    // 反向：不许把按钮行做成可见页面（is_visible TRUE + component 非 NULL）
    assert!(
        !squashed.contains("'科目外币核算标记'")
            || !squashed.contains("TRUE, 'business'), ('20000000-0000-0000-0000-000000001720'"),
        "按钮行不许做成可见页面"
    );
}

/// ⑧ 自检写进迁移：每个 `DO $$` 块都有 `RAISE EXCEPTION`，关键断言逐条点名，探针自清场。
#[test]
fn v0408_self_check_raises_exceptions_and_cleans_up() {
    let raw = read_migration(V0408_UP);
    let blocks: Vec<&str> = raw.split("DO $$").skip(1).collect();
    assert_eq!(
        blocks.len(),
        3,
        "V0408 应有 3 个 DO 块（前置护栏 / 约束守卫 / 自检），实际 {} 个",
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
        "两条外币标记 CHECK 只建成",
        "is_foreign_currency 可空",
        "is_foreign_currency 类型 =",
        "is_foreign_currency 默认值 =",
        "定义里没有 is_foreign_currency",
        "idx_biz_account_foreign_currency 缺失或谓词",
        "有 % 行 is_foreign_currency 为 NULL",
        "权限码 business:erp:account:update 的菜单按钮行不对",
        "新建科目的默认 is_foreign_currency =",
        "ck_biz_account_foreign_currency 没生效（值 2 写入成功）",
        "ck_biz_account_foreign_currency 没生效（值 -1 写入成功）",
        "is_foreign_currency 可写 NULL",
        "末级科目标记外币核算后读回 =",
        "ck_biz_account_fc_leaf 没生效（非末级科目标记外币核算成功）",
        "ck_biz_account_fc_leaf 没生效（带外币标记的科目被改成非末级）",
        "关闭外币核算失败",
        "科目探针行残留",
    ] {
        assert!(raw.contains(msg), "自检断言里必须覆盖「{msg}」（当前找不到）");
    }

    // 探针行必须删干净：迁移重复执行不许往真库里攒垃圾行
    let code = squash(&sql_only(&raw));
    assert!(
        code.contains("DELETE FROM biz_account WHERE account_code LIKE 'V0408PROBE%'"),
        "科目探针行必须按 account_code LIKE 'V0408PROBE%' 删掉（重放零残留）"
    );
}

/// ⑨ down：有外币科目就拒绝回滚 + 全 `IF EXISTS` + 结尾自检 + 不用 `::regclass` 写法。
#[test]
fn v0408_down_is_idempotent_and_guarded() {
    let down = read_migration(V0408_DOWN);

    assert_has(
        &down,
        "V0408 down 拒绝执行：biz_account 已有",
        "down 必须有「已有外币核算科目就拒绝回滚」的护栏（这一位是人工业务信息，删了重放也填不回来）",
    );
    assert_has(
        &down,
        "WHERE is_foreign_currency = 1",
        "护栏必须只看「标记为外币」的行（全部行都拦会让 down 永远跑不了）",
    );

    // 护栏自己也要幂等：列可能已经不存在（第二次 down / 手工删过列），必须先判列在不在再查值。
    // 反例（本卡实测踩到）：直接 `SELECT count(*) FROM biz_account WHERE is_foreign_currency = 1`
    // 在第二次 down 时抛 `column "is_foreign_currency" does not exist` —— 回滚路径因为
    // 「已经回滚过」而失败，是最难排查的一类（只有连跑两次才暴露）。
    assert_has(
        &down,
        "SELECT 1 FROM information_schema.columns",
        "数据护栏必须先判列存在（否则第二次 down 直接报 column does not exist）",
    );
    assert_has(
        &down,
        "AND column_name = 'is_foreign_currency'",
        "数据护栏的列存在判定必须点名 is_foreign_currency",
    );

    for snippet in [
        "DROP INDEX IF EXISTS idx_biz_account_foreign_currency",
        "ALTER TABLE biz_account DROP CONSTRAINT IF EXISTS ck_biz_account_fc_leaf",
        "ALTER TABLE biz_account DROP CONSTRAINT IF EXISTS ck_biz_account_foreign_currency",
        "ALTER TABLE biz_account DROP COLUMN IF EXISTS is_foreign_currency",
        "DELETE FROM sys_role_menu",
        "DELETE FROM sys_menu",
    ] {
        assert_has(
            &down,
            snippet,
            "down 必须用 IF EXISTS / 明确 DELETE 收口（重复执行不报错）",
        );
    }

    // 顺序：授权 → 菜单 → 索引 → 约束 → 列（列最后，它被约束与索引依赖）
    let code = sql_only(&down);
    let pos = |needle: &str| code.find(needle).unwrap_or_else(|| panic!("down 缺少 {needle}"));
    assert!(
        pos("DELETE FROM sys_role_menu") < pos("DELETE FROM sys_menu"),
        "先拆授权再拆菜单（反了会留下指向不存在菜单的授权行）"
    );
    assert!(
        pos("DROP INDEX IF EXISTS idx_biz_account_foreign_currency")
            < pos("ALTER TABLE biz_account DROP COLUMN IF EXISTS is_foreign_currency"),
        "先拆索引再拆列（列被索引表达式依赖时顺序反了会抛错）"
    );

    // 不许用 'tbl'::regclass 这种「列已删就抛错」的写法
    assert!(
        !code.contains(&format!("'{OWN_TABLE}'::regclass")),
        "down 里出现了 '{OWN_TABLE}'::regclass：第二次执行时列已不存在，这种写法会直接抛错（改用 information_schema）"
    );

    assert!(
        squash(&code).contains("RAISE NOTICE 'V0408 down 完成"),
        "down 结尾必须有自检提示：删完要断言列 / 两条约束 / 索引 / 菜单行确实没了"
    );
}

/// ⑩ 引擎拆分器必须把每个 `DO $$ … $$` 各留成一条语句。
#[test]
fn v0408_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    let raw = read_migration(V0408_UP);
    let statements =
        split_statements(&raw, DialectKind::Postgres).expect("引擎拆分器无法拆分 V0408（方言感知的 $$ 处理失败）");

    let do_blocks: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
    assert_eq!(
        do_blocks.len(),
        3,
        "三个 DO 块必须各被拆成**一条**语句（实际 {} 条）：>3 说明 $$ 内部的 `;` 被当成分隔符了",
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
        last.contains("V0408 自检通过"),
        "行为自检块必须以完成提示收口（否则是被截断的最后一块）"
    );

    let alter_count = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("ALTER TABLE"))
        .count();
    assert_eq!(alter_count, 1, "一条 ADD COLUMN 应各成一条语句，实际 {alter_count} 条");

    let create_index_count = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE") && sql_head(s).contains("INDEX"))
        .count();
    assert_eq!(
        create_index_count, 1,
        "一条 CREATE INDEX 应各成一条语句，实际 {create_index_count} 条"
    );

    // 权限码与授权两条 INSERT 必须完整（被切坏 = 授权语句残缺，症状是「谁都授不出去」）
    let insert_count = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("INSERT INTO SYS_"))
        .count();
    assert_eq!(
        insert_count, 2,
        "应有 2 条 INSERT（sys_menu 权限行 + sys_role_menu 授权），实际 {insert_count} 条"
    );
}
