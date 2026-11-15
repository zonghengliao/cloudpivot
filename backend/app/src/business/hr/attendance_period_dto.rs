//! 考勤期间入参 / 回执契约（`biz_attendance_period` · HR-19）。
//!
//! 设计 §5.2 的三条接口形状 + 本卡按卡面「期间 CRUD / 汇总重算」补的两条（下面「接口一览」
//! 里标 DK 的两行，偏差理由见 [`crate::business::hr::attendance_period_handler`] 模块文档）。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 入参 | 回执 |
//! |:--:|---|---|---|---|
//! | 18 | GET | `/business/hr/attendance/period/page` | [`PeriodPageQuery`] | `PageData<Vec<[`AttendancePeriodRes`]>>` |
//! | 19 | POST | `/business/hr/attendance/period` | [`PeriodCreateReq`] | [`AttendancePeriodRes`] |
//! | 20 | POST | `/business/hr/attendance/period/{id}/status` | [`PeriodStatusReq`] | [`AttendancePeriodRes`] |
//! | DK | PUT | `/business/hr/attendance/period/{id}` | [`PeriodUpdateReq`] | [`AttendancePeriodRes`] |
//! | DK | DELETE | `/business/hr/attendance/period/{id}` | — | [`AttendancePeriodRes`]（删除前的快照） |
//! | DK | POST | `/business/hr/attendance/period/{id}/recalc` | — | [`PeriodRecalcRes`] |
//!
//! # 字段口径
//!
//! - 天数 / 小时是 `NUMERIC(12,2)` → `Decimal` → JSON 字符串（设计 §5.1：绝不下发 `f64`，
//!   「合计 = 各行之和」不许在界面上差一分）；
//! - `period_code` 是 `yyyy-MM`（与 `biz_accounting_period.period_code` 同口径），
//!   **月份必须是 01~12**：宽正则 `^[0-9]{4}-[0-9]{2}$` 会放过 `2026-13`（ARCH-9-1 踩过），
//!   库层 `ck_biz_att_period_code` 也是窄正则，这里先按同一口径挡一次；
//! - `period_start` / `period_end` **不要求落在 `period_code` 的自然月内**：设计 §3.1 ④ 的列注释
//!   写明支持「26 日 ~ 次月 25 日」的计薪周期（`2026-11` 批次可以是 `2026-10-26 ~ 2026-11-25`），
//!   硬对齐会把这个客户的合法口径挡在门外。只校验「止 ≥ 起」+ 跨度 ≤ [`MAX_SPAN_DAYS`] 天。
//!
//! # `submitted_*` / `locked_*` 的语义（解锁不清空）
//!
//! 四列记的是**最近一次**对应动作的时间与操作人：解锁（`LOCKED → DRAFT`）只把 `status` 改回
//! `DRAFT`，`locked_at/locked_by` **原样保留**。理由是「谁在什么时候锁过」是审计信息，
//! 清空等于把轨迹抹掉；真正的流转轨迹在 `biz_status_log`（`state_machine::transition` 写的）。
//! 前端按 `status` 渲染按钮，不要把 `locked_at` 有值当成「当前已锁定」——`status` 才是唯一真相
//! （出参同时给 `locked: bool` 就是为了让前端别自己判）。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;

// ==================== 状态域（与 V0383 的 ck_biz_att_period_status 逐字一致） ====================

/// 草稿（可导入 / 可改 / 可删）。
pub const STATUS_DRAFT: &str = "DRAFT";

/// 已提交（可继续重跑导入，设计 §4.2.3「提交后还能重跑导入」）。
pub const STATUS_SUBMITTED: &str = "SUBMITTED";

/// 已锁定（记录冻结，薪资试算的稳定基线，设计 §4.2.3）。
pub const STATUS_LOCKED: &str = "LOCKED";

/// 三个状态值（顺序 = 状态机正向顺序）。
pub const STATUS_VALUES: [&str; 3] = [STATUS_DRAFT, STATUS_SUBMITTED, STATUS_LOCKED];

/// 状态中文名（前端列表 / 回执的 `status_label`，与设计 §6 的页面文案一致）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SUBMITTED => "已提交",
        STATUS_LOCKED => "已锁定",
        // 库层 CHECK 挡住其它值；走到这里说明代码与迁移漂移了，回显码值比空白强
        _ => "未知",
    }
}

// ==================== 长度 / 跨度上限 ====================

/// `period_code VARCHAR(7)`。
pub const PERIOD_CODE_LEN: usize = 7;

/// 备注上限（列是 `TEXT`，挡的是「把整份考勤说明贴进来」）。
pub const MAX_REMARK: usize = 500;

/// 一个考勤周期的最大跨度（天，含首尾）。
///
/// 设计 §3.1 ④ 是**月度**批次：自然月 28~31 天、或「26 日 ~ 次月 25 日」的 31 天计薪周期。
/// 31 天既是这两种口径的上界，也能挡住 `2026-01-01 ~ 2026-12-31` 这类把整年填成一期的笔误。
pub const MAX_SPAN_DAYS: i64 = 31;

/// 带原因的状态边（解锁）的原因最小字数。
///
/// 设计 §4.2.3 只写「解锁(给原因)」，没有字数；但解锁解开的是一份**已经被工资试算用过**的基线
/// （设计 §4.2.3 末条），一个「x」交代不了。5 字与员工状态机的原因类动作同口径
/// （`state_machine::EMPLOYEE_REASON_EDGES`，设计 §4.2.1）。
pub const MIN_REASON_CHARS: usize = 5;

// ==================== 入参 ====================

/// 列表筛选（设计 §5.2 #18：`period_code` / `status` / 分页）。
///
/// `status[]` 的落地形态与员工列表一致：查询串**逗号分隔**（`status=DRAFT,LOCKED`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PeriodPageQuery {
    /// 关键字（`period_code` 前缀 / 模糊，`LIKE %kw%`）。
    #[serde(default)]
    pub keyword: Option<String>,
    /// 状态多选，逗号分隔。
    #[serde(default)]
    pub status: Option<String>,
    /// 年份（`period_code` 前 4 位，`2026`）。
    #[serde(default)]
    pub year: Option<i32>,
    /// 页码（从 1 开始）。
    #[serde(default)]
    pub page: Option<u64>,
    /// 每页条数（默认 20，上限 200）。
    #[serde(default)]
    pub page_size: Option<u64>,
}

/// 建期间入参（设计 §5.2 #19：`period_code` + `period_start/end`）。
///
/// 字段都是 `Option`：把「没填」与「填错」区分成两条 400 人话，而不是让 serde 抛
/// `missing field \`period_code\``（前端只能直出英文）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PeriodCreateReq {
    /// 考勤月 `yyyy-MM`（必填）。
    #[serde(default)]
    pub period_code: Option<String>,
    /// 起（含，`yyyy-MM-dd`，必填）。
    #[serde(default)]
    pub period_start: Option<String>,
    /// 止（含，`yyyy-MM-dd`，必填）。
    #[serde(default)]
    pub period_end: Option<String>,
    /// 备注（开期说明 / 计薪口径）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 改期间入参（`PUT /business/hr/attendance/period/{id}`）。
///
/// **部分更新**语义：缺省的字段保持库里的值。`period_code` 不在可改列里 ——
/// 它是期间的人工标识（报表、薪资单、巡检脚本都按它对齐），改名要连带解释「历史口径怎么办」，
/// 属独立议题；错了就删掉重建（草稿期）或另开一期。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PeriodUpdateReq {
    /// 起（含）；缺省 = 不改。
    #[serde(default)]
    pub period_start: Option<String>,
    /// 止（含）；缺省 = 不改。
    #[serde(default)]
    pub period_end: Option<String>,
    /// 备注；**显式传空串 = 清空备注**（缺省 = 不改）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 状态变更入参（设计 §5.2 #20：提交 / 锁定 / 解锁）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PeriodStatusReq {
    /// 目标状态（`SUBMITTED` / `LOCKED` / `DRAFT`）。
    #[serde(default)]
    pub to_status: Option<String>,
    /// 原因（解锁必填；其余边忽略）。
    #[serde(default)]
    pub reason: Option<String>,
}

// ==================== 校验后的载荷（接口层直接吃这一份） ====================

/// 建期间的校验结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodCreate {
    /// 考勤月（已 trim，`yyyy-MM`）。
    pub period_code: String,
    /// 起（含）。
    pub period_start: NaiveDate,
    /// 止（含）。
    pub period_end: NaiveDate,
    /// 备注（空串 → `None`）。
    pub remark: Option<String>,
}

/// 改期间的校验结果（已与库里的当前值合并，全量三列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodPatch {
    /// 起（含）。
    pub period_start: NaiveDate,
    /// 止（含）。
    pub period_end: NaiveDate,
    /// 备注。
    pub remark: Option<String>,
    /// 与库里当前值相比是否有改动（无改动不写库、不写状态日志）。
    pub changed: bool,
}

/// 状态变更的校验结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChange {
    /// 目标状态。
    pub to_status: String,
    /// 原因（只有需要原因的边才带值，已 trim）。
    pub reason: Option<String>,
}

/// 列表筛选（校验后的形态；`statuses` 已拆开并逐项校验过域）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeriodFilter {
    /// 关键字（`period_code` 模糊）。
    pub keyword: Option<String>,
    /// 状态多选（空 = 全部）。
    pub statuses: Vec<String>,
    /// 年份（`period_code` 前 4 位）。
    pub year: Option<i32>,
}

// ==================== 校验 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `409 + 10003`：状态没变化 / 并发已被处理（不是入参格式错）。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

fn check_len(text: &str, field: &str, max: usize) -> std::result::Result<(), ApiError> {
    if text.chars().count() > max {
        return Err(bad_request(format!(
            "{field} 最长 {max} 个字符（收到 {} 个）",
            text.chars().count()
        )));
    }
    Ok(())
}

/// `yyyy-MM-dd`。
fn parse_date(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| bad_request(format!("{field} 日期格式应为 yyyy-MM-dd（收到 `{text}`）")))
}

/// `yyyy-MM`（月份必须是 01~12；与库层 `ck_biz_att_period_code` 同口径）。
///
/// 不用正则：这里要的是**点名哪一段错**的人话（「月份只能是 01~12」比「正则不匹配」有用），
/// 而且生产代码依赖正则 crate 要多一条依赖理由。
pub fn check_period_code(raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request("period_code（考勤月）不能为空，格式 yyyy-MM"));
    }
    if text.chars().count() != PERIOD_CODE_LEN {
        return Err(bad_request(format!(
            "period_code 必须是 7 个字符的 yyyy-MM（收到 `{text}`，{} 个字符）",
            text.chars().count()
        )));
    }
    let bytes = text.as_bytes();
    if bytes[4] != b'-' {
        return Err(bad_request(format!(
            "period_code 第 5 个字符必须是 `-`（收到 `{text}`，正确形态如 `2026-11`）"
        )));
    }
    if !bytes[..4].iter().all(u8::is_ascii_digit) {
        return Err(bad_request(format!(
            "period_code 的前 4 位必须是年份数字（收到 `{text}`，正确形态如 `2026-11`）"
        )));
    }
    if !bytes[5..].iter().all(u8::is_ascii_digit) {
        return Err(bad_request(format!(
            "period_code 的后 2 位必须是月份数字（收到 `{text}`，正确形态如 `2026-11`）"
        )));
    }
    let month: u32 = text[5..].parse().map_err(|_| {
        bad_request(format!(
            "period_code 的后 2 位必须是月份数字（收到 `{text}`，正确形态如 `2026-11`）"
        ))
    })?;
    if !(1..=12).contains(&month) {
        return Err(bad_request(format!(
            "period_code 的月份只能是 01~12（收到 `{text}`）—— 月份写 00 / 13 的批次永远不会有考勤数据，\
             库层 ck_biz_att_period_code 与巡检都会拒"
        )));
    }
    Ok(text.to_string())
}

/// 区间校验（`止 ≥ 起` + 跨度 ≤ [`MAX_SPAN_DAYS`] 天）。
fn check_range(start: NaiveDate, end: NaiveDate) -> std::result::Result<(), ApiError> {
    if end < start {
        return Err(bad_request(format!(
            "period_end（{end}）不能早于 period_start（{start}）"
        )));
    }
    let span = (end - start).num_days() + 1;
    if span > MAX_SPAN_DAYS {
        return Err(bad_request(format!(
            "考勤周期跨度 {span} 天（{start} ~ {end}）：月度批次最长 {MAX_SPAN_DAYS} 天\
             （自然月或「26 日 ~ 次月 25 日」的计薪周期）；跨季度/跨年的统计请分开建期"
        )));
    }
    Ok(())
}

/// 备注归一：trim 后空串 → `None`，超长 400。
fn normalize_remark(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => {
            check_len(text, "remark", MAX_REMARK)?;
            Ok(Some(text.to_string()))
        }
    }
}

/// 建期间入参 → 校验后的载荷（**纯函数**：不连库就能把这些口径测掉）。
pub fn parse_create(req: &PeriodCreateReq) -> std::result::Result<PeriodCreate, ApiError> {
    let period_code = check_period_code(req.period_code.as_deref().unwrap_or_default())?;
    let start_raw = req.period_start.as_deref().unwrap_or_default();
    let end_raw = req.period_end.as_deref().unwrap_or_default();
    let period_start = parse_date(start_raw, "period_start")?;
    let period_end = parse_date(end_raw, "period_end")?;
    check_range(period_start, period_end)?;
    Ok(PeriodCreate {
        period_code,
        period_start,
        period_end,
        remark: normalize_remark(req.remark.as_deref())?,
    })
}

/// 改期间入参 + 库里的当前值 → 全量三列 + `changed` 标记。
///
/// # 参数
///
/// - `current`：库里当前的三列（`period_start` / `period_end` / `remark`）；
/// - `req`：部分更新载荷（缺省 = 不改）。
///
/// # 错误
///
/// 字段格式错 → 400；合并后区间不合法 → 400（点明最终生效的那对日期）。
pub fn parse_update(
    req: &PeriodUpdateReq,
    current: (NaiveDate, NaiveDate, Option<&str>),
) -> std::result::Result<PeriodPatch, ApiError> {
    let period_start = match req.period_start.as_deref().map(str::trim) {
        None | Some("") => current.0,
        Some(raw) => parse_date(raw, "period_start")?,
    };
    let period_end = match req.period_end.as_deref().map(str::trim) {
        None | Some("") => current.1,
        Some(raw) => parse_date(raw, "period_end")?,
    };
    check_range(period_start, period_end)?;
    // 备注：缺省 = 不改；显式空串 = 清空（`normalize_remark` 把空串归一成 None）
    let remark = match req.remark.as_deref() {
        None => current.2.map(str::to_string),
        Some(raw) => normalize_remark(Some(raw))?,
    };
    let changed = period_start != current.0
        || period_end != current.1
        || remark.as_deref() != current.2.map(str::trim).filter(|text| !text.is_empty());
    Ok(PeriodPatch {
        period_start,
        period_end,
        remark,
        changed,
    })
}

/// 状态变更入参 + 当前状态 → 目标状态 + 原因（边是否合法交给状态机判，这里只判形状）。
///
/// # 为什么正文里不判边
///
/// 合法边唯一真相是 `shared::state_machine::ATTENDANCE_PERIOD_EDGES`（HR-08 注册）。
/// 在 DTO 里再写一份 `match (from, to)` 就有两个答案，早晚漂移（设计 §4.2.1 的硬口径）。
pub fn parse_status(req: &PeriodStatusReq, from: &str) -> std::result::Result<StatusChange, ApiError> {
    let to_status = req
        .to_status
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("to_status 不能为空：提交用 `SUBMITTED`、锁定用 `LOCKED`、解锁用 `DRAFT`"))?
        .to_string();
    if !STATUS_VALUES.contains(&to_status.as_str()) {
        return Err(bad_request(format!(
            "to_status 只能是 {}（收到 `{to_status}`）",
            STATUS_VALUES.join(" / ")
        )));
    }
    if to_status == from {
        return Err(conflict(format!(
            "考勤期间当前就是 {from}（{}），状态没有变化：不要重复提交/锁定",
            status_label(from)
        )));
    }
    let reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    // 解锁必须给原因（设计 §4.2.3）。边表里 `LOCKED → DRAFT` 是 reason 边，这里提前给
    // 人话（状态机的 400 报文是通用的「该流转需要原因」，不告诉 HR 为什么需要）。
    if from == STATUS_LOCKED && to_status == STATUS_DRAFT {
        let reason = reason.clone().ok_or_else(|| {
            bad_request(format!(
                "解锁必须填原因（≥ {MIN_REASON_CHARS} 字）：锁定期间是薪资试算的基线，\
                 解开它会让已经算过的工资与考勤对不上，要能交代为什么解"
            ))
        })?;
        if reason.chars().count() < MIN_REASON_CHARS {
            return Err(bad_request(format!(
                "解锁原因至少 {MIN_REASON_CHARS} 个字（收到 {} 个）：{}",
                reason.chars().count(),
                reason
            )));
        }
        return Ok(StatusChange {
            to_status,
            reason: Some(reason),
        });
    }
    Ok(StatusChange {
        to_status,
        // 非 reason 边：忽略前端可能带上来的原因（状态日志只记流转与操作人）
        reason: None,
    })
}

// ==================== 列表筛选 ====================

/// 默认每页条数。
pub const DEFAULT_PAGE_SIZE: u64 = 20;

/// 每页上限（挡住 `page_size=100000` 把列表变成导出）。
pub const MAX_PAGE_SIZE: u64 = 200;

/// 分页参数归一（`page` 从 1 起，`page_size` 夹在 1..=[`MAX_PAGE_SIZE`]）。
pub fn normalize_page(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    let page = page.unwrap_or(1).max(1);
    let page_size = page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE);
    (page, page_size)
}

/// 查询串 → [`PeriodFilter`]（**纯函数**：逗号拆分 / 状态域 / 年份范围不连库就能测）。
pub fn filter_of(query: &PeriodPageQuery) -> std::result::Result<PeriodFilter, ApiError> {
    let keyword = query
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);

    let mut statuses: Vec<String> = Vec::new();
    if let Some(raw) = query.status.as_deref() {
        for token in raw.split(',') {
            let text = token.trim();
            if text.is_empty() {
                continue;
            }
            if !STATUS_VALUES.contains(&text) {
                return Err(bad_request(format!(
                    "status 只能是 {}（收到 `{text}`；多选用逗号分隔，如 status=DRAFT,LOCKED）",
                    STATUS_VALUES.join(" / ")
                )));
            }
            statuses.push(text.to_string());
        }
    }

    // 年份：`period_code` 是 `yyyy-MM`，所以 1900~2999 之外一定是手滑（`year=26` / `year=20261`）
    let year = match query.year {
        None => None,
        Some(year) if (1900..=2999).contains(&year) => Some(year),
        Some(year) => {
            return Err(bad_request(format!("year 应是四位数年份（收到 {year}；如 year=2026）")));
        }
    };

    Ok(PeriodFilter {
        keyword,
        statuses,
        year,
    })
}

// ==================== 出参 ====================

/// 四个汇总快照列（重算前后对比 + 列表里的合计行都用这一份形状）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PeriodTotals {
    /// 期间内考勤记录条数（= 参与本期的员工数，一人一行）。
    pub employee_count: i32,
    /// 应出勤天数合计。
    pub should_days_total: Decimal,
    /// 实际出勤天数合计。
    pub actual_days_total: Decimal,
    /// 加班小时合计（平时 + 休息日 + 法定节假日）。
    pub ot_hours_total: Decimal,
}

impl PeriodTotals {
    /// 是否完全相同（重算幂等性的判据：两次重算之间 `changed` 必须为 `false`）。
    pub fn same_as(&self, other: &Self) -> bool {
        self.employee_count == other.employee_count
            && self.should_days_total == other.should_days_total
            && self.actual_days_total == other.actual_days_total
            && self.ot_hours_total == other.ot_hours_total
    }
}

/// 一条考勤期间的出参（列表行 / 建·改·状态回执共用同一份形状）。
#[derive(Debug, Clone, Serialize)]
pub struct AttendancePeriodRes {
    /// 主键（uuid 出参一律字符串）。
    pub id: String,
    /// 考勤月 `yyyy-MM`。
    pub period_code: String,
    /// 起（含）。
    pub period_start: NaiveDate,
    /// 止（含）。
    pub period_end: NaiveDate,
    /// 状态码值。
    pub status: String,
    /// 状态中文名。
    pub status_label: &'static str,
    /// 是否已锁定（`status == LOCKED`）：前端据此禁掉「改 / 删 / 重算」按钮。
    pub locked: bool,
    /// 期间内记录条数（汇总快照）。
    pub employee_count: i32,
    /// 应出勤天数合计。
    pub should_days_total: Decimal,
    /// 实际出勤天数合计。
    pub actual_days_total: Decimal,
    /// 加班小时合计。
    pub ot_hours_total: Decimal,
    /// 最近一次导入的 `sys_import_task.id`（HR-21 回写；未导入为 `null`）。
    pub import_task_id: Option<String>,
    /// 最近一次提交时间（ISO8601 `+08:00`）。
    pub submitted_at: Option<String>,
    /// 最近一次提交人（uuid）。
    pub submitted_by: Option<String>,
    /// 最近一次提交人姓名（`sys_user.real_name`，join 不到回退用户名）。
    pub submitted_by_name: Option<String>,
    /// 最近一次锁定时间（解锁后**保留**，见模块文档）。
    pub locked_at: Option<String>,
    /// 最近一次锁定人（uuid）。
    pub locked_by: Option<String>,
    /// 最近一次锁定人姓名。
    pub locked_by_name: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: String,
    /// 最后修改时间。
    pub updated_at: String,
}

impl AttendancePeriodRes {
    /// 汇总快照投影（重算回执的 `before` / `after` 用）。
    pub fn totals(&self) -> PeriodTotals {
        PeriodTotals {
            employee_count: self.employee_count,
            should_days_total: self.should_days_total,
            actual_days_total: self.actual_days_total,
            ot_hours_total: self.ot_hours_total,
        }
    }
}

/// 汇总重算回执（`POST /business/hr/attendance/period/{id}/recalc`）。
///
/// # 为什么把 `before` / `after` / `changed` 都回出去
///
/// 「重算幂等」是卡面验收项，但用户在界面上要能**看见**它：第一次点重算 `changed=true`
/// 且数字变了，第二次点 `changed=false` 且数字一模一样。只回最新数字的话，
/// 「幂等」只能靠工程师连库比对。
#[derive(Debug, Clone, Serialize)]
pub struct PeriodRecalcRes {
    /// 本次参与汇总的考勤记录条数（`biz_attendance_record` 未删除行）。
    pub scanned: i64,
    /// 重算前的汇总快照。
    pub before: PeriodTotals,
    /// 重算后的汇总快照。
    pub after: PeriodTotals,
    /// 快照是否变了（`false` = 本次重算没有改变任何数字 = 幂等）。
    pub changed: bool,
    /// 重算后的期间全量（前端直接拿这份刷界面）。
    pub period: AttendancePeriodRes,
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
    }

    fn money(text: &str) -> Decimal {
        Decimal::from_str_exact(text).expect("合法金额")
    }

    fn create(period_code: &str, start: &str, end: &str) -> PeriodCreateReq {
        PeriodCreateReq {
            period_code: Some(period_code.to_string()),
            period_start: Some(start.to_string()),
            period_end: Some(end.to_string()),
            remark: None,
        }
    }

    /// 正常建期：`2026-11` + 26 日 ~ 次月 25 日的计薪周期（设计 §3.1 ④ 明确支持）。
    #[test]
    fn parse_create_accepts_payroll_cycle() {
        let parsed = parse_create(&create("2026-11", "2026-10-26", "2026-11-25")).expect("合法");
        assert_eq!(parsed.period_code, "2026-11");
        assert_eq!(parsed.period_start, day("2026-10-26"));
        assert_eq!(parsed.period_end, day("2026-11-25"));
        assert!(parsed.remark.is_none());
    }

    /// `period_code` 的月份必须是 01~12（`2026-13` / `2026-00` 都要拦，宽正则放过它们）。
    #[test]
    fn parse_create_rejects_bad_period_code() {
        for code in ["2026-13", "2026-00", "2026-1", "26-11", "2026/11", "", "2026-11-01"] {
            let error = parse_create(&create(code, "2026-11-01", "2026-11-30")).expect_err(&format!("`{code}` 应被拦"));
            assert_eq!(error.status, 400, "`{code}` → 400");
            assert!(
                error.msg.contains("period_code"),
                "报文要点名字段（`{code}`）：{}",
                error.msg
            );
        }
    }

    /// 区间：止 ≥ 起、跨度 ≤ 31 天（含首尾）。自然月 31 天与 26-25 周期都放行。
    #[test]
    fn parse_create_checks_range_and_span() {
        assert!(
            parse_create(&create("2026-01", "2026-01-01", "2026-01-31")).is_ok(),
            "31 天自然月"
        );
        assert!(
            parse_create(&create("2026-02", "2026-02-01", "2026-02-28")).is_ok(),
            "28 天"
        );
        assert!(
            parse_create(&create("2026-11", "2026-10-26", "2026-11-25")).is_ok(),
            "31 天计薪周期"
        );

        let reversed = parse_create(&create("2026-11", "2026-11-25", "2026-11-01")).expect_err("止早于起要拦");
        assert_eq!(reversed.status, 400);
        assert!(reversed.msg.contains("不能早于"), "报文：{}", reversed.msg);

        let too_long = parse_create(&create("2026-01", "2026-01-01", "2026-03-01")).expect_err("跨两月要拦");
        assert!(too_long.msg.contains("跨度"), "报文：{}", too_long.msg);
        assert!(too_long.msg.contains("31"), "报文要点明上限：{}", too_long.msg);
    }

    /// 改期间：缺省字段保持原值；显式空串清空备注；`changed` 正确。
    #[test]
    fn parse_update_merges_with_current() {
        let current = (day("2026-11-01"), day("2026-11-30"), Some("自然月"));
        // 只改备注
        let patch = parse_update(
            &PeriodUpdateReq {
                remark: Some("  按 26-25 计薪周期  ".to_string()),
                ..PeriodUpdateReq::default()
            },
            current,
        )
        .expect("合法");
        assert_eq!(patch.period_start, current.0, "未传的字段不动");
        assert_eq!(patch.period_end, current.1);
        assert_eq!(patch.remark.as_deref(), Some("按 26-25 计薪周期"), "trim 后落库");
        assert!(patch.changed);

        // 清空备注
        let cleared = parse_update(
            &PeriodUpdateReq {
                remark: Some(String::new()),
                ..PeriodUpdateReq::default()
            },
            current,
        )
        .expect("合法");
        assert_eq!(cleared.remark, None);
        assert!(cleared.changed);

        // 原样重放 → changed=false（handler 会翻成 409，不写库不写日志）
        let same = parse_update(&PeriodUpdateReq::default(), current).expect("合法");
        assert!(!same.changed, "什么都没变");
    }

    /// 改期间也要判合并后的区间（只改一端就可能把区间弄反）。
    #[test]
    fn parse_update_validates_merged_range() {
        let current = (day("2026-11-01"), day("2026-11-30"), None);
        let error = parse_update(
            &PeriodUpdateReq {
                period_start: Some("2026-12-01".to_string()),
                ..PeriodUpdateReq::default()
            },
            current,
        )
        .expect_err("起跑到止之后要拦");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("不能早于"), "报文：{}", error.msg);
    }

    /// 状态：三值域、同态 409、解锁必须给 ≥5 字原因、非 reason 边忽略原因。
    #[test]
    fn parse_status_guards_domain_and_reason() {
        let ok = parse_status(
            &PeriodStatusReq {
                to_status: Some("SUBMITTED".to_string()),
                reason: Some("随便写的原因".to_string()),
            },
            STATUS_DRAFT,
        )
        .expect("DRAFT→SUBMITTED 合法");
        assert_eq!(ok.to_status, STATUS_SUBMITTED);
        assert_eq!(ok.reason, None, "非 reason 边不写原因（状态日志只记流转）");

        let same = parse_status(
            &PeriodStatusReq {
                to_status: Some("DRAFT".to_string()),
                ..PeriodStatusReq::default()
            },
            STATUS_DRAFT,
        )
        .expect_err("同态要拦");
        assert_eq!(same.status, 409, "同态不是入参格式错");

        let bad = parse_status(
            &PeriodStatusReq {
                to_status: Some("APPROVED".to_string()),
                ..PeriodStatusReq::default()
            },
            STATUS_DRAFT,
        )
        .expect_err("域外要拦");
        assert_eq!(bad.status, 400);
        assert!(bad.msg.contains("SUBMITTED"), "报文要点明可选值：{}", bad.msg);

        let missing = parse_status(
            &PeriodStatusReq {
                to_status: Some("DRAFT".to_string()),
                ..PeriodStatusReq::default()
            },
            STATUS_LOCKED,
        )
        .expect_err("解锁不给原因要拦");
        assert_eq!(missing.status, 400);
        assert!(missing.msg.contains("原因"), "报文：{}", missing.msg);

        let short = parse_status(
            &PeriodStatusReq {
                to_status: Some("DRAFT".to_string()),
                reason: Some("手滑".to_string()),
            },
            STATUS_LOCKED,
        )
        .expect_err("原因太短要拦");
        assert_eq!(short.status, 400);

        let unlock = parse_status(
            &PeriodStatusReq {
                to_status: Some("DRAFT".to_string()),
                reason: Some("  薪资口径调整，需重导考勤  ".to_string()),
            },
            STATUS_LOCKED,
        )
        .expect("合法解锁");
        assert_eq!(
            unlock.reason.as_deref(),
            Some("薪资口径调整，需重导考勤"),
            "trim 后进状态日志"
        );
    }

    /// 状态中文名与库层 CHECK 三值一一对应（漏一个 → 前端出现空白标签）。
    #[test]
    fn status_labels_cover_all_values() {
        for status in STATUS_VALUES {
            assert_ne!(status_label(status), "未知", "缺中文名：{status}");
        }
        assert_eq!(status_label(STATUS_DRAFT), "草稿");
        assert_eq!(status_label(STATUS_SUBMITTED), "已提交");
        assert_eq!(status_label(STATUS_LOCKED), "已锁定");
        assert_eq!(status_label("NOPE"), "未知");
    }

    /// 汇总快照等值判定：四个数全等才算「没变」（重算幂等的判据）。
    #[test]
    fn totals_same_as_needs_every_column() {
        let base = PeriodTotals {
            employee_count: 3,
            should_days_total: money("66.00"),
            actual_days_total: money("63.50"),
            ot_hours_total: money("12.00"),
        };
        assert!(base.same_as(&base));
        // 数值相等但小数位不同（`66.0` vs `66.00`）：Decimal 的 `==` 按值比，视为相同
        let trailing_zero = PeriodTotals {
            should_days_total: money("66.0"),
            ..base
        };
        assert!(base.same_as(&trailing_zero));
        let overtime = PeriodTotals {
            ot_hours_total: money("12.50"),
            ..base
        };
        assert!(!base.same_as(&overtime), "加班小时变了要能看出来");
        let counted = PeriodTotals {
            employee_count: 4,
            ..base
        };
        assert!(!base.same_as(&counted), "人数变了要能看出来");
    }
}
