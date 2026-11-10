//! 迁移 `V0377`（成本科目翻末级 + 种 9 个末级科目）的静态不变量。
//!
//! 卡：t_486e652d（COST-B1-6【成本】迁移 V0375）—— 设计依据
//! `docs/architecture/ARCH-9-成本核算升级设计.md` §3.3 ③（科目 DDL 逐字）、§1 D-14（科目补种表）、
//! §7.2（存量处理：翻非末级前先查历史分录）、§4.7（三张结转凭证的借贷科目）、拆卡清单 §8.1 的 B1-6。
//!
//! # 为什么号段是 V0377 而不是卡面的 V0375
//!
//! V0375/V0376 = B1-5（出库加列 / 入库 PRODUCTION），V0372/0373 = B1-3、V0374 = B1-4。
//! nik 迁移加载器对版本号做**跨目录全局唯一**校验，重号会让服务启动直接失败（AGENTS 约定 6）。
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs::migration_versions_are_unique_padded_and_have_down`
//! 统一覆盖，这里不重复。
//!
//! # 这层测试钉死什么
//!
//! 本迁移动的是**所有报表与凭证都依赖的科目树**，写错的后果全是「静默算错钱」这一类：
//!
//! 1. ★ **翻末级前没查历史分录** ⇒ 已有分录留在汇总科目上，报表按末级取数 / 年结按末级结转
//!    把这笔钱漏掉 ⇒ 跨年试算不平，且事后只能改库（AGENTS 约定 11 的同一条精神）。
//! 2. ★ **翻末级没种够叶子** ⇒ 记账闸拒掉一切 `借 4001/4101/2211`，但新叶子不存在 ⇒
//!    三张结转凭证（§4.7）全部失败，成本模块上线即不可用。
//! 3. ★ **叶子编码/父级/级次写错**（比如 4101.09 挂到 4001 下）⇒ 报表口径错、
//!    §4「制造费用按科目分摊」的规则匹配不到科目。
//! 4. ★ **种子把类别/方向硬编码**（而不是继承父级）⇒ 客户改过父级类别后，
//!    「制造费用挂在资产下」这种烂账直接进资产负债表。
//! 5. ★ **INSERT 没有 `ON CONFLICT DO NOTHING`** ⇒ 与人力资源卡（ARCH9-2）的 2211.01 撞键，
//!    并行上线时后跑的那张卡直接迁移失败；重复执行也不幂等。
//! 6. ★ **down 先翻父级再删子科目** ⇒ 出现「末级科目挂子科目」的裂树（凭证闸当它可记账、
//!    报表又当它汇总，同一笔钱两头算）。
//! 7. ★ **down 无护栏就删科目** ⇒ 分录指向不存在的科目（明细账/报表报「科目不存在」）。
//! 8. **迁移声明 `-- +no-transaction`** ⇒ 护栏 RAISE 之后前面已执行的语句**留在库里**，
//!    「不许翻」变成「翻了一半」。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（有分录被拦 ×3 / 干净库能翻 / up 幂等 / down 护栏 / down 幂等 / 回滚后重放）见
//! `docs/evidence/2026-11-10-cost-b1-6-v0377-account-leaf/02_account_leaf_drill.out`；
//! 空库全链重放 297/297 见同目录 `01_replay_fresh.out`。

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

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材（比如「不设 DEFAULT」），
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
        "{why}\n期望包含：{}\n实际：{squashed}",
        squash(snippet)
    );
}

fn assert_lacks(sql: &str, snippet: &str, why: &str) {
    let squashed = squash(sql);
    assert!(
        !squashed.contains(&squash(snippet)),
        "{why}\n不该包含：{}",
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

const V0377_UP: &str = "business/V0377__cost_accounts.up.sql";
const V0377_DOWN: &str = "business/V0377__cost_accounts.down.sql";

/// 设计 §3.3 ③ / §1 D-14 的 9 个末级科目：(编码, 名称, 父级编码)。
const DESIGN_LEAVES: [(&str, &str, &str); 9] = [
    ("4001.01", "生产成本-直接材料", "4001"),
    ("4001.02", "生产成本-直接人工", "4001"),
    ("4001.03", "生产成本-制造费用", "4001"),
    ("4101.01", "制造费用-折旧费", "4101"),
    ("4101.02", "制造费用-水电费", "4101"),
    ("4101.03", "制造费用-修理费", "4101"),
    ("4101.04", "制造费用-车间管理费", "4101"),
    ("4101.09", "制造费用-其他", "4101"),
    ("2211.01", "应付职工薪酬-工资", "2211"),
];

const PARENTS: [&str; 3] = ["4001", "4101", "2211"];

/// ① up / down 必须成对存在（缺 down = 回滚无路）。
#[test]
fn v0377_up_and_down_exist() {
    for rel in [V0377_UP, V0377_DOWN] {
        let path = backend_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
    // 版本号 4 位补零（AGENTS 约定 6）：V0377 而不是 V377 / V03770
    assert!(V0377_UP.contains("V0377__"), "迁移文件必须按 V0377__ 命名");
}

/// ② ★ 卡面验收：三科目历史分录非 0 必须 RAISE EXCEPTION，且**护栏在翻末级之前**。
///
/// 顺序反了会怎样：先翻末级再查分录，查到分录时 `4xxx` 已经变成汇总科目，
/// 迁移抛错回滚（默认事务）还好，一旦有人给它加 `-- +no-transaction`，
/// 库里就留下「汇总科目上挂着分录」的半中间态 —— 正是 §7.2 要防的那个状态。
#[test]
fn v0377_guard_checks_history_entries_before_flipping_leaf() {
    let raw = read_migration(V0377_UP);
    let code = sql_only(&raw);
    let squashed = squash(&code);

    // 分录表护栏：按 account_code 精确统计（不用 LIKE —— 4001.01 不该挡 4001 的翻末级判断）
    assert_has(
        &code,
        "SELECT count(*) INTO v_entries FROM biz_voucher_entry e WHERE e.account_code = rec.code;",
        "护栏必须按 account_code = 待翻科目编码统计历史分录（§7.2 给的 SQL）",
    );
    assert_has(
        &code,
        "WHERE e.account_code IN ('4001', '4101', '2211');",
        "翻转后还要再查一次：三个汇总科目上不许有任何分录残留（自检 A ⑤）",
    );

    // 三个科目都必须被遍历到（只查一个 = 另外两个裸奔）
    for parent in PARENTS {
        assert!(
            squashed.contains(&format!("('{parent}')")) || squashed.contains(&format!("'{parent}',")),
            "护栏的科目清单里没有 {parent} —— 三个科目都要查"
        );
    }

    // 拒绝文案必须点名科目与条数（运维要能照着找凭证）
    assert_has(
        &code,
        "RAISE EXCEPTION 'V0377 拒绝执行：%",
        "护栏失败必须 RAISE EXCEPTION（不是 NOTICE / WARNING）",
    );
    assert!(
        squashed.contains("%s 有 %s 条分录"),
        "拒绝文案必须点名「科目 + 条数」，运维才知道要改哪些凭证"
    );

    // 位置：分录护栏必须在 UPDATE biz_account ... is_leaf = 0 之前
    let pos_guard = squashed
        .find("FROM biz_voucher_entry e WHERE e.account_code = rec.code")
        .expect("找不到分录护栏的查询");
    let pos_flip = squashed
        .find("UPDATE biz_account SET is_leaf = 0")
        .expect("找不到「翻非末级」的 UPDATE");
    assert!(
        pos_guard < pos_flip,
        "分录护栏必须排在「翻非末级」之前（否则查到分录时科目已经翻过去了）"
    );

    // 事务性：整份迁移必须能被引擎包在单事务里（否则护栏抛错也留半中间态）
    assert_lacks(
        &raw,
        "+no-transaction",
        "V0377 不许声明 -- +no-transaction：护栏 RAISE 后必须整体回滚（git blame 前请看 migrate/exec.rs 的默认值）",
    );
}

/// ③ ★ 翻末级的 UPDATE 必须幂等，且只碰 is_leaf / updated_at。
///
/// 不动 `account_name` / `account_category` / `balance_direction` / `remark`：
/// 客户的科目名与类别是主数据，翻级次不该顺手覆盖（V0158/V0160 的补种科目都带自己的 remark）。
#[test]
fn v0377_flip_update_is_idempotent_and_minimal() {
    let code = sql_only(&read_migration(V0377_UP));
    let squashed = squash(&code);

    assert_has(
        &code,
        "UPDATE biz_account SET is_leaf = 0, updated_at = CURRENT_TIMESTAMP
          WHERE account_code IN ('4001', '4101', '2211')
            AND deleted_at IS NULL
            AND is_leaf = 1;",
        "翻非末级的 UPDATE 必须带 `AND is_leaf = 1`（幂等：第二遍 0 行）与 `deleted_at IS NULL`",
    );
    assert_eq!(
        squashed.matches("UPDATE biz_account SET is_leaf = 0").count(),
        1,
        "翻非末级只许有一条 UPDATE（多条 = 有人把三个科目写成了三段，容易漏改）"
    );
    // 反面：UPDATE 里不许出现别的列
    let update_line = squashed
        .split("UPDATE biz_account SET is_leaf = 0")
        .nth(1)
        .and_then(|rest| rest.split("WHERE").next())
        .expect("取不到 UPDATE 的 SET 子句");
    for forbidden in ["account_name", "account_category", "balance_direction", "remark"] {
        assert!(
            !update_line.contains(forbidden),
            "翻非末级的 UPDATE 顺手改了 {forbidden}：客户的科目主数据不该被级次变更覆盖（{update_line}）"
        );
    }
}

/// ④ ★ 9 个末级科目的编码 / 名称 / 父级必须与设计 §3.3 逐字一致，且全部来自同一张 VALUES 表。
#[test]
fn v0377_seeds_exactly_the_nine_design_leaves() {
    let code = sql_only(&read_migration(V0377_UP));

    for (leaf_code, name, parent) in DESIGN_LEAVES {
        assert_has(
            &code,
            &format!("('{leaf_code}', '{name}', '{parent}')"),
            &format!("设计 §3.3 ③ / D-14 要求种 {leaf_code} = 「{name}」，父级 {parent}"),
        );
    }

    // 恰好 9 行（多一行 = 顺手加了设计外的科目，报表口径要重新对）
    // 注意：只取 INSERT 本身（到 ON CONFLICT 为止），否则会把后面自检块里的编码也数进来。
    let insert_block = code
        .split("INSERT INTO biz_account")
        .nth(1)
        .and_then(|rest| rest.split("ON CONFLICT (account_code) DO NOTHING").next())
        .expect("找不到种末级科目的 INSERT");
    let seeded: Vec<&str> = insert_block
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            DESIGN_LEAVES
                .iter()
                .map(|(c, _, _)| *c)
                .find(|c| line.contains(&format!("('{c}'")))
        })
        .collect();
    assert_eq!(
        seeded.len(),
        9,
        "种末级科目的 VALUES 里应恰好 9 行，实际 {} 行：{seeded:?}",
        seeded.len()
    );

    // 级次 / 类别 / 方向必须**继承父级**，不许硬编码
    assert_has(
        &code,
        "p.level + 1",
        "子科目级次必须 = 父级级次 + 1（硬编码 2 在父级不是 1 级时会错）",
    );
    assert_has(
        &code,
        "p.account_category",
        "子科目类别必须继承父级（硬编码 COST 会让客户改过类别的父级对不上）",
    );
    assert_has(
        &code,
        "p.balance_direction",
        "子科目余额方向必须继承父级（制造费用若被改成贷方，硬编码 DEBIT 就是烂账）",
    );
    let squashed = squash(&code);
    for hardcoded in ["'COST'", "'DEBIT'", "'CREDIT'"] {
        assert!(
            !squashed.contains(&format!(", {hardcoded},")),
            "种科目时出现了硬编码的 {hardcoded}：类别/方向必须从父级继承（设计 §3.3 ③ 的 INSERT 就是用 p.account_category / p.balance_direction）"
        );
    }

    // 必须挂父级的**id**（不是编码），且父级未软删
    assert_has(
        &code,
        "JOIN biz_account p ON p.account_code = v.parent_code AND p.deleted_at IS NULL",
        "种子科目必须 join 父级取 id，且父级未软删（软删父级下种子科目 = 界面上看不见）",
    );
}

/// ⑤ ★ INSERT 必须幂等（`ON CONFLICT (account_code) DO NOTHING`）。
///
/// 两个理由，缺一个都会炸：
/// · 与人力资源卡（ARCH9-2）的 2211.01 撞键（设计 D-14 明说幂等共存）；
/// · 迁移重复执行（客户重放 / 回滚后重放）不许报错、不许种出重复科目。
#[test]
fn v0377_seed_insert_is_idempotent_on_account_code() {
    let code = sql_only(&read_migration(V0377_UP));
    let squashed = squash(&code);

    assert_has(
        &code,
        "ON CONFLICT (account_code) DO NOTHING;",
        "种末级科目必须 ON CONFLICT (account_code) DO NOTHING（uk_biz_account_code 是全表唯一索引）",
    );
    assert_eq!(
        squashed.matches("INSERT INTO biz_account").count(),
        1,
        "只许有一条 INSERT INTO biz_account（9 个科目必须来自同一张 VALUES 表，方便一眼对设计稿）"
    );
    // 幂等的另一半：插入前不许先 DELETE（那会连带删掉已经有分录的科目）
    assert_lacks(
        &code,
        "DELETE FROM biz_account",
        "up 不许 DELETE 科目（up 只翻级次 + 种新科目）",
    );
}

/// ⑥ 迁移的 DML 只碰 `biz_account`：不建表、不删表、不动单据与凭证。
#[test]
fn v0377_touches_only_biz_account() {
    let code = sql_only(&read_migration(V0377_UP));
    let squashed = squash(&code);

    for forbidden in ["CREATE TABLE", "DROP TABLE", "TRUNCATE", "ALTER TABLE"] {
        assert!(
            !squashed.contains(forbidden),
            "V0377 出现了 {forbidden}：本卡只改科目树（biz_account 的行），不建表不改表结构"
        );
    }
    let targets = dml_targets(&code);
    assert!(!targets.is_empty(), "V0377 应该至少有一条 DML（翻级次 / 种科目）");
    for table in &targets {
        assert_eq!(
            table, "biz_account",
            "V0377 对 {table} 做了 DML：本卡只动科目表，凭证/单据/成本表一行都不许写"
        );
    }
    // 反向：别的表只允许出现在**查询**里（护栏 / 自检读它们）
    for read_only in ["biz_voucher_entry", "biz_report_item"] {
        assert!(
            squashed.contains(read_only),
            "自检里应查询 {read_only}（分录为 0 / 存货报表项含 4001、4101）"
        );
    }
}

/// ⑦ 自检要覆盖三块：前置护栏 / 形态 / 口径，每块每个失败路径都 `RAISE EXCEPTION`。
#[test]
fn v0377_self_checks_raise_exceptions() {
    let up = read_migration(V0377_UP);

    assert_eq!(
        up.matches("DO $$").count(),
        3,
        "V0377 应有 3 个 DO 块（前置护栏 / 形态 / 口径）"
    );

    let key_messages = [
        // 前置护栏
        "V0377 拒绝执行：biz_voucher_entry 不存在",
        "V0377 拒绝执行：上级科目 % 不存在",
        "V0377 拒绝执行：上级科目 % 已被软删除",
        "V0377 拒绝执行：% —— 这些汇总科目还有历史分录",
        "V0377 拒绝执行：末级科目编码已被占用且形态不符",
        "V0377 前置护栏通过",
        // 形态自检
        "V0377 自检失败：上级科目没翻成非末级",
        "V0377 自检失败：末级科目形态不对",
        "V0377 自检失败：4001/4101/2211 下应恰有 9 个末级科目",
        "V0377 自检失败：还有 % 个「末级科目却挂着子科目」的裂树节点",
        "V0377 自检失败：汇总科目上仍有 % 条分录",
        "V0377 形态断言通过",
        // 口径自检
        "V0377 自检失败：记账闸只会拒 % 个汇总科目",
        "V0377 自检失败：可记账的末级成本科目只有 % 个",
        "V0377 自检失败：资产负债表「存货」项的科目模式不再同时包含 4001 与 4101",
        "V0377 口径断言通过",
        // 名称差异只提示不拦（显示层差异不该挡住升级）
        "V0377 提示：以下末级科目的名称与设计稿",
    ];
    for msg in key_messages {
        assert!(
            up.contains(msg),
            "自检断言里必须有一条覆盖「{msg}」（当前文件里找不到）"
        );
    }

    let parts: Vec<&str> = up.split("DO $$").skip(1).collect();
    assert_eq!(parts.len(), 3, "V0377 的 DO 块数量不对");
    for (i, block) in parts.iter().enumerate() {
        assert!(
            block.contains("RAISE EXCEPTION"),
            "V0377 第 {} 个 DO 块没有任何 RAISE EXCEPTION —— 自检失败也不会拦住迁移",
            i + 1
        );
    }
}

/// ⑧ 口径断言必须真的断言「记账闸两侧」与「报表前缀合并」，而不是只做形态检查。
///
/// 翻末级最容易犯的错是「翻了但没种够」：闸拒掉 4001，叶子又不存在 ⇒ 成本模块记账全 400。
/// 报表侧则靠 `report.rs::match_account` 的**前缀匹配**（`code == token || code.starts_with("token.")`）
/// 把 4001.01 并进「存货」项 —— 前提是 V0183 的 INVENTORY 模式里还有 4001/4101。
#[test]
fn v0377_asserts_posting_gate_and_report_rollup() {
    let code = sql_only(&read_migration(V0377_UP));

    assert_has(
        &code,
        "AND is_leaf = 0 AND account_code IN ('4001', '4101', '2211');",
        "必须断言三个汇总科目全部落在 is_leaf = 0（voucher.rs::non_leaf_codes 的拒绝面）",
    );
    assert_has(
        &code,
        "IF v_gate_reject <> 3 THEN",
        "拒绝面必须是「恰好 3 个」（少一个 = 有科目仍是末级，允许汇总级记账）",
    );
    assert_has(
        &code,
        "IF v_gate_allow <> 9 THEN",
        "放行面必须是「恰好 9 个」（少一个 = 翻完末级没种够，三张结转凭证会全失败）",
    );
    assert_has(
        &code,
        "WHERE report_code = 'BALANCE_SHEET' AND item_code = 'INVENTORY'",
        "必须断言 V0183 的资产负债表「存货」项映射还在（4001/4101 靠它进存货）",
    );
    assert_has(
        &code,
        "AND account_pattern LIKE '%4001%' AND account_pattern LIKE '%4101%'",
        "存货项的科目模式必须同时含 4001 与 4101（前缀匹配才能把新叶子并进存货）",
    );
}

/// ⑨ ★ down 顺序：**先删子科目再翻回父级**，且两道数据护栏都在最前面。
///
/// 顺序反了会怎样：父级先翻回 is_leaf=1，此时它下面还挂着 9 个子科目 ⇒
/// 「末级科目挂子科目」的裂树：凭证闸放行记账到父级，报表又把父级当汇总不取数，
/// 同一笔钱两头算；而且这不是一条 SQL 能补回来的状态（只能改库）。
#[test]
fn v0377_down_deletes_leaves_before_flipping_parents_back() {
    let down = read_migration(V0377_DOWN);
    let squashed = squash(&sql_only(&down));

    let pos_guard_entries = squashed
        .find("FROM biz_voucher_entry e WHERE e.account_code IN (")
        .expect("down 必须护栏：末级科目上有分录就拒绝（删科目会让分录指向不存在的科目）");
    let pos_guard_foreign = squashed
        .find("V0377 down 拒绝执行：上级科目下还有本迁移之外的子科目")
        .expect("down 必须护栏：父级下有本迁移之外的子科目就拒绝（会造出裂树）");
    let pos_delete = squashed
        .find("DELETE FROM biz_account WHERE account_code IN (")
        .expect("down 必须删掉 9 个末级科目");
    let pos_flip = squashed
        .find("UPDATE biz_account p SET is_leaf = 1")
        .expect("down 必须把三个上级科目翻回末级");

    assert!(
        pos_guard_entries < pos_delete,
        "分录护栏必须在删科目之前（否则删完才发现有分录，回滚已不可逆）"
    );
    assert!(
        pos_guard_foreign < pos_delete,
        "归属护栏必须在删科目之前（先看清父级下面还有什么）"
    );
    assert!(
        pos_delete < pos_flip,
        "必须先删 9 个末级科目、再把父级翻回末级（反了就出现「末级挂子」的裂树）"
    );

    assert!(
        squashed.contains("V0377 down 拒绝执行：末级科目上还有历史分录"),
        "拒绝文案要点名"
    );
    assert!(
        squashed.contains("AND p.is_leaf = 0 AND NOT EXISTS (SELECT 1 FROM biz_account c WHERE c.parent_id = p.id"),
        "翻回末级必须带两个条件：`is_leaf = 0`（幂等）+ `NOT EXISTS 子科目`（不造裂树）"
    );
    assert!(
        squashed.contains("RAISE NOTICE 'V0377 down 完成"),
        "down 结尾必须有反向自检（9 个编码全无 / 三科目末级 / 无裂树 / 无孤立分录）"
    );
}

/// ⑩ down 只删**本迁移种的那 9 个编码**：不删凭证、不删单据、不碰别的科目。
#[test]
fn v0377_down_only_deletes_the_nine_codes() {
    let down = sql_only(&read_migration(V0377_DOWN));
    let squashed = squash(&down);

    for (code, _, _) in DESIGN_LEAVES {
        assert!(
            squashed.contains(&format!("'{code}'")),
            "down 的清单里必须有 {code}（漏一个 = 回滚后留下孤立末级科目）"
        );
    }
    // DELETE 只许打在 biz_account 上
    let targets = dml_targets(&down);
    assert!(!targets.is_empty(), "down 应该有 DELETE / UPDATE");
    for table in &targets {
        assert_eq!(
            table, "biz_account",
            "V0377 down 对 {table} 做了 DML：回滚只拆科目树，凭证/单据一行都不许删"
        );
    }
    assert_lacks(&down, "DELETE FROM biz_voucher", "down 不许删凭证（回滚不丢账）");
    assert_lacks(&down, "DROP TABLE", "down 不许删表");
    // 护栏里要能识别「分录表已不存在」（凭证模块先回滚的场景），否则 down 不幂等
    assert!(
        squashed.contains("to_regclass('public.biz_voucher_entry') IS NULL"),
        "down 的护栏必须能识别「分录表不存在」并跳过（否则凭证模块先回滚时 down 直接抛错）"
    );
}

/// ⑪ 引擎拆分器必须把 `DO $$ … $$` 各留成一条语句（`$$` 内部的 `;` 不能被当分隔符）。
#[test]
fn v0377_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    for (rel, do_blocks) in [(V0377_UP, 3usize), (V0377_DOWN, 3usize)] {
        let sql = read_migration(rel);
        let statements = split_statements(&sql, DialectKind::Postgres)
            .unwrap_or_else(|e| panic!("引擎拆分器无法拆分 {rel}（方言感知的 $$ 处理失败）: {e}"));

        let dos: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
        assert_eq!(
            dos.len(),
            do_blocks,
            "{rel} 的 DO 块必须各被拆成**一条**语句（实际 {} 条）：多了说明 $$ 内部的 `;` 被当成分隔符了",
            dos.len()
        );
        for block in &dos {
            assert!(
                block.contains("RAISE ") || block.contains("RAISE("),
                "{rel} 拆分后的 DO 块必须完整包含断言（被截断就不会拦住迁移）"
            );
            let tail = squash(block);
            assert!(
                tail.ends_with("END $$;") || tail.ends_with("END $$"),
                "{rel} 拆分后的 DO 块必须以 END $$ 收口（被截断就会语法错误）：结尾是「{}」",
                tail.chars()
                    .rev()
                    .take(40)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
            );
        }
    }

    // up 的语句构成：3 个 DO + 1 条 UPDATE（翻级次）+ 1 条 INSERT（种科目）
    let up = read_migration(V0377_UP);
    let up_statements = split_statements(&up, DialectKind::Postgres).expect("up 拆分失败");
    let updates = up_statements
        .iter()
        .filter(|s| sql_head(s).starts_with("UPDATE"))
        .count();
    let inserts = up_statements
        .iter()
        .filter(|s| sql_head(s).starts_with("INSERT"))
        .count();
    assert_eq!(updates, 1, "up 应恰好有 1 条 UPDATE（翻非末级），实际 {updates}");
    assert_eq!(inserts, 1, "up 应恰好有 1 条 INSERT（种 9 个末级科目），实际 {inserts}");
}
