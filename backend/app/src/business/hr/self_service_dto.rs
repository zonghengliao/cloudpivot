//! 员工自助三接口的入参 / 回执契约（HR-41）：`/salary/my` · `/attendance/my` · `/leave/my`。
//!
//! | # | 方法 | 路径（`/api` 由框架注入） | 权限码 | 落地卡 |
//! |:--:|---|---|---|---|
//! | 25 | GET | `/business/hr/attendance/my` | 登录即可（设计 §5.2 #25） | HR-41 |
//! | 46 | GET | `/business/hr/salary/my` | `business:hr:salary:my` | HR-41 |
//! | 47 | GET | `/business/hr/leave/my` | 登录即可（设计 §5.2 脚注「4 处 `/my`」的第 3 处） | HR-41 |
//!
//! 数据层在 [`crate::business::hr::self_service_model`]，接口层在
//! [`crate::business::hr::self_service_handler`]。本文件**不连库**：只有入参校验、
//! 状态词白名单与出参投影，因此每条规则都能在没有数据库的环境里逐条断言。
//!
//! # 三条接口共用的一个前提：**没有 id 参数**
//!
//! 自助接口的「我要看谁的」**只能**来自令牌（`AuthClaims.user_id` → `biz_employee.id`），
//! 三条入参结构里刻意**没有** `employee_id` / `user_id` / `sheet_id` 字段：
//!
//! * 有 id 参数就必须校验它属于当前用户 —— 而这类校验最容易被写成「查到了就放行」
//!   （`find_in(id)?` 不比对归属），是数据越权最常见的入口；
//! * 没有 id 参数，越权请求连表达方式都没有：`GET /salary/my?id=<别人的明细>` 里的
//!   未知参数被 `serde` 忽略，查询恒按本人过滤。
//!
//! [`tests::query_structs_carry_no_identity_fields`] 用序列化键名把这条钉住。
//!
//! # 「员工看不到自己的档案」怎么办：返回空，不返回 403
//!
//! 没有绑定账号的员工（`biz_employee.user_id IS NULL`）与「账号还没绑档案」的新账号
//! （`user_id` 查不到员工）都会让三条接口返回 **200 + 空列表**（`meta.total = 0`）。
//!
//! 这是刻意的（卡面验收：「越权返回空不是 403（或明确 403）」）：
//!
//! * 403 会让普通员工打开「我的薪资」tab 就看到一片红色报错，而这是**正常的空态**
//!   （他本月还没发工资 / HR 还没给他绑账号），不是错误；
//! * 越权请求与「我确实没有数据」在响应上**不可区分** —— 攻击者拿不到「这个 id 存在」的
//!   任何信号（403 与 404 的差异本身就是一种枚举通道）。
//!
//! 需要提示「请 HR 绑定账号」时由前端按空态文案处理（`meta.total == 0`）。
//!
//! # 金额 / 日期口径
//!
//! 金额一律 `Decimal` → JSON 字符串（`"7354.03"`，避免 JS 浮点）；时间 `ISO8601 +08:00`；
//! 日期 `YYYY-MM-DD`（设计 §5.1）。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::hr::attendance_period_dto;
use crate::business::hr::attendance_record_dto::{self, RecordNumbers};
use crate::business::hr::employee_dto;
use crate::business::hr::leave_dto;
use crate::business::hr::salary_sheet_dto;
use crate::shared::error_code;

// ==================== 权限码 ====================

/// 「我的薪资」权限码（`V0387__hr_menu_perms` 已种，`sys_menu.auth_code` 同名）。
///
/// 与考勤 / 请假不同，薪资**要**一个权限码：工资条在本模块是敏感数据，
/// 允许 HR 在菜单里整体关掉「自助看工资」（有些客户要求只走纸质工资条）。
pub const PERM_SALARY_MY: &str = "business:hr:salary:my";

// ==================== 分页 ====================

/// 自助页默认每页条数。
///
/// 比台账（20）小：自助视角一次看一年也就 12 条，一屏能看完；不看 `OUTER` 翻页。
pub const MY_PAGE_DEFAULT: u64 = 12;

/// 自助页每页上限。
///
/// 自助接口是**登录即可**的（考勤 / 请假），不设上限等于给任何账号一个
/// 「翻全公司」的入口（虽然被 `employee_id` 裁住，但 count 与排序仍是全表扫）。
pub const MY_PAGE_MAX: u64 = 60;

/// 分页归一：`page >= 1`、`1 <= page_size <= MY_PAGE_MAX`。
pub fn normalize_paging(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    let page = page.unwrap_or(1).max(1);
    let page_size = page_size.unwrap_or(MY_PAGE_DEFAULT).clamp(1, MY_PAGE_MAX);
    (page, page_size)
}

// ==================== 状态词表 ====================

/// 员工**默认可见**的薪资单状态：审批通过 / 已过账 / 已红冲。
///
/// # 为什么草稿与在途单不默认可见
///
/// `DRAFT` / `CALCULATED` / `SUBMITTED` 的金额是「还会变」的：HR 试算、调项、重算都在这几档里，
/// 让员工先看到一版中途的数，第二版一变就要解释「为什么少了 300」——这是客服成本，
/// 也是真实纠纷源头（设计 §4.8.4 的手工覆盖会把数改掉）。
/// 所以默认只给**已定稿**的三档；HR 自己要看在途单就传 `include_pending=true`
/// （同一接口，权限码 `business:hr:salary:my` 之外还要求能算工资的账号才用得上）。
pub const VISIBLE_STATUSES: [&str; 3] = [
    salary_sheet_dto::STATUS_APPROVED,
    salary_sheet_dto::STATUS_POSTED,
    salary_sheet_dto::STATUS_REVERSED,
];

/// 在途状态（`include_pending=true` 时追加）。
pub const PENDING_STATUSES: [&str; 3] = [
    salary_sheet_dto::STATUS_DRAFT,
    salary_sheet_dto::STATUS_CALCULATED,
    salary_sheet_dto::STATUS_SUBMITTED,
];

/// 本接口会用到的那组状态（按开关决定是否含在途）。
///
/// 纯函数 + 单测：两个分支的并集必须**恰好**是 `ck_biz_sal_sheet_status` 的六档 ——
/// 以后加一档状态（比如 `PAID`）而忘了放进这里，员工会在「单已是终态但工资条不出现」
/// 与「工资条出现了但单还没定稿」之间二选一，而两种都有人报 bug。
pub fn visible_statuses(include_pending: bool) -> Vec<&'static str> {
    let mut out = VISIBLE_STATUSES.to_vec();
    if include_pending {
        out.extend(PENDING_STATUSES);
    }
    out
}

/// 请假单状态白名单（`ck_biz_leave_status`，拒绝未知码时点名收到的值）。
pub const LEAVE_STATUSES: [&str; 4] = [
    leave_dto::STATUS_DRAFT,
    leave_dto::STATUS_SUBMITTED,
    leave_dto::STATUS_APPROVED,
    leave_dto::STATUS_CANCELLED,
];

// ==================== 入参 ====================

/// 「我的考勤」入参（设计 §5.2 #25）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MyAttendanceQuery {
    /// 考勤月 `yyyy-MM`（可选；不给 = 全部期间倒序）。
    pub period_code: Option<String>,
    /// 页码（从 1 开始）。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

/// 「我的请假」入参（设计 §5.2 脚注第 3 处 `/my`）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MyLeaveQuery {
    /// 单据状态（可选，白名单见 [`LEAVE_STATUSES`]）。
    pub status: Option<String>,
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

/// 「我的薪资」入参（设计 §5.2 #46）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MySalaryQuery {
    /// 薪资期间 `yyyy-MM`（可选）。
    pub period_code: Option<String>,
    /// 是否含在途单（默认 `false`，见 [`VISIBLE_STATUSES`]）。
    pub include_pending: Option<bool>,
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

/// 400 + `10001`：入参不合法。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `yyyy-MM` 校验（与 `ck_biz_sal_sheet_code` / `ck_biz_att_period_code` 同一正则）。
///
/// 空串按「没给」处理：前端 `period_code=`（清空筛选）不该报 400。
pub fn parse_period_code(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let bytes = text.as_bytes();
    // 先判长度再逐位取下标（否则 `"26-11"` 这种短串会 panic 成 500 而不是 400）。
    // 月份必须按库层正则 `(0[1-9]|1[0-2])` 逐位判：只判「两位数字」会放过 2026-00。
    let month_ok = bytes.len() == 7 && matches!((bytes[5], bytes[6]), (b'0', b'1'..=b'9') | (b'1', b'0'..=b'2'));
    let shaped = bytes.len() == 7 && bytes[..4].iter().all(u8::is_ascii_digit) && bytes[4] == b'-' && month_ok;
    if !shaped {
        return Err(bad_request(format!(
            "period_code 必须是 yyyy-MM（例如 2026-11），收到 `{text}`"
        )));
    }
    Ok(Some(text.to_string()))
}

/// 请假状态校验（大小写不敏感，回大写码值）。
pub fn parse_leave_status(raw: Option<&str>) -> std::result::Result<Option<&'static str>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let upper = text.to_ascii_uppercase();
    LEAVE_STATUSES
        .iter()
        .find(|code| **code == upper)
        .copied()
        .map(Some)
        .ok_or_else(|| bad_request(format!("status 只能是 {}，收到 `{text}`", LEAVE_STATUSES.join(" / "))))
}

// ==================== 出参 ====================

/// 「我的考勤」一行 = 我在某一期的一条考勤记录。
///
/// `employee_no` / `employee_name` / `dept_name` 这里**都不返回**：自助视角下「我是谁」
/// 由登录态决定，再回一遍只是给前端多一个可被改错的字段。11 个数值列沿用台账契约
/// [`RecordNumbers`]（同名列、同口径），前端表格列可以直接复用。
#[derive(Debug, Clone, Serialize)]
pub struct MyAttendanceRes {
    /// 记录主键。
    pub id: String,
    /// 考勤月 `yyyy-MM`。
    pub period_code: String,
    /// 期间状态码值（`DRAFT` / `SUBMITTED` / `LOCKED`）。
    pub period_status: String,
    /// 期间状态中文标签。
    pub period_status_label: String,
    /// 11 个数值列（摊平）。
    #[serde(flatten)]
    pub numbers: RecordNumbers,
    /// 加班合计（三个加班列之和，与台账同口径）。
    pub ot_hours_total: Decimal,
    /// 来源码值（`IMPORT` / `MANUAL` / `LEAVE_SYNC`）。
    pub source: String,
    /// 来源中文标签。
    pub source_label: String,
    /// 备注（HR 写的解释，例如「补录半天事假」）。
    pub remark: Option<String>,
}

/// 「我的请假」一行 = 我提的一张请假单。
///
/// **不含**审批轨迹（那是 `/leave/{id}` + `leave:detail` 码的事）：员工自助只要
/// 「我请了几天、批了没有、什么时候请的」。
#[derive(Debug, Clone, Serialize)]
pub struct MyLeaveRes {
    /// 单据主键。
    pub id: String,
    /// 单号（`LV-yyyyMMdd-000001`）。
    pub leave_no: String,
    /// 假别码值。
    pub leave_type: String,
    /// 假别中文名。
    pub leave_type_label: String,
    /// 开始时间（`ISO8601 +08:00`）。
    pub start_time: String,
    /// 结束时间（`ISO8601 +08:00`）。
    pub end_time: String,
    /// 请假天数（0.5 天粒度）。
    pub duration_days: Decimal,
    /// 是否带薪（薪资扣款只看这一列）。
    pub paid: bool,
    /// 状态码值。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 事由。
    pub reason: Option<String>,
    /// 提交时间（草稿为 `None`）。
    pub submitted_at: Option<String>,
    /// 作废时间。
    pub cancelled_at: Option<String>,
    /// 作废原因。
    pub cancel_reason: Option<String>,
}

/// 「我的薪资」一个明细项。
///
/// **不含** `expr_snapshot` / `calc_note`：公式快照是 HR 的对账依据（设计 §5.3 工资表导出），
/// 暴露给员工等于把「扣款是怎么算出来的」交给一个可能不认同公式的人去逐字挑战 ——
/// 自助页只给「项目名 + 金额」，口径解释由 HR 负责。带 `item_type` 是为了前端能分包展示
/// （收入 / 应发前扣减 / 代扣）。
#[derive(Debug, Clone, Serialize)]
pub struct MySalaryItemRes {
    /// 薪资项编码（`BASE` / `OT_WD` / `SI_PERSONAL` …）。
    pub item_code: String,
    /// 薪资项名称。
    pub item_name: String,
    /// 类型码值（`EARNING` / `PRE_DEDUCT` / `WITHHOLD` / `EMPLOYER`）。
    pub item_type: String,
    /// 类型中文名。
    pub item_type_label: String,
    /// 金额（可负：补发 / 冲回，见设计 §4.8.5）。
    pub amount: Decimal,
}

/// 「我的薪资」一行 = 我在某一期的工资条（单头摘要 + 我的明细行 + 明细项）。
#[derive(Debug, Clone, Serialize)]
pub struct MySalaryRes {
    /// 薪资单主键。
    pub sheet_id: String,
    /// 单号（`SAL-yyyyMM-000001`）。
    pub sheet_no: String,
    /// 薪资期间 `yyyy-MM`。
    pub period_code: String,
    /// 单状态码值。
    pub sheet_status: String,
    /// 单状态中文名。
    pub sheet_status_label: String,
    /// 是否已发放（`sheet_status == POSTED`）。
    ///
    /// 单独给出而不是让前端 `switch(status)`：员工页要显示「已发放 / 已批准待发放 / 已红冲」，
    /// 这三句话与六个状态码不是一一对应（`REVERSED` 也是「不再发放」）。
    pub paid: bool,
    /// 我的明细行主键。
    pub detail_id: String,
    /// 工号（**快照列**：工资表是审计类，显示当时的工号）。
    pub employee_no: String,
    /// 姓名（**快照列**）。
    pub employee_name: String,
    /// 部门名（**快照列**：部门改名后工资表不漂移）。
    pub dept_name: Option<String>,
    /// 岗位码值（快照）。
    pub position_code: Option<String>,
    /// 职级（快照）。
    pub job_level: Option<String>,
    /// 应出勤天数（考勤快照）。
    pub should_days: Decimal,
    /// 实际出勤天数。
    pub actual_days: Decimal,
    /// 无薪假天数（本次扣款的依据）。
    pub leave_unpaid_days: Decimal,
    /// 加班小时（三列之和）。
    pub ot_hours: Decimal,
    /// Σ EARNING。
    pub earning_amount: Decimal,
    /// Σ PRE_DEDUCT。
    pub pre_deduct_amount: Decimal,
    /// 应发 = `earning_amount - pre_deduct_amount`。
    pub gross_amount: Decimal,
    /// 社保个人。
    pub social_personal: Decimal,
    /// 公积金个人。
    pub fund_personal: Decimal,
    /// 个税。
    pub iit_amount: Decimal,
    /// 其他代扣。
    pub other_withhold: Decimal,
    /// 代扣合计。
    pub withhold_amount: Decimal,
    /// 实发 = `gross_amount - withhold_amount`。
    pub net_amount: Decimal,
    /// 明细行备注（HR 的书面解释，例如「本月含补发 300」）。
    pub remark: Option<String>,
    /// 明细项（按 `line_no` 升序；空数组 = 该项数据缺失，不该 500）。
    pub items: Vec<MySalaryItemRes>,
}

// ==================== 投影辅助 ====================

/// UTC 时间 → `ISO8601 +08:00`（与员工档案 / 台账同一格式）。
pub fn timestamp_cst(at: DateTime<Utc>) -> String {
    employee_dto::timestamp_cst(at)
}

/// 明细项类型 → 中文名（`biz_salary_item.item_type` 的五个取值）。
pub fn item_type_label(item_type: &str) -> String {
    match item_type {
        "EARNING" => "收入".to_string(),
        "PRE_DEDUCT" => "应发前扣减".to_string(),
        "WITHHOLD" => "代扣".to_string(),
        "EMPLOYER" => "单位承担".to_string(),
        "SUMMARY" => "汇总".to_string(),
        other => other.to_string(),
    }
}

/// 期间状态 → 中文名（复用期间列表的同一份词表，不另写一份）。
pub fn period_status_label(status: &str) -> String {
    attendance_period_dto::status_label(status).to_string()
}

/// 考勤来源 → 中文名（复用台账词表）。
pub fn source_label(source: &str) -> String {
    attendance_record_dto::source_label(source).to_string()
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");
    const V0387_SEED: &str = include_str!("../../../migrations/business/V0387__hr_menu_perms.up.sql");

    /// 三条入参结构里**不许**出现身份字段（越权请求连表达方式都没有）。
    #[test]
    fn query_structs_carry_no_identity_fields() {
        let attendance = serde_json::to_value(MyAttendanceQuery::default()).expect("可序列化");
        let leave = serde_json::to_value(MyLeaveQuery::default()).expect("可序列化");
        let salary = serde_json::to_value(MySalaryQuery::default()).expect("可序列化");
        for (name, value) in [("attendance", attendance), ("leave", leave), ("salary", salary)] {
            let keys: Vec<&str> = value.as_object().expect("对象").keys().map(String::as_str).collect();
            for forbidden in [
                "employee_id",
                "employeeId",
                "user_id",
                "userId",
                "sheet_id",
                "detail_id",
                "id",
            ] {
                assert!(
                    !keys.contains(&forbidden),
                    "{name} 入参不许带身份字段 {forbidden}（自助接口只能看本人），实际 {keys:?}"
                );
            }
            println!("[自助] {name} 入参键：{keys:?} ✓");
        }
    }

    /// 分页归一：下界 / 上界 / 缺省三档。
    #[test]
    fn paging_is_clamped() {
        assert_eq!(normalize_paging(None, None), (1, MY_PAGE_DEFAULT));
        assert_eq!(normalize_paging(Some(0), Some(0)), (1, 1));
        assert_eq!(normalize_paging(Some(3), Some(10_000)), (3, MY_PAGE_MAX));
        assert_eq!(normalize_paging(Some(2), Some(30)), (2, 30));
    }

    /// `yyyy-MM`：合法值原样、空串当没给、形状/月份错都点名字段。
    #[test]
    fn period_code_is_validated_against_the_db_check() {
        assert_eq!(
            parse_period_code(Some("2026-11")).expect("合法"),
            Some("2026-11".to_string())
        );
        assert_eq!(parse_period_code(Some("  ")).expect("空串当没给"), None);
        assert_eq!(parse_period_code(None).expect("缺省"), None);
        for bad in [
            "2026-13",
            "2026-00",
            "26-11",
            "202611",
            "2026-1",
            "2026-11-01",
            "abcdefg",
        ] {
            let error = parse_period_code(Some(bad)).expect_err(bad);
            assert_eq!(error.status, 400, "{bad}");
            assert!(error.msg.contains("period_code"), "报文要点名：{}", error.msg);
        }
    }

    /// 请假状态：大小写不敏感、未知码点名收到的值。
    #[test]
    fn leave_status_is_whitelisted() {
        assert_eq!(
            parse_leave_status(Some("approved")).expect("小写也认"),
            Some("APPROVED")
        );
        assert_eq!(
            parse_leave_status(Some(" SUBMITTED ")).expect("去空格"),
            Some("SUBMITTED")
        );
        assert_eq!(parse_leave_status(Some("")).expect("空串当没给"), None);
        let error = parse_leave_status(Some("PAID")).expect_err("不是请假状态");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("PAID"), "{}", error.msg);
        // 词表必须与 V0383 的 CHECK 逐字一致
        for status in LEAVE_STATUSES {
            assert!(
                V0383_DDL.contains(&format!("'{status}'")),
                "请假状态 {status} 不在 V0383 的 ck_biz_leave_status 里"
            );
        }
    }

    /// 薪资可见状态：两个分支恰好覆盖 `ck_biz_sal_sheet_status` 的六档，且互不重叠。
    #[test]
    fn visible_statuses_cover_the_status_check_exactly() {
        let visible = visible_statuses(false);
        assert_eq!(visible, VISIBLE_STATUSES.to_vec());
        let all = visible_statuses(true);
        assert_eq!(all.len(), 6, "六档状态都要能被枚举到一次：{all:?}");
        for status in &all {
            assert!(
                V0383_DDL.contains(&format!("'{status}'")),
                "状态 {status} 不在 V0383 的 ck_biz_sal_sheet_status 里"
            );
        }
        // 默认分支不许含在途（员工先看到中途数 = 纠纷源头）
        for pending in PENDING_STATUSES {
            assert!(!visible.contains(&pending), "{pending} 不该默认可见");
        }
        // 默认分支必须含已过账（否则「已发工资」在自助页里查不到）
        assert!(visible.contains(&salary_sheet_dto::STATUS_POSTED));
        println!("[自助] 可见状态 default={visible:?} / all={all:?} ✓");
    }

    /// 权限码必须在 `V0387` 的菜单种子里（写错只在线上变 403）。
    #[test]
    fn salary_my_permission_is_seeded() {
        assert!(
            V0387_SEED.contains(PERM_SALARY_MY),
            "权限码 {PERM_SALARY_MY} 不在 V0387 种子里 —— 线上会 403"
        );
    }

    /// 工资条回执**不许**泄露公式快照（那是 HR 的对账依据）。
    #[test]
    fn payslip_item_does_not_expose_formula_snapshot() {
        let item = MySalaryItemRes {
            item_code: "OT_WD".to_string(),
            item_name: "平时加班费".to_string(),
            item_type: "EARNING".to_string(),
            item_type_label: item_type_label("EARNING"),
            amount: Decimal::new(68966, 2),
        };
        let value = serde_json::to_value(&item).expect("可序列化");
        let object = value.as_object().expect("对象");
        for leaked in ["expr_snapshot", "calc_note", "formula"] {
            assert!(!object.contains_key(leaked), "工资条不许带 {leaked}");
        }
        // 金额按字符串往返（避免 JS 浮点）
        assert_eq!(value["amount"], serde_json::json!("689.66"));
        assert_eq!(value["item_type_label"], serde_json::json!("收入"));
    }

    /// 请假回执的时间格式（`+08:00`）与金额类型。
    #[test]
    fn leave_res_serializes_cst_timestamps() {
        let at = DateTime::parse_from_rfc3339("2026-11-12T02:30:00Z")
            .expect("字面量")
            .with_timezone(&Utc);
        let res = MyLeaveRes {
            id: "x".to_string(),
            leave_no: "LV-20261112-000001".to_string(),
            leave_type: "ANNUAL".to_string(),
            leave_type_label: leave_dto::leave_type_label("ANNUAL").to_string(),
            start_time: timestamp_cst(at),
            end_time: timestamp_cst(at),
            duration_days: Decimal::new(150, 2),
            paid: true,
            status: "APPROVED".to_string(),
            status_label: leave_dto::status_label("APPROVED").to_string(),
            reason: Some("返乡".to_string()),
            submitted_at: None,
            cancelled_at: None,
            cancel_reason: None,
        };
        assert_eq!(res.start_time, "2026-11-12T10:30:00+08:00");
        let value = serde_json::to_value(&res).expect("可序列化");
        assert_eq!(value["duration_days"], serde_json::json!("1.50"));
        assert!(value["submitted_at"].is_null());
    }

    /// 期间状态 / 来源标签走的是台账那份词表（不另写一份，避免两处文案漂移）。
    #[test]
    fn labels_reuse_the_ledger_vocabulary() {
        assert_eq!(
            period_status_label("LOCKED"),
            attendance_period_dto::status_label("LOCKED")
        );
        assert_eq!(source_label("MANUAL"), attendance_record_dto::source_label("MANUAL"));
        assert_eq!(item_type_label("WITHHOLD"), "代扣");
        // 未知码原样返回（排障时看到码值比看到「未知」有用）
        assert_eq!(item_type_label("SOMETHING_NEW"), "SOMETHING_NEW");
    }
}
