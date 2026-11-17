//! 现金流底稿**录入 + 勾稽校验（直接法）**（M1-14 · 表 `biz_cashflow_worksheet` / 迁移 `V0414` 建表、`V0415` 种权限码）。
//!
//! 底稿 = 现金流量表的**编制依据**：一行 = 项目 × 期间 × 金额（`line_type = FLOW`），
//! 另加每期间一份**期初**（`OPENING`）与**期末**（`CLOSING`）余额行。直接法的硬等式只有一条：
//!
//! ```text
//! 期初余额 + 流入合计 − 流出合计 = 期末余额
//! ```
//!
//! 本模块只做两件事：**把底稿写进去**（upsert）与**证明它闭合**（勾稽校验）。
//! 由底稿出表是 M1-16、间接法调整项是 M1-15，都不在本文件。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/report/cash-flow/worksheet?period=2026-11` | `business:erp:cashflow-worksheet:list` | 读整张底稿（配置项 ∪ 已录入行）+ 小计 + 勾稽结果 |
//! | PUT | `/business/erp/report/cash-flow/worksheet` | `business:erp:cashflow-worksheet:save` | 录入（本期间明细行 / 期初 / 期末，同一事务 upsert） |
//! | POST | `/business/erp/report/cash-flow/worksheet/check` | `business:erp:cashflow-worksheet:list` | 勾稽校验（**不落库**；把草稿叠加到已存底稿上算一遍） |
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/...`（AGENTS.md 约定 8）。
//!
//! # 为什么两个码：读 / 写分开
//!
//! 读底稿是**看报表依据**（财务经理、财务总监、审计都要看），写底稿是**改报表依据**
//! （改错一分钱，出的现金流量表就错一分钱，而且看不出来）。与「汇率维护 vs 汇兑口径」
//! （V0411）同一条理由。校验接口是只读的（不写一个字节）⇒ 复用读码，不另开。
//!
//! # 写入语义（upsert，不是 append）
//!
//! 一个期间里一个项目**只有一行**、期初期末各只有一行 —— 这是 `V0414` 两条**部分唯一索引**
//! 钉死的库层事实（`uk_biz_cashflow_ws_item` / `uk_biz_cashflow_ws_balance`）。所以录入走
//! `INSERT … ON CONFLICT … DO UPDATE`：重复提交同一张底稿不会攒出两行同项目金额
//! （两行 = 报表把同一笔现金流算两遍，而且谁都不会发现）。
//!
//! # 三条快照列由**服务端**从配置表取（不信客户端）
//!
//! 请求体只给 `item_code` + `amount`（+ 备注）。`item_name` / `activity` / `direction` / `sort_order`
//! 一律从 `biz_cashflow_item` 读出来写进去：
//!
//! * 客户端改了方向 ⇒ 勾稽式左边凭平移项、流出变流入，**自己就能把不平“调平”**；
//! * 项目改名 / 停用后底稿仍按当时的名称与方向呈现（AGENTS.md 约定 13「历史类存快照」）。
//!
//! # 保存允许不平，校验（出表）才要求闭合
//!
//! 底稿是**逐行编制**的工作稿：会计经常先录完流入、期末余额最后才对得上。若保存就强制平，
//! 用户只能先编一个假数把单存进去 —— 那才是真正的账务风险。所以：
//!
//! ```text
//! PUT   保存      → 200，返回 check{balanced:false, diff:…}（不阻断，差多少写清楚）
//! POST  校验      → 200，同样返回 check；只有 require_balanced=true 时才 400 + 66087
//! ```
//!
//! 缺期初 / 缺期末在 `check.problems` 里逐条点名（`OPENING_MISSING` / `CLOSING_MISSING`），
//! **不**让 GET 报错 —— 页面要能打开一张还没编完的底稿（这与 `fx_settings` 的 GET 不因
//! 「配置坏了」而报错同一条理由：报错就没入口修）。
//!
//! # 期间闸（66085）
//!
//! 只有 `OPEN` 的期间可写底稿。`NOT_OPENED`（窗口没开，本期凭证根本不在这个月）与
//! `CLOSED`（账已定稿并对外报出）都拒 —— 后者若可写，同一期间就会出现「报表说 A、底稿说 B」。
//! 读不受限（历史底稿、已结账期间都要能看），返回 `editable: false` 让页面置灰。
//!
//! # 项目闸（66086）
//!
//! 写入的项目必须**存在且启用**。编码是弱引用（V0414 不建外键，见其头注释），所以闸在写入口：
//! 编码不存在 ⇒ 该行没有方向，勾稽时算不进流入 / 流出，金额凭空消失而勾稽「反而是平的」。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_cashflow_worksheet 是**全局报表工作底稿**（V0414 无 dept_id / 归属人列），
//! biz_cashflow_item 是报表分类配置、biz_accounting_period 是全局会计日历 —— 三者都是
//! 公司级口径，数据范围（本部门 / 本人）表达不出来。可见性由
//! business:erp:cashflow-worksheet:list（读 / 校验）与 :save（写）两个码控制，无分页查询。
//! ```
//!
//! # 不碰的东西
//!
//! 底稿不是单据：不生成凭证、不进流程引擎、不注册 `DocKind` / `LEGAL_EDGES`、不读写
//! `biz_voucher*`。本文件也不改分类配置（只读 `biz_cashflow_item`）、不改期间状态
//! （开账 / 结账在「会计期间」页）。

use axum::Json;
use axum::extract::Extension;
use nik::prelude::chrono::{Datelike, Local};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::cash_flow::ACTIVITIES;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 常量 ====================

/// 底稿行表（`V0414`）。
pub const WORKSHEET_TABLE: &str = "biz_cashflow_worksheet";

/// 现金流量项目分类配置（`V0185`）：本模块只读它取三个快照列。
pub const ITEM_TABLE: &str = "biz_cashflow_item";

/// 会计期间（会计日历）：本模块只读 `status`。
pub const PERIOD_TABLE: &str = "biz_accounting_period";

/// 明细行（项目 × 期间 × 金额）。
pub const LINE_FLOW: &str = "FLOW";
/// 期初现金及现金等价物余额行。
pub const LINE_OPENING: &str = "OPENING";
/// 期末现金及现金等价物余额行。
pub const LINE_CLOSING: &str = "CLOSING";

/// 流入。
pub const DIRECTION_INFLOW: &str = "INFLOW";
/// 流出。
pub const DIRECTION_OUTFLOW: &str = "OUTFLOW";

/// 期间状态：可写。
pub const PERIOD_OPEN: &str = "OPEN";

/// 期初余额行的项目名（表里 `item_name` 非空，余额行没有配置可抄）。
pub const OPENING_NAME: &str = "期初现金及现金等价物余额";
/// 期末余额行的项目名。
pub const CLOSING_NAME: &str = "期末现金及现金等价物余额";

/// 期初行的显示顺序（排在所有明细行之前）。
pub const OPENING_SORT: i32 = 1;
/// 期末行的显示顺序（排在所有明细行之后）。
pub const CLOSING_SORT: i32 = 9999;

/// 读 / 校验接口权限码（挂「现金流量表」页面 `…001018` 的按钮行，`V0415`）。
pub const PERM_WORKSHEET_LIST: &str = "business:erp:cashflow-worksheet:list";
/// 写接口权限码（同上）。
pub const PERM_WORKSHEET_SAVE: &str = "business:erp:cashflow-worksheet:save";

/// 本模块**新增**的权限码清单（供 `perm_menu_gate.py` 对账）。
pub const PERMISSIONS: [&str; 2] = [PERM_WORKSHEET_LIST, PERM_WORKSHEET_SAVE];

/// 一次最多提交的明细行数（配置表目前 12 行；给到 500 是防「把整本凭证当行传」的坏请求）。
pub const MAX_ITEMS: usize = 500;

/// 金额小数位（`amount NUMERIC(18,4)`，与 V0414 同口径）。
pub const AMOUNT_SCALE: u32 = 4;

/// `NUMERIC(18,4)` 的整数部分上限（14 位）：超了 PG 会报 22003，不如入口先说清楚。
pub const AMOUNT_ABS_LIMIT: i64 = 100_000_000_000_000;

/// 勾稽问题的机读码：未录入期初余额。
pub const PROBLEM_OPENING_MISSING: &str = "OPENING_MISSING";
/// 勾稽问题的机读码：未录入期末余额。
pub const PROBLEM_CLOSING_MISSING: &str = "CLOSING_MISSING";
/// 勾稽问题的机读码：金额不平。
pub const PROBLEM_IMBALANCED: &str = "IMBALANCED";
/// 勾稽问题的机读码：一行都没有（空底稿）。
pub const PROBLEM_EMPTY: &str = "EMPTY";

// ==================== 出参 ====================

/// 一条底稿明细行的呈现（已录入 / 未录入都是同一个形状，页面按 `entered` 区分）。
#[derive(Debug, Clone, Serialize)]
pub struct WorksheetRowRes {
    /// 行类型：恒为 `FLOW`（余额行在 `opening` / `closing` 里单列）。
    pub line_type: String,
    /// 项目编码（配置表 `biz_cashflow_item.item_code`）。
    pub item_code: String,
    /// 项目名称：**已录入行取底稿快照**（配置改名后历史底稿仍按当时名称呈现），
    /// 未录入行取当前配置名。
    pub item_name: String,
    /// 活动：`OPERATING` / `INVESTING` / `FINANCING`。
    pub activity: String,
    /// 方向：`INFLOW` / `OUTFLOW`。
    pub direction: String,
    /// 金额（`0` = 未录入或本期为 0）。
    pub amount: Decimal,
    /// 显示顺序（已录入行取快照，未录入行取配置值）。
    pub sort_order: i32,
    /// 编制说明。
    pub remark: Option<String>,
    /// 是否已有底稿行（`false` = 配置里有、本期还没录）。
    pub entered: bool,
    /// 对应的配置项是否仍启用（`false` = 已停用 / 已被删，历史行仍然显示）。
    pub item_active: bool,
}

/// 一条余额行（期初 / 期末）。
#[derive(Debug, Clone, Serialize)]
pub struct BalanceRes {
    /// `OPENING` / `CLOSING`。
    pub line_type: String,
    /// 行名称（`期初现金及现金等价物余额` / `期末现金及现金等价物余额`）。
    pub item_name: String,
    /// 余额（可为负：透支 / 受限资金）。
    pub amount: Decimal,
    /// 编制说明。
    pub remark: Option<String>,
}

/// 一个活动的小计（三大活动各一块，页面直接画「经营活动产生的现金流量净额」）。
#[derive(Debug, Clone, Serialize)]
pub struct ActivityBlockRes {
    /// 活动编码。
    pub code: String,
    /// 活动标题（`一、经营活动产生的现金流量` …）。
    pub name: String,
    /// 流入合计。
    pub inflow: Decimal,
    /// 流出合计。
    pub outflow: Decimal,
    /// 净额（流入 − 流出）。
    pub net: Decimal,
    /// 本活动下已录入的项目行数。
    pub entered_rows: usize,
}

/// 合计区（直接法三行：流入 / 流出 / 净额）。
#[derive(Debug, Clone, Serialize)]
pub struct TotalsRes {
    /// 流入合计（三大活动之和）。
    pub inflow: Decimal,
    /// 流出合计。
    pub outflow: Decimal,
    /// 净增加额（流入 − 流出）。
    pub net: Decimal,
}

/// 一条勾稽问题（机读码 + 人话）。
#[derive(Debug, Clone, Serialize)]
pub struct CheckProblemRes {
    /// 机读码：`OPENING_MISSING` / `CLOSING_MISSING` / `IMBALANCED` / `EMPTY`。
    pub code: String,
    /// 人话说明（带精确差额，前端可原样展示）。
    pub message: String,
}

/// 勾稽校验结果（直接法：期初 + 流入 − 流出 = 期末）。
#[derive(Debug, Clone, Serialize)]
pub struct CheckRes {
    /// 是否闭合（且期初 / 期末都在）。
    pub balanced: bool,
    /// 期初余额（`None` = 未录入）。
    pub opening: Option<Decimal>,
    /// 流入合计。
    pub inflow: Decimal,
    /// 流出合计。
    pub outflow: Decimal,
    /// 按勾稽式算出来的期末余额（`期初 + 流入 − 流出`；缺期初时 `None`）。
    pub expected_closing: Option<Decimal>,
    /// 实际录入的期末余额（`None` = 未录入）。
    pub closing: Option<Decimal>,
    /// 差额（`expected_closing − closing`；缺任一端时 `None`）。
    pub diff: Option<Decimal>,
    /// 一句话结论（闭合 / 差多少 / 缺什么）。
    pub message: String,
    /// 逐条问题（闭合时为空数组）。
    pub problems: Vec<CheckProblemRes>,
}

/// 整张底稿（GET 与 PUT 的返回体同一份：页面保存后不用再拉一次）。
#[derive(Debug, Clone, Serialize)]
pub struct WorksheetRes {
    /// 期间 `YYYY-MM`。
    pub period: String,
    /// 期间状态：`OPEN` / `CLOSED` / `NOT_OPENED`。
    pub period_status: String,
    /// 是否可编辑（= 期间 `OPEN`；前端据此置灰录入框）。
    pub editable: bool,
    /// 明细行（按 `sort_order`、`item_code` 升序，含未录入的配置项）。
    pub rows: Vec<WorksheetRowRes>,
    /// 期初余额行（`None` = 未录入）。
    pub opening: Option<BalanceRes>,
    /// 期末余额行（`None` = 未录入）。
    pub closing: Option<BalanceRes>,
    /// 三大活动小计（顺序固定 OPERATING / INVESTING / FINANCING）。
    pub activities: Vec<ActivityBlockRes>,
    /// 流入 / 流出 / 净额合计。
    pub totals: TotalsRes,
    /// 勾稽结果。
    pub check: CheckRes,
}

// ==================== 入参 ====================

/// 读底稿的查询参数。
#[derive(Debug, Clone, Deserialize)]
pub struct WorksheetQuery {
    /// 期间 `YYYY-MM`；缺省 = 当前自然月（页面打开即本期）。
    pub period: Option<String>,
}

/// 一条余额行的录入（期初 / 期末）。
#[derive(Debug, Clone, Deserialize)]
pub struct BalanceIn {
    /// 余额（可负：透支 / 受限资金）。
    pub amount: Decimal,
    /// 编制说明。
    pub remark: Option<String>,
}

/// 一条明细行的录入（`item_code` + 金额 + 备注；三个快照列由服务端从配置取）。
#[derive(Debug, Clone, Deserialize)]
pub struct WorksheetItemIn {
    /// 项目编码。
    pub item_code: String,
    /// 本期发生额（≥ 0；方向由配置的 `direction` 决定）。
    pub amount: Decimal,
    /// 编制说明。
    pub remark: Option<String>,
}

/// 录入请求（PUT）：只写传了的字段 —— `opening` / `closing` 为 `None` 表示「本期这项不动」，
/// `items` 为空表示「本期明细行不动」，三者全空则拒绝（空请求不该发）。
#[derive(Debug, Clone, Deserialize)]
pub struct SaveWorksheetReq {
    /// 期间 `YYYY-MM`。
    pub period: String,
    /// 期初余额（`None` = 不改）。
    pub opening: Option<BalanceIn>,
    /// 期末余额（`None` = 不改）。
    pub closing: Option<BalanceIn>,
    /// 明细行（按 `item_code` upsert；空数组 = 不改）。
    #[serde(default)]
    pub items: Vec<WorksheetItemIn>,
}

/// 校验请求（POST）：形状与录入相同，但**一个字节都不写库**，只把草稿叠加到已存底稿上算一遍。
#[derive(Debug, Clone, Deserialize)]
pub struct CheckWorksheetReq {
    /// 期间 `YYYY-MM`。
    pub period: String,
    /// 期初余额草稿（`None` = 用已存值）。
    pub opening: Option<BalanceIn>,
    /// 期末余额草稿（`None` = 用已存值）。
    pub closing: Option<BalanceIn>,
    /// 明细行草稿（`item_code` 覆盖已存值；未提到的行用已存值）。
    #[serde(default)]
    pub items: Vec<WorksheetItemIn>,
    /// 是否要求闭合（`true` = 不平就 `400 + 66087`；出表前的复核闸用它）。
    #[serde(default)]
    pub require_balanced: bool,
}

// ==================== 错误出口 ====================

/// `400 + 10001`（参数形状层面：期间格式 / 金额精度 / 重复行 / 空请求）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `400 + 66085`（期间状态不允许写）。
fn period_locked(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_CASHFLOW_WORKSHEET_PERIOD_LOCKED, message.into())
}

/// `400 + 66086`（项目编码不存在 / 已停用）。
fn item_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_CASHFLOW_ITEM_INVALID, message.into())
}

// ==================== 纯逻辑（可单测，不碰库）====================

/// 归一化并校验期间：`trim` 后必须是 `YYYY-MM` 且月份 1–12。
pub fn normalize_period(raw: &str) -> std::result::Result<String, String> {
    let trimmed = raw.trim();
    let bytes = trimmed.as_bytes();
    let shape_ok = bytes.len() == 7
        && bytes[4] == b'-'
        && bytes[..4].iter().all(|byte| byte.is_ascii_digit())
        && bytes[5..].iter().all(|byte| byte.is_ascii_digit());
    if !shape_ok {
        return Err(format!("period 必须是 `YYYY-MM`（如 2026-11），收到「{trimmed}」"));
    }
    let month: u32 = trimmed[5..]
        .parse()
        .map_err(|_| format!("period 的月份部分必须是两位数字（如 2026-11），收到「{trimmed}」"))?;
    if !(1..=12).contains(&month) {
        return Err(format!("period 的月份必须是 01–12，收到「{trimmed}」"));
    }
    Ok(trimmed.to_string())
}

/// 归一化项目编码（`trim` + 非空 + ≤32 字符 + 无控制字符）。
pub fn normalize_item_code(raw: &str) -> std::result::Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("item_code 不能为空（明细行必须挂到一个现金流量项目上）".to_string());
    }
    if trimmed.chars().count() > 32 {
        return Err(format!(
            "item_code 最长 32 个字符（与 biz_cashflow_item.item_code 同宽），收到 {} 个字符",
            trimmed.chars().count()
        ));
    }
    if trimmed.chars().any(|ch| ch.is_control()) {
        return Err(format!("item_code 不能含控制字符（收到「{trimmed}」）"));
    }
    Ok(trimmed.to_string())
}

/// 校验金额：不超过 `NUMERIC(18,4)` 的位宽，且**小数位不超过 4**。
///
/// 后者是静默错账的来源：`INSERT … 1.23456` 会被 PG 四舍五入成 `1.2346` 而不报错，
/// 于是「底稿合计 ≠ 凭证合计」，差的那点钱查起来极痛。入口先拒，让人自己决定舍去哪里。
pub fn check_amount(amount: Decimal, field: &str, allow_negative: bool) -> std::result::Result<(), String> {
    if !allow_negative && amount.is_sign_negative() && !amount.is_zero() {
        return Err(format!(
            "{} 不能为负数（{amount}）：方向由现金流量项目的 inflow/outflow 表达，\
             用负数会让流入合计出现负流入、与流出栏互相抵消",
            field
        ));
    }
    if amount.round_dp(AMOUNT_SCALE) != amount {
        return Err(format!(
            "{} 最多 4 位小数（{amount}）：底稿金额是 NUMERIC(18,4)，多出的小数位会被数据库\
             四舍五入且不报错，会造成「底稿合计 ≠ 凭证合计」的尾差",
            field
        ));
    }
    let limit = Decimal::from(AMOUNT_ABS_LIMIT);
    if amount.abs() >= limit {
        return Err(format!(
            "{} 超出 NUMERIC(18,4) 的表示范围（|金额| 必须小于 1e14），收到 {amount}",
            field
        ));
    }
    Ok(())
}

/// 归一化备注（`trim` + 空串归一成 `None` + 255 字截断）。
pub fn normalize_remark(remark: Option<String>) -> Option<String> {
    remark
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(255).collect::<String>())
}

/// 参与勾稽的一行（配置快照 + 金额）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowLine {
    /// 项目编码。
    pub item_code: String,
    /// 项目名称（快照）。
    pub item_name: String,
    /// 活动（`OPERATING` / `INVESTING` / `FINANCING`）。
    pub activity: String,
    /// 方向（`INFLOW` / `OUTFLOW`）。
    pub direction: String,
    /// 金额（≥ 0）。
    pub amount: Decimal,
}

/// **勾稽校验（直接法）**：`期初 + 流入 − 流出 = 期末`。
///
/// 纯函数：不碰库、不看配置，只吃「两个余额 + 明细行」，好单测也好复用（M1-16 出表前复核）。
///
/// 判定口径：
///
/// * 缺期初 / 缺期末 ⇒ `balanced = false` + `problems` 点名（**不**把缺行当 0：0 是「这一期
///   确实没有现金」还是「忘了录」，这两件事对报表是反的）；
/// * 有期初有期末 ⇒ 按 4 位小数精确比较，`diff != 0` 即不平，`diff` 原样给出（**绝不四舍五入抹平**）；
/// * 一行明细都没有且两个余额都缺 ⇒ 额外报 `EMPTY`（页面据此提示「本期还没编」）。
pub fn reconcile(opening: Option<Decimal>, flows: &[FlowLine], closing: Option<Decimal>) -> CheckRes {
    let mut inflow = Decimal::ZERO;
    let mut outflow = Decimal::ZERO;
    for line in flows {
        if line.direction == DIRECTION_INFLOW {
            inflow += line.amount;
        } else {
            outflow += line.amount;
        }
    }
    let net = inflow - outflow;
    let expected_closing = opening.map(|value| value + net);
    let diff = match (expected_closing, closing) {
        (Some(expected), Some(actual)) => Some(expected - actual),
        _ => None,
    };

    let mut problems: Vec<CheckProblemRes> = Vec::new();
    if opening.is_none() {
        problems.push(CheckProblemRes {
            code: PROBLEM_OPENING_MISSING.to_string(),
            message: format!(
                "未录入「{OPENING_NAME}」：勾稽的左端缺失，算不出期末应该是多少。\
                 请在底稿里填期初余额（或先在「期初余额」页确认上期已结转）"
            ),
        });
    }
    if closing.is_none() {
        problems.push(CheckProblemRes {
            code: PROBLEM_CLOSING_MISSING.to_string(),
            message: format!(
                "未录入「{CLOSING_NAME}」：勾稽的右端缺失。期末余额 = 期初 + 流入 − 流出，\
                 必须显式录入才能校验"
            ),
        });
    }
    if flows.is_empty() {
        problems.push(CheckProblemRes {
            code: PROBLEM_EMPTY.to_string(),
            message: "本期底稿一行明细都没有：请先录入各现金流量项目的本期发生额".to_string(),
        });
    }
    if let Some(diff) = diff
        && !diff.is_zero()
    {
        problems.push(CheckProblemRes {
            code: PROBLEM_IMBALANCED.to_string(),
            message: format!(
                "勾稽不闭合：期初 {} + 流入 {} − 流出 {} = {}，但录的期末是 {}，差 {}",
                money(opening),
                money(Some(inflow)),
                money(Some(outflow)),
                money(expected_closing),
                money(closing),
                money(Some(diff))
            ),
        });
    }

    let balanced = opening.is_some() && closing.is_some() && diff.is_some_and(|value| value.is_zero());
    let message = if balanced {
        format!(
            "勾稽闭合：期初 {} + 流入 {} − 流出 {} = 期末 {}",
            money(opening),
            money(Some(inflow)),
            money(Some(outflow)),
            money(closing)
        )
    } else if let Some(diff) = diff {
        format!(
            "勾稽不闭合：按「期初 + 流入 − 流出」应为 {}，实际期末 {}，差 {}",
            money(expected_closing),
            money(closing),
            money(Some(diff))
        )
    } else if opening.is_none() {
        "勾稽无法校验：未录入期初余额".to_string()
    } else {
        "勾稽无法校验：未录入期末余额".to_string()
    };

    CheckRes {
        balanced,
        opening,
        inflow,
        outflow,
        expected_closing,
        closing,
        diff,
        message,
        problems,
    }
}

/// 金额的定点文本（4 位小数；报文 / 单测断言都用它，避免浮点打印差异）。
pub fn money(value: Option<Decimal>) -> String {
    match value {
        Some(value) => format!("{:.4}", value.round_dp(AMOUNT_SCALE)),
        None => "—".to_string(),
    }
}

/// 活动标题（拿不到就用编码本身兜底）。
pub fn activity_name(code: &str) -> String {
    ACTIVITIES
        .iter()
        .find(|(key, _)| *key == code)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| code.to_string())
}

// ==================== 数据层 ====================

/// 分类配置的一行（只取本模块要用的列）。
#[derive(Debug, Clone)]
pub struct ConfigItem {
    /// 项目编码。
    pub item_code: String,
    /// 项目名称。
    pub item_name: String,
    /// 活动。
    pub activity: String,
    /// 方向。
    pub direction: String,
    /// 显示顺序。
    pub sort_order: i32,
    /// 是否启用（`1` = 启用）。
    pub is_active: i16,
}

/// 已存的底稿行。
#[derive(Debug, Clone)]
pub struct StoredRow {
    /// 行类型。
    pub line_type: String,
    /// 项目编码（余额行为 `None`）。
    pub item_code: Option<String>,
    /// 项目名称快照。
    pub item_name: String,
    /// 活动快照。
    pub activity: Option<String>,
    /// 方向快照。
    pub direction: Option<String>,
    /// 金额。
    pub amount: Decimal,
    /// 显示顺序。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
}

/// 期间状态（`None` = 该期间在会计日历里不存在）。
pub async fn load_period_status(ex: &impl SqlExecutor, period: &str) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: biz_accounting_period 是全局会计日历（无归属列），期间闸只需状态。
    let sql = format!("SELECT status FROM {PERIOD_TABLE} WHERE period_code = ?");
    let row = ex.query_first_sql(sql, vec![Value::Text(period.to_string())]).await?;
    match row {
        Some(row) => Ok(Some(row.get_typed::<String>("status")?)),
        None => Ok(None),
    }
}

/// 全部现金流量项目（含**停用**行：历史底稿要能显示它们，`is_active` 由调用方判断）。
pub async fn load_config_items(ex: &impl SqlExecutor) -> DbResult<Vec<ConfigItem>> {
    // SCOPE-EXEMPT: biz_cashflow_item 是报表分类配置（全局，无归属列）。
    let sql = format!(
        "SELECT item_code, item_name, activity, direction, sort_order, is_active \
           FROM {ITEM_TABLE} ORDER BY activity, sort_order, item_code"
    );
    let rows = ex.query_sql(sql, Vec::new()).await?;
    rows.iter()
        .map(|row| {
            Ok(ConfigItem {
                item_code: row.get_typed("item_code")?,
                item_name: row.get_typed("item_name")?,
                activity: row.get_typed("activity")?,
                direction: row.get_typed("direction")?,
                sort_order: row.get_typed("sort_order")?,
                is_active: row.get_typed("is_active")?,
            })
        })
        .collect()
}

/// 某期间的全部底稿行（明细 + 期初 + 期末）。
pub async fn load_rows(ex: &impl SqlExecutor, period: &str) -> DbResult<Vec<StoredRow>> {
    // SCOPE-EXEMPT: biz_cashflow_worksheet 是全局报表工作底稿（无归属列），一次取整张。
    let sql = format!(
        "SELECT line_type, item_code, item_name, activity, direction, amount, sort_order, remark \
           FROM {WORKSHEET_TABLE} WHERE period_code = ? ORDER BY sort_order, item_code"
    );
    let rows = ex.query_sql(sql, vec![Value::Text(period.to_string())]).await?;
    rows.iter()
        .map(|row| {
            Ok(StoredRow {
                line_type: row.get_typed("line_type")?,
                item_code: row.get_typed::<Option<String>>("item_code")?,
                item_name: row.get_typed("item_name")?,
                activity: row.get_typed::<Option<String>>("activity")?,
                direction: row.get_typed::<Option<String>>("direction")?,
                amount: row.get_typed("amount")?,
                sort_order: row.get_typed("sort_order")?,
                remark: row.get_typed::<Option<String>>("remark")?,
            })
        })
        .collect()
}

/// 把「配置项 ∪ 已录行」拼成整张底稿，并算小计与勾稽。
///
/// 三条合并规则（都有理由，别随手改）：
///
/// 1. **已录行为准**：名称 / 活动 / 方向 / 顺序取底稿快照，不取现配置 —— 配置改名后历史底稿
///    必须仍按当时名称呈现（AGENTS.md 约定 13）。
/// 2. **未录的配置项也要出**：录入页只有把 12 个待录项都列出来，会计才知道还差哪几行。
/// 3. **配置已没有的已录行照样出**（`item_active = false`）：那是历史编制结果，静默消失就是丢数。
pub fn compose_sheet(period: &str, period_status: &str, config: &[ConfigItem], stored: &[StoredRow]) -> WorksheetRes {
    let editable = period_status == PERIOD_OPEN;
    let config_by_code: std::collections::HashMap<&str, &ConfigItem> =
        config.iter().map(|item| (item.item_code.as_str(), item)).collect();

    let mut rows: Vec<WorksheetRowRes> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut flows: Vec<FlowLine> = Vec::new();
    let mut opening: Option<BalanceRes> = None;
    let mut closing: Option<BalanceRes> = None;
    let stored_flows: Vec<&StoredRow> = stored.iter().filter(|row| row.line_type == LINE_FLOW).collect();
    for row in &stored_flows {
        if let Some(code) = &row.item_code {
            seen.insert(code.clone());
        }
    }

    for row in &stored_flows {
        let Some(code) = row.item_code.clone() else { continue };
        let activity = row.activity.clone().unwrap_or_else(|| "OPERATING".to_string());
        let direction = row.direction.clone().unwrap_or_else(|| DIRECTION_INFLOW.to_string());
        rows.push(WorksheetRowRes {
            line_type: LINE_FLOW.to_string(),
            item_code: code.clone(),
            item_name: row.item_name.clone(),
            activity: activity.clone(),
            direction: direction.clone(),
            amount: row.amount,
            sort_order: row.sort_order,
            remark: row.remark.clone(),
            entered: true,
            item_active: config_by_code
                .get(code.as_str())
                .is_some_and(|item| item.is_active == 1),
        });
        flows.push(FlowLine {
            item_code: code,
            item_name: row.item_name.clone(),
            activity,
            direction,
            amount: row.amount,
        });
    }

    for item in config {
        if seen.contains(&item.item_code) {
            continue;
        }
        rows.push(WorksheetRowRes {
            line_type: LINE_FLOW.to_string(),
            item_code: item.item_code.clone(),
            item_name: item.item_name.clone(),
            activity: item.activity.clone(),
            direction: item.direction.clone(),
            amount: Decimal::ZERO,
            sort_order: item.sort_order,
            remark: None,
            entered: false,
            item_active: item.is_active == 1,
        });
    }

    // 顺序：活动（经营 → 投资 → 筹资）→ sort_order → item_code（与配置页一致）。
    let activity_rank = |code: &str| -> usize {
        ACTIVITIES
            .iter()
            .position(|(key, _)| *key == code)
            .unwrap_or(ACTIVITIES.len())
    };
    rows.sort_by(|left, right| {
        activity_rank(&left.activity)
            .cmp(&activity_rank(&right.activity))
            .then(left.sort_order.cmp(&right.sort_order))
            .then(left.item_code.cmp(&right.item_code))
    });

    for row in stored {
        match row.line_type.as_str() {
            LINE_OPENING => {
                opening = Some(BalanceRes {
                    line_type: LINE_OPENING.to_string(),
                    item_name: row.item_name.clone(),
                    amount: row.amount,
                    remark: row.remark.clone(),
                })
            }
            LINE_CLOSING => {
                closing = Some(BalanceRes {
                    line_type: LINE_CLOSING.to_string(),
                    item_name: row.item_name.clone(),
                    amount: row.amount,
                    remark: row.remark.clone(),
                })
            }
            _ => {}
        }
    }

    let activities: Vec<ActivityBlockRes> = ACTIVITIES
        .iter()
        .map(|(code, name)| {
            let mut inflow = Decimal::ZERO;
            let mut outflow = Decimal::ZERO;
            let mut entered_rows = 0usize;
            for row in rows.iter().filter(|row| row.activity == *code) {
                if row.entered {
                    entered_rows += 1;
                }
                if row.direction == DIRECTION_INFLOW {
                    inflow += row.amount;
                } else {
                    outflow += row.amount;
                }
            }
            ActivityBlockRes {
                code: (*code).to_string(),
                name: (*name).to_string(),
                inflow,
                outflow,
                net: inflow - outflow,
                entered_rows,
            }
        })
        .collect();

    let inflow: Decimal = activities.iter().map(|block| block.inflow).sum();
    let outflow: Decimal = activities.iter().map(|block| block.outflow).sum();
    let check = reconcile(
        opening.as_ref().map(|row| row.amount),
        &flows,
        closing.as_ref().map(|row| row.amount),
    );

    WorksheetRes {
        period: period.to_string(),
        period_status: period_status.to_string(),
        editable,
        rows,
        opening,
        closing,
        activities,
        totals: TotalsRes {
            inflow,
            outflow,
            net: inflow - outflow,
        },
        check,
    }
}

/// 读整张底稿（GET 的唯一实现，PUT 保存后也走它回读）。
pub async fn load_sheet(period: &str) -> std::result::Result<WorksheetRes, ApiError> {
    let status = load_period_status(db(), period)
        .await
        .map_err(|error| utils::db_error(error, "查询会计期间状态"))?;
    let Some(status) = status else {
        return Err(ApiError::new(
            400,
            error_code::ERP_PERIOD_NOT_FOUND,
            format!(
                "会计期间 {period} 不存在（biz_accounting_period 无记录）：现金流底稿只能对已开账的期间编制，\
                 请先在「会计期间」里开启该期间"
            ),
        ));
    };
    let config = load_config_items(db())
        .await
        .map_err(|error| utils::db_error(error, "查询现金流量项目配置"))?;
    let stored = load_rows(db(), period)
        .await
        .map_err(|error| utils::db_error(error, "查询现金流底稿"))?;
    Ok(compose_sheet(period, &status, &config, &stored))
}

/// 一次写入里的一条明细行（已经过校验 + 补齐快照）。
#[derive(Debug, Clone)]
pub struct FlowWrite {
    /// 项目编码。
    pub item_code: String,
    /// 项目名称快照。
    pub item_name: String,
    /// 活动快照。
    pub activity: String,
    /// 方向快照。
    pub direction: String,
    /// 金额。
    pub amount: Decimal,
    /// 显示顺序快照。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
}

/// 把请求里的明细行按配置补齐快照并校验（**写库前的唯一一道闸**，GET 不经过它）。
///
/// 返回 `Vec<FlowWrite>`：顺序与请求一致，重复 `item_code` 直接拒（同一项目两笔金额无法判断以哪个为准）。
pub fn prepare_flow_writes(
    items: &[WorksheetItemIn],
    config: &[ConfigItem],
) -> std::result::Result<Vec<FlowWrite>, ApiError> {
    if items.len() > MAX_ITEMS {
        return Err(bad_request(format!(
            "一次最多提交 {MAX_ITEMS} 行（当前 {} 行）",
            items.len()
        )));
    }
    let config_by_code: std::collections::HashMap<&str, &ConfigItem> =
        config.iter().map(|item| (item.item_code.as_str(), item)).collect();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut writes: Vec<FlowWrite> = Vec::with_capacity(items.len());

    for (index, item) in items.iter().enumerate() {
        let code = normalize_item_code(&item.item_code)
            .map_err(|message| bad_request(format!("items[{index}]：{message}")))?;
        if !seen.insert(code.clone()) {
            return Err(bad_request(format!(
                "items[{index}]：项目 `{code}` 在本批里重复出现 —— 同一期间一个项目只能有一笔金额，\
                 请合并后重提"
            )));
        }
        let Some(config_item) = config_by_code.get(code.as_str()) else {
            return Err(item_invalid(format!(
                "items[{index}]：现金流量项目 `{code}` 不存在。底稿行必须挂到已配置的项目上\
                 （否则它没有流入/流出的方向，勾稽会算错），请先到「现金流量项目」里新建该项目"
            )));
        };
        if config_item.is_active != 1 {
            return Err(item_invalid(format!(
                "items[{index}]：现金流量项目 `{code}`（{}）已停用。停用表示这一行不再编制，\
                 不能再往里写数；若确实需要，请先到「现金流量项目」里启用它",
                config_item.item_name
            )));
        }
        check_amount(item.amount, &format!("items[{index}].amount"), false).map_err(bad_request)?;
        writes.push(FlowWrite {
            item_code: code,
            item_name: config_item.item_name.clone(),
            activity: config_item.activity.clone(),
            direction: config_item.direction.clone(),
            amount: item.amount,
            sort_order: config_item.sort_order,
            remark: normalize_remark(item.remark.clone()),
        });
    }
    Ok(writes)
}

/// 校验并归一化一条余额行（`line_type` 只允许 OPENING / CLOSING）。
fn prepare_balance(
    balance: &BalanceIn,
    line_type: &str,
    field: &str,
) -> std::result::Result<(Decimal, Option<String>), ApiError> {
    debug_assert!(line_type == LINE_OPENING || line_type == LINE_CLOSING);
    // 余额允许负数：银行透支、受限资金被占用时期末现金确实可以是负的。
    check_amount(balance.amount, field, true).map_err(bad_request)?;
    Ok((balance.amount, normalize_remark(balance.remark.clone())))
}

/// 一条余额行的写入（期初 / 期末共用一个形状）。
#[derive(Debug, Clone)]
pub struct BalanceWrite {
    /// `OPENING` / `CLOSING`。
    pub line_type: String,
    /// 行名称（`期初现金及现金等价物余额` / `期末现金及现金等价物余额`）。
    pub item_name: String,
    /// 显示顺序。
    pub sort_order: i32,
    /// 余额（可负）。
    pub amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
}

/// 写一条余额行（部分唯一索引 `uk_biz_cashflow_ws_balance` 做冲突键）。
async fn upsert_balance(tx: &Transaction, period: &str, write: &BalanceWrite, operator: Option<Uuid>) -> DbResult<u64> {
    let sql = format!(
        "INSERT INTO {WORKSHEET_TABLE} \
             (period_code, line_type, item_code, item_name, activity, direction, amount, sort_order, \
              remark, create_by, update_by) \
         VALUES (?, ?, NULL, ?, NULL, NULL, ?::text::numeric, ?, ?, NULLIF(?::text,'')::uuid, \
                 NULLIF(?::text,'')::uuid) \
         ON CONFLICT (period_code, line_type) WHERE line_type <> 'FLOW' DO UPDATE \
            SET item_name = EXCLUDED.item_name, \
                amount = EXCLUDED.amount, \
                sort_order = EXCLUDED.sort_order, \
                remark = EXCLUDED.remark, \
                updated_at = now(), \
                update_by = EXCLUDED.update_by"
    );
    tx.execute_with_params(
        &sql,
        vec![
            Value::Text(period.to_string()),
            Value::Text(write.line_type.clone()),
            Value::Text(write.item_name.clone()),
            Value::Text(write.amount.to_string()),
            Value::I32(write.sort_order),
            Value::Text(write.remark.clone().unwrap_or_default()),
            Value::Text(uuid_text(operator)),
            Value::Text(uuid_text(operator)),
        ],
    )
    .await
}

/// 写一条明细行（部分唯一索引 `uk_biz_cashflow_ws_item` 做冲突键）。
async fn upsert_flow(tx: &Transaction, period: &str, write: &FlowWrite, operator: Option<Uuid>) -> DbResult<u64> {
    let sql = format!(
        "INSERT INTO {WORKSHEET_TABLE} \
             (period_code, line_type, item_code, item_name, activity, direction, amount, sort_order, \
              remark, create_by, update_by) \
         VALUES (?, 'FLOW', ?, ?, ?, ?, ?::text::numeric, ?, ?, NULLIF(?::text,'')::uuid, \
                 NULLIF(?::text,'')::uuid) \
         ON CONFLICT (period_code, item_code) WHERE item_code IS NOT NULL DO UPDATE \
            SET item_name = EXCLUDED.item_name, \
                activity = EXCLUDED.activity, \
                direction = EXCLUDED.direction, \
                amount = EXCLUDED.amount, \
                sort_order = EXCLUDED.sort_order, \
                remark = EXCLUDED.remark, \
                updated_at = now(), \
                update_by = EXCLUDED.update_by"
    );
    tx.execute_with_params(
        &sql,
        vec![
            Value::Text(period.to_string()),
            Value::Text(write.item_code.clone()),
            Value::Text(write.item_name.clone()),
            Value::Text(write.activity.clone()),
            Value::Text(write.direction.clone()),
            Value::Text(write.amount.to_string()),
            Value::I32(write.sort_order),
            Value::Text(write.remark.clone().unwrap_or_default()),
            Value::Text(uuid_text(operator)),
            Value::Text(uuid_text(operator)),
        ],
    )
    .await
}

/// 写底稿（PUT 的实现）：期间闸 → 项目闸 → 同一事务逐行 upsert → 回读整张。
///
/// **事务边界**：三块写入（期初 / 明细 / 期末）在**同一个事务**里，任一行失败整批回滚 ——
/// 不允许出现「期初写进去了、明细没写」的半张底稿（半张底稿的勾稽差额会指向错误的原因）。
pub async fn save_sheet(req: &SaveWorksheetReq, operator: Option<Uuid>) -> std::result::Result<WorksheetRes, ApiError> {
    let period = normalize_period(&req.period).map_err(bad_request)?;
    let has_opening = req.opening.is_some();
    let has_closing = req.closing.is_some();
    if !has_opening && !has_closing && req.items.is_empty() {
        return Err(bad_request(
            "请求里没有任何要保存的内容（opening / closing / items 都为空）—— 空请求不该发",
        ));
    }

    let opening = match &req.opening {
        Some(balance) => {
            let (amount, remark) = prepare_balance(balance, LINE_OPENING, "opening.amount")?;
            Some(BalanceWrite {
                line_type: LINE_OPENING.to_string(),
                item_name: OPENING_NAME.to_string(),
                sort_order: OPENING_SORT,
                amount,
                remark,
            })
        }
        None => None,
    };
    let closing = match &req.closing {
        Some(balance) => {
            let (amount, remark) = prepare_balance(balance, LINE_CLOSING, "closing.amount")?;
            Some(BalanceWrite {
                line_type: LINE_CLOSING.to_string(),
                item_name: CLOSING_NAME.to_string(),
                sort_order: CLOSING_SORT,
                amount,
                remark,
            })
        }
        None => None,
    };

    let config = load_config_items(db())
        .await
        .map_err(|error| utils::db_error(error, "查询现金流量项目配置"))?;
    let writes = prepare_flow_writes(&req.items, &config)?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启底稿保存事务"))?;

    // 期间闸：必须在事务里读（与写入同一快照），否则「读到 OPEN → 结账 → 写入」会漏。
    let status = match load_period_status(&tx, &period).await {
        Ok(status) => status,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "查询会计期间状态"));
        }
    };
    let Some(status) = status else {
        let _ = tx.rollback().await;
        return Err(ApiError::new(
            400,
            error_code::ERP_PERIOD_NOT_FOUND,
            format!("会计期间 {period} 不存在（biz_accounting_period 无记录）：请先在「会计期间」里开启该期间"),
        ));
    };
    if status != PERIOD_OPEN {
        let _ = tx.rollback().await;
        return Err(period_locked(format!(
            "会计期间 {period} 当前状态是 {status}，不允许编制 / 修改现金流底稿：\
             {}。请先在「会计期间」里{}",
            if status == "CLOSED" {
                "账已定稿并对外报出，底稿再改就会出现「报表说 A、底稿说 B」"
            } else {
                "期间窗口还没开，本期凭证不在这个月里，编出来的底稿与账无关"
            },
            if status == "CLOSED" { "反结账" } else { "开账" }
        )));
    }

    let result: DbResult<()> = async {
        if let Some(write) = opening.as_ref() {
            upsert_balance(&tx, &period, write, operator).await?;
        }
        for write in &writes {
            upsert_flow(&tx, &period, write, operator).await?;
        }
        if let Some(write) = closing.as_ref() {
            upsert_balance(&tx, &period, write, operator).await?;
        }
        Ok(())
    }
    .await;

    if let Err(error) = result {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "保存现金流底稿"));
    }
    if let Err(error) = tx.commit().await {
        return Err(utils::db_error(error, "提交底稿保存事务"));
    }
    load_sheet(&period).await
}

/// 勾稽校验（POST 的实现）：把草稿叠加到已存底稿上算一遍，**一个字节都不写库**。
///
/// 叠加规则：请求里出现的 `item_code` 覆盖已存金额；期初 / 期末传了就覆盖。
/// 这也是「保存前先看一眼平不平」的唯一入口（保存本身允许不平，见模块文档）。
pub async fn check_sheet(req: &CheckWorksheetReq) -> std::result::Result<CheckRes, ApiError> {
    let period = normalize_period(&req.period).map_err(bad_request)?;
    let config = load_config_items(db())
        .await
        .map_err(|error| utils::db_error(error, "查询现金流量项目配置"))?;
    let writes = prepare_flow_writes(&req.items, &config)?;
    let stored = load_rows(db(), &period)
        .await
        .map_err(|error| utils::db_error(error, "查询现金流底稿"))?;

    let opening_in = match &req.opening {
        Some(balance) => Some(prepare_balance(balance, LINE_OPENING, "opening.amount")?.0),
        None => None,
    };
    let closing_in = match &req.closing {
        Some(balance) => Some(prepare_balance(balance, LINE_CLOSING, "closing.amount")?.0),
        None => None,
    };

    // 已存行 → 叠加表（draft 覆盖同 code 的行）。
    let mut merged: std::collections::BTreeMap<String, FlowLine> = std::collections::BTreeMap::new();
    for row in stored.iter().filter(|row| row.line_type == LINE_FLOW) {
        let Some(code) = row.item_code.clone() else { continue };
        merged.insert(
            code.clone(),
            FlowLine {
                item_code: code,
                item_name: row.item_name.clone(),
                activity: row.activity.clone().unwrap_or_else(|| "OPERATING".to_string()),
                direction: row.direction.clone().unwrap_or_else(|| DIRECTION_INFLOW.to_string()),
                amount: row.amount,
            },
        );
    }
    for write in &writes {
        merged.insert(
            write.item_code.clone(),
            FlowLine {
                item_code: write.item_code.clone(),
                item_name: write.item_name.clone(),
                activity: write.activity.clone(),
                direction: write.direction.clone(),
                amount: write.amount,
            },
        );
    }

    let stored_opening = stored
        .iter()
        .find(|row| row.line_type == LINE_OPENING)
        .map(|row| row.amount);
    let stored_closing = stored
        .iter()
        .find(|row| row.line_type == LINE_CLOSING)
        .map(|row| row.amount);

    let flows: Vec<FlowLine> = merged.into_values().collect();
    let check = reconcile(opening_in.or(stored_opening), &flows, closing_in.or(stored_closing));
    if req.require_balanced && !check.balanced {
        return Err(ApiError::new(
            400,
            error_code::ERP_CASHFLOW_WORKSHEET_UNBALANCED,
            format!(
                "现金流底稿勾稽不闭合（期间 {period}）：{}。直接法要求「期初 + 流入 − 流出 = 期末」，\
                 差一分钱都不能出表",
                check.message
            ),
        ));
    }
    Ok(check)
}

/// UUID → 参数文本（`NULLIF(?::text,'')::uuid` 用；`None` → 空串）。
fn uuid_text(value: Option<Uuid>) -> String {
    value.map(|id| id.to_string()).unwrap_or_default()
}

// ==================== 接口 ====================

/// 读整张现金流底稿（配置项 ∪ 已录入行 + 小计 + 勾稽结果）。
#[nik::permission(
    path = "/business/erp/report/cash-flow/worksheet",
    method = "GET",
    permission = "business:erp:cashflow-worksheet:list"
)]
#[nik::get("/business/erp/report/cash-flow/worksheet")]
pub async fn worksheet_get_api(Query(query): Query<WorksheetQuery>) -> ResResult<WorksheetRes> {
    let period = match query.period.as_deref() {
        Some(raw) => normalize_period(raw).map_err(bad_request)?,
        None => current_period(),
    };
    Ok(Res::ok(load_sheet(&period).await?))
}

/// 录入底稿（期初 / 明细行 / 期末，同一事务 upsert；返回保存后的整张底稿 + 勾稽结果）。
#[nik::permission(
    path = "/business/erp/report/cash-flow/worksheet",
    method = "PUT",
    permission = "business:erp:cashflow-worksheet:save"
)]
#[nik::put("/business/erp/report/cash-flow/worksheet")]
pub async fn worksheet_put_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<SaveWorksheetReq>,
) -> ResResult<WorksheetRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(save_sheet(&req, operator).await?))
}

/// 勾稽校验（不落库）：把草稿叠加到已存底稿上算「期初 + 流入 − 流出 = 期末」。
///
/// 权限码用 `:list`（读码）而不是单独的 `:check`：本接口一行都不写库（只读
/// `biz_cashflow_worksheet` 与项目配置，返回校验结果），能看底稿的人当然能校验底稿；
/// 单独开码只会逼管理员为「点一下校验」再授一次。这一条已在越权闸
/// `system::authz_tests::o13_every_write_route_requires_permission` 的
/// `QUERY_BY_POST_EXACT` 里**逐条登记**（不落库的依据 + 证明用例
/// `draft_check_writes_nothing` 都写在那里）；授权理由见
/// `V0415__cashflow_worksheet_perm.up.sql` 的「为什么校验接口复用读码」。
#[nik::permission(
    path = "/business/erp/report/cash-flow/worksheet/check",
    method = "POST",
    permission = "business:erp:cashflow-worksheet:list"
)]
#[nik::post("/business/erp/report/cash-flow/worksheet/check")]
pub async fn worksheet_check_api(Json(req): Json<CheckWorksheetReq>) -> ResResult<CheckRes> {
    Ok(Res::ok(check_sheet(&req).await?))
}

/// 当前自然月 `YYYY-MM`（GET 不传 period 时的缺省：底稿页面打开即本期）。
pub fn current_period() -> String {
    let now = Local::now();
    format!("{:04}-{:02}", now.year(), now.month())
}

// ==================== 单测（纯逻辑 + 真库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::NaiveDate;

    /// 真库用例的期间（`1913-xx` 是刻意选的死年份：模板库里没有，也不会与业务冲突）。
    const OPEN_PERIOD: &str = "1913-01";
    /// 已结账期间（写入必须被 66085 拒）。
    const CLOSED_PERIOD: &str = "1913-02";
    /// 本卡自建的临时分类配置（停用场景用，收尾删除）。
    const SCRATCH_ITEM: &str = "M114_SCRATCH_ITEM";

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点数解析")
    }

    fn flow(code: &str, direction: &str, amount: &str) -> FlowLine {
        FlowLine {
            item_code: code.to_string(),
            item_name: format!("{code} 名称"),
            activity: "OPERATING".to_string(),
            direction: direction.to_string(),
            amount: dec(amount),
        }
    }

    // ---------- 纯逻辑 ----------

    /// ① 期间：`YYYY-MM` 形状 + 月份 1–12，其余一律拒（写错格式的行永远聚合不进任何一期）。
    #[test]
    fn period_shape_is_strict() {
        assert_eq!(normalize_period(" 2026-11 ").expect("合法期间"), "2026-11");
        for bad in ["2026-1", "2026-13", "2026-00", "26-11", "2026/11", "", "2026-11-01"] {
            assert!(normalize_period(bad).is_err(), "「{bad}」必须被拒");
        }
    }

    /// ② 金额：4 位小数上限（多出的会被 PG 静默四舍五入造成尾差）+ 明细行不许为负 + 位宽上限。
    #[test]
    fn amount_guards_are_load_bearing() {
        assert!(check_amount(dec("0"), "amount", false).is_ok());
        assert!(check_amount(dec("1200.1234"), "amount", false).is_ok());
        // 5 位小数：会造「底稿合计 ≠ 凭证合计」的尾差，必须拒
        let error = check_amount(dec("1200.12345"), "amount", false).expect_err("5 位小数必须拒");
        assert!(error.contains("4 位小数"), "{error}");
        // 明细行为负：方向已由方向列表达
        assert!(check_amount(dec("-1"), "amount", false).is_err());
        // 余额行允许负数（透支 / 受限资金）
        assert!(check_amount(dec("-1"), "amount", true).is_ok());
        // NUMERIC(18,4) 的整数部分只有 14 位
        assert!(check_amount(dec("100000000000000"), "amount", true).is_err());
    }

    /// ③ 勾稽闭合：期初 500 + 流入 1200 − 流出 700 = 期末 1000（差 0 才算闭合）。
    #[test]
    fn reconcile_closes_exactly() {
        let flows = [flow("A", DIRECTION_INFLOW, "1200"), flow("B", DIRECTION_OUTFLOW, "700")];
        let check = reconcile(Some(dec("500")), &flows, Some(dec("1000")));
        assert!(check.balanced, "{}", check.message);
        assert_eq!(check.inflow, dec("1200"));
        assert_eq!(check.outflow, dec("700"));
        assert_eq!(check.expected_closing, Some(dec("1000")));
        assert_eq!(check.diff, Some(Decimal::ZERO));
        assert!(check.problems.is_empty());
        assert!(check.message.contains("勾稽闭合"), "{}", check.message);
    }

    /// ④ 差 0.0001 就是不闭合（**绝不四舍五入抹平**），差值精确给出。
    #[test]
    fn reconcile_is_exact_to_four_decimals() {
        let flows = [flow("A", DIRECTION_INFLOW, "1200.0001")];
        let check = reconcile(Some(dec("500")), &flows, Some(dec("1700")));
        assert!(!check.balanced, "差 0.0001 也必须报不平");
        assert_eq!(check.diff, Some(dec("0.0001")));
        assert_eq!(check.problems.len(), 1);
        assert_eq!(check.problems[0].code, PROBLEM_IMBALANCED);
        assert!(
            check.problems[0].message.contains("0.0001"),
            "{}",
            check.problems[0].message
        );
    }

    /// ⑤ 缺期初 / 缺期末 / 空底稿逐条点名（缺行不当 0：0 与「忘了录」对报表是反的）。
    #[test]
    fn reconcile_names_missing_parts() {
        let flows = [flow("A", DIRECTION_INFLOW, "10")];
        let check = reconcile(None, &flows, Some(dec("10")));
        assert!(!check.balanced);
        assert_eq!(check.expected_closing, None, "缺期初 ⇒ 算不出应有期末");
        assert_eq!(check.diff, None);
        assert!(check.problems.iter().any(|p| p.code == PROBLEM_OPENING_MISSING));

        let check = reconcile(Some(dec("10")), &flows, None);
        assert!(!check.balanced);
        assert!(check.problems.iter().any(|p| p.code == PROBLEM_CLOSING_MISSING));

        let check = reconcile(None, &[], None);
        assert!(!check.balanced);
        assert_eq!(check.problems.len(), 3, "期初缺 + 期末缺 + 空底稿三条都要点名");
        assert!(check.problems.iter().any(|p| p.code == PROBLEM_EMPTY));
    }

    /// ⑥ 权限码：与 V0415 的 `sys_menu.auth_code` 逐字一致，且只有两个。
    #[test]
    fn permissions_are_well_formed() {
        assert_eq!(PERM_WORKSHEET_LIST, "business:erp:cashflow-worksheet:list");
        assert_eq!(PERM_WORKSHEET_SAVE, "business:erp:cashflow-worksheet:save");
        assert_eq!(PERMISSIONS, [PERM_WORKSHEET_LIST, PERM_WORKSHEET_SAVE]);
    }

    /// ⑦ 三块快照由服务端从配置取（客户端只给编码 + 金额），且四类坏输入各自被拒。
    #[test]
    fn flow_writes_take_snapshots_from_config() {
        let config = vec![
            ConfigItem {
                item_code: "A".to_string(),
                item_name: "销售商品、提供劳务收到的现金".to_string(),
                activity: "OPERATING".to_string(),
                direction: DIRECTION_INFLOW.to_string(),
                sort_order: 101,
                is_active: 1,
            },
            ConfigItem {
                item_code: "B".to_string(),
                item_name: "已停用项".to_string(),
                activity: "OPERATING".to_string(),
                direction: DIRECTION_OUTFLOW.to_string(),
                sort_order: 201,
                is_active: 0,
            },
        ];
        let writes = prepare_flow_writes(
            &[WorksheetItemIn {
                item_code: " A ".to_string(),
                amount: dec("1200"),
                remark: Some("  注释  ".to_string()),
            }],
            &config,
        )
        .expect("合法行");
        assert_eq!(writes[0].item_name, "销售商品、提供劳务收到的现金");
        assert_eq!(writes[0].direction, DIRECTION_INFLOW);
        assert_eq!(writes[0].sort_order, 101);
        assert_eq!(writes[0].remark.as_deref(), Some("注释"));

        let unknown = prepare_flow_writes(
            &[WorksheetItemIn {
                item_code: "NOPE".to_string(),
                amount: dec("1"),
                remark: None,
            }],
            &config,
        )
        .expect_err("不存在的项目必须拒");
        assert_eq!(unknown.code, error_code::ERP_CASHFLOW_ITEM_INVALID);

        let disabled = prepare_flow_writes(
            &[WorksheetItemIn {
                item_code: "B".to_string(),
                amount: dec("1"),
                remark: None,
            }],
            &config,
        )
        .expect_err("停用项目必须拒");
        assert_eq!(disabled.code, error_code::ERP_CASHFLOW_ITEM_INVALID);

        let duplicated = prepare_flow_writes(
            &[
                WorksheetItemIn {
                    item_code: "A".to_string(),
                    amount: dec("1"),
                    remark: None,
                },
                WorksheetItemIn {
                    item_code: "A".to_string(),
                    amount: dec("2"),
                    remark: None,
                },
            ],
            &config,
        )
        .expect_err("同批重复项目必须拒");
        assert_eq!(duplicated.code, error_code::PARAM_INVALID);

        let negative = prepare_flow_writes(
            &[WorksheetItemIn {
                item_code: "A".to_string(),
                amount: dec("-1"),
                remark: None,
            }],
            &config,
        )
        .expect_err("明细行负数必须拒");
        assert_eq!(negative.code, error_code::PARAM_INVALID);
    }

    /// ⑧ 合并规则：已录行取快照；未录的配置项也要出；配置里已没有的已录行照样出（`item_active=false`）。
    #[test]
    fn compose_sheet_merges_three_sources() {
        let config = vec![
            ConfigItem {
                item_code: "A".to_string(),
                item_name: "配置里的新名字".to_string(),
                activity: "OPERATING".to_string(),
                direction: DIRECTION_INFLOW.to_string(),
                sort_order: 101,
                is_active: 1,
            },
            ConfigItem {
                item_code: "B".to_string(),
                item_name: "未录入项".to_string(),
                activity: "OPERATING".to_string(),
                direction: DIRECTION_OUTFLOW.to_string(),
                sort_order: 201,
                is_active: 1,
            },
        ];
        let stored = vec![
            StoredRow {
                line_type: LINE_FLOW.to_string(),
                item_code: Some("A".to_string()),
                item_name: "编制当时的名字".to_string(),
                activity: Some("OPERATING".to_string()),
                direction: Some(DIRECTION_INFLOW.to_string()),
                amount: dec("1200"),
                sort_order: 101,
                remark: None,
            },
            StoredRow {
                line_type: LINE_FLOW.to_string(),
                item_code: Some("GONE".to_string()),
                item_name: "配置已删除的项目".to_string(),
                activity: Some("INVESTING".to_string()),
                direction: Some(DIRECTION_OUTFLOW.to_string()),
                amount: dec("50"),
                sort_order: 351,
                remark: None,
            },
            StoredRow {
                line_type: LINE_OPENING.to_string(),
                item_code: None,
                item_name: OPENING_NAME.to_string(),
                activity: None,
                direction: None,
                amount: dec("500"),
                sort_order: OPENING_SORT,
                remark: None,
            },
        ];
        let sheet = compose_sheet(OPEN_PERIOD, PERIOD_OPEN, &config, &stored);
        assert_eq!(sheet.rows.len(), 3, "A（已录）+ B（未录）+ GONE（配置已删）");
        let row_a = sheet.rows.iter().find(|row| row.item_code == "A").expect("A 行");
        assert_eq!(row_a.item_name, "编制当时的名字", "已录行取快照，不取配置新名");
        assert!(row_a.entered);
        assert!(row_a.item_active);
        let row_b = sheet.rows.iter().find(|row| row.item_code == "B").expect("B 行");
        assert!(!row_b.entered);
        assert_eq!(row_b.amount, Decimal::ZERO);
        let row_gone = sheet.rows.iter().find(|row| row.item_code == "GONE").expect("GONE 行");
        assert!(!row_gone.item_active, "配置里已没有 ⇒ 标记停用但仍显示");
        assert_eq!(sheet.opening.as_ref().map(|row| row.amount), Some(dec("500")));
        assert_eq!(sheet.totals.inflow, dec("1200"));
        assert_eq!(sheet.totals.outflow, dec("50"));
        assert!(sheet.editable);
        assert!(!sheet.check.balanced, "缺期末余额 ⇒ 不闭合");
        // 经营 / 投资 / 筹资三块固定顺序，GONE 落在投资块
        assert_eq!(sheet.activities.len(), 3);
        assert_eq!(sheet.activities[0].inflow, dec("1200"));
        assert_eq!(sheet.activities[1].outflow, dec("50"));
    }

    // ---------- 真库 ----------

    /// 真库用例串行锁（与凭证 / 汇率用例共用同一把：都会写共享底稿与期间表）。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = crate::business::erp::voucher::voucher_query::tests::DB_LOCK
            .lock()
            .await;
        if !ensure_db().await {
            return None;
        }
        reset().await;
        Some(guard)
    }

    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 清场 + 铺夹具：两个期间（打开 / 已结账）+ 一条本卡自建分类配置（启用）。
    async fn reset() {
        for sql in [
            "DELETE FROM biz_cashflow_worksheet WHERE period_code IN ('1913-01', '1913-02')",
            "DELETE FROM biz_accounting_period WHERE period_code IN ('1913-01', '1913-02')",
        ] {
            db().execute_with_params(sql, Vec::new())
                .await
                .unwrap_or_else(|error| panic!("清场失败（{sql}）：{error}"));
        }
        db().execute_with_params(
            "DELETE FROM biz_cashflow_item WHERE item_code = ?",
            vec![Value::Text(SCRATCH_ITEM.to_string())],
        )
        .await
        .expect("清临时分类配置");

        for (period, status) in [(OPEN_PERIOD, "OPEN"), (CLOSED_PERIOD, "CLOSED")] {
            let year: i32 = period[..4].parse().expect("年");
            let month: u32 = period[5..].parse().expect("月");
            let start = NaiveDate::from_ymd_opt(year, month, 1).expect("期间起始日");
            let next_first = if month == 12 {
                NaiveDate::from_ymd_opt(year + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(year, month + 1, 1)
            }
            .expect("下月一日");
            let end = next_first.pred_opt().expect("期间结束日");
            db().execute_with_params(
                "INSERT INTO biz_accounting_period \
                     (id, period_year, period_month, period_code, start_date, end_date, status) \
                 VALUES (gen_random_uuid(), ?, ?, ?, ?::text::date, ?::text::date, ?)",
                vec![
                    Value::I16(year as i16),
                    Value::I16(month as i16),
                    Value::Text(period.to_string()),
                    Value::Text(start.to_string()),
                    Value::Text(end.to_string()),
                    Value::Text(status.to_string()),
                ],
            )
            .await
            .unwrap_or_else(|error| panic!("铺期间 {period} 失败：{error}"));
        }

        db().execute_with_params(
            "INSERT INTO biz_cashflow_item \
                 (id, item_code, item_name, activity, direction, account_pattern, sort_order, is_active) \
             VALUES (gen_random_uuid(), ?, '本卡临时项目', 'OPERATING', 'INFLOW', 'T_114', 901, 1)",
            vec![Value::Text(SCRATCH_ITEM.to_string())],
        )
        .await
        .expect("铺临时分类配置");
    }

    fn item(code: &str, amount: &str) -> WorksheetItemIn {
        WorksheetItemIn {
            item_code: code.to_string(),
            amount: dec(amount),
            remark: None,
        }
    }

    fn save_req(items: Vec<WorksheetItemIn>, opening: Option<&str>, closing: Option<&str>) -> SaveWorksheetReq {
        SaveWorksheetReq {
            period: OPEN_PERIOD.to_string(),
            opening: opening.map(|amount| BalanceIn {
                amount: dec(amount),
                remark: None,
            }),
            closing: closing.map(|amount| BalanceIn {
                amount: dec(amount),
                remark: None,
            }),
            items,
        }
    }

    /// ⑨ 真库往返：保存 → 回读闭合 → 再存一次仍是一行（upsert 幂等，不会攒两行同项目金额）。
    #[tokio::test]
    async fn save_roundtrip_is_closed_and_idempotent() {
        let Some(_guard) = setup().await else {
            return;
        };
        let req = save_req(
            vec![
                item("OPER_SALES_RECEIPTS", "1200.0000"),
                item("OPER_PURCHASE_PAYMENTS", "700.0000"),
            ],
            Some("500.0000"),
            Some("1000.0000"),
        );
        let sheet = save_sheet(&req, None).await.expect("保存底稿");
        assert!(sheet.check.balanced, "{}", sheet.check.message);
        assert_eq!(sheet.check.diff, Some(Decimal::ZERO));
        assert_eq!(sheet.totals.inflow, dec("1200"));
        assert_eq!(sheet.totals.outflow, dec("700"));
        let sales = sheet
            .rows
            .iter()
            .find(|row| row.item_code == "OPER_SALES_RECEIPTS")
            .expect("销售收款行");
        assert_eq!(sales.item_name, "销售商品、提供劳务收到的现金", "快照取自配置");
        assert_eq!(sales.direction, DIRECTION_INFLOW);
        assert!(sales.entered);

        // 再存一次（同一批）：行数不涨，金额不变
        let again = save_sheet(&req, None).await.expect("重复保存");
        assert!(again.check.balanced);
        let stored = load_rows(db(), OPEN_PERIOD).await.expect("回读底稿行");
        let flow_rows: Vec<&StoredRow> = stored.iter().filter(|row| row.line_type == LINE_FLOW).collect();
        assert_eq!(
            flow_rows.len(),
            2,
            "同期间同项目只一行（upsert 键 period_code+item_code）"
        );
        let balance_rows: Vec<&StoredRow> = stored.iter().filter(|row| row.line_type != LINE_FLOW).collect();
        assert_eq!(balance_rows.len(), 2, "期初 / 期末各一行");

        // 改期末（不平）→ 保存成功但 check 明确报差
        let unbalanced = save_req(vec![], None, Some("1100.0000"));
        let sheet = save_sheet(&unbalanced, None).await.expect("允许保存不平的草稿");
        assert!(!sheet.check.balanced, "差 100 必须报不平");
        assert_eq!(sheet.check.diff, Some(dec("-100")));
        assert!(!sheet.check.problems.is_empty());
    }

    /// ⑩ 校验接口不写库：草稿叠加算出闭合，但一行都没落库。
    #[tokio::test]
    async fn draft_check_writes_nothing() {
        let Some(_guard) = setup().await else {
            return;
        };
        let req = CheckWorksheetReq {
            period: OPEN_PERIOD.to_string(),
            opening: Some(BalanceIn {
                amount: dec("500"),
                remark: None,
            }),
            closing: Some(BalanceIn {
                amount: dec("1000"),
                remark: None,
            }),
            items: vec![
                item("OPER_SALES_RECEIPTS", "1200"),
                item("OPER_PURCHASE_PAYMENTS", "700"),
            ],
            require_balanced: true,
        };
        let check = check_sheet(&req).await.expect("草稿闭合");
        assert!(check.balanced, "{}", check.message);
        let stored = load_rows(db(), OPEN_PERIOD).await.expect("回读底稿行");
        assert!(
            stored.is_empty(),
            "校验接口一个字节都不许写库，实际落了 {} 行",
            stored.len()
        );

        // require_balanced + 不平 ⇒ 400 + 66087
        let mut bad = req;
        bad.closing = Some(BalanceIn {
            amount: dec("999"),
            remark: None,
        });
        let error = check_sheet(&bad).await.expect_err("不平必须拒");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_CASHFLOW_WORKSHEET_UNBALANCED);
        assert!(error.msg.contains("期初 + 流入 − 流出 = 期末"), "{}", error.msg);
    }

    /// ⑪ 期间闸：已结账 / 未开账拒绝写入（66085），期间不存在拒绝（66010）。
    #[tokio::test]
    async fn period_guards_block_writes() {
        let Some(_guard) = setup().await else {
            return;
        };
        let mut req = save_req(vec![item("OPER_SALES_RECEIPTS", "1")], None, None);
        req.period = CLOSED_PERIOD.to_string();
        let error = save_sheet(&req, None).await.expect_err("已结账期间必须拒");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_CASHFLOW_WORKSHEET_PERIOD_LOCKED);
        assert!(error.msg.contains(CLOSED_PERIOD), "报文要点名期间：{}", error.msg);

        req.period = "1913-09".to_string();
        let error = save_sheet(&req, None).await.expect_err("不存在的期间必须拒");
        assert_eq!(error.code, error_code::ERP_PERIOD_NOT_FOUND);

        let error = load_sheet("1913-09").await.expect_err("不存在的期间读也拒");
        assert_eq!(error.code, error_code::ERP_PERIOD_NOT_FOUND);

        // 已结账期间可以读（历史底稿要能看），只是 editable = false
        let sheet = load_sheet(CLOSED_PERIOD).await.expect("已结账期间可读");
        assert!(!sheet.editable);
        assert_eq!(sheet.period_status, "CLOSED");
    }

    /// ⑫ 停用后历史底稿仍可读（`item_active = false`），但不能再往里写（66086）。
    #[tokio::test]
    async fn disabled_item_history_stays_readable_but_not_writable() {
        let Some(_guard) = setup().await else {
            return;
        };
        save_sheet(&save_req(vec![item(SCRATCH_ITEM, "88.0000")], None, None), None)
            .await
            .expect("先录一行临时项目的底稿");
        db().execute_with_params(
            "UPDATE biz_cashflow_item SET is_active = 0 WHERE item_code = ?",
            vec![Value::Text(SCRATCH_ITEM.to_string())],
        )
        .await
        .expect("停用临时项目");

        let sheet = load_sheet(OPEN_PERIOD).await.expect("停用后仍可读");
        let row = sheet
            .rows
            .iter()
            .find(|row| row.item_code == SCRATCH_ITEM)
            .expect("历史行不能因为配置停用而消失");
        assert!(row.entered);
        assert!(!row.item_active, "配置已停用 ⇒ item_active=false");
        assert_eq!(row.amount, dec("88"));

        let error = save_sheet(&save_req(vec![item(SCRATCH_ITEM, "1.0000")], None, None), None)
            .await
            .expect_err("停用项目不能再写");
        assert_eq!(error.code, error_code::ERP_CASHFLOW_ITEM_INVALID);
    }
}
