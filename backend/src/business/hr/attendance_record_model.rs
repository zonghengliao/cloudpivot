//! 考勤记录数据层（`biz_attendance_record` · HR-20）。
//!
//! 本文件是 `biz_attendance_record` **唯一**的 SQL 出口：建 / 读 / 锁定读 / 整行改写 /
//! 台账分页 / 变更留痕快照。契约在 [`crate::business::hr::attendance_record_dto`]，
//! 接口层在 [`crate::business::hr::attendance_record_handler`]。
//!
//! # 表结构的唯一依据
//!
//! `backend/migrations/business/V0383__hr_tables.up.sql` 的 `biz_attendance_record`
//! （设计 §3.1 ⑤）。列名 / `CHECK` 名 / 唯一索引名逐字取自该迁移；**迁移已应用且不可改**
//! （AGENTS.md 约定 6），本卡零迁移。
//!
//! # 「台账按人一行」的库层保证
//!
//! `uk_biz_att_rec (period_id, employee_id) WHERE deleted_at IS NULL`。本文件的
//! [`insert`] 不自造 upsert 语法：撞这个唯一键就是「这个人这一期已经有行了」，翻译成
//! 409 让人去改那一行（否则重复导入会静默产生两条口径、`SUM` 翻倍）。
//!
//! # 分页为什么走 `scope::paginate`（不是 `paginate_unscoped`）
//!
//! 设计 §4.7 的登记表清单里，`biz_attendance_record` **是**范围表
//! （`dept_col = dept_id`、`owner_col = NULL`）：HR 只能看自己部门（及下级）的考勤，
//! `ALL` / 超管全看。锚点用 [`RECORD_TARGET`]（= `scope::ScopeTarget::BIZ_ATTENDANCE_RECORD`，
//! 已在 `SCOPED_TABLES` 里登记，别名与本文件 `ROW_SQL` 的 `r` 必须一致）。
//!
//! # 「锁定期间不可改」为什么在库层也守一道
//!
//! [`UPDATE_SQL`] 带 `AND NOT EXISTS (SELECT 1 FROM biz_attendance_period p
//! WHERE p.id = r.period_id AND p.status = 'LOCKED')`。handler 已经先锁期间再判状态，
//! 但库层这份守卫是**最后一道**：以后谁再写第二个改考勤记录的入口（导入 upsert、
//! 请假同步），忘了判期间状态也改不动锁定期间 —— 与 `attendance_period_model::RECALC_SQL`
//! 的 `p.status <> 'LOCKED'` 同一取向（设计 §4.2.3）。
//!
//! # 姓名口径（AGENTS.md 约定 13）
//!
//! - `employee_no` / `employee_name`：**读快照列**（工资按快照出表，改名字不影响历史工资）；
//! - `dept_name`：**实时 join** `sys_dept`（本表只存 `dept_id`，没存部门名快照）；
//! - `updated_by_name`：实时 join `sys_user`（审计轨迹在 `sys_change_log`）。
//!
//! 三个 join 都在同一条 SQL 里，列表一次查完 —— 不按行查（防 N+1）。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::attendance_period_dto::{STATUS_LOCKED, status_label};
use crate::business::hr::attendance_record_dto::{
    AttendanceRecordRes, RecordFilter, RecordNumbers, RecordPatch, source_label,
};
use crate::business::hr::model::{SENTINEL_ORG_ID, TABLE_ATTENDANCE_RECORD};
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;
use crate::shared::error_code;
use crate::utils;

/// 表名（与 [`TABLE_ATTENDANCE_RECORD`] 同一常量，这里只是本文件的可读别名）。
pub const TABLE: &str = TABLE_ATTENDANCE_RECORD;

/// 唯一索引名（`V0383`：`(period_id, employee_id) WHERE deleted_at IS NULL`）。
pub const UK_RECORD: &str = "uk_biz_att_rec";

/// 本表的 2 个 `CHECK` 约束（`V0383` 的 `ck_biz_att_rec_*`）。
///
/// 列出来是为了让 [`check_message`] 的映射**可被单测逐条对拍**：漏一条的后果是用户看到
/// 「考勤记录数据不满足库约束 ck_…」这种运维语言，而不是「天数不能是负数」。
pub const RECORD_CHECKS: [&str; 2] = ["ck_biz_att_rec_nonneg", "ck_biz_att_rec_source"];

/// 台账分页锚点（设计 §4.7：`dept_col = dept_id`、`owner_col = NULL`）。
///
/// 别名取 `r`（[`ROW_SQL`] 用 `FROM biz_attendance_record r`），两处**必须**一致，
/// 否则片段会拼出 `r.dept_id` 之外的别名、直接报列不存在。
/// 直接复用 `shared/scope.rs` 的常量而不是本地 `new(…)` 一份：两处各写一份会让
/// 「登记的表」与「分页用的锚点」悄悄漂移（列名改一处忘另一处，数据权限静默失效）。
pub const RECORD_TARGET: scope::ScopeTarget = scope::ScopeTarget::BIZ_ATTENDANCE_RECORD;

// ==================== 错误翻译 ====================

/// 唯一键冲突 → 用户文案（这个人这一期已经有行了）。
///
/// 处置只有一条：**去改那一行**（台账上按工号搜出来改数字），而不是「再插一条」——
/// 一人一月两行会让期间汇总翻倍、工资按两倍算。
pub fn duplicate_error(employee_no: &str, period_code: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::RESOURCE_CONFLICT,
        format!(
            "工号 {employee_no} 在考勤期间「{period_code}」已经有考勤记录了（一人一月一行）：\
             请在台账上找到这一行**修正数字**，不要重复导入/新建"
        ),
    )
}

/// `CHECK` 约束 → 用户可读文案（`ck_biz_att_rec_*`，见 `V0383`）。
pub fn check_message(constraint: Option<&str>) -> String {
    let name = constraint.unwrap_or_default();
    let human = match name {
        "ck_biz_att_rec_nonneg" => "考勤数据不能是负数：天数 / 加班小时 / 迟到早退次数都必须是 0 或正数",
        "ck_biz_att_rec_source" => "考勤来源只能是 IMPORT（导入）/ MANUAL（手工）/ LEAVE_SYNC（请假同步）",
        _ => return format!("考勤记录数据不满足库约束 {name}，请检查必填项与取值范围后重试"),
    };
    human.to_string()
}

/// 写路径的**唯一**数据库错误出口。
///
/// | 场景 | HTTP | 业务码 |
/// |---|---|---|
/// | `(period_id, employee_id)` 重复（`uk_biz_att_rec`） | 409 | `10003`（报文点名工号与期间） |
/// | `CHECK` 违反 | 400 | `10001`（报文点名该改哪个字段） |
/// | 其它 | 500 | `10004` |
pub fn write_error(err: DbError, ctx: &str, employee_no: &str, period_code: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => {
            let name = constraint.as_deref().unwrap_or_default().trim();
            if name.eq_ignore_ascii_case(UK_RECORD) {
                duplicate_error(employee_no, period_code)
            } else {
                ApiError::new(
                    409,
                    error_code::RESOURCE_CONFLICT,
                    format!("考勤记录写入冲突（唯一约束 {name}）：该值已存在于另一条未删除的记录上，请刷新后重试"),
                )
            }
        }
        DbError::CheckViolation { constraint, .. } => {
            ApiError::new(400, error_code::PARAM_INVALID, check_message(constraint.as_deref()))
        }
        _ => utils::db_error(err, ctx),
    }
}

/// 「考勤记录不存在或已删除」的 404。
pub fn not_found_error() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "考勤记录不存在或已删除")
}

// ==================== 行 ====================

/// 考勤记录全字段行（含实时 join 出来的 `dept_name` / `period_code` / `period_status`）。
#[derive(Debug, Clone)]
pub struct AttendanceRecordRow {
    /// 主键。
    pub id: Uuid,
    /// 所属考勤期间。
    pub period_id: Uuid,
    /// 考勤月 `yyyy-MM`（join 期间头）。
    pub period_code: String,
    /// 期间状态码值（join 期间头；台账「能不能改」由它决定）。
    pub period_status: String,
    /// 员工 id。
    pub employee_id: Uuid,
    /// 员工工号（**快照列**）。
    pub employee_no: String,
    /// 员工姓名（**快照列**）。
    pub employee_name: String,
    /// 部门 id（记录上的部门快照）。
    pub dept_id: Option<Uuid>,
    /// 部门名（实时 join `sys_dept`；空串 = join 不到，投影成 `None`）。
    pub dept_name: Option<String>,
    /// 11 个数值列。
    pub numbers: RecordNumbers,
    /// 来源码值。
    pub source: String,
    /// 备注。
    pub remark: Option<String>,
    /// 最后修改时间。
    pub updated_at: DateTime<Utc>,
    /// 最后修改人。
    pub update_by: Option<Uuid>,
    /// 最后修改人姓名（实时 join `sys_user`）。
    pub updated_by_name: Option<String>,
}

impl AttendanceRecordRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            period_id: row.get_typed("period_id")?,
            period_code: row.get_typed("period_code")?,
            period_status: row.get_typed("period_status")?,
            employee_id: row.get_typed("employee_id")?,
            employee_no: row.get_typed("employee_no")?,
            employee_name: row.get_typed("employee_name")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: clean(row.get_typed("dept_name")?),
            numbers: RecordNumbers {
                should_days: row.get_typed("should_days")?,
                actual_days: row.get_typed("actual_days")?,
                leave_paid_days: row.get_typed("leave_paid_days")?,
                leave_unpaid_days: row.get_typed("leave_unpaid_days")?,
                sick_days: row.get_typed("sick_days")?,
                absent_days: row.get_typed("absent_days")?,
                ot_hours_workday: row.get_typed("ot_hours_workday")?,
                ot_hours_weekend: row.get_typed("ot_hours_weekend")?,
                ot_hours_holiday: row.get_typed("ot_hours_holiday")?,
                late_count: row.get_typed("late_count")?,
                early_leave_count: row.get_typed("early_leave_count")?,
            },
            source: row.get_typed("source")?,
            remark: clean(row.get_typed("remark")?),
            updated_at: row.get_typed("updated_at")?,
            update_by: row.get_typed("update_by")?,
            updated_by_name: clean(row.get_typed("updated_by_name")?),
        })
    }

    /// 这一行当前能不能改（期间没锁定才能改）。
    pub fn editable(&self) -> bool {
        self.period_status != STATUS_LOCKED
    }

    /// 投影成出参契约。
    pub fn to_res(&self) -> AttendanceRecordRes {
        AttendanceRecordRes {
            id: self.id.to_string(),
            period_id: self.period_id.to_string(),
            period_code: self.period_code.clone(),
            period_status: self.period_status.clone(),
            period_status_label: status_label(&self.period_status).to_string(),
            editable: self.editable(),
            employee_id: self.employee_id.to_string(),
            employee_no: self.employee_no.clone(),
            employee_name: self.employee_name.clone(),
            dept_id: self.dept_id.map(|id| id.to_string()),
            dept_name: self.dept_name.clone(),
            numbers: self.numbers,
            ot_hours_total: self.numbers.ot_hours_total(),
            source: self.source.clone(),
            source_label: source_label(&self.source).to_string(),
            remark: self.remark.clone(),
            updated_at: crate::business::hr::employee_dto::timestamp_cst(self.updated_at),
            updated_by: self.update_by.map(|id| id.to_string()),
            updated_by_name: self.updated_by_name.clone(),
        }
    }
}

/// 空串 → `None`（join 不到姓名 / 部门名时驱动给空串）。
fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 可空文本参数：空 → 空串，SQL 里包 `NULLIF(?::text,'')`。
fn opt_text(value: Option<&str>) -> Value {
    Value::Text(value.map(str::trim).unwrap_or_default().to_string())
}

/// `NUMERIC` 参数：走 `?::text::numeric`（见 `attendance_period_db_tests` 的驱动踩坑）。
fn num(value: Decimal) -> Value {
    Value::Text(value.to_string())
}

/// 可空 uuid 参数（SQL 里包 `NULLIF(?::text,'')::uuid`）。
fn opt_uuid(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

// ==================== SQL ====================

/// 单条投影：期间头（`p`）+ 部门名（`d`）+ 最后修改人（`u`）三个 join 一次查完。
///
/// `period_code` / `period_status` 一起带出来，是为了让台账行自己就知道「这一期锁没锁」
/// （出参的 `editable` 由它算），不必前端再查一次期间列表去配对。
const ROW_SQL: &str = "\
SELECT r.id, r.period_id, p.period_code, p.status AS period_status, \
       r.employee_id, r.employee_no, r.employee_name, r.dept_id, \
       COALESCE(d.dept_name, '') AS dept_name, \
       r.should_days, r.actual_days, r.leave_paid_days, r.leave_unpaid_days, \
       r.sick_days, r.absent_days, r.ot_hours_workday, r.ot_hours_weekend, r.ot_hours_holiday, \
       r.late_count, r.early_leave_count, r.source, r.remark, \
       r.updated_at, r.update_by, \
       COALESCE(NULLIF(u.real_name, ''), u.username, '') AS updated_by_name \
  FROM biz_attendance_record r \
  JOIN biz_attendance_period p ON p.id = r.period_id \
  LEFT JOIN sys_dept d ON d.id = r.dept_id AND d.deleted_at IS NULL \
  LEFT JOIN sys_user u ON u.id = r.update_by";

/// 建一行（22 个绑定参数）。
///
/// 调用方负责生成 `id`、并保证 `employee_no` / `employee_name` 是**当时的**快照
/// （从 `biz_employee` 读，不从请求里取）。目前只有 HR-21/22 的导入链路会调它 ——
/// 台账页不提供「新增一行」按钮（人是导入进来的，人录错了应当改导入源）。
pub const INSERT_SQL: &str = "\
INSERT INTO biz_attendance_record \
    (id, org_id, period_id, employee_id, employee_no, employee_name, dept_id, \
     should_days, actual_days, leave_paid_days, leave_unpaid_days, sick_days, absent_days, \
     ot_hours_workday, ot_hours_weekend, ot_hours_holiday, late_count, early_leave_count, \
     source, remark, create_by, update_by) \
 VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?, ?, NULLIF(?::text,'')::uuid, \
         ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
         ?::text::numeric, ?::text::numeric, \
         ?::text::numeric, ?::text::numeric, ?::text::numeric, ?, ?, \
         ?, NULLIF(?::text,''), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)";

/// 整行改写 11 个数值列 + `remark`（15 个绑定参数：11 + 备注 + `updated_at` + 操作人 + id）。
///
/// **不含**：`period_id` / `employee_id` / `employee_no` / `employee_name` / `dept_id` /
/// `source`（决定「这是谁、哪一期、从哪来」，见 DTO 模块文档）、`deleted_at`、`create_by`。
/// 单测 `update_sql_never_touches_guarded_columns` 钉住。
///
/// `AND NOT EXISTS (… p.status = 'LOCKED')` 是库层锁定守卫（模块文档「锁定期间不可改」）。
pub const UPDATE_SQL: &str = "\
UPDATE biz_attendance_record r SET \
    should_days = ?::text::numeric, actual_days = ?::text::numeric, \
    leave_paid_days = ?::text::numeric, leave_unpaid_days = ?::text::numeric, \
    sick_days = ?::text::numeric, absent_days = ?::text::numeric, \
    ot_hours_workday = ?::text::numeric, ot_hours_weekend = ?::text::numeric, \
    ot_hours_holiday = ?::text::numeric, \
    late_count = ?, early_leave_count = ?, remark = NULLIF(?::text,''), \
    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
  WHERE r.id = ?::uuid AND r.deleted_at IS NULL \
    AND NOT EXISTS (SELECT 1 FROM biz_attendance_period p \
                     WHERE p.id = r.period_id AND p.status = 'LOCKED')";

/// 变更留痕的 `before` 侧（库里当前值）。
pub fn diff_snapshot_before(row: &AttendanceRecordRow) -> serde_json::Value {
    diff_fields(&row.numbers, row.remark.as_deref())
}

/// 变更留痕的 `after` 侧（合并后的最终值）。
pub fn diff_snapshot_after(patch: &RecordPatch) -> serde_json::Value {
    diff_fields(&patch.numbers, patch.remark.as_deref())
}

/// diff 输入（键 = 列名，数值固定两位小数）。
///
/// # 为什么数值固定两位小数
///
/// 库读回来 `NUMERIC(6,2)` 是 `22.00`，前端回传 `22` —— 直接比字符串会天天写「假变更」
/// （`contract_model::amount_text` 的四位小数版本同理）。`Decimal` 的 `PartialEq`
/// 本身对 scale 不敏感，但 diff 是**落库的 JSON**，两侧文本必须同形，否则审计页上会
/// 显示「22.00 改成 22」这种不存在的变更。
fn diff_fields(numbers: &RecordNumbers, remark: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "should_days": decimal_text(numbers.should_days),
        "actual_days": decimal_text(numbers.actual_days),
        "leave_paid_days": decimal_text(numbers.leave_paid_days),
        "leave_unpaid_days": decimal_text(numbers.leave_unpaid_days),
        "sick_days": decimal_text(numbers.sick_days),
        "absent_days": decimal_text(numbers.absent_days),
        "ot_hours_workday": decimal_text(numbers.ot_hours_workday),
        "ot_hours_weekend": decimal_text(numbers.ot_hours_weekend),
        "ot_hours_holiday": decimal_text(numbers.ot_hours_holiday),
        "late_count": numbers.late_count,
        "early_leave_count": numbers.early_leave_count,
        "remark": remark,
    })
}

/// 两位小数的文本形态（与 `NUMERIC(6,2)` / `NUMERIC(8,2)` 同精度）。
fn decimal_text(value: Decimal) -> String {
    format!("{value:.2}")
}

/// 本表的审计白名单（与 `changelog::diff::BIZ_ATTENDANCE_RECORD` **逐字一致**）。
///
/// 单测 `diff_fields_match_changelog_whitelist` 双向对拍：白名单多的列永远不会有日志、
/// 少的列会静默丢审计。
pub const DIFF_FIELDS: [&str; 12] = [
    "should_days",
    "actual_days",
    "leave_paid_days",
    "leave_unpaid_days",
    "sick_days",
    "absent_days",
    "ot_hours_workday",
    "ot_hours_weekend",
    "ot_hours_holiday",
    "late_count",
    "early_leave_count",
    "remark",
];

// ==================== 写操作 ====================

/// 建一行考勤记录的入参（收进结构体：避开 `clippy::too_many_arguments`，也让导入链路
/// HR-21 调用时读起来是「谁、哪一期、多少天」而不是十个位置参数）。
///
/// `employee_no` / `employee_name` 是**快照**：调用方从 `biz_employee` 读当时的工号姓名，
/// 不从请求里取（否则改个名字连历史考勤都变，工资按快照出表就错了）。
pub struct NewRecord<'a> {
    /// 所属考勤期间。
    pub period_id: Uuid,
    /// 员工 id。
    pub employee_id: Uuid,
    /// 工号快照。
    pub employee_no: &'a str,
    /// 姓名快照。
    pub employee_name: &'a str,
    /// 归属部门（可空：记录上没有部门时凭证 `aux_dept_id` 用空值）。
    pub dept_id: Option<Uuid>,
    /// 11 个数值列。
    pub numbers: &'a RecordNumbers,
    /// 来源（`IMPORT` / `MANUAL` / `LEAVE_SYNC`）。
    pub source: &'a str,
    /// 备注。
    pub remark: Option<&'a str>,
    /// 操作人。
    pub operator: Option<Uuid>,
}

/// 建一行考勤记录（导入 / 请假同步用）。返回受影响行数。
///
/// 撞 `uk_biz_att_rec` → 由调用方用 [`write_error`] 翻成 409（需要工号与期间码做报文）。
pub async fn insert(ex: &impl SqlExecutor, id: Uuid, rec: &NewRecord<'_>) -> DbResult<u64> {
    let numbers = rec.numbers;
    let operator = rec.operator;
    // SCOPE-EXEMPT: 业务写入；数据范围在读取侧（page）注入，写侧由 handler 的权限码把关。
    ex.exec_sql(
        INSERT_SQL.to_string(),
        vec![
            Value::from(&id),
            Value::from(&SENTINEL_ORG_ID),
            Value::from(&rec.period_id),
            Value::from(&rec.employee_id),
            Value::Text(rec.employee_no.to_string()),
            Value::Text(rec.employee_name.to_string()),
            opt_uuid(rec.dept_id),
            num(numbers.should_days),
            num(numbers.actual_days),
            num(numbers.leave_paid_days),
            num(numbers.leave_unpaid_days),
            num(numbers.sick_days),
            num(numbers.absent_days),
            num(numbers.ot_hours_workday),
            num(numbers.ot_hours_weekend),
            num(numbers.ot_hours_holiday),
            Value::from(numbers.late_count),
            Value::from(numbers.early_leave_count),
            Value::Text(rec.source.to_string()),
            opt_text(rec.remark),
            opt_uuid(operator),
            opt_uuid(operator),
        ],
    )
    .await
}

/// 整行改写（单行修正）。返回受影响行数：
///
/// - `0` = 记录不存在 / 已软删 / **期间已锁定**（库层守卫）；
/// - `1` = 改成功。
///
/// 三种 `0` 的区分靠调用方：它先 `FOR UPDATE` 读过这一行与期间状态，所以
/// 走到这里还是 `0` 只可能是「期间在这几毫秒里被锁定」→ 报并发冲突让人刷新。
pub async fn update(ex: &impl SqlExecutor, id: Uuid, patch: &RecordPatch, operator: Option<Uuid>) -> DbResult<u64> {
    let numbers = &patch.numbers;
    // SCOPE-EXEMPT: 业务写入；可见性由 handler 的权限码 + 期间守卫把关。
    ex.exec_sql(
        UPDATE_SQL.to_string(),
        vec![
            num(numbers.should_days),
            num(numbers.actual_days),
            num(numbers.leave_paid_days),
            num(numbers.leave_unpaid_days),
            num(numbers.sick_days),
            num(numbers.absent_days),
            num(numbers.ot_hours_workday),
            num(numbers.ot_hours_weekend),
            num(numbers.ot_hours_holiday),
            Value::from(numbers.late_count),
            Value::from(numbers.early_leave_count),
            opt_text(patch.remark.as_deref()),
            Value::from(Utc::now()),
            opt_uuid(operator),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 读操作 ====================

/// 按 id 取单条（不含已软删）。
pub async fn find_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<AttendanceRecordRow>> {
    let sql = format!("{ROW_SQL} WHERE r.id = ?::uuid AND r.deleted_at IS NULL");
    let row = ex.query_first_sql(sql, vec![Value::from(&id)]).await?;
    row.as_ref().map(AttendanceRecordRow::from_row).transpose()
}

/// 按 id 取单条并加行锁（`FOR UPDATE OF r`）：单行修正前先锁，两个 HR 同时改同一行时
/// 第二个会排队而不是各读一份旧值再各写一次（后写覆盖先写会丢掉一次修正，且两次 diff
/// 会各自记一条「从旧值变成自己的新值」的日志，审计上看不出谁最后生效）。
pub async fn lock_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<AttendanceRecordRow>> {
    let sql = format!("{ROW_SQL} WHERE r.id = ?::uuid AND r.deleted_at IS NULL FOR UPDATE OF r");
    let row = ex.query_first_sql(sql, vec![Value::from(&id)]).await?;
    row.as_ref().map(AttendanceRecordRow::from_row).transpose()
}

/// 台账分页（设计 §5.2 #21）：`period_id` 必填 + `dept_id` / `keyword` 可选，
/// 数据权限走 [`RECORD_TARGET`]（部门范围）。
///
/// # 返回值
///
/// `(列表, 总数)`；总数是**过滤 + 数据权限之后**的条数（`scope::paginate` 内部算）。
pub async fn page(
    filter: &RecordFilter,
    page: u64,
    page_size: u64,
    principal: &ScopedPrincipal,
) -> DbResult<(Vec<AttendanceRecordRow>, i64)> {
    let (sql, params) = list_query(filter);
    let result = scope::paginate(
        RECORD_TARGET,
        &sql,
        params,
        Some(LIST_ORDER_BY),
        page,
        page_size,
        principal,
    )
    .await?;
    let mut out = Vec::with_capacity(result.list.len());
    for row in &result.list {
        out.push(AttendanceRecordRow::from_row(row)?);
    }
    Ok((out, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 台账排序：工号（快照列）升序 —— HR 核对时按工号一列一列对下来最顺手；
/// 工号相同（理论上不会，唯一键在员工上）再按 id 保证顺序稳定（翻页不重复/不漏行）。
pub const LIST_ORDER_BY: &str = "ORDER BY r.employee_no ASC, r.id ASC";

/// 拼台账 SQL 与参数（**纯函数**：不连库就能验证条件个数与占位符个数一致）。
///
/// `{FILTER}` 是数据权限注入位（[`scope::FILTER_MARKER`]），必须出现在业务条件**之前**：
/// `scope::paginate` 会把片段参数前置，顺序才不错位。
fn list_query(filter: &RecordFilter) -> (String, Vec<Value>) {
    let mut sql = format!("{ROW_SQL} WHERE r.deleted_at IS NULL {}", scope::FILTER_MARKER);
    let mut params: Vec<Value> = Vec::new();

    // `period_id` 必填（DTO 已保证非空）：台账永远是「某一期」的台账，跨期间看没有意义
    // （同一人不同期间的数不可比，且期间状态不同、能不能改也不同）。
    sql.push_str(" AND r.period_id = ?::uuid");
    params.push(Value::from(&filter.period_id));

    if let Some(dept_id) = filter.dept_id {
        sql.push_str(" AND r.dept_id = ?::uuid");
        params.push(Value::from(&dept_id));
    }
    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        // 工号 / 姓名都是**快照列**：搜的是「当时那个人」，与台账显示一致
        sql.push_str(" AND (r.employee_no LIKE ? OR r.employee_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    (sql, params)
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// 建表迁移原文（编译期嵌入）：约束名 / 唯一索引名改了就红。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    fn zero_numbers() -> RecordNumbers {
        RecordNumbers {
            should_days: Decimal::ZERO,
            actual_days: Decimal::ZERO,
            leave_paid_days: Decimal::ZERO,
            leave_unpaid_days: Decimal::ZERO,
            sick_days: Decimal::ZERO,
            absent_days: Decimal::ZERO,
            ot_hours_workday: Decimal::ZERO,
            ot_hours_weekend: Decimal::ZERO,
            ot_hours_holiday: Decimal::ZERO,
            late_count: 0,
            early_leave_count: 0,
        }
    }

    fn sample_patch() -> RecordPatch {
        RecordPatch {
            numbers: RecordNumbers {
                should_days: Decimal::new(2200, 2),
                actual_days: Decimal::new(2000, 2),
                leave_paid_days: Decimal::ZERO,
                leave_unpaid_days: Decimal::new(200, 2),
                sick_days: Decimal::ZERO,
                absent_days: Decimal::ZERO,
                ot_hours_workday: Decimal::new(1000, 2),
                ot_hours_weekend: Decimal::new(400, 2),
                ot_hours_holiday: Decimal::ZERO,
                late_count: 1,
                early_leave_count: 0,
            },
            remark: Some("补录半天事假".to_string()),
            changed: true,
        }
    }

    /// 每个 SQL 常量的 `?` 个数必须等于实际绑定的参数个数。
    ///
    /// 这类错（多一个 / 少一个占位符）不会编译报错，只会在真跑时抛
    /// 「bind message supplies N parameters」—— 而那条路径可能只在某个分支里走到。
    #[test]
    fn sql_placeholder_counts_match_params() {
        let numbers = zero_numbers();
        let patch = sample_patch();
        let id = Uuid::nil();
        let operator = Some(Uuid::nil());

        let insert_params = vec![
            Value::from(&id),
            Value::from(&SENTINEL_ORG_ID),
            Value::from(&id),
            Value::from(&id),
            Value::Text("EMP-0001".to_string()),
            Value::Text("张三".to_string()),
            opt_uuid(Some(id)),
            num(numbers.should_days),
            num(numbers.actual_days),
            num(numbers.leave_paid_days),
            num(numbers.leave_unpaid_days),
            num(numbers.sick_days),
            num(numbers.absent_days),
            num(numbers.ot_hours_workday),
            num(numbers.ot_hours_weekend),
            num(numbers.ot_hours_holiday),
            Value::from(numbers.late_count),
            Value::from(numbers.early_leave_count),
            Value::Text("IMPORT".to_string()),
            opt_text(Some("备注")),
            opt_uuid(operator),
            opt_uuid(operator),
        ];
        assert_eq!(INSERT_SQL.matches('?').count(), insert_params.len(), "INSERT_SQL");

        let update_params = vec![
            num(numbers.should_days),
            num(numbers.actual_days),
            num(numbers.leave_paid_days),
            num(numbers.leave_unpaid_days),
            num(numbers.sick_days),
            num(numbers.absent_days),
            num(numbers.ot_hours_workday),
            num(numbers.ot_hours_weekend),
            num(numbers.ot_hours_holiday),
            Value::from(numbers.late_count),
            Value::from(numbers.early_leave_count),
            opt_text(patch.remark.as_deref()),
            Value::from(Utc::now()),
            opt_uuid(operator),
            Value::from(&id),
        ];
        assert_eq!(UPDATE_SQL.matches('?').count(), update_params.len(), "UPDATE_SQL");
    }

    /// 改路径**不许**碰「这是谁 / 哪一期 / 从哪来」与审计列。
    ///
    /// 改 `employee_id` 让这一行变成另一人的考勤（一人一月两行 / 别人少一行）；
    /// 改 `period_id` 让记录跨期（期间汇总两边都不对）；改 `source` 抹掉导入留痕。
    #[test]
    fn update_sql_never_touches_guarded_columns() {
        for column in [
            "period_id",
            "employee_id",
            "employee_no",
            "employee_name",
            "dept_id",
            "source",
            "org_id",
            "created_at",
            "create_by",
            "deleted_at",
        ] {
            assert!(
                !UPDATE_SQL.contains(&format!("{column} =")),
                "UPDATE_SQL 不得写 {column}（可写列只有 11 个数值列 + remark + 审计列）"
            );
        }
        for column in DIFF_FIELDS {
            assert!(
                UPDATE_SQL.contains(&format!("{column} =")),
                "UPDATE_SQL 漏了可写列 {column}"
            );
        }
        // 库层锁定守卫（设计 §4.2.3：LOCKED 之后记录不许改）
        assert!(
            UPDATE_SQL.contains("p.status = 'LOCKED'"),
            "整行改写必须带「期间未锁定」守卫"
        );
    }

    /// `CHECK` 名清单与建表迁移逐字一致（改迁移要同步这里，否则冲突翻译会退化）。
    #[test]
    fn check_names_match_migration() {
        for name in RECORD_CHECKS {
            assert!(V0383_DDL.contains(name), "V0383 里没有约束 {name}");
            assert!(check_message(Some(name)).contains("考勤"), "{name} 的文案不像人话");
        }
        assert!(V0383_DDL.contains(UK_RECORD), "唯一索引名与迁移不一致：{UK_RECORD}");
    }

    /// diff 快照的键集合与 `changelog::diff::BIZ_ATTENDANCE_RECORD` 的白名单**双向相等**。
    ///
    /// 少一列 = 那列改了不留痕（审计空洞）；多一列 = 登记了却永远不会有日志
    /// （审计页显示「这表登记了但零记录」，误导）。
    #[test]
    fn diff_fields_match_changelog_whitelist() {
        let spec = crate::business::changelog::diff::spec(TABLE).expect("biz_attendance_record 必须已登记");
        let whitelist: BTreeSet<&str> = spec.fields.iter().copied().collect();
        let mine: BTreeSet<&str> = DIFF_FIELDS.iter().copied().collect();
        assert_eq!(whitelist, mine, "登记白名单与本文件的 DIFF_FIELDS 必须逐字相等");

        // 快照键集合 == 白名单（`json!` 字面量里漏一列会在这里被抓到）
        let snapshot = diff_snapshot_after(&sample_patch());
        let keys: BTreeSet<&str> = snapshot
            .as_object()
            .expect("diff 输入必须是对象")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, mine, "diff 快照的键必须与白名单一致");
    }

    /// 数值进 diff 前统一两位小数：`22` 与 `22.00` 不能变成「假变更」。
    #[test]
    fn diff_values_use_fixed_two_decimals() {
        let mut patch = sample_patch();
        patch.numbers.should_days = Decimal::new(22, 0);
        patch.numbers.ot_hours_workday = Decimal::new(1055, 2);
        let snapshot = diff_snapshot_after(&patch);
        assert_eq!(snapshot["should_days"], serde_json::json!("22.00"));
        assert_eq!(snapshot["ot_hours_workday"], serde_json::json!("10.55"));
        assert_eq!(snapshot["late_count"], serde_json::json!(1));
        assert_eq!(snapshot["remark"], serde_json::json!("补录半天事假"));

        // 同一个数两种 scale 进 diff 后文本相同（否则审计页会显示不存在的变更）
        let mut same = patch.clone();
        same.numbers.should_days = Decimal::new(2200, 2);
        assert_eq!(
            diff_snapshot_after(&patch)["should_days"],
            diff_snapshot_after(&same)["should_days"]
        );
    }

    /// 台账 SQL：`{FILTER}` 必须在业务条件之前（`scope::paginate` 会把片段参数前置），
    /// 且 `period_id` 恒在其中（一人一期一行的前提）。
    #[test]
    fn list_query_puts_filter_marker_first() {
        let filter = RecordFilter {
            period_id: Uuid::nil(),
            dept_id: Some(Uuid::nil()),
            keyword: Some("张".to_string()),
        };
        let (sql, params) = list_query(&filter);
        let marker = sql.find(scope::FILTER_MARKER).expect("必须有 {FILTER}");
        let period = sql.find("r.period_id = ?::uuid").expect("必须有期间条件");
        assert!(marker < period, "{{FILTER}} 必须在业务条件之前");
        assert_eq!(sql.matches(scope::FILTER_MARKER).count(), 1, "标记只能出现一次");
        // 3 个业务参数：period_id + dept_id + 两个 LIKE（关键字两列）
        assert_eq!(params.len(), 4, "period + dept + 工号/姓名两个 LIKE");
        assert_eq!(
            sql.matches('?').count(),
            params.len(),
            "占位符只算业务条件；{{FILTER}} 的占位符由 scope::paginate 注入"
        );

        // 只给期间：1 个参数，关键字为空不产生条件
        let plain = RecordFilter {
            period_id: Uuid::nil(),
            dept_id: None,
            keyword: Some("   ".to_string()),
        };
        let (sql, params) = list_query(&plain);
        assert_eq!(params.len(), 1);
        assert!(!sql.contains("LIKE"), "空白关键字不该产生 LIKE 条件");
    }
}
