//! 迁移 `V0375`（出库加 `cost_object_id`）/ `V0376`（入库加 `PRODUCTION` + 两列）的静态不变量。
//!
//! 卡：t_b2c9aa3d（COST-B1-5【成本】迁移 V0373/V0374）—— 设计依据
//! `docs/architecture/ARCH-9-成本核算升级设计.md` §3.3 ①②、§1 D-11、§4.3 领料口径、
//! §4.7 结算（完工入库）、拆卡清单 §8.1 的 B1-5。
//!
//! # 为什么号段是 V0375/V0376 而不是卡面的 V0373/V0374
//!
//! V0372/V0373 = B1-3（分摊两表 / 期间两表）、V0374 = B1-4（二期三表），
//! nik 迁移加载器对版本号做跨目录全局唯一校验，重号会让服务启动直接失败（AGENTS 约定 6）。
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs::migration_versions_are_unique_padded_and_have_down`
//! 统一覆盖，这里不重复。
//!
//! # 这层测试钉死什么
//!
//! 本卡动的是**两张跑着几十万行历史单据的存量表**，错误只在两种时刻炸：
//! 部署时（加 NOT NULL / 带默认值 ⇒ 全表重写或直接失败）和回滚时（删列丢数据）。
//! 这里把「写对了」的形状固化在 `cargo test` 阶段，覆盖的都是**静默损坏**的类型：
//!
//! 1. ★ **新列带了 `NOT NULL` 或 `DEFAULT`** ⇒ 全表重写（锁表）+ 给历史单据凭空编值。
//! 2. ★ **给成本对象建物理外键** ⇒ 违反设计 §3.4 弱引用，主数据软删锁死历史单据。
//! 3. ★ **`ck_biz_si_type` 被「替换」而不是「放宽」**（丢了 PURCHASE/RETURN/OTHER/SOURCELESS
//!    任何一个）⇒ 存量单据全部变成非法行，下一次 UPDATE 就炸。
//! 4. ★ **`ck_biz_si_production` 少要素**（缺 cost_period_id / 允许供应商 / 允许无源）
//!    ⇒ 完工入库单红冲时查不到单据、或界面上出现「生产入库有供应商」。
//! 5. ★ **down 顺序反了**（先回滚值域再护数据）⇒ 存量 PRODUCTION 行让旧 4 值 CHECK 加不回去，
//!    回滚直接失败在最后一步，库停在半中间。
//! 6. ★ **down 无数据护栏就删列** ⇒ 挂着的成本对象静默消失（业务信息真金白银地丢）。
//! 7. **顺手动了别的约束**（`ck_biz_si_supplier_scope` / `ck_biz_so_type`）⇒ 与 V0330 / V0203 撞车。
//! 8. **写业务行**（迁移里 UPDATE/INSERT 存量单据）⇒ 本卡只改结构，一行都不许改。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up×3 幂等 / 存量指纹 / PRODUCTION 活体插入 / down 护栏反证 / down×2 / 重放）结果见
//! `docs/evidence/2026-11-10-cost-b1-5-v0375-v0376/02_up_down_drill.out`；
//! 空库全链重放 296/296 见同目录 `01_replay_fresh.out`。

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

const V0375_UP: &str = "business/V0375__stock_out_cost_object.up.sql";
const V0375_DOWN: &str = "business/V0375__stock_out_cost_object.down.sql";
const V0376_UP: &str = "business/V0376__stock_in_production.up.sql";
const V0376_DOWN: &str = "business/V0376__stock_in_production.down.sql";

/// ① up / down 必须成对存在（缺 down = 回滚无路）。
#[test]
fn v0375_v0376_up_and_down_exist() {
    for rel in [V0375_UP, V0375_DOWN, V0376_UP, V0376_DOWN] {
        let path = backend_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ② V0375 只加一列，且必须 `IF NOT EXISTS` + 可空 + 无默认值。
///
/// 带 `DEFAULT` 会触发全表重写（设计 §7.2「加可空列，不设默认值」），带 `NOT NULL`
/// 在存量表上直接失败；`DEFAULT gen_random_uuid()` 之流更糟 —— 给每张历史出库单
/// 凭空编一个成本对象。
#[test]
fn v0375_adds_exactly_one_nullable_column_idempotently() {
    let raw = read_migration(V0375_UP);
    let code = sql_only(&raw);

    assert_has(
        &code,
        "ALTER TABLE biz_stock_out ADD COLUMN IF NOT EXISTS cost_object_id UUID;",
        "V0375 必须用 ADD COLUMN IF NOT EXISTS 加 cost_object_id（幂等 + 可空）",
    );

    let add_cols: Vec<&str> = code
        .lines()
        .filter(|line| line.to_ascii_uppercase().contains("ADD COLUMN"))
        .collect();
    assert_eq!(
        add_cols.len(),
        1,
        "V0375 只许加 1 列（cost_object_id），实际 {} 行",
        add_cols.len()
    );
    for line in add_cols {
        assert!(
            !line.contains("DEFAULT"),
            "V0375 的加列不许带 DEFAULT（全表重写 + 给历史单据编值）：{line}"
        );
        assert!(
            !line.to_ascii_uppercase().contains("NOT NULL"),
            "V0375 的加列不许 NOT NULL（存量行全 NULL，加 NOT NULL 直接失败）：{line}"
        );
        assert!(
            !line.contains("REFERENCES"),
            "V0375 的加列不许带 REFERENCES（主数据弱引用）：{line}"
        );
    }

    // 本卡不动出库类型值域（V0203 的 ISSUE/SCRAP/LOSS/OTHER 一个都不许改）
    let squashed = squash(&code);
    assert!(
        !squashed.contains("ck_biz_so_type"),
        "V0375 动了 ck_biz_so_type：出库类型值域属 V0203，本卡只加「挂哪个成本对象」"
    );
    for forbidden in ["CREATE TABLE", "DROP TABLE", "TRUNCATE"] {
        assert!(
            !squashed.contains(forbidden),
            "V0375 出现了 {forbidden}：本卡只给存量表加列，不建表不删表"
        );
    }
    assert!(
        dml_targets(&squashed).is_empty(),
        "V0375 里有 DML（{:?}）：加列迁移不许写业务行，存量数据必须一个值都不动",
        dml_targets(&squashed)
    );
}

/// ③ V0375 的索引必须是**局部索引**（`WHERE cost_object_id IS NOT NULL`）。
///
/// 存量行全 NULL，建全量索引等于白占空间；而「按成本对象查领料单」是成本归集的
/// 唯一入口（§4.5 MATERIAL 扫描），必须有索引。
#[test]
fn v0375_index_is_partial() {
    let code = sql_only(&read_migration(V0375_UP));
    assert_has(
        &code,
        "CREATE INDEX IF NOT EXISTS idx_biz_so_cost_obj
            ON biz_stock_out (cost_object_id) WHERE cost_object_id IS NOT NULL;",
        "idx_biz_so_cost_obj 必须是「IF NOT EXISTS + 部分索引（非空行）」",
    );
    assert_eq!(
        squash(&code).matches("CREATE INDEX").count(),
        1,
        "V0375 只应有 1 条索引（idx_biz_so_cost_obj）"
    );
}

/// ④ V0376 加两列，都可空、无默认、无外键、`IF NOT EXISTS`。
#[test]
fn v0376_adds_two_nullable_columns_idempotently() {
    let code = sql_only(&read_migration(V0376_UP));

    for col in ["cost_object_id", "cost_period_id"] {
        assert_has(
            &code,
            &format!("ALTER TABLE biz_stock_in ADD COLUMN IF NOT EXISTS {col} UUID;"),
            &format!("V0376 必须用 ADD COLUMN IF NOT EXISTS 加 {col}（幂等 + 可空）"),
        );
    }

    let add_cols: Vec<&str> = code
        .lines()
        .filter(|line| line.to_ascii_uppercase().contains("ADD COLUMN"))
        .collect();
    assert_eq!(
        add_cols.len(),
        2,
        "V0376 只许加 2 列（cost_object_id / cost_period_id）"
    );
    for line in add_cols {
        assert!(!line.contains("DEFAULT"), "V0376 的加列不许带 DEFAULT：{line}");
        assert!(
            !line.to_ascii_uppercase().contains("NOT NULL"),
            "V0376 的加列不许 NOT NULL（存量行没有值）：{line}"
        );
        assert!(
            !line.contains("REFERENCES"),
            "V0376 的加列不许带 REFERENCES（主数据/批次弱引用）：{line}"
        );
    }

    let squashed = squash(&code);
    for forbidden in ["CREATE TABLE", "DROP TABLE", "TRUNCATE"] {
        assert!(
            !squashed.contains(forbidden),
            "V0376 出现了 {forbidden}：本卡只给存量表加列"
        );
    }
    // 弱引用：全迁移不许出现任何物理外键定义（REFERENCES 只允许出现在注释里，已被 sql_only 剔掉）
    assert!(
        !squashed.contains("REFERENCES"),
        "V0376 里出现了 REFERENCES：cost_object_id / cost_period_id 必须弱引用（设计 §3.4）"
    );
}

/// ⑤ ★ `ck_biz_si_type` 必须是**放宽**（4 值 ⊂ 5 值），不是替换。
///
/// 丢掉任何一个历史值 ⇒ 存量单据立刻变成非法行；少 `DROP CONSTRAINT IF EXISTS`
/// ⇒ 第二次执行 `ADD CONSTRAINT` 撞名失败（PG 没有 `ADD CONSTRAINT IF NOT EXISTS`）。
#[test]
fn v0376_widens_in_type_domain_keeping_all_history() {
    let code = sql_only(&read_migration(V0376_UP));
    let squashed = squash(&code);

    assert_has(
        &code,
        "ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_type;",
        "重建 ck_biz_si_type 前必须 DROP CONSTRAINT IF EXISTS（幂等）",
    );
    assert_has(
        &code,
        "ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_type
            CHECK (in_type IN ('PURCHASE','RETURN','OTHER','SOURCELESS','PRODUCTION'));",
        "ck_biz_si_type 必须是 5 值：历史 4 值 + PRODUCTION（设计 §3.3 ②）",
    );
    assert_eq!(
        squashed.matches("ADD CONSTRAINT ck_biz_si_type").count(),
        1,
        "ck_biz_si_type 只许 ADD 一次（多次 = 语义二义）"
    );

    // 历史 4 值一个都不能丢
    for legacy in ["PURCHASE", "RETURN", "OTHER", "SOURCELESS"] {
        assert!(
            squashed.contains(&format!("'{legacy}'")),
            "ck_biz_si_type 丢了历史值 {legacy} —— 存量单据会全部变成非法行"
        );
    }
    // 也不许顺手改状态 / 供应商两个约束（同一对象族，属 V0145/V0330）
    for other in ["ck_biz_si_status", "ck_biz_si_supplier_scope"] {
        assert!(
            !squashed.contains(&format!("ADD CONSTRAINT {other}")),
            "V0376 顺手 ADD 了 {other}：与 V0145/V0330 的语义撞车（本卡只加 PRODUCTION 一件事）"
        );
    }
}

/// ⑥ ★ `ck_biz_si_production` 必须与设计 §3.3 逐字一致。
///
/// 四要素缺一条就是一个脏数据入口：缺 `cost_period_id` ⇒ 红冲查不到该批次的入库单；
/// 允许 `supplier_id` ⇒ 界面上出现「生产入库有供应商」；允许 `is_sourceless` ⇒
/// 「无源的完工入库」既没有采购单也没有结算批次，永远对不上账。
#[test]
fn v0376_production_constraint_matches_design_verbatim() {
    let code = sql_only(&read_migration(V0376_UP));

    assert_has(
        &code,
        "ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_production;",
        "重建 ck_biz_si_production 前必须 DROP CONSTRAINT IF EXISTS（幂等）",
    );
    assert_has(
        &code,
        "ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_production
            CHECK (in_type <> 'PRODUCTION'
                   OR (cost_object_id IS NOT NULL AND cost_period_id IS NOT NULL
                       AND supplier_id IS NULL AND is_sourceless = FALSE));",
        "ck_biz_si_production 必须与设计 §3.3 ② 逐字一致（四要素：对象 + 批次 + 无供应商 + 非无源）",
    );
}

/// ⑦ V0376 的两条索引必须是局部索引，且不许给两列建唯一键。
#[test]
fn v0376_indexes_are_partial_and_not_unique() {
    let code = sql_only(&read_migration(V0376_UP));

    assert_has(
        &code,
        "CREATE INDEX IF NOT EXISTS idx_biz_si_cost_obj
            ON biz_stock_in (cost_object_id) WHERE cost_object_id IS NOT NULL;",
        "idx_biz_si_cost_obj 必须是局部索引（成本归集按对象扫完工入库）",
    );
    assert_has(
        &code,
        "CREATE INDEX IF NOT EXISTS idx_biz_si_cost_period
            ON biz_stock_in (cost_period_id) WHERE cost_period_id IS NOT NULL;",
        "idx_biz_si_cost_period 必须是局部索引（红冲按批次反查入库单，§4.7 ④）",
    );

    let squashed = squash(&code);
    assert!(
        !squashed.contains("CREATE UNIQUE INDEX"),
        "V0376 不许建唯一索引：一个成本对象可以有多次完工入库，一个批次有多张入库单（§4.7）"
    );
    assert_eq!(
        squashed.matches("CREATE INDEX").count(),
        2,
        "V0376 只应有 2 条索引（cost_obj / cost_period）"
    );
}

/// ⑧ 两张迁移只许动 biz_stock_out / biz_stock_in，别的一律不碰。
#[test]
fn v0375_v0376_touch_only_their_own_tables() {
    for (rel, table) in [(V0375_UP, "biz_stock_out"), (V0376_UP, "biz_stock_in")] {
        let code = sql_only(&read_migration(rel));
        let squashed = squash(&code);

        let alters = squashed.matches("ALTER TABLE ").count();
        let own_alters = squashed.matches(&format!("ALTER TABLE {table} ")).count();
        assert_eq!(
            alters,
            own_alters,
            "{rel} 有 {} 条 ALTER TABLE 不是打在 {table} 上：本卡只改自己那张表",
            alters - own_alters
        );

        let creates = squashed.matches("CREATE INDEX IF NOT EXISTS ").count();
        assert_eq!(
            squashed.matches(&format!(" ON {table} ")).count(),
            creates,
            "{rel} 有索引建在 {table} 以外的表上"
        );

        // 反向：别的存量表一个字都不许出现
        for forbidden in [
            "biz_stock_in_item",
            "biz_stock_out_item",
            "biz_inventory",
            "biz_cost_object ",
            "biz_cost_period ",
            "biz_account",
            "sys_menu",
        ] {
            assert!(
                !squashed.contains(&format!("ALTER TABLE {forbidden}")),
                "{rel} 对 {forbidden} 做了 ALTER：越界（各表归属见 §8.1 拆卡清单）"
            );
        }
    }
}

/// ⑨ 迁移里的 DML 只允许出现在自检探针（`biz_stock_in` 自己的 PROBE 行），且必须删净。
#[test]
fn v0376_dml_is_only_self_check_probes_and_is_cleaned_up() {
    let code = sql_only(&read_migration(V0376_UP));
    let targets = dml_targets(&code);

    assert!(
        !targets.is_empty(),
        "V0376 的自检探针行应该用 INSERT/DELETE 写 biz_stock_in；一个 DML 都没有说明行为断言被删光了"
    );
    for table in &targets {
        assert_eq!(
            table, "biz_stock_in",
            "V0376 写了 {table}：除自检探针（终态删净）之外，本卡不许对任何表做 DML"
        );
    }

    let squashed = squash(&code);
    assert!(
        squashed.contains("DELETE FROM biz_stock_in WHERE in_no LIKE 'V0376-PROBE-%'"),
        "探针行必须以 PROBE 前缀删净（迁移重复执行不许往真库攒垃圾行）"
    );
    assert!(
        squashed.contains("RAISE EXCEPTION 'V0376 自检失败：探针行未清理干净"),
        "必须断言探针零残留"
    );
}

/// ⑩ 自检写进迁移：形态块 + 行为块，每个失败路径都 `RAISE EXCEPTION`。
#[test]
fn v0375_v0376_self_checks_raise_exceptions() {
    let up5 = read_migration(V0375_UP);
    let up6 = read_migration(V0376_UP);

    assert_eq!(up5.matches("DO $$").count(), 1, "V0375 应有 1 个 DO 块（形态断言）");
    assert_eq!(up6.matches("DO $$").count(), 2, "V0376 应有 2 个 DO 块（形态 / 行为）");

    let key_messages = [
        // V0375
        "V0375 自检失败：biz_stock_out.cost_object_id 没建出来",
        "V0375 自检失败：cost_object_id 不可空",
        "V0375 自检失败：cost_object_id 带了默认值",
        "V0375 自检失败：idx_biz_so_cost_obj 不是局部索引",
        "V0375 自检失败：加列后有 % 张出库单的 cost_object_id 非空",
        "V0375 自检失败：biz_stock_out → biz_cost_object 建了 % 条物理外键",
        "V0375 形态断言通过：cost_object_id(UUID, 可空, 无默认)",
        // V0376 形态
        "V0376 自检失败：cost_object_id / cost_period_id 没建全",
        "V0376 自检失败：两列类型应为 uuid",
        "V0376 自检失败：新列带了默认值",
        "V0376 自检失败：ck_biz_si_type 没包含 PRODUCTION",
        "V0376 自检失败：ck_biz_si_type 丢掉了历史值域",
        "V0376 自检失败：ck_biz_si_production 没建出来",
        "V0376 自检失败：ck_biz_si_production 缺要素",
        "V0376 自检失败：ck_biz_si_supplier_scope 不见了",
        "V0376 自检失败：两条局部索引只找到",
        "V0376 自检失败：迁移后已有 % 张 PRODUCTION 单",
        "V0376 形态断言通过：两列(UUID 可空无默认)",
        // V0376 行为（★ 卡面验收 + 三个反向反证 + 存量路径不破）
        "V0376 自检失败：PRODUCTION 入库单写不进去",
        "V0376 自检失败：PRODUCTION 带供应商写入成功",
        "V0376 自检失败：PRODUCTION 且 is_sourceless=TRUE 写入成功",
        "V0376 自检失败：PRODUCTION 缺 cost_period_id 写入成功",
        "V0376 自检失败：采购入库（存量口径）写不进去了",
        "V0376 行为断言通过：PRODUCTION 可写",
        // 已知开放问题必须在部署日志里自我暴露（不能静默）
        "ck_biz_si_sourceless_pair",
    ];
    for msg in key_messages {
        assert!(
            up5.contains(msg) || up6.contains(msg),
            "自检断言里必须有一条覆盖「{msg}」（当前两个文件都找不到）"
        );
    }

    for (name, sql, blocks) in [("V0375", &up5, 1usize), ("V0376", &up6, 2usize)] {
        let parts: Vec<&str> = sql.split("DO $$").skip(1).collect();
        assert_eq!(parts.len(), blocks, "{name} 的 DO 块数量不对");
        for (i, block) in parts.iter().enumerate() {
            assert!(
                block.contains("RAISE EXCEPTION"),
                "{name} 第 {} 个 DO 块没有任何 RAISE EXCEPTION —— 自检失败也不会拦住迁移",
                i + 1
            );
        }
    }

    assert!(
        up6.contains("V0376 完成") || up6.contains("V0376 行为断言通过"),
        "行为自检块必须以完成提示收口"
    );
}

/// ⑪ ★ down 的数据护栏必须在「回滚值域 / 删列」**之前**。
///
/// 顺序反了会怎样：库里还有 PRODUCTION 单 → 旧 4 值 CHECK 加不回去 → 回滚在最后一步炸，
/// 库停在「列已删、约束还是新的」中间态，只能改库修（AGENTS 约定 11 的同一条精神）。
#[test]
fn v0376_down_guards_data_before_rolling_back_type_domain() {
    let down = read_migration(V0376_DOWN);
    let squashed = squash(&sql_only(&down));

    let pos_guard = squashed
        .find("FROM biz_stock_in WHERE in_type = 'PRODUCTION'")
        .expect("V0376 down 必须断言「没有 PRODUCTION 单」，否则回滚会把它们变成非法行");
    let pos_restore = squashed
        .find("ADD CONSTRAINT ck_biz_si_type")
        .expect("V0376 down 必须把 ck_biz_si_type 回滚到 4 值");
    let pos_drop_prod = squashed
        .find("DROP CONSTRAINT IF EXISTS ck_biz_si_production")
        .expect("V0376 down 必须先删 ck_biz_si_production（它引用两个新列）");
    let pos_drop_col = squashed
        .find("DROP COLUMN IF EXISTS cost_object_id")
        .expect("V0376 down 必须删 cost_object_id 列");

    assert!(
        pos_guard < pos_drop_prod,
        "数据护栏必须在删约束之前（护栏失败要尽早收口）"
    );
    assert!(
        pos_drop_prod < pos_restore,
        "必须先删 ck_biz_si_production 再回滚 ck_biz_si_type（否则旧约束与残留列语义打结）"
    );
    assert!(
        pos_restore < pos_drop_col,
        "必须先回滚值域再删列（顺序反了会停在中间态）"
    );

    assert!(
        squashed.contains("V0376 down 拒绝执行：还有 % 张完工入库单"),
        "down 的拒绝信息必须点名数量与人话（先作废/删除单据再回滚）"
    );
    assert!(
        squashed.contains("DROP INDEX IF EXISTS idx_biz_si_cost_obj")
            && squashed.contains("DROP INDEX IF EXISTS idx_biz_si_cost_period"),
        "down 必须用 DROP INDEX IF EXISTS 删两条索引（重复回滚不报错）"
    );
    assert!(
        squashed.contains("DROP COLUMN IF EXISTS cost_period_id"),
        "down 必须用 DROP COLUMN IF EXISTS 删 cost_period_id"
    );
    assert!(
        squashed.contains("CHECK (in_type IN ('PURCHASE', 'RETURN', 'OTHER', 'SOURCELESS'))"),
        "down 必须把值域回滚成 V0330 的 4 值原文"
    );

    // 反向：down 不许删行、不许动别的约束
    assert!(
        !squashed.contains("DELETE FROM biz_stock_in"),
        "down 不许删单据行（回滚只拆结构，业务数据丢不得）"
    );
    assert!(
        !squashed.contains("DROP CONSTRAINT IF EXISTS ck_biz_si_supplier_scope"),
        "down 不许顺手删 ck_biz_si_supplier_scope（本迁移没动它）"
    );
    assert!(
        squashed.contains("RAISE NOTICE 'V0376 down 完成"),
        "down 结尾必须有自检：拆完要断言约束/列/索引确实没了"
    );
}

/// ⑫ V0375 down：先护栏后删列，且是 `IF EXISTS` 全家桶。
#[test]
fn v0375_down_guards_data_before_dropping_column() {
    let down = read_migration(V0375_DOWN);
    let squashed = squash(&sql_only(&down));

    let pos_guard = squashed
        .find("SELECT count(*) FROM biz_stock_out WHERE cost_object_id IS NOT NULL")
        .expect("V0375 down 必须断言「没有单据挂着成本对象」，否则删列静默丢业务数据");
    let pos_drop = squashed
        .find("ALTER TABLE biz_stock_out DROP COLUMN IF EXISTS cost_object_id")
        .expect("V0375 down 必须用 DROP COLUMN IF EXISTS 删列");
    assert!(pos_guard < pos_drop, "数据护栏必须在 DROP COLUMN 之前");
    assert!(
        squashed.contains("V0375 down 拒绝执行：还有 % 张出库单挂着成本对象"),
        "down 的拒绝信息必须点名数量"
    );
    assert!(
        squashed.contains("DROP INDEX IF EXISTS idx_biz_so_cost_obj"),
        "down 必须用 DROP INDEX IF EXISTS 删索引"
    );
    assert!(
        !squashed.contains("DELETE FROM biz_stock_out"),
        "down 不许删单据行（回滚只拆结构）"
    );
    assert!(
        squashed.contains("RAISE NOTICE 'V0375 down 完成"),
        "down 结尾必须有反向自检"
    );

    // 幂等：down 第二次执行时列已不存在，护栏读列会直接抛 relation/column does not exist
    assert!(
        squashed.contains("information_schema.columns") && squashed.contains("cost_object_id 已不存在（重复回滚）"),
        "V0375 down 的护栏必须能识别「列已删」并跳过（否则 down 不幂等）"
    );
}

/// ⑬ 引擎拆分器必须把 `DO $$ … $$` 各留成一条语句（`$$` 内部的 `;` 不能被当分隔符）。
#[test]
fn v0375_v0376_split_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    for (rel, do_blocks, add_cols, idx) in [(V0375_UP, 1usize, 1usize, 1usize), (V0376_UP, 2usize, 2usize, 2usize)] {
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
                block.contains("RAISE EXCEPTION"),
                "{rel} 拆分后的 DO 块必须完整包含断言"
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

        let alters = statements
            .iter()
            .filter(|s| sql_head(s).starts_with("ALTER TABLE") && sql_head(s).contains("ADD COLUMN"))
            .count();
        assert_eq!(alters, add_cols, "{rel} 的 ADD COLUMN 语句数不对（实际 {alters}）");

        let indexes = statements
            .iter()
            .filter(|s| sql_head(s).starts_with("CREATE") && sql_head(s).contains("INDEX"))
            .count();
        assert_eq!(indexes, idx, "{rel} 的 CREATE INDEX 语句数不对（实际 {indexes}）");
    }
}
