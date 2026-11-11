//! 「从登录账号批量生成员工档」（`seed-from-users`）—— 设计 §5.2 #10 / §7.2 存量数据 / §8.1 HR-17。
//!
//! 存量系统里「先有账号、后有 HR 模块」是常态：`sys_user` 已经有一批人，但
//! `biz_employee` 是空的。设计 §7.2 定的是**不自动回填**（人事主档要有工号 / 入职日期 /
//! 身份证，机器猜不出来），上线后由 HR 在员工档案页点「从账号生成员工档」：
//! 先 `dry_run=true` 出预览，确认后落库（`status='ACTIVE'`、`hire_date` 取 HR 选的日期、
//! 工号自动取号）。本文件就是这条链路的后端实现。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 卡 |
//! |:--:|---|---|---|---|
//! | 10 | POST | `/business/hr/employee/seed-from-users` | `business:hr:employee:create` | HR-17 |
//!
//! # 三个验收项 → 三条实现纪律
//!
//! | 验收项 | 实现纪律 | 真库证据（`employee_seed_db_tests`） |
//! |---|---|---|
//! | **dry-run 只报告不写库** | `dry_run` 缺省就是 `true`（不传不敢写）；判定与落库是**两段代码**，预览路径一次 `INSERT` 都不发 | `dry_run_writes_nothing_and_eats_no_number` |
//! | **批量建不回滚已成功的** | 每个候选**自己一个事务**（`BEGIN → 取号 → INSERT → COMMIT`），失败只回滚自己那一个，循环继续 | `failure_isolates_earlier_successes` |
//! | **`user_id` 回写正确** | `user_id` 写进 [`EmployeeWrite`] 由 `INSERT` **同一条语句**落库（不是先建后 UPDATE） | `creates_active_employees_and_writes_back_user_id` |
//!
//! ## 为什么每个候选一个事务，而不是整批一个事务
//!
//! 卡面验收写的是「批量建**不回滚已成功的**」：500 个账号里第 37 个撞了唯一键，
//! 整批回滚意味着 HR 要重来一遍、还要自己找出「哪 36 个已经好了」——而重来那 36 个
//! 会因为「账号已有档案」被跳过（结果倒是对，但白转一圈、号段白吃 36 个号）。
//! 逐条提交的代价是「没有全有全无」，但这正是想要的语义：**建档是幂等的单条操作**，
//! 失败的那条在 `preview[]` 里点明原因，HR 修好那一个人的账号（或改名）再点一次即可
//! （重复提交不会重复建档，见下）。
//!
//! ## 幂等：重复点是安全的
//!
//! 候选查询带 `LEFT JOIN biz_employee e ON e.user_id = u.id AND e.deleted_at IS NULL`，
//! 已经有档案的账号一律 `SKIP/HAS_EMPLOYEE`；数据库侧还有 `uk_biz_employee_user`
//! （HR-10 翻译成 `409 + 68003`）兜底。所以「点两次」= 第二次 `created=0`。
//!
//! ## 取号与插入必须同事务（HR-14 的既有口径）
//!
//! [`employee_no::generate_emp_no`] 推进 `biz_seq.current_no`，它拿到的执行器就是
//! 本条候选的事务：插入失败 → 回滚 → **号不被白吃**（工号是肉眼可读的档案码，
//! 空号会被误读成「有人被删档」）。这条由 `failure_isolates_earlier_successes`
//! 用「号段恰好前进成功条数」钉住。
//!
//! # 候选是谁 / 谁被跳过（判定顺序即下表顺序，先命中先报）
//!
//! | 顺序 | 判定 | `reason` | 说明 |
//! |:--:|---|---|---|
//! | 1 | 账号已关联未删除的员工档 | `HAS_EMPLOYEE` | 幂等的基础；报文点名已有工号 |
//! | 2 | `sys_user.user_type <> 'HUMAN'` | `NOT_HUMAN` | 机器人 / 系统账号不该进工资表（与 `bind-user` 同口径） |
//! | 3 | `sys_user.status <> 1` | `USER_DISABLED` | 停用账号多半已离职：建成 `ACTIVE` 员工是**错**的 |
//! | 4 | `real_name` 为空 | `NAME_MISSING` | 姓名是主数据，不许拿登录名冒充（设计 §7.2「机器猜不出来」） |
//! | 5 | `real_name` 超过 50 字 | `NAME_TOO_LONG` | 与 `biz_employee.name VARCHAR(50)` 对齐（单测从 DDL 反解） |
//! | 6 | 账号无部门且未传 `dept_id` | `DEPT_MISSING` | `biz_employee.dept_id NOT NULL`，且数据权限按部门裁剪 |
//!
//! 已在**软删**状态的历史档案不挡路（`e.deleted_at IS NULL` + 唯一索引同样带
//! `WHERE deleted_at IS NULL`）：返聘人员可以重建档案。
//!
//! # `dept_id` 的语义（设计只写了「入参 `dept_id`」，这里定死）
//!
//! - **传了**：既作**筛选**（只处理「本部门 + 无部门」的账号），也作**缺省部门**
//!   （无部门的账号就挂到这个部门下）。别的部门的账号不出现——HR 选「研发部」时
//!   不该把财务部的账号拉进来。
//! - **没传**：处理全部人类账号，员工部门取**账号自己的** `dept_id`；账号没部门的
//!   进 `SKIP/DEPT_MISSING`（报文提示「先在系统用户里设置部门，或传 dept_id 作缺省部门」）。
//!
//! # 数据权限
//!
//! 候选查询接 `scope::filter_for(principal, ScopeTarget::SYS_USER)`
//! （`sys_user` 已在 `SCOPED_TABLES`，`dept_col = dept_id`、`owner_col = id`）：
//! 数据范围不是「全部」的人（`DEPT_LEADER` 之类）只能从自己看得到的账号里生成，
//! 否则会出现「我能建出别人的员工档、建完自己却看不到」。出参只含账号的**基础字段**
//! （`username` / `real_name` / `mobile` / `email` / 部门），没有一个敏感列。
//!
//! # 不做的事
//!
//! - **不猜敏感字段**：`id_card` / `bank_account` / `birth_date` / `gender` 一律留空 /
//!   `0`（未知），由 HR 补齐；猜出来的身份证会撞 `uk_biz_employee_idcard`，
//!   而且「机器填的身份证」是最坏的一类脏数据；
//! - **不改 `sys_user`**：账号不需要任何回写（`biz_employee.user_id` 是唯一连接点）；
//! - **不动既有接口**：`employee_handler.rs` 的 7 条路由口径不变（本文件只是新增一张卡的路由）。

use axum::Json;
use axum::extract::Extension;
use nik::prelude::chrono::{Local, NaiveDate};
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::hr::employee_model::{self, EmployeeWrite};
use crate::business::hr::employee_no;
use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::business::shared::scope;
use crate::shared::error_code;
use crate::utils;

// ==================== 契约常量 ====================

/// 路由路径（不含 `/api`，由 `[router] prefix` 注入；`ROUTE_CONTRACT` 逐字同此值）。
pub const ROUTE_SEED: &str = "/business/hr/employee/seed-from-users";

/// 权限码（设计 §5.2 #10：与「新建员工」共用 `business:hr:employee:create`）。
pub const PERM_SEED: &str = "business:hr:employee:create";

/// 批量生成的员工初始状态 —— 设计 §7.2 明确写 `status='ACTIVE'`：
/// 存量账号对应的都是在职老员工，走 `PROBATION`（新建员工的默认入口）是错的。
pub const STATUS_ACTIVE: &str = "ACTIVE";

/// 用工形式缺省值（`sys_user` 没有这个维度，设计未给，取 `biz_employee` 的列缺省）。
pub const EMPLOYMENT_TYPE_DEFAULT: &str = "FULL_TIME";

/// 证件类型缺省值（同上，取列缺省）。
pub const ID_CARD_TYPE_DEFAULT: &str = "ID_CARD";

/// 姓名列宽（`biz_employee.name VARCHAR(50)`）；单测从 `V0383` 的 DDL 反解对拍。
pub const MAX_NAME: usize = 50;

/// 单次最多处理多少候选。
///
/// 存在理由：这个接口会**逐条开事务**，一次几千个账号会把连接池按在手里几分钟
/// （前端超时 + 其它请求排队）。超限直接 400 让 HR 按部门分批点，
/// 比「转圈五分钟后超时、不知道建了几个」好得多。
pub const MAX_CANDIDATES: usize = 500;

// ==================== 入参 ====================

/// `POST /business/hr/employee/seed-from-users` 的入参（设计 §5.2 #10）。
///
/// 三个字段全可选，且**缺省都取安全值**：
///
/// - `dry_run` 缺省 `true` —— 不传就只看预览。这条是刻意的：这个接口一次能建 500 个档案，
///   「漏传参数就把人事主档写花」比「多看一眼」代价大;
/// - `hire_date` 缺省**今天**（本地时区）—— 存量账号的真实入职日期机器不知道，
///   HR 会自己改；缺省给今天至少不违反 `hire_date NOT NULL`；
/// - `dept_id` 缺省 `None` —— 用账号自己的部门（见模块文档）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmployeeSeedReq {
    /// 目标部门（`sys_dept.id`）：既是筛选范围，也是无部门账号的缺省部门。
    #[serde(default)]
    pub dept_id: Option<String>,
    /// 入职日期（`yyyy-MM-dd`），缺省今天。
    #[serde(default)]
    pub hire_date: Option<String>,
    /// `true` = 只出预览不落库（**缺省 `true`**）。
    #[serde(default)]
    pub dry_run: Option<bool>,
}

impl EmployeeSeedReq {
    /// 是否 dry-run（缺省 `true`）。
    pub fn is_dry_run(&self) -> bool {
        self.dry_run.unwrap_or(true)
    }
}

// ==================== 出参 ====================

/// 一个候选账号的处置结果（`preview[]` 的一行）。
///
/// dry-run 时 `action=CREATE` 表示「将会建」、`emp_no` 为空（预览不占号，见
/// [`EmployeeSeedRes::next_no`]）；真实执行时 `CREATE` 行的 `emp_no` / `employee_id`
/// 是刚落库的值 —— 前端据此把「已建档 EMP-0007」直接显示出来，不用再查一遍。
#[derive(Debug, Clone, Serialize)]
pub struct SeedItem {
    /// 账号 id。
    pub user_id: String,
    /// 登录账号。
    pub username: String,
    /// 账号上的真实姓名（空串 = 账号没填姓名，见 `NAME_MISSING`）。
    pub real_name: String,
    /// 账号部门（可能为空）。
    pub dept_id: Option<String>,
    /// 账号部门名（无部门时为空串）。
    pub dept_name: String,
    /// 本轮对这条候选做了什么。
    pub action: SeedAction,
    /// 跳过 / 失败的原因码（`action=CREATE` 时为空）。
    pub reason: Option<String>,
    /// 人话说明（成功、跳过、失败都有）。
    pub message: String,
    /// 落库后的工号（dry-run 时为 `None`）。
    pub emp_no: Option<String>,
    /// 落库后的员工档案 id（dry-run 时为 `None`）。
    pub employee_id: Option<String>,
    /// 失败时的业务码（与 `ApiError::code` 一致，便于支持排障）。
    pub error_code: Option<i32>,
}

/// 一条候选的处置动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SeedAction {
    /// 建档（dry-run 时 = 将会建档）。
    Create,
    /// 跳过（账号本身不满足建档条件，不是错误）。
    Skip,
    /// 建档失败（库层拒绝：唯一键 / CHECK / 连接），只影响这一条。
    Failed,
}

/// `seed-from-users` 的出参（设计 §5.2 #10：`created, skipped, preview[]`）。
///
/// 比设计多三个字段，都是「不给前端就得再查一次」的：
///
/// - `failed`：设计只写了 `created/skipped`，但**批量建不回滚已成功的**意味着
///   必须有第三个桶装「建失败的」（否则失败的那条既不在 created 也不在 skipped 里，
///   前端只能自己数 `preview[]`）；
/// - `planned_create`：dry-run 下 `created` 恒为 0（这是验收项「只报告不写库」的直接体现），
///   而 HR 要知道「点下去会建几个」，所以单独给一个计划数；
/// - `next_no`：dry-run 时顺手给下一个工号（`peek`，**不耗号**）。批量 N 个不逐个预测 ——
///   `peek` 不推进序号，逐个 `+1` 是假数据。
#[derive(Debug, Clone, Serialize)]
pub struct EmployeeSeedRes {
    /// 本次是否 dry-run（回显，前端据此显示「预览 / 已落库」）。
    pub dry_run: bool,
    /// 实际使用的入职日期（`yyyy-MM-dd`）。
    pub hire_date: String,
    /// 实际使用的目标部门（回显 `dept_id`）。
    pub dept_id: Option<String>,
    /// 扫描到的候选账号数（数据范围 + 部门筛选之后）。
    pub scanned: usize,
    /// 判定为「可建档」的条数（dry-run 与真实执行都给）。
    pub planned_create: usize,
    /// **实际写入**的员工数（dry-run 恒 `0`）。
    pub created: usize,
    /// 跳过的条数（账号不满足建档条件）。
    pub skipped: usize,
    /// 建档失败的条数（库层拒绝，逐条回滚，不影响其它条）。
    pub failed: usize,
    /// 下一个工号（仅 dry-run 且有计划建档时给；`peek` 不耗号）。
    pub next_no: Option<String>,
    /// 逐条处置明细（顺序 = 候选查询顺序：账号创建时间、用户名）。
    pub preview: Vec<SeedItem>,
}

// ==================== 跳过原因 ====================

/// 跳过原因（顺序见模块文档的表；`code` 是对外契约，别改名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedSkip {
    /// 账号已关联未删除的员工档。
    HasEmployee,
    /// 不是人类账号（`user_type <> 'HUMAN'`）。
    NotHuman,
    /// 账号已停用。
    UserDisabled,
    /// 账号没有真实姓名。
    NameMissing,
    /// 姓名超过列宽。
    NameTooLong,
    /// 账号无部门且未指定缺省部门。
    DeptMissing,
}

impl SeedSkip {
    /// 对外原因码（`preview[].reason`，前端按它分流文案；**不要**改成中文）。
    pub const fn code(self) -> &'static str {
        match self {
            Self::HasEmployee => "HAS_EMPLOYEE",
            Self::NotHuman => "NOT_HUMAN",
            Self::UserDisabled => "USER_DISABLED",
            Self::NameMissing => "NAME_MISSING",
            Self::NameTooLong => "NAME_TOO_LONG",
            Self::DeptMissing => "DEPT_MISSING",
        }
    }

    /// 人话说明（带上下文：已有工号 / 账号类型 / 部门怎么补）。
    pub fn message(self, cand: &Candidate, default_dept: Option<Uuid>) -> String {
        match self {
            Self::HasEmployee => format!(
                "该账号已关联员工档 {}（{}）：无需再生成；要换人请走员工档案的「绑定账号」",
                cand.employee_emp_no.as_deref().unwrap_or("<未知工号>"),
                cand.employee_name.as_deref().unwrap_or("姓名缺失")
            ),
            Self::NotHuman => format!(
                "账号 {} 的类型是「{}」：员工档案只给人类账号（机器人 / 系统账号不该进工资表）",
                cand.username, cand.user_type
            ),
            Self::UserDisabled => format!(
                "账号 {} 已停用：停用账号多半已离职，建成在职档案会让工资表把离职的人算进来；\
                 返聘请先启用账号",
                cand.username
            ),
            Self::NameMissing => format!(
                "账号 {} 没有真实姓名：姓名是员工主数据（改名后所有单据显示新名），\
                 不许用登录名冒充 —— 请先在系统用户里补 real_name",
                cand.username
            ),
            Self::NameTooLong => format!(
                "账号 {} 的姓名超过 {MAX_NAME} 个字：超出 biz_employee.name 的列宽，请先修正",
                cand.username
            ),
            Self::DeptMissing => format!(
                "账号 {} 没有部门，且本次未指定 dept_id：员工档案必须挂在真实部门上\
                 （数据权限按部门裁剪）—— 请在系统用户里设置部门，或传入 dept_id 作缺省部门{}",
                cand.username,
                match default_dept {
                    Some(_) => String::new(),
                    None => String::new(),
                }
            ),
        }
    }
}

// ==================== 候选账号 ====================

/// 一个候选账号（`sys_user` + 部门名 + 已有关联档案）。
///
/// 一次查询把「部门名」和「是否已有员工档」都 join 出来（`LEFT JOIN`）：
/// 逐条再查是最容易写出来的 N+1，而候选可能有几百条。
#[derive(Debug, Clone)]
pub struct Candidate {
    /// 账号 id。
    pub user_id: Uuid,
    /// 登录账号。
    pub username: String,
    /// 真实姓名（库里可能为 `NULL`，读出来是空串）。
    pub real_name: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 手机号。
    pub mobile: Option<String>,
    /// 账号部门。
    pub dept_id: Option<Uuid>,
    /// 账号部门名（`dept_id` 为空或部门已删时为空串）。
    pub dept_name: String,
    /// 账号状态（1 启用 / 0 停用）。
    pub user_status: i16,
    /// 账号类型（`HUMAN` / 其它）。
    pub user_type: String,
    /// 已关联的员工档 id（`deleted_at IS NULL`）。
    pub employee_id: Option<Uuid>,
    /// 已关联档案的工号（报文里点名用）。
    pub employee_emp_no: Option<String>,
    /// 已关联档案的姓名（报文里点名用）。
    pub employee_name: Option<String>,
}

impl Candidate {
    /// 判定这条候选的处置结果。
    ///
    /// `Ok(dept_id)` = 可建档，且员工挂在该部门；`Err(reason)` = 跳过。
    /// **判定顺序就是模块文档那张表**（先命中先报）：已有档案 → 非人类账号 →
    /// 账号停用 → 姓名缺失 / 过长 → 部门缺失。
    pub fn classify(&self, default_dept: Option<Uuid>) -> std::result::Result<Uuid, SeedSkip> {
        if self.employee_id.is_some() {
            return Err(SeedSkip::HasEmployee);
        }
        if !self.user_type.eq_ignore_ascii_case("HUMAN") {
            return Err(SeedSkip::NotHuman);
        }
        if self.user_status != 1 {
            return Err(SeedSkip::UserDisabled);
        }
        let name = self.real_name.trim();
        if name.is_empty() {
            return Err(SeedSkip::NameMissing);
        }
        // `VARCHAR(50)` 数的是**字符**（PG 语义），不是字节：中文名 50 字 = 150 字节，
        // 按字节判会把 17 个字的名字错拒。所以用 `chars().count()`。
        if name.chars().count() > MAX_NAME {
            return Err(SeedSkip::NameTooLong);
        }
        self.dept_id.or(default_dept).ok_or(SeedSkip::DeptMissing)
    }
}

/// 由候选账号造一份 [`EmployeeWrite`]（`emp_no` 留空，落库前由取号器回填）。
///
/// 不猜的列（身份证 / 生日 / 性别 / 学历 / 银行卡…）一律 `None` / `0`：
/// 见模块文档「不猜敏感字段」。
pub fn build_write(cand: &Candidate, dept_id: Uuid, hire_date: NaiveDate) -> EmployeeWrite {
    EmployeeWrite {
        // 取号发生在事务里（与 INSERT 同事务，失败不吃号）。
        emp_no: String::new(),
        name: cand.real_name.trim().to_string(),
        // 0 = 未知。`sys_user` 没有性别维度，猜男女是最典型的「顺手瞎填」。
        gender: 0,
        birth_date: None,
        id_card: None,
        id_card_type: ID_CARD_TYPE_DEFAULT.to_string(),
        mobile: cand.mobile.clone().map(|value| value.trim().to_string()),
        email: cand.email.clone().map(|value| value.trim().to_string()),
        dept_id,
        position_code: None,
        job_level: None,
        employment_type: EMPLOYMENT_TYPE_DEFAULT.to_string(),
        hire_date,
        probation_end_date: None,
        education: None,
        school: None,
        major: None,
        graduate_date: None,
        native_place: None,
        address: None,
        emergency_contact: None,
        emergency_phone: None,
        emergency_relation: None,
        bank_name: None,
        bank_branch: None,
        bank_account: None,
        bank_account_name: None,
        // **回写 user_id**：员工档与登录账号的唯一连接点。
        user_id: Some(cand.user_id),
        // 留痕：员工详情页能看出这行是批量生成的，不是 HR 一条条填的。
        remark: Some(format!("由登录账号 {} 批量生成（seed-from-users）", cand.username)),
    }
}

// ==================== 候选查询 ====================

/// 候选查询的公共前缀（部门筛选与数据权限片段由调用方拼在其后）。
///
/// `LEFT JOIN sys_dept` 取部门名（`AGENTS.md` 约定 13：主数据 join 实时取名）；
/// `LEFT JOIN biz_employee ... AND e.deleted_at IS NULL` 一次判「是否已有档案」
/// （软删的历史档案不挡路）。
const CANDIDATE_SQL: &str = "\
SELECT u.id AS user_id, u.username, COALESCE(u.real_name, '') AS real_name, \
       u.email, u.mobile, u.dept_id, COALESCE(d.dept_name, '') AS dept_name, \
       u.status AS user_status, COALESCE(u.user_type, 'HUMAN') AS user_type, \
       e.id AS employee_id, e.emp_no AS employee_emp_no, e.name AS employee_name \
  FROM sys_user u \
  LEFT JOIN sys_dept d ON d.id = u.dept_id AND d.deleted_at IS NULL \
  LEFT JOIN biz_employee e ON e.user_id = u.id AND e.deleted_at IS NULL \
 WHERE u.deleted_at IS NULL";

/// 部门筛选片段：只留「本部门 + 无部门」的账号（无部门的由 `dept_id` 兜底）。
const DEPT_CLAUSE: &str = " AND (u.dept_id = ?::uuid OR u.dept_id IS NULL)";

/// 候选顺序：账号创建时间（先建先建）、再按用户名兜稳定序。
const CANDIDATE_ORDER: &str = " ORDER BY u.created_at, u.username";

/// 读候选账号（部门筛选 + 数据权限）。
///
/// 参数顺序 = SQL 里 `?` 出现的顺序：先是部门（可选），再是 `scope::` 片段的参数。
async fn load_candidates(
    principal: &ScopedPrincipal,
    dept_id: Option<Uuid>,
) -> std::result::Result<Vec<Candidate>, ApiError> {
    // 数据权限：`sys_user` 已在 `SCOPED_TABLES`（`dept_col = dept_id`、`owner_col = id`）。
    let filter = scope::filter_for(principal, scope::ScopeTarget::SYS_USER);
    let mut params: Vec<Value> = Vec::new();
    let dept_sql = match dept_id {
        Some(id) => {
            params.push(Value::from(&id));
            DEPT_CLAUSE
        }
        None => "",
    };
    params.extend(filter.params().iter().cloned());
    let sql = format!("{CANDIDATE_SQL}{dept_sql}{}{CANDIDATE_ORDER}", filter.sql());
    let rows = db()
        .find_with_params(&sql, params)
        .await
        .map_err(|error| utils::db_error(error, "查询待建档的登录账号"))?;
    let mut candidates = Vec::with_capacity(rows.len());
    for row in &rows {
        candidates.push(Candidate {
            user_id: row.get_typed("user_id")?,
            username: row.get_typed("username")?,
            real_name: row.get_typed("real_name")?,
            email: row.get_typed("email")?,
            mobile: row.get_typed("mobile")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            user_status: row.get_typed("user_status")?,
            user_type: row.get_typed("user_type")?,
            employee_id: row.get_typed("employee_id")?,
            employee_emp_no: row.get_typed("employee_emp_no")?,
            employee_name: row.get_typed("employee_name")?,
        });
    }
    Ok(candidates)
}

/// 部门存在性 + 名字（`dept_id` 是入参，必须校验；`None` = 不存在 / 已删）。
async fn dept_name_of(dept_id: Uuid) -> std::result::Result<Option<String>, ApiError> {
    // SCOPE-EXEMPT: 按主键取单个部门做入参校验（不是列表查询，且部门本身另有 sys_dept 的界面闸）。
    let rows = db()
        .find_with_params(
            "SELECT dept_name FROM sys_dept WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&dept_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "校验目标部门"))?;
    Ok(rows.first().map(|row| row.get_typed("dept_name")).transpose()?)
}

// ==================== 接口 ====================

/// 从登录账号批量生成员工档（设计 §5.2 #10）。
#[nik::permission(
    path = "/business/hr/employee/seed-from-users",
    method = "POST",
    permission = "business:hr:employee:create"
)]
#[nik::post("/business/hr/employee/seed-from-users")]
pub async fn employee_seed_from_users(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<EmployeeSeedReq>,
) -> ResResult<EmployeeSeedRes> {
    let operator = utils::current_user_uuid(&claims);
    let principal = principal::resolve_for_claims(&claims).await?;
    Ok(Res::ok(seed_from_users(&principal, operator, &req).await?))
}

/// 批量建档的业务实现（接口与真库用例共用同一条路径，单测只跳过认证）。
///
/// 顺序：**校验入参 → 读候选 → 逐条判定**（到这里都还没有写库）**→ 若非 dry-run
/// 才逐条开事务建档**。dry-run 与真实执行共用同一份判定代码，
/// 所以「预览里说会建的，落库时就是那几个」不会漂移。
pub async fn seed_from_users(
    principal: &ScopedPrincipal,
    operator: Option<Uuid>,
    req: &EmployeeSeedReq,
) -> std::result::Result<EmployeeSeedRes, ApiError> {
    let dry_run = req.is_dry_run();
    let hire_date = parse_hire_date(req.hire_date.as_deref())?;
    let (default_dept, default_dept_name) = match req.dept_id.as_deref().map(str::trim) {
        None | Some("") => (None, None),
        Some(raw) => {
            let dept_id = parse_uuid(raw, "dept_id")?;
            match dept_name_of(dept_id).await? {
                Some(name) => (Some(dept_id), Some(name)),
                None => {
                    return Err(bad_request(format!(
                        "dept_id {dept_id} 不存在或已删除：员工档案必须挂在真实部门上\
                         （数据权限按部门裁剪）"
                    )));
                }
            }
        }
    };
    let default_dept_name = default_dept_name.as_deref();

    let candidates = load_candidates(principal, default_dept).await?;
    if candidates.len() > MAX_CANDIDATES {
        return Err(bad_request(format!(
            "候选账号 {} 个，超过单次上限 {MAX_CANDIDATES} 个：请先传入 dept_id 按部门分批生成\
             （每批逐条建事务，一次几千个会把连接池按几分钟）",
            candidates.len()
        )));
    }
    let scanned = candidates.len();

    let mut preview = Vec::with_capacity(scanned);
    let mut planned_create = 0_usize;
    let mut created = 0_usize;
    let mut skipped = 0_usize;
    let mut failed = 0_usize;

    for cand in &candidates {
        match cand.classify(default_dept) {
            Err(reason) => {
                skipped += 1;
                preview.push(SeedItem {
                    user_id: cand.user_id.to_string(),
                    username: cand.username.clone(),
                    real_name: cand.real_name.trim().to_string(),
                    dept_id: cand.dept_id.map(|id| id.to_string()),
                    dept_name: cand.dept_name.clone(),
                    action: SeedAction::Skip,
                    reason: Some(reason.code().to_string()),
                    message: reason.message(cand, default_dept),
                    emp_no: None,
                    employee_id: None,
                    error_code: None,
                });
            }
            Ok(dept_id) => {
                planned_create += 1;
                if dry_run {
                    preview.push(SeedItem {
                        user_id: cand.user_id.to_string(),
                        username: cand.username.clone(),
                        real_name: cand.real_name.trim().to_string(),
                        dept_id: Some(dept_id.to_string()),
                        dept_name: dept_label(cand, dept_id, default_dept, default_dept_name),
                        action: SeedAction::Create,
                        reason: None,
                        message: format!(
                            "将建档：部门 {}、入职日期 {hire_date}、状态 ACTIVE、工号落库时自动取号（预览不占号）",
                            dept_label(cand, dept_id, default_dept, default_dept_name)
                        ),
                        emp_no: None,
                        employee_id: None,
                        error_code: None,
                    });
                    continue;
                }
                match create_one(operator, cand, dept_id, hire_date).await {
                    Ok((employee_id, emp_no)) => {
                        created += 1;
                        preview.push(SeedItem {
                            user_id: cand.user_id.to_string(),
                            username: cand.username.clone(),
                            real_name: cand.real_name.trim().to_string(),
                            dept_id: Some(dept_id.to_string()),
                            dept_name: dept_label(cand, dept_id, default_dept, default_dept_name),
                            action: SeedAction::Create,
                            reason: None,
                            message: format!(
                                "已建档 {emp_no}（部门 {}、入职日期 {hire_date}、状态 ACTIVE）",
                                dept_label(cand, dept_id, default_dept, default_dept_name)
                            ),
                            emp_no: Some(emp_no),
                            employee_id: Some(employee_id.to_string()),
                            error_code: None,
                        });
                    }
                    Err(error) => {
                        // 逐条失败：**只**回滚这一条（见 create_one），循环继续。
                        //
                        // 报文 = 库层给出的对外文案（唯一键 / CHECK 是**人话**，
                        // 例如「工号 EMP-0007 已被其他员工使用」；其余是「服务器内部错误」
                        // —— 内部细节只在服务端日志，不往响应体里放）**+ 一句怎么往下走**。
                        // 不拼这一句的话，HR 看到「服务器内部错误」只能来问工程师。
                        failed += 1;
                        preview.push(SeedItem {
                            user_id: cand.user_id.to_string(),
                            username: cand.username.clone(),
                            real_name: cand.real_name.trim().to_string(),
                            dept_id: Some(dept_id.to_string()),
                            dept_name: dept_label(cand, dept_id, default_dept, default_dept_name),
                            action: SeedAction::Failed,
                            reason: Some("CREATE_FAILED".to_string()),
                            message: format!(
                                "{}（本条已单独回滚、未占用工号，其它条不受影响：\
                                 修正该账号后重新提交即可，已建档的会自动跳过）",
                                error.msg
                            ),
                            emp_no: None,
                            employee_id: None,
                            error_code: Some(error.code),
                        });
                    }
                }
            }
        }
    }

    // dry-run 的工号提示：`peek` 不推进 `current_no`（HR-14 的口径），失败不影响判定。
    let next_no = if dry_run && planned_create > 0 {
        employee_no::preview_emp_no().await.ok()
    } else {
        None
    };

    Ok(EmployeeSeedRes {
        dry_run,
        hire_date: hire_date.to_string(),
        dept_id: default_dept.map(|id| id.to_string()),
        scanned,
        planned_create,
        created,
        skipped,
        failed,
        next_no,
        preview,
    })
}

/// 一条候选的建档：**自己一个事务**（`BEGIN → 取号 → INSERT → COMMIT`）。
///
/// 任何一步失败 ⇒ `ROLLBACK` ⇒ 号不被白吃、库不留半条；返回的错误带业务码，
/// 由调用方塞进 `preview[]` 的 `Failed` 行（不冒到 HTTP 层）。
async fn create_one(
    operator: Option<Uuid>,
    cand: &Candidate,
    dept_id: Uuid,
    hire_date: NaiveDate,
) -> std::result::Result<(Uuid, String), ApiError> {
    let mut draft = build_write(cand, dept_id, hire_date);
    let id = Uuid::now_v7();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启批量建档事务"))?;
    match employee_no::generate_emp_no(&tx).await {
        Ok(emp_no) => draft.emp_no = emp_no,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    }
    if let Err(error) = employee_model::insert_with_status(&tx, id, &draft, operator, STATUS_ACTIVE, None, None).await {
        let _ = tx.rollback().await;
        return Err(employee_model::write_error(
            error,
            "hr_employee_seed",
            &employee_model::WriteValues::of(&draft),
        ));
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交批量建档事务"))?;
    Ok((id, draft.emp_no))
}

/// 预览行里显示的部门名：账号有部门就用账号的，否则用本次缺省部门的名字。
///
/// 无部门账号被挂到缺省部门时，`cand.dept_name` 是空串（账号本来就没部门）——
/// 直接显示空串会让 HR 看不出这个人会落到哪个部门，所以取调用方校验时读到的部门名。
fn dept_label(cand: &Candidate, dept_id: Uuid, default_dept: Option<Uuid>, default_dept_name: Option<&str>) -> String {
    if cand.dept_id == Some(dept_id) {
        cand.dept_name.clone()
    } else if default_dept == Some(dept_id) {
        default_dept_name.unwrap_or_default().to_string()
    } else {
        cand.dept_name.clone()
    }
}

// ==================== 入参解析 ====================

/// `400 + PARAM_INVALID`（入参格式错，不是业务冲突）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 解析 uuid 入参（错误文案点名参数名）。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| bad_request(format!("{field}（{raw}）不是合法 uuid")))
}

/// 解析 `hire_date`（缺省 = 本地时区的今天）。
///
/// 为什么不是 `Utc::now()`：老大 / 客户都在东八区，UTC 的 `date_naive()` 在
/// 08:00 之前还是「昨天」，会让早上点按钮的 HR 把入职日期建到前一天。
fn parse_hire_date(raw: Option<&str>) -> std::result::Result<NaiveDate, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(Local::now().date_naive()),
        Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| {
            bad_request(format!(
                "hire_date（{text}）不是合法日期：请用 yyyy-MM-dd，如 2026-03-01"
            ))
        }),
    }
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 建表迁移原文（编译期嵌入）：姓名列宽改了就红。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
    }

    fn candidate(username: &str) -> Candidate {
        Candidate {
            user_id: Uuid::now_v7(),
            username: username.to_string(),
            real_name: format!("员工{username}"),
            email: Some(format!("{username}@example.com")),
            mobile: Some("13800000000".to_string()),
            dept_id: Some(Uuid::now_v7()),
            dept_name: "研发部".to_string(),
            user_status: 1,
            user_type: "HUMAN".to_string(),
            employee_id: None,
            employee_emp_no: None,
            employee_name: None,
        }
    }

    /// 契约：本文件的路由与权限码必须与 `ROUTE_CONTRACT` 里登记给 HR-17 的那一行逐字一致。
    #[test]
    fn route_and_permission_match_route_contract() {
        let rows: Vec<_> = crate::business::hr::handler::ROUTE_CONTRACT
            .iter()
            .filter(|spec| spec.card == "HR-17")
            .collect();
        assert_eq!(rows.len(), 1, "ROUTE_CONTRACT 里 HR-17 只应有一行接口");
        assert_eq!(rows[0].method, "POST");
        assert_eq!(rows[0].path, ROUTE_SEED, "路由路径与契约不一致");
        assert_eq!(rows[0].permission, Some(PERM_SEED), "权限码与契约不一致");
    }

    /// 设计 §5.2 #10 的权限码是 `business:hr:employee:create`（与新建共用）。
    #[test]
    fn permission_is_the_create_code() {
        assert_eq!(PERM_SEED, "business:hr:employee:create");
        assert_eq!(ROUTE_SEED, "/business/hr/employee/seed-from-users");
    }

    /// 缺省必须是 dry-run：不传 `dry_run` 的请求**不许**写库。
    #[test]
    fn dry_run_defaults_to_true() {
        let req: EmployeeSeedReq = serde_json::from_str("{}").expect("空对象可反序列化");
        assert!(req.is_dry_run(), "不传 dry_run 时必须按 dry-run 处理");
        assert_eq!(req.dept_id, None);
        assert_eq!(req.hire_date, None);

        let explicit: EmployeeSeedReq = serde_json::from_str(r#"{"dry_run":false}"#).expect("可反序列化");
        assert!(!explicit.is_dry_run(), "显式 false 才允许落库");
    }

    /// `hire_date` 缺省 = 本地时区的今天（不是 UTC 的昨天）。
    #[test]
    fn hire_date_defaults_to_local_today() {
        assert_eq!(parse_hire_date(None).expect("缺省可用"), Local::now().date_naive());
        assert_eq!(
            parse_hire_date(Some("  ")).expect("空白按缺省"),
            Local::now().date_naive()
        );
        assert_eq!(
            parse_hire_date(Some("2026-03-01")).expect("合法日期"),
            day("2026-03-01")
        );
        let err = parse_hire_date(Some("2026/03/01")).expect_err("斜杠不是合法格式");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("yyyy-MM-dd"), "{}", err.msg);
    }

    /// 判定顺序：已有档案 → 非人类 → 停用 → 姓名缺失 → 姓名过长 → 部门缺失。
    #[test]
    fn classify_reports_first_matching_reason() {
        let dept = Uuid::now_v7();

        // ① 已有档案：即使同时「停用 + 无姓名」也只报 HAS_EMPLOYEE（幂等优先）。
        let mut has = candidate("s1");
        has.employee_id = Some(Uuid::now_v7());
        has.employee_emp_no = Some("EMP-0007".to_string());
        has.user_status = 0;
        has.real_name = String::new();
        assert_eq!(has.classify(Some(dept)), Err(SeedSkip::HasEmployee));

        // ② 非人类账号（robot）：停用与否都先报 NOT_HUMAN。
        let mut robot = candidate("s2");
        robot.user_type = "ROBOT".to_string();
        robot.user_status = 0;
        assert_eq!(robot.classify(Some(dept)), Err(SeedSkip::NotHuman));

        // ③ 停用账号：不能建成 ACTIVE 的在职档案。
        let mut disabled = candidate("s3");
        disabled.user_status = 0;
        assert_eq!(disabled.classify(Some(dept)), Err(SeedSkip::UserDisabled));

        // ④ 没姓名：不许拿登录名冒充。
        let mut nameless = candidate("s4");
        nameless.real_name = "   ".to_string();
        assert_eq!(nameless.classify(Some(dept)), Err(SeedSkip::NameMissing));

        // ⑤ 姓名超 50 字（按字符数，不按字节）。
        let mut long = candidate("s5");
        long.real_name = "名".repeat(MAX_NAME + 1);
        assert_eq!(long.classify(Some(dept)), Err(SeedSkip::NameTooLong));
        long.real_name = "名".repeat(MAX_NAME);
        assert_eq!(long.classify(Some(dept)), Ok(long.dept_id.expect("有部门")));

        // ⑥ 无部门且没有缺省部门 → DEPT_MISSING；给了缺省部门就落它。
        let mut homeless = candidate("s6");
        homeless.dept_id = None;
        assert_eq!(homeless.classify(None), Err(SeedSkip::DeptMissing));
        assert_eq!(homeless.classify(Some(dept)), Ok(dept));

        // ⑦ 全部合格 → 用账号自己的部门（不走缺省部门）。
        let ok = candidate("s7");
        let own = ok.dept_id.expect("有部门");
        assert_eq!(ok.classify(Some(dept)), Ok(own), "应优先用账号部门");
    }

    /// 原因码是对外契约：不许改字面量（前端按它分流文案）。
    #[test]
    fn skip_reason_codes_are_stable() {
        let cases = [
            (SeedSkip::HasEmployee, "HAS_EMPLOYEE"),
            (SeedSkip::NotHuman, "NOT_HUMAN"),
            (SeedSkip::UserDisabled, "USER_DISABLED"),
            (SeedSkip::NameMissing, "NAME_MISSING"),
            (SeedSkip::NameTooLong, "NAME_TOO_LONG"),
            (SeedSkip::DeptMissing, "DEPT_MISSING"),
        ];
        let mut seen = std::collections::HashSet::new();
        for (reason, code) in cases {
            assert_eq!(reason.code(), code);
            assert!(seen.insert(code), "原因码不能重复：{code}");
            let text = reason.message(&candidate("u"), None);
            assert!(!text.trim().is_empty(), "{code} 缺文案");
        }
        // 报文里要点名「已有工号」与「怎么补部门」这两条最容易被忽略的信息。
        let mut has = candidate("u1");
        has.employee_id = Some(Uuid::now_v7());
        has.employee_emp_no = Some("EMP-0009".to_string());
        has.employee_name = Some("张三".to_string());
        assert!(SeedSkip::HasEmployee.message(&has, None).contains("EMP-0009"));
        assert!(
            SeedSkip::DeptMissing
                .message(&candidate("u2"), None)
                .contains("dept_id")
        );
    }

    /// 由账号造草稿：该带的带、该空的一律空（不猜敏感字段）。
    #[test]
    fn build_write_copies_account_fields_and_guesses_nothing() {
        let cand = candidate("zhangsan");
        let dept = Uuid::now_v7();
        let draft = build_write(&cand, dept, day("2024-03-01"));

        assert_eq!(draft.emp_no, "", "工号必须留给取号器回填");
        assert_eq!(draft.name, cand.real_name.trim());
        assert_eq!(draft.mobile.as_deref(), cand.mobile.as_deref());
        assert_eq!(draft.email.as_deref(), cand.email.as_deref());
        assert_eq!(draft.dept_id, dept);
        assert_eq!(draft.hire_date, day("2024-03-01"));
        assert_eq!(draft.employment_type, EMPLOYMENT_TYPE_DEFAULT);
        assert_eq!(draft.id_card_type, ID_CARD_TYPE_DEFAULT);
        // **回写 user_id**（验收项）：员工档与登录账号的唯一连接点。
        assert_eq!(draft.user_id, Some(cand.user_id));
        assert!(draft.remark.as_deref().unwrap_or_default().contains(&cand.username));
        // 不猜的列。
        assert_eq!(draft.gender, 0, "性别未知就是 0，不许猜");
        assert_eq!(draft.birth_date, None);
        assert_eq!(draft.id_card, None);
        assert_eq!(draft.bank_account, None);
        assert_eq!(draft.bank_name, None);
        assert_eq!(draft.emergency_contact, None);
        assert_eq!(draft.education, None);
        assert_eq!(draft.position_code, None);
        assert_eq!(draft.job_level, None);
        assert_eq!(draft.probation_end_date, None);
    }

    /// 姓名列宽从建表迁移反解：`VARCHAR(50)` 改了而常量没改 → 红。
    #[test]
    fn name_width_matches_ddl() {
        let block = V0383_DDL
            .split("CREATE TABLE IF NOT EXISTS biz_employee (")
            .nth(1)
            .expect("V0383 里应有 biz_employee 建表语句");
        let block = block.split(");").next().expect("建表语句有结尾");
        let line = block
            .lines()
            .find(|line| line.trim_start().starts_with("name "))
            .or_else(|| block.lines().find(|line| line.contains(" name ")))
            .expect("应有 name 列定义");
        let width = line
            .split("VARCHAR(")
            .nth(1)
            .and_then(|rest| rest.split(')').next())
            .and_then(|digits| digits.trim().parse::<usize>().ok())
            .expect("name 列应为 VARCHAR(n)");
        assert_eq!(width, MAX_NAME, "MAX_NAME 与 biz_employee.name 的列宽不一致");
    }

    /// 出参字段名是对外契约（设计 §5.2 #10 写 `created, skipped, preview[]`）。
    #[test]
    fn response_serializes_the_contract_fields() {
        let res = EmployeeSeedRes {
            dry_run: true,
            hire_date: "2026-03-01".to_string(),
            dept_id: None,
            scanned: 1,
            planned_create: 1,
            created: 0,
            skipped: 0,
            failed: 0,
            next_no: Some(employee_no::EMP_NO_EXAMPLE.to_string()),
            preview: vec![SeedItem {
                user_id: Uuid::now_v7().to_string(),
                username: "u1".to_string(),
                real_name: "员工u1".to_string(),
                dept_id: None,
                dept_name: "研发部".to_string(),
                action: SeedAction::Create,
                reason: None,
                message: "将建档".to_string(),
                emp_no: None,
                employee_id: None,
                error_code: None,
            }],
        };
        let json = serde_json::to_value(&res).expect("可序列化");
        for key in [
            "dry_run",
            "hire_date",
            "dept_id",
            "scanned",
            "planned_create",
            "created",
            "skipped",
            "failed",
            "next_no",
            "preview",
        ] {
            assert!(json.get(key).is_some(), "出参缺字段 {key}");
        }
        let item = &json["preview"][0];
        for key in [
            "user_id",
            "username",
            "real_name",
            "dept_id",
            "dept_name",
            "action",
            "reason",
            "message",
            "emp_no",
            "employee_id",
            "error_code",
        ] {
            assert!(item.get(key).is_some(), "preview 缺字段 {key}");
        }
        assert_eq!(item["action"], "CREATE", "action 必须是 SCREAMING_SNAKE_CASE");
    }

    /// 单次上限存在（防「一次几千个把连接池按几分钟」）。
    ///
    /// 用 `let` 包一层再断言：直接 `assert!(MAX_CANDIDATES >= 100)` 会被
    /// `clippy::assertions_on_constants` 判成「常量断言」（-D warnings 下编译失败）。
    #[test]
    fn batch_limit_is_sane() {
        let limit = MAX_CANDIDATES;
        let lower_ok = limit >= 100;
        let upper_ok = limit <= 2000;
        assert!(lower_ok, "上限太小（{limit}）会让客户天天分批点");
        assert!(upper_ok, "上限太大（{limit}）就不是保护了");
    }
}
