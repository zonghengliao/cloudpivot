//! 分摊规则契约层（成本核算 · ARCH-9 §3.1 ⑦ / §4.4 / §5.2 #20–#24 / §5.3）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
//!
//! | 章节 | 本模块用到的地方 |
//! |---|---|
//! | §3.1 ⑦ | `biz_cost_alloc_rule` 表结构与 5 条 CHECK（`V0372` 逐字落地） |
//! | §3.1 ⑧ | `biz_cost_alloc_result`（试算结果**不落库**，但字段口径对齐它） |
//! | §4.4.1 | 规则语义：「X 车间的 Y 制造费用科目，按 Z 标准，摊给该车间当期在产的这批对象」 |
//! | §4.4.2 | 四种分配标准的基数定义（本模块 [`ObjectBasis`] / [`basis_value`]） |
//! | §4.4.3 | 池 / 基数 / 最大余数法 / 守恒断言（本模块 [`allocate`]） |
//! | §4.4.4 | 尾差为什么必须只落在唯一一行（`rounding_adj`） |
//! | §4.4.5 | 版本化：改规则不溯及历史（生效期选版本，见 [`pick_latest_version`]） |
//! | §5.2 #20–#24 | 5 个接口的入出参形状 |
//!
//! # 三条口径（本模块的全部规则）
//!
//! 1. **规则是版本化的**：`(source_dept_id, account_code)` 是「规则族」的键，
//!    `effective_from` 更晚的版本赢。算某个期间时先按「与该期间有交集」筛出候选，
//!    再取 `effective_from` 最晚的一条（同 `cost_rate_dto` 的「生效更晚版本赢」口径）。
//!    这样「11 月起水电费改按机器工时分摊」只是新增一条 `effective_from = 2026-11-01`
//!    的规则，10 月重算仍然拿旧版本，**历史不会被改口径**。
//! 2. **池金额按 (车间, 科目) 分组**：设计 §4.4.3 ① 写的是
//!    `collect.account_code`，但 §3.1 ③ 的 DDL 里 `biz_cost_collect` **没有** 这一列
//!    （`V0371` 也没有）。落地口径：池行的科目由**来源表反查**
//!    （`OVERHEAD_TICKET → biz_cost_overhead.debit_account_code`、
//!    `DEPRECIATION → biz_depreciation.expense_account_code`、
//!    `EXPENSE → biz_expense_item.account_code`）。反查不到的池行**不静默丢弃**：
//!    由 [`crate::business::erp::cost_alloc_rule_model::unassigned_pool`] 单独统计，
//!    handler 把它作为诊断返回（见卡内汇报的偏差项 ①）。
//! 3. **分摊尾差用最大余数法**：先对 `raw_i = pool × basis_i / basis_total` 截断到
//!    4 位（`alloc_i`），把 `pool - Σ alloc_i` 全额记到**唯一一行**（余数最大者；
//!    并列取基数大者，再并列取 `cost_object_id` 字典序最小），即 `rounding_adj`。
//!    守恒 `Σ alloc_i = pool` 是硬断言，不满足直接报错（不是「差不多就行」）。
//!
//! # 为什么「没有在产对象 / 基数全 0」是硬错误
//!
//! §4.4.3 明写「不做『挂在池里留到下期』的静默处理（那正是上期漏摊的来源）」。
//! 试算看见「池 4000、摊 0」而界面不报错，用户会以为算完了 —— 结算时 4101 余额
//! 清不掉，C-COST-1 巡检天天红，最后只能改库。所以：
//!
//! * 池 > 0 且当期无参与分摊的对象 → `409` + [`error_code::ERP_COST_ALLOC_NO_OBJECT`]；
//! * 池 > 0 且该标准下基数合计为 0 → `409` + [`error_code::ERP_COST_ALLOC_BASIS_ZERO`]
//!   （守恒不可能成立，例如「水电按机器工时分摊」但当期没人填机器工时）。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::cost_labor_dto;
use crate::shared::error_code;

// ==================== 值域（与 V0372 的 CHECK 逐字一致） ====================

/// 业务类型：制造费用分摊（`ck_biz_cost_rule_biz` 一期只允许它）。
pub const BIZ_TYPE_OVERHEAD: &str = "OVERHEAD";

/// 业务类型值域（三期加 `STEP_TRANSFER` 时必须新迁移重建 CHECK）。
pub const BIZ_TYPES: [&str; 1] = [BIZ_TYPE_OVERHEAD];

/// 分配标准：人工工时。
pub const BASIS_LABOR_HOURS: &str = "LABOR_HOURS";
/// 分配标准：机器工时。
pub const BASIS_MACHINE_HOURS: &str = "MACHINE_HOURS";
/// 分配标准：本期完工数量（成本计算单）。
pub const BASIS_OUTPUT_QTY: &str = "OUTPUT_QTY";
/// 分配标准：材料成本。
pub const BASIS_MATERIAL_COST: &str = "MATERIAL_COST";

/// 分配标准值域（`ck_biz_cost_rule_basis`，四种，一种一个单测）。
pub const BASES: [&str; 4] = [
    BASIS_LABOR_HOURS,
    BASIS_MACHINE_HOURS,
    BASIS_OUTPUT_QTY,
    BASIS_MATERIAL_COST,
];

/// 启用状态（`ck_biz_cost_rule_status`：`1` 启用 / `0` 停用）。
pub const STATUS_ACTIVE: i16 = 1;
/// 停用状态。
pub const STATUS_DISABLED: i16 = 0;
/// 状态值域。
pub const STATUSES: [i16; 2] = [STATUS_ACTIVE, STATUS_DISABLED];

/// 制造费用科目前缀（`ck_biz_cost_rule_account`：`account_code LIKE '4101.%'`）。
pub const ACCOUNT_PREFIX: &str = "4101.";

/// 金额 / 基数的标度（`NUMERIC(18,4)`，与 §4.4.3「先截断到分（4 位）」一致）。
pub const AMOUNT_SCALE: u32 = 4;
/// 标度因子（`10^4`），截断用整数运算避免浮点。
pub const SCALE_FACTOR: i64 = 10_000;

/// 规则编码长度上限（`rule_code VARCHAR(32)`）。
pub const RULE_CODE_MAX: usize = 32;
/// 规则名长度上限（`rule_name VARCHAR(120)`）。
pub const RULE_NAME_MAX: usize = 120;
/// 车间名快照长度上限（`source_dept_name VARCHAR(120)`）。
pub const DEPT_NAME_MAX: usize = 120;
/// 科目编码长度上限（`account_code VARCHAR(30)`）。
pub const ACCOUNT_CODE_MAX: usize = 30;
/// 备注长度上限（`remark VARCHAR(255)`）。
pub const REMARK_MAX: usize = 255;
/// 期间长度（`yyyy-MM`）。
pub const PERIOD_LEN: usize = 7;

/// 优先级默认值（`priority INT NOT NULL DEFAULT 10`）。
pub const PRIORITY_DEFAULT: i32 = 10;
/// 优先级下限。
pub const PRIORITY_MIN: i32 = 1;
/// 优先级上限（只用于挡明显的手滑：`0` / 负数 / 极大值）。
pub const PRIORITY_MAX: i32 = 999;

// ==================== 入参 ====================

/// 规则列表查询（§5.2 #20）。
///
/// 设计没逐条列筛选字段；按「财务配规则时要怎么找」给 6 个：
/// 期间（= 只看该期间生效的版本）/ 车间 / 科目 / 分配标准 / 状态 / 关键字。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostAllocRulePageQuery {
    /// 会计期间 `yyyy-MM`：只返回**生效期覆盖该期间**的规则（版本选择视角）。
    pub period_code: Option<String>,
    /// 被摊车间。
    pub dept_id: Option<String>,
    /// 被摊科目（`4101.*`）。
    pub account_code: Option<String>,
    /// 分配标准（`LABOR_HOURS` / …）。
    pub basis: Option<String>,
    /// 状态（`1` 启用 / `0` 停用）。
    pub status: Option<i16>,
    /// 关键字：规则编码 / 规则名 / 车间名 / 科目编码 / 科目名 / 备注（模糊）。
    pub keyword: Option<String>,
    /// 页码（1 起）。
    pub page: Option<u64>,
    /// 每页条数（0 = 默认 20，上限 200）。
    pub page_size: Option<u64>,
}

/// 归一化后的列表筛选（model 只认这个）。
#[derive(Debug, Clone)]
pub struct CostAllocRuleFilter {
    /// 会计期间（原样回显用）。
    pub period_code: Option<String>,
    /// 期间首日（生效期交集判定用；`period_code` 为空时也是 `None`）。
    pub period_from: Option<NaiveDate>,
    /// 期间末日。
    pub period_to: Option<NaiveDate>,
    /// 车间。
    pub dept_id: Option<Uuid>,
    /// 科目。
    pub account_code: Option<String>,
    /// 分配标准。
    pub basis: Option<String>,
    /// 状态。
    pub status: Option<i16>,
    /// 关键字。
    pub keyword: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 新建分摊规则（§5.2 #21）。
///
/// `source_dept_name` 不在入参里：车间名是**快照**，由服务端从 `sys_dept` 取名写入
/// （允许客户端写快照 = 允许伪造归属，与工时 / 费用台账同一条纪律）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostAllocRuleCreateReq {
    /// 规则编码（唯一，大写字母 / 数字 / `_` / `-` / `.`）。
    pub rule_code: Option<String>,
    /// 规则名。
    pub rule_name: Option<String>,
    /// 被摊车间。
    pub dept_id: Option<String>,
    /// 被摊科目（必须 `4101.*`）。
    pub account_code: Option<String>,
    /// 分配标准（四选一）。
    pub basis: Option<String>,
    /// 优先级（缺省 10）。
    pub priority: Option<i32>,
    /// 生效开始日（`YYYY-MM-DD`，必填）。
    pub effective_from: Option<String>,
    /// 生效结束日（可空 = 长期有效）。
    pub effective_to: Option<String>,
    /// 状态（缺省 1 启用）。
    pub status: Option<i16>,
    /// 备注。
    pub remark: Option<String>,
}

/// 修改分摊规则（§5.2 #22）。
///
/// 所有字段 `Option`：**缺省 = 不改**。`effective_to` 传空串 = 清空（改成长期有效），
/// 传 `null` / 不传 = 不改 —— 三态在这里只能退化成「空串清空」，
/// 因为 `serde` 的 `Option<String>` 分不出「没传」和「传了 null」，
/// 而规则界面每次都会把整行表单提交上来（`effective_to` 输入框清空就是空串）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostAllocRuleUpdateReq {
    /// 规则编码。
    pub rule_code: Option<String>,
    /// 规则名。
    pub rule_name: Option<String>,
    /// 被摊车间。
    pub dept_id: Option<String>,
    /// 被摊科目。
    pub account_code: Option<String>,
    /// 分配标准。
    pub basis: Option<String>,
    /// 优先级。
    pub priority: Option<i32>,
    /// 生效开始日。
    pub effective_from: Option<String>,
    /// 生效结束日（空串 = 清空）。
    pub effective_to: Option<String>,
    /// 状态（`1` 启用 / `0` 停用）。
    pub status: Option<i16>,
    /// 备注（空串 = 清空）。
    pub remark: Option<String>,
}

/// 试算入参（§5.2 #24）：给期间 + 池金额，返回各对象分摊额，**不落库**。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostAllocPreviewReq {
    /// 会计期间 `yyyy-MM`（必填）。
    pub period_code: Option<String>,
    /// 只算这几条规则（空 = 该期间生效的全部规则）。
    pub rule_ids: Option<Vec<String>>,
}

// ==================== 归一化规格 ====================

/// 归一化后的规则规格（dto 与 model 之间唯一的形状）。
#[derive(Debug, Clone, PartialEq)]
pub struct CostAllocRuleSpec {
    /// 规则编码。
    pub rule_code: String,
    /// 规则名。
    pub rule_name: String,
    /// 业务类型（本卡恒 `OVERHEAD`）。
    pub biz_type: String,
    /// 被摊车间。
    pub source_dept_id: Uuid,
    /// 车间名（快照；handler 查库取名）。
    pub source_dept_name: String,
    /// 被摊科目。
    pub account_code: String,
    /// 分配标准。
    pub basis: String,
    /// 优先级。
    pub priority: i32,
    /// 生效开始日。
    pub effective_from: NaiveDate,
    /// 生效结束日。
    pub effective_to: Option<NaiveDate>,
    /// 状态。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
}

// ==================== 出参 ====================

/// 规则列表行（`*_id` 都带 `*_name` —— AGENTS 约定 13）。
#[derive(Debug, Clone, Serialize)]
pub struct CostAllocRuleListRes {
    /// 主键。
    pub id: String,
    /// 规则编码。
    pub rule_code: String,
    /// 规则名。
    pub rule_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 被摊车间。
    pub source_dept_id: String,
    /// 车间名（`sys_dept` 实时名优先，落空回落到建规则快照）。
    pub source_dept_name: Option<String>,
    /// 被摊科目编码。
    pub account_code: String,
    /// 科目名（账套实时名）。
    pub account_name: Option<String>,
    /// 分配标准编码。
    pub basis: String,
    /// 分配标准中文说明（「按人工工时分摊」）。
    pub basis_label: String,
    /// 优先级。
    pub priority: i32,
    /// 生效开始日（`YYYY-MM-DD`）。
    pub effective_from: String,
    /// 生效结束日（空 = 长期有效）。
    pub effective_to: Option<String>,
    /// 状态 `1` / `0`。
    pub status: i16,
    /// 状态中文。
    pub status_label: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
    /// 更新时间（RFC3339）。
    pub updated_at: String,
}

/// 试算里的一行（= `biz_cost_alloc_result` 一行的字段口径，但不落库）。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CostAllocPreviewLine {
    /// 成本对象。
    pub cost_object_id: String,
    /// 成本对象号。
    pub cost_object_no: String,
    /// 成本对象状态（`OPEN` / `CLOSED`）。
    pub cost_object_status: String,
    /// 本对象在该标准下的基数（字符串，防 JS 精度丢失）。
    pub basis_value: String,
    /// 摊到本对象的金额（含尾差）。
    pub alloc_amount: String,
    /// 尾差调整（最大余数法：**只有一行**非 0）。
    pub rounding_adj: String,
}

/// 一条规则的试算结果。
#[derive(Debug, Clone, Serialize)]
pub struct CostAllocPreviewGroup {
    /// 规则主键。
    pub rule_id: String,
    /// 规则编码。
    pub rule_code: String,
    /// 规则名。
    pub rule_name: String,
    /// 被摊车间。
    pub source_dept_id: String,
    /// 车间名。
    pub source_dept_name: String,
    /// 被摊科目。
    pub account_code: String,
    /// 科目名。
    pub account_name: Option<String>,
    /// 分配标准。
    pub basis: String,
    /// 分配标准中文说明。
    pub basis_label: String,
    /// 待分摊池金额（字符串）。
    pub pool_amount: String,
    /// 全车间基数合计（字符串）。
    pub basis_total: String,
    /// 本组分摊合计（= 池，守恒）。
    pub alloc_total: String,
    /// 本组尾差合计（= 池 − Σ截断值）。
    pub rounding_total: String,
    /// 逐对象明细。
    pub rows: Vec<CostAllocPreviewLine>,
}

/// 「有池、但当期没有生效规则能摊」的池行（诊断用，**不静默吞掉**）。
#[derive(Debug, Clone, Serialize)]
pub struct CostAllocPreviewUnallocated {
    /// 车间。
    pub source_dept_id: Option<String>,
    /// 车间名。
    pub source_dept_name: Option<String>,
    /// 科目编码（未知为 `None`）。
    pub account_code: Option<String>,
    /// 科目名。
    pub account_name: Option<String>,
    /// 未分摊金额（字符串）。
    pub pool_amount: String,
    /// 为什么没摊（人话）。
    pub reason: String,
}

/// 试算出参（§5.2 #24：`{period_code}` + 逐对象 `basis_value/basis_total/pool_amount/alloc_amount/rounding_adj`）。
#[derive(Debug, Clone, Serialize)]
pub struct CostAllocPreviewRes {
    /// 会计期间。
    pub period_code: String,
    /// 逐规则结果（每条规则一组）。
    pub groups: Vec<CostAllocPreviewGroup>,
    /// 已摊池合计（= 各组 `pool_amount` 之和）。
    pub total_pool_amount: String,
    /// 已摊金额合计（= 各组 `alloc_total` 之和，守恒时等于上一项）。
    pub total_alloc_amount: String,
    /// 未分摊池（没规则覆盖 / 反查不到科目 / 无在产对象被挡下的不在此列——那些直接报错）。
    pub unallocated: Vec<CostAllocPreviewUnallocated>,
}

// ==================== 错误构造 ====================

/// 400 构造（参数类）。
pub fn bad_request(message: impl Into<String>) -> ApiError {
    cost_labor_dto::bad_request(message)
}

/// 409 构造（业务规则拒绝）。
pub fn conflict(message: impl Into<String>) -> ApiError {
    cost_labor_dto::conflict(message)
}

/// 404 构造（规则不存在 / 已软删）。
pub fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("分摊规则不存在或已删除（{id}）"),
    )
}

/// 400 + `66060`：科目不是制造费用科目 / 不存在 / 非末级 / 停用。
///
/// **复用费用台账的 `66060`**：动作完全相同（换一个 `4101` 下的启用末级科目），
/// 前端只需要一条引导文案；原因写进报文（点名编码 + 账套现状）。
pub fn account_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_COST_OVERHEAD_ACCOUNT, message.into())
}

/// 400 + `66060`：科目不在 `4101.*`。
pub fn account_out_of_scope(code: &str) -> ApiError {
    account_invalid(format!(
        "account_code「{code}」不是制造费用科目：分摊规则只摊 `4101.*` 下面的费用\
         （如 4101.01 折旧费 / 4101.02 水电费 / 4101.09 其他）。\n\
         为什么：分摊规则的语义是「把某个制造费用科目的余额按标准摊给在产对象」，\
         科目不在 4101 下面意味着这笔钱本来就不是制造费用，摊进去会让产品成本虚增"
    ))
}

/// 400 + `66060`：科目不存在 / 非末级 / 停用（三种原因共用一码，报文点名）。
pub fn account_state_invalid(code: &str, name: Option<&str>, reason: &str) -> ApiError {
    let named = match name {
        Some(name) => format!("「{code} {name}」"),
        None => format!("「{code}」"),
    };
    account_invalid(format!(
        "account_code{} {reason}。\n\
         分摊规则要摊的是账上真实存在的制造费用末级科目：汇总科目不能记账、\
         停用科目不许再记账、编码拼错则在账套里找不到 —— 这三种到结算时才炸，\
         会让整批成本算不出来。请到「科目管理」确认后重选",
        named
    ))
}

/// 409：规则编码已存在（`uk_biz_cost_rule_code`）。
pub fn rule_code_taken(code: &str) -> ApiError {
    conflict(format!(
        "规则编码「{code}」已被占用（规则编码在未删除的规则里唯一）。\n\
         请换一个编码，或先把旧规则删掉（软删后编码可复用）"
    ))
}

/// 409：同一 (车间, 科目) 同一天已有生效版本。
pub fn version_duplicated(code: &str, effective_from: NaiveDate) -> ApiError {
    conflict(format!(
        "车间 + 科目 + 生效开始日 {effective_from} 已经有一条规则「{code}」。\n\
         同一 (车间, 科目, 生效日) 只能有一条版本 —— 否则算某个期间时「取哪个版本」\
         没有确定答案。要改口径请把生效日挪到下个月，或先停用旧版本"
    ))
}

/// 409：已被分摊结果引用的规则不许改口径 / 删。
pub fn rule_in_use(periods: &str) -> ApiError {
    conflict(format!(
        "这条规则已经参与过期间 {periods} 的分摊（`biz_cost_alloc_result` 有引用行）。\n\
         规则是版本化的：改口径不能往回改已用过的版本（那等于篡改历史成本）。\n\
         正确做法是**新建一条规则**，把生效开始日设成下一期（旧版本继续为历史期间服务）"
    ))
}

/// 409 + `66062`：池有金额，但当期没有参与分摊的成本对象（§4.4.3 明写要报错）。
pub fn no_cost_object(dept_name: &str, account_code: &str, pool: Decimal) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_ALLOC_NO_OBJECT,
        format!(
            "车间「{dept_name}」科目 {account_code} 有 {pool} 待分摊，但当期没有在产成本对象。\n\
             参与分摊的条件是：对象属于该车间、状态在制或已关批、且当期有料或工的投入。\n\
             系统不做「留在池里下期再摊」的静默处理 —— 那正是上期漏摊、
             4101 余额永远清不掉的来源。请先补当期的领料 / 工时，或确认该车间当月确实没生产\
             （没生产就把这笔费用记到别的期间或别的车间）"
        ),
    )
}

/// 409 + `66063`：池有金额，但该分配标准下基数合计为 0（守恒不可能成立）。
pub fn basis_total_zero(basis: &str, dept_name: &str, account_code: &str, pool: Decimal) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_ALLOC_BASIS_ZERO,
        format!(
            "车间「{dept_name}」科目 {account_code} 有 {pool} 待分摊，但分配标准 {} 的基数合计为 0，\
             无法按比例摊（分摊额全 0 会让池里永远剩 {pool}）。\n\
             请检查当期该车间是否真的没有{}；如果这个标准本就不适用，\
             请把规则改成其它标准（人工工时 / 机器工时 / 本期完工数量 / 材料成本）",
            basis_label(basis),
            basis_requirement(basis)
        ),
    )
}

/// 写库错误 → 友好 `ApiError`（**唯一的错误出口**；判据走 `DbError` 变体）。
pub fn map_write_error(err: DbError, spec: &CostAllocRuleSpec, ctx: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => match constraint.as_deref() {
            Some("uk_biz_cost_rule_code") => rule_code_taken(&spec.rule_code),
            other => conflict(format!(
                "分摊规则写入冲突（唯一约束{}），请刷新后重试",
                other.map(|name| format!(" {name}")).unwrap_or_default()
            )),
        },
        DbError::CheckViolation { constraint, .. } => match constraint.as_deref() {
            Some("ck_biz_cost_rule_account") => account_out_of_scope(&spec.account_code),
            Some("ck_biz_cost_rule_dates") => bad_request(format!(
                "生效结束日（{}）不能早于生效开始日（{}）",
                spec.effective_to
                    .map(|date| date.to_string())
                    .unwrap_or_else(|| "未填".to_string()),
                spec.effective_from
            )),
            other => ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!(
                    "数据不满足数据库约束{}（业务类型 / 分配标准 / 状态值域）",
                    other.map(|name| format!(" {name}")).unwrap_or_default()
                ),
            ),
        },
        _ => crate::utils::db_error(err, ctx),
    }
}

// ==================== 文案 ====================

/// 分配标准的中文说明（前端下拉 + 试算表头用，§6.2.3「带一句解释」）。
pub fn basis_label(code: &str) -> &'static str {
    match code {
        BASIS_LABOR_HOURS => "按人工工时分摊",
        BASIS_MACHINE_HOURS => "按机器工时分摊",
        BASIS_OUTPUT_QTY => "按本期完工数量分摊",
        BASIS_MATERIAL_COST => "按材料成本分摊",
        _ => "未知分配标准",
    }
}

/// 该标准要求「当期有什么数据」（报错文案用）。
pub fn basis_requirement(code: &str) -> &'static str {
    match code {
        BASIS_LABOR_HOURS => "已确认的工时记录（工时台账 status=CONFIRMED）",
        BASIS_MACHINE_HOURS => "已确认的机器工时（工时台账的 machine_hours 列）",
        BASIS_OUTPUT_QTY => "成本计算单上的本期完工数量（要先算过该期间的 calc）",
        BASIS_MATERIAL_COST => "料要素的归集金额（当期要有已过账的领料）",
        _ => "对应基数数据",
    }
}

/// 状态中文。
pub fn status_label(status: i16) -> &'static str {
    if status == STATUS_ACTIVE { "启用" } else { "停用" }
}

/// 金额 / 基数出参：**固定 4 位小数**的字符串。
///
/// 为什么固定标度而不是 `Decimal::to_string()`：`to_string()` 会把 `2400.0000`
/// 打成 `2400`，前端「合计 = 池」这类字面比较与表格对齐都会抖。`NUMERIC(18,4)`
/// 的标度就是 4，出参按列标度出即可（与其它成本模块的金额出参口径一致）。
pub fn money4(value: Decimal) -> String {
    format!("{value:.4}")
}

// ==================== 纯校验 ====================

/// 分配标准归一化（大写 + 白名单）。
pub fn normalize_basis(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request(format!("basis（分配标准）不能为空，只认 {:?}", BASES)))?
        .to_ascii_uppercase();
    if !BASES.contains(&text.as_str()) {
        let listed = BASES
            .iter()
            .map(|code| format!("{code}（{}）", basis_label(code)))
            .collect::<Vec<_>>()
            .join(" / ");
        return Err(bad_request(format!("basis（分配标准）只认 {listed}（收到「{text}」）")));
    }
    Ok(text)
}

/// 规则编码归一化（trim + 大写 + 字符集 + 长度）。
///
/// 为什么强制大写：`uk_biz_cost_rule_code` 是大小写敏感的普通唯一索引，
/// `WL-001` 与 `wl-001` 会被当成两条规则 —— 同一件事两个编码，用户找不回来。
pub fn normalize_rule_code(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("rule_code（规则编码）不能为空"))?
        .to_ascii_uppercase();
    if text.chars().count() > RULE_CODE_MAX {
        return Err(bad_request(format!(
            "rule_code（规则编码）最长 {RULE_CODE_MAX} 个字符（收到 {} 个）",
            text.chars().count()
        )));
    }
    if !text
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-' | '.'))
    {
        return Err(bad_request(format!(
            "rule_code（规则编码）只允许大写字母、数字与 `_ - .`（收到「{text}」）"
        )));
    }
    Ok(text)
}

/// 规则名归一化（非空 + 长度）。
pub fn normalize_rule_name(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("rule_name（规则名）不能为空"))?;
    if text.chars().count() > RULE_NAME_MAX {
        return Err(bad_request(format!(
            "rule_name（规则名）最长 {RULE_NAME_MAX} 个字符（收到 {} 个）",
            text.chars().count()
        )));
    }
    Ok(text.to_string())
}

/// 科目编码：清洗 + `4101.*`（**只做纯格式判定**，账套现状由 handler 查库）。
pub fn normalize_account_code(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("account_code（被摊科目）不能为空"))?;
    if text.chars().count() > ACCOUNT_CODE_MAX {
        return Err(bad_request(format!(
            "account_code 最长 {ACCOUNT_CODE_MAX} 个字符（收到 {} 个）",
            text.chars().count()
        )));
    }
    if !is_manufacturing_account(text) {
        return Err(account_out_of_scope(text));
    }
    Ok(text.to_string())
}

/// `4101.*` 判定（与 `ck_biz_cost_rule_account CHECK (account_code LIKE '4101.%')` 等价，
/// 且额外要求分段都是数字，挡掉 `4101.abc` 这种「前缀对但账套里不可能存在」的输入）。
pub fn is_manufacturing_account(code: &str) -> bool {
    let Some(rest) = code.strip_prefix(ACCOUNT_PREFIX) else {
        return false;
    };
    !rest.is_empty()
        && rest
            .split('.')
            .all(|segment| !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit()))
}

/// 优先级归一化（缺省 10，范围 1..=999）。
pub fn normalize_priority(raw: Option<i32>) -> std::result::Result<i32, ApiError> {
    let value = raw.unwrap_or(PRIORITY_DEFAULT);
    if !(PRIORITY_MIN..=PRIORITY_MAX).contains(&value) {
        return Err(bad_request(format!(
            "priority（优先级）必须在 {PRIORITY_MIN}–{PRIORITY_MAX} 之间（收到 {value}）；\
             数字小的先摊，缺省 {PRIORITY_DEFAULT}"
        )));
    }
    Ok(value)
}

/// 状态归一化（缺省 1 启用）。
pub fn normalize_status(raw: Option<i16>) -> std::result::Result<i16, ApiError> {
    let value = raw.unwrap_or(STATUS_ACTIVE);
    if !STATUSES.contains(&value) {
        return Err(bad_request(format!("status 只认 1（启用）/ 0（停用）（收到 {value}）")));
    }
    Ok(value)
}

/// 备注清洗（空串 → `None`）+ 长度。
pub fn normalize_remark(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if text.chars().count() > REMARK_MAX {
        return Err(bad_request(format!(
            "remark（备注）最长 {REMARK_MAX} 个字符（收到 {} 个）",
            text.chars().count()
        )));
    }
    Ok(Some(text.to_string()))
}

/// 生效期区间校验（`ck_biz_cost_rule_dates`：结束日不早于开始日）。
pub fn ensure_effective_range(from: NaiveDate, to: Option<NaiveDate>) -> std::result::Result<(), ApiError> {
    if let Some(to) = to
        && to < from
    {
        return Err(bad_request(format!("生效结束日（{to}）不能早于生效开始日（{from}）")));
    }
    Ok(())
}

// ==================== 期间（版本选择的时间轴） ====================

/// 会计期间 → (首日, 末日)。
pub fn period_bounds(period_code: &str) -> std::result::Result<(NaiveDate, NaiveDate), ApiError> {
    let period = cost_labor_dto::normalize_period(Some(period_code))?;
    let year: i32 = period[..4]
        .parse()
        .map_err(|_| bad_request(format!("period_code 的年份不合法（{period}）")))?;
    let month: u32 = period[5..7]
        .parse()
        .map_err(|_| bad_request(format!("period_code 的月份不合法（{period}）")))?;
    let first = NaiveDate::from_ymd_opt(year, month, 1)
        .ok_or_else(|| bad_request(format!("period_code 不是合法期间（{period}）")))?;
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let next_first = NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .ok_or_else(|| bad_request(format!("period_code 不是合法期间（{period}）")))?;
    let last = next_first
        .pred_opt()
        .ok_or_else(|| bad_request(format!("period_code 不是合法期间（{period}）")))?;
    Ok((first, last))
}

/// 规则版本（版本选择只需要这几个字段，纯逻辑可单测）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleVersion {
    /// 规则主键。
    pub rule_id: Uuid,
    /// 规则编码（并列时的确定性 tiebreak）。
    pub rule_code: String,
    /// 状态（停用永不参与）。
    pub status: i16,
    /// 生效开始日。
    pub effective_from: NaiveDate,
    /// 生效结束日。
    pub effective_to: Option<NaiveDate>,
    /// 优先级（数字小的先摊）。
    pub priority: i32,
}

impl RuleVersion {
    /// 生效期是否与该期间**有交集**（两端都含）。
    ///
    /// 期间是月粒度、生效期是日粒度：`effective_from = 2026-11-15` 的规则
    /// 对 `2026-11` 这一期算生效（本月改口径，本月就按新口径摊），
    /// 对 `2026-10` 不算（上月没这条版本）。
    pub fn covers_period(&self, from: NaiveDate, to: NaiveDate) -> bool {
        self.effective_from <= to && self.effective_to.is_none_or(|end| end >= from)
    }

    /// 是否参与版本选择（启用 + 覆盖该期间）。
    pub fn eligible(&self, from: NaiveDate, to: NaiveDate) -> bool {
        self.status == STATUS_ACTIVE && self.covers_period(from, to)
    }
}

/// 从同一条规则族的多个版本里选出「该期间适用」的那一条。
///
/// 口径（§4.4.1「按期间取生效版本」+ `cost_rate_dto` 的「生效更晚版本赢」）：
/// 1. 只认启用且覆盖该期间的版本；
/// 2. 取 `effective_from` 最晚的；
/// 3. 并列时取 `rule_code` 字典序最小的（确定性，避免同一天两条版本时结果随行序漂移）。
pub fn pick_latest_version(versions: &[RuleVersion], from: NaiveDate, to: NaiveDate) -> Option<&RuleVersion> {
    versions
        .iter()
        .filter(|version| version.eligible(from, to))
        .min_by(|left, right| {
            right
                .effective_from
                .cmp(&left.effective_from)
                .then_with(|| left.rule_code.cmp(&right.rule_code))
        })
}

// ==================== 基数（四种分配标准） ====================

/// 一个成本对象在某期间的四个候选基数（都从库里聚合好，纯逻辑只负责「取哪一个」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObjectBasis {
    /// `Σ biz_cost_labor.hours`（只算 `CONFIRMED`）。
    pub labor_hours: Decimal,
    /// `Σ biz_cost_labor.machine_hours`（只算 `CONFIRMED`）。
    pub machine_hours: Decimal,
    /// `biz_cost_period_item.completed_qty`（本期完工数量）。
    pub completed_qty: Decimal,
    /// `Σ biz_cost_collect.amount WHERE element='MATERIAL'`。
    pub material_cost: Decimal,
}

/// 按分配标准取基数（§4.4.2 的四行表格，一行一个分支 —— **每个分支都有单测**）。
///
/// 未知标准返回 `Decimal::ZERO`：走到这里说明库里存了值域外的脏数据，
/// 报错文案由调用方给（此处不 panic，避免试算整批 500）。
pub fn basis_value(basis: &str, object: &ObjectBasis) -> Decimal {
    match basis {
        BASIS_LABOR_HOURS => object.labor_hours,
        BASIS_MACHINE_HOURS => object.machine_hours,
        BASIS_OUTPUT_QTY => object.completed_qty,
        BASIS_MATERIAL_COST => object.material_cost,
        _ => Decimal::ZERO,
    }
}

/// 一个参与分摊的对象（基数已算好）。
#[derive(Debug, Clone, PartialEq)]
pub struct AllocObject {
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 对象号（回显）。
    pub cost_object_no: String,
    /// 对象状态（`OPEN` / `CLOSED`）。
    pub cost_object_status: String,
    /// 该标准下的基数。
    pub basis_value: Decimal,
}

/// 分摊结果的一行。
#[derive(Debug, Clone, PartialEq)]
pub struct AllocLine {
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 对象号。
    pub cost_object_no: String,
    /// 对象状态。
    pub cost_object_status: String,
    /// 基数。
    pub basis_value: Decimal,
    /// 摊到本对象的金额（含尾差）。
    pub alloc_amount: Decimal,
    /// 尾差调整（只有一行非 0）。
    pub rounding_adj: Decimal,
}

/// 分摊结果。
#[derive(Debug, Clone, PartialEq)]
pub struct AllocOutcome {
    /// 待分摊池。
    pub pool_amount: Decimal,
    /// 基数合计。
    pub basis_total: Decimal,
    /// 逐对象结果（按入参顺序）。
    pub lines: Vec<AllocLine>,
}

/// 4 位小数截断（**不是四舍五入**，§4.4.3 ③「先截断到分」）。
///
/// 用整数乘法而不是 `trunc_with_scale`：行为显式、不依赖 Decimal 的舍入策略设置。
pub fn trunc4(value: Decimal) -> Decimal {
    let scaled = value * Decimal::from(SCALE_FACTOR);
    let truncated = scaled.trunc();
    truncated / Decimal::from(SCALE_FACTOR)
}

/// 最大余数法分摊（§4.4.3 ①–④）。
///
/// ```text
/// raw_i       = pool × basis_i / basis_total
/// alloc_i     = trunc4(raw_i)
/// shortfall   = pool − Σ alloc_i                     （≥ 0，且 < 0.0001 × n）
/// alloc_k    += shortfall    k = 余数最大者（并列取基数大者，再并列取 id 字典序最小）
/// rounding_adj_i = (i == k ? shortfall : 0)
/// ```
///
/// 硬断言：`Σ alloc_i = pool` 且**只有一行** `rounding_adj ≠ 0`。
pub fn allocate(pool: Decimal, objects: &[AllocObject]) -> std::result::Result<AllocOutcome, ApiError> {
    if pool < Decimal::ZERO {
        return Err(bad_request(format!(
            "待分摊池金额不能为负（收到 {pool}）；池金额是归集金额之和，恒 ≥ 0"
        )));
    }
    let mut basis_total = Decimal::ZERO;
    for object in objects {
        if object.basis_value < Decimal::ZERO {
            return Err(bad_request(format!(
                "成本对象「{}」的基数不能为负（收到 {}）",
                object.cost_object_no, object.basis_value
            )));
        }
        basis_total += object.basis_value;
    }
    let mut lines = objects
        .iter()
        .map(|object| AllocLine {
            cost_object_id: object.cost_object_id,
            cost_object_no: object.cost_object_no.clone(),
            cost_object_status: object.cost_object_status.clone(),
            basis_value: object.basis_value,
            alloc_amount: Decimal::ZERO,
            rounding_adj: Decimal::ZERO,
        })
        .collect::<Vec<_>>();
    if pool == Decimal::ZERO {
        // 池空：全都 0，无需守恒调整（也没有「谁多拿 0.0001」的问题）
        return Ok(AllocOutcome {
            pool_amount: pool,
            basis_total,
            lines,
        });
    }
    if lines.is_empty() {
        // 车间 / 科目的名字由 caller 补进文案（这里只给「没有对象」这个事实）
        return Err(no_cost_object("", "", pool));
    }
    if basis_total == Decimal::ZERO {
        return Err(basis_total_zero("", "", "", pool));
    }

    let mut allocated = Decimal::ZERO;
    let mut remainders: Vec<Decimal> = Vec::with_capacity(lines.len());
    for line in lines.iter_mut() {
        let raw = pool * line.basis_value / basis_total;
        let truncated = trunc4(raw);
        line.alloc_amount = truncated;
        allocated += truncated;
        remainders.push(raw - truncated);
    }
    let shortfall = pool - allocated;
    if shortfall > Decimal::ZERO {
        // 余数最大者；并列 → 基数大者；再并列 → cost_object_id 字符串字典序最小
        let mut winner = 0usize;
        for index in 1..lines.len() {
            let better = remainders[index] > remainders[winner]
                || (remainders[index] == remainders[winner]
                    && (lines[index].basis_value > lines[winner].basis_value
                        || (lines[index].basis_value == lines[winner].basis_value
                            && lines[index].cost_object_id.to_string() < lines[winner].cost_object_id.to_string())));
            if better {
                winner = index;
            }
        }
        lines[winner].alloc_amount += shortfall;
        lines[winner].rounding_adj = shortfall;
    }
    let outcome = AllocOutcome {
        pool_amount: pool,
        basis_total,
        lines,
    };
    debug_assert_eq!(
        outcome
            .lines
            .iter()
            .map(|line| line.alloc_amount)
            .fold(Decimal::ZERO, |acc, value| acc + value),
        pool,
        "分摊守恒断言：Σ alloc_i 必须等于 pool"
    );
    Ok(outcome)
}

// ==================== 规格组装 ====================

/// 由建规则入参构造归一化规格（**纯逻辑**，`dept_name` 由 handler 查库后传入）。
pub fn spec_from_create(
    req: &CostAllocRuleCreateReq,
    dept_name: &str,
) -> std::result::Result<CostAllocRuleSpec, ApiError> {
    let rule_code = normalize_rule_code(req.rule_code.as_deref())?;
    let rule_name = normalize_rule_name(req.rule_name.as_deref())?;
    let source_dept_id = cost_labor_dto::require_uuid(req.dept_id.as_deref(), "dept_id")?;
    let account_code = normalize_account_code(req.account_code.as_deref())?;
    let basis = normalize_basis(req.basis.as_deref())?;
    let priority = normalize_priority(req.priority)?;
    let effective_from = cost_labor_dto::require_date(req.effective_from.as_deref(), "effective_from")?;
    let effective_to = match cost_labor_dto::clean_optional(req.effective_to.clone()) {
        None => None,
        Some(_) => Some(cost_labor_dto::parse_date(
            req.effective_to.as_deref().unwrap_or_default(),
            "effective_to",
        )?),
    };
    ensure_effective_range(effective_from, effective_to)?;
    let status = normalize_status(req.status)?;
    let remark = normalize_remark(req.remark.as_deref())?;
    if dept_name.trim().is_empty() {
        return Err(bad_request("source_dept_name（车间名）不能为空：规则要按车间归属"));
    }
    if dept_name.trim().chars().count() > DEPT_NAME_MAX {
        return Err(bad_request(format!("车间名快照最长 {DEPT_NAME_MAX} 个字符")));
    }
    Ok(CostAllocRuleSpec {
        rule_code,
        rule_name,
        biz_type: BIZ_TYPE_OVERHEAD.to_string(),
        source_dept_id,
        source_dept_name: dept_name.trim().to_string(),
        account_code,
        basis,
        priority,
        effective_from,
        effective_to,
        status,
        remark,
    })
}

/// 由修改入参 + 现值合并出完整规格（**纯逻辑**；缺省字段保持现值）。
pub fn spec_from_update(
    req: &CostAllocRuleUpdateReq,
    current: &CostAllocRuleSpec,
    dept_name: Option<&str>,
) -> std::result::Result<CostAllocRuleSpec, ApiError> {
    let rule_code = match req.rule_code.as_deref() {
        None => current.rule_code.clone(),
        Some(_) => normalize_rule_code(req.rule_code.as_deref())?,
    };
    let rule_name = match req.rule_name.as_deref() {
        None => current.rule_name.clone(),
        Some(_) => normalize_rule_name(req.rule_name.as_deref())?,
    };
    let (source_dept_id, source_dept_name) = match req.dept_id.as_deref() {
        None => (current.source_dept_id, current.source_dept_name.clone()),
        Some(raw) => {
            let id = cost_labor_dto::require_uuid(Some(raw), "dept_id")?;
            let name = dept_name
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| bad_request("换车间时必须能取到车间名（快照来源）"))?;
            (id, name.to_string())
        }
    };
    let account_code = match req.account_code.as_deref() {
        None => current.account_code.clone(),
        Some(_) => normalize_account_code(req.account_code.as_deref())?,
    };
    let basis = match req.basis.as_deref() {
        None => current.basis.clone(),
        Some(_) => normalize_basis(req.basis.as_deref())?,
    };
    let priority = match req.priority {
        None => current.priority,
        Some(value) => normalize_priority(Some(value))?,
    };
    let effective_from = match req.effective_from.as_deref() {
        None => current.effective_from,
        Some(_) => cost_labor_dto::require_date(req.effective_from.as_deref(), "effective_from")?,
    };
    let effective_to = match req.effective_to.as_deref() {
        // 不传 = 不改
        None => current.effective_to,
        // 传空串（或全空白）= 清空 → 长期有效
        Some(raw) if raw.trim().is_empty() => None,
        Some(raw) => Some(cost_labor_dto::parse_date(raw, "effective_to")?),
    };
    ensure_effective_range(effective_from, effective_to)?;
    let status = match req.status {
        None => current.status,
        Some(value) => normalize_status(Some(value))?,
    };
    let remark = match req.remark.as_deref() {
        None => current.remark.clone(),
        Some(raw) => normalize_remark(Some(raw))?,
    };
    Ok(CostAllocRuleSpec {
        rule_code,
        rule_name,
        biz_type: current.biz_type.clone(),
        source_dept_id,
        source_dept_name,
        account_code,
        basis,
        priority,
        effective_from,
        effective_to,
        status,
        remark,
    })
}

/// 解析试算入参里的 `rule_ids`（可空；非法 UUID → 400 指名）。
pub fn parse_rule_ids(raw: &Option<Vec<String>>) -> std::result::Result<Option<Vec<Uuid>>, ApiError> {
    let Some(list) = raw else { return Ok(None) };
    if list.is_empty() {
        return Ok(None);
    }
    let mut ids = Vec::with_capacity(list.len());
    for (index, item) in list.iter().enumerate() {
        ids.push(cost_labor_dto::parse_uuid(item, &format!("rule_ids[{index}]"))?);
    }
    Ok(Some(ids))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::from_str(text).expect("日期字面量")
    }

    fn version(code: &str, from: &str, to: Option<&str>, status: i16) -> RuleVersion {
        RuleVersion {
            rule_id: Uuid::now_v7(),
            rule_code: code.to_string(),
            status,
            effective_from: date(from),
            effective_to: to.map(date),
            priority: PRIORITY_DEFAULT,
        }
    }

    fn object(id: u128, no: &str, basis: &str) -> AllocObject {
        AllocObject {
            cost_object_id: Uuid::from_u128(id),
            cost_object_no: no.to_string(),
            cost_object_status: "OPEN".to_string(),
            basis_value: dec(basis),
        }
    }

    // ---------- 值域 / 与 DDL 对齐 ----------

    #[test]
    fn bases_match_the_db_check() {
        assert_eq!(BASES.len(), 4);
        for basis in ["LABOR_HOURS", "MACHINE_HOURS", "OUTPUT_QTY", "MATERIAL_COST"] {
            assert!(BASES.contains(&basis), "{basis}");
            assert!(normalize_basis(Some(basis)).is_ok(), "{basis}");
        }
        assert!(normalize_basis(Some("labor_hours")).is_ok(), "小写归一化");
        assert!(normalize_basis(Some("HOURS")).is_err());
        assert!(normalize_basis(None).is_err());
        assert_eq!(BIZ_TYPES, ["OVERHEAD"]);
        assert_eq!(STATUSES, [1, 0]);
    }

    #[test]
    fn manufacturing_account_matches_the_db_check() {
        for ok in ["4101.01", "4101.09", "4101.01.02"] {
            assert!(is_manufacturing_account(ok), "{ok}");
            assert!(normalize_account_code(Some(ok)).is_ok(), "{ok}");
        }
        for bad in ["4101", "41010.1", "4101.", "4101.abc", "5602.01", "1405.01"] {
            assert!(!is_manufacturing_account(bad), "{bad}");
            let error = normalize_account_code(Some(bad)).expect_err(bad);
            assert_eq!(error.status, 400, "{bad}");
            assert_eq!(error.code, error_code::ERP_COST_OVERHEAD_ACCOUNT, "{bad}");
        }
    }

    #[test]
    fn rule_code_is_normalized_and_bounded() {
        assert_eq!(normalize_rule_code(Some(" wl-001 ")).expect("编码"), "WL-001");
        assert!(
            normalize_rule_code(Some("1111111111111111111111111111111111")).is_err(),
            "32 位上限"
        );
        assert!(normalize_rule_code(Some("水 电")).is_err(), "只允许 ASCII 编码字符");
        assert!(normalize_rule_code(Some("  ")).is_err());
        assert!(normalize_rule_code(None).is_err());
    }

    #[test]
    fn priority_and_status_defaults() {
        assert_eq!(normalize_priority(None).expect("缺省"), PRIORITY_DEFAULT);
        assert_eq!(normalize_priority(Some(1)).expect("下限"), 1);
        assert_eq!(normalize_priority(Some(999)).expect("上限"), 999);
        assert!(normalize_priority(Some(0)).is_err());
        assert!(normalize_priority(Some(1000)).is_err());
        assert_eq!(normalize_status(None).expect("缺省"), STATUS_ACTIVE);
        assert_eq!(normalize_status(Some(0)).expect("停用"), STATUS_DISABLED);
        assert!(normalize_status(Some(2)).is_err());
        assert_eq!(status_label(STATUS_ACTIVE), "启用");
    }

    #[test]
    fn effective_range_rejects_reverse() {
        assert!(ensure_effective_range(date("2026-11-01"), None).is_ok());
        assert!(ensure_effective_range(date("2026-11-01"), Some(date("2026-11-01"))).is_ok());
        let error = ensure_effective_range(date("2026-11-01"), Some(date("2026-10-31"))).expect_err("结束早于开始");
        assert_eq!(error.status, 400);
    }

    // ---------- 生效期版本选择（卡面验收项 ①） ----------

    #[test]
    fn period_bounds_cover_the_whole_month() {
        assert_eq!(
            period_bounds("2026-11").expect("11 月"),
            (date("2026-11-01"), date("2026-11-30"))
        );
        assert_eq!(
            period_bounds("2026-02").expect("2 月"),
            (date("2026-02-01"), date("2026-02-28"))
        );
        assert_eq!(
            period_bounds("2028-02").expect("闰年 2 月"),
            (date("2028-02-01"), date("2028-02-29"))
        );
        assert_eq!(
            period_bounds("2026-12").expect("12 月跨年"),
            (date("2026-12-01"), date("2026-12-31"))
        );
        assert!(period_bounds("2026-13").is_err());
        assert!(period_bounds("2026/11").is_err());
    }

    /// **验收项「规则按生效期选最新版本」**：11 月取 11-01 的版本，10 月仍取旧版本。
    #[test]
    fn latest_version_wins_within_the_period() {
        let versions = vec![
            version("WL-001-OLD", "2026-01-01", None, STATUS_ACTIVE),
            version("WL-001-NEW", "2026-11-01", None, STATUS_ACTIVE),
        ];
        let (nov_from, nov_to) = period_bounds("2026-11").expect("11 月");
        let picked = pick_latest_version(&versions, nov_from, nov_to).expect("11 月有版本");
        assert_eq!(picked.rule_code, "WL-001-NEW", "同族取生效更晚的版本");

        let (oct_from, oct_to) = period_bounds("2026-10").expect("10 月");
        let picked = pick_latest_version(&versions, oct_from, oct_to).expect("10 月有版本");
        assert_eq!(picked.rule_code, "WL-001-OLD", "历史期间不许被新版本改口径");
    }

    /// 旧版本被 `effective_to` 收口后，新期间只剩新版本（不重叠也正确）。
    #[test]
    fn closed_version_no_longer_eligible() {
        let versions = vec![
            version("OLD", "2026-01-01", Some("2026-10-31"), STATUS_ACTIVE),
            version("NEW", "2026-11-01", None, STATUS_ACTIVE),
        ];
        let (from, to) = period_bounds("2026-11").expect("11 月");
        assert!(!versions[0].covers_period(from, to));
        assert_eq!(pick_latest_version(&versions, from, to).expect("版本").rule_code, "NEW");
    }

    /// 停用 / 已过期 / 尚未生效 的版本都不参与。
    #[test]
    fn disabled_and_out_of_range_versions_are_skipped() {
        let (from, to) = period_bounds("2026-11").expect("11 月");
        let disabled = vec![version("OFF", "2026-01-01", None, STATUS_DISABLED)];
        assert!(pick_latest_version(&disabled, from, to).is_none());

        let expired = vec![version("EXPIRED", "2025-01-01", Some("2025-12-31"), STATUS_ACTIVE)];
        assert!(pick_latest_version(&expired, from, to).is_none());

        let future = vec![version("FUTURE", "2026-12-01", None, STATUS_ACTIVE)];
        assert!(pick_latest_version(&future, from, to).is_none());
    }

    /// 同一天两条版本（理论上被入口拦，但库里可能已有）⇒ 取 rule_code 字典序最小，结果确定。
    #[test]
    fn same_day_versions_are_deterministic() {
        let versions = vec![
            version("B-RULE", "2026-11-01", None, STATUS_ACTIVE),
            version("A-RULE", "2026-11-01", None, STATUS_ACTIVE),
        ];
        let (from, to) = period_bounds("2026-11").expect("11 月");
        for _ in 0..5 {
            assert_eq!(
                pick_latest_version(&versions, from, to).expect("版本").rule_code,
                "A-RULE"
            );
        }
    }

    // ---------- 四种分配标准各一例（卡面验收项 ③） ----------

    #[test]
    fn basis_labor_hours_takes_labor_hours() {
        let object = ObjectBasis {
            labor_hours: dec("60"),
            machine_hours: dec("11"),
            completed_qty: dec("7"),
            material_cost: dec("1.5"),
        };
        assert_eq!(basis_value(BASIS_LABOR_HOURS, &object), dec("60"));
    }

    #[test]
    fn basis_machine_hours_takes_machine_hours() {
        let object = ObjectBasis {
            labor_hours: dec("60"),
            machine_hours: dec("11"),
            completed_qty: dec("7"),
            material_cost: dec("1.5"),
        };
        assert_eq!(basis_value(BASIS_MACHINE_HOURS, &object), dec("11"));
    }

    #[test]
    fn basis_output_qty_takes_completed_qty() {
        let object = ObjectBasis {
            labor_hours: dec("60"),
            machine_hours: dec("11"),
            completed_qty: dec("7"),
            material_cost: dec("1.5"),
        };
        assert_eq!(basis_value(BASIS_OUTPUT_QTY, &object), dec("7"));
    }

    #[test]
    fn basis_material_cost_takes_material_cost() {
        let object = ObjectBasis {
            labor_hours: dec("60"),
            machine_hours: dec("11"),
            completed_qty: dec("7"),
            material_cost: dec("1.5"),
        };
        assert_eq!(basis_value(BASIS_MATERIAL_COST, &object), dec("1.5"));
    }

    #[test]
    fn unknown_basis_yields_zero_instead_of_panic() {
        assert_eq!(basis_value("STEP_TRANSFER", &ObjectBasis::default()), Decimal::ZERO);
    }

    // ---------- 最大余数法（§4.4.4 的 B1 / B2 两个算例） ----------

    /// 算例 B1（整除）：池 4000，基数 60/20/20（合计 100）→ 2400 / 800 / 800。
    #[test]
    fn allocation_b1_exact_division() {
        let objects = vec![
            object(1, "CB-A", "60"),
            object(2, "CB-B", "20"),
            object(3, "CB-C", "20"),
        ];
        let outcome = allocate(dec("4000"), &objects).expect("分摊");
        let amounts: Vec<String> = outcome.lines.iter().map(|line| money4(line.alloc_amount)).collect();
        assert_eq!(amounts, ["2400.0000", "800.0000", "800.0000"]);
        assert_eq!(outcome.basis_total, dec("100"));
        assert!(
            outcome.lines.iter().all(|line| line.rounding_adj == Decimal::ZERO),
            "整除时无尾差"
        );
    }

    /// 算例 B2（除不尽）：池 1000，基数 1/1/1 → 333.3334 / 333.3333 / 333.3333，
    /// 尾差 0.0001 **只落在一行**，Σ = 1000（真库演算见 §4.4.4）。
    #[test]
    fn allocation_b2_remainder_lands_on_one_row() {
        let objects = vec![object(1, "CB-A", "1"), object(2, "CB-B", "1"), object(3, "CB-C", "1")];
        let outcome = allocate(dec("1000"), &objects).expect("分摊");
        let amounts: Vec<String> = outcome.lines.iter().map(|line| money4(line.alloc_amount)).collect();
        assert_eq!(amounts, ["333.3334", "333.3333", "333.3333"]);
        let adjusted = outcome
            .lines
            .iter()
            .filter(|line| line.rounding_adj != Decimal::ZERO)
            .collect::<Vec<_>>();
        assert_eq!(adjusted.len(), 1, "尾差只能落在唯一一行");
        assert_eq!(adjusted[0].rounding_adj, dec("0.0001"));
        assert_eq!(
            outcome
                .lines
                .iter()
                .map(|line| line.alloc_amount)
                .fold(Decimal::ZERO, |acc, value| acc + value),
            dec("1000"),
            "守恒：Σ alloc = pool"
        );
    }

    /// 三个对象余数并列时，尾差给**基数大者**（再并列取 id 字典序最小）。
    #[test]
    fn allocation_ties_prefer_larger_basis_then_smaller_id() {
        // 池 100、基数 1/1/1：raw 各 33.333333…，余数并列 → 基数并列 → 取 id 最小
        let objects = vec![object(9, "CB-Z", "1"), object(2, "CB-B", "1"), object(5, "CB-M", "1")];
        let outcome = allocate(dec("100"), &objects).expect("分摊");
        let winner = outcome
            .lines
            .iter()
            .find(|line| line.rounding_adj != Decimal::ZERO)
            .expect("必有一行收尾差");
        assert_eq!(winner.cost_object_id, Uuid::from_u128(2), "并列取 id 字典序最小");
    }

    #[test]
    fn zero_pool_allocates_zeros_and_keeps_basis_total() {
        let objects = vec![object(1, "CB-A", "10"), object(2, "CB-B", "30")];
        let outcome = allocate(Decimal::ZERO, &objects).expect("池空");
        assert_eq!(outcome.basis_total, dec("40"));
        assert!(outcome.lines.iter().all(|line| line.alloc_amount == Decimal::ZERO));
        assert!(outcome.lines.iter().all(|line| line.rounding_adj == Decimal::ZERO));
    }

    /// 池 > 0 但没有对象 ⇒ `409` + `66062`（设计 §4.4.3 明写要报错，不许静默留池）。
    #[test]
    fn pool_without_objects_is_a_hard_error() {
        let error = allocate(dec("4000"), &[]).expect_err("无对象");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_ALLOC_NO_OBJECT);
    }

    /// 池 > 0 但基数全 0 ⇒ `409` + `66063`（守恒不可能成立）。
    #[test]
    fn pool_with_zero_basis_is_a_hard_error() {
        let objects = vec![object(1, "CB-A", "0"), object(2, "CB-B", "0")];
        let error = allocate(dec("4000"), &objects).expect_err("基数全 0");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_ALLOC_BASIS_ZERO);
        assert!(error.msg.contains("基数合计为 0"), "{}", error.msg);
    }

    #[test]
    fn negative_pool_or_basis_is_rejected() {
        assert_eq!(
            allocate(dec("-1"), &[object(1, "CB-A", "1")]).expect_err("负池").status,
            400
        );
        assert_eq!(
            allocate(dec("10"), &[object(1, "CB-A", "-1")])
                .expect_err("负基数")
                .status,
            400
        );
    }

    #[test]
    fn trunc4_truncates_instead_of_rounding() {
        assert_eq!(trunc4(dec("1.23456")).to_string(), "1.2345");
        assert_eq!(trunc4(dec("1.99999")).to_string(), "1.9999");
        assert_eq!(trunc4(dec("2")).to_string(), "2");
    }

    /// 大批量对象时守恒仍然成立（n × 0.0001 的尾差一次收在唯一一行）。
    #[test]
    fn allocation_conserves_with_many_objects() {
        let objects = (1..=97u128)
            .map(|index| object(index, &format!("CB-{index:03}"), "1"))
            .collect::<Vec<_>>();
        let outcome = allocate(dec("12345.6789"), &objects).expect("分摊");
        let total = outcome
            .lines
            .iter()
            .map(|line| line.alloc_amount)
            .fold(Decimal::ZERO, |acc, value| acc + value);
        assert_eq!(total, dec("12345.6789"));
        assert_eq!(
            outcome
                .lines
                .iter()
                .filter(|line| line.rounding_adj != Decimal::ZERO)
                .count(),
            1
        );
    }

    // ---------- 规格组装 ----------

    fn create_req() -> CostAllocRuleCreateReq {
        CostAllocRuleCreateReq {
            rule_code: Some(" wl-001 ".to_string()),
            rule_name: Some("一车间水电按工时分摊".to_string()),
            dept_id: Some("018f4b2c-1111-7222-8333-444455556666".to_string()),
            account_code: Some("4101.02".to_string()),
            basis: Some("labor_hours".to_string()),
            priority: None,
            effective_from: Some("2026-11-01".to_string()),
            effective_to: None,
            status: None,
            remark: Some("  ".to_string()),
        }
    }

    #[test]
    fn spec_from_create_normalizes_everything() {
        let spec = spec_from_create(&create_req(), "一车间").expect("规格");
        assert_eq!(spec.rule_code, "WL-001");
        assert_eq!(spec.basis, BASIS_LABOR_HOURS);
        assert_eq!(spec.biz_type, BIZ_TYPE_OVERHEAD);
        assert_eq!(spec.priority, PRIORITY_DEFAULT);
        assert_eq!(spec.status, STATUS_ACTIVE);
        assert_eq!(spec.effective_from, date("2026-11-01"));
        assert_eq!(spec.effective_to, None);
        assert_eq!(spec.remark, None, "空白备注 = 无备注");
        assert_eq!(spec.source_dept_name, "一车间");
    }

    #[test]
    fn spec_from_create_rejects_missing_required_fields() {
        for (field, mutate) in [
            (
                "rule_code",
                (|req: &mut CostAllocRuleCreateReq| req.rule_code = None) as fn(&mut CostAllocRuleCreateReq),
            ),
            ("rule_name", |req: &mut CostAllocRuleCreateReq| req.rule_name = None),
            ("dept_id", |req: &mut CostAllocRuleCreateReq| req.dept_id = None),
            ("account_code", |req: &mut CostAllocRuleCreateReq| {
                req.account_code = None
            }),
            ("basis", |req: &mut CostAllocRuleCreateReq| req.basis = None),
            ("effective_from", |req: &mut CostAllocRuleCreateReq| {
                req.effective_from = None
            }),
        ] {
            let mut req = create_req();
            mutate(&mut req);
            let error = spec_from_create(&req, "一车间").expect_err(field);
            assert_eq!(error.status, 400, "{field}: {}", error.msg);
            assert!(error.msg.contains(field), "{field}: {}", error.msg);
        }
    }

    #[test]
    fn spec_from_update_keeps_absent_fields_and_clears_empty_end_date() {
        let current = spec_from_create(&create_req(), "一车间").expect("现值");

        let merged = spec_from_update(&CostAllocRuleUpdateReq::default(), &current, None).expect("空改动");
        assert_eq!(merged, current, "全缺省 = 原样");

        let cleared = spec_from_update(
            &CostAllocRuleUpdateReq {
                effective_to: Some("   ".to_string()),
                ..Default::default()
            },
            &current,
            None,
        )
        .expect("清空结束日");
        assert_eq!(cleared.effective_to, None);

        let renamed = spec_from_update(
            &CostAllocRuleUpdateReq {
                rule_name: Some("新名".to_string()),
                priority: Some(20),
                ..Default::default()
            },
            &current,
            None,
        )
        .expect("改名");
        assert_eq!(renamed.rule_name, "新名");
        assert_eq!(renamed.priority, 20);
        assert_eq!(renamed.rule_code, current.rule_code, "编码不传就不变");
    }

    #[test]
    fn spec_from_update_requires_name_when_dept_changes() {
        let current = spec_from_create(&create_req(), "一车间").expect("现值");
        let error = spec_from_update(
            &CostAllocRuleUpdateReq {
                dept_id: Some(Uuid::now_v7().to_string()),
                ..Default::default()
            },
            &current,
            None,
        )
        .expect_err("换车间必须带名字");
        assert_eq!(error.status, 400);
    }

    #[test]
    fn rule_ids_parse() {
        assert!(parse_rule_ids(&None).expect("空").is_none());
        assert!(parse_rule_ids(&Some(vec![])).expect("空数组").is_none());
        let id = Uuid::now_v7();
        let parsed = parse_rule_ids(&Some(vec![id.to_string()])).expect("一个 id");
        assert_eq!(parsed, Some(vec![id]));
        let error = parse_rule_ids(&Some(vec!["nope".to_string()])).expect_err("非法 id");
        assert!(error.msg.contains("rule_ids[0]"), "{}", error.msg);
    }

    /// 建规则写库错误的翻译：唯一约束 / CHECK 都得变成人话（不是英文约束名）。
    #[test]
    fn write_errors_are_translated() {
        let spec = spec_from_create(&create_req(), "一车间").expect("规格");
        let taken = map_write_error(
            DbError::UniqueViolation {
                column: None,
                constraint: Some("uk_biz_cost_rule_code".to_string()),
                source: None,
            },
            &spec,
            "建规则",
        );
        assert_eq!(taken.status, 409);
        assert!(taken.msg.contains("WL-001"), "{}", taken.msg);

        let account = map_write_error(
            DbError::CheckViolation {
                constraint: Some("ck_biz_cost_rule_account".to_string()),
                source: None,
            },
            &spec,
            "建规则",
        );
        assert_eq!(account.code, error_code::ERP_COST_OVERHEAD_ACCOUNT);

        let dates = map_write_error(
            DbError::CheckViolation {
                constraint: Some("ck_biz_cost_rule_dates".to_string()),
                source: None,
            },
            &spec,
            "建规则",
        );
        assert_eq!(dates.status, 400);
        assert!(dates.msg.contains("生效"), "{}", dates.msg);
    }
}
