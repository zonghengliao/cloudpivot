//! 工时台账契约层（成本核算 · ARCH-9 §3.1 ④ / §4.5 / §5.2 #10–#14 / §5.3）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
//! §3.1 ④（`biz_cost_labor` 表结构 + 6 条 CHECK）、
//! §4.5.3（8 列录入口径）、§5.2「工时台账」5 个接口、
//! §5.3 权限码 `business:erp:cost-labor:{list,create,update,delete,import}`。
//!
//! # 金额为什么由服务端算，而不是信客户端
//!
//! DDL 有一条 `ck_biz_cost_labor_amount CHECK (amount = round(hours * rate, 4))`
//! —— 它的存在就是「堵改工时忘改金额」（迁移注释原话）。如果让前端传 `amount`，
//! 就变成两道口径赛跑：前端算错了，库直接抛 23514，用户看到的是英文约束名；
//! 前端算对了，也只是重复计算。
//!
//! 所以本模块的口径是：
//!
//! 1. **金额一律服务端算**（`round(hours × rate, 4)`），入参里的 `amount` 只是
//!    「可选的核对值」：给了就必须等于服务端算出来的，否则 [`AMOUNT_CODE`]（`66058`）
//!    报「工时×费率与金额不符」；
//! 2. **写库时金额由 SQL 自己算**（`round(?::numeric * ?::numeric, 4)`），
//!    这样算出来的值与 CHECK 的求值表达式是**同一个引擎、同一套舍入规则**，
//!    不存在「Rust 舍入 vs PG 舍入」在 .5 边界上打架导致插不进去；
//! 3. 万一被绕过（并发 / 手工 SQL / 未来的批量写入），`DbError::CheckViolation`
//!    命中 `ck_biz_cost_labor_amount` 时由 [`map_write_error`] 翻译成同一条人话。
//!
//! # 工时 / 费率的精度为什么先拦，而不是让库四舍五入
//!
//! 列是 `NUMERIC(10,2)` / `NUMERIC(18,6)`：用户要是填 `hours=8.555`，库会把**列值**
//! 舍成 8.56 再参与 CHECK 求值，而用户心里的 8.555 与 8.56 差 0.005 小时 ——
//! 金额对得上但工时是错的，而且没人会发现。所以超过列精度直接 400 指名，
//! 不做静默舍入（与设计 §4.5.2「不猜」同一条原则）。
//!
//! # 出参口径（§5.1）
//!
//! ```text
//! 列表  { list, total, page, page_size }
//! 金额  hours / machine_hours / rate / amount 一律字符串（防 JS number 精度丢失）
//! 日期  work_date = "YYYY-MM-DD"；审计列 = RFC3339
//! ```
//!
//! # 名字怎么给（AGENTS 约定 13）
//!
//! | 列 | 口径 | 做法 |
//! |---|---|---|
//! | `employee_name` | **审计 / 历史快照**（当时是谁干的活） | 存列，不 join（`biz_employee` 属 ARCH9-2，未上线） |
//! | `dept_name`（快照列） | 审计快照 | 建 / 改时写入，join 落空时兜底 |
//! | `dept_id` → `dept_name` | 主数据关联 | `LEFT JOIN sys_dept` 实时取名优先 |
//! | `cost_object_id` → `cost_object_no` / `product_name` | 主数据关联 | `LEFT JOIN biz_cost_object` + `LEFT JOIN biz_material` |

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::shared::error_code;

// ==================== 值域（与 V0371 的 CHECK 逐字一致） ====================

/// 状态：草稿（可改、可删、可确认）。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：已确认（只读；`calc` 第 2 段只归集这一种，设计 §4.3）。
pub const STATUS_CONFIRMED: &str = "CONFIRMED";
/// 状态值域（`ck_biz_cost_labor_status`）。
pub const STATUSES: [&str; 2] = [STATUS_DRAFT, STATUS_CONFIRMED];

/// 来源：网页手工录入（本卡 HTTP 建单只写这个）。
pub const SOURCE_MANUAL: &str = "MANUAL";
/// 来源：Excel 批量导入（B2-2 写入）。
pub const SOURCE_IMPORT: &str = "IMPORT";
/// 来源：MES 报工（ARCH-4 上线后写入）。
pub const SOURCE_MES: &str = "MES";
/// 来源值域（`ck_biz_cost_labor_src`）。
pub const SOURCE_TYPES: [&str; 3] = [SOURCE_MANUAL, SOURCE_IMPORT, SOURCE_MES];

/// 金额小数位（`amount NUMERIC(18,4)`，CHECK 里的 `round(…, 4)`）。
pub const AMOUNT_SCALE: u32 = 4;
/// 费率小数位（`rate NUMERIC(18,6)`）。
pub const RATE_SCALE: u32 = 6;
/// 工时小数位（`hours` / `machine_hours` 均 `NUMERIC(10,2)`）。
pub const HOURS_SCALE: u32 = 2;
/// 期间串长度（`period_code VARCHAR(7)`，形如 `2026-11`）。
pub const PERIOD_LEN: usize = 7;
/// 姓名列长度（`employee_name VARCHAR(50)`）。
pub const EMPLOYEE_NAME_MAX: usize = 50;
/// 工作中心名列长度（`dept_name VARCHAR(120)`）。
pub const DEPT_NAME_MAX: usize = 120;
/// 备注列长度（`remark VARCHAR(255)`）。
pub const REMARK_MAX: usize = 255;
/// `hours NUMERIC(10,2)` 的上限（8 位整数 + 2 位小数）。
pub const HOURS_MAX: &str = "99999999.99";
/// `rate NUMERIC(18,6)` 的上限（12 位整数 + 6 位小数）。
pub const RATE_MAX: &str = "999999999999.999999";
/// `amount NUMERIC(18,4)` 的上限（14 位整数 + 4 位小数）。
pub const AMOUNT_MAX: &str = "99999999999999.9999";

/// 金额与「工时×费率」不符的业务错误码（`66058`，见 `shared::error_code`）。
pub const AMOUNT_CODE: i32 = error_code::ERP_COST_LABOR_AMOUNT_MISMATCH;

/// 与 PG `numeric` 一致的舍入策略（PG 对 numeric 的 `round()` 是**半数远离零**）。
///
/// 与 `stockin_base::ROUNDING` / `stockout_base::rust_decimal_rounding()` 同口径：
/// 全仓金额舍入只有这一种规则，否则同一条工时在两条路径上算出不同的金额。
pub const ROUNDING: RoundingStrategy = RoundingStrategy::MidpointAwayFromZero;

// ==================== 入参 ====================

/// 列表查询（§5.2 #10：`period_code` / `cost_object_id` / `dept_id` / `employee_id` / `status`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostLaborPageQuery {
    /// 会计期间 `yyyy-MM`。
    pub period_code: Option<String>,
    /// 成本对象。
    pub cost_object_id: Option<String>,
    /// 工作中心（部门）。
    pub dept_id: Option<String>,
    /// 员工（弱引用，`biz_employee` 未上线时为空）。
    pub employee_id: Option<String>,
    /// 状态 `DRAFT` / `CONFIRMED`。
    pub status: Option<String>,
    /// 关键字：员工姓名 / 对象号 / 产出品名称（模糊）。
    pub keyword: Option<String>,
    /// 页码（1 起）。
    pub page: Option<u64>,
    /// 每页条数（0 = 默认 20，上限 200）。
    pub page_size: Option<u64>,
}

/// 归一化后的列表筛选（model 只认这个）。
#[derive(Debug, Clone)]
pub struct CostLaborFilter {
    /// 会计期间。
    pub period_code: Option<String>,
    /// 成本对象。
    pub cost_object_id: Option<Uuid>,
    /// 工作中心。
    pub dept_id: Option<Uuid>,
    /// 员工。
    pub employee_id: Option<Uuid>,
    /// 状态。
    pub status: Option<String>,
    /// 关键字（`LIKE` 模式由 model 侧拼）。
    pub keyword: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 新建工时（§5.2 #11）。
///
/// `dept_name` 不在入参里：工作中心名是**快照**，由服务端从 `sys_dept` 取名写入
/// （设计 §4.5.3「工作中心必须是 `sys_dept` 的现有部门」）。允许客户端改写快照
/// 等于允许伪造归属，所以不给这个口子。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostLaborCreateReq {
    /// 会计期间 `yyyy-MM`（必填）。
    pub period_code: Option<String>,
    /// 工作日期 `YYYY-MM-DD`（必填，必须落在 `period_code` 内）。
    pub work_date: Option<String>,
    /// 成本对象（必填，且必须是 `OPEN`）。
    pub cost_object_id: Option<String>,
    /// 员工（可空：空 = 按班组计，设计 §4.5.3）。
    pub employee_id: Option<String>,
    /// 员工姓名（必填，姓名快照）。
    pub employee_name: Option<String>,
    /// 工作中心（必填）。
    pub dept_id: Option<String>,
    /// 人工工时（必填，> 0，最多 2 位小数）。
    pub hours: Option<Decimal>,
    /// 机器工时（可空，缺省 0，≥ 0，最多 2 位小数）。
    pub machine_hours: Option<Decimal>,
    /// 工资率（必填，≥ 0，最多 6 位小数）。
    ///
    /// 为什么必填：自动取数（对象 > 车间 > 公司）是 **B2-3** 的活。本卡只做
    /// 「录进来的这张单对不对」，缺费率就让 B2-3 补 —— 在这里偷偷取 0 会让
    /// 人工成本凭空少一块（设计 D-7：不猜）。
    pub rate: Option<Decimal>,
    /// 金额核对值（**可选**）：给了就必须等于 `round(hours × rate, 4)`。
    pub amount: Option<Decimal>,
    /// 备注。
    pub remark: Option<String>,
}

/// 编辑工时（§5.2 #12）：**部分更新** —— 缺失 = 不改，`null` = 清空（可空字段）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostLaborUpdateReq {
    /// 会计期间。
    pub period_code: Option<String>,
    /// 工作日期。
    pub work_date: Option<String>,
    /// 成本对象。
    pub cost_object_id: Option<String>,
    /// 员工（三态）。
    #[serde(default, deserialize_with = "double_option")]
    pub employee_id: Option<Option<String>>,
    /// 员工姓名。
    pub employee_name: Option<String>,
    /// 工作中心。
    pub dept_id: Option<String>,
    /// 人工工时。
    pub hours: Option<Decimal>,
    /// 机器工时。
    pub machine_hours: Option<Decimal>,
    /// 工资率。
    pub rate: Option<Decimal>,
    /// 金额核对值（可选，同建单）。
    pub amount: Option<Decimal>,
    /// 备注（三态）。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

// ==================== 归一化规格（dto 与 model 之间唯一的形状） ====================

/// 归一化后的工时行规格（纯逻辑产物，查库校验由 handler 做）。
#[derive(Debug, Clone, PartialEq)]
pub struct CostLaborSpec {
    /// 会计期间。
    pub period_code: String,
    /// 工作日期。
    pub work_date: NaiveDate,
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 员工。
    pub employee_id: Option<Uuid>,
    /// 员工姓名（快照）。
    pub employee_name: String,
    /// 工作中心。
    pub dept_id: Uuid,
    /// 工作中心名（快照；建 / 改由 handler 从 `sys_dept` 取名）。
    pub dept_name: String,
    /// 人工工时。
    pub hours: Decimal,
    /// 机器工时。
    pub machine_hours: Decimal,
    /// 工资率。
    pub rate: Decimal,
    /// 金额（**恒等于** `round(hours × rate, 4)`，由 dto 算出、SQL 复核）。
    pub amount: Decimal,
    /// 来源类型。
    pub source_type: String,
    /// 状态。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
}

// ==================== 出参 ====================

/// 列表行 / 详情行（`*_id` 都带 `*_name` —— AGENTS 约定 13）。
#[derive(Debug, Clone, Serialize)]
pub struct CostLaborListRes {
    /// 主键。
    pub id: String,
    /// 会计期间。
    pub period_code: String,
    /// 工作日期（`YYYY-MM-DD`）。
    pub work_date: String,
    /// 成本对象。
    pub cost_object_id: String,
    /// 对象号（`CB-yyyyMMdd-NNNNNN`，用户认的就是它）。
    pub cost_object_no: Option<String>,
    /// 成本对象状态（前端要能标「已关批」）。
    pub cost_object_status: Option<String>,
    /// 产出品。
    pub product_id: Option<String>,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 员工。
    pub employee_id: Option<String>,
    /// 员工姓名（姓名快照 —— 审计 / 历史口径，故意不 join 实时名）。
    pub employee_name: String,
    /// 工作中心。
    pub dept_id: String,
    /// 工作中心名（`sys_dept` 实时名优先，落空回落到建单快照）。
    pub dept_name: Option<String>,
    /// 人工工时（字符串）。
    pub hours: String,
    /// 机器工时（字符串）。
    pub machine_hours: String,
    /// 工资率（字符串）。
    pub rate: String,
    /// 金额（字符串；= round(hours × rate, 4)）。
    pub amount: String,
    /// 来源类型。
    pub source_type: String,
    /// 状态 `DRAFT` / `CONFIRMED`。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
    /// 更新时间（RFC3339）。
    pub updated_at: String,
}

/// 确认动作（`DRAFT → CONFIRMED` 的判定，纯函数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    /// 已经是 `CONFIRMED`：**幂等返回**，不写库（重复确认不翻倍的前提）。
    AlreadyConfirmed,
    /// `DRAFT`：写一次状态跃迁。
    Flip,
}

// ==================== 构建 / 解析小件 ====================

/// 400 构造（本模块唯一出口）。
pub fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409 构造。
pub fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 404 构造。
pub fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("工时台账行不存在或已删除（{id}）"),
    )
}

/// 409 构造：只有 `DRAFT` 能改 / 能删。
pub fn wrong_status(action: &str, status: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::RESOURCE_CONFLICT,
        format!(
            "只有草稿（DRAFT）状态的工时才能{action}，当前状态是 {status}。\n\
             已确认的工时是成本归集与结算的依据（设计 §3.1 ④ 列注释「CONFIRMED 只读」），\
             要改就作废这一行重新录入"
        ),
    )
}

/// 金额与「工时×费率」不符的人话文案（客户端核对 + 库 CHECK 两个入口共用）。
///
/// `given` 是「实际拿到的金额」：客户端传的核对值，或库里已有的金额。
pub fn amount_mismatch_message(hours: Decimal, rate: Decimal, expected: Decimal, given: Decimal) -> String {
    format!(
        "工时×费率与金额不符：{hours} 小时 × {rate} 元/小时 = {expected} 元（round(工时×费率, {AMOUNT_SCALE})），\
         但{}是 {given} 元。\n\
         金额由系统按「工时 × 工资率」计算，不能手工指定；请核对工时或工资率后重试\
         （改工时忘改金额是最常见的错，所以库里有 ck_biz_cost_labor_amount 兜底）",
        if given == expected { "提交的金额" } else { "金额" }
    )
}

/// 400 + [`AMOUNT_CODE`]（`66058`）：客户端给的 `amount` 与服务端算的对不上。
pub fn amount_mismatch(hours: Decimal, rate: Decimal, expected: Decimal, given: Decimal) -> ApiError {
    ApiError::new(400, AMOUNT_CODE, amount_mismatch_message(hours, rate, expected, given))
}

/// 库里的 `amount` 与「工时×费率」不符（`ck_biz_cost_labor_amount` 触发）→ 400 + `66058`。
pub fn amount_check_violation(hours: Decimal, rate: Decimal, expected: Decimal) -> ApiError {
    ApiError::new(
        400,
        AMOUNT_CODE,
        format!(
            "工时×费率与金额不符：{hours} 小时 × {rate} 元/小时 = {expected} 元\
             （round(工时×费率, {AMOUNT_SCALE})），但库里的金额与此不符（约束 ck_biz_cost_labor_amount）。\n\
             请核对工时 / 工资率后重新提交；若这一行是从 Excel 导进来的，请检查模板里「工资率」列是否被改过"
        ),
    )
}

/// 写库错误 → 友好 `ApiError`（**唯一的错误出口**，判据走 `DbError` 变体不看文案）。
///
/// | 变体 | 结果 |
/// |---|---|
/// | `CheckViolation` 且约束是 `ck_biz_cost_labor_amount` | 400 + `66058`「工时×费率与金额不符」 |
/// | 其它 `CheckViolation` | 400 + `10001`（点名约束，便于定位枚举 / 期间格式） |
/// | `UniqueViolation` | 409（本表只有主键唯一，撞了基本是取号天灾） |
/// | 其它 | 500（`utils::db_error`，真实原因只进日志） |
pub fn map_write_error(err: DbError, spec: &CostLaborSpec, ctx: &str) -> ApiError {
    match &err {
        DbError::CheckViolation { constraint, .. } if constraint.as_deref() == Some("ck_biz_cost_labor_amount") => {
            amount_check_violation(spec.hours, spec.rate, spec.amount)
        }
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "数据不满足数据库约束{}（工时 / 机器工时 / 费率 / 期间格式 / 状态值域）",
                constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
            ),
        ),
        DbError::UniqueViolation { .. } => conflict("工时台账写入冲突（主键重复），请刷新后重试"),
        _ => crate::utils::db_error(err, ctx),
    }
}

/// 解析 UUID（非法 → 400 + 指名）。
pub fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| bad_request(format!("{field} 不是合法的 UUID（收到「{raw}」）")))
}

/// 解析可空 UUID（空白串等同于空）。
pub fn parse_opt_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => parse_uuid(text, field).map(Some),
    }
}

/// 必填 UUID。
pub fn require_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request(format!("{field} 不能为空")))?;
    parse_uuid(text, field)
}

/// 解析 `YYYY-MM-DD`（10 位定长，形状先卡死再解析）。
pub fn parse_date(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    let text = raw.trim();
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..].iter().all(u8::is_ascii_digit);
    if !shaped {
        return Err(bad_request(format!("{field} 必须是 YYYY-MM-DD（收到「{raw}」）")));
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|error| bad_request(format!("{field} 不是合法日期（{error}）")))
}

/// 解析必填日期。
pub fn require_date(raw: Option<&str>, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    let text = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request(format!("{field} 不能为空")))?;
    parse_date(text, field)
}

/// 清洗可空文本（trim 后空串 → `None`）。
pub fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 三态字段反序列化：缺失 ⇒ `None`（不改）；显式 `null` ⇒ `Some(None)`（清空）；
/// 有值 ⇒ `Some(Some(v))`。
fn double_option<'de, T, D>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// 会计期间格式（与 `ck_biz_cost_labor_period` 的正则等价：`yyyy-MM`，月份 01–12）。
///
/// 手写而不是引正则库：只接受 7 个字符、第 5 位是 `-`、其余是数字、月份 01–12。
pub fn normalize_period(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("period_code（会计期间）不能为空，形如 2026-11"))?;
    let bytes = text.as_bytes();
    let shaped = bytes.len() == PERIOD_LEN
        && bytes[4] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit);
    if !shaped {
        return Err(bad_request(format!("period_code 必须是 yyyy-MM（收到「{text}」）")));
    }
    let month: u8 = text[5..7].parse().unwrap_or(0);
    if !(1..=12).contains(&month) {
        return Err(bad_request(format!("period_code 的月份必须是 01–12（收到「{text}」）")));
    }
    Ok(text.to_string())
}

/// 工作日期必须落在所选期间内（设计 §4.5.3 第 2 列）。
///
/// 为什么非要卡：`period_code` 是归集 / 分摊 / 结算的分组键，而 `work_date` 是
/// 生产日期。两者不同月时，这一行会「记在 A 期的成本上、但导出的工时明细显示 B 月」，
/// 月结对账时没人能解释。跨月补录的正确做法是直接把 `period_code` 选成目标期间。
pub fn ensure_date_in_period(work_date: NaiveDate, period_code: &str) -> std::result::Result<(), ApiError> {
    let actual = work_date.format("%Y-%m").to_string();
    if actual != period_code {
        return Err(bad_request(format!(
            "work_date（{work_date}）不在所选期间 period_code（{period_code}）内：\
             工时按会计期间归集，跨月补录请把 period_code 选成工作日期所属的那个月"
        )));
    }
    Ok(())
}

/// 姓名必填 + 长度 + 不全是空白。
fn require_employee_name(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("employee_name（员工姓名）不能为空：成本记录要留「当时是谁」的姓名快照"))?;
    if text.chars().count() > EMPLOYEE_NAME_MAX {
        return Err(bad_request(format!(
            "employee_name（员工姓名）最长 {EMPLOYEE_NAME_MAX} 个字符（收到 {} 个）",
            text.chars().count()
        )));
    }
    Ok(text.to_string())
}

/// 备注长度（超长提前 400，别让库抛 22001）。
fn clean_remark(value: Option<String>) -> std::result::Result<Option<String>, ApiError> {
    let text = clean_optional(value);
    if let Some(text) = text.as_deref()
        && text.chars().count() > REMARK_MAX
    {
        return Err(bad_request(format!(
            "remark（备注）最长 {REMARK_MAX} 个字符（收到 {} 个）",
            text.chars().count()
        )));
    }
    Ok(text)
}

/// 数值必须落在列能存的范围内、且小数位不超过列的标度。
///
/// 超范围 → 400（否则 PG 抛 22003 numeric field overflow，前端看到英文）；
/// 超标度 → 400（**不做静默四舍五入**，理由见模块头注释）。
pub fn ensure_decimal(
    value: Decimal,
    field: &str,
    scale: u32,
    max: &str,
    allow_zero: bool,
) -> std::result::Result<Decimal, ApiError> {
    if value.is_sign_negative() {
        return Err(bad_request(format!("{field} 不能是负数（收到 {value}）")));
    }
    if value.is_zero() && !allow_zero {
        return Err(bad_request(format!("{field} 必须大于 0（收到 0）")));
    }
    // `Decimal::scale()` 是「小数位数」；`8.50` 的 scale 是 2（尾零也算），符合列定义。
    // 负数 / 科学计数法由 `is_sign_positive` + PG 的 numeric 解析兜住。
    let normalized_scale = value.normalize().scale();
    if value.scale() > scale || normalized_scale > scale {
        return Err(bad_request(format!(
            "{field} 最多 {scale} 位小数（收到 {value}）：数据库列就是这个精度，\
             多出来的位会被静默四舍五入，金额 CHECK 就与实际填的数对不上了。请自行取舍后再提交"
        )));
    }
    let max = Decimal::from_str(max).expect("列上限字面量必须合法");
    if value > max {
        return Err(bad_request(format!(
            "{field} 超出数据库列能存的上限（{value} > {max}）"
        )));
    }
    Ok(value)
}

/// 金额 = `round(hours × rate, AMOUNT_SCALE)`（全模块唯一的算法）。
///
/// 舍入策略与 PG `numeric` 的 `round()` 一致（半数远离零），所以 Rust 这边算出的
/// 期望值与 SQL 里 `round(hours*rate,4)` 求值结果恒等 —— 客户端核对与库 CHECK
/// 不会在 .5 边界上打架。
pub fn expected_amount(hours: Decimal, rate: Decimal) -> std::result::Result<Decimal, ApiError> {
    let product = hours.checked_mul(rate).ok_or_else(|| {
        bad_request(format!(
            "工时×费率的乘积超出可表示范围（{hours} × {rate}），请缩小数值后再提交"
        ))
    })?;
    let amount = product.round_dp_with_strategy(AMOUNT_SCALE, ROUNDING);
    let max = Decimal::from_str(AMOUNT_MAX).expect("金额上限字面量必须合法");
    if amount > max {
        return Err(bad_request(format!(
            "工时×费率算出的金额 {amount} 超出金额列能存的上限（{max}）"
        )));
    }
    Ok(amount)
}

/// 客户端核对值 vs 服务端算值：给了就必须相等（`66058`）。
fn check_amount(
    declared: Option<Decimal>,
    hours: Decimal,
    rate: Decimal,
    expected: Decimal,
) -> std::result::Result<(), ApiError> {
    match declared {
        None => Ok(()),
        Some(given) if given == expected => Ok(()),
        Some(given) => Err(amount_mismatch(hours, rate, expected, given)),
    }
}

// ==================== 建 / 改：字段校验（纯逻辑） ====================

/// 新建请求 → 归一化规格（**纯逻辑**；成本对象 / 部门是否存在由 handler 查库）。
///
/// `dept_name` 由 handler 从 `sys_dept` 取（快照）。
pub fn spec_from_create(req: &CostLaborCreateReq, dept_name: &str) -> std::result::Result<CostLaborSpec, ApiError> {
    let period_code = normalize_period(req.period_code.as_deref())?;
    let work_date = require_date(req.work_date.as_deref(), "work_date")?;
    ensure_date_in_period(work_date, &period_code)?;
    let hours = ensure_decimal(
        req.hours.ok_or_else(|| bad_request("hours（人工工时）不能为空"))?,
        "hours",
        HOURS_SCALE,
        HOURS_MAX,
        false,
    )?;
    let machine_hours = ensure_decimal(
        req.machine_hours.unwrap_or(Decimal::ZERO),
        "machine_hours",
        HOURS_SCALE,
        HOURS_MAX,
        true,
    )?;
    let rate = ensure_decimal(
        req.rate.ok_or_else(|| {
            bad_request(
                "rate（工资率）不能为空：自动取数（成本对象 > 车间 > 公司）属 B2-3，\
                     本接口要求录入时明确费率，不会默认按 0 计",
            )
        })?,
        "rate",
        RATE_SCALE,
        RATE_MAX,
        true,
    )?;
    let amount = expected_amount(hours, rate)?;
    check_amount(req.amount, hours, rate, amount)?;
    let dept_name = normalize_dept_name(dept_name)?;
    Ok(CostLaborSpec {
        period_code,
        work_date,
        cost_object_id: require_uuid(req.cost_object_id.as_deref(), "cost_object_id")?,
        employee_id: parse_opt_uuid(req.employee_id.as_deref(), "employee_id")?,
        employee_name: require_employee_name(req.employee_name.as_deref())?,
        dept_id: require_uuid(req.dept_id.as_deref(), "dept_id")?,
        dept_name,
        hours,
        machine_hours,
        rate,
        amount,
        // 手工建单只写 MANUAL：IMPORT 由 B2-2 的导入入口写，MES 由 ARCH-4 写。
        source_type: SOURCE_MANUAL.to_string(),
        status: STATUS_DRAFT.to_string(),
        remark: clean_remark(req.remark.clone())?,
    })
}

/// 编辑请求 → 归一化规格（与当前值合并；**纯逻辑**）。
///
/// 合并后重新算金额：改了工时 / 费率而金额没同步，是最容易漏的一步 ——
/// 这里让金额**永远**由合并后的工时与费率重算，客户端永远不需要（也不能）指定。
pub fn spec_from_update(
    current: &CostLaborSpec,
    req: &CostLaborUpdateReq,
) -> std::result::Result<CostLaborSpec, ApiError> {
    let mut spec = current.clone();
    if let Some(raw) = req.period_code.as_deref() {
        spec.period_code = normalize_period(Some(raw))?;
    }
    if let Some(raw) = req.work_date.as_deref() {
        spec.work_date = require_date(Some(raw), "work_date")?;
    }
    if let Some(raw) = req.cost_object_id.as_deref() {
        spec.cost_object_id = require_uuid(Some(raw), "cost_object_id")?;
    }
    if let Some(value) = req.employee_id.as_ref() {
        spec.employee_id = parse_opt_uuid(value.as_deref(), "employee_id")?;
    }
    if let Some(raw) = req.employee_name.as_deref() {
        spec.employee_name = require_employee_name(Some(raw))?;
    }
    if let Some(raw) = req.dept_id.as_deref() {
        spec.dept_id = require_uuid(Some(raw), "dept_id")?;
    }
    if let Some(value) = req.hours {
        spec.hours = ensure_decimal(value, "hours", HOURS_SCALE, HOURS_MAX, false)?;
    }
    if let Some(value) = req.machine_hours {
        spec.machine_hours = ensure_decimal(value, "machine_hours", HOURS_SCALE, HOURS_MAX, true)?;
    }
    if let Some(value) = req.rate {
        spec.rate = ensure_decimal(value, "rate", RATE_SCALE, RATE_MAX, true)?;
    }
    if let Some(value) = req.remark.as_ref() {
        spec.remark = clean_remark(value.clone())?;
    }
    // 期间 / 日期任何一个被改，都要重新校验「日期落在期间内」（合并后的口径）
    ensure_date_in_period(spec.work_date, &spec.period_code)?;
    let amount = expected_amount(spec.hours, spec.rate)?;
    check_amount(req.amount, spec.hours, spec.rate, amount)?;
    spec.amount = amount;
    Ok(spec)
}

/// 工作中心名快照长度（`dept_name VARCHAR(120)`）。
fn normalize_dept_name(raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request("dept_name（工作中心名）不能为空"));
    }
    if text.chars().count() > DEPT_NAME_MAX {
        return Err(bad_request(format!(
            "dept_name 最长 {DEPT_NAME_MAX} 个字符（收到 {} 个）",
            text.chars().count()
        )));
    }
    Ok(text.to_string())
}

/// 成本对象状态的人话（错误文案用）：在制 / 已关批 / 已作废。
///
/// 与 `stockout_dto::cost_object_status_label` 同口径（同一套标签），
/// 但本模块自带一份：工时台账不该为了一个标签依赖领料单模块。
pub fn object_status_label(status: &str) -> &'static str {
    use crate::business::erp::cost_object_dto::{STATUS_CANCELLED, STATUS_CLOSED, STATUS_OPEN};
    if status == STATUS_OPEN {
        "在制"
    } else if status == STATUS_CLOSED {
        "已关批"
    } else if status == STATUS_CANCELLED {
        "已作废"
    } else {
        "未知状态"
    }
}

/// 成本对象不是 `OPEN` 时的 409 文案（与领料单同码 `66057`，但口径是工时）。
pub fn object_not_open(object_no: &str, status: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_OBJECT_NOT_OPEN,
        format!(
            "成本对象 {object_no} 当前是「{}」（{status}），不能录工时：只有 OPEN（在制）的对象可以归集人工成本。\n\
             已关批的对象该批成本已经结转，再记工时会让「已结算期间的人工成本」发生变化（设计 §4.1：CLOSED 只读）；\
             请换一个在制的成本对象，或新建一批",
            object_status_label(status)
        ),
    )
}

/// 确认动作判定（**幂等**的唯一入口）。
/// `CONFIRMED` 再确认一次 ⇒ [`ConfirmAction::AlreadyConfirmed`]：不写库、不改
/// `updated_at`、金额当然也不会变（「重复确认不翻倍」的实现点）。
pub fn confirm_action(status: &str) -> ConfirmAction {
    if status == STATUS_CONFIRMED {
        ConfirmAction::AlreadyConfirmed
    } else {
        ConfirmAction::Flip
    }
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    fn base_create() -> CostLaborCreateReq {
        CostLaborCreateReq {
            period_code: Some(" 2026-11 ".to_string()),
            work_date: Some("2026-11-06".to_string()),
            cost_object_id: Some("018f4b2c-1111-7222-8333-444455556666".to_string()),
            employee_id: None,
            employee_name: Some(" 张三 ".to_string()),
            dept_id: Some("018f4b2c-aaaa-7222-8333-444455556666".to_string()),
            hours: Some(dec("8")),
            machine_hours: None,
            rate: Some(dec("50")),
            amount: None,
            remark: None,
        }
    }

    /// 值域与 V0371 的 CHECK 逐字一致（迁移不可改 ⇒ 这里也不能动）。
    #[test]
    fn value_domains_match_migration_checks() {
        assert_eq!(STATUSES, ["DRAFT", "CONFIRMED"]);
        assert_eq!(SOURCE_TYPES, ["MANUAL", "IMPORT", "MES"]);
        assert_eq!(AMOUNT_SCALE, 4, "ck_biz_cost_labor_amount 的 round(…, 4)");
        assert_eq!(RATE_SCALE, 6, "rate NUMERIC(18,6)");
        assert_eq!(HOURS_SCALE, 2, "hours NUMERIC(10,2)");
        assert_eq!(EMPLOYEE_NAME_MAX, 50, "employee_name VARCHAR(50)");
        assert_eq!(DEPT_NAME_MAX, 120, "dept_name VARCHAR(120)");
        assert_eq!(REMARK_MAX, 255, "remark VARCHAR(255)");
        assert_eq!(AMOUNT_CODE, 66058);
    }

    /// 金额算法的 4 条真值（含 V0371 自检里那条「真实工资率除不尽」的行）。
    #[test]
    fn expected_amount_matches_design_examples() {
        assert_eq!(expected_amount(dec("8"), dec("50")).expect("8×50"), dec("400"));
        assert_eq!(
            expected_amount(dec("1"), dec("33.333333")).expect("1×33.333333"),
            dec("33.3333"),
            "V0371 自检的正面用例：除不尽的工资率要留 4 位，不能掉成 2 位"
        );
        assert_eq!(
            expected_amount(dec("7.25"), dec("12.5")).expect("7.25×12.5"),
            dec("90.6250")
        );
        // .5 边界：PG numeric 的 round 是「半数远离零」，Rust 侧必须同口径
        assert_eq!(
            expected_amount(dec("1"), dec("0.00005")).expect("半数边界"),
            dec("0.0001"),
            "0.00005 舍入到 4 位应是 0.0001（若用银行家舍入会得到 0.0000，与 PG 打架）"
        );
    }

    /// 建单：默认值 + trim + 金额自算 + 状态 / 来源收敛。
    #[test]
    fn create_fills_defaults_and_computes_amount() {
        let spec = spec_from_create(&base_create(), "机加工车间").expect("建单");
        assert_eq!(spec.period_code, "2026-11");
        assert_eq!(spec.employee_name, "张三", "姓名要 trim");
        assert_eq!(spec.machine_hours, Decimal::ZERO, "机器工时空 = 0");
        assert_eq!(spec.amount, dec("400"), "金额 = 8 × 50");
        assert_eq!(spec.status, STATUS_DRAFT);
        assert_eq!(spec.source_type, SOURCE_MANUAL);
        assert_eq!(spec.dept_name, "机加工车间");
    }

    /// 必填项缺失 ⇒ 400 且指名（不猜、不给默认 0）。
    #[test]
    fn create_rejects_missing_required_fields() {
        let mut req = base_create();
        req.period_code = None;
        assert!(spec_from_create(&req, "车间").unwrap_err().msg.contains("period_code"));

        let mut req = base_create();
        req.work_date = None;
        assert!(spec_from_create(&req, "车间").unwrap_err().msg.contains("work_date"));

        let mut req = base_create();
        req.cost_object_id = None;
        assert!(
            spec_from_create(&req, "车间")
                .unwrap_err()
                .msg
                .contains("cost_object_id")
        );

        let mut req = base_create();
        req.dept_id = None;
        assert!(spec_from_create(&req, "车间").unwrap_err().msg.contains("dept_id"));

        let mut req = base_create();
        req.employee_name = Some("   ".to_string());
        assert!(
            spec_from_create(&req, "车间")
                .unwrap_err()
                .msg
                .contains("employee_name")
        );

        let mut req = base_create();
        req.hours = None;
        assert!(spec_from_create(&req, "车间").unwrap_err().msg.contains("hours"));
    }

    /// 费率缺失必须报错，**不许默认 0**（设计 D-7：不猜）。
    #[test]
    fn create_requires_rate_and_mentions_b2_3() {
        let mut req = base_create();
        req.rate = None;
        let error = spec_from_create(&req, "车间").expect_err("缺费率");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("rate"), "{}", error.msg);
        assert!(error.msg.contains("B2-3"), "要指明自动取数归 B2-3：{}", error.msg);
        assert!(!error.msg.contains("0 元"), "不许暗示按 0 计：{}", error.msg);
    }

    /// 期间格式 / 日期落在期间内。
    #[test]
    fn period_format_and_date_range_are_checked() {
        for bad in ["2026-13", "2026-1", "26-11", "2026/11", "202611"] {
            let mut req = base_create();
            req.period_code = Some(bad.to_string());
            let error = spec_from_create(&req, "车间").expect_err(bad);
            assert_eq!(error.status, 400, "{bad}");
            assert!(error.msg.contains("period_code"), "{bad}: {}", error.msg);
        }

        let mut req = base_create();
        req.work_date = Some("2026-12-01".to_string());
        let error = spec_from_create(&req, "车间").expect_err("跨月");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("不在所选期间"), "{}", error.msg);
    }

    /// 超列精度必须 400，**不做静默四舍五入**（工时 8.555 ≠ 8.56）。
    #[test]
    fn over_precision_is_rejected_not_rounded() {
        let mut req = base_create();
        req.hours = Some(dec("8.555"));
        let error = spec_from_create(&req, "车间").expect_err("工时 3 位小数");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("最多 2 位小数"), "{}", error.msg);

        let mut req = base_create();
        req.rate = Some(dec("50.1234567"));
        let error = spec_from_create(&req, "车间").expect_err("费率 7 位小数");
        assert!(error.msg.contains("最多 6 位小数"), "{}", error.msg);
    }

    /// 工时必须 > 0（0 工时会让分摊基数恒 0，最后除零）；机器工时可 0。
    #[test]
    fn hours_must_be_positive_machine_hours_may_be_zero() {
        let mut req = base_create();
        req.hours = Some(dec("0"));
        assert!(spec_from_create(&req, "车间").unwrap_err().msg.contains("必须大于 0"));

        let mut req = base_create();
        req.machine_hours = Some(dec("0"));
        assert_eq!(
            spec_from_create(&req, "车间").expect("机器工时可 0").machine_hours,
            Decimal::ZERO
        );

        let mut req = base_create();
        req.machine_hours = Some(dec("-1"));
        assert!(
            spec_from_create(&req, "车间")
                .unwrap_err()
                .msg
                .contains("machine_hours")
        );
    }

    /// **验收项**：客户端给了金额但与「工时×费率」不符 ⇒ 400 + 66058 + 人话。
    #[test]
    fn declared_amount_mismatch_is_66058() {
        let mut req = base_create();
        req.amount = Some(dec("401"));
        let error = spec_from_create(&req, "车间").expect_err("金额不符");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, 66058);
        assert!(error.msg.contains("工时×费率与金额不符"), "{}", error.msg);
        assert!(error.msg.contains("401"), "要点名拿到的金额：{}", error.msg);

        // 对得上就放行（8 × 50 = 400）
        let mut req = base_create();
        req.amount = Some(dec("400.0000"));
        assert_eq!(spec_from_create(&req, "车间").expect("金额对得上").amount, dec("400"));
    }

    /// **验收项**：库的 CHECK 触发的错误翻译成同一条码同一条文案。
    #[test]
    fn amount_check_violation_maps_to_400_66058() {
        let spec = spec_from_create(&base_create(), "车间").expect("建单");
        let error = map_write_error(
            DbError::CheckViolation {
                constraint: Some("ck_biz_cost_labor_amount".to_string()),
                source: None,
            },
            &spec,
            "测试",
        );
        assert_eq!(error.status, 400);
        assert_eq!(error.code, 66058);
        assert!(error.msg.contains("工时×费率与金额不符"), "{}", error.msg);
        assert!(
            !error.msg.contains("check constraint"),
            "不许把 PG 原文给用户：{}",
            error.msg
        );

        // 其它 CHECK（枚举 / 期间 / 工时）→ 400 + 10001，点名约束便于定位
        let error = map_write_error(
            DbError::CheckViolation {
                constraint: Some("ck_biz_cost_labor_period".to_string()),
                source: None,
            },
            &spec,
            "测试",
        );
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("ck_biz_cost_labor_period"), "{}", error.msg);

        // 其它错误仍是 500（真实原因只进日志）
        let error = map_write_error(DbError::InvalidValue("别的错".to_string()), &spec, "测试");
        assert_eq!(error.status, 500);
        assert_eq!(error.code, error_code::INTERNAL_ERROR);
    }

    /// 改单：部分更新合并 + 金额永远重算（改工时不同步金额也不会漏）。
    #[test]
    fn update_recomputes_amount_from_merged_values() {
        let current = spec_from_create(&base_create(), "机加工车间").expect("建单");
        assert_eq!(current.amount, dec("400"));

        // 只改工时：金额必须跟着变
        let req = CostLaborUpdateReq {
            hours: Some(dec("9.5")),
            ..Default::default()
        };
        let spec = spec_from_update(&current, &req).expect("改单");
        assert_eq!(spec.hours, dec("9.5"));
        assert_eq!(spec.amount, dec("475"), "9.5 × 50");
        assert_eq!(spec.rate, dec("50"), "没提的字段不动");

        // 只改费率：金额同样跟着变
        let req = CostLaborUpdateReq {
            rate: Some(dec("60")),
            ..Default::default()
        };
        let spec = spec_from_update(&current, &req).expect("改单");
        assert_eq!(spec.amount, dec("480"));
    }

    /// 改单的三态字段：显式 null 清空，没提不动。
    #[test]
    fn update_three_state_fields() {
        let mut create = base_create();
        create.employee_id = Some("018f4b2c-bbbb-7222-8333-444455556666".to_string());
        create.remark = Some("首件".to_string());
        let current = spec_from_create(&create, "车间").expect("建单");
        assert!(current.employee_id.is_some());

        let req = CostLaborUpdateReq {
            employee_id: Some(None),
            remark: Some(None),
            ..Default::default()
        };
        let spec = spec_from_update(&current, &req).expect("清空");
        assert_eq!(spec.employee_id, None, "显式 null 要清空");
        assert_eq!(spec.remark, None);
        assert_eq!(spec.amount, current.amount, "没改工时 / 费率 ⇒ 金额不变");
    }

    /// 改单也要重校验：日期 / 期间组合、精度、金额核对值。
    #[test]
    fn update_rechecks_period_and_amount() {
        let current = spec_from_create(&base_create(), "车间").expect("建单");

        // 把期间改到 12 月，但工作日期还是 11-06 ⇒ 400
        let req = CostLaborUpdateReq {
            period_code: Some("2026-12".to_string()),
            ..Default::default()
        };
        let error = spec_from_update(&current, &req).expect_err("跨月");
        assert!(error.msg.contains("不在所选期间"), "{}", error.msg);

        // 期间与日期一起改 ⇒ 通过
        let req = CostLaborUpdateReq {
            period_code: Some("2026-12".to_string()),
            work_date: Some("2026-12-03".to_string()),
            ..Default::default()
        };
        assert_eq!(spec_from_update(&current, &req).expect("同改").period_code, "2026-12");

        // 客户端给错金额 ⇒ 66058
        let req = CostLaborUpdateReq {
            amount: Some(dec("399")),
            ..Default::default()
        };
        let error = spec_from_update(&current, &req).expect_err("金额不符");
        assert_eq!(error.code, 66058);
    }

    /// **验收项**：确认幂等的判定（纯函数）。
    #[test]
    fn confirm_action_is_idempotent() {
        assert_eq!(confirm_action(STATUS_DRAFT), ConfirmAction::Flip);
        assert_eq!(confirm_action(STATUS_CONFIRMED), ConfirmAction::AlreadyConfirmed);
        // 重复确认走的是同一个判定，永远落 AlreadyConfirmed ⇒ 不会写第二次
        assert_eq!(confirm_action(STATUS_CONFIRMED), ConfirmAction::AlreadyConfirmed);
    }

    /// 非 DRAFT 的改 / 删文案要点明「怎么修」。
    #[test]
    fn wrong_status_message_says_how_to_fix() {
        let error = wrong_status("修改", STATUS_CONFIRMED);
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("DRAFT"), "{}", error.msg);
        assert!(error.msg.contains("重新录入"), "{}", error.msg);
    }

    /// 工作中心名快照长度 / 空白。
    #[test]
    fn dept_name_is_validated() {
        let mut req = base_create();
        req.hours = Some(dec("1"));
        assert!(spec_from_create(&req, "   ").unwrap_err().msg.contains("dept_name"));
        assert!(
            spec_from_create(&req, &"车".repeat(DEPT_NAME_MAX + 1))
                .unwrap_err()
                .msg
                .contains("dept_name")
        );
    }
}
