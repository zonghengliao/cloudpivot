//! 迁移 `V0416`（M1-15：间接法调整项配置 `biz_cashflow_adjust_item`）的静态不变量。
//!
//! 卡：t_ce938a2a（M1-15【M1·财务】现金流底稿：间接法调整项配置）。
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §4 M1 财务 的 M1-15 行；
//! 下游 M1-16（由底稿生成现金流量表）按本表的「口径」契约取数。
//!
//! # 这层测试钉死什么（都是会**静默算错账**的类型）
//!
//! 1. **配置表里混进金额列** ⇒ 配置与金额两个真相：改了配置金额不跟着变，谁也说不清以谁为准。
//!    本表是纯配置（9 列，无 NUMERIC），出数在 M1-16。
//! 2. **`MANUAL` 行带口径** ⇒ 留着不参与计算的模式串，就是「看起来配了、其实不用」的摆设。
//!    `ck_biz_cashflow_adjust_shape` 必须让这种行插不进去。
//! 3. **加「加 / 减」符号列** ⇒ 方向出现两个真相（符号说加、口径算出负数）。方向只能由
//!    4 个取数口径表达。
//! 4. **口径枚举少一个 / 改名** ⇒ 该行永远取 0，用户分不清「本期没发生」与「配错了」。
//! 5. **种子用 `ON CONFLICT DO UPDATE`** ⇒ 重放会覆盖管理员改过的名称 / 口径 / 顺序。
//! 6. **给 `item_code` / 期间加物理外键** ⇒ 配置换版、期间运维被卡住（AGENTS.md 弱引用口径）。
//! 7. **权限码没挂到「现金流量表」页面** ⇒ 孤儿按钮（`orphan_button_gate.py` 判红）。
//! 8. **down 先删菜单再删授权** ⇒ 外键冲突，回滚直接失败。
//! 9. **`DO $$ … $$` 被引擎拆分器切坏**（块内 `;` 被当分隔符）⇒ PG 语法错误、服务起不来。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up×2 / down×2 / 护栏反证）见 `bash scripts/verify/m115_cashflow_adjust_item_drill.sh`。

use std::path::PathBuf;

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

/// 去掉整行 `--` 注释：注释里会**引用**反面写法做教材，「禁止出现」类断言必须只看真 SQL。
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

/// 取语句块首行真正的 SQL（引擎拆分器把注释原样留在块里）。
fn sql_head(statement: &str) -> String {
    statement
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("--"))
        .unwrap_or("")
        .to_ascii_uppercase()
}

const V0416_UP: &str = "business/V0416__cashflow_adjust_item.up.sql";
const V0416_DOWN: &str = "business/V0416__cashflow_adjust_item.down.sql";

/// 本卡建的唯一样本表（M1-16 / M1-17 各有自己的卡）。
const OWN_TABLE: &str = "biz_cashflow_adjust_item";

/// 权限码（与 `cashflow_adjust::PERM_LIST` / `PERM_MANAGE` 逐字一致）。
const CODE_LIST: &str = "business:erp:cashflow-adjust:list";
const CODE_MANAGE: &str = "business:erp:cashflow-adjust:manage";

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
fn v0416_up_and_down_exist() {
    for rel in [V0416_UP, V0416_DOWN] {
        let path = app_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ② 只建这一张表，建表幂等，且不动任何存量表结构。
#[test]
fn v0416_creates_exactly_one_table_idempotently() {
    let sql = sql_only(&read_migration(V0416_UP));
    let squashed = squash(&sql);

    assert_has(
        &sql,
        "CREATE TABLE IF NOT EXISTS biz_cashflow_adjust_item (",
        "建表必须是 `CREATE TABLE IF NOT EXISTS biz_cashflow_adjust_item`（幂等 + 表名固定）",
    );
    let create_table_count = squashed.matches("CREATE TABLE").count();
    assert_eq!(
        create_table_count, 1,
        "本卡只许建 1 张表（{OWN_TABLE}），实际 {create_table_count} 处 CREATE TABLE —— 多出来的越界了"
    );
    for forbidden in ["ALTER TABLE", "DROP TABLE", "TRUNCATE"] {
        assert!(
            !squashed.contains(forbidden),
            "V0416 出现 {forbidden}：本卡只新建一张配置表，改/删存量表属别的卡"
        );
    }

    // DML 只许打自己的表 + 菜单/授权（种权限码是本卡范围内的事）
    for table in dml_targets(&sql) {
        assert!(
            [OWN_TABLE, "sys_menu", "sys_role_menu"].contains(&table.as_str()),
            "V0416 的 DML 打到了 {table} —— 只许写 {OWN_TABLE} / sys_menu / sys_role_menu"
        );
    }
    // 反向：不碰直接法分类表、底稿金额表、凭证
    for forbidden in ["biz_cashflow_item", "biz_cashflow_worksheet", "biz_voucher"] {
        assert!(
            !squashed.contains(&format!("INTO {forbidden}")),
            "V0416 往 {forbidden} 写数据了：直接法配置 / 底稿金额 / 凭证各有各的卡"
        );
    }
}

/// ③ 9 列 + 时间戳逐列形状；**没有金额列**（配置表不许存数）。
#[test]
fn v0416_columns_have_designed_shape_and_no_amount() {
    let sql = read_migration(V0416_UP);
    for (snippet, why) in [
        (
            "id UUID PRIMARY KEY DEFAULT gen_random_uuid()",
            "主键必须是 DB 侧 UUID 默认值",
        ),
        (
            "item_code VARCHAR(32) NOT NULL",
            "item_code 必须 VARCHAR(32) NOT NULL（落数身份，不能为空）",
        ),
        (
            "item_name VARCHAR(64) NOT NULL",
            "item_name 必须 VARCHAR(64) NOT NULL（报表行原文）",
        ),
        (
            "source_type VARCHAR(16) NOT NULL DEFAULT 'ACCOUNT'",
            "source_type 必须 VARCHAR(16) NOT NULL DEFAULT 'ACCOUNT'",
        ),
        (
            "calc_basis VARCHAR(24)",
            "calc_basis 必须 VARCHAR(24) 且可空（MANUAL 行为 NULL）",
        ),
        (
            "account_pattern VARCHAR(255)",
            "account_pattern 必须 VARCHAR(255) 且可空（MANUAL 行为 NULL）",
        ),
        (
            "sort_order INT NOT NULL DEFAULT 0",
            "sort_order 必须 INT NOT NULL DEFAULT 0",
        ),
        (
            "is_active SMALLINT NOT NULL DEFAULT 1",
            "is_active 必须 SMALLINT NOT NULL DEFAULT 1",
        ),
        ("remark TEXT", "remark 必须 TEXT（取数口径说明可以长）"),
        (
            "created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP",
            "created_at 必须有默认值",
        ),
        (
            "updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP",
            "updated_at 必须有默认值",
        ),
    ] {
        assert_has(&sql, snippet, why);
    }

    // 反向：配置表不许有金额 / 期间列（出了这张表就变成两个真相的地方）
    let code = squash(&sql_only(&sql));
    for forbidden in ["amount NUMERIC", "NUMERIC(18,4)", "period_code", "currency"] {
        assert!(
            !code.contains(forbidden),
            "V0416 的建表语句里出现了 {forbidden}：本表是**纯配置**（一行 = 一条取数口径），\
             金额与期间属于 M1-16 出表 / M1-17 单测，别混进来"
        );
    }
}

/// ④ 6 条 CHECK 一条都不能少（漏一条 = 一条脏数据入口）。
#[test]
fn v0416_keeps_all_six_check_constraints() {
    let sql = read_migration(V0416_UP);
    for (snippet, why) in [
        (
            "CONSTRAINT ck_biz_cashflow_adjust_source CHECK (source_type IN ('ACCOUNT', 'MANUAL'))",
            "取数来源必须二选一（写错的来源永远不会取数）",
        ),
        (
            "CONSTRAINT ck_biz_cashflow_adjust_basis CHECK (calc_basis IS NULL OR calc_basis IN ('CREDIT_NET', 'DEBIT_NET', 'BALANCE_INCREASE', 'BALANCE_DECREASE'))",
            "四个取数口径必须锁死（多一个 = 出表代码未定义的口径，少一个 = 标准模板缺行）",
        ),
        (
            "CONSTRAINT ck_biz_cashflow_adjust_active CHECK (is_active IN (0, 1))",
            "启用位必须 0/1",
        ),
        (
            "CONSTRAINT ck_biz_cashflow_adjust_name CHECK (length(btrim(item_name)) > 0)",
            "名称不许为空/全空白（空名字的调整项在报表上就是一条看不见的金额）",
        ),
        (
            "CONSTRAINT ck_biz_cashflow_adjust_code CHECK (length(btrim(item_code)) > 0)",
            "编码不许为空/全空白",
        ),
    ] {
        assert_has(&sql, snippet, why);
    }

    // 第 6 条是跨字段形状断言：两条腿都要在
    assert_has(
        &sql,
        "(source_type = 'ACCOUNT' AND calc_basis IS NOT NULL AND account_pattern IS NOT NULL AND length(btrim(account_pattern)) > 0)",
        "形状约束第一支：ACCOUNT 行必须口径 + 模式齐全（缺一个就永远取 0 或取错）",
    );
    assert_has(
        &sql,
        "(source_type = 'MANUAL' AND calc_basis IS NULL AND account_pattern IS NULL)",
        "形状约束第二支：MANUAL 行必须口径 / 模式皆空（否则就是摆设）",
    );

    let check_count = squash(&sql).matches("CONSTRAINT ck_biz_cashflow_adjust_").count();
    assert_eq!(
        check_count, 6,
        "本表应有 6 条 ck_biz_cashflow_adjust_* 约束（含 UNIQUE 的行形状约束），实际 {check_count} 条"
    );
}

/// ⑤ `item_code` 唯一 + 弱引用（不许物理外键）+ 取数顺序索引。
#[test]
fn v0416_unique_and_weak_reference() {
    let sql = read_migration(V0416_UP);
    assert_has(
        &sql,
        "CONSTRAINT uk_biz_cashflow_adjust_item UNIQUE (item_code)",
        "缺 item_code 唯一约束：两行同码 = 落数时按码定位到两行，谁也说不清取哪一行",
    );
    assert_has(
        &sql,
        "CREATE INDEX IF NOT EXISTS idx_biz_cashflow_adjust_order ON biz_cashflow_adjust_item (sort_order, item_code)",
        "缺出表排序索引 (sort_order, item_code)",
    );
    let code = squash(&sql_only(&sql));
    assert!(
        !code.contains("REFERENCES"),
        "V0416 出现了 REFERENCES：item_code 是自身唯一编码、不指向 biz_cashflow_item —— \
         物理外键会让分类配置换版这类正常运维被卡住（AGENTS.md 弱引用口径）"
    );
}

/// ⑥ 两条「决定不做」反向钉死：没有加/减符号列、没有软删列。
#[test]
fn v0416_has_no_sign_column_and_no_soft_delete() {
    let code = squash(&sql_only(&read_migration(V0416_UP)));
    // 只看**建表语句本身**：迁移其余部分要查 sys_menu.deleted_at（那是系统表，不是本表的列）
    let start = code
        .find("CREATE TABLE IF NOT EXISTS biz_cashflow_adjust_item")
        .expect("建表语句不见了");
    let end = code[start..]
        .find(");")
        .map(|offset| start + offset)
        .expect("建表语句没闭合");
    let ddl = &code[start..end];
    for forbidden in [" sign ", "deleted_at", "is_deduction"] {
        assert!(
            !ddl.contains(forbidden),
            "V0416 建表语句里出现了 {forbidden}：这是**决定不做**的列（方向由 4 个取数口径表达，\
             见迁移头注释与 cashflow_adjust 模块文档）。要改先改决定与测试，别顺手加\n建表语句：{ddl}"
        );
    }
}

/// ⑦ 12 行标准模板：`ON CONFLICT DO NOTHING`（重放不覆盖管理员改动）+ 4 口径 / 2 MANUAL。
#[test]
fn v0416_seeds_twelve_row_template_without_overwriting() {
    let sql = read_migration(V0416_UP);
    assert_has(
        &sql,
        "INSERT INTO biz_cashflow_adjust_item (item_code, item_name, source_type, calc_basis, account_pattern, sort_order, remark)",
        "种子插入必须显式列名（列顺序变化时不许把值写偏）",
    );
    assert_has(
        &sql,
        "ON CONFLICT (item_code) DO NOTHING",
        "种子必须 `ON CONFLICT (item_code) DO NOTHING`：重放时管理员改过的名称 / 口径 / 顺序必须保得住",
    );
    assert!(
        !squash(&sql_only(&sql)).contains("DO UPDATE"),
        "V0416 出现 DO UPDATE：重放会覆盖管理员改过的配置（名称 / 口径 / 顺序全被冲回模板）"
    );

    for (code, basis) in [
        ("ADJ_ASSET_IMPAIRMENT", "CREDIT_NET"),
        ("ADJ_DEPRECIATION", "CREDIT_NET"),
        ("ADJ_AMORT_INTANGIBLE", "CREDIT_NET"),
        ("ADJ_AMORT_LONGTERM", "CREDIT_NET"),
        ("ADJ_DISPOSAL_LOSS", "DEBIT_NET"),
        ("ADJ_FINANCE_COST", "DEBIT_NET"),
        ("ADJ_INVEST_LOSS", "DEBIT_NET"),
        ("ADJ_DEFERRED_TAX", "NULL"),
        ("ADJ_INVENTORY_DECREASE", "BALANCE_DECREASE"),
        ("ADJ_OPER_RECEIVABLE_DECREASE", "BALANCE_DECREASE"),
        ("ADJ_OPER_PAYABLE_INCREASE", "BALANCE_INCREASE"),
        ("ADJ_OTHER", "NULL"),
    ] {
        assert!(
            sql.contains(&format!("'{code}'")),
            "标准模板缺行 {code}（企业会计准则《现金流量表补充资料》间接法部分）"
        );
        assert!(
            sql.contains(basis) || basis == "NULL",
            "模板行 {code} 的取数口径 {basis} 不在迁移里"
        );
    }
    for basis in ["CREDIT_NET", "DEBIT_NET", "BALANCE_INCREASE", "BALANCE_DECREASE"] {
        assert!(
            sql.contains(basis),
            "四个口径里的 {basis} 在模板里一次都没用到 —— 口径是全量的，模板必须覆盖"
        );
    }
}

/// ⑧ 两个权限码：挂「现金流量表」页面、无 path、sort 30/40、只授超管 + 财务经理。
#[test]
fn v0416_seeds_two_button_codes_under_cash_flow_page() {
    let sql = read_migration(V0416_UP);
    assert_has(&sql, CODE_LIST, "缺读权限码（列表 / 字典都靠它）");
    assert_has(&sql, CODE_MANAGE, "缺写权限码（新增 / 编辑 / 启停都靠它）");
    assert_has(
        &sql,
        "'20000000-0000-0000-0000-000000001018'::uuid",
        "两个按钮必须挂在既有「现金流量表」页面 …001018 下（挂别处 = 侧栏/授权树里找不到入口）",
    );
    assert_has(
        &sql,
        "('20000000-0000-0000-0000-000000001790'::uuid,",
        "读码节点 id 必须是 …001790（V0416 的 id 段，已核对无占用）",
    );
    assert_has(
        &sql,
        "('20000000-0000-0000-0000-000000001800'::uuid,",
        "写码节点 id 必须是 …001800",
    );
    assert_has(
        &sql,
        "r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER')",
        "授权对象必须是 SUPER_ADMIN + FINANCE_MANAGER（财务经理是编调整项的主力）",
    );
    assert_has(
        &sql,
        "NOT EXISTS ( SELECT 1 FROM sys_role_menu rm",
        "授权必须用 NOT EXISTS 去重（重放不许攒重复行）",
    );
    // FINANCE_DIRECTOR 没有报表页码：授它 = 孤儿按钮（orphan_button_gate 判红）
    assert!(
        !squash(&sql_only(&sql)).contains("FINANCE_DIRECTOR"),
        "V0416 给 FINANCE_DIRECTOR 授权了：它没有现金流量表页面码，按钮会成为不可达的孤儿按钮"
    );
}

/// ⑨ 自检块：两个 `DO $$` 都有 RAISE，关键断言逐条点名。
#[test]
fn v0416_self_check_raises_exceptions() {
    let sql = read_migration(V0416_UP);
    let blocks: Vec<&str> = sql.split("DO $$").skip(1).collect();
    assert_eq!(
        blocks.len(),
        2,
        "V0416 应有 2 个 DO 块（前置护栏 / 结构 + 种子 + 菜单自检），实际 {} 个",
        blocks.len()
    );
    for (i, block) in blocks.iter().enumerate() {
        assert!(
            block.contains("RAISE EXCEPTION") || block.contains("RAISE NOTICE"),
            "第 {} 个 DO 块既没有 RAISE EXCEPTION 也没有 RAISE NOTICE —— 自检失败也拦不住迁移",
            i + 1
        );
    }
    for msg in [
        "biz_cashflow_item 不存在，请先应用 V0185",
        "「现金流量表」页面节点 …001018 不存在",
        "biz_cashflow_adjust_item 没建出来",
        "标准模板应有 12 行调整项",
        "种子行应全部启用",
        "MANUAL 行只有",
        "4 个取数口径应各被用一次",
        "MANUAL 行带取数口径竟然插得进去",
        "两个调整项按钮节点只建成",
        "两个码在 sys_menu 里应各恰好 1 行",
        "title 不合菜单规范",
        "组重复 sort_order",
        "授权数",
        "V0416 自检通过",
    ] {
        assert!(sql.contains(msg), "自检断言里必须覆盖「{msg}」（当前找不到）");
    }
}

/// ⑩ down：护栏 → 先删授权再删菜单 → 删表 → 自检零残留。
#[test]
fn v0416_down_is_ordered_and_guarded() {
    let down = read_migration(V0416_DOWN);
    assert_has(
        &down,
        "V0416 down 拒绝执行：间接法调整项里有",
        "down 必须有「有自建配置就拒绝回滚」的护栏（回滚 = 配置永久丢失）",
    );
    assert_has(
        &down,
        "DELETE FROM sys_role_menu WHERE menu_id IN",
        "down 必须先删授权（sys_role_menu 有外键指向 sys_menu）",
    );
    assert_has(&down, "DELETE FROM sys_menu WHERE id IN", "down 必须删两个按钮节点");
    assert_has(
        &down,
        "DROP TABLE IF EXISTS biz_cashflow_adjust_item",
        "down 必须用 DROP TABLE IF EXISTS 收口（重复执行不报错）",
    );

    // 顺序：授权删除必须在菜单删除之前
    let code = sql_only(&down);
    let grants_at = code.find("DELETE FROM sys_role_menu").expect("缺授权删除");
    let menus_at = code.find("DELETE FROM sys_menu").expect("缺菜单删除");
    let drop_at = code.find("DROP TABLE IF EXISTS").expect("缺删表");
    assert!(
        grants_at < menus_at && menus_at < drop_at,
        "down 的执行顺序必须是 授权 → 菜单 → 表（先删菜单会撞外键）"
    );

    // 不许用 'tbl'::regclass（表已删就抛错）
    assert!(
        !code.contains("'biz_cashflow_adjust_item'::regclass"),
        "down 里出现了 'biz_cashflow_adjust_item'::regclass：第二次执行时表不存在，这种写法直接抛错（改用 to_regclass）"
    );
    assert_has(
        &down,
        "仍然存在",
        "down 自检必须断言失败态（只 NOTICE 不 EXCEPTION 等于没验）",
    );
    assert_has(&down, "V0416 down 完成", "down 结尾必须有完成提示");
}

/// ⑪ 引擎拆分器：两个 DO 块各一条语句，建表 / 索引 / 注释条数正确。
#[test]
fn v0416_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    let sql = read_migration(V0416_UP);
    let statements =
        split_statements(&sql, DialectKind::Postgres).expect("引擎拆分器无法拆分 V0416（方言感知的 $$ 处理失败）");

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
            "拆分后的 DO 块必须以 END $$ 收口（被截断就语法错误）"
        );
    }

    let count_of = |prefix: &str| -> usize { statements.iter().filter(|s| sql_head(s).starts_with(prefix)).count() };
    assert_eq!(count_of("CREATE TABLE"), 1, "建表语句应成一条");
    assert_eq!(count_of("CREATE INDEX"), 1, "排序索引应成一条");
    assert_eq!(count_of("INSERT INTO"), 3, "三段 INSERT：模板 / 按钮节点 / 授权");
    assert_eq!(count_of("COMMENT ON"), 3, "表注释 + 2 条列注释应各成一条");
}
