//! 考勤记录入参 / 回执契约（`biz_attendance_record` · HR-20）。
//!
//! 数据层在 [`crate::business::hr::attendance_record_model`]，接口层在
//! [`crate::business::hr::attendance_record_handler`]。本文件**不连库**：只做
//! 「形状校验 + 合并 + 出参投影」，所以每条规则都能在没有数据库的环境里逐条断言。
//!
//! # 本文件服务的两个接口（设计 §5.2 #21 / #22）
//!
//! | # | 方法 | 路径（`/api` 由框架注入） | 权限码 |
//! |:--:|---|---|---|
//! | 21 | GET | `/business/hr/attendance/record/page` | `business:hr:attendance:record` |
//! | 22 | PUT | `/business/hr/attendance/record/{id}` | `business:hr:attendance:record` |
//!
//! # 「台账按人一行」是怎么保证的
//!
//! 三层，缺一层都能出重行：
//!
//! 1. **库**：`uk_biz_att_rec (period_id, employee_id) WHERE deleted_at IS NULL`（`V0383`）——
//!    同一人同一期间软删之后才能重建一行；
//! 2. **模型**：[`crate::business::hr::attendance_record_model::page`] 只从本表出数（
//!    不去 join 员工表把「没记录的人」补成一行，那样同一人会出现两行）;
//! 3. **契约**：本文件的 [`AttendanceRecordRes`] 一行对应一条记录，`employee_id` /
//!    `employee_no` 都是单值字段，没有可聚合的数组。
//!
//! # 为什么可写列里**没有** `employee_id` / `employee_no` / `dept_id` / `period_id` / `source`
//!
//! 这几列决定「这是谁、哪一期、从哪来」：改了就变成另一条业务事实，而不是「修正数字」。
//! 人录错了要删掉重建（导入是 upsert 语义，见 HR-21/22），期间建错了走期间接口（HR-19），
//! 来源是导入链路的留痕。修正接口只允许改 11 个数值列 + `remark`。
//!
//! # 天数口径为什么不硬校验守恒
//!
//! `should_days`（应出勤）在客户之间口径不同（自然月 30/31、固定 21.75、排班制），
//! 所以「应出勤 = 实出勤 + 各类假 + 旷工」**不做**库层 CHECK、也**不做**接口 400
//! （设计 §3.1 ⑤ 末段 + §4.9 W1）。本文件的 [`consistency_warnings`] 只产出**警告文本**
//! 挂在回执上，让人看见「这个数对不上」，但不拦他保存 —— 口径是客户的，账是他的。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::hr::attendance_period_dto::{PeriodTotals, normalize_page};
use crate::shared::error_code;

/// 每页默认条数（与考勤期间列表同口径）。
pub const DEFAULT_PAGE_SIZE: u64 = 20;

/// 每页上限（台账一页最多 200 人：再多也没人看，只会拖慢导出前的核对）。
pub const MAX_PAGE_SIZE: u64 = 200;

/// `remark TEXT` 的长度上限。
///
/// 库列是 `TEXT`（不限长），但台账备注是「这一行的解释」（为什么补了一天），
/// 2000 字足够；不设限会让一次粘贴 Excel 把整列撑爆。
pub const MAX_REMARK: usize = 2000;

/// `source` 的三个取值（`ck_biz_att_rec_source`，`V0383`）。
pub const SOURCE_IMPORT: &str = "IMPORT";

/// 手工录入。
pub const SOURCE_MANUAL: &str = "MANUAL";

/// 由请假单同步（一期不做自动同步，留列位给二期）。
pub const SOURCE_LEAVE_SYNC: &str = "LEAVE_SYNC";

/// `source` 的全部合法值。
pub const SOURCE_VALUES: [&str; 3] = [SOURCE_IMPORT, SOURCE_MANUAL, SOURCE_LEAVE_SYNC];

/// `source` → 中文标签（台账「来源」列直出，前端不再维护第二份词表）。
pub fn source_label(source: &str) -> &'static str {
    match source {
        SOURCE_IMPORT => "导入",
        SOURCE_MANUAL => "手工",
        SOURCE_LEAVE_SYNC => "请假同步",
        // 库层 CHECK 挡住其它值；走到这里说明代码与迁移漂移了，回显码值比空白强
        _ => "未知",
    }
}

// ==================== 数值上限 ====================

/// 天数类列上限：一个月最多 31 天。
///
/// 库列是 `NUMERIC(6,2)`（能存到 9999.99），但「某人某月应出勤 500 天」一定是手滑
/// （多敲了一个 0）。31 是自然月的物理上限；固定 21.75 / 26 天计薪周期都在里面。
pub fn max_days() -> Decimal {
    Decimal::new(31, 0)
}

/// 加班小时上限：31 天 × 24 小时 = 744。
///
/// 上限不是「合理性」判断而是「显然不可能」判断：一个人一个月不可能加班超过全部小时数。
pub fn max_ot_hours() -> Decimal {
    Decimal::new(744, 0)
}

/// 迟到 / 早退次数上限：31 天 × 每天 2 次（上下班各一次）= 62。
pub const MAX_COUNT: i32 = 62;

// ==================== ① 列表入参（设计 §5.2 #21） ====================

/// 考勤台账列表查询串。
///
/// 字段全 `Option`：把「没填」与「填错」分成人话（`period_id 不能为空` vs
/// `period_id 必须是合法的 uuid`），而不是让 serde 抛英文 `missing field`。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RecordPageQuery {
    /// 考勤期间 id（**必填**：台账按期间查，一人一行）。
    #[serde(default)]
    pub period_id: Option<String>,
    /// 部门 id（可选：查某部门这一期的人）。
    #[serde(default)]
    pub dept_id: Option<String>,
    /// 关键字（工号 / 姓名 `/` 模糊）。
    #[serde(default)]
    pub keyword: Option<String>,
    /// 页码（从 1 开始）。
    #[serde(default)]
    pub page: Option<u64>,
    /// 每页条数（默认 20，上限 200）。
    #[serde(default)]
    pub page_size: Option<u64>,
}

/// 归一化后的列表筛选条件（模型层只吃这个结构）。
#[derive(Debug, Clone, PartialEq)]
pub struct RecordFilter {
    /// 考勤期间 id。
    pub period_id: Uuid,
    /// 部门（`biz_attendance_record.dept_id`，记录上的部门快照）。
    pub dept_id: Option<Uuid>,
    /// 关键字（工号 / 姓名）。
    pub keyword: Option<String>,
}

/// 查询串 → [`RecordFilter`]（**纯函数**：不连库就能测）。
///
/// # 异常情况
///
/// * 400 + `10001`：`period_id` 缺失 / 不是 uuid；`dept_id` 不是 uuid。
pub fn filter_of(query: &RecordPageQuery) -> std::result::Result<RecordFilter, ApiError> {
    let raw_period = query.period_id.as_deref().map(str::trim).unwrap_or_default();
    if raw_period.is_empty() {
        return Err(bad_request(
            "period_id 不能为空：考勤台账按期间查（一人一行），请先在期间下拉里选一期",
        ));
    }
    let period_id = Uuid::parse_str(raw_period).map_err(|_| {
        bad_request(format!(
            "period_id 必须是合法的 uuid（收到 `{raw_period}`；期间列表回执里的 id 直接用）"
        ))
    })?;

    let dept_id = match query.dept_id.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        Some(raw) => Some(Uuid::parse_str(raw).map_err(|_| {
            bad_request(format!(
                "dept_id 必须是合法的 uuid（收到 `{raw}`；不筛部门就别传这个参数）"
            ))
        })?),
        None => None,
    };

    let keyword = query
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);

    Ok(RecordFilter {
        period_id,
        dept_id,
        keyword,
    })
}

/// 页码归一化（与考勤期间列表同一口径，直接复用避免两处上限漂移）。
pub fn normalize_paging(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    normalize_page(page, page_size)
}

// ==================== ② 可写数值列 ====================

/// 台账上「数字部分」的 11 个可写列。
///
/// 单独一个结构而不是散在行 / 入参 / 出参三处，是为了让**新旧两侧的快照**天然同形：
/// 变更留痕（`sys_change_log` 的 `b` / `a`）、守卫判断、一致性警告都拿同一个类型比，
/// 不会出现「入参有 11 列、出参少一列」这种一改就漏的漂移。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RecordNumbers {
    /// 应出勤天数（口径由客户定：自然月 / 21.75 / 排班）。
    pub should_days: Decimal,
    /// 实际出勤天数。
    pub actual_days: Decimal,
    /// 带薪假天数（年假 / 婚丧 / 产假）。
    pub leave_paid_days: Decimal,
    /// 无薪假天数（事假）→ 薪资扣款依据。
    pub leave_unpaid_days: Decimal,
    /// 病假天数（按比例发，比例在薪资项公式里配）。
    pub sick_days: Decimal,
    /// 旷工天数。
    pub absent_days: Decimal,
    /// 平时加班小时。
    pub ot_hours_workday: Decimal,
    /// 休息日加班小时。
    pub ot_hours_weekend: Decimal,
    /// 法定节假日加班小时。
    pub ot_hours_holiday: Decimal,
    /// 迟到次数。
    pub late_count: i32,
    /// 早退次数。
    pub early_leave_count: i32,
}

impl RecordNumbers {
    /// 三个加班列之和（台账「加班合计」列；期间汇总也按这个口径加总。
    ///
    /// 设计 §3.1 ④ 的期间头只有一个 `ot_hours_total`，明细分三列存在记录上
    /// （`attendance_period_model::RECALC_SQL` 的 `SUM(wd + weekend + holiday)`）——
    /// 两处口径必须一致，所以合计算在这里，不各写一份。
    pub fn ot_hours_total(&self) -> Decimal {
        self.ot_hours_workday + self.ot_hours_weekend + self.ot_hours_holiday
    }

    /// 天数类列逐条取值（校验与警告共用，避免「新增一列忘了校验」）。
    fn day_columns(&self) -> [(&'static str, Decimal); 6] {
        [
            ("should_days", self.should_days),
            ("actual_days", self.actual_days),
            ("leave_paid_days", self.leave_paid_days),
            ("leave_unpaid_days", self.leave_unpaid_days),
            ("sick_days", self.sick_days),
            ("absent_days", self.absent_days),
        ]
    }

    /// 加班小时列逐条取值。
    fn hour_columns(&self) -> [(&'static str, Decimal); 3] {
        [
            ("ot_hours_workday", self.ot_hours_workday),
            ("ot_hours_weekend", self.ot_hours_weekend),
            ("ot_hours_holiday", self.ot_hours_holiday),
        ]
    }

    /// 全部数值列的范围校验（非负 / 天数 ≤31 / 小时 ≤744 / 次数 ≤62）。
    ///
    /// 库层的 `ck_biz_att_rec_nonneg` 只管「非负」，且违反时报的是约束名；
    /// 这里提前拦一次，用户看到的是「should_days 不能是负数（应出勤天数）」这种人话。
    ///
    /// # 异常情况
    ///
    /// * 400 + `10001`：点名哪个字段、什么范围。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        let zero = Decimal::ZERO;
        for (field, value) in self.day_columns() {
            if value < zero {
                return Err(bad_request(format!("{field} 不能是负数（天数）")));
            }
            if value > max_days() {
                return Err(bad_request(format!(
                    "{field} 最多 31 天（一个月不可能超过 31 天，收到 `{value}`）"
                )));
            }
        }
        for (field, value) in self.hour_columns() {
            if value < zero {
                return Err(bad_request(format!("{field} 不能是负数（小时）")));
            }
            if value > max_ot_hours() {
                return Err(bad_request(format!(
                    "{field} 最多 744 小时（31 天 × 24 小时，收到 `{value}`）"
                )));
            }
        }
        for (field, value) in [
            ("late_count", self.late_count),
            ("early_leave_count", self.early_leave_count),
        ] {
            if value < 0 {
                return Err(bad_request(format!("{field} 不能是负数（次数）")));
            }
            if value > MAX_COUNT {
                return Err(bad_request(format!(
                    "{field} 最多 {MAX_COUNT} 次（31 天 × 每天 2 次，收到 `{value}`）"
                )));
            }
        }
        Ok(())
    }
}

// ==================== ③ 单行修正入参（设计 §5.2 #22） ====================

/// 单行修正入参：**部分更新**语义（缺省的字段保持库里的值）。
///
/// 前端台账是「单元格失焦保存」（设计 §6.2.3），一次只发**一个**字段 —— 所以
/// 不能要求整行回传（整行覆盖会让两个 HR 各自改一列时互相抹掉对方的改动）。
///
/// `late_count` / `early_leave_count` 用 `i32`（库列是 `INT`），其余数值列用
/// [`Decimal`]（`NUMERIC(6,2)` / `NUMERIC(8,2)`；`serde` 默认按字符串往返，
/// 前端不必担心 JS 浮点把 `7.25` 变成 `7.250000000001`）。
///
/// `remark` 是 `Option<Option<String>>`：缺省 = 不改，`null` = 清空，字符串 = 覆盖
/// （与 `PUT /business/erp/contract/{id}` 同一口径，靠 [`double_option`] 区分）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RecordPatchReq {
    /// 应出勤天数。
    #[serde(default)]
    pub should_days: Option<Decimal>,
    /// 实际出勤天数。
    #[serde(default)]
    pub actual_days: Option<Decimal>,
    /// 带薪假天数。
    #[serde(default)]
    pub leave_paid_days: Option<Decimal>,
    /// 无薪假天数。
    #[serde(default)]
    pub leave_unpaid_days: Option<Decimal>,
    /// 病假天数。
    #[serde(default)]
    pub sick_days: Option<Decimal>,
    /// 旷工天数。
    #[serde(default)]
    pub absent_days: Option<Decimal>,
    /// 平时加班小时。
    #[serde(default)]
    pub ot_hours_workday: Option<Decimal>,
    /// 休息日加班小时。
    #[serde(default)]
    pub ot_hours_weekend: Option<Decimal>,
    /// 法定节假日加班小时。
    #[serde(default)]
    pub ot_hours_holiday: Option<Decimal>,
    /// 迟到次数。
    #[serde(default)]
    pub late_count: Option<i32>,
    /// 早退次数。
    #[serde(default)]
    pub early_leave_count: Option<i32>,
    /// 备注（`null` = 清空，缺省 = 不改）。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

/// 「合并后的最终值」—— 模型层照这个整行写，不再做局部 SET。
///
/// 整行写比「按给了的字段拼动态 SET」便宜得多：SQL 只有一个、占位符个数固定
/// （`sql_placeholder_counts_match_params` 单测钉死），而且并发下不会出现
/// 「A 改天数、B 改小时」拼出两条各自只写一半的语句。
#[derive(Debug, Clone, PartialEq)]
pub struct RecordPatch {
    /// 合并后的 11 个数值列。
    pub numbers: RecordNumbers,
    /// 合并后的备注（`None` = 空）。
    pub remark: Option<String>,
    /// 与库里的值相比是否有实际变化（`false` = 不写库、不留痕）。
    pub changed: bool,
}

/// 入参里是否给了任何字段（一个都没给 = 400，而不是静默 200）。
fn any_field(req: &RecordPatchReq) -> bool {
    req.should_days.is_some()
        || req.actual_days.is_some()
        || req.leave_paid_days.is_some()
        || req.leave_unpaid_days.is_some()
        || req.sick_days.is_some()
        || req.absent_days.is_some()
        || req.ot_hours_workday.is_some()
        || req.ot_hours_weekend.is_some()
        || req.ot_hours_holiday.is_some()
        || req.late_count.is_some()
        || req.early_leave_count.is_some()
        || req.remark.is_some()
}

impl RecordPatchReq {
    /// 合并到一行当前值上，得到最终值 + `changed`。
    ///
    /// # 参数
    ///
    /// - `current`：库里这一行现在的 11 个数值列；
    /// - `current_remark`：库里这一行现在的备注。
    ///
    /// # 异常情况
    ///
    /// * 400 + `10001`：一个字段都没给 / 数值越界 / 备注超长。
    pub fn apply(
        &self,
        current: &RecordNumbers,
        current_remark: Option<&str>,
    ) -> std::result::Result<RecordPatch, ApiError> {
        if !any_field(self) {
            return Err(bad_request(
                "没有要修改的字段：至少给一个（天数 / 加班小时 / 迟到早退次数 / remark）",
            ));
        }

        let numbers = RecordNumbers {
            should_days: self.should_days.unwrap_or(current.should_days),
            actual_days: self.actual_days.unwrap_or(current.actual_days),
            leave_paid_days: self.leave_paid_days.unwrap_or(current.leave_paid_days),
            leave_unpaid_days: self.leave_unpaid_days.unwrap_or(current.leave_unpaid_days),
            sick_days: self.sick_days.unwrap_or(current.sick_days),
            absent_days: self.absent_days.unwrap_or(current.absent_days),
            ot_hours_workday: self.ot_hours_workday.unwrap_or(current.ot_hours_workday),
            ot_hours_weekend: self.ot_hours_weekend.unwrap_or(current.ot_hours_weekend),
            ot_hours_holiday: self.ot_hours_holiday.unwrap_or(current.ot_hours_holiday),
            late_count: self.late_count.unwrap_or(current.late_count),
            early_leave_count: self.early_leave_count.unwrap_or(current.early_leave_count),
        };
        numbers.validate()?;

        let remark = match &self.remark {
            None => normalize_remark(current_remark),
            Some(value) => normalize_remark(value.as_deref()),
        };
        if let Some(text) = &remark
            && text.chars().count() > MAX_REMARK
        {
            return Err(bad_request(format!(
                "remark 最长 {MAX_REMARK} 个字符（收到 {} 个）",
                text.chars().count()
            )));
        }

        let changed = numbers != *current || remark.as_deref() != normalize_remark(current_remark).as_deref();
        Ok(RecordPatch {
            numbers,
            remark,
            changed,
        })
    }
}

/// 备注归一化：`trim` 后空串 → `None`（库里 `''` 与 `NULL` 是两种状态，
/// 台账上都是「没写备注」，统一成 `NULL` 免得 diff 里冒出假的 `'' → null`）。
pub fn normalize_remark(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

/// 「缺省」与「显式 null」的区分（`Option<Option<T>>` 的 serde 补丁）。
///
/// `Option<Option<T>>` 直接用 serde 反序列化时 `null` 与「字段不存在」都得到 `None`，
/// 于是「清空备注」永远发不出来。社区解法就是这一层包装（本仓 `erp/*_dto.rs` 各自
/// 有一份同样的函数：DTO 之间不互相 import，避免二开时牵一发）。
fn double_option<'de, T, D>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

// ==================== ④ 出参 ====================

/// 台账一行（一人一行）。
///
/// # 姓名口径（AGENTS.md 约定 13）
///
/// - `employee_no` / `employee_name` 是**快照列**（`V0383` 的列注释：考勤口径的历史留痕）：
///   工资按这两个值出表，所以台账必须显示**当时**的工号姓名，而不是员工主档现在的名字；
/// - `dept_name` 是**实时 join**（`biz_attendance_record` 只存 `dept_id`，没存部门名快照）：
///   部门改名/撤并后，台账显示现在的名字才是对的；
/// - `updated_by_name` 是实时 join（审计轨迹在 `sys_change_log`，这里只要「最后谁改过」）。
#[derive(Debug, Clone, Serialize)]
pub struct AttendanceRecordRes {
    /// 记录主键。
    pub id: String,
    /// 所属考勤期间。
    pub period_id: String,
    /// 考勤月 `yyyy-MM`（台账顶部「当前期间」直出，不用二次查期间）。
    pub period_code: String,
    /// 期间状态码值（`DRAFT` / `SUBMITTED` / `LOCKED`）。
    pub period_status: String,
    /// 期间状态中文标签。
    pub period_status_label: String,
    /// 这一行当前**能不能改**（`period_status != LOCKED`）。
    ///
    /// 台账单元格编辑靠它决定禁用态：锁定期间是薪资试算的基线（设计 §4.2.3），
    /// 前端置灰 + 后端 409 双层，用户不会「改了才被拒」。
    pub editable: bool,
    /// 员工 id。
    pub employee_id: String,
    /// 员工工号（**快照列**）。
    pub employee_no: String,
    /// 员工姓名（**快照列**）。
    pub employee_name: String,
    /// 部门 id（记录上的部门快照，凭证 `aux_dept_id` 用）。
    pub dept_id: Option<String>,
    /// 部门名（实时 join `sys_dept`）。
    pub dept_name: Option<String>,
    /// 11 个数值列（摊平到出参顶层，前端表格列名与库列名一一对应）。
    #[serde(flatten)]
    pub numbers: RecordNumbers,
    /// 加班合计（三列之和；台账「加班」汇总列）。
    pub ot_hours_total: Decimal,
    /// 来源码值（`IMPORT` / `MANUAL` / `LEAVE_SYNC`）。
    pub source: String,
    /// 来源中文标签。
    pub source_label: String,
    /// 备注。
    pub remark: Option<String>,
    /// 最后修改时间（`ISO8601 +08:00`）。
    pub updated_at: String,
    /// 最后修改人。
    pub updated_by: Option<String>,
    /// 最后修改人姓名（实时 join）。
    pub updated_by_name: Option<String>,
}

/// 单行修正回执。
///
/// 除了改后的这一行，还带三样东西：
///
/// - `changed`：这次是否真的有变化（`false` = 前端失焦时值没动，库里没写、也没留痕）；
/// - `warnings`：一致性警告（设计 §4.9 W1 只报不拦）；
/// - `period_totals`：修正后**重算过的期间汇总**（台账底部的合计），省掉前端再打一次
///   `recalc` 接口 —— 单行修正与汇总重算在同一个事务里完成，读到的数一定自洽。
#[derive(Debug, Clone, Serialize)]
pub struct RecordPatchRes {
    /// 改后的这一行。
    pub record: AttendanceRecordRes,
    /// 是否有实际变化。
    pub changed: bool,
    /// 一致性警告（空数组 = 没发现问题）。
    pub warnings: Vec<String>,
    /// 修正后重算的期间汇总。
    pub period_totals: PeriodTotals,
}

// ==================== ⑤ 一致性警告（设计 §4.9 W1，只报不拦） ====================

/// 「应出勤 = 实出勤 + 各类假 + 旷工」的守恒检查，**只产出警告文本**。
///
/// 为什么不拦（设计 §3.1 ⑤ 末段）：应出勤口径客户之间不同 —— 自然月 30/31、固定 21.75、
/// 排班制都能是合法值（固定 21.75 口径下实际出勤 22 天完全正常）。硬拦等于把合法数据
/// 挡在门外，所以这里只把差额说出来，让人自己判断。
///
/// 容差 0.01：`NUMERIC(6,2)` 的两位小数，四舍五入不该报成「差 0.00 天」。
pub fn consistency_warnings(numbers: &RecordNumbers) -> Vec<String> {
    let mut warnings = Vec::new();
    let tolerance = Decimal::new(1, 2);
    // 应出勤为 0 时「差额 = 0 - 其它」会天天报警（还没导入的人），不算异常
    if numbers.should_days > Decimal::ZERO {
        let accounted = numbers.actual_days
            + numbers.leave_paid_days
            + numbers.leave_unpaid_days
            + numbers.sick_days
            + numbers.absent_days;
        let diff = numbers.should_days - accounted;
        if diff.abs() > tolerance {
            warnings.push(format!(
                "应出勤 {} 天 ≠ 实际 {} + 带薪假 {} + 无薪假 {} + 病假 {} + 旷工 {} = {} 天（差 {}）：\
                 口径可能是固定 21.75 / 排班制，本系统只提示不拦截（设计 §4.9 W1）；\
                 若确实是漏录了假或旷工，请补齐后再算工资",
                numbers.should_days,
                numbers.actual_days,
                numbers.leave_paid_days,
                numbers.leave_unpaid_days,
                numbers.sick_days,
                numbers.absent_days,
                accounted,
                diff.abs()
            ));
        }
    }
    warnings
}

// ==================== ⑥ 错误原语 ====================

/// `400 + 10001`：入参形状 / 范围不对。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 出参里的时间统一 `ISO8601 +08:00`（设计 §5.1）。委托给员工模块的同一个转换，
/// 免得三个模块各写一份时区格式化。
pub fn timestamp_cst(at: DateTime<Utc>) -> String {
    crate::business::hr::employee_dto::timestamp_cst(at)
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 建表迁移原文（编译期嵌入）：`source` 词表 / 约束名改了就红。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    fn dec(text: &str) -> Decimal {
        Decimal::from_str_exact(text).expect("合法小数")
    }

    fn zero_numbers() -> RecordNumbers {
        RecordNumbers {
            should_days: Decimal::ZERO,
            actual_days: Decimal::ZERO,
            leave_paid_days: Decimal::ZERO,
            leave_unpaid_days: Decimal::ZERO,
            sick_days: Decimal::ZERO,
            absent_days: Decimal::ZERO,
            ot_hours_workday: Decimal::ZERO,
            ot_hours_weekend: Decimal::ZERO,
            ot_hours_holiday: Decimal::ZERO,
            late_count: 0,
            early_leave_count: 0,
        }
    }

    fn sample_numbers() -> RecordNumbers {
        RecordNumbers {
            should_days: dec("22"),
            actual_days: dec("20"),
            leave_paid_days: dec("0"),
            leave_unpaid_days: dec("2"),
            sick_days: dec("0"),
            absent_days: dec("0"),
            ot_hours_workday: dec("10"),
            ot_hours_weekend: dec("4"),
            ot_hours_holiday: dec("0"),
            late_count: 1,
            early_leave_count: 0,
        }
    }

    /// `source` 词表与 `V0383` 的 `ck_biz_att_rec_source` 逐字一致。
    ///
    /// 词表漂移的后果：接口收下一个库层不认的值 → 500 而不是 400，
    /// 用户看到「数据库错误」而不是「来源不合法」。
    #[test]
    fn source_values_match_check_constraint() {
        let probe = "CHECK (source IN ('IMPORT','MANUAL','LEAVE_SYNC'))";
        assert!(V0383_DDL.contains(probe), "V0383 的 source CHECK 变了：{probe}");
        for value in SOURCE_VALUES {
            assert!(probe.contains(&format!("'{value}'")), "source 词表多了 {value}");
        }
        assert_eq!(SOURCE_VALUES.len(), probe.matches('\'').count() / 2);
    }

    /// 三个加班列之和 = 台账「加班合计」列。
    #[test]
    fn ot_total_sums_three_columns() {
        let numbers = sample_numbers();
        assert_eq!(numbers.ot_hours_total(), dec("14"));
    }

    /// 查询串：`period_id` 必填，缺失 / 不是 uuid / `dept_id` 不是 uuid 都要 400。
    #[test]
    fn filter_requires_period_uuid() {
        let missing = filter_of(&RecordPageQuery::default()).expect_err("缺 period_id 必须 400");
        assert_eq!(missing.status, 400);
        assert!(missing.msg.contains("period_id 不能为空"), "{}", missing.msg);

        let bad = RecordPageQuery {
            period_id: Some("2026-11".to_string()),
            ..RecordPageQuery::default()
        };
        let error = filter_of(&bad).expect_err("非 uuid 必须 400");
        assert_eq!(error.code, error_code::PARAM_INVALID);

        let bad_dept = RecordPageQuery {
            period_id: Some(Uuid::nil().to_string()),
            dept_id: Some("dept-1".to_string()),
            ..RecordPageQuery::default()
        };
        let error = filter_of(&bad_dept).expect_err("非 uuid 的 dept_id 必须 400");
        assert!(error.msg.contains("dept_id"), "{}", error.msg);

        // 只有空白的 keyword / dept_id 等于没传（前端清空输入框会发空串）
        let ok = RecordPageQuery {
            period_id: Some(Uuid::nil().to_string()),
            dept_id: Some("  ".to_string()),
            keyword: Some("   ".to_string()),
            page: Some(0),
            page_size: Some(9999),
        };
        let filter = filter_of(&ok).expect("合法查询");
        assert_eq!(filter.dept_id, None);
        assert_eq!(filter.keyword, None);
        assert_eq!(normalize_paging(ok.page, ok.page_size), (1, MAX_PAGE_SIZE));
    }

    /// 一个字段都没给 → 400（不是静默 200 让人以为「保存成功」）。
    #[test]
    fn patch_requires_at_least_one_field() {
        let req = RecordPatchReq::default();
        let error = req.apply(&zero_numbers(), None).expect_err("空入参必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("没有要修改的字段"), "{}", error.msg);
    }

    /// 部分更新：只给 `actual_days` 时其余字段原样保留；`changed` 正确。
    #[test]
    fn patch_merges_only_given_fields() {
        let current = sample_numbers();
        let req = RecordPatchReq {
            actual_days: Some(dec("19.5")),
            ..RecordPatchReq::default()
        };
        let patch = req.apply(&current, Some("补录半天事假")).expect("合法修正");
        assert!(patch.changed, "改了 actual_days 就是有变化");
        assert_eq!(patch.numbers.actual_days, dec("19.5"));
        // 其余列一个都不能动
        assert_eq!(patch.numbers.should_days, current.should_days);
        assert_eq!(patch.numbers.leave_unpaid_days, current.leave_unpaid_days);
        assert_eq!(patch.numbers.ot_hours_workday, current.ot_hours_workday);
        assert_eq!(patch.numbers.late_count, current.late_count);
        assert_eq!(patch.remark.as_deref(), Some("补录半天事假"));
    }

    /// 值没变 → `changed = false`（前端失焦保存不该写库、更不该留「假变更」日志）。
    ///
    /// `22` 与 `22.00` 必须判成相等：库读回来是 `22.00`，前端回传 `22`。
    #[test]
    fn patch_with_same_values_is_unchanged() {
        let current = sample_numbers();
        let req = RecordPatchReq {
            should_days: Some(dec("22.00")),
            late_count: Some(1),
            ot_hours_workday: Some(dec("10.0")),
            remark: Some(Some("   ".to_string())),
            ..RecordPatchReq::default()
        };
        let patch = req.apply(&current, None).expect("合法修正");
        assert!(!patch.changed, "同值（含 22 vs 22.00）必须判为无变化");
        assert_eq!(patch.remark, None, "空白备注归一化成 None");
    }

    /// 备注三态：缺省 = 不改、`null` = 清空、字符串 = 覆盖。
    #[test]
    fn remark_distinguishes_absent_null_and_value() {
        let current = zero_numbers();
        let absent = RecordPatchReq {
            actual_days: Some(dec("1")),
            ..RecordPatchReq::default()
        };
        assert_eq!(
            absent.apply(&current, Some("原备注")).expect("合法").remark.as_deref(),
            Some("原备注"),
            "缺省 remark 必须保留原值"
        );

        let cleared = RecordPatchReq {
            remark: Some(None),
            ..RecordPatchReq::default()
        };
        let patch = cleared.apply(&current, Some("原备注")).expect("合法");
        assert_eq!(patch.remark, None, "显式 null 必须清空");
        assert!(patch.changed, "清空备注是有变化");

        // serde 侧：`null` → Some(None)；字段缺失 → None
        let null_json: RecordPatchReq = serde_json::from_str(r#"{"remark": null}"#).expect("null");
        assert_eq!(null_json.remark, Some(None));
        let missing: RecordPatchReq = serde_json::from_str(r#"{"actual_days": 1}"#).expect("缺省");
        assert_eq!(missing.remark, None);
        // 数值列两种写法都收（JSON 数字 / 字符串），且都按 Decimal 精确解析
        assert_eq!(missing.actual_days, Some(dec("1")));
        let text: RecordPatchReq = serde_json::from_str(r#"{"actual_days": "19.50"}"#).expect("字符串");
        assert_eq!(text.actual_days, Some(dec("19.5")));
    }

    /// 越界：负数 / 天数 >31 / 小时 >744 / 次数 >62 / 备注超长都 400，且点名字段。
    #[test]
    fn range_guards_are_named() {
        let current = zero_numbers();
        let cases: [(&str, RecordPatchReq); 5] = [
            (
                "负数天数",
                RecordPatchReq {
                    actual_days: Some(dec("-0.5")),
                    ..RecordPatchReq::default()
                },
            ),
            (
                "天数超 31",
                RecordPatchReq {
                    should_days: Some(dec("32")),
                    ..RecordPatchReq::default()
                },
            ),
            (
                "小时超 744",
                RecordPatchReq {
                    ot_hours_holiday: Some(dec("745")),
                    ..RecordPatchReq::default()
                },
            ),
            (
                "次数超 62",
                RecordPatchReq {
                    late_count: Some(63),
                    ..RecordPatchReq::default()
                },
            ),
            (
                "备注超长",
                RecordPatchReq {
                    remark: Some(Some("备".repeat(MAX_REMARK + 1))),
                    ..RecordPatchReq::default()
                },
            ),
        ];
        for (label, req) in cases {
            let error = req.apply(&current, None).expect_err(label);
            assert_eq!(error.status, 400, "{label}");
            assert_eq!(error.code, error_code::PARAM_INVALID, "{label}");
        }
        // 边界值本身是合法的（31 天 / 744 小时 / 62 次）
        let edge = RecordPatchReq {
            should_days: Some(dec("31")),
            ot_hours_holiday: Some(dec("744")),
            late_count: Some(MAX_COUNT),
            ..RecordPatchReq::default()
        };
        edge.apply(&current, None).expect("边界值必须放行");
    }

    /// 一致性警告：口径对不上只报告；应出勤 0（还没录）与容差内不报告。
    #[test]
    fn consistency_warning_only_warns() {
        // 22 = 20 + 0 + 2 + 0 + 0 → 对得上
        assert!(consistency_warnings(&sample_numbers()).is_empty());

        // 应出勤 22、实际 20、无薪假 0 → 差 2 天
        let mut skewed = sample_numbers();
        skewed.leave_unpaid_days = Decimal::ZERO;
        let warnings = consistency_warnings(&skewed);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("应出勤 22"), "{}", warnings[0]);
        assert!(warnings[0].contains("只提示不拦截"), "{}", warnings[0]);

        // 容差 0.01 以内不报（两位小数的四舍五入）
        let mut rounded = sample_numbers();
        rounded.should_days = dec("22.01");
        assert!(consistency_warnings(&rounded).is_empty(), "0.01 以内算对得上");

        // 应出勤 0 = 还没录，不报
        assert!(consistency_warnings(&zero_numbers()).is_empty());
    }
}
