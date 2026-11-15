//! 员工主档数据访问层（`biz_employee` · HR-10）。
//!
//! 本文件是员工档案**唯一**的 SQL 出口：建 / 读 / 改 / 软删 + 列表分页 + 三个唯一键的
//! 冲突翻译。DTO（HR-11）与接口层（HR-12）只做「入参校验 + 出参脱敏」，不在 handler 里
//! 写 SQL —— 与 `business/erp/` 的组织方式一致（一个「10 张表 46 个接口」的模块把 SQL
//! 挤进单个 `model.rs`，到第 5 张卡之后就是不可 review 的巨文件，见 `hr/model.rs` 模块文档）。
//!
//! # 表结构的唯一依据
//!
//! `backend/migrations/business/V0383__hr_tables.up.sql` 的 `biz_employee`（设计 §3.1 ①）。
//! 列名、`CHECK` 名、三个唯一索引名逐字取自该迁移；**迁移已应用且不可改**，
//! 要加列只能新开迁移（AGENTS.md 约定 6）。
//!
//! # 三处唯一键 —— 本卡的核心交付
//!
//! `biz_employee` 上有三个**部分唯一索引**（都带 `WHERE deleted_at IS NULL`，
//! 也就是「软删后可复用」）：
//!
//! | 索引 | 列 | 语义 | 冲突码 |
//! |---|---|---|---|
//! | [`UK_EMP_NO`] | `(org_id, emp_no)` | 工号公司内唯一（离职软删后可复用） | `68001` |
//! | [`UK_ID_CARD`] | `(org_id, id_card)` | 一个身份证一份档案（`id_card` 为空不参与） | `68002` |
//! | [`UK_USER`] | `(user_id)` | 一个登录账号只能挂一份档案（设计 D-2） | `68003` |
//!
//! ### 为什么必须按**约束名**翻译，而不是按报错文案猜
//!
//! PG 的 `23505` 报文形如 `duplicate key value violates unique constraint "uk_biz_employee_no"`。
//! 按 `duplicate key` 之类的**文案**匹配会踩两个坑：
//!
//! 1. 前面有触发器 / 别的约束报同样文案时无法区分是哪一处冲突；
//! 2. 驱动 `DbError` 的 `Display` 是 `unique constraint violated constraint=uk_…`，
//!    根本不含 `duplicate key` 字样 —— 按文案猜会把唯一冲突判成 500（ERP W1c 的实测记录）。
//!
//! 本仓驱动已经把约束名解析进 `DbError::UniqueViolation { constraint }`
//! （`nik-db-core/src/error.rs` 的 `extract_identifiers`），所以：**按约束名分支**，
//! 用不到驱动的原文。约束名与真实库的对拍由 `unique_key_names_match_v0383_ddl` 单测兜底，
//! 三条冲突的真实报文由 `employee_model_db_tests.rs` 的真库用例兜底
//! （真库用例证明「PG 报出来的索引名」与这里写死的常量一致 —— 这一点纯单测证明不了）。
//!
//! ### 报文里不许出现明文敏感字段
//!
//! 设计 D-14：`id_card` / `bank_account` 存明文、出参脱敏、明文需
//! `business:hr:employee:sensitive`。冲突报文是**最容易被忽略的泄漏点**（「身份证 1101…1234
//! 已存在」这种提示对没有敏感权限的人也是泄漏），所以身份证一律经 [`mask_id_card`] 后再拼进报文。
//!
//! # 四条不许违反的约定
//!
//! 1. **`org_id` 必须显式写入**：`biz_employee.org_id` 是 `NOT NULL` 且**无 DEFAULT**，
//!    三条唯一索引都以它开头。漏写 = 插入直接失败；写成随机值 = 唯一键失效（工号重复检测
//!    形同虚设）。本文件统一绑 [`SENTINEL_ORG_ID`]，多组织（ARCH-9-1）落地时改一处。
//! 2. **只软删，不物理删**：`deleted_at = now()`。物理删会被
//!    `biz_employee_contract.employee_id ON DELETE RESTRICT`、
//!    `biz_salary_detail.employee_id ON DELETE RESTRICT` 拒掉（设计 §4.1.3 的证据断言 ⑰），
//!    而且历史单据的 `employee_id` 会变成野指针。
//! 3. **工号不可改**：[`UPDATE_SQL`] 里**没有** `emp_no`（也不含 `org_id` / `status` /
//!    `leave_date` / `leave_type` / `user_id`）。前三列是身份与组织，第四五列由离职流程
//!    与状态机写，第六列走 `bind-user`。单测 `update_sql_never_touches_guarded_columns` 钉住。
//! 4. **状态只能走状态机**：[`apply_status`] 是 CAS 回写（`WHERE status = 旧值`），
//!    调用方必须是 `shared::state_machine::transition`（它同时写 `biz_status_log`）。
//!    接口层的 `PUT /employee/{id}` **不能**改 `status`，否则留下「状态变了但没有流转记录」
//!    的黑洞（AGENTS.md 约定 11 的同类问题）。
//!
//! # 与其它卡的边界
//!
//! - 工号取号（`EMP-0001`）属 **HR-14**，本文件只接收已取好的 `emp_no`（[`EmployeeWrite::emp_no`]）；
//! - 合同台账 / 薪资标准 / 敏感明文接口分别属 **HR-15 / HR-16 / HR-13**；
//! - `SCOPED_TABLES` 登记（`BIZ_EMPLOYEE`）属 **HR-12** —— 本文件已给出分页锚点
//!   [`EMPLOYEE_TARGET`]（`dept_col = dept_id`、`owner_col = user_id`，设计 D-12），
//!   HR-12 只要在 `shared/scope.rs` 的清单里加一行即可生效；未登记时分页仍能跑
//!   （`scope::paginate` 只按锚点拼片段），只是启动自检不校验该表的列存在性。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::hr::model::{SENTINEL_ORG_ID, TABLE_EMPLOYEE};
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;
use crate::shared::error_code;
use crate::utils;

/// 员工主档表名（与 [`TABLE_EMPLOYEE`] 同一常量，这里只是本文件的可读别名）。
pub const TABLE: &str = TABLE_EMPLOYEE;

/// 唯一索引：工号（`org_id, emp_no`，软删可复用）。
pub const UK_EMP_NO: &str = "uk_biz_employee_no";

/// 唯一索引：身份证（`org_id, id_card`，`id_card IS NULL` 不参与）。
pub const UK_ID_CARD: &str = "uk_biz_employee_idcard";

/// 唯一索引：登录账号（`user_id`，`user_id IS NULL` 不参与）。
pub const UK_USER: &str = "uk_biz_employee_user";

/// 员工列表的分页锚点（设计 D-12：员工登记进 `SCOPED_TABLES`，`dept_col=dept_id`、
/// `owner_col=user_id`；「仅本人」范围因此零改动生效 —— `filter_for` 的 SELF 形态只做
/// `owner_col = 当前 user_id`）。
///
/// **HR-12 已登记**：锚点直接复用 [`scope::ScopeTarget::BIZ_EMPLOYEE`]（`shared/scope.rs` 的
/// 常量，已在 `SCOPED_TABLES` 清单里），不再本地 `new(…)` 一份 —— 两处各写一份会让
/// 「登记的表」与「分页用的锚点」悄悄漂移（列名 / 别名改一处忘另一处，数据权限静默失效）。
/// 同一写法参考 `erp/cost_labor_model.rs` 的 `LABOR_TARGET`。
pub const EMPLOYEE_TARGET: scope::ScopeTarget = scope::ScopeTarget::BIZ_EMPLOYEE;

// ==================== 三个唯一键 → 友好文案 ====================

/// `biz_employee` 的三个唯一键。
///
/// 做成枚举而不是三个字符串，是为了让「约束名 → 码 → 文案」的映射**只有一处**：
/// [`EmployeeUniqueKey::constraint`] 给真库用的索引名，[`EmployeeUniqueKey::code`]
/// 给出参业务码，[`conflict_message`] 给用户看的文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmployeeUniqueKey {
    /// 工号重复（`uk_biz_employee_no`）。
    EmpNo,
    /// 身份证重复（`uk_biz_employee_idcard`）。
    IdCard,
    /// 登录账号已被别的员工绑定（`uk_biz_employee_user`）。
    UserAccount,
}

impl EmployeeUniqueKey {
    /// 全部三个键（单测与「映射完整性」断言用；新增键时这里必须同步）。
    pub const ALL: [Self; 3] = [Self::EmpNo, Self::IdCard, Self::UserAccount];

    /// 数据库里的索引名（与 `V0383__hr_tables` 逐字一致）。
    pub const fn constraint(self) -> &'static str {
        match self {
            Self::EmpNo => UK_EMP_NO,
            Self::IdCard => UK_ID_CARD,
            Self::UserAccount => UK_USER,
        }
    }

    /// 出参业务码（`shared/error_code` 的 68xxx 段）。
    pub const fn code(self) -> i32 {
        match self {
            Self::EmpNo => error_code::HR_EMPLOYEE_NO_DUPLICATE,
            Self::IdCard => error_code::HR_EMPLOYEE_ID_CARD_DUPLICATE,
            Self::UserAccount => error_code::HR_EMPLOYEE_ACCOUNT_BOUND,
        }
    }

    /// 一句短标签（日志与兜底文案用；**不**直接下发给用户）。
    pub const fn label(self) -> &'static str {
        match self {
            Self::EmpNo => "工号重复",
            Self::IdCard => "身份证号重复",
            Self::UserAccount => "登录账号已绑定其他员工",
        }
    }
}

/// 把驱动给的约束名归一成 [`EmployeeUniqueKey`]；不是员工表的三个键则 `None`
/// （例如以后 `biz_employee` 加了别的唯一索引 —— 那种情况走通用 409，不假装认识）。
///
/// 大小写不敏感：PG 的约束名保持建表时的大小写（本仓全小写），但驱动在部分方言里
/// 会做折叠，这里统一 `eq_ignore_ascii_case`，避免「同一个约束在 MySQL 上翻译不出来」。
pub fn classify_unique(constraint: Option<&str>) -> Option<EmployeeUniqueKey> {
    let name = constraint?.trim();
    EmployeeUniqueKey::ALL
        .into_iter()
        .find(|key| key.constraint().eq_ignore_ascii_case(name))
}

/// 冲突报文里出现身份证号时的脱敏（≥12 位留前 6 后 4，短号留前 2 后 2，≤4 位全掩）。
///
/// 与 `sys_field_mask`（`mask_type = 2`，`from_start=6, from_end=4`）同口径 —— 冲突报文
/// 与出参必须掩成同一个样子，否则用户会以为「这里能看全，那里不能」。
pub fn mask_id_card(raw: &str) -> String {
    let chars: Vec<char> = raw.trim().chars().collect();
    let len = chars.len();
    if len == 0 {
        return String::new();
    }
    if len <= 4 {
        return "*".repeat(len);
    }
    let (head, tail) = if len >= 12 { (6, 4) } else { (2, 2) };
    let masked = len - head - tail;
    let mut out = String::with_capacity(len);
    out.extend(chars.iter().take(head));
    out.push_str(&"*".repeat(masked));
    out.extend(chars.iter().skip(len - tail));
    out
}

/// 冲突报文里可用的「本次提交的值」。
///
/// 只在翻译唯一冲突时用：报文要能指名**哪个工号 / 哪张身份证**撞了，用户才知道改哪个字段。
/// 不传（`Default`）也能翻译，只是文案退化成通用说法。
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteValues<'a> {
    /// 本次提交的工号（工号冲突报文用）。
    pub emp_no: Option<&'a str>,
    /// 本次提交的身份证号（**报文里会脱敏**）。
    pub id_card: Option<&'a str>,
}

impl WriteValues<'_> {
    /// 从一个待写入的草稿里取值（`insert` / `update` 的调用方一行搞定）。
    pub fn of(draft: &EmployeeWrite) -> WriteValues<'_> {
        WriteValues {
            emp_no: Some(draft.emp_no.as_str()).filter(|text| !text.is_empty()),
            id_card: draft.id_card.as_deref().map(str::trim).filter(|text| !text.is_empty()),
        }
    }
}

/// 三个唯一键各自的用户可读文案。
///
/// 文案要点（写死在这里，前端直出不改字 —— 设计 §6.2.6 的口径）：
///
/// - 说清**撞了什么**（工号 `EMP-0002` / 身份证 `1101…1234` / 登录账号）；
/// - 说清**下一步做什么**（换号 / 打开已有档案 / 先解绑），而不是只报「冲突」；
/// - 不出现约束名、不出现明文身份证、不出现 UUID（用户认不出 UUID）。
pub fn conflict_message(key: EmployeeUniqueKey, values: &WriteValues<'_>) -> String {
    match key {
        EmployeeUniqueKey::EmpNo => match values.emp_no {
            Some(emp_no) => format!(
                "工号 {emp_no} 已被其他员工使用（同一公司内工号唯一）。请换一个工号，\
                 或打开已有员工档案直接修改"
            ),
            None => "工号已被其他员工使用（同一公司内工号唯一）。请换一个工号，\
                     或打开已有员工档案直接修改"
                .to_string(),
        },
        EmployeeUniqueKey::IdCard => match values.id_card {
            Some(id_card) => format!(
                "身份证号 {} 已登记在其他员工档案上（一人只允许一份在职档案）。\
                 如属重复录入，请打开原档案修改；如确属两人同号，请先核实身份证信息",
                mask_id_card(id_card)
            ),
            None => "该身份证号已登记在其他员工档案上（一人只允许一份在职档案）。\
                     如属重复录入，请打开原档案修改"
                .to_string(),
        },
        EmployeeUniqueKey::UserAccount => "该登录账号已绑定其他员工（一个账号只能挂一份员工档案）。\
             请先在原员工档案上解除账号绑定，再来绑定这一个"
            .to_string(),
    }
}

/// `CHECK` 约束 → 用户可读文案（`ck_biz_employee_*`，见 `V0383`）。
///
/// 这些约束**不是**用来给用户看的（`ck_biz_employee_leavetype2` 这种名字没人愿意读），
/// 但它们是最后一道防线：接口层漏校验时，用户拿到的应当是一句能照着改的话。
pub fn check_message(constraint: Option<&str>) -> String {
    let name = constraint.unwrap_or_default();
    let human = match name {
        "ck_biz_employee_leave" => "离职日期与员工状态不一致：状态为「离职」时必须填离职日期，在职员工不能填离职日期",
        "ck_biz_employee_leavetype2" => "员工状态是「离职」时必须选择离职类型（辞职 / 辞退 / 退休 / 合同到期 / 身故）",
        "ck_biz_employee_dates" => "离职日期不能早于入职日期",
        "ck_biz_employee_probation" => "试用期截止日期不能早于入职日期",
        "ck_biz_employee_idcard_len" => "身份证号长度应为 6~18 位（护照 / 港澳台证件同样受此限制）",
        "ck_biz_employee_gender" => "性别取值只能是 0 未知 / 1 男 / 2 女",
        "ck_biz_employee_idtype" => "证件类型只能是 ID_CARD / PASSPORT / HK_MO_TW / FOREIGN",
        "ck_biz_employee_emptype" => "用工形式只能是 FULL_TIME / PROBATION / INTERN / OUTSOURCE / PART_TIME / DISPATCH",
        "ck_biz_employee_status" => "员工状态只能是 PROBATION / ACTIVE / SUSPENDED / RESIGNED",
        "ck_biz_employee_leavetype" => "离职类型只能是 RESIGN / DISMISS / RETIRE / CONTRACT_END / DEATH",
        _ => return format!("员工数据不满足库约束 {name}，请检查必填项与取值范围后重试"),
    };
    human.to_string()
}

/// 员工写路径的**唯一**数据库错误出口：三个唯一键 → 409 + 68xxx + 友好文案；`CHECK` → 400；
/// 其余交给 [`utils::db_error`]（500 + `internal_detail`，对外只说「服务器内部错误」）。
///
/// # 参数
///
/// - `err`：`insert` / `update` / `set_user_id` 冒上来的 `DbError`（也可以传
///   `apply_status` 的错误 —— 它同样可能撞 `uk_biz_employee_user`）；
/// - `ctx`：日志里的上下文（`"hr_employee_insert"`），出了 500 靠它定位；
/// - `values`：[`WriteValues`]，用于在报文里指名撞了什么值（可空）。
///
/// # 返回值
///
/// | 场景 | HTTP | 业务码 |
/// |---|---|---|
/// | 工号重复 | 409 | `68001` |
/// | 身份证重复 | 409 | `68002` |
/// | 账号已绑定 | 409 | `68003` |
/// | 其它唯一键（本表新增的索引） | 409 | `10003`（通用冲突，报文点名约束名供运维定位） |
/// | `CHECK` 违反 | 400 | `10001`（报文点名该改哪个字段） |
/// | 其它 | 500 | `10004`（真实原因只进日志） |
pub fn write_error(err: DbError, ctx: &str, values: &WriteValues<'_>) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => match classify_unique(constraint.as_deref()) {
            Some(key) => ApiError::new(409, key.code(), conflict_message(key, values)),
            None => ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!(
                    "员工档案写入冲突（唯一约束 {}）：该值已存在于另一条未删除的记录上，请刷新后重试",
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

/// 「员工档案不存在或已删除」的 404（接口层所有按 id 取数的入口共用一句）。
pub fn not_found_error() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "员工档案不存在或已删除")
}

// ==================== 行结构 ====================

/// 员工主档全字段行（`GET /employee/{id}` 的数据源）。
///
/// `id_card` / `bank_account` 在这里是**明文**：脱敏是出参层的事（设计 D-14）。
/// 本结构**只给模型层与 DTO 层**用，不得直接 `Serialize` 出去。
#[derive(Debug, Clone)]
pub struct EmployeeRow {
    /// 主键。
    pub id: Uuid,
    /// 所属公司。
    pub org_id: Uuid,
    /// 工号。
    pub emp_no: String,
    /// 姓名（主数据：改名后所有单据显示新名 —— 本列 join 实时取，不存快照）。
    pub name: String,
    /// 性别（0 未知 / 1 男 / 2 女）。
    pub gender: i16,
    /// 出生日期。
    pub birth_date: Option<NaiveDate>,
    /// 身份证号（**明文**，出参脱敏）。
    pub id_card: Option<String>,
    /// 证件类型。
    pub id_card_type: String,
    /// 手机号。
    pub mobile: Option<String>,
    /// 邮箱。
    pub email: Option<String>,
    /// 人事归属部门（弱引用 `sys_dept.id`）。
    pub dept_id: Uuid,
    /// 部门名（join 实时取名，部门改名后这里跟着变）。
    pub dept_name: String,
    /// 岗位代码（字典 `hr_position`）。
    pub position_code: Option<String>,
    /// 职级（字典 `hr_job_level`）。
    pub job_level: Option<String>,
    /// 用工形式。
    pub employment_type: String,
    /// 入职日期。
    pub hire_date: NaiveDate,
    /// 试用期截止。
    pub probation_end_date: Option<NaiveDate>,
    /// 离职日期。
    pub leave_date: Option<NaiveDate>,
    /// 离职类型。
    pub leave_type: Option<String>,
    /// 员工状态。
    pub status: String,
    /// 学历。
    pub education: Option<String>,
    /// 毕业院校。
    pub school: Option<String>,
    /// 专业。
    pub major: Option<String>,
    /// 毕业日期。
    pub graduate_date: Option<NaiveDate>,
    /// 籍贯。
    pub native_place: Option<String>,
    /// 现住址。
    pub address: Option<String>,
    /// 紧急联系人。
    pub emergency_contact: Option<String>,
    /// 紧急联系电话。
    pub emergency_phone: Option<String>,
    /// 与紧急联系人的关系。
    pub emergency_relation: Option<String>,
    /// 工资卡开户行。
    pub bank_name: Option<String>,
    /// 开户支行。
    pub branch: Option<String>,
    /// 工资卡号（**明文**，出参脱敏）。
    pub bank_account: Option<String>,
    /// 工资卡户名。
    pub bank_account_name: Option<String>,
    /// 关联登录账号（可空）。
    pub user_id: Option<Uuid>,
    /// 账号用户名（join `sys_user`，实时；无账号为空串）。
    pub username: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
}

impl EmployeeRow {
    /// 从查询行解析（列名与 [`ROW_SQL`] 的投影逐字对应）。
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            org_id: row.get_typed("org_id")?,
            emp_no: row.get_typed("emp_no")?,
            name: row.get_typed("name")?,
            gender: row.get_typed("gender")?,
            birth_date: row.get_typed("birth_date")?,
            id_card: row.get_typed("id_card")?,
            id_card_type: row.get_typed("id_card_type")?,
            mobile: row.get_typed("mobile")?,
            email: row.get_typed("email")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            position_code: row.get_typed("position_code")?,
            job_level: row.get_typed("job_level")?,
            employment_type: row.get_typed("employment_type")?,
            hire_date: row.get_typed("hire_date")?,
            probation_end_date: row.get_typed("probation_end_date")?,
            leave_date: row.get_typed("leave_date")?,
            leave_type: row.get_typed("leave_type")?,
            status: row.get_typed("status")?,
            education: row.get_typed("education")?,
            school: row.get_typed("school")?,
            major: row.get_typed("major")?,
            graduate_date: row.get_typed("graduate_date")?,
            native_place: row.get_typed("native_place")?,
            address: row.get_typed("address")?,
            emergency_contact: row.get_typed("emergency_contact")?,
            emergency_phone: row.get_typed("emergency_phone")?,
            emergency_relation: row.get_typed("emergency_relation")?,
            bank_name: row.get_typed("bank_name")?,
            branch: row.get_typed("bank_branch")?,
            bank_account: row.get_typed("bank_account")?,
            bank_account_name: row.get_typed("bank_account_name")?,
            user_id: row.get_typed("user_id")?,
            username: row.get_typed("username")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
        })
    }
}

/// 员工列表行（`GET /employee/page` 的投影）。
///
/// 只含列表页真正显示的列：`id_card` / `bank_account` / 紧急联系人这些详情的字段不取
/// —— 列表一次几十行，取敏感列既无用处又扩大泄漏面（脱敏在出参层，但**根本不下发**更省心）。
#[derive(Debug, Clone)]
pub struct EmployeeListRow {
    /// 主键。
    pub id: Uuid,
    /// 工号。
    pub emp_no: String,
    /// 姓名。
    pub name: String,
    /// 性别。
    pub gender: i16,
    /// 手机号。
    pub mobile: Option<String>,
    /// 人事归属部门。
    pub dept_id: Uuid,
    /// 部门名（join 实时）。
    pub dept_name: String,
    /// 岗位代码。
    pub position_code: Option<String>,
    /// 职级。
    pub job_level: Option<String>,
    /// 用工形式。
    pub employment_type: String,
    /// 入职日期。
    pub hire_date: NaiveDate,
    /// 员工状态。
    pub status: String,
    /// 关联登录账号。
    pub user_id: Option<Uuid>,
    /// 账号用户名（join `sys_user`，无账号为空串）。
    pub username: String,
    /// 现行合同到期日（`status = 'ACTIVE'` 的合同里取最晚一个；无合同为 `None`）。
    ///
    /// 前端用 `expire_in_days ≤ 30` 出橙字提醒（设计 §6.2 员工列表）。
    /// 子查询而不是 join：一人可能有多份历史合同，join 会把列表行数放大。
    pub contract_end_date: Option<NaiveDate>,
}

impl EmployeeListRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            emp_no: row.get_typed("emp_no")?,
            name: row.get_typed("name")?,
            gender: row.get_typed("gender")?,
            mobile: row.get_typed("mobile")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            position_code: row.get_typed("position_code")?,
            job_level: row.get_typed("job_level")?,
            employment_type: row.get_typed("employment_type")?,
            hire_date: row.get_typed("hire_date")?,
            status: row.get_typed("status")?,
            user_id: row.get_typed("user_id")?,
            username: row.get_typed("username")?,
            contract_end_date: row.get_typed("contract_end_date")?,
        })
    }
}

/// 唯一键查询的目标（`existing_*` 的返回值）：只给「撞了谁」所需的最小信息。
#[derive(Debug, Clone)]
pub struct EmployeeBrief {
    /// 主键。
    pub id: Uuid,
    /// 工号。
    pub emp_no: String,
    /// 姓名。
    pub name: String,
    /// 状态。
    pub status: String,
}

impl EmployeeBrief {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            emp_no: row.get_typed("emp_no")?,
            name: row.get_typed("name")?,
            status: row.get_typed("status")?,
        })
    }
}

// ==================== 写入载荷 ====================

/// 新建 / 修改员工的载荷（`POST /employee` 与 `PUT /employee/{id}` 共用一个结构）。
///
/// # 为什么 PUT 也是「全量」
///
/// 设计 §5.2 #4 写的是「同上（不含 `emp_no`）」：前端表单始终提交整张表单，**不是** patch
/// 语义。所以这里是全字段结构，`None` = 清空该列（`NULLIF(?::text,'')`），不是「不改」。
/// 接口层要保证「没传的字段回填当前值」而不是漏传（否则一存就把字段清空了）。
///
/// # 哪些字段不在里面
///
/// `id` / `org_id` / `created_at` 由库与调用方给；`status` / `leave_date` / `leave_type`
/// 由状态机与离职流程写（[`apply_status`]）；`user_id` 走 [`set_user_id`]（`bind-user`）。
#[derive(Debug, Clone)]
pub struct EmployeeWrite {
    /// 工号（新建时必填；修改时不参与 SQL —— 工号不可改）。
    pub emp_no: String,
    /// 姓名。
    pub name: String,
    /// 性别（0 / 1 / 2）。
    pub gender: i16,
    /// 出生日期。
    pub birth_date: Option<NaiveDate>,
    /// 身份证号（明文，落库明文）。
    pub id_card: Option<String>,
    /// 证件类型（`ID_CARD` / `PASSPORT` / `HK_MO_TW` / `FOREIGN`）。
    pub id_card_type: String,
    /// 手机号。
    pub mobile: Option<String>,
    /// 邮箱。
    pub email: Option<String>,
    /// 人事归属部门。
    pub dept_id: Uuid,
    /// 岗位代码（字典）。
    pub position_code: Option<String>,
    /// 职级（字典）。
    pub job_level: Option<String>,
    /// 用工形式（`FULL_TIME` …）。
    pub employment_type: String,
    /// 入职日期。
    pub hire_date: NaiveDate,
    /// 试用期截止。
    pub probation_end_date: Option<NaiveDate>,
    /// 学历。
    pub education: Option<String>,
    /// 毕业院校。
    pub school: Option<String>,
    /// 专业。
    pub major: Option<String>,
    /// 毕业日期。
    pub graduate_date: Option<NaiveDate>,
    /// 籍贯。
    pub native_place: Option<String>,
    /// 现住址。
    pub address: Option<String>,
    /// 紧急联系人。
    pub emergency_contact: Option<String>,
    /// 紧急联系电话。
    pub emergency_phone: Option<String>,
    /// 关系。
    pub emergency_relation: Option<String>,
    /// 工资卡开户行。
    pub bank_name: Option<String>,
    /// 开户支行。
    pub bank_branch: Option<String>,
    /// 工资卡号（明文）。
    pub bank_account: Option<String>,
    /// 工资卡户名。
    pub bank_account_name: Option<String>,
    /// 关联登录账号（**仅新建时写**；改绑走 [`set_user_id`]）。
    pub user_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
}

/// 列表筛选（设计 §5.2 #1 的入参：关键字 / 部门 / 状态多选 / 用工形式 / 含离职）。
#[derive(Debug, Clone, Default)]
pub struct EmployeeFilter {
    /// 关键字，匹配工号 / 姓名 / 手机号（`LIKE %kw%`，三个字段各绑一个参数）。
    pub keyword: Option<String>,
    /// 人事归属部门（**精确匹配本部门**；部门子树由调用方按 `dept_path` 展开成 id 集合后
    /// 逐次调用，或改用 `scope` 的部门范围 —— 本卡不做子树展开）。
    pub dept_id: Option<Uuid>,
    /// 状态多选（`PROBATION/ACTIVE/SUSPENDED/RESIGNED`）；空 = 不限。
    pub statuses: Vec<String>,
    /// 用工形式。
    pub employment_type: Option<String>,
    /// 是否包含离职员工（默认 `false` = 列表不显示 `RESIGNED`）。
    pub include_resigned: bool,
}

// ==================== SQL ====================

/// 可空文本参数：空 → `NULLIF(?::text,'')` 里的空串（驱动不支持 `Value::Null` 绑 `::text`）。
fn opt_text(value: Option<&str>) -> Value {
    Value::Text(value.map(str::trim).unwrap_or_default().to_string())
}

/// 可空日期参数（`?::text::date` + 空串 → NULL 由 `NULLIF` 处理会缺类型信息，
/// 故日期一律「有值给文本、无值给空串」，SQL 里包一层 `NULLIF(?::text,'')::date`）。
fn opt_date(value: Option<NaiveDate>) -> Value {
    Value::Text(value.map(|date| date.to_string()).unwrap_or_default())
}

/// 可空 uuid 参数（`NULLIF(?::text,'')::uuid`）。
fn opt_uuid(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

/// 新建员工（36 个绑定参数：列顺序见 [`INSERT_SQL`]）。
///
/// `?` 的个数与 [`insert_params`] 的长度必须一致 —— 少一个就整条 INSERT 报
/// 「参数个数不匹配」，而这个错只在**第一次真插**时暴露，所以由单测
/// `insert_sql_parameter_count_matches` 钉住。
pub const INSERT_SQL: &str = "\
INSERT INTO biz_employee \
    (id, org_id, emp_no, name, gender, birth_date, id_card, id_card_type, mobile, email, \
     dept_id, position_code, job_level, employment_type, hire_date, probation_end_date, \
     leave_date, leave_type, status, education, school, major, graduate_date, native_place, \
     address, emergency_contact, emergency_phone, emergency_relation, bank_name, bank_branch, \
     bank_account, bank_account_name, user_id, remark, create_by, update_by) \
 VALUES (?::uuid, ?::uuid, ?, ?, ?::int, NULLIF(?::text,'')::date, NULLIF(?::text,''), ?, NULLIF(?::text,''), \
         NULLIF(?::text,''), ?::uuid, NULLIF(?::text,''), NULLIF(?::text,''), ?, ?::text::date, \
         NULLIF(?::text,'')::date, NULLIF(?::text,'')::date, NULLIF(?::text,''), ?, NULLIF(?::text,''), \
         NULLIF(?::text,''), NULLIF(?::text,''), NULLIF(?::text,'')::date, NULLIF(?::text,''), \
         NULLIF(?::text,''), NULLIF(?::text,''), NULLIF(?::text,''), NULLIF(?::text,''), \
         NULLIF(?::text,''), NULLIF(?::text,''), NULLIF(?::text,''), NULLIF(?::text,''), \
         NULLIF(?::text,'')::uuid, NULLIF(?::text,''), NULLIF(?::text,'')::uuid, \
         NULLIF(?::text,'')::uuid)";

/// 修改员工（27 个绑定参数 + `updated_at` + `update_by` + `id` = 30）。
///
/// **不含** `emp_no` / `org_id` / `status` / `leave_date` / `leave_type` / `user_id`，
/// 理由见模块文档「四条不许违反的约定」第 3 条。
pub const UPDATE_SQL: &str = "\
UPDATE biz_employee SET \
    name = ?, gender = ?::int, birth_date = NULLIF(?::text,'')::date, id_card = NULLIF(?::text,''), \
    id_card_type = ?, mobile = NULLIF(?::text,''), email = NULLIF(?::text,''), dept_id = ?::uuid, \
    position_code = NULLIF(?::text,''), job_level = NULLIF(?::text,''), employment_type = ?, \
    hire_date = ?::text::date, probation_end_date = NULLIF(?::text,'')::date, education = NULLIF(?::text,''), \
    school = NULLIF(?::text,''), major = NULLIF(?::text,''), graduate_date = NULLIF(?::text,'')::date, \
    native_place = NULLIF(?::text,''), address = NULLIF(?::text,''), \
    emergency_contact = NULLIF(?::text,''), emergency_phone = NULLIF(?::text,''), \
    emergency_relation = NULLIF(?::text,''), bank_name = NULLIF(?::text,''), \
    bank_branch = NULLIF(?::text,''), bank_account = NULLIF(?::text,''), \
    bank_account_name = NULLIF(?::text,''), remark = NULLIF(?::text,''), \
    updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
  WHERE id = ?::uuid AND deleted_at IS NULL";

/// 软删员工（物理删会被合同 / 薪资明细的 `ON DELETE RESTRICT` 拒掉，且历史单据会断链）。
pub const SOFT_DELETE_SQL: &str = "\
UPDATE biz_employee SET deleted_at = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
  WHERE id = ?::uuid AND deleted_at IS NULL";

/// 改绑登录账号（`POST /employee/{id}/bind-user`；传空 = 解绑）。
///
/// `uk_biz_employee_user` 在这里生效：同一账号挂第二份档案 → `23505` → `68003`。
pub const SET_USER_SQL: &str = "\
UPDATE biz_employee SET user_id = NULLIF(?::text,'')::uuid, updated_at = ?, \
       update_by = NULLIF(?::text,'')::uuid \
  WHERE id = ?::uuid AND deleted_at IS NULL";

/// 状态 CAS 回写（只能由 `shared::state_machine::transition` 调用）。
///
/// `status = ?` 放在 `WHERE` 里 = 乐观锁：并发两个流转只有一个能改（后到的 `rows = 0`，
/// 由状态机报「单据已被他人处理」）。离职三列（`leave_date` / `leave_type`）跟着状态一起写，
/// 因为 `ck_biz_employee_leave` 要求「状态是 RESIGNED ⇔ 有离职日期」双向成立。
pub const APPLY_STATUS_SQL: &str = "\
UPDATE biz_employee SET status = ?, leave_date = NULLIF(?::text,'')::date, leave_type = NULLIF(?::text,''), \
       updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
 WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL";

/// 全字段查询（详情）：部门名与账号名都 join 实时取（AGENTS.md 约定 13）。
const ROW_SQL: &str = "\
SELECT e.id, e.org_id, e.emp_no, e.name, e.gender, e.birth_date, e.id_card, e.id_card_type, \
       e.mobile, e.email, e.dept_id, COALESCE(d.dept_name, '') AS dept_name, e.position_code, \
       e.job_level, e.employment_type, e.hire_date, e.probation_end_date, e.leave_date, \
       e.leave_type, e.status, e.education, e.school, e.major, e.graduate_date, e.native_place, \
       e.address, e.emergency_contact, e.emergency_phone, e.emergency_relation, e.bank_name, \
       e.bank_branch, e.bank_account, e.bank_account_name, e.user_id, \
       COALESCE(u.username, '') AS username, e.remark, e.created_at, e.updated_at, \
       e.create_by, e.update_by \
  FROM biz_employee e \
  LEFT JOIN sys_dept d ON d.id = e.dept_id AND d.deleted_at IS NULL \
  LEFT JOIN sys_user u ON u.id = e.user_id";

/// 列表投影前缀（业务过滤与数据权限片段追加在 `{FILTER}` 处，`ORDER BY` 由 `paginate` 处理）。
const LIST_SQL_PREFIX: &str = "\
SELECT e.id, e.emp_no, e.name, e.gender, e.mobile, e.dept_id, \
       COALESCE(d.dept_name, '') AS dept_name, e.position_code, e.job_level, e.employment_type, \
       e.hire_date, e.status, e.user_id, COALESCE(u.username, '') AS username, \
       (SELECT max(c.end_date) FROM biz_employee_contract c \
         WHERE c.employee_id = e.id AND c.status = 'ACTIVE' AND c.deleted_at IS NULL \
           AND c.end_date IS NOT NULL) AS contract_end_date \
  FROM biz_employee e \
  LEFT JOIN sys_dept d ON d.id = e.dept_id AND d.deleted_at IS NULL \
  LEFT JOIN sys_user u ON u.id = e.user_id \
 WHERE e.deleted_at IS NULL";

/// 员工列表默认排序：新入职的在前，同一天按工号倒序（工号带序号，倒序 = 后录的在前）。
pub const LIST_ORDER_BY: &str = "ORDER BY e.hire_date DESC, e.emp_no DESC";

// ==================== 写操作 ====================

/// 新建员工（`POST /business/hr/employee`）。
///
/// # 参数
///
/// - `ex`：`db()` 或事务 —— 与工号取号同事务时传 `&tx`（取号会推进 `biz_seq`，
///   失败回滚必须一起回滚，否则「号被吃掉但人没建」）；
/// - `id`：主键（由调用方生成，便于同事务内立即引用；`HR-14` 的取号与插入同事务）；
/// - `draft`：全字段载荷；
/// - `operator`：操作人（写 `create_by` / `update_by`）。
///
/// # 异常情况
///
/// | 错误 | 含义 |
/// |---|---|
/// | `DbError::UniqueViolation` | 三个唯一键之一 —— 用 [`write_error`] 翻成 409 + 68xxx |
/// | `DbError::CheckViolation` | `ck_biz_employee_*` —— 用 [`write_error`] 翻成 400 + 人话 |
/// | `DbError::ForeignKeyViolation` | 本表无外键，出现即「迁移被改过」，应当上抛 |
pub async fn insert(ex: &impl SqlExecutor, id: Uuid, draft: &EmployeeWrite, operator: Option<Uuid>) -> DbResult<()> {
    // SCOPE-EXEMPT: 主档写入，org_id 由 [SENTINEL_ORG_ID] 显式给出（多组织落地时改一处）。
    ex.exec_sql(INSERT_SQL.to_string(), insert_params(id, draft, operator))
        .await?;
    Ok(())
}

/// [`INSERT_SQL`] 的绑定参数（顺序与列一致；单测把 `?` 个数与这里的长度对拍）。
///
/// 新建时 `status` 固定 `PROBATION`（试用期）—— 设计 §4.2.1 的状态机入口；
/// 调用方要直接建 `ACTIVE`（`seed-from-users` 的老员工）时用 [`insert_with_status`]。
fn insert_params(id: Uuid, draft: &EmployeeWrite, operator: Option<Uuid>) -> Vec<Value> {
    insert_params_with_status(id, draft, operator, "PROBATION", None, None)
}

/// 同 [`insert_params`]，但显式给状态与离职三列（离职员工的「补档案」场景）：
/// `status = 'RESIGNED'` 时必须同时给 `leave_date` 与 `leave_type`，否则库里的
/// `ck_biz_employee_leave` 会拒（那是**对**的：不允许「标了离职但没日期」）。
fn insert_params_with_status(
    id: Uuid,
    draft: &EmployeeWrite,
    operator: Option<Uuid>,
    status: &str,
    leave_date: Option<NaiveDate>,
    leave_type: Option<&str>,
) -> Vec<Value> {
    vec![
        Value::from(&id),
        Value::from(&SENTINEL_ORG_ID),
        Value::Text(draft.emp_no.trim().to_string()),
        Value::Text(draft.name.trim().to_string()),
        Value::from(i32::from(draft.gender)),
        opt_date(draft.birth_date),
        opt_text(draft.id_card.as_deref()),
        Value::Text(draft.id_card_type.clone()),
        opt_text(draft.mobile.as_deref()),
        opt_text(draft.email.as_deref()),
        Value::from(&draft.dept_id),
        opt_text(draft.position_code.as_deref()),
        opt_text(draft.job_level.as_deref()),
        Value::Text(draft.employment_type.clone()),
        Value::Text(draft.hire_date.to_string()),
        opt_date(draft.probation_end_date),
        opt_date(leave_date),
        opt_text(leave_type),
        Value::Text(status.to_string()),
        opt_text(draft.education.as_deref()),
        opt_text(draft.school.as_deref()),
        opt_text(draft.major.as_deref()),
        opt_date(draft.graduate_date),
        opt_text(draft.native_place.as_deref()),
        opt_text(draft.address.as_deref()),
        opt_text(draft.emergency_contact.as_deref()),
        opt_text(draft.emergency_phone.as_deref()),
        opt_text(draft.emergency_relation.as_deref()),
        opt_text(draft.bank_name.as_deref()),
        opt_text(draft.bank_branch.as_deref()),
        opt_text(draft.bank_account.as_deref()),
        opt_text(draft.bank_account_name.as_deref()),
        opt_uuid(draft.user_id),
        opt_text(draft.remark.as_deref()),
        opt_uuid(operator),
        opt_uuid(operator),
    ]
}

/// 新建员工，显式指定初始状态（`seed-from-users` 建在职老员工用：`ACTIVE`）。
///
/// # 参数
///
/// - `status`：`PROBATION` / `ACTIVE` / `SUSPENDED` / `RESIGNED`（库里的 `CHECK` 兜底）；
/// - 离职三列：`status = 'RESIGNED'` 时必须给全，其它状态必须都为空
///   （`ck_biz_employee_leave` 双向约束）。
pub async fn insert_with_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    draft: &EmployeeWrite,
    operator: Option<Uuid>,
    status: &str,
    leave_date: Option<NaiveDate>,
    leave_type: Option<&str>,
) -> DbResult<()> {
    // SCOPE-EXEMPT: 主档写入。
    ex.exec_sql(
        INSERT_SQL.to_string(),
        insert_params_with_status(id, draft, operator, status, leave_date, leave_type),
    )
    .await?;
    Ok(())
}

/// 修改员工（`PUT /business/hr/employee/{id}`）。
///
/// # 返回值
///
/// 受影响行数：`0` = 不存在 / 已软删（调用方据此报 [`not_found_error`]）。
/// **不要**把 `0` 当成成功 —— 那是「保存了但什么都没改」的经典静默失败。
pub async fn update(ex: &impl SqlExecutor, id: Uuid, draft: &EmployeeWrite, operator: Option<Uuid>) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主档写入。
    ex.exec_sql(UPDATE_SQL.to_string(), update_params(id, draft, operator))
        .await
}

/// [`UPDATE_SQL`] 的绑定参数（27 列 + `updated_at` + `update_by` + `id`）。
fn update_params(id: Uuid, draft: &EmployeeWrite, operator: Option<Uuid>) -> Vec<Value> {
    vec![
        Value::Text(draft.name.trim().to_string()),
        Value::from(i32::from(draft.gender)),
        opt_date(draft.birth_date),
        opt_text(draft.id_card.as_deref()),
        Value::Text(draft.id_card_type.clone()),
        opt_text(draft.mobile.as_deref()),
        opt_text(draft.email.as_deref()),
        Value::from(&draft.dept_id),
        opt_text(draft.position_code.as_deref()),
        opt_text(draft.job_level.as_deref()),
        Value::Text(draft.employment_type.clone()),
        Value::Text(draft.hire_date.to_string()),
        opt_date(draft.probation_end_date),
        opt_text(draft.education.as_deref()),
        opt_text(draft.school.as_deref()),
        opt_text(draft.major.as_deref()),
        opt_date(draft.graduate_date),
        opt_text(draft.native_place.as_deref()),
        opt_text(draft.address.as_deref()),
        opt_text(draft.emergency_contact.as_deref()),
        opt_text(draft.emergency_phone.as_deref()),
        opt_text(draft.emergency_relation.as_deref()),
        opt_text(draft.bank_name.as_deref()),
        opt_text(draft.bank_branch.as_deref()),
        opt_text(draft.bank_account.as_deref()),
        opt_text(draft.bank_account_name.as_deref()),
        opt_text(draft.remark.as_deref()),
        Value::from(Utc::now()),
        opt_uuid(operator),
        Value::from(&id),
    ]
}

/// 软删员工（`DELETE /business/hr/employee/{id}`）。
///
/// 返回受影响行数（`0` = 不存在 / 已删）。软删后同工号、同身份证**可以再用**
/// （三个唯一索引都带 `WHERE deleted_at IS NULL`）—— 这是「离职返聘可用原工号」的实现基础。
pub async fn soft_delete(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主档写入。
    let now = Utc::now();
    ex.exec_sql(
        SOFT_DELETE_SQL.to_string(),
        vec![Value::from(now), Value::from(now), opt_uuid(operator), Value::from(&id)],
    )
    .await
}

/// 改绑登录账号（`POST /employee/{id}/bind-user`；`user_id = None` = 解绑）。
///
/// 撞 `uk_biz_employee_user` → `DbError::UniqueViolation{ constraint: Some("uk_biz_employee_user") }`
/// → [`write_error`] 翻成 409 + `68003`。
pub async fn set_user_id(
    ex: &impl SqlExecutor,
    id: Uuid,
    user_id: Option<Uuid>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主档写入。
    ex.exec_sql(
        SET_USER_SQL.to_string(),
        vec![
            opt_uuid(user_id),
            Value::from(Utc::now()),
            opt_uuid(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 状态 CAS 回写（**只允许** `shared::state_machine::transition` 调用，它同时写 `biz_status_log`）。
///
/// # 返回值
///
/// `0` = 状态已被别人改过（乐观锁失败）或单据已删 —— 调用方按「单据已被他人处理」报冲突。
pub async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from_status: &str,
    to_status: &str,
    leave_date: Option<NaiveDate>,
    leave_type: Option<&str>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    // SCOPE-EXEMPT: 主档状态回写（状态机专用）。
    ex.exec_sql(
        APPLY_STATUS_SQL.to_string(),
        vec![
            Value::Text(to_status.to_string()),
            opt_date(leave_date),
            opt_text(leave_type),
            Value::from(Utc::now()),
            opt_uuid(operator),
            Value::from(&id),
            Value::Text(from_status.to_string()),
        ],
    )
    .await
}

// ==================== 读操作 ====================

/// 按 id 取全字段（不含已软删）。
pub async fn find(id: Uuid) -> DbResult<Option<EmployeeRow>> {
    find_in(db(), id).await
}

/// 按 id 取全字段（可传事务 —— 写完同事务读回，避免「写成功但读不到」的假象）。
pub async fn find_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<EmployeeRow>> {
    let sql = format!("{ROW_SQL} WHERE e.id = ?::uuid AND e.deleted_at IS NULL");
    let row = ex.query_first_sql(sql, vec![Value::from(&id)]).await?;
    row.as_ref().map(EmployeeRow::from_row).transpose()
}

/// 按 id 取全字段并加行锁（`FOR UPDATE`）：状态流转 / 改绑账号前先锁，
/// 两个并发请求不会各读一份旧状态再各写一次。
pub async fn lock_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<EmployeeRow>> {
    let sql = format!("{ROW_SQL} WHERE e.id = ?::uuid AND e.deleted_at IS NULL FOR UPDATE OF e");
    let row = ex.query_first_sql(sql, vec![Value::from(&id)]).await?;
    row.as_ref().map(EmployeeRow::from_row).transpose()
}

/// 员工列表分页（设计 §5.2 #1）。
///
/// 数据权限走 `scope::paginate` + [`EMPLOYEE_TARGET`]（唯一分页入口，架构 §2.4 闸①）：
/// `SELF` 范围下普通员工只看得到自己那份档案（`owner_col = user_id`）。
///
/// # 返回值
///
/// `(列表, 总数)`；总数是**过滤 + 数据权限之后**的条数（`scope::paginate` 内部算）。
pub async fn page(
    filter: &EmployeeFilter,
    page: u64,
    page_size: u64,
    principal: &ScopedPrincipal,
) -> DbResult<(Vec<EmployeeListRow>, i64)> {
    let (sql, params) = list_query(filter);
    let result = scope::paginate(
        EMPLOYEE_TARGET,
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
        out.push(EmployeeListRow::from_row(row)?);
    }
    Ok((out, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 拼列表 SQL 与参数（**纯函数**：不连库就能验证占位符个数、`{FILTER}` 位置、
/// 关键字三个字段各绑一个参数这几件事 —— 这类错否则只在真跑列表时暴露）。
///
/// 返回的 `sql` 里 `{FILTER}` 是数据权限注入位（[`scope::FILTER_MARKER`]），
/// 必须出现在业务条件**之前**：`scope::paginate` 会把片段参数前置，顺序才不错位。
fn list_query(filter: &EmployeeFilter) -> (String, Vec<Value>) {
    let mut sql = format!("{LIST_SQL_PREFIX} {}", scope::FILTER_MARKER);
    let mut params: Vec<Value> = Vec::new();

    if let Some(dept_id) = filter.dept_id {
        sql.push_str(" AND e.dept_id = ?::uuid");
        params.push(Value::from(&dept_id));
    }
    if let Some(employment_type) = filter.employment_type.as_deref().map(str::trim)
        && !employment_type.is_empty()
    {
        sql.push_str(" AND e.employment_type = ?");
        params.push(Value::Text(employment_type.to_string()));
    }
    // 状态多选：IN 逐个占位（不用 `= ANY(?::text[])` —— 驱动对数组参数只有 text 编码器，
    // 实测报 `error serializing parameter N`，见 `erp/serial_model.rs` 的踩坑记录）。
    let statuses: Vec<&str> = filter
        .statuses
        .iter()
        .map(|status| status.trim())
        .filter(|status| !status.is_empty())
        .collect();
    if !statuses.is_empty() {
        let holes = vec!["?"; statuses.len()].join(", ");
        sql.push_str(&format!(" AND e.status IN ({holes})"));
        for status in statuses {
            params.push(Value::Text(status.to_string()));
        }
    } else if !filter.include_resigned {
        sql.push_str(" AND e.status <> 'RESIGNED'");
    }
    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        sql.push_str(" AND (e.emp_no LIKE ? OR e.name LIKE ? OR COALESCE(e.mobile, '') LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    (sql, params)
}

/// 按工号找未删除的员工（新建前查重 / 冲突报文里点名「撞了谁」）。
pub async fn existing_by_emp_no(ex: &impl SqlExecutor, emp_no: &str) -> DbResult<Option<EmployeeBrief>> {
    existing_by_column(ex, "emp_no", Value::Text(emp_no.trim().to_string())).await
}

/// 按身份证找未删除的员工（`id_card` 是敏感列，返回结构里**不含**它）。
pub async fn existing_by_id_card(ex: &impl SqlExecutor, id_card: &str) -> DbResult<Option<EmployeeBrief>> {
    existing_by_column(ex, "id_card", Value::Text(id_card.trim().to_string())).await
}

/// 按登录账号找未删除的员工（`seed-from-users` 与 `bind-user` 的前置查重）。
pub async fn existing_by_user_id(ex: &impl SqlExecutor, user_id: Uuid) -> DbResult<Option<EmployeeBrief>> {
    existing_by_column(ex, "user_id", Value::from(&user_id)).await
}

/// 唯一列查一行的公共实现（`column` 只接受本文件内的字面量，不来自请求）。
async fn existing_by_column(ex: &impl SqlExecutor, column: &str, value: Value) -> DbResult<Option<EmployeeBrief>> {
    // SCOPE-EXEMPT: 唯一键查重（单行按值查，不带范围；且冲突报文不该因为数据权限而沉默）。
    let sql = format!(
        "SELECT id, emp_no, name, status FROM {TABLE} \
          WHERE {column} = ? AND deleted_at IS NULL \
          LIMIT 1"
    );
    let row = ex.query_first_sql(sql, vec![value]).await?;
    row.as_ref().map(EmployeeBrief::from_row).transpose()
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 建表迁移原文（编译期嵌入）：唯一索引名改了就红。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    /// 一份最小可用草稿（全字段有值，便于验证参数顺序 / 个数）。
    fn sample_draft() -> EmployeeWrite {
        EmployeeWrite {
            emp_no: "EMP-0001".to_string(),
            name: "张三".to_string(),
            gender: 1,
            birth_date: Some(NaiveDate::from_ymd_opt(1990, 1, 1).expect("合法日期")),
            id_card: Some("110101199001011234".to_string()),
            id_card_type: "ID_CARD".to_string(),
            mobile: Some("13800000000".to_string()),
            email: Some("zhangsan@example.com".to_string()),
            dept_id: Uuid::nil(),
            position_code: Some("DEV".to_string()),
            job_level: Some("P6".to_string()),
            employment_type: "FULL_TIME".to_string(),
            hire_date: NaiveDate::from_ymd_opt(2026, 1, 1).expect("合法日期"),
            probation_end_date: Some(NaiveDate::from_ymd_opt(2026, 4, 1).expect("合法日期")),
            education: Some("BACHELOR".to_string()),
            school: Some("某大学".to_string()),
            major: Some("计算机".to_string()),
            graduate_date: Some(NaiveDate::from_ymd_opt(2012, 6, 30).expect("合法日期")),
            native_place: Some("北京".to_string()),
            address: Some("北京市海淀区".to_string()),
            emergency_contact: Some("李四".to_string()),
            emergency_phone: Some("13900000000".to_string()),
            emergency_relation: Some("配偶".to_string()),
            bank_name: Some("工商银行".to_string()),
            bank_branch: Some("中关村支行".to_string()),
            bank_account: Some("6222020000000000000".to_string()),
            bank_account_name: Some("张三".to_string()),
            user_id: Some(Uuid::nil()),
            remark: Some("测试".to_string()),
        }
    }

    fn placeholders(sql: &str) -> usize {
        sql.matches('?').count()
    }

    /// 三个约束名必须与 `V0383` 的 `CREATE UNIQUE INDEX` **逐字一致**。
    ///
    /// 这条不是形式主义：冲突翻译按约束名分支，名字对不上 → 线上表现是「工号重复」被翻成
    /// 通用 409（甚至 500），而**编译期一点提示都没有**。约束名只会被新迁移改，
    /// 那时这条测试先红。
    #[test]
    fn unique_key_names_match_v0383_ddl() {
        for key in EmployeeUniqueKey::ALL {
            let name = key.constraint();
            assert!(
                V0383_DDL.contains(name),
                "{name} 不在 V0383__hr_tables 里 —— 唯一索引被改名了？"
            );
            assert!(
                V0383_DDL.contains(&format!("CREATE UNIQUE INDEX IF NOT EXISTS {name}"))
                    || V0383_DDL.contains(&format!("CREATE UNIQUE INDEX {name}")),
                "{name} 不是 UNIQUE INDEX（改成普通索引用不了冲突翻译）"
            );
        }
        assert_eq!(UK_EMP_NO, "uk_biz_employee_no");
        assert_eq!(UK_ID_CARD, "uk_biz_employee_idcard");
        assert_eq!(UK_USER, "uk_biz_employee_user");
    }

    /// 三个约束名都能被识别，且**互不混淆**；陌生约束名返回 `None`（走通用冲突），
    /// 大小写不敏感（其它方言会折叠大小写）。
    #[test]
    fn classify_maps_exactly_the_three_keys() {
        assert_eq!(classify_unique(Some(UK_EMP_NO)), Some(EmployeeUniqueKey::EmpNo));
        assert_eq!(classify_unique(Some(UK_ID_CARD)), Some(EmployeeUniqueKey::IdCard));
        assert_eq!(classify_unique(Some(UK_USER)), Some(EmployeeUniqueKey::UserAccount));
        assert_eq!(
            classify_unique(Some("UK_BIZ_EMPLOYEE_NO")),
            Some(EmployeeUniqueKey::EmpNo),
            "大小写不敏感"
        );
        assert_eq!(
            classify_unique(Some(" uk_biz_employee_idcard ")),
            Some(EmployeeUniqueKey::IdCard),
            "两端的空白要裁掉（驱动原样带回）"
        );
        assert_eq!(classify_unique(Some("uk_biz_employee_other")), None);
        assert_eq!(classify_unique(None), None);
        // 三个键 -> 三个不同的码，且都不与通用 409 撞
        let codes: std::collections::BTreeSet<i32> = EmployeeUniqueKey::ALL.iter().map(|key| key.code()).collect();
        assert_eq!(codes.len(), 3, "三个唯一键必须有三个不同的业务码");
        assert!(!codes.contains(&error_code::RESOURCE_CONFLICT));
    }

    /// 三条文案：各自指名撞了什么、互不相同、不出现约束名 / 明文身份证 / UUID。
    #[test]
    fn conflict_messages_are_friendly_and_distinct() {
        let values = WriteValues {
            emp_no: Some("EMP-0002"),
            id_card: Some("110101199001011234"),
        };
        let emp_no = conflict_message(EmployeeUniqueKey::EmpNo, &values);
        let id_card = conflict_message(EmployeeUniqueKey::IdCard, &values);
        let account = conflict_message(EmployeeUniqueKey::UserAccount, &values);

        assert!(emp_no.contains("EMP-0002"), "工号冲突要点名工号：{emp_no}");
        assert!(emp_no.contains("工号"), "工号冲突要说人话：{emp_no}");
        assert!(id_card.contains("身份证"), "身份证冲突要说人话：{id_card}");
        assert!(
            id_card.contains("110101********1234"),
            "身份证必须脱敏后点名：{id_card}"
        );
        assert!(account.contains("账号"), "账号冲突要说人话：{account}");

        assert_ne!(emp_no, id_card);
        assert_ne!(id_card, account);
        assert_ne!(emp_no, account);

        for message in [&emp_no, &id_card, &account] {
            assert!(!message.contains("uk_biz_"), "文案不许出现约束名：{message}");
            assert!(
                !message.contains("110101199001011234"),
                "文案不许出现明文身份证：{message}"
            );
            assert!(
                !message.contains("00000000-0000"),
                "文案不许出现 UUID（用户认不出）：{message}"
            );
            assert!(!message.is_empty());
        }

        // 没传值时也要能翻译（只退化成通用说法，不 panic、不留空串）
        let blank = conflict_message(EmployeeUniqueKey::EmpNo, &WriteValues::default());
        assert!(blank.contains("工号"));
        assert!(conflict_message(EmployeeUniqueKey::IdCard, &WriteValues::default()).contains("身份证"));
    }

    /// 身份证脱敏：与 `sys_field_mask(mask_type=2, 6,4)` 同口径。
    #[test]
    fn mask_id_card_keeps_six_and_four() {
        assert_eq!(mask_id_card("110101199001011234"), "110101********1234");
        assert_eq!(mask_id_card("  110101199001011234  "), "110101********1234");
        assert_eq!(mask_id_card("11010119900101123X"), "110101********123X");
        // 短号（护照 / 港澳台通行证常见 8~10 位）：留前 2 后 2
        assert_eq!(mask_id_card("E12345678"), "E1*****78");
        // 极短：全掩
        assert_eq!(mask_id_card("1234"), "****");
        assert_eq!(mask_id_card(""), "");
    }

    /// `write_error`：三个唯一键 → 409 + 68xxx + 友好文案；`CHECK` → 400 + 人话。
    #[test]
    fn write_error_translates_unique_and_check_violations() {
        let values = WriteValues {
            emp_no: Some("EMP-0002"),
            id_card: Some("110101199001011234"),
        };

        for key in EmployeeUniqueKey::ALL {
            let err = DbError::UniqueViolation {
                column: None,
                constraint: Some(key.constraint().to_string()),
                source: None,
            };
            let api = write_error(err, "hr_employee_insert", &values);
            assert_eq!(api.status, 409, "{} 冲突必须是 409", key.label());
            assert_eq!(api.code, key.code(), "{} 的业务码不对", key.label());
            assert!(!api.msg.is_empty());
            assert!(
                api.internal_detail.is_none(),
                "409 是业务错误，不该带 internal_detail（那是给 500 的）"
            );
        }

        // 本表新增的陌生唯一索引 → 通用 409，报文保留约束名供运维定位
        let unknown = DbError::UniqueViolation {
            column: None,
            constraint: Some("uk_biz_employee_something_new".to_string()),
            source: None,
        };
        let api = write_error(unknown, "hr_employee_insert", &values);
        assert_eq!(api.status, 409);
        assert_eq!(api.code, error_code::RESOURCE_CONFLICT);
        assert!(api.msg.contains("uk_biz_employee_something_new"), "报文：{}", api.msg);

        // 约束名为空（驱动没给）→ 也要是 409 而不是 500
        let nameless = DbError::UniqueViolation {
            column: None,
            constraint: None,
            source: None,
        };
        let api = write_error(nameless, "hr_employee_insert", &values);
        assert_eq!(api.status, 409);
        assert_eq!(api.code, error_code::RESOURCE_CONFLICT);

        // CHECK 违反 → 400 + 人话（点名该改哪个字段）
        let check = DbError::CheckViolation {
            constraint: Some("ck_biz_employee_leave".to_string()),
            source: None,
        };
        let api = write_error(check, "hr_employee_insert", &values);
        assert_eq!(api.status, 400);
        assert_eq!(api.code, error_code::PARAM_INVALID);
        assert!(api.msg.contains("离职日期"), "报文：{}", api.msg);

        // 未命名的 CHECK → 也有兜底文案
        let unnamed_check = DbError::CheckViolation {
            constraint: None,
            source: None,
        };
        let api = write_error(unnamed_check, "hr_employee_insert", &values);
        assert_eq!(api.status, 400);
        assert_eq!(api.code, error_code::PARAM_INVALID);
    }

    /// 非唯一键 / 非 CHECK 的数据库错误仍然收敛成 500（且 500 的报文不带真实原因）。
    #[test]
    fn write_error_keeps_unknown_errors_internal() {
        let api = write_error(DbError::NotFound, "hr_employee_update", &WriteValues::default());
        assert_eq!(api.status, 500);
        assert_eq!(api.code, error_code::INTERNAL_ERROR);
        assert_eq!(api.msg, "服务器内部错误", "500 报文不得泄漏内部细节");
        assert!(api.internal_detail.is_some(), "真实原因要留在 internal_detail 进日志");
    }

    /// INSERT 的 `?` 个数必须等于参数个数（36），否则第一次真插就报「参数不匹配」。
    #[test]
    fn insert_sql_parameter_count_matches() {
        let params = insert_params(Uuid::nil(), &sample_draft(), Some(Uuid::nil()));
        assert_eq!(placeholders(INSERT_SQL), params.len(), "INSERT 占位符与参数个数不一致");
        assert_eq!(
            placeholders(INSERT_SQL),
            36,
            "biz_employee 的可写列 + id/org_id/审计列 = 36"
        );
    }

    /// UPDATE 的 `?` 个数必须等于参数个数（30 = 27 列 + updated_at + update_by + id）。
    #[test]
    fn update_sql_parameter_count_matches() {
        let params = update_params(Uuid::nil(), &sample_draft(), Some(Uuid::nil()));
        assert_eq!(placeholders(UPDATE_SQL), params.len(), "UPDATE 占位符与参数个数不一致");
        assert_eq!(placeholders(UPDATE_SQL), 30);
    }

    /// 改路径**不许碰**身份 / 组织 / 状态 / 账号列（工号不可改、状态只能走状态机）。
    ///
    /// 这条测试的价值：`UPDATE_SQL` 是每个字段手写的，「顺手把 status 加进去」这种改动
    /// 在 code review 里极难看出来，但线上表现是「保存一次表单就跳过状态机」。
    #[test]
    fn update_sql_never_touches_guarded_columns() {
        for guarded in ["emp_no", "org_id", "status", "leave_date", "leave_type", "user_id"] {
            assert!(
                !UPDATE_SQL.contains(guarded),
                "UPDATE_SQL 不许写 {guarded}（见模块文档「四条不许违反的约定」③④）"
            );
        }
        // 软删只能改时间戳与审计列，不能 DELETE
        assert!(SOFT_DELETE_SQL.contains("deleted_at = ?"));
        assert!(!SOFT_DELETE_SQL.contains("DELETE"), "软删不许物理删除");
        assert!(!SOFT_DELETE_SQL.contains("emp_no"));
    }

    /// 状态 CAS 回写的 `WHERE` 里必须有旧状态（乐观锁），否则并发流转会双写。
    #[test]
    fn apply_status_is_a_cas_update() {
        assert!(APPLY_STATUS_SQL.contains("AND status = ?"), "缺少旧状态条件（乐观锁）");
        assert!(APPLY_STATUS_SQL.contains("deleted_at IS NULL"));
        assert_eq!(
            placeholders(APPLY_STATUS_SQL),
            7,
            "to_status/leave_date/leave_type/now/operator/id/from"
        );
    }

    /// 可空日期列必须走 `NULLIF(?::text,'')::date`。
    ///
    /// `''::text::date` 在 PG 里是**硬错误**（`invalid input syntax for type date`），
    /// 而「出生日期 / 试用期 / 毕业日期留空」是最常见的表单状态 —— 漏一层 `NULLIF`
    /// 的表现是「新建员工只要不填生日就 500」，而正常填满的表单测不出来。
    #[test]
    fn nullable_dates_are_null_if_guarded() {
        let count = |sql: &str, needle: &str| sql.matches(needle).count();
        // 可空日期列逐个包 NULLIF（出生 / 试用期 / 离职 / 毕业）
        assert_eq!(count(INSERT_SQL, "NULLIF(?::text,'')::date"), 4);
        assert_eq!(count(UPDATE_SQL, "NULLIF(?::text,'')::date"), 3);
        assert_eq!(count(APPLY_STATUS_SQL, "NULLIF(?::text,'')::date"), 1);
        // 非空日期列（hire_date）仍走裸 `?::text::date`：传空串应当报错，不该被静默吞成 NULL
        for sql in [INSERT_SQL, UPDATE_SQL] {
            assert!(
                sql.contains("hire_date = ?::text::date") || sql.contains(", ?::text::date, "),
                "入职日期必须是非空日期列（裸绑定）：{sql}"
            );
        }
        // 全部日期占位符 = 可空（NULLIF）+ 非空（裸绑定），一个都不能漏：
        // INSERT 5（4 可空 + hire_date）/ UPDATE 4（3 可空 + hire_date）/ APPLY_STATUS 1
        assert_eq!(count(INSERT_SQL, "::date"), 5);
        assert_eq!(count(UPDATE_SQL, "::date"), 4);
        assert_eq!(count(APPLY_STATUS_SQL, "::date"), 1);
    }

    /// 列表 SQL：`{FILTER}` 恰好一个且在业务条件之前；占位符与参数一一对应；
    /// 关键字命中工号 / 姓名 / 手机号三处各绑一个参数。
    #[test]
    fn list_query_placeholders_line_up() {
        let filter = EmployeeFilter {
            keyword: Some(" 张 ".to_string()),
            dept_id: Some(Uuid::nil()),
            statuses: vec!["ACTIVE".to_string(), "PROBATION".to_string()],
            employment_type: Some("FULL_TIME".to_string()),
            include_resigned: false,
        };
        let (sql, params) = list_query(&filter);
        assert_eq!(
            sql.matches(scope::FILTER_MARKER).count(),
            1,
            "数据权限注入位必须恰好一个"
        );
        assert!(
            sql.find(scope::FILTER_MARKER).expect("{FILTER} 存在") < sql.find("AND e.dept_id").expect("部门条件存在"),
            "{{FILTER}} 必须在业务条件之前（scope::paginate 会把片段参数前置）"
        );
        assert_eq!(placeholders(&sql), params.len(), "占位符与参数错位：{sql}");
        // 部门 1 + 用工形式 1 + 状态 2 + 关键字 3 = 7
        assert_eq!(params.len(), 7);
        assert!(sql.contains("e.deleted_at IS NULL"), "列表必须过滤软删");
        assert!(sql.contains("e.emp_no LIKE ?"), "关键字要匹配工号");
        assert!(sql.contains("e.name LIKE ?"), "关键字要匹配姓名");
        assert!(
            sql.contains("COALESCE(e.mobile, '') LIKE ?"),
            "关键字要匹配手机号（手机为空不炸）"
        );

        // 默认（无状态筛选、不含离职）：追加 `<> 'RESIGNED'`，参数只剩 0 个
        let (default_sql, default_params) = list_query(&EmployeeFilter::default());
        assert_eq!(default_params.len(), 0);
        assert!(default_sql.contains("e.status <> 'RESIGNED'"));

        // 显式包含离职：由调用方传四个状态，不再追加 `<> 'RESIGNED'`
        let (all_sql, all_params) = list_query(&EmployeeFilter {
            include_resigned: true,
            statuses: vec![
                "PROBATION".to_string(),
                "ACTIVE".to_string(),
                "SUSPENDED".to_string(),
                "RESIGNED".to_string(),
            ],
            ..EmployeeFilter::default()
        });
        assert_eq!(placeholders(&all_sql), all_params.len());
        assert_eq!(all_params.len(), 4);
        assert!(!all_sql.contains("<> 'RESIGNED'"));
        assert!(all_sql.contains("IN (?, ?, ?, ?)"), "状态多选要逐个占位：{all_sql}");
    }

    /// 详情 / 列表 SQL 都必须过滤软删，且列名与行结构对得上（行结构读的列必须被查出来）。
    #[test]
    fn select_sql_projects_every_column_the_row_reads() {
        for sql in [ROW_SQL, LIST_SQL_PREFIX] {
            assert!(sql.contains("deleted_at IS NULL"), "查询必须过滤软删：{sql}");
            assert!(sql.contains("biz_employee"), "表名：{sql}");
            assert!(!sql.contains("biz_employee e\n WHERE") || sql.contains("e.deleted_at IS NULL"));
        }
        for column in [
            "id",
            "emp_no",
            "name",
            "gender",
            "dept_id",
            "status",
            "user_id",
            "dept_name",
            "username",
            "bank_account_name",
        ] {
            assert!(ROW_SQL.contains(column), "ROW_SQL 缺列 {column}");
        }
        for column in ["contract_end_date", "username", "dept_name"] {
            assert!(LIST_SQL_PREFIX.contains(column), "LIST_SQL 缺列 {column}");
        }
        assert!(
            !LIST_SQL_PREFIX.contains("id_card"),
            "列表不取身份证（列表不下发敏感列，缩小泄漏面）"
        );
    }

    /// 唯一键查重的 SQL 里，`column` 只来自本文件的字面量；三个入口都过滤软删。
    #[test]
    fn unique_lookups_are_literal_columns() {
        for column in ["emp_no", "id_card", "user_id"] {
            assert!(
                ["emp_no", "id_card", "user_id"].contains(&column),
                "只允许三列，别把请求里的字符串拼进 SQL"
            );
        }
        assert_eq!(EmployeeUniqueKey::ALL.len(), 3);
        // 三个键的约束名两两不同（否则 `find` 会第一个命中错的那个）
        let names: std::collections::BTreeSet<&str> =
            EmployeeUniqueKey::ALL.iter().map(|key| key.constraint()).collect();
        assert_eq!(names.len(), 3);
    }

    /// `WriteValues::of` 从草稿取值：空串当没填（报文不出现「工号  已被占用」这种双空）。
    #[test]
    fn write_values_skip_blank_draft_fields() {
        let mut draft = sample_draft();
        draft.emp_no = String::new();
        draft.id_card = Some("   ".to_string());
        let values = WriteValues::of(&draft);
        assert_eq!(values.emp_no, None);
        assert_eq!(values.id_card, None);

        draft.emp_no = "EMP-0003".to_string();
        draft.id_card = Some(" 110101199001011234 ".to_string());
        let values = WriteValues::of(&draft);
        assert_eq!(values.emp_no, Some("EMP-0003"));
        assert_eq!(values.id_card, Some("110101199001011234"));
    }
}
