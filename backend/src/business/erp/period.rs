//! 会计期间生命周期（ERP-P1）：**开账 / 结账 / 反结账**。
//!
//! # 为什么单独一个模块
//!
//! W1c 只给了 `biz_accounting_period` 的两个**只读**接口（`/period/page`、`/period/{id}`），
//! 前端写着「只读：开账 / 结账由财务主管操作」，但**没有那个入口** —— 老板问
//! 「会计期间为什么不能添加新的期间」，答案就是「能力从来没做」。
//! 本模块补的正是执行者要用的那三个动作 + 一个结账预检。
//!
//! 读接口仍在 [`crate::business::erp::handler`]（`period_page` / `period_detail`），
//! 本模块只负责写；表结构以 `V0137` 为准（`status / closed_at / closed_by` 三列够用，
//! **不新增列、不建新表**）。
//!
//! # 接口（前缀 `/api` 由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/period` | `business:erp:period:open` | 开账（建期间，或把 `NOT_OPENED` 置为 `OPEN`） |
//! | POST | `/business/erp/period/batch-open` | `business:erp:period:open` | 批量开账（一次开一整年） |
//! | GET | `/business/erp/period/{id}/close-check` | `business:erp:period:close` | 结账预检（只读） |
//! | POST | `/business/erp/period/{id}/close` | `business:erp:period:close` | 结账 |
//! | POST | `/business/erp/period/{id}/reopen` | `business:erp:period:reopen` | 反结账（必填原因 ≥ 10 字） |
//! | POST | `/business/erp/period/{id}/remind` | `business:erp:period:remind` | 催办（把挡路单据的待办发给处理人，见 [`crate::business::erp::period_remind`]） |
//!
//! # 结账的四项前置校验（卡面硬红线 2）
//!
//! ```text
//! ① 期间内没有 DRAFT 状态的凭证            → 409「还有 N 张草稿凭证未过账」
//! ② 期间内单据全部终态                    → 409「还有 N 张单据未完成（采购单 5 / 收货单 2 …）」
//! ③ 试算平衡（分录 Σ借 = Σ贷）             → 409「借贷不平，差 X 元」
//! ④ 期间内不变量（erp_invariants A 组）     → 409「不变量未通过：A1 有 2 张凭证分录取和不平 …」
//! 每一条 blocker 都带明细（`items`，最多 50 条）：单号 / 类型 / 金额 / 制单人 /
//! 该谁处理 / 已等多久 / 「去处理」跳转。明细的取数口径与判定口径同源，
//! 实现见 [`crate::business::erp::period_items`]（**改判定必须两处同步**）。
//! ```
//!
//! **全库口径的账实 / 账账巡检（B4 流水累计、C1/C2 负库存）不拦结账**，
//! 只在 `close-check` 的 `warnings` 里提示 —— 理由是它们不按期间切，
//! 任何一处库存漂移都会封死**所有**期间的结账（含过账事务进行中的瞬时中间态），
//! 详见 [`global_health_warnings`] 的推导。整本账的巡检仍由
//! `scripts/verify/erp_invariants.py` 每天跑，C1/C2 在那里是 BLOCKER 级。
//!
//! ## 为什么试算平衡要拿**分录**求和，不看单头
//!
//! `ck_biz_voucher_balance` 已经在库层强制 `total_debit = total_credit`，
//! 看单头永远「平」；但分录侧没有「Σ分录 = 单头」的约束（A2 不变量就是查这个）。
//! 试算平衡若只看单头，等于把一个恒真的条件当校验。
//! 所以：**③ 用分录求和**（能抓到「分录被手工改过」的账），**④ 的 A1/A2 用逐张比对**。
//!
//! ## 校验与状态变更**同一个事务**（卡面 L11）
//!
//! 结账走 `BEGIN → SELECT … FOR UPDATE（锁期间行）→ 四项校验 → 状态机 → UPDATE … WHERE status='OPEN' → COMMIT`。
//! 校验读完到 UPDATE 之间别人插进来的草稿凭证，会被 `FOR UPDATE` 行锁 + CAS 拦在门外：
//! 要么它先提交、本次校验看得见并 409；要么它等锁、本次结账成功后它的记账请求被
//! [`ensure_period_writable`] 400 挡回。
//!
//! # 审计（卡面 L12）
//!
//! 开账 / 结账 / 反结账都写 `biz_status_log`（`entity_type = 'ACCOUNTING_PERIOD'`，
//! `entity_no = period_code`），走 [`state_machine`] 唯一入口：
//! `NULL → OPEN`（开账）/ `NOT_OPENED → OPEN` / `OPEN → CLOSED` / `CLOSED → OPEN`。
//! 反结账的原因落在 `reason` 列 —— 这正是「为什么把已经封住的月份又打开」的唯一证据。
//!
//! # 结账后的三道闸
//!
//! | 动作 | 结果 |
//! |------|------|
//! | 在该期间生成凭证（自动 / 手工 / 付款） | **400**「会计期间 X 已结账，不能再生成凭证」—— 闸在 [`voucher::post_voucher`]（所有凭证写入口的唯一收口） |
//! | 该期间的单据过账 | **400**（过账要生成凭证，撞上同一条闸，整笔事务回滚） |
//! | 红冲到已结账期间 | **409**（`voucher::reverse_posted` 的既有闸，比 400 更严格：先拦在红冲前置校验） |
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_accounting_period / biz_status_log / biz_voucher / biz_voucher_entry 是
//! 全局会计字典与台账（V0137 / V0149 / V0159 建表，无 dept_id 与归属人列），数据范围表达不出来；
//! 可见性与操作性由 business:erp:period:{open,close,reopen} 三个独立权限码控制。
//! ```

use axum::Json;
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};
use std::result::Result;
use std::str::FromStr;

use crate::business::erp::dto::PeriodRes;
use crate::business::erp::model::{self, PeriodRow};
use crate::business::erp::period_items::{self, BlockingItem, OwnerResolver};
use crate::business::erp::voucher;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 口径常量 ====================

/// 开账权限码（建期间 / 批量开账）。
pub const PERM_OPEN: &str = "business:erp:period:open";
/// 结账权限码（含 `close-check` 预检）。
pub const PERM_CLOSE: &str = "business:erp:period:close";
/// 反结账权限码（高危：撤销已封账的月份）。
pub const PERM_REOPEN: &str = "business:erp:period:reopen";

/// 四个权限码清单（`open/close/reopen` 与 `V0188__erp_period_close.up.sql` 逐字一致，
/// `remind` 见 `V0207__erp_period_remind.up.sql`）。
///
/// 为什么三个动作各自一个码（卡面硬红线 4）：能看期间列表 ≠ 能封账 ≠ 能撤封账。
/// 只读的会计要 `period:list`（列在 V0141），不该顺手拿到封账能力；
/// 反结账只给财务总监 / 超管，与「结账」再分一层。
/// **催办另立一个码**：能给处理人发催办 ≠ 有资格替他把单过账。
pub const PERIOD_PERMISSIONS: [(&str, &str); 4] = [
    (PERM_OPEN, "开账（建期间 / 批量开账）"),
    (PERM_CLOSE, "结账（含结账预检）"),
    (PERM_REOPEN, "反结账（撤销已封账的期间）"),
    (
        crate::business::erp::period_remind::PERM_REMIND,
        "催办（把结账待办发给处理人）",
    ),
];

/// 期间状态（逐字抄 `ck_biz_period_status`）。
pub const STATUS_NOT_OPENED: &str = "NOT_OPENED";
/// 已开账。
pub const STATUS_OPEN: &str = "OPEN";
/// 已结账。
pub const STATUS_CLOSED: &str = "CLOSED";

/// 反结账原因的最小字数（卡面 §一 ③：≥ 10 字）。
///
/// 为什么不是「非空」：反结账会让人在已经封住的月份里改账，
/// 「改一下」「补一下」这种两个字的原因等于没写，审计时查不出动机。
pub const MIN_REOPEN_REASON_CHARS: usize = 10;

/// 结账备注的最大字数（`reason` 落 `remark` 列，TEXT 但别让它变成小说）。
pub const MAX_CLOSE_REASON_CHARS: usize = 200;

/// 批量开账一次最多几个月（10 年）。
///
/// 上限不是业务限制，是**误操作护栏**：`from: 2000-01, to: 2099-12` = 1200 条期间行，
/// 一次误填就把期间列表灌满，且期间行不可删除（`soft_delete = false`）。
pub const MAX_BATCH_MONTHS: i64 = 120;

/// 结账阻塞项类型（前端按它决定图标 / 跳转）。
pub const BLOCKER_DRAFT_VOUCHER: &str = "DRAFT_VOUCHER";
/// 有未完成单据。
pub const BLOCKER_DOC_UNFINISHED: &str = "DOC_UNFINISHED";
/// 借贷不平。
pub const BLOCKER_UNBALANCED: &str = "UNBALANCED";
/// 不变量未通过。
pub const BLOCKER_INVARIANT: &str = "INVARIANT";
/// 期间自身状态不允许这次操作。
pub const BLOCKER_PERIOD_STATUS: &str = "PERIOD_STATUS";

/// 试算平衡的容差（与 `scripts/verify/erp_invariants.py` 的 `TOL` 同为 0.0001）。
///
/// 写成函数而不是 `const`：这个版本的 `Decimal::new` 不是 `const fn`。
fn balance_tolerance() -> Decimal {
    Decimal::new(1, 4)
}

// ==================== 入参 / 出参 ====================

/// 开账入参。
#[derive(Debug, Clone, Deserialize)]
pub struct OpenPeriodReq {
    /// 会计年度（如 2026）。
    pub period_year: i16,
    /// 会计月份（1–12）。
    pub period_month: i16,
    /// 备注（可选）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 批量开账入参（`from` / `to` 均为 `YYYY-MM`，含两端）。
#[derive(Debug, Clone, Deserialize)]
pub struct BatchOpenReq {
    /// 起始期间，如 `2026-11`。
    pub from: String,
    /// 结束期间，如 `2027-12`。
    pub to: String,
    /// 备注（写到每个新建期间上，可选）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 结账入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClosePeriodReq {
    /// 结账备注（可选；落 `remark`，也进状态日志）。
    #[serde(default)]
    pub reason: Option<String>,
}

/// 反结账入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReopenPeriodReq {
    /// 反结账原因（**必填，≥ 10 字**）。
    #[serde(default)]
    pub reason: Option<String>,
}

/// 一个阻塞项。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Blocker {
    /// 类型（`DRAFT_VOUCHER` / `DOC_UNFINISHED` / `UNBALANCED` / `INVARIANT` / `PERIOD_STATUS`）。
    #[serde(rename = "type")]
    pub kind: String,
    /// 条数（凭证数 / 单据数 / 不变量条数）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    /// 差额（仅 `UNBALANCED`，定点字符串，避免 JSON 浮点丢精度）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// 给人看的一句话（**点名数量 / 差额**，卡面 O5）。
    pub message: String,
    /// 去处理的页面（前端直接跳）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// 定位线索（凭证号 / 单号，最多 3 个）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub samples: Vec<String>,
    /// ★ 挡路单据明细（PERIOD-CLOSE，最多 [`period_items::ITEM_LIMIT`] 条）。
    ///
    /// 空数组 = 这一类没有「单据级」明细（例如期间自身状态不对），
    /// 前端此时退回「一句话 + 去看看」的老样式（backward compat）。
    /// 超过上限时 `message` 里会写明「共 N 张，仅显示前 50 张」。
    #[serde(default)]
    pub items: Vec<BlockingItem>,
}

impl Blocker {
    /// 造一个带条数的阻塞项。
    fn with_count(kind: &str, count: i64, message: String, link: Option<String>) -> Self {
        Self {
            kind: kind.to_string(),
            count: Some(count),
            diff: None,
            message,
            link,
            samples: Vec::new(),
            items: Vec::new(),
        }
    }

    /// 追加定位线索。
    fn samples(mut self, samples: Vec<String>) -> Self {
        self.samples = samples;
        self
    }

    /// 挂上单据明细（PERIOD-CLOSE）。
    fn items(mut self, items: Vec<BlockingItem>) -> Self {
        self.items = items;
        self
    }
}

/// 超过明细上限时追加在 `message` 后面的那句话（前端据此提示「先去列表页批量处理」）。
fn limit_note(count: i64) -> String {
    if count > period_items::ITEM_LIMIT {
        format!(
            "（共 {count} 张，仅显示前 {} 张，请先去列表页批量处理）",
            period_items::ITEM_LIMIT
        )
    } else {
        String::new()
    }
}

/// 结账预检响应（`GET /business/erp/period/{id}/close-check`）。
#[derive(Debug, Clone, Serialize)]
pub struct CloseCheckRes {
    /// 能不能结账（= `blockers` 为空 且期间是 `OPEN`）。
    pub can_close: bool,
    /// 期间本身（前端弹窗标题用）。
    pub period: PeriodRes,
    /// 阻塞项（空 = 可以结账）。
    pub blockers: Vec<Blocker>,
    /// 全库巡检告警（B4 / C1 / C2）：**不拦结账**，前端灰字提示。
    ///
    /// 与 `blockers` 分开的理由写在 [`global_health_warnings`]：这些是「整本账」的
    /// 时点状态，混进阻断项会让任何一处库存漂移封死所有期间的结账。
    #[serde(default)]
    pub warnings: Vec<Blocker>,
}

/// 批量开账响应。
#[derive(Debug, Clone, Serialize)]
pub struct BatchOpenRes {
    /// 本次新建的期间。
    pub created: Vec<PeriodRes>,
    /// 已存在而跳过的期间编码（幂等：重复批量开账不会 409）。
    pub skipped: Vec<String>,
    /// 新建条数。
    pub created_count: i64,
    /// 跳过条数。
    pub skipped_count: i64,
}

// ==================== 错误收口 ====================

/// 400：入参不合法。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409：状态冲突（重复开账 / 已结账 / 有阻塞项）。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 403：没有对应权限码。
fn forbidden(code: &str, action: &str) -> ApiError {
    ApiError::new(
        403,
        error_code::PERMISSION_DENIED,
        format!(
            "没有{action}的权限（{code}）：开账 / 结账 / 反结账是财务主管的动作，\
             请让管理员在「系统管理 · 菜单权限」里授予该权限码"
        ),
    )
}

/// 404：期间不存在。
fn not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "会计期间不存在（期间行不可删除，请确认 id）".to_string(),
    )
}

/// 数据库错误 → 500（带上下文）。
fn db_error(error: DbError, ctx: &str) -> ApiError {
    utils::db_error(error, ctx)
}

// ==================== 权限闸 ====================

/// 行级权限闸（与 `#[nik::permission]` 路由注解同判据）。
///
/// 路由层的注解已经拦了一次，这里再判一次的理由：
/// **单测能直接调业务函数**（不提 HTTP），没有这一句就没法证明「无码 → 403」
/// 这件事在业务分支里也成立；另外前台按钮藏起来了，接口仍然必须自己守。
pub fn ensure_permission(claims: &TokenClaims, code: &str, action: &str) -> Result<(), ApiError> {
    if claims.has_permission(code) {
        return Ok(());
    }
    Err(forbidden(code, action))
}

// ==================== 纯逻辑（不连库，可单测） ====================

/// `(年, 月)` 的起止日（当月 1 日 ~ 当月最后一天，自动算闰年）。
pub fn month_range(year: i16, month: i16) -> Result<(NaiveDate, NaiveDate), ApiError> {
    if !(1..=12).contains(&month) {
        return Err(bad_request(format!(
            "period_month 只能是 1–12（当前 {month}）"
        )));
    }
    if !(2000..=2999).contains(&year) {
        return Err(bad_request(format!(
            "period_year 只能是 2000–2999（当前 {year}）：再早/再晚的期间是误填，\
             期间行建出来又删不掉"
        )));
    }
    let start = NaiveDate::from_ymd_opt(i32::from(year), u32::try_from(month).unwrap_or(1), 1)
        .ok_or_else(|| bad_request(format!("{year}-{month:02} 不是合法年月")))?;
    // 下月 1 日减一天 = 本月最后一天（闰年 2 月自动 29）
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let first_of_next = NaiveDate::from_ymd_opt(
        i32::from(next_year),
        u32::try_from(next_month).unwrap_or(1),
        1,
    )
    .ok_or_else(|| bad_request(format!("{year}-{month:02} 的下一个月不合法")))?;
    let end = first_of_next
        .pred_opt()
        .ok_or_else(|| bad_request("计算期间结束日失败"))?;
    Ok((start, end))
}

/// 期间编码 `YYYY-MM`（与 `VARCHAR(7)` 等长）。
pub fn period_code(year: i16, month: i16) -> String {
    format!("{year:04}-{month:02}")
}

/// 严格解析 `YYYY-MM`（前端下拉 / curl 手输都会走这里）。
///
/// 为什么不用 `NaiveDate` 宽松解析：`2026-1` / `2026-13` / `2026/11` 都必须 400，
/// 静默修正会让人以为开的是 1 月，实际开了别的期间。
pub fn parse_year_month(raw: &str) -> Result<(i16, i16), ApiError> {
    let text = raw.trim();
    let mut parts = text.split('-');
    let (Some(year), Some(month), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(bad_request(format!(
            "期间格式必须是 YYYY-MM（如 2026-11），当前是 `{text}`"
        )));
    };
    if year.len() != 4 || month.len() != 2 {
        return Err(bad_request(format!(
            "期间格式必须是 YYYY-MM（年 4 位、月 2 位，如 2026-11），当前是 `{text}`"
        )));
    }
    let year: i16 = year
        .parse()
        .map_err(|_| bad_request(format!("期间年份不是数字：`{text}`")))?;
    let month: i16 = month
        .parse()
        .map_err(|_| bad_request(format!("期间月份不是数字：`{text}`")))?;
    month_range(year, month)?;
    Ok((year, month))
}

/// 反结账原因校验：**必填且 ≥ 10 字**（卡面硬红线 3）。
pub fn require_reopen_reason(raw: Option<&str>) -> Result<String, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(bad_request(
            "反结账必须填写原因（reason）：撤销已封账的月份会让人在已结账期间改账，\
             原因要能让审计看懂「为什么」",
        ));
    }
    let chars = text.chars().count();
    if chars < MIN_REOPEN_REASON_CHARS {
        return Err(bad_request(format!(
            "反结账原因至少 {MIN_REOPEN_REASON_CHARS} 个字（当前 {chars} 字）：\
             「{text}」这种写法审计时看不出动机"
        )));
    }
    Ok(text.to_string())
}

/// 结账备注校验（可选，超长 400）。
pub fn optional_close_reason(raw: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let chars = text.chars().count();
    if chars > MAX_CLOSE_REASON_CHARS {
        return Err(bad_request(format!(
            "结账备注最多 {MAX_CLOSE_REASON_CHARS} 字（当前 {chars} 字）"
        )));
    }
    Ok(Some(text.to_string()))
}

/// 借贷不平 → 阻塞项（差额按定点打印，差 0.01 就写 0.01）。
pub fn unbalanced_blocker(debit: Decimal, credit: Decimal) -> Option<Blocker> {
    let diff = debit - credit;
    if diff.abs() <= balance_tolerance() {
        return None;
    }
    Some(Blocker {
        kind: BLOCKER_UNBALANCED.to_string(),
        count: None,
        diff: Some(diff.abs().round_dp(4).normalize().to_string()),
        message: format!(
            "借贷不平，差 {} 元（借方合计 {debit} / 贷方合计 {credit}）：\
             检查该期间凭证的分录，不平的账不能封",
            diff.abs().round_dp(4).normalize()
        ),
        link: Some("/business/erp/ledger".to_string()),
        samples: Vec::new(),
        items: Vec::new(),
    })
}

/// 草稿凭证 → 阻塞项（**文案点名张数**，卡面 O5；明细给到单号 / 金额 / 制单人 / 处理人）。
pub fn draft_voucher_blocker(
    count: i64,
    period_code: &str,
    items: Vec<BlockingItem>,
) -> Option<Blocker> {
    if count <= 0 {
        return None;
    }
    // 定位线索 = 明细里的前 3 个凭证号（老字段保留：前端/日志可能还在读）
    let samples = items
        .iter()
        .take(3)
        .map(|item| item.doc_no.clone())
        .collect::<Vec<_>>();
    Some(
        Blocker::with_count(
            BLOCKER_DRAFT_VOUCHER,
            count,
            format!(
                "还有 {count} 张草稿凭证未过账：结账前必须全部过账或作废{}",
                limit_note(count)
            ),
            Some(format!(
                "/business/erp/voucher?status=DRAFT&period={period_code}"
            )),
        )
        .samples(samples)
        .items(items),
    )
}

/// 未完成单据 → 阻塞项（按类型点名：采购单 5 张 / 收货单 2 张 …）。
pub fn unfinished_doc_blocker(counts: &[(&str, i64)], items: Vec<BlockingItem>) -> Option<Blocker> {
    let total: i64 = counts.iter().map(|(_, count)| *count).sum();
    if total <= 0 {
        return None;
    }
    let detail = counts
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(label, count)| format!("{label} {count} 张"))
        .collect::<Vec<_>>()
        .join(" / ");
    Some(Blocker::with_count(
        BLOCKER_DOC_UNFINISHED,
        total,
        format!(
            "还有 {total} 张单据未完成（{detail}）：结账前单据必须到终态（已过账 / 已作废 / 已完成）{}",
            limit_note(total)
        ),
        Some("/business/erp/stock-in".to_string()),
    )
    .items(items))
}

/// 不变量未通过 → 阻塞项（明细是失败的那几张凭证）。
pub fn invariant_blocker(failures: Vec<String>, items: Vec<BlockingItem>) -> Option<Blocker> {
    if failures.is_empty() {
        return None;
    }
    let count = i64::try_from(failures.len()).unwrap_or(i64::MAX);
    Some(
        Blocker::with_count(
            BLOCKER_INVARIANT,
            count,
            format!(
                "不变量未通过（{count} 项）：{}。账不平不能封，请先修复后再结账",
                failures.join("；")
            ),
            Some("/business/erp/inventory?audit=1".to_string()),
        )
        .items(items),
    )
}

// ==================== 期间行读写 ====================

/// 期间列清单来源（与 `model::PERIOD_COLUMNS` 共用，避免两处口径漂移）。
fn select_period_sql(where_clause: &str) -> String {
    format!(
        "SELECT {} FROM biz_accounting_period WHERE {where_clause}",
        model::PERIOD_COLUMNS
    )
}

/// 按编码读期间（走调用方的连接 / 事务）。
async fn find_by_code(ex: &impl SqlExecutor, code: &str) -> Result<Option<PeriodRow>, ApiError> {
    let rows = ex
        .query_sql(
            select_period_sql("period_code = ?"),
            vec![Value::Text(code.to_string())],
        )
        .await
        .map_err(|error| db_error(error, "查询会计期间"))?;
    rows.first()
        .map(PeriodRow::from_row)
        .transpose()
        .map_err(|error| db_error(error, "解析会计期间"))
}

/// 按主键读期间并**加行锁**（`SELECT … FOR UPDATE`）。
///
/// 结账 / 反结账 / 开账都靠这把锁把「校验 → 改状态」串起来：
/// 并发两次结账，第二次会等锁，拿到的是已 CLOSED 的行，直接 409。
async fn find_by_id_locked(ex: &impl SqlExecutor, id: Uuid) -> Result<Option<PeriodRow>, ApiError> {
    let rows = ex
        .query_sql(
            select_period_sql("id = ?::uuid FOR UPDATE"),
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| db_error(error, "锁定会计期间"))?;
    rows.first()
        .map(PeriodRow::from_row)
        .transpose()
        .map_err(|error| db_error(error, "解析会计期间"))
}

/// 与 `[start, end]` 重叠的**其它**期间（期间可能不是自然月，重叠就是配置错）。
async fn find_overlapping(
    ex: &impl SqlExecutor,
    start: NaiveDate,
    end: NaiveDate,
    exclude_code: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let rows = ex
        .query_sql(
            "SELECT period_code FROM biz_accounting_period \
              WHERE start_date <= ?::text::date AND end_date >= ?::text::date \
                AND period_code <> ? \
              ORDER BY period_code LIMIT 1"
                .to_string(),
            vec![
                Value::Text(end.to_string()),
                Value::Text(start.to_string()),
                Value::Text(exclude_code.unwrap_or_default().to_string()),
            ],
        )
        .await
        .map_err(|error| db_error(error, "查询重叠期间"))?;
    match rows.first() {
        Some(row) => Ok(Some(
            row.get_typed::<String>("period_code")
                .map_err(|error| db_error(error, "解析期间编码"))?,
        )),
        None => Ok(None),
    }
}

/// 插入一条期间行（`status = OPEN`）。
async fn insert_period(
    ex: &impl SqlExecutor,
    year: i16,
    month: i16,
    start: NaiveDate,
    end: NaiveDate,
    remark: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Uuid, ApiError> {
    let id = Uuid::now_v7();
    let code = period_code(year, month);
    ex.exec_sql(
        "INSERT INTO biz_accounting_period \
            (id, period_year, period_month, period_code, start_date, end_date, status, \
             remark, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?::text::date, ?::text::date, ?, NULLIF(?::text,''), ?, ?)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::I16(year),
            Value::I16(month),
            Value::Text(code),
            Value::Text(start.to_string()),
            Value::Text(end.to_string()),
            Value::Text(STATUS_OPEN.to_string()),
            Value::Text(remark.unwrap_or_default().to_string()),
            Value::from(now),
            Value::from(now),
        ],
    )
    .await
    .map_err(|error| match error {
        DbError::UniqueViolation { .. } => conflict(format!(
            "会计期间 {year}-{month:02} 已存在：期间编码 (period_code) 唯一，\
             同一个月份不能开两次账"
        )),
        other => db_error(other, "创建会计期间"),
    })?;
    Ok(id)
}

/// 期间状态迁移 + 备注（CAS：`WHERE status = 迁移前状态`）。
#[allow(clippy::too_many_arguments)]
async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    to: &str,
    closed_at: Option<DateTime<Utc>>,
    closed_by: Option<Uuid>,
    remark: Option<&str>,
    now: DateTime<Utc>,
) -> Result<u64, ApiError> {
    let affected = ex
        .exec_sql(
            // `closed_at` / `closed_by` 走 `?::text` 再转型：直接把 `Value::Text` 绑到
            // `?::timestamptz` 会让驱动按「参数类型 = timestamptz」编码文本 →
            // `error serializing parameter`（与本仓 DATE 列必须写 `?::text::date` 同理）。
            "UPDATE biz_accounting_period \
                SET status = ?, closed_at = NULLIF(?::text, '')::timestamptz, \
                    closed_by = NULLIF(?::text, '')::uuid, \
                    remark = COALESCE(NULLIF(?::text, ''), remark), updated_at = ? \
              WHERE id = ?::uuid AND status = ?"
                .to_string(),
            vec![
                Value::Text(to.to_string()),
                Value::Text(
                    closed_at
                        .map(|value| value.to_rfc3339())
                        .unwrap_or_default(),
                ),
                Value::Text(closed_by.map(|value| value.to_string()).unwrap_or_default()),
                Value::Text(remark.unwrap_or_default().to_string()),
                Value::from(now),
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        )
        .await
        .map_err(|error| db_error(error, "更新会计期间状态"))?;
    Ok(affected)
}

/// 读回一个期间行（按 id）；不存在 → `Ok(None)`。
///
/// 预检、催办、结账前读行都走它：三处各自写一遍 `select_period_sql` 是漂移之源。
pub(crate) async fn load_period_row(
    ex: &impl SqlExecutor,
    id: Uuid,
) -> Result<Option<PeriodRow>, ApiError> {
    let rows = ex
        .query_sql(select_period_sql("id = ?::uuid"), vec![Value::from(&id)])
        .await
        .map_err(|error| db_error(error, "查询会计期间"))?;
    match rows.into_iter().next() {
        Some(row) => {
            let row = PeriodRow::from_row(&row).map_err(|error| db_error(error, "解析会计期间"))?;
            Ok(Some(row))
        }
        None => Ok(None),
    }
}

/// 读回一个期间的出参形态。
async fn load_res(ex: &impl SqlExecutor, id: Uuid) -> Result<PeriodRes, ApiError> {
    let row = load_period_row(ex, id).await?.ok_or_else(not_found)?;
    Ok(PeriodRes::from(&row))
}

// ==================== ① 开账 ====================

/// 开账（建期间，或把 `NOT_OPENED` 的期间置为 `OPEN`）。
///
/// # 语义
///
/// * 期间不存在 → 新建（`start_date` = 当月 1 日、`end_date` = 当月最后一天，`status = OPEN`）；
/// * 期间已存在且 `NOT_OPENED` → 置为 `OPEN`（V0137 种子 / 手工插的行属于这种）；
/// * 期间已存在且 `OPEN` → **409**（重复开账）；
/// * 期间已存在且 `CLOSED` → **409**（已封账，要改先反结账）；
/// * 与任何已有期间的日期区间重叠 → **409**（期间不能交叉，否则凭证归期无解）。
///
/// 为什么允许「补历史期间」：卡面 §一 ① 明写不限制早于最大期间 —— 补 2020 年的账是合法业务。
pub async fn open_period(
    year: i16,
    month: i16,
    remark: Option<&str>,
    operator: state_machine::Operator,
) -> Result<PeriodRes, ApiError> {
    let (start, end) = month_range(year, month)?;
    let code = period_code(year, month);
    let remark = remark.map(str::trim).filter(|text| !text.is_empty());
    let now = Utc::now();

    let tx = db()
        .begin()
        .await
        .map_err(|error| db_error(error, "开启开账事务"))?;

    // 行锁：同一期间的并发开账只有一次能过（编码唯一索引只是最后兜底）
    let existing = find_by_code(&tx, &code).await?;
    let id = match existing {
        Some(row) => match row.status.as_str() {
            STATUS_OPEN => {
                return Err(conflict(format!(
                    "会计期间 {code} 已经开账（{} ~ {}），无需重复开账：\
                     需要新期间请选下一个月",
                    row.start_date, row.end_date
                )));
            }
            STATUS_CLOSED => {
                return Err(conflict(format!(
                    "会计期间 {code} 已结账，不能重复开账：如需在该月补账，\
                     请先「反结账」（需要财务总监权限 + 填写原因）"
                )));
            }
            _ => {
                state_machine::transition(
                    &tx,
                    state_machine::ENTITY_PERIOD,
                    row.id,
                    &code,
                    &state_machine::PERIOD_EDGES,
                    STATUS_NOT_OPENED,
                    STATUS_OPEN,
                    operator.clone(),
                    Some("开账"),
                    None,
                )
                .await?;
                let affected = apply_status(
                    &tx,
                    row.id,
                    STATUS_NOT_OPENED,
                    STATUS_OPEN,
                    None,
                    None,
                    remark,
                    now,
                )
                .await?;
                if affected == 0 {
                    return Err(conflict(format!(
                        "会计期间 {code} 的状态在本次开账期间已被改动，请刷新后重试"
                    )));
                }
                row.id
            }
        },
        None => {
            if let Some(other) = find_overlapping(&tx, start, end, Some(&code)).await? {
                return Err(conflict(format!(
                    "期间 {code} 与已有期间 {other} 的日期区间重叠（{start} ~ {end}）：\
                     两个月不能覆盖同一天，请先核对已有期间的起止日"
                )));
            }
            let id = insert_period(&tx, year, month, start, end, remark, now).await?;
            state_machine::log_created(
                &tx,
                state_machine::ENTITY_PERIOD,
                id,
                &code,
                STATUS_OPEN,
                operator.clone(),
                remark,
            )
            .await?;
            id
        }
    };

    let res = load_res(&tx, id).await?;
    tx.commit()
        .await
        .map_err(|error| db_error(error, "提交开账事务"))?;
    tracing::info!(period = %code, "会计期间已开账");
    Ok(res)
}

/// 批量开账（`from` ~ `to` 逐月，幂等：已存在的跳过而不是 409）。
///
/// 「一开开一年」是真实场景：每月初手动点 12 次，第 3 个月就会有人忘记开，
/// 结果凭证记到 `NOT_OPENED` 的期间（`period_of` 对未开期间是 fail-open 的）。
pub async fn batch_open(
    from: &str,
    to: &str,
    remark: Option<&str>,
    operator: state_machine::Operator,
) -> Result<BatchOpenRes, ApiError> {
    let (from_year, from_month) = parse_year_month(from)?;
    let (to_year, to_month) = parse_year_month(to)?;
    let from_key = i64::from(from_year) * 12 + i64::from(from_month);
    let to_key = i64::from(to_year) * 12 + i64::from(to_month);
    if from_key > to_key {
        return Err(bad_request(format!(
            "起始期间 {from} 晚于结束期间 {to}：批量开账的 from 必须 ≤ to"
        )));
    }
    let count = to_key - from_key + 1;
    if count > MAX_BATCH_MONTHS {
        return Err(bad_request(format!(
            "一次最多开 {MAX_BATCH_MONTHS} 个期间（当前 {count} 个）：\
             期间行建出来不可删除，别一次开几十年的账"
        )));
    }
    let remark = remark.map(str::trim).filter(|text| !text.is_empty());
    let now = Utc::now();

    let tx = db()
        .begin()
        .await
        .map_err(|error| db_error(error, "开启批量开账事务"))?;

    let mut created = Vec::new();
    let mut skipped = Vec::new();
    let mut year = from_year;
    let mut month = from_month;
    for _ in 0..count {
        let code = period_code(year, month);
        match find_by_code(&tx, &code).await? {
            Some(row) => {
                // 幂等：已存在的期间不重复建，只记账跳过（已结账的也跳过，
                // 不把「批量开账」变成「悄悄反结账」）
                skipped.push(row.period_code);
            }
            None => {
                let (start, end) = month_range(year, month)?;
                if let Some(other) = find_overlapping(&tx, start, end, Some(&code)).await? {
                    return Err(conflict(format!(
                        "期间 {code} 与已有期间 {other} 的日期区间重叠（{start} ~ {end}）：\
                         批量开账已中止，一个期间都没建"
                    )));
                }
                let id = insert_period(&tx, year, month, start, end, remark, now).await?;
                state_machine::log_created(
                    &tx,
                    state_machine::ENTITY_PERIOD,
                    id,
                    &code,
                    STATUS_OPEN,
                    operator.clone(),
                    remark,
                )
                .await?;
                created.push(load_res(&tx, id).await?);
            }
        }
        if month == 12 {
            year += 1;
            month = 1;
        } else {
            month += 1;
        }
    }

    tx.commit()
        .await
        .map_err(|error| db_error(error, "提交批量开账事务"))?;
    let created_count = i64::try_from(created.len()).unwrap_or(0);
    let skipped_count = i64::try_from(skipped.len()).unwrap_or(0);
    tracing::info!(
        created = created_count,
        skipped = skipped_count,
        "批量开账完成"
    );
    Ok(BatchOpenRes {
        created,
        skipped,
        created_count,
        skipped_count,
    })
}

// ==================== ② 结账 ====================

// ① 草稿凭证 / ② 未完成单据的取数（条数 + 明细）搬到了
// [`crate::business::erp::period_items`]：判定条件与明细投影必须是同一份 SQL，
// 两处各写一遍必然漂成「文案说 3 张、表格列 5 行」。

/// 期间内试算平衡：分录 Σ借 / Σ贷。
async fn trial_balance(ex: &impl SqlExecutor, code: &str) -> Result<(Decimal, Decimal), ApiError> {
    let rows = ex
        .query_sql(
            "SELECT COALESCE(SUM(e.debit_amount), 0)::text AS debit, \
                    COALESCE(SUM(e.credit_amount), 0)::text AS credit \
               FROM biz_voucher_entry e \
               JOIN biz_voucher v ON v.id = e.voucher_id \
              WHERE v.deleted_at IS NULL AND v.period = ?"
                .to_string(),
            vec![Value::Text(code.to_string())],
        )
        .await
        .map_err(|error| db_error(error, "试算平衡"))?;
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

// ④ 期间内不变量（A 组）的判定与明细在 [`crate::business::erp::period_items::invariant_details`]：
// 它同时给出**文案**（A1 / A2 各多少张）与**明细**（哪几张凭证），一次查询口径。
// 全库口径的**账实 / 账账**检查（erp_invariants 的 B4 流水累计、C1/C2 负库存）
// 不在这里拦，理由见 [`global_health_warnings`]。

/// 全库账实 / 账账巡检（erp_invariants 的 B4 + C1/C2）——**只提示，不拦结账**。
///
/// # 为什么降级为提示（设计判断，不是偷懒）
///
/// 这三条是**全库时点数**，不按期间切：
/// * B4 库存流水累计（IN 加 / OUT 减）= 当前库存
/// * C1 无负库存 / C2 无负库位库存
///
/// 让它们拦结账会带来两个坏后果：
/// 1. **任何一处库存漂移都会封死所有期间的结账** —— 2027 年的一笔错误调拨
///    能让 2018 年的账永远结不了，财务只能找运维改库；
/// 2. 这些漂移**不一定是错**：单据过账事务进行中、批量导入中途，都会短暂出现
///    「流水已写、库存未写」的中间态（本仓的单测就大量制造这种瞬间状态）。
///    用瞬时态去否决一个会计动作，是把断言当门禁。
///
/// 所以分工是：**结账闸管「这个月的账对不对」**（A1/A2 + 试算平衡 + 草稿凭证 + 单据终态），
/// **账实巡检管「整本账对不对」**（`scripts/verify/erp_invariants.py`，每天跑，
/// C1/C2 在那里是 BLOCKER 级）。close-check 仍然把它们的结论带出来（`warnings`），
/// 让人在封账那一刻看得见「库里现在有 5 组账实不符」，但决定权不交给他。
async fn global_health_warnings(ex: &impl SqlExecutor) -> Result<Vec<Blocker>, ApiError> {
    let mut warnings = Vec::new();

    let (negative, negative_items) = period_items::negative_stock_items(ex).await?;
    if negative > 0 {
        warnings.push(
            Blocker::with_count(
                BLOCKER_INVARIANT,
                negative,
                format!(
                    "巡检提示：库里有 {negative} 行负库存（账实不符，见库存巡检页），不影响本次结账{}",
                    limit_note(negative)
                ),
                Some("/business/erp/inventory?audit=1".to_string()),
            )
            .items(negative_items),
        );
    }

    let (drift, drift_items) = period_items::stock_drift_items(ex).await?;
    if drift > 0 {
        warnings.push(
            Blocker::with_count(
                BLOCKER_INVARIANT,
                drift,
                format!(
                    "巡检提示：{drift} 组「库存流水累计 ≠ 当前库存」（账账不符），不影响本次结账{}",
                    limit_note(drift)
                ),
                Some("/business/erp/inventory?audit=1".to_string()),
            )
            .items(drift_items),
        );
    }

    Ok(warnings)
}

/// 一次预检的完整结论：**拦人的**阻塞项 + **只提示**的巡检告警。
#[derive(Debug, Clone, Default)]
pub struct CheckOutcome {
    /// 必须清掉才能结账的问题。
    pub blockers: Vec<Blocker>,
    /// 全库巡检告警（不拦结账，前端灰字提示）。
    pub warnings: Vec<Blocker>,
}

impl CheckOutcome {
    /// 能否结账（只看 blockers）。
    pub fn can_close(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// 四项前置校验（结账与预检**共用同一份实现**，不许两处口径分裂）。
///
/// 返回 [`CheckOutcome`]：`blockers` 拦人，`warnings` 只提示（全库巡检口径）。
/// 每条 blocker 都带 `items`（最多 50 条明细 + 该谁处理 + 跳转），实现在
/// [`crate::business::erp::period_items`]。
pub(crate) async fn collect_blockers(
    ex: &impl SqlExecutor,
    row: &PeriodRow,
) -> Result<CheckOutcome, ApiError> {
    let mut blockers = Vec::new();
    // 处理人反查按请求缓存：同一个权限码一次预检只查一次库
    let mut owners = OwnerResolver::new();

    // ⓪ 期间自身状态
    match row.status.as_str() {
        STATUS_CLOSED => blockers.push(Blocker::with_count(
            BLOCKER_PERIOD_STATUS,
            0,
            format!(
                "会计期间 {} 已经是「已结账」状态（{} 结的），不需要再结一次",
                row.period_code,
                row.closed_at
                    .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "-".to_string())
            ),
            None,
        )),
        STATUS_NOT_OPENED => blockers.push(Blocker::with_count(
            BLOCKER_PERIOD_STATUS,
            0,
            format!(
                "会计期间 {} 还没开账（NOT_OPENED）：先开账再结账",
                row.period_code
            ),
            None,
        )),
        _ => {}
    }

    // ① 草稿凭证
    let (draft_count, draft_items) =
        period_items::draft_voucher_items(ex, &row.period_code, &mut owners).await?;
    if let Some(blocker) = draft_voucher_blocker(draft_count, &row.period_code, draft_items) {
        blockers.push(blocker);
    }

    // ② 未完成单据
    let (docs, doc_items) = period_items::unfinished_doc_items(ex, row, &mut owners).await?;
    if let Some(blocker) = unfinished_doc_blocker(&docs, doc_items) {
        blockers.push(blocker);
    }

    // ③ 试算平衡（分录口径）
    let (debit, credit) = trial_balance(ex, &row.period_code).await?;
    if let Some(blocker) = unbalanced_blocker(debit, credit) {
        blockers.push(blocker);
    }

    // ④ 不变量（期间内 A 组拦人；全库 B/C 只提示）
    let (failures, invariant_items) =
        period_items::invariant_details(ex, &row.period_code, &mut owners).await?;
    if let Some(blocker) = invariant_blocker(failures, invariant_items) {
        blockers.push(blocker);
    }
    let warnings = global_health_warnings(ex).await?;

    Ok(CheckOutcome { blockers, warnings })
}

/// 把阻塞项拼成 409（文案点名每一项，前端 toast 直接可读）。
pub fn rejection_message(period_code: &str, blockers: &[Blocker]) -> String {
    let detail = blockers
        .iter()
        .filter(|blocker| blocker.kind != BLOCKER_PERIOD_STATUS)
        .map(|blocker| blocker.message.clone())
        .collect::<Vec<_>>();
    if detail.is_empty() {
        return blockers
            .iter()
            .map(|blocker| blocker.message.clone())
            .collect::<Vec<_>>()
            .join("；");
    }
    format!("会计期间 {period_code} 不能结账：{}", detail.join("；"))
}

/// 结账预检（只读，不锁行）。
pub async fn close_check(id: Uuid) -> Result<CloseCheckRes, ApiError> {
    let row = load_period_row(db(), id).await?.ok_or_else(not_found)?;
    let outcome = collect_blockers(db(), &row).await?;
    Ok(CloseCheckRes {
        can_close: outcome.can_close(),
        period: PeriodRes::from(&row),
        blockers: outcome.blockers,
        warnings: outcome.warnings,
    })
}

/// 结账：四项校验 + 状态变更**同一个事务**（卡面 L11）。
pub async fn close_period(
    id: Uuid,
    reason: Option<&str>,
    operator: state_machine::Operator,
) -> Result<PeriodRes, ApiError> {
    let remark = optional_close_reason(reason)?;
    let now = Utc::now();

    let tx = db()
        .begin()
        .await
        .map_err(|error| db_error(error, "开启结账事务"))?;

    let row = find_by_id_locked(&tx, id).await?.ok_or_else(not_found)?;
    if row.status == STATUS_CLOSED {
        return Err(conflict(format!(
            "会计期间 {} 已经结账（{} 结的），不能重复结账",
            row.period_code,
            row.closed_at
                .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "-".to_string())
        )));
    }

    // 四项前置校验：**在同一个事务、同一把行锁里**跑完再改状态
    let outcome = collect_blockers(&tx, &row).await?;
    if let Some(warning) = outcome.warnings.first() {
        // 全库巡检告警只记日志，不拦封账（口径见 `global_health_warnings`）
        tracing::warn!(
            period = %row.period_code,
            warnings = outcome.warnings.len(),
            first = %warning.message,
            "结账时存在全库巡检告警（不拦截）"
        );
    }
    if !outcome.blockers.is_empty() {
        return Err(conflict(rejection_message(
            &row.period_code,
            &outcome.blockers,
        )));
    }

    state_machine::transition(
        &tx,
        state_machine::ENTITY_PERIOD,
        row.id,
        &row.period_code,
        &state_machine::PERIOD_EDGES,
        STATUS_OPEN,
        STATUS_CLOSED,
        operator.clone(),
        remark.as_deref().or(Some("结账")),
        None,
    )
    .await?;

    let affected = apply_status(
        &tx,
        row.id,
        STATUS_OPEN,
        STATUS_CLOSED,
        Some(now),
        operator.id,
        remark.as_deref(),
        now,
    )
    .await?;
    if affected == 0 {
        return Err(conflict(format!(
            "会计期间 {} 的状态在本次结账期间已被改动（期望 OPEN），请刷新后重试",
            row.period_code
        )));
    }

    let res = load_res(&tx, row.id).await?;
    tx.commit()
        .await
        .map_err(|error| db_error(error, "提交结账事务"))?;
    tracing::info!(period = %row.period_code, "会计期间已结账");
    Ok(res)
}

// ==================== ③ 反结账 ====================

/// 反结账：`CLOSED → OPEN`，`closed_at / closed_by` 清空，原因进状态日志。
///
/// 前置：状态必须是 `CLOSED`；原因 ≥ 10 字；**后续期间不能有已结账的**
/// （否则会出现「1 月开着、2 月封着」的窟窿，跨期调整没有落脚点）。
pub async fn reopen_period(
    id: Uuid,
    reason: Option<&str>,
    operator: state_machine::Operator,
) -> Result<PeriodRes, ApiError> {
    let reason = require_reopen_reason(reason)?;
    let now = Utc::now();

    let tx = db()
        .begin()
        .await
        .map_err(|error| db_error(error, "开启反结账事务"))?;

    let row = find_by_id_locked(&tx, id).await?.ok_or_else(not_found)?;
    if row.status != STATUS_CLOSED {
        return Err(conflict(format!(
            "会计期间 {} 当前是「{}」，只有「已结账（CLOSED）」的期间才能反结账",
            row.period_code, row.status
        )));
    }

    // 后续期间必须都还没结账（一次只能从最新封的月份往回撤）
    let later = tx
        .query_sql(
            "SELECT period_code FROM biz_accounting_period \
              WHERE period_code > ? AND status = 'CLOSED' \
              ORDER BY period_code LIMIT 3"
                .to_string(),
            vec![Value::Text(row.period_code.clone())],
        )
        .await
        .map_err(|error| db_error(error, "查询后续已结账期间"))?;
    if !later.is_empty() {
        let codes = later
            .iter()
            .map(|item| item.get_typed::<String>("period_code").unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" / ");
        return Err(conflict(format!(
            "后续期间 {codes} 还处于「已结账」状态：反结账必须从最后一个封账的月份往回撤，\
             否则会出现「前一个月开着、后一个月封着」的窟窿"
        )));
    }

    state_machine::transition(
        &tx,
        state_machine::ENTITY_PERIOD,
        row.id,
        &row.period_code,
        &state_machine::PERIOD_EDGES,
        STATUS_CLOSED,
        STATUS_OPEN,
        operator.clone(),
        Some(reason.as_str()),
        None,
    )
    .await?;

    let affected = ex_update_reopen(&tx, row.id, now).await?;
    if affected == 0 {
        return Err(conflict(format!(
            "会计期间 {} 的状态在本次反结账期间已被改动（期望 CLOSED），请刷新后重试",
            row.period_code
        )));
    }

    let res = load_res(&tx, row.id).await?;
    tx.commit()
        .await
        .map_err(|error| db_error(error, "提交反结账事务"))?;
    tracing::warn!(period = %row.period_code, reason = %reason, "会计期间已反结账");
    Ok(res)
}

/// 反结账的 UPDATE：状态回 `OPEN`、结账痕迹清空（**不写 remark**，保留原备注）。
async fn ex_update_reopen(
    ex: &impl SqlExecutor,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<u64, ApiError> {
    ex.exec_sql(
        "UPDATE biz_accounting_period \
            SET status = ?, closed_at = NULL, closed_by = NULL, updated_at = ? \
          WHERE id = ?::uuid AND status = ?"
            .to_string(),
        vec![
            Value::Text(STATUS_OPEN.to_string()),
            Value::from(now),
            Value::from(&id),
            Value::Text(STATUS_CLOSED.to_string()),
        ],
    )
    .await
    .map_err(|error| db_error(error, "更新会计期间状态（反结账）"))
}

// ==================== 记账入口的期间闸 ====================

/// 期间可写闸：已结账 → **400**（卡面 §一 ②「结账后」三条）。
///
/// 挂点：`voucher::post_voucher`（自动凭证 / 手工凭证 / 付款凭证的共同收口）。
/// 单据过账要生成凭证，因此「该期间的单据不能过账」也由这一条闸兜住（400 + 整笔回滚）。
pub async fn ensure_period_writable(
    ex: &impl SqlExecutor,
    period_code: &str,
    action: &str,
) -> Result<(), ApiError> {
    if voucher::is_period_closed(ex, period_code)
        .await
        .map_err(|error| db_error(error, "查询期间结账状态"))?
    {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "会计期间 {period_code} 已结账，不能再{action}：\
                 已封账的月份只读，请在下一个未结账的期间做调整（或先反结账）"
            ),
        ));
    }
    Ok(())
}

// ==================== 接口层 ====================

/// 开账（单月）。
#[nik::permission(
    path = "/business/erp/period",
    method = "POST",
    permission = "business:erp:period:open"
)]
#[nik::post("/business/erp/period")]
pub async fn period_open_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<OpenPeriodReq>,
) -> ResResult<PeriodRes> {
    ensure_permission(&claims, PERM_OPEN, "开账")?;
    Ok(Res::ok(
        open_period(
            req.period_year,
            req.period_month,
            req.remark.as_deref(),
            state_machine::Operator::from(&claims),
        )
        .await?,
    ))
}

/// 批量开账（`from` ~ `to` 逐月，幂等）。
#[nik::permission(
    path = "/business/erp/period/batch-open",
    method = "POST",
    permission = "business:erp:period:open"
)]
#[nik::post("/business/erp/period/batch-open")]
pub async fn period_batch_open_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchOpenReq>,
) -> ResResult<BatchOpenRes> {
    ensure_permission(&claims, PERM_OPEN, "开账")?;
    Ok(Res::ok(
        batch_open(
            &req.from,
            &req.to,
            req.remark.as_deref(),
            state_machine::Operator::from(&claims),
        )
        .await?,
    ))
}

/// 结账预检。
#[nik::permission(
    path = "/business/erp/period/{id}/close-check",
    method = "GET",
    permission = "business:erp:period:close"
)]
#[nik::get("/business/erp/period/{id}/close-check")]
pub async fn period_close_check_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<CloseCheckRes> {
    ensure_permission(&claims, PERM_CLOSE, "结账")?;
    let id = parse_period_id(&id)?;
    Ok(Res::ok(close_check(id).await?))
}

/// 结账。
#[nik::permission(
    path = "/business/erp/period/{id}/close",
    method = "POST",
    permission = "business:erp:period:close"
)]
#[nik::post("/business/erp/period/{id}/close")]
pub async fn period_close_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<ClosePeriodReq>,
) -> ResResult<PeriodRes> {
    ensure_permission(&claims, PERM_CLOSE, "结账")?;
    let id = parse_period_id(&id)?;
    Ok(Res::ok(
        close_period(
            id,
            req.reason.as_deref(),
            state_machine::Operator::from(&claims),
        )
        .await?,
    ))
}

/// 反结账。
#[nik::permission(
    path = "/business/erp/period/{id}/reopen",
    method = "POST",
    permission = "business:erp:period:reopen"
)]
#[nik::post("/business/erp/period/{id}/reopen")]
pub async fn period_reopen_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<ReopenPeriodReq>,
) -> ResResult<PeriodRes> {
    ensure_permission(&claims, PERM_REOPEN, "反结账")?;
    let id = parse_period_id(&id)?;
    Ok(Res::ok(
        reopen_period(
            id,
            req.reason.as_deref(),
            state_machine::Operator::from(&claims),
        )
        .await?,
    ))
}

/// 路径里的期间 id：非法 UUID → 400（不静默当成 404）。
pub(crate) fn parse_period_id(raw: &str) -> Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request("id 不能为空"));
    }
    Uuid::parse_str(text).map_err(|_| bad_request(format!("id 不是合法的 UUID：{text}")))
}

// ==================== 单测（真库，15 条）====================
//
// 需要真库：`ERP_DB_URL` 指向「已应用 V0137–V0188 的隔离库」；缺省则全部 SKIP：
//
// ```bash
// export ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_p1v'
// cargo test --lib business::erp::period -- --test-threads=1
// ```
//
// # 夹具约定（两条都不能破）
//
// 1. 期间编码一律 `2031-xx` / `2032-xx`（生产在用 `2026-xx`）。**绝不结账 2026-10**：
//    别的模块（report / income_statement / voucher_write）的夹具往 2026-10 记凭证，
//    本模块把它封了，那些用例会立刻收到「期间已结账」的 400 —— 那是跨模块污染，不是它们的错。
// 2. 串行（`DB_LOCK`）：清理按前缀删，并行会互删夹具。
//
// 夹具前缀：凭证号 `P1-V-`、采购单号 `P1-PO-`、状态日志靠期间编码识别。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::voucher::{AccountRef, EntryLine, PostVoucher};
    use crate::system::tests_support as ts;

    /// 真库用例串行跑。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    /// 夹具操作人（`closed_by` 是弱引用，不要求真实用户）。
    const TEST_UID: &str = "00000000-0000-7000-8000-0000000000a1";

    /// 夹具操作人 id。
    fn uid() -> Uuid {
        Uuid::parse_str(TEST_UID).expect("测试 uuid 非法")
    }

    /// 夹具操作人。
    fn operator() -> state_machine::Operator {
        state_machine::Operator::named(Some(uid()), "p1-tester")
    }

    /// 金额字面量。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试金额字面量非法")
    }

    /// 期间 id 字符串 → `Uuid`。
    fn uuid_of(raw: &str) -> Uuid {
        Uuid::parse_str(raw).expect("期间 id 不是 uuid")
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
                // 每个 `#[tokio::test]` 是独立 runtime，而 `db()` 是进程级全局池：
                // `max_lifetime = 0` 逼池子每次新建连接，避免借到上一个 runtime 的死连接。
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

    /// 清掉本模块造的所有夹具（按前缀收敛，**不碰 2026-xx 的生产期间**）。
    async fn cleanup() {
        // 状态日志没有外键，必须按期间编码先删（期间行一删就查不到 id 了）
        sql(
            "DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_PERIOD' \
             AND (entity_no LIKE '2031-%' OR entity_no LIKE '2032-%' OR entity_no LIKE '2033-%')",
        )
        .await;
        // 分录随凭证 CASCADE
        sql(
            "DELETE FROM biz_voucher WHERE period LIKE '2031-%' OR period LIKE '2032-%' \
             OR period LIKE '2033-%'",
        )
        .await;
        sql("DELETE FROM biz_purchase_order WHERE order_no LIKE 'P1-PO-%'").await;
        sql(
            "DELETE FROM biz_accounting_period WHERE period_code LIKE '2031-%' \
             OR period_code LIKE '2032-%' OR period_code LIKE '2033-%'",
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

    /// 期间行数（按编码）。
    async fn period_count(code: &str) -> i64 {
        db().scalar_with_params(
            "SELECT COUNT(*) FROM biz_accounting_period WHERE period_code = ?",
            vec![Value::Text(code.to_string())],
        )
        .await
        .expect("统计期间")
    }

    /// 期间状态（按编码）。
    async fn status_of(code: &str) -> Option<String> {
        let rows = db()
            .find_with_params(
                "SELECT status FROM biz_accounting_period WHERE period_code = ?",
                vec![Value::Text(code.to_string())],
            )
            .await
            .expect("读期间状态");
        rows.first()
            .map(|row| row.get_typed("status").expect("status"))
    }

    /// 期间内凭证数。
    async fn voucher_count(period: &str) -> i64 {
        db().scalar_with_params(
            "SELECT COUNT(*) FROM biz_voucher WHERE period = ?",
            vec![Value::Text(period.to_string())],
        )
        .await
        .expect("统计凭证")
    }

    /// 造一张凭证（单头 + 两条分录）。
    ///
    /// 单头金额与分录金额**分开给**：`ck_biz_voucher_balance` 只约束单头，
    /// 「分录被改过」是 A1/A2 不变量的地盘 —— 这也是试算平衡必须按分录求和的原因。
    async fn seed_voucher(
        period: &str,
        voucher_no: &str,
        status: &str,
        head: (&str, &str),
        entries: (&str, &str),
    ) -> Uuid {
        let id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_voucher \
                (id, voucher_no, period, voucher_date, total_debit, total_credit, entry_count, \
                 status, summary, voucher_type, source_type) \
             VALUES (?, ?, ?, ?::text::date, ?::text::numeric, ?::text::numeric, 2, ?, ?, \
                     'MANUAL', 'MANUAL')",
            vec![
                Value::from(&id),
                Value::Text(voucher_no.to_string()),
                Value::Text(period.to_string()),
                Value::Text(format!("{period}-15")),
                Value::Text(head.0.to_string()),
                Value::Text(head.1.to_string()),
                Value::Text(status.to_string()),
                Value::Text(format!("P1 夹具 {voucher_no}")),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("插入凭证头失败：{err}"));
        let debit_acc = Uuid::now_v7();
        let credit_acc = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_voucher_entry \
                (voucher_id, line_no, summary, account_id, account_code, account_name, \
                 debit_amount, credit_amount) \
             VALUES (?, 1, ?, ?, '1001', '库存现金', ?::text::numeric, 0)",
            vec![
                Value::from(&id),
                Value::Text(format!("P1 夹具 {voucher_no}")),
                Value::from(&debit_acc),
                Value::Text(entries.0.to_string()),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("插入借方分录失败：{err}"));
        db().execute_with_params(
            "INSERT INTO biz_voucher_entry \
                (voucher_id, line_no, summary, account_id, account_code, account_name, \
                 debit_amount, credit_amount) \
             VALUES (?, 2, ?, ?, '3001', '实收资本', 0, ?::text::numeric)",
            vec![
                Value::from(&id),
                Value::Text(format!("P1 夹具 {voucher_no}")),
                Value::from(&credit_acc),
                Value::Text(entries.1.to_string()),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("插入贷方分录失败：{err}"));
        id
    }

    /// 走**生产同款**入口落一张手工凭证（`voucher::post_voucher`）。
    async fn post_simple_voucher(voucher_date: &str) -> std::result::Result<Uuid, DbError> {
        let date = NaiveDate::parse_from_str(voucher_date, "%Y-%m-%d").expect("日期字面量非法");
        let cash = AccountRef {
            id: Uuid::now_v7(),
            code: "1001".to_string(),
            name: "库存现金".to_string(),
        };
        let capital = AccountRef {
            id: Uuid::now_v7(),
            code: "3001".to_string(),
            name: "实收资本".to_string(),
        };
        let lines = vec![
            EntryLine {
                line_no: 1,
                summary: "P1 单测".to_string(),
                account: cash,
                debit: dec("10"),
                credit: Decimal::ZERO,
                aux_supplier_id: None,
                aux_material_id: None,
                aux_warehouse_id: None,
            },
            EntryLine {
                line_no: 2,
                summary: "P1 单测".to_string(),
                account: capital,
                debit: Decimal::ZERO,
                credit: dec("10"),
                aux_supplier_id: None,
                aux_material_id: None,
                aux_warehouse_id: None,
            },
        ];
        voucher::post_voucher(
            db(),
            &PostVoucher {
                voucher_type: voucher::VOUCHER_TYPE_MANUAL,
                source_type: voucher::SOURCE_MANUAL,
                source_id: None,
                source_no: Some("P1 单测凭证"),
                voucher_date: date,
                summary: "P1 单测凭证".to_string(),
                lines,
                remark: None,
                red_of: None,
            },
            Some(uid()),
            Utc::now(),
        )
        .await
    }

    // ---------- ① 开账 ----------

    /// ① 开 2031-02 → 创建成功，起止日正确（含闰年 2 月），并写一条 `NULL → OPEN` 日志。
    #[tokio::test]
    async fn p1_open_period_computes_month_range() {
        let Some(_guard) = setup().await else {
            return;
        };
        let res = open_period(2031, 2, Some("P1 单测"), operator())
            .await
            .expect("开账应成功");
        assert_eq!(res.period_code, "2031-02");
        assert_eq!(res.period_year, 2031);
        assert_eq!(res.period_month, 2);
        assert_eq!(res.start_date.to_string(), "2031-02-01");
        assert_eq!(res.end_date.to_string(), "2031-02-28");
        assert_eq!(res.status, STATUS_OPEN);
        assert!(res.closed_at.is_none());

        let trail = state_machine::trail_of_no(db(), "2031-02")
            .await
            .expect("读状态轨迹");
        assert_eq!(trail.len(), 1, "开账只该写一条日志：{trail:?}");
        assert_eq!(trail[0].entity_type, state_machine::ENTITY_PERIOD);
        assert_eq!(trail[0].to_status, STATUS_OPEN);
        assert!(
            trail[0].from_status.is_none(),
            "建账日志的 from_status 必须是 NULL"
        );

        // 2032 是闰年 → 2 月 29 天（期间结束日必须按真实日历算）
        let leap = open_period(2032, 2, None, operator())
            .await
            .expect("开账应成功");
        assert_eq!(leap.end_date.to_string(), "2032-02-29");
    }

    /// ② 重复开同一期间 → 409，且库里仍然只有一行。
    #[tokio::test]
    async fn p1_open_same_period_twice_is_conflict() {
        let Some(_guard) = setup().await else {
            return;
        };
        open_period(2031, 3, None, operator())
            .await
            .expect("首次开账应成功");
        let err = open_period(2031, 3, None, operator())
            .await
            .expect_err("重复开账必须 409");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("已经开账"), "文案要点名：{}", err.msg);
        assert_eq!(period_count("2031-03").await, 1);
    }

    /// ③ 批量开 2031-04 ~ 2032-05 → 14 个期间；重跑幂等（0 新建 / 14 跳过）。
    #[tokio::test]
    async fn p1_batch_open_creates_fourteen_periods() {
        let Some(_guard) = setup().await else {
            return;
        };
        let res = batch_open("2031-04", "2032-05", None, operator())
            .await
            .expect("批量开账应成功");
        assert_eq!(res.created_count, 14, "2031-04 ~ 2032-05 是 14 个月");
        assert_eq!(
            res.created.first().map(|item| item.period_code.as_str()),
            Some("2031-04")
        );
        assert_eq!(
            res.created.last().map(|item| item.period_code.as_str()),
            Some("2032-05")
        );
        assert!(res.skipped.is_empty());
        for (code, start, end) in [
            ("2031-04", "2031-04-01", "2031-04-30"),
            ("2031-12", "2031-12-01", "2031-12-31"),
            ("2032-02", "2032-02-01", "2032-02-29"),
        ] {
            let row = res
                .created
                .iter()
                .find(|item| item.period_code == code)
                .unwrap_or_else(|| panic!("缺期间 {code}"));
            assert_eq!(row.start_date.to_string(), start);
            assert_eq!(row.end_date.to_string(), end);
        }

        // 幂等：同一段再开一次 → 不 409、不重复建
        let again = batch_open("2031-04", "2032-05", None, operator())
            .await
            .expect("重跑必须幂等");
        assert_eq!(again.created_count, 0);
        assert_eq!(again.skipped_count, 14);
        assert_eq!(period_count("2031-04").await, 1);
    }

    /// ④ 无 `business:erp:period:open` → 403；有码放行；三个码在 `sys_menu` 里都有节点。
    #[tokio::test]
    async fn p1_open_without_permission_is_forbidden() {
        let Some(_guard) = setup().await else {
            return;
        };
        let no_perm = ts::claims(TEST_UID, "p1-no-perm", &[]);
        let err = ensure_permission(&no_perm.0, PERM_OPEN, "开账").expect_err("无码必须 403");
        assert_eq!(err.status, 403);
        assert!(err.msg.contains(PERM_OPEN), "文案要点名权限码：{}", err.msg);

        let with_perm = ts::claims(TEST_UID, "p1-finance", &[PERM_OPEN]);
        assert!(ensure_permission(&with_perm.0, PERM_OPEN, "开账").is_ok());

        // 三个码必须都种进 sys_menu（否则注解守的是一个不存在的码，勾给角色永远不生效）
        for (code, _) in PERIOD_PERMISSIONS {
            let rows = db()
                .find_with_params(
                    "SELECT auth_code FROM sys_menu WHERE auth_code = ? AND deleted_at IS NULL",
                    vec![Value::Text(code.to_string())],
                )
                .await
                .expect("查权限节点");
            assert_eq!(
                rows.len(),
                1,
                "权限码 {code} 没有 sys_menu 节点（V0188 未应用？）"
            );
        }
    }

    // ---------- ② 结账 ----------

    /// ⑤ 空期间（无凭证无单据）→ 结账成功，`status = CLOSED` + `closed_at / closed_by` 落库。
    #[tokio::test]
    async fn p1_close_empty_period_succeeds() {
        let Some(_guard) = setup().await else {
            return;
        };
        let opened = open_period(2031, 5, None, operator())
            .await
            .expect("开账应成功");
        let id = uuid_of(&opened.id);

        let check = close_check(id).await.expect("预检");
        assert!(check.can_close, "空期间应可结账：{:?}", check.blockers);
        assert!(check.blockers.is_empty(), "{:?}", check.blockers);
        // 全库巡检告警（B4 流水累计 / C1/C2 负库存）**只提示、不拦结账**：
        // 它们是「整本账」的时点状态（别的用例跑一半时库里就有漂移），
        // 混进阻断项会让任何一处库存漂移封死所有期间的结账（见模块文档 §四项前置校验）。
        assert!(
            check
                .warnings
                .iter()
                .all(|item| item.kind == BLOCKER_INVARIANT),
            "warnings 里只该有巡检类提示：{:?}",
            check.warnings
        );

        let closed = close_period(id, Some("2031 年 5 月结账"), operator())
            .await
            .expect("结账应成功");
        assert_eq!(closed.status, STATUS_CLOSED);
        assert!(closed.closed_at.is_some(), "closed_at 必须落库");
        assert_eq!(closed.closed_by.as_deref(), Some(TEST_UID));

        let trail = state_machine::trail_of_no(db(), "2031-05")
            .await
            .expect("读状态轨迹");
        assert_eq!(trail.len(), 2);
        assert_eq!(trail[1].from_status.as_deref(), Some(STATUS_OPEN));
        assert_eq!(trail[1].to_status, STATUS_CLOSED);
        assert_eq!(trail[1].reason.as_deref(), Some("2031 年 5 月结账"));

        // 重复结账 → 409
        let err = close_period(id, None, operator())
            .await
            .expect_err("重复结账必须 409");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("已经结账"), "{}", err.msg);
    }

    /// ⑥ 有 3 张草稿凭证 → 409 且**文案点名「3 张草稿凭证」**；期间仍是 OPEN。
    #[tokio::test]
    async fn p1_close_blocked_by_draft_vouchers() {
        let Some(_guard) = setup().await else {
            return;
        };
        let opened = open_period(2031, 6, None, operator())
            .await
            .expect("开账应成功");
        let id = uuid_of(&opened.id);
        for no in ["P1-V-001", "P1-V-002", "P1-V-003"] {
            seed_voucher("2031-06", no, "DRAFT", ("100", "100"), ("100", "100")).await;
        }

        let check = close_check(id).await.expect("预检");
        assert!(!check.can_close);
        let blocker = check
            .blockers
            .iter()
            .find(|item| item.kind == BLOCKER_DRAFT_VOUCHER)
            .expect("应有草稿凭证阻塞项");
        assert_eq!(blocker.count, Some(3));
        assert!(
            blocker.message.contains("还有 3 张草稿凭证未过账"),
            "文案要点名张数：{}",
            blocker.message
        );
        assert_eq!(blocker.samples.len(), 3, "定位线索最多 3 个凭证号");

        let err = close_period(id, None, operator())
            .await
            .expect_err("有草稿凭证必须 409");
        assert_eq!(err.status, 409);
        assert!(
            err.msg.contains("还有 3 张草稿凭证未过账"),
            "409 文案要点名张数：{}",
            err.msg
        );
        assert_eq!(
            status_of("2031-06").await.as_deref(),
            Some(STATUS_OPEN),
            "结账失败不能改状态"
        );
    }

    /// ⑦ 借贷不平（分录侧 Σ借 60 / Σ贷 40）→ 409 + 差额 20 元。
    #[tokio::test]
    async fn p1_close_blocked_by_unbalanced_entries() {
        let Some(_guard) = setup().await else {
            return;
        };
        let opened = open_period(2031, 7, None, operator())
            .await
            .expect("开账应成功");
        let id = uuid_of(&opened.id);
        // 单头 100/100（满足 ck_biz_voucher_balance），分录却是 60/40 —— 只有分录侧的校验抓得到
        seed_voucher(
            "2031-07",
            "P1-V-101",
            "POSTED",
            ("100", "100"),
            ("60", "40"),
        )
        .await;

        let check = close_check(id).await.expect("预检");
        let blocker = check
            .blockers
            .iter()
            .find(|item| item.kind == BLOCKER_UNBALANCED)
            .expect("应有借贷不平阻塞项");
        assert_eq!(blocker.diff.as_deref(), Some("20"));
        assert!(
            blocker.message.contains("借贷不平，差 20 元"),
            "文案要点名差额：{}",
            blocker.message
        );

        let err = close_period(id, None, operator())
            .await
            .expect_err("借贷不平必须 409");
        assert_eq!(err.status, 409);
        assert!(err.msg.contains("借贷不平"), "{}", err.msg);
        assert!(err.msg.contains("20"), "差额要写进文案：{}", err.msg);
        assert_eq!(status_of("2031-07").await.as_deref(), Some(STATUS_OPEN));
    }

    /// ⑧ 期间内有 DRAFT 采购单 → 409「还有 1 张单据未完成（采购单 1 张）」。
    #[tokio::test]
    async fn p1_close_blocked_by_unfinished_documents() {
        let Some(_guard) = setup().await else {
            return;
        };
        let opened = open_period(2031, 8, None, operator())
            .await
            .expect("开账应成功");
        let id = uuid_of(&opened.id);
        sql("INSERT INTO biz_purchase_order \
                (order_no, supplier_id, order_date, warehouse_id, status) \
             VALUES ('P1-PO-001', gen_random_uuid(), '2031-08-10', gen_random_uuid(), 'DRAFT')")
        .await;

        let check = close_check(id).await.expect("预检");
        let blocker = check
            .blockers
            .iter()
            .find(|item| item.kind == BLOCKER_DOC_UNFINISHED)
            .expect("应有未完成单据阻塞项");
        assert_eq!(blocker.count, Some(1));
        assert!(
            blocker.message.contains("还有 1 张单据未完成"),
            "文案要点名张数：{}",
            blocker.message
        );
        assert!(
            blocker.message.contains("采购单 1 张"),
            "文案要点名单据类型：{}",
            blocker.message
        );

        let err = close_period(id, None, operator())
            .await
            .expect_err("有未完成单据必须 409");
        assert_eq!(err.status, 409);
        assert!(err.msg.contains("采购单"), "{}", err.msg);
        assert_eq!(status_of("2031-08").await.as_deref(), Some(STATUS_OPEN));
    }

    /// ⑨ 结账后生成凭证 → **400**「会计期间 X 已结账」，且一行都没写进去。
    #[tokio::test]
    async fn p1_post_voucher_to_closed_period_is_bad_request() {
        let Some(_guard) = setup().await else {
            return;
        };
        let opened = open_period(2031, 9, None, operator())
            .await
            .expect("开账应成功");
        close_period(uuid_of(&opened.id), None, operator())
            .await
            .expect("结账应成功");

        let err = post_simple_voucher("2031-09-10")
            .await
            .expect_err("已结账期间不能再生成凭证");
        match err {
            DbError::InvalidValue(message) => {
                assert!(message.contains("已结账"), "{message}");
                assert!(message.contains("2031-09"), "文案要点名期间：{message}");
            }
            other => panic!("期望 InvalidValue（接口层翻 400），实际 {other:?}"),
        }
        assert_eq!(voucher_count("2031-09").await, 0, "凭证一行都不该落库");
    }

    // ---------- ③ 反结账 ----------

    /// ⑩ 反结账成功 → `status = OPEN`、`closed_at / closed_by` 清空、日志带原因。
    #[tokio::test]
    async fn p1_reopen_clears_closed_marks() {
        let Some(_guard) = setup().await else {
            return;
        };
        let opened = open_period(2031, 9, None, operator())
            .await
            .expect("开账应成功");
        let id = uuid_of(&opened.id);
        close_period(id, None, operator())
            .await
            .expect("结账应成功");

        let reopened = reopen_period(id, Some("发现 9 月漏记一笔运费，需要补凭证"), operator())
            .await
            .expect("反结账应成功");
        assert_eq!(reopened.status, STATUS_OPEN);
        assert!(reopened.closed_at.is_none(), "closed_at 必须清空");
        assert!(reopened.closed_by.is_none(), "closed_by 必须清空");

        let trail = state_machine::trail_of_no(db(), "2031-09")
            .await
            .expect("读状态轨迹");
        assert_eq!(trail.len(), 3, "开账 / 结账 / 反结账三条：{trail:?}");
        assert_eq!(trail[2].from_status.as_deref(), Some(STATUS_CLOSED));
        assert_eq!(trail[2].to_status, STATUS_OPEN);
        assert!(
            trail[2]
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("漏记")),
            "反结账原因必须进状态日志：{:?}",
            trail[2].reason
        );
    }

    /// ⑪ 反结账不给原因 / 原因太短 → 400，且期间仍是 CLOSED。
    #[tokio::test]
    async fn p1_reopen_requires_reason() {
        let Some(_guard) = setup().await else {
            return;
        };
        let opened = open_period(2031, 9, None, operator())
            .await
            .expect("开账应成功");
        let id = uuid_of(&opened.id);
        close_period(id, None, operator())
            .await
            .expect("结账应成功");

        let err = reopen_period(id, None, operator())
            .await
            .expect_err("不给原因必须 400");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("必须填写原因"), "{}", err.msg);

        let err = reopen_period(id, Some("太短"), operator())
            .await
            .expect_err("原因太短必须 400");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("至少 10 个字"), "{}", err.msg);

        assert_eq!(
            status_of("2031-09").await.as_deref(),
            Some(STATUS_CLOSED),
            "校验失败不能改状态"
        );
    }

    /// ⑫ 反结账后可以继续生成凭证（记到同一个期间）。
    #[tokio::test]
    async fn p1_reopen_restores_posting() {
        let Some(_guard) = setup().await else {
            return;
        };
        let opened = open_period(2031, 9, None, operator())
            .await
            .expect("开账应成功");
        let id = uuid_of(&opened.id);
        close_period(id, None, operator())
            .await
            .expect("结账应成功");
        assert!(
            post_simple_voucher("2031-09-10").await.is_err(),
            "结账期间应当拒绝记账"
        );

        reopen_period(id, Some("供应商退回上月运费，需要补记一笔凭证"), operator())
            .await
            .expect("反结账应成功");

        let voucher_id = post_simple_voucher("2031-09-10")
            .await
            .expect("反结账后必须能继续生成凭证");
        let rows = db()
            .find_with_params(
                "SELECT period, status FROM biz_voucher WHERE id = ?",
                vec![Value::from(&voucher_id)],
            )
            .await
            .expect("回读凭证");
        let row = rows.first().expect("凭证应存在");
        let period: String = row.get_typed("period").expect("period");
        let status: String = row.get_typed("status").expect("status");
        assert_eq!(period, "2031-09");
        assert_eq!(status, "POSTED");
        assert_eq!(voucher_count("2031-09").await, 1);
    }

    /// ⑬ 后续期间已结账时反结前一个期间 → 409（必须从最后一个封账的月份往回撤）。
    #[tokio::test]
    async fn p1_reopen_blocked_by_later_closed_period() {
        let Some(_guard) = setup().await else {
            return;
        };
        let sep = open_period(2031, 9, None, operator())
            .await
            .expect("开账应成功");
        close_period(uuid_of(&sep.id), None, operator())
            .await
            .expect("9 月结账");
        let oct = open_period(2031, 10, None, operator())
            .await
            .expect("开账应成功");
        close_period(uuid_of(&oct.id), None, operator())
            .await
            .expect("10 月结账");

        let err = reopen_period(
            uuid_of(&sep.id),
            Some("9 月凭证科目挂错，需要重记一张"),
            operator(),
        )
        .await
        .expect_err("后续期间已结账必须 409");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("2031-10"), "要点名后续期间：{}", err.msg);
        assert_eq!(status_of("2031-09").await.as_deref(), Some(STATUS_CLOSED));
    }

    /// ⑮ 手工凭证接口（`voucher_write::create_manual_voucher`）在已结账期间 → **400**。
    ///
    /// ⑨ 证明的是 `post_voucher`（唯一落库收口）会挡；这条证明**手工凭证入口**
    /// 在拼分录之前就给同一个 400 —— 生产真机验收第一轮正是在这里发现它返回了 409
    /// （「手工凭证只能记到已开启期间」），与卡面要求的 400「期间已结账」不一致。
    #[tokio::test]
    async fn p1_manual_voucher_on_closed_period_is_bad_request() {
        use crate::business::erp::voucher_write::{
            CreateVoucherReq, ManualEntryReq, create_manual_voucher,
        };

        let Some(_guard) = setup().await else {
            return;
        };
        let opened = open_period(2031, 9, None, operator())
            .await
            .expect("开账应成功");
        close_period(uuid_of(&opened.id), None, operator())
            .await
            .expect("结账应成功");

        let rows = db()
            .find_with_params(
                "SELECT id FROM biz_account WHERE account_code IN ('1001', '3001') \
                  AND deleted_at IS NULL ORDER BY account_code",
                vec![],
            )
            .await
            .expect("查科目");
        assert_eq!(rows.len(), 2, "隔离库应已种 1001 / 3001 两个科目");
        let ids: Vec<String> = rows
            .iter()
            .map(|row| row.get_typed::<Uuid>("id").expect("id").to_string())
            .collect();

        let req = CreateVoucherReq {
            voucher_date: Some(NaiveDate::from_ymd_opt(2031, 9, 10).expect("日期")),
            summary: Some("P1E2E 结账后手工凭证".to_string()),
            entries: Some(vec![
                ManualEntryReq {
                    account_id: Some(ids[0].clone()),
                    debit: Some(dec("10")),
                    credit: None,
                    summary: None,
                },
                ManualEntryReq {
                    account_id: Some(ids[1].clone()),
                    debit: None,
                    credit: Some(dec("10")),
                    summary: None,
                },
            ]),
            remark: None,
        };
        let err = create_manual_voucher(&req, Some(uid()))
            .await
            .expect_err("已结账期间不能记手工凭证");
        assert_eq!(err.status, 400, "{} / {}", err.code, err.msg);
        assert!(err.msg.contains("已结账"), "{}", err.msg);
        assert!(err.msg.contains("2031-09"), "文案要点名期间：{}", err.msg);
        assert_eq!(voucher_count("2031-09").await, 0, "凭证一行都不该落库");
    }

    // ---------- 纯逻辑（不连库） ----------

    /// ⑭ 入参护栏：`YYYY-MM` 严格解析、月份/年份边界、试算平衡容差。
    #[test]
    fn p1_input_guards_are_strict() {
        assert_eq!(parse_year_month("2031-11").expect("合法"), (2031, 11));
        for bad in ["2031-1", "2031-13", "2031/11", "31-11", "2031-11-01", ""] {
            let err = parse_year_month(bad).expect_err("必须 400");
            assert_eq!(err.status, 400, "{bad}");
        }
        // 月份越界 / 年份越界都在 month_range 拦
        assert_eq!(month_range(2031, 0).expect_err("0 月").status, 400);
        assert_eq!(month_range(2031, 13).expect_err("13 月").status, 400);
        assert_eq!(month_range(1999, 1).expect_err("年份太早").status, 400);
        // 12 月 31 日不能算成下一年 1 月 1 日
        let (start, end) = month_range(2031, 12).expect("12 月");
        assert_eq!(start.to_string(), "2031-12-01");
        assert_eq!(end.to_string(), "2031-12-31");
        // 试算平衡容差：0.0001 以内不算不平，0.0002 就算
        assert!(unbalanced_blocker(dec("100"), dec("100.0001")).is_none());
        let blocker = unbalanced_blocker(dec("100"), dec("99.9998")).expect("必须有阻塞项");
        assert_eq!(blocker.diff.as_deref(), Some("0.0002"));
    }
}
