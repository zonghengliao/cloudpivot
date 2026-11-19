//! 成本方法切换的迁移策略（成本核算 · M5-31 —— 存量期间不重算）。
//!
//! 前置卡：M5-01（`biz_cost_method_config` 版本时间线 + 已生效锁定）、
//! M5-03…M5-30（成本计算 / 结算 / 还原）。设计依据：
//! `docs/architecture/ARCH-9-成本核算升级设计.md` D-3（一期只做分批法；**整期按一种方法算**）、
//! `docs/evidence/2026-11-18-m501-cost-method/README.md` §五（切换史 = 配置时间线，不另建表）。
//!
//! # 一句话：切换只在期间边界发生，历史期间一个字节都不动
//!
//! 一期引擎的算账单位是**自然月**（`biz_cost_period.period_code = yyyy-MM`），
//! 一期一次算一期用的方法由「该期间内是否有一条配置生效」决定（`cost_calc_period` 的
//! `ensure_methods_runnable_in_tx`）。所以「切换方法」在数据上**只有两笔写**：
//!
//! ```text
//! 旧版本（覆盖切换日前一天的、同范围启用中版本）  effective_to := 切换日 − 1 天
//! 新版本                                          effective_from := 切换日
//! ```
//!
//! 因为旧版本的窗口只被**收窄到切换日前一天**，而切换日一定是某个期间的首日，
//! 所以「切换日之前的每一个期间」解析出来的方法**逐日不变**（`resolve` 按日期取行，
//! 收窄的是切换日之后的日子）。这就是「存量期间不重算」在数据层面的全部含义：
//!
//! - **不碰结果表**：`biz_cost_period` / `biz_cost_period_item` / 归集 / 分摊 / 还原
//!   一行不改（本卡零迁移、零重算 SQL）；
//! - **不碰已结账期间**：已 `SETTLED` / `CLOSED` 的期间本来也不许重算（走红冲），
//!   切换只影响「从切换日起还没算过的期间」；
//! - **`DRAFT` 的存量期间仍按旧方法**：用户什么时候点「重算」，`resolve(…, 期间末日)`
//!   还是落在旧版本上 —— 口径不会因为改了配置就悄悄变。
//!
//! # 切换日的三条护栏（`409` + `66113`，报文都给建议日）
//!
//! | # | 规则 | 为什么 |
//! |---|---|---|
//! | 1 | 必须是某月 1 日 | 一期整期按一种方法算；期内切换 = 同一期前后半段两种方法 = 要分段重算（二期） |
//! | 2 | 不许早于「今天所在期间的下月 1 日」 | 不许追溯：本期已经开始了，切它就得重算本期 |
//! | 3 | 切换日所属期间不许已有成本批次 | 该期间已经有归集 / 计算 / 结算数据，切它就是改写已算过的口径 |
//!
//! 「已经切过一次」不复用 `66113`：那是「这个日子不能切」；同一个日子上已经有一条
//! 启用中版本，撞的是 `66102`（同范围生效期重叠 / 同一切换日只能切一次）。
//!
//! 目标方法是二期 / 三期才实现的方法（`VARIETY` / `STANDARD`）时**允许预演**，
//! 但 `engine_ready = false` 并在 `warnings` 里说清楚「切了以后新期间点计算会 409 + 66101」；
//! 真执行时要显式 `confirm_unimplemented = true`（不给就返回 `66101` 的说明报文）——
//! 一期只有分批法，静默把一个「算不出来」的方法改成默认口径等于让下个月没法结算。
//!
//! # 本模块的边界
//!
//! 纯逻辑（不连库）：期间边界 / 护栏 / 版本时间线的「切换前 vs 切换后」推演 / 影响面清单。
//! 取数（配置行、成本期间台账）在 `cost_method_switch_model`，事务与 HTTP 入口在
//! `cost_method_switch_handler`。

use nik::prelude::chrono::{Datelike, Duration, NaiveDate};
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::cost::cost_method_dto::{
    self as method_dto, METHOD_JOB, MethodConfigView, SCOPE_GLOBAL, SCOPE_OBJECT, SOURCE_DEFAULT, SOURCE_GLOBAL,
    SOURCE_OBJECT, STATUS_ENABLED,
};
use cloudpivot_kernel::shared::error_code;

// ==================== 错误码 ====================

/// `409`：切换日不合法（`66113`，M5-31 新增）。
pub const CODE_SWITCH_GUARD: i32 = error_code::ERP_COST_METHOD_SWITCH_GUARD;
/// `409`：同一切换日已有一条启用中版本（复用 M5-01 的 `66102`）。
pub const CODE_OVERLAP: i32 = method_dto::CODE_OVERLAP;
/// `409`：目标方法在一期引擎里没实现（复用 M5-01 的 `66101`）。
pub const CODE_UNIMPLEMENTED: i32 = method_dto::CODE_UNIMPLEMENTED;

// ==================== 动作常量 ====================

/// 切换动作之一：把旧版本收口到切换日前一天。
pub const OP_CLOSE: &str = "CLOSE";
/// 切换动作之二：新增一条从切换日生效的版本。
pub const OP_INSERT: &str = "INSERT";

/// 预演里「将来」展示的期间数（含切换月本身）。
pub const UPCOMING_MONTHS: usize = 3;

// ==================== 期间边界（纯函数） ====================

/// 日期 → 成本期间码（`2026-12-01` → `2026-12`）。
pub fn month_of(date: NaiveDate) -> String {
    format!("{:04}-{:02}", date.year(), date.month())
}

/// 期间码 → 该期间的起止日（含两端；都是自然月，一期口径）。
///
/// 形状非法（`2026-13` / `2026/11` / `26-11` / 空串）返回 `None`：
/// 库层 `ck_biz_cost_period_code` 已保证 `^\d{4}-(0[1-9]|1[0-2])$`，
/// 这里的严格解析是给单测与「台账里混进脏数据」兜底的。
pub fn month_bounds(period_code: &str) -> Option<(NaiveDate, NaiveDate)> {
    let bytes = period_code.as_bytes();
    if bytes.len() != 7 || bytes[4] != b'-' {
        return None;
    }
    let year = period_code.get(0..4)?.parse::<i32>().ok()?;
    let month = period_code.get(5..7)?.parse::<u32>().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    let from = NaiveDate::from_ymd_opt(year, month, 1)?;
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year.checked_add(1)?, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)?
    };
    Some((from, next - Duration::days(1)))
}

/// 某天所在期间的下一个期间首日（`2026-11-20` → `2026-12-01`）。
pub fn next_month_start(date: NaiveDate) -> NaiveDate {
    let (year, month) = (date.year(), date.month());
    if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1).expect("年份 +1 不会溢出可表示范围")
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1).expect("月 +1 不会溢出可表示范围")
    }
}

/// 期间码 + `offset` 个月之后的期间首日（`offset = 0` 就是该期间首日）。
pub fn shift_month_start(period_code: &str, offset: u32) -> Option<NaiveDate> {
    let (from, _) = month_bounds(period_code)?;
    let mut date = from;
    for _ in 0..offset {
        date = next_month_start(date);
    }
    Some(date)
}

// ==================== 输入数据（model 投影） ====================

/// 一个成本期间的口径台账行（`biz_cost_period` 按期间聚合，model 层给）。
#[derive(Debug, Clone, PartialEq)]
pub struct PeriodLedger {
    /// 期间码 `yyyy-MM`。
    pub period_code: String,
    /// 该期间现有批次数（含红冲 / 作废留痕行；**不区分状态**，报文要能如实说）。
    pub batch_count: i64,
    /// 现有批次的状态（去重排序；空 = 历史上没算过）。
    pub statuses: Vec<String>,
}

impl PeriodLedger {
    /// 该期间是否已有「活批次」（`DRAFT` 也算：草稿里已经有归集数据，改口径就要重算）。
    ///
    /// `REVERSED` / `CANCELLED` 是审计留痕、不占期间位置（V0373 唯一键同口径）：
    /// 只剩这两类行的期间，切它不需要重算 —— 但本卡仍按「保守」处理见
    /// [`Self::blocks_switch`]。
    pub fn blocks_switch(&self) -> bool {
        self.statuses
            .iter()
            .any(|status| status != "REVERSED" && status != "CANCELLED")
    }

    /// 状态的中文/英文混排（报文用）：`DRAFT,SETTLED`。
    pub fn status_text(&self) -> String {
        if self.statuses.is_empty() {
            "无".to_string()
        } else {
            self.statuses.join(",")
        }
    }
}

/// 切换目标（预演与执行共用的纯数据）。
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchTarget {
    /// 切换日（护栏要求：某月 1 日、不早于下月 1 日、该期间无批次）。
    pub switch_on: NaiveDate,
    /// 范围（`GLOBAL` / `COST_OBJECT`）。
    pub scope_type: String,
    /// 成本对象（`GLOBAL` 时为 `None`）。
    pub scope_id: Option<Uuid>,
    /// 目标方法（`JOB` / `VARIETY` / `STANDARD`）。
    pub to_method: String,
}

// ==================== 预演结果（纯数据） ====================

/// 预演里的一笔写入（界面照着它就能手工复现，执行接口做的就是这两笔）。
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchOp {
    /// `CLOSE` / `INSERT`。
    pub action: String,
    /// 被收口的那条版本（`INSERT` 时为空 —— 新行还没 id）。
    pub config_id: Option<Uuid>,
    /// 方法。
    pub method: String,
    /// 生效日。
    pub effective_from: NaiveDate,
    /// 停止日（`CLOSE` 时 = 切换日前一天；`INSERT` 时为 `None` = 长期有效）。
    pub effective_to: Option<NaiveDate>,
    /// 一句话说明（界面直接显示）。
    pub reason: String,
}

/// 一个期间在「切换前 / 切换后」的方法对照。
#[derive(Debug, Clone, PartialEq)]
pub struct PeriodMethodRow {
    /// 期间码。
    pub period_code: String,
    /// 期间起。
    pub period_from: NaiveDate,
    /// 期间止。
    pub period_to: NaiveDate,
    /// 批次数（存量 = 现有批次；将来期间 = 0）。
    pub batch_count: i64,
    /// 批次状态。
    pub statuses: Vec<String>,
    /// 切换前（当前配置时间线）该期间按什么方法算。
    pub method_before: String,
    /// 切换后（收口 + 新增之后）该期间按什么方法算。
    pub method_after: String,
    /// 切换是否会让这个期间重算（**永远是 `false`**，是结论不是选项）。
    pub recalculated: bool,
    /// 一句话说明。
    pub note: String,
}

impl PeriodMethodRow {
    /// 方法是否一变都没变（存量期间的自检判据）。
    pub fn unchanged(&self) -> bool {
        self.method_before == self.method_after && !self.recalculated
    }
}

/// 对象级覆盖：这些成本对象有自己的方法配置，全公司默认切换**不影响**它们。
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectOverride {
    /// 配置行 id。
    pub config_id: Uuid,
    /// 成本对象 id。
    pub scope_id: Uuid,
    /// 方法。
    pub method: String,
    /// 生效日。
    pub effective_from: NaiveDate,
}

/// 切换预演（纯数据，出参 DTO 见 [`SwitchPlanRes`]）。
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchPlan {
    /// 今天（CST）。
    pub today: NaiveDate,
    /// 切换日（用户指定或建议值）。
    pub switch_on: NaiveDate,
    /// 建议切换日（护栏 2 / 3 触发时用户照它改就能过）。
    pub suggested_switch_on: NaiveDate,
    /// 切换日所属期间。
    pub switch_period: String,
    /// 范围。
    pub scope_type: String,
    /// 成本对象。
    pub scope_id: Option<Uuid>,
    /// 切换前该范围在切换日前一天的方法。
    pub from_method: String,
    /// 切换前方法的来源（`COST_OBJECT` / `GLOBAL` / `DEFAULT`）。
    pub from_source: String,
    /// 目标方法。
    pub to_method: String,
    /// 目标方法是否已实现（一期只有分批法）。
    pub to_method_implemented: bool,
    /// 切换后新期间能不能算（= 目标方法已实现）。
    pub engine_ready: bool,
    /// 存量期间（切换日之前的期间）：**不重算**。
    pub history: Vec<PeriodMethodRow>,
    /// 已有批次的将来期间（切换日之后；正常情况为空）。
    pub future: Vec<PeriodMethodRow>,
    /// 切换月及之后 [`UPCOMING_MONTHS`] 个月（尚无批次的期间）。
    pub upcoming: Vec<PeriodMethodRow>,
    /// 会被对象级配置覆盖的成本对象（全公司默认切换对它们无效）。
    pub object_overrides: Vec<ObjectOverride>,
    /// 要写的两笔。
    pub ops: Vec<SwitchOp>,
    /// 存量期间的批次总数（零改动）。
    pub history_batches: i64,
    /// 给界面的告警 / 说明。
    pub warnings: Vec<String>,
}

// ==================== 护栏（错误出口） ====================

/// `409` + `66113`：切换日不是期间首日。
pub fn guard_not_period_start(switch_on: NaiveDate, suggested: NaiveDate) -> ApiError {
    ApiError::new(
        409,
        CODE_SWITCH_GUARD,
        format!(
            "切换日 {switch_on} 不在期间边界上：成本期间是自然月，一期引擎**整期只按一种方法算**\
             （ARCH-9 D-3），期间中间切换意味着同一期前半段旧方法、后半段新方法 —— 那要分段重算，一期不做。\n\
             请把切换日改成某月 1 日，建议 {suggested}"
        ),
    )
}

/// `409` + `66113`：切换日追溯 / 落在已开始的期间。
pub fn guard_retroactive(switch_on: NaiveDate, suggested: NaiveDate, today: NaiveDate) -> ApiError {
    ApiError::new(
        409,
        CODE_SWITCH_GUARD,
        format!(
            "切换日 {switch_on} 太早（今天 {today}）：切换只允许从**还没开始的期间**起生效，\
             否则本期已归集的料工费就得分段重算。建议切换日 {suggested}（下个期间首日）。\n\
             历史期间的口径不会被改写：切换前已算过的期间继续按原方法，不重算、不出新凭证"
        ),
    )
}

/// `409` + `66113`：切换日所属期间已经有成本批次。
pub fn guard_period_has_batches(switch_on: NaiveDate, period: &PeriodLedger, suggested: NaiveDate) -> ApiError {
    ApiError::new(
        409,
        CODE_SWITCH_GUARD,
        format!(
            "切换日 {switch_on} 所属期间 {} 已经有 {} 个成本批次（状态 {}）：切它就会改写已经算过的期间口径，\
             必须重算 —— 与「历史期间不重算」的口径冲突。\n\
             请把切换日推到该期间之后的第一个期间首日，建议 {suggested}",
            period.period_code,
            period.batch_count,
            period.status_text()
        ),
    )
}

/// `409` + `66102`：同一切换日已经有一条启用中版本（同一范围）。
pub fn already_switched(scope_name: &str, switch_on: NaiveDate, hit: &MethodConfigView) -> ApiError {
    ApiError::new(
        409,
        CODE_OVERLAP,
        format!(
            "{scope_name} 从 {switch_on} 起已经有一条启用中的方法配置（{}，生效期 {}）：\
             同一个切换日只能切一次。要改这一段的口径，请直接改那条版本（已生效过的方法 / 生效日不许改，\
             只能改停止日 / 状态 / 备注）；要再往后换方法，请选一个更晚的期间首日",
            method_dto::method_label(&hit.method),
            method_dto::range_text(hit.effective_from, hit.effective_to)
        ),
    )
}

/// `409` + `66101`：目标方法在一期引擎里没实现，执行需要显式确认。
pub fn unimplemented_target_needs_confirm(scope_name: &str, method: &str, switch_on: NaiveDate) -> ApiError {
    ApiError::new(
        409,
        CODE_UNIMPLEMENTED,
        format!(
            "目标方法「{}」在一期引擎里还没实现（ARCH-9 D-3：一期只做分批法）：从 {switch_on} 起 {scope_name} \
             会按它算，点「计算成本」会被拒绝（66101），该期间也就无法结算。\n\
             历史期间不受影响（继续按原方法，不重算），但新期间会算不出来。\n\
             确实要先把口径改成「{}」（等二期 / 三期引擎接上就能算）请显式确认：\
             请求体加 \"confirm_unimplemented\": true",
            method_dto::method_label(method),
            method_dto::method_label(method)
        ),
    )
}

// ==================== 目标解析 ====================

/// 解析切换目标（GET 预演与 POST 执行共用）。
///
/// `switch_on_raw` 为空时用 `fallback_on`（handler 传「建议切换日」）；`to_method` 必填。
/// 范围成对规则与 M5-01 的 `spec_from_create` 逐字一致（`GLOBAL` 不许带对象 /
/// `COST_OBJECT` 必须带对象），错误码沿用 `66103`。
pub fn parse_target(
    switch_on_raw: Option<&str>,
    to_method_raw: Option<&str>,
    scope_raw: Option<&str>,
    scope_id_raw: Option<&str>,
    fallback_on: NaiveDate,
) -> std::result::Result<SwitchTarget, ApiError> {
    let switch_on = match switch_on_raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => fallback_on,
        Some(raw) => crate::business::erp::cost::cost_labor_dto::parse_date(raw, "switch_on")?,
    };
    let to_method = method_dto::normalize_method(to_method_raw)?;
    let scope_type = match scope_raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => SCOPE_GLOBAL.to_string(),
        Some(raw) => method_dto::normalize_scope(Some(raw))?,
    };
    let scope_id = match scope_type.as_str() {
        SCOPE_OBJECT => {
            let raw = scope_id_raw
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    method_dto::scope_invalid(
                        "scope = COST_OBJECT 时必须给 scope_id（给哪个成本对象换方法）；\
                         要换全公司默认口径请用 scope = GLOBAL",
                    )
                })?;
            Some(crate::business::erp::cost::cost_labor_dto::parse_uuid(raw, "scope_id")?)
        }
        _ => {
            if scope_id_raw.map(str::trim).is_some_and(|text| !text.is_empty()) {
                return Err(method_dto::scope_invalid(
                    "scope = GLOBAL 时不能给 scope_id（全公司默认没有具体对象）；\
                     要给某个成本对象换方法请用 scope = COST_OBJECT",
                ));
            }
            None
        }
    };
    Ok(SwitchTarget {
        switch_on,
        scope_type,
        scope_id,
        to_method,
    })
}

// ==================== 建议切换日 ====================

/// 建议切换日 = 「今天所在期间之后的下一个期间首日」与「现有最大期间之后的下一个期间首日」取晚者。
///
/// 前者对应「本期已开始、不许追溯」；后者对应「将来期间已经预先算过」。
pub fn suggested_switch_on(today: NaiveDate, periods: &[PeriodLedger]) -> NaiveDate {
    let mut candidate = next_month_start(today);
    for period in periods {
        if let Some((_, to)) = month_bounds(&period.period_code) {
            let next = next_month_start(to);
            if next > candidate {
                candidate = next;
            }
        }
    }
    candidate
}

// ==================== 预演（核心纯函数） ====================

/// 生成切换预演：校验护栏 → 推演「切换前 / 切换后」每个期间的方法 → 给出两笔写。
///
/// # 异常情况
///
/// - 切换日不是某月 1 日 ⇒ `409` + `66113`
/// - 切换日早于「下月 1 日」⇒ `409` + `66113`
/// - 切换日所属期间已有成本批次 ⇒ `409` + `66113`
/// - 同范围在切换日已有一条启用中版本 ⇒ `409` + `66102`
pub fn build_plan(
    rows: &[MethodConfigView],
    periods: &[PeriodLedger],
    target: &SwitchTarget,
    today: NaiveDate,
) -> std::result::Result<SwitchPlan, ApiError> {
    let suggested = suggested_switch_on(today, periods);
    let switch_on = target.switch_on;

    // 护栏 1：必须是期间首日
    if switch_on.day() != 1 {
        return Err(guard_not_period_start(switch_on, suggested));
    }
    // 护栏 2：不许追溯 / 落在本期
    if switch_on < next_month_start(today) {
        return Err(guard_retroactive(switch_on, suggested, today));
    }
    // 护栏 3：切换日所属期间已有成本批次
    let switch_period = month_of(switch_on);
    if let Some(hit) = periods
        .iter()
        .find(|period| period.period_code == switch_period && period.blocks_switch())
    {
        return Err(guard_period_has_batches(switch_on, hit, suggested));
    }
    // 同一切换日不许重复切：先把「切换日前一天仍在生效的那条」认出来（它是被收口的旧版，
    // 不算冲突），剩下任何与 `[切换日, ∞)` 相交的启用版本都是冲突（库层保证启用版本不重叠，
    // 所以实际只可能是「已经有一条从切换日起生效的版本」）。
    let prev_day = switch_on - Duration::days(1);
    // 旧版 = 同范围、启用中、覆盖切换日前一天的那条（多条时取生效日最晚）
    let closing = rows
        .iter()
        .filter(|row| {
            row.status == STATUS_ENABLED
                && same_scope(row, target)
                && row.covers(prev_day)
                && row.effective_from <= prev_day
        })
        .max_by(|a, b| a.effective_from.cmp(&b.effective_from).then_with(|| a.id.cmp(&b.id)))
        .cloned();
    if let Some(hit) = rows.iter().find(|row| {
        row.status == STATUS_ENABLED
            && same_scope(row, target)
            && Some(row.id) != closing.as_ref().map(|old| old.id)
            && row.effective_to.is_none_or(|to| to >= switch_on)
    }) {
        return Err(already_switched(&scope_name(target), switch_on, hit));
    }

    let from = method_dto::resolve(rows, target.scope_id, prev_day);
    let to_method_implemented = method_dto::method_implemented(&target.to_method);

    // 两笔写
    let mut ops = Vec::new();
    if let Some(old) = &closing {
        ops.push(SwitchOp {
            action: OP_CLOSE.to_string(),
            config_id: Some(old.id),
            method: old.method.clone(),
            effective_from: old.effective_from,
            effective_to: Some(prev_day),
            reason: format!(
                "把旧版本（{}，{}）的停止日收到 {prev_day}：切换日之前的日子继续按它算",
                method_dto::method_label(&old.method),
                method_dto::range_text(old.effective_from, old.effective_to)
            ),
        });
    }
    ops.push(SwitchOp {
        action: OP_INSERT.to_string(),
        config_id: None,
        method: target.to_method.clone(),
        effective_from: switch_on,
        effective_to: None,
        reason: format!(
            "新增一条从 {switch_on} 起生效的版本（{}，长期有效）：从这一天起算的期间按新方法",
            method_dto::method_label(&target.to_method)
        ),
    });

    // 模拟「切换后」的配置时间线（旧版收口 + 新行）
    let mut after = rows.to_vec();
    if let Some(old) = &closing {
        for row in after.iter_mut() {
            if row.id == old.id {
                row.effective_to = Some(prev_day);
            }
        }
    }
    after.push(MethodConfigView {
        id: Uuid::nil(),
        method: target.to_method.clone(),
        scope_type: target.scope_type.clone(),
        scope_id: target.scope_id,
        effective_from: switch_on,
        effective_to: None,
        status: STATUS_ENABLED,
    });

    // 期间分类：存量（切换日之前）/ 将来的已有批次期间 / 切换月起 UPCOMING_MONTHS 个月
    let mut history = Vec::new();
    let mut future = Vec::new();
    let mut upcoming = Vec::new();
    let mut ordered = periods.to_vec();
    ordered.sort_by(|a, b| a.period_code.cmp(&b.period_code));
    for period in &ordered {
        let Some((from_date, to_date)) = month_bounds(&period.period_code) else {
            continue; // 脏期间码：不参与推演（库层 CHECK 已挡；这里只防御）
        };
        let before = method_dto::resolve(rows, target.scope_id, to_date).method;
        let after_method = method_dto::resolve(&after, target.scope_id, to_date).method;
        if to_date < switch_on {
            history.push(PeriodMethodRow {
                period_code: period.period_code.clone(),
                period_from: from_date,
                period_to: to_date,
                batch_count: period.batch_count,
                statuses: period.statuses.clone(),
                method_before: before.clone(),
                method_after: after_method,
                recalculated: false,
                note: format!(
                    "存量期间：口径已经定了（{}），切换只改 {switch_on} 之后的日子，这一期不重算、凭证不动",
                    method_dto::method_label(&before)
                ),
            });
        } else {
            let note = if period.batch_count > 0 {
                format!(
                    "将来期间但已经有 {} 个批次（状态 {}）：切换不改它的结果，下次重算会按新方法 —— \
                     若这些数据已经对外出数，请把切换日推到它们之后",
                    period.batch_count,
                    period.status_text()
                )
            } else {
                format!(
                    "切换后按「{}」算；一期引擎整期一种方法，期间内不会再切",
                    method_dto::method_label(&after_method)
                )
            };
            future.push(PeriodMethodRow {
                period_code: period.period_code.clone(),
                period_from: from_date,
                period_to: to_date,
                batch_count: period.batch_count,
                statuses: period.statuses.clone(),
                method_before: before,
                method_after: after_method,
                recalculated: false,
                note,
            });
        }
    }
    for offset in 0..UPCOMING_MONTHS {
        let Some(month_start) = shift_month_start(&switch_period, u32::try_from(offset).unwrap_or(0)) else {
            break;
        };
        let code = month_of(month_start);
        if ordered.iter().any(|period| period.period_code == code) {
            continue; // 已有台账行，上面已经列过
        }
        let Some((from_date, to_date)) = month_bounds(&code) else {
            break;
        };
        let before = method_dto::resolve(rows, target.scope_id, to_date).method;
        let after_method = method_dto::resolve(&after, target.scope_id, to_date).method;
        upcoming.push(PeriodMethodRow {
            period_code: code,
            period_from: from_date,
            period_to: to_date,
            batch_count: 0,
            statuses: Vec::new(),
            method_before: before,
            method_after: after_method,
            recalculated: false,
            note: "还没有算过（无批次）：从这一期起按新方法，历史期间不受影响".to_string(),
        });
    }

    // 对象级覆盖：全公司默认切换时，这些对象仍按自己的方法算
    let object_overrides = if target.scope_type == SCOPE_GLOBAL {
        rows.iter()
            .filter(|row| {
                row.scope_type == SCOPE_OBJECT
                    && row.status == STATUS_ENABLED
                    && row.covers(switch_on)
                    && row.scope_id.is_some()
            })
            .map(|row| ObjectOverride {
                config_id: row.id,
                scope_id: row.scope_id.expect("已过滤 is_some"),
                method: row.method.clone(),
                effective_from: row.effective_from,
            })
            .collect()
    } else {
        Vec::new()
    };

    let history_batches = history.iter().map(|row| row.batch_count).sum();

    // 告警（界面上直接显示；不是错误 —— 执行与否由用户决定）
    let mut warnings = Vec::new();
    if !to_method_implemented {
        warnings.push(format!(
            "目标方法「{}」在一期引擎里还没实现：切换后新期间的「计算成本」会被拒绝（66101），该期间无法结算。\
             历史期间不受影响（继续按原方法，不重算）。执行时需要显式确认 confirm_unimplemented = true",
            method_dto::method_label(&target.to_method)
        ));
    }
    if closing.is_none() {
        warnings.push(format!(
            "{switch_on} 之前该范围没有任何生效中的配置（当前按代码兜底 {} 算）：切换只新增一条版本，不需要收口旧版",
            method_dto::method_label(METHOD_JOB)
        ));
    }
    if !history.is_empty() {
        warnings.push(format!(
            "存量期间 {} 个、批次 {} 个：口径保持不变，**不重算**、不出新凭证（本接口不碰结果表）",
            history.len(),
            history_batches
        ));
    }
    if !object_overrides.is_empty() {
        warnings.push(format!(
            "{} 个成本对象有自己的方法配置（对象级优先于全公司默认）：这次切换不影响它们，它们仍按自己的版本算",
            object_overrides.len()
        ));
    }

    Ok(SwitchPlan {
        today,
        switch_on,
        suggested_switch_on: suggested,
        switch_period,
        scope_type: target.scope_type.clone(),
        scope_id: target.scope_id,
        from_method: from.method,
        from_source: from.source,
        to_method: target.to_method.clone(),
        to_method_implemented,
        engine_ready: to_method_implemented,
        history,
        future,
        upcoming,
        object_overrides,
        ops,
        history_batches,
        warnings,
    })
}

/// 切换后要插入的新版本规格（handler 落库用）。
pub fn new_version_spec(plan: &SwitchPlan, remark: Option<String>) -> method_dto::CostMethodSpec {
    method_dto::CostMethodSpec {
        method: plan.to_method.clone(),
        scope_type: plan.scope_type.clone(),
        scope_id: plan.scope_id,
        effective_from: plan.switch_on,
        effective_to: None,
        status: STATUS_ENABLED,
        remark,
    }
}

/// 同范围判定（`GLOBAL` 看 `scope_id` 也为 `None`，避免与对象级串味）。
fn same_scope(row: &MethodConfigView, target: &SwitchTarget) -> bool {
    row.scope_type == target.scope_type && row.scope_id == target.scope_id
}

/// 范围展示名（报文用；对象号由 handler 另行补，这里给通用说法）。
pub fn scope_name(target: &SwitchTarget) -> String {
    match (target.scope_type.as_str(), target.scope_id) {
        (SCOPE_GLOBAL, _) => "全公司默认".to_string(),
        (_, Some(id)) => format!("成本对象 {id}"),
        _ => "该范围".to_string(),
    }
}

/// 来源中文名（`COST_OBJECT` / `GLOBAL` / `DEFAULT` → 界面文案）。
pub fn source_label(source: &str) -> &'static str {
    match source {
        SOURCE_OBJECT => "成本对象级配置",
        SOURCE_GLOBAL => "全公司默认配置",
        SOURCE_DEFAULT => "代码兜底（分批法）",
        _ => "未知来源",
    }
}

// ==================== 出参 DTO ====================

/// 生效方法查询入参（`GET /business/erp/cost-method/switch-plan`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostMethodSwitchQuery {
    /// 切换日（`YYYY-MM-DD`，缺省 = 建议切换日）。
    pub switch_on: Option<String>,
    /// 目标方法（必填：`JOB` / `VARIETY` / `STANDARD`）。
    pub to_method: Option<String>,
    /// 范围（缺省 `GLOBAL`）。
    pub scope: Option<String>,
    /// 成本对象（`scope = COST_OBJECT` 时必填）。
    pub scope_id: Option<String>,
}

/// 执行切换入参（`POST /business/erp/cost-method/switch`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CostMethodSwitchReq {
    /// 切换日（`YYYY-MM-DD`，缺省 = 建议切换日）。
    pub switch_on: Option<String>,
    /// 目标方法（必填）。
    pub to_method: Option<String>,
    /// 范围（缺省 `GLOBAL`）。
    pub scope: Option<String>,
    /// 成本对象（`scope = COST_OBJECT` 时必填）。
    pub scope_id: Option<String>,
    /// 备注（写进新版本的 `remark`）。
    pub remark: Option<String>,
    /// 目标方法一期未实现时的显式确认（缺省 `false`）。
    #[serde(default)]
    pub confirm_unimplemented: bool,
}

/// 预演出参。
#[derive(Debug, Clone, Serialize)]
pub struct SwitchPlanRes {
    /// 今天（CST）。
    pub today: String,
    /// 切换日。
    pub switch_on: String,
    /// 建议切换日。
    pub suggested_switch_on: String,
    /// 切换日所属期间。
    pub switch_period: String,
    /// 范围。
    pub scope: String,
    /// 范围中文名。
    pub scope_label: String,
    /// 成本对象 id。
    pub scope_id: Option<String>,
    /// 成本对象号。
    pub scope_object_no: Option<String>,
    /// 切换前方法。
    pub from_method: String,
    /// 切换前方法中文名。
    pub from_method_label: String,
    /// 切换前方法的来源。
    pub from_source: String,
    /// 切换前来源中文名。
    pub from_source_label: String,
    /// 目标方法。
    pub to_method: String,
    /// 目标方法中文名。
    pub to_method_label: String,
    /// 目标方法是否已实现。
    pub to_method_implemented: bool,
    /// 切换后新期间能不能算。
    pub engine_ready: bool,
    /// 存量期间（不重算）。
    pub history: Vec<PeriodMethodRes>,
    /// 已有批次的将来期间。
    pub future: Vec<PeriodMethodRes>,
    /// 切换月起 3 个月（尚无批次）。
    pub upcoming: Vec<PeriodMethodRes>,
    /// 对象级覆盖。
    pub object_overrides: Vec<ObjectOverrideRes>,
    /// 要写的两笔。
    pub ops: Vec<SwitchOpRes>,
    /// 存量期间批次总数。
    pub history_batches: i64,
    /// 存量期间零改动的结论（恒 `true`，给界面一句可以写死的话）。
    pub history_untouched: bool,
    /// 告警。
    pub warnings: Vec<String>,
}

/// 期间对照出参。
#[derive(Debug, Clone, Serialize)]
pub struct PeriodMethodRes {
    /// 期间码。
    pub period_code: String,
    /// 期间起。
    pub period_from: String,
    /// 期间止。
    pub period_to: String,
    /// 批次数。
    pub batch_count: i64,
    /// 批次状态。
    pub statuses: Vec<String>,
    /// 切换前方法。
    pub method_before: String,
    /// 切换前方法中文名。
    pub method_before_label: String,
    /// 切换后方法。
    pub method_after: String,
    /// 切换后方法中文名。
    pub method_after_label: String,
    /// 是否重算（恒 `false`）。
    pub recalculated: bool,
    /// 方法是否没变。
    pub unchanged: bool,
    /// 说明。
    pub note: String,
}

/// 对象级覆盖出参。
#[derive(Debug, Clone, Serialize)]
pub struct ObjectOverrideRes {
    /// 配置行 id。
    pub config_id: String,
    /// 成本对象 id。
    pub scope_id: String,
    /// 成本对象号。
    pub scope_object_no: Option<String>,
    /// 方法。
    pub method: String,
    /// 方法中文名。
    pub method_label: String,
    /// 生效日。
    pub effective_from: String,
}

/// 写入动作出参。
#[derive(Debug, Clone, Serialize)]
pub struct SwitchOpRes {
    /// `CLOSE` / `INSERT`。
    pub action: String,
    /// 被收口的版本 id（`INSERT` 时为空）。
    pub config_id: Option<String>,
    /// 方法。
    pub method: String,
    /// 方法中文名。
    pub method_label: String,
    /// 生效日。
    pub effective_from: String,
    /// 停止日。
    pub effective_to: Option<String>,
    /// 说明。
    pub reason: String,
}

/// 执行结果出参。
#[derive(Debug, Clone, Serialize)]
pub struct SwitchResultRes {
    /// 预演（用户点「执行」前看到的就是它）。
    pub plan: SwitchPlanRes,
    /// 被收口的旧版本（没有旧版时为空）。
    pub closed: Option<SwitchOpRes>,
    /// 新建的版本 id。
    pub created_config_id: String,
    /// 新建版本的生效日。
    pub created_effective_from: String,
    /// 一句话结论。
    pub message: String,
}

/// 预演纯数据 → 出参（`object_nos` = 成本对象 id → 对象号，handler 一次查好）。
pub fn plan_res(
    plan: &SwitchPlan,
    scope_label: &str,
    scope_object_no: Option<&str>,
    object_nos: &std::collections::HashMap<Uuid, String>,
) -> SwitchPlanRes {
    let period_res = |row: &PeriodMethodRow| PeriodMethodRes {
        period_code: row.period_code.clone(),
        period_from: row.period_from.to_string(),
        period_to: row.period_to.to_string(),
        batch_count: row.batch_count,
        statuses: row.statuses.clone(),
        method_before: row.method_before.clone(),
        method_before_label: method_dto::method_label(&row.method_before).to_string(),
        method_after: row.method_after.clone(),
        method_after_label: method_dto::method_label(&row.method_after).to_string(),
        recalculated: row.recalculated,
        unchanged: row.unchanged(),
        note: row.note.clone(),
    };
    let op_res = |op: &SwitchOp| SwitchOpRes {
        action: op.action.clone(),
        config_id: op.config_id.map(|id| id.to_string()),
        method: op.method.clone(),
        method_label: method_dto::method_label(&op.method).to_string(),
        effective_from: op.effective_from.to_string(),
        effective_to: op.effective_to.map(|date| date.to_string()),
        reason: op.reason.clone(),
    };
    SwitchPlanRes {
        today: plan.today.to_string(),
        switch_on: plan.switch_on.to_string(),
        suggested_switch_on: plan.suggested_switch_on.to_string(),
        switch_period: plan.switch_period.clone(),
        scope: plan.scope_type.clone(),
        scope_label: scope_label.to_string(),
        scope_id: plan.scope_id.map(|id| id.to_string()),
        scope_object_no: scope_object_no.map(str::to_string),
        from_method: plan.from_method.clone(),
        from_method_label: method_dto::method_label(&plan.from_method).to_string(),
        from_source: plan.from_source.clone(),
        from_source_label: source_label(&plan.from_source).to_string(),
        to_method: plan.to_method.clone(),
        to_method_label: method_dto::method_label(&plan.to_method).to_string(),
        to_method_implemented: plan.to_method_implemented,
        engine_ready: plan.engine_ready,
        history: plan.history.iter().map(period_res).collect(),
        future: plan.future.iter().map(period_res).collect(),
        upcoming: plan.upcoming.iter().map(period_res).collect(),
        object_overrides: plan
            .object_overrides
            .iter()
            .map(|row| ObjectOverrideRes {
                config_id: row.config_id.to_string(),
                scope_id: row.scope_id.to_string(),
                scope_object_no: object_nos.get(&row.scope_id).cloned(),
                method: row.method.clone(),
                method_label: method_dto::method_label(&row.method).to_string(),
                effective_from: row.effective_from.to_string(),
            })
            .collect(),
        ops: plan.ops.iter().map(op_res).collect(),
        history_batches: plan.history_batches,
        history_untouched: plan.history.iter().all(PeriodMethodRow::unchanged),
        warnings: plan.warnings.clone(),
    }
}

/// 单笔写入的出参（执行结果用）。
pub fn op_res(op: &SwitchOp) -> SwitchOpRes {
    SwitchOpRes {
        action: op.action.clone(),
        config_id: op.config_id.map(|id| id.to_string()),
        method: op.method.clone(),
        method_label: method_dto::method_label(&op.method).to_string(),
        effective_from: op.effective_from.to_string(),
        effective_to: op.effective_to.map(|date| date.to_string()),
        reason: op.reason.clone(),
    }
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::cost::cost_method_dto::{METHOD_STANDARD, METHOD_VARIETY};

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期")
    }

    fn view(
        id: u128,
        method: &str,
        scope_type: &str,
        scope_id: Option<Uuid>,
        from: &str,
        to: Option<&str>,
        status: i16,
    ) -> MethodConfigView {
        MethodConfigView {
            id: Uuid::from_u128(id),
            method: method.to_string(),
            scope_type: scope_type.to_string(),
            scope_id,
            effective_from: date(from),
            effective_to: to.map(date),
            status,
        }
    }

    /// 种子行：全公司默认分批法，1970-01-01 起长期有效（V0459）。
    fn seed_row() -> MethodConfigView {
        view(1, METHOD_JOB, SCOPE_GLOBAL, None, "1970-01-01", None, STATUS_ENABLED)
    }

    fn ledger(period_code: &str, batch_count: i64, statuses: &[&str]) -> PeriodLedger {
        PeriodLedger {
            period_code: period_code.to_string(),
            batch_count,
            statuses: statuses.iter().map(|status| (*status).to_string()).collect(),
        }
    }

    fn target(on: &str, method: &str) -> SwitchTarget {
        SwitchTarget {
            switch_on: date(on),
            scope_type: SCOPE_GLOBAL.to_string(),
            scope_id: None,
            to_method: method.to_string(),
        }
    }

    // ---------- 期间边界 ----------

    /// 月份边界：闰年 2 月、12 月跨年、非法形状。
    #[test]
    fn month_bounds_cover_leap_and_year_end() {
        assert_eq!(
            month_bounds("2026-02").expect("2 月"),
            (date("2026-02-01"), date("2026-02-28"))
        );
        assert_eq!(
            month_bounds("2024-02").expect("闰年 2 月"),
            (date("2024-02-01"), date("2024-02-29"))
        );
        assert_eq!(
            month_bounds("2026-12").expect("12 月"),
            (date("2026-12-01"), date("2026-12-31"))
        );
        for bad in ["2026-13", "2026-00", "2026/11", "26-11", "", "2026-1"] {
            assert!(month_bounds(bad).is_none(), "{bad} 应当解析失败");
        }
    }

    /// 下个期间首日：跨年正确，且不受当天是几号影响。
    #[test]
    fn next_month_start_handles_year_end() {
        assert_eq!(next_month_start(date("2026-11-20")), date("2026-12-01"));
        assert_eq!(next_month_start(date("2026-12-31")), date("2027-01-01"));
        assert_eq!(next_month_start(date("2026-11-01")), date("2026-12-01"));
        assert_eq!(month_of(date("2026-12-31")), "2026-12");
        assert_eq!(shift_month_start("2026-11", 2), Some(date("2027-01-01")));
    }

    // ---------- 护栏 ----------

    /// 护栏 1：不是期间首日 ⇒ 66113，报文给建议日。
    #[test]
    fn switch_on_must_be_a_period_start() {
        let rows = [seed_row()];
        let err = build_plan(&rows, &[], &target("2026-12-15", METHOD_VARIETY), date("2026-11-20"))
            .expect_err("期间中间不许切");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, CODE_SWITCH_GUARD);
        assert!(err.msg.contains("2026-12-01"), "必须给建议日：{}", err.msg);
        assert!(err.msg.contains("整期只按一种方法"), "{}", err.msg);
    }

    /// 护栏 2：追溯 / 本期 ⇒ 66113；下月 1 日是允许的最早切换日。
    #[test]
    fn switch_on_cannot_be_retroactive_or_in_current_period() {
        let rows = [seed_row()];
        for bad in ["2026-11-01", "2026-10-01", "2025-12-01"] {
            let err = build_plan(&rows, &[], &target(bad, METHOD_VARIETY), date("2026-11-20"))
                .expect_err("追溯 / 本期必须被拦");
            assert_eq!(err.code, CODE_SWITCH_GUARD, "{bad}");
            assert!(err.msg.contains("2026-12-01"), "{bad} 的报文要给建议日：{}", err.msg);
        }
        // 恰好下月 1 日 ⇒ 放行
        let plan =
            build_plan(&rows, &[], &target("2026-12-01", METHOD_VARIETY), date("2026-11-20")).expect("下月 1 日可以切");
        assert_eq!(plan.switch_on, date("2026-12-01"));
        assert_eq!(plan.switch_period, "2026-12");
    }

    /// 护栏 3：切换日所属期间已有批次 ⇒ 66113，点名期间、批次、状态与建议日。
    #[test]
    fn switch_on_period_with_batches_is_rejected() {
        let rows = [seed_row()];
        let periods = [ledger("2026-12", 2, &["DRAFT", "SETTLED"])];
        let err = build_plan(
            &rows,
            &periods,
            &target("2026-12-01", METHOD_VARIETY),
            date("2026-11-20"),
        )
        .expect_err("本期已有批次必须被拦");
        assert_eq!(err.code, CODE_SWITCH_GUARD);
        assert!(err.msg.contains("2026-12"), "{}", err.msg);
        assert!(err.msg.contains('2'), "要点名批次数：{}", err.msg);
        assert!(err.msg.contains("2027-01-01"), "建议日要越过该期间：{}", err.msg);
        assert!(err.msg.contains("DRAFT"), "要点名状态：{}", err.msg);
    }

    /// 只剩红冲 / 作废留痕行的期间不算「已有批次」（V0373 唯一键同口径）。
    #[test]
    fn reversed_only_period_does_not_block_the_switch() {
        let rows = [seed_row()];
        let periods = [ledger("2026-12", 2, &["REVERSED", "CANCELLED"])];
        let plan = build_plan(
            &rows,
            &periods,
            &target("2026-12-01", METHOD_VARIETY),
            date("2026-11-20"),
        )
        .expect("只剩留痕行可以切");
        assert_eq!(plan.switch_period, "2026-12");
    }

    /// 同一切换日已有启用版本 ⇒ 66102（不是 66113：日子没问题，是已经切过了）。
    #[test]
    fn second_switch_on_the_same_day_is_an_overlap() {
        let rows = [
            seed_row(),
            view(
                2,
                METHOD_VARIETY,
                SCOPE_GLOBAL,
                None,
                "2026-12-01",
                None,
                STATUS_ENABLED,
            ),
        ];
        let err = build_plan(&rows, &[], &target("2026-12-01", METHOD_STANDARD), date("2026-11-20"))
            .expect_err("同一天切两次必须被拦");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, CODE_OVERLAP);
        assert!(err.msg.contains("2026-12-01"), "{}", err.msg);
    }

    // ---------- 预演：两笔写 + 影响面 ----------

    /// 核心用例：收口种子行 + 新增版本；切换前的存量期间一天不变。
    #[test]
    fn plan_closes_old_version_and_keeps_history_unchanged() {
        let rows = [seed_row()];
        let periods = [ledger("2026-10", 2, &["SETTLED"]), ledger("2026-11", 1, &["DRAFT"])];
        let plan = build_plan(
            &rows,
            &periods,
            &target("2026-12-01", METHOD_VARIETY),
            date("2026-11-20"),
        )
        .expect("预演");

        // 两笔写：种子行收到 11-30 + 新增从 12-01 起的版本
        assert_eq!(plan.ops.len(), 2);
        assert_eq!(plan.ops[0].action, OP_CLOSE);
        assert_eq!(plan.ops[0].effective_to, Some(date("2026-11-30")));
        assert_eq!(plan.ops[0].config_id, Some(Uuid::from_u128(1)));
        assert_eq!(plan.ops[1].action, OP_INSERT);
        assert_eq!(plan.ops[1].effective_from, date("2026-12-01"));
        assert_eq!(plan.ops[1].method, METHOD_VARIETY);

        // 切换前的方法与来源
        assert_eq!(plan.from_method, METHOD_JOB);
        assert_eq!(plan.from_source, SOURCE_GLOBAL);

        // 存量期间：两个都不重算、方法没变
        assert_eq!(plan.history.len(), 2);
        assert!(plan.history.iter().all(|row| row.unchanged()));
        assert!(plan.history.iter().all(|row| !row.recalculated));
        assert_eq!(plan.history[0].period_code, "2026-10");
        assert_eq!(plan.history[0].batch_count, 2);
        assert_eq!(plan.history_batches, 3);

        // 切换月是 upcoming 的第一行，按新方法
        assert_eq!(plan.upcoming[0].period_code, "2026-12");
        assert_eq!(plan.upcoming[0].method_before, METHOD_JOB);
        assert_eq!(plan.upcoming[0].method_after, METHOD_VARIETY);
        assert_eq!(plan.upcoming.len(), UPCOMING_MONTHS);

        // 目标方法未实现 ⇒ engine_ready=false + 告警
        assert!(!plan.to_method_implemented);
        assert!(!plan.engine_ready);
        assert!(plan.warnings.iter().any(|line| line.contains("66101")));
        assert!(plan.warnings.iter().any(|line| line.contains("不重算")));
    }

    /// 切到已实现的（分批法）也成立：engine_ready = true，且没有「算不出来」告警。
    #[test]
    fn switching_to_an_implemented_method_is_ready() {
        let rows = [seed_row()];
        let plan = build_plan(&rows, &[], &target("2026-12-01", METHOD_JOB), date("2026-11-20")).expect("预演");
        assert!(plan.engine_ready);
        assert!(!plan.warnings.iter().any(|line| line.contains("66101")));
        assert_eq!(plan.upcoming[0].method_after, METHOD_JOB);
    }

    /// 切换前该范围没有配置（只有代码兜底）⇒ 只新增、不收口，并给出告警。
    #[test]
    fn empty_timeline_only_inserts() {
        let plan = build_plan(&[], &[], &target("2026-12-01", METHOD_VARIETY), date("2026-11-20")).expect("预演");
        assert_eq!(plan.ops.len(), 1);
        assert_eq!(plan.ops[0].action, OP_INSERT);
        assert_eq!(plan.from_source, SOURCE_DEFAULT);
        assert!(plan.warnings.iter().any(|line| line.contains("代码兜底")));
    }

    /// 旧版已经过期（切换日前有空洞）⇒ 收口的是「切换日前一天仍在生效」的那条；
    /// 切换日之后原本也有一条将来版本 ⇒ 被 66102 拦（同一天只能一条）。
    #[test]
    fn closes_the_version_that_still_covers_the_day_before() {
        let rows = [
            view(
                1,
                METHOD_JOB,
                SCOPE_GLOBAL,
                None,
                "1970-01-01",
                Some("2026-09-30"),
                STATUS_ENABLED,
            ),
            view(
                2,
                METHOD_VARIETY,
                SCOPE_GLOBAL,
                None,
                "2026-10-01",
                None,
                STATUS_ENABLED,
            ),
        ];
        let plan = build_plan(&rows, &[], &target("2026-12-01", METHOD_STANDARD), date("2026-11-20")).expect("预演");
        assert_eq!(plan.ops[0].action, OP_CLOSE);
        assert_eq!(plan.ops[0].config_id, Some(Uuid::from_u128(2)), "收口 10-01 起那条");
        assert_eq!(plan.from_method, METHOD_VARIETY);
        // 旧版 1 号窗口没被碰（它本来就止于 09-30）
        assert!(plan.ops.len() == 2);
    }

    /// 停用的行不参与收口（停用 = 不参与判定）。
    #[test]
    fn disabled_versions_are_ignored() {
        let rows = [
            seed_row(),
            view(2, METHOD_STANDARD, SCOPE_GLOBAL, None, "2026-11-01", None, 0),
        ];
        let plan = build_plan(&rows, &[], &target("2026-12-01", METHOD_VARIETY), date("2026-11-20")).expect("预演");
        assert_eq!(
            plan.ops[0].config_id,
            Some(Uuid::from_u128(1)),
            "收口种子行而不是停用行"
        );
        assert_eq!(plan.from_method, METHOD_JOB);
    }

    /// 将来期间已有批次（例如提前算过 2027-01）⇒ 列进 future，note 提示「下次重算会按新方法」。
    #[test]
    fn future_periods_with_batches_are_reported() {
        let rows = [seed_row()];
        let periods = [ledger("2027-01", 3, &["SUBMITTED"])];
        let plan = build_plan(
            &rows,
            &periods,
            &target("2026-12-01", METHOD_VARIETY),
            date("2026-11-20"),
        )
        .expect("预演");
        assert_eq!(plan.future.len(), 1);
        assert_eq!(plan.future[0].period_code, "2027-01");
        assert_eq!(plan.future[0].method_before, METHOD_JOB);
        assert_eq!(plan.future[0].method_after, METHOD_VARIETY);
        assert!(plan.future[0].note.contains("下次重算会按新方法"));
        // upcoming 里不重复列 2027-01
        assert!(plan.upcoming.iter().all(|row| row.period_code != "2027-01"));
    }

    /// 对象级覆盖：全公司切换时这些对象仍按自己的方法 —— 必须列出来，且历史期间不受影响。
    #[test]
    fn object_level_overrides_are_listed_and_untouched() {
        let object = Uuid::from_u128(99);
        let rows = [
            seed_row(),
            view(
                5,
                METHOD_JOB,
                SCOPE_OBJECT,
                Some(object),
                "1970-01-01",
                None,
                STATUS_ENABLED,
            ),
        ];
        let plan = build_plan(&rows, &[], &target("2026-12-01", METHOD_VARIETY), date("2026-11-20")).expect("预演");
        assert_eq!(plan.object_overrides.len(), 1);
        assert_eq!(plan.object_overrides[0].scope_id, object);
        // 全公司默认切换：对象级那条对象在新期间仍是分批法（对象级优先）
        let at_new_period = method_dto::resolve(
            &{
                let mut after = rows.to_vec();
                after[0].effective_to = Some(date("2026-11-30"));
                after.push(view(
                    9000,
                    METHOD_VARIETY,
                    SCOPE_GLOBAL,
                    None,
                    "2026-12-01",
                    None,
                    STATUS_ENABLED,
                ));
                after
            },
            Some(object),
            date("2026-12-05"),
        );
        assert_eq!(at_new_period.method, METHOD_JOB, "对象级配置优先，不随全公司默认改");
        assert!(plan.warnings.iter().any(|line| line.contains("对象级优先")));
    }

    /// 对象级切换只影响那一个对象，全公司默认不动。
    #[test]
    fn object_scope_switch_does_not_touch_global() {
        let object = Uuid::from_u128(7);
        let rows = [
            seed_row(),
            view(
                5,
                METHOD_JOB,
                SCOPE_OBJECT,
                Some(object),
                "1970-01-01",
                None,
                STATUS_ENABLED,
            ),
        ];
        let target = SwitchTarget {
            switch_on: date("2026-12-01"),
            scope_type: SCOPE_OBJECT.to_string(),
            scope_id: Some(object),
            to_method: METHOD_VARIETY.to_string(),
        };
        let plan = build_plan(
            &rows,
            &[ledger("2026-11", 1, &["SETTLED"])],
            &target,
            date("2026-11-20"),
        )
        .expect("预演");
        assert_eq!(plan.ops[0].config_id, Some(Uuid::from_u128(5)));
        assert!(plan.object_overrides.is_empty(), "对象级切换没有「被覆盖」清单");
        // 全公司默认那行不在 ops 里
        assert!(plan.ops.iter().all(|op| op.config_id != Some(Uuid::from_u128(1))));
    }

    /// `new_version_spec` 与 ops 的 INSERT 逐字一致（落库参数就是这个）。
    #[test]
    fn new_version_spec_matches_the_insert_op() {
        let rows = [seed_row()];
        let plan = build_plan(&rows, &[], &target("2026-12-01", METHOD_VARIETY), date("2026-11-20")).expect("预演");
        let spec = new_version_spec(&plan, Some("M5-31 切换".to_string()));
        assert_eq!(spec.method, plan.ops[1].method);
        assert_eq!(spec.effective_from, plan.ops[1].effective_from);
        assert_eq!(spec.effective_to, None);
        assert_eq!(spec.scope_type, SCOPE_GLOBAL);
        assert_eq!(spec.scope_id, None);
        assert_eq!(spec.status, STATUS_ENABLED);
        assert_eq!(spec.remark.as_deref(), Some("M5-31 切换"));
    }

    /// 建议切换日：取「下月 1 日」与「现有最大期间之后」的晚者。
    #[test]
    fn suggested_switch_day_is_the_later_of_the_two() {
        let today = date("2026-11-20");
        assert_eq!(suggested_switch_on(today, &[]), date("2026-12-01"));
        assert_eq!(
            suggested_switch_on(today, &[ledger("2026-10", 1, &["SETTLED"])]),
            date("2026-12-01")
        );
        assert_eq!(
            suggested_switch_on(today, &[ledger("2027-02", 1, &["SETTLED"])]),
            date("2027-03-01")
        );
    }

    /// `parse_target`：缺省切换日用 fallback；范围成对校验与 M5-01 一致（66103）。
    #[test]
    fn parse_target_defaults_and_validates_scope_pairing() {
        let fallback = date("2026-12-01");
        let ok = parse_target(None, Some("variety"), None, None, fallback).expect("缺省 GLOBAL");
        assert_eq!(ok.switch_on, fallback);
        assert_eq!(ok.to_method, METHOD_VARIETY);
        assert_eq!(ok.scope_type, SCOPE_GLOBAL);
        assert_eq!(ok.scope_id, None);

        let err = parse_target(None, None, None, None, fallback).expect_err("缺 to_method");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);

        let err = parse_target(None, Some("JOB"), Some("GLOBAL"), Some("x"), fallback).expect_err("GLOBAL 不许带对象");
        assert_eq!(err.code, method_dto::CODE_SCOPE_INVALID);

        let err =
            parse_target(None, Some("JOB"), Some("COST_OBJECT"), None, fallback).expect_err("对象级必须给 scope_id");
        assert_eq!(err.code, method_dto::CODE_SCOPE_INVALID);

        let object = Uuid::from_u128(11);
        let ok = parse_target(
            Some("2026-12-01"),
            Some("JOB"),
            Some("COST_OBJECT"),
            Some(&object.to_string()),
            fallback,
        )
        .expect("对象级");
        assert_eq!(ok.scope_id, Some(object));
    }

    /// `plan_res`：出参带中文名 / 期间窗口 / `history_untouched` 结论。
    #[test]
    fn plan_res_carries_labels_and_the_untouched_flag() {
        let rows = [seed_row()];
        let periods = [ledger("2026-11", 1, &["DRAFT"])];
        let plan = build_plan(
            &rows,
            &periods,
            &target("2026-12-01", METHOD_VARIETY),
            date("2026-11-20"),
        )
        .expect("预演");
        let res = plan_res(&plan, "全公司默认", None, &std::collections::HashMap::new());
        assert_eq!(res.scope_label, "全公司默认");
        assert_eq!(res.from_method_label, "分批法");
        assert_eq!(res.to_method_label, "品种法");
        assert_eq!(res.history[0].period_from, "2026-11-01");
        assert_eq!(res.history[0].period_to, "2026-11-30");
        assert!(res.history[0].unchanged);
        assert!(!res.history[0].recalculated);
        assert!(res.history_untouched);
        let json = serde_json::to_value(&res).expect("序列化");
        assert_eq!(json["history_untouched"], serde_json::json!(true));
        assert_eq!(json["ops"].as_array().map(Vec::len), Some(2));
    }
}
