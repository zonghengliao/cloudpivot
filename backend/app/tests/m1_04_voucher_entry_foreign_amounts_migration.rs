//! 迁移 `V0409`（M1-04：凭证分录加「原币金额」两列）的静态不变量。
//!
//! 卡：t_b95e935d（M1-04【M1·财务】多币种：凭证录入支持外币金额 + 汇率 + 本位币换算）。
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §4 M1 财务 的 M1-04 行。
//! 上游：M1-01（V0406 单头 `currency_code` / `exchange_rate`）、M1-02（V0407 汇率区间）、
//! M1-03（V0408 `biz_account.is_foreign_currency`）。
//!
//! # 这层测试钉死什么（每一条都对应一个「上线才发现」的错法）
//!
//! 1. **列写成 `NOT NULL DEFAULT 0`** ⇒ 本仓有 11 处裸 SQL 写 `biz_voucher_entry`（年结结转 /
//!    报表夹具 / 期间夹具 / 成本 / 佣金 / 期初），漏改一处就是运行期 500；而且「原币 0 + 本位币
//!    720」这种自相矛盾的行会被写进去。正确形态是**可空、无默认**：`NULL` = 本位币分录。
//! 2. **错误地做数据回填**（把存量行 `fc = debit/credit`）⇒ 账套本位币一旦不是 CNY，历史行就
//!    自动变成「外币行」，M1-05 会对它们做调汇，凭空生损益。本迁移必须**零回填**。
//! 3. **CHECK 少写 `IS NOT NULL` 守卫** ⇒ PG 的 CHECK 在求值为 NULL 时**放行**，「只写一半原币」
//!    那一支正好求值成 NULL（本次实现里真的漏过一次，演练库 P3 探针抓到的）。所以断言里必须有
//!    `IS NOT NULL` 两次、且三列都被引用。
//! 4. **约束没先查 `pg_constraint`** ⇒ 回滚后重放 / 演习库补跑中断。
//! 5. **部分索引谓词写错（漏 WHERE / 写成 `<> 0`）** ⇒ 索引收进全部本位币分录，等于白建；
//!    索引必须与「外币凭证」这一取数形态同形（`fc_debit_amount IS NOT NULL`）。
//! 6. **down 没有数据护栏** ⇒ 回滚把原币金额（用户录入的业务事实）整列删掉，重放 up 只能得到 NULL。
//! 7. **语句被引擎拆分器切坏**（`DO $$ … $$` 里含分号）⇒ PG 报语法错误、整个迁移事务回滚。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（psql up/down 幂等 + 约束反证）见 `scripts/verify/m104_voucher_fx_drill.sh`，
//! 真引擎升级 / 空库首启 / 真 HTTP 见 `scripts/verify/m104_engine_and_api.sh`。

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

const V0409_UP: &str = "business/V0409__voucher_entry_foreign_amounts.up.sql";
const V0409_DOWN: &str = "business/V0409__voucher_entry_foreign_amounts.down.sql";

/// 本卡只允许改这张业务表的**结构**。
const OWN_TABLE: &str = "biz_voucher_entry";

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
fn v0409_up_and_down_exist() {
    for rel in [V0409_UP, V0409_DOWN] {
        let path = app_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ② 两列都必须 `ADD COLUMN IF NOT EXISTS`（重放 / 演习库补跑安全），且**不许**带 NOT NULL / DEFAULT。
#[test]
fn v0409_adds_two_nullable_columns_idempotently() {
    let code = sql_only(&read_migration(V0409_UP));
    let squashed = squash(&code);
    assert_eq!(
        squashed.matches("ADD COLUMN IF NOT EXISTS").count(),
        2,
        "本卡只加两列，且都必须带 IF NOT EXISTS（重放安全）"
    );
    for (column, kind) in [("fc_debit_amount", "借方"), ("fc_credit_amount", "贷方")] {
        assert_has(
            &squashed,
            &format!("ADD COLUMN IF NOT EXISTS {column}  NUMERIC(20,4) NULL"),
            &format!("{kind}原币列必须是 NUMERIC(20,4) 且可空（NULL = 本位币分录）"),
        );
    }
    assert!(
        !squashed.contains("NOT NULL DEFAULT 0"),
        "原币列**不许**写成 NOT NULL DEFAULT 0：11 处裸 SQL 写分录，漏改一处就 500；\
         而且「原币 0 / 本位币 720」自相矛盾"
    );
}

/// ③ 零数据回填：本迁移**不得**出现任何 `UPDATE biz_voucher_entry`（回填会污染历史本位币凭证）。
#[test]
fn v0409_does_not_backfill_any_row() {
    let squashed = squash(&sql_only(&read_migration(V0409_UP)));
    assert!(
        !squashed.to_uppercase().contains("UPDATE BIZ_VOUCHER_ENTRY"),
        "本迁移必须零回填（NULL 语义天然幂等）：写出 UPDATE 就会把存量本位币行标成外币行"
    );
}

/// ④ CHECK 必须是「成对 / 同边 / 非负」一条表达，而且**必须有 IS NOT NULL 守卫**。
///
/// PG 的 CHECK 在求值为 NULL 时放行：没有守卫时「只写一半原币」正好求值成 NULL ⇒ 脏数据进库。
#[test]
fn v0409_check_guards_null_pairs() {
    let raw = read_migration(V0409_UP);
    let code = sql_only(&raw);
    assert_has(
        &code,
        "ck_biz_voucher_entry_fc_side",
        "约束名必须固定（自检块与 down 都按它点名）",
    );
    assert_has(
        &code,
        "FROM pg_constraint WHERE conname = 'ck_biz_voucher_entry_fc_side'",
        "加约束前必须先查 pg_constraint（重放安全）",
    );
    let guard_count = code.matches("fc_debit_amount IS NOT NULL").count();
    assert!(
        guard_count >= 1,
        "CHECK 里必须有 fc_debit_amount IS NOT NULL 守卫（否则半对原币会因 NULL 求值被放行）"
    );
    assert_has(
        &code,
        "fc_credit_amount IS NOT NULL",
        "CHECK 里必须有 fc_credit_amount IS NOT NULL 守卫（同一理由）",
    );
    for column in ["fc_debit_amount", "fc_credit_amount", "debit_amount", "credit_amount"] {
        assert_has(
            &code,
            column,
            "CHECK 必须同时引用原币两列与本位币两列（同边约束的判据）",
        );
    }
}

/// ⑤ 部分索引：只收外币分录，谓词与查询同形。
#[test]
fn v0409_creates_partial_index_for_fc_entries() {
    let code = sql_only(&read_migration(V0409_UP));
    assert_has(
        &code,
        "CREATE INDEX IF NOT EXISTS idx_biz_voucher_entry_fc",
        "索引必须带 IF NOT EXISTS（重放安全）",
    );
    assert_has(
        &code,
        "WHERE fc_debit_amount IS NOT NULL",
        "谓词必须是 fc_debit_amount IS NOT NULL（与 M1-05 调汇 / M1-07 余额表的取数形态同形）",
    );
    assert!(
        !squash(&code).contains("WHERE fc_debit_amount <> 0"),
        "谓词不能写 <> 0：NULL 行在 `<> 0` 下不成立，将来放宽语义会静默漏行"
    );
}

/// ⑥ 只动 `biz_voucher_entry` 这一张业务表（夹具表不算业务表结构）。
#[test]
fn v0409_alters_only_voucher_entry() {
    let code = sql_only(&read_migration(V0409_UP));
    for target in alter_targets(&code) {
        assert_eq!(
            target, OWN_TABLE,
            "本卡只允许改 {OWN_TABLE} 的结构，实际还动了 {target}"
        );
    }
    // down 也只拆这张表
    let down_code = sql_only(&read_migration(V0409_DOWN));
    for target in alter_targets(&down_code) {
        assert_eq!(target, OWN_TABLE, "down 只该拆 {OWN_TABLE}，实际还动了 {target}");
    }
}

/// ⑦ 自检块：每个 `DO $$` 都要有 `RAISE EXCEPTION` 形式的失败出口，且探针自清场。
#[test]
fn v0409_self_check_raises_and_cleans_up() {
    let raw = read_migration(V0409_UP);
    let blocks: Vec<&str> = raw.split("DO $$").skip(1).collect();
    assert_eq!(
        blocks.len(),
        3,
        "应当有 3 个 DO 块（前置护栏 / 加约束 / 自检），实际 {}",
        blocks.len()
    );
    for (index, block) in blocks.iter().enumerate() {
        assert!(
            block.contains("RAISE EXCEPTION"),
            "第 {} 个 DO 块没有失败出口（出了问题会静默通过）",
            index + 1
        );
    }
    assert_has(
        &raw,
        "RAISE NOTICE 'V0409 自检通过",
        "自检通过必须打 NOTICE（真引擎日志要能看见）",
    );
    assert_has(
        &raw,
        "DELETE FROM biz_voucher_entry WHERE voucher_id = v_voucher",
        "行为探针必须自清场（重放零残留）",
    );
    assert_has(
        &raw,
        "DELETE FROM biz_voucher WHERE voucher_no = 'V0409PROBE'",
        "探针凭证必须自清场（否则每跑一次多一张 1999 年的凭证）",
    );
}

/// ⑧ down 必须有数据护栏，并且先拆索引 / 约束再拆列。
#[test]
fn v0409_down_guards_data_and_orders_drops() {
    let raw = read_migration(V0409_DOWN);
    let code = sql_only(&raw);
    assert_has(
        &code,
        "V0409 down 拒绝执行",
        "有外币分录时 down 必须 RAISE EXCEPTION（原币金额是业务事实，不是结构）",
    );
    let index_at = code
        .find("DROP INDEX IF EXISTS idx_biz_voucher_entry_fc")
        .expect("down 必须拆索引");
    let check_at = code
        .find("DROP CONSTRAINT IF EXISTS ck_biz_voucher_entry_fc_side")
        .expect("down 必须拆 CHECK");
    let column_at = code
        .find("DROP COLUMN IF EXISTS fc_debit_amount")
        .expect("down 必须拆原币列");
    assert!(
        index_at < check_at && check_at < column_at,
        "down 顺序必须是「索引 → 约束 → 列」（反了会报「列被依赖」）"
    );
    assert_has(&raw, "RAISE NOTICE 'V0409 down 完成", "down 结尾必须有收尾自检提示");
}

/// ⑨ 引擎拆分器必须把每个 `DO $$ … $$` 各留成一条语句。
#[test]
fn v0409_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    let raw = read_migration(V0409_UP);
    let statements =
        split_statements(&raw, DialectKind::Postgres).expect("引擎拆分器无法拆分 V0409（方言感知的 $$ 处理失败）");

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
            "拆分后的 DO 块必须完整（块体被截断会让自检变成空跑）"
        );
    }
    // 两列 / 索引 / 约束都必须出现在拆分后的语句里（拆分器把某句吞了会静默漏结构）
    let joined = statements.join("\n");
    for needle in [
        "fc_debit_amount",
        "fc_credit_amount",
        "idx_biz_voucher_entry_fc",
        "ck_biz_voucher_entry_fc_side",
    ] {
        assert!(joined.contains(needle), "拆分后的语句里找不到 {needle}");
    }
}
