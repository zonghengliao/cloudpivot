//! 手工制造费用台账契约层（成本核算 · ARCH-9 §3.1 ⑤ / §4.3 / §4.5.1 / §5.2 #17–#19 / §5.3）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
//! §3.1 ⑤（`biz_cost_overhead` 表结构 + 5 条 CHECK）、
//! §4.3（费·手工归集口径：`cost_object_id` 非空 → 直接归集，为空 → 进车间费用池）、
//! §4.5.1（「费·手工（无单据）」：财务每月录，不填就是没有这笔费用，**不自动估算**）、
//! §5.2 #17–#19（3 个接口）、§5.3（权限码 `business:erp:cost-overhead:{list,create,delete}`）。
//!
//! # 本模块拦什么（卡面三条验收的落地位置）
//!
//! | 卡面验收 | 落地 |
//! |---|---|
//! | CRUD | C（`POST`）/ R（`GET` 分页）/ D（`DELETE` 软删）—— 设计 §5.2 只给这三个接口，**没有 PUT** |
//! | 科目不在 `4101.*` → 拒绝 | [`ensure_debit_account`] → `400 + 66060`；库层 `ck_biz_cost_oh_debit` 同口径兜底 |
//! | 金额必须 > 0 | [`ensure_amount`] → `400 + 66061`；库层 `ck_biz_cost_oh_amount` 同口径兜底 |
//!
//! # 为什么没有「编辑」（PUT）
//!
//! 设计 §5.2「手工费用台账」只列了 `GET` / `POST` / `DELETE` 三条，§5.3 的权限码也只有
//! `list | create | delete`，V0378 菜单种子也只种了「新增费用 / 删除费用」两个按钮。
//! 三条证据指向同一结论：**录错了就删掉重录**（表没有状态列，删是软删，行留在库里可追溯）。
//! 因此本模块**不造** `PUT`、也不新开 `cost-overhead:update` 权限码 —— 后者的承载行在
//! 已应用的迁移 V0378 里没有，新开码会让 `perm_menu_gate` 直接红（与 B1-9 批次卡同一条纪律）。
//!
//! # 除格式外还查账套现状（比设计文字多做一步，理由在此）
//!
//! 设计的硬约束只有「只允许 `4101.*`」（库层 CHECK）。但只查前缀会放过三种**到结算才炸**
//! 的输入：
//!
//! 1. `4101.99` —— 前缀对，账套里根本没有这个科目 → 结算时 `post_voucher` 找不到科目；
//! 2. `4101`（非末级）—— 前缀都不匹配（没有点），但真正危险的是 `4101.01` 这类被改成
//!    非末级的行：凭证闸 `non_leaf_codes` 只允许末级记账（`voucher.rs::post_voucher`）；
//! 3. 已停用（`status = 0`）的科目 —— 账套停用了，单据还在往里记。
//!
//! 这三种在录入时就拦掉，报文点名「哪个编码、账套里是什么状态、该换成什么」，
//! 比让用户等到月结结算那一步看到一句英文约束名有用得多。**只拦本模块（费用台账）**，
//! 不改任何已上线口径：`biz_depreciation` / `biz_expense_report` 走的是它们自己的科目配置。
//!
//! # 参数解析复用 `cost_labor_dto`
//!
//! 期间格式（`yyyy-MM`）、UUID 解析、文本清洗、数值精度/范围这批工具，工时台账（B2-1 已上线）
//! 里已经有一份**带单测的实现**。费用台账再抄一遍的唯一后果是「两套校验漂移」——
//! 比如某天有人只改了其中一边的期间正则，于是同一张月结单里工时能录 2026-13、费用不能。
//! 所以这里复用而不是复制；只在**本表特有**的地方写自己的实现（金额 > 0 的专用码、
//! 科目合法性、没有状态列带来的删除闸）。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::cost_labor_dto;
use crate::shared::error_code;

// ==================== 值域（与 V0371 的 CHECK 逐字一致） ====================

/// 费用项目：折旧费（映射 `4101.01`）。
pub const ITEM_DEPRECIATION: &str = "DEPRECIATION";
/// 费用项目：水电费（映射 `4101.02`）。
pub const ITEM_UTILITY: &str = "UTILITY";
/// 费用项目：租金。
pub const ITEM_RENT: &str = "RENT";
/// 费用项目：修理费（映射 `4101.03`）。
pub const ITEM_REPAIR: &str = "REPAIR";
/// 费用项目：车间管理费（映射 `4101.04`）。
pub const ITEM_WORKSHOP_ADMIN: &str = "WORKSHOP_ADMIN";
/// 费用项目：机物料消耗。
pub const ITEM_CONSUMABLE: &str = "CONSUMABLE";
/// 费用项目：其他（映射 `4101.09`）。
pub const ITEM_OTHER: &str = "OTHER";

/// 费用项目值域（`ck_biz_cost_oh_item`，与字典 `cost_expense_item` 的 7 项一一对应）。
pub const EXPENSE_ITEMS: [&str; 7] = [
    ITEM_DEPRECIATION,
    ITEM_UTILITY,
    ITEM_RENT,
    ITEM_REPAIR,
    ITEM_WORKSHOP_ADMIN,
    ITEM_CONSUMABLE,
    ITEM_OTHER,
];

/// 来源：网页手工录入（本卡 HTTP 建单只写这个）。
pub const SOURCE_MANUAL: &str = "MANUAL";
/// 来源：Excel 导入（设计 §5.4：费用台账导入**二期**再开，一期不实现）。
pub const SOURCE_IMPORT: &str = "IMPORT";
/// 来源值域（`ck_biz_cost_oh_src`）。
pub const SOURCE_TYPES: [&str; 2] = [SOURCE_MANUAL, SOURCE_IMPORT];

/// 借方科目必须落在这个前缀下（`ck_biz_cost_oh_debit`：`debit_account_code LIKE '4101.%'`）。
pub const DEBIT_ACCOUNT_PREFIX: &str = "4101.";
/// 借方科目缺省值（DDL 默认 `4101.09` 制造费用-其他）。
pub const DEFAULT_DEBIT_ACCOUNT: &str = "4101.09";
/// 金额小数位（`amount NUMERIC(18,4)`）。
pub const AMOUNT_SCALE: u32 = 4;
/// `amount NUMERIC(18,4)` 的上限（14 位整数 + 4 位小数）。
pub const AMOUNT_MAX: &str = "99999999999999.9999";
/// 科目编码列长度（`debit_account_code` / `credit_account_code` 均 `VARCHAR(30)`）。
pub const ACCOUNT_CODE_MAX: usize = 30;
/// 费用项目列长度（`expense_item VARCHAR(16)`）。
pub const EXPENSE_ITEM_MAX: usize = 16;
/// 备注列长度（`remark VARCHAR(255)`）。
pub const REMARK_MAX: usize = 255;
/// 期间串长度（`period_code VARCHAR(7)`，形如 `2026-11`）。
pub const PERIOD_LEN: usize = 7;

// ==================== 入参 ====================

/// 列表查询（§5.2 #17）。
///
/// 设计只写了 `cost-overhead:list`，没逐条列筛选字段；本模块按「财务每月对账要用什么查」
/// 给出 5 个筛选：期间（主口径）/ 成本对象 / 车间 / 费用项目 / 关键字。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostOverheadPageQuery {
    /// 会计期间 `yyyy-MM`。
    pub period_code: Option<String>,
    /// 成本对象（空 = 不筛；池行 `cost_object_id IS NULL` 目前无正筛口子）。
    pub cost_object_id: Option<String>,
    /// 车间（费用池的归属车间）。
    pub dept_id: Option<String>,
    /// 费用项目（`DEPRECIATION` / `UTILITY` / …）。
    pub expense_item: Option<String>,
    /// 关键字：借方/贷方科目编码、科目名、车间名、备注（模糊）。
    pub keyword: Option<String>,
    /// 页码（1 起）。
    pub page: Option<u64>,
    /// 每页条数（0 = 默认 20，上限 200）。
    pub page_size: Option<u64>,
}

/// 归一化后的列表筛选（model 只认这个）。
#[derive(Debug, Clone)]
pub struct CostOverheadFilter {
    /// 会计期间。
    pub period_code: Option<String>,
    /// 成本对象。
    pub cost_object_id: Option<Uuid>,
    /// 车间。
    pub dept_id: Option<Uuid>,
    /// 费用项目。
    pub expense_item: Option<String>,
    /// 关键字（`LIKE` 模式由 model 侧拼）。
    pub keyword: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 新建费用台账行（§5.2 #18）。
///
/// `dept_name` 不在入参里：车间名是**快照**，由服务端从 `sys_dept` 取名写入
/// （允许客户端写快照 = 允许伪造归属，与工时台账同一条纪律）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostOverheadCreateReq {
    /// 会计期间 `yyyy-MM`（必填）。
    pub period_code: Option<String>,
    /// 成本对象（**可空**：空 = 车间费用池，等分摊；非空 = 直接计入该批，§4.3）。
    pub cost_object_id: Option<String>,
    /// 车间（必填；费用池的归属车间，也是分摊规则的匹配键）。
    pub dept_id: Option<String>,
    /// 费用项目（必填，`EXPENSE_ITEMS` 之一）。
    pub expense_item: Option<String>,
    /// 借方科目（可空，缺省 `4101.09`）：**必须** `4101.*` 且是启用末级科目。
    pub debit_account_code: Option<String>,
    /// 对方科目（必填，如 `1002` 银行 / `2241.01` 应付账款 / `1801` 累计折旧）。
    pub credit_account_code: Option<String>,
    /// 金额（必填，> 0，最多 4 位小数）。
    pub amount: Option<Decimal>,
    /// 备注。
    pub remark: Option<String>,
}

// ==================== 归一化规格（dto 与 model 之间唯一的形状） ====================

/// 归一化后的费用台账行规格（纯逻辑产物；查库校验由 handler 做）。
#[derive(Debug, Clone, PartialEq)]
pub struct CostOverheadSpec {
    /// 会计期间。
    pub period_code: String,
    /// 成本对象（空 = 车间费用池）。
    pub cost_object_id: Option<Uuid>,
    /// 车间。
    pub dept_id: Uuid,
    /// 车间名（快照；由 handler 从 `sys_dept` 取名）。
    pub dept_name: String,
    /// 费用项目。
    pub expense_item: String,
    /// 借方科目（`4101.*`）。
    pub debit_account_code: String,
    /// 对方科目。
    pub credit_account_code: String,
    /// 金额（> 0）。
    pub amount: Decimal,
    /// 来源类型（本卡恒 `MANUAL`）。
    pub source_type: String,
    /// 备注。
    pub remark: Option<String>,
}

// ==================== 出参 ====================

/// 列表行（`*_id` 都带 `*_name` —— AGENTS 约定 13）。
#[derive(Debug, Clone, Serialize)]
pub struct CostOverheadListRes {
    /// 主键。
    pub id: String,
    /// 会计期间。
    pub period_code: String,
    /// 成本对象（空 = 车间费用池）。
    pub cost_object_id: Option<String>,
    /// 对象号（`CB-yyyyMMdd-NNNNNN`）。
    pub cost_object_no: Option<String>,
    /// 成本对象状态（`OPEN` / `CLOSED` / `CANCELLED`；空 = 池行或对象已删）。
    pub cost_object_status: Option<String>,
    /// 车间。
    pub dept_id: String,
    /// 车间名（`sys_dept` 实时名优先，落空回落到建单快照）。
    pub dept_name: Option<String>,
    /// 费用项目编码。
    pub expense_item: String,
    /// 费用项目名（字典 `cost_expense_item` 的实时名；字典行被删时回落到内置 7 项文案）。
    pub expense_item_label: Option<String>,
    /// 借方科目编码。
    pub debit_account_code: String,
    /// 借方科目名（账套实时名；科目被删时为空，编码仍在）。
    pub debit_account_name: Option<String>,
    /// 对方科目编码。
    pub credit_account_code: String,
    /// 对方科目名。
    pub credit_account_name: Option<String>,
    /// 金额（字符串；防前端 JS number 精度丢失）。
    pub amount: String,
    /// 来源类型 `MANUAL` / `IMPORT`。
    pub source_type: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
    /// 更新时间（RFC3339）。
    pub updated_at: String,
}

// ==================== 错误构造 ====================

/// 400 构造（参数类）。
pub fn bad_request(message: impl Into<String>) -> ApiError {
    cost_labor_dto::bad_request(message)
}

/// 409 构造。
pub fn conflict(message: impl Into<String>) -> ApiError {
    cost_labor_dto::conflict(message)
}

/// 404 构造（本表的行不存在 / 已软删）。
pub fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("费用台账行不存在或已删除（{id}）"),
    )
}

/// 费用项目名（内置 7 项文案，与 V0379 字典种子 `item_label` 一致）。
///
/// 只作为**兜底**：列表出参优先用 `sys_dict_item` 的实时 label（字典是唯一真源），
/// 字典行被删 / 改名时用这份保证界面不至于显示一个裸 `DEPRECIATION`。
pub fn expense_item_label(code: &str) -> &'static str {
    match code {
        ITEM_DEPRECIATION => "折旧费",
        ITEM_UTILITY => "水电费",
        ITEM_RENT => "租金",
        ITEM_REPAIR => "修理费",
        ITEM_WORKSHOP_ADMIN => "车间管理费",
        ITEM_CONSUMABLE => "机物料消耗",
        ITEM_OTHER => "其他",
        _ => "未知费用项目",
    }
}

/// 借方科目必须落在 `4101.*`（制造费用）—— **卡面验收项**「科目不在 4101.* → 拒绝」。
///
/// 判定与库 CHECK `debit_account_code LIKE '4101.%'` 等价：前缀是 `4101.` 且后面**还有内容**。
/// 额外要求分段都是数字（编码形如 `4101.01` / `4101.09`），挡掉 `4101.abc` 这种「前缀对
/// 但账套里不可能存在」的输入，免得白跑一次查库。
pub fn is_manufacturing_account(code: &str) -> bool {
    let Some(rest) = code.strip_prefix(DEBIT_ACCOUNT_PREFIX) else {
        return false;
    };
    !rest.is_empty()
        && rest
            .split('.')
            .all(|segment| !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit()))
}

/// 400 + `66060`：借方科目不在 `4101.*`。
pub fn debit_out_of_scope(code: &str) -> ApiError {
    ApiError::new(
        400,
        error_code::ERP_COST_OVERHEAD_ACCOUNT,
        format!(
            "借方科目「{code}」不是制造费用科目：手工费用台账的借方必须落在 `4101.*`\
             （如 4101.01 折旧费 / 4101.02 水电费 / 4101.03 修理费 / 4101.09 其他）。\n\
             为什么必须 4101：费用要先归到制造费用池（`4101`），结算时才按分摊规则转到\
             生产成本（`4001.03`）从而进产品成本；配成 5602 等其他科目，这笔费用一辈子进不了成本\
             （设计风险 R-2）。如果这笔支出确实不是制造费用，请走费用报销模块，不要录在费用台账。"
        ),
    )
}

/// 400 + `66060`：科目在账套里不存在 / 已软删。
pub fn account_not_found(code: &str, field: &str) -> ApiError {
    ApiError::new(
        400,
        error_code::ERP_COST_OVERHEAD_ACCOUNT,
        format!(
            "{field}「{code}」在账套（`biz_account`）里不存在或已删除。\n\
             费用台账的借方 / 对方科目都必须先建好科目再来录；请到「科目管理」确认编码，\
             或改用已有的 4101 末级科目（4101.01 折旧费 / 4101.02 水电费 / 4101.03 修理费 / \
             4101.04 车间管理费 / 4101.09 其他）"
        ),
    )
}

/// 400 + `66060`：科目存在但不是末级（凭证闸只允许末级记账）。
pub fn account_not_leaf(code: &str, name: &str, field: &str) -> ApiError {
    ApiError::new(
        400,
        error_code::ERP_COST_OVERHEAD_ACCOUNT,
        format!(
            "{field}「{code} {name}」不是末级科目（它是汇总科目，下面还有子科目）。\n\
             凭证闸只允许末级科目记账，选它到结算时才报错、单据会卡在待结算；\
             请改选它下面的具体末级科目"
        ),
    )
}

/// 400 + `66060`：科目已停用。
pub fn account_disabled(code: &str, name: &str, field: &str) -> ApiError {
    ApiError::new(
        400,
        error_code::ERP_COST_OVERHEAD_ACCOUNT,
        format!(
            "{field}「{code} {name}」已在账套里停用（status=0）。\n\
             停用科目不许再记账；请到「科目管理」启用它，或改选其他启用中的 4101 末级科目"
        ),
    )
}

/// 400 + `66061`：金额必须大于 0（卡面验收项）。
pub fn amount_not_positive(amount: Decimal) -> ApiError {
    ApiError::new(
        400,
        error_code::ERP_COST_OVERHEAD_AMOUNT,
        format!(
            "金额必须大于 0（收到 {amount}）。\n\
             费用台账一笔费用一行，只记正数金额：录错了请删掉这一行重录；\
             真要冲回某笔费用请做凭证红字（`biz_cost_overhead` 的 CHECK 也是 amount > 0）"
        ),
    )
}

/// 400 + `66061`：金额超出列精度 / 范围（**不做静默四舍五入**）。
pub fn amount_out_of_range(amount: Decimal, reason: &str) -> ApiError {
    ApiError::new(
        400,
        error_code::ERP_COST_OVERHEAD_AMOUNT,
        format!(
            "金额 {amount} {reason}（amount 列是 NUMERIC(18,4)：最多 {AMOUNT_SCALE} 位小数、\
             整数部分最多 14 位）。系统不会替你四舍五入 —— 静默改数会让账上金额与单据\
             上写的金额对不上，而这笔费用要进产品成本"
        ),
    )
}

/// 409：这一行已经被成本计算归集过，不许删（删了归集行会指向空来源）。
pub fn already_collected(period_code: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::RESOURCE_CONFLICT,
        format!(
            "这条费用已经被期间 {period_code} 的成本计算归集（`biz_cost_collect` 里有来源行），\
             不能删除。\n\
             已归集的数据是结算与凭证的依据，删掉会让归集金额失去来源、账实不符；\
             金额录错了请作废该期间的成本计算（仅在 DRAFT 状态可重算）后重录，\
             或对该期间做反结账 / 红冲"
        ),
    )
}

/// 409：成本对象不是 `OPEN`（已关批 / 已作废的批不再吃新费用）。
///
/// 与工时台账同码（`66057`）：语义完全一致（「这份成本不能再往上加了」），
/// 前端按码给同一个引导（换对象 / 重新开一批）；只是报文里的单据名换成「费用台账」。
pub fn object_not_open(object_no: &str, status: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_OBJECT_NOT_OPEN,
        format!(
            "成本对象「{object_no}」当前状态是{}（{}），不能计入新费用。\n\
             已关批 / 已作废的批成本已经结转，再往上加费用会让账面与结转凭证对不上；\
             请换一个在制的成本对象，或重新开一批（不挂成本对象的行表示车间费用池，等分摊）",
            object_status_label(status),
            status
        ),
    )
}

/// 成本对象状态的中文（只用于文案）。
pub fn object_status_label(status: &str) -> &'static str {
    match status {
        "OPEN" => "在制",
        "CLOSED" => "已关批",
        "CANCELLED" => "已作废",
        _ => "未知状态",
    }
}

// ==================== 纯校验 ====================

/// 费用项目归一化（大写 + 白名单）。
pub fn normalize_expense_item(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request(format!("expense_item（费用项目）不能为空，只认 {:?}", EXPENSE_ITEMS)))?
        .to_ascii_uppercase();
    if text.chars().count() > EXPENSE_ITEM_MAX || !EXPENSE_ITEMS.contains(&text.as_str()) {
        let listed = EXPENSE_ITEMS
            .iter()
            .map(|code| format!("{code}（{}）", expense_item_label(code)))
            .collect::<Vec<_>>()
            .join(" / ");
        return Err(bad_request(format!(
            "expense_item（费用项目）只认 {listed}（收到「{text}」）。\
             费用项目决定这笔费用进哪一类制造费用，不认识的编码无法归集"
        )));
    }
    Ok(text)
}

/// 科目编码清洗：trim + 非空 + 长度。
fn clean_account_code(raw: Option<&str>, field: &str) -> std::result::Result<String, ApiError> {
    let text = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request(format!("{field}（科目编码）不能为空")))?;
    if text.chars().count() > ACCOUNT_CODE_MAX {
        return Err(bad_request(format!(
            "{field} 最长 {ACCOUNT_CODE_MAX} 个字符（收到 {} 个）",
            text.chars().count()
        )));
    }
    Ok(text.to_string())
}

/// 借方科目：清洗 + `4101.*`。**只做纯格式判定**；账套现状（存在 / 末级 / 启用）由 handler 查库。
pub fn normalize_debit_account(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let code = clean_account_code(raw, "debit_account_code")?;
    if !is_manufacturing_account(&code) {
        return Err(debit_out_of_scope(&code));
    }
    Ok(code)
}

/// 对方科目：清洗（不限制前缀，但必须是账套里的启用末级科目，查库由 handler 做）。
pub fn normalize_credit_account(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    clean_account_code(raw, "credit_account_code")
}

/// 金额：> 0 且不超 `NUMERIC(18,4)` 的标度 / 范围。
///
/// **不做静默四舍五入**：录 12.34567 直接拒绝，而不是悄悄存成 12.3457 ——
/// 改数与工时台账「超精度直接 400」同一条纪律（设计 §4.5.2 硬规则 1「不猜」）。
pub fn ensure_amount(value: Decimal) -> std::result::Result<Decimal, ApiError> {
    if value <= Decimal::ZERO {
        return Err(amount_not_positive(value));
    }
    // 尾零不算超标度：`12.50000` 的 scale 是 5，但数值上就是 `12.5`
    if value.normalize().scale() > AMOUNT_SCALE {
        return Err(amount_out_of_range(value, &format!("超过 {AMOUNT_SCALE} 位小数")));
    }
    let max = AMOUNT_MAX.parse::<Decimal>().unwrap_or(Decimal::MAX);
    if value > max {
        return Err(amount_out_of_range(value, "超出金额上限"));
    }
    Ok(value)
}

/// 由建单入参构造归一化规格（**纯逻辑**，`dept_name` 由 handler 查库后传入）。
pub fn spec_from_create(
    req: &CostOverheadCreateReq,
    dept_name: &str,
) -> std::result::Result<CostOverheadSpec, ApiError> {
    let period_code = cost_labor_dto::normalize_period(req.period_code.as_deref())?;
    let dept_id = cost_labor_dto::require_uuid(req.dept_id.as_deref(), "dept_id")?;
    let cost_object_id = cost_labor_dto::parse_opt_uuid(req.cost_object_id.as_deref(), "cost_object_id")?;
    let expense_item = normalize_expense_item(req.expense_item.as_deref())?;
    let debit_account_code = match cost_labor_dto::clean_optional(req.debit_account_code.clone()) {
        None => DEFAULT_DEBIT_ACCOUNT.to_string(),
        Some(_) => normalize_debit_account(req.debit_account_code.as_deref())?,
    };
    let credit_account_code = normalize_credit_account(req.credit_account_code.as_deref())?;
    let amount = match req.amount {
        Some(amount) => ensure_amount(amount)?,
        None => {
            return Err(bad_request(
                "amount（金额）不能为空：费用台账一行一笔记账，金额是必填项",
            ));
        }
    };
    let remark = match cost_labor_dto::clean_optional(req.remark.clone()) {
        None => None,
        Some(text) => {
            if text.chars().count() > REMARK_MAX {
                return Err(bad_request(format!(
                    "remark（备注）最长 {REMARK_MAX} 个字符（收到 {} 个）",
                    text.chars().count()
                )));
            }
            Some(text)
        }
    };
    if dept_name.trim().is_empty() {
        return Err(bad_request("dept_name（车间名）不能为空：费用要按车间归集与分摊"));
    }
    Ok(CostOverheadSpec {
        period_code,
        cost_object_id,
        dept_id,
        dept_name: dept_name.trim().to_string(),
        expense_item,
        debit_account_code,
        credit_account_code,
        amount,
        source_type: SOURCE_MANUAL.to_string(),
        remark,
    })
}

/// 写库错误 → 友好 `ApiError`（**唯一的错误出口**；判据走 `DbError` 变体，不看 Display 文案）。
///
/// | 变体 | 结果 |
/// |---|---|
/// | `CheckViolation` + `ck_biz_cost_oh_amount` | 400 + `66061`「金额必须大于 0」 |
/// | `CheckViolation` + `ck_biz_cost_oh_debit` | 400 + `66060`「借方必须是 4101.*」 |
/// | 其它 `CheckViolation` | 400 + `10001`（点名约束，便于定位费用项目 / 期间格式 / 来源） |
/// | `ForeignKeyViolation` | 400（成本对象被并发删掉：`ON DELETE RESTRICT` 的另一面） |
/// | 其它 | 500（`utils::db_error`，真实原因只进日志） |
pub fn map_write_error(err: DbError, spec: &CostOverheadSpec, ctx: &str) -> ApiError {
    match &err {
        DbError::CheckViolation { constraint, .. } => match constraint.as_deref() {
            Some("ck_biz_cost_oh_amount") => amount_not_positive(spec.amount),
            Some("ck_biz_cost_oh_debit") => debit_out_of_scope(&spec.debit_account_code),
            other => ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!(
                    "数据不满足数据库约束{}（费用项目 / 期间格式 / 来源值域 / 金额）",
                    other.map(|name| format!(" {name}")).unwrap_or_default()
                ),
            ),
        },
        DbError::ForeignKeyViolation { constraint, .. } => ApiError::new(
            400,
            error_code::ERP_REFERENCE_MISSING,
            format!(
                "引用的成本对象已被删除，无法保存这条费用{}。\
                 请刷新页面重新选择成本对象（不挂对象的行表示车间费用池）",
                constraint
                    .as_deref()
                    .map(|name| format!("（{name}）"))
                    .unwrap_or_default()
            ),
        ),
        _ => crate::utils::db_error(err, ctx),
    }
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    fn sample_req() -> CostOverheadCreateReq {
        CostOverheadCreateReq {
            period_code: Some(" 2026-11 ".to_string()),
            cost_object_id: None,
            dept_id: Some("018f4b2c-1111-7222-8333-444455556666".to_string()),
            expense_item: Some("utility".to_string()),
            debit_account_code: None,
            credit_account_code: Some("1002".to_string()),
            amount: Some(dec("4000")),
            remark: Some(" 车间电费 ".to_string()),
        }
    }

    // ---------- 科目合法性（卡面验收项 ①） ----------

    /// `4101.*` 判定与库 CHECK `LIKE '4101.%'` 等价，且挡掉前缀对但不可能存在的编码。
    #[test]
    fn manufacturing_account_matches_the_db_check() {
        for ok in ["4101.01", "4101.09", "4101.01.02", "4101.1"] {
            assert!(is_manufacturing_account(ok), "{ok} 应当合法");
        }
        for bad in [
            "", "4101", "41010.1", "4101.", "4101.abc", "5602.01", " 4101.01", "4101.01 ", "1401.01", "4101..01",
        ] {
            assert!(!is_manufacturing_account(bad), "「{bad}」不该被当成制造费用科目");
        }
    }

    /// 验收项「科目不在 4101.* → 拒绝」：走到的是 400 + 66060，报文告诉用户去哪里改。
    #[test]
    fn non_4101_debit_is_rejected_with_66060() {
        let error = normalize_debit_account(Some("5602.01")).expect_err("科目不在 4101");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, 66060);
        assert!(error.msg.contains("4101"), "{}", error.msg);
        assert!(error.msg.contains("5602.01"), "要点名收到的编码：{}", error.msg);
        assert!(error.msg.contains("费用报销"), "要给出下一步动作：{}", error.msg);

        // 纯空白串走「必填」这条路（10001），不是「不在 4101.*」——
        // 建单时空串表示「用缺省 4101.09」，由 `spec_from_create` 先拦掉，不会走到这里
        let blank = normalize_debit_account(Some("   ")).expect_err("空白");
        assert_eq!(blank.status, 400);
        assert!(blank.msg.contains("debit_account_code"), "{}", blank.msg);
    }

    /// 借方留空 ⇒ 缺省 `4101.09`（DDL 默认值，口径只有一份）。
    #[test]
    fn blank_debit_falls_back_to_default_account() {
        let spec = spec_from_create(&sample_req(), "机加工车间").expect("规格");
        assert_eq!(spec.debit_account_code, DEFAULT_DEBIT_ACCOUNT);
        assert_eq!(DEFAULT_DEBIT_ACCOUNT, "4101.09");
        // 账套现状的三种拒绝文案都要点名「换哪个」
        for error in [
            account_not_found("4101.99", "debit_account_code"),
            account_not_leaf("4101.77", "制造费用-汇总", "debit_account_code"),
            account_disabled("4101.04", "制造费用-车间管理费", "debit_account_code"),
        ] {
            assert_eq!(error.status, 400);
            assert_eq!(error.code, 66060);
            assert!(error.msg.contains("科目"), "{}", error.msg);
        }
        assert!(
            account_not_leaf("4101.77", "汇总", "debit_account_code")
                .msg
                .contains("末级")
        );
        assert!(
            account_disabled("4101.04", "车间管理费", "debit_account_code")
                .msg
                .contains("停用")
        );
    }

    // ---------- 金额（卡面验收项 ②） ----------

    /// 验收项「金额必须 > 0」：0 / 负数一律 400 + 66061。
    #[test]
    fn amount_must_be_positive() {
        for bad in ["0", "0.0000", "-1", "-0.01"] {
            let error = ensure_amount(dec(bad)).expect_err(bad);
            assert_eq!(error.status, 400, "{bad}");
            assert_eq!(error.code, 66061, "{bad}");
            assert!(error.msg.contains("大于 0"), "{bad}: {}", error.msg);
        }
        // 正数放行，尾零不算超标度
        assert_eq!(ensure_amount(dec("0.0001")).expect("最小正数"), dec("0.0001"));
        assert_eq!(ensure_amount(dec("4000.0000")).expect("尾零"), dec("4000"));
    }

    /// 超 4 位小数 / 超上限 → 66061，**不做静默四舍五入**。
    #[test]
    fn amount_precision_is_not_silently_rounded() {
        let error = ensure_amount(dec("12.34567")).expect_err("5 位小数");
        assert_eq!(error.code, 66061);
        assert!(error.msg.contains("12.34567"), "要回显收到的值：{}", error.msg);
        assert!(error.msg.contains("四舍五入"), "{}", error.msg);

        let error = ensure_amount(dec("100000000000000")).expect_err("超出 18,4 上限");
        assert_eq!(error.code, 66061);
        assert!(error.msg.contains("NUMERIC(18,4)"), "{}", error.msg);
    }

    // ---------- 费用项目 / 期间 / 规格 ----------

    #[test]
    fn expense_item_is_normalized_and_whitelisted() {
        assert_eq!(
            normalize_expense_item(Some(" utility ")).expect("小写也认"),
            ITEM_UTILITY
        );
        assert_eq!(
            normalize_expense_item(Some("Depreciation")).expect("混合大小写"),
            ITEM_DEPRECIATION
        );
        let error = normalize_expense_item(Some("TRAVEL")).expect_err("不认识的项目");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("TRAVEL"), "{}", error.msg);
        // 报错必须把 7 个合法值连中文名一起列出来（用户要能直接照着选）
        for code in EXPENSE_ITEMS {
            assert!(error.msg.contains(code), "缺 {code}: {}", error.msg);
            assert!(error.msg.contains(expense_item_label(code)), "缺中文名 {code}");
        }
        assert_eq!(normalize_expense_item(None).expect_err("必填").status, 400);
    }

    /// 7 个费用项目的中文文案与 V0379 字典种子逐字一致（改名了这里的兜底也要跟着改）。
    #[test]
    fn expense_item_labels_match_the_dictionary_seed() {
        assert_eq!(expense_item_label(ITEM_DEPRECIATION), "折旧费");
        assert_eq!(expense_item_label(ITEM_UTILITY), "水电费");
        assert_eq!(expense_item_label(ITEM_RENT), "租金");
        assert_eq!(expense_item_label(ITEM_REPAIR), "修理费");
        assert_eq!(expense_item_label(ITEM_WORKSHOP_ADMIN), "车间管理费");
        assert_eq!(expense_item_label(ITEM_CONSUMABLE), "机物料消耗");
        assert_eq!(expense_item_label(ITEM_OTHER), "其他");
        assert_eq!(EXPENSE_ITEMS.len(), 7, "ck_biz_cost_oh_item 给了 7 个值");
    }

    /// 规格：期间归一化、对象可空、来源恒 MANUAL、备注 trim。
    #[test]
    fn spec_from_create_normalizes_every_field() {
        let spec = spec_from_create(&sample_req(), " 机加工车间 ").expect("规格");
        assert_eq!(spec.period_code, "2026-11");
        assert_eq!(spec.expense_item, ITEM_UTILITY);
        assert_eq!(spec.cost_object_id, None, "不挂对象 = 车间费用池");
        assert_eq!(spec.source_type, SOURCE_MANUAL);
        assert_eq!(spec.dept_name, "机加工车间");
        assert_eq!(spec.remark.as_deref(), Some("车间电费"));
        assert_eq!(spec.amount, dec("4000"));
        assert_eq!(spec.credit_account_code, "1002");

        // 挂了对象 ⇒ 落到规格里（是否 OPEN 由 handler 查库判）
        let mut req = sample_req();
        req.cost_object_id = Some("018f4b2c-1111-7222-8333-444455556677".to_string());
        let spec = spec_from_create(&req, "机加工车间").expect("带对象");
        assert!(spec.cost_object_id.is_some());

        // 车间名不能是空白（快照列 NOT NULL 且有语义）
        assert!(spec_from_create(&sample_req(), "   ").is_err());
    }

    /// 缺字段的报文必须点名是哪个字段（不要「参数错误」四个字）。
    #[test]
    fn missing_fields_are_named() {
        let mut req = sample_req();
        req.period_code = Some("2026-13".to_string());
        assert!(
            spec_from_create(&req, "车间")
                .expect_err("13 月")
                .msg
                .contains("period_code")
        );

        let mut req = sample_req();
        req.amount = None;
        assert!(
            spec_from_create(&req, "车间")
                .expect_err("缺金额")
                .msg
                .contains("amount")
        );

        let mut req = sample_req();
        req.dept_id = Some("not-a-uuid".to_string());
        assert!(
            spec_from_create(&req, "车间")
                .expect_err("坏 UUID")
                .msg
                .contains("dept_id")
        );

        let mut req = sample_req();
        req.credit_account_code = None;
        assert!(
            spec_from_create(&req, "车间")
                .expect_err("缺对方科目")
                .msg
                .contains("credit_account_code")
        );

        let mut req = sample_req();
        req.remark = Some("备".repeat(REMARK_MAX + 1));
        assert!(
            spec_from_create(&req, "车间")
                .expect_err("备注超长")
                .msg
                .contains("remark")
        );
    }

    // ---------- 库错误翻译 / 文案 ----------

    /// 库层两条 CHECK 都要翻译成与入口一致的人话码（绕过 handler 直插时也看得懂）。
    #[test]
    fn db_check_violations_map_to_the_same_codes() {
        let spec = spec_from_create(&sample_req(), "机加工车间").expect("规格");
        let amount_error = map_write_error(
            DbError::CheckViolation {
                constraint: Some("ck_biz_cost_oh_amount".to_string()),
                source: None,
            },
            &spec,
            "单测",
        );
        assert_eq!(amount_error.code, 66061);
        let debit_error = map_write_error(
            DbError::CheckViolation {
                constraint: Some("ck_biz_cost_oh_debit".to_string()),
                source: None,
            },
            &spec,
            "单测",
        );
        assert_eq!(debit_error.code, 66060);
        let other_error = map_write_error(
            DbError::CheckViolation {
                constraint: Some("ck_biz_cost_oh_item".to_string()),
                source: None,
            },
            &spec,
            "单测",
        );
        assert_eq!(other_error.status, 400);
        assert!(other_error.msg.contains("ck_biz_cost_oh_item"), "{}", other_error.msg);
    }

    /// 409 文案：对象已关批 / 已归集 都要说清「为什么不能」和「下一步做什么」。
    #[test]
    fn conflict_messages_point_to_next_action() {
        let error = object_not_open("CB-20261101-000001", "CLOSED");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, 66057);
        assert!(error.msg.contains("已关批"), "{}", error.msg);
        assert!(error.msg.contains("成本对象"), "{}", error.msg);
        assert!(error.msg.contains("费用池"), "{}", error.msg);

        let error = already_collected("2026-11");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("2026-11"), "{}", error.msg);
        assert!(error.msg.contains("重算"), "要给出下一步动作：{}", error.msg);
    }

    /// 值域与 DDL 对齐（改 CHECK 必须同步改这里，否则入口放行、库层拒）。
    #[test]
    fn value_domains_match_the_ddl() {
        assert_eq!(SOURCE_TYPES, [SOURCE_MANUAL, SOURCE_IMPORT]);
        assert_eq!(AMOUNT_SCALE, 4, "amount NUMERIC(18,4)");
        assert_eq!(ACCOUNT_CODE_MAX, 30, "account_code VARCHAR(30)");
        assert_eq!(EXPENSE_ITEM_MAX, 16, "expense_item VARCHAR(16)");
        assert_eq!(REMARK_MAX, 255, "remark VARCHAR(255)");
        assert_eq!(PERIOD_LEN, 7, "period_code VARCHAR(7)");
        assert!(DEFAULT_DEBIT_ACCOUNT.starts_with(DEBIT_ACCOUNT_PREFIX));
    }
}
