//! 成本还原「还原后报表：料工费占比」的**契约层与纯逻辑**（成本核算 · M5-28）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-restore-report`（AGENTS 约定 8）。
//!
//! # 这张报表回答什么
//!
//! **还原之后，这块钱在车间 / 产品 / 成本对象这些口径上是什么结构**：
//!
//! ```text
//!                 还原前（归集口径）              还原后（原始成本项目）        占比怎么变
//!   车间 A   材料 500（含半成品 400）        →    外购材料 220                       材料 −40.00pp
//!            人工 100                            人工 196                          人工 +13.71pp
//!            制造费用 100                        制造费用 284                      费用 +26.29pp
//!   车间 B   …                                    …                                 …
//!   ───────────────────────────────────────────────────────────────────────────
//!   合计     还原前合计 700                      还原后合计 700 + 不彻底 0         四项 = 100.00%
//! ```
//!
//! # 与 M5-26 / M5-27 的分工（**不许重算**）
//!
//! | 卡片 | 出什么 | 粒度 |
//! |---|---|---|
//! | M5-26 `cost-restore-structure` | 每一层**是什么**（结构 / 比例） | 逐成本对象 × 逐层节点 |
//! | M5-27 `cost-restore-rollup` | 每块钱**还原成了什么**（料 / 工 / 费 + 逐层台账） | 逐成本对象 |
//! | **M5-28（本卡）** | **按口径汇总的占比与结构变化** | 车间 / 产品 / 成本对象 / 整表 |
//!
//! 本卡是**纯汇总**：行由 M5-27 的 [`rollup_rows_of`] 一次给出（结构与卷积全在那边），
//! 这里只做「按维度分组 + 加法 + 占比 + 各档行数」，**零 SQL、零迁移、零新增权限码**。
//! 所以本报表的每一行金额必然等于明细表对应行的金额之和 —— 两张表不可能吵架。
//!
//! [`rollup_rows_of`]: crate::business::erp::cost::cost_restore_rollup_handler::rollup_rows_of
//!
//! # 三条硬口径
//!
//! 1. **占比分母统一取「还原前合计」**（= 还原后三要素合计 + 还原不彻底）：
//!    所以 `材料 + 人工 + 制造费用 + 还原不彻底` 四项合计恰好 100.00%（`share_sum` 自检）。
//!    M5-27 行级的 `after_*_share` 分母是「还原后三要素合计」，两者**口径不同**：
//!    本表 `restored.material_share` = M5-27 的 `after_material_share` × (100 − 不彻底占比) / 100，
//!    这样才保证「还原不彻底」也在报表里占一格，而不是被悄悄摊到三要素头上。
//! 2. **占比变化（pp）= 还原后占比 − 还原前占比**（2 位）。材料减少的百分点 = 人工与制造费用
//!    增加的百分点之和（+ 还原不彻底的百分点）；三要素的 pp 之和 = `− 不彻底占比`。
//! 3. **占比只做展示，金额才是真值**：所有占比都由 4 位金额算出后四舍五入到 2 位，
//!    四项合计可能出现 ±0.01 的舍入差 —— `share_sum` 把实际合计暴露出来，不假装必然 100.00。
//!
//! # 只读
//!
//! 零写库、零迁移、零新增权限码（复用 `business:erp:cost-object:list`，与结构表 / 还原表 /
//! 约当表 / 余额表同码）；也没有「事务提交后失败留死单」的面（AGENTS 约定 11）。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use cloudpivot_kernel::shared::error_code;

pub use crate::business::erp::cost::cost_restore_rollup_dto::{
    FORMULA_RESTORE, FORMULA_UNRESOLVED, MAX_DEPTH_DEFAULT, MAX_DEPTH_LIMIT, PERM_LIST, PERMISSIONS, Row, StateFilter,
    money_text, parse_money, pct_text, share_pct,
};
pub use crate::business::erp::cost::cost_restore_structure_dto::{BatchView, ElementSplit, ElementView, OptionView};

// ==================== 默认值 / 上限 ====================

/// 默认每页分组数（报表一行一个车间 / 产品，50 足够一屏翻页）。
pub const PAGE_SIZE_DEFAULT: u64 = 50;

/// 每页分组数上限。
pub const PAGE_SIZE_LIMIT: u64 = 500;

// ==================== 公式文案 ====================

/// 占比分母口径（**后端唯一真源**，前端直接显示，别自己写一句）。
pub const BASIS: &str = "占比分母 = 还原前合计（= 还原后三要素合计 + 还原不彻底）：\
                          材料 + 人工 + 制造费用 + 还原不彻底 四项合计 100.00%";

/// 占比公式。
pub const FORMULA_SHARE: &str = "占比 = 该要素金额 ÷ 还原前合计 × 100（2 位）；\
                                  还原后三要素 + 还原不彻底 = 还原前合计 ⇒ 四项占比合计 100.00%\
                                  （各自四舍五入，允许 ±0.01 舍入差，见 share_sum）";

/// 结构变化公式。
pub const FORMULA_SHIFT: &str = "占比变化（pp）= 还原后占比 − 还原前占比；\
                                  材料减少的百分点 = 人工 + 制造费用增加的百分点（+ 不彻底的百分点），\
                                  三项合计 = − 不彻底占比";

// ==================== 分组维度 ====================

/// 汇总维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    /// 按车间 / 部门（默认）。
    Dept,
    /// 按产出品。
    Product,
    /// 按成本对象（一对象一行，用于和明细表对账）。
    Object,
    /// 整表一行。
    All,
}

/// 维度选项（顺序即下拉顺序）。
pub const GROUP_BYS: [GroupBy; 4] = [GroupBy::Dept, GroupBy::Product, GroupBy::Object, GroupBy::All];

impl GroupBy {
    /// 出参值。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Dept => "dept",
            Self::Product => "product",
            Self::Object => "object",
            Self::All => "all",
        }
    }

    /// 中文名（2–4 字，列标直接用）。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Dept => "按车间",
            Self::Product => "按产品",
            Self::Object => "按对象",
            Self::All => "整表",
        }
    }

    /// 解析（空 / 缺省 ⇒ `dept`；**拼错 400**，不静默回落）。
    pub fn parse(raw: Option<&str>) -> std::result::Result<Self, ApiError> {
        let text = cost_restore_text(raw);
        if text.is_empty() {
            return Ok(Self::Dept);
        }
        match text.as_str() {
            "dept" => Ok(Self::Dept),
            "product" => Ok(Self::Product),
            "object" => Ok(Self::Object),
            "all" => Ok(Self::All),
            other => Err(bad_request(&format!(
                "group_by 只支持 dept / product / object / all（收到 `{other}`）：\
                 拼错时静默回落会让报表看起来「就是没数据」"
            ))),
        }
    }
}

/// 去空白的小工具（`None` / 空白 ⇒ 空串）。
fn cost_restore_text(raw: Option<&str>) -> String {
    raw.map(str::trim).unwrap_or_default().to_string()
}

/// 维度选项（出参）。
pub fn group_options() -> Vec<OptionView> {
    GROUP_BYS
        .iter()
        .map(|item| OptionView {
            value: item.as_str().to_string(),
            label: item.label().to_string(),
        })
        .collect()
}

// ==================== 分组的键与名字 ====================

/// 产出品文本（`编码 名称`；都缺 ⇒ `None`）。
fn product_text(row: &Row) -> Option<String> {
    let code = row.product_code.as_deref().unwrap_or("").trim();
    let name = row.product_name.as_deref().unwrap_or("").trim();
    match (code.is_empty(), name.is_empty()) {
        (false, false) => Some(format!("{code} {name}")),
        (false, true) => Some(code.to_string()),
        (true, false) => Some(name.to_string()),
        (true, true) => None,
    }
}

/// 一行落在哪个分组：`(key, label, hint)`。
///
/// * `dept` → 车间 id / 车间名（缺名 ⇒「未指定车间」）；
/// * `product` → 产出品 id / `编码 名称`；
/// * `object` → 成本对象 id / 对象号，副标是「产出品 · 批号」；
/// * `all` → 固定一个分组（整表）。
pub fn key_of(row: &Row, group_by: GroupBy) -> (String, String, Option<String>) {
    match group_by {
        GroupBy::Dept => {
            let label = row
                .workshop_dept_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or("未指定车间")
                .to_string();
            (row.workshop_dept_id.clone(), label, None)
        }
        GroupBy::Product => {
            let key = if row.product_id.trim().is_empty() {
                "UNKNOWN".to_string()
            } else {
                row.product_id.clone()
            };
            let label = product_text(row).unwrap_or_else(|| "未指定产出品".to_string());
            (key, label, None)
        }
        GroupBy::Object => {
            let mut parts: Vec<String> = Vec::new();
            if let Some(product) = product_text(row) {
                parts.push(product);
            }
            if let Some(lot) = row.lot_no.as_deref().map(str::trim).filter(|lot| !lot.is_empty()) {
                parts.push(format!("批号 {lot}"));
            }
            let hint = if parts.is_empty() {
                None
            } else {
                Some(parts.join(" · "))
            };
            (row.object_id.clone(), row.object_no.clone(), hint)
        }
        GroupBy::All => ("ALL".to_string(), "全部".to_string(), None),
    }
}

// ==================== 累加（纯加法，不判口径） ====================

/// 一组（或整表）的累加器。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupAcc {
    /// 分组键。
    pub key: String,
    /// 分组名。
    pub label: String,
    /// 副标（对象行：产出品 · 批号）。
    pub hint: Option<String>,
    /// 行数（成本对象个数）。
    pub object_count: u64,
    /// 还原前三要素。
    pub before: ElementSplit,
    /// 还原前合计（**占比分母**）。
    pub before_total: Decimal,
    /// 还原后三要素（材料只含外购料）。
    pub after: ElementSplit,
    /// 还原后合计（= 三要素之和，不含不彻底）。
    pub after_total: Decimal,
    /// 还原不彻底。
    pub unresolved: Decimal,
    /// 材料里的自制半成品。
    pub semi_in_material: Decimal,
    /// 已还原行数。
    pub restored_rows: u64,
    /// 还原不彻底行数。
    pub partial_rows: u64,
    /// 无需还原行数。
    pub no_semi_rows: u64,
}

impl GroupAcc {
    /// 一个空分组。
    pub fn new(key: String, label: String, hint: Option<String>) -> Self {
        Self {
            key,
            label,
            hint,
            ..Self::default()
        }
    }

    /// 累加一行（**只做加法**：金额原样取自 M5-27 的出参，不重算）。
    pub fn add_row(&mut self, row: &Row) {
        self.object_count += 1;
        self.before.add(&ElementSplit {
            material: parse_money(&row.before.material),
            labor: parse_money(&row.before.labor),
            overhead: parse_money(&row.before.overhead),
        });
        self.before_total += parse_money(&row.before.total);
        self.after.add(&ElementSplit {
            material: parse_money(&row.after.material),
            labor: parse_money(&row.after.labor),
            overhead: parse_money(&row.after.overhead),
        });
        self.after_total += parse_money(&row.after.total);
        self.unresolved += parse_money(&row.unresolved);
        self.semi_in_material += parse_money(&row.semi_in_material);
        match row.state.as_str() {
            "restored" => self.restored_rows += 1,
            "partial" => self.partial_rows += 1,
            "no_semi" => self.no_semi_rows += 1,
            _ => {}
        }
    }

    /// 从材料里拆出来的人工（= 还原后人工 − 还原前人工）。
    pub fn restored_labor(&self) -> Decimal {
        self.after.labor - self.before.labor
    }

    /// 从材料里拆出来的制造费用。
    pub fn restored_overhead(&self) -> Decimal {
        self.after.overhead - self.before.overhead
    }

    /// 守恒自检：还原后合计 + 不彻底 − 还原前合计（恒为 0）。
    pub fn check(&self) -> Decimal {
        self.after_total + self.unresolved - self.before_total
    }
}

/// 按维度分组累加。
///
/// 排序是**确定的**：还原后合计降序、再按 key 升序 —— 报表翻页时行序不会每次刷新都变。
pub fn aggregate(rows: &[Row], group_by: GroupBy) -> Vec<GroupAcc> {
    let mut accs: Vec<GroupAcc> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for row in rows {
        let (key, label, hint) = key_of(row, group_by);
        let slot = match index.get(&key) {
            Some(position) => &mut accs[*position],
            None => {
                accs.push(GroupAcc::new(key.clone(), label, hint));
                index.insert(key, accs.len() - 1);
                accs.last_mut().expect("刚 push 过，必然有最后一项")
            }
        };
        slot.add_row(row);
    }
    accs.sort_by(|left, right| {
        right
            .after_total
            .cmp(&left.after_total)
            .then_with(|| left.key.cmp(&right.key))
    });
    accs
}

/// 整表累加（**不受分组 / 分页影响**）。
pub fn totals_acc(rows: &[Row]) -> GroupAcc {
    let mut acc = GroupAcc::new("ALL".to_string(), "合计".to_string(), None);
    for row in rows {
        acc.add_row(row);
    }
    acc
}

// ==================== 主导要素 ====================

/// 还原后占比最高的要素（报表行首的标签，别在前端再算一遍）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dominant {
    /// 材料最高。
    Material,
    /// 人工最高。
    Labor,
    /// 制造费用最高。
    Overhead,
    /// 没有金额。
    None,
}

impl Dominant {
    /// 出参值。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Material => "material",
            Self::Labor => "labor",
            Self::Overhead => "overhead",
            Self::None => "none",
        }
    }

    /// 中文名。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Material => "材料最高",
            Self::Labor => "人工最高",
            Self::Overhead => "制造费用最高",
            Self::None => "无数据",
        }
    }
}

/// 取占比最高的要素（按**金额**比，避免 2 位舍入导致「谁最高」抖）。
pub fn dominant_of(material: Decimal, labor: Decimal, overhead: Decimal) -> Dominant {
    if material <= Decimal::ZERO && labor <= Decimal::ZERO && overhead <= Decimal::ZERO {
        return Dominant::None;
    }
    if material >= labor && material >= overhead {
        Dominant::Material
    } else if labor >= overhead {
        Dominant::Labor
    } else {
        Dominant::Overhead
    }
}

// ==================== 出参 ====================

/// 还原前（一组 / 整表）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ShareView {
    /// 材料（含半成品）。
    pub material: String,
    /// 人工。
    pub labor: String,
    /// 制造费用。
    pub overhead: String,
    /// 合计。
    pub total: String,
    /// 材料占比（分母 = 还原前合计）。
    pub material_share: Option<String>,
    /// 人工占比。
    pub labor_share: Option<String>,
    /// 制造费用占比。
    pub overhead_share: Option<String>,
}

/// 还原后（一组 / 整表）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RestoredShareView {
    /// 外购材料。
    pub material: String,
    /// 人工（本期归集 + 从材料里拆出来的）。
    pub labor: String,
    /// 制造费用（同上）。
    pub overhead: String,
    /// 还原后三要素合计（**不含**还原不彻底）。
    pub total: String,
    /// 还原不彻底。
    pub unresolved: String,
    /// 占比分母（= 还原前合计 = 三要素合计 + 不彻底）。
    pub denominator: String,
    /// 材料占比。
    pub material_share: Option<String>,
    /// 人工占比。
    pub labor_share: Option<String>,
    /// 制造费用占比。
    pub overhead_share: Option<String>,
    /// 还原不彻底占比。
    pub unresolved_share: Option<String>,
}

/// 占比变化（百分点）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ShiftView {
    /// 材料占比变化（通常为负）。
    pub material_pp: Option<String>,
    /// 人工占比变化（通常为正）。
    pub labor_pp: Option<String>,
    /// 制造费用占比变化（通常为正）。
    pub overhead_pp: Option<String>,
    /// 口径说明。
    pub note: String,
}

/// 一组（或整表合计）的出参。
#[derive(Debug, Clone, serde::Serialize)]
pub struct GroupRow {
    /// 分组键（`all` ⇒ `ALL`；合计行也是 `ALL`）。
    pub key: String,
    /// 分组名。
    pub label: String,
    /// 副标（按对象时是「产出品 · 批号」）。
    pub hint: Option<String>,
    /// 成本对象个数。
    pub object_count: u64,
    /// 还原前。
    pub before: ShareView,
    /// 还原后。
    pub restored: RestoredShareView,
    /// 占比变化。
    pub shift: ShiftView,
    /// 从材料里拆出来的人工（金额）。
    pub restored_labor: String,
    /// 从材料里拆出来的制造费用（金额）。
    pub restored_overhead: String,
    /// 材料里的自制半成品。
    pub semi_in_material: String,
    /// 半成品占材料投入的百分比。
    pub semi_share: Option<String>,
    /// 已还原行数。
    pub restored_rows: u64,
    /// 还原不彻底行数。
    pub partial_rows: u64,
    /// 无需还原行数。
    pub no_semi_rows: u64,
    /// 占比最高的要素。
    pub dominant: String,
    /// 要素中文名。
    pub dominant_label: String,
    /// 四项占比的实际合计（自检：应为 100.00，允许 ±0.01 舍入差）。
    pub share_sum: Option<String>,
    /// 守恒自检：还原后合计 + 不彻底 − 还原前合计（应为 0）。
    pub check: String,
}

/// 一个累加器 → 出参（纯函数）。
pub fn group_view(acc: &GroupAcc) -> GroupRow {
    let denominator = acc.before_total;
    // 还原前：分母就是自己
    let before_material_share = share_pct(acc.before.material, denominator);
    let before_labor_share = share_pct(acc.before.labor, denominator);
    let before_overhead_share = share_pct(acc.before.overhead, denominator);
    // 还原后：同一个分母，所以「材料 + 人工 + 费 + 不彻底 = 100%」
    let material_share = share_pct(acc.after.material, denominator);
    let labor_share = share_pct(acc.after.labor, denominator);
    let overhead_share = share_pct(acc.after.overhead, denominator);
    let unresolved_share = share_pct(acc.unresolved, denominator);

    let pp = |after: Option<Decimal>, before: Option<Decimal>| {
        after.zip(before).map(|(after, before)| pct_text(after - before))
    };
    let share_sum = [material_share, labor_share, overhead_share, unresolved_share]
        .iter()
        .copied()
        .collect::<Option<Vec<Decimal>>>()
        .map(|shares| pct_text(shares.iter().copied().sum::<Decimal>()));
    let dominant = dominant_of(acc.after.material, acc.after.labor, acc.after.overhead);

    GroupRow {
        key: acc.key.clone(),
        label: acc.label.clone(),
        hint: acc.hint.clone(),
        object_count: acc.object_count,
        before: ShareView {
            material: money_text(acc.before.material),
            labor: money_text(acc.before.labor),
            overhead: money_text(acc.before.overhead),
            total: money_text(acc.before_total),
            material_share: before_material_share.map(pct_text),
            labor_share: before_labor_share.map(pct_text),
            overhead_share: before_overhead_share.map(pct_text),
        },
        restored: RestoredShareView {
            material: money_text(acc.after.material),
            labor: money_text(acc.after.labor),
            overhead: money_text(acc.after.overhead),
            total: money_text(acc.after_total),
            unresolved: money_text(acc.unresolved),
            denominator: money_text(denominator),
            material_share: material_share.map(pct_text),
            labor_share: labor_share.map(pct_text),
            overhead_share: overhead_share.map(pct_text),
            unresolved_share: unresolved_share.map(pct_text),
        },
        shift: ShiftView {
            material_pp: pp(material_share, before_material_share),
            labor_pp: pp(labor_share, before_labor_share),
            overhead_pp: pp(overhead_share, before_overhead_share),
            note: FORMULA_SHIFT.to_string(),
        },
        restored_labor: money_text(acc.restored_labor()),
        restored_overhead: money_text(acc.restored_overhead()),
        semi_in_material: money_text(acc.semi_in_material),
        semi_share: share_pct(acc.semi_in_material, acc.before.material).map(pct_text),
        restored_rows: acc.restored_rows,
        partial_rows: acc.partial_rows,
        no_semi_rows: acc.no_semi_rows,
        dominant: dominant.as_str().to_string(),
        dominant_label: dominant.label().to_string(),
        share_sum,
        check: money_text(acc.check()),
    }
}

// ==================== 入参 / 出参（整张报表） ====================

/// 入参。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ReportQuery {
    /// 期间（`yyyy-MM`，必填）。
    pub period_code: Option<String>,
    /// 关键字（对象号 / 生产订单号 / 产出批号 / 产出品编码 / 产出品名称）。
    pub keyword: Option<String>,
    /// 汇总维度（`dept` / `product` / `object` / `all`，缺省 `dept`）。
    pub group_by: Option<String>,
    /// 状态筛选（`all` / `restored` / `partial` / `no_semi`）。
    pub state: Option<String>,
    /// 页码（1 起）。
    pub page: Option<u64>,
    /// 每页分组数（默认 50，上限 500）。
    pub page_size: Option<u64>,
    /// 下钻层数（默认 6，上限 12）。
    pub max_depth: Option<u32>,
}

/// 归一化后的入参。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportFilter {
    /// 期间。
    pub period_code: String,
    /// 关键字。
    pub keyword: Option<String>,
    /// 汇总维度。
    pub group_by: GroupBy,
    /// 状态筛选。
    pub state: StateFilter,
    /// 页码。
    pub page: u64,
    /// 每页分组数。
    pub page_size: u64,
    /// 下钻层数。
    pub max_depth: u32,
}

impl ReportFilter {
    /// 报表取行用的还原表筛选条件（**不分页**：汇总必须基于整表）。
    pub fn rollup_filter(&self) -> crate::business::erp::cost::cost_restore_rollup_dto::RollupFilter {
        crate::business::erp::cost::cost_restore_rollup_dto::RollupFilter {
            period_code: self.period_code.clone(),
            keyword: self.keyword.clone(),
            state: self.state,
            page: 1,
            page_size: PAGE_SIZE_LIMIT,
            max_depth: self.max_depth,
        }
    }
}

/// 整张报表的出参。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReportRes {
    /// 期间。
    pub period_code: String,
    /// 当期成本核算批次。
    pub batch: Option<BatchView>,
    /// 是否已建批次。
    pub cost_ready: bool,
    /// 汇总维度回显。
    pub group_by: String,
    /// 汇总维度中文名。
    pub group_by_label: String,
    /// 维度选项。
    pub group_options: Vec<OptionView>,
    /// 状态筛选回显。
    pub state: String,
    /// 状态筛选中文名。
    pub state_label: String,
    /// 关键字回显。
    pub keyword: Option<String>,
    /// 下钻层数回显。
    pub max_depth: u32,
    /// 占比分母口径。
    pub basis: String,
    /// 卷积口径（沿用 M5-27）。
    pub formula_restore: String,
    /// 不彻底口径（沿用 M5-27）。
    pub formula_unresolved: String,
    /// 占比公式。
    pub formula_share: String,
    /// 结构变化公式。
    pub formula_shift: String,
    /// 整表合计（不受分组 / 分页影响）。
    pub totals: GroupRow,
    /// 分组行。
    pub groups: Vec<GroupRow>,
    /// 分组数（= 分页前的总数）。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页分组数。
    pub page_size: u64,
    /// 状态选项。
    pub state_options: Vec<OptionView>,
    /// 页脚说明。
    pub note: String,
}

// ==================== 错误 ====================

/// 400 + `PARAM_INVALID`。
pub fn bad_request(message: &str) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.to_string())
}
