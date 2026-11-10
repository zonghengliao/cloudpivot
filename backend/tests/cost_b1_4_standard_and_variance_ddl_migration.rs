//! 迁移 `V0374`（二期三表：`biz_cost_standard` / `biz_cost_standard_item` /
//! `biz_cost_variance`）的静态不变量。
//!
//! 卡：t_b8b96fac（COST-B1-4【成本】迁移 V0372）—— 设计依据
//! `docs/architecture/ARCH-9-成本核算升级设计.md` §3.2 ⑪⑫⑬、§4.9 差异分析、
//! 拆卡清单 §8.1 的 B1-4「二期三表（先建表，代码后写）」。
//!
//! # 为什么号段是 V0374 而不是卡面的 V0372
//!
//! V0372（分摊两表）与 V0373（期间两表）已被前置卡 B1-3 用掉，
//! nik 迁移加载器对版本号做跨目录全局唯一校验，重号会让服务启动直接失败。
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs::migration_versions_are_unique_padded_and_have_down`
//! 统一覆盖，这里不重复。
//!
//! # 这层测试钉死什么
//!
//! 迁移是**一次性、不可回改**的东西（`schema_migrations` 记内容指纹，改一个字节
//! 已升级的机器就起不来），所以它的错误只有下次部署时才炸。这里把「写对了」的
//! 形状在 `cargo test` 阶段固化，覆盖的都是会**静默算错差异**的类型：
//!
//! 1. ★ **`ck_biz_cost_std_sum` 被删**（标准总成本 ≠ 料+工+费）⇒ 差异报表的分母
//!    就是错的，每个差异都被同步放大，而报表不会报错。
//! 2. ★ **`ck_biz_cost_std_labor` 被删**（人工成本 ≠ round(工时 × 工资率, 6)）⇒
//!    §4.9.2 的「率差 + 效差 = 实际 − 标准」恒等式破掉。
//! 3. ★ **`ck_biz_cost_std_item_cal` 被删**（明细金额 ≠ round(用量 × 单价, 6)）⇒
//!    材料量差 = (实际用量 − 标准用量) × 标准单价 直接算错。
//! 4. ★ **`ck_biz_cost_var_sign` 被删**（direction 与 amount 符号脱钩）⇒ 出现
//!    「标了不利、金额是负」的自相矛盾行，汇总时正负抵消。
//! 5. **`uk_biz_cost_std` 漏软删条件** ⇒ 卡录错删掉后无法重建同物料同生效日的卡。
//! 6. **`standard_item → standard` 写成 RESTRICT / `variance → standard` 写成 CASCADE** ⇒
//!    前者删卡头留下无主标准用量、后者删卡头静默带走历史差异依据。
//! 7. **给物料列加物理外键** ⇒ 违反设计 §3.4 弱引用约定，主数据软删锁死历史标准。
//! 8. **越界建表 / 改存量表**（一期 10 表、二期之后的加列卡）⇒ 与别的卡撞车。
//! 9. **自检断言被删**（尤其 §4.9.2 两因素恒等式那条）⇒ 迁移「跑过了但没验过」。
//! 10. **语句被引擎拆分器切坏**（`DO $$ … $$` 里含分号）⇒ PG 语法错误、整个迁移回滚。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up×3 / down×2 / 回滚重放 / 下游护栏 / 两因素恒等式活体验证）的结果见
//! `docs/evidence/2026-11-10-cost-b1-4-v0374/02_up_down_drill.out`。

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

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材（比如「唯一键最初不带软删条件」），
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

const V0374_UP: &str = "business/V0374__cost_standard_tables.up.sql";
const V0374_DOWN: &str = "business/V0374__cost_standard_tables.down.sql";

/// 本卡的范围：二期三表（§3.2 ⑪⑫⑬）。
const STD_TABLES: [&str; 3] = ["biz_cost_standard", "biz_cost_standard_item", "biz_cost_variance"];

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
fn v0374_up_and_down_exist() {
    for rel in [V0374_UP, V0374_DOWN] {
        let path = backend_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ② 只建自己范围的三张表，且建表语句必须幂等（`IF NOT EXISTS`）。
///
/// 多建一张就是越界（一期 10 表属 B1-1/2/3、出库入库加列属 B1-5、科目/菜单属 B1-6/7）；
/// 少一个 `IF NOT EXISTS`，迁移重复执行（回滚后重放 / 演习库补跑）就会
/// `relation already exists` 中断。
#[test]
fn v0374_creates_exactly_three_tables_idempotently() {
    let sql = sql_only(&read_migration(V0374_UP));

    for table in STD_TABLES {
        assert_has(
            &sql,
            &format!("CREATE TABLE IF NOT EXISTS {table} ("),
            &format!("{table} 必须以 CREATE TABLE IF NOT EXISTS 建（幂等）"),
        );
    }
    assert_eq!(
        squash(&sql).matches("CREATE TABLE").count(),
        3,
        "V0374 只许建二期三表（standard / standard_item / variance）"
    );

    // 一期 10 表 + 后续卡的改造对象，一张都不许在这里出现
    for forbidden in [
        "biz_lot",
        "biz_cost_object",
        "biz_cost_collect",
        "biz_cost_labor",
        "biz_cost_overhead",
        "biz_cost_rate",
        "biz_cost_alloc_rule",
        "biz_cost_alloc_result",
        "biz_cost_period",
        "biz_cost_period_item",
        // 未来卡的（差异分摊、成本科目、菜单）
        "biz_cost_variance_alloc",
        "biz_account",
        "sys_menu",
    ] {
        assert!(
            !squash(&sql).contains(&format!("CREATE TABLE IF NOT EXISTS {forbidden}")),
            "V0374 不许建 {forbidden}：一期表属 B1-1/2/3，科目/菜单属 B1-6/7，差异分摊是未来 ADR"
        );
    }
}

/// ③ 不许碰任何存量表结构（本卡是「只加新表」）。
#[test]
fn v0374_never_alters_existing_tables() {
    let code = squash(&sql_only(&read_migration(V0374_UP)));
    for forbidden in ["ALTER TABLE", "TRUNCATE", "DROP TABLE", "DROP INDEX", "DROP CONSTRAINT"] {
        assert!(
            !code.contains(forbidden),
            "V0374 只允许「建新表 + 建索引 + 注释」，不允许出现 {forbidden}：存量表改造属别的卡"
        );
    }
}

/// ④ up 里的 DML 只允许出现在「自己三张表 + biz_cost_object 探针父行」上。
#[test]
fn v0374_dml_only_touches_own_tables_and_probe_parent() {
    let code = sql_only(&read_migration(V0374_UP));
    let targets = dml_targets(&code);
    assert!(
        !targets.is_empty(),
        "自检里的探针行应该用 INSERT/DELETE 写自己的表；一个 DML 都没有说明自检被删光了"
    );
    for table in targets {
        assert!(
            STD_TABLES.contains(&table.as_str()) || table == "biz_cost_object",
            "V0374 写了 {table} —— 除 biz_cost_object（RESTRICT 反证用的探针父行，断言后删除）\
             之外，本卡不许对任何其它表做 DML"
        );
    }
}

/// ⑤ 外键必须精确：
///    `biz_cost_standard`      0 条（物料/人员全弱引用，设计 §3.4）
///    `biz_cost_standard_item` 1 条，`→ standard` 是 **CASCADE**（明细随卡头走）
///    `biz_cost_variance`      2 条，`→ cost_object` / `→ standard` 都是 **RESTRICT**
#[test]
fn v0374_foreign_keys_have_exact_delete_actions() {
    let sql = sql_only(&read_migration(V0374_UP));

    assert_has(
        &sql,
        "standard_id UUID NOT NULL REFERENCES biz_cost_standard(id) ON DELETE CASCADE",
        "卡明细 → 卡头必须 ON DELETE CASCADE（删卡头会留下无主标准用量）",
    );
    assert_has(
        &sql,
        "cost_object_id UUID REFERENCES biz_cost_object(id) ON DELETE RESTRICT",
        "差异 → 成本对象必须 ON DELETE RESTRICT（差异是已出的报表结论）",
    );
    // 差异 → 卡头：同一句里的第二条 REFERENCES
    let squashed = squash(&sql);
    assert!(
        squashed.contains("standard_id UUID REFERENCES biz_cost_standard(id) ON DELETE RESTRICT"),
        "差异 → 卡头必须 ON DELETE RESTRICT（删卡头会让历史差异失去依据）"
    );
    assert_eq!(
        squashed.matches("REFERENCES").count(),
        3,
        "V0374 只允许 3 条物理外键（1 条明细→卡头、2 条差异→(对象,卡头)）：\
         material_id / component_material_id / create_by 必须弱引用（设计 §3.4）"
    );

    // 反向：不许 REFERENCES 到这三张表以外的东西（除 biz_cost_standard / biz_cost_object）
    let mut rest: &str = &squashed;
    while let Some(pos) = rest.find("REFERENCES") {
        let after = &rest[pos + "REFERENCES".len()..];
        let target: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        assert!(
            ["biz_cost_standard", "biz_cost_object"].contains(&target.as_str()),
            "发现一条指向 {target} 的外键：成本表对主数据 / 单据必须弱引用（设计 §3.4）"
        );
        rest = after;
    }
}

/// ⑥ ★ `uk_biz_cost_std` 必须是「(material_id, effective_from) + 软删」。
///
/// 少了软删条件 ⇒ 录错的标准卡删掉后无法重建同物料同生效日的卡（只能改库）；
/// 加了 version_no 进键 ⇒ 同一生效日可以并存两版标准，「哪版有效」变二义。
#[test]
fn v0374_standard_unique_key_is_versioned_by_effect_date() {
    let sql = read_migration(V0374_UP);

    assert_has(
        &sql,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_std
            ON biz_cost_standard (material_id, effective_from) WHERE deleted_at IS NULL",
        "uk_biz_cost_std 必须是「UNIQUE + IF NOT EXISTS + (material_id, effective_from) + 软删」",
    );

    let code = squash(&sql_only(&sql));
    assert!(
        !code.contains("ON biz_cost_standard (material_id, effective_from, version_no)"),
        "uk_biz_cost_std 把 version_no 加进了唯一键 —— 同一生效日可并存多版标准（二义）"
    );
    assert!(
        !code.contains("material_id, effective_from);"),
        "uk_biz_cost_std 少了 WHERE 谓词（软删的旧卡会占位，录错卡删了也建不回来）"
    );
}

/// ⑦ 其余索引：卡明细 (standard_id, line_no) 唯一；差异两条报表索引；差异**不许**有唯一键。
#[test]
fn v0374_other_indexes_are_correctly_shaped() {
    let sql = read_migration(V0374_UP);

    assert_has(
        &sql,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_std_item ON biz_cost_standard_item (standard_id, line_no)",
        "uk_biz_cost_std_item 必须是 (standard_id, line_no) 唯一（同卡行号不重复）",
    );
    assert_has(
        &sql,
        "CREATE INDEX IF NOT EXISTS idx_biz_cost_var_period ON biz_cost_variance (period_code, variance_type)",
        "差异报表按「期间 × 类型」钻取（§4.9.4），索引必须在",
    );
    assert_has(
        &sql,
        "CREATE INDEX IF NOT EXISTS idx_biz_cost_var_obj ON biz_cost_variance (cost_object_id, period_code)",
        "差异按「成本对象 × 期间」钻取，索引必须在",
    );

    // 差异是「按期间整删重灌」的计算产物，设计 §3.2 未给唯一键 —— 不许擅自加
    let code = squash(&sql_only(&sql));
    assert!(
        !code.contains("CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_var"),
        "biz_cost_variance 不许加唯一键：差异是按期间整删重灌的产物，唯一键会把重算变成冲突"
    );
    assert_eq!(
        code.matches("CREATE UNIQUE INDEX").count(),
        2,
        "V0374 只应有 2 条唯一索引（uk_biz_cost_std / uk_biz_cost_std_item）"
    );
}

/// ⑧ CHECK 约束一条都不能少（漏一条 = 一条脏数据入口）。
#[test]
fn v0374_keeps_all_check_constraints() {
    let sql = read_migration(V0374_UP);

    let expected: [(&str, &[&str]); 3] = [
        (
            "biz_cost_standard",
            &[
                "ck_biz_cost_std_money",
                "ck_biz_cost_std_sum",
                "ck_biz_cost_std_dates",
                "ck_biz_cost_std_ver",
                "ck_biz_cost_std_labor",
            ],
        ),
        (
            "biz_cost_standard_item",
            &["ck_biz_cost_std_item_num", "ck_biz_cost_std_item_cal"],
        ),
        (
            "biz_cost_variance",
            &[
                "ck_biz_cost_var_type",
                "ck_biz_cost_var_elem",
                "ck_biz_cost_var_dir",
                "ck_biz_cost_var_period",
                "ck_biz_cost_var_sign",
            ],
        ),
    ];

    for (table, names) in expected {
        for name in names {
            assert_has(
                &sql,
                &format!("CONSTRAINT {name} CHECK"),
                &format!("{table} 缺约束 {name}（漏一条 = 一条脏数据入口）"),
            );
        }
    }

    // ★ 四条会「静默算错差异」的约束逐字校验
    assert_has(
        &sql,
        "CONSTRAINT ck_biz_cost_std_sum   CHECK (std_total_cost = std_material_cost + std_labor_cost + std_overhead_cost)",
        "标准总成本必须逐项写全 = 料 + 工 + 费（差异报表的分母）",
    );
    assert_has(
        &sql,
        "CONSTRAINT ck_biz_cost_std_labor CHECK (std_labor_cost = round(std_labor_hours * std_labor_rate, 6))",
        "标准人工成本必须 = round(标准工时 × 标准工资率, 6)（否则 §4.9.2 率差+效差 恒等式破掉）",
    );
    assert_has(
        &sql,
        "CONSTRAINT ck_biz_cost_std_item_cal CHECK (std_amount = round(std_qty * std_price, 6))",
        "明细金额必须 = round(标准用量 × 标准价, 6)（材料量差的计算依据）",
    );
    assert_has(
        &sql,
        "CONSTRAINT ck_biz_cost_var_sign  CHECK (
            (direction = 'UNFAVORABLE' AND amount >= 0) OR (direction = 'FAVORABLE' AND amount <= 0))",
        "差异 direction 与 amount 符号必须锁死（正=不利、负=有利，§4.9.4 报表口径）",
    );

    // 六类差异 / 三要素 / 期间格式：值域必须与 §4.9.1 一致
    assert_has(
        &sql,
        "('MAT_PRICE','MAT_QTY','LABOR_RATE','LABOR_EFF','OH_SPEND','OH_VOLUME')",
        "六类差异必须是 §4.9.1 的那六种（MAT_PRICE/MAT_QTY/LABOR_RATE/LABOR_EFF/OH_SPEND/OH_VOLUME）",
    );
    assert_has(
        &sql,
        "CONSTRAINT ck_biz_cost_var_elem  CHECK (element IN ('MATERIAL','LABOR','OVERHEAD'))",
        "差异要素必须只有 MATERIAL/LABOR/OVERHEAD 三要素（与 period_item 口径一致）",
    );
    let var_period_checks = squash(&sql_only(&sql))
        .matches("period_code ~ '^\\d{4}-(0[1-9]|1[0-2])$'")
        .count();
    assert_eq!(
        var_period_checks, 1,
        "biz_cost_variance.period_code 必须有 yyyy-MM 正则 CHECK"
    );
}

/// ⑨ 关键列的类型 / 精度 / 默认值必须与设计 §3.2 一致。
#[test]
fn v0374_column_types_follow_design() {
    let sql = read_migration(V0374_UP);

    for snippet in [
        // ⑪ 标准成本卡（卡头）
        "material_id UUID NOT NULL",
        "version_no INT NOT NULL DEFAULT 1",
        "effective_from DATE NOT NULL",
        "effective_to DATE",
        "std_material_cost NUMERIC(18,6) NOT NULL DEFAULT 0",
        "std_labor_cost NUMERIC(18,6) NOT NULL DEFAULT 0",
        "std_overhead_cost NUMERIC(18,6) NOT NULL DEFAULT 0",
        "std_total_cost NUMERIC(18,6) NOT NULL DEFAULT 0",
        "std_labor_hours NUMERIC(18,4) NOT NULL DEFAULT 0",
        "std_labor_rate NUMERIC(18,6) NOT NULL DEFAULT 0",
        "status SMALLINT NOT NULL DEFAULT 1",
        // ⑫ 卡明细
        "line_no INT NOT NULL",
        "component_material_id UUID NOT NULL",
        "std_qty NUMERIC(18,6) NOT NULL",
        "std_price NUMERIC(18,6) NOT NULL",
        "std_amount NUMERIC(18,6) NOT NULL",
        // ⑬ 差异明细
        "period_code VARCHAR(7) NOT NULL",
        "variance_type VARCHAR(12) NOT NULL",
        "element VARCHAR(12) NOT NULL",
        "actual_qty NUMERIC(18,6)",
        "actual_price NUMERIC(18,6)",
        "amount NUMERIC(18,4) NOT NULL",
        "direction VARCHAR(12) NOT NULL",
    ] {
        assert_has(&sql, snippet, &format!("二期三表列定义与设计 §3.2 不符：{snippet}"));
    }

    // 反向：标准价 / 费率 / 差异金额都不许被改窄（标准成本按 6 位存、差异按 4 位）
    let code = squash(&sql_only(&sql));
    for bad in ["NUMERIC(18,2)", "NUMERIC(20,2)", "NUMERIC(20,6)"] {
        assert!(
            !code.contains(bad),
            "V0374 出现了 {bad}：标准成本/单价必须是设计 §3.2 的 NUMERIC(18,6)、差异金额 18,4"
        );
    }

    // 三张表都必须有 COMMENT ON TABLE（本期要求：新表要能自解释）
    for table in STD_TABLES {
        assert_has(
            &sql,
            &format!("COMMENT ON TABLE  {table} IS"),
            &format!("{table} 缺 COMMENT ON TABLE（表要能自解释，别只写列名）"),
        );
    }
}

/// ⑩ 自检写进迁移：3 个 `DO $$` 块（前置护栏 + 形态 + 行为），
///    每个失败路径都 `RAISE EXCEPTION`，且 ★两条核心语义（符号约定 / §4.9.2 恒等式）必须有断言。
#[test]
fn v0374_self_check_raises_exceptions() {
    let sql = read_migration(V0374_UP);

    let key_messages = [
        "biz_cost_object 不存在，请先执行 V0370",
        "uk_biz_cost_std 缺软删条件",
        "uk_biz_cost_std 的列组合不是 (material_id, effective_from)",
        "uk_biz_cost_std_item 的列组合不是 (standard_id, line_no)",
        "biz_cost_standard 的 5 条 CHECK 约束只有",
        "biz_cost_standard_item 的 2 条 CHECK 约束只有",
        "biz_cost_variance 的 5 条 CHECK 约束只有",
        "standard_item.standard_id → biz_cost_standard 必须是 ON DELETE CASCADE",
        "variance.cost_object_id → biz_cost_object 必须是 ON DELETE RESTRICT",
        "variance.standard_id → biz_cost_standard 必须是 ON DELETE RESTRICT",
        // ★ 行为断言：唯一键 / 四条自洽 CHECK / 符号约定 / 恒等式 / 外键
        "同物料同生效日第二张活卡写入成功",
        "软删后同物料同生效日重建失败",
        "标准总成本 ≠ 料+工+费 写入成功",
        "标准人工成本 ≠ round(工时 × 工资率,6) 写入成功",
        "明细金额 ≠ round(用量 × 单价,6) 写入成功",
        "标准用量=0 写入成功",
        "direction=UNFAVORABLE 且 amount<0 写入成功",
        "direction=FAVORABLE 且 amount>0 写入成功",
        "variance_type=MAT_MIX 写入成功",
        "element=ASSET 写入成功",
        "period_code=2026-13 写入成功",
        // ★ §4.9.2 两因素恒等式的活体验证（算例 C：600 + 3840 = 15000 − 10560）
        "两因素之和 % ≠ 实际 − 标准 4440",
        "价差 + 量差 % ≠ 实际材料成本 15000 − 标准材料成本 10560",
        "有差异记录时成本对象被删掉",
        "有差异记录时标准卡被删掉",
        "删卡头后明细残留",
        "探针行未清理干净",
    ];

    let blocks: Vec<&str> = sql.split("DO $$").skip(1).collect();
    assert_eq!(
        blocks.len(),
        3,
        "V0374 应有 3 个 DO 块（前置护栏 / 形态断言 / 行为断言），实际 {} 个",
        blocks.len()
    );
    for (i, block) in blocks.iter().enumerate() {
        assert!(
            block.contains("RAISE EXCEPTION"),
            "V0374 第 {} 个 DO 块没有任何 RAISE EXCEPTION —— 自检失败也不会拦住迁移",
            i + 1
        );
    }
    for msg in key_messages {
        assert!(sql.contains(msg), "自检断言里必须有一条覆盖「{msg}」（当前找不到）");
    }

    // 自检日志里要能看出是哪一号迁移干的
    assert!(
        sql.contains("RAISE NOTICE 'V0374 形态断言通过"),
        "形态自检块必须以完成提示收口"
    );
    assert!(sql.contains("V0374 完成"), "行为自检块必须以完成提示收口");

    // 探针行必须删干净：迁移重复执行不能往真库里攒垃圾行
    let code = squash(&sql_only(&sql));
    for table in STD_TABLES {
        assert!(
            code.contains(&format!("DELETE FROM {table}")),
            "{table} 的自检探针行必须在迁移里被 DELETE 掉（重复执行零残留）"
        );
    }
}

/// ⑪ down 必须幂等 + 有下游依赖护栏 + 结尾断言零残留。
#[test]
fn v0374_down_is_idempotent_and_guarded() {
    let down = read_migration(V0374_DOWN);

    for table in STD_TABLES {
        assert_has(
            &down,
            &format!("DROP TABLE IF EXISTS {table};"),
            &format!("down 必须用 DROP TABLE IF EXISTS 删 {table}（重复执行不报错）"),
        );
    }

    // 先删差异（它 RESTRICT 引用卡头），再删明细（CASCADE 从属），最后删卡头
    let squashed = squash(&down);
    let pos_var = squashed.find("DROP TABLE IF EXISTS biz_cost_variance;").unwrap();
    let pos_item = squashed.find("DROP TABLE IF EXISTS biz_cost_standard_item;").unwrap();
    let pos_std = squashed.find("DROP TABLE IF EXISTS biz_cost_standard;").unwrap();
    assert!(
        pos_var < pos_item && pos_item < pos_std,
        "down 的 DROP 顺序必须是「差异 → 明细 → 卡头」（先拆有 RESTRICT 外键的子表，再拆被引用的父表）"
    );

    assert_has(
        &down,
        "FROM pg_constraint f",
        "down 必须用 pg_constraint 扫「有没有外部表 FK 到我的表」，而不是硬编码表名",
    );
    assert_has(
        &down,
        "V0374 down 拒绝执行：下游表仍在引用",
        "down 的拒绝信息必须说清「先回滚哪张卡」",
    );

    // down 自身幂等：不许出现 `'表名'::regclass` 这种「表已删就抛错」的写法
    let code = sql_only(&down);
    for table in STD_TABLES {
        assert!(
            !code.contains(&format!("'{table}'::regclass")),
            "down 里出现了 '{table}'::regclass：第二次执行 down 时表已不存在，\
             这行会直接抛 `relation does not exist`，down 就不幂等了"
        );
    }
    assert!(
        squash(&code).contains("RAISE NOTICE 'V0374 down 完成"),
        "down 结尾必须有自检：删完要断言表和索引确实没了"
    );
}

/// ⑫ 引擎拆分器必须把 3 个 `DO $$ … $$` 各留成一条语句（`$$` 内部的 `;` 不能被当分隔符）。
#[test]
fn v0374_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    let sql = read_migration(V0374_UP);
    let statements = split_statements(&sql, DialectKind::Postgres)
        .unwrap_or_else(|e| panic!("引擎拆分器无法拆分 {V0374_UP}（方言感知的 $$ 处理失败）: {e}"));

    let do_blocks: Vec<&String> = statements.iter().filter(|s| sql_head(s).starts_with("DO")).collect();
    assert_eq!(
        do_blocks.len(),
        3,
        "三个 DO 块必须各被拆成**一条**语句（实际 {} 条）：>3 说明 $$ 内部的 `;` 被当成分隔符了",
        do_blocks.len()
    );
    for block in &do_blocks {
        assert!(block.contains("RAISE EXCEPTION"), "拆分后的 DO 块必须完整包含断言");
        assert!(
            block.trim_end().ends_with("END $$;") || block.trim_end().ends_with("END $$"),
            "拆分后的 DO 块必须以 END $$ 收口（被截断就会语法错误）"
        );
    }

    let create_table_count = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE TABLE"))
        .count();
    assert_eq!(
        create_table_count, 3,
        "三条 CREATE TABLE 应各成一条语句，实际 {create_table_count} 条"
    );

    let create_index_count = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE") && sql_head(s).contains("INDEX"))
        .count();
    assert_eq!(
        create_index_count, 4,
        "应有 4 条 CREATE INDEX（uk_std / uk_std_item / idx_var_period / idx_var_obj），\
         实际 {create_index_count} 条"
    );
}
