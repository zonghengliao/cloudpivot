//! 员工档案出参契约（**HR-11**）：`*_id` / `*_name` 成对 + 敏感字段只出 `*_masked`。
//!
//! 设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md`
//! —— §5.2 #1 / #2（列表与详情出参字段）、§5.4（详情示例）、
//! §3.1 ①（`biz_employee` 的 `CHECK` 取值域）、§3.2 ⑥（`sys_field_mask` 两条全局规则）、
//! §3.2 ③（4 个 HR 字典）、§6.2（列表 30 天内合同到期提醒），以及 `AGENTS.md` 约定 13
//! （姓名：快照 vs join 实时取名）。
//!
//! # 三条硬约定（本文件的全部存在理由）
//!
//! 1. **凡是 `*_id` 必带 `*_name`** —— 且是**同时有或同时无**：
//!    `position_code: None` ⇒ `position_name: None`；`user_id: Some(..)` ⇒ `username: Some(..)`。
//!    前端拿到「有 id 没 name」只能显示 UUID（2026-11-08 附件表就是这么漏的，见约定 13 反例）。
//! 2. **敏感列永不出明文** —— `id_card` / `bank_account` **不进 DTO**，
//!    只出 [`SensitiveMasked`]（`id_card_masked` / `bank_account_masked`）。
//!    这是比 `sys_field_mask` 更严的口径，见下文「为什么超管也看不到明文」。
//! 3. **名字全部由 handler 一次批量 join 后传进来**（[`EmployeeNames`] / [`ContractInput`] /
//!    [`SalaryInput`]），本文件**不查库**：DTO 层是纯函数，因此能在无库环境逐条断言形状
//!    （这正是本卡验收第 3 条「单测验证 DTO 形状」的可行前提，也天然杜绝 N+1）。
//!
//! # 为什么超管也看不到明文
//!
//! `business::masking` 的 `MaskSet` 对 `SUPER_ADMIN` / 持 `system:field-mask:plain` 的人是
//! **整表豁免**（`is_exempt()` ⇒ `hits()` 恒 `None`）。但设计 D-14 与 §5.4 把员工敏感字段的
//! 明文出口**唯一化**了：`GET /business/hr/employee/{id}/sensitive`
//! （要 `business:hr:employee:sensitive`，且每次访问写 `audit_logs`）。
//!
//! 因此本文件对 `id_card` / `bank_account` **不看豁免**：命中规则就按规则打码，
//! 没命中（含豁免、含规则被删）就按设计 §3.2 ⑥ 的两条全局规则的参数打码
//! （[`DEFAULT_ID_CARD_PARAM`] / [`DEFAULT_BANK_ACCOUNT_PARAM`]，与 `V0388` 逐字一致）。
//! 好处是「看明文」永远留痕，而普通列表/详情连明文都不经过网络。
//!
//! # 与 `business::masking::Maskable` 的分工
//!
//! 既有 `Maskable`（`sys_user` 的 DTO）是**就地脱敏**：DTO 里先放明文，渲染前 `mask()` 替换。
//! 员工档案**不用那条路**——字段名本身就是 `*_masked`，明文从一开始就不进结构体，
//! 「忘了调 `mask()`」这类事故在这里不可能发生（没有可泄漏的字段）。
//!
//! # 不在出参里的三样东西（都是**故意的**）
//!
//! | 列 | 为什么不下发 |
//! |---|---|
//! | `org_id` | 多组织隔离维度，不是用户看得懂的关联；ARCH-9-1 落地前**没有组织主数据表**可 join，下发一个没有名字的 UUID 违反约定 1 |
//! | `create_by` / `update_by` | 只给 [`EmployeeDetailRes::create_by_name`] / `update_by_name`（用户名 join 实时取）；下发裸 UUID 就成了「有 id 没 name」 |
//! | `id_card` / `bank_account` 明文 | 见上文「为什么超管也看不到明文」 |

use nik::prelude::chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::uuid::Uuid;
use serde::Serialize;

use crate::business::hr::employee_model::{self, EmployeeListRow, EmployeeRow};
use crate::business::masking::MaskSet;
use crate::business::masking::masker::{self, MaskType};

// ==================== 常量 ====================

/// 员工档案表名（`sys_field_mask` 的 `table_name`，也用于日志）。
pub const TABLE: &str = employee_model::TABLE;

/// 身份证列名（`sys_field_mask` 的 `column_name`）。
pub const COL_ID_CARD: &str = "id_card";

/// 工资卡号列名（`sys_field_mask` 的 `column_name`）。
pub const COL_BANK_ACCOUNT: &str = "bank_account";

/// 手机号列名：`V0388` **没有**给 `biz_employee.mobile` 种规则，所以默认明文下发（设计 §5.2 #1
/// 列表列就有 `mobile`）；一旦客户在 `sys_field_mask` 里加规则，这里自动改遮 —— 规则生效面
/// 与「谁能看」都由数据决定，不写死在代码里。
pub const COL_MOBILE: &str = "mobile";

/// 身份证缺规则时的打码参数，与 `V0388` 的全局规则 `mask_type=2, mask_param='6,4'` 逐字一致。
///
/// 实测输出 `110101****011234`（`nik-utils::Mask::id_card` 口径是「保留前 N 后 M、中间固定 4 个星号」，
/// 见 §3.2 ⑥ 的落地记录）。
pub const DEFAULT_ID_CARD_PARAM: &str = "6,4";

/// 工资卡缺规则时的打码参数，与 `V0388` 的全局规则 `mask_type=5, mask_param='4,4'` 逐字一致。
pub const DEFAULT_BANK_ACCOUNT_PARAM: &str = "4,4";

/// 部门已被删除（`dept_name` join 不到）时的展示文案。
///
/// 不下发空串：空串在前端是「这个字段没数据」，而这行数据**有**部门（只是名查不到），
/// 两者混在一起会让 HR 以为员工没归属部门。
pub const DEPT_GONE: &str = "（部门已删除）";

/// 账号已被删除（`sys_user` join 不到）时的展示文案。理由同 [`DEPT_GONE`]。
pub const USER_GONE: &str = "（账号已删除）";

/// 被删员工的姓名占位（合同 / 薪资明细里出现「关联对象已删」时用）。
pub const EMPLOYEE_GONE: &str = "（员工已删除）";

/// 业务日时区偏移（秒）：Asia/Shanghai = 固定 `+08:00`（中国无夏令时，不引时区库）。
pub const TZ_OFFSET_SECS: i32 = 8 * 3600;

/// 现行合同「快到期」阈值（天）：设计 §6.2 —— 列表页合同到期 ≤30 天出橙字。
pub const CONTRACT_EXPIRE_WARN_DAYS: i64 = 30;

// ==================== 取值域（与 DDL 的 CHECK 逐字一致） ====================

/// 员工状态的合法取值（与 `ck_biz_employee_status` 逐字一致）。
///
/// 单独列出来是为了让「标签表有没有漏一行」可断言
/// （[`tests::status_labels_match_ddl_check`]），也让前端筛选下拉不必自己抄 DDL。
pub const STATUS_VALUES: &[&str] = &["PROBATION", "ACTIVE", "SUSPENDED", "RESIGNED"];

/// 列表默认要显示的状态（设计 §5.3：不含离职；`include_resigned=true` 才含 `RESIGNED`）。
pub const DEFAULT_STATUSES: &[&str] = &["PROBATION", "ACTIVE", "SUSPENDED"];

/// 员工状态 → 中文（`ck_biz_employee_status` 的 4 个取值）。
///
/// **增删条目必须同步** `ck_biz_employee_status`（单测 [`tests::status_labels_match_ddl_check`]
/// 会与 `employee_model::STATUS_VALUES` 对拍——DDL 改了而这里没跟上，前端就会直接看到
/// `PROBATION` 这种码值）。
pub const STATUS_LABELS: &[(&str, &str)] = &[
    ("PROBATION", "试用期"),
    ("ACTIVE", "在职"),
    ("SUSPENDED", "停职"),
    ("RESIGNED", "离职"),
];

/// 用工形式 → 中文（`ck_biz_employee_emptype` 的 6 个取值）。
pub const EMPLOYMENT_TYPE_LABELS: &[(&str, &str)] = &[
    ("FULL_TIME", "正式"),
    ("PROBATION", "试用"),
    ("INTERN", "实习"),
    ("OUTSOURCE", "外包"),
    ("PART_TIME", "兼职"),
    ("DISPATCH", "劳务派遣"),
];

/// 离职类型 → 中文（`ck_biz_employee_leavetype` 的 5 个取值）。
pub const LEAVE_TYPE_LABELS: &[(&str, &str)] = &[
    ("RESIGN", "辞职"),
    ("DISMISS", "辞退"),
    ("RETIRE", "退休"),
    ("CONTRACT_END", "合同到期"),
    ("DEATH", "身故"),
];

/// 证件类型 → 中文（`ck_biz_employee_idtype` 的 4 个取值）。
pub const ID_CARD_TYPE_LABELS: &[(&str, &str)] = &[
    ("ID_CARD", "身份证"),
    ("PASSPORT", "护照"),
    ("HK_MO_TW", "港澳台居民证件"),
    ("FOREIGN", "外籍证件"),
];

/// 合同类型 → 中文（`ck_biz_emp_contract_type` 的 5 个取值）。
pub const CONTRACT_TYPE_LABELS: &[(&str, &str)] = &[
    ("FIXED_TERM", "固定期限"),
    ("UNFIXED_TERM", "无固定期限"),
    ("INTERN", "实习协议"),
    ("LABOR_SERVICE", "劳务合同"),
    ("DISPATCH", "派遣协议"),
];

/// 合同状态 → 中文（`ck_biz_emp_contract_status` 的 4 个取值）。
pub const CONTRACT_STATUS_LABELS: &[(&str, &str)] = &[
    ("ACTIVE", "生效中"),
    ("EXPIRED", "已到期"),
    ("TERMINATED", "已解除"),
    ("RENEWED", "已续签"),
];

// ==================== 标签 / 日期的纯函数 ====================

/// 在一张「码值 → 中文」表里查标签。
///
/// 查不到时**原样返回码值**（而不是空串或「未知」）：库里的 `CHECK` 已限定取值域，
/// 查不到只可能是「代码没跟上新加的取值」——把码值显示出来，运维一眼就知道要补哪一行；
/// 显示「未知」反而会把这条线索藏起来。
fn label_of(table: &'static [(&'static str, &'static str)], code: &str) -> String {
    let code = code.trim();
    if code.is_empty() {
        return String::new();
    }
    table
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(code))
        .map_or_else(|| code.to_string(), |(_, label)| (*label).to_string())
}

/// 员工状态中文（未知码值原样返回）。
pub fn status_label(code: &str) -> String {
    label_of(STATUS_LABELS, code)
}

/// 用工形式中文。
pub fn employment_type_label(code: &str) -> String {
    label_of(EMPLOYMENT_TYPE_LABELS, code)
}

/// 离职类型中文；`None`（在职）→ `None`。
pub fn leave_type_label(code: Option<&str>) -> Option<String> {
    code.map(|value| label_of(LEAVE_TYPE_LABELS, value))
}

/// 证件类型中文。
pub fn id_card_type_label(code: &str) -> String {
    label_of(ID_CARD_TYPE_LABELS, code)
}

/// 合同类型中文。
pub fn contract_type_label(code: &str) -> String {
    label_of(CONTRACT_TYPE_LABELS, code)
}

/// 合同状态中文。
pub fn contract_status_label(code: &str) -> String {
    label_of(CONTRACT_STATUS_LABELS, code)
}

/// 性别中文（`0` 未知 / `1` 男 / `2` 女；越界值也如实显示，不 panic）。
pub fn gender_label(gender: i16) -> String {
    match gender {
        1 => "男".to_string(),
        2 => "女".to_string(),
        0 => "未知".to_string(),
        other => format!("未知({other})"),
    }
}

/// 业务日时区（固定 `+08:00`）。
fn tz() -> FixedOffset {
    FixedOffset::east_opt(TZ_OFFSET_SECS).expect("+08:00 一定是合法偏移")
}

/// 「今天」的业务日（`Asia/Shanghai`）。
///
/// 列表的合同到期天数、薪资标准的「现行」判定都按业务日算 —— 用 UTC 日期会在北京时间
/// 每天 0:00~8:00 之间错一天（那正是 HR 早上看考勤/合同的时段）。
pub fn today() -> NaiveDate {
    Utc::now().with_timezone(&tz()).date_naive()
}

/// 时间戳 → ISO8601 `+08:00`（设计 §5.1 的硬格式：前端 `new Date()` 可直接解析）。
pub fn timestamp_cst(value: DateTime<Utc>) -> String {
    value
        .with_timezone(&tz())
        .to_rfc3339_opts(nik::prelude::chrono::SecondsFormat::Secs, false)
}

/// 距业务日还有几天：`end - as_of`。
///
/// **负数照给**（已过期）：前端据此显示「已过期 N 天」，比抹成 `0` 或 `None` 更能说明问题。
pub fn expire_in_days(end: NaiveDate, as_of: NaiveDate) -> i64 {
    (end - as_of).num_days()
}

// ==================== 脱敏（本文件唯一的敏感值出口） ====================

/// 按「请求的规则集合 + 设计默认规则」决定 `(类型, 参数)`。
///
/// 优先 `MaskSet::hits`（已经是「角色规则优先于全局规则」合并后的结果）；
/// 没命中（含豁免、含规则被删）→ 用调用方给的默认值。
/// 规则在库里但 `mask_type` 非法（有人直接改库）→ 记 error 并按默认值打码：
/// **绝不因为规则坏了就下发明文**。
fn decide(
    set: &MaskSet,
    column: &str,
    fallback_ty: MaskType,
    fallback_param: &'static str,
) -> (MaskType, Option<String>) {
    match set.hits(TABLE, column) {
        Some(rule) => match rule.ty() {
            Some(ty) => (ty, rule.effective_param().map(str::to_string)),
            None => {
                tracing::error!(
                    table = TABLE,
                    column,
                    mask_type = rule.mask_type,
                    rule_id = %rule.rule_id,
                    "脱敏规则的 mask_type 非法（应 1..7），按设计默认规则打码"
                );
                (fallback_ty, Some(fallback_param.to_string()))
            }
        },
        None => (fallback_ty, Some(fallback_param.to_string())),
    }
}

/// 打码一个值：返回 `None` = 没有值（不要在前端造 `""` 或 `"****"` 的噪音）。
///
/// # 参数
///
/// - `set`：本次请求的规则集合；
/// - `column`：列名（`id_card` / `bank_account`）；
/// - `fallback_ty` / `fallback_param`：规则缺失时的设计默认口径；
/// - `raw`：明文（`None` / 空白 ⇒ `None`）。
///
/// # fail-closed
///
/// [`masker::apply`] 报错（参数被改坏）时返回**全星号**（`MaskSet::fail_closed`）并记 error：
/// 长度信息保留、内容一个字节都不出去。
pub fn masked_value(
    set: &MaskSet,
    column: &str,
    fallback_ty: MaskType,
    fallback_param: &'static str,
    raw: Option<&str>,
) -> Option<String> {
    let raw = raw?;
    if raw.trim().is_empty() {
        return None;
    }
    let (ty, param) = decide(set, column, fallback_ty, fallback_param);
    Some(render(ty, param.as_deref(), raw))
}

/// 按给定类型打码；失败 → 全星号（[`masked_value`] 与单测共用这一个落点）。
fn render(ty: MaskType, param: Option<&str>, raw: &str) -> String {
    match masker::apply(ty, param, raw) {
        Ok(masked) => masked,
        Err(error) => {
            tracing::error!(
                ty = ty.code(),
                param = param.unwrap_or(""),
                error = %error.message(),
                "脱敏失败（参数非法），按 fail-closed 输出全掩码"
            );
            MaskSet::fail_closed(raw)
        }
    }
}

/// 身份证打码（`sys_field_mask` 命中就用规则，否则 `V0388` 的 `6,4` 口径）。
pub fn mask_id_card(set: &MaskSet, raw: Option<&str>) -> Option<String> {
    masked_value(set, COL_ID_CARD, MaskType::IdCard, DEFAULT_ID_CARD_PARAM, raw)
}

/// 工资卡号打码（规则，否则 `V0388` 的 `4,4` 口径）。
pub fn mask_bank_account(set: &MaskSet, raw: Option<&str>) -> Option<String> {
    masked_value(
        set,
        COL_BANK_ACCOUNT,
        MaskType::BankCard,
        DEFAULT_BANK_ACCOUNT_PARAM,
        raw,
    )
}

/// **就地**打码（用于 `mobile` 这类「有规则才遮、没规则就明文」的列，语义同
/// `masking::mask_field`）：命中规则 → 替换；没命中 → 原样；参数坏 → 全星号。
fn mask_in_place(set: &MaskSet, column: &str, raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    if raw.trim().is_empty() {
        return None;
    }
    match set.mask_value(TABLE, column, raw) {
        Ok(Some(masked)) => Some(masked),
        Ok(None) => Some(raw.to_string()),
        Err(error) => {
            tracing::error!(
                table = TABLE,
                column,
                error = %error.message(),
                "脱敏失败（规则非法），按 fail-closed 输出全掩码"
            );
            Some(MaskSet::fail_closed(raw))
        }
    }
}

// ==================== 关联名字载体（handler 批量 join 后填进来） ====================

/// 出参里「关联对象的名字」。
///
/// 全部 `Option`：DTO 层不知道库里的字典/用户/部门有没有这一条，**handler 查到了就填**。
/// 未填时的回退口径见各消费点（字典类回退码值、人员类回退「已删除」）。
///
/// # 为什么不给 `org_name`
///
/// `org_id` 不在出参里（见模块头表格），自然不需要它的名字。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmployeeNames {
    /// 岗位名（字典 `hr_position`，`position_code` 的中文）。
    pub position: Option<String>,
    /// 职级名（字典 `hr_job_level`）。
    pub job_level: Option<String>,
    /// 学历名（字典 `hr_education`）。
    pub education: Option<String>,
    /// 创建人用户名（`sys_user.username` / `real_name` join 实时取）。
    pub create_by: Option<String>,
    /// 最后修改人用户名。
    pub update_by: Option<String>,
}

impl EmployeeNames {
    /// 全部留空（只有一条员工、且什么都没 join 时用）。
    pub fn none() -> Self {
        Self::default()
    }

    /// 依次补齐岗位名。
    pub fn with_position(mut self, name: Option<&str>) -> Self {
        self.position = clean(name);
        self
    }

    /// 依次补齐职级名。
    pub fn with_job_level(mut self, name: Option<&str>) -> Self {
        self.job_level = clean(name);
        self
    }

    /// 依次补齐学历名。
    pub fn with_education(mut self, name: Option<&str>) -> Self {
        self.education = clean(name);
        self
    }

    /// 补齐创建人 / 修改人（一次给两个，因为它们永远一起查）。
    pub fn with_operators(mut self, created_by: Option<&str>, updated_by: Option<&str>) -> Self {
        self.create_by = clean(created_by);
        self.update_by = clean(updated_by);
        self
    }
}

/// 空白 → `None`（join 出来的 `''` 与「没查出人」在出参里应完全一样）。
fn clean(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

/// 「有码值就给名字」：字典 / 主数据类关联的统一口径。
///
/// `code = None` → 两个字段都 `None`（成对为空的唯一合法方式）；
/// `code = Some` 而名字没 join 到 → **回退码值**（宁可在界面上看到 `SENIOR`，
/// 也不要看到空白格；空白格会让 HR 以为学历没填）。
fn pair_of(code: &Option<String>, name: &Option<String>) -> Option<String> {
    code.as_ref()
        .map(|_| name.clone().unwrap_or_else(|| code.clone().unwrap_or_default()))
}

// ==================== 列表行出参 ====================

/// 员工列表行（`GET /business/hr/employee/page` 的一项，设计 §5.2 #1）。
///
/// 只含列表真正显示的列：`id_card` / `bank_account` / 紧急联系人 / 银行信息这些**根本不取**
/// （`employee_model::EmployeeListRow` 就没 select），列表一次几十行，少下发一列就少一份泄漏面。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmployeeListRes {
    /// 主键（`Uuid` → `String`：前端不需要知道它是 UUID，但也别让它变成 `null`）。
    pub id: String,
    /// 工号（`EMP-0001`）。
    pub emp_no: String,
    /// 姓名（主数据：改名后所有单据显示新名）。
    pub name: String,
    /// 性别码值（0 / 1 / 2）。
    pub gender: i16,
    /// 性别中文（`未知` / `男` / `女`）。
    pub gender_label: String,
    /// 手机号（`sys_field_mask` 有 `biz_employee.mobile` 规则时才遮，见 [`COL_MOBILE`]）。
    pub mobile: Option<String>,
    /// 人事归属部门 id。
    pub dept_id: String,
    /// 部门名（join 实时取；部门已删 → [`DEPT_GONE`]）。
    pub dept_name: String,
    /// 岗位码值（字典 `hr_position`）。
    pub position_code: Option<String>,
    /// 岗位名（与 `position_code` **成对**：码值没有则 `None`）。
    pub position_name: Option<String>,
    /// 职级码值（字典 `hr_job_level`）。
    pub job_level: Option<String>,
    /// 职级名（与 `job_level` 成对）。
    pub job_level_name: Option<String>,
    /// 用工形式码值。
    pub employment_type: String,
    /// 用工形式中文。
    pub employment_type_label: String,
    /// 入职日期。
    pub hire_date: NaiveDate,
    /// 员工状态码值。
    pub status: String,
    /// 员工状态中文。
    pub status_label: String,
    /// 关联登录账号 id（车间工人没有账号 → `None`）。
    pub user_id: Option<String>,
    /// 账号用户名（与 `user_id` 成对；账号已删 → [`USER_GONE`]）。
    pub username: Option<String>,
    /// 现行合同到期日（`status='ACTIVE'` 里最晚的一份；无合同 → `None`）。
    pub contract_end_date: Option<NaiveDate>,
    /// 距合同到期还有几天（负数 = 已过期；`≤` [`CONTRACT_EXPIRE_WARN_DAYS`] 时前端出橙字）。
    ///
    /// 服务端算而不是前端算：业务日按 `+08:00`，前端用本机时钟会在跨时区/改系统时间时飘。
    pub contract_expire_in_days: Option<i64>,
}

impl EmployeeListRes {
    /// 由模型行 + 关联名字构造。
    ///
    /// # 参数
    ///
    /// - `row`：`employee_model::page` 的行；
    /// - `names`：岗位 / 职级名（列表**只需要这两个**，学历不上列表）；
    /// - `mask`：本次请求的脱敏规则集合（只影响 `mobile`）；
    /// - `as_of`：业务日（[`today`]；单测传固定日期，避免「跑测试那天刚好到期」的假红）。
    pub fn from_row(row: &EmployeeListRow, names: &EmployeeNames, mask: &MaskSet, as_of: NaiveDate) -> Self {
        Self {
            id: row.id.to_string(),
            emp_no: row.emp_no.clone(),
            name: row.name.clone(),
            gender: row.gender,
            gender_label: gender_label(row.gender),
            mobile: mask_in_place(mask, COL_MOBILE, row.mobile.as_deref()),
            dept_id: row.dept_id.to_string(),
            dept_name: dept_name_of(&row.dept_name),
            position_name: pair_of(&row.position_code, &names.position),
            position_code: row.position_code.clone(),
            job_level_name: pair_of(&row.job_level, &names.job_level),
            job_level: row.job_level.clone(),
            employment_type_label: employment_type_label(&row.employment_type),
            employment_type: row.employment_type.clone(),
            hire_date: row.hire_date,
            status_label: status_label(&row.status),
            status: row.status.clone(),
            username: username_of(row.user_id, &row.username),
            user_id: row.user_id.map(|id| id.to_string()),
            contract_end_date: row.contract_end_date,
            contract_expire_in_days: row.contract_end_date.map(|end| expire_in_days(end, as_of)),
        }
    }

    /// 是否需要提醒「合同快到期」（设计 §6.2：`≤30` 天，含已过期）。
    ///
    /// 放在服务端是为了让前端和导出/巡检用**同一套判据**：
    /// `expire_in_days ≤ 30`（含负数）—— 只判 `0 < x ≤ 30` 会漏掉已过期的人。
    pub fn contract_urgent(&self) -> bool {
        self.contract_expire_in_days
            .is_some_and(|days| days <= CONTRACT_EXPIRE_WARN_DAYS)
    }
}

/// `dept_name` 为空（join 不到）时的展示回退。
fn dept_name_of(raw: &str) -> String {
    let text = raw.trim();
    if text.is_empty() {
        DEPT_GONE.to_string()
    } else {
        text.to_string()
    }
}

/// `user_id` 与 `username` 的成对生成：
///
/// - 没有账号（`user_id = None`）→ 两个都 `None`；
/// - 有账号且 join 到了 → `(Some(id), Some(name))`；
/// - 有账号但 join 不到（账号被删 / 被停用删行）→ `(Some(id), Some("（账号已删除）"))`
///   —— 宁可显示占位，也不要出现「有 id 没 name」。
fn username_of(user_id: Option<Uuid>, username: &str) -> Option<String> {
    user_id.map(|_| {
        let text = username.trim();
        if text.is_empty() {
            USER_GONE.to_string()
        } else {
            text.to_string()
        }
    })
}

// ==================== 合同 / 薪资标准（详情里的子表） ====================

/// 劳动合同的**行投影**（`biz_employee_contract` 的出参前置）。
///
/// 为什么 DTO 自己定输入结构：本卡（HR-11）先定契约，合同的 model 由 HR-15 落地 ——
/// 只要 HR-15 的查询按这 12 列投影，就能直接 `map(EmployeeContractRes::from_input)`，
/// 不需要改本文件。`renewed_from_no` 由 SQL 自连接取（续签链的上一份合同号），
/// 不允许 DTO 层再查一次库（N+1）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContractInput {
    /// 合同主键。
    pub id: Uuid,
    /// 合同编号（与纸质一致，人工填）。
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
    /// 合同状态码值。
    pub status: String,
    /// 解除日（`status='TERMINATED'` 时必有）。
    pub terminated_date: Option<NaiveDate>,
    /// 解除原因。
    pub terminate_reason: Option<String>,
    /// 续签自哪份合同（首份合同为 `None`）。
    pub renewed_from_id: Option<Uuid>,
    /// 续签自那份合同的**编号**（`renewed_from_id` 的配对名，SQL 自连接取）。
    pub renewed_from_no: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 一份劳动合同的出参。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmployeeContractRes {
    /// 合同主键。
    pub id: String,
    /// 合同编号。
    pub contract_no: String,
    /// 合同类型码值。
    pub contract_type: String,
    /// 合同类型中文。
    pub contract_type_label: String,
    /// 签订日。
    pub sign_date: NaiveDate,
    /// 起始日。
    pub start_date: NaiveDate,
    /// 终止日（无固定期限为 `None`）。
    pub end_date: Option<NaiveDate>,
    /// 试用期（月）。
    pub probation_months: i16,
    /// 合同状态码值。
    pub status: String,
    /// 合同状态中文。
    pub status_label: String,
    /// 解除日。
    pub terminated_date: Option<NaiveDate>,
    /// 解除原因。
    pub terminate_reason: Option<String>,
    /// 续签来源合同 id（首份为 `None`）。
    pub renewed_from_id: Option<String>,
    /// 续签来源合同**编号**（与 `renewed_from_id` 成对）。
    pub renewed_from_no: Option<String>,
    /// 距到期天数（仅 `ACTIVE` 且有终止日时给；负数 = 已过期但状态还没被批处理刷成 `EXPIRED`）。
    pub expire_in_days: Option<i64>,
    /// 备注。
    pub remark: Option<String>,
}

impl EmployeeContractRes {
    /// 由行投影构造。
    ///
    /// # 参数
    ///
    /// - `input`：合同行（[`ContractInput`]）；
    /// - `as_of`：业务日（算 [`Self::expire_in_days`]）。
    pub fn from_input(input: ContractInput, as_of: NaiveDate) -> Self {
        let expire = if input.status == "ACTIVE" {
            input.end_date.map(|end| expire_in_days(end, as_of))
        } else {
            None
        };
        Self {
            contract_type_label: contract_type_label(&input.contract_type),
            contract_type: input.contract_type,
            status_label: contract_status_label(&input.status),
            status: input.status,
            id: input.id.to_string(),
            contract_no: input.contract_no,
            sign_date: input.sign_date,
            start_date: input.start_date,
            end_date: input.end_date,
            probation_months: input.probation_months,
            terminated_date: input.terminated_date,
            terminate_reason: input.terminate_reason,
            renewed_from_no: input
                .renewed_from_id
                .map(|_| clean(input.renewed_from_no.as_deref()).unwrap_or_else(|| "（原合同已删除）".to_string())),
            renewed_from_id: input.renewed_from_id.map(|id| id.to_string()),
            expire_in_days: expire,
            remark: clean(input.remark.as_deref()),
        }
    }

    /// 是否现行合同（`status = ACTIVE`）。
    pub fn is_active(&self) -> bool {
        self.status == "ACTIVE"
    }
}

/// 员工薪资标准的**行投影**（`biz_employee_salary`；薪资项名由 SQL join `biz_salary_item`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalaryInput {
    /// 薪资项编码（弱引用 `biz_salary_item.item_code`）。
    pub item_code: String,
    /// 薪资项名（join 不到时由 DTO 回退码值）。
    pub item_name: Option<String>,
    /// 月标准金额。
    pub amount: Decimal,
    /// 生效起（含）。
    pub effective_from: NaiveDate,
    /// 生效止（含），`None` = 至今。
    pub effective_to: Option<NaiveDate>,
    /// 备注。
    pub remark: Option<String>,
}

/// 一条员工薪资标准的出参。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmployeeSalaryRes {
    /// 薪资项编码。
    pub item_code: String,
    /// 薪资项名（与 `item_code` 成对：编码必有，所以名字必给——join 不到就回退编码）。
    pub item_name: String,
    /// 月标准金额（`Decimal` → JSON 字符串，定点 4 位，前端不做浮点）。
    pub amount: Decimal,
    /// 生效起（含）。
    pub effective_from: NaiveDate,
    /// 生效止（含）。
    pub effective_to: Option<NaiveDate>,
    /// 业务日是否落在生效区间内（前端「现行标准」高亮；区间可能为空 = 未来才生效）。
    pub current: bool,
    /// 备注。
    pub remark: Option<String>,
}

impl EmployeeSalaryRes {
    /// 由行投影构造。
    pub fn from_input(input: SalaryInput, as_of: NaiveDate) -> Self {
        let current = input.effective_from <= as_of && input.effective_to.is_none_or(|to| to >= as_of);
        Self {
            item_name: clean(input.item_name.as_deref()).unwrap_or_else(|| input.item_code.clone()),
            item_code: input.item_code,
            amount: input.amount,
            effective_from: input.effective_from,
            effective_to: input.effective_to,
            current,
            remark: clean(input.remark.as_deref()),
        }
    }
}

// ==================== 敏感字段（只出 masked） ====================

/// 敏感字段的脱敏值（设计 §5.2 #2 的 `sensitive_masked`）。
///
/// `None` = 该字段没登记（不是「遮掉了」）—— 前端显示「未登记」，
/// 与「有值但被打码」在界面上一眼可分。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SensitiveMasked {
    /// 身份证号（打码形态，如 `110101****011234`）。
    pub id_card_masked: Option<String>,
    /// 工资卡号（打码形态，如 `6222 **** 1234`）。
    pub bank_account_masked: Option<String>,
}

impl SensitiveMasked {
    /// 从明文行构造：本结构是**唯一**能拿到敏感值的出参字段，明文到此为止。
    pub fn from_plain(id_card: Option<&str>, bank_account: Option<&str>, set: &MaskSet) -> Self {
        Self {
            id_card_masked: mask_id_card(set, id_card),
            bank_account_masked: mask_bank_account(set, bank_account),
        }
    }

    /// 是否有任何一项需要「看明文」入口（前端据此决定显不显示「查看明文」按钮）。
    pub fn has_any(&self) -> bool {
        self.id_card_masked.is_some() || self.bank_account_masked.is_some()
    }
}

// ==================== 详情出参 ====================

/// 员工详情（`GET /business/hr/employee/{id}`，设计 §5.2 #2 / §5.4）。
///
/// # 与设计 §5.4 的一处口径统一
///
/// §5.4 的示例把 `id_card_masked` / `bank_account_masked` 平铺在顶层，而 §5.2 #2 的字段表
/// 写的是 `sensitive_masked`（`id_card_masked` / `bank_account_masked`）。**本文件按 §5.2**
/// （接口清单是契约，示例是示意）：JSON 形状是
/// `"sensitive_masked": {"id_card_masked": …, "bank_account_masked": …}`。
/// 单测 [`tests::detail_json_shape_is_pinned`] 把这条钉住，改形状会红。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmployeeDetailRes {
    /// 主键。
    pub id: String,
    /// 工号。
    pub emp_no: String,
    /// 姓名。
    pub name: String,
    /// 性别码值。
    pub gender: i16,
    /// 性别中文。
    pub gender_label: String,
    /// 出生日期。
    pub birth_date: Option<NaiveDate>,
    /// 证件类型码值。
    pub id_card_type: String,
    /// 证件类型中文。
    pub id_card_type_label: String,
    /// 手机号（有 `sys_field_mask` 规则才遮，见 [`COL_MOBILE`]）。
    pub mobile: Option<String>,
    /// 邮箱。
    pub email: Option<String>,
    /// 部门 id。
    pub dept_id: String,
    /// 部门名（join 实时取；已删 → [`DEPT_GONE`]）。
    pub dept_name: String,
    /// 岗位码值。
    pub position_code: Option<String>,
    /// 岗位名（与 `position_code` 成对）。
    pub position_name: Option<String>,
    /// 职级码值。
    pub job_level: Option<String>,
    /// 职级名（与 `job_level` 成对）。
    pub job_level_name: Option<String>,
    /// 用工形式码值。
    pub employment_type: String,
    /// 用工形式中文。
    pub employment_type_label: String,
    /// 入职日期。
    pub hire_date: NaiveDate,
    /// 试用期截止。
    pub probation_end_date: Option<NaiveDate>,
    /// 离职日期（`status=RESIGNED` 时必有）。
    pub leave_date: Option<NaiveDate>,
    /// 离职类型码值。
    pub leave_type: Option<String>,
    /// 离职类型中文（与 `leave_type` 成对）。
    pub leave_type_label: Option<String>,
    /// 员工状态码值。
    pub status: String,
    /// 员工状态中文。
    pub status_label: String,
    /// 学历码值。
    pub education: Option<String>,
    /// 学历名（字典 `hr_education`；与 `education` 成对）。
    pub education_name: Option<String>,
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
    pub bank_branch: Option<String>,
    /// 工资卡户名。
    pub bank_account_name: Option<String>,
    /// 登录账号 id（成对给 `username`）。
    pub user_id: Option<String>,
    /// 登录账号用户名。
    pub username: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间（ISO8601 `+08:00`）。
    pub created_at: String,
    /// 最后修改时间（ISO8601 `+08:00`）。
    pub updated_at: String,
    /// 创建人（用户名；join 不到 → `None`）。
    pub create_by_name: Option<String>,
    /// 最后修改人（用户名）。
    pub update_by_name: Option<String>,
    /// 敏感字段的脱敏值（明文唯一出口是 `/employee/{id}/sensitive`）。
    pub sensitive_masked: SensitiveMasked,
    /// 全部劳动合同（按 `start_date DESC`，由 HR-15 的 SQL 排序）。
    pub contracts: Vec<EmployeeContractRes>,
    /// 现行合同（`contracts` 里状态为 `ACTIVE` 且 `start_date` 最晚的一份；
    /// 没有 `ACTIVE` 则取最近一份历史合同）。设计 §5.4 详情头部要「这份合同」，列表/时间线用
    /// `contracts[]`，两个形状都给以免前端自己挑。
    pub current_contract: Option<EmployeeContractRes>,
    /// 现行薪资标准（业务日在生效区间内的那批；`salary_items` 全量历史里标了 `current`）。
    pub salary_items: Vec<EmployeeSalaryRes>,
}

impl EmployeeDetailRes {
    /// 由模型行 + 关联名字 + 脱敏规则构造（子表先留空，用 [`Self::with_contracts`] /
    /// [`Self::with_salary_items`] 补）。
    ///
    /// # 参数
    ///
    /// - `row`：`employee_model::find` 的全字段行（含 `id_card` / `bank_account` **明文**）；
    /// - `names`：岗位 / 职级 / 学历名 + 创建人 / 修改人；
    /// - `mask`：本次请求的脱敏规则集合。
    ///
    /// 不接 `as_of`：这里不产出任何「按今天算」的字段（到期天数在子表构造里算），
    /// 多传一个用不上的参数只会让调用方以为它有用。
    pub fn from_row(row: &EmployeeRow, names: &EmployeeNames, mask: &MaskSet) -> Self {
        Self {
            id: row.id.to_string(),
            emp_no: row.emp_no.clone(),
            name: row.name.clone(),
            gender: row.gender,
            gender_label: gender_label(row.gender),
            birth_date: row.birth_date,
            id_card_type_label: id_card_type_label(&row.id_card_type),
            id_card_type: row.id_card_type.clone(),
            mobile: mask_in_place(mask, COL_MOBILE, row.mobile.as_deref()),
            email: clean(row.email.as_deref()),
            dept_id: row.dept_id.to_string(),
            dept_name: dept_name_of(&row.dept_name),
            position_name: pair_of(&row.position_code, &names.position),
            position_code: row.position_code.clone(),
            job_level_name: pair_of(&row.job_level, &names.job_level),
            job_level: row.job_level.clone(),
            employment_type_label: employment_type_label(&row.employment_type),
            employment_type: row.employment_type.clone(),
            hire_date: row.hire_date,
            probation_end_date: row.probation_end_date,
            leave_date: row.leave_date,
            leave_type_label: leave_type_label(row.leave_type.as_deref()),
            leave_type: row.leave_type.clone(),
            status_label: status_label(&row.status),
            status: row.status.clone(),
            education_name: pair_of(&row.education, &names.education),
            education: row.education.clone(),
            school: clean(row.school.as_deref()),
            major: clean(row.major.as_deref()),
            graduate_date: row.graduate_date,
            native_place: clean(row.native_place.as_deref()),
            address: clean(row.address.as_deref()),
            emergency_contact: clean(row.emergency_contact.as_deref()),
            emergency_phone: clean(row.emergency_phone.as_deref()),
            emergency_relation: clean(row.emergency_relation.as_deref()),
            bank_name: clean(row.bank_name.as_deref()),
            bank_branch: clean(row.branch.as_deref()),
            bank_account_name: clean(row.bank_account_name.as_deref()),
            username: username_of(row.user_id, &row.username),
            user_id: row.user_id.map(|id| id.to_string()),
            remark: clean(row.remark.as_deref()),
            created_at: timestamp_cst(row.created_at),
            updated_at: timestamp_cst(row.updated_at),
            create_by_name: names.create_by.clone(),
            update_by_name: names.update_by.clone(),
            sensitive_masked: SensitiveMasked::from_plain(row.id_card.as_deref(), row.bank_account.as_deref(), mask),
            contracts: Vec::new(),
            current_contract: None,
            salary_items: Vec::new(),
        }
    }

    /// 补合同子表，并顺带算出 [`Self::current_contract`]。
    ///
    /// 「现行合同」的选法：`ACTIVE` 里 `start_date` 最晚的一份；没有 `ACTIVE` 就取
    /// `start_date` 最晚的一份历史合同（离职员工的详情仍要显示最后那份合同）。
    /// 排序不依赖调用方：这里自己挑，避免「SQL 忘了 ORDER BY 就选错合同」。
    pub fn with_contracts(mut self, contracts: Vec<ContractInput>, as_of: NaiveDate) -> Self {
        let mut items: Vec<EmployeeContractRes> = contracts
            .into_iter()
            .map(|input| EmployeeContractRes::from_input(input, as_of))
            .collect();
        items.sort_by(|a, b| b.start_date.cmp(&a.start_date));
        self.current_contract = items
            .iter()
            .find(|item| item.is_active())
            .or_else(|| items.first())
            .cloned();
        self.contracts = items;
        self
    }

    /// 补薪资标准子表（顺序保持调用方给的 `effective_from DESC`）。
    pub fn with_salary_items(mut self, items: Vec<SalaryInput>, as_of: NaiveDate) -> Self {
        self.salary_items = items
            .into_iter()
            .map(|input| EmployeeSalaryRes::from_input(input, as_of))
            .collect();
        self
    }

    /// 现行薪资标准（业务日生效的那一批）。
    ///
    /// 返回值而不是字段：一个员工可能同时有「基本工资 + 岗位津贴」两条现行标准，
    /// 这不是列表，前端要自己排（薪资单预览按 `item_code` 匹配）。
    pub fn current_salary(&self) -> Vec<&EmployeeSalaryRes> {
        self.salary_items.iter().filter(|item| item.current).collect()
    }

    /// 是否有可看明文的字段（决定详情页显不显示「查看明文」按钮）。
    pub fn can_view_plain(&self) -> bool {
        self.sensitive_masked.has_any()
    }
}

// ==================== 单测夹具（真库用例也复用） ====================

/// 固定的测试主键。
#[cfg(test)]
pub fn sample_uuid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// 固定基准日（2026-11-12）：避免「跑测试那天刚好跨月 / 合同到期」造成假红。
#[cfg(test)]
pub fn sample_as_of() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 12).expect("固定日期")
}

/// 一条「该有的都有」的列表行。
///
/// `#[cfg(test)]` + `pub`：真库用例（[`crate::business::hr::employee_dto_db_tests`]）与
/// 本文件单测共用同一份样例。给结构体加字段时这里会编译失败 —— 这正是
/// `Maskable::sample_for_mask_test` 那套「全字段样例挡住新字段漏脱敏」的用意。
#[cfg(test)]
pub fn sample_list_row() -> EmployeeListRow {
    EmployeeListRow {
        id: sample_uuid(1),
        emp_no: "EMP-0007".to_string(),
        name: "张三".to_string(),
        gender: 1,
        mobile: Some("13812345678".to_string()),
        dept_id: sample_uuid(2),
        dept_name: "生产部".to_string(),
        position_code: Some("WORKER".to_string()),
        job_level: Some("P3".to_string()),
        employment_type: "FULL_TIME".to_string(),
        hire_date: "2024-03-01".parse().expect("日期"),
        status: "ACTIVE".to_string(),
        user_id: Some(sample_uuid(3)),
        username: "zhangsan".to_string(),
        contract_end_date: Some("2027-02-28".parse().expect("日期")),
    }
}

/// 全字段详情行（含敏感列**明文**：用来证明它们不会出现在出参 JSON 里）。
#[cfg(test)]
pub fn sample_row() -> EmployeeRow {
    EmployeeRow {
        id: sample_uuid(1),
        org_id: sample_uuid(9),
        emp_no: "EMP-0007".to_string(),
        name: "张三".to_string(),
        gender: 1,
        birth_date: Some("1990-01-01".parse().expect("日期")),
        id_card: Some("110101199001011234".to_string()),
        id_card_type: "ID_CARD".to_string(),
        mobile: Some("13812345678".to_string()),
        email: Some("zhangsan@example.com".to_string()),
        dept_id: sample_uuid(2),
        dept_name: "生产部".to_string(),
        position_code: Some("WORKER".to_string()),
        job_level: Some("P3".to_string()),
        employment_type: "FULL_TIME".to_string(),
        hire_date: "2024-03-01".parse().expect("日期"),
        probation_end_date: Some("2024-06-01".parse().expect("日期")),
        leave_date: None,
        leave_type: None,
        status: "ACTIVE".to_string(),
        education: Some("SENIOR".to_string()),
        school: Some("第一中学".to_string()),
        major: None,
        graduate_date: Some("2008-06-30".parse().expect("日期")),
        native_place: Some("北京".to_string()),
        address: Some("北京市朝阳区".to_string()),
        emergency_contact: Some("李四".to_string()),
        emergency_phone: Some("13900001111".to_string()),
        emergency_relation: Some("配偶".to_string()),
        bank_name: Some("招商银行".to_string()),
        branch: Some("朝阳支行".to_string()),
        bank_account: Some("6222021234567890".to_string()),
        bank_account_name: Some("张三".to_string()),
        user_id: Some(sample_uuid(3)),
        username: "zhangsan".to_string(),
        remark: Some("备注".to_string()),
        created_at: "2026-11-12T02:36:00Z".parse().expect("时间"),
        updated_at: "2026-11-12T03:00:00Z".parse().expect("时间"),
        create_by: Some(sample_uuid(4)),
        update_by: Some(sample_uuid(5)),
    }
}

/// 全部关联名字都 join 到的正常情况。
#[cfg(test)]
pub fn sample_names() -> EmployeeNames {
    EmployeeNames::none()
        .with_position(Some("车间工人"))
        .with_job_level(Some("P3"))
        .with_education(Some("高中"))
        .with_operators(Some("admin"), Some("hr01"))
}

// ==================== 单测：DTO 形状（纯内存，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::masking::registry::{self, MaskSet};
    use serde_json::Value as Json;

    /// 夹具从模块级（`#[cfg(test)] pub`）取，真库用例共用同一份。
    use super::{
        sample_as_of as as_of, sample_list_row as list_row, sample_names as names, sample_row as detail_row,
        sample_uuid as uuid,
    };

    fn date(text: &str) -> NaiveDate {
        text.parse().expect("测试用日期")
    }

    /// `MaskSet::empty()`：没有任何规则（等价于「V0388 的规则被删了」）。
    /// 本 DTO 对身份证 / 银行卡**仍然打码**（走设计默认参数），这是「超管也看不到明文」的一半证据。
    fn no_rules() -> MaskSet {
        MaskSet::empty()
    }

    /// 带 V0388 两条全局规则的集合（生产环境的常态）。
    fn seeded_rules() -> MaskSet {
        // 两条规则不能同时塞进 single_rule_set，所以逐个验证：先身份证，再银行卡。
        registry::single_rule_set(TABLE, COL_ID_CARD, MaskType::IdCard, Some(DEFAULT_ID_CARD_PARAM))
    }

    fn contract(status: &str, end: Option<NaiveDate>, renewed: Option<Uuid>) -> ContractInput {
        ContractInput {
            id: uuid(11),
            contract_no: "HT-2024-007".to_string(),
            contract_type: "FIXED_TERM".to_string(),
            sign_date: date("2024-02-20"),
            start_date: date("2024-03-01"),
            end_date: end,
            probation_months: 3,
            status: status.to_string(),
            terminated_date: None,
            terminate_reason: None,
            renewed_from_id: renewed,
            renewed_from_no: renewed.map(|_| "HT-2021-001".to_string()),
            remark: None,
        }
    }

    fn salary(code: &str, amount: Decimal, from: &str, to: Option<&str>) -> SalaryInput {
        SalaryInput {
            item_code: code.to_string(),
            item_name: Some(format!("{code}-名")),
            amount,
            effective_from: date(from),
            effective_to: to.map(date),
            remark: None,
        }
    }

    // ---------- 1. 成对输出 ----------

    #[test]
    fn list_pairs_are_complete() {
        let item = EmployeeListRes::from_row(&list_row(), &names(), &no_rules(), as_of());

        // 四对关联字段：有 id 就必须有 name
        assert_eq!(item.dept_name, "生产部");
        assert_eq!(item.position_code.as_deref(), Some("WORKER"));
        assert_eq!(item.position_name.as_deref(), Some("车间工人"));
        assert_eq!(item.job_level.as_deref(), Some("P3"));
        assert_eq!(item.job_level_name.as_deref(), Some("P3"));
        assert!(item.user_id.is_some());
        assert_eq!(item.username.as_deref(), Some("zhangsan"));

        // code 与 name 必须同生同灭
        for (code, name) in [
            (&item.position_code, &item.position_name),
            (&item.job_level, &item.job_level_name),
            (&item.user_id, &item.username),
        ] {
            assert_eq!(code.is_some(), name.is_some(), "code/name 必须成对：{item:?}");
        }

        // 标签齐全
        assert_eq!(item.status_label, "在职");
        assert_eq!(item.employment_type_label, "正式");
        assert_eq!(item.gender_label, "男");
    }

    #[test]
    fn list_pairs_are_null_together() {
        let mut row = list_row();
        row.position_code = None;
        row.job_level = None;
        row.user_id = None;
        row.username = String::new();
        let item = EmployeeListRes::from_row(&row, &names(), &no_rules(), as_of());

        assert!(item.position_code.is_none() && item.position_name.is_none());
        assert!(item.job_level.is_none() && item.job_level_name.is_none());
        assert!(item.user_id.is_none() && item.username.is_none());

        // 字典名没 join 到 → 回退码值（不是空、不是 None）
        let item = EmployeeListRes::from_row(&list_row(), &EmployeeNames::none(), &no_rules(), as_of());
        assert_eq!(item.position_name.as_deref(), Some("WORKER"));
        assert_eq!(item.job_level_name.as_deref(), Some("P3"));
    }

    #[test]
    fn deleted_relations_fall_back_to_placeholder_not_empty() {
        let mut row = list_row();
        row.dept_name = "   ".to_string();
        row.username = String::new(); // 账号行被删，join 出空串
        let item = EmployeeListRes::from_row(&row, &names(), &no_rules(), as_of());
        assert_eq!(item.dept_name, DEPT_GONE);
        assert_eq!(item.username.as_deref(), Some(USER_GONE));
        assert!(item.user_id.is_some(), "id 还在，只是名字查不到");
    }

    #[test]
    fn detail_pairs_are_complete() {
        let detail = EmployeeDetailRes::from_row(&detail_row(), &names(), &seeded_rules());
        for (code, name) in [
            (&detail.position_code, &detail.position_name),
            (&detail.job_level, &detail.job_level_name),
            (&detail.education, &detail.education_name),
            (&detail.leave_type, &detail.leave_type_label),
            (&detail.user_id, &detail.username),
        ] {
            assert_eq!(code.is_some(), name.is_some(), "code/name 必须成对：{detail:?}");
        }
        assert_eq!(detail.education_name.as_deref(), Some("高中"));
        assert_eq!(detail.leave_type, None);
        assert_eq!(detail.create_by_name.as_deref(), Some("admin"));
        assert_eq!(detail.update_by_name.as_deref(), Some("hr01"));

        // 离职员工：leave_type 与中文标签成对
        let mut row = detail_row();
        row.status = "RESIGNED".to_string();
        row.leave_date = Some(date("2026-10-31"));
        row.leave_type = Some("RESIGN".to_string());
        let resigned = EmployeeDetailRes::from_row(&row, &names(), &seeded_rules());
        assert_eq!(resigned.leave_type.as_deref(), Some("RESIGN"));
        assert_eq!(resigned.leave_type_label.as_deref(), Some("辞职"));
        assert_eq!(resigned.status_label, "离职");
    }

    // ---------- 2. 敏感字段只出 masked ----------

    #[test]
    fn detail_never_carries_plaintext_sensitive_columns() {
        let detail = EmployeeDetailRes::from_row(&detail_row(), &names(), &seeded_rules());
        let text = serde_json::to_string(&detail).expect("序列化");

        // 明文一个字节都不许出现
        assert!(!text.contains("110101199001011234"), "身份证明文泄漏：{text}");
        assert!(!text.contains("6222021234567890"), "银行卡明文泄漏：{text}");
        // 也不许出现这两个字段名（字段名在就说明有人把明文塞进来了）
        let json: Json = serde_json::from_str(&text).expect("反序列化");
        assert!(json.get("id_card").is_none(), "出参不得有 id_card 字段");
        assert!(json.get("bank_account").is_none(), "出参不得有 bank_account 字段");
        assert!(
            json["sensitive_masked"]["id_card_masked"]
                .as_str()
                .is_some_and(|value| value.contains('*')),
            "id_card_masked 必须是打码形态：{text}"
        );
        assert!(
            json["sensitive_masked"]["bank_account_masked"]
                .as_str()
                .is_some_and(|value| value.contains('*')),
            "bank_account_masked 必须是打码形态：{text}"
        );
    }

    #[test]
    fn masked_values_follow_v0388_rules() {
        let detail = EmployeeDetailRes::from_row(&detail_row(), &names(), &seeded_rules());
        // 身份证规则 2 + 参数 6,4 → 保留前 6 后 4、中间固定 4 星（与 V0388 落地记录一致）
        assert_eq!(
            detail.sensitive_masked.id_card_masked.as_deref(),
            Some("110101****1234")
        );
        // 规则集里没有 bank_account 规则 → 走设计默认 5 + 4,4
        let bank = detail
            .sensitive_masked
            .bank_account_masked
            .clone()
            .expect("银行卡应有打码值");
        assert!(bank.starts_with("6222"), "保留前 4 位：{bank}");
        assert!(bank.ends_with("7890"), "保留后 4 位：{bank}");
        assert!(bank.contains('*'), "中间必须打码：{bank}");
        assert!(!bank.contains("12345678"), "中间明文不许留：{bank}");
    }

    #[test]
    fn empty_rule_set_still_masks() {
        // 规则被删 / 规则表为空时也不能退化成明文
        let detail = EmployeeDetailRes::from_row(&detail_row(), &names(), &no_rules());
        assert_eq!(
            detail.sensitive_masked.id_card_masked.as_deref(),
            Some("110101****1234")
        );
        assert!(detail.sensitive_masked.bank_account_masked.is_some());
        assert!(detail.can_view_plain());
    }

    #[test]
    fn exempt_set_still_masks_employee_sensitive_fields() {
        // 超管 / 持 field-mask:plain 的人在 ordinary DTO 上仍然看不到明文：
        // 明文唯一出口是 /employee/{id}/sensitive（要 sensitive 码 + 写审计）
        let mask = registry::resolve_super();
        assert!(mask.is_exempt(), "夹具前提：这是豁免集合");
        let detail = EmployeeDetailRes::from_row(&detail_row(), &names(), &mask);
        let text = serde_json::to_string(&detail).expect("序列化");
        assert!(!text.contains("110101199001011234"), "豁免不得绕过 HR 明文出口：{text}");
        assert!(!text.contains("6222021234567890"), "豁免不得绕过 HR 明文出口：{text}");
        assert_eq!(
            detail.sensitive_masked.id_card_masked.as_deref(),
            Some("110101****1234")
        );
    }

    #[test]
    fn absent_sensitive_values_are_null_not_masked() {
        let mut row = detail_row();
        row.id_card = None;
        row.bank_account = Some("   ".to_string());
        let detail = EmployeeDetailRes::from_row(&row, &names(), &seeded_rules());
        assert!(detail.sensitive_masked.id_card_masked.is_none());
        assert!(
            detail.sensitive_masked.bank_account_masked.is_none(),
            "空白值不该产出「****」噪音"
        );
        assert!(!detail.can_view_plain(), "没登记就不该出现「查看明文」入口");
    }

    #[test]
    fn broken_mask_param_fails_closed_to_full_stars() {
        // 参数被改坏（如 mask_param='bad'）→ 全星号，绝不回退明文
        let raw = "6222021234567890";
        let masked = render(MaskType::BankCard, Some("bad"), raw);
        assert_eq!(masked, "*".repeat(raw.chars().count()));
        // 正常参数仍然是可读形态（幂等：再打一次不会吃掉掩码）
        let once = render(MaskType::BankCard, Some(DEFAULT_BANK_ACCOUNT_PARAM), raw);
        assert!(once.contains('*'));
    }

    #[test]
    fn mobile_is_masked_only_when_a_rule_exists() {
        let row = list_row();
        let plain = EmployeeListRes::from_row(&row, &names(), &no_rules(), as_of());
        assert_eq!(
            plain.mobile.as_deref(),
            Some("13812345678"),
            "V0388 没给 biz_employee.mobile 种规则 ⇒ 设计 §5.2 #1 的列表列保持明文"
        );
        let masked = EmployeeListRes::from_row(
            &row,
            &names(),
            &registry::single_rule_set(TABLE, COL_MOBILE, MaskType::Mobile, None),
            as_of(),
        );
        let value = masked.mobile.expect("有值");
        assert!(value.contains('*'), "有规则就必须遮：{value}");
        assert_ne!(value, "13812345678");
    }

    // ---------- 3. DTO 形状（字段名是前后端硬契约） ----------

    #[test]
    fn list_json_shape_is_pinned() {
        let item = EmployeeListRes::from_row(&list_row(), &names(), &no_rules(), as_of());
        let json = serde_json::to_value(&item).expect("序列化");
        let mut keys: Vec<&str> = json.as_object().expect("对象").keys().map(String::as_str).collect();
        keys.sort_unstable();
        let expected = [
            "contract_end_date",
            "contract_expire_in_days",
            "dept_id",
            "dept_name",
            "emp_no",
            "employment_type",
            "employment_type_label",
            "gender",
            "gender_label",
            "hire_date",
            "id",
            "job_level",
            "job_level_name",
            "mobile",
            "name",
            "position_code",
            "position_name",
            "status",
            "status_label",
            "user_id",
            "username",
        ];
        assert_eq!(keys, expected, "列表行字段集合变了（前端会静默 undefined）");
        // 日期必须是 YYYY-MM-DD 字符串
        assert_eq!(json["hire_date"], serde_json::json!("2024-03-01"));
        assert_eq!(json["contract_end_date"], serde_json::json!("2027-02-28"));
        // 空值必须是 null（不是 ""）
        assert_eq!(json["mobile"], serde_json::json!("13812345678"));
    }

    #[test]
    fn detail_json_shape_is_pinned() {
        let detail = EmployeeDetailRes::from_row(&detail_row(), &names(), &seeded_rules())
            .with_contracts(vec![contract("ACTIVE", Some(date("2027-02-28")), None)], as_of())
            .with_salary_items(
                vec![salary("BASE", Decimal::new(80000000, 4), "2024-03-01", None)],
                as_of(),
            );
        let json = serde_json::to_value(&detail).expect("序列化");
        let mut keys: Vec<&str> = json.as_object().expect("对象").keys().map(String::as_str).collect();
        keys.sort_unstable();
        let expected = [
            "address",
            "bank_account_name",
            "bank_branch",
            "bank_name",
            "birth_date",
            "contracts",
            "create_by_name",
            "created_at",
            "current_contract",
            "dept_id",
            "dept_name",
            "education",
            "education_name",
            "email",
            "emergency_contact",
            "emergency_phone",
            "emergency_relation",
            "emp_no",
            "employment_type",
            "employment_type_label",
            "gender",
            "gender_label",
            "graduate_date",
            "hire_date",
            "id",
            "id_card_type",
            "id_card_type_label",
            "job_level",
            "job_level_name",
            "leave_date",
            "leave_type",
            "leave_type_label",
            "major",
            "mobile",
            "name",
            "native_place",
            "position_code",
            "position_name",
            "probation_end_date",
            "remark",
            "salary_items",
            "school",
            "sensitive_masked",
            "status",
            "status_label",
            "update_by_name",
            "updated_at",
            "user_id",
            "username",
        ];
        assert_eq!(keys, expected, "详情字段集合变了（前端会静默 undefined）");

        // 敏感值是嵌套对象（设计 §5.2 #2 形状），不是平铺
        assert_eq!(
            json["sensitive_masked"]
                .as_object()
                .expect("sensitive_masked 必须是对象")
                .len(),
            2
        );
        // 时间戳必须带 +08:00（前端 new Date() 才能对齐业务日）
        let created = json["created_at"].as_str().expect("created_at 字符串");
        assert!(created.ends_with("+08:00"), "时间戳必须带 +08:00：{created}");
        assert_eq!(created, "2026-11-12T10:36:00+08:00");
        // 子表即使为空也是数组（不是缺字段 / null）
        assert!(json["contracts"].is_array());
        assert!(json["salary_items"].is_array());
        assert!(json["current_contract"].is_object(), "有合同时必须给出「现行合同」");
    }

    /// 把列表行与详情的 JSON 原样打出来（`--nocapture` 时可见）。
    ///
    /// 这条用例是给**前端（HR-18）看的契约样例**：跑一次就能拿到真实出参，
    /// 不用等后端起服务。断言只保证「能序列化 + 关键字段在」，形状由上面两条 census 钉。
    #[test]
    fn dto_json_samples_are_printable() {
        let list = EmployeeListRes::from_row(&list_row(), &names(), &no_rules(), as_of());
        println!(
            "[HR-11] list row JSON = {}",
            serde_json::to_string_pretty(&list).expect("序列化")
        );
        let detail = EmployeeDetailRes::from_row(&detail_row(), &names(), &seeded_rules())
            .with_contracts(
                vec![contract("ACTIVE", Some(date("2027-02-28")), Some(uuid(10)))],
                as_of(),
            )
            .with_salary_items(
                vec![salary("BASE", Decimal::new(80000000, 4), "2024-03-01", None)],
                as_of(),
            );
        let text = serde_json::to_string_pretty(&detail).expect("序列化");
        println!("[HR-11] detail JSON = {text}");
        assert!(text.contains("\"sensitive_masked\""));
        assert!(text.contains("110101****1234"));
    }

    #[test]
    fn empty_subsets_are_empty_arrays_and_null_current_contract() {
        let detail =
            EmployeeDetailRes::from_row(&detail_row(), &names(), &seeded_rules()).with_contracts(Vec::new(), as_of());
        let json = serde_json::to_value(&detail).expect("序列化");
        assert_eq!(json["contracts"], serde_json::json!([]));
        assert_eq!(json["current_contract"], Json::Null);
        assert!(detail.current_salary().is_empty());
    }

    // ---------- 4. 合同 / 薪资子表 ----------

    #[test]
    fn contract_pairs_and_expire_days() {
        let detail = EmployeeDetailRes::from_row(&detail_row(), &names(), &seeded_rules()).with_contracts(
            vec![
                contract("RENEWED", Some(date("2024-02-28")), None),
                contract("ACTIVE", Some(date("2027-02-28")), Some(uuid(10))),
            ],
            as_of(),
        );

        // 现行合同 = ACTIVE 那份，且按 start_date 倒序排在首位
        let current = detail.current_contract.clone().expect("应有现行合同");
        assert!(current.is_active());
        assert_eq!(current.status_label, "生效中");
        assert_eq!(current.contract_type_label, "固定期限");
        // 续签链成对：id 与编号同生同灭
        assert!(current.renewed_from_id.is_some());
        assert_eq!(current.renewed_from_no.as_deref(), Some("HT-2021-001"));
        // 距 2027-02-28 还有多少天（as_of = 2026-11-12）
        assert_eq!(
            current.expire_in_days,
            Some(expire_in_days(date("2027-02-28"), as_of()))
        );
        assert!(current.expire_in_days.expect("天数") > CONTRACT_EXPIRE_WARN_DAYS);

        // 非 ACTIVE 的合同不给 expire_in_days（它已经不算「现时」的了）
        let renewed = detail
            .contracts
            .iter()
            .find(|item| item.status == "RENEWED")
            .expect("一份续签历史");
        assert_eq!(renewed.expire_in_days, None);
        assert_eq!(renewed.renewed_from_id, None);
        assert_eq!(renewed.renewed_from_no, None);
    }

    #[test]
    fn expired_contract_reports_negative_days() {
        let item = EmployeeContractRes::from_input(contract("ACTIVE", Some(date("2026-10-31")), None), as_of());
        assert_eq!(
            item.expire_in_days,
            Some(-12),
            "已过期照给负数，前端才能显示「已过期 12 天」"
        );
        let row = EmployeeListRes::from_row(
            &EmployeeListRow {
                contract_end_date: Some(date("2026-12-01")),
                ..list_row()
            },
            &names(),
            &no_rules(),
            as_of(),
        );
        assert_eq!(row.contract_expire_in_days, Some(19));
        assert!(row.contract_urgent(), "20 天内到期必须进「快到期」名单");
    }

    #[test]
    fn current_contract_falls_back_to_latest_history() {
        let detail = EmployeeDetailRes::from_row(&detail_row(), &names(), &seeded_rules()).with_contracts(
            vec![
                contract("TERMINATED", Some(date("2024-02-28")), None),
                contract("EXPIRED", Some(date("2021-02-28")), None),
            ],
            as_of(),
        );
        let current = detail.current_contract.expect("离职员工也要显示最后一份合同");
        assert_eq!(current.end_date, Some(date("2024-02-28")));
    }

    #[test]
    fn salary_current_flag_and_amount_as_string() {
        let detail = EmployeeDetailRes::from_row(&detail_row(), &names(), &seeded_rules()).with_salary_items(
            vec![
                salary("BASE", Decimal::new(80000000, 4), "2024-03-01", None),
                salary("ALLOW", Decimal::new(150000, 4), "2027-01-01", None),
                salary("OLD", Decimal::new(10000000, 4), "2020-01-01", Some("2024-02-29")),
            ],
            as_of(),
        );
        let current: Vec<&str> = detail
            .current_salary()
            .iter()
            .map(|item| item.item_code.as_str())
            .collect();
        assert_eq!(current, vec!["BASE"], "只有落在区间内的算现行");

        let json = serde_json::to_value(&detail).expect("序列化");
        // 金额是字符串，不是浮点（前端不做浮点运算）
        assert_eq!(json["salary_items"][0]["amount"], serde_json::json!("8000.0000"));
        assert!(json["salary_items"][0]["amount"].is_string());
        // 薪资项名与编码成对
        assert_eq!(json["salary_items"][0]["item_name"], serde_json::json!("BASE-名"));
    }

    #[test]
    fn salary_item_name_falls_back_to_code() {
        let item = EmployeeSalaryRes::from_input(
            SalaryInput {
                item_name: None,
                ..salary("BASE", Decimal::new(1, 4), "2024-03-01", None)
            },
            as_of(),
        );
        assert_eq!(item.item_name, "BASE", "join 不到名字时回退编码，不留空白格");
    }

    // ---------- 5. 标签表与 DDL 对拍 ----------

    #[test]
    fn labels_fall_back_to_code_never_empty() {
        assert_eq!(status_label("PROBATION"), "试用期");
        assert_eq!(status_label("ACTIVE"), "在职");
        assert_eq!(status_label("SUSPENDED"), "停职");
        assert_eq!(status_label("RESIGNED"), "离职");
        // 库里新加的取值（代码没跟上）→ 原样显示码值，且不 panic
        assert_eq!(status_label("FURLOUGH"), "FURLOUGH");
        assert_eq!(status_label("  "), "");
        assert_eq!(gender_label(0), "未知");
        assert_eq!(gender_label(1), "男");
        assert_eq!(gender_label(2), "女");
        assert_eq!(gender_label(7), "未知(7)");
        assert_eq!(id_card_type_label("ID_CARD"), "身份证");
        assert_eq!(id_card_type_label("HK_MO_TW"), "港澳台居民证件");
        assert_eq!(contract_type_label("UNFIXED_TERM"), "无固定期限");
        assert_eq!(contract_status_label("TERMINATED"), "已解除");
        assert_eq!(employment_type_label("DISPATCH"), "劳务派遣");
        assert_eq!(leave_type_label(Some("CONTRACT_END")).as_deref(), Some("合同到期"));
        assert_eq!(leave_type_label(None), None);
    }

    /// 标签表必须覆盖 DDL `CHECK` 的全部取值 —— 少一行，界面直接显示码值。
    #[test]
    fn status_labels_match_ddl_check() {
        assert_eq!(STATUS_LABELS.len(), STATUS_VALUES.len());
        for value in STATUS_VALUES {
            assert!(
                STATUS_LABELS.iter().any(|(code, _)| code == value),
                "状态 {value} 缺中文标签"
            );
        }
        // 默认列表状态是 STATUS_VALUES 去掉 RESIGNED（设计 §5.3）
        assert_eq!(DEFAULT_STATUSES.len(), STATUS_VALUES.len() - 1);
        assert!(!DEFAULT_STATUSES.contains(&"RESIGNED"));
        // 六个用工形式、四个证件类型、五类合同 / 四种合同状态：条数与 DDL 对齐
        assert_eq!(EMPLOYMENT_TYPE_LABELS.len(), 6);
        assert_eq!(ID_CARD_TYPE_LABELS.len(), 4);
        assert_eq!(LEAVE_TYPE_LABELS.len(), 5);
        assert_eq!(CONTRACT_TYPE_LABELS.len(), 5);
        assert_eq!(CONTRACT_STATUS_LABELS.len(), 4);
        // 同一张表里不许有重复码值（重复会让 label_of 取到第一条，静默错）
        for table in [
            STATUS_LABELS,
            EMPLOYMENT_TYPE_LABELS,
            LEAVE_TYPE_LABELS,
            ID_CARD_TYPE_LABELS,
            CONTRACT_TYPE_LABELS,
            CONTRACT_STATUS_LABELS,
        ] {
            let mut codes: Vec<&str> = table.iter().map(|(code, _)| *code).collect();
            codes.sort_unstable();
            let count = codes.len();
            codes.dedup();
            assert_eq!(codes.len(), count, "标签表有重复码值");
        }
    }

    #[test]
    fn sensitive_constants_match_v0388_seed() {
        // 参数写错 = 打码形态与设计记录不一致（V0388 的 mask_param）
        assert_eq!(DEFAULT_ID_CARD_PARAM, "6,4");
        assert_eq!(DEFAULT_BANK_ACCOUNT_PARAM, "4,4");
        assert_eq!(MaskType::IdCard.as_i16(), 2, "V0388 的 mask_type=2");
        assert_eq!(MaskType::BankCard.as_i16(), 5, "V0388 的 mask_type=5");
        // 列名与 DDL 列名一致（sys_field_mask 按表+列匹配，写错就静默失效）
        assert_eq!(TABLE, "biz_employee");
        assert_eq!(COL_ID_CARD, "id_card");
        assert_eq!(COL_BANK_ACCOUNT, "bank_account");
    }

    #[test]
    fn today_is_cst_business_day() {
        // 业务日必须按 +08:00 算：UTC 与 CST 的日期差 ≤1 天，且不 panic
        let cst = today();
        let utc = Utc::now().date_naive();
        assert!((cst - utc).num_days().abs() <= 1);
    }
}
