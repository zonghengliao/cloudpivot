//! 合同执行量统计契约（M4-19）：**已执行 / 合同额**。
//!
//! 数据来源是 V0458 的两个视图（见
//! `backend/app/migrations/business/V0458__erp_contract_execution.up.sql`）：
//!
//! ```text
//! v_contract_execution_order   单据级：一行 = 一张挂在合同上的单据（采购单 / 销售单，含作废 / 软删）
//! v_contract_execution         单头级：一张存活合同一行（已执行额 + 剩余 + 执行率 + 单据数）
//! ```
//!
//! # 口径（每一条都有纯函数 + 单测钉住，与迁移头注释逐字一致）
//!
//! ```text
//! ① 计入的单据 = `deleted_at IS NULL AND status <> 'CANCELLED'`
//!                —— 草稿也算（单已生成、额度已占住，与两个合同额度守卫同口径）；
//!                   作废 / 软删不算（链已断），只进 released_doc_count 供解释
//! ② 已执行额   = Σ 单据 `total_with_tax`（价税合计）
//!                —— 与 `purchase_model::committed_amount` /
//!                   `sales_order_model::committed_amount` **逐字同口径**
//! ③ 剩余额     = 合同额 − 已执行额（**可为负**：守卫只拦新单，合同额改小 / 单据恢复会越界）
//! ④ 执行率     = 已执行额 / 合同额（6 位小数；合同额由 DDL `amount > 0` 保证 ⇒ 恒有值）
//! ⑤ 执行状态   = NO_DOWNSTREAM / NOT_STARTED / PARTIAL / DONE / OVER
//! ⑥ 下游类型   = 按**合同类型**分派：PURCHASE→采购单、SALES→销售单、SERVICE / LEASE→无
//! ⑦ 币种风险   = 计入的单据里币种 ≠ 合同币种的行数 → `currency_mismatch_doc_count`
//!                （**不改口径**：守卫也不看币种，总额照加，只把风险显形）
//! ```
//!
//! # 为什么单头里同时给「净额」
//!
//! `executed_amount`（含税）是**判据口径**（与守卫一致，比额度只能用这个）；
//! `executed_amount_net`（不含税）是财务报表要的口径。两个都返回、都标明含义 ——
//! 只给一个的话，另一侧的人一定会自己拿税率去反算，然后和报表差一分钱。
//!
//! # 权限码
//!
//! 一个接口码：`business:erp:contract:execution`（两个只读接口共用）。
//! 合同页的显形码沿用既有的 `business:erp:contract:list`（ERP 尚未解耦页面码/接口码，
//! 过渡期条款见 `docs/编码规范.md` §1.7.2 末尾）。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use cloudpivot_kernel::shared::error_code;

use crate::business::erp::contract::contract_dto::CONTRACT_TYPES;
use crate::business::erp::contract::contract_model::STATUSES;

// ==================== 域常量 ====================

/// 本模块的**唯一**接口权限码（V0458 的按钮行 `…004498`）。
pub const EXEC_PERMISSION: &str = "business:erp:contract:execution";

/// 单据级视图名（与 V0458 逐字一致）。
pub const ORDER_VIEW: &str = "v_contract_execution_order";
/// 单头级视图名（与 V0458 逐字一致）。
pub const HEAD_VIEW: &str = "v_contract_execution";

/// 执行率保留位数（视图 `round(…, 6)`；这里只做说明与纯函数对齐）。
pub const RATE_DP: u32 = 6;
/// 百分比保留位数（与框架协议执行量 / 采购计划执行率同口径：两位小数）。
pub const PCT_DP: u32 = 2;

/// 下游单据类型：采购单。
pub const DOWNSTREAM_PURCHASE: &str = "PURCHASE_ORDER";
/// 下游单据类型：销售单。
pub const DOWNSTREAM_SALES: &str = "SALES_ORDER";

/// 执行状态：合同类型没有下游执行单据（SERVICE / LEASE）。
pub const STATE_NO_DOWNSTREAM: &str = "NO_DOWNSTREAM";
/// 执行状态：一张单都没执行（已执行额 0）。
pub const STATE_NOT_STARTED: &str = "NOT_STARTED";
/// 执行状态：部分执行（0 < 已执行额 < 合同额）。
pub const STATE_PARTIAL: &str = "PARTIAL";
/// 执行状态：恰好用满（已执行额 = 合同额）。
pub const STATE_DONE: &str = "DONE";
/// 执行状态：超合同额（已执行额 > 合同额，执行率 > 100%）。
pub const STATE_OVER: &str = "OVER";
/// 状态闭集（与视图 `exec_state` 的 CASE 逐字一致）。
pub const EXEC_STATES: [&str; 5] = [
    STATE_NO_DOWNSTREAM,
    STATE_NOT_STARTED,
    STATE_PARTIAL,
    STATE_DONE,
    STATE_OVER,
];

/// 关键字长度上限（与框架协议执行量 / 采购计划分页同口径）。
pub const MAX_KEYWORD_LEN: usize = 64;

// ==================== 中文标签（报文 / 前端徽标共用） ====================

/// 执行状态中文名。未知名原样返回，绝不 panic。
pub fn exec_state_label(state: &str) -> &str {
    match state {
        STATE_NO_DOWNSTREAM => "无下游单据",
        STATE_NOT_STARTED => "未开工",
        STATE_PARTIAL => "部分执行",
        STATE_DONE => "已用满",
        STATE_OVER => "已超限",
        other => other,
    }
}

/// 合同类型中文名（与 `contract_model` 的枚举逐字一致；未知原样返回）。
pub fn contract_type_label(contract_type: &str) -> &str {
    match contract_type {
        "PURCHASE" => "采购合同",
        "SALES" => "销售合同",
        "SERVICE" => "服务合同",
        "LEASE" => "租赁合同",
        other => other,
    }
}

/// 下游单据类型中文名（`None` = 无下游）。
pub fn downstream_label(downstream: Option<&str>) -> Option<&'static str> {
    match downstream {
        Some(DOWNSTREAM_PURCHASE) => Some("采购单"),
        Some(DOWNSTREAM_SALES) => Some("销售单"),
        _ => None,
    }
}

/// 单据状态中文名（**采购单 ∪ 销售单**两个状态域的并集）。
///
/// 两个枚举里 `DRAFT` / `SUBMITTED` / `APPROVED` / `CLOSED` / `CANCELLED` 同名同义；
/// `PARTIAL_RECEIVED` / `RECEIVED` 是采购独有、`PARTIAL_OUT` / `OUTBOUND` 是销售独有。
/// 并成一份映射是为了让执行量下钻里一行「采购单」和一行「销售单」的措辞一致 ——
/// 两套映射必然漂移（一处改了另一处忘改），而这里不存在同名不同义的冲突。
pub fn order_status_label(status: &str) -> &str {
    match status {
        "DRAFT" => "草稿",
        "SUBMITTED" => "审批中",
        "APPROVED" => "已批准",
        "PARTIAL_RECEIVED" => "部分收货",
        "RECEIVED" => "已收货",
        "PARTIAL_OUT" => "部分出库",
        "OUTBOUND" => "已出库",
        "CLOSED" => "已关闭",
        "CANCELLED" => "已作废",
        other => other,
    }
}

/// 400 参数错误（与其它 ERP 模块同一个出口）。
pub fn param_error(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 纯函数（handler / model / 单测共用） ====================

/// 剩余可执行额 = 合同额 − 已执行额（**故意保留负数**：越界要能看出来）。
///
/// 合同额由 `ck_biz_contract_amount (amount > 0)` 保证非空非零，所以这里恒有值 ——
/// 与框架协议的「封顶可为 NULL（不限）」不同，合同不存在「不限」这一态。
pub fn remaining_of(amount: Decimal, executed: Decimal) -> Decimal {
    amount - executed
}

/// 执行率 = 已执行额 / 合同额（6 位小数）。
///
/// 合同额 `<= 0` 时返回 `None` 而不是 0：没有分母就没有比率。DDL 已挡住 `amount <= 0`，
/// 这里是对「绕过库层写进来的脏值」兜底（宁可显示 null，也不显示一个假 0%）。
pub fn rate_of(executed: Decimal, amount: Decimal) -> Option<Decimal> {
    if amount <= Decimal::ZERO {
        return None;
    }
    Some((executed / amount).round_dp(RATE_DP))
}

/// 百分比（0–100，两位小数）：前端进度条直接用。
pub fn pct_of(rate: Option<Decimal>) -> Option<Decimal> {
    rate.map(|value| (value * Decimal::from(100)).round_dp(PCT_DP))
}

/// 执行状态判据（与视图的 CASE **逐字对应**，改一处必须两处一起改）。
///
/// * `downstream = None` ⇒ NO_DOWNSTREAM（**先**判：服务/租赁合同没有可执行的单据，
///   报成 NOT_STARTED 会被读成「该下单了」）
/// * `executed > amount` ⇒ OVER；`executed == amount` ⇒ DONE；`0 < executed < amount` ⇒ PARTIAL；
///   `executed == 0` ⇒ NOT_STARTED
pub fn exec_state_of(downstream: Option<&str>, executed: Decimal, amount: Decimal) -> &'static str {
    if downstream.is_none() {
        return STATE_NO_DOWNSTREAM;
    }
    if executed > amount {
        STATE_OVER
    } else if executed >= amount {
        STATE_DONE
    } else if executed > Decimal::ZERO {
        STATE_PARTIAL
    } else {
        STATE_NOT_STARTED
    }
}

/// 下游类型分派（与视图的 `downstream_type` CASE 逐字一致）。
pub fn downstream_of(contract_type: &str) -> Option<&'static str> {
    match contract_type {
        "PURCHASE" => Some(DOWNSTREAM_PURCHASE),
        "SALES" => Some(DOWNSTREAM_SALES),
        _ => None,
    }
}

/// 单据是否计入已执行额 + 人话原因（与视图 `counted` 逐字一致）。
///
/// 判据顺序：**先看作废、再看软删**（一张单既作废又软删时，用户最关心「它被作废了」；
/// 视图只给布尔，原因在这里按同一顺序给出，所以两边文案不会打架）。
pub fn source_verdict(order_status: &str, deleted: bool) -> (bool, &'static str) {
    if order_status == "CANCELLED" {
        (false, "单据已作废（不占合同额）")
    } else if deleted {
        (false, "单据已删除（不占合同额）")
    } else {
        (true, "计入已执行额")
    }
}

// ==================== 入参 ====================

/// 执行量总览分页查询参数（`GET /business/erp/contract/execution/page`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ExecutionPageQuery {
    /// 页码（1 起）。
    pub page: Option<u64>,
    /// 每页条数（1–200）。
    pub page_size: Option<u64>,
    /// 关键字（合同号 / 合同名称 / 乙方名称 / 负责人姓名）。
    pub keyword: Option<String>,
    /// 合同类型（PURCHASE / SALES / SERVICE / LEASE）。
    pub contract_type: Option<String>,
    /// 合同状态（DRAFT / PENDING_APPROVAL / SIGNED / ACTIVE / COMPLETED / TERMINATED）。
    pub status: Option<String>,
    /// 执行状态（NO_DOWNSTREAM / NOT_STARTED / PARTIAL / DONE / OVER）。
    pub exec_state: Option<String>,
    /// 只看「已超合同额」的合同。
    pub over_only: Option<bool>,
    /// 只看「已过期」的合同（`end_date < 当日`）。
    pub expired_only: Option<bool>,
    /// 只看「有币种串了的单据」的合同。
    pub currency_mismatch_only: Option<bool>,
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
    /// 合同类型（已校验在闭集内）。
    pub contract_type: Option<String>,
    /// 合同状态（已校验在闭集内）。
    pub status: Option<String>,
    /// 执行状态（已校验在闭集内）。
    pub exec_state: Option<String>,
    /// 只看超合同额。
    pub over_only: bool,
    /// 只看已过期。
    pub expired_only: bool,
    /// 只看有币种串了的单据。
    pub currency_mismatch_only: bool,
}

impl ExecutionPageQuery {
    /// 校验并归一化（非法值 → 400，不静默忽略）。
    pub fn filter(&self) -> std::result::Result<ExecutionFilter, ApiError> {
        let page = self.page.unwrap_or(1).max(1);
        let page_size = self.page_size.unwrap_or(20).clamp(1, 200);
        let contract_type = match self.contract_type.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(value) => {
                let value = value.to_ascii_uppercase();
                if !CONTRACT_TYPES.contains(&value.as_str()) {
                    return Err(param_error(format!(
                        "contract_type={value} 不在白名单内（{}）",
                        CONTRACT_TYPES.join(" / ")
                    )));
                }
                Some(value)
            }
        };
        let status = match self.status.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(value) => {
                let value = value.to_ascii_uppercase();
                if !STATUSES.contains(&value.as_str()) {
                    return Err(param_error(format!(
                        "status={value} 不在状态闭集内（{}）",
                        STATUSES.join(" / ")
                    )));
                }
                Some(value)
            }
        };
        let exec_state = match self.exec_state.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(value) => {
                let value = value.to_ascii_uppercase();
                if !EXEC_STATES.contains(&value.as_str()) {
                    return Err(param_error(format!(
                        "exec_state={value} 不在闭集内（{}）",
                        EXEC_STATES.join(" / ")
                    )));
                }
                Some(value)
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
            contract_type,
            status,
            exec_state,
            over_only: self.over_only.unwrap_or(false),
            expired_only: self.expired_only.unwrap_or(false),
            currency_mismatch_only: self.currency_mismatch_only.unwrap_or(false),
        })
    }
}

// ==================== 出参 ====================

/// 合同执行量（总览的一行 / 详情里的单头）。
#[derive(Debug, Clone, Serialize)]
pub struct ContractExecRowRes {
    /// 合同 id。
    pub contract_id: String,
    /// 合同号。
    pub contract_no: String,
    /// 合同名称。
    pub contract_name: String,
    /// 合同类型。
    pub contract_type: String,
    /// 合同类型中文。
    pub contract_type_label: String,
    /// 合同状态。
    pub status: String,
    /// 合同状态中文。
    pub status_label: String,
    /// 今天能不能接着下单（SIGNED / ACTIVE）。
    pub usable_for_order: bool,
    /// 合同是否已过期（`end_date < 当日`）。
    pub expired: bool,
    /// 距到期日还有几天（已过期 = 负数；无到期日 = null）。
    pub days_left: Option<i64>,
    /// 甲方 id。
    pub party_a_id: Option<String>,
    /// 甲方名称快照。
    pub party_a_name: Option<String>,
    /// 乙方类型（SUPPLIER / CUSTOMER）。
    pub party_b_type: String,
    /// 乙方 id。
    pub party_b_id: String,
    /// 乙方名称快照（签约时）。
    pub party_b_name: String,
    /// 乙方现名（实时 join；主档已删则 null）。
    pub party_b_current_name: Option<String>,
    /// 展示用乙方名：快照优先，快照空时用现名。
    pub party_b_display_name: String,
    /// 负责人 id。
    pub owner_id: Option<String>,
    /// 负责人姓名（实时 join）。
    pub owner_name: Option<String>,
    /// 归属部门 id。
    pub dept_id: Option<String>,
    /// 归属部门名（实时 join）。
    pub dept_name: Option<String>,
    /// 合同额（含税口径，判据分母）。
    pub amount: Decimal,
    /// 合同币种。
    pub currency_code: String,
    /// 已执行额 = Σ 计入单据 `total_with_tax`（判据口径，与额度守卫一致）。
    pub executed_amount: Decimal,
    /// 已执行净额 = Σ 计入单据 `total_amount`（不含税，财务报表口径）。
    pub executed_amount_net: Decimal,
    /// 剩余可执行额（**可为负**）。
    pub remaining_amount: Decimal,
    /// 执行率（0–1，6 位）。
    pub exec_amount_rate: Option<Decimal>,
    /// 执行率百分比（0–100，2 位）。
    pub exec_amount_rate_pct: Option<Decimal>,
    /// 执行状态（NO_DOWNSTREAM / NOT_STARTED / PARTIAL / DONE / OVER）。
    pub exec_state: String,
    /// 执行状态中文。
    pub exec_state_label: String,
    /// 下游单据类型（SERVICE / LEASE 为 null）。
    pub downstream_type: Option<String>,
    /// 下游单据类型中文（同上）。
    pub downstream_type_label: Option<String>,
    /// 计入已执行额的单据数。
    pub order_doc_count: i64,
    /// 释放（作废 / 软删）的单据数。
    pub released_doc_count: i64,
    /// 计入的单据里币种 ≠ 合同币种的行数。
    pub currency_mismatch_doc_count: i64,
    /// 计入的采购单数。
    pub purchase_doc_count: i64,
    /// 计入的销售单数。
    pub sales_doc_count: i64,
    /// 最近一次执行日。
    pub last_order_on: Option<String>,
    /// 签订日期。
    pub sign_date: Option<String>,
    /// 生效日。
    pub start_date: Option<String>,
    /// 到期日。
    pub end_date: Option<String>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
}

/// 下钻：这张合同上的单据（含作废 / 软删，`counted=false` 标明原因）。
#[derive(Debug, Clone, Serialize)]
pub struct ExecSourceRes {
    /// 单据 id。
    pub order_id: String,
    /// 单据号。
    pub order_no: String,
    /// 单据类型（PURCHASE_ORDER / SALES_ORDER）。
    pub source_type: String,
    /// 单据类型中文。
    pub source_type_label: String,
    /// 单据状态。
    pub order_status: String,
    /// 单据状态中文。
    pub order_status_label: String,
    /// 单据日期。
    pub order_date: String,
    /// 单据币种。
    pub currency_code: String,
    /// 单据币种是否等于合同币种（false = 币种串了，只显形）。
    pub currency_matched: bool,
    /// 不含税金额。
    pub total_amount: Decimal,
    /// 含税金额（计入已执行额的那个数）。
    pub total_with_tax: Decimal,
    /// 单据软删时间（有值 = 已删）。
    pub order_deleted_at: Option<String>,
    /// 是否计入已执行额。
    pub counted: bool,
    /// 判定原因（「计入已执行额」/「单据已作废…」/「单据已删除…」）。
    pub verdict: String,
}

/// 单合同执行量详情：单头汇总 + 下钻单据。
#[derive(Debug, Clone, Serialize)]
pub struct ContractExecutionRes {
    /// 单头汇总。
    pub contract: ContractExecRowRes,
    /// 下钻单据（按单据日期 / 单号升序，含作废与软删）。
    pub sources: Vec<ExecSourceRes>,
    /// 单据总行数。
    pub source_count: usize,
    /// 计入已执行额的条数。
    pub counted_count: usize,
    /// 释放（作废 / 软删）的条数。
    pub released_count: usize,
}

/// 单据类型中文（下钻用）。
pub fn source_type_label(source_type: &str) -> &str {
    match source_type {
        DOWNSTREAM_PURCHASE => "采购单",
        DOWNSTREAM_SALES => "销售单",
        other => other,
    }
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::contract::contract_model;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    /// 剩余额：恒有值，负数必须保留（越界要显形，不许截成 0）。
    #[test]
    fn remaining_keeps_negative() {
        assert_eq!(remaining_of(dec("1000"), dec("300")), dec("700"));
        assert_eq!(remaining_of(dec("1000"), dec("1000")), Decimal::ZERO);
        assert_eq!(remaining_of(dec("1000"), dec("1200")), dec("-200"));
    }

    /// 执行率：分母为 0 / 负时是 None（不是 0 —— 0 会被读成「一点没做」）。
    #[test]
    fn rate_needs_a_positive_denominator() {
        assert_eq!(rate_of(dec("30"), dec("100")), Some(dec("0.3")));
        assert_eq!(rate_of(dec("30"), Decimal::ZERO), None);
        assert_eq!(rate_of(dec("30"), dec("-1")), None);
        // 6 位小数（与视图 round(…,6) 同精度）
        assert_eq!(rate_of(dec("1"), dec("3")), Some(dec("0.333333")));
        // 超合同额：率 > 1，不截断
        assert_eq!(rate_of(dec("120"), dec("100")), Some(dec("1.2")));
    }

    /// 百分比：两位小数，None 一路传下去。
    #[test]
    fn pct_keeps_two_decimals() {
        assert_eq!(pct_of(Some(dec("0.333333"))), Some(dec("33.33")));
        assert_eq!(pct_of(Some(dec("1.2"))), Some(dec("120.00")));
        assert_eq!(pct_of(None), None);
    }

    /// 执行状态五态 + 边界（== 合同额是 DONE，不是 OVER；无下游优先判）。
    #[test]
    fn exec_state_matrix() {
        assert_eq!(
            exec_state_of(None, dec("999"), dec("100")),
            STATE_NO_DOWNSTREAM,
            "服务/租赁合同没有下游单据：不许报成 NOT_STARTED"
        );
        assert_eq!(
            exec_state_of(Some(DOWNSTREAM_PURCHASE), Decimal::ZERO, dec("100")),
            STATE_NOT_STARTED
        );
        assert_eq!(
            exec_state_of(Some(DOWNSTREAM_PURCHASE), dec("1"), dec("100")),
            STATE_PARTIAL
        );
        assert_eq!(
            exec_state_of(Some(DOWNSTREAM_PURCHASE), dec("100"), dec("100")),
            STATE_DONE
        );
        assert_eq!(
            exec_state_of(Some(DOWNSTREAM_PURCHASE), dec("100.0001"), dec("100")),
            STATE_OVER
        );
    }

    /// 下游类型按合同类型分派（SERVICE / LEASE 无下游）。
    #[test]
    fn downstream_follows_contract_type() {
        assert_eq!(downstream_of("PURCHASE"), Some(DOWNSTREAM_PURCHASE));
        assert_eq!(downstream_of("SALES"), Some(DOWNSTREAM_SALES));
        assert_eq!(downstream_of("SERVICE"), None);
        assert_eq!(downstream_of("LEASE"), None);
        assert_eq!(downstream_of("WHATEVER"), None);
        // 状态判据与分派必须同源：SERVICE + 0 元单 → NO_DOWNSTREAM
        assert_eq!(
            exec_state_of(downstream_of("SERVICE"), Decimal::ZERO, dec("100")),
            STATE_NO_DOWNSTREAM
        );
    }

    /// 单据判定：先作废再软删（两个都占时给「作废」，与视图 counted 一致）。
    #[test]
    fn source_verdict_prefers_cancelled() {
        assert_eq!(source_verdict("APPROVED", false), (true, "计入已执行额"));
        assert_eq!(source_verdict("DRAFT", false), (true, "计入已执行额"));
        assert!(!source_verdict("CANCELLED", false).0);
        assert_eq!(source_verdict("CANCELLED", true), (false, "单据已作废（不占合同额）"));
        assert_eq!(source_verdict("APPROVED", true), (false, "单据已删除（不占合同额）"));
        assert!(source_verdict("CANCELLED", false).1.contains("作废"));
        assert!(source_verdict("RECEIVED", true).1.contains("删除"));
    }

    /// 标签：未知值原样返回（不许 panic，也不许显示成空）。
    #[test]
    fn labels_never_panic() {
        assert_eq!(exec_state_label(STATE_OVER), "已超限");
        assert_eq!(exec_state_label("WHATEVER"), "WHATEVER");
        assert_eq!(contract_type_label("PURCHASE"), "采购合同");
        assert_eq!(contract_type_label("ODD"), "ODD");
        assert_eq!(downstream_label(Some(DOWNSTREAM_SALES)), Some("销售单"));
        assert_eq!(downstream_label(None), None);
        assert_eq!(order_status_label("PARTIAL_RECEIVED"), "部分收货");
        assert_eq!(order_status_label("OUTBOUND"), "已出库");
        assert_eq!(order_status_label("WHATEVER"), "WHATEVER");
        assert_eq!(source_type_label(DOWNSTREAM_PURCHASE), "采购单");
        assert_eq!(source_type_label("ODD"), "ODD");
        // 合同状态中文沿用 contract_model 的唯一一份措辞
        assert_eq!(contract_model::status_label("ACTIVE"), "已生效");
    }

    /// 入参校验：非法枚举一律 400，不静默忽略。
    #[test]
    fn filter_rejects_illegal_enums() {
        let bad = ExecutionPageQuery {
            contract_type: Some("BUY".to_string()),
            ..Default::default()
        };
        assert_eq!(bad.filter().expect_err("非法合同类型必须 400").status, 400);

        let bad = ExecutionPageQuery {
            status: Some("CANCELLED".to_string()),
            ..Default::default()
        };
        assert_eq!(bad.filter().expect_err("合同没有 CANCELLED 态").status, 400);

        let bad = ExecutionPageQuery {
            exec_state: Some("UNLIMITED".to_string()),
            ..Default::default()
        };
        assert_eq!(bad.filter().expect_err("合同没有不限量态").status, 400);

        let bad = ExecutionPageQuery {
            keyword: Some("x".repeat(MAX_KEYWORD_LEN + 1)),
            ..Default::default()
        };
        assert_eq!(bad.filter().expect_err("超长关键字必须 400").status, 400);
    }

    /// 入参归一化：缺省 1 / 20、上限 200、空串当没传、大小写归一。
    #[test]
    fn filter_normalizes_defaults() {
        let filter = ExecutionPageQuery::default().filter().expect("缺省合法");
        assert_eq!((filter.page, filter.page_size), (1, 20));
        assert_eq!(filter.keyword, None);
        assert!(!filter.over_only && !filter.expired_only && !filter.currency_mismatch_only);

        let filter = ExecutionPageQuery {
            page: Some(0),
            page_size: Some(9999),
            keyword: Some("  HT-2026  ".to_string()),
            contract_type: Some(" purchase ".to_string()),
            exec_state: Some("over".to_string()),
            over_only: Some(true),
            ..Default::default()
        }
        .filter()
        .expect("归一化后合法");
        assert_eq!((filter.page, filter.page_size), (1, 200));
        assert_eq!(filter.keyword.as_deref(), Some("HT-2026"));
        assert_eq!(filter.contract_type.as_deref(), Some("PURCHASE"));
        assert_eq!(filter.exec_state.as_deref(), Some("OVER"));
        assert!(filter.over_only);
    }

    /// 视图名与迁移逐字一致（改名时这里先红，而不是运行期 500）。
    #[test]
    fn view_names_match_migration() {
        assert_eq!(ORDER_VIEW, "v_contract_execution_order");
        assert_eq!(HEAD_VIEW, "v_contract_execution");
        assert_eq!(EXEC_PERMISSION, "business:erp:contract:execution");
        // 权限码四段式 + 小写动作段（与 contract_handler::PERMISSIONS 的格式断言同口径）
        assert_eq!(EXEC_PERMISSION.matches(':').count(), 3);
        let action = EXEC_PERMISSION.rsplit(':').next().unwrap_or_default();
        assert!(action.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
    }

    /// 执行状态闭集必须覆盖视图 CASE 的每一个分支（少一个 = 筛选下拉会漏项）。
    #[test]
    fn exec_states_cover_view_cases() {
        assert_eq!(EXEC_STATES.len(), 5);
        for state in EXEC_STATES {
            assert_ne!(exec_state_label(state), state, "{state} 必须有中文名");
        }
    }
}
