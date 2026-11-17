//! 采购计划契约（M4-02）：建单 / 改单 / 删单 / 分页 / 详情 / 提交审批。
//!
//! 与 [`crate::business::erp::purchase::purchase_requisition_dto`] 同一套约定：
//! 主键与外键一律 `String`（`nik::uuid::Uuid` 没开 serde feature）、
//! 金额/数量 `Decimal`（**JSON 里是字符串**，4/6 位标度）、
//! 状态与类型一律大写字符串且与 V0446 的 `CHECK` 逐字一致。
//!
//! # 本卡的口径（每条都有纯函数 + 单测钉住，不能靠 handler 里随手写）
//!
//! ```text
//! ① 类型与月份自洽：ANNUAL ⇒ plan_month 必须缺省；MONTHLY ⇒ 必须 1–12。
//!    库层 `ck_biz_pp_period` 也拦，但在这里先拦能给出人话报文（400 而不是 500）。
//! ② 单头三个 total_* 是**派生列**：请求体里即便带了也一律忽略，
//!    金额由 `Σ(planned_qty × planned_price)` 现算（`line_amount`）。
//! ③ 明细粒度 = 物料 × 仓库：同一张单里 (material_id, warehouse_id) 重复在
//!    **请求层面**就拒（库层 `uk_biz_pp_item_cell` 兜底是 500），并指出是第几行撞了第几行。
//! ④ 月度计划的 `parent_plan_id` 必须指向**年度**计划（跨行语义，库层 CHECK 做不到），
//!    且 (plan_year, plan_month) 必须落在父年度内 —— 交给 handler 查父单后判定，
//!    判据函数 [`check_parent_period`] 是纯函数。
//! ```
//!
//! # 权限码（与 V0447 的 `sys_menu.auth_code` 逐字一致）
//!
//! 页面显形码 `business:erp:purchase-plan:page` **不在这里**（AGENTS 约定 14：
//! 只给前端，后端零引用）。这里只有 5 个接口码：
//! `{list,create,update,delete,submit}`。

use nik::prelude::chrono::{Duration, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use cloudpivot_kernel::business::shared::doc_lines;
use cloudpivot_kernel::shared::error_code;

pub use crate::business::erp::purchase::purchase_plan_model::{
    PLAN_TYPE_ANNUAL, PLAN_TYPE_MONTHLY, PLAN_TYPES, STATUS_APPROVED, STATUS_DRAFT, STATUS_SUBMITTED, STATUSES,
};

// ==================== 域常量 ====================

/// 明细行数上限（一次建单最多多少行；计划是「一期一编」，比申请单宽一些）。
pub const MAX_LINES: usize = 500;
/// 计划名称长度上限（与 `biz_purchase_plan.plan_name VARCHAR(128)` 一致）。
pub const MAX_NAME_LEN: usize = 128;
/// 备注长度上限。
pub const MAX_REMARK_LEN: usize = 500;
/// 计划年度下界（与 `ck_biz_pp_year` 一致）。
pub const MIN_PLAN_YEAR: i32 = 1990;
/// 计划年度上界（与 `ck_biz_pp_year` 一致）。
pub const MAX_PLAN_YEAR: i32 = 2200;

/// 本模块 5 个**接口**权限码（页面显形码 `…:page` 只给前端，按约定 14 不在此列）。
pub const PLAN_PERMISSIONS: [&str; 5] = [
    "business:erp:purchase-plan:list",
    "business:erp:purchase-plan:create",
    "business:erp:purchase-plan:update",
    "business:erp:purchase-plan:delete",
    "business:erp:purchase-plan:submit",
];

/// 今天（北京时间）的日期。
pub fn today_cst() -> chrono::NaiveDate {
    (Utc::now() + Duration::hours(8)).date_naive()
}

/// 状态中文名（报文用）。未知名原样返回，绝不 panic。
pub fn status_label(status: &str) -> &str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SUBMITTED => "审批中",
        STATUS_APPROVED => "已批准",
        other => other,
    }
}

/// 计划类型中文名（报文用）。
pub fn plan_type_label(plan_type: &str) -> &str {
    match plan_type {
        PLAN_TYPE_ANNUAL => "年度计划",
        PLAN_TYPE_MONTHLY => "月度计划",
        other => other,
    }
}

// ==================== 入参 ====================

/// 新建 / 修改采购计划（`PUT` 为整单替换：单头字段 + 全量明细）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CreatePlanReq {
    /// 计划名称（必填，≤128 字）。
    pub plan_name: Option<String>,
    /// 计划类型：ANNUAL / MONTHLY（必填）。
    pub plan_type: Option<String>,
    /// 计划年度（必填，1990–2200）。
    pub plan_year: Option<i32>,
    /// 月份（月度计划必填 1–12；年度计划不许传）。
    pub plan_month: Option<i32>,
    /// 月度计划挂到的年度计划 id（可空；给了就必须是**已存在的年度计划**）。
    pub parent_plan_id: Option<String>,
    /// 编制部门（可空 = 公司级计划）。
    pub dept_id: Option<String>,
    /// 编制人（缺省 = 当前登录人；姓名一律服务端快照）。
    pub planner_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 明细（不能为空；`(物料, 仓库)` 不许重复）。
    pub items: Vec<PlanItemReq>,
}

/// 计划明细行（数量 > 0；单价 ≥ 0；单位由物料带出，客户端不许传）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlanItemReq {
    /// 物料（必填）。
    pub material_id: Option<String>,
    /// 仓库（必填 —— 粒度轴之一）。
    pub warehouse_id: Option<String>,
    /// 计划量（> 0，4 位标度）。
    pub planned_qty: Option<Decimal>,
    /// 预估不含税单价（≥ 0，6 位标度；缺省 0）。
    pub planned_price: Option<Decimal>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 提交审批（可选说明，写进备注供审批人参考）。
///
/// `skip_approval` / `skip_reason` 平铺进请求体（与采购申请 / 采购订单同款）：
/// 只有**没配已发布的 `pp_approve` 流程**时才用得上，且需要
/// `business:erp:skip-approval` + 理由 ≥10 字。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SubmitPlanReq {
    /// 说明（可选，≤ 200 字）。
    pub reason: Option<String>,
    /// 显式跳过审批的请求片段。
    #[serde(flatten)]
    pub skip: crate::business::erp::approval_gate::SkipApprovalReq,
}

// ==================== 出参 ====================

/// 分页查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlanPageQuery {
    /// 页码（1 起）。
    pub page: Option<u64>,
    /// 每页条数（1–200）。
    pub page_size: Option<u64>,
    /// 关键字（单号 / 名称 / 部门名 / 备注）。
    pub keyword: Option<String>,
    /// 状态筛选。
    pub status: Option<String>,
    /// 类型筛选。
    pub plan_type: Option<String>,
    /// 年度筛选。
    pub plan_year: Option<i32>,
    /// 部门筛选。
    pub dept_id: Option<String>,
}

/// 已校验的分页筛选条件（model 只认这个，不再看 `Option`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFilter {
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
    /// 部门。
    pub dept_id: Option<Uuid>,
}

impl PlanPageQuery {
    /// 校验并归一化（非法值 → 400，不静默忽略）。
    pub fn filter(&self) -> std::result::Result<PlanFilter, ApiError> {
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
        let keyword = match self.keyword.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(text) => {
                if text.chars().count() > 64 {
                    return Err(bad_request("keyword 最长 64 个字符"));
                }
                Some(text.to_string())
            }
        };
        Ok(PlanFilter {
            page,
            page_size,
            keyword,
            status,
            plan_type,
            plan_year,
            dept_id: parse_optional_uuid(self.dept_id.as_deref(), "dept_id")?,
        })
    }
}

/// 单头（列表 / 详情）。
#[derive(Debug, Clone, Serialize)]
pub struct PlanRowRes {
    /// 主键。
    pub id: String,
    /// 计划单号。
    pub plan_no: String,
    /// 计划名称。
    pub plan_name: String,
    /// 类型。
    pub plan_type: String,
    /// 类型中文。
    pub plan_type_label: String,
    /// 年度。
    pub plan_year: i32,
    /// 月份（年度计划为 null）。
    pub plan_month: Option<i32>,
    /// 父年度计划 id。
    pub parent_plan_id: Option<String>,
    /// 父年度计划单号（实时查）。
    pub parent_plan_no: Option<String>,
    /// 编制部门。
    pub dept_id: Option<String>,
    /// 编制部门名（快照优先，空则实时 join）。
    pub dept_name: Option<String>,
    /// 编制人。
    pub planner_id: Option<String>,
    /// 编制人姓名（快照；审计类，按约定 13）。
    pub planner_name: Option<String>,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 审批实例。
    pub approval_instance_id: Option<String>,
    /// 计划量合计。
    pub total_planned_qty: Decimal,
    /// 已执行量合计。
    pub total_executed_qty: Decimal,
    /// 计划金额合计（不含税）。
    pub total_planned_amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 明细行数。
    pub item_count: i64,
    /// 创建时间。
    pub created_at: String,
    /// 创建人。
    pub create_by: Option<String>,
    /// 能否编辑 / 删除 / 提交（`DRAFT` 才行）—— 前端不必自己推状态机。
    pub editable: bool,
}

/// 明细行。
#[derive(Debug, Clone, Serialize)]
pub struct PlanItemRes {
    /// 主键。
    pub id: String,
    /// 行号。
    pub line_no: i32,
    /// 物料。
    pub material_id: String,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 仓库。
    pub warehouse_id: String,
    /// 仓库名称。
    pub warehouse_name: String,
    /// 单位。
    pub unit_id: String,
    /// 单位名称。
    pub unit_name: String,
    /// 计划量。
    pub planned_qty: Decimal,
    /// 已执行量。
    pub executed_qty: Decimal,
    /// 单价。
    pub planned_price: Decimal,
    /// 金额。
    pub planned_amount: Decimal,
    /// 行备注。
    pub remark: Option<String>,
}

/// 详情（单头 + 明细）。
#[derive(Debug, Clone, Serialize)]
pub struct PlanDetailRes {
    /// 单头。
    pub plan: PlanRowRes,
    /// 明细。
    pub items: Vec<PlanItemRes>,
}

// ==================== 纯校验函数（handler 与单测共用） ====================

/// 400。
pub fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 解析必填 uuid 字段（空串 / 缺省 → 400）。
pub fn parse_uuid_field(raw: Option<&str>, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    Uuid::parse_str(text).map_err(|_| bad_request(format!("{field}={text} 不是合法 uuid")))
}

/// 解析可选 uuid 字段（空串 → None；非法 → 400）。
pub fn parse_optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => Uuid::parse_str(text)
            .map(Some)
            .map_err(|_| bad_request(format!("{field}={text} 不是合法 uuid"))),
    }
}

/// 清洗备注：trim → 空串折成 `None` → 超长 400。
pub fn clean_remark(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim) else {
        return Ok(None);
    };
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().count() > MAX_REMARK_LEN {
        return Err(bad_request(format!(
            "备注最长 {MAX_REMARK_LEN} 个字符（收到 {}）",
            text.chars().count()
        )));
    }
    Ok(Some(text.to_string()))
}

/// 校验计划名称（trim 后非空，≤128）。
pub fn check_name(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(bad_request("plan_name 不能为空"));
    }
    if text.chars().count() > MAX_NAME_LEN {
        return Err(bad_request(format!(
            "plan_name 最长 {MAX_NAME_LEN} 个字符（收到 {}）",
            text.chars().count()
        )));
    }
    Ok(text.to_string())
}

/// 校验类型（闭集 ANNUAL / MONTHLY）。
pub fn check_type(raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim().to_ascii_uppercase();
    if !PLAN_TYPES.contains(&text.as_str()) {
        return Err(bad_request(format!(
            "plan_type={raw} 不是合法类型（{}）",
            PLAN_TYPES.join(" / ")
        )));
    }
    Ok(text)
}

/// 校验年度（1990–2200）。
pub fn check_year(year: i32) -> std::result::Result<i32, ApiError> {
    if !(MIN_PLAN_YEAR..=MAX_PLAN_YEAR).contains(&year) {
        return Err(bad_request(format!(
            "plan_year={year} 超范围（{MIN_PLAN_YEAR}–{MAX_PLAN_YEAR}）"
        )));
    }
    Ok(year)
}

/// 类型 + 月份自洽（年度计划不许带月；月度计划必须 1–12）。
///
/// 与库层 `ck_biz_pp_period` 同一口径，但这里给人话报文；
/// **两处都要有**：库层拦的是「绕过接口的写入」，这里拦的是「正常请求的坏参数」。
pub fn check_month_for_type(plan_type: &str, plan_month: Option<i32>) -> std::result::Result<Option<i32>, ApiError> {
    // 与 [`check_type`] 同一口径：大小写与空格都容错（前端偶尔把枚举小写传上来）。
    let plan_type = plan_type.trim().to_ascii_uppercase();
    match (plan_type.as_str(), plan_month) {
        (PLAN_TYPE_ANNUAL, None) => Ok(None),
        (PLAN_TYPE_ANNUAL, Some(month)) => Err(bad_request(format!(
            "年度计划不能带 plan_month（收到 {month}）：要编某个月请用 MONTHLY"
        ))),
        (PLAN_TYPE_MONTHLY, Some(month)) if (1..=12).contains(&month) => Ok(Some(month)),
        (PLAN_TYPE_MONTHLY, Some(month)) => Err(bad_request(format!("月度计划的 plan_month={month} 超范围（1–12）"))),
        (PLAN_TYPE_MONTHLY, None) => Err(bad_request("月度计划必须给 plan_month（1–12）；只说年度请用 ANNUAL")),
        (other, _) => Err(bad_request(format!("plan_type={other} 不是合法类型"))),
    }
}

/// 月度计划的父单是否合规（纯判据：父单类型 / 年度 / 月份落在父年度内）。
///
/// 参数是「父单的事实」，查库交给 handler —— 这样判据本身不需要库即可单测。
pub fn check_parent_period(
    plan_type: &str,
    plan_year: i32,
    plan_month: Option<i32>,
    parent: Option<(&str, i32, Option<i32>)>,
) -> std::result::Result<(), ApiError> {
    let Some((parent_type, parent_year, parent_month)) = parent else {
        return Ok(());
    };
    if plan_type != PLAN_TYPE_MONTHLY {
        return Err(bad_request(format!(
            "只有月度计划能挂 parent_plan_id（当前是 {}）",
            plan_type_label(plan_type)
        )));
    }
    if parent_type != PLAN_TYPE_ANNUAL {
        return Err(bad_request(format!(
            "parent_plan_id 必须指向**年度**计划（对方是 {}）",
            plan_type_label(parent_type)
        )));
    }
    if parent_month.is_some() {
        return Err(bad_request("parent_plan_id 指向的计划自己不应该是月度计划"));
    }
    if parent_year != plan_year {
        return Err(bad_request(format!(
            "月度计划的年度（{plan_year}）与父年度计划（{parent_year}）不一致"
        )));
    }
    let _ = plan_month;
    Ok(())
}

/// 校验计划量（> 0，4 位舍入）。
pub fn check_qty(raw: Option<Decimal>, field: &str) -> std::result::Result<Decimal, ApiError> {
    let Some(qty) = raw else {
        return Err(bad_request(format!("{field} 不能为空")));
    };
    let qty = qty.round_dp(4);
    if qty <= Decimal::ZERO {
        return Err(bad_request(format!("{field}={qty} 必须 > 0")));
    }
    Ok(qty)
}

/// 校验单价（≥ 0，6 位舍入；缺省 0）。
pub fn check_price(raw: Option<Decimal>, field: &str) -> std::result::Result<Decimal, ApiError> {
    let price = raw.unwrap_or(Decimal::ZERO).round_dp(6);
    if price < Decimal::ZERO {
        return Err(bad_request(format!("{field}={price} 不能为负")));
    }
    Ok(price)
}

/// 行金额（4 位舍入）：`qty × price`。
pub fn line_amount(qty: Decimal, price: Decimal) -> Decimal {
    (qty * price).round_dp(4)
}

/// 明细行的「卡面粒度」键：(material_id, warehouse_id)。
pub fn cell_key(material_id: Uuid, warehouse_id: Uuid) -> (Uuid, Uuid) {
    (material_id, warehouse_id)
}

/// 明细行数闸：非空 + ≤ `MAX_LINES`。
pub fn check_items(items: &[PlanItemReq]) -> std::result::Result<(), ApiError> {
    if items.is_empty() {
        return Err(bad_request("items 不能为空：计划至少要有 1 行物料"));
    }
    if items.len() > MAX_LINES {
        return Err(bad_request(doc_lines::too_many_lines(MAX_LINES, items.len())));
    }
    Ok(())
}

/// 把「已校验的明细行」聚成 `(material, warehouse) → 第几行`，重复即 400 并点出两行行号。
///
/// 为什么不靠库层唯一索引兜：撞 `uk_biz_pp_item_cell` 出来的是 500 + 10004，
/// 用户完全不知道是第 3 行和第 7 行重复了。
pub fn check_cells_unique(cells: &[(Uuid, Uuid)]) -> std::result::Result<(), ApiError> {
    let mut seen: std::collections::BTreeMap<(Uuid, Uuid), usize> = std::collections::BTreeMap::new();
    for (index, cell) in cells.iter().enumerate() {
        let line_no = index + 1;
        if let Some(first) = seen.get(cell) {
            return Err(bad_request(format!(
                "第 {line_no} 行与第 {first} 行是同一个「物料 × 仓库」：\
                 同一张计划里一个物料在一个仓库只能有一行（要分仓库请拆成多行）"
            )));
        }
        seen.insert(*cell, line_no);
    }
    Ok(())
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    /// ① 类型 × 月份：四条合法 / 四条非法，逐条钉住。
    #[test]
    fn month_must_match_plan_type() {
        assert_eq!(check_month_for_type("ANNUAL", None).expect("年度不带月合法"), None);
        assert_eq!(
            check_month_for_type("monthly", Some(11)).expect("月度带月合法"),
            Some(11)
        );

        for (plan_type, month) in [
            ("ANNUAL", Some(11)),
            ("MONTHLY", None),
            ("MONTHLY", Some(0)),
            ("MONTHLY", Some(13)),
        ] {
            let error = check_month_for_type(plan_type, month).expect_err("必须拒");
            assert_eq!(error.code, error_code::PARAM_INVALID, "{plan_type}/{month:?} 必须 400");
        }
    }

    /// ② 类型与年度闭集。
    #[test]
    fn type_and_year_ranges() {
        assert_eq!(check_type(" annual ").expect("容错大小写与空格"), "ANNUAL");
        assert!(check_type("WEEKLY").is_err());
        assert_eq!(check_year(2026).expect("正常年份"), 2026);
        assert!(check_year(1989).is_err(), "下界外必须拒");
        assert!(check_year(2201).is_err(), "上界外必须拒");
    }

    /// ③ 父单判据：只有「月度挂年度、同一年度、父单不是月度」才放行。
    #[test]
    fn parent_plan_must_be_annual_of_same_year() {
        assert!(
            check_parent_period("MONTHLY", 2026, Some(11), None).is_ok(),
            "不挂父单合法"
        );
        assert!(
            check_parent_period("MONTHLY", 2026, Some(11), Some(("ANNUAL", 2026, None))).is_ok(),
            "月度挂同年度年度计划合法"
        );
        let cases = [
            ("ANNUAL", 2026, None, ("ANNUAL", 2026, None)),
            ("MONTHLY", 2026, Some(11), ("MONTHLY", 2026, Some(10))),
            ("MONTHLY", 2026, Some(11), ("ANNUAL", 2025, None)),
        ];
        for (plan_type, year, month, parent) in cases {
            assert!(
                check_parent_period(plan_type, year, month, Some(parent)).is_err(),
                "{plan_type}/{year}/{month:?} 挂 {parent:?} 必须拒"
            );
        }
    }

    /// ④ 数量 > 0 / 单价 ≥ 0 / 行金额 4 位舍入。
    #[test]
    fn qty_price_guards_and_line_amount() {
        assert_eq!(check_qty(Some(dec("1.5")), "qty").expect("正数"), dec("1.5"));
        assert!(check_qty(Some(Decimal::ZERO), "qty").is_err(), "0 必须拒");
        assert!(check_qty(Some(dec("-1")), "qty").is_err(), "负数必须拒");
        assert!(check_qty(None, "qty").is_err(), "缺省必须拒");

        assert_eq!(check_price(None, "price").expect("缺省 0"), Decimal::ZERO);
        assert!(check_price(Some(dec("-0.01")), "price").is_err());

        assert_eq!(line_amount(dec("3"), dec("2.5")), dec("7.5000"));
        assert_eq!(line_amount(dec("0.3333"), dec("3")), dec("0.9999"));
    }

    /// ⑤ 「物料 × 仓库」重复必须在请求层拦住，且报文点出两行行号（不是让库层报 500）。
    #[test]
    fn duplicate_cells_are_rejected_with_line_numbers() {
        let m = Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid");
        let w1 = Uuid::parse_str("00000000-0000-0000-0000-000000000002").expect("uuid");
        let w2 = Uuid::parse_str("00000000-0000-0000-0000-000000000003").expect("uuid");

        assert!(
            check_cells_unique(&[cell_key(m, w1), cell_key(m, w2)]).is_ok(),
            "同物料换仓 = 两行合法"
        );
        let error =
            check_cells_unique(&[cell_key(m, w1), cell_key(m, w2), cell_key(m, w1)]).expect_err("同物料同仓重复必须拒");
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("第 3 行"), "报文要点出重复行：{}", error.msg);
        assert!(error.msg.contains("第 1 行"), "报文要点出首行：{}", error.msg);
    }

    /// ⑥ 名称 / 备注：trim、空串折 None、超长 400。
    #[test]
    fn name_and_remark_guards() {
        assert_eq!(
            check_name(Some("  2026 年度采购计划 ")).expect("正常"),
            "2026 年度采购计划"
        );
        assert!(check_name(Some("   ")).is_err(), "空白名必须拒");
        assert!(check_name(None).is_err());
        assert!(check_name(Some(&"长".repeat(MAX_NAME_LEN + 1))).is_err());

        assert_eq!(clean_remark(Some("  ")).expect("空备注"), None);
        assert_eq!(clean_remark(Some(" 备注 ")).expect("正常"), Some("备注".to_string()));
        assert!(clean_remark(Some(&"长".repeat(MAX_REMARK_LEN + 1))).is_err());
    }

    /// ⑦ 分页参数：默认值 / 钳制 / 非法状态与类型必须 400（不许静默忽略）。
    #[test]
    fn page_query_normalizes_and_rejects() {
        let filter = PlanPageQuery::default().filter().expect("默认合法");
        assert_eq!((filter.page, filter.page_size), (1, 20));
        assert!(filter.status.is_none() && filter.plan_type.is_none() && filter.keyword.is_none());

        let filter = PlanPageQuery {
            page: Some(0),
            page_size: Some(9999),
            status: Some(" draft ".to_string()),
            plan_type: Some("monthly".to_string()),
            plan_year: Some(2026),
            ..PlanPageQuery::default()
        }
        .filter()
        .expect("钳制后合法");
        assert_eq!((filter.page, filter.page_size), (1, 200));
        assert_eq!(filter.status.as_deref(), Some("DRAFT"));
        assert_eq!(filter.plan_type.as_deref(), Some("MONTHLY"));

        assert!(
            PlanPageQuery {
                status: Some("CLOSED".to_string()),
                ..PlanPageQuery::default()
            }
            .filter()
            .is_err()
        );
        assert!(
            PlanPageQuery {
                plan_type: Some("WEEKLY".to_string()),
                ..PlanPageQuery::default()
            }
            .filter()
            .is_err()
        );
        assert!(
            PlanPageQuery {
                plan_year: Some(1900),
                ..PlanPageQuery::default()
            }
            .filter()
            .is_err()
        );
    }

    /// ⑧ 权限码清单与库层（V0447）逐字一致；页面显形码**不在**接口码里（约定 14）。
    #[test]
    fn permission_codes_are_the_five_api_codes() {
        assert_eq!(PLAN_PERMISSIONS.len(), 5);
        for code in PLAN_PERMISSIONS {
            assert!(code.starts_with("business:erp:purchase-plan:"), "{code} 前缀不对");
            assert!(!code.ends_with(":page"), "{code} 是页面显形码，后端不许引用");
        }
        for action in ["list", "create", "update", "delete", "submit"] {
            let expected = format!("business:erp:purchase-plan:{action}");
            assert!(PLAN_PERMISSIONS.contains(&expected.as_str()), "缺 {expected}");
        }
    }

    /// ⑨ 状态 / 类型中文名兜底：未知值原样返回，不 panic、不空白。
    #[test]
    fn labels_fall_back_to_raw_values() {
        assert_eq!(status_label("DRAFT"), "草稿");
        assert_eq!(status_label("UNKNOWN"), "UNKNOWN");
        assert_eq!(plan_type_label("ANNUAL"), "年度计划");
        assert_eq!(plan_type_label("WEEKLY"), "WEEKLY");
    }
}
