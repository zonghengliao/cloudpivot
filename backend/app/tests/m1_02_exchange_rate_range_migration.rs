//! 迁移 `V0407`（M1-02：多币种第二步 —— `biz_exchange_rate` 加生效区间 `effective_to`
//! + 区间不重叠排除约束）的静态不变量。
//!
//! 卡：t_438a4c0d（M1-02【M1·财务】多币种：迁移 biz_exchange_rate 加生效区间 + 唯一键）。
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §4 M1 财务 的 M1-02 行。
//!
//! # 这层测试钉死什么
//!
//! 迁移一次性、不可回改（`schema_migrations` 记内容指纹，改一个字节已升级的机器就起不来），
//! 写错了只有下次部署才炸。这里把「写对了的形状」固化进 `cargo test`，挑的都是会**静默算错账**
//! 或**静默毁掉现有功能**的写法：
//!
//! 1. **区间右端点写成可空却没约束** ⇒ 止早于起的行能落库，取汇率 WHERE 条件形同虚设。
//! 2. **排除约束少列**（漏 `rate_type`）⇒ SPOT 与 MONTH_END 互相排斥，月末重估直接录不进去；
//!    **多列**（带上 `id`）⇒ 等于不约束。
//! 3. **区间写成左闭右开 `[)`** ⇒ 「月末最后一天失效、次日生效」这套全年无休的录入节奏失效
//!    （必须 `'[]'`）。
//! 4. **约束没标 DEFERRABLE** ⇒ 写入口「先写行、后重算整组区间」的第一步就被拒，汇率维护
//!    整页 500（V0407 头注释决定 2）。
//! 5. **回填写成无条件全表 UPDATE** ⇒ 重放时把写入口已经派生好的区间改成别人算的值。
//! 6. **顺手把 `uk_biz_exchange_rate` 拆了或改列序** ⇒ `rate_upsert` 的 `ON CONFLICT`
//!    目标消失，月度批量录入整批失败（V0272 的 upsert 契约）。
//! 7. **down 先删列后删约束** ⇒ 列被表达式索引依赖，DROP COLUMN 直接报错，回滚卡死。
//! 8. **自检断言被删 / 探针不清场** ⇒ 「跑过了但没验过」，且重放往真库攒垃圾行。
//! 9. **语句被引擎拆分器切坏**（`DO $$ … $$` 里含分号）⇒ PG 语法错误、整笔回滚、服务起不来。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 的
//! `migration_versions_are_unique_padded_and_have_down` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up×2 / 回填逐行对拍 / 4 组行为反证 / down×2 / 重放）见
//! `scripts/verify/m102_exchange_rate_range_drill.sh`。

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

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材（例如「不要写成 `[)`」），
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
fn sql_head(statement: &str) -> String {
    statement
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("--"))
        .unwrap_or("")
        .to_ascii_uppercase()
}

const V0407_UP: &str = "business/V0407__exchange_rate_effective_range.up.sql";
const V0407_DOWN: &str = "business/V0407__exchange_rate_effective_range.down.sql";

/// ① up / down 必须成对存在（缺 down = 回滚无路）。
#[test]
fn v0407_up_and_down_exist() {
    for rel in [V0407_UP, V0407_DOWN] {
        let path = app_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ② 只加一列，且必须 `ADD COLUMN IF NOT EXISTS`（幂等）、必须可空 DATE。
///
/// 本卡命名的表只有 `biz_exchange_rate`：多碰一张表就是越界到 M1-03（科目外币标记）/
/// M1-04（凭证本位币金额），撞车成本比省下的时间高得多。
#[test]
fn v0407_adds_one_nullable_date_column_idempotently() {
    let sql = sql_only(&read_migration(V0407_UP));
    let squashed = squash(&sql);

    assert_has(
        &sql,
        "ALTER TABLE biz_exchange_rate ADD COLUMN IF NOT EXISTS effective_to DATE",
        "effective_to 必须是可空 DATE 且用 IF NOT EXISTS（重放不报 column already exists）",
    );

    let add_column_count = squashed.matches("ADD COLUMN IF NOT EXISTS").count();
    assert_eq!(
        add_column_count, 1,
        "本迁移只许加 1 列（biz_exchange_rate.effective_to），实际 {add_column_count} 处 —— \
         多出来的是越界（M1-03 科目外币标记 / M1-04 凭证外币金额各有自己的卡）"
    );

    // 本卡不建表、不删表、不清业务数据（探针行的 DELETE 是第 ⑦ 条单独验的，不在此列）
    for forbidden in [
        "CREATE TABLE",
        "DROP TABLE",
        "TRUNCATE",
        "DELETE FROM biz_exchange_rate WHERE remark IS NULL",
    ] {
        assert!(
            !squashed.contains(forbidden),
            "V0407 出现 {forbidden}：本卡只给存量表加列 + 加约束"
        );
    }
}

/// ③ 反向区间护栏：`effective_to IS NULL OR effective_to >= effective_date`。
#[test]
fn v0407_keeps_reverse_interval_check() {
    let sql = read_migration(V0407_UP);
    assert_has(
        &sql,
        "ADD CONSTRAINT ck_biz_exchange_rate_dates \
         CHECK (effective_to IS NULL OR effective_to >= effective_date)",
        "反向区间（止早于起）必须由库层 CHECK 拒（漏了它，取汇率的区间谓词就是个摆设）",
    );
    // NULL 必须放行：末行「长期有效」靠 NULL 表达，写成 `>= COALESCE(...)` 会把末行拒掉
    let code = squash(&sql_only(&sql));
    assert!(
        code.contains("effective_to IS NULL OR effective_to >= effective_date"),
        "CHECK 必须放行 NULL（NULL = 长期有效）"
    );
}

/// ④ 区间排除约束：key 必须是「币对 + 类型」三列，区间必须 `'[]'`，且必须 DEFERRABLE。
#[test]
fn v0407_exclusion_constraint_shape() {
    let sql = read_migration(V0407_UP);
    let code = squash(&sql_only(&sql));

    assert_has(
        &sql,
        "ADD CONSTRAINT ex_biz_exchange_rate_period EXCLUDE USING gist ( \
         from_currency WITH =, to_currency WITH =, rate_type WITH =, \
         daterange(effective_date, effective_to, '[]') WITH && ) DEFERRABLE INITIALLY DEFERRED",
        "排除约束必须是 (from_currency, to_currency, rate_type) 三列 + daterange(...,'[]') && + \
         DEFERRABLE INITIALLY DEFERRED（写入口先写行后重算区间，缺 DEFERRABLE 会当场被拒）",
    );

    // 左闭右开会把「月末失效、次日生效」判成相邻不重叠以外的错，语义不对
    for bad in ["daterange(effective_date, effective_to, '[)')", "'[)'"] {
        assert!(
            !code.contains(bad),
            "V0407 出现 {bad}：区间必须是左闭右闭 '[]'（失效日含当日）"
        );
    }
    // 区间里不许出现 id：带上主键等于不约束
    assert!(
        !code.contains("id WITH ="),
        "排除约束里出现 id：带上主键就不是「区间不重叠」了"
    );
}

/// ⑤ 回填只能动 `effective_to IS NULL` 的行，且必须是「下一行生效日 - 1」。
#[test]
fn v0407_backfill_is_derived_and_idempotent() {
    let sql = read_migration(V0407_UP);
    let code = squash(&sql_only(&sql));

    assert!(
        code.contains("lead(effective_date) OVER"),
        "回填必须用 lead 求「下一条生效日」"
    );
    assert!(
        code.contains("PARTITION BY from_currency, to_currency, rate_type"),
        "回填必须按「币对 + 汇率类型」分区（少了 rate_type 会把 SPOT 与 MONTH_END 串起来）"
    );
    assert!(
        code.contains("- 1 AS derived_to"),
        "回填必须是「下一行生效日 - 1」（含当日语义，不能直接取下一条的日期）"
    );
    assert!(
        code.contains("AND r.effective_to IS NULL"),
        "回填必须只动 effective_to IS NULL 的行（否则重放会覆盖写入口已经派生好的区间）"
    );
}

/// ⑥ 回归护栏：V0272 的 `uk_biz_exchange_rate` 不许被拆、不许被改列序。
#[test]
fn v0407_never_touches_v0272_unique_index() {
    let code = squash(&sql_only(&read_migration(V0407_UP)));
    for bad in [
        "DROP INDEX IF EXISTS uk_biz_exchange_rate",
        "DROP INDEX uk_biz_exchange_rate",
        "DROP CONSTRAINT uk_biz_exchange_rate",
    ] {
        assert!(
            !code.contains(bad),
            "V0407 出现 {bad}：`uk_biz_exchange_rate` 是 `rate_upsert` 的 ON CONFLICT 目标，\
             拆了月度批量录入整批失败（两张网各管一件事：uk 管起点唯一，ex 管区间不重叠）"
        );
    }
    // 自检里必须反向确认它还在
    assert_has(
        &read_migration(V0407_UP),
        "'M1-02: uk_biz_exchange_rate 丢了（V0272 的 upsert 目标，不许拆）'",
        "自检必须断言 uk_biz_exchange_rate 仍在位",
    );
}

/// ⑦ 自检块覆盖：结构、回填、行为探针、清场，一个都不能少。
#[test]
fn v0407_selfcheck_covers_structure_backfill_and_probes() {
    let sql = read_migration(V0407_UP);

    for msg in [
        "M1-02: biz_exchange_rate.effective_to 不是可空 DATE",
        "M1-02: 缺 ck_biz_exchange_rate_dates",
        "M1-02: uk_biz_exchange_rate 丢了",
        "M1-02: btree_gist 在，但缺 ex_biz_exchange_rate_period 排除约束",
        "M1-02: 回填后仍有 % 对区间重叠",
        "M1-02: 回填不完整 —— % 行不是本组最后一行却仍 effective_to IS NULL",
        "M1-02: P1 —— 起点插进开放区间竟然成功",
        "M1-02: P2 —— 完全嵌进开放区间的区间竟然成功",
        "M1-02: P4 —— 重叠已闭合区间竟然成功",
        "M1-02: P5 —— effective_to < effective_date 竟然成功",
        "M1-02: P6 —— 同起点重复插入竟然成功",
        "M1-02: P7 —— 2025-03-15 命中 % 条",
        "M1-02: P8 —— 派生口径算得 %",
        "M1-02 自检通过",
    ] {
        assert!(sql.contains(msg), "自检断言里必须覆盖「{msg}」（当前找不到）");
    }

    // 探针必须自清场：先删汇率、再删币种（FK RESTRICT，顺序不能反）
    let code = squash(&sql_only(&sql));
    let delete_rate = code.find("DELETE FROM biz_exchange_rate WHERE remark = '__M102_PROBE__'");
    let delete_currency = code.find("DELETE FROM biz_currency WHERE code IN ('ZZM', 'ZZN')");
    assert!(
        delete_rate.is_some() && delete_currency.is_some(),
        "探针必须自清场（汇率行 + 探针币种都要删干净，重放零残留）"
    );
    assert!(
        delete_rate.unwrap() < delete_currency.unwrap(),
        "清场顺序必须是「先删汇率、后删币种」（FK ON DELETE RESTRICT，反了会被拒）"
    );
    // 探针用专用币种，别拿真实币对做区间实验（真库里 EUR 2025 年的行会被撞上）
    assert!(
        code.contains("'ZZM', '__M102_PROBE_FROM__'"),
        "探针必须用专用币种 ZZM/ZZN，不许拿 CNY/USD/EUR/HKD 真币对做重叠实验"
    );
}

/// ⑧ down 对称、幂等、顺序正确（先约束后列），且不删 btree_gist。
#[test]
fn v0407_down_drops_in_dependency_order() {
    let down = read_migration(V0407_DOWN);
    let code = sql_only(&down);

    for snippet in [
        "DROP CONSTRAINT IF EXISTS ex_biz_exchange_rate_period",
        "DROP CONSTRAINT IF EXISTS ck_biz_exchange_rate_dates",
        "DROP COLUMN IF EXISTS effective_to",
    ] {
        assert_has(&down, snippet, "down 必须用 IF EXISTS 收口（重复执行不报错）");
    }

    let squashed = squash(&code);
    let ex_pos = squashed
        .find("DROP CONSTRAINT IF EXISTS ex_biz_exchange_rate_period")
        .unwrap();
    let col_pos = squashed.find("DROP COLUMN IF EXISTS effective_to").unwrap();
    assert!(
        ex_pos < col_pos,
        "down 必须先拆排除约束再拆列：`daterange(effective_date, effective_to)` 是表达式索引，\
         列被它依赖，顺序反了 DROP COLUMN 直接报错"
    );

    assert!(
        !squashed.contains("DROP EXTENSION"),
        "down 不许删 btree_gist：扩展是库级对象，别的区间约束（如 V0336 的委托）可能也在用"
    );
    assert!(
        !squashed.contains("'biz_exchange_rate'::regclass"),
        "down 不许用 'biz_exchange_rate'::regclass：第二次执行时列已不存在，这种写法会直接抛错"
    );
}

/// ⑨ 引擎拆分器必须把每个 `DO $$ … $$` 各留成一条语句。
///
/// 用框架自己的 `split_statements`（引擎执行迁移走的就是它）验证：`$$` 内部的 `;`
/// 若被当成语句分隔符，PG 报语法错误、整个迁移事务回滚 ⇒ 服务起不来。
#[test]
fn v0407_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    let sql = read_migration(V0407_UP);
    let statements =
        split_statements(&sql, DialectKind::Postgres).expect("引擎拆分器无法拆分 V0407（方言感知的 $$ 处理失败）");

    let do_blocks: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
    assert_eq!(
        do_blocks.len(),
        4,
        "四个 DO 块（btree_gist / CHECK / 排除约束 / 自检）必须各被拆成一条语句，实际 {} 条",
        do_blocks.len()
    );
    for block in &do_blocks {
        // 每个 DO 块拆完必须是完整的一段：要么带自己的错误/提示出口，要么真的在改结构
        assert!(
            block.contains("RAISE EXCEPTION")
                || block.contains("RAISE NOTICE")
                || block.contains("RAISE WARNING")
                || block.contains("ALTER TABLE"),
            "拆分后的 DO 块必须完整（既无 RAISE 也无 ALTER TABLE，疑似被截断）"
        );
        assert!(
            block.trim_end().ends_with("END $$;") || block.trim_end().ends_with("END $$"),
            "拆分后的 DO 块必须以 END $$ 收口（被截断就会语法错误）"
        );
    }
    assert!(
        do_blocks.last().unwrap().contains("M1-02 自检通过"),
        "行为自检块必须以完成提示收口（否则是被截断的最后一块）"
    );

    let alter_count = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("ALTER TABLE"))
        .count();
    assert_eq!(
        alter_count, 1,
        "只有一条顶层 ADD COLUMN 语句（约束 ALTER 都在 DO 块里做存在性判断），实际 {alter_count} 条"
    );
}

/// ⑩ 写入侧口径必须与迁移一致 —— 派生规则只许有一处定义。
///
/// 迁移用 `lead(...) - 1`，写入口 `rederive_group` 也必须用同一套（含同样的分区列）。
/// 两处分叉就会出现「升级时算出来是 A、之后每次写又改成 B」的漂移，且只在真库看得见。
#[test]
fn v0407_rederive_matches_migration_derivation() {
    let model = std::fs::read_to_string(app_root().join("src/business/erp/currency_admin_model.rs"))
        .expect("读不到 currency_admin_model.rs");

    for snippet in [
        "(lead(effective_date) OVER (ORDER BY effective_date)) - 1 AS derived_to",
        "WHERE from_currency = ? AND to_currency = ? AND rate_type = ?",
        "r.effective_to IS DISTINCT FROM d.derived_to",
    ] {
        assert!(
            squash(&model).contains(&squash(snippet)),
            "rederive_group 的口径必须与迁移回填逐字一致，缺少：{snippet}"
        );
    }

    // 取汇率的区间谓词两处都要有（建单锁定 + 界面预览），少一处就会出现「预览 7.1、落库 7.0」
    let read = std::fs::read_to_string(app_root().join("src/business/erp/currency.rs")).expect("读不到 currency.rs");
    let model_sql = squash(&model);
    let read_sql = squash(&read);
    assert!(
        read_sql.contains("AND (effective_to IS NULL OR effective_to >= ?::text::date)"),
        "currency::spot_rate 必须带区间谓词（建单锁定走它）"
    );
    assert!(
        model_sql.contains("AND (effective_to IS NULL OR effective_to >= ?::text::date)"),
        "currency_admin_model::spot_row 必须带同一个区间谓词（预览走它）"
    );
}
