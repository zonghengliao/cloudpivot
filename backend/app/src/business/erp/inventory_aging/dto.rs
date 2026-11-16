//! 库龄（M2-08）的出入参契约 + 纯逻辑。
//!
//! 卡：t_397dc77c（M2-08【M2·仓库】库龄：计算（按批次入库时间））；拆卡依据
//! `docs/planning/20261113-新卡清单.md` §M2 的 M2-08 行 —— **本卡只做计算**，
//! 0-30/31-60/61-90/90+ 的报表是 M2-09、呆滞料标记是 M2-10、前端页是 M2-12。
//!
//! # 库龄层是什么
//!
//! `biz_inventory_txn` 是**追加写**的库存流水（`tzx_type = IN/OUT`，见 V0137）。
//! 每一笔 `IN` 就是一层「批次」：它带着自己的入库数量、入库单（批号）与**业务日期**。
//! 结存量按 **FIFO** 摊回这些层（先入先出 ⇒ 出库先吃掉最老的层，剩下的是较新的层），
//! 于是「某一层还剩多少 + 它入库到现在多少天」就是库龄。
//!
//! ```text
//! 层① 100 件 09-01 入库 ─┐
//! 层② 200 件 10-15 入库 ─┤ 出库 180 件（FIFO）
//! 层③  50 件 11-10 入库 ─┘
//!   ⇒ 层① 剩 0（吃满）、层② 剩 120、层③ 剩 50
//!   ⇒ 库龄：层② = as_of − 10-15，层③ = as_of − 11-10
//! ```
//!
//! # 「入库时间」取业务日期，不取过账时间
//!
//! 流水的 `created_at` 是**过账那一刻的墙钟时间**（`stockin_handler_post.rs` 里
//! `let now = Utc::now()`）。入库单的 `in_date` 才是「这批货哪天进的仓」，两者可以差几天
//! （补录 / 审批跨周）。所以层日期一律 **COALESCE 到来源单据的业务日期**：
//!
//! | `doc_type` | 层日期取 |
//! |:--|:--|
//! | `STOCK_IN`（采购入库 / 销售退货回库 / 完工入库） | `biz_stock_in.in_date` |
//! | `STOCK_OUT`（出库 / 采购退货） | `biz_stock_out.out_date` |
//! | `STOCK_TRANSFER`（调拨两段） | `biz_stock_transfer.transfer_date` |
//! | `STOCK_TAKE`（盘盈盘亏） | `biz_stock_take.take_date` |
//! | `OPENING`（库存期初） | `period_code` 的月初（期初=期间起点） |
//! | 其它（`MATERIAL_SERIAL` / `SERIAL_BATCH` 等） | `created_at` 换算成 CST 日期 |
//!
//! 兜底 `created_at` 的那一档是**已知的近似**（写这份代码时它们没有业务日期列）；
//! 只要以后哪类单据补了业务日期，在 `model::DATED_CTE` 里加一条 `LEFT JOIN` 即可，
//! 本文件的 `age_days_of` 与「按天算」的口径不用动。
//!
//! # 出参里的 `*_id` 成对返回 `*_name`（AGENTS.md 约定 13）
//!
//! 批次层是**业务展示类**（要显示物料 / 仓库「现在叫什么」），所以名字实时 `LEFT JOIN`
//! 主数据、软删按「缺字段」处理（`null`），不存快照。
//!
//! # `age_days` 由谁算
//!
//! **Rust 侧算**（[`age_days_of`]）：`as_of − inbound_date` 的整日数，一天一个定义、只写一处。
//! SQL 侧只做 `inbound_date` 的区间过滤（把 `min/max_age_days` 折算成日期上下界，
//! 见 [`inbound_bounds`]），算术结果与这里逐字一致（单测 `aging_tests` 盯着这件事）。
//! **不夹 0**：入库日期晚于 `as_of`（补录到未来的单）时 `age_days` 是负数 —— 那是真数据，
//! 夹成 0 会把它伪装成「今天刚入库」。要只看已入库的，用 `min_age_days = 0` 过滤。

use nik::prelude::chrono::{DateTime, Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use cloudpivot_kernel::business::shared::period_key;

// ==================== 分页筛选 ====================

/// 批次层（库龄层）分页筛选（`GET /business/erp/inventory-aging/page`）。
///
/// 所有字段留空 = 不加该条件；**`as_of` 留空 ≠ 不加条件**：留空取「今天（CST）」
/// （库龄是「截止到某一天放了多久」，没有基准日的库龄没有意义）。
///
/// `include_consumed` 留空 = `false`：**只返回还有结存的层**（已被吃满的层是历史，
/// 一条 1000 件的老物料能翻出几百行「剩 0」的噪音）；要看「这层被谁吃完了」传 `true`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AgingFilter {
    /// 物料 id（精确 UUID）。
    pub material_id: Option<String>,
    /// 仓库 id（精确 UUID）。
    pub warehouse_id: Option<String>,
    /// 批号（精确匹配；来自入库单明细 `lot_no`，为空表示该层没有批号）。
    pub lot_no: Option<String>,
    /// 基准日（`yyyy-MM-dd`，含当天；留空 = 今天 CST）。
    pub as_of: Option<String>,
    /// 最小库龄（天，含；0 = 只要 `as_of` 当天或更早入库的层）。
    pub min_age_days: Option<i64>,
    /// 最大库龄（天，含）。
    pub max_age_days: Option<i64>,
    /// 是否把「已吃满（qty_remaining = 0）」的层也返回（`true/false`，默认 `false`）。
    pub include_consumed: Option<String>,
    /// 页码（从 1 起）。
    pub page: u64,
    /// 每页条数（0 → 默认 20，上限 200）。
    pub page_size: u64,
}

// ==================== 出参 ====================

/// 一层入库（= 一个批次层）在基准日的库龄快照。
///
/// 数量口径全部**取 SQL 算出来的列**（`qty_in` / `qty_consumed` / `qty_remaining`），
/// Rust 侧只做 id/名字/日期的搬运，不在这里重算一遍 FIFO —— 两处各算一遍迟早会漂。
#[derive(Debug, Clone, Serialize)]
pub struct AgingLayerRes {
    /// 层主键（= 那笔 `IN` 流水的 id）；同一张入库单的多行是多个层。
    pub layer_id: String,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码（物料软删后为 `null`）。
    pub material_code: Option<String>,
    /// 物料名称（物料软删后为 `null`）。
    pub material_name: Option<String>,
    /// 规格型号（物料软删后为 `null`）。
    pub spec: Option<String>,
    /// 单位名称（物料软删后为 `null`）。
    pub unit_name: Option<String>,
    /// 仓库 id。
    pub warehouse_id: String,
    /// 仓库编码（仓库软删后为 `null`）。
    pub warehouse_code: Option<String>,
    /// 仓库名称（仓库软删后为 `null`）。
    pub warehouse_name: Option<String>,
    /// 批号（入库单明细上的 `lot_no`；该层没有批号或非入库单来源时为 `null`）。
    pub lot_no: Option<String>,
    /// 来源单据类型（`STOCK_IN` / `STOCK_OUT` / `STOCK_TRANSFER` / `STOCK_TAKE` / `OPENING` …）。
    pub doc_type: String,
    /// 来源单号。
    pub doc_no: String,
    /// 来源单据行号。
    pub line_no: i32,
    /// 业务类型（`PURCHASE_IN` / `SALES_RETURN_IN` / `PRODUCTION_IN` / `OPENING` …）。
    pub biz_type: String,
    /// 层的入库日期（业务日期，见模块头注释的表）。
    pub inbound_date: NaiveDate,
    /// 库龄（天）= `as_of − inbound_date`；入库日期晚于基准日时为**负数**（不夹 0）。
    pub age_days: i64,
    /// 本层入库数量（原始数量，恒正）。
    pub qty_in: Decimal,
    /// 已被出库按 FIFO 吃掉的数量。
    pub qty_consumed: Decimal,
    /// 本层在基准日还剩的数量（= 库龄层的数量）。
    pub qty_remaining: Decimal,
    /// 本层入库单价（历史成本快照）。
    ///
    /// ⚠ 这是**层单价**，不是账面单价：`biz_inventory.avg_cost` 是移动加权，
    /// 两者本来就不相等（移动加权下账面金额不按层拆）。库龄是**数量**分析，
    /// 金额列只作参考（见 [`Self::amount_remaining`]）。
    pub unit_cost: Decimal,
    /// 参考金额 = `qty_remaining × unit_cost`（4 位；**不是**账面结存金额）。
    pub amount_remaining: Decimal,
    /// 过账时间（那笔 `IN` 流水的 `created_at`）。
    pub posted_at: DateTime<Utc>,
    /// 基准日（回显，调用方不用自己猜「今天」是哪天）。
    pub as_of: NaiveDate,
}

// ==================== 纯逻辑 ====================

/// 「今天」的默认基准日（北京时间，与 `period_key` 同口径）。
pub fn default_as_of() -> NaiveDate {
    period_key::today_cst(Utc::now())
}

/// 库龄（天）= 基准日 − 入库日期；**不夹 0**（负数是真数据，见模块头注释）。
pub fn age_days_of(as_of: NaiveDate, inbound_date: NaiveDate) -> i64 {
    (as_of - inbound_date).num_days()
}

/// 校验库龄区间：两个界都必须 ≥ 0，且 `min ≤ max`。
///
/// 为什么不接受负的界：库龄是「放了多久」，负界没有业务含义；而**行上的负数库龄**
/// （补录到未来的入库）靠 `min_age_days = 0` 就能筛掉，不需要负界。
pub fn ensure_age_range(min_age_days: Option<i64>, max_age_days: Option<i64>) -> std::result::Result<(), String> {
    if let Some(value) = min_age_days
        && value < 0
    {
        return Err(format!("min_age_days 不能为负（当前 {value}）"));
    }
    if let Some(value) = max_age_days
        && value < 0
    {
        return Err(format!("max_age_days 不能为负（当前 {value}）"));
    }
    if let (Some(from), Some(to)) = (min_age_days, max_age_days)
        && from > to
    {
        return Err(format!("库龄区间反了：min_age_days = {from} > max_age_days = {to}"));
    }
    Ok(())
}

/// 把库龄区间折算成**入库日期**的闭区间 `(from, to)`。
///
/// `min ≤ as_of − inbound ≤ max` ⇔ `as_of − max ≤ inbound ≤ as_of − min`。
///
/// 这条等价关系是 SQL 侧唯一做日期算术的地方（`inbound_date >= /*from*/ AND <= /*to*/`），
/// 单测逐天对拍它与 [`age_days_of`] 的结果（`aging_tests::bounds_match_age_days`）。
pub fn inbound_bounds(
    as_of: NaiveDate,
    min_age_days: Option<i64>,
    max_age_days: Option<i64>,
) -> (Option<NaiveDate>, Option<NaiveDate>) {
    let from = max_age_days.map(|days| as_of - Duration::days(days));
    let to = min_age_days.map(|days| as_of - Duration::days(days));
    (from, to)
}

/// 批号筛选归一化：去空白、空串 → `None`（= 不筛）。
pub fn normalize_lot_no(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    /// 库龄就是「基准日 − 入库日」的整日数，跨月 / 跨年都不许差一天。
    #[test]
    fn age_days_is_calendar_day_distance() {
        assert_eq!(age_days_of(date("2026-11-16"), date("2026-11-16")), 0);
        assert_eq!(age_days_of(date("2026-11-16"), date("2026-11-15")), 1);
        assert_eq!(age_days_of(date("2026-11-16"), date("2026-10-15")), 32);
        assert_eq!(age_days_of(date("2026-11-16"), date("2025-11-16")), 365);
        // 负数不夹 0（补录到未来的入库是真数据）
        assert_eq!(age_days_of(date("2026-11-16"), date("2026-11-17")), -1);
        println!("[M2-08] age_days = as_of − inbound_date（含负数）✓");
    }

    /// 区间折算与逐天判档**必须**等价：0..=400 每天都比一遍。
    #[test]
    fn bounds_match_age_days() {
        let as_of = date("2026-11-16");
        for min in [None, Some(0), Some(1), Some(31)] {
            for max in [None, Some(30), Some(60), Some(400)] {
                if let (Some(from), Some(to)) = (min, max)
                    && from > to
                {
                    continue;
                }
                let (from_date, to_date) = inbound_bounds(as_of, min, max);
                for age in -5..=420 {
                    let inbound = as_of - Duration::days(age);
                    let want = min.is_none_or(|value| age >= value) && max.is_none_or(|value| age <= value);
                    let got =
                        from_date.is_none_or(|bound| inbound >= bound) && to_date.is_none_or(|bound| inbound <= bound);
                    assert_eq!(
                        got, want,
                        "min={min:?} max={max:?} age={age} 折算区间判定不一致（from={from_date:?} to={to_date:?}）"
                    );
                }
            }
        }
        println!("[M2-08] inbound_bounds ⇔ age_days 逐天等价（-5..=420 × 12 组区间）✓");
    }

    /// 非法区间必须报错且**点名**是哪个字段（静默忽略 = 返回全量假数据）。
    #[test]
    fn invalid_age_ranges_are_rejected() {
        assert!(ensure_age_range(None, None).is_ok());
        assert!(ensure_age_range(Some(0), Some(0)).is_ok());
        assert!(ensure_age_range(Some(30), Some(60)).is_ok());

        let negative_min = ensure_age_range(Some(-1), None).expect_err("负界必须报错");
        assert!(negative_min.contains("min_age_days"), "{negative_min}");
        let negative_max = ensure_age_range(None, Some(-5)).expect_err("负界必须报错");
        assert!(negative_max.contains("max_age_days"), "{negative_max}");
        let reversed = ensure_age_range(Some(60), Some(30)).expect_err("反区间必须报错");
        assert!(
            reversed.contains("min_age_days") && reversed.contains("max_age_days"),
            "{reversed}"
        );
        println!("[M2-08] 负数界 / 反区间 → 400 文案点名字段 ✓");
    }

    /// 批号筛选：空白等于不筛（不能变成 `lot_no = ''` 把有批号的层全滤掉）。
    #[test]
    fn lot_no_filter_normalizes_blanks() {
        assert_eq!(normalize_lot_no(None), None);
        assert_eq!(normalize_lot_no(Some("")), None);
        assert_eq!(normalize_lot_no(Some("   ")), None);
        assert_eq!(normalize_lot_no(Some(" LOT-01 ")).as_deref(), Some("LOT-01"));
        println!("[M2-08] lot_no 空白归一化成「不筛」✓");
    }

    /// 基准日留空取 CST 今天（不是 UTC 今天：跨零点差一天会让「今天入库」变成库龄 1）。
    #[test]
    fn default_as_of_is_cst_today() {
        let today = default_as_of();
        let cst_today = (Utc::now() + Duration::hours(8)).date_naive();
        assert_eq!(today, cst_today);
        println!("[M2-08] 默认基准日 = CST 今天（{today}）✓");
    }
}
