//! 标准成本卡 **版本表（物料 × 期间）** 契约层（成本核算 · M5-08）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md` §3.2 ⑪⑫（两张表 V0374 已建）、
//! §4.9（差异分析口径）、D-15（标准成本只做差异分析、不改存货计价）；
//! 拆卡依据：`docs/planning/20261113-新卡清单.md` M5-08 + `20261113-全量开发计划.md` 2-1。
//!
//! # 这张「版本表」到底长什么样
//!
//! | 层 | 表 | 一行是什么 |
//! |---|---|---|
//! | 卡头 | `biz_cost_standard` | 一个**物料的一版标准成本**：单位标准料 / 工 / 费 + 生效区间 + 版本号 + 状态 |
//! | 明细 | `biz_cost_standard_item` | 这一版的标准 BOM 一行：耗用物料 × 单位标准用量 × 标准价 |
//!
//! **「物料 × 期间」不是新列**：版本的时间维度已经由 `effective_from` / `effective_to` 表达，
//! 期间（`yyyy-MM`）只是取数视图 —— `period_code → [期初, 期末]`（[`period_window`]），
//! 再问「这个期间末生效的是哪一版」（[`window_covers`]）。这样**一版可以连月有效**，
//! 不需要每月复制一张卡；期间中途换版会由 `switch_in_period` 如实报出来，不静默取其一。
//!
//! # 三条算数口径（与库层 CHECK 一一对应，改这里必须同时改库）
//!
//! ```text
//! std_material_cost = Σ round(标准用量 × 标准价, 6)          -- 明细求和（ck_biz_cost_std_item_cal 逐行锁）
//! std_labor_hours   = round(标准工时, 4)                    -- 列是 NUMERIC(18,4)
//! std_labor_cost    = round(std_labor_hours × 标准工资率, 6) -- ck_biz_cost_std_labor 锁死
//! std_total_cost    = 料 + 工 + 费                           -- ck_biz_cost_std_sum 锁死
//! ```
//!
//! **先舍工时到 4 位再乘费率**：库里的 CHECK 是拿**存下来的**（4 位）工时乘费率，
//! 若拿请求里的原始值去乘，.00005 这类边界上 Rust 与 PG 会算出不同的 6 位金额 ⇒ 写库直接
//! `check_violation`。舍入策略与 PG `numeric::round` 一致（半数远离零，见
//! [`cost_labor_dto::ROUNDING`](crate::business::erp::cost::cost_labor_dto::ROUNDING)）。
//!
//! # 三条纪律（handler 执行，dto 只提供判据）
//!
//! 1. **同物料启用版本生效期不许重叠**（[`find_overlap`] + 库层 EXCLUDE 兜并发）：
//!    一重叠，「这一天按哪版算」就没有确定答案（409 + `66108`）。
//! 2. **已生效过的版本不许改口径**（`effective_from < 今天`）：只允许改停止日 / 状态 / 备注
//!    （409 + `66109`）。换口径的正确姿势 = 新增一版 + 旧版收口停止日。
//! 3. **已生效过的版本不许删**（409 + `66110`）：版本行是历史差异的依据，要停用请「失效」。
//!
//! # 错误码（`66xxx` 段，接在 66107 之后）
//!
//! | 码 | 场景 | HTTP |
//! |---|---|---|
//! | [`CODE_OVERLAP`] `66108` | 同物料启用版本生效期重叠 | 409 |
//! | [`CODE_LOCKED`] `66109` | 改写已生效版本的口径 | 409 |
//! | [`CODE_DELETE_IN_USE`] `66110` | 删除已生效过的版本 | 409 |

use nik::prelude::chrono::{Datelike, Duration, NaiveDate};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::rust_decimal::RoundingStrategy;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::cost::cost_labor_dto;
use cloudpivot_kernel::shared::error_code;

// ==================== 值域 ====================

/// 启用：参与「某期间按哪版标准算」的取数。
pub const STATUS_ACTIVE: i16 = 1;
/// 失效：保留历史，不占生效窗口（失效后可以立刻启用新版覆盖同一期间）。
pub const STATUS_INACTIVE: i16 = 0;

/// 金额 / 单价 / 费率标度（`NUMERIC(18,6)`）。
pub const AMOUNT_SCALE: u32 = 6;
/// 工时标度（`std_labor_hours NUMERIC(18,4)`）。
pub const HOURS_SCALE: u32 = 4;
/// 金额列上限（`NUMERIC(18,6)` = 12 位整数 + 6 位小数）。
pub const AMOUNT_MAX: &str = "999999999999.999999";
/// 工时列上限（`NUMERIC(18,4)`）。
pub const HOURS_MAX: &str = "99999999999999.9999";
/// 标准 BOM 最多的行数（200 行足够覆盖任何真实 BOM；再多是导错了数据）。
pub const ITEM_MAX: usize = 200;
/// 备注最长字符数（超长提前 400，别让库抛 22001）。
pub const REMARK_MAX: usize = 200;

/// 细分行号从 1 开始（与 `uk_biz_cost_std_item (standard_id, line_no)` 一致）。
pub const FIRST_LINE_NO: i32 = 1;

/// 舍入策略：与 PG `numeric` 的 `round()` 同口径（半数远离零）。
pub const ROUNDING: RoundingStrategy = RoundingStrategy::MidpointAwayFromZero;

// ==================== 发布轴（M5-10 · 审核后生效） ====================

/// 草稿：还没提交审批（新建 / 被驳回 / 撤回后的状态）。**不参与取数**。
pub const PUBLISH_DRAFT: &str = "DRAFT";
/// 审批中：已提交，等审批人处理。**不参与取数**，且不许改口径。
pub const PUBLISH_PENDING: &str = "PENDING";
/// 已发布：审批通过。只有它（且 `status = 1`）才参与期间取数。
pub const PUBLISH_PUBLISHED: &str = "PUBLISHED";

/// 发布状态三值域（与 `ck_biz_cost_std_publish` 逐字一致）。
pub const PUBLISH_STATES: [&str; 3] = [PUBLISH_DRAFT, PUBLISH_PENDING, PUBLISH_PUBLISHED];

/// 发布状态是否合法（纯函数，列表筛选与 DTO 归一都用它）。
///
/// **大小写 / 前后空白都归一**：值域是写死的大写三值，前端漏 trim / 传小写时不该变成 400
/// （这类报文只会让人以为「字段名写错了」）。
pub fn is_publish_state(raw: &str) -> bool {
    let upper = raw.trim().to_ascii_uppercase();
    PUBLISH_STATES.contains(&upper.as_str())
}

/// 发布状态 → 中文（界面徽标 / 报文用）。
pub fn publish_state_label(state: &str) -> &'static str {
    match state.trim().to_ascii_uppercase().as_str() {
        PUBLISH_DRAFT => "草稿",
        PUBLISH_PENDING => "审批中",
        PUBLISH_PUBLISHED => "已发布",
        _ => "未知",
    }
}

/// 列表里给人看的**状态文案**：发布轴没走完时不说「生效」。
///
/// `status`（1/0）与 `publish_state` 是两条正交的轴（见 V0465 头注释）。草稿卡的
/// `status` 可能已经是 1（占住生效窗口等审批），但它在差异分析里**还没生效** ——
/// 列表只打「生效」会让人以为已经在算，所以这里按发布轴优先出文案。
/// **判据仍然只认两条轴的原值**，本函数纯粹是展示层。
pub fn headline_status_label(status: i16, publish_state: &str) -> String {
    match publish_state.trim().to_ascii_uppercase().as_str() {
        PUBLISH_PENDING => format!("{}（未生效）", publish_state_label(PUBLISH_PENDING)),
        PUBLISH_DRAFT => format!("{}（未生效）", publish_state_label(PUBLISH_DRAFT)),
        _ => status_label(status).to_string(),
    }
}

// ==================== 错误码 ====================

/// `66108`：同物料启用版本生效期重叠。
pub const CODE_OVERLAP: i32 = error_code::ERP_COST_STANDARD_OVERLAP;
/// `66109`：改写已生效版本的口径。
pub const CODE_LOCKED: i32 = error_code::ERP_COST_STANDARD_LOCKED_EFFECTIVE;
/// `66110`：删除已生效过的版本。
pub const CODE_DELETE_IN_USE: i32 = error_code::ERP_COST_STANDARD_DELETE_IN_USE;
/// `66111`：发布状态不接受该动作（非草稿不能提交 / 审批中不能改 / 未发布不能置生效）。
pub const CODE_PUBLISH_GUARD: i32 = error_code::ERP_COST_STANDARD_PUBLISH_GUARD;

/// 409 + `66111`：发布状态闸（提交 / 撤回 / 改 / 删 / 置生效都走它）。
///
/// 报文由调用方写清「当前是什么状态、出路是什么」—— 这类错误必须是**可操作**的
/// （约定 11 的判据：用户不许靠改库恢复）。
pub fn publish_guard_error(message: impl Into<String>) -> ApiError {
    ApiError::new(409, CODE_PUBLISH_GUARD, message.into())
}

// ==================== 期间 ====================

/// `yyyy-MM` → `(期初, 期末)`（两端都含）。
///
/// # 异常情况
///
/// 形状不对 / 月份越界 / 年份离谱 ⇒ 400。**不做「猜」**：`2026-1`、`2026-13`、`26-01`
/// 一律拒，因为期间是差异分析的落库键，猜错一个月就是整月报表错位。
pub fn period_window(raw: &str) -> std::result::Result<(NaiveDate, NaiveDate), ApiError> {
    let text = raw.trim();
    if text.len() != 7 {
        return Err(bad_request(format!("period_code 必须是 yyyy-MM 形状（收到「{raw}」）")));
    }
    let (year, month) = text
        .split_once('-')
        .ok_or_else(|| bad_request(format!("period_code 必须是 yyyy-MM 形状（收到「{raw}」）")))?;
    if year.len() != 4 || month.len() != 2 {
        return Err(bad_request(format!(
            "period_code 必须是 yyyy-MM 形状（年月各 4 / 2 位，收到「{raw}」）"
        )));
    }
    let year: i32 = year
        .parse()
        .map_err(|_| bad_request(format!("period_code 的年份不是数字（收到「{raw}」）")))?;
    let month: u32 = month
        .parse()
        .map_err(|_| bad_request(format!("period_code 的月份不是数字（收到「{raw}」）")))?;
    if !(2000..=2099).contains(&year) {
        return Err(bad_request(format!(
            "period_code 的年份应在 2000–2099 之间（收到 {year}）"
        )));
    }
    if !(1..=12).contains(&month) {
        return Err(bad_request(format!(
            "period_code 的月份应在 01–12 之间（收到 {month}）"
        )));
    }
    let start = NaiveDate::from_ymd_opt(year, month, 1)
        .ok_or_else(|| bad_request(format!("{raw} 不是合法期间（构造期初失败）")))?;
    let next_month_start = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .ok_or_else(|| bad_request(format!("{raw} 不是合法期间（构造下月失败）")))?;
    let end = next_month_start - Duration::days(1);
    Ok((start, end))
}

/// 日期 → `yyyy-MM`（报文里点名期间用）。
pub fn period_of_date(date: NaiveDate) -> String {
    format!("{:04}-{:02}", date.year(), date.month())
}

/// 两个生效区间是否重叠（**端点相接不算重叠**：1 月底失效、2 月 1 日生效是常态）。
pub fn windows_overlap(a_from: NaiveDate, a_to: Option<NaiveDate>, b_from: NaiveDate, b_to: Option<NaiveDate>) -> bool {
    a_to.is_none_or(|a_end| b_from <= a_end) && b_to.is_none_or(|b_end| a_from <= b_end)
}

// ==================== 数值 ====================

/// 金额 / 单价 / 费率文本（6 位小数，与列精度一致）。
pub fn amount_text(value: Decimal) -> String {
    format!("{:.6}", value.round_dp_with_strategy(AMOUNT_SCALE, ROUNDING))
}

/// 工时文本（4 位小数）。
pub fn hours_text(value: Decimal) -> String {
    format!("{:.4}", value.round_dp_with_strategy(HOURS_SCALE, ROUNDING))
}

/// 日期文本（`yyyy-MM-dd`）。
pub fn date_text(date: NaiveDate) -> String {
    date.to_string()
}

/// 生效区间文本：`2026-11-01 ~ 至今` / `2026-11-01 ~ 2026-11-30`。
pub fn window_text(from: NaiveDate, to: Option<NaiveDate>) -> String {
    match to {
        None => format!("{from} ~ 至今"),
        Some(end) => format!("{from} ~ {end}"),
    }
}

// ==================== 三要素与规格 ====================

/// 一条标准 BOM 行（归一化后）。
#[derive(Debug, Clone, PartialEq)]
pub struct StandardItemSpec {
    /// 耗用物料。
    pub component_material_id: Uuid,
    /// 单位标准用量（> 0）。
    pub std_qty: Decimal,
    /// 标准单价（≥ 0）。
    pub std_price: Decimal,
}

impl StandardItemSpec {
    /// 标准金额 = `round(用量 × 单价, 6)`（与 `ck_biz_cost_std_item_cal` 同口径）。
    pub fn amount(&self) -> Decimal {
        (self.std_qty * self.std_price).round_dp_with_strategy(AMOUNT_SCALE, ROUNDING)
    }
}

/// 单位标准成本三要素（料 / 工 / 费）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Amounts {
    /// 标准材料成本 = Σ 明细金额。
    pub material: Decimal,
    /// 标准人工成本 = round(标准工时 × 标准工资率, 6)。
    pub labor: Decimal,
    /// 标准制造费用。
    pub overhead: Decimal,
}

impl Amounts {
    /// 标准总成本 = 料 + 工 + 费（`ck_biz_cost_std_sum` 锁死恒等）。
    pub fn total(&self) -> Decimal {
        self.material + self.labor + self.overhead
    }
}

/// 标准成本卡归一化规格（dto ↔ model 之间唯一的形状）。
#[derive(Debug, Clone, PartialEq)]
pub struct StandardSpec {
    /// 成品 / 半成品物料（弱引用 `biz_material`）。
    pub material_id: Uuid,
    /// 生效起始日（含）。
    pub effective_from: NaiveDate,
    /// 停止日（含；`None` = 至今有效）。
    pub effective_to: Option<NaiveDate>,
    /// 单位标准工时（已舍到 4 位）。
    pub std_labor_hours: Decimal,
    /// 标准工资率。
    pub std_labor_rate: Decimal,
    /// 单位标准制造费用。
    pub std_overhead_cost: Decimal,
    /// 状态（1 启用 / 0 失效）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 标准 BOM（行号按数组顺序从 1 起）。
    pub items: Vec<StandardItemSpec>,
}

impl StandardSpec {
    /// 三要素（料由明细求和，工由工时 × 费率，费直接给）。
    pub fn amounts(&self) -> Amounts {
        let material = self.items.iter().fold(Decimal::ZERO, |acc, item| acc + item.amount());
        Amounts {
            material,
            labor: labor_cost(self.std_labor_hours, self.std_labor_rate),
            overhead: self.std_overhead_cost,
        }
    }

    /// 区间文本（报文用）。
    pub fn window_text(&self) -> String {
        window_text(self.effective_from, self.effective_to)
    }

    /// 是否启用。
    pub fn is_active(&self) -> bool {
        self.status == STATUS_ACTIVE
    }
}

/// 标准人工成本 = `round(工时 × 费率, 6)`（全模块唯一的算法）。
///
/// 入参工时**必须先舍到 4 位**（[`StandardSpec::amounts`] 已保证），否则与库层
/// `ck_biz_cost_std_labor` 的求值结果会在小数边界上打架。
pub fn labor_cost(hours: Decimal, rate: Decimal) -> Decimal {
    (hours * rate).round_dp_with_strategy(AMOUNT_SCALE, ROUNDING)
}

// ==================== 区间判定（纯函数） ====================

/// 生效区间是否覆盖某一天（两端都含）。
pub fn window_covers(from: NaiveDate, to: Option<NaiveDate>, date: NaiveDate) -> bool {
    from <= date && to.is_none_or(|end| end >= date)
}

/// 生效区间是否与 `[start, end]` 有交集（两端都含）。
pub fn window_intersects(from: NaiveDate, to: Option<NaiveDate>, start: NaiveDate, end: NaiveDate) -> bool {
    from <= end && to.is_none_or(|own_end| own_end >= start)
}

/// 判定用的版本投影（model 查库后交过来，判定本身零查库）。
#[derive(Debug, Clone, PartialEq)]
pub struct VersionView {
    /// 主键。
    pub id: Uuid,
    /// 物料。
    pub material_id: Uuid,
    /// 版本号。
    pub version_no: i32,
    /// 生效起始日。
    pub effective_from: NaiveDate,
    /// 停止日。
    pub effective_to: Option<NaiveDate>,
    /// 状态。
    pub status: i16,
}

/// 找「同物料 + 启用 + 与目标区间重叠」的版本（`exclude` = 改自己时排除自己）。
///
/// 只看 `status = STATUS_ACTIVE`：失效版本不占窗口，正是「先失效旧版再启用新版」这条
/// 正路能走通的原因（库层 EXCLUDE 的谓词与这里逐字同口径）。
pub fn find_overlap<'a>(
    views: &'a [VersionView],
    spec: &StandardSpec,
    exclude: Option<Uuid>,
) -> Option<&'a VersionView> {
    views.iter().find(|view| {
        Some(view.id) != exclude
            && view.material_id == spec.material_id
            && view.status == STATUS_ACTIVE
            && windows_overlap(
                view.effective_from,
                view.effective_to,
                spec.effective_from,
                spec.effective_to,
            )
    })
}

/// 某物料某天生效的版本（启用 + 覆盖该日）。
pub fn resolve_on(views: &[VersionView], material_id: Uuid, date: NaiveDate) -> Option<&VersionView> {
    views.iter().find(|view| {
        view.material_id == material_id
            && view.status == STATUS_ACTIVE
            && window_covers(view.effective_from, view.effective_to, date)
    })
}

// ==================== 错误构造 ====================

/// 400：参数不合法。
pub fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 404：版本不存在（已删 / 从来没建）。
pub fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!(
            "标准成本版本不存在或已删除（{id}）。请用 GET /business/erp/cost-standard?period_code=yyyy-MM \
             找到现有版本"
        ),
    )
}

/// 404：某物料某期间没有生效版本。
pub fn no_effective_version(material_label: &str, on: NaiveDate) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!(
            "{material_label} 在 {on} 没有生效的标准成本版本。先建一版（POST /business/erp/cost-standard，\
             可用 period_code={} 直接落整期窗口），或把已有版本的生效区间覆盖到这一天",
            period_of_date(on)
        ),
    )
}

/// 409 + `66108`：同物料启用版本生效期重叠。
pub fn overlap_error(
    material_label: &str,
    existing_version_no: i32,
    existing_window: &str,
    target_window: &str,
) -> ApiError {
    ApiError::new(
        409,
        CODE_OVERLAP,
        format!(
            "{material_label} 已有启用版本 v{existing_version_no}（{existing_window}）与本次 {target_window} 重叠：\
             同一物料在同一段日期只能有一个生效版本，否则「这一天按哪版标准算」没有确定答案。\
             三条出路：① 把旧版停止日收口到新版生效日前一天；② 失效旧版（POST …/{{id}}/status，\
             status=0，失效不占生效窗口）；③ 把新版的生效日推到旧版停止日之后"
        ),
    )
}

/// 409 + `66109`：改写已生效版本的口径。
pub fn locked_effective(
    material_label: &str,
    version_no: i32,
    effective_from: NaiveDate,
    today: NaiveDate,
    changed: &str,
) -> ApiError {
    ApiError::new(
        409,
        CODE_LOCKED,
        format!(
            "{material_label} v{version_no} 从 {effective_from} 起就已生效（今天 {today}），\
             本次试图改写 {changed} —— 已生效版本只允许改停止日 / 状态 / 备注，\
             改了等于事后改写「那段期间按什么标准算的」，历史差异会跟着变而看不出来。\
             要换口径请新增一版（生效日放今天或下期），再把本版停止日收口到新一版生效日前一天"
        ),
    )
}

/// 409 + `66110`：删除已生效过的版本。
pub fn delete_in_use(material_label: &str, version_no: i32, effective_from: NaiveDate, today: NaiveDate) -> ApiError {
    ApiError::new(
        409,
        CODE_DELETE_IN_USE,
        format!(
            "{material_label} v{version_no} 从 {effective_from} 起就已生效（今天 {today}），不能删：\
             版本行是历史差异的依据（biz_cost_variance.standard_id 回指它）。\
             不再使用请改成失效：POST /business/erp/cost-standard/{{id}}/status，status=0"
        ),
    )
}

// ==================== 入参 ====================

/// 标准 BOM 一行（请求）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StandardItemReq {
    /// 耗用物料（必填）。
    pub component_material_id: Option<String>,
    /// 单位标准用量（必填，> 0）。
    pub std_qty: Option<Decimal>,
    /// 标准单价（必填，≥ 0）。
    pub std_price: Option<Decimal>,
}

/// 新建一版标准成本（请求）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StandardCreateReq {
    /// 成品 / 半成品物料（必填）。
    pub material_id: Option<String>,
    /// 整期便捷写法：`period_code` = `yyyy-MM` ⇒ 生效区间直接落整期（期初 ~ 期末）。
    /// 与 `effective_from` / `effective_to` 同时给时必须一致（矛盾一律 400）。
    pub period_code: Option<String>,
    /// 生效起始日（`period_code` 为空时必填）。
    pub effective_from: Option<String>,
    /// 停止日（可空 = 至今有效）。
    pub effective_to: Option<String>,
    /// 单位标准工时（可空 = 0）。
    pub std_labor_hours: Option<Decimal>,
    /// 标准工资率（可空 = 0）。
    pub std_labor_rate: Option<Decimal>,
    /// 单位标准制造费用（可空 = 0）。
    pub std_overhead_cost: Option<Decimal>,
    /// 标准 BOM 明细（可空 = 无材料耗用；给了就整表插入）。
    pub items: Option<Vec<StandardItemReq>>,
    /// 状态（可空 = 启用）。
    pub status: Option<i16>,
    /// 备注。
    pub remark: Option<String>,
}

/// 改一版标准成本（请求）：`None` = 不改这一项。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StandardUpdateReq {
    /// 物料（`None` = 不改）。已生效版本不许改。
    pub material_id: Option<String>,
    /// 整期便捷写法（同新建）。
    pub period_code: Option<String>,
    /// 生效起始日（`None` = 不改）。已生效版本不许改。
    pub effective_from: Option<String>,
    /// 停止日（`None` = 不改）。
    pub effective_to: Option<String>,
    /// `true` = 把停止日清成「至今有效」（`effective_to` 传空串表达不了这个语义）。
    pub clear_effective_to: Option<bool>,
    /// 单位标准工时（`None` = 不改）。已生效版本不许改。
    pub std_labor_hours: Option<Decimal>,
    /// 标准工资率（`None` = 不改）。已生效版本不许改。
    pub std_labor_rate: Option<Decimal>,
    /// 制造费用（`None` = 不改）。已生效版本不许改。
    pub std_overhead_cost: Option<Decimal>,
    /// 标准 BOM（`None` = 不改；`Some([])` = 清空）。已生效版本不许改。
    pub items: Option<Vec<StandardItemReq>>,
    /// 状态（`None` = 不改）。
    pub status: Option<i16>,
    /// 备注（`Some("")` = 清空；`None` = 不改）。
    pub remark: Option<String>,
}

/// 生效 / 失效（请求）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StandardStatusReq {
    /// 1 生效 / 0 失效（必填）。
    pub status: Option<i16>,
    /// 变更原因（可空；给了会以 `[生效|失效 yyyy-MM-dd]` 前缀追加到备注，留痕）。
    pub reason: Option<String>,
}

/// 版本列表查询。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StandardPageQuery {
    /// 物料。
    pub material_id: Option<String>,
    /// 状态（1 / 0）。
    pub status: Option<i16>,
    /// 发布状态（DRAFT / PENDING / PUBLISHED，可不传）。M5-10。
    pub publish_state: Option<String>,
    /// 期间 `yyyy-MM`：版本生效区间与该期间有交集即命中。
    pub period_code: Option<String>,
    /// 某一天 `yyyy-MM-dd`：版本生效区间覆盖该日即命中。
    pub on_date: Option<String>,
    /// 关键字（物料编码 / 物料名称 / 备注）。
    pub keyword: Option<String>,
    /// 页号。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

/// 「某物料某期间生效的是哪一版」查询。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StandardEffectiveQuery {
    /// 物料（必填）。
    pub material_id: Option<String>,
    /// 期间 `yyyy-MM`（与 `on_date` 二选一；都给时日期必须落在期间内）。
    pub period_code: Option<String>,
    /// 某一天 `yyyy-MM-dd`（不传期间时按这一天取）。
    pub on_date: Option<String>,
}

/// 「期间 × 物料」矩阵查询（版本表主视图）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PeriodSheetQuery {
    /// 期间 `yyyy-MM`（必填）。
    pub period_code: Option<String>,
    /// 关键字（物料编码 / 物料名称）。
    pub keyword: Option<String>,
    /// 页号。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

// ==================== 入参 → 规格（纯逻辑） ====================

/// 状态归一化：只认 0 / 1。
pub fn normalize_status(raw: Option<i16>) -> std::result::Result<i16, ApiError> {
    match raw {
        None => Ok(STATUS_ACTIVE),
        Some(value) if value == STATUS_ACTIVE || value == STATUS_INACTIVE => Ok(value),
        Some(other) => Err(bad_request(format!(
            "status 只能是 1（启用）或 0（失效），收到 {other}"
        ))),
    }
}

/// 状态文本（卡面口径：版本是「生效 / 失效」，对应 `biz_cost_standard.status` 的 1 / 0）。
pub fn status_label(status: i16) -> &'static str {
    if status == STATUS_ACTIVE { "生效" } else { "失效" }
}

/// 备注归一化（长度上限 200）。
pub fn clean_remark(value: Option<String>) -> std::result::Result<Option<String>, ApiError> {
    let text = cost_labor_dto::clean_optional(value);
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

/// 生效区间归一化：`period_code`（整期）优先，其次显式日期；两者都给必须一致。
fn window_from_req(
    period_code: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    fallback: Option<(NaiveDate, Option<NaiveDate>)>,
) -> std::result::Result<(NaiveDate, Option<NaiveDate>), ApiError> {
    let period = period_code.map(str::trim).filter(|text| !text.is_empty());
    let explicit_from = from.map(str::trim).filter(|text| !text.is_empty());
    let explicit_to = to.map(str::trim).filter(|text| !text.is_empty());

    match period {
        Some(period) => {
            let (start, end) = period_window(period)?;
            if let Some(raw) = explicit_from {
                let date = cost_labor_dto::parse_date(raw, "effective_from")?;
                if date != start {
                    return Err(bad_request(format!(
                        "period_code={period} 与 effective_from={date} 矛盾：整期写法下生效日必须是 {start}。\
                         要自定义生效日就别传 period_code"
                    )));
                }
            }
            if let Some(raw) = explicit_to {
                let date = cost_labor_dto::parse_date(raw, "effective_to")?;
                if date != end {
                    return Err(bad_request(format!(
                        "period_code={period} 与 effective_to={date} 矛盾：整期写法下停止日必须是 {end}。\
                         要自定义停止日就别传 period_code"
                    )));
                }
            }
            Ok((start, Some(end)))
        }
        None => {
            let start = match (explicit_from, fallback) {
                (Some(raw), _) => cost_labor_dto::parse_date(raw, "effective_from")?,
                (None, Some((current, _))) => current,
                (None, None) => {
                    return Err(bad_request(
                        "effective_from（生效起始日）或 period_code（yyyy-MM 整期）至少给一个",
                    ));
                }
            };
            let end = match (explicit_to, fallback) {
                (Some(raw), _) => Some(cost_labor_dto::parse_date(raw, "effective_to")?),
                (None, Some((_, current))) => current,
                (None, None) => None,
            };
            if let Some(end) = end
                && end < start
            {
                return Err(bad_request(format!(
                    "effective_to（{end}）不能早于 effective_from（{start}）"
                )));
            }
            Ok((start, end))
        }
    }
}

/// 标准 BOM 归一化：行数上限 / 重复物料 / 数值值域。
pub fn items_from_req(items: Option<&[StandardItemReq]>) -> std::result::Result<Vec<StandardItemSpec>, ApiError> {
    let Some(items) = items else {
        return Ok(Vec::new());
    };
    if items.len() > ITEM_MAX {
        return Err(bad_request(format!(
            "标准 BOM 最多 {ITEM_MAX} 行（收到 {} 行）",
            items.len()
        )));
    }
    let mut specs: Vec<StandardItemSpec> = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let line_no = index as i32 + FIRST_LINE_NO;
        let raw_id = item
            .component_material_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| bad_request(format!("第 {line_no} 行：component_material_id（耗用物料）不能为空")))?;
        let component_id = cost_labor_dto::parse_uuid(raw_id, "component_material_id")?;
        let std_qty = cost_labor_dto::ensure_decimal(
            item.std_qty
                .ok_or_else(|| bad_request(format!("第 {line_no} 行：std_qty（单位标准用量）不能为空")))?,
            "std_qty",
            AMOUNT_SCALE,
            AMOUNT_MAX,
            false,
        )?;
        let std_price = cost_labor_dto::ensure_decimal(
            item.std_price
                .ok_or_else(|| bad_request(format!("第 {line_no} 行：std_price（标准单价）不能为空")))?,
            "std_price",
            AMOUNT_SCALE,
            AMOUNT_MAX,
            true,
        )?;
        if specs.iter().any(|spec| spec.component_material_id == component_id) {
            return Err(bad_request(format!(
                "第 {line_no} 行：耗用物料 {component_id} 重复出现。\
                 同一物料在一张标准卡里只能有一行（要拆用量请合并到同一行的 std_qty）"
            )));
        }
        specs.push(StandardItemSpec {
            component_material_id: component_id,
            std_qty,
            std_price,
        });
    }
    Ok(specs)
}

/// 新建请求 → 归一化规格（**纯逻辑**；物料是否存在由 handler 查库）。
pub fn spec_from_create(req: &StandardCreateReq) -> std::result::Result<StandardSpec, ApiError> {
    let raw_material = req
        .material_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("material_id（成品 / 半成品物料）不能为空"))?;
    let material_id = cost_labor_dto::parse_uuid(raw_material, "material_id")?;
    let (effective_from, effective_to) = window_from_req(
        req.period_code.as_deref(),
        req.effective_from.as_deref(),
        req.effective_to.as_deref(),
        None,
    )?;
    let std_labor_hours = cost_labor_dto::ensure_decimal(
        req.std_labor_hours.unwrap_or(Decimal::ZERO),
        "std_labor_hours",
        HOURS_SCALE,
        HOURS_MAX,
        true,
    )?
    .round_dp_with_strategy(HOURS_SCALE, ROUNDING);
    let std_labor_rate = cost_labor_dto::ensure_decimal(
        req.std_labor_rate.unwrap_or(Decimal::ZERO),
        "std_labor_rate",
        AMOUNT_SCALE,
        AMOUNT_MAX,
        true,
    )?;
    let std_overhead_cost = cost_labor_dto::ensure_decimal(
        req.std_overhead_cost.unwrap_or(Decimal::ZERO),
        "std_overhead_cost",
        AMOUNT_SCALE,
        AMOUNT_MAX,
        true,
    )?;
    Ok(StandardSpec {
        material_id,
        effective_from,
        effective_to,
        std_labor_hours,
        std_labor_rate,
        std_overhead_cost,
        status: normalize_status(req.status)?,
        remark: clean_remark(req.remark.clone())?,
        items: items_from_req(req.items.as_deref())?,
    })
}

/// 改请求 + 当前值 → 归一化规格（**纯逻辑**；`None` = 保持当前值）。
pub fn spec_from_update(
    req: &StandardUpdateReq,
    current: &StandardSpec,
) -> std::result::Result<StandardSpec, ApiError> {
    let material_id = match req
        .material_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => current.material_id,
        Some(raw) => cost_labor_dto::parse_uuid(raw, "material_id")?,
    };
    let fallback = (current.effective_from, current.effective_to);
    let (effective_from, mut effective_to) = window_from_req(
        req.period_code.as_deref(),
        req.effective_from.as_deref(),
        req.effective_to.as_deref(),
        Some(fallback),
    )?;
    if req.clear_effective_to.unwrap_or(false) {
        effective_to = None;
    }
    if let Some(end) = effective_to
        && end < effective_from
    {
        return Err(bad_request(format!(
            "effective_to（{end}）不能早于 effective_from（{effective_from}）"
        )));
    }
    let std_labor_hours = match req.std_labor_hours {
        None => current.std_labor_hours,
        Some(value) => cost_labor_dto::ensure_decimal(value, "std_labor_hours", HOURS_SCALE, HOURS_MAX, true)?
            .round_dp_with_strategy(HOURS_SCALE, ROUNDING),
    };
    let std_labor_rate = match req.std_labor_rate {
        None => current.std_labor_rate,
        Some(value) => cost_labor_dto::ensure_decimal(value, "std_labor_rate", AMOUNT_SCALE, AMOUNT_MAX, true)?,
    };
    let std_overhead_cost = match req.std_overhead_cost {
        None => current.std_overhead_cost,
        Some(value) => cost_labor_dto::ensure_decimal(value, "std_overhead_cost", AMOUNT_SCALE, AMOUNT_MAX, true)?,
    };
    let items = match req.items.as_deref() {
        None => current.items.clone(),
        Some(items) => items_from_req(Some(items))?,
    };
    let remark = match &req.remark {
        None => current.remark.clone(),
        Some(raw) => clean_remark(Some(raw.clone()))?,
    };
    Ok(StandardSpec {
        material_id,
        effective_from,
        effective_to,
        std_labor_hours,
        std_labor_rate,
        std_overhead_cost,
        status: match req.status {
            None => current.status,
            Some(value) => normalize_status(Some(value))?,
        },
        remark,
        items,
    })
}

/// 已生效版本被改写的字段清单（空 = 只改了允许的停止日 / 状态 / 备注）。
///
/// 判据只比**口径字段**：物料 / 生效起始日 / 标准工时 / 标准工资率 / 制造费用 / 标准 BOM。
/// 「生效起始日」在锁定区也算口径字段 —— 把起始日往后挪同样是在改写历史（原来那段没人管了）。
pub fn rewritten_fields(current: &StandardSpec, next: &StandardSpec) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if current.material_id != next.material_id {
        changed.push("物料");
    }
    if current.effective_from != next.effective_from {
        changed.push("生效起始日");
    }
    if current.std_labor_hours != next.std_labor_hours {
        changed.push("单位标准工时");
    }
    if current.std_labor_rate != next.std_labor_rate {
        changed.push("标准工资率");
    }
    if current.std_overhead_cost != next.std_overhead_cost {
        changed.push("单位标准制造费用");
    }
    if current.items != next.items {
        changed.push("标准 BOM 明细");
    }
    changed
}

// ==================== 出参 ====================

/// 版本行（列表 / 详情共用）。
#[derive(Debug, Clone, Serialize)]
pub struct StandardRes {
    /// 主键。
    pub id: String,
    /// 物料。
    pub material_id: String,
    /// 物料编码（实时 join `biz_material`；物料被删后为空）。
    pub material_code: Option<String>,
    /// 物料名称（实时 join）。
    pub material_name: Option<String>,
    /// 物料类型（RAW / SEMI / FINISHED / SERVICE）。
    pub material_type: Option<String>,
    /// 物料规格。
    pub material_spec: Option<String>,
    /// 版本号（同物料内递增）。
    pub version_no: i32,
    /// 生效起始日。
    pub effective_from: String,
    /// 停止日（空 = 至今有效）。
    pub effective_to: Option<String>,
    /// 状态（1 / 0）。
    pub status: i16,
    /// 状态文本。
    pub status_label: String,
    /// 发布状态（DRAFT / PENDING / PUBLISHED，M5-10）。
    pub publish_state: String,
    /// 发布状态文本（草稿 / 审批中 / 已发布）。
    pub publish_state_label: String,
    /// 在跑 / 最近一次的发布审批实例 id（PENDING 必有；PUBLISHED 作审计）。
    pub publish_instance_id: Option<String>,
    /// 最近一次发布时刻。
    pub published_at: Option<String>,
    /// 单位标准材料成本。
    pub std_material_cost: String,
    /// 单位标准人工成本。
    pub std_labor_cost: String,
    /// 单位标准制造费用。
    pub std_overhead_cost: String,
    /// 单位标准总成本。
    pub std_total_cost: String,
    /// 单位标准工时。
    pub std_labor_hours: String,
    /// 标准工资率。
    pub std_labor_rate: String,
    /// 标准 BOM 行数。
    pub items_count: i64,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: String,
    /// 更新时间。
    pub updated_at: String,
}

/// 标准 BOM 一行（出参）。
#[derive(Debug, Clone, Serialize)]
pub struct StandardItemRes {
    /// 明细主键。
    pub id: String,
    /// 行号。
    pub line_no: i32,
    /// 耗用物料。
    pub component_material_id: String,
    /// 耗用物料编码（实时 join）。
    pub component_material_code: Option<String>,
    /// 耗用物料名称（实时 join）。
    pub component_material_name: Option<String>,
    /// 单位标准用量。
    pub std_qty: String,
    /// 标准单价。
    pub std_price: String,
    /// 标准金额 = round(用量 × 单价, 6)。
    pub std_amount: String,
}

/// 版本详情（卡头 + 明细）。
#[derive(Debug, Clone, Serialize)]
pub struct StandardDetailRes {
    /// 卡头。
    pub head: StandardRes,
    /// 标准 BOM。
    pub items: Vec<StandardItemRes>,
}

/// 「某物料某期间生效版本」出参。
#[derive(Debug, Clone, Serialize)]
pub struct StandardEffectiveRes {
    /// 解析用的期间（传了 `period_code` 才有）。
    pub period_code: Option<String>,
    /// 实际取数日期（期间 = 期末；否则 = 调用方给的日期）。
    pub on_date: String,
    /// 版本详情。
    pub version: StandardDetailRes,
    /// 该期间内是否换过版（另一版也在期间内生效过）。
    pub switch_in_period: bool,
    /// 期间内在该版本之外还生效过的版本（切换提示；不含本版）。
    pub other_versions: Vec<StandardRes>,
    /// 给界面的口径说明（差异分析按哪一天的标准取数）。
    pub note: String,
}

/// 期间 × 物料 一行（版本表主视图）。
#[derive(Debug, Clone, Serialize)]
pub struct PeriodSheetRes {
    /// 版本行。
    pub head: StandardRes,
    /// 该期间内是否换过版（期间内还有别的启用版本）。
    pub switch_in_period: bool,
}

/// 生效 / 失效结果。
#[derive(Debug, Clone, Serialize)]
pub struct StandardStatusRes {
    /// 变更后的版本详情。
    pub version: StandardDetailRes,
    /// 结果说明。
    pub message: String,
    /// 需要操作人注意的后果（失效后该期间已无标准成本 / 期间内会留下缺口…）。
    pub warnings: Vec<String>,
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr as _;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    fn item(qty: &str, price: &str) -> StandardItemSpec {
        StandardItemSpec {
            component_material_id: Uuid::now_v7(),
            std_qty: dec(qty),
            std_price: dec(price),
        }
    }

    fn spec(from: &str, to: Option<&str>, status: i16) -> StandardSpec {
        StandardSpec {
            material_id: Uuid::from_u128(1),
            effective_from: date(from),
            effective_to: to.map(date),
            std_labor_hours: dec("2.0000"),
            std_labor_rate: dec("30.000000"),
            std_overhead_cost: dec("12.000000"),
            status,
            remark: None,
            items: vec![item("3.000000", "5.000000")],
        }
    }

    // ---------- 期间 ----------

    /// `yyyy-MM` 的期初 / 期末（含闰年 2 月与 12 月跨年）。
    #[test]
    fn period_window_is_inclusive_and_handles_month_ends() {
        assert_eq!(
            period_window("2026-11").expect("11 月"),
            (date("2026-11-01"), date("2026-11-30"))
        );
        assert_eq!(
            period_window("2026-02").expect("2026 年 2 月"),
            (date("2026-02-01"), date("2026-02-28"))
        );
        assert_eq!(
            period_window("2028-02").expect("闰年 2 月"),
            (date("2028-02-01"), date("2028-02-29"))
        );
        assert_eq!(
            period_window("2026-12").expect("12 月跨年"),
            (date("2026-12-01"), date("2026-12-31"))
        );
        assert_eq!(period_of_date(date("2026-03-31")), "2026-03");
    }

    /// 形状 / 值域不对一律 400（不猜：猜错一个月 = 整月报表错位）。
    #[test]
    fn period_window_rejects_bad_shapes() {
        for bad in ["", "2026-1", "2026-13", "2026-00", "26-01", "2026/01", "abc", "2026-1a"] {
            let err = period_window(bad).expect_err(bad);
            assert_eq!(err.status, 400, "{bad}");
            assert_eq!(err.code, error_code::PARAM_INVALID, "{bad}");
        }
        let err = period_window("1999-12").expect_err("年份越界");
        assert_eq!(err.status, 400);
    }

    // ---------- 三要素 ----------

    /// 料 = Σ 明细金额，工 = round(工时 × 费率, 6)，总 = 三者之和。
    #[test]
    fn amounts_follow_the_db_checks() {
        let mut s = spec("2026-11-01", None, STATUS_ACTIVE);
        s.items = vec![item("3.000000", "5.500000"), item("2.000000", "0.333333")];
        s.std_labor_hours = dec("2.5000");
        s.std_labor_rate = dec("31.111111");
        s.std_overhead_cost = dec("12.345678");
        let amounts = s.amounts();
        assert_eq!(amounts.material, dec("16.500000") + dec("0.666666"));
        assert_eq!(amounts.labor, dec("77.777778")); // 2.5 × 31.111111 = 77.7777775 → 半数远离零
        assert_eq!(amounts.overhead, dec("12.345678"));
        assert_eq!(
            amounts.total(),
            amounts.material + amounts.labor + amounts.overhead,
            "总成本必须恒等于三要素之和（ck_biz_cost_std_sum）"
        );
    }

    /// 明细金额按 6 位半舍远离零（与 PG round 同口径）。
    #[test]
    fn item_amount_rounds_half_away_from_zero() {
        assert_eq!(item("1.000000", "0.0000005").amount(), dec("0.000001"));
        assert_eq!(item("3.000000", "1.000000").amount(), dec("3.000000"));
    }

    /// 工时列是 `NUMERIC(18,4)`：超 4 位的输入提前 400（不静默丢精度），
    /// 4 位以内原样保留，人工成本按**存下来的**工时算（否则写库会 check_violation）。
    #[test]
    fn labor_hours_keep_column_scale_and_over_scale_is_rejected() {
        let mut req = StandardCreateReq {
            material_id: Some(Uuid::from_u128(9).to_string()),
            effective_from: Some("2026-11-01".to_string()),
            std_labor_hours: Some(dec("2.5")),
            std_labor_rate: Some(dec("10")),
            ..Default::default()
        };
        let built = spec_from_create(&req).expect("建规格");
        assert_eq!(built.std_labor_hours, dec("2.5"));
        assert_eq!(built.amounts().labor, dec("25.000000"));

        req.std_labor_hours = Some(dec("1.00005"));
        let err = spec_from_create(&req).expect_err("第 5 位小数应 400");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("最多 4 位小数"), "{}", err.msg);
    }

    // ---------- 区间 ----------

    /// 区间两端都含；端点相接不算重叠。
    #[test]
    fn windows_are_inclusive_and_touching_is_not_overlap() {
        assert!(window_covers(
            date("2026-11-01"),
            Some(date("2026-11-30")),
            date("2026-11-01")
        ));
        assert!(window_covers(
            date("2026-11-01"),
            Some(date("2026-11-30")),
            date("2026-11-30")
        ));
        assert!(!window_covers(
            date("2026-11-01"),
            Some(date("2026-11-30")),
            date("2026-12-01")
        ));
        assert!(window_covers(date("2026-11-01"), None, date("2030-01-01")), "至今有效");

        assert!(window_intersects(
            date("2026-11-01"),
            Some(date("2026-11-15")),
            date("2026-11-10"),
            date("2026-11-30")
        ));
        assert!(!window_intersects(
            date("2026-11-01"),
            Some(date("2026-11-15")),
            date("2026-11-16"),
            date("2026-11-30")
        ));

        assert!(!windows_overlap(
            date("2026-11-01"),
            Some(date("2026-11-30")),
            date("2026-12-01"),
            Some(date("2026-12-31"))
        ));
        assert!(windows_overlap(
            date("2026-11-01"),
            None,
            date("2026-12-01"),
            Some(date("2026-12-31"))
        ));
    }

    /// 重叠判定只看同物料的**启用**版本，且能排除自己。
    #[test]
    fn overlap_ignores_other_materials_inactive_rows_and_self() {
        let target = spec("2026-11-01", Some("2026-11-30"), STATUS_ACTIVE);
        let other_material = VersionView {
            id: Uuid::from_u128(11),
            material_id: Uuid::from_u128(2),
            version_no: 1,
            effective_from: date("2026-11-01"),
            effective_to: Some(date("2026-11-30")),
            status: STATUS_ACTIVE,
        };
        let inactive = VersionView {
            id: Uuid::from_u128(12),
            material_id: target.material_id,
            version_no: 2,
            effective_from: date("2026-11-10"),
            effective_to: Some(date("2026-11-20")),
            status: STATUS_INACTIVE,
        };
        let active = VersionView {
            id: Uuid::from_u128(13),
            material_id: target.material_id,
            version_no: 3,
            effective_from: date("2026-11-20"),
            effective_to: Some(date("2026-12-31")),
            status: STATUS_ACTIVE,
        };
        let views = vec![other_material.clone(), inactive.clone(), active.clone()];

        let hit = find_overlap(&views, &target, None).expect("应命中启用的同物料版本");
        assert_eq!(hit.id, active.id, "失效/异物料不许挡路");
        assert!(
            find_overlap(&views, &target, Some(active.id)).is_none(),
            "视图里只有这一条启用的同物料版本时，排除自己后应放行"
        );
        assert!(
            find_overlap(&views[..2], &target, None).is_none(),
            "只剩异物料 + 失效版本时应放行（先失效再启用这条正路）"
        );

        // 按日期解析：2026-11-25 落在 active 上，2026-11-05 什么都不落
        assert_eq!(
            resolve_on(&views, target.material_id, date("2026-11-25")).map(|v| v.id),
            Some(active.id)
        );
        assert!(
            resolve_on(&views, target.material_id, date("2026-11-05")).is_none(),
            "11-05 只有失效版本 ⇒ 无生效版本"
        );
    }

    // ---------- 建 / 改 ----------

    /// 整期写法：`period_code` 直接落整期窗口；与显式日期矛盾时 400。
    #[test]
    fn create_accepts_period_code_and_rejects_contradiction() {
        let req = StandardCreateReq {
            material_id: Some(Uuid::from_u128(7).to_string()),
            period_code: Some("2026-11".to_string()),
            items: Some(vec![StandardItemReq {
                component_material_id: Some(Uuid::from_u128(8).to_string()),
                std_qty: Some(dec("2")),
                std_price: Some(dec("3.5")),
            }]),
            ..Default::default()
        };
        let built = spec_from_create(&req).expect("建规格");
        assert_eq!(built.effective_from, date("2026-11-01"));
        assert_eq!(built.effective_to, Some(date("2026-11-30")));
        assert_eq!(built.status, STATUS_ACTIVE, "默认启用");
        assert_eq!(built.amounts().material, dec("7.000000"));

        let conflict = StandardCreateReq {
            period_code: Some("2026-11".to_string()),
            effective_from: Some("2026-11-15".to_string()),
            ..req.clone()
        };
        let err = spec_from_create(&conflict).expect_err("期间与生效日矛盾");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("矛盾"), "{}", err.msg);
    }

    /// 建卡缺字段 / 明细重复物料 / 用工量为负都要 400，且报文点名第几行。
    #[test]
    fn create_rejects_bad_inputs_with_line_numbers() {
        let missing_material = StandardCreateReq {
            effective_from: Some("2026-11-01".to_string()),
            ..Default::default()
        };
        assert_eq!(spec_from_create(&missing_material).expect_err("缺物料").status, 400);

        let missing_window = StandardCreateReq {
            material_id: Some(Uuid::from_u128(7).to_string()),
            ..Default::default()
        };
        assert!(
            spec_from_create(&missing_window)
                .expect_err("缺生效日")
                .msg
                .contains("period_code")
        );

        let component = Uuid::from_u128(8).to_string();
        let duplicate = StandardCreateReq {
            material_id: Some(Uuid::from_u128(7).to_string()),
            period_code: Some("2026-11".to_string()),
            items: Some(vec![
                StandardItemReq {
                    component_material_id: Some(component.clone()),
                    std_qty: Some(dec("1")),
                    std_price: Some(dec("2")),
                },
                StandardItemReq {
                    component_material_id: Some(component),
                    std_qty: Some(dec("1")),
                    std_price: Some(dec("2")),
                },
            ]),
            ..Default::default()
        };
        let err = spec_from_create(&duplicate).expect_err("重复物料");
        assert!(err.msg.contains("第 2 行"), "{}", err.msg);

        let negative = StandardCreateReq {
            material_id: Some(Uuid::from_u128(7).to_string()),
            period_code: Some("2026-11".to_string()),
            std_overhead_cost: Some(dec("-1")),
            ..Default::default()
        };
        assert_eq!(spec_from_create(&negative).expect_err("负数费用").status, 400);
    }

    /// 改请求只覆盖给了的字段；`clear_effective_to` 能把停止日清回「至今有效」。
    #[test]
    fn update_merges_only_provided_fields_and_can_clear_end_date() {
        let current = spec("2026-11-01", Some("2026-11-30"), STATUS_ACTIVE);
        let untouched = spec_from_update(&StandardUpdateReq::default(), &current).expect("原样");
        assert_eq!(untouched, current, "全空的改请求 = 不改");

        let cleared = spec_from_update(
            &StandardUpdateReq {
                clear_effective_to: Some(true),
                remark: Some("".to_string()),
                ..Default::default()
            },
            &current,
        )
        .expect("清停止日");
        assert_eq!(cleared.effective_to, None);
        assert_eq!(cleared.remark, None, "remark 传空串 = 清空");

        let replaced = spec_from_update(
            &StandardUpdateReq {
                items: Some(vec![]),
                std_labor_rate: Some(dec("40")),
                ..Default::default()
            },
            &current,
        )
        .expect("换明细");
        assert!(replaced.items.is_empty(), "Some([]) = 清空明细");
        assert_eq!(replaced.amounts().material, Decimal::ZERO);
        assert_eq!(replaced.std_labor_rate, dec("40"));
    }

    /// 「已生效版本被改写了哪些口径」的判据：停止日 / 状态 / 备注不算改写。
    #[test]
    fn rewritten_fields_only_counts_cost_basis() {
        let current = spec("2026-11-01", None, STATUS_ACTIVE);
        let mut next = current.clone();
        next.effective_to = Some(date("2026-11-30"));
        next.remark = Some("收口".to_string());
        next.status = STATUS_INACTIVE;
        assert!(
            rewritten_fields(&current, &next).is_empty(),
            "改停止日 / 备注 / 状态是允许的动作"
        );

        for mutate in [
            (|s: &mut StandardSpec| s.std_labor_rate = dec("31")) as fn(&mut StandardSpec),
            |s: &mut StandardSpec| s.std_labor_hours = dec("2.5"),
            |s: &mut StandardSpec| s.std_overhead_cost = dec("13"),
            |s: &mut StandardSpec| s.effective_from = date("2026-11-02"),
            |s: &mut StandardSpec| s.material_id = Uuid::from_u128(3),
            |s: &mut StandardSpec| s.items = vec![item("4", "5")],
        ] {
            let mut changed = current.clone();
            mutate(&mut changed);
            assert_eq!(rewritten_fields(&current, &changed).len(), 1);
        }
    }

    /// 报文文本：金额 6 位、工时 4 位、区间文本「至今」。
    #[test]
    fn text_helpers_are_stable() {
        assert_eq!(amount_text(dec("1.5")), "1.500000");
        assert_eq!(hours_text(dec("2.5")), "2.5000");
        assert_eq!(window_text(date("2026-11-01"), None), "2026-11-01 ~ 至今");
        assert_eq!(
            window_text(date("2026-11-01"), Some(date("2026-11-30"))),
            "2026-11-01 ~ 2026-11-30"
        );
        assert_eq!(status_label(STATUS_ACTIVE), "生效");
        assert_eq!(status_label(STATUS_INACTIVE), "失效");
    }

    /// 三个业务码与 kernel 码表逐字一致（改码值 = 前端文案错位）。
    #[test]
    fn error_codes_match_the_kernel_table() {
        assert_eq!(CODE_OVERLAP, 66108);
        assert_eq!(CODE_LOCKED, 66109);
        assert_eq!(CODE_DELETE_IN_USE, 66110);
    }
}
