//! 成本核算批次（期间结算单）契约层（成本核算 · ARCH-9 §5.2 #25–#33 · B3-1 骨架）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
//! §3.1 ⑨（`biz_cost_period` 表结构，V0373 已应用）、§4.1（状态机 7 条边）、
//! §4.8（结算事务）、§5.2 #25–#33（API 清单）、§5.3（权限码）。
//!
//! # 卡面的 `PENDING` ↔ 落库的 `SUBMITTED`
//!
//! 卡面写 `DRAFT → PENDING → APPROVED → SETTLED → CLOSED / REVERSED`；设计 §4.1 与
//! V0373 的 `ck_biz_cost_period_status` 用的是 `SUBMITTED`（已应用迁移不可改，且全仓
//! 其余审批单据都是 `SUBMITTED`）。**本模块只认 `SUBMITTED`**，不给 `PENDING` 留别名 ——
//! 留别名等于前端能发一个库里不存在的状态，报错要等到落库才发现。
//!
//! # 出参口径（与仓库其余列表页一致，不造第二套形状）
//!
//! ```text
//! 分页  { list, total, page, page_size }
//! 详情  单头 + items[]（成本计算单）+ items_summary{}（按明细现算）+ next_statuses[] + legal_edges[[from,to]]
//! 金额  Decimal.to_string()（字符串，防前端 JS number 丢精度）
//! ```
//!
//! # 为什么单头合计有「两套数」
//!
//! `biz_cost_period.total_*` 是**结算事务落库项**（设计 §4.8 ⑤）：`DRAFT` 阶段它们**就是 0**，
//! 因为那时还没结算。前端「成本核算」页在提交审批前必须能看见合计，所以详情额外给
//! [`CostPeriodSummary`]（按 `biz_cost_period_item` **现算**）。两者语义不同，不互相覆盖：
//!
//! * 列表 `total_*` = 落库值（已结算批次的审计口径）；
//! * 详情 `items_summary` = 明细现算值（半成品批次的作业口径）。
//!
//! # 批量取号的形状（`CS-yyyyMM-0001`）
//!
//! 设计 §3.1 ⑨ 列注释 + §4.11 报文例子（「批次 CS-202610-0001」）都写死**按期间月**、
//! **4 位流水**。仓库通用取号 `voucher::next_doc_no` 是「按天 + 6 位」，形状不对，所以
//! 本模块自己拼 [`format_batch_no`]（纯函数，单测覆盖）；`biz_seq` 的读写仍在 model 层
//! （与凭证取号同一张表，不另建号段表）。

use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;

// ==================== 值域（与 V0373 的 CHECK 逐字一致） ====================

/// 草稿（可反复重算、可改数、可提交、可作废）。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 已提交待审（等审批引擎推进；可撤回 / 被驳回，两者都回 `DRAFT`）。
pub const STATUS_SUBMITTED: &str = "SUBMITTED";
/// 审批通过（数据冻结，等结算）。
pub const STATUS_APPROVED: &str = "APPROVED";
/// 已结算（完工入库 + 三张凭证已生成）。
pub const STATUS_SETTLED: &str = "SETTLED";
/// 期间已结账（只读）。
pub const STATUS_CLOSED: &str = "CLOSED";
/// 作废（未结算就放弃；只读，不占唯一键）。
pub const STATUS_CANCELLED: &str = "CANCELLED";
/// 已红冲（凭证红字 + 库存回冲；只读，不占唯一键 → 同期可重建新批次）。
pub const STATUS_REVERSED: &str = "REVERSED";

/// 状态值域（`ck_biz_cost_period_status`，7 值）。
pub const STATUSES: [&str; 7] = [
    STATUS_DRAFT,
    STATUS_SUBMITTED,
    STATUS_APPROVED,
    STATUS_SETTLED,
    STATUS_CLOSED,
    STATUS_CANCELLED,
    STATUS_REVERSED,
];

/// 只有这个状态可写（设计 §4.1：`SUBMITTED` 起数据冻结）。
pub const WRITABLE_STATUSES: [&str; 1] = [STATUS_DRAFT];

/// 人工成本来源：手工台账（一期）。
pub const LABOR_SOURCE_MANUAL: &str = "MANUAL";
/// 人工成本来源：薪资单（ARCH9-2 上线后切换，切了就不再生成人工计提凭证）。
pub const LABOR_SOURCE_PAYROLL: &str = "PAYROLL";
/// 人工来源值域（`ck_biz_cost_period_labor`）。
pub const LABOR_SOURCES: [&str; 2] = [LABOR_SOURCE_MANUAL, LABOR_SOURCE_PAYROLL];

/// 作废原因最短长度（影响「整期放弃核算」，要能追溯到为什么）。
pub const CANCEL_REASON_MIN: usize = 10;
/// 红冲原因最短长度（设计 §5.2 #30：`{reason}` ≥10 字）。
pub const REVERSE_REASON_MIN: usize = 10;
/// 批次号流水位宽（`CS-yyyyMM-0001`）。
pub const BATCH_NO_WIDTH: usize = 4;

// ==================== 错误出口 ====================

/// 400：入参不合法。
pub fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409：状态 / 并发冲突。
pub fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 404：批次不存在（或已软删）。
pub fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("成本核算批次不存在或已删除（{id}）"),
    )
}

/// 409：同期间已有活跃批次（`uk_biz_cost_period` 的前置人话版本）。
pub fn duplicate_period(period_code: &str, existing: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_PERIOD_DUPLICATE,
        format!(
            "期间 {period_code} 已有活跃批次 {existing}：一个期间同时只能有一个活跃批次\
             （红冲 / 作废的批次不占位）；要重建请先红冲或作废那一批"
        ),
    )
}

/// 409：当前状态不允许这个动作（`66073`）。
pub fn status_guard(batch_no: &str, action: &str, status: &str, allowed: &[&str]) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_PERIOD_STATUS_GUARD,
        format!(
            "成本核算批次 {batch_no} 当前是「{status}」，不能{action}：只允许在 {:?} 状态下{}",
            allowed, action
        ),
    )
}

// ==================== 小工具 ====================

/// 去空白 + 空串归一为 `None`。
pub fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 解析路径 / 查询里的 uuid（400 指名是哪个字段）。
pub fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| bad_request(format!("{field} 不是合法 UUID（收到「{raw}」）")))
}

/// 解析可选 uuid。
pub fn parse_opt_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => parse_uuid(text, field).map(Some),
        None => Ok(None),
    }
}

// ==================== 批次号（纯逻辑） ====================

/// 期间 → 取号周期键：`2026-10` → `202610`。
///
/// 为什么用**期间月**而不是建单日：设计 §4.11 的报文例子写的是「批次 CS-202610-0001」
/// （期间 2026-10 的批次），运营口语也是「10 月的核算批次」。用建单日会让 11 月补建
/// 10 月批次时叫 `CS-202611-…`，月结对账时没人能一眼对上期间。
pub fn batch_period_key(period_code: &str) -> std::result::Result<String, ApiError> {
    let period = crate::business::erp::cost_labor_dto::normalize_period(Some(period_code))?;
    Ok(period.replace('-', ""))
}

/// 拼批次号：`CS-{yyyyMM}-{no:04}`（流水 4 位只补零不截断）。
///
/// 只补不截：流水超过 9999 时自然变 5 位（一个月 1 万个核算批次已远超设计容量，
/// 宁可长 1 位也不要撞号 —— 与 `serial_model::format_serial_no` 同口径）。
pub fn format_batch_no(period_key: &str, no: i64) -> String {
    format!(
        "CS-{period_key}-{no:0BATCH_NO_WIDTH$}",
        no = no,
        BATCH_NO_WIDTH = BATCH_NO_WIDTH
    )
}

// ==================== 列表 / 详情：查询与出参 ====================

/// 列表筛选（handler 归一化后的结果）。
#[derive(Debug, Clone, Default)]
pub struct CostPeriodFilter {
    /// 期间（精确，`yyyy-MM`）。
    pub period_code: Option<String>,
    /// 状态（精确，大写）。
    pub status: Option<String>,
    /// 关键字（批次号 / 期间 / 备注）。
    pub keyword: Option<String>,
    /// 页码（1 起）。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 列表查询串。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostPeriodPageQuery {
    /// 期间 `yyyy-MM`。
    pub period_code: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 关键字。
    pub keyword: Option<String>,
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

/// 列表一行。
#[derive(Debug, Clone, Serialize)]
pub struct CostPeriodListRes {
    /// 主键。
    pub id: String,
    /// 期间 `yyyy-MM`。
    pub period_code: String,
    /// 批次号 `CS-yyyyMM-0001`。
    pub batch_no: String,
    /// 状态。
    pub status: String,
    /// 人工来源。
    pub labor_source: String,
    /// 成本对象数（结算时落库）。
    pub object_count: i32,
    /// 材料合计。
    pub total_material: String,
    /// 人工合计。
    pub total_labor: String,
    /// 制造费用合计。
    pub total_overhead: String,
    /// 投入合计。
    pub total_cost: String,
    /// 完工转出合计。
    pub total_finished: String,
    /// 期末在制合计。
    pub total_wip: String,
    /// 结算时间（RFC3339）。
    pub settled_at: Option<String>,
    /// 制单人（实时 join 姓名；审计另有 `biz_status_log`）。
    pub created_by_name: Option<String>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
    /// 备注。
    pub remark: Option<String>,
}

/// 成本计算单一行（详情里的 `items[]`）。
#[derive(Debug, Clone, Serialize)]
pub struct CostSheetItemRes {
    /// 计算单主键。
    pub id: String,
    /// 成本对象。
    pub cost_object_id: String,
    /// 对象号。
    pub object_no: Option<String>,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 期初在制数量。
    pub wip_beg_qty: String,
    /// 期初在制成本。
    pub wip_beg_cost: String,
    /// 本期完工数量。
    pub completed_qty: String,
    /// 期末在制数量。
    pub wip_end_qty: String,
    /// 期末在制完工进度 %。
    pub completion_pct: String,
    /// 约当产量（材料）。
    pub equiv_material: String,
    /// 约当产量（转换成本）。
    pub equiv_conversion: String,
    /// 本期材料投入。
    pub mat_cur: String,
    /// 本期人工投入。
    pub labor_cur: String,
    /// 本期制造费用投入。
    pub oh_cur: String,
    /// 单位材料成本。
    pub unit_material: String,
    /// 单位人工成本。
    pub unit_labor: String,
    /// 单位制造费用。
    pub unit_overhead: String,
    /// 完工转出成本。
    pub finished_cost: String,
    /// 期末在制成本。
    pub wip_end_cost: String,
    /// 完工入库单价。
    pub unit_finished_cost: String,
    /// 完工入库单（结算后回写）。
    pub stock_in_id: Option<String>,
    /// 计算单状态（`CALC` / `SETTLED`）。
    pub status: String,
}

/// 明细现算合计（详情用；`DRAFT` 阶段单头 `total_*` 还是 0）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct CostPeriodSummary {
    /// 成本对象数（有计算单行的对象数）。
    pub object_count: i64,
    /// 材料合计。
    pub total_material: String,
    /// 人工合计。
    pub total_labor: String,
    /// 制造费用合计。
    pub total_overhead: String,
    /// 投入合计。
    pub total_cost: String,
    /// 完工转出合计。
    pub total_finished: String,
    /// 期末在制合计。
    pub total_wip: String,
}

/// 详情（单头 + 明细 + 可去状态）。
#[derive(Debug, Clone, Serialize)]
pub struct CostPeriodDetailRes {
    /// 主键。
    pub id: String,
    /// 机构（多组织哨兵值）。
    pub org_id: String,
    /// 期间。
    pub period_code: String,
    /// 批次号。
    pub batch_no: String,
    /// 状态。
    pub status: String,
    /// 人工来源。
    pub labor_source: String,
    /// 成本对象数（落库值）。
    pub object_count: i32,
    /// 材料合计（落库值）。
    pub total_material: String,
    /// 人工合计（落库值）。
    pub total_labor: String,
    /// 制造费用合计（落库值）。
    pub total_overhead: String,
    /// 投入合计（落库值）。
    pub total_cost: String,
    /// 完工转出合计（落库值）。
    pub total_finished: String,
    /// 期末在制合计（落库值）。
    pub total_wip: String,
    /// 审批实例（`SUBMITTED` 起必须有）。
    pub approval_instance_id: Option<String>,
    /// 人工计提凭证。
    pub labor_voucher_id: Option<String>,
    /// 制造费用凭证。
    pub overhead_voucher_id: Option<String>,
    /// 完工结转凭证。
    pub finish_voucher_id: Option<String>,
    /// 结算时间。
    pub settled_at: Option<String>,
    /// 结算人。
    pub settled_by: Option<String>,
    /// 结算人姓名。
    pub settled_by_name: Option<String>,
    /// 红冲时间。
    pub reversed_at: Option<String>,
    /// 红冲原因。
    pub reverse_reason: Option<String>,
    /// 作废原因。
    pub cancel_reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 制单人姓名。
    pub created_by_name: Option<String>,
    /// 创建时间。
    pub created_at: String,
    /// 更新时间。
    pub updated_at: String,
    /// 成本计算单。
    pub items: Vec<CostSheetItemRes>,
    /// 明细现算合计。
    pub items_summary: CostPeriodSummary,
    /// 当前态可去的状态（读**有效边**，管理端禁用某条边后这里就不出现它）。
    pub next_statuses: Vec<String>,
    /// 有效边表（与 `next_statuses` 同源）。
    pub legal_edges: Vec<(String, String)>,
}

/// 由有效边算 `next_statuses`（纯函数，单独可测）。
pub fn next_statuses_of(edges: &[(String, String)], status: &str) -> Vec<String> {
    edges
        .iter()
        .filter(|(from, _)| from == status)
        .map(|(_, to)| to.clone())
        .collect()
}

// ==================== 请求体 ====================

/// 新建批次（`DRAFT`）。设计 §5.2 没有独立建单接口 —— 创建原语由 #27 `calc`
/// 「不存在则建 `DRAFT`」使用；本卡按卡面「CRUD」把它暴露成 `POST /cost-period`，
/// 与 #27 **同一语义、同一权限码**（`cost-period:calc`），不新增权限码。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostPeriodCreateReq {
    /// 期间 `yyyy-MM`（必填）。
    pub period_code: Option<String>,
    /// 人工来源（缺省 `MANUAL`）。
    pub labor_source: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// calc 编排（设计 §5.2 #27：`POST /cost-period/calc`）。
///
/// 入参只有期间：六段要算什么、按什么口径算全在服务端（前端传不了对象 / 金额，
/// 否则「试算」就变成「改数」）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostPeriodCalcReq {
    /// 期间 `yyyy-MM`（必填）。批次不存在时按它建 `DRAFT`。
    pub period_code: Option<String>,
}

/// 编辑批次（只允许 `DRAFT`；只放「改数不算账」的字段）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostPeriodUpdateReq {
    /// 人工来源（`MANUAL` / `PAYROLL`，防双计提的开关）。
    pub labor_source: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 作废（设计 §5.2 #31：仅 `DRAFT`，原因必填）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostPeriodCancelReq {
    /// 作废原因（≥10 字）。
    pub reason: Option<String>,
}

/// 红冲（设计 §5.2 #30：`SETTLED` → `REVERSED`，原因必填）。
///
/// 只收 `reason`：红冲的对象、红字凭证日期（= 被红冲期间末日）、回冲数量金额
/// 全部由服务端按库内事实推导 —— 让调用方能传数量金额就等于允许「红冲一半」。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostPeriodReverseReq {
    /// 红冲原因（≥10 字；进红字凭证摘要 + 入库单作废原因 + 批次 `reverse_reason`）。
    pub reason: Option<String>,
}

// ==================== 入参归一化（纯逻辑，单测覆盖） ====================

/// 归一化人工来源（缺省 `MANUAL`；值域外 400）。
pub fn normalize_labor_source(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(LABOR_SOURCE_MANUAL.to_string());
    };
    let upper = text.to_ascii_uppercase();
    if !LABOR_SOURCES.contains(&upper.as_str()) {
        return Err(bad_request(format!(
            "labor_source 只认 {LABOR_SOURCES:?}（收到「{text}」）：\
             人工成本只能来自手工台账或薪资单，两者同时计就是双计提"
        )));
    }
    Ok(upper)
}

/// 归一化「必填原因」（长度卡在 `min`，去空白后判）。
pub fn require_reason(raw: Option<&str>, min: usize, action: &str) -> std::result::Result<String, ApiError> {
    let text = raw.map(str::trim).filter(|text| !text.is_empty()).ok_or_else(|| {
        bad_request(format!(
            "{action}必须给出原因（reason，至少 {min} 个字）：这个动作会影响已入账 / 已核算的数据，\
             原因要能追溯到「为什么」"
        ))
    })?;
    let chars = text.chars().count();
    if chars < min {
        return Err(bad_request(format!(
            "{action}的原因至少 {min} 个字（收到 {chars} 个字：「{text}」）"
        )));
    }
    Ok(text.to_string())
}

/// 备注长度上限（`remark` 是 TEXT，但界面输入框按 500 卡，超了给 400 而不是静默截断）。
pub const REMARK_MAX: usize = 500;

/// 归一化备注（超长 400）。
pub fn normalize_remark(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if text.chars().count() > REMARK_MAX {
        return Err(bad_request(format!(
            "remark 最长 {REMARK_MAX} 个字（收到 {} 个字）",
            text.chars().count()
        )));
    }
    Ok(Some(text.to_string()))
}

/// 状态中文名（报文 / 前端标签共用；与 `state_machine` 的落库值一一对应）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SUBMITTED => "已提交待审",
        STATUS_APPROVED => "审批通过",
        STATUS_SETTLED => "已结算",
        STATUS_CLOSED => "期间已结账",
        STATUS_CANCELLED => "已作废",
        STATUS_REVERSED => "已红冲",
        _ => "未知状态",
    }
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- 值域 ----------

    #[test]
    fn statuses_match_v0373_check() {
        // `ck_biz_cost_period_status` 的 7 个值，逐字一致
        assert_eq!(
            STATUSES,
            [
                "DRAFT",
                "SUBMITTED",
                "APPROVED",
                "SETTLED",
                "CLOSED",
                "CANCELLED",
                "REVERSED"
            ]
        );
        // 卡面的 PENDING **不是**本域的值（不给兼容别名）
        assert!(!STATUSES.contains(&"PENDING"));
        assert_eq!(status_label(STATUS_SUBMITTED), "已提交待审");
        assert_eq!(status_label(STATUS_REVERSED), "已红冲");
        assert_eq!(status_label("PENDING"), "未知状态");
    }

    #[test]
    fn only_draft_is_writable() {
        assert_eq!(WRITABLE_STATUSES, ["DRAFT"]);
        // 状态机侧的只读清单必须正好是「除 DRAFT 之外的 6 个」
        let locked = crate::business::shared::state_machine::COST_PERIOD_LOCKED;
        assert_eq!(locked.len(), STATUSES.len() - 1);
        for status in STATUSES {
            assert_eq!(
                locked.contains(&status),
                status != STATUS_DRAFT,
                "{status} 的只读口径与 state_machine::COST_PERIOD_LOCKED 不一致"
            );
        }
    }

    #[test]
    fn labor_sources_match_check() {
        assert_eq!(LABOR_SOURCES, ["MANUAL", "PAYROLL"]);
        assert_eq!(normalize_labor_source(None).expect("缺省"), "MANUAL");
        assert_eq!(normalize_labor_source(Some(" payroll ")).expect("归一"), "PAYROLL");
        let error = normalize_labor_source(Some("BOTH")).expect_err("双计提");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("labor_source"), "{}", error.msg);
    }

    // ---------- 批次号 ----------

    #[test]
    fn batch_no_is_cs_yyyymm_0001() {
        // 设计 §3.1 ⑨ 列注释 + §4.11 报文例子
        assert_eq!(format_batch_no("202610", 1), "CS-202610-0001");
        assert_eq!(format_batch_no("202610", 42), "CS-202610-0042");
        assert_eq!(format_batch_no("202610", 9999), "CS-202610-9999");
        // 溢出只补不截（不撞号）
        assert_eq!(format_batch_no("202610", 10000), "CS-202610-10000");
    }

    #[test]
    fn batch_period_key_strips_dash_and_validates() {
        assert_eq!(batch_period_key("2026-10").expect("合法期间"), "202610");
        assert_eq!(batch_period_key(" 2026-01 ").expect("去空白"), "202601");
        for bad in ["2026-13", "202610", "2026-1", ""] {
            let error = batch_period_key(bad).expect_err(bad);
            assert_eq!(error.status, 400, "{bad}");
        }
    }

    // ---------- 原因 / 备注 ----------

    #[test]
    fn cancel_reason_requires_ten_chars() {
        assert!(require_reason(None, CANCEL_REASON_MIN, "作废").is_err());
        assert!(require_reason(Some("   "), CANCEL_REASON_MIN, "作废").is_err());
        let error = require_reason(Some("算错了"), CANCEL_REASON_MIN, "作废").expect_err("太短");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("至少 10 个字"), "{}", error.msg);
        // 中文字符按字符数算（不是字节数）
        assert_eq!(
            require_reason(Some("  期间的产量录错了，整期重做  "), CANCEL_REASON_MIN, "作废").expect("够长"),
            "期间的产量录错了，整期重做"
        );
        assert_eq!(REVERSE_REASON_MIN, 10, "设计 §5.2 #30 写 ≥10 字");
    }

    #[test]
    fn remark_is_optional_and_capped() {
        assert_eq!(normalize_remark(None).expect("空"), None);
        assert_eq!(normalize_remark(Some("  ")).expect("空白"), None);
        assert_eq!(
            normalize_remark(Some(" 月结重算 ")).expect("正常"),
            Some("月结重算".to_string())
        );
        let long = "长".repeat(REMARK_MAX + 1);
        assert_eq!(normalize_remark(Some(&long)).expect_err("超长").status, 400);
    }

    // ---------- next_statuses ----------

    #[test]
    fn next_statuses_follow_edges() {
        let edges = vec![
            ("DRAFT".to_string(), "SUBMITTED".to_string()),
            ("DRAFT".to_string(), "CANCELLED".to_string()),
            ("SUBMITTED".to_string(), "APPROVED".to_string()),
        ];
        assert_eq!(
            next_statuses_of(&edges, "DRAFT"),
            vec!["SUBMITTED".to_string(), "CANCELLED".to_string()]
        );
        assert_eq!(next_statuses_of(&edges, "SUBMITTED"), vec!["APPROVED".to_string()]);
        assert!(next_statuses_of(&edges, "CLOSED").is_empty(), "终态没有去向");
    }

    // ---------- 错误构造 ----------

    #[test]
    fn duplicate_period_carries_its_own_code() {
        let error = duplicate_period("2026-10", "CS-202610-0001");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_PERIOD_DUPLICATE);
        assert!(error.msg.contains("2026-10"), "{}", error.msg);
        assert!(error.msg.contains("CS-202610-0001"), "{}", error.msg);
    }

    #[test]
    fn status_guard_names_allowed_statuses() {
        let error = status_guard("CS-202610-0001", "作废", "SETTLED", &["DRAFT"]);
        assert_eq!(error.code, error_code::ERP_COST_PERIOD_STATUS_GUARD);
        assert!(error.msg.contains("CS-202610-0001"), "{}", error.msg);
        assert!(error.msg.contains("SETTLED"), "{}", error.msg);
        assert!(error.msg.contains("DRAFT"), "{}", error.msg);
    }

    #[test]
    fn parse_uuid_names_the_field() {
        let error = parse_uuid("not-a-uuid", "id").expect_err("非法");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("id"), "{}", error.msg);
        assert!(parse_opt_uuid(None, "id").expect("空").is_none());
        assert!(parse_opt_uuid(Some("  "), "id").expect("空白").is_none());
    }
}
