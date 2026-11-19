//! 在制品「约当产量表」的**契约层与纯逻辑**（成本核算 · M5-20）。
//!
//! # 这张表算什么
//!
//! ```text
//! 本期完工 FQ = 成本对象累计完工 − 前期已结算计算单的完工合计
//! 期末在制 WQ = biz_cost_object.wip_qty
//! 完工率   P  = biz_cost_object.completion_pct （%）
//!
//! 材料约当（开工一次投入）：EQ_mat = FQ + WQ × 100%
//! 加工约当（人工 / 费用）  ：EQ_cnv = FQ + WQ × P / 100
//! ```
//!
//! 与 ARCH-9 §4.6.1 的公式**逐字一致**；实现上直接复用
//! [`cost_period_item_dto::equivalents`] + [`cost_period_item_dto::money`]，
//! 不在这里另写一份算式 —— 否则「表里的约当」与「计算单里的约当」迟早会差一位。
//!
//! # 为什么表里要带「与计算单是否一致」
//!
//! 完工率与在制数量是车间在成本对象页按月填的（`biz_cost_object`），而计算单
//! （`biz_cost_period_item`）是执行 `POST /cost-period/calc` 那一刻的**快照**。
//! 于是有一个非常真实的状态：**算过了，之后又改了完工率 / 在制数量**。
//! 这时表里的约当是按新数字算的、计算单里是旧数字，两者对不上，而界面上没有任何
//! 提示 —— 用户会拿旧计算单的单位成本去解释新表，越看越糊涂。
//!
//! 所以每行都带 [`EquivCalc::recalc_required`] + [`DriftField`]（哪个字段、表里是多少、
//! 计算单里是多少），列表可 `state=drift` 一屏筛出「需要重算」的那几行。
//!
//! # 三种行状态（不画假 0 是硬口径）
//!
//! | 状态 | 含义 | 约当怎么显示 |
//! |---|---|---|
//! | `wip` | 期末在制 > 0 | 两列都有数（加工约当 = FQ + WQ×P/100） |
//! | `finished` | 期末在制 = 0 | 两列都等于本期完工（加工与材料同值） |
//! | `blocked` | 这行算不出来 | 算不出来的那一列给 `null`（**不是 0**） |
//!
//! `blocked` 的五种成因（每一种都给「怎么修」，与 calc 第 6 段的 409 同口径）：
//!
//! 1. `WIP_PCT_MISSING`：期末在制 > 0 但完工率 0 ⇒ 加工约当算不出来（材料约当照算，100%）；
//! 2. `COMPLETED_NEGATIVE`：累计完工 < 前期已结算完工 ⇒ 本期完工是负数，两列都不给；
//! 3. `NO_OUTPUT`：本期既无完工也无期末在制，但本期有归集 / 上期还挂着在制 ⇒ 两列都不给；
//! 4. `PCT_OUT_OF_RANGE`：完工率不在 0–100（库有 CHECK，这里是第二道防线）；
//! 5. `MATERIAL_MODE_UNSUPPORTED`：投料方式不是「开工一次投入」⇒ 材料约当不该按 100% 算，
//!    一期不实现按进度投料，材料一侧给 `null`（加工一侧照算）。
//!
//! # 与其它卡的分工（本卡不越界）
//!
//! * **M5-21 完工率录入（按工序）**：录入侧的事；本表只读 `biz_cost_object.completion_pct` 这个
//!   「对象级期末完工率」。工序级完工率落地后按加权汇总写回对象级即可，本表**不需要改**。
//! * **M5-22 料工费在完工与在制间分配**：拿本表的 EQ 去除「期初 + 本期投入」得单位成本，
//!   再乘完工数 / 在制约当数。本卡只给「约当产量」这个分母，**不算钱**。
//! * **M5-23 期末在制余额**：金额侧。
//!
//! # 只读
//!
//! 零写库、零迁移、零新增权限码（复用 `business:erp:cost-object:list`，与
//! `cost-collect-sheet` / `cost-collect-sheet/wip-balance` 同码：看得到成本对象的人
//! 就是该看这张表的人）。因此也没有「事务提交后失败留死单」的面（AGENTS 约定 11）。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use crate::business::erp::cost::cost_period_item_dto;
use cloudpivot_kernel::shared::error_code;

// ==================== 权限码（复用 cost-object:list） ====================

/// 表读取（**复用**成本对象列表的码：零新增码，菜单里已有承载行）。
pub const PERM_LIST: &str = "business:erp:cost-object:list";

/// 本模块全部权限码（与其它成本模块 `all_permissions()` 同形）。
pub const PERMISSIONS: [&str; 1] = [PERM_LIST];

// ==================== 口径文案（前端页脚直接用，别在前端手抄） ====================

/// 材料约当的公式说明（一次投入 ⇒ 在制按 100% 计材料）。
pub const FORMULA_MATERIAL: &str = "材料约当 = 本期完工 + 期末在制 × 100%（材料开工一次投入，在制全额计料）";

/// 加工约当的公式说明（人工 / 制造费用按完工率折算）。
pub const FORMULA_CONVERSION: &str = "加工约当 = 本期完工 + 期末在制 × 完工率（人工与制造费用同一分母）";

/// 完工数量的口径说明（累计 − 前期已结算，避免跨月重复转出）。
pub const FORMULA_COMPLETED: &str = "本期完工 = 成本对象累计完工 − 前期已结算计算单的完工合计（跨月对象不能用累计数）";

/// 约当产量的落库标度：与计算单 `equiv_material` / `equiv_conversion` 同列（4 位）。
pub const EQ_SCALE: u32 = cost_period_item_dto::MONEY_SCALE;

// ==================== 行状态 ====================

/// 一行的形状（能不能算、算出来是什么形状）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquivState {
    /// 期末在制 > 0：材料约当 与 加工约当 不同。
    Wip,
    /// 期末在制 = 0：两列都等于本期完工。
    Finished,
    /// 这一行算不出来（见 [`EquivIssue::code`]）。
    Blocked,
}

impl EquivState {
    /// 出参值（前端判色 / 判图标用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Wip => "wip",
            Self::Finished => "finished",
            Self::Blocked => "blocked",
        }
    }

    /// 中文名（筛选项与列标用，2–4 字）。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Wip => "有在制",
            Self::Finished => "全部完工",
            Self::Blocked => "算不出来",
        }
    }
}

/// 列表的状态筛选（**只读筛选用**，不影响算数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquivStateFilter {
    /// 全部行。
    All,
    /// 期末在制 > 0。
    Wip,
    /// 期末在制 = 0（本期全部完工）。
    Finished,
    /// 算不出来的行（需要先修数据）。
    Blocked,
    /// 与本期计算单对不上的行（需要重算）。
    Drift,
}

impl EquivStateFilter {
    /// 入参值 → 枚举（缺省 / 空串 = `all`；拼错 400，不静默回落）。
    pub fn parse(raw: Option<&str>) -> std::result::Result<Self, ApiError> {
        let text = raw.map(str::trim).unwrap_or("").to_ascii_lowercase();
        match text.as_str() {
            "" | "all" => Ok(Self::All),
            "wip" => Ok(Self::Wip),
            "finished" => Ok(Self::Finished),
            "blocked" => Ok(Self::Blocked),
            "drift" => Ok(Self::Drift),
            other => Err(bad_request(&format!(
                "state（筛选）只能是 all / wip / finished / blocked / drift，收到 {other}"
            ))),
        }
    }

    /// 出参值。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Wip => "wip",
            Self::Finished => "finished",
            Self::Blocked => "blocked",
            Self::Drift => "drift",
        }
    }

    /// 中文名（下拉项用）。
    pub fn label(&self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Wip => "有在制",
            Self::Finished => "全部完工",
            Self::Blocked => "算不出来",
            Self::Drift => "与计算单不一致",
        }
    }

    /// 这一行是否命中筛选。
    ///
    /// `drift` **不在** [`EquivState`] 里：它是「表值 vs 计算单」的对比结果，
    /// 与行形状正交（有在制的行也可能需要重算）。
    pub fn matches(&self, calc: &EquivCalc) -> bool {
        match self {
            Self::All => true,
            Self::Wip => calc.state == EquivState::Wip,
            Self::Finished => calc.state == EquivState::Finished,
            Self::Blocked => calc.state == EquivState::Blocked,
            Self::Drift => calc.recalc_required,
        }
    }
}

/// 全部筛选档（给前端下拉用；顺序即界面顺序）。
pub const STATE_FILTERS: [EquivStateFilter; 5] = [
    EquivStateFilter::All,
    EquivStateFilter::Wip,
    EquivStateFilter::Finished,
    EquivStateFilter::Blocked,
    EquivStateFilter::Drift,
];

// ==================== 输入事实 ====================

/// 本期计算单里同一对象的快照（没有批次 / 没算过 ⇒ `None`）。
#[derive(Debug, Clone, PartialEq)]
pub struct StoredCalc {
    /// 计算单行状态（`CALC` / `SETTLED`）。
    pub status: String,
    /// 计算单里的本期完工数量。
    pub completed_qty: Decimal,
    /// 计算单里的期末在制数量。
    pub wip_qty: Decimal,
    /// 计算单里的完工率。
    pub completion_pct: Decimal,
    /// 计算单里的材料约当。
    pub equiv_material: Decimal,
    /// 计算单里的加工约当。
    pub equiv_conversion: Decimal,
}

impl StoredCalc {
    /// 这一行是否已结算（已结算的漂移只能红冲重算，不能直接重算 CALC）。
    pub fn settled(&self) -> bool {
        self.status == "SETTLED"
    }
}

/// 算一行约当产量所需的**全部**事实（纯数据，零 DB）。
#[derive(Debug, Clone, PartialEq)]
pub struct EquivFacts {
    /// 成本对象号（报文点名用）。
    pub object_no: String,
    /// 投料方式（一期只有 `ONE_TIME` 参与材料约当=100%）。
    pub material_input_mode: String,
    /// 累计完工数量（`biz_cost_object.completed_qty`）。
    pub cumulative_completed_qty: Decimal,
    /// 前期**已结算**计算单的完工合计（本期完工 = 累计 − 它）。
    pub prior_completed_qty: Decimal,
    /// 期末在制数量。
    pub wip_qty: Decimal,
    /// 期末在制完工率 %（0–100）。
    pub completion_pct: Decimal,
    /// 本期归集到本对象的成本（判 `NO_OUTPUT` 用，不参与约当算数）。
    pub collected_total: Decimal,
    /// 上期已结算计算单的期末在制数量（判 `NO_OUTPUT` / 跨月对象是否该在表里）。
    pub prior_wip_end_qty: Decimal,
    /// 本期计算单快照（无批次 / 没算过 / 该对象没算出计算单 ⇒ `None`）。
    pub stored: Option<StoredCalc>,
}

// ==================== 问题 / 迁移字段 ====================

/// 一条「这行算不出来」的原因（`code` 稳定，前端可判；`message` 说事实；`fix` 说怎么修）。
#[derive(Debug, Clone, PartialEq)]
pub struct EquivIssue {
    /// 稳定码（`WIP_PCT_MISSING` / `COMPLETED_NEGATIVE` / `NO_OUTPUT` /
    /// `PCT_OUT_OF_RANGE` / `MATERIAL_MODE_UNSUPPORTED`）。
    pub code: &'static str,
    /// 事实说明。
    pub message: String,
    /// 怎么修。
    pub fix: String,
}

/// 表值与计算单的一个差异字段。
#[derive(Debug, Clone, PartialEq)]
pub struct DriftField {
    /// 字段名（中文，前端直接当列头）。
    pub field: &'static str,
    /// 表里（成本对象页当前）的值。
    pub table_value: Decimal,
    /// 计算单里的值。
    pub calc_value: Decimal,
}

// ==================== 一行的计算结果 ====================

/// 一行的约当产量结果（含状态 / 问题 / 警告 / 与计算单的差异）。
#[derive(Debug, Clone, PartialEq)]
pub struct EquivCalc {
    /// 本期完工数量（= 累计 − 前期已结算）。
    pub completed_qty: Decimal,
    /// 期末在制折算成完工品的数量（材料口径：= WQ）。
    pub wip_equiv_material: Option<Decimal>,
    /// 期末在制折算成完工品的数量（加工口径：= WQ × P/100）。
    pub wip_equiv_conversion: Option<Decimal>,
    /// 材料约当产量（4 位；算不出来 ⇒ `None`）。
    pub equiv_material: Option<Decimal>,
    /// 加工约当产量（4 位；算不出来 ⇒ `None`）。
    pub equiv_conversion: Option<Decimal>,
    /// 行形状。
    pub state: EquivState,
    /// 算不出来的原因（`state = blocked` 时非空）。
    pub issues: Vec<EquivIssue>,
    /// 只提示不拦的提醒（如「在制按 100% 计料」「完工率填了但没在制」）。
    pub warnings: Vec<String>,
    /// 表值与本期计算单不一致 ⇒ 需要重算。
    pub recalc_required: bool,
    /// 具体差在哪些字段。
    pub drift: Vec<DriftField>,
}

impl EquivCalc {
    /// 是否有可用的约当产量（两列都有数）。
    pub fn complete(&self) -> bool {
        self.equiv_material.is_some() && self.equiv_conversion.is_some()
    }

    /// 第一类问题码（报文 / 汇总用）。
    pub fn first_issue(&self) -> Option<&'static str> {
        self.issues.first().map(|issue| issue.code)
    }
}

// ==================== 算数 ====================

/// 4 位数量（与计算单 `equiv_*` 列的标度一致）。
pub fn eq_qty(value: Decimal) -> Decimal {
    value.round_dp(EQ_SCALE)
}

/// 算一行的约当产量（**纯函数**：不连库、不读时钟、不写日志）。
///
/// 详见模块头的公式与三种状态。任何「算不出来」的情形都**不**回落成 0：
/// 写 0 会被读成「没有在制 / 没有产量」，而真相是「缺一个输入」。
pub fn equiv_of(facts: &EquivFacts) -> EquivCalc {
    let completed = facts.cumulative_completed_qty - facts.prior_completed_qty;
    let pct = facts.completion_pct;
    let wip = facts.wip_qty;

    let mut issues: Vec<EquivIssue> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    // 材料侧 / 加工侧各自能不能算（两侧的输入不同：材料不看完工率）
    let mut material_ok = true;
    let mut conversion_ok = true;

    // ① 投料方式：一期只有「开工一次投入」才允许材料按 100% 计
    if facts.material_input_mode != cost_period_item_dto::INPUT_MODE_ONE_TIME {
        material_ok = false;
        issues.push(EquivIssue {
            code: "MATERIAL_MODE_UNSUPPORTED",
            message: format!(
                "投料方式是 {}，材料不是「开工一次投入」——材料约当不能按 100% 计",
                facts.material_input_mode
            ),
            fix: "把成本对象的投料方式改成「开工一次投入」（按进度投料属三期）；或先不核算本期料成本".to_string(),
        });
    }

    // ② 完工率值域（库有 ck_biz_cost_object_pct，这里是第二道防线）
    if pct < Decimal::ZERO || pct > Decimal::ONE_HUNDRED {
        conversion_ok = false;
        issues.push(EquivIssue {
            code: "PCT_OUT_OF_RANGE",
            message: format!("完工率 {}% 不在 0–100 之间", pct_brief(pct)),
            fix: "在成本对象页把完工率改到 0–100 之间（列是 NUMERIC(5,2)）".to_string(),
        });
    }

    // ③ 本期完工不能是负的（累计完工被改小了 / 前期已结算完工更大）
    if completed < Decimal::ZERO {
        material_ok = false;
        conversion_ok = false;
        issues.push(EquivIssue {
            code: "COMPLETED_NEGATIVE",
            message: format!(
                "累计完工 {} 小于前期已结算计算单的完工合计 {}，本期完工算出来是 {}",
                qty_text(facts.cumulative_completed_qty),
                qty_text(facts.prior_completed_qty),
                qty_text(completed)
            ),
            fix: "核对成本对象页的累计完工数量（是不是被改小了）：要比前期已结算的完工合计大；\
                  若前期算错了，先红冲前期批次再重算"
                .to_string(),
        });
    }

    // ④ 在制 > 0 但完工率 0：加工约当算不出来（与 calc 66067 同一判据）
    if wip > Decimal::ZERO && pct <= Decimal::ZERO {
        conversion_ok = false;
        issues.push(EquivIssue {
            code: "WIP_PCT_MISSING",
            message: format!(
                "期末在制 {} 但完工率是 0%：加工约当（= 完工 + 在制 × 完工率）算不出来",
                qty_text(wip)
            ),
            fix: "在成本对象页填写期末在制的完工进度（0 < 完工率 ≤ 100）；不填的话跑期间计算会被 66067 拦下"
                .to_string(),
        });
    }

    // ⑤ 本期既无完工也无期末在制：这行的约当是空的，画 0 会被读成「都完工了」
    if completed.is_zero() && wip.is_zero() {
        material_ok = false;
        conversion_ok = false;
        let why = if facts.collected_total > Decimal::ZERO {
            format!("本期已归集 {} 元成本", money_text(facts.collected_total))
        } else if facts.prior_wip_end_qty > Decimal::ZERO {
            format!("上期还挂着期末在制 {}", qty_text(facts.prior_wip_end_qty))
        } else {
            "本期没有投入".to_string()
        };
        issues.push(EquivIssue {
            code: "NO_OUTPUT",
            message: format!("本期既无完工也无期末在制（{why}）：约当产量无从谈起"),
            fix: "若是「本期全部完工」，把累计完工填成实际入库数量；若是「跨月继续生产」，\
                  填期末在制数量与完工率"
                .to_string(),
        });
    }

    // ---- 约当产量（复用 calc 第 6 段的算式与取整口径）----
    let (eq_material, eq_conversion) = cost_period_item_dto::equivalents(completed, wip, pct);
    let wip_equiv_material = material_ok.then(|| eq_qty(wip));
    let wip_equiv_conversion = conversion_ok.then(|| eq_qty(wip * pct / Decimal::ONE_HUNDRED));
    let equiv_material = material_ok.then(|| eq_qty(eq_material));
    let equiv_conversion = conversion_ok.then(|| eq_qty(eq_conversion));

    // ---- 只提示不拦 ----
    if wip > Decimal::ZERO && pct >= Decimal::ONE_HUNDRED {
        warnings.push(format!(
            "期末在制 {} 按完工率 100% 计加工约当（等同于完工品）：如果这批已经做完了，\
             应计入完工数量而不是留在期末在制",
            qty_text(wip)
        ));
    }
    if wip.is_zero() && pct > Decimal::ZERO {
        warnings.push(format!(
            "完工率填了 {}% 但期末在制是 0：本期完工率不参与约当（材料与加工都等于本期完工 {}）",
            pct_brief(pct),
            qty_text(completed)
        ));
    }
    if completed.is_zero() && wip > Decimal::ZERO {
        warnings.push(format!(
            "本期没有完工，全部成本留在期末在制（加工约当 = {} × {}% = {}）",
            qty_text(wip),
            pct_brief(pct),
            wip_equiv_conversion.map(qty_text).unwrap_or_else(|| "—".to_string())
        ));
    }

    // ---- 与本期计算单对拍 ----
    let drift = facts
        .stored
        .as_ref()
        .map(|stored| drift_of(facts, &completed, stored))
        .unwrap_or_default();
    let recalc_required = !drift.is_empty();

    let state = if !material_ok || !conversion_ok {
        EquivState::Blocked
    } else if wip > Decimal::ZERO {
        EquivState::Wip
    } else {
        EquivState::Finished
    };

    EquivCalc {
        completed_qty: completed,
        wip_equiv_material,
        wip_equiv_conversion,
        equiv_material,
        equiv_conversion,
        state,
        issues,
        warnings,
        recalc_required,
        drift,
    }
}

/// 表值 vs 计算单：逐字段对比，列全了才叫「一致」。
///
/// 比对口径：数量 4 位、金额 4 位、完工率按原样（库列 `NUMERIC(5,2)`）。
fn drift_of(facts: &EquivFacts, completed: &Decimal, stored: &StoredCalc) -> Vec<DriftField> {
    let mut drift = Vec::new();
    let mut push_if_diff = |field: &'static str, table_value: Decimal, calc_value: Decimal| {
        if table_value != calc_value {
            drift.push(DriftField {
                field,
                table_value,
                calc_value,
            });
        }
    };
    push_if_diff("本期完工数量", *completed, stored.completed_qty);
    push_if_diff("期末在制数量", facts.wip_qty, stored.wip_qty);
    push_if_diff("完工率", facts.completion_pct, stored.completion_pct);
    // 输入一致但约当不同 ⇒ 计算单不是按本口径算的（换过公式 / 手改过数），必须报出来
    let (eq_material, eq_conversion) =
        cost_period_item_dto::equivalents(*completed, facts.wip_qty, facts.completion_pct);
    push_if_diff("材料约当", eq_qty(eq_material), stored.equiv_material);
    push_if_diff("加工约当", eq_qty(eq_conversion), stored.equiv_conversion);
    drift
}

/// 漂移的一句话说明（前端 tooltip / 列表展开用）。
pub fn drift_hint(calc: &EquivCalc, stored: &StoredCalc) -> String {
    if calc.drift.is_empty() {
        return String::new();
    }
    let pairs: Vec<String> = calc
        .drift
        .iter()
        .map(|item| format!("{}：表里 {} / 计算单 {}", item.field, item.table_value, item.calc_value))
        .collect();
    let tail = if stored.settled() {
        "该计算单已结算：改完工率 / 在制后不能直接重算，需红冲本期批次再重算"
    } else {
        "重算本期（POST /business/erp/cost-period/calc）后两侧就会一致"
    };
    format!("与本期计算单不一致（{}）。{tail}", pairs.join("；"))
}

// ==================== 合计 ====================

/// 整张表的合计（**不是当前页**：翻页时这个数不能变）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EquivTotals {
    /// 行数。
    pub row_count: u64,
    /// 有在制的行数。
    pub wip_row_count: u64,
    /// 全部完工的行数。
    pub finished_row_count: u64,
    /// 算不出来的行数。
    pub blocked_row_count: u64,
    /// 与计算单不一致的行数。
    pub drift_row_count: u64,
    /// 本期完工数量合计。
    pub completed_qty: Decimal,
    /// 期末在制数量合计。
    pub wip_qty: Decimal,
    /// 期末在制折材料约当合计（只有能算的行参与）。
    pub wip_equiv_material: Decimal,
    /// 期末在制折加工约当合计（只有能算的行参与）。
    pub wip_equiv_conversion: Decimal,
    /// 材料约当产量合计（只有能算的行参与）。
    pub equiv_material: Decimal,
    /// 加工约当产量合计（只有能算的行参与）。
    pub equiv_conversion: Decimal,
}

impl EquivTotals {
    /// 累加一行。
    ///
    /// 数量合计（完工 / 在制）**算所有行**（它们是事实，不因缺完工率而消失）；
    /// 约当合计只累加**整行能算**的行 —— 算不出来的行如果按 0 加进去，
    /// 合计会悄悄变小，像「这些在制不存在」一样；而只加它的材料侧（加工侧算不出来）
    /// 又会给出一个「半个分母」，读的人分不清这是不是定数。整行跳过最诚实：
    /// 合计旁边就写着「有 N 行算不出来」。
    pub fn add(&mut self, facts: &EquivFacts, calc: &EquivCalc) {
        self.row_count += 1;
        match calc.state {
            EquivState::Wip => self.wip_row_count += 1,
            EquivState::Finished => self.finished_row_count += 1,
            EquivState::Blocked => self.blocked_row_count += 1,
        }
        if calc.recalc_required {
            self.drift_row_count += 1;
        }
        self.completed_qty += calc.completed_qty;
        self.wip_qty += facts.wip_qty;
        if calc.state == EquivState::Blocked {
            return;
        }
        self.wip_equiv_material += calc.wip_equiv_material.unwrap_or_default();
        self.wip_equiv_conversion += calc.wip_equiv_conversion.unwrap_or_default();
        self.equiv_material += calc.equiv_material.unwrap_or_default();
        self.equiv_conversion += calc.equiv_conversion.unwrap_or_default();
    }
}

// ==================== 出参 ====================

/// `GET /business/erp/cost-wip-equiv` 的入参。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct EquivQuery {
    /// 期间（必填，`yyyy-MM`）。
    pub period_code: Option<String>,
    /// 关键字（对象号 / 生产订单号 / 批号 / 产出品编码 / 产出品名称）。
    pub keyword: Option<String>,
    /// 状态筛选（`all` / `wip` / `finished` / `blocked` / `drift`；缺省 `all`）。
    pub state: Option<String>,
    /// 页码（缺省 1）。
    pub page: Option<u64>,
    /// 每页条数（缺省 20，上限 200）。
    pub page_size: Option<u64>,
}

/// 归一化后的筛选（纯逻辑可单测）。
#[derive(Debug, Clone, PartialEq)]
pub struct EquivFilter {
    /// 期间编码。
    pub period_code: String,
    /// 关键字（已 trim，空串 ⇒ `None`）。
    pub keyword: Option<String>,
    /// 状态筛选。
    pub state: EquivStateFilter,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 一行（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct EquivRow {
    /// 成本对象 id。
    pub object_id: String,
    /// 对象号。
    pub object_no: String,
    /// 对象类型（`BATCH` / …）。
    pub object_type: String,
    /// 生产订单号。
    pub source_doc_no: Option<String>,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 产出品 id。
    pub product_id: String,
    /// 产出品编码。
    pub product_code: Option<String>,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 生产车间 id。
    pub workshop_dept_id: String,
    /// 生产车间名称。
    pub workshop_dept_name: Option<String>,
    /// 计划数量。
    pub plan_qty: String,
    /// 累计完工数量。
    pub cumulative_completed_qty: String,
    /// 前期已结算完工合计。
    pub prior_completed_qty: String,
    /// 本期完工数量。
    pub completed_qty: String,
    /// 期末在制数量。
    pub wip_qty: String,
    /// 完工率 %。
    pub completion_pct: String,
    /// 期末在制折材料约当（`null` = 材料侧算不出来）。
    pub wip_equiv_material: Option<String>,
    /// 期末在制折加工约当（`null` = 加工侧算不出来）。
    pub wip_equiv_conversion: Option<String>,
    /// 材料约当产量（`null` = 算不出来）。
    pub equiv_material: Option<String>,
    /// 加工约当产量（`null` = 算不出来）。
    pub equiv_conversion: Option<String>,
    /// 行状态（`wip` / `finished` / `blocked`）。
    pub state: String,
    /// 行状态中文名。
    pub state_label: String,
    /// 需要重算（表值与本期计算单不一致）。
    pub recalc_required: bool,
    /// 本期计算单快照（没批次 / 没算过 ⇒ `null`）。
    pub calc: Option<CalcView>,
    /// 算不出来的原因。
    pub issues: Vec<IssueView>,
    /// 只提示不拦的提醒。
    pub warnings: Vec<String>,
}

/// 计算单快照（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CalcView {
    /// 计算单状态（`CALC` / `SETTLED`）。
    pub status: String,
    /// 计算单里的完工数量。
    pub completed_qty: String,
    /// 计算单里的期末在制数量。
    pub wip_qty: String,
    /// 计算单里的完工率。
    pub completion_pct: String,
    /// 计算单里的材料约当。
    pub equiv_material: String,
    /// 计算单里的加工约当。
    pub equiv_conversion: String,
    /// 两侧一致（含约当）。
    pub agrees: bool,
    /// 差异字段（`agrees = false` 时非空）。
    pub drift: Vec<DriftView>,
    /// 一句话说明（一致 / 怎么修）。
    pub note: String,
}

/// 差异字段（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DriftView {
    /// 字段名（中文）。
    pub field: String,
    /// 表里的值。
    pub table_value: String,
    /// 计算单里的值。
    pub calc_value: String,
}

/// 一条问题（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct IssueView {
    /// 稳定码。
    pub code: String,
    /// 事实。
    pub message: String,
    /// 怎么修。
    pub fix: String,
}

/// 合计（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TotalsView {
    /// 行数。
    pub row_count: u64,
    /// 有在制的行数。
    pub wip_row_count: u64,
    /// 全部完工的行数。
    pub finished_row_count: u64,
    /// 算不出来的行数。
    pub blocked_row_count: u64,
    /// 与计算单不一致的行数。
    pub drift_row_count: u64,
    /// 本期完工数量合计。
    pub completed_qty: String,
    /// 期末在制数量合计。
    pub wip_qty: String,
    /// 期末在制折材料约当合计。
    pub wip_equiv_material: String,
    /// 期末在制折加工约当合计。
    pub wip_equiv_conversion: String,
    /// 材料约当合计。
    pub equiv_material: String,
    /// 加工约当合计。
    pub equiv_conversion: String,
    /// 合计口径的一句话（**算不出来的行不参与约当合计**）。
    pub note: String,
}

/// 当期批次快照（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BatchView {
    /// 批次 id。
    pub id: String,
    /// 批次号。
    pub batch_no: String,
    /// 批次状态。
    pub status: String,
}

/// 整张表的出参。
#[derive(Debug, Clone, serde::Serialize)]
pub struct EquivRes {
    /// 期间。
    pub period_code: String,
    /// 当期成本核算批次（没建 ⇒ `null`，表是试算视图）。
    pub batch: Option<BatchView>,
    /// 是否已建批次（决定 `calc` 一列有没有）。
    pub cost_ready: bool,
    /// 状态筛选回显。
    pub state: String,
    /// 关键字回显。
    pub keyword: Option<String>,
    /// 材料约当公式。
    pub formula_material: String,
    /// 加工约当公式。
    pub formula_conversion: String,
    /// 本期完工公式。
    pub formula_completed: String,
    /// 合计（整张表，不受分页影响）。
    pub totals: TotalsView,
    /// 数据页。
    pub list: Vec<EquivRow>,
    /// 命中总行数。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 可选筛档（前端下拉）。
    pub state_options: Vec<OptionView>,
    /// 页脚说明（含「没批次」「有算不出来的行」等提示）。
    pub note: String,
}

/// 一个下拉选项。
#[derive(Debug, Clone, serde::Serialize)]
pub struct OptionView {
    /// 值。
    pub value: String,
    /// 中文名。
    pub label: String,
}

// ==================== 装配 ====================

/// 行 → 出参（纯函数：所有数字都已由 [`equiv_of`] 算好）。
pub fn row_view(head: &RowHead, facts: &EquivFacts, calc: &EquivCalc) -> EquivRow {
    EquivRow {
        object_id: head.object_id.clone(),
        object_no: head.object_no.clone(),
        object_type: head.object_type.clone(),
        source_doc_no: head.source_doc_no.clone(),
        lot_no: head.lot_no.clone(),
        product_id: head.product_id.clone(),
        product_code: head.product_code.clone(),
        product_name: head.product_name.clone(),
        workshop_dept_id: head.workshop_dept_id.clone(),
        workshop_dept_name: head.workshop_dept_name.clone(),
        plan_qty: head.plan_qty.clone(),
        cumulative_completed_qty: qty_text(facts.cumulative_completed_qty),
        prior_completed_qty: qty_text(facts.prior_completed_qty),
        completed_qty: qty_text(calc.completed_qty),
        wip_qty: qty_text(facts.wip_qty),
        completion_pct: pct_text(facts.completion_pct),
        wip_equiv_material: calc.wip_equiv_material.map(qty_text),
        wip_equiv_conversion: calc.wip_equiv_conversion.map(qty_text),
        equiv_material: calc.equiv_material.map(qty_text),
        equiv_conversion: calc.equiv_conversion.map(qty_text),
        state: calc.state.as_str().to_string(),
        state_label: calc.state.label().to_string(),
        recalc_required: calc.recalc_required,
        calc: facts.stored.as_ref().map(|stored| CalcView {
            status: stored.status.clone(),
            completed_qty: qty_text(stored.completed_qty),
            wip_qty: qty_text(stored.wip_qty),
            completion_pct: pct_text(stored.completion_pct),
            equiv_material: qty_text(stored.equiv_material),
            equiv_conversion: qty_text(stored.equiv_conversion),
            agrees: !calc.recalc_required,
            drift: calc
                .drift
                .iter()
                .map(|item| DriftView {
                    field: item.field.to_string(),
                    table_value: drift_text(item.field, item.table_value),
                    calc_value: drift_text(item.field, item.calc_value),
                })
                .collect(),
            note: if calc.recalc_required {
                drift_hint(calc, stored)
            } else {
                "表值与本期计算单一致（同一个完工数量、在制数量、完工率算出的同一组约当）".to_string()
            },
        }),
        issues: calc
            .issues
            .iter()
            .map(|issue| IssueView {
                code: issue.code.to_string(),
                message: issue.message.clone(),
                fix: issue.fix.clone(),
            })
            .collect(),
        warnings: calc.warnings.clone(),
    }
}

/// 行头（名称 / 编码 / 维度）——与约当算数无关的那部分。
///
/// 单独一个结构是为了让 [`row_view`] 保持纯函数（不依赖 model 的行类型），
/// 单测可以直接造一个行头来断言出参形状。
#[derive(Debug, Clone, PartialEq)]
pub struct RowHead {
    /// 成本对象 id。
    pub object_id: String,
    /// 对象号。
    pub object_no: String,
    /// 对象类型。
    pub object_type: String,
    /// 生产订单号。
    pub source_doc_no: Option<String>,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 产出品 id。
    pub product_id: String,
    /// 产出品编码。
    pub product_code: Option<String>,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 车间 id。
    pub workshop_dept_id: String,
    /// 车间名称。
    pub workshop_dept_name: Option<String>,
    /// 计划数量。
    pub plan_qty: String,
}

/// 合计 → 出参。
pub fn totals_view(totals: &EquivTotals) -> TotalsView {
    TotalsView {
        row_count: totals.row_count,
        wip_row_count: totals.wip_row_count,
        finished_row_count: totals.finished_row_count,
        blocked_row_count: totals.blocked_row_count,
        drift_row_count: totals.drift_row_count,
        completed_qty: qty_text(totals.completed_qty),
        wip_qty: qty_text(totals.wip_qty),
        wip_equiv_material: qty_text(totals.wip_equiv_material),
        wip_equiv_conversion: qty_text(totals.wip_equiv_conversion),
        equiv_material: qty_text(totals.equiv_material),
        equiv_conversion: qty_text(totals.equiv_conversion),
        note: format!(
            "合计是整张表的（不受分页影响）；约当合计只累加能算的行 —— \
             当前有 {} 行算不出来（它们的在制与完工仍计入数量合计）",
            totals.blocked_row_count
        ),
    }
}

/// 筛档 → 出参。
pub fn state_options() -> Vec<OptionView> {
    STATE_FILTERS
        .iter()
        .map(|item| OptionView {
            value: item.as_str().to_string(),
            label: item.label().to_string(),
        })
        .collect()
}

// ==================== 文本 ====================

/// 数量 → 文本（**4 位**，与库列 `NUMERIC(18,4)` / 计算单 `equiv_*` 标度一致）。
///
/// 固定标度是为了让「表值 vs 计算单」在前端逐字可比：`1000` 与 `1000.0000`
/// 在字符串层面不相等，会让人误以为有差异。前端只做展示，不重新算。
pub fn qty_text(value: Decimal) -> String {
    format!("{:.4}", value.round_dp(EQ_SCALE))
}

/// 完工率 → 文本（2 位，与 `NUMERIC(5,2)` 一致）。
pub fn pct_text(value: Decimal) -> String {
    format!("{:.2}", value.round_dp(2))
}

/// 完工率 → 报文里的短写法（去掉尾随 0：`60.00` ⇒ `60`，`33.50` ⇒ `33.5`）。
fn pct_brief(value: Decimal) -> String {
    value.normalize().to_string()
}

/// 差异字段 → 文本：完工率按 2 位，其它（数量 / 约当）按 4 位。
fn drift_text(field: &'static str, value: Decimal) -> String {
    if field == "完工率" {
        pct_text(value)
    } else {
        qty_text(value)
    }
}

/// 金额 → 文本（4 位）。
pub fn money_text(value: Decimal) -> String {
    value.round_dp(cost_period_item_dto::MONEY_SCALE).to_string()
}

// ==================== 错误 ====================

/// 400 + `PARAM_INVALID`（与成本域其它模块同形）。
pub fn bad_request(message: &str) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.to_string())
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数量 / 金额字面量")
    }

    fn facts() -> EquivFacts {
        EquivFacts {
            object_no: "CB-T-001".to_string(),
            material_input_mode: cost_period_item_dto::INPUT_MODE_ONE_TIME.to_string(),
            cumulative_completed_qty: dec("800"),
            prior_completed_qty: Decimal::ZERO,
            wip_qty: dec("200"),
            completion_pct: dec("50"),
            collected_total: dec("23000"),
            prior_wip_end_qty: Decimal::ZERO,
            stored: None,
        }
    }

    // ---------- 权限 ----------

    #[test]
    fn permission_reuses_cost_object_list() {
        assert_eq!(PERM_LIST, "business:erp:cost-object:list");
        assert_eq!(PERMISSIONS, [PERM_LIST]);
    }

    // ---------- 公式（ARCH-9 §4.6.2 算例 A） ----------

    #[test]
    fn design_example_a_equiv_is_1000_and_900() {
        let calc = equiv_of(&facts());
        assert_eq!(calc.state, EquivState::Wip);
        assert_eq!(calc.completed_qty, dec("800"));
        assert_eq!(calc.equiv_material, Some(dec("1000.0000")), "EQ_mat = 800 + 200");
        assert_eq!(calc.equiv_conversion, Some(dec("900.0000")), "EQ_cnv = 800 + 200×50%");
        assert_eq!(calc.wip_equiv_material, Some(dec("200.0000")));
        assert_eq!(calc.wip_equiv_conversion, Some(dec("100.0000")));
        assert!(calc.issues.is_empty());
        assert!(!calc.recalc_required);
        assert!(calc.complete());
    }

    #[test]
    fn equiv_matches_calc_layer_formula_verbatim() {
        // 与 calc 第 6 段同一算式（复用 equivalents）：逐点比对 random-ish 组合
        for (done, wip, pct) in [
            ("0", "100", "33.33"),
            ("120", "0", "0"),
            ("7", "3", "66.67"),
            ("1500.5", "499.5", "99.99"),
        ] {
            let mut f = facts();
            f.cumulative_completed_qty = dec(done);
            f.wip_qty = dec(wip);
            f.completion_pct = dec(pct);
            f.collected_total = dec("1");
            let calc = equiv_of(&f);
            let (mat, cnv) = cost_period_item_dto::equivalents(dec(done), dec(wip), dec(pct));
            assert_eq!(calc.equiv_material, Some(eq_qty(mat)), "材料约当口径必须与 calc 一致");
            assert_eq!(calc.equiv_conversion, Some(eq_qty(cnv)), "加工约当口径必须与 calc 一致");
        }
    }

    #[test]
    fn rounding_is_four_digits_like_the_item_table() {
        let mut f = facts();
        f.cumulative_completed_qty = dec("3");
        f.wip_qty = dec("1");
        f.completion_pct = dec("33.33");
        // 3 + 1×33.33% = 3.3333 → 4 位正好；再验证 1/3 类的循环小数被截到 4 位
        f.cumulative_completed_qty = dec("0");
        f.wip_qty = dec("1");
        f.completion_pct = dec("33.3333");
        let calc = equiv_of(&f);
        assert_eq!(calc.equiv_conversion, Some(dec("0.3333")), "落库标度 4 位");
        assert_eq!(EQ_SCALE, 4);
    }

    // ---------- 三种状态 ----------

    #[test]
    fn no_wip_means_both_equivalents_equal_completed() {
        let mut f = facts();
        f.wip_qty = Decimal::ZERO;
        f.completion_pct = dec("60");
        let calc = equiv_of(&f);
        assert_eq!(calc.state, EquivState::Finished);
        assert_eq!(calc.equiv_material, Some(dec("800.0000")));
        assert_eq!(calc.equiv_conversion, Some(dec("800.0000")), "没在制时完工率不参与");
        assert!(
            calc.warnings.iter().any(|text| text.contains("完工率填了 60%")),
            "要提示完工率被忽略"
        );
    }

    #[test]
    fn wip_with_zero_rate_blocks_conversion_but_keeps_material() {
        let mut f = facts();
        f.completion_pct = Decimal::ZERO;
        let calc = equiv_of(&f);
        assert_eq!(calc.state, EquivState::Blocked);
        assert_eq!(calc.first_issue(), Some("WIP_PCT_MISSING"));
        assert_eq!(calc.equiv_material, Some(dec("1000.0000")), "材料不看完工率，照算");
        assert_eq!(calc.equiv_conversion, None, "加工约当算不出来 ⇒ null，不是 0");
        assert_eq!(calc.wip_equiv_conversion, None);
        assert!(!calc.complete());
        assert!(calc.issues[0].fix.contains("66067"), "修法要点名 calc 的闸");
    }

    #[test]
    fn negative_completed_blocks_both_sides() {
        let mut f = facts();
        f.cumulative_completed_qty = dec("300");
        f.prior_completed_qty = dec("500");
        let calc = equiv_of(&f);
        assert_eq!(calc.state, EquivState::Blocked);
        assert_eq!(calc.first_issue(), Some("COMPLETED_NEGATIVE"));
        assert_eq!(calc.equiv_material, None);
        assert_eq!(calc.equiv_conversion, None);
        assert_eq!(calc.completed_qty, dec("-200"));
        assert!(calc.issues[0].message.contains("前期已结算"));
    }

    #[test]
    fn no_output_with_collected_cost_is_blocked_not_zero() {
        let mut f = facts();
        f.cumulative_completed_qty = Decimal::ZERO;
        f.wip_qty = Decimal::ZERO;
        f.collected_total = dec("5000");
        let calc = equiv_of(&f);
        assert_eq!(calc.state, EquivState::Blocked);
        assert_eq!(calc.first_issue(), Some("NO_OUTPUT"));
        assert_eq!(calc.equiv_material, None, "画 0 会被读成「全完工」");
        assert!(calc.issues[0].message.contains("5000"));
    }

    #[test]
    fn no_output_with_prior_wip_is_blocked_and_says_so() {
        let mut f = facts();
        f.cumulative_completed_qty = Decimal::ZERO;
        f.wip_qty = Decimal::ZERO;
        f.collected_total = Decimal::ZERO;
        f.prior_wip_end_qty = dec("120");
        let calc = equiv_of(&f);
        assert_eq!(calc.first_issue(), Some("NO_OUTPUT"));
        assert!(calc.issues[0].message.contains("上期还挂着期末在制 120"));
    }

    #[test]
    fn rate_out_of_range_is_blocked() {
        let mut f = facts();
        f.completion_pct = dec("120");
        let calc = equiv_of(&f);
        assert_eq!(calc.first_issue(), Some("PCT_OUT_OF_RANGE"));
        assert_eq!(calc.equiv_conversion, None);
    }

    #[test]
    fn non_one_time_material_mode_blocks_material_only() {
        let mut f = facts();
        f.material_input_mode = "PROGRESSIVE".to_string();
        let calc = equiv_of(&f);
        assert_eq!(calc.state, EquivState::Blocked);
        assert_eq!(calc.first_issue(), Some("MATERIAL_MODE_UNSUPPORTED"));
        assert_eq!(calc.equiv_material, None, "按进度投料不能按 100% 计料");
        assert_eq!(calc.equiv_conversion, Some(dec("900.0000")), "加工一侧不受投料方式影响");
    }

    #[test]
    fn wip_at_full_rate_warns_that_it_should_be_finished() {
        let mut f = facts();
        f.completion_pct = dec("100");
        let calc = equiv_of(&f);
        assert_eq!(calc.state, EquivState::Wip);
        assert_eq!(calc.equiv_conversion, Some(dec("1000.0000")));
        assert!(calc.warnings.iter().any(|text| text.contains("等同于完工品")));
    }

    // ---------- 与计算单对拍 ----------

    #[test]
    fn agreed_calc_item_leaves_no_drift() {
        let mut f = facts();
        f.stored = Some(StoredCalc {
            status: "CALC".to_string(),
            completed_qty: dec("800"),
            wip_qty: dec("200"),
            completion_pct: dec("50"),
            equiv_material: dec("1000.0000"),
            equiv_conversion: dec("900.0000"),
        });
        let calc = equiv_of(&f);
        assert!(!calc.recalc_required);
        assert!(calc.drift.is_empty());
    }

    #[test]
    fn changed_rate_marks_recalc_required_with_the_field() {
        let mut f = facts();
        f.completion_pct = dec("60");
        f.stored = Some(StoredCalc {
            status: "CALC".to_string(),
            completed_qty: dec("800"),
            wip_qty: dec("200"),
            completion_pct: dec("50"),
            equiv_material: dec("1000.0000"),
            equiv_conversion: dec("900.0000"),
        });
        let calc = equiv_of(&f);
        assert!(calc.recalc_required);
        let fields: Vec<&str> = calc.drift.iter().map(|item| item.field).collect();
        assert!(fields.contains(&"完工率"));
        assert!(fields.contains(&"加工约当"), "约当跟着变也要列出来");
        let hint = drift_hint(&calc, f.stored.as_ref().expect("有计算单"));
        assert!(hint.contains("重算"), "要告诉用户怎么修");
    }

    #[test]
    fn settled_calc_item_drift_says_reverse_not_recalc() {
        let mut f = facts();
        f.wip_qty = dec("260");
        f.stored = Some(StoredCalc {
            status: "SETTLED".to_string(),
            completed_qty: dec("800"),
            wip_qty: dec("200"),
            completion_pct: dec("50"),
            equiv_material: dec("1000.0000"),
            equiv_conversion: dec("900.0000"),
        });
        let calc = equiv_of(&f);
        let hint = drift_hint(&calc, f.stored.as_ref().expect("有计算单"));
        assert!(hint.contains("红冲"), "已结算的只能红冲重算");
    }

    #[test]
    fn stored_only_equiv_drift_is_reported_too() {
        // 输入全对、约当对不上 ⇒ 说明计算单不是按本口径算的（换过公式 / 手改过）
        let mut f = facts();
        f.stored = Some(StoredCalc {
            status: "CALC".to_string(),
            completed_qty: dec("800"),
            wip_qty: dec("200"),
            completion_pct: dec("50"),
            equiv_material: dec("999.0000"),
            equiv_conversion: dec("900.0000"),
        });
        let calc = equiv_of(&f);
        assert!(calc.recalc_required);
        assert_eq!(calc.drift.len(), 1);
        assert_eq!(calc.drift[0].field, "材料约当");
    }

    // ---------- 合计 ----------

    #[test]
    fn totals_skip_blocked_rows_for_equiv_but_keep_quantities() {
        let mut totals = EquivTotals::default();
        let good = facts();
        let good_calc = equiv_of(&good);
        totals.add(&good, &good_calc);

        let mut bad = facts();
        bad.completion_pct = Decimal::ZERO; // WIP_PCT_MISSING
        let bad_calc = equiv_of(&bad);
        totals.add(&bad, &bad_calc);

        assert_eq!(totals.row_count, 2);
        assert_eq!(totals.wip_row_count, 1);
        assert_eq!(totals.blocked_row_count, 1);
        assert_eq!(totals.completed_qty, dec("1600"), "数量合计含所有行");
        assert_eq!(totals.wip_qty, dec("400"));
        assert_eq!(totals.equiv_material, dec("1000.0000"), "算不出来的行不参与约当合计");
        assert_eq!(totals.equiv_conversion, dec("900.0000"));
        assert_eq!(totals.wip_equiv_conversion, dec("100.0000"));
        let view = totals_view(&totals);
        assert!(view.note.contains("1 行算不出来"));
    }

    // ---------- 入参 ----------

    #[test]
    fn state_filter_parses_and_rejects_typos() {
        assert_eq!(EquivStateFilter::parse(None).expect("缺省"), EquivStateFilter::All);
        assert_eq!(EquivStateFilter::parse(Some(" ")).expect("空串"), EquivStateFilter::All);
        assert_eq!(
            EquivStateFilter::parse(Some("WIP")).expect("大小写不敏感"),
            EquivStateFilter::Wip
        );
        assert_eq!(
            EquivStateFilter::parse(Some("drift")).expect("漂移"),
            EquivStateFilter::Drift
        );
        let error = EquivStateFilter::parse(Some("wipp")).expect_err("拼错必须 400");
        assert_eq!(error.status, 400);
    }

    #[test]
    fn state_filter_matches_rows() {
        let wip_calc = equiv_of(&facts());
        assert!(EquivStateFilter::Wip.matches(&wip_calc));
        assert!(!EquivStateFilter::Finished.matches(&wip_calc));
        assert!(!EquivStateFilter::Blocked.matches(&wip_calc));
        assert!(EquivStateFilter::All.matches(&wip_calc));

        let mut bad = facts();
        bad.wip_qty = Decimal::ZERO;
        bad.cumulative_completed_qty = Decimal::ZERO;
        bad.collected_total = dec("1");
        let blocked = equiv_of(&bad);
        assert!(EquivStateFilter::Blocked.matches(&blocked));

        let mut drift = facts();
        drift.stored = Some(StoredCalc {
            status: "CALC".to_string(),
            completed_qty: dec("700"),
            wip_qty: dec("200"),
            completion_pct: dec("50"),
            equiv_material: dec("900.0000"),
            equiv_conversion: dec("800.0000"),
        });
        let drift_calc = equiv_of(&drift);
        assert!(EquivStateFilter::Drift.matches(&drift_calc));
        assert!(EquivStateFilter::Wip.matches(&drift_calc), "漂移与行形状正交");
    }

    #[test]
    fn state_options_cover_all_five_filters() {
        let options = state_options();
        let values: Vec<&str> = options.iter().map(|item| item.value.as_str()).collect();
        assert_eq!(values, vec!["all", "wip", "finished", "blocked", "drift"]);
        assert_eq!(options[0].label, "全部");
    }

    // ---------- 出参 ----------

    #[test]
    fn row_view_renders_nulls_as_missing_not_zero() {
        let head = RowHead {
            object_id: "0190".to_string(),
            object_no: "CB-T-001".to_string(),
            object_type: "BATCH".to_string(),
            source_doc_no: Some("PO-001".to_string()),
            lot_no: None,
            product_id: "0191".to_string(),
            product_code: Some("M-001".to_string()),
            product_name: Some("测试成品".to_string()),
            workshop_dept_id: "0192".to_string(),
            workshop_dept_name: Some("一号车间".to_string()),
            plan_qty: "1000".to_string(),
        };
        let mut f = facts();
        f.completion_pct = Decimal::ZERO;
        let calc = equiv_of(&f);
        let row = row_view(&head, &f, &calc);
        assert_eq!(row.state, "blocked");
        assert_eq!(row.state_label, "算不出来");
        assert_eq!(row.equiv_conversion, None, "算不出来 ⇒ null，绝不画 0");
        assert_eq!(row.equiv_material, Some("1000.0000".to_string()));
        assert_eq!(row.issues.len(), 1);
        assert_eq!(row.issues[0].code, "WIP_PCT_MISSING");
        assert!(row.calc.is_none(), "没算过 ⇒ calc 列为 null");
    }

    #[test]
    fn row_view_shows_calc_snapshot_and_note() {
        let head = RowHead {
            object_id: "0190".to_string(),
            object_no: "CB-T-001".to_string(),
            object_type: "BATCH".to_string(),
            source_doc_no: None,
            lot_no: None,
            product_id: "0191".to_string(),
            product_code: None,
            product_name: None,
            workshop_dept_id: "0192".to_string(),
            workshop_dept_name: None,
            plan_qty: "1000".to_string(),
        };
        let mut f = facts();
        f.stored = Some(StoredCalc {
            status: "CALC".to_string(),
            completed_qty: dec("800"),
            wip_qty: dec("200"),
            completion_pct: dec("50"),
            equiv_material: dec("1000.0000"),
            equiv_conversion: dec("900.0000"),
        });
        let calc = equiv_of(&f);
        let row = row_view(&head, &f, &calc);
        let view = row.calc.expect("有计算单就有快照");
        assert!(view.agrees);
        assert_eq!(view.status, "CALC");
        assert_eq!(view.equiv_conversion, "900.0000");
        assert!(view.note.contains("一致"));
        assert!(view.drift.is_empty());
    }

    #[test]
    fn formula_text_is_quoted_verbatim() {
        assert!(FORMULA_MATERIAL.contains("× 100%"));
        assert!(FORMULA_CONVERSION.contains("× 完工率"));
        assert!(FORMULA_COMPLETED.contains("累计完工"));
    }
}
