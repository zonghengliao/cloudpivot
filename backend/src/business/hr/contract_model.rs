//! 劳动合同台账数据层（`biz_employee_contract` · HR-15）。
//!
//! 本文件是劳动合同**唯一**的 SQL 出口：建 / 读 / 改 / 状态回写 / 续签链 + 唯一键冲突翻译。
//! 出参契约在 [`crate::business::hr::employee_dto`]（HR-11 已定稿：`ContractInput` 12 列 +
//! `EmployeeContractRes`），接口层在 [`crate::business::hr::contract_handler`]。
//!
//! # 表结构的唯一依据
//!
//! `backend/migrations/business/V0383__hr_tables.up.sql` 的 `biz_employee_contract`（设计 §3.1 ②）。
//! 列名 / `CHECK` 名 / 唯一索引名逐字取自该迁移；**迁移已应用且不可改**（AGENTS.md 约定 6），
//! 本卡零迁移。
//!
//! # 三条硬约束（本文件负责守住）
//!
//! 1. **续签链只能往前长**：`renewed_from_id` 由 [`insert`] 的 `renewed_from_id` 参数写入；
//!    [`UPDATE_SQL`] 里**没有**这一列 —— 续签关系一旦落库就是审计事实，不能靠「修改」改写。
//!    续签链的读取走 [`chain_of`]（递归 CTE，一次查询取回「原合同 → … → 当前合同」）。
//! 2. **状态只能走状态机**：[`set_status`] 是 CAS 回写（`WHERE status = 旧值`），调用方必须是
//!    `shared::state_machine::transition`（它同时写 `biz_status_log`）。接口层的
//!    `PUT /contract/{id}` 的字段部分**不能**改 `status`，否则留下「状态变了但没有流转记录」。
//! 3. **`ck_biz_emp_contract_term` 是双向的**：`status = 'TERMINATED'` ⇔ `terminated_date` 非空。
//!    所以 [`set_status`] 把三列（`status` / `terminated_date` / `terminate_reason`）**一起**写，
//!    续签（→`RENEWED`）时把后两列显式写 `NULL`（旧合同若曾带解除日，必须先解除再续签，
//!    而解除是终态，续签链不会遇到这种行）。
//!
//! # 与「员工列表 / 详情」的关系
//!
//! `biz_employee` 侧的到期提醒字段（`contract_end_date` / `contract_expire_in_days`）与
//! 详情里的 `contracts[]` 都来自本文件：[`list_by_employee`] 给详情时间线，
//! [`has_active`] 给「转正必须先有生效合同」的守卫。两处共用同一份 SQL 常量，
//! 不会出现「列表说快到期、详情说没有合同」。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::employee_dto::ContractInput;
use crate::business::hr::model::SENTINEL_ORG_ID;
use crate::shared::error_code;
use crate::utils;

// ==================== 约束名（与 V0383 逐字一致） ====================

/// 合同编号唯一索引（`(org_id, contract_no) WHERE deleted_at IS NULL`）。
pub const UK_CONTRACT_NO: &str = "uk_biz_emp_contract_no";

/// 本表的 7 个 `CHECK` 约束（`V0383` 的 `ck_biz_emp_contract_*`）。
///
/// 列出来是为了让 [`check_message`] 的映射**可被单测逐条对拍**：漏一条的后果是
/// 用户看到「合同数据不满足库约束 ck_…」这种运维语言，而不是「试用期只能是 0~6 个月」。
pub const CONTRACT_CHECKS: [&str; 7] = [
    "ck_biz_emp_contract_type",
    "ck_biz_emp_contract_status",
    "ck_biz_emp_contract_range",
    "ck_biz_emp_contract_sign",
    "ck_biz_emp_contract_prob",
    "ck_biz_emp_contract_term",
    "ck_biz_emp_contract_unfixed",
];

// ==================== 唯一键 → 友好文案 ====================

/// 本表的唯一键（目前只有合同编号一个；做成枚举是为了让「约束名 → 码 → 文案」只有一处）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractUniqueKey {
    /// 合同编号重复（`uk_biz_emp_contract_no`）。
    ContractNo,
}

impl ContractUniqueKey {
    /// 全部键（新增键时这里必须同步，单测会遍历）。
    pub const ALL: [Self; 1] = [Self::ContractNo];

    /// 数据库里的索引名（与 `V0383__hr_tables` 逐字一致）。
    pub const fn constraint(self) -> &'static str {
        match self {
            Self::ContractNo => UK_CONTRACT_NO,
        }
    }

    /// 冲突业务码（`shared/error_code.rs` 的 68xxx 段）。
    pub const fn code(self) -> i32 {
        match self {
            Self::ContractNo => error_code::HR_CONTRACT_NO_DUPLICATE,
        }
    }
}

/// 从驱动的约束名解析唯一键；不是本表的唯一键 → `None`（交给调用方报通用冲突）。
pub fn classify_unique(constraint: Option<&str>) -> Option<ContractUniqueKey> {
    let name = constraint.unwrap_or_default().trim();
    ContractUniqueKey::ALL
        .into_iter()
        .find(|key| key.constraint().eq_ignore_ascii_case(name))
}

/// 唯一键冲突的用户文案。
///
/// 合同编号是**人工填的纸质编号**（设计 §3.1 ② 的列注释：不取号，必须与纸质一致），
/// 所以冲突文案要把编号原样报回来 —— HR 拿着纸质合同逐个核对，比看约束名有用得多。
pub fn conflict_message(key: ContractUniqueKey, contract_no: &str) -> String {
    match key {
        ContractUniqueKey::ContractNo => format!(
            "合同编号「{contract_no}」已被其他合同占用：合同编号必须与纸质一致且公司内唯一，\
             请核对后重填（若那份合同已作废，请先在合同台账里修正它的编号）"
        ),
    }
}

/// `CHECK` 约束 → 用户可读文案（`ck_biz_emp_contract_*`，见 `V0383`）。
pub fn check_message(constraint: Option<&str>) -> String {
    let name = constraint.unwrap_or_default();
    let human = match name {
        "ck_biz_emp_contract_type" => "合同类型只能是 FIXED_TERM / UNFIXED_TERM / INTERN / LABOR_SERVICE / DISPATCH",
        "ck_biz_emp_contract_status" => "合同状态只能是 ACTIVE / EXPIRED / TERMINATED / RENEWED",
        "ck_biz_emp_contract_range" => "合同终止日期不能早于起始日期",
        "ck_biz_emp_contract_sign" => "签订日期不能晚于起始日期（先签后生效，倒签请改签订日）",
        "ck_biz_emp_contract_prob" => "试用期只能是 0~6 个月（劳动合同法第 19 条上限）",
        "ck_biz_emp_contract_term" => "解除劳动合同必须同时给出解除日期，非解除状态不能带解除日期",
        "ck_biz_emp_contract_unfixed" => "无固定期限合同不能有终止日期",
        _ => return format!("合同数据不满足库约束 {name}，请检查必填项与取值范围后重试"),
    };
    human.to_string()
}

/// 合同写路径的**唯一**数据库错误出口。
///
/// | 场景 | HTTP | 业务码 |
/// |---|---|---|
/// | 合同编号重复 | 409 | `68005` |
/// | `CHECK` 违反 | 400 | `10001`（报文点名该改哪个字段） |
/// | 其它（含外键 —— 本表只可能指向不存在的员工 / 续签来源） | 500 | `10004` |
pub fn write_error(err: DbError, ctx: &str, contract_no: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => match classify_unique(constraint.as_deref()) {
            Some(key) => ApiError::new(409, key.code(), conflict_message(key, contract_no)),
            None => ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!(
                    "劳动合同写入冲突（唯一约束 {}）：该值已存在于另一条未删除的记录上，请刷新后重试",
                    constraint.as_deref().unwrap_or("<未知>")
                ),
            ),
        },
        DbError::CheckViolation { constraint, .. } => {
            ApiError::new(400, error_code::PARAM_INVALID, check_message(constraint.as_deref()))
        }
        _ => utils::db_error(err, ctx),
    }
}

/// 「劳动合同不存在或已删除」的 404（接口层按 id 取数的入口共用一句）。
pub fn not_found_error() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "劳动合同不存在或已删除")
}

// ==================== 行 / 载荷 ====================

/// 合同全字段行（含续签来源合同号的**自连接**结果，避免 DTO 层再查一次库）。
#[derive(Debug, Clone)]
pub struct ContractRow {
    /// 主键。
    pub id: Uuid,
    /// 所属员工。
    pub employee_id: Uuid,
    /// 合同编号（人工填，公司内唯一）。
    pub contract_no: String,
    /// 合同类型码值。
    pub contract_type: String,
    /// 签订日。
    pub sign_date: NaiveDate,
    /// 起始日。
    pub start_date: NaiveDate,
    /// 终止日（无固定期限为 `None`）。
    pub end_date: Option<NaiveDate>,
    /// 试用期（月）。
    pub probation_months: i16,
    /// 状态码值。
    pub status: String,
    /// 解除日。
    pub terminated_date: Option<NaiveDate>,
    /// 解除原因。
    pub terminate_reason: Option<String>,
    /// 续签自哪份合同（首份为 `None`）。
    pub renewed_from_id: Option<Uuid>,
    /// 续签来源合同**编号**（自连接取；来源行已被删时为空串）。
    pub renewed_from_no: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最后修改时间。
    pub updated_at: DateTime<Utc>,
}

impl ContractRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            employee_id: row.get_typed("employee_id")?,
            contract_no: row.get_typed("contract_no")?,
            contract_type: row.get_typed("contract_type")?,
            sign_date: row.get_typed("sign_date")?,
            start_date: row.get_typed("start_date")?,
            end_date: row.get_typed("end_date")?,
            probation_months: row.get_typed("probation_months")?,
            status: row.get_typed("status")?,
            terminated_date: row.get_typed("terminated_date")?,
            terminate_reason: row.get_typed("terminate_reason")?,
            renewed_from_id: row.get_typed("renewed_from_id")?,
            renewed_from_no: clean(row.get_typed("renewed_from_no")?),
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }

    /// 投影成 DTO 契约（HR-11 的 [`ContractInput`]，12 列 + 续签来源编号）。
    ///
    /// 三处读取（详情时间线 / 单条回执 / 续签链）共用这一个投影，
    /// 「详情里有、回执里没有」这种字段漂移不可能发生。
    pub fn to_input(&self) -> ContractInput {
        ContractInput {
            id: self.id,
            contract_no: self.contract_no.clone(),
            contract_type: self.contract_type.clone(),
            sign_date: self.sign_date,
            start_date: self.start_date,
            end_date: self.end_date,
            probation_months: self.probation_months,
            status: self.status.clone(),
            terminated_date: self.terminated_date,
            terminate_reason: self.terminate_reason.clone(),
            renewed_from_id: self.renewed_from_id,
            renewed_from_no: clean(self.renewed_from_no.clone()),
            remark: self.remark.clone(),
        }
    }

    /// 是否处于「不可再修改」的封闭终态（已解除 / 已续签，见 `state_machine::EMPLOYEE_CONTRACT_LOCKED`）。
    pub fn is_locked(&self) -> bool {
        crate::business::shared::state_machine::EMPLOYEE_CONTRACT_LOCKED.contains(&self.status.as_str())
    }
}

/// 合同的**可写列**（新建与修改共用；`status` / `terminated_*` / `renewed_from_id` 不在其中）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContractWrite {
    /// 合同编号（必填，≤32）。
    pub contract_no: String,
    /// 合同类型（5 个取值之一）。
    pub contract_type: String,
    /// 签订日。
    pub sign_date: NaiveDate,
    /// 起始日。
    pub start_date: NaiveDate,
    /// 终止日（无固定期限为 `None`）。
    pub end_date: Option<NaiveDate>,
    /// 试用期（月，0~6）。
    pub probation_months: i16,
    /// 备注。
    pub remark: Option<String>,
}

/// 可空文本参数：空 → 空串，SQL 里包 `NULLIF(?::text,'')`。
fn opt_text(value: Option<&str>) -> Value {
    Value::Text(value.map(str::trim).unwrap_or_default().to_string())
}

/// 可空日期参数（SQL 里包 `NULLIF(?::text,'')::date`；`''::date` 是硬错误，必须走 NULLIF）。
fn opt_date(value: Option<NaiveDate>) -> Value {
    Value::Text(value.map(|date| date.to_string()).unwrap_or_default())
}

/// 可空 uuid 参数（SQL 里包 `NULLIF(?::text,'')::uuid`）。
fn opt_uuid(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

/// 空串 → `None`（自连接取不到来源编号时驱动给的是空串）。
fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

// ==================== SQL ====================

/// 新建合同（16 个绑定参数）。
pub const INSERT_SQL: &str = "\
INSERT INTO biz_employee_contract \
    (id, org_id, employee_id, contract_no, contract_type, sign_date, start_date, end_date, \
     probation_months, status, terminated_date, terminate_reason, renewed_from_id, remark, \
     create_by, update_by) \
 VALUES (?::uuid, ?::uuid, ?::uuid, ?, ?, ?::text::date, ?::text::date, \
         NULLIF(?::text,'')::date, ?::int, ?, NULLIF(?::text,'')::date, NULLIF(?::text,''), \
         NULLIF(?::text,'')::uuid, NULLIF(?::text,''), NULLIF(?::text,'')::uuid, \
         NULLIF(?::text,'')::uuid)";

/// 修改合同的**可写列**（10 个绑定参数：8 列 + `updated_at` + `id`）。
///
/// **不含** `employee_id`（合同不能换人，换人 = 新合同）/ `org_id`（隔离维度）/
/// `status` / `terminated_date` / `terminate_reason`（状态走 [`set_status`] + 状态机）/
/// `renewed_from_id`（续签链是审计事实）。单测
/// `update_sql_never_touches_guarded_columns` 钉住。
pub const UPDATE_SQL: &str = "\
UPDATE biz_employee_contract SET \
    contract_no = ?, contract_type = ?, sign_date = ?::text::date, start_date = ?::text::date, \
    end_date = NULLIF(?::text,'')::date, probation_months = ?::int, remark = NULLIF(?::text,''), \
    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
  WHERE id = ?::uuid AND deleted_at IS NULL";

/// 状态 CAS 回写（7 个绑定参数；只能由 `shared::state_machine::transition` 之后调用）。
///
/// `WHERE status = ?` = 乐观锁：并发两次流转只有一次能改（后到的 `rows = 0`，
/// 接口层翻成「已被他人处理」）。
pub const SET_STATUS_SQL: &str = "\
UPDATE biz_employee_contract SET \
    status = ?, terminated_date = NULLIF(?::text,'')::date, \
    terminate_reason = NULLIF(?::text,''), updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
 WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL";

/// 单条投影（`renewed_from_no` 走自连接，DTO 不再查库）。
const ROW_SQL: &str = "\
SELECT c.id, c.employee_id, c.contract_no, c.contract_type, c.sign_date, c.start_date, c.end_date, \
       c.probation_months, c.status, c.terminated_date, c.terminate_reason, c.renewed_from_id, \
       COALESCE(r.contract_no, '') AS renewed_from_no, c.remark, c.created_at, c.updated_at \
  FROM biz_employee_contract c \
  LEFT JOIN biz_employee_contract r ON r.id = c.renewed_from_id";

/// 一个员工的全部合同（台账时间线：新的在前）。
pub const LIST_SQL: &str = "\
SELECT c.id, c.employee_id, c.contract_no, c.contract_type, c.sign_date, c.start_date, c.end_date, \
       c.probation_months, c.status, c.terminated_date, c.terminate_reason, c.renewed_from_id, \
       COALESCE(r.contract_no, '') AS renewed_from_no, c.remark, c.created_at, c.updated_at \
  FROM biz_employee_contract c \
  LEFT JOIN biz_employee_contract r ON r.id = c.renewed_from_id \
 WHERE c.employee_id = ?::uuid AND c.deleted_at IS NULL \
 ORDER BY c.start_date DESC, c.contract_no DESC";

/// 续签链：从给定合同沿 `renewed_from_id` **回溯到原合同**（递归 CTE，一次查询）。
///
/// 结果按 `depth DESC` 排 = 「原合同 → 第一次续签 → … → 当前合同」，
/// 前端时间线可以直接顺序渲染。链上任一行被软删时链就断在那里（不补假行）。
pub const CHAIN_SQL: &str = "\
WITH RECURSIVE chain AS ( \
    SELECT c.id, c.renewed_from_id, 0 AS depth \
      FROM biz_employee_contract c \
     WHERE c.id = ?::uuid AND c.deleted_at IS NULL \
    UNION ALL \
    SELECT p.id, p.renewed_from_id, chain.depth + 1 \
      FROM biz_employee_contract p \
      JOIN chain ON p.id = chain.renewed_from_id \
     WHERE p.deleted_at IS NULL \
) \
SELECT c.id, c.employee_id, c.contract_no, c.contract_type, c.sign_date, c.start_date, c.end_date, \
       c.probation_months, c.status, c.terminated_date, c.terminate_reason, c.renewed_from_id, \
       COALESCE(r.contract_no, '') AS renewed_from_no, c.remark, c.created_at, c.updated_at \
  FROM chain \
  JOIN biz_employee_contract c ON c.id = chain.id \
  LEFT JOIN biz_employee_contract r ON r.id = c.renewed_from_id \
 ORDER BY chain.depth DESC";

/// 该员工是否有生效中的合同（列表 / 转正守卫 / 续签提示共用）。
pub const HAS_ACTIVE_SQL: &str = "\
SELECT 1 FROM biz_employee_contract \
 WHERE employee_id = ?::uuid AND status = 'ACTIVE' AND deleted_at IS NULL LIMIT 1";

// ==================== 写操作 ====================

/// 新建合同（`POST /business/hr/employee/{id}/contract`；续签也走这里，见 `renewed_from_id`）。
///
/// # 参数
///
/// - `id`：主键（调用方生成，便于同事务内继续写状态日志 / 读链）；
/// - `employee_id`：所属员工（外键 `ON DELETE RESTRICT`）；
/// - `draft`：可写列；
/// - `status`：初始状态（接口层只允许 `ACTIVE` / `EXPIRED`，见 `contract_dto`）；
/// - `renewed_from_id`：续签来源（首份合同为 `None`）；
/// - `operator`：操作人（写 `create_by` / `update_by`）。
pub async fn insert(
    ex: &impl SqlExecutor,
    id: Uuid,
    employee_id: Uuid,
    draft: &ContractWrite,
    status: &str,
    renewed_from_id: Option<Uuid>,
    operator: Option<Uuid>,
) -> DbResult<()> {
    // SCOPE-EXEMPT: 主数据写入；org_id 由 SENTINEL_ORG_ID 显式给出（多组织落地时改一处）。
    ex.exec_sql(
        INSERT_SQL.to_string(),
        insert_params(id, employee_id, draft, status, renewed_from_id, operator),
    )
    .await?;
    Ok(())
}

/// [`INSERT_SQL`] 的绑定参数（顺序与列一致；单测把 `?` 个数与这里的长度对拍）。
fn insert_params(
    id: Uuid,
    employee_id: Uuid,
    draft: &ContractWrite,
    status: &str,
    renewed_from_id: Option<Uuid>,
    operator: Option<Uuid>,
) -> Vec<Value> {
    vec![
        Value::from(&id),
        Value::from(&SENTINEL_ORG_ID),
        Value::from(&employee_id),
        Value::Text(draft.contract_no.trim().to_string()),
        Value::Text(draft.contract_type.clone()),
        Value::Text(draft.sign_date.to_string()),
        Value::Text(draft.start_date.to_string()),
        opt_date(draft.end_date),
        Value::from(i32::from(draft.probation_months)),
        Value::Text(status.to_string()),
        Value::Text(String::new()), // terminated_date：新建时永远为空（ck_…_term 双向约束）
        Value::Text(String::new()), // terminate_reason
        opt_uuid(renewed_from_id),
        opt_text(draft.remark.as_deref()),
        opt_uuid(operator),
        opt_uuid(operator),
    ]
}

/// 修改合同的可写列（`PUT /business/hr/contract/{contract_id}`）。
///
/// 返回受影响行数：`0` = 不存在 / 已软删（调用方据此报 [`not_found_error`]）。
pub async fn update(ex: &impl SqlExecutor, id: Uuid, draft: &ContractWrite, operator: Option<Uuid>) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主数据写入。
    ex.exec_sql(UPDATE_SQL.to_string(), update_params(id, draft, operator))
        .await
}

/// [`UPDATE_SQL`] 的绑定参数。
fn update_params(id: Uuid, draft: &ContractWrite, operator: Option<Uuid>) -> Vec<Value> {
    vec![
        Value::Text(draft.contract_no.trim().to_string()),
        Value::Text(draft.contract_type.clone()),
        Value::Text(draft.sign_date.to_string()),
        Value::Text(draft.start_date.to_string()),
        opt_date(draft.end_date),
        Value::from(i32::from(draft.probation_months)),
        opt_text(draft.remark.as_deref()),
        Value::from(Utc::now()),
        opt_uuid(operator),
        Value::from(&id),
    ]
}

/// 状态 CAS 回写（必须紧跟 `state_machine::transition` 之后调用）。
///
/// # 参数
///
/// - `from_status`：状态机读到的旧值（CAS 条件）；
/// - `to_status`：目标状态；
/// - `terminated_date` / `terminate_reason`：只有 `→TERMINATED` 时才给值，
///   其它目标状态必须都为 `None`（否则 `ck_biz_emp_contract_term` 拒）。
///
/// # 返回值
///
/// 受影响行数：`0` = 状态已被他人改过（并发保护，调用方报 409）。
pub async fn set_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from_status: &str,
    to_status: &str,
    terminated_date: Option<NaiveDate>,
    terminate_reason: Option<&str>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主数据写入。
    ex.exec_sql(
        SET_STATUS_SQL.to_string(),
        vec![
            Value::Text(to_status.to_string()),
            opt_date(terminated_date),
            opt_text(terminate_reason),
            Value::from(Utc::now()),
            opt_uuid(operator),
            Value::from(&id),
            Value::Text(from_status.to_string()),
        ],
    )
    .await
}

// ==================== 读操作 ====================

/// 按 id 取单条（不含已软删）。
pub async fn find_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ContractRow>> {
    let sql = format!("{ROW_SQL} WHERE c.id = ?::uuid AND c.deleted_at IS NULL");
    let row = ex.query_first_sql(sql, vec![Value::from(&id)]).await?;
    row.as_ref().map(ContractRow::from_row).transpose()
}

/// 按 id 取单条并加行锁（`FOR UPDATE`）：改 / 解除 / 续签前先锁，
/// 两个并发请求不会各读一份旧状态再各写一次。
pub async fn lock_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ContractRow>> {
    let sql = format!("{ROW_SQL} WHERE c.id = ?::uuid AND c.deleted_at IS NULL FOR UPDATE OF c");
    let row = ex.query_first_sql(sql, vec![Value::from(&id)]).await?;
    row.as_ref().map(ContractRow::from_row).transpose()
}

/// 一个员工的全部合同（时间线，`start_date DESC`）。
///
/// 员工详情（HR-12 的 `load_contracts`）直接吃这一份 —— 合同 SQL 只有本文件一个出口。
pub async fn list_by_employee(ex: &impl SqlExecutor, employee_id: Uuid) -> DbResult<Vec<ContractInput>> {
    let rows = ex
        .query_sql(LIST_SQL.to_string(), vec![Value::from(&employee_id)])
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(ContractRow::from_row(row)?.to_input());
    }
    Ok(out)
}

/// 续签链（原合同 → … → 给定合同，按时间正向）。
///
/// 验收项「续签链能追溯到原合同」的读取实现：从最新一份合同出发，
/// 递归回溯 `renewed_from_id`，返回链上每一份（含原合同）。
pub async fn chain_of(ex: &impl SqlExecutor, contract_id: Uuid) -> DbResult<Vec<ContractInput>> {
    let rows = ex
        .query_sql(CHAIN_SQL.to_string(), vec![Value::from(&contract_id)])
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(ContractRow::from_row(row)?.to_input());
    }
    Ok(out)
}

/// 该员工是否有生效中的合同（转正守卫 / 续签提示）。
pub async fn has_active(ex: &impl SqlExecutor, employee_id: Uuid) -> DbResult<bool> {
    let found = ex
        .query_first_sql(HAS_ACTIVE_SQL.to_string(), vec![Value::from(&employee_id)])
        .await?;
    Ok(found.is_some())
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 建表迁移原文（编译期嵌入）：约束名 / 索引名改了就红。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    fn placeholders(sql: &str) -> usize {
        sql.matches('?').count()
    }

    fn sample_draft() -> ContractWrite {
        ContractWrite {
            contract_no: "HT-2026-007".to_string(),
            contract_type: "FIXED_TERM".to_string(),
            sign_date: NaiveDate::from_ymd_opt(2026, 1, 1).expect("合法日期"),
            start_date: NaiveDate::from_ymd_opt(2026, 1, 5).expect("合法日期"),
            end_date: NaiveDate::from_ymd_opt(2029, 1, 4),
            probation_months: 3,
            remark: Some("测试".to_string()),
        }
    }

    fn sample_row() -> ContractRow {
        ContractRow {
            id: Uuid::nil(),
            employee_id: Uuid::from_u128(9),
            contract_no: "HT-2026-007".to_string(),
            contract_type: "FIXED_TERM".to_string(),
            sign_date: NaiveDate::from_ymd_opt(2026, 1, 1).expect("合法日期"),
            start_date: NaiveDate::from_ymd_opt(2026, 1, 5).expect("合法日期"),
            end_date: Some(NaiveDate::from_ymd_opt(2029, 1, 4).expect("合法日期")),
            probation_months: 3,
            status: "ACTIVE".to_string(),
            terminated_date: None,
            terminate_reason: None,
            renewed_from_id: Some(Uuid::from_u128(9)),
            renewed_from_no: Some("HT-2023-001".to_string()),
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// 唯一索引名必须与 `V0383` 逐字一致（冲突翻译按名字分支，改名字不会编译报错）。
    #[test]
    fn unique_key_names_match_v0383_ddl() {
        for key in ContractUniqueKey::ALL {
            assert_eq!(key.constraint(), UK_CONTRACT_NO);
            assert!(
                V0383_DDL.contains(&format!("CREATE UNIQUE INDEX IF NOT EXISTS {}", key.constraint())),
                "{} 不在 V0383 的 CREATE UNIQUE INDEX 里 —— 索引被改名了？",
                key.constraint()
            );
        }
    }

    /// 7 个 `CHECK` 都在迁移里，且每一个都有**人话**文案（不是兜底文案）。
    #[test]
    fn every_check_constraint_has_a_human_message() {
        for name in CONTRACT_CHECKS {
            assert!(V0383_DDL.contains(name), "{name} 不在 V0383 里 —— 约束被改名了？");
            let message = check_message(Some(name));
            assert!(
                !message.contains("不满足库约束"),
                "{name} 没有专属文案，用户会看到运维语言"
            );
            assert!(!message.trim().is_empty());
        }
        assert!(check_message(None).contains("不满足库约束"), "未知约束走兜底");
    }

    /// 约束名大小写不敏感（驱动不同版本报的 case 不完全一致）。
    #[test]
    fn classify_unique_is_case_insensitive() {
        assert_eq!(
            classify_unique(Some("UK_BIZ_EMP_CONTRACT_NO")),
            Some(ContractUniqueKey::ContractNo)
        );
        assert_eq!(classify_unique(Some("uk_biz_employee_no")), None, "别的表的键不认");
        assert_eq!(classify_unique(None), None);
    }

    /// 冲突文案必须把编号原样报回来（HR 要拿纸质合同核对）。
    #[test]
    fn conflict_message_names_the_contract_no() {
        let message = conflict_message(ContractUniqueKey::ContractNo, "HT-2026-007");
        assert!(message.contains("HT-2026-007"));
        assert!(message.contains("唯一"));
        assert_eq!(ContractUniqueKey::ContractNo.code(), 68005);
    }

    /// `?` 个数与绑定参数长度必须相等（少一个只在第一次真插时报错）。
    #[test]
    fn insert_sql_parameter_count_matches() {
        let params = insert_params(
            Uuid::nil(),
            Uuid::from_u128(9),
            &sample_draft(),
            "ACTIVE",
            None,
            Some(Uuid::nil()),
        );
        assert_eq!(placeholders(INSERT_SQL), params.len());
        assert_eq!(params.len(), 16);
    }

    #[test]
    fn update_sql_parameter_count_matches() {
        let params = update_params(Uuid::nil(), &sample_draft(), Some(Uuid::nil()));
        assert_eq!(placeholders(UPDATE_SQL), params.len());
        assert_eq!(params.len(), 10);
    }

    #[test]
    fn set_status_sql_parameter_count_matches() {
        assert_eq!(placeholders(SET_STATUS_SQL), 7);
        assert_eq!(placeholders(HAS_ACTIVE_SQL), 1);
        assert_eq!(placeholders(LIST_SQL), 1);
        assert_eq!(placeholders(CHAIN_SQL), 1);
    }

    /// 修改 SQL **不许**碰这几列：换人（employee_id）/ 组织（org_id）/ 状态与解除三列 /
    /// 续签链（renewed_from_id）。写了就是「改一行合同顺手改了审计事实」。
    #[test]
    fn update_sql_never_touches_guarded_columns() {
        let set_part = UPDATE_SQL
            .split("SET")
            .nth(1)
            .expect("UPDATE 语句必须有 SET")
            .split("WHERE")
            .next()
            .expect("SET 之后必须有 WHERE");
        for column in [
            "employee_id",
            "org_id",
            "status",
            "terminated_date",
            "terminate_reason",
            "renewed_from_id",
            "deleted_at",
        ] {
            assert!(
                !set_part.contains(column),
                "UPDATE_SQL 的 SET 部分不许出现 {column}（走状态机 / 专用入口）"
            );
        }
        for column in [
            "contract_no",
            "contract_type",
            "sign_date",
            "start_date",
            "end_date",
            "probation_months",
            "remark",
        ] {
            assert!(set_part.contains(column), "UPDATE_SQL 缺可写列 {column}");
        }
    }

    /// 列表与链的读取必须带续签来源编号（自连接），且排序是「台账口径」。
    #[test]
    fn list_and_chain_sql_carry_renew_source() {
        for sql in [LIST_SQL, CHAIN_SQL] {
            assert!(sql.contains("renewed_from_no"), "缺续签来源编号列");
            assert!(sql.contains("LEFT JOIN biz_employee_contract r"), "缺自连接");
            assert!(sql.contains("deleted_at IS NULL"), "必须排除软删行");
        }
        assert!(LIST_SQL.contains("ORDER BY c.start_date DESC"), "台账新的在前");
        assert!(CHAIN_SQL.contains("WITH RECURSIVE chain"), "续签链必须递归回溯");
        assert!(CHAIN_SQL.contains("ORDER BY chain.depth DESC"), "链要从原合同排到最新");
    }

    /// 行 → DTO 投影：12 列一对一，空串来源编号要变 `None`。
    #[test]
    fn row_projects_to_contract_input() {
        let input = sample_row().to_input();
        assert_eq!(input.id, Uuid::nil());
        assert_eq!(input.contract_no, "HT-2026-007");
        assert_eq!(input.renewed_from_no.as_deref(), Some("HT-2023-001"));
        assert_eq!(input.probation_months, 3);
        assert_eq!(input.status, "ACTIVE");

        let mut row = sample_row();
        row.renewed_from_no = Some("   ".to_string());
        assert_eq!(row.to_input().renewed_from_no, None, "空白来源编号 = 没有");
    }

    /// 封闭终态判定（已解除 / 已续签不可改）。
    #[test]
    fn locked_statuses_follow_state_machine_registry() {
        let mut row = sample_row();
        for status in ["TERMINATED", "RENEWED"] {
            row.status = status.to_string();
            assert!(row.is_locked(), "{status} 必须锁");
        }
        for status in ["ACTIVE", "EXPIRED"] {
            row.status = status.to_string();
            assert!(!row.is_locked(), "{status} 不该锁（补正错日期是常态）");
        }
    }
}
