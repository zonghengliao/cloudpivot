//! 呆滞料标记（M2-10）的契约 + 纯逻辑：阈值解析 / 判定 / 行排序 / 截断 / 合计。
//!
//! 卡：t_bb0fe3c6（M2-10【M2·仓库】库龄：呆滞料标记（超 N 天无出库））；拆卡依据
//! `docs/planning/20261113-新卡清单.md` §M2 的 M2-10 行 —— 计算（M2-08）、分段报表
//! （M2-09）、单测扩充（M2-11）、前端页（M2-12）都不在本卡。
//!
//! # 这张表是什么
//!
//! 「仓里哪些货压着不动、压了多少钱？」—— 把每个 **物料 × 仓库** 格里**库龄超过 N 天
//! 仍有剩余**的入库层挑出来，按格汇总成一行；N 是**可配置阈值**（见下面三条优先级）。
//!
//! | 出参列 | 含义 |
//! |:--|:--|
//! | `qty_on_hand` | 本格全部有结存的层剩余数量（呆滞 + 不呆滞） |
//! | `qty_slow`    | 其中**呆滞层**的剩余数量 |
//! | `slow_ratio`  | `qty_slow / qty_on_hand`（4 位小数） |
//! | `amount_slow` | 呆滞层参考金额（Σ 层剩余 × 层入库单价） |
//!
//! 只返回 `qty_slow > 0` 的格 —— 这是「标记」的结果集；全库有多少格、总占压多少，
//! 在 `summary.scanned_row_count` / `summary.qty_on_hand` 里给全（**截断前**统计）。
//!
//! # 判定口径：「超 N 天无出库」按**批次库龄**算（写死，别在别处再解释）
//!
//! 一个入库层算呆滞 ⟺ 它**还有剩余**（`qty_remaining > 0`）且 `基准日 − 入库业务日期 > N`。
//!
//! 为什么**不**用「这一格最后一次出库距今 > N 天」：
//!
//! * 那一刀会把**新到的货**一起判成呆滞 —— 一整年没动过的物料，今天刚进一批，整格
//!   「最后一次出库」还是 300 天前，新货也跟着上榜；
//! * 反过来，200 天前的老货只要有一笔小额出库（FIFO 先吃老层），「最后一次出库」
//!   就变成昨天，老货被**洗白** —— 而账面上一分钱没少压。
//!
//! 批次口径正好回答「哪一批货压了多久」：老层没被出库吃掉，它就一直挂着。这也是
//! V0424 的库函数 `biz_inventory_aging_layers(...)` 的语义（层 = 每笔 `IN` 流水）。
//!
//! 边界：`库龄 > N`（严格大于）—— 与库龄报表的档位口径一致（「90 天以上」= 91 起）。
//! 所以**缺省 N = 90 时，本表的「呆滞」与 M2-09 报表的「90 天以上」档逐格相等**，
//! 真库用例 `default_threshold_matches_report_over_90` 拿两张表对拍钉住这件事。
//!
//! # 阈值 N 从哪来（三级，优先级从高到低）
//!
//! | 顺序 | 来源 | `threshold_source` | 说明 |
//! |:--:|:--|:--|:--|
//! | 1 | 请求参数 `days` | `QUERY` | 只想看「超 30 天」的临时视角，不改全局配置 |
//! | 2 | 系统参数 `erp.inventory.slow-moving-days` | `CONFIG` | 唯一的持久化配置（V0426 种 90） |
//! | 3 | 常量 [`DEFAULT_THRESHOLD_DAYS`] | `DEFAULT` | 没配 / 配了不可用 —— 后者在 `hints` 里明写 |
//!
//! **阈值只在时间上切一刀，不参与金额 / 数量计算**：改 N 只会改变「哪些层被算成呆滞」，
//! 不会改变层的剩余数量。`qty_on_hand` 与 `amount_on_hand` 因此与 N 无关。
//!
//! # 为什么不新开一张 `biz_slow_moving` 表
//!
//! 「呆滞」是库龄的一个**视图**（同一批层 + 一个天数阈值），不是独立事实：
//! 存表就要在每次出入库时维护它，而阈值一改全表过期。库龄层本身已经是**推导**的
//! （V0424 的库函数），所以标记也只能是推导的 —— 只读、零写 SQL、随查随算。
//!
//! # 金额是**参考金额**
//!
//! `amount_slow` = Σ(呆滞层剩余数量 × 层入库单价)，与移动加权账面金额本来就不相等
//! （与 M2-09 报表同一口径）。呆滞判定只看**库龄天数**，金额只回答「占压多少钱」。

use nik::prelude::rust_decimal::Decimal;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::inventory_aging::report;
use crate::business::erp::payable::payable_aging;

// ==================== 常量 ====================

/// 报表编码（与前端 `report_code` 对应；列表页 / 导出文件名共用）。
pub const SLOW_MOVING_CODE: &str = "INVENTORY_SLOW_MOVING";

/// 报表中文名。
pub const SLOW_MOVING_NAME: &str = "呆滞料清单";

/// 阈值的系统参数键（`sys_config.config_key`；V0426 种下，缺省值见
/// [`DEFAULT_THRESHOLD_DAYS`]）。改阈值走「系统参数」页（`/support/config`），
/// **不需要**新写一个 ERP 侧的配置接口：这一行就是一个普通参数。
pub const THRESHOLD_CONFIG_KEY: &str = "erp.inventory.slow-moving-days";

/// 缺省阈值（天）：与库龄报表的「90 天以上」档对齐（见模块头注释）。
pub const DEFAULT_THRESHOLD_DAYS: i64 = 90;

/// 阈值下界（含）：0 天 = 「所有还有结存的层都算呆滞」，那是「全部库存清单」，
/// 不是呆滞料标记 ⇒ 拒掉。
pub const MIN_THRESHOLD_DAYS: i64 = 1;

/// 阈值上界（含）：10 年。再大的阈值等于「永远不会呆滞」，明显是填错了。
pub const MAX_THRESHOLD_DAYS: i64 = 3650;

/// 行上限（物料 × 仓库 组合数）。
///
/// 与库龄报表**同一个上限**（直接引用 [`report::dto::ROW_LIMIT`]，不另写一个 2000）：
/// 两张表行粒度一致，上限漂了会出现「报表 2000 行、呆滞 3000 行」这种无解释的差异。
/// **合计永远是全量**，不受此限（出参 `rows_truncated` 明写）。
pub const ROW_LIMIT: usize = report::dto::ROW_LIMIT;

// ==================== 阈值 ====================

/// 阈值的来源（回显给前端，用户传了 `days` 却没生效时只有这列能看出来）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThresholdSource {
    /// 请求参数 `days`。
    Query,
    /// 系统参数 [`THRESHOLD_CONFIG_KEY`]。
    Config,
    /// 常量 [`DEFAULT_THRESHOLD_DAYS`]（没配 / 配了不可用）。
    Default,
}

impl ThresholdSource {
    /// 出参字面量（`QUERY` / `CONFIG` / `DEFAULT`）。
    pub fn as_str(self) -> &'static str {
        match self {
            ThresholdSource::Query => "QUERY",
            ThresholdSource::Config => "CONFIG",
            ThresholdSource::Default => "DEFAULT",
        }
    }
}

/// 解析结果：生效阈值 + 来源 + 配置坏掉时的告警文案（要进 `hints`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThresholdResolution {
    /// 生效阈值（天）。
    pub days: i64,
    /// 来源。
    pub source: ThresholdSource,
    /// 配置值存在但不可用时的告警（`None` = 配置正常或根本没配）。
    pub warning: Option<String>,
}

/// 请求参数 `days` 的解析：空白 → `None`；非整数 / 越界 → `Err(人话)`。
///
/// 越界**不静默夹取**：`days=0` 被悄悄改成 1、`days=99999` 被改成 3650，用户会以为
/// 「我查的就是 0 天 / 99999 天」——又一种假数据（与 M2-08 / M2-09 的筛选口径一致）。
pub fn parse_threshold_param(raw: Option<&str>) -> std::result::Result<Option<i64>, String> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let days: i64 = text.parse().map_err(|_| {
        format!("days 必须是整数（单位：天，范围 {MIN_THRESHOLD_DAYS}..={MAX_THRESHOLD_DAYS}），当前 `{text}`")
    })?;
    if !(MIN_THRESHOLD_DAYS..=MAX_THRESHOLD_DAYS).contains(&days) {
        return Err(format!(
            "days 必须在 {MIN_THRESHOLD_DAYS}..={MAX_THRESHOLD_DAYS} 之间（当前 {days}）：\
             0 天等于「全部库存」、超过 {MAX_THRESHOLD_DAYS} 天等于「永不呆滞」"
        ));
    }
    Ok(Some(days))
}

/// 阈值三级解析（纯函数，不碰库）：参数 → 系统参数 → 缺省。
///
/// `config` 传 [`THRESHOLD_CONFIG_KEY`] 的文本值（未配置 / 已停用 → `None`）。
/// 配置值的**存在但不可用**（非整数 / 越界）不静默忽略：回缺省 + 带一条 `warning`，
/// 由 handler 放进 `hints` —— 否则运维改了参数没生效会毫无线索（与应收账龄
/// `as_of_source` 同一个取向：生效与否必须看得见）。
pub fn resolve_threshold(param: Option<i64>, config: Option<&str>) -> ThresholdResolution {
    if let Some(days) = param {
        return ThresholdResolution {
            days,
            source: ThresholdSource::Query,
            warning: None,
        };
    }
    if let Some(raw) = config.map(str::trim).filter(|text| !text.is_empty()) {
        match raw.parse::<i64>() {
            Ok(days) if (MIN_THRESHOLD_DAYS..=MAX_THRESHOLD_DAYS).contains(&days) => {
                return ThresholdResolution {
                    days,
                    source: ThresholdSource::Config,
                    warning: None,
                };
            }
            _ => {
                return ThresholdResolution {
                    days: DEFAULT_THRESHOLD_DAYS,
                    source: ThresholdSource::Default,
                    warning: Some(format!(
                        "系统参数 {THRESHOLD_CONFIG_KEY} 的值 `{raw}` 不可用\
                         （必须是 {MIN_THRESHOLD_DAYS}..={MAX_THRESHOLD_DAYS} 的整数），\
                         本次按缺省 {DEFAULT_THRESHOLD_DAYS} 天判定 —— 请到「系统参数」页改正"
                    )),
                };
            }
        }
    }
    ThresholdResolution {
        days: DEFAULT_THRESHOLD_DAYS,
        source: ThresholdSource::Default,
        warning: None,
    }
}

/// 单层是否呆滞：`库龄 > 阈值`（严格大于）。
///
/// **SQL 侧的判据逐字是 `(基准日 − 入库日期) > 阈值`**（见 [`super::model::cells_sql`]），
/// 本函数是它在 Rust 侧的镜像，只给纯逻辑用例用；两边一起改才算改口径（真库用例
/// `threshold_boundary_exactly_n_days_is_not_slow` 用真账钉住 SQL 那侧）。
pub fn is_slow(age_days: i64, threshold_days: i64) -> bool {
    age_days > threshold_days
}

// ==================== 入参 ====================

/// 呆滞料标记入参（`GET /business/erp/inventory-aging/slow-moving`）。
///
/// 四个字段都可留空：`as_of` 留空取「今天（CST）」、物料 / 仓库留空 = 全量、
/// `days` 留空 = 按系统参数 / 缺省。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SlowMovingQuery {
    /// 基准日（`yyyy-MM-dd`，含当天；留空 = 今天 CST）。
    pub as_of: Option<String>,
    /// 物料 id（精确 UUID；留空 = 全部物料）。
    pub material_id: Option<String>,
    /// 仓库 id（精确 UUID；留空 = 全部仓库）。
    pub warehouse_id: Option<String>,
    /// 阈值天数（留空 = 系统参数 → 缺省）。
    pub days: Option<String>,
}

// ==================== 出参 ====================

/// 呆滞料清单里的一行 = 一个 物料 × 仓库 组合。
///
/// 名字（`material_*` / `warehouse_*`）是**主数据关联**（AGENTS.md 约定 13）：
/// 实时取「现在叫什么」，主数据软删后为 `null`，历史呆滞行照常可读。
#[derive(Debug, Clone, Serialize)]
pub struct SlowMovingRowRes {
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
    /// 本格有结存的层数（呆滞 + 不呆滞）。
    pub layer_count: i64,
    /// 本格总剩余数量（= Σ 全部层剩余）。
    pub qty_on_hand: Decimal,
    /// 本格总参考金额（= Σ 全部层 剩余 × 层单价）。
    pub amount_on_hand: Decimal,
    /// 本格呆滞层数（库龄 > 阈值且仍有剩余）。
    pub slow_layer_count: i64,
    /// 本格呆滞层剩余数量合计（> 0，本表只返回有呆滞的行）。
    pub qty_slow: Decimal,
    /// 本格呆滞层参考金额合计。
    pub amount_slow: Decimal,
    /// 呆滞占本格结存数量的比例（`qty_slow / qty_on_hand`，4 位小数；分母 0 时为 0）。
    pub slow_ratio: Decimal,
    /// 本格最老的（还有剩余的）层入库日期 —— 有呆滞层时它就是**呆滞层里最老的那层**
    /// （库龄随日期单调，最老的一层一定也是呆滞层）。
    pub oldest_inbound_date: nik::prelude::chrono::NaiveDate,
    /// 本格最大库龄（天）= 最老那层的库龄（有呆滞时即呆滞最大库龄）。
    pub max_age_days: i64,
}

/// 全表汇总（**截断前**统计，行被截断也不受影响）。
#[derive(Debug, Clone, Serialize)]
pub struct SlowMovingSummaryRes {
    /// 有结存的 物料 × 仓库 格数（分母：全库有多少格被扫过）。
    pub scanned_row_count: i64,
    /// 其中被判为呆滞的格数（= `rows` 截断前的总数）。
    pub slow_row_count: i64,
    /// 扫描到的层数（全部有结存的层）。
    pub layer_count: i64,
    /// 其中呆滞层数。
    pub slow_layer_count: i64,
    /// 全库有结存的剩余数量合计（跨物料时**单位不同，只作总量参考**）。
    pub qty_on_hand: Decimal,
    /// 全库呆滞剩余数量合计。
    pub qty_slow: Decimal,
    /// 全库有结存的参考金额合计。
    pub amount_on_hand: Decimal,
    /// 全库呆滞参考金额合计（= Σ 行 `amount_slow`）。
    pub amount_slow: Decimal,
    /// 呆滞数量占全库结存的比例（`qty_slow / qty_on_hand`，4 位小数）。
    pub slow_ratio: Decimal,
}

/// 呆滞料标记出参。
#[derive(Debug, Clone, Serialize)]
pub struct SlowMovingRes {
    /// 报表编码（[`SLOW_MOVING_CODE`]）。
    pub report_code: String,
    /// 报表名称（[`SLOW_MOVING_NAME`]）。
    pub report_name: String,
    /// 基准日（回显）。
    pub as_of: nik::prelude::chrono::NaiveDate,
    /// 基准日来源：`QUERY`（请求里传了 `as_of`）/ `TODAY`（留空 → 今天 CST）。
    pub as_of_source: String,
    /// 生效阈值（天）。
    pub threshold_days: i64,
    /// 阈值来源：`QUERY` / `CONFIG` / `DEFAULT`（见 [`ThresholdSource`]）。
    pub threshold_source: String,
    /// 阈值的系统参数键（前端「去改配置」的跳转锚点）。
    pub threshold_config_key: String,
    /// 全表汇总（截断前的全量）。
    pub summary: SlowMovingSummaryRes,
    /// 呆滞行总数（**截断前**的总数）。
    pub row_count: i64,
    /// 行上限（[`ROW_LIMIT`]）。
    pub row_limit: u64,
    /// `rows` 是否被截断（`summary` 不受影响）。
    pub rows_truncated: bool,
    /// 呆滞明细行（按 呆滞金额降序 → 呆滞数量降序 → 最老日期升序 → id）。
    pub rows: Vec<SlowMovingRowRes>,
    /// 口径提示（前端直接展示在表下方）。
    pub hints: Vec<String>,
}

/// SQL 聚合出来的一格：某个 物料 × 仓库 的全部层 + 其中呆滞层。
#[derive(Debug, Clone)]
pub struct CellRow {
    /// 物料。
    pub material_id: Uuid,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 有结存的层数。
    pub layer_count: i64,
    /// 总剩余数量。
    pub qty_on_hand: Decimal,
    /// 总参考金额。
    pub amount_on_hand: Decimal,
    /// 呆滞层数。
    pub slow_layer_count: i64,
    /// 呆滞层剩余数量。
    pub qty_slow: Decimal,
    /// 呆滞层参考金额。
    pub amount_slow: Decimal,
    /// 最老层的入库日期。
    pub oldest_inbound_date: nik::prelude::chrono::NaiveDate,
    /// 最大库龄（天）。
    pub max_age_days: i64,
}

// ==================== 纯逻辑 ====================

/// 比例（4 位小数；分母 0 / 负数一律 0 —— 不 panic、不出 `NaN`）。
pub fn ratio(part: Decimal, whole: Decimal) -> Decimal {
    if whole.is_zero() {
        return Decimal::ZERO;
    }
    payable_aging::money(part / whole)
}

/// 全表汇总：**从同一个 `cells` 数组**算（不另发 SQL 加总，见模块头注释）。
///
/// * `scanned_row_count` = 有结存的格数（含不呆滞的格）—— 它是「呆滞面」的分母，
///   少了它用户无法判断「8 格呆滞」到底是 8/10 还是 8/8000；
/// * `slow_row_count` = 呆滞格数（`slow_layer_count > 0`）；
/// * 数量 / 金额同样按「全部 / 仅呆滞」两组给。
pub fn summarise_cells(cells: &[CellRow]) -> SlowMovingSummaryRes {
    let mut out = SlowMovingSummaryRes {
        scanned_row_count: 0,
        slow_row_count: 0,
        layer_count: 0,
        slow_layer_count: 0,
        qty_on_hand: Decimal::ZERO,
        qty_slow: Decimal::ZERO,
        amount_on_hand: Decimal::ZERO,
        amount_slow: Decimal::ZERO,
        slow_ratio: Decimal::ZERO,
    };
    for cell in cells {
        out.scanned_row_count += 1;
        out.layer_count += cell.layer_count;
        out.qty_on_hand += cell.qty_on_hand;
        out.amount_on_hand += cell.amount_on_hand;
        if cell.slow_layer_count > 0 {
            out.slow_row_count += 1;
            out.slow_layer_count += cell.slow_layer_count;
            out.qty_slow += cell.qty_slow;
            out.amount_slow += cell.amount_slow;
        }
    }
    out.qty_on_hand = payable_aging::money(out.qty_on_hand);
    out.qty_slow = payable_aging::money(out.qty_slow);
    out.amount_on_hand = payable_aging::money(out.amount_on_hand);
    out.amount_slow = payable_aging::money(out.amount_slow);
    out.slow_ratio = ratio(out.qty_slow, out.qty_on_hand);
    out
}

/// 把格透视成行：**只留呆滞格**（`slow_layer_count > 0`），并排好序。
///
/// 不呆滞的格不是「丢了」：它们进了 [`summarise_cells`] 的
/// `scanned_row_count` / `qty_on_hand`。
pub fn build_rows(cells: &[CellRow]) -> Vec<SlowMovingRowRes> {
    let mut rows: Vec<SlowMovingRowRes> = cells
        .iter()
        .filter(|cell| cell.slow_layer_count > 0)
        .map(|cell| SlowMovingRowRes {
            material_id: cell.material_id.to_string(),
            material_code: None,
            material_name: None,
            spec: None,
            unit_name: None,
            warehouse_id: cell.warehouse_id.to_string(),
            warehouse_code: None,
            warehouse_name: None,
            layer_count: cell.layer_count,
            qty_on_hand: payable_aging::money(cell.qty_on_hand),
            amount_on_hand: payable_aging::money(cell.amount_on_hand),
            slow_layer_count: cell.slow_layer_count,
            qty_slow: payable_aging::money(cell.qty_slow),
            amount_slow: payable_aging::money(cell.amount_slow),
            slow_ratio: ratio(cell.qty_slow, cell.qty_on_hand),
            oldest_inbound_date: cell.oldest_inbound_date,
            max_age_days: cell.max_age_days,
        })
        .collect();
    sort_rows(&mut rows);
    rows
}

/// 行顺序：**呆滞金额降序**（先看占压资金大的）→ 呆滞数量降序 → 最老日期升序 →
/// 物料 id → 仓库 id（全序，刷新不换行）。
///
/// 与库龄报表「最老货在前」不同：报表回答「压了多久」，呆滞料回答「压了多少钱」。
pub fn sort_rows(rows: &mut [SlowMovingRowRes]) {
    rows.sort_by(|left, right| {
        right
            .amount_slow
            .cmp(&left.amount_slow)
            .then_with(|| right.qty_slow.cmp(&left.qty_slow))
            .then_with(|| left.oldest_inbound_date.cmp(&right.oldest_inbound_date))
            .then_with(|| left.material_id.cmp(&right.material_id))
            .then_with(|| left.warehouse_id.cmp(&right.warehouse_id))
    });
}

/// 按 [`ROW_LIMIT`] 截断行（**只在合计与行数统计完成之后**调用），返回是否被截断。
pub fn truncate_rows(rows: &mut Vec<SlowMovingRowRes>) -> bool {
    if rows.len() <= ROW_LIMIT {
        return false;
    }
    rows.truncate(ROW_LIMIT);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::NaiveDate;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    /// 一格：物料 0x11 × 仓库 0x22，参数是「全部层」与「呆滞层」两组数。
    fn cell(material: u128, warehouse: u128, qty: &str, amount: &str, slow_qty: &str, slow_amount: &str) -> CellRow {
        let slow_layer_count = if dec(slow_qty).is_zero() { 0 } else { 1 };
        CellRow {
            material_id: Uuid::from_u128(material),
            warehouse_id: Uuid::from_u128(warehouse),
            layer_count: 2,
            qty_on_hand: dec(qty),
            amount_on_hand: dec(amount),
            slow_layer_count,
            qty_slow: dec(slow_qty),
            amount_slow: dec(slow_amount),
            oldest_inbound_date: date("2026-06-01"),
            max_age_days: 168,
        }
    }

    /// 阈值边界：`> N`（严格大于）—— 与报表「90 天以上 = 91 起」同一把尺。
    #[test]
    fn threshold_boundary_is_strictly_greater() {
        for (threshold, age, expected) in [
            (90, 0, false),
            (90, 89, false),
            (90, 90, false),
            (90, 91, true),
            (90, 4000, true),
            (1, 1, false),
            (1, 2, true),
            (3650, 3650, false),
            (3650, 3651, true),
        ] {
            assert_eq!(
                is_slow(age, threshold),
                expected,
                "阈值 {threshold} 天 / 库龄 {age} 天 → 期望 {expected}"
            );
        }
        println!("[M2-10] 判定边界：库龄 > N 才算呆滞（N=90 时 90 天不算、91 天算）✓");
    }

    /// 参数校验：非整数 / 0 / 负数 / 超上界必须拒（不静默夹取）。
    #[test]
    fn threshold_param_is_validated_not_clamped() {
        assert_eq!(parse_threshold_param(None).expect("空"), None);
        assert_eq!(parse_threshold_param(Some("   ")).expect("空白"), None);
        assert_eq!(parse_threshold_param(Some(" 30 ")).expect("正常"), Some(30));
        assert_eq!(parse_threshold_param(Some("1")).expect("下界"), Some(1));
        assert_eq!(parse_threshold_param(Some("3650")).expect("上界"), Some(3650));

        for bad in ["0", "-1", "3651", "90.5", "abc", "90d"] {
            let error = parse_threshold_param(Some(bad)).expect_err("必须拒");
            assert!(error.contains("days"), "文案要点名字段：{error}");
        }
        println!("[M2-10] 阈值参数：0 / 负数 / 3651 / 非整数一律 400（不夹取）✓");
    }

    /// 三级解析：参数 > 配置 > 缺省；配置坏了回缺省**并带告警**（不静默）。
    #[test]
    fn threshold_resolution_prefers_query_then_config_then_default() {
        // ① 参数优先（哪怕配置也合法）
        let resolved = resolve_threshold(Some(30), Some("180"));
        assert_eq!((resolved.days, resolved.source), (30, ThresholdSource::Query));
        assert!(resolved.warning.is_none());

        // ② 没传参数 → 读配置
        let resolved = resolve_threshold(None, Some("180"));
        assert_eq!((resolved.days, resolved.source), (180, ThresholdSource::Config));
        assert!(resolved.warning.is_none());
        assert_eq!(resolved.source.as_str(), "CONFIG");

        // ③ 没配 → 缺省
        let resolved = resolve_threshold(None, None);
        assert_eq!(
            (resolved.days, resolved.source),
            (DEFAULT_THRESHOLD_DAYS, ThresholdSource::Default)
        );
        assert!(resolved.warning.is_none(), "没配不是错，不告警");
        assert_eq!(resolved.source.as_str(), "DEFAULT");

        // ④ 配了但不可用 → 回缺省 + 告警（点名参数键与坏值）
        for bad in ["abc", "0", "-5", "99999", "90.5"] {
            let resolved = resolve_threshold(None, Some(bad));
            assert_eq!(
                (resolved.days, resolved.source),
                (DEFAULT_THRESHOLD_DAYS, ThresholdSource::Default),
                "坏配置 `{bad}` 必须回缺省"
            );
            let warning = resolved.warning.expect("坏配置必须带告警");
            assert!(warning.contains(THRESHOLD_CONFIG_KEY), "告警要点名参数键：{warning}");
            assert!(warning.contains(bad), "告警要带上坏值：{warning}");
        }

        // 空白配置 = 没配（不告警）
        let resolved = resolve_threshold(None, Some("   "));
        assert_eq!(resolved.source, ThresholdSource::Default);
        assert!(resolved.warning.is_none());
        println!("[M2-10] 阈值三级：参数 > 配置 > 缺省；坏配置回缺省并点名参数键 ✓");
    }

    /// 汇总：分母是**全部格**（含不呆滞的），分子只算呆滞格；比例 4 位小数。
    #[test]
    fn summary_counts_scanned_rows_not_only_slow_rows() {
        let cells = vec![
            cell(0x11, 0x22, "120", "240", "70", "140"),
            cell(0x33, 0x22, "500", "500", "0", "0"),
            cell(0x44, 0x22, "30", "60", "30", "60"),
        ];
        let summary = summarise_cells(&cells);
        assert_eq!(summary.scanned_row_count, 3, "扫过 3 格");
        assert_eq!(summary.slow_row_count, 2, "其中 2 格呆滞");
        assert_eq!(summary.layer_count, 6);
        assert_eq!(summary.slow_layer_count, 2);
        assert_eq!(summary.qty_on_hand, dec("650"));
        assert_eq!(summary.qty_slow, dec("100"));
        assert_eq!(summary.amount_slow, dec("200"));
        // 100 / 650 = 0.1538（4 位小数）
        assert_eq!(summary.slow_ratio, dec("0.1538"));
        println!("[M2-10] 汇总：扫过 3 格 / 呆滞 2 格 / 数量 100/650 = 15.38% ✓");
    }

    /// 行只留呆滞格；行合计与 summary 的自洽性（Σ行 = summary 的呆滞列）。
    #[test]
    fn rows_only_keep_slow_cells_and_match_summary() {
        let cells = vec![
            cell(0x11, 0x22, "120", "240", "70", "140"),
            cell(0x33, 0x22, "500", "500", "0", "0"),
            cell(0x44, 0x22, "30", "60", "30", "60"),
        ];
        let summary = summarise_cells(&cells);
        let rows = build_rows(&cells);
        assert_eq!(rows.len(), 2, "不呆滞的格不进行（但进 summary）");
        let row_slow_qty: Decimal = rows.iter().map(|row| row.qty_slow).sum();
        let row_slow_amount: Decimal = rows.iter().map(|row| row.amount_slow).sum();
        assert_eq!(row_slow_qty, summary.qty_slow, "Σ行呆滞数量 = summary.qty_slow");
        assert_eq!(
            row_slow_amount, summary.amount_slow,
            "Σ行呆滞金额 = summary.amount_slow"
        );
        let row_layers: i64 = rows.iter().map(|row| row.slow_layer_count).sum();
        assert_eq!(row_layers, summary.slow_layer_count);
        println!("[M2-10] 行只留呆滞格；Σ行呆滞 = summary 呆滞 ✓");
    }

    /// 行内比例：`qty_slow / qty_on_hand`；分母 0 时给 0（不 panic）。
    #[test]
    fn row_ratio_is_safe_on_zero_and_rounds_to_four_decimals() {
        let rows = build_rows(&[cell(0x11, 0x22, "120", "240", "70", "140")]);
        assert_eq!(rows[0].slow_ratio, dec("0.5833"), "70 / 120 = 0.5833");
        assert_eq!(ratio(dec("1"), Decimal::ZERO), Decimal::ZERO, "分母 0 → 0");
        println!("[M2-10] 行比例 70/120 = 0.5833；分母 0 → 0 ✓");
    }

    /// 排序：呆滞金额降序 → 呆滞数量降序 → 最老日期升序 → id（全序）。
    #[test]
    fn rows_sort_by_slow_amount_then_qty_then_age() {
        let mut big_amount = cell(0x11, 0x22, "10", "10", "10", "900");
        big_amount.oldest_inbound_date = date("2026-01-01");
        let mut small_amount_old = cell(0x33, 0x22, "10", "10", "10", "5");
        small_amount_old.oldest_inbound_date = date("2025-01-01");
        let mut small_amount_new = cell(0x44, 0x22, "10", "10", "10", "5");
        small_amount_new.oldest_inbound_date = date("2026-10-01");
        let rows = build_rows(&[small_amount_new, small_amount_old, big_amount]);
        let ids: Vec<&str> = rows.iter().map(|row| row.material_id.as_str()).collect();
        assert_eq!(
            ids,
            [
                Uuid::from_u128(0x11).to_string(),
                Uuid::from_u128(0x33).to_string(),
                Uuid::from_u128(0x44).to_string(),
            ],
            "金额大的在前；金额相同看数量（都 10）；再看最老日期"
        );
        println!("[M2-10] 行序：呆滞金额降序 → 数量降序 → 最老在前 → id ✓");
    }

    /// 行上限只砍行；**行数与合计是截断前算的**（截断了也要如实标）。
    #[test]
    fn row_limit_truncates_rows_but_not_summary() {
        let cells: Vec<CellRow> = (0..(ROW_LIMIT as u128 + 3))
            .map(|index| cell(0x1000 + index, 0x22, "10", "10", "1", "1"))
            .collect();
        let summary = summarise_cells(&cells);
        let mut rows = build_rows(&cells);
        let row_count = i64::try_from(rows.len()).unwrap_or(0);
        assert_eq!(row_count, i64::try_from(ROW_LIMIT + 3).unwrap_or(0));
        let truncated = truncate_rows(&mut rows);
        assert!(truncated, "超上限必须标记截断");
        assert_eq!(rows.len(), ROW_LIMIT);
        assert_eq!(summary.slow_row_count, row_count, "行数是截断前的全量");
        assert_eq!(summary.qty_slow, dec(&format!("{}", ROW_LIMIT + 3)), "合计不受截断影响");
        println!("[M2-10] 截断：行 {ROW_LIMIT} 条，行数与合计仍是全量 {row_count} ✓");
    }

    /// 常量口径：编码 / 名称 / 参数键 / 缺省阈值 / 行上限与报表同一。
    #[test]
    fn constants_are_pinned() {
        assert_eq!(SLOW_MOVING_CODE, "INVENTORY_SLOW_MOVING");
        assert_eq!(SLOW_MOVING_NAME, "呆滞料清单");
        assert_eq!(THRESHOLD_CONFIG_KEY, "erp.inventory.slow-moving-days");
        assert_eq!(DEFAULT_THRESHOLD_DAYS, 90);
        assert_eq!(MIN_THRESHOLD_DAYS, 1);
        assert_eq!(MAX_THRESHOLD_DAYS, 3650);
        assert_eq!(ROW_LIMIT, report::dto::ROW_LIMIT, "与库龄报表同一个行上限");
        assert_eq!(ThresholdSource::Query.as_str(), "QUERY");
        println!("[M2-10] 常量：编码 / 名称 / 参数键 / 缺省 90 天 / 行上限同报表 ✓");
    }
}
