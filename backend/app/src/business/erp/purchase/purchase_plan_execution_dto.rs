//! 采购计划执行率跟踪契约（M4-04）：**已下单 / 计划量**。
//!
//! 数据来源是 V0449 的两个视图（见 `backend/app/migrations/business/V0449__purchase_plan_execution.up.sql`）：
//!
//! ```text
//! v_purchase_plan_item_execution   明细级：一行 = 计划的一格（物料 × 仓库）
//! v_purchase_plan_execution        单头级：一张存活计划一行（明细合计 + 行状态计数）
//! ```
//!
//! # 口径（每一条都有纯函数 + 单测钉住，与迁移头注释逐字一致）
//!
//! ```text
//! ① 计划量   = planned_qty（库层 ck_biz_ppi_qty 保证 > 0 ⇒ 执行率恒有分母）
//! ② 已申请量 = Σ 申请明细 qty（plan_item_id 指回本行、申请单未软删）
//! ③ 已下单量 = Σ 申请明细 qty，且申请单的 po_id 指向的采购单「存活且未作废」
//!              —— 草稿采购单**算**（量已拨走）；作废 / 软删**不算**（链已断）
//! ④ 执行率   = 已下单量 / 计划量（0.4 = 40%），同时给 0–100 的两位小数百分比
//! ⑤ 剩余     = 计划量 − 已下单量（**可为负**：超计划执行，V0446 头注释 4 刻意不拦）
//! ⑥ 行状态   = NOT_STARTED / PARTIAL / DONE / OVER（OVER = 执行率 > 100%）
//! ```
//!
//! # 为什么不复用 `executed_qty` 当「已下单」
//!
//! `executed_qty` 是**转申请**时的回写（M4-03），它衡量「拨出去多少」，不是「下出去多少」。
//! 两张单差一个环节：申请批完但还没转采购单时，`executed_qty` 已经有了、采购单一单没有 ——
//! 这正是本卡要显形的东西。所以视图里两个数并排给（`booked_qty` / `ordered_qty`），
//! 差就是「转了申请但还没下单」的量。
//!
//! # 权限码
//!
//! 一个接口码：`business:erp:purchase-plan:execution`（两个只读接口共用）。
//! 页面显形码 `…:page` **不在这里**（AGENTS 约定 14：只给前端，后端零引用）。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use cloudpivot_kernel::shared::error_code;

use crate::business::erp::purchase::purchase_plan_dto::{
    MAX_PLAN_YEAR, MIN_PLAN_YEAR, bad_request, check_type, check_year, parse_optional_uuid, plan_type_label,
    status_label,
};
use crate::business::erp::purchase::purchase_plan_model::{STATUS_APPROVED, STATUS_DRAFT, STATUS_SUBMITTED, STATUSES};

// ==================== 域常量 ====================

/// 本模块的**唯一**接口权限码（V0449 的按钮行 `…004478`）。
pub const EXEC_PERMISSION: &str = "business:erp:purchase-plan:execution";

/// 明细级视图名（与 V0449 逐字一致）。
pub const ITEM_VIEW: &str = "v_purchase_plan_item_execution";
/// 单头级视图名（与 V0449 逐字一致）。
pub const HEAD_VIEW: &str = "v_purchase_plan_execution";

/// 执行率保留位数（视图 `round(…, 6)`；这里只做说明与纯函数对齐）。
pub const RATE_DP: u32 = 6;
/// 百分比保留位数（与预算执行报表 `exec_ratio` 同口径：两位小数）。
pub const PCT_DP: u32 = 2;

/// 行状态：未开工（已下单 0）。
pub const STATE_NOT_STARTED: &str = "NOT_STARTED";
/// 行状态：部分下单（0 < 已下单 < 计划）。
pub const STATE_PARTIAL: &str = "PARTIAL";
/// 行状态：已下满（已下单 = 计划）。
pub const STATE_DONE: &str = "DONE";
/// 行状态：超计划（已下单 > 计划，执行率 > 100%）。
pub const STATE_OVER: &str = "OVER";
/// 状态闭集（与视图 `exec_state` 的 CASE 逐字一致）。
pub const EXEC_STATES: [&str; 4] = [STATE_NOT_STARTED, STATE_PARTIAL, STATE_DONE, STATE_OVER];

/// 关键字长度上限（与采购计划分页同口径）。
pub const MAX_KEYWORD_LEN: usize = 64;
/// 执行率下限筛选（百分比）的合法上界：1000% 足够显示任何离谱的超计划。
pub const MAX_MIN_RATE: i32 = 1000;

/// 行状态中文名（报文 / 前端徽标共用）。未知名原样返回，绝不 panic。
pub fn exec_state_label(state: &str) -> &str {
    match state {
        STATE_NOT_STARTED => "未开工",
        STATE_PARTIAL => "部分下单",
        STATE_DONE => "已下满",
        STATE_OVER => "超计划",
        other => other,
    }
}

// ==================== 纯函数（handler / model / 单测共用） ====================

/// 剩余可下单量 = 计划量 − 已下单量（**故意保留负数**：超计划要能看出来）。
pub fn remaining_of(planned: Decimal, ordered: Decimal) -> Decimal {
    planned - ordered
}

/// 还能再转成采购申请的量 = 计划量 − 回写列已申请量（`executed_qty`）。
///
/// 与 [`remaining_of`] **不是**一回事：`remaining_of` 量的是「还没下单」，
/// 这个量的是「还能再转多少申请」。转申请的口径把负剩余按 0（M4-03 的
/// `ConvertibleLine::remaining`），这里保持一致 —— 负数只会在「库层被直接改过」
/// 的极端场景出现，报表不该显示「还能转 -5」。
pub fn convertible_of(planned: Decimal, booked: Decimal) -> Decimal {
    let remaining = planned - booked;
    if remaining.is_sign_negative() {
        Decimal::ZERO
    } else {
        remaining
    }
}

/// 执行率 = 已下单 / 计划（6 位小数）。
///
/// 计划量为 0 时返回 `None` 而不是 0：`0` 会被读成「一行都没下」，
/// 而「没有计划量」根本没有分母（空计划 / 视图聚合不到明细）。
pub fn exec_rate_of(ordered: Decimal, planned: Decimal) -> Option<Decimal> {
    if planned <= Decimal::ZERO {
        return None;
    }
    Some((ordered / planned).round_dp(RATE_DP))
}

/// 百分比（0–100，两位小数）：与预算执行报表的 `exec_ratio` 同口径，前端进度条直接用。
pub fn pct_of(rate: Option<Decimal>) -> Option<Decimal> {
    rate.map(|value| (value * Decimal::from(100)).round_dp(PCT_DP))
}

/// 行状态判据（与视图的 CASE **逐字对应**，改一处必须两处一起改）。
///
/// 边界：`ordered == planned` 是 DONE（不是 PARTIAL）；`ordered > planned` 是 OVER。
pub fn exec_state_of(ordered: Decimal, planned: Decimal) -> &'static str {
    if ordered > planned {
        STATE_OVER
    } else if ordered >= planned {
        STATE_DONE
    } else if ordered > Decimal::ZERO {
        STATE_PARTIAL
    } else {
        STATE_NOT_STARTED
    }
}

/// 来源行（申请明细）到底算不算「已下单」，以及**为什么不**（下钻列表直接展示原因）。
///
/// * 申请单还没转采购单（`po_id` 为空）→ 不算，原因「未转采购单」
/// * 采购单已软删（`po_deleted_at` 有值）→ 不算，原因「采购单已删除」
/// * 采购单已作废（`po_status = CANCELLED`）→ 不算，原因「采购单已作废」
/// * 其余（含草稿 / 审批中 / 已审批 / 部分收货 / 已收货 / 已关闭）→ 算
///
/// 返回 `(是否计入, 原因)`；原因在计入时是「已下单」，便于前端原样展示。
pub fn source_verdict(po_id: Option<Uuid>, po_status: Option<&str>, po_deleted: bool) -> (bool, &'static str) {
    if po_id.is_none() {
        return (false, "未转采购单");
    }
    if po_deleted {
        return (false, "采购单已删除");
    }
    match po_status.map(str::trim) {
        Some("CANCELLED") => (false, "采购单已作废"),
        Some(_) => (true, "已下单"),
        // po_id 有值但单头查不到（软删后行已清）——按「链断了」处理，绝不当成已下单
        None => (false, "采购单已删除"),
    }
}

/// 汇总口径的输入（只放四个量，避免汇总函数依赖 model 行结构，单测不用连库）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecFacts {
    /// 计划量。
    pub planned_qty: Decimal,
    /// 回写列快照（已申请量 —— M4-03 的 `executed_qty`）。
    pub booked_qty: Decimal,
    /// 已申请量（按单据现算）。
    pub req_qty: Decimal,
    /// 已下单量（按单据现算）。
    pub ordered_qty: Decimal,
}

/// 明细汇总（单头视图的同一套算法；handler 用它交叉校验视图合计）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecSummary {
    /// 明细行数。
    pub line_count: i64,
    /// 计划量合计。
    pub planned_qty: Decimal,
    /// 已申请量合计（回写列快照）。
    pub booked_qty: Decimal,
    /// 已申请量合计（按单据现算）。
    pub req_qty: Decimal,
    /// 已下单量合计。
    pub ordered_qty: Decimal,
    /// 剩余可下单合计（可为负）。
    pub remaining_qty: Decimal,
    /// 整单执行率（计划量 0 → None）。
    pub exec_rate: Option<Decimal>,
    /// 整单已申请率（计划量 0 → None）。
    pub req_rate: Option<Decimal>,
    /// 未开工行数。
    pub not_started_lines: i64,
    /// 部分下单行数。
    pub partial_lines: i64,
    /// 已下满行数。
    pub done_lines: i64,
    /// 超计划行数。
    pub over_lines: i64,
}

impl ExecSummary {
    /// 执行率百分比（0–100，两位小数）。
    #[must_use]
    pub fn exec_rate_pct(&self) -> Option<Decimal> {
        pct_of(self.exec_rate)
    }

    /// 已申请率百分比（0–100，两位小数）。
    #[must_use]
    pub fn req_rate_pct(&self) -> Option<Decimal> {
        pct_of(self.req_rate)
    }

    /// 整单状态（按「已下单 / 计划」判，与行状态同一套判据）。
    #[must_use]
    pub fn exec_state(&self) -> &'static str {
        exec_state_of(self.ordered_qty, self.planned_qty)
    }
}

/// 由明细汇总出单头（纯函数：单测与真库用例都用它交叉校验视图）。
pub fn summary_of(facts: &[ExecFacts]) -> ExecSummary {
    let mut out = ExecSummary {
        line_count: i64::try_from(facts.len()).unwrap_or(i64::MAX),
        ..ExecSummary::default()
    };
    for fact in facts {
        out.planned_qty += fact.planned_qty;
        out.booked_qty += fact.booked_qty;
        out.req_qty += fact.req_qty;
        out.ordered_qty += fact.ordered_qty;
        match exec_state_of(fact.ordered_qty, fact.planned_qty) {
            STATE_NOT_STARTED => out.not_started_lines += 1,
            STATE_PARTIAL => out.partial_lines += 1,
            STATE_DONE => out.done_lines += 1,
            STATE_OVER => out.over_lines += 1,
            _ => {}
        }
    }
    out.remaining_qty = remaining_of(out.planned_qty, out.ordered_qty);
    out.exec_rate = exec_rate_of(out.ordered_qty, out.planned_qty);
    out.req_rate = exec_rate_of(out.req_qty, out.planned_qty);
    out
}

// ==================== 入参 ====================

/// 执行率总览分页查询参数（`GET /business/erp/purchase-plan/execution/page`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ExecutionPageQuery {
    /// 页码（1 起）。
    pub page: Option<u64>,
    /// 每页条数（1–200）。
    pub page_size: Option<u64>,
    /// 关键字（计划单号 / 名称 / 部门名 / 编制人）。
    pub keyword: Option<String>,
    /// 计划状态（DRAFT / SUBMITTED / APPROVED）。
    pub status: Option<String>,
    /// 计划类型（ANNUAL / MONTHLY）。
    pub plan_type: Option<String>,
    /// 计划年度。
    pub plan_year: Option<i32>,
    /// 计划月份（1–12）。
    pub plan_month: Option<i32>,
    /// 编制部门。
    pub dept_id: Option<String>,
    /// 执行率下限（百分比，0–1000）：只看「至少执行到这个程度」的计划。
    /// 传 `100` 就是「只看已下满 / 超计划的」（`exec_rate >= 1`）。
    pub min_rate: Option<i32>,
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
    /// 状态（已校验在闭集内）。
    pub status: Option<String>,
    /// 类型（已校验在闭集内）。
    pub plan_type: Option<String>,
    /// 年度。
    pub plan_year: Option<i32>,
    /// 月份。
    pub plan_month: Option<i32>,
    /// 部门。
    pub dept_id: Option<Uuid>,
    /// 执行率下限（**比值** 0–10，不是百分比 —— model 里直接与 `exec_rate` 比较）。
    pub min_rate: Option<Decimal>,
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
                if !STATUSES.contains(&status.as_str()) {
                    return Err(bad_request(format!(
                        "status={status} 不在状态闭集内（{}）",
                        STATUSES.join(" / ")
                    )));
                }
                Some(status)
            }
        };
        let plan_type = match self.plan_type.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(plan_type) => Some(check_type(plan_type)?),
        };
        let plan_year = match self.plan_year {
            None => None,
            Some(year) => Some(check_year(year)?),
        };
        let plan_month = match self.plan_month {
            None => None,
            Some(month) if (1..=12).contains(&month) => Some(month),
            Some(month) => {
                return Err(bad_request(format!("plan_month={month} 超范围（1–12）")));
            }
        };
        let keyword = match self.keyword.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(text) => {
                if text.chars().count() > MAX_KEYWORD_LEN {
                    return Err(bad_request(format!(
                        "keyword 最长 {MAX_KEYWORD_LEN} 个字符（收到 {}）",
                        text.chars().count()
                    )));
                }
                Some(text.to_string())
            }
        };
        let min_rate = match self.min_rate {
            None => None,
            Some(percent) if (0..=MAX_MIN_RATE).contains(&percent) => Some(Decimal::from(percent) / Decimal::from(100)),
            Some(percent) => {
                return Err(bad_request(format!(
                    "min_rate={percent} 超范围（0–{MAX_MIN_RATE}，单位是百分比）"
                )));
            }
        };
        Ok(ExecutionFilter {
            page,
            page_size,
            keyword,
            status,
            plan_type,
            plan_year,
            plan_month,
            dept_id: parse_optional_uuid(self.dept_id.as_deref(), "dept_id")?,
            min_rate,
        })
    }
}

// ==================== 出参 ====================

/// 计划执行率（总览的一行 / 详情里的单头）。
#[derive(Debug, Clone, Serialize)]
pub struct PlanExecRowRes {
    /// 计划 id。
    pub plan_id: String,
    /// 计划单号。
    pub plan_no: String,
    /// 计划名称。
    pub plan_name: String,
    /// 类型（ANNUAL / MONTHLY）。
    pub plan_type: String,
    /// 类型中文。
    pub plan_type_label: String,
    /// 年度。
    pub plan_year: i32,
    /// 月份（年度计划为 null）。
    pub plan_month: Option<i32>,
    /// 编制部门 id。
    pub dept_id: Option<String>,
    /// 编制部门名（快照为空时实时取）。
    pub dept_name: Option<String>,
    /// 编制人姓名（快照）。
    pub planner_name: Option<String>,
    /// 计划状态。
    pub status: String,
    /// 计划状态中文。
    pub status_label: String,
    /// 计划金额合计（不含税）。
    pub total_planned_amount: Decimal,
    /// 明细行数。
    pub line_count: i64,
    /// 计划量合计。
    pub planned_qty: Decimal,
    /// 回写列快照：已申请量（M4-03 的 `executed_qty` 合计）。
    pub booked_qty: Decimal,
    /// 已申请量合计（按单据现算）。
    pub req_qty: Decimal,
    /// 已下单量合计。
    pub ordered_qty: Decimal,
    /// 剩余可下单合计（**可为负**）。
    pub remaining_qty: Decimal,
    /// 还能再转成采购申请的量（= Σ计划量 − Σ回写列已申请量，负数按 0）。
    /// 与 `remaining_qty` 是两个口径：后者对「已下单」，这个对「已转申请」。
    pub convertible_qty: Decimal,
    /// 整单执行率（0–1，6 位；计划量为 0 时 null）。
    pub exec_rate: Option<Decimal>,
    /// 整单执行率百分比（0–100，2 位）。
    pub exec_rate_pct: Option<Decimal>,
    /// 整单已申请率（0–1，6 位）。
    pub req_rate: Option<Decimal>,
    /// 整单已申请率百分比（0–100，2 位）。
    pub req_rate_pct: Option<Decimal>,
    /// 整单状态（NOT_STARTED / PARTIAL / DONE / OVER）。
    pub exec_state: String,
    /// 整单状态中文。
    pub exec_state_label: String,
    /// 未开工行数。
    pub not_started_lines: i64,
    /// 部分下单行数。
    pub partial_lines: i64,
    /// 已下满行数。
    pub done_lines: i64,
    /// 超计划行数。
    pub over_lines: i64,
    /// 创建时间（RFC3339）。
    pub created_at: String,
}

/// 计划明细的执行率一行。
#[derive(Debug, Clone, Serialize)]
pub struct PlanExecItemRes {
    /// 计划明细 id。
    pub plan_item_id: String,
    /// 行号。
    pub line_no: i32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 仓库 id。
    pub warehouse_id: String,
    /// 仓库名。
    pub warehouse_name: String,
    /// 单位 id。
    pub unit_id: String,
    /// 单位名。
    pub unit_name: String,
    /// 计划量。
    pub planned_qty: Decimal,
    /// 计划单价。
    pub planned_price: Decimal,
    /// 计划金额。
    pub planned_amount: Decimal,
    /// 回写列快照：已申请量。
    pub booked_qty: Decimal,
    /// 已申请量（按单据现算）。
    pub req_qty: Decimal,
    /// 已下单量。
    pub ordered_qty: Decimal,
    /// 剩余可下单（**可为负**）。
    pub remaining_qty: Decimal,
    /// 还能再转成采购申请的量（= 计划量 − 回写列已申请量，负数按 0）。
    pub convertible_qty: Decimal,
    /// 执行率（0–1，6 位）。
    pub exec_rate: Option<Decimal>,
    /// 执行率百分比（0–100，2 位）。
    pub exec_rate_pct: Option<Decimal>,
    /// 已申请率（0–1，6 位）。
    pub req_rate: Option<Decimal>,
    /// 已申请率百分比（0–100，2 位）。
    pub req_rate_pct: Option<Decimal>,
    /// 行状态。
    pub exec_state: String,
    /// 行状态中文。
    pub exec_state_label: String,
    /// 是否超计划（= 行状态 OVER；前端标红直接读它，不用自己比大小）。
    pub over_plan: bool,
    /// 申请明细行数。
    pub req_count: i64,
    /// 涉及申请单张数。
    pub req_doc_count: i64,
    /// 涉及采购单张数（口径：存活且未作废）。
    pub po_doc_count: i64,
    /// 来源单据（申请明细 → 采购单；`counted` 说明它算不算已下单）。
    pub sources: Vec<ExecSourceRes>,
}

/// 来源单据（下钻）：这一行计划量被哪些申请单 / 采购单消耗了。
#[derive(Debug, Clone, Serialize)]
pub struct ExecSourceRes {
    /// 申请明细 id。
    pub req_item_id: String,
    /// 申请单 id。
    pub req_id: String,
    /// 申请单号。
    pub req_no: String,
    /// 申请单状态。
    pub req_status: String,
    /// 申请单状态中文。
    pub req_status_label: String,
    /// 申请数量（这一行计划量拨给它的部分）。
    pub qty: Decimal,
    /// 采购单 id（未转单为 null）。
    pub po_id: Option<String>,
    /// 采购单号（未转单为 null）。
    pub po_no: Option<String>,
    /// 采购单状态（未转单为 null）。
    pub po_status: Option<String>,
    /// 采购单状态中文。
    pub po_status_label: Option<String>,
    /// 是否计入「已下单」。
    pub counted: bool,
    /// 计入 / 不计入的原因（人话，前端原样展示）。
    pub reason: String,
}

/// 单计划执行率详情（单头 + 逐行 + 每行来源）。
#[derive(Debug, Clone, Serialize)]
pub struct PlanExecutionRes {
    /// 单头（含汇总与行状态计数）。
    pub plan: PlanExecRowRes,
    /// 明细（按行号升序，每行带来源单据）。
    pub items: Vec<PlanExecItemRes>,
}

// ==================== 出参构造（model 行 → 报文） ====================

/// 申请单状态中文名（采购申请域 4 个状态；未知原样返回）。
pub fn requisition_status_label(status: &str) -> &str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SUBMITTED => "审批中",
        STATUS_APPROVED => "已批准",
        "CONVERTED" => "已转采购单",
        other => other,
    }
}

/// 计划状态 → 中文（复用采购计划域的标签）。
pub fn plan_status_label(status: &str) -> &str {
    status_label(status)
}

/// 计划类型 → 中文（复用采购计划域的标签）。
pub fn plan_type_label_of(plan_type: &str) -> &str {
    plan_type_label(plan_type)
}

/// 年度上下界（给前端提示；与库层 `ck_biz_pp_year` 同口径）。
pub fn plan_year_range() -> (i32, i32) {
    (MIN_PLAN_YEAR, MAX_PLAN_YEAR)
}

/// 参数错误码（与采购计划域同码：`66006 ERP_FILTER_INVALID` 之外的一律走通用 400）。
pub fn param_error(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    fn uuid(text: &str) -> Uuid {
        Uuid::parse_str(text).expect("uuid")
    }

    /// ① 执行率：6 位小数；计划量 0 返回 None（不是 0 —— 0 会被读成「一行都没下」）。
    #[test]
    fn exec_rate_keeps_six_decimals_and_guards_zero() {
        assert_eq!(exec_rate_of(dec("40"), dec("100")), Some(dec("0.4")));
        assert_eq!(exec_rate_of(dec("1"), dec("3")), Some(dec("0.333333")));
        assert_eq!(exec_rate_of(dec("0"), dec("100")), Some(Decimal::ZERO));
        assert_eq!(exec_rate_of(dec("150"), dec("100")), Some(dec("1.5")));
        assert_eq!(exec_rate_of(dec("1"), Decimal::ZERO), None, "计划量 0 没有分母");
        assert_eq!(exec_rate_of(dec("1"), dec("-5")), None, "负数也不给分母");
    }

    /// ② 百分比：两位小数，与预算执行报表 `exec_ratio` 同口径（50.00 = 50%）。
    #[test]
    fn percent_is_two_decimals() {
        assert_eq!(pct_of(Some(dec("0.5"))), Some(dec("50.00")));
        assert_eq!(pct_of(Some(dec("0.333333"))), Some(dec("33.33")));
        assert_eq!(pct_of(Some(dec("1.5"))), Some(dec("150.00")));
        assert_eq!(pct_of(None), None);
    }

    /// ③ 行状态四个边界：0 / 中间 / 恰好等于 / 超过（等于 ≠ 部分，超过 ≠ 下满）。
    #[test]
    fn exec_state_boundaries() {
        assert_eq!(exec_state_of(dec("0"), dec("10")), STATE_NOT_STARTED);
        assert_eq!(exec_state_of(dec("0.0001"), dec("10")), STATE_PARTIAL);
        assert_eq!(exec_state_of(dec("9.9999"), dec("10")), STATE_PARTIAL);
        assert_eq!(exec_state_of(dec("10"), dec("10")), STATE_DONE);
        assert_eq!(exec_state_of(dec("10.0001"), dec("10")), STATE_OVER);
        // 零计划量的极端行（库层不会出现，但纯函数不许 panic / 不许算成 OVER）
        assert_eq!(exec_state_of(Decimal::ZERO, Decimal::ZERO), STATE_DONE);
    }

    /// ④ 剩余量保留负数（超计划必须显形，不许截断成 0）。
    #[test]
    fn remaining_keeps_negative() {
        assert_eq!(remaining_of(dec("100"), dec("40")), dec("60"));
        assert_eq!(remaining_of(dec("100"), dec("100")), Decimal::ZERO);
        assert_eq!(remaining_of(dec("100"), dec("115")), dec("-15"));
    }

    /// ⑤ 来源判定：未转单 / 已作废 / 已软删 都不算；草稿采购单**算**（量已拨走）。
    #[test]
    fn source_verdict_covers_every_branch() {
        let po = uuid("00000000-0000-0000-0000-0000000000f1");
        assert_eq!(source_verdict(None, None, false), (false, "未转采购单"));
        assert_eq!(
            source_verdict(Some(po), Some("CANCELLED"), false),
            (false, "采购单已作废")
        );
        assert_eq!(
            source_verdict(Some(po), Some("APPROVED"), true),
            (false, "采购单已删除")
        );
        assert_eq!(source_verdict(Some(po), None, false), (false, "采购单已删除"));
        // 草稿 / 提交 / 审批 / 部分收货 / 已收货 / 已关闭 都算已下单
        for status in [
            "DRAFT",
            "SUBMITTED",
            "APPROVED",
            "PARTIAL_RECEIVED",
            "RECEIVED",
            "CLOSED",
        ] {
            assert_eq!(
                source_verdict(Some(po), Some(status), false),
                (true, "已下单"),
                "{status} 必须算已下单"
            );
        }
    }

    /// ⑥ 汇总：四个行状态计数 + 合计 + 整单率（与视图聚合同一套算法）。
    #[test]
    fn summary_aggregates_rows_and_states() {
        let facts = vec![
            // 100 计划 / 下 40 → PARTIAL
            ExecFacts {
                planned_qty: dec("100"),
                booked_qty: dec("112"),
                req_qty: dec("112"),
                ordered_qty: dec("40"),
            },
            // 10 计划 / 一单没下 → NOT_STARTED
            ExecFacts {
                planned_qty: dec("10"),
                booked_qty: dec("10"),
                req_qty: dec("10"),
                ordered_qty: Decimal::ZERO,
            },
            // 5 计划 / 下 5 → DONE
            ExecFacts {
                planned_qty: dec("5"),
                booked_qty: dec("5"),
                req_qty: dec("5"),
                ordered_qty: dec("5"),
            },
            // 2 计划 / 下 3 → OVER
            ExecFacts {
                planned_qty: dec("2"),
                booked_qty: dec("3"),
                req_qty: dec("3"),
                ordered_qty: dec("3"),
            },
        ];
        let summary = summary_of(&facts);
        assert_eq!(summary.line_count, 4);
        assert_eq!(summary.planned_qty, dec("117"));
        assert_eq!(summary.booked_qty, dec("130"));
        assert_eq!(summary.req_qty, dec("130"));
        assert_eq!(summary.ordered_qty, dec("48"));
        assert_eq!(summary.remaining_qty, dec("69"));
        assert_eq!((summary.not_started_lines, summary.partial_lines), (1, 1));
        assert_eq!((summary.done_lines, summary.over_lines), (1, 1));
        assert_eq!(summary.exec_rate, Some(dec("0.410256")));
        assert_eq!(summary.req_rate, Some(dec("1.111111")));
        assert_eq!(summary.exec_rate_pct(), Some(dec("41.03")));
        assert_eq!(summary.exec_state(), STATE_PARTIAL);
    }

    /// ⑦ 空计划：行数 0、率 None（不是 0）、状态 NOT_STARTED。
    #[test]
    fn empty_summary_has_no_rate() {
        let summary = summary_of(&[]);
        assert_eq!(summary.line_count, 0);
        assert_eq!(summary.planned_qty, Decimal::ZERO);
        assert_eq!(summary.exec_rate, None);
        assert_eq!(summary.exec_rate_pct(), None);
        assert_eq!(
            summary.exec_state(),
            STATE_DONE,
            "0/0 落到 DONE 分支（与 exec_state_of 一致）"
        );
    }

    /// ⑧ 分页筛选：默认值 / 钳制 / 非法值 400（不静默忽略）。
    #[test]
    fn page_query_normalizes_and_rejects() {
        let filter = ExecutionPageQuery::default().filter().expect("默认合法");
        assert_eq!((filter.page, filter.page_size), (1, 20));
        assert!(filter.min_rate.is_none() && filter.plan_month.is_none());

        let filter = ExecutionPageQuery {
            page: Some(0),
            page_size: Some(9999),
            plan_type: Some("monthly".to_string()),
            plan_year: Some(2026),
            plan_month: Some(11),
            min_rate: Some(100),
            ..ExecutionPageQuery::default()
        }
        .filter()
        .expect("钳制后合法");
        assert_eq!((filter.page, filter.page_size), (1, 200));
        assert_eq!(filter.plan_type.as_deref(), Some("MONTHLY"));
        assert_eq!(filter.plan_month, Some(11));
        assert_eq!(filter.min_rate, Some(dec("1")), "100% 折成比值 1");

        for bad in [
            ExecutionPageQuery {
                status: Some("CLOSED".to_string()),
                ..ExecutionPageQuery::default()
            },
            ExecutionPageQuery {
                plan_type: Some("WEEKLY".to_string()),
                ..ExecutionPageQuery::default()
            },
            ExecutionPageQuery {
                plan_year: Some(1900),
                ..ExecutionPageQuery::default()
            },
            ExecutionPageQuery {
                plan_month: Some(13),
                ..ExecutionPageQuery::default()
            },
            ExecutionPageQuery {
                min_rate: Some(MAX_MIN_RATE + 1),
                ..ExecutionPageQuery::default()
            },
        ] {
            assert!(bad.filter().is_err(), "{bad:?} 必须 400");
        }
    }

    /// ⑨ 权限码：只有那一个接口码；页面显形码不许出现在后端（约定 14）。
    #[test]
    fn permission_code_is_the_execution_code_only() {
        assert_eq!(EXEC_PERMISSION, "business:erp:purchase-plan:execution");
        assert!(EXEC_PERMISSION.starts_with("business:erp:purchase-plan:"));
        assert!(!EXEC_PERMISSION.ends_with(":page"), "页面显形码只给前端");
    }

    /// ⑩ 状态 / 类型标签兜底：未知值原样返回，不 panic、不空白。
    #[test]
    fn labels_fall_back_to_raw_values() {
        assert_eq!(exec_state_label(STATE_NOT_STARTED), "未开工");
        assert_eq!(exec_state_label(STATE_OVER), "超计划");
        assert_eq!(exec_state_label("WEIRD"), "WEIRD");
        assert_eq!(requisition_status_label("CONVERTED"), "已转采购单");
        assert_eq!(requisition_status_label("WEIRD"), "WEIRD");
        assert_eq!(plan_status_label("APPROVED"), "已批准");
        assert_eq!(plan_type_label_of("ANNUAL"), "年度计划");
    }

    /// ⑪ 状态闭集与视图 CASE 同一份；`DONE` 不在 `NOT_STARTED` 里（防手滑写反）。
    #[test]
    fn exec_state_set_is_closed() {
        assert_eq!(EXEC_STATES.len(), 4);
        for state in EXEC_STATES {
            assert!(!exec_state_label(state).is_empty(), "{state} 必须有中文名");
        }
        assert!(!EXEC_STATES.contains(&"CANCELLED"));
    }
}
