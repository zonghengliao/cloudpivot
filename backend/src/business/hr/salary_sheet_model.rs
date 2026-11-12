//! 薪资单数据层（`biz_salary_sheet` + `biz_salary_detail` + `biz_salary_detail_item` · HR-36）。
//!
//! 本文件是这三张表**唯一**的 SQL 出口：单头读、明细分页、明细项批量读、部门汇总、
//! 三份口径的对账合计。写入路径**不在**这里 —— 建单落库那一笔事务属于引擎
//! （[`crate::business::hr::salary_compute::calc`]），本卡只负责把落库后的东西读出来，
//! 「建单」接口只做一层薄壳（解析入参 → 调引擎 → 把 `CalcOutcome` 变成回执）。
//!
//! # 表结构的唯一依据
//!
//! `backend/migrations/business/V0383__hr_tables.up.sql` 的 `biz_salary_sheet`（设计 §3.1 ⑧）、
//! `biz_salary_detail`（⑨）、`biz_salary_detail_item`（⑩）。列名 / `CHECK` 名 / 唯一索引名逐字
//! 取自该迁移；**迁移已应用且不可改**（AGENTS.md 约定 6），本卡零迁移。
//!
//! # 明细分页为什么走 `paginate_unscoped`
//!
//! 设计 D-12 把 `biz_salary_detail` 列为**范围表**（`dept_col = dept_id`、`owner_col = user_id`），
//! 但 `shared/scope.rs` 的 `SCOPED_TABLES` 里**还没有**它（登记动作落在 `/salary/my` 的卡上，
//! 见卡内汇报）。当前状态决定了本文件只能走 [`scope::paginate_unscoped`]：
//!
//! * 硬走 `scope::paginate` 会当场 `DbError::InvalidValue`（锚点没登记 = 不是范围表）；
//! * 写死 `AND 1 = 1` 又不会有任何闸门，所以锚点显式写成 [`DETAIL_TARGET`]，
//!   等 `SCOPED_TABLES` 登记后这行会**立刻编译期不通过**（`paginate_unscoped` 的守卫会在
//!   运行期报错逼你改走 `paginate`）—— 那是本卡期望的「被强行叫醒」，不是静默降级。
//!
//! 单头（`biz_salary_sheet`）按设计**不登记**范围：单头是全公司合计，`SELF` 语义不成立，
//! 可见性只由 `business:hr:salary:list` / `:detail` 权限码控制（设计 §4.7）。
//!
//! # 姓名口径（AGENTS.md 约定 13）
//!
//! 明细行的 `employee_no` / `employee_name` / `dept_name` **全部读快照列**：工资表是
//! 审计/历史类，员工改名、部门改名、甚至脱离组织之后，这张表必须还显示「当时是谁」。
//! 这也是明细表为什么要冗余 `dept_name`（`V0383` 的列注释：部门改名后工资表不漂移）。
//! 本文件**不做任何 join**，因此也没有 N+1 的机会。
//!
//! # 分页 + 明细项 = 两次查询（不是 N+1）
//!
//! 一页最多 [`MAX_PAGE_SIZE`](crate::business::hr::salary_sheet_dto::MAX_PAGE_SIZE) 人，
//! 明细项按 `IN (detail_id…)` **一次查完**再按 `detail_id` 分组 —— 绝不能「每人查一次项」：
//! 100 人的工资表就是 100 次往返，比算工资本身还慢。

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::salary_compute::CalcTotals;
use crate::business::hr::salary_sheet_dto::{
    SalaryDeptSummaryRes, SalaryDetailFilter, SalaryDetailRes, SalaryItemLineRes, SalaryItemTypeTotalsRes,
    SalarySheetCardRes, SalarySheetFilter, SalarySheetHeaderRes, SalaryTotalsRes,
};
use crate::business::shared::scope;

/// 单头表名（与 `hr::model` 的表名单同源；这里只是本文件的可读别名）。
pub const TABLE_SHEET: &str = "biz_salary_sheet";

/// 明细表名。
pub const TABLE_DETAIL: &str = "biz_salary_detail";

/// 明细项表名。
pub const TABLE_DETAIL_ITEM: &str = "biz_salary_detail_item";

/// 明细分页锚点。
///
/// `biz_salary_detail` 尚未登记进 `SCOPED_TABLES`（见模块文档），所以这里显式 `unscoped`。
/// 一旦有人把它登记进去，[`scope::paginate_unscoped`] 会返回
/// `DbError::InvalidValue`，逼调用方改用 `scope::paginate` —— 这条「被叫醒」的路径是
/// 故意的：数据权限静默失效比报错危险得多。
pub const DETAIL_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE_DETAIL);

/// 单头列表锚点：设计 §4.7 明确 `biz_salary_sheet` **不登记**数据权限（单头是全公司合计，
/// `SELF` 语义不成立），可见性只由 `business:hr:salary:list` 权限码控制。
pub const SHEET_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE_SHEET);

/// 明细项排序（与配置页的计算顺序一致：`line_no` 再 `item_code` 兜底稳定）。
const ITEM_ORDER: &str = "ORDER BY i.detail_id, i.line_no, i.item_code";

/// 明细分页排序：工号升序（工资表按工号对拍 Excel），同工号再按 id 稳定。
const DETAIL_ORDER: &str = "ORDER BY d.employee_no ASC, d.id ASC";

/// 单头的全部读取列（[`SheetRow::from_row`] 按**列名**取值，所以这份清单与它必须一致；
/// 少一列不会编译报错，只会在运行期 `ColumnNotFound` —— 单测
/// `sheet_columns_cover_every_projection` 把它钉住）。
pub const SHEET_COLUMNS: &str = "id, sheet_no, period_code, period_start, period_end, status, \
     attendance_period_id, employee_count, earning_total, pre_deduct_total, gross_total, \
     withhold_total, net_total, si_company_total, hf_company_total, accrual_voucher_id, \
     accrual_voucher_no, payment_voucher_id, payment_voucher_no, reversal_voucher_id, \
     approval_instance_id, calculated_at, calculated_by, submitted_at, posted_at, reversed_at, \
     reverse_reason, remark, created_at, updated_at";

/// 明细的全部读取列（同上：与 [`DetailRow::from_row`] 逐个对齐）。
pub const DETAIL_COLUMNS: &str = "d.id, d.sheet_id, d.employee_id, d.employee_no, d.employee_name, \
     d.dept_id, d.dept_name, d.user_id, d.position_code, d.job_level, d.should_days, d.actual_days, \
     d.leave_unpaid_days, d.ot_hours, d.earning_amount, d.pre_deduct_amount, d.gross_amount, \
     d.social_personal, d.fund_personal, d.iit_amount, d.other_withhold, d.withhold_amount, \
     d.net_amount, d.si_company, d.hf_company, d.remark";

// ==================== 行投影 ====================

/// 一行单头（`biz_salary_sheet` 全列）。
#[derive(Debug, Clone)]
pub struct SheetRow {
    /// 主键。
    pub id: Uuid,
    /// 单号。
    pub sheet_no: String,
    /// 薪资期间。
    pub period_code: String,
    /// 期间起。
    pub period_start: NaiveDate,
    /// 期间止。
    pub period_end: NaiveDate,
    /// 状态。
    pub status: String,
    /// 考勤期间 id。
    pub attendance_period_id: Option<Uuid>,
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
    /// 计提凭证 id。
    pub accrual_voucher_id: Option<Uuid>,
    /// 计提凭证号。
    pub accrual_voucher_no: Option<String>,
    /// 发放凭证 id。
    pub payment_voucher_id: Option<Uuid>,
    /// 发放凭证号。
    pub payment_voucher_no: Option<String>,
    /// 红冲凭证 id。
    pub reversal_voucher_id: Option<Uuid>,
    /// 审批实例 id。
    pub approval_instance_id: Option<Uuid>,
    /// 计算时间。
    pub calculated_at: Option<DateTime<Utc>>,
    /// 计算人。
    pub calculated_by: Option<Uuid>,
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

impl SheetRow {
    /// 由结果行投影。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            sheet_no: row.get_typed("sheet_no")?,
            period_code: row.get_typed("period_code")?,
            period_start: row.get_typed("period_start")?,
            period_end: row.get_typed("period_end")?,
            status: row.get_typed("status")?,
            attendance_period_id: row.get_typed("attendance_period_id")?,
            employee_count: row.get_typed("employee_count")?,
            earning_total: row.get_typed("earning_total")?,
            pre_deduct_total: row.get_typed("pre_deduct_total")?,
            gross_total: row.get_typed("gross_total")?,
            withhold_total: row.get_typed("withhold_total")?,
            net_total: row.get_typed("net_total")?,
            si_company_total: row.get_typed("si_company_total")?,
            hf_company_total: row.get_typed("hf_company_total")?,
            accrual_voucher_id: row.get_typed("accrual_voucher_id")?,
            accrual_voucher_no: row.get_typed("accrual_voucher_no")?,
            payment_voucher_id: row.get_typed("payment_voucher_id")?,
            payment_voucher_no: row.get_typed("payment_voucher_no")?,
            reversal_voucher_id: row.get_typed("reversal_voucher_id")?,
            approval_instance_id: row.get_typed("approval_instance_id")?,
            calculated_at: row.get_typed("calculated_at")?,
            calculated_by: row.get_typed("calculated_by")?,
            submitted_at: row.get_typed("submitted_at")?,
            posted_at: row.get_typed("posted_at")?,
            reversed_at: row.get_typed("reversed_at")?,
            reverse_reason: row.get_typed("reverse_reason")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 单头 7 个金额列 + 人数（落库值）。
    pub fn totals(&self) -> SalaryTotalsRes {
        SalaryTotalsRes::of(&CalcTotals {
            employee_count: self.employee_count,
            earning_total: self.earning_total,
            pre_deduct_total: self.pre_deduct_total,
            gross_total: self.gross_total,
            withhold_total: self.withhold_total,
            net_total: self.net_total,
            si_company_total: self.si_company_total,
            hf_company_total: self.hf_company_total,
        })
    }

    /// 列表卡片回执。
    pub fn to_card(&self) -> SalarySheetCardRes {
        let totals = self.totals();
        SalarySheetCardRes {
            id: self.id.to_string(),
            sheet_no: self.sheet_no.clone(),
            period_code: self.period_code.clone(),
            period_start: self.period_start,
            period_end: self.period_end,
            status_label: crate::business::hr::salary_sheet_dto::status_label(&self.status),
            status: self.status.clone(),
            employee_count: totals.employee_count,
            gross_total: totals.gross_total,
            withhold_total: totals.withhold_total,
            net_total: totals.net_total,
            accrual_total: totals.accrual_total,
            accrual_voucher_no: self.accrual_voucher_no.clone(),
            payment_voucher_no: self.payment_voucher_no.clone(),
            posted_at: self.posted_at,
            remark: self.remark.clone(),
        }
    }

    /// 单头回执。
    pub fn to_header(&self) -> SalarySheetHeaderRes {
        SalarySheetHeaderRes {
            id: self.id.to_string(),
            sheet_no: self.sheet_no.clone(),
            period_code: self.period_code.clone(),
            period_start: self.period_start,
            period_end: self.period_end,
            status_label: crate::business::hr::salary_sheet_dto::status_label(&self.status),
            status: self.status.clone(),
            attendance_period_id: self.attendance_period_id.map(|id| id.to_string()),
            totals: self.totals(),
            accrual_voucher_id: self.accrual_voucher_id.map(|id| id.to_string()),
            accrual_voucher_no: self.accrual_voucher_no.clone(),
            payment_voucher_id: self.payment_voucher_id.map(|id| id.to_string()),
            payment_voucher_no: self.payment_voucher_no.clone(),
            reversal_voucher_id: self.reversal_voucher_id.map(|id| id.to_string()),
            approval_instance_id: self.approval_instance_id.map(|id| id.to_string()),
            calculated_at: self.calculated_at,
            calculated_by: self.calculated_by.map(|id| id.to_string()),
            submitted_at: self.submitted_at,
            posted_at: self.posted_at,
            reversed_at: self.reversed_at,
            reverse_reason: self.reverse_reason.clone(),
            remark: self.remark.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// 一行明细（`biz_salary_detail` + 明细项条数）。
#[derive(Debug, Clone)]
pub struct DetailRow {
    /// 明细 id。
    pub id: Uuid,
    /// 单头 id。
    pub sheet_id: Uuid,
    /// 员工 id。
    pub employee_id: Uuid,
    /// 工号（快照）。
    pub employee_no: String,
    /// 姓名（快照）。
    pub employee_name: String,
    /// 部门 id。
    pub dept_id: Option<Uuid>,
    /// 部门名（快照）。
    pub dept_name: Option<String>,
    /// 账号 id。
    pub user_id: Option<Uuid>,
    /// 岗位。
    pub position_code: Option<String>,
    /// 职级。
    pub job_level: Option<String>,
    /// 应出勤。
    pub should_days: Decimal,
    /// 实出勤。
    pub actual_days: Decimal,
    /// 无薪假。
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
    /// 备注。
    pub remark: Option<String>,
    /// 明细项条数（`with_items = false` 时也要给，前端显示「15 项」）。
    pub item_count: i32,
}

impl DetailRow {
    /// 由结果行投影（列名与 [`detail_query`] 的 `SELECT` 逐个对齐）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            sheet_id: row.get_typed("sheet_id")?,
            employee_id: row.get_typed("employee_id")?,
            employee_no: row.get_typed("employee_no")?,
            employee_name: row.get_typed("employee_name")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            user_id: row.get_typed("user_id")?,
            position_code: row.get_typed("position_code")?,
            job_level: row.get_typed("job_level")?,
            should_days: row.get_typed("should_days")?,
            actual_days: row.get_typed("actual_days")?,
            leave_unpaid_days: row.get_typed("leave_unpaid_days")?,
            ot_hours: row.get_typed("ot_hours")?,
            earning_amount: row.get_typed("earning_amount")?,
            pre_deduct_amount: row.get_typed("pre_deduct_amount")?,
            gross_amount: row.get_typed("gross_amount")?,
            social_personal: row.get_typed("social_personal")?,
            fund_personal: row.get_typed("fund_personal")?,
            iit_amount: row.get_typed("iit_amount")?,
            other_withhold: row.get_typed("other_withhold")?,
            withhold_amount: row.get_typed("withhold_amount")?,
            net_amount: row.get_typed("net_amount")?,
            si_company: row.get_typed("si_company")?,
            hf_company: row.get_typed("hf_company")?,
            remark: row.get_typed("remark")?,
            item_count: row.get_typed("item_count")?,
        })
    }

    /// 明细回执（明细项由调用方批量塞进来）。
    pub fn to_res(&self, items: Vec<SalaryItemLineRes>) -> SalaryDetailRes {
        SalaryDetailRes {
            id: self.id.to_string(),
            sheet_id: self.sheet_id.to_string(),
            employee_id: self.employee_id.to_string(),
            employee_no: self.employee_no.clone(),
            employee_name: self.employee_name.clone(),
            dept_id: self.dept_id.map(|id| id.to_string()),
            dept_name: self.dept_name.clone(),
            user_id: self.user_id.map(|id| id.to_string()),
            position_code: self.position_code.clone(),
            job_level: self.job_level.clone(),
            should_days: self.should_days,
            actual_days: self.actual_days,
            leave_unpaid_days: self.leave_unpaid_days,
            ot_hours: self.ot_hours,
            earning_amount: self.earning_amount,
            pre_deduct_amount: self.pre_deduct_amount,
            gross_amount: self.gross_amount,
            social_personal: self.social_personal,
            fund_personal: self.fund_personal,
            iit_amount: self.iit_amount,
            other_withhold: self.other_withhold,
            withhold_amount: self.withhold_amount,
            net_amount: self.net_amount,
            si_company: self.si_company,
            hf_company: self.hf_company,
            accrual_amount: self.gross_amount + self.si_company + self.hf_company,
            remark: self.remark.clone(),
            items,
            item_count: self.item_count,
        }
    }
}

/// 一行明细项。
#[derive(Debug, Clone)]
pub struct DetailItemRow {
    /// 明细 id（分组键）。
    pub detail_id: Uuid,
    /// 行 id。
    pub id: Uuid,
    /// 项编码。
    pub item_code: String,
    /// 项名称（快照）。
    pub item_name: String,
    /// 项类型。
    pub item_type: String,
    /// 算法。
    pub calc_type: String,
    /// 金额（可负）。
    pub amount: Decimal,
    /// 公式快照。
    pub expr_snapshot: Option<String>,
    /// 代入值说明。
    pub calc_note: String,
    /// 行号。
    pub line_no: i32,
}

impl DetailItemRow {
    /// 由结果行投影。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            detail_id: row.get_typed("detail_id")?,
            id: row.get_typed("id")?,
            item_code: row.get_typed("item_code")?,
            item_name: row.get_typed("item_name")?,
            item_type: row.get_typed("item_type")?,
            calc_type: row.get_typed("calc_type")?,
            amount: row.get_typed("amount")?,
            expr_snapshot: row.get_typed("expr_snapshot")?,
            calc_note: row.get_typed("calc_note")?,
            line_no: row.get_typed("line_no")?,
        })
    }

    /// 明细项回执。
    pub fn to_res(&self) -> SalaryItemLineRes {
        SalaryItemLineRes {
            id: Some(self.id.to_string()),
            item_code: self.item_code.clone(),
            item_name: self.item_name.clone(),
            item_type_label: crate::business::hr::salary_item_dto::label_of(
                &crate::business::hr::salary_item_dto::ITEM_TYPES,
                &self.item_type,
            ),
            calc_type_label: crate::business::hr::salary_item_dto::label_of(
                &crate::business::hr::salary_item_dto::CALC_TYPES,
                &self.calc_type,
            ),
            item_type: self.item_type.clone(),
            calc_type: self.calc_type.clone(),
            amount: self.amount,
            expr_snapshot: self.expr_snapshot.clone(),
            calc_note: self.calc_note.clone(),
            line_no: self.line_no,
        }
    }
}

// ==================== 查询 ====================

/// 读单头（软删的单当不存在）。
pub async fn find_sheet(id: Uuid) -> DbResult<Option<SheetRow>> {
    let sql = format!("SELECT {SHEET_COLUMNS} FROM biz_salary_sheet WHERE id = ?::uuid AND deleted_at IS NULL");
    let row = db().query_first_sql(sql, vec![Value::from(&id)]).await?;
    match row {
        None => Ok(None),
        Some(row) => SheetRow::from_row(&row).map(Some),
    }
}

/// 薪资单列表的 SQL 与参数（纯函数：单测对拍「`?` 个数 == 参数个数」）。
fn sheet_list_query(filter: &SalarySheetFilter) -> (String, Vec<Value>) {
    let mut sql = format!("SELECT {SHEET_COLUMNS} FROM biz_salary_sheet WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(period_code) = filter.period_code.as_deref() {
        sql.push_str(" AND period_code = ?");
        params.push(Value::Text(period_code.to_string()));
    }
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(keyword) = filter.keyword.as_deref() {
        sql.push_str(" AND sheet_no ILIKE ?");
        params.push(Value::Text(format!("%{keyword}%")));
    }
    (sql, params)
}

/// 薪资单列表（设计 §5.2 #36「期间、状态、金额合计」；`ROUTE_CONTRACT` 把这条挂在 HR-36）。
///
/// 排序：期间倒序 → 单号倒序。财务翻列表永远是「先看最近这个月」，正序要翻到最后才能看。
pub async fn page_sheets(filter: &SalarySheetFilter, page: u64, page_size: u64) -> DbResult<(Vec<SheetRow>, i64)> {
    let (sql, params) = sheet_list_query(filter);
    let result = scope::paginate_unscoped(
        SHEET_TARGET,
        &sql,
        params,
        Some("ORDER BY period_code DESC, sheet_no DESC"),
        page,
        page_size,
    )
    .await?;
    let total = i64::try_from(result.total).unwrap_or(i64::MAX);
    let mut rows = Vec::with_capacity(result.list.len());
    for row in &result.list {
        rows.push(SheetRow::from_row(row)?);
    }
    Ok((rows, total))
}

/// 明细行实算的合计（A6 的对账口径）。
///
/// `COALESCE` 是必须的：一张零明细的单（理论上建不出来，但软删/人工改库能造出来）
/// `SUM` 返回 `NULL`，不兜底会把「读详情」变成 500。
pub async fn totals_of_details(sheet_id: Uuid) -> DbResult<CalcTotals> {
    const SQL: &str = "\
SELECT count(*) AS employee_count, \
       COALESCE(SUM(earning_amount), 0) AS earning_total, \
       COALESCE(SUM(pre_deduct_amount), 0) AS pre_deduct_total, \
       COALESCE(SUM(gross_amount), 0) AS gross_total, \
       COALESCE(SUM(withhold_amount), 0) AS withhold_total, \
       COALESCE(SUM(net_amount), 0) AS net_total, \
       COALESCE(SUM(si_company), 0) AS si_company_total, \
       COALESCE(SUM(hf_company), 0) AS hf_company_total \
  FROM biz_salary_detail WHERE sheet_id = ?::uuid";
    let row = db()
        .query_first_sql(SQL.to_string(), vec![Value::from(&sheet_id)])
        .await?
        .ok_or_else(|| DbError::InvalidValue("明细合计查询没有返回行（不应发生）".to_string()))?;
    let count: i64 = row.get_typed("employee_count")?;
    Ok(CalcTotals {
        employee_count: i32::try_from(count).unwrap_or(i32::MAX),
        earning_total: row.get_typed("earning_total")?,
        pre_deduct_total: row.get_typed("pre_deduct_total")?,
        gross_total: row.get_typed("gross_total")?,
        withhold_total: row.get_typed("withhold_total")?,
        net_total: row.get_typed("net_total")?,
        si_company_total: row.get_typed("si_company_total")?,
        hf_company_total: row.get_typed("hf_company_total")?,
    })
}

/// 明细项按类型的合计（A7 的对账口径）。
pub async fn item_totals(sheet_id: Uuid) -> DbResult<SalaryItemTypeTotalsRes> {
    const SQL: &str = "\
SELECT COALESCE(SUM(CASE WHEN item_type = 'EARNING'    THEN amount ELSE 0 END), 0) AS earning_total, \
       COALESCE(SUM(CASE WHEN item_type = 'PRE_DEDUCT' THEN amount ELSE 0 END), 0) AS pre_deduct_total, \
       COALESCE(SUM(CASE WHEN item_type = 'WITHHOLD'   THEN amount ELSE 0 END), 0) AS withhold_total, \
       COALESCE(SUM(CASE WHEN item_type = 'EMPLOYER'   THEN amount ELSE 0 END), 0) AS employer_total, \
       count(*) AS line_count \
  FROM biz_salary_detail_item WHERE sheet_id = ?::uuid";
    let row = db()
        .query_first_sql(SQL.to_string(), vec![Value::from(&sheet_id)])
        .await?
        .ok_or_else(|| DbError::InvalidValue("明细项合计查询没有返回行（不应发生）".to_string()))?;
    Ok(SalaryItemTypeTotalsRes {
        earning_total: row.get_typed("earning_total")?,
        pre_deduct_total: row.get_typed("pre_deduct_total")?,
        withhold_total: row.get_typed("withhold_total")?,
        employer_total: row.get_typed("employer_total")?,
        line_count: row.get_typed("line_count")?,
    })
}

/// 部门汇总（工资表按部门分组）。
///
/// `dept_id` 为空的行（没分部门的员工）也要出现，否则「部门合计加起来 ≠ 单头合计」，
/// 前端做交叉校验时会以为少算了一笔。排序把空部门放最后。
pub async fn dept_summary(sheet_id: Uuid) -> DbResult<Vec<SalaryDeptSummaryRes>> {
    const SQL: &str = "\
SELECT d.dept_id, d.dept_name, count(*) AS employee_count, \
       COALESCE(SUM(d.earning_amount), 0) AS earning_total, \
       COALESCE(SUM(d.pre_deduct_amount), 0) AS pre_deduct_total, \
       COALESCE(SUM(d.gross_amount), 0) AS gross_total, \
       COALESCE(SUM(d.withhold_amount), 0) AS withhold_total, \
       COALESCE(SUM(d.net_amount), 0) AS net_total, \
       COALESCE(SUM(d.si_company), 0) AS si_company_total, \
       COALESCE(SUM(d.hf_company), 0) AS hf_company_total \
  FROM biz_salary_detail d \
 WHERE d.sheet_id = ?::uuid \
 GROUP BY d.dept_id, d.dept_name \
 ORDER BY (d.dept_name IS NULL), d.dept_name, d.dept_id";
    let rows = db().query_sql(SQL.to_string(), vec![Value::from(&sheet_id)]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let count: i64 = row.get_typed("employee_count")?;
        let dept_id: Option<Uuid> = row.get_typed("dept_id")?;
        let dept_name: Option<String> = row.get_typed("dept_name")?;
        let gross: Decimal = row.get_typed("gross_total")?;
        let si: Decimal = row.get_typed("si_company_total")?;
        let hf: Decimal = row.get_typed("hf_company_total")?;
        out.push(SalaryDeptSummaryRes {
            dept_id: dept_id.map(|id| id.to_string()),
            dept_name: dept_name.unwrap_or_else(|| "(未分配部门)".to_string()),
            employee_count: i32::try_from(count).unwrap_or(i32::MAX),
            earning_total: row.get_typed("earning_total")?,
            pre_deduct_total: row.get_typed("pre_deduct_total")?,
            gross_total: gross,
            withhold_total: row.get_typed("withhold_total")?,
            net_total: row.get_typed("net_total")?,
            si_company_total: si,
            hf_company_total: hf,
            accrual_total: gross + si + hf,
        });
    }
    Ok(out)
}

/// 明细查询的 SQL 与参数（纯函数：单测对拍「`?` 个数 == 参数个数」）。
///
/// 三处 `?::uuid` 的顺序是 **子查询的 sheet_id → 主 WHERE 的 sheet_id → 筛选条件**，
/// 这个顺序**不能**调换（`count(*)` 包装不会重排参数，但会原样带过去）。
fn detail_query(filter: &SalaryDetailFilter) -> (String, Vec<Value>) {
    let mut sql = format!(
        "SELECT {DETAIL_COLUMNS}, \
                COALESCE(i.item_count, 0) AS item_count \
           FROM biz_salary_detail d \
           LEFT JOIN (SELECT detail_id, count(*) AS item_count \
                        FROM biz_salary_detail_item WHERE sheet_id = ?::uuid GROUP BY detail_id) i \
                  ON i.detail_id = d.id \
          WHERE d.sheet_id = ?::uuid",
    );
    let mut params = vec![Value::from(&filter.sheet_id), Value::from(&filter.sheet_id)];
    if let Some(dept_id) = filter.dept_id {
        sql.push_str(" AND d.dept_id = ?::uuid");
        params.push(Value::from(&dept_id));
    }
    if let Some(keyword) = filter.keyword.as_deref() {
        sql.push_str(" AND (d.employee_no ILIKE ? OR d.employee_name ILIKE ?)");
        let pattern = format!("%{keyword}%");
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
    }
    (sql, params)
}

/// 明细分页（设计 §5.2 #38）。
///
/// 返回 `(本页行, 总数)`；`主键` 排序稳定（同工号按 id），翻页不重不漏。
pub async fn page_details(filter: &SalaryDetailFilter, page: u64, page_size: u64) -> DbResult<(Vec<DetailRow>, i64)> {
    let (sql, params) = detail_query(filter);
    let result = scope::paginate_unscoped(DETAIL_TARGET, &sql, params, Some(DETAIL_ORDER), page, page_size).await?;
    let total = i64::try_from(result.total).unwrap_or(i64::MAX);
    let mut rows = Vec::with_capacity(result.list.len());
    for row in &result.list {
        rows.push(DetailRow::from_row(row)?);
    }
    Ok((rows, total))
}

/// 一页明细的明细项（**一次** `IN` 查询，按 `detail_id` 分组返回）。
///
/// 空 ids 直接返回空 map：拼出 `IN ()` 是语法错误，会让「零人单」的详情页 500。
pub async fn items_of_details(sheet_id: Uuid, detail_ids: &[Uuid]) -> DbResult<HashMap<Uuid, Vec<DetailItemRow>>> {
    if detail_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?::uuid"; detail_ids.len()].join(", ");
    let sql = format!(
        "SELECT i.detail_id, i.id, i.item_code, i.item_name, i.item_type, i.calc_type, i.amount, \
                i.expr_snapshot, i.calc_note, i.line_no \
           FROM biz_salary_detail_item i \
          WHERE i.sheet_id = ?::uuid AND i.detail_id IN ({placeholders}) \
          {ITEM_ORDER}"
    );
    let mut params: Vec<Value> = Vec::with_capacity(detail_ids.len() + 1);
    params.push(Value::from(&sheet_id));
    params.extend(detail_ids.iter().map(Value::from));
    let rows = db().query_sql(sql, params).await?;

    let mut grouped: HashMap<Uuid, Vec<DetailItemRow>> = HashMap::with_capacity(detail_ids.len());
    for row in &rows {
        let item = DetailItemRow::from_row(row)?;
        grouped.entry(item.detail_id).or_default().push(item);
    }
    Ok(grouped)
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(with_dept: bool, with_keyword: bool) -> SalaryDetailFilter {
        SalaryDetailFilter {
            sheet_id: Uuid::now_v7(),
            dept_id: with_dept.then(Uuid::now_v7),
            keyword: with_keyword.then(|| "张三".to_string()),
            with_items: true,
        }
    }

    /// 占位符个数必须等于参数个数（错位是 SQL 绑定最经典的坑，且只在真库上炸）。
    #[test]
    fn placeholder_count_matches_params() {
        for (with_dept, with_keyword) in [(false, false), (true, false), (false, true), (true, true)] {
            let (sql, params) = detail_query(&filter(with_dept, with_keyword));
            let holes = sql.matches('?').count();
            assert_eq!(
                holes,
                params.len(),
                "占位符 {holes} ≠ 参数 {}（dept={with_dept} kw={with_keyword}）\n{sql}",
                params.len()
            );
            // 两处 sheet_id 在前（子查询 → 主 WHERE）
            assert!(sql.contains("WHERE sheet_id = ?::uuid"), "{sql}");
            assert!(sql.contains("FROM biz_salary_detail d"), "{sql}");
            assert!(sql.matches("?::uuid").count() >= 2, "至少两处 uuid 绑定：{sql}");
        }
    }

    /// 两个可选筛同时给时，参数顺序 = 部门 → 关键字（两个相同模式串）。
    #[test]
    fn filter_params_are_ordered() {
        let (sql, params) = detail_query(&filter(true, true));
        assert_eq!(params.len(), 5, "sheet_id ×2 + dept + keyword ×2");
        assert!(
            sql.ends_with("AND (d.employee_no ILIKE ? OR d.employee_name ILIKE ?)"),
            "{sql}"
        );
        match (&params[3], &params[4]) {
            (Value::Text(a), Value::Text(b)) => {
                assert_eq!(a, "%张三%");
                assert_eq!(b, "%张三%");
            }
            other => panic!("关键字参数类型不对：{other:?}"),
        }
    }

    /// 没有筛选时不拼多余的 `AND`（空 WHERE 尾巴会让 count(*) 包装出语法错误）。
    #[test]
    fn no_filter_leaves_clean_where() {
        let (sql, _) = detail_query(&filter(false, false));
        assert!(!sql.contains("AND d.dept_id"), "{sql}");
        assert!(!sql.trim_end().ends_with("AND"), "{sql}");
    }

    /// 单头的 7 个金额列 → 合计回执，计提总额自己加（不能读现成列：表里没有这一列）。
    #[test]
    fn sheet_row_totals_add_up_accrual() {
        let row = SheetRow {
            id: Uuid::now_v7(),
            sheet_no: "SAL-202611-000001".to_string(),
            period_code: "2026-11".to_string(),
            period_start: "2026-11-01".parse().expect("日期"),
            period_end: "2026-11-30".parse().expect("日期"),
            status: "CALCULATED".to_string(),
            attendance_period_id: None,
            employee_count: 2,
            earning_total: "9954.0300".parse().expect("金额"),
            pre_deduct_total: "735.6300".parse().expect("金额"),
            gross_total: "9218.4000".parse().expect("金额"),
            withhold_total: "1800.0000".parse().expect("金额"),
            net_total: "7418.4000".parse().expect("金额"),
            si_company_total: "1680.0000".parse().expect("金额"),
            hf_company_total: "960.0000".parse().expect("金额"),
            accrual_voucher_id: None,
            accrual_voucher_no: None,
            payment_voucher_id: None,
            payment_voucher_no: None,
            reversal_voucher_id: None,
            approval_instance_id: None,
            calculated_at: None,
            calculated_by: None,
            submitted_at: None,
            posted_at: None,
            reversed_at: None,
            reverse_reason: None,
            remark: None,
            created_at: None,
            updated_at: None,
        };
        let totals = row.totals();
        assert_eq!(totals.employee_count, 2);
        assert_eq!(
            totals.accrual_total,
            totals.gross_total + totals.si_company_total + totals.hf_company_total
        );
        let header = row.to_header();
        assert_eq!(header.status_label, "已计算");
        assert_eq!(header.sheet_no, "SAL-202611-000001");
    }
}
