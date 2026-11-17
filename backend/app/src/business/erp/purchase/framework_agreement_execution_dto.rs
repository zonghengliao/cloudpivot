//! 框架协议执行量统计契约（M4-09）：**已用 / 剩余**。
//!
//! 数据来源是 V0452 的两个视图（见 `backend/app/migrations/business/V0452__framework_agreement_execution.up.sql`）：
//!
//! ```text
//! v_framework_agreement_item_execution   物料行级：一行 = 协议的一格（物料）
//! v_framework_agreement_execution        单头级：一张存活协议一行（物料行合计 + 行状态计数）
//! ```
//!
//! # 口径（每一条都有纯函数 + 单测钉住，与迁移头注释逐字一致）
//!
//! ```text
//! ① 已用量   = Σ 引用该协议行的采购明细 qty，且采购单「存活且未作废」
//!              —— 草稿也算（量已占住，与合同额度守卫 committed_amount 同口径）；
//!                 作废 / 软删不算（链已断，只进 released_doc_count 供解释）
//! ② 已用金额 = 按协议价口径二选一：协议价含税取 Σ with_tax_amount，否则取 Σ amount
//!              —— 与 M4-08 金额上限比较同一口径（换口径比金额就是白送 / 白扣一个税率）
//! ③ 剩余     = 封顶 − 已用（**可为负**：超上限执行，V0450 刻意不在库层拦）
//!              封顶为 NULL（不限）⇒ 剩余是 NULL，**不是** 0
//! ④ 执行率   = 已用 / 封顶（0.4 = 40%），同时给 0–100 的两位小数百分比
//! ⑤ 行状态   = UNLIMITED / NOT_STARTED / PARTIAL / DONE / OVER
//! ⑥ 单头封顶 = `total_max_qty`；没写则取「明细行上限合计」，但**只有明细全部设了上限**
//!              （`unlimited_items = 0`）时它才是有效封顶 —— 有一行不限，整体就不封顶
//! ⑦ 有效期   = `APPROVED AND start_date <= 当日 <= end_date`（与
//!              `framework_agreement_model::is_effective_on` 同一判据）；
//!              `validity_state` 只是把它翻译成人话，不落库
//! ```
//!
//! # 为什么不复用回写列 `executed_qty`
//!
//! 回写列是 M4-08 在下单 / 作废 / 删除时**按单据链重算**出来的快照。它只在「有人动过单」
//! 的那一刻对，绕过 handler 改库（数据修复 / 导入 / 并发失败残留）就会与单据链分叉。
//! 报表拿它当真相 = 多一份要对账的账；视图现算 = 永远自愈。两个数**并排给**
//! （`booked_qty` / `executed_qty`），差就是分叉，前端直接显形。
//!
//! # 权限码
//!
//! 一个接口码：`business:erp:framework-agreement:execution`（两个只读接口共用）。
//! 页面显形码 `…:page` **不在这里**（AGENTS 约定 14：只给前端路由，后端零引用）。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use cloudpivot_kernel::shared::error_code;

use crate::business::erp::purchase::framework_agreement_model as fwa;

// ==================== 域常量 ====================

/// 本模块的**唯一**接口权限码（V0452 的按钮行 `…004481`）。
pub const EXEC_PERMISSION: &str = "business:erp:framework-agreement:execution";

/// 物料行级视图名（与 V0452 逐字一致）。
pub const ITEM_VIEW: &str = "v_framework_agreement_item_execution";
/// 单头级视图名（与 V0452 逐字一致）。
pub const HEAD_VIEW: &str = "v_framework_agreement_execution";

/// 执行率保留位数（视图 `round(…, 6)`；这里只做说明与纯函数对齐）。
pub const RATE_DP: u32 = 6;
/// 百分比保留位数（与采购计划执行率 / 预算执行报表同口径：两位小数）。
pub const PCT_DP: u32 = 2;

/// 行状态：未设上限（`max_qty IS NULL`）。
pub const STATE_UNLIMITED: &str = "UNLIMITED";
/// 行状态：未开工（已用 0）。
pub const STATE_NOT_STARTED: &str = "NOT_STARTED";
/// 行状态：部分执行（0 < 已用 < 上限）。
pub const STATE_PARTIAL: &str = "PARTIAL";
/// 行状态：恰好用满（已用 = 上限）。
pub const STATE_DONE: &str = "DONE";
/// 行状态：超上限（已用 > 上限，执行率 > 100%）。
pub const STATE_OVER: &str = "OVER";
/// 状态闭集（与视图 `exec_state` 的 CASE 逐字一致）。
pub const EXEC_STATES: [&str; 5] = [
    STATE_UNLIMITED,
    STATE_NOT_STARTED,
    STATE_PARTIAL,
    STATE_DONE,
    STATE_OVER,
];

/// 有效期状态：拟稿（不生效）。
pub const VALIDITY_DRAFT: &str = "DRAFT";
/// 有效期状态：审批中（不生效）。
pub const VALIDITY_SUBMITTED: &str = "SUBMITTED";
/// 有效期状态：已批准但未到生效日。
pub const VALIDITY_NOT_STARTED: &str = "NOT_STARTED";
/// 有效期状态：生效中。
pub const VALIDITY_ACTIVE: &str = "ACTIVE";
/// 有效期状态：已过截止日。
pub const VALIDITY_EXPIRED: &str = "EXPIRED";
/// 有效期状态闭集（与视图 `validity_state` 的 CASE 逐字一致）。
pub const VALIDITY_STATES: [&str; 5] = [
    VALIDITY_DRAFT,
    VALIDITY_SUBMITTED,
    VALIDITY_NOT_STARTED,
    VALIDITY_ACTIVE,
    VALIDITY_EXPIRED,
];

/// 关键字长度上限（与采购计划分页同口径）。
pub const MAX_KEYWORD_LEN: usize = 64;

/// 行状态中文名（报文 / 前端徽标共用）。未知名原样返回，绝不 panic。
pub fn exec_state_label(state: &str) -> &str {
    match state {
        STATE_UNLIMITED => "不限量",
        STATE_NOT_STARTED => "未开工",
        STATE_PARTIAL => "部分执行",
        STATE_DONE => "已用满",
        STATE_OVER => "已超限",
        other => other,
    }
}

/// 有效期状态中文名（同上）。
pub fn validity_state_label(state: &str) -> &str {
    match state {
        VALIDITY_DRAFT => "拟稿",
        VALIDITY_SUBMITTED => "审批中",
        VALIDITY_NOT_STARTED => "未生效",
        VALIDITY_ACTIVE => "生效中",
        VALIDITY_EXPIRED => "已过期",
        other => other,
    }
}

/// 定价方式中文名（同上）。
pub fn price_mode_label(mode: &str) -> &str {
    match mode {
        fwa::PRICE_MODE_FIXED => "固定价",
        fwa::PRICE_MODE_FLOATING => "随行就市",
        other => other,
    }
}

/// 协议状态中文名（只覆盖协议域的三个状态；未知名原样返回）。
/// 与 `framework_agreement_ref::status_label` **同一份措辞** —— 两个报表并排看时不许一个叫
/// 「生效」一个叫「已批准」。
pub fn agreement_status_label(status: &str) -> &str {
    match status {
        fwa::STATUS_DRAFT => "拟稿",
        fwa::STATUS_SUBMITTED => "审批中",
        fwa::STATUS_APPROVED => "生效",
        other => other,
    }
}

/// 400 参数错误（与其它 ERP 模块同一个出口）。
pub fn param_error(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 纯函数（handler / model / 单测共用） ====================

/// 剩余 = 封顶 − 已用（**故意保留负数**：超上限要能看出来）。
///
/// 封顶为 `None`（不限）时返回 `None`：0 会被读成「一点都不剩」，与「不限」正好相反。
pub fn remaining_of(cap: Option<Decimal>, used: Decimal) -> Option<Decimal> {
    cap.map(|cap| cap - used)
}

/// 执行率 = 已用 / 封顶（6 位小数）。
///
/// 封顶为 `None`（不限）或 `<= 0` 时返回 `None` 而不是 0：没有分母就没有比率。
/// 库层 `max_qty > 0` / `total_max_qty > 0` 已挡住 0，这里是对「绕过库层写进来的脏值」兜底。
pub fn rate_of(used: Decimal, cap: Option<Decimal>) -> Option<Decimal> {
    match cap {
        Some(cap) if cap > Decimal::ZERO => Some((used / cap).round_dp(RATE_DP)),
        _ => None,
    }
}

/// 百分比（0–100，两位小数）：前端进度条直接用。
pub fn pct_of(rate: Option<Decimal>) -> Option<Decimal> {
    rate.map(|value| (value * Decimal::from(100)).round_dp(PCT_DP))
}

/// 行状态判据（与视图的 CASE **逐字对应**，改一处必须两处一起改）。
///
/// * `cap = None` ⇒ UNLIMITED（**先**判，否则 `None` 会被后面的比较静默吃掉）
/// * `used > cap` ⇒ OVER；`used == cap` ⇒ DONE；`0 < used < cap` ⇒ PARTIAL；`used == 0` ⇒ NOT_STARTED
pub fn exec_state_of(cap: Option<Decimal>, used: Decimal) -> &'static str {
    match cap {
        None => STATE_UNLIMITED,
        Some(cap) => {
            if used > cap {
                STATE_OVER
            } else if used >= cap {
                STATE_DONE
            } else if used > Decimal::ZERO {
                STATE_PARTIAL
            } else {
                STATE_NOT_STARTED
            }
        }
    }
}

/// 有效期状态（与视图的 CASE **逐字对应**）。
///
/// 顺序刻意是「先看状态、再看日期」：`DRAFT` / `SUBMITTED` 一律不生效，
/// 不能因为「日期区间刚好对」就被判成 ACTIVE（草稿也能生效 = 没有审批）。
pub fn validity_state_of(status: &str, start_date: NaiveDate, end_date: NaiveDate, today: NaiveDate) -> &'static str {
    match status {
        fwa::STATUS_DRAFT => VALIDITY_DRAFT,
        fwa::STATUS_SUBMITTED => VALIDITY_SUBMITTED,
        _ => {
            if today < start_date {
                VALIDITY_NOT_STARTED
            } else if today > end_date {
                VALIDITY_EXPIRED
            } else {
                VALIDITY_ACTIVE
            }
        }
    }
}

/// 来源采购单到底算不算「已用」，以及**为什么不算**（下钻列表直接展示原因）。
///
/// * 采购单已软删 → 不算，原因「采购单已删除」
/// * 采购单已作废 → 不算，原因「采购单已作废」
/// * 其余（草稿 / 审批中 / 已审批 / 部分收货 / 已收货 / 已关闭）→ 算，原因「已用」
pub fn source_verdict(status: &str, deleted: bool) -> (bool, &'static str) {
    if deleted {
        return (false, "采购单已删除");
    }
    if status.trim() == "CANCELLED" {
        return (false, "采购单已作废");
    }
    (true, "已用")
}

/// 物料行口径的输入（只放算汇总要用的量，避免汇总函数依赖 model 行结构，单测不用连库）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageFacts {
    /// 该行的数量上限（None = 不限）。
    pub max_qty: Option<Decimal>,
    /// 已用量（按单据现算）。
    pub executed_qty: Decimal,
    /// 已用金额（按协议价口径）。
    pub executed_amount_capped: Decimal,
}

/// 物料行汇总（单头视图的同一套算法；真库用例用它交叉校验视图合计）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageSummary {
    /// 物料行数。
    pub item_count: i64,
    /// 不限量的行数。
    pub unlimited_items: i64,
    /// 超上限的行数。
    pub over_items: i64,
    /// 已用满的行数。
    pub done_items: i64,
    /// 部分执行的行数。
    pub partial_items: i64,
    /// 未开工的行数。
    pub not_started_items: i64,
    /// 已用量合计。
    pub executed_qty: Decimal,
    /// 已用金额合计（协议价口径）。
    pub executed_amount_capped: Decimal,
    /// 有效数量封顶（明细全部设了上限时的合计；有一行不限 → None）。
    pub cap_qty: Option<Decimal>,
    /// 剩余（None = 不限；可为负）。
    pub remaining_qty: Option<Decimal>,
    /// 数量执行率（None = 不限）。
    pub exec_qty_rate: Option<Decimal>,
    /// 单头状态（按数量封顶判；数量不限时按金额封顶判由调用方决定）。
    pub exec_state: &'static str,
}

impl UsageSummary {
    /// 执行率百分比（0–100，两位小数）。
    #[must_use]
    pub fn exec_qty_rate_pct(&self) -> Option<Decimal> {
        pct_of(self.exec_qty_rate)
    }
}

/// 由物料行汇总出单头（纯函数：单测与真库用例都用它交叉校验视图）。
///
/// 与视图 `v_framework_agreement_execution` 的 item_agg 段同一套算法；
/// 金额封顶不在这里算（明细没有金额上限字段，只认单头 `total_max_amount`）。
pub fn summary_of(facts: &[UsageFacts]) -> UsageSummary {
    let mut out = UsageSummary {
        item_count: i64::try_from(facts.len()).unwrap_or(i64::MAX),
        ..UsageSummary::default()
    };
    let mut max_sum = Decimal::ZERO;
    for fact in facts {
        out.executed_qty += fact.executed_qty;
        out.executed_amount_capped += fact.executed_amount_capped;
        match exec_state_of(fact.max_qty, fact.executed_qty) {
            STATE_UNLIMITED => out.unlimited_items += 1,
            STATE_NOT_STARTED => out.not_started_items += 1,
            STATE_PARTIAL => out.partial_items += 1,
            STATE_DONE => out.done_items += 1,
            STATE_OVER => out.over_items += 1,
            _ => {}
        }
        if let Some(max) = fact.max_qty {
            max_sum += max;
        }
    }
    out.cap_qty = if out.unlimited_items == 0 && out.item_count > 0 {
        Some(max_sum)
    } else {
        None
    };
    out.remaining_qty = remaining_of(out.cap_qty, out.executed_qty);
    out.exec_qty_rate = rate_of(out.executed_qty, out.cap_qty);
    out.exec_state = exec_state_of(out.cap_qty, out.executed_qty);
    out
}

// ==================== 入参 ====================

/// 执行量总览分页查询参数（`GET /business/erp/framework-agreement/execution/page`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ExecutionPageQuery {
    /// 页码（1 起）。
    pub page: Option<u64>,
    /// 每页条数（1–200）。
    pub page_size: Option<u64>,
    /// 关键字（协议号 / 协议名称 / 供应商名 / 经办人）。
    pub keyword: Option<String>,
    /// 供应商。
    pub supplier_id: Option<String>,
    /// 协议状态（DRAFT / SUBMITTED / APPROVED）。
    pub status: Option<String>,
    /// 有效期状态（DRAFT / SUBMITTED / NOT_STARTED / ACTIVE / EXPIRED）。
    pub validity_state: Option<String>,
    /// 只看「有行超上限」的协议。
    pub over_only: Option<bool>,
}

/// 已校验的分页筛选条件（model 只认这个）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionFilter {
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 关键字（已 trim，空 → None）。
    pub keyword: Option<String>,
    /// 供应商。
    pub supplier_id: Option<Uuid>,
    /// 协议状态（已校验在闭集内）。
    pub status: Option<String>,
    /// 有效期状态（已校验在闭集内）。
    pub validity_state: Option<String>,
    /// 只看有行超上限的。
    pub over_only: bool,
}

fn parse_optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => Uuid::parse_str(text)
            .map(Some)
            .map_err(|_| param_error(format!("{field}={text} 不是合法 uuid"))),
    }
}

impl ExecutionPageQuery {
    /// 校验并归一化（非法值 → 400，不静默忽略）。
    pub fn filter(&self) -> std::result::Result<ExecutionFilter, ApiError> {
        let page = self.page.unwrap_or(1).max(1);
        let page_size = self.page_size.unwrap_or(20).clamp(1, 200);
        let status = match self.status.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(status) => {
                let status = status.to_ascii_uppercase();
                if !fwa::STATUSES.contains(&status.as_str()) {
                    return Err(param_error(format!(
                        "status={status} 不在状态闭集内（{}）",
                        fwa::STATUSES.join(" / ")
                    )));
                }
                Some(status)
            }
        };
        let validity_state = match self.validity_state.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(state) => {
                let state = state.to_ascii_uppercase();
                if !VALIDITY_STATES.contains(&state.as_str()) {
                    return Err(param_error(format!(
                        "validity_state={state} 不在闭集内（{}）",
                        VALIDITY_STATES.join(" / ")
                    )));
                }
                Some(state)
            }
        };
        let keyword = match self.keyword.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(text) => {
                if text.chars().count() > MAX_KEYWORD_LEN {
                    return Err(param_error(format!(
                        "keyword 最长 {MAX_KEYWORD_LEN} 个字符（收到 {}）",
                        text.chars().count()
                    )));
                }
                Some(text.to_string())
            }
        };
        Ok(ExecutionFilter {
            page,
            page_size,
            keyword,
            supplier_id: parse_optional_uuid(self.supplier_id.as_deref(), "supplier_id")?,
            status,
            validity_state,
            over_only: self.over_only.unwrap_or(false),
        })
    }
}

// ==================== 出参 ====================

/// 协议执行量（总览的一行 / 详情里的单头）。
#[derive(Debug, Clone, Serialize)]
pub struct AgreementExecRowRes {
    /// 协议 id。
    pub agreement_id: String,
    /// 协议号。
    pub agreement_no: String,
    /// 协议名称。
    pub agreement_name: String,
    /// 供应商 id。
    pub supplier_id: String,
    /// 供应商名快照（签约主体历史事实）。
    pub supplier_name: Option<String>,
    /// 供应商现名（实时 join；快照为空时回退）。
    pub supplier_display_name: String,
    /// 经办部门 id。
    pub dept_id: Option<String>,
    /// 经办部门名。
    pub dept_name: Option<String>,
    /// 经办人 id。
    pub owner_id: Option<String>,
    /// 经办人姓名（快照）。
    pub owner_name: Option<String>,
    /// 有效期起（含）。
    pub start_date: String,
    /// 有效期止（含）。
    pub end_date: String,
    /// 签订日期。
    pub sign_date: Option<String>,
    /// 协议状态。
    pub status: String,
    /// 协议状态中文。
    pub status_label: String,
    /// 有效期状态（DRAFT / SUBMITTED / NOT_STARTED / ACTIVE / EXPIRED）。
    pub validity_state: String,
    /// 有效期状态中文。
    pub validity_state_label: String,
    /// 今天是否生效（与下单判据同一口径）。
    pub is_effective: bool,
    /// 距截止日还有几天（已过期 = 负数）。
    pub days_left: i64,
    /// 定价方式（FIXED / FLOATING）。
    pub price_mode: String,
    /// 定价方式中文。
    pub price_mode_label: String,
    /// 协议价是否含税。
    pub price_includes_tax: bool,
    /// 协议价币种。
    pub price_currency: String,
    /// 允许高于协议价的比例上限。
    pub price_tolerance: Decimal,
    /// 单头数量上限（null = 不限）。
    pub total_max_qty: Option<Decimal>,
    /// 单头金额上限（null = 不限）。
    pub total_max_amount: Option<Decimal>,
    /// 有效数量封顶（单头 → 明细合计的兜底；null = 不限）。
    pub cap_qty: Option<Decimal>,
    /// 有效金额封顶（= 单头金额上限）。
    pub cap_amount: Option<Decimal>,
    /// 明细上限合计（仅在明细全部设了上限时有值）。
    pub item_max_qty_sum: Option<Decimal>,
    /// 物料行数。
    pub item_count: i64,
    /// 不限量的行数。
    pub unlimited_items: i64,
    /// 超上限的行数。
    pub over_items: i64,
    /// 已用满的行数。
    pub done_items: i64,
    /// 部分执行的行数。
    pub partial_items: i64,
    /// 未开工的行数。
    pub not_started_items: i64,
    /// 已用量合计（单据现算）。
    pub executed_qty: Decimal,
    /// 已用金额合计（不含税）。
    pub executed_amount: Decimal,
    /// 已用金额合计（含税）。
    pub executed_amount_with_tax: Decimal,
    /// 已用金额（协议价口径：含税协议取含税、否则取不含税）。
    pub executed_amount_capped: Decimal,
    /// 回写快照：已用量合计（M4-08 的 `total_executed_qty`）。
    pub booked_total_qty: Decimal,
    /// 回写快照：已用金额合计（M4-08 的 `total_executed_amount`）。
    pub booked_total_amount: Decimal,
    /// 剩余可下单量（**可为负**；null = 不限）。
    pub remaining_qty: Option<Decimal>,
    /// 剩余可用金额（**可为负**；null = 不限）。
    pub remaining_amount: Option<Decimal>,
    /// 数量执行率（0–1，6 位；null = 不限）。
    pub exec_qty_rate: Option<Decimal>,
    /// 数量执行率百分比（0–100，2 位）。
    pub exec_qty_rate_pct: Option<Decimal>,
    /// 金额执行率（0–1，6 位；null = 无金额上限）。
    pub exec_amount_rate: Option<Decimal>,
    /// 金额执行率百分比（0–100，2 位）。
    pub exec_amount_rate_pct: Option<Decimal>,
    /// 单头状态（UNLIMITED / NOT_STARTED / PARTIAL / DONE / OVER）。
    pub exec_state: String,
    /// 单头状态中文。
    pub exec_state_label: String,
    /// 计入额度的采购单单数。
    pub order_doc_count: i64,
    /// 已释放（作废 / 软删）的采购单单数。
    pub released_doc_count: i64,
    /// 最近一次下单日。
    pub last_order_on: Option<String>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
}

/// 物料行执行量。
#[derive(Debug, Clone, Serialize)]
pub struct AgreementExecItemRes {
    /// 协议行 id。
    pub agreement_item_id: String,
    /// 行号。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 单位 id。
    pub unit_id: String,
    /// 单位名。
    pub unit_name: String,
    /// 协议单价。
    pub agreed_price: Decimal,
    /// 进项税率（0–1 分数）。
    pub tax_rate: Decimal,
    /// 该物料数量上限（null = 不限）。
    pub max_qty: Option<Decimal>,
    /// 已用量（单据现算）。
    pub executed_qty: Decimal,
    /// 回写快照（M4-08 的 `executed_qty`）。
    pub booked_qty: Decimal,
    /// 回写与单据链的差（`booked_qty − executed_qty`；非 0 = 分叉）。
    pub booked_drift: Decimal,
    /// 已用金额（不含税）。
    pub executed_amount: Decimal,
    /// 已用金额（含税）。
    pub executed_amount_with_tax: Decimal,
    /// 已用金额（协议价口径）。
    pub executed_amount_capped: Decimal,
    /// 行金额上限（数量上限 × 协议价；null = 不限）。
    pub cap_amount: Option<Decimal>,
    /// 剩余数量（null = 不限；可为负）。
    pub remaining_qty: Option<Decimal>,
    /// 剩余金额（null = 不限；可为负）。
    pub remaining_amount: Option<Decimal>,
    /// 数量执行率（0–1，6 位；null = 不限）。
    pub exec_qty_rate: Option<Decimal>,
    /// 数量执行率百分比（0–100）。
    pub exec_qty_rate_pct: Option<Decimal>,
    /// 金额执行率（0–1，6 位）。
    pub exec_amount_rate: Option<Decimal>,
    /// 行状态。
    pub exec_state: String,
    /// 行状态中文。
    pub exec_state_label: String,
    /// 计入的采购单单数。
    pub order_doc_count: i64,
    /// 计入的采购明细行数。
    pub order_line_count: i64,
    /// 最近一次下单日。
    pub last_order_on: Option<String>,
}

/// 下钻：这一行的量被哪张采购单的哪一行用掉（含作废 / 软删，`counted=false` 标明原因）。
#[derive(Debug, Clone, Serialize)]
pub struct ExecSourceRes {
    /// 协议行 id。
    pub agreement_item_id: String,
    /// 采购单 id。
    pub order_id: String,
    /// 采购单号。
    pub order_no: String,
    /// 采购单状态。
    pub order_status: String,
    /// 采购单状态中文。
    pub order_status_label: String,
    /// 下单日。
    pub order_date: String,
    /// 采购单软删时间（有值 = 已删）。
    pub order_deleted_at: Option<String>,
    /// 采购明细行号。
    pub line_no: i32,
    /// 这一行用的数量。
    pub qty: Decimal,
    /// 这一行的不含税金额。
    pub amount: Decimal,
    /// 这一行的含税金额。
    pub with_tax_amount: Decimal,
    /// 这一行的下单价（不含税）。
    pub price: Decimal,
    /// 是否计入已用量。
    pub counted: bool,
    /// 判定原因（「已用」/「采购单已作废」/「采购单已删除」）。
    pub verdict: String,
}

/// 单协议执行量详情：单头汇总 + 逐行执行量 + 下钻来源。
#[derive(Debug, Clone, Serialize)]
pub struct AgreementExecutionRes {
    /// 单头汇总。
    pub agreement: AgreementExecRowRes,
    /// 逐行执行量（按行号升序）。
    pub items: Vec<AgreementExecItemRes>,
    /// 下钻来源（按行号 / 下单日 / 采购单号升序）。
    pub sources: Vec<ExecSourceRes>,
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    /// 剩余：封顶有值才算，None（不限）不许当成 0；负数必须保留（超上限要显形）。
    #[test]
    fn remaining_keeps_null_and_negative() {
        assert_eq!(remaining_of(Some(dec("100")), dec("30")), Some(dec("70")));
        assert_eq!(remaining_of(Some(dec("100")), dec("120")), Some(dec("-20")));
        assert_eq!(remaining_of(None, dec("30")), None);
    }

    /// 执行率：分母为 None / 0 时是 None（不是 0 —— 0 会被读成「一点没做」）。
    #[test]
    fn rate_needs_a_positive_denominator() {
        assert_eq!(rate_of(dec("30"), Some(dec("100"))), Some(dec("0.3")));
        assert_eq!(rate_of(dec("30"), None), None);
        assert_eq!(rate_of(dec("30"), Some(Decimal::ZERO)), None);
        // 6 位小数（与视图 round(…,6) 同精度）
        assert_eq!(rate_of(dec("1"), Some(dec("3"))), Some(dec("0.333333")));
    }

    /// 行状态五态：边界（== 上限是 DONE，不是 PARTIAL）+ 不限量优先判。
    #[test]
    fn exec_state_matrix() {
        assert_eq!(exec_state_of(None, dec("999")), STATE_UNLIMITED);
        assert_eq!(exec_state_of(Some(dec("100")), Decimal::ZERO), STATE_NOT_STARTED);
        assert_eq!(exec_state_of(Some(dec("100")), dec("1")), STATE_PARTIAL);
        assert_eq!(exec_state_of(Some(dec("100")), dec("100")), STATE_DONE);
        assert_eq!(exec_state_of(Some(dec("100")), dec("100.0001")), STATE_OVER);
    }

    /// 有效期状态：先看状态再看日期（草稿哪怕日期区间对也不许判 ACTIVE）。
    #[test]
    fn validity_state_prefers_status_over_dates() {
        let start = date("2026-01-01");
        let end = date("2026-12-31");
        let today = date("2026-06-01");
        assert_eq!(validity_state_of("DRAFT", start, end, today), VALIDITY_DRAFT);
        assert_eq!(validity_state_of("SUBMITTED", start, end, today), VALIDITY_SUBMITTED);
        assert_eq!(validity_state_of("APPROVED", start, end, today), VALIDITY_ACTIVE);
        // 起止日都是**含**边界
        assert_eq!(validity_state_of("APPROVED", start, end, start), VALIDITY_ACTIVE);
        assert_eq!(validity_state_of("APPROVED", start, end, end), VALIDITY_ACTIVE);
        assert_eq!(
            validity_state_of("APPROVED", start, end, date("2025-12-31")),
            VALIDITY_NOT_STARTED
        );
        assert_eq!(
            validity_state_of("APPROVED", start, end, date("2027-01-01")),
            VALIDITY_EXPIRED
        );
    }

    /// 下钻判定：作废 / 软删都不算，其余都算（草稿也算 —— 量已占住）。
    #[test]
    fn source_verdict_covers_all_three_failures() {
        assert_eq!(source_verdict("CANCELLED", false), (false, "采购单已作废"));
        assert_eq!(source_verdict("APPROVED", true), (false, "采购单已删除"));
        assert_eq!(source_verdict("DRAFT", false), (true, "已用"));
        assert_eq!(source_verdict("CLOSED", false), (true, "已用"));
    }

    /// 汇总：单头封顶 = 明细合计（全有上限），有一行不限就整体不封顶。
    #[test]
    fn summary_falls_back_to_item_caps_only_when_all_limited() {
        let all_limited = summary_of(&[
            UsageFacts {
                max_qty: Some(dec("100")),
                executed_qty: dec("30"),
                executed_amount_capped: dec("300"),
            },
            UsageFacts {
                max_qty: Some(dec("50")),
                executed_qty: dec("50"),
                executed_amount_capped: dec("250"),
            },
        ]);
        assert_eq!(all_limited.item_count, 2);
        assert_eq!(all_limited.cap_qty, Some(dec("150")));
        assert_eq!(all_limited.executed_qty, dec("80"));
        assert_eq!(all_limited.remaining_qty, Some(dec("70")));
        assert_eq!(all_limited.done_items, 1);
        assert_eq!(all_limited.partial_items, 1);
        assert_eq!(all_limited.exec_state, STATE_PARTIAL);

        let with_unlimited = summary_of(&[
            UsageFacts {
                max_qty: Some(dec("100")),
                executed_qty: dec("30"),
                executed_amount_capped: dec("300"),
            },
            UsageFacts {
                max_qty: None,
                executed_qty: dec("10"),
                executed_amount_capped: dec("50"),
            },
        ]);
        assert_eq!(with_unlimited.unlimited_items, 1);
        assert_eq!(with_unlimited.cap_qty, None);
        assert_eq!(with_unlimited.remaining_qty, None);
        assert_eq!(with_unlimited.exec_qty_rate, None);
        assert_eq!(with_unlimited.exec_state, STATE_UNLIMITED);
    }

    /// 百分比：两位小数，None 传染（不限量没有百分比可显示）。
    #[test]
    fn pct_is_two_decimals() {
        assert_eq!(pct_of(Some(dec("0.333333"))), Some(dec("33.33")));
        assert_eq!(pct_of(Some(dec("1.5"))), Some(dec("150.00")));
        assert_eq!(pct_of(None), None);
    }

    /// 筛选：非法状态 / 非法有效期状态 / 超长关键字一律 400，不静默忽略。
    #[test]
    fn filter_rejects_bad_input() {
        let ok = ExecutionPageQuery {
            status: Some(" approved ".into()),
            validity_state: Some("active".into()),
            ..ExecutionPageQuery::default()
        }
        .filter()
        .expect("合法筛选");
        assert_eq!(ok.status.as_deref(), Some("APPROVED"));
        assert_eq!(ok.validity_state.as_deref(), Some("ACTIVE"));
        assert_eq!(ok.page_size, 20);

        assert!(
            ExecutionPageQuery {
                status: Some("EXPIRED".into()),
                ..Default::default()
            }
            .filter()
            .is_err()
        );
        assert!(
            ExecutionPageQuery {
                validity_state: Some("BOGUS".into()),
                ..Default::default()
            }
            .filter()
            .is_err()
        );
        assert!(
            ExecutionPageQuery {
                supplier_id: Some("not-a-uuid".into()),
                ..Default::default()
            }
            .filter()
            .is_err()
        );
        assert!(
            ExecutionPageQuery {
                keyword: Some("x".repeat(MAX_KEYWORD_LEN + 1)),
                ..Default::default()
            }
            .filter()
            .is_err()
        );
    }

    /// 视图名与 V0452 逐字一致（改名 = 迁移与代码同时改，这里钉住）。
    #[test]
    fn view_names_match_migration() {
        assert_eq!(HEAD_VIEW, "v_framework_agreement_execution");
        assert_eq!(ITEM_VIEW, "v_framework_agreement_item_execution");
        assert_eq!(EXEC_STATES.len(), 5);
        assert_eq!(VALIDITY_STATES.len(), 5);
        assert_eq!(exec_state_label(STATE_OVER), "已超限");
        assert_eq!(validity_state_label(VALIDITY_EXPIRED), "已过期");
    }
}
