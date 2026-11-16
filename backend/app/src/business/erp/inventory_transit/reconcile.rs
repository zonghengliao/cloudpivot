// ============================================================================
// reconcile.rs —— 在途库存对账（M2-05）：在途 + 已出 = 发货总量
//
// 卡：t_b545b37a（M2-05【M2·仓库】在途库存：对账（在途 + 已出 = 发货总量））
// 拆卡依据 `docs/planning/20261113-新卡清单.md` §M2 的 M2-05 行。
//
// # 对账式（唯一口径，三侧比对）
//
// ```text
// 发货总量 item_qty        = Σ biz_shipment_item.qty          （发货单明细，SSOT）
// 已出     received_qty    = Σ biz_inventory_in_transit.received_qty （status <> 'CANCELLED'）
// 在途     qty_in_transit  = Σ biz_inventory_in_transit.qty_in_transit（status <> 'CANCELLED'）
//
// ① 行恒等（库层保证）：shipped_qty = received_qty + qty_in_transit   ← V0420 生成列
// ② 对账式（本文件）：  item_qty    = received_qty + qty_in_transit
//    ⇒ 等价于 item_qty = shipped_qty（在途账记的发货量 = 发货单发的量）
// ```
//
// ① 是生成列的数学恒等，**正常路径不可能破**；它还留在报告里（`IDENTITY_BROKEN`）
// 是因为「绕过应用的写入」（手工 UPDATE / 历史迁移刷数）真能把它弄破，
// 而那种坏账看起来和「部分签收」一模一样 —— 不单独报出来就永远发现不了。
//
// # 为什么「发货总量」取明细（`biz_shipment_item`）而不是单头 `total_qty`
//
// ```
// 单头 total_qty 是**明细的汇总字段**（V0283：NUMERIC(18,4) NOT NULL DEFAULT 0，
// 建单/改单时由 model 层算好写入）。拿汇总字段当 SSOT 有两个问题：
//   ① 它自己也可能被写坏（那就是「用一个未校验的数去校验另一个数」）；
//   ② 对账要定位到「哪一行物料错了」，单头粒度给不出这个答案。
// 所以三侧比对一律在 (发货单 × 物料) 粒度，单头只用来取单号 / 状态 / 发货日期。
// ```
//
// # 不变量检查的四个方向（少一个方向就等于少一半）
//
// | issue 码 | 含义 | 怎么产生 |
// |---|---|---|
// | `QTY_MISMATCH` | 两侧都有行，但发货量不等 | 过账时漏写 / 明细分被改 / 在途量被手工改 |
// | `MISSING_TRANSIT` | 有明细、**没有**在途行 | M2-02 的在途写入被绕过；或 M2-02 之前过账的历史单 |
// | `ORPHAN_TRANSIT` | 有在途行、明细里没有这个物料 | 明细被删 / 物料挂错 |
// | `IDENTITY_BROKEN` | 行级 shipped ≠ received + in_transit | 绕过生成列的写入（手工改库 / 刷数） |
// | `MULTI_WAREHOUSE` | 同一 (发货单, 物料) 在多仓有在途行 | 唯一键含 warehouse_id，多仓 = 同一批货被记了两份 |
// | `CANCELLED_ONLY` | 只有在途作废行、没有有效行 | 整批作废（**不是错**：作废行不算在途也不算已出） |
//
// 前五个是**硬问题**（`mismatched`，`balanced=false`）；`CANCELLED_ONLY` 只标注、不计入不平
// —— 它表达的是「这批货作废了」，量上本来就该对不上（V0420 的口径：作废行不参与在途）。
//
// # `MISSING_TRANSIT` 会报历史单：这是**诚实**，不是误报
//
// M2-02 之前发货过账 = 立即出库，根本没有在途账这一层（V0420 才建表）。那些历史单在
// 本报告里必然表现为「有明细、无在途行」。这不是本文件能修的事（也不该修：补一行假的
// 在途账等于伪造历史），正确用法是按 `ship_date_from/ship_date_to` 收到「在途账上线之后」
// 的区间再对账 —— 见 `docs/ERP-库存对账口径.md` §8。
//
// # 只读
//
// 本文件**一行写 SQL 都没有**：对账是「把三侧数摆在一起给结论」，修法是业务动作
// （重新过账 / 补签收 / 改单据），不是巡检能替业务决定的事。这与 `inventory_audit`
// （一物一码巡检，`dry_run=false` 会改库）刻意不同：那边有唯一正确的参照物
// （序列号件数），这边没有 —— 三侧都「有理」，谁是真相只有业务知道。
//
// # 内存与截断
//
// 一条 `WITH … SELECT` 取回合并后的全量组合（每组一行），Rust 侧判定 + 汇总结论。
// 组合数上限 [`RECONCILE_SCAN_LIMIT`]（多取一行判 `truncated`）：对账是**全量结论**类接口，
// 行级 `LIMIT + 翻页` 会让「合计」变成半截账（比截断更危险）。
// 超限时 `truncated=true` + 文案要求按发货日期区间收窄，绝不静默给半份结论。
// ============================================================================

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::shipment::shipment_transit;

/// 一次对账最多比对的 `(发货单 × 物料)` 组数（多取一行判 `truncated`）。
pub const RECONCILE_SCAN_LIMIT: usize = 2000;

// ==================== issue 码（报告 / 前端文案 / 巡检脚本共用） ====================

/// 两侧都有行，但发货量不等。
pub const ISSUE_QTY_MISMATCH: &str = "QTY_MISMATCH";
/// 有明细、没有在途行。
pub const ISSUE_MISSING_TRANSIT: &str = "MISSING_TRANSIT";
/// 有在途行、明细里没有这个物料。
pub const ISSUE_ORPHAN_TRANSIT: &str = "ORPHAN_TRANSIT";
/// 行级 `shipped_qty <> received_qty + qty_in_transit`（生成列被绕过）。
pub const ISSUE_IDENTITY_BROKEN: &str = "IDENTITY_BROKEN";
/// 同一 `(发货单, 物料)` 在多仓有在途行。
pub const ISSUE_MULTI_WAREHOUSE: &str = "MULTI_WAREHOUSE";
/// 只有在途作废行、没有有效行（整批作废，**不是错**）。
pub const ISSUE_CANCELLED_ONLY: &str = "CANCELLED_ONLY";

/// 硬问题（计入 `mismatched` / 让 `balanced = false`）。
pub const HARD_ISSUES: [&str; 5] = [
    ISSUE_QTY_MISMATCH,
    ISSUE_MISSING_TRANSIT,
    ISSUE_ORPHAN_TRANSIT,
    ISSUE_IDENTITY_BROKEN,
    ISSUE_MULTI_WAREHOUSE,
];

/// issue 码的中文说明（报告 / 日志 / 前端 tooltip 共用一份，避免三处各写一遍）。
pub fn issue_label(issue: &str) -> &'static str {
    match issue {
        ISSUE_QTY_MISMATCH => "发货单明细的发货量与在途账记的发货量不符",
        ISSUE_MISSING_TRANSIT => "发货单已过账但没有在途账记录（货发了没进在途）",
        ISSUE_ORPHAN_TRANSIT => "在途账有这批货，但发货单明细里没有对应物料",
        ISSUE_IDENTITY_BROKEN => "在途行自身不满足「发货 = 已签收 + 在途」（生成列被绕过）",
        ISSUE_MULTI_WAREHOUSE => "同一发货单同一物料在多仓有在途行（同一批货被记了两份）",
        ISSUE_CANCELLED_ONLY => "整批作废：只有在途作废行，不计在途也不计已出",
        _ => "未知差异",
    }
}

/// 是否硬问题（`CANCELLED_ONLY` 不是）。
pub fn is_hard_issue(issue: &str) -> bool {
    HARD_ISSUES.contains(&issue)
}

// ==================== 取数条件 ====================

/// 对账筛选（由 handler 从 [`crate::business::erp::inventory_transit::dto::TransitReconcileFilter`] 解析）。
#[derive(Debug, Clone, Default)]
pub struct ReconcileQuery {
    /// 发货单。
    pub shipment_id: Option<Uuid>,
    /// 物料。
    pub material_id: Option<Uuid>,
    /// 发货仓库。
    pub warehouse_id: Option<Uuid>,
    /// 发货日期下界（含）。
    pub ship_date_from: Option<NaiveDate>,
    /// 发货日期上界（含）。
    pub ship_date_to: Option<NaiveDate>,
}

/// 发货单侧（明细）的 WHERE：**恒带 `s.status = 'POSTED'`**。
///
/// 为什么必须带：`DRAFT` / `PENDING_POST` 的发货单有明细但**本来就不该有在途行**
/// （发货过账才进在途）。不带这个条件，每张草稿单都会被报成 `MISSING_TRANSIT` ——
/// 满屏假红会让人直接不看待这个报告。
fn items_where(query: &ReconcileQuery) -> (String, Vec<Value>) {
    let mut sql = String::from(" WHERE s.status = ?");
    let mut params: Vec<Value> = vec![Value::from(
        crate::business::erp::shipment::shipment_model::STATUS_POSTED,
    )];
    if let Some(shipment_id) = query.shipment_id {
        sql.push_str(" AND s.id = ?::uuid");
        params.push(Value::from(&shipment_id));
    }
    if let Some(material_id) = query.material_id {
        sql.push_str(" AND si.material_id = ?::uuid");
        params.push(Value::from(&material_id));
    }
    if let Some(warehouse_id) = query.warehouse_id {
        sql.push_str(" AND s.warehouse_id = ?::uuid");
        params.push(Value::from(&warehouse_id));
    }
    if let Some(from) = query.ship_date_from {
        sql.push_str(" AND s.ship_date >= ?::text::date");
        params.push(Value::Text(from.to_string()));
    }
    if let Some(to) = query.ship_date_to {
        sql.push_str(" AND s.ship_date <= ?::text::date");
        params.push(Value::Text(to.to_string()));
    }
    (sql, params)
}

/// 在途账侧的 WHERE（**不筛状态**：作废行也要取出来，否则「整批作废」会被误报成缺行）。
///
/// 与明细侧用**同一组筛选值**（物料 / 仓库 / 发货单 / 发货日期区间），
/// 只是挂在 `t.*` 上 —— 两侧条件不对齐时 `FULL OUTER JOIN` 会造出大量假差异
/// （一侧筛掉、另一侧还在 = 凭空多出一半的行）。
fn transit_where(query: &ReconcileQuery) -> (String, Vec<Value>) {
    let mut sql = String::from(" WHERE TRUE");
    let mut params: Vec<Value> = Vec::new();
    if let Some(shipment_id) = query.shipment_id {
        sql.push_str(" AND t.shipment_id = ?::uuid");
        params.push(Value::from(&shipment_id));
    }
    if let Some(material_id) = query.material_id {
        sql.push_str(" AND t.material_id = ?::uuid");
        params.push(Value::from(&material_id));
    }
    if let Some(warehouse_id) = query.warehouse_id {
        sql.push_str(" AND t.warehouse_id = ?::uuid");
        params.push(Value::from(&warehouse_id));
    }
    if let Some(from) = query.ship_date_from {
        sql.push_str(" AND t.ship_date >= ?::text::date");
        params.push(Value::Text(from.to_string()));
    }
    if let Some(to) = query.ship_date_to {
        sql.push_str(" AND t.ship_date <= ?::text::date");
        params.push(Value::Text(to.to_string()));
    }
    (sql, params)
}

/// 对账 SQL：一条 `WITH … SELECT`，返回 `(发货单 × 物料)` 合并后的全量组合。
///
/// ```text
/// items   发货单侧：POSTED 单的明细按 (发货单, 物料) 汇总
/// transit 在途侧  ：在途行按 (发货单, 物料) 汇总（有效行 / 作废行分开算）
/// joined  FULL OUTER JOIN：两个方向都不丢行（「没有在途行」和「没有明细」都要能报）
/// ```
///
/// `FULL OUTER JOIN` 而不是 `LEFT JOIN`：后者只能发现「有明细没在途」，
/// 「有在途没明细」会被静默吞掉 —— 而那正是「同一批货记了两份 / 物料挂错」的形态。
pub fn reconcile_sql(query: &ReconcileQuery) -> (String, Vec<Value>) {
    let (items_filter, mut params) = items_where(query);
    let (transit_filter, transit_params) = transit_where(query);
    params.extend(transit_params);

    let sql = format!(
        "WITH items AS ( \
             SELECT si.shipment_id, si.material_id, SUM(si.qty)::text AS item_qty \
               FROM biz_shipment_item si \
               JOIN biz_shipment s ON s.id = si.shipment_id \
              {items_filter} \
              GROUP BY si.shipment_id, si.material_id \
         ), transit AS ( \
             SELECT t.shipment_id, t.material_id, \
                    COALESCE(SUM(t.shipped_qty)  FILTER (WHERE t.status <> '{cancelled}'), 0)::text AS shipped_qty, \
                    COALESCE(SUM(t.received_qty) FILTER (WHERE t.status <> '{cancelled}'), 0)::text AS received_qty, \
                    COALESCE(SUM(t.qty_in_transit) FILTER (WHERE t.status <> '{cancelled}'), 0)::text AS qty_in_transit, \
                    COALESCE(SUM(t.shipped_qty)  FILTER (WHERE t.status =  '{cancelled}'), 0)::text AS cancelled_qty, \
                    count(*) AS transit_lines, \
                    count(*) FILTER (WHERE t.status <> '{cancelled}') AS active_lines, \
                    count(*) FILTER (WHERE t.status =  '{cancelled}') AS cancelled_lines, \
                    count(DISTINCT t.warehouse_id) AS warehouses, \
                    min(t.warehouse_id::text)::uuid AS warehouse_id, \
                    bool_or(t.shipped_qty <> t.received_qty + t.qty_in_transit) AS identity_broken \
               FROM {table} t \
              {transit_filter} \
              GROUP BY t.shipment_id, t.material_id \
         ), joined AS ( \
             SELECT COALESCE(a.shipment_id, b.shipment_id) AS shipment_id, \
                    COALESCE(a.material_id, b.material_id) AS material_id, \
                    COALESCE(a.item_qty, '0')            AS item_qty, \
                    COALESCE(b.shipped_qty, '0')         AS shipped_qty, \
                    COALESCE(b.received_qty, '0')        AS received_qty, \
                    COALESCE(b.qty_in_transit, '0')      AS qty_in_transit, \
                    COALESCE(b.cancelled_qty, '0')       AS cancelled_qty, \
                    b.transit_lines, b.active_lines, b.cancelled_lines, b.warehouses, \
                    b.warehouse_id, COALESCE(b.identity_broken, FALSE) AS identity_broken \
               FROM items a \
               FULL OUTER JOIN transit b \
                 ON a.shipment_id = b.shipment_id AND a.material_id = b.material_id \
         ) \
         SELECT j.shipment_id, j.material_id, j.item_qty, j.shipped_qty, j.received_qty, \
                j.qty_in_transit, j.cancelled_qty, \
                COALESCE(j.transit_lines, 0) AS transit_lines, \
                COALESCE(j.active_lines, 0)  AS active_lines, \
                COALESCE(j.cancelled_lines, 0) AS cancelled_lines, \
                COALESCE(j.warehouses, 0) AS warehouses, j.warehouse_id, j.identity_broken, \
                s.ship_no, s.ship_date, s.status AS shipment_status, \
                (s.deleted_at IS NOT NULL) AS shipment_deleted \
           FROM joined j \
           LEFT JOIN biz_shipment s ON s.id = j.shipment_id \
          ORDER BY s.ship_date ASC NULLS LAST, j.shipment_id ASC, j.material_id ASC \
          LIMIT {limit}",
        cancelled = shipment_transit::STATUS_CANCELLED,
        table = shipment_transit::TABLE,
        limit = RECONCILE_SCAN_LIMIT + 1,
    );
    (sql, params)
}

/// 一组 `(发货单 × 物料)` 的三侧数量 + 诊断位（从库里原样取出，**不在 SQL 里下结论**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboRow {
    /// 发货单。
    pub shipment_id: Uuid,
    /// 物料。
    pub material_id: Uuid,
    /// 发货单号（表头理论上必有：在途行是 CASCADE 外键；`None` 只作防御）。
    pub ship_no: Option<String>,
    /// 发货日期（单头）。
    pub ship_date: Option<NaiveDate>,
    /// 发货单状态。
    pub shipment_status: Option<String>,
    /// 发货单是否已软删（明细与在途行都还在 —— 对账必须看得见）。
    pub shipment_deleted: bool,
    /// 在途行上的仓库（`None` = 这组没有在途行）。
    pub warehouse_id: Option<Uuid>,
    /// 发货总量（`Σ biz_shipment_item.qty`；`0` = 明细里没有这个物料）。
    pub item_qty: Decimal,
    /// 在途账记的发货量（非作废行 `Σ shipped_qty`）。
    pub shipped_qty: Decimal,
    /// 已出（非作废行 `Σ received_qty`）。
    pub received_qty: Decimal,
    /// 在途（非作废行 `Σ qty_in_transit`）。
    pub qty_in_transit: Decimal,
    /// 作废行的发货量（不参与对账式，只做诊断）。
    pub cancelled_qty: Decimal,
    /// 在途行总数（含作废）。
    pub transit_lines: i64,
    /// 有效在途行数（`status <> 'CANCELLED'`）。
    pub active_lines: i64,
    /// 作废在途行数。
    pub cancelled_lines: i64,
    /// 这组的在途行涉及多少个仓库（> 1 = 同一批货被记了两份）。
    pub warehouses: i64,
    /// 行级 `shipped <> received + in_transit` 是否出现过。
    pub identity_broken: bool,
}

impl ComboRow {
    /// 差额 = 发货总量 − （已出 + 在途）。`0` = 这组对平。
    pub fn diff(&self) -> Decimal {
        self.item_qty - (self.received_qty + self.qty_in_transit)
    }

    /// 是否已纳入在途账（有在途行）。没纳入的组（历史单 / 过账被绕过）只统计发货量。
    pub fn has_transit(&self) -> bool {
        self.transit_lines > 0
    }

    /// 差异判定（**唯一**口径，纯函数，单测直接钉）。
    ///
    /// 顺序有意义：`CANCELLED_ONLY` 必须先判（整批作废时 `item_qty ≠ shipped_qty` 是
    /// 正常的，先判 `QTY_MISMATCH` 会把「作废」误报成「算错」）；`MISSING/ORPHAN`
    /// 是「某一侧整块没有」，比数量不等更具体，也先判。
    pub fn issues(&self) -> Vec<&'static str> {
        let mut issues: Vec<&'static str> = Vec::new();
        let has_items = self.item_qty != Decimal::ZERO;
        let has_active = self.active_lines > 0;

        if has_items && !has_active && self.cancelled_lines > 0 {
            issues.push(ISSUE_CANCELLED_ONLY);
        } else if has_items && !self.has_transit() {
            issues.push(ISSUE_MISSING_TRANSIT);
        } else if !has_items && self.has_transit() {
            issues.push(ISSUE_ORPHAN_TRANSIT);
        } else if has_items && has_active && self.shipped_qty != self.item_qty {
            issues.push(ISSUE_QTY_MISMATCH);
        }

        if self.identity_broken {
            issues.push(ISSUE_IDENTITY_BROKEN);
        }
        if self.warehouses > 1 {
            issues.push(ISSUE_MULTI_WAREHOUSE);
        }
        issues
    }

    /// 这一组是否「对平」（没有任何 issue；`CANCELLED_ONLY` 也算一格标注，不算对平）。
    pub fn is_clean(&self) -> bool {
        self.issues().is_empty()
    }

    /// 是否有硬问题（计入 `mismatched`）。
    pub fn has_hard_issue(&self) -> bool {
        self.issues().iter().any(|issue| is_hard_issue(issue))
    }
}

/// 对账汇总（**口径**：只在「已纳入在途账」的组上算对账式，另单列未纳入的发货量）。
///
/// 为什么不把未纳入的组并进 `diff`：它们「还没有在途账」（M2-02 之前的历史单），
/// 并进去会让 `diff` 永远非零、把真差异埋掉。它们单独用 `untracked_*` 报出来，
/// 既不参与对账式、也不静默消失。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileTotals {
    /// 比对的组数（= 取回的组合数，受扫描上限影响）。
    pub checked: u64,
    /// 对平的组数。
    pub matched: u64,
    /// 有硬问题的组数。
    pub mismatched: u64,
    /// 有 `CANCELLED_ONLY` 标注的组数（整批作废，不算不平）。
    pub cancelled: u64,
    /// 未纳入在途账的组数（有明细、没有在途行，发的是 `POSTED` 单）。
    pub untracked: u64,
    /// 已纳入在途账的发货量合计（发货单明细的 SSOT）。
    pub item_qty: Decimal,
    /// 在途账记的发货量合计（非作废）。
    pub shipped_qty: Decimal,
    /// 已出合计（已签收）。
    pub received_qty: Decimal,
    /// 在途合计。
    pub in_transit_qty: Decimal,
    /// 作废行发货量合计（不参与对账式）。
    pub cancelled_qty: Decimal,
    /// 未纳入在途账的发货量合计。
    pub untracked_item_qty: Decimal,
}

impl ReconcileTotals {
    /// 对账差额 = 已纳入发货量 − （已出 + 在途）。`0` = 整体恒等成立。
    pub fn diff(&self) -> Decimal {
        self.item_qty - (self.received_qty + self.in_transit_qty)
    }
}

/// 从组合行算出汇总（纯函数：DB 用例与 HTTP 共用同一份口径）。
///
/// 对账式的范围 = **有有效在途行**（`active_lines > 0`）的组：
///
/// * 整批作废的组（`CANCELLED_ONLY`）不算 —— 作废后「在途 + 已出」本来就不该等于发货量；
/// * 没有在途行的组（`untracked`）不算 —— 那些货没进过在途账（M2-02 之前的历史单），
///   并进 `diff` 会让真差异永远埋在一个非零数里。
///
/// 两类都单独报（`cancelled` / `untracked` + `untracked_item_qty`），不静默消失。
pub fn summarize(combos: &[ComboRow]) -> ReconcileTotals {
    let mut totals = ReconcileTotals::default();
    for combo in combos {
        totals.checked += 1;
        let issues = combo.issues();
        if issues.is_empty() {
            totals.matched += 1;
        }
        if issues.iter().any(|issue| is_hard_issue(issue)) {
            totals.mismatched += 1;
        }
        if issues.contains(&ISSUE_CANCELLED_ONLY) {
            totals.cancelled += 1;
        }
        totals.cancelled_qty += combo.cancelled_qty;
        if combo.active_lines > 0 {
            totals.item_qty += combo.item_qty;
            totals.shipped_qty += combo.shipped_qty;
            totals.received_qty += combo.received_qty;
            totals.in_transit_qty += combo.qty_in_transit;
        } else if combo.has_transit() {
            // 只有在途作废行：量上不进对账式（V0420 口径：作废行既不算在途也不算已出）
            continue;
        } else {
            totals.untracked += 1;
            totals.untracked_item_qty += combo.item_qty;
        }
    }
    totals
}

/// 扫描是否被上限截断（多取了一行）。
pub fn is_truncated(fetched: usize) -> bool {
    fetched > RECONCILE_SCAN_LIMIT
}

/// 读一次对账组合（只读，不开事务）。返回 `(combos, truncated)`。
///
/// # 异常情况
///
/// 查库 / 行解析失败 → [`DbError`]（handler 直接 `?` 转 500：这类失败是数据访问问题，
/// 不是「对不平」—— 对不平要在 200 的报告里说，不能伪装成错误码）。
pub async fn scan(query: &ReconcileQuery) -> DbResult<(Vec<ComboRow>, bool)> {
    let (sql, params) = reconcile_sql(query);
    let rows = db().find_with_params(&sql, params).await?;
    let truncated = is_truncated(rows.len());
    let mut out = Vec::with_capacity(rows.len().min(RECONCILE_SCAN_LIMIT));
    for row in rows.iter().take(RECONCILE_SCAN_LIMIT) {
        out.push(ComboRow {
            shipment_id: row.get_typed("shipment_id")?,
            material_id: row.get_typed("material_id")?,
            ship_no: row.get_typed("ship_no")?,
            ship_date: row.get_typed("ship_date")?,
            shipment_status: row.get_typed("shipment_status")?,
            shipment_deleted: row.get_typed("shipment_deleted")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            item_qty: parse_decimal(row, "item_qty")?,
            shipped_qty: parse_decimal(row, "shipped_qty")?,
            received_qty: parse_decimal(row, "received_qty")?,
            qty_in_transit: parse_decimal(row, "qty_in_transit")?,
            cancelled_qty: parse_decimal(row, "cancelled_qty")?,
            transit_lines: row.get_typed("transit_lines")?,
            active_lines: row.get_typed("active_lines")?,
            cancelled_lines: row.get_typed("cancelled_lines")?,
            warehouses: row.get_typed("warehouses")?,
            identity_broken: row.get_typed("identity_broken")?,
        });
    }
    Ok((out, truncated))
}

/// `numeric` 一律 `::text` 出来再解析成定点数（与 `model::parse_decimal` 同款：
/// 驱动不做 `numeric → Decimal` 的直接映射，且 `::text` 顺带避开浮点丢精度）。
fn parse_decimal(row: &Row, column: &str) -> DbResult<Decimal> {
    let text = row.get_typed::<String>(column)?;
    text.trim()
        .parse::<Decimal>()
        .map_err(|error| DbError::InvalidValue(format!("对账 {column} 不是合法数值：{text}（{error}）")))
}

// ==================== 单测（纯逻辑，不碰库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量非法")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期")
    }

    /// 造一组组合行：默认「有明细 100 / 在途账 100 / 已出 40 / 在途 60」= 对平。
    fn combo() -> ComboRow {
        ComboRow {
            shipment_id: Uuid::from_u128(0x1000),
            material_id: Uuid::from_u128(0x2000),
            ship_no: Some("M205-SH-000001".to_string()),
            ship_date: Some(date("2026-11-10")),
            shipment_status: Some("POSTED".to_string()),
            shipment_deleted: false,
            warehouse_id: Some(Uuid::from_u128(0x3000)),
            item_qty: dec("100"),
            shipped_qty: dec("100"),
            received_qty: dec("40"),
            qty_in_transit: dec("60"),
            cancelled_qty: Decimal::ZERO,
            transit_lines: 1,
            active_lines: 1,
            cancelled_lines: 0,
            warehouses: 1,
            identity_broken: false,
        }
    }

    /// 卡面恒等式：发货总量 = 已出 + 在途 ⇒ 没有任何 issue、差额 0。
    #[test]
    fn balanced_combo_has_no_issue() {
        let row = combo();
        assert!(row.issues().is_empty(), "issues={:?}", row.issues());
        assert_eq!(row.diff(), Decimal::ZERO);
        assert!(row.is_clean());
        assert!(!row.has_hard_issue());
        println!("[M2-05] 发货 100 = 已出 40 + 在途 60 → 零差异 ✓");
    }

    /// 部分签收不影响恒等（已出 40 + 在途 60 仍然 = 100）。
    #[test]
    fn partial_receipt_stays_balanced() {
        for (received, open) in [("0", "100"), ("40", "60"), ("100", "0")] {
            let mut row = combo();
            row.received_qty = dec(received);
            row.qty_in_transit = dec(open);
            assert!(row.is_clean(), "已出 {received} / 在途 {open} 应仍对平");
            assert_eq!(row.diff(), Decimal::ZERO);
        }
        println!("[M2-05] 未签收 / 部分签收 / 全签收三种形态都恒等 ✓");
    }

    /// 四类硬问题各自能被单独报出来（少报一类 = 少查一个方向）。
    #[test]
    fn each_hard_issue_is_detected() {
        // ① 数量不等：明细 100、在途账只记了 90
        let mut qty = combo();
        qty.shipped_qty = dec("90");
        qty.received_qty = dec("30");
        qty.qty_in_transit = dec("60");
        assert_eq!(qty.issues(), vec![ISSUE_QTY_MISMATCH]);
        assert_eq!(qty.diff(), dec("10"), "差额 = 100 − (30 + 60)");
        assert!(qty.has_hard_issue());

        // ② 有明细、没有在途行
        let mut missing = combo();
        missing.shipped_qty = Decimal::ZERO;
        missing.received_qty = Decimal::ZERO;
        missing.qty_in_transit = Decimal::ZERO;
        missing.transit_lines = 0;
        missing.active_lines = 0;
        missing.warehouse_id = None;
        missing.warehouses = 0;
        assert_eq!(missing.issues(), vec![ISSUE_MISSING_TRANSIT]);
        assert!(!missing.has_transit());

        // ③ 有在途行、明细里没有这个物料
        let mut orphan = combo();
        orphan.item_qty = Decimal::ZERO;
        assert_eq!(orphan.issues(), vec![ISSUE_ORPHAN_TRANSIT]);
        assert_eq!(orphan.diff(), dec("-100"), "多出来的在途量差额为负");

        // ④ 行级恒等被破（生成列被绕过）
        let mut broken = combo();
        broken.identity_broken = true;
        assert_eq!(broken.issues(), vec![ISSUE_IDENTITY_BROKEN]);
        assert!(broken.has_hard_issue());

        println!("[M2-05] QTY_MISMATCH / MISSING_TRANSIT / ORPHAN_TRANSIT / IDENTITY_BROKEN 各自命中 ✓");
    }

    /// 多仓：同一 (发货单, 物料) 在两个仓都有在途行 = 同一批货被记两份。
    #[test]
    fn multi_warehouse_is_a_hard_issue() {
        let mut row = combo();
        row.warehouses = 2;
        assert_eq!(row.issues(), vec![ISSUE_MULTI_WAREHOUSE]);
        assert!(row.has_hard_issue());
        println!("[M2-05] 同一发货单同一物料多仓在途 → MULTI_WAREHOUSE（硬问题）✓");
    }

    /// 整批作废：只有在途作废行 ⇒ 标 CANCELLED_ONLY，**不算不平**（作废行不参与在途）。
    #[test]
    fn cancelled_only_is_flagged_but_not_hard() {
        let mut row = combo();
        row.shipped_qty = Decimal::ZERO;
        row.received_qty = Decimal::ZERO;
        row.qty_in_transit = Decimal::ZERO;
        row.cancelled_qty = dec("100");
        row.active_lines = 0;
        row.cancelled_lines = 1;
        assert_eq!(row.issues(), vec![ISSUE_CANCELLED_ONLY]);
        assert!(!row.has_hard_issue(), "作废不是算错，不能计入 mismatched");
        assert!(!row.is_clean(), "但要标注出来（否则作废整批就看不见了）");
        println!("[M2-05] 整批作废 → CANCELLED_ONLY（标注，不计入不平）✓");
    }

    /// 汇总：未纳入在途账的组不进对账式，单列 `untracked_*`；作废量也不进。
    #[test]
    fn totals_split_tracked_untracked_and_cancelled() {
        let mut balanced = combo(); // 100 = 40 + 60
        let mut mismatched = combo();
        mismatched.shipment_id = Uuid::from_u128(0x1001);
        mismatched.shipped_qty = dec("80");
        mismatched.received_qty = dec("30");
        mismatched.qty_in_transit = dec("50");
        mismatched.item_qty = dec("100"); // 差 20

        let mut untracked = combo();
        untracked.shipment_id = Uuid::from_u128(0x1002);
        untracked.shipped_qty = Decimal::ZERO;
        untracked.received_qty = Decimal::ZERO;
        untracked.qty_in_transit = Decimal::ZERO;
        untracked.item_qty = dec("7");
        untracked.transit_lines = 0;
        untracked.active_lines = 0;
        untracked.warehouse_id = None;
        untracked.warehouses = 0;

        let mut cancelled = combo();
        cancelled.shipment_id = Uuid::from_u128(0x1003);
        cancelled.shipped_qty = Decimal::ZERO;
        cancelled.received_qty = Decimal::ZERO;
        cancelled.qty_in_transit = Decimal::ZERO;
        cancelled.cancelled_qty = dec("5");
        cancelled.active_lines = 0;
        cancelled.cancelled_lines = 1;

        balanced.warehouse_id = Some(Uuid::from_u128(0x3001));
        let totals = summarize(&[balanced, mismatched, untracked, cancelled]);

        assert_eq!(totals.checked, 4);
        assert_eq!(totals.mismatched, 2, "数量不等 + 缺在途行；作废组不算");
        assert_eq!(totals.matched, 1);
        assert_eq!(totals.cancelled, 1);
        assert_eq!(totals.untracked, 1);
        // 已纳入的组：100 + 100 = 200；已出 40 + 30 = 70；在途 60 + 50 = 110
        assert_eq!(totals.item_qty, dec("200"));
        assert_eq!(totals.received_qty, dec("70"));
        assert_eq!(totals.in_transit_qty, dec("110"));
        assert_eq!(totals.diff(), dec("20"), "差额就是那组的 20，不掺未纳入的 7");
        assert_eq!(totals.untracked_item_qty, dec("7"));
        assert_eq!(totals.cancelled_qty, dec("5"));
        println!("[M2-05] 汇总：checked 4 / 不平 2 / 未纳入 1(发货 7) / 作废 1；对账式只在纳入组上算 ✓");
    }

    /// 明细侧 WHERE 必须恒带 `s.status = 'POSTED'`（草稿单有明细但本来就没有在途行）。
    #[test]
    fn items_side_always_requires_posted() {
        let (sql, params) = items_where(&ReconcileQuery::default());
        assert!(sql.contains("s.status = ?"), "sql={sql}");
        assert_eq!(params.len(), 1);
        match &params[0] {
            Value::Text(text) => assert_eq!(text, "POSTED"),
            other => panic!("状态参数应为文本 POSTED，实际 {other:?}"),
        }
        println!("[M2-05] 明细侧 WHERE = `{sql}`（恒 POSTED，草稿单不参与对账）✓");
    }

    /// 两侧筛选必须完全对称：同一组条件在两边各出现一次，占位符数量 = 参数数量。
    #[test]
    fn both_sides_use_symmetric_filters() {
        let query = ReconcileQuery {
            shipment_id: Some(Uuid::from_u128(0x01)),
            material_id: Some(Uuid::from_u128(0x02)),
            warehouse_id: Some(Uuid::from_u128(0x03)),
            ship_date_from: Some(date("2026-11-01")),
            ship_date_to: Some(date("2026-11-30")),
        };
        let (items_sql, items_params) = items_where(&query);
        let (transit_sql, transit_params) = transit_where(&query);
        assert_eq!(items_params.len(), 6, "状态 + 5 个条件");
        assert_eq!(transit_params.len(), 5, "在途侧没有状态条件（作废行也要取）");
        assert_eq!(
            items_sql.matches('?').count(),
            items_params.len(),
            "items sql={items_sql}"
        );
        assert_eq!(
            transit_sql.matches('?').count(),
            transit_params.len(),
            "transit sql={transit_sql}"
        );

        let (sql, params) = reconcile_sql(&query);
        assert_eq!(sql.matches('?').count(), params.len(), "合并后占位符必须与参数一一对应");
        assert!(sql.contains("FULL OUTER JOIN"), "两侧都不能被静默丢掉");
        assert!(
            sql.contains(shipment_transit::TABLE),
            "在途表名必须取自 shipment_transit::TABLE"
        );
        assert!(
            sql.contains(shipment_transit::STATUS_CANCELLED),
            "作废行必须被单独聚合（否则整批作废会误报成缺行）"
        );
        println!("[M2-05] 两侧对称筛选（明细 6 个参数 / 在途 5 个）+ FULL OUTER JOIN ✓");
    }

    /// 扫描截断：恰好上限 = 完整；多一行 = 截断（不能把半份账当全量）。
    #[test]
    fn truncation_is_judged_by_one_extra_row() {
        assert!(!is_truncated(0));
        assert!(!is_truncated(RECONCILE_SCAN_LIMIT));
        assert!(is_truncated(RECONCILE_SCAN_LIMIT + 1));
        println!("[M2-05] 截断判据：≤ {RECONCILE_SCAN_LIMIT} 组完整，多取一行即 truncated ✓");
    }

    /// issue 文案必须覆盖全部 6 个码（报告里的中文说明只有这一份）。
    #[test]
    fn issue_labels_cover_all_codes() {
        for issue in HARD_ISSUES.iter().chain(std::iter::once(&ISSUE_CANCELLED_ONLY)) {
            let label = issue_label(issue);
            assert!(
                label.contains('（') || label.contains('：') || label.len() > 6,
                "文案太短：{issue} → {label}"
            );
            assert_ne!(label, "未知差异", "{issue} 没有专属文案");
        }
        assert_eq!(issue_label("NOPE"), "未知差异");
        assert!(is_hard_issue(ISSUE_QTY_MISMATCH));
        assert!(!is_hard_issue(ISSUE_CANCELLED_ONLY));
        println!("[M2-05] 6 个 issue 码都有中文文案；CANCELLED_ONLY 不属硬问题 ✓");
    }
}
