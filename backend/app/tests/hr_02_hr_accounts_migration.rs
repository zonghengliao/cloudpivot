//! 迁移 `V0384`（人力资源工资相关科目 + 2 条凭证规则）的静态不变量。
//!
//! 卡：t_53922091（HR-02【HR】迁移 V0379）—— 设计依据
//! `docs/architecture/ARCH-9-人力资源模块设计.md` §3.2 ①（9 科目 SQL，真库跑通）、
//! §3.2 ⑧（2 条 `biz_voucher_rule`）、§4.6（工资凭证勾稽：借 `5602.1x` / 贷 `2211.0x`）、
//! §7.1（迁移清单）、§7.2（翻末级前查历史分录）、§7.3（回滚方案）、§8.1（拆卡清单 HR-02）。
//!
//! # 号段说明
//!
//! 卡面与设计 §7.1 写的是 `V0379`，但 `V0378~V0382` 已被成本核算卡片实占并已提交
//! （`V0378__cost_menu_and_perm` … `V0382__stock_in_production_sourceless`）。
//! 本仓迁移加载器对版本号做**跨目录全局唯一**校验（重号 = 服务启动失败），
//! 所以 HR 全系列整体后移 5 号，本卡落在 `V0384`（`V0379__hr_accounts.*` 一旦出现就是撞号）。
//!
//! # 这层测试钉死什么
//!
//! 科目种子与「翻末级」是一次性、不可回改的东西（`schema_migrations` 记内容指纹），
//! 写错的代价不是编译错误，而是**账算错、报表静默少钱**。这里把「写对了」的形状在
//! `cargo test` 阶段固化，覆盖会静默出事的类型：
//!
//! 1. 少种一个科目 ⇒ 工资凭证（§4.6）过账时借方/贷方解析不到科目，整单过账失败；
//! 2. 父科目没翻非末级 ⇒ 凭证闸仍允许「借 2211」这类汇总级分录，报表按末级取数时这块钱掉出报表；
//! 3. 翻末级前没查历史分录 ⇒ 既有金额被埋在汇总科目上，跨年试算不平（卡面验收项）；
//! 4. 翻末级不加「确实有子科目」条件 ⇒ 老库里造出「末级科目挂子科目」的裂树；
//! 5. 2 条规则解析不到科目 ⇒ 配置页/巡检对账失去锚点（空壳规则）；
//! 6. 本迁移越界改别的表（`ALTER TABLE biz_voucher` 之类）⇒ 与别的卡撞车；
//! 7. `down` 把 `2211.01` 一起删掉 ⇒ 只回滚 V0384 时成本结算的人工计提凭证失去科目；
//! 8. 引擎拆分器把 `DO $$ … $$` 内部的 `;` 当语句分隔符 ⇒ 整个迁移事务回滚，服务起不来。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 的
//! `migration_versions_are_unique_padded_and_have_down` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（老库 up×2 / down×2 + 全量库 up×2 / down、两条护栏反证、`2211.01` 复用与
//! 部门维度补写）的结果写在卡内汇报里。

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

const V0384_UP: &str = "business/V0384__hr_accounts.up.sql";
const V0384_DOWN: &str = "business/V0384__hr_accounts.down.sql";

/// 设计 §3.2 ① 的 9 个末级科目：(编码, 名称, 父级, 类别, 余额方向, 辅助核算)。
const NINE_ACCOUNTS: [(&str, &str, &str, &str, &str, &str); 9] = [
    ("2211.01", "应付职工薪酬-工资", "2211", "LIABILITY", "CREDIT", "DEPT"),
    (
        "2211.02",
        "应付职工薪酬-社保(单位)",
        "2211",
        "LIABILITY",
        "CREDIT",
        "DEPT",
    ),
    (
        "2211.03",
        "应付职工薪酬-公积金(单位)",
        "2211",
        "LIABILITY",
        "CREDIT",
        "DEPT",
    ),
    (
        "2221.03",
        "应交税费-应交个人所得税",
        "2221",
        "LIABILITY",
        "CREDIT",
        "NULL",
    ),
    (
        "2241.03",
        "其他应付款-社保(个人)",
        "2241",
        "LIABILITY",
        "CREDIT",
        "NULL",
    ),
    (
        "2241.04",
        "其他应付款-公积金(个人)",
        "2241",
        "LIABILITY",
        "CREDIT",
        "NULL",
    ),
    ("5602.10", "管理费用-工资", "5602", "PROFIT_LOSS", "DEBIT", "DEPT"),
    ("5602.11", "管理费用-社保(单位)", "5602", "PROFIT_LOSS", "DEBIT", "DEPT"),
    (
        "5602.12",
        "管理费用-公积金(单位)",
        "5602",
        "PROFIT_LOSS",
        "DEBIT",
        "DEPT",
    ),
];

/// 要被翻成非末级的 4 个父科目（§3.2 ① 的 `UPDATE` 名单）。
const PARENTS: [&str; 4] = ["2211", "2221", "2241", "5602"];

/// ① up / down 必须成对存在，且号段落在 V0384（不是卡面写的 V0379 —— 那个号已被成本占）。
#[test]
fn v0384_up_and_down_exist_and_use_free_version() {
    for rel in [V0384_UP, V0384_DOWN] {
        let path = backend_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
    // 反面：不许有人把文件改回撞号的 V0379~V0382
    for taken in [
        "business/V0379__hr_accounts.up.sql",
        "business/V0380__hr_accounts.up.sql",
        "business/V0381__hr_accounts.up.sql",
        "business/V0382__hr_accounts.up.sql",
    ] {
        let path = backend_root().join("migrations").join(taken);
        assert!(
            !path.exists(),
            "{} 存在：V0378~V0382 已被成本核算迁移实占，撞号会让服务启动失败",
            path.display()
        );
    }
}

/// ② 9 个末级科目必须与 §3.2 ① **逐行**一致（编码 / 名称 / 父级 / 类别 / 方向 / 辅助核算）。
///
/// 这是本卡最核心的验收项：编码错一位，HR-39 的工资凭证就解析不到科目；
/// 辅助核算少一个 `DEPT`，部门口径的工资就永远查不出来（§4.6.2 规则 3）。
#[test]
fn v0384_seeds_exactly_the_nine_design_accounts() {
    let sql = sql_only(&read_migration(V0384_UP));

    for (code, name, parent, category, direction, aux) in NINE_ACCOUNTS {
        // aux 是 SQL 字面量：`'DEPT'` 带引号，`NULL` 不带 —— 只有 NULL 原样拼
        let aux_sql = if aux == "NULL" {
            "NULL".to_string()
        } else {
            format!("'{aux}'")
        };
        assert_has(
            &sql,
            &format!("('{code}', '{name}', '{parent}', '{category}', '{direction}', {aux_sql})"),
            &format!("科目 {code} 的 VALUES 行必须与设计 §3.2 ① 逐字一致（名称/父级/类别/方向/辅助核算）"),
        );
    }

    // 9 条 VALUES 行，一条不多一条不少（多一行就说明有人夹带了第 10 个科目）
    let acct_block = {
        let start = sql.find("INSERT INTO biz_account").expect("找不到科目 INSERT");
        let end = sql[start..]
            .find("ON CONFLICT")
            .expect("科目 INSERT 必须带 ON CONFLICT")
            + start;
        squash(&sql[start..end])
    };
    // 每行 VALUES 都以 `('` 开头（编码字面量）⇒ 计数必须恰好 9
    // （不数 `'), ('` 分隔符：aux 为 NULL 的行接的是 `NULL), ('`，会漏数）
    let values_rows = acct_block.matches("('").count();
    assert_eq!(
        values_rows, 9,
        "科目 VALUES 应恰好 9 行，实际 {values_rows} 行 —— 多一行就是夹带了别的科目"
    );

    // 列清单必须包含辅助核算（V0377 漏掉了这一列，导致 2211.01 的部门维度为 NULL）
    assert_has(
        &sql,
        "INSERT INTO biz_account (account_code, account_name, parent_id, level, account_category, balance_direction, is_leaf, status, aux_dimensions, remark)",
        "科目 INSERT 必须显式写 aux_dimensions（§4.6.2 规则 3 要求 2211.0x / 5602.1x 带 DEPT）",
    );
    // 级次从父级推（父级都是 1 级 ⇒ 2 级；客户调过父级级次也不造错位的树）
    assert_has(
        &sql,
        "p.level + 1",
        "level 必须由父级级次推导（与设计稿字面值 2 等价，但不会在客户调过父级后错位）",
    );
    // 幂等：已存在的编码复用，不覆盖客户的改名/改父级
    assert_has(
        &sql,
        "ON CONFLICT (account_code) DO NOTHING",
        "科目 INSERT 必须走 ON CONFLICT DO NOTHING（V0377 已种 2211.01，D-14 要求幂等共存）",
    );
    // 父级必须真实存在且未软删
    assert_has(
        &sql,
        "JOIN biz_account p ON p.account_code = v.parent_code AND p.deleted_at IS NULL",
        "父级解析必须排除软删科目（软删科目下挂子科目 = 界面看不见的孤儿）",
    );
    assert_has(
        &sql,
        "1, 1, v.aux,",
        "落库必须是 is_leaf = 1（可记账）/ status = 1（启用）",
    );
}

/// ③ 父科目翻非末级：必须**有条件**（有未删子科目 + 当前还是末级），且只翻这 4 个。
///
/// 无条件翻会造裂树；翻到名单外的科目是越界（`2211` 之外的科目各有自己的卡）。
#[test]
fn v0384_flips_only_the_four_parents_conditionally() {
    let sql = sql_only(&read_migration(V0384_UP));

    assert_has(
        &sql,
        "UPDATE biz_account p SET is_leaf = 0, updated_at = CURRENT_TIMESTAMP",
        "必须把父科目翻成非末级（is_leaf = 0），否则凭证闸仍允许记汇总科目",
    );
    for parent in PARENTS {
        assert_has(
            &sql,
            &format!("'{parent}'"),
            &format!("父科目名单必须包含 {parent}（§3.2 ①）"),
        );
    }
    assert_has(
        &sql,
        "AND p.is_leaf = 1",
        "翻末级的 UPDATE 必须带 is_leaf = 1 条件（幂等：第二遍 0 行）",
    );
    assert_has(
        &sql,
        "AND EXISTS (SELECT 1 FROM biz_account c WHERE c.parent_id = p.id AND c.deleted_at IS NULL)",
        "翻末级必须要求「确实有未删子科目」——否则会翻出一个没有下级的空壳汇总科目，\
         或者把已有子科目的末级科目留在裂树状态（§7.5 实测教训）",
    );
    assert_has(
        &sql,
        "AND p.deleted_at IS NULL",
        "软删的父科目不许翻（界面上看不见，翻了也没人能用）",
    );
}

/// ④ 卡面验收：「翻末级前检查历史分录为 0」。
///
/// 断言的是**顺序**与**口径**：护栏必须在 UPDATE 之前，且只拦还需要翻的父科目
/// （`is_leaf = 1`），否则重复执行会因历史分录误报红。
#[test]
fn v0384_checks_history_entries_before_flipping() {
    let raw = read_migration(V0384_UP);
    let sql = sql_only(&raw);

    assert_has(
        &sql,
        "SELECT count(*) INTO v_entries FROM biz_voucher_entry e WHERE e.account_code = rec.code",
        "必须逐个父科目数 biz_voucher_entry 的分录（卡面验收：翻末级前历史分录为 0）",
    );
    assert_has(
        &sql,
        "CONTINUE WHEN rec.leaf_flag <> 1;",
        "只拦「还需要翻」的父科目（is_leaf = 1）：已是非末级的重复执行不该被历史分录误报红",
    );
    assert_has(
        &sql,
        "RAISE EXCEPTION 'V0384 拒绝执行：% —— 这些汇总科目还有历史分录",
        "父科目有分录必须 RAISE EXCEPTION 拒绝升级（只 NOTICE 等于没拦）",
    );

    // ★ 顺序：护栏 DO 块必须在翻末级的 UPDATE 之前
    //   （用 squash 后的串定位：SQL 里 `SET is_leaf    = 0` 有对齐空格，字面匹配会落空）
    let flat = squash(&sql);
    let guard_pos = flat.find("这些汇总科目还有历史分录").expect("找不到历史分录护栏");
    let flip_pos = flat
        .find("UPDATE biz_account p SET is_leaf = 0")
        .expect("找不到翻末级的 UPDATE");
    assert!(
        guard_pos < flip_pos,
        "历史分录护栏必须在翻末级的 UPDATE **之前**（顺序反了等于翻完才检查，钱已经埋进汇总科目）"
    );
}

/// ⑤ 2 条凭证规则：`rule_code` / `biz_type` / 借贷科目与 §3.2 ⑧ 一致，且幂等。
#[test]
fn v0384_seeds_the_two_payroll_voucher_rules() {
    let sql = sql_only(&read_migration(V0384_UP));

    for (rule, biz, debit, credit) in [
        ("PAYROLL_ACCRUAL_DEFAULT", "PAYROLL_ACCRUAL", "5602.10", "2211.01"),
        ("PAYROLL_PAYMENT_DEFAULT", "PAYROLL_PAYMENT", "2211.01", "1002"),
    ] {
        assert_has(
            &sql,
            &format!("('{rule}', '{biz}', '{debit}', '{credit}',"),
            &format!("规则 {rule} 的 (biz_type, 借方, 贷方) 必须与 §3.2 ⑧ 一致"),
        );
    }
    assert_has(
        &sql,
        "INSERT INTO biz_voucher_rule (rule_code, biz_type, debit_account_id, credit_account_id, tax_account_id, is_active, remark)",
        "凭证规则必须写全列（tax_account_id 留空 = 工资不涉及增值税）",
    );
    assert_has(
        &sql,
        "WHERE NOT EXISTS (SELECT 1 FROM biz_voucher_rule r WHERE r.rule_code = v.rule AND r.deleted_at IS NULL)",
        "规则 INSERT 必须幂等（重复执行不重复种）",
    );
    // 科目 id 由 LEFT JOIN 解析：解析不到会落 NULL，由自检拦下并点名（不留空壳规则）
    assert_has(
        &sql,
        "LEFT JOIN biz_account d ON d.account_code = v.debit AND d.deleted_at IS NULL",
        "借方科目必须按编码解析 id，且排除软删科目",
    );
}

/// ⑥ 部门辅助核算补写：`2211.0x` / `5602.1x` 落库必须是 `DEPT`，且对已存在的行幂等补写。
///
/// 这条是 V0377 留下的坑：它的 INSERT 没有 `aux_dimensions` 列，`2211.01` 落库为 NULL，
/// 而工资凭证借贷双方都要带部门（§4.6.2 规则 3）。
#[test]
fn v0384_backfills_department_aux_dimension() {
    let sql = sql_only(&read_migration(V0384_UP));

    assert_has(
        &sql,
        "SET aux_dimensions = 'DEPT'",
        "必须补写部门辅助核算（否则「研发部工资」永远查不出来）",
    );
    assert_has(
        &sql,
        "AND aux_dimensions IS NULL",
        "补写必须幂等（第二遍 0 行），且不许覆盖客户已配的其它维度",
    );
    for code in ["2211.01", "2211.02", "2211.03", "5602.10", "5602.11", "5602.12"] {
        assert_has(
            &sql,
            &format!("'{code}'"),
            &format!("{code} 必须带 DEPT 辅助核算（§3.2 ① 的 aux 列）"),
        );
    }
    // 个税 / 代扣社保 / 代扣公积金**不带**部门维度（设计稿 aux = NULL）
    assert_has(
        &sql,
        "'2221.03', '应交税费-应交个人所得税', '2221', 'LIABILITY', 'CREDIT', NULL",
        "2221.03 的辅助核算必须是 NULL（§3.2 ①，个税与部门无关）",
    );
}

/// ⑦ 自检必须真的断言（EXCEPTION 而非 NOTICE），且覆盖卡面的三个验收面。
#[test]
fn v0384_self_checks_raise_exceptions() {
    let sql = sql_only(&read_migration(V0384_UP));

    for (snippet, why) in [
        ("V0384 自检失败：父科目没翻成非末级", "必须断言 4 个父科目都翻成非末级"),
        (
            "V0384 自检失败：末级科目形态不对",
            "必须逐条断言 9 个科目的父级/级次/类别/方向/末级位",
        ),
        (
            "V0384 自检失败：以下科目的部门辅助核算没配上",
            "必须断言 6 个科目的 DEPT 维度",
        ),
        (
            "V0384 自检失败：还有 % 个「末级科目却挂着子科目」的裂树节点",
            "必须断言全表无裂树",
        ),
        ("V0384 自检失败：汇总科目上仍有 % 条分录", "必须断言父科目上没有分录"),
        ("V0384 自检失败：工资凭证规则只有 % 条在位", "必须断言 2 条规则都在位"),
        (
            "V0384 自检失败：工资凭证规则解析不对",
            "必须断言 2 条规则都解析到科目（卡面验收）",
        ),
        (
            "V0384 自检失败：记账闸只会拒 % 个汇总科目",
            "必须断言记账闸的拒绝面（4 个汇总科目）",
        ),
        (
            "V0384 自检失败：可记账的末级工资科目只有 % 个",
            "必须断言记账闸的放行面（9 个末级科目）",
        ),
        (
            "V0384 自检失败：报表项的科目模式里丢了前缀",
            "必须断言报表项前缀仍在（否则新叶子余额静默掉出报表）",
        ),
    ] {
        assert_has(&sql, snippet, why);
    }
    assert_has(
        &sql,
        "RAISE NOTICE 'V0384 完成：9 个工资科目 + 4 父科目翻非末级 + 2 条凭证规则",
        "迁移末尾必须有完成提示（部署日志里要能一眼看到种齐了）",
    );
}

/// ⑧ 越界检查：只允许写 `biz_account` / `biz_voucher_rule`，不许建表 / 改表 / 写凭证。
#[test]
fn v0384_never_touches_other_tables() {
    let raw = read_migration(V0384_UP);
    let sql = sql_only(&raw);

    let upper = sql.to_ascii_uppercase();
    for forbidden in ["CREATE TABLE", "ALTER TABLE", "DROP TABLE", "CREATE INDEX"] {
        assert!(
            !upper.contains(forbidden),
            "本迁移只种数据，不许出现 {forbidden}（改表结构属于别的卡，硬上会与并发卡撞车）"
        );
    }
    // 一条分录都不许写（只动科目树 + 规则登记）
    assert!(
        !upper.contains("INSERT INTO BIZ_VOUCHER_ENTRY")
            && !upper.contains("INSERT INTO BIZ_VOUCHER ")
            && !upper.contains("INSERT INTO BIZ_VOUCHER("),
        "本迁移不许写凭证 / 分录（那属 HR-39 过账逻辑，不是种子）"
    );

    let mut targets = dml_targets(&sql);
    targets.sort();
    targets.dedup();
    for target in &targets {
        assert!(
            target == "biz_account" || target == "biz_voucher_rule",
            "DML 目标只允许 biz_account / biz_voucher_rule，实际出现了 {target}"
        );
    }
    assert!(
        targets.contains(&"biz_account".to_string()) && targets.contains(&"biz_voucher_rule".to_string()),
        "两个目标表都应被写入（科目 + 规则）"
    );
}

/// ⑨ down 必须幂等 + 有数据护栏 + **不许把 V0377 的 `2211.01` 一起删掉**。
///
/// 这条是设计 §7.3 与本实现的有意差异：`2211.01` 通常归 V0377（版本更小、必然先跑），
/// 只回滚 V0384 时删掉它会让成本结算的人工计提凭证（贷 `2211.01`）失去科目。
#[test]
fn v0384_down_is_idempotent_guarded_and_keeps_v0377_account() {
    let down = read_migration(V0384_DOWN);
    let sql = sql_only(&down);

    // 护栏 A：待删科目有分录就拒绝
    assert_has(
        &sql,
        "V0384 down 拒绝执行：末级科目上还有历史分录",
        "down 必须拒绝删除有分录的科目（分录会指向不存在的科目）",
    );
    // 护栏 B：不许在裂树 / 删父留子 的库上继续
    assert_has(
        &sql,
        "V0384 down 拒绝执行：父科目已是裂树",
        "down 必须拒绝在「父科目 is_leaf=1 却挂着子科目」的库上继续改",
    );
    assert_has(
        &sql,
        "V0384 down 拒绝执行：本迁移种的科目下又被加了下级",
        "down 必须拒绝删除「自己挂了下级」的科目（会留孤儿）",
    );
    // 2211.01 只删「确由本迁移种下」的那一行
    assert_has(
        &sql,
        "OR (account_code = '2211.01' AND remark = 'ARCH-9-2 工资相关科目')",
        "2211.01 只许在归属本迁移时才删（V0377 种的那行要保留，否则成本结算失去科目）",
    );
    assert_has(
        &sql,
        "V0384 down 提示：% 保留未删",
        "保留 2211.01 时必须点名提示（否则下一个人以为 down 没跑干净）",
    );
    // 规则先删、科目后删、父科目最后翻回（顺序反转会造裂树）
    //   用 squash 后的串定位：down 里 `SET is_leaf    = 1` 有对齐空格。
    let flat = squash(&sql);
    let del_rule = flat
        .find("DELETE FROM biz_voucher_rule")
        .expect("down 必须删 2 条凭证规则");
    let del_acct = flat.find("DELETE FROM biz_account").expect("down 必须删本迁移种的科目");
    assert!(del_rule < del_acct, "规则必须先于科目删除（口径登记先撤）");
    let flip_back = flat
        .find("SET is_leaf = 1")
        .expect("down 必须在没有子科目时把父科目翻回末级");
    assert!(
        del_acct < flip_back,
        "父科目翻回末级必须在删子科目之后（否则立刻变成「末级挂子」的裂树）"
    );
    assert_has(
        &sql,
        "AND NOT EXISTS (SELECT 1 FROM biz_account c WHERE c.parent_id = p.id AND c.deleted_at IS NULL)",
        "翻回末级必须要求「已经没有未删子科目」（有子科目时保持非末级，与设计 §7.3 一致）",
    );
    // 反向自检
    for (snippet, why) in [
        (
            "V0384 down 自检失败：还有 % 个末级科目没删掉",
            "down 结尾必须断言科目删净",
        ),
        (
            "V0384 down 自检失败：还有 % 条工资凭证规则没删掉",
            "down 结尾必须断言规则删净",
        ),
        ("V0384 down 自检失败：回滚后有 % 个裂树节点", "down 结尾必须断言无裂树"),
        (
            "V0384 down 自检失败：% 条分录指向已删科目",
            "down 结尾必须断言无孤立分录",
        ),
    ] {
        assert_has(&sql, snippet, why);
    }
}

/// ⑩ 引擎拆分器必须把每个 `DO $$ … $$` 留在一条语句里。
///
/// 用框架自己的 `split_statements`（引擎执行迁移走的就是它）验证：
/// `$$` 内部的 `;` 若被当成语句分隔符，PG 会报语法错误，整个迁移事务回滚 ⇒ 服务起不来。
#[test]
fn v0384_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    for (rel, expected_do_blocks, tail) in [
        (V0384_UP, 3_usize, "RAISE NOTICE 'V0384 完成"),
        (V0384_DOWN, 4_usize, "RAISE NOTICE 'V0384 down 完成"),
    ] {
        let sql = read_migration(rel);
        let statements = split_statements(&sql, DialectKind::Postgres)
            .unwrap_or_else(|e| panic!("引擎拆分器无法拆分 {rel}（方言感知的 $$ 处理失败）: {e}"));

        let do_blocks: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
        assert_eq!(
            do_blocks.len(),
            expected_do_blocks,
            "{rel} 的 DO 块数不对（实际 {} 条）：> 预期说明 $$ 内部的 `;` 被当成分隔符了",
            do_blocks.len()
        );
        assert!(
            statements.iter().any(|s| s.contains(tail)),
            "{rel} 拆分后的最后一个 DO 块必须完整包含完成提示「{tail}」"
        );
        // 每个 DO 块的 BEGIN/END 必须配平（拆分成功但内容被截断时最容易漏掉 END IF）
        for block in &do_blocks {
            let upper = block.to_ascii_uppercase();
            let begin = upper.matches("BEGIN").count();
            let end = upper.matches("END IF").count() + upper.matches("END;").count();
            assert!(
                begin >= 1 && end >= 1,
                "{rel} 的某个 DO 块缺少 BEGIN/END 配平：\n{}",
                &block[..block.len().min(200)]
            );
        }
    }
}

/// ⑪ 本迁移与 V0377 的 9 个成本科目**不重叠**（除了设计 D-14 明说要共存的 `2211.01`）。
///
/// 撞码会让两卡的 `ON CONFLICT DO NOTHING` 互相顶掉对方想要的父级/名称，
/// 而哪张卡先跑取决于版本号 —— 设计 D-14 只允许 `2211.01` 这种明确登记过的重叠。
#[test]
fn v0384_accounts_do_not_collide_with_v0377_except_221101() {
    let v0377 = sql_only(&read_migration("business/V0377__cost_accounts.up.sql"));
    let v0384 = sql_only(&read_migration(V0384_UP));

    let v0377_codes = [
        "4001.01", "4001.02", "4001.03", "4101.01", "4101.02", "4101.03", "4101.04", "4101.09", "2211.01",
    ];
    for code in v0377_codes {
        let in_v0384 = v0384.contains(&format!("'{code}'"));
        if code == "2211.01" {
            assert!(
                in_v0384,
                "2211.01 必须在 V0384 里出现（设计 D-14：两卡幂等共存，谁先跑谁种）"
            );
        } else {
            assert!(
                !in_v0384,
                "{code} 是 V0377 的成本科目，V0384 不该碰（越界会与成本卡互相覆盖）"
            );
        }
    }
    // 反向：V0384 的独有科目不许出现在 V0377 里
    for (code, ..) in NINE_ACCOUNTS {
        if code == "2211.01" {
            continue;
        }
        assert!(
            !v0377.contains(&format!("'{code}'")),
            "{code} 是 V0384 的工资科目，V0377 不该种它（两卡的科目归属要清晰）"
        );
    }
}
