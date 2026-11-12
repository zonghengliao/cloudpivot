//! 请假单数据层（`biz_leave_request` · HR-24）。
//!
//! 本文件是 `biz_leave_request` **唯一**的 SQL 出口：单号取号 / 建 / 读 / 改草稿 /
//! 状态 CAS 回写 / 审批实例回写 / 软删 / 分页 / 无薪假天数汇总。契约与校验在
//! [`crate::business::hr::leave_dto`]，接口层与事务在 **HR-25**（CRUD + 提交/撤回/作废）、
//! **HR-27**（审批回调桥）。
//!
//! # 表结构的唯一依据
//!
//! `backend/migrations/business/V0383__hr_tables.up.sql` 的 `biz_leave_request`（设计 §3.1 ⑥）。
//! 列名 / `CHECK` 名 / 唯一索引名逐字取自该迁移；**迁移已应用且不可改**（AGENTS.md 约定 6），
//! 本卡零迁移。
//!
//! # 三条硬约束（本文件负责守住）
//!
//! 1. **状态只能走状态机**：[`set_status`] 是 CAS 回写（`WHERE status = 旧值`），调用方必须是
//!    `shared::state_machine::transition`（它同时写 `biz_status_log`）。[`UPDATE_SQL`] 的列清单里
//!    **没有** `status`、`employee_id`、`leave_no` —— 草稿改单不许顺手改状态/换人/改号，
//!    那条路径不会有流转记录（单测 `update_sql_never_touches_guarded_columns` 钉住）。
//! 2. **`ck_biz_leave_cancel` 是条件约束**：`status = 'CANCELLED'` ⇒ `cancel_reason` 非空。
//!    所以 [`set_status`] 把 `cancel_reason` 与 `status` 写在**同一条语句**里
//!    （分两条写会有一瞬间违约束、直接报 400）。传空原因给 `→CANCELLED` 会被库拒 ——
//!    接口层（[`leave_dto::cancel_of`]）已经拦成「≥5 字」的人话，库层这条是最后兜底。
//! 3. **已批准/已作废不许改**（设计 §4.2.2 `LEAVE_LOCKED`）：[`UPDATE_SQL`] 带
//!    `status = 'DRAFT'`、[`SOFT_DELETE_SQL`] 也带 —— 守卫在库层，不只靠 handler 判
//!    （并发下 handler 读到的状态可能已过期，返回行数 0 就是「有人先动了」，报 409）。
//!
//! # 单号为什么在本文件取
//!
//! `LV` 号段由 HR-05 的 `V0386__hr_seq_dict` 种进 `biz_seq`（`date_pattern='yyyyMMdd'`、
//! 6 位、按日滚动），形态 `LV-yyyyMMdd-000001`（设计 §3.1 ⑥ 的列注释）。
//! [`next_leave_no`] 与 ERP 合同号（`erp::contract_model::next_contract_no`）**同一套三分支写发**
//! （同日累加 / 新日重置 / 时钟回拨不重置），并且必须与建单在**同一事务**里取号 ——
//! 回滚时号一起回滚，不留空号。
//!
//! # 数据权限（设计 §4.7.3）
//!
//! 本表登记进 `SCOPED_TABLES`（[`LEAVE_TARGET`]：`dept_col = dept_id`、`owner_col = NULL`），
//! 分页走唯一入口 [`scope::paginate`]：部门 / 部门以下 / 全部范围按 `dept_id` 裁。
//! 「仅本人」范围在这里**不成立**（`owner_col = NULL`）—— 员工看自己的假走
//! [`LeaveFilter::mine`]（由 handler 用令牌里的 `user_id` 换 `employee_id`，设计 §4.7.3 末列）。
//!
//! # 姓名：快照 vs 实时 join（`AGENTS.md` 约定 13）
//!
//! `employee_no` / `employee_name` 是**快照列**（DDL 里就有，请假当时是谁）；
//! `dept_name` / `submitted_by_name` / `cancelled_by_name` 走 join **实时**取
//! （部门改名后显示新名；审计人取不到姓名时回退登录名 —— 显示空比显示 uuid 更没用）。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::rust_decimal::prelude::FromStr;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::employee_dto::timestamp_cst;
use crate::business::hr::leave_dto::{
    self, LeaveCreate, LeaveDraftState, LeaveFilter, LeavePatch, LeaveRes, STATUS_CANCELLED, STATUS_DRAFT,
};
use crate::business::hr::model::SENTINEL_ORG_ID;
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;
use crate::shared::error_code;
use crate::utils;

// ==================== 表 / 约束名（与 V0383 逐字一致） ====================

/// 表名（与 [`crate::business::hr::model::TABLE_LEAVE_REQUEST`] 同一常量，这里只是可读别名）。
pub const TABLE_LEAVE: &str = crate::business::hr::model::TABLE_LEAVE_REQUEST;

/// 单号唯一索引（`(org_id, leave_no) WHERE deleted_at IS NULL`）。
pub const UK_LEAVE_NO: &str = "uk_biz_leave_no";

/// 本表的 6 个 `CHECK`（`V0383` 的 `ck_biz_leave_*`）。
///
/// 列出来是为了让 [`check_message`] 的映射**可被单测逐条对拍**：漏一条的后果是用户看到
/// 「请假数据不满足库约束 ck_biz_leave_days」这种运维语言，而不是「请假天数必须大于 0」。
pub const LEAVE_CHECKS: [&str; 6] = [
    "ck_biz_leave_type",
    "ck_biz_leave_status",
    "ck_biz_leave_range",
    "ck_biz_leave_days",
    "ck_biz_leave_paid",
    "ck_biz_leave_cancel",
];

/// `biz_seq` 里的请假单号段（HR-05 的 `V0386` 种下：`date_pattern='yyyyMMdd'`、6 位）。
pub const SEQ_TYPE: &str = "LV";

/// 单号前缀（`LV-yyyyMMdd-000001`）。
pub const NO_PREFIX: &str = "LV";

/// 流水位数（`biz_seq.pad_width = 6`）。
pub const NO_SEQ_WIDTH: usize = 6;

/// 数据权限锚点（设计 §4.7.3：`dept_col = dept_id`、`owner_col = NULL`，别名 `l` 与 [`ROW_SQL`] 一致）。
pub const LEAVE_TARGET: scope::ScopeTarget = scope::ScopeTarget::BIZ_LEAVE_REQUEST;

// ==================== 错误翻译 ====================

/// 单号冲突（`uk_biz_leave_no`）→ 409 + `68011`。
///
/// 单号是**服务端取号**的，正常路径撞不上；撞了说明取号语句与建单不在同一事务，
/// 或者有人手工插了行。报文要告诉用户「重试」而不是「换个号」（他根本不知道号从哪来）。
pub fn duplicate_error(leave_no: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::HR_LEAVE_NO_DUPLICATE,
        format!(
            "请假单号「{leave_no}」已被占用（取号与建单不在同一事务，或有人手工插过行）：\
             请重新提交一次（会取到新号）；若连续失败，请联系管理员核对 biz_seq 的 LV 号段"
        ),
    )
}

/// 当前状态下不允许该操作（409 + `68012`）。
///
/// 与 [`duplicate_error`] 分开：这类冲突**不是**撞键，而是「单据当前状态做不了这件事」——
/// 已批准的假要改数、已作废的单再作废、审批中的单直接删。前端文案要点名下一步
/// （作废重开 / 先撤回），不是「换个值重试」。
pub fn rule_blocked_error(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::HR_LEAVE_RULE_BLOCKED, message.into())
}

/// `CHECK` 约束 → 用户可读文案（`ck_biz_leave_*`，见 `V0383`）。
pub fn check_message(constraint: Option<&str>) -> String {
    let name = constraint.unwrap_or_default();
    let human = match name {
        "ck_biz_leave_type" => {
            "假别只能是 ANNUAL / SICK / PERSONAL / MARRIAGE / MATERNITY / \
                                PATERNITY / BEREAVEMENT / COMPENSATORY / OTHER"
        }
        "ck_biz_leave_status" => "请假单状态只能是 DRAFT / SUBMITTED / APPROVED / CANCELLED",
        "ck_biz_leave_range" => "请假结束时间必须晚于开始时间",
        "ck_biz_leave_days" => "请假天数必须大于 0（半天填 0.5）",
        "ck_biz_leave_paid" => "带薪标记只能是 1（带薪）或 0（无薪）",
        "ck_biz_leave_cancel" => "作废请假单必须同时给出作废原因（销假要能交代为什么）",
        _ => return format!("请假数据不满足库约束 {name}，请检查必填项与取值范围后重试"),
    };
    human.to_string()
}

/// 请假单写路径的**唯一**数据库错误出口。
///
/// | 场景 | HTTP | 业务码 |
/// |---|---|---|
/// | 单号重复（`uk_biz_leave_no`） | 409 | `68011` |
/// | `CHECK` 违反 | 400 | `10001`（报文点名该改哪个字段） |
/// | 其它（含外键 —— 员工被删） | 500 | `10004` |
pub fn write_error(err: DbError, ctx: &str, leave_no: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => {
            let name = constraint.as_deref().unwrap_or_default().trim();
            if name.eq_ignore_ascii_case(UK_LEAVE_NO) {
                duplicate_error(leave_no)
            } else {
                ApiError::new(
                    409,
                    error_code::RESOURCE_CONFLICT,
                    format!("请假单写入冲突（唯一约束 {name}）：该值已存在于另一条未删除的记录上，请刷新后重试"),
                )
            }
        }
        DbError::CheckViolation { constraint, .. } => {
            ApiError::new(400, error_code::PARAM_INVALID, check_message(constraint.as_deref()))
        }
        _ => utils::db_error(err, ctx),
    }
}

/// 「请假单不存在或已删除」的 404（接口层按 id 取数的入口共用一句）。
pub fn not_found_error() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "请假单不存在或已删除")
}

// ==================== 行 / 载荷 ====================

/// 请假单全字段行（含三处 join 的姓名/部门名，避免 DTO 层再查库）。
#[derive(Debug, Clone)]
pub struct LeaveRow {
    /// 主键。
    pub id: Uuid,
    /// 单号。
    pub leave_no: String,
    /// 员工 id。
    pub employee_id: Uuid,
    /// 工号（快照）。
    pub employee_no: String,
    /// 姓名（快照）。
    pub employee_name: String,
    /// 部门 id。
    pub dept_id: Option<Uuid>,
    /// 部门名（join 实时取；取不到为空串）。
    pub dept_name: String,
    /// 假别码值。
    pub leave_type: String,
    /// 起。
    pub start_time: DateTime<Utc>,
    /// 止。
    pub end_time: DateTime<Utc>,
    /// 请假天数。
    pub duration_days: Decimal,
    /// 1 带薪 / 0 无薪。
    pub paid: i16,
    /// 状态码值。
    pub status: String,
    /// 事由。
    pub reason: Option<String>,
    /// 审批实例 id。
    pub approval_instance_id: Option<Uuid>,
    /// 提交时间。
    pub submitted_at: Option<DateTime<Utc>>,
    /// 提交人。
    pub submitted_by: Option<Uuid>,
    /// 提交人姓名（join）。
    pub submitted_by_name: Option<String>,
    /// 作废时间。
    pub cancelled_at: Option<DateTime<Utc>>,
    /// 作废人。
    pub cancelled_by: Option<Uuid>,
    /// 作废人姓名（join）。
    pub cancelled_by_name: Option<String>,
    /// 作废原因。
    pub cancel_reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最后修改时间。
    pub updated_at: DateTime<Utc>,
}

impl LeaveRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            leave_no: row.get_typed("leave_no")?,
            employee_id: row.get_typed("employee_id")?,
            employee_no: row.get_typed("employee_no")?,
            employee_name: row.get_typed("employee_name")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            leave_type: row.get_typed("leave_type")?,
            start_time: row.get_typed("start_time")?,
            end_time: row.get_typed("end_time")?,
            duration_days: row.get_typed("duration_days")?,
            paid: row.get_typed("paid")?,
            status: row.get_typed("status")?,
            reason: clean(row.get_typed("reason")?),
            approval_instance_id: row.get_typed("approval_instance_id")?,
            submitted_at: row.get_typed("submitted_at")?,
            submitted_by: row.get_typed("submitted_by")?,
            submitted_by_name: clean(row.get_typed("submitted_by_name")?),
            cancelled_at: row.get_typed("cancelled_at")?,
            cancelled_by: row.get_typed("cancelled_by")?,
            cancelled_by_name: clean(row.get_typed("cancelled_by_name")?),
            cancel_reason: clean(row.get_typed("cancel_reason")?),
            remark: clean(row.get_typed("remark")?),
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 是否锁死（`APPROVED` / `CANCELLED`，设计 §4.2.2）。
    pub fn is_locked(&self) -> bool {
        leave_dto::is_locked(&self.status)
    }

    /// 是否可编辑 / 可删除（只有 `DRAFT`）。
    pub fn is_editable(&self) -> bool {
        leave_dto::EDITABLE_STATUSES.contains(&self.status.as_str())
    }

    /// 可改列的当前值（改草稿时与入参合并，见 [`leave_dto::patch_of`]）。
    pub fn draft_state(&self) -> LeaveDraftState {
        LeaveDraftState {
            leave_type: self.leave_type.clone(),
            start_time: self.start_time,
            end_time: self.end_time,
            duration_days: self.duration_days,
            paid: self.paid,
            reason: self.reason.clone(),
            remark: self.remark.clone(),
        }
    }

    /// 投影成出参契约。
    pub fn to_res(&self) -> LeaveRes {
        LeaveRes {
            id: self.id.to_string(),
            leave_no: self.leave_no.clone(),
            employee_id: self.employee_id.to_string(),
            employee_no: self.employee_no.clone(),
            employee_name: self.employee_name.clone(),
            dept_id: self.dept_id.map(|id| id.to_string()),
            dept_name: self.dept_name.clone(),
            leave_type: self.leave_type.clone(),
            leave_type_label: leave_dto::leave_type_label(&self.leave_type).to_string(),
            start_time: timestamp_cst(self.start_time),
            end_time: timestamp_cst(self.end_time),
            duration_days: self.duration_days,
            paid: self.paid,
            paid_label: leave_dto::paid_label(self.paid).to_string(),
            status: self.status.clone(),
            status_label: leave_dto::status_label(&self.status).to_string(),
            locked: self.is_locked(),
            reason: self.reason.clone(),
            approval_instance_id: self.approval_instance_id.map(|id| id.to_string()),
            submitted_at: self.submitted_at.map(timestamp_cst),
            submitted_by: self.submitted_by.map(|id| id.to_string()),
            submitted_by_name: self.submitted_by_name.clone(),
            cancelled_at: self.cancelled_at.map(timestamp_cst),
            cancelled_by: self.cancelled_by.map(|id| id.to_string()),
            cancelled_by_name: self.cancelled_by_name.clone(),
            cancel_reason: self.cancel_reason.clone(),
            remark: self.remark.clone(),
            created_at: timestamp_cst(self.created_at),
            updated_at: timestamp_cst(self.updated_at),
        }
    }
}

/// 新建请假单的完整载荷（业务列 + 服务端抄来的员工快照 + 服务端取好的单号）。
///
/// 为什么快照也在这里：`employee_no` / `employee_name` / `dept_id` 是**落库值**，
/// 由 handler 从 `biz_employee` 查出来（同一事务、加锁读），不由前端决定 ——
/// 但 model 不该自己去 join `biz_employee`（那是 HR-12 的表，且建单时要的是「当时」的值）。
#[derive(Debug, Clone, Copy)]
pub struct LeaveInsert<'a> {
    /// 主键（handler 生成 `Uuid::now_v7()`，同事务写 `biz_status_log` 要用）。
    pub id: Uuid,
    /// 单号（[`next_leave_no`] 在同一事务里取）。
    pub leave_no: &'a str,
    /// 工号快照。
    pub employee_no: &'a str,
    /// 姓名快照。
    pub employee_name: &'a str,
    /// 部门（取自员工档案，可为空 —— `biz_employee.dept_id` 是 NOT NULL，理论上不会空）。
    pub dept_id: Option<Uuid>,
    /// 业务列（[`leave_dto::create_of`] 的产物）。
    pub draft: &'a LeaveCreate,
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

/// 时间参数（`?::text::timestamptz`）。
fn timestamp(value: DateTime<Utc>) -> Value {
    Value::Text(value.to_rfc3339())
}

/// 天数参数（`?::text::numeric`：`Value::Text` 与 `numeric` 列的组合只有这条写法稳）。
fn numeric(value: Decimal) -> Value {
    Value::Text(value.to_string())
}

/// `paid` 参数（`?::int`，与 `employee_model` 的 `gender = ?::int` 同款）。
fn int_of(value: i16) -> Value {
    Value::from(i32::from(value))
}

// ==================== SQL ====================

/// 单条投影（`dept_name` / 两个人名走 join，DTO 层不再查库）。
///
/// 人名取 `real_name`，为空回退登录名：审计列显示「空」比显示 uuid 更没用
/// （`AGENTS.md` 约定 13 的反例：附件表只存 `uploaded_by` → 界面显示 UUID）。
const ROW_SQL: &str = "\
SELECT l.id, l.leave_no, l.employee_id, l.employee_no, l.employee_name, l.dept_id, \
       COALESCE(d.dept_name, '') AS dept_name, \
       l.leave_type, l.start_time, l.end_time, l.duration_days, l.paid, l.status, l.reason, \
       l.approval_instance_id, l.submitted_at, l.submitted_by, \
       COALESCE(NULLIF(su.real_name, ''), su.username, '') AS submitted_by_name, \
       l.cancelled_at, l.cancelled_by, \
       COALESCE(NULLIF(cu.real_name, ''), cu.username, '') AS cancelled_by_name, \
       l.cancel_reason, l.remark, l.created_at, l.updated_at \
  FROM biz_leave_request l \
  LEFT JOIN sys_dept d ON d.id = l.dept_id AND d.deleted_at IS NULL \
  LEFT JOIN sys_user su ON su.id = l.submitted_by \
  LEFT JOIN sys_user cu ON cu.id = l.cancelled_by";

/// 建单（17 个绑定参数）。`status` **显式写 `DRAFT`**（不靠 DEFAULT）：
/// 建单那一刻的状态必须是确定的，而不是「列默认值是 DRAFT」这种隐式知识。
pub const INSERT_SQL: &str = "\
INSERT INTO biz_leave_request \
    (id, org_id, leave_no, employee_id, employee_no, employee_name, dept_id, leave_type, \
     start_time, end_time, duration_days, paid, status, reason, remark, create_by, update_by) \
 VALUES (?::uuid, ?::uuid, ?, ?::uuid, ?, ?, NULLIF(?::text,'')::uuid, ?, \
         ?::text::timestamptz, ?::text::timestamptz, ?::text::numeric, ?::int, ?, \
         NULLIF(?::text,''), NULLIF(?::text,''), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)";

/// 改草稿的**可写列**（10 个绑定参数：7 列 + `updated_at` + `update_by` + `id`）。
///
/// **不含** `employee_id`（换人 = 作废重开，见 DTO 文档）/ `leave_no`（单号是审计标识）/
/// `status`（走状态机）/ `approval_instance_id` / `submitted_*` / `cancelled_*`（状态机回写）。
/// `WHERE status = 'DRAFT'` 是库层守卫：并发下有人先提交了，本语句影响 0 行 → 调用方报 409。
pub const UPDATE_SQL: &str = "\
UPDATE biz_leave_request SET \
    leave_type = ?, start_time = ?::text::timestamptz, end_time = ?::text::timestamptz, \
    duration_days = ?::text::numeric, paid = ?::int, reason = NULLIF(?::text,''), \
    remark = NULLIF(?::text,''), updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
  WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL";

/// 状态 CAS 回写（9 个绑定参数；只能由 `shared::state_machine::transition` 之后调用）。
///
/// `submitted_*` 与 `cancelled_*` 都用 `COALESCE(NULLIF(…), 原值)`：目标状态是哪个就只写那一对，
/// 另一对**原样保留**（审计信息不清空 —— 撤回后 `submitted_at` 还留着「上次谁提的」）。
/// `cancel_reason` 同理：只有作废那一次带值。
/// `WHERE status = ?` 是乐观锁：并发两次流转只有一次能改（后到的 `rows = 0` → 409）。
pub const SET_STATUS_SQL: &str = "\
UPDATE biz_leave_request SET \
    status = ?, \
    submitted_at = COALESCE(NULLIF(?::text,'')::timestamptz, submitted_at), \
    submitted_by = COALESCE(NULLIF(?::text,'')::uuid, submitted_by), \
    cancelled_at = COALESCE(NULLIF(?::text,'')::timestamptz, cancelled_at), \
    cancelled_by = COALESCE(NULLIF(?::text,'')::uuid, cancelled_by), \
    cancel_reason = COALESCE(NULLIF(?::text,''), cancel_reason), \
    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL";

/// 回写审批实例 id（3 个绑定参数；提交时发起 `hr_leave` 流程后立即写）。
///
/// 单独一条而不是并进 [`SET_STATUS_SQL`]：实例 id 是「发起流程」的产物，
/// 而状态流转要在 `state_machine::transition` 之后回写 —— 两件事分两条语句，
/// 调用方按顺序执行，读代码的人一眼能看出「先发流程还是先改状态」。
pub const SET_APPROVAL_SQL: &str = "\
UPDATE biz_leave_request SET \
    approval_instance_id = NULLIF(?::text,'')::uuid, updated_at = ?, \
    update_by = NULLIF(?::text,'')::uuid \
  WHERE id = ?::uuid AND deleted_at IS NULL";

/// 软删草稿（3 个绑定参数）。
///
/// 只删 `DRAFT`：`SUBMITTED` 要先撤回（撤回 = 状态机回草稿，有流转记录），
/// `APPROVED` 只能作废（销假，要原因）。硬删会带走附件与审批轨迹（`biz_status_log` 按 entity_id
/// 关联），所以本表**没有**硬删 SQL。
pub const SOFT_DELETE_SQL: &str = "\
UPDATE biz_leave_request SET deleted_at = ?, update_by = NULLIF(?::text,'')::uuid \
 WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL";

/// 单号流水推进（4 个绑定参数，三分支：同日累加 / 新日重置 / 时钟回拨不重置）。
///
/// 与 `erp::contract_model::bump_seq` 逐句同款：`UPDATE … RETURNING current_no` 是单语句，
/// PG 对该行加行锁 ⇒ 两个并发事务先后拿到 `n` 与 `n+1`，**天然不重号**。
/// 时钟回拨（`period_key > 今天`）时继续累加而不是重置：重置会撞上「未来那天已经发过的号」。
pub const BUMP_SEQ_SQL: &str = "\
UPDATE biz_seq \
   SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                         WHEN period_key < ? THEN 1 \
                         ELSE current_no + 1 END, \
       period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
       updated_at = now() \
 WHERE seq_type = 'LV' AND deleted_at IS NULL \
 RETURNING current_no";

/// 某员工在区间内**已批准的无薪假**天数合计（设计 §4.2.2 末条 / HR-22 的「无薪假 ≤ 已批天数」）。
///
/// 区间是半开 `[from, to)`：调用方传「考勤期间起 00:00」与「期间止 +1 天 00:00」。
/// `paid = 0` 才算扣款口径（设计 §3.1 ⑥：带薪与否与假别解耦，薪资只看 `paid`）。
pub const APPROVED_UNPAID_DAYS_SQL: &str = "\
SELECT (COALESCE(SUM(l.duration_days), 0))::text AS days \
  FROM biz_leave_request l \
 WHERE l.employee_id = ?::uuid AND l.deleted_at IS NULL \
   AND l.status = 'APPROVED' AND l.paid = 0 \
   AND l.start_time < ?::text::timestamptz AND l.end_time > ?::text::timestamptz";

// ==================== 取号 ====================

/// 组装单号（`LV-{周期键}-{6 位流水}`）。
pub fn format_leave_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// 取一个请假单号（周期键按北京时间，与 `PO` / `IN` / `CT` 同款）。
///
/// 必须在**建单的同一事务**里调用（`ex` 传事务）：取号与插入一起提交、一起回滚，不留空号。
/// 号段行由 `V0386` 种下；缺失时报「号段不存在」而不是编一个号 —— 编出来的号会撞唯一键，
/// 用户看到的会是「单号重复」这种完全指错方向的报错。
pub async fn next_leave_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = crate::business::erp::serial_model::period_key_of(now);
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有部门 / 归属列。
    let rows = ex
        .query_sql(
            BUMP_SEQ_SQL.to_string(),
            vec![
                Value::Text(period_key.clone()),
                Value::Text(period_key.clone()),
                Value::Text(period_key.clone()),
                Value::Text(period_key.clone()),
            ],
        )
        .await?;
    let no = rows
        .first()
        .map(|row| row.get_typed::<i64>("current_no"))
        .transpose()?
        .ok_or_else(|| {
            DbError::InvalidValue(format!(
                "取号失败：biz_seq(seq_type='LV') 行不存在（period_key={period_key}）—— \
                 请假单号段应由迁移 V0386 种下，请检查该迁移是否已执行"
            ))
        })?;
    Ok(format_leave_no(&period_key, no))
}

// ==================== 写操作 ====================

/// 建单。返回行数（正常恒 1；0 表示语句没命中，属于编程错误，调用方按 500 处理）。
pub async fn insert(ex: &impl SqlExecutor, new: LeaveInsert<'_>, operator: Option<Uuid>) -> DbResult<u64> {
    // SCOPE-EXEMPT: 单据写入；org_id 由 SENTINEL_ORG_ID 显式给出（多组织落地时改一处）。
    ex.exec_sql(
        INSERT_SQL.to_string(),
        vec![
            Value::from(&new.id),
            Value::from(&SENTINEL_ORG_ID),
            Value::Text(new.leave_no.to_string()),
            Value::from(&new.draft.employee_id),
            Value::Text(new.employee_no.to_string()),
            Value::Text(new.employee_name.to_string()),
            opt_uuid(new.dept_id),
            Value::Text(new.draft.leave_type.clone()),
            timestamp(new.draft.start_time),
            timestamp(new.draft.end_time),
            numeric(new.draft.duration_days),
            int_of(new.draft.paid),
            Value::Text(STATUS_DRAFT.to_string()),
            opt_text(new.draft.reason.as_deref()),
            opt_text(new.draft.remark.as_deref()),
            opt_uuid(operator),
            opt_uuid(operator),
        ],
    )
    .await
}

/// 改草稿的可写列。返回受影响行数：`0` = 不存在 / 已软删 / 已不是草稿（调用方报 409）。
pub async fn update(ex: &impl SqlExecutor, id: Uuid, patch: &LeavePatch, operator: Option<Uuid>) -> DbResult<u64> {
    // SCOPE-EXEMPT: 单据写入。
    ex.exec_sql(
        UPDATE_SQL.to_string(),
        vec![
            Value::Text(patch.leave_type.clone()),
            timestamp(patch.start_time),
            timestamp(patch.end_time),
            numeric(patch.duration_days),
            int_of(patch.paid),
            opt_text(patch.reason.as_deref()),
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
/// # 参数
///
/// * `from_status` —— 状态机读到的旧状态（CAS 的 `WHERE` 条件）
/// * `cancel_reason` —— 只有 `→CANCELLED` 带值；库层 `ck_biz_leave_cancel` 要求它在
///   `CANCELLED` 时非空，所以必须与 `status` 同一条语句写（见模块文档第 2 条）
///
/// # 返回值
///
/// 受影响行数：`0` = 状态已被他人改过（并发保护，调用方报 409）。
pub async fn set_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from_status: &str,
    to_status: &str,
    cancel_reason: Option<&str>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    let now = Utc::now();
    // 只有「进 SUBMITTED / 进 CANCELLED」才写对应那一对列；其余边两对都不动。
    let submitted_at = (to_status == leave_dto::STATUS_SUBMITTED).then_some(now);
    let cancelled_at = (to_status == STATUS_CANCELLED).then_some(now);
    // SCOPE-EXEMPT: 单据状态回写。
    ex.exec_sql(
        SET_STATUS_SQL.to_string(),
        vec![
            Value::Text(to_status.to_string()),
            opt_timestamp(submitted_at),
            opt_uuid(submitted_at.and(operator)),
            opt_timestamp(cancelled_at),
            opt_uuid(cancelled_at.and(operator)),
            opt_text(cancel_reason),
            Value::from(now),
            opt_uuid(operator),
            Value::from(&id),
            Value::Text(from_status.to_string()),
        ],
    )
    .await
}

/// 回写审批实例 id（提交发起流程后立即写；撤回/驳回时传 `None` 清空）。
pub async fn set_approval_instance(
    ex: &impl SqlExecutor,
    id: Uuid,
    instance_id: Option<Uuid>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 单据关联回写。
    ex.exec_sql(
        SET_APPROVAL_SQL.to_string(),
        vec![
            opt_uuid(instance_id),
            Value::from(Utc::now()),
            opt_uuid(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删草稿。返回受影响行数：`0` = 不存在 / 已软删 / 已不是草稿。
pub async fn soft_delete(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    let now = Utc::now();
    // SCOPE-EXEMPT: 单据软删。
    ex.exec_sql(
        SOFT_DELETE_SQL.to_string(),
        vec![Value::from(now), opt_uuid(operator), Value::from(&id)],
    )
    .await
}

// ==================== 读操作 ====================

/// 按 id 取单条（不含已软删）。
pub async fn find_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<LeaveRow>> {
    let sql = format!("{ROW_SQL} WHERE l.id = ?::uuid AND l.deleted_at IS NULL");
    let row = ex.query_first_sql(sql, vec![Value::from(&id)]).await?;
    row.as_ref().map(LeaveRow::from_row).transpose()
}

/// 按 id 取单条并加行锁（`FOR UPDATE OF l`）：改 / 提交 / 作废前先锁，
/// 两个并发请求不会各读一份旧状态再各写一次。
pub async fn lock_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<LeaveRow>> {
    let sql = format!("{ROW_SQL} WHERE l.id = ?::uuid AND l.deleted_at IS NULL FOR UPDATE OF l");
    let row = ex.query_first_sql(sql, vec![Value::from(&id)]).await?;
    row.as_ref().map(LeaveRow::from_row).transpose()
}

/// 某员工在 `[from, to)` 区间内已批准的无薪假天数（HR-22 的导入校验用；本卡只提供读原语）。
pub async fn approved_unpaid_days(
    ex: &impl SqlExecutor,
    employee_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> DbResult<Decimal> {
    // SCOPE-EXEMPT: 主数据口径的聚合读，按 employee_id 定点过滤，不是列表。
    // 参数顺序按 SQL 占位符：`start_time < ?` 收**上界**（`to`）、`end_time > ?` 收**下界**（`from`）。
    // 这里颠倒过一次：上界传成 `from` ⇒ `start_time < from` 恒假 ⇒ 永远返回 0（真库用例抓到的）。
    let row = ex
        .query_first_sql(
            APPROVED_UNPAID_DAYS_SQL.to_string(),
            vec![Value::from(&employee_id), timestamp(to), timestamp(from)],
        )
        .await?;
    let Some(row) = row else {
        return Ok(Decimal::ZERO);
    };
    let text: String = row.get_typed("days")?;
    Decimal::from_str(text.trim())
        .map_err(|err| DbError::InvalidValue(format!("无薪假天数合计不是合法小数（`{text}`）：{err}")))
}

/// 请假单列表分页（设计 §5.2 #26）。
///
/// # 数据权限
///
/// 走唯一分页入口 [`scope::paginate`] + [`LEAVE_TARGET`]（架构 §2.4 闸①）：
/// 部门 / 部门以下 / 自定义范围按 `dept_id` 裁；`SELF`（仅本人）范围在本表**不成立**
/// （`owner_col = NULL`），员工看自己的假走 `mine`（[`LeaveFilter::mine`]）。
///
/// # 返回值
///
/// `(列表, 总数)`；总数是**过滤 + 数据权限之后**的条数（`scope::paginate` 内部算）。
pub async fn page(
    filter: &LeaveFilter,
    page: u64,
    page_size: u64,
    principal: &ScopedPrincipal,
) -> DbResult<(Vec<LeaveRow>, i64)> {
    let (sql, params) = list_query(filter, filter.mine.then(|| principal.user_id()));
    let result = scope::paginate(
        LEAVE_TARGET,
        &sql,
        params,
        Some("ORDER BY l.start_time DESC, l.id DESC"),
        page,
        page_size,
        principal,
    )
    .await?;
    let mut out = Vec::with_capacity(result.list.len());
    for row in &result.list {
        out.push(LeaveRow::from_row(row)?);
    }
    Ok((out, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 拼列表 SQL 与参数（**纯函数**：不连库就能验证条件个数与占位符个数一致）。
///
/// 不含 `ORDER BY` / `LIMIT`：`scope::paginate` 要求它们在参数里单独传。
///
/// # `mine_user` 为什么是 `Option<Uuid>`
///
/// 「我的请假」= 我这份员工档的假 ⇒ `EXISTS (SELECT 1 FROM biz_employee me WHERE
/// me.id = l.employee_id AND me.user_id = ?)`。用 EXISTS 而不是先查 `employee_id` 再拼 `IN`：
/// 少一次往返，也不会因为「一个账号挂两份员工档」的脏数据把结果放大（`uk_biz_employee_user`
/// 本来就不允许，但读路径不该依赖它）。
fn list_query(filter: &LeaveFilter, mine_user: Option<Uuid>) -> (String, Vec<Value>) {
    let mut sql = format!("{ROW_SQL} WHERE l.deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();

    let statuses: Vec<&str> = filter
        .statuses
        .iter()
        .map(|status| status.trim())
        .filter(|status| !status.is_empty())
        .collect();
    if !statuses.is_empty() {
        // 不用 `= ANY(?::text[])`：驱动对数组参数只有 text 编码器（见 `attendance_period_model` 的注释）
        let holes = vec!["?"; statuses.len()].join(", ");
        sql.push_str(&format!(" AND l.status IN ({holes})"));
        for status in statuses {
            params.push(Value::Text(status.to_string()));
        }
    }

    let leave_types: Vec<&str> = filter
        .leave_types
        .iter()
        .map(|kind| kind.trim())
        .filter(|kind| !kind.is_empty())
        .collect();
    if !leave_types.is_empty() {
        let holes = vec!["?"; leave_types.len()].join(", ");
        sql.push_str(&format!(" AND l.leave_type IN ({holes})"));
        for kind in leave_types {
            params.push(Value::Text(kind.to_string()));
        }
    }

    if let Some(employee_id) = filter.employee_id {
        sql.push_str(" AND l.employee_id = ?::uuid");
        params.push(Value::from(&employee_id));
    }
    if let Some(dept_id) = filter.dept_id {
        sql.push_str(" AND l.dept_id = ?::uuid");
        params.push(Value::from(&dept_id));
    }
    if let Some(from) = filter.date_from {
        // 按「请假起始日」过滤（列表展示的就是起始日，按它筛才与眼睛看到的一致）
        sql.push_str(" AND l.start_time >= ?::text::timestamptz");
        params.push(Value::Text(format!("{from}T00:00:00+08:00")));
    }
    if let Some(to) = filter.date_to {
        sql.push_str(" AND l.start_time < ?::text::timestamptz");
        params.push(Value::Text(format!(
            "{}T00:00:00+08:00",
            to + nik::prelude::chrono::Duration::days(1)
        )));
    }
    if let Some(user_id) = mine_user {
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM biz_employee me \
               WHERE me.id = l.employee_id AND me.user_id = ?::uuid AND me.deleted_at IS NULL)",
        );
        params.push(Value::from(&user_id));
    }
    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        sql.push_str(" AND (l.leave_no LIKE ? OR l.employee_no LIKE ? OR l.employee_name LIKE ?)");
        for _ in 0..3 {
            params.push(Value::from(pattern.clone()));
        }
    }
    (sql, params)
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::hr::leave_dto::STATUS_APPROVED;
    use nik::prelude::chrono::NaiveDate;
    use nik::prelude::rust_decimal::prelude::FromStr;

    /// 建表迁移原文（编译期嵌入）：约束名 / 唯一索引名 / 列名改了就红。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("合法小数")
    }

    fn at(text: &str) -> DateTime<Utc> {
        leave_dto::parse_datetime(text, "t").expect("合法时间")
    }

    fn sample_create() -> LeaveCreate {
        LeaveCreate {
            employee_id: Uuid::now_v7(),
            leave_type: leave_dto::LEAVE_ANNUAL.to_string(),
            start_time: at("2026-11-10 09:00"),
            end_time: at("2026-11-11 18:00"),
            duration_days: dec("2"),
            paid: leave_dto::PAID_YES,
            reason: Some("回老家".to_string()),
            remark: Some("提前报备".to_string()),
        }
    }

    fn sample_patch() -> LeavePatch {
        LeavePatch {
            leave_type: leave_dto::LEAVE_SICK.to_string(),
            start_time: at("2026-11-10 09:00"),
            end_time: at("2026-11-10 18:00"),
            duration_days: dec("1"),
            paid: leave_dto::PAID_NO,
            reason: None,
            remark: None,
            changed: true,
        }
    }

    /// 本表的 6 条 CHECK 与迁移逐字一致（多一条少一条都红）。
    #[test]
    fn check_names_match_ddl() {
        for name in LEAVE_CHECKS {
            assert!(
                V0383_DDL.contains(&format!("CONSTRAINT {name}")),
                "约束 `{name}` 不在 V0383 的 biz_leave_request 里"
            );
        }
        // 迁移里 biz_leave_request 的 CHECK 正好 6 条（V0383 自检的计数口径）
        let block_start = V0383_DDL
            .find("CREATE TABLE IF NOT EXISTS biz_leave_request")
            .expect("找建表语句");
        let block = &V0383_DDL[block_start..];
        let block_end = block.find(");").expect("建表语句结束");
        let count = block[..block_end].matches("CONSTRAINT ck_biz_leave_").count();
        assert_eq!(
            count,
            LEAVE_CHECKS.len(),
            "V0383 的 biz_leave_request CHECK 条数与 LEAVE_CHECKS 不一致"
        );
    }

    /// 唯一索引名与迁移一致，且带软删谓词（软删后可重建同号，否则只能改库）。
    #[test]
    fn unique_index_matches_ddl() {
        let line = V0383_DDL
            .lines()
            .find(|line| line.contains("ON biz_leave_request") && line.contains(UK_LEAVE_NO))
            .expect("V0383 里应有 uk_biz_leave_no");
        assert!(
            line.contains("WHERE deleted_at IS NULL"),
            "唯一索引必须带软删谓词：{line}"
        );
        assert!(
            line.contains("(org_id, leave_no)"),
            "唯一键口径应为 (org_id, leave_no)：{line}"
        );
    }

    /// 列清单与 DDL 对齐：`ROW_SQL` 选出的每个列都真在表上（写错列名只会在运行时炸）。
    #[test]
    fn row_sql_columns_exist_in_ddl() {
        // biz_leave_request 的全部列（V0383 建表语句里的列定义行）
        let block_start = V0383_DDL
            .find("CREATE TABLE IF NOT EXISTS biz_leave_request")
            .expect("建表");
        let block = &V0383_DDL[block_start..];
        let block_end = block.find(");").expect("结束");
        let columns: Vec<&str> = block[..block_end]
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.starts_with("CONSTRAINT") || trimmed.starts_with("CREATE") || trimmed.starts_with(')') {
                    return None;
                }
                trimmed.split_whitespace().next()
            })
            .filter(|name| !name.is_empty() && *name != "(")
            .collect();
        // 16 个业务列 + 6 个审计列 + 6 个约束行（约束行已被上面过滤掉）
        for expected in [
            "id",
            "org_id",
            "leave_no",
            "employee_id",
            "employee_no",
            "employee_name",
            "dept_id",
            "leave_type",
            "start_time",
            "end_time",
            "duration_days",
            "paid",
            "status",
            "reason",
            "approval_instance_id",
            "submitted_at",
            "submitted_by",
            "cancelled_at",
            "cancelled_by",
            "cancel_reason",
            "remark",
            "created_at",
            "updated_at",
            "create_by",
            "update_by",
            "deleted_at",
        ] {
            assert!(
                columns.contains(&expected),
                "DDL 里缺列 `{expected}`（V0383 与设计 §3.1 ⑥ 漂移）"
            );
        }
        // ROW_SQL 选出的列名必须都能在 DDL 里找到（`COALESCE(...) AS xxx` 的派生列除外）
        for derived in ["dept_name", "submitted_by_name", "cancelled_by_name"] {
            assert!(ROW_SQL.contains(&format!("AS {derived}")), "ROW_SQL 缺派生列 {derived}");
        }
        assert!(
            ROW_SQL.contains("FROM biz_leave_request l"),
            "ROW_SQL 的表别名必须是 l（数据权限片段按 l 拼）"
        );
    }

    /// 数据权限锚点：别名与 `ROW_SQL` 一致、`owner_col` 为空（设计 §4.7.3）、已登记进清单。
    #[test]
    fn leave_target_matches_scope_registry() {
        assert_eq!(LEAVE_TARGET.table, TABLE_LEAVE);
        assert_eq!(LEAVE_TARGET.alias, "l", "别名必须与 ROW_SQL 的 `l` 一致");
        assert_eq!(LEAVE_TARGET.dept_col, Some("dept_id"));
        assert_eq!(LEAVE_TARGET.owner_col, None, "请假单没有「仅本人」语义（设计 §4.7.3）");
        assert!(
            scope::SCOPED_TABLES.contains(&LEAVE_TARGET),
            "biz_leave_request 必须登记进 scope::SCOPED_TABLES（否则分页绕过数据权限）"
        );
    }

    /// 每个 SQL 常量的 `?` 个数必须等于实际绑定的参数个数。
    ///
    /// 这类错（多一个 / 少一个占位符）不会编译报错，只会在真跑时抛
    /// 「bind message supplies N parameters」—— 而那条路径可能只在某个分支里走到。
    #[test]
    fn sql_placeholder_counts_match_params() {
        let draft = sample_create();
        let id = Uuid::nil();
        let operator = Some(Uuid::nil());
        let now = Utc::now();

        let insert_params = [
            Value::from(&id),
            Value::from(&SENTINEL_ORG_ID),
            Value::Text("LV-20261110-000001".to_string()),
            Value::from(&draft.employee_id),
            Value::Text("EMP-0001".to_string()),
            Value::Text("张三".to_string()),
            opt_uuid(Some(Uuid::nil())),
            Value::Text(draft.leave_type.clone()),
            timestamp(draft.start_time),
            timestamp(draft.end_time),
            numeric(draft.duration_days),
            int_of(draft.paid),
            Value::Text(STATUS_DRAFT.to_string()),
            opt_text(draft.reason.as_deref()),
            opt_text(draft.remark.as_deref()),
            opt_uuid(operator),
            opt_uuid(operator),
        ];
        assert_eq!(INSERT_SQL.matches('?').count(), insert_params.len(), "INSERT_SQL");

        let patch = sample_patch();
        let update_params = [
            Value::Text(patch.leave_type.clone()),
            timestamp(patch.start_time),
            timestamp(patch.end_time),
            numeric(patch.duration_days),
            int_of(patch.paid),
            opt_text(patch.reason.as_deref()),
            opt_text(patch.remark.as_deref()),
            Value::from(now),
            opt_uuid(operator),
            Value::from(&id),
        ];
        assert_eq!(UPDATE_SQL.matches('?').count(), update_params.len(), "UPDATE_SQL");

        let status_params = [
            Value::Text(STATUS_APPROVED.to_string()),
            opt_timestamp(Some(now)),
            opt_uuid(operator),
            opt_timestamp(None),
            opt_uuid(None),
            opt_text(None),
            Value::from(now),
            opt_uuid(operator),
            Value::from(&id),
            Value::Text(STATUS_DRAFT.to_string()),
        ];
        assert_eq!(
            SET_STATUS_SQL.matches('?').count(),
            status_params.len(),
            "SET_STATUS_SQL"
        );

        let approval_params = [
            opt_uuid(Some(Uuid::nil())),
            Value::from(now),
            opt_uuid(operator),
            Value::from(&id),
        ];
        assert_eq!(
            SET_APPROVAL_SQL.matches('?').count(),
            approval_params.len(),
            "SET_APPROVAL_SQL"
        );

        let delete_params = [Value::from(now), opt_uuid(operator), Value::from(&id)];
        assert_eq!(
            SOFT_DELETE_SQL.matches('?').count(),
            delete_params.len(),
            "SOFT_DELETE_SQL"
        );

        let bump_params = [
            Value::Text("20261110".to_string()),
            Value::Text("20261110".to_string()),
            Value::Text("20261110".to_string()),
            Value::Text("20261110".to_string()),
        ];
        assert_eq!(BUMP_SEQ_SQL.matches('?').count(), bump_params.len(), "BUMP_SEQ_SQL");

        let days_params = [Value::from(&id), timestamp(now), timestamp(now)];
        assert_eq!(
            APPROVED_UNPAID_DAYS_SQL.matches('?').count(),
            days_params.len(),
            "APPROVED_UNPAID_DAYS_SQL"
        );
    }

    /// 改草稿的 SQL 绝不能碰状态机负责的列（碰了就没有流转记录）。
    #[test]
    fn update_sql_never_touches_guarded_columns() {
        // 只看 SET 段：WHERE 里必须有 `status = 'DRAFT'`（那是守卫，不是改列）
        let set_clause = UPDATE_SQL.split("WHERE").next().expect("UPDATE_SQL 必有 WHERE");
        for guarded in ["status =", "employee_id", "leave_no", "approval_instance_id"] {
            assert!(
                !set_clause.contains(guarded),
                "UPDATE_SQL 的 SET 段不许改 `{guarded}`：它属状态机 / 建单路径"
            );
        }
        assert!(UPDATE_SQL.contains("status = 'DRAFT'"), "改草稿必须在库层限定 DRAFT");
        assert!(SOFT_DELETE_SQL.contains("status = 'DRAFT'"), "软删必须在库层限定 DRAFT");
        // 状态回写必须带 CAS（WHERE status = 旧值）
        assert!(SET_STATUS_SQL.contains("status = ?"), "状态回写必须带 CAS 条件");
    }

    /// 状态回写只在对应边上写那一对审计列（撤回不该抹掉 submitted_at）。
    #[test]
    fn set_status_uses_coalesce_for_audit_columns() {
        for column in [
            "submitted_at",
            "submitted_by",
            "cancelled_at",
            "cancelled_by",
            "cancel_reason",
        ] {
            assert!(
                SET_STATUS_SQL.contains(&format!("COALESCE(NULLIF(?::text,''), {column})"))
                    || SET_STATUS_SQL.contains(&format!(
                        "COALESCE(NULLIF(?::text,'')::{}, {column})",
                        if column.ends_with("_at") { "timestamptz" } else { "uuid" }
                    )),
                "`{column}` 必须以 COALESCE 保留原值（审计信息不清空）：{SET_STATUS_SQL}"
            );
        }
    }

    /// 单号形态：`LV-yyyyMMdd-000001`，6 位补零；流水超 6 位只长出位数不截断。
    #[test]
    fn leave_no_format() {
        assert_eq!(format_leave_no("20261110", 1), "LV-20261110-000001");
        assert_eq!(format_leave_no("20261110", 123456), "LV-20261110-123456");
        assert_eq!(format_leave_no("20261110", 1_234_567), "LV-20261110-1234567");
        assert!(
            format_leave_no("20261110", 1).len() < leave_dto::LEAVE_NO_LEN,
            "单号必须能放进 VARCHAR(32)"
        );
    }

    /// 列表 SQL：条件个数与占位符个数一致；`mine` 参数与「只看本人」开关一一对应。
    #[test]
    fn list_query_placeholders_match() {
        let empty = LeaveFilter::default();
        let (sql, params) = list_query(&empty, None);
        assert_eq!(sql.matches('?').count(), params.len(), "空筛选");
        assert!(!sql.contains("biz_employee me"), "mine=false 不该带本人子查询");

        let filter = LeaveFilter {
            keyword: Some("张三".to_string()),
            employee_id: Some(Uuid::nil()),
            dept_id: Some(Uuid::nil()),
            leave_types: vec![leave_dto::LEAVE_ANNUAL.to_string(), leave_dto::LEAVE_SICK.to_string()],
            statuses: vec![STATUS_DRAFT.to_string(), leave_dto::STATUS_SUBMITTED.to_string()],
            date_from: Some(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()),
            date_to: Some(NaiveDate::from_ymd_opt(2026, 11, 30).unwrap()),
            mine: true,
        };
        let (sql, params) = list_query(&filter, Some(Uuid::nil()));
        assert_eq!(sql.matches('?').count(), params.len(), "全条件");
        assert!(sql.contains("biz_employee me"), "mine=true 必须限定本人");
        // 关键字三个 LIKE 共用一个 pattern
        assert_eq!(sql.matches("LIKE ?").count(), 3, "单号 / 工号 / 姓名各一个 LIKE");
        // 起始日上界是**半开**：`to + 1 天 00:00`
        assert!(sql.contains("l.start_time < ?::text::timestamptz"), "上界必须半开");
    }

    /// 出参投影：`*_id` 与 `*_name` 成对、标签齐全、时间带 `+08:00`。
    #[test]
    fn row_to_res_pairs_ids_with_names() {
        let row = LeaveRow {
            id: Uuid::nil(),
            leave_no: "LV-20261110-000001".to_string(),
            employee_id: Uuid::nil(),
            employee_no: "EMP-0001".to_string(),
            employee_name: "张三".to_string(),
            dept_id: Some(Uuid::nil()),
            dept_name: "生产部".to_string(),
            leave_type: leave_dto::LEAVE_ANNUAL.to_string(),
            start_time: at("2026-11-10 09:00"),
            end_time: at("2026-11-10 18:00"),
            duration_days: dec("1"),
            paid: leave_dto::PAID_YES,
            status: STATUS_APPROVED.to_string(),
            reason: None,
            approval_instance_id: Some(Uuid::nil()),
            submitted_at: Some(at("2026-11-09 10:00")),
            submitted_by: Some(Uuid::nil()),
            submitted_by_name: Some("李四".to_string()),
            cancelled_at: None,
            cancelled_by: None,
            cancelled_by_name: None,
            cancel_reason: None,
            remark: None,
            created_at: at("2026-11-09 10:00"),
            updated_at: at("2026-11-10 11:00"),
        };
        let res = row.to_res();
        assert_eq!(res.leave_type_label, "年假");
        assert_eq!(res.paid_label, "带薪");
        assert_eq!(res.status_label, "已批准");
        assert!(res.locked, "APPROVED 是锁死态");
        assert_eq!(res.start_time, "2026-11-10T09:00:00+08:00");
        assert!(row.is_locked());
        assert!(!row.is_editable(), "已批准不许改");
        assert_eq!(row.draft_state().duration_days, dec("1"));
    }
}
