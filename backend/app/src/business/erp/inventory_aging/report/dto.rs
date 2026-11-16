//! 库龄分段报表（M2-09）的契约 + 纯逻辑：档位表 / 入参 / 出参 / 透视 / 合计 / 截断。
//!
//! 卡：t_4e2cb6f6（M2-09【M2·仓库】库龄分析：报表（0-30/31-60/61-90/90+））。
//! 拆卡依据 `docs/planning/20261113-新卡清单.md` §M2 的 M2-09 行 —— 本卡只做**报表**：
//! 计算（M2-08，库函数 `biz_inventory_aging_layers`）、呆滞料（M2-10）、单测扩充（M2-11）、
//! 前端页（M2-12）都不在本卡。
//!
//! # 这张报表是什么
//!
//! 「仓里的结存，压了多久？」—— 按 **物料 × 仓库** 汇总，把结存数量摊到四个库龄档：
//!
//! | 档位键 | 名称 | 库龄（天） |
//! |:--|:--|:--|
//! | `D0_30`   | 0-30 天    | 0 – 30 |
//! | `D31_60`  | 31-60 天   | 31 – 60 |
//! | `D61_90`  | 61-90 天   | 61 – 90 |
//! | `OVER_90` | 90 天以上  | ≥ 91 |
//!
//! 数据来自 [`super::model::cells`]，而它只是
//! `biz_inventory_aging_layers(p_as_of, p_material_id, p_warehouse_id)`（V0424 · M2-08）
//! 外面套一层 `GROUP BY`。**本模块没有第二份 FIFO**：出库吃掉哪些层、每层剩多少，
//! 全部由库函数算完再交给这里分组（M2-08 定下的口径，M2-09 只许复用）。
//!
//! # 档位边界为什么这样切（0-30 / 31-60 / 61-90 / 90 天以上）
//!
//! 四档是卡面钉死的，对应仓管 / 财务对呆滞节奏的共识：30 天内是正常周转，
//! 31-60 开始要盯，61-90 要催销 / 催用，90 天以上进呆滞候选（M2-10 的呆滞料标记
//! 就是在同一批层上按天数阈值再切一刀）。边界口径：
//!
//! * **含两端**：0 → `D0_30`、30 → `D0_30`、31 → `D31_60`、90 → `D61_90`、91 → `OVER_90`；
//! * 「90 天以上」= **> 90**（91 起），与应付 / 应收账龄「365 天以上 = ≥ 366」同一写法，
//!   避免 90 那天同时落在 `D61_90` 和 `OVER_90` 两侧；
//! * 边界全部由 [`LAYER_BUCKETS`] 一张表定义：SQL 的 `CASE` 由
//!   [`payable_aging::bucket_case_sql_in`] 按同一张表生成，Rust 侧用
//!   [`payable_aging::bucket_of_in`]；改阈值只改这张表，两边不会漂。
//!
//! # 库龄不会为负（与 M2-08 的 `age_days` 不一样，别搞混）
//!
//! M2-08 的出参可以让 `age_days` 是负数（补录到**未来**的入库），并明写「不夹 0」。
//! 报表这一层不会出现负数，因为库函数自己就只回 `biz_date <= p_as_of` 的层
//! （V0424 的 `within` CTE）—— 未来的入库根本不在基准日的快照里。
//! 这里仍按「≤ 30 进 `D0_30`」写 `CASE`：万一以后库函数改了快照口径，
//! 负数库龄会落进第一档而不是**被静默丢掉**（丢数量比错分档更严重）。
//!
//! # 合计为什么一定要等于各档之和
//!
//! 报表的可信度全靠一条恒等式：`Σ各档数量 == Σ行数量 == Σ层剩余`。
//! 所以：
//!
//! * 分档在 SQL 里做（`GROUP BY 物料, 仓库, 档位`），**行合计 = Σ本行各档**（不是在后面另算一遍）；
//! * 全表合计 = Σ各行（[`summarise_rows`]），**不另发一条 SQL 加总** —— 两条路径各算一次
//!   迟早会漂，而报表数字对不上时没人知道该信哪条；
//! * 行上限（[`ROW_LIMIT`]）在**合计之后**才截：[`truncate_rows`] 只砍 `rows`，
//!   档位合计与总量是全量的。否则「明细截断了，合计也跟着少」就是一张假报表。
//!
//! # 金额是**参考金额**，不是账面金额
//!
//! `amount_total` / 各档 `amount` = `Σ(层剩余数量 × 层入库单价)`（4 位小数）。
//! 层单价是入库时的历史成本快照，`biz_inventory.avg_cost` 是移动加权 —— 两者本来就不相等
//! （移动加权下账面金额不按层拆）。库龄是**数量**分析，金额列只作参考，表头也不写「金额」。

use std::collections::HashMap;

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::payable::payable_aging::{self, AgingBucketDef, BucketSpec};

// ==================== 常量 ====================

/// 报表编码（与前端 `report_code` 对应）。
pub const REPORT_CODE: &str = "INVENTORY_AGING";

/// 报表中文名（页头 / 导出文件名）。
pub const REPORT_NAME: &str = "库存库龄分析表";

/// 行上限（物料 × 仓库 的组合数；**合计永远是全量**，不受此限）。
///
/// 2000 行够一家中型制造企业的物料 × 仓库组合；超了说明该按物料分类 / 仓库先收窄
/// （出参里 `rows_truncated = true` + `hints` 明写被截了，绝不静默少行）。
pub const ROW_LIMIT: usize = 2000;

/// **库龄档位表 —— 库存库龄全库唯一一份**（4 档：0-30 / 31-60 / 61-90 / 90 天以上）。
///
/// 与应付 / 应收账龄的 [`payable_aging::AGING_BUCKETS`]（逾期天数 7 档）**不是同一张表**：
/// 那张表的语义是「到期后拖了多久」，这张是「入库后压了多久」，阈值与档数都不同，
/// 硬套会把「未到期 / 365 天以上」这类档挂在库龄报表上。
/// **算法**不重复：`bucket_of_in` / `bucket_case_sql_in` / `bucket_defs` 直接用 payable 那份
/// （`BucketSpec` 只是「一张档位表」的形状，不是阈值）。
///
/// `min_days` 是**文档下界**（第一档填 0）：判定实际只比较 `max_days`（`<= 30` / `<= 60` /
/// `<= 90` / `ELSE`），所以万一出现负库龄（库函数理论上不会给，见下面「库龄不会为负」），
/// 它落进第一档而不是掉到表外 —— 表头仍显示「0-30 天」。
pub const LAYER_BUCKETS: [BucketSpec; 4] = [
    BucketSpec {
        key: "D0_30",
        label: "0-30 天",
        min_days: 0,
        max_days: Some(30),
    },
    BucketSpec {
        key: "D31_60",
        label: "31-60 天",
        min_days: 31,
        max_days: Some(60),
    },
    BucketSpec {
        key: "D61_90",
        label: "61-90 天",
        min_days: 61,
        max_days: Some(90),
    },
    BucketSpec {
        key: "OVER_90",
        label: "90 天以上",
        min_days: 91,
        max_days: None,
    },
];

// ==================== 入参 ====================

/// 库龄分段报表入参（`GET /business/erp/inventory-aging/report`）。
///
/// 三个字段都可留空：`as_of` 留空取「今天（CST）」，物料 / 仓库留空 = 全量。
/// 报表本身是**汇总**，不做分页（要钻取用 M2-08 的批次层分页接口）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct InventoryAgingReportQuery {
    /// 基准日（`yyyy-MM-dd`，含当天；留空 = 今天 CST）。
    pub as_of: Option<String>,
    /// 物料 id（精确 UUID；留空 = 全部物料）。
    pub material_id: Option<String>,
    /// 仓库 id（精确 UUID；留空 = 全部仓库）。
    pub warehouse_id: Option<String>,
}

// ==================== 出参 ====================

/// 一个档位在一行里的数量 / 金额 / 层数。
#[derive(Debug, Clone, Serialize)]
pub struct AgingBucketQtyRes {
    /// 档位键（[`LAYER_BUCKETS`] 之一）。
    pub key: String,
    /// 档位中文名（表头）。
    pub label: String,
    /// 本档的层数（有几层入库批次落在这里）。
    pub layer_count: i64,
    /// 本档剩余数量合计。
    pub qty: Decimal,
    /// 本档参考金额合计（= Σ 层剩余 × 层单价；**不是**账面金额）。
    pub amount: Decimal,
}

/// 报表里的一行 = 一个 物料 × 仓库 组合。
///
/// 名字（`material_*` / `warehouse_*`）是**主数据关联**（AGENTS.md 约定 13）：
/// 实时取「现在叫什么」，主数据软删后为 `null`，历史库龄行照常可读。
#[derive(Debug, Clone, Serialize)]
pub struct MaterialWarehouseAgingRes {
    /// 物料 id。
    pub material_id: String,
    /// 物料编码（软删后 `null`）。
    pub material_code: Option<String>,
    /// 物料名称（软删后 `null`）。
    pub material_name: Option<String>,
    /// 规格型号（软删后 `null`）。
    pub spec: Option<String>,
    /// 计量单位名（软删后 `null`）。
    pub unit_name: Option<String>,
    /// 仓库 id。
    pub warehouse_id: String,
    /// 仓库编码（软删后 `null`）。
    pub warehouse_code: Option<String>,
    /// 仓库名称（软删后 `null`）。
    pub warehouse_name: Option<String>,
    /// 本行层数（还有结存的层）。
    pub layer_count: i64,
    /// 本行剩余数量合计（= Σ 各档 `qty`）。
    pub qty_total: Decimal,
    /// 本行参考金额合计（= Σ 各档 `amount`）。
    pub amount_total: Decimal,
    /// 各档（按键位表顺序，缺档给 0 —— 空档不是「未知」而是「这一档真的没有货」）。
    pub buckets: Vec<AgingBucketQtyRes>,
    /// 本行最老那层还剩货的入库日期。
    pub oldest_inbound_date: NaiveDate,
    /// 本行最大库龄（天）= 最老那层的库龄。
    pub max_age_days: i64,
    /// 数量加权平均库龄（天，2 位小数）= Σ(层剩余 × 层库龄) / Σ层剩余。
    ///
    /// 为什么要它：`max_age_days` 是「最老那层」的极端值，一个 90 天前入 1 件的层
    /// 会把整行拉成「90 天」；加权平均才回答「这批货平均压了多久」。
    pub avg_age_days: Decimal,
}

/// 库龄分段报表出参。
#[derive(Debug, Clone, Serialize)]
pub struct InventoryAgingReportRes {
    /// 报表编码（[`REPORT_CODE`]）。
    pub report_code: String,
    /// 报表名称（[`REPORT_NAME`]）。
    pub report_name: String,
    /// 基准日（回显：调用方不用自己猜「今天」是哪天）。
    pub as_of: NaiveDate,
    /// 基准日来源：`QUERY`（请求里传了 `as_of`）/ `TODAY`（留空 → 今天 CST）。
    ///
    /// 为什么要回显来源：前端页头要能说清「这张表是几号的」，用户传了 `as_of` 却没生效
    /// （或被静默忽略）时，只有这一列能看出来 —— 与应收 / 应付账龄的 `as_of_source` 同口径。
    pub as_of_source: String,
    /// 档位定义（渲染表头用；前端按它出列，不写死 4 列）。
    pub buckets: Vec<AgingBucketDef>,
    /// 全表各档合计（按键位表顺序）。
    pub summary: Vec<AgingBucketQtyRes>,
    /// 全表剩余数量合计（= Σ `summary[].qty`；跨物料时**单位不同，只作总量参考**）。
    pub qty_total: Decimal,
    /// 全表参考金额合计（= Σ `summary[].amount`）。
    pub amount_total: Decimal,
    /// 全表层数（还有结存的层）。
    pub layer_count: i64,
    /// 有结存的 物料 × 仓库 行数（**截断前**的总数）。
    pub row_count: i64,
    /// 涉及物料数。
    pub material_count: i64,
    /// 涉及仓库数。
    pub warehouse_count: i64,
    /// 行上限（[`ROW_LIMIT`]）。
    pub row_limit: u64,
    /// `rows` 是否被截断（`summary` / 总量不受影响）。
    pub rows_truncated: bool,
    /// 明细行（按「最老货在前」= `oldest_inbound_date` 升序 → 数量降序 → id）。
    pub rows: Vec<MaterialWarehouseAgingRes>,
    /// 口径提示（前端直接展示在表下方，不塞脚注里）。
    pub hints: Vec<String>,
}

// ==================== 取数中间结构 ====================

/// SQL 聚合出来的一格：某个 物料 × 仓库 × 档位。
#[derive(Debug, Clone)]
pub struct CellRow {
    /// 物料。
    pub material_id: Uuid,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 档位键（SQL 按 [`LAYER_BUCKETS`] 生成的 `CASE` 回的字面量）。
    pub bucket: String,
    /// 层数。
    pub layer_count: i64,
    /// 剩余数量合计。
    pub qty: Decimal,
    /// 参考金额合计。
    pub amount: Decimal,
    /// 本格最老的入库日期。
    pub oldest_inbound_date: NaiveDate,
    /// 本格最大库龄（天）。
    pub max_age_days: i64,
    /// 本格数量加权平均库龄（天，**未四舍五入**：行级平均要再用它按数量加权，
    /// 提前舍入会把误差带进行平均）。
    pub avg_age_days: Decimal,
}

// ==================== 纯逻辑 ====================

/// 档位键 → 中文名（找不到时回退成键本身，不 panic）。
pub fn bucket_label(key: &str) -> &str {
    LAYER_BUCKETS
        .iter()
        .find(|bucket| bucket.key == key)
        .map(|bucket| bucket.label)
        .unwrap_or(key)
}

/// 库龄天数 → 档位键（按 [`LAYER_BUCKETS`]，与 SQL 侧同源）。
pub fn bucket_of(age_days: i64) -> &'static str {
    payable_aging::bucket_of_in(&LAYER_BUCKETS, age_days)
}

/// 档位定义出参（表头），按 [`LAYER_BUCKETS`] 生成。
pub fn bucket_defs() -> Vec<AgingBucketDef> {
    payable_aging::bucket_defs(&LAYER_BUCKETS)
}

/// 全 0 的档位行（行 / 汇总的初值），按 [`LAYER_BUCKETS`] 生成。
pub fn empty_bucket_cells() -> Vec<AgingBucketQtyRes> {
    LAYER_BUCKETS
        .iter()
        .map(|bucket| AgingBucketQtyRes {
            key: bucket.key.to_string(),
            label: bucket.label.to_string(),
            layer_count: 0,
            qty: Decimal::ZERO,
            amount: Decimal::ZERO,
        })
        .collect()
}

/// 透视用的累加器（`avg_age_days` 要在最后按数量加权算，所以先攒分子）。
struct RowAcc {
    material_id: Uuid,
    warehouse_id: Uuid,
    layer_count: i64,
    qty_total: Decimal,
    amount_total: Decimal,
    buckets: Vec<AgingBucketQtyRes>,
    oldest_inbound_date: NaiveDate,
    max_age_days: i64,
    /// Σ(本档数量 × 本档平均库龄) —— 行级加权平均的分子。
    age_weight: Decimal,
}

/// 把「物料 × 仓库 × 档位」的格子透视成行（纯逻辑，不碰库）。
///
/// * 行合计 = Σ本行各档（同一个数算一遍，不在后面另加）；
/// * 未知档位键（正常不会出现：SQL 的 `CASE` 也是按本表生成的）落进**最后一档** ——
///   与 SQL 里 `ELSE '<最后一档>'` 同义，宁可归到「90 天以上」也不丢数量；
/// * 行 `avg_age_days` = Σ(档数量 × 档平均库龄) / Σ档数量（未舍入的档平均，见 [`CellRow`]）。
pub fn pivot_rows(cells: Vec<CellRow>) -> Vec<MaterialWarehouseAgingRes> {
    let mut index: HashMap<(Uuid, Uuid), RowAcc> = HashMap::new();
    let mut order: Vec<(Uuid, Uuid)> = Vec::new();

    for cell in cells {
        let key = (cell.material_id, cell.warehouse_id);
        let acc = index.entry(key).or_insert_with(|| {
            order.push(key);
            RowAcc {
                material_id: cell.material_id,
                warehouse_id: cell.warehouse_id,
                layer_count: 0,
                qty_total: Decimal::ZERO,
                amount_total: Decimal::ZERO,
                buckets: empty_bucket_cells(),
                oldest_inbound_date: cell.oldest_inbound_date,
                max_age_days: 0,
                age_weight: Decimal::ZERO,
            }
        });

        // 未知键 → 最后一档（与 SQL 的 ELSE 同义）
        let slot = acc
            .buckets
            .iter()
            .position(|bucket| bucket.key == cell.bucket)
            .unwrap_or_else(|| acc.buckets.len() - 1);
        acc.buckets[slot].layer_count += cell.layer_count;
        acc.buckets[slot].qty += cell.qty;
        acc.buckets[slot].amount += cell.amount;

        acc.layer_count += cell.layer_count;
        acc.qty_total += cell.qty;
        acc.amount_total += cell.amount;
        acc.age_weight += cell.qty * cell.avg_age_days;
        if cell.oldest_inbound_date < acc.oldest_inbound_date {
            acc.oldest_inbound_date = cell.oldest_inbound_date;
        }
        acc.max_age_days = acc.max_age_days.max(cell.max_age_days);
    }

    let mut rows: Vec<MaterialWarehouseAgingRes> = order
        .into_iter()
        .filter_map(|key| index.remove(&key))
        .map(|acc| {
            let avg_age_days = if acc.qty_total.is_zero() {
                Decimal::ZERO
            } else {
                (acc.age_weight / acc.qty_total).round_dp(2)
            };
            MaterialWarehouseAgingRes {
                material_id: acc.material_id.to_string(),
                material_code: None,
                material_name: None,
                spec: None,
                unit_name: None,
                warehouse_id: acc.warehouse_id.to_string(),
                warehouse_code: None,
                warehouse_name: None,
                layer_count: acc.layer_count,
                qty_total: payable_aging::money(acc.qty_total),
                amount_total: payable_aging::money(acc.amount_total),
                buckets: acc
                    .buckets
                    .into_iter()
                    .map(|mut cell| {
                        cell.qty = payable_aging::money(cell.qty);
                        cell.amount = payable_aging::money(cell.amount);
                        cell
                    })
                    .collect(),
                oldest_inbound_date: acc.oldest_inbound_date,
                max_age_days: acc.max_age_days,
                avg_age_days,
            }
        })
        .collect();

    sort_rows(&mut rows);
    rows
}

/// 行顺序：**最老货在前**（库龄分析先看压得久的）→ 数量降序 → 物料 id → 仓库 id（全序，
/// 同一批数据每次出来的顺序都一样，前端表格不会「刷新一下就换行」）。
pub fn sort_rows(rows: &mut [MaterialWarehouseAgingRes]) {
    rows.sort_by(|left, right| {
        left.oldest_inbound_date
            .cmp(&right.oldest_inbound_date)
            .then_with(|| right.qty_total.cmp(&left.qty_total))
            .then_with(|| left.material_id.cmp(&right.material_id))
            .then_with(|| left.warehouse_id.cmp(&right.warehouse_id))
    });
}

/// 全表各档合计 = 各行按档加总（**不另发 SQL 加总**，见模块头注释）。
pub fn summarise_rows(rows: &[MaterialWarehouseAgingRes]) -> Vec<AgingBucketQtyRes> {
    let mut out = empty_bucket_cells();
    for row in rows {
        for (target, cell) in out.iter_mut().zip(row.buckets.iter()) {
            target.layer_count += cell.layer_count;
            target.qty += cell.qty;
            target.amount += cell.amount;
        }
    }
    for cell in &mut out {
        cell.qty = payable_aging::money(cell.qty);
        cell.amount = payable_aging::money(cell.amount);
    }
    out
}

/// 按 [`ROW_LIMIT`] 截断行（**只在合计算完之后**调用），返回是否被截断。
pub fn truncate_rows(rows: &mut Vec<MaterialWarehouseAgingRes>) -> bool {
    if rows.len() <= ROW_LIMIT {
        return false;
    }
    rows.truncate(ROW_LIMIT);
    true
}

/// 行里出现的物料数 / 仓库数（**截断前**统计）。
pub fn distinct_counts(rows: &[MaterialWarehouseAgingRes]) -> (i64, i64) {
    let materials: std::collections::HashSet<&str> = rows.iter().map(|row| row.material_id.as_str()).collect();
    let warehouses: std::collections::HashSet<&str> = rows.iter().map(|row| row.warehouse_id.as_str()).collect();
    (
        i64::try_from(materials.len()).unwrap_or(0),
        i64::try_from(warehouses.len()).unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    fn cell(material: u128, warehouse: u128, bucket: &str, qty: &str, avg_age: &str) -> CellRow {
        CellRow {
            material_id: Uuid::from_u128(material),
            warehouse_id: Uuid::from_u128(warehouse),
            bucket: bucket.to_string(),
            layer_count: 1,
            qty: dec(qty),
            amount: dec(qty),
            oldest_inbound_date: date("2026-10-15"),
            max_age_days: 32,
            avg_age_days: dec(avg_age),
        }
    }

    /// 四个档位的边界逐个钉死（含两端；「90 天以上」= 91 起）。
    #[test]
    fn bucket_boundaries_are_inclusive_and_non_overlapping() {
        for (days, key) in [
            (0, "D0_30"),
            (30, "D0_30"),
            (31, "D31_60"),
            (60, "D31_60"),
            (61, "D61_90"),
            (90, "D61_90"),
            (91, "OVER_90"),
            (365, "OVER_90"),
            (4000, "OVER_90"),
        ] {
            assert_eq!(bucket_of(days), key, "{days} 天应该落在 {key}");
        }
        // 每个非负天数都有且只有一个档位（档位表覆盖 0 起全区间，没有空档）
        for days in 0..=400 {
            let key = bucket_of(days);
            let hits = LAYER_BUCKETS
                .iter()
                .filter(|bucket| days >= bucket.min_days && bucket.max_days.is_none_or(|max| days <= max))
                .count();
            assert_eq!(hits, 1, "{days} 天命中 {hits} 个档位（应为 1，键 {key}）");
        }
        println!("[M2-09] 档位边界：0/30→D0_30、31/60→D31_60、61/90→D61_90、91+→OVER_90 ✓");
    }

    /// 负数库龄落进第一档而**不是被丢掉**（库函数目前不会给出负数，但口径要兜住）。
    #[test]
    fn negative_age_falls_into_first_bucket() {
        assert_eq!(bucket_of(-1), "D0_30");
        assert_eq!(bucket_of(-999), "D0_30");
        println!("[M2-09] 负数库龄 → D0_30（不丢数量）✓");
    }

    /// 透视：同一 物料 × 仓库 的多档合并成一行，行合计 = Σ各档。
    #[test]
    fn pivot_merges_buckets_into_one_row() {
        let rows = pivot_rows(vec![
            cell(0x11, 0x22, "D31_60", "120", "32"),
            cell(0x11, 0x22, "D0_30", "50", "6"),
        ]);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.qty_total, dec("170"));
        assert_eq!(row.layer_count, 2);
        let by_key: HashMap<&str, Decimal> = row.buckets.iter().map(|c| (c.key.as_str(), c.qty)).collect();
        assert_eq!(by_key["D0_30"], dec("50"));
        assert_eq!(by_key["D31_60"], dec("120"));
        assert_eq!(by_key["D61_90"], Decimal::ZERO, "缺档给 0 而不是 NULL");
        assert_eq!(by_key["OVER_90"], Decimal::ZERO);
        // 加权平均：(120×32 + 50×6) / 170 = 4140 / 170 = 24.35（2 位小数）
        assert_eq!(row.avg_age_days, dec("24.35"));
        println!("[M2-09] 透视：两档合成一行，合计 170，加权平均库龄 24.35 天 ✓");
    }

    /// 不同物料 / 不同仓库**不许**合并（库龄按 物料 × 仓库 分格）。
    #[test]
    fn pivot_keeps_material_and_warehouse_partitions_apart() {
        let rows = pivot_rows(vec![
            cell(0x11, 0x22, "D0_30", "10", "1"),
            cell(0x11, 0x33, "D0_30", "20", "2"),
            cell(0x44, 0x22, "D0_30", "30", "3"),
        ]);
        assert_eq!(rows.len(), 3, "物料/仓库任一不同就是不同的行");
        assert_eq!(distinct_counts(&rows), (2, 2));
        println!("[M2-09] 分格：3 格不合并，物料 2 / 仓库 2 ✓");
    }

    /// 未知档位键落进最后一档（与 SQL 的 ELSE 同义），数量不许丢。
    #[test]
    fn unknown_bucket_key_folds_into_last_bucket() {
        let rows = pivot_rows(vec![cell(0x11, 0x22, "SOMETHING_ELSE", "77", "120")]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].qty_total, dec("77"));
        let last = rows[0].buckets.last().expect("最后一档");
        assert_eq!(last.key, "OVER_90");
        assert_eq!(last.qty, dec("77"), "未知键的数量要落进最后一档，不许丢");
        println!("[M2-09] 未知档位键 → 最后一档（数量不丢）✓");
    }

    /// 全表合计 = Σ各行各档（同一条路径算出来的，不另发 SQL）。
    #[test]
    fn summary_equals_sum_of_rows() {
        let rows = pivot_rows(vec![
            cell(0x11, 0x22, "D0_30", "10", "1"),
            cell(0x11, 0x22, "OVER_90", "5", "100"),
            cell(0x44, 0x22, "D61_90", "7", "75"),
        ]);
        let summary = summarise_rows(&rows);
        let row_qty: Decimal = rows.iter().map(|row| row.qty_total).sum();
        let summary_qty: Decimal = summary.iter().map(|cell| cell.qty).sum();
        assert_eq!(row_qty, dec("22"));
        assert_eq!(summary_qty, row_qty, "Σ档位必须等于 Σ行");
        let row_layers: i64 = rows.iter().map(|row| row.layer_count).sum();
        let summary_layers: i64 = summary.iter().map(|cell| cell.layer_count).sum();
        assert_eq!(row_layers, summary_layers);
        println!("[M2-09] 合计恒等式：Σ档位 {summary_qty} == Σ行 {row_qty} ✓");
    }

    /// 行顺序：最老货在前；同最老日期时数量大的在前；再按 id 收全序。
    #[test]
    fn rows_are_oldest_first_then_biggest() {
        let mut older = cell(0x11, 0x22, "OVER_90", "5", "120");
        older.oldest_inbound_date = date("2026-01-01");
        let mut newer_big = cell(0x44, 0x22, "D0_30", "900", "1");
        newer_big.oldest_inbound_date = date("2026-11-01");
        let mut newer_small = cell(0x55, 0x22, "D0_30", "1", "1");
        newer_small.oldest_inbound_date = date("2026-11-01");
        let rows = pivot_rows(vec![newer_small, newer_big, older]);
        let ids: Vec<&str> = rows.iter().map(|row| row.material_id.as_str()).collect();
        assert_eq!(
            ids,
            [
                Uuid::from_u128(0x11).to_string(),
                Uuid::from_u128(0x44).to_string(),
                Uuid::from_u128(0x55).to_string(),
            ]
        );
        println!("[M2-09] 行序：最老在前 → 数量降序 → id ✓");
    }

    /// 行上限只砍行；**合计是截断前算的**（截断了也要如实标 `rows_truncated`）。
    #[test]
    fn row_limit_truncates_rows_but_not_totals() {
        let mut rows: Vec<MaterialWarehouseAgingRes> = (0..(ROW_LIMIT as u128 + 5))
            .map(|index| cell(0x1000 + index, 0x22, "D0_30", "1", "1"))
            .map(|cell| MaterialWarehouseAgingRes {
                material_id: cell.material_id.to_string(),
                material_code: None,
                material_name: None,
                spec: None,
                unit_name: None,
                warehouse_id: cell.warehouse_id.to_string(),
                warehouse_code: None,
                warehouse_name: None,
                layer_count: 1,
                qty_total: cell.qty,
                amount_total: cell.amount,
                buckets: {
                    let mut buckets = empty_bucket_cells();
                    buckets[0].qty = cell.qty;
                    buckets[0].layer_count = 1;
                    buckets
                },
                oldest_inbound_date: cell.oldest_inbound_date,
                max_age_days: cell.max_age_days,
                avg_age_days: cell.avg_age_days,
            })
            .collect();
        let summary_before = summarise_rows(&rows);
        let total_before: Decimal = summary_before.iter().map(|cell| cell.qty).sum();
        let truncated = truncate_rows(&mut rows);
        assert!(truncated, "超上限必须标记截断");
        assert_eq!(rows.len(), ROW_LIMIT);
        assert_eq!(
            total_before,
            dec(&format!("{}", ROW_LIMIT + 5)),
            "合计是截断前的全量（{} 行）",
            ROW_LIMIT + 5
        );
        println!("[M2-09] 截断：行 {ROW_LIMIT} 条，合计仍是全量 {total_before} ✓");
    }

    /// 档位中文名与键一一对应（授权树 / 表头用中文，接口用键）。
    #[test]
    fn bucket_labels_match_keys() {
        assert_eq!(bucket_label("D0_30"), "0-30 天");
        assert_eq!(bucket_label("D31_60"), "31-60 天");
        assert_eq!(bucket_label("D61_90"), "61-90 天");
        assert_eq!(bucket_label("OVER_90"), "90 天以上");
        assert_eq!(bucket_label("NOPE"), "NOPE", "找不到就回退成键，不 panic");
        assert_eq!(bucket_defs().len(), LAYER_BUCKETS.len());
        assert_eq!(REPORT_CODE, "INVENTORY_AGING");
        assert_eq!(REPORT_NAME, "库存库龄分析表");
        println!("[M2-09] 档位中文名 / 报表编码 ✓");
    }
}
