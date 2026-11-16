//! 补货建议（M2-15）：安全库存 + 在途 + 未清订单 → **建议采购量**。
//!
//! 卡：t_527ceaa3（M2-15【M2·仓库】补货建议：安全库存 + 在途 + 未清订单）；
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §M2 的 M2-15 行。上游：M2-13 的水位设置与
//! 生效水位口径（[`crate::business::erp::stock_policy::dto::resolve_min_qty`] /
//! [`resolve_max_qty`]）、M2-04 的在途账（`biz_inventory_in_transit`）。
//!
//! ```text
//! 接口      GET /business/erp/replenish/page      权限码 business:erp:replenish:list（V0430）
//! 数据源    biz_inventory（结存）× biz_material_stock_policy（水位）×
//!           biz_inventory_in_transit（发货在途）× biz_purchase_order(_item)（未清订单）
//! 写入      本卡**零写入**：只读四张表，不给任何单据落状态（建议量给到人，下单是采购的活）
//! ```
//!
//! # 算的是哪一个数（写死，别再解释第二遍）
//!
//! ```text
//! 可用结存   = biz_inventory.qty − 发货在途（status = 'IN_TRANSIT' 的 qty_in_transit 合计）
//! 预计可用   = 可用结存 + 未清采购订单未收量（SUBMITTED / APPROVED / PARTIAL_RECEIVED 且 received_qty < qty）
//! 目标水位   = 策略行 max_qty（设了最高水位就补到最高水位）；否则生效最低水位 min_qty
//! 触发条件   预计可用 < 生效最低水位（**严格小于**，与 M2-14 的判定逐字一致）
//! 建议采购量 = 触发 ? (目标水位 − 预计可用) : 0      （恒 >= 0）
//! ```
//!
//! 这三项**各自回答一句话**，缺哪一项都会算错：
//!
//! | 项 | 来自 | 为什么必须算进来 |
//! |:--|:--|:--|
//! | 安全库存 | `stock_policy::dto::resolve_min_qty`（三级回退的唯一实现） | 它定义「补到多少边才够」；本卡不重写回退，见下「阈值只调一次」 |
//! | 在途 | `biz_inventory_in_transit`（发货过账产生，`status='IN_TRANSIT'`） | 货**已经离开仓库**、账面还没扣（M2-02 的两段过账）。只用账面 qty 会把「已发走但还没签收」的货当成仓里有的，建议量偏小、真断料 |
//! | 未清订单 | `biz_purchase_order` + `_item` 的 `qty − received_qty` | 已下单未到货的量。不算它 → 同一批货重复下单（采购刚下完单，补货建议还让再买一遍） |
//!
//! # 为什么在途是「发货在途」而不是「采购在途」
//!
//! 「采购在途」在本仓就是**未清采购订单**（PRD E2-09 的一行 = 一个未收完的订单行）——
//! 它已经作为第二项算进 `open_po_qty` 了，再算一遍就是同一批货加两次。
//! 而 `biz_inventory_in_transit` 是 M2-01~M2-07 那条线上**唯一叫「在途」的账**（发货过账 →
//! 签收确认），所以本表的 `outbound_transit_qty` 指它，口径与 M2-04 的 `/summary` 逐格相等
//! （真库用例 `outbound_transit_matches_transit_summary` 拿真账对拍）。
//!
//! # 阈值只调一次（M2-13 的规矩，M2-14 已经照做，这里第三次）
//!
//! 三级回退（策略启用 > 物料级 `safety_stock` > 不判；停用回退；策略行 `min = 0` 阻断回退）
//! 的唯一实现在 [`crate::business::erp::stock_policy::dto::resolve_min_qty`]。
//! SQL 只负责把**两处事实**（策略行三列 + 物料级 `safety_stock`）取出来，判定在 Rust 里调它 ——
//! 一旦这里也写一份 `COALESCE(NULLIF(...))`，「设置页显示 100、建议按 50 算」就再也没有
//! 任何地方会报错。
//!
//! # 补货建议 = min/max 补货法，不是「低于最低就补到最低」
//!
//! ```text
//! 触发点 = 最低水位（再订货点），补到 = 最高水位（设了的话）
//! min = 50 / max = 100 / 预计可用 = 30  → 建议 70（补到 100，一次补够，少下几次单）
//! min = 50 / 未设 max / 预计可用 = 30   → 建议 20（补到 50）
//! 预计可用 = 55（>= min，但 < max）     → 建议 0（没跌破再订货点，不补；这不是「超储」）
//! ```
//!
//! 若「低于最低补到最低」，「最高水位」就永远只是个摆设列（M2-13 的迁移注释写明它是
//! 「M2-15 不建议再补」的抓手）—— 所以触发用 min、补到用 max，两列各有各的活。
//!
//! # 三种结论（`verdict`）
//!
//! | verdict | 含义 | 默认返回 |
//! |:--|:--|:--:|
//! | `PURCHASE` | 建议采购量 > 0，要下单 | ✅ |
//! | `COVERED` | 账面结存已低于最低水位，但未清订单把缺口盖住了 → **不用再下单** | 仅 `include_covered=true` |
//! | `OK` | 没跌破再订货点（或该格没有水位设置） → 无建议 | ❌ |
//!
//! `COVERED` 是这张表的价值之一：采购最怕的是「刚下完单又看到一条建议」。
//! 但它默认不出现在列表里（列表是待办清单，不是台账）。
//!
//! # 命名 / 边界口径（逐条都有出处）
//!
//! * **结存**取 `biz_inventory.qty`（已过账结存），**负库存照实数**（不 `GREATEST(qty,0)` 抹平：
//!   账实不符比「低一点」更该补，与 `docs/ERP-摆设字段口径.md` §2 同口径）；
//! * **锁定 `locked_qty` 不减**：本版本恒为 0、无占用流程（同上文档 §3），减它是在算一个不存在的数；
//! * **在途仓排除**（`warehouse_type <> 'IN_TRANSIT'`）：虚拟记账仓不可补货，
//!   与 M2-14 的扫描、`reminder_stock` 同口径；
//! * **停用 / 删除的物料不出现**（`m.deleted_at IS NULL AND m.status = 1`），仓库同理；
//! * **调拨在途不计**：调拨过账（`DONE`）在一个事务里同时写调出仓与调入仓，中途没有可观测的
//!   半途量（`biz_stock_transfer.transit_warehouse_id` 只是单据字段，同上文档 §2）；
//! * **未清订单按行上的收货仓归属**：`biz_purchase_order.warehouse_id`（订单头的默认收货仓），
//!   与水位、结存同一维（物料 × 仓库）—— 不按供应商 / 不按期望到货日拆；
//! * **`DRAFT` 的采购订单不计**（还没提交，可随时删改、不可收货）；`SUBMITTED` **计入**
//!   （审批流之外它也能被收货，见 `purchase_receivable` 同时接受 SUBMITTED / APPROVED）；
//!   被驳回 / 作废的单下一轮就掉出，建议量随即回来 —— 这正是「实时现算」的意义。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_inventory 是仓库作业台账，无 dept_id / 归属人列（V0137），
//!               数据范围（SELF / DEPT_*）表达不出来；与 M2-14 的水位检查、
//!               M2-10 的呆滞料、reminder_stock 的低库存扫描同锚点。
//!               可见性由读码 business:erp:replenish:list 控制（V0430 种在「即时库存」页下）。
//! ```

use std::collections::BTreeSet;

use nik::prelude::rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::business::erp::stock_policy::dto::{
    EffectiveMin, MinSource, PolicyLevels, resolve_max_qty, resolve_min_qty,
};

// ==================== 常量 ====================

/// 一次扫描最多取多少格（`物料 × 仓库`）。
///
/// 与 M2-14 的 `SCAN_LIMIT = 500` **故意不同值**：那边是「每小时给低水位的格建待办」，
/// 500 格已经超过一个人一天能处理的量；这边是**列表页**，翻页要看得见全景，2000 格覆盖
/// 常态库存规模。超过则 `scan_truncated = true` 并在 `hints` 里点名，提示按物料 / 仓库收窄 ——
/// 不静默截断（截断后说「没有更多」是最坏的假数据）。
pub const CELL_SCAN_LIMIT: i64 = 2000;

/// 默认每页条数（`page_size = 0` 时回落）。
pub const DEFAULT_PAGE_SIZE: u64 = 20;

/// 每页条数上限（与全仓分页口径一致：`erp/model_base.rs::MAX_PAGE_SIZE`）。
pub const MAX_PAGE_SIZE: u64 = 200;

/// 结论：建议采购量 > 0（要下单）。
pub const VERDICT_PURCHASE: &str = "PURCHASE";
/// 结论：跌破最低水位，但未清订单 + 在途已覆盖缺口（不用再下单）。
pub const VERDICT_COVERED: &str = "COVERED";
/// 结论：没跌破再订货点（或该格没有水位设置）→ 无建议。
pub const VERDICT_OK: &str = "OK";

/// 目标水位来源：最高水位（策略行的 `max_qty`）。
pub const TARGET_MAX: &str = "MAX";
/// 目标水位来源：最低水位（未设最高水位时补到最低水位）。
pub const TARGET_MIN: &str = "MIN";
/// 目标水位来源：无（该格不判水位）。
pub const TARGET_NONE: &str = "NONE";

/// 结论中文标签（前端直接显示，避免每处各写一份 match）。
pub fn verdict_label(verdict: &str) -> &'static str {
    match verdict {
        VERDICT_PURCHASE => "建议采购",
        VERDICT_COVERED => "在途覆盖",
        VERDICT_OK => "无需补货",
        _ => "未知",
    }
}

// ==================== 入参 ====================

/// 补货建议列表筛选（`GET /business/erp/replenish/page`）。
///
/// 全部可留空（= 全库扫）。`include_covered` 是开关而不是筛选：它决定「不需要下单但已经
/// 跌破水位」的行要不要一起出现（见模块头「三种结论」）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ReplenishFilter {
    /// 物料 id（精确 UUID；留空 = 全部）。
    pub material_id: Option<String>,
    /// 仓库 id（精确 UUID；留空 = 全部）。
    pub warehouse_id: Option<String>,
    /// 关键字（物料编码 / 名称 / 仓库编码 / 名称）。
    pub keyword: Option<String>,
    /// 是否连「在途覆盖」的行一起返回（`true` / `false` / `1` / `0`；缺省 `false`）。
    pub include_covered: Option<String>,
    /// 页码（从 1 起；`0` 回落第 1 页）。
    pub page: u64,
    /// 每页条数（`0` 回落 20，上限 200）。
    pub page_size: u64,
}

// ==================== 一格的事实（model → dto） ====================

/// 一格（`物料 × 仓库`）在**这一刻**的四个事实。
///
/// 全部来自库里的列，本结构不做任何算术 —— 算术在 [`suggest`]（纯函数，可单独测）。
/// 四个量刻意分开而不是在 SQL 里先算出「预计可用」：出参要把各项**分别**给用户看
/// （「缺口 70 = 目标 100 − 结存 25 − 在途 5」），一分项不见了就没人能复核。
#[derive(Debug, Clone)]
pub struct CellFacts {
    /// 账面结存（`biz_inventory.qty`，可为负）。
    pub on_hand: Decimal,
    /// 发货在途（`biz_inventory_in_transit` 里 `status='IN_TRANSIT'` 的 `qty_in_transit` 合计）。
    pub outbound_transit: Decimal,
    /// 未清采购订单未收量（`qty − received_qty` 合计）。
    pub open_po: Decimal,
    /// 物料级安全库存（`biz_material.safety_stock`，策略行缺席时的回退值）。
    pub material_safety_stock: Decimal,
    /// 策略行三列（`LEFT JOIN` 未命中 = `None`）。
    pub policy: Option<PolicyLevels>,
}

// ==================== 一格的结论（纯函数） ====================

/// 一格的补货结论（全部由 [`suggest`] 从 [`CellFacts`] 算出）。
#[derive(Debug, Clone)]
pub struct Suggestion {
    /// 可用结存 = 账面结存 − 发货在途。
    pub available_qty: Decimal,
    /// 预计可用 = 可用结存 + 未清订单未收量。
    pub expected_qty: Decimal,
    /// 生效最低水位（`None` = 该格不判水位）。
    pub min_qty: Option<Decimal>,
    /// 最低水位来源（`POLICY` / `MATERIAL` / `NONE`）。
    pub min_source: MinSource,
    /// 生效最高水位（只来自启用中的策略行）。
    pub max_qty: Option<Decimal>,
    /// 目标水位（补到这里）= `max_qty` 有则用它，否则用 `min_qty`。
    pub target_qty: Option<Decimal>,
    /// 目标水位来源（`MAX` / `MIN` / `NONE`）。
    pub target_source: &'static str,
    /// 缺口 = `max(0, 目标水位 − 预计可用)`（未触发时也算出来，方便解释「差多少到满水位」）。
    pub shortage_qty: Decimal,
    /// **建议采购量** = 触发时取缺口，未触发取 0。
    pub suggested_qty: Decimal,
    /// 账面结存是否低于最低水位（**M2-14 的判定口径**，与待办是否生成逐格一致）。
    pub below_min: bool,
    /// 预计可用是否低于最低水位（**本表的补货触发口径**）。
    pub triggered: bool,
    /// 结论：`PURCHASE` / `COVERED` / `OK`。
    pub verdict: &'static str,
}

/// 算一格的补货结论（**判定口径的唯一实现**）。
///
/// 判定顺序（改顺序会改结果）：
///
/// ```text
/// 1. 阈值    min = resolve_min_qty(策略行, 物料级 safety_stock)   ← M2-13 的唯一实现
///            max = resolve_max_qty(策略行)
/// 2. 供给    available = on_hand − outbound_transit
///            expected  = available + open_po
/// 3. 触发    min 有值 且 expected < min          （严格小于；min = 0 在 resolve_min_qty 里已是 None）
/// 4. 目标    target = max ?? min
/// 5. 数量    shortage  = max(0, target − expected)
///            suggested = triggered ? shortage : 0
/// 6. 结论    suggested > 0        → PURCHASE
///            on_hand < min（有值） → COVERED（账面低，但在途/未清订单盖住了）
///            否则                  → OK
/// ```
///
/// 边界（真库与纯逻辑用例都盯着）：
///
/// * `expected == min` → **不触发**（「等于不报」与 M2-14 的 `qty < min` 同一刀）；
/// * 负结存 / 负可用 → 缺口照实算（不夹 0 以外的抹平）；
/// * 该格 `min` 为 `None`（没设 / 策略 `0` 明示不预警 / 策略停用且物料级为 0）→ 永远 `OK` +
///   建议 0：**没有水位就没有「补到多少」这个目标**，凭空按 0 补货就是拍数；
/// * `max` 只在策略启用时有值（物料级从来没有最高水位，见 M2-13 模块头）。
pub fn suggest(facts: &CellFacts) -> Suggestion {
    let EffectiveMin {
        qty: min_qty,
        source: min_source,
    } = resolve_min_qty(facts.policy.as_ref(), facts.material_safety_stock);
    let max_qty = resolve_max_qty(facts.policy.as_ref());

    let available_qty = facts.on_hand - facts.outbound_transit;
    let expected_qty = available_qty + facts.open_po;

    let target_qty = max_qty.or(min_qty);
    let target_source = match (max_qty, min_qty) {
        (Some(_), _) => TARGET_MAX,
        (None, Some(_)) => TARGET_MIN,
        (None, None) => TARGET_NONE,
    };

    let triggered = matches!(min_qty, Some(min) if expected_qty < min);
    let shortage_qty = match target_qty {
        Some(target) => (target - expected_qty).max(Decimal::ZERO),
        None => Decimal::ZERO,
    };
    let suggested_qty = if triggered { shortage_qty } else { Decimal::ZERO };
    let below_min = matches!(min_qty, Some(min) if facts.on_hand < min);

    let verdict = if suggested_qty > Decimal::ZERO {
        VERDICT_PURCHASE
    } else if below_min {
        // 账面已经跌破水位，但未清订单 / 在途把缺口盖住了 → 不用再下单
        VERDICT_COVERED
    } else {
        VERDICT_OK
    };

    Suggestion {
        available_qty,
        expected_qty,
        min_qty,
        min_source,
        max_qty,
        target_qty,
        target_source,
        shortage_qty,
        suggested_qty,
        below_min,
        triggered,
        verdict,
    }
}

/// 这一行要不要出现在结果里。
///
/// 默认只留 `PURCHASE`（列表是待办清单）；`include_covered = true` 时把 `COVERED` 也带上
/// （回答「为什么这条建议消失了」）。`OK` 永远不出现 —— 没跌破再订货点的格子有几百上千行，
/// 混进来只会把真正的缺口埋掉（要看全景的走「即时库存」页）。
pub fn keep_row(suggestion: &Suggestion, include_covered: bool) -> bool {
    match suggestion.verdict {
        VERDICT_PURCHASE => true,
        VERDICT_COVERED => include_covered,
        _ => false,
    }
}

// ==================== 出参 ====================

/// 一行补货建议（`物料 × 仓库` 一格）。
///
/// `*_id` 一律成对返回名字（AGENTS.md 约定 13）。这里是**主数据关联**（要显示「现在叫什么」），
/// 名字由同一条 SQL `LEFT JOIN` 带出；主数据软删后为 `null`（那一格也就不会出现在本表里，
/// 因为扫描条件已经排除停用 / 删除的物料与仓库）。
#[derive(Debug, Clone, Serialize)]
pub struct ReplenishRowRes {
    /// 这一格由几条 `biz_inventory` 成本桶汇总而来（绝大多数是 1；按库位分成本桶时 > 1）。
    ///
    /// 身份是 `(material_id, warehouse_id)` 而不是某一条库存行 id —— 补货是按**仓**补的，
    /// 同一个仓里多个库位成本桶必须合计（否则每一桶都单独判水位，账面 35 会被看成 30 和 5 两格，
    /// 建议量凭空变大）。本列只为让用户能复核「这个数是怎么来的」。
    pub inventory_row_count: i64,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 规格型号。
    pub spec: Option<String>,
    /// 计量单位名。
    pub unit_name: Option<String>,
    /// 仓库 id。
    pub warehouse_id: String,
    /// 仓库编码。
    pub warehouse_code: Option<String>,
    /// 仓库名称。
    pub warehouse_name: Option<String>,
    /// 账面结存（`biz_inventory.qty`，可为负）。
    pub on_hand_qty: Decimal,
    /// 发货在途（已发未签收；账面还没扣）。
    pub outbound_transit_qty: Decimal,
    /// 未清采购订单未收量。
    pub open_po_qty: Decimal,
    /// 可用结存 = 账面结存 − 发货在途。
    pub available_qty: Decimal,
    /// 预计可用 = 可用结存 + 未清订单未收量。
    pub expected_qty: Decimal,
    /// 生效最低水位（`null` = 不判水位）。
    pub min_qty: Option<Decimal>,
    /// 最低水位来源（`POLICY` / `MATERIAL` / `NONE`）。
    pub min_source: String,
    /// 生效最高水位（`null` = 未设，补到最低水位）。
    pub max_qty: Option<Decimal>,
    /// 补货目标水位（`max_qty ?? min_qty`）。
    pub target_qty: Option<Decimal>,
    /// 目标水位来源（`MAX` / `MIN` / `NONE`）。
    pub target_source: String,
    /// 缺口 = `max(0, 目标水位 − 预计可用)`。
    pub shortage_qty: Decimal,
    /// **建议采购量** = 触发时取缺口，未触发为 0。
    pub suggested_qty: Decimal,
    /// 账面结存 < 最低水位（M2-14 判定口径）。
    pub below_min: bool,
    /// 预计可用 < 最低水位（本表触发口径）。
    pub triggered: bool,
    /// 结论（`PURCHASE` / `COVERED` / `OK`）。
    pub verdict: String,
    /// 结论中文名。
    pub verdict_label: String,
}

/// 全量汇总（**分页前**算，与 `slow_moving` 的「先汇总再截断」同一条硬要求）。
///
/// 先截断再汇总 = 给用户一张「扫过 2000 格、缺 200 格」的假合计。
#[derive(Debug, Clone, Serialize)]
pub struct ReplenishTotals {
    /// 满足条件的行数（= `total`）。
    pub row_count: u64,
    /// 其中 `PURCHASE` 的行数。
    pub purchase_row_count: u64,
    /// 其中 `COVERED` 的行数（只在 `include_covered=true` 时非 0）。
    pub covered_row_count: u64,
    /// 涉及多少个物料（去重）。
    pub material_count: u64,
    /// 建议采购量合计。
    ///
    /// ⚠ 跨物料的 `qty` 是各物料单位的直接相加（件 / 公斤 / 米 …），只能看量级；
    /// 「多少行要下单」才是个可靠的数（与 M2-10 的 `qty_slow` 同一条免责）。
    pub suggested_qty_total: Decimal,
}

/// 补货建议列表出参。
#[derive(Debug, Clone, Serialize)]
pub struct ReplenishRes {
    /// 本次扫描到多少格（`物料 × 仓库`）。
    pub scanned_cells: u64,
    /// 扫描上限 [`CELL_SCAN_LIMIT`]。
    pub scan_limit: u64,
    /// 是否撞到扫描上限（撞到 = 结果可能不全，`hints` 会提示收窄）。
    pub scan_truncated: bool,
    /// 满足条件的行数（分页前全量）。
    pub total: u64,
    /// 当前页码（从 1 起）。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 全量汇总。
    pub totals: ReplenishTotals,
    /// 本页的行。
    pub rows: Vec<ReplenishRowRes>,
    /// 口径说明 / 空态解释（前端提示条直接显示）。
    pub hints: Vec<String>,
}

// ==================== 排序 / 截断 / 汇总（纯函数，可单独测） ====================

/// 排序：**建议采购量大的在前**（最急的在第一屏）。
///
/// 同量时按 物料编码 → 物料 id → 仓库 id 定序 —— 三列合起来唯一（一格 = 物料 × 仓库），
/// 翻页不重不漏（只用 `suggested_qty` 排序时，同量行在两页之间会漂）。
pub fn sort_rows(rows: &mut [ReplenishRowRes]) {
    rows.sort_by(|left, right| {
        right
            .suggested_qty
            .cmp(&left.suggested_qty)
            .then_with(|| left.material_code.cmp(&right.material_code))
            .then_with(|| left.material_id.cmp(&right.material_id))
            .then_with(|| left.warehouse_id.cmp(&right.warehouse_id))
    });
}

/// 汇总（传入的是**过滤后、分页前**的全量行）。
pub fn totals_of(rows: &[ReplenishRowRes]) -> ReplenishTotals {
    let mut materials: BTreeSet<&str> = BTreeSet::new();
    let mut purchase = 0u64;
    let mut covered = 0u64;
    let mut sum = Decimal::ZERO;
    for row in rows {
        materials.insert(row.material_id.as_str());
        match row.verdict.as_str() {
            VERDICT_PURCHASE => purchase += 1,
            VERDICT_COVERED => covered += 1,
            _ => {}
        }
        sum += row.suggested_qty;
    }
    ReplenishTotals {
        row_count: rows.len() as u64,
        purchase_row_count: purchase,
        covered_row_count: covered,
        material_count: materials.len() as u64,
        suggested_qty_total: sum,
    }
}

/// 归一化 `(page, page_size)`：页码从 1 起、`page_size = 0` 回落默认、上限 [`MAX_PAGE_SIZE`]。
///
/// 与 `erp/model_base.rs::normalize_page` 同口径，但本表在 Rust 里分页（结论算完才知道行数），
/// 所以在这里再落一份而不是去 clamp SQL 的 `LIMIT/OFFSET`。
pub fn normalize_page(page: u64, page_size: u64) -> (u64, u64) {
    let page = page.max(1);
    let size = if page_size == 0 {
        DEFAULT_PAGE_SIZE
    } else {
        page_size.min(MAX_PAGE_SIZE)
    };
    (page, size)
}

/// 取本页的切片（越界返回空页，不报错 —— 「翻到第 9 页而只有 3 页」是正常的翻页手感）。
///
/// 边界（M2-16 的 `boundary_tests::page_bounds_are_clamped_and_never_panic` 钉着）：
/// `page = 0` 与 `page = u64::MAX` 都必须给空页而不是 panic —— 前者会让 `page - 1`
/// 下溢，后者会让 `(page - 1) * page_size` 上溢（debug 构建 panic ⇒ 接口 500，
/// 而 `page` 是 URL 上随手可改的数）。
pub fn page_slice<T: Clone>(rows: &[T], page: u64, page_size: u64) -> Vec<T> {
    let page = page.max(1);
    let start = (page - 1).saturating_mul(page_size);
    let start = usize::try_from(start).unwrap_or(usize::MAX);
    if start >= rows.len() {
        return Vec::new();
    }
    let end = start.saturating_add(page_size as usize).min(rows.len());
    rows[start..end].to_vec()
}

/// 扫描撞上限时的提示（[`crate::business::erp::replenish::model`] 返回行数够了就置位）。
pub fn scan_truncated_hint(scanned: usize) -> String {
    format!(
        "扫描到 {scanned} 格（物料 × 仓库）就撞到上限 {} —— 结果**可能不全**，\
         请按物料或仓库收窄条件重查；汇总与合计只统计扫描到的这些格",
        CELL_SCAN_LIMIT
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    fn levels(min: &str, max: Option<&str>, enabled: bool) -> PolicyLevels {
        PolicyLevels {
            min_qty: dec(min),
            max_qty: max.map(dec),
            enabled,
        }
    }

    fn facts(on_hand: &str, transit: &str, open_po: &str, safety: &str, policy: Option<PolicyLevels>) -> CellFacts {
        CellFacts {
            on_hand: dec(on_hand),
            outbound_transit: dec(transit),
            open_po: dec(open_po),
            material_safety_stock: dec(safety),
            policy,
        }
    }

    /// 卡面的主场景：安全库存 + 在途 + 未清订单三样都参与算术（少了任一项数字就变）。
    #[test]
    fn suggested_qty_uses_all_three_inputs() {
        let policy = levels("50", Some("100"), true);
        let s = suggest(&facts("30", "5", "10", "0", Some(policy)));
        assert_eq!(s.available_qty, dec("25"), "可用 = 结存 30 − 在途 5");
        assert_eq!(s.expected_qty, dec("35"), "预计可用 = 25 + 未清订单 10");
        assert_eq!(s.target_qty, Some(dec("100")), "设了最高水位就补到最高水位");
        assert_eq!(s.target_source, TARGET_MAX);
        assert_eq!(s.suggested_qty, dec("65"), "建议 = 100 − 35");
        assert_eq!(s.verdict, VERDICT_PURCHASE);
        assert!(s.triggered && s.below_min);

        // 同一结存、把在途与未清订单都归零 → 数字必须变大（证明两项真的参与了）
        let s = suggest(&facts("30", "0", "0", "0", Some(levels("50", Some("100"), true))));
        assert_eq!(s.suggested_qty, dec("70"));
        println!("[M2-15] 建议采购量 = 目标水位 − 预计可用（结存 / 在途 / 未清订单三项都参与）✓");
    }

    /// 边界：`预计可用 == 最低水位` **不触发**（与 M2-14 的 `qty < min` 同一刀）。
    #[test]
    fn equal_to_min_does_not_trigger() {
        // 结存 55 − 在途 5 = 预计可用 50 == 最低水位 50
        let policy = levels("50", None, true);
        let s = suggest(&facts("55", "5", "0", "0", Some(policy)));
        assert_eq!(s.expected_qty, dec("50"));
        assert!(!s.triggered, "等于再订货点不补");
        assert_eq!(s.suggested_qty, Decimal::ZERO);
        assert_eq!(s.verdict, VERDICT_OK, "没跌破就不该有行");

        // 差 0.0001 就触发（严格小于）：预计可用 49.9999 → 补 0.0001
        let s = suggest(&facts("54.9999", "5", "0", "0", Some(levels("50", None, true))));
        assert!(s.triggered);
        assert_eq!(s.suggested_qty, dec("0.0001"));
        println!("[M2-15] 预计可用 == 最低水位不触发、差 0.0001 即触发 ✓");
    }

    /// 未设最高水位 → 补到最低水位；设了 → 触发点仍是 min、补到 max。
    #[test]
    fn trigger_uses_min_but_fills_up_to_max() {
        let no_max = suggest(&facts("30", "0", "0", "0", Some(levels("50", None, true))));
        assert_eq!(no_max.target_source, TARGET_MIN);
        assert_eq!(no_max.suggested_qty, dec("20"));

        // 跌破 min 但远低于 max：一次补到 max（30 → 100），不是补到 50
        let with_max = suggest(&facts("30", "0", "0", "0", Some(levels("50", Some("100"), true))));
        assert_eq!(with_max.suggested_qty, dec("70"));

        // 在 min 与 max 之间：不触发（min/max 补货法的正确行为，不是「没补满就要补」）
        let between = suggest(&facts("70", "0", "0", "0", Some(levels("50", Some("100"), true))));
        assert_eq!(between.expected_qty, dec("70"));
        assert!(!between.triggered);
        assert_eq!(between.suggested_qty, Decimal::ZERO);
        assert_eq!(between.verdict, VERDICT_OK);
        assert_eq!(
            between.shortage_qty,
            dec("30"),
            "缺口仍算出来（解释「差多少到满水位」）"
        );
        println!("[M2-15] 触发用最低水位、补到用最高水位；min~max 之间不补 ✓");
    }

    /// `COVERED`：账面跌破水位，但未清订单把缺口盖住 → 建议 0、结论不是在途覆盖之外的「无需补货」。
    #[test]
    fn open_po_covers_the_shortfall() {
        let s = suggest(&facts("30", "0", "40", "0", Some(levels("50", None, true))));
        assert_eq!(s.expected_qty, dec("70"));
        assert!(!s.triggered);
        assert!(s.below_min, "账面 30 < 水位 50");
        assert_eq!(s.suggested_qty, Decimal::ZERO);
        assert_eq!(s.verdict, VERDICT_COVERED);
        assert!(!keep_row(&s, false), "默认不返回（列表是待办清单）");
        assert!(keep_row(&s, true), "include_covered=true 时才返回");
        println!("[M2-15] 未清订单覆盖缺口 → COVERED、建议 0、默认不出现在列表 ✓");
    }

    /// 三级回退只走 `resolve_min_qty`：策略启用覆盖物料级 / 停用回退 / `min = 0` 明示不预警。
    #[test]
    fn water_level_falls_back_through_the_single_implementation() {
        // 策略启用 100 覆盖物料级 20
        let s = suggest(&facts("50", "0", "0", "20", Some(levels("100", None, true))));
        assert_eq!(s.min_qty, Some(dec("100")));
        assert_eq!(s.min_source, MinSource::Policy);
        assert_eq!(s.suggested_qty, dec("50"));

        // 策略停用 → 回退物料级 20（50 >= 20 不触发）
        let s = suggest(&facts("50", "0", "0", "20", Some(levels("100", None, false))));
        assert_eq!(s.min_qty, Some(dec("20")));
        assert_eq!(s.min_source, MinSource::Material);
        assert_eq!(s.suggested_qty, Decimal::ZERO);
        assert_eq!(s.max_qty, None, "停用行的 max 不生效");

        // 策略启用且 min = 0 → 明示本格不预警（阻断回退）：物料级 20 也不管用
        let s = suggest(&facts("0", "0", "0", "20", Some(levels("0", None, true))));
        assert_eq!(s.min_qty, None);
        assert_eq!(s.min_source, MinSource::Policy);
        assert_eq!(s.verdict, VERDICT_OK);
        assert_eq!(s.target_source, TARGET_NONE);
        println!("[M2-15] 三级回退只走 resolve_min_qty（启用 / 停用 / min=0 阻断）✓");
    }

    /// 负结存照实算（不 `GREATEST(qty,0)`）；负可用同样不抹平。
    #[test]
    fn negative_stock_is_not_flattened() {
        let s = suggest(&facts("-10", "0", "0", "0", Some(levels("50", None, true))));
        assert_eq!(s.available_qty, dec("-10"));
        assert_eq!(s.suggested_qty, dec("60"), "50 − (−10) = 60");
        assert!(s.below_min && s.triggered);

        // 在途比结存还多（账面 10、已发 30）→ 可用 −20，缺口比账面更大
        let s = suggest(&facts("10", "30", "0", "0", Some(levels("50", None, true))));
        assert_eq!(s.available_qty, dec("-20"));
        assert_eq!(s.suggested_qty, dec("70"));
        println!("[M2-15] 负结存 / 负可用不夹 0，缺口照实算 ✓");
    }

    /// 排序：建议量降序，同量按 物料编码 → 物料 id → 仓库 定序（翻页稳定）。
    #[test]
    fn sort_is_stable_and_urgency_first() {
        let mut rows: Vec<ReplenishRowRes> = Vec::new();
        for (code, mat, wh, qty) in [
            ("M-A", "a", "w1", "10"),
            ("M-B", "b", "w1", "70"),
            ("M-A", "a", "w2", "10"),
            ("M-C", "c", "w1", "70"),
        ] {
            let mut row = sample_row();
            row.material_code = Some(code.to_string());
            row.material_id = mat.to_string();
            row.warehouse_id = wh.to_string();
            row.suggested_qty = dec(qty);
            rows.push(row);
        }
        sort_rows(&mut rows);
        let order: Vec<String> = rows
            .iter()
            .map(|row| format!("{}@{}", row.material_code.as_deref().unwrap_or(""), row.warehouse_id))
            .collect();
        assert_eq!(
            order,
            vec!["M-B@w1", "M-C@w1", "M-A@w1", "M-A@w2"],
            "70 在前；同量按物料编码 / 物料 id / 仓库定序"
        );
        println!("[M2-15] 排序：建议量降序 + 三列兜底全序 ✓");
    }

    /// 汇总：行数 / PURCHASE / COVERED / 物料去重 / 合计。
    #[test]
    fn totals_are_computed_on_the_full_set() {
        let mut rows = Vec::new();
        for (verdict, qty, mat) in [
            (VERDICT_PURCHASE, "10", "mat-a"),
            (VERDICT_PURCHASE, "5.5", "mat-a"),
            (VERDICT_COVERED, "0", "mat-b"),
        ] {
            let mut row = sample_row();
            row.verdict = verdict.to_string();
            row.suggested_qty = dec(qty);
            row.material_id = mat.to_string();
            rows.push(row);
        }
        let totals = totals_of(&rows);
        assert_eq!(totals.row_count, 3);
        assert_eq!(totals.purchase_row_count, 2);
        assert_eq!(totals.covered_row_count, 1);
        assert_eq!(totals.material_count, 2, "同一物料两行只算一个");
        assert_eq!(totals.suggested_qty_total, dec("15.5"));
        println!("[M2-15] 汇总：行数 / 结论计数 / 物料去重 / 建议量合计 ✓");
    }

    /// 分页：页码从 1 起、page_size 有默认与上限、越界返回空页（不报错）。
    #[test]
    fn page_slice_clamps_and_never_panics() {
        assert_eq!(normalize_page(0, 0), (1, DEFAULT_PAGE_SIZE));
        assert_eq!(normalize_page(2, 10_000), (2, MAX_PAGE_SIZE));

        let rows: Vec<u32> = (1..=5).collect();
        assert_eq!(page_slice(&rows, 1, 2), vec![1, 2]);
        assert_eq!(page_slice(&rows, 3, 2), vec![5]);
        assert!(page_slice(&rows, 4, 2).is_empty());
        assert!(page_slice(&rows, 99, 20).is_empty());
        println!("[M2-15] 分页：归一化 + 越界空页 ✓");
    }

    /// 结论标签与常量互相对应（前端显示名不许缺一项）。
    #[test]
    fn verdict_labels_cover_every_verdict() {
        assert_eq!(verdict_label(VERDICT_PURCHASE), "建议采购");
        assert_eq!(verdict_label(VERDICT_COVERED), "在途覆盖");
        assert_eq!(verdict_label(VERDICT_OK), "无需补货");
        assert_ne!(verdict_label("???") as &str, "");
        println!("[M2-15] 三种结论都有中文标签 ✓");
    }

    fn sample_row() -> ReplenishRowRes {
        ReplenishRowRes {
            inventory_row_count: 1,
            material_id: "mat".to_string(),
            material_code: None,
            material_name: None,
            spec: None,
            unit_name: None,
            warehouse_id: "wh".to_string(),
            warehouse_code: None,
            warehouse_name: None,
            on_hand_qty: Decimal::ZERO,
            outbound_transit_qty: Decimal::ZERO,
            open_po_qty: Decimal::ZERO,
            available_qty: Decimal::ZERO,
            expected_qty: Decimal::ZERO,
            min_qty: None,
            min_source: MinSource::Unset.as_str().to_string(),
            max_qty: None,
            target_qty: None,
            target_source: TARGET_NONE.to_string(),
            shortage_qty: Decimal::ZERO,
            suggested_qty: Decimal::ZERO,
            below_min: false,
            triggered: false,
            verdict: VERDICT_OK.to_string(),
            verdict_label: verdict_label(VERDICT_OK).to_string(),
        }
    }
}
