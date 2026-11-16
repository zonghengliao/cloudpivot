//! 迁移 `V0420`（M2-01：在途库存账表结构 `biz_inventory_in_transit`）的静态不变量。
//!
//! 卡：t_7ba5b0e2（M2-01【M2·仓库】在途库存：表结构 biz_inventory_in_transit）。
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §5 M2 仓库 的 M2-01 行；
//! 下游 M2-02（发货过账 → 在途 +）、M2-03（签收 → 在途转已出库）、M2-04（查询）、
//! M2-05（对账：在途 + 已出 = 发货总量）都按本表的契约写 SQL。
//!
//! # 这层测试钉死什么
//!
//! 迁移是**一次性、不可回改**的东西（`schema_migrations` 记内容指纹，改一个字节
//! 已升级的机器就起不来），写错了只有下次部署才炸。这里把「写对了的形状」固化到
//! `cargo test`，覆盖的都是会**静默算错在途量**的类型：
//!
//! 1. **在途量退化成普通列** ⇒ 「在途 = 发货 − 已签收」要靠每条写路径自己维护，
//!    改一处漏一处，两个读入口就会对「还剩多少在途」给出两个答案，而且不报错。
//!    必须是 `GENERATED ALWAYS AS (shipped_qty - received_qty) STORED`。
//! 2. **数量列精度被写窄/写宽**（`NUMERIC(18,2)` / `(18,6)`）⇒ 与 `biz_inventory.qty`
//!    （`NUMERIC(18,4)`）对不上，对账时出尾差，不报错只差几分。
//! 3. **`shipped_qty` 允许 ≤ 0 / `received_qty` 允许 > 发货量** ⇒ 在途量变负数，
//!    补货建议（可用量 = 库存 + 在途）当场失真。
//! 4. **状态与数量脱钩**（签满了还挂 `IN_TRANSIT`）⇒ M2-05 对账把已签收的量再算一遍在途。
//! 5. **唯一键退化成普通索引或加 `WHERE` 谓词** ⇒ 同一批发货能插两行，在途翻倍。
//! 6. **给 `material_id` / `warehouse_id` 加物理外键** ⇒ 物料/仓库软删后历史在途账失去
//!    可读性（AGENTS.md 弱引用口径）；而 `shipment_id` 必须是 `ON DELETE CASCADE`。
//! 7. **两条「不做」的决定反向钉死**：不加 `deleted_at`（在途行没有「删一行」的业务动作，
//!    留墓碑会造出第二种隐藏状态）、不加库位维度 `location_id`（卡面口径是「物料+仓库+发货单」，
//!    库位维度的在途没有业务需求，加了就要跟着维护一条没人读的维度）。
//! 8. **本卡越界**：建了别的表 / 动了 `biz_inventory`、`biz_inventory_txn`、`biz_shipment`
//!    的存量结构（三本账唯一写入口与发货单都在别的卡）。
//! 9. **自检断言被删** ⇒ 迁移「跑过了但没验过」，等于没有自检。
//! 10. **语句被引擎拆分器切坏**（`DO $$ … $$` 里含分号）⇒ PG 报语法错误、整个迁移事务
//!     回滚、服务起不来。这条用引擎自己的 `split_statements` 验。
//!
//! 全局不变量（版本号唯一 / 4 位补零 / up 必有 down）由
//! `tests/flow_all_role_multiversion_migration.rs` 的
//! `migration_versions_are_unique_padded_and_have_down` 统一覆盖，这里不重复。
//!
//! 纯文件解析 + 框架拆分器，**不连库、不需要任何环境变量** ⇒ 每次 `cargo test` 都会跑到。
//! 真库往返（up×2 / down×2 / 护栏反证 / upsert / 对账恒等等）的实测见
//! `bash scripts/verify/m201_inventory_in_transit_drill.sh`。

use std::path::PathBuf;

/// `backend/app` 目录（集成测试 crate 的 manifest 目录就是迁移根所在层）。
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

/// 去掉 `--` 行注释。注释里会**引用**错误写法做反面教材（比如「在途量不能当普通列」），
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

const V0420_UP: &str = "business/V0420__inventory_in_transit.up.sql";
const V0420_DOWN: &str = "business/V0420__inventory_in_transit.down.sql";

/// 本卡只允许建这一张表（M2-02..M2-05 各有自己的卡）。
const OWN_TABLE: &str = "biz_inventory_in_transit";

/// 自检探针允许**搭夹具**的两张存量表：`shipment_id` 是硬外键，没有一张真发货单
/// 就验不了「级联 + 唯一键 + 生成列」。两张表都只插 `__M201_PROBE_*` 行并块尾删除
/// （见 `v0420_probe_fixtures_are_cleaned_up`）。
const PROBE_FIXTURE_TABLES: [&str; 2] = ["biz_sales_order", "biz_shipment"];

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
fn v0420_up_and_down_exist() {
    for rel in [V0420_UP, V0420_DOWN] {
        let path = app_root().join("migrations").join(rel);
        assert!(path.is_file(), "缺少迁移文件 {}", path.display());
    }
}

/// ② 只建这一张表，且建表语句必须幂等；DML 只许打在自己的表与两张探针夹具上。
#[test]
fn v0420_creates_exactly_one_table_idempotently() {
    let sql = sql_only(&read_migration(V0420_UP));
    let squashed = squash(&sql);

    assert_has(
        &sql,
        "CREATE TABLE IF NOT EXISTS biz_inventory_in_transit (",
        "建表语句必须是 `CREATE TABLE IF NOT EXISTS biz_inventory_in_transit`（幂等 + 表名固定）",
    );

    let create_table_count = squashed.matches("CREATE TABLE").count();
    assert_eq!(
        create_table_count, 1,
        "本卡只许建 1 张表（{OWN_TABLE}），实际 {create_table_count} 处 CREATE TABLE —— 多出来的越界了"
    );

    // 不许改 / 删存量表：本卡是「纯新建」，三本账与发货单的结构都归别的卡
    for forbidden in ["ALTER TABLE", "DROP TABLE", "TRUNCATE"] {
        assert!(
            !squashed.contains(forbidden),
            "V0420 出现 {forbidden}：本卡只新建一张表，改/删存量表属别的卡（三本账唯一写入口见 inventory_write_gate.py）"
        );
    }

    // DML 目标：自己的表 + 两张探针夹具，别的表一律越界
    for table in dml_targets(&sql) {
        let allowed = table == OWN_TABLE || PROBE_FIXTURE_TABLES.contains(&table.as_str());
        assert!(
            allowed,
            "V0420 的 DML 打到了 {table} —— 本卡只许写 {OWN_TABLE} 与其探针夹具 {PROBE_FIXTURE_TABLES:?}（别去动库存三本账）"
        );
    }
}

/// ③ 15 列逐列形状：长度 / 精度 / 可空性 / 默认值。
#[test]
fn v0420_columns_have_designed_shape() {
    let sql = read_migration(V0420_UP);

    for (snippet, why) in [
        (
            "shipment_id UUID NOT NULL",
            "shipment_id 必须 UUID NOT NULL（在途行挂在发货单上，没有单头就没有这一行）",
        ),
        (
            "material_id UUID NOT NULL",
            "material_id 必须 UUID NOT NULL（卡面口径：按物料记账）",
        ),
        (
            "warehouse_id UUID NOT NULL",
            "warehouse_id 必须 UUID NOT NULL（卡面口径：按仓库记账，值是发货仓）",
        ),
        (
            "ship_date DATE NOT NULL",
            "ship_date 必须 DATE NOT NULL（在途账龄口径：过账时快照发货日期）",
        ),
        (
            "shipped_qty NUMERIC(18,4) NOT NULL",
            "shipped_qty 必须 NUMERIC(18,4) NOT NULL（与 biz_inventory.qty 同口径）",
        ),
        (
            "received_qty NUMERIC(18,4) NOT NULL DEFAULT 0",
            "received_qty 必须 NUMERIC(18,4) NOT NULL DEFAULT 0（刚过账时一件都没签收）",
        ),
        (
            "status VARCHAR(24) NOT NULL DEFAULT 'IN_TRANSIT'",
            "status 必须 VARCHAR(24) NOT NULL DEFAULT 'IN_TRANSIT'（业务状态列用 VARCHAR 枚举，与发货单同口径）",
        ),
        (
            "received_at TIMESTAMPTZ",
            "received_at 必须 TIMESTAMPTZ 且可空（只有全部签收后才有值）",
        ),
        (
            "remark VARCHAR(255)",
            "remark 必须 VARCHAR(255) 且可空（作废原因写这里）",
        ),
    ] {
        assert_has(&sql, snippet, why);
    }

    // 反向：数量精度不许写窄到 2 位、也不许放宽成 6 位（会与库存/对账口径对不上）
    let code = squash(&sql_only(&sql));
    for bad in [
        "shipped_qty NUMERIC(18,2)",
        "shipped_qty NUMERIC(20,2)",
        "shipped_qty NUMERIC(18,6)",
        "shipped_qty NUMERIC(12,2)",
        "received_qty NUMERIC(18,2)",
        "received_qty NUMERIC(18,6)",
        "qty_in_transit NUMERIC(18,2)",
    ] {
        assert!(
            !code.contains(bad),
            "V0420 出现了 {bad}：三个数量列必须 NUMERIC(18,4)（与 biz_inventory.qty 同口径）"
        );
    }

    // 列数：15 列是契约（迁移自检里也数了这个数，两处必须一致）
    let declared_columns = [
        "id UUID",
        "shipment_id UUID",
        "material_id UUID",
        "warehouse_id UUID",
        "ship_date DATE",
        "shipped_qty NUMERIC(18,4)",
        "received_qty NUMERIC(18,4)",
        "qty_in_transit NUMERIC(18,4)",
        "status VARCHAR(24)",
        "received_at TIMESTAMPTZ",
        "remark VARCHAR(255)",
        "created_at TIMESTAMPTZ",
        "updated_at TIMESTAMPTZ",
        "create_by UUID",
        "update_by UUID",
    ];
    for col in declared_columns {
        assert_has(&sql, col, &format!("缺少设计列 {col}"));
    }
}

/// ④ 在途量必须是**生成列**（`GENERATED ALWAYS AS (shipped_qty - received_qty) STORED`）。
///
/// 这是本表最重要的一条不变量：在途量若靠写路径维护，M2-03 的签收（`received_qty += n`）
/// 与 M2-04 的在途汇总（读 `qty_in_transit`）就会各算一遍，改一处漏一处即静默错账。
#[test]
fn v0420_qty_in_transit_is_generated_column() {
    let sql = read_migration(V0420_UP);
    assert_has(
        &sql,
        "qty_in_transit NUMERIC(18,4) GENERATED ALWAYS AS (shipped_qty - received_qty) STORED",
        "在途量必须是生成列 `GENERATED ALWAYS AS (shipped_qty - received_qty) STORED`：\
         靠 Rust 维护会漂移（全仓先例：V0211 的 biz_stock_take_item.diff_qty）",
    );

    // 自检块必须验到这件事（否则「跑过了但没验过」）
    assert!(
        sql.contains("qty_in_transit 不是生成列"),
        "迁移自检必须断言 qty_in_transit 是生成列（当前找不到这条断言）"
    );
    assert!(
        sql.contains("直接给 qty_in_transit 赋值竟然成功"),
        "迁移自检必须有「直接写生成列必须被拒」的行为探针"
    );
}

/// ⑤ 4 条 CHECK 一条都不能少（漏一条 = 一条脏数据入口）。
#[test]
fn v0420_keeps_four_check_constraints() {
    let sql = read_migration(V0420_UP);

    for (snippet, why) in [
        (
            "CONSTRAINT ck_biz_inv_transit_status CHECK (status IN ('IN_TRANSIT', 'RECEIVED', 'CANCELLED'))",
            "三种状态必须用 CHECK 锁死（多一种状态 = 在途取数口径未定义）",
        ),
        (
            "CONSTRAINT ck_biz_inv_transit_qty CHECK (shipped_qty > 0 AND received_qty >= 0 AND received_qty <= shipped_qty)",
            "数量必须锁死：发货量 > 0、签收 ≥ 0 且不超发货量（否则在途量变负数）",
        ),
        (
            "(status = 'IN_TRANSIT' AND received_qty < shipped_qty)",
            "ck_biz_inv_transit_state 的第一支：IN_TRANSIT 必须还有余量",
        ),
        (
            "(status = 'RECEIVED' AND received_qty = shipped_qty)",
            "ck_biz_inv_transit_state 的第二支：RECEIVED 必须余量为 0",
        ),
        (
            "OR status = 'CANCELLED'",
            "ck_biz_inv_transit_state 的第三支：作废行不受数量一致性约束",
        ),
        (
            "CONSTRAINT ck_biz_inv_transit_received_at CHECK (status <> 'RECEIVED' OR received_at IS NOT NULL)",
            "RECEIVED 必须有签收时间（没有时间的「已签收」无法追溯）",
        ),
    ] {
        assert_has(&sql, snippet, why);
    }

    let check_count = squash(&sql).matches("CONSTRAINT ck_biz_inv_transit_").count();
    assert_eq!(
        check_count, 4,
        "本表应有 4 条 ck_biz_inv_transit_* 约束，实际 {check_count} 条"
    );
}

/// ⑥ 外键口径：`shipment_id` 唯一硬外键 CASCADE；物料/仓库不许加外键。
#[test]
fn v0420_fk_is_cascade_and_master_data_is_weak_reference() {
    let sql = read_migration(V0420_UP);

    assert_has(
        &sql,
        "CONSTRAINT fk_biz_inv_transit_shipment FOREIGN KEY (shipment_id) REFERENCES biz_shipment (id) ON DELETE CASCADE",
        "shipment_id 必须是 ON DELETE CASCADE 的硬外键（在途行是发货单的派生账，与 biz_shipment_item 同口径）",
    );

    let code = squash(&sql_only(&sql));
    // 数 `FOREIGN KEY` 而不是 `REFERENCES`：自检块里有一句
    // `d NOT LIKE '%REFERENCES biz_shipment(id)%'`（它在验约束形状，是字符串字面量，不是外键）。
    let fk_count = code.matches("FOREIGN KEY (").count();
    assert_eq!(
        fk_count, 1,
        "V0420 只许有 1 条外键（shipment_id）：物料/仓库是弱引用 —— 外键会让物料、仓库软删后历史在途账失去可读性（AGENTS.md 弱引用口径）"
    );
    for (col, why) in [("material_id", "物料"), ("warehouse_id", "仓库")] {
        assert!(
            !code.contains(&format!("FOREIGN KEY ({col})")),
            "{col}（{why}）不许加外键：主数据软删后历史在途账必须仍可读",
        );
    }
}

/// ⑦ 索引：1 条普通唯一键（upsert 键）+ 3 条普通索引，全部带 `IF NOT EXISTS`。
#[test]
fn v0420_indexes_are_idempotent_and_unique_key_is_plain() {
    let sql = read_migration(V0420_UP);

    assert_has(
        &sql,
        "CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_inv_transit_key ON biz_inventory_in_transit (material_id, warehouse_id, shipment_id)",
        "缺 (物料, 发货仓, 发货单) 唯一键：M2-02 的 upsert 靠它，重号会让同一批在途算两遍",
    );
    assert_has(
        &sql,
        "CREATE INDEX IF NOT EXISTS idx_biz_inv_transit_shipment ON biz_inventory_in_transit (shipment_id)",
        "缺按发货单取在途的索引（M2-03 签收确认要一次拿全该单的在途行）",
    );
    assert_has(
        &sql,
        "CREATE INDEX IF NOT EXISTS idx_biz_inv_transit_open ON biz_inventory_in_transit (material_id, warehouse_id) WHERE status = 'IN_TRANSIT'",
        "缺「按物料 + 仓库汇总在途」的部分索引（M2-04 物料视角 / 补货建议的可用量口径）",
    );
    assert_has(
        &sql,
        "CREATE INDEX IF NOT EXISTS idx_biz_inv_transit_age ON biz_inventory_in_transit (status, ship_date)",
        "缺在途账龄取数索引（按状态 + 发货日筛「在途超过 N 天」）",
    );

    let code = squash(&sql_only(&sql));
    let unique = code.matches("CREATE UNIQUE INDEX IF NOT EXISTS").count();
    let plain = code.matches("CREATE INDEX IF NOT EXISTS").count();
    assert_eq!(unique, 1, "本表应有 1 条唯一索引，实际 {unique} 条");
    assert_eq!(plain, 3, "本表应有 3 条普通索引，实际 {plain} 条");

    // 唯一键**不带** WHERE：本表没有 deleted_at，部分索引会引出「存活行/墓碑行」两套语义
    assert_has(
        &sql,
        "ON biz_inventory_in_transit (material_id, warehouse_id, shipment_id);",
        "唯一键必须是完整三列、不带谓词（本表无软删，部分索引会造出两种存活口径）",
    );

    // 反证：不许出现不带 IF NOT EXISTS 的索引语句（重放/演练补跑会 already exists 中断）
    let total = code.matches("CREATE UNIQUE INDEX").count() + code.matches("CREATE INDEX").count();
    assert_eq!(
        total,
        unique + plain,
        "仓库里有 {} 处 CREATE [UNIQUE] INDEX 没带 IF NOT EXISTS（重放不安全）",
        total - (unique + plain)
    );
}

/// ⑧ 两条「不做」的决定反向钉死：不加 `deleted_at`、不加库位维度 `location_id`。
///
/// 两条都是**决定**（写进迁移头注释）：
/// · 在途行没有「删一行」的业务动作 —— 作废是状态（CANCELLED），发货单物理删时 CASCADE；
///   留墓碑会造出「这行没删但也不算在途」的第二种隐藏状态，查询漏一处就静默多算。
/// · 卡面口径是「物料 + 仓库 + 发货单」，在途没有库位维度：加了就要跟着维护一条没人读的维度。
#[test]
fn v0420_has_no_soft_delete_and_no_location_dimension() {
    let code = squash(&sql_only(&read_migration(V0420_UP)));
    for forbidden in ["deleted_at", "location_id"] {
        assert!(
            !code.contains(forbidden),
            "V0420 的建表语句里出现了 {forbidden}：这是**决定不做**的列（理由见迁移头注释）。\n\
             要改先改决定与测试，别顺手加"
        );
    }
}

/// ⑨ 自检块：两个 `DO $$` 都有 `RAISE EXCEPTION`，关键断言逐条点名，探针自清场。
#[test]
fn v0420_self_check_raises_exceptions() {
    let sql = read_migration(V0420_UP);
    let blocks: Vec<&str> = sql.split("DO $$").skip(1).collect();
    assert_eq!(
        blocks.len(),
        2,
        "V0420 应有 2 个 DO 块（前置护栏 / 结构+行为自检），实际 {} 个",
        blocks.len()
    );
    for (i, block) in blocks.iter().enumerate() {
        let has_raise = block.contains("RAISE EXCEPTION") || block.contains("RAISE NOTICE");
        assert!(
            has_raise,
            "第 {} 个 DO 块既没有 RAISE EXCEPTION 也没有 RAISE NOTICE —— 自检失败也拦不住迁移",
            i + 1
        );
    }

    for msg in [
        "biz_shipment 不存在，请先应用 V0283",
        "biz_material 不存在",
        "biz_warehouse 不存在",
        "应有 15 列",
        "七个关键列的类型/精度/可空性不对",
        "qty_in_transit 不是生成列",
        "4 条 CHECK 只建成",
        "发货单外键缺失或不是 CASCADE",
        "唯一索引 uk_biz_inv_transit_key 缺失",
        "重号未被 uk_biz_inv_transit_key 拒绝",
        "直接给 qty_in_transit 赋值竟然成功",
        "shipped_qty = 0 未被",
        "received_qty 超过 shipped_qty 未被拒绝",
        "签满仍是 IN_TRANSIT 未被",
        "RECEIVED 无 received_at 未被",
        "status = SHIPPED 未被",
        "shipment_id 指向不存在的发货单却插入成功",
        "部分签收后",
        "签满后",
        "删发货单后仍剩",
        "探针残留",
    ] {
        assert!(sql.contains(msg), "自检断言里必须覆盖「{msg}」（当前找不到）");
    }

    // 探针行必须删干净：迁移重复执行不许往真库里攒垃圾行
    assert!(
        sql.contains("V0420 自检通过"),
        "自检块必须以完成提示收口（否则是被截断的最后一块）"
    );
}

/// ⑩ 探针夹具必须自清场：`__M201_PROBE_*` 行在块尾全部删除。
///
/// 本表的 `shipment_id` 是硬外键，没有真发货单就验不了级联/唯一键 —— 所以允许
/// 往 `biz_sales_order` / `biz_shipment` 插探针行，但**必须删干净**（真库重放零残留）。
#[test]
fn v0420_probe_fixtures_are_cleaned_up() {
    let sql = sql_only(&read_migration(V0420_UP));
    let squashed = squash(&sql);

    for cleanup in [
        "DELETE FROM biz_shipment WHERE id IN (probe_ship, probe_ship2)",
        "DELETE FROM biz_sales_order WHERE id = probe_order",
        "WHERE ship_no LIKE '__M201_PROBE%'",
        "WHERE so_no LIKE '__M201_PROBE%'",
    ] {
        assert_has(&sql, cleanup, &format!("探针夹具缺少清场语句：{cleanup}"));
    }

    // 探针单号必须带明确的卡面孔径前缀（重放时能一眼认出、也不会撞真数据）
    assert!(
        squashed.contains("'__M201_PROBE_SO__'") && squashed.contains("'__M201_PROBE_SH1__'"),
        "探针单号必须用 __M201_PROBE_* 前缀（撞真数据就麻烦了）"
    );
}

/// ⑪ down 幂等 + 有在途行时拒绝回滚 + 结尾断言零残留。
#[test]
fn v0420_down_is_idempotent_and_guarded() {
    let down = read_migration(V0420_DOWN);

    assert_has(
        &down,
        "V0420 down 拒绝执行：biz_inventory_in_transit 还有",
        "down 必须有「还有在途/已签收行就拒绝回滚」的护栏（在途账无法从别的表重建）",
    );
    assert_has(
        &down,
        "WHERE status <> 'CANCELLED'",
        "护栏只应拦在途/已签收行：作废行是纯审计痕迹，可以随表删除",
    );
    assert_has(
        &down,
        "DROP TABLE IF EXISTS biz_inventory_in_transit",
        "down 必须用 DROP TABLE IF EXISTS 收口（重复执行不报错）",
    );
    assert_has(
        &down,
        "biz_inventory_in_transit 不存在，无需回滚（幂等）",
        "down 对「表已不存在」必须先 NOTICE 再 RETURN（幂等）",
    );

    let code = sql_only(&down);
    // 不许用 'tbl'::regclass 这种「表已删就抛错」的写法
    assert!(
        !code.contains(&format!("'{OWN_TABLE}'::regclass")),
        "down 里出现了 '{OWN_TABLE}'::regclass：第二次执行时表已不存在，这种写法会直接抛错（改用 to_regclass）"
    );
    assert!(
        squash(&code).contains("仍然是存在") || squash(&code).contains("仍然存在"),
        "down 自检必须断言「表仍然存在」这种失败态（只 NOTICE 不 EXCEPTION 等于没验）"
    );
    assert!(
        squash(&code).contains("V0420 down 完成"),
        "down 结尾必须有完成提示：删完要断言表与索引确实没了"
    );
}

/// ⑫ 引擎拆分器必须把每个 `DO $$ … $$` 各留成一条语句，且建表/索引/注释条数正确。
///
/// 用框架自己的 `split_statements`（引擎执行迁移走的就是它）验证：`$$` 内部的 `;`
/// 若被当成语句分隔符，PG 会报语法错误、整个迁移事务回滚 ⇒ 服务起不来。
#[test]
fn v0420_splits_cleanly_with_engine_splitter() {
    use nik::__db::migrate::{DialectKind, split_statements};

    let sql = read_migration(V0420_UP);
    let statements =
        split_statements(&sql, DialectKind::Postgres).expect("引擎拆分器无法拆分 V0420（方言感知的 $$ 处理失败）");

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
            "拆分后的 DO 块必须以 END $$ 收口（被截断就会语法错误）"
        );
    }

    let create_table = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE TABLE"))
        .count();
    assert_eq!(create_table, 1, "建表语句应成一条，实际 {create_table} 条");

    let create_unique_index = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE UNIQUE INDEX"))
        .count();
    assert_eq!(
        create_unique_index, 1,
        "唯一索引应成一条，实际 {create_unique_index} 条"
    );

    let create_index = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("CREATE INDEX"))
        .count();
    assert_eq!(create_index, 3, "三条普通索引应各成一条，实际 {create_index} 条");

    let comments = statements
        .iter()
        .filter(|s| sql_head(s).starts_with("COMMENT ON"))
        .count();
    assert_eq!(comments, 10, "1 条表注释 + 9 条列注释应各成一条，实际 {comments} 条");
}
