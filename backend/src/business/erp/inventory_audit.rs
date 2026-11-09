//! 库存巡检（不变式③）——「账面数量 == 在库序列号件数」的自查与修正。
//!
//! 归属：批次 D · W2c-2（云枢 TSK-202610-000764）。前置 W2c-1（[`super::serial_model::recompute_quantity`]）
//! 已经把「数量由序列号派生」写进所有写路径；本文件负责**事后自查**：
//! 一旦有绕过应用的写入（手工 `UPDATE qty`、历史脏账、别的卡没走派生路径），
//! 必须能一次查出来并且定位到 (物料, 仓库)，还能按序列号重算修正。
//!
//! ```text
//! 对每个 serial_required = TRUE 的物料 × 仓库：
//!     biz_inventory.qty  ==  COUNT(biz_material_serial WHERE status='IN_STOCK')
//! ```
//!
//! # 接口
//!
//! ```text
//! GET /api/business/erp/inventory/audit?dry_run=true    （默认 true：只报不修）
//! GET /api/business/erp/inventory/audit?dry_run=false   （按序列号重算 qty）
//! ```
//!
//! 权限码 `business:erp:inventory:audit`（V0143 种进 `sys_menu.auth_code`）。
//!
//! # 三个方向都查（不只是「账面 > 实际」）
//!
//! 1. **账多**：`qty > count(IN_STOCK)`（典型：手工改大、序列号被软删没重算）→ `diff > 0`；
//! 2. **账少**：`qty < count(IN_STOCK)`（典型：序列号入库跳过派生）→ `diff < 0`；
//! 3. **反向：有序列号但没有库存行**（`biz_inventory` 整行缺失，最容易被纯 `UPDATE` 型巡检漏掉）
//!    → `book_qty = 0`、`diff < 0`。用 `FULL OUTER JOIN` 把两侧都并进来，
//!    所以「没建账」不会被当成「一致」。
//!
//! 第 4 类脏数据（`warehouse_id IS NULL` 的在库序列号）也会出现在结果里（`warehouse_id = null`）：
//! 它没法定量到任何一本仓账（`biz_inventory.warehouse_id` 是 NOT NULL），
//! `dry_run=false` 时**跳过并计入 `unfixable`**，不会被静默吞掉，也不能把它「修」成 0。
//!
//! # 一次查完（红线②）
//!
//! 报告是**一条** `WITH … SELECT`（两条 CTE 各自聚合一次 + 一次 `FULL OUTER JOIN`）。
//! 修正是**两条**集合语句（① 单桶差额 `UPDATE`、② 零桶 `INSERT`），都在同一个事务里、
//! 按 `combos` 集合分流（`bucket_count = 1` / `= 0`，互不相交），多桶一律不碰。
//! 绝不按 (物料, 仓库) 循环。
//!
//! 三条语句共用同一个 [`AUDIT_CTE`] 常量 —— 报出来的口径和修的口径只有一份，
//! 不可能出现「报的是一套、修的是另一套」。
//!
//! `checked` 用 `COUNT(*) FROM combos` 一次算出来（`LEFT JOIN combos ON 不一致` 保证
//! 「零不一致」时也返回 1 行，只带 `checked`、其余列 NULL），所以不额外多打一次查询。
//!
//! # 事务
//!
//! 两个函数都吃 `&impl SqlExecutor`：`dry_run=false` 时由
//! [`crate::business::erp::handler::inventory_audit`] 开事务，**扫描与修正在同一事务里**
//! （否则「查到 3 个不一致」和「实际修了 4 个」可能对不上）。`dry_run=true` 只读。
//!
//! # 与卡面的差异（逐条实测）
//!
//! 1. 卡面写「`biz_inventory.qty` 和 `COUNT(*)` 用 `HAVING` 过滤」：真实要同时覆盖**反向**
//!    （没有库存行）就必须 `FULL OUTER JOIN` + `WHERE diff <> 0`，`HAVING` 只对 `GROUP BY`
//!    结果生效、漏掉「行都不存在」那一侧。
//! 2. 卡面响应只有 `checked / mismatched / items / fixed`：本实现额外给
//!    `dry_run`（回显本次是不是真改）与 `unfixable`（无法修的行数），
//!    因为「报了但修不了」必须让调用方看得见。
//! 3. 卡面「`fixed` = 修了几个」：落成 `INSERT … ON CONFLICT DO UPDATE … RETURNING`
//!    的**实际受影响行数** —— 反向脏账是 `INSERT` 建档、账多/账少是 `UPDATE`，两种都算。
//! 4. `book_qty` / `actual_qty` / `diff` 三列统一 `::numeric(18,4)`：PG 的 `COUNT(*)`
//!    是 0 位小数、`qty` 是 4 位，不显式定标会让**同一个字段**在不同行出现 `"3"` 与
//!    `"5.0000"` 两种字符串形态（e2e 实测踩到）。
//!
//! # 与 V0139（W3a · 三列唯一键）的对齐 —— 以及「未分配桶」到底是什么
//!
//! W3a 的 `V0139` 给 `biz_inventory` 加了 `location_id`，唯一键从
//! `uk_biz_inv_mat_wh (material_id, warehouse_id)` 换成
//! `uk_biz_inv_mat_wh_loc (material_id, warehouse_id, COALESCE(location_id, 全零 uuid))`。
//!
//! **`location_id IS NULL` 是「未分配库位」这一个真实的桶，不是整仓汇总行。**
//! 同一个 (物料, 仓库) 在成本账里可以有多行（期初 / 批量生成落未分配桶，收货落默认库位桶），
//! 整仓数量 = **各桶之和**。这一点有三处独立证据：
//!
//! 1. `docs/ERP-库存对账口径.md` §1 / §2.1：`biz_inventory` 的粒度是「桶」，
//!    「库存表 vs 流水累计」按 `(物料, 仓库) GROUP BY` + `SUM(qty)`；
//! 2. `transfer_settle::assert_settle_invariants` 第 4 条：`SUM(biz_inventory_location.qty)
//!    == SUM(biz_inventory.qty)`（两本账**按桶相加**后相等，说明成本账的桶是可加的）；
//! 3. `inventory_writer::add_cost` 等写入口每次只写一个桶，从不「先写库位桶再同步汇总桶」。
//!
//! 所以 **[`AUDIT_CTE`] 的 `book` 必须 `SUM(i.qty)`**。历史实现写的是
//! `COALESCE(MAX(qty) FILTER (WHERE location_id IS NULL), SUM(qty))`，
//! 即「有未分配桶就只取那一桶、忽略库位桶」—— 两桶并存时必然报**假差异**
//! （E2E-03 实测：`Σqty = 5`（未分配 2 + 默认库位 3）报成 `book = 2`，`diff = -3`）。
//!
//! 1. **账面侧**（[`AUDIT_CTE`] 的 `book`）：`SUM(i.qty)`，同时 `COUNT(*)` 出桶数
//!    （`bucket_count`，修正路径据此判断「能不能自动修」）。
//! 2. **修正侧**（[`inventory_writer::align_cost_qty_delta_sql`] /
//!    [`inventory_writer::align_cost_qty_insert_sql`]）：**单桶按差额调整**
//!    （`qty += actual - book`，不覆盖）、**多桶一律不动**（标 `MULTI_BUCKET` 交人工）、
//!    **零桶建档**（`INSERT` 到未分配桶），冲突目标是三列表达式 `uk_biz_inv_mat_wh_loc`。
//!
//! 卡面写的是「不改 V0139」，那不是「不用管 V0139」：本文件在 V0139 落地后仍要按新唯一键写
//! （这里已对齐并实测），但**不碰** V0139 文件本身。
//!
//! # 数据权限（门禁闸①/闸②）
//!
//! ```text
//! SCOPE-EXEMPT: 即时库存是仓储数据（V0137 表里没有 dept_id / 归属人列），
//! 巡检是**全库一致性**自查，按部门注入范围会让「别人仓库的脏账查不出来」，
//! 比漏权限更危险；可见性由 business:erp:inventory:audit 权限码控制。
//! 本文件只有聚合查询，没有分页接口（不走 scope::paginate）。
//! ```

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use super::inventory_writer;

// ==================== 权限与路由 ====================

/// 巡检权限码（与 `V0143__erp_audit_permission.up.sql` 逐字一致）。
pub const AUDIT_PERMISSION: &str = "business:erp:inventory:audit";

/// 本模块声明的权限码清单（口径同 [`crate::business::erp::serial_handler::SERIAL_PERMISSIONS`]）。
pub const AUDIT_PERMISSIONS: [(&str, &str); 1] = [(AUDIT_PERMISSION, "库存巡检（不变式③）")];

/// 巡检路由（前缀 `/api` 由 `[router].prefix` 注入）。
pub const AUDIT_ROUTE: &str = "/business/erp/inventory/audit";

/// 巡检口径里「物料编码」的默认过滤（`LIKE '%'` = 全量）。
///
/// 留成参数是为了单测能在**共享测试库**里只校验自己造的那几个物料
/// （别的模块的夹具同时在同一张表上跑，全量断言会互相污染）。
pub const AUDIT_ALL_MATERIALS: &str = "%";

// ==================== SQL（报告与修正共用一个 CTE） ====================

/// 巡检口径的唯一实现：把「账面」与「在库序列号数」聚合成 `(物料, 仓库)` 级的一行。
///
/// 两个 `?::text` 是物料编码 `LIKE` 的过滤（生产传 `'%'`）。
/// `FULL OUTER JOIN` 是**反向巡检**的关键：`biz_inventory` 缺行的那一侧也要出现。
const AUDIT_CTE: &str = "\
WITH serial_cnt AS ( \
    SELECT s.material_id, s.warehouse_id, COUNT(*)::numeric AS actual_qty \
      FROM biz_material_serial s \
      JOIN biz_material m ON m.id = s.material_id \
     WHERE m.serial_required IS TRUE AND m.deleted_at IS NULL \
       AND m.material_code LIKE ?::text \
       AND s.status = 'IN_STOCK' AND s.deleted_at IS NULL \
     GROUP BY s.material_id, s.warehouse_id \
), book AS ( \
    SELECT i.material_id, i.warehouse_id, \
           SUM(i.qty)::numeric AS book_qty, \
           COUNT(*) AS bucket_count \
      FROM biz_inventory i \
      JOIN biz_material m ON m.id = i.material_id \
     WHERE m.serial_required IS TRUE AND m.deleted_at IS NULL \
       AND m.material_code LIKE ?::text \
     GROUP BY i.material_id, i.warehouse_id \
), combos AS ( \
    SELECT COALESCE(b.material_id, a.material_id) AS material_id, \
           COALESCE(b.warehouse_id, a.warehouse_id) AS warehouse_id, \
           COALESCE(b.book_qty, 0)::numeric AS book_qty, \
           COALESCE(a.actual_qty, 0)::numeric AS actual_qty, \
           COALESCE(b.bucket_count, 0)::integer AS bucket_count \
      FROM book b \
      FULL OUTER JOIN serial_cnt a \
        ON a.material_id = b.material_id AND a.warehouse_id = b.warehouse_id \
)";

/// 报告语句：一条 `WITH … SELECT`，返回 `checked` + 所有不一致行（零不一致时返回 1 行空明细）。
fn audit_sql() -> String {
    format!(
        "{AUDIT_CTE} \
         , stats AS (SELECT COUNT(*) AS checked FROM combos) \
         SELECT s.checked, c.material_id, m.material_code, m.material_name, \
                c.warehouse_id, w.warehouse_name, \
                c.bucket_count, \
                c.book_qty::numeric(18,4) AS book_qty, \
                c.actual_qty::numeric(18,4) AS actual_qty, \
                (c.book_qty - c.actual_qty)::numeric(18,4) AS diff \
           FROM stats s \
           LEFT JOIN combos c ON c.book_qty <> c.actual_qty \
           LEFT JOIN biz_material m ON m.id = c.material_id \
           LEFT JOIN biz_warehouse w ON w.id = c.warehouse_id \
          ORDER BY m.material_code NULLS LAST, w.warehouse_name NULLS LAST"
    )
}

/// 修正语句（只用于单测断言 SQL 形态）。生产路径直接用写入口执行。
#[cfg(test)]
fn fix_sql() -> String {
    // P2-5A：写语句本体住在唯一写入口 [`inventory_writer`]，这里只把巡检侧「报的口径」
    // 那段只读 CTE 拼上去（读与写共用同一段 combos 定义）。
    inventory_writer::align_cost_qty_delta_sql(AUDIT_CTE)
}

// ==================== 契约 ====================

/// 一条不一致明细（`(物料, 仓库)` 级）。
#[derive(Debug, Clone, Serialize)]
pub struct AuditItem {
    /// 物料主键。
    pub material_id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 仓库主键（`None` = 在库序列号没有仓库归属，无法定量到仓账）。
    pub warehouse_id: Option<String>,
    /// 仓库名称（与 `warehouse_id` 同生共死）。
    pub warehouse_name: Option<String>,
    /// 该 `(物料, 仓库)` 在成本账里有多少个桶（`biz_inventory` 行数）。
    ///
    /// `1` = 单桶（可自动修）；`0` = 一行没建账（可建档）；`≥2` = 多桶并存（拒绝自动修）。
    pub bucket_count: i32,
    /// 账面数量（该 `(物料, 仓库)` **所有桶之和**；行不存在时按 0 报）。
    pub book_qty: Decimal,
    /// 实际在库件数（`COUNT(status='IN_STOCK' AND deleted_at IS NULL)`）。
    pub actual_qty: Decimal,
    /// 账面 - 实际（正数 = 账多，负数 = 账少）。
    pub diff: Decimal,
    /// 本行能否被 `dry_run=false` 修正（缺仓库或多桶都不能）。
    pub fixable: bool,
    /// 不能自动修的原因：`NO_WAREHOUSE` / `MULTI_BUCKET`；`None` = 可修。
    pub unfixable_reason: Option<String>,
}

/// 在库序列号没有仓库归属 → 写不进 `biz_inventory`，没法修。
pub const UNFIXABLE_NO_WAREHOUSE: &str = "NO_WAREHOUSE";
/// 该 `(物料, 仓库)` 成本账里有多行桶 → 差额该记哪个桶有歧义，拒绝自动修。
pub const UNFIXABLE_MULTI_BUCKET: &str = "MULTI_BUCKET";

/// 巡检报告（卡面 §二 的响应体 + 两个诊断字段）。
#[derive(Debug, Clone, Serialize)]
pub struct AuditReport {
    /// 本次是否**只报不修**。
    pub dry_run: bool,
    /// 检查了多少个 `(物料, 仓库)` 组合（含「有序列号没库存行」那类）。
    pub checked: u64,
    /// 不一致的组合数（= `items.len()`）。
    pub mismatched: u64,
    /// 不一致明细。
    pub items: Vec<AuditItem>,
    /// 实际修好的行数（`dry_run=true` 恒为 0；多桶被跳过的行不算）。
    pub fixed: u64,
    /// 报出来但**修不了**的行数（缺仓库归属 / 多桶并存）。
    pub unfixable: u64,
}

impl AuditReport {
    /// 只有检查数的空报告（`dry_run` 决定语义）。
    fn empty(dry_run: bool, checked: u64) -> Self {
        Self {
            dry_run,
            checked,
            mismatched: 0,
            items: Vec::new(),
            fixed: 0,
            unfixable: 0,
        }
    }

    /// 库里完全没有 `serial_required` 物料时也算「巡检过一轮」（`checked = 0`）。
    pub fn is_clean(&self) -> bool {
        self.mismatched == 0
    }
}

// ==================== 参数解析 ====================

/// `?dry_run=` 的解析：**缺省 true**（红线③：防误修）。
///
/// 接受 `true/false`、`1/0`、`yes/no`、`on/off`（大小写不敏感）与空串（= 缺省）。
/// 其它值一律报错 —— 静默把 `dry_run=treu` 当成 `false` 会直接改库。
pub fn parse_dry_run(raw: Option<&str>) -> std::result::Result<bool, String> {
    match raw.map(str::trim) {
        None | Some("") => Ok(true),
        Some(text) => {
            let lowered = text.to_ascii_lowercase();
            match lowered.as_str() {
                "true" | "1" | "yes" | "on" => Ok(true),
                "false" | "0" | "no" | "off" => Ok(false),
                _ => Err(format!(
                    "dry_run 只接受 true/false（或 1/0、yes/no、on/off）：{text}"
                )),
            }
        }
    }
}

// ==================== 巡检 ====================

/// 读一次巡检明细（只读，不开事务）。
///
/// `prefix` 是物料编码 `LIKE` 过滤；生产入口传 [`AUDIT_ALL_MATERIALS`]。
async fn scan(ex: &impl SqlExecutor, prefix: &str) -> DbResult<AuditReport> {
    let rows = ex
        .query_sql(audit_sql(), vec![Value::from(prefix), Value::from(prefix)])
        .await?;
    let mut report = AuditReport::empty(true, 0);
    for row in &rows {
        report.checked = u64::try_from(row.get_typed::<i64>("checked")?).unwrap_or(0);
        // `LEFT JOIN combos ON 不一致`：零不一致时只回一行、明细列全 NULL
        let Some(material_id) = row.get_typed::<Option<Uuid>>("material_id")? else {
            continue;
        };
        let warehouse_id = row.get_typed::<Option<Uuid>>("warehouse_id")?;
        let bucket_count = row.get_typed::<i32>("bucket_count")?;
        let unfixable_reason = if warehouse_id.is_none() {
            Some(UNFIXABLE_NO_WAREHOUSE.to_string())
        } else if bucket_count > 1 {
            Some(UNFIXABLE_MULTI_BUCKET.to_string())
        } else {
            None
        };
        report.items.push(AuditItem {
            material_id: material_id.to_string(),
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            warehouse_id: warehouse_id.map(|id| id.to_string()),
            warehouse_name: row.get_typed("warehouse_name")?,
            bucket_count,
            book_qty: row.get_typed("book_qty")?,
            actual_qty: row.get_typed("actual_qty")?,
            diff: row.get_typed("diff")?,
            fixable: unfixable_reason.is_none(),
            unfixable_reason,
        });
    }
    report.mismatched = u64::try_from(report.items.len()).unwrap_or(0);
    report.unfixable =
        u64::try_from(report.items.iter().filter(|i| !i.fixable).count()).unwrap_or(0);
    Ok(report)
}

/// 执行修正（单桶差额调整 + 零桶建档；多桶跳过），返回实际改动行数。
async fn fix(ex: &impl SqlExecutor, prefix: &str) -> DbResult<u64> {
    // P2-5A：执行也走唯一写入口（SQL 只有一份，报与修不可能各写一套）。
    inventory_writer::align_cost_qty_batch(
        ex,
        AUDIT_CTE,
        vec![Value::from(prefix), Value::from(prefix)],
    )
    .await
}

/// 巡检入口（`prefix` = 物料编码过滤，测试用）。
async fn audit_scoped(ex: &impl SqlExecutor, dry_run: bool, prefix: &str) -> DbResult<AuditReport> {
    // 扫描与修正在同一个 executor 里发生：调用方传 `&tx` 时整段是原子的。
    let mut report = scan(ex, prefix).await?;
    report.dry_run = dry_run;
    if dry_run || report.mismatched == 0 {
        return Ok(report);
    }
    report.fixed = fix(ex, prefix).await?;
    Ok(report)
}

/// 巡检（全量）：`dry_run=true` 只报不修（默认），`false` 按序列号重算并修正。
///
/// 事务边界由调用方决定（接口层用 `db().begin()` 包住扫描 + 修正）。
pub async fn audit(ex: &impl SqlExecutor, dry_run: bool) -> DbResult<AuditReport> {
    audit_scoped(ex, dry_run, AUDIT_ALL_MATERIALS).await
}

// ==================== 单测（真库用例需 ERP_DB_URL，未设置则跳过） ====================

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- 夹具（物料编码统一 `W2C2-` 前缀，巡检按前缀自校验，不受别的卡污染） ----------
    const PREFIX: &str = "W2C2";
    const W_A: &str = "b2c20002-0000-4000-8000-000000000001";
    const W_B: &str = "b2c20002-0000-4000-8000-000000000002";
    /// 库位字典（`biz_location`）——只是 `biz_inventory_location` 的上游；
    /// 序列号的 `location_id` **不指向它**。
    const LOC_A: &str = "b2c20003-0000-4000-8000-000000000001";
    /// 库位库存行：`biz_material_serial.location_id` 的硬外键指向 **`biz_inventory_location.id`**
    /// （不是 `biz_location.id`，见 serial_model 文件头第 3 条差异）—— 夹具必须两跳都建。
    const IL_A: &str = "b2c20008-0000-4000-8000-000000000001";
    const M_SN: &str = "b2c20004-0000-4000-8000-000000000001";
    const M_RT: &str = "b2c20004-0000-4000-8000-000000000002";
    const M_PLAIN: &str = "b2c20004-0000-4000-8000-000000000003";
    /// 三个在 `M_SN × W_A` 里的在库序列号（move_serials 用它们）。
    const SN_IDS: [&str; 3] = [
        "b2c20005-0000-4000-8000-000000000001",
        "b2c20005-0000-4000-8000-000000000002",
        "b2c20005-0000-4000-8000-000000000003",
    ];

    /// 夹具：每个用例开始前**重建**成「全部一致」的基线。
    ///
    /// - `M_SN × W_A`：3 条序列号 + `qty = 3`；
    /// - `M_RT × W_B`：1 条序列号 + `qty = 1`（用例④删库存行造反向脏账）；
    /// - `M_PLAIN`：按数量管理的物料（巡检必须看不见它）。
    const FIXTURES: &[&str] = &[
        "DELETE FROM biz_inventory WHERE material_id IN \
            ('b2c20004-0000-4000-8000-000000000001', \
             'b2c20004-0000-4000-8000-000000000002', \
             'b2c20004-0000-4000-8000-000000000003')",
        "DELETE FROM biz_material_serial WHERE material_id IN \
            ('b2c20004-0000-4000-8000-000000000001', \
             'b2c20004-0000-4000-8000-000000000002')",
        "DELETE FROM biz_inventory_location WHERE material_id IN \
            ('b2c20004-0000-4000-8000-000000000001', \
             'b2c20004-0000-4000-8000-000000000002')",
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, status, sort_order) \
         VALUES ('b2c20001-0000-4000-8000-000000000001', 'W2C2-PCS', '个', 1, 1, 1) \
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, \
                                    allow_negative, status, sort_order, parent_id, level, \
                                    warehouse_path, is_leaf) \
         VALUES ('b2c20002-0000-4000-8000-000000000001', 'W2C2-WH-A', 'W2C2 A 仓', 'NORMAL', \
                 0, 1, 11, NULL, 1, '/b2c20002-0000-4000-8000-000000000001/', 1), \
                ('b2c20002-0000-4000-8000-000000000002', 'W2C2-WH-B', 'W2C2 B 仓', 'NORMAL', \
                 0, 1, 12, NULL, 1, '/b2c20002-0000-4000-8000-000000000002/', 1) \
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, \
                                   location_type, is_default, capacity, status, sort_order) \
         VALUES ('b2c20003-0000-4000-8000-000000000001', 'W2C2-A-01', 'W2C2 A 区一号', \
                 'b2c20002-0000-4000-8000-000000000001', 'STORAGE', 0, 100.0000, 1, 1) \
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type, \
                                   safety_stock, costing_method, tax_rate, status, serial_required) \
         VALUES ('b2c20004-0000-4000-8000-000000000001', 'W2C2-M-SN', 'W2C2 巡检一物一码物料', \
                 'SN-1', 'b2c20001-0000-4000-8000-000000000001', 'FINISHED', 0.0000, \
                 'MOVING_AVG', 0.1300, 1, TRUE), \
                ('b2c20004-0000-4000-8000-000000000002', 'W2C2-M-RT', 'W2C2 反向巡检物料', \
                 'SN-2', 'b2c20001-0000-4000-8000-000000000001', 'FINISHED', 0.0000, \
                 'MOVING_AVG', 0.1300, 1, TRUE), \
                ('b2c20004-0000-4000-8000-000000000003', 'W2C2-M-PLAIN', 'W2C2 按数量物料', \
                 NULL, 'b2c20001-0000-4000-8000-000000000001', 'RAW', 0.0000, \
                 'MOVING_AVG', 0.1300, 1, FALSE) \
         ON CONFLICT DO NOTHING",
        // 一物一码开关可能被别的卡的夹具改过 → 幂等纠正
        "UPDATE biz_material SET serial_required = TRUE \
          WHERE id IN ('b2c20004-0000-4000-8000-000000000001', \
                       'b2c20004-0000-4000-8000-000000000002')",
        "UPDATE biz_material SET serial_required = FALSE \
          WHERE id = 'b2c20004-0000-4000-8000-000000000003'",
        // 库位库存行（序列号的 location_id 指向它）
        "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty) \
         VALUES ('b2c20008-0000-4000-8000-000000000001', \
                 'b2c20004-0000-4000-8000-000000000001', \
                 'b2c20002-0000-4000-8000-000000000001', \
                 'b2c20003-0000-4000-8000-000000000001', 3.0000)",
        // 3 条在库序列号（M_SN × W_A），都落在 A 区库位上
        "INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, location_id, \
                                          status, inbound_at) \
         VALUES ('b2c20005-0000-4000-8000-000000000001', 'W2C2-SN-0001', \
                 'b2c20004-0000-4000-8000-000000000001', \
                 'b2c20002-0000-4000-8000-000000000001', \
                 'b2c20008-0000-4000-8000-000000000001', 'IN_STOCK', now()), \
                ('b2c20005-0000-4000-8000-000000000002', 'W2C2-SN-0002', \
                 'b2c20004-0000-4000-8000-000000000001', \
                 'b2c20002-0000-4000-8000-000000000001', \
                 'b2c20008-0000-4000-8000-000000000001', 'IN_STOCK', now()), \
                ('b2c20005-0000-4000-8000-000000000003', 'W2C2-SN-0003', \
                 'b2c20004-0000-4000-8000-000000000001', \
                 'b2c20002-0000-4000-8000-000000000001', \
                 'b2c20008-0000-4000-8000-000000000001', 'IN_STOCK', now())",
        // 反向用例的序列号（M_RT × W_B）
        "INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, status, inbound_at) \
         VALUES ('b2c20005-0000-4000-8000-000000000004', 'W2C2-SN-0004', \
                 'b2c20004-0000-4000-8000-000000000002', \
                 'b2c20002-0000-4000-8000-000000000002', 'IN_STOCK', now())",
        // 基线账：两条都一致（3 / 1）
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount) \
         VALUES ('b2c20006-0000-4000-8000-000000000001', \
                 'b2c20004-0000-4000-8000-000000000001', \
                 'b2c20002-0000-4000-8000-000000000001', 3.0000, 0.0000, 10.000000, 30.0000), \
                ('b2c20006-0000-4000-8000-000000000002', \
                 'b2c20004-0000-4000-8000-000000000002', \
                 'b2c20002-0000-4000-8000-000000000002', 1.0000, 0.0000, 10.000000, 10.0000)",
        // 按数量管理的物料：账面 7 件（巡检绝不该看见它）
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount) \
         VALUES ('b2c20006-0000-4000-8000-000000000003', \
                 'b2c20004-0000-4000-8000-000000000003', \
                 'b2c20002-0000-4000-8000-000000000001', 7.0000, 0.0000, 20.000000, 140.0000)",
    ];

    /// 真库用例串行（同一个库，别的模块的夹具也在跑；有 `UPDATE` 的用例必须互斥）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 初始化全局 db（只做一次）；`false` = 没配 `ERP_DB_URL`。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        for sql in FIXTURES {
            if let Err(err) = db().execute_with_params(sql, vec![]).await {
                panic!("夹具写入失败：{err}\nSQL: {sql}");
            }
        }
        Some(guard)
    }

    fn uuid(text: &str) -> Uuid {
        Uuid::parse_str(text).expect("夹具 UUID 非法")
    }

    /// 巡检（按 `W2C2-` 前缀自校验，不受共享测试库其它夹具影响）。
    async fn audit_prefix(dry_run: bool) -> AuditReport {
        let prefix = format!("{PREFIX}-%");
        audit_scoped(db(), dry_run, &prefix)
            .await
            .expect("巡检执行失败")
    }

    async fn book_qty(material_id: &str, warehouse_id: &str) -> Option<Decimal> {
        let rows = db()
            .find_with_params(
                "SELECT qty FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
                vec![
                    Value::from(&uuid(material_id)),
                    Value::from(&uuid(warehouse_id)),
                ],
            )
            .await
            .expect("读账面数量");
        rows.first().map(|row| row.get_typed("qty").expect("qty"))
    }

    /// 把某 `(物料, 仓库)` 的成本账桶**重排**成给定的一组 `(库位账行 id, 数量)`；
    /// `None` = 未分配桶（`location_id IS NULL`）。`qty` 用字符串传，落 `numeric`。
    ///
    /// 用于「两桶并存」这类场景：基线是一个未分配桶，先清掉再造。
    async fn set_buckets(material_id: &str, warehouse_id: &str, buckets: &[(Option<&str>, &str)]) {
        db().execute_with_params(
            "DELETE FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![
                Value::from(&uuid(material_id)),
                Value::from(&uuid(warehouse_id)),
            ],
        )
        .await
        .expect("清空成本账桶");
        for (location_row, qty) in buckets {
            db().execute_with_params(
                "INSERT INTO biz_inventory \
                     (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount) \
                 VALUES (gen_random_uuid(), ?::uuid, ?::uuid, NULLIF(?::text,'')::uuid, \
                         NULLIF(?::text,'')::numeric, 0, 10.000000, \
                         NULLIF(?::text,'')::numeric * 10)",
                vec![
                    Value::from(&uuid(material_id)),
                    Value::from(&uuid(warehouse_id)),
                    Value::Text(location_row.map(str::to_string).unwrap_or_default()),
                    Value::Text((*qty).to_string()),
                    Value::Text((*qty).to_string()),
                ],
            )
            .await
            .expect("造成本账桶");
        }
    }

    /// 读某 `(物料, 仓库)` 的全部桶：`(location_id, qty)`，按 `location_id` 升序（NULL 在前）。
    async fn buckets(material_id: &str, warehouse_id: &str) -> Vec<(Option<Uuid>, Decimal)> {
        let rows = db()
            .find_with_params(
                "SELECT location_id, qty FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                  ORDER BY location_id NULLS FIRST",
                vec![
                    Value::from(&uuid(material_id)),
                    Value::from(&uuid(warehouse_id)),
                ],
            )
            .await
            .expect("读成本账桶");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed("location_id").expect("location_id"),
                    row.get_typed("qty").expect("qty"),
                )
            })
            .collect()
    }

    async fn serial_row(id: &str) -> (Option<Uuid>, Option<Uuid>, String) {
        let rows = db()
            .find_with_params(
                "SELECT warehouse_id, location_id, status FROM biz_material_serial \
                  WHERE id = ?::uuid",
                vec![Value::from(&uuid(id))],
            )
            .await
            .expect("读序列号");
        let row = rows.first().expect("序列号应该还在");
        (
            row.get_typed("warehouse_id").expect("warehouse_id"),
            row.get_typed("location_id").expect("location_id"),
            row.get_typed("status").expect("status"),
        )
    }

    // ---------- 纯逻辑用例（不连库） ----------

    /// ⑩-逻辑：权限码与路由逐字对齐卡面（真 403 由 `scripts/verify/w2c2_audit.sh` 打）。
    #[test]
    fn permission_code_matches_card() {
        assert_eq!(AUDIT_PERMISSIONS.len(), 1);
        assert_eq!(AUDIT_PERMISSIONS[0].0, "business:erp:inventory:audit");
        assert!(AUDIT_PERMISSIONS[0].1.contains("巡检"));
        assert_eq!(AUDIT_PERMISSION, AUDIT_PERMISSIONS[0].0);
        assert_eq!(AUDIT_ROUTE, "/business/erp/inventory/audit");
        // 巡检 SQL 必须同时覆盖两个方向：FULL OUTER JOIN（反向）+ diff 过滤
        let sql = audit_sql();
        assert!(
            sql.contains("FULL OUTER JOIN"),
            "反向巡检靠 FULL OUTER JOIN"
        );
        assert!(sql.contains("<> c.actual_qty"), "只报不一致");
        assert!(
            sql.matches("COUNT(*)").count() >= 1,
            "checked 用一次聚合算出来：{sql}"
        );
        // book_qty 口径：按 (物料, 仓库) 把**所有桶相加**。
        // E2E-03 的 P1（两桶并存时只取未分配桶 → 假差异 + 修复会抹掉库位桶）就死在这条上。
        assert!(
            sql.contains("SUM(i.qty)::numeric AS book_qty"),
            "账面必须是 SUM(所有桶)：{sql}"
        );
        assert!(
            !sql.contains("FILTER (WHERE"),
            "不许再按 location_id 挑桶（那是假差异的来源）：{sql}"
        );
        assert!(
            sql.contains("COUNT(*) AS bucket_count"),
            "要同时报出桶数（修正路径据此判断能否自动修）：{sql}"
        );
        assert!(sql.contains("c.bucket_count"), "报告要带出 bucket_count：{sql}");
        // 修正语句：单桶差额调整（不是覆盖），且必须带三列表达式的 ON CONFLICT 目标备用；
        let fix = fix_sql();
        assert!(
            fix.contains("qty = i.qty + (b.actual_qty - b.book_qty)"),
            "单桶必须按差额调整，不许覆盖：{fix}"
        );
        assert!(
            fix.contains("AND bucket_count = 1"),
            "多桶必须被排除在自动修之外：{fix}"
        );
        let insert = inventory_writer::align_cost_qty_insert_sql(AUDIT_CTE);
        assert!(
            insert.contains("AND bucket_count = 0"),
            "只有整行没建账才 INSERT：{insert}"
        );
        assert!(
            insert.contains("ON CONFLICT (material_id, warehouse_id,"),
            "冲突目标必须是三列表达式：{insert}"
        );
        assert!(
            insert.contains("COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid)"),
            "冲突目标必须与 uk_biz_inv_mat_wh_loc 逐字一致：{insert}"
        );
        assert!(insert.contains("DO UPDATE"));
        assert!(insert.contains("warehouse_id IS NOT NULL"), "没有仓库的行不修");
    }

    /// ⑪ `dry_run` 缺省 true（红线③），非法值必须报错而不是静默当成 false。
    #[test]
    fn dry_run_defaults_to_true() {
        assert_eq!(parse_dry_run(None), Ok(true));
        assert_eq!(parse_dry_run(Some("")), Ok(true));
        assert_eq!(parse_dry_run(Some("  ")), Ok(true));
        for raw in ["true", "TRUE", "1", "yes", "on"] {
            assert_eq!(parse_dry_run(Some(raw)), Ok(true), "{raw}");
        }
        for raw in ["false", "False", "0", "no", "off"] {
            assert_eq!(parse_dry_run(Some(raw)), Ok(false), "{raw}");
        }
        let err = parse_dry_run(Some("treu")).expect_err("拼错必须报错");
        assert!(err.contains("dry_run"), "{err}");
    }

    /// ⑫ 空报告的语义：`is_clean()` 只看 mismatched。
    #[test]
    fn empty_report_is_clean() {
        let report = AuditReport::empty(true, 0);
        assert!(report.is_clean());
        assert_eq!(report.fixed, 0);
        assert!(report.items.is_empty());
        let mut dirty = AuditReport::empty(false, 5);
        dirty.mismatched = 1;
        assert!(!dirty.is_clean());
    }

    // ---------- 真库用例 ----------

    /// ① 基线（3 条序列号 + qty=3、1 条 + qty=1）→ 前缀内零不一致。
    #[tokio::test]
    async fn audit_clean_when_consistent() {
        let Some(_guard) = setup().await else { return };
        let report = audit_prefix(true).await;
        assert_eq!(report.mismatched, 0, "基线不该有不一致：{:?}", report.items);
        assert!(report.items.is_empty());
        assert_eq!(report.checked, 2, "两个 (物料, 仓库) 组合：M_SN×A、M_RT×B");
        assert_eq!(report.fixed, 0, "dry_run 恒为 0");
        assert!(report.dry_run);
    }

    /// ② 账多 2 件（手工 `UPDATE qty`）→ 能查出来，diff = 2。
    #[tokio::test]
    async fn audit_finds_overbooked() {
        let Some(_guard) = setup().await else { return };
        db().execute_with_params(
            "UPDATE biz_inventory SET qty = 5 \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![Value::from(&uuid(M_SN)), Value::from(&uuid(W_A))],
        )
        .await
        .expect("造脏账");

        let report = audit_prefix(true).await;
        assert_eq!(report.mismatched, 1, "{:?}", report.items);
        let item = &report.items[0];
        assert_eq!(item.material_code, "W2C2-M-SN");
        assert_eq!(item.warehouse_name.as_deref(), Some("W2C2 A 仓"));
        assert_eq!(item.book_qty, Decimal::from(5));
        assert_eq!(item.actual_qty, Decimal::from(3));
        assert_eq!(item.diff, Decimal::from(2), "正数 = 账多");
        assert!(item.fixable);
        assert_eq!(report.unfixable, 0);
    }

    /// ③ 账少 1 件 → diff = -1。
    #[tokio::test]
    async fn audit_finds_underbooked() {
        let Some(_guard) = setup().await else { return };
        db().execute_with_params(
            "UPDATE biz_inventory SET qty = 2 \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![Value::from(&uuid(M_SN)), Value::from(&uuid(W_A))],
        )
        .await
        .expect("造脏账");

        let report = audit_prefix(true).await;
        assert_eq!(report.mismatched, 1, "{:?}", report.items);
        let item = &report.items[0];
        assert_eq!(item.book_qty, Decimal::from(2));
        assert_eq!(item.actual_qty, Decimal::from(3));
        assert_eq!(item.diff, Decimal::from(-1), "负数 = 账少");
    }

    /// ④ 反向：有在库序列号但 `biz_inventory` 整行缺失 → 也要查出来（book=0）。
    #[tokio::test]
    async fn audit_finds_serials_without_inventory_row() {
        let Some(_guard) = setup().await else { return };
        db().execute_with_params(
            "DELETE FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![Value::from(&uuid(M_RT)), Value::from(&uuid(W_B))],
        )
        .await
        .expect("删库存行造反向脏账");
        assert_eq!(book_qty(M_RT, W_B).await, None, "库存行应该没了");

        let report = audit_prefix(true).await;
        assert_eq!(report.mismatched, 1, "{:?}", report.items);
        let item = &report.items[0];
        assert_eq!(item.material_code, "W2C2-M-RT");
        assert_eq!(item.book_qty, Decimal::ZERO, "没有库存行按 0 报");
        assert_eq!(item.actual_qty, Decimal::from(1));
        assert_eq!(item.diff, Decimal::from(-1));
        assert!(item.fixable, "有仓库维度 → 可以 INSERT 建档");
    }

    /// ⑤ `dry_run=true` → 数据一行都不改（含账多与反向两种脏账）。
    #[tokio::test]
    async fn dry_run_does_not_touch_data() {
        let Some(_guard) = setup().await else { return };
        db().execute_with_params(
            "UPDATE biz_inventory SET qty = 9 \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![Value::from(&uuid(M_SN)), Value::from(&uuid(W_A))],
        )
        .await
        .expect("造脏账");
        db().execute_with_params(
            "DELETE FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![Value::from(&uuid(M_RT)), Value::from(&uuid(W_B))],
        )
        .await
        .expect("造反向脏账");

        let report = audit_prefix(true).await;
        assert_eq!(report.mismatched, 2, "{:?}", report.items);
        assert_eq!(report.fixed, 0, "dry_run 不修");
        assert_eq!(
            book_qty(M_SN, W_A).await,
            Some(Decimal::from(9)),
            "账面不许被改"
        );
        assert_eq!(book_qty(M_RT, W_B).await, None, "不许被凭空建档");
    }

    /// ⑥ `dry_run=false` → 修正后 `qty == actual_qty`（含反向建档）。
    #[tokio::test]
    async fn dry_run_false_fixes_quantities() {
        let Some(_guard) = setup().await else { return };
        db().execute_with_params(
            "UPDATE biz_inventory SET qty = 9, avg_cost = 2.5 \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![Value::from(&uuid(M_SN)), Value::from(&uuid(W_A))],
        )
        .await
        .expect("造脏账");
        db().execute_with_params(
            "DELETE FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![Value::from(&uuid(M_RT)), Value::from(&uuid(W_B))],
        )
        .await
        .expect("造反向脏账");

        let report = audit_prefix(false).await;
        assert_eq!(report.mismatched, 2, "{:?}", report.items);
        assert_eq!(report.fixed, 2, "一改一建：两行都该被修");
        assert!(!report.dry_run);
        // 账多那行是单桶（可修）；反向那行一个桶都没有（可建档）
        let mut counts: Vec<i32> = report.items.iter().map(|i| i.bucket_count).collect();
        counts.sort_unstable();
        assert_eq!(counts, vec![0, 1], "一行为 0 桶（反向建档）、一行为 1 桶（差额调整）");
        assert!(report.items.iter().all(|i| i.fixable && i.unfixable_reason.is_none()));

        assert_eq!(
            book_qty(M_SN, W_A).await,
            Some(Decimal::from(3)),
            "账多 → 改回 3"
        );
        assert_eq!(
            book_qty(M_RT, W_B).await,
            Some(Decimal::from(1)),
            "反向 → 建档 1"
        );

        // 再巡检一次：前缀内必须全绿
        let again = audit_prefix(true).await;
        assert_eq!(again.mismatched, 0, "修完还有不一致：{:?}", again.items);

        // `amount = qty * avg_cost`（数量变了金额必须跟着变）
        let rows = db()
            .find_with_params(
                "SELECT qty, amount, avg_cost FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
                vec![Value::from(&uuid(M_SN)), Value::from(&uuid(W_A))],
            )
            .await
            .expect("读金额");
        let row = rows.first().expect("库存行");
        let qty: Decimal = row.get_typed("qty").expect("qty");
        let amount: Decimal = row.get_typed("amount").expect("amount");
        let avg: Decimal = row.get_typed("avg_cost").expect("avg_cost");
        assert_eq!(amount, qty * avg, "金额必须随数量重算");
    }

    /// ⑦-bis **两桶并存、总数正确** → 零漂移（E2E-03 P1 的回归：不再报假红）。
    ///
    /// 场景来自演练现场：一物一码物料「批量生成 3 件」落未分配桶、同仓「入库 3 件」落默认库位桶，
    /// 两种写法并存但总数对。旧口径 `COALESCE(MAX(NULL 桶), SUM)` 只取未分配桶 → 报假差异。
    #[tokio::test]
    async fn book_qty_sums_all_buckets_no_false_drift() {
        let Some(_guard) = setup().await else { return };
        // 3 条在库序列号（基线），成本账拆成两桶：默认库位桶 2 + 未分配桶 1（Σ = 3）
        set_buckets(M_SN, W_A, &[(Some(IL_A), "2"), (None, "1")]).await;

        let report = audit_prefix(true).await;
        assert_eq!(
            report.mismatched, 0,
            "两桶并存、Σqty == 在库件数 → 不该报漂移：{:?}",
            report.items
        );
        assert_eq!(report.checked, 2);
        assert_eq!(report.fixed, 0);

        // dry_run=false 也不该动任何账（没有不一致）
        let strict = audit_prefix(false).await;
        assert_eq!(strict.fixed, 0, "一致时不许动账");
        assert_eq!(
            buckets(M_SN, W_A).await,
            vec![(None, Decimal::from(1)), (Some(uuid(IL_A)), Decimal::from(2))],
            "两个桶都必须原样保留"
        );
    }

    /// ⑦-ter **多桶 + 真差异** → 标签 `MULTI_BUCKET` 且 `dry_run=false` 拒绝自动修（一行都不改）。
    #[tokio::test]
    async fn multi_bucket_mismatch_is_never_auto_fixed() {
        let Some(_guard) = setup().await else { return };
        set_buckets(M_SN, W_A, &[(Some(IL_A), "2"), (None, "1")]).await;
        // 再加一条在库序列号（同仓同库位）→ actual = 4、book = Σ桶 = 3、diff = -1
        db().execute_with_params(
            "INSERT INTO biz_material_serial \
                 (id, serial_no, material_id, warehouse_id, location_id, status, inbound_at) \
             VALUES ('b2c20005-0000-4000-8000-000000000005', 'W2C2-SN-0005', ?::uuid, ?::uuid, \
                     ?::uuid, 'IN_STOCK', now())",
            vec![
                Value::from(&uuid(M_SN)),
                Value::from(&uuid(W_A)),
                Value::from(&uuid(IL_A)),
            ],
        )
        .await
        .expect("再插一条在库序列号");

        let report = audit_prefix(true).await;
        assert_eq!(report.mismatched, 1, "{:?}", report.items);
        let item = &report.items[0];
        assert_eq!(item.material_code, "W2C2-M-SN");
        assert_eq!(item.bucket_count, 2, "成本账里有两个桶");
        assert_eq!(item.book_qty, Decimal::from(3), "账面 = 2 + 1");
        assert_eq!(item.actual_qty, Decimal::from(4));
        assert_eq!(item.diff, Decimal::from(-1));
        assert!(!item.fixable, "多桶必须拒绝自动修");
        assert_eq!(
            item.unfixable_reason.as_deref(),
            Some(UNFIXABLE_MULTI_BUCKET),
            "原因要能一眼看出是「多桶」"
        );
        assert_eq!(report.unfixable, 1);

        // dry_run=false：一条都不许动（差额该记哪个桶本身有歧义）
        let strict = audit_prefix(false).await;
        assert_eq!(strict.mismatched, 1);
        assert_eq!(strict.fixed, 0, "多桶不许被「修」");
        assert_eq!(
            buckets(M_SN, W_A).await,
            vec![(None, Decimal::from(1)), (Some(uuid(IL_A)), Decimal::from(2))],
            "两个桶都必须原样保留（旧实现会把未分配桶覆盖成 4）"
        );

        // 修完再巡检：仍然报同一条，不许静默吞掉
        let again = audit_prefix(true).await;
        assert_eq!(again.mismatched, 1);
        assert_eq!(
            again.items[0].unfixable_reason.as_deref(),
            Some(UNFIXABLE_MULTI_BUCKET)
        );
    }

    /// ⑦-quater **单桶差额语义**：值被推平到 `actual`，金额仍是 `qty × avg_cost`（不覆盖、不丢列）。
    #[tokio::test]
    async fn single_bucket_fix_is_delta_not_overwrite() {
        let Some(_guard) = setup().await else { return };
        // 单桶（未分配），账面 9 / 均价 2.5，实际在库 3 → 差额 -6
        db().execute_with_params(
            "UPDATE biz_inventory SET qty = 9, avg_cost = 2.5, amount = 22.5 \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![Value::from(&uuid(M_SN)), Value::from(&uuid(W_A))],
        )
        .await
        .expect("造单桶脏账");

        let report = audit_prefix(false).await;
        assert_eq!(report.mismatched, 1, "{:?}", report.items);
        assert_eq!(report.items[0].bucket_count, 1);
        assert_eq!(report.fixed, 1, "单桶必须被修");

        let rows = db()
            .find_with_params(
                "SELECT qty::numeric(18,4)::text AS qty, amount::numeric(18,4)::text AS amount \
                   FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
                vec![Value::from(&uuid(M_SN)), Value::from(&uuid(W_A))],
            )
            .await
            .expect("读修后账面");
        assert_eq!(rows.len(), 1, "单桶修复不许凭空多出第二个桶");
        assert_eq!(rows[0].get_typed::<String>("qty").unwrap(), "3.0000");
        assert_eq!(
            rows[0].get_typed::<String>("amount").unwrap(),
            "7.5000",
            "金额 = 新 qty × 均价（3 × 2.5）"
        );
        assert_eq!(audit_prefix(true).await.mismatched, 0, "修完必须全绿");
    }

    /// 单桶修复之外的 `move_serials`：3 个序列号从 A 仓移到 B 仓 → 3 条 `warehouse_id` 变更。
    #[tokio::test]
    async fn move_serials_moves_three_rows() {
        let Some(_guard) = setup().await else { return };
        let ids: Vec<Uuid> = SN_IDS.iter().map(|id| uuid(id)).collect();
        let moved = serial_model_move(&ids, W_B, None).await;
        assert_eq!(moved, 3, "3 条都该被改");
        for id in SN_IDS {
            let (warehouse, location, status) = serial_row(id).await;
            assert_eq!(warehouse, Some(uuid(W_B)), "{id} 应该到 B 仓");
            assert_eq!(location, None, "没传库位 → location_id 置空");
            assert_eq!(status, "IN_STOCK", "不动 status");
        }
        // 带库位移一次：`location_id` 收的是 `biz_inventory_location.id`（IL_A），
        // 而 IL_A 的上游库位字典才是 `biz_location.id`（LOC_A）—— 两跳关系钉在这里。
        let moved_again = serial_model_move(&ids, W_A, Some(IL_A)).await;
        assert_eq!(moved_again, 3);
        for id in SN_IDS {
            let (warehouse, location, _) = serial_row(id).await;
            assert_eq!(warehouse, Some(uuid(W_A)));
            assert_eq!(
                location,
                Some(uuid(IL_A)),
                "location_id = biz_inventory_location.id"
            );
        }
        let upstream: Option<Uuid> = db()
            .scalar_with_params(
                "SELECT location_id FROM biz_inventory_location WHERE id = ?::uuid",
                vec![Value::from(&uuid(IL_A))],
            )
            .await
            .expect("读库位库存行");
        assert_eq!(upstream, Some(uuid(LOC_A)), "IL_A 的上游是 LOC_A（两跳）");
    }

    /// ⑧ 含不存在的 id → 跳过，返回实际改的数量（不报错）。
    #[tokio::test]
    async fn move_serials_skips_missing_ids() {
        let Some(_guard) = setup().await else { return };
        let ghost = uuid("b2c2ffff-0000-4000-8000-00000000dead");
        let mut ids: Vec<Uuid> = SN_IDS.iter().map(|id| uuid(id)).collect();
        ids.push(ghost);
        let moved = serial_model_move(&ids, W_B, Some(IL_A)).await;
        assert_eq!(moved, 3, "不存在的 id 被跳过");
        // 软删的序列号同样跳过
        db().execute_with_params(
            "UPDATE biz_material_serial SET deleted_at = now() WHERE id = ?::uuid",
            vec![Value::from(&uuid(SN_IDS[0]))],
        )
        .await
        .expect("软删一条");
        let again = serial_model_move(&ids, W_A, None).await;
        assert_eq!(again, 2, "软删的那条不该被改");
        // 全都不存在 → 0，且不报错
        let none = serial_model_move(&[ghost], W_A, None).await;
        assert_eq!(none, 0);
        let empty = serial_model_move(&[], W_A, None).await;
        assert_eq!(empty, 0, "空数组直接返回 0");
    }

    /// ⑨ `move_serials` 不动 `status`（在库件移仓后仍是 `IN_STOCK`，也不会多出流水）。
    #[tokio::test]
    async fn move_serials_keeps_status() {
        let Some(_guard) = setup().await else { return };
        let before_txn: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_inventory_txn WHERE material_id = ?::uuid",
                vec![Value::from(&uuid(M_SN))],
            )
            .await
            .expect("统计流水");
        let ids: Vec<Uuid> = SN_IDS.iter().map(|id| uuid(id)).collect();
        serial_model_move(&ids, W_B, Some(IL_A)).await;
        for id in SN_IDS {
            let (_, _, status) = serial_row(id).await;
            assert_eq!(status, "IN_STOCK", "{id} 状态不许变");
        }
        let after_txn: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_inventory_txn WHERE material_id = ?::uuid",
                vec![Value::from(&uuid(M_SN))],
            )
            .await
            .expect("统计流水");
        assert_eq!(before_txn, after_txn, "移仓不是收发，不写库存流水");
        // 移仓后源仓账面没跟着变（重算是调用方 W3b 的事）→ 巡检此时应报不一致
        let report = audit_prefix(true).await;
        assert!(
            report
                .items
                .iter()
                .any(|i| i.material_code == "W2C2-M-SN" && i.warehouse_id.as_deref() == Some(W_B)),
            "移过去但没重算 → 目标仓必须能被巡检发现：{:?}",
            report.items
        );
    }

    /// 差集：`(物料, 仓库)` 组合里，非 `serial_required` 物料绝不出现在巡检结果里。
    #[tokio::test]
    async fn audit_ignores_quantity_managed_materials() {
        let Some(_guard) = setup().await else { return };
        // 把按数量物料的账面改成 999 —— 它不是一物一码，巡检不该管
        db().execute_with_params(
            "UPDATE biz_inventory SET qty = 999 WHERE material_id = ?::uuid",
            vec![Value::from(&uuid(M_PLAIN))],
        )
        .await
        .expect("改按数量物料账面");
        let report = audit_prefix(true).await;
        assert!(
            report
                .items
                .iter()
                .all(|i| i.material_code != "W2C2-M-PLAIN"),
            "按数量物料不该进巡检：{:?}",
            report.items
        );
        assert_eq!(report.checked, 2, "只数一物一码的两个组合");
    }

    /// 便利包装：直接调 [`super::super::serial_model::move_serials`]（`&Db` 即 `impl SqlExecutor`）。
    async fn serial_model_move(ids: &[Uuid], to_warehouse: &str, to_location: Option<&str>) -> u64 {
        super::super::serial_model::move_serials(
            db(),
            ids,
            uuid(to_warehouse),
            to_location.map(uuid),
            uuid("b2c20007-0000-4000-8000-000000000001"),
        )
        .await
        .expect("move_serials 执行失败")
    }
}
