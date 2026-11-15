//! 请假单入参 / 回执契约（`biz_leave_request` · HR-24）。
//!
//! 设计定稿：`docs/architecture/ARCH-9-人力资源模块设计.md` §3.1 ⑥（建表 DDL）、
//! §4.2.2（状态机）、§5.1（出参口径）、§5.2 #26~#31（6 条接口）。**本卡只做契约与校验**：
//! SQL 在 [`crate::business::hr::leave_model`]，接口层与事务在 **HR-25**（CRUD + 提交/撤回/作废）、
//! **HR-27**（审批回调桥）。本文件零 SQL、零事务、零权限判断，全是可单测的纯函数。
//!
//! # 接口一览（设计 §5.2 的 6 条，路径**不含** `/api`，由 `[router] prefix` 注入）
//!
//! | # | 方法 | 路径 | 入参 | 回执 |
//! |:--:|---|---|---|---|
//! | 26 | GET | `/business/hr/leave/page` | [`LeavePageQuery`] | `PageData<Vec<[`LeaveRes`]>>` |
//! | 27 | GET | `/business/hr/leave/{id}` | — | [`LeaveRes`] + 审批轨迹（HR-27 补） |
//! | 28 | POST | `/business/hr/leave` | [`LeaveCreateReq`] | [`LeaveRes`] |
//! | 29 | PUT | `/business/hr/leave/{id}` | [`LeaveUpdateReq`] | [`LeaveRes`] |
//! | 30 | POST | `/business/hr/leave/{id}/submit` | [`LeaveSubmitReq`] | [`LeaveRes`] |
//! | 31 | POST | `/business/hr/leave/{id}/cancel` | [`LeaveCancelReq`] | [`LeaveRes`] |
//!
//! 「我的请假」（设计 §5.2 #41 `/leave/my`）**不在本卡**：它随 HR-41 落，走
//! `GET /leave/page?mine=true`（列表筛选里的 [`LeavePageQuery::mine`] 已经预留这个开关）。
//!
//! # 字段口径（逐条对得上 `V0383__hr_tables.up.sql` 的列与 CHECK）
//!
//! | 列 | 类型 / 域 | 校验口径与理由 |
//! |---|---|---|
//! | `leave_no` | `VARCHAR(32)`，`LV-yyyyMMdd-000001` | **服务端取号**（`biz_seq` 的 `LV` 号段，HR-05 已种）；前端传了也忽略 —— 单号是审计标识，不能让客户端指定 |
//! | `employee_id` | `UUID NOT NULL` FK | 必填、必须是未删除员工；`employee_no` / `employee_name` / `dept_id` 由服务端从 `biz_employee` **抄快照**，不信任前端 |
//! | `leave_type` | 9 值 CHECK | 与字典 `hr_leave_type` 一一对应（[`LEAVE_TYPES`]），越界即 400 |
//! | `start_time` / `end_time` | `TIMESTAMPTZ NOT NULL` | 精确到**时**（半天假用 12:00 切）；`end > start`（库 `ck_biz_leave_range`）+ 天数校验见下 |
//! | `duration_days` | `NUMERIC(6,2)`，`> 0` | **0.5 天粒度**（列注释）；且不得超过起止覆盖的自然日数（下面「两道校验的解释」） |
//! | `paid` | `SMALLINT`，`IN (0,1)` | 1 带薪 / 0 无薪。**不按假别推导**：设计 §3.1 ⑥ 的列注释明确「请假类型与带薪与否解耦」，薪资扣款只看这一列 |
//! | `status` | 4 值 CHECK | 新建恒为 `DRAFT`；流转只走 `shared::state_machine`（[`LEAVE_EDGES`](crate::business::shared::state_machine::LEAVE_EDGES)） |
//! | `reason` | `VARCHAR(500)` | 请假事由（前端必填感由页面负责，契约层只封长度；`NULL` 合法） |
//! | `cancel_reason` | `VARCHAR(255)` | 作废原因**≥ 5 字**（设计 §5.2 #31）；库 `ck_biz_leave_cancel` 只要求非空，这里更严 |
//! | `remark` | `TEXT` | 封 1000 字（挡住「把整封邮件贴进备注」），与合同台账同口径 |
//!
//! # 为什么「0.5 天粒度」之外还要「天数 ≤ 起止跨度」
//!
//! 库层只有 `ck_biz_leave_days CHECK (duration_days > 0)`。于是
//! `start_time = 11-10 09:00 / end_time = 11-20 18:00 / duration_days = 1` 在库里**完全合法**，
//! 但它意味着「请了 10 天假只扣 1 天」—— 这张单会直接进 `ATT_LEAVE_UNPAID_DAYS`
//! （设计 §4.2.2 末条：`paid=0` 的假才进薪资扣款基线），错一天就错一笔工资，而且是
//! **审批通过后**才在工资表上显形。所以契约层补一道「天数不得超过起止覆盖的自然日数」，
//! 报文里把两个数都摆出来让 HR 自己改。
//!
//! 反向（天数 > 跨度）同样拒：`11-10 09:00 ~ 11-10 18:00` 填 3 天，等于凭空多出两天假。
//!
//! # 状态与「不可改」（设计 §4.2.2）
//!
//! ```text
//! DRAFT ──提交──> SUBMITTED ──审批通过──> APPROVED ──销假/作废(给原因)──> CANCELLED
//!   │                 │
//!   │                 └──驳回/撤回──> DRAFT
//!   └──删除（软删）──> (deleted_at)
//! ```
//!
//! - 只有 `DRAFT` 可编辑（[`EDITABLE_STATUSES`]）、可删除；
//! - `APPROVED` / `CANCELLED` 是锁死态（[`is_locked`] 直接复用
//!   `state_machine::LEAVE_LOCKED`，不另立一份清单 —— 两份清单迟早漂移）；
//! - `CANCELLED` 必须带原因，所以 [`LeaveCancel`] 的 `reason` 是**必填**而不是 `Option`。
//!
//! # 姓名：快照 vs 实时 join（`AGENTS.md` 约定 13）
//!
//! - `employee_no` / `employee_name` 是**请假发生时的快照列**（DDL 里就有）：员工改名或离职后，
//!   这张单要能回溯「当时是谁请的假」；
//! - `dept_id` 是主数据关联 → 出参 `dept_name` 由 SQL **实时 join**（部门改名/调整后显示现在的归属）；
//! - `submitted_by` / `cancelled_by` 是审计列 → 出参 join `sys_user` 取姓名，取不到回退登录名
//!   （显示空比显示 uuid 更没用，同 `attendance_period_model` 的口径）。

use nik::prelude::chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::hr::employee_dto::{self, TZ_OFFSET_SECS};
use crate::business::shared::state_machine;
use crate::shared::error_code;

// ==================== 状态域（与 V0383 的 ck_biz_leave_status 逐字一致） ====================

/// 草稿（可改 / 可提交 / 可删除）。
pub const STATUS_DRAFT: &str = "DRAFT";

/// 已提交（等审批中；可撤回回草稿）。
pub const STATUS_SUBMITTED: &str = "SUBMITTED";

/// 已批准（锁死：改数据只能作废重开，设计 §4.2.2）。
pub const STATUS_APPROVED: &str = "APPROVED";

/// 已作废（锁死：销假 / 草稿外的终止；必须带原因）。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 四个状态值（顺序 = 状态机正向顺序，与 `state_machine::LEAVE_EDGES` 的域一致）。
pub const STATUS_VALUES: [&str; 4] = [STATUS_DRAFT, STATUS_SUBMITTED, STATUS_APPROVED, STATUS_CANCELLED];

/// 请假单**详情**接口的码（`GET /business/hr/leave/{id}`）。
///
/// 语义就是「能打开一张别人的请假单」，因此它同时是「能否看全员请假」的判据来源。
///
/// # 与页面码的关系（卡 t_0af7ec46 解耦后）
///
/// `business:hr:leave:list`（全量列表）与 `business:hr:leave:page`（页面显形）已分成两个码
/// （迁移 `V0401`）：页面码只在 `sys_menu` 的页面行上，员工自助持有它**不再**意味着持有
/// 全量列表码。所以列表接口**不需要**再用 `leave:detail` 做第二道数据闸
/// （`t_be54535c` 的 `scope_to_self_without_detail` 已删除）；员工的自助入口是
/// `/business/hr/leave/my`（登录即可、只出本人）。
pub const PERM_LEAVE_DETAIL: &str = "business:hr:leave:detail";

/// 状态中文名（列表 / 回执的 `status_label`，文案与设计 §6 的页面一致）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SUBMITTED => "审批中",
        STATUS_APPROVED => "已批准",
        STATUS_CANCELLED => "已作废",
        // 库层 CHECK 挡住其它值；走到这里说明代码与迁移漂移了，回显码值比空白强
        _ => "未知",
    }
}

/// 是否锁死（`APPROVED` / `CANCELLED`）。
///
/// 直接复用 `state_machine::LEAVE_LOCKED`：状态机的「不可改」清单与接口层的编辑闸
/// **必须**是同一份，否则会出现「状态机说锁了、接口还能改」或反之。
pub fn is_locked(status: &str) -> bool {
    state_machine::LEAVE_LOCKED.contains(&status)
}

/// 可编辑 / 可删除的状态（只有草稿）。
pub const EDITABLE_STATUSES: [&str; 1] = [STATUS_DRAFT];

/// 可提交的状态（只有草稿）。
pub const SUBMITTABLE_STATUSES: [&str; 1] = [STATUS_DRAFT];

/// 可撤回的状态（只有审批中）。
pub const WITHDRAWABLE_STATUSES: [&str; 1] = [STATUS_SUBMITTED];

/// 可作废的状态（设计 §5.2 #31：「`APPROVED` 也可作废 = 销假」）。
///
/// - `DRAFT`：草稿直接作废 = 软删（handler 走 `DELETE` / 软删，不写 `CANCELLED`）——
///   库里留着一条没意义也没原因的 `CANCELLED` 只会污染列表；所以本清单**不含** `DRAFT`。
/// - `SUBMITTED`：审批中要撤销，正常路径是「撤回 → 草稿 → 删」，但 HR 直接作废也合法
///   （比如员工当天离职，单据不必再走撤回）。
/// - `APPROVED`：销假（假没休完 / 提前返岗），必须给原因。
/// - `CANCELLED`：终态，不能再作废一次。
pub const CANCELLABLE_STATUSES: [&str; 2] = [STATUS_SUBMITTED, STATUS_APPROVED];

// ==================== 假别域（与 ck_biz_leave_type 逐字一致，一一对应字典 hr_leave_type） ====================

/// 年假。
pub const LEAVE_ANNUAL: &str = "ANNUAL";
/// 病假（按比例发，比例在薪资项公式里配）。
pub const LEAVE_SICK: &str = "SICK";
/// 事假（`paid = 0` 的典型）。
pub const LEAVE_PERSONAL: &str = "PERSONAL";
/// 婚假。
pub const LEAVE_MARRIAGE: &str = "MARRIAGE";
/// 产假。
pub const LEAVE_MATERNITY: &str = "MATERNITY";
/// 陪产假。
pub const LEAVE_PATERNITY: &str = "PATERNITY";
/// 丧假。
pub const LEAVE_BEREAVEMENT: &str = "BEREAVEMENT";
/// 调休（冲抵只在类型里存在，自动冲抵不做，设计 D-6）。
pub const LEAVE_COMPENSATORY: &str = "COMPENSATORY";
/// 其他。
pub const LEAVE_OTHER: &str = "OTHER";

/// 9 个假别（顺序 = 字典 `hr_leave_type` 的 `sort_order`，与 `ck_biz_leave_type` 逐字一致）。
pub const LEAVE_TYPES: [&str; 9] = [
    LEAVE_ANNUAL,
    LEAVE_SICK,
    LEAVE_PERSONAL,
    LEAVE_MARRIAGE,
    LEAVE_MATERNITY,
    LEAVE_PATERNITY,
    LEAVE_BEREAVEMENT,
    LEAVE_COMPENSATORY,
    LEAVE_OTHER,
];

/// 假别中文名（列表 / 回执的 `leave_type_label`，与设计 §3.1 的字典项逐字对应）。
pub fn leave_type_label(leave_type: &str) -> &'static str {
    match leave_type {
        LEAVE_ANNUAL => "年假",
        LEAVE_SICK => "病假",
        LEAVE_PERSONAL => "事假",
        LEAVE_MARRIAGE => "婚假",
        LEAVE_MATERNITY => "产假",
        LEAVE_PATERNITY => "陪产假",
        LEAVE_BEREAVEMENT => "丧假",
        LEAVE_COMPENSATORY => "调休",
        LEAVE_OTHER => "其他",
        _ => "未知",
    }
}

// ==================== 带薪域（`paid SMALLINT IN (0,1)`） ====================

/// 带薪（DDL 列默认值）。
pub const PAID_YES: i16 = 1;
/// 无薪（进薪资扣款基线 `ATT_LEAVE_UNPAID_DAYS`）。
pub const PAID_NO: i16 = 0;

/// `paid` 中文名。
pub fn paid_label(paid: i16) -> &'static str {
    match paid {
        PAID_YES => "带薪",
        PAID_NO => "无薪",
        _ => "未知",
    }
}

// ==================== 长度 / 上限 ====================

/// `leave_no VARCHAR(32)`。
pub const LEAVE_NO_LEN: usize = 32;
/// `reason VARCHAR(500)`。
pub const MAX_REASON: usize = 500;
/// `cancel_reason VARCHAR(255)`。
pub const MAX_CANCEL_REASON: usize = 255;
/// `remark TEXT`（库里无上限；接口层封顶，与合同台账同口径）。
pub const MAX_REMARK: usize = 1000;
/// 作废原因的最小字数（设计 §5.2 #31「`reason` ≥ 5 字」，与员工状态变更同口径）。
pub const MIN_CANCEL_REASON_CHARS: usize = 5;
/// 天数上限 = `NUMERIC(6,2)` 的精度上限（`9999.99`）。
///
/// 不设「一年」这种业务上限：病假可以连着请很久（长期病休），硬拦会把合法单挡在门外。
/// 真正防手滑的是「天数 ≤ 起止覆盖的自然日数」那条（见模块文档）。
pub const MAX_DURATION_DAYS: Decimal = Decimal::from_parts(999_999, 0, 0, false, 2);
/// 半天粒度（0.5 天）。
pub const HALF_DAY: Decimal = Decimal::from_parts(5, 0, 0, false, 1);
/// 默认每页条数。
pub const DEFAULT_PAGE_SIZE: u64 = 20;
/// 每页上限（挡住 `page_size=100000` 把列表变成导出）。
pub const MAX_PAGE_SIZE: u64 = 200;

// ==================== 错误原语 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
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

// ==================== 入参结构 ====================

/// 列表筛选（设计 §5.2 #26：按 dept 数据权限；`mine=true` 给「我的请假」tab 用）。
///
/// `status[]` / `leave_type[]` 的落地形态与员工列表一致：查询串**逗号分隔**
/// （`status=SUBMITTED,APPROVED`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LeavePageQuery {
    /// 关键字（单号 / 工号 / 姓名，`LIKE %kw%`）。
    #[serde(default)]
    pub keyword: Option<String>,
    /// 指定员工。
    #[serde(default)]
    pub employee_id: Option<String>,
    /// 指定部门。
    #[serde(default)]
    pub dept_id: Option<String>,
    /// 假别多选，逗号分隔。
    #[serde(default)]
    pub leave_type: Option<String>,
    /// 状态多选，逗号分隔。
    #[serde(default)]
    pub status: Option<String>,
    /// 请假起始日的下界（含当天，`yyyy-MM-dd`）。
    #[serde(default)]
    pub date_from: Option<String>,
    /// 请假起始日的上界（含当天，`yyyy-MM-dd`）。
    #[serde(default)]
    pub date_to: Option<String>,
    /// 只看本人（「我的请假」tab；由 handler 按令牌里的 `user_id` 解析成员工过滤）。
    #[serde(default)]
    pub mine: Option<bool>,
    /// 页码（从 1 开始）。
    #[serde(default)]
    pub page: Option<u64>,
    /// 每页条数（默认 20，上限 200）。
    #[serde(default)]
    pub page_size: Option<u64>,
}

/// 建单入参（设计 §5.2 #28：`employee_id`、类型、起止、`duration_days`、`paid`、事由）。
///
/// 字段全 `Option`：把「没填」与「填错」区分成两条 400 人话，而不是让 serde 抛
/// `missing field \`employee_id\``（前端只能直出英文）。
/// `leave_no` / `status` / `employee_no` / `employee_name` / `dept_id` **都不收**：
/// 单号服务端取号，状态恒为 `DRAFT`，员工快照由服务端查 `biz_employee` 抄（见模块文档）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LeaveCreateReq {
    /// 员工 id（必填）。
    #[serde(default)]
    pub employee_id: Option<String>,
    /// 假别（必填，[`LEAVE_TYPES`]）。
    #[serde(default)]
    pub leave_type: Option<String>,
    /// 起（含，精确到分；`2026-11-10 09:00` / `2026-11-10T09:00:00+08:00`）。
    #[serde(default)]
    pub start_time: Option<String>,
    /// 止（不含当天后半段的口径由天数表达；必须晚于起）。
    #[serde(default)]
    pub end_time: Option<String>,
    /// 请假天数（0.5 的整数倍）。
    #[serde(default)]
    pub duration_days: Option<Decimal>,
    /// 1 带薪 / 0 无薪（默认 1，与 DDL 列默认值一致）。
    #[serde(default)]
    pub paid: Option<i16>,
    /// 请假事由。
    #[serde(default)]
    pub reason: Option<String>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 改草稿入参（设计 §5.2 #29：`PUT /business/hr/leave/{id}`）。
///
/// **部分更新**语义：缺省的字段保持库里的值；可空文本传空串 = 清空。
/// 不可改的列：`employee_id`（换人就作废重开 —— 换人会让已走的审批轨迹与考勤口径失效）、
/// `leave_no`、`status`（走状态机）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LeaveUpdateReq {
    /// 假别。
    #[serde(default)]
    pub leave_type: Option<String>,
    /// 起。
    #[serde(default)]
    pub start_time: Option<String>,
    /// 止。
    #[serde(default)]
    pub end_time: Option<String>,
    /// 请假天数。
    #[serde(default)]
    pub duration_days: Option<Decimal>,
    /// 1 带薪 / 0 无薪。
    #[serde(default)]
    pub paid: Option<i16>,
    /// 事由（传空串 = 清空）。
    #[serde(default)]
    pub reason: Option<String>,
    /// 备注（传空串 = 清空）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 提交审批入参（设计 §5.2 #30）。
///
/// 只带一个备注：提交动作本身不需要参数（发起 `hr_leave` 流程由 HR-25 做，
/// 审批人是流程按 `INITIATOR_SUPERIOR` 解析的，设计 §4.3）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LeaveSubmitReq {
    /// 提交说明（可选，进状态日志的 reason）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 作废入参（设计 §5.2 #31：`reason` ≥ 5 字；`APPROVED` 也可作废 = 销假）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LeaveCancelReq {
    /// 作废 / 销假原因（必填，≥ [`MIN_CANCEL_REASON_CHARS`] 字）。
    #[serde(default)]
    pub reason: Option<String>,
}

// ==================== 校验后的载荷（model / handler 直接吃这一份） ====================

/// 建单的校验结果：只含 `biz_leave_request` 的**业务列**。
///
/// 员工快照（`employee_no` / `employee_name` / `dept_id`）与单号不在这里：
/// 前者由 handler 从 `biz_employee` 抄（不信任前端），后者服务端取号 —— 一起塞进来会让
/// 「谁能决定这三个值」变模糊。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveCreate {
    /// 员工 id。
    pub employee_id: Uuid,
    /// 假别码值。
    pub leave_type: String,
    /// 起（UTC 内部表示，出参转 `+08:00`）。
    pub start_time: DateTime<Utc>,
    /// 止。
    pub end_time: DateTime<Utc>,
    /// 请假天数（0.5 粒度）。
    pub duration_days: Decimal,
    /// 1 带薪 / 0 无薪。
    pub paid: i16,
    /// 事由。
    pub reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 草稿的**当前值**（改草稿时与之合并；由 `leave_model::LeaveRow::draft_state()` 给出）。
///
/// 只含可改的 7 列 —— 有了它，[`patch_of`] 才能在纯函数里完成「缺省 = 不改」的合并与
/// 「合并后的天数与起止是否自洽」的校验，不必连库。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveDraftState {
    /// 假别码值。
    pub leave_type: String,
    /// 起。
    pub start_time: DateTime<Utc>,
    /// 止。
    pub end_time: DateTime<Utc>,
    /// 请假天数。
    pub duration_days: Decimal,
    /// 1 带薪 / 0 无薪。
    pub paid: i16,
    /// 事由。
    pub reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 改草稿的校验结果（已与库里的当前值合并，全量 7 列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeavePatch {
    /// 假别码值。
    pub leave_type: String,
    /// 起。
    pub start_time: DateTime<Utc>,
    /// 止。
    pub end_time: DateTime<Utc>,
    /// 请假天数。
    pub duration_days: Decimal,
    /// 1 带薪 / 0 无薪。
    pub paid: i16,
    /// 事由。
    pub reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 与库里当前值相比是否有改动（无改动不写库、不写状态日志）。
    pub changed: bool,
}

/// 作废的校验结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveCancel {
    /// 作废 / 销假原因（已 trim，≥ 5 字）。
    pub reason: String,
}

/// 列表筛选（校验后的形态；多选项已拆开并逐项验过域，日期已解析）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeaveFilter {
    /// 关键字（单号 / 工号 / 姓名）。
    pub keyword: Option<String>,
    /// 指定员工。
    pub employee_id: Option<Uuid>,
    /// 指定部门。
    pub dept_id: Option<Uuid>,
    /// 假别多选（空 = 全部）。
    pub leave_types: Vec<String>,
    /// 状态多选（空 = 全部）。
    pub statuses: Vec<String>,
    /// 请假起始日下界（含）。
    pub date_from: Option<NaiveDate>,
    /// 请假起始日上界（含）。
    pub date_to: Option<NaiveDate>,
    /// 只看本人。
    pub mine: bool,
}

// ==================== 出参 ====================

/// 请假单出参（列表与详情**共用同一形状**）。
///
/// 列表 / 详情各写一套形状迟早漂移（「列表有 `dept_name`、详情没有」这类），
/// 所以只留一份；详情多出来的审批轨迹由 HR-27 在 handler 层包一层，不改本结构。
///
/// 所有 `*_id` 都配了 `*_name`（`AGENTS.md` 约定 13）：`employee_id` → `employee_no` +
/// `employee_name`（快照列）、`dept_id` → `dept_name`（join 实时取名）、
/// `submitted_by` → `submitted_by_name`、`cancelled_by` → `cancelled_by_name`（join `sys_user`）。
/// `approval_instance_id` 没有「名字」这个概念，前端拿它去 `flow` 模块查轨迹。
#[derive(Debug, Clone, Serialize)]
pub struct LeaveRes {
    /// 主键。
    pub id: String,
    /// 单号（`LV-yyyyMMdd-000001`）。
    pub leave_no: String,
    /// 员工 id。
    pub employee_id: String,
    /// 工号（快照）。
    pub employee_no: String,
    /// 姓名（快照：请假当时是谁）。
    pub employee_name: String,
    /// 部门 id。
    pub dept_id: Option<String>,
    /// 部门名（join 实时取；部门被删或未填时为空串）。
    pub dept_name: String,
    /// 假别码值。
    pub leave_type: String,
    /// 假别中文名。
    pub leave_type_label: String,
    /// 起（ISO8601 `+08:00`）。
    pub start_time: String,
    /// 止（ISO8601 `+08:00`）。
    pub end_time: String,
    /// 请假天数（`Decimal` → JSON 字符串，前端不做浮点）。
    pub duration_days: Decimal,
    /// 1 带薪 / 0 无薪。
    pub paid: i16,
    /// 带薪与否的中文名。
    pub paid_label: String,
    /// 状态码值。
    pub status: String,
    /// 状态中文名。
    pub status_label: String,
    /// 是否锁死（`APPROVED` / `CANCELLED`）：前端据此禁掉编辑按钮，不自己 `match` 状态串。
    pub locked: bool,
    /// 请假事由。
    pub reason: Option<String>,
    /// 审批实例 id。
    pub approval_instance_id: Option<String>,
    /// 提交时间（ISO8601 `+08:00`）。
    pub submitted_at: Option<String>,
    /// 提交人 id。
    pub submitted_by: Option<String>,
    /// 提交人姓名（join；审计列，取不到回退登录名）。
    pub submitted_by_name: Option<String>,
    /// 作废时间。
    pub cancelled_at: Option<String>,
    /// 作废人 id。
    pub cancelled_by: Option<String>,
    /// 作废人姓名。
    pub cancelled_by_name: Option<String>,
    /// 作废 / 销假原因。
    pub cancel_reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: String,
    /// 最后修改时间。
    pub updated_at: String,
}

/// 分页参数归一（`page` 从 1 起，`page_size` 夹在 1..=[`MAX_PAGE_SIZE`]）。
pub fn normalize_page(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    let page = page.unwrap_or(1).max(1);
    let page_size = page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE);
    (page, page_size)
}

// ==================== 详情 / 作废回执（HR-25 · handler 层包装，不动 [`LeaveRes`]） ====================

/// 详情接口（#27）的回执：**`LeaveRes` 的字段全部平铺在顶层** + 审批轨迹。
///
/// # 为什么用 `#[serde(flatten)]` 而不是 `{ "leave": {...}, "approval": {...} }`
///
/// 设计 §5.2 #27 是「详情 + 审批轨迹」，而列表 / 详情共用 [`LeaveRes`] 形状是 HR-24 定下的
/// 口径（前端 HR-28 的列表行与详情抽屉吃同一个类型）。平铺之后：
/// 详情回执 = 列表行 + 两个额外键 ⇒ 前端可以用**同一个** `LeaveRes` 类型接收，
/// 再按需读 `approval`；不会出现「列表 30 个字段、详情 30 个字段嵌一层」的两套映射。
///
/// # `approval` 何时为空
///
/// ① 草稿（还没提交，没有实例）；② `approval_instance_id` 指向的实例已被硬删
/// （历史脏数据，不报错 —— 详情不该因为审批单缺失整个 404）。
#[derive(Debug, Clone, Serialize)]
pub struct LeaveDetailRes {
    /// 请假单本体（字段平铺）。
    #[serde(flatten)]
    pub leave: LeaveRes,
    /// 当前审批实例 + 时间线（`None` = 还没提交过 / 实例已不存在）。
    pub approval: Option<LeaveApprovalRes>,
    /// 历史上提交过几次（撤回重提会 > 1）。前端「审批轨迹」tab 的角标用它。
    pub approval_history: i32,
}

/// 请假单的审批实例摘要 + 时间线（详情接口 #27 的 `approval` 字段）。
///
/// 字段全部来自 `business::flow`：实例行（`flow_model::find_instance`）、节点名快照
/// （`flow_model::nodes_of`，`node_key → node_name` 的映射）、时间线（`flow_model::timeline`）。
/// **本模块不写任何流程 SQL** —— 审批的真相只在 flow 模块。
#[derive(Debug, Clone, Serialize)]
pub struct LeaveApprovalRes {
    /// 审批实例 id。
    pub instance_id: String,
    /// 审批单号（`AP-yyyyMMdd-000001`）。
    pub instance_no: String,
    /// 流程编码（`hr_leave`）。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 实例状态（`PENDING` / `APPROVED` / `REJECTED` / `CANCELLED` / `WITHDRAWN`）。
    pub status: String,
    /// 实例状态中文名。
    pub status_label: String,
    /// 发起人姓名（快照）。
    pub initiator_name: String,
    /// 发起时间（ISO8601 `+08:00`）。
    pub started_at: String,
    /// 结束时间（未完成为 `None`）。
    pub ended_at: Option<String>,
    /// 结束说明（审批意见 / 终止原因）。
    pub result_comment: Option<String>,
    /// 审批时间线（按 `round, created_at, id` 升序）。
    pub timeline: Vec<LeaveApprovalTaskRes>,
}

/// 审批时间线上的一行。
///
/// 只出「谁、在哪个节点、什么结论、什么时候、什么意见」——够审批人回看，也不把
/// 流程引擎的内部字段（`assignee_source` / `revision` / 抄送 JSON）漏给 HR 页面。
#[derive(Debug, Clone, Serialize)]
pub struct LeaveApprovalTaskRes {
    /// 节点 key。
    pub node_key: String,
    /// 节点中文名（实例节点快照；取不到时回退 `node_key`）。
    pub node_name: String,
    /// 轮次（退回重提会 > 1）。
    pub round: i32,
    /// 审批人 id。
    pub assignee_id: String,
    /// 审批人姓名（快照列）。
    pub assignee_name: String,
    /// 待办状态（`PENDING` / `APPROVED` / `REJECTED` / `SKIPPED` / `TRANSFERRED` …）。
    pub status: String,
    /// 待办状态中文名。
    pub status_label: String,
    /// 审批意见。
    pub comment: Option<String>,
    /// 是否自审（内控留痕）。
    pub is_self_approval: bool,
    /// 是否系统代点（免审直过 / 超时自动）。
    pub is_auto: bool,
    /// 到达时间。
    pub created_at: String,
    /// 处理时间（未完成为 `None`）。
    pub completed_at: Option<String>,
}

/// 作废接口（#31）做了哪个动作 —— 一条路径承载三件事，回执要说清楚做的是哪件。
///
/// 设计 §5.2 #31 只有一条 `POST /{id}/cancel`，但设计 §4.2.2 的状态机里
/// 「草稿删除 / 审批中撤回 / 已批准销假」是**三种不同后果**（软删 / 回草稿 / 进 `CANCELLED`）。
/// 前端按钮文案与后续提示都依赖这个区别，所以显式回传而不是让前端自己推。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveCancelAction {
    /// `DRAFT` → 软删（`deleted_at`），状态**不变**（草稿从未提交过）。
    Deleted,
    /// `SUBMITTED` → 撤回（终止在跑实例 + 回到 `DRAFT`）。
    Withdrawn,
    /// `APPROVED` → 作废 / 销假（进 `CANCELLED`，必须带原因）。
    Cancelled,
}

impl LeaveCancelAction {
    /// 落日志 / 回执用的码值。
    pub const fn as_str(self) -> &'static str {
        match self {
            LeaveCancelAction::Deleted => "DELETED",
            LeaveCancelAction::Withdrawn => "WITHDRAWN",
            LeaveCancelAction::Cancelled => "CANCELLED",
        }
    }

    /// 中文名（前端直接显示）。
    pub const fn label(self) -> &'static str {
        match self {
            LeaveCancelAction::Deleted => "草稿已删除",
            LeaveCancelAction::Withdrawn => "已撤回（回到草稿）",
            LeaveCancelAction::Cancelled => "已作废（销假）",
        }
    }

    /// 由单据当前状态推出该走哪条路（纯函数，单测钉死）。
    ///
    /// `None` = 这个状态没有作废路径（`CANCELLED` 已作废过、未知状态是脏数据）。
    pub fn of_status(status: &str) -> Option<Self> {
        match status.trim().to_ascii_uppercase().as_str() {
            "DRAFT" => Some(LeaveCancelAction::Deleted),
            "SUBMITTED" => Some(LeaveCancelAction::Withdrawn),
            "APPROVED" => Some(LeaveCancelAction::Cancelled),
            _ => None,
        }
    }
}

/// 作废接口（#31）的回执：`LeaveRes` 平铺 + 「做的是哪件事」。
///
/// 平铺的理由同 [`LeaveDetailRes`]：前端一个 `LeaveRes` 类型通吃。
/// 草稿被软删后记录已不在列表里，回执里的行是**删除前**的快照
/// （用户需要看到「我删掉的是哪一张」）。
#[derive(Debug, Clone, Serialize)]
pub struct LeaveCancelRes {
    /// 请假单本体（字段平铺；草稿删除时是删除前的快照）。
    #[serde(flatten)]
    pub leave: LeaveRes,
    /// `DELETED` / `WITHDRAWN` / `CANCELLED`。
    pub action: String,
    /// 动作中文名。
    pub action_label: String,
    /// 审批实例终止后的状态（`None` = 本单没有在跑的实例）。
    pub instance_status: Option<String>,
}

// ==================== 校验（全是纯函数） ====================

/// 业务时区（固定 `+08:00`，与 `employee_dto` 同一常量）。
fn tz() -> FixedOffset {
    FixedOffset::east_opt(TZ_OFFSET_SECS).expect("+08:00 一定是合法偏移")
}

/// 可空文本：trim 后空串 → `None`。
fn optional_text(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

/// 解析时间入参。接受三种形态（**都按 `+08:00` 解释**，除非自带偏移）：
///
/// 1. `2026-11-10 09:00` / `2026-11-10 09:00:00`（前端 TDesign 的默认格式，无偏移）；
/// 2. `2026-11-10T09:00:00+08:00`（RFC3339，带偏移的按自带偏移换算）；
/// 3. `2026-11-10`（**拒**：请假要精确到分 —— 半天假靠 12:00 切，只给日期等于让人猜上午还是下午）。
pub fn parse_datetime(raw: &str, field: &str) -> std::result::Result<DateTime<Utc>, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    if let Ok(aware) = DateTime::parse_from_rfc3339(text) {
        return Ok(aware.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return tz()
                .from_local_datetime(&naive)
                .single()
                .map(|aware| aware.with_timezone(&Utc))
                .ok_or_else(|| {
                    bad_request(format!(
                        "{field} 无法换算成北京时间：`{text}` 落在夏令时歧义区（本系统固定 +08:00，不会出现，请核对格式）"
                    ))
                });
        }
    }
    if NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok() {
        return Err(bad_request(format!(
            "{field} 必须精确到分（收到 `{text}`）：半天假用 12:00 切，只给日期无法判断上午还是下午，\
             请写成 `{text} 09:00` 或 `{text} 12:00`"
        )));
    }
    Err(bad_request(format!(
        "{field} 时间格式应为 `yyyy-MM-dd HH:mm`（如 `2026-11-10 09:00`）\
         或 RFC3339（如 `2026-11-10T09:00:00+08:00`），收到 `{text}`"
    )))
}

/// 日期入参（`yyyy-MM-dd`）。
fn parse_date(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| bad_request(format!("{field} 日期格式应为 yyyy-MM-dd（收到 `{text}`）")))
}

/// 假别域校验。
pub fn check_leave_type(raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request("leave_type（假别）不能为空"));
    }
    let upper = text.to_ascii_uppercase();
    if !LEAVE_TYPES.contains(&upper.as_str()) {
        return Err(bad_request(format!(
            "leave_type（假别）只能是 {}（收到 `{text}`）；中文名由字典 hr_leave_type 提供，接口只收码值",
            LEAVE_TYPES.join(" / ")
        )));
    }
    Ok(upper)
}

/// `paid` 域校验（缺省 = 带薪，与 DDL 列默认值一致）。
pub fn check_paid(raw: Option<i16>) -> std::result::Result<i16, ApiError> {
    match raw.unwrap_or(PAID_YES) {
        PAID_YES => Ok(PAID_YES),
        PAID_NO => Ok(PAID_NO),
        other => Err(bad_request(format!(
            "paid 只能是 1（带薪）或 0（无薪），收到 `{other}`"
        ))),
    }
}

/// 天数校验：`> 0`、0.5 的整数倍、不超过 `NUMERIC(6,2)` 精度上限。
///
/// 「不超过起止覆盖的自然日数」是**另一条**（[`check_duration_against_range`]）：
/// 它需要起止时间，建单与改草稿都要在合并且两端都确定之后再判。
pub fn check_duration(raw: Option<Decimal>) -> std::result::Result<Decimal, ApiError> {
    let value = raw.ok_or_else(|| bad_request("duration_days（请假天数）不能为空：半天假填 0.5，一天假填 1"))?;
    if value <= Decimal::ZERO {
        return Err(bad_request(format!(
            "duration_days（请假天数）必须大于 0（收到 `{value}`）"
        )));
    }
    if value > MAX_DURATION_DAYS {
        return Err(bad_request(format!(
            "duration_days（请假天数）超出可存精度上限（`{MAX_DURATION_DAYS}`，收到 `{value}`）：\
             请检查是不是把日期填错了"
        )));
    }
    let fraction = value - value.trunc();
    if !fraction.is_zero() && fraction != HALF_DAY {
        return Err(bad_request(format!(
            "duration_days（请假天数）只能以 0.5 天为步长（收到 `{value}`）：\
             半天假填 0.5 / 1.5，整天假填 1 / 2"
        )));
    }
    Ok(value)
}

/// 起止区间校验（`止 > 起`，与库层 `ck_biz_leave_range` 同口径）。
pub fn check_range(start: DateTime<Utc>, end: DateTime<Utc>) -> std::result::Result<(), ApiError> {
    if end <= start {
        return Err(bad_request(format!(
            "end_time（{}）必须晚于 start_time（{}）：请假至少要跨一个时刻",
            employee_dto::timestamp_cst(end),
            employee_dto::timestamp_cst(start)
        )));
    }
    Ok(())
}

/// 天数与起止区间是否自洽：`0.5 ≤ duration_days ≤ 起止覆盖的自然日数`。
///
/// 上半句（`≥ 0.5`）由 [`check_duration`] 保证，这里判「不超过跨度」——
/// 理由见模块文档「为什么还要天数 ≤ 起止跨度」。
pub fn check_duration_against_range(
    duration_days: Decimal,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    let span_days = (end.date_naive() - start.date_naive()).num_days() + 1;
    let span = Decimal::from(span_days);
    if duration_days > span {
        return Err(bad_request(format!(
            "请假天数 `{duration_days}` 超过了起止时间覆盖的自然日数 `{span_days}` 天\
             （{} ~ {}）：请假天数进薪资扣款（`ATT_LEAVE_UNPAID_DAYS`），必须与起止对得上",
            employee_dto::timestamp_cst(start),
            employee_dto::timestamp_cst(end)
        )));
    }
    Ok(())
}

/// 建单：入参 → [`LeaveCreate`]。
///
/// 顺序是刻意的：先逐字段校验（报错点名哪个字段），最后才判「天数 vs 起止」
/// —— 否则一个 `leave_type` 写错的单会先收到一条关于天数的报错，改完再改类型。
pub fn create_of(req: &LeaveCreateReq) -> std::result::Result<LeaveCreate, ApiError> {
    let employee_id = req
        .employee_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("employee_id（员工）不能为空"))?;
    let employee_id = Uuid::parse_str(employee_id)
        .map_err(|_| bad_request(format!("employee_id 不是合法 uuid（收到 `{employee_id}`）")))?;

    let leave_type = check_leave_type(req.leave_type.as_deref().unwrap_or_default())?;

    let start_raw = req
        .start_time
        .as_deref()
        .ok_or_else(|| bad_request("start_time（请假开始时间）不能为空"))?;
    let end_raw = req
        .end_time
        .as_deref()
        .ok_or_else(|| bad_request("end_time（请假结束时间）不能为空"))?;
    let start_time = parse_datetime(start_raw, "start_time")?;
    let end_time = parse_datetime(end_raw, "end_time")?;
    check_range(start_time, end_time)?;

    let duration_days = check_duration(req.duration_days)?;
    check_duration_against_range(duration_days, start_time, end_time)?;
    let paid = check_paid(req.paid)?;

    let reason = optional_text(req.reason.as_deref());
    if let Some(text) = reason.as_deref() {
        check_len(text, "reason（请假事由）", MAX_REASON)?;
    }
    let remark = optional_text(req.remark.as_deref());
    if let Some(text) = remark.as_deref() {
        check_len(text, "remark（备注）", MAX_REMARK)?;
    }

    Ok(LeaveCreate {
        employee_id,
        leave_type,
        start_time,
        end_time,
        duration_days,
        paid,
        reason,
        remark,
    })
}

/// 改草稿：入参 + 库里的当前值 → [`LeavePatch`]（缺省 = 不改）。
///
/// 合并**之后**才判「止 > 起」与「天数 ≤ 跨度」：只改 `end_time` 或只改 `duration_days`
/// 的单据，必须按合并后的整体判，否则「改半天」能绕过区间校验。
pub fn patch_of(req: &LeaveUpdateReq, current: &LeaveDraftState) -> std::result::Result<LeavePatch, ApiError> {
    let leave_type = match req.leave_type.as_deref() {
        Some(raw) => check_leave_type(raw)?,
        None => current.leave_type.clone(),
    };
    let start_time = match req.start_time.as_deref() {
        Some(raw) => parse_datetime(raw, "start_time")?,
        None => current.start_time,
    };
    let end_time = match req.end_time.as_deref() {
        Some(raw) => parse_datetime(raw, "end_time")?,
        None => current.end_time,
    };
    check_range(start_time, end_time)?;

    let duration_days = match req.duration_days {
        Some(raw) => check_duration(Some(raw))?,
        None => current.duration_days,
    };
    check_duration_against_range(duration_days, start_time, end_time)?;

    let paid = match req.paid {
        Some(raw) => check_paid(Some(raw))?,
        None => current.paid,
    };

    // 事由 / 备注：`Some("")` 是**显式清空**，`None` 才是不改（同员工 / 合同的修改语义）
    let reason = match req.reason.as_deref() {
        Some(raw) => optional_text(Some(raw)),
        None => current.reason.clone(),
    };
    if let Some(text) = reason.as_deref() {
        check_len(text, "reason（请假事由）", MAX_REASON)?;
    }
    let remark = match req.remark.as_deref() {
        Some(raw) => optional_text(Some(raw)),
        None => current.remark.clone(),
    };
    if let Some(text) = remark.as_deref() {
        check_len(text, "remark（备注）", MAX_REMARK)?;
    }

    let changed = leave_type != current.leave_type
        || start_time != current.start_time
        || end_time != current.end_time
        || duration_days != current.duration_days
        || paid != current.paid
        || reason != current.reason
        || remark != current.remark;

    Ok(LeavePatch {
        leave_type,
        start_time,
        end_time,
        duration_days,
        paid,
        reason,
        remark,
        changed,
    })
}

/// 提交审批：入参 → 提交说明（可选）。
pub fn submit_of(req: &LeaveSubmitReq) -> std::result::Result<Option<String>, ApiError> {
    let remark = optional_text(req.remark.as_deref());
    if let Some(text) = remark.as_deref() {
        check_len(text, "remark（提交说明）", MAX_REASON)?;
    }
    Ok(remark)
}

/// 作废：入参 → [`LeaveCancel`]（原因必填、≥ 5 字、≤ 255 字）。
///
/// 库层只要求 `CANCELLED` 有 `cancel_reason`（`ck_biz_leave_cancel`），**不要求字数**；
/// 5 字是设计 §5.2 #31 的接口口径：销假 / 作废会直接影响已批准的考勤与工资口径，
/// 一个「x」交代不了。
pub fn cancel_of(req: &LeaveCancelReq) -> std::result::Result<LeaveCancel, ApiError> {
    let reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            bad_request(format!(
                "作废原因（reason）必填（≥ {MIN_CANCEL_REASON_CHARS} 字）：\
                 作废 / 销假会改掉已批准的考勤口径，要能交代为什么作废"
            ))
        })?
        .to_string();
    if reason.chars().count() < MIN_CANCEL_REASON_CHARS {
        return Err(bad_request(format!(
            "作废原因至少 {MIN_CANCEL_REASON_CHARS} 个字（收到 {} 个）：{reason}",
            reason.chars().count()
        )));
    }
    check_len(&reason, "作废原因（reason）", MAX_CANCEL_REASON)?;
    Ok(LeaveCancel { reason })
}

/// 查询串 → [`LeaveFilter`]（纯函数：逗号拆分 / 域校验 / 日期解析都不连库）。
pub fn filter_of(query: &LeavePageQuery) -> std::result::Result<LeaveFilter, ApiError> {
    let employee_id = match query.employee_id.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(raw) => {
            Some(Uuid::parse_str(raw).map_err(|_| bad_request(format!("employee_id 不是合法 uuid（收到 `{raw}`）")))?)
        }
        None => None,
    };
    let dept_id = match query.dept_id.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(raw) => {
            Some(Uuid::parse_str(raw).map_err(|_| bad_request(format!("dept_id 不是合法 uuid（收到 `{raw}`）")))?)
        }
        None => None,
    };

    let mut leave_types: Vec<String> = Vec::new();
    if let Some(raw) = query.leave_type.as_deref() {
        for token in raw.split(',') {
            let text = token.trim();
            if text.is_empty() {
                continue;
            }
            leave_types.push(check_leave_type(text)?);
        }
    }

    let mut statuses: Vec<String> = Vec::new();
    if let Some(raw) = query.status.as_deref() {
        for token in raw.split(',') {
            let text = token.trim();
            if text.is_empty() {
                continue;
            }
            let upper = text.to_ascii_uppercase();
            if !STATUS_VALUES.contains(&upper.as_str()) {
                return Err(bad_request(format!(
                    "status 只能是 {}（收到 `{text}`；多选用逗号分隔，如 status=SUBMITTED,APPROVED）",
                    STATUS_VALUES.join(" / ")
                )));
            }
            statuses.push(upper);
        }
    }

    let date_from = match query.date_from.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(raw) => Some(parse_date(raw, "date_from")?),
        None => None,
    };
    let date_to = match query.date_to.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(raw) => Some(parse_date(raw, "date_to")?),
        None => None,
    };
    if let (Some(from), Some(to)) = (date_from, date_to)
        && to < from
    {
        return Err(bad_request(format!("date_to（{to}）不能早于 date_from（{from}）")));
    }

    Ok(LeaveFilter {
        keyword: optional_text(query.keyword.as_deref()),
        employee_id,
        dept_id,
        leave_types,
        statuses,
        date_from,
        date_to,
        mine: query.mine.unwrap_or(false),
    })
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::rust_decimal::prelude::FromStr;

    /// 建表迁移原文（编译期嵌入）：约束名 / 列名 / 状态域改了就红。
    const V0383_DDL: &str = include_str!("../../../migrations/business/V0383__hr_tables.up.sql");

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("合法小数")
    }

    fn at(text: &str) -> DateTime<Utc> {
        parse_datetime(text, "t").expect("合法时间")
    }

    fn create_req() -> LeaveCreateReq {
        LeaveCreateReq {
            employee_id: Some(Uuid::now_v7().to_string()),
            leave_type: Some("ANNUAL".to_string()),
            start_time: Some("2026-11-10 09:00".to_string()),
            end_time: Some("2026-11-11 18:00".to_string()),
            duration_days: Some(dec("2")),
            paid: Some(PAID_YES),
            reason: Some("回老家".to_string()),
            remark: Some("提前报备".to_string()),
        }
    }

    /// 状态域与库层 `ck_biz_leave_status` 逐值一致（迁移是唯一依据）。
    #[test]
    fn status_domain_matches_ddl_check() {
        for status in STATUS_VALUES {
            assert!(
                V0383_DDL.contains(&format!("'{status}'")),
                "状态 `{status}` 不在 V0383 的 ck_biz_leave_status 里"
            );
        }
        // 反向：DDL 的四个值必须都在 STATUS_VALUES 里
        assert_eq!(STATUS_VALUES.len(), 4, "ck_biz_leave_status 是 4 值域");
        // 与状态机同一份锁死清单（防「状态机说锁了、接口还能改」）
        assert!(is_locked(STATUS_APPROVED));
        assert!(is_locked(STATUS_CANCELLED));
        assert!(!is_locked(STATUS_DRAFT));
        assert!(!is_locked(STATUS_SUBMITTED));
    }

    /// 假别域与库层 `ck_biz_leave_type` 逐值一致，且中文名与设计 §3.1 的字典项一致。
    #[test]
    fn leave_type_domain_matches_ddl_check() {
        assert_eq!(LEAVE_TYPES.len(), 9, "ck_biz_leave_type 是 9 值域");
        for leave_type in LEAVE_TYPES {
            assert!(
                V0383_DDL.contains(&format!("'{leave_type}'")),
                "假别 `{leave_type}` 不在 V0383 的 ck_biz_leave_type 里"
            );
            assert_ne!(leave_type_label(leave_type), "未知", "假别 `{leave_type}` 缺中文名");
        }
        assert_eq!(leave_type_label(LEAVE_ANNUAL), "年假");
        assert_eq!(leave_type_label(LEAVE_PERSONAL), "事假");
        assert_eq!(leave_type_label(LEAVE_PATERNITY), "陪产假");
    }

    /// 状态 / 假别 / 长度上限与 DDL 的实际宽度对齐（写宽了库会截断或报错）。
    #[test]
    fn bounds_match_ddl_widths() {
        assert!(V0383_DDL.contains("leave_no              VARCHAR(32)"));
        assert!(V0383_DDL.contains("reason                VARCHAR(500)"));
        assert!(V0383_DDL.contains("cancel_reason         VARCHAR(255)"));
        assert_eq!(LEAVE_NO_LEN, 32);
        assert_eq!(MAX_REASON, 500);
        assert_eq!(MAX_CANCEL_REASON, 255);
        assert_eq!(MAX_DURATION_DAYS, dec("9999.99"));
    }

    /// 时间入参：三种接受形态 + 两种人话拒绝。
    #[test]
    fn parse_datetime_accepts_known_formats() {
        // 无偏移 → 按 +08:00 解释
        let local = at("2026-11-10 09:00");
        assert_eq!(employee_dto::timestamp_cst(local), "2026-11-10T09:00:00+08:00");
        // 带秒
        assert_eq!(at("2026-11-10 09:00:00"), local);
        // T 分隔无偏移
        assert_eq!(at("2026-11-10T09:00:00"), local);
        // RFC3339 换算出同一时刻
        assert_eq!(at("2026-11-10T01:00:00Z"), local);

        // 只给日期 → 明确拒（半天假靠 12:00 切，不能让人猜）
        let error = parse_datetime("2026-11-10", "start_time").expect_err("只给日期应拒");
        assert!(error.msg.contains("必须精确到分"), "报文要点名原因：{}", error.msg);
        // 乱填
        let error = parse_datetime("11/10/2026", "start_time").expect_err("乱填应拒");
        assert!(
            error.msg.contains("yyyy-MM-dd HH:mm"),
            "报文要给正确格式：{}",
            error.msg
        );
    }

    /// 建单：合法载荷逐字段落到 [`LeaveCreate`]。
    #[test]
    fn create_of_maps_valid_payload() {
        let mut req = create_req();
        let employee = req.employee_id.clone().expect("uuid");
        let created = create_of(&req).expect("合法载荷");
        assert_eq!(created.employee_id.to_string(), employee);
        assert_eq!(created.leave_type, LEAVE_ANNUAL);
        assert_eq!(created.duration_days, dec("2"));
        assert_eq!(created.paid, PAID_YES);
        assert_eq!(created.reason.as_deref(), Some("回老家"));
        assert_eq!(created.remark.as_deref(), Some("提前报备"));

        // 缺省 paid → 带薪（DDL 默认值）
        req.paid = None;
        assert_eq!(create_of(&req).expect("合法").paid, PAID_YES);
        // 事由传空串 → None（不是空字符串）
        req.reason = Some("   ".to_string());
        assert_eq!(create_of(&req).expect("合法").reason, None);
    }

    /// 建单：每个必填项缺失都点名该字段。
    #[test]
    fn create_of_names_the_missing_field() {
        /// 一列缺字段的用例：字段名 + 把它清空的改法。
        type Case = (&'static str, fn(&mut LeaveCreateReq));

        let cases: [Case; 5] = [
            ("employee_id", |req| req.employee_id = None),
            ("leave_type", |req| req.leave_type = None),
            ("start_time", |req| req.start_time = None),
            ("end_time", |req| req.end_time = None),
            ("duration_days", |req| req.duration_days = None),
        ];
        for (field, mutate) in cases {
            let mut req = create_req();
            mutate(&mut req);
            let error = create_of(&req).expect_err("缺字段应拒");
            assert!(error.msg.contains(field), "报文必须点名 `{field}`，实际：{}", error.msg);
        }
    }

    /// 假别越界 / 小写能纠正 / paid 越界。
    #[test]
    fn domain_checks_reject_bad_values() {
        let mut req = create_req();
        req.leave_type = Some("VACATION".to_string());
        let error = create_of(&req).expect_err("假别越界");
        assert!(error.msg.contains("ANNUAL"), "报文要列出合法值：{}", error.msg);

        // 小写自动纠正（前端各种来源都可能传小写）
        let mut req = create_req();
        req.leave_type = Some("annual".to_string());
        assert_eq!(create_of(&req).expect("小写可纠正").leave_type, LEAVE_ANNUAL);

        let mut req = create_req();
        req.paid = Some(2);
        let error = create_of(&req).expect_err("paid 越界");
        assert!(error.msg.contains("1（带薪）或 0（无薪）"), "{}", error.msg);
    }

    /// 天数：0.5 粒度 / 正数 / 精度上限。
    #[test]
    fn duration_checks_enforce_half_day_granularity() {
        assert_eq!(check_duration(Some(dec("0.5"))).expect("半天"), dec("0.5"));
        assert_eq!(check_duration(Some(dec("1.5"))).expect("一天半"), dec("1.5"));
        assert_eq!(check_duration(Some(dec("3"))).expect("整天"), dec("3"));

        let error = check_duration(Some(dec("1.25"))).expect_err("1.25 天应拒");
        assert!(error.msg.contains("0.5 天为步长"), "{}", error.msg);

        let error = check_duration(Some(dec("0"))).expect_err("0 天应拒");
        assert!(error.msg.contains("必须大于 0"), "{}", error.msg);

        let error = check_duration(Some(dec("-1"))).expect_err("负数应拒");
        assert!(error.msg.contains("必须大于 0"), "{}", error.msg);

        let error = check_duration(Some(dec("10000"))).expect_err("超精度应拒");
        assert!(error.msg.contains("精度上限"), "{}", error.msg);
    }

    /// 天数 vs 起止：跨度内放行，超跨度拒，反向（天数 > 跨度）也拒。
    #[test]
    fn duration_must_fit_the_range() {
        let start = at("2026-11-10 09:00");
        let end = at("2026-11-11 18:00"); // 覆盖 11-10、11-11 两天
        assert!(check_duration_against_range(dec("2"), start, end).is_ok());
        assert!(check_duration_against_range(dec("1.5"), start, end).is_ok());
        assert!(check_duration_against_range(dec("0.5"), start, end).is_ok());

        let error = check_duration_against_range(dec("3"), start, end).expect_err("超跨度应拒");
        assert!(error.msg.contains("自然日数"), "{}", error.msg);
        assert!(error.msg.contains("2"), "报文要摆出跨度：{}", error.msg);

        // 同一天 9:00~18:00 只能算 1 天
        let same_day = check_duration_against_range(dec("2"), start, at("2026-11-10 18:00"));
        assert!(same_day.is_err(), "同一天填 2 天应拒");
    }

    /// 建单时「天数 > 跨度」必须被拦（库层合法但会算错工资的那种）。
    #[test]
    fn create_rejects_days_exceeding_range() {
        let mut req = create_req();
        req.duration_days = Some(dec("10"));
        let error = create_of(&req).expect_err("10 天 vs 2 天跨度应拒");
        assert!(error.msg.contains("超过了起止时间覆盖的自然日数"), "{}", error.msg);
    }

    /// 起止：止 = 起 与 止 < 起都拒（库层 `ck_biz_leave_range` 同口径）。
    #[test]
    fn range_check_rejects_empty_and_reversed_ranges() {
        let start = at("2026-11-10 09:00");
        assert!(check_range(start, start).is_err(), "止 = 起 应拒");
        assert!(check_range(start, at("2026-11-10 08:00")).is_err(), "止 < 起 应拒");
        assert!(check_range(start, at("2026-11-10 12:00")).is_ok(), "半天应放行");
    }

    /// 改草稿：缺省 = 不改；只改一端也按合并后的整体校验。
    #[test]
    fn patch_merges_missing_fields_and_keeps_consistency() {
        let current = LeaveDraftState {
            leave_type: LEAVE_ANNUAL.to_string(),
            start_time: at("2026-11-10 09:00"),
            end_time: at("2026-11-11 18:00"),
            duration_days: dec("2"),
            paid: PAID_YES,
            reason: Some("回老家".to_string()),
            remark: None,
        };

        // 什么都不传 → 值不变 + changed = false
        let patch = patch_of(&LeaveUpdateReq::default(), &current).expect("空 patch");
        assert!(!patch.changed, "空 patch 不应报告改动");

        // 只改类型
        let patch = patch_of(
            &LeaveUpdateReq {
                leave_type: Some("SICK".to_string()),
                ..LeaveUpdateReq::default()
            },
            &current,
        )
        .expect("改类型");
        assert_eq!(patch.leave_type, LEAVE_SICK);
        assert_eq!(patch.duration_days, dec("2"), "未传的字段保持原值");
        assert!(patch.changed);

        // 只缩短起止、不改天数 → 合并后「2 天 > 1 天跨度」必须拒
        let error = patch_of(
            &LeaveUpdateReq {
                end_time: Some("2026-11-10 18:00".to_string()),
                ..LeaveUpdateReq::default()
            },
            &current,
        )
        .expect_err("只改止时间应触发天数校验");
        assert!(error.msg.contains("自然日数"), "{}", error.msg);

        // 显式清空备注（Some("")）与不改（None）区分开
        let with_remark = LeaveDraftState {
            remark: Some("旧的".to_string()),
            ..current.clone()
        };
        let patch = patch_of(
            &LeaveUpdateReq {
                remark: Some(String::new()),
                ..LeaveUpdateReq::default()
            },
            &with_remark,
        )
        .expect("清空备注");
        assert_eq!(patch.remark, None);
        assert!(patch.changed, "清空备注是改动");

        let patch = patch_of(&LeaveUpdateReq::default(), &with_remark).expect("不改备注");
        assert_eq!(patch.remark.as_deref(), Some("旧的"));
    }

    /// 作废原因：必填 / ≥5 字 / ≤255 字，前后空格要 trim。
    #[test]
    fn cancel_requires_meaningful_reason() {
        let ok = cancel_of(&LeaveCancelReq {
            reason: Some("  员工提前返岗  ".to_string()),
        })
        .expect("合法原因");
        assert_eq!(ok.reason, "员工提前返岗");

        assert!(cancel_of(&LeaveCancelReq { reason: None }).is_err(), "缺原因应拒");
        assert!(
            cancel_of(&LeaveCancelReq {
                reason: Some("   ".to_string())
            })
            .is_err(),
            "全空格应拒"
        );
        let error = cancel_of(&LeaveCancelReq {
            reason: Some("错".to_string()),
        })
        .expect_err("1 个字应拒");
        assert!(error.msg.contains("至少 5 个字"), "{}", error.msg);
        let error = cancel_of(&LeaveCancelReq {
            reason: Some("啊".repeat(MAX_CANCEL_REASON + 1)),
        })
        .expect_err("超长应拒");
        assert!(error.msg.contains("最长 255"), "{}", error.msg);
    }

    /// 列表筛选：逗号多选 / 域校验 / uuid / 日期交叉校验。
    #[test]
    fn filter_parses_multi_select_and_rejects_bad_values() {
        let filter = filter_of(&LeavePageQuery {
            keyword: Some("  张三  ".to_string()),
            status: Some("SUBMITTED, approved".to_string()),
            leave_type: Some("ANNUAL,personal".to_string()),
            date_from: Some("2026-11-01".to_string()),
            date_to: Some("2026-11-30".to_string()),
            mine: Some(true),
            ..LeavePageQuery::default()
        })
        .expect("合法筛选");
        assert_eq!(filter.keyword.as_deref(), Some("张三"));
        assert_eq!(filter.statuses, vec!["SUBMITTED", "APPROVED"]);
        assert_eq!(filter.leave_types, vec![LEAVE_ANNUAL, LEAVE_PERSONAL]);
        assert_eq!(filter.date_from, Some(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()));
        assert!(filter.mine);

        let error = filter_of(&LeavePageQuery {
            status: Some("DRAFT,DONE".to_string()),
            ..LeavePageQuery::default()
        })
        .expect_err("状态越界");
        assert!(error.msg.contains("DONE"), "{}", error.msg);

        let error = filter_of(&LeavePageQuery {
            employee_id: Some("not-a-uuid".to_string()),
            ..LeavePageQuery::default()
        })
        .expect_err("uuid 非法");
        assert!(error.msg.contains("employee_id"), "{}", error.msg);

        let error = filter_of(&LeavePageQuery {
            date_from: Some("2026-11-30".to_string()),
            date_to: Some("2026-11-01".to_string()),
            ..LeavePageQuery::default()
        })
        .expect_err("日期倒挂");
        assert!(error.msg.contains("不能早于"), "{}", error.msg);
    }

    /// 分页归一：页码下限 1、每页夹在 1..=200。
    #[test]
    fn page_params_are_normalized() {
        assert_eq!(normalize_page(None, None), (1, DEFAULT_PAGE_SIZE));
        assert_eq!(normalize_page(Some(0), Some(0)), (1, 1));
        assert_eq!(normalize_page(Some(3), Some(100_000)), (3, MAX_PAGE_SIZE));
    }

    /// 出参形状：`*_id` 必配 `*_name`（`AGENTS.md` 约定 13），且序列化字段名是 snake_case。
    #[test]
    fn leave_res_pairs_ids_with_names() {
        let res = LeaveRes {
            id: "id".to_string(),
            leave_no: "LV-20261110-000001".to_string(),
            employee_id: "emp".to_string(),
            employee_no: "EMP-0001".to_string(),
            employee_name: "张三".to_string(),
            dept_id: Some("dept".to_string()),
            dept_name: "生产部".to_string(),
            leave_type: LEAVE_ANNUAL.to_string(),
            leave_type_label: leave_type_label(LEAVE_ANNUAL).to_string(),
            start_time: employee_dto::timestamp_cst(at("2026-11-10 09:00")),
            end_time: employee_dto::timestamp_cst(at("2026-11-10 18:00")),
            duration_days: dec("1"),
            paid: PAID_YES,
            paid_label: paid_label(PAID_YES).to_string(),
            status: STATUS_APPROVED.to_string(),
            status_label: status_label(STATUS_APPROVED).to_string(),
            locked: true,
            reason: None,
            approval_instance_id: Some("inst".to_string()),
            submitted_at: None,
            submitted_by: Some("user".to_string()),
            submitted_by_name: Some("李四".to_string()),
            cancelled_at: None,
            cancelled_by: None,
            cancelled_by_name: None,
            cancel_reason: None,
            remark: None,
            created_at: employee_dto::timestamp_cst(at("2026-11-09 10:00")),
            updated_at: employee_dto::timestamp_cst(at("2026-11-10 11:00")),
        };
        let json = serde_json::to_value(&res).expect("可序列化");
        let obj = json.as_object().expect("对象");
        // 每个 *_id 都有对应的 *_name（或 *_no）伙伴
        for pair in [
            ("employee_id", "employee_name"),
            ("dept_id", "dept_name"),
            ("submitted_by", "submitted_by_name"),
            ("cancelled_by", "cancelled_by_name"),
        ] {
            assert!(obj.contains_key(pair.0), "缺字段 {}", pair.0);
            assert!(obj.contains_key(pair.1), "缺配对字段 {}", pair.1);
        }
        assert_eq!(obj["duration_days"], serde_json::json!("1"), "天数必须下发字符串");
        assert_eq!(obj["status_label"], serde_json::json!("已批准"));
        assert_eq!(obj["start_time"], serde_json::json!("2026-11-10T09:00:00+08:00"));
    }
}
