//! 员工档案接口层（HR-12）：设计 §5.2 的 #1~#6 六个接口 + #8 绑定账号。
//!
//! 数据层在 [`crate::business::hr::employee_model`]（HR-10），出参契约在
//! [`crate::business::hr::employee_dto`]（HR-11）。本文件只做三件事：
//! **入参校验 → 数据权限闸 → 调用模型层/状态机 → 组装出参**。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 落地卡 |
//! |:--:|---|---|---|---|
//! | 1 | GET | `/business/hr/employee/page` | `business:hr:employee:list` | HR-12 |
//! | 2 | GET | `/business/hr/employee/{id}` | `business:hr:employee:detail` | HR-12 |
//! | 3 | POST | `/business/hr/employee` | `business:hr:employee:create` | HR-12 |
//! | 4 | PUT | `/business/hr/employee/{id}` | `business:hr:employee:update` | HR-12 |
//! | 5 | DELETE | `/business/hr/employee/{id}` | `business:hr:employee:delete` | HR-12 |
//! | 6 | POST | `/business/hr/employee/{id}/status` | `business:hr:employee:update` | HR-12 |
//! | 8 | POST | `/business/hr/employee/{id}/bind-user` | `business:hr:employee:update` | HR-12 |
//!
//! `ROUTE_CONTRACT`（[`crate::business::hr::handler`]）把 #8 的落地卡也登记成 HR-12，
//! 所以本文件把它一起落地 —— 否则它会变成「契约里有、卡清单里没有」的孤儿接口
//! （`employee_model::set_user_id` 是 HR-10 已经备好的写入点，接口侧只差这一层）。
//! 其余 5 条（#7 明文、#9 取号、#10 seed、#11 导入、#12 导出）分别属 HR-13/14/17/TBD。
//!
//! # 数据权限（本卡验收项：**只能看本部门**）
//!
//! `biz_employee` 已登记进 `shared::scope::SCOPED_TABLES`（[`crate::business::shared::scope::ScopeTarget::BIZ_EMPLOYEE`]，
//! `dept_col = dept_id`、`owner_col = user_id`，设计 D-12）：
//!
//! - **列表**走 `employee_model::page` → `scope::paginate`（唯一分页入口，闸①）；
//! - **单条**（详情 / 改 / 删 / 状态 / 绑账号）额外过一道 [`ensure_visible`] 探针：
//!   不可见一律 **404**（不是 403）—— 403 会告诉越权者「这个 id 真实存在」，
//!   而按 id 猜档案正是最先被尝试的越权路径。数据范围之外的单据在界面上本来就不存在。
//!
//! 为什么单条也要判：分页只挡住「列表」，`GET /employee/{uuid}` 一旦漏判，
//! 换个 id 就能逐个拖走别人的身份证打码值、工资卡打码值、紧急联系人。
//! 设计 §4.7.3 / D-12 把员工与薪资明细都登记进范围清单，就是这个意思。
//!
//! # 状态变更为什么不在本文件里判边
//!
//! 合法边、`biz_status_log`、CAS 回写全在
//! [`crate::business::shared::state_machine::transition`]（设计 §4.2.1：
//! 「handler 里不许出现 `match (from, to)`」）。本文件只做**字段级守卫**
//! （[`check_status_change`] 纯函数）：生效合同、原因字数、离职必须走审批单。
//!
//! # 离职为什么在这里被拒
//!
//! `* → RESIGNED` 的**唯一合法入口**是离职单（`hr_employee_resign`）审批通过后的回调
//! （设计 §4.1.3 ③ / D-13）：它要在同一个事务里写 `status=RESIGNED` + `leave_date` +
//! `leave_type` + 停用登录账号。状态接口如果把这条边放开，界面上就能「一键离职」而
//! 账号还活着、也没有任何审批痕迹 —— 所以本文件对 `target = RESIGNED` 直接 409 并
//! 点名该走哪条路（离职单的接口属 HR-30 批 3，落地前这里是唯一提示）。
//!
//! # 与其它卡的边界（本卡**不做**）
//!
//! - 工号取号（`emp_no` 留空自动取号）属 **HR-14**（已落地）：
//!   [`crate::business::hr::employee_no::generate_emp_no`] 在**新建的同一个事务**里取号，
//!   失败一起回滚 ⇒ 不留空号；预览入口是 `GET /business/hr/employee/next-no`（不耗号）；
//! - 敏感字段**明文**接口属 **HR-13**：本文件的出参只有 `*_masked`（明文一个字节都不出）；
//! - 合同台账 / 薪资标准的**写**接口属 HR-15 / HR-16：本文件只读投影它们（详情页要显示
//!   `contracts[]` / `salary_items[]`，见 [`load_contracts`] / [`load_salary_items`]）。

use axum::Json;
use axum::extract::{Extension, Path, Query};
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::hr::contract_model;
use crate::business::hr::employee_dto::{
    self, ContractInput, EmployeeDetailRes, EmployeeListRes, EmployeeNames, SalaryInput,
};
use crate::business::hr::employee_model::{self, EmployeeFilter, EmployeeRow, EmployeeWrite};
use crate::business::hr::employee_no;
use crate::business::hr::salary_model;
use crate::business::masking::MaskSet;
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::state_machine::{self, Operator};
use crate::business::shared::{principal, scope};
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0387 的 sys_menu.auth_code 逐字一致） ====================

/// 列表。
pub const PERM_LIST: &str = "business:hr:employee:list";
/// 详情。
pub const PERM_DETAIL: &str = "business:hr:employee:detail";
/// 新建。
pub const PERM_CREATE: &str = "business:hr:employee:create";
/// 修改 / 状态 / 绑账号（设计 §5.2 的 #4/#6/#8 共用这一个码）。
pub const PERM_UPDATE: &str = "business:hr:employee:update";
/// 删除。
pub const PERM_DELETE: &str = "business:hr:employee:delete";

/// 本卡用到的全部权限码（顺序 = 迁移里的种子顺序；单测与 `V0387` 对拍）。
pub const EMPLOYEE_PERMISSIONS: [&str; 5] = [PERM_LIST, PERM_DETAIL, PERM_CREATE, PERM_UPDATE, PERM_DELETE];

// ==================== 长度上限（与 V0383 的 VARCHAR(n) 逐列对齐） ====================

/// `emp_no VARCHAR(32)`。
const MAX_EMP_NO: usize = 32;
/// `name VARCHAR(50)`。
const MAX_NAME: usize = 50;
/// `id_card VARCHAR(18)`。
const MAX_ID_CARD: usize = 18;
/// `mobile VARCHAR(20)`。
const MAX_MOBILE: usize = 20;
/// `email VARCHAR(100)`。
const MAX_EMAIL: usize = 100;
/// `position_code VARCHAR(32)`。
const MAX_POSITION: usize = 32;
/// `job_level VARCHAR(20)`。
const MAX_JOB_LEVEL: usize = 20;
/// `school VARCHAR(100)`。
const MAX_SCHOOL: usize = 100;
/// `major VARCHAR(100)`。
const MAX_MAJOR: usize = 100;
/// `native_place VARCHAR(100)`。
const MAX_PLACE: usize = 100;
/// `address VARCHAR(200)`。
const MAX_ADDRESS: usize = 200;
/// `emergency_contact VARCHAR(50)` / `bank_account_name VARCHAR(50)`。
const MAX_PERSON_NAME: usize = 50;
/// `emergency_phone VARCHAR(20)`。
const MAX_PHONE: usize = 20;
/// `emergency_relation VARCHAR(20)`。
const MAX_RELATION: usize = 20;
/// `bank_name VARCHAR(100)`。
const MAX_BANK_NAME: usize = 100;
/// `bank_branch VARCHAR(200)`。
const MAX_BRANCH: usize = 200;
/// `bank_account VARCHAR(40)`。
const MAX_BANK_ACCOUNT: usize = 40;

// ==================== 枚举与分页 ====================

/// 证件类型域（`ck_biz_employee_idtype`）。
pub const ID_CARD_TYPES: [&str; 4] = ["ID_CARD", "PASSPORT", "HK_MO_TW", "FOREIGN"];
/// 用工形式域（`ck_biz_employee_emptype`）。
pub const EMPLOYMENT_TYPES: [&str; 6] = ["FULL_TIME", "PROBATION", "INTERN", "OUTSOURCE", "PART_TIME", "DISPATCH"];
/// 原因类动作的最小字数（设计 §4.2.1：停薪留职「原因 ≥ 5 字进 `biz_status_log.reason`」）。
pub const MIN_REASON_CHARS: usize = 5;
/// 默认每页条数。
pub const DEFAULT_PAGE_SIZE: u64 = 20;
/// 每页上限（防止 `page_size=100000` 把列表变成导出）。
pub const MAX_PAGE_SIZE: u64 = 200;

// ==================== 错误原语 ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `409 + 10003`（状态没变化 / 与状态机语义冲突，不是入参格式错）。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 「员工档案不存在或已删除」（404；**数据范围之外也走这一句**，不泄露存在性）。
fn not_visible() -> ApiError {
    employee_model::not_found_error()
}

// ==================== 入参 / 出参结构 ====================

/// 列表筛选（设计 §5.2 #1）。
///
/// `status[]` 的落地形态：查询串里用**逗号分隔**（`status=ACTIVE,SUSPENDED`）。
/// 不用重复键（`status=ACTIVE&status=SUSPENDED`）：`Query<T>` 反序列化对重复键只保留
/// 最后一个，前端会看到「勾了两个状态只出一个」的鬼现象，而且不报错。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmployeePageQuery {
    /// 关键字（工号 / 姓名 / 手机号，`LIKE %kw%`）。
    #[serde(default)]
    pub keyword: Option<String>,
    /// 人事归属部门（精确匹配本部门）。
    #[serde(default)]
    pub dept_id: Option<String>,
    /// 状态多选，逗号分隔（空 = 默认在职三态；`include_resigned=true` 才含离职）。
    #[serde(default)]
    pub status: Option<String>,
    /// 用工形式（`FULL_TIME` …）。
    #[serde(default)]
    pub employment_type: Option<String>,
    /// 是否包含离职员工（默认 `false`）。
    #[serde(default)]
    pub include_resigned: Option<bool>,
    /// 页码（从 1 开始）。
    #[serde(default)]
    pub page: Option<u64>,
    /// 每页条数（默认 20，上限 200）。
    #[serde(default)]
    pub page_size: Option<u64>,
}

/// 新建员工入参（设计 §5.2 #3 / §3.1 ① 的全字段）。
///
/// 每个字段都是 `Option`：把「没填」与「填错」区分成两条 400 人话，而不是让 serde
/// 抛一句 `missing field \`name\``（前端只能直出英文报错）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmployeeCreateReq {
    /// 工号（**留空 ⇒ 自动取号**：`EMP-0001` 形态，取号与插入同事务，见 HR-14；
    /// 有值则原样保留，允许企业自有工号体系如 `E2003`）。
    #[serde(default)]
    pub emp_no: Option<String>,
    /// 姓名（必填）。
    #[serde(default)]
    pub name: Option<String>,
    /// 性别（0 未知 / 1 男 / 2 女，缺省 0）。
    #[serde(default)]
    pub gender: Option<i16>,
    /// 出生日期（`yyyy-MM-dd`）。
    #[serde(default)]
    pub birth_date: Option<String>,
    /// 身份证号（明文入参，落库明文，出参只给打码值）。
    #[serde(default)]
    pub id_card: Option<String>,
    /// 证件类型（缺省 `ID_CARD`）。
    #[serde(default)]
    pub id_card_type: Option<String>,
    /// 手机号。
    #[serde(default)]
    pub mobile: Option<String>,
    /// 邮箱。
    #[serde(default)]
    pub email: Option<String>,
    /// 人事归属部门（必填，必须是未删除的 `sys_dept` 行）。
    #[serde(default)]
    pub dept_id: Option<String>,
    /// 岗位码值（字典 `hr_position`）。
    #[serde(default)]
    pub position_code: Option<String>,
    /// 职级码值（字典 `hr_job_level`）。
    #[serde(default)]
    pub job_level: Option<String>,
    /// 用工形式（缺省 `FULL_TIME`）。
    #[serde(default)]
    pub employment_type: Option<String>,
    /// 入职日期（必填）。
    #[serde(default)]
    pub hire_date: Option<String>,
    /// 试用期截止（可空；有值则不得早于入职日期）。
    #[serde(default)]
    pub probation_end_date: Option<String>,
    /// 学历码值（字典 `hr_education`）。
    #[serde(default)]
    pub education: Option<String>,
    /// 毕业院校。
    #[serde(default)]
    pub school: Option<String>,
    /// 专业。
    #[serde(default)]
    pub major: Option<String>,
    /// 毕业日期。
    #[serde(default)]
    pub graduate_date: Option<String>,
    /// 籍贯。
    #[serde(default)]
    pub native_place: Option<String>,
    /// 现住址。
    #[serde(default)]
    pub address: Option<String>,
    /// 紧急联系人。
    #[serde(default)]
    pub emergency_contact: Option<String>,
    /// 紧急联系电话。
    #[serde(default)]
    pub emergency_phone: Option<String>,
    /// 与紧急联系人的关系。
    #[serde(default)]
    pub emergency_relation: Option<String>,
    /// 工资卡开户行。
    #[serde(default)]
    pub bank_name: Option<String>,
    /// 开户支行。
    #[serde(default)]
    pub bank_branch: Option<String>,
    /// 工资卡号（明文入参）。
    #[serde(default)]
    pub bank_account: Option<String>,
    /// 工资卡户名。
    #[serde(default)]
    pub bank_account_name: Option<String>,
    /// 关联登录账号（可空；必须是未删除的 `HUMAN` 账号）。
    #[serde(default)]
    pub user_id: Option<String>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 修改员工入参（设计 §5.2 #4：同新建但不含 `emp_no`）。
///
/// # 缺省语义（与 `employee_model::EmployeeWrite` 的「全字段覆盖」配套）
///
/// - 字段**不传 / 传 null** → 保持库里当前值（[`merged_write`] 从锁定行回填）；
/// - 字段**传空串** → 清空该列（`NULL`）；
/// - 有值 → 覆盖。
///
/// 这条约定是必须的：`UPDATE_SQL` 是整行覆盖（设计就是「整表单提交」），
/// 如果接口层把「没传」也当 `NULL` 传下去，前端少渲染一个字段就会把数据清掉。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmployeeUpdateReq {
    /// 姓名（不传 = 不改；不允许传空）。
    #[serde(default)]
    pub name: Option<String>,
    /// 性别。
    #[serde(default)]
    pub gender: Option<i16>,
    /// 出生日期（空串 = 清空）。
    #[serde(default)]
    pub birth_date: Option<String>,
    /// 身份证号（空串 = 清空）。
    #[serde(default)]
    pub id_card: Option<String>,
    /// 证件类型。
    #[serde(default)]
    pub id_card_type: Option<String>,
    /// 手机号（空串 = 清空）。
    #[serde(default)]
    pub mobile: Option<String>,
    /// 邮箱（空串 = 清空）。
    #[serde(default)]
    pub email: Option<String>,
    /// 人事归属部门（不传 = 不改）。
    #[serde(default)]
    pub dept_id: Option<String>,
    /// 岗位码值（空串 = 清空）。
    #[serde(default)]
    pub position_code: Option<String>,
    /// 职级码值（空串 = 清空）。
    #[serde(default)]
    pub job_level: Option<String>,
    /// 用工形式。
    #[serde(default)]
    pub employment_type: Option<String>,
    /// 入职日期（不传 = 不改）。
    #[serde(default)]
    pub hire_date: Option<String>,
    /// 试用期截止（空串 = 清空）。
    #[serde(default)]
    pub probation_end_date: Option<String>,
    /// 学历码值（空串 = 清空）。
    #[serde(default)]
    pub education: Option<String>,
    /// 毕业院校（空串 = 清空）。
    #[serde(default)]
    pub school: Option<String>,
    /// 专业（空串 = 清空）。
    #[serde(default)]
    pub major: Option<String>,
    /// 毕业日期（空串 = 清空）。
    #[serde(default)]
    pub graduate_date: Option<String>,
    /// 籍贯（空串 = 清空）。
    #[serde(default)]
    pub native_place: Option<String>,
    /// 现住址（空串 = 清空）。
    #[serde(default)]
    pub address: Option<String>,
    /// 紧急联系人（空串 = 清空）。
    #[serde(default)]
    pub emergency_contact: Option<String>,
    /// 紧急联系电话（空串 = 清空）。
    #[serde(default)]
    pub emergency_phone: Option<String>,
    /// 与紧急联系人的关系（空串 = 清空）。
    #[serde(default)]
    pub emergency_relation: Option<String>,
    /// 工资卡开户行（空串 = 清空）。
    #[serde(default)]
    pub bank_name: Option<String>,
    /// 开户支行（空串 = 清空）。
    #[serde(default)]
    pub bank_branch: Option<String>,
    /// 工资卡号（空串 = 清空）。
    #[serde(default)]
    pub bank_account: Option<String>,
    /// 工资卡户名（空串 = 清空）。
    #[serde(default)]
    pub bank_account_name: Option<String>,
    /// 备注（空串 = 清空）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 状态变更入参（设计 §5.2 #6）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmployeeStatusReq {
    /// 目标状态（`ACTIVE` / `SUSPENDED` / `PROBATION`；`RESIGNED` 走离职单）。
    #[serde(default)]
    pub to_status: Option<String>,
    /// 原因（停薪留职必填且 ≥5 字，进 `biz_status_log.reason`）。
    #[serde(default)]
    pub reason: Option<String>,
    /// 生效日（`yyyy-MM-dd`；转正时回写 `probation_end_date`，其它边忽略，见模块文档）。
    #[serde(default)]
    pub effective_date: Option<String>,
}

/// 绑定 / 解绑登录账号入参（设计 §5.2 #8）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmployeeBindUserReq {
    /// 账号 id；**不传 / null / 空串 = 解绑**。
    #[serde(default)]
    pub user_id: Option<String>,
}

/// 新建回执（设计 §5.2 #3 出参 `id, emp_no`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmployeeCreatedRes {
    /// 主键。
    pub id: String,
    /// 工号（留空 ⇒ `employee_no::generate_emp_no` 取号；有值 ⇒ 原样回显）。
    pub emp_no: String,
}

/// 修改 / 删除回执（设计 §5.2 #4 出参 `id`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmployeeIdRes {
    /// 主键。
    pub id: String,
}

/// 状态变更回执（设计 §5.2 #6 出参 `status`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmployeeStatusRes {
    /// 主键。
    pub id: String,
    /// 状态码值。
    pub status: String,
    /// 状态中文（前端直出，不硬编码码表）。
    pub status_label: String,
}

/// 绑账号回执（设计 §5.2 #8 出参 `user_id, username`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmployeeBindUserRes {
    /// 账号 id（解绑后为 `null`）。
    pub user_id: Option<String>,
    /// 账号用户名（与 `user_id` 成对）。
    pub username: Option<String>,
}

// ==================== 入参解析原语 ====================

/// 必填 UUID 文本。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| bad_request(format!("{field} 不是合法的主键：`{raw}`")))
}

/// 可空 UUID 文本（`None` / 空白 = 空）。
fn parse_optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => Uuid::parse_str(text)
            .map(Some)
            .map_err(|_| bad_request(format!("{field} 不是合法的主键：`{text}`"))),
    }
}

/// 必填日期（`yyyy-MM-dd`）。
fn parse_date(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| bad_request(format!("{field} 必须是 yyyy-MM-dd 格式的日期：`{raw}`")))
}

/// 可空日期（`None` / 空白 = 空）。
fn parse_optional_date(raw: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => parse_date(text, field).map(Some),
    }
}

/// 必填文本：去空白后非空、限长。
fn require_text(value: &str, field: &str, max: usize) -> std::result::Result<String, ApiError> {
    let text = value.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    check_len(text, field, max)?;
    Ok(text.to_string())
}

/// 选填文本：空白 → `None`；有值则限长。
///
/// 空串与「没填」在库里都是 `NULL`（`NULLIF(?::text,'')`），所以这里直接归一成 `None`：
/// 否则「手机号填了个空格」会以空串存进去，列表上看起来像有值、导出却是空白。
fn optional_text(value: Option<&str>, field: &str, max: usize) -> std::result::Result<Option<String>, ApiError> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => {
            check_len(text, field, max)?;
            Ok(Some(text.to_string()))
        }
    }
}

/// 长度上限（按字符数，与 `VARCHAR(n)` 同口径 —— PG 的 n 是字符不是字节）。
fn check_len(text: &str, field: &str, max: usize) -> std::result::Result<(), ApiError> {
    if text.chars().count() > max {
        return Err(bad_request(format!(
            "{field} 最长 {max} 个字符（当前 {} 个）",
            text.chars().count()
        )));
    }
    Ok(())
}

/// 枚举域校验（大写化后比对，回传归一值）。
fn require_one_of(value: &str, allowed: &[&str], field: &str, hint: &str) -> std::result::Result<String, ApiError> {
    let text = value.trim().to_ascii_uppercase();
    if allowed.contains(&text.as_str()) {
        return Ok(text);
    }
    Err(bad_request(format!(
        "{field} 只能是 {} 之一（当前 `{value}`）{hint}",
        allowed.join(" / ")
    )))
}

/// 性别码值（缺省 0 = 未知）。
fn parse_gender(raw: Option<i16>) -> std::result::Result<i16, ApiError> {
    match raw.unwrap_or(0) {
        0..=2 => Ok(raw.unwrap_or(0)),
        other => Err(bad_request(format!(
            "gender 只能是 0 未知 / 1 男 / 2 女（当前 {other}）"
        ))),
    }
}

/// 分页参数归一（`page` 从 1 起，`page_size` 夹在 1..=[`MAX_PAGE_SIZE`]）。
fn normalize_page(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    let page = page.unwrap_or(1).max(1);
    let page_size = page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE);
    (page, page_size)
}

/// 查询串 → [`EmployeeFilter`]（**纯函数**：不连库就能把「逗号拆分 / 枚举 / 默认口径」测掉）。
pub(crate) fn filter_of(query: &EmployeePageQuery) -> std::result::Result<EmployeeFilter, ApiError> {
    let keyword = query
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);

    let dept_id = parse_optional_uuid(query.dept_id.as_deref(), "dept_id")?;

    let mut statuses: Vec<String> = Vec::new();
    if let Some(raw) = query.status.as_deref() {
        for token in raw.split(',') {
            let text = token.trim();
            if text.is_empty() {
                continue;
            }
            statuses.push(require_one_of(
                text,
                employee_dto::STATUS_VALUES,
                "status",
                "（多选用逗号分隔，如 status=ACTIVE,SUSPENDED）",
            )?);
        }
    }

    let employment_type = match query.employment_type.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(require_one_of(raw, &EMPLOYMENT_TYPES, "employment_type", "")?),
    };

    Ok(EmployeeFilter {
        keyword,
        dept_id,
        statuses,
        employment_type,
        include_resigned: query.include_resigned.unwrap_or(false),
    })
}

// ==================== 写入载荷组装（纯函数） ====================

/// 新建入参 → 写入载荷（含全部字段级校验）。
///
/// 不碰库：部门 / 账号的**引用存在性**由 [`create_employee`] 单独查（那两件是 IO，
/// 与「格式对不对」分开测）。
///
/// # `emp_no` 可以是空串（= 待取号，HR-14）
///
/// 留空（`None` / `""` / 纯空白）⇒ 这里给 `emp_no = ""`，由 [`create_employee`] 在
/// **同一个事务**里调 [`employee_no::generate_emp_no`] 回填后再 INSERT：
/// 取号要 `biz_seq` 的行锁 + 查重，那是 IO，不该混进这个纯函数（否则单测要连库）。
/// `employee_model::insert` 之前一定已经回填过（[`create_employee`] 的路径唯一），
/// 空串落库会被 `NOT NULL` 之外的 CHECK/唯一键当普通值收下 —— 所以那里有一句
/// `debug_assert` 兜住「有人绕过 create 直接调 insert」。
pub(crate) fn build_write(req: &EmployeeCreateReq) -> std::result::Result<EmployeeWrite, ApiError> {
    // 有值则原样保留（企业自有工号体系），留空交给取号器 —— 口径与 `erp::seq::manual_code` 一致。
    let emp_no = match req.emp_no.as_deref().map(str::trim) {
        None | Some("") => String::new(),
        Some(raw) => require_text(raw, "emp_no", MAX_EMP_NO)?,
    };

    let name = require_text(req.name.as_deref().unwrap_or_default(), "name", MAX_NAME)?;
    let gender = parse_gender(req.gender)?;

    let id_card_type = match req.id_card_type.as_deref().map(str::trim) {
        None | Some("") => "ID_CARD".to_string(),
        Some(raw) => require_one_of(raw, &ID_CARD_TYPES, "id_card_type", "")?,
    };
    let employment_type = match req.employment_type.as_deref().map(str::trim) {
        None | Some("") => "FULL_TIME".to_string(),
        Some(raw) => require_one_of(raw, &EMPLOYMENT_TYPES, "employment_type", "")?,
    };

    let hire_date_raw = req.hire_date.as_deref().unwrap_or_default();
    if hire_date_raw.trim().is_empty() {
        return Err(bad_request(
            "hire_date（入职日期）不能为空：员工档案的排序与考勤口径都以它为准",
        ));
    }
    let hire_date = parse_date(hire_date_raw, "hire_date")?;
    let probation_end_date = parse_optional_date(req.probation_end_date.as_deref(), "probation_end_date")?;
    if let Some(end) = probation_end_date
        && end < hire_date
    {
        return Err(bad_request(format!(
            "probation_end_date（{end}）不能早于 hire_date（{hire_date}）"
        )));
    }

    let dept_id = {
        let raw = req.dept_id.as_deref().unwrap_or_default();
        if raw.trim().is_empty() {
            return Err(bad_request(
                "dept_id（人事归属部门）不能为空：数据权限按部门裁剪，无部门档案谁都看不到",
            ));
        }
        parse_uuid(raw, "dept_id")?
    };

    Ok(EmployeeWrite {
        emp_no,
        name,
        gender,
        birth_date: parse_optional_date(req.birth_date.as_deref(), "birth_date")?,
        id_card: optional_text(req.id_card.as_deref(), "id_card", MAX_ID_CARD)?,
        id_card_type,
        mobile: optional_text(req.mobile.as_deref(), "mobile", MAX_MOBILE)?,
        email: optional_text(req.email.as_deref(), "email", MAX_EMAIL)?,
        dept_id,
        position_code: optional_text(req.position_code.as_deref(), "position_code", MAX_POSITION)?,
        job_level: optional_text(req.job_level.as_deref(), "job_level", MAX_JOB_LEVEL)?,
        employment_type,
        hire_date,
        probation_end_date,
        education: optional_text(req.education.as_deref(), "education", MAX_JOB_LEVEL)?,
        school: optional_text(req.school.as_deref(), "school", MAX_SCHOOL)?,
        major: optional_text(req.major.as_deref(), "major", MAX_MAJOR)?,
        graduate_date: parse_optional_date(req.graduate_date.as_deref(), "graduate_date")?,
        native_place: optional_text(req.native_place.as_deref(), "native_place", MAX_PLACE)?,
        address: optional_text(req.address.as_deref(), "address", MAX_ADDRESS)?,
        emergency_contact: optional_text(req.emergency_contact.as_deref(), "emergency_contact", MAX_PERSON_NAME)?,
        emergency_phone: optional_text(req.emergency_phone.as_deref(), "emergency_phone", MAX_PHONE)?,
        emergency_relation: optional_text(req.emergency_relation.as_deref(), "emergency_relation", MAX_RELATION)?,
        bank_name: optional_text(req.bank_name.as_deref(), "bank_name", MAX_BANK_NAME)?,
        bank_branch: optional_text(req.bank_branch.as_deref(), "bank_branch", MAX_BRANCH)?,
        bank_account: optional_text(req.bank_account.as_deref(), "bank_account", MAX_BANK_ACCOUNT)?,
        bank_account_name: optional_text(req.bank_account_name.as_deref(), "bank_account_name", MAX_PERSON_NAME)?,
        user_id: parse_optional_uuid(req.user_id.as_deref(), "user_id")?,
        remark: optional_text(req.remark.as_deref(), "remark", usize::MAX)?,
    })
}

/// 把锁定行转回写入载荷（状态接口转正时只改 `probation_end_date`，其余列原值回写）。
pub(crate) fn write_of_row(row: &EmployeeRow) -> EmployeeWrite {
    EmployeeWrite {
        emp_no: row.emp_no.clone(),
        name: row.name.clone(),
        gender: row.gender,
        birth_date: row.birth_date,
        id_card: row.id_card.clone(),
        id_card_type: row.id_card_type.clone(),
        mobile: row.mobile.clone(),
        email: row.email.clone(),
        dept_id: row.dept_id,
        position_code: row.position_code.clone(),
        job_level: row.job_level.clone(),
        employment_type: row.employment_type.clone(),
        hire_date: row.hire_date,
        probation_end_date: row.probation_end_date,
        education: row.education.clone(),
        school: row.school.clone(),
        major: row.major.clone(),
        graduate_date: row.graduate_date,
        native_place: row.native_place.clone(),
        address: row.address.clone(),
        emergency_contact: row.emergency_contact.clone(),
        emergency_phone: row.emergency_phone.clone(),
        emergency_relation: row.emergency_relation.clone(),
        bank_name: row.bank_name.clone(),
        bank_branch: row.branch.clone(),
        bank_account: row.bank_account.clone(),
        bank_account_name: row.bank_account_name.clone(),
        user_id: row.user_id,
        remark: row.remark.clone(),
    }
}

/// 修改入参 + 锁定行 → 写入载荷（缺省字段回填当前值，空串清空）。
pub(crate) fn merged_write(req: &EmployeeUpdateReq, row: &EmployeeRow) -> std::result::Result<EmployeeWrite, ApiError> {
    let name = match req.name.as_deref() {
        None => row.name.clone(),
        Some(raw) => require_text(raw, "name", MAX_NAME)?,
    };
    let gender = match req.gender {
        None => row.gender,
        Some(raw) => parse_gender(Some(raw))?,
    };
    let id_card_type = match req.id_card_type.as_deref().map(str::trim) {
        None | Some("") => row.id_card_type.clone(),
        Some(raw) => require_one_of(raw, &ID_CARD_TYPES, "id_card_type", "")?,
    };
    let employment_type = match req.employment_type.as_deref().map(str::trim) {
        None | Some("") => row.employment_type.clone(),
        Some(raw) => require_one_of(raw, &EMPLOYMENT_TYPES, "employment_type", "")?,
    };
    let hire_date = match req.hire_date.as_deref().map(str::trim) {
        None | Some("") => row.hire_date,
        Some(raw) => parse_date(raw, "hire_date")?,
    };
    let probation_end_date = match req.probation_end_date.as_deref() {
        None => row.probation_end_date,
        Some(raw) => parse_optional_date(Some(raw), "probation_end_date")?,
    };
    if let Some(end) = probation_end_date
        && end < hire_date
    {
        return Err(bad_request(format!(
            "probation_end_date（{end}）不能早于 hire_date（{hire_date}）"
        )));
    }
    let dept_id = match req.dept_id.as_deref().map(str::trim) {
        None | Some("") => row.dept_id,
        Some(raw) => parse_uuid(raw, "dept_id")?,
    };

    Ok(EmployeeWrite {
        emp_no: row.emp_no.clone(),
        name,
        gender,
        birth_date: merge_date(req.birth_date.as_deref(), row.birth_date, "birth_date")?,
        id_card: merge_text(req.id_card.as_deref(), &row.id_card, "id_card", MAX_ID_CARD)?,
        id_card_type,
        mobile: merge_text(req.mobile.as_deref(), &row.mobile, "mobile", MAX_MOBILE)?,
        email: merge_text(req.email.as_deref(), &row.email, "email", MAX_EMAIL)?,
        dept_id,
        position_code: merge_text(
            req.position_code.as_deref(),
            &row.position_code,
            "position_code",
            MAX_POSITION,
        )?,
        job_level: merge_text(req.job_level.as_deref(), &row.job_level, "job_level", MAX_JOB_LEVEL)?,
        employment_type,
        hire_date,
        probation_end_date,
        education: merge_text(req.education.as_deref(), &row.education, "education", MAX_JOB_LEVEL)?,
        school: merge_text(req.school.as_deref(), &row.school, "school", MAX_SCHOOL)?,
        major: merge_text(req.major.as_deref(), &row.major, "major", MAX_MAJOR)?,
        graduate_date: merge_date(req.graduate_date.as_deref(), row.graduate_date, "graduate_date")?,
        native_place: merge_text(
            req.native_place.as_deref(),
            &row.native_place,
            "native_place",
            MAX_PLACE,
        )?,
        address: merge_text(req.address.as_deref(), &row.address, "address", MAX_ADDRESS)?,
        emergency_contact: merge_text(
            req.emergency_contact.as_deref(),
            &row.emergency_contact,
            "emergency_contact",
            MAX_PERSON_NAME,
        )?,
        emergency_phone: merge_text(
            req.emergency_phone.as_deref(),
            &row.emergency_phone,
            "emergency_phone",
            MAX_PHONE,
        )?,
        emergency_relation: merge_text(
            req.emergency_relation.as_deref(),
            &row.emergency_relation,
            "emergency_relation",
            MAX_RELATION,
        )?,
        bank_name: merge_text(req.bank_name.as_deref(), &row.bank_name, "bank_name", MAX_BANK_NAME)?,
        bank_branch: merge_text(req.bank_branch.as_deref(), &row.branch, "bank_branch", MAX_BRANCH)?,
        bank_account: merge_text(
            req.bank_account.as_deref(),
            &row.bank_account,
            "bank_account",
            MAX_BANK_ACCOUNT,
        )?,
        bank_account_name: merge_text(
            req.bank_account_name.as_deref(),
            &row.bank_account_name,
            "bank_account_name",
            MAX_PERSON_NAME,
        )?,
        user_id: row.user_id,
        remark: merge_text(req.remark.as_deref(), &row.remark, "remark", usize::MAX)?,
    })
}

/// 可选文本合并：`None` = 不改，`Some("")` = 清空，其它 = 覆盖（限长）。
fn merge_text(
    patch: Option<&str>,
    current: &Option<String>,
    field: &str,
    max: usize,
) -> std::result::Result<Option<String>, ApiError> {
    match patch {
        None => Ok(current.clone()),
        Some(raw) => optional_text(Some(raw), field, max),
    }
}

/// 可选日期合并：`None` = 不改，`Some("")` = 清空，其它 = 覆盖。
fn merge_date(
    patch: Option<&str>,
    current: Option<NaiveDate>,
    field: &str,
) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match patch {
        None => Ok(current),
        Some(raw) => parse_optional_date(Some(raw), field),
    }
}

// ==================== 状态变更守卫（纯函数） ====================

/// 状态变更的**字段级**守卫（合法边由 `state_machine` 判，这里只管字段与业务前置）。
///
/// 覆盖四件事（设计 §4.2.1 的表格）：
///
/// 1. `target` 必须在状态域内；`RESIGNED` **一律拒绝**（唯一入口是离职单审批回调）；
/// 2. 原地流转（`from == to`）→ 409（状态日志不该出现无变化记录）；
/// 3. `PROBATION → ACTIVE` 必须有生效中的劳动合同（该边的守卫）；
/// 4. `ACTIVE → SUSPENDED` 必须给 ≥[`MIN_REASON_CHARS`] 字的原因（进 `biz_status_log`）。
pub(crate) fn check_status_change(
    current: &str,
    target: &str,
    reason: Option<&str>,
    has_active_contract: bool,
    hire_date: NaiveDate,
    effective_date: Option<NaiveDate>,
) -> std::result::Result<(), ApiError> {
    if !employee_dto::STATUS_VALUES.contains(&target) {
        return Err(bad_request(format!(
            "to_status 只能是 {} 之一（当前 `{target}`）",
            employee_dto::STATUS_VALUES.join(" / ")
        )));
    }
    if target == "RESIGNED" {
        return Err(conflict(
            "离职不能直接改状态：唯一合法入口是离职单（流程 hr_employee_resign）审批通过后的回调 —— \
             它会同时写离职日期、离职类型并停用登录账号。请到「请假 / 离职」发起离职单",
        ));
    }
    if current == target {
        return Err(conflict(format!(
            "员工状态已经是「{}」，本次没有变化（原地流转不写状态日志）",
            employee_dto::status_label(current)
        )));
    }
    if let Some(date) = effective_date
        && date < hire_date
    {
        return Err(bad_request(format!(
            "effective_date（{date}）不能早于入职日期（{hire_date}）"
        )));
    }
    match (current, target) {
        ("PROBATION", "ACTIVE") => {
            if !has_active_contract {
                return Err(bad_request(
                    "转正必须先有生效中的劳动合同：请在「劳动合同」里登记一份 status=ACTIVE 的合同\
                     （设计 §4.2.1 的守卫，避免「没合同的人被转正」）",
                ));
            }
        }
        ("ACTIVE", "SUSPENDED") => {
            let text = reason.map(str::trim).unwrap_or_default();
            if text.chars().count() < MIN_REASON_CHARS {
                return Err(bad_request(format!(
                    "停薪留职必须写清原因（≥{MIN_REASON_CHARS} 字）：原因会进状态日志，\
                     审计要能回答「为什么停的」"
                )));
            }
        }
        _ => {}
    }
    Ok(())
}

// ==================== ① 列表 ====================

/// 员工列表（设计 §5.2 #1）。
///
/// 数据权限走 `employee_model::page` → `scope::paginate`：`DEPT` / `DEPT_AND_CHILD` 只出本部门
/// （及下级），`SELF` 只出自己的档案（`owner_col = user_id`），`ALL` / 超管全出。
#[nik::permission(
    path = "/business/hr/employee/page",
    method = "GET",
    permission = "business:hr:employee:list"
)]
#[nik::get("/business/hr/employee/page")]
pub async fn employee_page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<EmployeePageQuery>,
) -> ResResult<PageData<Vec<EmployeeListRes>>> {
    let (page, page_size) = normalize_page(query.page, query.page_size);
    let filter = filter_of(&query)?;

    let principal = principal::resolve_for_claims(&claims).await?;
    let mask = mask_of(&principal, &claims).await?;
    let (list, total) = page_employees(&principal, &mask, &filter, page, page_size).await?;
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 列表组装（接口与真库用例共用同一条路径）：分页 → 出参，字典名一次查全。
pub async fn page_employees(
    principal: &ScopedPrincipal,
    mask: &MaskSet,
    filter: &EmployeeFilter,
    page: u64,
    page_size: u64,
) -> std::result::Result<(Vec<EmployeeListRes>, i64), ApiError> {
    let (rows, total) = employee_model::page(filter, page, page_size, principal)
        .await
        .map_err(|error| utils::db_error(error, "分页查询员工档案"))?;

    // 字典名一次查全（岗位 / 职级两列）：几十行共用一份 map，杜绝 N+1。
    let labels = dict_labels().await?;
    let as_of = employee_dto::today();
    let list: Vec<EmployeeListRes> = rows
        .iter()
        .map(|row| {
            let names = names_from_map(&labels, &row.position_code, &row.job_level, None);
            EmployeeListRes::from_row(row, &names, mask, as_of)
        })
        .collect();
    Ok((list, total))
}

// ==================== ② 详情 ====================

/// 员工详情（设计 §5.2 #2）：全字段 + `*_name` 成对 + `sensitive_masked` + 合同 / 薪资标准。
///
/// 数据范围之外 → 404（见模块文档「数据权限」）。
#[nik::permission(
    path = "/business/hr/employee/{id}",
    method = "GET",
    permission = "business:hr:employee:detail"
)]
#[nik::get("/business/hr/employee/{id}")]
pub async fn employee_detail(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<EmployeeDetailRes> {
    let id = parse_uuid(&id, "id")?;
    let principal = principal::resolve_for_claims(&claims).await?;
    let mask = mask_of(&principal, &claims).await?;
    Ok(Res::ok(detail_of(&principal, id, &mask).await?))
}

/// 详情组装（接口与真库用例共用同一条路径）。
pub async fn detail_of(
    principal: &ScopedPrincipal,
    id: Uuid,
    mask: &MaskSet,
) -> std::result::Result<EmployeeDetailRes, ApiError> {
    ensure_visible(db(), principal, id).await?;
    let row = employee_model::find(id)
        .await
        .map_err(|error| utils::db_error(error, "查询员工档案"))?
        .ok_or_else(not_visible)?;

    let labels = dict_labels().await?;
    let names = names_from_map(&labels, &row.position_code, &row.job_level, row.education.as_deref());
    let (create_by, update_by) = operator_names(row.create_by, row.update_by).await?;
    let names = names.with_operators(create_by.as_deref(), update_by.as_deref());

    let as_of = employee_dto::today();
    let contracts = load_contracts(db(), id).await?;
    let salary_items = load_salary_items(db(), id).await?;

    Ok(EmployeeDetailRes::from_row(&row, &names, mask)
        .with_contracts(contracts, as_of)
        .with_salary_items(salary_items, as_of))
}

// ==================== ③ 新建 ====================

/// 新建员工（设计 §5.2 #3）。新档一律 `PROBATION`（设计 §4.2.1 的状态机入口）。
#[nik::permission(
    path = "/business/hr/employee",
    method = "POST",
    permission = "business:hr:employee:create"
)]
#[nik::post("/business/hr/employee")]
pub async fn employee_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<EmployeeCreateReq>,
) -> ResResult<EmployeeCreatedRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_employee(operator, &req).await?))
}

/// 新建的业务实现（与接口同一条代码路径，单测只跳过认证）。
///
/// # 工号取号（HR-14）
///
/// `emp_no` 留空 ⇒ [`employee_no::generate_emp_no`] 取号（`EMP-0001` 形态）；有值 ⇒ 原样保留。
/// 取号与 `INSERT` 在**同一个事务**里：
///
/// * 取号失败（号段规则缺失/停用）⇒ 整个事务回滚，`biz_seq.current_no` 不动 ⇒ **不留空号**；
/// * 插入失败（身份证 / 账号撞唯一键、CHECK 违反）⇒ 同样回滚，号不被白吃 ——
///   工号是人工肉眼可读的档案码，空号会被误读成「有人被删档」。
///
/// 部门 / 账号的引用存在性检查放在**取号之前**（纯读，不进事务）：格式或引用错时
/// 连 `biz_seq` 的行锁都不必碰。
pub async fn create_employee(
    operator: Option<Uuid>,
    req: &EmployeeCreateReq,
) -> std::result::Result<EmployeeCreatedRes, ApiError> {
    let mut draft = build_write(req)?;
    ensure_dept_exists(draft.dept_id).await?;
    if let Some(user_id) = draft.user_id {
        account_of(user_id).await?;
    }

    let id = Uuid::now_v7();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启新建员工事务"))?;
    if draft.emp_no.is_empty() {
        match employee_no::generate_emp_no(&tx).await {
            Ok(emp_no) => draft.emp_no = emp_no,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        }
    }
    // 取号之后 `emp_no` 一定非空；这条断言兜住「将来有人绕过本函数直接调 insert」。
    debug_assert!(!draft.emp_no.is_empty(), "工号必须在此处已回填（取号与插入同事务）");
    if let Err(error) = employee_model::insert(&tx, id, &draft, operator).await {
        let _ = tx.rollback().await;
        return Err(employee_model::write_error(
            error,
            "hr_employee_insert",
            &employee_model::WriteValues::of(&draft),
        ));
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交新建员工事务"))?;

    Ok(EmployeeCreatedRes {
        id: id.to_string(),
        emp_no: draft.emp_no,
    })
}

// ==================== ④ 修改 ====================

/// 修改员工（设计 §5.2 #4）：整表单提交，缺省字段回填当前值（见 [`EmployeeUpdateReq`]）。
///
/// `emp_no` / `org_id` / `status` / `user_id` 不在可改列里（工号是身份、组织是隔离维度、
/// 状态只能走状态机、账号走 `bind-user`）—— 由 `employee_model::UPDATE_SQL` 的列白名单兜住。
#[nik::permission(
    path = "/business/hr/employee/{id}",
    method = "PUT",
    permission = "business:hr:employee:update"
)]
#[nik::put("/business/hr/employee/{id}")]
pub async fn employee_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<EmployeeUpdateReq>,
) -> ResResult<EmployeeIdRes> {
    let id = parse_uuid(&id, "id")?;
    let operator = utils::current_user_uuid(&claims);
    let principal = principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(update_employee(&principal, operator, id, &req).await?))
}

/// 修改的业务实现。
pub async fn update_employee(
    principal: &ScopedPrincipal,
    operator: Option<Uuid>,
    id: Uuid,
    req: &EmployeeUpdateReq,
) -> std::result::Result<EmployeeIdRes, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启修改员工事务"))?;
    let row = match lock_visible(&tx, principal, id).await {
        Ok(row) => row,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    if employee_dto::STATUS_LABELS
        .iter()
        .any(|(code, _)| *code == row.status && *code == "RESIGNED")
    {
        let _ = tx.rollback().await;
        return Err(conflict(
            "已离职档案是只读的（主数据基线：入职日期 / 部门一改，历史薪资单的快照就对不上）。\
             需要更正请撤销离职单，或新建一份档案",
        ));
    }

    let draft = match merged_write(req, &row) {
        Ok(draft) => draft,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    if draft.dept_id != row.dept_id
        && let Err(error) = ensure_dept_exists(draft.dept_id).await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    let affected = match employee_model::update(&tx, id, &draft, operator).await {
        Ok(affected) => affected,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(employee_model::write_error(
                error,
                "hr_employee_update",
                &employee_model::WriteValues::of(&draft),
            ));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(not_visible());
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交修改员工事务"))?;
    Ok(EmployeeIdRes { id: id.to_string() })
}

// ==================== ⑤ 删除 ====================

/// 删除员工（**软删**，设计 §5.2 #5）：`deleted_at` 落时间戳，三个唯一键随之释放
/// （离职返聘可复用原工号，设计 §3.1 ① 的部分唯一索引）。
#[nik::permission(
    path = "/business/hr/employee/{id}",
    method = "DELETE",
    permission = "business:hr:employee:delete"
)]
#[nik::delete("/business/hr/employee/{id}")]
pub async fn employee_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<EmployeeIdRes> {
    let id = parse_uuid(&id, "id")?;
    let operator = utils::current_user_uuid(&claims);
    let principal = principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(delete_employee(&principal, operator, id).await?))
}

/// 删除的业务实现。
pub async fn delete_employee(
    principal: &ScopedPrincipal,
    operator: Option<Uuid>,
    id: Uuid,
) -> std::result::Result<EmployeeIdRes, ApiError> {
    // 先判可见性（越权者只能拿到 404，不能借「删除成功 / 失败」探测档案是否存在）。
    ensure_visible(db(), principal, id).await?;
    let affected = employee_model::soft_delete(db(), id, operator)
        .await
        .map_err(|error| utils::db_error(error, "删除员工档案"))?;
    if affected == 0 {
        return Err(not_visible());
    }
    Ok(EmployeeIdRes { id: id.to_string() })
}

// ==================== ⑥ 状态变更 ====================

/// 员工状态流转（设计 §5.2 #6）：转正 / 停薪留职 / 复职。
///
/// 合法边与 `biz_status_log` 由 `state_machine::transition` 一个入口负责（CAS 回写同事务），
/// 本接口只补字段级守卫（[`check_status_change`]）。`RESIGNED` 一律拒绝（走离职单）。
#[nik::permission(
    path = "/business/hr/employee/{id}/status",
    method = "POST",
    permission = "business:hr:employee:update"
)]
#[nik::post("/business/hr/employee/{id}/status")]
pub async fn employee_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<EmployeeStatusReq>,
) -> ResResult<EmployeeStatusRes> {
    let id = parse_uuid(&id, "id")?;
    let principal = principal::resolve_for_claims(&claims).await?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(change_status(&principal, operator, id, &req).await?))
}

/// 状态变更的业务实现。
pub async fn change_status(
    principal: &ScopedPrincipal,
    operator: Operator,
    id: Uuid,
    req: &EmployeeStatusReq,
) -> std::result::Result<EmployeeStatusRes, ApiError> {
    let target = require_one_of(
        req.to_status.as_deref().unwrap_or_default(),
        employee_dto::STATUS_VALUES,
        "to_status",
        "",
    )?;
    let reason = req.reason.as_deref().map(str::trim).filter(|text| !text.is_empty());
    let effective_date = parse_optional_date(req.effective_date.as_deref(), "effective_date")?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启员工状态变更事务"))?;
    let row = match lock_visible(&tx, principal, id).await {
        Ok(row) => row,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    let has_active_contract = matches!((row.status.as_str(), target.as_str()), ("PROBATION", "ACTIVE"))
        && match active_contract_exists(&tx, id).await {
            Ok(found) => found,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        };
    if let Err(error) = check_status_change(
        &row.status,
        &target,
        reason,
        has_active_contract,
        row.hire_date,
        effective_date,
    ) {
        let _ = tx.rollback().await;
        return Err(error);
    }

    // 状态机唯一入口：边校验 + reason 校验 + 写 biz_status_log（与本次回写同事务）。
    if let Err(error) = state_machine::transition(
        &tx,
        state_machine::ENTITY_EMPLOYEE,
        id,
        &row.emp_no,
        &state_machine::EMPLOYEE_EDGES,
        &row.status,
        &target,
        operator.clone(),
        reason,
        None,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }

    let affected = match employee_model::apply_status(&tx, id, &row.status, &target, None, None, operator.id).await {
        Ok(affected) => affected,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(employee_model::write_error(
                error,
                "hr_employee_status",
                &employee_model::WriteValues::default(),
            ));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(conflict(
            "员工状态在本次流转期间已被他人改动，请刷新后重试（并发保护：状态 CAS 只允许一个成功）",
        ));
    }

    // 转正且给了生效日 → 回写试用期截止日（转正日即试用期结束日）；同一事务，失败整笔回滚。
    if target == "ACTIVE"
        && row.status == "PROBATION"
        && let Some(date) = effective_date
    {
        let mut draft = write_of_row(&row);
        draft.probation_end_date = Some(date);
        if let Err(error) = employee_model::update(&tx, id, &draft, operator.id).await {
            let _ = tx.rollback().await;
            return Err(employee_model::write_error(
                error,
                "hr_employee_promotion",
                &employee_model::WriteValues::of(&draft),
            ));
        }
    }

    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交员工状态变更事务"))?;
    Ok(EmployeeStatusRes {
        id: id.to_string(),
        status_label: employee_dto::status_label(&target),
        status: target,
    })
}

// ==================== ⑧ 绑定登录账号 ====================

/// 绑定 / 解绑登录账号（设计 §5.2 #8）。传空 = 解绑。
///
/// 一人一档一账号（`uk_biz_employee_user`）：撞键翻 409 + 68003（`employee_model::write_error`）。
#[nik::permission(
    path = "/business/hr/employee/{id}/bind-user",
    method = "POST",
    permission = "business:hr:employee:update"
)]
#[nik::post("/business/hr/employee/{id}/bind-user")]
pub async fn employee_bind_user(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<EmployeeBindUserReq>,
) -> ResResult<EmployeeBindUserRes> {
    let id = parse_uuid(&id, "id")?;
    let operator = utils::current_user_uuid(&claims);
    let principal = principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(bind_user(&principal, operator, id, &req).await?))
}

/// 绑账号的业务实现。
pub async fn bind_user(
    principal: &ScopedPrincipal,
    operator: Option<Uuid>,
    id: Uuid,
    req: &EmployeeBindUserReq,
) -> std::result::Result<EmployeeBindUserRes, ApiError> {
    let user_id = parse_optional_uuid(req.user_id.as_deref(), "user_id")?;
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启绑定账号事务"))?;
    if let Err(error) = lock_visible(&tx, principal, id).await {
        let _ = tx.rollback().await;
        return Err(error);
    }
    // 账号存在性 + 人/机器人区分（V0102 的 user_type）：机器人账号挂员工会让工资表把 RPA 算成一个人。
    let username = match user_id {
        None => None,
        Some(account_id) => match account_of(account_id).await {
            Ok(username) => Some(username),
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        },
    };
    let affected = match employee_model::set_user_id(&tx, id, user_id, operator).await {
        Ok(affected) => affected,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(employee_model::write_error(
                error,
                "hr_employee_bind_user",
                &employee_model::WriteValues::default(),
            ));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(not_visible());
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交绑定账号事务"))?;
    Ok(EmployeeBindUserRes {
        user_id: user_id.map(|value| value.to_string()),
        username,
    })
}

// ==================== 共用：数据权限 / 关联查询 ====================

/// 本次请求的脱敏规则集合（`sys_field_mask` + 角色覆盖；明版权限不生效，见 HR-11 的取舍）。
async fn mask_of(principal: &ScopedPrincipal, claims: &TokenClaims) -> std::result::Result<MaskSet, ApiError> {
    crate::business::masking::resolve(Some(principal), Some(claims))
        .await
        .map_err(|error| utils::db_error(error, "加载字段脱敏规则"))
}

/// 单条可见性探针：数据范围之外 → 404（见模块文档）。
///
/// 用 [`scope::filter_for`] 生成与列表**同一份**片段（同一锚点、同一部门集合），
/// 所以「列表看得到、详情打不开」这类不一致不可能出现。
///
/// `pub(crate)`：HR-13 的明文接口（[`crate::business::hr::employee_sensitive`]）必须走
/// 同一份探针 —— 单条入口多一份「自己写的范围判断」就是多一个越权缺口。
pub(crate) async fn ensure_visible(
    ex: &impl SqlExecutor,
    principal: &ScopedPrincipal,
    id: Uuid,
) -> std::result::Result<(), ApiError> {
    let filter = scope::filter_for(principal, employee_model::EMPLOYEE_TARGET);
    let sql = format!(
        "SELECT e.id FROM biz_employee e WHERE e.deleted_at IS NULL {} AND e.id = ?::uuid",
        filter.sql()
    );
    let mut params: Vec<Value> = filter.params().to_vec();
    params.push(Value::from(&id));
    let found = ex
        .query_first_sql(sql, params)
        .await
        .map_err(|error| utils::db_error(error, "校验员工档案数据范围"))?;
    if found.is_none() {
        return Err(not_visible());
    }
    Ok(())
}

/// 取行 + 加锁 + 判可见（写路径统一入口：先锁再判，避免「判完可见、写时已被人改」）。
async fn lock_visible(
    ex: &impl SqlExecutor,
    principal: &ScopedPrincipal,
    id: Uuid,
) -> std::result::Result<EmployeeRow, ApiError> {
    let row = employee_model::lock_in(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定员工档案"))?
        .ok_or_else(not_visible)?;
    ensure_visible(ex, principal, id).await?;
    Ok(row)
}

/// 部门必须存在（未软删）：弱引用不建 FK，所以引用存在性只能在应用层查。
// SCOPE-EXEMPT: 按主键取单个部门的名称做引用校验（非列表 / 非导出查询），
//   数据范围应作用于员工行而不是「挑部门」这一步（部门选择框由 sys_dept 的既有接口裁剪）。
async fn ensure_dept_exists(dept_id: Uuid) -> std::result::Result<(), ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT dept_name FROM sys_dept WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&dept_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "校验人事归属部门"))?;
    if rows.is_empty() {
        return Err(bad_request(format!(
            "dept_id {dept_id} 不存在或已删除：员工档案必须挂在真实部门上（数据权限按部门裁剪）"
        )));
    }
    Ok(())
}

/// 登录账号必须存在、未删除、且是**人**（`user_type = 'HUMAN'`）；返回用户名。
// SCOPE-EXEMPT: 按主键取单个账号（绑账号 / 建档时的引用校验），非列表查询；
//   可见性由 business:hr:employee:* 权限码控制，账号本身另有 system:user:* 的界面入口。
async fn account_of(user_id: Uuid) -> std::result::Result<String, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT username, user_type FROM sys_user WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&user_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "校验登录账号"))?;
    let Some(row) = rows.first() else {
        return Err(bad_request(format!(
            "user_id {user_id} 不存在或已删除：请选择一个可用的登录账号"
        )));
    };
    let user_type: String = row.get_typed("user_type")?;
    if !user_type.eq_ignore_ascii_case("HUMAN") {
        return Err(bad_request(format!(
            "账号 {} 是「{}」类型：员工档案只能绑定人类账号（机器人 / 系统账号不该进工资表）",
            row.get_typed::<String>("username")?,
            user_type
        )));
    }
    Ok(row.get_typed("username")?)
}

/// `(字典编码, 值) → 中文`：一次查三个 HR 字典（岗位 / 职级 / 学历）。
///
/// 列表与详情共用这一份 map（几十行 / 一次请求只查一次库，杜绝 N+1）。
async fn dict_labels() -> std::result::Result<std::collections::HashMap<(String, String), String>, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT t.dict_code, i.item_value, i.item_label \
               FROM sys_dict_type t \
               JOIN sys_dict_item i ON i.dict_type_id = t.id AND i.deleted_at IS NULL \
              WHERE t.deleted_at IS NULL \
                AND t.dict_code IN ('hr_position', 'hr_job_level', 'hr_education')",
            vec![],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询 HR 字典"))?;
    let mut labels = std::collections::HashMap::with_capacity(rows.len());
    for row in &rows {
        labels.insert(
            (
                row.get_typed::<String>("dict_code")?,
                row.get_typed::<String>("item_value")?,
            ),
            row.get_typed::<String>("item_label")?,
        );
    }
    Ok(labels)
}

/// 从字典 map 拼 [`EmployeeNames`]（缺项留空，DTO 层负责「码值回退」）。
fn names_from_map(
    labels: &std::collections::HashMap<(String, String), String>,
    position_code: &Option<String>,
    job_level: &Option<String>,
    education: Option<&str>,
) -> EmployeeNames {
    let lookup = |code: Option<&str>, dict: &str| -> Option<String> {
        let code = code?.trim();
        if code.is_empty() {
            return None;
        }
        labels.get(&(dict.to_string(), code.to_string())).cloned()
    };
    EmployeeNames::none()
        .with_position(lookup(position_code.as_deref(), "hr_position").as_deref())
        .with_job_level(lookup(job_level.as_deref(), "hr_job_level").as_deref())
        .with_education(lookup(education, "hr_education").as_deref())
}

/// 创建人 / 修改人用户名（一次查两人；`real_name` 优先，空则回退 `username`）。
// SCOPE-EXEMPT: 按主键批量取两个操作人姓名（详情页的 create_by_name / update_by_name），
//   非列表查询；不做范围裁剪 —— 否则看不到自己档案的人连「谁建的档」都不显示。
async fn operator_names(
    create_by: Option<Uuid>,
    update_by: Option<Uuid>,
) -> std::result::Result<(Option<String>, Option<String>), ApiError> {
    if create_by.is_none() && update_by.is_none() {
        return Ok((None, None));
    }
    let rows = db()
        .find_with_params(
            "SELECT id::text AS id, COALESCE(NULLIF(real_name, ''), username) AS label \
               FROM sys_user WHERE id IN (NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)",
            vec![
                Value::Text(create_by.map(|id| id.to_string()).unwrap_or_default()),
                Value::Text(update_by.map(|id| id.to_string()).unwrap_or_default()),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询员工档案的创建人 / 修改人"))?;
    let mut map = std::collections::HashMap::with_capacity(rows.len());
    for row in &rows {
        map.insert(row.get_typed::<String>("id")?, row.get_typed::<String>("label")?);
    }
    Ok((
        create_by.and_then(|id| map.get(&id.to_string()).cloned()),
        update_by.and_then(|id| map.get(&id.to_string()).cloned()),
    ))
}

/// 该员工是否有生效中的劳动合同（`PROBATION → ACTIVE` 的守卫）。
async fn active_contract_exists(ex: &impl SqlExecutor, employee_id: Uuid) -> std::result::Result<bool, ApiError> {
    contract_model::has_active(ex, employee_id)
        .await
        .map_err(|error| utils::db_error(error, "查询生效中的劳动合同"))
}

/// 详情子表：全部劳动合同（时间线）。
///
/// SQL 由 HR-15 的 [`contract_model::list_by_employee`] 提供 —— 合同查询只有那一个出口，
/// 详情 / 合同回执 / 续签链看到的是同一份投影（`renewed_from_no` 走自连接，不许 N+1）。
async fn load_contracts(ex: &impl SqlExecutor, employee_id: Uuid) -> std::result::Result<Vec<ContractInput>, ApiError> {
    contract_model::list_by_employee(ex, employee_id)
        .await
        .map_err(|error| utils::db_error(error, "查询员工劳动合同"))
}

/// 详情子表：员工薪资标准（按 `effective_from DESC`）。
///
/// SQL 由 HR-16 的 [`salary_model::list_by_employee`] 提供 —— 薪资标准查询只有那一个出口，
/// 详情 / 薪资 tab / 调薪回执看到的是同一份投影。
async fn load_salary_items(
    ex: &impl SqlExecutor,
    employee_id: Uuid,
) -> std::result::Result<Vec<SalaryInput>, ApiError> {
    salary_model::list_by_employee(ex, employee_id)
        .await
        .map_err(|error| utils::db_error(error, "查询员工薪资标准"))
}

/// 时间戳（保留给后续卡的时间轴出参；本卡 DTO 走 `employee_dto::timestamp_cst`）。
pub fn timestamp_cst(value: DateTime<Utc>) -> String {
    employee_dto::timestamp_cst(value)
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 菜单 / 权限种子原文（编译期嵌入）。
    const V0387_SEED: &str = include_str!("../../../migrations/business/V0387__hr_menu_perms.up.sql");

    /// 最小可用新建入参。
    fn create_req() -> EmployeeCreateReq {
        EmployeeCreateReq {
            emp_no: Some("EMP-9001".to_string()),
            name: Some("张三".to_string()),
            dept_id: Some(Uuid::from_u128(0x9911).to_string()),
            hire_date: Some("2024-03-01".to_string()),
            ..EmployeeCreateReq::default()
        }
    }

    /// 一条锁定行（详情 / 合并用例的输入）。
    fn row_fixture() -> EmployeeRow {
        use nik::prelude::chrono::TimeZone;
        EmployeeRow {
            id: Uuid::from_u128(0x9001),
            org_id: Uuid::from_u128(0x9002),
            emp_no: "EMP-0007".to_string(),
            name: "张三".to_string(),
            gender: 1,
            birth_date: NaiveDate::from_ymd_opt(1990, 1, 1),
            id_card: Some("110101199001011234".to_string()),
            id_card_type: "ID_CARD".to_string(),
            mobile: Some("13800000000".to_string()),
            email: Some("zhangsan@example.com".to_string()),
            dept_id: Uuid::from_u128(0x9101),
            dept_name: "生产部".to_string(),
            position_code: Some("WORKER".to_string()),
            job_level: Some("P3".to_string()),
            employment_type: "FULL_TIME".to_string(),
            hire_date: NaiveDate::from_ymd_opt(2024, 3, 1).expect("合法日期"),
            probation_end_date: NaiveDate::from_ymd_opt(2024, 6, 1),
            leave_date: None,
            leave_type: None,
            status: "ACTIVE".to_string(),
            education: Some("BACHELOR".to_string()),
            school: Some("某大学".to_string()),
            major: Some("计算机".to_string()),
            graduate_date: NaiveDate::from_ymd_opt(2012, 6, 30),
            native_place: Some("北京".to_string()),
            address: Some("北京市海淀区".to_string()),
            emergency_contact: Some("李四".to_string()),
            emergency_phone: Some("13900000000".to_string()),
            emergency_relation: Some("配偶".to_string()),
            bank_name: Some("工商银行".to_string()),
            branch: Some("中关村支行".to_string()),
            bank_account: Some("6222021234567890".to_string()),
            bank_account_name: Some("张三".to_string()),
            user_id: Some(Uuid::from_u128(0x9201)),
            username: "zhangsan".to_string(),
            remark: Some("备注".to_string()),
            created_at: Utc.with_ymd_and_hms(2026, 11, 12, 2, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 11, 12, 2, 0, 0).unwrap(),
            create_by: Some(Uuid::from_u128(0x9202)),
            update_by: Some(Uuid::from_u128(0x9203)),
        }
    }

    /// 本卡用到的 5 个权限码必须全在 `V0387` 的菜单种子里（写错只会在线上变 403）。
    #[test]
    fn permission_codes_are_seeded_by_v0387() {
        for code in EMPLOYEE_PERMISSIONS {
            assert!(
                V0387_SEED.contains(code),
                "权限码 {code} 不在 V0387 的菜单种子里 —— 线上会 403"
            );
        }
    }

    /// 本文件声明的每条 `#[nik::*]` 路径都必须是 `ROUTE_CONTRACT` 里**登记给 HR-12** 的那几条。
    ///
    /// 按 `(方法, 路径)` 去重：`GET / PUT / DELETE` 共用 `/employee/{id}` 一条路径，
    /// 只按路径去重会把 7 条路由误算成 5 条。
    #[test]
    fn routes_are_the_six_card_routes() {
        use crate::business::hr::handler::ROUTE_CONTRACT;
        let mut mine: Vec<(String, String)> = Vec::new();
        let source = include_str!("employee_handler.rs");
        let pattern = regex::Regex::new(r#"#\[nik::(get|post|put|delete)\(\s*"([^"]+)""#).expect("正则合法");
        for capture in pattern.captures_iter(source) {
            mine.push((capture[1].to_ascii_uppercase(), capture[2].to_string()));
        }
        mine.sort_unstable();
        mine.dedup();
        assert_eq!(mine.len(), 7, "本卡 6 条 + #8 绑账号 = 7 条路由，实际 {mine:?}");

        for (method, path) in &mine {
            let spec = ROUTE_CONTRACT
                .iter()
                .find(|spec| spec.path == path && spec.method == method)
                .unwrap_or_else(|| panic!("{method} {path} 不在 ROUTE_CONTRACT 里 —— 先登记契约再实现"));
            assert_eq!(spec.card, "HR-12", "{method} {path} 在契约里登记的卡是 {}", spec.card);
        }
    }

    /// 每条路由的权限码与契约表逐字一致（这里是接口侧的第二道对上：`#[nik::permission]` 写错的码
    /// 不会编译失败，只会 403）。
    ///
    /// 按 `(方法, 路径)` 对上：`GET / PUT / DELETE` 共用 `/employee/{id}` 这一条路径，
    /// 它们的权限码分别是 `detail / update / delete` —— 只按路径找契约会取到错的那一行。
    #[test]
    fn permission_attributes_match_contract() {
        use crate::business::hr::handler::ROUTE_CONTRACT;
        let source = include_str!("employee_handler.rs");
        let pattern =
            regex::Regex::new(r#"permission = "([^"]+)"\s*\)\]\s*#\[nik::(get|post|put|delete)\(\s*"([^"]+)"\)"#)
                .expect("正则合法");
        let pairs: Vec<(String, String, String)> = pattern
            .captures_iter(source)
            .map(|capture| {
                (
                    capture[2].to_ascii_uppercase(),
                    capture[3].to_string(),
                    capture[1].to_string(),
                )
            })
            .collect();
        assert_eq!(pairs.len(), 7, "应当有 7 条「权限码 + 路由」声明，实际 {pairs:?}");
        for (method, path, permission) in pairs {
            let spec = ROUTE_CONTRACT
                .iter()
                .find(|spec| spec.path == path && spec.method == method)
                .unwrap_or_else(|| panic!("{method} {path} 不在契约表"));
            assert_eq!(
                spec.permission,
                Some(permission.as_str()),
                "{method} {path} 的权限码与契约不一致"
            );
        }
    }

    /// 分页筛选：逗号多选 / 枚举 / 默认口径（不含离职）。
    #[test]
    fn page_query_parses_filters() {
        let query = EmployeePageQuery {
            keyword: Some("  EMP-00  ".to_string()),
            dept_id: Some(Uuid::from_u128(0x9101).to_string()),
            status: Some(" ACTIVE , SUSPENDED ".to_string()),
            employment_type: Some("full_time".to_string()),
            include_resigned: None,
            page: None,
            page_size: None,
        };
        let filter = filter_of(&query).expect("合法筛选");
        assert_eq!(filter.keyword.as_deref(), Some("EMP-00"));
        assert_eq!(filter.dept_id, Some(Uuid::from_u128(0x9101)));
        assert_eq!(filter.statuses, vec!["ACTIVE".to_string(), "SUSPENDED".to_string()]);
        assert_eq!(filter.employment_type.as_deref(), Some("FULL_TIME"));
        assert!(!filter.include_resigned, "默认不含离职");

        // 空串 / 全空白 = 不筛
        let empty = filter_of(&EmployeePageQuery {
            keyword: Some("   ".to_string()),
            status: Some(" , ".to_string()),
            ..EmployeePageQuery::default()
        })
        .expect("空筛选");
        assert!(empty.keyword.is_none());
        assert!(empty.statuses.is_empty());
        // 空状态集 = 「默认在职三态」，由 `employee_model::list_query` 落成
        // `AND e.status <> 'RESIGNED'`（此处不重复一份名单，避免两处口径漂移）。
    }

    /// 状态枚举写错 → 400 且报文列出可选值（不是静默忽略）。
    #[test]
    fn page_query_rejects_unknown_status() {
        let error = filter_of(&EmployeePageQuery {
            status: Some("ACTIVE,WORKING".to_string()),
            ..EmployeePageQuery::default()
        })
        .expect_err("非法状态必须报错");
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("WORKING"), "报文点名非法值：{}", error.msg);
        assert!(error.msg.contains("RESIGNED"), "报文列出可选值：{}", error.msg);
    }

    /// 分页归一：页码 0 → 1，条数上限 200。
    #[test]
    fn page_normalization_clamps() {
        assert_eq!(normalize_page(None, None), (1, DEFAULT_PAGE_SIZE));
        assert_eq!(normalize_page(Some(0), Some(0)), (1, 1));
        assert_eq!(normalize_page(Some(3), Some(10_000)), (3, MAX_PAGE_SIZE));
    }

    /// 新建必填项：姓名 / 入职日期 / 部门，缺一即 400 且点名该填什么。
    ///
    /// 工号**不在此列**：留空 = 交给取号器（HR-14），见 [`create_auto_numbers_when_emp_no_blank`]。
    #[test]
    fn create_requires_mandatory_fields() {
        let missing_name = EmployeeCreateReq {
            name: None,
            ..create_req()
        };
        assert!(build_write(&missing_name).expect_err("缺姓名").msg.contains("name"));

        let missing_hire = EmployeeCreateReq {
            hire_date: None,
            ..create_req()
        };
        assert!(
            build_write(&missing_hire)
                .expect_err("缺入职日期")
                .msg
                .contains("hire_date")
        );

        let missing_dept = EmployeeCreateReq {
            dept_id: None,
            ..create_req()
        };
        assert!(build_write(&missing_dept).expect_err("缺部门").msg.contains("dept_id"));
    }

    /// 新建：枚举 / 日期 / 长度三类格式错都被挡在入库前，且默认值给对。
    #[test]
    fn create_validates_enums_dates_and_lengths() {
        let ok = build_write(&create_req()).expect("最小合法入参");
        assert_eq!(ok.id_card_type, "ID_CARD", "证件类型缺省 ID_CARD");
        assert_eq!(ok.employment_type, "FULL_TIME", "用工形式缺省 FULL_TIME");
        assert_eq!(ok.gender, 0, "性别缺省未知");
        assert_eq!(ok.emp_no, "EMP-9001");

        let bad_gender = EmployeeCreateReq {
            gender: Some(9),
            ..create_req()
        };
        assert!(build_write(&bad_gender).expect_err("性别非法").msg.contains("gender"));

        let bad_type = EmployeeCreateReq {
            id_card_type: Some("SFZ".to_string()),
            ..create_req()
        };
        assert!(
            build_write(&bad_type)
                .expect_err("证件类型非法")
                .msg
                .contains("id_card_type")
        );

        let bad_employment = EmployeeCreateReq {
            employment_type: Some("CONTRACTOR".to_string()),
            ..create_req()
        };
        assert!(
            build_write(&bad_employment)
                .expect_err("用工形式非法")
                .msg
                .contains("employment_type")
        );

        let bad_date = EmployeeCreateReq {
            hire_date: Some("2024/03/01".to_string()),
            ..create_req()
        };
        assert!(
            build_write(&bad_date)
                .expect_err("日期格式错")
                .msg
                .contains("yyyy-MM-dd")
        );

        let bad_probation = EmployeeCreateReq {
            probation_end_date: Some("2024-01-01".to_string()),
            ..create_req()
        };
        assert!(
            build_write(&bad_probation)
                .expect_err("试用期早于入职")
                .msg
                .contains("probation_end_date")
        );

        let long_name = EmployeeCreateReq {
            name: Some("张".repeat(MAX_NAME + 1)),
            ..create_req()
        };
        assert!(
            build_write(&long_name)
                .expect_err("姓名超长")
                .msg
                .contains(&MAX_NAME.to_string())
        );
    }

    /// HR-14：工号留空的三种形态（`None` / 空串 / 纯空白）都归一成 `""`（= 待取号），
    /// 有值则原样保留（去首尾空白，允许 `E2003` 这类企业自有号）。
    ///
    /// 取号本身要连库（`biz_seq` 行锁 + 查重），不在本纯函数里做 —— 真库行为见
    /// [`crate::business::hr::employee_no_db_tests`]。这里钉住的是**契约**：
    /// 「留空不是错误」，以及「有值时不静默改成自动号」。
    #[test]
    fn create_auto_numbers_when_emp_no_blank() {
        for blank in [None, Some("".to_string()), Some("   ".to_string())] {
            let draft = build_write(&EmployeeCreateReq {
                emp_no: blank.clone(),
                ..create_req()
            })
            .unwrap_or_else(|error| panic!("留空工号不该报错（{blank:?}）：{}", error.msg));
            assert_eq!(draft.emp_no, "", "留空 ⇒ 待取号（由 create 在同一事务里回填）");
        }

        let manual = build_write(&EmployeeCreateReq {
            emp_no: Some("  E2003  ".to_string()),
            ..create_req()
        })
        .expect("手工工号");
        assert_eq!(manual.emp_no, "E2003", "有值 ⇒ 原样保留（只去空白，不强制 EMP- 形态）");

        let too_long = build_write(&EmployeeCreateReq {
            emp_no: Some("E".repeat(MAX_EMP_NO + 1)),
            ..create_req()
        })
        .expect_err("超长工号");
        assert!(too_long.msg.contains("emp_no"), "超长点名列名：{}", too_long.msg);
    }

    /// 修改：不传 = 不改，空串 = 清空，有值 = 覆盖（三种语义各测一遍）。
    #[test]
    fn update_merge_keeps_absent_and_clears_empty() {
        let row = row_fixture();
        let req = EmployeeUpdateReq {
            name: Some("  李四  ".to_string()),
            mobile: Some("".to_string()),
            email: None,
            remark: Some("新备注".to_string()),
            ..EmployeeUpdateReq::default()
        };
        let draft = merged_write(&req, &row).expect("合法修改");
        assert_eq!(draft.name, "李四", "有值 → 覆盖并去空白");
        assert_eq!(draft.mobile, None, "空串 → 清空");
        assert_eq!(draft.email, row.email, "不传 → 保持原值");
        assert_eq!(draft.remark.as_deref(), Some("新备注"));
        // 工号 / 部门 / 账号这些「不在可改列」的字段一律取当前值
        assert_eq!(draft.emp_no, row.emp_no);
        assert_eq!(draft.dept_id, row.dept_id);
        assert_eq!(draft.user_id, row.user_id);
        assert_eq!(draft.hire_date, row.hire_date);
    }

    /// 修改：把姓名传空、日期传坏、试用期早于入职都要 400（不允许静默存空名）。
    #[test]
    fn update_rejects_invalid_values() {
        let row = row_fixture();
        let empty_name = EmployeeUpdateReq {
            name: Some("   ".to_string()),
            ..EmployeeUpdateReq::default()
        };
        assert!(
            merged_write(&empty_name, &row)
                .expect_err("空姓名")
                .msg
                .contains("name")
        );

        let bad_date = EmployeeUpdateReq {
            hire_date: Some("2024-13-01".to_string()),
            ..EmployeeUpdateReq::default()
        };
        assert!(
            merged_write(&bad_date, &row)
                .expect_err("坏日期")
                .msg
                .contains("hire_date")
        );

        let bad_probation = EmployeeUpdateReq {
            probation_end_date: Some("2023-01-01".to_string()),
            ..EmployeeUpdateReq::default()
        };
        assert!(
            merged_write(&bad_probation, &row)
                .expect_err("试用期早于入职")
                .msg
                .contains("probation_end_date")
        );
    }

    /// 状态守卫矩阵：离职拦截 / 原地流转 / 转正守卫 / 停薪原因字数 / 生效日。
    #[test]
    fn status_guard_matrix() {
        let hire = NaiveDate::from_ymd_opt(2024, 3, 1).expect("合法日期");

        // ① 离职一律拒绝（唯一入口是离职单审批）
        let resign = check_status_change("ACTIVE", "RESIGNED", Some("个人原因辞职"), true, hire, None)
            .expect_err("状态接口不能直接离职");
        assert_eq!(resign.status, 409);
        assert!(resign.msg.contains("离职单"), "报文点名该走哪条路：{}", resign.msg);

        // ② 状态域之外的取值 → 400
        let unknown = check_status_change("ACTIVE", "WORKING", None, true, hire, None).expect_err("非法状态");
        assert_eq!(unknown.code, error_code::PARAM_INVALID);

        // ③ 原地流转 → 409
        let same = check_status_change("ACTIVE", "ACTIVE", None, true, hire, None).expect_err("无变化");
        assert_eq!(same.status, 409);

        // ④ 转正必须有生效合同
        let no_contract =
            check_status_change("PROBATION", "ACTIVE", None, false, hire, None).expect_err("没有生效合同不能转正");
        assert_eq!(no_contract.code, error_code::PARAM_INVALID);
        assert!(no_contract.msg.contains("劳动合同"), "{}", no_contract.msg);
        check_status_change("PROBATION", "ACTIVE", None, true, hire, None).expect("有合同可转正");

        // ⑤ 停薪留职必须 ≥5 字原因
        let short = check_status_change("ACTIVE", "SUSPENDED", Some("有事"), false, hire, None).expect_err("原因太短");
        assert!(short.msg.contains(&MIN_REASON_CHARS.to_string()), "{}", short.msg);
        check_status_change("ACTIVE", "SUSPENDED", Some("出国进修一年"), false, hire, None).expect("原因够长");

        // ⑥ 复职不需要原因
        check_status_change("SUSPENDED", "ACTIVE", None, false, hire, None).expect("复职");

        // ⑦ 生效日不能早于入职日
        let early = check_status_change(
            "PROBATION",
            "ACTIVE",
            None,
            true,
            hire,
            NaiveDate::from_ymd_opt(2024, 1, 1),
        )
        .expect_err("生效日早于入职日");
        assert_eq!(early.code, error_code::PARAM_INVALID);
    }

    /// `biz_employee` 必须登记进 `SCOPED_TABLES`，且本文件的锚点与登记项**是同一个**。
    #[test]
    fn employee_is_registered_in_scoped_tables() {
        let registered = scope::SCOPED_TABLES
            .iter()
            .find(|target| target.table == employee_model::TABLE)
            .expect("biz_employee 未登记进 SCOPED_TABLES —— 数据权限不会生效");
        assert!(registered.is_scoped());
        assert_eq!(registered.dept_col, Some("dept_id"), "部门列（只能看本部门靠它）");
        assert_eq!(
            registered.owner_col,
            Some("user_id"),
            "SELF 范围的「本人」= 账号 id（设计 D-12）"
        );
        assert_eq!(
            *registered,
            employee_model::EMPLOYEE_TARGET,
            "登记项必须与模型层的分页锚点一致（两处不一致 = 列表与详情裁的不是同一批行）"
        );
    }

    /// 绑账号 / 建档的 UUID 解析：空 = 解绑，乱码 = 400（不静默当 None）。
    #[test]
    fn uuid_parsing_guards() {
        assert_eq!(parse_optional_uuid(None, "user_id").expect("空"), None);
        assert_eq!(parse_optional_uuid(Some("   "), "user_id").expect("空白"), None);
        let error = parse_optional_uuid(Some("not-a-uuid"), "user_id").expect_err("乱码");
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("user_id"), "{}", error.msg);
    }
}
