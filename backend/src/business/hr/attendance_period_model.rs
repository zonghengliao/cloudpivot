//! 考勤期间数据层（`biz_attendance_period` · HR-19）。
//!
//! 本文件是考勤期间**唯一**的 SQL 出口：建 / 读 / 改 / 状态 CAS 回写 / 软删 / 分页 /
//! **汇总重算**。契约在 [`crate::business::hr::attendance_period_dto`]，接口层在
//! [`crate::business::hr::attendance_period_handler`]。
//!
//! # 表结构的唯一依据
//!
//! `backend/migrations/business/V0383__hr_tables.up.sql` 的 `biz_attendance_period`（设计 §3.1 ④）。
//! 列名 / `CHECK` 名 / 唯一索引名逐字取自该迁移；**迁移已应用且不可改**（AGENTS.md 约定 6），
//! 本卡零迁移。
//!
//! # 汇总重算为什么是「全量覆盖」而不是「增量累加」
//!
//! `employee_count / should_days_total / actual_days_total / ot_hours_total` 是**快照列**
//! （设计 §3.1 ④ 的列注释：「汇总快照（导入/修改后重算）」）。重算 = 从本期间的
//! `biz_attendance_record` 未删除行**重新算一遍再覆盖**：
//!
//! ```text
//! UPDATE biz_attendance_period p SET …
//!   FROM (SELECT count(*), SUM(should_days), SUM(actual_days), SUM(三个加班列)
//!           FROM biz_attendance_record WHERE period_id = $1 AND deleted_at IS NULL) s
//!  WHERE p.id = $1 AND p.deleted_at IS NULL AND p.status <> 'LOCKED'
//! ```
//!
//! 全量覆盖**天生幂等**：同一个人再点一次「重算」，读到的记录集相同 ⇒ 写入值相同 ⇒
//! `changed = false`（接口层把这个标记回给前端，用户能看见「这次没变」）。
//! 增量累加（`SET should_days_total = should_days_total + Σ…`）则会「重算两次翻倍」，
//! 而且导入失败重试 / 单行修正之后**永远收敛不回真值** —— 这类脏数据只能改库修，
//! 正是 AGENTS.md 约定 11 那类「必须工程师改库才能恢复」的死局。
//!
//! # 锁定为什么连重算都拦
//!
//! 设计 §4.2.3：「`LOCKED` 之后**记录不许增删改**」，锁定期间是**薪资试算的稳定基线**。
//! 四个汇总列是这个基线的组成部分：允许对 `LOCKED` 期间重算，等于允许「考勤记录没变、
//! 工资依据变了」。所以 [`recalc_totals`] 的 SQL 里带 `p.status <> 'LOCKED'`（守卫在库层，
//! 不只靠 handler 判），必要时先解锁（要原因，写 `biz_status_log`）再重算。
//!
//! # 与 `sys_user` 的两次 join
//!
//! 出参要给 `submitted_by_name` / `locked_by_name`（AGENTS.md 约定 13：凡 `*_id` 必带 `*_name`）。
//! 这两列是**审计类**姓名，所以取姓名快照的替代做法（实时 join）在这里也是对的：
//! 查的是「现在这个人叫什么」，改名的历史由 `biz_status_log.operator_name` 负责。
//! join 只是投影，不参与数据权限（本表没有部门 / 归属人列，设计 §4.7 的登记表清单里
//! 只有 `biz_attendance_record` 是范围表）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::attendance_period_dto::{
    AttendancePeriodRes, PeriodCreate, PeriodFilter, PeriodPatch, PeriodTotals, STATUS_SUBMITTED, status_label,
};
use crate::business::hr::model::SENTINEL_ORG_ID;
use crate::business::shared::scope;
use crate::shared::error_code;
use crate::utils;

/// 唯一索引名（`V0383`：`(org_id, period_code) WHERE deleted_at IS NULL`）。
pub const UK_PERIOD: &str = "uk_biz_att_period";

/// 本表的 4 个 `CHECK` 约束（`V0383` 的 `ck_biz_att_period_*`）。
///
/// 列出来是为了让 [`check_message`] 的映射**可被单测逐条对拍**：漏一条的后果是用户看到
/// 「考勤期间数据不满足库约束 ck_…」这种运维语言，而不是「月份只能是 01~12」。
pub const PERIOD_CHECKS: [&str; 4] = [
    "ck_biz_att_period_code",
    "ck_biz_att_period_range",
    "ck_biz_att_period_status",
    "ck_biz_att_period_counts",
];

// ==================== 错误翻译 ====================

/// 唯一键冲突 → 用户文案。
///
/// 期间是「一公司一月一批」，撞码的处置只有一条：**去看那一期**（改它的日期、或改本期的月份）。
pub fn duplicate_error(period_code: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::HR_ATTENDANCE_PERIOD_DUPLICATE,
        format!(
            "考勤期间「{period_code}」已存在（一个月只能有一批）：\
             请直接打开那一期导入考勤；若那一期建错了月份，先删掉它（仅草稿可删）再重建"
        ),
    )
}

/// `CHECK` 约束 → 用户可读文案（`ck_biz_att_period_*`，见 `V0383`）。
pub fn check_message(constraint: Option<&str>) -> String {
    let name = constraint.unwrap_or_default();
    let human = match name {
        "ck_biz_att_period_code" => "考勤月必须是 yyyy-MM 且月份在 01~12 之间（如 `2026-11`）",
        "ck_biz_att_period_range" => "考勤期间的结束日不能早于开始日",
        "ck_biz_att_period_status" => "考勤期间状态只能是 DRAFT / SUBMITTED / LOCKED",
        "ck_biz_att_period_counts" => "汇总快照（人数 / 天数 / 小时）不能是负数",
        _ => return format!("考勤期间数据不满足库约束 {name}，请检查必填项与取值范围后重试"),
    };
    human.to_string()
}

/// 写路径的**唯一**数据库错误出口。
///
/// | 场景 | HTTP | 业务码 |
/// |---|---|---|
/// | `period_code` 重复（`uk_biz_att_period`） | 409 | `68009` |
/// | `CHECK` 违反 | 400 | `10001`（报文点名该改哪个字段） |
/// | 其它 | 500 | `10004` |
pub fn write_error(err: DbError, ctx: &str, period_code: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => {
            let name = constraint.as_deref().unwrap_or_default().trim();
            if name.eq_ignore_ascii_case(UK_PERIOD) {
                duplicate_error(period_code)
            } else {
                ApiError::new(
                    409,
                    error_code::RESOURCE_CONFLICT,
                    format!("考勤期间写入冲突（唯一约束 {name}）：该值已存在于另一条未删除的记录上，请刷新后重试"),
                )
            }
        }
        DbError::CheckViolation { constraint, .. } => {
            ApiError::new(400, error_code::PARAM_INVALID, check_message(constraint.as_deref()))
        }
        _ => utils::db_error(err, ctx),
    }
}

/// 「考勤期间不存在或已删除」的 404（接口层按 id 取数的入口共用一句）。
pub fn not_found_error() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "考勤期间不存在或已删除")
}

// ==================== 行 ====================

/// 考勤期间全字段行（含两次 `sys_user` join 出来的姓名）。
#[derive(Debug, Clone)]
pub struct AttendancePeriodRow {
    /// 主键。
    pub id: Uuid,
    /// 考勤月 `yyyy-MM`。
    pub period_code: String,
    /// 起（含）。
    pub period_start: NaiveDate,
    /// 止（含）。
    pub period_end: NaiveDate,
    /// 状态码值。
    pub status: String,
    /// 期间内记录条数（汇总快照）。
    pub employee_count: i32,
    /// 应出勤天数合计。
    pub should_days_total: Decimal,
    /// 实际出勤天数合计。
    pub actual_days_total: Decimal,
    /// 加班小时合计。
    pub ot_hours_total: Decimal,
    /// 最近一次导入任务 id。
    pub import_task_id: Option<Uuid>,
    /// 最近一次提交时间。
    pub submitted_at: Option<DateTime<Utc>>,
    /// 最近一次提交人。
    pub submitted_by: Option<Uuid>,
    /// 提交人姓名（join；空串 = join 不到，投影成 `None`）。
    pub submitted_by_name: Option<String>,
    /// 最近一次锁定时间。
    pub locked_at: Option<DateTime<Utc>>,
    /// 最近一次锁定人。
    pub locked_by: Option<Uuid>,
    /// 锁定人姓名。
    pub locked_by_name: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最后修改时间。
    pub updated_at: DateTime<Utc>,
}

impl AttendancePeriodRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            period_code: row.get_typed("period_code")?,
            period_start: row.get_typed("period_start")?,
            period_end: row.get_typed("period_end")?,
            status: row.get_typed("status")?,
            employee_count: row.get_typed("employee_count")?,
            should_days_total: row.get_typed("should_days_total")?,
            actual_days_total: row.get_typed("actual_days_total")?,
            ot_hours_total: row.get_typed("ot_hours_total")?,
            import_task_id: row.get_typed("import_task_id")?,
            submitted_at: row.get_typed("submitted_at")?,
            submitted_by: row.get_typed("submitted_by")?,
            submitted_by_name: clean(row.get_typed("submitted_by_name")?),
            locked_at: row.get_typed("locked_at")?,
            locked_by: row.get_typed("locked_by")?,
            locked_by_name: clean(row.get_typed("locked_by_name")?),
            remark: clean(row.get_typed("remark")?),
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 四个汇总快照列。
    pub fn totals(&self) -> PeriodTotals {
        PeriodTotals {
            employee_count: self.employee_count,
            should_days_total: self.should_days_total,
            actual_days_total: self.actual_days_total,
            ot_hours_total: self.ot_hours_total,
        }
    }

    /// 是否已锁定（设计 §4.2.3：锁定后记录不许增删改，期间头也不许重算）。
    pub fn is_locked(&self) -> bool {
        self.status == crate::business::hr::attendance_period_dto::STATUS_LOCKED
    }

    /// 投影成出参契约。
    pub fn to_res(&self) -> AttendancePeriodRes {
        AttendancePeriodRes {
            id: self.id.to_string(),
            period_code: self.period_code.clone(),
            period_start: self.period_start,
            period_end: self.period_end,
            status_label: status_label(&self.status),
            locked: self.is_locked(),
            status: self.status.clone(),
            employee_count: self.employee_count,
            should_days_total: self.should_days_total,
            actual_days_total: self.actual_days_total,
            ot_hours_total: self.ot_hours_total,
            import_task_id: self.import_task_id.map(|id| id.to_string()),
            submitted_at: self.submitted_at.map(crate::business::hr::employee_dto::timestamp_cst),
            submitted_by: self.submitted_by.map(|id| id.to_string()),
            submitted_by_name: self.submitted_by_name.clone(),
            locked_at: self.locked_at.map(crate::business::hr::employee_dto::timestamp_cst),
            locked_by: self.locked_by.map(|id| id.to_string()),
            locked_by_name: self.locked_by_name.clone(),
            remark: self.remark.clone(),
            created_at: crate::business::hr::employee_dto::timestamp_cst(self.created_at),
            updated_at: crate::business::hr::employee_dto::timestamp_cst(self.updated_at),
        }
    }
}

/// 空串 → `None`（join 不到姓名时驱动给空串）。
fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 可空文本参数：空 → 空串，SQL 里包 `NULLIF(?::text,'')`。
fn opt_text(value: Option<&str>) -> Value {
    Value::Text(value.map(str::trim).unwrap_or_default().to_string())
}

/// 可空时间参数（SQL 里包 `NULLIF(?::text,'')::timestamptz`）。
fn opt_timestamp(value: Option<DateTime<Utc>>) -> Value {
    Value::Text(value.map(|at| at.to_rfc3339()).unwrap_or_default())
}

/// 可空 uuid 参数（SQL 里包 `NULLIF(?::text,'')::uuid`）。
fn opt_uuid(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

// ==================== SQL ====================

/// 单条投影（`submitted_by_name` / `locked_by_name` 走两次 `sys_user` 自连接，DTO 不再查库）。
///
/// 姓名取 `real_name`，为空回退登录名：审计列显示「空」比显示 uuid 更没用
/// （同 AGENTS.md 约定 13 的反例：附件表只存 `uploaded_by` → 界面显示 UUID）。
const ROW_SQL: &str = "\
SELECT p.id, p.period_code, p.period_start, p.period_end, p.status, p.employee_count, \
       p.should_days_total, p.actual_days_total, p.ot_hours_total, p.import_task_id, \
       p.submitted_at, p.submitted_by, \
       COALESCE(NULLIF(su.real_name, ''), su.username, '') AS submitted_by_name, \
       p.locked_at, p.locked_by, \
       COALESCE(NULLIF(lu.real_name, ''), lu.username, '') AS locked_by_name, \
       p.remark, p.created_at, p.updated_at \
  FROM biz_attendance_period p \
  LEFT JOIN sys_user su ON su.id = p.submitted_by \
  LEFT JOIN sys_user lu ON lu.id = p.locked_by";

/// 建期间（9 个绑定参数：7 列 + 两个审计列）。
///
/// 汇总四列**显式写 0**（不靠 DEFAULT）：建期那一刻一定没有记录，四个数必须是确定的 `0`
/// 而不是「列默认值是 0」这种隐式知识；`import_task_id` 等由 HR-21 / 状态流转回写。
pub const INSERT_SQL: &str = "\
INSERT INTO biz_attendance_period \
    (id, org_id, period_code, period_start, period_end, status, employee_count, \
     should_days_total, actual_days_total, ot_hours_total, remark, create_by, update_by) \
 VALUES (?::uuid, ?::uuid, ?, ?::text::date, ?::text::date, ?, 0, 0, 0, 0, \
         NULLIF(?::text,''), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)";

/// 改期间的**可写列**（6 个绑定参数：3 列 + `updated_at` + `update_by` + `id`）。
///
/// **不含** `period_code`（人工标识，改名的连带影响见 DTO 文档）/ `status`（走状态机）/
/// 四个汇总列（走 [`RECALC_SQL`]）/ `submitted_*` / `locked_*`（状态机回写）。
/// 单测 `update_sql_never_touches_guarded_columns` 钉住。
pub const UPDATE_SQL: &str = "\
UPDATE biz_attendance_period SET \
    period_start = ?::text::date, period_end = ?::text::date, remark = NULLIF(?::text,''), \
    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
  WHERE id = ?::uuid AND deleted_at IS NULL";

/// 状态 CAS 回写（9 个绑定参数；只能由 `shared::state_machine::transition` 之后调用）。
///
/// `submitted_at/by` 与 `locked_at/by` 都用 `COALESCE(NULLIF(…), 原值)`：目标状态是哪个，
/// 就只写那一对，另一对**原样保留**（审计信息不清空，见 DTO 模块文档）。
/// `WHERE status = ?` 是乐观锁：并发两次流转只有一次能改（后到的 `rows = 0` → 409）。
pub const SET_STATUS_SQL: &str = "\
UPDATE biz_attendance_period SET \
    status = ?, \
    submitted_at = COALESCE(NULLIF(?::text,'')::timestamptz, submitted_at), \
    submitted_by = COALESCE(NULLIF(?::text,'')::uuid, submitted_by), \
    locked_at = COALESCE(NULLIF(?::text,'')::timestamptz, locked_at), \
    locked_by = COALESCE(NULLIF(?::text,'')::uuid, locked_by), \
    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL";

/// 汇总重算（全量覆盖，4 个绑定参数：`updated_at` / `update_by` / `period_id` / `period_id`）。
///
/// 三个加班列加总进 `ot_hours_total`（设计 §3.1 ④ 只有一个合计列，明细按平时/休息日/
/// 节假日分三列存在记录表上）。`status <> 'LOCKED'` 是库层守卫，见模块文档。
pub const RECALC_SQL: &str = "\
UPDATE biz_attendance_period p SET \
    employee_count = s.cnt, \
    should_days_total = s.should_days, \
    actual_days_total = s.actual_days, \
    ot_hours_total = s.ot_hours, \
    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
  FROM ( \
    SELECT count(*)::int AS cnt, \
           COALESCE(SUM(r.should_days), 0) AS should_days, \
           COALESCE(SUM(r.actual_days), 0) AS actual_days, \
           COALESCE(SUM(r.ot_hours_workday + r.ot_hours_weekend + r.ot_hours_holiday), 0) AS ot_hours \
      FROM biz_attendance_record r \
     WHERE r.period_id = ?::uuid AND r.deleted_at IS NULL \
  ) s \
 WHERE p.id = ?::uuid AND p.deleted_at IS NULL AND p.status <> 'LOCKED'";

/// 软删（4 个绑定参数：`deleted_at` / `update_by` / `id` / `deleted_at` 幂等守卫）。
///
/// 只删草稿（调用方还要判「期间下没有记录」）：`biz_attendance_record.period_id` 是
/// `ON DELETE CASCADE`，硬删会连带抹掉考勤记录；软删保留数据，恢复只需把 `deleted_at` 置空。
pub const SOFT_DELETE_SQL: &str = "\
UPDATE biz_attendance_period SET deleted_at = ?, update_by = NULLIF(?::text,'')::uuid \
 WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL";

/// 期间内未删除的考勤记录条数（软删前置守卫）。
pub const COUNT_RECORDS_SQL: &str = "\
SELECT count(*)::bigint AS total FROM biz_attendance_record \
 WHERE period_id = ?::uuid AND deleted_at IS NULL";

// ==================== 写操作 ====================

/// 建期间。
///
/// 调用方负责生成 `id`（同事务里还要写 `biz_status_log`，日志要 `entity_id`）。
pub async fn insert(ex: &impl SqlExecutor, id: Uuid, draft: &PeriodCreate, operator: Option<Uuid>) -> DbResult<()> {
    // SCOPE-EXEMPT: 主数据写入；org_id 由 SENTINEL_ORG_ID 显式给出（多组织落地时改一处）。
    ex.exec_sql(
        INSERT_SQL.to_string(),
        vec![
            Value::from(&id),
            Value::from(&SENTINEL_ORG_ID),
            Value::Text(draft.period_code.clone()),
            Value::Text(draft.period_start.to_string()),
            Value::Text(draft.period_end.to_string()),
            Value::Text(crate::business::hr::attendance_period_dto::STATUS_DRAFT.to_string()),
            opt_text(draft.remark.as_deref()),
            opt_uuid(operator),
            opt_uuid(operator),
        ],
    )
    .await?;
    Ok(())
}

/// 改期间的可写列。返回受影响行数：`0` = 不存在 / 已软删。
pub async fn update(ex: &impl SqlExecutor, id: Uuid, patch: &PeriodPatch, operator: Option<Uuid>) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主数据写入。
    ex.exec_sql(
        UPDATE_SQL.to_string(),
        vec![
            Value::Text(patch.period_start.to_string()),
            Value::Text(patch.period_end.to_string()),
            opt_text(patch.remark.as_deref()),
            Value::from(Utc::now()),
            opt_uuid(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 状态 CAS 回写（必须紧跟 `state_machine::transition` 之后调用）。
///
/// 只有目标状态是 `SUBMITTED` 才写 `submitted_*`、是 `LOCKED` 才写 `locked_*`；
/// 解锁（→`DRAFT`）两对都不写（保留最近一次动作的时间与操作人）。
///
/// # 返回值
///
/// 受影响行数：`0` = 状态已被他人改过（并发保护，调用方报 409）。
pub async fn set_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from_status: &str,
    to_status: &str,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    let now = Utc::now();
    let submitted_at = (to_status == STATUS_SUBMITTED).then_some(now);
    let locked_at = (to_status == crate::business::hr::attendance_period_dto::STATUS_LOCKED).then_some(now);
    // SCOPE-EXEMPT: 主数据写入。
    ex.exec_sql(
        SET_STATUS_SQL.to_string(),
        vec![
            Value::Text(to_status.to_string()),
            opt_timestamp(submitted_at),
            opt_uuid(submitted_at.and(operator)),
            opt_timestamp(locked_at),
            opt_uuid(locked_at.and(operator)),
            Value::from(now),
            opt_uuid(operator),
            Value::from(&id),
            Value::Text(from_status.to_string()),
        ],
    )
    .await
}

/// 软删草稿期间。返回受影响行数：`0` = 不存在 / 已软删 / 已不是草稿。
pub async fn soft_delete(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    let now = Utc::now();
    // SCOPE-EXEMPT: 主数据写入。
    ex.exec_sql(
        SOFT_DELETE_SQL.to_string(),
        vec![Value::from(now), opt_uuid(operator), Value::from(&id)],
    )
    .await
}

/// 汇总重算：把四个快照列按期间内未删除的考勤记录**全量覆盖**。返回受影响行数（`0` = 已锁定 / 不存在）。
pub async fn recalc_totals(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主数据（汇总快照）写入。
    ex.exec_sql(
        RECALC_SQL.to_string(),
        vec![
            Value::from(Utc::now()),
            opt_uuid(operator),
            Value::from(&id),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 读操作 ====================

/// 按 id 取单条（不含已软删）。
pub async fn find_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<AttendancePeriodRow>> {
    let sql = format!("{ROW_SQL} WHERE p.id = ?::uuid AND p.deleted_at IS NULL");
    let row = ex.query_first_sql(sql, vec![Value::from(&id)]).await?;
    row.as_ref().map(AttendancePeriodRow::from_row).transpose()
}

/// 按 id 取单条并加行锁（`FOR UPDATE OF p`）：改 / 状态流转 / 重算 / 删除前先锁，
/// 两个并发请求不会各读一份旧状态再各写一次。
pub async fn lock_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<AttendancePeriodRow>> {
    let sql = format!("{ROW_SQL} WHERE p.id = ?::uuid AND p.deleted_at IS NULL FOR UPDATE OF p");
    let row = ex.query_first_sql(sql, vec![Value::from(&id)]).await?;
    row.as_ref().map(AttendancePeriodRow::from_row).transpose()
}

/// 期间内未删除的考勤记录条数（删期间的前置守卫；HR-22 也会用到）。
pub async fn count_records(ex: &impl SqlExecutor, period_id: Uuid) -> DbResult<i64> {
    let row = ex
        .query_first_sql(COUNT_RECORDS_SQL.to_string(), vec![Value::from(&period_id)])
        .await?;
    match row {
        Some(row) => row.get_typed::<i64>("total"),
        None => Ok(0),
    }
}

/// 期间列表分页（设计 §5.2 #18）。
///
/// # 为什么走 `scope::paginate_unscoped`
///
/// 本表没有部门 / 归属人列（设计 §4.7 的登记表清单里只有 `biz_attendance_record` 是范围表），
/// 所以是**显式声明**的不参与数据权限的表；分页仍走唯一入口（架构 §2.4 闸①），
/// 只是锚点是 `ScopeTarget::unscoped` —— 「不注入范围」这件事写在代码里，而不是绕过分页。
///
/// # 返回值
///
/// `(列表, 总数)`；总数是过滤后的条数。
pub async fn page(filter: &PeriodFilter, page: u64, page_size: u64) -> DbResult<(Vec<AttendancePeriodRow>, i64)> {
    let (sql, params) = list_query(filter);
    let result = scope::paginate_unscoped(
        scope::ScopeTarget::unscoped("biz_attendance_period"),
        &sql,
        params,
        Some("ORDER BY p.period_code DESC, p.id DESC"),
        page,
        page_size,
    )
    .await?;
    let mut out = Vec::with_capacity(result.list.len());
    for row in &result.list {
        out.push(AttendancePeriodRow::from_row(row)?);
    }
    Ok((out, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 拼列表 SQL 与参数（**纯函数**：不连库就能验证条件个数与占位符个数一致）。
///
/// 不含 `ORDER BY` / `LIMIT`：`scope::paginate_unscoped` 要求它们在参数里单独传。
fn list_query(filter: &PeriodFilter) -> (String, Vec<Value>) {
    let mut sql = format!("{ROW_SQL} WHERE p.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();

    let statuses: Vec<&str> = filter
        .statuses
        .iter()
        .map(|status| status.trim())
        .filter(|status| !status.is_empty())
        .collect();
    if !statuses.is_empty() {
        // 不用 `= ANY(?::text[])`：驱动对数组参数只有 text 编码器（见 `erp/serial_model.rs` 的踩坑）
        let holes = vec!["?"; statuses.len()].join(", ");
        sql.push_str(&format!(" AND p.status IN ({holes})"));
        for status in statuses {
            params.push(Value::Text(status.to_string()));
        }
    }
    if let Some(year) = filter.year {
        // `period_code` 是 `yyyy-MM`：前 4 位就是年份。用 `left(…,4)` 而非拼接 LIKE，
        // 免得年份里带 `%` / `_` 通配符（`year` 是 `i32`，其实不会，但口径统一）。
        sql.push_str(" AND left(p.period_code, 4) = ?");
        params.push(Value::Text(format!("{year:04}")));
    }
    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        sql.push_str(" AND p.period_code LIKE ?");
        params.push(Value::from(pattern));
    }
    (sql, params)
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 建表迁移原文（编译期嵌入）：约束名 / 唯一索引名改了就红。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    fn sample_create() -> PeriodCreate {
        PeriodCreate {
            period_code: "2026-11".to_string(),
            period_start: NaiveDate::parse_from_str("2026-10-26", "%Y-%m-%d").expect("合法日期"),
            period_end: NaiveDate::parse_from_str("2026-11-25", "%Y-%m-%d").expect("合法日期"),
            remark: Some("26-25 计薪周期".to_string()),
        }
    }

    /// 每个 SQL 常量的 `?` 个数必须等于实际绑定的参数个数。
    ///
    /// 这类错（多一个 / 少一个占位符）不会编译报错，只会在真跑时抛
    /// 「bind message supplies N parameters」—— 而那条路径可能只在某个分支里走到。
    #[test]
    fn sql_placeholder_counts_match_params() {
        let draft = sample_create();
        let patch = PeriodPatch {
            period_start: draft.period_start,
            period_end: draft.period_end,
            remark: None,
            changed: true,
        };
        let id = Uuid::nil();
        let operator = Some(Uuid::nil());
        let now = Utc::now();

        let insert_params = vec![
            Value::from(&id),
            Value::from(&SENTINEL_ORG_ID),
            Value::Text(draft.period_code.clone()),
            Value::Text(draft.period_start.to_string()),
            Value::Text(draft.period_end.to_string()),
            Value::Text(crate::business::hr::attendance_period_dto::STATUS_DRAFT.to_string()),
            opt_text(draft.remark.as_deref()),
            opt_uuid(operator),
            opt_uuid(operator),
        ];
        assert_eq!(INSERT_SQL.matches('?').count(), insert_params.len(), "INSERT_SQL");

        let update_params = [
            Value::Text(patch.period_start.to_string()),
            Value::Text(patch.period_end.to_string()),
            opt_text(patch.remark.as_deref()),
            Value::from(now),
            opt_uuid(operator),
            Value::from(&id),
        ];
        assert_eq!(UPDATE_SQL.matches('?').count(), update_params.len(), "UPDATE_SQL");

        let status_params = vec![
            Value::Text("SUBMITTED".to_string()),
            opt_timestamp(Some(now)),
            opt_uuid(operator),
            opt_timestamp(None),
            opt_uuid(None),
            Value::from(now),
            opt_uuid(operator),
            Value::from(&id),
            Value::Text("DRAFT".to_string()),
        ];
        assert_eq!(
            SET_STATUS_SQL.matches('?').count(),
            status_params.len(),
            "SET_STATUS_SQL"
        );

        let recalc_params = [Value::from(now), opt_uuid(operator), Value::from(&id), Value::from(&id)];
        assert_eq!(RECALC_SQL.matches('?').count(), recalc_params.len(), "RECALC_SQL");

        let delete_params = [Value::from(now), opt_uuid(operator), Value::from(&id)];
        assert_eq!(
            SOFT_DELETE_SQL.matches('?').count(),
            delete_params.len(),
            "SOFT_DELETE_SQL"
        );

        assert_eq!(COUNT_RECORDS_SQL.matches('?').count(), 1, "COUNT_RECORDS_SQL");
    }

    /// 改路径**不许**碰人工标识 / 状态 / 汇总快照 / 审计列。
    ///
    /// 三处都能改坏数据：改 `period_code` 让历史报表对不上；改 `status` 绕过状态机
    /// （留下「状态变了但没有 `biz_status_log`」）；改汇总列让「合计 = 各行之和」失效。
    #[test]
    fn update_sql_never_touches_guarded_columns() {
        for column in [
            "period_code",
            "status",
            "employee_count",
            "should_days_total",
            "actual_days_total",
            "ot_hours_total",
            "submitted_at",
            "submitted_by",
            "locked_at",
            "locked_by",
            "org_id",
            "deleted_at",
        ] {
            assert!(
                !UPDATE_SQL.contains(&format!("{column} =")),
                "UPDATE_SQL 不得写 {column}（可写列只有 period_start / period_end / remark + 审计列）"
            );
        }
        assert!(UPDATE_SQL.contains("period_start ="));
        assert!(UPDATE_SQL.contains("period_end ="));
        assert!(UPDATE_SQL.contains("remark ="));

        // 软删只对草稿生效（库层守卫，不只靠 handler）
        assert!(SOFT_DELETE_SQL.contains("status = 'DRAFT'"), "软删必须带草稿守卫");
        // 重算不许动已锁定期间（设计 §4.2.3 的基线）
        assert!(RECALC_SQL.contains("p.status <> 'LOCKED'"), "重算必须带锁定守卫");
    }

    /// `CHECK` 名清单与建表迁移逐字一致（改迁移要同步这里，否则冲突翻译会退化）。
    #[test]
    fn check_names_exist_in_migration() {
        for name in PERIOD_CHECKS {
            assert!(V0383_DDL.contains(name), "迁移里找不到约束 {name}");
        }
        assert!(
            V0383_DDL.contains(&format!("INDEX IF NOT EXISTS {UK_PERIOD}")),
            "唯一索引名"
        );
    }

    /// 重算 SQL 覆盖四个快照列，且加班合计是三个加班列之和（漏一列 → 加班费算少）。
    #[test]
    fn recalc_sql_covers_all_four_totals() {
        for column in [
            "employee_count =",
            "should_days_total =",
            "actual_days_total =",
            "ot_hours_total =",
        ] {
            assert!(RECALC_SQL.contains(column), "RECALC_SQL 少了 {column}");
        }
        for column in [
            "should_days",
            "actual_days",
            "ot_hours_workday",
            "ot_hours_weekend",
            "ot_hours_holiday",
        ] {
            assert!(RECALC_SQL.contains(column), "RECALC_SQL 少了记录列 {column}");
        }
        assert!(RECALC_SQL.contains("r.deleted_at IS NULL"), "软删的记录不许进汇总");
        assert!(RECALC_SQL.contains("p.deleted_at IS NULL"), "软删的期间不许被写");
    }

    /// 列表拼装：状态多选逐个占位、年份取前 4 位、关键字 LIKE；参数个数与 `?` 一致。
    #[test]
    fn list_query_binds_status_year_keyword() {
        let filter = PeriodFilter {
            keyword: Some("2026-1".to_string()),
            statuses: vec!["DRAFT".to_string(), "LOCKED".to_string()],
            year: Some(2026),
        };
        let (sql, params) = list_query(&filter);
        assert!(sql.starts_with("SELECT"));
        assert!(sql.contains("p.status IN (?, ?)"), "状态多选逐个占位：{sql}");
        assert!(sql.contains("left(p.period_code, 4) = ?"));
        assert!(sql.contains("p.period_code LIKE ?"));
        assert!(
            !sql.contains("ORDER BY"),
            "ORDER BY 必须单独传给 scope::paginate_unscoped"
        );
        assert!(!sql.contains("LIMIT"), "LIMIT 必须单独传给 scope::paginate_unscoped");
        assert_eq!(sql.matches('?').count(), params.len(), "占位符与参数个数");

        // 空过滤：只剩软删守卫（不允许多出悬空的 AND）
        let (bare, bare_params) = list_query(&PeriodFilter::default());
        assert!(bare.ends_with("WHERE p.deleted_at IS NULL"), "空过滤：{bare}");
        assert!(bare_params.is_empty());
    }

    /// 汇总快照投影 + `is_locked` + 出参投影（时间是 `+08:00` 的 ISO8601）。
    #[test]
    fn row_projects_to_res() {
        let row = AttendancePeriodRow {
            id: Uuid::nil(),
            period_code: "2026-11".to_string(),
            period_start: NaiveDate::parse_from_str("2026-11-01", "%Y-%m-%d").expect("合法日期"),
            period_end: NaiveDate::parse_from_str("2026-11-30", "%Y-%m-%d").expect("合法日期"),
            status: "LOCKED".to_string(),
            employee_count: 2,
            should_days_total: Decimal::from_str_exact("40.00").expect("合法金额"),
            actual_days_total: Decimal::from_str_exact("38.50").expect("合法金额"),
            ot_hours_total: Decimal::from_str_exact("6.00").expect("合法金额"),
            import_task_id: None,
            submitted_at: Some(Utc::now()),
            submitted_by: Some(Uuid::nil()),
            submitted_by_name: Some("张人事".to_string()),
            locked_at: Some(Utc::now()),
            locked_by: Some(Uuid::nil()),
            locked_by_name: Some("张人事".to_string()),
            remark: Some("  26-25  ".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert!(row.is_locked());
        assert_eq!(row.totals().employee_count, 2);

        let res = row.to_res();
        assert_eq!(res.status_label, "已锁定");
        assert!(res.locked);
        assert_eq!(res.submitted_by_name.as_deref(), Some("张人事"));
        assert!(res.locked_at.as_deref().unwrap_or_default().ends_with("+08:00"));
        // 出参的金额是天数字符串（设计 §5.1），不是浮点
        assert_eq!(res.should_days_total.to_string(), "40.00");
    }

    /// 撞码报文点名期间码 + 给下一步（不是「唯一约束冲突」这种运维语言）。
    #[test]
    fn duplicate_error_names_period_code() {
        let error = duplicate_error("2026-11");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::HR_ATTENDANCE_PERIOD_DUPLICATE);
        assert!(error.msg.contains("2026-11"));
        assert!(error.msg.contains("一个月只能有一批"));
    }

    /// 每个 `CHECK` 都有中文映射（漏一条 → 用户看到约束名）。
    #[test]
    fn every_check_has_human_message() {
        for name in PERIOD_CHECKS {
            let message = check_message(Some(name));
            assert!(!message.contains(name), "约束 {name} 没翻译：{message}");
            assert!(!message.contains("不满足库约束"), "约束 {name} 落到了兜底文案");
        }
        assert!(
            check_message(Some("ck_unknown")).contains("ck_unknown"),
            "未知约束要回显名字"
        );
    }
}
