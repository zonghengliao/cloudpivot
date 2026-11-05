//! 年结（ERP-R4）：**损益结转 + 下年期初 + 反年结**。
//!
//! # 为什么需要这张卡
//!
//! ERP-P1 能逐月结账、ERP-R5 能录期初，但**跨年**没有出口：2026 年的 5xxx / 6xxx
//! 余额会一直挂在账上，2027 年的利润表「本年累计」会把 2026 年的收入再算一遍，
//! 资产负债表的「未分配利润」永远不和利润表勾稽。年结就是这道年与年之间的门。
//!
//! # 年结做的四件事（一个事务，卡面 L11）
//!
//! ```text
//! ① 损益结转 → 一张 voucher_type='YEAR_END' 的凭证（period = YYYY-12，日期 12-31）
//!      借 5xxx（收入）/ 贷 6xxx（费用）→ 先结平各损益类科目，差额进 3103 本年利润
//!      再 借 3103 / 贷 3104 利润分配（把本年利润搬进未分配利润）
//! ② 下年期初 → biz_opening_balance(YYYY+1-01, source='YEAR_END')
//!      资产 / 负债 / 权益类：期末余额 = 下年期初（按方向单边落一行）
//!      损益类：归零（不写行）
//! ③ 12 月期间 → 保持 CLOSED（卡面 §一 ④；前置条件本来就是「12 个月全结账」）
//! ④ 审计留痕 → biz_status_log（entity_type='ACCOUNTING_YEAR'，entity_no='2026'）
//! ```
//!
//! # 三处设计决议（与卡面字面不同，理由都在这里）
//!
//! ## ① 结转凭证允许写进已结账的 12 月（卡面 §四 ⚠️ 的第二个选项）
//!
//! 年结的前置条件就是「本年 12 个月全部结账」，所以结转凭证要落的 12 月必然 CLOSED。
//! 闸口在 [`voucher::post_voucher`]（所有凭证的唯一写入口），本卡在那里加**唯一例外**：
//! `voucher_type='YEAR_END'` 且 `source_type='YEAR_END'` 时放行已结账期间。
//! 类型白名单在库层（`ck_biz_voucher_type`，V0190 扩成 `AUTO|MANUAL|OPENING|YEAR_END`），
//! 一个年度一张（部分唯一索引 `uk_biz_voucher_year_end_period`）。
//!
//! 为什么不用「临时解封 12 月再重新结账」（卡面的另一个选项）：那是三个额外状态变更
//! （`CLOSED→OPEN`、生成凭证、`OPEN→CLOSED`），每一步都要写状态日志，反年结时还要再反一遍；
//! 而「类型白名单 + 单用途常量」把风险收在一个人为指定的凭证类型上，账务效果相同。
//!
//! ## ② 下年期初**只写台账、不生成期初凭证**（与 R5 的 OPENING 凭证不同）
//!
//! 报表的「期初」列（`report::load_accounts` 的 `net_before`）= 期间开始日之前的**历史累计**
//! 加上本期间自己的 OPENING 凭证。2027 年的历史累计里已经有 2026 年的结转凭证，
//! 它本身就等于 2026 年的期末余额 —— 再生成一张 2027-01 的 OPENING 凭证就是**双计**
//! （银行存款、存货全部翻倍）。
//!
//! 所以年结把「下年期初」写成 `biz_opening_balance(2027-01, source='YEAR_END')`：
//! 它是「新的一年从多少开始」的**据**（页面展示、审计对账都用它），而报表的期初
//! 仍由历史累计给出，两者必须相等（单测 ⑨ 用同一个 SQL 两边各算一次钉住）。
//! R5 的 OPENING 凭证解决的是「新系统上线、库里没有历史」那种场景，两者不冲突。
//!
//! 反向保护：`source='YEAR_END'` 的期初行**不许手工覆盖**（[`opening::save_opening`]
//! 会 409），否则手工再写一遍又是一次双计。
//!
//! ## ③ 年结状态记在 `biz_status_log`，不靠「有没有结转凭证」推断
//!
//! 空年度（整年没有损益发生、也没有本年利润）不需要结转凭证（卡面 §七 ⑭：
//! 「凭证只有 0 金额或跳过」），但它**已经年结过了** —— 状态必须查得到。
//! 所以 `entity_no = '2026'`、`to_status = 'CLOSED'` 是唯一判据，
//! `entity_id` 存结转凭证 id（空年度存一次性 id）。
//!
//! # 五项前置校验（卡面 §二，任一不过就 409 并**逐条点名**）
//!
//! | # | 校验 | 阻塞项 `type` |
//! |---|------|----------------|
//! | ① | 本年 12 个月必须全部结账 | `PERIOD_NOT_CLOSED`（带 `periods` 清单） |
//! | ② | 本年没有未完成单据（采购 / 收货 / 调拨） | `DOC_UNFINISHED` |
//! | ③ | 全年度试算平衡（分录 Σ借 = Σ贷） | `UNBALANCED`（带 `diff`） |
//! | ④ | 全年度不变量（A1 / A2） | `INVARIANT` |
//! | ⑤ | 下一年度 12 个期间全部已开账 | `NEXT_YEAR_NOT_OPENED` |
//!
//! 另有两项**附加闸**（卡面没写，但不加会出坏账）：
//!
//! * `NEXT_YEAR_OPENING_EXISTS`：下一年度第一个期间的期初**已经被手工录过** ——
//!   年结会用算出来的数覆盖它，必须让人先自己决定（清空后再结，或放弃年结）；
//! * `ALREADY_CLOSED`：本年度已经结转过了（重复年结不是幂等操作）。
//!
//! # 只报汇总不够：每条阻塞项都要带「单据明细 + 处理人」（YEAR-END 卡）
//!
//! 老板对结账提的三句话（列单据 / 写清谁处理 / 能发消息催）在年结页原来一句都没落地 ——
//! `YearEndBlocker` 只有一句 `message` 和最多 3 个 `samples`。现在：
//!
//! ```text
//! ② DOC_UNFINISHED → period_items::unfinished_doc_items_between(全年 1/1~12/31)
//! ④ INVARIANT      → period_items::invariant_details_between(全年凭证)
//!                     ↓ 两者都带 BlockingItem（单号 / 类型 / 金额 / 制单人 / 状态 /
//!                       owner_role + owner_name（权限码反查）/ 已等 / 去处理路由）
//!                     ↓ 明细合并后统一截断到 50 条（不是 12 个月 × 50）
//! 催办 → POST /business/erp/year-end/remind（见 year_end_remind）
//! ```
//!
//! **取数一行 SQL 都不复制**：两个 `period_items` 的「日期区间」入口是期间结账那年
//! （`t_1d3a6fce`）就写好的同一份实现，年结只是把区间换成「一整年」。
//! `can_close()` 仍然只看 `blockers` 是否为空 —— 明细是「给人看的」，不参与判定。
//!
//! # 反年结（卡面 §五，必填原因 ≥ 10 字）
//!
//! ```text
//! 前置：本年度已结转 + 下一年度除 OPENING 外没有任何凭证（否则倒序删除就是数据错乱）
//! 动作：① 删结转凭证（含它的状态日志）② 删下年期初台账 ③ 12 月解封 ④ 留痕
//! ```
//!
//! 下一年度的凭证判据里**排除 OPENING**：那是 R5 期初功能录的，与本年结转无关；
//! 但 `AUTO` / `MANUAL` / `YEAR_END` 都算「新年已经用起来了」，反年结必须拦住。
//!
//! # 接口（前缀 `/api` 由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 权限码 |
//! |------|------|--------|
//! | GET  | `/business/erp/year-end/check?year=2026` | `business:erp:yearend:read` |
//! | POST | `/business/erp/year-end/close` | `business:erp:yearend:close` |
//! | POST | `/business/erp/year-end/reopen` | `business:erp:yearend:reopen` |
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_voucher / biz_voucher_entry / biz_accounting_period /
//! biz_opening_balance / biz_status_log 是全局会计台账与字典（V0137 / V0149 / V0159 / V0189），
//! 无 dept_id 与归属人列，数据范围表达不出来；可见性与操作性由
//! business:erp:yearend:{read,close,reopen} 三个独立权限码控制。
//! ```

use axum::{Extension, Json};
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};
use std::result::Result;
use std::str::FromStr;

use crate::business::erp::opening;
use crate::business::erp::period;
use crate::business::erp::period_items::{self, BlockingItem, OwnerResolver};
use crate::business::erp::voucher::{self, AccountRef, EntryLine, PostVoucher};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 口径常量 ====================

/// 查看年份结转状态 / 前置检查的权限码。
pub const PERM_READ: &str = "business:erp:yearend:read";
/// 年结权限码。
pub const PERM_CLOSE: &str = "business:erp:yearend:close";
/// 反年结权限码（高危：撤销整个年度的结转）。
pub const PERM_REOPEN: &str = "business:erp:yearend:reopen";

/// 三个权限码（与 `V0190__erp_year_end.up.sql` 的 `sys_menu` 节点逐字对应）。
///
/// 与 ERP-P1 的期间三码同构：能看状态 ≠ 能年结 ≠ 能拆年结。
pub const YEAR_END_PERMISSIONS: [(&str, &str); 3] = [
    (PERM_READ, "查看年度结转状态 / 前置检查"),
    (PERM_CLOSE, "年结（损益结转 + 生成下年期初）"),
    (PERM_REOPEN, "反年结（撤销整年的结转，必填原因）"),
];

/// 本年利润科目（小企业会计制度 `3103`，V0183 种）。
pub const ACCOUNT_CURRENT_PROFIT: &str = "3103";
/// 利润分配科目（`3104`，V0183 种）：年结把本年利润搬到这里。
pub const ACCOUNT_PROFIT_DISTRIBUTION: &str = "3104";

/// 年度状态：未结转。
pub const STATUS_NOT_CLOSED: &str = "NOT_CLOSED";
/// 年度状态：已结转。
pub const STATUS_CLOSED: &str = "CLOSED";

/// 反年结原因的最小字数（与期间反结账同口径）。
pub const MIN_REOPEN_REASON_CHARS: usize = 10;
/// 年结备注的最大字数。
pub const MAX_CLOSE_REASON_CHARS: usize = 200;

/// 阻塞项：本年有月份没结账。
pub const BLOCKER_PERIOD_NOT_CLOSED: &str = "PERIOD_NOT_CLOSED";
/// 阻塞项：本年有未完成单据。
pub const BLOCKER_DOC_UNFINISHED: &str = "DOC_UNFINISHED";
/// 阻塞项：全年度试算不平。
pub const BLOCKER_UNBALANCED: &str = "UNBALANCED";
/// 阻塞项：全年度不变量未通过。
pub const BLOCKER_INVARIANT: &str = "INVARIANT";
/// 阻塞项：下一年度期间没开账。
pub const BLOCKER_NEXT_YEAR_NOT_OPENED: &str = "NEXT_YEAR_NOT_OPENED";
/// 阻塞项：下一年度期初已经被手工录过（年结会覆盖它）。
pub const BLOCKER_NEXT_YEAR_OPENING_EXISTS: &str = "NEXT_YEAR_OPENING_EXISTS";
/// 阻塞项：本年度已经结转过了。
pub const BLOCKER_ALREADY_CLOSED: &str = "ALREADY_CLOSED";
/// 阻塞项：本年度还没结转（反年结时）。
pub const BLOCKER_NOT_CLOSED: &str = "NOT_CLOSED";

/// 试算平衡容差（与 `scripts/verify/erp_invariants.py` 的 `TOL` 一致）。
fn balance_tolerance() -> Decimal {
    Decimal::new(1, 4)
}

// ==================== 入参 / 出参 ====================

/// 年结入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct YearEndCloseReq {
    /// 会计年度（如 2026）。
    pub year: i16,
    /// 备注（可选；落状态日志）。
    #[serde(default)]
    pub reason: Option<String>,
}

/// 反年结入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct YearEndReopenReq {
    /// 会计年度（如 2026）。
    pub year: i16,
    /// 反年结原因（**必填，≥ 10 字**）。
    #[serde(default)]
    pub reason: Option<String>,
}

/// 查询入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct YearEndQuery {
    /// 会计年度（缺省 = 1 月所在年度……不猜，必填）。
    pub year: Option<i16>,
}

/// 一个阻塞项（前端按 `type` 决定图标 / 跳转，按 `message` 直接展示）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct YearEndBlocker {
    /// 类型（见 [`BLOCKER_PERIOD_NOT_CLOSED`] 等常量）。
    #[serde(rename = "type")]
    pub kind: String,
    /// 涉及的期间编码（仅 `PERIOD_NOT_CLOSED` / `NEXT_YEAR_NOT_OPENED`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub periods: Vec<String>,
    /// 条数（未完成单据 / 不变量项数）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    /// 差额（仅 `UNBALANCED`，定点字符串）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// 给人看的一句话（**点名数量 / 期间 / 差额**，卡面 O5）。
    pub message: String,
    /// 去处理的页面。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// 定位线索（单号 / 凭证号，最多 3 个）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<String>,
    /// ★ 挡路单据明细（YEAR-END，结构**逐字照搬** `period_items::BlockingItem`）。
    ///
    /// 前端因此能直接复用结账弹窗那张表（单号 / 类型 / 金额 / 制单人 / 该谁处理 /
    /// 已等 / 去处理 / 催办），不必再写一套。
    ///
    /// * 空数组 = 这一类没有「单据级」明细（期间未结 / 借贷不平 / 下年未开账），
    ///   前端此时只显示 [`Self::message`]；
    /// * 最多 [`period_items::ITEM_LIMIT`] 条：超限时 `message` 里写明「共 N 张，仅显示前 50 张」。
    #[serde(default)]
    pub items: Vec<BlockingItem>,
}

impl YearEndBlocker {
    /// 造一个纯文案阻塞项。
    fn new(kind: &str, message: impl Into<String>, link: Option<&str>) -> Self {
        Self {
            kind: kind.to_string(),
            periods: Vec::new(),
            count: None,
            diff: None,
            message: message.into(),
            link: link.map(str::to_string),
            samples: Vec::new(),
            items: Vec::new(),
        }
    }

    /// 补期间清单。
    fn with_periods(mut self, periods: Vec<String>) -> Self {
        self.periods = periods;
        self
    }

    /// 补条数。
    fn with_count(mut self, count: i64) -> Self {
        self.count = Some(count);
        self
    }

    /// 补明细（YEAR-END）；同时把前 3 个单号写进老字段 `samples`
    /// （老前端 / 日志可能还在读它，保持不空）。
    fn with_items(mut self, items: Vec<BlockingItem>) -> Self {
        self.samples = items
            .iter()
            .take(3)
            .map(|item| item.doc_no.clone())
            .collect();
        self.items = items;
        self
    }
}

/// 一个年度的结转状态（页面顶部 + 已结转面板都用它）。
#[derive(Debug, Clone, Serialize)]
pub struct YearEndState {
    /// 年度。
    pub year: i16,
    /// 是否已结转（唯一判据 = 状态日志最新一行为 `CLOSED`）。
    pub closed: bool,
    /// `CLOSED` / `NOT_CLOSED`。
    pub status: String,
    /// 结转时间（状态日志的 `created_at`）。
    pub closed_at: Option<String>,
    /// 结转操作人（状态日志的 `operator_name`）。
    pub closed_by: Option<String>,
    /// 结转备注 / 反年结原因。
    pub reason: Option<String>,
    /// 结转凭证 id（空年度为 `None`）。
    pub carry_forward_voucher_id: Option<String>,
    /// 结转凭证号（`PZ-yyyyMM-000001`）。
    pub carry_forward_voucher_no: Option<String>,
    /// 结转凭证借方合计。
    pub carry_forward_total: Decimal,
    /// 结转凭证分录数。
    pub carry_forward_entries: i64,
    /// 本年净利润（结转进 `3104` 的金额；亏损为负）。
    pub net_profit: Decimal,
    /// 本年已结账的月份数（0–12）。
    pub periods_closed: i64,
    /// 下一年度第一个期间（`2027-01`）。
    pub opening_period: String,
    /// 下年期初是否已生成（`biz_opening_balance` 有行）。
    pub opening_balance_created: bool,
    /// 下年期初行数。
    pub opening_rows: i64,
    /// 下一年度 12 个期间是否都已开账。
    pub next_year_periods_ready: bool,
    /// 下一年度已开账的期间数（0–12）。
    pub next_year_open_count: i64,
}

/// 前置检查响应（`GET /business/erp/year-end/check`）。
#[derive(Debug, Clone, Serialize)]
pub struct YearEndCheckRes {
    /// 年度。
    pub year: i16,
    /// 能不能年结（= 没有阻塞项且本年未结转）。
    pub can_close: bool,
    /// 阻塞项（空 = 可以年结）。
    pub blockers: Vec<YearEndBlocker>,
    /// 结转状态。
    pub state: YearEndState,
}

/// 年结响应（`POST /business/erp/year-end/close`）。
#[derive(Debug, Clone, Serialize)]
pub struct YearEndCloseRes {
    /// 年度。
    pub year: i16,
    /// 结转凭证号（空年度没有损益可结 → `None`）。
    pub carry_forward_voucher_no: Option<String>,
    /// 下年期初是否已生成。
    pub opening_balance_created: bool,
    /// 下年期初行数。
    pub opening_rows: i64,
    /// 本年已结账的月份数（恒为 12，前置条件保证）。
    pub periods_closed: i64,
    /// 下一年度 12 个期间是否都已开账。
    pub next_year_periods_ready: bool,
    /// 本年净利润（结转进 `3104` 的金额）。
    pub net_profit: Decimal,
    /// 结转凭证分录数。
    pub entry_count: i64,
    /// 结转后的状态。
    pub state: YearEndState,
}

/// 反年结响应（`POST /business/erp/year-end/reopen`）。
#[derive(Debug, Clone, Serialize)]
pub struct YearEndReopenRes {
    /// 年度。
    pub year: i16,
    /// 是否已回滚（成功才返回）。
    pub reopened: bool,
    /// 反年结原因（原样回显，便于前端提示「已按原因回滚」）。
    pub reason: String,
    /// 被删掉的结转凭证号。
    pub deleted_voucher_no: Option<String>,
    /// 被删掉的分录数。
    pub deleted_entries: i64,
    /// 被删掉的下年期初行数。
    pub deleted_opening_rows: u64,
    /// 12 月是否已解封。
    pub december_reopened: bool,
    /// 回滚后的状态。
    pub state: YearEndState,
}

// ==================== 错误收口 ====================

/// 400：入参不合法。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409：状态冲突 / 前置校验未通过。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 数据库错误 → 500（带上下文）。
fn db_error(error: DbError, ctx: &str) -> ApiError {
    utils::db_error(error, ctx)
}

// ==================== 纯逻辑（不连库，可单测）====================

/// 年度 → 12 个期间编码（`2026-01` … `2026-12`）。
pub fn period_codes_of(year: i16) -> Vec<String> {
    (1..=12)
        .map(|month| period::period_code(year, month))
        .collect()
}

/// 年度入参校验（2000–2998：留给「下一年度」一整年）。
pub fn validate_year(raw: Option<i16>) -> Result<i16, ApiError> {
    let year = raw.ok_or_else(|| bad_request("year 必填（如 2026）：年结按会计年度做"))?;
    if !(2000..=2998).contains(&year) {
        return Err(bad_request(format!(
            "year 只能是 2000–2998（当前 {year}）：再早/再晚都是误填，年度结转没有落点"
        )));
    }
    Ok(year)
}

/// 年度 → `(1 月 1 日, 12 月 31 日)`。
pub fn year_range(year: i16) -> Result<(NaiveDate, NaiveDate), ApiError> {
    let start = NaiveDate::from_ymd_opt(i32::from(year), 1, 1)
        .ok_or_else(|| bad_request(format!("{year} 不是合法年度")))?;
    let end = NaiveDate::from_ymd_opt(i32::from(year), 12, 31)
        .ok_or_else(|| bad_request(format!("{year} 的 12 月 31 日不合法")))?;
    Ok((start, end))
}

/// 下一年度第一个期间的编码（`2027-01`）。
pub fn next_year_opening_period(year: i16) -> String {
    period::period_code(year + 1, 1)
}

/// 反年结原因校验：**必填且 ≥ 10 字**（卡面硬红线 5）。
pub fn require_reopen_reason(raw: Option<&str>) -> Result<String, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(bad_request(
            "反年结必须填写原因（reason）：拆掉整年的结转会让下一年度的账全部重来，\
             原因要能让审计看懂「为什么」",
        ));
    }
    let chars = text.chars().count();
    if chars < MIN_REOPEN_REASON_CHARS {
        return Err(bad_request(format!(
            "反年结原因至少 {MIN_REOPEN_REASON_CHARS} 个字（当前 {chars} 字）：「{text}」\
             这种写法审计时看不出动机"
        )));
    }
    Ok(text.to_string())
}

/// 年结备注校验（可选，超长 400）。
pub fn optional_close_reason(raw: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let chars = text.chars().count();
    if chars > MAX_CLOSE_REASON_CHARS {
        return Err(bad_request(format!(
            "年结备注最多 {MAX_CLOSE_REASON_CHARS} 字（当前 {chars} 字）"
        )));
    }
    Ok(Some(text.to_string()))
}

/// 造一条分录（`summary` 由调用方给，`line_no` 由 [`build_closing_lines`] 统一编号）。
fn line(
    account: &AccountRef,
    debit: Decimal,
    credit: Decimal,
    summary: impl Into<String>,
) -> EntryLine {
    EntryLine {
        line_no: 0,
        summary: summary.into(),
        account: account.clone(),
        debit,
        credit,
        aux_supplier_id: None,
        aux_material_id: None,
        aux_warehouse_id: None,
        aux_dept_id: None,
    }
}

/// 由损益类科目余额 + 本年利润余额拼出结转分录（**纯函数，可单测**）。
///
/// # 参数
///
/// * `pl_rows` —— `(损益类科目, 余额)`，余额口径是 `Σ借方 − Σ贷方`
///   （所以收入类为负、费用类为正，与 [`report::AccountBalance::net`] 同口径）；
/// * `current_profit_balance` —— `3103` 结转前余额（同口径：贷方余额为负）。
///
/// # 分录结构
///
/// ```text
/// ① 逐科目结平：借方余额（费用）→ 贷方一行；贷方余额（收入）→ 借方一行
/// ② 差额进 3103：借 3103 = Σ费用、贷 3103 = Σ收入
/// ③ 3103 → 3104：把 3103 结转后的**全部余额**搬进利润分配
///    （不只是本年数：R5 的库存期初也往 3103 记过贷方，留在这里会让「本年利润」年年不为零）
/// ```
///
/// 空年度（全部为 0 且 3103 无余额）→ 返回空列表，调用方据此跳过凭证。
pub fn build_closing_lines(
    pl_rows: &[(AccountRef, Decimal)],
    current_profit: &AccountRef,
    profit_distribution: &AccountRef,
    current_profit_balance: Decimal,
) -> Vec<EntryLine> {
    let mut lines: Vec<EntryLine> = Vec::new();
    let mut expense_total = Decimal::ZERO;
    let mut revenue_total = Decimal::ZERO;

    for (account, balance) in pl_rows {
        let amount = balance.round_dp(4);
        if amount.is_zero() {
            continue;
        }
        if amount > Decimal::ZERO {
            // 借方余额（成本 / 费用）：贷方结平
            expense_total += amount;
            lines.push(line(
                account,
                Decimal::ZERO,
                amount,
                format!("{} 年末结转（费用结平）", account.name),
            ));
        } else {
            // 贷方余额（收入）：借方结平
            let flat = -amount;
            revenue_total += flat;
            lines.push(line(
                account,
                flat,
                Decimal::ZERO,
                format!("{} 年末结转（收入结平）", account.name),
            ));
        }
    }

    if !expense_total.is_zero() {
        lines.push(line(
            current_profit,
            expense_total,
            Decimal::ZERO,
            "本年利润 年末结转（费用转入）",
        ));
    }
    if !revenue_total.is_zero() {
        lines.push(line(
            current_profit,
            Decimal::ZERO,
            revenue_total,
            "本年利润 年末结转（收入转入）",
        ));
    }

    // ③ 本年利润 → 利润分配（结转后的 3103 余额必须归零）
    let after = (current_profit_balance + expense_total - revenue_total).round_dp(4);
    if !after.is_zero() {
        if after < Decimal::ZERO {
            // 贷方余额 = 盈利：借 3103 / 贷 3104
            let amount = -after;
            lines.push(line(
                current_profit,
                amount,
                Decimal::ZERO,
                "本年利润转利润分配（盈利）",
            ));
            lines.push(line(
                profit_distribution,
                Decimal::ZERO,
                amount,
                "本年利润转利润分配（盈利）",
            ));
        } else {
            // 借方余额 = 亏损：贷 3103 / 借 3104
            lines.push(line(
                current_profit,
                Decimal::ZERO,
                after,
                "本年利润转利润分配（亏损）",
            ));
            lines.push(line(
                profit_distribution,
                after,
                Decimal::ZERO,
                "本年利润转利润分配（亏损）",
            ));
        }
    }

    for (index, item) in lines.iter_mut().enumerate() {
        item.line_no = i32::try_from(index + 1).unwrap_or(0);
    }
    lines
}

/// 由期末余额拼出「下年期初」台账行（**纯函数，可单测**）。
///
/// 规则（卡面 §一 ③ + R5 的库约束 `ck_biz_opening_single_side`）：
///
/// * 损益类（`PROFIT_LOSS`）→ **不写**（归零）；
/// * 余额 > 0（借方）→ `debit_opening = 余额`、`credit_opening = NULL/0`；
/// * 余额 < 0（贷方）→ `credit_opening = −余额`；
/// * 余额 = 0 → 不写（期初表里 0 行等于没录）。
pub fn opening_rows_of(rows: &[(AccountRef, Decimal, String)]) -> Vec<opening::YearEndOpeningRow> {
    let mut out: Vec<opening::YearEndOpeningRow> = Vec::new();
    for (account, balance, category) in rows {
        if category == "PROFIT_LOSS" {
            continue;
        }
        let amount = balance.round_dp(4);
        if amount.is_zero() {
            continue;
        }
        let (debit, credit) = if amount > Decimal::ZERO {
            (amount, Decimal::ZERO)
        } else {
            (Decimal::ZERO, -amount)
        };
        out.push((account.id, account.code.clone(), debit, credit));
    }
    out
}

// ==================== 读：期间 / 余额 / 状态 ====================

/// 本年 12 个期间里**没结账**的（含还没建行的期间）。
async fn months_not_closed(ex: &impl SqlExecutor, year: i16) -> Result<Vec<String>, ApiError> {
    let rows = ex
        .query_sql(
            "SELECT period_code, status FROM biz_accounting_period WHERE period_code LIKE ? \
              ORDER BY period_code"
                .to_string(),
            vec![Value::Text(format!("{year:04}-%"))],
        )
        .await
        .map_err(|error| db_error(error, "查询本年会计期间"))?;
    let mut closed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for row in &rows {
        let code: String = row
            .get_typed("period_code")
            .map_err(|error| db_error(error, "解析期间编码"))?;
        let status: String = row
            .get_typed("status")
            .map_err(|error| db_error(error, "解析期间状态"))?;
        if status == period::STATUS_CLOSED {
            closed.insert(code);
        }
    }
    Ok(period_codes_of(year)
        .into_iter()
        .filter(|code| !closed.contains(code))
        .collect())
}

/// 下一年度**没开账**的期间（不含已结账的：已结账说明它早开过了）。
async fn months_not_opened_next_year(
    ex: &impl SqlExecutor,
    year: i16,
) -> Result<Vec<String>, ApiError> {
    let next = year + 1;
    let rows = ex
        .query_sql(
            "SELECT period_code, status FROM biz_accounting_period WHERE period_code LIKE ? \
              ORDER BY period_code"
                .to_string(),
            vec![Value::Text(format!("{next:04}-%"))],
        )
        .await
        .map_err(|error| db_error(error, "查询下一年度会计期间"))?;
    let mut opened: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for row in &rows {
        let code: String = row
            .get_typed("period_code")
            .map_err(|error| db_error(error, "解析期间编码"))?;
        let status: String = row
            .get_typed("status")
            .map_err(|error| db_error(error, "解析期间状态"))?;
        if status == period::STATUS_OPEN || status == period::STATUS_CLOSED {
            opened.insert(code);
        }
    }
    Ok(period_codes_of(next)
        .into_iter()
        .filter(|code| !opened.contains(code))
        .collect())
}

/// 全年度试算平衡：**分录**求和（不看单头，理由见 `period` 模块文档）。
async fn trial_balance_of_year(
    ex: &impl SqlExecutor,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<(Decimal, Decimal), ApiError> {
    let rows = ex
        .query_sql(
            "SELECT COALESCE(SUM(e.debit_amount), 0)::text AS debit, \
                    COALESCE(SUM(e.credit_amount), 0)::text AS credit \
               FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
              WHERE v.status = 'POSTED' AND v.deleted_at IS NULL \
                AND v.voucher_date >= ?::text::date AND v.voucher_date <= ?::text::date"
                .to_string(),
            vec![Value::Text(start.to_string()), Value::Text(end.to_string())],
        )
        .await
        .map_err(|error| db_error(error, "全年度试算平衡"))?;
    let row = rows
        .first()
        .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "试算平衡查询没有返回行"))?;
    let debit: String = row
        .get_typed("debit")
        .map_err(|error| db_error(error, "解析借方合计"))?;
    let credit: String = row
        .get_typed("credit")
        .map_err(|error| db_error(error, "解析贷方合计"))?;
    Ok((
        Decimal::from_str(&debit).unwrap_or(Decimal::ZERO),
        Decimal::from_str(&credit).unwrap_or(Decimal::ZERO),
    ))
}

/// 截至 `end`（含）全科目的余额：`(科目, Σ借−Σ贷, 科目类别)`。
///
/// 只取**末级**科目（`is_leaf = 1`）：期初台账与报表都以末级为口径，
/// 上级科目写进去就是父子双计。
async fn account_balances(
    ex: &impl SqlExecutor,
    end: NaiveDate,
) -> Result<Vec<(AccountRef, Decimal, String)>, ApiError> {
    let rows = ex
        .query_sql(
            "SELECT a.id, a.account_code, a.account_name, a.account_category, \
                    COALESCE(s.amount, 0)::text AS amount \
               FROM biz_account a \
               LEFT JOIN ( \
                    SELECT e.account_id, SUM(e.debit_amount - e.credit_amount) AS amount \
                      FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
                     WHERE v.status = 'POSTED' AND v.deleted_at IS NULL \
                       AND v.voucher_date <= ?::text::date \
                     GROUP BY e.account_id \
               ) s ON s.account_id = a.id \
              WHERE a.deleted_at IS NULL AND a.is_leaf = 1 \
              ORDER BY a.account_code"
                .to_string(),
            vec![Value::Text(end.to_string())],
        )
        .await
        .map_err(|error| db_error(error, "读取科目余额"))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let amount: String = row
            .get_typed("amount")
            .map_err(|error| db_error(error, "解析科目余额"))?;
        out.push((
            AccountRef {
                id: row
                    .get_typed::<Uuid>("id")
                    .map_err(|error| db_error(error, "读科目 id"))?,
                code: row
                    .get_typed("account_code")
                    .map_err(|error| db_error(error, "读科目编码"))?,
                name: row
                    .get_typed("account_name")
                    .map_err(|error| db_error(error, "读科目名称"))?,
            },
            Decimal::from_str(&amount).unwrap_or(Decimal::ZERO),
            row.get_typed("account_category")
                .map_err(|error| db_error(error, "读科目类别"))?,
        ));
    }
    Ok(out)
}

/// 兜底：年结前确认**没有非末级科目带余额**（fail-closed，不静默漏结）。
///
/// 记账入口已经拒绝非末级科目（`voucher::post_voucher` 的「记账科目口径」闸），
/// 正常情况下这里什么都查不到。能查到的只可能是两类历史脏数据：
///
/// 1. FIN-1（V0226）把 `5602` / `2241` 从末级翻成「仅汇总」**之前**记在父科目上的分录
///    —— 分录的 `account_code` 快照仍是父编码，翻完之后谁也不会自动改写它；
/// 2. 绕过引擎、用裸 SQL 灌进去的夹具 / 数据修复脚本。
///
/// [`account_balances`] 只取 `is_leaf = 1`，这些余额会被**静默跳过**：年结照样「成功」，
/// 但下年期初的借贷合计会差这一笔（实测事故：`5602` 上 400 元费用漏结 → 净利润多 400 →
/// 跨年期初差 150）。所以宁可 409 让人先整理数据，也不要结出一本不平的账。
///
/// 口径是**累计到年末**（不是只看本年）：下年期初取的是全时点余额，早年留下的父科目余额
/// 一样会漏进期初。
async fn ensure_no_non_leaf_balance(ex: &impl SqlExecutor, end: NaiveDate) -> Result<(), ApiError> {
    let rows = ex
        .query_sql(
            "SELECT a.account_code, a.account_name, COALESCE(s.amount, 0)::text AS amount \
               FROM biz_account a \
               JOIN ( \
                    SELECT e.account_id, SUM(e.debit_amount - e.credit_amount) AS amount \
                      FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
                     WHERE v.status = 'POSTED' AND v.deleted_at IS NULL \
                       AND v.voucher_date <= ?::text::date \
                     GROUP BY e.account_id \
               ) s ON s.account_id = a.id \
              WHERE a.deleted_at IS NULL AND a.is_leaf = 0 AND s.amount <> 0 \
              ORDER BY a.account_code"
                .to_string(),
            vec![Value::Text(end.to_string())],
        )
        .await
        .map_err(|error| db_error(error, "检查非末级科目余额"))?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut details = Vec::with_capacity(rows.len());
    for row in &rows {
        let code: String = row
            .get_typed("account_code")
            .map_err(|error| db_error(error, "读科目编码"))?;
        let name: String = row
            .get_typed("account_name")
            .map_err(|error| db_error(error, "读科目名称"))?;
        let amount: String = row
            .get_typed("amount")
            .map_err(|error| db_error(error, "解析科目余额"))?;
        details.push(format!("{code} {name}（余额 {amount}）"));
    }
    Err(conflict(format!(
        "年结被拦住：科目 {} 是非末级（汇总）科目，但账上仍有余额。\
         年结与下年期初都只按末级科目结算，直接结账会把这些余额**静默漏掉**（跨年试算不平）。\
         请先把这些余额调整到它们的末级明细科目（做一张调整凭证），再重新年结",
        details.join("、")
    )))
}

/// 按编码取科目（不存在 / 已删除 → 400，消息给出去处）。
async fn account_of_code(
    ex: &impl SqlExecutor,
    code: &str,
    purpose: &str,
) -> Result<AccountRef, ApiError> {
    voucher::account_by_code(ex, code)
        .await
        .map_err(|error| db_error(error, "查询年结科目"))?
        .ok_or_else(|| {
            bad_request(format!(
                "科目 {code} 不存在或已停用：年结需要它{purpose}，请先在「会计科目」页启用"
            ))
        })
}

/// 下一年度里「不是 OPENING」的凭证（反年结的拦截依据）。
async fn next_year_other_vouchers(
    ex: &impl SqlExecutor,
    next_year: i16,
) -> Result<(i64, Vec<String>), ApiError> {
    let rows = ex
        .query_sql(
            "SELECT voucher_no FROM biz_voucher \
              WHERE deleted_at IS NULL AND period LIKE ? \
                AND voucher_type <> 'OPENING' \
              ORDER BY voucher_no LIMIT 3"
                .to_string(),
            vec![Value::Text(format!("{next_year:04}-%"))],
        )
        .await
        .map_err(|error| db_error(error, "查询下一年度凭证"))?;
    let count: i64 = ex
        .scalar_sql(
            "SELECT COUNT(*) FROM biz_voucher \
              WHERE deleted_at IS NULL AND period LIKE ? AND voucher_type <> 'OPENING'"
                .to_string(),
            vec![Value::Text(format!("{next_year:04}-%"))],
        )
        .await
        .map_err(|error| db_error(error, "统计下一年度凭证"))?;
    let samples = rows
        .iter()
        .map(|row| row.get_typed::<String>("voucher_no").unwrap_or_default())
        .collect::<Vec<_>>();
    Ok((count, samples))
}

/// 本年结转凭证（一个年度最多一张，库层有部分唯一索引兜着）。
struct CarryForwardVoucher {
    id: Uuid,
    voucher_no: String,
    total_debit: Decimal,
    entry_count: i64,
    net_profit: Decimal,
}

/// 读本年结转凭证（含「结转进 3104 的净额」）。
async fn carry_forward_voucher(
    ex: &impl SqlExecutor,
    year: i16,
) -> Result<Option<CarryForwardVoucher>, ApiError> {
    let Some(row) = ex
        .query_first_sql(
            "SELECT id, voucher_no, COALESCE(total_debit, 0)::text AS total_debit, \
                    COALESCE(entry_count, 0) AS entry_count \
               FROM biz_voucher \
              WHERE voucher_type = 'YEAR_END' AND period = ? AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(period::period_code(year, 12))],
        )
        .await
        .map_err(|error| db_error(error, "查询结转凭证"))?
    else {
        return Ok(None);
    };
    let id: Uuid = row
        .get_typed("id")
        .map_err(|error| db_error(error, "读结转凭证 id"))?;
    let total_debit_text: String = row
        .get_typed("total_debit")
        .map_err(|error| db_error(error, "读结转凭证借方合计"))?;
    let entry_count: i32 = row
        .get_typed("entry_count")
        .map_err(|error| db_error(error, "读结转凭证分录数"))?;
    let net_profit_text: String = ex
        .scalar_sql(
            "SELECT COALESCE(SUM(e.credit_amount - e.debit_amount), 0)::text \
               FROM biz_voucher_entry e \
              WHERE e.voucher_id = ?::uuid AND e.account_code = ?"
                .to_string(),
            vec![
                Value::from(&id),
                Value::Text(ACCOUNT_PROFIT_DISTRIBUTION.to_string()),
            ],
        )
        .await
        .map_err(|error| db_error(error, "读结转进利润分配的金额"))?;
    Ok(Some(CarryForwardVoucher {
        id,
        voucher_no: row
            .get_typed("voucher_no")
            .map_err(|error| db_error(error, "读结转凭证号"))?,
        total_debit: Decimal::from_str(&total_debit_text).unwrap_or(Decimal::ZERO),
        entry_count: i64::from(entry_count),
        net_profit: Decimal::from_str(&net_profit_text).unwrap_or(Decimal::ZERO),
    }))
}

/// 下一年度第一个期间的期初台账行数（`(总数, 非年结行数)`）。
async fn next_year_opening_rows(
    ex: &impl SqlExecutor,
    period_code: &str,
) -> Result<(i64, i64), ApiError> {
    let row = ex
        .query_first_sql(
            "SELECT COUNT(*) AS total, \
                    COUNT(*) FILTER (WHERE COALESCE(source, 'MANUAL') <> 'YEAR_END') AS manual \
               FROM biz_opening_balance WHERE period_code = ?"
                .to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .map_err(|error| db_error(error, "统计下年期初"))?;
    match row {
        Some(row) => {
            let total: i64 = row
                .get_typed("total")
                .map_err(|error| db_error(error, "解析下年期初行数"))?;
            let manual: i64 = row
                .get_typed("manual")
                .map_err(|error| db_error(error, "解析下年手工期初行数"))?;
            Ok((total, manual))
        }
        None => Ok((0, 0)),
    }
}

/// 年度结转状态（**唯一判据 = 状态日志最新一行**）。
pub async fn load_state_in(ex: &impl SqlExecutor, year: i16) -> Result<YearEndState, ApiError> {
    let year_text = year.to_string();
    let log = ex
        .query_first_sql(
            "SELECT to_status, reason, operator_name, created_at \
               FROM biz_status_log \
              WHERE entity_type = ? AND entity_no = ? \
              ORDER BY created_at DESC LIMIT 1"
                .to_string(),
            vec![
                Value::Text(state_machine::ENTITY_YEAR_END.to_string()),
                Value::Text(year_text.clone()),
            ],
        )
        .await
        .map_err(|error| db_error(error, "查询年度结转日志"))?;

    let mut status = STATUS_NOT_CLOSED.to_string();
    let mut closed_at = None;
    let mut closed_by = None;
    let mut reason = None;
    if let Some(row) = &log {
        let to_status: String = row
            .get_typed("to_status")
            .map_err(|error| db_error(error, "解析年度状态"))?;
        status = to_status.clone();
        reason = row.get_typed::<String>("reason").ok();
        closed_by = row.get_typed::<String>("operator_name").ok();
        let created: Option<DateTime<Utc>> = row.get_typed("created_at").ok();
        closed_at = created.map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string());
    }
    let closed = status == STATUS_CLOSED;

    let carry = carry_forward_voucher(ex, year).await?;
    let (_, end) = year_range(year)?;
    let net_profit = match &carry {
        Some(voucher) => voucher.net_profit,
        None => {
            // 未年结：本年损益净额（贷方为正）—— 结转前的「本年净利润」
            let text: String = ex
                .scalar_sql(
                    "SELECT COALESCE(-SUM(e.debit_amount - e.credit_amount), 0)::text \
                       FROM biz_voucher_entry e \
                       JOIN biz_voucher v ON v.id = e.voucher_id \
                       JOIN biz_account a ON a.id = e.account_id \
                      WHERE v.status = 'POSTED' AND v.deleted_at IS NULL \
                        AND v.voucher_date <= ?::text::date \
                        AND a.account_category = 'PROFIT_LOSS'"
                        .to_string(),
                    vec![Value::Text(end.to_string())],
                )
                .await
                .map_err(|error| db_error(error, "计算本年损益净额"))?;
            Decimal::from_str(&text).unwrap_or(Decimal::ZERO)
        }
    };

    let not_closed = months_not_closed(ex, year).await?;
    let periods_closed = 12 - i64::try_from(not_closed.len()).unwrap_or(12);
    let opening_period = next_year_opening_period(year);
    let (opening_rows, _manual) = next_year_opening_rows(ex, &opening_period).await?;
    let not_opened = months_not_opened_next_year(ex, year).await?;
    let next_year_open_count = 12 - i64::try_from(not_opened.len()).unwrap_or(12);

    Ok(YearEndState {
        year,
        closed,
        status,
        closed_at,
        closed_by,
        reason,
        carry_forward_voucher_id: carry.as_ref().map(|item| item.id.to_string()),
        carry_forward_voucher_no: carry.as_ref().map(|item| item.voucher_no.clone()),
        carry_forward_total: carry
            .as_ref()
            .map(|item| item.total_debit)
            .unwrap_or(Decimal::ZERO),
        carry_forward_entries: carry.as_ref().map(|item| item.entry_count).unwrap_or(0),
        net_profit,
        periods_closed,
        opening_period,
        opening_balance_created: opening_rows > 0,
        opening_rows,
        next_year_periods_ready: not_opened.is_empty(),
        next_year_open_count,
    })
}

// ==================== 五项前置校验 ====================

/// 五项前置校验（检查接口与年结对**共用同一份实现**，不许两处口径分裂）。
pub async fn collect_blockers(
    ex: &impl SqlExecutor,
    year: i16,
) -> Result<Vec<YearEndBlocker>, ApiError> {
    let (start, end) = year_range(year)?;
    let mut blockers: Vec<YearEndBlocker> = Vec::new();
    // 处理人反查器（与结账页同一实现）：一次预检里同一个权限码只查一次库
    let mut owners = OwnerResolver::new();

    // ① 本年 12 个月必须全部结账
    let open_months = months_not_closed(ex, year).await?;
    if !open_months.is_empty() {
        let count = open_months.len();
        blockers.push(
            YearEndBlocker::new(
                BLOCKER_PERIOD_NOT_CLOSED,
                format!(
                    "{year} 年还有 {count} 个月没结账：{}。年结前必须逐月结账（含 12 月）",
                    open_months.join("、")
                ),
                Some(&format!("/business/erp/period?year={year}")),
            )
            .with_periods(open_months)
            .with_count(i64::try_from(count).unwrap_or(0)),
        );
    }

    // ② 本年不能有未完成单据
    //
    // **明细与条数同源**：直接用 `period_items` 的日期区间入口（全年一次 BETWEEN 取完），
    // 不再自写一份 COUNT(*) SQL —— 两份 SQL 各自演化就会出现「文案说 3 张、表格列 5 行」。
    // 明细合并（采购 / 收货 / 调拨）后统一截断到 50 条，与结账页同一个上限。
    let (docs, doc_items) =
        period_items::unfinished_doc_items_between(ex, start, end, &mut owners).await?;
    let total: i64 = docs.iter().map(|(_, count)| *count).sum();
    if total > 0 {
        let detail = docs
            .iter()
            .filter(|(_, count)| *count > 0)
            .map(|(label, count)| format!("{label} {count} 张"))
            .collect::<Vec<_>>()
            .join(" / ");
        blockers.push(
            YearEndBlocker::new(
                BLOCKER_DOC_UNFINISHED,
                format!(
                    "{year} 年还有 {total} 张单据未完成（{detail}）：年结前单据必须到终态\
                     （已过账 / 已作废 / 已完成），否则明年才到的货会记不进账{}",
                    period::limit_note(total)
                ),
                Some("/business/erp/stock-in"),
            )
            .with_count(total)
            .with_items(doc_items),
        );
    }

    // ③ 全年度试算平衡
    let (debit, credit) = trial_balance_of_year(ex, start, end).await?;
    let diff = debit - credit;
    if diff.abs() > balance_tolerance() {
        blockers.push(YearEndBlocker {
            kind: BLOCKER_UNBALANCED.to_string(),
            periods: Vec::new(),
            count: None,
            diff: Some(diff.abs().round_dp(4).normalize().to_string()),
            message: format!(
                "{year} 年借贷不平，差 {} 元（借方合计 {debit} / 贷方合计 {credit}）：\
                 不平的账不能年结，请先修凭证",
                diff.abs().round_dp(4).normalize()
            ),
            link: Some("/business/erp/ledger".to_string()),
            samples: Vec::new(),
            // 借贷不平是「整年合计」的差，没有某一张单据能负责 → 不编造明细行
            items: Vec::new(),
        });
    }

    // ④ 全年度不变量（A1 / A2）
    //
    // 明细同样复用 `period_items::invariant_details_between`（与结账页同一份 SQL）：
    // 单号 / 状态标签 / 「去看凭证」路由都由它给，年结这边只负责挂到 blocker 上。
    let (failures, invariant_items) =
        period_items::invariant_details_between(ex, start, end, &mut owners).await?;
    if !failures.is_empty() {
        let count = i64::try_from(failures.len()).unwrap_or(i64::MAX);
        // 上限口径统一：**任何一条 blocker 的明细都不超过 50 行**
        // （A1 / A2 各取 50 再按凭证去重，理论上能到 100 行，这里再兜一次）。
        let mut invariant_items = invariant_items;
        invariant_items.truncate(usize::try_from(period_items::ITEM_LIMIT).unwrap_or(50));
        blockers.push(
            YearEndBlocker::new(
                BLOCKER_INVARIANT,
                format!(
                    "{year} 年不变量未通过（{count} 项）：{}。账不平不能年结，请先修复",
                    failures.join("；")
                ),
                Some("/business/erp/inventory?audit=1"),
            )
            .with_count(count)
            .with_items(invariant_items),
        );
    }

    // ⑤ 下一年度 12 个期间必须已开账
    let not_opened = months_not_opened_next_year(ex, year).await?;
    if !not_opened.is_empty() {
        let next = year + 1;
        let count = not_opened.len();
        blockers.push(
            YearEndBlocker::new(
                BLOCKER_NEXT_YEAR_NOT_OPENED,
                format!(
                    "{next} 年期间还没开账（缺 {} 个月：{}）：年结要往 {next}-01 写期初，\
                     请先在「会计期间」页批量开账",
                    count,
                    not_opened.join("、")
                ),
                Some(&format!("/business/erp/period?year={next}")),
            )
            .with_periods(not_opened)
            .with_count(i64::try_from(count).unwrap_or(0)),
        );
    }

    // 附加闸：下一年度第一个期间的期初已经被**手工**录过（年结会覆盖它）
    let opening_period = next_year_opening_period(year);
    let (_, manual_rows) = next_year_opening_rows(ex, &opening_period).await?;
    if manual_rows > 0 {
        blockers.push(
            YearEndBlocker::new(
                BLOCKER_NEXT_YEAR_OPENING_EXISTS,
                format!(
                    "{opening_period} 的期初已经有 {manual_rows} 行是手工录的：年结会用本年期末\
                     覆盖它，可能悄悄抹掉财务录入的数据。请先在「期初余额」页清空该期间，\
                     或确认由年结接管"
                ),
                Some(&format!("/business/erp/opening?period={opening_period}")),
            )
            .with_count(manual_rows),
        );
    }

    Ok(blockers)
}

/// 把阻塞项拼成 409 文案（**逐条点名**，卡面 O5）。
pub fn rejection_message(year: i16, blockers: &[YearEndBlocker]) -> String {
    let detail = blockers
        .iter()
        .map(|blocker| blocker.message.clone())
        .collect::<Vec<_>>()
        .join("；");
    format!("{year} 年度不能结转：{detail}")
}

// ==================== 接口层前置检查 ====================

/// 前置检查（只读）。
pub async fn check_year(year: i16) -> Result<YearEndCheckRes, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| db_error(error, "开启年结检查事务"))?;
    let state = load_state_in(&tx, year).await?;
    let mut blockers = collect_blockers(&tx, year).await?;
    let _ = tx.rollback().await;
    if state.closed {
        blockers.insert(
            0,
            YearEndBlocker::new(
                BLOCKER_ALREADY_CLOSED,
                format!(
                    "{year} 年度已结转（{} {}）：不能重复年结，如需重做请先反年结",
                    state.closed_at.clone().unwrap_or_else(|| "-".to_string()),
                    state.closed_by.clone().unwrap_or_default()
                ),
                Some("/business/erp/year-end"),
            ),
        );
    }
    let can_close = blockers.is_empty();
    Ok(YearEndCheckRes {
        year,
        can_close,
        blockers,
        state,
    })
}

// ==================== 年结 ====================

/// 年结：五项校验 + 结转凭证 + 下年期初 + 留痕，**同一个事务**（卡面 L11）。
pub async fn close_year(
    year: i16,
    reason: Option<&str>,
    operator: state_machine::Operator,
) -> Result<YearEndCloseRes, ApiError> {
    let remark = optional_close_reason(reason)?;
    let (_, end) = year_range(year)?;
    let now = Utc::now();

    let tx = db()
        .begin()
        .await
        .map_err(|error| db_error(error, "开启年结事务"))?;

    // 事务级 advisory lock：同一会计年度的并发年结串行化
    // （空年度没有结转凭证，唯一索引兜不住，必须自己上锁）
    tx.exec_sql(
        "SELECT pg_advisory_xact_lock(?)".to_string(),
        vec![Value::I64(advisory_key(year))],
    )
    .await
    .map_err(|error| db_error(error, "锁定会计年度"))?;

    let state = load_state_in(&tx, year).await?;
    if state.closed {
        return Err(conflict(format!(
            "{year} 年度已经结转过了（{}），不能重复年结：如需重做请先反年结",
            state.closed_at.clone().unwrap_or_else(|| "-".to_string())
        )));
    }

    let blockers = collect_blockers(&tx, year).await?;
    if !blockers.is_empty() {
        return Err(conflict(rejection_message(year, &blockers)));
    }

    // 兜底闸（fail-closed）：非末级科目上不许有余额，否则结转/期初会静默漏掉它。
    // 放在写出任何东西之前 —— 拦在门口，比结出一本不平的账再回滚便宜得多。
    ensure_no_non_leaf_balance(&tx, end).await?;

    // ---------- ① 损益结转凭证 ----------
    let balances = account_balances(&tx, end).await?;
    let current_profit =
        account_of_code(&tx, ACCOUNT_CURRENT_PROFIT, "作为本年利润的结转落点").await?;
    let profit_distribution =
        account_of_code(&tx, ACCOUNT_PROFIT_DISTRIBUTION, "接收本年利润").await?;

    let pl_rows: Vec<(AccountRef, Decimal)> = balances
        .iter()
        .filter(|(_, _, category)| category == "PROFIT_LOSS")
        .map(|(account, balance, _)| (account.clone(), *balance))
        .collect();
    let current_profit_balance = balances
        .iter()
        .find(|(account, _, _)| account.code == ACCOUNT_CURRENT_PROFIT)
        .map(|(_, balance, _)| *balance)
        .unwrap_or(Decimal::ZERO);

    let lines = build_closing_lines(
        &pl_rows,
        &current_profit,
        &profit_distribution,
        current_profit_balance,
    );

    let voucher_id = if lines.is_empty() {
        // 空年度：没有损益可结、也没有本年利润 → 不出凭证（卡面 §七 ⑭）
        tracing::info!(year, "本年度没有损益余额，跳过结转凭证");
        None
    } else {
        let (debit, credit) = voucher::totals_of(&lines);
        if debit != credit {
            return Err(ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!(
                    "年结分录自身不平衡（借 {debit} / 贷 {credit}）：这是本模块的 bug，\
                     请把这条消息报给后端"
                ),
            ));
        }
        let source_no = format!("YEAR-END-{year}");
        let id = voucher::post_voucher(
            &tx,
            &PostVoucher {
                voucher_type: voucher::VOUCHER_TYPE_YEAR_END,
                source_type: voucher::SOURCE_TYPE_YEAR_END,
                source_id: None,
                source_no: Some(&source_no),
                voucher_date: end,
                summary: format!("{year} 年度损益结转"),
                lines,
                remark: remark.as_deref().or(Some(
                    "年度损益结转（由「年结」页生成：损益类结平 + 本年利润转利润分配）",
                )),
                red_of: None,
                status: crate::business::erp::voucher::STATUS_POSTED,
            },
            operator.id,
            now,
        )
        .await
        .map_err(|error| utils::db_error(error, "生成年度结转凭证"))?;
        Some(id)
    };

    // ---------- ② 下年期初台账（不生成期初凭证，理由见模块文档 ②）----------
    let balances_after = account_balances(&tx, end).await?;
    let opening_rows = opening_rows_of(&balances_after);
    let opening_period = next_year_opening_period(year);
    opening::write_year_end_opening_in(&tx, &opening_period, &opening_rows, operator.id, now)
        .await?;

    // ---------- ③ 留痕（本年 12 月保持 CLOSED，卡面 §一 ④）----------
    let carry = carry_forward_voucher(&tx, year).await?;
    let entity_id = voucher_id.unwrap_or_else(Uuid::now_v7);
    state_machine::transition(
        &tx,
        state_machine::ENTITY_YEAR_END,
        entity_id,
        &year.to_string(),
        &state_machine::YEAR_END_EDGES,
        STATUS_NOT_CLOSED,
        STATUS_CLOSED,
        operator,
        remark.as_deref().or(Some("年度结转")),
        carry
            .as_ref()
            .map(|item| (voucher::VOUCHER_TYPE_YEAR_END, item.voucher_no.as_str())),
    )
    .await?;

    let state = load_state_in(&tx, year).await?;
    tx.commit()
        .await
        .map_err(|error| db_error(error, "提交年结事务"))?;

    tracing::info!(
        year,
        voucher = state.carry_forward_voucher_no.as_deref().unwrap_or("-"),
        opening_rows = state.opening_rows,
        "年度结转完成"
    );

    Ok(YearEndCloseRes {
        year,
        carry_forward_voucher_no: state.carry_forward_voucher_no.clone(),
        opening_balance_created: state.opening_balance_created,
        opening_rows: state.opening_rows,
        periods_closed: state.periods_closed,
        next_year_periods_ready: state.next_year_periods_ready,
        net_profit: state.net_profit,
        entry_count: state.carry_forward_entries,
        state,
    })
}

// ==================== 反年结 ====================

/// 反年结：删结转凭证 → 删下年期初 → 12 月解封 → 留痕（**同一个事务**）。
///
/// 前置：
/// * 本年度已结转（状态日志最新一行为 `CLOSED`）；
/// * 下一年度除 `OPENING` 外**没有任何凭证** —— 明年已经用起来了还拆今年的账，
///   会让明年的余额与新生成的下年期初对不上（卡面 §五）。
pub async fn reopen_year(
    year: i16,
    reason: Option<&str>,
    operator: state_machine::Operator,
) -> Result<YearEndReopenRes, ApiError> {
    let reason = require_reopen_reason(reason)?;
    let now = Utc::now();

    let tx = db()
        .begin()
        .await
        .map_err(|error| db_error(error, "开启反年结事务"))?;

    tx.exec_sql(
        "SELECT pg_advisory_xact_lock(?)".to_string(),
        vec![Value::I64(advisory_key(year))],
    )
    .await
    .map_err(|error| db_error(error, "锁定会计年度"))?;

    let state = load_state_in(&tx, year).await?;
    if !state.closed {
        return Err(conflict(format!(
            "{year} 年度当前是「未结转」：没有年结可反。反年结只用于撤销一次已经做完的年度结转"
        )));
    }

    // 下一年度不能有「非期初」凭证
    let (count, samples) = next_year_other_vouchers(&tx, year + 1).await?;
    if count > 0 {
        return Err(conflict(format!(
            "{} 年已经有 {count} 张凭证（{}）：反年结会让新生成的期初与这些凭证冲突，\
             请先作废/删除它们（或先反年结下一年度）",
            year + 1,
            if samples.is_empty() {
                "-".to_string()
            } else {
                samples.join(" / ")
            }
        )));
    }

    // ---------- ① 删结转凭证 ----------
    let mut deleted_voucher_no = None;
    let mut deleted_entries = 0_i64;
    if let Some(voucher) = carry_forward_voucher(&tx, year).await? {
        deleted_voucher_no = Some(voucher.voucher_no.clone());
        deleted_entries = voucher.entry_count;
        // 凭证的状态日志随凭证一起清（`biz_status_log` 没有外键，留着就是孤儿行）
        tx.exec_sql(
            "DELETE FROM biz_status_log WHERE entity_type = ? AND entity_id = ?::uuid".to_string(),
            vec![
                Value::Text(state_machine::ENTITY_VOUCHER.to_string()),
                Value::from(&voucher.id),
            ],
        )
        .await
        .map_err(|error| db_error(error, "删除结转凭证状态日志"))?;
        tx.exec_sql(
            "DELETE FROM biz_voucher WHERE id = ?::uuid".to_string(),
            vec![Value::from(&voucher.id)],
        )
        .await
        .map_err(|error| db_error(error, "删除结转凭证"))?;
    }

    // ---------- ② 删下年期初 ----------
    let opening_period = next_year_opening_period(year);
    let deleted_opening_rows = opening::clear_opening_in(&tx, &opening_period).await?;

    // ---------- ③ 12 月解封 ----------
    let december = period::period_code(year, 12);
    let mut december_reopened = false;
    if let Some(row) = tx
        .query_first_sql(
            "SELECT id, status FROM biz_accounting_period WHERE period_code = ? FOR UPDATE"
                .to_string(),
            vec![Value::Text(december.clone())],
        )
        .await
        .map_err(|error| db_error(error, "锁定 12 月期间"))?
    {
        let id: Uuid = row
            .get_typed("id")
            .map_err(|error| db_error(error, "读 12 月期间 id"))?;
        let status: String = row
            .get_typed("status")
            .map_err(|error| db_error(error, "读 12 月期间状态"))?;
        if status == period::STATUS_CLOSED {
            state_machine::transition(
                &tx,
                state_machine::ENTITY_PERIOD,
                id,
                &december,
                &state_machine::PERIOD_EDGES,
                period::STATUS_CLOSED,
                period::STATUS_OPEN,
                operator.clone(),
                Some(&reason),
                None,
            )
            .await?;
            tx.exec_sql(
                "UPDATE biz_accounting_period \
                    SET status = ?, closed_at = NULL, closed_by = NULL, updated_at = ? \
                  WHERE id = ?::uuid AND status = ?"
                    .to_string(),
                vec![
                    Value::Text(period::STATUS_OPEN.to_string()),
                    Value::from(now),
                    Value::from(&id),
                    Value::Text(period::STATUS_CLOSED.to_string()),
                ],
            )
            .await
            .map_err(|error| db_error(error, "解封 12 月期间"))?;
            december_reopened = true;
        }
    }

    // ---------- ④ 留痕 ----------
    state_machine::transition(
        &tx,
        state_machine::ENTITY_YEAR_END,
        state
            .carry_forward_voucher_id
            .as_deref()
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .unwrap_or_else(Uuid::now_v7),
        &year.to_string(),
        &state_machine::YEAR_END_EDGES,
        STATUS_CLOSED,
        STATUS_NOT_CLOSED,
        operator,
        Some(&reason),
        deleted_voucher_no
            .as_deref()
            .map(|no| (voucher::VOUCHER_TYPE_YEAR_END, no)),
    )
    .await?;

    let state = load_state_in(&tx, year).await?;
    tx.commit()
        .await
        .map_err(|error| db_error(error, "提交反年结事务"))?;

    tracing::warn!(
        year,
        voucher = deleted_voucher_no.as_deref().unwrap_or("-"),
        opening_rows = deleted_opening_rows,
        december_reopened,
        "反年结完成"
    );

    Ok(YearEndReopenRes {
        year,
        reopened: true,
        reason,
        deleted_voucher_no,
        deleted_entries,
        deleted_opening_rows,
        december_reopened,
        state,
    })
}

/// 年结的 advisory lock key（年度 * 1000 + 7，避开期间模块的取值）。
fn advisory_key(year: i16) -> i64 {
    i64::from(year) * 1000 + 7
}

// ==================== 接口层 ====================

/// 年度结转状态 + 前置检查。
#[nik::permission(
    path = "/business/erp/year-end/check",
    method = "GET",
    permission = "business:erp:yearend:read"
)]
#[nik::get("/business/erp/year-end/check")]
pub async fn year_end_check_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<YearEndQuery>,
) -> ResResult<YearEndCheckRes> {
    period::ensure_permission(&claims, PERM_READ, "查看年度结转状态")?;
    let year = validate_year(query.year)?;
    Ok(Res::ok(check_year(year).await?))
}

/// 年结（损益结转 + 生成下年期初）。
#[nik::permission(
    path = "/business/erp/year-end/close",
    method = "POST",
    permission = "business:erp:yearend:close"
)]
#[nik::post("/business/erp/year-end/close")]
pub async fn year_end_close_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<YearEndCloseReq>,
) -> ResResult<YearEndCloseRes> {
    period::ensure_permission(&claims, PERM_CLOSE, "年结")?;
    let year = validate_year(Some(req.year))?;
    Ok(Res::ok(
        close_year(
            year,
            req.reason.as_deref(),
            state_machine::Operator::from(&claims),
        )
        .await?,
    ))
}

/// 反年结（撤销整年的结转）。
#[nik::permission(
    path = "/business/erp/year-end/reopen",
    method = "POST",
    permission = "business:erp:yearend:reopen"
)]
#[nik::post("/business/erp/year-end/reopen")]
pub async fn year_end_reopen_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<YearEndReopenReq>,
) -> ResResult<YearEndReopenRes> {
    period::ensure_permission(&claims, PERM_REOPEN, "反年结")?;
    let year = validate_year(Some(req.year))?;
    Ok(Res::ok(
        reopen_year(
            year,
            req.reason.as_deref(),
            state_machine::Operator::from(&claims),
        )
        .await?,
    ))
}

// ==================== 单测（真库，15 条）====================
//
// 需要真库：`ERP_DB_URL` 指向「已应用 V0137–V0190 的隔离库」；缺省则全部 SKIP：
//
// ```bash
// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_r4'
// cargo test --lib business::erp::year_end -- --test-threads=1
// ```
//
// # 夹具约定（三条都不能破）
//
// 1. 年度一律 `2021`（被年结的年度）/ `2022` / `2023`（下一年度）。
//    生产在用 `2026-xx`，period 模块在 `2031–2033`，opening 在 `2034–2035`
//    —— **绝不碰这些**：年结会改期间状态、写结转凭证，撞上就是跨模块污染。
// 2. 串行（`DB_LOCK`）：清理按前缀删，并行会互删夹具。
// 3. 金额断言一律**增量口径**：`net_before` 是「截至某日的历史累计」，别的模块的
//    夹具（2026 的银行存款 / 5001）也在里面。先量一次基线，再断言差
//    （见 `references/financial-period-cards.md` §4）。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::voucher::{AccountRef, EntryLine, PostVoucher};
    use std::str::FromStr;

    /// 真库用例串行跑。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    /// 夹具操作人。
    const TEST_UID: &str = "00000000-0000-7000-8000-0000000000c4";
    /// 夹具用的**末级**费用科目：`5602.01 管理费用-折旧费`（V0226 / FIN-1 种子）。
    ///
    /// 不能用 `5602` 本级：它自 V0226 起 `is_leaf = 0`（8 个末级子科目挂在下面），
    /// 记账入口会 400，年结也只会结末级 —— 父科目上的余额会被静默漏掉。
    const EXPENSE_ACCOUNT: &str = "5602.01";

    /// 夹具操作人 id。
    fn uid() -> Uuid {
        Uuid::parse_str(TEST_UID).expect("测试 uuid 非法")
    }

    /// 夹具操作人。
    fn operator() -> state_machine::Operator {
        state_machine::Operator::named(Some(uid()), "r4-tester")
    }

    /// 金额字面量。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试金额字面量非法")
    }

    /// 连库（只从 `ERP_DB_URL` 取；缺省 SKIP）。
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
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 执行一条写 SQL（失败直接 panic，不静默吞错）。
    async fn sql(text: &str) {
        if let Err(err) = db().execute_with_params(text, vec![]).await {
            panic!("SQL 失败：{err}\nSQL: {text}");
        }
    }

    /// 清掉本模块造的所有夹具（按前缀 / 年度收敛，**不碰生产年度**）。
    async fn cleanup() {
        sql(
            "DELETE FROM biz_status_log WHERE entity_type IN ('ACCOUNTING_YEAR', 'ACCOUNTING_PERIOD') \
             AND (entity_no IN ('2021', '2022', '2023') \
                  OR entity_no LIKE '2021-%' OR entity_no LIKE '2022-%' OR entity_no LIKE '2023-%')",
        )
        .await;
        // 结转凭证的 VOUCHER 日志（entity_id 指向凭证）
        sql("DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' \
             AND entity_id IN (SELECT id FROM biz_voucher \
                                WHERE period LIKE '2021-%' OR period LIKE '2022-%' \
                                   OR period LIKE '2023-%')")
        .await;
        sql(
            "DELETE FROM biz_voucher WHERE period LIKE '2021-%' OR period LIKE '2022-%' \
              OR period LIKE '2023-%'",
        )
        .await;
        sql("DELETE FROM biz_opening_balance WHERE period_code IN \
             ('2022-01', '2023-01', '2021-01', '2022-12', '2023-12')")
        .await;
        sql("DELETE FROM biz_purchase_order WHERE order_no LIKE 'R4-PO-%'").await;
        sql(
            "DELETE FROM biz_accounting_period WHERE period_code LIKE '2021-%' \
              OR period_code LIKE '2022-%' OR period_code LIKE '2023-%'",
        )
        .await;
    }

    /// 取锁 + 连库 + 清场。`None` = 没有测试库，用例直接 return。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    /// 批量开账整年（走生产同款入口）。
    async fn open_year(year: i16) {
        period::batch_open(
            &format!("{year}-01"),
            &format!("{year}-12"),
            Some("R4 夹具"),
            operator(),
        )
        .await
        .unwrap_or_else(|err| panic!("开账 {year} 失败：{} {}", err.code, err.msg));
    }

    /// 直接把 12 个月置为已结账（本卡测的是年结侧的闸，不走 P1 的结账预检）。
    ///
    /// 顺序很重要：**先记凭证再封账**（封账后 `post_voucher` 会 400）。
    async fn close_all_months(year: i16) {
        sql(&format!(
            "UPDATE biz_accounting_period SET status = 'CLOSED', closed_at = now() \
              WHERE period_code LIKE '{year:04}-%'"
        ))
        .await;
    }

    /// 期间状态。
    async fn period_status(code: &str) -> Option<String> {
        let rows = db()
            .find_with_params(
                "SELECT status FROM biz_accounting_period WHERE period_code = ?",
                vec![Value::Text(code.to_string())],
            )
            .await
            .expect("读期间状态");
        rows.first()
            .map(|row| row.get_typed::<String>("status").expect("解析期间状态"))
    }

    /// 记一张平衡凭证（`(科目, 借, 贷)`）。
    async fn post_at(date: NaiveDate, source_no: &str, lines: &[(&str, &str, &str)]) {
        let tx = db().begin().await.expect("开启凭证事务");
        let mut entries = Vec::new();
        for (index, (code, debit, credit)) in lines.iter().enumerate() {
            let account = voucher::account_required(&tx, code, "R4 夹具")
                .await
                .unwrap_or_else(|err| panic!("取科目 {code} 失败：{err}"));
            entries.push(EntryLine {
                line_no: i32::try_from(index + 1).unwrap_or(0),
                summary: format!("R4 夹具 {source_no}"),
                account,
                debit: dec(debit),
                credit: dec(credit),
                aux_supplier_id: None,
                aux_material_id: None,
                aux_dept_id: None,
                aux_warehouse_id: None,
            });
        }
        voucher::post_voucher(
            &tx,
            &PostVoucher {
                voucher_type: voucher::VOUCHER_TYPE_MANUAL,
                source_type: voucher::SOURCE_MANUAL,
                source_id: None,
                source_no: Some(source_no),
                voucher_date: date,
                summary: format!("R4 夹具 {source_no}"),
                lines: entries,
                remark: Some("R4 夹具"),
                red_of: None,
                status: crate::business::erp::voucher::STATUS_POSTED,
            },
            Some(uid()),
            Utc::now(),
        )
        .await
        .unwrap_or_else(|err| panic!("记凭证 {source_no} 失败：{err}"));
        tx.commit().await.expect("提交凭证事务");
    }

    /// 某科目截至 `end` 的余额（Σ借 − Σ贷）。
    async fn balance_of(code: &str, end: NaiveDate) -> Decimal {
        let text: String = db()
            .scalar_with_params(
                "SELECT COALESCE(SUM(e.debit_amount - e.credit_amount), 0)::text \
                   FROM biz_voucher_entry e \
                   JOIN biz_voucher v ON v.id = e.voucher_id \
                  WHERE v.status = 'POSTED' AND v.deleted_at IS NULL \
                    AND v.voucher_date <= ?::text::date AND e.account_code = ?",
                vec![Value::Text(end.to_string()), Value::Text(code.to_string())],
            )
            .await
            .expect("读科目余额");
        Decimal::from_str(&text).unwrap_or(Decimal::ZERO)
    }

    /// 全部损益类科目截至 `end` 的余额合计（Σ借 − Σ贷）。
    async fn pl_total(end: NaiveDate) -> Decimal {
        let text: String = db()
            .scalar_with_params(
                "SELECT COALESCE(SUM(e.debit_amount - e.credit_amount), 0)::text \
                   FROM biz_voucher_entry e \
                   JOIN biz_voucher v ON v.id = e.voucher_id \
                   JOIN biz_account a ON a.id = e.account_id \
                  WHERE v.status = 'POSTED' AND v.deleted_at IS NULL \
                    AND v.voucher_date <= ?::text::date \
                    AND a.account_category = 'PROFIT_LOSS'",
                vec![Value::Text(end.to_string())],
            )
            .await
            .expect("读损益余额");
        Decimal::from_str(&text).unwrap_or(Decimal::ZERO)
    }

    /// 本年结转凭证的分录 `(科目编码, 借, 贷)`。
    async fn carry_entries(year: i16) -> Vec<(String, Decimal, Decimal)> {
        let rows = db()
            .find_with_params(
                "SELECT e.account_code, e.debit_amount::text AS debit, e.credit_amount::text AS credit \
                   FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
                  WHERE v.voucher_type = 'YEAR_END' AND v.period = ? AND v.deleted_at IS NULL \
                  ORDER BY e.line_no",
                vec![Value::Text(format!("{year:04}-12"))],
            )
            .await
            .expect("读结转凭证分录");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed::<String>("account_code").expect("科目编码"),
                    Decimal::from_str(&row.get_typed::<String>("debit").expect("借方"))
                        .unwrap_or(Decimal::ZERO),
                    Decimal::from_str(&row.get_typed::<String>("credit").expect("贷方"))
                        .unwrap_or(Decimal::ZERO),
                )
            })
            .collect()
    }

    /// 结转凭证里某科目某侧合计。
    fn side_of(entries: &[(String, Decimal, Decimal)], code: &str, side: &str) -> Decimal {
        entries
            .iter()
            .filter(|(account, _, _)| account == code)
            .map(
                |(_, debit, credit)| {
                    if side == "debit" { *debit } else { *credit }
                },
            )
            .sum()
    }

    /// 一张年结凭证的条数 / 是否存在。
    async fn carry_voucher_count(year: i16) -> i64 {
        db().scalar_with_params(
            "SELECT COUNT(*) FROM biz_voucher \
              WHERE voucher_type = 'YEAR_END' AND period = ? AND deleted_at IS NULL",
            vec![Value::Text(format!("{year:04}-12"))],
        )
        .await
        .expect("统计结转凭证")
    }

    /// 下年期初台账的一行 `(借, 贷, 来源)`。
    async fn opening_row(period: &str, code: &str) -> Option<(Decimal, Decimal, String)> {
        let rows = db()
            .find_with_params(
                "SELECT debit_opening::text AS debit, credit_opening::text AS credit, source \
                   FROM biz_opening_balance WHERE period_code = ? AND account_code = ?",
                vec![
                    Value::Text(period.to_string()),
                    Value::Text(code.to_string()),
                ],
            )
            .await
            .expect("读期初行");
        rows.first().map(|row| {
            (
                Decimal::from_str(&row.get_typed::<String>("debit").expect("借方"))
                    .unwrap_or(Decimal::ZERO),
                Decimal::from_str(&row.get_typed::<String>("credit").expect("贷方"))
                    .unwrap_or(Decimal::ZERO),
                row.get_typed::<String>("source").expect("来源"),
            )
        })
    }

    /// 下年期初行数。
    async fn opening_count(period: &str) -> i64 {
        db().scalar_with_params(
            "SELECT COUNT(*) FROM biz_opening_balance WHERE period_code = ?",
            vec![Value::Text(period.to_string())],
        )
        .await
        .expect("统计期初行")
    }

    /// 造一张草稿采购单（本年未完成单据）。
    async fn seed_draft_po(no: &str, order_date: &str) {
        // 只填 NOT NULL 且无默认值的列（`warehouse_id` 是弱引用，不校验存在性）
        sql(&format!(
            "INSERT INTO biz_purchase_order \
                (order_no, supplier_id, warehouse_id, order_date, status) \
             VALUES ('{no}', '00000000-0000-7000-8000-0000000000aa'::uuid, \
                     '00000000-0000-7000-8000-0000000000bb'::uuid, \
                     '{order_date}'::date, 'DRAFT')"
        ))
        .await;
    }

    // ==================== ① 前置校验 ====================

    /// ① 有月份未结账 → 409 + 点名未结账月份。
    #[tokio::test]
    async fn r4_01_period_not_closed_blocks() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        open_year(2022).await;
        // 只封 1–10 月，11 / 12 月还开着
        sql("UPDATE biz_accounting_period SET status = 'CLOSED' \
              WHERE period_code LIKE '2021-%' AND period_code < '2021-11'")
        .await;

        let check = check_year(2021).await.expect("前置检查");
        assert!(!check.can_close, "还有两个月没结账，不能年结");
        let blocker = check
            .blockers
            .iter()
            .find(|item| item.kind == BLOCKER_PERIOD_NOT_CLOSED)
            .expect("必须报 PERIOD_NOT_CLOSED");
        assert_eq!(
            blocker.periods,
            vec!["2021-11".to_string(), "2021-12".to_string()],
            "必须点名未结账的月份"
        );

        let error = close_year(2021, None, operator())
            .await
            .expect_err("未结账的年度不许年结");
        assert_eq!(error.status, 409, "前置校验不过 → 409");
        for code in ["2021-11", "2021-12"] {
            assert!(
                error.msg.contains(code),
                "409 文案要点名 {code}：{}",
                error.msg
            );
        }
        assert_eq!(carry_voucher_count(2021).await, 0, "被拦下就不该留下凭证");
    }

    /// ② 有未完成单据 → 409。
    #[tokio::test]
    async fn r4_02_unfinished_doc_blocks() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        open_year(2022).await;
        seed_draft_po("R4-PO-0001", "2021-06-15").await;
        close_all_months(2021).await;

        let check = check_year(2021).await.expect("前置检查");
        assert!(!check.can_close);
        let blocker = check
            .blockers
            .iter()
            .find(|item| item.kind == BLOCKER_DOC_UNFINISHED)
            .expect("必须报 DOC_UNFINISHED");
        assert_eq!(blocker.count, Some(1), "1 张草稿采购单");

        let error = close_year(2021, None, operator())
            .await
            .expect_err("有未完成单据不许年结");
        assert_eq!(error.status, 409);
        assert!(
            error.msg.contains("采购单 1 张"),
            "409 文案要点名单据类型与张数：{}",
            error.msg
        );
    }

    /// ③ 试算不平 → 409 + 差额（同时必然踩到 A1 不变量，两条都要点名）。
    #[tokio::test]
    async fn r4_03_unbalanced_blocks() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        open_year(2022).await;
        post_at(
            NaiveDate::from_ymd_opt(2021, 5, 10).expect("日期"),
            "R4-UNBAL-1",
            &[("1002", "100", "0"), ("5001", "0", "100")],
        )
        .await;
        // 绕过接口把一条分录改歪 10 元（模拟手工改库）
        sql(
            "UPDATE biz_voucher_entry SET debit_amount = debit_amount + 10 \
              WHERE account_code = '1002' \
                AND voucher_id IN (SELECT id FROM biz_voucher WHERE source_no = 'R4-UNBAL-1')",
        )
        .await;
        close_all_months(2021).await;

        let check = check_year(2021).await.expect("前置检查");
        assert!(!check.can_close);
        let blocker = check
            .blockers
            .iter()
            .find(|item| item.kind == BLOCKER_UNBALANCED)
            .expect("必须报 UNBALANCED");
        assert_eq!(blocker.diff.as_deref(), Some("10"), "必须给出差额 10");

        let error = close_year(2021, None, operator())
            .await
            .expect_err("不平的账不许年结");
        assert_eq!(error.status, 409);
        assert!(
            error.msg.contains("借贷不平") && error.msg.contains("10"),
            "409 文案要点名差额：{}",
            error.msg
        );
    }

    /// ④ 下一年度未开账 → 409。
    #[tokio::test]
    async fn r4_04_next_year_not_opened_blocks() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        close_all_months(2021).await;
        // 2022 **不**开账

        let check = check_year(2021).await.expect("前置检查");
        assert!(!check.can_close);
        let blocker = check
            .blockers
            .iter()
            .find(|item| item.kind == BLOCKER_NEXT_YEAR_NOT_OPENED)
            .expect("必须报 NEXT_YEAR_NOT_OPENED");
        assert_eq!(blocker.periods.len(), 12, "12 个月都没开账");
        assert!(
            blocker.message.contains("2022 年期间还没开账"),
            "{}",
            blocker.message
        );

        let error = close_year(2021, None, operator())
            .await
            .expect_err("下一年度没开账不许年结");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("2022"), "{}", error.msg);
    }

    /// ⑤ 五项全过 → can_close = true（且 check 不写任何东西）。
    #[tokio::test]
    async fn r4_05_all_checks_pass() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        open_year(2022).await;
        post_at(
            NaiveDate::from_ymd_opt(2021, 3, 20).expect("日期"),
            "R4-OK-1",
            &[("1002", "1000", "0"), ("5001", "0", "1000")],
        )
        .await;
        close_all_months(2021).await;

        let check = check_year(2021).await.expect("前置检查");
        assert!(
            check.blockers.is_empty(),
            "不该有阻塞项：{:?}",
            check.blockers
        );
        assert!(check.can_close, "五项全过 → 可以年结");
        assert!(!check.state.closed, "还没年结");
        assert_eq!(check.state.periods_closed, 12, "本年 12 个月都已结账");
        assert!(
            check.state.next_year_periods_ready,
            "下一年度 12 个月已开账"
        );
        assert_eq!(carry_voucher_count(2021).await, 0, "只读检查不落凭证");
    }

    // ==================== ② 年结 ====================

    /// ⑥ 收入类结转到本年利润（借收入 / 贷 3103）。
    #[tokio::test]
    async fn r4_06_revenue_closing_to_current_profit() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        open_year(2022).await;
        let end = NaiveDate::from_ymd_opt(2021, 12, 31).expect("日期");
        let revenue_base = balance_of("5001", end).await;
        post_at(
            NaiveDate::from_ymd_opt(2021, 3, 20).expect("日期"),
            "R4-REV-1",
            &[("1002", "1000", "0"), ("5001", "0", "1000")],
        )
        .await;
        let revenue_before = balance_of("5001", end).await;
        let profit_before = balance_of(ACCOUNT_CURRENT_PROFIT, end).await;
        let pl_before = pl_total(end).await;
        assert_eq!(
            revenue_before - revenue_base,
            dec("-1000"),
            "收入类贷方余额增加 1000（增量口径）"
        );
        close_all_months(2021).await;

        let result = close_year(2021, Some("2026 年度结账（夹具）"), operator())
            .await
            .expect("年结成功");

        let entries = carry_entries(2021).await;
        assert_eq!(
            side_of(&entries, "5001", "debit"),
            -revenue_before,
            "借 5001 把它的贷方余额结平"
        );
        assert_eq!(
            side_of(&entries, "5001", "credit"),
            Decimal::ZERO,
            "收入类不该再出现贷方结转行"
        );
        assert!(
            side_of(&entries, ACCOUNT_CURRENT_PROFIT, "credit") >= dec("1000"),
            "贷 3103 至少 1000（本年收入）"
        );
        assert_eq!(
            balance_of("5001", end).await,
            Decimal::ZERO,
            "结转后收入类归零"
        );
        // 净利润 = 3103 结转后的余额（= 结转前 3103 + 本年损益净额），也就是搬进 3104 的金额。
        // 用「结转前实测」而不是硬编码 1000：库里可能带着别的模块夹具留下的 3103 余额
        // （R5 的库存期初就贷过 3103），硬编码会随库污染假红。
        assert_eq!(
            result.net_profit,
            -(profit_before + pl_before),
            "净利润 = 结转进 3104 的金额"
        );
        assert_eq!(
            balance_of(ACCOUNT_CURRENT_PROFIT, end).await,
            Decimal::ZERO,
            "本年利润结转后必须归零"
        );
    }

    /// ⑦ 费用类结转到本年利润（借 3103 / 贷费用）。
    ///
    /// 夹具记的是**末级**科目 `5602.01 管理费用-折旧费`：FIN-1（V0226）把 `5602` 翻成
    /// 「仅汇总」并挂了 8 个末级子科目之后，`5602` 本级已经不能记账（`voucher::post_voucher`
    /// 直接 400），而年结只结末级 —— 拿父科目当夹具就是本卡要修的那个 bug 本身。
    #[tokio::test]
    async fn r4_07_expense_closing_to_current_profit() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        open_year(2022).await;
        let end = NaiveDate::from_ymd_opt(2021, 12, 31).expect("日期");
        let expense_base = balance_of(EXPENSE_ACCOUNT, end).await;
        post_at(
            NaiveDate::from_ymd_opt(2021, 4, 8).expect("日期"),
            "R4-EXP-1",
            &[(EXPENSE_ACCOUNT, "400", "0"), ("1002", "0", "400")],
        )
        .await;
        let expense_before = balance_of(EXPENSE_ACCOUNT, end).await;
        assert_eq!(
            expense_before - expense_base,
            dec("400"),
            "费用类借方余额增加 400（增量口径）"
        );
        close_all_months(2021).await;

        close_year(2021, None, operator()).await.expect("年结成功");

        let entries = carry_entries(2021).await;
        assert_eq!(
            side_of(&entries, EXPENSE_ACCOUNT, "credit"),
            expense_before,
            "贷 5602.01 把它的借方余额结平"
        );
        assert_eq!(side_of(&entries, EXPENSE_ACCOUNT, "debit"), Decimal::ZERO);
        assert!(
            side_of(&entries, ACCOUNT_CURRENT_PROFIT, "debit") >= dec("400"),
            "借 3103 至少 400（本年费用）"
        );
        assert_eq!(
            balance_of(EXPENSE_ACCOUNT, end).await,
            Decimal::ZERO,
            "结转后费用类归零"
        );
    }

    /// ⑧ 本年利润 → 利润分配（3103 归零，净利润落进 3104）。
    #[tokio::test]
    async fn r4_08_profit_transferred_to_distribution() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        open_year(2022).await;
        post_at(
            NaiveDate::from_ymd_opt(2021, 6, 6).expect("日期"),
            "R4-PROFIT-1",
            &[("1002", "1000", "0"), ("5001", "0", "1000")],
        )
        .await;
        post_at(
            NaiveDate::from_ymd_opt(2021, 6, 7).expect("日期"),
            "R4-PROFIT-2",
            &[(EXPENSE_ACCOUNT, "400", "0"), ("1002", "0", "400")],
        )
        .await;
        let end = NaiveDate::from_ymd_opt(2021, 12, 31).expect("日期");
        let pl_before = pl_total(end).await;
        let distribution_before = balance_of(ACCOUNT_PROFIT_DISTRIBUTION, end).await;
        close_all_months(2021).await;

        let result = close_year(2021, None, operator()).await.expect("年结成功");

        // 本年净利润 = −损益合计（本卡增量：收入 1000 − 费用 400 = 600；
        // 绝对值里还可能有别的模块夹具留下的损益余量，所以只断言「与 3103 的口径一致」）
        assert_eq!(
            result.net_profit, -pl_before,
            "响应里的净利润口径与损益合计一致"
        );
        let entries = carry_entries(2021).await;
        assert_eq!(
            side_of(&entries, ACCOUNT_PROFIT_DISTRIBUTION, "credit")
                - side_of(&entries, ACCOUNT_PROFIT_DISTRIBUTION, "debit"),
            -pl_before,
            "3104 收到本年利润（净利润）"
        );
        assert_eq!(
            balance_of(ACCOUNT_PROFIT_DISTRIBUTION, end).await - distribution_before,
            pl_before,
            "利润分配余额的变动 = 损益净额（贷方为正 → 余额口径为负，所以是 pl_before）"
        );
        assert_eq!(
            balance_of(ACCOUNT_CURRENT_PROFIT, end).await,
            Decimal::ZERO,
            "本年利润结转后必须归零"
        );
    }

    /// ⑨ 下年期初 = 本年期末（资产 / 负债 / 权益有值，损益类 = 0）。
    #[tokio::test]
    async fn r4_09_next_year_opening_equals_year_end() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        open_year(2022).await;
        post_at(
            NaiveDate::from_ymd_opt(2021, 2, 2).expect("日期"),
            "R4-OPEN-1",
            &[("1002", "1000", "0"), ("5001", "0", "1000")],
        )
        .await;
        post_at(
            NaiveDate::from_ymd_opt(2021, 2, 3).expect("日期"),
            "R4-OPEN-2",
            &[(EXPENSE_ACCOUNT, "150", "0"), ("2202", "0", "150")],
        )
        .await;
        let end = NaiveDate::from_ymd_opt(2021, 12, 31).expect("日期");
        let cash_before = balance_of("1002", end).await;
        close_all_months(2021).await;

        let result = close_year(2021, None, operator()).await.expect("年结成功");
        assert!(result.opening_balance_created, "下年期初必须生成");
        assert!(result.opening_rows > 0);

        // ① 银行存款：期末 = 期初，且来源标记为 YEAR_END
        let (debit, credit, source) = opening_row("2022-01", "1002")
            .await
            .expect("银行存款必须带进下年期初");
        assert_eq!(
            debit - credit,
            cash_before,
            "2027 期初 = 2026 期末（银行存款）"
        );
        assert_eq!(source, opening::SOURCE_YEAR_END, "来源必须是年结");

        // ② 负债类：贷方
        let (liability_debit, liability_credit, _) = opening_row("2022-01", "2202")
            .await
            .expect("应付账款必须带进下年期初");
        assert_eq!(liability_debit, Decimal::ZERO, "负债类只能单边（贷方）");
        assert!(
            liability_credit >= dec("150"),
            "应付账款贷方 {liability_credit}"
        );

        // ③ 损益类：一行都不许有（归零）
        let pl_rows: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_opening_balance b JOIN biz_account a ON a.id = b.account_id \
                  WHERE b.period_code = '2022-01' AND a.account_category = 'PROFIT_LOSS'",
                vec![],
            )
            .await
            .expect("统计损益类期初");
        assert_eq!(pl_rows, 0, "损益类科目在下年期初里必须是 0（不写行）");

        // ④ 本年利润归零 → 也不写行；利润分配有值
        assert!(
            opening_row("2022-01", ACCOUNT_CURRENT_PROFIT)
                .await
                .is_none(),
            "3103 归零"
        );
        assert!(
            opening_row("2022-01", ACCOUNT_PROFIT_DISTRIBUTION)
                .await
                .is_some(),
            "3104 有余额"
        );

        // ⑤ 借贷合计必须相等（否则报表直接拒绝出表）
        let (debit_total, credit_total): (Decimal, Decimal) = {
            let text: String = db()
                .scalar_with_params(
                    "SELECT COALESCE(SUM(debit_opening), 0)::text FROM biz_opening_balance \
                      WHERE period_code = '2022-01'",
                    vec![],
                )
                .await
                .expect("期初借方合计");
            let text2: String = db()
                .scalar_with_params(
                    "SELECT COALESCE(SUM(credit_opening), 0)::text FROM biz_opening_balance \
                      WHERE period_code = '2022-01'",
                    vec![],
                )
                .await
                .expect("期初贷方合计");
            (
                Decimal::from_str(&text).unwrap_or_default(),
                Decimal::from_str(&text2).unwrap_or_default(),
            )
        };
        assert_eq!(debit_total, credit_total, "下年期初借贷必须相等");

        // ⑥ 报表侧「2027 的期初」= 历史累计（不生成 OPENING 凭证，避免双计）
        let opening_voucher: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_voucher WHERE voucher_type = 'OPENING' \
                  AND period = '2022-01' AND deleted_at IS NULL",
                vec![],
            )
            .await
            .expect("统计期初凭证");
        assert_eq!(
            opening_voucher, 0,
            "年结不生成 2027-01 的期初凭证（会双计）"
        );
    }

    /// 记账入口拒绝**非末级**科目（记账科目口径 · 见 `voucher` 模块文档）。
    ///
    /// 这是 r4_07/08/09 三条红的根因闸：`5602` 自 V0226 起是「仅汇总」科目，
    /// 以前可以直接往它记账 ⇒ 年结只结末级 ⇒ 余额被静默漏掉。
    #[tokio::test]
    async fn r4_16_posting_to_non_leaf_account_is_rejected() {
        let Some(_guard) = setup().await else {
            return;
        };
        // 前提：库里的 5602 确实是「仅汇总」（老库可能还是末级 → 本用例不成立，跳过）
        let leaf: i16 = db()
            .scalar_with_params(
                "SELECT is_leaf FROM biz_account WHERE account_code = '5602' AND deleted_at IS NULL",
                vec![],
            )
            .await
            .expect("读 5602 的 is_leaf");
        if leaf == 1 {
            println!("SKIP: 本库的 5602 仍是末级科目（未应用 V0226），本用例不成立");
            return;
        }
        open_year(2021).await;

        let tx = db().begin().await.expect("开启凭证事务");
        let parent = voucher::account_required(&tx, "5602", "R4 夹具")
            .await
            .expect("取 5602 科目");
        let bank = voucher::account_required(&tx, "1002", "R4 夹具")
            .await
            .expect("取 1002 科目");
        let error = voucher::post_voucher(
            &tx,
            &PostVoucher {
                voucher_type: voucher::VOUCHER_TYPE_MANUAL,
                source_type: voucher::SOURCE_MANUAL,
                source_id: None,
                source_no: Some("R4-NONLEAF-1"),
                voucher_date: NaiveDate::from_ymd_opt(2021, 5, 5).expect("日期"),
                summary: "R4 父科目记账（应被拒）".to_string(),
                lines: vec![
                    EntryLine {
                        line_no: 1,
                        summary: "借 5602 本级".to_string(),
                        account: parent,
                        debit: dec("400"),
                        credit: Decimal::ZERO,
                        aux_supplier_id: None,
                        aux_material_id: None,
                        aux_dept_id: None,
                        aux_warehouse_id: None,
                    },
                    EntryLine {
                        line_no: 2,
                        summary: "贷 1002".to_string(),
                        account: bank,
                        debit: Decimal::ZERO,
                        credit: dec("400"),
                        aux_supplier_id: None,
                        aux_material_id: None,
                        aux_dept_id: None,
                        aux_warehouse_id: None,
                    },
                ],
                remark: Some("R4 夹具"),
                red_of: None,
                status: crate::business::erp::voucher::STATUS_POSTED,
            },
            Some(uid()),
            Utc::now(),
        )
        .await
        .expect_err("往非末级科目 5602 记账必须被拒绝");
        let _ = tx.rollback().await;
        let text = format!("{error}");
        assert!(
            text.contains("5602") && text.contains("非末级"),
            "错误消息必须点名科目与非末级原因：{text}"
        );
        // 拒绝之后不能留下凭证
        let count: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_voucher WHERE source_no = 'R4-NONLEAF-1'",
                vec![],
            )
            .await
            .expect("统计被拒凭证");
        assert_eq!(count, 0, "被拒的凭证不许落库");
    }

    /// 历史脏数据兜底：非末级科目上还有余额时，年结**拒绝**而不是静默漏结。
    ///
    /// 模拟 FIN-1 之前记在 `5602` 本级的历史分录（裸 SQL 绕过记账入口），验证
    /// `ensure_no_non_leaf_balance` 的 fail-closed 行为：409 + 点名科目 + 不落结转凭证。
    #[tokio::test]
    async fn r4_17_close_year_refuses_non_leaf_balance() {
        let Some(_guard) = setup().await else {
            return;
        };
        let leaf: i16 = db()
            .scalar_with_params(
                "SELECT is_leaf FROM biz_account WHERE account_code = '5602' AND deleted_at IS NULL",
                vec![],
            )
            .await
            .expect("读 5602 的 is_leaf");
        if leaf == 1 {
            println!("SKIP: 本库的 5602 仍是末级科目（未应用 V0226），本用例不成立");
            return;
        }
        open_year(2021).await;
        open_year(2022).await;
        close_all_months(2021).await;
        // 历史脏数据：借 5602 100 / 贷 1002 100（裸 SQL，绕开记账入口的末级校验）
        sql(
            "WITH v AS ( \
                INSERT INTO biz_voucher (voucher_word, voucher_no, period, voucher_date, summary, \
                                         total_debit, total_credit, entry_count, status) \
                VALUES ('记', 'R4-NONLEAF-2', '2021-05', DATE '2021-05-05', 'R4 历史父科目分录', \
                        100, 100, 2, 'POSTED') RETURNING id \
             ) \
             INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, \
                                            account_name, debit_amount, credit_amount) \
             SELECT v.id, 1, '借 5602 本级', a.id, a.account_code, a.account_name, 100, 0 \
               FROM v, biz_account a WHERE a.account_code = '5602' AND a.deleted_at IS NULL \
             UNION ALL \
             SELECT v.id, 2, '贷 1002', a.id, a.account_code, a.account_name, 0, 100 \
               FROM v, biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL",
        )
        .await;

        let error = close_year(2021, None, operator())
            .await
            .expect_err("非末级科目带余额时年结必须被拦住");
        assert_eq!(
            error.status, 409,
            "数据状态不允许 → HTTP 409：{} {}",
            error.status, error.msg
        );
        assert_eq!(
            error.code,
            error_code::RESOURCE_CONFLICT,
            "业务码必须是 RESOURCE_CONFLICT：{}",
            error.code
        );
        assert!(
            error.msg.contains("5602") && error.msg.contains("非末级"),
            "错误消息必须点名科目与非末级原因：{}",
            error.msg
        );
        // 拦住之后不许留下结转凭证 / 下年期初
        assert_eq!(
            carry_voucher_count(2021).await,
            0,
            "被拒的年结不许落结转凭证"
        );
        let opening: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_opening_balance WHERE period_code = '2022-01'",
                vec![],
            )
            .await
            .expect("统计下年期初");
        assert_eq!(opening, 0, "被拒的年结不许写下年期初");
    }

    /// ⑩ 12 月保持 CLOSED，结转凭证落在 12 月（已结账期间）。
    #[tokio::test]
    async fn r4_10_december_stays_closed() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        open_year(2022).await;
        post_at(
            NaiveDate::from_ymd_opt(2021, 12, 3).expect("日期"),
            "R4-DEC-1",
            &[("1002", "800", "0"), ("5001", "0", "800")],
        )
        .await;
        close_all_months(2021).await;
        assert_eq!(period_status("2021-12").await.as_deref(), Some("CLOSED"));

        let result = close_year(2021, Some("年末结转（夹具）"), operator())
            .await
            .expect("年结成功");

        assert_eq!(result.periods_closed, 12);
        assert_eq!(
            period_status("2021-12").await.as_deref(),
            Some("CLOSED"),
            "12 月结转后仍然是已结账（不是被解封）"
        );
        let row = db()
            .find_with_params(
                "SELECT period, voucher_date::text AS d, voucher_type FROM biz_voucher \
                  WHERE voucher_type = 'YEAR_END' AND deleted_at IS NULL AND period = '2021-12'",
                vec![],
            )
            .await
            .expect("读结转凭证");
        assert_eq!(row.len(), 1, "正好一张结转凭证");
        assert_eq!(
            row[0].get_typed::<String>("d").expect("凭证日期"),
            "2021-12-31"
        );
        assert_eq!(result.state.status, STATUS_CLOSED, "年度状态已结转");
        assert!(result.carry_forward_voucher_no.is_some());
    }

    // ==================== ③ 反年结 ====================

    /// ⑪ 反年结 → 删凭证、删期初、12 月解封。
    #[tokio::test]
    async fn r4_11_reopen_rolls_everything_back() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        open_year(2022).await;
        post_at(
            NaiveDate::from_ymd_opt(2021, 7, 7).expect("日期"),
            "R4-REOPEN-1",
            &[("1002", "600", "0"), ("5001", "0", "600")],
        )
        .await;
        close_all_months(2021).await;
        let closed = close_year(2021, None, operator()).await.expect("年结成功");
        assert!(closed.carry_forward_voucher_no.is_some());
        assert!(opening_count("2022-01").await > 0);

        let reopened = reopen_year(2021, Some("测试反年结：这批数据要重做一遍"), operator())
            .await
            .expect("反年结成功");

        assert!(reopened.reopened);
        assert_eq!(reopened.deleted_voucher_no, closed.carry_forward_voucher_no);
        assert_eq!(carry_voucher_count(2021).await, 0, "结转凭证必须删掉");
        assert_eq!(opening_count("2022-01").await, 0, "下年期初必须删掉");
        assert_eq!(
            period_status("2021-12").await.as_deref(),
            Some("OPEN"),
            "12 月必须解封"
        );
        assert!(reopened.december_reopened);
        assert!(!reopened.state.closed, "年度回到未结转");
        assert_eq!(reopened.state.status, STATUS_NOT_CLOSED);
    }

    /// ⑫ 下一年度已有凭证时反年结 → 409。
    #[tokio::test]
    async fn r4_12_reopen_blocked_by_next_year_vouchers() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        open_year(2022).await;
        post_at(
            NaiveDate::from_ymd_opt(2021, 8, 8).expect("日期"),
            "R4-BLOCK-1",
            &[("1002", "500", "0"), ("5001", "0", "500")],
        )
        .await;
        close_all_months(2021).await;
        close_year(2021, None, operator()).await.expect("年结成功");

        // 2027 年已经用起来了
        post_at(
            NaiveDate::from_ymd_opt(2022, 1, 5).expect("日期"),
            "R4-NEXT-1",
            &[("1002", "50", "0"), ("5001", "0", "50")],
        )
        .await;

        let error = reopen_year(2021, Some("试图拆掉已经用起来的年度"), operator())
            .await
            .expect_err("下一年度有凭证时不许反年结");
        assert_eq!(error.status, 409);
        assert!(
            error.msg.contains("R4-NEXT-1") || error.msg.contains("已经有 1 张凭证"),
            "409 文案要点名下一年度的凭证：{}",
            error.msg
        );
        assert_eq!(
            carry_voucher_count(2021).await,
            1,
            "被拦下时结转凭证必须还在"
        );
    }

    /// ⑬ 反年结不填原因 / 原因过短 → 400。
    #[tokio::test]
    async fn r4_13_reopen_requires_reason() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        open_year(2022).await;
        close_all_months(2021).await;
        close_year(2021, None, operator()).await.expect("年结成功");

        let empty = reopen_year(2021, None, operator())
            .await
            .expect_err("不填原因必须 400");
        assert_eq!(empty.status, 400, "{}", empty.msg);
        assert!(empty.msg.contains("必须填写原因"), "{}", empty.msg);

        let short = reopen_year(2021, Some("改一下"), operator())
            .await
            .expect_err("原因过短必须 400");
        assert_eq!(short.status, 400);
        assert!(short.msg.contains("至少 10 个字"), "{}", short.msg);

        assert!(
            load_state_in(db(), 2021).await.expect("读状态").closed,
            "状态没被改动"
        );
    }

    // ==================== ④ 边界 ====================

    /// ⑭ 空年度（全年无凭证）→ 年结成功，结转凭证跳过。
    ///
    /// 前提：先结掉 2021（把历史损益全部结平、3103 归零），2022 一整年不记账，
    /// 于是 2022 的损益余额 = 0、3103 = 0 → 没有可结转的分录 → 不出凭证，
    /// 但年度状态照样是「已结转」，下年期初照样生成。
    #[tokio::test]
    async fn r4_14_empty_year_skips_voucher() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_year(2021).await;
        open_year(2022).await;
        open_year(2023).await;
        post_at(
            NaiveDate::from_ymd_opt(2021, 9, 9).expect("日期"),
            "R4-EMPTY-1",
            &[("1002", "300", "0"), ("5001", "0", "300")],
        )
        .await;
        close_all_months(2021).await;
        close_year(2021, None, operator())
            .await
            .expect("2021 年结成功");
        assert_eq!(
            balance_of(
                ACCOUNT_CURRENT_PROFIT,
                NaiveDate::from_ymd_opt(2022, 12, 31).expect("日期")
            )
            .await,
            Decimal::ZERO,
            "2021 结转后 3103 归零"
        );

        // 2022 一整年没有任何凭证
        close_all_months(2022).await;
        let result = close_year(2022, Some("空年度（夹具）"), operator())
            .await
            .expect("空年度也要能年结");

        assert_eq!(
            result.carry_forward_voucher_no, None,
            "没有损益可结 → 跳过凭证"
        );
        assert_eq!(result.entry_count, 0);
        assert_eq!(carry_voucher_count(2022).await, 0);
        assert!(result.state.closed, "空年度也是「已结转」");
        assert!(result.opening_balance_created, "下年期初照样生成");
        assert!(opening_count("2023-01").await > 0);
    }

    /// ⑮ 纯函数：亏损方向（本年利润出现借方余额 → 贷 3103 / 借 3104）。
    #[test]
    fn r4_15_loss_direction_in_closing_lines() {
        let account = |code: &str, name: &str| AccountRef {
            id: Uuid::now_v7(),
            code: code.to_string(),
            name: name.to_string(),
        };
        let current_profit = account(ACCOUNT_CURRENT_PROFIT, "本年利润");
        let distribution = account(ACCOUNT_PROFIT_DISTRIBUTION, "利润分配");
        // 收入 100、费用 500 → 亏损 400（余额口径：收入 −100、费用 +500）
        let pl = vec![
            (account("5001", "主营业务收入"), dec("-100")),
            (account("5602", "管理费用"), dec("500")),
        ];
        let lines = build_closing_lines(&pl, &current_profit, &distribution, Decimal::ZERO);
        let (debit, credit) = voucher::totals_of(&lines);
        assert_eq!(debit, credit, "亏损的结转分录也必须借贷平衡");

        let cp_debit: Decimal = lines
            .iter()
            .filter(|item| item.account.code == ACCOUNT_CURRENT_PROFIT)
            .map(|item| item.debit)
            .sum();
        let cp_credit: Decimal = lines
            .iter()
            .filter(|item| item.account.code == ACCOUNT_CURRENT_PROFIT)
            .map(|item| item.credit)
            .sum();
        assert_eq!(cp_debit, dec("500"), "借 3103 = 费用合计");
        assert_eq!(
            cp_credit,
            dec("100") + dec("400"),
            "贷 3103 = 收入 + 亏损转出"
        );

        let dist_debit: Decimal = lines
            .iter()
            .filter(|item| item.account.code == ACCOUNT_PROFIT_DISTRIBUTION)
            .map(|item| item.debit)
            .sum();
        assert_eq!(dist_debit, dec("400"), "亏损时 借 3104 400");

        // 空年度：什么都没有 → 空分录（调用方据此跳过凭证）
        assert!(
            build_closing_lines(&[], &current_profit, &distribution, Decimal::ZERO).is_empty(),
            "空年度不该产出分录"
        );
    }
}

#[cfg(test)]
#[path = "year_end_items_tests.rs"]
mod item_tests;
