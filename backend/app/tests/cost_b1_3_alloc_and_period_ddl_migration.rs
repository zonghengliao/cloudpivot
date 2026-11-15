//! 迁移 `V0372`（分摊两表：`biz_cost_alloc_rule` / `biz_cost_alloc_result`）与
//! `V0373`（期间两表：`biz_cost_period` / `biz_cost_period_item`）的静态不变量。
//!
//! 卡：t_7efea068（COST-B1-3【成本】迁移 V0370/0371）—— 设计依据
//! `docs/architecture/ARCH-9-成本核算升级设计.md` §3.1 ⑦⑧⑨⑩、§4.1 状态机、
//! 拆卡清单 §8.1 的 B1-3。
//!
//! # 为什么号段是 V0372/V0373 而不是卡面的 V0370/V0371
//!
//! V0370（B1-1 批次 + 成本对象）与 V0371（B1-2 归集四表）已被前置卡用掉，
//! nik 迁移加载器对版本号做跨目录全局唯一校验，重号会让服务启动直接失败。
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs::migration_versions_are_unique_padded_and_have_down`
//! 统一覆盖，这里不重复。
//!
//! # 这层测试钉死什么
//!
//! 迁移是**一次性、不可回改**的东西（`schema_migrations` 记内容指纹，改一个字节
//! 已升级的机器就起不来），所以它的错误只有下次部署时才炸。这里把「写对了」的
//! 形状在 `cargo test` 阶段固化，覆盖的都是会**静默算错账 / 造成死单**的类型：
//!
//! 1. ★ **`uk_biz_cost_period` 忘了排除 `REVERSED`/`CANCELLED`** ⇒ 红冲后该期间
//!    永远无法重新结算（红冲行占着键、状态机又不许改回去）——只能改库的死单。
//!    这正是设计附录 A 记录的「真库跑通时改掉的 3 个真错误」第 1 条。
//! 2. **唯一键漏掉 `WHERE deleted_at IS NULL`** ⇒ 软删后同编码规则/同批次号再也建不了。
//! 3. **`period_item → period` 写成 RESTRICT / `→ cost_object` 写成 CASCADE** ⇒
//!    前者删批次留下孤儿计算单、后者删主数据静默带走成本账。
//! 4. **状态关联 CHECK 漏一条**（`SETTLED` 不带凭证、`REVERSED` 不带原因、
//!    `APPROVED` 不带审批实例）⇒ 出现「说结算了但没凭证」的假结算。
//! 5. **行级成本守恒 CHECK 被删**（`finished + wip_end = wip_beg + 投入`）⇒
//!    约当产量算错也不拦，账面上成本凭空多出来或少掉。
//! 6. **金额/数量列精度被改窄** ⇒ 4 位尾差倒挤失效（设计 §4.4.4 / §4.6.2 全按 4 位算）。
//! 7. **给主数据列加物理外键** ⇒ 违反设计 §3.4 弱引用约定，主数据软删锁死历史成本。
//! 8. **越界建表**（V0372 建期间表 / 建二期表 / 改存量表）⇒ 与别的卡撞车。
//! 9. **自检断言被删**（尤其「同期间 REVERSED + SETTLED 共存」那条）⇒
//!    迁移「跑过了但没验过」，等于没有自检。
//! 10. **语句被引擎拆分器切坏**（`DO $$ … $$` 里含分号）⇒ PG 语法错误、整个迁移回滚。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up×3 / down×2 / 回滚重放 / 下游护栏 / 核心验收活体验证）的结果见
//! `docs/evidence/2026-11-10-cost-b1-3-v0372-v0373/02_up_down_drill.out`，
//! 空库全链重放见同目录 `01_replay_fresh.out`（293/293 PASS）。

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

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材（比如
/// 「唯一键最初写成 period_code 全表唯一」），「禁止出现」类断言必须只看真正会执行的 SQL。
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

const V0372_UP: &str = "business/V0372__cost_alloc_tables.up.sql";
const V0372_DOWN: &str = "business/V0372__cost_alloc_tables.down.sql";
const V0373_UP: &str = "business/V0373__cost_period_tables.up.sql";
const V0373_DOWN: &str = "business/V0373__cost_period_tables.down.sql";

/// V0372 的范围：分摊两表（§3.1 ⑦⑧）。
const ALLOC_TABLES: [&str; 2] = ["biz_cost_alloc_rule", "biz_cost_alloc_result"];
/// V0373 的范围：期间两表（§3.1 ⑨⑩）。
const PERIOD_TABLES: [&str; 2] = ["biz_cost_period", "biz_cost_period_item"];

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
fn v0372_v0373_up_and_down_exist() {
    for rel in [V0372_UP, V0372_DOWN, V0373_UP, V0373_DOWN] {
        let path = backend_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ② 各迁移只建自己范围的两张表，且建表语句必须幂等（`IF NOT EXISTS`）。
///
/// 多建一张就是越界（二期表属 B1-4、出库/入库加列属 B1-5）；少一个 `IF NOT EXISTS`，
/// 迁移重复执行（回滚后重放 / 演习库补跑）就会 `relation already exists` 中断。
#[test]
fn v0372_v0373_create_exactly_their_own_tables_idempotently() {
    let alloc = sql_only(&read_migration(V0372_UP));
    let period = sql_only(&read_migration(V0373_UP));

    for table in ALLOC_TABLES {
        assert_has(
            &alloc,
            &format!("CREATE TABLE IF NOT EXISTS {table} ("),
            &format!("{table} 必须以 CREATE TABLE IF NOT EXISTS 建（幂等）"),
        );
    }
    for table in PERIOD_TABLES {
        assert_has(
            &period,
            &format!("CREATE TABLE IF NOT EXISTS {table} ("),
            &format!("{table} 必须以 CREATE TABLE IF NOT EXISTS 建（幂等）"),
        );
    }

    assert_eq!(
        squash(&alloc).matches("CREATE TABLE").count(),
        2,
        "V0372 只许建分摊两表（alloc_rule / alloc_result）"
    );
    assert_eq!(
        squash(&period).matches("CREATE TABLE").count(),
        2,
        "V0373 只许建期间两表（period / period_item）"
    );

    // 互相不越界：V0372 不碰期间表，V0373 不碰分摊表
    for forbidden in PERIOD_TABLES {
        assert!(
            !squash(&alloc).contains(&format!("CREATE TABLE IF NOT EXISTS {forbidden}")),
            "V0372 不许建 {forbidden} —— 期间两表属同一张卡的 V0373"
        );
    }
    for forbidden in ALLOC_TABLES {
        assert!(
            !squash(&period).contains(&format!("CREATE TABLE IF NOT EXISTS {forbidden}")),
            "V0373 不许建 {forbidden} —— 分摊两表属同一张卡的 V0372"
        );
    }

    // 一期 10 张表里不属于 B1-3 的，以及二期三张表，一张都不许出现
    for forbidden in [
        "biz_lot",
        "biz_cost_object",
        "biz_cost_collect",
        "biz_cost_labor",
        "biz_cost_overhead",
        "biz_cost_rate",
        "biz_cost_standard",
        "biz_cost_standard_item",
        "biz_cost_variance",
    ] {
        for sql in [&alloc, &period] {
            assert!(
                !squash(sql).contains(&format!("CREATE TABLE IF NOT EXISTS {forbidden}")),
                "B1-3 不建 {forbidden}：biz_lot/biz_cost_object 是 B1-1（V0370）、归集四表是 \
                 B1-2（V0371）、二期三表是 B1-4"
            );
        }
    }
}

/// ③ 不许碰任何存量表结构（本卡是「只加新表」）。
///
/// 一旦出现 `ALTER TABLE biz_stock_out_item …`，就和 B1-5（出库/入库加列）正面撞车。
#[test]
fn v0372_v0373_never_alter_existing_tables() {
    for rel in [V0372_UP, V0373_UP] {
        let code = squash(&sql_only(&read_migration(rel)));
        for forbidden in ["ALTER TABLE", "TRUNCATE", "DROP TABLE", "DROP INDEX"] {
            assert!(
                !code.contains(forbidden),
                "{rel} 只允许「建新表 + 建索引」，不允许出现 {forbidden}：存量表改造属 B1-5 等卡"
            );
        }
    }
}

/// ④ up 里的 DML 只允许出现在「自己两张表 + biz_cost_object 探针父行」上。
#[test]
fn v0372_v0373_dml_only_touches_own_tables_and_probe_parent() {
    for (rel, own) in [(V0372_UP, ALLOC_TABLES), (V0373_UP, PERIOD_TABLES)] {
        let code = sql_only(&read_migration(rel));
        let targets = dml_targets(&code);
        assert!(
            !targets.is_empty(),
            "{rel} 自检里的探针行应该用 INSERT/DELETE 写自己的表；一个 DML 都没有说明自检被删光了"
        );
        for table in targets {
            assert!(
                own.contains(&table.as_str()) || table == "biz_cost_object",
                "{rel} 写了 {table} —— 除 biz_cost_object（RESTRICT 反证用的探针父行，断言后删除）\
                 之外，本卡不许对任何其它表做 DML"
            );
        }
    }
}

/// ⑤ 外键必须精确：
///    V0372：2 条，全部 `ON DELETE RESTRICT`（→alloc_rule / →cost_object）
///    V0373：2 条，`period_item → period` 是 **CASCADE**、`→ cost_object` 是 **RESTRICT**，
///           且 `biz_cost_period` 自己 **0 条**外键（审批/凭证/组织全是弱引用）。
#[test]
fn v0372_v0373_foreign_keys_have_exact_delete_actions() {
    let alloc = sql_only(&read_migration(V0372_UP));
    let period = sql_only(&read_migration(V0373_UP));

    assert_has(
        &alloc,
        "rule_id UUID NOT NULL REFERENCES biz_cost_alloc_rule(id) ON DELETE RESTRICT",
        "分摊结果 → 分摊规则必须 ON DELETE RESTRICT（删规则会静默带走历史分摊）",
    );
    assert_has(
        &alloc,
        "cost_object_id UUID NOT NULL REFERENCES biz_cost_object(id) ON DELETE RESTRICT",
        "分摊结果 → 成本对象必须 ON DELETE RESTRICT",
    );
    assert_eq!(
        squash(&alloc).matches("REFERENCES").count(),
        2,
        "V0372 只允许 2 条物理外键（都落在 biz_cost_alloc_result 上）"
    );

    assert_has(
        &period,
        "period_id UUID NOT NULL REFERENCES biz_cost_period(id) ON DELETE CASCADE",
        "计算单 → 批次必须 ON DELETE CASCADE（计算单是批次的从属明细，删批次不许留孤儿）",
    );
    assert_has(
        &period,
        "cost_object_id UUID NOT NULL REFERENCES biz_cost_object(id) ON DELETE RESTRICT",
        "计算单 → 成本对象必须 ON DELETE RESTRICT（删主数据会带走成本账）",
    );
    assert_eq!(
        squash(&period).matches("REFERENCES").count(),
        2,
        "V0373 只允许 2 条物理外键（都在 biz_cost_period_item 上）：\
         biz_cost_period 的 approval_instance_id / 凭证 / org_id 必须弱引用（设计 §3.4）"
    );

    // 反向：不许 REFERENCES 到成本模块内部两表与 biz_cost_object 以外的东西
    for (sql, allowed) in [
        (squash(&alloc), ["biz_cost_alloc_rule", "biz_cost_object"].as_slice()),
        (squash(&period), ["biz_cost_period", "biz_cost_object"].as_slice()),
    ] {
        let mut rest: &str = &sql;
        while let Some(pos) = rest.find("REFERENCES") {
            let after = &rest[pos + "REFERENCES".len()..];
            let target: String = after
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            assert!(
                allowed.contains(&target.as_str()),
                "发现一条指向 {target} 的外键：成本表对主数据 / 单据必须弱引用（设计 §3.4）"
            );
            rest = after;
        }
    }
}

/// ⑥ ★ 核心验收：`uk_biz_cost_period` 必须是「(org_id, period_code) + 软删 + 排除 REVERSED/CANCELLED」。
///
/// 这条唯一键是整张卡最重要的形状 —— 写成全表唯一就是设计附录 A 记录的**真错误 1**：
/// 红冲后该期间永远无法重新结算（红冲行占位、状态机不许改回），只能工程师改库。
#[test]
fn v0373_period_unique_key_excludes_reversed_and_cancelled() {
    let sql = read_migration(V0373_UP);

    assert_has(
        &sql,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_period
            ON biz_cost_period (org_id, period_code)
            WHERE deleted_at IS NULL AND status NOT IN ('REVERSED','CANCELLED')",
        "uk_biz_cost_period 必须是「UNIQUE + IF NOT EXISTS + (org_id, period_code) + 软删 + \
         排除 REVERSED/CANCELLED」的完整形状（少跑一个状态都会留下死单）",
    );

    // 反向：绝不能退化成 period_code 单列全表唯一（真错误 1 的原形）
    let code = squash(&sql_only(&sql));
    assert!(
        !code.contains("ON biz_cost_period (period_code)"),
        "uk_biz_cost_period 退化成了 period_code 单列唯一 —— 红冲后该期间永远无法重新结算（死单）"
    );
    assert!(
        !code.contains("uk_biz_cost_period ON biz_cost_period (org_id, period_code);"),
        "uk_biz_cost_period 少了 WHERE 谓词（软删行 / 红冲行会占位）"
    );

    // 自检里必须有一条断言专门守这个谓词
    assert!(
        sql.contains("uk_biz_cost_period 未排除 REVERSED/CANCELLED"),
        "迁移自检里必须有一条断言守「唯一键排除 REVERSED/CANCELLED」"
    );
}

/// ⑦ 其余唯一键 / 索引：软删感知（无软删列的产物表除外，理由写在迁移头里）。
#[test]
fn v0372_v0373_other_unique_keys_are_correctly_shaped() {
    let alloc = read_migration(V0372_UP);
    let period = read_migration(V0373_UP);

    assert_has(
        &alloc,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_rule_code ON biz_cost_alloc_rule (rule_code) WHERE deleted_at IS NULL",
        "uk_biz_cost_rule_code 必须是「UNIQUE + IF NOT EXISTS + 软删」（停用规则后可重建同编码）",
    );
    // 分摊结果是计算产物、没有 deleted_at / status：全表唯一四列组合是**设计有意**的形状
    // （重算＝按期间整删重灌，见设计 §4.4.5），不是漏了软删条件。
    assert_has(
        &alloc,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_alloc_result
            ON biz_cost_alloc_result (period_code, rule_id, cost_object_id, account_code)",
        "uk_biz_cost_alloc_result 必须是四列组合唯一（同规则同对象同科目只能一行）",
    );
    assert!(
        !squash(&sql_only(&alloc))
            .contains("biz_cost_alloc_result (period_code, rule_id, cost_object_id, account_code) WHERE"),
        "分摊结果表没有 deleted_at 列，带 WHERE 反而说明有人加了设计里不存在的列"
    );

    assert_has(
        &period,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_period_no
            ON biz_cost_period (batch_no) WHERE deleted_at IS NULL",
        "uk_biz_cost_period_no 必须带软删条件（红冲后重建批次会复用批次号）",
    );
    assert_has(
        &period,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_item ON biz_cost_period_item (period_id, cost_object_id)",
        "uk_biz_cost_item 必须是 (period_id, cost_object_id) 唯一（一个批次里一个对象只有一张单）",
    );

    // 反向：本卡的唯一索引里，凡是建在「带 deleted_at 的表」上的，都必须有软删谓词。
    // 注意匹配要带上结尾的 ` (`：uk_biz_cost_item 建在 biz_cost_period_item 上，
    // 而 biz_cost_period_item **没有** deleted_at 列（它是从属明细，靠 CASCADE 跟批次走）。
    for (sql, soft_delete_tables) in [
        (&alloc, ["biz_cost_alloc_rule"].as_slice()),
        (&period, ["biz_cost_period"].as_slice()),
    ] {
        let squashed = squash(&sql_only(sql));
        let mut rest: &str = &squashed;
        while let Some(pos) = rest.find("CREATE UNIQUE INDEX") {
            let stmt_end = rest[pos..].find(';').map(|e| pos + e).unwrap_or(rest.len());
            let stmt = &rest[pos..stmt_end];
            let on_soft_delete_table = soft_delete_tables.iter().any(|t| stmt.contains(&format!(" ON {t} (")));
            if on_soft_delete_table {
                assert!(
                    stmt.contains("WHERE deleted_at IS NULL"),
                    "找到一条建在有 deleted_at 的表上、却不带软删条件的唯一索引：{stmt}"
                );
            }
            rest = &rest[stmt_end..];
        }
    }
}

/// ⑧ CHECK 约束一条都不能少（漏一条 = 一条脏数据入口）。
#[test]
fn v0372_v0373_keep_all_check_constraints() {
    let alloc = read_migration(V0372_UP);
    let period = read_migration(V0373_UP);

    let expected: [(&str, &[&str]); 4] = [
        (
            "biz_cost_alloc_rule",
            &[
                "ck_biz_cost_rule_biz",
                "ck_biz_cost_rule_basis",
                "ck_biz_cost_rule_status",
                "ck_biz_cost_rule_dates",
                "ck_biz_cost_rule_account",
            ],
        ),
        (
            "biz_cost_alloc_result",
            &[
                "ck_biz_cost_alloc_period",
                "ck_biz_cost_alloc_num",
                "ck_biz_cost_alloc_base",
            ],
        ),
        (
            "biz_cost_period",
            &[
                "ck_biz_cost_period_code",
                "ck_biz_cost_period_status",
                "ck_biz_cost_period_labor",
                "ck_biz_cost_period_sum",
                "ck_biz_cost_period_cons",
                "ck_biz_cost_period_appr",
                "ck_biz_cost_period_settle",
                "ck_biz_cost_period_rev",
                "ck_biz_cost_period_cancel",
            ],
        ),
        (
            "biz_cost_period_item",
            &[
                "ck_biz_cost_item_status",
                "ck_biz_cost_item_equiv",
                "ck_biz_cost_item_units",
                "ck_biz_cost_item_money",
                "ck_biz_cost_item_pct",
                "ck_biz_cost_item_cons",
                "ck_biz_cost_item_settled",
            ],
        ),
    ];

    for (table, names) in expected {
        let sql = if ALLOC_TABLES.contains(&table) { &alloc } else { &period };
        for name in names {
            assert_has(
                sql,
                &format!("CONSTRAINT {name} CHECK"),
                &format!("{table} 缺约束 {name}（漏一条 = 一条脏数据入口）"),
            );
        }
    }

    // ★ 两条守恒 / 死单防线逐字校验
    assert_has(
        &period,
        "CONSTRAINT ck_biz_cost_item_cons CHECK (
            finished_cost + wip_end_cost = wip_beg_cost + mat_cur + labor_cur + oh_cur)",
        "行级成本守恒必须逐项写全：完工 + 期末在制 = 期初在制 + 本期投入（料+工+费）",
    );
    assert_has(
        &period,
        "CONSTRAINT ck_biz_cost_period_settle CHECK (status NOT IN ('SETTLED','CLOSED','REVERSED')
                                                OR (settled_at IS NOT NULL AND finish_voucher_id IS NOT NULL))",
        "SETTLED/CLOSED/REVERSED 必须同时有 settled_at 与完工结转凭证（否则就是「说结算了但没凭证」的死单）",
    );
    assert_has(
        &period,
        "CONSTRAINT ck_biz_cost_period_appr CHECK (status NOT IN ('SUBMITTED','APPROVED','SETTLED','CLOSED','REVERSED')
                                                OR approval_instance_id IS NOT NULL)",
        "SUBMITTED 之后必须挂审批实例（否则可以绕过审批流直接改状态）",
    );

    // 期间格式 yyyy-MM 正则：period 系列三处（alloc_result / period / period_item 无期间列）
    let alloc_period_checks = squash(&sql_only(&alloc))
        .matches("period_code ~ '^\\d{4}-(0[1-9]|1[0-2])$'")
        .count();
    assert_eq!(
        alloc_period_checks, 1,
        "biz_cost_alloc_result.period_code 必须有 yyyy-MM 正则 CHECK"
    );
    let period_checks = squash(&sql_only(&period))
        .matches("period_code ~ '^\\d{4}-(0[1-9]|1[0-2])$'")
        .count();
    assert_eq!(
        period_checks, 1,
        "biz_cost_period.period_code 必须有 yyyy-MM 正则 CHECK"
    );
}

/// ⑨ 关键列的类型 / 精度 / 默认值必须与设计 §3.1 一致。
#[test]
fn v0372_v0373_column_types_follow_design() {
    let alloc = read_migration(V0372_UP);
    let period = read_migration(V0373_UP);

    for snippet in [
        "rule_code VARCHAR(32) NOT NULL",
        "biz_type VARCHAR(16) NOT NULL DEFAULT 'OVERHEAD'",
        "source_dept_name VARCHAR(120) NOT NULL",
        "account_code VARCHAR(30) NOT NULL",
        "basis VARCHAR(16) NOT NULL",
        "priority INT NOT NULL DEFAULT 10",
        "effective_from DATE NOT NULL",
        // ⑧ 分摊结果
        "basis_value NUMERIC(18,4) NOT NULL",
        "basis_total NUMERIC(18,4) NOT NULL",
        "pool_amount NUMERIC(18,4) NOT NULL",
        "alloc_amount NUMERIC(18,4) NOT NULL",
        "rounding_adj NUMERIC(18,4) NOT NULL DEFAULT 0",
    ] {
        assert_has(&alloc, snippet, &format!("分摊两表列定义与设计 §3.1 不符：{snippet}"));
    }

    for snippet in [
        "org_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000'",
        "period_code VARCHAR(7) NOT NULL",
        "batch_no VARCHAR(32) NOT NULL",
        "status VARCHAR(12) NOT NULL DEFAULT 'DRAFT'",
        "labor_source VARCHAR(12) NOT NULL DEFAULT 'MANUAL'",
        "total_cost NUMERIC(18,4) NOT NULL DEFAULT 0",
        "finish_voucher_id UUID",
        "settled_at TIMESTAMPTZ",
        "reverse_reason VARCHAR(255)",
        "cancel_reason VARCHAR(255)",
        // ⑩ 计算单
        "product_id UUID NOT NULL",
        "completion_pct NUMERIC(5,2) NOT NULL DEFAULT 0",
        "equiv_material NUMERIC(18,4) NOT NULL DEFAULT 0",
        "equiv_conversion NUMERIC(18,4) NOT NULL DEFAULT 0",
        "unit_material NUMERIC(18,6) NOT NULL DEFAULT 0",
        "unit_finished_cost NUMERIC(18,6) NOT NULL DEFAULT 0",
        "finish_voucher_id",
        "status VARCHAR(12) NOT NULL DEFAULT 'CALC'",
        "stock_in_id UUID",
    ] {
        assert_has(&period, snippet, &format!("期间两表列定义与设计 §3.1 不符：{snippet}"));
    }

    // 反向：金额 / 数量列不许出现 scale=2（4 位尾差倒挤会被抹平）
    for rel in [V0372_UP, V0373_UP] {
        let code = squash(&sql_only(&read_migration(rel)));
        for bad in ["NUMERIC(20,2)", "NUMERIC(18,2)", "NUMERIC(20,4)"] {
            assert!(
                !code.contains(bad),
                "{rel} 出现了 {bad}：金额/数量列必须是设计 §3.1 的 NUMERIC(18,4)"
            );
        }
    }
}

/// ⑩ 自检写进迁移：每个迁移 3 个 `DO $$` 块（前置护栏 + 形态 + 行为），
///    每个失败路径都 `RAISE EXCEPTION`，且 ★核心验收（同期间 REVERSED+SETTLED 共存）必须有断言。
#[test]
fn v0372_v0373_self_check_raises_exceptions() {
    let alloc = read_migration(V0372_UP);
    let period = read_migration(V0373_UP);

    for (rel, sql, version, key_messages) in [
        (
            "V0372",
            &alloc,
            "V0372",
            vec![
                "biz_cost_object 不存在，请先执行 V0370",
                "uk_biz_cost_rule_code 缺软删条件",
                "uk_biz_cost_alloc_result 的列组合不是",
                "biz_cost_alloc_rule 的 5 条 CHECK 约束只有",
                "biz_cost_alloc_result 的 3 条 CHECK 约束只有",
                "分摊两表上的外键应为 2 条",
                "同 rule_code 第二条活规则写入成功",
                "被摊科目记 5602.01 写入成功",
                "effective_to < effective_from 写入成功",
                "同 (期间,规则,对象,科目) 第二条分摊结果写入成功",
                "basis_total=0 却摊了 100 元",
                "有分摊结果时规则被删掉",
                "有分摊结果时成本对象被删掉",
                "探针行未清理干净",
            ],
        ),
        (
            "V0373",
            &period,
            "V0373",
            vec![
                "biz_cost_object 不存在，请先执行 V0370",
                "uk_biz_cost_period 未排除 REVERSED/CANCELLED",
                "uk_biz_cost_period_no 缺软删条件",
                "biz_cost_period 的 9 条 CHECK 约束只有",
                "biz_cost_period_item 的 7 条 CHECK 约束只有",
                "period_item.cost_object_id → biz_cost_object 必须是 ON DELETE RESTRICT",
                "period_item.period_id → biz_cost_period 必须是 ON DELETE CASCADE",
                "org_id 的默认值不是全 0 哨兵 uuid",
                // ★ 卡面验收的可执行版本
                "同期间一条 REVERSED + 一条 SETTLED 应共存",
                "同期间第二名活跃批次写入成功",
                "同期间两条 REVERSED 留痕应共存",
                "CANCELLED 后重开同期间批次失败",
                "软删后同期间同批次号重建失败",
                "SETTLED 不带 settled_at / finish_voucher_id 写入成功",
                "APPROVED 没有审批实例就写入成功",
                "REVERSED 不带 reverse_reason 写入成功",
                "完工+在制 ≠ 期初+投入 写入成功",
                "SETTLED 计算单不带 stock_in_id 写入成功",
                "批次删除后计算单残留",
                "探针行未清理干净",
            ],
        ),
    ] {
        let blocks: Vec<&str> = sql.split("DO $$").skip(1).collect();
        assert_eq!(
            blocks.len(),
            3,
            "{rel} 应有 3 个 DO 块（前置护栏 / 形态断言 / 行为断言），实际 {} 个",
            blocks.len()
        );
        for (i, block) in blocks.iter().enumerate() {
            assert!(
                block.contains("RAISE EXCEPTION"),
                "{rel} 第 {} 个 DO 块没有任何 RAISE EXCEPTION —— 自检失败也不会拦住迁移",
                i + 1
            );
        }
        for msg in key_messages {
            assert!(
                sql.contains(msg),
                "{rel} 自检断言里必须有一条覆盖「{msg}」（当前找不到）"
            );
        }
        // 自检日志里要能看出是哪一号迁移干的
        assert!(
            sql.contains(&format!("RAISE NOTICE '{} 形态断言通过", version)),
            "{rel} 形态自检块必须以完成提示收口"
        );
        assert!(
            sql.contains(&format!("{version} 完成")),
            "{rel} 行为自检块必须以完成提示收口"
        );
    }

    // 探针行必须删干净：迁移重复执行不能往真库里攒垃圾行
    let alloc_code = squash(&sql_only(&alloc));
    let period_code = squash(&sql_only(&period));
    for table in ALLOC_TABLES {
        assert!(
            alloc_code.contains(&format!("DELETE FROM {table}")),
            "{table} 的自检探针行必须在迁移里被 DELETE 掉（重复执行零残留）"
        );
    }
    for table in PERIOD_TABLES {
        assert!(
            period_code.contains(&format!("DELETE FROM {table}")),
            "{table} 的自检探针行必须在迁移里被 DELETE 掉（重复执行零残留）"
        );
    }
}

/// ⑪ down 必须幂等 + 有下游依赖护栏 + 结尾断言零残留。
#[test]
fn v0372_v0373_down_is_idempotent_and_guarded() {
    for (rel, version, tables, fk_target_1, fk_target_2) in [
        (
            V0372_DOWN,
            "V0372",
            ALLOC_TABLES,
            "biz_cost_alloc_rule",
            "biz_cost_alloc_result",
        ),
        (
            V0373_DOWN,
            "V0373",
            PERIOD_TABLES,
            "biz_cost_period",
            "biz_cost_period_item",
        ),
    ] {
        let down = read_migration(rel);

        for table in tables {
            assert_has(
                &down,
                &format!("DROP TABLE IF EXISTS {table};"),
                &format!("{rel} 必须用 DROP TABLE IF EXISTS 删 {table}（重复执行不报错）"),
            );
        }

        // 子表在前、父表在后（否则 RESTRICT 的父表删不动）
        let pos_1 = squash(&down)
            .find(&format!("DROP TABLE IF EXISTS {fk_target_1};"))
            .unwrap();
        let pos_2 = squash(&down)
            .find(&format!("DROP TABLE IF EXISTS {fk_target_2};"))
            .unwrap();
        assert!(
            pos_2 < pos_1,
            "{rel} 的 DROP 顺序必须是「先子表后父表」（{fk_target_2} 应在 {fk_target_1} 之前）"
        );

        assert_has(
            &down,
            "FROM pg_constraint f",
            &format!("{rel} 必须用 pg_constraint 扫「有没有外部表 FK 到我的表」，而不是硬编码表名"),
        );
        assert_has(
            &down,
            &format!("{version} down 拒绝执行：下游表仍在引用"),
            &format!("{rel} 的拒绝信息必须说清「先回滚哪张卡」"),
        );

        // down 自身幂等：不许出现 `'表名'::regclass` 这种「表已删就抛错」的写法
        let code = sql_only(&down);
        for table in tables {
            assert!(
                !code.contains(&format!("'{table}'::regclass")),
                "{rel} 里出现了 '{table}'::regclass：第二次执行 down 时表已不存在，\
                 这行会直接抛 `relation does not exist`，down 就不幂等了"
            );
        }
        assert!(
            squash(&code).contains(&format!("RAISE NOTICE '{version} down 完成")),
            "{rel} 结尾必须有自检：删完要断言表和索引确实没了"
        );
    }
}

/// ⑫ 引擎拆分器必须各留成 3 条 `DO $$ … $$` 语句（`$$` 内部的 `;` 不能被当分隔符）。
#[test]
fn v0372_v0373_split_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    for (rel, index_count) in [(V0372_UP, 4), (V0373_UP, 5)] {
        let sql = read_migration(rel);
        let statements = split_statements(&sql, DialectKind::Postgres)
            .unwrap_or_else(|e| panic!("引擎拆分器无法拆分 {rel}（方言感知的 $$ 处理失败）: {e}"));

        let do_blocks: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
        assert_eq!(
            do_blocks.len(),
            3,
            "{rel} 的三个 DO 块必须各被拆成**一条**语句（实际 {} 条）：\
             >3 说明 $$ 内部的 `;` 被当成分隔符了",
            do_blocks.len()
        );
        for block in &do_blocks {
            assert!(
                block.contains("RAISE EXCEPTION"),
                "{rel} 拆分后的 DO 块必须完整包含断言"
            );
            assert!(
                block.trim_end().ends_with("END $$;") || block.trim_end().ends_with("END $$"),
                "{rel} 拆分后的 DO 块必须以 END $$ 收口（被截断就会语法错误）"
            );
        }

        let create_table_count = statements
            .iter()
            .filter(|s| sql_head(s).starts_with("CREATE TABLE"))
            .count();
        assert_eq!(
            create_table_count, 2,
            "{rel} 的两条 CREATE TABLE 应各成一条语句，实际 {create_table_count} 条"
        );

        let create_index_count = statements
            .iter()
            .filter(|s| sql_head(s).starts_with("CREATE") && sql_head(s).contains("INDEX"))
            .count();
        assert_eq!(
            create_index_count, index_count,
            "{rel} 应有 {index_count} 条 CREATE INDEX，实际 {create_index_count} 条"
        );
    }
}
