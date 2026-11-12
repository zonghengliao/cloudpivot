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
use crate::shared::error_code;
use crate::utils;

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

/// 一行明细 + 明细项条数的 SQL（[`DetailRow::from_row`] 按列名取值，两处必须一致）。
///
/// 不拼 `ORDER BY` / `FOR UPDATE`：调用方各自追加（锁读 / 普通读共用同一份投影）。
pub const DETAIL_ROW_SQL: &str = "SELECT d.id, d.sheet_id, d.employee_id, d.employee_no, d.employee_name, \
     d.dept_id, d.dept_name, d.user_id, d.position_code, d.job_level, d.should_days, d.actual_days, \
     d.leave_unpaid_days, d.ot_hours, d.earning_amount, d.pre_deduct_amount, d.gross_amount, \
     d.social_personal, d.fund_personal, d.iit_amount, d.other_withhold, d.withhold_amount, \
     d.net_amount, d.si_company, d.hf_company, d.remark, \
     (SELECT count(*) FROM biz_salary_detail_item i WHERE i.detail_id = d.id)::int AS item_count \
     FROM biz_salary_detail d \
    WHERE d.id = ?::uuid AND d.sheet_id = ?::uuid";

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
    find_sheet_in(db(), id).await
}

/// 读单头（指定执行器：事务内读回用 —— 覆盖写完要读**本事务**里的新值，
/// 用另一个连接读会读到旧快照）。
pub async fn find_sheet_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<SheetRow>> {
    let sql = format!("SELECT {SHEET_COLUMNS} FROM biz_salary_sheet WHERE id = ?::uuid AND deleted_at IS NULL");
    let row = ex.query_first_sql(sql, vec![Value::from(&id)]).await?;
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

// ==================== 写路径（HR-37 明细覆盖） ====================

/// 明细**写路径**的数据权限锚点（`dept_col = dept_id`、`owner_col = user_id`，设计 D-12）。
///
/// # 为什么它不是 [`DETAIL_TARGET`]
///
/// [`DETAIL_TARGET`] 是**读路径**（分页）的锚点，显式 `unscoped`：`biz_salary_detail`
/// 还没登记进 `SCOPED_TABLES`（登记动作落在 `/salary/my` 的卡上）。**写路径不能跟着放宽**：
/// 改别人工资是越权，越权判定的口径按设计 D-12 原样生效，不等登记。
///
/// 两处并存不是矛盾，是「读的一侧暂缓、写的一侧先紧」：读得宽顶多是信息范围大，
/// 写得宽就是能把别人的工资改掉 —— 后者没有理由等任何一张卡。
pub const DETAIL_SCOPE_TARGET: scope::ScopeTarget =
    scope::ScopeTarget::new(TABLE_DETAIL, "d", Some("dept_id"), Some("user_id"));

/// 一条明细项 + 它所属薪资项的贷方科目（覆盖时 `EMPLOYER` 归桶的第二判据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideItemRow {
    /// 明细项行 id（写回金额用）。
    pub id: Uuid,
    /// 薪资项编码。
    pub item_code: String,
    /// 项名称（快照）。
    pub item_name: String,
    /// 项类型。
    pub item_type: String,
    /// 算法。
    pub calc_type: String,
    /// 金额。
    pub amount: Decimal,
    /// 公式原文快照（**不改**：它是「当初按什么公式算的」的证据）。
    pub expr_snapshot: Option<String>,
    /// 代入值说明（覆盖后会被重写成「人工覆盖 原→新」）。
    pub calc_note: String,
    /// 项的贷方科目（`biz_salary_item.credit_account_code`；项被删 / 未填时 `None`）。
    pub credit_account_code: Option<String>,
    /// 行号。
    pub line_no: i32,
}

impl OverrideItemRow {
    /// 由结果行投影。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            item_code: row.get_typed("item_code")?,
            item_name: row.get_typed("item_name")?,
            item_type: row.get_typed("item_type")?,
            calc_type: row.get_typed("calc_type")?,
            amount: row.get_typed("amount")?,
            expr_snapshot: row.get_typed("expr_snapshot")?,
            calc_note: row.get_typed::<Option<String>>("calc_note")?.unwrap_or_default(),
            credit_account_code: row.get_typed("credit_account_code")?,
            line_no: row.get_typed("line_no")?,
        })
    }

    /// 转成覆盖纯函数要的快照（丢掉写入用的 `id` / `calc_type` / 快照文案）。
    pub fn to_amount(&self) -> crate::business::hr::salary_override_dto::ItemAmount {
        crate::business::hr::salary_override_dto::ItemAmount {
            item_code: self.item_code.clone(),
            item_name: self.item_name.clone(),
            item_type: self.item_type.clone(),
            amount: self.amount,
            credit_account_code: self.credit_account_code.clone(),
        }
    }

    /// 覆盖后的明细项回执（金额与说明用覆盖后的值，其余照快照）。
    pub fn to_line(&self, amount: Decimal, note: &str) -> crate::business::hr::salary_sheet_dto::SalaryItemLineRes {
        use crate::business::hr::salary_item_dto as item_dto;
        crate::business::hr::salary_sheet_dto::SalaryItemLineRes {
            id: Some(self.id.to_string()),
            item_code: self.item_code.clone(),
            item_name: self.item_name.clone(),
            item_type: self.item_type.clone(),
            item_type_label: item_dto::label_of(&item_dto::ITEM_TYPES, &self.item_type),
            calc_type: self.calc_type.clone(),
            calc_type_label: item_dto::label_of(&item_dto::CALC_TYPES, &self.calc_type),
            amount,
            expr_snapshot: self.expr_snapshot.clone(),
            calc_note: note.to_string(),
            line_no: self.line_no,
        }
    }
}

/// 锁单头行（`FOR UPDATE`）：设计 §4.8.3 的并发口径 —— 两人同时改同一张单，
/// 后到的等锁，重算覆盖前一次结果。
///
/// 不加锁的话「读明细 → 算汇总 → 写汇总」三步之间会被别人插进一笔覆盖，
/// 后写的一方用自己的旧快照覆盖掉对方的改动（丢失更新）。
pub async fn lock_sheet(ex: &impl SqlExecutor, sheet_id: Uuid) -> DbResult<Option<SheetRow>> {
    let sql =
        format!("SELECT {SHEET_COLUMNS} FROM biz_salary_sheet WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE");
    let row = ex.query_first_sql(sql, vec![Value::from(&sheet_id)]).await?;
    match row {
        None => Ok(None),
        Some(row) => SheetRow::from_row(&row).map(Some),
    }
}

/// 锁一行明细（`FOR UPDATE OF d`）+ 带出明细项条数。
///
/// `sheet_id` 也进 `WHERE`：前端传错单号时不能靠 `detail_id` 就改到别的单上。
pub async fn lock_detail(ex: &impl SqlExecutor, sheet_id: Uuid, detail_id: Uuid) -> DbResult<Option<DetailRow>> {
    let sql = format!("{DETAIL_ROW_SQL} FOR UPDATE OF d");
    let row = ex
        .query_first_sql(sql, vec![Value::from(&detail_id), Value::from(&sheet_id)])
        .await?;
    match row {
        None => Ok(None),
        Some(row) => DetailRow::from_row(&row).map(Some),
    }
}

/// 读一行明细（指定执行器：事务内读回用）。
pub async fn find_detail_in(ex: &impl SqlExecutor, sheet_id: Uuid, detail_id: Uuid) -> DbResult<Option<DetailRow>> {
    let row = ex
        .query_first_sql(
            DETAIL_ROW_SQL.to_string(),
            vec![Value::from(&detail_id), Value::from(&sheet_id)],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => DetailRow::from_row(&row).map(Some),
    }
}

/// 数据权限探针的 SQL 与参数（**纯函数**：单测对拍「`?` 个数 == 参数个数」）。
///
/// 片段在前、业务参数在后 —— 与 `scope::paginate` 的绑定顺序一致（架构 §2.6 规则 2）。
fn scope_probe_query(sheet_id: Uuid, detail_id: Uuid, filter: &scope::RowFilter) -> (String, Vec<Value>) {
    let sql = format!(
        "SELECT 1 AS ok FROM {TABLE_DETAIL} d WHERE 1 = 1{} AND d.id = ?::uuid AND d.sheet_id = ?::uuid",
        filter.sql()
    );
    let mut params: Vec<Value> = filter.params().to_vec();
    params.push(Value::from(&detail_id));
    params.push(Value::from(&sheet_id));
    (sql, params)
}

/// 这一行明细对当前主体**可见**吗（写路径的越权闸）。
///
/// 语义：
///
/// * 超管 / `ALL` 范围 → 恒真（[`scope::filter_for`] 短路 `AND 1 = 1`）；
/// * 部门类范围 → `d.dept_id IN (可见部门)`；
/// * `SELF` → `d.user_id = 我`（员工的工资明细由 `employee.user_id` 派生 `user_id` 列）。
///
/// 返回 `false` 的两种成因由调用方区分：行不存在（404）vs 行存在但不在范围（403）。
pub async fn detail_in_scope(
    ex: &impl SqlExecutor,
    sheet_id: Uuid,
    detail_id: Uuid,
    principal: &crate::business::shared::principal::ScopedPrincipal,
) -> DbResult<bool> {
    let filter = scope::filter_for(principal, DETAIL_SCOPE_TARGET);
    let (sql, params) = scope_probe_query(sheet_id, detail_id, &filter);
    let row = ex.query_first_sql(sql, params).await?;
    Ok(row.is_some())
}

/// 一行明细的全部明细项（含项定义上的贷方科目），按 `line_no` 稳定排序。
///
/// `biz_salary_item` 用 `LEFT JOIN`：薪资项被删（`item_id` 置 `NULL`）后明细项快照仍在，
/// 覆盖时科目为空 —— 那时 `EMPLOYER` 项若为非零就会被 [`summarize`] 拦下点名，
/// 而不是静默丢进「其他」桶。
pub async fn items_for_override(
    ex: &impl SqlExecutor,
    sheet_id: Uuid,
    detail_id: Uuid,
) -> DbResult<Vec<OverrideItemRow>> {
    let sql = "SELECT i.id, i.item_code, i.item_name, i.item_type, i.calc_type, i.amount, \
                      i.expr_snapshot, i.calc_note, si.credit_account_code, i.line_no \
                 FROM biz_salary_detail_item i \
                 LEFT JOIN biz_salary_item si ON si.id = i.item_id \
                WHERE i.sheet_id = ?::uuid AND i.detail_id = ?::uuid \
                ORDER BY i.line_no, i.item_code"
        .to_string();
    let rows = ex
        .query_sql(sql, vec![Value::from(&sheet_id), Value::from(&detail_id)])
        .await?;
    rows.iter().map(OverrideItemRow::from_row).collect()
}

/// 写回一项明细项的 SQL（3 个 `?`：金额 / 说明 / 行 id）。
pub const ITEM_UPDATE_SQL: &str =
    "UPDATE biz_salary_detail_item SET amount = ?::text::numeric, calc_note = ? WHERE id = ?::uuid";

/// 写回明细行 11 个金额列 + `remark` 的 SQL（13 个 `?`：11 金额 + remark + id）。
pub const DETAIL_UPDATE_SQL: &str = "UPDATE biz_salary_detail SET \
        earning_amount = ?::text::numeric, pre_deduct_amount = ?::text::numeric, \
        gross_amount = ?::text::numeric, social_personal = ?::text::numeric, \
        fund_personal = ?::text::numeric, iit_amount = ?::text::numeric, \
        other_withhold = ?::text::numeric, withhold_amount = ?::text::numeric, \
        net_amount = ?::text::numeric, si_company = ?::text::numeric, \
        hf_company = ?::text::numeric, remark = COALESCE(?, remark), \
        updated_at = CURRENT_TIMESTAMP \
      WHERE id = ?::uuid";

/// 单头合计重算 SQL（2 个 `?`：聚合子查询的 `sheet_id` + 外层 `WHERE` 的 `id`）。
pub const SHEET_RECALC_SQL: &str = "\
UPDATE biz_salary_sheet s \
   SET employee_count = t.employee_count, \
       earning_total = t.earning_total, pre_deduct_total = t.pre_deduct_total, \
       gross_total = t.gross_total, withhold_total = t.withhold_total, \
       net_total = t.net_total, si_company_total = t.si_company_total, \
       hf_company_total = t.hf_company_total, updated_at = CURRENT_TIMESTAMP \
  FROM (SELECT count(*)::int AS employee_count, \
               COALESCE(SUM(earning_amount), 0) AS earning_total, \
               COALESCE(SUM(pre_deduct_amount), 0) AS pre_deduct_total, \
               COALESCE(SUM(gross_amount), 0) AS gross_total, \
               COALESCE(SUM(withhold_amount), 0) AS withhold_total, \
               COALESCE(SUM(net_amount), 0) AS net_total, \
               COALESCE(SUM(si_company), 0) AS si_company_total, \
               COALESCE(SUM(hf_company), 0) AS hf_company_total \
          FROM biz_salary_detail WHERE sheet_id = ?::uuid) t \
 WHERE s.id = ?::uuid AND s.deleted_at IS NULL";

/// 写回一项被覆盖的金额 + `calc_note`（`expr_snapshot` 不动：公式原文是当初怎么算的证据）。
pub async fn update_item_amount(ex: &impl SqlExecutor, item_id: Uuid, amount: Decimal, note: &str) -> DbResult<()> {
    ex.exec_sql(
        ITEM_UPDATE_SQL.to_string(),
        vec![
            Value::Text(amount.to_string()),
            Value::Text(note.to_string()),
            Value::from(&item_id),
        ],
    )
    .await?;
    Ok(())
}

/// 用重算结果重写明细行的 11 个金额列（`remark` 只在传了值时才改）。
///
/// 参数顺序与 [`crate::business::hr::salary_override_dto::DetailAmounts`] 的字段顺序一致；
/// 单测 `detail_update_placeholder_count_matches_params` 钉住这一点。
pub async fn update_detail_amounts(
    ex: &impl SqlExecutor,
    detail_id: Uuid,
    amounts: &crate::business::hr::salary_override_dto::DetailAmounts,
    remark: Option<&str>,
) -> DbResult<()> {
    ex.exec_sql(
        DETAIL_UPDATE_SQL.to_string(),
        vec![
            Value::Text(amounts.earning_amount.to_string()),
            Value::Text(amounts.pre_deduct_amount.to_string()),
            Value::Text(amounts.gross_amount.to_string()),
            Value::Text(amounts.social_personal.to_string()),
            Value::Text(amounts.fund_personal.to_string()),
            Value::Text(amounts.iit_amount.to_string()),
            Value::Text(amounts.other_withhold.to_string()),
            Value::Text(amounts.withhold_amount.to_string()),
            Value::Text(amounts.net_amount.to_string()),
            Value::Text(amounts.si_company.to_string()),
            Value::Text(amounts.hf_company.to_string()),
            Value::Text(remark.unwrap_or_default().to_string()),
            Value::from(&detail_id),
        ],
    )
    .await?;
    Ok(())
}

/// 单头 7 个金额列 + 人数 = `SUM(明细)`（**覆盖后必做的第二层重算**）。
///
/// 一条语句完成：`UPDATE … FROM (聚合子查询)`。分成「先 SELECT 再 UPDATE」会多一次往返，
/// 也会在两次之间被别人插一笔（已在 `lock_sheet` 里锁了单头行，但少一次往返总归更稳）。
///
/// `employee_count` 一起重算：明细增减之后「人数」必须跟着走，否则单头自报的人数
/// 与 `SUM(明细)` 的行数对不上（设计 §4.9 A6 的相邻断言）。
pub async fn recalc_sheet_totals(ex: &impl SqlExecutor, sheet_id: Uuid) -> DbResult<()> {
    ex.exec_sql(
        SHEET_RECALC_SQL.to_string(),
        vec![Value::from(&sheet_id), Value::from(&sheet_id)],
    )
    .await?;
    Ok(())
}

// ==================== 状态回写（HR-39：提交 / 审批回调 / 过账） ====================

/// 状态 CAS 回写 SQL（4 个 `?`：新状态 / 操作人 / id / 期望的旧状态）。
///
/// `AND status = ?` 是**并发闸**：两个人同时点「提交 / 过账 / 审批同意」时，
/// 后到的那次 `affected = 0`，由调用方翻成 409 而不是静默覆盖。
pub const SHEET_STATUS_SQL: &str = "UPDATE biz_salary_sheet SET \
        status = ?, updated_at = CURRENT_TIMESTAMP, \
        update_by = NULLIF(?::text, '')::uuid \
      WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL";

/// 提交审批的 CAS 回写 SQL（7 个 `?`：新状态 / 操作人 / 实例 id / id / 期望旧状态 / 操作人 / 操作人）。
///
/// 与 [`SHEET_STATUS_SQL`] 分开的原因：提交要**同时**写 `submitted_at` /
/// `submitted_by` / `approval_instance_id` 三列。拆成两条语句会在中间留一个
/// 「状态已提交、实例 id 还没写」的窗口（这期间过账闸看不到实例，审批也找不到单）。
pub const SHEET_SUBMIT_SQL: &str = "UPDATE biz_salary_sheet SET \
        status = ?, submitted_at = CURRENT_TIMESTAMP, \
        submitted_by = NULLIF(?::text, '')::uuid, \
        approval_instance_id = ?::uuid, updated_at = CURRENT_TIMESTAMP, \
        update_by = NULLIF(?::text, '')::uuid \
      WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL";

/// 过账的 CAS 回写 SQL（9 个 `?`：状态 / 计提 id / 计提号 / 发放 id / 发放号 / 操作人 ×3 / id / 期望旧状态）。
pub const SHEET_POSTED_SQL: &str = "UPDATE biz_salary_sheet SET \
        status = ?, accrual_voucher_id = ?::uuid, accrual_voucher_no = ?, \
        payment_voucher_id = ?::uuid, payment_voucher_no = ?, \
        posted_at = CURRENT_TIMESTAMP, posted_by = NULLIF(?::text, '')::uuid, \
        updated_at = CURRENT_TIMESTAMP, update_by = NULLIF(?::text, '')::uuid \
      WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL";

/// 状态 CAS 回写（`from` → `to`）。返回受影响行数：`0` = 不存在 / 已软删 / 状态已变。
///
/// 状态机（`shared::state_machine::transition`）只写 `biz_status_log`，**不回写业务表**；
/// 业务表的状态列由各 handler / 桥用本函数 CAS 落地 —— 与 `leave_model::set_status` 同款。
pub async fn set_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from_status: &str,
    to_status: &str,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 单据状态回写（按主键 CAS）。
    ex.exec_sql(
        SHEET_STATUS_SQL.to_string(),
        vec![
            Value::Text(to_status.to_string()),
            opt_uuid(operator),
            Value::from(&id),
            Value::Text(from_status.to_string()),
        ],
    )
    .await
}

/// 提交审批的 CAS 回写（状态 + `submitted_at/by` + `approval_instance_id` 一条语句）。
pub async fn set_submitted(
    ex: &impl SqlExecutor,
    id: Uuid,
    from_status: &str,
    to_status: &str,
    instance_id: Uuid,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 单据状态回写（按主键 CAS）。
    ex.exec_sql(
        SHEET_SUBMIT_SQL.to_string(),
        vec![
            Value::Text(to_status.to_string()),
            opt_uuid(operator),
            Value::from(&instance_id),
            opt_uuid(operator),
            Value::from(&id),
            Value::Text(from_status.to_string()),
        ],
    )
    .await
}

/// 过账结果的 CAS 回写（状态 + 两张凭证 id/no + `posted_at/by`）。
pub async fn mark_posted(
    ex: &impl SqlExecutor,
    id: Uuid,
    accrual: (Uuid, &str),
    payment: (Uuid, &str),
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 单据状态回写（按主键 CAS）。
    ex.exec_sql(
        SHEET_POSTED_SQL.to_string(),
        vec![
            Value::Text(crate::business::hr::salary_sheet_dto::STATUS_POSTED.to_string()),
            Value::from(&accrual.0),
            Value::Text(accrual.1.to_string()),
            Value::from(&payment.0),
            Value::Text(payment.1.to_string()),
            opt_uuid(operator),
            opt_uuid(operator),
            Value::from(&id),
            Value::Text(crate::business::hr::salary_sheet_dto::STATUS_APPROVED.to_string()),
        ],
    )
    .await
}

/// 只回写审批实例 id（撤回 / 作废时清空）。返回受影响行数。
pub async fn set_approval_instance(
    ex: &impl SqlExecutor,
    id: Uuid,
    instance_id: Option<Uuid>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 单据关联回写。
    ex.exec_sql(
        "UPDATE biz_salary_sheet SET approval_instance_id = NULLIF(?::text, '')::uuid, \
                updated_at = CURRENT_TIMESTAMP, update_by = NULLIF(?::text, '')::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![opt_uuid(instance_id), opt_uuid(operator), Value::from(&id)],
    )
    .await
}

/// 可空 uuid 参数（SQL 里包 `NULLIF(?::text, '')::uuid`：空串落 NULL，不是报错）。
fn opt_uuid(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

/// 写路径失败的人话翻译：唯一键 / CHECK / 其余。
///
/// 薪资单的两条唯一索引与六条 `CHECK` 都有明确的业务含义（同期间撞单 / 恒等式被绕过），
/// 直接透出 PG 的原文等于让用户看 SQL —— 这里逐类翻译，并**保留单号**当检索锚点。
pub fn write_error(err: DbError, ctx: &str, sheet_no: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            match constraint.as_deref() {
                Some("uk_biz_sal_sheet_period") => format!(
                    "薪资单写入冲突：这个期间已经有一张未删除的薪资单了（本次写的是 {sheet_no}）。\
                     要重发请用「补差单」，不要删旧单"
                ),
                Some("uk_biz_sal_sheet_no") => {
                    format!("薪资单写入冲突：单号 {sheet_no} 已被占用，请重试（取号会自动顺延）")
                }
                Some(other) => format!("薪资单 {sheet_no} 写入冲突（唯一约束 {other}）：请刷新后重试"),
                None => format!("薪资单 {sheet_no} 写入冲突：请刷新后重试"),
            },
        ),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "薪资单 {sheet_no} 违反库层恒等式（约束 {}）：应发 = 收入 − 应发前扣减、\
                 实发 = 应发 − 代扣 必须恒成立。请重新试算（不要绕过引擎改金额）",
                constraint.as_deref().unwrap_or("<未知>")
            ),
        ),
        _ => utils::db_error(err, ctx),
    }
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

    /// 越权探针的 SQL 形状：片段在前、业务参数在后（`?` 个数 == 参数个数）。
    ///
    /// 三种范围各验一遍：`ALL`（片段恒真无参数）/ `DEPT`（1 个参数）/ `SELF`（1 个参数）。
    /// 片段与参数错位是「越权闸反向」的典型成因：多绑定一个就把 `id` 顶进了部门位。
    #[test]
    fn scope_probe_placeholder_count_matches_params() {
        use crate::business::shared::principal::ScopedPrincipal;
        use crate::business::shared::scope::ScopeKind;

        let cases = [
            ScopedPrincipal::for_test(Uuid::now_v7(), None, true, ScopeKind::All, Vec::new(), false),
            ScopedPrincipal::for_test(
                Uuid::now_v7(),
                Some(Uuid::now_v7()),
                false,
                ScopeKind::Dept,
                vec![Uuid::now_v7()],
                false,
            ),
            ScopedPrincipal::for_test(Uuid::now_v7(), None, false, ScopeKind::SelfOnly, Vec::new(), true),
        ];
        for principal in cases {
            let filter = scope::filter_for(&principal, DETAIL_SCOPE_TARGET);
            let (sql, params) = scope_probe_query(Uuid::now_v7(), Uuid::now_v7(), &filter);
            assert_eq!(
                sql.matches('?').count(),
                params.len(),
                "占位符与参数不匹配（范围 {:?}）：{sql}",
                principal.kind()
            );
            assert!(sql.contains("FROM biz_salary_detail d"), "{sql}");
            assert!(sql.contains("WHERE 1 = 1 AND"), "片段必须在业务条件之前：{sql}");
        }
    }

    /// 三条写路径 SQL 的占位符个数（错位只会在真库上炸，所以在这里先钉住）。
    #[test]
    fn write_sql_placeholder_counts() {
        assert_eq!(ITEM_UPDATE_SQL.matches('?').count(), 3, "金额 / 说明 / id");
        assert_eq!(DETAIL_UPDATE_SQL.matches('?').count(), 13, "11 个金额列 + remark + id");
        assert_eq!(SHEET_RECALC_SQL.matches('?').count(), 2, "聚合 sheet_id + 外层 id");
        // remark 只在传值时覆盖（`COALESCE(?, remark)`）—— 没传不许把已有备注清空
        assert!(DETAIL_UPDATE_SQL.contains("COALESCE(?, remark)"), "{DETAIL_UPDATE_SQL}");
        // 单头重算必须把 7 个金额列 + 人数全部写回（少一列就与 SUM(明细) 漂移）
        for column in [
            "employee_count",
            "earning_total",
            "pre_deduct_total",
            "gross_total",
            "withhold_total",
            "net_total",
            "si_company_total",
            "hf_company_total",
        ] {
            assert!(
                SHEET_RECALC_SQL.contains(&format!("{column} = t.{column}")),
                "重算 SQL 缺列 {column}：{SHEET_RECALC_SQL}"
            );
        }
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
