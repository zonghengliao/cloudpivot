//! 数据权限 · SQL 片段生成与「列表查询唯一入口」。
//!
//! 依据 `docs/批次B-技术架构.md` §2.1（五种范围语义与并集规则）、§2.3（注入选型）、
//! §2.4（四道防漏闸）、§2.5（登记表清单）、§2.6（JOIN 注入三条规则）、§2.8（缓存）。
//!
//! # 为什么要这个模块
//!
//! 数据权限最大的风险不是「算错」，而是**漏接**：一处列表忘了注入，用户就多看到一批
//! 不该看的数据，而且**不报错**。所以本模块把「列表分页查询」收敛成**唯一入口**
//! [`paginate`]，它要求调用方传入 [`ScopedPrincipal`]，而 `ScopedPrincipal` 只能由
//! [`crate::business::shared::principal::resolve`] 构造（字段私有）——于是「想查分页
//! 就必须先解析 scope」由类型系统强制，而不是靠 code review 提醒。
//!
//! ```text
//! handler ──► scope::resolve(user_id) ──► ScopedPrincipal
//!                                              │
//!            列表分页 ──► scope::paginate( …, &principal)   ← 唯一入口（注入 + count + 分页）
//!            单条/导出 ─► scope::filter_for(&principal, target) → RowFilter
//! ```
//!
//! # 硬约束（架构 §2.1 / §2.6 / §5.4）
//!
//! 1. **不在 SQL 里内嵌递归 CTE**（S4）。部门子树由 [`crate::business::shared::principal`]
//!    在应用层预解析成 `dept_id` 集合，SQL 里只出现 `= ANY(?)`。POC-4b 实测：内嵌递归
//!    CTE 走 `Seq Scan` 28.0 ms，预解析后 `Bitmap Index Scan` 7.8 ms（3.6×）。
//! 2. **`?` 的顺序与 `params` 顺序一致**（S8/S13）。[`RowFilter`] 把 SQL 片段与参数封在
//!    同一个结构体里，构造后不可拆，[`paginate`] 内部固定把 `RowFilter.params`
//!    **前置**于调用方的 `base_params`（因为 WHERE 里片段的占位符出现在业务过滤之前）。
//! 3. **count 不剥 ORDER BY 的包裹由本模块自己处理**（S12）：`ORDER BY` 必须作为独立
//!    参数传入，count 用「去掉 ORDER BY 的前缀」执行——**禁止**对 SQL 做字符串切割。
//!
//! # 一处与架构文字不同的实现细节（`= ANY(?)` 的绑定形态）
//!
//! 本栈的 PG 驱动**不支持数组参数绑定**（`nik-db-driver-postgres` 的
//! `value_to_pg_param` 对 `Value::Array` 直接返回 `UnsupportedFeature { feature:
//! "array binding" }`），因此 `= ANY(?)` 无法直接绑 `uuid[]`。实际形态是
//! `= ANY(?::text::uuid[])`：应用层把部门集合编码成 PG 文本数组字面量 `{uuid,uuid}`
//! 并以文本参数绑定，`::text::uuid[]` 让占位符的类型推断落在 `text` 上（驱动只支持
//! 标量绑定），再在库侧转成 `uuid[]`。实测证据（B2 探针）：
//! `A(array-literal double cast) -> Some(Ok(12))`、`C(Value::Array) -> unsupported
//! feature array binding`。这不影响索引使用：数组表达式在**每次执行**求值一次，
//! 不是逐行函数包装（架构 §5.5 反模式清单里的「对 dept_id 做函数包装」指的是
//! `dept_id::text = ?` 这类把**列**包进函数的写法，本模块没有那样做）。

use std::collections::BTreeSet;

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::shared::principal::ScopedPrincipal;
use crate::shared::error_code;

// ==================== 注入位置标记 ====================

/// 片段注入位置标记（架构 §2.6 `page_users` 形态里的 `{FILTER}`）。
///
/// 带业务过滤参数的 `base_sql` 必须把它写在「业务条件之前」，[`paginate`] 会原位替换
/// 成数据权限片段；这样 SQL 里 `?` 的顺序（片段在前）才与参数顺序（片段参数前置）一致。
pub const FILTER_MARKER: &str = "{FILTER}";

// ==================== 五种范围 ====================

/// 数据范围（对应 `sys_role.data_scope` 的 `SMALLINT` 1..5）。
///
/// 语义与合并规则见架构 §2.1；本枚举是「库里的整数」与「代码里的类型」之间唯一的桥，
/// 任何从库里读到 `data_scope` 的地方都必须经 [`ScopeKind::from_i16`] 转换，
/// 不允许自己 `match 1 => …`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i16)]
pub enum ScopeKind {
    /// 1 · 全部数据。
    All = 1,
    /// 2 · 本部门（**不含**下级）。
    Dept = 2,
    /// 3 · 本部门及以下。
    DeptAndChild = 3,
    /// 4 · 仅本人。
    SelfOnly = 4,
    /// 5 · 自定义部门集合（`sys_role_dept`）。
    Custom = 5,
}

impl ScopeKind {
    /// 由库中的 `SMALLINT` 转换。
    ///
    /// # 参数
    ///
    /// - `raw`：`sys_role.data_scope` 原值。
    ///
    /// # 返回值
    ///
    /// 1..5 → 对应枚举。
    ///
    /// # 异常情况
    ///
    /// 取值不在 1..5（含 0 / 6 / 负数）→ `400 + DATA_SCOPE_INVALID(61001)`。
    /// 库侧虽有 `ck_sys_role_data_scope` 约束兜底，但缓存反序列化 / 手工改库
    /// 都可能绕过约束，所以代码侧必须再判一次（S1）。
    pub fn from_i16(raw: i16) -> std::result::Result<Self, ApiError> {
        match raw {
            1 => Ok(Self::All),
            2 => Ok(Self::Dept),
            3 => Ok(Self::DeptAndChild),
            4 => Ok(Self::SelfOnly),
            5 => Ok(Self::Custom),
            other => Err(ApiError::new(
                400,
                error_code::DATA_SCOPE_INVALID,
                format!("数据范围取值非法：{other}（应为 1..5）"),
            )),
        }
    }

    /// 转回库中的 `SMALLINT`。
    pub const fn as_i16(self) -> i16 {
        self as i16
    }

    /// 对外稳定编码（日志 / 自检接口 / 前端枚举），与架构 §2.1 的表格一致。
    pub const fn code(self) -> &'static str {
        match self {
            Self::All => "ALL",
            Self::Dept => "DEPT",
            Self::DeptAndChild => "DEPT_AND_CHILD",
            Self::SelfOnly => "SELF",
            Self::Custom => "CUSTOM",
        }
    }

    /// 中文标签（自检接口 / 错误提示直接可用，前端不硬编码）。
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "全部数据",
            Self::Dept => "本部门",
            Self::DeptAndChild => "本部门及以下",
            Self::SelfOnly => "仅本人",
            Self::Custom => "自定义部门",
        }
    }
}

// ==================== 登记表清单（§2.5） ====================

/// 一个「会被注入数据权限」的表在 SQL 里的锚点。
///
/// # 为什么字段全是 `&'static str`
///
/// 别名与列名是**编译期常量**：运行时不存在「用户输入拼进 SQL」的路径，这是
/// 「禁拼 SQL」红线的结构层保证（架构 §2.3 末段），不是靠 code review 提醒。
///
/// # 字段语义
///
/// - `table`：真实表名（启动自检按它查 `information_schema`，也用于 `GET
///   /system/data-scope/tables` 自检接口）；
/// - `alias`：**主体表**在这次查询里的别名（§2.6 规则 1：片段只锚定主体表，
///   绝不打在 JOIN 出来的字典表上，也绝不写进 `LEFT JOIN … ON`）；
/// - `dept_col`：部门列，`None` 表示**该表没有部门列**（如日志表、`sys_file`），
///   此时部门类范围无法表达，[`filter_for`] 会安全侧降级为「仅本人」；
/// - `owner_col`：归属列（SELF 范围用），`None` 表示该表没有归属人（如 `sys_dept`，
///   它的「本人」就是本人所在部门这一行）；
/// - `scoped`：该表**是否参与数据权限注入**。见 [`ScopeTarget::unscoped`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeTarget {
    /// 真实表名。
    pub table: &'static str,
    /// 主体表别名（编译期常量）。
    pub alias: &'static str,
    /// 部门列；`None` = 该表无部门列。
    pub dept_col: Option<&'static str>,
    /// 归属列；`None` = 该表无归属人。
    pub owner_col: Option<&'static str>,
    /// 是否参与数据权限注入（`false` 只能由 [`ScopeTarget::unscoped`] 构造）。
    scoped: bool,
}

impl ScopeTarget {
    /// 构造一个「参与数据权限」的锚点（`const`，可在常量上下文里用）。
    pub const fn new(
        table: &'static str,
        alias: &'static str,
        dept_col: Option<&'static str>,
        owner_col: Option<&'static str>,
    ) -> Self {
        Self {
            table,
            alias,
            dept_col,
            owner_col,
            scoped: true,
        }
    }

    /// 构造一个**不参与**数据权限的锚点（架构 §2.5 判为 ❌ 的全局元数据表）。
    ///
    /// # 为什么需要它
    ///
    /// 闸 ① 要求「分页只有一种写法」（`scope::paginate`），但 §2.5 明确判了一批表
    /// **不该**注入范围：`sys_role` / `sys_menu` 是全局元数据、`sys_config` /
    /// `sys_dict_*` / `sys_job*` / `sys_storage_config|policy` / `sys_notice_template`
    /// 是全局配置。对这些表强行注入 scope 会让「有权限码的人看不到配置」，
    /// 属于**静默少数据**（比漏权限更危险，见架构 §2.4 的取向）。
    ///
    /// 所以把它们显式声明成 `unscoped`：注入形态恒为 `AND 1 = 1`（[`RowFilter::allow_all`]），
    /// 且**不会被登记进** [`SCOPED_TABLES`]（启动自检不校验它们）。这是一个
    /// **写下来的决定**，不是一个疏漏 —— 想给某张表加数据权限，就得改用 [`Self::new`]
    /// 并登记进 [`SCOPED_TABLES`]，那时 `paginate_unscoped` 会直接报错逼你走
    /// [`paginate`]（见 [`paginate_unscoped`] 的守卫）。
    pub const fn unscoped(table: &'static str) -> Self {
        Self {
            table,
            alias: "",
            dept_col: None,
            owner_col: None,
            scoped: false,
        }
    }

    /// 是否参与数据权限注入。
    pub const fn is_scoped(self) -> bool {
        self.scoped
    }

    /// 换一个别名（调用方的 SQL 用了别的别名时用）。
    pub const fn aliased(self, alias: &'static str) -> Self {
        Self { alias, ..self }
    }

    /// `sys_user`：人员列表是数据权限最主要的场景；SELF 的「本人」是 `id` 而不是创建人。
    pub const SYS_USER: Self = Self::new("sys_user", "u", Some("dept_id"), Some("id"));

    /// `sys_dept`：部门树必须按范围裁剪，否则「看不见人但看得见部门」仍是信息泄露。
    pub const SYS_DEPT: Self = Self::new("sys_dept", "d", Some("id"), None);

    /// `sys_import_task`：归属列 `create_by`（架构 §5.5 明确本批要按它注入 SELF 范围）。
    pub const SYS_IMPORT_TASK: Self = Self::new("sys_import_task", "t", None, Some("create_by"));

    /// `sys_export_task`：同上。
    pub const SYS_EXPORT_TASK: Self = Self::new("sys_export_task", "t", None, Some("create_by"));

    /// `operation_logs`：操作人是 `user_id`（**实测列名**，架构 §2.5 注里的
    /// `operate_by` 是笔误，`information_schema` 校对结果见 B2 证据文档）。
    pub const OPERATION_LOGS: Self = Self::new("operation_logs", "l", None, Some("user_id"));

    /// `audit_logs`：操作人是 `user_id`（同上，非 `operator_id`）。
    pub const AUDIT_LOGS: Self = Self::new("audit_logs", "l", None, Some("user_id"));

    /// `auth_logs`：登录日志含 IP / 设备，属个人信息。
    pub const AUTH_LOGS: Self = Self::new("auth_logs", "l", None, Some("user_id"));

    /// `security_logs`：安全日志，同上。
    pub const SECURITY_LOGS: Self = Self::new("security_logs", "l", None, Some("user_id"));

    /// `password_change_logs`：密码变更日志（B5 补登记，§2.5 的 ✅(P1) 行）。
    /// 归属列是 `user_id`（迁移 `V0017`，`information_schema` 已核对）。
    pub const PASSWORD_CHANGE_LOGS: Self = Self::new("password_change_logs", "l", None, Some("user_id"));

    /// `sys_notice`（管理视角 `page_notices`）：发送人是 `sender_id`。
    pub const SYS_NOTICE: Self = Self::new("sys_notice", "n", None, Some("sender_id"));

    /// `sys_file`：上传人是 `uploaded_by`；**没有部门列**，部门形态需要 join
    /// `sys_user`（P1，B5 之后），在此之前部门类范围降级为「仅本人」。
    pub const SYS_FILE: Self = Self::new("sys_file", "f", None, Some("uploaded_by"));

    /// `biz_commission_settlement`（SO-36）：销售员只看自己的结算单。
    ///
    /// 归属列是 `salesperson_id`（这张结算单算在哪个销售员头上），**没有部门列** ——
    /// 部门类范围（本部门 / 本部门及以下 / 自定义）由 [`filter_for`] 安全侧降级为
    /// 「仅本人」。这是佣金的正确语义：提成对个人，不对部门。
    pub const BIZ_COMMISSION_SETTLEMENT: Self =
        Self::new("biz_commission_settlement", "t", None, Some("salesperson_id"));

    /// `biz_cost_object`（成本核算 ARCH-9）：成本对象（生产批）按**车间**裁剪。
    ///
    /// 部门列是 `workshop_dept_id`（生产车间）；归属列给 `None` —— 成本对象没有
    /// 「本人」语义（它不是某个人名下的提成单据，而是一个车间的生产批），
    /// SELF 范围由 [`filter_for`] 安全侧降级为「本人所在部门」。
    ///
    /// 设计 §5.4 点名这张表要走 `scope::ScopeTarget`（`dept_col='workshop_dept_id'`），
    /// **不要**改成 `unscoped`：车间之间互相看得到对方的在产成本，是数据越权。
    pub const BIZ_COST_OBJECT: Self = Self::new("biz_cost_object", "t", Some("workshop_dept_id"), None);

    /// `biz_cost_labor`（成本核算 ARCH-9）：工时台账按**工作中心**裁剪。
    ///
    /// 部门列是 `dept_id`（工作中心；注意工时台账里这一列**不叫** `workshop_dept_id`
    /// —— 那是 `biz_cost_object` 的列名，两张表按同一个部门维度裁剪但列名不同）。
    /// 归属列给 `None`：工时台账记的是「活干了多少」，不是「谁的单据」，没有
    /// 「本人」语义，SELF 范围由 [`filter_for`] 安全侧降级为「本人所在部门」。
    ///
    /// 设计 D-18 点名这张表要登记：A 车间的工时台账不该被 B 车间的统计员看见
    /// —— 它是人工成本的原始凭证，不是参考信息。
    pub const BIZ_COST_LABOR: Self = Self::new("biz_cost_labor", "t", Some("dept_id"), None);

    /// `biz_employee`（人力资源 ARCH-9 D-12）：员工主档按**人事归属部门**裁剪。
    ///
    /// 两个列都给了，与成本类的取舍不同：
    ///
    /// - 部门列 `dept_id` —— 「只能看本部门」的载体（部门/下级/自定义范围都走它）；
    /// - 归属列 `user_id` —— 员工档案是**唯一**有「本人」语义的主数据：`SELF` 范围下
    ///   普通员工只看得到自己那份档案（`employee_model::EMPLOYEE_TARGET` 同一锚点）。
    ///   `user_id` 可空（车间工人无账号 ⇒ `NULL` 不匹配任何 `=` 比较，天然不可见）。
    ///
    /// 别名取 `e`（`employee_model` 的行 SQL 用 `FROM biz_employee e`），
    /// 两处**必须**一致，否则片段会拼出 `t.dept_id` 这种不存在的别名。
    pub const BIZ_EMPLOYEE: Self = Self::new("biz_employee", "e", Some("dept_id"), Some("user_id"));

    /// `biz_leave_request`（请假单，ARCH-9-2 设计 §4.7.3）：`dept_col = dept_id`、`owner_col = NULL`。
    ///
    /// 别名取 `l`（`leave_model::ROW_SQL` 用 `FROM biz_leave_request l`），两处必须一致。
    pub const BIZ_LEAVE_REQUEST: Self = Self::new("biz_leave_request", "l", Some("dept_id"), None);

    /// `biz_attendance_record`（考勤记录，ARCH-9-2 设计 §4.7 的登记表清单）：
    /// `dept_col = dept_id`、`owner_col = NULL`。
    ///
    /// # 为什么归属列是 `NULL`
    ///
    /// 考勤台账是给 **HR / 部门负责人**核对用的（一人一月一行），没有「只看自己那行」的
    /// 列表语义：员工自助看考勤走 `/attendance/my`（HR-41，按 `employee.user_id` 过滤），
    /// 与列表页是两条路径。给一个假的归属列会让 `SELF` 范围的普通员工在台账上
    /// 「看得见自己的行、看不见本部门的行」，而本表的可见性本来就该由
    /// `attendance:record` 权限码决定（设计 §4.7.3 的注释：一期不给普通员工这个码）。
    ///
    /// 别名取 `r`（`attendance_record_model::ROW_SQL` 用 `FROM biz_attendance_record r`），
    /// 两处**必须**一致，否则片段会拼出 `r.dept_id` 之外的别名、直接报列不存在。
    ///
    /// `dept_id` 可空（记录上没有部门时凭证只用空值），`NULL` 不匹配任何 `=` 比较 ⇒
    /// 部门范围的人看不见「无部门」的行 —— 安全侧降级（宁可少看，不可多看）。
    pub const BIZ_ATTENDANCE_RECORD: Self = Self::new("biz_attendance_record", "r", Some("dept_id"), None);
}

/// 登记表清单（架构 §2.5 的 `SCOPED_TABLES`）。
///
/// # 这是「开放清单」
///
/// 新表要接入数据权限，只需：① 在此加一行常量；② 过一个单测（[`tests::scoped_tables_are_sane`]）；
/// ③ 在启动自检（[`crate::business::shared::guard::preflight_permission`]）里被自动校验列存在。
/// 「新增列表接口必须走 [`paginate`]、新表必须登记本清单」已写进
/// `docs/后端二开指南.md` 的检查清单（架构 §5.6 R1 的缓解措施）。
pub const SCOPED_TABLES: &[ScopeTarget] = &[
    ScopeTarget::SYS_USER,
    ScopeTarget::SYS_DEPT,
    ScopeTarget::SYS_IMPORT_TASK,
    ScopeTarget::SYS_EXPORT_TASK,
    ScopeTarget::OPERATION_LOGS,
    ScopeTarget::AUDIT_LOGS,
    ScopeTarget::AUTH_LOGS,
    ScopeTarget::SECURITY_LOGS,
    ScopeTarget::PASSWORD_CHANGE_LOGS,
    ScopeTarget::SYS_NOTICE,
    ScopeTarget::SYS_FILE,
    ScopeTarget::BIZ_COMMISSION_SETTLEMENT,
    ScopeTarget::BIZ_COST_OBJECT,
    ScopeTarget::BIZ_COST_LABOR,
    ScopeTarget::BIZ_EMPLOYEE,
    ScopeTarget::BIZ_LEAVE_REQUEST,
];

// ==================== 片段 ====================

/// 一段可直接追加到 `WHERE` 之后的 SQL 片段**及其绑定参数**。
///
/// # 不变量
///
/// 1. `sql` 以 ` AND ` 开头（调用方的 `base_sql` 必须是「`WHERE …` 已存在」的形态，
///    本项目所有列表查询都满足）；
/// 2. `sql` 里 `?` 的出现顺序与 `params` **逐一对应**；
/// 3. 两个字段私有——片段与参数只能一起产生、一起消费，从结构上排除「两次调用
///    分别取 SQL 与 params 导致错位」的风险（架构 §2.3 选型表「参数错位风险」列）。
#[derive(Debug, Clone)]
pub struct RowFilter {
    /// 追加到 `WHERE` 之后的片段。
    sql: String,
    /// 与片段里 `?` 一一对应的参数。
    params: Vec<Value>,
    /// 该片段实际生效的范围（用于日志 / 自检接口展示）。
    kind: ScopeKind,
    /// 部门集合（供自检 / 日志展示，**不参与** SQL 生成）。
    dept_set: Option<Vec<Uuid>>,
    /// 是否发生了「安全侧降级」（表缺部门列 / 部门集合为空等）。
    degraded: bool,
}

impl RowFilter {
    /// 恒真片段（`AND 1 = 1`）。
    ///
    /// 为什么不直接返回空串：实现上保留一个恒真片段，日志与自检接口才能显示
    /// 「已注入」（架构 §2.1 表头注释）。
    pub fn allow_all() -> Self {
        Self {
            sql: " AND 1 = 1".to_string(),
            params: Vec::new(),
            kind: ScopeKind::All,
            dept_set: None,
            degraded: false,
        }
    }

    /// 恒假片段（`AND 1 = 0`）。
    ///
    /// 只在「本该有范围但无法表达」时使用（例如无部门列的表 + 无法降级的范围）。
    /// 语义是**拒绝全部**而不是放行——安全侧降级（架构 §5.4 S10 的取向）。
    pub fn deny_all() -> Self {
        Self {
            sql: " AND 1 = 0".to_string(),
            params: Vec::new(),
            kind: ScopeKind::SelfOnly,
            dept_set: None,
            degraded: true,
        }
    }

    /// 内部构造：片段 + 参数 + 生效范围。
    fn new(sql: String, params: Vec<Value>, kind: ScopeKind, dept_set: Option<Vec<Uuid>>, degraded: bool) -> Self {
        debug_assert_eq!(
            sql.matches('?').count(),
            params.len(),
            "RowFilter 不变量被破坏：占位符数量与参数数量不一致（S8）"
        );
        Self {
            sql,
            params,
            kind,
            dept_set,
            degraded,
        }
    }

    /// 片段原文（以 ` AND ` 开头）。
    pub fn sql(&self) -> &str {
        &self.sql
    }

    /// 绑定参数（顺序与片段里 `?` 一致）。
    pub fn params(&self) -> &[Value] {
        &self.params
    }

    /// 该片段实际生效的范围。
    pub fn kind(&self) -> ScopeKind {
        self.kind
    }

    /// 可见部门集合（自检 / 日志展示用，不参与 SQL 生成）。
    pub fn dept_set(&self) -> Option<&[Uuid]> {
        self.dept_set.as_deref()
    }

    /// 片段是否「等于不过滤」（`kind == All`）。
    ///
    /// 注意：`deny_all()` 返回 `kind == SelfOnly`，因此本方法对它返回 `false`——
    /// 这也正是「`is_noop()` 为 true 才代表可以跳过注入」的语义。
    pub fn is_noop(&self) -> bool {
        self.kind == ScopeKind::All
    }

    /// 是否发生了安全侧降级（表缺部门列 / 部门集合为空等），自检接口据此提示运维。
    pub fn is_degraded(&self) -> bool {
        self.degraded
    }

    /// 绑定参数个数（自检接口 `binds` 字段，不暴露参数值）。
    pub fn bind_count(&self) -> usize {
        self.params.len()
    }
}

/// 为某个主体表生成数据权限片段（**单条 / 下拉 / 导出取数**用）。
///
/// 列表分页请走 [`paginate`]（唯一入口）；本函数用于不值得分页的读路径，
/// 返回值必须被显式拼进 SQL（Rust 不会因为「没用返回值」报错，所以由 CI 门禁
/// ② 兜底，见架构 §2.4）。
///
/// # 参数
///
/// - `principal`：当前用户的数据范围（由 `resolve` 解析，字段私有）；
/// - `target`：主体表锚点（[`ScopeTarget`] 常量或 `.aliased()` 后的值）。
///
/// # 返回值
///
/// 见 [`RowFilter`]。五种范围的实际形态：
///
/// | 生效范围 | 片段 | 参数 |
/// |---|---|---|
/// | `All` / 超管 | `AND 1 = 1` | 无 |
/// | `Dept` | `AND {a}.{dept} = ?` | `[uuid]` |
/// | `DeptAndChild` / `Custom` | `AND {a}.{dept} = ANY(?::text::uuid[])` | `[text 数组字面量]` |
/// | `SelfOnly` | `AND {a}.{owner} = ?` | `[user_id]` |
/// | 部门范围 ∪ 仅本人 | `AND ({a}.{dept} = ANY(?::text::uuid[]) OR {a}.{owner} = ?)` | `[数组, user_id]` |
///
/// # 安全侧降级（三条，全部 fail-closed）
///
/// 1. 表**没有**部门列（`dept_col == None`）却要部门范围 → 降级为「仅本人」
///    （有 `owner_col` 时）或拒绝全部（也没有 `owner_col` 时）。理由：把
///    「本该只看本部门」静默变成「全部」，是这类注入最危险的失效方式。
/// 2. 部门集合为空却要部门范围 → 同样降级为「仅本人」（`AND {a}.{owner} = ?`），并置
///    `degraded = true` 让自检接口能提示。正常路径下 `resolve` 已把这种情况降级成
///    `SelfOnly`（S10/S11），这里是第二道防线；无归属列时才是 [`RowFilter::deny_all`]。
/// 3. `SelfOnly` 作用在没有归属列的表上（`sys_dept`）→ 用「本人所在部门」这一行
///    （`AND {a}.{dept} = ?`）；连部门都没有 → 拒绝全部。
pub fn filter_for(principal: &ScopedPrincipal, target: ScopeTarget) -> RowFilter {
    // ⓪ 不参与数据权限的表（§2.5 判为 ❌ 的全局元数据/配置，见 `ScopeTarget::unscoped`）：
    //    恒真，且**不看** principal —— 这类表的可见性由权限码控制，不看数据范围。
    if !target.is_scoped() {
        return RowFilter::allow_all();
    }
    // ① 超管短路（S9）：不看库里的 data_scope，直接恒真。
    if principal.is_super() {
        return RowFilter::allow_all();
    }
    let kind = principal.kind();
    if kind == ScopeKind::All {
        return RowFilter::allow_all();
    }

    let dept_ids = principal.dept_ids();
    let self_only = principal.self_only();
    let owner = target.owner_col;
    let dept = target.dept_col;
    let user_id = Value::from(&principal.user_id());
    let alias = target.alias;

    // ② 部门范围（含与 SELF 合并的形态）
    if !dept_ids.is_empty() {
        let Some(dept_col) = dept else {
            // 表无部门列：部门范围无法表达 → 降级为仅本人 / 拒绝全部
            return match owner {
                Some(owner_col) => owner_only_filter(format!(" AND {alias}.{owner_col} = ?"), user_id, kind, true),
                None => RowFilter::deny_all(),
            };
        };
        let dept_set = dept_ids.to_vec();
        if self_only {
            // 部门集合 ∪ 仅本人：固定两段，不生成 OR 树（架构 §2.1）。
            // 无归属列的表用部门集合覆盖「本人」那一行（本人的部门必在集合内）。
            return match owner {
                Some(owner_col) => RowFilter::new(
                    format!(" AND ({alias}.{dept_col} = ANY(?::text::uuid[]) OR {alias}.{owner_col} = ?)"),
                    vec![array_param(&dept_set), user_id],
                    kind,
                    Some(dept_set),
                    false,
                ),
                None => any_filter(alias, dept_col, dept_set, kind),
            };
        }
        if kind == ScopeKind::Dept && dept_set.len() == 1 {
            // 「本部门」保留单值形态（1 个 `?`，与架构 §2.1 表格一致，S3）。
            let only = Value::from(&dept_set[0]);
            return RowFilter::new(
                format!(" AND {alias}.{dept_col} = ?"),
                vec![only],
                kind,
                Some(dept_set),
                false,
            );
        }
        return any_filter(alias, dept_col, dept_set, kind);
    }

    // ③ 仅本人形态（SELF / 已降级的范围）
    match owner {
        Some(owner_col) => owner_only_filter(
            format!(" AND {alias}.{owner_col} = ?"),
            user_id,
            kind,
            // 部门类范围却没拿到部门集合（表无部门列 / 集合为空）→ 标记降级，自检接口要提示
            matches!(kind, ScopeKind::Dept | ScopeKind::DeptAndChild | ScopeKind::Custom),
        ),
        None => match (dept, principal.own_dept_id()) {
            // 无归属人但有部门列（`sys_dept`）：本人的「自己」= 本人所在部门这一行
            (Some(dept_col), Some(own_dept)) => {
                let dept_set = vec![own_dept];
                RowFilter::new(
                    format!(" AND {alias}.{dept_col} = ?"),
                    vec![Value::from(&own_dept)],
                    kind,
                    Some(dept_set),
                    false,
                )
            }
            _ => RowFilter::deny_all(),
        },
    }
}

/// `AND {a}.{col} = ANY(?::text::uuid[])` 形态。
fn any_filter(alias: &str, dept_col: &str, dept_set: Vec<Uuid>, kind: ScopeKind) -> RowFilter {
    RowFilter::new(
        format!(" AND {alias}.{dept_col} = ANY(?::text::uuid[])"),
        vec![array_param(&dept_set)],
        kind,
        Some(dept_set),
        false,
    )
}

/// `AND {a}.{col} = ?` 形态（归属人）。
fn owner_only_filter(sql: String, user_id: Value, kind: ScopeKind, degraded: bool) -> RowFilter {
    // `kind` 保持原范围（自检接口据此显示「你被限制在本部门，但这张表只能按人过滤」）
    RowFilter::new(sql, vec![user_id], kind, None, degraded)
}

/// 把部门集合编码成 PostgreSQL 文本数组字面量 `{uuid,uuid}`。
///
/// 排序后编码：同一集合无论角色顺序如何，生成的参数与 SQL 都逐字节相同
/// （便于日志比对与单测断言）。空集合编码为 `{}`（合法空数组）。
fn array_param(ids: &[Uuid]) -> Value {
    let sorted: BTreeSet<String> = ids.iter().map(|id| id.to_string()).collect();
    let mut literal = String::with_capacity(sorted.len() * 37 + 2);
    literal.push('{');
    for (index, id) in sorted.iter().enumerate() {
        if index > 0 {
            literal.push(',');
        }
        literal.push_str(id);
    }
    literal.push('}');
    Value::Text(literal)
}

// ==================== 分页（唯一入口） ====================

/// 列表分页查询的**唯一入口**：注入数据范围 + 自己算 count + 分页取数。
///
/// # 参数
///
/// - `target`：主体表锚点（片段只作用于它的别名上）；
/// - `base_sql`：**不含** `ORDER BY` / `LIMIT` 的基础查询（`WHERE …` 已存在）。
///   若 `base_sql` 自带业务过滤参数（如 `AND u.username LIKE ?`），**必须**用
///   [`FILTER_MARKER`]（`{FILTER}`）标出注入位置——见下方「注入位置与参数顺序」；
/// - `base_params`：`base_sql` 里 `?` 的参数；
/// - `order_by`：`ORDER BY …` 原文（作为独立参数传入，`None` = 不排序）；
/// - `page` / `page_size`：页码（从 1 开始）与每页条数；
/// - `principal`：当前用户的数据范围。
///
/// # 注入位置与参数顺序（S8 / S13 的契约）
///
/// 片段参数**恒前置**于 `base_params`（架构 §2.6）。要在参数上成立，片段在 SQL 里就必须
/// 出现在业务过滤条件**之前**——所以带业务参数的 `base_sql` 要写成：
///
/// ```sql
/// SELECT u.*, d.dept_name FROM sys_user u
/// LEFT JOIN sys_dept d ON d.id = u.dept_id AND d.deleted_at IS NULL
/// WHERE u.deleted_at IS NULL
///   {FILTER}                      -- ← 片段插在这里
///   AND u.username LIKE ?         -- ← 业务参数在后
/// ```
///
/// 三条规则（`paginate` 按此校验，违反即报错，绝不猜）：
///
/// 1. `base_sql` 含 `{FILTER}` → 原位替换（**必须恰好一次**）；
/// 2. `base_sql` 不含 `{FILTER}` 且 `base_params` 为空 → 片段追加到末尾（等价且安全）；
/// 3. `base_sql` 不含 `{FILTER}` 但 `base_params` 非空 → **报错**。这条最容易被忽略：
///    追加到末尾会让 SQL 里 `?` 的顺序变成「业务参数在前、片段参数在后」，而参数列表是
///    「片段在前」——静默错位会绑定错值（比报错危险得多）。
///
/// # 返回值
///
/// 与框架 `paginate_with_params` 同构的 [`Page<Row>`]。
///
/// # 与框架分页的唯一差异
///
/// count 用**去掉 ORDER BY 的前缀**执行（`SELECT COUNT(*) FROM (base_sql + 片段)`），
/// 数据页才带 `ORDER BY`。POC-5 实测：20 万行时框架的
/// `SELECT COUNT(*) FROM (原 SQL 含 ORDER BY)` 多花一次全排序（780 kB / 6.7 ms，
/// 11.17 ms → 4.49 ms）。因此**不允许**把 `ORDER BY` 留在 `base_sql` 里——
/// 本函数会直接报错而不是猜着切割字符串（切错就是语法错误或静默错序）。
///
/// # 异常情况
///
/// - `base_sql` 里出现 `ORDER BY` / `LIMIT` → `DbError::InvalidValue`；
/// - `{FILTER}` 出现多次 → `DbError::InvalidValue`（无法判断插哪个）；
/// - 带业务参数却没用 `{FILTER}` → `DbError::InvalidValue`；
/// - 查库失败 → 原样上抛 [`DbError`]（handler 直接 `?` 即可）。
pub async fn paginate(
    target: ScopeTarget,
    base_sql: &str,
    base_params: Vec<Value>,
    order_by: Option<&str>,
    page: u64,
    page_size: u64,
    principal: &ScopedPrincipal,
) -> DbResult<Page<Row>> {
    let filter = filter_for(principal, target);
    paginate_with_filter(base_sql, base_params, order_by, page, page_size, &filter).await
}

/// 列表分页的第二入口：**只用于 §2.5 判为 ❌ 的表**（`ScopeTarget::unscoped`）。
///
/// # 为什么必须有它，而不是「让调用方自己拼分页」
///
/// 闸 ① 的硬门禁是「`paginate_with_params` 只能出现在 `scope.rs`」。全局元数据
/// （`sys_role` / `sys_config` / `sys_dict_*` / `sys_job*` / `sys_storage_*` /
/// `sys_notice_template`）**不该**注入数据范围，但它们的列表仍要分页 —— 如果允许
/// 调用方自己调框架分页，闸 ① 就退化成了「谁都可以绕过」，门禁等于没有。
/// 所以给它们一个**显式的**入口：必须传 [`ScopeTarget::unscoped`] 构造的锚点。
///
/// # 守卫（不是靠自觉）
///
/// `target.is_scoped()` 为真 → 直接 `DbError::InvalidValue`。也就是说：某张表一旦
/// 被登记进 [`SCOPED_TABLES`]（获得 `ScopeTarget::new` 构造的常量），就走不了本函数，
/// 只能走 [`paginate`]。反之，未被登记的表明明有部门/归属列却想蒙混过关，也不会
/// 被 `unscoped` 之外的路径放到这里来（`unscoped` 要求调用方在源码里写清表名）。
///
/// # 参数 / 返回值 / 异常
///
/// 与 [`paginate`] 一致（少了 `principal`：不参与数据权限的表不需要解析范围）。
pub async fn paginate_unscoped(
    target: ScopeTarget,
    base_sql: &str,
    base_params: Vec<Value>,
    order_by: Option<&str>,
    page: u64,
    page_size: u64,
) -> DbResult<Page<Row>> {
    if target.is_scoped() {
        return Err(DbError::InvalidValue(format!(
            "{} 参与数据权限（或已登记进 SCOPED_TABLES），必须走 scope::paginate(…, &principal)，\
             不得使用 paginate_unscoped（架构 §2.5 / §2.4 闸①）",
            target.table
        )));
    }
    paginate_with_filter(
        base_sql,
        base_params,
        order_by,
        page,
        page_size,
        &RowFilter::allow_all(),
    )
    .await
}

/// [`paginate`] 与 [`paginate_unscoped`] 的共用实现（count + 数据页）。
async fn paginate_with_filter(
    base_sql: &str,
    base_params: Vec<Value>,
    order_by: Option<&str>,
    page: u64,
    page_size: u64,
    filter: &RowFilter,
) -> DbResult<Page<Row>> {
    if let Some(word) = forbidden_clause(base_sql) {
        return Err(DbError::InvalidValue(format!(
            "scope::paginate 的 base_sql 不得包含 {word}：ORDER BY / LIMIT 必须作为参数单独传入\
             （架构 §2.3：禁止对 SQL 做字符串切割）"
        )));
    }
    if filter.params().is_empty() {
        // 片段无绑定参数（ALL 范围 / 不参与数据权限的表）：追加到末尾不会让 `?` 顺序错位，
        // 所以只校验「标记至多一次」，不强制要求 `{FILTER}`。
        let markers = base_sql.matches(FILTER_MARKER).count();
        if markers > 1 {
            return Err(DbError::InvalidValue(format!(
                "{FILTER_MARKER} 在 base_sql 里出现了 {markers} 次，只允许出现一次（无法判断插哪个）"
            )));
        }
    } else {
        ensure_injection_point(base_sql, base_params.len())?;
    }

    let page = page.max(1);
    let size = page_size.max(1);
    let offset = (page - 1) * size;

    // ① count：剥掉 ORDER BY 的形态（S12）
    let count_sql = build_count_sql(base_sql, filter);
    let total: i64 = match db()
        .scalar_with_params(&count_sql, merge_params(filter, &base_params))
        .await
    {
        Ok(total) => total,
        Err(error) => {
            // 数据权限片段是拼进去的：出错时 SQL 原文是唯一可诊断的线索（不含参数值）。
            tracing::error!(%error, count_sql, "scope::paginate count 查询失败");
            return Err(error);
        }
    };
    let total = u64::try_from(total).unwrap_or(0);

    // ② 数据页：片段 + ORDER BY + LIMIT/OFFSET
    let data_sql = build_data_sql(base_sql, filter, order_by, size, offset);
    let list = match db()
        .find_with_params(&data_sql, merge_params(filter, &base_params))
        .await
    {
        Ok(list) => list,
        Err(error) => {
            tracing::error!(%error, data_sql, "scope::paginate 数据页查询失败");
            return Err(error);
        }
    };

    Ok(Page {
        list,
        total,
        page,
        size,
        pages: Page::<Row>::calc_pages(total, size),
    })
}

/// 校验「片段注入位置」与参数顺序是否自洽（见 [`paginate`] 的三条规则）。
pub(crate) fn ensure_injection_point(base_sql: &str, base_param_count: usize) -> DbResult<()> {
    let markers = base_sql.matches(FILTER_MARKER).count();
    if markers > 1 {
        return Err(DbError::InvalidValue(format!(
            "{FILTER_MARKER} 在 base_sql 里出现了 {markers} 次，只允许出现一次（无法判断插哪个）"
        )));
    }
    if markers == 0 && base_param_count > 0 {
        return Err(DbError::InvalidValue(format!(
            "base_sql 带 {base_param_count} 个业务参数但没有 {FILTER_MARKER} 标记：\
             片段追加到末尾会让 `?` 顺序与参数顺序错位（架构 §2.6 参数顺序契约）"
        )));
    }
    Ok(())
}

/// 把片段拼进 `base_sql`（`{FILTER}` 原位替换；无标记则追加到末尾）。
///
/// # 为什么不直接 `replacen`
///
/// 调用方推送标记的姿势有两种：写在 SQL 字面量里（`… IS NULL {FILTER}`）和
/// `sql.push_str(scope::FILTER_MARKER)`（`… IS NULL{FILTER}`）。后者若直接替换
/// 会拼成 `… IS NULLAND 1 = 1` —— 一条**语法合法但语义错误**的 SQL（PG 会把它
/// 当成 `NULLAND` 标识符报错，实际是 500）。B5 真机验收就在这里踩过一次，
/// 所以替换时对**两侧**各补一个空格并裁掉原空白，任何姿势都得到单空格分隔。
fn inject_filter(base_sql: &str, filter: &RowFilter) -> String {
    let fragment = filter.sql().trim();
    let Some((head, tail)) = base_sql.split_once(FILTER_MARKER) else {
        // 无标记：追加到末尾（片段参数为空时安全，非空时 ensure_injection_point 已拦住）
        let mut sql = String::with_capacity(base_sql.len() + fragment.len() + 1);
        sql.push_str(base_sql.trim_end());
        sql.push(' ');
        sql.push_str(fragment);
        return sql;
    };
    let mut sql = String::with_capacity(base_sql.len() + fragment.len() + 2);
    sql.push_str(head.trim_end());
    sql.push(' ');
    sql.push_str(fragment);
    let tail = tail.trim_start();
    if !tail.is_empty() {
        sql.push(' ');
        sql.push_str(tail);
    }
    sql
}

/// 构造 count 语句：**不含** `ORDER BY`（S12）。
///
/// 形如 `SELECT COUNT(*) FROM (base_sql 已注入片段) AS _scope_count`。
pub(crate) fn build_count_sql(base_sql: &str, filter: &RowFilter) -> String {
    format!(
        "SELECT COUNT(*) FROM ({}) AS _scope_count",
        inject_filter(base_sql, filter).trim_end()
    )
}

/// 构造数据页语句：`base_sql（已注入片段） + ORDER BY + LIMIT/OFFSET`。
///
/// `LIMIT` / `OFFSET` 由本函数用**字面量**内联（与框架 `push_limit_offset` 同取向：
/// 参数化 LIMIT 会让驱动多一次绑定；它们是 `u64` 运算结果，不存在注入面）。
pub(crate) fn build_data_sql(
    base_sql: &str,
    filter: &RowFilter,
    order_by: Option<&str>,
    size: u64,
    offset: u64,
) -> String {
    let injected = inject_filter(base_sql, filter);
    let mut sql = String::with_capacity(injected.len() + 64);
    sql.push_str(injected.trim_end());
    if let Some(order) = order_by.filter(|order| !order.trim().is_empty()) {
        sql.push(' ');
        sql.push_str(order.trim());
    }
    sql.push_str(&format!(" LIMIT {size} OFFSET {offset}"));
    sql
}

/// 参数合并：**片段参数前置**于业务参数（S8/S13 的顺序契约）。
///
/// 原因：片段拼在 `WHERE` 的业务过滤条件**之前**，SQL 里 `?` 的位置自然在前。
pub(crate) fn merge_params(filter: &RowFilter, base_params: &[Value]) -> Vec<Value> {
    let mut params = Vec::with_capacity(filter.params().len() + base_params.len());
    params.extend(filter.params().iter().cloned());
    params.extend(base_params.iter().cloned());
    params
}

/// 检测 `base_sql` 里被禁止出现的子句，返回命中的子句名。
///
/// 只做「整词 + 大小写不敏感」的判断，不解析 SQL（解析永远比误判更危险）。
fn forbidden_clause(base_sql: &str) -> Option<&'static str> {
    let upper = base_sql.to_uppercase();
    if upper.contains("ORDER BY") {
        return Some("ORDER BY");
    }
    // `LIMIT` 作为独立词出现（避免误伤 `limit_value` 这类列名）
    let is_limit = upper
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|word| word == "LIMIT");
    if is_limit {
        return Some("LIMIT");
    }
    None
}

// ==================== 单元测试（S1–S5、S12、S13 + 五范围 SQL 打印） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::shared::principal::ScopedPrincipal;

    /// 固定 UUID（避免随机值让断言不可复现）。
    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn principal(
        kind: ScopeKind,
        dept_ids: Vec<Uuid>,
        self_only: bool,
        own_dept: Option<Uuid>,
        is_super: bool,
    ) -> ScopedPrincipal {
        ScopedPrincipal::for_test(uid(0xC0FFEE), own_dept, is_super, kind, dept_ids, self_only)
    }

    /// S1：`ScopeKind::from_i16` 1..5 映射正确；0 / 6 / -1 → `DATA_SCOPE_INVALID(61001)`。
    #[test]
    fn s1_scope_kind_from_i16() {
        assert_eq!(ScopeKind::from_i16(1).unwrap(), ScopeKind::All);
        assert_eq!(ScopeKind::from_i16(2).unwrap(), ScopeKind::Dept);
        assert_eq!(ScopeKind::from_i16(3).unwrap(), ScopeKind::DeptAndChild);
        assert_eq!(ScopeKind::from_i16(4).unwrap(), ScopeKind::SelfOnly);
        assert_eq!(ScopeKind::from_i16(5).unwrap(), ScopeKind::Custom);
        for bad in [0_i16, 6, -1, i16::MAX] {
            let error = ScopeKind::from_i16(bad).expect_err("非法取值必须报错");
            assert_eq!(error.code, error_code::DATA_SCOPE_INVALID, "取值 {bad}");
            assert_eq!(error.status, 400, "取值 {bad}");
        }
        // 来回转换恒等（缓存里存的就是 i16）
        for raw in 1_i16..=5 {
            assert_eq!(ScopeKind::from_i16(raw).unwrap().as_i16(), raw);
        }
        println!("[S1] 1..5 → All/Dept/DeptAndChild/SelfOnly/Custom；0/6/-1/MAX → 61001 ✓");
    }

    /// S2：`filter_for(All)` → `1 = 1`、参数为空、`is_noop()`。
    #[test]
    fn s2_filter_for_all() {
        let filter = filter_for(
            &principal(ScopeKind::All, vec![], false, None, false),
            ScopeTarget::SYS_USER,
        );
        assert!(filter.sql().contains("1 = 1"), "sql={}", filter.sql());
        assert!(filter.params().is_empty());
        assert!(filter.is_noop());
        assert_eq!(filter.bind_count(), 0);
        println!("[S2] All → `{}` params=0 is_noop=true ✓", filter.sql());
    }

    /// S3：`filter_for(Dept)` → **恰好** 1 个 `?`、`params.len() == 1`。
    #[test]
    fn s3_filter_for_dept() {
        let dept = uid(0xD1);
        let filter = filter_for(
            &principal(ScopeKind::Dept, vec![dept], false, Some(dept), false),
            ScopeTarget::SYS_USER,
        );
        assert_eq!(filter.sql().matches('?').count(), 1, "sql={}", filter.sql());
        assert_eq!(filter.params().len(), 1);
        assert_eq!(filter.sql(), " AND u.dept_id = ?");
        assert!(!filter.is_noop());
        println!("[S3] Dept → `{}` params=1 ✓", filter.sql());
    }

    /// S4（硬门槛）：`filter_for(DeptAndChild)` 是 `= ANY(…)` 形态，**不含** `WITH RECURSIVE`。
    #[test]
    fn s4_filter_for_dept_and_child_has_no_recursive_cte() {
        let dept = uid(0xD2);
        let filter = filter_for(
            &principal(
                ScopeKind::DeptAndChild,
                vec![dept, uid(0xD3), uid(0xD4)],
                false,
                Some(dept),
                false,
            ),
            ScopeTarget::SYS_USER,
        );
        assert!(filter.sql().contains("= ANY("), "sql={}", filter.sql());
        assert!(
            !filter.sql().to_uppercase().contains("WITH RECURSIVE"),
            "禁止在列表 SQL 里内嵌递归 CTE（POC-4b：28.0 ms → 7.8 ms），实际 sql={}",
            filter.sql()
        );
        assert!(!filter.sql().contains("sys_dept"), "SQL 里不应再出现部门表");
        assert_eq!(filter.sql().matches('?').count(), 1);
        assert_eq!(filter.params().len(), 1);
        println!("[S4] DeptAndChild → `{}`（无 WITH RECURSIVE）✓", filter.sql());
    }

    /// S5：`SelfOnly` 在 `sys_user` 用 `id`，在 `sys_import_task` 用 `create_by`。
    #[test]
    fn s5_self_only_uses_target_owner_column() {
        let owner = principal(ScopeKind::SelfOnly, vec![], true, Some(uid(0xD5)), false);
        let user_filter = filter_for(&owner, ScopeTarget::SYS_USER);
        let task_filter = filter_for(&owner, ScopeTarget::SYS_IMPORT_TASK);
        assert_eq!(user_filter.sql(), " AND u.id = ?");
        assert_eq!(task_filter.sql(), " AND t.create_by = ?");
        assert_eq!(user_filter.params().len(), 1);
        assert_eq!(task_filter.params().len(), 1);
        println!(
            "[S5] SelfOnly：sys_user → `{}` / sys_import_task → `{}` ✓",
            user_filter.sql(),
            task_filter.sql()
        );
    }

    /// 附加：别名与「无部门列的表」的降级（架构 §2.6 规则 1 + 本模块降级 ③）。
    #[test]
    fn s5b_alias_and_missing_dept_column_degrade() {
        let dept = uid(0xD6);
        // 换别名后片段跟着换，且只锚定主体表
        let filter = filter_for(
            &principal(ScopeKind::Dept, vec![dept], false, Some(dept), false),
            ScopeTarget::SYS_USER.aliased("usr"),
        );
        assert_eq!(filter.sql(), " AND usr.dept_id = ?");

        // 日志表没有部门列 → 降级为仅本人（fail-closed），并标记 degraded
        let filter = filter_for(
            &principal(ScopeKind::DeptAndChild, vec![dept, uid(0xD7)], false, Some(dept), false),
            ScopeTarget::OPERATION_LOGS,
        );
        assert_eq!(filter.sql(), " AND l.user_id = ?");
        assert!(filter.is_degraded(), "降级必须被标记，自检接口要能提示");
        println!(
            "[S5b] 别名 usr → ` AND usr.dept_id = ?`；无部门列的表降级 → `{}` degraded=true ✓",
            filter.sql()
        );
    }

    /// 附加：`sys_dept` 上的 SELF（无归属列）用「本人所在部门」这一行；没有部门则拒绝全部。
    #[test]
    fn s5c_self_on_dept_table_uses_own_dept() {
        let own = uid(0xD8);
        let filter = filter_for(
            &principal(ScopeKind::SelfOnly, vec![], true, Some(own), false),
            ScopeTarget::SYS_DEPT,
        );
        assert_eq!(filter.sql(), " AND d.id = ?");
        assert_eq!(filter.bind_count(), 1);

        let no_dept = filter_for(
            &principal(ScopeKind::SelfOnly, vec![], true, None, false),
            ScopeTarget::SYS_DEPT,
        );
        assert_eq!(no_dept.sql(), " AND 1 = 0", "无部门时必须拒绝全部而不是放行");
        assert!(no_dept.is_degraded());
        println!(
            "[S5c] sys_dept + SelfOnly → `{}`；无部门 → `{}`（fail-closed）✓",
            filter.sql(),
            no_dept.sql()
        );
    }

    /// S12（硬门槛）：count 语句**不含** `ORDER BY`，数据页**含**。
    #[test]
    fn s12_count_strips_order_by() {
        let dept = uid(0xD9);
        let filter = filter_for(
            &principal(ScopeKind::DeptAndChild, vec![dept], false, Some(dept), false),
            ScopeTarget::SYS_USER,
        );
        let base = "SELECT u.* FROM sys_user u WHERE u.deleted_at IS NULL";
        let order = Some("ORDER BY u.created_at DESC");
        let count_sql = build_count_sql(base, &filter);
        let data_sql = build_data_sql(base, &filter, order, 20, 0);
        assert!(
            !count_sql.to_uppercase().contains("ORDER BY"),
            "count 语句不得含 ORDER BY：{count_sql}"
        );
        assert!(
            data_sql.to_uppercase().contains("ORDER BY"),
            "数据页必须含 ORDER BY：{data_sql}"
        );
        assert!(data_sql.ends_with("LIMIT 20 OFFSET 0"), "data_sql={data_sql}");
        assert!(count_sql.starts_with("SELECT COUNT(*) FROM ("));
        println!("[S12] count → `{count_sql}`");
        println!("[S12] data  → `{data_sql}`");
    }

    /// S13：参数顺序契约——SQL 里第 1 个 `?` 对应 `RowFilter.params[0]`，
    /// 片段参数**全部前置**于业务参数。
    #[test]
    fn s13_param_order_contract() {
        let dept = uid(0xDA);
        let filter = filter_for(
            &principal(
                ScopeKind::Dept,
                vec![dept],
                true, // 部门 ∪ 仅本人 → 2 个占位符
                Some(dept),
                false,
            ),
            ScopeTarget::SYS_USER,
        );
        assert_eq!(filter.sql(), " AND (u.dept_id = ANY(?::text::uuid[]) OR u.id = ?)");
        let base_params = vec![Value::Text("%ali%".to_string()), Value::I16(1)];
        let merged = merge_params(&filter, &base_params);
        assert_eq!(merged.len(), 4, "片段 2 个 + 业务 2 个");
        // 前两个就是片段参数：数组字面量 + user_id
        match &merged[0] {
            Value::Text(literal) => {
                assert!(literal.starts_with('{') && literal.ends_with('}'), "literal={literal}");
                assert_eq!(literal, &format!("{{{dept}}}"), "第 1 个参数必须是部门数组");
            }
            other => panic!("第 1 个参数应为部门数组文本，实际 {other:?}"),
        }
        assert_eq!(merged[1], Value::from(&uid(0xC0FFEE)), "第 2 个参数必须是 user_id");
        assert_eq!(merged[2], base_params[0]);
        assert_eq!(merged[3], base_params[1]);
        // 占位符数量 == 参数数量，且片段的 `?` 在业务参数之前（S8/S13 的顺序契约）
        let base = "SELECT u.* FROM sys_user u WHERE u.deleted_at IS NULL \
                    {FILTER} AND u.username LIKE ? AND u.status = ?";
        let sql = build_data_sql(base, &filter, None, 10, 10);
        assert_eq!(sql.matches('?').count(), merged.len(), "sql={sql}");
        let first_business = sql.find("username LIKE ?").expect("业务条件必须在 SQL 里");
        let filter_at = sql.find("= ANY(").expect("片段必须在 SQL 里");
        assert!(filter_at < first_business, "片段必须出现在业务条件之前：{sql}");
        assert!(!sql.contains(FILTER_MARKER), "标记必须被替换掉：{sql}");
        println!("[S13] sql=`{sql}` params[0]=数组 params[1]=user_id params[2..]=业务参数 ✓");
    }

    /// 注入位置守卫：带业务参数却没用 `{FILTER}` → 报错（防参数静默错位）；
    /// `{FILTER}` 出现多次 → 报错；无参数无标记 → 允许（追加到末尾）。
    #[test]
    fn injection_point_rules_are_enforced() {
        assert!(ensure_injection_point("SELECT 1 FROM t WHERE a = 1 {FILTER}", 2).is_ok());
        assert!(ensure_injection_point("SELECT 1 FROM t WHERE a = 1", 0).is_ok());

        let error = ensure_injection_point("SELECT 1 FROM t WHERE a = ?", 1).expect_err("带业务参数必须用 {FILTER}");
        assert!(matches!(error, DbError::InvalidValue(_)), "实际 {error:?}");

        let error = ensure_injection_point("SELECT 1 FROM t WHERE {FILTER} AND b = ? {FILTER}", 1)
            .expect_err("{FILTER} 只允许一次");
        assert!(matches!(error, DbError::InvalidValue(_)), "实际 {error:?}");
        println!("[守卫] FILTER 标记三条注入规则：有标记→替换；无标记无参数→追加；无标记有参数→报错 ✓");
    }

    /// 硬门槛：五类范围各打印一次生成的 SQL（评审证据，`--nocapture` 可见）。
    #[test]
    fn five_scopes_print_generated_sql() {
        let dept = uid(0xE1);
        let base = "SELECT u.*, d.dept_name FROM sys_user u \
                    LEFT JOIN sys_dept d ON d.id = u.dept_id AND d.deleted_at IS NULL \
                    WHERE u.deleted_at IS NULL";
        let cases = [
            ("① ALL", ScopeKind::All, vec![], false),
            ("② DEPT", ScopeKind::Dept, vec![dept], false),
            (
                "③ DEPT_AND_CHILD",
                ScopeKind::DeptAndChild,
                vec![dept, uid(0xE2), uid(0xE3)],
                false,
            ),
            ("④ SELF", ScopeKind::SelfOnly, vec![], true),
            ("⑤ CUSTOM", ScopeKind::Custom, vec![uid(0xE4), uid(0xE5)], false),
        ];
        println!("\n========== 五种范围生成的 SQL（S2–S8 评审证据） ==========");
        for (name, kind, dept_ids, self_only) in cases {
            let owner = principal(kind, dept_ids, self_only, Some(dept), false);
            let filter = filter_for(&owner, ScopeTarget::SYS_USER);
            let count_sql = build_count_sql(base, &filter);
            let data_sql = build_data_sql(base, &filter, Some("ORDER BY u.created_at DESC"), 20, 0);
            println!("{name}  片段: `{}`", filter.sql());
            println!(
                "      binds={} degraded={} is_noop={}",
                filter.bind_count(),
                filter.is_degraded(),
                filter.is_noop()
            );
            println!("      count: {count_sql}");
            println!("      data : {data_sql}");
            assert!(!data_sql.contains("WITH RECURSIVE"), "{name} 不得内嵌递归 CTE");
        }
        println!("=========================================================\n");
    }

    /// 参数化查询的 `base_sql` 守卫：ORDER BY / LIMIT 留在 base_sql 里必须报错。
    #[test]
    fn paginate_rejects_order_by_or_limit_inside_base_sql() {
        assert_eq!(
            forbidden_clause("SELECT 1 FROM t WHERE a = 1 ORDER BY created_at DESC"),
            Some("ORDER BY")
        );
        assert_eq!(forbidden_clause("SELECT 1 FROM t WHERE a = 1 limit 10"), Some("LIMIT"));
        assert_eq!(forbidden_clause("SELECT limit_value FROM t WHERE a = 1"), None);
        assert_eq!(forbidden_clause("SELECT 1 FROM t WHERE a = 1"), None);
        println!("[守卫] base_sql 里出现 ORDER BY / LIMIT → DbError::InvalidValue ✓");
    }

    /// 空部门集合 + 部门范围 → 降级为「仅本人」（第二道防线，正常路径由 resolve 降级为 SELF）。
    #[test]
    fn empty_dept_set_degrades_to_self() {
        let filter = filter_for(
            &principal(ScopeKind::Custom, vec![], false, Some(uid(0xE6)), false),
            ScopeTarget::SYS_USER,
        );
        assert_eq!(filter.sql(), " AND u.id = ?", "有归属列时降级为仅本人");
        assert!(!filter.is_noop(), "is_noop 为 true 才代表可以跳过注入");
        assert!(filter.is_degraded(), "降级必须被标记");

        // 既无归属列又无部门（`sys_dept` + 用户无部门）→ 拒绝全部
        let denied = filter_for(
            &principal(ScopeKind::Custom, vec![], false, None, false),
            ScopeTarget::SYS_DEPT,
        );
        assert_eq!(denied.sql(), " AND 1 = 0", "无法表达时只能拒绝全部");
        assert!(denied.is_degraded());
        println!(
            "[降级] Custom 空集合 → `{}`（仅本人）/ sys_dept 无部门 → `{}`（拒绝全部）✓",
            filter.sql(),
            denied.sql()
        );
    }

    /// 登记表清单自洽性（闸 3 的前置：清单本身不能自相矛盾）。
    #[test]
    fn scoped_tables_are_sane() {
        let mut tables = std::collections::BTreeSet::new();
        for target in SCOPED_TABLES {
            assert!(tables.insert(target.table), "登记表重复：{}", target.table);
            assert!(!target.alias.is_empty());
            assert!(!target.table.is_empty());
            assert!(
                target.dept_col.is_some() || target.owner_col.is_some(),
                "{} 既无部门列也无归属列，无法注入任何范围",
                target.table
            );
            // 列名/表名必须是纯小写标识符（禁止在编译期常量里夹带 SQL 片段）
            for name in [Some(target.table), target.dept_col, target.owner_col]
                .into_iter()
                .flatten()
            {
                assert!(
                    name.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                    "非法标识符：{name}"
                );
            }
        }
        assert_eq!(tables.len(), SCOPED_TABLES.len());
        println!("[清单] SCOPED_TABLES {} 张表自洽 ✓", SCOPED_TABLES.len());
    }

    /// B5：登记表清单里的每一张都必须是 `scoped = true`（防「用 unscoped 混进登记清单」）。
    #[test]
    fn registered_tables_are_all_scoped() {
        for target in SCOPED_TABLES {
            assert!(
                target.is_scoped(),
                "{} 在 SCOPED_TABLES 里但 is_scoped=false（unscoped 不能登记）",
                target.table
            );
            assert!(!target.alias.is_empty(), "{} 缺少别名", target.table);
        }
        println!("[清单] {} 张登记表全部 scoped=true ✓", SCOPED_TABLES.len());
    }

    /// B5：不参与数据权限的表（§2.5 判为 ❌）→ 五种范围都恒真、不产生绑定参数。
    #[test]
    fn unscoped_target_always_allows() {
        let target = ScopeTarget::unscoped("sys_config");
        assert!(!target.is_scoped());
        for kind in [
            ScopeKind::All,
            ScopeKind::Dept,
            ScopeKind::DeptAndChild,
            ScopeKind::SelfOnly,
            ScopeKind::Custom,
        ] {
            let filter = filter_for(&principal(kind, vec![uid(0xF1)], false, Some(uid(0xF1)), false), target);
            assert_eq!(filter.sql(), " AND 1 = 1", "范围={}", kind.code());
            assert!(filter.params().is_empty(), "范围={}", kind.code());
            assert!(filter.is_noop(), "范围={}", kind.code());
        }
        // 与「登记表 + SELF」形成对照：登记表必须收口
        let scoped = filter_for(
            &principal(ScopeKind::SelfOnly, vec![], true, Some(uid(0xF2)), false),
            ScopeTarget::SYS_FILE,
        );
        assert_eq!(scoped.sql(), " AND f.uploaded_by = ?");
        println!("[unscoped] sys_config → ` AND 1 = 1`（五范围都不注入）；sys_file → 收口 ✓");
    }

    /// B5 守卫：参与数据权限的锚点传给 `paginate_unscoped` → 立即报错（不允许绕过闸①）。
    ///
    /// 不需要数据库：守卫在任何 `db()` 调用之前返回。
    #[tokio::test]
    async fn paginate_unscoped_rejects_scoped_target() {
        let error = super::paginate_unscoped(
            ScopeTarget::SYS_IMPORT_TASK,
            "SELECT t.* FROM sys_import_task t WHERE t.deleted_at IS NULL",
            Vec::new(),
            Some("ORDER BY t.created_at DESC"),
            1,
            10,
        )
        .await
        .expect_err("登记表必须走 scope::paginate");
        match error {
            DbError::InvalidValue(message) => {
                assert!(message.contains("sys_import_task"), "message={message}");
                assert!(message.contains("scope::paginate"), "message={message}");
            }
            other => panic!("期望 DbError::InvalidValue，实际 {other:?}"),
        }
        println!("[守卫] paginate_unscoped 拒绝登记表 ✓");
    }

    /// B5 回归（真机踩过）：`{FILTER}` 两侧的空格必须由 `inject_filter` 自己保证。
    ///
    /// 调用方两种姿势：字面量里写 `… {FILTER}`、或 `push_str(FILTER_MARKER)` 拼成
    /// `…{FILTER}`。后者若直接 `replacen` 会产生 `NULLAND`（500）。
    #[test]
    fn inject_filter_keeps_token_boundaries() {
        let filter = RowFilter::allow_all();

        // 姿势 A：标记前已有空格
        let with_space = "SELECT * FROM t WHERE a = 1 {FILTER} AND b = 2";
        assert_eq!(
            inject_filter(with_space, &filter),
            "SELECT * FROM t WHERE a = 1 AND 1 = 1 AND b = 2"
        );

        // 姿势 B：push_str 紧随 WHERE 条件，**没有**空格（本次真机踩到的形态）
        let no_space = "SELECT * FROM t WHERE a IS NULL{FILTER}";
        let sql = inject_filter(no_space, &filter);
        assert_eq!(sql, "SELECT * FROM t WHERE a IS NULL AND 1 = 1");
        assert!(!sql.contains("NULLAND"), "sql={sql}");

        // 姿势 C：标记在末尾且前后都有空白
        let trailing = "SELECT * FROM t WHERE a = 1 {FILTER}   ";
        assert_eq!(
            inject_filter(trailing, &filter),
            "SELECT * FROM t WHERE a = 1 AND 1 = 1"
        );

        // 无标记 → 追加到末尾（语言无关地补一个空格）
        assert_eq!(
            inject_filter("SELECT * FROM t WHERE a = 1", &filter),
            "SELECT * FROM t WHERE a = 1 AND 1 = 1"
        );
        println!("[注入] 三种标记姿势都得到单空格分隔的 SQL，无 NULLAND 粘连 ✓");
    }
}
