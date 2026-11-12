//! 薪资单入参 / 回执契约（`biz_salary_sheet` + `biz_salary_detail` · HR-36）。
//!
//! 设计 §5.2 的四条接口形状全在这里（`/api` 前缀由框架注入）：
//!
//! | # | 方法 | 路径 | 权限码 | 入参 | 回执 |
//! |:--:|---|---|---|---|---|
//! | 39 | POST | `/business/hr/salary/calc` | `business:hr:salary:calc` | [`SalarySheetCalcReq`] | [`SalaryCalcRes`] |
//! | 40 | POST | `/business/hr/salary` | `business:hr:salary:calc` | [`SalarySheetCalcReq`] | [`SalaryCalcRes`]（带 `sheet`） |
//! | 37 | GET | `/business/hr/salary/{id}` | `business:hr:salary:detail` | — | [`SalarySheetSummaryRes`] |
//! | 38 | GET | `/business/hr/salary/{id}/detail/page` | `business:hr:salary:detail` | [`SalaryDetailPageQuery`] | `PageData<Vec<SalaryDetailRes>>` |
//!
//! # 试算与建单**同一套入参**，但**两个接口**
//!
//! 设计 §5.2 #39 的入参列写着 `dry_run`，容易被理解成「一个接口两个模式」。本卡拆成两条
//! 路由并在 [`parse_calc_req`] 里**互相拒绝**对方的 `dry_run` 值：
//!
//! * `POST /salary/calc` 传 `dry_run:false` → 400，报文指向 `POST /business/hr/salary`；
//! * `POST /salary` 传 `dry_run:true` → 400，报文指向 `POST /business/hr/salary/calc`。
//!
//! 理由：一个 `dry_run` 布尔决定「落不落库」，前端漏传 / 传错就是**静默建出一张真单**（或
//! 「以为建了其实没建」），而建单会占掉 `uk_biz_sal_sheet_period`（同期间第二张直接 409）。
//! 让接口名表达意图，布尔只做**一致性校验**而不是模式开关 —— 与 HR-34 的
//! `CalcRequest::preview` / `CalcRequest::persist` 两个构造函数同一取向。
//!
//! # 「汇总与明细一致」的口径（本卡验收项）
//!
//! [`SalarySheetSummaryRes`] 同时给**两份合计**：
//!
//! | 字段 | 来源 | 意义 |
//! |---|---|---|
//! | `totals` | `biz_salary_sheet` 的 7 个金额列 | 单头落库值（凭证与列表都读它） |
//! | `detail_totals` | `SUM(biz_salary_detail)` 实时算 | 明细口径（工资表逐行加出来） |
//! | `item_totals` | `SUM(biz_salary_detail_item) GROUP BY item_type` | 明细项口径（公式解释层） |
//! | `consistent` | 三者对拍 | `false` = 有 [`SalaryMismatchRes`]，逐字段点名 |
//!
//! 三份口径的字段对照写在 [`diff_totals`] / [`diff_item_totals`] 里，**纯函数**，单测把
//! 「相等 → 空」「改一个字段 → 恰好一条且点名该字段」钉住。这不是重复计算：库层 CHECK
//! 只保证单头内部与明细行内部自洽，**跨表**一致（A6/A7）没有任何约束能兜，
//! 只能每次读时对拍（HR-45 的巡检脚本是同一口径的异步版本）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::hr::salary_compute::{self as compute, CalcOutcome, CalcTotals, Payslip};
use crate::business::hr::salary_item_dto::{CALC_TYPES, ITEM_TYPES, label_of};
use crate::shared::error_code;

/// 列表默认每页条数（与薪资项配置页同口径）。
pub const DEFAULT_PAGE_SIZE: u64 = 20;

/// 列表最大每页条数（工资表明细一人一行，200 人一页已够看，再多前端也滚不动）。
pub const MAX_PAGE_SIZE: u64 = 200;

/// 单头备注上限（`biz_salary_sheet.remark` 是 `TEXT`，但界面上只放得下这点；超了截断不如报错）。
pub const MAX_REMARK_LEN: usize = 500;

// ==================== 状态词表（`ck_biz_sal_sheet_status`） ====================

/// 草稿（建单第一笔状态，落库后立即被 `CALCULATED` 覆盖）。
pub const STATUS_DRAFT: &str = "DRAFT";

/// 已计算（一期建单的终态；提交审批由 HR-42 推进）。
pub const STATUS_CALCULATED: &str = "CALCULATED";

/// 审批中。
pub const STATUS_SUBMITTED: &str = "SUBMITTED";

/// 审批通过。
pub const STATUS_APPROVED: &str = "APPROVED";

/// 已过账。
pub const STATUS_POSTED: &str = "POSTED";

/// 已红冲。
pub const STATUS_REVERSED: &str = "REVERSED";

/// 六档状态 + 中文名（顺序 = `ck_biz_sal_sheet_status`）。
pub const STATUSES: [(&str, &str); 6] = [
    (STATUS_DRAFT, "草稿"),
    (STATUS_CALCULATED, "已计算"),
    (STATUS_SUBMITTED, "审批中"),
    (STATUS_APPROVED, "已批准"),
    (STATUS_POSTED, "已过账"),
    (STATUS_REVERSED, "已红冲"),
];

/// 状态码 → 中文名（未知码回退成码本身：前端宁可看到 `FOO` 也不要空白）。
pub fn status_label(status: &str) -> String {
    let label = label_of(&STATUSES, status);
    if label == status { status.to_string() } else { label }
}

// ==================== 入参 ====================

/// 一条人工录入项（社保 / 公积金 / 个税 / 绩效 / 补发…）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SalaryManualReq {
    /// 员工 id（必填）。
    pub employee_id: Option<String>,
    /// 薪资项编码（必须是启用中的 `INPUT` 项，由引擎校验）。
    pub item_code: Option<String>,
    /// 金额（可负：补发 / 冲回，见设计 §4.8.5）。
    pub amount: Option<Decimal>,
}

/// 试算 / 建单入参（设计 §5.2 #39 / #40 共用形状）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SalarySheetCalcReq {
    /// 薪资期间 `yyyy-MM`（必填）。
    pub period_code: Option<String>,
    /// 数据来源考勤期间 id（`None` = 按 `period_code` 找当期期间）。
    pub attendance_period_id: Option<String>,
    /// 是否只试算。**只做一致性校验**：必须与本接口身份一致（见模块文档）。
    pub dry_run: Option<bool>,
    /// 单头备注。
    pub remark: Option<String>,
    /// 人工录入项。
    pub manual: Option<Vec<SalaryManualReq>>,
}

/// 接口身份：试算 / 建单。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalcMode {
    /// `POST /business/hr/salary/calc`：只算不落库。
    Preview,
    /// `POST /business/hr/salary`：算完落库。
    Persist,
}

impl CalcMode {
    /// 该模式对应的 `dry_run` 值。
    pub fn dry_run(self) -> bool {
        matches!(self, Self::Preview)
    }

    /// 该模式的接口路径（非法 `dry_run` 的报文里指向「应该用哪一条」）。
    pub fn endpoint(self) -> &'static str {
        match self {
            Self::Preview => "POST /business/hr/salary/calc（试算，不落库）",
            Self::Persist => "POST /business/hr/salary（建单并落库）",
        }
    }

    /// 该模式的中文名。
    pub fn name(self) -> &'static str {
        match self {
            Self::Preview => "试算",
            Self::Persist => "建单",
        }
    }
}

/// 400 参数错误。
fn param_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 解析 UUID 文本（`salary_item_dto::parse_uuid` 的薄封装，报错文案带字段名）。
pub fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    crate::business::hr::salary_item_dto::parse_uuid(raw, field)
}

/// 查询串里的可选 UUID（空串 / 全空白 = 未传）。
fn optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => parse_uuid(text, field).map(Some),
    }
}

/// 入参 → 引擎请求。
///
/// # 校验顺序（先形状后业务，报错一次说清一件事）
///
/// 1. `period_code` 必填且是 `yyyy-MM`（借 [`compute::period_range`]，与引擎同一把尺子）；
/// 2. `attendance_period_id` 合法 UUID；
/// 3. `remark` ≤ 500 字符；
/// 4. `dry_run` 必须与本接口身份一致（见模块文档）；
/// 5. 每条人工项：员工 UUID + 非空 `item_code` + 金额必填（金额可为负）。
///
/// # 异常情况
///
/// 任一条不满足 → 400 `10001`，报文点名字段下标（`manual[2].amount 必填`）。
pub fn parse_calc_req(req: &SalarySheetCalcReq, mode: CalcMode) -> std::result::Result<compute::CalcRequest, ApiError> {
    let period_code = req
        .period_code
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| param_invalid("period_code 必填（薪资期间，形如 `2026-11`）"))?
        .to_string();
    // 期间码格式与「这个月有多少天」都由引擎回答：两处各写一份正则迟早漂移。
    compute::period_range(&period_code)?;

    let attendance_period_id = optional_uuid(req.attendance_period_id.as_deref(), "attendance_period_id")?;

    let remark = match req.remark.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        None => None,
        Some(text) => {
            if text.chars().count() > MAX_REMARK_LEN {
                return Err(param_invalid(format!(
                    "remark 最多 {MAX_REMARK_LEN} 个字符（收到 {} 个）",
                    text.chars().count()
                )));
            }
            Some(text.to_string())
        }
    };

    if let Some(flag) = req.dry_run
        && flag != mode.dry_run()
    {
        return Err(param_invalid(format!(
            "dry_run={flag} 与「{}」接口不符：{}请用 {}",
            mode.name(),
            mode.name(),
            match mode {
                CalcMode::Preview => CalcMode::Persist.endpoint(),
                CalcMode::Persist => CalcMode::Preview.endpoint(),
            }
        )));
    }

    let mut manual = Vec::with_capacity(req.manual.as_ref().map_or(0, Vec::len));
    for (index, entry) in req.manual.iter().flatten().enumerate() {
        let raw_id = entry
            .employee_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| param_invalid(format!("manual[{index}].employee_id 必填")))?;
        let employee_id = parse_uuid(raw_id, &format!("manual[{index}].employee_id"))?;
        let item_code = entry
            .item_code
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| param_invalid(format!("manual[{index}].item_code 必填")))?;
        let amount = entry
            .amount
            .ok_or_else(|| param_invalid(format!("manual[{index}].amount 必填（允许 0 与负数）")))?;
        manual.push(compute::ManualEntry {
            employee_id,
            item_code: item_code.to_string(),
            amount,
        });
    }

    Ok(compute::CalcRequest {
        period_code,
        attendance_period_id,
        dry_run: mode.dry_run(),
        manual,
        remark,
    })
}

// ==================== 明细分页入参 ====================

/// 明细分页查询串（设计 §5.2 #38）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SalaryDetailPageQuery {
    /// 页码（从 1 起）。
    pub page: Option<u64>,
    /// 每页条数（1~200）。
    pub page_size: Option<u64>,
    /// 按部门筛（`biz_salary_detail.dept_id`）。
    pub dept_id: Option<String>,
    /// 工号 / 姓名模糊搜。
    pub keyword: Option<String>,
    /// 是否展开 `detail_item`（默认 `true`：工资表要「为什么扣这么多」的答案）。
    pub with_items: Option<bool>,
}

/// 校验通过的明细分页筛选条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalaryDetailFilter {
    /// 单头 id。
    pub sheet_id: Uuid,
    /// 部门 id。
    pub dept_id: Option<Uuid>,
    /// 关键字（已 trim）。
    pub keyword: Option<String>,
    /// 是否带明细项。
    pub with_items: bool,
}

/// 页码规整（`page ≥ 1`、`1 ≤ page_size ≤ 200`）。
pub fn normalize_page(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    (
        page.unwrap_or(1).max(1),
        page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE),
    )
}

/// 查询串 → 筛选条件。
pub fn parse_detail_query(
    sheet_id: Uuid,
    query: &SalaryDetailPageQuery,
) -> std::result::Result<SalaryDetailFilter, ApiError> {
    let dept_id = optional_uuid(query.dept_id.as_deref(), "dept_id")?;
    let keyword = query
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    Ok(SalaryDetailFilter {
        sheet_id,
        dept_id,
        keyword,
        with_items: query.with_items.unwrap_or(true),
    })
}

// ==================== 回执 ====================

/// 单头合计（= `biz_salary_sheet` 的 7 个金额列 + 人数 + 计提总额）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct SalaryTotalsRes {
    /// 人数。
    pub employee_count: i32,
    /// Σ EARNING。
    pub earning_total: Decimal,
    /// Σ PRE_DEDUCT。
    pub pre_deduct_total: Decimal,
    /// 应发合计。
    pub gross_total: Decimal,
    /// 代扣合计。
    pub withhold_total: Decimal,
    /// 实发合计。
    pub net_total: Decimal,
    /// 单位社保合计。
    pub si_company_total: Decimal,
    /// 单位公积金合计。
    pub hf_company_total: Decimal,
    /// 计提总额 = 应发 + 单位社保 + 单位公积金（HR-39 过账用它对计提凭证借方）。
    pub accrual_total: Decimal,
}

impl SalaryTotalsRes {
    /// 由引擎合计构造（建单 / 试算回执）。
    pub fn of(totals: &CalcTotals) -> Self {
        Self {
            employee_count: totals.employee_count,
            earning_total: totals.earning_total,
            pre_deduct_total: totals.pre_deduct_total,
            gross_total: totals.gross_total,
            withhold_total: totals.withhold_total,
            net_total: totals.net_total,
            si_company_total: totals.si_company_total,
            hf_company_total: totals.hf_company_total,
            accrual_total: totals.accrual_total(),
        }
    }

    /// 由明细行实算的合计构造（对账口径；计提总额自己加）。
    pub fn of_details(totals: &CalcTotals) -> Self {
        Self::of(totals)
    }
}

/// 一条薪资明细项（公式解释层）。
#[derive(Debug, Clone, Serialize)]
pub struct SalaryItemLineRes {
    /// 明细项主键（试算回执里没有行 id → `None`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// 薪资项编码。
    pub item_code: String,
    /// 薪资项名称（快照）。
    pub item_name: String,
    /// 项类型码值。
    pub item_type: String,
    /// 项类型中文名。
    pub item_type_label: String,
    /// 算法码值。
    pub calc_type: String,
    /// 算法中文名。
    pub calc_type_label: String,
    /// 金额（可负）。
    pub amount: Decimal,
    /// 计算时的公式原文（快照：改公式不改历史）。
    pub expr_snapshot: Option<String>,
    /// 代入值说明。
    pub calc_note: String,
    /// 行号（拓扑序）。
    pub line_no: i32,
}

impl SalaryItemLineRes {
    /// 由引擎的逐项结果构造。
    pub fn of_item(item: &compute::PayslipItem) -> Self {
        Self {
            id: None,
            item_code: item.item_code.clone(),
            item_type_label: label_of(&ITEM_TYPES, &item.item_type),
            calc_type_label: label_of(&CALC_TYPES, &item.calc_type),
            item_name: item.item_name.clone(),
            item_type: item.item_type.clone(),
            calc_type: item.calc_type.clone(),
            amount: item.amount,
            expr_snapshot: item.expr_snapshot.clone(),
            calc_note: item.calc_note.clone(),
            line_no: item.line_no,
        }
    }
}

/// 一个人的工资条（试算 / 建单回执里的逐人逐项结果）。
#[derive(Debug, Clone, Serialize)]
pub struct SalarySlipRes {
    /// 员工 id。
    pub employee_id: String,
    /// 工号（快照）。
    pub employee_no: String,
    /// 姓名（快照）。
    pub employee_name: String,
    /// 部门 id。
    pub dept_id: Option<String>,
    /// 部门名（快照）。
    pub dept_name: Option<String>,
    /// 关联账号（SELF 数据权限锚点）。
    pub user_id: Option<String>,
    /// 岗位（快照）。
    pub position_code: Option<String>,
    /// 职级（快照）。
    pub job_level: Option<String>,
    /// 应出勤天数。
    pub should_days: Decimal,
    /// 实际出勤天数。
    pub actual_days: Decimal,
    /// 无薪假天数。
    pub leave_unpaid_days: Decimal,
    /// 加班小时（全部类型合计）。
    pub ot_hours: Decimal,
    /// Σ 收入。
    pub earning_amount: Decimal,
    /// Σ 应发前扣减。
    pub pre_deduct_amount: Decimal,
    /// 应发。
    pub gross_amount: Decimal,
    /// 社保（个人）。
    pub social_personal: Decimal,
    /// 公积金（个人）。
    pub fund_personal: Decimal,
    /// 个税。
    pub iit_amount: Decimal,
    /// 其他代扣。
    pub other_withhold: Decimal,
    /// 代扣合计。
    pub withhold_amount: Decimal,
    /// 实发。
    pub net_amount: Decimal,
    /// 单位社保。
    pub si_company: Decimal,
    /// 单位公积金。
    pub hf_company: Decimal,
    /// 计提额 = 应发 + 单位社保 + 单位公积金。
    pub accrual_amount: Decimal,
    /// 备注（无考勤记录时写明）。
    pub remark: Option<String>,
    /// 逐项结果（不含 `SUMMARY`）。
    pub items: Vec<SalaryItemLineRes>,
}

impl SalarySlipRes {
    /// 由引擎工资条构造。
    pub fn of_slip(slip: &Payslip) -> Self {
        Self {
            employee_id: slip.head.id.to_string(),
            employee_no: slip.head.emp_no.clone(),
            employee_name: slip.head.name.clone(),
            dept_id: slip.head.dept_id.map(|id| id.to_string()),
            dept_name: slip.head.dept_name.clone(),
            user_id: slip.head.user_id.map(|id| id.to_string()),
            position_code: slip.head.position_code.clone(),
            job_level: slip.head.job_level.clone(),
            should_days: slip.attendance.should_days,
            actual_days: slip.attendance.actual_days,
            leave_unpaid_days: slip.attendance.leave_unpaid_days,
            ot_hours: slip.attendance.ot_hours_total(),
            earning_amount: slip.earning_amount,
            pre_deduct_amount: slip.pre_deduct_amount,
            gross_amount: slip.gross_amount,
            social_personal: slip.social_personal,
            fund_personal: slip.fund_personal,
            iit_amount: slip.iit_amount,
            other_withhold: slip.other_withhold,
            withhold_amount: slip.withhold_amount,
            net_amount: slip.net_amount,
            si_company: slip.si_company,
            hf_company: slip.hf_company,
            accrual_amount: slip.accrual_amount(),
            remark: slip.remark.clone(),
            items: slip.items.iter().map(SalaryItemLineRes::of_item).collect(),
        }
    }
}

/// 落库后的单头引用。
#[derive(Debug, Clone, Serialize)]
pub struct SalarySheetRefRes {
    /// 单头 id。
    pub id: String,
    /// 单号（`SAL-yyyyMM-NNNNNN`）。
    pub sheet_no: String,
    /// 状态码值。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
}

/// 试算 / 建单回执（设计 §5.2 #39 / #40）。
#[derive(Debug, Clone, Serialize)]
pub struct SalaryCalcRes {
    /// 薪资期间 `yyyy-MM`。
    pub period_code: String,
    /// 期间起。
    pub period_start: NaiveDate,
    /// 期间止。
    pub period_end: NaiveDate,
    /// 数据来源考勤期间（解析后的值）。
    pub attendance_period_id: Option<String>,
    /// `true` = 只试算（库里零行）。
    pub dry_run: bool,
    /// 落库的单头（试算为 `None`）。
    pub sheet: Option<SalarySheetRefRes>,
    /// 合计。
    pub totals: SalaryTotalsRes,
    /// 参与计算的薪资项个数。
    pub item_count: usize,
    /// 进了表但没有当期考勤记录的员工（`工号 姓名`）。
    pub missing_attendance: Vec<String>,
    /// 逐人逐项结果（按工号升序）。
    pub employees: Vec<SalarySlipRes>,
}

impl SalaryCalcRes {
    /// 由引擎结果构造。
    pub fn of(outcome: &CalcOutcome) -> Self {
        Self {
            period_code: outcome.period_code.clone(),
            period_start: outcome.period_start,
            period_end: outcome.period_end,
            attendance_period_id: outcome.attendance_period_id.map(|id| id.to_string()),
            dry_run: outcome.dry_run,
            sheet: outcome.sheet.as_ref().map(|sheet| SalarySheetRefRes {
                id: sheet.id.to_string(),
                sheet_no: sheet.sheet_no.clone(),
                status_label: status_label(&sheet.status),
                status: sheet.status.clone(),
            }),
            totals: SalaryTotalsRes::of(&outcome.totals),
            item_count: outcome.item_count,
            missing_attendance: outcome.missing_attendance.clone(),
            employees: outcome.slips.iter().map(SalarySlipRes::of_slip).collect(),
        }
    }
}

/// 单头回执（`biz_salary_sheet` 全列，凭证 / 时间戳齐）。
#[derive(Debug, Clone, Serialize)]
pub struct SalarySheetHeaderRes {
    /// 单头 id。
    pub id: String,
    /// 单号。
    pub sheet_no: String,
    /// 薪资期间。
    pub period_code: String,
    /// 期间起。
    pub period_start: NaiveDate,
    /// 期间止。
    pub period_end: NaiveDate,
    /// 状态码值。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 数据来源考勤期间 id。
    pub attendance_period_id: Option<String>,
    /// 单头合计（落库值）。
    pub totals: SalaryTotalsRes,
    /// 计提凭证 id。
    pub accrual_voucher_id: Option<String>,
    /// 计提凭证号。
    pub accrual_voucher_no: Option<String>,
    /// 发放凭证 id。
    pub payment_voucher_id: Option<String>,
    /// 发放凭证号。
    pub payment_voucher_no: Option<String>,
    /// 红冲凭证 id。
    pub reversal_voucher_id: Option<String>,
    /// 审批实例 id。
    pub approval_instance_id: Option<String>,
    /// 计算时间。
    pub calculated_at: Option<DateTime<Utc>>,
    /// 计算人。
    pub calculated_by: Option<String>,
    /// 提交时间。
    pub submitted_at: Option<DateTime<Utc>>,
    /// 过账时间。
    pub posted_at: Option<DateTime<Utc>>,
    /// 红冲时间。
    pub reversed_at: Option<DateTime<Utc>>,
    /// 红冲原因。
    pub reverse_reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: Option<DateTime<Utc>>,
    /// 更新时间。
    pub updated_at: Option<DateTime<Utc>>,
}

/// 部门汇总行（工资表按部门分组，设计 §5.2 #37「单头 + 部门汇总」）。
#[derive(Debug, Clone, Serialize)]
pub struct SalaryDeptSummaryRes {
    /// 部门 id。
    pub dept_id: Option<String>,
    /// 部门名（快照；空部门回退成 `(未分配部门)`）。
    pub dept_name: String,
    /// 人数。
    pub employee_count: i32,
    /// Σ 收入。
    pub earning_total: Decimal,
    /// Σ 应发前扣减。
    pub pre_deduct_total: Decimal,
    /// 应发合计。
    pub gross_total: Decimal,
    /// 代扣合计。
    pub withhold_total: Decimal,
    /// 实发合计。
    pub net_total: Decimal,
    /// 单位社保合计。
    pub si_company_total: Decimal,
    /// 单位公积金合计。
    pub hf_company_total: Decimal,
    /// 计提合计。
    pub accrual_total: Decimal,
}

/// 明细项按类型的合计（A7 对账口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct SalaryItemTypeTotalsRes {
    /// Σ `EARNING`。
    pub earning_total: Decimal,
    /// Σ `PRE_DEDUCT`。
    pub pre_deduct_total: Decimal,
    /// Σ `WITHHOLD`。
    pub withhold_total: Decimal,
    /// Σ `EMPLOYER`（= 单位社保 + 单位公积金）。
    pub employer_total: Decimal,
    /// 明细项行数。
    pub line_count: i64,
}

/// 一条对账差异（`consistent = false` 时逐字段点名）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SalaryMismatchRes {
    /// 字段名（机器读）。
    pub field: String,
    /// 中文名（人读）。
    pub label: String,
    /// 单头 / 明细行口径的值。
    pub header: String,
    /// 明细 / 明细项口径的值。
    pub detail: String,
}

/// 单头汇总回执（设计 §5.2 #37 的「单头 + 部门汇总」，本卡不内嵌明细分页）。
#[derive(Debug, Clone, Serialize)]
pub struct SalarySheetSummaryRes {
    /// 单头全列。
    pub sheet: SalarySheetHeaderRes,
    /// 明细行实算的合计。
    pub detail_totals: SalaryTotalsRes,
    /// 明细项按类型实算的合计。
    pub item_totals: SalaryItemTypeTotalsRes,
    /// 三份口径是否一致。
    pub consistent: bool,
    /// 不一致的字段（`consistent = true` 时为空）。
    pub mismatches: Vec<SalaryMismatchRes>,
    /// 部门汇总。
    pub dept_summary: Vec<SalaryDeptSummaryRes>,
}

// ==================== 单头列表入参 / 回执 ====================

/// 薪资单列表查询串（设计 §5.2 #36）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SalarySheetListQuery {
    /// 薪资期间 `yyyy-MM`。
    pub period_code: Option<String>,
    /// 状态码值（六档之一）。
    pub status: Option<String>,
    /// 单号模糊搜。
    pub keyword: Option<String>,
    /// 页码（从 1 起）。
    pub page: Option<u64>,
    /// 每页条数（1~200）。
    pub page_size: Option<u64>,
}

/// 校验通过的列表筛选条件。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SalarySheetFilter {
    /// 薪资期间。
    pub period_code: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 单号关键字。
    pub keyword: Option<String>,
}

/// 查询串 → 筛选条件（域值非法 400，**不静默忽略**：状态拼错时宁可见红也不给「筛完还是全量」）。
pub fn parse_list_query(query: &SalarySheetListQuery) -> std::result::Result<SalarySheetFilter, ApiError> {
    let period_code = match query.period_code.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        None => None,
        Some(text) => {
            // 期间码格式与引擎同一把尺子（`2026-13` 这类必须在筛选层就被拒）
            compute::period_range(text)?;
            Some(text.to_string())
        }
    };
    let status = match query.status.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        None => None,
        Some(value) => {
            if !STATUSES.iter().any(|(code, _)| *code == value) {
                return Err(param_invalid(format!(
                    "status `{value}` 不合法：只能是 {}",
                    STATUSES
                        .iter()
                        .map(|(code, _)| format!("`{code}`"))
                        .collect::<Vec<_>>()
                        .join(" / ")
                )));
            }
            Some(value.to_string())
        }
    };
    let keyword = query
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    Ok(SalarySheetFilter {
        period_code,
        status,
        keyword,
    })
}

/// 列表卡片（一单一行的摘要；点开看 [`SalarySheetSummaryRes`]）。
#[derive(Debug, Clone, Serialize)]
pub struct SalarySheetCardRes {
    /// 单头 id。
    pub id: String,
    /// 单号。
    pub sheet_no: String,
    /// 薪资期间。
    pub period_code: String,
    /// 期间起。
    pub period_start: NaiveDate,
    /// 期间止。
    pub period_end: NaiveDate,
    /// 状态码值。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 人数。
    pub employee_count: i32,
    /// 应发合计。
    pub gross_total: Decimal,
    /// 代扣合计。
    pub withhold_total: Decimal,
    /// 实发合计。
    pub net_total: Decimal,
    /// 计提合计。
    pub accrual_total: Decimal,
    /// 计提凭证号。
    pub accrual_voucher_no: Option<String>,
    /// 发放凭证号。
    pub payment_voucher_no: Option<String>,
    /// 过账时间。
    pub posted_at: Option<DateTime<Utc>>,
    /// 备注。
    pub remark: Option<String>,
}

// ==================== 明细回执 ====================

/// 一条薪资明细（一人一行）。
#[derive(Debug, Clone, Serialize)]
pub struct SalaryDetailRes {
    /// 明细 id。
    pub id: String,
    /// 单头 id。
    pub sheet_id: String,
    /// 员工 id。
    pub employee_id: String,
    /// 工号（快照）。
    pub employee_no: String,
    /// 姓名（快照）。
    pub employee_name: String,
    /// 部门 id。
    pub dept_id: Option<String>,
    /// 部门名（快照）。
    pub dept_name: Option<String>,
    /// 关联账号。
    pub user_id: Option<String>,
    /// 岗位（快照）。
    pub position_code: Option<String>,
    /// 职级（快照）。
    pub job_level: Option<String>,
    /// 应出勤天数。
    pub should_days: Decimal,
    /// 实际出勤天数。
    pub actual_days: Decimal,
    /// 无薪假天数。
    pub leave_unpaid_days: Decimal,
    /// 加班小时。
    pub ot_hours: Decimal,
    /// Σ 收入。
    pub earning_amount: Decimal,
    /// Σ 应发前扣减。
    pub pre_deduct_amount: Decimal,
    /// 应发。
    pub gross_amount: Decimal,
    /// 社保（个人）。
    pub social_personal: Decimal,
    /// 公积金（个人）。
    pub fund_personal: Decimal,
    /// 个税。
    pub iit_amount: Decimal,
    /// 其他代扣。
    pub other_withhold: Decimal,
    /// 代扣合计。
    pub withhold_amount: Decimal,
    /// 实发。
    pub net_amount: Decimal,
    /// 单位社保。
    pub si_company: Decimal,
    /// 单位公积金。
    pub hf_company: Decimal,
    /// 计提额。
    pub accrual_amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 明细项（`with_items=false` 时为空数组，`item_count` 仍是真实条数）。
    pub items: Vec<SalaryItemLineRes>,
    /// 明细项条数（即使不展开也给，前端才能显示「15 项」）。
    pub item_count: i32,
}

// ==================== 对账（纯函数，单测钉住） ====================

/// 单头 vs 明细行的 8 个字段对照（A6）。
///
/// 逐字段而不是逐总额：只比 `net_total` 时，「应发少 100、代扣多 100」这种**恰好抵消**的
/// 脏数据会被放过；而这类漂移正是「有人绕过引擎改了一张单」的典型痕迹。
pub fn diff_totals(header: &SalaryTotalsRes, detail: &SalaryTotalsRes) -> Vec<SalaryMismatchRes> {
    let fields: [(&str, &str, Decimal, Decimal); 7] = [
        ("earning_total", "收入合计", header.earning_total, detail.earning_total),
        (
            "pre_deduct_total",
            "应发前扣减合计",
            header.pre_deduct_total,
            detail.pre_deduct_total,
        ),
        ("gross_total", "应发合计", header.gross_total, detail.gross_total),
        (
            "withhold_total",
            "代扣合计",
            header.withhold_total,
            detail.withhold_total,
        ),
        ("net_total", "实发合计", header.net_total, detail.net_total),
        (
            "si_company_total",
            "单位社保合计",
            header.si_company_total,
            detail.si_company_total,
        ),
        (
            "hf_company_total",
            "单位公积金合计",
            header.hf_company_total,
            detail.hf_company_total,
        ),
    ];
    let mut out: Vec<SalaryMismatchRes> = fields
        .into_iter()
        .filter(|(_, _, head, row)| head != row)
        .map(|(field, label, head, row)| SalaryMismatchRes {
            field: field.to_string(),
            label: label.to_string(),
            header: head.to_string(),
            detail: row.to_string(),
        })
        .collect();
    if header.employee_count != detail.employee_count {
        out.push(SalaryMismatchRes {
            field: "employee_count".to_string(),
            label: "人数".to_string(),
            header: header.employee_count.to_string(),
            detail: detail.employee_count.to_string(),
        });
    }
    out
}

/// 明细行 vs 明细项（A7）：按类型聚合的四个桶必须相等。
///
/// `EMPLOYER` 只对**总额**：`si_company` / `hf_company` 的拆分靠薪资项贷方科目
/// （`2211.02` / `2211.03`），明细项行里没有科目列，硬拆要再 join 回配置表 —— 那是 HR-39
/// 过账的活，这里只验「单位承担总额没丢」。
pub fn diff_item_totals(detail: &SalaryTotalsRes, items: &SalaryItemTypeTotalsRes) -> Vec<SalaryMismatchRes> {
    let employer = detail.si_company_total + detail.hf_company_total;
    let fields: [(&str, &str, Decimal, Decimal); 4] = [
        (
            "item_earning_total",
            "收入项合计",
            detail.earning_total,
            items.earning_total,
        ),
        (
            "item_pre_deduct_total",
            "应发前扣减项合计",
            detail.pre_deduct_total,
            items.pre_deduct_total,
        ),
        (
            "item_withhold_total",
            "代扣项合计",
            detail.withhold_total,
            items.withhold_total,
        ),
        ("item_employer_total", "单位承担项合计", employer, items.employer_total),
    ];
    fields
        .into_iter()
        .filter(|(_, _, row, item)| row != item)
        .map(|(field, label, row, item)| SalaryMismatchRes {
            field: field.to_string(),
            label: label.to_string(),
            header: row.to_string(),
            detail: item.to_string(),
        })
        .collect()
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// `V0383` 建表原文（编译期嵌入）：状态词表 / 列宽 / CHECK 名逐条对拍。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    fn money(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量")
    }

    fn ok_req() -> SalarySheetCalcReq {
        SalarySheetCalcReq {
            period_code: Some(" 2026-11 ".to_string()),
            ..SalarySheetCalcReq::default()
        }
    }

    // ---------- ① 状态词表 ----------

    /// 六个状态值必须逐字出现在迁移的 CHECK 里（前端 `doc-kinds.ts` 与筛选下拉都靠它）。
    #[test]
    fn statuses_match_the_migration_check() {
        let line = V0383_DDL
            .lines()
            .find(|line| line.contains("ck_biz_sal_sheet_status"))
            .expect("V0383 里必有 ck_biz_sal_sheet_status");
        for (code, label) in STATUSES {
            assert!(line.contains(&format!("'{code}'")), "CHECK 缺状态 {code}：{line}");
            assert!(!label.is_empty(), "状态 {code} 缺中文名");
            assert_eq!(status_label(code), label);
        }
        // 未知码回退成码本身，不是空串
        assert_eq!(status_label("WHATEVER"), "WHATEVER");
    }

    // ---------- ② 入参校验 ----------

    /// 期间码必填，且格式由引擎那一把尺子判（`2026-13` 必须被拒）。
    #[test]
    fn period_code_is_required_and_validated() {
        let err = parse_calc_req(&SalarySheetCalcReq::default(), CalcMode::Preview).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("period_code"), "报文：{}", err.msg);

        let bad = SalarySheetCalcReq {
            period_code: Some("2026-13".to_string()),
            ..SalarySheetCalcReq::default()
        };
        let err = parse_calc_req(&bad, CalcMode::Preview).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("2026-13"), "报文：{}", err.msg);
    }

    /// 前导空白被 trim；`dry_run` 不给就按接口身份填。
    #[test]
    fn period_is_trimmed_and_dry_run_follows_mode() {
        let preview = parse_calc_req(&ok_req(), CalcMode::Preview).expect("试算入参");
        assert_eq!(preview.period_code, "2026-11");
        assert!(preview.dry_run);
        assert!(preview.attendance_period_id.is_none());
        assert!(preview.manual.is_empty());

        let persist = parse_calc_req(&ok_req(), CalcMode::Persist).expect("建单入参");
        assert!(!persist.dry_run);
    }

    /// 两条接口互相拒绝对方的 `dry_run`，且报文点名该用哪一条。
    #[test]
    fn dry_run_must_match_the_endpoint() {
        let wrong_preview = SalarySheetCalcReq {
            dry_run: Some(true),
            ..ok_req()
        };
        let err = parse_calc_req(&wrong_preview, CalcMode::Persist).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("/business/hr/salary/calc"), "报文：{}", err.msg);

        let wrong_persist = SalarySheetCalcReq {
            dry_run: Some(false),
            ..ok_req()
        };
        let err = parse_calc_req(&wrong_persist, CalcMode::Preview).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("POST /business/hr/salary（"), "报文：{}", err.msg);

        // 与身份一致时放行
        let same = SalarySheetCalcReq {
            dry_run: Some(false),
            ..ok_req()
        };
        assert!(!parse_calc_req(&same, CalcMode::Persist).unwrap().dry_run);
    }

    /// 人工项三类错都要点名下标；负数金额必须放行（补发 / 冲回）。
    #[test]
    fn manual_entries_are_checked_one_by_one() {
        let missing_amount = SalarySheetCalcReq {
            manual: Some(vec![SalaryManualReq {
                employee_id: Some(Uuid::now_v7().to_string()),
                item_code: Some("BASE".to_string()),
                amount: None,
            }]),
            ..ok_req()
        };
        let err = parse_calc_req(&missing_amount, CalcMode::Preview).unwrap_err();
        assert!(err.msg.contains("manual[0].amount"), "报文：{}", err.msg);

        let bad_uuid = SalarySheetCalcReq {
            manual: Some(vec![
                SalaryManualReq {
                    employee_id: Some(Uuid::now_v7().to_string()),
                    item_code: Some("BASE".to_string()),
                    amount: Some(money("1")),
                },
                SalaryManualReq {
                    employee_id: Some("not-a-uuid".to_string()),
                    item_code: Some("BASE".to_string()),
                    amount: Some(money("1")),
                },
            ]),
            ..ok_req()
        };
        let err = parse_calc_req(&bad_uuid, CalcMode::Preview).unwrap_err();
        assert!(err.msg.contains("manual[1].employee_id"), "报文：{}", err.msg);

        let negative = SalarySheetCalcReq {
            manual: Some(vec![SalaryManualReq {
                employee_id: Some(Uuid::now_v7().to_string()),
                item_code: Some(" POST ".to_string()),
                amount: Some(money("-500.5000")),
            }]),
            ..ok_req()
        };
        let parsed = parse_calc_req(&negative, CalcMode::Preview).expect("负数补发放行");
        assert_eq!(parsed.manual.len(), 1);
        assert_eq!(parsed.manual[0].item_code, "POST", "编码要 trim");
        assert_eq!(parsed.manual[0].amount, money("-500.5"));
    }

    /// 备注超 500 字符报错（不静默截断：截断后单头上的说明就骗人了）。
    #[test]
    fn remark_length_is_capped() {
        let long = SalarySheetCalcReq {
            remark: Some("补".repeat(MAX_REMARK_LEN + 1)),
            ..ok_req()
        };
        let err = parse_calc_req(&long, CalcMode::Preview).unwrap_err();
        assert!(err.msg.contains("remark"), "报文：{}", err.msg);

        let blank = SalarySheetCalcReq {
            remark: Some("   ".to_string()),
            ..ok_req()
        };
        assert_eq!(parse_calc_req(&blank, CalcMode::Preview).unwrap().remark, None);
    }

    // ---------- ③ 明细分页入参 ----------

    #[test]
    fn detail_query_normalizes_page_and_filters() {
        assert_eq!(normalize_page(None, None), (1, DEFAULT_PAGE_SIZE));
        assert_eq!(normalize_page(Some(0), Some(0)), (1, 1));
        assert_eq!(normalize_page(Some(3), Some(9_999)), (3, MAX_PAGE_SIZE));

        let sheet_id = Uuid::now_v7();
        let query = SalaryDetailPageQuery {
            dept_id: Some("  ".to_string()),
            keyword: Some("  张三 ".to_string()),
            with_items: None,
            ..SalaryDetailPageQuery::default()
        };
        let filter = parse_detail_query(sheet_id, &query).expect("查询串");
        assert_eq!(filter.sheet_id, sheet_id);
        assert_eq!(filter.dept_id, None, "空白部门 = 不筛");
        assert_eq!(filter.keyword.as_deref(), Some("张三"));
        assert!(filter.with_items, "默认展开明细项");

        let bad = SalaryDetailPageQuery {
            dept_id: Some("dept-1".to_string()),
            ..SalaryDetailPageQuery::default()
        };
        let err = parse_detail_query(sheet_id, &bad).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("dept_id"), "报文：{}", err.msg);
    }

    // ---------- ④ 汇总与明细一致 ----------

    fn totals(earning: &str, pre: &str, gross: &str, withhold: &str, net: &str) -> SalaryTotalsRes {
        SalaryTotalsRes {
            employee_count: 2,
            earning_total: money(earning),
            pre_deduct_total: money(pre),
            gross_total: money(gross),
            withhold_total: money(withhold),
            net_total: money(net),
            si_company_total: money("1680"),
            hf_company_total: money("960"),
            accrual_total: money(gross) + money("1680") + money("960"),
        }
    }

    /// 三份口径相等 → 零差异（这是建单后的**正常态**，必须一条都不报）。
    #[test]
    fn equal_totals_produce_no_mismatch() {
        let head = totals("9954.03", "735.63", "9218.40", "1800", "7418.40");
        let diff = diff_totals(&head, &head);
        assert!(diff.is_empty(), "相等却报了差异：{diff:?}");

        let items = SalaryItemTypeTotalsRes {
            earning_total: head.earning_total,
            pre_deduct_total: head.pre_deduct_total,
            withhold_total: head.withhold_total,
            employer_total: head.si_company_total + head.hf_company_total,
            line_count: 30,
        };
        assert!(diff_item_totals(&head, &items).is_empty());
    }

    /// 改一个字段 → **恰好一条**且点名该字段（不能只报「不一致」让人自己找）。
    #[test]
    fn one_wrong_field_is_named() {
        let head = totals("9954.03", "735.63", "9218.40", "1800", "7418.40");
        let mut detail = head;
        detail.net_total = money("7418.41");
        let diff = diff_totals(&head, &detail);
        assert_eq!(diff.len(), 1, "{diff:?}");
        assert_eq!(diff[0].field, "net_total");
        assert_eq!(diff[0].label, "实发合计");
        assert_eq!(diff[0].header, "7418.40");
        assert_eq!(diff[0].detail, "7418.41");

        // 人数也是对账项
        let mut detail = head;
        detail.employee_count = 3;
        let diff = diff_totals(&head, &detail);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].field, "employee_count");
    }

    /// 「应发少 100、代扣多 100」这种**恰好抵消**的脏数据必须被抓出来（只比实发会漏）。
    #[test]
    fn offsetting_drift_is_still_caught() {
        let head = totals("9954.03", "735.63", "9218.40", "1800", "7418.40");
        let mut detail = head;
        detail.gross_total = money("9118.40");
        detail.withhold_total = money("1700");
        let diff = diff_totals(&head, &detail);
        let fields: Vec<&str> = diff.iter().map(|row| row.field.as_str()).collect();
        assert_eq!(fields, vec!["gross_total", "withhold_total"], "{diff:?}");
        assert_eq!(detail.net_total, head.net_total, "实发相同但两份口径已经漂了");
    }

    /// 明细项口径：`EMPLOYER` 只对「单位社保 + 单位公积金」的总额。
    #[test]
    fn item_totals_compare_employer_as_one_bucket() {
        let head = totals("9954.03", "735.63", "9218.40", "1800", "7418.40");
        let items = SalaryItemTypeTotalsRes {
            earning_total: head.earning_total,
            pre_deduct_total: head.pre_deduct_total,
            withhold_total: head.withhold_total,
            employer_total: money("2640"),
            line_count: 30,
        };
        assert!(diff_item_totals(&head, &items).is_empty());

        let off = SalaryItemTypeTotalsRes {
            employer_total: money("2639.99"),
            ..items
        };
        let diff = diff_item_totals(&head, &off);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].field, "item_employer_total");
    }

    // ---------- ⑤ 回执组装 ----------

    /// 一个人的工资条 → 回执：金额直通、`id` 字符串化、明细项带中文名。
    #[test]
    fn slip_res_maps_every_field() {
        use crate::business::hr::salary_compute::{AttendanceNumbers, EmployeeHead, PayslipItem};

        let slip = Payslip {
            head: EmployeeHead {
                id: Uuid::now_v7(),
                emp_no: "HR36-1".to_string(),
                name: "张三".to_string(),
                dept_id: Some(Uuid::now_v7()),
                dept_name: Some("生产部".to_string()),
                user_id: None,
                position_code: Some("OP".to_string()),
                job_level: Some("P3".to_string()),
            },
            attendance: AttendanceNumbers {
                should_days: money("22"),
                actual_days: money("20"),
                leave_unpaid_days: money("2"),
                ot_hours_workday: money("8"),
                ..AttendanceNumbers::default()
            },
            attendance_record_id: None,
            items: vec![PayslipItem {
                item_code: "BASE".to_string(),
                item_name: "基本工资".to_string(),
                item_type: "EARNING".to_string(),
                calc_type: "FIXED".to_string(),
                amount: money("8000"),
                expr_snapshot: None,
                calc_note: "标准 8000".to_string(),
                line_no: 1,
            }],
            earning_amount: money("8000"),
            pre_deduct_amount: money("0"),
            gross_amount: money("8000"),
            social_personal: money("840"),
            fund_personal: money("960"),
            iit_amount: money("0"),
            other_withhold: money("0"),
            withhold_amount: money("1800"),
            net_amount: money("6200"),
            si_company: money("1680"),
            hf_company: money("960"),
            remark: None,
        };

        let res = SalarySlipRes::of_slip(&slip);
        assert_eq!(res.employee_no, "HR36-1");
        assert_eq!(res.dept_name.as_deref(), Some("生产部"));
        assert_eq!(res.ot_hours, money("8"), "加班小时取合计");
        assert_eq!(res.accrual_amount, money("10640"), "计提 = 8000 + 1680 + 960");
        assert_eq!(res.items.len(), 1);
        assert_eq!(res.items[0].item_type_label, "收入项");
        assert_eq!(res.items[0].calc_type_label, "取薪资标准");
        assert!(res.items[0].id.is_none(), "试算回执没有行 id");
    }

    /// 合计回执：计提总额由三个桶加出来（HR-39 过账对它）。
    #[test]
    fn totals_res_carries_accrual() {
        let res = SalaryTotalsRes::of(&CalcTotals {
            employee_count: 2,
            earning_total: money("9954.03"),
            pre_deduct_total: money("735.63"),
            gross_total: money("9218.40"),
            withhold_total: money("1800"),
            net_total: money("7418.40"),
            si_company_total: money("1680"),
            hf_company_total: money("960"),
        });
        assert_eq!(res.accrual_total, money("11858.40"));
        assert_eq!(res.gross_total, res.net_total + res.withhold_total);
    }
}
